//! Decodes arbitrary data as a PNG, a Sixel body, a kitty graphics
//! command and an iTerm2 inline image. It must never panic, never go past
//! the limits, and every picture must hold exactly width x height pixels.

#![no_main]

use ferossh_images::kitty::Command;
use ferossh_images::{Limits, Picture, iterm, png, sixel};
use libfuzzer_sys::fuzz_target;

/// Small limits, so the fuzzer reaches them.
const LIMITS: Limits = Limits {
    max_side: 300,
    max_pixels: 20_000,
    max_encoded: 1 << 16,
};

fn check(p: &Picture) {
    assert!(LIMITS.allows(p.width, p.height), "{p:?}");
    assert_eq!(p.pixels.len(), p.width as usize * p.height as usize);
    // Premultiplied: no channel above alpha.
    for &px in &p.pixels {
        let a = px >> 24;
        assert!((px >> 16 & 0xFF) <= a && (px >> 8 & 0xFF) <= a && (px & 0xFF) <= a);
    }
}

fuzz_target!(|data: &[u8]| {
    let Some((&which, rest)) = data.split_first() else {
        return;
    };
    match which % 4 {
        0 => {
            // Give PNG-shaped input a head start.
            let mut file = b"\x89PNG\r\n\x1a\n".to_vec();
            file.extend_from_slice(rest);
            if let Ok(p) = png::decode(&file, &LIMITS) {
                check(&p);
            }
        }
        1 => {
            if let Ok(p) = sixel::decode(rest, &LIMITS) {
                check(&p);
            }
        }
        2 => {
            let (keys, payload) = match rest.iter().position(|&b| b == b';') {
                Some(at) => (&rest[..at], &rest[at + 1..]),
                None => (rest, &[][..]),
            };
            let cmd = Command::parse(keys);
            if let Ok(p) = cmd.decode(payload, &LIMITS) {
                check(&p);
            }
            for result in [Ok(()), Err(ferossh_images::Error::Invalid)] {
                if let Some(reply) = cmd.reply(result) {
                    assert!(reply.iter().all(|b| (0x20..0x7f).contains(b) || *b == 0x1b));
                }
            }
        }
        _ => {
            let (_, payload) = iterm::parse(rest);
            if let Ok(p) = iterm::decode(payload, &LIMITS) {
                check(&p);
            }
            if let Ok(p) = png::decode(rest, &LIMITS) {
                check(&p);
            }
        }
    }
});
