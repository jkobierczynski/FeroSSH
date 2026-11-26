//! WSL (Windows Subsystem for Linux) panes: File > Open WSL, and coding
//! agents in tmux inside a WSL distro.
//!
//! A WSL shell is an ordinary local pane: `wsl.exe -d <distro>` on ConPTY.
//! tmux integration cannot run there, since ConPTY redraws what a program
//! writes as a screen and so breaks tmux's control protocol. A WSL
//! *gateway* runs tmux in control mode on plain pipes instead
//! ([`crate::pipes`]): `wsl.exe -d <distro> -e sh -c <script>`, where the
//! script (optionally after starting a coding agent's window) writes the
//! DCS that `tmux -CC` would write and execs `tmux -C`, which needs no
//! terminal. From there the gateway is the same as an SSH pane running
//! tmux: [`crate::tmux::Tmux`] shows each tmux window as a native pane.
//!
//! Distros are read from the registry where WSL keeps them, so opening the
//! File menu starts nothing.

/// The most distros File > Open WSL lists.
pub const MAX_DISTROS: usize = 16;

/// What `tmux -CC` writes first, which `tmux -C` does not: the gateway's
/// script writes it before tmux starts, so the tmux watcher sees control
/// mode begin.
const INTRO: &str = "printf '\\033P1000p'";

/// A WSL distro.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Distro {
    pub name: String,
    /// WSL's default distro (what plain `wsl.exe` opens).
    pub default: bool,
}

/// Whether `name` is a distro name FeroSSH passes on: what WSL itself
/// allows (letters, digits, `.`, `_`, `-`), at most 64 characters.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// The distros worth listing, the default first: Docker Desktop's own
/// (`docker-desktop`, `docker-desktop-data`) are left out, as are names
/// WSL would not have made.
pub fn listed(mut distros: Vec<Distro>) -> Vec<Distro> {
    distros.retain(|d| valid_name(&d.name) && !d.name.starts_with("docker-desktop"));
    distros.sort_by(|a, b| {
        b.default
            .cmp(&a.default)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    distros.dedup_by(|a, b| a.name.eq_ignore_ascii_case(&b.name));
    distros.truncate(MAX_DISTROS);
    distros
}

/// The command line of a WSL shell pane: `distro`'s login shell in its
/// home folder.
pub fn shell_command(distro: &str) -> String {
    if valid_name(distro) {
        format!("wsl.exe -d {distro} --cd ~")
    } else {
        "wsl.exe --cd ~".to_owned()
    }
}

/// The label of a WSL pane, its tmux panes and its agents' host.
pub fn label(distro: &str) -> String {
    if distro.is_empty() {
        "WSL".to_owned()
    } else {
        format!("WSL {distro}")
    }
}

/// `text` single-quoted for `sh`.
fn sh_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// `script` run in folder `rel` of the home folder (a path a prompt
/// showed as `~/rel`), for a gateway that starts in the home folder; in
/// the home folder itself when `rel` is gone.
pub fn in_home_folder(rel: &str, script: &str) -> String {
    format!("cd -- {} 2>/dev/null; {script}", sh_quote(rel))
}

/// The arguments for `wsl.exe` that start a gateway in `distro` (the
/// default when empty): in folder `dir` (a Linux path; the home folder
/// when `None`), it runs `before` (a script, such as
/// [`crate::agents::start_script`]), then attaches tmux session `session`
/// in control mode, making it when there is none.
pub fn gateway_args(
    distro: &str,
    dir: Option<&str>,
    before: Option<&str>,
    session: &str,
) -> Vec<String> {
    let mut args = Vec::new();
    if valid_name(distro) {
        args.extend(["-d".to_owned(), distro.to_owned()]);
    }
    let dir = dir
        .filter(|d| d.starts_with('/') && !d.chars().any(char::is_control))
        .unwrap_or("~");
    args.extend(["--cd".to_owned(), dir.to_owned()]);
    args.extend(["-e".to_owned(), "sh".to_owned(), "-c".to_owned()]);
    args.push(gateway_script(before, session));
    args
}

/// The arguments for `wsl.exe` that run `command` (a line for a login
/// shell, such as [`crate::worktree::add_command`]) under `sh` in
/// `distro`, in the home folder.
pub fn script_args(distro: &str, command: &str) -> Vec<String> {
    let mut args = Vec::new();
    if valid_name(distro) {
        args.extend(["-d".to_owned(), distro.to_owned()]);
    }
    args.extend(
        ["--cd", "~", "-e", "sh", "-c", command]
            .into_iter()
            .map(str::to_owned),
    );
    args
}

/// The script a gateway runs (see [`gateway_args`]).
pub fn gateway_script(before: Option<&str>, session: &str) -> String {
    let check = "command -v tmux >/dev/null 2>&1 || { echo 'tmux is not installed in this \
                 WSL distro. Install it there (for example: sudo apt install tmux), then try \
                 again.'; exit 127; }";
    let before = before.map_or(String::new(), |b| format!("{{ {b}; }} >/dev/null; "));
    format!(
        "{check}; {before}{INTRO}; exec tmux -C new-session -A -s {}",
        sh_quote(session)
    )
}

/// The distros in the registry (`HKCU\Software\Microsoft\Windows\
/// CurrentVersion\Lxss`, one subkey per distro with its
/// `DistributionName`; `DefaultDistribution` names the default's key).
#[cfg(windows)]
pub fn distros() -> Vec<Distro> {
    listed(registry::read())
}

#[cfg(windows)]
mod registry {
    // Registry calls are FFI; each unsafe block is a direct call on a key
    // this module opened, with buffers of the sizes passed.
    #![allow(unsafe_code)]

    use std::ffi::c_void;

    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_READ, RRF_RT_REG_SZ, RegCloseKey, RegEnumKeyExW, RegGetValueW,
        RegOpenKeyExW,
    };
    use windows::core::{HSTRING, PWSTR, w};

    use super::{Distro, MAX_DISTROS};

    fn text(key: HKEY, sub: &str, name: &str) -> Option<String> {
        let mut buf = vec![0u16; 512];
        let mut size = (buf.len() * 2) as u32;
        // SAFETY: an open key, valid strings and a buffer of `size` bytes.
        let status = unsafe {
            RegGetValueW(
                key,
                &HSTRING::from(sub),
                &HSTRING::from(name),
                RRF_RT_REG_SZ,
                None,
                Some(buf.as_mut_ptr().cast::<c_void>()),
                Some(&mut size),
            )
        };
        if status != ERROR_SUCCESS {
            return None;
        }
        let units = (size as usize / 2).min(buf.len());
        let t = buf.get(..units)?;
        let end = t.iter().position(|&u| u == 0).unwrap_or(t.len());
        Some(String::from_utf16_lossy(t.get(..end)?))
    }

    pub fn read() -> Vec<Distro> {
        let mut key = HKEY::default();
        // SAFETY: a constant path and a local out-pointer.
        let status = unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                w!("Software\\Microsoft\\Windows\\CurrentVersion\\Lxss"),
                None,
                KEY_READ,
                &mut key,
            )
        };
        if status != ERROR_SUCCESS {
            return Vec::new();
        }
        let default = text(key, "", "DefaultDistribution").unwrap_or_default();
        let mut out = Vec::new();
        for index in 0..(MAX_DISTROS as u32 * 4) {
            let mut name = [0u16; 256];
            let mut len = name.len() as u32;
            // SAFETY: an open key and a buffer of `len` units.
            let status = unsafe {
                RegEnumKeyExW(
                    key,
                    index,
                    Some(PWSTR(name.as_mut_ptr())),
                    &mut len,
                    None,
                    None,
                    None,
                    None,
                )
            };
            if status != ERROR_SUCCESS {
                break;
            }
            let sub = String::from_utf16_lossy(name.get(..len as usize).unwrap_or_default());
            if let Some(n) = text(key, &sub, "DistributionName") {
                out.push(Distro {
                    name: n,
                    default: sub.eq_ignore_ascii_case(&default),
                });
            }
        }
        // SAFETY: the key opened above, closed once.
        unsafe {
            let _ = RegCloseKey(key);
        }
        out
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn d(name: &str, default: bool) -> Distro {
        Distro {
            name: name.into(),
            default,
        }
    }

    #[test]
    fn home_relative_folders() {
        assert_eq!(
            in_home_folder("my app", "run"),
            "cd -- 'my app' 2>/dev/null; run"
        );
    }

    #[test]
    fn default_first_and_docker_left_out() {
        let l = listed(vec![
            d("docker-desktop", false),
            d("Ubuntu-24.04", false),
            d("debian", true),
            d("Alpine", false),
            d("bad name", false),
            d("docker-desktop-data", false),
        ]);
        let names: Vec<&str> = l.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["debian", "Alpine", "Ubuntu-24.04"]);
        assert!(l[0].default);
    }

    #[test]
    fn names_are_checked() {
        assert!(valid_name("Ubuntu-24.04"));
        assert!(valid_name("kali_linux"));
        assert!(!valid_name(""));
        assert!(!valid_name("a b"));
        assert!(!valid_name("x;calc"));
        assert!(!valid_name("\"q\""));
        assert!(!valid_name(&"a".repeat(65)));
    }

    #[test]
    fn shell_commands() {
        assert_eq!(shell_command("Ubuntu"), "wsl.exe -d Ubuntu --cd ~");
        assert_eq!(shell_command(""), "wsl.exe --cd ~");
        assert_eq!(shell_command("a b"), "wsl.exe --cd ~");
        assert_eq!(label("Ubuntu"), "WSL Ubuntu");
        assert_eq!(label(""), "WSL");
    }

    #[test]
    fn script_arguments() {
        assert_eq!(
            script_args("Ubuntu", "exec sh -c 'x'"),
            [
                "-d",
                "Ubuntu",
                "--cd",
                "~",
                "-e",
                "sh",
                "-c",
                "exec sh -c 'x'"
            ]
        );
        assert_eq!(script_args("", "y")[..2], ["--cd", "~"]);
    }

    #[test]
    fn gateway_arguments() {
        let a = gateway_args("Ubuntu", Some("/home/me/app"), None, "agents");
        assert_eq!(
            &a[..7],
            ["-d", "Ubuntu", "--cd", "/home/me/app", "-e", "sh", "-c"]
        );
        assert!(
            a[7].ends_with("exec tmux -C new-session -A -s 'agents'"),
            "{}",
            a[7]
        );
        assert!(a[7].contains("printf '\\033P1000p'"));
        // A relative or odd folder: the home folder instead.
        let b = gateway_args("", Some("C:\\x"), None, "it's");
        assert_eq!(&b[..2], ["--cd", "~"]);
        assert!(b[5].ends_with("-s 'it'\\''s'"), "{}", b[5]);
        let c = gateway_args("x y", Some("/a\nb"), Some("echo hi"), "agents");
        assert_eq!(&c[..2], ["--cd", "~"]);
        assert!(c[5].contains("{ echo hi; } >/dev/null; printf"), "{}", c[5]);
    }
}

/// The gateway against a real tmux, run by `sh` here as WSL would run it.
#[cfg(all(test, unix))]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod e2e {
    use std::path::Path;
    use std::time::{Duration, Instant};

    use ferossh_tmuxcc::{Client, Event};

    use super::*;
    use crate::pipes::{Output, PipeChild};

    fn tmux_installed() -> bool {
        std::process::Command::new("tmux")
            .arg("-V")
            .output()
            .is_ok_and(|o| o.status.success())
    }

    /// `sh -c <script>` with a private tmux server, as `wsl.exe -e sh -c`
    /// runs it in the distro.
    fn spawn(socket: &str, before: Option<&str>) -> PipeChild {
        let script = gateway_script(before, "agents");
        let wrapped = format!("export TMUX_TMPDIR={socket}; unset TMUX; {script}");
        PipeChild::spawn(Path::new("/bin/sh"), &["-c".into(), wrapped], || {}).unwrap()
    }

    fn until(
        c: &PipeChild,
        client: &mut Client<u32>,
        want: impl Fn(&[Event<u32>]) -> bool,
    ) -> Vec<Event<u32>> {
        let start = Instant::now();
        let mut all = Vec::new();
        while start.elapsed() < Duration::from_secs(15) {
            match c.read_timeout(Duration::from_millis(200)) {
                Some(Output::Data(d)) => {
                    all.extend(client.feed(&d));
                    if want(&all) {
                        return all;
                    }
                }
                Some(Output::Exited(code)) => panic!("exited {code:?}: {all:?}"),
                None => {}
            }
        }
        panic!("timed out: {all:?}");
    }

    #[test]
    fn a_gateway_starts_an_agent_window_and_attaches_in_control_mode() {
        if !tmux_installed() {
            eprintln!("tmux is not installed: skipped");
            return;
        }
        let dir = std::env::temp_dir().join(format!("ferossh-wsl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.to_str().unwrap().to_owned();
        let script = crate::agents::start_script("cat", None);
        let c = spawn(&socket, Some(&script));
        let mut client = Client::<u32>::new();
        until(&c, &mut client, |e| e.contains(&Event::Started));
        let line = client
            .command("list-windows -F '#{window_name} #{@ferossh-agent}'", 1)
            .unwrap();
        c.write(line);
        let events = until(&c, &mut client, |e| {
            e.iter().any(|x| matches!(x, Event::Reply { tag: 1, .. }))
        });
        let lines = events
            .iter()
            .find_map(|e| match e {
                Event::Reply { tag: 1, lines, .. } => Some(lines.clone()),
                _ => None,
            })
            .unwrap();
        let text: Vec<String> = lines
            .iter()
            .map(|l| String::from_utf8_lossy(l).into_owned())
            .collect();
        assert_eq!(text, ["cat cat"], "{text:?}");
        // A second gateway joins the same session.
        let c2 = spawn(&socket, None);
        let mut client2 = Client::<u32>::new();
        until(&c2, &mut client2, |e| e.contains(&Event::Started));
        drop(c2);
        drop(c);
        let _ = std::process::Command::new("tmux")
            .env("TMUX_TMPDIR", &socket)
            .env_remove("TMUX")
            .arg("kill-server")
            .status();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_worktree_script_runs_as_wsl_would_run_it() {
        let ok = std::process::Command::new("git")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success());
        if !ok {
            return;
        }
        let root = std::env::temp_dir().join(format!("ferossh-wslwt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let repo = root.join("app");
        std::fs::create_dir_all(&repo).unwrap();
        let st = std::process::Command::new("sh")
            .arg("-c")
            .arg("git init -q -b main . && git -c user.name=t -c user.email=t@t commit -q --allow-empty -m one")
            .current_dir(&repo)
            .status()
            .unwrap();
        assert!(st.success());
        let command = String::from_utf8(crate::worktree::add_command()).unwrap();
        // `wsl.exe <args>` runs `sh -c <command>`: the same here, minus
        // the distro and folder options.
        let args = script_args("", &command);
        let input =
            crate::worktree::add_input(repo.to_str().unwrap().as_bytes(), "claude").unwrap();
        let (out, err) = crate::pipes::run_once(Path::new("/bin/sh"), &args[4..], &input).unwrap();
        match crate::worktree::parse_add(&out, &err) {
            crate::worktree::Added::Made(w) => assert_eq!(w.branch, "claude"),
            other => panic!("{other:?}"),
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn without_tmux_the_gateway_says_so() {
        let script = gateway_script(None, "agents");
        let c = PipeChild::spawn(
            Path::new("/bin/sh"),
            &["-c".into(), format!("PATH=/nonexistent; {script}")],
            || {},
        )
        .unwrap();
        let mut out = Vec::new();
        loop {
            match c.read_timeout(Duration::from_secs(10)).unwrap() {
                Output::Data(d) => out.extend(d),
                Output::Exited(code) => {
                    assert_eq!(code, Some(127));
                    break;
                }
            }
        }
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("tmux is not installed"), "{text}");
    }
}
