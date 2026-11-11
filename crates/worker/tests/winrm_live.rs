//! A WinRM session against this Windows machine's own WinRM service,
//! with real SSPI: the worker as the window runs it, and the test playing
//! the window's part with the broker's Negotiate contexts (NTLM with the
//! logged-on user, as the machine is in no domain). It runs cmd and
//! PowerShell, sends a line to each and reads the answer, then exits.
//!
//! Runs only where `FEROSSH_WINRM_LIVE` is set: the Windows CI job turns
//! WinRM on (`Enable-PSRemoting`), adds an HTTPS listener on 5986 with a
//! self-signed certificate, and sets it.

#![cfg(windows)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::time::Duration;

use ferossh_auth::negotiate::{Contexts, SspiJob, sspi::Sspi};
use ferossh_ipc::{Message, PROTOCOL_VERSION};
use tokio::sync::mpsc;

/// Runs `command`, types `line`, waits for `want` in the output, types
/// `exit` and returns the exit code and everything shown. With `https`
/// the session goes to the HTTPS listener on 5986, with TLS done by
/// SChannel in the test as the window would do it.
async fn run(command: &str, line: &str, want: &str, https: bool) -> (Option<u32>, String) {
    let (to_worker, inbox) = mpsc::channel(64);
    let (out, mut from_worker) = mpsc::channel(64);
    tokio::spawn(ferossh_worker::serve(inbox, out));
    assert!(matches!(
        from_worker.recv().await,
        Some(Message::Hello { .. })
    ));
    let send = |m| {
        let tx = to_worker.clone();
        async move { tx.send(m).await.unwrap() }
    };
    send(Message::Hello {
        version: PROTOCOL_VERSION,
    })
    .await;
    send(Message::WinrmConnect {
        channel: 1,
        host: "localhost".into(),
        port: if https { 5986 } else { 5985 },
        connect_timeout: 20,
        command: command.into(),
        https,
    })
    .await;
    let mut contexts = Contexts::default();
    let mut shown = String::new();
    let mut typed = false;
    let mut exited = false;
    let mut code = None;
    let mut certified = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
    loop {
        let msg = tokio::time::timeout_at(deadline, from_worker.recv())
            .await
            .unwrap_or_else(|_| panic!("{command}: stuck; shown {shown:?}"))
            .unwrap();
        match msg {
            Message::SspiRequest { conn, op, .. } => {
                let output = contexts.run(
                    &Sspi,
                    SspiJob {
                        login: 1,
                        conn,
                        host: "localhost".into(),
                        credentials: None,
                        op,
                    },
                );
                if let Some(p) = &output.package {
                    eprintln!("{command}: logged in with {p}");
                }
                // The CI listener has a self-signed certificate, which
                // Windows does not trust; the window would ask about it.
                if let Some(cert) = &output.cert {
                    eprintln!(
                        "{command}: certificate {} ({:?})",
                        cert.sha256, cert.windows
                    );
                    assert!(https, "{command}: a certificate over HTTP");
                    certified = true;
                }
                send(Message::SspiReply {
                    channel: 1,
                    conn,
                    result: output.result,
                })
                .await;
            }
            Message::Connected { .. } => {
                assert_eq!(certified, https, "{command}: certificate check");
                send(Message::Data {
                    channel: 1,
                    bytes: format!("{line}\r\n").into_bytes(),
                })
                .await;
                typed = true;
            }
            Message::Data { bytes, .. } | Message::ExecStderr { bytes, .. } => {
                shown.push_str(&String::from_utf8_lossy(&bytes));
                if typed && !exited && shown.contains(want) {
                    exited = true;
                    send(Message::Data {
                        channel: 1,
                        bytes: b"exit\r\n".to_vec(),
                    })
                    .await;
                }
            }
            Message::ExitStatus { code: c, .. } => code = Some(c),
            Message::Disconnected { reason, .. } => {
                eprintln!("{command}: {reason}\n{shown}");
                assert!(exited, "{command}: ended early: {reason}; shown {shown:?}");
                return (code, shown);
            }
            Message::Notice { .. } => {}
            other => panic!("{command}: unexpected {other:?}"),
        }
    }
}

#[tokio::test]
async fn runs_cmd_and_powershell_over_winrm() {
    if std::env::var_os("FEROSSH_WINRM_LIVE").is_none() {
        eprintln!("FEROSSH_WINRM_LIVE is not set; skipping");
        return;
    }
    let (code, shown) = run("cmd", "set /a 6*7", "42", false).await;
    assert_eq!(code, Some(0), "{shown}");
    // FeroSSH's default command. The output, not the typed line, holds
    // the word.
    let (code, shown) = run(
        "powershell -NoLogo",
        "Write-Output ('fero' + 'ssh')",
        "ferossh",
        false,
    )
    .await;
    assert_eq!(code, Some(0), "{shown}");
}

#[tokio::test]
async fn runs_cmd_over_winrm_https() {
    if std::env::var_os("FEROSSH_WINRM_LIVE").is_none() {
        eprintln!("FEROSSH_WINRM_LIVE is not set; skipping");
        return;
    }
    let (code, shown) = run("cmd", "set /a 6*7", "42", true).await;
    assert_eq!(code, Some(0), "{shown}");
}
