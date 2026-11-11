//! `ssh_config` parser, known_hosts and imports from other SSH clients.
//!
//! Everything here runs in the UI process, which owns the user's files; the
//! sandboxed worker never reads them. Per-host colour themes build on the
//! OpenSSH host patterns and ssh_config's `Tag`; FeroSSH's own settings
//! (session profiles, favourites, theme rules) use ssh_config's syntax.

pub mod forward;
pub mod highlight;
mod host_theme;
pub mod hostkey;
pub mod hotkey;
pub mod import;
mod json;
pub mod known_hosts;
pub mod login;
pub mod mobaxterm;
pub mod mremoteng;
pub mod password;
pub mod pattern;
pub mod proxy;
pub mod putty;
pub mod record;
pub mod saved;
pub mod securecrt;
pub mod serial;
pub mod settings;
pub mod snippet_import;
pub mod snippet_store;
pub mod ssh_config;
pub mod trigger;
pub mod winscp;

pub use forward::{ForwardKind, ForwardSpec};
pub use highlight::{HighlightColour, HighlightRule};
pub use host_theme::{HostThemes, ThemeRule};
pub use password::PasswordSource;
pub use pattern::{match_host, match_pattern};
pub use proxy::{ProxyKind, ProxyServer};
pub use settings::{
    Bookmark, DEFAULT_TELNET_PORT, DEFAULT_WINRM_COMMAND, DEFAULT_WINRM_PORT, Profile, SessionKind,
    Settings, WindowColours,
};
