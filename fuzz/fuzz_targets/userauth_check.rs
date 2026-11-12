//! Feeds arbitrary sign requests to the check that stands between a
//! possibly compromised SSH worker and the user's keys. It must never
//! panic, and whatever it accepts must be exactly one RFC 4252 publickey
//! request for that user and key, with an algorithm FeroSSH allows: the
//! accepted bytes are rebuilt from the parts and must match. The check
//! for a forwarded agent's sign requests reads the same bytes and must
//! not panic either, nor may the check of what a Kerberos (gssapi-with-mic)
//! MIC signs, which must accept only that request for that user.

#![no_main]

use ferossh_auth::gss::check_mic_data;
use ferossh_auth::userauth::{allowed_algorithms, check, check_forwarded, key_type};
use libfuzzer_sys::fuzz_target;

fn string(out: &mut Vec<u8>, s: &[u8]) {
    out.extend_from_slice(&u32::try_from(s.len()).unwrap_or(u32::MAX).to_be_bytes());
    out.extend_from_slice(s);
}

/// Splits `data` on 0xff bytes into a few parts.
fn parts(data: &[u8]) -> Vec<&[u8]> {
    data.split(|&b| b == 0xff).collect()
}

fuzz_target!(|data: &[u8]| {
    let Some((&mode, rest)) = data.split_first() else {
        return;
    };
    let p = parts(rest);
    let user = std::str::from_utf8(p.first().copied().unwrap_or_default()).unwrap_or("me");
    let kind = [
        "ssh-ed25519",
        "ssh-rsa",
        "ecdsa-sha2-nistp256",
        "sk-ssh-ed25519@openssh.com",
    ][usize::from(mode & 3)];
    let mut key = Vec::new();
    string(&mut key, kind.as_bytes());
    string(&mut key, p.get(1).copied().unwrap_or_default());
    let request = if mode & 4 != 0 {
        // Raw bytes: the parser on its own.
        p.get(2).copied().unwrap_or_default().to_vec()
    } else {
        // A request built from fuzzed fields, so the later checks run.
        let mut d = Vec::new();
        string(&mut d, p.get(2).copied().unwrap_or(&[7; 32]));
        d.push(if mode & 8 != 0 { 50 } else { mode });
        string(
            &mut d,
            if mode & 16 != 0 {
                user.as_bytes()
            } else {
                p.get(3).copied().unwrap_or_default()
            },
        );
        string(&mut d, b"ssh-connection");
        string(&mut d, b"publickey");
        d.push(1);
        string(&mut d, p.get(4).copied().unwrap_or(kind.as_bytes()));
        string(
            &mut d,
            if mode & 32 != 0 {
                &key
            } else {
                p.get(5).copied().unwrap_or_default()
            },
        );
        d.extend_from_slice(p.get(6).copied().unwrap_or_default());
        d
    };
    let _ = check_forwarded(&request, &key, u32::from(mode >> 6) << 1);
    // The same bytes as data a Kerberos MIC would sign: accepted only
    // when it is exactly a gssapi-with-mic request for this user.
    let mut mic = request.clone();
    if mode & 4 == 0 && mode & 64 != 0 {
        // Built the same way, with gssapi-with-mic in place of the rest.
        mic.clear();
        string(&mut mic, p.get(2).copied().unwrap_or(&[7; 32]));
        mic.push(if mode & 8 != 0 { 50 } else { mode });
        string(&mut mic, if mode & 16 != 0 { user.as_bytes() } else { p.get(3).copied().unwrap_or_default() });
        string(&mut mic, b"ssh-connection");
        string(&mut mic, p.get(4).copied().unwrap_or(b"gssapi-with-mic"));
        mic.extend_from_slice(p.get(6).copied().unwrap_or_default());
    }
    if check_mic_data(&mic, user).is_ok() {
        let id_len = u32::from_be_bytes(mic[..4].try_into().unwrap_or_default()) as usize;
        assert!((20..=64).contains(&id_len));
        let mut rebuilt = mic[..4 + id_len].to_vec();
        rebuilt.push(50);
        string(&mut rebuilt, user.as_bytes());
        string(&mut rebuilt, b"ssh-connection");
        string(&mut rebuilt, b"gssapi-with-mic");
        assert_eq!(rebuilt, mic);
    }
    let Ok(target) = check(&request, user, &key) else {
        return;
    };
    assert_eq!(key_type(&key), Some(kind));
    assert!(allowed_algorithms(kind).contains(&target.algorithm.as_str()));
    // Rebuild: the session id is the only part the check does not pin.
    let id_len = u32::from_be_bytes(request[..4].try_into().unwrap_or_default()) as usize;
    assert!((1..=64).contains(&id_len));
    let mut rebuilt = request[..4 + id_len].to_vec();
    rebuilt.push(50);
    string(&mut rebuilt, user.as_bytes());
    string(&mut rebuilt, b"ssh-connection");
    string(&mut rebuilt, b"publickey");
    rebuilt.push(1);
    string(&mut rebuilt, target.algorithm.as_bytes());
    string(&mut rebuilt, &key);
    assert_eq!(rebuilt, request);
});
