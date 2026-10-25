//! Logging in with FIDO2 security keys against a real `sshd`, with no
//! hardware:
//!
//! - key files, FeroSSH's own (`ferossh --new-security-key`) and
//!   `ssh-keygen`'s, signed through the broker by the software
//!   authenticator, where the window has Windows' WebAuthn;
//! - keys held by a real `ssh-agent`, which signs through OpenSSH's own
//!   software security key (built by `interop/sk-dummy.sh`, its path in
//!   `FEROSSH_SK_DUMMY`), so the agent's sk signature format is the real
//!   one.
//!
//! Its own test binary, because it points `SSH_AUTH_SOCK` at its agent.

#![cfg(unix)]
#![allow(
    unsafe_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use common::{Kill, available, base64, broker_reply, log_in, run, sshd, whoami};
use ferossh_auth::sk::soft::SoftAuthenticator;
use ferossh_auth::sk::{self, DEFAULT_APPLICATION, EnrollRequest, SkAlgorithm};
use ferossh_auth::{Broker, Identity, Job, KeyFileSpec, Reply, Source, userauth};

/// OpenSSH's software security key, if built.
fn sk_dummy() -> Option<PathBuf> {
    let path = std::env::var_os("FEROSSH_SK_DUMMY")
        .map(PathBuf::from)
        .filter(|p| p.exists());
    if path.is_none() {
        assert!(
            std::env::var_os("FEROSSH_REQUIRE_SK_DUMMY").is_none(),
            "FEROSSH_REQUIRE_SK_DUMMY is set but FEROSSH_SK_DUMMY names no \
             sk-dummy.so (interop/sk-dummy.sh builds it)"
        );
        eprintln!("FEROSSH_SK_DUMMY not set; skipping ssh-keygen's and ssh-agent's security keys");
    }
    path
}

fn authorized(identity: &Identity) -> String {
    format!(
        "{} {}\n",
        userauth::key_type(&identity.public).unwrap(),
        base64(&identity.public)
    )
}

fn identities(broker: &Broker, files: Vec<KeyFileSpec>) -> Vec<Identity> {
    broker.submit(Job::Identities { files });
    let Reply::Identities { keys, notes } = broker_reply(broker) else {
        panic!("not identities");
    };
    assert!(notes.is_empty(), "{notes:?}");
    keys
}

fn spec(path: &Path) -> KeyFileSpec {
    KeyFileSpec {
        path: path.to_owned(),
        explicit: true,
    }
}

#[tokio::test]
async fn signs_in_with_security_keys() {
    if !available() {
        return;
    }
    let dir: PathBuf = std::env::temp_dir().join(format!("ferossh-sk-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let user = whoami();
    // Only this test's keys, not the user's agent. SAFETY: this test
    // binary has no other threads reading the environment yet.
    unsafe { std::env::remove_var("SSH_AUTH_SOCK") };

    // FeroSSH's own key files, as `--new-security-key` writes them.
    let mut files = Vec::new();
    for (name, algorithm) in [
        ("own_ed25519_sk", SkAlgorithm::Ed25519),
        ("own_ecdsa_sk", SkAlgorithm::EcdsaP256),
    ] {
        let enrolled = sk::enroll(
            &SoftAuthenticator::new(),
            &EnrollRequest {
                algorithm,
                application: DEFAULT_APPLICATION,
                challenge: b"challenge",
                platform: false,
                user_verification: false,
            },
            name,
        )
        .unwrap();
        std::fs::write(dir.join(name), enrolled.private).unwrap();
        files.push(spec(&dir.join(name)));
    }
    // ssh-keygen's, made with OpenSSH's software security key; the
    // software authenticator understands its key handles.
    let dummy = sk_dummy();
    let keygen = ["keygen_ed25519_sk", "keygen_ecdsa_sk"];
    if let Some(so) = &dummy {
        for (name, kind) in keygen.iter().zip(["ed25519-sk", "ecdsa-sk"]) {
            run(Command::new("ssh-keygen")
                .args(["-q", "-t", kind, "-N", "", "-C", name, "-w"])
                .arg(so)
                .arg("-f")
                .arg(dir.join(name)));
            files.push(spec(&dir.join(name)));
        }
    }

    let soft = Arc::new(SoftAuthenticator::new());
    let broker = Broker::spawn_with(|| {}, Box::new(soft.clone())).unwrap();
    let keys = identities(&broker, files.clone());
    assert_eq!(keys.len(), files.len(), "{keys:?}");
    for identity in &keys {
        assert!(matches!(identity.source, Source::File(_)));
        let (_server, port) = sshd(&dir, &identity.comment, &authorized(identity), None);
        log_in(port, &user, &broker, identity)
            .await
            .unwrap_or_else(|e| panic!("{}: {e}", identity.comment));
    }

    // A key that was not touched signs nothing.
    soft.set_touched(false);
    let identity = &keys[0];
    let (_server, port) = sshd(&dir, &identity.comment, &authorized(identity), None);
    let refused = log_in(port, &user, &broker, identity).await.unwrap_err();
    assert!(refused.contains("not touched"), "{refused}");
    drop(broker);

    // Keys in ssh-agent: the agent signs through the software security
    // key, and the broker needs no authenticator of its own.
    if let Some(so) = &dummy {
        let sock = dir.join("agent.sock");
        let _agent = Kill(
            Command::new("ssh-agent")
                .args(["-D", "-a"])
                .arg(&sock)
                .arg("-P")
                .arg(so)
                .stdout(Stdio::null())
                .spawn()
                .unwrap(),
        );
        for _ in 0..100 {
            if sock.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        // SAFETY: as above; the broker thread above has ended.
        unsafe { std::env::set_var("SSH_AUTH_SOCK", &sock) };
        for name in keygen {
            run(Command::new("ssh-add")
                .arg("-S")
                .arg(so)
                .arg(dir.join(name))
                .env("SSH_AUTH_SOCK", &sock));
        }
        let broker = Broker::spawn_with(|| {}, Box::new(sk::Unavailable)).unwrap();
        let keys = identities(&broker, vec![]);
        let mut comments: Vec<&str> = keys.iter().map(|k| k.comment.as_str()).collect();
        comments.sort_unstable();
        assert_eq!(comments, ["keygen_ecdsa_sk", "keygen_ed25519_sk"]);
        for identity in &keys {
            assert!(matches!(identity.source, Source::Agent(_)));
            let name = format!("agent-{}", identity.comment);
            let (_server, port) = sshd(&dir, &name, &authorized(identity), None);
            log_in(port, &user, &broker, identity)
                .await
                .unwrap_or_else(|e| panic!("agent {}: {e}", identity.comment));
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}
