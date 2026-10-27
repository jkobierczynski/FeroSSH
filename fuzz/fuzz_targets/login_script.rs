//! Runs arbitrary login scripts against arbitrary host output in arbitrary
//! chunks, with time passing and secrets typed in between. The runner is
//! the app's own (`crates/app/src/login_script.rs`, compiled in here: the
//! app is a binary, not a library). Nothing may panic, and what it types
//! at the host is always the script's next `send` or secret, in order:
//! host output can only make it wait, never type something else.

#![no_main]

use std::time::{Duration, Instant};

use ferossh_config::login::Step;
use libfuzzer_sys::fuzz_target;

#[allow(dead_code)]
#[path = "../../crates/app/src/login_script.rs"]
mod login_script;

use login_script::{Action, Runner};

/// The runner shows a timed-out `expect` through the app's banner filter;
/// what it shows does not matter here.
mod ssh {
    pub fn sanitize(text: &str) -> String {
        text.to_owned()
    }
}

const SECRET: &str = "s3cret";

fuzz_target!(|data: &[u8]| {
    // The script, one step per line, then a NUL, then what the host prints.
    let (script, host) = match data.iter().position(|&b| b == 0) {
        Some(at) => (&data[..at], &data[at + 1..]),
        None => (data, &[][..]),
    };
    let steps: Vec<Step> = String::from_utf8_lossy(script)
        .lines()
        .filter_map(|l| Step::parse(l).ok())
        .take(16)
        .collect();
    let mut expected = steps.iter().filter_map(|s| match s {
        Step::Send(_) | Step::Type(_) | Step::Key(_) => s.bytes(),
        Step::Password | Step::Secret(_) => Some(format!("{SECRET}\r").into_bytes()),
        Step::Expect(_) | Step::Capture(_) | Step::Pause(_) => None,
    });

    let start = Instant::now();
    let mut now = start;
    let mut runner = Runner::new(steps.clone());
    let mut pending = runner.start(now);
    let Some((&split, host)) = host.split_first() else {
        return;
    };
    let mut chunks = host.chunks(usize::from(split % 32).max(1)).enumerate();
    loop {
        for action in std::mem::take(&mut pending) {
            match action {
                Action::Send(bytes) => {
                    let want = expected.next();
                    assert_eq!(want.as_deref(), Some(&bytes[..]), "typed out of turn");
                }
                // Half the secrets come from a store, half are typed at
                // the pane's prompt.
                Action::Fetch(_) if split & 0x80 == 0 => pending.extend(runner.secret(SECRET, now)),
                Action::Fetch(_) => {
                    pending.extend(runner.ask("Secret: ", None));
                    pending.extend(runner.prompt_input(SECRET.as_bytes(), now));
                    pending.extend(runner.prompt_input(b"\r", now));
                }
                // A captured value is one short line.
                Action::Captured(value) => {
                    assert!(value.chars().count() <= ferossh_config::login::MAX_VALUE);
                    assert!(!value.chars().any(char::is_control));
                }
                Action::Show(_) | Action::Save(..) | Action::Done => {}
            }
        }
        if !pending.is_empty() {
            continue;
        }
        let Some((i, chunk)) = chunks.next() else {
            break;
        };
        pending = runner.feed(chunk, now);
        // Now and then time passes: an expect may time out, a pause end.
        if (i + usize::from(split)) % 7 == 0 {
            now += Duration::from_secs(u64::from(chunk[0] % 64));
            pending.extend(runner.tick(now));
        }
    }
});
