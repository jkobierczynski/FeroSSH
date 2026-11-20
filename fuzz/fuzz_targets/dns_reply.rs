//! Feeds arbitrary bytes, as a DNS server's answer to the SSHFP query of
//! `VerifyHostKeyDNS`, to the worker's parser, for one of a few names,
//! and arbitrary text to the name encoder. None may panic; an accepted
//! answer keeps within its bounds; an encoded name is valid wire format.

#![no_main]

use ferossh_worker::dns::{self, MAX_DIGEST, MAX_RECORDS};
use libfuzzer_sys::fuzz_target;

const NAMES: [&str; 3] = ["host.example", "a", "Deep.Sub.Example.ORG."];

fuzz_target!(|data: &[u8]| {
    let Some((&pick, msg)) = data.split_first() else {
        return;
    };
    let host = NAMES[usize::from(pick) % NAMES.len()];
    let Some(name) = dns::wire_name(host) else {
        panic!("a valid name was refused");
    };
    // The id the answer is expected to carry: its own, so the parser
    // gets past the header, or another one.
    let own = match msg {
        [a, b, ..] => u16::from_be_bytes([*a, *b]),
        _ => 0,
    };
    let id = if pick & 0x80 == 0 { own } else { own ^ 1 };
    if let Ok(answer) = dns::parse(msg, id, &name) {
        assert!(answer.records.len() <= MAX_RECORDS);
        for r in &answer.records {
            assert!(!r.digest.is_empty() && r.digest.len() <= MAX_DIGEST);
        }
    }
    // The query for the name parses as a question (an answer without
    // the QR bit is not ours).
    let q = dns::query(id, &name);
    assert!(dns::parse(&q, id, &name).is_err());

    let text = String::from_utf8_lossy(msg);
    if let Some(wire) = dns::wire_name(&text) {
        assert!(wire.len() <= 255 && wire.last() == Some(&0));
        let mut at = 0;
        while let Some(&len) = wire.get(at) {
            if len == 0 {
                assert_eq!(at + 1, wire.len());
                break;
            }
            assert!(len <= 63);
            at += 1 + usize::from(len);
        }
    }
});
