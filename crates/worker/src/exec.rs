//! A command on an exec channel of a logged-in connection, without a
//! terminal: Open virtual desktop's script, which starts Xvfb and x11vnc
//! on the server and stops them when its standard input ends.
//!
//! The UI's [`Message::Data`] goes to the command's standard input and its
//! [`Message::Eof`] ends that input; the command's output (standard error
//! as well, unless asked to keep it apart as [`Message::ExecStderr`])
//! comes back as [`Message::Data`]. Closing the channel, or the
//! connection breaking, ends the input too, which is how the script knows
//! to clean up.

use std::sync::Arc;

use ferossh_ipc::{ChannelId, MAX_DATA_CHUNK, Message};
use russh::ChannelMsg;
use tokio::sync::mpsc;

use crate::session::{Connection, Connections};
use crate::sftp::release;

/// Runs `command` on the connection `connection` made, as channel
/// `channel`, until it ends or the UI closes it; then reports why.
pub async fn run(
    channel: ChannelId,
    connection: ChannelId,
    command: Vec<u8>,
    split_stderr: bool,
    mut inbox: mpsc::Receiver<Message>,
    out: mpsc::Sender<Message>,
    connections: Connections,
) {
    let reason = match connections.get(connection) {
        Some((conn, ..)) => {
            let reason = serve(channel, &conn, command, split_stderr, &mut inbox, &out).await;
            release(conn).await;
            reason
        }
        None => "That connection is closed; connect again.".to_owned(),
    };
    let _ = out.send(Message::Disconnected { channel, reason }).await;
}

async fn serve(
    channel: ChannelId,
    conn: &Arc<Connection>,
    command: Vec<u8>,
    split_stderr: bool,
    inbox: &mut mpsc::Receiver<Message>,
    out: &mpsc::Sender<Message>,
) -> String {
    let Ok(ch) = conn.channel_open_session().await else {
        return "The server would not open a channel for the command.".into();
    };
    if ch.exec(true, command).await.is_err() {
        return "The server would not run the command.".into();
    }
    let (mut read, write) = ch.split();
    let send = |bytes: &[u8], stderr: bool| {
        let msgs: Vec<Message> = bytes
            .chunks(MAX_DATA_CHUNK)
            .map(|c| {
                if stderr {
                    Message::ExecStderr {
                        channel,
                        bytes: c.to_vec(),
                    }
                } else {
                    Message::Data {
                        channel,
                        bytes: c.to_vec(),
                    }
                }
            })
            .collect();
        async move {
            for m in msgs {
                if out.send(m).await.is_err() {
                    return false;
                }
            }
            true
        }
    };
    loop {
        tokio::select! {
            msg = read.wait() => match msg {
                Some(ChannelMsg::Data { data }) => {
                    if !send(&data, false).await {
                        break;
                    }
                }
                Some(ChannelMsg::ExtendedData { data, .. }) => {
                    if !send(&data, split_stderr).await {
                        break;
                    }
                }
                Some(ChannelMsg::ExitStatus { exit_status }) => {
                    let _ = out.send(Message::ExitStatus { channel, code: exit_status }).await;
                }
                Some(ChannelMsg::Failure) => {
                    return "The server would not run the command.".into();
                }
                Some(ChannelMsg::Close) | None => break,
                Some(_) => {}
            },
            msg = inbox.recv() => match msg {
                Some(Message::Data { bytes, .. }) => {
                    let _ = write.data(&bytes[..]).await;
                }
                Some(Message::Eof { .. }) => {
                    let _ = write.eof().await;
                }
                None | Some(Message::Close { .. }) => {
                    // The end of its input tells the command to stop.
                    let _ = write.eof().await;
                    let _ = write.close().await;
                    return "Closed.".into();
                }
                Some(_) => {}
            },
        }
    }
    "The command ended.".into()
}
