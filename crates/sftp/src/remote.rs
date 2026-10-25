//! One host's file transfer, from the UI's side: the SFTP channel, the
//! directory the side pane lists, and the transfer queue.
//!
//! Everything here is a state machine fed with the worker's messages. It
//! puts the requests it wants sent in an outbox ([`Remote::take_outbox`]),
//! and does the local disk work itself: the worker cannot open files, so a
//! download's bytes arrive in replies and are written here, and an
//! upload's are read here and sent in requests.
//!
//! The queue survives the connection. When it drops, running transfers
//! wait; when the host is connected again, [`Remote::attach`] opens SFTP on
//! the new connection and they carry on from their partial files.
//!
//! Partial files make resuming safe: a download goes to `name.part` and an
//! upload to `name.filepart`, renamed only once complete (and verified, if
//! asked). A download writes strictly in order, so a partial file never
//! has a hole; an upload relies on the server applying writes in order,
//! as SFTP servers do.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use ferossh_ipc::{
    ChannelId, DirEntry, FileAttrs, Message, OpenMode, SftpOp, SftpReply, sftp_code,
};
use sha2::{Digest, Sha256};

use crate::names;

/// Bytes per read or write request.
pub const CHUNK: u32 = 32 * 1024;
/// Requests in flight per file: 1 MiB, enough for a long fat link.
const WINDOW: usize = 32;
/// Transfers running at once unless set otherwise.
pub const DEFAULT_PARALLEL: usize = 3;
/// Names tried for "keep both" before giving up.
const MAX_NUMBERED: u32 = 99;
/// Directory entries kept in the side pane's listing.
const MAX_LISTED: usize = 50_000;

pub type JobId = u32;

/// What a transfer moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Download,
    Upload,
    /// A remote folder, downloaded file by file.
    FetchFolder,
    /// A local folder, uploaded file by file.
    SendFolder,
}

impl Kind {
    fn is_folder(self) -> bool {
        matches!(self, Kind::FetchFolder | Kind::SendFolder)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Queued,
    /// The target exists; waiting for [`Remote::answer`].
    Asking,
    Running,
    Paused,
    /// The connection is gone; it carries on after a reconnect.
    Waiting,
    Done,
    Failed,
    Cancelled,
    Skipped,
}

impl State {
    pub fn finished(self) -> bool {
        matches!(
            self,
            State::Done | State::Failed | State::Cancelled | State::Skipped
        )
    }
}

/// What to do about a target that already exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    Overwrite,
    Skip,
    /// Give the new file a numbered name, `name (2).txt`.
    KeepBoth,
}

/// One entry in the transfer queue.
#[derive(Debug)]
pub struct Job {
    pub id: JobId,
    pub kind: Kind,
    /// What the list shows.
    pub name: String,
    /// The remote file or folder: the source of a download, the target of
    /// an upload.
    pub remote: Vec<u8>,
    /// The local file or folder, the same way round.
    pub local: PathBuf,
    pub size: Option<u64>,
    /// Bytes moved, including a resumed part.
    pub done: u64,
    pub state: State,
    /// An error, or how verification went.
    pub note: Option<String>,
    /// The folder job that added it.
    pub parent: Option<JobId>,
    choice: Option<Choice>,
    /// Bumped whenever the job restarts, so replies to an earlier run are
    /// told apart and dropped.
    generation: u32,
    run: Run,
    speed: Speed,
}

impl Job {
    /// Bytes a second over the last few seconds, while running.
    pub fn rate(&self) -> Option<f64> {
        (self.state == State::Running).then(|| self.speed.rate())?
    }
}

/// A moving average of a job's progress.
#[derive(Debug, Default)]
struct Speed {
    samples: VecDeque<(Instant, u64)>,
}

impl Speed {
    const SPAN: Duration = Duration::from_secs(3);

    fn note(&mut self, done: u64) {
        let now = Instant::now();
        if self
            .samples
            .back()
            .is_some_and(|(t, _)| now.duration_since(*t) < Duration::from_millis(200))
        {
            return;
        }
        self.samples.push_back((now, done));
        while self
            .samples
            .front()
            .is_some_and(|(t, _)| now.duration_since(*t) > Self::SPAN)
        {
            self.samples.pop_front();
        }
    }

    fn rate(&self) -> Option<f64> {
        let (t0, d0) = self.samples.front()?;
        let (t1, d1) = self.samples.back()?;
        let secs = t1.duration_since(*t0).as_secs_f64();
        (secs > 0.5).then(|| d1.saturating_sub(*d0) as f64 / secs)
    }
}

/// A job's state while it runs.
#[derive(Debug, Default)]
enum Run {
    #[default]
    Idle,
    Down(Box<Down>),
    Up(Box<Up>),
    Fetch(Box<Fetch>),
    Send(Box<SendDirs>),
}

/// Where a file transfer is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Looking at the target and a partial file.
    Probing,
    /// Waiting for the remote file's handle.
    Opening,
    /// Bytes are moving.
    Moving,
    /// Waiting for the remote file to close (uploads).
    Closing,
    /// Both sides' SHA-256 are being computed.
    Checking,
    /// Renaming the part file (uploads).
    Renaming,
}

#[derive(Debug)]
struct Down {
    phase: Phase,
    handle: Option<u32>,
    file: Option<File>,
    target: PathBuf,
    part: PathBuf,
    /// The next offset to ask for.
    next: u64,
    /// Where the file ends: its size, or less if it shrank. `u64::MAX`
    /// while unknown.
    end: u64,
    /// Bytes in the part file, all of them in order.
    written: u64,
    inflight: usize,
    /// Chunks that arrived ahead of `written`.
    ahead: BTreeMap<u64, Vec<u8>>,
    check: Check,
}

#[derive(Debug)]
struct Up {
    phase: Phase,
    handle: Option<u32>,
    file: Option<File>,
    target: Vec<u8>,
    part: Vec<u8>,
    overwrite: bool,
    next: u64,
    acked: u64,
    inflight: usize,
    /// The local file is read to its end.
    read_all: bool,
    /// The numbered name being probed for "keep both"; 0 for none.
    numbered: u32,
    check: Check,
}

/// Verification after a transfer: both sides' SHA-256.
#[derive(Debug, Default)]
struct Check {
    local: Option<Result<String, String>>,
    remote: Option<Result<String, String>>,
}

impl Check {
    fn complete(&self) -> bool {
        self.local.is_some() && self.remote.is_some()
    }

    fn mismatch(&self) -> bool {
        matches!((&self.local, &self.remote), (Some(Ok(a)), Some(Ok(b))) if !a.eq_ignore_ascii_case(b))
    }

    fn note(&self) -> Option<String> {
        match (&self.local, &self.remote) {
            (Some(Ok(_)), Some(Ok(_))) => Some("SHA-256 verified".to_owned()),
            (_, Some(Err(why))) | (Some(Err(why)), _) => Some(format!("Not verified: {why}")),
            _ => None,
        }
    }
}

#[derive(Debug)]
struct Fetch {
    /// The folder's canonical path; links must resolve inside it.
    root: Option<Vec<u8>>,
    /// Listings and link lookups not answered yet.
    outstanding: usize,
    scanned: bool,
    links_skipped: usize,
    names_skipped: usize,
    errors: usize,
}

#[derive(Debug)]
struct SendDirs {
    /// Remote folders to make, parents first.
    dirs: Vec<Vec<u8>>,
    /// How many of them exist.
    made: usize,
    /// Files to queue once the folders exist: local, remote, size.
    files: Vec<(PathBuf, Vec<u8>, u64)>,
    queued: bool,
}

/// What a request was for.
#[derive(Debug, Clone)]
enum Purpose {
    List {
        generation: u32,
    },
    Delete,
    Job {
        job: JobId,
        generation: u32,
        step: Step,
    },
    /// Replies nobody waits for (closing a handle, removing a part file).
    Ignore,
}

#[derive(Debug, Clone)]
enum Step {
    StatSource,
    Open,
    Read { offset: u64, len: u32 },
    StatTarget,
    StatPart,
    Write { len: u64 },
    CloseWritten,
    Rename,
    Checksum,
    RootPath,
    ListDir { remote: Vec<u8>, local: PathBuf },
    LinkPath { local: PathBuf },
    LinkStat { remote: Vec<u8>, local: PathBuf },
    Mkdir { path: Vec<u8> },
    MkdirCheck,
}

/// The directory the side pane shows.
#[derive(Debug, Default)]
pub struct Listing {
    pub path: Vec<u8>,
    /// Folders first, then by name.
    pub entries: Vec<DirEntry>,
    pub loading: bool,
    pub error: Option<String>,
    want: Vec<u8>,
    incoming: Vec<DirEntry>,
    generation: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Link {
    Detached,
    Opening(ChannelId),
    Ready(ChannelId),
}

/// A finished local hash: job, generation, result.
type HashResult = (JobId, u32, Result<String, String>);

/// One host's SFTP: its channel, listing and transfer queue.
#[derive(Debug)]
pub struct Remote {
    /// `user@host:port`, which names the queue.
    pub label: String,
    link: Link,
    pub home: Option<Vec<u8>>,
    pub listing: Listing,
    jobs: Vec<Job>,
    next_job: JobId,
    next_id: u32,
    pending: HashMap<u32, Purpose>,
    outbox: Vec<Message>,
    /// Transfers at once.
    pub parallel: usize,
    /// Compare SHA-256 after each file.
    pub verify: bool,
    /// The answer to apply to every further existing target.
    always: Option<Choice>,
    hashes: (Sender<HashResult>, Receiver<HashResult>),
    /// Something the user should read, such as a failed delete.
    pub notice: Option<String>,
}

fn failed_text(code: u32, message: &str) -> String {
    let message = names::display(message.as_bytes());
    let message = message.trim().trim_end_matches('.');
    match code {
        sftp_code::NO_SUCH_FILE => "No such file or folder.".to_owned(),
        sftp_code::PERMISSION_DENIED => "Permission denied.".to_owned(),
        _ if !message.is_empty() => format!("{message}."),
        sftp_code::UNSUPPORTED => "The server does not support this.".to_owned(),
        _ => "The server refused.".to_owned(),
    }
}

fn hash_file(path: &Path) -> Result<String, String> {
    let mut f = File::open(path).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = f.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hasher.update(buf.get(..n).unwrap_or_default());
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// `path` with `suffix` added to its file name.
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// A local name not taken yet: `name (2).ext`, `name (3).ext`, ...
fn free_local(path: &Path) -> Option<PathBuf> {
    let dir = path.parent()?;
    let name = path.file_name()?.to_string_lossy().into_owned();
    (2..=MAX_NUMBERED)
        .map(|n| dir.join(names::numbered(&name, n)))
        .find(|p| !p.exists() && !with_suffix(p, ".part").exists())
}

fn remote_numbered(path: &[u8], n: u32) -> Vec<u8> {
    let dir = names::parent(path);
    let name = String::from_utf8_lossy(names::file_name(path)).into_owned();
    names::join(&dir, names::numbered(&name, n).as_bytes())
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

impl Remote {
    pub fn new(label: &str) -> Self {
        Self {
            label: label.to_owned(),
            link: Link::Detached,
            home: None,
            listing: Listing::default(),
            jobs: Vec::new(),
            next_job: 1,
            next_id: 1,
            pending: HashMap::new(),
            outbox: Vec::new(),
            parallel: DEFAULT_PARALLEL,
            verify: false,
            always: None,
            hashes: channel(),
            notice: None,
        }
    }

    /// The requests to send to the worker.
    pub fn take_outbox(&mut self) -> Vec<Message> {
        std::mem::take(&mut self.outbox)
    }

    /// The SFTP channel, open or opening.
    pub fn channel(&self) -> Option<ChannelId> {
        match self.link {
            Link::Opening(c) | Link::Ready(c) => Some(c),
            Link::Detached => None,
        }
    }

    pub fn is_ready(&self) -> bool {
        matches!(self.link, Link::Ready(_))
    }

    pub fn jobs(&self) -> &[Job] {
        &self.jobs
    }

    /// Something is moving or about to: the window keeps a progress timer
    /// running meanwhile.
    pub fn busy(&self) -> bool {
        self.jobs
            .iter()
            .any(|j| matches!(j.state, State::Running | State::Queued))
    }

    /// Transfers that need a connection to carry on.
    pub fn wants_connection(&self) -> bool {
        self.jobs
            .iter()
            .any(|j| matches!(j.state, State::Waiting | State::Queued | State::Running))
    }

    /// Opens SFTP as channel `channel` on the connection `connection`
    /// made.
    pub fn attach(&mut self, channel: ChannelId, connection: ChannelId) {
        self.detach();
        self.link = Link::Opening(channel);
        self.outbox.push(Message::SftpOpen {
            channel,
            connection,
        });
    }

    /// The channel or its connection is gone: running transfers wait for
    /// the next [`Self::attach`].
    pub fn detach(&mut self) {
        self.link = Link::Detached;
        self.pending.clear();
        self.listing.loading = false;
        for i in 0..self.jobs.len() {
            if self.jobs.get(i).is_some_and(|j| j.state == State::Running) {
                self.drop_run(i);
                if let Some(job) = self.jobs.get_mut(i) {
                    job.state = State::Waiting;
                }
            }
        }
    }

    /// Closes the SFTP channel (no pane uses the connection any more).
    pub fn close(&mut self) {
        if let Some(channel) = self.channel() {
            self.outbox.push(Message::Close { channel });
        }
        self.detach();
    }

    /// A message from the worker for this channel.
    pub fn on_message(&mut self, msg: Message) {
        match msg {
            Message::SftpReady { home, .. } => {
                let Link::Opening(c) = self.link else {
                    return;
                };
                self.link = Link::Ready(c);
                let want = if self.listing.want.is_empty() {
                    if self.listing.path.is_empty() {
                        home.clone()
                    } else {
                        self.listing.path.clone()
                    }
                } else {
                    self.listing.want.clone()
                };
                self.home = Some(home);
                self.browse(&want);
                for job in &mut self.jobs {
                    if job.state == State::Waiting {
                        job.state = State::Queued;
                    }
                }
                self.pump();
            }
            Message::SftpReply { id, reply, .. } => {
                if let Some(purpose) = self.pending.remove(&id) {
                    // A listing comes in parts under one id.
                    if matches!(reply, SftpReply::Entries { last: false, .. }) {
                        self.pending.insert(id, purpose.clone());
                    }
                    self.on_reply(purpose, reply);
                }
                self.pump();
            }
            Message::Disconnected { .. } => self.detach(),
            _ => {}
        }
    }

    fn request(&mut self, op: SftpOp, purpose: Purpose) {
        let Link::Ready(channel) = self.link else {
            return;
        };
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        self.pending.insert(id, purpose);
        self.outbox.push(Message::SftpRequest { channel, id, op });
    }

    fn job_request(&mut self, i: usize, op: SftpOp, step: Step) {
        let Some(job) = self.jobs.get(i) else {
            return;
        };
        let purpose = Purpose::Job {
            job: job.id,
            generation: job.generation,
            step,
        };
        self.request(op, purpose);
    }

    /// Lists `path` in the side pane. The old listing stays until the new
    /// one arrives, and stays if it fails.
    pub fn browse(&mut self, path: &[u8]) {
        self.listing.want = path.to_vec();
        if !self.is_ready() {
            return;
        }
        self.listing.generation = self.listing.generation.wrapping_add(1);
        self.listing.loading = true;
        self.listing.incoming.clear();
        let generation = self.listing.generation;
        self.request(
            SftpOp::List {
                path: path.to_vec(),
            },
            Purpose::List { generation },
        );
    }

    pub fn refresh(&mut self) {
        let path = if self.listing.path.is_empty() {
            self.listing.want.clone()
        } else {
            self.listing.path.clone()
        };
        if !path.is_empty() {
            self.browse(&path);
        }
    }

    /// Deletes a file or an empty folder in the listed directory.
    pub fn delete(&mut self, entry: &DirEntry) {
        let path = names::join(&self.listing.path, &entry.name);
        let op = if entry.attrs.is_dir() {
            SftpOp::Rmdir { path }
        } else {
            SftpOp::Remove { path }
        };
        self.request(op, Purpose::Delete);
    }

    fn add(&mut self, kind: Kind, name: String, remote: Vec<u8>, local: PathBuf) -> JobId {
        let id = self.next_job;
        self.next_job = self.next_job.wrapping_add(1).max(1);
        self.jobs.push(Job {
            id,
            kind,
            name,
            remote,
            local,
            size: None,
            done: 0,
            state: State::Queued,
            note: None,
            parent: None,
            choice: None,
            generation: 0,
            run: Run::Idle,
            speed: Speed::default(),
        });
        id
    }

    /// Queues the download of a listed entry into folder `into`. A folder
    /// comes down whole; links are resolved on the server, and only
    /// followed when they stay inside the folder.
    pub fn download(&mut self, entry: &DirEntry, into: &Path) -> Result<JobId, String> {
        let local = names::local_name(&entry.name).ok_or_else(|| {
            format!(
                "\"{}\" is not a name FeroSSH can create.",
                names::display(&entry.name)
            )
        })?;
        let remote = names::join(&self.listing.path, &entry.name);
        let kind = if entry.attrs.is_dir() {
            Kind::FetchFolder
        } else {
            Kind::Download
        };
        let id = self.add(kind, local.clone(), remote, into.join(&local));
        if let Some(job) = self.jobs.last_mut() {
            job.size = entry.attrs.size.filter(|_| kind == Kind::Download);
        }
        self.pump();
        Ok(id)
    }

    /// Queues the upload of a local file or folder into remote folder
    /// `into`. Links and junctions inside a folder are not followed.
    pub fn upload(&mut self, local: &Path, into: &[u8]) -> Result<JobId, String> {
        let meta = std::fs::metadata(local).map_err(|e| format!("{}: {e}", local.display()))?;
        let name = local
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_owned)
            .ok_or_else(|| format!("{} has no usable file name.", local.display()))?;
        let remote = names::join(into, name.as_bytes());
        if meta.is_dir() {
            let mut dirs = vec![remote.clone()];
            let mut files = Vec::new();
            walk_local(local, &remote, &mut dirs, &mut files)
                .map_err(|e| format!("{}: {e}", local.display()))?;
            let id = self.add(Kind::SendFolder, name, remote, local.to_path_buf());
            if let Some(job) = self.jobs.last_mut() {
                job.size = Some(files.iter().map(|f| f.2).sum());
                job.run = Run::Send(Box::new(SendDirs {
                    dirs,
                    made: 0,
                    files,
                    queued: false,
                }));
            }
            self.pump();
            return Ok(id);
        }
        let id = self.add(Kind::Upload, name, remote, local.to_path_buf());
        if let Some(job) = self.jobs.last_mut() {
            job.size = Some(meta.len());
        }
        self.pump();
        Ok(id)
    }

    fn index(&self, id: JobId) -> Option<usize> {
        self.jobs.iter().position(|j| j.id == id)
    }

    fn children(&self, id: JobId) -> Vec<JobId> {
        self.jobs
            .iter()
            .filter(|j| j.parent == Some(id))
            .map(|j| j.id)
            .collect()
    }

    /// Stops a job (and what a folder queued); [`Self::resume`] carries on
    /// from the partial files.
    pub fn pause(&mut self, id: JobId) {
        for child in self.children(id) {
            self.pause(child);
        }
        if let Some(i) = self.index(id)
            && self
                .jobs
                .get(i)
                .is_some_and(|j| matches!(j.state, State::Running | State::Queued | State::Waiting))
        {
            self.drop_run(i);
            if let Some(job) = self.jobs.get_mut(i) {
                job.state = State::Paused;
            }
        }
        self.pump();
    }

    pub fn resume(&mut self, id: JobId) {
        for child in self.children(id) {
            self.resume(child);
        }
        let next = if self.is_ready() {
            State::Queued
        } else {
            State::Waiting
        };
        if let Some(job) = self.jobs.iter_mut().find(|j| j.id == id)
            && job.state == State::Paused
        {
            job.state = next;
        }
        self.pump();
    }

    /// Stops a job for good and removes its partial file. Cancelling a
    /// folder cancels what it queued.
    pub fn cancel(&mut self, id: JobId) {
        for child in self.children(id) {
            self.cancel(child);
        }
        let Some(i) = self.index(id) else {
            return;
        };
        if self.jobs.get(i).is_none_or(|j| j.state.finished()) {
            return;
        }
        let part = match self.jobs.get(i).map(|j| &j.run) {
            Some(Run::Up(u)) if !u.part.is_empty() => Some(u.part.clone()),
            _ => None,
        };
        self.drop_run(i);
        let Some(job) = self.jobs.get_mut(i) else {
            return;
        };
        job.state = State::Cancelled;
        job.run = Run::Idle;
        if job.kind == Kind::Download {
            let _ = std::fs::remove_file(with_suffix(&job.local, ".part"));
        }
        if let Some(path) = part {
            self.request(SftpOp::Remove { path }, Purpose::Ignore);
        }
        self.pump();
    }

    /// Queues a failed, cancelled or skipped job again.
    pub fn retry(&mut self, id: JobId) {
        for child in self.children(id) {
            self.retry(child);
        }
        let ready = self.is_ready();
        if let Some(job) = self.jobs.iter_mut().find(|j| j.id == id)
            && matches!(job.state, State::Failed | State::Cancelled | State::Skipped)
        {
            job.state = if ready { State::Queued } else { State::Waiting };
            job.note = None;
            job.choice = None;
        }
        self.pump();
    }

    /// Drops finished jobs from the list (a folder's files go with it),
    /// and forgets "apply to all".
    pub fn clear_finished(&mut self) {
        let finished: Vec<JobId> = self
            .jobs
            .iter()
            .filter(|j| j.state.finished())
            .map(|j| j.id)
            .collect();
        self.jobs.retain(|j| {
            !finished.contains(&j.id) || j.parent.is_some_and(|p| !finished.contains(&p))
        });
        if !self.jobs.iter().any(|j| j.state == State::Asking) {
            self.always = None;
        }
    }

    /// The question waiting for an answer: the job and its text.
    pub fn question(&self) -> Option<(JobId, String)> {
        let job = self.jobs.iter().find(|j| j.state == State::Asking)?;
        let place = match job.kind {
            Kind::Upload => format!("in {}", names::display(&names::parent(&job.remote))),
            _ => format!(
                "in {}",
                job.local
                    .parent()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default()
            ),
        };
        Some((job.id, format!("{} already exists {place}.", job.name)))
    }

    /// Answers the question about `id`; `all` answers it for every further
    /// existing target too.
    pub fn answer(&mut self, id: JobId, choice: Choice, all: bool) {
        let ready = self.is_ready();
        let next = if ready { State::Queued } else { State::Waiting };
        if all {
            self.always = Some(choice);
            for job in &mut self.jobs {
                if job.state == State::Asking {
                    job.choice = Some(choice);
                    job.state = next;
                }
            }
        } else if let Some(job) = self.jobs.iter_mut().find(|j| j.id == id)
            && job.state == State::Asking
        {
            job.choice = Some(choice);
            job.state = next;
        }
        self.pump();
    }

    /// Picks up finished local hashes and samples the speeds. Call on a
    /// timer while busy.
    pub fn poll(&mut self) {
        while let Ok((id, generation, result)) = self.hashes.1.try_recv() {
            let Some(i) = self.index(id) else {
                continue;
            };
            let Some(job) = self.jobs.get_mut(i) else {
                continue;
            };
            if job.generation != generation {
                continue;
            }
            match &mut job.run {
                Run::Down(d) => d.check.local = Some(result),
                Run::Up(u) => u.check.local = Some(result),
                _ => {}
            }
            self.advance(i);
        }
        for job in &mut self.jobs {
            if job.state == State::Running {
                job.speed.note(job.done);
            } else {
                job.speed = Speed::default();
            }
        }
        self.pump();
    }

    /// Ends a job's run: its handle is closed and its file dropped, and
    /// replies still on the way are ignored. A folder upload keeps track
    /// of the folders it made.
    fn drop_run(&mut self, i: usize) {
        let Some(job) = self.jobs.get_mut(i) else {
            return;
        };
        job.generation = job.generation.wrapping_add(1);
        let handle = match &job.run {
            Run::Down(d) => d.handle,
            Run::Up(u) => u.handle,
            _ => None,
        };
        if !matches!(job.run, Run::Send(_)) {
            job.run = Run::Idle;
        }
        if let Some(handle) = handle {
            self.request(SftpOp::Close { handle }, Purpose::Ignore);
        }
    }

    fn fail(&mut self, i: usize, why: String) {
        self.drop_run(i);
        if let Some(job) = self.jobs.get_mut(i) {
            job.state = State::Failed;
            job.note = Some(why);
        }
    }

    fn finish(&mut self, i: usize, note: Option<String>) {
        if let Some(job) = self.jobs.get_mut(i) {
            job.state = State::Done;
            job.note = note;
            job.run = Run::Idle;
            if let Some(size) = job.size {
                job.done = job.done.max(size);
            }
        }
    }

    fn skip(&mut self, i: usize) {
        self.drop_run(i);
        if let Some(job) = self.jobs.get_mut(i) {
            job.state = State::Skipped;
        }
    }

    fn ask(&mut self, i: usize) {
        self.drop_run(i);
        if let Some(job) = self.jobs.get_mut(i) {
            job.state = State::Asking;
        }
    }

    /// Starts queued jobs while there is room, and keeps the running ones'
    /// requests flowing.
    fn pump(&mut self) {
        if !self.is_ready() {
            return;
        }
        let file_running = |j: &Job| j.state == State::Running && !j.kind.is_folder();
        let mut active = self.jobs.iter().filter(|j| file_running(j)).count();
        for i in 0..self.jobs.len() {
            let Some(job) = self.jobs.get(i) else {
                break;
            };
            if job.state != State::Queued {
                continue;
            }
            if job.kind.is_folder() {
                self.start(i);
            } else if active < self.parallel {
                active += 1;
                self.start(i);
            }
        }
        for i in 0..self.jobs.len() {
            if self.jobs.get(i).is_some_and(|j| j.state == State::Running) {
                self.top_up(i);
            }
        }
        self.settle_folders();
    }

    fn start(&mut self, i: usize) {
        let always = self.always;
        let Some(job) = self.jobs.get_mut(i) else {
            return;
        };
        job.generation = job.generation.wrapping_add(1);
        job.state = State::Running;
        job.note = None;
        if job.choice.is_none() {
            job.choice = always;
        }
        let remote = job.remote.clone();
        match job.kind {
            Kind::Download => {
                job.run = Run::Idle;
                self.job_request(
                    i,
                    SftpOp::Stat {
                        path: remote,
                        follow: true,
                    },
                    Step::StatSource,
                );
            }
            Kind::Upload => {
                match std::fs::metadata(&job.local) {
                    Ok(m) if m.is_file() => job.size = Some(m.len()),
                    Ok(_) => return self.fail(i, "It is no longer a file.".into()),
                    Err(e) => return self.fail(i, e.to_string()),
                }
                job.run = Run::Up(Box::new(Up {
                    phase: Phase::Probing,
                    handle: None,
                    file: None,
                    target: remote.clone(),
                    part: Vec::new(),
                    overwrite: false,
                    next: 0,
                    acked: 0,
                    inflight: 0,
                    read_all: false,
                    numbered: 0,
                    check: Check::default(),
                }));
                self.job_request(
                    i,
                    SftpOp::Stat {
                        path: remote,
                        follow: false,
                    },
                    Step::StatTarget,
                );
            }
            Kind::FetchFolder => {
                if let Err(e) = std::fs::create_dir_all(&job.local) {
                    let why = format!("{}: {e}", job.local.display());
                    return self.fail(i, why);
                }
                job.run = Run::Fetch(Box::new(Fetch {
                    root: None,
                    outstanding: 1,
                    scanned: false,
                    links_skipped: 0,
                    names_skipped: 0,
                    errors: 0,
                }));
                self.job_request(i, SftpOp::RealPath { path: remote }, Step::RootPath);
            }
            Kind::SendFolder => self.next_mkdir(i),
        }
    }

    /// Sends the next read or write requests of a moving file job.
    fn top_up(&mut self, i: usize) {
        let mut ops: Vec<(SftpOp, Step)> = Vec::new();
        let mut failure = None;
        let Some(job) = self.jobs.get_mut(i) else {
            return;
        };
        match &mut job.run {
            Run::Down(d) if d.phase == Phase::Moving => {
                let Some(handle) = d.handle else {
                    return;
                };
                while d.inflight < WINDOW && d.next < d.end {
                    let len =
                        u32::try_from((d.end - d.next).min(u64::from(CHUNK))).unwrap_or(CHUNK);
                    ops.push((
                        SftpOp::Read {
                            handle,
                            offset: d.next,
                            len,
                        },
                        Step::Read {
                            offset: d.next,
                            len,
                        },
                    ));
                    d.next += u64::from(len);
                    d.inflight += 1;
                }
            }
            Run::Up(u) if u.phase == Phase::Moving => {
                let (Some(handle), Some(file)) = (u.handle, u.file.as_mut()) else {
                    return;
                };
                while u.inflight < WINDOW && !u.read_all {
                    let mut buf = vec![0u8; CHUNK as usize];
                    let n = match read_full(file, &mut buf) {
                        Ok(n) => n,
                        Err(e) => {
                            failure = Some(e.to_string());
                            break;
                        }
                    };
                    if n < buf.len() {
                        u.read_all = true;
                    }
                    if n == 0 {
                        break;
                    }
                    buf.truncate(n);
                    ops.push((
                        SftpOp::Write {
                            handle,
                            offset: u.next,
                            data: buf,
                        },
                        Step::Write { len: n as u64 },
                    ));
                    u.next += n as u64;
                    u.inflight += 1;
                }
            }
            _ => return,
        }
        for (op, step) in ops {
            self.job_request(i, op, step);
        }
        match failure {
            Some(why) => self.fail(i, why),
            None => self.advance(i),
        }
    }

    fn on_reply(&mut self, purpose: Purpose, reply: SftpReply) {
        match purpose {
            Purpose::Ignore => {}
            Purpose::Delete => {
                if let SftpReply::Failed { code, message } = &reply {
                    self.notice =
                        Some(format!("Could not delete: {}", failed_text(*code, message)));
                }
                self.refresh();
            }
            Purpose::List { generation } => self.on_listing(generation, reply),
            Purpose::Job {
                job,
                generation,
                step,
            } => {
                let Some(i) = self.index(job) else {
                    return;
                };
                if self
                    .jobs
                    .get(i)
                    .is_none_or(|j| j.generation != generation || j.state != State::Running)
                {
                    return;
                }
                self.on_job_reply(i, step, reply);
            }
        }
    }

    fn on_listing(&mut self, generation: u32, reply: SftpReply) {
        let l = &mut self.listing;
        if generation != l.generation {
            return;
        }
        match reply {
            SftpReply::Entries { entries, last } => {
                let room = MAX_LISTED.saturating_sub(l.incoming.len());
                l.incoming.extend(
                    entries
                        .into_iter()
                        .filter(|e| !e.name.is_empty() && !e.name.contains(&b'/'))
                        .take(room),
                );
                if last {
                    let mut entries = std::mem::take(&mut l.incoming);
                    let key = |e: &DirEntry| String::from_utf8_lossy(&e.name).to_lowercase();
                    entries.sort_by(|a, b| {
                        b.attrs
                            .is_dir()
                            .cmp(&a.attrs.is_dir())
                            .then_with(|| key(a).cmp(&key(b)))
                            .then_with(|| a.name.cmp(&b.name))
                    });
                    l.entries = entries;
                    l.path = std::mem::take(&mut l.want);
                    l.loading = false;
                    l.error = None;
                }
            }
            SftpReply::Failed { code, message } => {
                l.loading = false;
                l.error = Some(format!(
                    "{}: {}",
                    names::display(&l.want),
                    failed_text(code, &message)
                ));
                l.want.clear();
            }
            _ => l.loading = false,
        }
    }

    fn on_job_reply(&mut self, i: usize, step: Step, reply: SftpReply) {
        if let SftpReply::Failed { code, message } = &reply {
            return self.on_failure(i, step, *code, message);
        }
        match (step, reply) {
            (Step::StatSource, SftpReply::Attrs(a)) => self.download_stat(i, a),
            (Step::Open, SftpReply::Opened { handle }) => {
                match self.jobs.get_mut(i).map(|j| &mut j.run) {
                    Some(Run::Down(d)) => {
                        d.handle = Some(handle);
                        d.phase = Phase::Moving;
                    }
                    Some(Run::Up(u)) => {
                        u.handle = Some(handle);
                        u.phase = Phase::Moving;
                    }
                    // Stopped meanwhile: let the handle go.
                    _ => self.request(SftpOp::Close { handle }, Purpose::Ignore),
                }
                self.top_up(i);
            }
            (Step::Read { offset, len }, SftpReply::Data(data)) => {
                self.downloaded(i, offset, len, data);
            }
            (Step::Read { offset, .. }, SftpReply::Eof) => {
                if let Some(Run::Down(d)) = self.jobs.get_mut(i).map(|j| &mut j.run) {
                    d.inflight = d.inflight.saturating_sub(1);
                    d.end = d.end.min(offset);
                    d.next = d.next.min(d.end);
                    d.ahead.retain(|&o, _| o < offset);
                }
                self.advance(i);
            }
            (Step::StatTarget, SftpReply::Attrs(a)) => self.upload_target_exists(i, a),
            (Step::StatPart, SftpReply::Attrs(a)) => self.upload_open(i, a.size),
            (Step::Write { len }, SftpReply::Done) => {
                if let Some(job) = self.jobs.get_mut(i)
                    && let Run::Up(u) = &mut job.run
                {
                    u.inflight = u.inflight.saturating_sub(1);
                    u.acked += len;
                    job.done = u.acked;
                }
                self.top_up(i);
            }
            (Step::CloseWritten, SftpReply::Done) => self.upload_closed(i),
            (Step::Checksum, SftpReply::Checksum(hex)) => self.set_remote_sum(i, Ok(hex)),
            (Step::Rename, SftpReply::Done) => {
                let note = match self.jobs.get(i).map(|j| &j.run) {
                    Some(Run::Up(u)) => u.check.note(),
                    _ => None,
                };
                self.finish(i, note);
            }
            (Step::RootPath, SftpReply::Path(root)) => {
                let local = self
                    .jobs
                    .get(i)
                    .map(|j| j.local.clone())
                    .unwrap_or_default();
                if let Some(Run::Fetch(f)) = self.jobs.get_mut(i).map(|j| &mut j.run) {
                    f.root = Some(root.clone());
                }
                let step = Step::ListDir {
                    remote: root.clone(),
                    local,
                };
                self.job_request(i, SftpOp::List { path: root }, step);
            }
            (Step::ListDir { remote, local }, SftpReply::Entries { entries, last }) => {
                self.scanned(i, &remote, &local, entries, last);
            }
            (Step::LinkPath { local }, SftpReply::Path(target)) => {
                self.link_resolved(i, local, target);
            }
            (Step::LinkStat { remote, local }, SftpReply::Attrs(a)) => {
                let file = a.is_file() && !a.is_dir();
                if let Some(Run::Fetch(f)) = self.jobs.get_mut(i).map(|j| &mut j.run) {
                    f.outstanding = f.outstanding.saturating_sub(1);
                    if !file {
                        f.links_skipped += 1;
                    }
                }
                if file {
                    self.add_child(i, Kind::Download, remote, local, a.size);
                }
                self.advance(i);
            }
            (Step::Mkdir { .. }, SftpReply::Done) => self.made_dir(i),
            (Step::MkdirCheck, SftpReply::Attrs(a)) => {
                if a.is_dir() {
                    self.made_dir(i);
                } else {
                    self.fail(i, "A file is in the way of a folder.".into());
                }
            }
            (_, other) => {
                let why = format!("Unexpected reply from the SSH worker: {other:?}");
                self.fail(i, why);
            }
        }
    }

    fn on_failure(&mut self, i: usize, step: Step, code: u32, message: &str) {
        match step {
            // No such target: nothing to ask about.
            Step::StatTarget if code == sftp_code::NO_SUCH_FILE => self.upload_part(i),
            // No partial upload: start from the beginning.
            Step::StatPart if code == sftp_code::NO_SUCH_FILE => self.upload_open(i, None),
            // The folder may exist already.
            Step::Mkdir { path } => {
                self.job_request(i, SftpOp::Stat { path, follow: true }, Step::MkdirCheck)
            }
            Step::Checksum => self.set_remote_sum(i, Err(failed_text(code, message))),
            // One unreadable folder or link does not stop the rest.
            Step::ListDir { .. } | Step::LinkPath { .. } | Step::LinkStat { .. } => {
                if let Some(Run::Fetch(f)) = self.jobs.get_mut(i).map(|j| &mut j.run) {
                    f.errors += 1;
                    f.outstanding = f.outstanding.saturating_sub(1);
                }
                self.advance(i);
            }
            _ => self.fail(i, failed_text(code, message)),
        }
    }

    fn download_stat(&mut self, i: usize, attrs: FileAttrs) {
        if attrs.is_dir() {
            return self.fail(i, "It is a folder now.".into());
        }
        let Some(job) = self.jobs.get_mut(i) else {
            return;
        };
        job.size = attrs.size;
        let mut target = job.local.clone();
        if target.exists() {
            match job.choice {
                None => return self.ask(i),
                Some(Choice::Skip) => return self.skip(i),
                Some(Choice::KeepBoth) => match free_local(&target) {
                    Some(t) => target = t,
                    None => return self.fail(i, "No free name to keep both.".into()),
                },
                Some(Choice::Overwrite) => {}
            }
        }
        let part = with_suffix(&target, ".part");
        let have = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
        let resume = if attrs.size.is_some_and(|s| have <= s) {
            have
        } else {
            0
        };
        let opened = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&part)
            .and_then(|mut f| {
                f.set_len(resume)?;
                f.seek(SeekFrom::Start(resume))?;
                Ok(f)
            });
        let file = match opened {
            Ok(f) => f,
            Err(e) => return self.fail(i, format!("{}: {e}", part.display())),
        };
        job.done = resume;
        let remote = job.remote.clone();
        job.run = Run::Down(Box::new(Down {
            phase: Phase::Opening,
            handle: None,
            file: Some(file),
            target,
            part,
            next: resume,
            end: attrs.size.unwrap_or(u64::MAX),
            written: resume,
            inflight: 0,
            ahead: BTreeMap::new(),
            check: Check::default(),
        }));
        self.job_request(
            i,
            SftpOp::Open {
                path: remote,
                mode: OpenMode::Read,
            },
            Step::Open,
        );
    }

    fn downloaded(&mut self, i: usize, offset: u64, len: u32, data: Vec<u8>) {
        let mut again = None;
        let mut failure = None;
        let Some(job) = self.jobs.get_mut(i) else {
            return;
        };
        let Run::Down(d) = &mut job.run else {
            return;
        };
        d.inflight = d.inflight.saturating_sub(1);
        let got = data.len() as u64;
        if got > u64::from(len) {
            failure = Some("The server sent more than was asked for.".to_owned());
        } else if got == 0 {
            // An empty read is an end of file too.
            d.end = d.end.min(offset);
            d.next = d.next.min(d.end);
        } else {
            if got < u64::from(len)
                && let Some(handle) = d.handle
            {
                // A short read: ask for the rest.
                let rest = u32::try_from(u64::from(len) - got).unwrap_or(0);
                again = Some((handle, offset + got, rest));
                d.inflight += 1;
            }
            if offset >= d.written {
                d.ahead.insert(offset, data);
            }
            while let Some(chunk) = d.ahead.remove(&d.written) {
                let Some(file) = d.file.as_mut() else {
                    break;
                };
                if let Err(e) = file.write_all(&chunk) {
                    failure = Some(format!("{}: {e}", d.part.display()));
                    break;
                }
                d.written += chunk.len() as u64;
            }
            job.done = d.written;
        }
        if let Some((handle, offset, len)) = again {
            self.job_request(
                i,
                SftpOp::Read {
                    handle,
                    offset,
                    len,
                },
                Step::Read { offset, len },
            );
        }
        match failure {
            Some(why) => self.fail(i, why),
            None => self.top_up(i),
        }
    }

    /// The remote target of an upload (or a numbered name tried for it)
    /// exists.
    fn upload_target_exists(&mut self, i: usize, attrs: FileAttrs) {
        let numbered = match self.jobs.get(i).map(|j| &j.run) {
            Some(Run::Up(u)) => u.numbered,
            _ => return,
        };
        if numbered > 0 {
            return self.try_numbered(i, numbered + 1);
        }
        if attrs.is_dir() {
            return self.fail(i, "A folder of that name is in the way.".into());
        }
        match self.jobs.get(i).and_then(|j| j.choice) {
            None => self.ask(i),
            Some(Choice::Skip) => self.skip(i),
            Some(Choice::KeepBoth) => self.try_numbered(i, 2),
            Some(Choice::Overwrite) => {
                if let Some(Run::Up(u)) = self.jobs.get_mut(i).map(|j| &mut j.run) {
                    u.overwrite = true;
                }
                self.upload_part(i);
            }
        }
    }

    fn try_numbered(&mut self, i: usize, n: u32) {
        if n > MAX_NUMBERED {
            return self.fail(i, "No free name to keep both.".into());
        }
        let Some(job) = self.jobs.get_mut(i) else {
            return;
        };
        let Run::Up(u) = &mut job.run else {
            return;
        };
        u.numbered = n;
        u.target = remote_numbered(&job.remote, n);
        let path = u.target.clone();
        self.job_request(
            i,
            SftpOp::Stat {
                path,
                follow: false,
            },
            Step::StatTarget,
        );
    }

    /// Looks for a partial upload to resume.
    fn upload_part(&mut self, i: usize) {
        let Some(Run::Up(u)) = self.jobs.get_mut(i).map(|j| &mut j.run) else {
            return;
        };
        let mut part = u.target.clone();
        part.extend_from_slice(b".filepart");
        u.part = part.clone();
        u.numbered = 0;
        self.job_request(
            i,
            SftpOp::Stat {
                path: part,
                follow: false,
            },
            Step::StatPart,
        );
    }

    /// Opens the part file, resuming after `have` bytes when they fit.
    fn upload_open(&mut self, i: usize, have: Option<u64>) {
        let Some(job) = self.jobs.get_mut(i) else {
            return;
        };
        let size = job.size.unwrap_or(0);
        let resume = have.filter(|&h| h <= size).unwrap_or(0);
        let opened = File::open(&job.local).and_then(|mut f| {
            f.seek(SeekFrom::Start(resume))?;
            Ok(f)
        });
        let file = match opened {
            Ok(f) => f,
            Err(e) => {
                let why = format!("{}: {e}", job.local.display());
                return self.fail(i, why);
            }
        };
        job.done = resume;
        let Run::Up(u) = &mut job.run else {
            return;
        };
        u.phase = Phase::Opening;
        u.file = Some(file);
        u.next = resume;
        u.acked = resume;
        let path = u.part.clone();
        self.job_request(
            i,
            SftpOp::Open {
                path,
                mode: OpenMode::Write {
                    truncate: resume == 0,
                },
            },
            Step::Open,
        );
    }

    /// Starts both hashes of a finished file: the local one on a thread,
    /// the remote one on the server.
    fn start_check(&mut self, i: usize, local: PathBuf, remote: Vec<u8>) {
        let Some(job) = self.jobs.get(i) else {
            return;
        };
        let (id, generation) = (job.id, job.generation);
        let tx = self.hashes.0.clone();
        std::thread::spawn(move || {
            let _ = tx.send((id, generation, hash_file(&local)));
        });
        self.job_request(i, SftpOp::Checksum { path: remote }, Step::Checksum);
    }

    /// Moves a job on once its bytes are across: close, verify, rename.
    fn advance(&mut self, i: usize) {
        let verify = self.verify;
        let Some(job) = self.jobs.get_mut(i) else {
            return;
        };
        if job.state != State::Running {
            return;
        }
        match &mut job.run {
            Run::Down(d) => match d.phase {
                Phase::Moving => {
                    if d.inflight > 0 || d.written < d.end {
                        if d.inflight == 0 && d.next >= d.end {
                            return self.fail(i, "The download came out short.".into());
                        }
                        return;
                    }
                    if let Some(handle) = d.handle.take() {
                        self.request(SftpOp::Close { handle }, Purpose::Ignore);
                    }
                    let Some(job) = self.jobs.get_mut(i) else {
                        return;
                    };
                    let Run::Down(d) = &mut job.run else {
                        return;
                    };
                    if let Some(mut file) = d.file.take()
                        && let Err(e) = file.flush()
                    {
                        let why = format!("{}: {e}", d.part.display());
                        return self.fail(i, why);
                    }
                    job.size = Some(d.written);
                    if verify {
                        d.phase = Phase::Checking;
                        let (part, remote) = (d.part.clone(), job.remote.clone());
                        return self.start_check(i, part, remote);
                    }
                    self.complete_download(i);
                }
                Phase::Checking if d.check.complete() => self.complete_download(i),
                _ => {}
            },
            Run::Up(u) => match u.phase {
                Phase::Moving if u.read_all && u.inflight == 0 => {
                    u.file = None;
                    u.phase = Phase::Closing;
                    if let Some(handle) = u.handle.take() {
                        self.job_request(i, SftpOp::Close { handle }, Step::CloseWritten);
                    }
                }
                Phase::Checking if u.check.complete() => {
                    if u.check.mismatch() {
                        let path = u.part.clone();
                        self.request(SftpOp::Remove { path }, Purpose::Ignore);
                        return self.fail(i, MISMATCH.into());
                    }
                    self.rename_upload(i);
                }
                _ => {}
            },
            Run::Fetch(f) => {
                if f.outstanding == 0 {
                    f.scanned = true;
                }
                self.settle_folders();
            }
            _ => {}
        }
    }

    /// The part file is complete (and verified): it becomes the file.
    fn complete_download(&mut self, i: usize) {
        let Some(Run::Down(d)) = self.jobs.get(i).map(|j| &j.run) else {
            return;
        };
        if d.check.mismatch() {
            let _ = std::fs::remove_file(&d.part);
            return self.fail(i, MISMATCH.into());
        }
        let note = d.check.note();
        if let Err(e) = std::fs::rename(&d.part, &d.target) {
            let why = format!("{}: {e}", d.target.display());
            return self.fail(i, why);
        }
        self.finish(i, note);
    }

    fn upload_closed(&mut self, i: usize) {
        let verify = self.verify;
        let Some(job) = self.jobs.get_mut(i) else {
            return;
        };
        let Run::Up(u) = &mut job.run else {
            return;
        };
        if verify {
            u.phase = Phase::Checking;
            let (local, part) = (job.local.clone(), u.part.clone());
            return self.start_check(i, local, part);
        }
        self.rename_upload(i);
    }

    fn rename_upload(&mut self, i: usize) {
        let Some(Run::Up(u)) = self.jobs.get_mut(i).map(|j| &mut j.run) else {
            return;
        };
        u.phase = Phase::Renaming;
        let op = SftpOp::Rename {
            from: u.part.clone(),
            to: u.target.clone(),
            overwrite: u.overwrite,
        };
        self.job_request(i, op, Step::Rename);
    }

    fn set_remote_sum(&mut self, i: usize, sum: Result<String, String>) {
        match self.jobs.get_mut(i).map(|j| &mut j.run) {
            Some(Run::Down(d)) => d.check.remote = Some(sum),
            Some(Run::Up(u)) => u.check.remote = Some(sum),
            _ => {}
        }
        self.advance(i);
    }

    // Folder downloads.

    fn scanned(&mut self, i: usize, dir: &[u8], local: &Path, entries: Vec<DirEntry>, last: bool) {
        for e in entries {
            let Some(name) = names::local_name(&e.name) else {
                if let Some(Run::Fetch(f)) = self.jobs.get_mut(i).map(|j| &mut j.run) {
                    f.names_skipped += 1;
                }
                continue;
            };
            let remote = names::join(dir, &e.name);
            let into = local.join(&name);
            if e.attrs.is_dir() {
                if std::fs::create_dir_all(&into).is_err() {
                    if let Some(Run::Fetch(f)) = self.jobs.get_mut(i).map(|j| &mut j.run) {
                        f.errors += 1;
                    }
                    continue;
                }
                self.bump_outstanding(i);
                let step = Step::ListDir {
                    remote: remote.clone(),
                    local: into,
                };
                self.job_request(i, SftpOp::List { path: remote }, step);
            } else if e.attrs.is_symlink() {
                self.bump_outstanding(i);
                let step = Step::LinkPath { local: into };
                self.job_request(i, SftpOp::RealPath { path: remote }, step);
            } else if e.attrs.is_file() {
                self.add_child(i, Kind::Download, remote, into, e.attrs.size);
            }
            // Devices, sockets and pipes are not files to copy.
        }
        if last {
            if let Some(Run::Fetch(f)) = self.jobs.get_mut(i).map(|j| &mut j.run) {
                f.outstanding = f.outstanding.saturating_sub(1);
            }
            self.advance(i);
        }
    }

    fn bump_outstanding(&mut self, i: usize) {
        if let Some(Run::Fetch(f)) = self.jobs.get_mut(i).map(|j| &mut j.run) {
            f.outstanding += 1;
        }
    }

    /// A link in a folder download resolved to `target`: it is followed
    /// only if that stays inside the folder.
    fn link_resolved(&mut self, i: usize, local: PathBuf, target: Vec<u8>) {
        let inside = match self.jobs.get(i).map(|j| &j.run) {
            Some(Run::Fetch(f)) => f.root.as_deref().is_some_and(|r| names::inside(r, &target)),
            _ => return,
        };
        if !inside {
            if let Some(Run::Fetch(f)) = self.jobs.get_mut(i).map(|j| &mut j.run) {
                f.links_skipped += 1;
                f.outstanding = f.outstanding.saturating_sub(1);
            }
            return self.advance(i);
        }
        self.job_request(
            i,
            SftpOp::Stat {
                path: target.clone(),
                follow: true,
            },
            Step::LinkStat {
                remote: target,
                local,
            },
        );
    }

    fn add_child(
        &mut self,
        i: usize,
        kind: Kind,
        remote: Vec<u8>,
        local: PathBuf,
        size: Option<u64>,
    ) {
        let Some(parent) = self.jobs.get(i).map(|j| j.id) else {
            return;
        };
        // A rescan after a reconnect finds the same files again.
        if self
            .jobs
            .iter()
            .any(|j| j.parent == Some(parent) && j.remote == remote && j.local == local)
        {
            return;
        }
        let name = match kind {
            Kind::Upload => String::from_utf8_lossy(names::file_name(&remote)).into_owned(),
            _ => local
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
        };
        self.add(kind, name, remote, local);
        if let Some(job) = self.jobs.last_mut() {
            job.parent = Some(parent);
            job.size = size;
        }
    }

    // Folder uploads.

    /// Makes the next remote folder, or queues the files once all exist.
    fn next_mkdir(&mut self, i: usize) {
        let Some(Run::Send(s)) = self.jobs.get_mut(i).map(|j| &mut j.run) else {
            return self.fail(i, "Nothing to upload.".into());
        };
        if let Some(path) = s.dirs.get(s.made).cloned() {
            let step = Step::Mkdir { path: path.clone() };
            return self.job_request(i, SftpOp::Mkdir { path }, step);
        }
        if s.queued {
            return;
        }
        s.queued = true;
        let files = std::mem::take(&mut s.files);
        for (local, remote, size) in files {
            self.add_child(i, Kind::Upload, remote, local, Some(size));
        }
        self.settle_folders();
    }

    fn made_dir(&mut self, i: usize) {
        if let Some(Run::Send(s)) = self.jobs.get_mut(i).map(|j| &mut j.run) {
            s.made += 1;
        }
        self.next_mkdir(i);
    }

    /// A folder job is done when its scan is and every file it queued has
    /// finished; it shows their total progress meanwhile.
    fn settle_folders(&mut self) {
        for i in 0..self.jobs.len() {
            let Some(job) = self.jobs.get(i) else {
                continue;
            };
            if !job.kind.is_folder() || job.state != State::Running {
                continue;
            }
            let id = job.id;
            let (scanned, mut notes) = match &job.run {
                Run::Fetch(f) => (f.scanned, fetch_notes(f)),
                Run::Send(s) => (s.queued, Vec::new()),
                _ => (false, Vec::new()),
            };
            let unreadable = matches!(&job.run, Run::Fetch(f) if f.errors > 0);
            let (mut done, mut size, mut count, mut failed, mut open) = (0, 0, 0, 0, 0);
            for c in self.jobs.iter().filter(|j| j.parent == Some(id)) {
                done += c.done;
                size += c.size.unwrap_or(c.done);
                count += 1;
                failed += usize::from(c.state == State::Failed);
                open += usize::from(!c.state.finished());
            }
            let Some(job) = self.jobs.get_mut(i) else {
                continue;
            };
            job.done = done;
            if job.kind == Kind::FetchFolder || scanned {
                job.size = Some(size.max(done));
            }
            if !scanned || open > 0 {
                continue;
            }
            if failed > 0 || count == 0 && unreadable {
                notes.insert(0, format!("{failed} of {count} files failed"));
                job.state = State::Failed;
            } else {
                notes.insert(0, format!("{count} file{}", plural(count)));
                job.state = State::Done;
            }
            job.note = Some(notes.join("; "));
            job.run = Run::Idle;
        }
    }
}

const MISMATCH: &str = "SHA-256 mismatch: the copy differs from the original and was removed.";

fn fetch_notes(f: &Fetch) -> Vec<String> {
    let mut notes = Vec::new();
    if f.links_skipped > 0 {
        notes.push(format!(
            "{} link{} skipped (outside the folder, or to a folder)",
            f.links_skipped,
            plural(f.links_skipped)
        ));
    }
    if f.names_skipped > 0 {
        notes.push(format!(
            "{} unusable name{} skipped",
            f.names_skipped,
            plural(f.names_skipped)
        ));
    }
    if f.errors > 0 {
        notes.push(format!(
            "{} folder{} or link{} unreadable",
            f.errors,
            plural(f.errors),
            plural(f.errors)
        ));
    }
    notes
}

/// Reads until `buf` is full or the file ends.
fn read_full(file: &mut File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        let Some(rest) = buf.get_mut(n..) else {
            break;
        };
        match file.read(rest) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

/// Collects a local folder's subfolders and files for upload, without
/// following links or junctions.
fn walk_local(
    dir: &Path,
    remote: &[u8],
    dirs: &mut Vec<Vec<u8>>,
    files: &mut Vec<(PathBuf, Vec<u8>, u64)>,
) -> std::io::Result<()> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)?.filter_map(Result::ok).collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for e in entries {
        let Ok(meta) = std::fs::symlink_metadata(e.path()) else {
            continue;
        };
        if meta.file_type().is_symlink() || is_reparse_point(&meta) {
            continue;
        }
        let Some(name) = e.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let path = names::join(remote, name.as_bytes());
        if meta.is_dir() {
            dirs.push(path.clone());
            walk_local(&e.path(), &path, dirs, files)?;
        } else if meta.is_file() {
            files.push((e.path(), path, meta.len()));
        }
    }
    Ok(())
}

#[cfg(windows)]
fn is_reparse_point(meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_reparse_point(_meta: &std::fs::Metadata) -> bool {
    false
}
