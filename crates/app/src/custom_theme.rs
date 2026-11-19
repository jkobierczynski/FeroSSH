//! The user's own themes (Settings > Custom themes): a text file each in the
//! themes folder, giving a terminal palette and, if wanted, a font and
//! window colours. "Save this look as a theme" writes one from the
//! focused pane; any text editor changes it, and the menu reads the
//! folder again each time it opens.
//!
//! ```text
//! # FeroSSH theme
//! Name       Sandstorm
//! Base       Nord            # start from a built-in theme (optional)
//! Font       Cascadia Mono   # optional
//! FontSize   11              # optional
//! Foreground #D8DEE9
//! Background #2E3440
//! Cursor     #88C0D0
//! CursorText #2E3440
//! Color0     #3B4252         # Color0 to Color15: the ANSI colours
//! Window     sand            # a Window colours look (optional), or
//! WindowBackground #ECE5D8   # the window's own three colours
//! WindowText       #2A241C
//! WindowAccent     #8C5A1E
//! ```

use std::path::{Path, PathBuf};

use ferossh_config::WindowColours;
use ferossh_render::{Rgb, Theme};

use crate::tabstrip::{DIM_CONTRAST, contrast};

/// The file extension of a theme file.
pub const EXTENSION: &str = "theme";

/// The most themes the menu lists (its ids' room).
pub const MAX: usize = 50;

/// The longest theme name, in characters.
const MAX_NAME: usize = 40;

/// The window colours a theme brings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WindowSpec {
    /// One of Settings > Window colours' fixed looks.
    Look(WindowColours),
    /// Its own background, text and accent.
    Colours(Rgb, Rgb, Rgb),
}

/// One theme file, read.
#[derive(Clone, Debug, PartialEq)]
pub struct CustomTheme {
    pub theme: Theme,
    pub font: Option<String>,
    pub size: Option<f32>,
    pub window: Option<WindowSpec>,
    /// What was wrong but left out, for the menu to show.
    pub warnings: Vec<String>,
}

/// A theme file that could not be used: its file name and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Broken {
    pub file: String,
    pub reason: String,
}

fn parse_colour(word: &str) -> Option<Rgb> {
    let hex = word.strip_prefix('#').unwrap_or(word);
    if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(hex, 16).ok().map(Rgb::hex)
}

/// `line` without its comment: from a `#` that does not start a colour.
fn strip_comment(line: &str) -> &str {
    let mut from = 0;
    while let Some(at) = line.get(from..).and_then(|l| l.find('#')).map(|i| from + i) {
        let rest = line.get(at + 1..).unwrap_or_default();
        let colour = rest
            .get(..6)
            .is_some_and(|h| h.chars().all(|c| c.is_ascii_hexdigit()))
            && rest
                .get(6..)
                .and_then(|r| r.chars().next())
                .is_none_or(char::is_whitespace);
        if !colour {
            return line.get(..at).unwrap_or_default();
        }
        from = at + 1;
    }
    line
}

fn hex(c: Rgb) -> String {
    format!("#{:02X}{:02X}{:02X}", c.r, c.g, c.b)
}

/// A usable theme name: trimmed, printable, not too long.
pub fn clean_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("it has no name".into());
    }
    if name.chars().count() > MAX_NAME {
        return Err(format!("its name is longer than {MAX_NAME} characters"));
    }
    if name.chars().any(char::is_control) {
        return Err("its name has control characters".into());
    }
    if Theme::is_builtin(name) {
        return Err(format!("{name} is a built-in theme's name"));
    }
    Ok(name.to_owned())
}

/// Reads a theme file's text; `fallback` names it when it has no `Name`
/// line (its file name).
pub fn parse(text: &str, fallback: &str) -> Result<CustomTheme, String> {
    let mut name = None;
    let mut base = Theme::default();
    let mut colours: Vec<(String, Rgb)> = Vec::new();
    let mut font = None;
    let mut size = None;
    let mut look = None;
    let mut window = [None; 3];
    let mut warnings = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = strip_comment(line);
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (key, value) = line
            .split_once(char::is_whitespace)
            .map_or((line, ""), |(k, v)| (k, v.trim()));
        let line_no = n + 1;
        let colour = || {
            parse_colour(value)
                .ok_or_else(|| format!("line {line_no}: {value} is not a #RRGGBB colour"))
        };
        match key.to_ascii_lowercase().as_str() {
            "name" => name = Some(value.to_owned()),
            "base" => match Theme::by_name(value) {
                Some(t) => base = t,
                None => warnings.push(format!("no theme named {value}; Nord used")),
            },
            "font" if !value.is_empty() => font = Some(value.to_owned()),
            "fontsize" => match value.parse::<f32>() {
                Ok(s) if (6.0..=72.0).contains(&s) => size = Some(s),
                _ => warnings.push(format!("line {line_no}: font size 6 to 72")),
            },
            "window" => match WindowColours::parse(value) {
                Some(WindowColours::Theme | WindowColours::System | WindowColours::Light)
                | None => warnings.push(format!("line {line_no}: no window colours named {value}")),
                Some(c) => look = Some(c),
            },
            "windowbackground" => window[0] = Some(colour()?),
            "windowtext" => window[1] = Some(colour()?),
            "windowaccent" => window[2] = Some(colour()?),
            k @ ("foreground" | "background" | "cursor" | "cursortext") => {
                colours.push((k.to_owned(), colour()?));
            }
            k if k.starts_with("color") => {
                let i: usize = k
                    .strip_prefix("color")
                    .and_then(|i| i.parse().ok())
                    .filter(|i| *i < 16)
                    .ok_or_else(|| format!("line {line_no}: Color0 to Color15"))?;
                colours.push((format!("color{i}"), colour()?));
            }
            _ => warnings.push(format!("line {line_no}: unknown {key}")),
        }
    }
    let name = clean_name(name.as_deref().unwrap_or(fallback))?;
    let mut t = base;
    for (key, c) in colours {
        match key.as_str() {
            "foreground" => t.foreground = c,
            "background" => t.background = c,
            "cursor" => t.cursor = c,
            "cursortext" => t.cursor_text = c,
            k => {
                if let Some(slot) = k
                    .strip_prefix("color")
                    .and_then(|i| i.parse::<usize>().ok())
                    .and_then(|i| t.ansi.get_mut(i))
                {
                    *slot = c;
                }
            }
        }
    }
    if contrast(t.foreground, t.background) < DIM_CONTRAST {
        warnings.push("its text is hard to read on its background".into());
    }
    let window = match (look, window) {
        (_, [Some(bg), Some(text), accent]) => {
            if contrast(text, bg) < DIM_CONTRAST {
                warnings.push(
                    "window text below 4.5:1 on its background; window colours left out".into(),
                );
                None
            } else {
                Some(WindowSpec::Colours(bg, text, accent.unwrap_or(text)))
            }
        }
        (Some(c), [None, None, None]) => Some(WindowSpec::Look(c)),
        (look, [None, None, None]) => look.map(WindowSpec::Look),
        (look, _) => {
            warnings.push("WindowBackground needs WindowText too".into());
            look.map(WindowSpec::Look)
        }
    };
    // The name lives as long as the program: theme names are `'static`.
    // A theme is read again only when its menu opens, so this stays small.
    t.name = intern(&name);
    Ok(CustomTheme {
        theme: t,
        font,
        size,
        window,
        warnings,
    })
}

/// A `'static` copy of `name`, made once per distinct name.
fn intern(name: &str) -> &'static str {
    use std::sync::{Mutex, PoisonError};
    static NAMES: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
    let mut names = NAMES.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(n) = names.iter().find(|n| **n == name) {
        return n;
    }
    let n: &'static str = Box::leak(name.to_owned().into_boxed_str());
    names.push(n);
    n
}

/// A theme file's text: theme `name` with `theme`'s colours, and a font
/// and window colours if given.
pub fn write(
    name: &str,
    theme: &Theme,
    font: Option<(&str, f32)>,
    window: Option<WindowSpec>,
) -> String {
    let mut out = String::from(
        "# FeroSSH theme. Change any line; Settings > Custom themes reads it again\n\
         # when it opens. Colours are #RRGGBB. Font, FontSize and the Window\n\
         # lines are optional.\n",
    );
    out.push_str(&format!("Name       {name}\n"));
    if let Some((family, size)) = font {
        out.push_str(&format!("Font       {family}\nFontSize   {size}\n"));
    }
    out.push_str(&format!("Foreground {}\n", hex(theme.foreground)));
    out.push_str(&format!("Background {}\n", hex(theme.background)));
    out.push_str(&format!("Cursor     {}\n", hex(theme.cursor)));
    out.push_str(&format!("CursorText {}\n", hex(theme.cursor_text)));
    for (i, c) in theme.ansi.iter().enumerate() {
        out.push_str(&format!("Color{i:<5} {}\n", hex(*c)));
    }
    match window {
        Some(WindowSpec::Look(c)) => out.push_str(&format!("Window     {}\n", c.name())),
        Some(WindowSpec::Colours(bg, text, accent)) => {
            out.push_str(&format!("WindowBackground {}\n", hex(bg)));
            out.push_str(&format!("WindowText       {}\n", hex(text)));
            out.push_str(&format!("WindowAccent     {}\n", hex(accent)));
        }
        None => {}
    }
    out
}

/// A file name for theme `name`: characters Windows refuses become `_`.
pub fn file_name(name: &str) -> String {
    let stem: String = name
        .trim()
        .chars()
        .map(|c| {
            if c.is_control() || r#"<>:"/\|?*"#.contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let stem = stem.trim_end_matches(['.', ' ']);
    format!(
        "{}.{EXTENSION}",
        if stem.is_empty() { "theme" } else { stem }
    )
}

/// Reads every theme file in `dir`, sorted by name; at most [`MAX`]. A
/// second file with a name already read is broken.
pub fn load(dir: &Path) -> (Vec<CustomTheme>, Vec<Broken>) {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|d| {
            d.flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.extension()
                        .is_some_and(|e| e.eq_ignore_ascii_case(EXTENSION))
                })
                .collect()
        })
        .unwrap_or_default();
    paths.sort();
    let mut themes: Vec<CustomTheme> = Vec::new();
    let mut broken = Vec::new();
    for path in paths {
        let file = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let stem = path
            .file_stem()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        // Theme files are small; anything big is not one.
        let text = match std::fs::metadata(&path) {
            Ok(m) if m.len() > 64 * 1024 => Err("larger than 64 KB".to_owned()),
            _ => std::fs::read_to_string(&path).map_err(|e| e.to_string()),
        };
        match text.and_then(|t| parse(&t, &stem)) {
            Ok(t)
                if themes
                    .iter()
                    .any(|o| o.theme.name.eq_ignore_ascii_case(t.theme.name)) =>
            {
                broken.push(Broken {
                    file,
                    reason: format!("another file is named {} too", t.theme.name),
                });
            }
            Ok(_) if themes.len() >= MAX => broken.push(Broken {
                file,
                reason: format!("more than {MAX} themes"),
            }),
            Ok(t) => themes.push(t),
            Err(reason) => broken.push(Broken { file, reason }),
        }
    }
    themes.sort_by_key(|t| t.theme.name.to_lowercase());
    (themes, broken)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn a_written_theme_reads_back_the_same() {
        let mut t = Theme::by_name("Dracula").unwrap_or_default();
        t.name = "My Dracula";
        let text = write(
            "My Dracula",
            &t,
            Some(("Cascadia Mono", 11.5)),
            Some(WindowSpec::Look(WindowColours::Slate)),
        );
        let back = parse(&text, "file").expect("reads");
        assert_eq!(back.theme, t);
        assert_eq!(back.font.as_deref(), Some("Cascadia Mono"));
        assert_eq!(back.size, Some(11.5));
        assert_eq!(back.window, Some(WindowSpec::Look(WindowColours::Slate)));
        assert!(back.warnings.is_empty(), "{:?}", back.warnings);

        let own = WindowSpec::Colours(Rgb::hex(0xECE5D8), Rgb::hex(0x2A241C), Rgb::hex(0x8C5A1E));
        let back = parse(&write("My Dracula", &t, None, Some(own)), "file").expect("reads");
        assert_eq!(back.window, Some(own));
        assert_eq!(back.font, None);
    }

    #[test]
    fn a_short_file_starts_from_its_base_and_its_file_name() {
        let text = "Base Solarized Dark  # a comment\nBackground #101010\n";
        let t = parse(text, "Night").expect("reads");
        assert_eq!(t.theme.name, "Night");
        let base = Theme::by_name("Solarized Dark").unwrap_or_default();
        assert_eq!(t.theme.foreground, base.foreground);
        assert_eq!(t.theme.background, Rgb::hex(0x101010));
        assert_eq!(t.window, None);
    }

    #[test]
    fn bad_files_say_why() {
        assert!(
            parse("Background #12345\n", "x")
                .unwrap_err()
                .contains("line 1")
        );
        assert!(parse("Color16 #123456\n", "x").is_err());
        assert!(parse("Name Nord\n", "x").unwrap_err().contains("built-in"));
        assert!(parse("", "  ").is_err());
        let t = parse("FontSize 200\nWindow purple\nFoo bar\n", "x").expect("reads");
        assert_eq!(t.warnings.len(), 3, "{:?}", t.warnings);
    }

    #[test]
    fn unreadable_window_colours_are_left_out() {
        let text = "WindowBackground #808080\nWindowText #888888\n";
        let t = parse(text, "x").expect("reads");
        assert_eq!(t.window, None);
        assert_eq!(t.warnings.len(), 1);
        let low = "Foreground #333333\nBackground #2E3440\n";
        assert_eq!(parse(low, "x").expect("reads").warnings.len(), 1);
    }

    #[test]
    fn file_names_are_safe() {
        assert_eq!(file_name("Sand / Night?"), "Sand _ Night_.theme");
        assert_eq!(file_name(" .. "), "theme.theme");
    }

    #[test]
    fn the_folder_is_read_sorted_with_duplicates_and_junk_reported() {
        let dir = std::env::temp_dir().join(format!("ferossh-themes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let put = |f: &str, t: &str| std::fs::write(dir.join(f), t).expect("write");
        put("b.theme", "Name Zed\n");
        put("a.theme", "Name alpha\n");
        put("c.theme", "Name ZED\n");
        put("d.theme", "Background nope\n");
        put("notes.txt", "Name Ignored\n");
        let (themes, broken) = load(&dir);
        let names: Vec<_> = themes.iter().map(|t| t.theme.name).collect();
        assert_eq!(names, ["alpha", "Zed"]);
        assert_eq!(broken.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
