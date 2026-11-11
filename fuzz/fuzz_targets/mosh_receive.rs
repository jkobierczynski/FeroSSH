//! Plays a hostile Mosh server that holds the session key: arbitrary
//! packet contents (timestamps, fragments, compressed instructions,
//! screen diffs) are encrypted as the server would and fed to the
//! worker's Mosh client, with keystrokes, resizes and time passing in
//! between. Nothing may panic, the client never sends more than its own
//! input calls for, and what it renders stays bounded.

#![no_main]

use ferossh_mosh::client::Client;
use ferossh_mosh::crypto::Session;
use ferossh_mosh::wire::{Fragment, Instruction, TO_CLIENT, deflate};
use libfuzzer_sys::fuzz_target;

const KEY: [u8; 16] = [9; 16];

fuzz_target!(|data: &[u8]| {
    let mut client = Client::new(&KEY, false, 80, 24, 1, 0);
    let mut server = Session::new(&KEY);
    let mut now = 0u64;
    let mut seq = 0u64;
    let mut rest = data;
    while let Some((&op, tail)) = rest.split_first() {
        let len = usize::from(tail.first().copied().unwrap_or(0));
        let body = tail.get(1..1 + len.min(tail.len().saturating_sub(1))).unwrap_or(&[]);
        rest = tail.get(1 + body.len()..).unwrap_or(&[]);
        match op % 8 {
            // A raw packet plaintext: timestamps, then a fragment.
            0 | 1 => {
                seq += u64::from(op >> 4);
                if let Ok(p) = server.encrypt(TO_CLIENT | seq, body) {
                    let _ = client.receive(&p, now);
                }
            }
            // A whole instruction with `body` as the screen diff, from a
            // state the client may have.
            2 | 3 => {
                let num = u64::from(op >> 3);
                let inst = Instruction {
                    protocol_version: 2,
                    old_num: num.saturating_sub(1) * u64::from(op & 1),
                    new_num: num + 1,
                    ack_num: num,
                    throwaway_num: num.saturating_sub(2),
                    diff: body.to_vec(),
                    chaff: Vec::new(),
                };
                let frag = Fragment {
                    id: seq,
                    num: 0,
                    last: true,
                    contents: deflate(&inst.to_bytes()),
                };
                let mut text = vec![0, 0, 0xFF, 0xFF];
                text.extend(frag.to_bytes());
                seq += 1;
                if let Ok(p) = server.encrypt(TO_CLIENT | seq, &text) {
                    let _ = client.receive(&p, now);
                }
            }
            4 => client.keys(body),
            5 => client.resize(u16::from(op), u16::from(len as u8 % 60)),
            6 => now += u64::from(op) * u64::from(len as u8),
            _ => client.start_shutdown(now),
        }
        if let Ok(out) = client.tick(now) {
            assert!(out.len() <= 64, "{}", out.len());
        }
        let _ = client.wait(now);
        if let Some(frame) = client.frame(now) {
            assert!(frame.len() <= 4 << 20, "{}", frame.len());
        }
    }
});
