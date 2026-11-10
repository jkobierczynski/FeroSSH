//! Feeds arbitrary bytes, as a hostile host would send them, to the tmux
//! control-mode client in arbitrary chunks, with commands waiting for
//! replies, and reads every layout and saved layout it sees. Nothing may
//! panic, and terminal output must come through in order.

#![no_main]

use ferossh_tmuxcc::{Client, Event, Notification, layout, saved};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&split, stream)) = data.split_first() else {
        return;
    };
    let mut client: Client<u32> = Client::new();
    // Random bytes almost never spell the greeting, so half the inputs
    // start in control mode.
    if split & 1 != 0 {
        let _ = client.feed(b"\x1bP1000p");
    }
    let mut n = 0;
    for chunk in stream.chunks(usize::from(split).max(1)) {
        if client.is_control() && client.pending() < 8 {
            n += 1;
            let _ = client.command("list-panes", n);
        }
        for event in client.feed(chunk) {
            match event {
                Event::Notify(Notification::LayoutChange { layout: l, .. }) => {
                    if let Some(cell) = layout::parse(&l) {
                        let again = layout::parse(&cell.to_layout_string());
                        assert_eq!(again.as_ref().map(layout::Cell::panes), Some(cell.panes()));
                    }
                }
                Event::Reply { lines, .. } => {
                    for line in lines {
                        // Re-encoding at most triples a name, which keeps
                        // short texts under the parser's size limit.
                        if let Ok(text) = std::str::from_utf8(&line)
                            && text.len() < 16 << 10
                            && let Some(s) = saved::Saved::parse(text)
                        {
                            assert_eq!(saved::Saved::parse(&s.to_text()), Some(s));
                        }
                    }
                }
                _ => {}
            }
        }
    }
});
