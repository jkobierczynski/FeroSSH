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

/// What Shift+Enter sends while the program has not turned on the kitty
/// keyboard protocol (with it, Shift+Enter is always `ESC [13;2u`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ShiftEnter {
    /// The same as Enter, as xterm does.
    #[default]
    Enter,
    /// `ESC CR`, as Alt+Enter: coding agents (Claude Code, Codex and
    /// others) read it as a new line in their input box, and it passes
    /// through tmux.
    EscCr,
    /// `ESC [13;2u`, the kitty keyboard protocol's Shift+Enter.
    CsiU,
}

/// How keys are sent beyond what xterm does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeyOptions {
    /// Programs may turn on the kitty keyboard protocol (`CSI > 1 u`).
    /// Its first level, "disambiguate escape codes", is what is sent:
    /// Escape, and Enter, Tab and Backspace with a modifier, and Ctrl or
    /// Alt with a character, as `CSI code ; modifiers u`.
    pub kitty: bool,
    pub shift_enter: ShiftEnter,
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

/// `CSI code u`, or `CSI code ; m u` with modifiers `m` (the kitty
/// keyboard protocol's form).
fn csi_u(code: u32, m: u8) -> Vec<u8> {
    if m > 1 {
        format!("\x1b[{code};{m}u").into_bytes()
    } else {
        format!("\x1b[{code}u").into_bytes()
    }
}

/// The bytes a key sends, or `None` for keys xterm has no sequence for.
pub(crate) fn encode_key(
    key: Key,
    mods: Mods,
    mode: TermMode,
    opts: KeyOptions,
) -> Option<Vec<u8>> {
    let m = mods.param();
    if mode.contains(TermMode::DISAMBIGUATE_ESC_CODES) {
        // The kitty keyboard protocol, level 1: Escape always, and Enter,
        // Tab and Backspace with a modifier, as CSI u; the other keys as
        // xterm sends them.
        match key {
            Key::Escape => return Some(csi_u(27, m)),
            Key::Enter if m > 1 => return Some(csi_u(13, m)),
            Key::Tab if m > 1 => return Some(csi_u(9, m)),
            Key::Backspace if m > 1 => return Some(csi_u(127, m)),
            _ => {}
        }
    } else if key == Key::Enter
        && mods
            == (Mods {
                shift: true,
                ..Mods::NONE
            })
    {
        match opts.shift_enter {
            ShiftEnter::Enter => {}
            ShiftEnter::EscCr => return Some(b"\x1b\r".to_vec()),
            ShiftEnter::CsiU => return Some(csi_u(13, m)),
        }
    }
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
        // The kitty protocol sends F3 as CSI 13 ~: CSI 1 ; m R reads as a
        // cursor position report.
        Key::F(3) if mode.contains(TermMode::DISAMBIGUATE_ESC_CODES) => tilde(13),
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

/// A typed character; Alt sends it with an ESC prefix. With the kitty
/// keyboard protocol on, a Ctrl or Alt character goes as `CSI code ; m u`
/// instead: the control codes Ctrl+letter types (and Ctrl+[ \\ ]) by
/// their key, and an Alt character by its lower case with Shift.
pub(crate) fn encode_char(ch: char, alt: bool, mode: TermMode) -> Vec<u8> {
    if mode.contains(TermMode::DISAMBIGUATE_ESC_CODES) {
        let ctrl_key = match ch {
            '\x01'..='\x1a' => Some(u32::from(ch) + 0x60),
            '\x1b'..='\x1d' => Some(u32::from(ch) + 0x40),
            _ => None,
        };
        let (code, ctrl, shift) = match ctrl_key {
            Some(code) => (Some(code), true, false),
            None if alt && !ch.is_control() => {
                let shift = ch.is_uppercase();
                let lower = if shift {
                    ch.to_lowercase().next().unwrap_or(ch)
                } else {
                    ch
                };
                (Some(u32::from(lower)), false, shift)
            }
            None => (None, false, false),
        };
        if let Some(code) = code {
            let m = Mods { shift, alt, ctrl }.param();
            return csi_u(code, m);
        }
    }
    let mut buf = [0u8; 4];
    let mut out = Vec::with_capacity(5);
    if alt {
        out.push(0x1b);
    }
    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
    out
}

/// Bytes for a Ctrl chord whose control code has no character of its
/// own on most layouts (Ctrl+Space, Ctrl+2, Ctrl+6, Ctrl+/): `legacy`, as
/// xterm sends it, or with the kitty keyboard protocol `CSI u` for `key`,
/// the key's own character.
pub(crate) fn encode_ctrl(key: char, legacy: u8, mods: Mods, mode: TermMode) -> Vec<u8> {
    if mode.contains(TermMode::DISAMBIGUATE_ESC_CODES) {
        return csi_u(u32::from(key), mods.param());
    }
    vec![legacy]
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
        String::from_utf8(encode_key(k, mods, mode, KeyOptions::default()).unwrap()).unwrap()
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
        assert!(encode_key(Key::F(13), Mods::NONE, normal, KeyOptions::default()).is_none());
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
        assert_eq!(encode_char('é', false, TermMode::empty()), "é".as_bytes());
        assert_eq!(encode_char('b', true, TermMode::empty()), b"\x1bb");
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

    #[test]
    fn shift_enter_sends_what_the_setting_says() {
        let normal = TermMode::empty();
        let with = |shift_enter| {
            let opts = KeyOptions {
                shift_enter,
                ..KeyOptions::default()
            };
            String::from_utf8(encode_key(Key::Enter, SHIFT, normal, opts).unwrap()).unwrap()
        };
        assert_eq!(with(ShiftEnter::Enter), "\r");
        assert_eq!(with(ShiftEnter::EscCr), "\x1b\r");
        assert_eq!(with(ShiftEnter::CsiU), "\x1b[13;2u");
        // Only Shift+Enter: Enter and Ctrl+Shift+Enter are as before.
        let opts = KeyOptions {
            shift_enter: ShiftEnter::CsiU,
            ..KeyOptions::default()
        };
        let plain = encode_key(Key::Enter, Mods::NONE, normal, opts).unwrap();
        assert_eq!(plain, b"\r");
        let ctrl_shift = Mods {
            shift: true,
            ..CTRL
        };
        assert_eq!(
            encode_key(Key::Enter, ctrl_shift, normal, opts).unwrap(),
            b"\r"
        );
    }

    #[test]
    fn the_kitty_protocol_disambiguates_escape_and_modified_keys() {
        let kitty = TermMode::DISAMBIGUATE_ESC_CODES;
        assert_eq!(key(Key::Escape, Mods::NONE, kitty), "\x1b[27u");
        assert_eq!(key(Key::Escape, ALT, kitty), "\x1b[27;3u");
        assert_eq!(key(Key::Enter, Mods::NONE, kitty), "\r");
        assert_eq!(key(Key::Enter, SHIFT, kitty), "\x1b[13;2u");
        assert_eq!(key(Key::Enter, ALT, kitty), "\x1b[13;3u");
        assert_eq!(key(Key::Tab, Mods::NONE, kitty), "\t");
        assert_eq!(key(Key::Tab, SHIFT, kitty), "\x1b[9;2u");
        assert_eq!(key(Key::Backspace, Mods::NONE, kitty), "\x7f");
        assert_eq!(key(Key::Backspace, CTRL, kitty), "\x1b[127;5u");
        // The other keys are as xterm sends them.
        assert_eq!(key(Key::Up, CTRL, kitty), "\x1b[1;5A");
        assert_eq!(key(Key::F(5), Mods::NONE, kitty), "\x1b[15~");
        // The Shift+Enter setting gives way to the protocol.
        let opts = KeyOptions {
            kitty: true,
            shift_enter: ShiftEnter::EscCr,
        };
        assert_eq!(
            encode_key(Key::Enter, SHIFT, kitty, opts).unwrap(),
            b"\x1b[13;2u"
        );
    }

    #[test]
    fn the_kitty_protocol_sends_ctrl_and_alt_characters_by_key() {
        let kitty = TermMode::DISAMBIGUATE_ESC_CODES;
        assert_eq!(encode_char('\x03', false, kitty), b"\x1b[99;5u");
        assert_eq!(encode_char('\x01', false, kitty), b"\x1b[97;5u");
        assert_eq!(encode_char('\x1b', false, kitty), b"\x1b[91;5u");
        assert_eq!(encode_char('\x03', true, kitty), b"\x1b[99;7u");
        assert_eq!(encode_char('b', true, kitty), b"\x1b[98;3u");
        assert_eq!(encode_char('B', true, kitty), b"\x1b[98;4u");
        assert_eq!(encode_char('é', true, kitty), "\x1b[233;3u".as_bytes());
        // Plain text stays text.
        assert_eq!(encode_char('a', false, kitty), b"a");
        assert_eq!(encode_char('A', false, kitty), b"A");
        // Without the protocol, as before.
        assert_eq!(encode_char('\x03', false, TermMode::empty()), b"\x03");
    }
}
