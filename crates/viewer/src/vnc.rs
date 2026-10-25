//! The VNC client: RFB 3.3, 3.7 and 3.8 (RFC 6143), without I/O.
//!
//! [`Client::feed`] takes what the server sent; [`Client::take_output`]
//! gives what to send it, and [`Client::take_events`] what changed on the
//! remote screen. Pixels are always asked for as 32-bit little-endian
//! `0x00RRGGBB`, which is what Direct2D draws as B8G8R8A8.
//!
//! Security types: None and VNC Authentication. TLS-based types
//! (VeNCrypt, TLS) are refused; the connection runs inside SSH, which
//! already encrypts it.
//!
//! Every length and rectangle the server sends is checked: rectangles must
//! lie on the screen, screens are at most [`MAX_SIDE`] wide and high and
//! [`MAX_PIXELS`] in all, and compressed data may not inflate beyond what
//! its rectangle can hold. Anything else ends the session with
//! [`Event::Failed`].

use des::Des;
use des::cipher::{Block, BlockCipherEncrypt, KeyInit};
use miniz_oxide::inflate::stream::InflateState;
use zeroize::Zeroizing;

use crate::{MAX_PIXELS, MAX_SIDE};

/// Encodings, as RFC 6143 numbers them.
mod encoding {
    pub const RAW: i32 = 0;
    pub const COPY_RECT: i32 = 1;
    pub const RRE: i32 = 2;
    pub const HEXTILE: i32 = 5;
    pub const ZRLE: i32 = 16;
    /// Pseudo-encoding: the screen changed size.
    pub const DESKTOP_SIZE: i32 = -223;
}

/// The longest reason or desktop name read.
const MAX_TEXT: u32 = 64 * 1024;
/// The largest clipboard text a server may send; it is skipped.
const MAX_CUT_TEXT: u32 = 16 * 1024 * 1024;
/// The largest compressed ZRLE rectangle.
const MAX_ZRLE: u32 = 64 * 1024 * 1024;

/// What the server changed, for the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The screen is now this size (and blank until drawn).
    Size {
        width: u16,
        height: u16,
    },
    /// The desktop's name.
    Name(String),
    /// New pixels for a rectangle, row by row, `0x00RRGGBB`.
    Pixels {
        x: u16,
        y: u16,
        width: u16,
        height: u16,
        data: Vec<u32>,
    },
    /// Copy a rectangle of the screen from (`src_x`, `src_y`).
    Copy {
        src_x: u16,
        src_y: u16,
        x: u16,
        y: u16,
        width: u16,
        height: u16,
    },
    Bell,
    /// The server wants the VNC password: call [`Client::password`].
    NeedPassword,
    /// The session is over.
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Version {
    V3,
    V7,
    V8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rect {
    x: u16,
    y: u16,
    width: u16,
    height: u16,
}

impl Rect {
    fn area(self) -> usize {
        usize::from(self.width) * usize::from(self.height)
    }
}

/// A Hextile rectangle part-way decoded: tiles arrive one by one.
#[derive(Debug)]
struct Hextile {
    rect: Rect,
    data: Vec<u32>,
    /// The next tile, counted left to right, top to bottom.
    tile: usize,
    bg: u32,
    fg: u32,
}

#[derive(Debug)]
enum State {
    Version,
    SecurityTypes(Version),
    /// RFB 3.3: the server chose.
    SecurityType33,
    Challenge(Version),
    WaitPassword {
        challenge: [u8; 16],
        version: Version,
    },
    SecurityResult(Version),
    /// The server explained why it refused.
    Reason(&'static str),
    ServerInit,
    Normal,
    /// Rectangles still to come in a framebuffer update.
    Update(u16),
    Rect {
        rect: Rect,
        encoding: i32,
        left: u16,
    },
    Hextile {
        tile: Box<Hextile>,
        left: u16,
    },
    Failed,
}

/// A cursor over the unread input; `None` from a read means "not yet".
struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let b = self.data.get(self.pos..end)?;
        self.pos = end;
        Some(b)
    }

    fn u8(&mut self) -> Option<u8> {
        self.bytes(1)?.first().copied()
    }

    fn u16(&mut self) -> Option<u16> {
        let b = self.bytes(2)?;
        Some(u16::from_be_bytes([*b.first()?, *b.get(1)?]))
    }

    fn u32(&mut self) -> Option<u32> {
        let b: [u8; 4] = self.bytes(4)?.try_into().ok()?;
        Some(u32::from_be_bytes(b))
    }

    /// A 32-bit pixel in the format the client asked for.
    fn pixel(&mut self) -> Option<u32> {
        let b: [u8; 4] = self.bytes(4)?.try_into().ok()?;
        Some(u32::from_le_bytes(b) & 0x00ff_ffff)
    }

    fn has(&self, n: usize) -> bool {
        self.data.len().saturating_sub(self.pos) >= n
    }
}

/// What one step of parsing came to.
enum Step {
    /// Consumed this many bytes; go on.
    Done(usize),
    /// Not enough input yet.
    More,
    Fail(String),
}

/// A VNC session's client side.
pub struct Client {
    input: Vec<u8>,
    /// Bytes of `input` already parsed.
    pos: usize,
    state: State,
    output: Vec<u8>,
    events: Vec<Event>,
    width: u16,
    height: u16,
    zlib: Box<InflateState>,
    password: Option<Zeroizing<Vec<u8>>>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("state", &self.state)
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}

/// The DES key VNC derives from a password: its first eight bytes, each
/// with its bits reversed.
fn vnc_key(password: &[u8]) -> Zeroizing<[u8; 8]> {
    let mut key = Zeroizing::new([0u8; 8]);
    for (k, p) in key.iter_mut().zip(password) {
        *k = p.reverse_bits();
    }
    key
}

/// The answer to a VNC Authentication challenge.
pub fn vnc_response(password: &[u8], challenge: &[u8; 16]) -> [u8; 16] {
    let key = vnc_key(password);
    let mut out = *challenge;
    if let Ok(des) = Des::new_from_slice(&key[..]) {
        for chunk in out.chunks_exact_mut(8) {
            let mut block = Block::<Des>::default();
            block.copy_from_slice(chunk);
            des.encrypt_block(&mut block);
            chunk.copy_from_slice(&block);
        }
    }
    out
}

fn size_ok(width: u16, height: u16) -> bool {
    width <= MAX_SIDE
        && height <= MAX_SIDE
        && usize::from(width) * usize::from(height) <= MAX_PIXELS
}

impl Client {
    pub fn new() -> Self {
        Self {
            input: Vec::new(),
            pos: 0,
            state: State::Version,
            output: Vec::new(),
            events: Vec::new(),
            width: 0,
            height: 0,
            zlib: InflateState::new_boxed(miniz_oxide::DataFormat::Zlib),
            password: None,
        }
    }

    /// The screen size, once known.
    pub fn size(&self) -> (u16, u16) {
        (self.width, self.height)
    }

    /// Whether the session ended.
    pub fn failed(&self) -> bool {
        matches!(self.state, State::Failed)
    }

    /// Bytes for the server.
    pub fn take_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.output)
    }

    /// What happened since the last call.
    pub fn take_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
    }

    /// The VNC password, given when [`Event::NeedPassword`] asked for it
    /// (or before).
    pub fn password(&mut self, password: &[u8]) {
        self.password = Some(Zeroizing::new(password.to_vec()));
        if let State::WaitPassword { challenge, version } = self.state {
            self.answer(&challenge);
            self.state = State::SecurityResult(version);
            // The result may already be here.
            self.parse();
        }
    }

    /// A key went down or up; `keysym` is an X keysym.
    pub fn key(&mut self, down: bool, keysym: u32) {
        if matches!(
            self.state,
            State::Normal | State::Update(_) | State::Rect { .. } | State::Hextile { .. }
        ) {
            self.output.extend([4, u8::from(down), 0, 0]);
            self.output.extend(keysym.to_be_bytes());
        }
    }

    /// The pointer is at (`x`, `y`) with these buttons down (bit 0 left,
    /// 1 middle, 2 right, 3 and 4 the wheel up and down).
    pub fn pointer(&mut self, buttons: u8, x: u16, y: u16) {
        if matches!(
            self.state,
            State::Normal | State::Update(_) | State::Rect { .. } | State::Hextile { .. }
        ) {
            let x = x.min(self.width.saturating_sub(1));
            let y = y.min(self.height.saturating_sub(1));
            self.output.push(5);
            self.output.push(buttons);
            self.output.extend(x.to_be_bytes());
            self.output.extend(y.to_be_bytes());
        }
    }

    /// Takes bytes from the server.
    pub fn feed(&mut self, data: &[u8]) {
        if self.failed() {
            return;
        }
        // Drop what was parsed before growing the buffer.
        if self.pos > 0 {
            self.input.drain(..self.pos);
            self.pos = 0;
        }
        self.input.extend_from_slice(data);
        self.parse();
    }

    fn fail(&mut self, why: String) {
        self.state = State::Failed;
        self.input = Vec::new();
        self.pos = 0;
        self.events.push(Event::Failed(why));
    }

    fn parse(&mut self) {
        loop {
            let input = std::mem::take(&mut self.input);
            let step = self.step(input.get(self.pos..).unwrap_or_default());
            self.input = input;
            match step {
                Step::Done(n) => self.pos += n,
                Step::More => return,
                Step::Fail(why) => {
                    self.fail(why);
                    return;
                }
            }
            if matches!(self.state, State::Failed | State::WaitPassword { .. }) {
                return;
            }
        }
    }

    fn answer(&mut self, challenge: &[u8; 16]) {
        let password = self
            .password
            .as_deref()
            .map(Vec::as_slice)
            .unwrap_or_default();
        self.output.extend(vnc_response(password, challenge));
    }

    /// One message, or one part of one.
    fn step(&mut self, data: &[u8]) -> Step {
        let mut r = Reader::new(data);
        macro_rules! need {
            ($e:expr) => {
                match $e {
                    Some(v) => v,
                    None => return Step::More,
                }
            };
        }
        match std::mem::replace(&mut self.state, State::Failed) {
            State::Failed => {
                self.state = State::Failed;
                Step::More
            }
            State::Version => {
                let Some(line) = r.bytes(12) else {
                    self.state = State::Version;
                    return Step::More;
                };
                let Some(version) = parse_version(line) else {
                    let shown: String = line
                        .iter()
                        .map(|&b| if b.is_ascii_graphic() { b as char } else { '?' })
                        .collect();
                    return Step::Fail(format!("This is not a VNC server (it said \"{shown}\")."));
                };
                let minor = match version {
                    Version::V3 => 3,
                    Version::V7 => 7,
                    Version::V8 => 8,
                };
                self.output
                    .extend(format!("RFB 003.{minor:03}\n").into_bytes());
                self.state = match version {
                    Version::V3 => State::SecurityType33,
                    v => State::SecurityTypes(v),
                };
                Step::Done(r.pos)
            }
            State::SecurityType33 => {
                let Some(kind) = r.u32() else {
                    self.state = State::SecurityType33;
                    return Step::More;
                };
                self.state = match kind {
                    0 => State::Reason("The VNC server refused the connection"),
                    1 => self.client_init(),
                    2 => State::Challenge(Version::V3),
                    k => {
                        return Step::Fail(format!(
                            "The VNC server asks for security type {k}, which FeroSSH does not support."
                        ));
                    }
                };
                Step::Done(r.pos)
            }
            State::SecurityTypes(v) => {
                let state = State::SecurityTypes(v);
                let Some(n) = r.u8() else {
                    self.state = state;
                    return Step::More;
                };
                if n == 0 {
                    self.state = State::Reason("The VNC server refused the connection");
                    return Step::Done(r.pos);
                }
                let Some(types) = r.bytes(usize::from(n)) else {
                    self.state = state;
                    return Step::More;
                };
                if types.contains(&1) {
                    self.output.push(1);
                    self.state = if v == Version::V8 {
                        State::SecurityResult(v)
                    } else {
                        self.client_init()
                    };
                } else if types.contains(&2) {
                    self.output.push(2);
                    self.state = State::Challenge(v);
                } else {
                    let list: Vec<String> = types.iter().map(|t| security_name(*t)).collect();
                    return Step::Fail(format!(
                        "The VNC server only offers {}. FeroSSH supports no password or a VNC \
                         password; the SSH tunnel encrypts the connection.",
                        list.join(", ")
                    ));
                }
                Step::Done(r.pos)
            }
            State::Challenge(v) => {
                let Some(c) = r.bytes(16) else {
                    self.state = State::Challenge(v);
                    return Step::More;
                };
                let mut challenge = [0u8; 16];
                challenge.copy_from_slice(c);
                if self.password.is_some() {
                    self.answer(&challenge);
                    self.state = State::SecurityResult(v);
                } else {
                    self.events.push(Event::NeedPassword);
                    self.state = State::WaitPassword {
                        challenge,
                        version: v,
                    };
                }
                Step::Done(r.pos)
            }
            s @ State::WaitPassword { .. } => {
                self.state = s;
                Step::More
            }
            State::SecurityResult(v) => {
                let Some(result) = r.u32() else {
                    self.state = State::SecurityResult(v);
                    return Step::More;
                };
                if result == 0 {
                    self.state = self.client_init();
                    return Step::Done(r.pos);
                }
                // 3.8 says why; older servers just close.
                if v == Version::V8 {
                    self.state = State::Reason("VNC authentication failed");
                    return Step::Done(r.pos);
                }
                Step::Fail("VNC authentication failed: wrong password?".into())
            }
            State::Reason(what) => {
                let Some(len) = r.u32() else {
                    self.state = State::Reason(what);
                    return Step::More;
                };
                if len > MAX_TEXT {
                    return Step::Fail(format!("{what}."));
                }
                let Some(text) = r.bytes(len as usize) else {
                    self.state = State::Reason(what);
                    return Step::More;
                };
                Step::Fail(format!("{what}: {}", clean_text(text)))
            }
            State::ServerInit => {
                self.state = State::ServerInit;
                let width = need!(r.u16());
                let height = need!(r.u16());
                let _format = need!(r.bytes(16));
                let len = need!(r.u32());
                if len > MAX_TEXT {
                    return Step::Fail("The VNC server's desktop name is too long.".into());
                }
                let name = need!(r.bytes(len as usize));
                if !size_ok(width, height) {
                    return Step::Fail(format!(
                        "The remote screen is {width}x{height}, larger than FeroSSH shows."
                    ));
                }
                self.width = width;
                self.height = height;
                self.events.push(Event::Size { width, height });
                self.events.push(Event::Name(clean_text(name)));
                self.set_up();
                self.state = State::Normal;
                Step::Done(r.pos)
            }
            State::Normal => {
                self.state = State::Normal;
                let kind = need!(r.u8());
                match kind {
                    0 => {
                        let _pad = need!(r.u8());
                        let n = need!(r.u16());
                        self.state = State::Update(n);
                    }
                    1 => {
                        let _pad = need!(r.u8());
                        let _first = need!(r.u16());
                        let n = need!(r.u16());
                        need!(r.bytes(usize::from(n) * 6));
                    }
                    2 => self.events.push(Event::Bell),
                    3 => {
                        let _pad = need!(r.bytes(3));
                        let len = need!(r.u32());
                        if len > MAX_CUT_TEXT {
                            return Step::Fail(
                                "The VNC server sent too much clipboard text.".into(),
                            );
                        }
                        // Not used: the remote clipboard never reaches
                        // this PC's.
                        need!(r.bytes(len as usize));
                    }
                    k => {
                        return Step::Fail(format!(
                            "The VNC server sent message type {k}, which FeroSSH does not know."
                        ));
                    }
                }
                Step::Done(r.pos)
            }
            State::Update(0) => {
                self.request(true);
                self.state = State::Normal;
                Step::Done(0)
            }
            State::Update(left) => {
                self.state = State::Update(left);
                let x = need!(r.u16());
                let y = need!(r.u16());
                let width = need!(r.u16());
                let height = need!(r.u16());
                let encoding = need!(r.u32()) as i32;
                let rect = Rect {
                    x,
                    y,
                    width,
                    height,
                };
                if encoding != encoding::DESKTOP_SIZE && !self.on_screen(rect) {
                    return Step::Fail(format!(
                        "The VNC server drew a {width}x{height} rectangle at {x},{y}, off its \
                         {}x{} screen.",
                        self.width, self.height
                    ));
                }
                self.state = State::Rect {
                    rect,
                    encoding,
                    left: left - 1,
                };
                Step::Done(r.pos)
            }
            State::Rect {
                rect,
                encoding,
                left,
            } => {
                let step = self.rect(&mut r, rect, encoding, left);
                if matches!(step, Step::More) && matches!(self.state, State::Failed) {
                    self.state = State::Rect {
                        rect,
                        encoding,
                        left,
                    };
                }
                step
            }
            State::Hextile { mut tile, left } => match hextile_tiles(&mut r, &mut tile) {
                Ok(true) => {
                    let Hextile { rect, data, .. } = *tile;
                    self.pixels(rect, data);
                    self.state = State::Update(left);
                    Step::Done(r.pos)
                }
                Ok(false) => {
                    self.state = State::Hextile { tile, left };
                    if r.pos > 0 {
                        Step::Done(r.pos)
                    } else {
                        Step::More
                    }
                }
                Err(why) => Step::Fail(why),
            },
        }
    }

    /// One rectangle's data, by encoding. Leaves `self.state` as `Failed`
    /// when more input is needed (the caller puts the state back).
    fn rect(&mut self, r: &mut Reader<'_>, rect: Rect, encoding: i32, left: u16) -> Step {
        macro_rules! need {
            ($e:expr) => {
                match $e {
                    Some(v) => v,
                    None => return Step::More,
                }
            };
        }
        match encoding {
            encoding::RAW => {
                let bytes = need!(r.bytes(rect.area() * 4));
                let data = bytes.chunks_exact(4).map(crate::rgb).collect();
                self.pixels(rect, data);
            }
            encoding::COPY_RECT => {
                let src_x = need!(r.u16());
                let src_y = need!(r.u16());
                let src = Rect {
                    x: src_x,
                    y: src_y,
                    ..rect
                };
                if !self.on_screen(src) {
                    return Step::Fail("The VNC server copied from off its screen.".into());
                }
                if rect.area() > 0 {
                    self.events.push(Event::Copy {
                        src_x,
                        src_y,
                        x: rect.x,
                        y: rect.y,
                        width: rect.width,
                        height: rect.height,
                    });
                }
            }
            encoding::RRE => {
                let n = need!(r.u32()) as usize;
                if n > rect.area() {
                    return Step::Fail(
                        "The VNC server sent more RRE subrectangles than pixels.".into(),
                    );
                }
                if !r.has(4 + n * 12) {
                    return Step::More;
                }
                let bg = need!(r.pixel());
                let mut data = vec![bg; rect.area()];
                for _ in 0..n {
                    let colour = need!(r.pixel());
                    let sx = need!(r.u16());
                    let sy = need!(r.u16());
                    let sw = need!(r.u16());
                    let sh = need!(r.u16());
                    if !fill(&mut data, rect.width, rect.height, (sx, sy, sw, sh), colour) {
                        return Step::Fail(
                            "An RRE subrectangle lies outside its rectangle.".into(),
                        );
                    }
                }
                self.pixels(rect, data);
            }
            encoding::HEXTILE => {
                self.state = State::Hextile {
                    tile: Box::new(Hextile {
                        rect,
                        data: vec![0; rect.area()],
                        tile: 0,
                        bg: 0,
                        fg: 0,
                    }),
                    left,
                };
                return Step::Done(r.pos);
            }
            encoding::ZRLE => {
                let len = need!(r.u32());
                if len > MAX_ZRLE {
                    return Step::Fail("A ZRLE rectangle is too large.".into());
                }
                let compressed = need!(r.bytes(len as usize));
                match crate::zrle::decode(&mut self.zlib, compressed, rect.width, rect.height) {
                    Ok(data) => self.pixels(rect, data),
                    Err(why) => return Step::Fail(why),
                }
            }
            encoding::DESKTOP_SIZE => {
                if !size_ok(rect.width, rect.height) {
                    return Step::Fail(format!(
                        "The remote screen is now {}x{}, larger than FeroSSH shows.",
                        rect.width, rect.height
                    ));
                }
                self.width = rect.width;
                self.height = rect.height;
                self.events.push(Event::Size {
                    width: rect.width,
                    height: rect.height,
                });
            }
            e => {
                return Step::Fail(format!(
                    "The VNC server used encoding {e}, which FeroSSH did not ask for."
                ));
            }
        }
        self.state = State::Update(left);
        Step::Done(r.pos)
    }

    fn on_screen(&self, rect: Rect) -> bool {
        u32::from(rect.x) + u32::from(rect.width) <= u32::from(self.width)
            && u32::from(rect.y) + u32::from(rect.height) <= u32::from(self.height)
    }

    fn pixels(&mut self, rect: Rect, data: Vec<u32>) {
        if rect.area() > 0 {
            self.events.push(Event::Pixels {
                x: rect.x,
                y: rect.y,
                width: rect.width,
                height: rect.height,
                data,
            });
        }
    }

    /// ClientInit: share the desktop with other viewers.
    fn client_init(&mut self) -> State {
        self.output.push(1);
        State::ServerInit
    }

    /// The pixel format, the encodings, and the first full update.
    fn set_up(&mut self) {
        // SetPixelFormat: 32 bits, depth 24, little-endian, true colour,
        // 8 bits each of red, green and blue at 16, 8 and 0.
        self.output.extend([0, 0, 0, 0]);
        self.output
            .extend([32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0]);
        let encodings = [
            encoding::ZRLE,
            encoding::HEXTILE,
            encoding::RRE,
            encoding::COPY_RECT,
            encoding::RAW,
            encoding::DESKTOP_SIZE,
        ];
        self.output.extend([2, 0]);
        self.output
            .extend(u16::try_from(encodings.len()).unwrap_or(0).to_be_bytes());
        for e in encodings {
            self.output.extend(e.to_be_bytes());
        }
        self.request(false);
    }

    /// FramebufferUpdateRequest for the whole screen.
    fn request(&mut self, incremental: bool) {
        self.output.extend([3, u8::from(incremental), 0, 0, 0, 0]);
        self.output.extend(self.width.to_be_bytes());
        self.output.extend(self.height.to_be_bytes());
    }
}

fn parse_version(line: &[u8]) -> Option<Version> {
    let text = std::str::from_utf8(line).ok()?;
    let rest = text.strip_prefix("RFB ")?.strip_suffix('\n')?;
    let (major, minor) = rest.split_once('.')?;
    let (major, minor): (u32, u32) = (major.parse().ok()?, minor.parse().ok()?);
    match (major, minor) {
        (3, 7) => Some(Version::V7),
        // 3.889 is Apple's 3.8.
        (3, m) if m >= 8 => Some(Version::V8),
        (3, _) => Some(Version::V3),
        (m, _) if m > 3 => Some(Version::V8),
        _ => None,
    }
}

fn security_name(kind: u8) -> String {
    match kind {
        5 => "RA2".into(),
        16 => "Tight".into(),
        18 => "TLS".into(),
        19 => "VeNCrypt".into(),
        30 => "Apple remote desktop".into(),
        k => format!("security type {k}"),
    }
}

/// Text from the server, without control characters.
fn clean_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .chars()
        .filter(|c| !c.is_control())
        .take(256)
        .collect()
}

/// Fills (`x`, `y`, `w`, `h`) of a `width` x `height` pixel block; false
/// when it does not fit.
pub(crate) fn fill(
    data: &mut [u32],
    width: u16,
    height: u16,
    (x, y, w, h): (u16, u16, u16, u16),
    colour: u32,
) -> bool {
    if u32::from(x) + u32::from(w) > u32::from(width)
        || u32::from(y) + u32::from(h) > u32::from(height)
    {
        return false;
    }
    let stride = usize::from(width);
    for row in usize::from(y)..usize::from(y) + usize::from(h) {
        let start = row * stride + usize::from(x);
        if let Some(line) = data.get_mut(start..start + usize::from(w)) {
            line.fill(colour);
        }
    }
    true
}

/// Decodes Hextile tiles while input lasts; `Ok(true)` once the rectangle
/// is done.
fn hextile_tiles(r: &mut Reader<'_>, h: &mut Hextile) -> Result<bool, String> {
    const RAW: u8 = 1;
    const BG: u8 = 2;
    const FG: u8 = 4;
    const ANY: u8 = 8;
    const COLOURED: u8 = 16;
    let (rw, rh) = (h.rect.width, h.rect.height);
    let across = usize::from(rw).div_ceil(16);
    let tiles = across * usize::from(rh).div_ceil(16);
    while h.tile < tiles {
        let tx = (h.tile % across) as u16 * 16;
        let ty = (h.tile / across) as u16 * 16;
        let tw = (rw - tx).min(16);
        let th = (rh - ty).min(16);
        let start = r.pos;
        let mut parse = || -> Option<Result<(), String>> {
            let mask = r.u8()?;
            let mut tile = vec![0u32; usize::from(tw) * usize::from(th)];
            if mask & RAW != 0 {
                for p in &mut tile {
                    *p = r.pixel()?;
                }
            } else {
                let mut bg = h.bg;
                let mut fg = h.fg;
                if mask & BG != 0 {
                    bg = r.pixel()?;
                }
                if mask & FG != 0 {
                    fg = r.pixel()?;
                }
                tile.fill(bg);
                if mask & ANY != 0 {
                    let n = r.u8()?;
                    for _ in 0..n {
                        let colour = if mask & COLOURED != 0 { r.pixel()? } else { fg };
                        let xy = r.u8()?;
                        let wh = r.u8()?;
                        let sub = (
                            u16::from(xy >> 4),
                            u16::from(xy & 15),
                            u16::from(wh >> 4) + 1,
                            u16::from(wh & 15) + 1,
                        );
                        if !fill(&mut tile, tw, th, sub, colour) {
                            return Some(Err(
                                "A Hextile subrectangle lies outside its tile.".into()
                            ));
                        }
                    }
                }
                h.bg = bg;
                h.fg = fg;
            }
            // Into the rectangle.
            let stride = usize::from(rw);
            for (row, line) in tile.chunks_exact(usize::from(tw)).enumerate() {
                let at = (usize::from(ty) + row) * stride + usize::from(tx);
                if let Some(dst) = h.data.get_mut(at..at + usize::from(tw)) {
                    dst.copy_from_slice(line);
                }
            }
            Some(Ok(()))
        };
        match parse() {
            Some(Ok(())) => h.tile += 1,
            Some(Err(why)) => return Err(why),
            None => {
                r.pos = start;
                return Ok(false);
            }
        }
    }
    Ok(true)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;

    /// A server's ServerInit for a `w` x `h` screen called "test".
    fn server_init(w: u16, h: u16) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend(w.to_be_bytes());
        v.extend(h.to_be_bytes());
        v.extend([32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0]);
        v.extend(4u32.to_be_bytes());
        v.extend(b"test");
        v
    }

    fn rect_header(x: u16, y: u16, w: u16, h: u16, enc: i32) -> Vec<u8> {
        let mut v = Vec::new();
        for n in [x, y, w, h] {
            v.extend(n.to_be_bytes());
        }
        v.extend(enc.to_be_bytes());
        v
    }

    /// A client logged in to a 3.8 server without a password.
    fn logged_in(w: u16, h: u16) -> Client {
        let mut c = Client::new();
        c.feed(b"RFB 003.008\n");
        assert_eq!(c.take_output(), b"RFB 003.008\n");
        c.feed(&[2, 2, 1]);
        assert_eq!(c.take_output(), [1]);
        c.feed(&0u32.to_be_bytes());
        assert_eq!(c.take_output(), [1], "ClientInit, shared");
        c.feed(&server_init(w, h));
        assert_eq!(
            c.take_events(),
            [
                Event::Size {
                    width: w,
                    height: h
                },
                Event::Name("test".into())
            ]
        );
        let out = c.take_output();
        assert_eq!(out[0], 0, "SetPixelFormat first");
        assert_eq!(&out[out.len() - 10..out.len() - 4], &[3, 0, 0, 0, 0, 0]);
        c
    }

    #[test]
    fn version_is_capped_at_3_8() {
        for (server, reply) in [
            (&b"RFB 003.003\n"[..], &b"RFB 003.003\n"[..]),
            (b"RFB 003.007\n", b"RFB 003.007\n"),
            (b"RFB 003.889\n", b"RFB 003.008\n"),
            (b"RFB 004.001\n", b"RFB 003.008\n"),
        ] {
            let mut c = Client::new();
            c.feed(server);
            assert_eq!(c.take_output(), reply);
        }
        let mut c = Client::new();
        c.feed(b"SSH-2.0-Open");
        assert!(
            matches!(&c.take_events()[..], [Event::Failed(w)] if w.contains("not a VNC server"))
        );
    }

    #[test]
    fn vnc_password_matches_known_answer() {
        // Checked against `openssl enc -des-ecb -K 0e86ceceeef64e26`: the
        // key is "password" with each byte's bits reversed.
        let challenge: [u8; 16] = core::array::from_fn(|i| i as u8);
        let a = vnc_response(b"password", &challenge);
        assert_eq!(
            a,
            [
                0xb8, 0x66, 0x92, 0x41, 0x25, 0xc8, 0xee, 0xbb, 0x9d, 0xeb, 0xc1, 0xdb, 0x61, 0xc5,
                0x38, 0xe2
            ]
        );
        let b = vnc_response(b"password-longer", &challenge);
        assert_eq!(a, b, "only eight bytes count");
        assert_ne!(a, vnc_response(b"Password", &challenge));
        assert_ne!(a, challenge);
    }

    #[test]
    fn asks_for_the_password_then_logs_in() {
        let mut c = Client::new();
        c.feed(b"RFB 003.008\n");
        c.take_output();
        c.feed(&[1, 2]);
        assert_eq!(c.take_output(), [2]);
        let challenge = [7u8; 16];
        c.feed(&challenge);
        assert_eq!(c.take_events(), [Event::NeedPassword]);
        // The result comes before the password, as a fast server may not.
        c.password(b"secret");
        assert_eq!(c.take_output(), vnc_response(b"secret", &challenge));
        c.feed(&0u32.to_be_bytes());
        assert_eq!(c.take_output(), [1]);
        c.feed(&server_init(4, 4));
        assert_eq!(c.size(), (4, 4));
    }

    #[test]
    fn a_wrong_password_says_why() {
        let mut c = Client::new();
        c.password(b"x");
        c.feed(b"RFB 003.008\n\x01\x02");
        c.feed(&[0; 16]);
        let mut refusal = 1u32.to_be_bytes().to_vec();
        refusal.extend(9u32.to_be_bytes());
        refusal.extend(b"bad\x07pass!");
        c.feed(&refusal);
        assert!(
            matches!(&c.take_events()[..], [Event::Failed(w)] if w == "VNC authentication failed: badpass!"),
        );
    }

    #[test]
    fn tls_only_servers_are_refused_plainly() {
        let mut c = Client::new();
        c.feed(b"RFB 003.008\n\x02\x13\x12");
        let ev = c.take_events();
        assert!(
            matches!(&ev[..], [Event::Failed(w)] if w.contains("VeNCrypt, TLS")),
            "{ev:?}"
        );
    }

    #[test]
    fn rfb_3_3_without_password() {
        let mut c = Client::new();
        c.feed(b"RFB 003.003\n");
        c.feed(&1u32.to_be_bytes());
        assert_eq!(c.take_output(), b"RFB 003.003\n\x01");
        c.feed(&server_init(2, 2));
        assert_eq!(c.size(), (2, 2));
    }

    #[test]
    fn raw_copy_rre_and_size_rects() {
        let mut c = logged_in(8, 8);
        let mut msg = vec![0, 0, 0, 4];
        msg.extend(rect_header(0, 0, 2, 1, encoding::RAW));
        msg.extend([0x30, 0x20, 0x10, 0xff, 1, 2, 3, 0]);
        msg.extend(rect_header(4, 4, 2, 1, encoding::COPY_RECT));
        msg.extend([0, 0, 0, 0]);
        msg.extend(rect_header(0, 2, 3, 2, encoding::RRE));
        msg.extend(1u32.to_be_bytes());
        msg.extend([1, 0, 0, 0]);
        msg.extend([2, 0, 0, 0]);
        for n in [1u16, 0, 2, 2] {
            msg.extend(n.to_be_bytes());
        }
        msg.extend(rect_header(0, 0, 16, 4, encoding::DESKTOP_SIZE));
        // Byte by byte: every split point must work.
        for b in &msg {
            c.feed(&[*b]);
        }
        let ev = c.take_events();
        assert_eq!(
            ev,
            [
                Event::Pixels {
                    x: 0,
                    y: 0,
                    width: 2,
                    height: 1,
                    data: vec![0x102030, 0x030201]
                },
                Event::Copy {
                    src_x: 0,
                    src_y: 0,
                    x: 4,
                    y: 4,
                    width: 2,
                    height: 1
                },
                Event::Pixels {
                    x: 0,
                    y: 2,
                    width: 3,
                    height: 2,
                    data: vec![1, 2, 2, 1, 2, 2]
                },
                Event::Size {
                    width: 16,
                    height: 4
                },
            ]
        );
        // The update is over: ask for the next, incrementally.
        let out = c.take_output();
        assert_eq!(out, [3, 1, 0, 0, 0, 0, 0, 16, 0, 4]);
    }

    #[test]
    fn hextile_tiles_arrive_in_pieces() {
        let mut c = logged_in(20, 2);
        let mut msg = vec![0, 0, 0, 1];
        msg.extend(rect_header(0, 0, 20, 2, encoding::HEXTILE));
        // Tile 1 (16x2): background 5, one foreground subrect at 1,0 1x1.
        msg.extend([2 | 4 | 8, 5, 0, 0, 0, 9, 0, 0, 0, 1, 0x10, 0x00]);
        // Tile 2 (4x2): raw.
        msg.push(1);
        for p in 0..8u8 {
            msg.extend([p, 0, 0, 0]);
        }
        for b in &msg {
            c.feed(&[*b]);
        }
        let ev = c.take_events();
        let [Event::Pixels { data, .. }] = &ev[..] else {
            panic!("{ev:?}");
        };
        assert_eq!(data[0], 5);
        assert_eq!(data[1], 9);
        assert_eq!(data[16..20], [0, 1, 2, 3]);
        assert_eq!(data[20 + 16..], [4, 5, 6, 7]);
    }

    #[test]
    fn rectangles_off_the_screen_end_the_session() {
        let mut c = logged_in(8, 8);
        let mut msg = vec![0, 0, 0, 1];
        msg.extend(rect_header(7, 0, 2, 1, encoding::RAW));
        c.feed(&msg);
        assert!(matches!(&c.take_events()[..], [Event::Failed(w)] if w.contains("off its")));
        c.feed(&[0; 100]);
        assert!(c.take_events().is_empty(), "nothing after failing");

        let mut c = logged_in(8, 8);
        let mut msg = vec![0, 0, 0, 1];
        msg.extend(rect_header(0, 0, 9000, 9000, encoding::DESKTOP_SIZE));
        c.feed(&msg);
        assert!(matches!(&c.take_events()[..], [Event::Failed(_)]));

        let mut c = logged_in(8, 8);
        let mut msg = vec![0, 0, 0, 1];
        msg.extend(rect_header(0, 0, 2, 2, encoding::COPY_RECT));
        msg.extend([0, 7, 0, 7]);
        c.feed(&msg);
        assert!(matches!(&c.take_events()[..], [Event::Failed(_)]));
    }

    #[test]
    fn input_goes_out_once_running() {
        let mut c = Client::new();
        c.key(true, 0x61);
        assert!(c.take_output().is_empty(), "nothing before login");
        let mut c = logged_in(100, 100);
        c.key(true, 0xff0d);
        c.pointer(1, 500, 7);
        assert_eq!(
            c.take_output(),
            [4, 1, 0, 0, 0, 0, 0xff, 0x0d, 5, 1, 0, 99, 0, 7]
        );
    }

    #[test]
    fn bell_cut_text_and_colour_maps() {
        let mut c = logged_in(4, 4);
        c.feed(&[2]);
        c.feed(&[3, 0, 0, 0, 0, 0, 0, 3, b'a', b'b', b'c']);
        c.feed(&[1, 0, 0, 0, 0, 1, 1, 2, 3, 4, 5, 6]);
        assert_eq!(c.take_events(), [Event::Bell]);
        c.feed(&[0xee]);
        assert!(matches!(&c.take_events()[..], [Event::Failed(_)]));
    }
}
