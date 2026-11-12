//! Session recordings: a pane's output with its timing, as an asciicast
//! v2 file (`.cast`), and playing one back.
//!
//! The format is asciinema's: a JSON header line, then one JSON array per
//! line, `[seconds, code, data]`. A recording holds only what the host
//! sent (`"o"`) and size changes (`"r"`), never what was typed, so
//! passwords typed at a prompt (which hosts do not echo) cannot end up in
//! it. A recording opened with a chain file is sealed against later edits
//! like a log (see `chain`).
//!
//! [`Cast::parse`] reads files from anywhere, so it trusts nothing: the
//! JSON is read by a small parser here with depth and size limits, lines
//! it cannot read are skipped and counted, and sizes are clamped. The
//! `cast_parse` fuzz target runs it.

use std::fmt::Write as _;
use std::io::{self, Write};
use std::time::{Duration, Instant};

use crate::chain::Chained;
use crate::log::Sealed;

/// The largest recording [`Cast::parse`] reads.
pub const MAX_FILE: usize = 64 * 1024 * 1024;
/// The most events a recording keeps; the rest are dropped.
pub const MAX_EVENTS: usize = 4_000_000;
/// The largest terminal a recording can ask for.
pub const MAX_COLS: u16 = 1000;
pub const MAX_ROWS: u16 = 500;
/// How deep JSON may nest (the header's `env` and `theme` are objects).
const MAX_DEPTH: usize = 32;
/// The size used when the header gives none it can use.
const DEFAULT_SIZE: (u16, u16) = (80, 24);

/// Where a recording's bytes go: the file alone, or the file and its chain.
enum Sink<W: Write> {
    Plain(W),
    Chained(Box<Chained<W>>),
}

impl<W: Write> Write for Sink<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Plain(w) => w.write(buf),
            Self::Chained(c) => c.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Plain(w) => w.flush(),
            Self::Chained(c) => c.flush(),
        }
    }
}

/// What goes in a recording's header line.
#[derive(Debug, Clone, Default)]
pub struct CastHeader {
    pub cols: u16,
    pub rows: u16,
    /// Seconds since 1970 when the recording started.
    pub timestamp: u64,
    pub title: String,
}

/// Writes one pane's recording.
pub struct CastWriter<W: Write> {
    out: Sink<W>,
    started: Instant,
    /// The end of the last chunk when it stopped inside a UTF-8 character:
    /// the rest comes with the next chunk.
    carry: Vec<u8>,
    /// The time of the last event, so times never go back.
    last: f64,
}

impl<W: Write> std::fmt::Debug for CastWriter<W> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CastWriter").finish_non_exhaustive()
    }
}

impl<W: Write> CastWriter<W> {
    pub fn new(out: W, header: &CastHeader) -> io::Result<Self> {
        Self::open(Sink::Plain(out), header)
    }

    /// A recording sealed in `chain` as it is written; `file_name` is the
    /// recording's own file name, which the chain starts from.
    pub fn sealed(out: W, chain: W, file_name: &str, header: &CastHeader) -> io::Result<Self> {
        let chained = Chained::new(out, chain, file_name)?;
        Self::open(Sink::Chained(Box::new(chained)), header)
    }

    fn open(mut out: Sink<W>, h: &CastHeader) -> io::Result<Self> {
        let mut line = format!(
            "{{\"version\": 2, \"width\": {}, \"height\": {}, \"timestamp\": {}",
            h.cols.clamp(1, MAX_COLS),
            h.rows.clamp(1, MAX_ROWS),
            h.timestamp
        );
        if !h.title.is_empty() {
            line.push_str(", \"title\": ");
            json_string(&mut line, &h.title);
        }
        line.push_str(", \"env\": {\"TERM\": \"xterm-256color\"}}\n");
        out.write_all(line.as_bytes())?;
        out.flush()?;
        Ok(Self {
            out,
            started: Instant::now(),
            carry: Vec::new(),
            last: 0.0,
        })
    }

    fn seconds(&mut self, at: Duration) -> f64 {
        self.last = at.as_secs_f64().max(self.last);
        self.last
    }

    fn event(&mut self, at: Duration, code: &str, data: &str) -> io::Result<()> {
        let t = self.seconds(at);
        let mut line = format!("[{t:.6}, \"{code}\", ");
        json_string(&mut line, data);
        line.push_str("]\n");
        self.out.write_all(line.as_bytes())?;
        self.out.flush()?;
        if let Sink::Chained(c) = &mut self.out {
            c.maybe_seal()?;
        }
        Ok(())
    }

    /// Records bytes the host sent, now.
    pub fn output(&mut self, bytes: &[u8]) -> io::Result<()> {
        let at = self.started.elapsed();
        self.output_at(at, bytes)
    }

    /// Records bytes the host sent, `at` after the start.
    pub fn output_at(&mut self, at: Duration, bytes: &[u8]) -> io::Result<()> {
        let mut data = std::mem::take(&mut self.carry);
        data.extend_from_slice(bytes);
        let text = split_utf8(&mut data);
        self.carry = data;
        if text.is_empty() {
            return Ok(());
        }
        self.event(at, "o", &text)
    }

    /// Records that the terminal changed size, now.
    pub fn resize(&mut self, cols: u16, rows: u16) -> io::Result<()> {
        let at = self.started.elapsed();
        self.resize_at(at, cols, rows)
    }

    pub fn resize_at(&mut self, at: Duration, cols: u16, rows: u16) -> io::Result<()> {
        self.event(at, "r", &format!("{cols}x{rows}"))
    }

    /// Writes what was held back and seals the end; `None` for a
    /// recording without a chain.
    pub fn finish_sealed(mut self) -> io::Result<Option<Sealed>> {
        if !self.carry.is_empty() {
            let rest = String::from_utf8_lossy(&std::mem::take(&mut self.carry)).into_owned();
            let at = self.started.elapsed();
            self.event(at, "o", &rest)?;
        }
        self.out.flush()?;
        match self.out {
            Sink::Plain(_) => Ok(None),
            Sink::Chained(c) => {
                let (_, _, bytes, head) = c.end()?;
                Ok(Some(Sealed { bytes, head }))
            }
        }
    }

    /// Like `finish_sealed`, and gives back the file (for tests).
    pub fn finish(mut self) -> io::Result<W> {
        if !self.carry.is_empty() {
            let rest = String::from_utf8_lossy(&std::mem::take(&mut self.carry)).into_owned();
            let at = self.started.elapsed();
            self.event(at, "o", &rest)?;
        }
        match self.out {
            Sink::Plain(w) => Ok(w),
            Sink::Chained(c) => c.end().map(|(w, ..)| w),
        }
    }
}

/// Takes the text off the front of `data`, leaving an unfinished UTF-8
/// character at its end (at most 3 bytes) for the next chunk. Bytes that
/// are not UTF-8 become U+FFFD.
fn split_utf8(data: &mut Vec<u8>) -> String {
    let mut out = String::new();
    let mut rest: &[u8] = data;
    loop {
        match std::str::from_utf8(rest) {
            Ok(s) => {
                out.push_str(s);
                rest = &[];
                break;
            }
            Err(e) => {
                let (good, bad) = rest.split_at(e.valid_up_to());
                out.push_str(std::str::from_utf8(good).unwrap_or_default());
                match e.error_len() {
                    Some(n) => {
                        out.push('\u{FFFD}');
                        rest = bad.get(n..).unwrap_or_default();
                    }
                    None => {
                        rest = bad;
                        break;
                    }
                }
            }
        }
    }
    let keep = rest.to_vec();
    *data = keep;
    out
}

/// Appends `s` as a JSON string.
fn json_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// One thing that happens in a recording.
#[derive(Debug, Clone, PartialEq)]
pub enum CastEvent {
    /// What the host sent.
    Output(String),
    /// The terminal's new size.
    Resize(u16, u16),
}

/// A recording read from a file.
#[derive(Debug, Clone, PartialEq)]
pub struct Cast {
    pub cols: u16,
    pub rows: u16,
    pub title: Option<String>,
    /// Each event and the time it plays at, in seconds from the start,
    /// never going back. Long pauses are already shortened to the
    /// header's `idle_time_limit`.
    pub events: Vec<(f64, CastEvent)>,
    /// Lines that could not be read and were left out.
    pub skipped: usize,
}

/// Why a file is not a recording that can be played.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CastError {
    TooBig,
    /// The first line is not an asciicast v2 header.
    NotCast,
    /// An asciicast of another version.
    Version(i64),
}

impl std::fmt::Display for CastError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooBig => write!(f, "the file is larger than {} MB", MAX_FILE / 1024 / 1024),
            Self::NotCast => write!(f, "it is not an asciicast recording (.cast)"),
            Self::Version(v) => write!(
                f,
                "it is an asciicast version {v} recording; FeroSSH plays version 2"
            ),
        }
    }
}

impl std::error::Error for CastError {}

impl Cast {
    /// Reads a recording. Event lines that cannot be read (a line cut off
    /// when the recording stopped, say) are skipped and counted.
    pub fn parse(bytes: &[u8]) -> Result<Self, CastError> {
        if bytes.len() > MAX_FILE {
            return Err(CastError::TooBig);
        }
        let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
        let mut lines = bytes
            .split(|&b| b == b'\n')
            .map(|l| l.strip_suffix(b"\r").unwrap_or(l))
            .filter(|l| !l.iter().all(u8::is_ascii_whitespace));
        let header = lines.next().ok_or(CastError::NotCast)?;
        let Some(Json::Object(header)) = Json::parse(header) else {
            return Err(CastError::NotCast);
        };
        match field(&header, "version") {
            Some(Json::Number(v)) if *v == 2.0 => {}
            Some(Json::Number(v)) if v.is_finite() && v.fract() == 0.0 => {
                return Err(CastError::Version(*v as i64));
            }
            _ => return Err(CastError::NotCast),
        }
        let size = |key: &str, max: u16, default: u16| match field(&header, key) {
            Some(Json::Number(n)) if n.is_finite() && *n >= 1.0 => (*n as u16).clamp(1, max),
            _ => default,
        };
        let cols = size("width", MAX_COLS, DEFAULT_SIZE.0);
        let rows = size("height", MAX_ROWS, DEFAULT_SIZE.1);
        let title = match field(&header, "title") {
            Some(Json::String(t)) if !t.is_empty() => Some(t.clone()),
            _ => None,
        };
        let idle = match field(&header, "idle_time_limit") {
            Some(Json::Number(n)) if n.is_finite() && *n > 0.0 => Some(*n),
            _ => None,
        };

        let mut events = Vec::new();
        let mut skipped = 0;
        // The time in the file of the last event, and when it plays.
        let (mut last_file, mut last_play) = (0.0f64, 0.0f64);
        for line in lines {
            if events.len() >= MAX_EVENTS {
                skipped += 1;
                continue;
            }
            let Some((t, code, data)) = event_line(line) else {
                skipped += 1;
                continue;
            };
            let event = match code.as_str() {
                "o" => CastEvent::Output(data),
                "r" => match parse_size(&data) {
                    Some((c, r)) => CastEvent::Resize(c, r),
                    None => {
                        skipped += 1;
                        continue;
                    }
                },
                // Input ("i"), markers ("m") and codes from later versions
                // play no part.
                _ => continue,
            };
            let t = t.max(last_file);
            let mut gap = t - last_file;
            if let Some(limit) = idle {
                gap = gap.min(limit);
            }
            last_file = t;
            last_play += gap;
            events.push((last_play, event));
        }
        Ok(Self {
            cols,
            rows,
            title,
            events,
            skipped,
        })
    }

    /// How long the recording plays, in seconds.
    pub fn duration(&self) -> f64 {
        self.events.last().map_or(0.0, |(t, _)| *t)
    }
}

/// `COLSxROWS`, clamped to what a recording may ask for.
fn parse_size(text: &str) -> Option<(u16, u16)> {
    let (c, r) = text.split_once('x')?;
    let c: u32 = c.trim().parse().ok()?;
    let r: u32 = r.trim().parse().ok()?;
    if c == 0 || r == 0 {
        return None;
    }
    let c = c.min(u32::from(MAX_COLS)) as u16;
    let r = r.min(u32::from(MAX_ROWS)) as u16;
    Some((c, r))
}

/// `[time, "code", "data"]`, with a time that is a number of seconds.
fn event_line(line: &[u8]) -> Option<(f64, String, String)> {
    let Json::Array(items) = Json::parse(line)? else {
        return None;
    };
    let mut items = items.into_iter();
    let Some(Json::Number(t)) = items.next() else {
        return None;
    };
    let Some(Json::String(code)) = items.next() else {
        return None;
    };
    let Some(Json::String(data)) = items.next() else {
        return None;
    };
    if !t.is_finite() || t < 0.0 {
        return None;
    }
    // About eleven days: enough for any recording, and no overflow when
    // turned into a `Duration`.
    Some((t.min(1e6), code, data))
}

fn field<'a>(object: &'a [(String, Json)], key: &str) -> Option<&'a Json> {
    object.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

/// A JSON value, read by [`Json::parse`].
#[derive(Debug, Clone, PartialEq)]
enum Json {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl Json {
    /// The value that is the whole of `text`, or `None`.
    fn parse(text: &[u8]) -> Option<Self> {
        let mut p = JsonParser { text, at: 0 };
        let v = p.value(0)?;
        p.space();
        (p.at == text.len()).then_some(v)
    }
}

struct JsonParser<'a> {
    text: &'a [u8],
    at: usize,
}

impl JsonParser<'_> {
    fn peek(&self) -> Option<u8> {
        self.text.get(self.at).copied()
    }

    fn space(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\r' | b'\n')) {
            self.at += 1;
        }
    }

    fn eat(&mut self, b: u8) -> bool {
        self.space();
        if self.peek() == Some(b) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn word(&mut self, w: &[u8], v: Json) -> Option<Json> {
        let end = self.at.checked_add(w.len())?;
        (self.text.get(self.at..end)? == w).then(|| {
            self.at = end;
            v
        })
    }

    fn value(&mut self, depth: usize) -> Option<Json> {
        if depth > MAX_DEPTH {
            return None;
        }
        self.space();
        match self.peek()? {
            b'{' => {
                self.at += 1;
                let mut fields = Vec::new();
                if self.eat(b'}') {
                    return Some(Json::Object(fields));
                }
                loop {
                    self.space();
                    let key = self.string()?;
                    if !self.eat(b':') {
                        return None;
                    }
                    let v = self.value(depth + 1)?;
                    fields.push((key, v));
                    if self.eat(b'}') {
                        return Some(Json::Object(fields));
                    }
                    if !self.eat(b',') {
                        return None;
                    }
                }
            }
            b'[' => {
                self.at += 1;
                let mut items = Vec::new();
                if self.eat(b']') {
                    return Some(Json::Array(items));
                }
                loop {
                    items.push(self.value(depth + 1)?);
                    if self.eat(b']') {
                        return Some(Json::Array(items));
                    }
                    if !self.eat(b',') {
                        return None;
                    }
                }
            }
            b'"' => self.string().map(Json::String),
            b't' => self.word(b"true", Json::Bool(true)),
            b'f' => self.word(b"false", Json::Bool(false)),
            b'n' => self.word(b"null", Json::Null),
            b'-' | b'0'..=b'9' => self.number(),
            _ => None,
        }
    }

    fn number(&mut self) -> Option<Json> {
        let start = self.at;
        while matches!(
            self.peek(),
            Some(b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')
        ) {
            self.at += 1;
        }
        let text = std::str::from_utf8(self.text.get(start..self.at)?).ok()?;
        // Rust's float syntax is wider than JSON's (`inf`, `+1`) only in
        // ways the characters above already rule out, except a leading
        // `+`, which is refused here.
        if text.starts_with('+') {
            return None;
        }
        text.parse().ok().map(Json::Number)
    }

    fn hex4(&mut self) -> Option<u32> {
        let end = self.at.checked_add(4)?;
        let digits = std::str::from_utf8(self.text.get(self.at..end)?).ok()?;
        let v = u32::from_str_radix(digits, 16).ok()?;
        if !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        self.at = end;
        Some(v)
    }

    fn string(&mut self) -> Option<String> {
        if self.peek()? != b'"' {
            return None;
        }
        self.at += 1;
        let mut out = String::new();
        loop {
            // The plain run up to the next quote or backslash.
            let start = self.at;
            while let Some(b) = self.peek() {
                if b == b'"' || b == b'\\' {
                    break;
                }
                self.at += 1;
            }
            out.push_str(&String::from_utf8_lossy(self.text.get(start..self.at)?));
            match self.peek()? {
                b'"' => {
                    self.at += 1;
                    return Some(out);
                }
                _ => {
                    self.at += 1;
                    let c = self.peek()?;
                    self.at += 1;
                    match c {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let mut u = self.hex4()?;
                            if (0xD800..0xDC00).contains(&u) {
                                // A high surrogate wants its low half next.
                                let back = self.at;
                                let low = (self.text.get(self.at..self.at + 2) == Some(b"\\u"))
                                    .then(|| {
                                        self.at += 2;
                                        self.hex4()
                                    })
                                    .flatten()
                                    .filter(|l| (0xDC00..0xE000).contains(l));
                                match low {
                                    Some(l) => u = 0x10000 + ((u - 0xD800) << 10) + (l - 0xDC00),
                                    None => self.at = back,
                                }
                            }
                            out.push(char::from_u32(u).unwrap_or('\u{FFFD}'));
                        }
                        _ => return None,
                    }
                }
            }
        }
    }
}

/// What playing a recording has to do now.
#[derive(Debug, Clone, PartialEq)]
pub enum Play<'a> {
    /// Start again from an empty terminal of this size (before going
    /// back).
    Reset(u16, u16),
    Output(&'a str),
    Resize(u16, u16),
}

/// The slowest and fastest playing speeds.
pub const SPEEDS: [f64; 9] = [0.25, 0.5, 1.0, 1.5, 2.0, 4.0, 8.0, 16.0, 32.0];
const NORMAL_SPEED: usize = 2;

/// Plays a recording in time: says which events are due and when the next
/// one is.
#[derive(Debug)]
pub struct Player {
    cast: Cast,
    /// The next event to play.
    next: usize,
    /// Where in the recording playing stood when it last started, paused
    /// or changed speed, and when that was (`None` while paused).
    pos: f64,
    since: Option<Instant>,
    speed: usize,
    /// Go back to the start before playing on.
    reset: bool,
}

impl Player {
    /// A player at the start, playing from `now`.
    pub fn new(cast: Cast, now: Instant) -> Self {
        Self {
            cast,
            next: 0,
            pos: 0.0,
            since: Some(now),
            speed: NORMAL_SPEED,
            reset: false,
        }
    }

    pub fn cast(&self) -> &Cast {
        &self.cast
    }

    /// Where playing stands, in seconds of the recording.
    pub fn position(&self, now: Instant) -> f64 {
        let played = self.since.map_or(0.0, |s| {
            now.saturating_duration_since(s).as_secs_f64() * self.speed()
        });
        (self.pos + played).min(self.cast.duration())
    }

    pub fn speed(&self) -> f64 {
        SPEEDS.get(self.speed).copied().unwrap_or(1.0)
    }

    pub fn paused(&self) -> bool {
        self.since.is_none()
    }

    /// Every event played.
    pub fn ended(&self) -> bool {
        self.next >= self.cast.events.len()
    }

    fn hold(&mut self, now: Instant) {
        self.pos = self.position(now);
        if self.since.is_some() {
            self.since = Some(now);
        }
    }

    /// Pauses, or plays on (from the start again once it ended).
    pub fn toggle_pause(&mut self, now: Instant) {
        if self.paused() {
            if self.ended() {
                self.seek(0.0, now);
            }
            self.since = Some(now);
        } else {
            self.pos = self.position(now);
            self.since = None;
        }
    }

    /// One step faster (`up`) or slower.
    pub fn change_speed(&mut self, up: bool, now: Instant) {
        self.hold(now);
        self.speed = if up {
            (self.speed + 1).min(SPEEDS.len() - 1)
        } else {
            self.speed.saturating_sub(1)
        };
    }

    /// Goes to `to` seconds in. Going back starts again from the start.
    pub fn seek(&mut self, to: f64, now: Instant) {
        let to = to.clamp(0.0, self.cast.duration());
        if to < self.position(now) || to == 0.0 {
            self.reset = true;
            self.next = 0;
        }
        self.pos = to;
        if self.since.is_some() {
            self.since = Some(now);
        }
    }

    /// What is due by `now`, in order.
    pub fn due(&mut self, now: Instant) -> Vec<Play<'_>> {
        let pos = self.position(now);
        let mut out = Vec::new();
        if std::mem::take(&mut self.reset) {
            out.push(Play::Reset(self.cast.cols, self.cast.rows));
        }
        let start = self.next;
        let end = start
            + self
                .cast
                .events
                .get(start..)
                .unwrap_or_default()
                .iter()
                .take_while(|(t, _)| *t <= pos)
                .count();
        self.next = end;
        if self.ended() && !self.paused() {
            // Stop at the end, so Space starts again.
            self.pos = self.cast.duration();
            self.since = None;
        }
        for (_, e) in self.cast.events.get(start..end).unwrap_or_default() {
            out.push(match e {
                CastEvent::Output(s) => Play::Output(s),
                CastEvent::Resize(c, r) => Play::Resize(*c, *r),
            });
        }
        out
    }

    /// How long until the next event is due, while playing.
    pub fn wait(&self, now: Instant) -> Option<Duration> {
        if self.paused() {
            return None;
        }
        let (t, _) = self.cast.events.get(self.next)?;
        let ahead = (t - self.position(now)).max(0.0) / self.speed();
        Some(Duration::from_secs_f64(ahead.min(1e6)))
    }
}

/// `m:ss` (or `h:mm:ss`) for a time in seconds.
pub fn clock_text(seconds: f64) -> String {
    let s = if seconds.is_finite() {
        seconds.max(0.0) as u64
    } else {
        0
    };
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn header() -> CastHeader {
        CastHeader {
            cols: 100,
            rows: 30,
            timestamp: 1_700_000_000,
            title: "web01 \"prod\"".into(),
        }
    }

    #[test]
    fn writes_and_reads_back() {
        let mut w = CastWriter::new(Vec::new(), &header()).unwrap();
        w.output_at(Duration::from_millis(100), b"hello\r\n")
            .unwrap();
        // A character cut in two by the chunks comes out whole.
        w.output_at(Duration::from_millis(200), b"caf\xc3").unwrap();
        w.output_at(Duration::from_millis(300), b"\xa9 \x1b[31mred\x07")
            .unwrap();
        w.resize_at(Duration::from_millis(400), 120, 40).unwrap();
        // Bytes that are not UTF-8.
        w.output_at(Duration::from_millis(500), b"\xff!").unwrap();
        let file = w.finish().unwrap();
        let text = String::from_utf8(file.clone()).unwrap();
        assert!(text.starts_with("{\"version\": 2, \"width\": 100, \"height\": 30,"));
        assert!(text.contains("\"title\": \"web01 \\\"prod\\\"\""));
        assert!(text.contains("\\u001b[31mred\\u0007"));

        let cast = Cast::parse(&file).unwrap();
        assert_eq!((cast.cols, cast.rows), (100, 30));
        assert_eq!(cast.title.as_deref(), Some("web01 \"prod\""));
        assert_eq!(cast.skipped, 0);
        let events: Vec<_> = cast.events.iter().map(|(_, e)| e.clone()).collect();
        assert_eq!(
            events,
            vec![
                CastEvent::Output("hello\r\n".into()),
                CastEvent::Output("caf".into()),
                CastEvent::Output("é \x1b[31mred\x07".into()),
                CastEvent::Resize(120, 40),
                CastEvent::Output("\u{FFFD}!".into()),
            ]
        );
        assert!((cast.events[3].0 - 0.4).abs() < 1e-9);
    }

    #[test]
    fn sealed_recording_verifies() {
        let mut file = Vec::new();
        let mut chain = Vec::new();
        {
            let w = CastWriter::sealed(&mut file, &mut chain, "a.cast", &header());
            let mut w = w.unwrap();
            w.output(b"x").unwrap();
            assert!(w.finish_sealed().unwrap().is_some());
        }
        let chain = String::from_utf8(chain).unwrap();
        assert!(crate::chain::verify("a.cast", &file, &chain).is_ok());
    }

    #[test]
    fn reads_asciinema_files() {
        let file = br##"{"version": 2, "width": 80, "height": 24, "timestamp": 1504467315, "idle_time_limit": 2.5, "env": {"SHELL": "/bin/zsh", "TERM": "xterm-256color"}, "theme": {"fg": "#d0d0d0", "palette": "#000:#111"}}
[0.248848, "o", "\u001b[1;31mHello \u001b[32mWorld!\u001b[0m\n"]
[1.001376, "i", "secret"]
[1.5, "m", "chapter"]
[10.0, "o", "later \ud83d\ude00"]
[11.0, "r", "90x30"]
[11.5, "o", "cut off
"##;
        let cast = Cast::parse(file).unwrap();
        assert_eq!((cast.cols, cast.rows), (80, 24));
        assert_eq!(cast.skipped, 1);
        assert_eq!(cast.events.len(), 3);
        assert_eq!(
            cast.events[0].1,
            CastEvent::Output("\x1b[1;31mHello \x1b[32mWorld!\x1b[0m\n".into())
        );
        // The pause of almost 10 s is cut to 2.5.
        assert!((cast.events[1].0 - (0.248848 + 2.5)).abs() < 1e-9);
        assert_eq!(cast.events[1].1, CastEvent::Output("later 😀".into()));
        assert_eq!(cast.events[2].1, CastEvent::Resize(90, 30));
        // Input never plays.
        assert!(
            !cast
                .events
                .iter()
                .any(|(_, e)| matches!(e, CastEvent::Output(s) if s.contains("secret")))
        );
    }

    #[test]
    fn refuses_what_is_not_a_recording() {
        assert_eq!(Cast::parse(b""), Err(CastError::NotCast));
        assert_eq!(Cast::parse(b"hello\n"), Err(CastError::NotCast));
        assert_eq!(Cast::parse(b"[1, \"o\", \"x\"]\n"), Err(CastError::NotCast));
        assert_eq!(
            Cast::parse(b"{\"version\": 3, \"term\": {}}\n"),
            Err(CastError::Version(3))
        );
        // Nesting past the limit does not overflow the stack.
        let deep = format!("{{\"version\": 2, \"x\": {}}}", "[".repeat(100_000));
        assert_eq!(Cast::parse(deep.as_bytes()), Err(CastError::NotCast));
    }

    #[test]
    fn clamps_sizes_and_times() {
        let file = b"{\"version\": 2, \"width\": 1e9, \"height\": -4}\n\
            [5, \"o\", \"a\"]\n[2, \"o\", \"b\"]\n[1e300, \"r\", \"99999x0\"]\n[NaN, \"o\", \"c\"]\n[3, \"r\", \"5000x7000\"]\n";
        let cast = Cast::parse(file).unwrap();
        assert_eq!((cast.cols, cast.rows), (MAX_COLS, 24));
        // Times never go back.
        assert_eq!(cast.events[0].0, 5.0);
        assert_eq!(cast.events[1].0, 5.0);
        assert_eq!(cast.events[2].1, CastEvent::Resize(MAX_COLS, MAX_ROWS));
        assert_eq!(cast.skipped, 2);
    }

    fn cast(times: &[f64]) -> Cast {
        Cast {
            cols: 80,
            rows: 24,
            title: None,
            events: times
                .iter()
                .map(|t| (*t, CastEvent::Output(format!("{t}"))))
                .collect(),
            skipped: 0,
        }
    }

    fn outputs(plays: &[Play<'_>]) -> Vec<String> {
        plays
            .iter()
            .map(|p| match p {
                Play::Output(s) => (*s).to_owned(),
                Play::Reset(..) => "reset".to_owned(),
                Play::Resize(..) => "resize".to_owned(),
            })
            .collect()
    }

    #[test]
    fn plays_in_time() {
        let t0 = Instant::now();
        let at = |s: f64| t0 + Duration::from_secs_f64(s);
        let mut p = Player::new(cast(&[0.0, 1.0, 2.0, 4.0]), t0);
        assert_eq!(outputs(&p.due(at(0.0))), ["0"]);
        assert_eq!(p.wait(at(0.5)), Some(Duration::from_millis(500)));
        assert_eq!(outputs(&p.due(at(2.0))), ["1", "2"]);
        p.toggle_pause(at(2.0));
        assert!(p.due(at(100.0)).is_empty());
        assert_eq!(p.wait(at(100.0)), None);
        p.toggle_pause(at(100.0));
        // Twice as fast: 4.0 is 2 s of recording away, 1 s of real time.
        p.change_speed(true, at(100.0));
        p.change_speed(true, at(100.0));
        assert_eq!(p.speed(), 2.0);
        assert_eq!(p.wait(at(100.0)), Some(Duration::from_secs(1)));
        assert_eq!(outputs(&p.due(at(101.0))), ["4"]);
        assert!(p.ended() && p.paused());
        // Space after the end plays again from the start.
        p.toggle_pause(at(200.0));
        assert_eq!(outputs(&p.due(at(200.0))), ["reset", "0"]);
    }

    #[test]
    fn seeks_both_ways() {
        let t0 = Instant::now();
        let mut p = Player::new(cast(&[0.0, 1.0, 2.0, 3.0]), t0);
        p.seek(2.5, t0);
        assert_eq!(outputs(&p.due(t0)), ["0", "1", "2"]);
        p.seek(1.0, t0);
        assert_eq!(outputs(&p.due(t0)), ["reset", "0", "1"]);
        p.seek(-5.0, t0);
        assert_eq!(outputs(&p.due(t0)), ["reset", "0"]);
        p.seek(99.0, t0);
        assert_eq!(outputs(&p.due(t0)), ["1", "2", "3"]);
    }

    #[test]
    fn clock() {
        assert_eq!(clock_text(0.0), "0:00");
        assert_eq!(clock_text(75.9), "1:15");
        assert_eq!(clock_text(3725.0), "1:02:05");
        assert_eq!(clock_text(f64::NAN), "0:00");
    }
}
