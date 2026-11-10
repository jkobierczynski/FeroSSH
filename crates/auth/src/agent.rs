//! The SSH agents FeroSSH signs through: the Windows OpenSSH agent (named
//! pipe), Pageant, and on other systems for tests `SSH_AUTH_SOCK`.
//!
//! russh's agent client is async; the broker thread runs it on its own
//! small runtime, never on the UI thread. A connection is opened per
//! request, as `ssh` does, so an agent started after FeroSSH still works.

use russh::keys::agent::AgentIdentity;
use russh::keys::agent::client::{AgentClient, AgentStream};
use russh::keys::{Certificate, HashAlg};
use tokio::runtime::Runtime;

use crate::userauth::{SSH_AGENT_RSA_SHA2_256, SSH_AGENT_RSA_SHA2_512};

/// Which agent holds a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentKind {
    /// The Windows OpenSSH agent, or whatever `SSH_AUTH_SOCK` names.
    OpenSsh,
    /// PuTTY's Pageant.
    Pageant,
}

impl AgentKind {
    pub fn label(self) -> &'static str {
        match self {
            AgentKind::OpenSsh => "ssh-agent",
            AgentKind::Pageant => "Pageant",
        }
    }
}

/// Which agents to ask, after `IdentityAgent`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum AgentChoice {
    /// The OpenSSH agent (`SSH_AUTH_SOCK`, else its usual pipe) and Pageant.
    #[default]
    Default,
    /// `IdentityAgent none`: no agent at all.
    None,
    /// `IdentityAgent PIPE`: only this agent (a named pipe on Windows, a
    /// socket elsewhere).
    Pipe(String),
}

impl AgentChoice {
    /// From an `IdentityAgent` value: `none`, `SSH_AUTH_SOCK` (or
    /// `$SSH_AUTH_SOCK`), or a pipe.
    pub fn from_config(value: Option<&str>) -> Self {
        match value {
            None => Self::Default,
            Some(v) if v.eq_ignore_ascii_case("none") => Self::None,
            Some("SSH_AUTH_SOCK" | "$SSH_AUTH_SOCK") => Self::Default,
            Some(v) => Self::Pipe(v.to_owned()),
        }
    }
}

/// A public key an agent offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentKey {
    pub agent: AgentKind,
    /// SSH wire format.
    pub public: Vec<u8>,
    pub comment: String,
}

async fn list_from<S: AgentStream + Unpin + Send>(
    mut client: AgentClient<S>,
    agent: AgentKind,
) -> Result<Vec<AgentKey>, russh::keys::Error> {
    let ids = client.request_identities().await?;
    Ok(ids
        .into_iter()
        .filter_map(|id| match id {
            AgentIdentity::PublicKey { key, comment } => Some(AgentKey {
                agent,
                public: key.to_bytes().ok()?,
                comment,
            }),
            // A user certificate: offered as the certificate, signed with
            // the key inside it.
            AgentIdentity::Certificate {
                certificate,
                comment,
            } => Some(AgentKey {
                agent,
                public: certificate.to_bytes().ok()?,
                comment,
            }),
        })
        .collect())
}

async fn sign_with<S: AgentStream + Unpin + Send>(
    mut client: AgentClient<S>,
    public: &[u8],
    data: &[u8],
    flags: u32,
) -> Result<Vec<u8>, String> {
    let identity = identity(public)?;
    let hash = match flags {
        SSH_AGENT_RSA_SHA2_512 => Some(HashAlg::Sha512),
        SSH_AGENT_RSA_SHA2_256 => Some(HashAlg::Sha256),
        _ => None,
    };
    // The client appends the signature to the data as an SSH string, and
    // checks that an RSA signature uses the hash asked for.
    let signed = client
        .sign_request(&identity, hash, data.to_vec())
        .await
        .map_err(|e| e.to_string())?;
    let blob = signed
        .get(data.len()..)
        .and_then(|rest| rest.split_first_chunk::<4>())
        .filter(|(len, blob)| u32::from_be_bytes(**len) as usize == blob.len())
        .map(|(_, blob)| blob.to_vec())
        .ok_or("the agent returned a malformed signature")?;
    Ok(blob)
}

/// The agent identity for a public key or certificate blob.
fn identity(public: &[u8]) -> Result<AgentIdentity, String> {
    if crate::userauth::is_certificate(public) {
        let certificate = Certificate::from_bytes(public).map_err(|e| e.to_string())?;
        return Ok(AgentIdentity::Certificate {
            certificate,
            comment: String::new(),
        });
    }
    let key = russh::keys::PublicKey::from_bytes(public).map_err(|e| e.to_string())?;
    Ok(key.into())
}

#[cfg(windows)]
fn openssh_pipe() -> std::ffi::OsString {
    std::env::var_os("SSH_AUTH_SOCK").unwrap_or_else(|| r"\\.\pipe\openssh-ssh-agent".into())
}

/// Keys from every agent that is running, OpenSSH's first. Agents that
/// are not running are skipped silently; other failures are returned as
/// notes for the user.
pub fn list(rt: &Runtime, choice: &AgentChoice) -> (Vec<AgentKey>, Vec<String>) {
    let mut keys = Vec::new();
    let mut notes = Vec::new();
    match choice {
        AgentChoice::None => return (keys, notes),
        AgentChoice::Pipe(pipe) => {
            rt.block_on(async {
                match connect_pipe(pipe).await {
                    Ok(c) => match list_from(c, AgentKind::OpenSsh).await {
                        Ok(k) => keys.extend(k),
                        Err(e) => notes.push(format!("{pipe}: {e}")),
                    },
                    Err(e) => notes.push(format!("The agent at {pipe} did not answer: {e}")),
                }
            });
            return (keys, notes);
        }
        AgentChoice::Default => {}
    }
    rt.block_on(async {
        #[cfg(windows)]
        {
            if let Ok(c) = AgentClient::connect_named_pipe(openssh_pipe()).await {
                match list_from(c, AgentKind::OpenSsh).await {
                    Ok(k) => keys.extend(k),
                    Err(e) => notes.push(format!("ssh-agent: {e}")),
                }
            }
            if let Ok(c) = AgentClient::connect_pageant().await {
                match list_from(c, AgentKind::Pageant).await {
                    Ok(k) => keys.extend(k),
                    // Without Pageant's named pipe the client falls back to
                    // window messages and "connects" even when Pageant is
                    // not running; the first request then ends the stream.
                    Err(e) if not_running(&e) => {}
                    Err(e) => notes.push(format!("Pageant: {e}")),
                }
            }
        }
        #[cfg(unix)]
        if let Ok(c) = AgentClient::connect_env().await {
            match list_from(c, AgentKind::OpenSsh).await {
                Ok(k) => keys.extend(k),
                Err(e) => notes.push(format!("ssh-agent: {e}")),
            }
        }
    });
    let mut seen = Vec::new();
    keys.retain(|k: &AgentKey| {
        let new = !seen.contains(&k.public);
        seen.push(k.public.clone());
        new
    });
    (keys, notes)
}

/// Connects to the agent `IdentityAgent` names.
#[cfg(windows)]
async fn connect_pipe(
    pipe: &str,
) -> Result<AgentClient<impl AgentStream + Unpin + Send>, russh::keys::Error> {
    AgentClient::connect_named_pipe(pipe).await
}

/// Connects to the agent `IdentityAgent` names.
#[cfg(unix)]
async fn connect_pipe(
    pipe: &str,
) -> Result<AgentClient<impl AgentStream + Unpin + Send>, russh::keys::Error> {
    AgentClient::connect_uds(pipe).await
}

/// Whether an agent error just means nothing answered.
#[cfg_attr(not(windows), allow(dead_code))]
fn not_running(e: &russh::keys::Error) -> bool {
    matches!(e, russh::keys::Error::IO(io) if io.kind() == std::io::ErrorKind::UnexpectedEof)
}

/// Asks `agent` to sign `data` (already checked by [`crate::userauth`])
/// with `public`; returns the SSH signature blob.
pub fn sign(
    rt: &Runtime,
    agent: AgentKind,
    choice: &AgentChoice,
    public: &[u8],
    data: &[u8],
    flags: u32,
) -> Result<Vec<u8>, String> {
    rt.block_on(async {
        match (choice, agent) {
            (AgentChoice::None, _) => return Err("IdentityAgent is none".to_owned()),
            (AgentChoice::Pipe(pipe), _) => {
                let c = connect_pipe(pipe).await.map_err(|e| e.to_string())?;
                return sign_with(c, public, data, flags).await;
            }
            (AgentChoice::Default, _) => {}
        }
        match agent {
            #[cfg(windows)]
            AgentKind::OpenSsh => {
                let c = AgentClient::connect_named_pipe(openssh_pipe())
                    .await
                    .map_err(|e| e.to_string())?;
                sign_with(c, public, data, flags).await
            }
            #[cfg(windows)]
            AgentKind::Pageant => {
                let c = AgentClient::connect_pageant()
                    .await
                    .map_err(|e| e.to_string())?;
                sign_with(c, public, data, flags).await
            }
            #[cfg(unix)]
            AgentKind::OpenSsh => {
                let c = AgentClient::connect_env()
                    .await
                    .map_err(|e| e.to_string())?;
                sign_with(c, public, data, flags).await
            }
            #[cfg(not(windows))]
            AgentKind::Pageant => Err("Pageant runs on Windows only".to_owned()),
        }
    })
}
