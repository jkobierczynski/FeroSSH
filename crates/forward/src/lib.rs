//! The protocol pieces of port and X11 forwarding that FeroSSH parses
//! itself: the SOCKS handshake of a dynamic forward, and X11's display
//! names, `.Xauthority` file and connection setup.
//!
//! None of it does I/O, so it is tested and fuzzed on any host. The
//! sandboxed worker uses [`socks`] and [`x11::swap_cookie`] (both read what
//! untrusted peers send); the UI reads the display setting and the user's
//! `.Xauthority` file.

pub mod socks;
pub mod x11;
