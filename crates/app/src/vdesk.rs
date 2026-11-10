//! Session > Open virtual desktop: a desktop that exists only on the
//! server, in Xvfb, shown through x11vnc.
//!
//! FeroSSH runs [`script`] on an exec channel of the pane's connection.
//! The script checks for Xvfb, x11vnc and xauth, starts Xvfb on a free
//! display with a fresh cookie (only this user can draw on it), runs a
//! desktop session or the host's `VirtualDesktop` program in it, and
//! starts x11vnc on that display, listening on the server's loopback
//! only and asking for a one-time password. It then prints its port; the
//! window tunnels that port through SSH and opens a viewer on it. When
//! the channel's input ends (the tab closed, Stop virtual desktop, the
//! connection dropped) or the program in the display ends, the script
//! stops x11vnc, the program and Xvfb and removes its files.
//!
//! Copied text crosses only the ways the clipboard settings allow, and
//! the server enforces that too: x11vnc gets `-seldir` or `-nosel`,
//! Xvnc `-AcceptCutText` and `-SendCutText`; mouse selections (X
//! PRIMARY) go out only when DesktopShareSelection says so.
//!
//! The script is a constant: what varies (password, program, screen
//! size, clipboard) arrives on its standard input, never in the command line, so
//! nothing is quoted into it and `ps` on the server shows no secret. It
//! is one line of POSIX sh without quotes inside, so the login shell
//! (bash, dash, zsh, fish, tcsh) passes it to `sh` unchanged.

use zeroize::Zeroizing;

/// What the script prints at the start of each line meant for FeroSSH.
const MARK: &str = "FEROSSH-VD ";

/// The script's source, one statement per line for reading; [`script`]
/// joins the lines. Each line ends where a newline could be a space.
const SCRIPT: &str = r#"umask 077;
exec 3<&0;
IFS= read -r pw || exit 4;
IFS= read -r prog || exit 4;
IFS= read -r size || exit 4;
IFS= read -r mode || exit 4;
read -r ca cs cp || exit 4;
[ "$ca" = 1 ] || ca=0; [ "$cs" = 1 ] || cs=0; [ "$cp" = 1 ] || cp=0;
xs=; [ $ca = 1 ] || xs="-seldir send"; [ $cs = 1 ] || xs="-seldir recv"; [ $ca$cs = 00 ] && xs=-nosel; [ $cp = 1 ] || xs="$xs -noprimary";
case "$size" in *[0-9]x*[0-9]) ;; *) size=1600x900;; esac;
have() { command -v $1 >/dev/null 2>&1; };
b=;
if [ "$mode" = follow ] && have Xvnc && have vncpasswd; then b=xvnc; elif have Xvfb && have x11vnc; then b=xvfb; elif have Xvnc && have vncpasswd; then b=xvnc; fi;
m=;
if [ -z "$b" ]; then for c in Xvfb x11vnc; do have $c || m="$m $c"; done; fi;
have xauth || m="$m xauth";
if [ -n "$m" ]; then echo "FEROSSH-VD missing$m"; exit 3; fi;
d=$(mktemp -d "${TMPDIR:-/tmp}/ferossh-vd.XXXXXX") || { echo "FEROSSH-VD error mktemp failed"; exit 5; };
x=; v=; a=; w=;
stop() { [ -z "$w" ] || kill $w 2>/dev/null; [ -z "$v" ] || kill $v 2>/dev/null; if [ -n "$a" ]; then kill -s TERM -- -$a 2>/dev/null || kill $a 2>/dev/null; fi; [ -z "$x" ] || kill $x 2>/dev/null; rm -rf "$d"; };
trap stop EXIT;
trap "exit 1" HUP INT TERM PIPE;
if [ "$b" = xvnc ]; then echo "$pw" | vncpasswd -f >"$d/vncpw" 2>/dev/null || { echo "FEROSSH-VD error vncpasswd failed"; exit 5; }; fi;
r=0; [ "$mode" = follow ] && r=1;
n=20;
while [ $n -lt 100 ]; do
if [ -e /tmp/.X$n-lock ] || [ -e /tmp/.X11-unix/X$n ]; then n=$((n+1)); continue; fi;
k=$(od -An -N16 -tx1 /dev/urandom | tr -dc 0-9a-f);
xauth -q -f "$d/auth" add :$n . $k >/dev/null 2>&1;
if [ "$b" = xvnc ]; then
Xvnc :$n -auth "$d/auth" -nolisten tcp -localhost -interface 127.0.0.1 -rfbport $((5900+n)) -SecurityTypes VncAuth -PasswordFile "$d/vncpw" -AcceptSetDesktopSize=$r -AcceptCutText=$ca -SendCutText=$cs -SendPrimary=$cp -AlwaysShared -geometry $size -depth 24 </dev/null >"$d/xvfb.log" 2>&1 3<&- &
else
Xvfb :$n -auth "$d/auth" -nolisten tcp -screen 0 ${size}x24 </dev/null >"$d/xvfb.log" 2>&1 3<&- &
fi;
x=$!;
i=0;
o() { [ -e /tmp/.X11-unix/X$n ] && [ "$(tr -dc 0-9 </tmp/.X$n-lock 2>/dev/null)" = "$x" ]; };
while [ $i -lt 50 ]; do o && break; kill -0 $x 2>/dev/null || break; sleep 0.2; i=$((i+1)); done;
if o && kill -0 $x 2>/dev/null; then break; fi;
kill $x 2>/dev/null; x=; n=$((n+1));
done;
[ -n "$x" ] || { echo "FEROSSH-VD error the X server did not start: $(tail -n 2 "$d/xvfb.log" | tr -s "[:space:]" " ")"; exit 6; };
DISPLAY=:$n; XAUTHORITY="$d/auth"; export DISPLAY XAUTHORITY;
unset WAYLAND_DISPLAY SESSION_MANAGER DBUS_SESSION_BUS_ADDRESS;
if [ -z "$prog" ]; then
if [ -f "$HOME/.xsession" ]; then prog="sh $HOME/.xsession";
else for c in startxfce4 mate-session startlxqt startlxde startplasma-x11 x-session-manager xterm; do if have $c; then prog=$c; break; fi; done;
fi;
fi;
[ -n "$prog" ] || { echo "FEROSSH-VD error no desktop session found"; exit 7; };
r=; have dbus-run-session && r=dbus-run-session;
s=; have setsid && s=setsid;
$s $r sh -c "$prog" </dev/null >"$d/app.log" 2>&1 3<&- &
a=$!;
q=;
if [ "$b" = xvnc ]; then q=$((5900+n));
else
echo "$pw" >"$d/pw";
x11vnc -display :$n -auth "$d/auth" -localhost -forever -shared -norc $xs -passwdfile "rm:$d/pw" </dev/null >"$d/vnc.out" 2>"$d/vnc.log" 3<&- &
v=$!;
i=0;
while [ $i -lt 150 ]; do q=$(sed -n "s/^PORT=//p" "$d/vnc.out"); [ -n "$q" ] && break; kill -0 $v 2>/dev/null || break; sleep 0.2; i=$((i+1)); done;
[ -n "$q" ] || { echo "FEROSSH-VD error x11vnc did not start: $(tail -n 2 "$d/vnc.log" | tr -s "[:space:]" " ")"; exit 8; };
fi;
echo "FEROSSH-VD ready $q :$n $b";
{ cat; kill -s HUP $$; } <&3 >/dev/null 2>&1 &
w=$!;
exec 3<&-;
wait $a;
echo "FEROSSH-VD ended $(tail -n 2 "$d/app.log" | tr -s "[:space:]" " ")";
"#;

/// The command for the exec channel: `sh -c` and the script in single
/// quotes, which it never holds.
pub fn command() -> Vec<u8> {
    let joined: Vec<&str> = SCRIPT
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    format!("sh -c '{}'", joined.join(" ")).into_bytes()
}

/// Which ways the virtual desktop's server lets copied text travel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Clip {
    /// From the viewer (Windows) into the desktop.
    pub to_host: bool,
    /// From the desktop to the viewer.
    pub from_host: bool,
    /// Mouse selections (X PRIMARY) go to the viewer too, not only
    /// copies (CLIPBOARD).
    pub selection: bool,
}

/// What the script reads on its standard input before it starts: the
/// VNC password, the program to run (empty: a desktop session), the
/// screen size, whether it should `follow` the viewer's size (the
/// script then prefers Xvnc, which can) and the clipboard's ways. `None`
/// when the program cannot travel as one line.
pub fn input(
    password: &str,
    program: Option<&str>,
    width: u16,
    height: u16,
    follow: bool,
    clip: Clip,
) -> Option<Zeroizing<Vec<u8>>> {
    let program = program.unwrap_or_default();
    if [password, program]
        .iter()
        .any(|t| t.chars().any(char::is_control))
    {
        return None;
    }
    let (width, height) = (width.clamp(320, 8192), height.clamp(200, 8192));
    let mode = if follow { "follow" } else { "fixed" };
    let bit = u8::from;
    let clip = format!(
        "{} {} {}",
        bit(clip.to_host),
        bit(clip.from_host),
        bit(clip.from_host && clip.selection)
    );
    Some(Zeroizing::new(
        format!("{password}\n{program}\n{width}x{height}\n{mode}\n{clip}\n").into_bytes(),
    ))
}

/// A fresh VNC password: eight letters and digits (VNC uses eight
/// bytes), good for one virtual desktop.
pub fn password() -> Zeroizing<String> {
    const ALPHABET: &[u8] = b"abcdefghijkmnpqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    // Rejection sampling keeps the letters equally likely.
    let limit = 256 - 256 % ALPHABET.len();
    let mut out = Zeroizing::new(String::with_capacity(8));
    while out.len() < 8 {
        let mut b = [0u8; 1];
        if getrandom::fill(&mut b).is_err() {
            continue;
        }
        let n = usize::from(b[0]);
        if n < limit
            && let Some(&c) = ALPHABET.get(n % ALPHABET.len())
        {
            out.push(char::from(c));
        }
    }
    out
}

/// What a line of the script's output said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The server lacks these programs.
    Missing(Vec<String>),
    /// It could not start; why, in the script's words.
    Error(String),
    /// The VNC server listens on this port of the server's loopback,
    /// showing this display; `resizes` when it is Xvnc, which takes the
    /// viewer's size changes (x11vnc on Xvfb keeps its size).
    Ready {
        port: u16,
        display: String,
        resizes: bool,
    },
    /// The program in the display ended, with its last words.
    Ended(String),
}

/// Reads the script's output: whole lines with [`MARK`]; anything else
/// is kept a little, to show when the script fails without a word.
#[derive(Debug, Default)]
pub struct Output {
    partial: Vec<u8>,
    /// The last lines without a mark.
    pub other: Vec<String>,
}

/// The longest line kept.
const MAX_LINE: usize = 4096;

impl Output {
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Event> {
        let mut events = Vec::new();
        for &b in bytes {
            if b == b'\n' {
                let line = String::from_utf8_lossy(&self.partial).into_owned();
                self.partial.clear();
                match parse(&line) {
                    Some(e) => events.push(e),
                    None if !line.trim().is_empty() => {
                        self.other.push(line.trim().to_owned());
                        if self.other.len() > 4 {
                            self.other.remove(0);
                        }
                    }
                    None => {}
                }
            } else if self.partial.len() < MAX_LINE {
                self.partial.push(b);
            }
        }
        events
    }
}

fn parse(line: &str) -> Option<Event> {
    let rest = line.trim_end_matches('\r').strip_prefix(MARK)?;
    let (word, text) = rest.split_once(' ').unwrap_or((rest, ""));
    let text = crate::ssh::sanitize(text.trim());
    Some(match word {
        "missing" => Event::Missing(text.split_whitespace().map(str::to_owned).collect()),
        "error" => Event::Error(text),
        "ended" => Event::Ended(text),
        "ready" => {
            let mut parts = text.split_whitespace();
            let port = parts.next()?.parse().ok().filter(|&p| p != 0)?;
            let display = parts.next().unwrap_or_default().to_owned();
            let resizes = parts.next() == Some("xvnc");
            Event::Ready {
                port,
                display,
                resizes,
            }
        }
        _ => return None,
    })
}

/// How to install the `missing` programs, for the usual distributions.
pub fn install_advice(missing: &[String]) -> String {
    let has = |name: &str| missing.iter().any(|m| m == name);
    let pick = |names: [&'static str; 3]| -> Vec<&'static str> {
        ["Xvfb", "x11vnc", "xauth"]
            .iter()
            .zip(names)
            .filter(|(m, _)| has(m))
            .map(|(_, n)| n)
            .collect()
    };
    format!(
        "The server needs {} for a virtual desktop. Install {} there: \
         Debian or Ubuntu: sudo apt install {}; Fedora or RHEL (EPEL for x11vnc): \
         sudo dnf install {}; Arch: sudo pacman -S {}. For a desktop that follows the \
         window's size, install TigerVNC's Xvnc too (tigervnc-standalone-server, \
         tigervnc-server or tigervnc).",
        missing.join(", "),
        if missing.len() == 1 { "it" } else { "them" },
        pick(["xvfb", "x11vnc", "xauth"]).join(" "),
        pick(["xorg-x11-server-Xvfb", "x11vnc", "xorg-x11-xauth"]).join(" "),
        pick(["xorg-server-xvfb", "x11vnc", "xorg-xauth"]).join(" "),
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn the_script_is_one_line_without_quotes_or_backslashes() {
        for line in SCRIPT.lines().map(str::trim).filter(|l| !l.is_empty()) {
            assert!(
                ["; ", ";", "then", "do", "else", "&"]
                    .iter()
                    .any(|end| line.ends_with(end)),
                "a newline here is not a space: {line}"
            );
        }
        let command = String::from_utf8(command()).unwrap();
        let inner = command
            .strip_prefix("sh -c '")
            .unwrap()
            .strip_suffix('\'')
            .unwrap();
        assert!(!inner.contains(['\'', '\\', '\n']), "{inner}");
        // `!` only as `$!`: no history expansion to trip over in csh.
        assert!(!inner.replace("$!", "").contains('!'), "{inner}");
    }

    #[test]
    fn input_is_five_lines() {
        let pw = password();
        let both = Clip {
            to_host: true,
            from_host: true,
            selection: false,
        };
        let i = input(&pw, Some("xterm -fa Mono"), 1280, 720, true, both).unwrap();
        assert_eq!(
            &i[..],
            format!("{}\nxterm -fa Mono\n1280x720\nfollow\n1 1 0\n", *pw).as_bytes()
        );
        // Selections need the way from the host.
        let only_to = Clip {
            to_host: true,
            from_host: false,
            selection: true,
        };
        let i = input(&pw, None, 10, 60000, false, only_to).unwrap();
        assert_eq!(
            &i[..],
            format!("{}\n\n320x8192\nfixed\n1 0 0\n", *pw).as_bytes()
        );
        assert!(input(&pw, Some("evil\nrm -rf ~"), 800, 600, true, both).is_none());
    }

    #[test]
    fn passwords_are_eight_fresh_characters() {
        let a = password();
        let b = password();
        assert_eq!(a.len(), 8);
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric()));
        assert_ne!(*a, *b);
    }

    #[test]
    fn reads_the_marked_lines() {
        let mut o = Output::default();
        let mut events = o.feed(b"noise\nFEROSSH-VD missing Xvfb x11");
        events.extend(o.feed(b"vnc\nFEROSSH-VD ready 5901 :20 xvfb\r\nFEROSSH-VD ready 0\n"));
        events.extend(o.feed(b"FEROSSH-VD ready 5921 :21 xvnc\n"));
        events.extend(o.feed(b"FEROSSH-VD error Xvfb did not start: \x1b[31mno\n"));
        events.extend(o.feed(b"FEROSSH-VD ended\n"));
        assert_eq!(
            events,
            [
                Event::Missing(vec!["Xvfb".into(), "x11vnc".into()]),
                Event::Ready {
                    port: 5901,
                    display: ":20".into(),
                    resizes: false,
                },
                Event::Ready {
                    port: 5921,
                    display: ":21".into(),
                    resizes: true,
                },
                Event::Error("Xvfb did not start: [31mno".into()),
                Event::Ended(String::new()),
            ]
        );
        assert_eq!(o.other, ["noise", "FEROSSH-VD ready 0"]);
    }

    #[test]
    fn advice_names_the_packages() {
        let a = install_advice(&["x11vnc".into()]);
        assert!(a.contains("sudo apt install x11vnc;"), "{a}");
        let a = install_advice(&["Xvfb".into(), "xauth".into()]);
        assert!(a.contains("apt install xvfb xauth"), "{a}");
        assert!(a.contains("pacman -S xorg-server-xvfb xorg-xauth"), "{a}");
    }
}
