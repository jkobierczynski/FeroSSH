//! Windows keys to X keysyms, as VNC sends them.
//!
//! Keys that type nothing (arrows, function keys, modifiers, Enter) go by
//! virtual-key code from `WM_KEYDOWN` and `WM_KEYUP`; everything that types
//! a character goes by the character from `WM_CHAR`, so the PC's keyboard
//! layout decides what a key means, as it does in the terminal. AltGr,
//! which Windows reports as Ctrl and right Alt, is let go around the
//! character it typed, or the server would see Ctrl+Alt+@ instead of @.

const SHIFT_L: u32 = 0xffe1;
const SHIFT_R: u32 = 0xffe2;
const CONTROL_L: u32 = 0xffe3;
const CONTROL_R: u32 = 0xffe4;
const ALT_L: u32 = 0xffe9;
const ALT_R: u32 = 0xffea;

/// The scan code of the right Shift key.
const RIGHT_SHIFT_SCAN: u16 = 0x36;

/// The keysym of a key that types nothing; `None` for keys `WM_CHAR`
/// covers. `extended` is bit 24 of the key message's `lParam`.
pub fn special(vk: u16, extended: bool, scan: u16) -> Option<u32> {
    Some(match vk {
        0x08 => 0xff08,             // BackSpace
        0x09 => 0xff09,             // Tab
        0x0d if extended => 0xff8d, // KP_Enter
        0x0d => 0xff0d,             // Return
        0x10 if scan == RIGHT_SHIFT_SCAN => SHIFT_R,
        0x10 => SHIFT_L,
        0x11 if extended => CONTROL_R,
        0x11 => CONTROL_L,
        0x12 if extended => ALT_R,
        0x12 => ALT_L,
        0x13 => 0xff13,                               // Pause
        0x1b => 0xff1b,                               // Escape
        0x21 => 0xff55,                               // Prior
        0x22 => 0xff56,                               // Next
        0x23 => 0xff57,                               // End
        0x24 => 0xff50,                               // Home
        0x25 => 0xff51,                               // Left
        0x26 => 0xff52,                               // Up
        0x27 => 0xff53,                               // Right
        0x28 => 0xff54,                               // Down
        0x2c => 0xff61,                               // Print
        0x2d => 0xff63,                               // Insert
        0x2e => 0xffff,                               // Delete
        0x5b => 0xffeb,                               // Super_L
        0x5c => 0xffec,                               // Super_R
        0x5d => 0xff67,                               // Menu
        0x70..=0x87 => 0xffbe + u32::from(vk - 0x70), // F1 to F24
        0xa0 => SHIFT_L,
        0xa1 => SHIFT_R,
        0xa2 => CONTROL_L,
        0xa3 => CONTROL_R,
        0xa4 => ALT_L,
        0xa5 => ALT_R,
        _ => return None,
    })
}

/// The keysym for a typed character: Latin-1 as itself, the rest as
/// Unicode keysyms. `None` for control characters.
pub fn of_char(c: char) -> Option<u32> {
    let cp = u32::from(c);
    match cp {
        0..=0x1f | 0x7f..=0x9f => None,
        0x20..=0x7e | 0xa0..=0xff => Some(cp),
        _ => Some(0x0100_0000 | cp),
    }
}

/// The keyboard's state as the server sees it: which modifiers are down.
#[derive(Debug, Default, Clone)]
pub struct Keyboard {
    held: Vec<u32>,
}

/// Key events for the server: down or up, and the keysym.
pub type Keys = Vec<(bool, u32)>;

impl Keyboard {
    /// A key that types nothing went down or up. Empty when the key
    /// types a character instead.
    pub fn key(&mut self, vk: u16, extended: bool, scan: u16, down: bool) -> Keys {
        let Some(sym) = special(vk, extended, scan) else {
            return Keys::new();
        };
        if (0xffe1..=0xffee).contains(&sym) {
            if down {
                if !self.held.contains(&sym) {
                    self.held.push(sym);
                }
            } else {
                self.held.retain(|&h| h != sym);
            }
        }
        vec![(down, sym)]
    }

    /// A character was typed (`WM_CHAR`). Ctrl with a letter arrives as
    /// a control character and goes out as the letter.
    pub fn char(&mut self, c: char) -> Keys {
        let ctrl = self
            .held
            .iter()
            .any(|h| matches!(*h, CONTROL_L | CONTROL_R));
        let alt = self.held.iter().any(|h| matches!(*h, ALT_L | ALT_R));
        let cp = u32::from(c);
        let sym = match cp {
            0x01..=0x1a if ctrl => 0x60 + cp,
            _ => match of_char(c) {
                Some(s) => s,
                None => return Keys::new(),
            },
        };
        if ctrl && alt && cp >= 0x20 {
            // AltGr: the character alone.
            let lifted: Vec<u32> = self
                .held
                .iter()
                .copied()
                .filter(|h| matches!(*h, CONTROL_L | CONTROL_R | ALT_L | ALT_R))
                .collect();
            let mut keys: Keys = lifted.iter().map(|&h| (false, h)).collect();
            keys.extend([(true, sym), (false, sym)]);
            keys.extend(lifted.iter().map(|&h| (true, h)));
            return keys;
        }
        vec![(true, sym), (false, sym)]
    }

    /// Lets go of every held modifier, when the pane loses the keyboard.
    pub fn release_all(&mut self) -> Keys {
        self.held.drain(..).map(|h| (false, h)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn special_keys() {
        assert_eq!(special(0x25, true, 0), Some(0xff51));
        assert_eq!(special(0x70, false, 0), Some(0xffbe));
        assert_eq!(special(0x7b, false, 0), Some(0xffc9), "F12");
        assert_eq!(special(0x10, false, 0x36), Some(SHIFT_R));
        assert_eq!(special(0x11, true, 0), Some(CONTROL_R));
        assert_eq!(special(0x41, false, 0), None, "letters come as characters");
    }

    #[test]
    fn characters() {
        assert_eq!(of_char('a'), Some(0x61));
        assert_eq!(of_char('é'), Some(0xe9));
        assert_eq!(of_char('€'), Some(0x0100_20ac));
        assert_eq!(of_char('\r'), None);
    }

    #[test]
    fn ctrl_letters_and_altgr() {
        let mut k = Keyboard::default();
        assert_eq!(k.key(0x11, false, 0x1d, true), [(true, CONTROL_L)]);
        assert_eq!(k.char('\u{3}'), [(true, 0x63), (false, 0x63)], "Ctrl+C");
        // AltGr+Q on a Belgian keyboard types @.
        k.key(0x12, true, 0x38, true);
        assert_eq!(
            k.char('@'),
            [
                (false, CONTROL_L),
                (false, ALT_R),
                (true, 0x40),
                (false, 0x40),
                (true, CONTROL_L),
                (true, ALT_R)
            ]
        );
        assert_eq!(k.release_all(), [(false, CONTROL_L), (false, ALT_R)]);
        assert!(k.char('\r').is_empty());
    }
}
