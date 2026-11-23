//! Closes a session channel on every way out.
//!
//! russh sends no `SSH_MSG_CHANNEL_CLOSE` when a [`russh::Channel`] or its
//! halves are dropped: the server keeps the session (and the shell, SFTP
//! server or command behind it) until the whole connection ends. On a
//! connection that tabs, panes and File transfer share, every channel
//! dropped that way counts against sshd's `MaxSessions` (10 by default),
//! until the server refuses new ones as "administratively prohibited".

use std::ops::Deref;
use std::sync::Arc;

use russh::ChannelWriteHalf;
use russh::client::Msg;

/// A channel's writing half that closes the channel when dropped. Closing
/// one the server already closed sends nothing.
#[derive(Debug)]
pub(crate) struct CloseOnDrop(Arc<ChannelWriteHalf<Msg>>);

impl CloseOnDrop {
    pub(crate) fn new(write: ChannelWriteHalf<Msg>) -> Self {
        Self(Arc::new(write))
    }

    /// Another handle for writing, for a task of its own; the channel
    /// still closes when this guard is dropped.
    pub(crate) fn share(&self) -> Arc<ChannelWriteHalf<Msg>> {
        Arc::clone(&self.0)
    }
}

impl Deref for CloseOnDrop {
    type Target = ChannelWriteHalf<Msg>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Drop for CloseOnDrop {
    fn drop(&mut self) {
        let write = Arc::clone(&self.0);
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(async move {
                let _ = write.close().await;
            });
        }
    }
}
