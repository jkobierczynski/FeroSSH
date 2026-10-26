//! Receiving files: the server ran `sz`.
//!
//! The receiver answers the sender's ZRQINIT with ZRINIT, reads each
//! ZFILE's name and size and hands them to the caller, which accepts or
//! skips the file. Accepted file data comes out in order as
//! [`RxEvent::Data`]; the receiver asks for a resend (ZRPOS) when a
//! subpacket is damaged or arrives out of place. It never runs a command
//! (ZCOMMAND) and never asks for anything but the data.

use std::collections::VecDeque;
use std::time::Duration;

use crate::frame::{
    CANCEL, CANFC32, CANFDX, CANOVIO, Event, Header, Reader, ZABORT, ZACK, ZCAN, ZCHALLENGE,
    ZCOMMAND, ZCRCE, ZCRCQ, ZCRCW, ZDATA, ZEOF, ZFERR, ZFILE, ZFIN, ZFREECNT, ZNAK, ZRINIT, ZRPOS,
    ZRQINIT, ZSINIT, ZSKIP, hex_header,
};

/// How long to wait for the sender before asking again.
const PATIENCE: Duration = Duration::from_secs(10);
/// How long to wait for the sender's "OO" after the last ZFIN.
const OVER_AND_OUT: Duration = Duration::from_secs(1);
/// Times to ask again before giving up.
const MAX_RETRIES: u32 = 10;
/// The longest name kept from a ZFILE.
const MAX_NAME: usize = 1024;

/// A file the sender offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileInfo {
    /// As the sender gave it; may hold a path, need not be UTF-8.
    pub name: Vec<u8>,
    pub size: Option<u64>,
    /// Seconds since the Unix epoch.
    pub mtime: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RxEvent {
    /// The sender offers a file: answer with [`Receiver::accept`] or
    /// [`Receiver::skip`].
    File(FileInfo),
    /// The next bytes of the accepted file.
    Data(Vec<u8>),
    /// The accepted file is complete.
    FileDone,
    /// The sender has no more files.
    Done,
    /// The transfer stopped; why, for the user.
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rx {
    /// ZRINIT sent; waiting for ZFILE, ZSINIT or ZFIN.
    Init,
    /// ZSINIT's subpacket next.
    Sinit,
    /// ZFILE's subpacket next.
    FileInfo,
    /// The caller decides about the file.
    Asking,
    /// ZRPOS sent; waiting for ZDATA at our position (or ZEOF).
    WaitData,
    /// Reading the subpackets of a ZDATA frame.
    Data,
    /// Our ZFIN sent; the sender's "OO" may follow.
    Fin {
        o: u8,
    },
    Ended,
}

#[derive(Debug, Clone)]
pub struct Receiver {
    reader: Reader,
    state: Rx,
    /// Bytes of the current file received.
    pos: u64,
    retries: u32,
    out: Vec<u8>,
    events: VecDeque<RxEvent>,
}

impl Default for Receiver {
    fn default() -> Self {
        Self::new()
    }
}

impl Receiver {
    /// A receiver that has sent its ZRINIT.
    pub fn new() -> Self {
        let mut r = Self {
            reader: Reader::new(),
            state: Rx::Init,
            pos: 0,
            retries: 0,
            out: Vec::new(),
            events: VecDeque::new(),
        };
        r.zrinit();
        r
    }

    /// Feeds bytes from the sender. Returns how many were for the
    /// transfer: all of them, unless it ended part way, when the rest is
    /// terminal output again.
    pub fn feed(&mut self, bytes: &[u8]) -> usize {
        for (i, &b) in bytes.iter().enumerate() {
            match self.state {
                Rx::Ended => return i,
                Rx::Fin { o } => {
                    if b == b'O' && o < 2 {
                        self.state = if o == 1 { Rx::Ended } else { Rx::Fin { o: 1 } };
                        continue;
                    }
                    self.state = Rx::Ended;
                    return i;
                }
                _ => {}
            }
            if let Some(event) = self.reader.push(b) {
                self.on_event(event);
            }
        }
        bytes.len()
    }

    /// Takes the offered file; its data follows.
    pub fn accept(&mut self) {
        if self.state == Rx::Asking {
            self.pos = 0;
            self.zrpos();
            self.state = Rx::WaitData;
        }
    }

    /// Declines the offered file; the sender goes on to the next.
    pub fn skip(&mut self) {
        if self.state == Rx::Asking {
            hex_header(&Header::pos(ZSKIP, 0), &mut self.out);
            self.state = Rx::Init;
        }
    }

    /// Gives up, telling the sender.
    pub fn cancel(&mut self, why: &str) {
        if self.state != Rx::Ended {
            self.out.extend_from_slice(&CANCEL);
            self.fail(why);
        }
    }

    /// How long to wait for the sender before [`Receiver::timeout`];
    /// `None` while the caller decides.
    pub fn patience(&self) -> Option<Duration> {
        match self.state {
            Rx::Asking | Rx::Ended => None,
            Rx::Fin { .. } => Some(OVER_AND_OUT),
            _ => Some(PATIENCE),
        }
    }

    /// The sender said nothing for [`Receiver::patience`].
    pub fn timeout(&mut self) {
        match self.state {
            Rx::Fin { .. } => self.state = Rx::Ended,
            Rx::Asking | Rx::Ended => {}
            _ if self.retries >= MAX_RETRIES => {
                self.cancel("The sender stopped answering.");
            }
            Rx::WaitData | Rx::Data => {
                self.retries += 1;
                self.zrpos();
                self.state = Rx::WaitData;
            }
            Rx::Init | Rx::Sinit | Rx::FileInfo => {
                self.retries += 1;
                self.zrinit();
                self.state = Rx::Init;
            }
        }
    }

    pub fn ended(&self) -> bool {
        self.state == Rx::Ended
    }

    /// Bytes for the sender.
    pub fn take_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.out)
    }

    pub fn next_event(&mut self) -> Option<RxEvent> {
        self.events.pop_front()
    }

    fn zrinit(&mut self) {
        let h = Header::flags(ZRINIT, CANFDX | CANOVIO | CANFC32, 0, 0, 0);
        hex_header(&h, &mut self.out);
    }

    fn zrpos(&mut self) {
        hex_header(&Header::pos(ZRPOS, self.pos as u32), &mut self.out);
    }

    fn ack(&mut self) {
        hex_header(&Header::pos(ZACK, self.pos as u32), &mut self.out);
    }

    fn fail(&mut self, why: &str) {
        self.events.push_back(RxEvent::Failed(why.to_owned()));
        self.state = Rx::Ended;
    }

    fn at_pos(&self, h: &Header) -> bool {
        h.position() == self.pos as u32
    }

    fn on_event(&mut self, event: Event) {
        match (self.state, event) {
            (_, Event::Cancel) => self.fail("The sender cancelled the transfer."),
            (_, Event::Header(h)) => self.on_header(h),
            (Rx::Data, Event::Data { bytes, end }) => {
                self.retries = 0;
                self.pos += bytes.len() as u64;
                if !bytes.is_empty() {
                    self.events.push_back(RxEvent::Data(bytes));
                }
                match end {
                    ZCRCW => {
                        self.ack();
                        self.state = Rx::WaitData;
                    }
                    ZCRCQ => self.ack(),
                    ZCRCE => self.state = Rx::WaitData,
                    _ => {}
                }
            }
            (Rx::Data, Event::BadData) => {
                self.retries += 1;
                if self.retries > MAX_RETRIES {
                    self.cancel("Too many damaged blocks.");
                } else {
                    self.zrpos();
                    self.state = Rx::WaitData;
                }
            }
            (Rx::FileInfo, Event::Data { bytes, .. }) => match parse_info(&bytes) {
                Some(info) => {
                    self.events.push_back(RxEvent::File(info));
                    self.state = Rx::Asking;
                }
                None => {
                    hex_header(&Header::pos(ZSKIP, 0), &mut self.out);
                    self.state = Rx::Init;
                }
            },
            (Rx::FileInfo, Event::BadData) => {
                self.zrinit();
                self.state = Rx::Init;
            }
            (Rx::Sinit, Event::Data { .. }) => {
                hex_header(&Header::pos(ZACK, 1), &mut self.out);
                self.state = Rx::Init;
            }
            (Rx::Sinit, Event::BadData) => {
                hex_header(&Header::pos(ZNAK, 0), &mut self.out);
                self.state = Rx::Init;
            }
            _ => {}
        }
    }

    fn on_header(&mut self, h: Header) {
        let state = self.state;
        match h.kind {
            ZRQINIT if state == Rx::Init => self.zrinit(),
            ZSINIT if state == Rx::Init => self.state = Rx::Sinit,
            ZFILE if matches!(state, Rx::Init | Rx::FileInfo) => {
                self.retries = 0;
                self.state = Rx::FileInfo;
            }
            ZDATA if matches!(state, Rx::WaitData | Rx::Data) => {
                if self.at_pos(&h) {
                    self.retries = 0;
                    self.state = Rx::Data;
                } else {
                    self.zrpos();
                    self.state = Rx::WaitData;
                }
            }
            // A ZEOF at another position may have left before our ZRPOS
            // arrived: wait for what the ZRPOS brings.
            ZEOF if matches!(state, Rx::WaitData | Rx::Data) && self.at_pos(&h) => {
                self.events.push_back(RxEvent::FileDone);
                self.zrinit();
                self.state = Rx::Init;
            }
            ZFIN if state == Rx::Init => {
                hex_header(&Header::pos(ZFIN, 0), &mut self.out);
                self.events.push_back(RxEvent::Done);
                self.state = Rx::Fin { o: 0 };
            }
            ZCOMMAND => self.cancel(
                "The server asked FeroSSH to run a command (ZMODEM ZCOMMAND). FeroSSH never \
                 does, so it stopped the transfer.",
            ),
            ZCHALLENGE => hex_header(
                &Header {
                    kind: ZACK,
                    data: h.data,
                },
                &mut self.out,
            ),
            ZFREECNT => hex_header(&Header::pos(ZACK, u32::MAX), &mut self.out),
            ZCAN | ZABORT => self.fail("The sender cancelled the transfer."),
            ZFERR => self.fail("The sender could not read the file."),
            _ => {}
        }
    }
}

/// A ZFILE's subpacket: the name, a NUL, then the size, modification time
/// (octal) and more, separated by spaces.
fn parse_info(bytes: &[u8]) -> Option<FileInfo> {
    let mut parts = bytes.splitn(2, |&b| b == 0);
    let name = parts.next()?;
    if name.is_empty() || name.len() > MAX_NAME {
        return None;
    }
    let rest = parts.next().unwrap_or_default();
    let rest = rest.split(|&b| b == 0).next().unwrap_or_default();
    let text = String::from_utf8_lossy(rest);
    let mut fields = text.split_ascii_whitespace();
    let size = fields.next().and_then(|s| s.parse::<u64>().ok());
    let mtime = fields.next().and_then(|s| u64::from_str_radix(s, 8).ok());
    Some(FileInfo {
        name: name.to_vec(),
        size,
        mtime,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn reads_file_info() {
        assert_eq!(
            parse_info(b"dir/report.pdf\x0012345 14712345671 100644 0 1 12345\x00"),
            Some(FileInfo {
                name: b"dir/report.pdf".to_vec(),
                size: Some(12345),
                mtime: Some(0o14712345671),
            })
        );
        assert_eq!(
            parse_info(b"bare\x00"),
            Some(FileInfo {
                name: b"bare".to_vec(),
                size: None,
                mtime: None,
            })
        );
        assert_eq!(parse_info(b"\x00123"), None);
    }

    #[test]
    fn refuses_commands() {
        let mut r = Receiver::new();
        let _ = r.take_output();
        let mut esc = crate::frame::Escape::default();
        let mut cmd = Vec::new();
        esc.bin_header(&Header::pos(ZCOMMAND, 0), false, &mut cmd);
        esc.subpacket(b"rm -rf ~\x00", crate::frame::ZCRCW, false, &mut cmd);
        r.feed(&cmd);
        assert!(matches!(r.next_event(), Some(RxEvent::Failed(_))));
        assert!(r.ended());
        assert!(r.take_output().starts_with(&CANCEL));
    }
}
