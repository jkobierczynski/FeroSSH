//! Keyboard and mouse encoding, as xterm sends them.

use alacritty_terminal::term::TermMode;

/// Modifier keys held with a key or mouse event.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
}

impl Mods {
    pub const NONE: Mods = Mods {
        shift: false,
        alt: false,
        ctrl: false,
    };

    /// xterm's modifier parameter: 1 plus a bit per modifier.
    fn param(self) -> u8 {
        1 + u8::from(self.shift) + 2 * u8::from(self.alt) + 4 * u8::from(self.ctrl)
    }

    /// The bits a mouse report adds to its button code.
    fn mouse_bits(self) -> u32 {
        4 * u32::from(self.shift) + 8 * u32::from(self.alt) + 16 * u32::from(self.ctrl)
    }
}

/// Keys that do not produce text by themselves. Text arrives as characters
/// from the keyboard layout (see [`crate::Terminal::text`]), which is what
/// keeps AltGr, dead keys and IME input correct.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    Right,
    Left,
    Home,
    End,
    Insert,
    Delete,
    PageUp,
    PageDown,
    /// F1 to F12.
    F(u8),
    Backspace,
    Tab,
    Enter,
    Escape,
}

/// The bytes a key sends, or `None` for keys xterm has no sequence for.
pub(crate) fn encode_key(key: Key, mods: Mods, mode: TermMode) -> Option<Vec<u8>> {
    let m = mods.param();
    let app_cursor = mode.contains(TermMode::APP_CURSOR);
    // Cursor-style keys: ESC [ A, or ESC O A in application mode, or
    // ESC [ 1 ; m A with modifiers.
    let letter = |c: char, ss3: bool| {
        if m > 1 {
            format!("\x1b[1;{m}{c}")
        } else if ss3 {
            format!("\x1bO{c}")
        } else {
            format!("\x1b[{c}")
        }
    };
    let tilde = |n: u8| {
        if m > 1 {
            format!("\x1b[{n};{m}~")
        } else {
            format!("\x1b[{n}~")
        }
    };
    let alt_prefixed = |s: &[u8]| {
        let mut out = Vec::with_capacity(s.len() + 1);
        if mods.alt {
            out.push(0x1b);
        }
        out.extend_from_slice(s);
        out
    };
    let seq = match key {
        Key::Up => letter('A', app_cursor),
        Key::Down => letter('B', app_cursor),
        Key::Right => letter('C', app_cursor),
        Key::Left => letter('D', app_cursor),
        Key::Home => letter('H', app_cursor),
        Key::End => letter('F', app_cursor),
        Key::Insert => tilde(2),
        Key::Delete => tilde(3),
        Key::PageUp => tilde(5),
        Key::PageDown => tilde(6),
        Key::F(n @ 1..=4) => letter(char::from(b'P' + n - 1), true),
        Key::F(n @ 5..=12) => {
            const CODES: [u8; 8] = [15, 17, 18, 19, 20, 21, 23, 24];
            tilde(*CODES.get(usize::from(n - 5))?)
        }
        Key::F(_) => return None,
        Key::Backspace => return Some(alt_prefixed(if mods.ctrl { b"\x08" } else { b"\x7f" })),
        Key::Tab if mods.shift => "\x1b[Z".to_owned(),
        Key::Tab => return Some(alt_prefixed(b"\t")),
        Key::Enter if mode.contains(TermMode::LINE_FEED_NEW_LINE) => {
            return Some(alt_prefixed(b"\r\n"));
        }
        Key::Enter => return Some(alt_prefixed(b"\r")),
        Key::Escape => return Some(alt_prefixed(b"\x1b")),
    };
    Some(seq.into_bytes())
}

/// A typed character; Alt sends it with an ESC prefix.
pub(crate) fn encode_char(ch: char, alt: bool) -> Vec<u8> {
    let mut buf = [0u8; 4];
    let mut out = Vec::with_capacity(5);
    if alt {
        out.push(0x1b);
    }
    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
}

impl MouseButton {
    fn code(self) -> u32 {
        match self {
            MouseButton::Left => 0,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
            MouseButton::WheelUp => 64,
            MouseButton::WheelDown => 65,
        }
    }

    fn is_wheel(self) -> bool {
        matches!(self, MouseButton::WheelUp | MouseButton::WheelDown)
    }
}

/// What the mouse did, at a cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseEvent {
    Press(MouseButton),
    Release(MouseButton),
    /// Moved to another cell, with this button held (or none).
    Motion(Option<MouseButton>),
}

/// The report for a mouse event in the modes the host enabled, or `None`
/// when the host did not ask for this kind of event.
pub(crate) fn encode_mouse(
    mode: TermMode,
    event: MouseEvent,
    col: usize,
    row: usize,
    mods: Mods,
) -> Option<Vec<u8>> {
    if !mode.intersects(TermMode::MOUSE_MODE) {
        return None;
    }
    let (code, release) = match event {
        MouseEvent::Press(b) => (b.code(), false),
        // Wheels have no release.
        MouseEvent::Release(b) if b.is_wheel() => return None,
        MouseEvent::Release(b) => (b.code(), true),
        MouseEvent::Motion(held) => {
            let wanted = mode.contains(TermMode::MOUSE_MOTION)
                || (mode.contains(TermMode::MOUSE_DRAG) && held.is_some());
            if !wanted {
                return None;
            }
            // 3 means "no button" in a motion report.
            (32 + held.map_or(3, MouseButton::code), false)
        }
    };
    let code = code + mods.mouse_bits();
    let (x, y) = (col.saturating_add(1), row.saturating_add(1));

    if mode.contains(TermMode::SGR_MOUSE) {
        let end = if release { 'm' } else { 'M' };
        return Some(format!("\x1b[<{code};{x};{y}{end}").into_bytes());
    }

    // X10-style report: ESC [ M, then button and coordinates offset by 32.
    // A release does not say which button.
    let code = if release { 3 + mods.mouse_bits() } else { code };
    let mut out = b"\x1b[M".to_vec();
    out.push(u8::try_from(32 + code).ok()?);
    for v in [x, y] {
        let v = u32::try_from(v).ok()? + 32;
        if mode.contains(TermMode::UTF8_MOUSE) {
            // Mode 1005: coordinates as UTF-8 characters, up to 2015.
            let c = char::from_u32(v).filter(|_| v < 0x800)?;
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        } else {
            out.push(u8::try_from(v).ok()?);
        }
    }
    Some(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn key(k: Key, mods: Mods, mode: TermMode) -> String {
        String::from_utf8(encode_key(k, mods, mode).unwrap()).unwrap()
    }

    const CTRL: Mods = Mods {
        ctrl: true,
        ..Mods::NONE
    };
    const SHIFT: Mods = Mods {
        shift: true,
        ..Mods::NONE
    };
    const ALT: Mods = Mods {
        alt: true,
        ..Mods::NONE
    };

    #[test]
    fn cursor_keys_follow_application_mode() {
        let normal = TermMode::empty();
        assert_eq!(key(Key::Up, Mods::NONE, normal), "\x1b[A");
        assert_eq!(key(Key::Up, Mods::NONE, TermMode::APP_CURSOR), "\x1bOA");
        assert_eq!(key(Key::Home, Mods::NONE, normal), "\x1b[H");
        // Modifiers always use the CSI form.
        assert_eq!(key(Key::Left, CTRL, TermMode::APP_CURSOR), "\x1b[1;5D");
        let ctrl_shift = Mods {
            shift: true,
            ..CTRL
        };
        assert_eq!(key(Key::Right, ctrl_shift, normal), "\x1b[1;6C");
    }

    #[test]
    fn editing_and_function_keys() {
        let normal = TermMode::empty();
        assert_eq!(key(Key::Delete, Mods::NONE, normal), "\x1b[3~");
        assert_eq!(key(Key::PageUp, SHIFT, normal), "\x1b[5;2~");
        assert_eq!(key(Key::F(1), Mods::NONE, normal), "\x1bOP");
        assert_eq!(key(Key::F(4), ALT, normal), "\x1b[1;3S");
        assert_eq!(key(Key::F(5), Mods::NONE, normal), "\x1b[15~");
        assert_eq!(key(Key::F(12), CTRL, normal), "\x1b[24;5~");
        assert!(encode_key(Key::F(13), Mods::NONE, normal).is_none());
    }

    #[test]
    fn control_keys() {
        let normal = TermMode::empty();
        assert_eq!(key(Key::Backspace, Mods::NONE, normal), "\x7f");
        assert_eq!(key(Key::Backspace, CTRL, normal), "\x08");
        assert_eq!(key(Key::Backspace, ALT, normal), "\x1b\x7f");
        assert_eq!(key(Key::Tab, SHIFT, normal), "\x1b[Z");
        assert_eq!(key(Key::Enter, Mods::NONE, normal), "\r");
        assert_eq!(
            key(Key::Enter, Mods::NONE, TermMode::LINE_FEED_NEW_LINE),
            "\r\n"
        );
        assert_eq!(key(Key::Escape, Mods::NONE, normal), "\x1b");
    }

    #[test]
    fn characters_with_alt() {
        assert_eq!(encode_char('é', false), "é".as_bytes());
        assert_eq!(encode_char('b', true), b"\x1bb");
    }

    #[test]
    fn mouse_needs_a_mode() {
        let ev = MouseEvent::Press(MouseButton::Left);
        assert!(encode_mouse(TermMode::empty(), ev, 0, 0, Mods::NONE).is_none());
    }

    #[test]
    fn sgr_mouse_reports() {
        let mode = TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE;
        let enc =
            |ev, mods| String::from_utf8(encode_mouse(mode, ev, 9, 4, mods).unwrap()).unwrap();
        assert_eq!(
            enc(MouseEvent::Press(MouseButton::Left), Mods::NONE),
            "\x1b[<0;10;5M"
        );
        assert_eq!(
            enc(MouseEvent::Release(MouseButton::Right), Mods::NONE),
            "\x1b[<2;10;5m"
        );
        assert_eq!(
            enc(MouseEvent::Press(MouseButton::WheelDown), CTRL),
            "\x1b[<81;10;5M"
        );
        assert!(
            encode_mouse(
                mode,
                MouseEvent::Release(MouseButton::WheelUp),
                0,
                0,
                Mods::NONE
            )
            .is_none()
        );
        // Click mode alone does not report motion.
        assert!(
            encode_mouse(
                mode,
                MouseEvent::Motion(Some(MouseButton::Left)),
                0,
                0,
                Mods::NONE
            )
            .is_none()
        );
    }

    #[test]
    fn drag_and_motion_modes() {
        let drag = TermMode::MOUSE_DRAG | TermMode::SGR_MOUSE;
        let held = MouseEvent::Motion(Some(MouseButton::Left));
        let free = MouseEvent::Motion(None);
        assert_eq!(
            encode_mouse(drag, held, 0, 0, Mods::NONE).unwrap(),
            b"\x1b[<32;1;1M"
        );
        assert!(encode_mouse(drag, free, 0, 0, Mods::NONE).is_none());
        let motion = TermMode::MOUSE_MOTION | TermMode::SGR_MOUSE;
        assert_eq!(
            encode_mouse(motion, free, 0, 0, Mods::NONE).unwrap(),
            b"\x1b[<35;1;1M"
        );
    }

    #[test]
    fn x10_mouse_reports() {
        let mode = TermMode::MOUSE_REPORT_CLICK;
        let press = MouseEvent::Press(MouseButton::Middle);
        assert_eq!(
            encode_mouse(mode, press, 0, 1, SHIFT).unwrap(),
            b"\x1b[M\x25\x21\x22"
        );
        let release = MouseEvent::Release(MouseButton::Middle);
        assert_eq!(
            encode_mouse(mode, release, 0, 1, Mods::NONE).unwrap(),
            b"\x1b[M\x23\x21\x22"
        );
        // Columns past 222 do not fit in a byte.
        assert!(encode_mouse(mode, press, 300, 0, Mods::NONE).is_none());
        // Mode 1005 encodes them as UTF-8.
        let utf8 = mode | TermMode::UTF8_MOUSE;
        let out = encode_mouse(utf8, press, 300, 0, Mods::NONE).unwrap();
        assert_eq!(&out[..4], b"\x1b[M\x21");
        assert_eq!(std::str::from_utf8(&out[4..6]).unwrap(), "\u{14d}");
    }
}
