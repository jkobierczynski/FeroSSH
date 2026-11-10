//! The UI side of one Telnet session.
//!
//! The connection itself runs in a sandboxed worker of its own, as an SSH
//! connection does ([`ferossh_worker::telnet`]); this side only starts it,
//! passes typing and sizes on, and shows what comes back. Telnet sends
//! everything, passwords included, in the clear, so the pane says so
//! before anything is typed and the tab carries a red badge.
//!
//! A Telnet worker never has a reason to ask for host keys, keys,
//! signatures or passwords. One that does is treated as compromised and
//! the connection is closed, as for SSH.

use ferossh_config::DEFAULT_TELNET_PORT;
use ferossh_config::ssh_config;
use ferossh_ipc::{ChannelId, MAX_DATA_CHUNK, Message, Timing};

use crate::ssh::{Effect, Env, State, sanitize};

const DIM: &str = "\x1b[0;90m";
const RED: &str = "\x1b[0;31m";
const RESET: &str = "\x1b[0m";

/// The warning shown once the connection is open.
pub const WARNING: &str = "Telnet is not encrypted: anyone on the network path can read \
                           this session, passwords included. Use SSH where the device has it.";

/// Where a Telnet session goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelnetTarget {
    /// The saved session's name, or the host as typed; names the tab and
    /// picks the look.
    pub alias: String,
    pub hostname: String,
    pub port: u16,
    /// ssh_config's `Tag`, which picks a colour theme.
    pub tag: Option<String>,
    /// Seconds for the TCP connection (`ConnectTimeout`).
    pub connect_timeout: u32,
}

impl TelnetTarget {
    /// What was typed at the connect prompt, if it asks for Telnet:
    /// `telnet://host[:port]`, or `telnet host [port]` as on a command
    /// line.
    pub fn typed(text: &str) -> Option<Self> {
        let text = text.trim();
        let rest = match strip_prefix_ci(text, "telnet://") {
            Some(r) => r.trim_end_matches('/').to_owned(),
            None => {
                let mut words = strip_prefix_ci(text, "telnet ")?.split_whitespace();
                let host = words.next()?;
                match (words.next(), words.next()) {
                    (None, _) => host.to_owned(),
                    (Some(port), None) if !host.contains(':') => format!("{host}:{port}"),
                    _ => return None,
                }
            }
        };
        let (user, hostname, port) = ssh_config::parse_destination(&rest)?;
        if user.is_some() {
            return None;
        }
        Some(Self {
            alias: hostname.clone(),
            hostname,
            port: port.unwrap_or(DEFAULT_TELNET_PORT),
            tag: None,
            connect_timeout: Timing::default().connect_timeout,
        })
    }

    /// A saved (or imported) Telnet session `name`: its `HostName`,
    /// `Port` (23 when unset), `Tag` and `ConnectTimeout`.
    pub fn resolve(name: &str, env: &Env) -> Option<Self> {
        let includes = ssh_config::DirIncludes {
            home: env.home.clone(),
        };
        let cfg = ssh_config::resolve_in(&env.config, name, &includes);
        let hostname = cfg.hostname.unwrap_or_else(|| name.to_owned());
        if !ferossh_config::saved::valid_hostname(&hostname) {
            return None;
        }
        Some(Self {
            alias: name.to_owned(),
            hostname,
            port: cfg.port.unwrap_or(DEFAULT_TELNET_PORT),
            tag: cfg.tag,
            connect_timeout: cfg
                .connect_timeout
                .unwrap_or(Timing::default().connect_timeout),
        })
    }

    /// `host`, or `host:port` off the Telnet port, for titles and logs.
    pub fn label(&self) -> String {
        if self.port == DEFAULT_TELNET_PORT {
            self.alias.clone()
        } else {
            format!("{}:{}", self.alias, self.port)
        }
    }
}

fn strip_prefix_ci<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let head = text.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| text.get(prefix.len()..))
        .flatten()
}

fn dim(text: &str) -> String {
    format!("{DIM}{text}{RESET}\r\n")
}

/// One Telnet connection, from the UI's side.
#[derive(Debug)]
pub struct TelnetSession {
    pub channel: ChannelId,
    pub target: TelnetTarget,
    state: State,
}

impl TelnetSession {
    pub fn new(channel: ChannelId, target: TelnetTarget) -> Self {
        Self {
            channel,
            target,
            state: State::Connecting,
        }
    }

    pub fn state(&self) -> State {
        self.state
    }

    /// The worker's connection: a Telnet session has its own.
    pub fn connection(&self) -> ChannelId {
        self.channel
    }

    /// Starts connecting with a terminal of `cols` x `rows`.
    pub fn start(&mut self, cols: u16, rows: u16) -> Vec<Effect> {
        let t = &self.target;
        let mut text = format!("Connecting to {}", sanitize(&t.label()));
        if t.hostname != t.alias {
            text.push_str(&format!(" ({}:{})", sanitize(&t.hostname), t.port));
        }
        text.push_str(" over Telnet...");
        vec![
            Effect::Show(dim(&text)),
            Effect::Send(Message::TelnetConnect {
                channel: self.channel,
                host: t.hostname.clone(),
                port: t.port,
                cols,
                rows,
                connect_timeout: t.connect_timeout,
            }),
        ]
    }

    /// Typed or pasted bytes for the host, once connected.
    pub fn input(&self, bytes: &[u8]) -> Vec<Effect> {
        if self.state != State::Connected {
            return Vec::new();
        }
        bytes
            .chunks(MAX_DATA_CHUNK)
            .map(|c| {
                Effect::Send(Message::Data {
                    channel: self.channel,
                    bytes: c.to_vec(),
                })
            })
            .collect()
    }

    pub fn resize(&self, cols: u16, rows: u16) -> Option<Effect> {
        matches!(self.state, State::Connecting | State::Connected).then(|| {
            Effect::Send(Message::Resize {
                channel: self.channel,
                cols,
                rows,
            })
        })
    }

    /// Ends the connection from this side.
    pub fn close(&mut self) -> Vec<Effect> {
        if self.state == State::Ended {
            return Vec::new();
        }
        self.state = State::Ended;
        vec![Effect::Send(Message::Close {
            channel: self.channel,
        })]
    }

    pub fn on_worker(&mut self, msg: Message) -> Vec<Effect> {
        match msg {
            Message::Connected { .. } if self.state == State::Connecting => {
                self.state = State::Connected;
                vec![
                    Effect::Show(dim("Connected.")),
                    Effect::Show(format!("{RED}{WARNING}{RESET}\r\n")),
                    Effect::Connected,
                ]
            }
            Message::Data { bytes, .. } if self.state == State::Connected => {
                vec![Effect::Output(bytes)]
            }
            Message::Notice { text, .. } => vec![Effect::Show(dim(&sanitize(&text)))],
            Message::Disconnected { reason, .. } => {
                if self.state == State::Ended {
                    return vec![Effect::Ended];
                }
                self.state = State::Ended;
                vec![
                    Effect::Show(format!(
                        "\r\n{DIM}[{} Press Enter to reconnect.]{RESET}\r\n",
                        sanitize(&reason)
                    )),
                    Effect::Ended,
                ]
            }
            // Once closed from this side, late data is dropped.
            Message::Data { .. } if self.state == State::Ended => Vec::new(),
            _ => {
                let mut effects = vec![Effect::Show(format!(
                    "\r\n{RED}The Telnet worker sent something a Telnet session never \
                     does, so FeroSSH closed the connection.{RESET}\r\n"
                ))];
                effects.extend(self.close());
                effects.push(Effect::Ended);
                effects
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn shown(effects: &[Effect]) -> String {
        effects
            .iter()
            .filter_map(|e| match e {
                Effect::Show(t) => Some(t.as_str()),
                _ => None,
            })
            .collect()
    }

    fn target() -> TelnetTarget {
        TelnetTarget::typed("telnet://router").unwrap()
    }

    #[test]
    fn reads_what_was_typed() {
        let t = TelnetTarget::typed("telnet://10.0.0.1:2003/").unwrap();
        assert_eq!((t.hostname.as_str(), t.port), ("10.0.0.1", 2003));
        assert_eq!(t.label(), "10.0.0.1:2003");
        let t = TelnetTarget::typed("TELNET console-1 7001").unwrap();
        assert_eq!((t.hostname.as_str(), t.port), ("console-1", 7001));
        let t = TelnetTarget::typed("telnet [fe80::1]:23").unwrap();
        assert_eq!(
            (t.hostname.as_str(), t.port, t.label()),
            ("fe80::1", 23, "fe80::1".into())
        );
        assert_eq!(TelnetTarget::typed("router"), None);
        assert_eq!(TelnetTarget::typed("telnet://admin@router"), None);
        assert_eq!(TelnetTarget::typed("telnet -oProxyCommand=calc"), None);
        assert_eq!(TelnetTarget::typed("telnet a 1 2"), None);
        assert_eq!(TelnetTarget::typed("telnet://router:99999"), None);
    }

    #[test]
    fn resolves_saved_sessions_with_the_telnet_port() {
        let env = Env {
            home: std::env::temp_dir(),
            local_user: "me".into(),
            config: "Host core\n    HostName 10.1.1.1\n    Tag production\n\
                     Host console\n    HostName cs.lan\n    Port 2004\n"
                .into(),
        };
        let core = TelnetTarget::resolve("core", &env).unwrap();
        assert_eq!((core.hostname.as_str(), core.port), ("10.1.1.1", 23));
        assert_eq!(core.tag.as_deref(), Some("production"));
        assert_eq!(core.label(), "core");
        let console = TelnetTarget::resolve("console", &env).unwrap();
        assert_eq!(console.port, 2004);
        assert_eq!(console.label(), "console:2004");
    }

    #[test]
    fn connects_warns_and_passes_data() {
        let mut s = TelnetSession::new(5, target());
        let fx = s.start(80, 24);
        assert!(matches!(
            &fx[1],
            Effect::Send(Message::TelnetConnect {
                channel: 5,
                port: 23,
                ..
            })
        ));
        // Nothing is sent before the connection is up.
        assert!(s.input(b"x").is_empty());
        let fx = s.on_worker(Message::Connected {
            channel: 5,
            kex: "telnet".into(),
            cipher: "none".into(),
            host_key: "none".into(),
        });
        assert!(shown(&fx).contains("not encrypted"));
        assert!(matches!(fx.last(), Some(Effect::Connected)));
        assert_eq!(s.state(), State::Connected);
        assert!(matches!(
            &s.input(b"ls\r")[..],
            [Effect::Send(Message::Data { channel: 5, .. })]
        ));
        let fx = s.on_worker(Message::Data {
            channel: 5,
            bytes: b"login: ".to_vec(),
        });
        assert!(matches!(&fx[..], [Effect::Output(b)] if b == b"login: "));
        let fx = s.on_worker(Message::Disconnected {
            channel: 5,
            reason: "The host closed the connection.".into(),
        });
        assert!(shown(&fx).contains("Press Enter to reconnect"));
        assert!(matches!(fx.last(), Some(Effect::Ended)));
        assert_eq!(s.state(), State::Ended);
    }

    #[test]
    fn a_worker_that_asks_for_ssh_things_is_cut_off() {
        for msg in [
            Message::HostKey {
                channel: 5,
                key: vec![1],
            },
            Message::IdentitiesRequest { channel: 5 },
            Message::Prompt {
                channel: 5,
                kind: ferossh_ipc::PromptKind::Password,
                name: String::new(),
                instructions: String::new(),
                fields: Vec::new(),
            },
            // Data before the connection was reported.
            Message::Data {
                channel: 5,
                bytes: vec![1],
            },
        ] {
            let mut s = TelnetSession::new(5, target());
            s.start(80, 24);
            let fx = s.on_worker(msg);
            assert!(shown(&fx).contains("closed the connection"));
            assert!(
                fx.iter()
                    .any(|e| matches!(e, Effect::Send(Message::Close { channel: 5 })))
            );
            assert_eq!(s.state(), State::Ended);
        }
    }
}
