//! File > Export connections, Import encrypted connections and the
//! Master password menu: the dialogs around [`crate::vault`] and
//! [`ferossh_config::sealed`]. A part of `win`, for its file helpers.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use ferossh_config::Settings;
use ferossh_config::bundle::{self, Bundle, Merged, Secret};
use ferossh_config::import::Client;
use ferossh_config::sealed::{self, Key, OpenError};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Controls::Dialogs::{
    GetSaveFileNameW, OFN_EXPLORER, OFN_NOCHANGEDIR, OFN_OVERWRITEPROMPT, OFN_PATHMUSTEXIST,
    OPENFILENAMEW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    HCURSOR, IDC_WAIT, IDYES, LoadCursorW, MB_DEFBUTTON2, MB_ICONERROR, MB_ICONINFORMATION,
    MB_ICONWARNING, MB_OK, MB_YESNO, SetCursor,
};
use windows::core::{PCWSTR, PWSTR, w};

use super::{
    DIALOG_OPEN, SideView, app_dir, app_read, dialog, home, imported_path, pick_file,
    sessions_path, settings_path, with_window, write_app_file,
};
use crate::credman;
use crate::passphrase_view::{self, Ask};
use crate::vault;

/// The file that says where the vault is, when it is not in FeroSSH's
/// own folder (a OneDrive folder, say).
fn pointer_path() -> PathBuf {
    app_dir().join("vault-location.txt")
}

fn default_vault_path() -> PathBuf {
    app_dir().join(vault::FILE_NAME)
}

/// Where the vault is (or would be).
pub(super) fn vault_path() -> PathBuf {
    std::fs::read_to_string(pointer_path())
        .ok()
        .map(|t| PathBuf::from(t.trim()))
        .filter(|p| p.is_absolute())
        .unwrap_or_else(default_vault_path)
}

/// Whether the vault is somewhere of the user's choosing.
fn vault_elsewhere() -> bool {
    vault_path() != default_vault_path()
}

/// The connection files, in the order they are bundled.
fn connection_files() -> Vec<(String, PathBuf)> {
    let mut out = vec![
        ("ferossh.conf".to_owned(), settings_path()),
        ("sessions.conf".to_owned(), sessions_path()),
    ];
    for c in Client::ALL {
        out.push((c.file_name().to_owned(), imported_path(c)));
    }
    out
}

/// What the connection files hold now.
fn connection_bundle() -> Bundle {
    let mut b = Bundle::default();
    for (name, path) in connection_files() {
        if let Ok(text) = app_read(&path) {
            b.set_file(&name, text);
        }
    }
    b
}

/// Every saved password: the vault's, then the Credential Manager's
/// that the vault has not.
fn all_secrets() -> Vec<Secret> {
    let mut out = vault::secrets();
    for s in credman::os_list() {
        if !out.iter().any(|o| o.target == s.target) {
            out.push(s);
        }
    }
    out
}

/// The number of connections in a bundle's ssh_config-syntax files.
fn host_count(b: &Bundle) -> usize {
    b.files
        .iter()
        .filter(|(n, _)| n != "ferossh.conf")
        .map(|(_, t)| bundle::count_hosts(t))
        .sum()
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// Runs the passphrase window with the dialog flag set.
fn ask(hwnd: HWND, a: &Ask) -> Option<passphrase_view::Answer> {
    if DIALOG_OPEN.get() {
        return None;
    }
    DIALOG_OPEN.set(true);
    let answer = passphrase_view::ask(Some(hwnd), a);
    DIALOG_OPEN.set(false);
    answer
}

/// The hourglass, while Argon2 runs (about half a second).
fn busy() {
    // SAFETY: a system cursor.
    unsafe {
        if let Ok(c) = LoadCursorW(None, IDC_WAIT) {
            let _: HCURSOR = SetCursor(Some(c));
        }
    }
}

/// The save dialog, starting at `start` (a folder and a file name).
fn save_file(hwnd: HWND, title: PCWSTR, start: &Path, filter: PCWSTR) -> Option<PathBuf> {
    if DIALOG_OPEN.get() {
        return None;
    }
    let mut buf = vec![0u16; 4096];
    let name = start
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    for (d, c) in buf.iter_mut().zip(name.encode_utf16()) {
        *d = c;
    }
    let dir = start.parent().map(Path::to_path_buf).unwrap_or_else(home);
    let dir: Vec<u16> = dir
        .as_os_str()
        .to_string_lossy()
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let ext: Vec<u16> = start
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_default()
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut ofn = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: hwnd,
        lpstrFilter: filter,
        lpstrFile: PWSTR(buf.as_mut_ptr()),
        nMaxFile: (buf.len() - 1) as u32,
        lpstrInitialDir: PCWSTR(dir.as_ptr()),
        lpstrDefExt: PCWSTR(ext.as_ptr()),
        lpstrTitle: title,
        Flags: OFN_EXPLORER | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR | OFN_OVERWRITEPROMPT,
        ..Default::default()
    };
    DIALOG_OPEN.set(true);
    // SAFETY: the struct is filled in as documented; the buffers outlive
    // the call; no state borrow is held during it.
    let ok = unsafe { GetSaveFileNameW(&mut ofn) }.as_bool();
    DIALOG_OPEN.set(false);
    if !ok {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(PathBuf::from(String::from_utf16_lossy(
        buf.get(..len).unwrap_or_default(),
    )))
}

const EXPORT_FILTER: PCWSTR =
    w!("FeroSSH encrypted files (*.ferossh)\0*.ferossh\0All files\0*.*\0");
const VAULT_FILTER: PCWSTR = w!("FeroSSH vaults (*.vault)\0*.vault\0All files\0*.*\0");

/// Writes `text` through a temporary file beside `path`.
fn write_new(path: &Path, text: &str) -> std::io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// The text of an encrypted file picked to open, or why not.
fn read_sealed(path: &Path) -> Result<String, String> {
    let too_big = std::fs::metadata(path)
        .map(|m| m.len() > (sealed::MAX_CONTENT as u64) * 2)
        .unwrap_or(false);
    if too_big {
        return Err(format!(
            "{} is too large to be a FeroSSH file.",
            path.display()
        ));
    }
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("Could not read {}: {e}", path.display()))?;
    sealed::header(&text).map_err(|e| format!("{}\n\n{}", path.display(), e))?;
    Ok(text)
}

/// Asks for `text`'s passphrase until it opens (or the user gives up).
fn open_sealed(hwnd: HWND, path: &Path, text: &str, title: &str) -> Option<Bundle> {
    let intro = format!(
        "Type the passphrase of {}.",
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    );
    let mut problem: Option<String> = None;
    loop {
        let answer = ask(
            hwnd,
            &Ask {
                title,
                intro: &intro,
                new: false,
                option: None,
                ok: "Open",
                cancel: "Cancel",
                problem: problem.as_deref(),
            },
        )?;
        busy();
        match sealed::open(&answer.passphrase, text) {
            Ok((content, _)) => {
                return match Bundle::parse(&content) {
                    Some(b) => Some(b),
                    None => {
                        dialog(
                            hwnd,
                            &format!("{} is damaged or from a newer FeroSSH.", path.display()),
                            MB_OK | MB_ICONERROR,
                        );
                        None
                    }
                };
            }
            Err(OpenError::WrongPassphrase) => {
                problem = Some(OpenError::WrongPassphrase.to_string());
            }
            Err(e) => {
                dialog(hwnd, &e.to_string(), MB_OK | MB_ICONERROR);
                return None;
            }
        }
    }
}

/// File > Export connections: a passphrase, the file, and the export.
pub(super) fn export(hwnd: HWND) {
    let mut b = connection_bundle();
    let hosts = host_count(&b);
    let profiles = b
        .file("ferossh.conf")
        .map_or(0, |t| Settings::parse(t).profiles.len());
    if hosts == 0 && profiles == 0 {
        dialog(
            hwnd,
            "There are no saved connections to export yet. Connections from ~/.ssh/config \
             stay in that file; copy it as it is.",
            MB_OK | MB_ICONINFORMATION,
        );
        return;
    }
    let secrets = all_secrets();
    let intro = format!(
        "FeroSSH writes your {} and their settings to one file, encrypted with this \
         passphrase. Keep it in OneDrive, a git repository or on a USB stick, and open it \
         on another PC with File > Import encrypted connections. Nobody can read it \
         without the passphrase, and FeroSSH cannot recover a forgotten one.",
        plural(hosts, "saved connection", "saved connections"),
    );
    let option = format!(
        "Include {}",
        plural(secrets.len(), "saved password", "saved passwords")
    );
    let Some(answer) = ask(
        hwnd,
        &Ask {
            title: "Export connections",
            intro: &intro,
            new: true,
            option: (!secrets.is_empty()).then_some((option.as_str(), false)),
            ok: "Export...",
            cancel: "Cancel",
            problem: None,
        },
    ) else {
        return;
    };
    let start = home().join("Documents").join("FeroSSH connections.ferossh");
    let Some(path) = save_file(hwnd, w!("Export connections to"), &start, EXPORT_FILTER) else {
        return;
    };
    let with_passwords = answer.option && !secrets.is_empty();
    if with_passwords {
        b.secrets = secrets;
    }
    busy();
    let written = Key::new(&answer.passphrase)
        .and_then(|k| k.seal(&b.to_bytes()))
        .and_then(|text| write_new(&path, &text).map_err(|e| e.to_string()));
    match written {
        Ok(()) => {
            let mut text = format!(
                "Exported {} to {}.",
                plural(hosts, "connection", "connections"),
                path.display()
            );
            if with_passwords {
                text.push_str(&format!(
                    " It holds {} too: keep the passphrase strong.",
                    plural(b.secrets.len(), "saved password", "saved passwords")
                ));
            }
            dialog(hwnd, &text, MB_OK | MB_ICONINFORMATION);
        }
        Err(e) => {
            dialog(
                hwnd,
                &format!("Could not export to {}: {e}", path.display()),
                MB_OK | MB_ICONERROR,
            );
        }
    }
}

/// What importing `incoming` would do: the merged connection files, the
/// merged settings, and the counts.
struct Plan {
    files: Vec<(PathBuf, String)>,
    hosts: Merged,
    profiles: Merged,
    settings: Option<Settings>,
}

fn plan(incoming: &Bundle, current: &Settings) -> Plan {
    let mut plan = Plan {
        files: Vec::new(),
        hosts: Merged::default(),
        profiles: Merged::default(),
        settings: None,
    };
    for (name, path) in connection_files() {
        let Some(theirs) = incoming.file(&name) else {
            continue;
        };
        if name == "ferossh.conf" {
            let mut s = current.clone();
            plan.profiles = bundle::merge_profiles(&mut s, &Settings::parse(theirs));
            plan.settings = Some(s);
            continue;
        }
        let ours = app_read(&path).unwrap_or_default();
        let (text, m) = bundle::merge_hosts(&ours, theirs);
        plan.hosts += m;
        if m.changed() {
            plan.files.push((path, text));
        }
    }
    plan
}

/// File > Import encrypted connections: the file, its passphrase, what
/// it would change, and the import.
pub(super) fn import(hwnd: HWND) {
    let start = home().join("Documents");
    let Some(path) = pick_file(hwnd, w!("Encrypted connections file"), &start) else {
        return;
    };
    let text = match read_sealed(&path) {
        Ok(t) => t,
        Err(e) => {
            dialog(hwnd, &e, MB_OK | MB_ICONWARNING);
            return;
        }
    };
    let Some(incoming) = open_sealed(hwnd, &path, &text, "Import encrypted connections") else {
        return;
    };
    let Some(current) = with_window(|w| w.settings.clone()) else {
        return;
    };
    let plan = plan(&incoming, &current);
    let n_secrets = incoming.secrets.len();
    let can_keep = vault::is_open() || !crate::portable::is_portable();
    let h = plan.hosts;
    if !h.changed() && !plan.profiles.changed() && n_secrets == 0 {
        dialog(
            hwnd,
            &format!(
                "Nothing to import: the {} in {} are already here, the same.",
                plural(h.total(), "connection", "connections"),
                path.display()
            ),
            MB_OK | MB_ICONINFORMATION,
        );
        return;
    }
    let mut question = format!(
        "{} has {}: {} new, {} replacing yours of the same name, {} the same. Your other \
         connections stay as they are.",
        path.display(),
        plural(h.total(), "connection", "connections"),
        h.added,
        h.replaced,
        h.same,
    );
    if n_secrets > 0 {
        question.push_str(&if can_keep {
            format!(
                "\n\nIt also has {}, which replace any saved for the same logins.",
                plural(n_secrets, "saved password", "saved passwords")
            )
        } else {
            format!(
                "\n\nIts {} are not imported: a portable FeroSSH keeps passwords only in a \
                 master password vault.",
                plural(n_secrets, "saved password", "saved passwords")
            )
        });
    }
    question.push_str("\n\nImport?");
    if dialog(hwnd, &question, MB_YESNO | MB_ICONINFORMATION) != Some(IDYES) {
        return;
    }
    let mut problems = Vec::new();
    for (p, t) in &plan.files {
        if let Err(e) = write_app_file(p, t) {
            problems.push(format!("Could not write {}: {e}", p.display()));
        }
    }
    if let Some(s) = plan.settings {
        with_window(|w| {
            w.settings.profiles = s.profiles;
            w.settings.favourites = s.favourites;
            w.save_settings();
        });
    }
    let mut saved = 0;
    if can_keep {
        for s in &incoming.secrets {
            match credman::write(&s.target, &s.user, &s.password) {
                Ok(()) => saved += 1,
                Err(e) => problems.push(e),
            }
        }
    }
    with_window(|w| {
        w.show_side(SideView::Sessions, true);
        w.update_menu_checks();
        w.refresh_chrome();
    });
    let mut text = format!(
        "Imported {} new and {} changed {}.",
        h.added,
        h.replaced,
        if h.added + h.replaced == 1 {
            "connection"
        } else {
            "connections"
        }
    );
    if saved > 0 {
        text.push_str(&format!(
            " {} saved.",
            plural(saved, "password", "passwords")
        ));
    }
    for p in problems.iter().take(4) {
        text.push_str("\n\n");
        text.push_str(p);
    }
    let icon = if problems.is_empty() {
        MB_ICONINFORMATION
    } else {
        MB_ICONWARNING
    };
    dialog(hwnd, &text, MB_OK | icon);
}

const FORGOTTEN: &str = "There is no way to recover a forgotten master password, so keep an \
                         export (File > Export connections) somewhere safe as well.";

/// Removes the plain connection files the vault now holds.
fn remove_plain_files() {
    for (_, path) in connection_files() {
        let _ = std::fs::remove_file(&path);
    }
}

/// Master password > Set a master password: a new vault in FeroSSH's
/// folder with everything the plain files hold.
pub(super) fn set(hwnd: HWND) {
    if vault::is_open() {
        return;
    }
    let path = default_vault_path();
    if path.exists() {
        dialog(
            hwnd,
            &format!(
                "There is already a vault at {}. Restart FeroSSH to unlock it, or remove \
                 it first.",
                path.display()
            ),
            MB_OK | MB_ICONWARNING,
        );
        return;
    }
    let portable = crate::portable::is_portable();
    let os_secrets = if portable {
        Vec::new()
    } else {
        credman::os_list()
    };
    let intro = format!(
        "FeroSSH keeps your connections, their settings and saved passwords in one \
         encrypted file, {}, and asks for this password each time it starts. {FORGOTTEN}",
        path.display()
    );
    let option = format!(
        "Move {} from Windows into the vault",
        plural(os_secrets.len(), "saved password", "saved passwords")
    );
    let Some(answer) = ask(
        hwnd,
        &Ask {
            title: "Set a master password",
            intro: &intro,
            new: true,
            option: (!os_secrets.is_empty()).then_some((option.as_str(), true)),
            ok: "Set",
            cancel: "Cancel",
            problem: None,
        },
    ) else {
        return;
    };
    let mut b = connection_bundle();
    let moved = answer.option && !os_secrets.is_empty();
    if moved {
        b.secrets = os_secrets;
    }
    busy();
    if let Err(e) = vault::create(&path, &answer.passphrase, b.clone()) {
        dialog(
            hwnd,
            &format!("The vault could not be made: {e}"),
            MB_OK | MB_ICONERROR,
        );
        return;
    }
    remove_plain_files();
    if moved {
        for s in &b.secrets {
            credman::os_delete(&s.target);
        }
    }
    after_change();
    let mut text = format!(
        "Your connections are now in the vault {}. FeroSSH asks for the master password \
         when it starts.",
        path.display()
    );
    if moved {
        text.push_str(&format!(
            " {} moved out of the Windows Credential Manager into it.",
            plural(b.secrets.len(), "saved password", "saved passwords")
        ));
    }
    text.push_str("\n\nTo share it between PCs, use Master password > Move the vault file to put it in a OneDrive folder.");
    dialog(hwnd, &text, MB_OK | MB_ICONINFORMATION);
}

/// Asks for the current master password before a change to the vault;
/// whether it was right.
fn confirm_master(hwnd: HWND, title: &str) -> bool {
    let mut problem: Option<String> = None;
    loop {
        let Some(answer) = ask(
            hwnd,
            &Ask {
                title,
                intro: "Type the current master password first.",
                new: false,
                option: None,
                ok: "Next",
                cancel: "Cancel",
                problem: problem.as_deref(),
            },
        ) else {
            return false;
        };
        busy();
        if vault::verify(&answer.passphrase) {
            return true;
        }
        problem = Some("Wrong master password.".into());
    }
}

/// Master password > Change master password.
pub(super) fn change(hwnd: HWND) {
    if !vault::is_open() || !confirm_master(hwnd, "Change master password") {
        return;
    }
    let intro = format!("The new master password for the vault. {FORGOTTEN}");
    let Some(answer) = ask(
        hwnd,
        &Ask {
            title: "Change master password",
            intro: &intro,
            new: true,
            option: None,
            ok: "Change",
            cancel: "Cancel",
            problem: None,
        },
    ) else {
        return;
    };
    busy();
    match vault::change_passphrase(&answer.passphrase) {
        Ok(()) => {
            let mut text = "The master password is changed.".to_owned();
            if vault_elsewhere() {
                text.push_str(
                    " Other PCs that use this vault ask for the new one when FeroSSH next starts.",
                );
            }
            dialog(hwnd, &text, MB_OK | MB_ICONINFORMATION);
        }
        Err(e) => {
            dialog(hwnd, &e, MB_OK | MB_ICONERROR);
        }
    }
}

/// Remembers where the vault is; none in FeroSSH's own folder.
fn set_pointer(path: &Path) -> std::io::Result<()> {
    if path == default_vault_path() {
        match std::fs::remove_file(pointer_path()) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    } else {
        write_new(&pointer_path(), &format!("{}\n", path.display()))
    }
}

/// Master password > Move the vault file: to a OneDrive folder, say, so
/// other PCs can use it too.
pub(super) fn move_file(hwnd: HWND) {
    let Some(now) = vault::path() else {
        return;
    };
    let Some(to) = save_file(hwnd, w!("Move the vault to"), &now, VAULT_FILTER) else {
        return;
    };
    if to == now {
        return;
    }
    if to.exists() {
        dialog(
            hwnd,
            &format!(
                "There is already a file at {}. To use a vault another PC made, remove the \
                 master password here first, then pick Master password > Use a vault file.",
                to.display()
            ),
            MB_OK | MB_ICONWARNING,
        );
        return;
    }
    let done = vault::move_to(&to).and_then(|()| {
        set_pointer(&to).map_err(|e| {
            // Without the pointer the next start would not find the vault:
            // put it back where FeroSSH looks.
            let why = format!("where it is could not be remembered ({e})");
            match vault::move_to(&now) {
                Ok(()) => why,
                Err(back) => format!(
                    "{why}, and it could not be moved back ({back}). It is now {}: \
                     after a restart, pick Master password > Use a vault file and choose it.",
                    to.display()
                ),
            }
        })
    });
    match done {
        Ok(()) => {
            dialog(
                hwnd,
                &format!(
                    "The vault is now {}. On another PC, pick Master password > Use a vault \
                     file and choose it there to share these connections.",
                    to.display()
                ),
                MB_OK | MB_ICONINFORMATION,
            );
        }
        Err(e) => {
            dialog(
                hwnd,
                &format!("The vault was not moved: {e}"),
                MB_OK | MB_ICONERROR,
            );
        }
    }
}

/// Master password > Use a vault file: unlocks a vault another PC made
/// (in a shared folder) and adds this PC's connections to it.
pub(super) fn use_file(hwnd: HWND) {
    if vault::is_open() {
        return;
    }
    let start = home().join("OneDrive");
    let start = if start.is_dir() { start } else { home() };
    let Some(path) = pick_file(hwnd, w!("A FeroSSH vault"), &start) else {
        return;
    };
    if let Err(e) = read_sealed(&path) {
        dialog(hwnd, &e, MB_OK | MB_ICONWARNING);
        return;
    }
    let portable = crate::portable::is_portable();
    let os_secrets = if portable {
        Vec::new()
    } else {
        credman::os_list()
    };
    let intro = format!(
        "Type the master password of {}. This PC's connections are added to it, and \
         FeroSSH uses it from now on.",
        path.display()
    );
    let option = format!(
        "Move {} from Windows into the vault",
        plural(os_secrets.len(), "saved password", "saved passwords")
    );
    // This PC's files, read before the vault takes over the reads.
    let mine = connection_bundle();
    let mut problem: Option<String> = None;
    let answer = loop {
        let Some(answer) = ask(
            hwnd,
            &Ask {
                title: "Use a vault file",
                intro: &intro,
                new: false,
                option: (!os_secrets.is_empty()).then_some((option.as_str(), true)),
                ok: "Use",
                cancel: "Cancel",
                problem: problem.as_deref(),
            },
        ) else {
            return;
        };
        busy();
        match vault::unlock(&path, &answer.passphrase) {
            Ok(()) => break answer,
            Err(vault::UnlockError::WrongPassphrase) => {
                problem = Some(vault::UnlockError::WrongPassphrase.to_string());
            }
            Err(e) => {
                dialog(hwnd, &e.to_string(), MB_OK | MB_ICONERROR);
                return;
            }
        }
    };
    // The vault's connections win over this PC's of the same name.
    let mut problems = Vec::new();
    for (name, p) in connection_files() {
        if name == "ferossh.conf" {
            continue;
        }
        let Some(ours) = mine.file(&name) else {
            continue;
        };
        let shared = vault::read(&name).unwrap_or_default();
        let (merged, _) = bundle::merge_hosts(ours, &shared);
        if let Err(e) = write_app_file(&p, &merged) {
            problems.push(e.to_string());
        }
    }
    // This PC's window settings; the vault's host profiles added.
    let shared = vault::read("ferossh.conf").map(|t| Settings::parse(&t));
    if let Some(t) = vault::read("ferossh.conf") {
        note_settings(&t);
    }
    let saved = with_window(|w| {
        if let Some(s) = &shared {
            // A profile in both takes the vault's.
            let mut merged = w.settings.clone();
            bundle::merge_profiles(&mut merged, s);
            w.settings.profiles = merged.profiles;
            w.settings.favourites = merged.favourites;
        }
        w.try_save_settings()
    })
    .unwrap_or_else(|| Err("Could not save the settings.".to_owned()));
    if let Err(e) = saved {
        problems.push(e);
    }
    if answer.option {
        for s in &os_secrets {
            if vault::secret(&s.target).is_none() {
                match vault::set_secret(&s.target, &s.user, &s.password) {
                    Ok(()) => credman::os_delete(&s.target),
                    Err(e) => problems.push(e),
                }
            }
        }
    }
    if let Err(e) = set_pointer(&path) {
        problems.push(format!("Could not remember where the vault is: {e}"));
    }
    // The plain files go only once everything is safely in the vault and
    // FeroSSH will find it again; otherwise they are what it starts with.
    if problems.is_empty() {
        remove_plain_files();
    } else {
        problems.push(format!(
            "The plain files in {} are kept until this works.",
            app_dir().display()
        ));
    }
    after_change();
    let mut text = format!(
        "FeroSSH now uses the vault {}, with this PC's connections added. It asks for its \
         master password when it starts.",
        path.display()
    );
    for p in problems.iter().take(4) {
        text.push_str("\n\n");
        text.push_str(p);
    }
    dialog(hwnd, &text, MB_OK | MB_ICONINFORMATION);
}

/// Master password > Remove master password: the files go back to plain
/// files, the passwords back to the Credential Manager.
pub(super) fn remove(hwnd: HWND) {
    if !vault::is_open() || !confirm_master(hwnd, "Remove master password") {
        return;
    }
    let portable = crate::portable::is_portable();
    let elsewhere = vault_elsewhere();
    let mut question = format!(
        "FeroSSH will keep your connections and settings as plain files in {} again, \
         readable by anyone who can open that folder.",
        app_dir().display()
    );
    question.push_str(if portable {
        " Saved passwords are dropped: a portable FeroSSH keeps them only in a vault."
    } else {
        " Saved passwords go back to the Windows Credential Manager."
    });
    if elsewhere {
        question.push_str(&format!(
            "\n\nThe vault file {} stays where it is, for other PCs that use it.",
            vault::path().unwrap_or_default().display()
        ));
    }
    question.push_str("\n\nRemove the master password?");
    if dialog(hwnd, &question, MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2) != Some(IDYES) {
        return;
    }
    let (b, file) = match vault::remove() {
        Ok(b) => b,
        Err(e) => {
            dialog(hwnd, &e, MB_OK | MB_ICONERROR);
            return;
        }
    };
    let mut problems = Vec::new();
    for (name, path) in connection_files() {
        if let Some(text) = b.file(&name)
            && let Err(e) = write_app_file(&path, text)
        {
            problems.push(format!("Could not write {}: {e}", path.display()));
        }
    }
    if !portable {
        for s in &b.secrets {
            if let Err(e) = credman::os_write(&s.target, &s.user, &s.password) {
                problems.push(e);
            }
        }
    }
    // The window's settings are written out as they are now.
    let saved = with_window(|w| w.try_save_settings())
        .unwrap_or_else(|| Err("Could not save the settings.".to_owned()));
    if let Err(e) = saved {
        problems.push(e);
    }
    after_change();
    if problems.is_empty()
        && let Err(e) = set_pointer(&default_vault_path())
    {
        problems.push(format!("Could not forget where the vault is: {e}"));
    }
    let text = if problems.is_empty() {
        // Only FeroSSH's own vault file is deleted; one the user chose
        // (in OneDrive, say) stays for the other PCs that use it.
        if file == default_vault_path()
            && let Err(e) = std::fs::remove_file(&file)
        {
            format!(
                "The master password could not be fully removed: the old vault file {} could \
                 not be deleted ({e}). Delete it to finish; until then FeroSSH asks for the \
                 master password at each start.",
                file.display()
            )
        } else {
            "The master password is removed.".to_owned()
        }
    } else {
        // Nothing is lost: the vault stays, and FeroSSH opens it again
        // at the next start.
        format!(
            "Not everything could be written out, so the vault {} is kept and FeroSSH asks \
             for its master password again when it next starts:\n\n{}",
            file.display(),
            problems.join("\n")
        )
    };
    dialog(hwnd, &text, MB_OK | MB_ICONINFORMATION);
}

fn after_change() {
    with_window(|w| {
        w.update_menu_checks();
        w.refresh_chrome();
    });
}

/// When FeroSSH starts with a vault: asks for its master password until
/// it opens. `false` when the user quits instead.
pub(super) fn unlock_at_start() -> bool {
    let path = vault_path();
    if !path.exists() {
        if !vault_elsewhere() {
            return true;
        }
        let text = format!(
            "Your connections are in the vault {}, which is not there now (is the drive or \
             the OneDrive folder available?).\n\nStart without them? FeroSSH then uses this \
             PC's own files until it finds the vault again.",
            path.display()
        );
        return super::message(&text, MB_YESNO | MB_ICONWARNING) == Some(IDYES);
    }
    let intro = format!(
        "Your connections are kept in a vault. Type the master password to unlock {}.",
        path.display()
    );
    let mut problem: Option<String> = None;
    loop {
        let Some(answer) = passphrase_view::ask(
            None,
            &Ask {
                title: "FeroSSH",
                intro: &intro,
                new: false,
                option: None,
                ok: "Unlock",
                cancel: "Quit",
                problem: problem.as_deref(),
            },
        ) else {
            return false;
        };
        busy();
        match vault::unlock(&path, &answer.passphrase) {
            Ok(()) => return true,
            Err(e) => problem = Some(e.to_string()),
        }
    }
}

/// The text of `ferossh.conf` as this window last read or wrote it: the
/// base for merging another PC's changes to the shared vault.
static SETTINGS_BASE: Mutex<Option<String>> = Mutex::new(None);

pub(super) fn note_settings(text: &str) {
    if let Ok(mut b) = SETTINGS_BASE.lock() {
        *b = Some(text.to_owned());
    }
}

/// Before the window saves its settings into a shared vault: host
/// profiles another PC changed since we read them are taken in, so our
/// save does not undo them.
pub(super) fn sync_settings(settings: &mut Settings) {
    if !vault::is_open() {
        return;
    }
    let Some(theirs) = vault::read("ferossh.conf") else {
        return;
    };
    let base = SETTINGS_BASE.lock().ok().and_then(|b| b.clone());
    if base.as_deref() == Some(theirs.as_str()) {
        return;
    }
    let base = base.map(|b| Settings::parse(&b)).unwrap_or_default();
    let theirs = Settings::parse(&theirs);
    settings.profiles =
        bundle::merge_profiles3(&base.profiles, &settings.profiles, &theirs.profiles);
}
