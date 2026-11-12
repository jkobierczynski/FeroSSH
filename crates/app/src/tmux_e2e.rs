//! [`Tmux`] against a real tmux: attach, split, type, save the layout,
//! attach again from a fresh controller (another PC), switch layout mode,
//! and detach. tmux runs as `tmux -C` over pipes, which speaks the same
//! protocol as `-CC` without the terminal, so the test adds the DCS that
//! `-CC` would write. Skipped where tmux is missing.
//!
//! With `FEROSSH_TMUX_CONTAINER` set to a running container (one of the
//! `interop/` images), tmux runs inside it instead, which is how CI covers
//! other tmux versions.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use ferossh_tmuxcc::saved::Look;
use ferossh_tmuxcc::{MIN_VERSION, Version, WINDOW_SIZES_VERSION};

use crate::layout::{Dir, PaneId as Native, Workspace};
use crate::tmux::{Action, Mode, Tmux};

/// One tmux server for the test, on its own socket.
struct Server {
    socket: String,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = tmux().args(["-L", &self.socket, "kill-server"]).status();
    }
}

/// A control client and what the window would hold.
struct Harness {
    child: Child,
    stdin: ChildStdin,
    output: Receiver<Vec<u8>>,
    tmux: Tmux,
    ws: Workspace,
    /// Each native pane's size and everything fed to it.
    panes: BTreeMap<Native, ((u16, u16), Vec<u8>)>,
    gateway_shown: bool,
    /// The session names the saved looks gave new panes.
    names: BTreeMap<Native, String>,
    pickers: Vec<Vec<String>>,
    notices: Vec<String>,
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// `tmux`, here or in `FEROSSH_TMUX_CONTAINER`, with the shell the tests
/// expect.
fn tmux() -> Command {
    match std::env::var("FEROSSH_TMUX_CONTAINER") {
        Ok(id) if !id.is_empty() => {
            let mut c = Command::new("docker");
            c.args([
                "exec",
                "-i",
                "-e",
                "SHELL=/bin/sh",
                "-e",
                "PS1=$ ",
                &id,
                "tmux",
            ]);
            c
        }
        _ => {
            let mut c = Command::new("tmux");
            c.env("SHELL", "/bin/sh").env("PS1", "$ ");
            c
        }
    }
}

fn tmux_available() -> bool {
    let found = tmux().arg("-V").output().is_ok_and(|o| o.status.success());
    if !found {
        assert!(
            std::env::var_os("FEROSSH_REQUIRE_TMUX").is_none(),
            "FEROSSH_REQUIRE_TMUX is set but tmux is missing"
        );
        eprintln!("tmux not found; skipping");
    }
    found
}

impl Harness {
    fn attach(server: &Server, preferred: Mode) -> Self {
        let mut child = tmux()
            .args(["-L", &server.socket, "-f", "/dev/null", "-C"])
            .args(["new-session", "-A", "-s", "e2e", "-x", "80", "-y", "24"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let (tx, output) = channel();
        std::thread::spawn(move || {
            let mut buf = [0u8; 65536];
            while let Ok(n) = stdout.read(&mut buf) {
                if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
        let (ws, gateway) = Workspace::new();
        let mut h = Self {
            child,
            stdin,
            output,
            tmux: Tmux::new(gateway, preferred, 1000),
            ws,
            panes: BTreeMap::new(),
            gateway_shown: false,
            names: BTreeMap::new(),
            pickers: Vec::new(),
            notices: Vec::new(),
        };
        // What `tmux -CC` writes before its first line.
        let actions = h.tmux.feed(&mut h.ws, b"\x1bP1000p");
        h.apply(actions);
        h
    }

    fn apply(&mut self, actions: Vec<Action>) {
        for a in actions {
            match a {
                Action::Write(bytes) => {
                    let _ = self.stdin.write_all(&bytes);
                }
                Action::Create {
                    native,
                    cols,
                    rows,
                    look,
                    ..
                } => {
                    self.panes.insert(native, ((cols, rows), Vec::new()));
                    if let Some(name) = look.name {
                        self.names.insert(native, name);
                    }
                }
                Action::Feed { native, bytes } => {
                    if let Some(p) = self.panes.get_mut(&native) {
                        p.1.extend_from_slice(&bytes);
                    }
                }
                Action::Resize { native, cols, rows } => {
                    if let Some(p) = self.panes.get_mut(&native) {
                        p.0 = (cols, rows);
                    }
                }
                Action::Remove { native } => {
                    self.panes.remove(&native);
                }
                Action::ShowGateway => self.gateway_shown = true,
                Action::Picker(names) => self.pickers.push(names),
                Action::Notice(text) => self.notices.push(text),
                _ => {}
            }
        }
        let _ = self.stdin.flush();
    }

    /// Feeds tmux's output until `done` holds.
    fn until(&mut self, what: &str, mut done: impl FnMut(&Self) -> bool) {
        let end = Instant::now() + Duration::from_secs(10);
        while !done(self) {
            let left = end.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "timed out waiting for {what}");
            match self
                .output
                .recv_timeout(left.min(Duration::from_millis(100)))
            {
                Ok(bytes) => {
                    let actions = self.tmux.feed(&mut self.ws, &bytes);
                    self.apply(actions);
                }
                // `tmux -C` ends without the string terminator `-CC`
                // writes; add it.
                Err(RecvTimeoutError::Disconnected) if self.tmux.active() => {
                    let actions = self.tmux.feed(&mut self.ws, b"\x1b\\");
                    self.apply(actions);
                }
                Err(_) => {}
            }
        }
    }

    /// Pane size reports as the window's layout pass would make them.
    fn sizes(&mut self, cells: impl Fn(Native) -> (u16, u16)) {
        let actions = self.tmux.sync_sizes(&self.ws, &|n| Some(cells(n)));
        self.apply(actions);
    }

    fn save(&mut self, look: impl Fn(Native) -> Look) {
        let actions = self.tmux.sync_saved(&self.ws, &look);
        self.apply(actions);
    }

    fn text(&self, native: Native) -> String {
        String::from_utf8_lossy(&self.panes[&native].1).into_owned()
    }

    fn shown(&self) -> usize {
        self.ws.all_panes().len()
    }
}

fn tmux_cmd(server: &Server, args: &[&str]) -> String {
    let out = tmux()
        .args(["-L", &server.socket])
        .args(args)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// The tmux under test, from `tmux -V` ("tmux 3.2a").
fn version() -> Version {
    let out = tmux().arg("-V").output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    Version::parse(text.trim().trim_start_matches("tmux ")).unwrap()
}

#[test]
fn real_tmux_round_trip() {
    if !tmux_available() {
        return;
    }
    if version() < WINDOW_SIZES_VERSION {
        eprintln!(
            "tmux {} has no per-window sizes; see older_tmux_*",
            version()
        );
        return;
    }
    let server = Server {
        socket: format!("ferossh-e2e-{}", std::process::id()),
    };
    let mut a = Harness::attach(&server, Mode::Ferossh);
    a.until("the first pane", |h| h.shown() == 1 && !h.panes.is_empty());
    let first = a.ws.all_panes()[0];
    assert_ne!(first, a.tmux.gateway);
    assert_eq!(a.tmux.mode(), Mode::Ferossh);
    assert_eq!(
        tmux_cmd(
            &server,
            &["show-options", "-v", "-t", "e2e", "@ferossh-mode"]
        ),
        "ferossh"
    );

    // Each native pane sizes its own window.
    a.sizes(|_| (100, 30));
    a.until("the new size", |h| h.panes[&first].0 == (100, 30));

    // Typing reaches the shell, and its output comes back.
    let actions = a.tmux.input(first, b"echo one-$((40+2))\r");
    a.apply(actions);
    a.until("the echo", |h| h.text(first).contains("one-42"));

    // A native split is a new tmux window beside the pane.
    let actions = a.tmux.split(first, Dir::Right);
    a.apply(actions);
    a.until("the split", |h| h.shown() == 2);
    let second = a.ws.focused();
    assert_ne!(second, first);
    assert_eq!(a.ws.tabs.len(), 1);
    a.sizes(|n| if n == first { (60, 30) } else { (40, 30) });
    a.until("both sizes", |h| {
        h.panes[&first].0 == (60, 30) && h.panes[&second].0 == (40, 30)
    });
    let actions = a.tmux.input(second, b"echo two\r");
    a.apply(actions);
    a.until("the second echo", |h| h.text(second).contains("two"));
    // A program like htop or vim turns on application cursor keys and
    // keypad, and click reports in SGR form.
    let actions = a.tmux.input(
        second,
        b"printf '\\033[?1h\\033=\\033[?1000h\\033[?1006h'\r",
    );
    a.apply(actions);
    a.until("the mouse modes", |_| {
        tmux_cmd(
            &server,
            &["list-panes", "-s", "-t", "e2e", "-F", "#{mouse_sgr_flag}"],
        )
        .contains('1')
    });
    // The second session gets a name: its window takes it in tmux.
    let actions = a.tmux.rename(second, Some("db one"));
    a.apply(actions);
    a.until("the window name", |_| {
        tmux_cmd(
            &server,
            &["list-windows", "-t", "e2e", "-F", "#{window_name}"],
        )
        .lines()
        .any(|l| l == "db one")
    });
    a.save(|n| Look {
        font: Some("Consolas".into()),
        size: Some(if n == first { 10.0 } else { 18.0 }),
        theme: Some("Nord".into()),
        name: (n == second).then(|| "db one".to_owned()),
    });
    a.until("the saved layout", |_| {
        tmux_cmd(
            &server,
            &["show-options", "-v", "-t", "e2e", "@ferossh-layout"],
        )
        .contains("tab=r500(")
    });

    // Another PC attaches: same tab, same split, history back.
    let mut b = Harness::attach(&server, Mode::Ferossh);
    b.until("the restored panes", |h| {
        h.shown() == 2 && h.panes.values().all(|p| !p.1.is_empty())
    });
    assert_eq!(b.ws.tabs.len(), 1);
    let restored = b.ws.all_panes();
    assert!(b.text(restored[0]).contains("one-42"));
    assert!(b.text(restored[1]).contains("two"));
    // The name comes back with the pane, and only there.
    assert_eq!(
        b.names.iter().collect::<Vec<_>>(),
        [(&restored[1], &"db one".to_owned())]
    );
    // The same mouse modes come back, and no motion reports (1003),
    // which would type garbage at the shell once htop is gone.
    let modes = b.text(restored[1]);
    assert!(modes.contains("\x1b[?1000h\x1b[?1006h"), "{modes:?}");
    assert!(!modes.contains("\x1b[?1003h"), "{modes:?}");
    // Arrow keys go out as vim asked for them (ESC O A, not ESC [ A).
    let mut term = ferossh_term::Terminal::new(40, 30, &ferossh_term::Settings::default());
    term.feed(modes.as_bytes());
    assert_eq!(
        term.key(ferossh_term::Key::Up, ferossh_term::Mods::default()),
        Some(b"\x1bOA".to_vec()),
        "{modes:?}"
    );
    assert!(!b.text(restored[0]).contains("\x1b[?1000h"));
    drop(b);

    // tmux layout: each window becomes a tab, and splits are tmux's.
    let actions = a.tmux.set_mode(Mode::Tmux);
    a.apply(actions);
    a.until("tabs per window", |h| {
        h.ws.tabs.len() == 2 && h.shown() == 2
    });
    assert_eq!(a.tmux.mode(), Mode::Tmux);
    let pane = a.ws.focused();
    let actions = a.tmux.split(pane, Dir::Down);
    a.apply(actions);
    a.until("the tmux split", |h| h.shown() == 3);
    let tab = a.ws.tab_of(a.ws.focused()).unwrap();
    assert_eq!(a.ws.tabs[tab].root.panes().len(), 2);
    assert_eq!(
        tmux_cmd(
            &server,
            &["list-panes", "-t", "e2e", "-s", "-F", "#{pane_id}"]
        )
        .lines()
        .count(),
        3
    );

    // Closing a pane closes it in tmux.
    let closing = a.ws.focused();
    let actions = a.tmux.close(closing);
    a.apply(actions);
    a.until("the closed pane", |h| {
        h.shown() == 2 && !h.panes.contains_key(&closing)
    });

    // The picker offers the other sessions.
    tmux_cmd(&server, &["new-session", "-d", "-s", "other"]);
    let actions = a.tmux.pick_session();
    a.apply(actions);
    a.until("the picker", |h| !h.pickers.is_empty());
    assert_eq!(a.pickers[0], ["other"]);

    // Detaching brings the gateway back; tmux keeps the session.
    let actions = a.tmux.detach();
    a.apply(actions);
    a.until("the detach", |h| h.gateway_shown && !h.tmux.active());
    assert_eq!(a.ws.all_panes(), [a.tmux.gateway]);
    assert!(a.panes.is_empty());
    assert!(tmux_cmd(&server, &["list-sessions", "-F", "#{session_name}"]).contains("e2e"));
}

/// tmux 3.0 to 3.2 cannot size windows one by one, so FeroSSH layout falls
/// back to tmux layout, says so, and everything else still works:
/// typing, tmux splits, reattach with history and mouse modes, detach.
#[test]
fn older_tmux_falls_back_to_tmux_layout() {
    if !tmux_available() {
        return;
    }
    let v = version();
    if v < MIN_VERSION || v >= WINDOW_SIZES_VERSION {
        return;
    }
    let server = Server {
        socket: format!("ferossh-e2e-old-{}", std::process::id()),
    };
    let mut a = Harness::attach(&server, Mode::Ferossh);
    a.until("the first pane", |h| h.shown() == 1 && !h.panes.is_empty());
    assert_eq!(a.tmux.mode(), Mode::Tmux);
    assert!(
        a.notices.iter().any(|n| n.contains("older than 3.3")),
        "{:?}",
        a.notices
    );
    let first = a.ws.all_panes()[0];
    let actions = a.tmux.input(first, b"echo one-$((40+2))\r");
    a.apply(actions);
    a.until("the echo", |h| h.text(first).contains("one-42"));

    let actions = a.tmux.split(first, Dir::Down);
    a.apply(actions);
    a.until("the tmux split", |h| h.shown() == 2);
    assert_eq!(a.ws.tabs.len(), 1);
    let second = a.ws.focused();
    let actions = a
        .tmux
        .input(second, b"printf '\\033[?1000h\\033[?1006h'; echo two\r");
    a.apply(actions);
    a.until("the second echo", |h| h.text(second).contains("two"));
    a.until("the mouse modes", |_| {
        tmux_cmd(
            &server,
            &["list-panes", "-s", "-t", "e2e", "-F", "#{mouse_sgr_flag}"],
        )
        .contains('1')
    });

    // Another PC attaches: the same split and history, the same mouse
    // modes and no motion reports.
    let mut b = Harness::attach(&server, Mode::Ferossh);
    b.until("the restored panes", |h| {
        h.shown() == 2 && h.panes.values().all(|p| !p.1.is_empty())
    });
    assert_eq!(b.ws.tabs.len(), 1);
    let texts: Vec<String> = b.ws.all_panes().iter().map(|&n| b.text(n)).collect();
    assert!(texts.iter().any(|t| t.contains("one-42")), "{texts:?}");
    let modes = texts.iter().find(|t| t.contains("two")).unwrap();
    assert!(modes.contains("\x1b[?1000h\x1b[?1006h"), "{modes:?}");
    assert!(!modes.contains("\x1b[?1003h"), "{modes:?}");
    drop(b);

    let actions = a.tmux.detach();
    a.apply(actions);
    a.until("the detach", |h| h.gateway_shown && !h.tmux.active());
    assert!(tmux_cmd(&server, &["list-sessions", "-F", "#{session_name}"]).contains("e2e"));
}

/// tmux before 3.0 is refused with a message and the client detaches;
/// the session stays. (Over pipes tmux 2.6 then prints no `%exit`, which
/// it does with `-CC` on a terminal, so the test checks tmux's side.)
#[test]
fn too_old_tmux_is_refused() {
    if !tmux_available() || version() >= MIN_VERSION {
        return;
    }
    let server = Server {
        socket: format!("ferossh-e2e-ancient-{}", std::process::id()),
    };
    let mut a = Harness::attach(&server, Mode::Ferossh);
    a.until("the refusal", |h| !h.notices.is_empty());
    assert!(
        a.notices[0].contains("needs tmux 3.0 or later"),
        "{:?}",
        a.notices
    );
    assert!(a.panes.is_empty());
    a.until("the detach", |_| {
        tmux_cmd(&server, &["list-clients", "-F", "#{client_name}"]).is_empty()
    });
    assert!(tmux_cmd(&server, &["list-sessions", "-F", "#{session_name}"]).contains("e2e"));
}

#[test]
fn real_tmux_split_in_the_same_directory() {
    // Older tmux is refused before any pane appears.
    if !tmux_available() || version() < MIN_VERSION {
        return;
    }
    let server = Server {
        socket: format!("ferossh-e2e-here-{}", std::process::id()),
    };
    let mut a = Harness::attach(&server, Mode::Ferossh);
    a.until("the first pane", |h| h.shown() == 1 && !h.panes.is_empty());
    let first = a.ws.all_panes()[0];
    // A space and a `#`, which tmux would read as a format.
    let dir = format!("/tmp/ferossh here #{}", std::process::id());
    let line = format!("mkdir -p '{dir}' && cd '{dir}' && echo in-$((40+2))\r");
    let actions = a.tmux.input(first, line.as_bytes());
    a.apply(actions);
    a.until("the cd", |h| h.text(first).contains("in-42"));

    // Older tmux falls back to tmux layout: a tmux split; both start
    // there.
    let actions = a.tmux.split(first, Dir::Right);
    a.apply(actions);
    a.until("the split", |h| h.shown() == 2);
    let second = a.ws.focused();
    assert_ne!(second, first);
    let actions = a.tmux.input(second, b"echo \"at:$(pwd):\"\r");
    a.apply(actions);
    a.until("the new pane's directory", |h| {
        h.text(second).contains(&format!("at:{dir}:"))
    });
    let _ = tmux_cmd(&server, &["run-shell", &format!("rmdir '{dir}'")]);
}

#[test]
fn real_tmux_new_tab_follows_the_pane_not_the_start_directory() {
    if !tmux_available() || version() < MIN_VERSION {
        return;
    }
    let server = Server {
        socket: format!("ferossh-e2e-start-{}", std::process::id()),
    };
    let mut a = Harness::attach(&server, Mode::Ferossh);
    a.until("the first pane", |h| h.shown() == 1 && !h.panes.is_empty());
    let first = a.ws.all_panes()[0];
    let dir = format!("/tmp/ferossh start #{}", std::process::id());
    let line = format!("mkdir -p '{dir}' && cd '{dir}' && echo made-$((40+2))\r");
    let actions = a.tmux.input(first, line.as_bytes());
    a.apply(actions);
    a.until("the mkdir", |h| h.text(first).contains("made-42"));

    // The host's start directory is only for when the pane's is not
    // known; tmux always knows it.
    a.tmux.start_dir = Some("/".into());
    let actions = a.tmux.new_tab(first);
    a.apply(actions);
    a.until("the new tab", |h| h.ws.all_panes().len() == 2);
    let second = *a.ws.all_panes().iter().find(|&&p| p != first).unwrap();
    let actions = a.tmux.input(second, b"echo \"at:$(pwd):\"\r");
    a.apply(actions);
    a.until("the new tab's directory", |h| {
        h.text(second).contains(&format!("at:{dir}:"))
    });
    let _ = tmux_cmd(&server, &["run-shell", &format!("rmdir '{dir}'")]);
}

#[test]
fn real_tmux_workspace_makes_a_gone_session_again() {
    use ferossh_config::workspace::{Layout, Open, PaneSpec, TabSpec};

    use crate::workspaces::{TmuxPlan, tmux_rebuild};

    // The rebuild runs a local `sh`; the containers are covered by the
    // attach tests.
    let container = std::env::var_os("FEROSSH_TMUX_CONTAINER").is_some_and(|v| !v.is_empty());
    if container || !tmux_available() || version() < WINDOW_SIZES_VERSION {
        return;
    }
    let id = std::process::id();
    let server = Server {
        socket: format!("ferossh-e2e-ws-{id}"),
    };
    // A folder of the test's own, gone when the test ends however it
    // ends.
    struct Scratch(std::path::PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let scratch = Scratch(std::env::temp_dir().join(format!("ferossh ws {id}")));
    let _ = std::fs::remove_dir_all(&scratch.0);
    std::fs::create_dir(&scratch.0).unwrap();
    let base = &scratch.0;
    let (left, right) = (base.join("it's left"), base.join("right #1"));
    std::fs::create_dir(&left).unwrap();
    std::fs::create_dir(&right).unwrap();
    // `tmux` on the PATH is this test's server.
    let real = Command::new("sh")
        .args(["-c", "command -v tmux"])
        .output()
        .unwrap();
    let real = String::from_utf8_lossy(&real.stdout).trim().to_owned();
    let shim = base.join("bin");
    std::fs::create_dir(&shim).unwrap();
    std::fs::write(
        shim.join("tmux"),
        format!(
            "#!/bin/sh\nexec '{real}' -L '{}' -f /dev/null \"$@\"\n",
            server.socket
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(shim.join("tmux"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
    }
    let pane = |dir: Option<&std::path::Path>, name: Option<&str>| {
        let mut p = PaneSpec::new(Open::Connection("web1".into()));
        p.tmux = Some("e2e".into());
        p.dir = dir.map(|d| d.to_string_lossy().into_owned());
        p.name = name.map(Into::into);
        Layout::Pane(p)
    };
    let plan = TmuxPlan {
        destination: "web1".into(),
        session: "e2e".into(),
        tabs: vec![TabSpec {
            root: Layout::Split {
                down: false,
                ratio: 0.3,
                first: Box::new(pane(Some(&left), Some("logs"))),
                second: Box::new(Layout::Split {
                    down: true,
                    ratio: 0.5,
                    first: Box::new(pane(Some(&right), None)),
                    second: Box::new(pane(None, None)),
                }),
            },
            focus: 1,
        }],
        active: 0,
    };
    let line = tmux_rebuild(&plan).unwrap();
    let path = format!(
        "{}:{}",
        shim.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let run = || {
        let status = Command::new("sh")
            .args(["-c", &format!("{line}true")])
            .env("PATH", &path)
            .env("SHELL", "/bin/sh")
            .env("PS1", "$ ")
            .status()
            .unwrap();
        assert!(status.success());
    };
    run();
    assert_eq!(
        tmux_cmd(&server, &["list-windows", "-t", "e2e"])
            .lines()
            .count(),
        3
    );
    // The session is there now: nothing more is made.
    run();
    assert_eq!(
        tmux_cmd(&server, &["list-windows", "-t", "e2e"])
            .lines()
            .count(),
        3
    );

    let mut a = Harness::attach(&server, Mode::Ferossh);
    a.until("the three panes", |h| h.shown() == 3 && h.panes.len() == 3);
    assert_eq!(a.ws.tabs.len(), 1);
    let panes = a.ws.tabs[0].root.panes();
    match &a.ws.tabs[0].root {
        crate::layout::Node::Split { axis, ratio, .. } => {
            assert_eq!(*axis, crate::layout::Axis::Row);
            assert!((ratio - 0.3).abs() < 0.01, "{ratio}");
        }
        other => panic!("not a split: {other:?}"),
    }
    assert_eq!(a.names.get(&panes[0]).map(String::as_str), Some("logs"));
    for (native, dir) in [(panes[0], &left), (panes[1], &right)] {
        let actions = a.tmux.input(native, b"echo \"at:$(pwd):\"\r");
        a.apply(actions);
        let want = format!("at:{}:", dir.display());
        a.until("the pane's directory", |h| h.text(native).contains(&want));
    }
}
