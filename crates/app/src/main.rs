//! FeroSSH entry point.
//!
//! Milestone 2 opens one terminal pane on a local shell (ConPTY), so the
//! emulator, keyboard and mouse handling can be used for real before SSH
//! sessions arrive in milestone 3. `ferossh.exe <command>` runs that
//! command instead of `cmd.exe`, for example `ferossh.exe wsl` or
//! `ferossh.exe ssh host`.

#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod clipboard;
#[cfg(windows)]
mod conpty;
// Only the Windows build has a window to show it in.
#[cfg_attr(not(windows), allow(dead_code))]
mod pattern;
#[cfg(windows)]
mod win;

#[cfg(windows)]
fn main() -> windows::core::Result<()> {
    win::run()
}

#[cfg(not(windows))]
fn main() {
    eprintln!("FeroSSH runs on Windows only.");
    std::process::exit(1);
}
