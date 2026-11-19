//! iTerm2's inline images: `ESC ] 1337 ; File = <args> : <base64> BEL`.
//! Only pictures shown inline (`inline=1`) and only PNG; a file the host
//! offers for download is ignored.

use crate::{Error, Limits, Picture};

/// A width or height the host asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Extent {
    /// The picture's own size.
    #[default]
    Auto,
    Cells(u32),
    Pixels(u32),
    /// Of the pane's width or height.
    Percent(u32),
}

impl Extent {
    fn parse(v: &str) -> Extent {
        let num = |s: &str| s.parse::<u32>().ok();
        if let Some(n) = v.strip_suffix("px").and_then(num) {
            Extent::Pixels(n)
        } else if let Some(n) = v.strip_suffix('%').and_then(num) {
            Extent::Percent(n.min(100))
        } else if let Some(n) = num(v) {
            Extent::Cells(n)
        } else {
            Extent::Auto
        }
    }
}

/// An inline image's arguments.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct File {
    pub inline: bool,
    pub width: Extent,
    pub height: Extent,
    /// Keep the shape when both sides are given (the default).
    pub keep_shape: bool,
}

/// Splits the OSC body after `1337;File=` into its arguments and the
/// base64 payload.
pub fn parse(body: &[u8]) -> (File, &[u8]) {
    let (args, payload) = match body.iter().position(|&b| b == b':') {
        Some(at) => (
            body.get(..at).unwrap_or(&[]),
            body.get(at + 1..).unwrap_or(&[]),
        ),
        None => (body, &[][..]),
    };
    let mut file = File {
        keep_shape: true,
        ..File::default()
    };
    for arg in args.split(|&b| b == b';') {
        let Ok(arg) = std::str::from_utf8(arg) else {
            continue;
        };
        let Some((k, v)) = arg.split_once('=') else {
            continue;
        };
        match k {
            "inline" => file.inline = v == "1",
            "width" => file.width = Extent::parse(v),
            "height" => file.height = Extent::parse(v),
            "preserveAspectRatio" => file.keep_shape = v != "0",
            _ => {}
        }
    }
    (file, payload)
}

/// Decodes the payload: PNG only.
pub fn decode(payload: &[u8], limits: &Limits) -> Result<Picture, Error> {
    if payload.len() > limits.max_encoded {
        return Err(Error::TooLarge);
    }
    let data = crate::base64::decode(payload, limits.max_encoded)?;
    if !crate::png::is_png(&data) {
        return Err(Error::Unsupported);
    }
    crate::png::decode(&data, limits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_args() {
        let (f, p) =
            parse(b"name=eA==;size=3;inline=1;width=10;height=50%;preserveAspectRatio=0:QUJD");
        assert!(f.inline && !f.keep_shape);
        assert_eq!(
            (f.width, f.height),
            (Extent::Cells(10), Extent::Percent(50))
        );
        assert_eq!(p, b"QUJD");
        let (f, p) = parse(b"inline=0");
        assert!(!f.inline && f.keep_shape);
        assert_eq!(f.width, Extent::Auto);
        assert!(p.is_empty());
        assert_eq!(Extent::parse("120px"), Extent::Pixels(120));
    }

    #[test]
    fn png_only() {
        assert_eq!(decode(b"QUJD", &Limits::DEFAULT), Err(Error::Unsupported));
    }
}
