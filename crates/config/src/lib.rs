//! `ssh_config` parser, known_hosts and PuTTY import.
//!
//! Everything here runs in the UI process, which owns the user's files; the
//! sandboxed worker never reads them. Per-host colour themes build on the
//! OpenSSH host patterns and ssh_config's `Tag`; FeroSSH's own settings
//! (session profiles, favourites, theme rules) use ssh_config's syntax.

pub mod forward;
mod host_theme;
pub mod hostkey;
pub mod known_hosts;
mod pattern;
pub mod putty;
pub mod settings;
pub mod ssh_config;

pub use forward::{ForwardKind, ForwardSpec};
pub use host_theme::{HostThemes, ThemeRule};
pub use pattern::{match_host, match_pattern};
pub use settings::{Profile, Settings};
