//! Feeds arbitrary bytes, as a hostile server's status command would print
//! them, to the worker's status bar sampler in arbitrary chunks. Nothing
//! may panic, every sample stays within what the IPC decoder accepts
//! (CPU at most 100%, used never over the whole), and output split
//! anywhere gives the same samples as output fed whole.

#![no_main]

use ferossh_ipc::{Decoder, Message, ServerStats, encode};
use ferossh_worker::stats::Sampler;
use libfuzzer_sys::fuzz_target;

fn check(stats: ServerStats) {
    let msg = Message::Stats { channel: 1, stats };
    let mut frame = Vec::new();
    encode(&msg, &mut frame).expect("a sample always encodes");
    let mut dec = Decoder::new();
    dec.feed(&frame);
    assert_eq!(dec.next_message(), Ok(Some(msg)));
}

fuzz_target!(|data: &[u8]| {
    let Some((&split, stream)) = data.split_first() else {
        return;
    };
    let mut whole = Vec::new();
    Sampler::new().feed(stream, |s| whole.push(s));

    let mut chunked = Vec::new();
    let mut sampler = Sampler::new();
    for chunk in stream.chunks(usize::from(split).max(1)) {
        sampler.feed(chunk, |s| chunked.push(s));
    }
    assert_eq!(whole, chunked);
    for s in whole {
        check(s);
    }
});
