//! The Theme manager's colour editor, without the window: the colours a
//! theme has, in the order the editor lists them, and where an edited
//! theme may be saved. A built-in theme is never written over: its
//! changes go to a new theme file under another name.

use ferossh_render::{Rgb, Theme};

/// How many colours the editor lists: four, then the 16 ANSI colours.
pub const SLOTS: usize = 20;

/// Each colour's label, in the editor's order.
pub const NAMES: [&str; SLOTS] = [
    "Foreground",
    "Background",
    "Cursor",
    "Cursor text",
    "Black",
    "Red",
    "Green",
    "Yellow",
    "Blue",
    "Magenta",
    "Cyan",
    "White",
    "Bright black",
    "Bright red",
    "Bright green",
    "Bright yellow",
    "Bright blue",
    "Bright magenta",
    "Bright cyan",
    "Bright white",
];

/// Colour `slot` of `theme` (see [`NAMES`]).
pub fn get(theme: &Theme, slot: usize) -> Option<Rgb> {
    match slot {
        0 => Some(theme.foreground),
        1 => Some(theme.background),
        2 => Some(theme.cursor),
        3 => Some(theme.cursor_text),
        n => theme.ansi.get(n.checked_sub(4)?).copied(),
    }
}

/// Sets colour `slot` of `theme`; false for no such slot.
pub fn set(theme: &mut Theme, slot: usize, c: Rgb) -> bool {
    let at = match slot {
        0 => &mut theme.foreground,
        1 => &mut theme.background,
        2 => &mut theme.cursor,
        3 => &mut theme.cursor_text,
        n => match n.checked_sub(4).and_then(|i| theme.ansi.get_mut(i)) {
            Some(c) => c,
            None => return false,
        },
    };
    *at = c;
    true
}

/// `c` as the editor and theme files write it: `#RRGGBB`.
pub fn hex(c: Rgb) -> String {
    format!("#{:02X}{:02X}{:02X}", c.r, c.g, c.b)
}

/// A colour typed as `#RRGGBB`, `RRGGBB` or `#RGB`, spaces around it
/// ignored.
pub fn parse(text: &str) -> Option<Rgb> {
    let t = text.trim();
    let t = t.strip_prefix('#').unwrap_or(t);
    if !t.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    match t.len() {
        6 => u32::from_str_radix(t, 16).ok().map(Rgb::hex),
        3 => {
            let v = u32::from_str_radix(t, 16).ok()?;
            let d = |n: u32| ((n & 0xF) * 0x11) as u8;
            Some(Rgb::new(d(v >> 8), d(v >> 4), d(v)))
        }
        _ => None,
    }
}

/// Whether Save may write theme `name` back to its own file: only one
/// of the user's own themes. A built-in one has no file to change, so
/// only Save as is offered.
pub fn can_save(name: &str) -> bool {
    !Theme::is_builtin(name)
}

/// The name Save as suggests for an edited `name`: "My <name>" for a
/// built-in theme (whose name a theme file may not take), else its own.
pub fn suggested_name(name: &str) -> String {
    if Theme::is_builtin(name) {
        format!("My {name}")
    } else {
        name.to_owned()
    }
}

/// The colours that changed from `was` to `now`, by label, for the
/// editor's summary line.
pub fn changed(was: &Theme, now: &Theme) -> Vec<&'static str> {
    NAMES
        .iter()
        .enumerate()
        .filter(|(i, _)| get(was, *i) != get(now, *i))
        .map(|(_, n)| *n)
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn every_slot_reads_and_writes_its_own_colour() {
        let mut t = Theme::by_name("Dracula").unwrap();
        for (slot, name) in NAMES.iter().enumerate() {
            let c = Rgb::new(slot as u8, 1, 2);
            assert!(set(&mut t, slot, c));
            assert_eq!(get(&t, slot), Some(c), "{name}");
        }
        assert_eq!(t.foreground, Rgb::new(0, 1, 2));
        assert_eq!(t.cursor_text, Rgb::new(3, 1, 2));
        assert_eq!(t.ansi.first(), Some(&Rgb::new(4, 1, 2)));
        assert_eq!(t.ansi.last(), Some(&Rgb::new(19, 1, 2)));
        assert!(!set(&mut t, SLOTS, Rgb::new(9, 9, 9)));
        assert_eq!(get(&t, SLOTS), None);
    }

    #[test]
    fn colours_parse_as_typed() {
        assert_eq!(parse("#88C0D0"), Some(Rgb::hex(0x88C0D0)));
        assert_eq!(parse(" 88c0d0 "), Some(Rgb::hex(0x88C0D0)));
        assert_eq!(parse("#fa0"), Some(Rgb::new(0xFF, 0xAA, 0x00)));
        assert_eq!(parse("#12345"), None);
        assert_eq!(parse("#GG0000"), None);
        assert_eq!(parse(""), None);
        assert_eq!(parse("+12345"), None);
        assert_eq!(hex(Rgb::new(1, 0xAB, 0xFF)), "#01ABFF");
        assert_eq!(
            parse(&hex(Rgb::new(1, 0xAB, 0xFF))),
            Some(Rgb::new(1, 0xAB, 0xFF))
        );
    }

    #[test]
    fn a_built_in_theme_is_only_saved_under_a_new_name() {
        assert!(!can_save("Nord"));
        assert!(!can_save("Production"));
        assert_eq!(suggested_name("Nord"), "My Nord");
        assert!(crate::custom_theme::clean_name(&suggested_name("Nord")).is_ok());
        assert!(crate::custom_theme::clean_name("Nord").is_err());
        assert!(can_save("Sandstorm"));
        assert_eq!(suggested_name("Sandstorm"), "Sandstorm");
    }

    #[test]
    fn the_changes_are_named() {
        let was = Theme::by_name("Nord").unwrap();
        let mut now = was.clone();
        assert!(changed(&was, &now).is_empty());
        set(&mut now, 1, Rgb::new(0, 0, 0));
        set(&mut now, 13, Rgb::new(1, 2, 3));
        assert_eq!(changed(&was, &now), ["Background", "Bright red"]);
    }
}
