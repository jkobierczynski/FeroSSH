//! Feeds arbitrary private key files to the reader: OpenSSH files (as
//! text, and as fuzzed binary containers in a valid PEM wrapper, so the
//! short-scalar repair path sees every shape) and PuTTY `.ppk` headers.
//! None may panic, and a file that loads must sign or fail cleanly.

#![no_main]

use std::path::Path;

use ferossh_auth::keyfile::KeyFile;
use libfuzzer_sys::fuzz_target;

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64(data: &[u8]) -> String {
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b = [
            chunk.first().copied().unwrap_or(0),
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(B64[(n >> (18 - 6 * i) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

fuzz_target!(|data: &[u8]| {
    let Some((&mode, rest)) = data.split_first() else {
        return;
    };
    let text = match mode % 3 {
        0 => String::from_utf8_lossy(rest).into_owned(),
        1 => {
            let mut body = b"openssh-key-v1\0".to_vec();
            body.extend_from_slice(rest);
            format!(
                "-----BEGIN OPENSSH PRIVATE KEY-----\n{}\n-----END OPENSSH PRIVATE KEY-----\n",
                base64(&body)
            )
        }
        _ => format!(
            "PuTTY-User-Key-File-3: ssh-ed25519\nEncryption: none\nComment: c\nPublic-Lines: 1\n{}\n",
            base64(rest)
        ),
    };
    let Ok(kf) = KeyFile::parse(Path::new("fuzz"), &text) else {
        return;
    };
    let _ = kf.is_security_key();
    let _ = kf.sign(Some("pw"), b"data");
    let _ = kf.sign(None, b"data");
});
