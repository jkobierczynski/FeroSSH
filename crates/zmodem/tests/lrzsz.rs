//! The receiver and sender against `lrzsz`'s `sz` and `rz`, over pipes,
//! and against each other with damage on the line. The `lrzsz` tests skip
//! when it is not installed, unless `FEROSSH_REQUIRE_LRZSZ` is set (CI
//! sets it).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use ferossh_zmodem::{Detector, Receiver, RxEvent, Sender, Start, TxEvent, Upload};

fn have(tool: &str) -> bool {
    let found = Command::new(tool)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok();
    if !found && std::env::var_os("FEROSSH_REQUIRE_LRZSZ").is_some() {
        panic!("{tool} is not installed, and FEROSSH_REQUIRE_LRZSZ is set");
    }
    if !found {
        eprintln!("skipping: {tool} is not installed");
    }
    found
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ferossh-zmodem-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Pseudo-random bytes with every byte value, ZDLE runs and CRs after `@`.
fn content(len: usize, seed: u32) -> Vec<u8> {
    let mut x = seed.wrapping_mul(2_654_435_761).max(1);
    let mut out: Vec<u8> = (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as u8
        })
        .collect();
    for (i, chunk) in out.chunks_mut(997).enumerate() {
        if let [a, b, c, d, ..] = chunk {
            (*a, *b, *c, *d) = if i % 2 == 0 {
                (0x18, 0x18, 0x18, 0x18)
            } else {
                (b'@', b'\r', 0x11, 0x93)
            };
        }
    }
    out
}

/// A child process with its output arriving on a channel.
struct Peer {
    child: Child,
    output: mpsc::Receiver<Vec<u8>>,
}

impl Peer {
    fn spawn(cmd: &mut Command) -> Self {
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let (tx, output) = mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = stdout.read(&mut buf) {
                if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
        Self { child, output }
    }

    fn write(&mut self, bytes: &[u8]) {
        if !bytes.is_empty() {
            let stdin = self.child.stdin.as_mut().unwrap();
            let _ = stdin.write_all(bytes);
            let _ = stdin.flush();
        }
    }

    /// Output until the transfer starts, through the detector.
    fn start(&mut self) -> (Start, Vec<u8>) {
        let mut d = Detector::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let chunk = self
                .output
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("no ZMODEM start");
            let (_, found) = d.scan(&chunk);
            if let Some(found) = found {
                return (found.start, found.rest);
            }
        }
    }
}

/// Receives what `sz args` sends, accepting every file; returns the files.
fn receive_from_sz(args: &[&str], files: &[PathBuf]) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut sz = Peer::spawn(Command::new("sz").args(args).args(files));
    let (start, rest) = sz.start();
    assert_eq!(start, Start::Receive);
    let mut rx = Receiver::new();
    let mut got: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    let mut pending = rest;
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        rx.feed(&pending);
        while let Some(event) = rx.next_event() {
            match event {
                RxEvent::File(info) => {
                    got.push((info.name, Vec::new()));
                    rx.accept();
                }
                RxEvent::Data(bytes) => got.last_mut().unwrap().1.extend(bytes),
                RxEvent::FileDone | RxEvent::Done => {}
                RxEvent::Failed(why) => panic!("receive failed: {why}"),
            }
        }
        sz.write(&rx.take_output());
        if rx.ended() {
            break;
        }
        assert!(Instant::now() < deadline, "sz took too long");
        pending = match sz
            .output
            .recv_timeout(rx.patience().unwrap_or(Duration::from_secs(1)))
        {
            Ok(bytes) => bytes,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                rx.timeout();
                Vec::new()
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                // sz exits right after its "OO".
                rx.timeout();
                Vec::new()
            }
        };
    }
    let status = sz.child.wait().unwrap();
    assert!(status.success(), "sz exited with {status}");
    got
}

/// Sends `files` to `rz args` running in `dir`.
fn send_to_rz(args: &[&str], dir: &Path, files: &[(String, Vec<u8>)]) {
    let mut rz = Peer::spawn(Command::new("rz").args(args).current_dir(dir));
    let (start, rest) = rz.start();
    assert_eq!(start, Start::Send);
    let uploads = files
        .iter()
        .map(|(name, data)| Upload {
            name: name.clone().into_bytes(),
            size: data.len() as u64,
            mtime: 1_700_000_000,
        })
        .collect();
    let mut tx = Sender::new(uploads);
    let mut pending = rest;
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        tx.feed(&pending);
        while let Some(event) = tx.next_event() {
            match event {
                TxEvent::Read { file, offset, len } => {
                    let data = &files[file as usize].1;
                    let start = offset as usize;
                    let end = (start + len as usize).min(data.len());
                    tx.provide(offset, &data[start..end]);
                }
                TxEvent::FileDone { skipped } => assert!(!skipped),
                TxEvent::Done => {}
                TxEvent::Failed(why) => panic!("send failed: {why}"),
            }
        }
        rz.write(&tx.take_output());
        if tx.ended() {
            break;
        }
        assert!(Instant::now() < deadline, "rz took too long");
        pending = match rz
            .output
            .recv_timeout(tx.patience().unwrap_or(Duration::from_secs(1)))
        {
            Ok(bytes) => bytes,
            Err(_) => {
                tx.timeout();
                Vec::new()
            }
        };
    }
    let status = rz.child.wait().unwrap();
    assert!(status.success(), "rz exited with {status}");
}

#[test]
fn receives_from_sz() {
    if !have("sz") {
        return;
    }
    let dir = temp_dir("sz");
    let a = content(200_000, 1);
    let b = content(0, 2);
    let c = content(1023, 3);
    let paths: Vec<PathBuf> = [("a.bin", &a), ("empty", &b), ("c.txt", &c)]
        .iter()
        .map(|(name, data)| {
            let p = dir.join(name);
            std::fs::write(&p, data).unwrap();
            p
        })
        .collect();
    // Plain; escaping every control character; 8 KiB blocks.
    for args in [&[][..], &["-e"], &["-8"]] {
        let got = receive_from_sz(args, &paths);
        let names: Vec<&[u8]> = got.iter().map(|(n, _)| n.as_slice()).collect();
        assert_eq!(names, [&b"a.bin"[..], b"empty", b"c.txt"], "{args:?}");
        assert!(got[0].1 == a, "{args:?}: a.bin differs");
        assert!(got[1].1.is_empty());
        assert!(got[2].1 == c, "{args:?}: c.txt differs");
    }
}

#[test]
fn sends_to_rz() {
    if !have("rz") {
        return;
    }
    let files = vec![
        ("big.bin".to_owned(), content(300_001, 7)),
        ("zero".to_owned(), Vec::new()),
        ("notes.txt".to_owned(), b"line one\r\nline two\n".to_vec()),
    ];
    for (i, args) in [&[][..], &["-e"]].into_iter().enumerate() {
        let dir = temp_dir(&format!("rz{i}"));
        send_to_rz(args, &dir, &files);
        for (name, data) in &files {
            let written = std::fs::read(dir.join(name)).unwrap();
            assert!(&written == data, "{args:?}: {name} differs");
        }
    }
}

/// Our sender and receiver, with the line between them damaging bytes;
/// the receiver asks again and the file still arrives whole.
#[test]
fn recovers_from_damage() {
    let data = content(100_000, 9);
    let mut tx = Sender::new(vec![Upload {
        name: b"f".to_vec(),
        size: data.len() as u64,
        mtime: 0,
    }]);
    let mut rx = Receiver::new();
    let mut got = Vec::new();
    let mut to_rx: Vec<u8> = Vec::new();
    let mut sent = 0usize;
    let mut from_rx = Vec::new();
    for _round in 0..10_000 {
        let quiet = from_rx.is_empty();
        tx.feed(&std::mem::take(&mut from_rx));
        while let Some(event) = tx.next_event() {
            if let TxEvent::Read { offset, len, .. } = event {
                let start = offset as usize;
                let end = (start + len as usize).min(data.len());
                tx.provide(offset, &data[start..end]);
            }
        }
        let mut wire = tx.take_output();
        // Damage a byte now and then (but not the first headers).
        for b in &mut wire {
            sent += 1;
            if sent > 400 && sent.is_multiple_of(20_011) {
                *b ^= 0x5a;
            }
        }
        let stalled = quiet && wire.is_empty();
        to_rx.extend(wire);
        rx.feed(&std::mem::take(&mut to_rx));
        while let Some(event) = rx.next_event() {
            match event {
                RxEvent::File(_) => rx.accept(),
                RxEvent::Data(bytes) => got.extend(bytes),
                RxEvent::Failed(why) => panic!("{why}"),
                _ => {}
            }
        }
        from_rx = rx.take_output();
        if tx.ended() && rx.ended() {
            break;
        }
        if stalled && from_rx.is_empty() {
            // Nothing moves: both sides' timers run out.
            rx.timeout();
            tx.timeout();
            from_rx = rx.take_output();
        }
    }
    assert!(tx.ended() && rx.ended());
    assert!(
        got == data,
        "received {} of {} bytes",
        got.len(),
        data.len()
    );
}
