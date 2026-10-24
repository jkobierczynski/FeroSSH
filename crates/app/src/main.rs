//! FeroSSH entry point.
//!
//! Milestone 1 opens one terminal pane that shows the renderer test pattern
//! and echoes what you type, so the window, renderer and DPI handling can be
//! checked before a terminal emulator or SSH session exists.

#![cfg_attr(windows, windows_subsystem = "windows")]

// Only the Windows build has a window to show it in.
#[cfg_attr(not(windows), allow(dead_code))]
mod echo;
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
