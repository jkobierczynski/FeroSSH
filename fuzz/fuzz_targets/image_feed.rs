//! Feeds arbitrary host output to a terminal with inline images on, in
//! arbitrary chunks, with resizes and scrolling in between. It must never
//! panic, every picture cell must point at a picture the grid has, and
//! replies must be plain ASCII escape sequences.

#![no_main]

use ferossh_render::Grid;
use ferossh_term::{Action, Settings, Terminal};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&split, stream)) = data.split_first() else {
        return;
    };
    let settings = Settings {
        images: true,
        ..Settings::new(50)
    };
    let mut t = Terminal::new(20, 6, &settings);
    // Cells of 1 to 8 pixels: small pictures cover many cells.
    t.set_cell_size(u32::from(split % 8) + 1, u32::from(split / 32) + 1);
    let mut grid = Grid::new(1, 1);
    for (i, chunk) in stream.chunks(usize::from(split % 64).max(1)).enumerate() {
        t.feed(chunk);
        for action in t.take_actions() {
            if let Action::Reply(bytes) = action {
                assert!(
                    bytes
                        .iter()
                        .all(|b| (0x20..0x7f).contains(b) || *b == 0x1b || *b == 0x07),
                    "reply {bytes:?}"
                );
            }
        }
        match i % 7 {
            3 => t.resize(10 + i % 23, 3 + i % 9),
            5 => t.scroll_lines(2),
            6 => {
                t.scroll_to_bottom();
                if split & 0x80 != 0 {
                    t.set_images(i % 2 == 0);
                }
            }
            _ => {}
        }
        let _ = t.draw_into(&mut grid, true);
        for y in 0..grid.rows() {
            for cell in grid.row(y).unwrap_or(&[]) {
                if let Some(at) = cell.picture {
                    assert!(grid.picture(at.key).is_some());
                    assert!(at.row < at.rows && at.col < at.cols);
                }
            }
        }
    }
});
