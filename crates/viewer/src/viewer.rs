//! The viewer process: `ferossh.exe --viewer <in> <out>`, in an
//! AppContainer with no capabilities at all. It reads the server's bytes
//! and the user's input from the window, decodes them with the VNC client
//! ([`Client`](crate::vnc::Client)) or the RDP session, and sends back
//! pixels and the bytes for the server. It never touches the network,
//! files or the screen itself.

use std::io::{self, Read, Write};

#[cfg(any(feature = "vnc", feature = "rdp"))]
use crate::pipe::MAX_STRIPE_PIXELS;
use crate::pipe::{FromViewer, Protocol, ToViewer, read_frame, write_frame};

pub(crate) fn send(out: &mut impl Write, msg: &FromViewer) -> io::Result<()> {
    write_frame(out, &msg.encode())
}

/// Sends a rectangle of pixels in stripes of whole rows, each small
/// enough for one frame.
#[cfg(any(feature = "vnc", feature = "rdp"))]
pub(crate) fn send_pixels(
    out: &mut impl Write,
    (x, y, width): (u16, u16, u16),
    data: &[u32],
) -> io::Result<()> {
    let w = usize::from(width).max(1);
    let rows = (MAX_STRIPE_PIXELS / w).max(1);
    for (i, stripe) in data.chunks(rows * w).enumerate() {
        let top = u16::try_from(i * rows).unwrap_or(u16::MAX);
        send(
            out,
            &FromViewer::Pixels {
                x,
                y: y.saturating_add(top),
                width,
                height: u16::try_from(stripe.len() / w).unwrap_or(0),
                data: stripe.to_vec(),
            },
        )?;
    }
    Ok(())
}

/// The next message from the window; `None` when the pipe closed.
pub(crate) fn next(input: &mut impl Read, output: &mut impl Write) -> io::Result<Option<ToViewer>> {
    let Some(frame) = read_frame(input)? else {
        return Ok(None);
    };
    match ToViewer::decode(&frame) {
        Some(m) => Ok(Some(m)),
        None => {
            send(
                output,
                &FromViewer::Failed("The window sent the viewer nonsense.".into()),
            )?;
            Err(io::Error::new(io::ErrorKind::InvalidData, "bad frame"))
        }
    }
}

/// Runs a viewer session until the window closes the pipe, the server's
/// connection ends, or the session fails. The first message says which
/// protocol.
pub fn run(mut input: impl Read + Send + 'static, mut output: impl Write) -> io::Result<()> {
    let Some(start) = next(&mut input, &mut output)? else {
        return Ok(());
    };
    let ToViewer::Start {
        protocol,
        width,
        height,
        layout,
        user,
    } = start
    else {
        send(
            &mut output,
            &FromViewer::Failed("The window did not say what to connect to.".into()),
        )?;
        return Err(io::Error::new(io::ErrorKind::InvalidData, "no start"));
    };
    let _ = (width, height, layout, &user);
    match protocol {
        #[cfg(feature = "vnc")]
        Protocol::Vnc => vnc_session(input, output, (width, height)),
        #[cfg(feature = "rdp")]
        Protocol::Rdp => crate::rdp::session(input, output, (width, height), layout, user),
        #[allow(unreachable_patterns)]
        other => {
            let _: Protocol = other;
            send(
                &mut output,
                &FromViewer::Failed("This FeroSSH was built without that desktop viewer.".into()),
            )
        }
    }
}

/// Sends what the VNC client has for the window.
#[cfg(feature = "vnc")]
fn flush(client: &mut crate::vnc::Client, out: &mut impl Write) -> io::Result<()> {
    use crate::pipe::MAX_NET;
    use crate::vnc::Event;
    for chunk in client.take_output().chunks(MAX_NET) {
        send(out, &FromViewer::Net(chunk.to_vec()))?;
    }
    for event in client.take_events() {
        match event {
            Event::Size { width, height } => send(out, &FromViewer::Size { width, height })?,
            Event::Name(n) => send(out, &FromViewer::Name(n))?,
            Event::Pixels {
                x, y, width, data, ..
            } => send_pixels(out, (x, y, width), &data)?,
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

#[cfg(feature = "vnc")]
fn vnc_session(
    mut input: impl Read,
    mut output: impl Write,
    (width, height): (u16, u16),
) -> io::Result<()> {
    let mut client = crate::vnc::Client::new();
    if width > 0 && height > 0 {
        client.resize(width, height);
    }
    let mut keyboard = crate::keysym::Keyboard::default();
    while let Some(msg) = next(&mut input, &mut output)? {
        match msg {
            ToViewer::Net(bytes) => client.feed(&bytes),
            ToViewer::Closed => {
                let why = if client.size() == (0, 0) {
                    "The VNC server closed the connection."
                } else {
                    "The remote desktop closed."
                };
                return send(&mut output, &FromViewer::Failed(why.into()));
            }
            ToViewer::Key {
                vk,
                scan,
                extended,
                down,
            } => {
                for (down, keysym) in keyboard.key(vk, extended, scan, down) {
                    client.key(down, keysym);
                }
            }
            ToViewer::Char(c) => {
                for (down, keysym) in keyboard.char(c) {
                    client.key(down, keysym);
                }
            }
            ToViewer::Pointer { buttons, x, y } => client.pointer(buttons, x, y),
            ToViewer::Password(p) => {
                let p = zeroize::Zeroizing::new(p);
                client.password(&p);
            }
            ToViewer::Release => {
                for (down, keysym) in keyboard.release_all() {
                    client.key(down, keysym);
                }
            }
            ToViewer::Resize { width, height } => client.resize(width, height),
            ToViewer::Start { .. } | ToViewer::User(_) => {}
        }
        flush(&mut client, &mut output)?;
        if client.failed() {
            return Ok(());
        }
    }
    Ok(())
}

#[cfg(all(test, feature = "vnc"))]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;
    use crate::framebuffer::{Applied, Framebuffer};
    use crate::pipe::MAX_NET;

    fn vnc_start() -> ToViewer {
        ToViewer::Start {
            protocol: Protocol::Vnc,
            width: 0,
            height: 0,
            layout: 0,
            user: String::new(),
        }
    }

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
        let mut msgs = vec![vnc_start()];
        msgs.extend(server.chunks(MAX_NET).map(|c| ToViewer::Net(c.to_vec())));
        msgs.push(ToViewer::Closed);
        let input = frames(&msgs);
        let mut out = Vec::new();
        run(std::io::Cursor::new(input), &mut out).unwrap();
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
        let mut input = frames(&[vnc_start()]);
        write_frame(&mut input, &[42]).unwrap();
        let mut out = Vec::new();
        assert!(run(std::io::Cursor::new(input), &mut out).is_err());
        assert!(matches!(&replies(&out)[..], [FromViewer::Failed(_)]));
    }
    /// A real VNC server, when `FEROSSH_TEST_VNC` names one as "host:port"
    /// without a password (CI runs TigerVNC's Xvnc): the viewer is asked
    /// for a new size and the server's screen follows.
    #[test]
    fn resizes_a_real_server() {
        use std::net::TcpStream;
        use std::sync::{Arc, Mutex};
        use std::time::{Duration, Instant};
        let Ok(addr) = std::env::var("FEROSSH_TEST_VNC") else {
            eprintln!("FEROSSH_TEST_VNC not set; skipped");
            return;
        };
        let tcp = TcpStream::connect(addr.trim()).unwrap();
        let (viewer_in, window_out) = std::io::pipe().unwrap();
        let (mut window_in, viewer_out) = std::io::pipe().unwrap();
        std::thread::spawn(move || run(viewer_in, viewer_out));
        let to_viewer = Arc::new(Mutex::new(window_out));
        let tell = |m: ToViewer| write_frame(&mut *to_viewer.lock().unwrap(), &m.encode()).unwrap();
        tell(ToViewer::Start {
            protocol: Protocol::Vnc,
            width: 1000,
            height: 700,
            layout: 0,
            user: String::new(),
        });
        let mut net = tcp.try_clone().unwrap();
        let pump = to_viewer.clone();
        std::thread::spawn(move || {
            let mut buf = vec![0u8; 65536];
            loop {
                let n = net.read(&mut buf).unwrap_or(0);
                let msg = if n == 0 {
                    ToViewer::Closed
                } else {
                    ToViewer::Net(buf[..n].to_vec())
                };
                if write_frame(&mut *pump.lock().unwrap(), &msg.encode()).is_err() || n == 0 {
                    return;
                }
            }
        });
        let mut tcp_out = tcp;
        let mut fb = Framebuffer::default();
        let mut resized = false;
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            assert!(
                Instant::now() < deadline,
                "the screen stayed {}x{}",
                fb.width(),
                fb.height()
            );
            let Some(frame) = read_frame(&mut window_in).unwrap() else {
                panic!("the viewer stopped");
            };
            match fb.apply(FromViewer::decode(&frame).unwrap()).unwrap() {
                Applied::Other(FromViewer::Net(b)) => tcp_out.write_all(&b).unwrap(),
                Applied::Other(FromViewer::Failed(why)) => panic!("{why}"),
                _ => {}
            }
            // The size the window started with, then a new one.
            if (fb.width(), fb.height()) == (1000, 700) && !resized {
                resized = true;
                tell(ToViewer::Resize {
                    width: 640,
                    height: 480,
                });
            }
            if (fb.width(), fb.height()) == (640, 480) {
                break;
            }
        }
        tell(ToViewer::Closed);
    }
}
