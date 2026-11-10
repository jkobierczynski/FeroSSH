//! Runs a password manager's command for a login password: no shell, no
//! input, no window, a time limit, and the output wiped after use. Only
//! the first line of what it prints is kept; nothing of it is logged.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use zeroize::Zeroizing;

/// Long enough to unlock a vault with a fingerprint or a master password
/// in the manager's own window.
pub const TIMEOUT: Duration = Duration::from_secs(60);
/// More than any password; output past it is dropped.
const MAX_OUTPUT: u64 = 64 * 1024;

/// Runs `program` with `args` and returns the first line it printed.
pub fn run(program: &str, args: &[String], timeout: Duration) -> Result<Zeroizing<String>, String> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: no console flashes up for a console tool.
        command.creation_flags(0x0800_0000);
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("{program} did not start ({e}). Is it installed and on PATH?"))?;
    // Read both pipes on their own threads so a chatty tool cannot block.
    let out = child.stdout.take().map(|o| {
        std::thread::spawn(move || {
            let mut buf = Zeroizing::new(Vec::new());
            let _ = o.take(MAX_OUTPUT).read_to_end(&mut buf);
            buf
        })
    });
    let err = child.stderr.take().map(|e| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = e.take(MAX_OUTPUT).read_to_end(&mut buf);
            buf
        })
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "{program} gave no password within {} seconds.",
                    timeout.as_secs()
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return Err(format!("{program} could not be watched ({e}).")),
        }
    };
    let output = out.and_then(|t| t.join().ok()).unwrap_or_default();
    let errors = err.and_then(|t| t.join().ok()).unwrap_or_default();
    if !status.success() {
        let why = String::from_utf8_lossy(&errors);
        let why: String = why
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or_default()
            .chars()
            .filter(|c| !c.is_control())
            .take(200)
            .collect();
        let code = status.code().map_or_else(
            || "was stopped".to_owned(),
            |c| format!("ended with code {c}"),
        );
        return Err(if why.is_empty() {
            format!("{program} {code}.")
        } else {
            format!("{program} {code}: {why}")
        });
    }
    match ferossh_config::password::first_line(&output) {
        Some(line) => Ok(Zeroizing::new(line)),
        None => Err(format!("{program} printed no password.")),
    }
}

#[cfg(all(test, unix))]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn sh(script: &str) -> Vec<String> {
        vec!["-c".into(), script.into()]
    }

    #[test]
    fn keeps_the_first_line() {
        let pw = run("sh", &sh("printf 's3cret\\nnext\\n'"), TIMEOUT).unwrap();
        assert_eq!(pw.as_str(), "s3cret");
    }

    #[test]
    fn says_why_it_failed() {
        let e = run("sh", &sh("echo 'vault is locked' >&2; exit 3"), TIMEOUT).unwrap_err();
        assert_eq!(e, "sh ended with code 3: vault is locked");
        let e = run("sh", &sh("true"), TIMEOUT).unwrap_err();
        assert!(e.contains("printed no password"), "{e}");
        let e = run("no-such-program-ferossh", &[], TIMEOUT).unwrap_err();
        assert!(e.contains("did not start"), "{e}");
    }

    #[test]
    fn gives_up_after_the_time_limit_and_reads_nothing_typed() {
        let at = Instant::now();
        let e = run("sh", &sh("sleep 5"), Duration::from_millis(300)).unwrap_err();
        assert!(e.contains("within"), "{e}");
        assert!(at.elapsed() < Duration::from_secs(3));
        // No input: a tool that asks gets end of file at once.
        let e = run("sh", &sh("read x || exit 4"), TIMEOUT).unwrap_err();
        assert!(e.contains("code 4"), "{e}");
    }
}
