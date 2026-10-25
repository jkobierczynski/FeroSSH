//! The signing broker: a thread in the UI process that lists the user's
//! public keys and makes signatures, so slow agents, Pageant confirmations
//! and passphrase key derivation never stall the window.
//!
//! The UI checks each request with [`crate::userauth::check`] before it
//! submits a [`Job::Sign`].

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;

use russh::keys::{Certificate, PublicKey};
use zeroize::Zeroizing;

use crate::agent::{self, AgentKind};
use crate::keyfile::{KeyFile, KeyFileError};
use crate::userauth::{allowed_algorithms, is_certificate, key_type};

/// The smallest RSA key used for user authentication.
pub const MIN_RSA_BITS: u32 = 3072;

/// Where a key's private half lives.
#[derive(Debug, Clone)]
pub enum Source {
    Agent(AgentKind),
    File(Arc<KeyFile>),
}

/// A public key FeroSSH can sign with.
#[derive(Debug, Clone)]
pub struct Identity {
    /// SSH wire format.
    pub public: Vec<u8>,
    pub comment: String,
    pub source: Source,
}

impl Identity {
    /// Where the key comes from, for messages: `ssh-agent`, a file path.
    pub fn origin(&self) -> String {
        match &self.source {
            Source::Agent(a) => a.label().to_owned(),
            Source::File(f) => f.path.display().to_string(),
        }
    }
}

/// A key file to offer: `explicit` ones come from `IdentityFile` and are
/// reported when unusable; the default ones are skipped quietly.
#[derive(Debug, Clone)]
pub struct KeyFileSpec {
    pub path: PathBuf,
    pub explicit: bool,
}

#[derive(Debug)]
pub enum Job {
    /// List agent keys, then these key files.
    Identities { files: Vec<KeyFileSpec> },
    /// Sign `data` with `identity`.
    Sign {
        identity: Identity,
        data: Vec<u8>,
        flags: u32,
        passphrase: Option<Zeroizing<String>>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignError {
    /// The key file is encrypted: ask for its passphrase, then retry.
    NeedsPassphrase,
    WrongPassphrase,
    Failed(String),
}

#[derive(Debug)]
pub enum Reply {
    Identities {
        keys: Vec<Identity>,
        /// Keys skipped and agents that failed, for the user.
        notes: Vec<String>,
    },
    Signed(Result<Vec<u8>, SignError>),
}

/// Why FeroSSH will not use a key, if it will not.
pub fn refusal(public: &[u8]) -> Option<String> {
    let kind = key_type(public).unwrap_or("unknown");
    if allowed_algorithms(kind).is_empty() {
        return Some(format!("{kind} keys are not allowed"));
    }
    if kind == "ssh-rsa" || kind == "ssh-rsa-cert-v01@openssh.com" {
        let bits = if is_certificate(public) {
            Certificate::from_bytes(public)
                .ok()
                .and_then(|c| c.public_key().rsa().map(|r| r.key_size()))
        } else {
            PublicKey::from_bytes(public)
                .ok()
                .and_then(|k| k.key_data().rsa().map(|r| r.key_size()))
        }
        .unwrap_or(0);
        if bits < MIN_RSA_BITS {
            return Some(format!(
                "{bits}-bit RSA keys are too small (at least {MIN_RSA_BITS} bits)"
            ));
        }
    }
    None
}

/// Says where FeroSSH looked, so "Permission denied (publickey)" that
/// follows is not a mystery.
fn no_keys_note(files: &[String]) -> String {
    let agents = if cfg!(windows) {
        "the OpenSSH agent and Pageant"
    } else {
        "the SSH agent"
    };
    if files.is_empty() {
        format!("No SSH keys found: {agents} hold none.")
    } else {
        format!(
            "No SSH keys found: {agents} hold none, and there is no usable key in {}.",
            files.join(", ")
        )
    }
}

fn identities(rt: &tokio::runtime::Runtime, files: Vec<KeyFileSpec>) -> Reply {
    let (agent_keys, mut notes) = agent::list(rt);
    let mut keys: Vec<Identity> = Vec::new();
    for k in agent_keys {
        if let Some(why) = refusal(&k.public) {
            notes.push(format!(
                "Skipping {} key \"{}\": {why}.",
                k.agent.label(),
                k.comment
            ));
            continue;
        }
        keys.push(Identity {
            public: k.public,
            comment: k.comment,
            source: Source::Agent(k.agent),
        });
    }
    let files_tried: Vec<String> = files.iter().map(|f| f.path.display().to_string()).collect();
    for spec in files {
        match KeyFile::load(&spec.path) {
            Ok(kf) => {
                if keys.iter().any(|k| k.public == kf.public) {
                    // The agent already holds it and needs no passphrase.
                    continue;
                }
                keys.push(Identity {
                    public: kf.public.clone(),
                    comment: kf.comment.clone(),
                    source: Source::File(Arc::new(kf)),
                });
            }
            Err(KeyFileError::Unreadable(_)) if !spec.explicit => {}
            Err(e) => notes.push(format!("Skipping key file {}: {e}.", spec.path.display())),
        }
    }
    if keys.is_empty() {
        notes.push(no_keys_note(&files_tried));
    }
    Reply::Identities { keys, notes }
}

fn sign(rt: &tokio::runtime::Runtime, job: Job) -> Option<Reply> {
    let Job::Sign {
        identity,
        data,
        flags,
        passphrase,
    } = job
    else {
        return None;
    };
    let result = match &identity.source {
        Source::Agent(a) => {
            agent::sign(rt, *a, &identity.public, &data, flags).map_err(SignError::Failed)
        }
        Source::File(f) => f
            .sign(passphrase.as_deref().map(String::as_str), &data)
            .map_err(|e| match e {
                KeyFileError::NeedsPassphrase => SignError::NeedsPassphrase,
                KeyFileError::WrongPassphrase => SignError::WrongPassphrase,
                other => SignError::Failed(other.to_string()),
            }),
    };
    Some(Reply::Signed(result))
}

/// The broker thread and its queues.
#[derive(Debug)]
pub struct Broker {
    jobs: Sender<Job>,
    replies: Receiver<Reply>,
}

impl Broker {
    /// Starts the thread; `notify` runs after each reply is queued.
    pub fn spawn(notify: impl Fn() + Send + 'static) -> std::io::Result<Self> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let (jobs, job_rx) = channel::<Job>();
        let (reply_tx, replies) = channel();
        thread::Builder::new()
            .name("signing broker".into())
            .spawn(move || {
                while let Ok(job) = job_rx.recv() {
                    let reply = match job {
                        Job::Identities { files } => Some(identities(&rt, files)),
                        sign_job @ Job::Sign { .. } => sign(&rt, sign_job),
                    };
                    if let Some(reply) = reply {
                        if reply_tx.send(reply).is_err() {
                            break;
                        }
                        notify();
                    }
                }
            })?;
        Ok(Self { jobs, replies })
    }

    pub fn submit(&self, job: Job) {
        let _ = self.jobs.send(job);
    }

    pub fn try_recv(&self) -> Option<Reply> {
        self.replies.try_recv().ok()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::userauth::tests::string;

    fn key(kind: &str) -> Vec<u8> {
        let mut k = Vec::new();
        string(&mut k, kind.as_bytes());
        k
    }

    #[test]
    fn refuses_legacy_and_small_keys() {
        assert!(refusal(&key("ssh-dss")).is_some());
        assert!(refusal(&key("ssh-dss-cert-v01@openssh.com")).is_some());
        assert!(refusal(&key("ssh-rsa-cert-v01@openssh.com")).is_some());
        assert!(refusal(&key("ecdsa-sha2-nistp521")).is_none());
        assert!(refusal(&key("ssh-ed25519-cert-v01@openssh.com")).is_none());
        // An RSA blob that does not parse counts as 0 bits.
        assert!(refusal(&key("ssh-rsa")).is_some());
        assert!(refusal(&key("ssh-ed25519")).is_none());
    }

    #[test]
    fn says_where_it_looked_when_there_are_no_keys() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let missing = std::env::temp_dir().join("ferossh-no-such-key");
        let Reply::Identities { keys, notes } = identities(
            &rt,
            vec![KeyFileSpec {
                path: missing.clone(),
                explicit: false,
            }],
        ) else {
            panic!("not identities");
        };
        if keys.is_empty() {
            let last = notes.last().unwrap();
            assert!(last.starts_with("No SSH keys found"), "{last}");
            assert!(last.contains(&missing.display().to_string()), "{last}");
        }
    }
}
