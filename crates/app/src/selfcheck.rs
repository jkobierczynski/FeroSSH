//! `ferossh.exe --check-sandbox`: starts the sandboxed processes the way
//! the window does and checks that they run. The CI's Windows job runs it
//! on the release exe, and a user can run it to see what this Windows
//! allows (`ferossh.exe --check-sandbox | more`; the exe has no console of
//! its own).
//!
//! The worker must answer the greeting and fail a connection to
//! 127.0.0.1, which an AppContainer cannot reach, with a reason: that runs
//! its tokio runtime and sockets under the job and the mitigations. A
//! VNC viewer must answer a server's version line.
//!
//! Exit code 0: everything ran sandboxed with mitigations; 2: it ran, but
//! without the mitigations (an older Windows); 1: something failed.

use std::io::{Read, Write};
use std::sync::mpsc;
use std::time::Duration;

use ferossh_ipc::{Decoder, Message, PROTOCOL_VERSION, Timing, encode};

use crate::sandbox::{self, Worker};

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

fn check_worker(w: &mut Worker) -> Result<String, String> {
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
            host: "127.0.0.1".into(),
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

fn describe(w: &Worker) -> &'static str {
    match (w.sandboxed, w.mitigated) {
        (true, true) => "AppContainer, job limits and process mitigations",
        (true, false) => "AppContainer and job limits, WITHOUT process mitigations",
        _ => "not sandboxed",
    }
}

/// Runs the check, prints what it found and returns the exit code.
pub fn run() -> i32 {
    let mut report = Vec::new();
    let mut failed = false;
    let mut mitigated = true;
    match sandbox::spawn_worker(true) {
        Ok(mut w) => {
            mitigated &= w.mitigated;
            match check_worker(&mut w) {
                Ok(reason) => report.push(format!(
                    "SSH worker: runs, in {}. Loopback is blocked as it should be: {reason}",
                    describe(&w)
                )),
                Err(e) => {
                    report.push(format!("SSH worker: FAILED ({e}), in {}.", describe(&w)));
                    failed = true;
                }
            }
        }
        Err(e) => {
            report.push(format!("SSH worker: could not start ({})", e.message()));
            failed = true;
        }
    }
    #[cfg(feature = "viewer")]
    match sandbox::spawn_viewer() {
        Ok(mut v) => {
            mitigated &= v.mitigated;
            match check_viewer(&mut v) {
                Ok(()) => report.push(format!("Desktop viewer: runs, in {}.", describe(&v))),
                Err(e) => {
                    report.push(format!(
                        "Desktop viewer: FAILED ({e}), in {}.",
                        describe(&v)
                    ));
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
    } else if mitigated {
        0
    } else {
        2
    }
}
