//! Keyword highlighting and stop patterns: regular expressions run over
//! the text of terminal lines.
//!
//! The patterns are the user's, so they must not be able to hang the
//! window. They run on `regex-automata`'s lazy DFA, which
//! `alacritty_terminal` already links: it matches in time linear in the
//! line, never backtracks, and its compiled size and cache are capped
//! here. A pattern too big for those caps is refused when it is set.
//!
//! Patterns match bytes with ASCII rules: `\w`, `\b` and `(?i)` know ASCII
//! letters only, and `.` matches any byte. A line's text is UTF-8, so a
//! literal accented word still matches as written.

use regex_automata::hybrid::dfa::Config as DfaConfig;
use regex_automata::hybrid::regex::Builder;
pub use regex_automata::hybrid::regex::{Cache, Regex};
use regex_automata::nfa::thompson::Config as NfaConfig;
use regex_automata::util::syntax::Config as SyntaxConfig;
use regex_automata::{Input, Match};

use ferossh_render::{Cell, Color, Flags};

/// The longest pattern accepted, in bytes.
pub const MAX_PATTERN: usize = 1000;
/// The most rules one highlighter takes; the rest are left out.
pub const MAX_RULES: usize = 64;
/// The compiled pattern set may take this much memory.
const NFA_LIMIT: usize = 2 << 20;
/// The lazy DFA's state cache per pane.
const CACHE_LIMIT: usize = 2 << 20;
/// Matches looked at per line; a line has at most a few hundred cells.
const MAX_MATCHES: usize = 512;

/// How highlighted text looks. Unset colours keep the text's own.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Style {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub bold: bool,
    pub underline: bool,
}

/// One rule: a regular expression and how its matches look.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    pub pattern: String,
    pub style: Style,
}

/// Compiles one or more patterns into a linear-time matcher with the
/// size caps above. Earlier patterns win where two match at one place.
pub fn compile(patterns: &[&str]) -> Result<Regex, String> {
    if let Some(p) = patterns.iter().find(|p| p.len() > MAX_PATTERN) {
        return Err(format!(
            "The pattern is {} bytes long; at most {MAX_PATTERN} are allowed.",
            p.len()
        ));
    }
    if patterns.iter().any(|p| p.is_empty()) {
        return Err("A pattern is empty.".into());
    }
    Builder::new()
        .syntax(SyntaxConfig::new().unicode(false).utf8(false))
        .thompson(NfaConfig::new().nfa_size_limit(Some(NFA_LIMIT)))
        .dfa(
            DfaConfig::new()
                .cache_capacity(CACHE_LIMIT)
                // A tiny cache that keeps filling up is slow, not wrong:
                // let the search carry on clearing it.
                .minimum_cache_clear_count(None),
        )
        .build_many(patterns)
        .map_err(|e| {
            let text = e.to_string();
            // The syntax error's own text points at the place.
            format!("The pattern is not valid: {text}")
        })
}

/// Checks one pattern, for a settings form or prompt.
pub fn check(pattern: &str) -> Result<(), String> {
    compile(&[pattern]).map(|_| ())
}

/// A compiled rule set, shared by the panes that use it.
#[derive(Debug)]
pub struct Highlighter {
    regex: Regex,
    styles: Vec<Style>,
}

impl Highlighter {
    /// Compiles the rules. A rule whose pattern does not compile is left
    /// out, and its index and the reason come back with the rest; `None`
    /// when no rule is left.
    pub fn new(rules: &[Rule]) -> (Option<Self>, Vec<(usize, String)>) {
        let mut errors = Vec::new();
        let mut kept = Vec::new();
        for (i, rule) in rules.iter().enumerate() {
            if kept.len() == MAX_RULES {
                errors.push((i, format!("Only {MAX_RULES} highlight rules are used.")));
                continue;
            }
            match check(&rule.pattern) {
                Ok(()) => kept.push(rule),
                Err(e) => errors.push((i, e)),
            }
        }
        if kept.is_empty() {
            return (None, errors);
        }
        let patterns: Vec<&str> = kept.iter().map(|r| r.pattern.as_str()).collect();
        match compile(&patterns) {
            Ok(regex) => (
                Some(Self {
                    regex,
                    styles: kept.iter().map(|r| r.style).collect(),
                }),
                errors,
            ),
            Err(e) => {
                // Each fits alone but not all together.
                errors.push((0, e));
                (None, errors)
            }
        }
    }

    pub fn create_cache(&self) -> Cache {
        self.regex.create_cache()
    }

    /// Recolours the matches in one line of cells. Only text in the
    /// terminal's default colours changes, so a program's own colours win.
    pub fn apply(&self, cache: &mut Cache, cells: &mut [Cell]) {
        let text = LineText::of(cells);
        for (m, style) in find_all(&self.regex, cache, &text.bytes)
            .filter_map(|m| Some((m, self.styles.get(m.pattern().as_usize())?)))
        {
            for x in text.cells(m.start(), m.end()) {
                let wide = cells.get(x).is_some_and(|c| c.flags.contains(Flags::WIDE));
                // A wide character's spacer draws its right half.
                for cell in cells.iter_mut().skip(x).take(1 + usize::from(wide)) {
                    paint(cell, style);
                }
            }
        }
    }
}

fn paint(cell: &mut Cell, style: &Style) {
    if cell.fg != Color::Default || cell.bg != Color::Default {
        return;
    }
    if let Some(fg) = style.fg {
        cell.fg = fg;
    }
    if let Some(bg) = style.bg {
        cell.bg = bg;
    }
    if style.bold {
        cell.flags.insert(Flags::BOLD);
    }
    if style.underline {
        cell.flags.insert(Flags::UNDERLINE);
    }
}

/// The matches in `text`, left to right, without empty ones. A search
/// the lazy DFA gives up on ends the list.
fn find_all<'a>(
    regex: &'a Regex,
    cache: &'a mut Cache,
    text: &'a [u8],
) -> impl Iterator<Item = Match> + 'a {
    let mut at = 0;
    std::iter::from_fn(move || {
        while at < text.len() {
            let input = Input::new(text).range(at..);
            let m = regex.try_search(cache, &input).ok().flatten()?;
            if m.is_empty() {
                at = m.end() + 1;
                continue;
            }
            at = m.end();
            return Some(m);
        }
        None
    })
    .take(MAX_MATCHES)
}

/// The first match in a line of cells, as the cell columns it covers.
pub fn find_in(regex: &Regex, cache: &mut Cache, cells: &[Cell]) -> Option<(usize, usize)> {
    let text = LineText::of(cells);
    let m = find_all(regex, cache, &text.bytes).next()?;
    let mut cols = text.cells(m.start(), m.end());
    let first = cols.next()?;
    let last = cols.last().unwrap_or(first);
    let wide = cells
        .get(last)
        .is_some_and(|c| c.flags.contains(Flags::WIDE));
    Some((first, last + usize::from(wide)))
}

/// A line's text as UTF-8, with the cell each byte came from.
struct LineText {
    bytes: Vec<u8>,
    cell_of: Vec<usize>,
}

impl LineText {
    fn of(cells: &[Cell]) -> Self {
        let mut bytes = Vec::with_capacity(cells.len());
        let mut cell_of = Vec::with_capacity(cells.len());
        let mut buf = [0u8; 4];
        for (x, cell) in cells.iter().enumerate() {
            if cell.flags.contains(Flags::WIDE_SPACER) {
                continue;
            }
            for &b in cell.ch.encode_utf8(&mut buf).as_bytes() {
                bytes.push(b);
                cell_of.push(x);
            }
        }
        // Blank cells after the last character are padding, not text, so
        // a pattern ending in `$` matches at the end of what is written.
        while bytes.last() == Some(&b' ') {
            bytes.pop();
            cell_of.pop();
        }
        Self { bytes, cell_of }
    }

    /// The cells bytes `start..end` came from, each once, left to right.
    fn cells(&self, start: usize, end: usize) -> impl Iterator<Item = usize> + '_ {
        let mut last = None;
        self.cell_of
            .get(start..end)
            .unwrap_or_default()
            .iter()
            .copied()
            .filter(move |&x| {
                let new = last != Some(x);
                last = Some(x);
                new
            })
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use ferossh_render::Cell;

    fn line(text: &str) -> Vec<Cell> {
        text.chars()
            .map(|c| Cell::new(c, Color::Default, Color::Default, Flags::empty()))
            .collect()
    }

    fn red() -> Style {
        Style {
            fg: Some(Color::Indexed(1)),
            bold: true,
            ..Style::default()
        }
    }

    fn painted(cells: &[Cell]) -> String {
        cells
            .iter()
            .map(|c| if c.fg == Color::Default { '.' } else { '^' })
            .collect()
    }

    #[test]
    fn words_are_coloured() {
        let rules = [Rule {
            pattern: r"(?i)\b(error|failed)\b".into(),
            style: red(),
        }];
        let (h, errors) = Highlighter::new(&rules);
        assert!(errors.is_empty());
        let h = h.expect("compiles");
        let mut cache = h.create_cache();
        let mut cells = line("an Error, then FAILED; errors stay");
        h.apply(&mut cache, &mut cells);
        assert_eq!(painted(&cells), "...^^^^^.......^^^^^^.............");
        assert!(cells[3].flags.contains(Flags::BOLD));
    }

    #[test]
    fn coloured_text_is_left_alone() {
        let (h, _) = Highlighter::new(&[Rule {
            pattern: "error".into(),
            style: red(),
        }]);
        let h = h.expect("compiles");
        let mut cache = h.create_cache();
        let mut cells = line("error error");
        for c in &mut cells[6..] {
            c.fg = Color::Indexed(2);
        }
        h.apply(&mut cache, &mut cells);
        assert_eq!(cells[0].fg, Color::Indexed(1));
        assert_eq!(cells[6].fg, Color::Indexed(2));
    }

    #[test]
    fn first_rule_wins() {
        let (h, _) = Highlighter::new(&[
            Rule {
                pattern: "fail".into(),
                style: red(),
            },
            Rule {
                pattern: "failed".into(),
                style: Style {
                    fg: Some(Color::Indexed(3)),
                    ..Style::default()
                },
            },
        ]);
        let h = h.expect("compiles");
        let mut cache = h.create_cache();
        let mut cells = line("failed");
        h.apply(&mut cache, &mut cells);
        assert_eq!(cells[0].fg, Color::Indexed(1));
        assert_eq!(cells[5].fg, Color::Default);
    }

    #[test]
    fn wide_and_accented_text() {
        let (h, _) = Highlighter::new(&[Rule {
            pattern: "café|日本".into(),
            style: red(),
        }]);
        let h = h.expect("compiles");
        let mut cache = h.create_cache();
        let mut cells = line("a café");
        h.apply(&mut cache, &mut cells);
        assert_eq!(painted(&cells), "..^^^^");
        // 日 and 本 are wide: each has a spacer after it.
        let mut cells = Vec::new();
        for c in ['x', '日', ' ', '本', ' ', 'y'] {
            let mut cell = Cell::new(c, Color::Default, Color::Default, Flags::empty());
            if c == ' ' {
                cell.flags.insert(Flags::WIDE_SPACER);
            } else if c != 'x' && c != 'y' {
                cell.flags.insert(Flags::WIDE);
            }
            cells.push(cell);
        }
        h.apply(&mut cache, &mut cells);
        assert_eq!(painted(&cells), ".^^^^.");
    }

    #[test]
    fn empty_matches_and_bad_patterns() {
        let (h, errors) = Highlighter::new(&[
            Rule {
                pattern: "x*".into(),
                style: red(),
            },
            Rule {
                pattern: "(".into(),
                style: red(),
            },
            Rule {
                pattern: String::new(),
                style: red(),
            },
        ]);
        assert_eq!(errors.len(), 2);
        assert_eq!(errors[0].0, 1);
        let h = h.expect("the first rule compiles");
        let mut cache = h.create_cache();
        let mut cells = line("ab xx c");
        h.apply(&mut cache, &mut cells);
        assert_eq!(painted(&cells), "...^^..");
    }

    #[test]
    fn huge_patterns_are_refused() {
        assert!(check(&"a".repeat(MAX_PATTERN + 1)).is_err());
        // Small to write, enormous once repetitions are spelled out.
        assert!(check("(a{1000}){1000}").is_err());
        assert!(check(r"\w{50}").is_ok());
    }

    #[test]
    fn evil_patterns_run_in_linear_time() {
        // Catastrophic for a backtracking engine.
        let (h, _) = Highlighter::new(&[Rule {
            pattern: "(a+)+b".into(),
            style: red(),
        }]);
        let h = h.expect("compiles");
        let mut cache = h.create_cache();
        let mut cells = line(&"a".repeat(4000));
        let start = std::time::Instant::now();
        for _ in 0..50 {
            h.apply(&mut cache, &mut cells);
        }
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
        assert_eq!(cells[0].fg, Color::Default);
    }

    #[test]
    fn end_anchors_ignore_the_padding() {
        let regex = compile(&["done$"]).expect("compiles");
        let mut cache = regex.create_cache();
        let mut cells = line("all done");
        cells.extend(line("        "));
        assert_eq!(find_in(&regex, &mut cache, &cells), Some((4, 7)));
    }

    #[test]
    fn find_gives_columns() {
        let regex = compile(&["fatal"]).expect("compiles");
        let mut cache = regex.create_cache();
        assert_eq!(
            find_in(&regex, &mut cache, &line("a fatal b")),
            Some((2, 6))
        );
        assert_eq!(find_in(&regex, &mut cache, &line("fine")), None);
    }
}
