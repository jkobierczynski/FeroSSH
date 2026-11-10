//! tmux's window layout strings, such as
//! `ff69,70x25,0,0{35x25,0,0,0,34x25,36,0,2}`: a checksum, then a cell
//! of `WIDTHxHEIGHT,X,Y` that is either a pane (`,ID`), panes side by side
//! (`{...}`) or panes one above the other (`[...]`). Neighbours are one
//! cell apart, for tmux's border.

use crate::PaneId;

/// Deeper layouts are refused.
const MAX_DEPTH: usize = 32;
/// Layouts with more panes are refused.
const MAX_PANES: usize = 256;
/// Larger sizes are refused.
pub const MAX_SIZE: u16 = 10_000;

/// One cell of a layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    pub width: u16,
    pub height: u16,
    pub x: u16,
    pub y: u16,
    pub kind: Kind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    Pane(PaneId),
    /// Side by side, left to right.
    Row(Vec<Cell>),
    /// One above the other, top to bottom.
    Column(Vec<Cell>),
}

impl Cell {
    /// The panes, first to last.
    pub fn panes(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        self.collect(&mut out);
        out
    }

    fn collect(&self, out: &mut Vec<PaneId>) {
        match &self.kind {
            Kind::Pane(p) => out.push(*p),
            Kind::Row(cells) | Kind::Column(cells) => {
                for c in cells {
                    c.collect(out);
                }
            }
        }
    }

    /// Each pane's size, in columns and rows.
    pub fn sizes(&self) -> Vec<(PaneId, u16, u16)> {
        let mut out = Vec::new();
        self.collect_sizes(&mut out);
        out
    }

    fn collect_sizes(&self, out: &mut Vec<(PaneId, u16, u16)>) {
        match &self.kind {
            Kind::Pane(p) => out.push((*p, self.width, self.height)),
            Kind::Row(cells) | Kind::Column(cells) => {
                for c in cells {
                    c.collect_sizes(out);
                }
            }
        }
    }

    /// The same split structure and panes, whatever the sizes.
    pub fn same_shape(&self, other: &Cell) -> bool {
        match (&self.kind, &other.kind) {
            (Kind::Pane(a), Kind::Pane(b)) => a == b,
            (Kind::Row(a), Kind::Row(b)) | (Kind::Column(a), Kind::Column(b)) => {
                a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.same_shape(y))
            }
            _ => false,
        }
    }

    /// The layout string, checksum included, as `select-layout` takes it.
    pub fn to_layout_string(&self) -> String {
        let mut body = String::new();
        self.write(&mut body);
        format!("{:04x},{body}", checksum(body.as_bytes()))
    }

    fn write(&self, out: &mut String) {
        use std::fmt::Write as _;
        let _ = write!(out, "{}x{},{},{}", self.width, self.height, self.x, self.y);
        match &self.kind {
            Kind::Pane(p) => {
                let _ = write!(out, ",{p}");
            }
            Kind::Row(cells) | Kind::Column(cells) => {
                let (open, close) = if matches!(self.kind, Kind::Row(_)) {
                    ('{', '}')
                } else {
                    ('[', ']')
                };
                out.push(open);
                for (i, c) in cells.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    c.write(out);
                }
                out.push(close);
            }
        }
    }
}

/// tmux's layout checksum (`layout_checksum` in layout-custom.c).
pub fn checksum(body: &[u8]) -> u16 {
    body.iter().fold(0u16, |csum, &b| {
        let rotated = (csum >> 1) | ((csum & 1) << 15);
        rotated.wrapping_add(u16::from(b))
    })
}

/// Reads a layout string. The checksum is skipped, not checked: tmux
/// wrote it, and a wrong one only means a strange layout.
pub fn parse(text: &str) -> Option<Cell> {
    let (_, body) = text.split_once(',')?;
    let mut p = Parser {
        s: body.as_bytes(),
        at: 0,
        panes: 0,
    };
    let cell = p.cell(0)?;
    (p.at == p.s.len()).then_some(cell)
}

struct Parser<'a> {
    s: &'a [u8],
    at: usize,
    panes: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.at).copied()
    }

    fn eat(&mut self, b: u8) -> Option<()> {
        (self.peek()? == b).then(|| self.at += 1)
    }

    fn number(&mut self) -> Option<u32> {
        let start = self.at;
        while self.peek().is_some_and(|b| b.is_ascii_digit()) {
            self.at += 1;
        }
        let digits = self.s.get(start..self.at)?;
        if digits.is_empty() || digits.len() > 9 {
            return None;
        }
        std::str::from_utf8(digits).ok()?.parse().ok()
    }

    fn size(&mut self) -> Option<u16> {
        u16::try_from(self.number()?)
            .ok()
            .filter(|&n| n <= MAX_SIZE)
    }

    fn cell(&mut self, depth: usize) -> Option<Cell> {
        if depth > MAX_DEPTH {
            return None;
        }
        let width = self.size()?;
        self.eat(b'x')?;
        let height = self.size()?;
        self.eat(b',')?;
        let x = self.size()?;
        self.eat(b',')?;
        let y = self.size()?;
        let kind = match self.peek() {
            Some(b',') => {
                self.at += 1;
                self.panes += 1;
                if self.panes > MAX_PANES {
                    return None;
                }
                Kind::Pane(self.number()?)
            }
            Some(open @ (b'{' | b'[')) => {
                self.at += 1;
                let close = if open == b'{' { b'}' } else { b']' };
                let mut cells = vec![self.cell(depth + 1)?];
                while self.peek() == Some(b',') {
                    self.at += 1;
                    cells.push(self.cell(depth + 1)?);
                }
                self.eat(close)?;
                if open == b'{' {
                    Kind::Row(cells)
                } else {
                    Kind::Column(cells)
                }
            }
            _ => return None,
        };
        Some(Cell {
            width,
            height,
            x,
            y,
            kind,
        })
    }
}

/// Builds a cell from a tree of sizes, placing children from `(x, y)`
/// one border apart. Used to write a layout for `select-layout`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shape {
    Pane(PaneId),
    Row(Vec<(u16, Shape)>),
    Column(Vec<(u16, Shape)>),
}

/// Lays `shape` out in `width` x `height` at `(x, y)`. Children of a row
/// get the widths they ask for (the last one the rest) and the row's
/// height; columns likewise. `None` when the sizes do not fit.
pub fn place(shape: &Shape, width: u16, height: u16, x: u16, y: u16) -> Option<Cell> {
    let kind = match shape {
        Shape::Pane(p) => Kind::Pane(*p),
        Shape::Row(children) | Shape::Column(children) => {
            let row = matches!(shape, Shape::Row(_));
            let total = if row { width } else { height };
            let n = u16::try_from(children.len()).ok()?;
            let mut left = total.checked_sub(n.checked_sub(1)?)?;
            let mut at = if row { x } else { y };
            let mut cells = Vec::with_capacity(children.len());
            for (i, (want, child)) in children.iter().enumerate() {
                let last = i + 1 == children.len();
                let rest = u16::try_from(children.len() - i - 1).ok()?;
                let len = if last {
                    left
                } else {
                    (*want).clamp(1, left.checked_sub(rest)?.max(1))
                };
                if len == 0 {
                    return None;
                }
                let cell = if row {
                    place(child, len, height, at, y)?
                } else {
                    place(child, width, len, x, at)?
                };
                cells.push(cell);
                left = left.checked_sub(len)?;
                at = at.checked_add(len)?.checked_add(1)?;
            }
            if row {
                Kind::Row(cells)
            } else {
                Kind::Column(cells)
            }
        }
    };
    Some(Cell {
        width,
        height,
        x,
        y,
        kind,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn reads_tmux_layouts() {
        let one = parse("a1dd,60x20,0,0,0").unwrap();
        assert_eq!(one.kind, Kind::Pane(0));
        assert_eq!((one.width, one.height), (60, 20));
        let split = parse("ff69,70x25,0,0{35x25,0,0,0,34x25,36,0,2}").unwrap();
        assert_eq!(split.panes(), [0, 2]);
        assert_eq!(split.sizes(), [(0, 35, 25), (2, 34, 25)]);
        let nested =
            parse("bd8d,80x24,0,0[80x12,0,0,1,80x11,0,13{40x11,0,13,2,39x11,41,13,3}]").unwrap();
        assert_eq!(nested.panes(), [1, 2, 3]);
        assert!(matches!(nested.kind, Kind::Column(ref c) if c.len() == 2));
    }

    #[test]
    fn writes_what_tmux_writes() {
        for text in [
            "a1dd,60x20,0,0,0",
            "ff69,70x25,0,0{35x25,0,0,0,34x25,36,0,2}",
            "b61d,70x25,0,0,0",
            "9f9e,50x10,0,0,1",
        ] {
            assert_eq!(parse(text).unwrap().to_layout_string(), text);
        }
    }

    #[test]
    fn refuses_bad_layouts() {
        for bad in [
            "",
            "abcd",
            "abcd,80x24,0,0",
            "abcd,80x24,0,0,",
            "abcd,80x24,0,0{}",
            "abcd,80x24,0,0{80x24,0,0,1",
            "abcd,80x24,0,0,1junk",
            "abcd,99999x24,0,0,1",
            "abcd,80x24,0,0,1234567890",
        ] {
            assert_eq!(parse(bad), None, "{bad}");
        }
        let deep = format!("0000,{}1x1,0,0,1{}", "1x1,0,0{".repeat(40), "}".repeat(40));
        assert_eq!(parse(&deep), None);
        let many: Vec<String> = (0..300).map(|i| format!("1x1,0,0,{i}")).collect();
        assert_eq!(parse(&format!("0000,80x1,0,0{{{}}}", many.join(","))), None);
    }

    #[test]
    fn places_shapes() {
        let shape = Shape::Column(vec![
            (12, Shape::Pane(1)),
            (
                0,
                Shape::Row(vec![(40, Shape::Pane(2)), (0, Shape::Pane(3))]),
            ),
        ]);
        let cell = place(&shape, 80, 24, 0, 0).unwrap();
        assert_eq!(
            cell.to_layout_string(),
            parse("bd8d,80x24,0,0[80x12,0,0,1,80x11,0,13{40x11,0,13,2,39x11,41,13,3}]")
                .unwrap()
                .to_layout_string()
        );
        // Asked-for sizes that do not fit are squeezed.
        let wide = Shape::Row(vec![(500, Shape::Pane(1)), (500, Shape::Pane(2))]);
        assert_eq!(
            place(&wide, 80, 24, 0, 0).unwrap().sizes(),
            [(1, 78, 24), (2, 1, 24)]
        );
        assert_eq!(place(&wide, 2, 24, 0, 0), None);
    }
}
