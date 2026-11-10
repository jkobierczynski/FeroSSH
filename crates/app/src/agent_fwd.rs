//! Agent forwarding on the window's side (`ForwardAgent`).
//!
//! The worker reads the agent protocol on the server's agent channels and
//! asks for two things only: the list of keys, and a signature. Both go to
//! the signing broker, which reaches the user's agents (the OpenSSH agent
//! and Pageant, or the `IdentityAgent`), never key files: as with OpenSSH,
//! forwarding hands on the agent, not keys on disk.
//!
//! Every signature asks the user first. The question names the host that
//! asks, the key, and what is to be signed: a login as some user to
//! another server, or an `SSHSIG` signature such as a git commit's.
//! Anything else is refused without asking ([`userauth::check_forwarded`]),
//! and so is everything from a connection whose forwarding is off. The
//! user can refuse one request, or refuse the connection's requests from
//! then on. Questions wait their turn, a few at most: a server cannot
//! bury the user in them.
//!
//! Platform-neutral; the window carries out the [`Out`]s.

use std::collections::{HashMap, VecDeque};

use ferossh_auth::{AgentChoice, Identity, Job, KeyChoice, Reply, Source, userauth};
use ferossh_config::hostkey;
use ferossh_ipc::{ChannelId, Message};

use crate::ssh::sanitize_line;

/// Signatures waiting for an answer, all connections together; past this
/// a request is refused at once.
pub const MAX_WAITING: usize = 4;

/// What a broker job of agent forwarding is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum For {
    List {
        connection: ChannelId,
        agent: ChannelId,
    },
    Sign {
        connection: ChannelId,
        agent: ChannelId,
    },
}

/// What the window must do.
#[derive(Debug)]
pub enum Out {
    /// Send to the worker of `connection`.
    Send { connection: ChannelId, msg: Message },
    /// Hand a job to the broker, remembering what it is for.
    Broker { what: For, job: Job },
    /// Ask the user about the request [`Forwarding::question`] describes,
    /// then call [`Forwarding::answer`].
    Ask,
}

/// The user's answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Allow,
    Refuse,
    /// Refuse, and every request of this connection from now on.
    RefuseAll,
}

/// The session a request came through, as the window knows it.
#[derive(Debug, Clone)]
pub struct Peer {
    /// `user@host`, for the question.
    pub label: String,
    /// Forwarding is on for it.
    pub forward: bool,
    /// Which agents to use (`IdentityAgent`).
    pub agent: AgentChoice,
}

#[derive(Debug, Default)]
struct Conn {
    /// The keys last listed, which a signature must use.
    keys: Vec<Identity>,
    agent: AgentChoice,
    /// The user refused all of its requests.
    refused: bool,
}

#[derive(Debug)]
struct Ask {
    connection: ChannelId,
    agent: ChannelId,
    host: String,
    identity: Identity,
    data: Vec<u8>,
    flags: u32,
    what: String,
}

/// Agent forwarding for one window.
#[derive(Debug, Default)]
pub struct Forwarding {
    conns: HashMap<ChannelId, Conn>,
    /// The front one is being asked while `asking`.
    asks: VecDeque<Ask>,
    asking: bool,
}

fn refuse(connection: ChannelId, agent: ChannelId) -> Out {
    Out::Send {
        connection,
        msg: Message::AgentSigned {
            channel: connection,
            agent,
            signature: Vec::new(),
        },
    }
}

impl Forwarding {
    /// A request from the worker of `connection`. `peer` is the session
    /// logged in on it, if one is.
    pub fn on_request(
        &mut self,
        connection: ChannelId,
        msg: Message,
        peer: Option<Peer>,
    ) -> Vec<Out> {
        let peer = peer.filter(|p| p.forward);
        match msg {
            Message::AgentList { agent, .. } => {
                let Some(peer) = peer else {
                    return vec![Out::Send {
                        connection,
                        msg: Message::AgentKeys {
                            channel: connection,
                            agent,
                            keys: Vec::new(),
                        },
                    }];
                };
                let conn = self.conns.entry(connection).or_default();
                conn.agent = peer.agent.clone();
                vec![Out::Broker {
                    what: For::List { connection, agent },
                    job: Job::Identities(KeyChoice {
                        agent: peer.agent,
                        ..KeyChoice::default()
                    }),
                }]
            }
            Message::AgentSign {
                agent,
                key,
                data,
                flags,
                ..
            } => {
                let Some(peer) = peer else {
                    return vec![refuse(connection, agent)];
                };
                let Some(conn) = self.conns.get(&connection) else {
                    return vec![refuse(connection, agent)];
                };
                let Some(identity) = conn.keys.iter().find(|k| k.public == key).cloned() else {
                    return vec![refuse(connection, agent)];
                };
                if conn.refused || self.asks.len() >= MAX_WAITING {
                    return vec![refuse(connection, agent)];
                }
                let Ok(purpose) = userauth::check_forwarded(&data, &key, flags) else {
                    return vec![refuse(connection, agent)];
                };
                let what = match purpose {
                    userauth::Forwarded::Login { user, host_key, .. } => format!(
                        "to log in as \"{}\" to another server{} through {}",
                        sanitize_line(&user),
                        host_key
                            .as_deref()
                            .and_then(hostkey::describe)
                            .map(|d| format!(" (its {} host key is {})", d.kind, d.fingerprint))
                            .unwrap_or_default(),
                        sanitize_line(&peer.label)
                    ),
                    userauth::Forwarded::SshSig { namespace } => format!(
                        "to sign data on {} (an SSH signature in namespace \"{}\"{})",
                        sanitize_line(&peer.label),
                        sanitize_line(&namespace),
                        if namespace == "git" {
                            ", as git uses to sign commits and tags"
                        } else {
                            ""
                        }
                    ),
                };
                self.asks.push_back(Ask {
                    connection,
                    agent,
                    host: peer.label,
                    identity,
                    data,
                    flags,
                    what,
                });
                self.next()
            }
            _ => Vec::new(),
        }
    }

    /// The next question, if none is open.
    fn next(&mut self) -> Vec<Out> {
        if self.asking || self.asks.is_empty() {
            return Vec::new();
        }
        self.asking = true;
        vec![Out::Ask]
    }

    /// The question for the request being asked.
    pub fn question(&self) -> Option<String> {
        let ask = self.asks.front().filter(|_| self.asking)?;
        let key = &ask.identity.public;
        let desc = if userauth::is_certificate(key) {
            hostkey::describe_certificate(key)
                .map(|d| format!("{} certificate for key {}", d.kind, d.fingerprint))
        } else {
            hostkey::describe(key).map(|d| format!("{} key {}", d.kind, d.fingerprint))
        }
        .unwrap_or_else(|| "a key".into());
        let from = match &ask.identity.source {
            Source::Agent(a) if !ask.identity.comment.is_empty() => {
                format!(
                    "{}, \"{}\"",
                    a.label(),
                    sanitize_line(&ask.identity.comment)
                )
            }
            Source::Agent(a) => a.label().to_owned(),
            Source::File(_) => "a key file".into(),
        };
        Some(format!(
            "{} asks to use your {desc} ({from}) through agent forwarding, {}.\n\n\
             Only allow this if you just started something there that needs your key. \
             A server you do not trust can use a forwarded agent to log in elsewhere \
             as you.\n\n\
             Yes: sign this once.\nNo: refuse it.\nCancel: refuse it and every further \
             request from this connection.",
            sanitize_line(&ask.host),
            ask.what
        ))
    }

    /// The user answered the question being asked.
    pub fn answer(&mut self, answer: Answer) -> Vec<Out> {
        if !self.asking {
            return Vec::new();
        }
        self.asking = false;
        let Some(ask) = self.asks.pop_front() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        match answer {
            Answer::Allow => {
                // A connection gone while the question was open: nothing
                // to sign for, and no agent to sign with.
                let Some(agent) = self.conns.get(&ask.connection).map(|c| c.agent.clone()) else {
                    out.push(refuse(ask.connection, ask.agent));
                    out.extend(self.next());
                    return out;
                };
                out.push(Out::Broker {
                    what: For::Sign {
                        connection: ask.connection,
                        agent: ask.agent,
                    },
                    job: Job::Sign {
                        identity: ask.identity,
                        data: ask.data,
                        flags: ask.flags,
                        passphrase: None,
                        agent,
                    },
                });
            }
            Answer::Refuse => out.push(refuse(ask.connection, ask.agent)),
            Answer::RefuseAll => {
                out.push(refuse(ask.connection, ask.agent));
                if let Some(c) = self.conns.get_mut(&ask.connection) {
                    c.refused = true;
                }
                // Its other waiting requests too.
                let (theirs, rest): (Vec<Ask>, Vec<Ask>) = self
                    .asks
                    .drain(..)
                    .partition(|a| a.connection == ask.connection);
                self.asks = rest.into();
                out.extend(theirs.iter().map(|a| refuse(a.connection, a.agent)));
            }
        }
        out.extend(self.next());
        out
    }

    /// The broker answered a job [`Out::Broker`] handed it.
    pub fn on_broker(&mut self, what: For, reply: Reply) -> Vec<Out> {
        match (what, reply) {
            (For::List { connection, agent }, Reply::Identities { keys, .. }) => {
                // Agent keys only; the job named no files anyway.
                let keys: Vec<Identity> = keys
                    .into_iter()
                    .filter(|k| matches!(k.source, Source::Agent(_)))
                    .collect();
                let listed = keys
                    .iter()
                    .map(|k| (k.public.clone(), k.comment.clone()))
                    .collect();
                if let Some(c) = self.conns.get_mut(&connection) {
                    c.keys = keys;
                }
                vec![Out::Send {
                    connection,
                    msg: Message::AgentKeys {
                        channel: connection,
                        agent,
                        keys: listed,
                    },
                }]
            }
            (For::Sign { connection, agent }, Reply::Signed(result)) => vec![Out::Send {
                connection,
                msg: Message::AgentSigned {
                    channel: connection,
                    agent,
                    signature: result.unwrap_or_default(),
                },
            }],
            (For::List { connection, agent }, _) => vec![Out::Send {
                connection,
                msg: Message::AgentKeys {
                    channel: connection,
                    agent,
                    keys: Vec::new(),
                },
            }],
            (For::Sign { connection, agent }, _) => vec![refuse(connection, agent)],
        }
    }

    /// Forgets connections that are gone, and their waiting requests.
    pub fn retain(&mut self, live: impl Fn(ChannelId) -> bool) {
        self.conns.retain(|&c, _| live(c));
        let asking = self.asking.then(|| self.asks.pop_front()).flatten();
        self.asks.retain(|a| live(a.connection));
        // The open question stays until it is answered.
        if let Some(a) = asking {
            self.asks.push_front(a);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use ferossh_auth::agent::AgentKind;

    fn string(out: &mut Vec<u8>, s: &[u8]) {
        out.extend_from_slice(&u32::try_from(s.len()).unwrap().to_be_bytes());
        out.extend_from_slice(s);
    }

    fn key() -> Vec<u8> {
        let mut k = Vec::new();
        string(&mut k, b"ssh-ed25519");
        string(&mut k, &[9; 32]);
        k
    }

    fn login(user: &str) -> Vec<u8> {
        let mut d = Vec::new();
        string(&mut d, &[1; 32]);
        d.push(50);
        string(&mut d, user.as_bytes());
        string(&mut d, b"ssh-connection");
        string(&mut d, b"publickey");
        d.push(1);
        string(&mut d, b"ssh-ed25519");
        string(&mut d, &key());
        d
    }

    fn peer(forward: bool) -> Option<Peer> {
        Some(Peer {
            label: "me@build".into(),
            forward,
            agent: AgentChoice::Default,
        })
    }

    fn listed(f: &mut Forwarding) {
        let out = f.on_request(
            1,
            Message::AgentList {
                channel: 1,
                agent: 9,
            },
            peer(true),
        );
        let [Out::Broker { what, .. }] = &out[..] else {
            panic!("{out:?}");
        };
        let identity = Identity {
            public: key(),
            comment: "laptop".into(),
            source: Source::Agent(AgentKind::OpenSsh),
        };
        let out = f.on_broker(
            *what,
            Reply::Identities {
                keys: vec![identity],
                notes: Vec::new(),
            },
        );
        let [
            Out::Send {
                msg: Message::AgentKeys { keys, .. },
                ..
            },
        ] = &out[..]
        else {
            panic!("{out:?}");
        };
        assert_eq!(keys.len(), 1);
    }

    fn sign(f: &mut Forwarding, agent: ChannelId, data: Vec<u8>) -> Vec<Out> {
        f.on_request(
            1,
            Message::AgentSign {
                channel: 1,
                agent,
                key: key(),
                data,
                flags: 0,
            },
            peer(true),
        )
    }

    fn refused(out: &[Out]) -> bool {
        matches!(
            out,
            [Out::Send { msg: Message::AgentSigned { signature, .. }, .. }] if signature.is_empty()
        )
    }

    #[test]
    fn nothing_without_forwarding() {
        let mut f = Forwarding::default();
        let out = f.on_request(
            1,
            Message::AgentList {
                channel: 1,
                agent: 9,
            },
            peer(false),
        );
        assert!(
            matches!(&out[..], [Out::Send { msg: Message::AgentKeys { keys, .. }, .. }] if keys.is_empty())
        );
        let out = f.on_request(
            1,
            Message::AgentSign {
                channel: 1,
                agent: 9,
                key: key(),
                data: login("git"),
                flags: 0,
            },
            None,
        );
        assert!(refused(&out));
    }

    #[test]
    fn asks_before_every_signature() {
        let mut f = Forwarding::default();
        listed(&mut f);
        let out = sign(&mut f, 9, login("git"));
        assert!(matches!(&out[..], [Out::Ask]));
        let q = f.question().unwrap();
        // No message with the question in it: CodeQL counts a key
        // fingerprint in a panic message as logging sensitive data.
        assert!(q.contains("me@build asks to use your ED25519 key SHA256:"));
        assert!(q.contains("log in as \"git\""));
        assert!(q.contains("ssh-agent, \"laptop\""));
        // A second one waits for the first.
        assert!(sign(&mut f, 10, login("deploy")).is_empty());
        let out = f.answer(Answer::Allow);
        assert!(matches!(
            &out[0],
            Out::Broker {
                what: For::Sign { agent: 9, .. },
                ..
            }
        ));
        assert!(matches!(&out[1], Out::Ask));
        assert!(f.question().unwrap().contains("\"deploy\""));
        assert!(refused(&f.answer(Answer::Refuse)));
        assert!(f.question().is_none());
    }

    #[test]
    fn refuses_what_is_no_login_unknown_keys_and_floods() {
        let mut f = Forwarding::default();
        listed(&mut f);
        assert!(refused(&sign(&mut f, 9, b"a document".to_vec())));
        let out = f.on_request(
            1,
            Message::AgentSign {
                channel: 1,
                agent: 9,
                key: b"other".to_vec(),
                data: login("git"),
                flags: 0,
            },
            peer(true),
        );
        assert!(refused(&out));
        for _ in 0..MAX_WAITING {
            sign(&mut f, 9, login("git"));
        }
        assert!(refused(&sign(&mut f, 9, login("git"))));
    }

    #[test]
    fn refuse_all_ends_the_connections_requests() {
        let mut f = Forwarding::default();
        listed(&mut f);
        sign(&mut f, 9, login("a"));
        sign(&mut f, 10, login("b"));
        let out = f.answer(Answer::RefuseAll);
        assert_eq!(out.len(), 2, "{out:?}");
        assert!(f.question().is_none());
        assert!(refused(&sign(&mut f, 11, login("c"))));
    }

    #[test]
    fn gone_connections_are_forgotten() {
        let mut f = Forwarding::default();
        listed(&mut f);
        sign(&mut f, 9, login("a"));
        sign(&mut f, 10, login("b"));
        f.retain(|_| false);
        // The open question stays until answered; the rest is gone.
        assert!(f.question().is_some());
        // Allowed after its connection went: refused, never signed.
        let out = f.answer(Answer::Allow);
        assert_eq!(out.len(), 1);
        assert!(refused(&out));
        assert!(refused(&sign(&mut f, 11, login("c"))));
    }
}
