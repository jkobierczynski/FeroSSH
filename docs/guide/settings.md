# Settings

FeroSSH keeps its settings in `%APPDATA%\FeroSSH\ferossh.conf`. Most of them are changed from the menus, and FeroSSH rewrites the file when they change, but you can edit it by hand too. It uses ssh_config syntax: one keyword per line, case doesn't matter, `#` starts a comment, and indented lines belong to the `Profile`, `ThemeRule`, `SecurityRule` or `HighlightRule` above them. Unknown keywords are skipped, so an older FeroSSH can read a newer file.

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
| `Notifications yes` | no | Tab badges and Windows notifications when a pane asks for attention. Settings > Desktop notifications. |
| `NotifyBell no` | yes | Whether the bell counts as asking for attention. Settings > Notify on the bell too. |
| `AgentCommand claude --model opus` | claude, codex, aider | One coding agent command for Session > Coding agents, typed at the server's shell exactly as written (the rest of the line, quotes and `#` included). Repeat the line for more, up to 16, in menu order; any non-empty line replaces the built-in three; empty lines and lines with control characters are ignored. |
| `TeamLibrary PATH` | | The team's signed connection library, on a share or in a synced folder (see [Team library](#team-library)). Settings > Team library. |
| `TeamLibraryKey KEY` | | The public key the library must be signed with, as one line of a `.pub` file. Repeat the line for more than one key. |
| `DefaultTheme NAME` | GitHub Dark | The theme new tabs open with (Theme manager > Set as default). `--theme` and theme rules come before it. |
| `DefaultFont FAMILY` | Cascadia Mono | The font new tabs open with, when it is installed (Theme manager > Set as default on a font). |
| `DefaultFontSize POINTS` | | The font size new tabs open with, 4 to 96 points. |
| `FontLigatures yes` | no | Joins programming ligatures (`->`, `!=`, `===`) in fonts that have them, in every terminal pane. Settings > Font ligatures. |
| `FullScreenKey no` | yes | Whether F11 switches full screen (Settings > F11 switches full screen). Off, F11 goes to the terminal. |
| `KeyBinding 213 Ctrl+K` | | The keys of one action, by its menu command number, written by Settings > Key bindings for the actions you changed: none after the number means no shortcut. Keys are spelled as for `StandbyHotkey` (`Ctrl+Shift+PageUp`, `Alt+Shift+OemPlus`); Win is not allowed. Actions with no `KeyBinding` line keep their default keys. |
| `WindowColours dark` | light | Where the menu bar, menus, title bar, tab strip and tools pane take their colours: `theme` (the focused pane's colour theme), `windows` (Windows' light or dark app mode), `light` (Windows' usual light look), a neutral grey: `softgrey`, `silver`, `midgrey`, `steel`, `graphite`, `dark` or `black`, or a tinted neutral: `sand`, `sage`, `mist`, `taupe`, `slate`, `moss`, `espresso` or `heather`. Settings > Window colours. |
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
| `SavedSearch QUERY` | none | A connection manager search kept as a folder (one line each), for example `SavedSearch tag:prod -tag:legacy`. The search language is in [Everyday use](everyday-use.md#the-connection-manager). |
| `ScriptsExpanded "linux" "library/databases"` | none (all folded) | Which script categories are open in the scripts pane; the rest start folded. Set by folding and opening categories there. |
| `TransferSort local name ascending remote size descending` | both by name, A to Z | How File transfer sorts each list: after `local` and `remote`, `name`, `size` or `modified`, then `ascending` or `descending`. Folders always come first. Clicking a column header sets it for that list. |
| `TransferLayout "stacked swapped 40"` | side by side, Local first, half each | How File transfer arranges its lists: `side` or `stacked`, `swapped` for Remote first, and the first list's share in percent. Layout ▾ and dragging the gap set it. |
| `Bookmark "web" "D:\Sites\web" "/var/www/web" "web1"` | none | A File transfer bookmark: its name, Local folder, Remote folder and the connection it is for, `""` for none (one folder at least). One line per bookmark; ★ Bookmarks ▾ adds, renames and deletes them. |
| `VerifyTransfers yes` | no | Compares SHA-256 on both ends after each file (the server needs `sha256sum` and `xargs`). |
| `ParallelTransfers 4` | 3 | Files that move at once, 1 to 8. |
| `FollowDirectory no` | yes | Whether the file view follows the shell's `cd` (OSC 7). |
| `ZModem yes` | no | Answers `rz` and `sz` in the terminal with ZMODEM (Session > ZMODEM: answer rz and sz). |
| `InlineImages yes` | no | Shows pictures the host prints with Sixel, the kitty graphics protocol or iTerm2 inline images (Session > Show images the host prints). |
| `UploadAndTypePath yes` | no | Pasting an image or copied files, or dropping files on a pane, uploads them to `~/.cache/ferossh/paste/` on its host and types their paths at the cursor (Session > Coding agents > Paste and drop files: upload and type the path). Off, drops upload to the shell's folder after asking. |
| `PortPreviews yes` | no | A web server's address on the host in a pane's output (`http://localhost:5173/`) shows a bar offering to forward the port and open it in the browser (Session > Coding agents > Offer to forward and open web servers on the host). |
| `KittyKeyboard yes` | no | Programs may turn on the kitty keyboard protocol's first level (disambiguate escape codes) (Session > Coding agents > Kitty keyboard protocol for programs that ask for it). |
| `EscapeChar ~\|<char>\|^X\|none` | `~` | What starts an SSH escape sequence (`~.`, `~C`) first on a line; `none` turns them off. A host's `EscapeChar` in ssh_config wins. |
| `EscapeSequences no` | yes | Turns escape sequences off in every pane, whatever `EscapeChar` says, keeping the character (Session > SSH escape sequences). |
| `ShiftEnter enter\|esc-enter\|csi-u` | `enter` | What Shift+Enter sends to a program without the kitty keyboard protocol: as Enter, `ESC CR` (a new line in coding agents; Session > Coding agents > Shift+Enter types a new line), or `ESC [13;2u`. |
| `AgentAudit yes` | no | Coding agent panes are logged and recorded (sealed) by themselves from when they open, as an audit trail (Session > Coding agents > Log and record agent panes). |
| `AnswerFromNotification yes` | no | Clicking the notification of a coding agent stopped at a yes-or-no question answers it from a dialog, without switching tabs (Session > Coding agents > Answer yes-or-no questions from the notification). |
| `Standby yes` | no | A global hotkey hides the FeroSSH window and brings it back (Window > Standby window). The first window that opens takes the hotkey. |
| `StandbyHotkey Ctrl+Alt+Space` | `Win+Oem3` | The standby window's hotkey: Ctrl, Alt, Shift and Win joined by `+` to a key. Keys are letters, digits, `F1` to `F24`, `Space`, `Esc`, `Pause`, `ScrollLock`, `Num0` to `Num9`, the arrows and the like, and the punctuation keys by code (`Oem3` is the key below Esc on a US keyboard). |
| `StandbyWindow notepad.exe` | this FeroSSH window | The program whose window the standby hotkey shows and hides, by file name (Window > Standby window > Window to show and hide). |
| `StandbyHidden 263044` | | Written by FeroSSH while the standby hotkey has another program's window hidden, so that the window comes back at the next start if FeroSSH ended first. Leave it alone. |
| `TmuxKeybindings yes` | no | In panes of a tmux session, the tmux prefix and a window or pane key work the tabs and panes (Session > tmux > Use tmux keybindings for tabs and panes). |

## Scripts

| Keyword | Default | Meaning |
| --- | --- | --- |
| `ConfirmProduction no` | yes | Whether a script typed into a pane with the Production theme asks first, and starting a coding agent there or pasting or dropping into an agent's pane there (Session > Scripts > Confirm scripts and agents on Production hosts). |
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
| `Tags TAG...` | The connection's own tags (Tags in the connection form), separated by spaces: shown and searched in the connection manager and matched by theme rules, on top of ssh_config's `Tag`. Up to 16, each up to 32 letters, digits, `-`, `_`, `.`, `/` and `:`; others are left out. |
| `SystemInfo yes` | After each SSH login, reads the server's OS and kernel for [Tools > Hosts](everyday-use.md#hosts) (Read OS in the connection form's Extras). |
| `Credential NAME` | The named credential the connection uses (see [Credentials](#credentials)): its user, key file and password source win over the connection's own. |
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
| `Type ssh`, `mosh`, `sftp`, `tunnel`, `winrm`, `telnet`, `serial`, `rdp`, `vnc`, `xvfb` or `remoteapp` | The kind of connection; `mosh` logs in over SSH and runs the terminal over [Mosh](everyday-use.md#mosh), `sftp` logs in for [file transfer only](everyday-use.md#sftp-only-connections), `tunnel` for the [port forwards only](everyday-use.md#tunnel-only-connections), `winrm` opens PowerShell or cmd on a Windows server through [WinRM](everyday-use.md#winrm-powershell-on-windows-servers), `xvfb` is a virtual desktop and `remoteapp` a remote app, both on the `Via` host. |
| `MoshServer PROGRAM` | For a Mosh connection: the server program, when not `mosh-server` on the path, such as `~/bin/mosh-server`. |
| `MoshPorts 60001:60010` | For a Mosh connection: the UDP port, or range of ports, the server may listen on. Unset, any free one from 60001 to 60999. |
| `MoshPredict always` | For a Mosh connection: when typed text shows before the server echoes it: `adaptive` (only on a slow link, the default), `always` or `never`. |
| `WinrmCommand COMMAND` | For a WinRM connection: what its shell runs, when not `powershell -NoLogo`, such as `cmd`. |
| `WinrmHttps yes` | For a WinRM connection: use HTTPS (port 5986 unless `Port` is given) instead of HTTP with encrypted messages on 5985. |
| `WinrmNtlm yes` | For a WinRM connection over HTTP: let the login use NTLM where Kerberos cannot be used (a server named by address, outside your domain, or a local account). Without it an HTTP login is Kerberos only, because NTLM over plain HTTP does not prove who answers. Over HTTPS NTLM is always allowed. |
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

## Credentials

A `Credential` block is a named login that many connections share. A profile names it with `Credential NAME`. Whatever the block sets (user, key file, password source) wins over the connection's own; whatever it leaves out does not. Names use letters, digits, `.`, `_` and `-`. Tools > Credential manager edits these blocks.

```
Credential ops-admin
    User ops
    IdentityFile "C:\Keys\ops_ed25519"
    PasswordSource windows

Profile web1
    Credential ops-admin
```

`PasswordCommand ...` works here as in a profile.

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

Rules pick a theme, a font and a font size from the host's name or its tags: ssh_config's `Tag` and the profile's own `Tags`. Tools > Theme rules edits these blocks for you and puts them in order. A host's profile wins over the rules (except an enforced one), and the rules win over `--theme` and `DefaultTheme`. Each of theme, font and size comes from the first rule that sets it, so a rule may set only a font.

```
ThemeRule
    Hosts *.prod.example.com !prod-sandbox
    Theme Production
ThemeRule
    Tags qa
    Theme Nord
    Tint ffb000 40
ThemeRule
    Tags lab
    Font "JetBrains Mono"
    FontSize 11
```

| Keyword | Meaning |
| --- | --- |
| `Hosts PATTERN...` | ssh_config-style host patterns; `!` excludes. |
| `Tags TAG...` | Matches hosts with this tag, from ssh_config's `Tag` or the host's profile `Tags`. Patterns such as `prod*` work. |
| `Theme NAME` | The theme to use; optional when the rule sets a font. |
| `Font FAMILY` | The font family to use, unless the connection sets its own. |
| `FontSize POINTS` | The font size, 4 to 96 points, unless the connection sets its own. |
| `Tint RRGGBB AMOUNT` | Mixes a colour into the theme by 0 to 255, for example a red tint over your usual theme. |
| `Enforce yes` | What the rule sets cannot be changed: it beats the connection's own look (`Profile`), and Settings > Theme and Font, zoom, custom themes, workspaces and tmux layouts leave it alone on the connection's panes, the tabs and panes opened from them and its tmux windows. The first enforced rule that sets each of theme, font and size wins. |

Hosts tagged `production` or `prod`, `staging` or `stage`, and `development` or `dev` always get Production, Staging or Nord. These built-in rules are enforced and come before yours, so no `ThemeRule`, `Profile` theme or pane setting overrides them; your rules can still set their font and size.

## Security rules

A `SecurityRule` puts limits on connections by tag, folder or host name, whatever each connection's own settings say. Tools > Security rules edits these blocks for you. A rule can only tighten. Every rule that matches adds its limits, and none takes one away, so a rule such as `ForwardAgent yes` does nothing.

```
SecurityRule
    Tags production
    Folders Customers
    KeysOnly yes
    SavePassword no
    ForwardAgent no
    Clipboard no
    ConfirmConnect yes
    LogSession yes
```

| Keyword | Meaning |
| --- | --- |
| `Hosts PATTERN...` | ssh_config-style patterns, matched against the connection's name and the machine it reaches. |
| `Tags TAG...` | Connections with this tag, from ssh_config's `Tag` or the profile's `Tags`. Patterns such as `prod*` work, in any case. |
| `Folders FOLDER...` | Connections in this connection manager folder, or in any folder inside it. Patterns work. |
| `KeysOnly yes` | SSH logins use a key, the agent or a security key. A password prompt ends the login, also inside a keyboard-interactive round. A one-time code is still asked, also one labelled "one-time password". Kerberos (GSSAPI) is not used, and the connection (or one through a jump host under this rule) is never shared with another pane's login. WinRM, which only has passwords, is not affected. |
| `SavePassword no` | No password is read from or kept in the Windows Credential Manager, for the login, a proxy (also when only a jump host on the way has the rule) or a login script, including its named secrets. A password manager command still works. |
| `ForwardAgent no` | No agent forwarding, even when the connection or ssh_config asks for it. |
| `Clipboard no` | The server cannot set this PC's clipboard: no OSC 52 in the terminal, and no text from the host in a Remote Desktop, VNC or virtual desktop viewer or a remote app, whatever the connection's or the menu's clipboard setting says (text can still go to the host where those allow it). A Remote Desktop Connection window shares the clipboard both ways or not at all, so it gets none. |
| `ConfirmConnect yes` | FeroSSH asks before connecting, also when only a jump host on the way matches. The question names the user, host and port (a saved Telnet or WinRM connection's own port) and the route there, and if any of that changes before the connection starts, it asks again. A reconnect or a split of a connected pane does not ask again. A direct Remote Desktop connection (one without Via SSH) asks in a message box before Remote Desktop Connection starts. A Remote Desktop or VNC connection through Via SSH asks in a message box once that SSH connection has logged in, before the tunnel to the desktop opens. |
| `LogSession yes` | The session is logged, and Session > Log this session cannot turn it off. If the log cannot be opened, nothing starts on the session (no login script, forwards or tmux) and it disconnects; if a write fails later, it disconnects too. Once the connection ends or you disconnect, Log this session goes back to how you had it. |

A rule needs at least one of `Hosts`, `Tags` or `Folders`, and at least one limit. `Tag`, `Host` and `Folder` are read as `Tags`, `Hosts` and `Folders`. A connection shows the limits that apply when it starts.

Rules follow the machine to addresses you did not save. An address typed in Connect to... or an `ssh://` link gets the limits of every connection in the connection manager that reaches the same host name: typing `db1`'s address gets `db1`'s tag and folder rules, and the line it shows names `db1`. A connection listed in the connection manager (saved in FeroSSH or in ssh_config) keeps to its own tags and folder, so tagging one connection to a host leaves the others to that host as they are; tag each one, or put the host in a rule's `Hosts`, to cover them all. Host names are compared as written, in any case, so an IP address and a DNS name for the same machine are not matched (put both in a rule's `Hosts` for that). `localhost` and loopback addresses never count as one machine.

FeroSSH remembers the limits each connection had when it last connected, in `limits.conf` in its folder for this PC. When a connection would get fewer (a tag, folder or name changed, a rule was removed, or the team library was not loaded), it asks before connecting, and a Yes is noted in `limit-changes.log` there. See [When a connection loses limits](everyday-use.md#when-a-connection-loses-limits). Desktops opened directly or through Via SSH and remote apps are not compared yet.

A line of a rule that FeroSSH does not understand (a keyword from a newer FeroSSH, a misspelling, or a value other than `yes` or `no`, such as `KeysOnly 1`) is never dropped: it stays in the file, and FeroSSH writes it back unchanged. It is not applied, though. Tools > Security rules marks the rule with ⚠ and names the line, and a connection that the rule covers says so in its pane when it starts. The limits cover SSH, Mosh, SFTP and tunnel connections, and the clipboard, confirmation and log limits cover Telnet and WinRM too.

## Team library

`TeamLibrary` names a file an administrator exports with File > Import and export > Export team library and signs with an SSH key:

```
ssh-keygen -Y sign -f admin_key -n ferossh-library team.ferossh-library
```

`ssh-keygen` writes `team.ferossh-library.sig` beside it, and both go on the share. FeroSSH reads the library at start (in the background) and on Refresh in the connection manager, and uses it only when the signature is by a `TeamLibraryKey` and made for `ferossh-library`. A library that holds saved passwords is refused, and a connection's password manager command (`PasswordCommand`) is left out of an export and ignored in a library, since it could hold a password itself. A good library is copied to a `team-library-….cache` file in FeroSSH's folder (one per library path); when the share cannot be read, or serves a file with a bad signature, FeroSSH says so and uses that copy, checked again against the keys set now. Each export is dated, and the date is signed with it: FeroSSH does not use a library older than the copy it checked last, so an earlier signed library put back on the share (one from before a security rule was added, say) is refused with a note and the newer copy stays in use. FeroSSH also notes, per signing key, the date of the newest library it used (`team-library-signers` in its folder), and refuses an older library signed with that key at any path. So setting Library file to an older file, or one put at a new path, does not bring back an earlier library either: FeroSSH says so and has no team library until a newer one is there. An administrator who signs libraries for several teams should use a key per team. A library exported before FeroSSH dated them counts as older than any dated one.

What the library gives comes before your own settings:

- Its connections show in the **Team** folder (their own folders below it). Their address, user, port, jump hosts and the rest come from the library first, and a connection of your own with the same name is not used. Edit shows them read-only; saving one under a new name makes a copy of your own. They cannot be deleted, moved or retagged here.
- Their profiles (tags, type, look, credential name) are the library's: a local `Profile` of the same name is not used.
- Its `SecurityRule` blocks apply together with yours, and since rules only add limits, yours cannot loosen them. Tools > Security rules lists them first as "team 1", "team 2" and so on. They cannot be changed or deleted there, but Edit (or a double click) shows everything one says: its hosts, tags and folders, every limit and every connection it covers.
- Its `ThemeRule` blocks are tried before yours. Tools > Theme rules lists them first, under "Team library rules"; Edit shows one read-only.
- The custom themes its theme rules and connections name come with it, so a PC without that `.theme` file still gets the team's look. They show in Settings > Custom themes and under "Team library themes" in Tools > Theme manager. A theme file of your own with the same name is not used (the Custom themes menu says so); the team's theme is.

A key file travels only when it is in the user's home folder (`IdentityFile ~/.ssh/id_team`, each person's own file); a full path such as `C:\Users\admin\.ssh\id_ed25519` is left out of an export and ignored in a library, since it names the exporting PC's file. The team's security and theme rules are not limited to the team's connections: like your own rules, they match every connection by host, tag and folder, your own connections included. A library connection may name a credential. A team security rule for folder `Production` also covers the team's `Team/Production`. No credential travels in the library: each person keeps their own credential of that name, with their own user, key and password. Options in your own `~/.ssh/config` that the library does not set for a connection (a `Host *` block, say) still apply to it, and the security rules still limit it.

On PCs an organisation manages, an administrator can set the library by machine policy instead (Group Policy or Intune, `HKLM\Software\Policies\FeroSSH`). The policy's library and keys then replace `TeamLibrary` and `TeamLibraryKey`, Settings > Team library only shows them, and FeroSSH connects nowhere while it has no good library. See [Enforcing the team library by machine policy](../admin/team-library-policy.md).

