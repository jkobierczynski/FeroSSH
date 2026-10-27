//! Feeds arbitrary bytes, as a hostile host would print them, to an
//! auto-response watch in arbitrary chunks, with arbitrary `When` texts.
//! Nothing may panic, only the user's snippets fire, never more than
//! the burst limit at one moment, and data split anywhere fires the same
//! as data fed whole. Each text is also tried as a snippet's `@capture`
//! pattern on the output, whose value must stay one short line. The same
//! bytes, as typing, go to a keystroke recorder, which must stay within
//! its limits.

#![no_main]

use ferossh_config::login::{MAX_VALUE, capture};
use ferossh_config::record::Recorder;
use ferossh_config::trigger::{BURST, Fire, OutputText, Watch};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&split, rest)) = data.split_first() else {
        return;
    };
    // Up to four `When` texts, then the stream, split at 0xff bytes.
    let mut parts = rest.splitn(5, |&b| b == 0xff);
    let mut texts = Vec::new();
    for _ in 0..4 {
        if let Some(p) = parts.next() {
            texts.push(String::from_utf8_lossy(p).into_owned());
        }
    }
    let stream = parts.next().unwrap_or_default();
    let whens: Vec<(usize, &str)> = texts
        .iter()
        .enumerate()
        .filter(|(_, t)| t.chars().count() <= ferossh_config::login::MAX_TEXT)
        .map(|(i, t)| (i, t.as_str()))
        .collect();

    let mut whole = Watch::default();
    let all = whole.feed(stream, &whens, 0);
    assert!(all.len() <= BURST + 1);

    let mut chunked = Watch::default();
    let mut fired = Vec::new();
    for chunk in stream.chunks(usize::from(split).max(1)) {
        fired.extend(chunked.feed(chunk, &whens, 0));
    }
    assert_eq!(fired, all);
    for f in &fired {
        if let Fire::Snippet(i) = f {
            assert!(whens.iter().any(|(n, _)| n == i));
        }
    }

    let mut seen = String::new();
    OutputText::default().feed(stream, &mut seen);
    for t in &texts {
        if let Some((end, value)) = capture(&seen, t) {
            assert!(end <= seen.len());
            assert!(value.chars().count() <= MAX_VALUE);
            assert!(!value.chars().any(char::is_control));
        }
    }

    let mut r = Recorder::default();
    for chunk in stream.chunks(usize::from(split).max(1)) {
        r.input(chunk);
    }
    let rec = r.finish();
    assert!(rec.lines.len() <= ferossh_config::record::MAX_LINES);
    assert_eq!(rec.hidden, rec.lines.iter().filter(|l| *l == "@password").count());
});
