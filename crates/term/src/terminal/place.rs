//! Showing the pictures [`crate::images::Scanner`] takes out of the
//! output: decoding them within limits and marking the cells they cover.

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::{Cell as TermCell, Hyperlink};
use alacritty_terminal::vte::ansi::Handler;
use ferossh_images::kitty::Command;
use ferossh_images::{Error, Limits, iterm};

use super::{MAX_COLS, MAX_ROWS, Terminal};
use crate::images::{MARKER_ID, Marker, Piece, Scanner, Sequence, Store};

/// Replies to kitty graphics commands kept between two `take_actions`.
const MAX_REPLIES: usize = 64;

/// A pane's inline images, while they are on.
#[derive(Debug, Default)]
pub(super) struct Images {
    scanner: Scanner,
    pub store: Store,
    pub replies: Vec<Vec<u8>>,
}

/// Where the cursor goes after a picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum After {
    /// To the picture's left edge on the line below it (Sixel).
    Below,
    /// Just right of the picture's last row (kitty, iTerm2).
    Right,
    /// Nowhere (kitty's `C=1`).
    Stay,
}

impl Terminal {
    /// Splits `bytes` into text for the emulator and pictures, and handles
    /// each in order.
    pub(super) fn feed_with_images(&mut self, bytes: &[u8]) {
        let Some(images) = self.images.as_mut() else {
            self.feed_text(bytes);
            return;
        };
        for piece in images.scanner.scan(bytes) {
            match piece {
                Piece::Text(range) => {
                    if let Some(text) = bytes.get(range) {
                        self.feed_text(text);
                    }
                }
                Piece::Held(held) => self.feed_text(&held),
                Piece::Image(seq) => self.show(seq),
            }
        }
    }

    fn show(&mut self, seq: Sequence) {
        let limits = Limits::DEFAULT;
        match seq {
            Sequence::Sixel(body) => {
                if let Ok(p) = ferossh_images::sixel::decode(&body, &limits) {
                    let cells = self.cells_for(p.width, p.height);
                    self.add_and_place(p, cells, Some(After::Below));
                }
            }
            Sequence::Iterm(body) => {
                let (file, payload) = iterm::parse(&body);
                if !file.inline {
                    return;
                }
                if let Ok(p) = iterm::decode(payload, &limits) {
                    let cells = self.iterm_cells(&file, p.width, p.height);
                    self.add_and_place(p, cells, Some(After::Right));
                }
            }
            Sequence::Kitty(body) => self.kitty(&body),
            Sequence::TooLong(keys) => {
                if let Some(images) = &mut self.images {
                    images.store.chunks = None;
                }
                if let Some(keys) = keys {
                    let cmd = Command::parse(&keys);
                    self.reply(&cmd, Err(Error::TooLarge));
                }
            }
        }
    }

    /// One kitty graphics command, joining chunked transmissions.
    fn kitty(&mut self, body: &[u8]) {
        let Some(images) = &mut self.images else {
            return;
        };
        let (keys, payload) = match body.iter().position(|&b| b == b';') {
            Some(at) => (
                body.get(..at).unwrap_or(&[]),
                body.get(at + 1..).unwrap_or(&[]),
            ),
            None => (body, &[][..]),
        };
        let cmd = Command::parse(keys);
        let (cmd, payload) = match images.store.chunks.take() {
            Some((first, _)) if first.is_empty() => {
                // The rest of a transmission already refused.
                if cmd.more {
                    images.store.chunks = Some((Vec::new(), Vec::new()));
                }
                return;
            }
            Some((first, mut data)) => {
                if data.len() + payload.len() > Limits::DEFAULT.max_encoded {
                    let first = Command::parse(&first);
                    if cmd.more {
                        // Swallow the rest of it.
                        images.store.chunks = Some((Vec::new(), Vec::new()));
                    }
                    drop(data);
                    self.reply(&first, Err(Error::TooLarge));
                    return;
                }
                data.extend_from_slice(payload);
                if cmd.more {
                    images.store.chunks = Some((first, data));
                    return;
                }
                let mut first_cmd = Command::parse(&first);
                first_cmd.quiet = first_cmd.quiet.max(cmd.quiet);
                (first_cmd, data)
            }
            None if cmd.more => {
                images.store.chunks = Some((keys.to_vec(), payload.to_vec()));
                return;
            }
            None => (cmd, payload.to_vec()),
        };
        let limits = Limits::DEFAULT;
        match cmd.action {
            b'q' => {
                let result = cmd.decode(&payload, &limits).map(|_| ());
                self.reply(&cmd, result);
            }
            b't' | b'T' => {
                let decoded = cmd.decode(&payload, &limits).and_then(|p| crop(&cmd, p));
                match decoded {
                    Ok(p) => {
                        let show = cmd.action == b'T';
                        let cells = self.kitty_cells(&cmd, p.width, p.height);
                        let key = self.add_and_place(p, cells, after(&cmd, show));
                        if let (Some(images), Some(key)) = (&mut self.images, key)
                            && cmd.id != 0
                        {
                            images.store.set_kitty(cmd.id, key);
                        }
                        self.reply(&cmd, Ok(()));
                    }
                    Err(e) => self.reply(&cmd, Err(e)),
                }
            }
            b'p' => {
                let found = self
                    .images
                    .as_ref()
                    .and_then(|i| i.store.kitty(cmd.id))
                    .cloned();
                let Some(picture) = found else {
                    self.reply(&cmd, Err(Error::NotFound));
                    return;
                };
                let cropped = cmd.src_x | cmd.src_y | cmd.src_w | cmd.src_h != 0;
                if cropped {
                    let whole = ferossh_images::Picture {
                        width: picture.width,
                        height: picture.height,
                        pixels: picture.pixels.to_vec(),
                    };
                    match crop(&cmd, whole) {
                        Ok(p) => {
                            let cells = self.kitty_cells(&cmd, p.width, p.height);
                            self.add_and_place(p, cells, after(&cmd, true));
                        }
                        Err(e) => {
                            self.reply(&cmd, Err(e));
                            return;
                        }
                    }
                } else {
                    let cells = self.kitty_cells(&cmd, picture.width, picture.height);
                    let after = if cmd.cursor_stays {
                        After::Stay
                    } else {
                        After::Right
                    };
                    self.place(picture.key, cells, after);
                }
                self.reply(&cmd, Ok(()));
            }
            b'd' => self.kitty_delete(&cmd),
            _ => self.reply(&cmd, Err(Error::Unsupported)),
        }
    }

    /// `a=d`: lower case removes pictures from the screen, upper case
    /// forgets their data too.
    fn kitty_delete(&mut self, cmd: &Command) {
        let Some(images) = &mut self.images else {
            return;
        };
        let free = cmd.delete.is_ascii_uppercase();
        let which = match cmd.delete.to_ascii_lowercase() {
            b'a' => None,
            b'i' => match images.store.kitty_key(cmd.id) {
                Some(key) => Some(key),
                None => return,
            },
            // Deleting by position, z-index or animation frame: nothing
            // FeroSSH keeps apart, so nothing to do.
            _ => return,
        };
        if free {
            images
                .store
                .free_kitty(if which.is_some() { Some(cmd.id) } else { None });
        }
        let screen = self.term.screen_lines();
        let cols = self.term.columns();
        let grid = self.term.grid_mut();
        for line in 0..screen {
            for col in 0..cols {
                let cell = &mut grid[Line(line as i32)][Column(col)];
                let hit = cell
                    .hyperlink()
                    .filter(|h| h.id() == MARKER_ID)
                    .and_then(|h| Marker::parse(h.uri()))
                    .is_some_and(|m| which.is_none_or(|k| k == m.key));
                if hit {
                    *cell = TermCell::default();
                }
            }
        }
    }

    fn reply(&mut self, cmd: &Command, result: Result<(), Error>) {
        if let (Some(images), Some(reply)) = (&mut self.images, cmd.reply(result))
            && images.replies.len() < MAX_REPLIES
        {
            images.replies.push(reply);
        }
    }

    /// The pixel size of a cell, at least one pixel each way.
    fn cell_px(&self) -> (u32, u32) {
        (
            u32::from(self.cell_px.0).max(1),
            u32::from(self.cell_px.1).max(1),
        )
    }

    /// Cells a `w` x `h` picture covers at its own size.
    fn cells_for(&self, w: u32, h: u32) -> (usize, usize) {
        let (cw, ch) = self.cell_px();
        (w.div_ceil(cw) as usize, h.div_ceil(ch) as usize)
    }

    /// Cells for a kitty picture: `c` and `r` if given (one of them
    /// keeps the shape), else its own size.
    fn kitty_cells(&self, cmd: &Command, w: u32, h: u32) -> (usize, usize) {
        let (cw, ch) = self.cell_px();
        let (w64, h64) = (u64::from(w.max(1)), u64::from(h.max(1)));
        match (cmd.cols, cmd.rows) {
            (0, 0) => self.cells_for(w, h),
            (c, 0) => {
                let px_h = u64::from(c) * u64::from(cw) * h64 / w64;
                (c as usize, px_h.div_ceil(u64::from(ch)) as usize)
            }
            (0, r) => {
                let px_w = u64::from(r) * u64::from(ch) * w64 / h64;
                (px_w.div_ceil(u64::from(cw)) as usize, r as usize)
            }
            (c, r) => (c as usize, r as usize),
        }
    }

    /// Cells for an iTerm2 picture, from its width and height arguments.
    fn iterm_cells(&self, file: &iterm::File, w: u32, h: u32) -> (usize, usize) {
        let (cw, ch) = (f64::from(self.cell_px().0), f64::from(self.cell_px().1));
        let (pw, ph) = (f64::from(w.max(1)), f64::from(h.max(1)));
        let side = |e: iterm::Extent, cell: f64, cells: usize| -> Option<f64> {
            match e {
                iterm::Extent::Auto => None,
                iterm::Extent::Cells(n) => Some(f64::from(n) * cell),
                iterm::Extent::Pixels(n) => Some(f64::from(n)),
                iterm::Extent::Percent(p) => Some(cells as f64 * cell * f64::from(p) / 100.0),
            }
        };
        let tw = side(file.width, cw, self.cols());
        let th = side(file.height, ch, self.rows());
        let (tw, th) = match (tw, th) {
            (None, None) => (pw, ph),
            (Some(tw), None) => (tw, tw * ph / pw),
            (None, Some(th)) => (th * pw / ph, th),
            (Some(tw), Some(th)) if file.keep_shape => {
                let scale = (tw / pw).min(th / ph);
                (pw * scale, ph * scale)
            }
            (Some(tw), Some(th)) => (tw, th),
        };
        (
            (tw / cw).ceil().max(1.0) as usize,
            (th / ch).ceil().max(1.0) as usize,
        )
    }

    /// Keeps `picture` and shows it; returns its key.
    fn add_and_place(
        &mut self,
        picture: ferossh_images::Picture,
        cells: (usize, usize),
        after: Option<After>,
    ) -> Option<u32> {
        let images = self.images.as_mut()?;
        let key = images.store.add(picture).key;
        if let Some(after) = after {
            self.place(key, cells, after);
        }
        Some(key)
    }

    /// Lays picture `key` over `cols` x `rows` cells from the cursor,
    /// scrolling as needed, and moves the cursor `after` it.
    fn place(&mut self, key: u32, (cols, rows): (usize, usize), after: After) {
        // Output held back by a synchronized update goes in first, so the
        // picture lands where the host meant.
        if self.sync_deadline().is_some() {
            self.flush_sync();
        }
        let (cols, rows) = (cols.clamp(1, MAX_COLS), rows.clamp(1, MAX_ROWS));
        let width = self.term.columns();
        let screen = self.term.screen_lines();
        let start = self.term.grid().cursor.point;
        let c0 = start.column.0.min(width.saturating_sub(1));
        let shown = cols.min(width - c0);
        // At most two screens of it: more could not be seen at once, and
        // a host asking for a huge size should not cost more than
        // clearing the screen does. The rest is cut off, not squeezed.
        let laid = rows.min(screen * 2);
        for j in 0..laid {
            let line = if after == After::Stay {
                let l = start.line.0 + j as i32;
                if l >= screen as i32 {
                    break;
                }
                Line(l)
            } else {
                if j > 0 {
                    self.term.linefeed();
                }
                self.term.grid().cursor.point.line
            };
            let marker = Marker {
                key,
                row: j as u16,
                rows: rows as u16,
                cols: cols as u16,
                left: c0 as u16,
            };
            // One cell carries the link and the rest share it.
            let mut template = TermCell::default();
            template.set_hyperlink(Some(Hyperlink::new(Some(MARKER_ID), marker.uri())));
            let row = &mut self.term.grid_mut()[line];
            for i in c0..c0 + shown {
                row[Column(i)] = template.clone();
            }
        }
        if after == After::Below {
            self.term.linefeed();
        }
        let cursor = &mut self.term.grid_mut().cursor;
        match after {
            After::Below => {
                cursor.point.column = Column(c0);
                cursor.input_needs_wrap = false;
            }
            After::Right => {
                let col = c0 + shown;
                if col >= width {
                    cursor.point.column = Column(width - 1);
                    cursor.input_needs_wrap = true;
                } else {
                    cursor.point.column = Column(col);
                    cursor.input_needs_wrap = false;
                }
            }
            After::Stay => {}
        }
        self.repair();
    }

    /// The part of a picture `cell`, in column `x`, shows, if it is
    /// part of one still kept.
    pub(super) fn picture_cell(
        &self,
        cell: &TermCell,
        x: usize,
    ) -> Option<(ferossh_render::PictureCell, &ferossh_render::Picture)> {
        let images = self.images.as_ref()?;
        let link = cell.hyperlink()?;
        if link.id() != MARKER_ID {
            return None;
        }
        let at = Marker::parse(link.uri())?.at(x)?;
        Some((at, images.store.get(at.key)?))
    }

    /// Turns inline images on or off. Off, the sequences go to the
    /// emulator, which ignores them, and kept pictures are dropped.
    pub fn set_images(&mut self, on: bool) {
        match (on, self.images.is_some()) {
            (true, false) => self.images = Some(Images::default()),
            (false, true) => self.images = None,
            _ => {}
        }
    }

    pub fn images_on(&self) -> bool {
        self.images.is_some()
    }

    /// Replies to kitty graphics commands since the last call.
    pub(super) fn take_image_replies(&mut self) -> Vec<Vec<u8>> {
        self.images
            .as_mut()
            .map(|i| std::mem::take(&mut i.replies))
            .unwrap_or_default()
    }
}

/// Where the cursor goes after a kitty picture, if it is shown.
fn after(cmd: &Command, show: bool) -> Option<After> {
    show.then_some(if cmd.cursor_stays {
        After::Stay
    } else {
        After::Right
    })
}

/// The part of `p` kitty's `x`, `y`, `w` and `h` ask for.
fn crop(cmd: &Command, p: ferossh_images::Picture) -> Result<ferossh_images::Picture, Error> {
    if cmd.src_x | cmd.src_y | cmd.src_w | cmd.src_h == 0 {
        return Ok(p);
    }
    let w = if cmd.src_w == 0 { p.width } else { cmd.src_w };
    let h = if cmd.src_h == 0 { p.height } else { cmd.src_h };
    p.crop(cmd.src_x, cmd.src_y, w, h).ok_or(Error::Invalid)
}
