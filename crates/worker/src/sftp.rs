//! SFTP on another channel of a logged-in connection, for the UI's file
//! transfers.
//!
//! The worker speaks the protocol to the server and the UI asks it for one
//! operation at a time; file contents travel in those messages, since the
//! worker cannot open files. The server's handles stay here, behind small
//! numbers of the worker's own, and every reply is checked by the codec
//! before the UI sees it. Requests are pipelined: the UI may have many
//! reads or writes in flight, and the replies go back as they come.
//!
//! Remote checksums run `sha256sum` on an exec channel of the same
//! connection; that is the one thing SFTP itself cannot do. The file's
//! name never goes on that command line: the server hands the line to the
//! user's login shell, which may be `cmd.exe` or PowerShell, and no one
//! quoting is safe in all of them. The command is fixed and the name goes
//! to `xargs -0` on stdin instead.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use ferossh_ipc::{ChannelId, DirEntry, Message, OpenMode, SftpOp, SftpReply, sftp_code};
use ferossh_sftp::wire::{self, Decoder, Request, Response, pflags};
use russh::{ChannelMsg, Disconnect, Sig};
use tokio::sync::{mpsc, watch};

use crate::session::{Connection, Connections};

/// Waiting for the server's version and home folder.
const START_TIMEOUT: Duration = Duration::from_secs(30);
/// Requests the UI may have in flight.
const MAX_PENDING: usize = 1024;
/// Remote files open at once.
const MAX_HANDLES: usize = 64;
/// Largest read the worker passes on.
const MAX_READ: u32 = 64 * 1024;
/// Names per [`SftpReply::Entries`], well inside a frame.
const ENTRIES_PER_REPLY: usize = 512;
/// How long a remote checksum may take: hashing a large file is slow.
const CHECKSUM_TIMEOUT: Duration = Duration::from_secs(600);
/// Checksums running at once: each holds an exec channel on the server
/// for up to [`CHECKSUM_TIMEOUT`].
const MAX_CHECKSUMS: usize = 4;
/// How long SFTP's end waits for checksums to stop their commands.
const CHECKSUM_STOP: Duration = Duration::from_secs(2);

/// What an SFTP request id is waiting for.
#[derive(Debug)]
enum Pend {
    /// One request, one reply of this kind.
    One { ui: u32, want: Want },
    /// `OPENDIR` for a listing.
    ListOpen { ui: u32 },
    /// `READDIR` for a listing, until end of file.
    ListRead { ui: u32, handle: Vec<u8> },
    /// A plain rename that must replace its target: the target moved
    /// aside first, so a failed promotion can put it back.
    MoveAside {
        ui: u32,
        from: Vec<u8>,
        to: Vec<u8>,
        aside: Vec<u8>,
    },
    /// The new file renamed onto the freed target name.
    Promote {
        ui: u32,
        to: Vec<u8>,
        aside: Vec<u8>,
    },
    /// The original moved back after a failed promotion; the UI hears
    /// the promotion's error once this answers.
    Restore {
        ui: u32,
        aside: Vec<u8>,
        code: u32,
        message: String,
    },
    /// Closing a listing's handle; nobody waits.
    Quiet,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Want {
    Status,
    Attrs,
    Path,
    Handle,
    Data,
}

struct Sftp {
    channel: ChannelId,
    out: mpsc::Sender<Message>,
    write: mpsc::Sender<Vec<u8>>,
    conn: Arc<Connection>,
    posix_rename: bool,
    next_id: u32,
    pending: HashMap<u32, Pend>,
    /// The server's handles by the worker's number.
    handles: HashMap<u32, Vec<u8>>,
    next_handle: u32,
    /// Remote checksums in progress, stopped when SFTP ends.
    checksums: tokio::task::JoinSet<()>,
    /// Set when SFTP ends: each checksum then stops its remote command.
    stop: watch::Sender<bool>,
}

/// Why the SFTP channel ended.
enum End {
    Closed,
    Refused(String),
}

impl Sftp {
    async fn reply(&self, ui: u32, reply: SftpReply) -> Result<(), End> {
        let msg = Message::SftpReply {
            channel: self.channel,
            id: ui,
            reply,
        };
        self.out.send(msg).await.map_err(|_| End::Closed)
    }

    async fn failed(&self, ui: u32, code: u32, message: &str) -> Result<(), End> {
        let reply = SftpReply::Failed {
            code,
            message: message.to_owned(),
        };
        self.reply(ui, reply).await
    }

    fn id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        id
    }

    async fn send(&mut self, req: impl FnOnce(u32) -> Vec<u8>, pend: Pend) -> Result<(), End> {
        let id = self.id();
        self.pending.insert(id, pend);
        self.write.send(req(id)).await.map_err(|_| End::Closed)
    }

    async fn one(
        &mut self,
        ui: u32,
        want: Want,
        req: impl FnOnce(u32) -> Vec<u8>,
    ) -> Result<(), End> {
        self.send(req, Pend::One { ui, want }).await
    }

    /// One operation from the UI.
    async fn request(&mut self, ui: u32, op: SftpOp) -> Result<(), End> {
        if self.pending.len() >= MAX_PENDING {
            return self
                .failed(ui, sftp_code::REFUSED, "Too many requests at once")
                .await;
        }
        match op {
            SftpOp::RealPath { path } => {
                self.one(ui, Want::Path, |id| {
                    wire::encode(&Request::RealPath { id, path: &path })
                })
                .await
            }
            SftpOp::Stat { path, follow } => {
                self.one(ui, Want::Attrs, |id| {
                    wire::encode(&if follow {
                        Request::Stat { id, path: &path }
                    } else {
                        Request::Lstat { id, path: &path }
                    })
                })
                .await
            }
            SftpOp::List { path } => {
                self.send(
                    |id| wire::encode(&Request::OpenDir { id, path: &path }),
                    Pend::ListOpen { ui },
                )
                .await
            }
            SftpOp::Open { path, mode } => {
                if self.handles.len() >= MAX_HANDLES {
                    return self
                        .failed(ui, sftp_code::REFUSED, "Too many files open")
                        .await;
                }
                let flags = match mode {
                    OpenMode::Read => pflags::READ,
                    OpenMode::Write { truncate } => {
                        pflags::WRITE | pflags::CREAT | if truncate { pflags::TRUNC } else { 0 }
                    }
                };
                self.one(ui, Want::Handle, |id| {
                    wire::encode(&Request::Open {
                        id,
                        path: &path,
                        pflags: flags,
                    })
                })
                .await
            }
            SftpOp::Read {
                handle,
                offset,
                len,
            } => {
                let Some(h) = self.handles.get(&handle).cloned() else {
                    return self.failed(ui, sftp_code::REFUSED, "No such handle").await;
                };
                self.one(ui, Want::Data, |id| {
                    wire::encode(&Request::Read {
                        id,
                        handle: &h,
                        offset,
                        len: len.min(MAX_READ),
                    })
                })
                .await
            }
            SftpOp::Write {
                handle,
                offset,
                data,
            } => {
                let Some(h) = self.handles.get(&handle).cloned() else {
                    return self.failed(ui, sftp_code::REFUSED, "No such handle").await;
                };
                self.one(ui, Want::Status, |id| {
                    wire::encode(&Request::Write {
                        id,
                        handle: &h,
                        offset,
                        data: &data,
                    })
                })
                .await
            }
            SftpOp::Close { handle } => {
                let Some(h) = self.handles.remove(&handle) else {
                    return self.failed(ui, sftp_code::REFUSED, "No such handle").await;
                };
                self.one(ui, Want::Status, |id| {
                    wire::encode(&Request::Close { id, handle: &h })
                })
                .await
            }
            SftpOp::Mkdir { path } => {
                self.one(ui, Want::Status, |id| {
                    wire::encode(&Request::Mkdir { id, path: &path })
                })
                .await
            }
            SftpOp::Remove { path } => {
                self.one(ui, Want::Status, |id| {
                    wire::encode(&Request::Remove { id, path: &path })
                })
                .await
            }
            SftpOp::Chmod { path, mode } => {
                self.one(ui, Want::Status, |id| {
                    wire::encode(&Request::Chmod {
                        id,
                        path: &path,
                        mode,
                    })
                })
                .await
            }
            SftpOp::Rmdir { path } => {
                self.one(ui, Want::Status, |id| {
                    wire::encode(&Request::Rmdir { id, path: &path })
                })
                .await
            }
            SftpOp::Rename {
                from,
                to,
                overwrite,
            } => {
                if overwrite && self.posix_rename {
                    self.one(ui, Want::Status, |id| {
                        wire::encode(&Request::PosixRename {
                            id,
                            from: &from,
                            to: &to,
                        })
                    })
                    .await
                } else if overwrite {
                    // Plain SFTP renames refuse to replace. Never remove the
                    // original before the new file is in place: move it
                    // aside, rename, then drop (or restore) the old one.
                    let aside = aside_name(&to, self.next_id);
                    let (t, a) = (to.clone(), aside.clone());
                    self.send(
                        |id| {
                            wire::encode(&Request::Rename {
                                id,
                                from: &t,
                                to: &a,
                            })
                        },
                        Pend::MoveAside {
                            ui,
                            from,
                            to,
                            aside,
                        },
                    )
                    .await
                } else {
                    self.one(ui, Want::Status, |id| {
                        wire::encode(&Request::Rename {
                            id,
                            from: &from,
                            to: &to,
                        })
                    })
                    .await
                }
            }
            SftpOp::Checksum { path } => {
                while self.checksums.try_join_next().is_some() {}
                if self.checksums.len() >= MAX_CHECKSUMS {
                    return self
                        .failed(ui, sftp_code::REFUSED, "Too many checksums at once")
                        .await;
                }
                let conn = self.conn.clone();
                let out = self.out.clone();
                let channel = self.channel;
                let stop = self.stop.subscribe();
                self.checksums.spawn(async move {
                    let reply = checksum(&conn, &path, stop).await;
                    let _ = out
                        .send(Message::SftpReply {
                            channel,
                            id: ui,
                            reply,
                        })
                        .await;
                });
                Ok(())
            }
        }
    }

    /// One packet from the server.
    async fn response(&mut self, resp: Response) -> Result<(), End> {
        let Some(id) = resp.id() else {
            return Err(End::Refused(
                "The server sent a second SFTP version.".into(),
            ));
        };
        // Replies to requests nobody made are dropped.
        let Some(pend) = self.pending.remove(&id) else {
            return Ok(());
        };
        match (pend, resp) {
            (Pend::Quiet, _) => Ok(()),
            (Pend::One { ui, want }, resp) => self.answer(ui, want, resp).await,
            (Pend::ListOpen { ui }, Response::Handle { handle, .. }) => {
                self.read_dir(ui, handle).await
            }
            (Pend::ListRead { ui, handle }, Response::Name { entries, .. }) => {
                for chunk in entries.chunks(ENTRIES_PER_REPLY) {
                    let reply = SftpReply::Entries {
                        entries: chunk.to_vec(),
                        last: false,
                    };
                    self.reply(ui, reply).await?;
                }
                self.read_dir(ui, handle).await
            }
            (Pend::ListRead { ui, handle }, Response::Status { code, message, .. }) => {
                self.send(
                    |id| {
                        wire::encode(&Request::Close {
                            id,
                            handle: &handle,
                        })
                    },
                    Pend::Quiet,
                )
                .await?;
                if code == sftp_code::EOF {
                    let reply = SftpReply::Entries {
                        entries: Vec::<DirEntry>::new(),
                        last: true,
                    };
                    self.reply(ui, reply).await
                } else {
                    self.failed(ui, code, &message).await
                }
            }
            (Pend::ListOpen { ui }, Response::Status { code, message, .. }) => {
                self.failed(ui, code, &message).await
            }
            (
                Pend::MoveAside {
                    ui,
                    from,
                    to,
                    aside,
                },
                Response::Status { code, message, .. },
            ) => {
                let rename =
                    |from: &[u8], to: &[u8], id| wire::encode(&Request::Rename { id, from, to });
                match code {
                    // Nothing to replace: a plain rename does it.
                    sftp_code::NO_SUCH_FILE => {
                        self.one(ui, Want::Status, |id| rename(&from, &to, id))
                            .await
                    }
                    0 => {
                        let pend = Pend::Promote {
                            ui,
                            to: to.clone(),
                            aside,
                        };
                        self.send(|id| rename(&from, &to, id), pend).await
                    }
                    _ => self.failed(ui, code, &message).await,
                }
            }
            (Pend::Promote { ui, to, aside }, Response::Status { code, message, .. }) => {
                if code == 0 {
                    self.send(
                        |id| wire::encode(&Request::Remove { id, path: &aside }),
                        Pend::Quiet,
                    )
                    .await?;
                    self.reply(ui, SftpReply::Done).await
                } else {
                    // Put the original back where it was.
                    let a = aside.clone();
                    self.send(
                        |id| {
                            wire::encode(&Request::Rename {
                                id,
                                from: &a,
                                to: &to,
                            })
                        },
                        Pend::Restore {
                            ui,
                            aside,
                            code,
                            message,
                        },
                    )
                    .await
                }
            }
            (
                Pend::Restore {
                    ui,
                    aside,
                    code,
                    message,
                },
                resp,
            ) => {
                match resp {
                    Response::Status { code: 0, .. } => self.failed(ui, code, &message).await,
                    // A plain code's fixed text would hide where the
                    // original is: send the full words instead.
                    _ => {
                        let message = restore_failed(&message, &aside);
                        self.failed(ui, sftp_code::FAILURE, &message).await
                    }
                }
            }
            (Pend::MoveAside { ui, .. } | Pend::Promote { ui, .. }, _) => {
                self.failed(ui, sftp_code::FAILURE, "Unexpected reply")
                    .await
            }
            (Pend::ListOpen { ui } | Pend::ListRead { ui, .. }, _) => {
                self.failed(ui, sftp_code::FAILURE, "Unexpected reply")
                    .await
            }
        }
    }

    async fn read_dir(&mut self, ui: u32, handle: Vec<u8>) -> Result<(), End> {
        let h = handle.clone();
        self.send(
            |id| wire::encode(&Request::ReadDir { id, handle: &h }),
            Pend::ListRead { ui, handle },
        )
        .await
    }

    async fn answer(&mut self, ui: u32, want: Want, resp: Response) -> Result<(), End> {
        let reply = match (want, resp) {
            (Want::Status, Response::Status { code: 0, .. }) => SftpReply::Done,
            (
                Want::Data,
                Response::Status {
                    code: sftp_code::EOF,
                    ..
                },
            ) => SftpReply::Eof,
            (_, Response::Status { code, message, .. }) => SftpReply::Failed {
                code: if code == 0 { sftp_code::FAILURE } else { code },
                message,
            },
            (Want::Attrs, Response::Attrs { attrs, .. }) => SftpReply::Attrs(attrs),
            (Want::Path, Response::Name { entries, .. }) => match entries.into_iter().next() {
                Some(e) => SftpReply::Path(e.name),
                None => SftpReply::Failed {
                    code: sftp_code::FAILURE,
                    message: "No path".into(),
                },
            },
            (Want::Data, Response::Data { data, .. }) => SftpReply::Data(data),
            (Want::Handle, Response::Handle { handle, .. }) => {
                if self.handles.len() >= MAX_HANDLES {
                    self.send(
                        |id| {
                            wire::encode(&Request::Close {
                                id,
                                handle: &handle,
                            })
                        },
                        Pend::Quiet,
                    )
                    .await?;
                    SftpReply::Failed {
                        code: sftp_code::REFUSED,
                        message: "Too many files open".into(),
                    }
                } else {
                    let ours = self.next_handle;
                    self.next_handle = self.next_handle.wrapping_add(1);
                    self.handles.insert(ours, handle);
                    SftpReply::Opened { handle: ours }
                }
            }
            _ => SftpReply::Failed {
                code: sftp_code::FAILURE,
                message: "Unexpected reply".into(),
            },
        };
        self.reply(ui, reply).await
    }
}

/// The command a remote checksum runs. It holds no part of the file's
/// name, so the login shell has nothing to misread, whichever shell it is.
const CHECKSUM_COMMAND: &[u8] = b"xargs -0 sha256sum -b --";

/// What the checksum command reads on stdin: the name, ended by a NUL.
/// `None` for a name holding a NUL, which no command could be given.
fn checksum_input(path: &[u8]) -> Option<Vec<u8>> {
    if path.is_empty() || path.contains(&0) {
        return None;
    }
    let mut input = path.to_vec();
    input.push(0);
    Some(input)
}

/// The hash `sha256sum` printed: 64 hex digits first on the line, after a
/// `\` when the name needed escaping.
fn parse_sum(output: &[u8]) -> Option<String> {
    let line = output.strip_prefix(b"\\").unwrap_or(output);
    let hex = line.get(..64)?;
    let ok = hex.iter().all(u8::is_ascii_hexdigit) && line.get(64).is_none_or(|b| *b == b' ');
    ok.then(|| String::from_utf8_lossy(hex).to_ascii_lowercase())
}

/// Runs `sha256sum` on the server for `path`.
async fn checksum<H: russh::client::Handler>(
    conn: &russh::client::Handle<H>,
    path: &[u8],
    mut stop: watch::Receiver<bool>,
) -> SftpReply {
    let unsupported = |why: &str| SftpReply::Failed {
        code: sftp_code::UNSUPPORTED,
        message: why.to_owned(),
    };
    let Some(input) = checksum_input(path) else {
        return unsupported("the name cannot be passed to a command");
    };
    let deadline = tokio::time::sleep(CHECKSUM_TIMEOUT);
    tokio::pin!(deadline);
    let mut ch = tokio::select! {
        ch = conn.channel_open_session() => match ch {
            Ok(ch) => ch,
            Err(_) => return unsupported("the server has no xargs or sha256sum"),
        },
        () = &mut deadline => return unsupported("sha256sum took too long"),
        _ = stop.wait_for(|s| *s) => return unsupported("SFTP closed"),
    };
    // From here on the channel is ours to stop on every way out.
    let start = async {
        ch.exec(true, CHECKSUM_COMMAND).await.ok()?;
        ch.data(&input[..]).await.ok()?;
        ch.eof().await.ok()
    };
    let why = tokio::select! {
        started = start => match started {
            Some(()) => None,
            None => Some("the server has no xargs or sha256sum"),
        },
        () = &mut deadline => Some("sha256sum took too long"),
        _ = stop.wait_for(|s| *s) => Some("SFTP closed"),
    };
    if let Some(why) = why {
        stop_remote(&ch).await;
        return unsupported(why);
    }
    let mut output = Vec::new();
    let mut status = None;
    let why = loop {
        tokio::select! {
            msg = ch.wait() => match msg {
                Some(ChannelMsg::Data { data }) => {
                    if output.len() < 4096 {
                        output.extend_from_slice(&data);
                    }
                }
                Some(ChannelMsg::ExitStatus { exit_status }) => status = Some(exit_status),
                Some(ChannelMsg::Close) | None => break None,
                Some(_) => {}
            },
            () = &mut deadline => break Some("sha256sum took too long"),
            _ = stop.wait_for(|s| *s) => break Some("SFTP closed"),
        }
    };
    if let Some(why) = why {
        stop_remote(&ch).await;
        return unsupported(why);
    }
    match status {
        Some(0) => match parse_sum(&output) {
            Some(hex) => SftpReply::Checksum(hex),
            None => unsupported("sha256sum printed something unexpected"),
        },
        _ => unsupported("the server has no xargs or sha256sum"),
    }
}

/// Stops a checksum's command rather than leave it running on the server
/// (OpenSSH 7.9 and later deliver the signal), then closes its channel.
async fn stop_remote(ch: &russh::Channel<russh::client::Msg>) {
    let _ = ch.signal(Sig::TERM).await;
    let _ = ch.close().await;
}

/// Serves SFTP on the connection `connection` made, as channel `channel`,
/// until the UI closes it or the server ends it; then reports why.
pub async fn run(
    channel: ChannelId,
    connection: ChannelId,
    mut inbox: mpsc::Receiver<Message>,
    out: mpsc::Sender<Message>,
    connections: Connections,
) {
    let reason = match connections.get(connection) {
        Some((conn, ..)) => {
            let result = serve(channel, conn.clone(), &mut inbox, out.clone()).await;
            release(conn).await;
            match result {
                End::Closed => "SFTP closed.".to_owned(),
                End::Refused(why) => why,
            }
        }
        None => "That connection is closed; connect again.".to_owned(),
    };
    let _ = out.send(Message::Disconnected { channel, reason }).await;
}

/// Drops a hold on the connection; the last one disconnects it.
pub(crate) async fn release(conn: Arc<Connection>) {
    if let Some(handle) = Arc::into_inner(conn) {
        let _ = handle.disconnect(Disconnect::ByApplication, "", "en").await;
    }
}

async fn serve(
    channel: ChannelId,
    conn: Arc<Connection>,
    inbox: &mut mpsc::Receiver<Message>,
    out: mpsc::Sender<Message>,
) -> End {
    let refused = |why: &str| End::Refused(why.to_owned());
    let Ok(ch) = conn.channel_open_session().await else {
        return refused("The server would not open a channel for SFTP.");
    };
    if ch.request_subsystem(true, "sftp").await.is_err() {
        return refused("The server has no SFTP.");
    }
    let (mut read, write) = ch.split();
    let (write_tx, mut write_rx) = mpsc::channel::<Vec<u8>>(64);
    let writer = tokio::spawn(async move {
        while let Some(packet) = write_rx.recv().await {
            if write.data(&packet[..]).await.is_err() {
                break;
            }
        }
    });
    let mut s = Sftp {
        channel,
        out,
        write: write_tx,
        conn,
        posix_rename: false,
        next_id: 1,
        pending: HashMap::new(),
        handles: HashMap::new(),
        next_handle: 1,
        checksums: tokio::task::JoinSet::new(),
        stop: watch::channel(false).0,
    };
    let mut decoder = Decoder::new();
    let end =
        match tokio::time::timeout(START_TIMEOUT, start(&mut s, &mut read, &mut decoder)).await {
            Ok(Ok(())) => pump(&mut s, &mut read, &mut decoder, inbox).await,
            Ok(Err(end)) => end,
            Err(_) => refused("The server's SFTP did not answer."),
        };
    writer.abort();
    // Checksums stop their remote commands and end; their connection
    // holds go with them, so the caller's release can still be the last
    // one. One that does not end in time is dropped.
    let _ = s.stop.send(true);
    let _ = tokio::time::timeout(CHECKSUM_STOP, async {
        while s.checksums.join_next().await.is_some() {}
    })
    .await;
    s.checksums.shutdown().await;
    end
}

/// A free-looking name beside `to` for the original while it is
/// replaced: same folder, so the rename stays on one file system.
fn aside_name(to: &[u8], n: u32) -> Vec<u8> {
    let mut name = to.to_vec();
    name.extend_from_slice(format!(".ferossh-old-{n}").as_bytes());
    name
}

/// The error when the original could not be moved back: say where
/// it is, so nothing is lost without a word.
fn restore_failed(message: &str, aside: &[u8]) -> String {
    let why = message.trim().trim_end_matches('.');
    let why = if why.is_empty() {
        "The new copy could not replace the file"
    } else {
        why
    };
    format!(
        "{why}; the original file is now {}",
        String::from_utf8_lossy(aside)
    )
}

/// The next packet from the server.
async fn next_response(
    read: &mut russh::ChannelReadHalf,
    decoder: &mut Decoder,
) -> Result<Response, End> {
    loop {
        match decoder.next_response() {
            Ok(Some(r)) => return Ok(r),
            Ok(None) => {}
            Err(e) => return Err(End::Refused(format!("The server sent {e}."))),
        }
        match read.wait().await {
            Some(ChannelMsg::Data { data }) => decoder.feed(&data),
            Some(ChannelMsg::Failure) => {
                return Err(End::Refused("The server has no SFTP.".into()));
            }
            Some(ChannelMsg::Close | ChannelMsg::Eof) | None => return Err(End::Closed),
            Some(_) => {}
        }
    }
}

/// Version exchange, then the home folder for [`Message::SftpReady`].
async fn start(
    s: &mut Sftp,
    read: &mut russh::ChannelReadHalf,
    decoder: &mut Decoder,
) -> Result<(), End> {
    s.write
        .send(wire::encode(&Request::Init))
        .await
        .map_err(|_| End::Closed)?;
    match next_response(read, decoder).await? {
        Response::Version {
            version,
            extensions,
        } if version >= wire::VERSION => {
            s.posix_rename = extensions.iter().any(|e| e == wire::POSIX_RENAME);
        }
        Response::Version { version, .. } => {
            return Err(End::Refused(format!(
                "The server speaks SFTP version {version}; FeroSSH needs 3."
            )));
        }
        _ => return Err(End::Refused("The server's SFTP did not say hello.".into())),
    }
    let id = s.id();
    s.write
        .send(wire::encode(&Request::RealPath { id, path: b"." }))
        .await
        .map_err(|_| End::Closed)?;
    let home = loop {
        match next_response(read, decoder).await? {
            Response::Name { id: got, entries } if got == id => {
                break entries
                    .into_iter()
                    .next()
                    .map(|e| e.name)
                    .unwrap_or_else(|| b"/".to_vec());
            }
            Response::Status { id: got, .. } if got == id => break b"/".to_vec(),
            _ => {}
        }
    };
    let ready = Message::SftpReady {
        channel: s.channel,
        home,
    };
    s.out.send(ready).await.map_err(|_| End::Closed)
}

async fn pump(
    s: &mut Sftp,
    read: &mut russh::ChannelReadHalf,
    decoder: &mut Decoder,
    inbox: &mut mpsc::Receiver<Message>,
) -> End {
    loop {
        let step = tokio::select! {
            resp = next_response(read, decoder) => match resp {
                Ok(r) => s.response(r).await,
                Err(end) => Err(end),
            },
            msg = inbox.recv() => match msg {
                Some(Message::SftpRequest { id, op, .. }) => s.request(id, op).await,
                None | Some(Message::Close { .. }) => Err(End::Closed),
                Some(_) => Ok(()),
            },
        };
        if let Err(end) = step {
            return end;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trust only the deterministic host key of the in-process checksum server.
    struct ChecksumClient;

    impl russh::client::Handler for ChecksumClient {
        type Error = russh::Error;

        /// Accept only the fixture key, so this client cannot trust an unrelated server.
        async fn check_server_key(
            &mut self,
            key: &russh::keys::PublicKeyOrCertificate,
        ) -> Result<bool, Self::Error> {
            use russh::keys::ssh_key::{PrivateKey, private::Ed25519Keypair};
            let expected = PrivateKey::from(Ed25519Keypair::from_seed(&[3; 32]));
            Ok(
                matches!(key, russh::keys::PublicKeyOrCertificate::PublicKey { key, .. }
                if key == expected.public_key()),
            )
        }
    }

    /// Capture exec and stdin separately, replying only after the client sends EOF.
    /// A name of `hang` gets no reply at all, like a command that never ends.
    struct ChecksumServer {
        command: Vec<u8>,
        input: Vec<u8>,
        received: mpsc::UnboundedSender<(Vec<u8>, Vec<u8>)>,
        /// What the client did to a hung command: `TERM` and `close`.
        cleanup: mpsc::UnboundedSender<&'static str>,
    }

    impl russh::server::Handler for ChecksumServer {
        type Error = russh::Error;

        /// Allow the in-memory client to authenticate without external credentials.
        async fn auth_none(&mut self, _: &str) -> Result<russh::server::Auth, Self::Error> {
            Ok(russh::server::Auth::Accept)
        }

        /// Accept each checksum channel opened by the production client.
        async fn channel_open_session(
            &mut self,
            _: russh::Channel<russh::server::Msg>,
            reply: russh::server::ChannelOpenHandle,
            _: &mut russh::server::Session,
        ) -> Result<(), Self::Error> {
            reply.accept().await;
            Ok(())
        }

        /// Record the exact command independently of its stdin and acknowledge exec.
        async fn exec_request(
            &mut self,
            channel: russh::ChannelId,
            command: &[u8],
            session: &mut russh::server::Session,
        ) -> Result<(), Self::Error> {
            self.command = command.to_vec();
            session.channel_success(channel)
        }

        /// Accumulate stdin across packet boundaries without decoding filename bytes.
        async fn data(
            &mut self,
            _: russh::ChannelId,
            data: &[u8],
            _: &mut russh::server::Session,
        ) -> Result<(), Self::Error> {
            self.input.extend_from_slice(data);
            Ok(())
        }

        /// Publish the complete request on EOF and simulate success or a missing tool.
        async fn channel_eof(
            &mut self,
            channel: russh::ChannelId,
            session: &mut russh::server::Session,
        ) -> Result<(), Self::Error> {
            let failed = self.input == b"missing-tool\0";
            let hang = self.input == b"hang\0";
            self.received
                .send((
                    std::mem::take(&mut self.command),
                    std::mem::take(&mut self.input),
                ))
                .map_err(|_| russh::Error::Inconsistent)?;
            if hang {
                return Ok(());
            }
            // Even plausible hash output must not override an unsuccessful exit.
            session.data(channel, format!("{} *file\n", "a".repeat(64)).into_bytes())?;
            session.exit_status_request(channel, if failed { 127 } else { 0 })?;
            session.eof(channel)?;
            session.close(channel)
        }

        /// Report the signal a stopped checksum sends.
        async fn signal(
            &mut self,
            _: russh::ChannelId,
            signal: Sig,
            _: &mut russh::server::Session,
        ) -> Result<(), Self::Error> {
            if matches!(signal, Sig::TERM) {
                let _ = self.cleanup.send("TERM");
            }
            Ok(())
        }

        /// Report the client closing a channel.
        async fn channel_close(
            &mut self,
            _: russh::ChannelId,
            _: &mut russh::server::Session,
        ) -> Result<(), Self::Error> {
            let _ = self.cleanup.send("close");
            Ok(())
        }
    }

    /// The fixture server's captures and cleanup events.
    type Fixture = (
        mpsc::UnboundedReceiver<(Vec<u8>, Vec<u8>)>,
        mpsc::UnboundedReceiver<&'static str>,
    );

    /// Connects a client to an in-process [`ChecksumServer`].
    async fn checksum_fixture() -> Result<
        (
            russh::client::Handle<ChecksumClient>,
            russh::server::RunningSession<ChecksumServer>,
            Fixture,
        ),
        Box<dyn std::error::Error>,
    > {
        use russh::keys::ssh_key::{PrivateKey, private::Ed25519Keypair};

        let (client_stream, server_stream) = tokio::io::duplex(64 * 1024);
        let mut config = russh::server::Config::default();
        config
            .keys
            .push(PrivateKey::from(Ed25519Keypair::from_seed(&[3; 32])));
        let (received, captures) = mpsc::unbounded_channel();
        let (cleanup, cleanups) = mpsc::unbounded_channel();
        let server = tokio::spawn(async move {
            russh::server::run_stream(
                Arc::new(config),
                server_stream,
                ChecksumServer {
                    command: Vec::new(),
                    input: Vec::new(),
                    received,
                    cleanup,
                },
            )
            .await
        });
        let mut conn = russh::client::connect_stream(
            Arc::new(russh::client::Config::default()),
            client_stream,
            ChecksumClient,
        )
        .await?;
        let running = server.await??;
        assert!(conn.authenticate_none("checksum-test").await?.success());
        Ok((conn, running, (captures, cleanups)))
    }

    /// Exercise the production checksum path over SSH without shells or external tools.
    #[tokio::test]
    async fn checksum_names_are_data_not_shell_syntax() -> Result<(), Box<dyn std::error::Error>> {
        tokio::time::timeout(Duration::from_secs(10), async {
            let (conn, running, (mut captures, _)) = checksum_fixture().await?;
            let (_stop_tx, stop) = watch::channel(false);
            for path in [
                b"a&calc&rem".as_slice(),
                b"$(touch injected)",
                b"`touch injected`",
                b"'; touch injected; #",
                b"a'$(touch injected)'",
                b"spaces tabs\tnewlines\nbackslash\\",
                b"-leading-dash",
                b"non-utf8-\xff",
                b"missing-tool",
            ] {
                let reply = checksum(&conn, path, stop.clone()).await;
                if path == b"missing-tool" {
                    assert!(matches!(
                        reply,
                        SftpReply::Failed {
                            code: sftp_code::UNSUPPORTED,
                            ..
                        }
                    ));
                } else {
                    assert!(matches!(reply, SftpReply::Checksum(sum) if sum == "a".repeat(64)));
                }
                let (command, input) = captures.recv().await.ok_or("missing checksum capture")?;
                assert_eq!(command, b"xargs -0 sha256sum -b --");
                let mut expected = path.to_vec();
                expected.push(0);
                assert_eq!(input, expected);
            }
            assert!(matches!(
                checksum(&conn, b"bad\0path", stop.clone()).await,
                SftpReply::Failed {
                    code: sftp_code::UNSUPPORTED,
                    ..
                }
            ));
            assert!(captures.try_recv().is_err());
            conn.disconnect(Disconnect::ByApplication, "test complete", "")
                .await?;
            running.await?;
            Ok::<_, Box<dyn std::error::Error>>(())
        })
        .await??;
        Ok(())
    }

    /// A checksum stopped while its command hangs answers at once, and
    /// signals and closes the remote command rather than leaving it running.
    #[tokio::test]
    async fn a_stopped_checksum_ends_its_command() -> Result<(), Box<dyn std::error::Error>> {
        tokio::time::timeout(Duration::from_secs(10), async {
            let (conn, running, (mut captures, mut cleanups)) = checksum_fixture().await?;
            let conn = Arc::new(conn);
            let (stop_tx, stop) = watch::channel(false);
            let job = tokio::spawn({
                let conn = conn.clone();
                async move { checksum(&conn, b"hang", stop).await }
            });
            // The server has the request and is holding its reply.
            captures.recv().await.ok_or("missing checksum capture")?;
            stop_tx.send(true)?;
            assert!(matches!(
                job.await?,
                SftpReply::Failed {
                    code: sftp_code::UNSUPPORTED,
                    ..
                }
            ));
            assert_eq!(cleanups.recv().await, Some("TERM"));
            assert_eq!(cleanups.recv().await, Some("close"));
            conn.disconnect(Disconnect::ByApplication, "test complete", "")
                .await?;
            running.await?;
            Ok::<_, Box<dyn std::error::Error>>(())
        })
        .await??;
        Ok(())
    }

    #[test]
    fn the_original_waits_beside_its_target() {
        assert_eq!(
            aside_name(b"/etc/app.conf", 7),
            b"/etc/app.conf.ferossh-old-7"
        );
    }

    #[test]
    fn keeps_the_name_off_the_command_line() {
        // Only plain words: nothing cmd.exe, PowerShell or sh would
        // treat specially, and no name.
        assert!(
            CHECKSUM_COMMAND
                .iter()
                .all(|b| b.is_ascii_alphanumeric() || b" -".contains(b))
        );
        for name in [
            &b"a&calc&rem"[..],
            b"it's",
            b"$(rm -rf ~)",
            b"x`whoami`;y|z\"\nw",
            b"-rf",
        ] {
            let mut want = name.to_vec();
            want.push(0);
            assert_eq!(checksum_input(name), Some(want));
        }
        assert_eq!(checksum_input(b"a\0b"), None);
        assert_eq!(checksum_input(b""), None);
    }

    #[test]
    fn reads_sha256sum_output() {
        let hex = "a".repeat(64);
        assert_eq!(
            parse_sum(format!("{hex} *file\n").as_bytes()),
            Some(hex.clone())
        );
        assert_eq!(
            parse_sum(format!("\\{} *a\\nb\n", hex.to_uppercase()).as_bytes()),
            Some(hex)
        );
        assert_eq!(parse_sum(b"sha256sum: not found"), None);
        assert_eq!(parse_sum(format!("{}g x", "a".repeat(63)).as_bytes()), None);
    }
}
