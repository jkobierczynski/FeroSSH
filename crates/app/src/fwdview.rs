//! The side pane's forwards view: every connection's port forwards and
//! X11 with their state and open connections, a line to add a forward
//! (`-L 8080:db:5432`, `-R`, `-D`) to the connection picked in the list
//! or else the focused pane's, and Delete to stop the selected forward.
//!
//! Drawn into a [`Grid`] like the session manager; keys come in as
//! [`SideKey`]s and what the user asked for goes out as a [`FwdAction`].

use ferossh_config::forward::parse_option;
use ferossh_config::{ForwardKind, ForwardSpec};
use ferossh_ipc::ChannelId;
use ferossh_render::{Cell, Color, Flags, Grid, Rgb};

use crate::forwards::{ConnView, FwdState, FwdView};
use crate::sessions::SideKey;
use crate::tabstrip::{Palette, fit};

/// Lines above the list: the title and the add line.
const HEADER: usize = 2;

/// What the window should do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FwdAction {
    /// Add to `connection` when the user picked one in the list, else to
    /// the focused pane's connection.
    Add {
        spec: ForwardSpec,
        connection: Option<ChannelId>,
    },
    Remove {
        connection: ChannelId,
        index: usize,
    },
    Leave,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tone {
    Heading,
    Normal,
    Dim,
    Bad,
}

/// One line of the list. `connection` is set on the lines that can be
/// selected (a connection's heading and its forwards), `forward` on a
/// forward that Delete stops.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Line {
    text: String,
    tone: Tone,
    connection: Option<ChannelId>,
    forward: Option<(ChannelId, usize)>,
}

#[derive(Debug, Default)]
pub struct ForwardsView {
    input: String,
    /// Shown under the add line until the next key: a parse error, say.
    message: Option<String>,
    /// The selected line, as an index among the selectable lines.
    selected: usize,
    /// Whether the user moved the selection, which makes its connection
    /// the one a new forward goes to.
    picked: bool,
    scroll: usize,
}

fn host_port(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

fn describe(f: &FwdView) -> String {
    let s = &f.spec;
    let target = s
        .target
        .as_ref()
        .map(|(h, p)| host_port(h, *p))
        .unwrap_or_default();
    let place = match &f.state {
        FwdState::Listening(at) => at.clone(),
        FwdState::Starting | FwdState::Failed(_) => match s.kind {
            ForwardKind::Remote => format!("server port {}", s.port),
            _ => format!("port {}", s.port),
        },
    };
    let mut text = match s.kind {
        ForwardKind::Local => format!("L {place} → {target}"),
        ForwardKind::Remote => format!("R {place} → {target}"),
        ForwardKind::Dynamic => format!("D {place} SOCKS"),
    };
    match &f.state {
        FwdState::Starting => text.push_str(" · starting"),
        FwdState::Failed(_) => text.push_str(" · stopped"),
        FwdState::Listening(_) if f.open > 0 => text.push_str(&format!(" · {} open", f.open)),
        FwdState::Listening(_) => {}
    }
    text
}

fn lines(view: &[ConnView]) -> Vec<Line> {
    let mut out = Vec::new();
    let line = |text: String, tone| Line {
        text,
        tone,
        connection: None,
        forward: None,
    };
    for c in view {
        out.push(Line {
            connection: Some(c.connection),
            ..line(format!(" {}", c.label), Tone::Heading)
        });
        for (i, f) in c.forwards.iter().enumerate() {
            out.push(Line {
                text: format!("  {}", describe(f)),
                tone: if matches!(f.state, FwdState::Failed(_)) {
                    Tone::Bad
                } else {
                    Tone::Normal
                },
                connection: Some(c.connection),
                forward: Some((c.connection, i)),
            });
            if let FwdState::Failed(why) = &f.state {
                out.push(line(format!("     {why}"), Tone::Bad));
            }
            for s in &f.streams {
                out.push(line(format!("     {s}"), Tone::Dim));
            }
            if let Some(e) = &f.error {
                out.push(line(format!("     last error: {e}"), Tone::Dim));
            }
        }
        if let Some(x) = &c.x11 {
            let open = if x.open > 0 {
                format!(" · {} open", x.open)
            } else {
                String::new()
            };
            out.push(line(format!("  X11 → {}{open}", x.display), Tone::Normal));
            if let Some(e) = &x.error {
                out.push(line(format!("     last error: {e}"), Tone::Dim));
            }
        }
        if c.forwards.is_empty() && c.x11.is_none() {
            out.push(line("  nothing forwarded".into(), Tone::Dim));
        }
    }
    if out.is_empty() {
        out.push(line(" No connection forwards anything.".into(), Tone::Dim));
    }
    out
}

impl ForwardsView {
    /// Rows that fit in a grid of `rows` lines.
    pub fn visible(rows: usize) -> usize {
        rows.saturating_sub(HEADER).max(1)
    }

    fn selectable_count(lines: &[Line]) -> usize {
        lines.iter().filter(|l| l.connection.is_some()).count()
    }

    /// The line index of the selected line.
    fn selected_line(&self, lines: &[Line]) -> Option<usize> {
        lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.connection.is_some())
            .nth(self.selected)
            .map(|(i, _)| i)
    }

    fn clamp(&mut self, lines: &[Line], visible: usize) {
        let n = Self::selectable_count(lines);
        self.selected = self.selected.min(n.saturating_sub(1));
        if let Some(at) = self.selected_line(lines) {
            if at < self.scroll {
                self.scroll = at;
            }
            if at >= self.scroll + visible.max(1) {
                self.scroll = at + 1 - visible.max(1);
            }
        }
        self.scroll = self.scroll.min(lines.len().saturating_sub(1));
    }

    /// Puts a forward that could not be added back on the add line, with
    /// `why` above it, so it can be retried.
    pub fn refuse(&mut self, spec: &ForwardSpec, why: String) {
        self.input = spec.to_string();
        self.message = Some(why);
    }

    pub fn key(&mut self, key: SideKey, view: &[ConnView], rows: usize) -> Option<FwdAction> {
        let lines = lines(view);
        let visible = Self::visible(rows);
        self.message = None;
        match key {
            SideKey::Up => self.pick(self.selected.saturating_sub(1)),
            SideKey::Down => self.pick(self.selected + 1),
            SideKey::Home => self.pick(0),
            SideKey::End => self.pick(Self::selectable_count(&lines).saturating_sub(1)),
            SideKey::Enter { .. } => {
                let typed = self.input.trim();
                if typed.is_empty() {
                    return None;
                }
                match parse_option(typed) {
                    Some(spec) => {
                        self.input.clear();
                        let connection = self
                            .picked
                            .then(|| self.selected_line(&lines))
                            .flatten()
                            .and_then(|at| lines.get(at)?.connection);
                        return Some(FwdAction::Add { spec, connection });
                    }
                    None => {
                        self.message =
                            Some("Try -L 8080:host:80, -R 9000:localhost:3000 or -D 1080".into());
                    }
                }
            }
            SideKey::Delete => {
                let at = self.selected_line(&lines)?;
                let (connection, index) = lines.get(at)?.forward?;
                return Some(FwdAction::Remove { connection, index });
            }
            SideKey::Escape => {
                if self.input.is_empty() {
                    return Some(FwdAction::Leave);
                }
                self.input.clear();
            }
            SideKey::Backspace => {
                self.input.pop();
            }
            SideKey::Char(c) if !c.is_control() && self.input.chars().count() < 120 => {
                self.input.push(c);
            }
            _ => {}
        }
        self.clamp(&lines, visible);
        None
    }

    fn pick(&mut self, selected: usize) {
        self.selected = selected;
        self.picked = true;
    }

    /// A click on grid row `y` selects the connection or forward there.
    pub fn click(&mut self, y: usize, view: &[ConnView], rows: usize) {
        let lines = lines(view);
        let Some(i) = y.checked_sub(HEADER).map(|i| i + self.scroll) else {
            return;
        };
        if lines.get(i).is_some_and(|l| l.connection.is_some()) {
            let n = lines
                .iter()
                .take(i)
                .filter(|l| l.connection.is_some())
                .count();
            self.pick(n);
        }
        self.clamp(&lines, Self::visible(rows));
    }

    /// Wheel scrolling by `lines` (positive: up).
    pub fn scroll_by(&mut self, by: i32, view: &[ConnView], rows: usize) {
        let max = lines(view).len().saturating_sub(Self::visible(rows));
        self.scroll = if by > 0 {
            self.scroll.saturating_sub(by as usize)
        } else {
            (self.scroll + by.unsigned_abs() as usize).min(max)
        };
    }

    /// Draws the view; `focused` shows the selection and the input cursor.
    pub fn draw(&mut self, grid: &mut Grid, palette: &Palette, focused: bool, view: &[ConnView]) {
        let width = grid.cols();
        let lines = lines(view);
        let visible = Self::visible(grid.rows());
        self.clamp(&lines, visible);
        let bad = Rgb::new(0xe0, 0x6c, 0x75);
        let row = |text: &str, fg: Rgb, bg: Rgb, flags: Flags| -> Vec<Cell> {
            let mut cells: Vec<Cell> = fit(text, width)
                .chars()
                .map(|c| Cell::new(c, Color::Rgb(fg), Color::Rgb(bg), flags))
                .collect();
            cells.resize(
                width,
                Cell::new(' ', Color::Rgb(fg), Color::Rgb(bg), Flags::empty()),
            );
            cells.truncate(width);
            cells
        };
        grid.set_row(
            0,
            row(" Forwards", palette.text, palette.strip, Flags::BOLD),
        );
        let (add, fg) = match (&self.message, self.input.is_empty(), focused) {
            (Some(m), ..) => (format!(" {m}"), bad),
            (None, true, false) => (
                " Ctrl+Shift+P, then -L/-R/-D to add".to_owned(),
                palette.dim,
            ),
            (None, true, true) => (
                " Add: -L 8080:host:80, -R, -D 1080▏".to_owned(),
                palette.dim,
            ),
            (None, false, _) => (
                format!(" Add: {}{}", self.input, if focused { "▏" } else { "" }),
                palette.text,
            ),
        };
        grid.set_row(1, row(&add, fg, palette.active, Flags::empty()));
        let selected = self.selected_line(&lines);
        for y in 0..visible {
            let i = self.scroll + y;
            let Some(l) = lines.get(i) else {
                grid.set_row(
                    HEADER + y,
                    row("", palette.text, palette.strip, Flags::empty()),
                );
                continue;
            };
            let chosen = focused && selected == Some(i);
            let bg = if chosen {
                palette.active
            } else {
                palette.strip
            };
            let (fg, flags) = match l.tone {
                Tone::Heading => (palette.text, Flags::BOLD),
                Tone::Normal => (palette.text, Flags::empty()),
                Tone::Dim => (palette.dim, Flags::empty()),
                Tone::Bad => (bad, Flags::empty()),
            };
            grid.set_row(HEADER + y, row(&l.text, fg, bg, flags));
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::forwards::X11View;

    fn sample() -> Vec<ConnView> {
        let spec = |t: &str| parse_option(t).unwrap();
        vec![ConnView {
            connection: 1,
            label: "me@db".into(),
            forwards: vec![
                FwdView {
                    spec: spec("-L 8080:db:5432"),
                    state: FwdState::Listening("127.0.0.1:8080".into()),
                    open: 1,
                    total: 3,
                    streams: vec!["127.0.0.1:50000 → db:5432".into()],
                    error: None,
                },
                FwdView {
                    spec: spec("-R 9000:localhost:3000"),
                    state: FwdState::Failed("The server refused.".into()),
                    open: 0,
                    total: 0,
                    streams: Vec::new(),
                    error: None,
                },
                FwdView {
                    spec: spec("-D 1080"),
                    state: FwdState::Starting,
                    open: 0,
                    total: 0,
                    streams: Vec::new(),
                    error: Some("x".into()),
                },
            ],
            x11: Some(X11View {
                display: "localhost:0.0".into(),
                open: 2,
                total: 2,
                error: None,
            }),
        }]
    }

    #[test]
    fn lists_forwards_with_their_state() {
        let text: Vec<String> = lines(&sample()).into_iter().map(|l| l.text).collect();
        assert_eq!(
            text,
            [
                " me@db",
                "  L 127.0.0.1:8080 → db:5432 · 1 open",
                "     127.0.0.1:50000 → db:5432",
                "  R server port 9000 → localhost:3000 · stopped",
                "     The server refused.",
                "  D port 1080 SOCKS · starting",
                "     last error: x",
                "  X11 → localhost:0.0 · 2 open",
            ]
            .map(str::to_owned)
        );
    }

    #[test]
    fn adds_and_removes() {
        let view = sample();
        let mut v = ForwardsView::default();
        for c in "-D 1081".chars() {
            assert_eq!(v.key(SideKey::Char(c), &view, 20), None);
        }
        assert_eq!(
            v.key(SideKey::Enter { split: false }, &view, 20),
            Some(FwdAction::Add {
                spec: parse_option("-D 1081").unwrap(),
                connection: None
            })
        );
        v.key(SideKey::Char('x'), &view, 20);
        assert_eq!(v.key(SideKey::Enter { split: false }, &view, 20), None);
        assert!(v.message.is_some());
        v.key(SideKey::Escape, &view, 20);
        assert_eq!(v.key(SideKey::Escape, &view, 20), Some(FwdAction::Leave));
        // The heading is selected first: Delete leaves it alone.
        assert_eq!(v.key(SideKey::Delete, &view, 20), None);
        v.key(SideKey::Down, &view, 20);
        v.key(SideKey::Down, &view, 20);
        assert_eq!(
            v.key(SideKey::Delete, &view, 20),
            Some(FwdAction::Remove {
                connection: 1,
                index: 1
            })
        );
        v.key(SideKey::End, &view, 20);
        v.key(SideKey::Down, &view, 20);
        assert_eq!(
            v.key(SideKey::Delete, &view, 20),
            Some(FwdAction::Remove {
                connection: 1,
                index: 2
            })
        );
        v.click(2 + 1, &view, 20);
        assert_eq!(
            v.key(SideKey::Delete, &view, 20),
            Some(FwdAction::Remove {
                connection: 1,
                index: 0
            })
        );
    }

    #[test]
    fn picking_a_connection_sends_new_forwards_there() {
        let mut view = sample();
        view.push(ConnView {
            connection: 7,
            label: "me@web".into(),
            forwards: Vec::new(),
            x11: None,
        });
        let mut v = ForwardsView::default();
        let add = |v: &mut ForwardsView, view: &[ConnView]| {
            for c in "-L 8000:localhost:8000".chars() {
                v.key(SideKey::Char(c), view, 20);
            }
            v.key(SideKey::Enter { split: false }, view, 20)
        };
        let spec = parse_option("-L 8000:localhost:8000").unwrap();
        // Nothing picked: the window uses the focused pane's connection.
        assert_eq!(
            add(&mut v, &view),
            Some(FwdAction::Add {
                spec: spec.clone(),
                connection: None
            })
        );
        // The second heading is line 8 of the list, below the header.
        v.click(HEADER + 8, &view, 20);
        assert_eq!(
            add(&mut v, &view),
            Some(FwdAction::Add {
                spec: spec.clone(),
                connection: Some(7)
            })
        );
        v.refuse(&spec, "no".into());
        assert_eq!(v.input, "-L 8000:localhost:8000");
        assert_eq!(v.message.as_deref(), Some("no"));
    }
}
