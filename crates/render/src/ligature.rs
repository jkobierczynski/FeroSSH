//! Where programming ligatures go, worked out from shaping results.
//!
//! Fonts such as Fira Code, JetBrains Mono and Cascadia Code join `->`,
//! `!=` or `===` by swapping in other glyphs when the characters are
//! shaped together (OpenType `calt` and `liga`). The renderer still puts
//! one glyph per cell: it shapes a run of neighbouring cells that share
//! their style and colours, compares the result with each character's own
//! glyph, and draws only the cells whose glyphs changed as one piece.
//! Everything else stays a plain cell glyph, so the grid never moves.
//!
//! This module is the platform-neutral part: which cells form a run, and
//! which parts of a shaped run differ from the plain glyphs.

use std::ops::Range;

/// The widest ligature drawn as one piece, in cells. Longer changed
/// stretches (a row of `=` in Fira Code) are drawn cell by cell.
pub const MAX_CELLS: usize = 16;

/// Whether a character may join a ligature: printable ASCII other than
/// the space. Every programming ligature is made of these, and every
/// terminal font has its own glyph for them, so no fallback font is
/// involved.
pub fn joins(ch: char) -> bool {
    ('!'..='~').contains(&ch)
}

/// The runs of cells to shape together: the longest stretches of at least
/// two neighbouring cells with the same `Some` key. `key` gives `None` for
/// a cell that never joins (a space, a wide character, the cursor).
pub fn runs<K: PartialEq>(
    len: usize,
    mut key: impl FnMut(usize) -> Option<K>,
) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut current: Option<K> = None;
    for x in 0..=len {
        let k = if x < len { key(x) } else { None };
        if k.is_none() || k != current {
            if current.is_some() && x - start >= 2 {
                out.push(start..x);
            }
            start = x;
        }
        current = k;
    }
    out
}

/// One piece of a shaped run that is drawn as a whole.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Piece {
    /// The first character (cell) of the piece, from the start of the run.
    pub start: usize,
    /// The glyphs to draw.
    pub glyphs: Box<[u16]>,
    /// How many cells each glyph advances; 0 for the extra glyphs of a
    /// cluster.
    pub cells: Box<[u8]>,
}

impl Piece {
    /// The piece's width in cells.
    pub fn width(&self) -> usize {
        self.cells.iter().map(|c| usize::from(*c)).sum()
    }
}

/// The pieces of a shaped run whose glyphs differ from the characters' own.
///
/// `plain` holds each character's own glyph, `glyphs` the shaped glyphs,
/// and `cluster_map` the first shaped glyph of each character's cluster,
/// as DirectWrite's `GetGlyphs` gives them. Neighbouring changed clusters
/// form one piece; pieces wider than [`MAX_CELLS`] are left out.
pub fn pieces(plain: &[u16], glyphs: &[u16], cluster_map: &[u16]) -> Vec<Piece> {
    let n = plain.len().min(cluster_map.len());
    let map = cluster_map.get(..n).unwrap_or_default();
    // Each cluster: its characters and its glyphs.
    let mut clusters: Vec<(Range<usize>, Range<usize>)> = Vec::new();
    let mut i = 0;
    while let Some(&first) = map.get(i) {
        let len = map
            .get(i..)
            .unwrap_or_default()
            .iter()
            .take_while(|g| **g == first)
            .count();
        let j = i + len;
        let g = usize::from(first);
        let g_end = map.get(j).map_or(glyphs.len(), |g| usize::from(*g));
        if g > g_end || g_end > glyphs.len() {
            // Not a map DirectWrite makes; draw the run cell by cell.
            return Vec::new();
        }
        clusters.push((i..j, g..g_end));
        i = j;
    }

    let changed = |(chars, gl): &(Range<usize>, Range<usize>)| {
        chars.len() != 1 || gl.len() != 1 || glyphs.get(gl.start) != plain.get(chars.start)
    };
    let mut out = Vec::new();
    for part in clusters.chunk_by(|a, b| changed(a) == changed(b)) {
        let (Some(head), Some(tail)) = (part.first(), part.last()) else {
            continue;
        };
        if !changed(head) {
            continue;
        }
        let start = head.0.start;
        if tail.0.end - start > MAX_CELLS {
            continue;
        }
        let mut gl = Vec::new();
        let mut cells = Vec::new();
        for (chars, g) in part {
            for (nth, glyph) in glyphs.get(g.clone()).unwrap_or_default().iter().enumerate() {
                gl.push(*glyph);
                let n = if nth == 0 { chars.len() } else { 0 };
                cells.push(u8::try_from(n).unwrap_or(u8::MAX));
            }
        }
        // A piece with no glyph at all leaves its cells blank.
        if gl.is_empty() {
            continue;
        }
        out.push(Piece {
            start,
            glyphs: gl.into(),
            cells: cells.into(),
        });
    }
    out
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn only_printable_ascii_joins() {
        assert!(joins('-') && joins('>') && joins('a') && joins('~'));
        assert!(!joins(' ') && !joins('\u{e9}') && !joins('\u{2500}'));
    }

    #[test]
    fn runs_split_where_the_key_changes() {
        // "a->b c==" with the colour changing after "->".
        let keys = [
            Some(1),
            Some(1),
            Some(1),
            Some(2),
            None,
            Some(2),
            Some(2),
            Some(2),
        ];
        assert_eq!(runs(keys.len(), |x| keys[x]), vec![0..3, 5..8]);
        let single = [Some(1), None, Some(1)];
        assert!(runs(single.len(), |x| single[x]).is_empty());
        assert!(runs(0, |_| Some(1)).is_empty());
    }

    #[test]
    fn unchanged_glyphs_give_no_pieces() {
        let plain = [10, 11, 12];
        assert!(pieces(&plain, &plain, &[0, 1, 2]).is_empty());
    }

    #[test]
    fn contextual_alternates_form_one_piece() {
        // "a->b": '-' and '>' became the two halves of an arrow.
        let plain = [1, 2, 3, 4];
        let shaped = [1, 20, 30, 4];
        let p = pieces(&plain, &shaped, &[0, 1, 2, 3]);
        assert_eq!(
            p,
            vec![Piece {
                start: 1,
                glyphs: [20, 30].into(),
                cells: [1, 1].into(),
            }]
        );
        assert_eq!(p[0].width(), 2);
    }

    #[test]
    fn a_true_ligature_spans_its_characters() {
        // "x!=y": "!=" became one glyph.
        let plain = [1, 2, 3, 4];
        let shaped = [1, 50, 4];
        let p = pieces(&plain, &shaped, &[0, 1, 1, 2]);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].start, 1);
        assert_eq!(&*p[0].glyphs, &[50]);
        assert_eq!(&*p[0].cells, &[2]);
    }

    #[test]
    fn separate_changes_are_separate_pieces() {
        let plain = [1, 2, 3, 4, 5];
        let shaped = [9, 2, 3, 4, 8];
        let p = pieces(&plain, &shaped, &[0, 1, 2, 3, 4]);
        assert_eq!(p.iter().map(|p| p.start).collect::<Vec<_>>(), vec![0, 4]);
    }

    #[test]
    fn too_wide_pieces_are_left_out() {
        let n = MAX_CELLS + 1;
        let plain: Vec<u16> = (0..n as u16).collect();
        let shaped: Vec<u16> = (100..100 + n as u16).collect();
        let map: Vec<u16> = (0..n as u16).collect();
        assert!(pieces(&plain, &shaped, &map).is_empty());
        assert_eq!(
            pieces(&plain[..MAX_CELLS], &shaped[..MAX_CELLS], &map[..MAX_CELLS]).len(),
            1
        );
    }

    #[test]
    fn broken_maps_are_ignored() {
        assert!(pieces(&[1, 2], &[1], &[0, 5]).is_empty());
        assert!(pieces(&[1, 2], &[1, 2], &[1, 0]).is_empty());
    }
}
