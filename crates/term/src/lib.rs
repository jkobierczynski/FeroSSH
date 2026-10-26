//! VT emulation wrapper, sanitising, paste and clipboard policy.
//!
//! [`Terminal`] wraps `alacritty_terminal`, which parses the host's output
//! (through `vte`) into a grid with scrollback. This crate adds what the
//! spec's terminal-escape hardening asks for on top of it:
//!
//! - OSC 52 clipboard writes come out as [`Action::ClipboardRequest`] for
//!   the UI to confirm; clipboard reads are never answered.
//! - Window titles and OSC 8 link targets are sanitised before the UI sees
//!   them ([`sanitize_title`], [`check_link`]).
//! - Pastes are stripped of control characters and bracketed when the host
//!   asks for it ([`prepare_paste`]).
//! - Only fixed-format replies go back to the host: device attributes,
//!   status and mode reports, colours and sizes. Nothing that echoes
//!   host-chosen text (title reports, DECRQSS, clipboard reads) is
//!   answered.
//!
//! OSC 7 working-directory reports come out as
//! [`Action::WorkingDirectory`], for the file view to follow.
//!
//! [`LogWriter`] writes a tab's session log, as plain text or raw bytes.
//!
//! It also encodes keys and mouse events the way xterm does, honouring the
//! modes the host has set. Everything here is platform-neutral and tested
//! on any host.

pub mod chain;
mod cwd;
mod input;
mod log;
mod sanitize;
mod terminal;

pub use cwd::WorkingDir;
pub use input::{Key, Mods, MouseButton, MouseEvent};
pub use log::{Clock, LogWriter, Sealed, log_file_name};
pub use sanitize::{Paste, check_link, clipboard_preview, prepare_paste, sanitize_title};
pub use terminal::{Action, SelectionKind, Settings, Terminal};
