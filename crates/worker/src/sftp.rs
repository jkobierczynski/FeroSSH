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
//! connection; that is the one thing SFTP itself cannot do.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use ferossh_ipc::{ChannelId, DirEntry, Message, OpenMode, SftpOp, SftpReply, sftp_code};
use ferossh_sftp::wire::{self, Decoder, Request, Response, pflags};
use russh::{ChannelMsg, Disconnect};
use tokio::sync::mpsc;

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

/// What an SFTP request id is waiting for.
#[derive(Debug)]
enum Pend {
    /// One request, one reply of this kind.
    One { ui: u32, want: Want },
    /// `OPENDIR` for a listing.
    ListOpen { ui: u32 },
    /// `READDIR` for a listing, until end of file.
    ListRead { ui: u32, handle: Vec<u8> },
    /// Removing the target before a plain rename that must replace it.
    RemoveThenRename { ui: u32, from: Vec<u8>, to: Vec<u8> },
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
                    // Plain SFTP renames refuse to replace: remove first.
                    let path = to.clone();
                    self.send(
                        |id| wire::encode(&Request::Remove { id, path: &path }),
                        Pend::RemoveThenRename { ui, from, to },
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
                let conn = self.conn.clone();
                let out = self.out.clone();
                let channel = self.channel;
                tokio::spawn(async move {
                    let reply = checksum(&conn, &path).await;
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
            (Pend::RemoveThenRename { ui, from, to }, _) => {
                self.one(ui, Want::Status, |id| {
                    wire::encode(&Request::Rename {
                        id,
                        from: &from,
                        to: &to,
                    })
                })
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

/// Quotes `arg` for a POSIX shell: inside single quotes, with each `'`
/// written as `'\''`.
fn shell_quote(arg: &[u8]) -> Vec<u8> {
    let mut q = vec![b'\''];
    for &b in arg {
        if b == b'\'' {
            q.extend_from_slice(b"'\\''");
        } else {
            q.push(b);
        }
    }
    q.push(b'\'');
    q
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
async fn checksum(conn: &Connection, path: &[u8]) -> SftpReply {
    let unsupported = |why: &str| SftpReply::Failed {
        code: sftp_code::UNSUPPORTED,
        message: why.to_owned(),
    };
    if path.contains(&0) {
        return unsupported("the name cannot be passed to a command");
    }
    let mut command = b"sha256sum -b -- ".to_vec();
    command.extend(shell_quote(path));
    let run = async {
        let mut ch = conn.channel_open_session().await.ok()?;
        ch.exec(true, command).await.ok()?;
        let mut output = Vec::new();
        let mut status = None;
        while let Some(msg) = ch.wait().await {
            match msg {
                ChannelMsg::Data { data } => {
                    if output.len() < 4096 {
                        output.extend_from_slice(&data);
                    }
                }
                ChannelMsg::ExitStatus { exit_status } => status = Some(exit_status),
                ChannelMsg::Close => break,
                _ => {}
            }
        }
        Some((status, output))
    };
    match tokio::time::timeout(CHECKSUM_TIMEOUT, run).await {
        Ok(Some((Some(0), output))) => match parse_sum(&output) {
            Some(hex) => SftpReply::Checksum(hex),
            None => unsupported("sha256sum printed something unexpected"),
        },
        Ok(_) => unsupported("the server has no sha256sum"),
        Err(_) => unsupported("sha256sum took too long"),
    }
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
    };
    let mut decoder = Decoder::new();
    let end =
        match tokio::time::timeout(START_TIMEOUT, start(&mut s, &mut read, &mut decoder)).await {
            Ok(Ok(())) => pump(&mut s, &mut read, &mut decoder, inbox).await,
            Ok(Err(end)) => end,
            Err(_) => refused("The server's SFTP did not answer."),
        };
    writer.abort();
    end
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

    #[test]
    fn quotes_for_the_shell() {
        assert_eq!(shell_quote(b"a b"), b"'a b'");
        assert_eq!(shell_quote(b"it's"), b"'it'\\''s'");
        assert_eq!(shell_quote(b"$(rm -rf ~)"), b"'$(rm -rf ~)'");
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
