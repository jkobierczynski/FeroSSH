# Settings

FeroSSH keeps its settings in `%APPDATA%\FeroSSH\ferossh.conf`. Most of them are changed from the menus, and FeroSSH rewrites the file when they change, but you can edit it by hand too. It uses ssh_config syntax: one keyword per line, case doesn't matter, `#` starts a comment, and indented lines belong to the `Profile` or `ThemeRule` above them. Unknown keywords are skipped, so an older FeroSSH can read a newer file.

Other files in the same folder:

| File | What it holds |
| --- | --- |
| `sessions.conf` | Connections saved with the connection form, in ssh_config syntax. |
| `imported-<client>.conf`, `putty-sessions.conf` | Connections imported from other clients. Importing again replaces the file. |

FeroSSH reads `~/.ssh/config` and `~/.ssh/known_hosts` too, and writes only to `known_hosts` (when you trust a new host).

## Window and terminal

| Keyword | Default | Meaning |
| --- | --- | --- |
| `SidePane right` | left | Shows the connection manager and file view on the right. |
| `TabStrip no` | yes | Hides the tab strip. |
| `CopyOnSelect yes` | no | Copies a selection as soon as you make it, as PuTTY does. |
| `RightClickPaste yes` | no | Pastes on a right click, as PuTTY does. |
| `LogTimestamps yes` | no | Puts the time in front of each line of plain session logs (Session > Timestamp each log line). |
| `Scrollback 50000` | 10000 | Lines of scrollback per pane, up to 1,000,000. |
| `Favourite web db1` | | Hosts marked as favourites in the connection manager. |
| `UpdateCheck weekly` | off | Looks for a new release once a week (Help > Check for updates weekly). |
| `AutoReconnect no` | yes | Whether a dropped SSH connection reconnects on its own (File > Reconnect automatically). |

## File transfer

| Keyword | Default | Meaning |
| --- | --- | --- |
| `DownloadFolder "D:\Transfers"` | your Downloads folder | Where downloads go. |
| `VerifyTransfers yes` | no | Compares SHA-256 on both ends after each file (the server needs `sha256sum`). |
| `ParallelTransfers 4` | 3 | Files that move at once, 1 to 8. |
| `FollowDirectory no` | yes | Whether the file view follows the shell's `cd` (OSC 7). |
| `ZModem yes` | no | Answers `rz` and `sz` in the terminal with ZMODEM (Session > ZMODEM: answer rz and sz). |
| `Standby yes` | no | A global hotkey hides the FeroSSH window and brings it back (Window > Standby window). The first window that opens takes the hotkey. |
| `StandbyHotkey Ctrl+Alt+Space` | `Win+Oem3` | The standby window's hotkey: Ctrl, Alt, Shift and Win joined by `+` to a key. Keys are letters, digits, `F1` to `F24`, `Space`, `Esc`, `Pause`, `ScrollLock`, `Num0` to `Num9`, the arrows and the like, and the punctuation keys by code (`Oem3` is the key below Esc on a US keyboard). |
| `StandbyWindow notepad.exe` | this FeroSSH window | The program whose window the standby hotkey shows and hides, by file name (Window > Standby window > Window to show and hide). |
| `StandbyHidden 263044` | | Written by FeroSSH while the standby hotkey has another program's window hidden, so that the window comes back at the next start if FeroSSH ended first. Leave it alone. |
| `TmuxKeybindings yes` | no | In panes of a tmux session, the tmux prefix and a window or pane key work the tabs and panes (Session > Use tmux keybindings for tabs and panes). |

## Snippets

| Keyword | Default | Meaning |
| --- | --- | --- |
| `ConfirmProduction no` | yes | Whether a snippet typed into a pane with the Production theme asks first (Session > Confirm snippets on Production hosts). |
| `AutoResponses no` | yes | Whether snippets with a `When` text answer by themselves (Session > Auto-responses from snippets). |
| `RunParallel 8` | 4 | How many hosts a multi-host snippet run logs in to at once, 1 to 16. |

## Remote apps

| Keyword | Default | Meaning |
| --- | --- | --- |
| `RemoteAppCopyAgent no` | yes | Whether FeroSSH copies the FeroStream agent to a host that lacks it (Session > Remote apps > Copy the agent to hosts). |
| `RemoteAppRemoveAgent yes` | no | Whether a copied agent deletes itself when its last program ends (Session > Remote apps > Remove the agent when done). |
| `RemoteAppAgentPath ~/bin/ferostream-agent` | `~/.local/bin/ferostream-agent` | Where on the host the agent is copied to: under `~/` or an absolute path, letters, digits and `._-+/` only. |
| `RemoteAppClipboard to-host` | off | Which way copied text goes between remote apps and Windows: `both`, `to-host`, `from-host` or `off` (Session > Remote apps > Clipboard). |
| `RemoteAppShareSelection yes` | no | Text selected with the mouse in a remote app (X PRIMARY, how xterm copies) goes to the Windows clipboard too, not only copied text. |
| `DesktopClipboard to-host` | off | Which way copied text goes between remote and virtual desktops and Windows: `both`, `to-host`, `from-host` or `off` (Session > Desktop clipboard). |
| `DesktopShareSelection yes` | no | Text selected with the mouse in a virtual desktop goes to the Windows clipboard too, not only copied text. |
| `RemoteAppPacking strong` | auto | How hard the host packs remote app pictures: `auto` picks from the measured link speed, `fast` uses LZ4 (least CPU), `strong` uses Deflate (fewest bytes, for slow links). Unchanged pixels are never sent and scrolls go as copies either way. |

## X11

| Keyword | Default | Meaning |
| --- | --- | --- |
| `X11Display 192.168.1.5:0.0` | `DISPLAY`, else `localhost:0` | The X server that forwarded X11 programs draw on. |

## Profiles

A `Profile` holds the settings for one host, named as in your ssh_config or saved connections.

```
Profile web1
    Font "Cascadia Mono"
    FontSize 12
    Theme Dracula
    Scrollback 100000
    Tmux main
    TmuxLayout ferossh
    StartDir /srv/www
```

| Keyword | Meaning |
| --- | --- |
| `Font NAME` | Font family. |
| `FontSize N` | Text size in points. |
| `Theme NAME` | Colour theme, for example `Nord`, `Dracula` or `Production`. |
| `Scrollback N` | Lines of scrollback for this host's panes. |
| `Tmux SESSION` | Attaches to this tmux session after every login. `off`, `no` or `none` (or leaving it out) means no tmux; `on`, `yes` or `true` attaches to `main`. |
| `StartDir PATH` | The server directory shells on this host start in: FeroSSH types `cd` after each login (before tmux attaches) and gives a new tmux session `-c`. New tabs and panes follow the pane they came from and use it only when that pane's directory is not known. Session > Start new tabs and panes in this directory sets it from the focused pane. |
| `TmuxLayout ferossh` or `tmux` | `ferossh` (the default) makes each native pane its own tmux window, so each can have its own font; `tmux` uses tmux's own splits. |
| `AutoReconnect no` or `yes` | Overrides the global `AutoReconnect` for this host, for example to keep a production host from logging in again by itself. |
| `LegacyAlgorithms yes` | Also offers older crypto (ECDH on NIST curves, DH groups 14 to 18 with SHA-2, AES-CTR, plain HMAC-SHA-2) for old switches and routers. Still no SHA-1, CBC or DSA. The tab shows an amber ⚠. |
| `DesktopPort N` | The server's RDP port for Open remote desktop (3389). |
| `VncPort N` | The server's VNC port (5900). |
| `DesktopSize follow` or `WxH` | The size of this connection's remote and virtual desktops: following the pane (the default) or fixed, such as `1920x1080`. See [Desktop size](everyday-use.md#desktop-size). |
| `VirtualDesktop COMMAND` | What Session > Open virtual desktop runs in its Xvfb display: a desktop session or one program, as a shell command, such as `startxfce4` or `firefox`. Unset, it runs `~/.xsession` or the first desktop session the server has. |
| `RemoteApp COMMAND` | The program a remote app connection runs, and the first one Run remote app offers on this host, such as `gimp` or `xterm -T 'my term'`. |
| `RemoteAppCopyAgent yes` or `no` | Overrides `RemoteAppCopyAgent` for this connection. |
| `RemoteAppRemoveAgent yes` or `no` | Overrides `RemoteAppRemoveAgent` for this connection. |
| `RemoteAppClipboard to-host`, `from-host` or `off` | Narrows the clipboard for this connection's remote apps (*App clip*); it never widens the global setting. |
| `DesktopClipboard to-host`, `from-host` or `off` | Narrows the clipboard for this connection's remote and virtual desktops (*Desk clip*); it never widens the global setting. A desktop opened through another connection is held to both connections' limits. |
| `Type ssh`, `telnet`, `serial`, `rdp`, `vnc`, `xvfb` or `remoteapp` | The kind of connection; `xvfb` is a virtual desktop and `remoteapp` a remote app, both on the `Via` host. |
| `Serial 115200,8N1,rtscts` | For a serial console: speed, framing (8N1, 8E1, 8O1, 7E1, 7O1, 8N2) and flow control (`none`, `xonxoff`, `rtscts`, `dsrdtr`), in any order. Left out: 9600, 8N1, none. |
| `LoginScript STEP` | One step of the login script, one line per step: `expect TEXT`, `send TEXT`, `password`, `secret NAME`, `pause SECONDS`. See [Login scripts](everyday-use.md#login-scripts). |
| `Via SESSION` | For RDP, VNC, virtual desktops and remote apps: log in to this saved SSH connection first and tunnel the desktop through it. |
| `PasswordSource windows` | Takes the password from Windows Credential Manager, after the first login that works. |
| `PasswordCommand ...` | Runs this command (no shell, no window, 60 seconds at most) and uses the first line it prints as the password. `%n` is the connection's name, `%h` the host, `%u` the user, `%p` the port. FeroSSH asks before running a command the first time. |

A login script for a console server could look like this:

```
Profile core-console
    Type serial
    Serial 115200,8N1,none
    LoginScript expect Username:
    LoginScript send admin
    LoginScript expect Password:
    LoginScript password
    LoginScript expect >
    LoginScript send enable
    LoginScript expect Password:
    LoginScript secret enable
```

## Secrets

A login script's `secret NAME` step, or a snippet's `@secret NAME`, types a named secret. Unless a `Secret` block names a command for it, FeroSSH asks for it once and keeps it in Windows Credential Manager as `FeroSSH secret:NAME`. With a block, the command's first line of output is the secret; `%n` is the secret's name. FeroSSH asks before it runs a command the first time.

```
Secret enable
    PasswordCommand op read "op://Network/%n/password"
```

## Snippets

Command snippets are not kept in `ferossh.conf` but in a file each in `%APPDATA%\FeroSSH\snippets`, in the same syntax, with the folder a file is in as its category (see [The snippets folder](everyday-use.md#the-snippets-folder)). The snippet editor (Tools > Command snippets, then Insert or F2) writes them, but you can also write a `.snippet` file by hand, one snippet per file:

```
Snippet "Back up the config"
    Hosts sw-* core-*
    Folder Network
    Hotkey F2
    Line copy running-config tftp://10.0.0.5/${host}-${stamp}.cfg
    Line @expect ]?
    Line @key enter
```

and in another file:

```
Snippet "Next page"
    Hosts sw-*
    When --More--
    Line @key space
```

| Keyword | Meaning |
| --- | --- |
| `Snippet NAME` | Starts a snippet. |
| `Line TEXT` | One line to type, or an `@` step. See [Command snippets](everyday-use.md#command-snippets). |
| `Hosts PATTERN...` | Keeps the snippet to these hosts. |
| `Folder NAME` | Keeps it to the hosts in this connection-manager folder. |
| `Hotkey F1` to `F12` | Runs it with Ctrl+Shift and that key. |
| `When TEXT` | Makes it an auto-response: it runs whenever the host prints this text. |

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
