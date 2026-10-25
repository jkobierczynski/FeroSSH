//! Feeds arbitrary bytes to the built-in desktop viewer's parsers: the VNC
//! client (as a server would send them, in arbitrary chunks, often after a
//! valid login so the rectangle decoders get the bytes), the frames the
//! window reads from the viewer process into its framebuffer, and the
//! frames the viewer reads from the window. None may panic, and the
//! client's rectangles must always fit its screen.

#![no_main]

use ferossh_vnc::framebuffer::Framebuffer;
use ferossh_vnc::pipe::{FromViewer, ToViewer};
use ferossh_vnc::rfb::{Client, Event};
use libfuzzer_sys::fuzz_target;

/// A 3.8 login without a password to a 64x48 screen.
fn login() -> Vec<u8> {
    let mut v = b"RFB 003.008\n\x01\x01".to_vec();
    v.extend(0u32.to_be_bytes());
    v.extend(64u16.to_be_bytes());
    v.extend(48u16.to_be_bytes());
    v.extend([32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0]);
    v.extend(0u32.to_be_bytes());
    v
}

fuzz_target!(|data: &[u8]| {
    let Some((&mode, rest)) = data.split_first() else {
        return;
    };
    match mode % 3 {
        0 => {
            let mut c = Client::new();
            let mut input = if mode & 0x80 != 0 { login() } else { Vec::new() };
            input.extend_from_slice(rest);
            let chunk = usize::from(mode >> 2 & 0x1f).max(1) * 7;
            for part in input.chunks(chunk) {
                c.feed(part);
                if mode & 0x40 != 0 {
                    c.password(b"pw");
                    c.key(true, 0x61);
                    c.pointer(1, 9999, 9999);
                }
                let (w, h) = c.size();
                for e in c.take_events() {
                    if let Event::Pixels { x, y, width, height, data } = e {
                        assert!(u32::from(x) + u32::from(width) <= u32::from(w));
                        assert!(u32::from(y) + u32::from(height) <= u32::from(h));
                        assert_eq!(data.len(), usize::from(width) * usize::from(height));
                    }
                }
                c.take_output();
            }
        }
        1 => {
            let mut fb = Framebuffer::default();
            let mut r = rest;
            while let Some((&len, tail)) = r.split_first() {
                let n = usize::from(len).min(tail.len());
                let (frame, next) = tail.split_at(n);
                r = next;
                if let Some(m) = FromViewer::decode(frame) {
                    let _ = fb.apply(m);
                }
            }
            assert_eq!(fb.pixels().len(), usize::from(fb.width()) * usize::from(fb.height()));
        }
        _ => {
            if let Some(m) = ToViewer::decode(rest) {
                assert_eq!(ToViewer::decode(&m.encode()), Some(m));
            }
        }
    }
});
