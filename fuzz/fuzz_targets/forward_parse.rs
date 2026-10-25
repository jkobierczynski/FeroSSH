//! Feeds arbitrary bytes to the forwarding parsers the worker and the UI
//! run on untrusted input: the SOCKS handshake (from local programs, in
//! arbitrary chunks), the X11 setup cookie swap (from the server) and the
//! `.Xauthority` reader. None may panic, and a swapped setup must keep
//! what followed the setup.

#![no_main]

use ferossh_forward::socks::{Handshake, Step};
use ferossh_forward::x11::{self, Display, Setup};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&split, rest)) = data.split_first() else {
        return;
    };
    let mut hs = Handshake::new();
    let mut fed = 0;
    for chunk in rest.chunks(usize::from(split).max(1)) {
        fed += chunk.len();
        match hs.feed(chunk) {
            Step::More { reply } => assert!(reply.len() <= fed + 2),
            Step::Connect { host, rest, .. } => {
                assert!(!host.is_empty() && host.len() <= 255);
                assert!(rest.len() <= fed);
                break;
            }
            Step::Refuse { .. } => break,
        }
    }

    let fake = [7u8; 16];
    if let Setup::Ready(out) = x11::swap_cookie(rest, &fake, x11::MIT_COOKIE, &[9; 16]) {
        // Same header length fields as sent, so the tail is unchanged.
        assert!(out.len() == rest.len());
    }
    let _ = x11::swap_cookie(rest, &fake, "", &[]);

    let entries = x11::xauth_entries(rest);
    if let Some(d) = Display::parse(&String::from_utf8_lossy(rest)) {
        let _ = x11::find_cookie(&entries, &d);
        let _ = d.port();
    }
});
