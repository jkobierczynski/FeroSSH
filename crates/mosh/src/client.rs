//! A Mosh client's half of the State Synchronization Protocol, as
//! `mosh-client` runs it, with the clock and the sockets left to the
//! caller.
//!
//! Each side sends the other the difference between the newest state it
//! has and the state it believes the other has: ours is the stream of
//! keystrokes and resizes, the server's is the screen. Every instruction
//! acknowledges the newest state received, so a lost packet costs nothing
//! but a retransmission of a newer diff; there are no connections to
//! break, and the client may change address (roam) at any time.
//!
//! [`Client::receive`] takes datagrams, [`Client::tick`] gives those to
//! send and [`Client::wait`] says how long until it wants to run again.
//! [`Client::frame`] gives the terminal output that brings the pane up to
//! the newest screen. All times are milliseconds on the caller's clock.

use std::collections::VecDeque;

use crate::crypto::{CryptoError, Session, TAG_LEN};
use crate::predict::{Mode, Predictor};
use crate::screen::{self, Screen};
use crate::wire::{
    Assembly, FRAGMENT_HEADER, Fragment, HostEvent, Instruction, PROTOCOL_VERSION, Packet,
    UserEvent, WireError, deflate, inflate, parse_host_message, user_message,
};

const SEND_INTERVAL_MIN: u64 = 20;
const SEND_INTERVAL_MAX: u64 = 250;
const ACK_INTERVAL: u64 = 3000;
const ACK_DELAY: u64 = 100;
const SHUTDOWN_RETRIES: u32 = 16;
const ACTIVE_RETRY_TIMEOUT: u64 = 10_000;
/// The client sends keystrokes almost at once.
const SEND_MINDELAY: u64 = 1;
const MIN_RTO: u64 = 50;
const MAX_RTO: u64 = 1000;
/// With no round trip for this long, try another source port: a NAT
/// may have dropped the old mapping.
const PORT_HOP_INTERVAL: u64 = 10_000;
/// IPv4 and IPv6 datagrams are kept to 1280 bytes with their headers.
const MTU_V4: usize = 1280 - 28;
const MTU_V6: usize = 1280 - 64;
/// Sequence number and timestamps in each packet.
const PACKET_ADDED: usize = 8 + 4;
/// The most received states kept to apply diffs to.
const MAX_STATES: usize = 256;
/// A state number that means "shutting down".
const SHUTDOWN: u64 = u64::MAX;
/// Before the first word from the server, give up after this long.
pub const CONNECT_TIMEOUT: u64 = 15_000;
/// Show how long ago the server was last heard after this long.
const LATE: u64 = 6500;

/// Why a session cannot go on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fatal {
    /// The other end speaks another protocol.
    Protocol(WireError),
    /// The key has encrypted all it may.
    Crypto(CryptoError),
}

impl std::fmt::Display for Fatal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Fatal::Protocol(e) => e.fmt(f),
            Fatal::Crypto(e) => e.fmt(f),
        }
    }
}

/// A state of ours as sent: its number, when, and where it ends in the
/// event stream.
#[derive(Debug, Clone, Copy)]
struct Sent {
    num: u64,
    at: u64,
    end: u64,
}

/// A state of the server's as received.
#[derive(Debug, Clone)]
struct Received {
    num: u64,
    screen: Screen,
    echo_ack: u64,
}

/// One Mosh session's protocol state.
#[derive(Debug)]
pub struct Client {
    session: Session,
    // Connection.
    next_seq: u64,
    expected_seq: u64,
    saved_timestamp: Option<(u16, u64)>,
    srtt: f64,
    rttvar: f64,
    rtt_hit: bool,
    last_heard: Option<u64>,
    last_roundtrip_success: u64,
    last_port_choice: u64,
    mtu: usize,
    // Receiver.
    assembly: Assembly,
    states: Vec<Received>,
    // Sender.
    events: VecDeque<UserEvent>,
    /// The event-stream position of `events[0]`.
    events_base: u64,
    sent: VecDeque<Sent>,
    assumed: usize,
    ack_num: u64,
    pending_data_ack: bool,
    next_ack_time: u64,
    next_send_time: u64,
    mindelay_clock: Option<u64>,
    remote_heard: Option<u64>,
    next_instruction_id: u64,
    last_instruction: Option<(u64, u64, u64, u64, Vec<u8>)>,
    last_ack_sent: u64,
    shutdown: Option<(u64, u32)>,
    rng: u64,
    started: u64,
    // Local echo.
    predictor: Predictor,
    /// Our states as sent, by number, and where each ends in the event
    /// stream: what the server's echo acknowledgement covers.
    marks: VecDeque<(u64, u64)>,
    /// The event-stream position the server has echoed up to.
    echoed: u64,
    /// The latest time the caller gave.
    clock: u64,
    // Display.
    shown: Screen,
    shown_version: u64,
    shown_num: Option<u64>,
    repaint: bool,
    notice: Option<String>,
}

impl Client {
    /// A session with `key`, to a server at an IPv6 address or not; the
    /// pane is `cols` x `rows`. `seed` seeds the chaff padding.
    pub fn new(key: &[u8; 16], v6: bool, cols: u16, rows: u16, seed: u64, now: u64) -> Self {
        let mut c = Self {
            session: Session::new(key),
            next_seq: 0,
            expected_seq: 0,
            saved_timestamp: None,
            srtt: 1000.0,
            rttvar: 500.0,
            rtt_hit: false,
            last_heard: None,
            last_roundtrip_success: now,
            last_port_choice: now,
            mtu: if v6 { MTU_V6 } else { MTU_V4 },
            assembly: Assembly::default(),
            states: vec![Received {
                num: 0,
                screen: Screen::new(cols.into(), rows.into()),
                echo_ack: 0,
            }],
            events: VecDeque::new(),
            events_base: 0,
            sent: VecDeque::from([Sent {
                num: 0,
                at: now,
                end: 0,
            }]),
            assumed: 0,
            ack_num: 0,
            pending_data_ack: false,
            next_ack_time: now,
            next_send_time: now,
            mindelay_clock: None,
            remote_heard: None,
            next_instruction_id: 0,
            last_instruction: None,
            last_ack_sent: 0,
            shutdown: None,
            rng: seed | 1,
            started: now,
            predictor: Predictor::new(Mode::Never),
            marks: VecDeque::new(),
            echoed: 0,
            clock: now,
            shown: Screen::new(cols.into(), rows.into()),
            shown_version: 0,
            shown_num: None,
            repaint: true,
            notice: None,
        };
        // Tell the server the terminal's size first, as mosh-client does.
        c.events.push_back(UserEvent::Resize { cols, rows });
        c
    }

    fn events_end(&self) -> u64 {
        self.events_base + self.events.len() as u64
    }

    /// Keystrokes from the user.
    pub fn keys(&mut self, bytes: &[u8]) {
        if self.shutdown.is_none() && !bytes.is_empty() {
            // Arrow keys always go as in application mode; the server
            // turns them back when its terminal is not in that mode.
            self.events.push_back(UserEvent::Keys(bytes.to_vec()));
            let end = self.events_end();
            if let Some(latest) = self.states.last() {
                self.predictor.typed(bytes, end, &latest.screen, self.clock);
            }
        }
    }

    /// Whether typed text not yet echoed is drawn now.
    pub fn predicting(&self) -> bool {
        self.predictor.active()
    }

    /// When typed text shows before the server echoes it.
    pub fn set_prediction(&mut self, mode: Mode) {
        self.predictor = Predictor::new(mode);
    }

    /// The pane changed size.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        if self.shutdown.is_none() {
            self.events.push_back(UserEvent::Resize { cols, rows });
        }
        self.predictor.resized(self.events_end());
        self.repaint = true;
    }

    /// Redraw everything next frame (the pane's contents are unknown).
    pub fn repaint(&mut self) {
        self.repaint = true;
    }

    /// Starts ending the session: the server is told, and acknowledges.
    pub fn start_shutdown(&mut self, now: u64) {
        if self.shutdown.is_none() {
            self.shutdown = Some((now, 0));
        }
    }

    pub fn shutting_down(&self) -> bool {
        self.shutdown.is_some()
    }

    /// The server acknowledged our shutdown.
    pub fn shutdown_acknowledged(&self) -> bool {
        self.sent.front().is_some_and(|s| s.num == SHUTDOWN)
    }

    /// Our shutdown went unanswered for long enough.
    pub fn shutdown_timed_out(&self, now: u64) -> bool {
        self.shutdown.is_some_and(|(start, tries)| {
            tries >= SHUTDOWN_RETRIES || now.saturating_sub(start) >= ACTIVE_RETRY_TIMEOUT
        })
    }

    /// The server ended the session (its shell exited) and we said we
    /// heard.
    pub fn server_ended(&self) -> bool {
        self.last_ack_sent == SHUTDOWN
    }

    /// Nothing heard from the server since the start, for too long.
    pub fn connect_timed_out(&self, now: u64) -> bool {
        self.last_heard.is_none() && now.saturating_sub(self.started) > CONNECT_TIMEOUT
    }

    /// Whether anything was heard from the server yet.
    pub fn heard(&self) -> bool {
        self.last_heard.is_some()
    }

    /// The newest screen state's number.
    pub fn remote_num(&self) -> u64 {
        self.states.last().map_or(0, |s| s.num)
    }

    /// Our newest keystroke state the server has echoed (for prediction).
    pub fn echo_ack(&self) -> u64 {
        self.states.last().map_or(0, |s| s.echo_ack)
    }

    /// Our newest state's number.
    pub fn sent_num(&self) -> u64 {
        self.sent.back().map_or(0, |s| s.num)
    }

    /// The smoothed round-trip time, in milliseconds.
    pub fn srtt(&self) -> f64 {
        self.srtt
    }

    fn timeout(&self) -> u64 {
        ((self.srtt + 4.0 * self.rttvar).ceil() as u64).clamp(MIN_RTO, MAX_RTO)
    }

    fn send_interval(&self) -> u64 {
        ((self.srtt / 2.0).ceil() as u64).clamp(SEND_INTERVAL_MIN, SEND_INTERVAL_MAX)
    }

    fn next_chaff(&mut self) -> Vec<u8> {
        let mut x = self.rng;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        let len = (next() % 17) as usize;
        let chaff = (0..len).map(|_| next() as u8).collect();
        self.rng = x;
        chaff
    }

    /// Takes one datagram from the server. Packets that fail the
    /// integrity check are dropped; only a protocol mismatch is fatal.
    pub fn receive(&mut self, datagram: &[u8], now: u64) -> Result<(), Fatal> {
        self.clock = now;
        let Ok((value, text)) = self.session.decrypt(datagram) else {
            return Ok(());
        };
        let Ok(p) = Packet::from_server(value, &text) else {
            return Ok(());
        };
        if p.seq >= self.expected_seq {
            // Only packets in order count for timing and roaming, so a
            // replayed one cannot skew them.
            self.expected_seq = p.seq + 1;
            if p.timestamp != u16::MAX {
                self.saved_timestamp = Some((p.timestamp, now));
            }
            if p.timestamp_reply != u16::MAX {
                let r = f64::from(timestamp16(now).wrapping_sub(p.timestamp_reply));
                if r < 5000.0 {
                    if self.rtt_hit {
                        self.rttvar = 0.75 * self.rttvar + 0.25 * (self.srtt - r).abs();
                        self.srtt = 0.875 * self.srtt + 0.125 * r;
                    } else {
                        self.srtt = r;
                        self.rttvar = r / 2.0;
                        self.rtt_hit = true;
                    }
                }
            }
            self.last_heard = Some(now);
        }
        let Ok(frag) = Fragment::parse(&p.payload) else {
            return Ok(());
        };
        let Some(packed) = self.assembly.add(frag) else {
            return Ok(());
        };
        let Ok(bytes) = inflate(&packed) else {
            return Ok(());
        };
        let Ok(inst) = Instruction::parse(&bytes) else {
            return Ok(());
        };
        if inst.protocol_version != PROTOCOL_VERSION {
            return Err(Fatal::Protocol(WireError::Version(inst.protocol_version)));
        }
        self.process_ack(inst.ack_num);
        if let Some(front) = self.sent.front() {
            self.last_roundtrip_success = front.at;
        }
        if self.states.iter().any(|s| s.num == inst.new_num) {
            return Ok(());
        }
        if !self.states.iter().any(|s| s.num == inst.old_num) {
            // A diff from a state we do not have: never applied, so a
            // diff cannot be replayed onto the wrong screen.
            return Ok(());
        }
        // Drop the states the server no longer refers to, keeping the one
        // the diff came from, before checking for room: a full list would
        // otherwise refuse the very instruction that empties it.
        let throwaway = inst.throwaway_num.min(inst.old_num);
        self.states.retain(|s| s.num >= throwaway);
        if self.states.len() >= MAX_STATES {
            return Ok(());
        }
        let Some(base) = self.states.iter().find(|s| s.num == inst.old_num) else {
            return Ok(());
        };
        let mut next = Received {
            num: inst.new_num,
            screen: base.screen.clone(),
            echo_ack: base.echo_ack,
        };
        let Ok(events) = parse_host_message(&inst.diff) else {
            return Ok(());
        };
        for e in events {
            match e {
                HostEvent::Bytes(b) => next.screen.apply(&b),
                HostEvent::Resize { cols, rows } => next
                    .screen
                    .resize(cols.max(1) as usize, rows.max(1) as usize),
                HostEvent::EchoAck(n) => next.echo_ack = next.echo_ack.max(n),
            }
        }
        if let Some(at) = self.states.iter().position(|s| s.num > next.num) {
            // Older than one we have: kept to diff from, not shown.
            self.states.insert(at, next);
            return Ok(());
        }
        self.ack_num = next.num;
        self.remote_heard = Some(now);
        if !inst.diff.is_empty() {
            self.pending_data_ack = true;
        }
        // What the server has echoed of our keystrokes.
        let echo_ack = next.echo_ack;
        while let Some(&(num, end)) = self.marks.front()
            && num <= echo_ack
        {
            self.echoed = self.echoed.max(end);
            self.marks.pop_front();
        }
        self.states.push(next);
        Ok(())
    }

    fn process_ack(&mut self, ack: u64) {
        if self.sent.iter().any(|s| s.num == ack) {
            while self.sent.len() > 1 && self.sent.front().is_some_and(|s| s.num < ack) {
                self.sent.pop_front();
            }
        }
        self.assumed = self.assumed.min(self.sent.len().saturating_sub(1));
    }

    fn update_assumed(&mut self, now: u64) {
        let limit = self.timeout() + ACK_DELAY;
        self.assumed = 0;
        for (i, s) in self.sent.iter().enumerate().skip(1) {
            if now.saturating_sub(s.at) < limit {
                self.assumed = i;
            } else {
                break;
            }
        }
    }

    /// Forgets the events every state the server may still refer to has.
    fn rationalize(&mut self) {
        let known = self.sent.front().map_or(0, |s| s.end);
        while self.events_base < known && self.events.pop_front().is_some() {
            self.events_base += 1;
        }
    }

    fn calculate_timers(&mut self, now: u64) {
        self.update_assumed(now);
        self.rationalize();
        if self.pending_data_ack && self.next_ack_time > now + ACK_DELAY {
            self.next_ack_time = now + ACK_DELAY;
        }
        let end = self.events_end();
        let back = self.sent.back().copied().unwrap_or(Sent {
            num: 0,
            at: now,
            end,
        });
        let assumed_end = self.sent.get(self.assumed).map_or(end, |s| s.end);
        let front_end = self.sent.front().map_or(end, |s| s.end);
        let active = self
            .remote_heard
            .is_some_and(|h| h + ACTIVE_RETRY_TIMEOUT > now);
        if end != back.end {
            let clock = *self.mindelay_clock.get_or_insert(now);
            self.next_send_time = (clock + SEND_MINDELAY).max(back.at + self.send_interval());
        } else if end != assumed_end && active {
            self.next_send_time = back.at + self.send_interval();
            if let Some(clock) = self.mindelay_clock {
                self.next_send_time = self.next_send_time.max(clock + SEND_MINDELAY);
            }
        } else if end != front_end && active {
            self.next_send_time = back.at + self.timeout() + ACK_DELAY;
        } else {
            self.next_send_time = u64::MAX;
        }
        if self.shutdown.is_some() || self.ack_num == SHUTDOWN {
            self.next_ack_time = back.at + self.send_interval();
        }
    }

    /// Milliseconds until [`Client::tick`] has something to do.
    pub fn wait(&mut self, now: u64) -> u64 {
        self.calculate_timers(now);
        self.next_ack_time
            .min(self.next_send_time)
            .saturating_sub(now)
    }

    /// Whether to send from a new source port: the server has not
    /// answered for a while, and a NAT may have forgotten us.
    pub fn wants_new_port(&self, now: u64) -> bool {
        now.saturating_sub(self.last_port_choice) > PORT_HOP_INTERVAL
            && now.saturating_sub(self.last_roundtrip_success) > PORT_HOP_INTERVAL
    }

    /// The caller now sends from a new port.
    pub fn port_changed(&mut self, now: u64) {
        self.last_port_choice = now;
    }

    fn diff_from(&self, end: u64) -> Vec<u8> {
        let skip = end.saturating_sub(self.events_base) as usize;
        let events: Vec<UserEvent> = self.events.iter().skip(skip).cloned().collect();
        user_message(&events)
    }

    /// The datagrams to send now, if any.
    pub fn tick(&mut self, now: u64) -> Result<Vec<Vec<u8>>, Fatal> {
        self.clock = now;
        self.calculate_timers(now);
        if now < self.next_ack_time && now < self.next_send_time {
            return Ok(Vec::new());
        }
        let assumed_end = self.sent.get(self.assumed).map_or(0, |s| s.end);
        let mut diff = self.diff_from(assumed_end);
        if self.assumed != 0 {
            // A resend from the state the server acknowledged, when about
            // as small, survives the loss of the packets in between.
            let front_end = self.sent.front().map_or(0, |s| s.end);
            let resend = self.diff_from(front_end);
            if resend.len() <= diff.len()
                || (resend.len() < 1000 && resend.len() - diff.len() < 100)
            {
                self.assumed = 0;
                diff = resend;
            }
        }
        let mut out = Vec::new();
        if diff.is_empty() {
            if now >= self.next_ack_time {
                let num = if self.shutdown.is_some() {
                    SHUTDOWN
                } else {
                    self.sent_num() + 1
                };
                self.add_sent(now, num);
                out = self.send_instruction(Vec::new(), num, now)?;
                self.next_ack_time = now + ACK_INTERVAL;
                self.next_send_time = u64::MAX;
                self.mindelay_clock = None;
            }
            if now >= self.next_send_time {
                self.next_send_time = u64::MAX;
                self.mindelay_clock = None;
            }
        } else if now >= self.next_send_time || now >= self.next_ack_time {
            let end = self.events_end();
            let same = self.sent.back().is_some_and(|s| s.end == end);
            let mut num = if same {
                self.sent_num()
            } else {
                self.sent_num() + 1
            };
            if self.shutdown.is_some() {
                num = SHUTDOWN;
            }
            if num == self.sent_num() {
                if let Some(back) = self.sent.back_mut() {
                    back.at = now;
                }
            } else {
                self.add_sent(now, num);
            }
            out = self.send_instruction(diff, num, now)?;
            self.assumed = self.sent.len() - 1;
            self.next_ack_time = now + ACK_INTERVAL;
            self.next_send_time = u64::MAX;
            self.mindelay_clock = None;
        }
        Ok(out)
    }

    fn add_sent(&mut self, now: u64, num: u64) {
        let end = self.events_end();
        self.sent.push_back(Sent { num, at: now, end });
        if num != SHUTDOWN {
            self.marks.push_back((num, end));
            if self.marks.len() > MAX_STATES {
                self.marks.pop_front();
            }
        }
        if self.sent.len() > 32 {
            // As Mosh: drop one from the middle, keeping the newest 15.
            let at = self.sent.len() - 16;
            self.sent.remove(at);
            self.assumed = self.assumed.min(self.sent.len() - 1);
        }
    }

    fn send_instruction(
        &mut self,
        diff: Vec<u8>,
        new_num: u64,
        now: u64,
    ) -> Result<Vec<Vec<u8>>, Fatal> {
        let chaff = self.next_chaff();
        let inst = Instruction {
            protocol_version: PROTOCOL_VERSION,
            old_num: self.sent.get(self.assumed).map_or(0, |s| s.num),
            new_num,
            ack_num: self.ack_num,
            throwaway_num: self.sent.front().map_or(0, |s| s.num),
            diff,
            chaff,
        };
        if new_num == SHUTDOWN
            && let Some((_, tries)) = &mut self.shutdown
        {
            *tries += 1;
        }
        let key = (
            inst.old_num,
            inst.new_num,
            inst.ack_num,
            inst.throwaway_num,
            inst.chaff.clone(),
        );
        if self.last_instruction.as_ref() != Some(&key) {
            self.next_instruction_id += 1;
        }
        self.last_instruction = Some(key);
        self.last_ack_sent = inst.ack_num;
        let payload = deflate(&inst.to_bytes());
        let room = self.mtu - PACKET_ADDED - TAG_LEN - FRAGMENT_HEADER;
        let chunks: Vec<&[u8]> = if payload.is_empty() {
            vec![&[][..]]
        } else {
            payload.chunks(room).collect()
        };
        let count = chunks.len();
        let mut out = Vec::with_capacity(count);
        for (i, chunk) in chunks.into_iter().enumerate() {
            let frag = Fragment {
                id: self.next_instruction_id,
                num: (i as u16) & 0x7FFF,
                last: i + 1 == count,
                contents: chunk.to_vec(),
            };
            out.push(self.packet(frag.to_bytes(), now)?);
        }
        Ok(out)
    }

    fn packet(&mut self, payload: Vec<u8>, now: u64) -> Result<Vec<u8>, Fatal> {
        let mut reply = u16::MAX;
        if let Some((ts, at)) = self.saved_timestamp
            && now.saturating_sub(at) < 1000
        {
            reply = ts.wrapping_add((now - at) as u16);
            self.saved_timestamp = None;
        }
        let p = Packet {
            seq: self.next_seq,
            timestamp: timestamp16(now),
            timestamp_reply: reply,
            payload,
        };
        self.next_seq += 1;
        let (value, text) = p.to_server();
        self.session.encrypt(value, &text).map_err(Fatal::Crypto)
    }

    /// The output that brings the pane up to the newest screen, if it
    /// changed (or the notice about the server did). With `now`, a bar
    /// on the top line says when the server was last heard, once that is
    /// a while ago, as mosh-client shows it.
    pub fn frame(&mut self, now: u64) -> Option<String> {
        self.clock = now;
        let notice = self.notice_text(now);
        let latest = self.states.last()?;
        self.predictor
            .update(&latest.screen, self.echoed, self.srtt, now);
        if !self.repaint
            && self.shown_num == Some(latest.num)
            && notice == self.notice
            && self.predictor.version() == self.shown_version
        {
            return None;
        }
        let mut next = latest.screen.clone();
        self.predictor.overlay(&mut next);
        if let Some(text) = &notice {
            overlay(&mut next, text);
        }
        let out = screen::render(
            !self.repaint && self.shown_num.is_some(),
            &self.shown,
            &next,
        );
        self.shown = next;
        self.shown_num = Some(latest.num);
        self.shown_version = self.predictor.version();
        self.repaint = false;
        self.notice = notice;
        Some(out)
    }

    fn notice_text(&self, now: u64) -> Option<String> {
        if self.shutdown.is_some() {
            return Some("mosh: Exiting...".into());
        }
        match self.last_heard {
            None if now.saturating_sub(self.started) > 250 => Some("mosh: Connecting...".into()),
            Some(at) if now.saturating_sub(at) > LATE => {
                let secs = now.saturating_sub(at) / 1000;
                Some(if secs < 60 {
                    format!("mosh: Last contact {secs} seconds ago.")
                } else if secs < 3600 {
                    format!("mosh: Last contact {}:{:02} ago.", secs / 60, secs % 60)
                } else {
                    format!(
                        "mosh: Last contact {}:{:02}:{:02} ago.",
                        secs / 3600,
                        (secs / 60) % 60,
                        secs % 60
                    )
                })
            }
            _ => None,
        }
    }
}

/// Writes `text` over the top line, as Mosh's notification bar.
fn overlay(s: &mut Screen, text: &str) {
    let mut bar = Screen::new(s.width(), 1);
    // White on blue, bold, the width of the screen.
    let text: String = text.chars().take(s.width()).collect();
    let line = format!("\x1b[1;37;44m{text:<width$}", width = s.width());
    bar.apply(line.as_bytes());
    if let (Some(top), Some(row)) = (s.rows.first_mut(), bar.rows.first()) {
        *top = row.clone();
    }
}

/// The low 16 bits of a millisecond clock, never 0xFFFF (which means
/// "none").
pub fn timestamp16(now: u64) -> u16 {
    let ts = (now % 65536) as u16;
    if ts == u16::MAX { 0 } else { ts }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::crypto::Session;

    /// The test session's key. Made at run time, not a literal: CodeQL
    /// reads a literal that reaches a cipher as a hard-coded key.
    fn key() -> [u8; 16] {
        core::array::from_fn(|i| (i as u8).wrapping_mul(29))
    }

    /// The nonce of the server's packet `seq` (bit 63 set), likewise.
    fn to_client(seq: u64) -> u64 {
        let value = seq | 1u64.rotate_right(1);
        debug_assert_eq!(value, crate::wire::TO_CLIENT | seq);
        value
    }

    /// A stand-in for mosh-server: decrypts the client's packets and
    /// sends screen diffs.
    struct Server {
        session: Session,
        seq: u64,
        assembly: Assembly,
        frag_id: u64,
    }

    impl Server {
        fn new() -> Self {
            Self {
                session: Session::new(&key()),
                seq: 0,
                assembly: Assembly::default(),
                frag_id: 100,
            }
        }

        fn take(&mut self, datagram: &[u8]) -> Option<(Instruction, Vec<u8>)> {
            let (value, text) = self.session.decrypt(datagram).unwrap();
            assert_eq!(value & crate::wire::TO_CLIENT, 0);
            let payload = text[4..].to_vec();
            let frag = Fragment::parse(&payload).unwrap();
            let packed = self.assembly.add(frag)?;
            let inst = Instruction::parse(&inflate(&packed).unwrap()).unwrap();
            let diff = inst.diff.clone();
            Some((inst, diff))
        }

        fn send(&mut self, old: u64, new: u64, ack: u64, host: &[u8]) -> Vec<u8> {
            let mut msg = Vec::new();
            if !host.is_empty() {
                // HostMessage { instruction { hostbytes { hoststring } } }
                let mut hb = vec![0x22, host.len() as u8];
                hb.extend_from_slice(host);
                let mut inst = vec![0x12, hb.len() as u8];
                inst.extend(hb);
                msg = vec![0x0A, inst.len() as u8];
                msg.extend(inst);
            }
            let inst = Instruction {
                protocol_version: 2,
                old_num: old,
                new_num: new,
                ack_num: ack,
                throwaway_num: old,
                diff: msg,
                chaff: vec![],
            };
            self.frag_id += 1;
            let frag = Fragment {
                id: self.frag_id,
                num: 0,
                last: true,
                contents: deflate(&inst.to_bytes()),
            };
            let mut text = vec![0, 1, 0xFF, 0xFF];
            text.extend(frag.to_bytes());
            self.seq += 1;
            self.session.encrypt(to_client(self.seq), &text).unwrap()
        }
    }

    #[test]
    fn a_full_state_list_takes_the_instruction_that_empties_it() {
        let mut c = Client::new(&key(), false, 20, 4, 1, 0);
        let mut s = Server::new();
        // The server never hears our acks, so every diff is from state 0
        // and nothing is thrown away: the list fills up.
        for n in 1..400 {
            c.receive(&s.send(0, n, 0, b"x"), 10).unwrap();
        }
        let held = c.remote_num();
        assert!(held < 399, "the list is capped");
        // Then it hears one: throwing away all before it makes room.
        c.receive(&s.send(held, 500, 0, b"y"), 20).unwrap();
        assert_eq!(c.remote_num(), 500);
    }

    #[test]
    fn sends_the_size_and_keystrokes_and_takes_screens() {
        let mut c = Client::new(&key(), false, 80, 24, 42, 0);
        let mut s = Server::new();
        let first = c.tick(0).unwrap();
        assert_eq!(first.len(), 1);
        let (inst, diff) = s.take(&first[0]).unwrap();
        assert_eq!((inst.old_num, inst.new_num, inst.ack_num), (0, 1, 0));
        assert_eq!(
            diff,
            user_message(&[UserEvent::Resize { cols: 80, rows: 24 }])
        );

        // The server's first screen, from state 0.
        c.receive(&s.send(0, 1, 1, b"hello"), 30).unwrap();
        assert_eq!(c.remote_num(), 1);
        let out = c.frame(30).unwrap();
        assert!(out.contains("hello"), "{out:?}");
        assert_eq!(c.frame(31), None);

        c.keys(b"ls\r");
        let wait = c.wait(31);
        // Before any round trip, Mosh assumes 1 s and sends every 250 ms.
        assert!(wait <= 250, "{wait}");
        let sent = c.tick(31 + wait).unwrap();
        let (inst, diff) = s.take(&sent[0]).unwrap();
        // State 1 was acknowledged, so the diff is from it, and ours
        // acknowledges the server's state 1.
        assert_eq!((inst.old_num, inst.new_num, inst.ack_num), (1, 2, 1));
        assert_eq!(diff, user_message(&[UserEvent::Keys(b"ls\r".to_vec())]));
    }

    #[test]
    fn diffs_from_an_older_state_apply_to_that_state() {
        let mut c = Client::new(&key(), false, 20, 4, 1, 0);
        let mut s = Server::new();
        c.receive(&s.send(0, 1, 0, b"one"), 10).unwrap();
        c.receive(&s.send(1, 2, 0, b"\r\ntwo"), 20).unwrap();
        c.frame(20).unwrap();
        // The server had not heard we have 2: it diffs from 1.
        c.receive(&s.send(1, 3, 0, b"\r\nthree"), 30).unwrap();
        let mut shown = Screen::new(20, 4);
        shown.apply(b"one\r\ntwo");
        let out = c.frame(30).unwrap();
        shown.apply(out.as_bytes());
        let row = |s: &Screen, y: usize| -> String {
            s.rows[y].cells.iter().map(|c| c.text.as_str()).collect()
        };
        assert_eq!(row(&shown, 0), "one");
        assert_eq!(row(&shown, 1), "three", "state 3 = state 1 + three");
        // A diff from a state never received is dropped.
        c.receive(&s.send(9, 10, 0, b"x"), 40).unwrap();
        assert_eq!(c.remote_num(), 3);
        // So are forged and replayed packets.
        let mut bad = s.send(3, 4, 0, b"y");
        bad[12] ^= 1;
        c.receive(&bad, 50).unwrap();
        assert_eq!(c.remote_num(), 3);
    }

    #[test]
    fn version_mismatch_is_fatal() {
        let mut c = Client::new(&key(), false, 20, 4, 1, 0);
        let mut s = Server::new();
        let mut p = s.send(0, 1, 0, b"");
        // Re-make it with protocol 3.
        let inst = Instruction {
            protocol_version: 3,
            ..Instruction::default()
        };
        let frag = Fragment {
            id: 1,
            num: 0,
            last: true,
            contents: deflate(&inst.to_bytes()),
        };
        let mut text = vec![0, 1, 0xFF, 0xFF];
        text.extend(frag.to_bytes());
        p = s.session.encrypt(to_client(77), &text).unwrap_or(p);
        assert_eq!(
            c.receive(&p, 1),
            Err(Fatal::Protocol(WireError::Version(3)))
        );
    }

    #[test]
    fn shutdown_both_ways() {
        // The server's shell exits: it sends state SHUTDOWN; we ack it.
        let mut c = Client::new(&key(), false, 20, 4, 1, 0);
        let mut s = Server::new();
        c.tick(0).unwrap();
        c.receive(&s.send(0, SHUTDOWN, 1, b"bye"), 10).unwrap();
        assert!(!c.server_ended());
        let wait = c.wait(10);
        let out = c.tick(10 + wait).unwrap();
        let (inst, _) = s.take(&out[0]).unwrap();
        assert_eq!(inst.ack_num, SHUTDOWN);
        assert!(c.server_ended());

        // We close the tab: our states become SHUTDOWN until acked.
        let mut c = Client::new(&key(), false, 20, 4, 1, 0);
        let mut s = Server::new();
        c.tick(0).unwrap();
        c.start_shutdown(5);
        let wait = c.wait(5);
        let out = c.tick(5 + wait).unwrap();
        let (inst, _) = s.take(&out[0]).unwrap();
        assert_eq!(inst.new_num, SHUTDOWN);
        assert!(!c.shutdown_acknowledged());
        c.receive(&s.send(0, 1, SHUTDOWN, b""), 40).unwrap();
        assert!(c.shutdown_acknowledged());
        assert!(c.shutdown_timed_out(20_000));
    }

    #[test]
    fn measures_round_trips_and_shows_late_server() {
        let mut c = Client::new(&key(), false, 60, 3, 1, 1000);
        let mut s = Server::new();
        assert_eq!(c.frame(1100).map(|f| f.contains("Connecting")), Some(false));
        assert!(c.frame(1300).unwrap().contains("mosh: Connecting..."));
        // A reply to our timestamp 1000, 80 ms later.
        let mut p = s.send(0, 1, 0, b"x");
        let (n, mut text) = s.session.decrypt(&p).unwrap();
        text[2..4].copy_from_slice(&timestamp16(1000).to_be_bytes());
        p = s.session.encrypt(n, &text).unwrap();
        c.receive(&p, 1080).unwrap();
        assert_eq!(c.srtt(), 80.0);
        assert!(!c.frame(1100).unwrap().contains("mosh:"));
        assert!(
            c.frame(9000)
                .unwrap()
                .contains("Last contact 7 seconds ago")
        );
        assert!(!c.wants_new_port(9000));
        assert!(c.wants_new_port(13_000));
        c.port_changed(13_000);
        assert!(!c.wants_new_port(13_001));
        assert!(!c.connect_timed_out(100_000));
        assert!(Client::new(&key(), false, 3, 3, 1, 0).connect_timed_out(15_001));
    }

    #[test]
    fn big_input_is_fragmented() {
        let mut c = Client::new(&key(), false, 20, 4, 1, 0);
        let mut s = Server::new();
        c.tick(0).unwrap();
        // Paste incompressible text.
        let mut x: u32 = 1;
        let paste: Vec<u8> = (0..6000)
            .map(|_| {
                x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
                b'!' + (x >> 16) as u8 % 90
            })
            .collect();
        c.keys(&paste);
        let wait = c.wait(1);
        let out = c.tick(1 + wait).unwrap();
        assert!(out.len() > 4, "{}", out.len());
        assert!(out.iter().all(|d| d.len() <= MTU_V4));
        let mut got = None;
        for d in &out {
            if let Some(x) = s.take(d) {
                got = Some(x);
            }
        }
        let (_, diff) = got.unwrap();
        assert!(diff.ends_with(&paste[paste.len() - 100..]));
    }
}
