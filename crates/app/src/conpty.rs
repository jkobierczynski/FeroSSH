//! A local shell on a Windows pseudo console (ConPTY).
//!
//! Until milestone 3 brings SSH sessions, this is the host the terminal
//! talks to: `cmd.exe` by default, or the command given on FeroSSH's
//! command line (for example `wsl` or `ssh host`, to run vttest).
//!
//! Three threads per shell: one reads the console's output, one writes
//! input to it so a slow child never blocks the UI, and one waits for the
//! process to exit. Output reaches the UI through a bounded channel; the
//! `notify` callback wakes the UI thread when something arrives.

// ConPTY, pipes and process creation are Win32 calls; each unsafe block is a
// direct call with handles this module created and owns.
#![allow(unsafe_code)]

use std::ffi::c_void;
use std::sync::mpsc::{Receiver, Sender, SyncSender, channel, sync_channel};
use std::thread;

use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows::Win32::System::Console::{
    COORD, ClosePseudoConsole, CreatePseudoConsole, HPCON, ResizePseudoConsole,
};
use windows::Win32::System::Pipes::CreatePipe;
use windows::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT,
    GetExitCodeProcess, INFINITE, InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
    PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, PROCESS_INFORMATION, STARTF_USESTDHANDLES, STARTUPINFOEXW,
    UpdateProcThreadAttribute, WaitForSingleObject,
};
use windows::core::{HSTRING, PCWSTR, PWSTR, Result};

/// Output chunks buffered between the reader thread and the UI; with
/// 64 KB reads this caps the backlog at 4 MB and slows a flooding child
/// down to what the UI can show.
const BACKLOG_CHUNKS: usize = 64;
const READ_CHUNK: usize = 64 * 1024;

#[derive(Debug)]
pub enum Output {
    Data(Vec<u8>),
    /// The process exited with this code.
    Exited(u32),
}

/// A handle that may move to another thread. Each one is used by exactly
/// one owner, which closes it.
struct OwnedHandle(HANDLE);

// SAFETY: a kernel handle is just a number valid process-wide; ownership
// moves with the value and only the owner uses or closes it.
unsafe impl Send for OwnedHandle {}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: we own the handle and close it once.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// A running child on a pseudo console.
pub struct Shell {
    console: HPCON,
    input: Option<Sender<Vec<u8>>>,
    output: Option<Receiver<Output>>,
}

impl std::fmt::Debug for Shell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shell").finish_non_exhaustive()
    }
}

/// The command `Shell::spawn` runs when none is given.
pub fn default_command() -> String {
    std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_owned())
}

fn coord(cols: usize, rows: usize) -> COORD {
    let clamp = |v: usize| i16::try_from(v.max(1)).unwrap_or(i16::MAX);
    COORD {
        X: clamp(cols),
        Y: clamp(rows),
    }
}

impl Shell {
    /// Starts `command` on a new pseudo console of `cols` x `rows`.
    pub fn spawn(
        command: &str,
        cols: usize,
        rows: usize,
        notify: impl Fn() + Send + Clone + 'static,
    ) -> Result<Self> {
        let (mut in_read, mut in_write) = (HANDLE::default(), HANDLE::default());
        let (mut out_read, mut out_write) = (HANDLE::default(), HANDLE::default());
        // SAFETY: out-pointers to locals; each handle is wrapped right away.
        unsafe { CreatePipe(&mut in_read, &mut in_write, None, 0)? };
        let (in_read, in_write) = (OwnedHandle(in_read), OwnedHandle(in_write));
        // SAFETY: as above.
        unsafe { CreatePipe(&mut out_read, &mut out_write, None, 0)? };
        let (out_read, out_write) = (OwnedHandle(out_read), OwnedHandle(out_write));

        // SAFETY: valid pipe handles; the console duplicates them, so ours
        // close when this function returns.
        let console = unsafe { CreatePseudoConsole(coord(cols, rows), in_read.0, out_write.0, 0)? };
        drop((in_read, out_write));

        let process = match start_process(command, console) {
            Ok(p) => p,
            Err(e) => {
                // SAFETY: the console we just created.
                unsafe { ClosePseudoConsole(console) };
                return Err(e);
            }
        };

        let (out_tx, output) = sync_channel(BACKLOG_CHUNKS);
        let (in_tx, in_rx) = channel::<Vec<u8>>();
        spawn_reader(out_read, out_tx.clone(), notify.clone());
        spawn_writer(in_write, in_rx);
        spawn_waiter(process, out_tx, notify);
        Ok(Self {
            console,
            input: Some(in_tx),
            output: Some(output),
        })
    }

    /// Queues bytes for the child's input.
    pub fn write(&self, bytes: Vec<u8>) {
        if let Some(tx) = &self.input {
            let _ = tx.send(bytes);
        }
    }

    pub fn resize(&self, cols: usize, rows: usize) {
        // SAFETY: our live console. A failure leaves the old size.
        unsafe {
            let _ = ResizePseudoConsole(self.console, coord(cols, rows));
        }
    }

    /// The next piece of output, if any is waiting.
    pub fn try_read(&self) -> Option<Output> {
        self.output.as_ref()?.try_recv().ok()
    }
}

impl Drop for Shell {
    fn drop(&mut self) {
        // Stop the writer and drop the output channel, then close the
        // console, which ends the child. The reader keeps draining (and
        // discarding) output meanwhile, so the close cannot block on a full
        // pipe, and stops once the pipe breaks.
        self.input = None;
        self.output = None;
        // SAFETY: our console, closed once.
        unsafe { ClosePseudoConsole(self.console) };
    }
}

fn start_process(command: &str, console: HPCON) -> Result<OwnedHandle> {
    let mut size = 0usize;
    // SAFETY: the first call only reports the size the list needs and is
    // expected to fail.
    let _ = unsafe { InitializeProcThreadAttributeList(None, 1, None, &mut size) };
    let mut list_buf = vec![0u8; size];
    let list = LPPROC_THREAD_ATTRIBUTE_LIST(list_buf.as_mut_ptr().cast());
    // SAFETY: `list_buf` has the size the call asked for and outlives the
    // list, which is deleted below.
    unsafe { InitializeProcThreadAttributeList(Some(list), 1, None, &mut size)? };

    let result = (|| {
        // SAFETY: the attribute value is the console handle itself, passed
        // by value as ConPTY expects; `list` is initialised.
        unsafe {
            UpdateProcThreadAttribute(
                list,
                0,
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                Some(console.0 as *const c_void),
                std::mem::size_of::<HPCON>(),
                None,
                None,
            )?;
        }
        let mut si = STARTUPINFOEXW::default();
        si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        // No inherited standard handles: the child's console is the
        // pseudo console.
        si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        si.lpAttributeList = list;

        let mut cmdline: Vec<u16> = command.encode_utf16().chain([0]).collect();
        let home = std::env::var("USERPROFILE").ok().map(HSTRING::from);
        let cwd = home.as_ref().map_or(PCWSTR::null(), |h| PCWSTR(h.as_ptr()));
        let mut pi = PROCESS_INFORMATION::default();
        // SAFETY: valid, NUL-terminated mutable command line; the startup
        // info and attribute list live until the call returns.
        unsafe {
            CreateProcessW(
                None,
                Some(PWSTR(cmdline.as_mut_ptr())),
                None,
                None,
                false,
                EXTENDED_STARTUPINFO_PRESENT,
                None,
                cwd,
                &si.StartupInfo,
                &mut pi,
            )?;
        }
        drop(OwnedHandle(pi.hThread));
        Ok(OwnedHandle(pi.hProcess))
    })();

    // SAFETY: the list initialised above, deleted once.
    unsafe { DeleteProcThreadAttributeList(list) };
    result
}

fn spawn_reader(pipe: OwnedHandle, tx: SyncSender<Output>, notify: impl Fn() + Send + 'static) {
    thread::spawn(move || {
        // Move the whole handle in, not just its field, so it closes here.
        let pipe = pipe;
        let mut buf = vec![0u8; READ_CHUNK];
        loop {
            let mut read = 0u32;
            // SAFETY: our pipe and a buffer of the length we pass.
            let ok = unsafe { ReadFile(pipe.0, Some(&mut buf), Some(&mut read), None) };
            if ok.is_err() || read == 0 {
                break;
            }
            let data = buf.get(..read as usize).unwrap_or_default().to_vec();
            if tx.send(Output::Data(data)).is_ok() {
                notify();
            }
        }
    });
}

fn spawn_writer(pipe: OwnedHandle, rx: Receiver<Vec<u8>>) {
    thread::spawn(move || {
        let pipe = pipe;
        while let Ok(bytes) = rx.recv() {
            let mut rest = bytes.as_slice();
            while !rest.is_empty() {
                let mut written = 0u32;
                // SAFETY: our pipe and a valid slice.
                let ok = unsafe { WriteFile(pipe.0, Some(rest), Some(&mut written), None) };
                if ok.is_err() || written == 0 {
                    return;
                }
                rest = rest.get(written as usize..).unwrap_or_default();
            }
        }
    });
}

fn spawn_waiter(process: OwnedHandle, tx: SyncSender<Output>, notify: impl Fn() + Send + 'static) {
    thread::spawn(move || {
        let process = process;
        // SAFETY: our process handle.
        if unsafe { WaitForSingleObject(process.0, INFINITE) } != WAIT_OBJECT_0 {
            return;
        }
        let mut code = 0u32;
        // SAFETY: our process handle and an out-pointer to a local.
        let _ = unsafe { GetExitCodeProcess(process.0, &mut code) };
        if tx.send(Output::Exited(code)).is_ok() {
            notify();
        }
    });
}
