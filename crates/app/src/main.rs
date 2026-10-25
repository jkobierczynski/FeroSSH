//! FeroSSH entry point.
//!
//! `ferossh.exe [user@]host[:port]` connects to a host (names from
//! `~/.ssh/config` work too); without a host it asks for one. The same exe
//! is also the SSH worker: the window starts it again as
//! `ferossh.exe --worker <in> <out>` inside an AppContainer, where it runs
//! the network side over two inherited pipes.
//!
//! Other options: `-p port`, `-l user`, `--no-sandbox` (run the worker
//! without the AppContainer, for hosts on localhost), and
//! `--local [command]`, which opens a local shell through ConPTY as
//! milestone 2 did. `--theme NAME`, before the rest, picks the pane's
//! colour theme.

#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod clipboard;
#[cfg(windows)]
mod conpty;
// Platform-neutral, tested everywhere; only the Windows build uses them.
#[cfg_attr(not(windows), allow(dead_code))]
mod files;
#[cfg_attr(not(windows), allow(dead_code))]
mod forwards;
#[cfg(all(test, unix))]
mod forwards_e2e;
#[cfg_attr(not(windows), allow(dead_code))]
mod fwdview;
#[cfg_attr(not(windows), allow(dead_code))]
mod layout;
#[cfg_attr(not(windows), allow(dead_code))]
mod link;
#[cfg_attr(not(windows), allow(dead_code))]
mod look;
#[cfg_attr(not(windows), allow(dead_code))]
mod pattern;
#[cfg(windows)]
mod putty_reg;
#[cfg(windows)]
mod sandbox;
#[cfg_attr(not(windows), allow(dead_code))]
mod sessions;
#[cfg_attr(not(windows), allow(dead_code))]
mod ssh;
#[cfg(all(test, unix))]
mod ssh_e2e;
#[cfg_attr(not(windows), allow(dead_code))]
mod tabstrip;
#[cfg_attr(not(windows), allow(dead_code))]
mod tmux;
#[cfg(all(test, unix))]
mod tmux_e2e;
#[cfg(windows)]
mod win;

/// What the command line asks for.
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq)]
enum Launch {
    /// Run as the SSH worker on these inherited handles.
    Worker(String, String),
    /// Connect to a host, or ask for one.
    Ssh {
        destination: Option<String>,
        sandbox: bool,
    },
    /// A local shell (milestone 2's test host).
    Local(String),
}

#[cfg_attr(not(windows), allow(dead_code))]
fn quote(arg: &str) -> String {
    if arg.is_empty() || arg.contains([' ', '\t']) {
        format!("\"{arg}\"")
    } else {
        arg.to_owned()
    }
}

/// Takes a leading `--theme NAME` off the command line.
#[cfg_attr(not(windows), allow(dead_code))]
fn take_theme(args: &[String]) -> (Option<String>, &[String]) {
    match args {
        [flag, name, rest @ ..] if flag == "--theme" => (Some(name.clone()), rest),
        _ => (None, args),
    }
}

#[cfg_attr(not(windows), allow(dead_code))]
fn parse_args(args: &[String]) -> Result<Launch, String> {
    let mut iter = args.iter();
    let mut destination: Option<String> = None;
    let mut port: Option<String> = None;
    let mut user: Option<String> = None;
    let mut sandbox = true;
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--worker" => {
                let (Some(i), Some(o)) = (iter.next(), iter.next()) else {
                    return Err("--worker needs two handles".into());
                };
                return Ok(Launch::Worker(i.clone(), o.clone()));
            }
            "--local" => {
                let command: Vec<String> = iter.map(|a| quote(a)).collect();
                return Ok(Launch::Local(command.join(" ")));
            }
            "--no-sandbox" => sandbox = false,
            "-p" => port = Some(iter.next().ok_or("-p needs a port")?.clone()),
            "-l" => user = Some(iter.next().ok_or("-l needs a user name")?.clone()),
            a if a.starts_with('-') => return Err(format!("unknown option {a}")),
            a if destination.is_none() => destination = Some(a.to_owned()),
            a => return Err(format!("unexpected argument {a}")),
        }
    }
    let destination = match destination {
        Some(mut d) => {
            if let Some(u) = user
                && !d.contains('@')
            {
                d = format!("{u}@{d}");
            }
            if let Some(p) = port {
                d = format!("{d}:{p}");
            }
            Some(d)
        }
        None if port.is_some() || user.is_some() => {
            return Err("-p and -l need a host".into());
        }
        None => None,
    };
    Ok(Launch::Ssh {
        destination,
        sandbox,
    })
}

#[cfg(windows)]
fn main() -> windows::core::Result<()> {
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let (theme, args) = take_theme(&args);
    match parse_args(args) {
        Ok(Launch::Worker(input, output)) => {
            // No window and no files: just the network side.
            if let Some((input, output)) = sandbox::worker_pipes(&input, &output) {
                let _ = ferossh_worker::run_blocking(input, output);
            }
            Ok(())
        }
        Ok(launch) => win::run(launch, theme),
        Err(e) => win::run_error(&e),
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("FeroSSH runs on Windows only.");
    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Launch, String> {
        parse_args(&args.iter().map(|a| (*a).to_owned()).collect::<Vec<_>>())
    }

    #[test]
    fn command_lines() {
        assert_eq!(
            parse(&[]),
            Ok(Launch::Ssh {
                destination: None,
                sandbox: true
            })
        );
        assert_eq!(
            parse(&["-l", "root", "-p", "2222", "host", "--no-sandbox"]),
            Ok(Launch::Ssh {
                destination: Some("root@host:2222".into()),
                sandbox: false
            })
        );
        assert_eq!(
            parse(&["--local", "wsl", "-d", "Ubuntu 24.04"]),
            Ok(Launch::Local("wsl -d \"Ubuntu 24.04\"".into()))
        );
        assert_eq!(
            parse(&["--worker", "12", "16"]),
            Ok(Launch::Worker("12".into(), "16".into()))
        );
        assert!(parse(&["-x"]).is_err());
        assert!(parse(&["a", "b"]).is_err());
        assert!(parse(&["-p", "22"]).is_err());
    }

    #[test]
    fn theme_comes_first() {
        let args: Vec<String> = ["--theme", "Dracula", "host"]
            .iter()
            .map(|a| (*a).to_owned())
            .collect();
        let (theme, rest) = take_theme(&args);
        assert_eq!(theme.as_deref(), Some("Dracula"));
        assert_eq!(rest, ["host".to_owned()]);
        assert_eq!(take_theme(rest).0, None);
    }
}
