//! FeroSSH entry point.
//!
//! M0 opens an empty, DPI-aware top-level window so the release pipeline
//! can measure the size baseline of a Win32 exe with no extra crates.

#![cfg_attr(windows, windows_subsystem = "windows")]

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
