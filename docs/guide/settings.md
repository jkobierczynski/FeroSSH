# Settings

FeroSSH keeps its settings in `%APPDATA%\FeroSSH\ferossh.conf`. Most of them are changed from the menus, and FeroSSH rewrites the file when they change, but you can edit it by hand too. It uses ssh_config syntax: one keyword per line, case doesn't matter, `#` starts a comment, and indented lines belong to the `Profile`, `ThemeRule` or `HighlightRule` above them. Unknown keywords are skipped, so an older FeroSSH can read a newer file.

Other files in the same folder:

| File | What it holds |
| --- | --- |
| `sessions.conf` | Connections saved with the connection form, in ssh_config syntax. |
| `imported-<client>.conf`, `putty-sessions.conf` | Connections imported from other clients. Importing again replaces the file. |
| `connections.vault` | With a master password set: all of the files above, and saved passwords, in one encrypted file. The plain files are then gone. |
| `vault-location.txt` | Where the vault is, when it was moved (to a OneDrive folder, say). |

FeroSSH reads `~/.ssh/config` and `~/.ssh/known_hosts` too, and writes only to `known_hosts` (when you trust a new host). A portable FeroSSH (a `FeroSSH-data` folder next to the exe) keeps all of these, and its own `known_hosts`, in that folder instead.

## Window and terminal

| Keyword | Default | Meaning |
| --- | --- | --- |
| `SidePane right` | left | Shows the connection manager and file view on the right. |
| `TabStrip no` | yes | Hides the tab strip. |
| `StatusBar yes` | no | Shows the server status bar under the panes. Settings > Server status bar. |
| `FullScreenKey no` | yes | Whether F11 switches full screen (Settings > F11 switches full screen). Off, F11 goes to the terminal. |
| `KeyBinding 213 Ctrl+K` | | The keys of one action, by its menu command number, written by Settings > Key bindings for the actions you changed: none after the number means no shortcut. Keys are spelled as for `StandbyHotkey` (`Ctrl+Shift+PageUp`, `Alt+Shift+OemPlus`); Win is not allowed. Actions with no `KeyBinding` line keep their default keys. |
| `WindowColours dark` | light | Where the menu bar, menus, title bar, tab strip and tools pane take their colours: `theme` (the focused pane's colour theme), `windows` (Windows' light or dark app mode), `light` (Windows' usual light look), or a neutral grey: `softgrey`, `silver`, `midgrey`, `steel`, `graphite`, `dark` or `black`. Settings > Window colours. |
| `CopyOnSelect yes` | no | Copies a selection as soon as you make it, as PuTTY does. |
| `RightClickPaste yes` | no | Pastes on a right click, as PuTTY does. |
| `CommandSuggestions yes` | no | Suggests commands as you type from the host's history and a short list of common ones, and keeps the commands you run in an encrypted history per host (Edit > Suggest commands as you type). |
| `LogTimestamps yes` | no | Puts the time in front of each line of plain session logs (Session > Logs and recordings > Timestamp each log line). |
| `Scrollback 50000` | 10000 | Lines of scrollback per pane, up to 1,000,000. |
| `Favourite web db1` | | Hosts marked as favourites in the connection manager. |
| `UpdateCheck weekly` | off | Looks for a new release once a week (Help > Check for updates weekly). |
| `AutoReconnect no` | yes | Whether a dropped SSH connection reconnects on its own (File > Reconnect automatically). |
| `Highlight no` | yes | Colours keywords in the output (Session > Highlight keywords). In a `Profile`, for that host only. |
| `HighlightStandard no` | yes | Whether the standard rules (errors, warnings, success, IP and MAC addresses) come after your own `HighlightRule`s. |

## File transfer

| Keyword | Default | Meaning |
| --- | --- | --- |
| `DownloadFolder "D:\Transfers"` | your Downloads folder | Where downloads go. |
| `TransferSort local name ascending remote size descending` | both by name, A to Z | How File transfer sorts each list: after `local` and `remote`, `name`, `size` or `modified`, then `ascending` or `descending`. Folders always come first. Clicking a column header sets it for that list. |
| `TransferLayout "stacked swapped 40"` | side by side, Local first, half each | How File transfer arranges its lists: `side` or `stacked`, `swapped` for Remote first, and the first list's share in percent. Layout ▾ and dragging the gap set it. |
| `Bookmark "web" "D:\Sites\web" "/var/www/web" "web1"` | none | A File transfer bookmark: its name, Local folder, Remote folder and the connection it is for, `""` for none (one folder at least). One line per bookmark; ★ Bookmarks ▾ adds, renames and deletes them. |
| `VerifyTransfers yes` | no | Compares SHA-256 on both ends after each file (the server needs `sha256sum` and `xargs`). |
| `ParallelTransfers 4` | 3 | Files that move at once, 1 to 8. |
| `FollowDirectory no` | yes | Whether the file view follows the shell's `cd` (OSC 7). |
| `ZModem yes` | no | Answers `rz` and `sz` in the terminal with ZMODEM (Session > ZMODEM: answer rz and sz). |
| `Standby yes` | no | A global hotkey hides the FeroSSH window and brings it back (Window > Standby window). The first window that opens takes the hotkey. |
| `StandbyHotkey Ctrl+Alt+Space` | `Win+Oem3` | The standby window's hotkey: Ctrl, Alt, Shift and Win joined by `+` to a key. Keys are letters, digits, `F1` to `F24`, `Space`, `Esc`, `Pause`, `ScrollLock`, `Num0` to `Num9`, the arrows and the like, and the punctuation keys by code (`Oem3` is the key below Esc on a US keyboard). |
| `StandbyWindow notepad.exe` | this FeroSSH window | The program whose window the standby hotkey shows and hides, by file name (Window > Standby window > Window to show and hide). |
| `StandbyHidden 263044` | | Written by FeroSSH while the standby hotkey has another program's window hidden, so that the window comes back at the next start if FeroSSH ended first. Leave it alone. |
| `TmuxKeybindings yes` | no | In panes of a tmux session, the tmux prefix and a window or pane key work the tabs and panes (Session > tmux > Use tmux keybindings for tabs and panes). |

## Scripts

| Keyword | Default | Meaning |
| --- | --- | --- |
| `ConfirmProduction no` | yes | Whether a script typed into a pane with the Production theme asks first (Session > Scripts > Confirm scripts on Production hosts). |
| `AutoResponses no` | yes | Whether scripts with a `When` text answer by themselves (Session > Scripts > Auto-responses from scripts). |
| `RunParallel 8` | 4 | How many hosts a multi-host script run logs in to at once, 1 to 16. |

## Remote apps

| Keyword | Default | Meaning |
| --- | --- | --- |
| `RemoteAppCopyAgent no` | yes | Whether FeroSSH copies the FeroStream agent to a host that lacks it (Session > Remote apps > Copy the agent to hosts). |
| `RemoteAppRemoveAgent yes` | no | Whether a copied agent deletes itself when its last program ends (Session > Remote apps > Remove the agent when done). |
| `RemoteAppAgentPath ~/bin/ferostream-agent` | `~/.local/bin/ferostream-agent` | Where on the host the agent is copied to: under `~/` or an absolute path, letters, digits and `._-+/` only. |
| `RemoteAppClipboard to-host` | off | Which way copied text goes between remote apps and Windows: `both`, `to-host`, `from-host` or `off` (Session > Remote apps > Clipboard). |
| `RemoteAppShareSelection yes` | no | Text selected with the mouse in a remote app (X PRIMARY, how xterm copies) goes to the Windows clipboard too, not only copied text. |
| `DesktopClipboard to-host` | off | Which way copied text goes between remote and virtual desktops and Windows: `both`, `to-host`, `from-host` or `off` (Session > Remote desktop > Clipboard). |
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
    LocalFolder "D:\Sites\web"
    RemoteFolder /var/www/web
```

| Keyword | Meaning |
| --- | --- |
| `Font NAME` | Font family. |
| `FontSize N` | Text size in points. |
| `Theme NAME` | Colour theme, for example `Nord`, `Dracula` or `Production`. |
| `Scrollback N` | Lines of scrollback for this host's panes. |
| `Tmux SESSION` | Attaches to this tmux session after every login. `off`, `no` or `none` (or leaving it out) means no tmux; `on`, `yes` or `true` attaches to `main`. |
| `StartDir PATH` | The server directory shells on this host start in: FeroSSH types `cd` after each login (before tmux attaches) and gives a new tmux session `-c`. New tabs and panes follow the pane they came from and use it only when that pane's directory is not known. Session > Start new tabs and panes in this directory sets it from the focused pane. |
| `LocalFolder PATH` | The folder on this PC the Local list of File transfer opens when it turns to this host (Local folder in the connection form). It does not change `DownloadFolder`. |
| `RemoteFolder PATH` | The server folder the Remote list of File transfer starts in for this host (Remote folder in the connection form), before following cd takes over. |
| `TmuxLayout ferossh` or `tmux` | `ferossh` (the default) makes each native pane its own tmux window, so each can have its own font; `tmux` uses tmux's own splits. |
| `AutoReconnect no` or `yes` | Overrides the global `AutoReconnect` for this host, for example to keep a production host from logging in again by itself. |
| `Highlight no` or `yes` | Overrides the global `Highlight` for this host: keyword highlighting off for a switch whose output is coloured already, say. |
| `LegacyAlgorithms yes` | Also offers older crypto (ECDH on NIST curves, DH groups 14 to 18 with SHA-2, AES-CTR, plain HMAC-SHA-2) for old switches and routers. Still no SHA-1, CBC or DSA. The tab shows an amber ⚠. |
| `DesktopPort N` | The server's RDP port for Open remote desktop (3389). |
| `VncPort N` | The server's VNC port (5900). |
| `DesktopSize follow` or `WxH` | The size of this connection's remote and virtual desktops: following the pane (the default) or fixed, such as `1920x1080`. See [Desktop size](everyday-use.md#desktop-size). |
| `VirtualDesktop COMMAND` | What Session > Remote desktop > Open virtual desktop runs in its Xvfb display: a desktop session or one program, as a shell command, such as `startxfce4` or `firefox`. Unset, it runs `~/.xsession` or the first desktop session the server has. |
| `RemoteApp COMMAND` | The program a remote app connection runs, and the first one Run remote app offers on this host, such as `gimp` or `xterm -T 'my term'`. |
| `RemoteAppCopyAgent yes` or `no` | Overrides `RemoteAppCopyAgent` for this connection. |
| `RemoteAppRemoveAgent yes` or `no` | Overrides `RemoteAppRemoveAgent` for this connection. |
| `RemoteAppClipboard to-host`, `from-host` or `off` | Narrows the clipboard for this connection's remote apps (*App clip*); it never widens the global setting. |
| `DesktopClipboard to-host`, `from-host` or `off` | Narrows the clipboard for this connection's remote and virtual desktops (*Desk clip*); it never widens the global setting. A desktop opened through another connection is held to both connections' limits. |
| `Type ssh`, `mosh`, `winrm`, `telnet`, `serial`, `rdp`, `vnc`, `xvfb` or `remoteapp` | The kind of connection; `mosh` logs in over SSH and runs the terminal over [Mosh](everyday-use.md#mosh), `winrm` opens PowerShell or cmd on a Windows server through [WinRM](everyday-use.md#winrm-powershell-on-windows-servers), `xvfb` is a virtual desktop and `remoteapp` a remote app, both on the `Via` host. |
| `MoshServer PROGRAM` | For a Mosh connection: the server program, when not `mosh-server` on the path, such as `~/bin/mosh-server`. |
| `MoshPorts 60001:60010` | For a Mosh connection: the UDP port, or range of ports, the server may listen on. Unset, any free one from 60001 to 60999. |
| `MoshPredict always` | For a Mosh connection: when typed text shows before the server echoes it: `adaptive` (only on a slow link, the default), `always` or `never`. |
| `WinrmCommand COMMAND` | For a WinRM connection: what its shell runs, when not `powershell -NoLogo`, such as `cmd`. |
| `WinrmHttps yes` | For a WinRM connection: use HTTPS (port 5986 unless `Port` is given) instead of HTTP with encrypted messages on 5985. |
| `Serial 115200,8N1,rtscts` | For a serial console: speed, framing (8N1, 8E1, 8O1, 7E1, 7O1, 8N2) and flow control (`none`, `xonxoff`, `rtscts`, `dsrdtr`), in any order. Left out: 9600, 8N1, none. |
| `LoginScript STEP` | One step of the login script, one line per step: `expect TEXT`, `send TEXT`, `password`, `secret NAME`, `pause SECONDS`, and the logic steps such as `on TEXT goto LABEL` and `label LABEL`. See [Login scripts](everyday-use.md#login-scripts). |
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

A login script's `secret NAME` step, or a script's `@secret NAME`, types a named secret. Unless a `Secret` block names a command for it, FeroSSH asks for it once and keeps it in Windows Credential Manager as `FeroSSH secret:NAME`. With a block, the command's first line of output is the secret; `%n` is the secret's name. FeroSSH asks before it runs a command the first time.

```
Secret enable
    PasswordCommand op read "op://Network/%n/password"
```

## Scripts

Scripts are not kept in `ferossh.conf` but in a file each in `%APPDATA%\FeroSSH\scripts`, in the same syntax, with the folder a file is in as its category (see [The scripts folder](everyday-use.md#the-scripts-folder)). The script editor (Tools > Scripts, then Insert or F2) writes them, but you can also write a `.script` file by hand (older `.snippet` files are read too), one script per file:

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
| `Snippet NAME` | Starts a script. |
| `Line TEXT` | One line to type, or an `@` step. See [Scripts](everyday-use.md#scripts). |
| `Hosts PATTERN...` | Keeps the script to these hosts. |
| `Folder NAME` | Keeps it to the hosts in this connection-manager folder. |
| `Hotkey F1` to `F12` | Runs it with Ctrl+Shift and that key. |
| `When TEXT` | Makes it an auto-response: it runs whenever the host prints this text. |

## Highlight rules

A `HighlightRule` colours what it matches in terminal output. The first rule that matches a stretch of text wins, so your rules come before the standard set.

```text
HighlightRule
    Words error failed "permission denied"
    Colour bright-red
    Bold yes
HighlightRule
    Pattern \bOOM-killer\b|Out of memory
    CaseSensitive yes
    Colour ffffff
    Background red
    Hosts db-* !db-test
    Stop yes
```

| Keyword | Meaning |
| --- | --- |
| `Words` | Whole words or phrases (quote a phrase), matched literally. |
| `Pattern` | A regular expression, the rest of the line as written. A rule can have both. |
| `CaseSensitive yes` | Matches only as written; any case otherwise. |
| `Colour`, `Background` | `black`, `red`, `green`, `yellow`, `blue`, `magenta`, `cyan`, `white`, `bright-` and one of those, a colour number 0 to 255, or `rrggbb`. Named colours follow the pane's theme. |
| `Bold yes`, `Underline yes` | Text style. |
| `Hosts` | Host patterns, as in `ThemeRule`; without it the rule is for every host and local shells. |
| `Stop yes` | Following the output stops at a match (see [Highlighting and following output](everyday-use.md#highlighting-and-following-output)). |

Only text in the terminal's own colours is recoloured. A rule that does not compile, such as an unclosed `(`, is left out and named in the pane when it opens. At most 64 rules are used, each pattern up to 1,000 characters.

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
