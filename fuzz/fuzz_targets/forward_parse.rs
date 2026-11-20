//! Feeds arbitrary bytes to the forwarding parsers the worker and the UI
//! run on untrusted input: the SOCKS handshake (from local programs, and
//! from programs on the server through a remote forward without a
//! target, in arbitrary chunks), the X11 setup cookie swap (from the
//! server), the `.Xauthority` reader, and forward specs as ssh_config
//! and imports write them (ports or Unix socket paths). None may panic, a
//! swapped setup must keep what followed the setup, every SOCKS target
//! must pass the UI's own check, and a forward spec must read back as it
//! was written.

#![no_main]

use ferossh_config::forward::{parse_cancel, parse_config, parse_option};
use ferossh_forward::socks::{self, Handshake, Step};
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
                // The UI connects only where this check passes.
                assert!(socks::valid_host(&host), "{host:?}");
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

    let text = String::from_utf8_lossy(rest);
    if let Some(spec) = parse_option(&text) {
        assert_eq!(parse_option(&spec.to_string()).as_ref(), Some(&spec), "{spec}");
        let line = spec.config_line();
        let args: Vec<String> = line.split_whitespace().skip(1).map(str::to_owned).collect();
        assert_eq!(parse_config(spec.kind, &args).as_ref(), Some(&spec), "{line}");
        if let Some(path) = &spec.socket {
            assert!(spec.bind.is_none() && spec.port == 0, "{spec:?}");
            assert!(!path.contains(char::is_whitespace));
        }
    }
    let _ = parse_cancel(&text);
});
