//! The protocol pieces FeroSSH parses itself for port and X11 forwarding
//! and for proxies: the SOCKS handshake of a dynamic forward, the client
//! side of a SOCKS 5 or HTTP `CONNECT` proxy on the way to a host, and
//! X11's display names, `.Xauthority` file and connection setup.
//!
//! None of it does I/O, so it is tested and fuzzed on any host. The
//! sandboxed worker uses [`socks`], [`proxy`] and [`x11::swap_cookie`]
//! (all read what untrusted peers send); the UI reads the display setting
//! and the user's `.Xauthority` file.

pub mod proxy;
pub mod socks;
pub mod x11;
