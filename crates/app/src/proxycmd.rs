//! Runs a host's `ProxyCommand`: a local program whose standard input and
//! output carry the SSH connection, such as `ncat --proxy ...` or a
//! corporate tunnel tool. It runs without a shell and without a window,
//! with the user's own rights (outside the sandbox, as it must reach what
//! the sandbox cannot), and only once the user approved its line.
//!
//! Its output goes to the worker as stream data, within the stream's
//! credit; what the worker sends goes to its input, and credit goes back
//! once it is written (see the `ipc` crate). It never sees anything but
//! the connection's encrypted bytes. What it prints on its standard error
//! is kept, so the pane can show why it failed.

use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use ferossh_ipc::{ChannelId, MAX_DATA_CHUNK, Message, STREAM_WINDOW};

/// The end of what the command printed on its standard error that is
/// kept.
const MAX_ERRORS: usize = 4096;

/// Where the worker's messages on the stream wait for the command, which
/// may start later (once the user allowed it): the sink to relay them to,
/// and the queue [`ProxyRun::spawn`] reads.
pub fn input() -> (impl Fn(Message) + Send + Sync + 'static, Receiver<Message>) {
    let (tx, rx) = channel::<Message>();
    let tx = Mutex::new(tx);
    let sink = move |msg| {
        if let Ok(tx) = tx.lock() {
            let _ = tx.send(msg);
        }
    };
    (sink, rx)
}

/// A running `ProxyCommand`; dropping it stops the program.
pub struct ProxyRun {
    child: Child,
    errors: Arc<Mutex<Vec<u8>>>,
    credit: Arc<(Mutex<Option<u32>>, Condvar)>,
}

impl std::fmt::Debug for ProxyRun {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProxyRun")
            .field("pid", &self.child.id())
            .finish_non_exhaustive()
    }
}

impl ProxyRun {
    /// Starts `program` with `args`; its output goes to `to_worker` as
    /// stream `tunnel`, and its input comes from `input` (see [`input`]).
    pub fn spawn(
        program: &str,
        args: &[String],
        tunnel: ChannelId,
        input: Receiver<Message>,
        to_worker: impl Fn(Message) + Send + Sync + 'static,
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
            // CREATE_NO_WINDOW: no console flashes up for a console tool.
            command.creation_flags(0x0800_0000);
        }
        let mut child = command.spawn()?;
        let to_worker = Arc::new(to_worker);
        // Credit for sending to the worker; `None` once the stream ended.
        let credit = Arc::new((Mutex::new(Some(STREAM_WINDOW)), Condvar::new()));
        if let Some(mut out) = child.stdout.take() {
            let (to_worker, credit) = (to_worker.clone(), credit.clone());
            thread::Builder::new()
                .name("proxy command output".into())
                .spawn(move || {
                    let mut buf = vec![0u8; MAX_DATA_CHUNK];
                    loop {
                        let Some(room) = wait_for_credit(&credit) else {
                            return;
                        };
                        let want = buf.len().min(room as usize);
                        let n = match out.read(buf.get_mut(..want).unwrap_or_default()) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => n,
                        };
                        take_credit(&credit, n);
                        to_worker(Message::Data {
                            channel: tunnel,
                            bytes: buf.get(..n).unwrap_or_default().to_vec(),
                        });
                    }
                    to_worker(Message::Eof { channel: tunnel });
                })?;
        }
        let errors = Arc::new(Mutex::new(Vec::new()));
        if let Some(mut err) = child.stderr.take() {
            let errors = errors.clone();
            thread::Builder::new()
                .name("proxy command errors".into())
                .spawn(move || {
                    let mut buf = [0u8; 1024];
                    while let Ok(n) = err.read(&mut buf) {
                        if n == 0 {
                            break;
                        }
                        if let Ok(mut e) = errors.lock() {
                            e.extend_from_slice(buf.get(..n).unwrap_or_default());
                            let extra = e.len().saturating_sub(MAX_ERRORS);
                            e.drain(..extra);
                        }
                    }
                })?;
        }
        let input_rx = input;
        let mut stdin = child.stdin.take();
        {
            let credit = credit.clone();
            thread::Builder::new()
                .name("proxy command input".into())
                .spawn(move || {
                    while let Ok(msg) = input_rx.recv() {
                        match msg {
                            Message::Data { bytes, .. } => {
                                let written = stdin.as_mut().is_some_and(|s| {
                                    s.write_all(&bytes).and_then(|()| s.flush()).is_ok()
                                });
                                if !written {
                                    break;
                                }
                                let n = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
                                to_worker(Message::Window {
                                    channel: tunnel,
                                    bytes: n,
                                });
                            }
                            Message::Window { bytes, .. } => give_credit(&credit, bytes),
                            // Closing its input tells the command there is
                            // no more.
                            Message::Eof { .. } => stdin = None,
                            Message::Close { .. } => break,
                            _ => {}
                        }
                    }
                    end_credit(&credit);
                })?;
        }
        Ok(Self {
            child,
            errors,
            credit,
        })
    }

    /// The last lines the command printed on its standard error, made safe
    /// to show.
    pub fn errors(&self) -> String {
        let bytes = self.errors.lock().map(|e| e.clone()).unwrap_or_default();
        let text = String::from_utf8_lossy(&bytes);
        let lines: Vec<&str> = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        let tail = lines
            .get(lines.len().saturating_sub(5)..)
            .unwrap_or_default();
        crate::ssh::sanitize(&tail.join("\n"))
    }
}

impl Drop for ProxyRun {
    fn drop(&mut self) {
        end_credit(&self.credit);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn wait_for_credit(credit: &(Mutex<Option<u32>>, Condvar)) -> Option<u32> {
    let (lock, cv) = credit;
    let mut c = lock.lock().ok()?;
    loop {
        match *c {
            None => return None,
            Some(n) if n > 0 => return Some(n),
            Some(_) => c = cv.wait(c).ok()?,
        }
    }
}

fn take_credit(credit: &(Mutex<Option<u32>>, Condvar), n: usize) {
    if let Ok(mut c) = credit.0.lock()
        && let Some(left) = c.as_mut()
    {
        *left = left.saturating_sub(u32::try_from(n).unwrap_or(u32::MAX));
    }
}

fn give_credit(credit: &(Mutex<Option<u32>>, Condvar), n: u32) {
    if let Ok(mut c) = credit.0.lock()
        && let Some(left) = c.as_mut()
    {
        *left = left.saturating_add(n).min(STREAM_WINDOW);
        credit.1.notify_all();
    }
}

fn end_credit(credit: &(Mutex<Option<u32>>, Condvar)) {
    if let Ok(mut c) = credit.0.lock() {
        *c = None;
        credit.1.notify_all();
    }
}

#[cfg(all(test, unix))]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// Collects what the command sends the worker.
    fn worker() -> (
        Arc<Mutex<Vec<Message>>>,
        impl Fn(Message) + Send + Sync + 'static,
    ) {
        let got = Arc::new(Mutex::new(Vec::new()));
        let sink = got.clone();
        (got, move |m| sink.lock().unwrap().push(m))
    }

    fn wait_until(mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done() {
            assert!(Instant::now() < deadline, "timed out");
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn relays_both_ways_with_credit() {
        let (got, to_worker) = worker();
        let (send, input) = input();
        // What comes before the command starts waits for it.
        send(Message::Data {
            channel: 9,
            bytes: b"SSH-2.0-test\r\n".to_vec(),
        });
        let _run = ProxyRun::spawn("cat", &[], 9, input, to_worker).unwrap();
        wait_until(|| {
            let got = got.lock().unwrap();
            let echoed: Vec<u8> = got
                .iter()
                .filter_map(|m| match m {
                    Message::Data { bytes, .. } => Some(bytes.clone()),
                    _ => None,
                })
                .flatten()
                .collect();
            echoed == b"SSH-2.0-test\r\n"
                && got
                    .iter()
                    .any(|m| matches!(m, Message::Window { bytes: 14, .. }))
        });
        send(Message::Eof { channel: 9 });
        wait_until(|| got.lock().unwrap().contains(&Message::Eof { channel: 9 }));
    }

    #[test]
    fn stops_at_the_window_until_credit_comes_back() {
        let (got, to_worker) = worker();
        let n = STREAM_WINDOW as usize + 100_000;
        let (send, input) = input();
        let _run = ProxyRun::spawn(
            "head",
            &["-c".into(), n.to_string(), "/dev/zero".into()],
            9,
            input,
            to_worker,
        )
        .unwrap();
        let sent = |got: &Arc<Mutex<Vec<Message>>>| -> usize {
            got.lock()
                .unwrap()
                .iter()
                .map(|m| match m {
                    Message::Data { bytes, .. } => bytes.len(),
                    _ => 0,
                })
                .sum()
        };
        wait_until(|| sent(&got) == STREAM_WINDOW as usize);
        thread::sleep(Duration::from_millis(100));
        assert_eq!(sent(&got), STREAM_WINDOW as usize);
        send(Message::Window {
            channel: 9,
            bytes: STREAM_WINDOW,
        });
        wait_until(|| sent(&got) == n);
    }

    #[test]
    fn keeps_what_it_printed_on_error() {
        let (got, to_worker) = worker();
        let run = ProxyRun::spawn(
            "sh",
            &[
                "-c".into(),
                "echo 'proxy: no route to host' >&2; exit 1".into(),
            ],
            9,
            input().1,
            to_worker,
        )
        .unwrap();
        // It ended: its output closed.
        wait_until(|| got.lock().unwrap().contains(&Message::Eof { channel: 9 }));
        wait_until(|| !run.errors().is_empty());
        assert_eq!(run.errors(), "proxy: no route to host");
        assert!(ProxyRun::spawn("/nonexistent/proxy", &[], 9, input().1, |_| {}).is_err());
    }
}
