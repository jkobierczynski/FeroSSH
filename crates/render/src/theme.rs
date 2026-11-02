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
    /// Dark themes first, then light ones, the warning themes and the
    /// classics (see [`Theme::group`]).
    ///
    /// Each palette is the theme's published terminal palette (as collected
    /// in iTerm2-Color-Schemes, MIT); the themes themselves are MIT
    /// licensed except Tokyo Night's terminal port (Apache-2.0) and Tango
    /// (public domain). PuTTY's are its default colours (MIT). Colour
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

    /// Which menu group this theme belongs to.
    pub fn group(&self) -> Group {
        match self.name {
            "Production" | "Staging" => Group::Warning,
            "Campbell" | "One Half Dark" | "One Half Light" | "PuTTY" | "Tango Dark"
            | "Tango Light" => Group::Classic,
            _ if self.is_light() => Group::Light,
            _ => Group::Dark,
        }
    }

    /// Whether the background is light (dark text on a pale page).
    pub fn is_light(&self) -> bool {
        let b = self.background;
        // Rec. 601 luma, on 0..=255.
        u32::from(b.r) * 299 + u32::from(b.g) * 587 + u32::from(b.b) * 114 > 128 * 1000
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

/// The groups the theme menu shows, in this order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    Dark,
    Light,
    /// Production and Staging, for marking hosts.
    Warning,
    /// The Windows Terminal schemes and PuTTY's colours.
    Classic,
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

static BUILTIN: [Theme; 44] = [
    // Dark themes, Nord first.
    NORD,
    Theme::new(
        "Ayu Mirage",
        0xCCCAC2,
        0x1F2430,
        0xFFCC66,
        0x1F2430,
        [
            0x171B24, 0xED8274, 0x87D96C, 0xFACC6E, 0x6DCBFA, 0xDABAFA, 0x90E1C6, 0xC7C7C7, //
            0x686868, 0xF28779, 0xD5FF80, 0xFFD173, 0x73D0FF, 0xDFBFFF, 0x95E6CB, 0xFFFFFF,
        ],
    ),
    Theme::new(
        "Carbonfox",
        0xF2F4F8,
        0x161616,
        0xF2F4F8,
        0x161616,
        [
            0x282828, 0xEE5396, 0x25BE6A, 0x08BDBA, 0x78A9FF, 0xBE95FF, 0x33B1FF, 0xDFDFE0, //
            0x484848, 0xF16DA6, 0x46C880, 0x2DC7C4, 0x8CB6FF, 0xC8A5FF, 0x52BDFF, 0xE4E4E5,
        ],
    ),
    Theme::new(
        "Catppuccin Frapp\u{e9}",
        0xC6D0F5,
        0x303446,
        0xF2D5CF,
        0x303446,
        [
            0x51576D, 0xE78284, 0xA6D189, 0xE5C890, 0x8CAAEE, 0xF4B8E4, 0x81C8BE, 0xB5BFE2, //
            0x626880, 0xEDA0A2, 0xB9DBA2, 0xECD7AE, 0xADC2F3, 0xF38ED8, 0x98D2CA, 0xA5ADCE,
        ],
    ),
    Theme::new(
        "Catppuccin Macchiato",
        0xCAD3F5,
        0x24273A,
        0xF4DBD6,
        0x24273A,
        [
            0x494D64, 0xED8796, 0xA6DA95, 0xEED49F, 0x8AADF4, 0xF5BDE6, 0x8BD5CA, 0xB8C0E0, //
            0x5B6078, 0xF2A7B2, 0xBDE3B0, 0xF4E3C1, 0xADC5F7, 0xF493DA, 0xA5DED6, 0xA5ADCB,
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
    Theme::new(
        "GitHub Dark",
        0xE6EDF3,
        0x0D1117,
        0x2F81F7,
        0x0D1117,
        [
            0x484F58, 0xFF7B72, 0x3FB950, 0xD29922, 0x58A6FF, 0xBC8CFF, 0x39C5CF, 0xB1BAC4, //
            0x6E7681, 0xFFA198, 0x56D364, 0xE3B341, 0x79C0FF, 0xD2A8FF, 0x56D4DD, 0xFFFFFF,
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
        "Gruvbox Material",
        0xD4BE98,
        0x282828,
        0xD4BE98,
        0x282828,
        [
            0x282828, 0xEA6962, 0xA9B665, 0xD8A657, 0x7DAEA3, 0xD3869B, 0x89B482, 0xD4BE98, //
            0x7C6F64, 0xEA6962, 0xA9B665, 0xD8A657, 0x7DAEA3, 0xD3869B, 0x89B482, 0xDDC7A1,
        ],
    ),
    Theme::new(
        "Kanagawa Dragon",
        0xC5C9C5,
        0x181616,
        0xC8C093,
        0x181616,
        [
            0x0D0C0C, 0xC4746E, 0x8A9A7B, 0xC4B28A, 0x8BA4B0, 0xA292A3, 0x8EA4A2, 0xC8C093, //
            0xA6A69C, 0xE46876, 0x87A987, 0xE6C384, 0x7FB4CA, 0x938AA9, 0x7AA89F, 0xC5C9C5,
        ],
    ),
    Theme::new(
        "Kanagawa Wave",
        0xDCD7BA,
        0x1F1F28,
        0xDCD7BA,
        0x1F1F28,
        [
            0x090618, 0xC34043, 0x76946A, 0xC0A36E, 0x7E9CD8, 0x957FB8, 0x6A9589, 0xC8C093, //
            0x727169, 0xE82424, 0x98BB6C, 0xE6C384, 0x7FB4CA, 0x938AA9, 0x7AA89F, 0xDCD7BA,
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
        "Night Owl",
        0xD6DEEB,
        0x011627,
        0x7E57C2,
        0x011627,
        [
            0x011627, 0xEF5350, 0x22DA6E, 0xADDB67, 0x82AAFF, 0xC792EA, 0x21C7A8, 0xFFFFFF, //
            0x575656, 0xEF5350, 0x22DA6E, 0xFFEB95, 0x82AAFF, 0xC792EA, 0x7FDBCA, 0xFFFFFF,
        ],
    ),
    Theme::new(
        "Nightfox",
        0xCDCECF,
        0x192330,
        0xCDCECF,
        0x192330,
        [
            0x393B44, 0xC94F6D, 0x81B29A, 0xDBC074, 0x719CD6, 0x9D79D6, 0x63CDCF, 0xDFDFE0, //
            0x575860, 0xD16983, 0x8EBAA4, 0xE0C989, 0x86ABDC, 0xBAA1E2, 0x7AD5D6, 0xE4E4E5,
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
        "Ros\u{e9} Pine Moon",
        0xE0DEF4,
        0x232136,
        0xE0DEF4,
        0x232136,
        [
            0x393552, 0xEB6F92, 0x3E8FB0, 0xF6C177, 0x9CCFD8, 0xC4A7E7, 0xEA9A97, 0xE0DEF4, //
            0x6E6A86, 0xEB6F92, 0x3E8FB0, 0xF6C177, 0x9CCFD8, 0xC4A7E7, 0xEA9A97, 0xE0DEF4,
        ],
    ),
    Theme::new(
        "Snazzy",
        0xEBECE6,
        0x1E1F29,
        0xE4E4E4,
        0x1E1F29,
        [
            0x000000, 0xFC4346, 0x50FB7C, 0xF0FB8C, 0x49BAFF, 0xFC4CB4, 0x8BE9FE, 0xEDEDEC, //
            0x555555, 0xFC4346, 0x50FB7C, 0xF0FB8C, 0x49BAFF, 0xFC4CB4, 0x8BE9FE, 0xEDEDEC,
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
        "Tokyo Night Storm",
        0xC0CAF5,
        0x24283B,
        0xC0CAF5,
        0x24283B,
        [
            0x1D202F, 0xF7768E, 0x9ECE6A, 0xE0AF68, 0x7AA2F7, 0xBB9AF7, 0x7DCFFF, 0xA9B1D6, //
            0x4E5575, 0xF7768E, 0x9ECE6A, 0xE0AF68, 0x7AA2F7, 0xBB9AF7, 0x7DCFFF, 0xC0CAF5,
        ],
    ),
    Theme::new(
        "Tomorrow Night",
        0xC5C8C6,
        0x1D1F21,
        0xC5C8C6,
        0x1D1F21,
        [
            0x000000, 0xCC6666, 0xB5BD68, 0xF0C674, 0x81A2BE, 0xB294BB, 0x8ABEB7, 0xFFFFFF, //
            0x4C4C4C, 0xCC6666, 0xB5BD68, 0xF0C674, 0x81A2BE, 0xB294BB, 0x8ABEB7, 0xFFFFFF,
        ],
    ),
    Theme::new(
        "Tomorrow Night Eighties",
        0xCCCCCC,
        0x2D2D2D,
        0xCCCCCC,
        0x2D2D2D,
        [
            0x000000, 0xF2777A, 0x99CC99, 0xFFCC66, 0x6699CC, 0xCC99CC, 0x66CCCC, 0xFFFFFF, //
            0x595959, 0xF2777A, 0x99CC99, 0xFFCC66, 0x6699CC, 0xCC99CC, 0x66CCCC, 0xFFFFFF,
        ],
    ),
    // Light themes.
    Theme::new(
        "Ayu Light",
        0x5C6166,
        0xF8F9FA,
        0xFFAA33,
        0xF8F9FA,
        [
            0x000000, 0xEA6C6D, 0x6CBF43, 0xECA944, 0x3199E1, 0x9E75C7, 0x46BA94, 0xBABABA, //
            0x686868, 0xF07171, 0x86B300, 0xF2AE49, 0x399EE6, 0xA37ACC, 0x4CBF99, 0xD1D1D1,
        ],
    ),
    Theme::new(
        "Catppuccin Latte",
        0x4C4F69,
        0xEFF1F5,
        0xDC8A78,
        0xEFF1F5,
        [
            0xBCC0CC, 0xD20F39, 0x40A02B, 0xDF8E1D, 0x1E66F5, 0xEA76CB, 0x179299, 0x5C5F77, //
            0xACB0BE, 0xE7103F, 0x46B02F, 0xE49931, 0x3878F6, 0xEF95D7, 0x19A1A8, 0x6C6F85,
        ],
    ),
    Theme::new(
        "Dawnfox",
        0x575279,
        0xFAF4ED,
        0x575279,
        0xFAF4ED,
        [
            0x575279, 0xB4637A, 0x618774, 0xEA9D34, 0x286983, 0x907AA9, 0x56949F, 0xB2B6BD, //
            0x5F5695, 0xC26D85, 0x629F81, 0xEEA846, 0x2D81A3, 0x9A80B9, 0x5CA7B4, 0xE6EBF3,
        ],
    ),
    Theme::new(
        "Everforest Light",
        0x5C6A72,
        0xEFEBD4,
        0xF57D26,
        0xEFEBD4,
        [
            0x7A8478, 0xE67E80, 0x9AB373, 0xC1A266, 0x7FBBB3, 0xD699B6, 0x83C092, 0xB2AF9F, //
            0xA6B0A0, 0xF85552, 0x8DA101, 0xDFA000, 0x3A94C5, 0xDF69BA, 0x35A77C, 0xFFFBEF,
        ],
    ),
    Theme::new(
        "GitHub Light",
        0x1F2328,
        0xFFFFFF,
        0x0969DA,
        0xFFFFFF,
        [
            0x24292F, 0xCF222E, 0x116329, 0x4D2D00, 0x0969DA, 0x8250DF, 0x1B7C83, 0x6E7781, //
            0x57606A, 0xA40E26, 0x1A7F37, 0x633C01, 0x218BFF, 0xA475F9, 0x3192AA, 0x8C959F,
        ],
    ),
    Theme::new(
        "Gruvbox Light",
        0x3C3836,
        0xFBF1C7,
        0x3C3836,
        0xFBF1C7,
        [
            0xFBF1C7, 0xCC241D, 0x98971A, 0xD79921, 0x458588, 0xB16286, 0x689D6A, 0x7C6F64, //
            0x928374, 0x9D0006, 0x79740E, 0xB57614, 0x076678, 0x8F3F71, 0x427B58, 0x3C3836,
        ],
    ),
    Theme::new(
        "Kanagawa Lotus",
        0x545464,
        0xF2ECBC,
        0x43436C,
        0xF2ECBC,
        [
            0x1F1F28, 0xC84053, 0x6F894E, 0x77713F, 0x4D699B, 0xB35B79, 0x597B75, 0x545464, //
            0x8A8980, 0xD7474B, 0x6E915F, 0x836F4A, 0x6693BF, 0x624C83, 0x5E857A, 0x43436C,
        ],
    ),
    Theme::new(
        "Ros\u{e9} Pine Dawn",
        0x575279,
        0xFAF4ED,
        0x575279,
        0xFAF4ED,
        [
            0xF2E9E1, 0xB4637A, 0x286983, 0xEA9D34, 0x56949F, 0x907AA9, 0xD7827E, 0x575279, //
            0x9893A5, 0xB4637A, 0x286983, 0xEA9D34, 0x56949F, 0x907AA9, 0xD7827E, 0x575279,
        ],
    ),
    // Text in base01 rather than base00: base00 on base3 is only 4.1:1.
    Theme::new(
        "Solarized Light",
        0x586E75,
        0xFDF6E3,
        0x657B83,
        0xFDF6E3,
        [
            0x073642, 0xDC322F, 0x859900, 0xB58900, 0x268BD2, 0xD33682, 0x2AA198, 0xBBB5A2, //
            0x002B36, 0xCB4B16, 0x586E75, 0x657B83, 0x839496, 0x6C71C4, 0x93A1A1, 0xFDF6E3,
        ],
    ),
    Theme::new(
        "Tokyo Night Day",
        0x3760BF,
        0xE1E2E7,
        0x3760BF,
        0xE1E2E7,
        [
            0xE9E9ED, 0xF52A65, 0x587539, 0x8C6C3E, 0x2E7DE9, 0x9854F1, 0x007197, 0x6172B0, //
            0xA1A6C5, 0xF52A65, 0x587539, 0x8C6C3E, 0x2E7DE9, 0x9854F1, 0x007197, 0x3760BF,
        ],
    ),
    Theme::new(
        "Tomorrow",
        0x4D4D4C,
        0xFFFFFF,
        0x4D4D4C,
        0xFFFFFF,
        [
            0x000000, 0xC82829, 0x718C00, 0xEAB700, 0x4271AE, 0x8959A8, 0x3E999F, 0xBFBFBF, //
            0x000000, 0xC82829, 0x718C00, 0xEAB700, 0x4271AE, 0x8959A8, 0x3E999F, 0xFFFFFF,
        ],
    ),
    // Warning themes.
    PRODUCTION,
    STAGING,
    // Classics: the Windows Terminal schemes and PuTTY's own colours.
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
        "One Half Dark",
        0xDCDFE4,
        0x282C34,
        0xA3B3CC,
        0x282C34,
        [
            0x282C34, 0xE06C75, 0x98C379, 0xE5C07B, 0x61AFEF, 0xC678DD, 0x56B6C2, 0xDCDFE4, //
            0x5D677A, 0xE06C75, 0x98C379, 0xE5C07B, 0x61AFEF, 0xC678DD, 0x56B6C2, 0xDCDFE4,
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
    Theme::new(
        "PuTTY",
        0xBBBBBB,
        0x000000,
        0x00FF00,
        0x000000,
        [
            0x000000, 0xBB0000, 0x00BB00, 0xBBBB00, 0x0000BB, 0xBB00BB, 0x00BBBB, 0xBBBBBB, //
            0x555555, 0xFF5555, 0x55FF55, 0xFFFF55, 0x5555FF, 0xFF55FF, 0x55FFFF, 0xFFFFFF,
        ],
    ),
    Theme::new(
        "Tango Dark",
        0xFFFFFF,
        0x000000,
        0xFFFFFF,
        0x000000,
        [
            0x000000, 0xCC0000, 0x4E9A06, 0xC4A000, 0x3465A4, 0x75507B, 0x06989A, 0xD3D7CF, //
            0x555753, 0xEF2929, 0x8AE234, 0xFCE94F, 0x729FCF, 0xAD7FA8, 0x34E2E2, 0xEEEEEC,
        ],
    ),
    Theme::new(
        "Tango Light",
        0x000000,
        0xFFFFFF,
        0x000000,
        0xFFFFFF,
        [
            0x000000, 0xCC0000, 0x4E9A06, 0xC4A000, 0x3465A4, 0x75507B, 0x06989A, 0xB9BDB5, //
            0x555753, 0xEF2929, 0x7DD527, 0xD6C329, 0x729FCF, 0xAD7FA8, 0x27D5D5, 0xEEEEEC,
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
    fn builtins_are_grouped_in_menu_order() {
        let groups: Vec<Group> = Theme::builtin().iter().map(Theme::group).collect();
        let mut seen = groups.clone();
        seen.dedup();
        assert_eq!(
            seen,
            [Group::Dark, Group::Light, Group::Warning, Group::Classic]
        );
        for t in Theme::builtin() {
            match t.group() {
                Group::Dark | Group::Warning => assert!(!t.is_light(), "{}", t.name),
                Group::Light => assert!(t.is_light(), "{}", t.name),
                Group::Classic => {}
            }
        }
        assert!(Theme::by_name("tango light").is_some_and(|t| t.is_light()));
    }

    #[test]
    fn foreground_reads_on_every_theme() {
        // WCAG contrast of the default text on the default background.
        let lum = |c: Rgb| {
            let ch = |v: u8| {
                let v = f64::from(v) / 255.0;
                if v <= 0.039_28 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * ch(c.r) + 0.7152 * ch(c.g) + 0.0722 * ch(c.b)
        };
        for t in Theme::builtin() {
            let (a, b) = (lum(t.foreground), lum(t.background));
            let c = (a.max(b) + 0.05) / (a.min(b) + 0.05);
            assert!(c >= 4.5, "{}: {c:.2}", t.name);
            assert_ne!(t.cursor, t.background, "{}", t.name);
        }
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
