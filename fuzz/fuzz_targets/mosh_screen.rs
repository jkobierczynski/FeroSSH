//! Feeds arbitrary terminal output, as a hostile Mosh server's diffs
//! would hold it, to the worker's screen model in arbitrary chunks, with
//! resizes in between, and renders every state to every later one.
//! Nothing may panic, screens stay their size, and rendering output stays
//! in proportion to the screen.

#![no_main]

use ferossh_mosh::screen::{Screen, render};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&split, stream)) = data.split_first() else {
        return;
    };
    let mut s = Screen::new(20, 6);
    let mut states = vec![s.clone()];
    for (i, chunk) in stream.chunks(usize::from(split).max(1)).enumerate() {
        s.apply(chunk);
        if i % 7 == 6 {
            let w = usize::from(chunk.first().copied().unwrap_or(1) % 40);
            let h = usize::from(chunk.last().copied().unwrap_or(1) % 12);
            s.resize(w, h);
        }
        assert_eq!(s.rows.len(), s.height());
        assert!(s.rows.iter().all(|r| r.cells.len() == s.width()));
        if states.len() < 8 {
            states.push(s.clone());
        }
    }
    states.push(s);
    for (i, last) in states.iter().enumerate() {
        for next in &states[i..] {
            for init in [true, false] {
                let out = render(init, last, next);
                // Each cell is at most its 32 bytes of text and a
                // rendition change and a link; plus titles and modes.
                let cells = next.width() * next.height();
                assert!(out.len() <= cells * 200 + 64 * 1024, "{}", out.len());
                let mut shown = last.clone();
                shown.resize(next.width(), next.height());
                shown.apply(out.as_bytes());
            }
        }
    }
});
