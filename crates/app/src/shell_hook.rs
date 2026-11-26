//! Shell integration for the PowerShell panes FeroSSH starts on this PC:
//! the OSC 133 prompt and command marks (see the terminal's `commands`)
//! put in by FeroSSH itself, so nothing has to go into the user's profile.
//!
//! PowerShell is started with `-NoExit -EncodedCommand`, which runs after
//! the profile: the hook wraps whatever prompt the profile set (oh-my-posh,
//! starship, a plain one) and PSReadLine's `PSConsoleHostReadLine`. A
//! profile that already has FeroSSH's hook (the one in the user guide) or
//! that defines `$global:__fsPrompt` is left alone, so marks never double.
//!
//! cmd has no hook for the end of a command, so it gets none here.

/// The hook: the user guide's PowerShell snippet, guarded so that it goes
/// in once, and working without PSReadLine too (then a command's end is
/// read from the history, and its output start is not marked).
pub const HOOK: &str = r#"if (-not (Test-Path variable:global:__fsPrompt)) {
$global:__fsE = [char]27; $global:__fsA = [char]7; $global:__fsRan = $false
$global:__fsPrompt = $function:prompt
$global:__fsLast = (Get-History -Count 1).Id
function global:prompt {
    $ok = $?; $code = $global:LASTEXITCODE; $out = ''
    $id = (Get-History -Count 1).Id
    if ($global:__fsRan -or ($id -and $id -ne $global:__fsLast)) {
        $c = if ($ok) { 0 } elseif ($code) { $code } else { 1 }
        $out += "$__fsE]133;D;$c$__fsA"; $global:__fsRan = $false
    }
    $global:__fsLast = $id
    $p = if ($global:__fsPrompt) { & $global:__fsPrompt } else { "PS $($executionContext.SessionState.Path.CurrentLocation)> " }
    $out + "$__fsE]133;A$__fsA" + $p + "$__fsE]133;B$__fsA"
}
if (Test-Path function:PSConsoleHostReadLine) {
    $global:__fsReadLine = $function:PSConsoleHostReadLine
    function global:PSConsoleHostReadLine {
        $line = & $global:__fsReadLine
        $global:__fsRan = $true
        [Console]::Write("$__fsE]133;C$__fsA")
        $line
    }
}
}"#;

/// Options PowerShell may already have for the hook to still go in: they
/// only change how it starts, not what it runs.
const HARMLESS: [&str; 6] = ["-nologo", "-nol", "-noprofile", "-nop", "-mta", "-sta"];

/// Whether `command` starts PowerShell (Windows PowerShell or PowerShell 7)
/// interactively, with nothing of its own to run: only then can the hook
/// be added.
pub fn is_plain_powershell(command: &str) -> bool {
    let program = crate::program::program(command);
    let name = program
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(program)
        .to_ascii_lowercase();
    let name = name.strip_suffix(".exe").unwrap_or(&name);
    if name != "powershell" && name != "pwsh" {
        return false;
    }
    let rest = rest_of(command);
    rest.split_whitespace()
        .all(|a| HARMLESS.contains(&a.to_ascii_lowercase().as_str()))
}

/// `command` after its program.
fn rest_of(command: &str) -> &str {
    let command = command.trim_start();
    if let Some(rest) = command.strip_prefix('"') {
        return rest.split_once('"').map_or("", |(_, r)| r);
    }
    command.split_once([' ', '\t']).map_or("", |(_, r)| r)
}

/// `command` with the hook added, when it starts PowerShell with nothing
/// of its own to run; else as it is.
pub fn with_hook(command: &str) -> String {
    if !is_plain_powershell(command) {
        return command.to_owned();
    }
    format!(
        "{} -NoExit -EncodedCommand {}",
        command.trim_end(),
        encoded(HOOK)
    )
}

/// The command line of a local coding agent's tab: PowerShell `shell`
/// runs `agent` (a command line in PowerShell's syntax, from the agent
/// list), then stays at its prompt. With `hook`, the shell integration
/// goes in first and the agent's run is marked as a command, so its end
/// and exit code reach the dashboard.
pub fn agent_command(shell: &str, agent: &str, hook: bool) -> String {
    let agent = agent.trim();
    let script = if hook && is_plain_powershell(shell) {
        format!(
            "{HOOK}\nif (Test-Path variable:global:__fsE) {{ [Console]::Write(\"$__fsE]133;C$__fsA\"); $global:__fsRan = $true }}\n{agent}"
        )
    } else {
        agent.to_owned()
    };
    format!(
        "{} -NoExit -EncodedCommand {}",
        shell.trim_end(),
        encoded(&script)
    )
}

/// `script` as `-EncodedCommand` takes it: Base64 of its UTF-16LE.
pub fn encoded(script: &str) -> String {
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    base64(&bytes)
}

fn base64(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk.first().copied().unwrap_or(0),
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                let six = (n >> (18 - 6 * i)) & 63;
                out.push(char::from(ABC.get(six as usize).copied().unwrap_or(b'A')));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The PowerShell that File > Open local PowerShell starts: PowerShell 7
/// (`pwsh.exe`) when it is installed, else Windows PowerShell.
pub fn powershell_command(pwsh_installed: bool) -> String {
    if pwsh_installed {
        "pwsh.exe -NoLogo".to_owned()
    } else {
        "powershell.exe -NoLogo".to_owned()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_powershell_gets_the_hook() {
        assert!(is_plain_powershell("powershell.exe"));
        assert!(is_plain_powershell("pwsh -NoLogo"));
        assert!(is_plain_powershell(
            "\"C:\\Program Files\\PowerShell\\7\\pwsh.exe\" -nologo -NoProfile"
        ));
        assert!(is_plain_powershell(
            "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\PowerShell.EXE"
        ));
        assert!(!is_plain_powershell("cmd.exe"));
        assert!(!is_plain_powershell("powershell -Command Get-Date"));
        assert!(!is_plain_powershell("pwsh -File x.ps1"));
        assert!(!is_plain_powershell("pwsh -EncodedCommand AAAA"));
        assert!(!is_plain_powershell("wsl -d Ubuntu"));
        assert!(!is_plain_powershell("mypwsh.exe"));
    }

    #[test]
    fn hook_goes_after_the_options() {
        let c = with_hook("pwsh.exe -NoLogo");
        assert!(c.starts_with("pwsh.exe -NoLogo -NoExit -EncodedCommand "));
        assert_eq!(with_hook("cmd.exe"), "cmd.exe");
        // Well within Windows' 32,767-character command line.
        assert!(c.len() < 8000, "{}", c.len());
    }

    #[test]
    fn base64_matches_known_values() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        // What PowerShell's own docs give for `dir "c:\program files" `.
        assert_eq!(
            encoded("dir \"c:\\program files\" "),
            "ZABpAHIAIAAiAGMAOgBcAHAAcgBvAGcAcgBhAG0AIABmAGkAbABlAHMAIgAgAA=="
        );
    }

    #[test]
    fn hook_sends_all_four_marks_and_guards_itself() {
        for mark in ["]133;A", "]133;B", "]133;C", "]133;D;"] {
            assert!(HOOK.contains(mark), "{mark}");
        }
        assert!(HOOK.starts_with("if (-not (Test-Path variable:global:__fsPrompt))"));
        // Balanced braces, or PowerShell refuses the whole script.
        let open = HOOK.matches('{').count();
        let close = HOOK.matches('}').count();
        assert_eq!(open, close);
    }

    fn decode(b64: &str) -> String {
        const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut bits = 0u32;
        let mut n = 0;
        let mut bytes = Vec::new();
        for c in b64.bytes().filter(|&c| c != b'=') {
            let v = ABC.iter().position(|&a| a == c).unwrap() as u32;
            bits = bits << 6 | v;
            n += 6;
            if n >= 8 {
                n -= 8;
                bytes.push((bits >> n) as u8);
            }
        }
        let units: Vec<u16> = bytes
            .chunks(2)
            .map(|c| u16::from_le_bytes([c[0], *c.get(1).unwrap_or(&0)]))
            .collect();
        String::from_utf16(&units).unwrap()
    }

    #[test]
    fn an_agent_runs_after_the_hook_as_a_marked_command() {
        let c = agent_command("pwsh.exe -NoLogo", " claude --model opus ", true);
        let (head, b64) = c.rsplit_once(' ').unwrap();
        assert_eq!(head, "pwsh.exe -NoLogo -NoExit -EncodedCommand");
        let script = decode(b64);
        assert!(script.starts_with(HOOK));
        assert!(
            script.ends_with("$global:__fsRan = $true }\nclaude --model opus"),
            "{script}"
        );
        let plain = agent_command("powershell.exe -NoLogo", "codex", false);
        assert_eq!(decode(plain.rsplit_once(' ').unwrap().1), "codex");
    }

    #[test]
    fn powershell_seven_first() {
        assert_eq!(powershell_command(true), "pwsh.exe -NoLogo");
        assert_eq!(powershell_command(false), "powershell.exe -NoLogo");
    }
}
