//! Feeds arbitrary bytes, as a hostile server would send them, to the
//! ZMODEM detector, receiver and sender in arbitrary chunks, with timeouts
//! in between. Nothing may panic; the detector loses no output; the
//! receiver hands out no more file data than it was fed and answers in
//! proportion; the sender only asks for bytes inside its files.

#![no_main]

use ferossh_zmodem::{Detector, MAX_READ, Receiver, RxEvent, Sender, TxEvent, Upload};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&split, stream)) = data.split_first() else {
        return;
    };
    let size = usize::from(split).max(1);

    // Without a start, every byte shows, in order.
    let mut d = Detector::new();
    let mut shown = Vec::new();
    let mut found = false;
    for chunk in stream.chunks(size) {
        let (s, f) = d.scan(chunk);
        shown.extend(s);
        if f.is_some() {
            found = true;
            break;
        }
    }
    if !found {
        shown.extend(d.flush());
        assert_eq!(shown, stream);
    }

    let mut rx = Receiver::new();
    let mut out = rx.take_output().len();
    let mut got = 0;
    for (i, chunk) in stream.chunks(size).enumerate() {
        let used = rx.feed(chunk);
        assert!(used <= chunk.len());
        if i % 7 == 6 {
            rx.timeout();
        }
        while let Some(event) = rx.next_event() {
            match event {
                RxEvent::File(_) if i % 3 == 0 => rx.skip(),
                RxEvent::File(_) => rx.accept(),
                RxEvent::Data(bytes) => got += bytes.len(),
                _ => {}
            }
        }
        out += rx.take_output().len();
        if rx.ended() {
            break;
        }
    }
    assert!(got <= stream.len());
    assert!(out <= 64 * (stream.len() + 1), "{out}");

    let sizes = [u64::from(split) * 1000, 3];
    let files = sizes
        .iter()
        .map(|&size| Upload {
            name: b"f".to_vec(),
            size,
            mtime: 0,
        })
        .collect();
    let mut tx = Sender::new(files);
    for (i, chunk) in stream.chunks(size).enumerate() {
        tx.feed(chunk);
        if i % 5 == 4 {
            tx.timeout();
        }
        while let Some(event) = tx.next_event() {
            if let TxEvent::Read { file, offset, len } = event {
                let size = sizes[file as usize];
                assert!(offset < size && len <= MAX_READ);
                assert!(offset + u64::from(len) <= size);
                tx.provide(offset, &vec![0x18; len as usize]);
            }
        }
        let _ = tx.take_output();
        if tx.ended() {
            break;
        }
    }
});
