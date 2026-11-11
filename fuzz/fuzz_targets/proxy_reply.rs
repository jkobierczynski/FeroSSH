//! Feeds arbitrary bytes, as a proxy's replies, to the client side of the
//! SOCKS 5 and HTTP `CONNECT` handshakes the worker runs on the way to a
//! host: both ways the worker reads them (only what it wants at a time)
//! and in arbitrary chunks. None may panic, it must end within its
//! bounds, and nothing fed may be lost or invented.

#![no_main]

use ferossh_forward::proxy::{Client, Kind, MAX_HTTP_REPLY, Step};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&mode, rest)) = data.split_first() else {
        return;
    };
    let kind = if mode & 1 == 0 { Kind::Socks5 } else { Kind::Http };
    let (user, password) = if mode & 2 == 0 { ("", "") } else { ("me", "pw") };
    let host = if mode & 4 == 0 { "host.example" } else { "2001:db8::1" };
    let Some(mut client) = Client::new(kind, host, 22, user, password) else {
        panic!("a valid request was refused");
    };

    if mode & 8 == 0 {
        // As the worker reads: exactly what the handshake wants.
        let _ = client.start();
        let mut fed = 0;
        loop {
            let want = client.wants();
            assert!(want > 0 && want <= 300, "wants {want}");
            let Some(chunk) = rest.get(fed..fed + want) else {
                return;
            };
            fed += want;
            assert!(fed <= MAX_HTTP_REPLY + 1, "read {fed} bytes");
            match client.feed(chunk) {
                Step::Send(bytes) => assert!(bytes.len() <= 600),
                Step::Open { rest } => {
                    assert!(rest.is_empty());
                    break;
                }
                Step::AuthFailed(_) | Step::Failed(_) => break,
            }
        }
        assert_eq!(client.wants(), 0);
    } else {
        // In arbitrary chunks; what follows the reply comes back.
        let split = usize::from(mode >> 4).max(1);
        let _ = client.start();
        let mut fed = 0;
        for chunk in rest.chunks(split) {
            fed += chunk.len();
            match client.feed(chunk) {
                Step::Send(_) => {}
                Step::Open { rest: after } => {
                    assert!(after.len() <= fed);
                    assert!(rest.get(fed - after.len()..fed) == Some(&after[..]));
                    return;
                }
                Step::AuthFailed(_) | Step::Failed(_) => return,
            }
        }
    }
});
