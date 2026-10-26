//! ZMODEM in a shell's channel: watches the output for `rz` or `sz`
//! starting, asks the UI, and runs the transfer from [`ferossh_zmodem`],
//! with file contents coming from and going to the UI.
//!
//! [`Zmodem`] sits between the server's bytes and the terminal and has no
//! I/O of its own: the shell's pump (SSH or Telnet) feeds it and carries
//! out each [`Step`]. While a transfer runs, the terminal sees nothing and
//! typing goes nowhere, but Ctrl+C cancels.

use std::time::Duration;

use ferossh_ipc::{ChannelId, MAX_DATA_CHUNK, Message, ZmodemAnswer};
use ferossh_zmodem::{
    Detector, MAX_FILE, Receiver, RxEvent, Sender, Start, TxEvent, Upload, frame::CANCEL,
};

/// Output held back while the UI decides; a sender says little then, so
/// more than this is dropped.
const MAX_HELD: usize = 64 * 1024;

/// Sleeps until `at`, or for ever without one: the pumps' ZMODEM timer.
pub(crate) async fn sleep_until(at: Option<tokio::time::Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

/// How long a possible start's first bytes wait for the rest.
const SHOW_HELD: Duration = Duration::from_millis(50);

/// What the pump does next: show, tell the UI, send to the server.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Step {
    pub terminal: Vec<u8>,
    pub ui: Vec<Message>,
    pub server: Vec<u8>,
}

#[derive(Debug)]
enum Mode {
    Watching,
    /// A start seen; the UI decides. The output from the start on waits.
    Asking {
        start: Start,
        held: Vec<u8>,
    },
    Receiving(Box<Receiver>),
    Sending(Box<Sender>),
}

#[derive(Debug)]
pub(crate) struct Zmodem {
    channel: ChannelId,
    detector: Detector,
    mode: Mode,
}

impl Zmodem {
    pub fn new(channel: ChannelId) -> Self {
        Self {
            channel,
            detector: Detector::new(),
            mode: Mode::Watching,
        }
    }

    /// Bytes from the server.
    pub fn on_server(&mut self, bytes: &[u8]) -> Step {
        let mut step = Step::default();
        self.server_bytes(bytes, &mut step);
        step
    }

    /// A message from the UI, if it is one for ZMODEM (typing counts while
    /// a transfer runs); `None` leaves it to the pump.
    pub fn on_ui(&mut self, msg: &Message) -> Option<Step> {
        let mut step = Step::default();
        match (msg, &mut self.mode) {
            (Message::ZmodemAnswer { answer, .. }, Mode::Asking { start, held }) => {
                let start = *start;
                let held = std::mem::take(held);
                self.mode = Mode::Watching;
                self.answered(start, answer, held, &mut step);
            }
            (Message::ZmodemFileAnswer { accept, .. }, Mode::Receiving(rx)) => {
                if *accept {
                    rx.accept();
                } else {
                    rx.skip();
                }
                self.drain(&mut step);
            }
            (Message::ZmodemChunk { offset, bytes, .. }, Mode::Sending(tx)) => {
                tx.provide(*offset, bytes);
                self.drain(&mut step);
            }
            (Message::ZmodemCancel { .. }, _) => self.cancel("Cancelled.", &mut step),
            (Message::Data { bytes, .. }, mode) if !matches!(mode, Mode::Watching) => {
                // Ctrl+C stops the transfer; other keys go nowhere.
                if bytes.contains(&0x03) {
                    self.cancel("Cancelled with Ctrl+C.", &mut step);
                }
            }
            (
                Message::ZmodemAnswer { .. }
                | Message::ZmodemFileAnswer { .. }
                | Message::ZmodemChunk { .. },
                _,
            ) => {
                // Late, or for a transfer that already ended.
            }
            _ => return None,
        }
        Some(step)
    }

    /// How long the server may stay quiet before [`Zmodem::timeout`], if
    /// anything waits on it.
    pub fn patience(&self) -> Option<Duration> {
        match &self.mode {
            Mode::Receiving(rx) => rx.patience(),
            Mode::Sending(tx) => tx.patience(),
            Mode::Watching => self.detector.holding().then_some(SHOW_HELD),
            Mode::Asking { .. } => None,
        }
    }

    /// The server said nothing for [`Zmodem::patience`].
    pub fn timeout(&mut self) -> Step {
        let mut step = Step::default();
        match &mut self.mode {
            Mode::Receiving(rx) => rx.timeout(),
            Mode::Sending(tx) => tx.timeout(),
            // What looked like it might start a transfer did not.
            Mode::Watching => step.terminal = self.detector.flush(),
            Mode::Asking { .. } => {}
        }
        self.drain(&mut step);
        step
    }

    fn server_bytes(&mut self, mut bytes: &[u8], step: &mut Step) {
        loop {
            let used = match &mut self.mode {
                Mode::Watching => {
                    let (shown, found) = self.detector.scan(bytes);
                    step.terminal.extend(shown);
                    if let Some(found) = found {
                        step.ui.push(Message::ZmodemStart {
                            channel: self.channel,
                            upload: found.start == Start::Send,
                        });
                        let mut held = found.rest;
                        held.truncate(MAX_HELD);
                        self.mode = Mode::Asking {
                            start: found.start,
                            held,
                        };
                    }
                    return;
                }
                Mode::Asking { held, .. } => {
                    let room = MAX_HELD.saturating_sub(held.len());
                    held.extend_from_slice(bytes.get(..room.min(bytes.len())).unwrap_or_default());
                    return;
                }
                Mode::Receiving(rx) => rx.feed(bytes),
                Mode::Sending(tx) => tx.feed(bytes),
            };
            self.drain(step);
            // The transfer ended part way: the rest is output again.
            match bytes.get(used..) {
                Some(rest) if !rest.is_empty() && matches!(self.mode, Mode::Watching) => {
                    bytes = rest;
                }
                _ => return,
            }
        }
    }

    fn answered(&mut self, start: Start, answer: &ZmodemAnswer, held: Vec<u8>, step: &mut Step) {
        match (start, answer) {
            (_, ZmodemAnswer::Off) => {
                // Shown as it came; the detector only looks past the
                // start, or it would find it again.
                let (head, rest) = held.split_at(held.len().min(6));
                step.terminal.extend_from_slice(head);
                self.server_bytes(rest, step);
            }
            (Start::Receive, ZmodemAnswer::Receive) => {
                self.mode = Mode::Receiving(Box::new(Receiver::new()));
                self.drain(step);
                self.server_bytes(&held, step);
            }
            (Start::Send, ZmodemAnswer::Send(files))
                if !files.is_empty() && files.iter().all(|f| f.size <= MAX_FILE) =>
            {
                let uploads = files
                    .iter()
                    .map(|f| Upload {
                        name: f.name.clone(),
                        size: f.size,
                        mtime: f.mtime,
                    })
                    .collect();
                self.mode = Mode::Sending(Box::new(Sender::new(uploads)));
                self.server_bytes(&held, step);
            }
            // Declined, or an answer that does not fit the question.
            _ => {
                step.server.extend_from_slice(&CANCEL);
                step.ui.push(Message::ZmodemEnd {
                    channel: self.channel,
                    error: "Declined.".into(),
                });
            }
        }
    }

    fn cancel(&mut self, why: &str, step: &mut Step) {
        match &mut self.mode {
            Mode::Receiving(rx) => rx.cancel(why),
            Mode::Sending(tx) => tx.cancel(why),
            Mode::Asking { .. } => {
                self.mode = Mode::Watching;
                step.server.extend_from_slice(&CANCEL);
                step.ui.push(Message::ZmodemEnd {
                    channel: self.channel,
                    error: why.into(),
                });
                return;
            }
            Mode::Watching => return,
        }
        self.drain(step);
    }

    /// Moves what the transfer produced into `step`, and goes back to
    /// watching once it has ended.
    fn drain(&mut self, step: &mut Step) {
        let channel = self.channel;
        let ended = match &mut self.mode {
            Mode::Receiving(rx) => {
                step.server.extend(rx.take_output());
                while let Some(event) = rx.next_event() {
                    match event {
                        RxEvent::File(info) => step.ui.push(Message::ZmodemFile {
                            channel,
                            name: info.name,
                            size: info.size,
                        }),
                        RxEvent::Data(bytes) => {
                            for chunk in bytes.chunks(MAX_DATA_CHUNK) {
                                step.ui.push(Message::ZmodemData {
                                    channel,
                                    bytes: chunk.to_vec(),
                                });
                            }
                        }
                        RxEvent::FileDone => step.ui.push(Message::ZmodemFileDone {
                            channel,
                            skipped: false,
                        }),
                        RxEvent::Done => step.ui.push(Message::ZmodemEnd {
                            channel,
                            error: String::new(),
                        }),
                        RxEvent::Failed(error) => {
                            step.ui.push(Message::ZmodemEnd { channel, error });
                        }
                    }
                }
                rx.ended()
            }
            Mode::Sending(tx) => {
                step.server.extend(tx.take_output());
                while let Some(event) = tx.next_event() {
                    step.ui.push(match event {
                        TxEvent::Read { file, offset, len } => Message::ZmodemRead {
                            channel,
                            file,
                            offset,
                            len,
                        },
                        TxEvent::FileDone { skipped } => {
                            Message::ZmodemFileDone { channel, skipped }
                        }
                        TxEvent::Done => Message::ZmodemEnd {
                            channel,
                            error: String::new(),
                        },
                        TxEvent::Failed(error) => Message::ZmodemEnd { channel, error },
                    });
                }
                tx.ended()
            }
            Mode::Watching | Mode::Asking { .. } => false,
        };
        if ended {
            self.mode = Mode::Watching;
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use ferossh_ipc::ZmodemUpload;

    const SZ_START: &[u8] = b"rz\r**\x18B00000000000000\r\x8a\x11";
    const RZ_START: &[u8] = b"rz waiting to receive.**\x18B0100000023be50\r\x8a\x11";

    fn answer(answer: ZmodemAnswer) -> Message {
        Message::ZmodemAnswer { channel: 4, answer }
    }

    #[test]
    fn asks_before_anything_happens() {
        let mut z = Zmodem::new(4);
        let step = z.on_server(SZ_START);
        assert_eq!(step.terminal, b"rz\r");
        assert_eq!(
            step.ui,
            [Message::ZmodemStart {
                channel: 4,
                upload: false
            }]
        );
        assert!(step.server.is_empty());
        // Output waits meanwhile.
        assert_eq!(z.on_server(b"more").terminal, b"");
        let step = z.on_ui(&answer(ZmodemAnswer::Receive)).unwrap();
        // ZRINIT goes out.
        assert!(step.server.starts_with(b"**\x18B01"));
    }

    #[test]
    fn off_shows_the_output_as_it_came() {
        let mut z = Zmodem::new(4);
        let _ = z.on_server(SZ_START);
        let _ = z.on_server(b" and after");
        let step = z.on_ui(&answer(ZmodemAnswer::Off)).unwrap();
        assert_eq!(
            step.terminal,
            b"**\x18B00000000000000\r\x8a\x11 and after".to_vec()
        );
        assert!(step.ui.is_empty() && step.server.is_empty());
        assert_eq!(z.on_server(b"x").terminal, b"x");
    }

    #[test]
    fn declining_cancels_the_server() {
        let mut z = Zmodem::new(4);
        let _ = z.on_server(RZ_START);
        let step = z.on_ui(&answer(ZmodemAnswer::Decline)).unwrap();
        assert_eq!(step.server, CANCEL);
        assert!(matches!(step.ui[..], [Message::ZmodemEnd { .. }]));
        // A receive answer to rz is not one either.
        let _ = z.on_server(RZ_START);
        let step = z.on_ui(&answer(ZmodemAnswer::Receive)).unwrap();
        assert_eq!(step.server, CANCEL);
    }

    #[test]
    fn sending_asks_the_ui_for_the_file() {
        let mut z = Zmodem::new(4);
        let _ = z.on_server(RZ_START);
        let step = z
            .on_ui(&answer(ZmodemAnswer::Send(vec![ZmodemUpload {
                name: b"a".to_vec(),
                size: 3,
                mtime: 0,
            }])))
            .unwrap();
        // ZFILE goes out at once: the held ZRINIT was enough.
        assert!(!step.server.is_empty());
        // Typing goes nowhere; Ctrl+C cancels.
        let typed = Message::Data {
            channel: 4,
            bytes: b"ls\r".to_vec(),
        };
        assert_eq!(z.on_ui(&typed), Some(Step::default()));
        let ctrl_c = Message::Data {
            channel: 4,
            bytes: vec![3],
        };
        let step = z.on_ui(&ctrl_c).unwrap();
        assert!(step.server.starts_with(&CANCEL));
        assert!(matches!(step.ui[..], [Message::ZmodemEnd { .. }]));
        // Back to normal.
        assert_eq!(z.on_ui(&typed), None);
    }
}
