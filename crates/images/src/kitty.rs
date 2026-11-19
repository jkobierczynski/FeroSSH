//! The kitty graphics protocol: `ESC _ G <keys> ; <payload> ESC \`.
//!
//! Only direct transmission (`t=d`, the data in the escape sequence) is
//! taken. Files, temporary files and shared memory (`t=f`, `t=t`,
//! `t=s`) would have FeroSSH read a path on this PC that the host
//! chose, so they are refused with an error, which makes programs such
//! as `kitten icat` fall back to sending the data.

use crate::{Error, Limits, Picture};

/// One graphics command's keys.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Command {
    /// `a`: `t` transmit, `T` transmit and show, `p` show, `d` delete,
    /// `q` query.
    pub action: u8,
    /// `f`: 24 RGB, 32 RGBA, 100 PNG.
    pub format: u32,
    /// `t`: only `d` (direct) is taken.
    pub medium: u8,
    /// `o=z`: zlib-compressed.
    pub compressed: bool,
    /// `s`, `v`: pixel size for raw data.
    pub width: u32,
    pub height: u32,
    /// `i`, `I`, `p`.
    pub id: u32,
    pub number: u32,
    pub placement: u32,
    /// `m=1`: more chunks follow.
    pub more: bool,
    /// `q`: 1 hides OK replies, 2 all replies.
    pub quiet: u8,
    /// `c`, `r`: cells to show the picture in.
    pub cols: u32,
    pub rows: u32,
    /// `x`, `y`, `w`, `h`: the part of the picture shown.
    pub src_x: u32,
    pub src_y: u32,
    pub src_w: u32,
    pub src_h: u32,
    /// `C=1`: leave the cursor where it is.
    pub cursor_stays: bool,
    /// `d`: what to delete.
    pub delete: u8,
}

impl Command {
    /// Parses the keys before the `;` (the leading `G` already taken).
    /// Unknown keys are skipped.
    pub fn parse(keys: &[u8]) -> Command {
        let mut c = Command {
            action: b't',
            format: 32,
            medium: b'd',
            delete: b'a',
            ..Command::default()
        };
        for pair in keys.split(|&b| b == b',') {
            let Some((&k, rest)) = pair.split_first() else {
                continue;
            };
            let Some(v) = rest.strip_prefix(b"=") else {
                continue;
            };
            let num = || -> u32 {
                std::str::from_utf8(v)
                    .ok()
                    .and_then(|s| s.parse::<u32>().ok())
                    .unwrap_or(0)
            };
            let ch = v.first().copied().unwrap_or(0);
            match k {
                b'a' => c.action = ch,
                b'f' => c.format = num(),
                b't' => c.medium = ch,
                b'o' => c.compressed = ch == b'z',
                b's' => c.width = num(),
                b'v' => c.height = num(),
                b'i' => c.id = num(),
                b'I' => c.number = num(),
                b'p' => c.placement = num(),
                b'm' => c.more = ch == b'1',
                b'q' => c.quiet = num().min(2) as u8,
                b'c' => c.cols = num(),
                b'r' => c.rows = num(),
                b'x' => c.src_x = num(),
                b'y' => c.src_y = num(),
                b'w' => c.src_w = num(),
                b'h' => c.src_h = num(),
                b'C' => c.cursor_stays = ch == b'1',
                b'd' => c.delete = ch,
                _ => {}
            }
        }
        c
    }

    /// The reply to send for `result`, if any: kitty answers only
    /// commands that carry an id or number, and `q` silences it.
    pub fn reply(&self, result: Result<(), Error>) -> Option<Vec<u8>> {
        if (self.id == 0 && self.number == 0)
            || self.quiet >= 2
            || (self.quiet == 1 && result.is_ok())
        {
            return None;
        }
        let mut out = String::from("\x1b_G");
        if self.id != 0 {
            out.push_str(&format!("i={}", self.id));
        }
        if self.number != 0 {
            if self.id != 0 {
                out.push(',');
            }
            out.push_str(&format!("I={}", self.number));
        }
        if self.placement != 0 {
            out.push_str(&format!(",p={}", self.placement));
        }
        out.push(';');
        out.push_str(match result {
            Ok(()) => "OK",
            Err(Error::Invalid) => "EINVAL:malformed picture",
            Err(Error::TooLarge) => "EFBIG:picture too large",
            Err(Error::Unsupported) => "EINVAL:not supported",
            Err(Error::NotFound) => "ENOENT:no such image",
        });
        out.push_str("\x1b\\");
        Some(out.into_bytes())
    }

    /// Decodes the whole payload (base64, chunks joined).
    pub fn decode(&self, payload: &[u8], limits: &Limits) -> Result<Picture, Error> {
        if self.medium != b'd' {
            return Err(Error::Unsupported);
        }
        if payload.len() > limits.max_encoded {
            return Err(Error::TooLarge);
        }
        let data = crate::base64::decode(payload, limits.max_encoded)?;
        let raw_max = limits.max_pixels.saturating_mul(4);
        let data = if self.compressed {
            crate::inflate(&data, raw_max.max(limits.max_encoded))?
        } else {
            data
        };
        match self.format {
            100 => crate::png::decode(&data, limits),
            24 => crate::from_raw(&data, self.width, self.height, false, limits),
            32 => crate::from_raw(&data, self.width, self.height, true, limits),
            _ => Err(Error::Unsupported),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_keys() {
        let c = Command::parse(b"a=T,f=24,s=2,v=1,i=7,q=1,m=1,C=1,zz=3,bad");
        assert_eq!(
            (c.action, c.format, c.width, c.height, c.id),
            (b'T', 24, 2, 1, 7)
        );
        assert!(c.more && c.cursor_stays);
        assert_eq!(c.quiet, 1);
        let d = Command::parse(b"");
        assert_eq!((d.action, d.format, d.medium), (b't', 32, b'd'));
    }

    #[test]
    fn decodes_raw_and_refuses_files() {
        let c = Command::parse(b"f=24,s=1,v=1");
        let p = c.decode(b"/wAA", &Limits::DEFAULT).map(|p| p.pixels);
        assert_eq!(p, Ok(vec![0xFFFF_0000]));
        // A path on this PC, base64: refused.
        let f = Command::parse(b"t=f,f=100");
        assert_eq!(
            f.decode(b"L2V0Yy9wYXNzd2Q=", &Limits::DEFAULT),
            Err(Error::Unsupported)
        );
    }

    #[test]
    fn compressed() {
        let z = miniz_oxide::deflate::compress_to_vec_zlib(&[0, 255, 0, 255], 6);
        let b64 = crate::base64::encode(&z);
        let c = Command::parse(b"f=32,s=1,v=1,o=z");
        assert_eq!(
            c.decode(b64.as_bytes(), &Limits::DEFAULT).map(|p| p.pixels),
            Ok(vec![0xFF00_FF00])
        );
    }

    #[test]
    fn replies() {
        let c = Command::parse(b"i=31,a=q");
        assert_eq!(c.reply(Ok(())), Some(b"\x1b_Gi=31;OK\x1b\\".to_vec()));
        assert_eq!(
            c.reply(Err(Error::Unsupported)),
            Some(b"\x1b_Gi=31;EINVAL:not supported\x1b\\".to_vec())
        );
        assert_eq!(Command::parse(b"a=T").reply(Ok(())), None);
        assert_eq!(Command::parse(b"i=1,q=1").reply(Ok(())), None);
        assert_eq!(Command::parse(b"i=1,q=2").reply(Err(Error::Invalid)), None);
    }
}
