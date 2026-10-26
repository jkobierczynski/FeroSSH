//! The window's side of a ZMODEM transfer in a terminal pane: the files.
//!
//! The worker runs the protocol ([`ferossh_ipc::Message::ZmodemStart`] and
//! the rest); this end writes what the server's `sz` sends into the
//! download folder and reads what the server's `rz` asks for from the
//! files the user picked. What the worker says is checked, not trusted: a
//! name becomes a plain Windows file name in the download folder
//! (`names::local_name`), files arrive as `.part` and never replace an
//! existing one, and reads stay inside the picked files.
//!
//! Progress shows in the pane itself, as local text that is not logged.

use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, UNIX_EPOCH};

use ferossh_ipc::{ChannelId, Message, ZmodemAnswer, ZmodemUpload};
use ferossh_sftp::names;

use crate::ssh::{Effect, sanitize};

const DIM: &str = "\x1b[0;90m";
const AMBER: &str = "\x1b[0;33m";
const RESET: &str = "\x1b[0m";
/// Back to the start of the line and clear it, for progress that updates
/// in place.
const LINE: &str = "\r\x1b[K";

/// The most a single read hands the worker.
const MAX_READ: u32 = 64 * 1024;
/// How often the progress line updates.
const PROGRESS_EVERY: Duration = Duration::from_millis(250);
/// ZMODEM's positions are 32-bit.
const MAX_FILE: u64 = u32::MAX as u64;

/// Whether `msg` belongs to a ZMODEM transfer.
pub fn is_zmodem(msg: &Message) -> bool {
    matches!(
        msg,
        Message::ZmodemStart { .. }
            | Message::ZmodemFile { .. }
            | Message::ZmodemData { .. }
            | Message::ZmodemRead { .. }
            | Message::ZmodemFileDone { .. }
            | Message::ZmodemEnd { .. }
    )
}

/// The answer when ZMODEM is turned off or the pane is busy.
pub fn refuse(channel: ChannelId, off: bool) -> Effect {
    Effect::Send(Message::ZmodemAnswer {
        channel,
        answer: if off {
            ZmodemAnswer::Off
        } else {
            ZmodemAnswer::Decline
        },
    })
}

fn line(text: &str) -> Effect {
    Effect::Show(format!("{LINE}{DIM}{}{RESET}\r\n", sanitize(text)))
}

fn warn(text: &str) -> Effect {
    Effect::Show(format!("{LINE}{AMBER}{}{RESET}\r\n", sanitize(text)))
}

fn progress(name: &str, done: u64, size: Option<u64>) -> Effect {
    let text = match size {
        Some(size) if size > 0 => format!(
            "{name}: {} of {} ({}%)",
            names::size(done),
            names::size(size),
            done.saturating_mul(100) / size
        ),
        _ => format!("{name}: {}", names::size(done)),
    };
    Effect::Show(format!("{LINE}{DIM}{}{RESET}", sanitize(&text)))
}

/// A file coming in.
#[derive(Debug)]
struct Incoming {
    /// The name it gets.
    name: String,
    part: PathBuf,
    out: File,
    size: Option<u64>,
    got: u64,
}

/// A picked file going out.
#[derive(Debug)]
struct Outgoing {
    name: String,
    file: File,
    size: u64,
}

#[derive(Debug)]
enum Kind {
    Receive {
        folder: PathBuf,
        file: Option<Incoming>,
        saved: usize,
    },
    Send {
        files: Vec<Outgoing>,
        /// The file being sent.
        current: usize,
        /// Bytes of it the worker has read.
        done: u64,
        sent: usize,
    },
}

/// One transfer, from start to end.
#[derive(Debug)]
pub struct Transfer {
    channel: ChannelId,
    kind: Kind,
    shown: Instant,
}

impl Transfer {
    /// Takes what the server's `sz` sends, into `folder`.
    pub fn receive(channel: ChannelId, folder: PathBuf) -> (Self, Vec<Effect>) {
        let effects = vec![
            line(&format!(
                "ZMODEM: receiving into {}. Ctrl+C cancels.",
                folder.display()
            )),
            Effect::Send(Message::ZmodemAnswer {
                channel,
                answer: ZmodemAnswer::Receive,
            }),
        ];
        let t = Self {
            channel,
            kind: Kind::Receive {
                folder,
                file: None,
                saved: 0,
            },
            shown: Instant::now(),
        };
        (t, effects)
    }

    /// Sends `paths` to the server's `rz`; `None` (and a refusal) when
    /// none of them can be sent.
    pub fn send(channel: ChannelId, paths: &[PathBuf]) -> (Option<Self>, Vec<Effect>) {
        let mut effects = Vec::new();
        if paths.is_empty() {
            effects.push(line("ZMODEM: nothing picked, so nothing sent."));
        }
        let mut files = Vec::new();
        for path in paths {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let opened = File::open(path).and_then(|f| {
                let meta = f.metadata()?;
                Ok((f, meta))
            });
            match opened {
                Ok((_, meta)) if !meta.is_file() => {
                    effects.push(warn(&format!("ZMODEM: {name} is not a file; left out.")));
                }
                Ok((_, meta)) if meta.len() > MAX_FILE => effects.push(warn(&format!(
                    "ZMODEM: {name} is over 4 GB, more than ZMODEM can send; left out. Use \
                     SFTP for it."
                ))),
                Ok((file, meta)) => {
                    let mtime = meta
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                        .map_or(0, |d| d.as_secs());
                    files.push((
                        Outgoing {
                            name: name.clone(),
                            file,
                            size: meta.len(),
                        },
                        mtime,
                    ));
                }
                Err(e) => effects.push(warn(&format!("ZMODEM: cannot open {name}: {e}."))),
            }
        }
        if files.is_empty() {
            effects.push(refuse(channel, false));
            return (None, effects);
        }
        let uploads = files
            .iter()
            .map(|(f, mtime)| ZmodemUpload {
                name: f.name.clone().into_bytes(),
                size: f.size,
                mtime: *mtime,
            })
            .collect();
        let count = files.len();
        effects.push(line(&format!(
            "ZMODEM: sending {count} file{}. Ctrl+C cancels.",
            if count == 1 { "" } else { "s" }
        )));
        effects.push(Effect::Send(Message::ZmodemAnswer {
            channel,
            answer: ZmodemAnswer::Send(uploads),
        }));
        let t = Self {
            channel,
            kind: Kind::Send {
                files: files.into_iter().map(|(f, _)| f).collect(),
                current: 0,
                done: 0,
                sent: 0,
            },
            shown: Instant::now(),
        };
        (Some(t), effects)
    }

    /// Handles what the worker said; returns the effects and whether the
    /// transfer is over.
    pub fn on_worker(&mut self, msg: Message) -> (Vec<Effect>, bool) {
        let channel = self.channel;
        let mut effects = Vec::new();
        match msg {
            Message::ZmodemEnd { error, .. } => {
                effects.extend(self.finish(&error));
                return (effects, true);
            }
            Message::ZmodemFile { name, size, .. } => {
                if let Kind::Receive { folder, file, .. } = &mut self.kind {
                    if file.is_some() {
                        // One file at a time; a second offer is confused.
                        effects.push(Effect::Send(Message::ZmodemCancel { channel }));
                    } else {
                        let (accept, said) = match open_incoming(folder, &name, size) {
                            Ok(incoming) => {
                                let said = progress(&incoming.name, 0, size);
                                *file = Some(incoming);
                                (true, said)
                            }
                            Err(why) => (false, warn(&why)),
                        };
                        effects.push(said);
                        effects.push(Effect::Send(Message::ZmodemFileAnswer { channel, accept }));
                    }
                }
            }
            Message::ZmodemData { bytes, .. } => {
                if let Kind::Receive {
                    file: Some(incoming),
                    ..
                } = &mut self.kind
                {
                    match incoming.out.write_all(&bytes) {
                        Ok(()) => {
                            incoming.got += bytes.len() as u64;
                            if self.shown.elapsed() >= PROGRESS_EVERY {
                                self.shown = Instant::now();
                                effects.push(progress(&incoming.name, incoming.got, incoming.size));
                            }
                        }
                        Err(e) => {
                            effects.push(warn(&format!(
                                "ZMODEM: cannot write {}: {e}.",
                                incoming.name
                            )));
                            effects.push(Effect::Send(Message::ZmodemCancel { channel }));
                        }
                    }
                }
            }
            Message::ZmodemFileDone { skipped, .. } => match &mut self.kind {
                Kind::Receive {
                    folder,
                    file,
                    saved,
                } => {
                    if let Some(incoming) = file.take() {
                        match keep(folder, incoming) {
                            Ok(path) => {
                                *saved += 1;
                                effects.push(line(&format!("Saved {}", path.display())));
                            }
                            Err(why) => effects.push(warn(&why)),
                        }
                    }
                }
                Kind::Send {
                    files,
                    current,
                    done,
                    sent,
                } => {
                    if let Some(f) = files.get(*current) {
                        effects.push(if skipped {
                            warn(&format!(
                                "{}: the server did not take it (it may have the file already).",
                                f.name
                            ))
                        } else {
                            *sent += 1;
                            line(&format!("Sent {} ({})", f.name, names::size(f.size)))
                        });
                    }
                    *current += 1;
                    *done = 0;
                }
            },
            Message::ZmodemRead {
                file: index,
                offset,
                len,
                ..
            } => {
                if let Kind::Send {
                    files,
                    current,
                    done,
                    ..
                } = &mut self.kind
                {
                    // The worker reads the files in order.
                    let index = index as usize;
                    *current = index.max(*current);
                    let bytes = files
                        .get_mut(index)
                        .filter(|_| index == *current)
                        .map(|f| read_at(f, offset, len))
                        .unwrap_or_default();
                    if let Some(f) = files.get(index) {
                        *done = offset + bytes.len() as u64;
                        if self.shown.elapsed() >= PROGRESS_EVERY {
                            self.shown = Instant::now();
                            effects.push(progress(&f.name, *done, Some(f.size)));
                        }
                    }
                    effects.push(Effect::Send(Message::ZmodemChunk {
                        channel,
                        offset,
                        bytes,
                    }));
                }
            }
            _ => {}
        }
        (effects, false)
    }

    /// The closing line, and a partial file removed.
    fn finish(&mut self, error: &str) -> Vec<Effect> {
        let mut effects = Vec::new();
        if let Kind::Receive { file, .. } = &mut self.kind
            && let Some(incoming) = file.take()
        {
            let _ = std::fs::remove_file(&incoming.part);
        }
        let summary = match &self.kind {
            Kind::Receive { saved, .. } => format!(
                "{saved} file{} received",
                if *saved == 1 { "" } else { "s" }
            ),
            Kind::Send { sent, .. } => {
                format!("{sent} file{} sent", if *sent == 1 { "" } else { "s" })
            }
        };
        if error.is_empty() {
            effects.push(line(&format!("ZMODEM: done, {summary}.")));
        } else {
            effects.push(warn(&format!("ZMODEM stopped: {error} ({summary}.)")));
        }
        effects
    }
}

impl Drop for Transfer {
    fn drop(&mut self) {
        if let Kind::Receive {
            file: Some(incoming),
            ..
        } = &self.kind
        {
            let _ = std::fs::remove_file(&incoming.part);
        }
    }
}

/// Up to `len` bytes of `f` from `offset`; empty when it cannot be read.
fn read_at(f: &mut Outgoing, offset: u64, len: u32) -> Vec<u8> {
    if offset >= f.size {
        return Vec::new();
    }
    let len = u64::from(len.min(MAX_READ)).min(f.size - offset);
    let mut buf = vec![0u8; usize::try_from(len).unwrap_or(0)];
    let read = f.file.seek(SeekFrom::Start(offset)).and_then(|_| {
        let mut n = 0;
        while n < buf.len() {
            match f.file.read(buf.get_mut(n..).unwrap_or_default()) {
                Ok(0) => break,
                Ok(k) => n += k,
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(n)
    });
    match read {
        Ok(n) => {
            buf.truncate(n);
            buf
        }
        Err(_) => Vec::new(),
    }
}

/// A file name the server's name becomes: its last part, made safe.
fn local_name(remote: &[u8]) -> Option<String> {
    // Some senders use Windows separators.
    let last = names::file_name(remote);
    let last = last.rsplit(|&b| b == b'\\').next().unwrap_or(last);
    names::local_name(last)
}

/// The first `name`, `name (2)`, ... in `folder` that `taken` says is free.
fn free_name(name: &str, taken: impl Fn(&str) -> bool) -> Option<String> {
    (1..1000u32)
        .map(|n| {
            if n == 1 {
                name.to_owned()
            } else {
                names::numbered(name, n)
            }
        })
        .find(|candidate| !taken(candidate))
}

fn open_incoming(folder: &Path, remote: &[u8], size: Option<u64>) -> Result<Incoming, String> {
    let shown = names::display(remote);
    let Some(name) = local_name(remote) else {
        return Err(format!(
            "ZMODEM: skipped {shown}: not a name a file can have."
        ));
    };
    std::fs::create_dir_all(folder)
        .map_err(|e| format!("ZMODEM: cannot use {}: {e}.", folder.display()))?;
    for _ in 0..1000 {
        let Some(name) = free_name(&name, |n| {
            folder.join(n).exists() || folder.join(format!("{n}.part")).exists()
        }) else {
            break;
        };
        let part = folder.join(format!("{name}.part"));
        // create_new: never write through an existing file or link.
        match OpenOptions::new().write(true).create_new(true).open(&part) {
            Ok(out) => {
                return Ok(Incoming {
                    name,
                    part,
                    out,
                    size,
                    got: 0,
                });
            }
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
            Err(e) => return Err(format!("ZMODEM: cannot create {}: {e}.", part.display())),
        }
    }
    Err(format!(
        "ZMODEM: skipped {shown}: too many files with that name."
    ))
}

/// Renames a finished `.part` to its name, or the next free one if a file
/// of that name appeared meanwhile.
fn keep(folder: &Path, incoming: Incoming) -> Result<PathBuf, String> {
    let Incoming {
        name, part, out, ..
    } = incoming;
    let flushed = out.sync_all();
    drop(out);
    if let Err(e) = flushed {
        let _ = std::fs::remove_file(&part);
        return Err(format!("ZMODEM: cannot write {name}: {e}."));
    }
    let target = free_name(&name, |n| folder.join(n).exists())
        .map(|n| folder.join(n))
        .ok_or_else(|| {
            format!(
                "ZMODEM: no free name for {name}; it stays as {}.",
                part.display()
            )
        })?;
    std::fs::rename(&part, &target)
        .map(|()| target)
        .map_err(|e| format!("ZMODEM: cannot rename {}: {e}.", part.display()))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("ferossh-app-zm-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sent(effects: &[Effect]) -> Vec<&Message> {
        effects
            .iter()
            .filter_map(|e| match e {
                Effect::Send(m) => Some(m),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn receives_into_the_folder_without_replacing_anything() {
        let dir = temp("rx");
        std::fs::write(dir.join("a.txt"), b"keep me").unwrap();
        let (mut t, fx) = Transfer::receive(3, dir.clone());
        assert!(matches!(
            sent(&fx)[..],
            [Message::ZmodemAnswer {
                answer: ZmodemAnswer::Receive,
                ..
            }]
        ));
        // A path from the server: only its last part counts.
        let (fx, _) = t.on_worker(Message::ZmodemFile {
            channel: 3,
            name: b"../../etc/a.txt".to_vec(),
            size: Some(5),
        });
        assert!(matches!(
            sent(&fx)[..],
            [Message::ZmodemFileAnswer { accept: true, .. }]
        ));
        t.on_worker(Message::ZmodemData {
            channel: 3,
            bytes: b"hello".to_vec(),
        });
        assert!(dir.join("a (2).txt.part").exists());
        t.on_worker(Message::ZmodemFileDone {
            channel: 3,
            skipped: false,
        });
        assert_eq!(std::fs::read(dir.join("a.txt")).unwrap(), b"keep me");
        assert_eq!(std::fs::read(dir.join("a (2).txt")).unwrap(), b"hello");
        // Windows' device names and separators.
        let (fx, _) = t.on_worker(Message::ZmodemFile {
            channel: 3,
            name: b"C:\\temp\\con".to_vec(),
            size: None,
        });
        assert!(matches!(
            sent(&fx)[..],
            [Message::ZmodemFileAnswer { accept: true, .. }]
        ));
        let (_, over) = t.on_worker(Message::ZmodemEnd {
            channel: 3,
            error: "The sender cancelled the transfer.".into(),
        });
        assert!(over);
        // The unfinished file is gone.
        assert!(!dir.join("_con.part").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refuses_names_that_are_not_files() {
        let dir = temp("names");
        let (mut t, _) = Transfer::receive(3, dir.clone());
        for bad in [&b".."[..], b"", b".", b"../.."] {
            let (fx, _) = t.on_worker(Message::ZmodemFile {
                channel: 3,
                name: bad.to_vec(),
                size: None,
            });
            assert!(
                matches!(
                    sent(&fx)[..],
                    [Message::ZmodemFileAnswer { accept: false, .. }]
                ),
                "{bad:?}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sends_picked_files_and_reads_only_inside_them() {
        let dir = temp("tx");
        let a = dir.join("a.bin");
        std::fs::write(&a, (0..=255u8).collect::<Vec<_>>()).unwrap();
        let (t, fx) = Transfer::send(3, &[a.clone(), dir.join("missing")]);
        let mut t = t.unwrap();
        match sent(&fx)[..] {
            [
                Message::ZmodemAnswer {
                    answer: ZmodemAnswer::Send(files),
                    ..
                },
            ] => {
                assert_eq!(files.len(), 1);
                assert_eq!(files[0].name, b"a.bin");
                assert_eq!(files[0].size, 256);
            }
            ref other => panic!("{other:?}"),
        }
        let read = |t: &mut Transfer, file, offset, len| {
            let (fx, _) = t.on_worker(Message::ZmodemRead {
                channel: 3,
                file,
                offset,
                len,
            });
            match sent(&fx)[..] {
                [Message::ZmodemChunk { bytes, .. }] => bytes.clone(),
                ref other => panic!("{other:?}"),
            }
        };
        assert_eq!(read(&mut t, 0, 250, 100), (250..=255u8).collect::<Vec<_>>());
        assert_eq!(read(&mut t, 0, 256, 100), b"");
        assert_eq!(read(&mut t, 1, 0, 100), b"");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nothing_to_send_declines() {
        let (t, fx) = Transfer::send(3, &[PathBuf::from("/nonexistent/zz")]);
        assert!(t.is_none());
        assert!(matches!(
            sent(&fx)[..],
            [Message::ZmodemAnswer {
                answer: ZmodemAnswer::Decline,
                ..
            }]
        ));
    }
}
