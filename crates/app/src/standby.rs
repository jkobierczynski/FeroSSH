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

/// An open window of some program that the standby hotkey can show and
/// hide.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// The window handle, as a number.
    pub hwnd: isize,
    pub title: String,
    /// The program's file name, such as `notepad.exe`.
    pub exe: String,
}

/// The most windows Window to show and hide lists.
pub const MAX_CANDIDATES: usize = 30;

/// The file name at the end of a program's path.
pub fn exe_name(path: &str) -> String {
    path.rsplit(['\\', '/']).next().unwrap_or(path).to_owned()
}

/// A window's menu item: its title (shortened), then its program at the
/// right. A `&` is doubled so that it shows instead of underlining.
pub fn menu_label(c: &Candidate) -> String {
    const MAX: usize = 50;
    let mut title: String = c.title.chars().take(MAX).collect();
    if c.title.chars().count() > MAX {
        title.push('…');
    }
    format!("{}\t{}", title.replace('&', "&&"), c.exe.replace('&', "&&"))
}

/// The window of program `exe` to use: `prefer` while it is one of its
/// windows, else the first listed (the one most recently in front).
pub fn find(candidates: &[Candidate], exe: &str, prefer: Option<isize>) -> Option<isize> {
    let of = |c: &&Candidate| c.exe.eq_ignore_ascii_case(exe);
    candidates
        .iter()
        .filter(of)
        .find(|c| Some(c.hwnd) == prefer)
        .or_else(|| candidates.iter().find(of))
        .map(|c| c.hwnd)
}

/// Window classes of the shell itself (the desktop, the taskbar), which
/// are never offered: hiding them would hide the desktop.
pub fn shell_class(class: &str) -> bool {
    [
        "Progman",
        "WorkerW",
        "Shell_TrayWnd",
        "Shell_SecondaryTrayWnd",
    ]
    .iter()
    .any(|c| c.eq_ignore_ascii_case(class))
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
    /// The other program's window the hotkey works now (the program is
    /// in the settings; this is which of its windows).
    pub target: Option<isize>,
    /// Another program's window the hotkey hid: it is shown again when
    /// standby goes off, the target changes or FeroSSH closes.
    pub hidden: Option<isize>,
    /// What Window to show and hide listed when it last opened.
    pub candidates: Vec<Candidate>,
}

impl Standby {
    pub fn new() -> Self {
        Self {
            held: None,
            taken: None,
            capture: Capture::Off,
            target: None,
            hidden: None,
            candidates: Vec::new(),
        }
    }

    /// The menu's status line; `label` names the hotkey as the keyboard
    /// shows it, `what` the window (`this window`, or a program).
    pub fn status(&self, label: &str, what: &str) -> String {
        if self.held.is_some() {
            format!("Status: on, {label} hides and shows {what}")
        } else if self.taken.is_some() {
            format!("Status: not active, {label} is taken")
        } else {
            format!("Status: off (hotkey {label}, for {what})")
        }
    }

    /// The longer explanation behind the status line.
    pub fn details(&self, label: &str, what: &str) -> String {
        if self.held.is_some() {
            format!(
                "Standby window is on.\n\n{label} hides {what} and brings it back in front. \
                 While hidden it has no taskbar button; only {label} brings it back, and \
                 FeroSSH shows it again when standby goes off or FeroSSH closes.\n\nPick \
                 the window under Window > Standby window > Window to show and hide, and turn \
                 it off with Standby hotkey on."
            )
        } else if self.taken.is_some() {
            format!(
                "Standby window is turned on, but not active: Windows did not give FeroSSH \
                 {label}, because another program or another FeroSSH window already uses \
                 it.\n\nPick another hotkey under Window > Standby window, or close what \
                 uses it and choose Standby hotkey on again."
            )
        } else {
            format!(
                "Standby window is off.\n\nTurn it on with Window > Standby window > Standby \
                 hotkey on; then {label} hides {what} and brings it back."
            )
        }
    }
}

/// The Win32 side: listing windows, finding a window's program, showing
/// and hiding another program's window.
#[cfg(windows)]
#[allow(unsafe_code)]
pub mod sys {
    use super::{Candidate, MAX_CANDIDATES, exe_name, shell_class};
    use std::ffi::c_void;
    use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM};
    use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GW_OWNER, GWL_EXSTYLE, GetClassNameW, GetWindow, GetWindowLongPtrW,
        GetWindowTextW, GetWindowThreadProcessId, IsHungAppWindow, IsWindow, IsWindowVisible,
        SW_SHOW, ShowWindowAsync, WS_EX_TOOLWINDOW,
    };
    use windows::core::{BOOL, PWSTR};

    pub fn hwnd(v: isize) -> HWND {
        HWND(v as *mut c_void)
    }

    /// The file name of the program that owns window `h`, such as
    /// `notepad.exe`.
    pub fn exe_of(h: HWND) -> Option<String> {
        let mut pid = 0u32;
        // SAFETY: a window handle (Windows checks it) and a local out-pointer.
        unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
        if pid == 0 {
            return None;
        }
        // SAFETY: the least access that reads the image name; the handle
        // is closed below.
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        // SAFETY: a process handle we opened, a local buffer and its length.
        let ok = unsafe {
            QueryFullProcessImageNameW(
                process,
                PROCESS_NAME_WIN32,
                PWSTR(buf.as_mut_ptr()),
                &mut len,
            )
        }
        .is_ok();
        // SAFETY: the handle OpenProcess gave.
        let _ = unsafe { CloseHandle(process) };
        let path = String::from_utf16_lossy(buf.get(..len as usize).filter(|_| ok)?);
        Some(exe_name(&path))
    }

    /// Whether `v` is still a window of program `exe`.
    pub fn alive_with_exe(v: isize, exe: &str) -> bool {
        let h = hwnd(v);
        // SAFETY: IsWindow takes any value.
        unsafe { IsWindow(Some(h)) }.as_bool()
            && exe_of(h).is_some_and(|e| e.eq_ignore_ascii_case(exe))
    }

    fn text(h: HWND) -> String {
        let mut buf = [0u16; 256];
        // SAFETY: a window handle and a local buffer.
        let n = unsafe { GetWindowTextW(h, &mut buf) };
        String::from_utf16_lossy(
            buf.get(..usize::try_from(n).unwrap_or(0))
                .unwrap_or_default(),
        )
    }

    fn class(h: HWND) -> String {
        let mut buf = [0u16; 128];
        // SAFETY: a window handle and a local buffer.
        let n = unsafe { GetClassNameW(h, &mut buf) };
        String::from_utf16_lossy(
            buf.get(..usize::try_from(n).unwrap_or(0))
                .unwrap_or_default(),
        )
    }

    /// Whether `h` is a window people see and switch to: visible, with a
    /// title, not owned by another window, not a tool window, not cloaked
    /// (a suspended Store app) and not the desktop or taskbar.
    fn listable(h: HWND) -> bool {
        // SAFETY: plain queries on a window handle from EnumWindows.
        unsafe {
            if !IsWindowVisible(h).as_bool()
                || GetWindow(h, GW_OWNER).is_ok_and(|o| !o.is_invalid())
            {
                return false;
            }
            let ex = GetWindowLongPtrW(h, GWL_EXSTYLE) as u32;
            if ex & WS_EX_TOOLWINDOW.0 != 0 {
                return false;
            }
            let mut cloaked = 0u32;
            let got = DwmGetWindowAttribute(
                h,
                DWMWA_CLOAKED,
                (&raw mut cloaked).cast(),
                size_of::<u32>() as u32,
            );
            if got.is_ok() && cloaked != 0 {
                return false;
            }
        }
        !text(h).trim().is_empty() && !shell_class(&class(h))
    }

    struct Walk {
        own: HWND,
        out: Vec<Candidate>,
    }

    extern "system" fn each(h: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: lparam is the Walk that `windows` passed, alive for the
        // whole EnumWindows call.
        let walk = unsafe { &mut *(lparam.0 as *mut Walk) };
        if h != walk.own
            && listable(h)
            && let Some(exe) = exe_of(h)
        {
            walk.out.push(Candidate {
                hwnd: h.0 as isize,
                title: text(h),
                exe,
            });
        }
        BOOL::from(walk.out.len() < MAX_CANDIDATES)
    }

    /// The open windows of other programs (and other FeroSSH windows),
    /// front to back, leaving out `own`.
    pub fn windows(own: HWND) -> Vec<Candidate> {
        let mut walk = Walk {
            own,
            out: Vec::new(),
        };
        // SAFETY: the callback only runs during this call, while `walk`
        // lives. EnumWindows reports an error when the callback stops it
        // early, which is not one.
        let _ = unsafe { EnumWindows(Some(each), LPARAM((&raw mut walk) as isize)) };
        walk.out
    }

    /// Shows another program's window again without waiting on it, so a
    /// hung program cannot hang FeroSSH.
    pub fn show_async(v: isize) {
        let h = hwnd(v);
        // SAFETY: Windows checks the handle; the call does not wait.
        unsafe {
            if IsWindow(Some(h)).as_bool() {
                let _ = ShowWindowAsync(h, SW_SHOW);
            }
        }
    }

    /// Whether the program behind `h` is not answering.
    pub fn hung(h: HWND) -> bool {
        // SAFETY: a plain query.
        unsafe { IsHungAppWindow(h) }.as_bool()
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
        assert!(s.status("Win+`", "this window").contains("off"));
        s.taken = Some(Hotkey::DEFAULT);
        assert!(s.status("Win+`", "this window").contains("taken"));
        assert!(
            s.details("Win+`", "this window")
                .contains("another program")
        );
        s.taken = None;
        s.held = Some(Hotkey::DEFAULT);
        assert_eq!(
            s.status("Win+`", "notepad.exe"),
            "Status: on, Win+` hides and shows notepad.exe"
        );
    }

    fn cand(hwnd: isize, title: &str, exe: &str) -> Candidate {
        Candidate {
            hwnd,
            title: title.to_owned(),
            exe: exe.to_owned(),
        }
    }

    #[test]
    fn finds_the_programs_window_keeping_the_one_in_use() {
        let list = [
            cand(1, "Inbox", "outlook.exe"),
            cand(2, "notes.txt - Notepad", "notepad.exe"),
            cand(3, "todo.txt - Notepad", "Notepad.exe"),
        ];
        assert_eq!(find(&list, "NOTEPAD.EXE", None), Some(2));
        assert_eq!(find(&list, "notepad.exe", Some(3)), Some(3));
        // A remembered window of another program does not count.
        assert_eq!(find(&list, "notepad.exe", Some(1)), Some(2));
        assert_eq!(find(&list, "calc.exe", None), None);
    }

    #[test]
    fn names_programs_and_menu_items() {
        assert_eq!(
            exe_name("C:\\Windows\\System32\\notepad.exe"),
            "notepad.exe"
        );
        assert_eq!(exe_name("notepad.exe"), "notepad.exe");
        let c = cand(1, "Tom & Jerry", "a&b.exe");
        assert_eq!(menu_label(&c), "Tom && Jerry\ta&&b.exe");
        let long = cand(1, &"x".repeat(80), "x.exe");
        assert!(menu_label(&long).starts_with(&format!("{}…", "x".repeat(50))));
        assert!(shell_class("Progman"));
        assert!(shell_class("shell_traywnd"));
        assert!(!shell_class("Notepad"));
    }
}
