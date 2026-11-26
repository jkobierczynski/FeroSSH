//! A program on this PC that talks over plain pipes, not a console: tmux
//! in control mode in WSL ([`crate::wsl`]). ConPTY would redraw what such a
//! program writes as a screen, which breaks a protocol, so it gets its own
//! stdin and stdout, and stderr joins stdout.
//!
//! As with [`crate::conpty`], threads read and write so the UI never
//! blocks: output reaches it through a bounded channel, and `notify` wakes
//! it when something arrives.

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, Sender, SyncSender, channel, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread;

/// Chunks buffered between the readers and the UI: 4 MB at most, so a
/// flood slows the program down to what the UI takes.
const BACKLOG_CHUNKS: usize = 64;
const READ_CHUNK: usize = 64 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum Output {
    Data(Vec<u8>),
    /// The program exited (with this code, when it had one) and all it
    /// wrote was read.
    Exited(Option<i32>),
}

/// A running program on pipes. Dropping it closes its input and ends it.
pub struct PipeChild {
    input: Option<Sender<Vec<u8>>>,
    output: Option<Receiver<Output>>,
    child: Arc<Mutex<Child>>,
}

impl std::fmt::Debug for PipeChild {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PipeChild").finish_non_exhaustive()
    }
}

impl PipeChild {
    /// Starts `program` (a full path: see [`crate::program`]) with `args`,
    /// each passed as one argument.
    pub fn spawn(
        program: &Path,
        args: &[String],
        notify: impl Fn() + Send + Clone + 'static,
    ) -> std::io::Result<Self> {
        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // CREATE_NO_WINDOW: no console flashes up.
            command.creation_flags(0x0800_0000);
        }
        let mut child = command.spawn()?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let child = Arc::new(Mutex::new(child));

        let (out_tx, output) = sync_channel(BACKLOG_CHUNKS);
        // The exit is told once both readers are done, so it comes after
        // the last output.
        let (done_tx, done_rx) = channel::<()>();
        let mut readers = 0;
        if let Some(out) = stdout {
            readers += 1;
            spawn_reader(out, out_tx.clone(), done_tx.clone(), notify.clone());
        }
        if let Some(err) = stderr {
            readers += 1;
            spawn_reader(err, out_tx.clone(), done_tx.clone(), notify.clone());
        }
        drop(done_tx);
        {
            let child = child.clone();
            thread::spawn(move || {
                for _ in 0..readers {
                    if done_rx.recv().is_err() {
                        break;
                    }
                }
                // Polled, not waited on: the lock stays free for a kill.
                let code = loop {
                    let status = match child.lock() {
                        Ok(mut c) => c.try_wait(),
                        Err(_) => break None,
                    };
                    match status {
                        Ok(Some(s)) => break s.code(),
                        Ok(None) => thread::sleep(std::time::Duration::from_millis(50)),
                        Err(_) => break None,
                    }
                };
                if out_tx.send(Output::Exited(code)).is_ok() {
                    notify();
                }
            });
        }
        let (in_tx, in_rx) = channel::<Vec<u8>>();
        if let Some(mut stdin) = stdin {
            thread::spawn(move || {
                while let Ok(bytes) = in_rx.recv() {
                    if stdin
                        .write_all(&bytes)
                        .and_then(|()| stdin.flush())
                        .is_err()
                    {
                        return;
                    }
                }
                // Dropping stdin closes it: the program reads its end.
            });
        }
        Ok(Self {
            input: Some(in_tx),
            output: Some(output),
            child,
        })
    }

    /// Queues bytes for the program's input.
    pub fn write(&self, bytes: Vec<u8>) {
        if let Some(tx) = &self.input {
            let _ = tx.send(bytes);
        }
    }

    /// The next piece of output, if any is waiting.
    pub fn try_read(&self) -> Option<Output> {
        self.output.as_ref()?.try_recv().ok()
    }

    /// Waits up to `timeout` for the next piece of output.
    #[cfg_attr(not(all(test, unix)), allow(dead_code))]
    pub fn read_timeout(&self, timeout: std::time::Duration) -> Option<Output> {
        self.output.as_ref()?.recv_timeout(timeout).ok()
    }
}

impl Drop for PipeChild {
    fn drop(&mut self) {
        // Closing its input lets tmux detach by itself; the kill makes
        // sure nothing is left behind (on Windows it ends wsl.exe, and the
        // tmux client in Linux with it; the tmux server stays).
        self.input = None;
        self.output = None;
        if let Ok(mut c) = self.child.lock() {
            let _ = c.kill();
        }
    }
}

/// Runs `program` with `args` to the end, `input` on its standard input:
/// its standard output and standard error. Blocks: call it off the UI
/// thread.
pub fn run_once(
    program: &Path,
    args: &[String],
    input: &[u8],
) -> std::io::Result<(Vec<u8>, Vec<u8>)> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let mut child = command.spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        // A script that reads less than it gets closes its end: fine.
        let _ = stdin.write_all(input);
    }
    let out = child.wait_with_output()?;
    Ok((out.stdout, out.stderr))
}

fn spawn_reader(
    mut pipe: impl Read + Send + 'static,
    tx: SyncSender<Output>,
    done: Sender<()>,
    notify: impl Fn() + Send + 'static,
) {
    thread::spawn(move || {
        let mut buf = vec![0u8; READ_CHUNK];
        loop {
            match pipe.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let data = buf.get(..n).unwrap_or_default().to_vec();
                    if tx.send(Output::Data(data)).is_err() {
                        break;
                    }
                    notify();
                }
            }
        }
        let _ = done.send(());
    });
}

#[cfg(all(test, unix))]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn read_all(c: &PipeChild) -> (Vec<u8>, Option<i32>) {
        let mut out = Vec::new();
        loop {
            match c.read_timeout(Duration::from_secs(10)) {
                Some(Output::Data(d)) => out.extend(d),
                Some(Output::Exited(code)) => return (out, code),
                None => panic!("no exit; got {:?}", String::from_utf8_lossy(&out)),
            }
        }
    }

    #[test]
    fn echoes_input_and_tells_the_exit_after_the_output() {
        let c = PipeChild::spawn(
            Path::new("/bin/sh"),
            &[
                "-c".into(),
                "read l; echo \"got $l\"; echo oops >&2; exit 3".into(),
            ],
            || {},
        )
        .unwrap();
        c.write(b"hello\n".to_vec());
        let (out, code) = read_all(&c);
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("got hello"), "{text}");
        assert!(text.contains("oops"), "{text}");
        assert_eq!(code, Some(3));
    }

    #[test]
    fn arguments_stay_whole() {
        let c = PipeChild::spawn(
            Path::new("/bin/sh"),
            &[
                "-c".into(),
                "printf '%s|' \"$@\"".into(),
                "sh".into(),
                "a b".into(),
                "it's \"q\"".into(),
            ],
            || {},
        )
        .unwrap();
        let (out, _) = read_all(&c);
        assert_eq!(out, b"a b|it's \"q\"|");
    }

    #[test]
    fn runs_once_with_input() {
        let (out, err) = run_once(
            Path::new("/bin/sh"),
            &["-c".into(), "read a; echo \"[$a]\"; echo e >&2".into()],
            b"x y\n",
        )
        .unwrap();
        assert_eq!(out, b"[x y]\n");
        assert_eq!(err, b"e\n");
    }

    #[test]
    fn dropping_ends_the_program() {
        let c = PipeChild::spawn(
            Path::new("/bin/sh"),
            &["-c".into(), "sleep 30".into()],
            || {},
        )
        .unwrap();
        let child = c.child.clone();
        drop(c);
        let status = child.lock().unwrap().wait().unwrap();
        assert!(!status.success());
    }
}
