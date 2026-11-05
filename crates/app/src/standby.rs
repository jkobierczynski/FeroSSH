//! Window > Standby window: a global hotkey (Windows' `RegisterHotKey`)
//! hides the FeroSSH window and brings it back. It is off unless turned
//! on. Only the window that holds the hotkey answers it; the first
//! FeroSSH window that opens with it turned on takes it.
//!
//! This is the logic the window needs that does not touch Win32: what a
//! press of the hotkey does, what a key pressed while picking a hotkey
//! means, and the status line. The Win32 calls are in `win.rs`.

use ferossh_config::hotkey::{Hotkey, is_modifier};

/// The hotkey's id for `RegisterHotKey` (an application's ids are 0 to
/// 0xBFFF); the window has only this one.
pub const HOTKEY_ID: i32 = 1;

/// The scan code of the key below Esc (left of 1), whatever the layout
/// prints on it.
pub const BELOW_ESC_SCAN: u32 = 0x29;

/// Picking a hotkey by pressing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capture {
    Off,
    /// The next key with its modifiers becomes the hotkey.
    Waiting,
    /// A hotkey was taken: the modifiers' key-ups are swallowed, so that
    /// letting go of Alt does not open the menu bar.
    Finishing,
}

/// What a press of the hotkey does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Toggle {
    Hide,
    Show,
}

/// The window hides when it is in front; otherwise (hidden, minimised or
/// behind another window) it comes to the front.
pub fn toggle(visible: bool, minimised: bool, foreground: bool) -> Toggle {
    if visible && !minimised && foreground {
        Toggle::Hide
    } else {
        Toggle::Show
    }
}

/// What a key pressed while picking a hotkey means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Picked {
    /// A modifier: wait for the key.
    Wait,
    /// Esc alone: stop picking.
    Cancel,
    Key(Hotkey),
    /// Not a hotkey FeroSSH takes, and why.
    Refused(String),
}

pub fn pick(vk: u16, ctrl: bool, alt: bool, shift: bool, win: bool) -> Picked {
    if is_modifier(vk) {
        return Picked::Wait;
    }
    const VK_ESCAPE: u16 = 0x1B;
    if vk == VK_ESCAPE && !(ctrl || alt || shift || win) {
        return Picked::Cancel;
    }
    let key = Hotkey {
        ctrl,
        alt,
        shift,
        win,
        vk,
    };
    match key.check() {
        Ok(()) => Picked::Key(key),
        Err(why) => Picked::Refused(why),
    }
}

/// The standby state of one window.
#[derive(Debug)]
pub struct Standby {
    /// The hotkey this window holds.
    pub held: Option<Hotkey>,
    /// Turned on, but Windows refused this hotkey: another program, or
    /// another FeroSSH window, has it.
    pub taken: Option<Hotkey>,
    pub capture: Capture,
}

impl Standby {
    pub fn new() -> Self {
        Self {
            held: None,
            taken: None,
            capture: Capture::Off,
        }
    }

    /// The menu's status line; `label` names the hotkey as the keyboard
    /// shows it.
    pub fn status(&self, label: &str) -> String {
        if self.held.is_some() {
            format!("Status: on, {label} hides and shows this window")
        } else if self.taken.is_some() {
            format!("Status: not active, {label} is taken")
        } else {
            format!("Status: off (hotkey {label})")
        }
    }

    /// The longer explanation behind the status line.
    pub fn details(&self, label: &str) -> String {
        if self.held.is_some() {
            format!(
                "Standby window is on.\n\n{label} hides this FeroSSH window, sessions and all, \
                 and brings it back in front. While hidden it has no taskbar button; only \
                 {label} brings it back.\n\nTurn it off with Window > Standby window > Use \
                 this window as the standby window."
            )
        } else if self.taken.is_some() {
            format!(
                "Standby window is turned on, but not active: Windows did not give FeroSSH \
                 {label}, because another program or another FeroSSH window already uses \
                 it.\n\nPick another hotkey under Window > Standby window, or close what \
                 uses it and choose Use this window as the standby window again."
            )
        } else {
            format!(
                "Standby window is off.\n\nTurn it on with Window > Standby window > Use this \
                 window as the standby window; then {label} hides this window and brings it \
                 back."
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hides_only_from_the_front() {
        assert_eq!(toggle(true, false, true), Toggle::Hide);
        assert_eq!(toggle(false, false, false), Toggle::Show);
        assert_eq!(toggle(true, true, false), Toggle::Show);
        assert_eq!(toggle(true, true, true), Toggle::Show);
        assert_eq!(toggle(true, false, false), Toggle::Show);
    }

    #[test]
    fn picking_waits_past_modifiers_and_stops_on_escape() {
        for vk in [0x10, 0x11, 0x12, 0x5B, 0xA4] {
            assert_eq!(pick(vk, true, false, false, false), Picked::Wait);
        }
        assert_eq!(pick(0x1B, false, false, false, false), Picked::Cancel);
        assert_eq!(
            pick(0xC0, false, false, false, true),
            Picked::Key(Hotkey::DEFAULT)
        );
        // Win+Esc is a hotkey, not a cancel.
        assert!(matches!(
            pick(0x1B, false, false, false, true),
            Picked::Key(_)
        ));
        assert!(matches!(
            pick(0x41, false, false, false, false),
            Picked::Refused(_)
        ));
        assert!(matches!(
            pick(0x54, true, true, false, false),
            Picked::Refused(_)
        ));
        assert!(matches!(
            pick(0x91, false, false, false, false),
            Picked::Key(_)
        ));
    }

    #[test]
    fn status_says_whether_the_hotkey_is_active() {
        let mut s = Standby::new();
        assert!(s.status("Win+`").contains("off"));
        s.taken = Some(Hotkey::DEFAULT);
        assert!(s.status("Win+`").contains("taken"));
        assert!(s.details("Win+`").contains("another program"));
        s.taken = None;
        s.held = Some(Hotkey::DEFAULT);
        assert_eq!(
            s.status("Win+`"),
            "Status: on, Win+` hides and shows this window"
        );
    }
}
