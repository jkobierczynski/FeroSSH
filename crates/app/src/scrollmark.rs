//! The scroll position mark: a thin bar at a pane's right edge, shown only
//! while the view is scrolled back. Its length is the share of the history
//! in view, its place where that view sits.

/// Where the mark goes along a track `len` pixels long, as (offset, length)
/// from the track's start, or `None` at the live bottom (or with no
/// history). `rows` is the screen height, `history` the lines above it and
/// `back` how far the view is scrolled back. The mark is at least `min`
/// pixels long so that it stays visible in a long history.
pub fn thumb(len: i32, rows: usize, history: usize, back: usize, min: i32) -> Option<(i32, i32)> {
    if back == 0 || history == 0 || rows == 0 || len <= 0 {
        return None;
    }
    let back = back.min(history);
    let total = (history + rows) as f64;
    let size = ((f64::from(len) * rows as f64 / total).round() as i32)
        .max(min)
        .min(len);
    // The view's top line runs from 0 (oldest) to `history` (live).
    let at = (history - back) as f64 / history as f64;
    let offset = (f64::from(len - size) * at).round() as i32;
    Some((offset, size))
}

#[cfg(test)]
mod tests {
    use super::thumb;

    #[test]
    fn hidden_at_the_bottom_and_without_history() {
        assert_eq!(thumb(400, 24, 1000, 0, 8), None);
        assert_eq!(thumb(400, 24, 0, 0, 8), None);
        assert_eq!(thumb(0, 24, 1000, 5, 8), None);
    }

    #[test]
    fn size_is_the_share_in_view() {
        // 24 of 48 lines in view: half the track.
        assert_eq!(thumb(480, 24, 24, 24, 8), Some((0, 240)));
        // One line back of 24 history: just above the bottom.
        assert_eq!(thumb(480, 24, 24, 1, 8), Some((230, 240)));
    }

    #[test]
    fn top_of_the_history_is_the_top_of_the_track() {
        assert_eq!(thumb(400, 24, 10_000, 10_000, 8).map(|t| t.0), Some(0));
        // Scrolled further than the history (it was trimmed): still the top.
        assert_eq!(thumb(400, 24, 100, 500, 8).map(|t| t.0), Some(0));
    }

    #[test]
    fn long_history_keeps_a_minimum_length() {
        // 24 of 100 024 lines would be a fraction of a pixel; one line
        // back sits at the very bottom.
        assert_eq!(thumb(400, 24, 100_000, 1, 8), Some((392, 8)));
    }
}
