//! Feeds arbitrary bytes, as a hostile WinRM server (or anyone on the
//! HTTP path) would send them, to everything the worker parses from one:
//! HTTP responses, the Negotiate header, the encryption framing, and the
//! SOAP and XML inside. Nothing may panic, and a response parsed from
//! bytes that arrive in pieces is the same as one parsed whole.

#![no_main]

use ferossh_worker::winrm::{fuzz_reply, fuzz_reply_chunked};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&split, stream)) = data.split_first() else {
        return;
    };
    fuzz_reply(stream);
    fuzz_reply_chunked(stream, usize::from(split).max(1));
});
