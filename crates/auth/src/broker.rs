//! The signing broker: a thread in the UI process that lists the user's
//! public keys and makes signatures, so slow agents, Pageant confirmations,
//! passphrase key derivation and waiting for a security key touch never
//! stall the window.
//!
//! The UI checks each request with [`crate::userauth::check`] before it
//! submits a [`Job::Sign`].

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;

use russh::keys::{Certificate, PublicKey};
use zeroize::Zeroizing;

use crate::agent::{self, AgentChoice, AgentKind};
use crate::gss::{Contexts, GssOutput, GssProvider, GssStepJob};
use crate::keyfile::{KeyFile, KeyFileError};
use crate::sk::Authenticator;
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

/// Which keys a login offers, from the host's config.
#[derive(Debug, Clone, Default)]
pub struct KeyChoice {
    pub files: Vec<KeyFileSpec>,
    /// `CertificateFile`s: each is offered with the key file it certifies.
    /// A `<key>-cert.pub` beside a key file is found without one.
    pub certificates: Vec<PathBuf>,
    /// `IdentityAgent`.
    pub agent: AgentChoice,
    /// `IdentitiesOnly yes`: agent keys only when a key file is the same key.
    pub identities_only: bool,
}

#[derive(Debug)]
pub enum Job {
    /// List agent keys, then the key files.
    Identities(KeyChoice),
    /// Sign `data` with `identity`; an agent's key through `agent`.
    Sign {
        identity: Identity,
        data: Vec<u8>,
        flags: u32,
        passphrase: Option<Zeroizing<String>>,
        agent: AgentChoice,
    },
    /// One step of a Kerberos login, checked by the caller with
    /// [`crate::gss::check_mic_data`].
    Gssapi(GssStepJob),
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
    /// A [`Job::Gssapi`] step's tokens, or why there is no Kerberos login.
    /// Kerberos runs on a thread of its own, so a KDC that is slow to
    /// answer never holds up signing: this reply comes out of order, and
    /// `login` says whose it is.
    Gssapi {
        login: u32,
        result: Result<GssOutput, String>,
    },
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

/// A certificate file's blob: an OpenSSH `*-cert.pub` line.
fn read_certificate(path: &std::path::Path) -> Option<Vec<u8>> {
    let text = std::fs::read_to_string(path).ok()?;
    let cert = Certificate::from_openssh(text.trim()).ok()?;
    cert.to_bytes().ok()
}

/// The public key inside a certificate blob.
fn certified_key(cert: &[u8]) -> Option<Vec<u8>> {
    let cert = Certificate::from_bytes(cert).ok()?;
    PublicKey::new(cert.public_key().clone(), "")
        .to_bytes()
        .ok()
}

/// The certificates for a key file: `CertificateFile`s that certify its
/// key, and `<path>-cert.pub`.
fn certificates_for(kf: &KeyFile, choice: &KeyChoice, notes: &mut Vec<String>) -> Vec<Vec<u8>> {
    let mut beside = kf.path.clone().into_os_string();
    beside.push("-cert.pub");
    let beside = PathBuf::from(beside);
    let mut out: Vec<Vec<u8>> = Vec::new();
    for path in choice.certificates.iter().chain(std::iter::once(&beside)) {
        let Some(cert) = read_certificate(path) else {
            if path != &beside && path.exists() {
                notes.push(format!(
                    "Skipping certificate {}: it is not an OpenSSH certificate.",
                    path.display()
                ));
            }
            continue;
        };
        if certified_key(&cert).as_deref() != Some(&kf.public[..]) || out.contains(&cert) {
            continue;
        }
        if let Some(why) = refusal(&cert) {
            notes.push(format!("Skipping certificate {}: {why}.", path.display()));
            continue;
        }
        out.push(cert);
    }
    out
}

fn identities(rt: &tokio::runtime::Runtime, choice: KeyChoice) -> Reply {
    let (agent_keys, mut notes) = agent::list(rt, &choice.agent);
    let mut keys: Vec<Identity> = Vec::new();
    let mut files: Vec<(KeyFileSpec, Result<KeyFile, KeyFileError>)> = choice
        .files
        .iter()
        .map(|spec| (spec.clone(), KeyFile::load(&spec.path)))
        .collect();
    let file_publics: Vec<Vec<u8>> = files
        .iter()
        .filter_map(|(_, f)| f.as_ref().ok().map(|f| f.public.clone()))
        .collect();
    for k in agent_keys {
        if choice.identities_only {
            // The key itself, or a certificate of it, must be a key file's.
            let plain = if is_certificate(&k.public) {
                certified_key(&k.public)
            } else {
                Some(k.public.clone())
            };
            if !plain.is_some_and(|p| file_publics.contains(&p)) {
                continue;
            }
        }
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
    let files_tried: Vec<String> = choice
        .files
        .iter()
        .map(|f| f.path.display().to_string())
        .collect();
    for (spec, loaded) in files.drain(..) {
        match loaded {
            Ok(kf) => {
                let certs = certificates_for(&kf, &choice, &mut notes);
                let kf = Arc::new(kf);
                // A certificate goes first: a server that trusts the CA
                // takes it, one that does not may still know the key.
                for cert in certs {
                    if !keys.iter().any(|k| k.public == cert) {
                        keys.push(Identity {
                            public: cert,
                            comment: kf.comment.clone(),
                            source: Source::File(kf.clone()),
                        });
                    }
                }
                if keys.iter().any(|k| k.public == kf.public) {
                    // The agent already holds it and needs no passphrase.
                    continue;
                }
                keys.push(Identity {
                    public: kf.public.clone(),
                    comment: kf.comment.clone(),
                    source: Source::File(kf),
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

fn sign(
    rt: &tokio::runtime::Runtime,
    authenticator: &dyn Authenticator,
    job: Job,
) -> Option<Reply> {
    let Job::Sign {
        identity,
        data,
        flags,
        passphrase,
        agent: choice,
    } = job
    else {
        return None;
    };
    let result = match &identity.source {
        Source::Agent(a) => {
            agent::sign(rt, *a, &choice, &identity.public, &data, flags).map_err(SignError::Failed)
        }
        Source::File(f) => f
            .sign_with(
                passphrase.as_deref().map(String::as_str),
                &data,
                authenticator,
            )
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

/// The authenticator security key files sign through on this system.
pub fn platform_authenticator() -> Box<dyn Authenticator> {
    #[cfg(windows)]
    return Box::new(crate::webauthn::WebAuthn);
    #[cfg(not(windows))]
    return Box::new(crate::sk::Unavailable);
}

impl Broker {
    /// Starts the thread; `notify` runs after each reply is queued.
    pub fn spawn(notify: impl Fn() + Send + 'static) -> std::io::Result<Self> {
        Self::spawn_with(notify, platform_authenticator())
    }

    /// [`Broker::spawn`] with another authenticator for security keys.
    pub fn spawn_with(
        notify: impl Fn() + Send + 'static,
        authenticator: Box<dyn Authenticator>,
    ) -> std::io::Result<Self> {
        Self::spawn_full(notify, authenticator, crate::gss::platform_provider())
    }

    /// [`Broker::spawn_with`] with another source of Kerberos tickets.
    pub fn spawn_full(
        notify: impl Fn() + Send + 'static,
        authenticator: Box<dyn Authenticator>,
        gss: Box<dyn GssProvider>,
    ) -> std::io::Result<Self> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let (jobs, job_rx) = channel::<Job>();
        let (gss_jobs, gss_rx) = channel::<GssStepJob>();
        let (reply_tx, replies) = channel();
        // Both threads wake the window; the callback need not be Sync.
        let notify = Arc::new(std::sync::Mutex::new(notify));
        fn wake<F: Fn()>(n: &std::sync::Mutex<F>) {
            if let Ok(f) = n.lock() {
                f();
            }
        }
        let (gss_reply, gss_notify) = (reply_tx.clone(), notify.clone());
        thread::Builder::new()
            .name("kerberos".into())
            .spawn(move || {
                let mut contexts = Contexts::default();
                while let Ok(step) = gss_rx.recv() {
                    let login = step.login;
                    let result = contexts.step(gss.as_ref(), step);
                    if gss_reply.send(Reply::Gssapi { login, result }).is_err() {
                        break;
                    }
                    wake(&gss_notify);
                }
            })?;
        thread::Builder::new()
            .name("signing broker".into())
            .spawn(move || {
                while let Ok(job) = job_rx.recv() {
                    let reply = match job {
                        Job::Identities(choice) => Some(identities(&rt, choice)),
                        Job::Gssapi(step) => {
                            let _ = gss_jobs.send(step);
                            None
                        }
                        sign_job @ Job::Sign { .. } => sign(&rt, authenticator.as_ref(), sign_job),
                    };
                    if let Some(reply) = reply {
                        if reply_tx.send(reply).is_err() {
                            break;
                        }
                        wake(&notify);
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
            KeyChoice {
                files: vec![KeyFileSpec {
                    path: missing.clone(),
                    explicit: false,
                }],
                ..KeyChoice::default()
            },
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
