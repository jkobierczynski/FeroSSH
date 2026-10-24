//! Colour themes. Each pane holds its own theme.

use crate::cell::{Color, Rgb};

/// The colours a pane draws with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Theme {
    pub name: &'static str,
    pub foreground: Rgb,
    pub background: Rgb,
    pub cursor: Rgb,
    /// Text drawn under a block cursor.
    pub cursor_text: Rgb,
    /// The 16 ANSI colours: 0-7 normal, 8-15 bright.
    pub ansi: [Rgb; 16],
}

fn palette(v: [u32; 16]) -> [Rgb; 16] {
    v.map(Rgb::hex)
}

impl Theme {
    /// Dark theme using the Campbell palette from Windows Terminal.
    pub fn dark() -> Self {
        Self {
            name: "Dark",
            foreground: Rgb::hex(0xCCCCCC),
            background: Rgb::hex(0x0C0C0C),
            cursor: Rgb::hex(0xCCCCCC),
            cursor_text: Rgb::hex(0x0C0C0C),
            ansi: palette([
                0x0C0C0C, 0xC50F1F, 0x13A10E, 0xC19C00, 0x0037DA, 0x881798, 0x3A96DD, 0xCCCCCC,
                0x767676, 0xE74856, 0x16C60C, 0xF9F1A5, 0x3B78FF, 0xB4009E, 0x61D6D6, 0xF2F2F2,
            ]),
        }
    }

    /// Light theme using the One Half Light palette.
    pub fn light() -> Self {
        Self {
            name: "Light",
            foreground: Rgb::hex(0x383A42),
            background: Rgb::hex(0xFAFAFA),
            cursor: Rgb::hex(0x4F525D),
            cursor_text: Rgb::hex(0xFAFAFA),
            ansi: palette([
                0x383A42, 0xE45649, 0x50A14F, 0xC18401, 0x0184BC, 0xA626A4, 0x0997B3, 0xFAFAFA,
                0x4F525D, 0xDF6C75, 0x98C379, 0xE4C07A, 0x61AFEF, 0xC577DD, 0x56B5C1, 0xFFFFFF,
            ]),
        }
    }

    /// The built-in themes, in menu order.
    pub fn builtin() -> [Theme; 2] {
        [Self::dark(), Self::light()]
    }

    /// One of the 256 xterm colours.
    pub fn indexed(&self, i: u8) -> Rgb {
        match i {
            0..=15 => self.ansi.get(usize::from(i)).copied().unwrap_or_default(),
            16..=231 => {
                const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
                let n = i - 16;
                let level = |v: u8| LEVELS.get(usize::from(v)).copied().unwrap_or(0);
                Rgb::new(level(n / 36), level(n / 6 % 6), level(n % 6))
            }
            232..=255 => {
                let v = 8 + 10 * (i - 232);
                Rgb::new(v, v, v)
            }
        }
    }

    pub fn fg(&self, c: Color) -> Rgb {
        self.resolve(c, self.foreground)
    }

    pub fn bg(&self, c: Color) -> Rgb {
        self.resolve(c, self.background)
    }

    fn resolve(&self, c: Color, default: Rgb) -> Rgb {
        match c {
            Color::Default => default,
            Color::Indexed(i) => self.indexed(i),
            Color::Rgb(rgb) => rgb,
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xterm_cube_and_greys() {
        let t = Theme::dark();
        assert_eq!(t.indexed(16), Rgb::new(0, 0, 0));
        assert_eq!(t.indexed(196), Rgb::new(255, 0, 0));
        assert_eq!(t.indexed(231), Rgb::new(255, 255, 255));
        assert_eq!(t.indexed(232), Rgb::new(8, 8, 8));
        assert_eq!(t.indexed(255), Rgb::new(238, 238, 238));
    }

    #[test]
    fn themes_resolve_the_same_cell_differently() {
        let (d, l) = (Theme::dark(), Theme::light());
        assert_ne!(d.fg(Color::Indexed(1)), l.fg(Color::Indexed(1)));
        assert_ne!(d.bg(Color::Default), l.bg(Color::Default));
        let rgb = Color::Rgb(Rgb::new(1, 2, 3));
        assert_eq!(d.fg(rgb), l.fg(rgb));
    }
}
