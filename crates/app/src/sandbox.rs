//! Starts the SSH worker: this same exe with `--worker`, inside an
//! AppContainer that may only use the network.
//!
//! The AppContainer ("FeroSSH.Worker") gets two capabilities:
//! `internetClient` and `privateNetworkClientServer` (without the second,
//! hosts on the LAN are unreachable). It has no access to the user's files,
//! registry or other processes. The worker talks to the UI over two
//! anonymous pipes it inherits, passed by handle value on its command line;
//! only those two handles are inherited. A job object kills the worker
//! when the UI exits, stops it from starting processes of its own, caps
//! its memory, and keeps it away from the clipboard, the desktop, global
//! atoms and other processes' windows.
//!
//! Both sandboxed processes also start with process mitigations (see
//! [`MITIGATIONS`]): no dynamic code, no legacy extension points (AppInit
//! DLLs, hooks, Winsock LSPs), no non-system fonts, mandatory ASLR, strict
//! handle checks, DLLs from System32 before the exe's folder, and no child
//! processes. A Windows that refuses the set starts them without it.
//! `--no-sandbox` runs the worker without mitigations too, so it stays a
//! full escape hatch.
//!
//! AppContainers cannot connect to loopback addresses, so `localhost`
//! (and WSL through localhost) is unreachable from the sandbox; such hosts
//! need `--no-sandbox` for now.
//!
//! With a desktop viewer (`vnc-viewer`, `rdp-viewer`), the same exe runs as the
//! remote desktop viewer (`--viewer`), in its own AppContainer
//! ("FeroSSH.Viewer") with no capabilities at all: no network, and, like
//! the worker, none of the user's files or registry. It is always
//! sandboxed; `--no-sandbox` only concerns the worker.

// AppContainer, pipe, job and process calls are Win32 FFI; each unsafe block
// is a direct call with handles and buffers this module owns.
#![allow(unsafe_code)]

use std::ffi::c_void;
use std::fs::File;
use std::os::windows::io::FromRawHandle;

use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, ERROR_SUCCESS, HANDLE, HANDLE_FLAG_INHERIT, HANDLE_FLAGS,
    HLOCAL, LocalFree, SetHandleInformation,
};
use windows::Win32::Security::Authorization::{
    EXPLICIT_ACCESS_W, GRANT_ACCESS, GetNamedSecurityInfoW, SE_FILE_OBJECT, SetEntriesInAclW,
    SetNamedSecurityInfoW, TRUSTEE_IS_SID, TRUSTEE_IS_WELL_KNOWN_GROUP, TRUSTEE_W,
};
use windows::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeriveAppContainerSidFromAppContainerName,
};
use windows::Win32::Security::{
    ACL, CreateWellKnownSid, DACL_SECURITY_INFORMATION, FreeSid, NO_INHERITANCE,
    PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, SECURITY_CAPABILITIES, SID_AND_ATTRIBUTES,
    WELL_KNOWN_SID_TYPE, WinCapabilityInternetClientSid,
    WinCapabilityPrivateNetworkClientServerSid,
};
use windows::Win32::Storage::FileSystem::{FILE_GENERIC_EXECUTE, FILE_GENERIC_READ};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
    JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOB_OBJECT_LIMIT_PROCESS_MEMORY, JOB_OBJECT_UILIMIT_DESKTOP,
    JOB_OBJECT_UILIMIT_DISPLAYSETTINGS, JOB_OBJECT_UILIMIT_EXITWINDOWS,
    JOB_OBJECT_UILIMIT_GLOBALATOMS, JOB_OBJECT_UILIMIT_HANDLES, JOB_OBJECT_UILIMIT_READCLIPBOARD,
    JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS, JOB_OBJECT_UILIMIT_WRITECLIPBOARD,
    JOBOBJECT_BASIC_UI_RESTRICTIONS, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectBasicUIRestrictions, JobObjectExtendedLimitInformation, SetInformationJobObject,
};
use windows::Win32::System::Pipes::CreatePipe;
use windows::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_SUSPENDED, CreateProcessW, DeleteProcThreadAttributeList,
    EXTENDED_STARTUPINFO_PRESENT, InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
    PROC_THREAD_ATTRIBUTE_CHILD_PROCESS_POLICY, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    PROC_THREAD_ATTRIBUTE_MITIGATION_POLICY, PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES,
    PROCESS_INFORMATION, ResumeThread, STARTUPINFOEXW, TerminateProcess, UpdateProcThreadAttribute,
};
use windows::core::{HSTRING, PWSTR, Result, w};

/// Which sandboxed process to start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Worker,
    #[cfg(feature = "viewer")]
    Viewer,
}

impl Kind {
    /// The AppContainer profile name (Windows derives its SID from this),
    /// its display name and description.
    fn profile(
        self,
    ) -> (
        windows::core::PCWSTR,
        windows::core::PCWSTR,
        windows::core::PCWSTR,
    ) {
        match self {
            Kind::Worker => (
                w!("FeroSSH.Worker"),
                w!("FeroSSH SSH worker"),
                w!("Runs FeroSSH's network connections with network access only."),
            ),
            #[cfg(feature = "viewer")]
            Kind::Viewer => (
                w!("FeroSSH.Viewer"),
                w!("FeroSSH desktop viewer"),
                w!(
                    "Decodes remote desktop pictures for FeroSSH, with no network, file or registry access."
                ),
            ),
        }
    }

    fn flag(self) -> &'static str {
        match self {
            Kind::Worker => "--worker",
            #[cfg(feature = "viewer")]
            Kind::Viewer => "--viewer",
        }
    }
}
/// The process mitigations both sandboxed processes start with, as
/// `PROCESS_CREATION_MITIGATION_POLICY_*_ALWAYS_ON` bits (winbase.h; the
/// windows crate does not carry them). Left out on purpose: Win32k
/// lockdown (the exe imports user32), Control Flow Guard (the exe is not
/// built with it), Microsoft-signed images only (FeroSSH is not), and no
/// remote images (FeroSSH may run from a network share).
const MITIGATIONS: u64 = (1 << 8) // force relocate images (mandatory ASLR)
    | (1 << 12) // terminate on heap corruption
    | (1 << 16) // bottom-up ASLR
    | (1 << 20) // high-entropy ASLR
    | (1 << 24) // strict handle checks
    | (1 << 32) // no legacy extension points
    | (1 << 36) // no dynamic code
    | (1 << 48) // no non-system fonts
    | (1 << 56) // no low-integrity images
    | (1 << 60); // DLLs from System32 first

/// PROCESS_CREATION_CHILD_PROCESS_RESTRICTED: no child processes, on top
/// of the job's one-process limit.
const CHILD_PROCESS_RESTRICTED: u32 = 1;

/// The most memory one sandboxed process may commit. Frames are at most
/// 1 MiB (worker) or 16 MiB (viewer) and an 8K screen is 130 MiB, so this
/// only stops a runaway or taken-over process from starving the PC.
const MEMORY_LIMIT: usize = 1 << 30;

/// SE_GROUP_ENABLED: the capability is in force.
const SE_GROUP_ENABLED: u32 = 4;
/// Largest SID, in bytes (SECURITY_MAX_SID_SIZE).
const MAX_SID: usize = 68;

/// A kernel handle closed on drop.
struct Owned(HANDLE);

// SAFETY: a handle is a process-wide value; the owner alone uses and closes it.
unsafe impl Send for Owned {}

impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: we own the handle and close it once.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// The running worker. Dropping it closes the job, which ends the worker.
pub struct Worker {
    pub to_worker: File,
    pub from_worker: File,
    _job: Owned,
    _process: Owned,
    /// Whether the worker runs in the AppContainer.
    pub sandboxed: bool,
    /// Whether it started with [`MITIGATIONS`].
    pub mitigated: bool,
}

impl std::fmt::Debug for Worker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Worker")
            .field("sandboxed", &self.sandboxed)
            .field("mitigated", &self.mitigated)
            .finish_non_exhaustive()
    }
}

/// The AppContainer SID, created on first use.
struct ContainerSid(PSID);

impl Drop for ContainerSid {
    fn drop(&mut self) {
        // SAFETY: a SID the AppContainer APIs allocated, freed once.
        unsafe {
            FreeSid(self.0);
        }
    }
}

fn container_sid(kind: Kind) -> Result<ContainerSid> {
    let (profile, name, description) = kind.profile();
    // SAFETY: constant strings; the profile is created once per user and
    // looked up afterwards.
    unsafe {
        match CreateAppContainerProfile(profile, name, description, None) {
            Ok(sid) => Ok(ContainerSid(sid)),
            Err(e) if e.code() == ERROR_ALREADY_EXISTS.to_hresult() => {
                DeriveAppContainerSidFromAppContainerName(profile).map(ContainerSid)
            }
            Err(e) => Err(e),
        }
    }
}

fn capability(kind: WELL_KNOWN_SID_TYPE, buf: &mut [u8; MAX_SID]) -> Result<PSID> {
    let sid = PSID(buf.as_mut_ptr().cast());
    let mut size = MAX_SID as u32;
    // SAFETY: `buf` is SECURITY_MAX_SID_SIZE bytes, as the call requires.
    unsafe { CreateWellKnownSid(kind, None, Some(sid), &mut size)? };
    Ok(sid)
}

/// Lets the AppContainer read and run the exe, which it needs to start;
/// a copy in Downloads is otherwise unreadable to it. Failure (for
/// example an exe under Program Files, already readable) is not an error.
fn grant_exe_access(exe: &HSTRING, sid: PSID) {
    // SAFETY: the descriptor and ACLs the calls allocate are freed below;
    // `sid` outlives every call.
    unsafe {
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let mut sd = PSECURITY_DESCRIPTOR::default();
        let got = GetNamedSecurityInfoW(
            exe,
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(&mut dacl),
            None,
            &mut sd,
        );
        if got != ERROR_SUCCESS {
            return;
        }
        let access = EXPLICIT_ACCESS_W {
            grfAccessPermissions: (FILE_GENERIC_READ | FILE_GENERIC_EXECUTE).0,
            grfAccessMode: GRANT_ACCESS,
            grfInheritance: NO_INHERITANCE,
            Trustee: TRUSTEE_W {
                TrusteeForm: TRUSTEE_IS_SID,
                TrusteeType: TRUSTEE_IS_WELL_KNOWN_GROUP,
                ptstrName: PWSTR(sid.0.cast()),
                ..Default::default()
            },
        };
        let mut new_dacl: *mut ACL = std::ptr::null_mut();
        if SetEntriesInAclW(Some(&[access]), Some(dacl), &mut new_dacl) == ERROR_SUCCESS {
            let _ = SetNamedSecurityInfoW(
                exe,
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                None,
                None,
                Some(new_dacl),
                None,
            );
            let _ = LocalFree(Some(HLOCAL(new_dacl.cast())));
        }
        let _ = LocalFree(Some(HLOCAL(sd.0)));
    }
}

/// A pipe whose `child` end is inheritable and whose `parent` end is not.
fn pipe(child_reads: bool) -> Result<(Owned, Owned)> {
    let sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: true.into(),
    };
    let (mut read, mut write) = (HANDLE::default(), HANDLE::default());
    // SAFETY: out-pointers to locals; both handles are wrapped right away.
    unsafe { CreatePipe(&mut read, &mut write, Some(&sa), 0)? };
    let (read, write) = (Owned(read), Owned(write));
    let (child, parent) = if child_reads {
        (read, write)
    } else {
        (write, read)
    };
    // SAFETY: our handle.
    unsafe { SetHandleInformation(parent.0, HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0))? };
    Ok((child, parent))
}

fn kill_on_close_job() -> Result<Owned> {
    // SAFETY: a new unnamed job, wrapped right away; the limit structs are
    // valid locals of the sizes passed.
    unsafe {
        let job = Owned(CreateJobObjectW(None, None)?);
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
            | JOB_OBJECT_LIMIT_ACTIVE_PROCESS
            | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION
            | JOB_OBJECT_LIMIT_PROCESS_MEMORY;
        limits.BasicLimitInformation.ActiveProcessLimit = 1;
        limits.ProcessMemoryLimit = MEMORY_LIMIT;
        SetInformationJobObject(
            job.0,
            JobObjectExtendedLimitInformation,
            (&raw const limits).cast::<c_void>(),
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )?;
        // Neither process has a window: no clipboard, no desktop switching,
        // no global atoms, no handles to other processes' windows, no
        // system settings and no logging off.
        let ui = JOBOBJECT_BASIC_UI_RESTRICTIONS {
            UIRestrictionsClass: JOB_OBJECT_UILIMIT_DESKTOP
                | JOB_OBJECT_UILIMIT_DISPLAYSETTINGS
                | JOB_OBJECT_UILIMIT_EXITWINDOWS
                | JOB_OBJECT_UILIMIT_GLOBALATOMS
                | JOB_OBJECT_UILIMIT_HANDLES
                | JOB_OBJECT_UILIMIT_READCLIPBOARD
                | JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS
                | JOB_OBJECT_UILIMIT_WRITECLIPBOARD,
        };
        SetInformationJobObject(
            job.0,
            JobObjectBasicUIRestrictions,
            (&raw const ui).cast::<c_void>(),
            std::mem::size_of::<JOBOBJECT_BASIC_UI_RESTRICTIONS>() as u32,
        )?;
        Ok(job)
    }
}

/// Starts the worker; `sandbox` false runs it as a plain child process
/// (the `--no-sandbox` escape hatch).
pub fn spawn_worker(sandbox: bool) -> Result<Worker> {
    spawn(Kind::Worker, sandbox)
}

/// Starts a remote desktop viewer, always in its AppContainer.
#[cfg(feature = "viewer")]
pub fn spawn_viewer() -> Result<Worker> {
    spawn(Kind::Viewer, true)
}

fn spawn(kind: Kind, sandbox: bool) -> Result<Worker> {
    let exe = std::env::current_exe()
        .map_err(|e| windows::core::Error::new(windows::core::HRESULT(-1), e.to_string()))?;
    let exe_w = HSTRING::from(exe.as_os_str());
    let sid = if sandbox {
        Some(container_sid(kind)?)
    } else {
        None
    };
    if let Some(sid) = &sid {
        grant_exe_access(&exe_w, sid.0);
    }

    // The worker reads what the UI writes, and the other way round.
    let (child_in, to_worker) = pipe(true)?;
    let (child_out, from_worker) = pipe(false)?;
    let inherit = [child_in.0, child_out.0];

    let mut caps_buf = [[0u8; MAX_SID]; 2];
    let [inet_buf, lan_buf] = &mut caps_buf;
    let mut caps = [
        SID_AND_ATTRIBUTES {
            Sid: capability(WinCapabilityInternetClientSid, inet_buf)?,
            Attributes: SE_GROUP_ENABLED,
        },
        SID_AND_ATTRIBUTES {
            Sid: capability(WinCapabilityPrivateNetworkClientServerSid, lan_buf)?,
            Attributes: SE_GROUP_ENABLED,
        },
    ];
    // The viewer gets no capabilities at all.
    let caps: &mut [SID_AND_ATTRIBUTES] = if kind == Kind::Worker {
        &mut caps
    } else {
        &mut []
    };
    let security = sid.as_ref().map(|sid| SECURITY_CAPABILITIES {
        AppContainerSid: sid.0,
        Capabilities: if caps.is_empty() {
            std::ptr::null_mut()
        } else {
            caps.as_mut_ptr()
        },
        CapabilityCount: caps.len() as u32,
        Reserved: 0,
    });

    // Mitigations come with the sandbox; a Windows that does not know one
    // of them refuses the process, which then starts without them.
    let started = if sid.is_some() {
        create(kind, &exe, &exe_w, security.as_ref(), &inherit, true)
            .map(|(job, process)| (job, process, true))
            .or_else(|_| {
                create(kind, &exe, &exe_w, security.as_ref(), &inherit, false)
                    .map(|(job, process)| (job, process, false))
            })
    } else {
        create(kind, &exe, &exe_w, None, &inherit, false)
            .map(|(job, process)| (job, process, false))
    };
    let (job, process, mitigated) = started?;
    // The child has its copies; ours would keep the pipes open forever.
    drop((child_in, child_out));

    let into_file = |h: Owned| {
        let raw = h.0.0;
        std::mem::forget(h);
        // SAFETY: we own the handle and hand it to the File, which closes it.
        unsafe { File::from_raw_handle(raw) }
    };
    Ok(Worker {
        to_worker: into_file(to_worker),
        from_worker: into_file(from_worker),
        _job: job,
        _process: process,
        sandboxed: sid.is_some(),
        mitigated,
    })
}

/// Starts the process suspended, puts it in a fresh job and lets it run.
fn create(
    kind: Kind,
    exe: &std::path::Path,
    exe_w: &HSTRING,
    security: Option<&SECURITY_CAPABILITIES>,
    inherit: &[HANDLE; 2],
    mitigate: bool,
) -> Result<(Owned, Owned)> {
    let mitigations = MITIGATIONS;
    let child_policy = CHILD_PROCESS_RESTRICTED;
    let attr_count = 1 + u32::from(security.is_some()) + if mitigate { 2 } else { 0 };
    let mut size = 0usize;
    // SAFETY: the first call only reports the size and is expected to fail.
    let _ = unsafe { InitializeProcThreadAttributeList(None, attr_count, None, &mut size) };
    let mut list_buf = vec![0u8; size];
    let list = LPPROC_THREAD_ATTRIBUTE_LIST(list_buf.as_mut_ptr().cast());
    // SAFETY: `list_buf` has the requested size and outlives the list.
    unsafe { InitializeProcThreadAttributeList(Some(list), attr_count, None, &mut size)? };

    let job = kill_on_close_job();
    let result = job.and_then(|job| {
        // SAFETY: each attribute value points at a local or argument that
        // lives until CreateProcessW returns; the list is initialised.
        unsafe {
            UpdateProcThreadAttribute(
                list,
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                Some(inherit.as_ptr().cast()),
                std::mem::size_of_val(inherit),
                None,
                None,
            )?;
            if let Some(security) = security {
                UpdateProcThreadAttribute(
                    list,
                    0,
                    PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize,
                    Some((security as *const SECURITY_CAPABILITIES).cast()),
                    std::mem::size_of::<SECURITY_CAPABILITIES>(),
                    None,
                    None,
                )?;
            }
            if mitigate {
                UpdateProcThreadAttribute(
                    list,
                    0,
                    PROC_THREAD_ATTRIBUTE_MITIGATION_POLICY as usize,
                    Some((&raw const mitigations).cast()),
                    std::mem::size_of::<u64>(),
                    None,
                    None,
                )?;
                UpdateProcThreadAttribute(
                    list,
                    0,
                    PROC_THREAD_ATTRIBUTE_CHILD_PROCESS_POLICY as usize,
                    Some((&raw const child_policy).cast()),
                    std::mem::size_of::<u32>(),
                    None,
                    None,
                )?;
            }
        }
        let mut si = STARTUPINFOEXW::default();
        si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        si.lpAttributeList = list;
        let cmdline = format!(
            "\"{}\" {} {} {}",
            exe.display(),
            kind.flag(),
            inherit[0].0 as usize,
            inherit[1].0 as usize
        );
        let mut cmdline: Vec<u16> = cmdline.encode_utf16().chain([0]).collect();
        let mut pi = PROCESS_INFORMATION::default();
        // SAFETY: a valid, NUL-terminated mutable command line; the startup
        // info and attribute list live until the call returns. Only the
        // two listed handles are inherited.
        unsafe {
            CreateProcessW(
                exe_w,
                Some(PWSTR(cmdline.as_mut_ptr())),
                None,
                None,
                true,
                EXTENDED_STARTUPINFO_PRESENT | CREATE_SUSPENDED | CREATE_NO_WINDOW,
                None,
                None,
                &si.StartupInfo,
                &mut pi,
            )?;
        }
        let process = Owned(pi.hProcess);
        let thread = Owned(pi.hThread);
        // SAFETY: our job and the suspended process we just created.
        unsafe {
            if let Err(e) = AssignProcessToJobObject(job.0, process.0) {
                let _ = TerminateProcess(process.0, 1);
                return Err(e);
            }
            ResumeThread(thread.0);
        }
        Ok((job, process))
    });
    // SAFETY: the list initialised above, deleted once.
    unsafe { DeleteProcThreadAttributeList(list) };
    result
}

/// The worker side: turns the handle values on the command line back into
/// the pipes. Only called in the `--worker` process.
pub fn worker_pipes(input: &str, output: &str) -> Option<(File, File)> {
    let input: usize = input.parse().ok()?;
    let output: usize = output.parse().ok()?;
    if input == 0 || output == 0 || input == output {
        return None;
    }
    // SAFETY: the UI passed these inherited handles for exactly this use;
    // each becomes the sole owner of its handle. A wrong value fails on the
    // first read or write and the worker exits.
    unsafe {
        Some((
            File::from_raw_handle(input as *mut c_void),
            File::from_raw_handle(output as *mut c_void),
        ))
    }
}
