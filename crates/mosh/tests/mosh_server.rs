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
use ferossh_mosh::screen::Screen;

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
