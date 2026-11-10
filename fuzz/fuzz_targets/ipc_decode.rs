//! Feeds arbitrary bytes to the IPC decoder in arbitrary chunks. It must
//! never panic, and every message it accepts must re-encode to the same bytes.

#![no_main]

use ferossh_ipc::{Decoder, encode};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&split, stream)) = data.split_first() else {
        return;
    };
    let mut dec = Decoder::new();
    let mut accepted = Vec::new();
    for chunk in stream.chunks(usize::from(split).max(1)) {
        dec.feed(chunk);
        loop {
            match dec.next_message() {
                Ok(Some(msg)) => accepted.push(msg),
                Ok(None) => break,
                Err(_) => return,
            }
        }
    }
    let mut wire = Vec::new();
    for msg in &accepted {
        assert!(encode(msg, &mut wire).is_ok());
    }
    assert!(stream.starts_with(&wire));
});
