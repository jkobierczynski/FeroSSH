//! Feeds arbitrary bytes, as a hostile Telnet server would send them, to
//! the worker's Telnet parser in arbitrary chunks, with resizes and typing
//! in between. Nothing may panic, answers stay small (no amplification),
//! and data split anywhere comes out the same as data fed whole.

#![no_main]

use ferossh_worker::telnet::{IAC, Telnet};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&split, stream)) = data.split_first() else {
        return;
    };
    let mut whole = Telnet::new(80, 24);
    let _ = whole.start();
    let all = whole.feed(stream);

    let mut chunked = Telnet::new(80, 24);
    let _ = chunked.start();
    let mut shown = Vec::new();
    let mut replies = 0;
    for (i, chunk) in stream.chunks(usize::from(split).max(1)).enumerate() {
        let fed = chunked.feed(chunk);
        // Every answer is to a three-byte request (or a TERMINAL-TYPE
        // SEND), and NAWS sends at most 13 bytes.
        assert!(fed.reply.len() <= 4 * chunk.len() + 32, "{}", fed.reply.len());
        replies += fed.reply.len();
        shown.extend(fed.data);
        let cols = u16::from(chunk.first().copied().unwrap_or(80)) * 3;
        let _ = chunked.resize(cols, (i % 300) as u16);
        let typed = chunked.encode(chunk);
        assert!(typed.len() <= chunk.len() * 2);
        assert_eq!(typed.iter().filter(|&&b| b == IAC).count() % 2, 0);
    }
    assert_eq!(shown, all.data);
    let _ = replies;
});
