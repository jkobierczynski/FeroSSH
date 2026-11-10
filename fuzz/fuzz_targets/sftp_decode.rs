//! Feeds arbitrary bytes, as a hostile SFTP server would send them, to the
//! worker's SFTP decoder in arbitrary chunks. It must never panic, and
//! every name it lets through must be one a listing can carry.

#![no_main]

use ferossh_sftp::wire::Decoder;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&split, stream)) = data.split_first() else {
        return;
    };
    let mut dec = Decoder::new();
    for chunk in stream.chunks(usize::from(split).max(1)) {
        dec.feed(chunk);
        loop {
            match dec.next_response() {
                Ok(Some(ferossh_sftp::wire::Response::Name { entries, .. })) => {
                    assert!(entries.iter().all(|e| e.name != b"." && e.name != b".."));
                }
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(_) => return,
            }
        }
    }
});
