//! Inline images: Sixel, kitty graphics and iTerm2 sequences become
//! pictures over the cells, and nothing of them reaches the screen as
//! text.

#![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

use ferossh_images::base64::encode;
use ferossh_render::{Grid, PictureCell};
use ferossh_term::{Action, Settings, Terminal};

fn term(cols: usize, rows: usize) -> Terminal {
    let settings = Settings {
        images: true,
        ..Settings::default()
    };
    let mut t = Terminal::new(cols, rows, &settings);
    t.set_cell_size(10, 20);
    t
}

fn grid(t: &Terminal) -> (Grid, Option<ferossh_render::Cursor>) {
    let mut g = Grid::new(1, 1);
    let c = t.draw_into(&mut g, true);
    (g, c)
}

fn pic(g: &Grid, x: usize, y: usize) -> Option<PictureCell> {
    g.cell(x, y).unwrap().picture
}

fn text(g: &Grid, y: usize) -> String {
    g.row(y)
        .unwrap()
        .iter()
        .map(|c| c.ch)
        .collect::<String>()
        .trim_end()
        .to_owned()
}

fn replies(t: &mut Terminal) -> Vec<Vec<u8>> {
    t.take_actions()
        .into_iter()
        .filter_map(|a| match a {
            Action::Reply(r) | Action::ImageReply(r) => Some(r),
            _ => None,
        })
        .collect()
}

/// A PNG of `w` x `h` RGBA pixels, deflate "stored" (no compression).
fn png(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
    let mut raw = Vec::new();
    for row in rgba.chunks(w as usize * 4) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    let mut z = vec![0x78, 0x01, 0x01];
    let n = raw.len() as u16;
    z.extend_from_slice(&n.to_le_bytes());
    z.extend_from_slice(&(!n).to_le_bytes());
    z.extend_from_slice(&raw);
    let (mut a, mut b) = (1u32, 0u32);
    for &x in &raw {
        a = (a + u32::from(x)) % 65521;
        b = (b + a) % 65521;
    }
    z.extend_from_slice(&(b << 16 | a).to_be_bytes());
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut chunk = |kind: &[u8], body: &[u8]| {
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        out.extend_from_slice(&[0; 4]);
    };
    let mut ihdr = w.to_be_bytes().to_vec();
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(b"IHDR", &ihdr);
    chunk(b"IDAT", &z);
    chunk(b"IEND", &[]);
    out
}

#[test]
fn off_by_default_and_sequences_vanish() {
    let mut t = Terminal::new(20, 5, &Settings::default());
    t.feed(b"a\x1bPq#0~~\x1b\\b");
    let (g, _) = grid(&t);
    assert_eq!(text(&g, 0), "ab");
    assert!(g.pictures().is_empty());
    assert!(!t.images_on());
}

#[test]
fn sixel_covers_cells_and_moves_below() {
    let mut t = term(20, 10);
    // 25 x 30 pixels: 3 x 2 cells of 10 x 20.
    let mut six = b"x\x1bPq#1;2;100;0;0#1!25~-!25~-!25~-!25~-!25~\x1b\\".to_vec();
    six.extend_from_slice(b"y");
    t.feed(&six);
    let (g, cursor) = grid(&t);
    assert_eq!(text(&g, 0), "x");
    assert_eq!(pic(&g, 0, 0), None);
    let p = pic(&g, 1, 0).unwrap();
    assert_eq!((p.row, p.col, p.rows, p.cols), (0, 0, 2, 3));
    assert_eq!(pic(&g, 3, 1).map(|p| (p.row, p.col)), Some((1, 2)));
    assert_eq!(pic(&g, 4, 1), None);
    // The text after goes below the picture, at its left edge.
    assert_eq!(text(&g, 2), " y");
    assert_eq!(cursor.map(|c| (c.x, c.y)), Some((2, 2)));
    let picture = g.picture(p.key).unwrap();
    assert_eq!((picture.width, picture.height), (25, 30));
    assert_eq!(picture.pixels[0], 0xFFFF_0000);
}

#[test]
fn pictures_scroll_and_are_overwritten() {
    let mut t = term(10, 3);
    t.feed(b"\x1bPq!10~\x1b\\");
    t.feed(b"\r\n\r\n\r\n");
    let (g, _) = grid(&t);
    assert!(g.row(0).unwrap().iter().all(|c| c.picture.is_none()));
    t.scroll_lines(2);
    let (g, _) = grid(&t);
    assert!(pic(&g, 0, 0).is_some(), "in the scrollback");
    t.scroll_to_bottom();
    let mut t = term(10, 3);
    t.feed(b"\x1bPq!10~\x1b\\\x1b[HZ");
    let (g, _) = grid(&t);
    assert_eq!(pic(&g, 0, 0), None);
    assert_eq!(g.cell(0, 0).unwrap().ch, 'Z');
}

#[test]
fn kitty_transmit_place_delete_and_reply() {
    let mut t = term(20, 6);
    // Query: OK, nothing shown.
    t.feed(
        format!(
            "\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;{}\x1b\\",
            encode(&[0, 0, 0])
        )
        .as_bytes(),
    );
    assert_eq!(replies(&mut t), vec![b"\x1b_Gi=31;OK\x1b\\".to_vec()]);
    // Files on this PC are refused.
    t.feed(b"\x1b_Gi=32,a=q,t=f;L2V0Yy9wYXNzd2Q=\x1b\\");
    assert_eq!(
        replies(&mut t),
        vec![b"\x1b_Gi=32;EINVAL:not supported\x1b\\".to_vec()]
    );
    // Transmit in two chunks, then show twice.
    let data = encode(&[255u8; 20 * 20 * 4]);
    let (a, b) = data.split_at(400);
    t.feed(format!("\x1b_Ga=t,f=32,s=20,v=20,i=7,m=1;{a}\x1b\\").as_bytes());
    t.feed(format!("\x1b_Gm=0;{b}\x1b\\").as_bytes());
    assert_eq!(replies(&mut t), vec![b"\x1b_Gi=7;OK\x1b\\".to_vec()]);
    t.feed(b"\x1b_Ga=p,i=7,q=1\x1b\\X");
    let (g, cursor) = grid(&t);
    assert_eq!(pic(&g, 0, 0).map(|p| (p.rows, p.cols)), Some((1, 2)));
    // The cursor stays right of the picture.
    assert_eq!(cursor.map(|c| (c.x, c.y)), Some((3, 0)));
    assert_eq!(g.cell(2, 0).unwrap().ch, 'X');
    t.feed(b"\r\n\x1b_Ga=p,i=7,c=4,C=1\x1b\\");
    let (g, cursor) = grid(&t);
    assert_eq!(pic(&g, 3, 1).map(|p| (p.rows, p.cols)), Some((2, 4)));
    assert_eq!(cursor.map(|c| (c.x, c.y)), Some((0, 1)));
    assert!(t.take_actions().len() == 1, "a=p without q answers");
    // Unknown image: ENOENT.
    t.feed(b"\x1b_Ga=p,i=99\x1b\\");
    assert_eq!(
        replies(&mut t),
        vec![b"\x1b_Gi=99;ENOENT:no such image\x1b\\".to_vec()]
    );
    // Delete it everywhere on the screen.
    t.feed(b"\x1b_Ga=d,d=I,i=7\x1b\\");
    let (g, _) = grid(&t);
    assert!((0..6).all(|y| g.row(y).unwrap().iter().all(|c| c.picture.is_none())));
    assert!(g.pictures().is_empty());
}

#[test]
fn kitty_too_large_is_refused() {
    let mut t = term(20, 6);
    t.feed(b"\x1b_Ga=T,f=24,s=9000,v=9000,i=3;AAAA\x1b\\");
    assert_eq!(
        replies(&mut t),
        vec![b"\x1b_Gi=3;EFBIG:picture too large\x1b\\".to_vec()]
    );
}

#[test]
fn iterm_png_inline() {
    let mut t = term(20, 6);
    let file = png(2, 1, &[255, 0, 0, 255, 0, 0, 255, 128]);
    t.feed(format!("\x1b]1337;File=inline=1;width=3:{}\x07!", encode(&file)).as_bytes());
    let (g, _) = grid(&t);
    let p = pic(&g, 0, 0).unwrap();
    assert_eq!((p.rows, p.cols), (1, 3));
    assert_eq!(g.cell(3, 0).unwrap().ch, '!');
    let picture = g.picture(p.key).unwrap();
    assert_eq!(&picture.pixels[..], &[0xFFFF_0000, 0x8000_0080]);
    // Not inline: a download, ignored.
    t.feed(format!("\x1b]1337;File=name=eA==:{}\x07", encode(&file)).as_bytes());
    let (g2, _) = grid(&t);
    assert_eq!(g2.pictures().len(), 1);
}

#[test]
fn device_attributes_say_sixel() {
    let mut t = term(20, 6);
    t.feed(b"\x1b[c");
    assert_eq!(replies(&mut t), vec![b"\x1b[?62;4;22c".to_vec()]);
    t.set_images(false);
    t.feed(b"\x1b[c");
    assert_eq!(replies(&mut t), vec![b"\x1b[?6c".to_vec()]);
}

#[test]
fn kitty_query_answers_before_device_attributes() {
    // How programs look for kitty graphics: a query, then device
    // attributes; the first answer to arrive decides.
    let mut t = term(20, 6);
    t.feed(b"\x1b[c\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[c");
    let got = replies(&mut t);
    assert_eq!(got.len(), 3);
    assert_eq!(got.first(), Some(&b"\x1b[?62;4;22c".to_vec()));
    assert!(got.get(1).is_some_and(|r| r.starts_with(b"\x1b_Gi=31;")));
    assert_eq!(got.get(2), Some(&b"\x1b[?62;4;22c".to_vec()));
    // The same inside a synchronized update.
    t.feed(b"\x1b[?2026h\x1b[c\x1b_Gi=32,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[c\x1b[?2026l");
    let got = replies(&mut t);
    assert_eq!(got.len(), 3);
    assert_eq!(got.first(), Some(&b"\x1b[?62;4;22c".to_vec()));
    assert!(got.get(1).is_some_and(|r| r.starts_with(b"\x1b_Gi=32;")));
}

#[test]
fn links_are_not_pictures() {
    let mut t = term(20, 6);
    t.feed(b"\x1bPq!10~\x1b\\");
    assert_eq!(t.link_at(0, 0), None);
}

#[test]
fn kitty_crop_is_per_placement() {
    let mut t = term(20, 6);
    // Red then green, shown cropped to the green pixel.
    let data = encode(&[255, 0, 0, 0, 255, 0]);
    t.feed(format!("\x1b_Ga=T,f=24,s=2,v=1,i=7,x=1,w=1,q=2;{data}\x1b\\").as_bytes());
    let (g, _) = grid(&t);
    let key = pic(&g, 0, 0).unwrap().key;
    assert_eq!(&g.picture(key).unwrap().pixels[..], &[0xFF00_FF00]);
    // Placed again uncropped: both pixels are still kept.
    t.feed(b"\r\n\x1b_Ga=p,i=7,q=2\x1b\\");
    let (g, _) = grid(&t);
    let whole = pic(&g, 0, 1).unwrap().key;
    assert_eq!(g.picture(whole).unwrap().width, 2);
    // Deleting the image removes the cropped placement too.
    t.feed(b"\x1b_Ga=d,d=i,i=7\x1b\\");
    let (g, _) = grid(&t);
    assert!(g.pictures().is_empty());
}
