//! Feeds arbitrary bytes to the built-in desktop viewer's RDP session as
//! the server's PDUs after the login: IronRDP's graphics, pointer and
//! bulk decompression decoders, and the viewer's own conversion to the
//! window's rectangles. None may panic, and every rectangle must fit the
//! window's framebuffer.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    ferossh_viewer::rdp::fuzz_server_pdus(data);
});
