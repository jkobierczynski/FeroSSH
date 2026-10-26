# Settings

FeroSSH keeps its settings in `%APPDATA%\FeroSSH\ferossh.conf`. Most of them are changed from the menus, and FeroSSH rewrites the file when they change, but you can edit it by hand too. It uses ssh_config syntax: one keyword per line, case doesn't matter, `#` starts a comment, and indented lines belong to the `Profile` or `ThemeRule` above them. Unknown keywords are skipped, so an older FeroSSH can read a newer file.

Other files in the same folder:

| File | What it holds |
| --- | --- |
| `sessions.conf` | Sessions saved with the session form, in ssh_config syntax. |
| `imported-<client>.conf`, `putty-sessions.conf` | Sessions imported from other clients. Importing again replaces the file. |

FeroSSH reads `~/.ssh/config` and `~/.ssh/known_hosts` too, and writes only to `known_hosts` (when you trust a new host).

## Window and terminal

| Keyword | Default | Meaning |
| --- | --- | --- |
| `SidePane right` | left | Shows the session manager and file view on the right. |
| `TabStrip no` | yes | Hides the tab strip. |
| `CopyOnSelect yes` | no | Copies a selection as soon as you make it, as PuTTY does. |
| `RightClickPaste yes` | no | Pastes on a right click, as PuTTY does. |
| `Scrollback 50000` | 10000 | Lines of scrollback per pane, up to 1,000,000. |
| `Favourite web db1` | | Hosts marked as favourites in the session manager. |
| `UpdateCheck weekly` | off | Looks for a new release once a week (Help > Check for updates weekly). |
| `AutoReconnect no` | yes | Whether a dropped SSH connection reconnects on its own (File > Reconnect automatically). |

## File transfer

| Keyword | Default | Meaning |
| --- | --- | --- |
| `DownloadFolder "D:\Transfers"` | your Downloads folder | Where downloads go. |
| `VerifyTransfers yes` | no | Compares SHA-256 on both ends after each file (the server needs `sha256sum`). |
| `ParallelTransfers 4` | 3 | Files that move at once, 1 to 8. |
| `FollowDirectory no` | yes | Whether the file view follows the shell's `cd` (OSC 7). |

## X11

| Keyword | Default | Meaning |
| --- | --- | --- |
| `X11Display 192.168.1.5:0.0` | `DISPLAY`, else `localhost:0` | The X server that forwarded X11 programs draw on. |

## Profiles

A `Profile` holds the settings for one host, named as in your ssh_config or saved sessions.

```
Profile web1
    Font "Cascadia Mono"
    FontSize 12
    Theme Dracula
    Scrollback 100000
    Tmux main
    TmuxLayout ferossh
```

| Keyword | Meaning |
| --- | --- |
| `Font NAME` | Font family. |
| `FontSize N` | Text size in points. |
| `Theme NAME` | Colour theme, for example `Nord`, `Dracula` or `Production`. |
| `Scrollback N` | Lines of scrollback for this host's panes. |
| `Tmux SESSION` | Attaches to this tmux session after every login. |
| `TmuxLayout ferossh` or `tmux` | `ferossh` (the default) makes each native pane its own tmux window, so each can have its own font; `tmux` uses tmux's own splits. |
| `AutoReconnect no` or `yes` | Overrides the global `AutoReconnect` for this host, for example to keep a production host from logging in again by itself. |
| `LegacyAlgorithms yes` | Also offers older crypto (ECDH on NIST curves, DH groups 14 to 18 with SHA-2, AES-CTR, plain HMAC-SHA-2) for old switches and routers. Still no SHA-1, CBC or DSA. The tab shows an amber ⚠. |
| `DesktopPort N` | The server's RDP port for Open remote desktop (3389). |
| `VncPort N` | The server's VNC port (5900). |
| `Type ssh`, `rdp` or `vnc` | The kind of session. |
| `Via SESSION` | For RDP and VNC: log in to this SSH session first and tunnel the desktop through it. |
| `PasswordSource windows` | Takes the password from Windows Credential Manager, after the first login that works. |
| `PasswordCommand ...` | Runs this command (no shell, no window, 60 seconds at most) and uses the first line it prints as the password. `%n` is the session's name, `%h` the host, `%u` the user, `%p` the port. FeroSSH asks before running a command the first time. |

## Theme rules

Rules pick a theme from the host's name or its ssh_config `Tag`. A host's profile wins over the rules, and the rules win over `--theme`.

```
ThemeRule
    Hosts *.prod.example.com !prod-sandbox
    Theme Production
ThemeRule
    Tags staging
    Theme Nord
    Tint ffb000 40
```

| Keyword | Meaning |
| --- | --- |
| `Hosts PATTERN...` | ssh_config-style host patterns; `!` excludes. |
| `Tags TAG...` | Matches hosts with this `Tag` in ssh_config. |
| `Theme NAME` | The theme to use. |
| `Tint RRGGBB AMOUNT` | Mixes a colour into the theme by 0 to 255, for example a red tint over your usual theme. |

Without any rules, hosts tagged `production`, `staging` or `development` get Production, Staging or Nord.
