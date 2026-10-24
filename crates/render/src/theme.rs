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

impl Theme {
    // `i < 16` indexes two `[_; 16]` arrays; `get` is not const.
    #[allow(clippy::indexing_slicing)]
    const fn new(
        name: &'static str,
        fg: u32,
        bg: u32,
        cursor: u32,
        cursor_text: u32,
        ansi: [u32; 16],
    ) -> Self {
        let mut rgb = [Rgb::new(0, 0, 0); 16];
        let mut i = 0;
        while i < 16 {
            rgb[i] = Rgb::hex(ansi[i]);
            i += 1;
        }
        Self {
            name,
            foreground: Rgb::hex(fg),
            background: Rgb::hex(bg),
            cursor: Rgb::hex(cursor),
            cursor_text: Rgb::hex(cursor_text),
            ansi: rgb,
        }
    }

    /// The built-in themes, in menu order. Nord is the default.
    ///
    /// Each palette is the theme's published terminal palette (as collected
    /// in iTerm2-Color-Schemes, MIT); the themes themselves are MIT
    /// licensed except Tokyo Night's terminal port (Apache-2.0). Colour
    /// values are all FeroSSH takes from them.
    pub fn builtin() -> &'static [Theme] {
        &BUILTIN
    }

    /// A built-in theme by name, ignoring case.
    pub fn by_name(name: &str) -> Option<Theme> {
        BUILTIN
            .iter()
            .find(|t| t.name.eq_ignore_ascii_case(name))
            .cloned()
    }

    /// The default theme: Nord.
    pub fn nord() -> Self {
        NORD
    }

    /// This theme with its background and ANSI black pulled toward `tint`
    /// by `amount` (0 keeps the theme, 255 replaces the colour). Marks a
    /// pane as dangerous without giving up the user's palette.
    pub fn tinted(&self, tint: Rgb, amount: u8) -> Self {
        let mix = |c: Rgb| {
            let ch = |a: u8, b: u8| {
                let (a, b, t) = (u16::from(a), u16::from(b), u16::from(amount));
                ((a * (255 - t) + b * t + 127) / 255) as u8
            };
            Rgb::new(ch(c.r, tint.r), ch(c.g, tint.g), ch(c.b, tint.b))
        };
        let mut t = self.clone();
        t.background = mix(t.background);
        if let Some(black) = t.ansi.first_mut() {
            *black = mix(*black);
        }
        t
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
        Self::nord()
    }
}

const NORD: Theme = Theme::new(
    "Nord",
    0xD8DEE9,
    0x2E3440,
    0xECEFF4,
    0x2E3440,
    [
        0x3B4252, 0xBF616A, 0xA3BE8C, 0xEBCB8B, 0x81A1C1, 0xB48EAD, 0x88C0D0, 0xE5E9F0, //
        0x596377, 0xBF616A, 0xA3BE8C, 0xEBCB8B, 0x81A1C1, 0xB48EAD, 0x8FBCBB, 0xECEFF4,
    ],
);

/// Nord on a muted red background with a red cursor: a pane on a
/// production host should never be mistaken for anything else.
const PRODUCTION: Theme = Theme::new(
    "Production",
    0xD8DEE9,
    0x3B2226,
    0xBF616A,
    0x3B2226,
    [
        0x4A2C31, 0xBF616A, 0xA3BE8C, 0xEBCB8B, 0x81A1C1, 0xB48EAD, 0x88C0D0, 0xE5E9F0, //
        0x6E5257, 0xD08770, 0xA3BE8C, 0xEBCB8B, 0x81A1C1, 0xB48EAD, 0x8FBCBB, 0xECEFF4,
    ],
);

/// Nord on a dark amber background with an amber cursor, for staging.
const STAGING: Theme = Theme::new(
    "Staging",
    0xD8DEE9,
    0x36301F,
    0xEBCB8B,
    0x36301F,
    [
        0x463E29, 0xBF616A, 0xA3BE8C, 0xEBCB8B, 0x81A1C1, 0xB48EAD, 0x88C0D0, 0xE5E9F0, //
        0x6B634C, 0xBF616A, 0xA3BE8C, 0xEBCB8B, 0x81A1C1, 0xB48EAD, 0x8FBCBB, 0xECEFF4,
    ],
);

static BUILTIN: [Theme; 14] = [
    NORD,
    Theme::new(
        "Dracula",
        0xF8F8F2,
        0x282A36,
        0xF8F8F2,
        0x282A36,
        [
            0x21222C, 0xFF5555, 0x50FA7B, 0xF1FA8C, 0xBD93F9, 0xFF79C6, 0x8BE9FD, 0xF8F8F2, //
            0x6272A4, 0xFF6E6E, 0x69FF94, 0xFFFFA5, 0xD6ACFF, 0xFF92DF, 0xA4FFFF, 0xFFFFFF,
        ],
    ),
    Theme::new(
        "Catppuccin Mocha",
        0xCDD6F4,
        0x1E1E2E,
        0xF5E0DC,
        0x1E1E2E,
        [
            0x45475A, 0xF38BA8, 0xA6E3A1, 0xF9E2AF, 0x89B4FA, 0xF5C2E7, 0x94E2D5, 0xBAC2DE, //
            0x585B70, 0xF7AEC2, 0xC2ECBF, 0xFCD682, 0xAECCFC, 0xF398DA, 0xB1EAE1, 0xA6ADC8,
        ],
    ),
    Theme::new(
        "Gruvbox Dark",
        0xEBDBB2,
        0x282828,
        0xEBDBB2,
        0x282828,
        [
            0x282828, 0xCC241D, 0x98971A, 0xD79921, 0x458588, 0xB16286, 0x689D6A, 0xA89984, //
            0x928374, 0xFB4934, 0xB8BB26, 0xFABD2F, 0x83A598, 0xD3869B, 0x8EC07C, 0xEBDBB2,
        ],
    ),
    Theme::new(
        "Solarized Dark",
        0x839496,
        0x002B36,
        0x93A1A1,
        0x002B36,
        [
            0x073642, 0xDC322F, 0x859900, 0xB58900, 0x268BD2, 0xD33682, 0x2AA198, 0xEEE8D5, //
            0x335E69, 0xCB4B16, 0x586E75, 0x657B83, 0x839496, 0x6C71C4, 0x93A1A1, 0xFDF6E3,
        ],
    ),
    Theme::new(
        "Tokyo Night",
        0xC0CAF5,
        0x1A1B26,
        0xC0CAF5,
        0x1A1B26,
        [
            0x15161E, 0xF7768E, 0x9ECE6A, 0xE0AF68, 0x7AA2F7, 0xBB9AF7, 0x7DCFFF, 0xA9B1D6, //
            0x414868, 0xF7768E, 0x9ECE6A, 0xE0AF68, 0x7AA2F7, 0xBB9AF7, 0x7DCFFF, 0xC0CAF5,
        ],
    ),
    Theme::new(
        "One Dark",
        0xABB2BF,
        0x282C34,
        0x528BFF,
        0x282C34,
        [
            0x21252B, 0xE06C75, 0x98C379, 0xE5C07B, 0x61AFEF, 0xC678DD, 0x56B6C2, 0xABB2BF, //
            0x5C6370, 0xE06C75, 0x98C379, 0xE5C07B, 0x61AFEF, 0xC678DD, 0x56B6C2, 0xFFFFFF,
        ],
    ),
    Theme::new(
        "Monokai",
        0xF8F8F2,
        0x272822,
        0xF8F8F0,
        0x272822,
        [
            0x272822, 0xF92672, 0xA6E22E, 0xE6DB74, 0xFD971F, 0xAE81FF, 0x66D9EF, 0xF8F8F2, //
            0x75715E, 0xF92672, 0xA6E22E, 0xE6DB74, 0xFD971F, 0xAE81FF, 0x66D9EF, 0xF9F8F5,
        ],
    ),
    Theme::new(
        "Ros\u{e9} Pine",
        0xE0DEF4,
        0x191724,
        0xE0DEF4,
        0x191724,
        [
            0x26233A, 0xEB6F92, 0x31748F, 0xF6C177, 0x9CCFD8, 0xC4A7E7, 0xEBBCBA, 0xE0DEF4, //
            0x6E6A86, 0xEB6F92, 0x31748F, 0xF6C177, 0x9CCFD8, 0xC4A7E7, 0xEBBCBA, 0xE0DEF4,
        ],
    ),
    Theme::new(
        "Everforest",
        0xD3C6AA,
        0x2D353B,
        0xD3C6AA,
        0x2D353B,
        [
            0x475258, 0xE67E80, 0xA7C080, 0xDBBC7F, 0x7FBBB3, 0xD699B6, 0x83C092, 0x9DA9A0, //
            0x7A8478, 0xE67E80, 0xA7C080, 0xDBBC7F, 0x7FBBB3, 0xD699B6, 0x83C092, 0xD3C6AA,
        ],
    ),
    PRODUCTION,
    STAGING,
    // Campbell, from Windows Terminal.
    Theme::new(
        "Campbell",
        0xCCCCCC,
        0x0C0C0C,
        0xCCCCCC,
        0x0C0C0C,
        [
            0x0C0C0C, 0xC50F1F, 0x13A10E, 0xC19C00, 0x0037DA, 0x881798, 0x3A96DD, 0xCCCCCC, //
            0x767676, 0xE74856, 0x16C60C, 0xF9F1A5, 0x3B78FF, 0xB4009E, 0x61D6D6, 0xF2F2F2,
        ],
    ),
    Theme::new(
        "One Half Light",
        0x383A42,
        0xFAFAFA,
        0x4F525D,
        0xFAFAFA,
        [
            0x383A42, 0xE45649, 0x50A14F, 0xC18401, 0x0184BC, 0xA626A4, 0x0997B3, 0xFAFAFA, //
            0x4F525D, 0xDF6C75, 0x98C379, 0xE4C07A, 0x61AFEF, 0xC577DD, 0x56B5C1, 0xFFFFFF,
        ],
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xterm_cube_and_greys() {
        let t = Theme::nord();
        assert_eq!(t.indexed(16), Rgb::new(0, 0, 0));
        assert_eq!(t.indexed(196), Rgb::new(255, 0, 0));
        assert_eq!(t.indexed(231), Rgb::new(255, 255, 255));
        assert_eq!(t.indexed(232), Rgb::new(8, 8, 8));
        assert_eq!(t.indexed(255), Rgb::new(238, 238, 238));
    }

    #[test]
    fn themes_resolve_the_same_cell_differently() {
        let d = Theme::nord();
        let l = Theme::by_name("one half light").unwrap_or_default();
        assert_ne!(d.fg(Color::Indexed(1)), l.fg(Color::Indexed(1)));
        assert_ne!(d.bg(Color::Default), l.bg(Color::Default));
        let rgb = Color::Rgb(Rgb::new(1, 2, 3));
        assert_eq!(d.fg(rgb), l.fg(rgb));
    }

    #[test]
    fn builtin_names_are_unique_and_found() {
        let names: std::collections::HashSet<_> = Theme::builtin().iter().map(|t| t.name).collect();
        assert_eq!(names.len(), Theme::builtin().len());
        for t in Theme::builtin() {
            assert_eq!(
                Theme::by_name(&t.name.to_ascii_uppercase()).as_ref(),
                Some(t)
            );
        }
        assert_eq!(Theme::default().name, "Nord");
        assert!(Theme::by_name("Ros\u{e9} Pine").is_some());
    }

    #[test]
    fn warning_themes_stand_apart_from_nord() {
        let nord = Theme::nord();
        for name in ["Production", "Staging"] {
            let t = Theme::by_name(name).unwrap_or_default();
            assert_ne!(t.background, nord.background, "{name}");
            assert_ne!(t.cursor, nord.cursor, "{name}");
        }
    }

    #[test]
    fn tint_mixes_background_only() {
        let t = Theme::nord();
        let red = Rgb::new(255, 0, 0);
        assert_eq!(t.tinted(red, 0), t);
        let full = t.tinted(red, 255);
        assert_eq!(full.background, red);
        assert_eq!(full.ansi[0], red);
        assert_eq!(full.ansi[1..], t.ansi[1..]);
        assert_eq!(full.foreground, t.foreground);
        let half = t.tinted(Rgb::new(0, 0, 0), 128);
        assert_eq!(half.background, Rgb::new(0x17, 0x1A, 0x20));
    }
}
