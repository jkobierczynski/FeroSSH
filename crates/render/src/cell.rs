//! One grid cell: a character, its colours and its attributes.

/// A 24-bit colour.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Parses `0xRRGGBB`.
    pub const fn hex(v: u32) -> Self {
        Self::new((v >> 16) as u8, (v >> 8) as u8, v as u8)
    }
}

/// A cell colour as the terminal stores it; the pane's theme resolves it.
///
/// Keeping indexed colours symbolic until drawing is what lets each pane
/// switch theme without touching its grid.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Color {
    /// The theme's foreground or background, depending on where it is used.
    #[default]
    Default,
    /// One of the 256 xterm colours; 0-15 come from the theme.
    Indexed(u8),
    /// Truecolour, drawn as is.
    Rgb(Rgb),
}

/// Cell attributes, as a small bit set.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Flags(u8);

impl Flags {
    pub const BOLD: Flags = Flags(1);
    pub const ITALIC: Flags = Flags(1 << 1);
    pub const UNDERLINE: Flags = Flags(1 << 2);
    pub const INVERSE: Flags = Flags(1 << 3);
    /// The left half of a double-width character.
    pub const WIDE: Flags = Flags(1 << 4);
    /// The right half of a double-width character; draws nothing itself.
    pub const WIDE_SPACER: Flags = Flags(1 << 5);
    /// Faint text: drawn halfway between foreground and background.
    pub const DIM: Flags = Flags(1 << 6);
    pub const STRIKEOUT: Flags = Flags(1 << 7);

    pub const fn empty() -> Self {
        Flags(0)
    }

    pub const fn contains(self, other: Flags) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn insert(&mut self, other: Flags) {
        self.0 |= other.0;
    }

    pub fn remove(&mut self, other: Flags) {
        self.0 &= !other.0;
    }

    #[must_use]
    pub const fn union(self, other: Flags) -> Self {
        Flags(self.0 | other.0)
    }
}

/// One character cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub fg: Color,
    pub bg: Color,
    pub flags: Flags,
    /// Combining marks drawn with `ch`, as an index into the grid's cluster
    /// table plus one; 0 means none.
    pub cluster: u16,
}

impl Cell {
    pub const BLANK: Cell = Cell {
        ch: ' ',
        fg: Color::Default,
        bg: Color::Default,
        flags: Flags::empty(),
        cluster: 0,
    };

    /// A cell with no combining marks.
    pub const fn new(ch: char, fg: Color, bg: Color, flags: Flags) -> Self {
        Self {
            ch,
            fg,
            bg,
            flags,
            cluster: 0,
        }
    }

    /// True when the cell shows no glyph (its background may still show).
    pub fn is_empty_glyph(&self) -> bool {
        self.ch == ' ' || self.flags.contains(Flags::WIDE_SPACER)
    }
}

impl Default for Cell {
    fn default() -> Self {
        Cell::BLANK
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_insert_and_remove() {
        let mut f = Flags::BOLD.union(Flags::UNDERLINE);
        assert!(f.contains(Flags::BOLD));
        assert!(!f.contains(Flags::ITALIC));
        f.remove(Flags::BOLD);
        assert!(!f.contains(Flags::BOLD));
        f.insert(Flags::ITALIC);
        assert!(f.contains(Flags::ITALIC.union(Flags::UNDERLINE)));
    }

    #[test]
    fn hex_colour() {
        assert_eq!(Rgb::hex(0x12_34_56), Rgb::new(0x12, 0x34, 0x56));
    }
}
