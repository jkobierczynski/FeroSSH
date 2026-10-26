//! Automatic reconnect: when a pane whose SSH connection dropped tries
//! again. The waits grow from a second to half a minute, so a short blip
//! reconnects at once and a host that is down for a while is not
//! hammered. After a quarter of an hour without getting through, the pane
//! stops and waits for Enter, as after any other disconnect.

use std::time::{Duration, Instant};

/// The waits before each attempt; the last one repeats.
const WAITS: [Duration; 6] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
    Duration::from_secs(8),
    Duration::from_secs(15),
    Duration::from_secs(30),
];

/// How long to keep trying after the drop.
pub const GIVE_UP: Duration = Duration::from_secs(15 * 60);

/// One pane's reconnect after a drop.
#[derive(Debug, Clone)]
pub struct Retry {
    /// When the connection dropped.
    since: Instant,
    /// Attempts made so far.
    attempts: u32,
    /// When the next attempt is due; `None` while one is under way.
    due: Option<Instant>,
}

/// What [`Retry::schedule`] decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Next {
    /// Try again after this wait.
    In(Duration),
    /// Tried long enough.
    GiveUp,
}

impl Retry {
    pub fn new(now: Instant) -> Self {
        Self {
            since: now,
            attempts: 0,
            due: None,
        }
    }

    /// After the drop or a failed attempt: when to try next.
    pub fn schedule(&mut self, now: Instant) -> Next {
        if now.saturating_duration_since(self.since) >= GIVE_UP {
            self.due = None;
            return Next::GiveUp;
        }
        let i = (self.attempts as usize).min(WAITS.len() - 1);
        let wait = WAITS.get(i).copied().unwrap_or(GIVE_UP);
        self.due = Some(now + wait);
        Next::In(wait)
    }

    /// Whether an attempt is due at `now`.
    pub fn is_due(&self, now: Instant) -> bool {
        self.due.is_some_and(|d| d <= now)
    }

    /// Whether the pane waits for its next attempt (rather than being in
    /// one).
    pub fn waiting(&self) -> bool {
        self.due.is_some()
    }

    /// An attempt starts.
    pub fn begin(&mut self) {
        self.attempts += 1;
        self.due = None;
    }

    pub fn attempts(&self) -> u32 {
        self.attempts
    }

    /// How long ago the connection dropped.
    pub fn down_for(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.since)
    }
}

/// The line a pane shows while it waits for its next attempt.
pub fn waiting_text(wait: Duration, attempts: u32) -> String {
    let secs = wait.as_secs().max(1);
    let attempt = if attempts == 0 {
        String::new()
    } else {
        format!(
            " ({attempts} attempt{} so far)",
            if attempts == 1 { "" } else { "s" }
        )
    };
    format!("\x1b[0;90m[Reconnecting in {secs} s{attempt}. Enter tries now, Esc stops.]\x1b[0m\r\n")
}

/// The line a pane shows when it stops trying.
pub fn gave_up_text(down: Duration) -> String {
    format!(
        "\x1b[0;90m[Could not reconnect for {} minutes. Press Enter to reconnect.]\x1b[0m\r\n",
        down.as_secs() / 60
    )
}

#[cfg(test)]
#[allow(clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn waits_grow_then_repeat() {
        let t0 = Instant::now();
        let mut r = Retry::new(t0);
        let mut now = t0;
        let mut waits = Vec::new();
        for _ in 0..8 {
            let Next::In(wait) = r.schedule(now) else {
                panic!("gave up early");
            };
            assert!(r.waiting());
            assert!(!r.is_due(now));
            now += wait;
            assert!(r.is_due(now));
            r.begin();
            assert!(!r.waiting() && !r.is_due(now));
            waits.push(wait.as_secs());
        }
        assert_eq!(waits, [1, 2, 4, 8, 15, 30, 30, 30]);
        assert_eq!(r.attempts(), 8);
    }

    #[test]
    fn gives_up_after_a_quarter_of_an_hour() {
        let t0 = Instant::now();
        let mut r = Retry::new(t0);
        assert!(matches!(
            r.schedule(t0 + GIVE_UP - Duration::from_secs(1)),
            Next::In(_)
        ));
        assert_eq!(r.schedule(t0 + GIVE_UP), Next::GiveUp);
        assert!(!r.waiting());
        assert!(gave_up_text(r.down_for(t0 + GIVE_UP)).contains("15 minutes"));
    }

    #[test]
    fn texts() {
        let first = waiting_text(Duration::from_secs(1), 0);
        assert!(first.contains("Reconnecting in 1 s."), "{first}");
        assert!(waiting_text(Duration::from_secs(4), 1).contains("(1 attempt so far)"));
        assert!(waiting_text(Duration::from_secs(4), 3).contains("(3 attempts so far)"));
    }
}
