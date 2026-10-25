//! The transfer queue against a fake SFTP server that serves a real
//! folder: downloads, uploads, resuming, conflicts, folders and links,
//! verification, and a connection that drops halfway.
//!
//! The fake answers with the local file system's semantics, so it stands
//! in for a POSIX server only on Unix (CI runs these on Linux).

#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ferossh_ipc::{DirEntry, FileAttrs, Message, OpenMode, SftpOp, SftpReply, sftp_code};
use ferossh_sftp::{Choice, Kind, Remote, State};
use sha2::{Digest, Sha256};

/// A scratch folder with `remote/` (what the server serves) and `local/`.
struct Scratch {
    dir: PathBuf,
}

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("ferossh-queue-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("remote")).unwrap();
        fs::create_dir_all(dir.join("local")).unwrap();
        Self { dir }
    }

    fn remote(&self) -> PathBuf {
        self.dir.join("remote")
    }

    fn local(&self) -> PathBuf {
        self.dir.join("local")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// Serves `root` as `/`, like sftp-server in a chroot.
struct Fake {
    root: PathBuf,
    handles: HashMap<u32, File>,
    dirs: HashMap<u32, ()>,
    next: u32,
    /// Offsets of the reads asked for, in order.
    reads: Vec<u64>,
    /// Requests answered before the connection drops.
    drop_after: Option<usize>,
    answered: usize,
    /// Return this instead of the real checksum.
    fake_sum: Option<String>,
    /// Answer reads with half of what was asked.
    short_reads: bool,
}

fn attrs_of(meta: &fs::Metadata) -> FileAttrs {
    use std::os::unix::fs::MetadataExt;
    FileAttrs {
        size: Some(meta.len()),
        permissions: Some(meta.mode()),
        mtime: None,
    }
}

fn failed(code: u32) -> SftpReply {
    SftpReply::Failed {
        code,
        message: String::new(),
    }
}

impl Fake {
    fn new(root: PathBuf) -> Self {
        Self {
            root,
            handles: HashMap::new(),
            dirs: HashMap::new(),
            next: 1,
            reads: Vec::new(),
            drop_after: None,
            answered: 0,
            fake_sum: None,
            short_reads: false,
        }
    }

    fn path(&self, remote: &[u8]) -> PathBuf {
        let text = String::from_utf8_lossy(remote);
        self.root.join(text.trim_start_matches('/'))
    }

    fn io(e: &std::io::Error) -> SftpReply {
        match e.kind() {
            std::io::ErrorKind::NotFound => failed(sftp_code::NO_SUCH_FILE),
            std::io::ErrorKind::PermissionDenied => failed(sftp_code::PERMISSION_DENIED),
            _ => failed(sftp_code::FAILURE),
        }
    }

    fn serve(&mut self, op: SftpOp) -> Vec<SftpReply> {
        let one = |r| vec![r];
        match op {
            SftpOp::RealPath { path } => {
                let p = self.path(&path);
                match fs::canonicalize(&p) {
                    Ok(real) => {
                        let root = fs::canonicalize(&self.root).unwrap();
                        let shown = match real.strip_prefix(&root) {
                            Ok(rel) => format!("/{}", rel.display()),
                            // Outside the served folder: the true path.
                            Err(_) => format!("/../outside{}", real.display()),
                        };
                        let shown = if shown == "/" {
                            "/".to_owned()
                        } else {
                            shown.trim_end_matches('/').to_owned()
                        };
                        one(SftpReply::Path(shown.into_bytes()))
                    }
                    Err(e) => one(Self::io(&e)),
                }
            }
            SftpOp::Stat { path, follow } => {
                let p = self.path(&path);
                let meta = if follow {
                    fs::metadata(&p)
                } else {
                    fs::symlink_metadata(&p)
                };
                match meta {
                    Ok(m) => one(SftpReply::Attrs(attrs_of(&m))),
                    Err(e) => one(Self::io(&e)),
                }
            }
            SftpOp::List { path } => {
                let p = self.path(&path);
                match fs::read_dir(&p) {
                    Ok(rd) => {
                        let mut entries: Vec<DirEntry> = Vec::new();
                        for e in rd {
                            let e = e.unwrap();
                            let m = fs::symlink_metadata(e.path()).unwrap();
                            entries.push(DirEntry {
                                name: e.file_name().into_encoded_bytes(),
                                attrs: attrs_of(&m),
                            });
                        }
                        // Two chunks, as a server sends them.
                        let half = entries.len() / 2;
                        let rest = entries.split_off(half);
                        vec![
                            SftpReply::Entries {
                                entries,
                                last: false,
                            },
                            SftpReply::Entries {
                                entries: rest,
                                last: true,
                            },
                        ]
                    }
                    Err(e) => one(Self::io(&e)),
                }
            }
            SftpOp::Open { path, mode } => {
                let p = self.path(&path);
                let f = match mode {
                    OpenMode::Read => File::open(&p),
                    OpenMode::Write { truncate } => OpenOptions::new()
                        .write(true)
                        .create(true)
                        .truncate(truncate)
                        .open(&p),
                };
                match f {
                    Ok(f) => {
                        let h = self.next;
                        self.next += 1;
                        self.handles.insert(h, f);
                        one(SftpReply::Opened { handle: h })
                    }
                    Err(e) => one(Self::io(&e)),
                }
            }
            SftpOp::Read {
                handle,
                offset,
                len,
            } => {
                self.reads.push(offset);
                let short = self.short_reads;
                let Some(f) = self.handles.get_mut(&handle) else {
                    return one(failed(sftp_code::FAILURE));
                };
                f.seek(SeekFrom::Start(offset)).unwrap();
                let want = if short { len.div_ceil(2) } else { len };
                let mut buf = vec![0; want as usize];
                let n = f.read(&mut buf).unwrap();
                if n == 0 {
                    return one(SftpReply::Eof);
                }
                buf.truncate(n);
                one(SftpReply::Data(buf))
            }
            SftpOp::Write {
                handle,
                offset,
                data,
            } => {
                let Some(f) = self.handles.get_mut(&handle) else {
                    return one(failed(sftp_code::FAILURE));
                };
                f.seek(SeekFrom::Start(offset)).unwrap();
                f.write_all(&data).unwrap();
                one(SftpReply::Done)
            }
            SftpOp::Close { handle } => {
                self.handles.remove(&handle);
                self.dirs.remove(&handle);
                one(SftpReply::Done)
            }
            SftpOp::Mkdir { path } => match fs::create_dir(self.path(&path)) {
                Ok(()) => one(SftpReply::Done),
                // OpenSSH says "failure" for an existing folder.
                Err(_) => one(failed(sftp_code::FAILURE)),
            },
            SftpOp::Remove { path } => match fs::remove_file(self.path(&path)) {
                Ok(()) => one(SftpReply::Done),
                Err(e) => one(Self::io(&e)),
            },
            SftpOp::Rmdir { path } => match fs::remove_dir(self.path(&path)) {
                Ok(()) => one(SftpReply::Done),
                Err(e) => one(Self::io(&e)),
            },
            SftpOp::Rename {
                from,
                to,
                overwrite,
            } => {
                let (from, to) = (self.path(&from), self.path(&to));
                if !overwrite && to.exists() {
                    return one(failed(sftp_code::FAILURE));
                }
                match fs::rename(from, to) {
                    Ok(()) => one(SftpReply::Done),
                    Err(e) => one(Self::io(&e)),
                }
            }
            SftpOp::Checksum { path } => {
                if let Some(sum) = &self.fake_sum {
                    return one(SftpReply::Checksum(sum.clone()));
                }
                match fs::read(self.path(&path)) {
                    Ok(bytes) => one(SftpReply::Checksum(hex(&bytes))),
                    Err(e) => one(Self::io(&e)),
                }
            }
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

const CHANNEL: u32 = 9;

/// Plays the worker until the queue settles. Returns early (`false`) when
/// the fake drops the connection.
fn run(remote: &mut Remote, fake: &mut Fake) -> bool {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        assert!(Instant::now() < deadline, "the queue never settled");
        let out = remote.take_outbox();
        if out.is_empty() {
            if !remote.jobs().iter().any(|j| j.state == State::Running) {
                return true;
            }
            // A hash is being computed on a thread.
            std::thread::sleep(Duration::from_millis(5));
            remote.poll();
            continue;
        }
        for msg in out {
            match msg {
                Message::SftpOpen { channel, .. } => remote.on_message(Message::SftpReady {
                    channel,
                    home: b"/".to_vec(),
                }),
                Message::SftpRequest { channel, id, op } => {
                    if fake.drop_after.is_some_and(|n| fake.answered >= n) {
                        fake.drop_after = None;
                        fake.handles.clear();
                        remote.on_message(Message::Disconnected {
                            channel,
                            reason: "gone".into(),
                        });
                        return false;
                    }
                    fake.answered += 1;
                    for reply in fake.serve(op) {
                        remote.on_message(Message::SftpReply { channel, id, reply });
                    }
                }
                Message::Close { .. } => {}
                other => panic!("unexpected {other:?}"),
            }
        }
    }
}

fn connected(fake_root: &Path) -> (Remote, Fake) {
    let mut remote = Remote::new("me@host");
    remote.attach(CHANNEL, 1);
    let mut fake = Fake::new(fake_root.to_path_buf());
    run(&mut remote, &mut fake);
    (remote, fake)
}

fn entry(remote: &Remote, name: &str) -> DirEntry {
    remote
        .listing
        .entries
        .iter()
        .find(|e| e.name == name.as_bytes())
        .unwrap_or_else(|| panic!("{name} not listed"))
        .clone()
}

fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 7 % 251) as u8).collect()
}

#[test]
fn lists_the_home_folder_folders_first() {
    let s = Scratch::new("list");
    fs::write(s.remote().join("b.txt"), "b").unwrap();
    fs::write(s.remote().join("A.txt"), "a").unwrap();
    fs::create_dir(s.remote().join("zdir")).unwrap();
    let (remote, _) = connected(&s.remote());
    let names: Vec<&[u8]> = remote.listing.entries.iter().map(|e| &e.name[..]).collect();
    assert_eq!(names, [&b"zdir"[..], b"A.txt", b"b.txt"]);
    assert_eq!(remote.listing.path, b"/");
    assert!(!remote.listing.loading);
}

#[test]
fn downloads_a_large_file_in_order() {
    let s = Scratch::new("down");
    let data = pattern(3 * 1024 * 1024 + 123);
    fs::write(s.remote().join("big.bin"), &data).unwrap();
    let (mut remote, mut fake) = connected(&s.remote());
    let e = entry(&remote, "big.bin");
    remote.download(&e, &s.local()).unwrap();
    assert!(run(&mut remote, &mut fake));
    let job = &remote.jobs()[0];
    assert_eq!(job.state, State::Done, "{:?}", job.note);
    assert_eq!(job.done, data.len() as u64);
    assert_eq!(fs::read(s.local().join("big.bin")).unwrap(), data);
    assert!(!s.local().join("big.bin.part").exists());
}

#[test]
fn short_reads_are_asked_again() {
    let s = Scratch::new("short");
    let data = pattern(200_000);
    fs::write(s.remote().join("f"), &data).unwrap();
    let (mut remote, mut fake) = connected(&s.remote());
    fake.short_reads = true;
    let e = entry(&remote, "f");
    remote.download(&e, &s.local()).unwrap();
    assert!(run(&mut remote, &mut fake));
    assert_eq!(remote.jobs()[0].state, State::Done);
    assert_eq!(fs::read(s.local().join("f")).unwrap(), data);
}

#[test]
fn resumes_a_partial_download() {
    let s = Scratch::new("resume");
    let data = pattern(500_000);
    fs::write(s.remote().join("f.iso"), &data).unwrap();
    fs::write(s.local().join("f.iso.part"), &data[..100_000]).unwrap();
    let (mut remote, mut fake) = connected(&s.remote());
    let e = entry(&remote, "f.iso");
    remote.download(&e, &s.local()).unwrap();
    assert!(run(&mut remote, &mut fake));
    assert_eq!(fake.reads.first(), Some(&100_000));
    assert_eq!(fs::read(s.local().join("f.iso")).unwrap(), data);
}

#[test]
fn a_dropped_connection_waits_and_carries_on() {
    let s = Scratch::new("drop");
    let data = pattern(2_000_000);
    fs::write(s.remote().join("f"), &data).unwrap();
    fs::write(s.remote().join("up.src"), b"").unwrap();
    let (mut remote, mut fake) = connected(&s.remote());
    let e = entry(&remote, "f");
    remote.download(&e, &s.local()).unwrap();
    let up = s.local().join("up.bin");
    fs::write(&up, &data).unwrap();
    remote.upload(&up, b"/").unwrap();
    fake.drop_after = Some(fake.answered + 40);
    assert!(!run(&mut remote, &mut fake));
    assert!(remote.jobs().iter().all(|j| j.state == State::Waiting));
    assert!(remote.wants_connection());
    // Reconnected: the transfers resume from their partial files.
    let part = fs::metadata(s.local().join("f.part")).unwrap().len();
    assert!(part > 0 && part < data.len() as u64, "{part}");
    remote.attach(CHANNEL + 1, 2);
    fake.reads.clear();
    assert!(run(&mut remote, &mut fake));
    assert!(remote.jobs().iter().all(|j| j.state == State::Done));
    assert_eq!(fake.reads.first(), Some(&part));
    assert_eq!(fs::read(s.local().join("f")).unwrap(), data);
    assert_eq!(fs::read(s.remote().join("up.bin")).unwrap(), data);
    assert!(!s.remote().join("up.bin.filepart").exists());
}

#[test]
fn asks_before_overwriting_and_can_keep_both() {
    let s = Scratch::new("conflict");
    fs::write(s.remote().join("a.txt"), "new").unwrap();
    fs::write(s.local().join("a.txt"), "old").unwrap();
    let (mut remote, mut fake) = connected(&s.remote());
    let e = entry(&remote, "a.txt");
    let id = remote.download(&e, &s.local()).unwrap();
    assert!(run(&mut remote, &mut fake));
    assert_eq!(remote.jobs()[0].state, State::Asking);
    let (asked, text) = remote.question().unwrap();
    assert_eq!(asked, id);
    assert!(text.contains("a.txt already exists"), "{text}");
    remote.answer(id, Choice::KeepBoth, false);
    assert!(run(&mut remote, &mut fake));
    assert_eq!(fs::read_to_string(s.local().join("a.txt")).unwrap(), "old");
    assert_eq!(
        fs::read_to_string(s.local().join("a (2).txt")).unwrap(),
        "new"
    );
}

#[test]
fn upload_conflicts_overwrite_skip_and_number() {
    let s = Scratch::new("upconflict");
    fs::write(s.remote().join("n.txt"), "remote").unwrap();
    let local = s.local().join("n.txt");
    fs::write(&local, "local").unwrap();
    let (mut remote, mut fake) = connected(&s.remote());
    let id = remote.upload(&local, b"/").unwrap();
    assert!(run(&mut remote, &mut fake));
    assert_eq!(remote.jobs()[0].state, State::Asking);
    remote.answer(id, Choice::Skip, false);
    assert!(run(&mut remote, &mut fake));
    assert_eq!(remote.jobs()[0].state, State::Skipped);
    assert_eq!(
        fs::read_to_string(s.remote().join("n.txt")).unwrap(),
        "remote"
    );

    let id = remote.upload(&local, b"/").unwrap();
    assert!(run(&mut remote, &mut fake));
    remote.answer(id, Choice::KeepBoth, true);
    assert!(run(&mut remote, &mut fake));
    assert_eq!(
        fs::read_to_string(s.remote().join("n (2).txt")).unwrap(),
        "local"
    );
    // "All" sticks: the next one numbers without asking.
    remote.upload(&local, b"/").unwrap();
    assert!(run(&mut remote, &mut fake));
    assert_eq!(
        fs::read_to_string(s.remote().join("n (3).txt")).unwrap(),
        "local"
    );

    remote.clear_finished();
    assert!(remote.jobs().is_empty());
    let id = remote.upload(&local, b"/").unwrap();
    assert!(run(&mut remote, &mut fake));
    remote.answer(id, Choice::Overwrite, false);
    assert!(run(&mut remote, &mut fake));
    assert_eq!(
        fs::read_to_string(s.remote().join("n.txt")).unwrap(),
        "local"
    );
}

#[test]
fn resumes_a_partial_upload() {
    let s = Scratch::new("upresume");
    let data = pattern(300_000);
    let local = s.local().join("u.bin");
    fs::write(&local, &data).unwrap();
    fs::write(s.remote().join("u.bin.filepart"), &data[..65_536]).unwrap();
    let (mut remote, mut fake) = connected(&s.remote());
    remote.upload(&local, b"/").unwrap();
    assert!(run(&mut remote, &mut fake));
    let job = &remote.jobs()[0];
    assert_eq!(job.state, State::Done, "{:?}", job.note);
    assert_eq!(fs::read(s.remote().join("u.bin")).unwrap(), data);
}

#[test]
fn verifies_with_sha256_and_removes_a_bad_copy() {
    let s = Scratch::new("verify");
    let data = pattern(100_000);
    fs::write(s.remote().join("v"), &data).unwrap();
    let (mut remote, mut fake) = connected(&s.remote());
    remote.verify = true;
    let e = entry(&remote, "v");
    remote.download(&e, &s.local()).unwrap();
    assert!(run(&mut remote, &mut fake));
    let job = &remote.jobs()[0];
    assert_eq!(job.state, State::Done);
    assert_eq!(job.note.as_deref(), Some("SHA-256 verified"));

    fake.fake_sum = Some("00".repeat(32));
    fs::remove_file(s.local().join("v")).unwrap();
    remote.download(&e, &s.local()).unwrap();
    assert!(run(&mut remote, &mut fake));
    let job = &remote.jobs()[1];
    assert_eq!(job.state, State::Failed);
    assert!(job.note.as_deref().unwrap_or_default().contains("mismatch"));
    assert!(!s.local().join("v").exists());
    assert!(!s.local().join("v.part").exists());

    // Uploads verify too.
    fake.fake_sum = None;
    let up = s.local().join("w");
    fs::write(&up, &data).unwrap();
    remote.upload(&up, b"/").unwrap();
    assert!(run(&mut remote, &mut fake));
    assert_eq!(remote.jobs()[2].note.as_deref(), Some("SHA-256 verified"));
}

#[test]
fn folder_download_sanitises_names_and_keeps_links_inside() {
    use std::os::unix::fs::symlink;
    let s = Scratch::new("fetch");
    let tree = s.remote().join("proj");
    fs::create_dir_all(tree.join("src/deep")).unwrap();
    fs::write(tree.join("src/main.rs"), "fn main() {}").unwrap();
    fs::write(tree.join("src/deep/x"), "x").unwrap();
    fs::write(tree.join("CON"), "device?").unwrap();
    fs::write(tree.join("a:b"), "colon").unwrap();
    // A link to a file inside is followed; one leading out is not, and
    // neither is a link to a folder.
    symlink("src/main.rs", tree.join("inside-link")).unwrap();
    symlink("/etc/hostname", tree.join("outside-link")).unwrap();
    symlink("../..", tree.join("up-link")).unwrap();
    symlink("src", tree.join("dir-link")).unwrap();
    let (mut remote, mut fake) = connected(&s.remote());
    let e = entry(&remote, "proj");
    remote.download(&e, &s.local()).unwrap();
    assert!(run(&mut remote, &mut fake));
    let folder = &remote.jobs()[0];
    assert_eq!(folder.kind, Kind::FetchFolder);
    assert_eq!(folder.state, State::Done, "{:?}", folder.note);
    let note = folder.note.clone().unwrap_or_default();
    assert!(note.starts_with("5 files"), "{note}");
    assert!(note.contains("3 links skipped"), "{note}");
    let got = s.local().join("proj");
    assert_eq!(
        fs::read_to_string(got.join("src/main.rs")).unwrap(),
        "fn main() {}"
    );
    assert_eq!(fs::read_to_string(got.join("src/deep/x")).unwrap(), "x");
    assert_eq!(fs::read_to_string(got.join("_CON")).unwrap(), "device?");
    assert_eq!(fs::read_to_string(got.join("a_b")).unwrap(), "colon");
    assert_eq!(
        fs::read_to_string(got.join("inside-link")).unwrap(),
        "fn main() {}"
    );
    assert!(!got.join("outside-link").exists());
    assert!(!got.join("up-link").exists());
    assert!(!got.join("dir-link").exists());
}

#[test]
fn folder_upload_makes_folders_first() {
    let s = Scratch::new("send");
    let tree = s.local().join("site");
    fs::create_dir_all(tree.join("css/fonts")).unwrap();
    fs::write(tree.join("index.html"), "<p>").unwrap();
    fs::write(tree.join("css/a.css"), "a{}").unwrap();
    fs::write(tree.join("css/fonts/f.woff"), "font").unwrap();
    // The folder exists already on the server: fine.
    fs::create_dir(s.remote().join("site")).unwrap();
    let (mut remote, mut fake) = connected(&s.remote());
    remote.upload(&tree, b"/").unwrap();
    assert!(run(&mut remote, &mut fake));
    let folder = &remote.jobs()[0];
    assert_eq!(folder.state, State::Done, "{:?}", folder.note);
    assert_eq!(folder.done, 3 + 3 + 4);
    let got = s.remote().join("site");
    assert_eq!(
        fs::read_to_string(got.join("css/fonts/f.woff")).unwrap(),
        "font"
    );
    assert_eq!(fs::read_to_string(got.join("index.html")).unwrap(), "<p>");
}

#[test]
fn runs_no_more_than_the_parallel_limit() {
    let s = Scratch::new("parallel");
    for i in 0..5 {
        fs::write(s.remote().join(format!("f{i}")), pattern(100_000)).unwrap();
    }
    let (mut remote, mut fake) = connected(&s.remote());
    remote.parallel = 2;
    for i in 0..5 {
        let e = entry(&remote, &format!("f{i}"));
        remote.download(&e, &s.local()).unwrap();
    }
    let running = remote
        .jobs()
        .iter()
        .filter(|j| j.state == State::Running)
        .count();
    assert_eq!(running, 2);
    assert!(run(&mut remote, &mut fake));
    assert!(remote.jobs().iter().all(|j| j.state == State::Done));
}

#[test]
fn pause_resume_and_cancel() {
    let s = Scratch::new("pause");
    let data = pattern(1_000_000);
    fs::write(s.remote().join("p"), &data).unwrap();
    fs::write(s.remote().join("c"), &data).unwrap();
    let (mut remote, mut fake) = connected(&s.remote());
    let p = remote.download(&entry(&remote, "p"), &s.local()).unwrap();
    let c = remote.download(&entry(&remote, "c"), &s.local()).unwrap();
    remote.pause(p);
    remote.cancel(c);
    assert!(run(&mut remote, &mut fake));
    assert_eq!(remote.jobs()[0].state, State::Paused);
    assert_eq!(remote.jobs()[1].state, State::Cancelled);
    assert!(!s.local().join("c.part").exists());
    remote.resume(p);
    assert!(run(&mut remote, &mut fake));
    assert_eq!(remote.jobs()[0].state, State::Done);
    assert_eq!(fs::read(s.local().join("p")).unwrap(), data);
    remote.retry(c);
    assert!(run(&mut remote, &mut fake));
    assert_eq!(fs::read(s.local().join("c")).unwrap(), data);
}

#[test]
fn refuses_names_windows_cannot_hold() {
    let s = Scratch::new("refuse");
    let (mut remote, _) = connected(&s.remote());
    let bad = DirEntry {
        name: b"..".to_vec(),
        attrs: FileAttrs::default(),
    };
    assert!(remote.download(&bad, &s.local()).is_err());
}
