//! The UI's end of the worker pipes: a reader thread that decodes frames
//! and a writer thread that encodes them, so a slow or stuck worker never
//! blocks the window. Messages for forward channels go straight to the
//! forwarding engine and never queue behind terminal output, and the
//! bytes of a relayed stream (a jump host's stream to the next hop, or a
//! `ProxyCommand`'s) go straight on to the other end ([`Relays`]).

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::mpsc::{Receiver, Sender, SyncSender, channel, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread;

use ferossh_ipc::{
    ChannelId, Decoder, Message, PROTOCOL_VERSION, Side, encode, is_forward_channel,
};

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
    relays: Relays,
}

type Sink = Arc<dyn Fn(Message) + Send + Sync>;

/// The streams of a worker whose messages go to another end instead of
/// the window: a jump host's stream to the next hop goes to the next
/// hop's worker and back, a `ProxyCommand`'s stream to the command. The
/// window never looks at those bytes; they are the next hop's encrypted
/// SSH connection.
#[derive(Clone, Default)]
pub struct Relays(Arc<Mutex<HashMap<ChannelId, Sink>>>);

impl std::fmt::Debug for Relays {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Relays").finish_non_exhaustive()
    }
}

impl Relays {
    /// Sends what the worker says on stream `channel` to `to`.
    pub fn insert(&self, channel: ChannelId, to: impl Fn(Message) + Send + Sync + 'static) {
        if let Ok(mut map) = self.0.lock() {
            map.insert(channel, Arc::new(to));
        }
    }

    pub fn remove(&self, channel: ChannelId) {
        if let Ok(mut map) = self.0.lock() {
            map.remove(&channel);
        }
    }

    /// Hands `msg` on if its stream is relayed: data, credit and end of
    /// data go across as they are. When the worker ends the stream, the
    /// other end is closed and the window still hears why. Gives back
    /// what is not relayed.
    fn relay(&self, msg: Message) -> Option<Message> {
        let channel = msg.channel()?;
        let sink = self.0.lock().ok()?.get(&channel).cloned();
        let Some(sink) = sink else {
            return Some(msg);
        };
        match msg {
            Message::Data { .. } | Message::Window { .. } | Message::Eof { .. } => {
                sink(msg);
                None
            }
            Message::Disconnected { .. } | Message::Close { .. } => {
                self.remove(channel);
                sink(Message::Close { channel });
                Some(msg)
            }
            other => Some(other),
        }
    }
}

fn reader(
    mut input: impl Read,
    tx: &SyncSender<LinkEvent>,
    notify: &(impl Fn() + Send),
    divert: &(impl Fn(Message) + Send),
    relays: &Relays,
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
            let Some(msg) = relays.relay(msg) else {
                continue;
            };
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
        let relays = Relays::default();
        let reader_relays = relays.clone();
        thread::Builder::new()
            .name("worker reader".into())
            .spawn(move || {
                let why = reader(input, &event_tx, &notify, &divert, &reader_relays);
                // The other ends of its relayed streams hear it is gone.
                let ends: Vec<(ChannelId, Sink)> = reader_relays
                    .0
                    .lock()
                    .map(|mut m| m.drain().collect())
                    .unwrap_or_default();
                for (channel, sink) in ends {
                    sink(Message::Close { channel });
                }
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
        let link = Self { tx, rx, relays };
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

    /// The streams relayed past the window.
    pub fn relays(&self) -> &Relays {
        &self.relays
    }

    /// Relays stream `channel` between this link's worker and `other`'s,
    /// both ways.
    pub fn relay_with(&self, other: &Link, channel: ChannelId) {
        self.relays.insert(channel, other.sender());
        other.relays.insert(channel, self.sender());
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
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
        let why = reader(frames(msgs), &tx, &|| {}, &|_| {}, &Relays::default());
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
    fn relayed_streams_skip_the_window() {
        let hello = Message::Hello {
            version: PROTOCOL_VERSION,
        };
        let data = Message::Data {
            channel: 5,
            bytes: b"ssh bytes".to_vec(),
        };
        let other = Message::Data {
            channel: 6,
            bytes: b"shell".to_vec(),
        };
        let gone = Message::Disconnected {
            channel: 5,
            reason: "x".into(),
        };
        let relays = Relays::default();
        let across = Arc::new(Mutex::new(Vec::new()));
        let seen = across.clone();
        relays.insert(5, move |m| seen.lock().unwrap().push(m));
        let (tx, rx) = sync_channel(BACKLOG);
        let msgs = [hello, data.clone(), other.clone(), gone.clone(), data];
        reader(frames(&msgs), &tx, &|| {}, &|_| {}, &relays);
        let got: Vec<Message> = rx
            .try_iter()
            .map(|e| match e {
                LinkEvent::Message(m) => m,
                LinkEvent::Closed(why) => panic!("{why}"),
            })
            .collect();
        // The window hears of the end, and of data after it, but never
        // sees the stream's bytes while it is relayed.
        assert_eq!(got[..2], [other, gone]);
        let across = across.lock().unwrap();
        assert!(matches!(across[0], Message::Data { channel: 5, .. }));
        assert_eq!(across[1], Message::Close { channel: 5 });
        assert_eq!(across.len(), 2);
    }

    #[test]
    fn the_worker_must_greet_first() {
        let (why, _) = run(&[Message::Close { channel: 1 }]);
        assert!(why.contains("another protocol version"));
    }
}
