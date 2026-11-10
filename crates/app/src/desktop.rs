//! A remote desktop in a pane: the window's half of the built-in VNC and
//! RDP viewer (the `vnc-viewer` and `rdp-viewer` features).
//!
//! Three parties: the desktop server, reached only through an SSH tunnel on
//! loopback; the viewer process, which parses everything the server sends
//! and has no network, file or registry access; and this side, which moves
//! bytes between the two and keeps the picture. What the viewer sends back
//! is checked frame by frame ([`FromViewer::decode`]) and rectangle by
//! rectangle ([`Framebuffer::apply`]) before it reaches the screen; a
//! viewer that misbehaves ends the session.
//!
//! Threads: one reads the tunnel and queues the bytes for the viewer, one
//! writes the queue to the viewer, and one reads the viewer, sending its
//! bytes for the server down the tunnel and the rest to the window. The
//! server's bytes in the queue, plus [`MESSAGE_COST`] for each message
//! holding them, are held to [`MAX_QUEUED_NET`]: past it the tunnel is not
//! read until the viewer catches up, so a slow (or stuck) viewer slows the
//! server down instead of filling the window's memory, however small the
//! pieces the server sends.

use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, PoisonError};

use ferossh_render::{Image, fit};
use ferossh_viewer::framebuffer::{Applied, Framebuffer};
use ferossh_viewer::keysym;
pub use ferossh_viewer::pipe::Protocol;
use ferossh_viewer::pipe::{
    FromViewer, MAX_NET, Secret, ToViewer, encode_wiped, read_frame, write_frame,
};
use zeroize::Zeroizing;

/// The longest password typed at a prompt, in bytes. Its buffer is this
/// big from the start and never grows, as a growing `String` would leave
/// copies of the first characters in freed memory, out of reach of
/// `Zeroizing`.
const MAX_TYPED_PASSWORD: usize = 256;

/// An empty buffer for a password typed at a prompt.
fn password_buffer() -> Zeroizing<String> {
    Zeroizing::new(String::with_capacity(MAX_TYPED_PASSWORD))
}

/// Adds a typed character to a password, unless the buffer would have to
/// move: past [`MAX_TYPED_PASSWORD`] the rest is dropped.
fn type_into(typed: &mut Zeroizing<String>, c: char) {
    if typed.len() + c.len_utf8() <= MAX_TYPED_PASSWORD {
        typed.push(c);
    }
}

/// The most bytes from the server queued for the viewer at once,
/// counting [`MESSAGE_COST`] for each message.
const MAX_QUEUED_NET: usize = 4 * 1024 * 1024;
/// What one queued message costs beyond its bytes: more than its `Vec`,
/// its slot in the channel and the allocator's share take, so that a
/// server sending its bytes one at a time fills the budget no less than
/// one sending them in big reads.
const MESSAGE_COST: usize = 256;

/// What a message of `n` of the server's bytes counts in the budget.
fn cost(n: usize) -> usize {
    n.saturating_add(MESSAGE_COST)
}

/// How many of the server's bytes wait in the queue for the viewer.
#[derive(Debug, Default)]
struct Budget {
    /// Bytes queued, and whether the viewer's writer stopped.
    state: Mutex<(usize, bool)>,
    changed: Condvar,
}

impl Budget {
    /// Waits until a message of `n` bytes fits, and counts it in.
    /// `false` once the writer stopped, as nobody will take it, and for
    /// more than [`MAX_NET`] bytes, which no message holds.
    fn take(&self, n: usize) -> bool {
        if n > MAX_NET {
            return false;
        }
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            let (queued, closed) = *state;
            if closed {
                return false;
            }
            if queued + cost(n) <= MAX_QUEUED_NET {
                state.0 = queued + cost(n);
                return true;
            }
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// A message of `n` bytes went to the viewer.
    fn give(&self, n: usize) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.0 = state.0.saturating_sub(cost(n));
        self.changed.notify_all();
    }

    /// The writer stopped.
    fn close(&self) {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).1 = true;
        self.changed.notify_all();
    }
}

/// Closes the budget when the writer thread ends, however it ends.
struct CloseOnDrop(Arc<Budget>);

impl Drop for CloseOnDrop {
    fn drop(&mut self) {
        self.0.close();
    }
}

/// What the window should do after [`Desktop::poll`].
#[derive(Debug, PartialEq, Eq)]
pub enum Update {
    /// The picture changed.
    Redraw,
    /// The desktop's name, for the tab.
    Title(String),
    Bell,
    /// Show this prompt; typing goes to it until Enter.
    Ask(String),
    /// The session is over, and why.
    Ended(String),
    /// Text copied on the remote desktop, for this PC's clipboard.
    Clipboard(String),
    /// Something the user should know about the session, for the pane.
    Note(String),
}

/// Where the session stands.
enum Phase {
    Connecting,
    /// Asking for the user name (RDP), with what was typed so far.
    User(String),
    Password(Zeroizing<String>),
    Running,
    Ended,
}

/// Names the phase only: never what was typed at a prompt.
impl std::fmt::Debug for Phase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Phase::Connecting => "Connecting",
            Phase::User(_) => "User",
            Phase::Password(_) => "Password",
            Phase::Running => "Running",
            Phase::Ended => "Ended",
        })
    }
}

enum FromThreads {
    Viewer(FromViewer),
    Ended(String),
}

/// What to open: the protocol, the screen size to ask an RDP server
/// for, the Windows keyboard layout id, and the user name to offer.
#[derive(Clone)]
pub struct Session {
    pub protocol: Protocol,
    pub width: u16,
    pub height: u16,
    pub layout: u32,
    pub user: String,
    /// The VNC password, when FeroSSH chose it (a virtual desktop's):
    /// given when the server asks, without a prompt.
    pub password: Option<Zeroizing<String>>,
    /// Keep `width` x `height` rather than follow the pane's size.
    pub fixed: bool,
    /// Which ways copied text may travel: to the host, from the host.
    pub clipboard: (bool, bool),
    /// The most the connection allows, which later changes of the global
    /// setting stay within.
    pub clipboard_limit: (bool, bool),
}

/// Everything but the password.
impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("protocol", &self.protocol)
            .field("size", &(self.width, self.height))
            .field("layout", &self.layout)
            .field("user", &self.user)
            .field("password", &self.password.as_ref().map(|_| ".."))
            .field("fixed", &self.fixed)
            .field("clipboard", &self.clipboard)
            .field("clipboard_limit", &self.clipboard_limit)
            .finish()
    }
}

/// One remote desktop session.
pub struct Desktop {
    protocol: Protocol,
    /// The user name offered when the prompt is left empty.
    user: String,
    /// The password to answer the server's first ask with.
    preset: Option<Zeroizing<String>>,
    /// The size stays as asked for: pane changes only scale the picture.
    fixed: bool,
    to_viewer: Sender<ToViewer>,
    from: Receiver<FromThreads>,
    tunnel: TcpStream,
    fb: Framebuffer,
    /// Changed since the last drawn frame: left, top, right, bottom.
    dirty: Option<(u32, u32, u32, u32)>,
    phase: Phase,
    buttons: u8,
    /// The pane size the viewer was last given.
    size: (u16, u16),
    /// Which ways copied text may travel: to the host, from the host.
    clip: (bool, bool),
    /// The connection's limit on [`Self::clip`].
    clip_limit: (bool, bool),
    /// The last text the desktop copied here, and the last sent to it
    /// (line ends as `\n`): neither is sent to it again.
    clip_in: Option<String>,
    clip_out: Option<String>,
    /// The picture just came up: this PC's clipboard may go to it now.
    fresh: bool,
    /// Keeps the viewer process alive; dropping it ends the process.
    _process: Box<dyn Send>,
}

impl std::fmt::Debug for Desktop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Desktop")
            .field("phase", &self.phase)
            .field("size", &(self.fb.width(), self.fb.height()))
            .finish_non_exhaustive()
    }
}

/// Wheel up and down, as VNC buttons 4 and 5 (the RDP viewer turns them
/// into wheel turns).
const WHEEL_UP: u8 = 1 << 3;
const WHEEL_DOWN: u8 = 1 << 4;

impl Desktop {
    /// Connects to the tunnel at `addr` and starts `session` in the
    /// viewer process behind `to_viewer` and `from_viewer`. `notify`
    /// wakes the window when something arrives.
    pub fn start(
        addr: &str,
        session: Session,
        to_viewer: impl Write + Send + 'static,
        from_viewer: impl Read + Send + 'static,
        process: Box<dyn Send>,
        notify: impl Fn() + Send + Clone + 'static,
    ) -> std::io::Result<Self> {
        let tunnel = TcpStream::connect(addr)?;
        let _ = tunnel.set_nodelay(true);
        let (tx, queue) = mpsc::channel::<ToViewer>();
        let (events, from) = mpsc::channel();
        let _ = tx.send(ToViewer::Start {
            protocol: session.protocol,
            width: session.width,
            height: session.height,
            layout: session.layout,
            user: session.user.clone(),
        });
        let (to_host, from_host) = session.clipboard;
        let _ = tx.send(ToViewer::ClipboardRules { to_host, from_host });

        // Window and tunnel to viewer.
        let ended = events.clone();
        let wake = notify.clone();
        let mut pipe = to_viewer;
        let budget = Arc::new(Budget::default());
        let written = Arc::clone(&budget);
        std::thread::Builder::new()
            .name("desktop-to-viewer".into())
            .spawn(move || {
                let _closer = CloseOnDrop(Arc::clone(&written));
                for msg in queue {
                    let last = msg == ToViewer::Closed;
                    let net = match &msg {
                        ToViewer::Net(b) => Some(b.len()),
                        _ => None,
                    };
                    let failed = write_frame(&mut pipe, &encode_wiped(&msg)).is_err();
                    if let Some(n) = net {
                        written.give(n);
                    }
                    if failed {
                        let _ =
                            ended.send(FromThreads::Ended("The desktop viewer stopped.".into()));
                        wake();
                        return;
                    }
                    if last {
                        return;
                    }
                }
            })?;

        // Tunnel to viewer.
        let mut net = tunnel.try_clone()?;
        let to = tx.clone();
        std::thread::Builder::new()
            .name("desktop-from-net".into())
            .spawn(move || {
                let mut buf = vec![0u8; MAX_NET];
                loop {
                    match net.read(&mut buf) {
                        Ok(0) | Err(_) => {
                            let _ = to.send(ToViewer::Closed);
                            return;
                        }
                        Ok(n) => {
                            // Waits while the viewer is behind.
                            if !budget.take(n) {
                                return;
                            }
                            let bytes = buf.get(..n).unwrap_or_default().to_vec();
                            if to.send(ToViewer::Net(bytes)).is_err() {
                                return;
                            }
                        }
                    }
                }
            })?;

        // Viewer to tunnel and window.
        let mut net = tunnel.try_clone()?;
        let mut pipe = from_viewer;
        std::thread::Builder::new()
            .name("desktop-from-viewer".into())
            .spawn(move || {
                let why = loop {
                    let frame = match read_frame(&mut pipe) {
                        Ok(Some(f)) => f,
                        Ok(None) => break "The desktop viewer stopped.".to_owned(),
                        Err(_) => break "The desktop viewer sent a bad frame.".to_owned(),
                    };
                    let Some(msg) = FromViewer::decode(&frame) else {
                        break "The desktop viewer sent a bad message.".to_owned();
                    };
                    match msg {
                        FromViewer::Net(bytes) => {
                            if net.write_all(&bytes).is_err() {
                                break "The tunnel to the remote desktop closed.".to_owned();
                            }
                        }
                        FromViewer::Failed(why) => break why,
                        other => {
                            if events.send(FromThreads::Viewer(other)).is_err() {
                                return;
                            }
                            notify();
                        }
                    }
                };
                let _ = net.shutdown(Shutdown::Both);
                let _ = events.send(FromThreads::Ended(why));
                notify();
            })?;

        Ok(Self {
            protocol: session.protocol,
            user: session.user,
            preset: session.password,
            fixed: session.fixed,
            to_viewer: tx,
            from,
            tunnel,
            fb: Framebuffer::default(),
            dirty: None,
            phase: Phase::Connecting,
            buttons: 0,
            size: (session.width, session.height),
            clip: session.clipboard,
            clip_limit: session.clipboard_limit,
            clip_in: None,
            clip_out: None,
            fresh: false,
            _process: process,
        })
    }

    fn send(&self, msg: ToViewer) {
        let _ = self.to_viewer.send(msg);
    }

    fn end(&mut self, why: String, out: &mut Vec<Update>) {
        if !matches!(self.phase, Phase::Ended) {
            self.phase = Phase::Ended;
            let _ = self.tunnel.shutdown(Shutdown::Both);
            self.send(ToViewer::Closed);
            out.push(Update::Ended(why));
        }
    }

    /// Takes what arrived and applies it to the picture.
    pub fn poll(&mut self) -> Vec<Update> {
        let mut out = Vec::new();
        let mut drawn = false;
        while let Ok(item) = self.from.try_recv() {
            if matches!(self.phase, Phase::Ended) {
                continue;
            }
            let msg = match item {
                FromThreads::Viewer(m) => m,
                FromThreads::Ended(why) => {
                    self.end(why, &mut out);
                    continue;
                }
            };
            match self.fb.apply(msg) {
                Ok(Applied::Drawn) => {
                    if matches!(self.phase, Phase::Connecting) {
                        self.phase = Phase::Running;
                        self.fresh = true;
                    }
                    drawn = true;
                }
                // A taken-over viewer must not spoof the tab with bidi tricks.
                Ok(Applied::Other(FromViewer::Name(n))) => {
                    out.push(Update::Title(ferossh_term::sanitize_title(&n)));
                }
                Ok(Applied::Other(FromViewer::Bell)) => out.push(Update::Bell),
                // FeroSSH started this server with a password: one that
                // now lets anyone in was swapped, or had the password
                // option taken out of its offer on the way.
                Ok(Applied::Other(FromViewer::NoPassword)) if self.preset.is_some() => self.end(
                    "This VNC server asked for no password, though FeroSSH started it with \
                     one. Something between may have removed the password from its offer, \
                     so FeroSSH did not open the desktop."
                        .into(),
                    &mut out,
                ),
                Ok(Applied::Other(FromViewer::NoPassword)) => out.push(Update::Note(
                    "This VNC server asks for no password: anyone who can reach its port \
                     can see and control this desktop."
                        .into(),
                )),
                // The window decides, whatever the viewer does.
                Ok(Applied::Other(FromViewer::Clipboard(text))) if self.clip.1 => {
                    let plain = text.replace("\r\n", "\n");
                    self.clip_out = None;
                    self.clip_in = Some(plain);
                    out.push(Update::Clipboard(text));
                }
                Ok(Applied::Other(FromViewer::NeedPassword)) => match self.protocol {
                    Protocol::Vnc if self.preset.is_some() => {
                        if let Some(pw) = self.preset.take() {
                            self.send(ToViewer::Password(Secret::new(pw.as_bytes().to_vec())));
                        }
                    }
                    Protocol::Vnc => {
                        self.phase = Phase::Password(password_buffer());
                        out.push(Update::Ask("VNC password: ".into()));
                    }
                    Protocol::Rdp => {
                        self.phase = Phase::User(String::new());
                        out.push(Update::Ask(self.user_prompt()));
                    }
                },
                Ok(Applied::Other(_)) => {}
                Err(why) => self.end(why, &mut out),
            }
        }
        if drawn {
            if let Some((l, t, r, b)) = self.fb.take_dirty() {
                let d = (u32::from(l), u32::from(t), u32::from(r), u32::from(b));
                self.dirty = Some(match self.dirty {
                    None => d,
                    Some(o) => (o.0.min(d.0), o.1.min(d.1), o.2.max(d.2), o.3.max(d.3)),
                });
            }
            out.push(Update::Redraw);
        }
        out
    }

    /// Whether the picture is up: input goes to the desktop.
    pub fn running(&self) -> bool {
        matches!(self.phase, Phase::Running) && self.fb.width() > 0
    }

    /// Whether typing goes to a prompt.
    pub fn asking(&self) -> bool {
        matches!(self.phase, Phase::User(_) | Phase::Password(_))
    }

    fn user_prompt(&self) -> String {
        if self.user.is_empty() {
            "User name: ".into()
        } else {
            format!("User name [{}]: ", self.user)
        }
    }

    /// The picture for the renderer, `key` naming it.
    pub fn image(&self, key: u64) -> Option<Image<'_>> {
        self.running().then(|| Image {
            key,
            width: u32::from(self.fb.width()),
            height: u32::from(self.fb.height()),
            pixels: self.fb.pixels(),
            dirty: self.dirty,
        })
    }

    /// The renderer drew the picture.
    pub fn drawn(&mut self) {
        self.dirty = None;
    }

    /// A key went down or up; `true` when the desktop took the whole key,
    /// so its character (`WM_CHAR`) must not follow. RDP sends every key
    /// as a scan code and the server's layout types it; VNC sends keys
    /// that type nothing here and the rest by character.
    pub fn key(&mut self, vk: u16, extended: bool, scan: u16, down: bool) -> bool {
        if !self.running() {
            return false;
        }
        self.send(ToViewer::Key {
            vk,
            scan,
            extended,
            down,
        });
        match self.protocol {
            Protocol::Rdp => true,
            Protocol::Vnc => keysym::special(vk, extended, scan).is_some(),
        }
    }

    /// A typed character. While a prompt is up it goes there; the result
    /// is what to echo in the pane.
    pub fn char(&mut self, c: char) -> Option<String> {
        let enter = matches!(c, '\r' | '\n');
        match &mut self.phase {
            Phase::User(typed) => match c {
                _ if enter => {
                    let user = if typed.is_empty() {
                        self.user.clone()
                    } else {
                        typed.clone()
                    };
                    self.send(ToViewer::User(user));
                    self.phase = Phase::Password(password_buffer());
                    Some("\r\nPassword: ".into())
                }
                '\u{8}' => typed.pop().map(|_| "\u{8} \u{8}".into()),
                c if !c.is_control() => {
                    typed.push(c);
                    Some(c.to_string())
                }
                _ => None,
            },
            Phase::Password(typed) => {
                match c {
                    _ if enter => {
                        let password = Secret::new(typed.as_bytes().to_vec());
                        self.phase = Phase::Connecting;
                        self.send(ToViewer::Password(password));
                        return Some("\r\n".into());
                    }
                    '\u{8}' => {
                        typed.pop();
                    }
                    c if !c.is_control() => type_into(typed, c),
                    _ => {}
                }
                None
            }
            _ => {
                if self.running() && self.protocol == Protocol::Vnc {
                    self.send(ToViewer::Char(c));
                }
                None
            }
        }
    }

    /// The remote screen point under (`x`, `y`) when the picture fills
    /// `area` (left, top, right, bottom, in window pixels).
    fn point(&self, area: (i32, i32, i32, i32), x: i32, y: i32) -> (u16, u16) {
        let (w, h) = (u32::from(self.fb.width()), u32::from(self.fb.height()));
        let (l, t, r, b) = fit(area, w, h);
        let map = |v: i32, lo: i32, hi: i32, size: u32| -> u16 {
            if hi <= lo || size == 0 {
                return 0;
            }
            let rel = i64::from((v - lo).clamp(0, hi - lo - 1));
            let scaled = rel * i64::from(size) / i64::from(hi - lo);
            u16::try_from(scaled).unwrap_or(u16::MAX)
        };
        (map(x, l, r, w), map(y, t, b, h))
    }

    /// The mouse moved, or a button (bit 0 left, 1 middle, 2 right) went
    /// down or up, over a picture filling `area`.
    pub fn mouse(
        &mut self,
        area: (i32, i32, i32, i32),
        x: i32,
        y: i32,
        buttons: Option<(u8, bool)>,
    ) {
        if !self.running() {
            return;
        }
        if let Some((bit, down)) = buttons {
            if down {
                self.buttons |= bit;
            } else {
                self.buttons &= !bit;
            }
        }
        let (px, py) = self.point(area, x, y);
        self.send(ToViewer::Pointer {
            buttons: self.buttons,
            x: px,
            y: py,
        });
    }

    /// The wheel turned by `notches` (positive: away from the user).
    pub fn wheel(&mut self, area: (i32, i32, i32, i32), x: i32, y: i32, notches: i32) {
        if !self.running() {
            return;
        }
        let (px, py) = self.point(area, x, y);
        let bit = if notches > 0 { WHEEL_UP } else { WHEEL_DOWN };
        for _ in 0..notches.unsigned_abs().min(10) {
            for buttons in [self.buttons | bit, self.buttons] {
                self.send(ToViewer::Pointer {
                    buttons,
                    x: px,
                    y: py,
                });
            }
        }
    }

    /// The pane is now `width` x `height` pixels: a server that can gives
    /// the desktop that size; the picture of one that cannot is scaled.
    pub fn resize(&mut self, width: u16, height: u16) {
        if !self.fixed && (width, height) != self.size && !matches!(self.phase, Phase::Ended) {
            self.size = (width, height);
            self.send(ToViewer::Resize { width, height });
        }
    }

    /// The connection's limit on the clipboard.
    pub fn clipboard_limit(&self) -> (bool, bool) {
        self.clip_limit
    }

    /// Whether this PC's clipboard text may go to the desktop.
    pub fn takes_clipboard(&self) -> bool {
        self.clip.0 && self.running()
    }

    /// New clipboard rules (to the host, from the host), at once.
    pub fn set_clipboard(&mut self, to_host: bool, from_host: bool) {
        if (to_host, from_host) == self.clip || matches!(self.phase, Phase::Ended) {
            return;
        }
        self.clip = (to_host, from_host);
        // Turned back on: the clipboard goes again.
        self.clip_out = None;
        self.send(ToViewer::ClipboardRules { to_host, from_host });
    }

    /// Offers this PC's clipboard text to the desktop, unless it is what
    /// the desktop itself copied or was already given.
    pub fn offer_clipboard(&mut self, text: &str) {
        if !self.takes_clipboard() {
            return;
        }
        let Some(text) = ferossh_viewer::pipe::clip_text(text) else {
            return;
        };
        let plain = text.replace("\r\n", "\n");
        if self.clip_in.as_ref() == Some(&plain) || self.clip_out.as_ref() == Some(&plain) {
            return;
        }
        self.clip_out = Some(plain);
        self.send(ToViewer::Clipboard(text));
    }

    /// Whether the picture just came up (once).
    pub fn take_fresh(&mut self) -> bool {
        std::mem::take(&mut self.fresh)
    }

    /// The pane lost the keyboard: let go of held modifiers.
    pub fn focus_lost(&mut self) {
        if self.running() {
            self.send(ToViewer::Release);
        }
    }
}

impl Drop for Desktop {
    fn drop(&mut self) {
        let _ = self.tunnel.shutdown(Shutdown::Both);
        self.send(ToViewer::Closed);
    }
}

#[cfg(all(test, feature = "vnc-viewer"))]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    //! A real VNC exchange over TCP: a small RFB server here, the viewer
    //! loop on a thread behind OS pipes as the sandboxed process would be,
    //! and the window's side in between.

    use super::*;
    use std::net::TcpListener;
    use std::time::{Duration, Instant};

    /// Serves one client: 3.8, VNC password "pw", a 3x2 screen, one Raw
    /// update; then reads a key event and a pointer event and reports them.
    fn server() -> (String, std::thread::JoinHandle<Vec<u8>>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        let t = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
            let mut out = s.try_clone().unwrap();
            let mut read = |n: usize| {
                let mut b = vec![0u8; n];
                s.read_exact(&mut b).unwrap();
                b
            };
            out.write_all(b"RFB 003.008\n").unwrap();
            assert_eq!(read(12), b"RFB 003.008\n");
            out.write_all(&[1, 2]).unwrap();
            assert_eq!(read(1), [2]);
            let challenge = [3u8; 16];
            out.write_all(&challenge).unwrap();
            assert_eq!(
                read(16),
                ferossh_viewer::vnc::vnc_response(b"pw", &challenge)
            );
            out.write_all(&0u32.to_be_bytes()).unwrap();
            assert_eq!(read(1), [1]);
            let mut init = vec![0, 3, 0, 2];
            init.extend([32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0]);
            init.extend(4u32.to_be_bytes());
            init.extend(b"desk");
            out.write_all(&init).unwrap();
            // SetPixelFormat 20, SetEncodings 4 + 7*4, request 10.
            read(20 + 4 + 28 + 10);
            let mut update = vec![0, 0, 0, 1, 0, 0, 0, 0, 0, 3, 0, 2, 0, 0, 0, 0];
            for p in 0..6u8 {
                update.extend([p, 0, 0, 0]);
            }
            out.write_all(&update).unwrap();
            read(10);
            // Key down and up, then a pointer event.
            let mut got = read(8 + 8);
            got.extend(read(6));
            got
        });
        (addr, t)
    }

    #[test]
    fn a_session_through_the_viewer() {
        let (addr, srv) = server();
        let (viewer_in, window_out) = std::io::pipe().unwrap();
        let (window_in, viewer_out) = std::io::pipe().unwrap();
        std::thread::spawn(move || ferossh_viewer::viewer::run(viewer_in, viewer_out));
        let session = Session {
            protocol: Protocol::Vnc,
            width: 0,
            height: 0,
            layout: 0,
            user: String::new(),
            password: None,
            fixed: false,
            clipboard: (true, true),
            clipboard_limit: (true, true),
        };
        let mut d =
            Desktop::start(&addr, session, window_out, window_in, Box::new(()), || {}).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut seen = Vec::new();
        while !d.running() {
            assert!(Instant::now() < deadline, "{seen:?}");
            for u in d.poll() {
                if u == Update::Ask("VNC password: ".into()) {
                    assert!(d.asking());
                    for c in "pX\u{8}w\r".chars() {
                        d.char(c);
                    }
                }
                seen.push(u);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(seen.contains(&Update::Title("desk".into())));
        let img = d.image(7).unwrap();
        assert_eq!((img.width, img.height), (3, 2));
        assert_eq!(img.pixels, [0, 1, 2, 3, 4, 5]);
        assert_eq!(img.dirty, Some((0, 0, 3, 2)));
        d.drawn();
        assert_eq!(d.image(7).unwrap().dirty, None);
        // Letters come as characters; Enter as a key. The 3x2 screen
        // fills a 300x200 area.
        assert!(!d.key(0x41, false, 0x1e, true));
        d.char('a');
        d.mouse((0, 0, 300, 200), 149, 100, Some((1, true)));
        let got = srv.join().unwrap();
        assert_eq!(&got[..8], [4, 1, 0, 0, 0, 0, 0, 0x61]);
        assert_eq!(&got[8..16], [4, 0, 0, 0, 0, 0, 0, 0x61]);
        assert_eq!(&got[16..], [5, 1, 0, 1, 0, 1]);
        // The server is gone: the session ends with a reason.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            assert!(Instant::now() < deadline);
            if d.poll().iter().any(|u| matches!(u, Update::Ended(_))) {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!d.running());
    }

    #[test]
    fn a_typed_password_never_moves() {
        let mut typed = password_buffer();
        let start = typed.as_ptr();
        for c in std::iter::repeat_n('é', 200) {
            type_into(&mut typed, c);
        }
        assert_eq!(typed.len(), MAX_TYPED_PASSWORD);
        assert_eq!(typed.as_ptr(), start);
        assert_eq!(typed.capacity(), MAX_TYPED_PASSWORD);
    }

    /// A server FeroSSH started with a password that then offers none.
    #[test]
    fn a_chosen_password_refuses_a_server_without_one() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        let srv = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
            s.write_all(b"RFB 003.008\n").unwrap();
            let mut b = [0u8; 12];
            s.read_exact(&mut b).unwrap();
            assert_eq!(&b, b"RFB 003.008\n");
            s.write_all(&[1, 1]).unwrap();
            let mut chose = [0u8; 1];
            s.read_exact(&mut chose).unwrap();
            assert_eq!(chose, [1]);
            // SecurityResult OK, then wait for the client to go.
            s.write_all(&0u32.to_be_bytes()).unwrap();
            let mut rest = Vec::new();
            let _ = s.read_to_end(&mut rest);
        });
        let (viewer_in, window_out) = std::io::pipe().unwrap();
        let (window_in, viewer_out) = std::io::pipe().unwrap();
        std::thread::spawn(move || ferossh_viewer::viewer::run(viewer_in, viewer_out));
        let session = Session {
            protocol: Protocol::Vnc,
            width: 0,
            height: 0,
            layout: 0,
            user: String::new(),
            password: Some(Zeroizing::new("pw".into())),
            fixed: true,
            clipboard: (false, false),
            clipboard_limit: (true, true),
        };
        let mut d =
            Desktop::start(&addr, session, window_out, window_in, Box::new(()), || {}).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let why = 'ended: loop {
            assert!(Instant::now() < deadline);
            for u in d.poll() {
                if let Update::Ended(why) = u {
                    break 'ended why;
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        assert!(why.contains("no password"), "{why}");
        assert!(!d.running());
        srv.join().unwrap();
    }

    /// A virtual desktop's password goes to the server without a prompt.
    #[test]
    fn a_chosen_password_needs_no_prompt() {
        let (addr, srv) = server();
        let (viewer_in, window_out) = std::io::pipe().unwrap();
        let (window_in, viewer_out) = std::io::pipe().unwrap();
        std::thread::spawn(move || ferossh_viewer::viewer::run(viewer_in, viewer_out));
        let session = Session {
            protocol: Protocol::Vnc,
            width: 0,
            height: 0,
            layout: 0,
            user: String::new(),
            password: Some(Zeroizing::new("pw".into())),
            fixed: true,
            clipboard: (false, false),
            clipboard_limit: (true, true),
        };
        let mut d =
            Desktop::start(&addr, session, window_out, window_in, Box::new(()), || {}).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !d.running() {
            assert!(Instant::now() < deadline);
            for u in d.poll() {
                assert!(!matches!(u, Update::Ask(_) | Update::Ended(_)), "{u:?}");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!d.asking());
        d.char('a');
        d.mouse((0, 0, 300, 200), 149, 100, Some((1, true)));
        let got = srv.join().unwrap();
        assert_eq!(&got[16..], [5, 1, 0, 1, 0, 1]);
    }

    /// Copied text both ways, without echoes: the server's copy reaches
    /// this PC, this PC's copy reaches the server, and the server's own
    /// text is not sent back to it.
    #[test]
    fn clipboard_text_both_ways() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        let srv = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
            let mut out = s.try_clone().unwrap();
            let mut read = |n: usize| {
                let mut b = vec![0u8; n];
                s.read_exact(&mut b).unwrap();
                b
            };
            out.write_all(b"RFB 003.008\n").unwrap();
            read(12);
            out.write_all(&[1, 1]).unwrap();
            assert_eq!(read(1), [1]);
            out.write_all(&0u32.to_be_bytes()).unwrap();
            read(1);
            let mut init = vec![0, 1, 0, 1];
            init.extend([32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0]);
            init.extend(0u32.to_be_bytes());
            out.write_all(&init).unwrap();
            read(20 + 4 + 28 + 10);
            let mut update = vec![0, 0, 0, 1, 0, 0, 0, 0, 0, 1, 0, 1, 0, 0, 0, 0];
            update.extend([1, 2, 3, 0]);
            out.write_all(&update).unwrap();
            read(10);
            out.write_all(&[3, 0, 0, 0, 0, 0, 0, 4, b'h', b'i', b'\n', 0xe9])
                .unwrap();
            let head = read(8);
            assert_eq!(&head[..4], [6, 0, 0, 0]);
            let len = u32::from_be_bytes(head[4..8].try_into().unwrap()) as usize;
            read(len)
        });
        let (viewer_in, window_out) = std::io::pipe().unwrap();
        let (window_in, viewer_out) = std::io::pipe().unwrap();
        std::thread::spawn(move || ferossh_viewer::viewer::run(viewer_in, viewer_out));
        let session = Session {
            protocol: Protocol::Vnc,
            width: 0,
            height: 0,
            layout: 0,
            user: String::new(),
            password: None,
            fixed: true,
            clipboard: (true, true),
            clipboard_limit: (true, true),
        };
        let mut d =
            Desktop::start(&addr, session, window_out, window_in, Box::new(()), || {}).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut copied = None;
        while copied.is_none() {
            assert!(Instant::now() < deadline);
            for u in d.poll() {
                assert!(!matches!(u, Update::Ended(_)), "{u:?}");
                if let Update::Clipboard(t) = u {
                    copied = Some(t);
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(copied.as_deref(), Some("hi\n\u{e9}"));
        assert!(d.take_fresh(), "the picture came up");
        assert!(!d.take_fresh());
        // Its own copy, as Windows has it, does not go back; ours does.
        d.offer_clipboard("hi\r\n\u{e9}");
        d.offer_clipboard("from the PC");
        d.offer_clipboard("from the PC");
        assert_eq!(srv.join().unwrap(), b"from the PC");
    }
}

#[cfg(test)]
mod budget_tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use std::time::Duration;

    #[test]
    fn a_slow_viewer_holds_the_tunnel_back() {
        let budget = Arc::new(Budget::default());
        // Three full reads fit, a fourth does not.
        for _ in 0..3 {
            assert!(budget.take(MAX_NET));
        }
        let reader = Arc::clone(&budget);
        let (done, waited) = mpsc::channel();
        let t = std::thread::spawn(move || {
            let ok = reader.take(MAX_NET);
            done.send(ok).unwrap();
        });
        // The reader waits while nothing is written to the viewer.
        assert!(waited.recv_timeout(Duration::from_millis(200)).is_err());
        budget.give(MAX_NET);
        assert_eq!(waited.recv_timeout(Duration::from_secs(5)), Ok(true));
        t.join().unwrap();
        assert_eq!(budget.state.lock().unwrap().0, 3 * cost(MAX_NET));
    }

    #[test]
    fn tiny_reads_are_held_back_too() {
        let budget = Budget::default();
        let mut queued = 0;
        while queued < 1_000_000 {
            let full = budget.state.lock().unwrap().0 + cost(1) > MAX_QUEUED_NET;
            if full {
                break;
            }
            assert!(budget.take(1));
            queued += 1;
        }
        // One-byte messages stop at a few thousand, not millions.
        assert_eq!(queued, MAX_QUEUED_NET / cost(1));
        assert!(budget.state.lock().unwrap().0 <= MAX_QUEUED_NET);
        for _ in 0..queued {
            budget.give(1);
        }
        assert_eq!(budget.state.lock().unwrap().0, 0);
    }

    #[test]
    fn a_stopped_viewer_stops_the_reader() {
        let budget = Arc::new(Budget::default());
        for _ in 0..3 {
            assert!(budget.take(MAX_NET));
        }
        let reader = Arc::clone(&budget);
        let t = std::thread::spawn(move || reader.take(MAX_NET));
        std::thread::sleep(Duration::from_millis(50));
        drop(CloseOnDrop(Arc::clone(&budget)));
        assert!(!t.join().unwrap());
        assert!(!budget.take(1));
    }

    #[test]
    fn no_message_is_larger_than_max_net() {
        let budget = Budget::default();
        assert!(!budget.take(MAX_NET + 1));
        assert!(budget.take(MAX_NET));
        assert_eq!(budget.state.lock().unwrap().0, cost(MAX_NET));
    }
}
