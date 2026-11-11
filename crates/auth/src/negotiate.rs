//! Negotiate (SPNEGO: Kerberos, or NTLM where Kerberos cannot be used)
//! security contexts for WinRM sessions, held in the UI process.
//!
//! The sandboxed worker speaks HTTP to the server and relays each login
//! token and each message to seal or unseal here ([`ferossh_ipc::SspiOp`]);
//! the credentials and the session keys never leave this process. Each
//! WinRM session has two HTTP connections, so two contexts, told apart by
//! `conn`. A context is for the host's `HTTP` service: a Kerberos ticket
//! for one host is never good for another.
//!
//! Credentials are the Windows logon session's, or a user name and
//! password the user typed for this session.

use std::collections::HashMap;

use ferossh_ipc::{SspiOp, SspiResult};
use zeroize::Zeroizing;

pub use crate::gss::ContextStep;

#[cfg(windows)]
pub mod sspi;

/// Contexts the broker keeps at once: two per WinRM session.
const MAX_CONTEXTS: usize = 64;

/// A user name (`DOMAIN\user`, `user@domain` or a local `user`) and
/// password to log in with, instead of the Windows logon's.
#[derive(Clone)]
pub struct Credentials {
    pub user: String,
    pub password: Zeroizing<String>,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("user", &self.user)
            .finish_non_exhaustive()
    }
}

impl Credentials {
    /// The user name and the domain (empty for none) SSPI takes:
    /// `DOMAIN\user` split; `user@domain` and plain `user` kept whole.
    pub fn split(&self) -> (&str, &str) {
        match self.user.split_once('\\') {
            Some((domain, user)) => (user, domain),
            None => (self.user.as_str(), ""),
        }
    }
}

/// One Negotiate security context, client side.
pub trait NegotiateContext: Send {
    /// Advances the login with the server's `input` token (none at first).
    fn step(&mut self, input: Option<&[u8]>) -> Result<ContextStep, String>;
    /// Encrypts `data`: the signature and the encrypted text.
    fn seal(&mut self, data: &[u8]) -> Result<(Vec<u8>, Vec<u8>), String>;
    /// Checks and decrypts a sealed message.
    fn unseal(&mut self, header: &[u8], data: &[u8]) -> Result<Vec<u8>, String>;
    /// The package the login used: `Kerberos` or `NTLM`.
    fn package(&self) -> String;
}

/// Makes Negotiate contexts.
pub trait NegotiateProvider: Send {
    /// A context for the `HTTP` service on `host`.
    fn start(
        &self,
        host: &str,
        credentials: Option<&Credentials>,
    ) -> Result<Box<dyn NegotiateContext>, String>;
}

/// No SSPI on this system.
#[derive(Debug, Clone, Copy, Default)]
pub struct Unavailable;

impl NegotiateProvider for Unavailable {
    fn start(
        &self,
        _host: &str,
        _credentials: Option<&Credentials>,
    ) -> Result<Box<dyn NegotiateContext>, String> {
        Err("WinRM logins need Windows".into())
    }
}

/// The provider for this system.
pub fn platform_provider() -> Box<dyn NegotiateProvider> {
    #[cfg(windows)]
    return Box::new(sspi::Sspi);
    #[cfg(not(windows))]
    return Box::new(Unavailable);
}

/// One SSPI step of a WinRM session's connection, for the broker.
#[derive(Debug, Clone)]
pub struct SspiJob {
    /// The session's channel.
    pub login: u32,
    pub conn: u8,
    pub host: String,
    /// Used when `op` starts a context.
    pub credentials: Option<Credentials>,
    pub op: SspiOp,
}

/// What a [`SspiJob`] gave: the answer for the worker, and the package
/// once a login is complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SspiOutput {
    pub result: SspiResult,
    pub package: Option<String>,
}

/// The broker's Negotiate contexts, by session and connection.
#[derive(Default)]
pub struct Contexts {
    open: HashMap<(u32, u8), Box<dyn NegotiateContext>>,
}

impl std::fmt::Debug for Contexts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Contexts")
            .field("open", &self.open.len())
            .finish()
    }
}

impl Contexts {
    /// Runs one step. A failure drops the context: the session ends.
    pub fn run(&mut self, provider: &dyn NegotiateProvider, job: SspiJob) -> SspiOutput {
        let key = (job.login, job.conn);
        match self.step(provider, key, &job) {
            Ok(out) => out,
            Err(why) => {
                self.open.remove(&key);
                SspiOutput {
                    result: SspiResult::Failed(why),
                    package: None,
                }
            }
        }
    }

    /// Forgets session `login`'s contexts.
    pub fn end(&mut self, login: u32) {
        self.open.retain(|(l, _), _| *l != login);
    }

    fn step(
        &mut self,
        provider: &dyn NegotiateProvider,
        key: (u32, u8),
        job: &SspiJob,
    ) -> Result<SspiOutput, String> {
        let plain = |result| SspiOutput {
            result,
            package: None,
        };
        if matches!(job.op, SspiOp::Start) {
            self.open.remove(&key);
            if self.open.len() >= MAX_CONTEXTS {
                return Err("too many WinRM logins at once".into());
            }
            let ctx = provider.start(&job.host, job.credentials.as_ref())?;
            self.open.insert(key, ctx);
        }
        let ctx = self.open.get_mut(&key).ok_or("the login was not started")?;
        Ok(match &job.op {
            SspiOp::Start => match ctx.step(None)? {
                ContextStep::Continue(t) => plain(SspiResult::Continue(t)),
                ContextStep::Done(t) => SspiOutput {
                    result: SspiResult::Done(t),
                    package: Some(ctx.package()),
                },
            },
            SspiOp::Step(token) => match ctx.step(Some(token))? {
                ContextStep::Continue(t) => plain(SspiResult::Continue(t)),
                ContextStep::Done(t) => SspiOutput {
                    result: SspiResult::Done(t),
                    package: Some(ctx.package()),
                },
            },
            SspiOp::Seal(data) => {
                let (header, data) = ctx.seal(data)?;
                plain(SspiResult::Sealed { header, data })
            }
            SspiOp::Unseal { header, data } => {
                plain(SspiResult::Unsealed(ctx.unseal(header, data)?))
            }
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
pub(crate) mod tests {
    use super::*;

    /// A stand-in context: one round, XOR "sealing".
    pub struct Fake {
        pub started: bool,
    }

    impl NegotiateContext for Fake {
        fn step(&mut self, input: Option<&[u8]>) -> Result<ContextStep, String> {
            match (self.started, input) {
                (false, None) => {
                    self.started = true;
                    Ok(ContextStep::Continue(b"hello".to_vec()))
                }
                (true, Some(b"challenge")) => Ok(ContextStep::Done(b"answer".to_vec())),
                _ => Err("bad step".into()),
            }
        }
        fn seal(&mut self, data: &[u8]) -> Result<(Vec<u8>, Vec<u8>), String> {
            Ok((b"sig".to_vec(), data.iter().map(|b| b ^ 1).collect()))
        }
        fn unseal(&mut self, header: &[u8], data: &[u8]) -> Result<Vec<u8>, String> {
            if header != b"sig" {
                return Err("bad signature".into());
            }
            Ok(data.iter().map(|b| b ^ 1).collect())
        }
        fn package(&self) -> String {
            "NTLM".into()
        }
    }

    pub struct FakeProvider;

    impl NegotiateProvider for FakeProvider {
        fn start(
            &self,
            host: &str,
            _c: Option<&Credentials>,
        ) -> Result<Box<dyn NegotiateContext>, String> {
            if host == "nowhere" {
                return Err("no such host".into());
            }
            Ok(Box::new(Fake { started: false }))
        }
    }

    fn job(conn: u8, op: SspiOp) -> SspiJob {
        SspiJob {
            login: 7,
            conn,
            host: "srv".into(),
            credentials: None,
            op,
        }
    }

    #[test]
    fn runs_a_login_and_seals_per_connection() {
        let mut c = Contexts::default();
        let p = FakeProvider;
        assert_eq!(
            c.run(&p, job(0, SspiOp::Start)).result,
            SspiResult::Continue(b"hello".to_vec())
        );
        // The other connection has no context yet.
        assert!(matches!(
            c.run(&p, job(1, SspiOp::Seal(b"x".to_vec()))).result,
            SspiResult::Failed(_)
        ));
        let done = c.run(&p, job(0, SspiOp::Step(b"challenge".to_vec())));
        assert_eq!(done.result, SspiResult::Done(b"answer".to_vec()));
        assert_eq!(done.package.as_deref(), Some("NTLM"));
        let SspiResult::Sealed { header, data } =
            c.run(&p, job(0, SspiOp::Seal(b"abc".to_vec()))).result
        else {
            panic!()
        };
        assert_eq!(
            c.run(&p, job(0, SspiOp::Unseal { header, data })).result,
            SspiResult::Unsealed(b"abc".to_vec())
        );
        // A bad signature ends the context.
        assert!(matches!(
            c.run(
                &p,
                job(
                    0,
                    SspiOp::Unseal {
                        header: b"x".to_vec(),
                        data: Vec::new()
                    }
                )
            )
            .result,
            SspiResult::Failed(_)
        ));
        assert!(matches!(
            c.run(&p, job(0, SspiOp::Seal(b"x".to_vec()))).result,
            SspiResult::Failed(_)
        ));
        c.end(7);
        assert_eq!(c.open.len(), 0);
    }

    #[test]
    fn refuses_new_logins_when_full_and_keeps_the_open_ones() {
        let mut c = Contexts::default();
        let p = FakeProvider;
        for login in 0..MAX_CONTEXTS as u32 {
            let mut j = job(0, SspiOp::Start);
            j.login = login;
            assert!(matches!(c.run(&p, j).result, SspiResult::Continue(_)));
        }
        let mut j = job(0, SspiOp::Start);
        j.login = 1000;
        assert!(matches!(c.run(&p, j).result, SspiResult::Failed(_)));
        assert_eq!(c.open.len(), MAX_CONTEXTS);
        // A restart of an open login still works.
        let mut j = job(0, SspiOp::Start);
        j.login = 3;
        assert!(matches!(c.run(&p, j).result, SspiResult::Continue(_)));
    }

    #[test]
    fn splits_user_names() {
        let c = |u: &str| Credentials {
            user: u.into(),
            password: Zeroizing::new(String::new()),
        };
        assert_eq!(c("LAB\\jurgen").split(), ("jurgen", "LAB"));
        assert_eq!(c("jurgen@lab.test").split(), ("jurgen@lab.test", ""));
        assert_eq!(c("admin").split(), ("admin", ""));
        assert!(!format!("{:?}", c("a")).contains("password:"));
    }
}
