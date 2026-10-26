//! The UI's end of the worker pipes: a reader thread that decodes frames
//! and a writer thread that encodes them, so a slow or stuck worker never
//! blocks the window. Messages for forward channels go straight to the
//! forwarding engine and never queue behind terminal output.

use std::io::{Read, Write};
use std::sync::mpsc::{Receiver, Sender, SyncSender, channel, sync_channel};
use std::thread;

use ferossh_ipc::{Decoder, Message, PROTOCOL_VERSION, Side, encode, is_forward_channel};

/// Messages buffered from the worker; a flooding host is slowed down to
/// what the window can show.
const BACKLOG: usize = 256;

#[derive(Debug)]
pub enum LinkEvent {
    Message(Message),
    /// The worker exited, broke the protocol or spoke another version.
    Closed(String),
}

#[derive(Debug)]
pub struct Link {
    tx: Sender<Message>,
    rx: Receiver<LinkEvent>,
}

fn reader(
    mut input: impl Read,
    tx: &SyncSender<LinkEvent>,
    notify: &(impl Fn() + Send),
    divert: &(impl Fn(Message) + Send),
) -> String {
    let mut decoder = Decoder::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut greeted = false;
    loop {
        let n = match input.read(&mut buf) {
            Ok(0) => return "The SSH worker exited.".into(),
            Err(e) => return format!("The SSH worker's pipe failed: {e}."),
            Ok(n) => n,
        };
        decoder.feed(buf.get(..n).unwrap_or_default());
        loop {
            let msg = match decoder.next_message() {
                Ok(Some(m)) => m,
                Ok(None) => break,
                Err(e) => return format!("The SSH worker sent a malformed message ({e})."),
            };
            if !greeted {
                match msg {
                    Message::Hello { version } if version == PROTOCOL_VERSION => {
                        greeted = true;
                        continue;
                    }
                    _ => return "The SSH worker speaks another protocol version.".into(),
                }
            }
            if msg.sent_by() == Side::Ui {
                return "The SSH worker sent a message only the window sends; it was stopped."
                    .into();
            }
            if msg.channel().is_some_and(is_forward_channel) {
                divert(msg);
                continue;
            }
            if tx.send(LinkEvent::Message(msg)).is_err() {
                return String::new();
            }
            notify();
        }
    }
}

impl Link {
    /// Starts both threads and greets the worker. `notify` runs whenever
    /// an event is queued; messages for forward channels go to `divert`
    /// instead.
    pub fn start(
        input: impl Read + Send + 'static,
        mut output: impl Write + Send + 'static,
        notify: impl Fn() + Send + Clone + 'static,
        divert: impl Fn(Message) + Send + 'static,
    ) -> std::io::Result<Self> {
        let (event_tx, rx) = sync_channel(BACKLOG);
        let (tx, msg_rx) = channel::<Message>();
        thread::Builder::new()
            .name("worker reader".into())
            .spawn(move || {
                let why = reader(input, &event_tx, &notify, &divert);
                if event_tx.send(LinkEvent::Closed(why)).is_ok() {
                    notify();
                }
            })?;
        thread::Builder::new()
            .name("worker writer".into())
            .spawn(move || {
                let mut frame = Vec::new();
                while let Ok(msg) = msg_rx.recv() {
                    frame.clear();
                    if encode(&msg, &mut frame).is_ok()
                        && output
                            .write_all(&frame)
                            .and_then(|()| output.flush())
                            .is_err()
                    {
                        return;
                    }
                }
            })?;
        let link = Self { tx, rx };
        link.send(Message::Hello {
            version: PROTOCOL_VERSION,
        });
        Ok(link)
    }

    pub fn send(&self, msg: Message) {
        let _ = self.tx.send(msg);
    }

    /// Sends to the worker from another thread.
    pub fn sender(&self) -> impl Fn(Message) + Send + Sync + 'static {
        let tx = std::sync::Mutex::new(self.tx.clone());
        move |msg| {
            if let Ok(tx) = tx.lock() {
                let _ = tx.send(msg);
            }
        }
    }

    pub fn try_recv(&self) -> Option<LinkEvent> {
        self.rx.try_recv().ok()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    fn frames(msgs: &[Message]) -> std::io::Cursor<Vec<u8>> {
        let mut out = Vec::new();
        for m in msgs {
            encode(m, &mut out).unwrap();
        }
        std::io::Cursor::new(out)
    }

    fn run(msgs: &[Message]) -> (String, Vec<Message>) {
        let (tx, rx) = sync_channel(BACKLOG);
        let why = reader(frames(msgs), &tx, &|| {}, &|_| {});
        let got = rx
            .try_iter()
            .map(|e| match e {
                LinkEvent::Message(m) => m,
                LinkEvent::Closed(why) => panic!("{why}"),
            })
            .collect();
        (why, got)
    }

    #[test]
    fn a_worker_sending_ui_messages_is_stopped() {
        let hello = Message::Hello {
            version: PROTOCOL_VERSION,
        };
        let notice = Message::Notice {
            channel: 1,
            text: "hi".into(),
        };
        let (why, got) = run(&[hello.clone(), notice.clone()]);
        assert_eq!(why, "The SSH worker exited.");
        assert_eq!(got, vec![notice.clone()]);

        let answers = Message::Answers {
            channel: 1,
            answers: ferossh_ipc::Secrets(vec!["pw".into()]),
        };
        let (why, got) = run(&[hello, answers, notice]);
        assert!(why.contains("only the window sends"), "{why}");
        assert!(got.is_empty());
    }

    #[test]
    fn the_worker_must_greet_first() {
        let (why, _) = run(&[Message::Close { channel: 1 }]);
        assert!(why.contains("another protocol version"));
    }
}
