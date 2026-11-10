//! Feeds arbitrary bytes, as a hostile server would send them, to russh's
//! client with the worker's algorithm choices: the version line, key
//! exchange and everything up to the exchange hash signature, which
//! random bytes cannot forge. This is what a server reaches before its
//! host key is checked. Nothing may panic or hang; the clock is paused,
//! so timeouts pass at once.

#![no_main]

use std::sync::Arc;
use std::time::Duration;

use ferossh_worker::crypto_policy;
use libfuzzer_sys::fuzz_target;
use russh::client;
use russh::keys::PublicKeyOrCertificate;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct Trusting;

impl client::Handler for Trusting {
    type Error = russh::Error;

    // Every host key is accepted, so the fuzzer gets as far as a real
    // server could after the user trusted it.
    async fn check_server_key(&mut self, _: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

fuzz_target!(|data: &[u8]| {
    let Some((&mode, server)) = data.split_first() else {
        return;
    };
    let preferred = match mode % 4 {
        0 => crypto_policy::preferred(),
        1 => crypto_policy::with_legacy(crypto_policy::preferred()),
        2 => crypto_policy::without_terrapin(crypto_policy::with_legacy(crypto_policy::preferred())),
        _ => crypto_policy::aes_gcm_first(crypto_policy::preferred()),
    };
    let config = Arc::new(client::Config {
        preferred,
        inactivity_timeout: Some(Duration::from_secs(30)),
        ..Default::default()
    });
    let server = server.to_vec();
    let Ok(rt) = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .start_paused(true)
        .build()
    else {
        return;
    };
    rt.block_on(async move {
        let (ours, theirs) = tokio::io::duplex(1 << 16);
        let (mut from_client, mut to_client) = tokio::io::split(theirs);
        // The server says its piece, whatever the client sends, and hangs up.
        tokio::spawn(async move {
            let _ = to_client.write_all(&server).await;
            let _ = to_client.shutdown().await;
        });
        tokio::spawn(async move {
            let mut sink = [0u8; 4096];
            while matches!(from_client.read(&mut sink).await, Ok(n) if n > 0) {}
        });
        let run = async {
            if let Ok(mut handle) = client::connect_stream(config, ours, Trusting).await {
                let _ = handle.authenticate_none("fuzz").await;
            }
        };
        // A paused clock makes this an hour of nothing happening, not a
        // real wait: a hang shows up as a timeout here.
        if tokio::time::timeout(Duration::from_secs(3600), run).await.is_err() {
            panic!("the client hung on a closed connection");
        }
    });
});
