# Menu reference

Every item in FeroSSH's menus, in the order they appear, with its default shortcut and what it does. The menu bar has seven menus: File, Edit, Settings, Tools, Window, Session and Help. Ctrl+Shift+M hides the menu bar; Alt shows it again. Every item here is also in the command palette (Ctrl+Shift+A), so you can type part of its name instead.

Settings > Key bindings changes the shortcuts, and gives one to any item that has none. The menus, the command palette and the tools rail's tooltips always show the keys as they are now; this page lists the defaults.

Items with a check mark are switches: picking them turns the setting on or off, and FeroSSH remembers it in `ferossh.conf` (see [Settings](settings.md)).

## File

| Item | Shortcut | What it does |
| --- | --- | --- |
| Connect to... | Ctrl+Shift+N | Asks for a host in a new tab. The prompt takes `user@host`, `user@host:2222`, a `Host` alias from your ssh_config, a saved connection's name, `sftp://user@host[:port][/folder]` (file transfer only, no shell), `telnet://host[:port]` or `telnet host port`, `winrm://[user@]host[:port]`, `winrms://[user@]host[:port]` (HTTPS) or `winrm host`, and a COM port such as `COM3` or `COM3:115200`. |
| Reconnect | Ctrl+Shift+R | Logs in again on the focused pane's connection. While FeroSSH is already waiting to reconnect, it tries at once instead. |
| Disconnect | | Ends the focused pane's connection, or stops the retries while it is reconnecting. |
| Reconnect automatically | ✓ on | Logs in again by itself when a connection drops (see [When the connection drops](everyday-use.md#when-the-connection-drops)). `AutoReconnect`. |
| Close pane | Ctrl+Shift+W | Closes the focused pane and ends its session. In tmux it closes the tmux pane. |
| Upload files... | Ctrl+Shift+U | Picks files on your PC and uploads them over SFTP to the folder the file view shows, or the shell's current folder. |
| Open download folder | | Opens the folder downloads go to in Explorer. |
| Open local shell | | Opens a Windows command prompt (`%COMSPEC%`, usually `cmd.exe`) in a new tab. To start another program, use `ferossh.exe --local wsl -d Ubuntu` and the like. |
| Import connections... | | Brings saved sessions over from PuTTY, KiTTY, WinSCP, MobaXterm, mRemoteNG and SecureCRT (see [Imports](everyday-use.md#importing-from-other-clients)). |
| Import scripts... | | Brings scripts over from Windows Terminal (`sendInput` actions and `.wt.json` scripts), SecureCRT button bars and MobaXterm macros, or makes one from a text file (see [Importing scripts](everyday-use.md#importing-scripts)). |
| Export connections... | | Writes your saved and imported connections and their settings to a `.ferossh` file encrypted with a passphrase, with the saved passwords only if you tick it (see [Backup and sync](everyday-use.md#backup-and-sync-between-pcs)). |
| Import encrypted connections... | | Opens such a file, asks its passphrase, says what it would add and replace, and imports it. Your other connections stay. |
| Master password > Set a master password... | | Moves your connections, settings and (if ticked) saved passwords into an encrypted vault, `connections.vault`. FeroSSH then asks for the master password when it starts. |
| Master password > Use a vault file... | | Uses a vault another PC made, in a shared folder such as OneDrive, and adds this PC's connections to it. |
| Master password > Move the vault file... | | Moves the vault, for example into a OneDrive folder to share it between PCs. |
| Master password > Change master password... | | Asks for the current master password, then a new one. |
| Master password > Remove master password... | | Asks for the current master password, then writes the connections and settings back as plain files and the passwords back to the Windows Credential Manager. A portable FeroSSH drops the saved passwords instead (export them first to keep them). A vault in a shared folder stays there for the other PCs. |
| Exit | Alt+F4 | Closes FeroSSH. tmux sessions keep running on their servers. |

## Edit

| Item | Shortcut | What it does |
| --- | --- | --- |
| Copy | Ctrl+Shift+C (or Ctrl+Insert) | Copies the selection. |
| Paste | Ctrl+Shift+V (or Shift+Insert) | Pastes. Control characters other than Tab are removed, and a paste with line breaks asks first unless the program has bracketed paste on. |
| Find in scrollback | Ctrl+Shift+F | Opens the search bar. Enter finds the older match, Shift+Enter the newer one, Esc closes. The match is shown on the middle row of the pane where the scrollback allows. |
| Stop following at a match... | | Opens a bar for a regular expression: Enter sets it, and when new output matches, the view holds still on that line until you go back to the bottom. An empty pattern stops watching (see [Highlighting and following output](everyday-use.md#highlighting-and-following-output)). |
| Follow new output | Ctrl+Shift+End (or Shift+End while scrolled back or stopped) | Goes back to the bottom and follows the output again. End or any key typed does too. |
| Clear scrollback | | Forgets the pane's scrollback. The screen stays. |
| Save selection as script... | | Opens the script editor with the selected text as the script's lines. |
| Record keystrokes as script | ✓ while recording | Records what you type in the focused pane until you pick it again, then opens the result in the script editor (see [Recording](everyday-use.md#recording-a-script)). |
| Copy on select | ✓ off | Copies a selection as soon as you make it, as PuTTY does. `CopyOnSelect`. |
| Right-click pastes | ✓ off | A right click pastes, as in PuTTY. `RightClickPaste`. |
| Suggest commands as you type | ✓ off | Shows an earlier command on this host (or a common one) greyed after the cursor; Right arrow or End types it (see [Command suggestions](everyday-use.md#command-suggestions)). `CommandSuggestions`. |
| Clear command history for this host | | Forgets the commands kept for the focused pane's host. |
| Clear command history for all hosts... | | Asks, then forgets the commands kept for every host. |

## Settings

| Item | Shortcut | What it does |
| --- | --- | --- |
| File transfer follows cd | ✓ on | The file view, and files dropped on a pane, follow the shell's current directory. `FollowDirectory`. |
| Side pane on the right | ✓ off | Moves the side pane from the left to the right. `SidePane`. |
| Tools rail | ✓ on | Shows a narrow bar of tool icons on the window's edge. A click opens or closes that tool; hover for its name. `ToolsRail`. |
| Keep tools open | ✓ on | Off, an opened tool is a peek: clicking in the terminal, Esc, or connecting covers it again. Also the pin at the bottom of the rail. `ToolsPinned`. |
| Tab strip | ✓ on | Shows the tab strip once there are two tabs. `TabStrip`. |
| Server status bar | ✓ off | A row under the panes with the CPU, memory, root disk and network use of the server the focused pane is logged in to, sampled every 2 seconds on a channel of its own. `StatusBar`. |
| Larger text | Ctrl+= (or Ctrl+wheel up) | Makes the focused pane's text bigger. |
| Smaller text | Ctrl+- (or Ctrl+wheel down) | Makes it smaller. |
| Reset text size | Ctrl+0 | Back to the pane's normal size. |
| Font (this pane) | | Eighteen monospace fonts for the focused pane. The five that come with Windows are listed first. A free font that is not installed reads "(download)" and opens its official download page; FeroSSH picks it up after a restart. |
| Theme (this pane) | | The colour theme for the focused pane, in three columns: Nord and 24 other dark themes, 11 light themes, then the Production and Staging warning themes over the classics (Campbell, One Half Dark and Light, Tango Dark and Light from Windows Terminal, and PuTTY's colours). The list is in [Everyday use](everyday-use.md). |
| Window colours (menus, tools) | | Where the menu bar and menus, the title bar, the tab strip, the tools rail and the tools pane take their colours. **Follow the terminal theme**: the focused pane's theme, so a dark theme gives a dark window and Production a red one. **Follow Windows**: Windows' light or dark app mode. **Light** (the default): Windows' usual light look. **Dark**: a neutral dark grey. `WindowColours`. |
| Show test pattern | | Prints colours, text styles, box drawing, wide and combining characters in the pane, to check a font. |
| Hide menu bar (Alt shows it) | Ctrl+Shift+M | Hides the menu bar. Pressing Alt shows it again. |
| Full screen | F11 | ✓ while on. The window drops its title bar, frame and menu bar and covers the whole monitor, taskbar too. F11 again (or this item) puts it back as it was, maximized or not. Alt still shows the menu bar; it hides again once a menu closes. |
| F11 switches full screen | ✓ on | Off, F11 goes to the terminal like the other function keys, for programs that use it; Full screen stays in this menu and the command palette. With another key for Full screen (Key bindings), the item names that key. `FullScreenKey`. |
| Key bindings... | | A window listing every action of the menus with its shortcuts and its default ones: change them, add a second key, take a key away, or reset one action or all of them. See [Key bindings](#key-bindings). `KeyBinding`. |

Font, theme and text size apply to the focused pane only. Session > Save font and theme for this host keeps them for that host.

## Tools

| Item | Shortcut | What it does |
| --- | --- | --- |
| Connection manager | Ctrl+Shift+S | Shows or hides the side pane with your saved hosts (see [The connection manager](everyday-use.md#the-connection-manager)). |
| Session manager | Ctrl+Shift+L | Shows the sessions open right now, in every tab (see [The session manager](everyday-use.md#the-session-manager)). |
| File transfer | Ctrl+Shift+E | Shows the focused pane's host over SFTP in the side pane (see [File transfer](everyday-use.md#file-transfer)). |
| Port forwards | Ctrl+Shift+P | Lists, adds, changes and stops port forwards on live connections (see [Port forwards](everyday-use.md#port-forwards)). |
| Scripts | Ctrl+Shift+Space | Lists your scripts by category, to type one into the pane or edit them (see [Scripts](everyday-use.md#scripts)). |
| Open scripts folder | | Opens `%APPDATA%\FeroSSH\scripts` in Explorer: one file per script, a folder per category, to copy to another PC or back up (see [The scripts folder](everyday-use.md#the-scripts-folder)). |
| Command palette | Ctrl+Shift+A | One search box over every menu item, connection and script (see [Command palette](everyday-use.md#the-command-palette)). |
| New SSH key... | | Makes an Ed25519 key file (with or without a passphrase) or a security key or Windows Hello key, and offers to copy it to the focused pane's host (see [Making a key](everyday-use.md#making-a-key-and-putting-it-on-a-server)). |

## Window

| Item | Shortcut | What it does |
| --- | --- | --- |
| New tab | Ctrl+Shift+T | A new tab on the focused pane's host, as a new channel on the same connection, so there is no second login. |
| Workspaces > Save layout as workspace... | | Saves the open tabs and split panes, each with its connection, directory, session name and own look, as a workspace under the name you give. See [Workspaces](everyday-use.md#workspaces). |
| Workspaces > Open workspace file... | | Opens a workspace file from any folder as new tabs. |
| Workspaces > Open workspaces folder | | Opens `%APPDATA%\FeroSSH\workspaces` in Explorer. |
| Workspaces > (a workspace) | | Opens that workspace's tabs next to the open ones; a window that only waits at the connect prompt makes way for them. |
| Standby window (hotkey) > Status | | Says whether the standby hotkey is active, and for which window: on, off, or turned on but taken by another program. Click it for the details. See [Standby window](everyday-use.md#standby-window). |
| Standby window (hotkey) > Standby hotkey on | | Turns the standby hotkey on, or off. ✓ while this FeroSSH window holds the hotkey. Turning it off shows a hidden window again. `Standby`. Off by default. |
| Standby window (hotkey) > Window to show and hide | | This FeroSSH window, or one of the open windows of other programs (listed with their program, such as `notepad.exe`). Picking one turns standby on. FeroSSH remembers the program, so after a restart it uses that program's front window. `StandbyWindow`. |
| Standby window (hotkey) > Press a new hotkey... | | After OK, the next key you press with Ctrl, Alt or Win becomes the hotkey, and standby turns on. Esc cancels. `StandbyHotkey`. |
| Standby window (hotkey) > Win+` … Pause | | Picks one of five hotkeys and turns standby on: Win or Ctrl with the key below Esc (shown as your keyboard prints it), Ctrl+Alt+Space, Scroll Lock or Pause. The one in use is marked. |
| Split right | Alt+Shift+= | Splits the focused pane, the new one on the right, on the same connection and in the same directory (see [New tabs and panes start in the same directory](everyday-use.md#new-tabs-and-panes-start-in-the-same-directory)). |
| Split down | Alt+Shift+- | The same, the new pane below. |
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
| Start new tabs and panes in this directory | Keeps the focused pane's directory as the host's start directory (Start dir in the connection form): new tabs, panes and tmux windows on that host start there when the directory of the pane they come from is not known, also after a restart. |
| Replace host key... | After a host key change you know the reason for, trusts the new key. It asks twice. |
| Copy public key to this server... | Like `ssh-copy-id`: adds a `.pub` file's key to `~/.ssh/authorized_keys` on the focused pane's host, unless it is there already (see [Making a key](everyday-use.md#making-a-key-and-putting-it-on-a-server)). |
| Verify transfers (SHA-256) | Compares SHA-256 sums on both ends after each SFTP transfer (the server needs `sha256sum` and `xargs`). ✓ `VerifyTransfers`. |
| ZMODEM: answer rz and sz | Answers `rz` and `sz` in the terminal (see [ZMODEM](everyday-use.md#zmodem)). ✓ `ZModem`. |
| Highlight keywords | Colours errors, warnings, success words and addresses in the output, and your own `HighlightRule`s (see [Highlighting and following output](everyday-use.md#highlighting-and-following-output)). ✓ on. `Highlight`. |
| Attach tmux | Types `tmux -CC new-session -A -s main` in the pane and turns tmux windows and panes into native tabs and panes (see [tmux](everyday-use.md#tmux)). |
| Switch tmux session... | Lists the other tmux sessions on the server and switches to one. |
| Detach tmux | Leaves tmux, and everything running in it, on the server. Attach again later from this PC or another. |
| tmux: one window per pane (own fonts) | ✓ on: each native pane is its own tmux window, so each can have its own font. Off: tmux's own splits are the panes. |
| Use tmux keybindings for tabs and panes | ✓ on: in a pane of a tmux session, your tmux prefix then `c`, `%`, `"`, `n`, `p`, an arrow and the other window and pane keys work the tabs and panes; the prefix then `?` lists them (see [tmux](everyday-use.md#tmux)). Off by default. |
| Send break (serial) | Holds a serial line in break for 300 ms (Cisco ROM monitor, Linux SysRq). |
| Run login script again | Runs the connection's login script from the start. |
| Auto-responses from scripts | ✓ on: scripts with a *When* text answer by themselves. Off turns all of them off. `AutoResponses`. |
| Confirm scripts on Production hosts | ✓ on: a script about to be typed into a pane with the Production theme asks first. `ConfirmProduction`. |
| Open remote desktop | Ctrl+Shift+D. Tunnels the host's RDP port through SSH and opens Windows' Remote Desktop Connection on it (see [Remote desktops](everyday-use.md#remote-desktops)). |
| Open remote desktop in a tab (RDP) | Only in `ferossh-desktop.exe`: the desktop in a FeroSSH tab. |
| Open remote desktop in a tab (VNC) | Only in `ferossh-desktop.exe`: a VNC desktop in a FeroSSH tab. |
| Open virtual desktop (Xvfb) | Starts Xvfb and x11vnc on the server and shows that headless desktop through SSH: in a tab in `ferossh-desktop.exe`, else in the VNC viewer on this PC (see [Virtual desktops](everyday-use.md#virtual-desktops-xvfb)). Again while one runs: another viewer on it. |
| Stop virtual desktop | Stops the virtual desktop on this connection, and everything running in it. |
| Desktop clipboard > Both ways, Only Windows to host, Only host to Windows, Off | Which way copied text goes between remote and virtual desktops and Windows; changes the desktops already open at once. Remote Desktop Connection can only share both ways or none. Off by default. `DesktopClipboard` (see [Desktop clipboard](everyday-use.md#desktop-clipboard)). |
| Desktop clipboard > Also share mouse selections from virtual desktops | ✓ on: text you only select with the mouse in a virtual desktop also goes to the Windows clipboard; for virtual desktops started after the change. Off by default. `DesktopShareSelection`. |
| Remote apps > Run remote app... | Runs a Linux program on the pane's host and shows its windows as native windows here, over the same SSH connection (see [Remote apps](everyday-use.md#remote-apps-native-windows)). |
| Remote apps > Reattach remote app | Brings back the windows of the newest remote app still running on the host. |
| Remote apps > Copy the agent to hosts | ✓ on (the default): copies the FeroStream agent to a host that lacks it. `RemoteAppCopyAgent`. |
| Remote apps > Remove the agent when done | ✓ on: the agent deletes itself from the host when its last program ends. Off by default. `RemoteAppRemoveAgent`. |
| Remote apps > Clipboard > Both ways, Only Windows to host, Only host to Windows, Off | Which way copied text goes between remote apps and Windows; changes the remote apps already running at once. Off by default. `RemoteAppClipboard`. |
| Remote apps > Clipboard > Also share mouse selections from the host | ✓ on: text you only select with the mouse in a remote app (how xterm copies) also goes to the Windows clipboard. Off by default, so a selection does not replace what you copied on Windows. `RemoteAppShareSelection`. |

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
| Typing | Searches (in the search box above the tree). With nothing left, Enter connects to what you typed. |
| Down (in the search box) | Goes to the tree. |
| Tab | Moves between the search box, the tree and the Connect, New, Edit and Import buttons. |
| Enter / Shift+Enter | Connects in this tab / in a split. On a folder heading, opens or closes it. |
| Left / Right | Closes / opens a folder. Left on a host goes to its folder. |
| Ctrl+D | Marks or unmarks a favourite. |
| Insert | New connection. |
| F2 | Edits the selected connection. |
| Del | Deletes the selected saved connection, after asking. |
| Esc | Clears the search, then leaves. |
| F5 | Reads the hosts again. |

### Connection form

The form has a tab for each section: Connection, Look and Extras (a desktop or remote app connection has only Connection).

| Key | Action |
| --- | --- |
| Tab, Shift+Tab | Moves between fields and buttons. |
| Ctrl+Tab, Ctrl+PageDown / Ctrl+PageUp | Goes to the next or previous section tab. |
| Up / Down, Alt+Down | Picks another choice in a drop-down field (type, password source, theme...), or opens its list. |
| Enter | Saves. |
| Shift+Enter | Saves and connects. |
| Esc | Cancels. |

The buttons at the bottom do the same, and a saved connection has a Delete button that asks first. A problem with what was typed shows in red above the buttons.

### Session manager (Ctrl+Shift+L)

| Key | Action |
| --- | --- |
| Typing | Goes to the search box and filters by host, name, title or kind (SSH, Mosh, SFTP, WinRM, Telnet, serial, tmux, desktop, local). |
| Down (in the search box) | Moves into the list. |
| Tab | Moves between the search box, the list and the buttons. |
| Enter or double-click | Goes to the session's tab and pane (from the search box, the first one listed). A hidden script-run session, or the login of a desktop or remote app connection, is shown in a tab. |
| F2 | Renames the session in place: Enter keeps the name, Esc drops it, an empty name clears it. |
| Del | Closes the session, after asking when it is live. |
| Esc | Clears the search, then leaves. |

The Show, Rename and Close buttons below the list, and the right-click menu on a session (or Shift+F10 with the list focused, for example after Down from the search box), do the same.

### File transfer (Ctrl+Shift+E)

In the Local and Remote lists:

| Key | Action |
| --- | --- |
| Enter or double-click | Opens a folder, or sends a file to the other list's folder. |
| Ctrl+Enter | Sends the selection across: uploads from Local, downloads from Remote (folders whole). |
| Backspace | Goes to the parent folder. |
| Ctrl+A | Selects everything in the list. |
| Del | Deletes the Remote selection on the server, after asking. |
| F2 | Renames the selected file or folder. |
| F5 | Refreshes the list. |
| Shift+F10 or right-click | The list's menu. |
| Tab | Moves between the filter, the lists, their buttons and the queue. |
| Escape | Clears the filter; with none, gives the keyboard back to the terminal. |

In a path box, Enter goes to the folder typed. In the queue, Del cancels a transfer and the right-click menu pauses, resumes, retries and clears the finished ones. When a file already exists, Overwrite, Skip and Keep both answer above the queue; For all answers the same for the rest.

The arrow buttons between the lists move the selection across, and the gap between them drags to share the room differently. ★ Bookmarks ▾ opens, adds, renames and deletes bookmarks; in its name box Enter saves and Escape cancels. ↻ Refresh reads both lists again (F5 outside the lists). ⇆ Sync turns on Sync browsing, so opening a folder or going up in one list does the same in the other. Layout ▾ arranges the lists side by side or stacked, swaps them, and has Choose local folder, Upload files and Open local folder in Explorer.

### Port forwards (Ctrl+Shift+P)

| Key | Action |
| --- | --- |
| Enter in the form, or Add | Adds the forward the form describes (Type, Listen, To) on the connection in the On box. The Listen box also takes `-L 8080:db.internal:5432`, `-R 9000:localhost:3000`, `-D 1080`, or `-KL 8080`, `-KR 9000`, `-KD 1080` to stop one. |
| Up / Down in the list | Selects a forward; the line under the list shows its details. |
| Enter, F2, a double click or Edit | Puts the selected forward in the form to change it; Save keeps the change. |
| Retry | Starts a stopped forward again. |
| Del or Stop | Stops the selected forward. |
| Right click or Shift+F10 | Edit, Retry and Stop for the forward. |
| Esc | Ends a change or clears the form, then leaves. |

### Scripts (Ctrl+Shift+Space)

| Key | Action |
| --- | --- |
| Typing | Searches names, categories and lines (in the list it goes to the search box; Up and Down there move in the list). |
| Enter, double-click | Types the script, with Enter after each line. |
| Shift+Enter | Types it but leaves the last line for you to finish. |
| Left / Right | Jumps to the previous or next category. |
| Insert | New script (the search text becomes its name; it goes in the selected script's category). |
| F2 | Edits the script. |
| Del | Deletes it, after asking. |
| F5 | Runs it on several saved hosts (see [Runs on many hosts](everyday-use.md#runs-on-many-hosts)). |
| Ctrl+Shift+F1 to F12 | From anywhere: runs the script with that hotkey for the focused pane's host. |
| Escape | Empties the search box, then goes back to the terminal. |

The Run, Edit, New, Delete and On hosts buttons under the list, and the list's right-click menu, do the same.

In the script editor's lines, Enter ends a line, which is typed with Enter after it; Shift+Enter ends a line that is typed without Enter (it becomes an `@type` line). Enter in a field or Ctrl+Enter in the lines saves; Escape cancels.

On the page that asks for a script's arguments, Enter goes to the next box and runs the script from the last one; Shift+Enter or Ctrl+Enter runs it from any box; Up and Down move between the boxes; Escape cancels.

In a run's report, Enter shows that host's pane, C copies the report as a table, S saves it as CSV in the download folder, R runs the failed hosts again and Esc stops the run (or closes the report when it is over).

### Command palette (Ctrl+Shift+A)

A standard Windows page in the tools pane: a search box over a list with each entry's name, its shortcut (or the connection's `user@host`, the script's first line) and where it comes from (the menu, Connection, Script, Workspace). The keyboard stays in the search box: type a few letters in any order, Up, Down, Page Up and Page Down select, Enter (or Alt+R, Run, a double click) runs the action, connects or types the script, Shift+Enter connects in a split or types a script without its last Enter. Esc clears the search, then closes; so does Close. Typing while the list has the keyboard goes on in the search box.

## Key bindings

Settings > Key bindings opens a window with every action of the menus, greyed ones too, plus the scroll keys, which are in no menu. Each row shows the action, its shortcuts now and its default ones; a dot marks the ones you changed. The search box finds an action by name or by key (`ctrl+shift+n`).

- **Change...** (Enter or a double click on a row) waits for a key: press it with Ctrl, Alt or Shift as you want it, and it becomes the action's only shortcut. Esc cancels. **Add key...** adds a second one instead.
- **No shortcut** takes the action's keys away. **Reset to default** gives it its default keys back, and **Reset all** every action.
- A key that another action has moves to this one, after asking. A key that would stop typing working (a letter, Enter, Tab, Shift with a letter, AltGr with a letter) is refused. A key that programs in the terminal use is taken only after a warning: Ctrl with a letter (Ctrl+C stops a program, Ctrl+R searches the shell's history), Alt with a letter (Meta in shells and editors, and the menu letters), plain function keys and the like. Keys with Ctrl+Shift are the safest.
- Nothing changes until OK. The new keys work at once, show in the menus, the palette and the rail, and are saved in `ferossh.conf` as `KeyBinding` lines for the actions you changed.

Some keys only work in some places, whatever they are: the tab keys (Next tab, Previous tab, Move tab, Join tab) while the window has more than one tab, the pane keys (Focus pane, Grow pane) while the tab has more than one pane. Elsewhere those keys go to the terminal, as before. Shift+End also follows new output while the view is scrolled back, and Alt+F4 always closes the window.

In the tools pane's search boxes and lists, a key bound to an action runs it when it has Ctrl+Shift, is a function key, or is Ctrl or Alt with a key the control does not use itself; Ctrl+C, Ctrl+V, Ctrl+A, Ctrl+arrows, Shift+Insert and Alt with a button's letter stay with the control.

**tmux.** With Session > Use tmux keybindings on, the key after the tmux prefix goes to tmux first, before any key binding, as before. The prefix itself is a terminal key: if you give it to an action (Ctrl+B unless your tmux has another), tmux never gets it and the tmux keybindings stop working, so the Key bindings window warns before it takes it. The window knows the prefix of an open tmux session; with none open it assumes Ctrl+B. Likewise Ctrl+A, Ctrl+B and the other Ctrl letters reach tmux or screen running inside a plain SSH pane only while no action has them.

**Scripts and standby.** A script's hotkey (Ctrl+Shift+F1 to F12) runs before a key binding on the hosts the script is for, and the standby window's hotkey goes to Windows before FeroSSH sees it; the window warns about both.

**Other PCs.** In a master password vault, the key bindings travel with the settings: the PC that saves last decides them. File > Import encrypted connections takes in the key bindings of the exported file too, after saying how many actions change.

### Import connections

A standard Windows page in the tools pane: the programs that have something saved, in a list (Enter or a double click opens one; Alt+O Open, Alt+M From a file...), then that program's connections with a tick box each. Space or a click ticks one, Alt+A selects all, Alt+L clears all, Enter or Alt+I imports, Esc, Backspace or Alt+B goes back. Names that are already a host show greyed with no box. File > Import scripts works the same way.
