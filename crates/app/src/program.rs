//! Finding the program a local command starts: the local shell, a
//! ProxyCommand, a password command.
//!
//! `CreateProcessW` given only a command line looks for a bare name such
//! as `cmd.exe` in FeroSSH's own folder and the current folder before the
//! system folders, and Rust's `Command` still looks in FeroSSH's own
//! folder first. FeroSSH is one exe that is often run from Downloads,
//! where downloads land too, so a planted `cmd.exe` or `nc.exe` would then
//! run instead of the real one. So every program FeroSSH starts by a bare
//! name is resolved here, in the system folders and the absolute folders
//! on `PATH` only, and started by its full path.

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
    find(program(command), dirs)
}

/// Where program `name` is: as [`resolve`], for a name on its own.
pub fn find(name: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
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

/// Folders a bare program name is looked for in: System32 and Windows
/// first, then `PATH` (whose relative entries [`find`] skips). Never
/// FeroSSH's own folder or the current one.
pub fn search_dirs() -> Vec<PathBuf> {
    let root =
        PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into()));
    let mut dirs = vec![root.join("System32"), root];
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    dirs
}

/// [`find`] in [`search_dirs`], as an error that says where it looked.
pub fn locate(name: &str) -> std::io::Result<PathBuf> {
    find(name, &search_dirs()).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!(
                "{name} is not in the Windows folders or a folder on PATH (FeroSSH does not \
                 run programs from its own folder or the current one; give the full path)"
            ),
        )
    })
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
    fn locate_never_looks_in_the_current_folder() {
        // Planted in the current folder itself.
        let name = format!("planted-ferossh-tool-{}", std::process::id());
        let here = std::env::current_dir().unwrap();
        let file = here.join(&name);
        // Our own file only: never overwrite or later delete someone else's.
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&file)
            .unwrap();
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let _cleanup = Cleanup(file.clone());
        // It is there to be found: by its folder's absolute path...
        assert_eq!(find(&name, std::slice::from_ref(&here)), Some(file));
        // ...but not as "."...
        assert!(find(&name, &[PathBuf::from(".")]).is_none());
        // ...nor by locate, which FeroSSH uses, unless PATH itself names
        // this folder by its absolute path (then finding it is correct).
        // Compared resolved, so an alias such as `/x/../project` counts.
        let real = |d: &PathBuf| std::fs::canonicalize(d).ok();
        let here_real = real(&here);
        if !search_dirs().iter().any(|d| real(d) == here_real) {
            let e = locate(&name).unwrap_err();
            assert!(e.to_string().contains("give the full path"), "{e}");
        }
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
