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
    checksum_unavailable: bool,
    /// Answer reads with half of what was asked.
    short_reads: bool,
    /// Refuse every chmod.
    chmod_fails: bool,
    /// Refuse to list folders with this name.
    unlistable: Option<&'static str>,
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
            checksum_unavailable: false,
            short_reads: false,
            chmod_fails: false,
            unlistable: None,
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
            SftpOp::List { path }
                if self.unlistable.is_some_and(|n| {
                    path == n.as_bytes() || path.ends_with(format!("/{n}").as_bytes())
                }) =>
            {
                one(failed(sftp_code::PERMISSION_DENIED))
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
                        // In name order, so tests that depend on which
                        // name is claimed first are deterministic.
                        entries.sort_by(|a, b| a.name.cmp(&b.name));
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
            SftpOp::Chmod { path, mode } => {
                if self.chmod_fails {
                    return one(failed(sftp_code::PERMISSION_DENIED));
                }
                use std::os::unix::fs::PermissionsExt;
                match fs::set_permissions(self.path(&path), fs::Permissions::from_mode(mode)) {
                    Ok(()) => one(SftpReply::Done),
                    Err(e) => one(Self::io(&e)),
                }
            }
            SftpOp::Checksum { path } => {
                if self.checksum_unavailable {
                    return one(failed(sftp_code::UNSUPPORTED));
                }
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
fn a_listing_is_settled_only_once_the_folder_asked_for_is_in() {
    let s = Scratch::new("settled");
    fs::create_dir(s.remote().join("sub")).unwrap();
    let (mut remote, mut fake) = connected(&s.remote());
    assert!(remote.listing_settled());
    // Asked for, not yet in: the old folder is still listed.
    remote.browse(b"/sub");
    assert!(!remote.listing_settled());
    assert_eq!(remote.listing.path, b"/");
    run(&mut remote, &mut fake);
    assert!(remote.listing_settled());
    assert_eq!(remote.listing.path, b"/sub");
    // A relative folder is in the home folder.
    remote.browse(b"sub");
    run(&mut remote, &mut fake);
    assert_eq!(remote.listing.path, b"/sub");
    // A failed listing keeps the old folder but is not settled.
    remote.browse(b"/missing");
    run(&mut remote, &mut fake);
    assert_eq!(remote.listing.path, b"/sub");
    assert!(!remote.listing_settled());
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
fn a_folder_with_an_unreadable_folder_in_it_is_done_but_not_whole() {
    let s = Scratch::new("whole");
    for name in ["full", "part"] {
        let tree = s.remote().join(name);
        fs::create_dir_all(tree.join("locked")).unwrap();
        fs::write(tree.join("a"), "a").unwrap();
        fs::write(tree.join("locked/b"), "b").unwrap();
    }
    fs::create_dir_all(s.remote().join("empty")).unwrap();
    let (mut remote, mut fake) = connected(&s.remote());
    let full = remote
        .download(&entry(&remote, "full"), &s.local())
        .unwrap();
    let empty = remote
        .download(&entry(&remote, "empty"), &s.local())
        .unwrap();
    assert!(run(&mut remote, &mut fake));
    assert!(remote.whole(full));
    assert!(remote.whole(empty));

    fake.unlistable = Some("part/locked");
    let part = remote
        .download(&entry(&remote, "part"), &s.local())
        .unwrap();
    assert!(run(&mut remote, &mut fake));
    // As a download it is done, with a note of what it missed; whole it
    // is not.
    assert_eq!(remote.job_state(part), Some(State::Done));
    assert!(!remote.whole(part));
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

#[test]
fn a_missing_first_block_bounds_read_ahead_and_recovers() {
    for short in [false, true] {
        let s = Scratch::new(if short { "gap-short" } else { "gap" });
        let data = pattern(3 * 1024 * 1024 + 17);
        fs::write(s.remote().join("gap.bin"), &data).unwrap();
        let (mut remote, mut fake) = connected(&s.remote());
        fake.short_reads = short;
        remote
            .download(&entry(&remote, "gap.bin"), &s.local())
            .unwrap();
        let mut held = None;
        let mut buffered = 0;
        let mut turns = 0;
        loop {
            let out = remote.take_outbox();
            if out.is_empty() {
                break;
            }
            turns += 1;
            assert!(
                turns < 100,
                "reading never stopped behind the missing block"
            );
            for msg in out {
                let Message::SftpRequest { channel, id, op } = msg else {
                    panic!("unexpected message {msg:?}");
                };
                if let SftpOp::Read { offset, len, .. } = &op {
                    assert!(
                        offset + u64::from(*len) <= 1024 * 1024,
                        "read escaped the bounded window: {offset} + {len}"
                    );
                    if *offset == 0 {
                        assert!(held.is_none());
                        held = Some((channel, id, op));
                        continue;
                    }
                }
                for reply in fake.serve(op) {
                    if let SftpReply::Data(bytes) = &reply {
                        buffered += bytes.len();
                    }
                    remote.on_message(Message::SftpReply { channel, id, reply });
                }
            }
        }
        assert!(buffered > 0 && buffered <= 1024 * 1024);
        assert_eq!(remote.jobs()[0].done, 0);
        assert_eq!(remote.jobs()[0].state, State::Running);
        let (channel, id, op) = held.expect("first read was held");
        for reply in fake.serve(op) {
            remote.on_message(Message::SftpReply { channel, id, reply });
        }
        assert!(run(&mut remote, &mut fake));
        assert_eq!(remote.jobs()[0].state, State::Done);
        assert_eq!(fs::read(s.local().join("gap.bin")).unwrap(), data);
    }
}

#[test]
fn stale_partial_downloads_are_never_promoted_without_verification_enabled() {
    for partial in [b"OLD!".as_slice(), b"OLD!tail".as_slice()] {
        let s = Scratch::new("stale-down");
        fs::write(s.remote().join("f"), b"NEW!tail").unwrap();
        fs::write(s.local().join("f.part"), partial).unwrap();
        let (mut remote, mut fake) = connected(&s.remote());
        assert!(!remote.verify);
        remote.download(&entry(&remote, "f"), &s.local()).unwrap();
        assert!(run(&mut remote, &mut fake));
        assert_eq!(remote.jobs()[0].state, State::Failed);
        assert!(!s.local().join("f").exists());
    }
}

#[test]
fn stale_partial_uploads_are_never_promoted_without_verification_enabled() {
    for partial in [b"OLD!".as_slice(), b"OLD!tail".as_slice()] {
        let s = Scratch::new("stale-up");
        let local = s.local().join("f");
        fs::write(&local, b"NEW!tail").unwrap();
        fs::write(s.remote().join("f.filepart"), partial).unwrap();
        let (mut remote, mut fake) = connected(&s.remote());
        assert!(!remote.verify);
        remote.upload(&local, b"/").unwrap();
        assert!(run(&mut remote, &mut fake));
        assert_eq!(remote.jobs()[0].state, State::Failed);
        assert!(!s.remote().join("f").exists());
    }
}

#[test]
fn resumed_transfers_fail_closed_when_checksums_are_unavailable() {
    let s = Scratch::new("resume-no-checksum");
    fs::write(s.remote().join("down"), b"correct content").unwrap();
    fs::write(s.local().join("down.part"), b"correct").unwrap();
    let up = s.local().join("up");
    fs::write(&up, b"correct content").unwrap();
    fs::write(s.remote().join("up.filepart"), b"correct").unwrap();
    let (mut remote, mut fake) = connected(&s.remote());
    assert!(!remote.verify);
    fake.checksum_unavailable = true;
    remote
        .download(&entry(&remote, "down"), &s.local())
        .unwrap();
    remote.upload(&up, b"/").unwrap();
    assert!(run(&mut remote, &mut fake));
    assert!(remote.jobs().iter().all(|j| j.state == State::Failed));
    assert!(!s.local().join("down").exists());
    assert!(!s.remote().join("up").exists());
    assert_eq!(
        fs::read(s.local().join("down.part")).unwrap(),
        b"correct content"
    );
    assert_eq!(
        fs::read(s.remote().join("up.filepart")).unwrap(),
        b"correct content"
    );
}

#[test]
fn makes_folders_renames_and_sets_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let s = Scratch::new("folderops");
    fs::write(s.remote().join("a.txt"), "a").unwrap();
    fs::write(s.remote().join("b.txt"), "b").unwrap();
    let (mut remote, mut fake) = connected(&s.remote());

    remote.make_folder(b"new").unwrap();
    assert!(run(&mut remote, &mut fake));
    assert!(s.remote().join("new").is_dir());
    assert!(remote.listing.entries.iter().any(|e| e.name == b"new"));
    // Made again: the server refuses, and the pane says so.
    remote.make_folder(b"new").unwrap();
    assert!(run(&mut remote, &mut fake));
    let notice = remote.notice.take().unwrap();
    assert!(
        notice.starts_with("Could not create the folder"),
        "{notice}"
    );
    for bad in [&b""[..], b".", b"..", b"x/y", b"a\nb"] {
        assert!(remote.make_folder(bad).is_err(), "{bad:?}");
        assert!(remote.rename(b"a.txt", bad).is_err(), "{bad:?}");
    }

    remote.rename(b"a.txt", b"c.txt").unwrap();
    assert!(run(&mut remote, &mut fake));
    assert_eq!(fs::read_to_string(s.remote().join("c.txt")).unwrap(), "a");
    // Never over an existing name.
    remote.rename(b"c.txt", b"b.txt").unwrap();
    assert!(run(&mut remote, &mut fake));
    assert!(
        remote
            .notice
            .take()
            .unwrap()
            .starts_with("Could not rename")
    );
    assert_eq!(fs::read_to_string(s.remote().join("b.txt")).unwrap(), "b");

    remote.chmod(b"b.txt", 0o100_750);
    assert!(run(&mut remote, &mut fake));
    let mode = fs::metadata(s.remote().join("b.txt"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o7777, 0o750);
    let listed = entry(&remote, "b.txt").attrs.permissions.unwrap();
    assert_eq!(listed & 0o7777, 0o750);
}

#[test]
fn puts_an_edited_file_back_without_asking_and_keeps_its_mode() {
    use std::os::unix::fs::PermissionsExt;
    let s = Scratch::new("putback");
    let script = s.remote().join("run.sh");
    fs::write(&script, "echo old\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    // A part left by an earlier save, longer than nothing: never resumed.
    fs::write(
        s.remote().join("run.sh.filepart"),
        "echo stale and longer\n",
    )
    .unwrap();
    let local = s.local().join("run.sh");
    fs::write(&local, "echo new and longer still\n").unwrap();
    let (mut remote, mut fake) = connected(&s.remote());
    let id = remote
        .put_back(&local, b"/run.sh", Some(0o100_755))
        .unwrap();
    assert!(run(&mut remote, &mut fake));
    assert_eq!(remote.job_state(id), Some(State::Done));
    assert_eq!(
        fs::read_to_string(&script).unwrap(),
        "echo new and longer still\n"
    );
    let mode = fs::metadata(&script).unwrap().permissions().mode();
    assert_eq!(mode & 0o7777, 0o755);
    assert!(remote.put_back(&s.local(), b"/x", None).is_err());
}

#[test]
fn saves_of_the_same_file_go_one_at_a_time() {
    let s = Scratch::new("putback-twice");
    let script = s.remote().join("run.sh");
    fs::write(&script, "echo old\n").unwrap();
    let (first, second) = (s.local().join("a"), s.local().join("b"));
    fs::write(&first, "echo first and much longer\n").unwrap();
    fs::write(&second, "echo second\n").unwrap();
    let (mut remote, mut fake) = connected(&s.remote());
    let one = remote.put_back(&first, b"/run.sh", None).unwrap();
    let two = remote.put_back(&second, b"/run.sh", None).unwrap();
    assert_eq!(remote.job_state(one), Some(State::Running));
    assert_eq!(remote.job_state(two), Some(State::Queued));
    // A paused save still owns the part file.
    remote.pause(one);
    assert_eq!(remote.job_state(one), Some(State::Paused));
    assert_eq!(remote.job_state(two), Some(State::Queued));
    remote.resume(one);
    assert!(run(&mut remote, &mut fake));
    assert_eq!(remote.job_state(one), Some(State::Done));
    assert_eq!(remote.job_state(two), Some(State::Done));
    assert_eq!(fs::read_to_string(&script).unwrap(), "echo second\n");
}

#[test]
fn a_retried_put_back_still_replaces_without_asking() {
    let s = Scratch::new("putback-retry");
    let script = s.remote().join("run.sh");
    fs::write(&script, "echo old\n").unwrap();
    let local = s.local().join("run.sh");
    fs::write(&local, "echo new\n").unwrap();
    let (mut remote, mut fake) = connected(&s.remote());
    fake.chmod_fails = true;
    let id = remote
        .put_back(&local, b"/run.sh", Some(0o100_644))
        .unwrap();
    run(&mut remote, &mut fake);
    assert_eq!(remote.job_state(id), Some(State::Failed));
    fake.chmod_fails = false;
    remote.retry(id);
    assert!(run(&mut remote, &mut fake));
    assert_eq!(remote.job_state(id), Some(State::Done));
    assert_eq!(fs::read_to_string(&script).unwrap(), "echo new\n");
}

#[test]
fn a_failed_chmod_leaves_the_original_in_place() {
    let s = Scratch::new("putback-chmod");
    let script = s.remote().join("run.sh");
    fs::write(&script, "echo old\n").unwrap();
    let local = s.local().join("run.sh");
    fs::write(&local, "echo new\n").unwrap();
    let (mut remote, mut fake) = connected(&s.remote());
    fake.chmod_fails = true;
    let id = remote
        .put_back(&local, b"/run.sh", Some(0o100_755))
        .unwrap();
    run(&mut remote, &mut fake);
    assert_eq!(remote.job_state(id), Some(State::Failed));
    assert_eq!(fs::read_to_string(&script).unwrap(), "echo old\n");
}

#[test]
fn a_file_named_like_another_files_part_file_survives_in_a_folder() {
    let s = Scratch::new("partname");
    let tree = s.remote().join("d");
    fs::create_dir_all(&tree).unwrap();
    let big = pattern(300_000);
    let small: Vec<u8> = pattern(1_000).iter().map(|b| b ^ 0x55).collect();
    // `a|` becomes `a_`, which downloads through `a_.part`. The fake lists
    // in name order, so `a_.part` is claimed first: the order that collides.
    fs::write(tree.join("a_.part"), &big).unwrap();
    fs::write(tree.join("a|"), &small).unwrap();
    let (mut remote, mut fake) = connected(&s.remote());
    remote.parallel = 1;
    let folder = remote.download(&entry(&remote, "d"), &s.local()).unwrap();
    assert!(run(&mut remote, &mut fake));
    assert!(remote.whole(folder));
    let mut got: Vec<Vec<u8>> = fs::read_dir(s.local().join("d"))
        .unwrap()
        .map(|e| fs::read(e.unwrap().path()).unwrap())
        .collect();
    got.sort_by_key(Vec::len);
    assert_eq!(got, vec![small, big]);
}

#[test]
fn folders_named_like_part_files_keep_their_names() {
    let s = Scratch::new("partdirs");
    let tree = s.remote().join("d");
    fs::create_dir_all(tree.join("assets")).unwrap();
    fs::create_dir_all(tree.join("assets.part")).unwrap();
    fs::write(tree.join("assets/a"), b"a").unwrap();
    fs::write(tree.join("assets.part/b"), b"b").unwrap();
    let (mut remote, mut fake) = connected(&s.remote());
    let folder = remote.download(&entry(&remote, "d"), &s.local()).unwrap();
    assert!(run(&mut remote, &mut fake));
    assert!(remote.whole(folder));
    let d = s.local().join("d");
    assert_eq!(fs::read(d.join("assets/a")).unwrap(), b"a");
    assert_eq!(fs::read(d.join("assets.part/b")).unwrap(), b"b");
}

#[test]
fn retrying_a_folder_brings_back_a_done_file_that_went_missing() {
    let s = Scratch::new("retrygone");
    let tree = s.remote().join("d");
    fs::create_dir_all(&tree).unwrap();
    let big = pattern(2_000_000);
    fs::write(tree.join("x"), b"small").unwrap();
    fs::write(tree.join("y"), &big).unwrap();
    let (mut remote, mut fake) = connected(&s.remote());
    remote.parallel = 1;
    let folder = remote.download(&entry(&remote, "d"), &s.local()).unwrap();
    fake.drop_after = Some(fake.answered + 30);
    assert!(!run(&mut remote, &mut fake));
    assert_eq!(fs::read(s.local().join("d/x")).unwrap(), b"small");
    // `y` is gone when the connection is back: the folder fails.
    fs::rename(tree.join("y"), s.remote().join("y")).unwrap();
    remote.attach(CHANNEL + 1, 2);
    assert!(run(&mut remote, &mut fake));
    assert_eq!(remote.job_state(folder), Some(State::Failed));
    // The download folder is swept; `y` is back; Retry brings both.
    fs::remove_dir_all(s.local().join("d")).unwrap();
    fs::rename(s.remote().join("y"), tree.join("y")).unwrap();
    remote.retry(folder);
    assert!(run(&mut remote, &mut fake));
    assert!(remote.whole(folder));
    assert_eq!(fs::read(s.local().join("d/x")).unwrap(), b"small");
    assert_eq!(fs::read(s.local().join("d/y")).unwrap(), big);
}

#[test]
fn two_names_for_one_local_name_never_share_a_part_file() {
    let s = Scratch::new("collide");
    let one = pattern(300_000);
    let two: Vec<u8> = pattern(250_000).iter().map(|b| b ^ 0x55).collect();
    // Both become `a_b` on Windows-safe local names.
    fs::write(s.remote().join("a:b"), &one).unwrap();
    fs::write(s.remote().join("a_b"), &two).unwrap();
    let (mut remote, mut fake) = connected(&s.remote());
    remote.parallel = 2;
    let (e1, e2) = (entry(&remote, "a:b"), entry(&remote, "a_b"));
    remote.download(&e1, &s.local()).unwrap();
    remote.download(&e2, &s.local()).unwrap();
    assert!(run(&mut remote, &mut fake));
    for job in remote.jobs() {
        assert_eq!(job.state, State::Done, "{:?}", job.note);
    }
    let mut got: Vec<Vec<u8>> = fs::read_dir(s.local())
        .unwrap()
        .map(|e| fs::read(e.unwrap().path()).unwrap())
        .collect();
    got.sort_by_key(Vec::len);
    assert_eq!(got, vec![two, one]);
}

#[test]
fn names_that_differ_only_in_case_never_share_a_part_file() {
    let s = Scratch::new("case");
    let one = pattern(300_000);
    let two: Vec<u8> = pattern(250_000).iter().map(|b| b ^ 0x33).collect();
    // One file on Windows, two here: treated as one either way.
    fs::write(s.remote().join("README"), &one).unwrap();
    fs::write(s.remote().join("readme"), &two).unwrap();
    let (mut remote, mut fake) = connected(&s.remote());
    remote.parallel = 2;
    remote
        .download(&entry(&remote, "README"), &s.local())
        .unwrap();
    remote
        .download(&entry(&remote, "readme"), &s.local())
        .unwrap();
    assert!(run(&mut remote, &mut fake));
    assert_eq!(fs::read(s.local().join("README")).unwrap(), one);
    assert_eq!(fs::read(s.local().join("readme (2)")).unwrap(), two);
}

#[test]
fn a_numbered_download_keeps_its_name_across_a_reconnect_and_cancel() {
    let s = Scratch::new("collide-drop");
    let one = pattern(2_000_000);
    let two: Vec<u8> = pattern(1_500_000).iter().map(|b| b ^ 0x55).collect();
    fs::write(s.remote().join("a:b"), &one).unwrap();
    fs::write(s.remote().join("a_b"), &two).unwrap();
    let (mut remote, mut fake) = connected(&s.remote());
    remote.parallel = 2;
    remote.download(&entry(&remote, "a:b"), &s.local()).unwrap();
    remote.download(&entry(&remote, "a_b"), &s.local()).unwrap();
    fake.drop_after = Some(fake.answered + 40);
    assert!(!run(&mut remote, &mut fake));
    let numbered = s.local().join("a_b (2).part");
    let part = fs::metadata(&numbered).unwrap().len();
    assert!(part > 0, "{part}");
    // Back again: each job goes on with its own part file, from where it
    // was; nothing starts over under a third name.
    remote.attach(CHANNEL + 1, 2);
    assert!(run(&mut remote, &mut fake));
    for job in remote.jobs() {
        assert_eq!(job.state, State::Done, "{:?}", job.note);
    }
    assert_eq!(fs::read(s.local().join("a_b")).unwrap(), one);
    assert_eq!(fs::read(s.local().join("a_b (2)")).unwrap(), two);
    assert!(!s.local().join("a_b (3)").exists());

    // Cancelling the numbered one removes its own part file only.
    let s = Scratch::new("collide-cancel");
    fs::write(s.remote().join("a:b"), &one).unwrap();
    fs::write(s.remote().join("a_b"), &two).unwrap();
    let (mut remote, mut fake) = connected(&s.remote());
    remote.parallel = 2;
    remote.download(&entry(&remote, "a:b"), &s.local()).unwrap();
    let second = remote.download(&entry(&remote, "a_b"), &s.local()).unwrap();
    fake.drop_after = Some(fake.answered + 40);
    assert!(!run(&mut remote, &mut fake));
    assert!(s.local().join("a_b.part").exists());
    remote.cancel(second);
    assert!(!s.local().join("a_b (2).part").exists());
    assert!(s.local().join("a_b.part").exists());
    remote.attach(CHANNEL + 1, 2);
    assert!(run(&mut remote, &mut fake));
    assert_eq!(fs::read(s.local().join("a_b")).unwrap(), one);
}

#[test]
fn cancelling_a_download_that_never_started_leaves_the_others_part_alone() {
    let s = Scratch::new("collide-queued");
    let one = pattern(2_000_000);
    fs::write(s.remote().join("a:b"), &one).unwrap();
    fs::write(s.remote().join("a_b"), b"other").unwrap();
    let (mut remote, mut fake) = connected(&s.remote());
    remote.parallel = 1;
    remote.download(&entry(&remote, "a:b"), &s.local()).unwrap();
    let second = remote.download(&entry(&remote, "a_b"), &s.local()).unwrap();
    // The first is part way through (and paused by the dropped
    // connection); the second never started.
    fake.drop_after = Some(fake.answered + 20);
    assert!(!run(&mut remote, &mut fake));
    let part = s.local().join("a_b.part");
    let have = fs::metadata(&part).unwrap().len();
    assert!(have > 0);
    remote.cancel(second);
    assert_eq!(fs::metadata(&part).unwrap().len(), have);
    remote.attach(CHANNEL + 1, 2);
    fake.reads.clear();
    assert!(run(&mut remote, &mut fake));
    assert_eq!(fs::read(s.local().join("a_b")).unwrap(), one);
    // It carried on from its part file instead of starting over.
    assert!(
        fake.reads.first().is_some_and(|&r| r > 0),
        "{:?}",
        fake.reads.first()
    );
}

#[test]
fn a_failed_downloads_part_file_is_kept_for_its_retry() {
    let s = Scratch::new("collide-failed");
    let one = pattern(2_000_000);
    fs::write(s.remote().join("a:b"), &one).unwrap();
    fs::write(s.remote().join("a_b"), b"other").unwrap();
    let (mut remote, mut fake) = connected(&s.remote());
    remote.parallel = 1;
    let first = remote.download(&entry(&remote, "a:b"), &s.local()).unwrap();
    let second = remote.download(&entry(&remote, "a_b"), &s.local()).unwrap();
    fake.drop_after = Some(fake.answered + 20);
    assert!(!run(&mut remote, &mut fake));
    let part = s.local().join("a_b.part");
    let have = fs::metadata(&part).unwrap().len();
    assert!(have > 0);
    // Cancelling the namesake that never started touches nothing.
    remote.cancel(second);
    assert_eq!(fs::metadata(&part).unwrap().len(), have);
    // The source is gone when the connection is back: the first fails,
    // keeping its part file for a retry.
    fs::rename(s.remote().join("a:b"), s.remote().join("moved")).unwrap();
    remote.attach(CHANNEL + 1, 2);
    assert!(run(&mut remote, &mut fake));
    assert_eq!(remote.job_state(first), Some(State::Failed));
    assert_eq!(fs::metadata(&part).unwrap().len(), have);
    // A new download of the same local name does not take it over.
    remote.download(&entry(&remote, "a_b"), &s.local()).unwrap();
    assert!(run(&mut remote, &mut fake));
    assert_eq!(fs::metadata(&part).unwrap().len(), have);
    assert_eq!(fs::read(s.local().join("a_b (2)")).unwrap(), b"other");
}

#[test]
fn a_part_file_never_takes_another_downloads_final_name() {
    let s = Scratch::new("collide-target");
    let one = pattern(400_000);
    let two: Vec<u8> = pattern(300_000).iter().map(|b| b ^ 0x11).collect();
    let three: Vec<u8> = pattern(350_000).iter().map(|b| b ^ 0x22).collect();
    // The third maps onto the first's name; its first numbered name's
    // part file, `a_b (2).part`, is the second's final name.
    fs::write(s.remote().join("a:b"), &one).unwrap();
    fs::write(s.remote().join("a_b (2).part"), &two).unwrap();
    fs::write(s.remote().join("a_b"), &three).unwrap();
    let (mut remote, mut fake) = connected(&s.remote());
    remote.parallel = 3;
    for name in ["a:b", "a_b (2).part", "a_b"] {
        remote.download(&entry(&remote, name), &s.local()).unwrap();
    }
    assert!(run(&mut remote, &mut fake));
    for job in remote.jobs() {
        assert_eq!(job.state, State::Done, "{:?}", job.note);
    }
    assert_eq!(fs::read(s.local().join("a_b")).unwrap(), one);
    assert_eq!(fs::read(s.local().join("a_b (2).part")).unwrap(), two);
    assert_eq!(fs::read(s.local().join("a_b (3)")).unwrap(), three);
}

#[test]
fn a_numbered_long_name_still_fits_with_its_part_file() {
    // 248 bytes locally: with " (2)" and ".part" it would be 257.
    long_collision("collide-long", &"a".repeat(247), "");
    // Nearly all extension: that has to be cut too.
    long_collision("collide-long-ext", "a", &format!(".{}", "x".repeat(247)));
}

/// Downloads `{stem}:{ext}` and `{stem}_{ext}`, which land on one local
/// name, and checks the second's numbered name and part file fit.
fn long_collision(scratch: &str, stem: &str, ext: &str) {
    let s = Scratch::new(scratch);
    let one = pattern(100_000);
    let two: Vec<u8> = pattern(90_000).iter().map(|b| b ^ 0x44).collect();
    let (colon, under) = (format!("{stem}:{ext}"), format!("{stem}_{ext}"));
    fs::write(s.remote().join(&colon), &one).unwrap();
    fs::write(s.remote().join(&under), &two).unwrap();
    let (mut remote, mut fake) = connected(&s.remote());
    remote.parallel = 2;
    remote
        .download(&entry(&remote, &colon), &s.local())
        .unwrap();
    remote
        .download(&entry(&remote, &under), &s.local())
        .unwrap();
    assert!(run(&mut remote, &mut fake));
    for job in remote.jobs() {
        assert_eq!(job.state, State::Done, "{:?}", job.note);
    }
    assert_eq!(fs::read(s.local().join(&under)).unwrap(), one);
    let numbered: Vec<_> = fs::read_dir(s.local())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| *n != under)
        .collect();
    assert_eq!(numbered.len(), 1, "{numbered:?}");
    let name = numbered.first().unwrap();
    assert!(name.contains(" (2)"), "{name}");
    assert!(name.len() + ".part".len() <= 255, "{}", name.len());
    assert_eq!(fs::read(s.local().join(name)).unwrap(), two);
}

#[test]
fn uploads_into_a_home_folder_it_makes_and_numbers_taken_names() {
    let s = Scratch::new("into-home");
    let shot = s.local().join("shot.png");
    fs::write(&shot, "png").unwrap();
    // Queued before the channel is open, so before the home folder is known.
    let mut remote = Remote::new("me@host");
    remote.attach(CHANNEL, 1);
    let mut fake = Fake::new(s.remote());
    let first = remote
        .upload_into_home(&shot, b".cache/ferossh/paste")
        .unwrap();
    assert!(run(&mut remote, &mut fake));
    assert_eq!(remote.job_state(first), Some(State::Done));
    assert_eq!(
        remote.landed(first),
        Some(&b"/.cache/ferossh/paste/shot.png"[..])
    );
    // Again, with the folders there: no question, a numbered name.
    let second = remote
        .upload_into_home(&shot, b".cache/ferossh/paste")
        .unwrap();
    assert!(run(&mut remote, &mut fake));
    assert_eq!(remote.job_state(second), Some(State::Done));
    assert_eq!(
        remote.landed(second),
        Some(&b"/.cache/ferossh/paste/shot (2).png"[..])
    );
    let dir = s.remote().join(".cache/ferossh/paste");
    assert_eq!(fs::read_to_string(dir.join("shot.png")).unwrap(), "png");
    assert_eq!(fs::read_to_string(dir.join("shot (2).png")).unwrap(), "png");
}

#[test]
fn an_upload_into_the_home_folder_stops_at_a_file_in_the_way() {
    let s = Scratch::new("into-home-file");
    let shot = s.local().join("shot.png");
    fs::write(&shot, "png").unwrap();
    fs::write(s.remote().join(".cache"), "not a folder").unwrap();
    let (mut remote, mut fake) = connected(&s.remote());
    let id = remote.upload_into_home(&shot, b".cache/ferossh").unwrap();
    assert!(run(&mut remote, &mut fake));
    assert_eq!(remote.job_state(id), Some(State::Failed));
    assert_eq!(remote.landed(id), None);
}

#[test]
fn an_upload_into_the_home_folder_takes_only_a_plain_path_and_a_file() {
    let s = Scratch::new("into-home-plain");
    let shot = s.local().join("shot.png");
    fs::write(&shot, "png").unwrap();
    let (mut remote, _) = connected(&s.remote());
    for bad in [&b""[..], b"/tmp", b"../up", b"a//b", b"a/./b", b"a/"] {
        assert!(remote.upload_into_home(&shot, bad).is_err(), "{bad:?}");
    }
    assert!(remote.upload_into_home(&s.local(), b"paste").is_err());
    assert!(remote.jobs().is_empty());
}

#[test]
fn two_pasted_files_of_one_name_never_share_a_part_file() {
    let s = Scratch::new("into-home-same");
    fs::create_dir_all(s.local().join("a")).unwrap();
    fs::create_dir_all(s.local().join("b")).unwrap();
    fs::write(s.local().join("a/shot.png"), "first").unwrap();
    fs::write(s.local().join("b/shot.png"), "second").unwrap();
    let (mut remote, mut fake) = connected(&s.remote());
    let a = remote
        .upload_into_home(&s.local().join("a/shot.png"), b"paste")
        .unwrap();
    let b = remote
        .upload_into_home(&s.local().join("b/shot.png"), b"paste")
        .unwrap();
    assert!(run(&mut remote, &mut fake));
    assert_eq!(remote.landed(a), Some(&b"/paste/shot.png"[..]));
    assert_eq!(remote.landed(b), Some(&b"/paste/shot (2).png"[..]));
    let dir = s.remote().join("paste");
    assert_eq!(fs::read_to_string(dir.join("shot.png")).unwrap(), "first");
    assert_eq!(
        fs::read_to_string(dir.join("shot (2).png")).unwrap(),
        "second"
    );
}

#[test]
fn a_retried_paste_upload_still_keeps_both() {
    let s = Scratch::new("into-home-retry");
    let shot = s.local().join("shot.png");
    fs::write(&shot, "new").unwrap();
    fs::create_dir_all(s.remote().join("paste")).unwrap();
    fs::write(s.remote().join("paste/shot.png"), "old").unwrap();
    let (mut remote, mut fake) = connected(&s.remote());
    // "Overwrite all" chosen for another transfer earlier.
    let other = s.local().join("other.txt");
    fs::write(&other, "x").unwrap();
    fs::write(s.remote().join("other.txt"), "y").unwrap();
    let o = remote.upload(&other, b"/").unwrap();
    assert!(run(&mut remote, &mut fake));
    remote.answer(o, Choice::Overwrite, true);
    assert!(run(&mut remote, &mut fake));
    let id = remote.upload_into_home(&shot, b"paste").unwrap();
    remote.cancel(id);
    remote.retry(id);
    assert!(run(&mut remote, &mut fake));
    assert_eq!(remote.landed(id), Some(&b"/paste/shot (2).png"[..]));
    let dir = s.remote().join("paste");
    assert_eq!(fs::read_to_string(dir.join("shot.png")).unwrap(), "old");
}

#[test]
fn a_numbered_name_another_upload_is_writing_is_skipped() {
    let s = Scratch::new("into-home-numbered");
    fs::create_dir_all(s.remote().join("paste")).unwrap();
    fs::write(s.remote().join("paste/shot.png"), "old").unwrap();
    fs::write(s.local().join("shot.png"), "new").unwrap();
    fs::write(s.local().join("shot (2).png"), "named two").unwrap();
    let (mut remote, mut fake) = connected(&s.remote());
    remote.parallel = 4;
    // Both start at once: the first must not pick the second's name.
    let named = remote
        .upload_into_home(&s.local().join("shot (2).png"), b"paste")
        .unwrap();
    let kept = remote
        .upload_into_home(&s.local().join("shot.png"), b"paste")
        .unwrap();
    assert!(run(&mut remote, &mut fake));
    assert_eq!(remote.landed(named), Some(&b"/paste/shot (2).png"[..]));
    assert_eq!(remote.landed(kept), Some(&b"/paste/shot (3).png"[..]));
    let dir = s.remote().join("paste");
    assert_eq!(
        fs::read_to_string(dir.join("shot (2).png")).unwrap(),
        "named two"
    );
    assert_eq!(fs::read_to_string(dir.join("shot (3).png")).unwrap(), "new");
}
