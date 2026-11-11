//! TLS for WinRM over HTTPS through Windows' SChannel: the handshake,
//! the records and the server's certificate, in the UI process. The
//! worker only carries the bytes.
//!
//! SChannel checks nothing about the certificate itself here
//! (`SCH_CRED_MANUAL_CRED_VALIDATION`): once the handshake is done,
//! [`TlsContext::certificate`] asks Windows whether it chains to a
//! trusted root and names the host, and the UI decides (it may know the
//! certificate from an earlier "yes").

#![allow(unsafe_code)]

use std::ffi::c_void;

use windows::Win32::Foundation::{
    SEC_E_INCOMPLETE_MESSAGE, SEC_E_OK, SEC_I_CONTEXT_EXPIRED, SEC_I_CONTINUE_NEEDED,
    SEC_I_INCOMPLETE_CREDENTIALS, SEC_I_RENEGOTIATE,
};
use windows::Win32::Security::Authentication::Identity::{
    AcquireCredentialsHandleW, DecryptMessage, DeleteSecurityContext, EncryptMessage,
    FreeContextBuffer, FreeCredentialsHandle, ISC_REQ_ALLOCATE_MEMORY, ISC_REQ_CONFIDENTIALITY,
    ISC_REQ_EXTENDED_ERROR, ISC_REQ_FLAGS, ISC_REQ_REPLAY_DETECT, ISC_REQ_SEQUENCE_DETECT,
    ISC_REQ_STREAM, ISC_REQ_USE_SUPPLIED_CREDS, InitializeSecurityContextW,
    QueryContextAttributesW, SCH_CRED_MANUAL_CRED_VALIDATION, SCH_CRED_NO_DEFAULT_CREDS,
    SCH_CREDENTIALS, SCH_CREDENTIALS_VERSION, SCH_USE_STRONG_CRYPTO, SECBUFFER_ALERT,
    SECBUFFER_DATA, SECBUFFER_EMPTY, SECBUFFER_EXTRA, SECBUFFER_STREAM_HEADER,
    SECBUFFER_STREAM_TRAILER, SECBUFFER_TOKEN, SECBUFFER_VERSION, SECPKG_ATTR_REMOTE_CERT_CONTEXT,
    SECPKG_ATTR_STREAM_SIZES, SECPKG_CRED_OUTBOUND, SecBuffer, SecBufferDesc,
    SecPkgContext_StreamSizes, UNISP_NAME,
};
use windows::Win32::Security::Credentials::SecHandle;
use windows::Win32::Security::Cryptography::{
    AUTHTYPE_SERVER, CERT_CHAIN_CONTEXT, CERT_CHAIN_PARA, CERT_CHAIN_POLICY_PARA,
    CERT_CHAIN_POLICY_SSL, CERT_CHAIN_POLICY_STATUS, CERT_CONTEXT, CERT_SHA256_HASH_PROP_ID,
    CERT_USAGE_MATCH, CTL_USAGE, CertFreeCertificateChain, CertFreeCertificateContext,
    CertGetCertificateChain, CertGetCertificateContextProperty, CertVerifyCertificateChainPolicy,
    HTTPSPolicyCallbackData, USAGE_MATCH_TYPE_AND, szOID_PKIX_KP_SERVER_AUTH,
};
use windows::core::{PSTR, PWSTR};

use super::sspi::Sspi;
use super::{ServerCert, TlsContext, TlsOutput};

/// How much of the server's bytes a connection keeps waiting for the
/// rest of a record: a record is at most 16 KiB plus its overhead.
const MAX_PENDING: usize = 64 * 1024;

/// Handshake rounds before a server counts as confused.
const MAX_ROUNDS: u32 = 32;

fn why(code: windows::core::HRESULT) -> String {
    let text = windows::core::Error::from_hresult(code).message();
    let text = text.trim();
    if text.is_empty() {
        format!("SChannel error {:#010x}", code.0)
    } else {
        text.to_owned()
    }
}

impl Sspi {
    /// A TLS connection to `host`, through SChannel.
    pub(super) fn tls_connection(host: &str) -> Result<Box<dyn TlsContext>, String> {
        let creds = SCH_CREDENTIALS {
            dwVersion: SCH_CREDENTIALS_VERSION,
            dwFlags: SCH_CRED_MANUAL_CRED_VALIDATION.0
                | SCH_CRED_NO_DEFAULT_CREDS.0
                | SCH_USE_STRONG_CRYPTO.0,
            ..SCH_CREDENTIALS::default()
        };
        let mut cred = SecHandle::default();
        // SAFETY: creds lives through the call; SChannel copies it.
        unsafe {
            AcquireCredentialsHandleW(
                None,
                UNISP_NAME,
                SECPKG_CRED_OUTBOUND,
                None,
                Some((&creds as *const SCH_CREDENTIALS).cast::<c_void>()),
                None,
                None,
                &mut cred,
                None,
            )
        }
        .map_err(|e| format!("TLS is not available ({})", e.message()))?;
        let host: Vec<u16> = host.encode_utf16().chain(std::iter::once(0)).collect();
        Ok(Box::new(Connection {
            cred,
            ctx: None,
            host,
            pending: Vec::new(),
            ready: false,
            sizes: None,
            rounds: 0,
        }))
    }
}

struct Connection {
    cred: SecHandle,
    ctx: Option<SecHandle>,
    /// The host's name, NUL-terminated: SNI and the certificate check.
    host: Vec<u16>,
    /// The server's bytes not used yet (part of a record).
    pending: Vec<u8>,
    ready: bool,
    sizes: Option<SecPkgContext_StreamSizes>,
    rounds: u32,
}

// SAFETY: SSPI handles are plain values, usable from any thread; the
// broker uses each connection from one thread at a time.
unsafe impl Send for Connection {}

impl Drop for Connection {
    fn drop(&mut self) {
        // SAFETY: both handles came from SSPI and are freed once.
        unsafe {
            if let Some(ctx) = self.ctx.take() {
                let _ = DeleteSecurityContext(&ctx);
            }
            let _ = FreeCredentialsHandle(&self.cred);
        }
    }
}

/// Copies and frees a token SSPI allocated.
fn take_token(buf: &SecBuffer) -> Vec<u8> {
    if buf.pvBuffer.is_null() || buf.cbBuffer == 0 {
        return Vec::new();
    }
    // SAFETY: SSPI wrote cbBuffer bytes there and allocated them for this
    // call (ISC_REQ_ALLOCATE_MEMORY); freed once here.
    unsafe {
        let t =
            std::slice::from_raw_parts(buf.pvBuffer as *const u8, buf.cbBuffer as usize).to_vec();
        let _ = FreeContextBuffer(buf.pvBuffer);
        t
    }
}

/// The length of a SECBUFFER_EXTRA among `bufs`: the input's last bytes
/// that SSPI left for the next call.
fn extra(bufs: &[SecBuffer]) -> usize {
    bufs.iter()
        .find(|b| b.BufferType == SECBUFFER_EXTRA)
        .map_or(0, |b| b.cbBuffer as usize)
}

impl Connection {
    fn flags() -> ISC_REQ_FLAGS {
        ISC_REQ_SEQUENCE_DETECT
            | ISC_REQ_REPLAY_DETECT
            | ISC_REQ_CONFIDENTIALITY
            | ISC_REQ_EXTENDED_ERROR
            | ISC_REQ_ALLOCATE_MEMORY
            | ISC_REQ_STREAM
            | ISC_REQ_USE_SUPPLIED_CREDS
    }

    /// One handshake call, with `pending` as the input (none at first).
    /// Returns the bytes for the server and the status.
    fn handshake_call(&mut self, first: bool) -> (Vec<u8>, windows::core::HRESULT) {
        let mut in_bufs = [
            SecBuffer {
                cbBuffer: u32::try_from(self.pending.len()).unwrap_or(u32::MAX),
                BufferType: SECBUFFER_TOKEN,
                pvBuffer: self.pending.as_mut_ptr().cast(),
            },
            SecBuffer {
                cbBuffer: 0,
                BufferType: SECBUFFER_EMPTY,
                pvBuffer: std::ptr::null_mut(),
            },
        ];
        let in_desc = SecBufferDesc {
            ulVersion: SECBUFFER_VERSION,
            cBuffers: 2,
            pBuffers: in_bufs.as_mut_ptr(),
        };
        let mut out_bufs = [
            SecBuffer {
                cbBuffer: 0,
                BufferType: SECBUFFER_TOKEN,
                pvBuffer: std::ptr::null_mut(),
            },
            SecBuffer {
                cbBuffer: 0,
                BufferType: SECBUFFER_ALERT,
                pvBuffer: std::ptr::null_mut(),
            },
        ];
        let mut out_desc = SecBufferDesc {
            ulVersion: SECBUFFER_VERSION,
            cBuffers: 2,
            pBuffers: out_bufs.as_mut_ptr(),
        };
        let mut new_ctx = self.ctx.unwrap_or_default();
        let mut attrs = 0u32;
        // SAFETY: the buffers outlive the call; SSPI allocates the output
        // tokens (ISC_REQ_ALLOCATE_MEMORY), freed by take_token.
        let status = unsafe {
            InitializeSecurityContextW(
                Some(&self.cred),
                self.ctx.as_ref().map(|c| c as *const SecHandle),
                Some(self.host.as_ptr()),
                Self::flags(),
                0,
                0,
                (!first).then_some(&in_desc as *const SecBufferDesc),
                0,
                Some(&mut new_ctx),
                Some(&mut out_desc),
                &mut attrs,
                None,
            )
        };
        let [token, alert] = &out_bufs;
        let send = take_token(token);
        let _ = take_token(alert);
        if status == SEC_E_OK
            || status == SEC_I_CONTINUE_NEEDED
            || status == SEC_I_INCOMPLETE_CREDENTIALS
        {
            self.ctx = Some(new_ctx);
        }
        if status != SEC_E_INCOMPLETE_MESSAGE && !first {
            // What the call did not use stays for the next one.
            let keep = extra(&in_bufs);
            let used = self.pending.len().saturating_sub(keep);
            self.pending.drain(..used);
        }
        (send, status)
    }

    /// Runs the handshake as far as the bytes in `pending` go.
    fn handshake(&mut self, out: &mut TlsOutput) -> Result<(), String> {
        loop {
            self.rounds += 1;
            if self.rounds > MAX_ROUNDS {
                return Err("the TLS handshake took too many rounds".into());
            }
            let (send, status) = self.handshake_call(false);
            out.send.extend_from_slice(&send);
            if status == SEC_E_OK {
                self.ready = true;
                return Ok(());
            }
            if status == SEC_E_INCOMPLETE_MESSAGE {
                return Ok(());
            }
            if status == SEC_I_CONTINUE_NEEDED || status == SEC_I_INCOMPLETE_CREDENTIALS {
                // No client certificate to give: SChannel goes on without.
                if self.pending.is_empty() && status == SEC_I_CONTINUE_NEEDED {
                    return Ok(());
                }
                continue;
            }
            return Err(format!("the TLS handshake failed ({})", why(status)));
        }
    }

    fn sizes(&mut self) -> Result<SecPkgContext_StreamSizes, String> {
        if let Some(s) = self.sizes {
            return Ok(s);
        }
        let ctx = self.ctx.ok_or("no TLS connection")?;
        let mut sizes = SecPkgContext_StreamSizes::default();
        // SAFETY: sizes is the struct SECPKG_ATTR_STREAM_SIZES fills in.
        unsafe {
            QueryContextAttributesW(
                &ctx,
                SECPKG_ATTR_STREAM_SIZES,
                (&mut sizes as *mut SecPkgContext_StreamSizes).cast::<c_void>(),
            )
        }
        .map_err(|e| e.message())?;
        self.sizes = Some(sizes);
        Ok(sizes)
    }

    /// Decrypts the whole records in `pending`.
    fn decrypt(&mut self, out: &mut TlsOutput) -> Result<(), String> {
        let ctx = self.ctx.ok_or("no TLS connection")?;
        while !self.pending.is_empty() && !out.closed {
            let mut bufs = [
                SecBuffer {
                    cbBuffer: u32::try_from(self.pending.len()).map_err(|_| "too much data")?,
                    BufferType: SECBUFFER_DATA,
                    pvBuffer: self.pending.as_mut_ptr().cast(),
                },
                SecBuffer::default(),
                SecBuffer::default(),
                SecBuffer::default(),
            ];
            let desc = SecBufferDesc {
                ulVersion: SECBUFFER_VERSION,
                cBuffers: 4,
                pBuffers: bufs.as_mut_ptr(),
            };
            // SAFETY: the buffers point into `pending`, which outlives the
            // call; SChannel decrypts in place and points DATA and EXTRA
            // into it.
            let status = unsafe { DecryptMessage(&ctx, &desc, 0, None) };
            if status == SEC_E_INCOMPLETE_MESSAGE {
                return Ok(());
            }
            if status != SEC_E_OK && status != SEC_I_RENEGOTIATE && status != SEC_I_CONTEXT_EXPIRED
            {
                return Err(format!("a TLS record failed its check ({})", why(status)));
            }
            let base = self.pending.as_ptr() as usize;
            let len = self.pending.len();
            for b in &bufs {
                if b.BufferType == SECBUFFER_DATA && !b.pvBuffer.is_null() {
                    let start = (b.pvBuffer as usize)
                        .checked_sub(base)
                        .ok_or("bad record")?;
                    let end = start + b.cbBuffer as usize;
                    let data = self.pending.get(start..end).ok_or("bad record")?;
                    out.plain.extend_from_slice(data);
                }
            }
            let keep = extra(&bufs);
            self.pending.drain(..len.saturating_sub(keep));
            if status == SEC_I_CONTEXT_EXPIRED {
                out.closed = true;
            } else if status == SEC_I_RENEGOTIATE {
                // TLS 1.3 sends tickets and key updates after the
                // handshake: they go through the handshake call, with
                // what follows them.
                let (send, status) = self.handshake_call(false);
                out.send.extend_from_slice(&send);
                if status != SEC_E_OK && status != SEC_I_CONTINUE_NEEDED {
                    return Err(format!("the server's TLS update failed ({})", why(status)));
                }
            }
        }
        Ok(())
    }
}

impl TlsContext for Connection {
    fn start(&mut self) -> Result<Vec<u8>, String> {
        let (send, status) = self.handshake_call(true);
        if status != SEC_I_CONTINUE_NEEDED {
            return Err(format!("TLS could not start ({})", why(status)));
        }
        Ok(send)
    }

    fn received(&mut self, data: &[u8]) -> Result<TlsOutput, String> {
        if self.pending.len() + data.len() > MAX_PENDING {
            return Err("the server sent too much without a whole TLS record".into());
        }
        self.pending.extend_from_slice(data);
        let mut out = TlsOutput::default();
        if !self.ready {
            self.handshake(&mut out)?;
        }
        if self.ready {
            // What came after the handshake's last message is records.
            self.decrypt(&mut out)?;
        }
        out.ready = self.ready;
        Ok(out)
    }

    fn send(&mut self, plain: &[u8]) -> Result<Vec<u8>, String> {
        let sizes = self.sizes()?;
        let ctx = self.ctx.ok_or("no TLS connection")?;
        let max = (sizes.cbMaximumMessage as usize).max(1);
        let mut out = Vec::new();
        for chunk in plain.chunks(max) {
            let (h, t) = (sizes.cbHeader as usize, sizes.cbTrailer as usize);
            let mut record = vec![0u8; h + chunk.len() + t];
            record
                .get_mut(h..h + chunk.len())
                .ok_or("bad record")?
                .copy_from_slice(chunk);
            let base = record.as_mut_ptr();
            // SAFETY: the three buffers are disjoint parts of `record`.
            let mut bufs = unsafe {
                [
                    SecBuffer {
                        cbBuffer: sizes.cbHeader,
                        BufferType: SECBUFFER_STREAM_HEADER,
                        pvBuffer: base.cast(),
                    },
                    SecBuffer {
                        cbBuffer: u32::try_from(chunk.len()).map_err(|_| "too much data")?,
                        BufferType: SECBUFFER_DATA,
                        pvBuffer: base.add(h).cast(),
                    },
                    SecBuffer {
                        cbBuffer: sizes.cbTrailer,
                        BufferType: SECBUFFER_STREAM_TRAILER,
                        pvBuffer: base.add(h + chunk.len()).cast(),
                    },
                    SecBuffer::default(),
                ]
            };
            let desc = SecBufferDesc {
                ulVersion: SECBUFFER_VERSION,
                cBuffers: 4,
                pBuffers: bufs.as_mut_ptr(),
            };
            // SAFETY: the buffers outlive the call and are as large as
            // SChannel said; it encrypts in place.
            let status = unsafe { EncryptMessage(&ctx, 0, &desc, 0) };
            if status != SEC_E_OK {
                return Err(format!("TLS encryption failed ({})", why(status)));
            }
            let [hb, db, tb, _] = &bufs;
            let used = (hb.cbBuffer + db.cbBuffer + tb.cbBuffer) as usize;
            record.truncate(used);
            out.extend_from_slice(&record);
        }
        Ok(out)
    }

    fn certificate(&self) -> Result<ServerCert, String> {
        let ctx = self.ctx.ok_or("no TLS connection")?;
        let mut cert: *mut CERT_CONTEXT = std::ptr::null_mut();
        // SAFETY: SECPKG_ATTR_REMOTE_CERT_CONTEXT fills in a certificate
        // context pointer, freed below.
        unsafe {
            QueryContextAttributesW(
                &ctx,
                SECPKG_ATTR_REMOTE_CERT_CONTEXT,
                (&mut cert as *mut *mut CERT_CONTEXT).cast::<c_void>(),
            )
        }
        .map_err(|e| format!("the server sent no certificate ({})", e.message()))?;
        if cert.is_null() {
            return Err("the server sent no certificate".into());
        }
        let result = (|| {
            let mut hash = [0u8; 32];
            let mut len = 32u32;
            // SAFETY: cert is a valid context; hash holds 32 bytes.
            unsafe {
                CertGetCertificateContextProperty(
                    cert,
                    CERT_SHA256_HASH_PROP_ID,
                    Some(hash.as_mut_ptr().cast()),
                    &mut len,
                )
            }
            .map_err(|e| format!("no fingerprint ({})", e.message()))?;
            let sha256 = hash
                .iter()
                .map(|b| format!("{b:02X}"))
                .collect::<Vec<_>>()
                .join(":");
            Ok(ServerCert {
                sha256,
                windows: self.verify(cert),
            })
        })();
        // SAFETY: the context SChannel handed out, freed once.
        unsafe {
            let _ = CertFreeCertificateContext(Some(cert));
        }
        result
    }
}

impl Connection {
    /// Windows' verdict on `cert` for this host: a chain to a trusted
    /// root, for server authentication, naming the host.
    fn verify(&self, cert: *const CERT_CONTEXT) -> Result<(), String> {
        let mut usage = [PSTR(szOID_PKIX_KP_SERVER_AUTH.0.cast_mut())];
        let para = CERT_CHAIN_PARA {
            cbSize: u32::try_from(std::mem::size_of::<CERT_CHAIN_PARA>()).unwrap_or(0),
            RequestedUsage: CERT_USAGE_MATCH {
                dwType: USAGE_MATCH_TYPE_AND,
                Usage: CTL_USAGE {
                    cUsageIdentifier: 1,
                    rgpszUsageIdentifier: usage.as_mut_ptr(),
                },
            },
            ..CERT_CHAIN_PARA::default()
        };
        let mut chain: *mut CERT_CHAIN_CONTEXT = std::ptr::null_mut();
        // SAFETY: cert is valid; the server's other certificates are in
        // its store; the chain is freed below.
        unsafe {
            // SAFETY: cert came from SChannel and is valid here.
            let store = (*cert).hCertStore;
            CertGetCertificateChain(None, cert, None, Some(store), &para, 0, None, &mut chain)
        }
        .map_err(|e| format!("no certificate chain ({})", e.message()))?;
        let mut host = self.host.clone();
        let mut ssl = HTTPSPolicyCallbackData {
            dwAuthType: AUTHTYPE_SERVER,
            pwszServerName: PWSTR(host.as_mut_ptr()),
            ..HTTPSPolicyCallbackData::default()
        };
        ssl.Anonymous.cbSize =
            u32::try_from(std::mem::size_of::<HTTPSPolicyCallbackData>()).unwrap_or(0);
        let policy = CERT_CHAIN_POLICY_PARA {
            cbSize: u32::try_from(std::mem::size_of::<CERT_CHAIN_POLICY_PARA>()).unwrap_or(0),
            pvExtraPolicyPara: (&mut ssl as *mut HTTPSPolicyCallbackData).cast(),
            ..CERT_CHAIN_POLICY_PARA::default()
        };
        let mut status = CERT_CHAIN_POLICY_STATUS {
            cbSize: u32::try_from(std::mem::size_of::<CERT_CHAIN_POLICY_STATUS>()).unwrap_or(0),
            ..CERT_CHAIN_POLICY_STATUS::default()
        };
        // SAFETY: chain, policy and status are valid for the call; the
        // chain is freed once afterwards.
        let ok = unsafe {
            let ok = CertVerifyCertificateChainPolicy(
                CERT_CHAIN_POLICY_SSL,
                chain,
                &policy,
                &mut status,
            );
            CertFreeCertificateChain(chain);
            ok
        };
        if !ok.as_bool() {
            return Err("Windows could not check the certificate".into());
        }
        if status.dwError != 0 {
            #[allow(clippy::cast_possible_wrap)]
            let code = windows::core::HRESULT(status.dwError as i32);
            return Err(why(code));
        }
        Ok(())
    }
}
