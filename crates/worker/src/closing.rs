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

/// How long a session open the tab no longer wants is still waited for,
/// so that a channel the server confirms late is closed, not left open.
pub(crate) const LATE_OPEN: std::time::Duration = std::time::Duration::from_secs(10);

/// Sees an open the caller gave up on (the tab closed while the server
/// had not answered yet) through: a channel the server confirms within
/// [`LATE_OPEN`] is closed again at once, so it does not hold one of the
/// connection's sessions (sshd `MaxSessions`) until the connection ends.
pub(crate) async fn close_late<F>(open: F)
where
    F: std::future::Future<Output = Result<russh::Channel<Msg>, russh::Error>>,
{
    if let Ok(Ok(ch)) = tokio::time::timeout(LATE_OPEN, open).await {
        let _ = ch.close().await;
    }
}
