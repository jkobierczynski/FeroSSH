//! Finding the program a local shell command starts.
//!
//! `CreateProcessW` given only a command line looks for a bare name such
//! as `cmd.exe` in FeroSSH's own folder and the current folder before the
//! system folders. FeroSSH is one exe that is often run from Downloads,
//! where a planted `cmd.exe` would then run instead of the real one. So
//! the local shell resolves the program itself, in the system folder and
//! the absolute folders on `PATH` only, and passes it as the
//! application name.

use std::path::{Path, PathBuf};

/// The program part of `command`: up to the closing quote when it starts
/// with one, else up to the first space or tab.
pub fn program(command: &str) -> &str {
    let command = command.trim_start();
    match command.strip_prefix('"') {
        Some(rest) => rest.split('"').next().unwrap_or_default(),
        None => command.split([' ', '\t']).next().unwrap_or_default(),
    }
}

/// Where `command`'s program is. A path (one with a folder in it) is used
/// as given; a bare name is looked for in `dirs`, skipping relative ones,
/// as given and with `.exe` added when it has no extension. `None` when a
/// bare name is in none of them.
pub fn resolve(command: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    let name = program(command);
    if name.is_empty() {
        return None;
    }
    if name.contains(['\\', '/', ':']) {
        return Some(PathBuf::from(name));
    }
    // CreateProcessW adds no `.exe` to an application name, so `tool.exe`
    // goes before a `tool` next to it.
    let mut names = Vec::new();
    if Path::new(name).extension().is_none() {
        names.push(format!("{name}.exe"));
    }
    names.push(name.to_owned());
    dirs.iter()
        .filter(|d| is_absolute(d))
        .flat_map(|d| names.iter().map(move |n| d.join(n)))
        .find(|p| p.is_file())
}

/// Absolute on Windows (`C:\…`, `\\server\…`) as well as here, so the
/// rule can be tested off Windows.
fn is_absolute(dir: &Path) -> bool {
    if dir.is_absolute() {
        return true;
    }
    let s = dir.to_string_lossy();
    s.starts_with("\\\\")
        || matches!(s.as_bytes(), [d, b':', b'\\' | b'/', ..] if d.is_ascii_alphabetic())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn takes_the_program_from_a_command_line() {
        assert_eq!(program("cmd.exe /k echo"), "cmd.exe");
        assert_eq!(program("  wsl"), "wsl");
        assert_eq!(
            program("\"C:\\Program Files\\PowerShell\\7\\pwsh.exe\" -NoLogo"),
            "C:\\Program Files\\PowerShell\\7\\pwsh.exe"
        );
        assert_eq!(program(""), "");
    }

    #[test]
    fn finds_bare_names_only_in_absolute_folders() {
        let root = std::env::temp_dir().join(format!("ferossh-program-{}", std::process::id()));
        let sys = root.join("sys");
        std::fs::create_dir_all(&sys).unwrap();
        std::fs::write(sys.join("cmd.exe"), b"").unwrap();
        std::fs::write(sys.join("wsl.exe"), b"").unwrap();
        // A planted copy in a relative folder (like ".") is never chosen.
        let planted = PathBuf::from("ferossh-program-test-planted");
        let dirs = vec![planted.clone(), sys.clone()];
        assert_eq!(resolve("cmd.exe /k", &dirs), Some(sys.join("cmd.exe")));
        assert_eq!(resolve("wsl", &dirs), Some(sys.join("wsl.exe")));
        std::fs::write(sys.join("wsl"), b"").unwrap();
        assert_eq!(resolve("wsl", &dirs), Some(sys.join("wsl.exe")));
        assert_eq!(resolve("nothere", &dirs), None);
        assert_eq!(resolve("cmd.exe", &[planted]), None);
        assert_eq!(
            resolve("C:\\Tools\\x.exe -a", &dirs),
            Some(PathBuf::from("C:\\Tools\\x.exe"))
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn windows_paths_count_as_absolute() {
        assert!(is_absolute(Path::new("C:\\Windows\\System32")));
        assert!(is_absolute(Path::new("\\\\server\\share")));
        assert!(!is_absolute(Path::new(".")));
        assert!(!is_absolute(Path::new("bin")));
        assert!(!is_absolute(Path::new("C:relative")));
    }
}
