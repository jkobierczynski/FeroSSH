//! The built-in desktop viewer's protocol code: a VNC (RFB, RFC 6143)
//! client, and the pipe protocol between FeroSSH's window and the
//! sandboxed viewer process.
//!
//! Only FeroSSH built with the `desktop-viewer` feature uses this crate;
//! the standard exe has none of it.
//!
//! The pieces, from the network inwards:
//!
//! - [`rfb::Client`] reads what the VNC server sends and decodes it
//!   (Raw, CopyRect, RRE, Hextile and ZRLE rectangles) into pixels. It
//!   does no I/O. It runs in the viewer process, an AppContainer without
//!   network, file or registry access, so a bug in a decoder reaches
//!   nothing worth having.
//! - [`viewer::run`] is that process: it reads the server's bytes from
//!   the window over a pipe, and sends back decoded rectangles and the
//!   bytes for the server, framed as [`pipe`] describes.
//! - [`framebuffer::Framebuffer`] is the window's copy of the remote
//!   screen. It trusts nothing the viewer says: every rectangle is checked
//!   against the screen before it is drawn.
//! - [`keysym`] turns Windows keys into the X keysyms VNC sends.
//!
//! The window only ever connects the viewer to the server through an SSH
//! tunnel; VNC's own security (none, or the DES "VNC password") is no
//! protection on an open network.

pub mod framebuffer;
pub mod keysym;
pub mod pipe;
pub mod rfb;
pub mod viewer;
mod zrle;

/// The largest remote screen side, in pixels.
pub const MAX_SIDE: u16 = 8192;
/// The most pixels a remote screen may have: an 8K screen.
pub const MAX_PIXELS: usize = 7680 * 4320;

/// A pixel from its low three bytes, blue first: `0x00RRGGBB`.
pub(crate) fn rgb(bytes: &[u8]) -> u32 {
    match bytes {
        [b, g, r, ..] => u32::from_le_bytes([*b, *g, *r, 0]),
        _ => 0,
    }
}
