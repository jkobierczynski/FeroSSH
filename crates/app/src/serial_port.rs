//! A serial console on a COM port of this PC.
//!
//! The UI process opens the port itself: the sandboxed SSH worker has no
//! access to devices, and a serial line carries no network traffic to
//! isolate. The port is opened for overlapped I/O so that reading and
//! writing never wait for each other: one thread reads, one writes, and a
//! break runs on its own short-lived thread. Output reaches the UI through
//! a bounded channel, and `notify` wakes the UI thread, as for a local
//! shell (see `conpty`).

use ferossh_config::serial::{Flow, LineSettings, Parity, StopBits};

/// DCB flags (the `_bitfield` word) for these line settings, as PuTTY
/// sets them: binary mode, parity checking when there is parity, DTR and
/// RTS raised unless they do handshaking, XON/XOFF in both directions for
/// software flow control.
pub fn dcb_flags(line: &LineSettings) -> u32 {
    const BINARY: u32 = 1;
    const PARITY: u32 = 1 << 1;
    const OUTX_CTS: u32 = 1 << 2;
    const OUTX_DSR: u32 = 1 << 3;
    const DTR_SHIFT: u32 = 4;
    const TX_CONTINUE_ON_XOFF: u32 = 1 << 7;
    const OUTX: u32 = 1 << 8;
    const INX: u32 = 1 << 9;
    const RTS_SHIFT: u32 = 12;
    const CONTROL_ENABLE: u32 = 1;
    const CONTROL_HANDSHAKE: u32 = 2;

    let mut flags = BINARY | TX_CONTINUE_ON_XOFF;
    if line.parity != Parity::None {
        flags |= PARITY;
    }
    let (dtr, rts) = match line.flow {
        Flow::None => (CONTROL_ENABLE, CONTROL_ENABLE),
        Flow::XonXoff => {
            flags |= OUTX | INX;
            (CONTROL_ENABLE, CONTROL_ENABLE)
        }
        Flow::RtsCts => {
            flags |= OUTX_CTS;
            (CONTROL_ENABLE, CONTROL_HANDSHAKE)
        }
        Flow::DsrDtr => {
            flags |= OUTX_DSR;
            (CONTROL_HANDSHAKE, CONTROL_ENABLE)
        }
    };
    flags | dtr << DTR_SHIFT | rts << RTS_SHIFT
}

/// The DCB's `Parity` and `StopBits` bytes.
pub fn dcb_framing(line: &LineSettings) -> (u8, u8) {
    let parity = match line.parity {
        Parity::None => 0,
        Parity::Odd => 1,
        Parity::Even => 2,
        Parity::Mark => 3,
        Parity::Space => 4,
    };
    let stop = match line.stop_bits {
        StopBits::One => 0,
        StopBits::OneAndHalf => 1,
        StopBits::Two => 2,
    };
    (parity, stop)
}

#[cfg(windows)]
pub use port::{Output, SerialPort};

// CreateFile, the comm state calls and overlapped I/O are Win32 calls;
// each unsafe block is a direct call on the handle this module opened and
// owns, with buffers and OVERLAPPED structures that outlive the I/O.
#[cfg(windows)]
#[allow(unsafe_code)]
mod port {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{Receiver, Sender, SyncSender, channel, sync_channel};
    use std::thread;
    use std::time::Duration;

    use ferossh_config::serial::SerialTarget;
    use windows::Win32::Devices::Communication::{
        COMMTIMEOUTS, ClearCommBreak, DCB, DCB_PARITY, DCB_STOP_BITS, GetCommState, PURGE_RXCLEAR,
        PURGE_TXCLEAR, PurgeComm, SetCommBreak, SetCommState, SetCommTimeouts, SetupComm,
    };
    use windows::Win32::Foundation::{
        CloseHandle, ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_IO_PENDING,
        ERROR_PATH_NOT_FOUND, GENERIC_READ, GENERIC_WRITE, HANDLE,
    };
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_OVERLAPPED, FILE_SHARE_NONE, OPEN_EXISTING, ReadFile, WriteFile,
    };
    use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
    use windows::Win32::System::Threading::CreateEventW;
    use windows::core::{HSTRING, PCWSTR};

    use super::{dcb_flags, dcb_framing};

    /// Output chunks buffered between the reader thread and the UI.
    const BACKLOG_CHUNKS: usize = 64;
    const READ_CHUNK: usize = 16 * 1024;
    /// A read waits this long for a first byte before trying again, so
    /// the reader notices the port closing.
    const READ_WAIT_MS: u32 = 500;
    /// How long a break holds the line low.
    const BREAK: Duration = Duration::from_millis(300);

    #[derive(Debug)]
    pub enum Output {
        Data(Vec<u8>),
        /// The port stopped working (an adapter unplugged): why.
        Closed(String),
    }

    /// A handle shared by the threads of one port; closed when the last
    /// one lets go.
    struct Shared(HANDLE);

    // SAFETY: a kernel handle is valid process-wide; overlapped reads,
    // writes and comm calls on one handle may come from several threads.
    unsafe impl Send for Shared {}
    // SAFETY: as above; the handle is never changed after it is opened.
    unsafe impl Sync for Shared {}

    impl Drop for Shared {
        fn drop(&mut self) {
            // SAFETY: the last owner closes the handle once.
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    /// An event for one thread's OVERLAPPED, closed with it.
    struct Event(HANDLE);

    impl Event {
        fn new() -> windows::core::Result<Self> {
            // SAFETY: a new unnamed manual-reset event.
            unsafe { CreateEventW(None, true, false, PCWSTR::null()) }.map(Self)
        }
    }

    impl Drop for Event {
        fn drop(&mut self) {
            // SAFETY: we own the event.
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    pub struct SerialPort {
        handle: Arc<Shared>,
        stop: Arc<AtomicBool>,
        input: Option<Sender<Vec<u8>>>,
        output: Receiver<Output>,
    }

    impl std::fmt::Debug for SerialPort {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("SerialPort").finish_non_exhaustive()
        }
    }

    impl SerialPort {
        /// Opens and sets up `target`'s port; the error says why not, in
        /// words for the user.
        pub fn open(
            target: &SerialTarget,
            notify: impl Fn() + Send + Clone + 'static,
        ) -> Result<Self, String> {
            let port = &target.port;
            // SAFETY: a valid path; the handle is wrapped right away.
            let opened = unsafe {
                CreateFileW(
                    &HSTRING::from(target.device_path()),
                    (GENERIC_READ | GENERIC_WRITE).0,
                    FILE_SHARE_NONE,
                    None,
                    OPEN_EXISTING,
                    FILE_FLAG_OVERLAPPED,
                    None,
                )
            };
            let handle = match opened {
                Ok(h) => Arc::new(Shared(h)),
                Err(e)
                    if e.code() == ERROR_FILE_NOT_FOUND.to_hresult()
                        || e.code() == ERROR_PATH_NOT_FOUND.to_hresult() =>
                {
                    return Err(format!(
                        "There is no {port} on this PC. Device Manager lists the COM ports \
                         under Ports (COM & LPT); a USB adapter shows there once plugged in."
                    ));
                }
                Err(e) if e.code() == ERROR_ACCESS_DENIED.to_hresult() => {
                    return Err(format!(
                        "{port} is in use by another program (or another FeroSSH tab)."
                    ));
                }
                Err(e) => return Err(format!("Could not open {port}: {}", e.message())),
            };
            configure(handle.0, target).map_err(|e| {
                format!(
                    "{port} refused {} {} ({}): {}",
                    target.settings.speed,
                    target.settings.framing(),
                    target.settings.flow.label(),
                    e.message()
                )
            })?;
            let stop = Arc::new(AtomicBool::new(false));
            let (out_tx, output) = sync_channel(BACKLOG_CHUNKS);
            let (in_tx, in_rx) = channel::<Vec<u8>>();
            spawn_reader(handle.clone(), stop.clone(), out_tx, notify);
            spawn_writer(handle.clone(), in_rx);
            Ok(Self {
                handle,
                stop,
                input: Some(in_tx),
                output,
            })
        }

        /// Queues bytes for the line.
        pub fn write(&self, bytes: Vec<u8>) {
            if let Some(tx) = &self.input {
                let _ = tx.send(bytes);
            }
        }

        pub fn try_read(&self) -> Option<Output> {
            self.output.try_recv().ok()
        }

        /// Holds the line in the break state for a moment: what a router
        /// or switch takes as "enter ROM monitor" or the SysRq key.
        pub fn send_break(&self) {
            let handle = self.handle.clone();
            thread::spawn(move || {
                // SAFETY: the shared handle stays open while we hold it.
                if unsafe { SetCommBreak(handle.0) }.is_ok() {
                    thread::sleep(BREAK);
                    // SAFETY: as above.
                    let _ = unsafe { ClearCommBreak(handle.0) };
                }
            });
        }
    }

    impl Drop for SerialPort {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            // The writer ends when its channel closes.
            self.input = None;
            // SAFETY: cancels this handle's pending reads and writes, from
            // every thread; they then see `stop` or the closed channel.
            let _ = unsafe { CancelIoEx(self.handle.0, None) };
        }
    }

    fn configure(h: HANDLE, target: &SerialTarget) -> windows::core::Result<()> {
        let line = &target.settings;
        // SAFETY: valid handle; the queue sizes are only advice.
        let _ = unsafe { SetupComm(h, 64 * 1024, 64 * 1024) };
        let mut dcb = DCB {
            DCBlength: std::mem::size_of::<DCB>() as u32,
            ..Default::default()
        };
        // SAFETY: valid handle and a DCB with its length set.
        unsafe { GetCommState(h, &mut dcb)? };
        let (parity, stop) = dcb_framing(line);
        dcb.BaudRate = line.speed;
        dcb.ByteSize = line.data_bits;
        dcb.Parity = DCB_PARITY(parity);
        dcb.StopBits = DCB_STOP_BITS(stop);
        dcb._bitfield = dcb_flags(line);
        dcb.XonChar = 0x11;
        dcb.XoffChar = 0x13;
        dcb.XonLim = 2048;
        dcb.XoffLim = 512;
        // SAFETY: as above.
        unsafe { SetCommState(h, &dcb)? };
        // A read returns as soon as bytes are there, or after READ_WAIT_MS
        // with none; writes wait as long as flow control holds them.
        let timeouts = COMMTIMEOUTS {
            ReadIntervalTimeout: u32::MAX,
            ReadTotalTimeoutMultiplier: u32::MAX,
            ReadTotalTimeoutConstant: READ_WAIT_MS,
            WriteTotalTimeoutMultiplier: 0,
            WriteTotalTimeoutConstant: 0,
        };
        // SAFETY: as above.
        unsafe { SetCommTimeouts(h, &timeouts)? };
        // SAFETY: as above; drops what an earlier user left in the queues.
        let _ = unsafe { PurgeComm(h, PURGE_RXCLEAR | PURGE_TXCLEAR) };
        Ok(())
    }

    /// One overlapped read or write, waited for; the bytes moved.
    fn overlapped(
        h: HANDLE,
        event: &Event,
        start: impl FnOnce(*mut OVERLAPPED) -> windows::core::Result<()>,
    ) -> windows::core::Result<u32> {
        let mut ov = OVERLAPPED {
            hEvent: event.0,
            ..Default::default()
        };
        match start(&mut ov) {
            Ok(()) => {}
            Err(e) if e.code() == ERROR_IO_PENDING.to_hresult() => {}
            Err(e) => return Err(e),
        }
        let mut n = 0u32;
        // SAFETY: `ov` stays in place until the I/O it started is done,
        // which this waits for.
        unsafe { GetOverlappedResult(h, &ov, &mut n, true)? };
        Ok(n)
    }

    fn spawn_reader(
        handle: Arc<Shared>,
        stop: Arc<AtomicBool>,
        out: SyncSender<Output>,
        notify: impl Fn() + Send + 'static,
    ) {
        thread::spawn(move || {
            let event = match Event::new() {
                Ok(e) => e,
                Err(e) => {
                    let _ = out.send(Output::Closed(e.message()));
                    notify();
                    return;
                }
            };
            let mut buf = vec![0u8; READ_CHUNK];
            while !stop.load(Ordering::Acquire) {
                // SAFETY: `buf` outlives the read, which `overlapped` waits
                // for.
                let read = overlapped(handle.0, &event, |ov| unsafe {
                    ReadFile(handle.0, Some(&mut buf), None, Some(ov))
                });
                match read {
                    Ok(0) => {}
                    Ok(n) => {
                        let data = buf.get(..n as usize).unwrap_or_default().to_vec();
                        if out.send(Output::Data(data)).is_err() {
                            return;
                        }
                        notify();
                    }
                    Err(_) if stop.load(Ordering::Acquire) => return,
                    Err(e) => {
                        let _ = out.send(Output::Closed(e.message()));
                        notify();
                        return;
                    }
                }
            }
        });
    }

    fn spawn_writer(handle: Arc<Shared>, input: Receiver<Vec<u8>>) {
        thread::spawn(move || {
            let Ok(event) = Event::new() else {
                return;
            };
            while let Ok(bytes) = input.recv() {
                let mut rest = bytes.as_slice();
                while !rest.is_empty() {
                    // SAFETY: `bytes` outlives the write, which
                    // `overlapped` waits for.
                    let wrote = overlapped(handle.0, &event, |ov| unsafe {
                        WriteFile(handle.0, Some(rest), None, Some(ov))
                    });
                    match wrote {
                        Ok(n) if n > 0 => rest = rest.get(n as usize..).unwrap_or_default(),
                        // A failed write shows as the reader's error.
                        _ => break,
                    }
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dcb_flags_follow_the_flow_control() {
        let mut line = LineSettings::default();
        // Binary, continue on XOFF, DTR and RTS enabled.
        assert_eq!(dcb_flags(&line), 0x1 | 0x80 | 0x10 | 0x1000);
        line.flow = Flow::RtsCts;
        assert_eq!(dcb_flags(&line), 0x1 | 0x80 | 0x4 | 0x10 | 0x2000);
        line.flow = Flow::DsrDtr;
        assert_eq!(dcb_flags(&line), 0x1 | 0x80 | 0x8 | 0x20 | 0x1000);
        line.flow = Flow::XonXoff;
        line.parity = Parity::Even;
        assert_eq!(
            dcb_flags(&line),
            0x1 | 0x2 | 0x80 | 0x100 | 0x200 | 0x10 | 0x1000
        );
    }

    #[test]
    fn dcb_framing_bytes() {
        let mut line = LineSettings::default();
        assert_eq!(dcb_framing(&line), (0, 0));
        assert!(line.set_framing("7O2"));
        assert_eq!(dcb_framing(&line), (1, 2));
        assert!(line.set_framing("8S1.5"));
        assert_eq!(dcb_framing(&line), (4, 1));
    }
}
