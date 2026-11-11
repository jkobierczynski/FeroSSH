//! Negotiate through Windows' SSPI: Kerberos with the logon's tickets or
//! the typed credentials, NTLM where Kerberos cannot be used (a host
//! named by address, a PC outside the domain), and WinRM's message
//! encryption with the context's keys.

#![allow(unsafe_code)]

use std::ffi::c_void;

use windows::Win32::Foundation::{SEC_E_OK, SEC_I_CONTINUE_NEEDED};
use windows::Win32::Security::Authentication::Identity::{
    AcquireCredentialsHandleW, DecryptMessage, DeleteSecurityContext, EncryptMessage,
    FreeContextBuffer, FreeCredentialsHandle, ISC_REQ_ALLOCATE_MEMORY, ISC_REQ_CONFIDENTIALITY,
    ISC_REQ_FLAGS, ISC_REQ_INTEGRITY, ISC_REQ_MUTUAL_AUTH, ISC_REQ_REPLAY_DETECT,
    ISC_REQ_SEQUENCE_DETECT, ISC_RET_CONFIDENTIALITY, ISC_RET_INTEGRITY, ISC_RET_MUTUAL_AUTH,
    InitializeSecurityContextW, QueryContextAttributesW, SECBUFFER_DATA, SECBUFFER_PADDING,
    SECBUFFER_TOKEN, SECBUFFER_VERSION, SECPKG_ATTR_NEGOTIATION_INFO, SECPKG_ATTR_SIZES,
    SECPKG_CRED_OUTBOUND, SECQOP_WRAP_NO_ENCRYPT, SECURITY_NATIVE_DREP, SecBuffer, SecBufferDesc,
    SecPkgContext_NegotiationInfoW, SecPkgContext_Sizes,
};
use windows::Win32::Security::Credentials::SecHandle;
use windows::Win32::System::Rpc::{SEC_WINNT_AUTH_IDENTITY_UNICODE, SEC_WINNT_AUTH_IDENTITY_W};
use windows::core::w;
use zeroize::Zeroize;

use super::{ContextStep, Credentials, NegotiateContext, NegotiateProvider};

/// SSPI's Negotiate package.
#[derive(Debug, Clone, Copy, Default)]
pub struct Sspi;

fn utf16(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

fn len32(v: &[u16]) -> Result<u32, String> {
    u32::try_from(v.len()).map_err(|_| "user name or password too long".to_owned())
}

impl NegotiateProvider for Sspi {
    fn start(
        &self,
        host: &str,
        credentials: Option<&Credentials>,
    ) -> Result<Box<dyn NegotiateContext>, String> {
        let mut cred = SecHandle::default();
        let result = match credentials {
            None => {
                // SAFETY: plain out-parameter call; the handle is freed on
                // drop.
                unsafe {
                    AcquireCredentialsHandleW(
                        None,
                        w!("Negotiate"),
                        SECPKG_CRED_OUTBOUND,
                        None,
                        None,
                        None,
                        None,
                        &mut cred,
                        None,
                    )
                }
            }
            Some(c) => {
                let (user, domain) = c.split();
                let mut user = utf16(user);
                let mut domain = utf16(domain);
                let mut password: Vec<u16> = c.password.encode_utf16().collect();
                let identity = SEC_WINNT_AUTH_IDENTITY_W {
                    User: user.as_mut_ptr(),
                    UserLength: len32(&user)?,
                    Domain: domain.as_mut_ptr(),
                    DomainLength: len32(&domain)?,
                    Password: password.as_mut_ptr(),
                    PasswordLength: len32(&password)?,
                    Flags: SEC_WINNT_AUTH_IDENTITY_UNICODE,
                };
                // SAFETY: the identity and the strings it points at live
                // through the call; SSPI copies what it keeps.
                let r = unsafe {
                    AcquireCredentialsHandleW(
                        None,
                        w!("Negotiate"),
                        SECPKG_CRED_OUTBOUND,
                        None,
                        Some((&identity as *const SEC_WINNT_AUTH_IDENTITY_W).cast::<c_void>()),
                        None,
                        None,
                        &mut cred,
                        None,
                    )
                };
                password.zeroize();
                r
            }
        };
        result.map_err(|e| format!("no credentials to log in with ({})", e.message()))?;
        let target: Vec<u16> = format!("HTTP/{host}")
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        Ok(Box::new(Context {
            cred,
            ctx: None,
            target,
            flags: ISC_REQ_MUTUAL_AUTH
                | ISC_REQ_CONFIDENTIALITY
                | ISC_REQ_INTEGRITY
                | ISC_REQ_SEQUENCE_DETECT
                | ISC_REQ_REPLAY_DETECT
                | ISC_REQ_ALLOCATE_MEMORY,
            sizes: None,
        }))
    }
}

struct Context {
    cred: SecHandle,
    ctx: Option<SecHandle>,
    /// `HTTP/<host>`, NUL-terminated.
    target: Vec<u16>,
    flags: ISC_REQ_FLAGS,
    sizes: Option<SecPkgContext_Sizes>,
}

// SAFETY: SSPI handles are plain values, usable from any thread; the
// broker uses each context from one thread at a time.
unsafe impl Send for Context {}

impl Drop for Context {
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

fn why(code: windows::core::HRESULT) -> String {
    let text = windows::core::Error::from_hresult(code).message();
    let text = text.trim();
    if text.is_empty() {
        format!("SSPI error {:#010x}", code.0)
    } else {
        text.to_owned()
    }
}

impl Context {
    fn sizes(&mut self) -> Result<SecPkgContext_Sizes, String> {
        if let Some(s) = self.sizes {
            return Ok(s);
        }
        let ctx = self.ctx.ok_or("no security context")?;
        let mut sizes = SecPkgContext_Sizes::default();
        // SAFETY: sizes is the struct SECPKG_ATTR_SIZES fills in.
        unsafe {
            QueryContextAttributesW(
                &ctx,
                SECPKG_ATTR_SIZES,
                (&mut sizes as *mut SecPkgContext_Sizes).cast::<c_void>(),
            )
        }
        .map_err(|e| e.message())?;
        self.sizes = Some(sizes);
        Ok(sizes)
    }
}

impl NegotiateContext for Context {
    fn step(&mut self, input: Option<&[u8]>) -> Result<ContextStep, String> {
        let mut input_copy = input.map(<[u8]>::to_vec).unwrap_or_default();
        let mut in_buf = SecBuffer {
            cbBuffer: u32::try_from(input_copy.len()).map_err(|_| "token too large")?,
            BufferType: SECBUFFER_TOKEN,
            pvBuffer: input_copy.as_mut_ptr().cast(),
        };
        let in_desc = SecBufferDesc {
            ulVersion: SECBUFFER_VERSION,
            cBuffers: 1,
            pBuffers: &mut in_buf,
        };
        let mut out_buf = SecBuffer {
            cbBuffer: 0,
            BufferType: SECBUFFER_TOKEN,
            pvBuffer: std::ptr::null_mut(),
        };
        let mut out_desc = SecBufferDesc {
            ulVersion: SECBUFFER_VERSION,
            cBuffers: 1,
            pBuffers: &mut out_buf,
        };
        let mut new_ctx = self.ctx.unwrap_or_default();
        let mut attrs = 0u32;
        // SAFETY: the buffers outlive the call; SSPI allocates the output
        // token (ISC_REQ_ALLOCATE_MEMORY), freed below.
        let status = unsafe {
            InitializeSecurityContextW(
                Some(&self.cred),
                self.ctx.as_ref().map(|c| c as *const SecHandle),
                Some(self.target.as_ptr()),
                self.flags,
                0,
                SECURITY_NATIVE_DREP,
                input.map(|_| &in_desc as *const SecBufferDesc),
                0,
                Some(&mut new_ctx),
                Some(&mut out_desc),
                &mut attrs,
                None,
            )
        };
        let token = if out_buf.pvBuffer.is_null() {
            Vec::new()
        } else {
            // SAFETY: SSPI wrote cbBuffer bytes there.
            let t = unsafe {
                std::slice::from_raw_parts(out_buf.pvBuffer as *const u8, out_buf.cbBuffer as usize)
            }
            .to_vec();
            // SAFETY: allocated by SSPI for this call.
            unsafe {
                let _ = FreeContextBuffer(out_buf.pvBuffer);
            }
            t
        };
        if status == SEC_E_OK || status == SEC_I_CONTINUE_NEEDED {
            self.ctx = Some(new_ctx);
        }
        if status == SEC_I_CONTINUE_NEEDED {
            return Ok(ContextStep::Continue(token));
        }
        if status != SEC_E_OK {
            return Err(why(status));
        }
        // Every message is encrypted and signed with the context's keys.
        if attrs & ISC_RET_CONFIDENTIALITY == 0 || attrs & ISC_RET_INTEGRITY == 0 {
            return Err("the login gives no encryption".into());
        }
        // Kerberos proves the server holds the service's key; NTLM cannot.
        if self.package().eq_ignore_ascii_case("Kerberos") && attrs & ISC_RET_MUTUAL_AUTH == 0 {
            return Err("the server did not authenticate itself".into());
        }
        Ok(ContextStep::Done(token))
    }

    fn seal(&mut self, data: &[u8]) -> Result<(Vec<u8>, Vec<u8>), String> {
        let sizes = self.sizes()?;
        let ctx = self.ctx.ok_or("no security context")?;
        let mut header = vec![0u8; sizes.cbSecurityTrailer as usize];
        let mut message = data.to_vec();
        let mut padding = vec![0u8; sizes.cbBlockSize as usize];
        let mut bufs = [
            SecBuffer {
                cbBuffer: sizes.cbSecurityTrailer,
                BufferType: SECBUFFER_TOKEN,
                pvBuffer: header.as_mut_ptr().cast(),
            },
            SecBuffer {
                cbBuffer: u32::try_from(message.len()).map_err(|_| "message too large")?,
                BufferType: SECBUFFER_DATA,
                pvBuffer: message.as_mut_ptr().cast(),
            },
            SecBuffer {
                cbBuffer: sizes.cbBlockSize,
                BufferType: SECBUFFER_PADDING,
                pvBuffer: padding.as_mut_ptr().cast(),
            },
        ];
        let desc = SecBufferDesc {
            ulVersion: SECBUFFER_VERSION,
            cBuffers: 3,
            pBuffers: bufs.as_mut_ptr(),
        };
        // SAFETY: the buffers outlive the call and are as large as SSPI
        // said; it encrypts in place and sets each one's length.
        let status = unsafe { EncryptMessage(&ctx, 0, &desc, 0) };
        if status != SEC_E_OK {
            return Err(why(status));
        }
        let [h, m, p] = &bufs;
        header.truncate(h.cbBuffer as usize);
        message.truncate(m.cbBuffer as usize);
        padding.truncate(p.cbBuffer as usize);
        message.extend_from_slice(&padding);
        Ok((header, message))
    }

    fn unseal(&mut self, header: &[u8], data: &[u8]) -> Result<Vec<u8>, String> {
        let ctx = self.ctx.ok_or("no security context")?;
        let mut header = header.to_vec();
        let mut message = data.to_vec();
        let mut bufs = [
            SecBuffer {
                cbBuffer: u32::try_from(header.len()).map_err(|_| "signature too large")?,
                BufferType: SECBUFFER_TOKEN,
                pvBuffer: header.as_mut_ptr().cast(),
            },
            SecBuffer {
                cbBuffer: u32::try_from(message.len()).map_err(|_| "message too large")?,
                BufferType: SECBUFFER_DATA,
                pvBuffer: message.as_mut_ptr().cast(),
            },
        ];
        let desc = SecBufferDesc {
            ulVersion: SECBUFFER_VERSION,
            cBuffers: 2,
            pBuffers: bufs.as_mut_ptr(),
        };
        let mut qop = 0u32;
        // SAFETY: the buffers outlive the call; SSPI decrypts in place.
        let status = unsafe { DecryptMessage(&ctx, &desc, 0, Some(&mut qop)) };
        if status != SEC_E_OK {
            return Err(format!("a reply failed its check ({})", why(status)));
        }
        if qop & SECQOP_WRAP_NO_ENCRYPT == SECQOP_WRAP_NO_ENCRYPT {
            return Err("a reply was signed but not encrypted".into());
        }
        let len = bufs.get(1).map_or(0, |b| b.cbBuffer as usize);
        message.truncate(len);
        Ok(message)
    }

    fn package(&self) -> String {
        let Some(ctx) = self.ctx else {
            return String::new();
        };
        let mut info = SecPkgContext_NegotiationInfoW::default();
        // SAFETY: info is the struct SECPKG_ATTR_NEGOTIATION_INFO fills
        // in; its PackageInfo is SSPI's, freed below.
        let ok = unsafe {
            QueryContextAttributesW(
                &ctx,
                SECPKG_ATTR_NEGOTIATION_INFO,
                (&mut info as *mut SecPkgContext_NegotiationInfoW).cast::<c_void>(),
            )
        };
        if ok.is_err() || info.PackageInfo.is_null() {
            return String::new();
        }
        // SAFETY: SSPI returned a valid SecPkgInfoW with a NUL-terminated
        // name, freed once here.
        unsafe {
            let name = (*info.PackageInfo).Name;
            let text = if name.is_null() {
                String::new()
            } else {
                windows::core::PCWSTR(name).to_string().unwrap_or_default()
            };
            let _ = FreeContextBuffer(info.PackageInfo.cast());
            text
        }
    }
}
