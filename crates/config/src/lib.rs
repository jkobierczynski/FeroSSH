//! ssh_config parser, known_hosts and PuTTY import.
//!
//! The parser arrives in milestone 4; see the architecture plan. Until then
//! this crate holds the pieces session profiles build on: OpenSSH host
//! patterns and per-host colour themes.

mod host_theme;
mod pattern;

pub use host_theme::{HostThemes, ThemeRule};
pub use pattern::{match_host, match_pattern};
