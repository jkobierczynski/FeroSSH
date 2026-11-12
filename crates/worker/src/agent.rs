//! Agent forwarding: the server's end of a forwarded agent.
//!
//! With `ForwardAgent`, the shell asks the server for agent forwarding
//! (`auth-agent-req@openssh.com`), and programs on the server (`ssh`,
//! `git`) then reach the user's agent through `auth-agent@openssh.com`
//! channels the server opens back. The worker cannot reach the agent and
//! must not see more of it than it needs, so it reads the agent protocol
//! here, in the sandbox, and passes on only two requests: the list of keys
//! and a signature, each as a message the UI answers (where the user
//! confirms every signature). Everything else (adding, removing or
//! locking keys, extensions) gets `SSH_AGENT_FAILURE` without the UI
//! hearing of it.

use ferossh_ipc::{ChannelId, Message};
use russh::client::Msg;
use russh::{Channel, ChannelMsg};
use tokio::sync::mpsc;

use crate::session::{Connections, Route};

/// Largest agent message taken: OpenSSH's agent allows 256 KiB.
pub(crate) const MAX_MESSAGE: usize = 256 * 1024;

const FAILURE: u8 = 5;
const REQUEST_IDENTITIES: u8 = 11;
const IDENTITIES_ANSWER: u8 = 12;
const SIGN_REQUEST: u8 = 13;
const SIGN_RESPONSE: u8 = 14;

/// One request from the server's side.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Request {
    List,
    Sign {
        key: Vec<u8>,
        data: Vec<u8>,
        flags: u32,
    },
    /// Anything else, answered with a failure.
    Other,
}

fn string(rest: &mut &[u8]) -> Option<Vec<u8>> {
    let (len, tail) = rest.split_first_chunk::<4>()?;
    let len = u32::from_be_bytes(*len) as usize;
    let s = tail.get(..len)?.to_vec();
    *rest = tail.get(len..)?;
    Some(s)
}

/// Reads one message body (without its length). A malformed sign request
/// is [`Request::Other`]: it fails, and the channel goes on.
pub(crate) fn parse(body: &[u8]) -> Request {
    match body.split_first() {
        Some((&REQUEST_IDENTITIES, [])) => Request::List,
        Some((&SIGN_REQUEST, mut rest)) => {
            let (Some(key), Some(data)) = (string(&mut rest), string(&mut rest)) else {
                return Request::Other;
            };
            match rest.split_first_chunk::<4>() {
                Some((flags, [])) => Request::Sign {
                    key,
                    data,
                    flags: u32::from_be_bytes(*flags),
                },
                _ => Request::Other,
            }
        }
        _ => Request::Other,
    }
}

fn put_string(out: &mut Vec<u8>, s: &[u8]) {
    out.extend_from_slice(&u32::try_from(s.len()).unwrap_or(0).to_be_bytes());
    out.extend_from_slice(s);
}

/// A whole message with its length in front.
fn frame(body: Vec<u8>) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 4);
    put_string(&mut out, &body);
    out
}

pub(crate) fn failure() -> Vec<u8> {
    frame(vec![FAILURE])
}

/// `SSH2_AGENT_IDENTITIES_ANSWER`.
pub(crate) fn identities(keys: &[(Vec<u8>, String)]) -> Vec<u8> {
    let mut body = vec![IDENTITIES_ANSWER];
    body.extend_from_slice(&u32::try_from(keys.len()).unwrap_or(0).to_be_bytes());
    for (key, comment) in keys {
        put_string(&mut body, key);
        put_string(&mut body, comment.as_bytes());
    }
    frame(body)
}

/// `SSH2_AGENT_SIGN_RESPONSE`, or a failure for an empty signature.
pub(crate) fn signed(signature: &[u8]) -> Vec<u8> {
    if signature.is_empty() {
        return failure();
    }
    let mut body = vec![SIGN_RESPONSE];
    put_string(&mut body, signature);
    frame(body)
}

/// Splits whole messages off the front of `buf`. `Err` when one claims
/// to be larger than [`MAX_MESSAGE`].
pub(crate) fn next_message(buf: &mut Vec<u8>) -> Result<Option<Vec<u8>>, ()> {
    let Some((len, _)) = buf.split_first_chunk::<4>() else {
        return Ok(None);
    };
    let len = u32::from_be_bytes(*len) as usize;
    if len > MAX_MESSAGE {
        return Err(());
    }
    if buf.len() < 4 + len {
        return Ok(None);
    }
    let body = buf.get(4..4 + len).unwrap_or_default().to_vec();
    buf.drain(..4 + len);
    Ok(Some(body))
}

/// Serves one agent channel the server opened on connection `connection`
/// until either side closes it: each request in turn, asking the UI for
/// the two it answers.
pub(crate) async fn serve(
    channel: Channel<Msg>,
    connection: ChannelId,
    out: mpsc::Sender<Message>,
    connections: Connections,
) {
    let id = connections.new_id();
    let (tx, mut inbox) = mpsc::unbounded_channel();
    if !connections.routes.insert(id, Route::Stream(tx)) {
        let _ = channel.close().await;
        return;
    }
    let mut buf: Vec<u8> = Vec::new();
    let mut channel = channel;
    'read: loop {
        let Some(msg) = channel.wait().await else {
            break;
        };
        match msg {
            ChannelMsg::Data { data } => {
                buf.extend_from_slice(&data);
                if buf.len() > MAX_MESSAGE + 4 {
                    break;
                }
            }
            ChannelMsg::Eof | ChannelMsg::Close => break,
            _ => continue,
        }
        loop {
            let body = match next_message(&mut buf) {
                Ok(Some(body)) => body,
                Ok(None) => break,
                Err(()) => break 'read,
            };
            let ask = match parse(&body) {
                Request::List => Message::AgentList {
                    channel: connection,
                    agent: id,
                },
                Request::Sign { key, data, flags } => Message::AgentSign {
                    channel: connection,
                    agent: id,
                    key,
                    data,
                    flags,
                },
                Request::Other => {
                    if channel.data(&failure()[..]).await.is_err() {
                        break 'read;
                    }
                    continue;
                }
            };
            if out.send(ask).await.is_err() {
                break 'read;
            }
            // One request at a time: the UI's answer (the user may take
            // a while to confirm), or the end of the channel meanwhile.
            let reply = loop {
                tokio::select! {
                    msg = inbox.recv() => match msg {
                        Some(Message::AgentKeys { keys, .. }) => break identities(&keys),
                        Some(Message::AgentSigned { signature, .. }) => break signed(&signature),
                        Some(Message::Close { .. }) | None => break 'read,
                        Some(_) => {}
                    },
                    msg = channel.wait() => match msg {
                        Some(ChannelMsg::Data { data }) => {
                            buf.extend_from_slice(&data);
                            if buf.len() > MAX_MESSAGE + 4 {
                                break 'read;
                            }
                        }
                        Some(ChannelMsg::Eof | ChannelMsg::Close) | None => break 'read,
                        Some(_) => {}
                    },
                }
            };
            if channel.data(&reply[..]).await.is_err() {
                break 'read;
            }
        }
    }
    connections.routes.remove(id);
    let _ = channel.close().await;
}

/// What [`serve`] makes of a server's bytes on an agent channel, for the
/// fuzzer: the bytes arrive `step` at a time and are read as `serve`
/// reads them (whole messages off the front, the same size limit). The
/// requests must be the same as from the bytes whole, a sign request
/// must be exactly its parts again, and the buffer never holds more than
/// one message over the limit.
pub fn fuzz_stream(data: &[u8], step: usize) {
    fn requests(chunks: &mut dyn Iterator<Item = &[u8]>) -> Vec<Vec<u8>> {
        let mut buf = Vec::new();
        let mut out = Vec::new();
        for chunk in chunks {
            buf.extend_from_slice(chunk);
            if buf.len() > MAX_MESSAGE + 4 {
                break;
            }
            loop {
                match next_message(&mut buf) {
                    Ok(Some(body)) => {
                        assert!(body.len() <= MAX_MESSAGE);
                        out.push(body);
                    }
                    Ok(None) => break,
                    Err(()) => return out,
                }
            }
            assert!(buf.len() <= MAX_MESSAGE + 4);
        }
        out
    }
    let whole = requests(&mut std::iter::once(data));
    let chunked = requests(&mut data.chunks(step.max(1)));
    assert_eq!(whole, chunked);
    for body in &whole {
        match parse(body) {
            Request::List => assert_eq!(body.as_slice(), [REQUEST_IDENTITIES]),
            Request::Sign { key, data, flags } => {
                let mut again = vec![SIGN_REQUEST];
                put_string(&mut again, &key);
                put_string(&mut again, &data);
                again.extend_from_slice(&flags.to_be_bytes());
                assert_eq!(&again, body);
            }
            Request::Other => {}
        }
    }
    // The answers the worker writes back stay well formed.
    let keys: Vec<(Vec<u8>, String)> = whole
        .iter()
        .take(4)
        .map(|b| (b.clone(), String::from_utf8_lossy(b).into_owned()))
        .collect();
    let mut answer = identities(&keys);
    assert_eq!(
        next_message(&mut answer)
            .ok()
            .flatten()
            .map(|b| b.first().copied()),
        Some(Some(IDENTITIES_ANSWER))
    );
    assert!(answer.is_empty());
    let mut answer = signed(data);
    assert!(matches!(next_message(&mut answer), Ok(Some(_)) | Err(())));
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn sign_body(key: &[u8], data: &[u8], flags: u32) -> Vec<u8> {
        let mut b = vec![SIGN_REQUEST];
        put_string(&mut b, key);
        put_string(&mut b, data);
        b.extend_from_slice(&flags.to_be_bytes());
        b
    }

    #[test]
    fn reads_the_two_requests_it_passes_on() {
        assert_eq!(parse(&[REQUEST_IDENTITIES]), Request::List);
        assert_eq!(
            parse(&sign_body(b"key", b"data", 4)),
            Request::Sign {
                key: b"key".to_vec(),
                data: b"data".to_vec(),
                flags: 4
            }
        );
    }

    #[test]
    fn everything_else_fails() {
        // Add identity, remove all, lock, extension, nothing at all.
        for body in [&[17u8][..], &[19], &[22, 0, 0, 0, 0], &[27], &[]] {
            assert_eq!(parse(body), Request::Other);
        }
        // A list request with a tail, a sign request cut short or too long.
        assert_eq!(parse(&[REQUEST_IDENTITIES, 0]), Request::Other);
        let full = sign_body(b"k", b"d", 0);
        for cut in 1..full.len() {
            assert_eq!(parse(&full[..cut]), Request::Other);
        }
        let mut long = full.clone();
        long.push(0);
        assert_eq!(parse(&long), Request::Other);
        // A length past the end.
        assert_eq!(
            parse(&[SIGN_REQUEST, 0xff, 0xff, 0xff, 0xff]),
            Request::Other
        );
    }

    #[test]
    fn frames_messages() {
        let mut buf = frame(vec![REQUEST_IDENTITIES]);
        buf.extend_from_slice(&frame(sign_body(b"k", b"d", 2)));
        buf.extend_from_slice(&[0, 0]);
        assert_eq!(next_message(&mut buf), Ok(Some(vec![REQUEST_IDENTITIES])));
        assert_eq!(next_message(&mut buf), Ok(Some(sign_body(b"k", b"d", 2))));
        assert_eq!(next_message(&mut buf), Ok(None));
        let mut huge = ((MAX_MESSAGE + 1) as u32).to_be_bytes().to_vec();
        assert_eq!(next_message(&mut huge), Err(()));
    }

    #[test]
    fn answers() {
        assert_eq!(failure(), vec![0, 0, 0, 1, FAILURE]);
        assert_eq!(signed(&[]), failure());
        assert_eq!(
            signed(b"sig"),
            vec![0, 0, 0, 8, SIGN_RESPONSE, 0, 0, 0, 3, b's', b'i', b'g']
        );
        let keys = vec![(b"kk".to_vec(), "c".to_owned())];
        assert_eq!(
            identities(&keys),
            vec![
                0,
                0,
                0,
                16,
                IDENTITIES_ANSWER,
                0,
                0,
                0,
                1,
                0,
                0,
                0,
                2,
                b'k',
                b'k',
                0,
                0,
                0,
                1,
                b'c'
            ]
        );
    }
}
