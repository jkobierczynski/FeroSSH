//! `ferossh.exe --check-sandbox`: starts the sandboxed processes the way
//! the window does and checks that they run. The CI's Windows job runs it
//! on the release exe, and a user can run it to see what this Windows
//! allows (`ferossh.exe --check-sandbox | more`; the exe has no console of
//! its own).
//!
//! A worker must answer the greeting and fail a connection to 127.0.0.1,
//! which an AppContainer cannot reach, with a reason: that runs its tokio
//! runtime and sockets under the job and the mitigations. A second one
//! looks up a name that does not exist, which runs Windows' name
//! resolution in the sandbox. A VNC viewer must answer a server's version
//! line. Each process's mitigations are as Windows reports them.
//!
//! Exit code 0: everything ran sandboxed, with Win32k lockdown, Control
//! Flow Guard and no dynamic code (CET shadow stacks depend on the CPU, so
//! they are reported but not required); 2: it ran with less (an older
//! Windows); 1: something failed.

use std::io::{Read, Write};
use std::sync::mpsc;
use std::time::Duration;

use ferossh_ipc::{Decoder, Message, PROTOCOL_VERSION, Timing, encode};

use crate::sandbox::{self, Hardening, Worker};

const WAIT: Duration = Duration::from_secs(30);

/// Reads messages from the worker on a thread, so the check can time out.
fn messages(mut input: std::fs::File) -> mpsc::Receiver<Message> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut decoder = Decoder::new();
        let mut buf = vec![0u8; 64 * 1024];
        while let Ok(n @ 1..) = input.read(&mut buf) {
            decoder.feed(buf.get(..n).unwrap_or_default());
            while let Ok(Some(msg)) = decoder.next_message() {
                if tx.send(msg).is_err() {
                    return;
                }
            }
        }
    });
    rx
}

fn send(output: &mut std::fs::File, msg: &Message) -> Result<(), String> {
    let mut frame = Vec::new();
    encode(msg, &mut frame).map_err(|e| e.to_string())?;
    output
        .write_all(&frame)
        .and_then(|()| output.flush())
        .map_err(|e| format!("cannot write to the worker: {e}"))
}

fn check_worker(w: &mut Worker, host: &str) -> Result<String, String> {
    let rx = messages(w.from_worker.try_clone().map_err(|e| e.to_string())?);
    send(
        &mut w.to_worker,
        &Message::Hello {
            version: PROTOCOL_VERSION,
        },
    )?;
    match rx.recv_timeout(WAIT) {
        Ok(Message::Hello { version }) if version == PROTOCOL_VERSION => {}
        Ok(other) => return Err(format!("greeted with {other:?}")),
        Err(_) => return Err("no greeting: the worker did not start".into()),
    }
    send(
        &mut w.to_worker,
        &Message::Connect {
            channel: 1,
            host: host.into(),
            port: 9,
            user: "check".into(),
            cols: 80,
            rows: 24,
            host_key_algs: Vec::new(),
            x11: None,
            legacy: false,
            timing: Timing {
                connect_timeout: 5,
                ..Timing::default()
            },
            tunnel: None,
            shell: true,
        },
    )?;
    loop {
        match rx.recv_timeout(WAIT) {
            Ok(Message::Disconnected { reason, .. }) => return Ok(reason),
            Ok(_) => {}
            Err(_) => return Err("no answer to a connection attempt".into()),
        }
    }
}

#[cfg(feature = "vnc-viewer")]
fn check_viewer(v: &mut Worker) -> Result<(), String> {
    use ferossh_viewer::pipe::{FromViewer, Protocol, ToViewer, read_frame, write_frame};
    let start = ToViewer::Start {
        protocol: Protocol::Vnc,
        width: 800,
        height: 600,
        layout: 0x0409,
        user: String::new(),
    };
    for msg in [start, ToViewer::Net(b"RFB 003.008\n".to_vec())] {
        write_frame(&mut v.to_worker, &msg.encode()).map_err(|e| e.to_string())?;
    }
    let mut input = v.from_worker.try_clone().map_err(|e| e.to_string())?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(read_frame(&mut input));
    });
    match rx.recv_timeout(WAIT) {
        Ok(Ok(Some(frame))) => match FromViewer::decode(&frame) {
            Some(FromViewer::Net(bytes)) if bytes.starts_with(b"RFB 003.") => Ok(()),
            other => Err(format!("answered {other:?}")),
        },
        Ok(Ok(None)) => Err("the viewer exited".into()),
        Ok(Err(e)) => Err(format!("the viewer's pipe failed: {e}")),
        Err(_) => Err("no answer: the viewer did not start".into()),
    }
}

#[cfg(all(feature = "viewer", not(feature = "vnc-viewer")))]
fn check_viewer(_: &mut Worker) -> Result<(), String> {
    // RDP's viewer only speaks once a server does; starting is the check.
    Ok(())
}

/// What the process runs in, and whether that is everything this check
/// asks for.
fn describe(w: &Worker) -> (String, bool) {
    if !w.sandboxed {
        return ("not sandboxed".into(), false);
    }
    let active = w.active();
    let mut on = Vec::new();
    let mut off = Vec::new();
    for (yes, name) in [
        (active.no_win32k, "Win32k lockdown"),
        (active.cfg, "Control Flow Guard"),
        (active.no_dynamic_code, "no dynamic code"),
    ] {
        if yes { on.push(name) } else { off.push(name) }
    }
    if w.hardening != Hardening::None {
        on.push("the other process mitigations");
    }
    let mut text = format!("AppContainer and job limits, with {}", on.join(", "));
    if !off.is_empty() {
        text.push_str(&format!("; WITHOUT {}", off.join(", ")));
    }
    text.push_str(if active.shadow_stacks {
        "; CET shadow stacks on"
    } else {
        "; CET shadow stacks off (this CPU or Windows lacks them)"
    });
    let strong = w.hardening <= Hardening::NoShadowStacks && off.is_empty();
    (text, strong)
}

/// Runs the check, prints what it found and returns the exit code.
pub fn run() -> i32 {
    let mut report = Vec::new();
    let mut failed = false;
    let mut strong = true;
    let probes = [
        ("127.0.0.1", "a connection to 127.0.0.1 fails as it should"),
        (
            "ferossh-check.invalid",
            "looking up a name that does not exist fails as it should",
        ),
    ];
    for (i, (host, expected)) in probes.into_iter().enumerate() {
        match sandbox::spawn_worker(true) {
            Ok(mut w) => {
                let (what, full) = describe(&w);
                strong &= full;
                match check_worker(&mut w, host) {
                    Ok(reason) if i == 0 => report.push(format!(
                        "SSH worker: runs, in {what}.\n  Network: {expected}: {reason}"
                    )),
                    Ok(reason) => report.push(format!("  Network: {expected}: {reason}")),
                    Err(e) => {
                        report.push(format!("SSH worker: FAILED ({e}), in {what}."));
                        failed = true;
                    }
                }
            }
            Err(e) => {
                report.push(format!("SSH worker: could not start ({})", e.message()));
                failed = true;
            }
        }
    }
    #[cfg(feature = "viewer")]
    match sandbox::spawn_viewer() {
        Ok(mut v) => {
            let (what, full) = describe(&v);
            strong &= full;
            match check_viewer(&mut v) {
                Ok(()) => report.push(format!("Desktop viewer: runs, in {what}.")),
                Err(e) => {
                    report.push(format!("Desktop viewer: FAILED ({e}), in {what}."));
                    failed = true;
                }
            }
        }
        Err(e) => {
            report.push(format!("Desktop viewer: could not start ({})", e.message()));
            failed = true;
        }
    }
    let mut out = std::io::stdout();
    for line in &report {
        let _ = writeln!(out, "{line}");
    }
    let _ = out.flush();
    if failed {
        1
    } else if strong {
        0
    } else {
        2
    }
}
