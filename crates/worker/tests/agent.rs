//! Signing in through a real `ssh-agent` with every key type FeroSSH
//! takes from an agent, and a user certificate, against a real `sshd`:
//! the broker lists the agent's keys (skipping ones the policy refuses),
//! the worker logs in with each, and every sign request goes through the
//! UI's check and the broker, as in the window.
//!
//! Its own test binary, because it points `SSH_AUTH_SOCK` at its agent.
//! Pageant and the Windows OpenSSH agent speak the same protocol over a
//! named pipe; only CI on Windows and a person can try those.

#![cfg(unix)]
#![allow(
    unsafe_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use common::{Kill, available, base64, broker_reply, log_in, run, sshd, whoami};
use ferossh_auth::{Broker, Identity, Job, Reply, userauth};

#[tokio::test]
async fn signs_in_with_every_agent_key_type_and_a_certificate() {
    if !available() {
        return;
    }
    let dir: PathBuf = std::env::temp_dir().join(format!("ferossh-agent-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let user = whoami();
    let sock = dir.join("agent.sock");
    let _agent = Kill(
        Command::new("ssh-agent")
            .arg("-D")
            .arg("-a")
            .arg(&sock)
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
    // SAFETY: this test binary has no other threads reading the
    // environment yet.
    unsafe { std::env::set_var("SSH_AUTH_SOCK", &sock) };

    let kinds = [
        ("ed25519", "ed25519", "256"),
        ("p256", "ecdsa", "256"),
        ("p384", "ecdsa", "384"),
        ("p521", "ecdsa", "521"),
        ("rsa3072", "rsa", "3072"),
        ("rsa2048", "rsa", "2048"),
        ("certified", "ed25519", "256"),
    ];
    for (name, kind, bits) in kinds {
        run(Command::new("ssh-keygen")
            .args(["-q", "-t", kind, "-b", bits, "-N", "", "-C", name, "-f"])
            .arg(dir.join(name)));
    }
    let ca = dir.join("user-ca");
    run(Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(&ca));
    run(Command::new("ssh-keygen")
        .args(["-q", "-s"])
        .arg(&ca)
        .args(["-I", "ferossh-agent-test", "-n", &user, "-V", "-5m:+1h"])
        .arg(dir.join("certified.pub")));
    for (name, ..) in kinds {
        // Adds `certified-cert.pub` along with its key.
        run(Command::new("ssh-add")
            .arg(dir.join(name))
            .env("SSH_AUTH_SOCK", &sock));
    }

    let broker = Broker::spawn(|| {}).unwrap();
    broker.submit(Job::Identities { files: vec![] });
    let Reply::Identities { keys, notes } = broker_reply(&broker) else {
        panic!("not identities");
    };
    let label = |i: &Identity| {
        let kind = userauth::key_type(&i.public).unwrap_or("?").to_owned();
        format!("{kind} {}", i.comment)
    };
    let mut offered: Vec<String> = keys.iter().map(label).collect();
    offered.sort();
    assert!(
        notes
            .iter()
            .any(|n| n.contains("2048-bit RSA keys are too small")),
        "{notes:?}"
    );
    for want in [
        "ecdsa-sha2-nistp256 p256",
        "ecdsa-sha2-nistp384 p384",
        "ecdsa-sha2-nistp521 p521",
        "ssh-ed25519 ed25519",
        "ssh-ed25519-cert-v01@openssh.com certified",
        "ssh-rsa rsa3072",
    ] {
        assert!(
            offered.iter().any(|o| o == want),
            "{want} not in {offered:?}"
        );
    }
    assert!(
        !offered.iter().any(|o| o.ends_with("rsa2048")),
        "{offered:?}"
    );

    for identity in &keys {
        let what = label(identity);
        let certificate = userauth::is_certificate(&identity.public);
        let authorized = if certificate {
            String::new()
        } else {
            let line = format!(
                "{} {}\n",
                userauth::key_type(&identity.public).unwrap(),
                base64(&identity.public)
            );
            line
        };
        let (_server, port) = sshd(
            &dir,
            &identity.comment,
            &authorized,
            certificate.then(|| ca.with_extension("pub")).as_deref(),
        );
        log_in(port, &user, &broker, identity)
            .await
            .unwrap_or_else(|e| panic!("{what}: {e}"));
    }
    let _ = std::fs::remove_dir_all(&dir);
}
