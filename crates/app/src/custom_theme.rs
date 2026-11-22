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

use std::path::Path;

use ferossh_config::WindowColours;
use ferossh_render::{Rgb, Theme};

use crate::tabstrip::{DIM_CONTRAST, contrast};

/// The file extension of a theme file.
pub const EXTENSION: &str = "theme";

/// The most themes the menu lists (its ids' room).
pub const MAX: usize = 50;

/// The least contrast of the window accent with the window (a graphic).
const ACCENT_CONTRAST: f32 = 3.0;

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
    /// Its file ([`load`] sets it; empty for a theme not read from one),
    /// so the colour editor's Save writes it back there, by its own path.
    pub path: std::path::PathBuf,
    /// From the team library ([`add_team`]): not a file here, and used
    /// instead of this PC's theme of the same name.
    pub team: bool,
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
    // '#' starts a comment in a theme file and '"' quotes in the settings
    // file, so a name with them would not read back the same.
    if name.contains(['#', '"']) {
        return Err("its name has a # or a \"".into());
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
            // Built-in themes only: another custom theme may not be read yet.
            "base" => match Theme::builtin()
                .iter()
                .find(|t| t.name.eq_ignore_ascii_case(value))
            {
                Some(t) => base = t.clone(),
                None => warnings.push(format!("no built-in theme named {value}; Nord used")),
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
            // The selected tab's line is a graphic: 3:1 on the window.
            let accent = match accent {
                Some(a) if contrast(a, bg) < ACCENT_CONTRAST => {
                    warnings
                        .push("window accent below 3:1 on its background; text colour used".into());
                    text
                }
                a => a.unwrap_or(text),
            };
            // Text must read on every part made from these colours: the
            // window, the menu bar, a highlighted item, edit boxes, tabs.
            if crate::ui_colours::all_readable(&crate::ui_colours::custom(bg, text, accent)) {
                Some(WindowSpec::Colours(bg, text, accent))
            } else {
                warnings.push(
                    "window text below 4.5:1 on the window, bar or tabs; window colours left out"
                        .into(),
                );
                None
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
        path: std::path::PathBuf::new(),
        team: false,
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
/// family, size and window colours if given.
pub fn write(
    name: &str,
    theme: &Theme,
    font: Option<&str>,
    size: Option<f32>,
    window: Option<WindowSpec>,
) -> String {
    let mut out = String::from(
        "# FeroSSH theme. Change any line; Settings > Custom themes reads it again\n\
         # when it opens. Colours are #RRGGBB. Font, FontSize and the Window\n\
         # lines are optional.\n",
    );
    out.push_str(&format!("Name       {name}\n"));
    if let Some(family) = font {
        out.push_str(&format!("Font       {family}\n"));
    }
    if let Some(size) = size {
        out.push_str(&format!("FontSize   {size}\n"));
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
    // Windows' device names stay devices even with an extension.
    let device = stem.split('.').next().is_some_and(|base| {
        let base = base.trim_end().to_ascii_uppercase();
        matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || ((base.starts_with("COM") || base.starts_with("LPT"))
                && base.len() == 4
                && base.ends_with(|c: char| ('1'..='9').contains(&c)))
    });
    let stem = if device {
        format!("_{stem}")
    } else {
        stem.to_owned()
    };
    format!(
        "{}.{EXTENSION}",
        if stem.is_empty() { "theme" } else { &stem }
    )
}

/// The themes [`load`] read: the usable ones, sorted by name, and the
/// files it could not use.
pub type Loaded = (Vec<CustomTheme>, Vec<Broken>);

/// Reads every theme file in `dir`, sorted by name; at most [`MAX`]. A
/// second file with a name already read is broken. A missing folder has
/// no themes; one that cannot be read is an error, so the caller keeps
/// what it had.
pub fn load(dir: &Path) -> Result<Loaded, String> {
    let entries = match std::fs::read_dir(dir) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((Vec::new(), Vec::new())),
        Err(e) => return Err(format!("the themes folder cannot be read: {e}")),
    };
    let mut paths = Vec::new();
    for entry in entries {
        let path = entry
            .map_err(|e| format!("the themes folder cannot be read: {e}"))?
            .path();
        if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case(EXTENSION))
        {
            paths.push(path);
        }
    }
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
            Ok(mut t) => {
                t.path = path;
                themes.push(t);
            }
            Err(reason) => broken.push(Broken { file, reason }),
        }
    }
    themes.sort_by_key(|t| t.theme.name.to_lowercase());
    Ok((themes, broken))
}

/// Adds the team library's themes (`team`: a theme file's text each) to
/// what [`load`] read. A team theme is used instead of this PC's theme
/// of the same name, as the team's rules come before the user's; the
/// PC's file is then listed as not used. A team theme that cannot be
/// read is listed too. Still at most [`MAX`]: the PC's own go first, and
/// team themes past it are left out.
pub fn add_team(loaded: &mut Loaded, team: &[String]) {
    let (themes, broken) = loaded;
    let mut added: Vec<CustomTheme> = Vec::new();
    for (n, text) in (1..).zip(team) {
        match parse(text, &format!("Team theme {n}")) {
            Ok(t)
                if added
                    .iter()
                    .any(|o| o.theme.name.eq_ignore_ascii_case(t.theme.name)) => {}
            Ok(mut t) => {
                t.team = true;
                added.push(t);
            }
            Err(reason) => broken.push(Broken {
                file: format!("team library theme {n}"),
                reason,
            }),
        }
    }
    // A team theme that replaces one of the PC's takes its slot, so it
    // comes before the team's new names when the list is full.
    added.sort_by_key(|t| {
        !themes
            .iter()
            .any(|own| own.theme.name.eq_ignore_ascii_case(t.theme.name))
    });
    themes.retain(|own| {
        let shadowed = added
            .iter()
            .any(|t| t.theme.name.eq_ignore_ascii_case(own.theme.name));
        if shadowed {
            broken.push(Broken {
                file: own
                    .path
                    .file_name()
                    .map(|f| f.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                reason: format!(
                    "the team library's theme {} is used instead",
                    own.theme.name
                ),
            });
        }
        !shadowed
    });
    // The PC's own stay; new team themes past the limit are left out.
    let room = MAX.saturating_sub(themes.len());
    for extra in added.drain(room.min(added.len())..) {
        broken.push(Broken {
            file: format!("team library theme {}", extra.theme.name),
            reason: format!("more than {MAX} themes with this PC's"),
        });
    }
    themes.extend(added);
    themes.sort_by_key(|t| t.theme.name.to_lowercase());
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn a_full_list_keeps_the_pcs_own_themes() {
        let own: Vec<CustomTheme> = (0..MAX)
            .map(|i| parse(&format!("Name Own {i:02}\n"), "x").unwrap())
            .collect();
        let mut loaded: Loaded = (own, Vec::new());
        // The new name first: the replacement still keeps its slot.
        add_team(
            &mut loaded,
            &["Name Extra\n".into(), "Name Own 07\n".into()],
        );
        let (themes, broken) = loaded;
        assert_eq!(themes.len(), MAX);
        // The same name still replaces; the new one does not fit.
        assert!(themes.iter().any(|t| t.theme.name == "Own 07" && t.team));
        assert!(!themes.iter().any(|t| t.theme.name == "Extra"));
        assert!(broken.iter().any(|b| b.file.ends_with("Extra")));
    }

    #[test]
    fn team_themes_replace_local_ones_of_the_same_name() {
        let mut mine = parse("Name Sandstorm\nBackground #000000\n", "x").unwrap();
        mine.path = std::path::PathBuf::from("Sandstorm.theme");
        let other = parse("Name Mine\n", "x").unwrap();
        let mut loaded: Loaded = (vec![mine, other], Vec::new());
        add_team(
            &mut loaded,
            &[
                "Name sandstorm\nBackground #112233\n".into(),
                "Name Nord\n".into(),
            ],
        );
        let (themes, broken) = loaded;
        let names: Vec<&str> = themes.iter().map(|t| t.theme.name).collect();
        assert_eq!(names, ["Mine", "sandstorm"]);
        assert!(themes[1].team && !themes[0].team);
        assert_eq!(themes[1].theme.background, Rgb::hex(0x0011_2233));
        assert_eq!(broken.len(), 2, "{broken:?}");
        assert_eq!(broken[0].file, "team library theme 2");
        assert_eq!(broken[1].file, "Sandstorm.theme");
    }

    #[test]
    fn a_written_theme_reads_back_the_same() {
        let mut t = Theme::by_name("Dracula").unwrap_or_default();
        t.name = "My Dracula";
        let text = write(
            "My Dracula",
            &t,
            Some("Cascadia Mono"),
            Some(11.5),
            Some(WindowSpec::Look(WindowColours::Slate)),
        );
        let back = parse(&text, "file").expect("reads");
        assert_eq!(back.theme, t);
        assert_eq!(back.font.as_deref(), Some("Cascadia Mono"));
        assert_eq!(back.size, Some(11.5));
        assert_eq!(back.window, Some(WindowSpec::Look(WindowColours::Slate)));
        assert!(back.warnings.is_empty(), "{:?}", back.warnings);

        let own = WindowSpec::Colours(Rgb::hex(0xECE5D8), Rgb::hex(0x2A241C), Rgb::hex(0x8C5A1E));
        let back = parse(&write("My Dracula", &t, None, None, Some(own)), "file").expect("reads");
        assert_eq!(back.window, Some(own));
        assert_eq!(back.font, None);
        // A size alone (a theme file may set only that) stays alone.
        let back = parse(&write("My Dracula", &t, None, Some(9.0), None), "file").expect("reads");
        assert_eq!((back.font, back.size), (None, Some(9.0)));
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
        assert!(parse("", "Night #2").is_err());
        assert!(parse("", "Night \"Blue\"").is_err());
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
        // 4.54:1 on the window, but not on the bar made from it.
        let text = "WindowBackground #FFFFFF\nWindowText #767676\n";
        assert_eq!(parse(text, "x").expect("reads").window, None);
        // An accent that would vanish on the window becomes the text colour.
        let text = "WindowBackground #000000\nWindowText #FFFFFF\nWindowAccent #000000\n";
        let t = parse(text, "x").expect("reads");
        let white = Rgb::hex(0xFFFFFF);
        assert_eq!(
            t.window,
            Some(WindowSpec::Colours(Rgb::hex(0), white, white))
        );
        // Base takes built-in themes only.
        assert_eq!(
            parse("Base My Own\n", "x").expect("reads").warnings.len(),
            1
        );
        let low = "Foreground #333333\nBackground #2E3440\n";
        assert_eq!(parse(low, "x").expect("reads").warnings.len(), 1);
    }

    #[test]
    fn file_names_are_safe() {
        assert_eq!(file_name("Sand / Night?"), "Sand _ Night_.theme");
        assert_eq!(file_name(" .. "), "theme.theme");
        assert_eq!(file_name("nul"), "_nul.theme");
        assert_eq!(file_name("COM1"), "_COM1.theme");
        assert_eq!(file_name("Console"), "Console.theme");
    }

    #[test]
    fn the_folder_is_read_sorted_with_duplicates_and_junk_reported() {
        /// A folder only this test made, removed even when it fails.
        struct Dir(std::path::PathBuf);
        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir =
            std::env::temp_dir().join(format!("ferossh-themes-{}-{nanos}", std::process::id()));
        std::fs::create_dir(&dir).expect("a new folder");
        let guard = Dir(dir.clone());
        let put = |f: &str, t: &str| std::fs::write(dir.join(f), t).expect("write");
        put("b.theme", "Name Zed\n");
        put("a.theme", "Name alpha\n");
        put("c.theme", "Name ZED\n");
        put("d.theme", "Background nope\n");
        put("notes.txt", "Name Ignored\n");
        let (themes, broken) = load(&dir).expect("reads");
        let names: Vec<_> = themes.iter().map(|t| t.theme.name).collect();
        assert_eq!(names, ["alpha", "Zed"]);
        assert_eq!(broken.len(), 2);
        drop(guard);
        assert_eq!(load(&dir), Ok((Vec::new(), Vec::new())), "missing is empty");
    }
}
