//! The standby window's global hotkey: a key and its modifiers, as the
//! settings file writes it (`Win+Oem3`, `Ctrl+Alt+Space`, `ScrollLock`)
//! and as Windows' `RegisterHotKey` takes it.
//!
//! Keys are Windows virtual-key codes. The punctuation keys have no fixed
//! character (`Oem3` is `` ` `` on a US keyboard and `ù` on a Belgian
//! one), so the file names them by code, and the window shows the
//! character the keyboard layout puts on them.

use std::fmt;

/// `RegisterHotKey`'s modifier flags.
pub const MOD_ALT: u32 = 0x1;
pub const MOD_CONTROL: u32 = 0x2;
pub const MOD_SHIFT: u32 = 0x4;
pub const MOD_WIN: u32 = 0x8;

/// A key with its modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hotkey {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
    /// The virtual-key code.
    pub vk: u16,
}

const VK_PAUSE: u16 = 0x13;
const VK_SCROLL: u16 = 0x91;
const VK_F1: u16 = 0x70;
const VK_F12: u16 = 0x7B;
const VK_F13: u16 = 0x7C;
const VK_F24: u16 = 0x87;
const VK_OEM_3: u16 = 0xC0;

/// Named keys: the name the file uses, then the code.
const NAMED: [(&str, u16); 37] = [
    ("Space", 0x20),
    ("Tab", 0x09),
    ("Enter", 0x0D),
    ("Esc", 0x1B),
    ("Backspace", 0x08),
    ("Insert", 0x2D),
    ("Delete", 0x2E),
    ("Home", 0x24),
    ("End", 0x23),
    ("PageUp", 0x21),
    ("PageDown", 0x22),
    ("Up", 0x26),
    ("Down", 0x28),
    ("Left", 0x25),
    ("Right", 0x27),
    ("Pause", VK_PAUSE),
    ("ScrollLock", VK_SCROLL),
    ("PrintScreen", 0x2C),
    ("NumMultiply", 0x6A),
    ("NumPlus", 0x6B),
    ("NumMinus", 0x6D),
    ("NumDecimal", 0x6E),
    ("NumDivide", 0x6F),
    ("Oem1", 0xBA),
    ("OemPlus", 0xBB),
    ("OemComma", 0xBC),
    ("OemMinus", 0xBD),
    ("OemPeriod", 0xBE),
    ("Oem2", 0xBF),
    ("Oem3", VK_OEM_3),
    ("Oem4", 0xDB),
    ("Oem5", 0xDC),
    ("Oem6", 0xDD),
    ("Oem7", 0xDE),
    ("Oem8", 0xDF),
    ("Oem102", 0xE2),
    ("Apps", 0x5D),
];

/// Other spellings the file may use: the punctuation as a US keyboard
/// has it, and a few common names.
const ALIASES: [(&str, u16); 16] = [
    ("`", VK_OEM_3),
    ("Backquote", VK_OEM_3),
    (";", 0xBA),
    ("=", 0xBB),
    (",", 0xBC),
    ("-", 0xBD),
    (".", 0xBE),
    ("/", 0xBF),
    ("[", 0xDB),
    ("\\", 0xDC),
    ("]", 0xDD),
    ("'", 0xDE),
    ("Escape", 0x1B),
    ("Return", 0x0D),
    ("PgUp", 0x21),
    ("PgDn", 0x22),
];

/// The file's name for a key code, if FeroSSH knows the key.
fn key_name(vk: u16) -> Option<String> {
    match vk {
        0x30..=0x39 | 0x41..=0x5A => Some(char::from(vk as u8).to_string()),
        VK_F1..=VK_F24 => Some(format!("F{}", vk - VK_F1 + 1)),
        0x60..=0x69 => Some(format!("Num{}", vk - 0x60)),
        _ => NAMED
            .iter()
            .find(|(_, code)| *code == vk)
            .map(|(name, _)| (*name).to_owned()),
    }
}

fn key_code(name: &str) -> Option<u16> {
    let upper = name.to_ascii_uppercase();
    let bytes = upper.as_bytes();
    if let [b] = bytes
        && b.is_ascii_alphanumeric()
    {
        return Some(u16::from(*b));
    }
    if let Some(n) = upper.strip_prefix('F').and_then(|n| n.parse::<u16>().ok())
        && (1..=24).contains(&n)
    {
        return Some(VK_F1 + n - 1);
    }
    if let Some(n) = upper
        .strip_prefix("NUM")
        .and_then(|n| n.parse::<u16>().ok())
        && n <= 9
        && upper.len() == 4
    {
        return Some(0x60 + n);
    }
    NAMED
        .iter()
        .chain(ALIASES.iter())
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, code)| *code)
}

/// Whether the key types a character: a letter, a digit or punctuation.
fn types_text(vk: u16) -> bool {
    matches!(vk, 0x30..=0x39 | 0x41..=0x5A | 0x20 | 0xBA..=0xC0 | 0xDB..=0xDF | 0xE2)
}

impl Hotkey {
    /// Win and the key left of 1 on a US keyboard, as Windows Terminal's
    /// quake mode uses.
    pub const DEFAULT: Hotkey = Hotkey {
        ctrl: false,
        alt: false,
        shift: false,
        win: true,
        vk: VK_OEM_3,
    };

    /// A hotkey as written: modifiers and a key joined by `+`, in any
    /// case, such as `Ctrl+Alt+Space`, `win+oem3` or `ScrollLock`. It is
    /// also checked (see [`Hotkey::check`]).
    pub fn parse(text: &str) -> Result<Hotkey, String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("Name a key, such as Win+Oem3 or Ctrl+Alt+Space.".to_owned());
        }
        let parts: Vec<&str> = text.split('+').map(str::trim).collect();
        let (key, mods) = parts.split_last().unwrap_or((&"", &[]));
        let mut h = Hotkey {
            ctrl: false,
            alt: false,
            shift: false,
            win: false,
            vk: 0,
        };
        for m in mods {
            let flag = match m.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => &mut h.ctrl,
                "alt" => &mut h.alt,
                "shift" => &mut h.shift,
                "win" | "windows" => &mut h.win,
                _ => return Err(format!("{m:?} is not Ctrl, Alt, Shift or Win.")),
            };
            *flag = true;
        }
        h.vk = key_code(key).ok_or_else(|| format!("FeroSSH does not know the key {key:?}."))?;
        h.check()?;
        Ok(h)
    }

    /// Whether this works as a hotkey that does not get in the way of
    /// typing: it needs Ctrl, Alt or Win, except on keys nothing else
    /// uses (F13 to F24, Pause, Scroll Lock). Ctrl+Alt with a character
    /// key is AltGr on many keyboards, so it needs Win as well.
    pub fn check(&self) -> Result<(), String> {
        let name = self.to_string();
        if key_name(self.vk).is_none() {
            return Err(format!("FeroSSH does not know key code {:#04x}.", self.vk));
        }
        let lone = matches!(self.vk, VK_F13..=VK_F24 | VK_PAUSE | VK_SCROLL);
        if !(self.ctrl || self.alt || self.win || lone) {
            return Err(format!(
                "{name} would stop that key working in every program. Add Ctrl, Alt or Win."
            ));
        }
        if self.vk == VK_F12 && !(self.ctrl || self.alt || self.win) {
            return Err("Windows keeps F12 for debuggers. Add Ctrl, Alt or Win.".to_owned());
        }
        if self.ctrl && self.alt && !self.win && types_text(self.vk) && self.vk != 0x20 {
            return Err(format!(
                "{name} is AltGr with that key on many keyboards, so it would stop you typing \
                 its character. Add Win, or pick another key."
            ));
        }
        Ok(())
    }

    /// The modifier flags for `RegisterHotKey` (without `MOD_NOREPEAT`).
    pub fn modifiers(&self) -> u32 {
        let mut m = 0;
        for (on, flag) in [
            (self.ctrl, MOD_CONTROL),
            (self.alt, MOD_ALT),
            (self.shift, MOD_SHIFT),
            (self.win, MOD_WIN),
        ] {
            if on {
                m |= flag;
            }
        }
        m
    }

    /// The hotkey for people to read: like the file's name, but a
    /// punctuation key shows as the character `oem_char` says the
    /// keyboard layout puts on it, when it says one.
    pub fn label(&self, oem_char: impl Fn(u16) -> Option<char>) -> String {
        let key = match key_name(self.vk) {
            Some(name) if name.starts_with("Oem") => match oem_char(self.vk) {
                Some(c) if !c.is_control() && !c.is_whitespace() => c.to_string(),
                _ => name,
            },
            Some(name) => name,
            None => format!("{:#04x}", self.vk),
        };
        format!("{}{key}", self.prefix())
    }

    fn prefix(&self) -> String {
        let mut out = String::new();
        for (on, name) in [
            (self.ctrl, "Ctrl+"),
            (self.alt, "Alt+"),
            (self.shift, "Shift+"),
            (self.win, "Win+"),
        ] {
            if on {
                out.push_str(name);
            }
        }
        out
    }
}

/// The settings file's spelling, such as `Win+Oem3`.
impl fmt::Display for Hotkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let key = key_name(self.vk).unwrap_or_else(|| format!("{:#04x}", self.vk));
        write!(f, "{}{key}", self.prefix())
    }
}

/// Whether a virtual key is a modifier (Shift, Ctrl, Alt or Win, either
/// side), which a hotkey being picked waits past.
pub fn is_modifier(vk: u16) -> bool {
    matches!(vk, 0x10..=0x12 | 0x5B | 0x5C | 0xA0..=0xA5)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_writes_back_the_same() {
        for text in [
            "Win+Oem3",
            "Ctrl+Alt+Space",
            "ScrollLock",
            "Pause",
            "F13",
            "Ctrl+Shift+F12",
            "Win+Num5",
            "Alt+Shift+Win+Z",
            "Ctrl+Win+Oem102",
        ] {
            let h = Hotkey::parse(text).unwrap();
            assert_eq!(h.to_string(), text);
            assert_eq!(Hotkey::parse(&h.to_string()), Ok(h));
        }
    }

    #[test]
    fn accepts_other_spellings() {
        assert_eq!(Hotkey::parse(" win + ` "), Ok(Hotkey::DEFAULT));
        assert_eq!(Hotkey::parse("windows+backquote"), Ok(Hotkey::DEFAULT));
        let h = Hotkey::parse("control+alt+shift+escape").unwrap();
        assert_eq!(h.to_string(), "Ctrl+Alt+Shift+Esc");
        assert_eq!(h.modifiers(), MOD_CONTROL | MOD_ALT | MOD_SHIFT);
        assert_eq!(Hotkey::DEFAULT.modifiers(), MOD_WIN);
    }

    #[test]
    fn refuses_keys_that_would_break_typing() {
        assert!(Hotkey::parse("").is_err());
        assert!(Hotkey::parse("A").is_err());
        assert!(Hotkey::parse("Shift+A").is_err());
        assert!(Hotkey::parse("F12").is_err());
        assert!(Hotkey::parse("F5").is_err());
        assert!(Hotkey::parse("Ctrl+Alt+T").is_err());
        assert!(Hotkey::parse("Ctrl+Alt+Oem3").is_err());
        assert!(Hotkey::parse("Hyper+T").is_err());
        assert!(Hotkey::parse("Ctrl+NoSuchKey").is_err());
        assert!(Hotkey::parse("Num10").is_err());
        assert!(Hotkey::parse("Ctrl+Alt+Win+T").is_ok());
        assert!(Hotkey::parse("Ctrl+Alt+Space").is_ok());
        assert!(Hotkey::parse("Ctrl+F12").is_ok());
    }

    #[test]
    fn labels_show_the_layout_character() {
        let us = |vk: u16| (vk == VK_OEM_3).then_some('`');
        assert_eq!(Hotkey::DEFAULT.label(us), "Win+`");
        let none = |_: u16| None;
        assert_eq!(Hotkey::DEFAULT.label(none), "Win+Oem3");
        let h = Hotkey::parse("Ctrl+Alt+Space").unwrap();
        assert_eq!(h.label(us), "Ctrl+Alt+Space");
    }

    #[test]
    fn modifiers_are_known() {
        for vk in [0x10, 0x11, 0x12, 0x5B, 0x5C, 0xA0, 0xA5] {
            assert!(is_modifier(vk));
        }
        assert!(!is_modifier(0x41));
        assert!(!is_modifier(VK_OEM_3));
    }
}
