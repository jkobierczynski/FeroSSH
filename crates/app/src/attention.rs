//! When a pane wants the user: the notifications a host asks for, the
//! bell, and the quiet after a stretch of output (a coding agent that
//! finished, or stopped to ask something, without saying so).
//!
//! Platform-neutral: the window feeds it times and decides what to show;
//! [`crate::toast`] puts the Windows notification up.

use std::time::{Duration, Instant};

/// Output that stops for this long has gone quiet.
pub const QUIET_AFTER: Duration = Duration::from_secs(5);
/// A stretch of output shorter than this (an echo, a prompt, a redraw
/// after a resize) is not work worth telling about when it ends.
pub const MIN_WORK: Duration = Duration::from_secs(8);
/// Windows notifications for one pane come at most this often; the badge
/// shows what came between.
pub const NOTIFY_EVERY: Duration = Duration::from_secs(10);

/// What a watched pane is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Nothing seen yet since the watch started or the user typed.
    Idle,
    /// Output is coming, since this time.
    Working(Instant),
    /// Output stopped at this time after a stretch of work.
    Quiet(Instant),
}

/// Watches one pane's output for the quiet after work.
#[derive(Clone, Debug, Default)]
pub struct Watch {
    /// When the current stretch of output began, and its last output.
    burst: Option<(Instant, Instant)>,
    /// When the last stretch long enough to count ended.
    quiet: Option<Instant>,
    /// A stretch that went quiet but was not told yet, because output
    /// came again before the next tick: how long it lasted.
    untold: Option<Duration>,
}

impl Watch {
    pub fn new() -> Self {
        Self::default()
    }

    /// The host sent output at `now` (live output only, not FeroSSH's own
    /// notices).
    pub fn output(&mut self, now: Instant) {
        self.burst = Some(match self.burst {
            Some((start, last)) if now.saturating_duration_since(last) < QUIET_AFTER => {
                (start, now)
            }
            // The stretch before went quiet between two ticks: it still
            // counts, once.
            Some(_) => {
                if let Some(worked) = self.ended() {
                    self.untold = Some(worked);
                }
                (now, now)
            }
            None => (now, now),
        });
        self.quiet = None;
    }

    /// The user typed in the pane: what comes next is an answer to that,
    /// so the stretch so far is not work that went quiet.
    pub fn typed(&mut self) {
        self.burst = None;
        self.quiet = None;
        self.untold = None;
    }

    /// Called about once a second: once, when a stretch of work has just
    /// gone quiet, how long that stretch lasted.
    pub fn tick(&mut self, now: Instant) -> Option<Duration> {
        if let Some(worked) = self.untold.take() {
            return Some(worked);
        }
        let (_, last) = self.burst?;
        if now.saturating_duration_since(last) < QUIET_AFTER {
            return None;
        }
        self.ended()
    }

    /// Ends the current stretch, quiet since its last output; how long it
    /// lasted if that was long enough to count.
    fn ended(&mut self) -> Option<Duration> {
        let (start, last) = self.burst.take()?;
        let worked = last.saturating_duration_since(start);
        if worked < MIN_WORK {
            return None;
        }
        self.quiet = Some(last);
        Some(worked)
    }

    pub fn state(&self, now: Instant) -> State {
        match (self.burst, self.quiet) {
            (Some((start, last)), _) if now.saturating_duration_since(last) < QUIET_AFTER => {
                State::Working(start)
            }
            (_, Some(at)) => State::Quiet(at),
            _ => State::Idle,
        }
    }
}

/// Spaces out the Windows notifications of one pane.
#[derive(Clone, Copy, Debug, Default)]
pub struct Throttle {
    last: Option<Instant>,
}

impl Throttle {
    /// Whether a notification may go up at `now`; if so, it counts.
    pub fn allow(&mut self, now: Instant) -> bool {
        if self
            .last
            .is_some_and(|t| now.saturating_duration_since(t) < NOTIFY_EVERY)
        {
            return false;
        }
        self.last = Some(now);
        true
    }
}

/// `d` the way a notification says it: "40 s", "3 min", "2 h 5 min".
pub fn spoken(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..60 => format!("{s} s"),
        60..3600 => format!("{} min", s / 60),
        _ if s % 3600 / 60 == 0 => format!("{} h", s / 3600),
        _ => format!("{} h {} min", s / 3600, s % 3600 / 60),
    }
}

/// The notification's heading: where, then what asked. The pane comes
/// first because Windows cuts a long heading short.
pub fn heading(what: Option<&str>, pane: &str) -> String {
    match what {
        Some(w) if !w.is_empty() && w != pane => format!("{pane} · {w}"),
        _ => pane.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(t0: Instant, s: u64) -> Instant {
        t0 + Duration::from_secs(s)
    }

    #[test]
    fn quiet_after_work_fires_once() {
        let t0 = Instant::now();
        let mut w = Watch::new();
        assert_eq!(w.tick(at(t0, 1)), None);
        // Ten seconds of output, every two seconds.
        for s in (0..=10).step_by(2) {
            w.output(at(t0, s));
        }
        assert_eq!(w.state(at(t0, 11)), State::Working(t0));
        assert_eq!(w.tick(at(t0, 12)), None);
        assert_eq!(w.tick(at(t0, 15)), Some(Duration::from_secs(10)));
        assert_eq!(w.tick(at(t0, 16)), None);
        assert_eq!(w.state(at(t0, 16)), State::Quiet(at(t0, 10)));
    }

    #[test]
    fn quiet_between_two_ticks_still_counts() {
        let t0 = Instant::now();
        let mut w = Watch::new();
        for s in 0..=10 {
            w.output(at(t0, s));
        }
        assert_eq!(w.tick(at(t0, 14)), None);
        // Output again just after five quiet seconds, before the tick.
        w.output(at(t0, 15) + Duration::from_millis(100));
        assert_eq!(w.tick(at(t0, 16)), Some(Duration::from_secs(10)));
        assert_eq!(w.tick(at(t0, 17)), None);
        assert_eq!(
            w.state(at(t0, 17)),
            State::Working(at(t0, 15) + Duration::from_millis(100))
        );
    }

    #[test]
    fn short_bursts_and_typing_do_not_count() {
        let t0 = Instant::now();
        let mut w = Watch::new();
        w.output(t0);
        w.output(at(t0, 3));
        assert_eq!(w.tick(at(t0, 9)), None);
        assert_eq!(w.state(at(t0, 9)), State::Idle);
        // Long output, then the user types: the stretch is over.
        for s in 0..10 {
            w.output(at(t0, 20 + s));
        }
        w.typed();
        assert_eq!(w.state(at(t0, 29)), State::Idle);
        w.output(at(t0, 30));
        assert_eq!(w.tick(at(t0, 36)), None);
    }

    #[test]
    fn a_gap_ends_a_stretch() {
        let t0 = Instant::now();
        let mut w = Watch::new();
        w.output(t0);
        w.output(at(t0, 4));
        // Six seconds later: a new stretch, not the old one going on.
        w.output(at(t0, 10));
        assert_eq!(w.state(at(t0, 10)), State::Working(at(t0, 10)));
        assert_eq!(w.tick(at(t0, 11)), None);
    }

    #[test]
    fn throttle_spaces_notifications() {
        let t0 = Instant::now();
        let mut t = Throttle::default();
        assert!(t.allow(t0));
        assert!(!t.allow(at(t0, 3)));
        assert!(t.allow(at(t0, 10)));
    }

    #[test]
    fn says_durations_and_headings() {
        assert_eq!(spoken(Duration::from_secs(40)), "40 s");
        assert_eq!(spoken(Duration::from_secs(185)), "3 min");
        assert_eq!(spoken(Duration::from_secs(7200)), "2 h");
        assert_eq!(spoken(Duration::from_secs(7500)), "2 h 5 min");
        assert_eq!(heading(Some("Codex"), "web-1"), "web-1 · Codex");
        assert_eq!(heading(None, "web-1"), "web-1");
        assert_eq!(heading(Some(""), "web-1"), "web-1");
    }
}
