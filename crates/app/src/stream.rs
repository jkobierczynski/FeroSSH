//! Remote apps (FeroStream): the decoder process and the pipe to it.
//!
//! Everything a remote app's host sends (the agent-upload request, then
//! FeroStream frames with LZ4-packed pixels) is taken apart by `ferossh.exe
//! --stream`, in an AppContainer of its own with no capabilities at all
//! (see `sandbox`): no network, files, registry, clipboard or windows. It
//! hands back only plain data, which the window side checks again before
//! anything reaches the screen:
//!
//! - a FeroStream message, re-encoded, whose tiles are raw pixels (a packed
//!   tile here means the decoder was taken over: the session ends);
//! - the host's request for the agent for one machine kind;
//! - why the stream broke.
//!
//! Window to decoder: frames of host bytes. Both directions: a
//! little-endian `u32` length, then that many bytes; from the decoder the
//! first byte says what follows.

use std::io::{self, Read, Write};

use ferostream_client::decode::{Decoder, Output};
use ferostream_proto::Message;
use ferostream_proto::tile::Encoding;

/// The most host bytes the window sends in one frame.
pub const MAX_IN: usize = 1024 * 1024;
/// The largest frame from the decoder: a FeroStream frame and its kind.
pub const MAX_OUT: usize = ferostream_proto::MAX_FRAME + 1;
/// The longest machine kind (`uname -m`) or error text passed on.
const MAX_TEXT: usize = 4096;

const KIND_MESSAGE: u8 = 1;
const KIND_UPLOAD: u8 = 2;
const KIND_FAILED: u8 = 3;

/// What the decoder hands the window.
#[derive(Debug, PartialEq, Eq)]
pub enum FromDecoder {
    Message(Message),
    /// The host asks for the agent for this machine kind.
    Upload(String),
    /// The host's stream broke (a protocol error); the session ends.
    Failed(String),
}

impl FromDecoder {
    fn encode(&self) -> Vec<u8> {
        let mut body = Vec::new();
        match self {
            FromDecoder::Message(m) => {
                body.push(KIND_MESSAGE);
                // The FeroStream frame without its length.
                body.extend_from_slice(m.encode().get(4..).unwrap_or_default());
            }
            FromDecoder::Upload(arch) => {
                body.push(KIND_UPLOAD);
                body.extend_from_slice(arch.as_bytes());
            }
            FromDecoder::Failed(why) => {
                body.push(KIND_FAILED);
                let mut end = why.len().min(MAX_TEXT);
                while !why.is_char_boundary(end) {
                    end -= 1;
                }
                body.extend_from_slice(why.get(..end).unwrap_or_default().as_bytes());
            }
        }
        body
    }

    /// Checks a frame from the decoder. It is trusted for nothing: a
    /// message must decode on its own and carry raw tiles only, a machine
    /// kind must look like one.
    pub fn decode(body: &[u8]) -> Result<FromDecoder, String> {
        let (&kind, rest) = body.split_first().ok_or("an empty frame")?;
        match kind {
            KIND_MESSAGE => {
                let m = Message::decode(rest).map_err(|e| e.to_string())?;
                if let Message::Tile(t) = &m
                    && t.encoding != Encoding::Raw
                {
                    return Err("a packed tile".into());
                }
                Ok(FromDecoder::Message(m))
            }
            KIND_UPLOAD => {
                let arch = std::str::from_utf8(rest).map_err(|_| "a machine kind not in UTF-8")?;
                if !ferostream_client::transport::is_arch(arch) {
                    return Err("a strange machine kind".into());
                }
                Ok(FromDecoder::Upload(arch.into()))
            }
            KIND_FAILED if rest.len() <= MAX_TEXT => Ok(FromDecoder::Failed(crate::ssh::sanitize(
                &String::from_utf8_lossy(rest),
            ))),
            _ => Err("an unknown frame".into()),
        }
    }
}

/// A remote app's command line as the program and its arguments: split at
/// spaces, with '...' and "..." keeping spaces (no other shell syntax;
/// the agent runs the program directly). `None` when a quote is left open
/// or there is nothing to run.
pub fn words(line: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    for c in line.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => word.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                in_word = true;
            }
            (None, c) if c.is_whitespace() => {
                if in_word {
                    out.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            (None, c) => {
                word.push(c);
                in_word = true;
            }
        }
    }
    if quote.is_some() {
        return None;
    }
    if in_word {
        out.push(word);
    }
    (!out.is_empty() && out.len() <= ferostream_proto::MAX_ARGS).then_some(out)
}

/// The remote app clipboard's ways, as Session > Remote apps > Clipboard
/// lists them and `RemoteAppClipboard` spells them.
pub const CLIPBOARD_WAYS: [&str; 4] = ["both", "to-host", "from-host", "off"];

/// Which ways copied text travels for a remote app: the global setting,
/// narrowed (never widened) by its connection's `limit`.
pub fn app_clipboard(
    settings: &ferossh_config::settings::Settings,
    limit: Option<&str>,
) -> ferostream_client::transport::Clipboard {
    use ferostream_client::transport::Clipboard;
    let global = settings
        .remote_app_clipboard
        .as_deref()
        .and_then(Clipboard::from_name)
        .unwrap_or(Clipboard::BOTH);
    let limit = limit
        .and_then(Clipboard::from_name)
        .unwrap_or(Clipboard::BOTH);
    Clipboard {
        to_host: global.to_host && limit.to_host,
        from_host: global.from_host && limit.from_host,
        selection: settings.remote_app_share_selection,
    }
}

/// Writes one frame.
pub fn write_frame(w: &mut impl Write, body: &[u8]) -> io::Result<()> {
    let len = u32::try_from(body.len()).map_err(|_| io::Error::other("frame too large"))?;
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend(len.to_le_bytes());
    frame.extend_from_slice(body);
    w.write_all(&frame)?;
    w.flush()
}

/// Reads one frame of at most `max` bytes; `None` at a clean end.
pub fn read_frame(r: &mut impl Read, max: usize) -> io::Result<Option<Vec<u8>>> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_le_bytes(len) as usize;
    if len == 0 || len > max {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "bad frame length",
        ));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    Ok(Some(body))
}

/// The decoder process: host bytes in, checked plain data out, until the
/// window closes the pipe or the stream breaks. Returns the exit code.
pub fn run(mut input: impl Read, mut output: impl Write) -> i32 {
    let mut decoder = Decoder::new(true);
    let mut out = Vec::new();
    loop {
        let bytes = match read_frame(&mut input, MAX_IN) {
            Ok(Some(b)) => b,
            Ok(None) => return 0,
            Err(_) => return 1,
        };
        let result = decoder.push(&bytes, &mut out);
        for o in out.drain(..) {
            let f = match o {
                Output::Upload(arch) => FromDecoder::Upload(arch),
                Output::Message(m) => FromDecoder::Message(m),
            };
            if write_frame(&mut output, &f.encode()).is_err() {
                return 1;
            }
        }
        if let Err(e) = result {
            let _ = write_frame(&mut output, &FromDecoder::Failed(e).encode());
            return 2;
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic
    )]

    use super::*;
    use ferostream_proto::PREAMBLE;
    use ferostream_proto::tile::Tile;

    #[test]
    fn a_connection_only_narrows_the_clipboard() {
        use ferossh_config::settings::Settings;
        let mut s = Settings::default();
        let c = app_clipboard(&s, None);
        assert!(c.to_host && c.from_host && !c.selection);
        let c = app_clipboard(&s, Some("to-host"));
        assert!(c.to_host && !c.from_host);
        s.remote_app_clipboard = Some("from-host".into());
        s.remote_app_share_selection = true;
        // Global from-host, connection to-host: nothing either way.
        let c = app_clipboard(&s, Some("to-host"));
        assert!(!c.to_host && !c.from_host);
        // A connection cannot widen it ("both" is no limit).
        let c = app_clipboard(&s, Some("both"));
        assert!(!c.to_host && c.from_host && c.selection);
        assert_eq!(
            c.rules(),
            ferostream_proto::Message::ClipboardRules {
                send: true,
                selection: true
            }
        );
        s.remote_app_clipboard = Some("off".into());
        let c = app_clipboard(&s, None);
        assert!(!c.to_host && !c.from_host);
        assert_eq!(
            c.rules(),
            ferostream_proto::Message::ClipboardRules {
                send: false,
                selection: false
            }
        );
        for way in CLIPBOARD_WAYS {
            assert!(ferostream_client::transport::Clipboard::from_name(way).is_some());
        }
    }

    fn decode_all(mut bytes: &[u8]) -> Vec<FromDecoder> {
        let mut out = Vec::new();
        while let Some(f) = read_frame(&mut bytes, MAX_OUT).unwrap() {
            out.push(FromDecoder::decode(&f).unwrap());
        }
        out
    }

    #[test]
    fn host_bytes_come_back_checked_and_unpacked() {
        let px: Vec<u8> = (0..32 * 16 * 4).map(|i| (i % 11) as u8).collect();
        let mut host = b"Welcome\n\0FEROSTREAM-UPLOAD aarch64\n".to_vec();
        host.extend_from_slice(PREAMBLE);
        host.extend(Message::Ping { token: 9 }.encode());
        host.extend(Message::Tile(Tile::lz4(3, 4, 5, 32, 16, &px)).encode());
        // Cut anywhere: here in 7-byte frames.
        let mut input = Vec::new();
        for chunk in host.chunks(7) {
            write_frame(&mut input, chunk).unwrap();
        }
        let mut output = Vec::new();
        assert_eq!(run(&input[..], &mut output), 0);
        let got = decode_all(&output);
        assert_eq!(got[0], FromDecoder::Upload("aarch64".into()));
        assert_eq!(got[1], FromDecoder::Message(Message::Ping { token: 9 }));
        match &got[2] {
            FromDecoder::Message(Message::Tile(t)) => {
                assert_eq!(t.encoding, Encoding::Raw);
                assert_eq!(t.data, px);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn packed_tiles_unpack_and_copies_pass_through() {
        use ferostream_proto::tile::Effort;
        // Few colours (a palette) and a gradient (colour rows), both ways
        // of compressing, and a scroll.
        let text: Vec<u8> = (0..64 * 32)
            .flat_map(|i: u32| [(i % 3) as u8 * 80, 0, 0, 0])
            .collect();
        let ramp: Vec<u8> = (0..64 * 32)
            .flat_map(|i: u32| [(i % 251) as u8, (i / 64) as u8, (i * 7 % 256) as u8, 0])
            .collect();
        let copy = Message::TileCopy {
            window: 3,
            x: 0,
            y: 16,
            width: 64,
            height: 16,
            to_x: 0,
            to_y: 0,
        };
        let mut host = PREAMBLE.to_vec();
        let mut want = Vec::new();
        for (px, effort) in [
            (&text, Effort::Fast),
            (&text, Effort::Strong),
            (&ramp, Effort::Fast),
            (&ramp, Effort::Strong),
        ] {
            host.extend(Message::Tile(Tile::pack(3, 0, 0, 64, 32, px, effort)).encode());
            want.push(px.clone());
        }
        host.extend(copy.encode());
        let mut input = Vec::new();
        write_frame(&mut input, &host).unwrap();
        let mut output = Vec::new();
        assert_eq!(run(&input[..], &mut output), 0);
        let got = decode_all(&output);
        assert_eq!(got.len(), 5);
        for (g, px) in got.iter().zip(&want) {
            match g {
                FromDecoder::Message(Message::Tile(t)) => {
                    assert_eq!(t.encoding, Encoding::Raw);
                    assert_eq!(&t.data, px);
                }
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(got[4], FromDecoder::Message(copy));
    }

    #[test]
    fn command_lines_split_like_a_shell_would_simply() {
        let w = |s: &str| words(s).map(|v| v.join("|"));
        assert_eq!(w("gimp ~/a.png"), Some("gimp|~/a.png".into()));
        assert_eq!(
            w("  xterm   -T 'my term' "),
            Some("xterm|-T|my term".into())
        );
        assert_eq!(w(r#"sh -c "echo 'hi'""#), Some("sh|-c|echo 'hi'".into()));
        assert_eq!(w("a ''"), Some("a|".into()));
        assert_eq!(w("'open"), None);
        assert_eq!(w("   "), None);
    }

    #[test]
    fn a_broken_stream_is_reported_and_ends() {
        let mut host = PREAMBLE.to_vec();
        host.extend([0xff, 0xff, 0xff, 0x7f]);
        let mut input = Vec::new();
        write_frame(&mut input, &host).unwrap();
        let mut output = Vec::new();
        assert_eq!(run(&input[..], &mut output), 2);
        assert!(matches!(decode_all(&output)[..], [FromDecoder::Failed(_)]));
        // Too big a frame from the window: the decoder stops.
        let mut output = Vec::new();
        assert_eq!(run(&[0xff, 0xff, 0xff, 0x00][..], &mut output), 1);
    }

    #[test]
    fn the_window_refuses_what_a_taken_over_decoder_could_send() {
        let packed = Message::Tile(Tile::lz4(1, 0, 0, 4, 4, &[0; 64])).encode();
        let mut f = vec![KIND_MESSAGE];
        f.extend_from_slice(&packed[4..]);
        assert!(FromDecoder::decode(&f).is_err());
        assert!(FromDecoder::decode(&[KIND_UPLOAD, b'.', b'.']).is_err());
        assert!(FromDecoder::decode(&[KIND_MESSAGE, 200]).is_err());
        assert!(FromDecoder::decode(&[9]).is_err());
        assert!(FromDecoder::decode(&[]).is_err());
        let long = vec![b'x'; MAX_TEXT + 1];
        assert!(FromDecoder::decode(&[&[KIND_FAILED][..], &long].concat()).is_err());
    }
}
