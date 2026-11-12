//! Feeds arbitrary bytes, as a hostile server (or a program on it) would
//! send them on a forwarded agent channel, to the worker's agent protocol
//! reader in arbitrary chunks. Nothing may panic, the buffer stays within
//! one message of the limit, bytes split anywhere give the same requests
//! as bytes fed whole, and a sign request passed on to the UI is exactly
//! its key, data and flags: nothing more of the server's bytes rides
//! along to the user's confirmation.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&split, stream)) = data.split_first() else {
        return;
    };
    ferossh_worker::fuzz_agent_stream(stream, usize::from(split).max(1));
});
