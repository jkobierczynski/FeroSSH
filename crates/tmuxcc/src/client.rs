//! Finds control mode in a pane's output and splits it into notifications
//! and replies.

use std::collections::VecDeque;

use crate::protocol::{Boundary, Guard, Notification, parse_boundary, parse_notification};

/// What `tmux -CC` writes first: a DCS that the terminal ignores.
const INTRO: &[u8] = b"\x1bP1000p";
/// What it writes last, after `%exit`: the DCS's string terminator.
const OUTRO: &[u8] = b"\x1b\\";
/// Longer lines are dropped. `%output` lines hold what a pane wrote in one
/// read, escaped, so they stay far below this.
const MAX_LINE: usize = 16 << 20;
/// A reply keeps at most this many bytes; the rest is dropped.
const MAX_REPLY: usize = 64 << 20;

/// What the stream held.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event<T> {
    /// Ordinary terminal output, for the pane itself.
    Terminal(Vec<u8>),
    /// Control mode began.
    Started,
    Notify(Notification),
    /// The reply to the command tagged `T` begins. Output that comes after
    /// this happened after the command ran.
    ReplyStarted(T),
    /// The reply to the command tagged `T`: its lines, and whether tmux
    /// ran it (`%end`) or refused it (`%error`, with the message as lines).
    Reply {
        tag: T,
        ok: bool,
        lines: Vec<Vec<u8>>,
    },
    /// Control mode is over; what follows is terminal output again.
    Ended,
}

#[derive(Debug)]
struct Block<T> {
    guard: Guard,
    /// `None` for a block that answers no command of ours (the attach
    /// command's own, or another client's).
    tag: Option<T>,
    lines: Vec<Vec<u8>>,
    bytes: usize,
}

/// One pane's output stream, in and out of control mode. Commands are
/// tagged; tmux answers them in order, so each reply goes back to its
/// command's tag.
#[derive(Debug)]
pub struct Client<T> {
    control: bool,
    /// Bytes of [`INTRO`] matched so far, in terminal mode.
    matched: usize,
    line: Vec<u8>,
    /// The current line grew past [`MAX_LINE`]; drop it.
    overlong: bool,
    /// The first line after the intro must be a `%begin`, or this was not
    /// tmux.
    first_line: bool,
    block: Option<Block<T>>,
    pending: VecDeque<T>,
}

impl<T: Clone> Default for Client<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Clone> Client<T> {
    pub fn new() -> Self {
        Self {
            control: false,
            matched: 0,
            line: Vec::new(),
            overlong: false,
            first_line: false,
            block: None,
            pending: VecDeque::new(),
        }
    }

    /// In control mode.
    pub fn is_control(&self) -> bool {
        self.control
    }

    /// Commands sent and not yet answered.
    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    /// Back to terminal mode with nothing pending, as after a dropped
    /// connection.
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// The bytes to write for command `line`, which tmux answers with a
    /// reply tagged `tag`. `None` outside control mode. Line breaks in
    /// `line` are dropped: they would start another command.
    pub fn command(&mut self, line: &str, tag: T) -> Option<Vec<u8>> {
        if !self.control {
            return None;
        }
        let mut out: Vec<u8> = line.bytes().filter(|&b| b != b'\n' && b != b'\r').collect();
        out.push(b'\n');
        self.pending.push_back(tag);
        Some(out)
    }

    /// Reads bytes from the pane.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Event<T>> {
        let mut events = Vec::new();
        let mut start = 0;
        let mut i = 0;
        while let Some(&b) = bytes.get(i) {
            i += 1;
            if !self.control {
                if INTRO.get(self.matched) == Some(&b) {
                    self.matched += 1;
                } else {
                    self.matched = usize::from(b == 0x1b);
                }
                if self.matched == INTRO.len() {
                    // The terminal gets the intro too: an empty DCS it
                    // skips, closed by the outro later.
                    push_terminal(&mut events, bytes.get(start..i));
                    start = i;
                    self.matched = 0;
                    self.control = true;
                    self.first_line = true;
                    events.push(Event::Started);
                }
                continue;
            }
            if b == b'\n' {
                let mut line = std::mem::take(&mut self.line);
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                if std::mem::take(&mut self.overlong) {
                    continue;
                }
                if std::mem::take(&mut self.first_line) && !line.starts_with(b"%begin ") {
                    // Not tmux after all: give the line back.
                    self.control = false;
                    self.pending.clear();
                    line.push(b'\n');
                    events.push(Event::Ended);
                    events.push(Event::Terminal(line));
                    start = i;
                    continue;
                }
                self.on_line(line, &mut events);
                start = i;
                continue;
            }
            if self.line.len() < MAX_LINE {
                self.line.push(b);
            } else {
                self.line.clear();
                self.overlong = true;
            }
            if self.block.is_none() && self.line == OUTRO {
                self.line.clear();
                self.control = false;
                self.pending.clear();
                self.first_line = false;
                events.push(Event::Ended);
                events.push(Event::Terminal(OUTRO.to_vec()));
                start = i;
            }
        }
        if !self.control {
            push_terminal(&mut events, bytes.get(start..));
        }
        events
    }

    fn on_line(&mut self, line: Vec<u8>, events: &mut Vec<Event<T>>) {
        if let Some(block) = &mut self.block {
            let closing = match parse_boundary(&line) {
                Some(Boundary::End(g)) if g == block.guard => Some(true),
                Some(Boundary::Error(g)) if g == block.guard => Some(false),
                _ => None,
            };
            match closing {
                Some(ok) => {
                    if let Some(block) = self.block.take()
                        && let Some(tag) = block.tag
                    {
                        events.push(Event::Reply {
                            tag,
                            ok,
                            lines: block.lines,
                        });
                    }
                }
                None if block.bytes + line.len() <= MAX_REPLY => {
                    block.bytes += line.len();
                    block.lines.push(line);
                }
                None => {}
            }
            return;
        }
        match parse_boundary(&line) {
            Some(Boundary::Begin(guard)) => {
                let tag = if guard.flags & 1 == 1 {
                    self.pending.pop_front()
                } else {
                    None
                };
                if let Some(t) = &tag {
                    events.push(Event::ReplyStarted(t.clone()));
                }
                self.block = Some(Block {
                    guard,
                    tag,
                    lines: Vec::new(),
                    bytes: 0,
                });
            }
            // A stray end outside a block.
            Some(_) => {}
            None => events.push(Event::Notify(parse_notification(&line))),
        }
    }
}

fn push_terminal<T>(events: &mut Vec<Event<T>>, bytes: Option<&[u8]>) {
    if let Some(b) = bytes.filter(|b| !b.is_empty()) {
        events.push(Event::Terminal(b.to_vec()));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    /// What tmux 3.4 wrote for `tmux -CC new -A -s main`, a command and a
    /// detach.
    const SESSION: &[u8] = b"$ tmux -CC\r\n\x1bP1000p%begin 1792899292 264 0\r\n\
        %end 1792899292 264 0\r\n%window-add @0\r\n%sessions-changed\r\n\
        %session-changed $0 main\r\n%output %0 \\033[?2004h$ \r\n\
        %begin 1792899293 270 1\r\n3.4\r\n%end 1792899293 270 1\r\n\
        %begin 1792899299 285 1\r\nparse error: unknown command: bogus\r\n\
        %error 1792899299 285 1\r\n%exit\r\n\x1b\\[detached (from session main)]\r\n";

    fn all(client: &mut Client<u32>, data: &[u8], chunk: usize) -> Vec<Event<u32>> {
        let mut out = Vec::new();
        for c in data.chunks(chunk) {
            out.extend(client.feed(c));
        }
        // Join split terminal output, as a caller would see it.
        let mut joined: Vec<Event<u32>> = Vec::new();
        for e in out {
            if let (Some(Event::Terminal(prev)), Event::Terminal(more)) = (joined.last_mut(), &e) {
                prev.extend_from_slice(more);
            } else {
                joined.push(e);
            }
        }
        joined
    }

    #[test]
    fn a_whole_session_in_any_chunks() {
        for chunk in [1, 2, 3, 7, 64, SESSION.len()] {
            let mut c = Client::new();
            let mut events = all(&mut c, &SESSION[..30], chunk);
            assert!(c.is_control());
            assert_eq!(
                c.command("display-message -p '#{version}'", 1)
                    .unwrap()
                    .last(),
                Some(&b'\n')
            );
            assert_eq!(
                c.command("bogus\nkill-server", 2).unwrap(),
                b"boguskill-server\n"
            );
            events.extend(all(&mut c, &SESSION[30..], chunk));
            assert!(!c.is_control());
            assert_eq!(c.pending(), 0);
            assert_eq!(
                events,
                vec![
                    Event::Terminal(b"$ tmux -CC\r\n\x1bP1000p".to_vec()),
                    Event::Started,
                    Event::Notify(Notification::WindowAdd { window: 0 }),
                    Event::Notify(Notification::SessionsChanged),
                    Event::Notify(Notification::SessionChanged {
                        session: 0,
                        name: "main".into()
                    }),
                    Event::Notify(Notification::Output {
                        pane: 0,
                        data: b"\x1b[?2004h$ ".to_vec()
                    }),
                    Event::ReplyStarted(1),
                    Event::Reply {
                        tag: 1,
                        ok: true,
                        lines: vec![b"3.4".to_vec()]
                    },
                    Event::ReplyStarted(2),
                    Event::Reply {
                        tag: 2,
                        ok: false,
                        lines: vec![b"parse error: unknown command: bogus".to_vec()]
                    },
                    Event::Notify(Notification::Exit {
                        reason: String::new()
                    }),
                    Event::Ended,
                    Event::Terminal(b"\x1b\\[detached (from session main)]\r\n".to_vec()),
                ],
                "chunks of {chunk}"
            );
        }
    }

    #[test]
    fn reply_lines_that_look_like_protocol() {
        let mut c = Client::new();
        c.feed(b"\x1bP1000p%begin 1 1 0\n%end 1 1 0\n");
        c.command("capture-pane -p", 7);
        // A pane can print anything, but not the guard's exact numbers.
        let events =
            c.feed(b"%begin 5 9 1\n%end 5 8 1\n%output %1 x\n\x1b\\\n%end 5 9 0\n%end 5 9 1\n");
        assert_eq!(
            events,
            vec![
                Event::ReplyStarted(7),
                Event::Reply {
                    tag: 7,
                    ok: true,
                    lines: vec![
                        b"%end 5 8 1".to_vec(),
                        b"%output %1 x".to_vec(),
                        b"\x1b\\".to_vec(),
                        b"%end 5 9 0".to_vec(),
                    ]
                }
            ]
        );
        assert!(c.is_control());
    }

    #[test]
    fn not_tmux_after_all() {
        let mut c: Client<u32> = Client::new();
        let events = c.feed(b"\x1bP1000phello\r\nworld");
        assert!(!c.is_control());
        assert_eq!(
            events,
            vec![
                Event::Terminal(b"\x1bP1000p".to_vec()),
                Event::Started,
                Event::Ended,
                Event::Terminal(b"hello\n".to_vec()),
                Event::Terminal(b"world".to_vec()),
            ]
        );
        assert_eq!(c.command("x", 1), None);
    }

    #[test]
    fn near_misses_stay_terminal_output() {
        let mut c: Client<u32> = Client::new();
        let events = all(&mut c, b"\x1bP1000q\x1b\x1bP100\x1b[0m", 1);
        assert!(!c.is_control());
        assert_eq!(
            events,
            vec![Event::Terminal(b"\x1bP1000q\x1b\x1bP100\x1b[0m".to_vec())]
        );
    }

    #[test]
    fn overlong_lines_are_dropped() {
        let mut c: Client<u32> = Client::new();
        c.feed(b"\x1bP1000p%begin 1 1 0\n%end 1 1 0\n%output %1 ");
        let big = vec![b'a'; MAX_LINE + 10];
        assert!(c.feed(&big).is_empty());
        let events = c.feed(b"\n%window-add @2\n");
        assert_eq!(
            events,
            vec![Event::Notify(Notification::WindowAdd { window: 2 })]
        );
    }

    #[test]
    fn replies_from_others_are_skipped() {
        let mut c: Client<u32> = Client::new();
        c.feed(b"\x1bP1000p%begin 1 1 0\n%end 1 1 0\n");
        c.command("list-windows", 3);
        let events = c.feed(b"%begin 2 2 0\nx\n%end 2 2 0\n%begin 2 3 1\n@1\n%end 2 3 1\n");
        assert_eq!(
            events,
            vec![
                Event::ReplyStarted(3),
                Event::Reply {
                    tag: 3,
                    ok: true,
                    lines: vec![b"@1".to_vec()]
                }
            ]
        );
    }
}
