//! The side pane's snippets view: named command lines to type into the
//! focused pane (and every pane broadcast input reaches) without typing
//! them out. Ctrl+Shift+Space opens it; typing filters the list, Enter
//! types the snippet with Enter, Shift+Enter without the last Enter.
//! Insert adds a snippet, F2 edits the selected one, Delete removes it.
//!
//! Drawn into a [`Grid`] like the session manager; keys come in as
//! [`SideKey`]s and what the user chose goes out as a [`SnipAction`].

use ferossh_config::login::Snippet;
use ferossh_render::{Flags, Grid, Rgb};

use crate::session_form::Line;
use crate::sessions::SideKey;
use crate::tabstrip::{Palette, fit};

/// Lines above the list: the title and the filter.
const HEADER: usize = 2;
/// Lines under the list: the key help.
const FOOTER: usize = 1;
/// Longest name or line one can type.
const MAX_TEXT: usize = 400;
/// Lines in one snippet.
const MAX_LINES: usize = 50;

/// What the window should do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnipAction {
    /// Type `lines` into the focused pane; `enter` presses Enter after the
    /// last one too.
    Send { lines: Vec<String>, enter: bool },
    /// Store `snippet` in place of snippet `index`, or as a new one.
    Save {
        index: Option<usize>,
        snippet: Snippet,
    },
    /// Remove snippet `index` (the window asks first).
    Delete(usize),
    /// Back to the terminal.
    Leave,
}

/// A snippet being made or changed.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Draft {
    /// The snippet it replaces.
    index: Option<usize>,
    name: String,
    /// Always ends with an empty line to type a new one in.
    lines: Vec<String>,
    /// 0 is the name, 1.. the lines, then Save and Cancel.
    focus: usize,
    cursor: usize,
    error: Option<String>,
}

impl Draft {
    fn new(index: Option<usize>, snippet: &Snippet) -> Self {
        let mut d = Self {
            index,
            name: snippet.name.clone(),
            lines: snippet.lines.clone(),
            focus: 0,
            cursor: snippet.name.chars().count(),
            error: None,
        };
        d.tidy();
        d
    }

    fn tidy(&mut self) {
        if self.lines.last().is_none_or(|l| !l.is_empty()) && self.lines.len() < MAX_LINES {
            self.lines.push(String::new());
        }
    }

    /// Fields, then the two buttons.
    fn count(&self) -> usize {
        1 + self.lines.len() + 2
    }

    fn text_mut(&mut self) -> Option<&mut String> {
        match self.focus {
            0 => Some(&mut self.name),
            i => self.lines.get_mut(i - 1),
        }
    }

    fn text(&self, i: usize) -> Option<&String> {
        match i {
            0 => Some(&self.name),
            i => self.lines.get(i - 1),
        }
    }

    fn set_focus(&mut self, i: usize) {
        self.focus = i.min(self.count() - 1);
        self.cursor = self.text(self.focus).map_or(0, |t| t.chars().count());
    }

    fn save(&mut self) -> Option<SnipAction> {
        let name = self.name.trim().to_owned();
        let lines: Vec<String> = self
            .lines
            .iter()
            .map(|l| l.trim_end().to_owned())
            .filter(|l| !l.trim().is_empty())
            .collect();
        if name.is_empty() {
            self.error = Some("Give the snippet a name.".into());
            self.set_focus(0);
            return None;
        }
        if lines.is_empty() {
            self.error = Some("Type at least one command line.".into());
            self.set_focus(1);
            return None;
        }
        Some(SnipAction::Save {
            index: self.index,
            snippet: Snippet { name, lines },
        })
    }
}

#[derive(Debug, Default)]
pub struct SnippetsView {
    filter: String,
    /// The selected row among the matching snippets.
    selected: usize,
    scroll: usize,
    draft: Option<Draft>,
}

impl SnippetsView {
    /// The indexes of the snippets the filter keeps.
    fn shown(&self, all: &[Snippet]) -> Vec<usize> {
        all.iter()
            .enumerate()
            .filter(|(_, s)| s.matches(&self.filter))
            .map(|(i, _)| i)
            .collect()
    }

    fn visible(rows: usize) -> usize {
        rows.saturating_sub(HEADER + FOOTER).max(1)
    }

    /// Whether a snippet is being edited.
    pub fn editing(&self) -> bool {
        self.draft.is_some()
    }

    /// Starts afresh with an empty filter (when the view is opened).
    pub fn reset(&mut self) {
        self.filter.clear();
        self.selected = 0;
        self.scroll = 0;
        self.draft = None;
    }

    /// The edit is over: saved, or the snippet is gone.
    pub fn saved(&mut self, all: &[Snippet], select: Option<usize>) {
        self.draft = None;
        if let Some(i) = select {
            self.filter.clear();
            self.selected = i;
        }
        self.selected = self.selected.min(all.len().saturating_sub(1));
    }

    fn clamp(&mut self, n: usize, visible: usize) {
        self.selected = self.selected.min(n.saturating_sub(1));
        if self.selected < self.scroll {
            self.scroll = self.selected;
        }
        if self.selected >= self.scroll + visible {
            self.scroll = self.selected + 1 - visible;
        }
    }

    pub fn key(&mut self, key: SideKey, all: &[Snippet], rows: usize) -> Option<SnipAction> {
        if self.draft.is_some() {
            return self.draft_key(key);
        }
        let shown = self.shown(all);
        let visible = Self::visible(rows);
        let here = shown.get(self.selected).copied();
        match key {
            SideKey::Up => self.selected = self.selected.saturating_sub(1),
            SideKey::Down => self.selected += 1,
            SideKey::PageUp => self.selected = self.selected.saturating_sub(visible),
            SideKey::PageDown => self.selected += visible,
            SideKey::Home => self.selected = 0,
            SideKey::End => self.selected = shown.len().saturating_sub(1),
            SideKey::Enter { split } => {
                let s = all.get(here?)?;
                return Some(SnipAction::Send {
                    lines: s.lines.clone(),
                    enter: !split,
                });
            }
            SideKey::Escape if self.filter.is_empty() => return Some(SnipAction::Leave),
            SideKey::Escape => self.filter.clear(),
            SideKey::Backspace => {
                self.filter.pop();
            }
            SideKey::Char(c) if !c.is_control() && self.filter.chars().count() < 60 => {
                self.filter.push(c);
                self.selected = 0;
            }
            SideKey::New => {
                // What was typed as a filter names the new snippet.
                let name = std::mem::take(&mut self.filter);
                self.draft = Some(Draft::new(
                    None,
                    &Snippet {
                        name,
                        lines: Vec::new(),
                    },
                ));
                if let Some(d) = &mut self.draft {
                    d.set_focus(if d.name.is_empty() { 0 } else { 1 });
                }
            }
            SideKey::Edit => {
                let i = here?;
                self.draft = Some(Draft::new(Some(i), all.get(i)?));
            }
            SideKey::Delete => return Some(SnipAction::Delete(here?)),
            _ => {}
        }
        self.clamp(self.shown(all).len(), visible);
        None
    }

    fn draft_key(&mut self, key: SideKey) -> Option<SnipAction> {
        let d = self.draft.as_mut()?;
        let n = d.count();
        let save = n - 2;
        let on_button = d.focus >= save;
        match key {
            SideKey::Escape => {
                self.draft = None;
                return None;
            }
            SideKey::Up => d.set_focus(d.focus.saturating_sub(1)),
            SideKey::Down | SideKey::Tab => d.set_focus((d.focus + 1) % n),
            SideKey::Home if on_button => d.set_focus(0),
            SideKey::End if on_button => d.set_focus(n - 1),
            SideKey::Home => d.cursor = 0,
            SideKey::End => d.cursor = d.text(d.focus).map_or(0, |t| t.chars().count()),
            SideKey::Left if on_button => d.set_focus(save),
            SideKey::Right if on_button => d.set_focus(save + 1),
            SideKey::Left => d.cursor = d.cursor.saturating_sub(1),
            SideKey::Right => {
                let len = d.text(d.focus).map_or(0, |t| t.chars().count());
                d.cursor = (d.cursor + 1).min(len);
            }
            SideKey::Enter { .. } if d.focus == save + 1 => {
                self.draft = None;
                return None;
            }
            SideKey::Enter { .. } if d.focus == save => return d.save(),
            // Enter on a line goes to the next, where a new one waits.
            SideKey::Enter { .. } => d.set_focus(d.focus + 1),
            SideKey::Char(' ') if on_button => {
                return self.draft_key(SideKey::Enter { split: false });
            }
            SideKey::Char(c) if !c.is_control() && !on_button => {
                let at = d.cursor;
                if let Some(t) = d.text_mut()
                    && t.chars().count() < MAX_TEXT
                {
                    let byte = t.char_indices().nth(at).map_or(t.len(), |(b, _)| b);
                    t.insert(byte, c);
                    d.cursor += 1;
                }
                d.error = None;
                d.tidy();
            }
            SideKey::Backspace | SideKey::Delete if !on_button => {
                let at = d.cursor;
                let back = key == SideKey::Backspace;
                if let Some(t) = d.text_mut() {
                    let gone = if back { at.checked_sub(1) } else { Some(at) };
                    if let Some(i) = gone
                        && let Some((b, _)) = t.char_indices().nth(i)
                    {
                        t.remove(b);
                        if back {
                            d.cursor -= 1;
                        }
                    }
                }
            }
            _ => {}
        }
        None
    }

    /// A click on grid row `y` selects the snippet or field there.
    pub fn click(&mut self, y: usize, all: &[Snippet], rows: usize) {
        if let Some(d) = &mut self.draft {
            if let Some(i) = y.checked_sub(HEADER)
                && i < d.count()
            {
                d.set_focus(i);
            }
            return;
        }
        if let Some(i) = y.checked_sub(HEADER) {
            self.selected = self.scroll + i;
        }
        self.clamp(self.shown(all).len(), Self::visible(rows));
    }

    /// Wheel scrolling by `lines` (positive: up).
    pub fn scroll_by(&mut self, by: i32, all: &[Snippet], rows: usize) {
        if self.draft.is_some() {
            return;
        }
        let max = self.shown(all).len().saturating_sub(Self::visible(rows));
        self.scroll = if by > 0 {
            self.scroll.saturating_sub(by as usize)
        } else {
            (self.scroll + by.unsigned_abs() as usize).min(max)
        };
    }

    pub fn draw(&mut self, grid: &mut Grid, palette: &Palette, focused: bool, all: &[Snippet]) {
        let width = grid.cols();
        let rows = grid.rows();
        let bg = palette.strip;
        let red = Rgb::new(0xd0, 0x6a, 0x6a);
        let row = |grid: &mut Grid, y: usize, line: &mut Line, bg: Rgb| {
            grid.set_row(y, line.cells(bg));
        };
        let title = if self.draft.as_ref().is_some_and(|d| d.index.is_some()) {
            " Edit snippet"
        } else if self.draft.is_some() {
            " New snippet"
        } else {
            " Snippets"
        };
        let mut line = Line::new(width, bg);
        line.put(title, palette.text, Flags::BOLD);
        row(grid, 0, &mut line, bg);
        if let Some(d) = &self.draft {
            let mut y = 1;
            let mut line = Line::new(width, bg);
            match &d.error {
                Some(e) => line.put(&format!(" {e}"), red, Flags::empty()),
                None => line.put(
                    " Enter: next line · Esc: cancel",
                    palette.dim,
                    Flags::ITALIC,
                ),
            };
            row(grid, y, &mut line, bg);
            y += 1;
            let save = d.count() - 2;
            for i in 0..save {
                if y >= rows {
                    break;
                }
                let here = focused && d.focus == i;
                let row_bg = if here { palette.active } else { bg };
                let mut line = Line::new(width, row_bg);
                let label = if i == 0 {
                    "Name".to_owned()
                } else {
                    format!("{i}")
                };
                line.put(&format!(" {label:>4} "), palette.dim, Flags::empty());
                let value = d.text(i).cloned().unwrap_or_default();
                let room = width.saturating_sub(line.col + 1);
                if here {
                    let chars: Vec<char> = value.chars().collect();
                    let at = d.cursor.min(chars.len());
                    let start = at.saturating_sub(room.saturating_sub(2));
                    let before: String = chars.get(start..at).unwrap_or_default().iter().collect();
                    let after: String = chars.get(at..).unwrap_or_default().iter().collect();
                    line.put(&before, palette.text, Flags::empty());
                    line.put("▏", palette.accent, Flags::empty());
                    let left = width.saturating_sub(line.col);
                    line.put(&fit(&after, left), palette.text, Flags::empty());
                } else if value.is_empty() {
                    let hint = if i == 0 {
                        "e.g. Show interfaces"
                    } else {
                        "command line"
                    };
                    line.put(&fit(hint, room), palette.dim, Flags::ITALIC);
                } else {
                    line.put(&fit(&value, room), palette.text, Flags::empty());
                }
                row(grid, y, &mut line, row_bg);
                y += 1;
            }
            if y < rows {
                let here = focused && d.focus >= save;
                let row_bg = if here { palette.active } else { bg };
                let mut line = Line::new(width, row_bg);
                line.put(" ", palette.text, Flags::empty());
                for (b, label) in ["Save", "Cancel"].into_iter().enumerate() {
                    let on = focused && d.focus == save + b;
                    let flags = if on {
                        Flags::BOLD.union(Flags::INVERSE)
                    } else {
                        Flags::BOLD
                    };
                    line.put(&format!(" {label} "), palette.text, flags);
                    line.put(" ", palette.text, Flags::empty());
                }
                row(grid, y, &mut line, row_bg);
                y += 1;
            }
            for y in y..rows {
                row(grid, y, &mut Line::new(width, bg), bg);
            }
            return;
        }
        let shown = self.shown(all);
        let visible = Self::visible(rows);
        self.clamp(shown.len(), visible);
        let mut line = Line::new(width, bg);
        if self.filter.is_empty() {
            let hint = if focused {
                " Type to filter▏"
            } else {
                " Ctrl+Shift+Space to pick"
            };
            line.put(hint, palette.dim, Flags::ITALIC);
        } else {
            line.put(" Filter ", palette.dim, Flags::empty());
            line.put(&self.filter, palette.text, Flags::empty());
            if focused {
                line.put("▏", palette.accent, Flags::empty());
            }
        }
        row(grid, 1, &mut line, bg);
        for y in 0..visible {
            let at = HEADER + y;
            if at >= rows.saturating_sub(FOOTER) {
                break;
            }
            let i = self.scroll + y;
            let here = focused && i == self.selected;
            let row_bg = if here { palette.active } else { bg };
            let mut line = Line::new(width, row_bg);
            match shown.get(i).and_then(|&n| all.get(n)) {
                Some(s) => {
                    line.put(
                        &fit(&format!(" {}", s.name), width),
                        palette.text,
                        Flags::empty(),
                    );
                    let first = s.lines.first().map(String::as_str).unwrap_or_default();
                    let more = if s.lines.len() > 1 {
                        format!(" (+{})", s.lines.len() - 1)
                    } else {
                        String::new()
                    };
                    let left = width.saturating_sub(line.col);
                    line.put(
                        &fit(&format!("  {first}{more}"), left),
                        palette.dim,
                        Flags::empty(),
                    );
                }
                None if i == 0 && all.is_empty() => {
                    line.put(
                        " No snippets yet. Insert adds one.",
                        palette.dim,
                        Flags::ITALIC,
                    );
                }
                None if i == 0 => {
                    line.put(" Nothing matches.", palette.dim, Flags::ITALIC);
                }
                None => {}
            }
            row(grid, at, &mut line, row_bg);
        }
        if rows > HEADER {
            let mut line = Line::new(width, bg);
            line.put(
                &fit(
                    " Enter types · Shift+Enter: no Enter · Ins new · F2 edit · Del",
                    width,
                ),
                palette.dim,
                Flags::empty(),
            );
            row(grid, rows - 1, &mut line, bg);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use ferossh_render::Theme;

    fn snippets() -> Vec<Snippet> {
        vec![
            Snippet {
                name: "Show interfaces".into(),
                lines: vec!["show ip interface brief".into()],
            },
            Snippet {
                name: "Save config".into(),
                lines: vec!["copy running-config startup-config".into(), String::new()],
            },
        ]
    }

    fn typed(v: &mut SnippetsView, all: &[Snippet], text: &str) {
        for c in text.chars() {
            v.key(SideKey::Char(c), all, 20);
        }
    }

    fn texts(v: &mut SnippetsView, all: &[Snippet]) -> Vec<String> {
        let mut grid = Grid::new(60, 12);
        v.draw(&mut grid, &Palette::of(&Theme::default()), true, all);
        (0..12)
            .map(|y| {
                grid.row(y)
                    .unwrap()
                    .iter()
                    .map(|c| c.ch)
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect()
    }

    #[test]
    fn filter_and_send() {
        let all = snippets();
        let mut v = SnippetsView::default();
        let t = texts(&mut v, &all);
        assert!(t[2].contains("Show interfaces") && t[2].contains("show ip"));
        assert!(
            t[3].contains("Save config") && t[3].ends_with("(+1)"),
            "{t:#?}"
        );
        typed(&mut v, &all, "save");
        let t = texts(&mut v, &all);
        assert!(t[1].contains("Filter save"));
        assert!(t[2].contains("Save config"));
        assert!(!t.iter().any(|l| l.contains("Show interfaces")));
        assert_eq!(
            v.key(SideKey::Enter { split: true }, &all, 20),
            Some(SnipAction::Send {
                lines: all[1].lines.clone(),
                enter: false
            })
        );
        v.key(SideKey::Escape, &all, 20);
        v.key(SideKey::Down, &all, 20);
        v.key(SideKey::Down, &all, 20);
        assert_eq!(
            v.key(SideKey::Enter { split: false }, &all, 20),
            Some(SnipAction::Send {
                lines: all[1].lines.clone(),
                enter: true
            }),
            "the selection stops at the last"
        );
        assert_eq!(
            v.key(SideKey::Delete, &all, 20),
            Some(SnipAction::Delete(1))
        );
        assert_eq!(v.key(SideKey::Escape, &all, 20), Some(SnipAction::Leave));
        typed(&mut v, &all, "vlan");
        assert_eq!(v.key(SideKey::Enter { split: false }, &all, 20), None);
        assert!(texts(&mut v, &all)[2].contains("Nothing matches"));
    }

    #[test]
    fn new_snippets_take_the_filter_as_name() {
        let all = snippets();
        let mut v = SnippetsView::default();
        typed(&mut v, &all, "Show vlans");
        v.key(SideKey::New, &all, 20);
        assert!(v.editing());
        typed(&mut v, &all, "terminal length 0");
        v.key(SideKey::Enter { split: false }, &all, 20);
        typed(&mut v, &all, "show vlan brief");
        let t = texts(&mut v, &all);
        assert!(t[0].contains("New snippet"));
        assert!(t[2].contains("Name Show vlans"), "{t:#?}");
        assert!(t[5].contains("command line"), "a new line waits: {t:#?}");
        // Down past the empty line to Save.
        v.key(SideKey::Down, &all, 20);
        v.key(SideKey::Down, &all, 20);
        assert_eq!(
            v.key(SideKey::Enter { split: false }, &all, 20),
            Some(SnipAction::Save {
                index: None,
                snippet: Snippet {
                    name: "Show vlans".into(),
                    lines: vec!["terminal length 0".into(), "show vlan brief".into()],
                }
            })
        );
    }

    #[test]
    fn editing_checks_and_cancels() {
        let all = snippets();
        let mut v = SnippetsView::default();
        v.key(SideKey::Edit, &all, 20);
        let t = texts(&mut v, &all);
        assert!(t[0].contains("Edit snippet"));
        for _ in 0..20 {
            v.key(SideKey::Backspace, &all, 20);
        }
        // Name, line 1, the empty line, then Save.
        for _ in 0..3 {
            v.key(SideKey::Tab, &all, 20);
        }
        assert_eq!(v.key(SideKey::Enter { split: false }, &all, 20), None);
        assert!(texts(&mut v, &all)[1].contains("Give the snippet a name."));
        typed(&mut v, &all, "Brief");
        for _ in 0..3 {
            v.key(SideKey::Tab, &all, 20);
        }
        assert_eq!(
            v.key(SideKey::Char(' '), &all, 20),
            Some(SnipAction::Save {
                index: Some(0),
                snippet: Snippet {
                    name: "Brief".into(),
                    lines: all[0].lines.clone(),
                }
            })
        );
        v.key(SideKey::Escape, &all, 20);
        assert!(!v.editing());
    }
}
