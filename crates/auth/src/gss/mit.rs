//! Kerberos through MIT's GSS-API library (`libgssapi_krb5.so.2`), loaded
//! at run time: the tests' stand-in for Windows' SSPI, so a login through
//! the worker can be checked against a real KDC and OpenSSH's sshd on
//! Linux. Never part of FeroSSH itself (the `mit-gssapi` feature).
//!
//! The tickets come from the usual MIT environment: `KRB5_CONFIG`,
//! `KRB5CCNAME`, or a client keytab in `KRB5_CLIENT_KTNAME`.

#![allow(unsafe_code)]

use std::ffi::c_void;
use std::sync::Arc;

use libloading::Library;

use super::{ContextStep, GssContext, GssProvider, KRB5_OID};

#[repr(C)]
struct Buffer {
    length: usize,
    value: *mut c_void,
}

#[repr(C)]
struct Oid {
    length: u32,
    elements: *mut c_void,
}

type Name = *mut c_void;
type Ctx = *mut c_void;

type ImportName = unsafe extern "C" fn(*mut u32, *const Buffer, *const Oid, *mut Name) -> u32;
type ReleaseName = unsafe extern "C" fn(*mut u32, *mut Name) -> u32;
type InitSecContext = unsafe extern "C" fn(
    *mut u32,
    *mut c_void,
    *mut Ctx,
    Name,
    *const Oid,
    u32,
    u32,
    *mut c_void,
    *const Buffer,
    *mut *mut Oid,
    *mut Buffer,
    *mut u32,
    *mut u32,
) -> u32;
type GetMic = unsafe extern "C" fn(*mut u32, Ctx, u32, *const Buffer, *mut Buffer) -> u32;
type ReleaseBuffer = unsafe extern "C" fn(*mut u32, *mut Buffer) -> u32;
type DeleteSecContext = unsafe extern "C" fn(*mut u32, *mut Ctx, *mut Buffer) -> u32;

const DELEG: u32 = 1;
const MUTUAL: u32 = 2;
const INTEG: u32 = 32;
const CONTINUE_NEEDED: u32 = 1;

/// MIT Kerberos, loaded.
#[derive(Debug, Clone)]
pub struct Mit {
    lib: Arc<Library>,
}

impl Mit {
    pub fn load() -> Result<Self, String> {
        // SAFETY: loading the system's GSS-API library runs its
        // initialisers, which have no preconditions.
        let lib = unsafe { Library::new("libgssapi_krb5.so.2") }.map_err(|e| e.to_string())?;
        Ok(Self { lib: Arc::new(lib) })
    }

    fn sym<T: Copy>(&self, name: &[u8]) -> Result<T, String> {
        // SAFETY: each name is looked up with the type of its C prototype.
        unsafe { self.lib.get::<T>(name) }
            .map(|s| *s)
            .map_err(|e| e.to_string())
    }
}

fn krb5_oid() -> Oid {
    // The OID's contents without the DER tag and length.
    let body = KRB5_OID.get(2..).unwrap_or_default();
    Oid {
        length: body.len() as u32,
        elements: body.as_ptr() as *mut c_void,
    }
}

impl GssProvider for Mit {
    fn start(&self, host: &str, delegate: bool) -> Result<Box<dyn GssContext>, String> {
        let import: ImportName = self.sym(b"gss_import_name\0")?;
        // SAFETY: the library exports this OID as a `gss_OID` variable.
        let hostbased: *const *const Oid = unsafe {
            self.lib
                .get::<*const *const Oid>(b"GSS_C_NT_HOSTBASED_SERVICE\0")
                .map(|s| *s)
                .map_err(|e| e.to_string())?
        };
        let text = format!("host@{host}");
        let buf = Buffer {
            length: text.len(),
            value: text.as_ptr() as *mut c_void,
        };
        let (mut minor, mut name) = (0u32, std::ptr::null_mut());
        // SAFETY: valid buffer and out-parameters; the name is released
        // with the context.
        let major = unsafe { import(&mut minor, &buf, *hostbased, &mut name) };
        if major != 0 {
            return Err(format!("gss_import_name: {major:#x}/{minor}"));
        }
        let mut flags = MUTUAL | INTEG;
        if delegate {
            flags |= DELEG;
        }
        Ok(Box::new(MitContext {
            mit: self.clone(),
            name,
            ctx: std::ptr::null_mut(),
            flags,
        }))
    }
}

struct MitContext {
    mit: Mit,
    name: Name,
    ctx: Ctx,
    flags: u32,
}

// SAFETY: MIT's GSS-API handles may move between threads; the broker uses
// each from one thread at a time.
unsafe impl Send for MitContext {}

impl Drop for MitContext {
    fn drop(&mut self) {
        let mut minor = 0;
        if let Ok(delete) = self
            .mit
            .sym::<DeleteSecContext>(b"gss_delete_sec_context\0")
            && !self.ctx.is_null()
        {
            // SAFETY: a context this library made, deleted once.
            unsafe { delete(&mut minor, &mut self.ctx, std::ptr::null_mut()) };
        }
        if let Ok(release) = self.mit.sym::<ReleaseName>(b"gss_release_name\0") {
            // SAFETY: the name from gss_import_name, released once.
            unsafe { release(&mut minor, &mut self.name) };
        }
    }
}

impl MitContext {
    fn take(&self, buf: &mut Buffer) -> Result<Vec<u8>, String> {
        let release: ReleaseBuffer = self.mit.sym(b"gss_release_buffer\0")?;
        let out = if buf.value.is_null() {
            Vec::new()
        } else {
            // SAFETY: the library wrote `length` bytes there.
            unsafe { std::slice::from_raw_parts(buf.value as *const u8, buf.length) }.to_vec()
        };
        let mut minor = 0;
        // SAFETY: a buffer the library allocated.
        unsafe { release(&mut minor, buf) };
        Ok(out)
    }
}

impl GssContext for MitContext {
    fn step(&mut self, input: Option<&[u8]>) -> Result<ContextStep, String> {
        let init: InitSecContext = self.mit.sym(b"gss_init_sec_context\0")?;
        let input = input.unwrap_or_default();
        let in_buf = Buffer {
            length: input.len(),
            value: input.as_ptr() as *mut c_void,
        };
        let mut out = Buffer {
            length: 0,
            value: std::ptr::null_mut(),
        };
        let (mut minor, mut ret_flags) = (0u32, 0u32);
        let oid = krb5_oid();
        // SAFETY: valid buffers and out-parameters for this call.
        let major = unsafe {
            init(
                &mut minor,
                std::ptr::null_mut(),
                &mut self.ctx,
                self.name,
                &oid,
                self.flags,
                0,
                std::ptr::null_mut(),
                &in_buf,
                std::ptr::null_mut(),
                &mut out,
                &mut ret_flags,
                std::ptr::null_mut(),
            )
        };
        let token = self.take(&mut out)?;
        match major {
            0 if ret_flags & MUTUAL == 0 => Err("no mutual authentication".into()),
            0 if ret_flags & INTEG == 0 => Err("no integrity protection".into()),
            0 => Ok(ContextStep::Done(token)),
            CONTINUE_NEEDED => Ok(ContextStep::Continue(token)),
            _ => Err(format!("gss_init_sec_context: {major:#x}/{minor}")),
        }
    }

    fn mic(&mut self, data: &[u8]) -> Result<Vec<u8>, String> {
        let get_mic: GetMic = self.mit.sym(b"gss_get_mic\0")?;
        let msg = Buffer {
            length: data.len(),
            value: data.as_ptr() as *mut c_void,
        };
        let mut out = Buffer {
            length: 0,
            value: std::ptr::null_mut(),
        };
        let mut minor = 0;
        // SAFETY: an established context and valid buffers.
        let major = unsafe { get_mic(&mut minor, self.ctx, 0, &msg, &mut out) };
        let mic = self.take(&mut out)?;
        if major != 0 {
            return Err(format!("gss_get_mic: {major:#x}/{minor}"));
        }
        Ok(mic)
    }
}
