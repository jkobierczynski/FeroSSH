//! The window's own colours: its menu bar and menus, title bar, tab strip,
//! tools rail and the native tools pane (Settings > Window colours).
//!
//! They follow the focused pane's colour theme by default, so a dark
//! terminal sits in a dark window instead of next to Windows' bright
//! controls; or Windows' own light or dark app mode, its classic light
//! look, or a neutral dark grey. This part only picks the colours; the
//! Win32 side (`ui_theme`) paints with them.

use ferossh_config::WindowColours;
use ferossh_render::{Rgb, Theme};

use crate::tabstrip::{DIM_CONTRAST, Palette, contrast, luminance, mix};

/// The system colours of Windows' light look (`GetSysColor`), and its app
/// mode.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct System {
    pub menu_bar: Rgb,
    pub window: Rgb,
    pub text: Rgb,
    pub gray: Rgb,
    pub highlight: Rgb,
    /// Windows' app mode is dark (Settings > Personalisation > Colours).
    pub dark: bool,
}

/// What the window's own parts are painted with.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UiColours {
    /// Our own colours. Off, Windows draws its usual light look and the
    /// colours below are its system colours.
    pub custom: bool,
    /// Dark: Windows' dark variants for menus, buttons and scroll bars.
    pub dark: bool,
    /// The tools pane's background, and its lists'.
    pub window: Rgb,
    /// Edit boxes and drop-down lists.
    pub control: Rgb,
    pub text: Rgb,
    /// Secondary text, readable on `window` and `bar`.
    pub dim: Rgb,
    /// A problem, readable on `window`.
    pub error: Rgb,
    /// The menu bar and title bar.
    pub bar: Rgb,
    /// A menu bar item under the mouse or open.
    pub hot: Rgb,
    /// The selected tab's line.
    pub accent: Rgb,
    /// A progress bar's empty part.
    pub track: Rgb,
    /// The tab strip's and tools rail's colours.
    pub strip: Palette,
}

/// The fixed dark look: Windows 11's dark greys.
const DARK_BACKGROUND: Rgb = Rgb::new(0x20, 0x20, 0x20);
const DARK_TEXT: Rgb = Rgb::new(0xE8, 0xE8, 0xE8);
/// Windows 11's accent blues for dark and light surfaces.
const DARK_ACCENT: Rgb = Rgb::new(0x4C, 0xC2, 0xFF);
const LIGHT_ACCENT: Rgb = Rgb::new(0x00, 0x5F, 0xB8);

/// The fixed neutral looks: pure greys (no tint) from light to dark, as
/// background, text and accent. `None` for the looks that follow
/// something (the terminal theme, Windows, Windows' light look).
fn neutral(mode: WindowColours) -> Option<(Rgb, Rgb, Rgb)> {
    let grey = |v: u8| Rgb::new(v, v, v);
    Some(match mode {
        WindowColours::Theme | WindowColours::System | WindowColours::Light => return None,
        WindowColours::SoftGrey => (grey(0xF3), grey(0x1A), LIGHT_ACCENT),
        WindowColours::Silver => (grey(0xDC), grey(0x14), LIGHT_ACCENT),
        WindowColours::MidGrey => (grey(0xB4), grey(0x00), LIGHT_ACCENT),
        WindowColours::Steel => (grey(0x5A), grey(0xFF), DARK_ACCENT),
        WindowColours::Graphite => (grey(0x38), grey(0xEA), DARK_ACCENT),
        WindowColours::Dark => (DARK_BACKGROUND, DARK_TEXT, DARK_ACCENT),
        WindowColours::Black => (grey(0x0C), grey(0xE0), DARK_ACCENT),
    })
}

/// Red for problems, before it is made readable on the background.
const RED_ON_DARK: Rgb = Rgb::new(0xFF, 0x70, 0x70);
const RED_ON_LIGHT: Rgb = Rgb::new(0xC0, 0x20, 0x20);

/// The window's colours in `mode`; `theme` is the focused pane's.
pub fn resolve(mode: WindowColours, theme: &Theme, system: &System) -> UiColours {
    if let Some((background, text, accent)) = neutral(mode) {
        return from_colours(background, text, accent);
    }
    match mode {
        WindowColours::Theme => from_colours(theme.background, theme.foreground, theme.cursor),
        WindowColours::System if system.dark => resolve(WindowColours::Dark, theme, system),
        _ => light(system),
    }
}

/// Our own colours from a background, a text colour and an accent.
fn from_colours(background: Rgb, foreground: Rgb, accent: Rgb) -> UiColours {
    let strip = Palette::from_colours(background, foreground, accent);
    let dark = luminance(background) < luminance(foreground);
    let bar = strip.strip;
    let red = if dark { RED_ON_DARK } else { RED_ON_LIGHT };
    UiColours {
        custom: true,
        dark,
        window: background,
        control: lift(background, foreground, 0.06),
        text: foreground,
        dim: strip.dim,
        error: readable(red, background, foreground),
        bar,
        hot: hot(bar, foreground, dark),
        accent,
        track: mix(background, foreground, 0.18),
        strip,
    }
}

/// `base` moved `amount` toward `text`: lighter on a dark background,
/// darker on a light one; less where the text would no longer read on it.
fn lift(base: Rgb, text: Rgb, amount: f32) -> Rgb {
    let mut amount = amount;
    let mut c = mix(base, text, amount);
    while amount > 0.0 && contrast(text, c) < DIM_CONTRAST {
        amount = (amount - 0.01_f32).max(0.0);
        c = mix(base, text, amount);
    }
    c
}

/// A menu bar item under the mouse: lifted off the bar, or where the text
/// leaves no room for that (a low contrast theme), sunk below it.
fn hot(bar: Rgb, text: Rgb, dark: bool) -> Rgb {
    let up = lift(bar, text, 0.16);
    if up != bar {
        return up;
    }
    let away = if dark {
        Rgb::new(0, 0, 0)
    } else {
        Rgb::new(0xFF, 0xFF, 0xFF)
    };
    mix(bar, away, 0.3)
}

/// `colour`, moved toward `text` until it reads on `background`.
fn readable(colour: Rgb, background: Rgb, text: Rgb) -> Rgb {
    let mut amount = 0.0_f32;
    let mut c = colour;
    while amount < 1.0 && contrast(c, background) < DIM_CONTRAST {
        amount = (amount + 0.1).min(1.0);
        c = mix(colour, text, amount);
    }
    c
}

/// Windows' light look, as before window colours existed.
fn light(s: &System) -> UiColours {
    UiColours {
        custom: false,
        dark: false,
        window: s.window,
        control: s.window,
        text: s.text,
        dim: s.gray,
        error: RED_ON_LIGHT,
        bar: s.menu_bar,
        hot: s.highlight,
        accent: s.highlight,
        track: Rgb::new(0xE5, 0xE5, 0xE5),
        strip: Palette::menu_bar(s.menu_bar, s.window, s.text, s.gray, s.highlight),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferossh_render::ThemeGroup;

    fn system(dark: bool) -> System {
        System {
            menu_bar: Rgb::new(0xF0, 0xF0, 0xF0),
            window: Rgb::new(0xFF, 0xFF, 0xFF),
            text: Rgb::new(0, 0, 0),
            gray: Rgb::new(0x6D, 0x6D, 0x6D),
            highlight: Rgb::new(0x00, 0x78, 0xD7),
            dark,
        }
    }

    #[test]
    fn every_theme_gives_readable_window_colours() {
        for theme in Theme::builtin() {
            let c = resolve(WindowColours::Theme, theme, &system(false));
            let name = theme.name;
            assert!(c.custom, "{name}");
            assert_eq!(c.window, theme.background, "{name}");
            assert!(contrast(c.text, c.window) >= DIM_CONTRAST, "{name} text");
            assert!(contrast(c.text, c.control) >= DIM_CONTRAST, "{name} edit");
            // As readable as the theme's own text allows.
            let room = |bg: Rgb| contrast(c.text, bg).min(DIM_CONTRAST) - 0.05;
            assert!(contrast(c.dim, c.window) >= room(c.window), "{name} dim");
            assert!(contrast(c.dim, c.bar) >= room(c.bar), "{name} dim on bar");
            assert!(
                contrast(c.text, c.bar) >= room(c.window) - 0.5,
                "{name} bar"
            );
            assert!(contrast(c.text, c.hot) >= room(c.bar), "{name} hot");
            assert!(c.hot != c.bar, "{name}: a hot item shows");
            assert!(contrast(c.error, c.window) >= DIM_CONTRAST, "{name} error");
        }
    }

    #[test]
    fn dark_themes_make_a_dark_window_and_light_ones_a_light_one() {
        for theme in Theme::builtin() {
            let c = resolve(WindowColours::Theme, theme, &system(false));
            match theme.group() {
                ThemeGroup::Dark => assert!(c.dark, "{}", theme.name),
                ThemeGroup::Light => assert!(!c.dark, "{}", theme.name),
                ThemeGroup::Warning | ThemeGroup::Classic => {}
            }
        }
    }

    #[test]
    fn light_is_the_system_look_and_dark_is_fixed() {
        let nord = Theme::default();
        let light = resolve(WindowColours::Light, &nord, &system(true));
        assert!(!light.custom && !light.dark);
        assert_eq!(light.window, system(true).window);
        assert_eq!(light.bar, system(true).menu_bar);
        let dark = resolve(WindowColours::Dark, &nord, &system(false));
        assert!(dark.custom && dark.dark);
        assert_eq!(dark.window, DARK_BACKGROUND);
        // The fixed looks do not change with the pane's theme.
        let other = Theme::builtin().last().cloned().unwrap_or_default();
        assert_eq!(resolve(WindowColours::Dark, &other, &system(false)), dark);
        assert_eq!(resolve(WindowColours::Light, &other, &system(true)), light);
    }

    #[test]
    fn neutral_looks_are_grey_readable_and_ordered() {
        let nord = Theme::default();
        let mut last = f32::INFINITY;
        for mode in WindowColours::ALL {
            let Some((background, ..)) = neutral(mode) else {
                continue;
            };
            let name = mode.name();
            let c = resolve(mode, &nord, &system(false));
            assert!(c.custom, "{name}");
            assert_eq!(c.window, background, "{name}");
            let grey = |x: Rgb| x.r == x.g && x.g == x.b;
            for (part, x) in [
                ("window", c.window),
                ("control", c.control),
                ("text", c.text),
                ("dim", c.dim),
                ("bar", c.bar),
                ("hot", c.hot),
                ("track", c.track),
            ] {
                assert!(grey(x), "{name} {part} is not neutral");
            }
            // Full readability, not just as much as a theme allows.
            for (part, fg, bg) in [
                ("text", c.text, c.window),
                ("edit", c.text, c.control),
                ("bar", c.text, c.bar),
                ("hot", c.text, c.hot),
                ("dim", c.dim, c.window),
                ("dim on bar", c.dim, c.bar),
                ("error", c.error, c.window),
                ("tab", c.strip.text, c.strip.strip),
                ("dim tab", c.strip.dim, c.strip.strip),
            ] {
                assert!(contrast(fg, bg) >= DIM_CONTRAST, "{name} {part}");
            }
            assert!(c.hot != c.bar, "{name}: a hot item shows");
            assert_eq!(c.dark, luminance(background) < 0.18, "{name} dark");
            // The menu lists them from light to dark.
            let l = luminance(background);
            assert!(l < last, "{name} out of order");
            last = l;
            // The pane's theme does not change them.
            let other = Theme::builtin().last().cloned().unwrap_or_default();
            assert_eq!(resolve(mode, &other, &system(true)), c, "{name}");
        }
    }

    #[test]
    fn windows_mode_follows_the_app_mode() {
        let nord = Theme::default();
        assert_eq!(
            resolve(WindowColours::System, &nord, &system(true)),
            resolve(WindowColours::Dark, &nord, &system(true))
        );
        assert_eq!(
            resolve(WindowColours::System, &nord, &system(false)),
            resolve(WindowColours::Light, &nord, &system(false))
        );
    }
}
