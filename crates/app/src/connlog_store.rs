//! Where the connection log is kept ([`ferossh_config::connlog`]): in
//! FeroSSH's folder for this PC, sealed while a vault is open (see
//! [`crate::local_store`]), and, when machine policy asks for it, copied
//! entry by entry to the Windows event log ([`crate::eventlog`]).

use ferossh_config::connlog::{Entry, Log};

/// The log's file in FeroSSH's folder for this PC.
pub const FILE: &str = "connections.log";

/// The log as kept, and whether a sealed part could not be read (no
/// vault open, or another vault's).
pub fn read() -> std::io::Result<(Log, bool)> {
    let r = crate::local_store::read(FILE)?;
    Ok((Log::parse(&r.text), r.locked))
}

/// Changes the log under its lock: read again, changed, written back.
fn change<T>(f: impl FnOnce(&mut Log) -> T) -> std::io::Result<T> {
    let Some(dir) = crate::portable::local_dir() else {
        return Ok(f(&mut Log::default()));
    };
    let _held = crate::inventory_view::lock(&dir)?;
    let was = crate::local_store::read(FILE)?;
    let mut log = Log::parse(&was.text);
    let out = f(&mut log);
    crate::local_store::write(FILE, &was, &log.to_text())?;
    Ok(out)
}

/// Adds `entry` to the log (and to the Windows event log when the
/// policy says so; that copy is made even when the file cannot be
/// written).
pub fn add(entry: &Entry) -> std::io::Result<()> {
    if crate::machine_policy::log().event_log {
        crate::eventlog::report(entry);
    }
    change(|log| log.push(entry))
}

/// Removes `gone` from the log and says how many went. Refused when
/// machine policy turns deleting off.
pub fn remove(gone: &[Entry]) -> std::io::Result<usize> {
    let policy = crate::machine_policy::log();
    if !policy.may_delete() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "your organisation's policy keeps every connection log entry",
        ));
    }
    let n = change(|log| log.remove(gone))?;
    if policy.event_log && n > 0 {
        crate::eventlog::report_deleted(n);
    }
    Ok(n)
}
