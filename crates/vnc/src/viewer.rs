//! The viewer process: `ferossh.exe --viewer <in> <out>`, in an
//! AppContainer with no capabilities at all. It reads the VNC server's
//! bytes and the user's input from the window, decodes with
//! [`Client`](crate::rfb::Client), and sends back pixels and the bytes for
//! the server. It never touches the network, files or the screen itself.

use std::io::{self, Read, Write};

use crate::pipe::{FromViewer, MAX_NET, MAX_STRIPE_PIXELS, ToViewer, read_frame, write_frame};
use crate::rfb::{Client, Event};

fn send(out: &mut impl Write, msg: &FromViewer) -> io::Result<()> {
    write_frame(out, &msg.encode())
}

/// Sends what the client has for the window.
fn flush(client: &mut Client, out: &mut impl Write) -> io::Result<()> {
    for chunk in client.take_output().chunks(MAX_NET) {
        send(out, &FromViewer::Net(chunk.to_vec()))?;
    }
    for event in client.take_events() {
        match event {
            Event::Size { width, height } => send(out, &FromViewer::Size { width, height })?,
            Event::Name(n) => send(out, &FromViewer::Name(n))?,
            Event::Pixels {
                x,
                y,
                width,
                height,
                data,
            } => {
                // Stripes of whole rows, each small enough for a frame.
                let rows = (MAX_STRIPE_PIXELS / usize::from(width).max(1)).max(1);
                for (i, stripe) in data.chunks(rows * usize::from(width)).enumerate() {
                    let top = i * rows;
                    let h = stripe.len() / usize::from(width).max(1);
                    send(
                        out,
                        &FromViewer::Pixels {
                            x,
                            y: y + u16::try_from(top).unwrap_or(0),
                            width,
                            height: u16::try_from(h).unwrap_or(0),
                            data: stripe.to_vec(),
                        },
                    )?;
                }
                let _ = height;
            }
            Event::Copy {
                src_x,
                src_y,
                x,
                y,
                width,
                height,
            } => send(
                out,
                &FromViewer::Copy {
                    src_x,
                    src_y,
                    x,
                    y,
                    width,
                    height,
                },
            )?,
            Event::Bell => send(out, &FromViewer::Bell)?,
            Event::NeedPassword => send(out, &FromViewer::NeedPassword)?,
            Event::Failed(why) => send(out, &FromViewer::Failed(why))?,
        }
    }
    Ok(())
}

/// Runs a viewer session until the window closes the pipe, the server's
/// connection ends, or the session fails.
pub fn run(mut input: impl Read, mut output: impl Write) -> io::Result<()> {
    let mut client = Client::new();
    loop {
        let Some(frame) = read_frame(&mut input)? else {
            return Ok(());
        };
        let Some(msg) = ToViewer::decode(&frame) else {
            send(
                &mut output,
                &FromViewer::Failed("The window sent the viewer nonsense.".into()),
            )?;
            return Err(io::Error::new(io::ErrorKind::InvalidData, "bad frame"));
        };
        match msg {
            ToViewer::Net(bytes) => client.feed(&bytes),
            ToViewer::Closed => {
                let why = if client.size() == (0, 0) {
                    "The VNC server closed the connection."
                } else {
                    "The remote desktop closed."
                };
                send(&mut output, &FromViewer::Failed(why.into()))?;
                return Ok(());
            }
            ToViewer::Key { down, keysym } => client.key(down, keysym),
            ToViewer::Pointer { buttons, x, y } => client.pointer(buttons, x, y),
            ToViewer::Password(p) => {
                let p = zeroize::Zeroizing::new(p);
                client.password(&p);
            }
        }
        flush(&mut client, &mut output)?;
        if client.failed() {
            return Ok(());
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;
    use crate::framebuffer::{Applied, Framebuffer};

    fn frames(msgs: &[ToViewer]) -> Vec<u8> {
        let mut v = Vec::new();
        for m in msgs {
            write_frame(&mut v, &m.encode()).unwrap();
        }
        v
    }

    fn replies(out: &[u8]) -> Vec<FromViewer> {
        let mut r = out;
        let mut v = Vec::new();
        while let Some(f) = read_frame(&mut r).unwrap() {
            v.push(FromViewer::decode(&f).unwrap());
        }
        v
    }

    #[test]
    fn a_big_screen_arrives_in_stripes_that_fit_the_framebuffer() {
        let (w, h) = (2560u16, 1700u16);
        let mut server = b"RFB 003.008\n\x01\x01".to_vec();
        server.extend(0u32.to_be_bytes());
        server.extend(w.to_be_bytes());
        server.extend(h.to_be_bytes());
        server.extend([32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0]);
        server.extend(0u32.to_be_bytes());
        server.extend([0, 0, 0, 1, 0, 0, 0, 0]);
        server.extend(w.to_be_bytes());
        server.extend(h.to_be_bytes());
        server.extend(0i32.to_be_bytes());
        server.extend(
            std::iter::repeat_n([0x11u8, 0x22, 0x33, 0], usize::from(w) * usize::from(h)).flatten(),
        );
        let mut msgs: Vec<ToViewer> = server
            .chunks(MAX_NET)
            .map(|c| ToViewer::Net(c.to_vec()))
            .collect();
        msgs.push(ToViewer::Closed);
        let input = frames(&msgs);
        let mut out = Vec::new();
        run(&input[..], &mut out).unwrap();
        let mut fb = Framebuffer::default();
        let mut stripes = 0;
        let mut ended = None;
        let mut sent = Vec::new();
        for m in replies(&out) {
            if matches!(m, FromViewer::Pixels { .. }) {
                stripes += 1;
            }
            match fb.apply(m).unwrap() {
                Applied::Drawn => {}
                Applied::Other(FromViewer::Net(b)) => sent.extend(b),
                Applied::Other(FromViewer::Failed(why)) => ended = Some(why),
                Applied::Other(o) => assert!(matches!(o, FromViewer::Name(_)), "{o:?}"),
            }
        }
        assert!(stripes > 1);
        assert!(fb.pixels().iter().all(|&p| p == 0x332211));
        assert_eq!(fb.take_dirty(), Some((0, 0, w, h)));
        assert_eq!(ended.as_deref(), Some("The remote desktop closed."));
        assert!(sent.starts_with(b"RFB 003.008\n\x01\x01"));
    }

    #[test]
    fn nonsense_from_the_window_stops_the_viewer() {
        let mut input = Vec::new();
        write_frame(&mut input, &[42]).unwrap();
        let mut out = Vec::new();
        assert!(run(&input[..], &mut out).is_err());
        assert!(matches!(&replies(&out)[..], [FromViewer::Failed(_)]));
    }
}
