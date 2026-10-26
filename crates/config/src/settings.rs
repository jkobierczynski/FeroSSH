//! FeroSSH's own settings file, `%APPDATA%\FeroSSH\ferossh.conf`: session
//! profiles (a host's font, text size and colour theme), favourite hosts,
//! colour theme rules and a few window options.
//!
//! It uses ssh_config's syntax, so it reads like the file it sits beside:
//!
//! ```text
//! SidePane right
//! DownloadFolder "D:\Transfers"
//! VerifyTransfers yes
//! Favourite web
//! ThemeRule
//!     Hosts prod-* *.prod.example.com
//!     Tags production
//!     Theme Production
//!     Tint 800000 60
//! Profile web
//!     Font "Cascadia Mono"
//!     FontSize 12
//!     Theme Dracula
//!     Tmux main
//!     TmuxLayout ferossh
//! Profile core-switch
//!     LegacyAlgorithms yes
//! Profile desktop-box
//!     DesktopPort 3390
//!     VncPort 5901
//! Profile office-pc
//!     Type rdp
//!     Via bastion
//! Profile core-console
//!     Type serial
//!     Serial 115200,8N1,none
//! ```
//!
//! FeroSSH rewrites the file when a setting changes. Unknown keywords are
//! skipped, so an older FeroSSH reads a newer file.

use std::fmt::Write as _;

use crate::host_theme::ThemeRule;
use crate::password::PasswordSource;
use crate::serial::LineSettings;
use crate::ssh_config::split_line;

/// How one host's panes look. Unset fields use the theme rules and
/// defaults.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Profile {
    /// The `Host` name from ssh_config, or what was typed.
    pub host: String,
    pub font: Option<String>,
    pub font_size: Option<f32>,
    pub theme: Option<String>,
    /// Lines of scrollback for this host's panes.
    pub scrollback: Option<usize>,
    /// Attach to this tmux session after logging in (off when unset).
    pub tmux: Option<String>,
    /// The tmux layout for new sessions: `ferossh` (one window per pane)
    /// or `tmux` (tmux's own splits).
    pub tmux_layout: Option<String>,
    /// Also offer the legacy algorithm set (ECDH NIST curves, DH groups
    /// 14 to 18 with SHA-2, AES-CTR, plain HMAC-SHA-2) for older switches,
    /// routers and servers.
    pub legacy_algorithms: bool,
    /// The server's RDP port for Session > Open remote desktop; 3389 when
    /// unset.
    pub desktop_port: Option<u16>,
    /// The server's VNC port for the built-in desktop viewer; 5900 when
    /// unset.
    pub vnc_port: Option<u16>,
    /// What the session opens: a shell (the default) or a desktop.
    pub kind: SessionKind,
    /// For a desktop session: the SSH host whose connection carries it.
    /// Unset, an RDP session connects straight to the machine.
    pub via: Option<String>,
    /// Where the login password comes from.
    pub password: PasswordSource,
    /// For a serial session: speed, framing and flow control. The COM
    /// port is the session's `HostName`.
    pub serial: Option<LineSettings>,
}

/// What opening a session does.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SessionKind {
    /// An SSH login with a shell.
    #[default]
    Ssh,
    /// A Remote Desktop (RDP) connection in Remote Desktop Connection.
    Rdp,
    /// A VNC desktop in the built-in viewer, always through SSH.
    Vnc,
    /// A serial console on a COM port of this PC.
    Serial,
}

impl SessionKind {
    pub fn name(self) -> &'static str {
        match self {
            SessionKind::Ssh => "ssh",
            SessionKind::Rdp => "rdp",
            SessionKind::Vnc => "vnc",
            SessionKind::Serial => "serial",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text.to_ascii_lowercase().as_str() {
            "ssh" => Some(SessionKind::Ssh),
            "rdp" => Some(SessionKind::Rdp),
            "vnc" => Some(SessionKind::Vnc),
            "serial" => Some(SessionKind::Serial),
            _ => None,
        }
    }
}

/// The RDP port xrdp and GNOME Remote Desktop listen on by default.
pub const DEFAULT_DESKTOP_PORT: u16 = 3389;
/// The VNC port of display :0.
pub const DEFAULT_VNC_PORT: u16 = 5900;

impl Profile {
    fn is_empty(&self) -> bool {
        self.font.is_none()
            && self.font_size.is_none()
            && self.theme.is_none()
            && self.scrollback.is_none()
            && self.tmux.is_none()
            && self.tmux_layout.is_none()
            && !self.legacy_algorithms
            && self.desktop_port.is_none()
            && self.vnc_port.is_none()
            && self.kind == SessionKind::Ssh
            && self.via.is_none()
            && self.password == PasswordSource::Ask
            && self.serial.is_none()
    }
}

/// Everything in the settings file.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub profiles: Vec<Profile>,
    pub favourites: Vec<String>,
    /// User theme rules, tried before the built-in tag rules.
    pub theme_rules: Vec<ThemeRule>,
    /// Dock the side pane on the right instead of the left.
    pub side_pane_right: bool,
    /// Show the tab strip when there are two or more tabs.
    pub tab_strip: bool,
    pub copy_on_select: bool,
    pub right_click_paste: bool,
    /// Lines of scrollback for panes without a profile that sets it.
    pub scrollback: Option<usize>,
    /// Where downloads go; the user's Downloads folder when unset.
    pub download_folder: Option<String>,
    /// Compare SHA-256 on both sides after each transfer.
    pub verify_transfers: bool,
    /// Files transferred at once.
    pub parallel_transfers: Option<usize>,
    /// The file view follows the shell's working directory (OSC 7).
    pub follow_directory: bool,
    /// The X server forwarded X11 goes to (`DISPLAY` syntax); the `DISPLAY`
    /// environment variable, or `localhost:0`, when unset.
    pub x11_display: Option<String>,
}

/// Scrollback is capped here, as the terminal caps it.
const MAX_SCROLLBACK: usize = 1_000_000;

fn lines(args: &[String]) -> Option<usize> {
    args.first()?
        .parse::<usize>()
        .ok()
        .map(|n| n.min(MAX_SCROLLBACK))
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            profiles: Vec::new(),
            favourites: Vec::new(),
            theme_rules: Vec::new(),
            side_pane_right: false,
            tab_strip: true,
            copy_on_select: false,
            right_click_paste: false,
            scrollback: None,
            download_folder: None,
            verify_transfers: false,
            parallel_transfers: None,
            follow_directory: true,
            x11_display: None,
        }
    }
}

/// Which block the parser is in.
enum Block {
    Top,
    Profile(usize),
    Rule(usize),
}

fn yes(args: &[String]) -> Option<bool> {
    match args.first()?.to_ascii_lowercase().as_str() {
        "yes" | "on" | "true" => Some(true),
        "no" | "off" | "false" => Some(false),
        _ => None,
    }
}

/// What follows a line's keyword, as written (for a command line, whose
/// quotes mean something later).
fn raw_rest(line: &str) -> String {
    let line = line.trim();
    let end = line
        .find(|c: char| c.is_ascii_whitespace() || c == '=')
        .unwrap_or(line.len());
    let rest = line.get(end..).unwrap_or_default().trim_start();
    let rest = rest.strip_prefix('=').unwrap_or(rest);
    rest.trim().to_owned()
}

fn hex_rgb(text: &str) -> Option<[u8; 3]> {
    let text = text.strip_prefix('#').unwrap_or(text);
    if text.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(text, 16).ok()?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

/// Quotes an argument that holds spaces; drops characters that would
/// break the line.
fn arg(text: &str) -> String {
    let clean: String = text
        .chars()
        .filter(|c| !c.is_control() && *c != '"')
        .collect();
    if clean.is_empty() || clean.contains([' ', '\t', '=', '#']) {
        format!("\"{clean}\"")
    } else {
        clean
    }
}

impl Settings {
    pub fn parse(text: &str) -> Self {
        let mut s = Settings::default();
        let mut block = Block::Top;
        for line in text.lines() {
            let Some((keyword, args)) = split_line(line) else {
                continue;
            };
            let first = args.first().cloned();
            // Font and theme names may hold spaces, quoted or not.
            let joined = args.join(" ");
            match keyword.as_str() {
                "profile" => {
                    let Some(host) = first else {
                        continue;
                    };
                    s.profiles.retain(|p| !p.host.eq_ignore_ascii_case(&host));
                    s.profiles.push(Profile {
                        host,
                        ..Profile::default()
                    });
                    block = Block::Profile(s.profiles.len() - 1);
                }
                "themerule" => {
                    s.theme_rules.push(ThemeRule::new(""));
                    block = Block::Rule(s.theme_rules.len() - 1);
                }
                "favourite" | "favorite" => {
                    block = Block::Top;
                    for host in args {
                        if !s.favourites.iter().any(|f| f.eq_ignore_ascii_case(&host)) {
                            s.favourites.push(host);
                        }
                    }
                }
                "sidepane" => {
                    block = Block::Top;
                    s.side_pane_right = first.is_some_and(|a| a.eq_ignore_ascii_case("right"));
                }
                "tabstrip" => {
                    block = Block::Top;
                    s.tab_strip = yes(&args).unwrap_or(true);
                }
                "copyonselect" => {
                    block = Block::Top;
                    s.copy_on_select = yes(&args).unwrap_or(false);
                }
                "rightclickpaste" => {
                    block = Block::Top;
                    s.right_click_paste = yes(&args).unwrap_or(false);
                }
                "downloadfolder" => {
                    block = Block::Top;
                    s.download_folder = Some(joined).filter(|f| !f.is_empty());
                }
                "verifytransfers" => {
                    block = Block::Top;
                    s.verify_transfers = yes(&args).unwrap_or(false);
                }
                "paralleltransfers" => {
                    block = Block::Top;
                    s.parallel_transfers = first
                        .and_then(|v| v.parse::<usize>().ok())
                        .map(|n| n.clamp(1, 8));
                }
                "followdirectory" => {
                    block = Block::Top;
                    s.follow_directory = yes(&args).unwrap_or(true);
                }
                "x11display" => {
                    block = Block::Top;
                    s.x11_display = first.filter(|d| !d.is_empty());
                }
                "scrollback" if matches!(block, Block::Profile(_)) => {
                    if let Block::Profile(i) = block
                        && let Some(p) = s.profiles.get_mut(i)
                    {
                        p.scrollback = lines(&args);
                    }
                }
                "scrollback" => {
                    block = Block::Top;
                    s.scrollback = lines(&args);
                }
                _ => match block {
                    Block::Profile(i) => {
                        let Some(p) = s.profiles.get_mut(i) else {
                            continue;
                        };
                        match keyword.as_str() {
                            "font" => p.font = Some(joined).filter(|f| !f.is_empty()),
                            "fontsize" => {
                                p.font_size = first
                                    .and_then(|v| v.parse::<f32>().ok())
                                    .filter(|v| (4.0..=96.0).contains(v));
                            }
                            "theme" => p.theme = Some(joined).filter(|t| !t.is_empty()),
                            "tmux" => {
                                p.tmux = first.filter(|t| !t.eq_ignore_ascii_case("no"));
                            }
                            "legacyalgorithms" => {
                                p.legacy_algorithms =
                                    first.is_some_and(|v| v.eq_ignore_ascii_case("yes"));
                            }
                            "desktopport" => {
                                p.desktop_port = first
                                    .and_then(|v| v.parse::<u16>().ok())
                                    .filter(|&v| v != 0);
                            }
                            "vncport" => {
                                p.vnc_port = first
                                    .and_then(|v| v.parse::<u16>().ok())
                                    .filter(|&v| v != 0);
                            }
                            "type" => {
                                p.kind = first
                                    .as_deref()
                                    .and_then(SessionKind::parse)
                                    .unwrap_or_default();
                            }
                            "passwordsource" => {
                                p.password = match first.as_deref() {
                                    Some(v) if v.eq_ignore_ascii_case("windows") => {
                                        PasswordSource::Windows
                                    }
                                    _ => PasswordSource::Ask,
                                };
                            }
                            // The rest of the line as written: quotes group
                            // the command's arguments.
                            "passwordcommand" => {
                                let raw = raw_rest(line);
                                p.password = if raw.is_empty() {
                                    PasswordSource::Ask
                                } else {
                                    PasswordSource::Command(raw)
                                };
                            }
                            "via" => {
                                p.via = first.filter(|v| crate::saved::valid_alias(v));
                            }
                            "serial" => p.serial = LineSettings::parse(&joined),
                            "tmuxlayout" => {
                                p.tmux_layout = first
                                    .map(|l| l.to_ascii_lowercase())
                                    .filter(|l| l == "ferossh" || l == "tmux");
                            }
                            _ => {}
                        }
                    }
                    Block::Rule(i) => {
                        let Some(r) = s.theme_rules.get_mut(i) else {
                            continue;
                        };
                        match keyword.as_str() {
                            "hosts" => r.hosts.extend(args),
                            "tags" => r.tags.extend(args),
                            "theme" => r.theme = joined,
                            "tint" => {
                                let rgb = first.as_deref().and_then(hex_rgb);
                                let amount = args.get(1).and_then(|a| a.parse::<u8>().ok());
                                r.tint = rgb.zip(amount);
                            }
                            _ => {}
                        }
                    }
                    Block::Top => {}
                },
            }
        }
        s.theme_rules
            .retain(|r| !r.theme.is_empty() && !(r.hosts.is_empty() && r.tags.is_empty()));
        s
    }

    /// The file's text.
    pub fn to_text(&self) -> String {
        let mut out = String::from(
            "# FeroSSH settings. FeroSSH rewrites this file; comments are not kept.\n",
        );
        let flag = |b: bool| if b { "yes" } else { "no" };
        let _ = writeln!(
            out,
            "SidePane {}",
            if self.side_pane_right {
                "right"
            } else {
                "left"
            }
        );
        let _ = writeln!(out, "TabStrip {}", flag(self.tab_strip));
        let _ = writeln!(out, "CopyOnSelect {}", flag(self.copy_on_select));
        let _ = writeln!(out, "RightClickPaste {}", flag(self.right_click_paste));
        if let Some(n) = self.scrollback {
            let _ = writeln!(out, "Scrollback {n}");
        }
        if let Some(d) = &self.download_folder {
            let _ = writeln!(out, "DownloadFolder {}", arg(d));
        }
        let _ = writeln!(out, "VerifyTransfers {}", flag(self.verify_transfers));
        if let Some(n) = self.parallel_transfers {
            let _ = writeln!(out, "ParallelTransfers {n}");
        }
        let _ = writeln!(out, "FollowDirectory {}", flag(self.follow_directory));
        if let Some(d) = &self.x11_display {
            let _ = writeln!(out, "X11Display {}", arg(d));
        }
        for f in &self.favourites {
            let _ = writeln!(out, "Favourite {}", arg(f));
        }
        for r in &self.theme_rules {
            out.push_str("ThemeRule\n");
            if !r.hosts.is_empty() {
                let hosts: Vec<String> = r.hosts.iter().map(|h| arg(h)).collect();
                let _ = writeln!(out, "    Hosts {}", hosts.join(" "));
            }
            if !r.tags.is_empty() {
                let tags: Vec<String> = r.tags.iter().map(|t| arg(t)).collect();
                let _ = writeln!(out, "    Tags {}", tags.join(" "));
            }
            let _ = writeln!(out, "    Theme {}", arg(&r.theme));
            if let Some(([red, green, blue], amount)) = r.tint {
                let _ = writeln!(out, "    Tint {red:02x}{green:02x}{blue:02x} {amount}");
            }
        }
        for p in self.profiles.iter().filter(|p| !p.is_empty()) {
            let _ = writeln!(out, "Profile {}", arg(&p.host));
            if let Some(f) = &p.font {
                let _ = writeln!(out, "    Font {}", arg(f));
            }
            if let Some(size) = p.font_size {
                let _ = writeln!(out, "    FontSize {size}");
            }
            if let Some(t) = &p.theme {
                let _ = writeln!(out, "    Theme {}", arg(t));
            }
            if let Some(n) = p.scrollback {
                let _ = writeln!(out, "    Scrollback {n}");
            }
            if let Some(t) = &p.tmux {
                let _ = writeln!(out, "    Tmux {}", arg(t));
            }
            if let Some(l) = &p.tmux_layout {
                let _ = writeln!(out, "    TmuxLayout {l}");
            }
            if p.legacy_algorithms {
                let _ = writeln!(out, "    LegacyAlgorithms yes");
            }
            if let Some(port) = p.desktop_port {
                let _ = writeln!(out, "    DesktopPort {port}");
            }
            if let Some(port) = p.vnc_port {
                let _ = writeln!(out, "    VncPort {port}");
            }
            if p.kind != SessionKind::Ssh {
                let _ = writeln!(out, "    Type {}", p.kind.name());
            }
            if let Some(v) = &p.via {
                let _ = writeln!(out, "    Via {}", arg(v));
            }
            if let Some(line) = &p.serial {
                let _ = writeln!(out, "    Serial {line}");
            }
            match &p.password {
                PasswordSource::Ask => {}
                PasswordSource::Windows => out.push_str("    PasswordSource windows\n"),
                PasswordSource::Command(c) => {
                    let line: String = c.chars().filter(|c| !c.is_control()).collect();
                    let _ = writeln!(out, "    PasswordCommand {}", line.trim());
                }
            }
        }
        out
    }

    pub fn profile(&self, host: &str) -> Option<&Profile> {
        self.profiles
            .iter()
            .find(|p| p.host.eq_ignore_ascii_case(host))
    }

    /// The host's profile, made if missing.
    pub fn profile_mut(&mut self, host: &str) -> &mut Profile {
        let i = match self
            .profiles
            .iter()
            .position(|p| p.host.eq_ignore_ascii_case(host))
        {
            Some(i) => i,
            None => {
                self.profiles.push(Profile {
                    host: host.to_owned(),
                    ..Profile::default()
                });
                self.profiles.len() - 1
            }
        };
        // `i` is in range: found above or just pushed.
        #[allow(clippy::indexing_slicing)]
        &mut self.profiles[i]
    }

    pub fn is_favourite(&self, host: &str) -> bool {
        self.favourites.iter().any(|f| f.eq_ignore_ascii_case(host))
    }

    /// Adds or removes a favourite; returns whether it is one now.
    pub fn toggle_favourite(&mut self, host: &str) -> bool {
        if self.is_favourite(host) {
            self.favourites.retain(|f| !f.eq_ignore_ascii_case(host));
            false
        } else {
            self.favourites.push(host.to_owned());
            true
        }
    }

    /// The theme rules: the user's, then the built-in tag rules.
    pub fn host_themes(&self) -> crate::HostThemes {
        let mut themes = crate::HostThemes::new();
        for r in &self.theme_rules {
            themes.push(r.clone());
        }
        themes
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    const TEXT: &str = r#"
# comment
SidePane right
TabStrip no
Scrollback 50000
DownloadFolder "D:\My Transfers"
VerifyTransfers yes
ParallelTransfers 20
FollowDirectory no
X11Display 192.168.1.5:0.0
Favourite web "db one"
Favorite WEB
ThemeRule
    Hosts prod-* *.prod.example.com
    Tags production
    Theme Production
    Tint #800000 60
ThemeRule
    Theme Nord
Profile web
    Font "Cascadia Mono"
    FontSize 12.5
    Theme Dracula
    Scrollback 99999999
    Tmux work
    TmuxLayout TMUX
    LegacyAlgorithms YES
    DesktopPort 3390
    VncPort 5901
    Type RDP
    Via jump-1
    PasswordCommand op read "op://Private/web 1/password"
    Unknown thing
Profile big
    FontSize 400
    Theme Solarized Dark
    Tmux no
    TmuxLayout split
    LegacyAlgorithms maybe
    DesktopPort 0
    Type telnet
    Via "a b"
    PasswordSource Windows
Profile console
    Type Serial
    Serial 115200,7E1,rtscts
NewKeyword later
"#;

    #[test]
    fn parses() {
        let s = Settings::parse(TEXT);
        assert!(s.side_pane_right);
        assert!(!s.tab_strip);
        assert_eq!(s.favourites, ["web", "db one"]);
        assert_eq!(
            s.theme_rules.len(),
            1,
            "a rule without hosts or tags is dropped"
        );
        let r = &s.theme_rules[0];
        assert_eq!(r.hosts, ["prod-*", "*.prod.example.com"]);
        assert_eq!(r.tint, Some(([0x80, 0, 0], 60)));
        let web = s.profile("WEB").unwrap();
        assert_eq!(web.font.as_deref(), Some("Cascadia Mono"));
        assert_eq!(web.font_size, Some(12.5));
        assert_eq!(web.theme.as_deref(), Some("Dracula"));
        assert_eq!(web.scrollback, Some(1_000_000));
        assert_eq!(web.tmux.as_deref(), Some("work"));
        assert_eq!(web.tmux_layout.as_deref(), Some("tmux"));
        assert!(web.legacy_algorithms);
        assert_eq!(web.desktop_port, Some(3390));
        assert_eq!(web.vnc_port, Some(5901));
        assert_eq!(web.kind, SessionKind::Rdp);
        assert_eq!(web.via.as_deref(), Some("jump-1"));
        assert_eq!(
            web.password,
            PasswordSource::Command(r#"op read "op://Private/web 1/password""#.into())
        );
        assert_eq!(s.scrollback, Some(50_000));
        assert_eq!(s.download_folder.as_deref(), Some("D:\\My Transfers"));
        assert!(s.verify_transfers);
        assert_eq!(s.parallel_transfers, Some(8));
        assert!(!s.follow_directory);
        assert_eq!(s.x11_display.as_deref(), Some("192.168.1.5:0.0"));
        let big = s.profile("big").unwrap();
        assert_eq!(big.font_size, None);
        assert_eq!(big.theme.as_deref(), Some("Solarized Dark"));
        assert_eq!(big.tmux, None, "tmux stays off unless a session is named");
        assert_eq!(big.tmux_layout, None);
        assert!(!big.legacy_algorithms, "only `yes` turns it on");
        assert_eq!(big.desktop_port, None, "port 0 is no port");
        assert_eq!(big.kind, SessionKind::Ssh);
        assert_eq!(big.via, None);
        assert_eq!(big.password, PasswordSource::Windows);
        let console = s.profile("console").unwrap();
        assert_eq!(console.kind, SessionKind::Serial);
        assert_eq!(
            console.serial.map(|l| l.to_string()).as_deref(),
            Some("115200,7E1,rtscts")
        );
    }

    #[test]
    fn round_trips() {
        let s = Settings::parse(TEXT);
        let again = Settings::parse(&s.to_text());
        assert_eq!(again, s);
        assert_eq!(Settings::parse(""), Settings::default());
    }

    #[test]
    fn writes_safe_lines() {
        let mut s = Settings::default();
        s.profile_mut("evil\nProfile x").theme = Some("a\"b".into());
        let text = s.to_text();
        assert!(!text.contains("\nProfile x"));
        s.profile_mut("cmd").password = PasswordSource::Command("bw get\nProfile y".into());
        let text = s.to_text();
        assert!(!text.contains("\nProfile y"));
        let back = Settings::parse(&text);
        assert_eq!(back.profiles.len(), 2);
        assert_eq!(back.profiles[0].theme.as_deref(), Some("ab"));
    }

    #[test]
    fn favourites_toggle() {
        let mut s = Settings::default();
        assert!(s.toggle_favourite("web"));
        assert!(s.is_favourite("Web"));
        assert!(!s.toggle_favourite("WEB"));
        assert!(s.favourites.is_empty());
    }
}
