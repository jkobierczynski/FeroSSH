//! Kerberos logins (`gssapi-with-mic`, RFC 4462).
//!
//! The user's Kerberos tickets live in the UI process: the Windows logon
//! session's, through SSPI ([`sspi`]). The sandboxed worker only relays
//! the opaque tokens between the server and this side, as it relays
//! signatures for keys. A security context is made for one host's
//! `host/<name>` service principal, with mutual authentication and
//! integrity required, so a server that does not hold that service's key
//! cannot finish the exchange; credentials are delegated only when the
//! connection asks for it (`GSSAPIDelegateCredentials yes`).
//!
//! Before it signs, the UI checks with [`check_mic_data`] that the data
//! is a `gssapi-with-mic` user-auth request for the session's own user,
//! as it checks key signatures with [`crate::userauth::check`].

use std::collections::HashMap;

#[cfg(all(unix, feature = "mit-gssapi"))]
pub mod mit;
#[cfg(windows)]
pub mod sspi;

/// The DER encoding of Kerberos 5's GSS-API mechanism OID,
/// 1.2.840.113554.1.2.2.
pub const KRB5_OID: &[u8] = &[
    0x06, 0x09, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x12, 0x01, 0x02, 0x02,
];

/// Security contexts the broker keeps at once: one per login in progress.
const MAX_CONTEXTS: usize = 32;

/// What one step of a security context produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextStep {
    /// Send this token and wait for the server's.
    Continue(Vec<u8>),
    /// Established, with mutual authentication and integrity; the last
    /// token for the server, possibly empty.
    Done(Vec<u8>),
}

/// One Kerberos security context, client side.
pub trait GssContext: Send {
    /// Advances the context with the server's `input` token (none on the
    /// first step).
    fn step(&mut self, input: Option<&[u8]>) -> Result<ContextStep, String>;
    /// The MIC (integrity token) over `data`, once established.
    fn mic(&mut self, data: &[u8]) -> Result<Vec<u8>, String>;
}

/// Makes security contexts with the user's tickets.
pub trait GssProvider: Send {
    /// A context for the `host` service on `host` (the name FeroSSH
    /// connected to, or `GSSAPIServerIdentity`); `delegate` forwards the
    /// user's ticket-granting ticket to the server.
    fn start(&self, host: &str, delegate: bool) -> Result<Box<dyn GssContext>, String>;
}

/// No Kerberos on this system.
#[derive(Debug, Clone, Copy, Default)]
pub struct Unavailable;

impl GssProvider for Unavailable {
    fn start(&self, _host: &str, _delegate: bool) -> Result<Box<dyn GssContext>, String> {
        Err("Kerberos logins need Windows".into())
    }
}

/// The provider for this system's tickets.
pub fn platform_provider() -> Box<dyn GssProvider> {
    #[cfg(windows)]
    return Box::new(sspi::Sspi);
    #[cfg(not(windows))]
    return Box::new(Unavailable);
}

/// A [`crate::Job::Gssapi`] step's result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GssOutput {
    Continue(Vec<u8>),
    Complete { token: Vec<u8>, mic: Vec<u8> },
}

/// One login step for the broker.
#[derive(Debug, Clone)]
pub struct GssStepJob {
    /// The login's channel: its context's key.
    pub login: u32,
    pub host: String,
    pub delegate: bool,
    /// The server chose Kerberos: start a new context.
    pub first: bool,
    /// The server's token; empty on the first step.
    pub token: Vec<u8>,
    /// What the MIC covers, checked by the caller.
    pub mic_data: Vec<u8>,
}

/// The broker's security contexts, by login.
#[derive(Default)]
pub struct Contexts {
    open: HashMap<u32, Box<dyn GssContext>>,
}

impl std::fmt::Debug for Contexts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Contexts")
            .field("open", &self.open.len())
            .finish()
    }
}

impl Contexts {
    /// Runs one step. A context ends when it is established or fails.
    pub fn step(
        &mut self,
        provider: &dyn GssProvider,
        job: GssStepJob,
    ) -> Result<GssOutput, String> {
        let mut ctx = if job.first {
            self.open.remove(&job.login);
            provider.start(&job.host, job.delegate)?
        } else {
            self.open
                .remove(&job.login)
                .ok_or("the Kerberos exchange was not started")?
        };
        let input = (!job.first).then_some(job.token.as_slice());
        match ctx.step(input)? {
            ContextStep::Continue(token) => {
                if self.open.len() >= MAX_CONTEXTS {
                    self.open.clear();
                }
                self.open.insert(job.login, ctx);
                Ok(GssOutput::Continue(token))
            }
            ContextStep::Done(token) => Ok(GssOutput::Complete {
                token,
                mic: ctx.mic(&job.mic_data)?,
            }),
        }
    }
}

fn take_string<'a>(data: &mut &'a [u8]) -> Option<&'a [u8]> {
    let (len, rest) = data.split_first_chunk::<4>()?;
    let len = u32::from_be_bytes(*len) as usize;
    let s = rest.get(..len)?;
    *data = rest.get(len..)?;
    Some(s)
}

/// Checks that `data` is what RFC 4462 3.5 says a MIC covers: the session
/// identifier, `SSH_MSG_USERAUTH_REQUEST`, `user`, `ssh-connection` and
/// `gssapi-with-mic`, and nothing else.
pub fn check_mic_data(data: &[u8], user: &str) -> Result<(), &'static str> {
    let mut d = data;
    let session = take_string(&mut d).ok_or("no session identifier")?;
    // A hash: SHA-256 to SHA-512 in practice; SHA-1 is 20 bytes.
    if !(20..=64).contains(&session.len()) {
        return Err("not a session identifier");
    }
    let (&kind, rest) = d.split_first().ok_or("no request type")?;
    if kind != 50 {
        return Err("not a user-auth request");
    }
    d = rest;
    if take_string(&mut d) != Some(user.as_bytes()) {
        return Err("another user");
    }
    if take_string(&mut d) != Some(b"ssh-connection".as_slice()) {
        return Err("another service");
    }
    if take_string(&mut d) != Some(b"gssapi-with-mic".as_slice()) {
        return Err("another method");
    }
    if !d.is_empty() {
        return Err("trailing data");
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn string(out: &mut Vec<u8>, s: &[u8]) {
        out.extend_from_slice(&u32::try_from(s.len()).unwrap().to_be_bytes());
        out.extend_from_slice(s);
    }

    fn mic_data(session: &[u8], kind: u8, user: &str, service: &str, method: &str) -> Vec<u8> {
        let mut d = Vec::new();
        string(&mut d, session);
        d.push(kind);
        string(&mut d, user.as_bytes());
        string(&mut d, service.as_bytes());
        string(&mut d, method.as_bytes());
        d
    }

    #[test]
    fn checks_what_the_mic_covers() {
        let ok = mic_data(&[1; 32], 50, "fero", "ssh-connection", "gssapi-with-mic");
        assert_eq!(check_mic_data(&ok, "fero"), Ok(()));
        assert!(check_mic_data(&ok, "root").is_err());
        let mut long = ok.clone();
        long.push(0);
        assert!(check_mic_data(&long, "fero").is_err());
        assert!(check_mic_data(&ok[..ok.len() - 1], "fero").is_err());
        for bad in [
            mic_data(&[1; 32], 51, "fero", "ssh-connection", "gssapi-with-mic"),
            mic_data(&[1; 32], 50, "fero", "ssh-userauth", "gssapi-with-mic"),
            mic_data(&[1; 32], 50, "fero", "ssh-connection", "publickey"),
            mic_data(&[1; 8], 50, "fero", "ssh-connection", "gssapi-with-mic"),
            mic_data(&[1; 65], 50, "fero", "ssh-connection", "gssapi-with-mic"),
        ] {
            assert!(check_mic_data(&bad, "fero").is_err());
        }
    }

    /// A context that takes `rounds` server tokens, each its own round
    /// number, and signs by reversing the data.
    struct Fake {
        rounds: u8,
        seen: u8,
        done: bool,
    }

    impl GssContext for Fake {
        fn step(&mut self, input: Option<&[u8]>) -> Result<ContextStep, String> {
            if let Some(input) = input
                && input != [self.seen]
            {
                return Err("out of order".into());
            }
            self.seen += 1;
            if self.seen > self.rounds {
                self.done = true;
                Ok(ContextStep::Done(Vec::new()))
            } else {
                Ok(ContextStep::Continue(vec![self.seen]))
            }
        }

        fn mic(&mut self, data: &[u8]) -> Result<Vec<u8>, String> {
            if !self.done {
                return Err("not established".into());
            }
            Ok(data.iter().rev().copied().collect())
        }
    }

    struct FakeProvider;

    impl GssProvider for FakeProvider {
        fn start(&self, host: &str, _delegate: bool) -> Result<Box<dyn GssContext>, String> {
            if host == "nowhere" {
                return Err("no such host".into());
            }
            Ok(Box::new(Fake {
                rounds: 1,
                seen: 0,
                done: false,
            }))
        }
    }

    fn job(login: u32, first: bool, token: Vec<u8>) -> GssStepJob {
        GssStepJob {
            login,
            host: "db.example".into(),
            delegate: false,
            first,
            token,
            mic_data: vec![1, 2, 3],
        }
    }

    #[test]
    fn keeps_a_context_per_login_until_it_is_established() {
        let mut c = Contexts::default();
        assert_eq!(
            c.step(&FakeProvider, job(1, true, Vec::new())),
            Ok(GssOutput::Continue(vec![1]))
        );
        assert_eq!(
            c.step(&FakeProvider, job(2, true, Vec::new())),
            Ok(GssOutput::Continue(vec![1]))
        );
        assert_eq!(
            c.step(&FakeProvider, job(1, false, vec![1])),
            Ok(GssOutput::Complete {
                token: Vec::new(),
                mic: vec![3, 2, 1]
            })
        );
        // Login 1's context is gone; login 2's is still there.
        assert!(c.step(&FakeProvider, job(1, false, vec![1])).is_err());
        assert!(c.step(&FakeProvider, job(2, false, vec![1])).is_ok());
        let mut nowhere = job(3, true, Vec::new());
        nowhere.host = "nowhere".into();
        assert!(c.step(&FakeProvider, nowhere).is_err());
    }
}
