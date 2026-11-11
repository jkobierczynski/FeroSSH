//! Talks to a real `mosh-server` on this machine (started directly, as
//! the SSH bootstrap would start it): the screen it sends comes through,
//! typed commands run, and the session ends cleanly both ways. Skipped
//! when `mosh-server` is not installed, unless FEROSSH_REQUIRE_MOSH is
//! set (as in CI).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::net::UdpSocket;
use std::process::Command;
use std::time::{Duration, Instant};

use ferossh_mosh::bootstrap::parse_connect;
use ferossh_mosh::client::Client;
use ferossh_mosh::predict::Mode;
use ferossh_mosh::screen::{Rendition, Screen};

fn start(shell: &str) -> Option<(u16, [u8; 16])> {
    let out = Command::new("mosh-server")
        .args([
            "new",
            "-i",
            "127.0.0.1",
            "-c",
            "256",
            "-l",
            "LANG=C.UTF-8",
            "--",
            "/bin/sh",
            "-c",
            shell,
        ])
        .env("MOSH_SERVER_NETWORK_TMOUT", "30")
        .output();
    let out = match out {
        Ok(o) => o,
        Err(_) if std::env::var_os("FEROSSH_REQUIRE_MOSH").is_none() => {
            eprintln!("mosh-server not installed; skipping");
            return None;
        }
        Err(e) => panic!("mosh-server: {e}"),
    };
    let mut all = out.stdout.clone();
    all.extend(&out.stderr);
    let c = parse_connect(&all).expect("MOSH CONNECT");
    Some((c.port, *c.key))
}

struct Run {
    client: Client,
    /// Drop this share of datagrams each way (0 to 100).
    loss: u64,
    rng: u64,
    sock: UdpSocket,
    shown: Screen,
    /// Every frame's output so far.
    frames: String,
    t0: Instant,
}

impl Run {
    fn now(&self) -> u64 {
        self.t0.elapsed().as_millis() as u64
    }

    /// Runs the protocol until `done` or the deadline.
    fn until(&mut self, secs: u64, mut done: impl FnMut(&mut Run) -> bool) -> bool {
        let end = Instant::now() + Duration::from_secs(secs);
        let mut buf = [0u8; 2048];
        while Instant::now() < end {
            let now = self.now();
            for d in self.client.tick(now).unwrap() {
                if !self.lose() {
                    self.sock.send(&d).unwrap();
                }
            }
            let wait = self.client.wait(self.now()).clamp(1, 50);
            self.sock
                .set_read_timeout(Some(Duration::from_millis(wait)))
                .unwrap();
            if let Ok(n) = self.sock.recv(&mut buf)
                && !self.lose()
            {
                let now = self.now();
                self.client.receive(&buf[..n], now).unwrap();
            }
            if let Some(out) = self.client.frame(self.now()) {
                self.shown.apply(out.as_bytes());
                self.frames.push_str(&out);
            }
            if done(self) {
                return true;
            }
        }
        false
    }

    fn lose(&mut self) -> bool {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        self.rng % 100 < self.loss
    }

    /// The lines of `line N` output on screen, in order.
    fn numbered(&self) -> Vec<u32> {
        self.screen_text()
            .lines()
            .filter_map(|l| l.trim().strip_prefix("line ")?.parse().ok())
            .collect()
    }

    fn screen_text(&self) -> String {
        self.shown
            .rows
            .iter()
            .map(|r| r.cells.iter().map(|c| c.text.as_str()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn run(port: u16, key: [u8; 16]) -> Run {
    run_lossy(port, key, 0)
}

fn run_lossy(port: u16, key: [u8; 16], loss: u64) -> Run {
    let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
    sock.connect(("127.0.0.1", port)).unwrap();
    Run {
        client: Client::new(&key, false, 80, 24, 7, 0),
        loss,
        rng: 0x9E37_79B9_7F4A_7C15,
        sock,
        shown: Screen::new(80, 24),
        frames: String::new(),
        t0: Instant::now(),
    }
}

#[test]
fn types_into_a_real_server_and_sees_the_screen() {
    let Some((port, key)) = start("PS1='ready> ' exec /bin/sh -i") else {
        return;
    };
    let mut r = run(port, key);
    assert!(
        r.until(10, |r| r.screen_text().contains("ready>")),
        "no prompt: {}",
        r.screen_text()
    );
    r.client.keys(b"echo $((6*7))mosh\r");
    assert!(
        r.until(10, |r| r.screen_text().contains("42mosh")),
        "no output: {}",
        r.screen_text()
    );
    // Lots of output scrolls, and the pane ends up the same as the
    // server's screen.
    r.client
        .keys(b"i=0; while [ $i -lt 300 ]; do echo line $i; i=$((i+1)); done\r");
    assert!(
        r.until(15, |r| r.screen_text().contains("line 299")),
        "no scroll: {}",
        r.screen_text()
    );
    let lines = r.numbered();
    assert!(lines.len() >= 20, "{}", r.screen_text());
    assert!(lines.windows(2).all(|w| w[1] == w[0] + 1), "{lines:?}");
    // Resizing: the server's screen follows.
    // The pane changes size first; the client tells the server.
    r.shown.resize(100, 30);
    r.client.resize(100, 30);
    r.client.keys(b"stty size\r");
    assert!(
        r.until(10, |r| r.screen_text().contains("30 100")),
        "no resize: {}",
        r.screen_text()
    );
    // The shell exits: the server ends the session and we acknowledge.
    r.client.keys(b"exit\r");
    assert!(r.until(10, |r| r.client.server_ended()), "no shutdown");
}

/// With local echo always on, typed text that the server echoes ends up
/// exactly as the server shows it, and text it hides is never drawn.
#[test]
fn local_echo_shows_only_what_the_server_would() {
    let Some((port, key)) = start("PS1='ready> ' exec bash --norc --noprofile -i") else {
        return;
    };
    let mut r = run(port, key);
    r.client.set_prediction(Mode::Always);
    assert!(r.until(10, |r| r.screen_text().contains("ready>")));
    // Half the packets lost: echoes come late, and guesses show.
    r.loss = 50;
    let mut guessed = false;
    for c in b"echo prex\x7fdic\x1b[D\x1b[Ct" {
        r.client.keys(&[*c]);
        r.until(0, |_| true);
        r.until(1, |r| r.client.predicting());
        guessed |= r.client.predicting();
    }
    r.loss = 0;
    assert!(guessed, "no guess was shown");
    r.client.keys(b"ed\r");
    assert!(
        r.until(10, |r| r.screen_text().contains("\npredicted")),
        "{}",
        r.screen_text()
    );
    // Nothing guessed is left over: the pane is the server's screen.
    assert!(r.until(5, |r| r.client.frame(r.now()).is_none()
        && !r.screen_text().contains("ed\n\nready")));
    let lines: Vec<String> = r
        .screen_text()
        .lines()
        .map(|l| l.trim_end().to_owned())
        .collect();
    assert!(
        lines.iter().any(|l| l == "ready> echo predicted"),
        "{lines:?}"
    );
    // A password typed with echo off is never drawn.
    r.frames.clear();
    r.client
        .keys(b"stty -echo; read secret; stty echo; echo done\r");
    assert!(r.until(10, |r| r.frames.contains("echo done")));
    for c in b"hunter2" {
        r.client.keys(&[*c]);
        r.until(0, |_| true);
    }
    r.until(1, |_| false);
    r.client.keys(b"\r");
    assert!(
        r.until(10, |r| r.screen_text().contains("\ndone")),
        "{}",
        r.screen_text()
    );
    assert!(!r.frames.contains("unter"), "{:?}", r.frames);
}

#[test]
fn closing_the_tab_ends_the_server() {
    let Some((port, key)) = start("exec /bin/sh -i") else {
        return;
    };
    let mut r = run(port, key);
    assert!(r.until(10, |r| r.client.heard() && r.client.remote_num() > 0));
    let now = r.now();
    r.client.start_shutdown(now);
    assert!(
        r.until(10, |r| r.client.shutdown_acknowledged()),
        "server did not acknowledge"
    );
}

/// With a third of the packets lost each way, diffs arrive from states
/// the pane has moved past; the pane still ends up as the server's.
#[test]
fn survives_packet_loss() {
    let Some((port, key)) = start("PS1='ready> ' exec /bin/sh -i") else {
        return;
    };
    let mut r = run_lossy(port, key, 33);
    assert!(r.until(20, |r| r.screen_text().contains("ready>")));
    for round in 0..5 {
        r.client.keys(
            format!(
                "i=0; while [ $i -lt 60 ]; do echo line $((i+{})); i=$((i+1)); done\r",
                round * 100
            )
            .as_bytes(),
        );
        let want = format!("line {}", round * 100 + 59);
        assert!(
            r.until(30, |r| r.screen_text().contains(&want)),
            "round {round}: {}",
            r.screen_text()
        );
    }
    r.loss = 0;
    assert!(r.until(5, |r| r.screen_text().matches("ready>").count() >= 1));
    let lines = r.numbered();
    assert!(
        lines.windows(2).all(|w| w[1] == w[0] + 1),
        "{lines:?}\n{}",
        r.screen_text()
    );
}

/// Random terminal output, run through the real server's emulator and
/// renderer, leaves the pane as reading that output directly does: the
/// two emulators agree.
#[test]
fn random_output_reads_as_the_server_reads_it() {
    let rounds: u64 = std::env::var("MOSH_SERVER_ROUNDS")
        .ok()
        .and_then(|r| r.parse().ok())
        .unwrap_or(4);
    for seed in 1..=rounds {
        let script = random_output(seed);
        let dir = std::env::temp_dir().join(format!("ferossh-mosh-{}-{seed}", std::process::id()));
        std::fs::write(&dir, &script).unwrap();
        let cmd = format!("stty -opost -echo; cat '{}'; exec sleep 60", dir.display());
        let Some((port, key)) = start(&cmd) else {
            return;
        };
        let mut want = Screen::new(80, 24);
        want.apply(&script);
        let mut r = run(port, key);
        let mut why = String::new();
        let same = r.until(10, |r| match differs(&r.shown, &want) {
            Some(w) => {
                why = w;
                false
            }
            None => true,
        });
        let _ = std::fs::remove_file(&dir);
        assert!(
            same,
            "seed {seed}: {why}\nscript {:?}",
            String::from_utf8_lossy(&script)
        );
    }
}

fn differs(a: &Screen, b: &Screen) -> Option<String> {
    // Blank, a space and a no-break space look the same, and a lone
    // combining character is drawn on a no-break space.
    let norm = |t: &str| -> String {
        let t = t.trim_start_matches('\u{a0}');
        if t == " " {
            String::new()
        } else {
            t.to_owned()
        }
    };
    for y in 0..b.height() {
        for x in 0..b.width() {
            let (p, q) = (a.cell(y, x)?, b.cell(y, x)?);
            // Right halves, and a wide character squeezed into the last
            // column (no terminal can draw it), are left out.
            if x > 0 && b.cell(y, x - 1).is_some_and(|c| c.wide) || q.wide && x + 1 == b.width() {
                continue;
            }
            if norm(&p.text) != norm(&q.text) || !same_look(p.rend, q.rend) || p.wide != q.wide {
                return Some(format!("cell {y},{x}: {p:?} != {q:?}"));
            }
        }
    }
    let (p, q) = (&a.ds, &b.ds);
    if (p.cursor_row, p.cursor_col) != (q.cursor_row, q.cursor_col) {
        return Some(format!(
            "cursor {:?} != {:?}",
            (p.cursor_row, p.cursor_col),
            (q.cursor_row, q.cursor_col)
        ));
    }
    if !same_look(p.rend, q.rend) || p.cursor_visible != q.cursor_visible {
        return Some(format!("modes {p:?} != {q:?}"));
    }
    None
}

/// Mosh has no faint text (SGR 2): the server drops it.
fn same_look(a: Rendition, b: Rendition) -> bool {
    let faint = 1 << 1;
    Rendition {
        attrs: a.attrs & !faint,
        ..a
    } == Rendition {
        attrs: b.attrs & !faint,
        ..b
    }
}

fn random_output(seed: u64) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut rnd = |n: u64| {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x % n
    };
    // From the top left (the server's screen may not start there).
    let mut out = String::from("\x1b[H");
    // Mosh's renderer draws a wide character squeezed into the last
    // column (autowrap off) where the terminal wraps it: none while off.
    let mut wrap = true;
    for _ in 0..400 {
        match rnd(24) {
            0..=7 => {
                for _ in 0..rnd(30) {
                    out.push(char::from(b' ' + rnd(95) as u8));
                }
            }
            8 => out.push_str(["é", "ü", "漢", "字", "😀"][rnd(if wrap { 5 } else { 2 }) as usize]),
            9 => out.push_str(["e\u{301}", "a\u{308}"][rnd(2) as usize]),
            10 => out.push_str("\r\n"),
            11 => out.push(['\r', '\n', '\x08', '\t'][rnd(4) as usize]),
            12 => out.push_str(&format!("\x1b[{};{}H", rnd(26), rnd(82))),
            13 => out.push_str(&format!(
                "\x1b[{}{}",
                rnd(3),
                ["J", "K", "X", "@", "P", "L", "M"][rnd(7) as usize]
            )),
            14 => out.push_str(&format!(
                "\x1b[{};{};{}m",
                rnd(10),
                [31, 42, 93, 104, 39, 49, 22, 27][rnd(8) as usize],
                rnd(2)
            )),
            15 => out.push_str(&format!(
                "\x1b[38;5;{}m\x1b[48;2;{};2;3m",
                rnd(256),
                rnd(256)
            )),
            16 => out.push_str(&format!("\x1b[{};{}r", 1 + rnd(24), 1 + rnd(24))),
            17 => out.push_str("\x1b[r"),
            18 => out.push_str(["\x1b[?25l", "\x1b[?25h", "\x1b[4h", "\x1b[4l"][rnd(4) as usize]),
            19 => out.push_str(["\x1b7", "\x1b8", "\x1bM", "\x1bD", "\x1bE"][rnd(5) as usize]),
            20 => out.push_str(&format!(
                "\x1b[{}{}",
                rnd(4),
                ["S", "T", "A", "B", "C", "D", "G", "d"][rnd(8) as usize]
            )),
            21 => {
                wrap = !wrap;
                out.push_str(if wrap { "\x1b[?7h" } else { "\x1b[?7l" });
            }
            // A wide character in the last column.
            22 if wrap => out.push_str("\x1b[80G漢"),
            _ => out.push_str("\x1b[0m"),
        }
    }
    out.into_bytes()
}
