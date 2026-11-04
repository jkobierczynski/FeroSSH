# Menu reference

Every item in FeroSSH's menus, in the order they appear, with its shortcut and what it does. The menu bar has seven menus: File, Edit, Settings, Tools, Window, Session and Help. Ctrl+Shift+M hides the menu bar; Alt shows it again. Every item here is also in the command palette (Ctrl+Shift+A), so you can type part of its name instead.

Items with a check mark are switches: picking them turns the setting on or off, and FeroSSH remembers it in `ferossh.conf` (see [Settings](settings.md)).

## File

| Item | Shortcut | What it does |
| --- | --- | --- |
| Connect to... | Ctrl+Shift+N | Asks for a host in a new tab. The prompt takes `user@host`, `user@host:2222`, a `Host` alias from your ssh_config, a saved connection's name, `telnet://host[:port]` or `telnet host port`, and a COM port such as `COM3` or `COM3:115200`. |
| Reconnect | Ctrl+Shift+R | Logs in again on the focused pane's connection. While FeroSSH is already waiting to reconnect, it tries at once instead. |
| Disconnect | | Ends the focused pane's connection, or stops the retries while it is reconnecting. |
| Reconnect automatically | ✓ on | Logs in again by itself when a connection drops (see [When the connection drops](everyday-use.md#when-the-connection-drops)). `AutoReconnect`. |
| Close pane | Ctrl+Shift+W | Closes the focused pane and ends its session. In tmux it closes the tmux pane. |
| Upload files... | Ctrl+Shift+U | Picks files on your PC and uploads them over SFTP to the folder the file view shows, or the shell's current folder. |
| Open download folder | | Opens the folder downloads go to in Explorer. |
| Open local shell | | Opens a Windows command prompt (`%COMSPEC%`, usually `cmd.exe`) in a new tab. To start another program, use `ferossh.exe --local wsl -d Ubuntu` and the like. |
| Import connections... | | Brings saved sessions over from PuTTY, KiTTY, WinSCP, MobaXterm, mRemoteNG and SecureCRT (see [Imports](everyday-use.md#importing-from-other-clients)). |
| Exit | Alt+F4 | Closes FeroSSH. tmux sessions keep running on their servers. |

## Edit

| Item | Shortcut | What it does |
| --- | --- | --- |
| Copy | Ctrl+Shift+C (or Ctrl+Insert) | Copies the selection. |
| Paste | Ctrl+Shift+V (or Shift+Insert) | Pastes. Control characters other than Tab are removed, and a paste with line breaks asks first unless the program has bracketed paste on. |
| Find in scrollback | Ctrl+Shift+F | Opens the search bar. Enter finds the older match, Shift+Enter the newer one, Esc closes. |
| Clear scrollback | | Forgets the pane's scrollback. The screen stays. |
| Save selection as snippet... | | Opens the snippet editor with the selected text as the snippet's lines. |
| Record keystrokes as snippet | ✓ while recording | Records what you type in the focused pane until you pick it again, then opens the result in the snippet editor (see [Recording](everyday-use.md#recording-a-snippet)). |
| Copy on select | ✓ off | Copies a selection as soon as you make it, as PuTTY does. `CopyOnSelect`. |
| Right-click pastes | ✓ off | A right click pastes, as in PuTTY. `RightClickPaste`. |

## Settings

| Item | Shortcut | What it does |
| --- | --- | --- |
| File transfer follows cd | ✓ on | The file view, and files dropped on a pane, follow the shell's current directory. `FollowDirectory`. |
| Side pane on the right | ✓ off | Moves the side pane from the left to the right. `SidePane`. |
| Tab strip | ✓ on | Shows the tab strip once there are two tabs. `TabStrip`. |
| Larger text | Ctrl+= (or Ctrl+wheel up) | Makes the focused pane's text bigger. |
| Smaller text | Ctrl+- (or Ctrl+wheel down) | Makes it smaller. |
| Reset text size | Ctrl+0 | Back to the pane's normal size. |
| Font (this pane) | | Eighteen monospace fonts for the focused pane. The five that come with Windows are listed first. A free font that is not installed reads "(download)" and opens its official download page; FeroSSH picks it up after a restart. |
| Theme (this pane) | | The colour theme for the focused pane, in three columns: Nord and 24 other dark themes, 11 light themes, then the Production and Staging warning themes over the classics (Campbell, One Half Dark and Light, Tango Dark and Light from Windows Terminal, and PuTTY's colours). The list is in [Everyday use](everyday-use.md). |
| Show test pattern | | Prints colours, text styles, box drawing, wide and combining characters in the pane, to check a font. |
| Hide menu bar (Alt shows it) | Ctrl+Shift+M | Hides the menu bar. Pressing Alt shows it again. |

Font, theme and text size apply to the focused pane only. Session > Save font and theme for this host keeps them for that host.

## Tools

| Item | Shortcut | What it does |
| --- | --- | --- |
| Connection manager | Ctrl+Shift+S | Shows or hides the side pane with your saved hosts (see [The connection manager](everyday-use.md#the-connection-manager)). |
| Session manager | Ctrl+Shift+L | Shows the sessions open right now, in every tab (see [The session manager](everyday-use.md#the-session-manager)). |
| File transfer | Ctrl+Shift+E | Shows the focused pane's host over SFTP in the side pane (see [File transfer](everyday-use.md#file-transfer)). |
| Port forwards | Ctrl+Shift+P | Lists, adds, changes and stops port forwards on live connections (see [Port forwards](everyday-use.md#port-forwards)). |
| Command snippets | Ctrl+Shift+Space | Lists your snippets, to type one into the pane or edit them (see [Snippets](everyday-use.md#command-snippets)). |
| Command palette | Ctrl+Shift+A | One search box over every menu item, connection and snippet (see [Command palette](everyday-use.md#the-command-palette)). |

## Window

| Item | Shortcut | What it does |
| --- | --- | --- |
| New tab | Ctrl+Shift+T | A new tab on the focused pane's host, as a new channel on the same connection, so there is no second login. |
| Split right | Alt+Shift+= | Splits the focused pane, the new one on the right, on the same connection. |
| Split down | Alt+Shift+- | The same, the new pane below. |
| Split right in same directory | | Splits and starts the new shell in the directory the focused pane's shell is in (see [Splits in the same directory](everyday-use.md#splits-in-the-same-directory)). |
| Split down in same directory | | The same, below. |
| Move pane to its own tab | | Takes the focused pane out of its tab into a new one. |
| Join tab into the tab before it | Ctrl+Shift+J | Puts this tab's panes into the tab to its left. |
| Broadcast input to this tab's panes | Ctrl+Shift+B | What you type and paste goes to every pane of the tab (see [Broadcast input](everyday-use.md#broadcast-input)). |
| Broadcast input to all tabs | | The same, to every pane in every tab. |
| Leave this pane out of broadcast | | Keeps the focused pane to itself while broadcast is on. |
| Next tab | Ctrl+Tab | |
| Previous tab | Ctrl+Shift+Tab | |
| Move tab left | Ctrl+Shift+PgUp | |
| Move tab right | Ctrl+Shift+PgDn | |
| Focus pane left, right, up, down | Alt+arrows | Moves the focus to the pane in that direction. |
| Grow pane left, right, up, down | Alt+Shift+arrows | Moves the focused pane's edge that way. |
| Close pane | Ctrl+Shift+W | |
| Close tab | Ctrl+F4 | Closes every pane in the tab, after asking when it has more than one open SSH session. |

## Session

| Item | What it does |
| --- | --- |
| Log to file | Writes what the focused tab receives to a log file. ✓ while logging. See [Logs](logs.md). |
| Log raw output (keep escape codes) | Logs every byte as received, so the log replays with `cat`. ✓ |
| Timestamp each log line | Puts the time each line started in front of it, in plain logs. ✓ `LogTimestamps`. |
| Save this pane's output to a log | Writes everything the pane still holds, scrollback and screen, to a new log file. |
| Save output of all panes to logs | The same for every pane in every tab, one file each. |
| Open log folder | Opens `%LOCALAPPDATA%\FeroSSH\logs` in Explorer. |
| Check a log for changes... | Picks a log and says whether it is intact, or where it was changed, cut short or added to. |
| Save font and theme for this host | Keeps the focused pane's font, text size and theme in the host's profile. |
| Start new tabs and panes in this directory | Keeps the focused pane's directory as the host's start directory (Start dir in the connection form): new tabs, panes and tmux windows on that host start there, also after a restart. Window > Split … in same directory still follows its pane. |
| Replace host key... | After a host key change you know the reason for, trusts the new key. It asks twice. |
| Verify transfers (SHA-256) | Compares SHA-256 sums on both ends after each SFTP transfer (the server needs `sha256sum`). ✓ `VerifyTransfers`. |
| ZMODEM: answer rz and sz | Answers `rz` and `sz` in the terminal (see [ZMODEM](everyday-use.md#zmodem)). ✓ `ZModem`. |
| Attach tmux | Types `tmux -CC new-session -A -s main` in the pane and turns tmux windows and panes into native tabs and panes (see [tmux](everyday-use.md#tmux)). |
| Switch tmux session... | Lists the other tmux sessions on the server and switches to one. |
| Detach tmux | Leaves tmux, and everything running in it, on the server. Attach again later from this PC or another. |
| tmux: one window per pane (own fonts) | ✓ on: each native pane is its own tmux window, so each can have its own font. Off: tmux's own splits are the panes. |
| Use tmux keybindings for tabs and panes | ✓ on: in a pane of a tmux session, your tmux prefix then `c`, `%`, `"`, `n`, `p`, an arrow and the other window and pane keys work the tabs and panes; the prefix then `?` lists them (see [tmux](everyday-use.md#tmux)). Off by default. |
| Send break (serial) | Holds a serial line in break for 300 ms (Cisco ROM monitor, Linux SysRq). |
| Run login script again | Runs the connection's login script from the start. |
| Auto-responses from snippets | ✓ on: snippets with a *When* text answer by themselves. Off turns all of them off. `AutoResponses`. |
| Confirm snippets on Production hosts | ✓ on: a snippet about to be typed into a pane with the Production theme asks first. `ConfirmProduction`. |
| Open remote desktop | Ctrl+Shift+D. Tunnels the host's RDP port through SSH and opens Windows' Remote Desktop Connection on it (see [Remote desktops](everyday-use.md#remote-desktops)). |
| Open remote desktop in a tab (RDP) | Only in `ferossh-desktop.exe`: the desktop in a FeroSSH tab. |
| Open remote desktop in a tab (VNC) | Only in `ferossh-desktop.exe`: a VNC desktop in a FeroSSH tab. |
| Open virtual desktop (Xvfb) | Starts Xvfb and x11vnc on the server and shows that headless desktop through SSH: in a tab in `ferossh-desktop.exe`, else in the VNC viewer on this PC (see [Virtual desktops](everyday-use.md#virtual-desktops-xvfb)). Again while one runs: another viewer on it. |
| Stop virtual desktop | Stops the virtual desktop on this connection, and everything running in it. |

## Help

| Item | What it does |
| --- | --- |
| Check for updates... | Asks GitHub whether a newer release exists, checks the answer's signature, and offers to open its page. |
| Check for updates weekly | ✓ off: does the same once a week, quietly unless there is a new release. `UpdateCheck weekly`. |
| About FeroSSH | Shows the version and licence. |

## Right-click menu in the connection manager

Right-clicking a host in the connection manager offers Connect (Enter), Open in a split (Shift+Enter), Edit connection... (F2), Delete connection... (Del) and New connection... (Ins). On an empty spot it offers only New connection.

## Keys in the side panes

The side pane shows one view at a time. Esc goes back to the terminal; the shortcut that opened a view closes it again.

### Connection manager (Ctrl+Shift+S)

| Key | Action |
| --- | --- |
| Typing | Filters the list. With nothing left, Enter connects to what you typed. |
| Enter / Shift+Enter | Connects in this tab / in a split. On a folder heading, opens or closes it. |
| Left / Right | Closes / opens a folder. Left on a host goes to its folder. |
| Ctrl+D | Marks or unmarks a favourite. |
| Insert | New connection. |
| F2 | Edits the selected connection. |
| Del | Deletes the selected saved connection, after asking. |
| Backspace / Esc | Edits the filter / clears it, then leaves. |

### Connection form

| Key | Action |
| --- | --- |
| Tab, Up, Down | Moves between fields. |
| Left / Right | Picks the next choice in a choice field (type, password source, theme...), or opens and closes a section. |
| Enter | Saves. On a section heading (Look, Extras), opens or closes it. |
| Shift+Enter | Saves and connects. |
| Esc | Cancels. |

The buttons at the bottom do the same, and a saved connection has a Delete button that asks first.

### Session manager (Ctrl+Shift+L)

| Key | Action |
| --- | --- |
| Typing | Filters by host, title or kind (SSH, Telnet, serial, tmux, desktop, local). |
| Enter or double-click | Goes to the session's tab and pane. A hidden snippet-run session is shown in a tab. |
| Del | Closes the session, after asking when it is live. |
| Esc | Clears the filter, then leaves. |

### File transfer (Ctrl+Shift+E)

In the remote listing:

| Key | Action |
| --- | --- |
| Enter or Right | Opens a folder or downloads a file. |
| Left or Backspace | Goes to the parent folder. |
| D | Downloads the selection (folders whole). |
| U | Uploads files into the listed folder. |
| Del | Deletes on the server, after asking. |
| ~ or H | Goes to your home folder. |
| F5 | Refreshes. |
| F | Turns following the shell's `cd` off or on. |
| O | Opens the download folder in Explorer. |
| B | Picks another local folder for downloads. |
| / | Searches the listing by name (see below). |
| Escape | Clears the search; with none, gives the keyboard back to the terminal. |
| Tab | Moves to the Local PC section. |

While typing a search, letters go into it and the listing shows only the names that contain them, in any case; with `*` or `?` the whole name must match the pattern (`*.log`). Up and Down still move the selection, Enter stops typing and keeps the search, Backspace deletes a letter and Escape clears it. Opening another folder ends the search.

In the Local PC section, Enter opens a folder or uploads a file, Left or Backspace goes up (above a drive comes the list of drives), U uploads the selected file or folder, / searches it by name as above, B picks another folder and F5 refreshes. Tab moves on to the transfer queue, where Space or P pauses and resumes, R retries a failed or cancelled transfer, X cancels and C clears the finished ones. When a file already exists, O overwrites, S skips and K keeps both (Shift for all).

Drag the title bar of the Local PC section or of the transfer queue up or down to give that section more or fewer rows. The heights stay for every host until FeroSSH closes.

### Port forwards (Ctrl+Shift+P)

| Key | Action |
| --- | --- |
| Typing, then Enter | Adds a forward: `-L 8080:db.internal:5432`, `-R 9000:localhost:3000`, `-D 1080`, or stops one with `-KL 8080`, `-KR 9000`, `-KD 1080`. |
| Up / Down | Selects a forward. New forwards go to the connection you clicked in the list, or else to the focused pane's. |
| Enter or F2 on a forward | Puts it on the add line, to change its port or target, or to retry it. |
| Del | Stops the selected forward. |
| Esc | Clears the add line, then leaves. |

### Command snippets (Ctrl+Shift+Space)

| Key | Action |
| --- | --- |
| Typing | Filters. |
| Enter | Types the snippet, with Enter after each line. |
| Shift+Enter | Types it but leaves the last line for you to finish. |
| Insert | New snippet (the filter text becomes its name). |
| F2 | Edits the snippet. |
| Del | Deletes it, after asking. |
| F5 | Runs it on several saved hosts (see [Runs on many hosts](everyday-use.md#runs-on-many-hosts)). |
| Ctrl+Shift+F1 to F12 | From anywhere: runs the snippet with that hotkey for the focused pane's host. |

In a run's report, Enter shows that host's pane, C copies the report as a table, S saves it as CSV in the download folder, R runs the failed hosts again and Esc stops the run (or closes the report when it is over).

### Command palette (Ctrl+Shift+A)

Type a few letters in any order. Up and Down select, Enter runs the action, connects or types the snippet, Shift+Enter connects in a split or types a snippet without its last Enter, Esc closes.

### Import connections

Up and Down move, Space or Enter ticks a session (or opens a client), Esc goes back.
