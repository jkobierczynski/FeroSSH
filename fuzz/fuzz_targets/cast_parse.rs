//! Feeds arbitrary files to the session recording reader (Session > Play a
//! recording opens `.cast` files from anywhere) and plays what it read.
//! It must not panic, must keep its limits, and what it reads must come
//! back the same when written out again.

#![no_main]

use std::time::{Duration, Instant};

use ferossh_term::cast::{
    Cast, CastEvent, CastHeader, CastWriter, MAX_COLS, MAX_EVENTS, MAX_ROWS, Play, Player,
};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(cast) = Cast::parse(data) else {
        return;
    };
    assert!((1..=MAX_COLS).contains(&cast.cols) && (1..=MAX_ROWS).contains(&cast.rows));
    assert!(cast.events.len() <= MAX_EVENTS);
    let mut last = 0.0;
    for (t, e) in &cast.events {
        assert!(t.is_finite() && *t >= last, "{t} after {last}");
        last = *t;
        if let CastEvent::Resize(c, r) = e {
            assert!((1..=MAX_COLS).contains(c) && (1..=MAX_ROWS).contains(r));
        }
    }

    // Written out and read again, the same events.
    let header = CastHeader {
        cols: cast.cols,
        rows: cast.rows,
        timestamp: 0,
        title: cast.title.clone().unwrap_or_default(),
    };
    let mut w = CastWriter::new(Vec::new(), &header).expect("writing to memory");
    for (t, e) in &cast.events {
        let at = Duration::from_secs_f64(*t);
        match e {
            CastEvent::Output(s) => w.output_at(at, s.as_bytes()),
            CastEvent::Resize(c, r) => w.resize_at(at, *c, *r),
        }
        .expect("writing to memory");
    }
    let file = w.finish().expect("writing to memory");
    let again = Cast::parse(&file).expect("its own recording");
    let kept = |c: &Cast| -> Vec<CastEvent> {
        c.events
            .iter()
            .map(|(_, e)| e.clone())
            .filter(|e| !matches!(e, CastEvent::Output(s) if s.is_empty()))
            .collect()
    };
    assert_eq!(kept(&cast), kept(&again));
    assert_eq!(
        (cast.cols, cast.rows, &cast.title),
        (again.cols, again.rows, &again.title)
    );
    assert_eq!(again.skipped, 0);

    // Played, with the controls a viewer would use, driven by the input's
    // last bytes.
    let total = cast.events.len();
    let t0 = Instant::now();
    let mut player = Player::new(cast, t0);
    let mut played = 0;
    let mut now = t0;
    for &b in data.iter().rev().take(16) {
        now += Duration::from_millis(u64::from(b) * 50);
        match b % 6 {
            0 => player.toggle_pause(now),
            1 => player.change_speed(b & 0x80 != 0, now),
            2 => player.seek(player.position(now) - f64::from(b), now),
            3 => player.seek(player.position(now) + f64::from(b), now),
            _ => {}
        }
        for p in player.due(now) {
            match p {
                Play::Reset(..) => played = 0,
                Play::Output(_) | Play::Resize(..) => played += 1,
            }
        }
        assert!(played <= total);
        if let Some(wait) = player.wait(now) {
            assert!(wait.as_secs_f64().is_finite());
        }
    }
});
