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
//!
//! Over HTTPS the TLS connection is held here too (Windows' SChannel):
//! the worker moves the server's bytes in and the encrypted bytes out,
//! the handshake, the keys and the check of the server's certificate
//! stay in this process.

use std::collections::HashMap;

use ferossh_ipc::{SspiOp, SspiResult};
use zeroize::Zeroizing;

pub use crate::gss::ContextStep;

#[cfg(windows)]
pub mod schannel;
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

/// What a TLS connection made of the bytes it was given.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TlsOutput {
    /// Bytes for the server.
    pub send: Vec<u8>,
    /// Plain bytes the server's records held.
    pub plain: Vec<u8>,
    /// The handshake is done.
    pub ready: bool,
    /// The server closed the TLS connection.
    pub closed: bool,
}

/// The certificate a TLS server showed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerCert {
    /// Its SHA-256 fingerprint, as `AB:CD:...`.
    pub sha256: String,
    /// Windows' verdict for this host name: `Ok` when it chains to a
    /// trusted root and names the host, else why not.
    pub windows: Result<(), String>,
    /// It is within its validity period now.
    pub current: bool,
}

/// One TLS connection, client side.
pub trait TlsContext: Send {
    /// The first bytes for the server (the ClientHello).
    fn start(&mut self) -> Result<Vec<u8>, String>;
    /// Bytes the server sent: the handshake's, then records.
    fn received(&mut self, data: &[u8]) -> Result<TlsOutput, String>;
    /// Encrypts plain bytes for the server.
    fn send(&mut self, plain: &[u8]) -> Result<Vec<u8>, String>;
    /// The server's certificate, once the handshake is done.
    fn certificate(&self) -> Result<ServerCert, String>;
    /// The connection's channel bindings (RFC 5929 `tls-server-end-point`),
    /// as SSPI hands them to a login over it, once the handshake is done.
    fn channel_bindings(&self) -> Result<Vec<u8>, String> {
        Err("no channel bindings".into())
    }
}

/// Makes Negotiate contexts.
pub trait NegotiateProvider: Send {
    /// A context for the `HTTP` service on `host`. Over HTTPS,
    /// `bindings` are the TLS connection's channel bindings, which tie the
    /// login to it (Extended Protection for Authentication).
    fn start(
        &self,
        host: &str,
        credentials: Option<&Credentials>,
        bindings: Option<&[u8]>,
    ) -> Result<Box<dyn NegotiateContext>, String>;

    /// A TLS connection to `host`.
    fn tls(&self, _host: &str) -> Result<Box<dyn TlsContext>, String> {
        Err("HTTPS needs Windows".into())
    }
}

/// No SSPI on this system.
#[derive(Debug, Clone, Copy, Default)]
pub struct Unavailable;

impl NegotiateProvider for Unavailable {
    fn start(
        &self,
        _host: &str,
        _credentials: Option<&Credentials>,
        _bindings: Option<&[u8]>,
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

/// What a [`SspiJob`] gave: the answer for the worker, the package once
/// a login is complete, and the server's certificate once a TLS
/// handshake is: the UI checks it before the worker gets the answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SspiOutput {
    pub result: SspiResult,
    pub package: Option<String>,
    pub cert: Option<ServerCert>,
}

/// The broker's Negotiate contexts and TLS connections, by session and
/// connection.
#[derive(Default)]
pub struct Contexts {
    open: HashMap<(u32, u8), Box<dyn NegotiateContext>>,
    tls: HashMap<(u32, u8), Tls>,
}

/// A TLS connection and whether its handshake was reported done.
struct Tls {
    ctx: Box<dyn TlsContext>,
    ready: bool,
}

impl std::fmt::Debug for Contexts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Contexts")
            .field("open", &self.open.len())
            .field("tls", &self.tls.len())
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
                if matches!(
                    job.op,
                    SspiOp::TlsStart | SspiOp::TlsReceived(_) | SspiOp::TlsSend(_)
                ) {
                    self.tls.remove(&key);
                } else {
                    self.open.remove(&key);
                }
                SspiOutput {
                    result: SspiResult::Failed(why),
                    package: None,
                    cert: None,
                }
            }
        }
    }

    /// Forgets session `login`'s contexts.
    pub fn end(&mut self, login: u32) {
        self.open.retain(|(l, _), _| *l != login);
        self.tls.retain(|(l, _), _| *l != login);
    }

    fn tls_step(
        &mut self,
        provider: &dyn NegotiateProvider,
        key: (u32, u8),
        job: &SspiJob,
    ) -> Result<SspiOutput, String> {
        let answer = |out: TlsOutput| SspiResult::Tls {
            send: out.send,
            plain: out.plain,
            ready: out.ready,
            closed: out.closed,
        };
        if matches!(job.op, SspiOp::TlsStart) {
            self.tls.remove(&key);
            if self.open.len() + self.tls.len() >= MAX_CONTEXTS {
                return Err("too many WinRM connections at once".into());
            }
            let mut ctx = provider.tls(&job.host)?;
            let hello = ctx.start()?;
            self.tls.insert(key, Tls { ctx, ready: false });
            return Ok(SspiOutput {
                result: answer(TlsOutput {
                    send: hello,
                    ..TlsOutput::default()
                }),
                package: None,
                cert: None,
            });
        }
        let tls = self
            .tls
            .get_mut(&key)
            .ok_or("the TLS connection was not started")?;
        match &job.op {
            SspiOp::TlsReceived(data) => {
                let out = tls.ctx.received(data)?;
                // The certificate goes with the answer that first says
                // ready, for the UI to check before the worker sees it.
                let cert = if out.ready && !tls.ready {
                    tls.ready = true;
                    Some(tls.ctx.certificate()?)
                } else {
                    None
                };
                Ok(SspiOutput {
                    result: answer(out),
                    package: None,
                    cert,
                })
            }
            SspiOp::TlsSend(plain) => {
                if !tls.ready {
                    return Err("data before the TLS handshake".into());
                }
                Ok(SspiOutput {
                    result: answer(TlsOutput {
                        send: tls.ctx.send(plain)?,
                        ready: true,
                        ..TlsOutput::default()
                    }),
                    package: None,
                    cert: None,
                })
            }
            _ => Err("not a TLS operation".into()),
        }
    }

    fn step(
        &mut self,
        provider: &dyn NegotiateProvider,
        key: (u32, u8),
        job: &SspiJob,
    ) -> Result<SspiOutput, String> {
        if matches!(
            job.op,
            SspiOp::TlsStart | SspiOp::TlsReceived(_) | SspiOp::TlsSend(_)
        ) {
            return self.tls_step(provider, key, job);
        }
        let plain = |result| SspiOutput {
            result,
            package: None,
            cert: None,
        };
        if matches!(job.op, SspiOp::Start) {
            self.open.remove(&key);
            if self.open.len() + self.tls.len() >= MAX_CONTEXTS {
                return Err("too many WinRM logins at once".into());
            }
            // A login on a TLS connection is bound to it.
            let bindings = match self.tls.get(&key) {
                Some(tls) if tls.ready => Some(tls.ctx.channel_bindings()?),
                Some(_) => return Err("a login before the TLS handshake".into()),
                None => None,
            };
            let ctx = provider.start(&job.host, job.credentials.as_ref(), bindings.as_deref())?;
            self.open.insert(key, ctx);
        }
        let ctx = self.open.get_mut(&key).ok_or("the login was not started")?;
        Ok(match &job.op {
            SspiOp::Start => match ctx.step(None)? {
                ContextStep::Continue(t) => plain(SspiResult::Continue(t)),
                ContextStep::Done(t) => SspiOutput {
                    result: SspiResult::Done(t),
                    package: Some(ctx.package()),
                    cert: None,
                },
            },
            SspiOp::Step(token) => match ctx.step(Some(token))? {
                ContextStep::Continue(t) => plain(SspiResult::Continue(t)),
                ContextStep::Done(t) => SspiOutput {
                    result: SspiResult::Done(t),
                    package: Some(ctx.package()),
                    cert: None,
                },
            },
            SspiOp::Seal(data) => {
                let (header, data) = ctx.seal(data)?;
                plain(SspiResult::Sealed { header, data })
            }
            SspiOp::Unseal { header, data } => {
                plain(SspiResult::Unsealed(ctx.unseal(header, data)?))
            }
            SspiOp::TlsStart | SspiOp::TlsReceived(_) | SspiOp::TlsSend(_) => {
                return Err("not a Negotiate operation".into());
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
            _b: Option<&[u8]>,
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
