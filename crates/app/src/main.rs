//! FeroSSH entry point.
//!
//! `ferossh.exe [user@]host[:port]` connects to a host (names from
//! `~/.ssh/config` work too); without a host it asks for one.
//! `ferossh.exe COM3:115200,8N1` opens a serial console instead,
//! `ferossh.exe telnet://host[:port]` an (unencrypted) Telnet session,
//! `ferossh.exe winrm://[user@]host[:port]` (or `winrms://` for HTTPS) PowerShell on a Windows server,
//! and `ferossh.exe sftp://user@host[/folder]` File transfer without a shell
//! (for SFTP-only accounts). The same exe is also the SSH worker: the
//! window starts it again as
//! `ferossh.exe --worker <in> <out>` inside an AppContainer, where it runs
//! the network side over two inherited pipes.
//!
//! `ferossh.exe ssh://[user@]host[:port]` logs in like `[user@]host[:port]`;
//! `--url LINK` is how Windows hands over an `ssh://` or `sftp://` link
//! (Settings > Open links, see `url_handler`), and takes nothing else.
//!
//! Other options: `-p port`, `-l user`, `-N` (no shell: the forwards
//! only, like `ssh -N`), `-L`, `-R` and `-D` forwards as `ssh` writes
//! them, `--no-sandbox` (run the worker
//! without the AppContainer, for hosts on localhost), and
//! `--local [command]`, which opens a local shell through ConPTY as
//! milestone 2 did. `--theme NAME`, before the rest, picks the pane's
//! colour theme. `--new-security-key [--hello] [--ed25519] FILE` makes a
//! FIDO2 security key file (see `newkey`). `--check-sandbox` starts the
//! sandboxed processes and reports whether they run (see `selfcheck`);
//! `--check-conpty` checks PowerShell's prompt marks through ConPTY (see
//! `conpty_check`).

#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg_attr(not(windows), allow(dead_code))]
mod agent_fwd;
#[cfg_attr(not(windows), allow(dead_code))]
mod agents;
#[cfg_attr(not(windows), allow(dead_code))]
mod attention;
#[cfg_attr(not(windows), allow(dead_code))]
mod batch;
#[cfg_attr(not(windows), allow(dead_code))]
mod broadcast;
#[cfg_attr(not(windows), allow(dead_code))]
mod bulk;
#[cfg_attr(not(windows), allow(dead_code))]
mod bulk_edit;
#[cfg(windows)]
mod clipboard;
#[cfg(windows)]
mod colour_editor;
#[cfg(windows)]
mod conn_view;
#[cfg(windows)]
mod connlog_panel;
#[cfg_attr(not(windows), allow(dead_code))]
mod connlog_store;
#[cfg_attr(not(windows), allow(dead_code))]
mod connlog_view;
#[cfg(windows)]
mod conpty;
#[cfg_attr(not(windows), allow(dead_code))]
mod conpty_check;
#[cfg(windows)]
mod credential_panel;
#[cfg_attr(not(windows), allow(dead_code))]
mod credential_view;
#[cfg_attr(not(windows), allow(dead_code))]
mod credentials;
#[cfg(windows)]
mod credman;
#[cfg(feature = "viewer")]
#[cfg_attr(not(windows), allow(dead_code))]
mod desktop;
#[cfg(not(feature = "viewer"))]
#[cfg_attr(not(windows), allow(dead_code))]
#[path = "desktop_off.rs"]
mod desktop;
#[cfg_attr(not(windows), allow(dead_code))]
mod eventlog;
#[cfg(windows)]
mod fields_view;
#[cfg(windows)]
mod files_panel;
#[cfg(windows)]
mod form_view;
#[cfg(windows)]
mod fwd_panel;
#[cfg(windows)]
mod hosts_panel;
#[cfg_attr(not(windows), allow(dead_code))]
mod hosts_view;
#[cfg_attr(not(windows), allow(dead_code))]
mod inventory_view;
#[cfg(windows)]
mod keygen_view;
#[cfg(windows)]
mod keys_view;
#[cfg(windows)]
mod known_hosts_panel;
#[cfg_attr(not(windows), allow(dead_code))]
mod limits_store;
#[cfg(windows)]
mod live_panel;
#[cfg_attr(not(windows), allow(dead_code))]
mod local_store;
#[cfg(windows)]
mod native;
#[cfg(windows)]
mod palette_panel;
#[cfg(windows)]
mod passphrase_view;
#[cfg(windows)]
mod rules_panel;
#[cfg(windows)]
mod script_args;
#[cfg(windows)]
mod script_editor;
#[cfg(windows)]
mod script_hosts;
#[cfg(windows)]
mod script_report;
#[cfg(windows)]
mod scripts_panel;
#[cfg(windows)]
mod theme_panel;
#[cfg(windows)]
mod toast;
// Platform-neutral, tested everywhere; only the Windows build uses them.
#[cfg_attr(not(windows), allow(dead_code))]
mod colour_edit;
#[cfg_attr(not(windows), allow(dead_code))]
mod copy_id;
#[cfg_attr(not(windows), allow(dead_code))]
mod custom_theme;
#[cfg_attr(not(windows), allow(dead_code))]
mod diff;
#[cfg_attr(not(windows), allow(dead_code))]
mod dns_servers;
#[cfg_attr(not(windows), allow(dead_code))]
mod drag_out;
#[cfg_attr(not(windows), allow(dead_code))]
mod edit_local;
#[cfg_attr(not(windows), allow(dead_code))]
mod escape;
#[cfg_attr(not(windows), allow(dead_code))]
mod files;
#[cfg_attr(not(windows), allow(dead_code))]
mod forwards;
#[cfg(all(test, unix))]
mod forwards_e2e;
#[cfg_attr(not(windows), allow(dead_code))]
mod fwdview;
#[cfg_attr(not(windows), allow(dead_code))]
mod highlight;
#[cfg(windows)]
mod history_store;
#[cfg(windows)]
mod import_panel;
#[cfg(windows)]
mod import_reg;
#[cfg_attr(not(windows), allow(dead_code))]
mod import_view;
#[cfg_attr(not(windows), allow(dead_code))]
mod keymap;
#[cfg_attr(not(windows), allow(dead_code))]
mod known_hosts_view;
#[cfg_attr(not(windows), allow(dead_code))]
mod layout;
#[cfg_attr(not(windows), allow(dead_code))]
mod link;
#[cfg_attr(not(windows), allow(dead_code))]
mod live;
#[cfg_attr(not(windows), allow(dead_code))]
mod local_dir;
#[cfg_attr(not(windows), allow(dead_code))]
mod local_socket;
#[cfg_attr(not(windows), allow(dead_code))]
mod login_script;
#[cfg_attr(not(windows), allow(dead_code))]
mod look;
#[cfg_attr(not(windows), allow(dead_code))]
mod machine_policy;
#[cfg_attr(not(windows), allow(dead_code))]
mod newkey;
#[cfg_attr(not(windows), allow(dead_code))]
mod palette;
#[cfg_attr(not(windows), allow(dead_code))]
mod paste_upload;
#[cfg_attr(not(windows), allow(dead_code))]
mod pattern;
#[cfg_attr(not(windows), allow(dead_code))]
mod peer_owner;
#[cfg_attr(not(windows), allow(dead_code))]
mod portable;
#[cfg_attr(not(windows), allow(dead_code))]
mod preview;
#[cfg_attr(not(windows), allow(dead_code))]
mod program;
#[cfg_attr(not(windows), allow(dead_code))]
mod proxycmd;
#[cfg_attr(not(windows), allow(dead_code))]
mod pwcommand;
#[cfg(windows)]
mod rail;
#[cfg_attr(not(windows), allow(dead_code))]
mod reconnect;
#[cfg(windows)]
mod remote_app;
#[cfg_attr(not(windows), allow(dead_code))]
mod rules_view;
#[cfg_attr(not(windows), allow(dead_code))]
mod same_dir;
#[cfg(windows)]
mod sandbox;
#[cfg_attr(not(windows), allow(dead_code))]
mod scrollmark;
#[cfg(windows)]
mod selfcheck;
#[cfg_attr(not(windows), allow(dead_code))]
mod serial_port;
#[cfg_attr(not(windows), allow(dead_code))]
mod session_form;
#[cfg_attr(not(windows), allow(dead_code))]
mod sessions;
#[cfg_attr(not(windows), allow(dead_code))]
mod shell_hook;
#[cfg_attr(not(windows), allow(dead_code))]
mod snippets_view;
#[cfg_attr(not(windows), allow(dead_code))]
mod ssh;
#[cfg(all(test, unix))]
mod ssh_e2e;
#[cfg_attr(not(windows), allow(dead_code))]
mod standby;
#[cfg_attr(not(windows), allow(dead_code))]
mod statusbar;
#[cfg_attr(not(windows), allow(dead_code))]
mod stream;
#[cfg_attr(not(windows), allow(dead_code))]
mod suggest;
#[cfg_attr(not(windows), allow(dead_code))]
mod tabstrip;
#[cfg_attr(not(windows), allow(dead_code))]
mod team_lib;
#[cfg_attr(not(windows), allow(dead_code))]
mod telnet;
#[cfg_attr(not(windows), allow(dead_code))]
mod theme_view;
#[cfg_attr(not(windows), allow(dead_code))]
mod tmux;
#[cfg(all(test, unix))]
mod tmux_e2e;
#[cfg_attr(not(windows), allow(dead_code))]
mod tmux_keys;
#[cfg_attr(not(windows), allow(dead_code))]
mod toolpane;
#[cfg_attr(not(windows), allow(dead_code))]
mod ui_colours;
#[cfg(windows)]
mod ui_theme;
#[cfg(windows)]
mod update;
#[cfg_attr(not(windows), allow(dead_code))]
mod url_handler;
#[cfg_attr(not(windows), allow(dead_code))]
mod vault;
#[cfg_attr(not(windows), allow(dead_code))]
mod vdesk;
#[cfg(windows)]
mod win;
#[cfg_attr(not(windows), allow(dead_code))]
mod winrm;
#[cfg_attr(not(windows), allow(dead_code))]
mod workspaces;
#[cfg_attr(not(windows), allow(dead_code))]
mod zmodem;

use ferossh_config::forward::{ForwardKind, parse_config};

/// What the command line asks for.
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq)]
enum Launch {
    /// Run as the SSH worker on these inherited handles.
    Worker(String, String),
    /// Run as the remote desktop viewer on these inherited handles.
    Viewer(String, String),
    /// Run as a remote app's decoder on these inherited handles.
    Stream(String, String),
    /// Connect to a host, or ask for one.
    Ssh {
        destination: Option<String>,
        sandbox: bool,
        /// `-N` and forwards, for an SSH login.
        extras: ssh::Extras,
    },
    /// A local shell (milestone 2's test host).
    Local(String),
    /// Open a saved workspace: its name, or a workspace file's path.
    Workspace { name: String, sandbox: bool },
    /// Make a security key file.
    NewSecurityKey(newkey::NewKey),
    /// Start the sandboxed processes, check that they run, and report.
    CheckSandbox,
    /// Check PowerShell's OSC 133 marks through ConPTY, and report.
    CheckConpty,
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
    // A link Windows hands over (see `url_handler`): exactly one valid
    // address, so a link that breaks out of its quotes to add options
    // (`--no-sandbox`, `--local ...`) is refused rather than run.
    if args.first().is_some_and(|a| a == "--url") {
        return match args {
            [_, link] if url_handler::valid_link(link) => Ok(Launch::Ssh {
                destination: Some(link.trim().to_owned()),
                sandbox: true,
                extras: ssh::Extras::default(),
            }),
            [_, link] => Err(format!(
                "FeroSSH opens ssh://[user@]host[:port] and sftp:// links; \
                 this one is not valid: {}",
                link.chars()
                    .filter(|c| !c.is_control())
                    .take(200)
                    .collect::<String>()
            )),
            _ => Err("--url takes one link and nothing else.".into()),
        };
    }
    let mut iter = args.iter();
    let mut extras = ssh::Extras::default();
    let mut destination: Option<String> = None;
    let mut port: Option<String> = None;
    let mut user: Option<String> = None;
    let mut sandbox = true;
    let mut workspace: Option<String> = None;
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--worker" => {
                let (Some(i), Some(o)) = (iter.next(), iter.next()) else {
                    return Err("--worker needs two handles".into());
                };
                return Ok(Launch::Worker(i.clone(), o.clone()));
            }
            "--viewer" if cfg!(feature = "viewer") => {
                let (Some(i), Some(o)) = (iter.next(), iter.next()) else {
                    return Err("--viewer needs two handles".into());
                };
                return Ok(Launch::Viewer(i.clone(), o.clone()));
            }
            "--stream" => {
                let (Some(i), Some(o)) = (iter.next(), iter.next()) else {
                    return Err("--stream needs two handles".into());
                };
                return Ok(Launch::Stream(i.clone(), o.clone()));
            }
            "--local" => {
                let command: Vec<String> = iter.map(|a| quote(a)).collect();
                return Ok(Launch::Local(command.join(" ")));
            }
            "--new-security-key" => {
                let rest: Vec<String> = iter.cloned().collect();
                return newkey::parse(&rest).map(Launch::NewSecurityKey);
            }
            "--check-sandbox" => return Ok(Launch::CheckSandbox),
            "--check-conpty" => return Ok(Launch::CheckConpty),
            "--no-sandbox" => sandbox = false,
            "--workspace" => {
                workspace = Some(
                    iter.next()
                        .ok_or("--workspace needs a workspace's name")?
                        .clone(),
                );
            }
            "-p" => port = Some(iter.next().ok_or("-p needs a port")?.clone()),
            "-l" => user = Some(iter.next().ok_or("-l needs a user name")?.clone()),
            "-N" => extras.no_shell = true,
            a if forward_flag(a).is_some() => {
                let (kind, attached) = forward_flag(a).unwrap_or((ForwardKind::Local, ""));
                let spec = if attached.is_empty() {
                    iter.next()
                        .ok_or_else(|| format!("{a} needs a forward"))?
                        .as_str()
                } else {
                    attached
                };
                let forward = parse_config(kind, &[spec.to_owned()])
                    .ok_or_else(|| format!("{a} {spec} is not a forward"))?;
                extras.forwards.push(forward);
            }
            a if a.starts_with('-') => return Err(format!("unknown option {a}")),
            a if destination.is_none() => destination = Some(a.to_owned()),
            a => return Err(format!("unexpected argument {a}")),
        }
    }
    if let Some(name) = workspace {
        if destination.is_some() || port.is_some() || user.is_some() || !extras.is_empty() {
            return Err("--workspace opens its own connections: give no host".into());
        }
        return Ok(Launch::Workspace { name, sandbox });
    }
    let destination = match destination {
        Some(mut d) => {
            // `-l` and `-p` go on the host an `ssh://` link names, not on
            // the link's text.
            if (user.is_some() || port.is_some())
                && let Some(host) = ferossh_config::ssh_config::ssh_url(&d)
            {
                d = host;
            }
            if let Some(u) = user
                && !d.contains('@')
            {
                d = format!("{u}@{d}");
            }
            if let Some(p) = port {
                if ferossh_config::ssh_config::parse_destination(&d)
                    .is_some_and(|(_, _, p)| p.is_some())
                {
                    return Err("-p and a port in the host: give the port once".into());
                }
                d = format!("{d}:{p}");
            }
            Some(d)
        }
        None if port.is_some() || user.is_some() || !extras.is_empty() => {
            return Err("-p, -l, -N, -L, -R and -D need a host".into());
        }
        None => None,
    };
    Ok(Launch::Ssh {
        destination,
        sandbox,
        extras,
    })
}

/// `-L`, `-R` or `-D`, with the forward written straight after it
/// (`-L8080:db:80`) or not.
#[cfg_attr(not(windows), allow(dead_code))]
fn forward_flag(arg: &str) -> Option<(ForwardKind, &str)> {
    let rest = arg.strip_prefix('-')?;
    let kind = match rest.get(..1)? {
        "L" => ForwardKind::Local,
        "R" => ForwardKind::Remote,
        "D" => ForwardKind::Dynamic,
        _ => return None,
    };
    Some((kind, rest.get(1..)?))
}

#[cfg(windows)]
fn main() -> windows::core::Result<()> {
    sandbox::system32_dlls_only();
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
        Ok(Launch::Stream(input, output)) => {
            // A remote app's decoder: no window, no network, no files.
            if let Some((input, output)) = sandbox::worker_pipes(&input, &output) {
                std::process::exit(stream::run(
                    std::io::BufReader::new(input),
                    std::io::BufWriter::new(output),
                ));
            }
            Ok(())
        }
        Ok(Launch::NewSecurityKey(key)) => win::new_security_key(&key),
        Ok(Launch::CheckSandbox) => std::process::exit(selfcheck::run()),
        Ok(Launch::CheckConpty) => std::process::exit(conpty_check::run()),
        #[cfg(feature = "viewer")]
        Ok(Launch::Viewer(input, output)) => {
            // Decoding only: no window, no network, no files.
            if let Some((input, output)) = sandbox::worker_pipes(&input, &output) {
                let _ = ferossh_viewer::viewer::run(
                    std::io::BufReader::new(input),
                    std::io::BufWriter::new(output),
                );
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
                sandbox: true,
                extras: ssh::Extras::default(),
            })
        );
        assert_eq!(
            parse(&["-l", "root", "-p", "2222", "host", "--no-sandbox"]),
            Ok(Launch::Ssh {
                destination: Some("root@host:2222".into()),
                sandbox: false,
                extras: ssh::Extras::default(),
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
        assert!(matches!(
            parse(&["--new-security-key", "--hello", "id_ecdsa_sk"]),
            Ok(Launch::NewSecurityKey(k)) if k.hello
        ));
        assert_eq!(parse(&["--check-sandbox"]), Ok(Launch::CheckSandbox));
        assert_eq!(parse(&["--check-conpty"]), Ok(Launch::CheckConpty));
        assert_eq!(
            parse(&["--workspace", "Web servers", "--no-sandbox"]),
            Ok(Launch::Workspace {
                name: "Web servers".into(),
                sandbox: false
            })
        );
        assert!(parse(&["--workspace"]).is_err());
        assert!(parse(&["--workspace", "w", "host"]).is_err());
        assert!(parse(&["-x"]).is_err());
        assert!(parse(&["a", "b"]).is_err());
        assert!(parse(&["-p", "22"]).is_err());
    }

    #[test]
    fn tunnels_on_the_command_line() {
        let Ok(Launch::Ssh {
            destination,
            extras,
            ..
        }) = parse(&[
            "-N",
            "-L",
            "5432:db:5432",
            "-D1080",
            "-R",
            "9000:localhost:3000",
            "jump",
        ])
        else {
            unreachable!("parses");
        };
        assert_eq!(destination.as_deref(), Some("jump"));
        assert!(extras.no_shell);
        let kinds: Vec<_> = extras.forwards.iter().map(|f| f.kind).collect();
        assert_eq!(
            kinds,
            [
                ForwardKind::Local,
                ForwardKind::Dynamic,
                ForwardKind::Remote
            ]
        );
        assert!(parse(&["-L", "nonsense", "h"]).is_err());
        assert!(parse(&["-L"]).is_err());
        assert!(parse(&["-N"]).is_err());
        assert!(parse(&["--workspace", "w", "-N"]).is_err());
        assert!(matches!(
            parse(&["-l", "bob", "-p", "2222", "ssh://web1/"]),
            Ok(Launch::Ssh { destination: Some(d), .. }) if d == "bob@web1:2222"
        ));
        assert!(parse(&["-p", "2222", "ssh://web1:22"]).is_err());
        assert!(parse(&["-p", "2222", "web1:22"]).is_err());
    }

    #[test]
    fn links_take_one_valid_address() {
        assert_eq!(
            parse(&["--url", "ssh://admin@web1.lan:2222/"]),
            Ok(Launch::Ssh {
                destination: Some("ssh://admin@web1.lan:2222/".into()),
                sandbox: true,
                extras: ssh::Extras::default(),
            })
        );
        assert!(matches!(
            parse(&["--url", "sftp://up@files.lan/in"]),
            Ok(Launch::Ssh { sandbox: true, .. })
        ));
        // A link that broke out of its quotes, or is not an address.
        assert!(parse(&["--url", "ssh://h", "--no-sandbox"]).is_err());
        assert!(parse(&["--url", "ssh://h", "--local", "calc"]).is_err());
        assert!(parse(&["--url", "--local"]).is_err());
        assert!(parse(&["--url", "ssh://-oProxyCommand=calc"]).is_err());
        assert!(parse(&["--url", "telnet://h"]).is_err());
        assert!(parse(&["--url"]).is_err());
        // Not first: an ordinary unknown option.
        assert!(parse(&["h", "--url", "ssh://h"]).is_err());
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
