//! Sending files: the server ran `rz`.
//!
//! The sender waits for the receiver's ZRINIT, offers each file with
//! ZFILE, and streams it from the position the receiver asks for (ZRPOS)
//! in 1 KiB subpackets. It holds no file: it asks the caller for the bytes
//! at an offset ([`TxEvent::Read`]) and the caller hands them over with
//! [`Sender::provide`], one read at a time.

use std::collections::VecDeque;
use std::time::Duration;

use crate::frame::{
    CANCEL, CANFC32, ESCCTL, Escape, Event, Header, Reader, ZABORT, ZACK, ZCAN, ZCBIN, ZCRCE,
    ZCRCG, ZCRCW, ZDATA, ZEOF, ZFERR, ZFILE, ZFIN, ZNAK, ZRINIT, ZRPOS, ZSKIP, hex_header,
};

/// Bytes per subpacket, as `sz` sends by default.
const SUBPACKET: usize = 1024;
/// The most asked for in one [`TxEvent::Read`].
pub const MAX_READ: u32 = 32 * 1024;
/// How long to wait for the receiver before trying again.
const PATIENCE: Duration = Duration::from_secs(10);
/// Times to try again before giving up.
const MAX_RETRIES: u32 = 5;
/// ZRPOS for one file before giving up: a receiver that keeps asking for
/// the same bytes is not getting anywhere.
const MAX_REPOSITIONS: u32 = 50;

/// A file to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upload {
    /// The name the receiver sees, without a folder.
    pub name: Vec<u8>,
    pub size: u64,
    /// Seconds since the Unix epoch.
    pub mtime: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TxEvent {
    /// Hand over up to `len` bytes of file `file` (an index into the
    /// uploads) from `offset`, with [`Sender::provide`].
    Read { file: u32, offset: u64, len: u32 },
    /// The receiver has the file, or `skipped` it.
    FileDone { skipped: bool },
    /// All files sent.
    Done,
    /// The transfer stopped; why, for the user.
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tx {
    /// Waiting for the receiver's ZRINIT.
    WaitInit,
    /// ZFILE sent; waiting for ZRPOS or ZSKIP.
    FileSent,
    /// Streaming data.
    Sending,
    /// A frame ended with ZCRCW; waiting for ZACK (a receiver with a
    /// buffer limit).
    AwaitAck,
    /// ZEOF sent; waiting for ZRINIT.
    EofSent,
    /// ZFIN sent; waiting for the receiver's ZFIN.
    FinSent,
    Ended,
}

#[derive(Debug, Clone)]
pub struct Sender {
    reader: Reader,
    state: Tx,
    files: Vec<Upload>,
    /// The file being sent: an index into `files`.
    current: usize,
    pos: u64,
    /// The offset of the read the caller owes us.
    reading: Option<u64>,
    crc32: bool,
    escape: Escape,
    /// The receiver's buffer size; 0 when it takes a stream.
    window: u32,
    retries: u32,
    repositions: u32,
    out: Vec<u8>,
    events: VecDeque<TxEvent>,
}

impl Sender {
    pub fn new(files: Vec<Upload>) -> Self {
        Self {
            reader: Reader::new(),
            state: Tx::WaitInit,
            files,
            current: 0,
            pos: 0,
            reading: None,
            crc32: false,
            escape: Escape::default(),
            window: 0,
            retries: 0,
            repositions: 0,
            out: Vec::new(),
            events: VecDeque::new(),
        }
    }

    /// Feeds bytes from the receiver. Returns how many were for the
    /// transfer: all of them, unless it ended part way, when the rest is
    /// terminal output again.
    pub fn feed(&mut self, bytes: &[u8]) -> usize {
        for (i, &b) in bytes.iter().enumerate() {
            if self.state == Tx::Ended {
                return i;
            }
            if let Some(event) = self.reader.push(b) {
                self.on_event(event);
            }
        }
        bytes.len()
    }

    /// The bytes of the current file at `offset`, as asked for by the last
    /// [`TxEvent::Read`]. Empty means the file could not be read.
    pub fn provide(&mut self, offset: u64, bytes: &[u8]) {
        if self.state != Tx::Sending || self.reading != Some(offset) {
            // An answer to a read a ZRPOS made stale.
            return;
        }
        self.reading = None;
        let size = self.size();
        let room = size.saturating_sub(self.pos);
        let bytes = bytes
            .get(..usize::try_from(room).unwrap_or(usize::MAX).min(bytes.len()))
            .unwrap_or_default();
        if bytes.is_empty() {
            self.cancel("FeroSSH could not read the file, or it got shorter while sending.");
            return;
        }
        let chunks = bytes.chunks(SUBPACKET).count();
        for (i, chunk) in bytes.chunks(SUBPACKET).enumerate() {
            self.pos += chunk.len() as u64;
            let last = i + 1 == chunks;
            let end = if self.pos >= size {
                ZCRCE
            } else if last && self.window > 0 {
                ZCRCW
            } else {
                ZCRCG
            };
            self.escape.subpacket(chunk, end, self.crc32, &mut self.out);
        }
        if self.pos >= size {
            self.eof();
        } else if self.window > 0 {
            self.state = Tx::AwaitAck;
        } else {
            self.read();
        }
    }

    /// Gives up, telling the receiver.
    pub fn cancel(&mut self, why: &str) {
        if self.state != Tx::Ended {
            self.out.extend_from_slice(&CANCEL);
            self.fail(why);
        }
    }

    /// How long to wait for the receiver before [`Sender::timeout`];
    /// `None` while the caller owes a read.
    pub fn patience(&self) -> Option<Duration> {
        match self.state {
            Tx::Sending | Tx::Ended => None,
            _ => Some(PATIENCE),
        }
    }

    /// The receiver said nothing for [`Sender::patience`].
    pub fn timeout(&mut self) {
        if self.state == Tx::FinSent {
            // It has everything; its ZFIN went missing.
            self.events.push_back(TxEvent::Done);
            self.state = Tx::Ended;
            return;
        }
        self.retries += 1;
        if self.retries > MAX_RETRIES {
            self.cancel("The receiver stopped answering.");
            return;
        }
        match self.state {
            Tx::FileSent => self.zfile(),
            Tx::EofSent => self.zeof(),
            Tx::AwaitAck => self.data_header(),
            _ => {}
        }
    }

    pub fn ended(&self) -> bool {
        self.state == Tx::Ended
    }

    /// Bytes for the receiver.
    pub fn take_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.out)
    }

    pub fn next_event(&mut self) -> Option<TxEvent> {
        self.events.pop_front()
    }

    fn size(&self) -> u64 {
        self.files.get(self.current).map_or(0, |f| f.size)
    }

    fn fail(&mut self, why: &str) {
        self.reading = None;
        self.events.push_back(TxEvent::Failed(why.to_owned()));
        self.state = Tx::Ended;
    }

    fn on_event(&mut self, event: Event) {
        match event {
            Event::Cancel => self.fail("The receiver cancelled the transfer."),
            Event::Header(h) => self.on_header(h),
            // The receiver sends no data this sender wants.
            Event::Data { .. } | Event::BadData => {}
        }
    }

    fn on_header(&mut self, h: Header) {
        let state = self.state;
        let in_file = matches!(
            state,
            Tx::FileSent | Tx::Sending | Tx::AwaitAck | Tx::EofSent
        );
        match h.kind {
            ZRINIT => {
                let f0 = h.f0();
                self.crc32 = f0 & CANFC32 != 0;
                self.escape.ctl = f0 & ESCCTL != 0;
                let [lo, hi, ..] = h.data;
                self.window = u32::from(u16::from_le_bytes([lo, hi]));
                match state {
                    Tx::WaitInit => self.next_file(),
                    Tx::EofSent => {
                        self.events.push_back(TxEvent::FileDone { skipped: false });
                        self.current += 1;
                        self.next_file();
                    }
                    // Sent before our ZFILE arrived; the timeout resends.
                    _ => {}
                }
            }
            ZRPOS if in_file => {
                self.repositions += 1;
                if self.repositions > MAX_REPOSITIONS {
                    self.cancel("The receiver kept asking for the same data.");
                    return;
                }
                self.retries = 0;
                self.pos = u64::from(h.position()).min(self.size());
                self.reading = None;
                self.data_header();
            }
            ZACK if state == Tx::AwaitAck => {
                self.retries = 0;
                self.data_header();
            }
            ZSKIP if in_file => {
                self.reading = None;
                self.events.push_back(TxEvent::FileDone { skipped: true });
                self.current += 1;
                self.next_file();
            }
            ZNAK if state == Tx::FileSent => self.zfile(),
            ZNAK if state == Tx::EofSent => self.zeof(),
            ZFIN if state == Tx::FinSent => {
                self.out.extend_from_slice(b"OO");
                self.events.push_back(TxEvent::Done);
                self.state = Tx::Ended;
            }
            ZCAN | ZABORT | ZFERR => self.fail("The receiver cancelled the transfer."),
            _ => {}
        }
    }

    fn next_file(&mut self) {
        self.retries = 0;
        self.repositions = 0;
        self.pos = 0;
        if self.current >= self.files.len() {
            hex_header(&Header::pos(ZFIN, 0), &mut self.out);
            self.state = Tx::FinSent;
        } else {
            self.zfile();
        }
    }

    fn zfile(&mut self) {
        let Some(file) = self.files.get(self.current) else {
            return;
        };
        let left: u64 = self
            .files
            .get(self.current..)
            .unwrap_or_default()
            .iter()
            .map(|f| f.size)
            .sum();
        let count = self.files.len() - self.current;
        let mut info = file.name.clone();
        info.push(0);
        info.extend_from_slice(
            format!("{} {:o} 100644 0 {count} {left}", file.size, file.mtime).as_bytes(),
        );
        info.push(0);
        let h = Header::flags(ZFILE, ZCBIN, 0, 0, 0);
        self.escape.bin_header(&h, self.crc32, &mut self.out);
        self.escape
            .subpacket(&info, ZCRCW, self.crc32, &mut self.out);
        self.state = Tx::FileSent;
    }

    /// Starts (or restarts) a data frame at `pos`.
    fn data_header(&mut self) {
        let h = Header::pos(ZDATA, self.pos as u32);
        self.escape.bin_header(&h, self.crc32, &mut self.out);
        if self.pos >= self.size() {
            // Nothing (left) to send: an empty subpacket ends the frame.
            self.escape.subpacket(&[], ZCRCE, self.crc32, &mut self.out);
            self.eof();
        } else {
            self.state = Tx::Sending;
            self.read();
        }
    }

    fn read(&mut self) {
        let left = self.size().saturating_sub(self.pos);
        let mut len = MAX_READ;
        if self.window > 0 {
            len = len.min(self.window);
        }
        let len = u32::try_from(left).unwrap_or(u32::MAX).min(len);
        self.reading = Some(self.pos);
        self.state = Tx::Sending;
        self.events.push_back(TxEvent::Read {
            file: self.current as u32,
            offset: self.pos,
            len,
        });
    }

    fn eof(&mut self) {
        self.zeof();
        self.state = Tx::EofSent;
    }

    fn zeof(&mut self) {
        let h = Header::pos(ZEOF, self.pos as u32);
        self.escape.bin_header(&h, self.crc32, &mut self.out);
    }
}
