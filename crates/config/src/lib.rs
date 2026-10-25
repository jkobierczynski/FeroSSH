//! `ssh_config` parser, known_hosts and PuTTY import.
//!
//! Everything here runs in the UI process, which owns the user's files; the
//! sandboxed worker never reads them. Per-host colour themes build on the
//! OpenSSH host patterns and are used once session profiles land.

mod host_theme;
pub mod hostkey;
pub mod known_hosts;
mod pattern;
pub mod ssh_config;

pub use host_theme::{HostThemes, ThemeRule};
pub use pattern::{match_host, match_pattern};
