//! Kerberos through Windows' SSPI (the `Kerberos` package), with the
//! tickets of the user's Windows logon, as PuTTY does: no password, no
//! keytab, nothing stored by FeroSSH.

#![allow(unsafe_code)]

use std::ffi::c_void;

use windows::Win32::Foundation::{SEC_E_OK, SEC_I_CONTINUE_NEEDED};
use windows::Win32::Security::Authentication::Identity::{
    AcquireCredentialsHandleW, DeleteSecurityContext, FreeContextBuffer, FreeCredentialsHandle,
    ISC_REQ_ALLOCATE_MEMORY, ISC_REQ_DELEGATE, ISC_REQ_FLAGS, ISC_REQ_INTEGRITY,
    ISC_REQ_MUTUAL_AUTH, ISC_RET_INTEGRITY, ISC_RET_MUTUAL_AUTH, InitializeSecurityContextW,
    MakeSignature, QueryContextAttributesW, SECBUFFER_DATA, SECBUFFER_TOKEN, SECBUFFER_VERSION,
    SECPKG_ATTR_SIZES, SECPKG_CRED_OUTBOUND, SECURITY_NATIVE_DREP, SecBuffer, SecBufferDesc,
    SecPkgContext_Sizes,
};
use windows::Win32::Security::Credentials::SecHandle;
use windows::core::w;

use super::{ContextStep, GssContext, GssProvider};

/// The Windows logon session's Kerberos tickets.
#[derive(Debug, Clone, Copy, Default)]
pub struct Sspi;

impl GssProvider for Sspi {
    fn start(&self, host: &str, delegate: bool) -> Result<Box<dyn GssContext>, String> {
        let mut cred = SecHandle::default();
        // SAFETY: plain out-parameter call; the handle is freed on drop.
        unsafe {
            AcquireCredentialsHandleW(
                None,
                w!("Kerberos"),
                SECPKG_CRED_OUTBOUND,
                None,
                None,
                None,
                None,
                &mut cred,
                None,
            )
        }
        .map_err(|e| format!("no Kerberos credentials ({})", e.message()))?;
        let mut flags = ISC_REQ_MUTUAL_AUTH | ISC_REQ_INTEGRITY | ISC_REQ_ALLOCATE_MEMORY;
        if delegate {
            flags |= ISC_REQ_DELEGATE;
        }
        let target: Vec<u16> = format!("host/{host}")
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        Ok(Box::new(Context {
            cred,
            ctx: None,
            target,
            flags,
        }))
    }
}

struct Context {
    cred: SecHandle,
    ctx: Option<SecHandle>,
    /// `host/<name>`, NUL-terminated.
    target: Vec<u16>,
    flags: ISC_REQ_FLAGS,
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

impl GssContext for Context {
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
        // The server must have proved it holds the service's key, and the
        // context must sign.
        if attrs & ISC_RET_MUTUAL_AUTH == 0 {
            return Err("the server did not authenticate itself".into());
        }
        if attrs & ISC_RET_INTEGRITY == 0 {
            return Err("no integrity protection".into());
        }
        Ok(ContextStep::Done(token))
    }

    fn mic(&mut self, data: &[u8]) -> Result<Vec<u8>, String> {
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
        let mut message = data.to_vec();
        let mut signature = vec![0u8; sizes.cbMaxSignature as usize];
        let mut bufs = [
            SecBuffer {
                cbBuffer: u32::try_from(message.len()).map_err(|_| "too much data")?,
                BufferType: SECBUFFER_DATA,
                pvBuffer: message.as_mut_ptr().cast(),
            },
            SecBuffer {
                cbBuffer: sizes.cbMaxSignature,
                BufferType: SECBUFFER_TOKEN,
                pvBuffer: signature.as_mut_ptr().cast(),
            },
        ];
        let desc = SecBufferDesc {
            ulVersion: SECBUFFER_VERSION,
            cBuffers: 2,
            pBuffers: bufs.as_mut_ptr(),
        };
        // SAFETY: both buffers outlive the call; SSPI writes at most
        // cbMaxSignature bytes and sets the token's length.
        unsafe { MakeSignature(&ctx, 0, &desc, 0) }.map_err(|e| e.message())?;
        let len = bufs.get(1).map_or(0, |b| b.cbBuffer as usize);
        signature.truncate(len);
        Ok(signature)
    }
}
