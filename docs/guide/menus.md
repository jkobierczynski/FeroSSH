# Menu reference

Every item in FeroSSH's menus, in the order they appear, with its default shortcut and what it does. The menu bar has seven menus: File, Edit, Settings, Tools, Window, Session and Help. Ctrl+Shift+M hides the menu bar; Alt shows it again. Every item here is also in the command palette (Ctrl+Shift+A), so you can type part of its name instead.

Settings > Key bindings changes the shortcuts, and gives one to any item that has none. The menus, the command palette and the tools rail's tooltips always show the keys as they are now; this page lists the defaults.

Items with a check mark are switches: picking them turns the setting on or off, and FeroSSH remembers it in `ferossh.conf` (see [Settings](settings.md)).

Items that can do nothing for the focused pane are greyed out, and the command palette leaves them out: the tmux items other than Attach while the pane shows no tmux session, Stop virtual desktop while none runs, Send break outside a serial console, the tab items with one tab and the pane items with one pane. Their shortcuts work as before.

## File

| Item | Shortcut | What it does |
| --- | --- | --- |
| Connect to... | Ctrl+Shift+N | Asks for a host in a new tab. The prompt takes `user@host`, `user@host:2222`, a `Host` alias from your ssh_config, a saved connection's name, `ssh://user@host[:port]`, `sftp://user@host[:port][/folder]` (file transfer only, no shell), `telnet://host[:port]` or `telnet host port`, `winrm://[user@]host[:port]`, `winrms://[user@]host[:port]` (HTTPS) or `winrm host`, and a COM port such as `COM3` or `COM3:115200`. |
| Reconnect | Ctrl+Shift+R | Logs in again on the focused pane's connection. While FeroSSH is already waiting to reconnect, it tries at once instead. |
| Disconnect | | Ends the focused pane's connection, or stops the retries while it is reconnecting. |
| Reconnect automatically | ✓ on | Logs in again by itself when a connection drops (see [When the connection drops](everyday-use.md#when-the-connection-drops)). `AutoReconnect`. |
| Recent connections > (a connection) | | The last eight hosts you connected to, from Connect to... or the connection manager, newest first: picking one connects again, as a double click in the connection manager does. Kept in `ferossh.conf` (`Recent` lines), so a master password vault holds them too. |
| Recent connections > Clear this list | | Forgets them. |
| Open local shell | | Opens a Windows command prompt (`%COMSPEC%`, usually `cmd.exe`) in a new tab. To start another program, use `ferossh.exe --local wsl -d Ubuntu` and the like. |
| Open local PowerShell | | Opens PowerShell in a new tab: PowerShell 7 (`pwsh.exe`) when it is installed, else Windows PowerShell. With Settings > Shell integration for local PowerShell on, it sends prompt marks without any change to your profile (see [Shell integration](everyday-use.md#shell-integration-prompt-marks)). A split of it opens PowerShell too. |
| Open WSL > (a distro) | | Opens that WSL distro's shell (`wsl.exe -d <distro>`) in a new tab, in its home folder; the default distro comes first. The list is read from where WSL keeps it each time the menu opens (Docker Desktop's own distros are left out). From a WSL pane, Session > Coding agents starts agents in tmux inside that distro (see [Coding agents in WSL and on this PC](everyday-use.md#coding-agents-in-wsl-and-on-this-pc)). |
| Upload files... | Ctrl+Shift+U | Picks files on your PC and uploads them over SFTP to the folder the file view shows, or the shell's current folder. |
| Open download folder | | Opens the folder downloads go to in Explorer. |
| Import and export > Import connections... | | Brings saved sessions over from PuTTY, KiTTY, WinSCP, MobaXterm, mRemoteNG and SecureCRT (see [Imports](everyday-use.md#importing-from-other-clients)). |
| Import and export > Import scripts... | | Brings scripts over from Windows Terminal (`sendInput` actions and `.wt.json` scripts), SecureCRT button bars and MobaXterm macros, or makes one from a text file (see [Importing scripts](everyday-use.md#importing-scripts)). |
| Import and export > Export connections... | | Writes your saved and imported connections and their settings to a `.ferossh` file encrypted with a passphrase, with the saved passwords only if you tick it (see [Backup and sync](everyday-use.md#backup-and-sync-between-pcs)). |
| Import and export > Export team library... | | Writes your saved connections, their settings, your security and theme rules and the custom themes they name, with no password or credential, to a `.ferossh-library` file for a team to share once you sign it (see [Team library](everyday-use.md#team-library)). |
| Import and export > Import encrypted connections... | | Opens such a file, asks its passphrase, says what it would add and replace, and imports it. Your other connections stay. |
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
| Previous command | Ctrl+Shift+Up | Scrolls the prompt of the command before the one at the top of the view to the top. Needs a shell that marks its prompts (OSC 133, see [Shell integration](everyday-use.md#shell-integration-prompt-marks)); without marks it says what to set up. |
| Next command | Ctrl+Shift+Down | The same, forward; past the last prompt, back to the live output. |
| Select command output | | Selects the output of the command at the top of the view, or of the last finished command at the live bottom. Same marks. |
| Copy last command's output | | Copies the output of the last command that finished. Same marks. |
| Save selection as script... | | Opens the script editor with the selected text as the script's lines. |
| Copy on select | ✓ off | Copies a selection as soon as you make it, as PuTTY does. `CopyOnSelect`. |
| Right-click pastes | ✓ off | A right click pastes, as in PuTTY. `RightClickPaste`. |
| Suggest commands as you type | ✓ off | Shows an earlier command on this host (or a common one) greyed after the cursor; Right arrow, End or Ctrl+E types it (see [Command suggestions](everyday-use.md#command-suggestions)). `CommandSuggestions`. |
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
| Desktop notifications | ✓ off | A tab badge and a Windows notification when a pane wants you: a notification the host sends (OSC 9, 777 or 99), the bell, or quiet after work. See [Notifications](everyday-use.md#notifications-coding-agents-and-long-jobs). `Notifications`. |
| Notify on the bell too | ✓ on | The bell counts as asking for attention. Greyed out while notifications are off. `NotifyBell`. |
| Notify when a long command finishes | off | A command that ran 10 seconds or more raises a notification when it finishes, from the shell's OSC 133 marks (see [Shell integration](everyday-use.md#shell-integration-prompt-marks)). Greyed out while notifications are off. `NotifyCommands`. |
| Shell integration for local PowerShell | ✓ on | PowerShell panes FeroSSH starts on this PC (File > Open local PowerShell, `--local powershell`, `--local pwsh`) get FeroSSH's prompt and command marks, added after your profile runs, so Previous command, Select command output, failed-command bars and the agent dashboard's *exited*/*failed* work there. A profile that already has the hook from the user guide is left alone. Takes effect in PowerShell panes opened afterwards. `LocalShellIntegration`. |
| Larger text | Ctrl+= (or Ctrl+wheel up) | Makes the focused pane's text bigger. |
| Smaller text | Ctrl+- (or Ctrl+wheel down) | Makes it smaller. |
| Reset text size | Ctrl+0 | Back to the pane's normal size. |
| Font (this pane) | | Eighteen monospace fonts for the focused pane. The five that come with Windows are listed first. A free font that is not installed reads "(download)" and opens its official download page; FeroSSH picks it up after a restart. |
| Font ligatures | ✓ off | Joins programming ligatures (`->`, `!=`, `===`) in fonts that have them, such as Cascadia Code, Fira Code and JetBrains Mono, in every terminal pane. Each character keeps its cell; the cursor, a selection and colour changes break a ligature into its characters. See [Fonts and colour themes](everyday-use.md#fonts-and-colour-themes). `FontLigatures`. |
| Theme (this pane) | | The colour theme for the focused pane, in three columns: GitHub Dark (the default) and 24 other dark themes, 11 light themes, then the Production and Staging warning themes over the classics (Campbell, One Half Dark and Light, Tango Dark and Light from Windows Terminal, and PuTTY's colours). The list is in [Everyday use](everyday-use.md). Like window colours, a theme under the mouse shows in the pane at once. |
| Custom themes (font and colours) | | Your own themes, one `.theme` text file each in the `themes` folder next to the settings. Each holds a terminal palette and, if you like, a font, a text size and window colours; picking one sets them all for the focused pane (a theme's window colours replace Settings > Window colours). **Save this look as a theme...** writes the focused pane's colours, font and size to a new file, with the window colours when they are a fixed look (not Light or Follow Windows; with Follow the terminal theme, only a custom theme's own); **Open themes folder** opens the folder to edit them in any text editor. The menu reads the folder each time it opens, so edits show at once. A file that cannot be used shows greyed with the reason; a theme with a smaller mistake stays selectable and reads "(see file)". The format is in [Everyday use](everyday-use.md#custom-themes). |
| Window colours (menus, tools) | | Where the menu bar and menus, the title bar, the tab strip, the tools rail and the tools pane take their colours. **Follow the terminal theme**: the focused pane's theme, so a dark theme gives a dark window and Production a red one. **Follow Windows**: Windows' light or dark app mode. **Light** (the default): Windows' usual light look. Neutral greys, light to dark, none of them tinted: **Soft grey**, **Silver**, **Mid grey**, **Steel grey**, **Graphite**, **Dark** and **Black**. Then soft tinted neutrals that are not grey: **Sand** (warm), **Sage** (green), **Mist** (cool), **Taupe** (a middle tone with black text), and the dark **Slate** (blue), **Moss** (green), **Espresso** (brown) and **Heather** (violet). Text on each reads at 4.5:1 or better. `WindowColours`. |
| Show test pattern | | Prints colours, text styles, box drawing, wide and combining characters in the pane, to check a font. |
| Hide menu bar (Alt shows it) | Ctrl+Shift+M | Hides the menu bar. Pressing Alt shows it again. |
| Full screen | F11 | ✓ while on. The window drops its title bar, frame and menu bar and covers the whole monitor, taskbar too. F11 again (or this item) puts it back as it was, maximized or not. Alt still shows the menu bar; it hides again once a menu closes. |
| F11 switches full screen | ✓ on | Off, F11 goes to the terminal like the other function keys, for programs that use it; Full screen stays in this menu and the command palette. With another key for Full screen (Key bindings), the item names that key. `FullScreenKey`. |
| Team library... | | Where your team's signed connection library is, and the public key it must be signed with (see [Team library](everyday-use.md#team-library)). Only shown, not changed, when an administrator sets it by [machine policy](../admin/team-library-policy.md). `TeamLibrary`, `TeamLibraryKey`. |
| Key bindings... | | A window listing every action of the menus with its shortcuts and its default ones: change them, add a second key, take a key away, or reset one action or all of them. See [Key bindings](#key-bindings). `KeyBinding`. |
| Open links > Open ssh:// links with FeroSSH, Open sftp:// links with FeroSSH | ✓ while on | Off until you turn them on. On, `ssh://` and `sftp://` links (in a browser, a wiki, an inventory) open in this `ferossh.exe`; written for your Windows account only, and taken out again when you turn it off. When another program opens them, FeroSSH offers itself in Windows' Default apps instead of taking over. See [Opening ssh:// links](everyday-use.md#opening-ssh-links). |
| Master password > Set a master password... | | Moves your connections, settings and (if ticked) saved passwords into an encrypted vault, `connections.vault`. FeroSSH then asks for the master password when it starts. |
| Master password > Use a vault file... | | Uses a vault another PC made, in a shared folder such as OneDrive, and adds this PC's connections to it. |
| Master password > Move the vault file... | | Moves the vault, for example into a OneDrive folder to share it between PCs. |
| Master password > Change master password... | | Asks for the current master password, then a new one. |
| Master password > Remove master password... | | Asks for the current master password, then writes the connections and settings back as plain files and the passwords back to the Windows Credential Manager. A portable FeroSSH drops the saved passwords instead (export them first to keep them). A vault in a shared folder stays there for the other PCs. |

Font, theme and text size apply to the focused pane only. Session > Save font and theme for this host keeps them for that host.

## Tools

| Item | Shortcut | What it does |
| --- | --- | --- |
| Connection manager | Ctrl+Shift+S | Shows or hides the side pane with your saved hosts (see [The connection manager](everyday-use.md#the-connection-manager)). |
| Session manager | Ctrl+Shift+L | Shows the sessions open right now, in every tab (see [The session manager](everyday-use.md#the-session-manager)). |
| File transfer | Ctrl+Shift+E | Shows the focused pane's host over SFTP in the side pane (see [File transfer](everyday-use.md#file-transfer)). |
| Port forwards | Ctrl+Shift+P | Lists, adds, changes and stops port forwards on live connections (see [Port forwards](everyday-use.md#port-forwards)). |
| Scripts | Ctrl+Shift+Space | Lists your scripts by category, to type one into the pane or edit them (see [Scripts](everyday-use.md#scripts)). |
| Known hosts | Ctrl+Shift+K | Lists the host keys in `known_hosts` to search, copy and remove, `@cert-authority` and `@revoked` lines and hashed host names included (see [Known hosts](everyday-use.md#known-hosts)). |
| Hosts | | One row per machine: the connections that reach the same address, with their users, ports, tags, latest login, host key and OS, to search, sort, connect and show in the connection manager (see [Hosts](everyday-use.md#hosts)). |
| Connection log | Ctrl+Shift+I | The log of every connection made on this PC (connected, disconnected, lost, failed), newest first, to search, sort, save as CSV and connect again (see [Connection log](everyday-use.md#connection-log)). |
| Open scripts folder | | Opens `%APPDATA%\FeroSSH\scripts` in Explorer: one file per script, a folder per category, to copy to another PC or back up (see [The scripts folder](everyday-use.md#the-scripts-folder)). |
| Command palette | Ctrl+Shift+A | One search box over every menu item, connection and script (see [Command palette](everyday-use.md#the-command-palette)). |
| Credential manager | | Lists your named credentials and the passwords FeroSSH keeps, each with the connections that use it, to search, add, change, rename and delete (see [The credential manager](everyday-use.md#the-credential-manager)). |
| Security rules | | Lists the security rules, each with what it matches, its limits and the connections it covers, to search, add, change and delete (see [Security rules](everyday-use.md#security-rules-by-tag-or-folder)). Also the padlock on the tools rail. |
| Theme rules | | Lists your theme rules (tags or host patterns that pick a theme, font and size) in the order they are tried, then the built-in tag rules, each with the connections it matches, to add, change, reorder and delete rules (see [Theme rules](everyday-use.md#theme-rules)). Also the tag on the tools rail. |
| Theme manager | | Lists the themes, by kind, and the eighteen fonts, each with the rules that use it, with a preview, to put one on the pane, make it the default for new tabs or use it in a rule (see [The theme manager](everyday-use.md#the-theme-manager)). Also the palette on the tools rail. |
| New SSH key... | | Makes an Ed25519 key file (with or without a passphrase) or a security key or Windows Hello key, and offers to copy it to the focused pane's host (see [Making a key](everyday-use.md#making-a-key-and-putting-it-on-a-server)). |

## Window

| Item | Shortcut | What it does |
| --- | --- | --- |
| New tab | Ctrl+Shift+T | A new tab on the focused pane's host, as a new channel on the same connection, so there is no second login. |
| Rename session... | | Names the focused session in place on its tab (F2 in the session manager does the same); an empty name gives the tab its title back. In tmux the name goes to the tmux window too. |
| Next tab | Ctrl+Tab | |
| Previous tab | Ctrl+Shift+Tab | |
| Move tab left | Ctrl+Shift+PgUp | |
| Move tab right | Ctrl+Shift+PgDn | |
| Split right | Alt+Shift+= | Splits the focused pane, the new one on the right, on the same connection and in the same directory (see [New tabs and panes start in the same directory](everyday-use.md#new-tabs-and-panes-start-in-the-same-directory)). |
| Split down | Alt+Shift+- | The same, the new pane below. |
| Move pane to its own tab | | Takes the focused pane out of its tab into a new one. |
| Hide pane (keeps running) | Ctrl+Shift+H | Takes the focused pane out of the tabs without closing it; the session runs on and the Session manager lists it as hidden, to show it again. Not for tmux sessions in tmux's own layout. |
| Show last hidden pane | | Puts the pane hidden last back where it was. |
| Join tab into the tab before it | Ctrl+Shift+J | Puts this tab's panes into the tab to its left. |
| Focus pane left, right, up, down | Alt+arrows | Moves the focus to the pane in that direction. |
| Back to the last pane | | Moves the focus back to the pane focused before this one, in this tab or another; again goes back and forth between the two. Greyed when that pane has closed or is hidden. |
| Back to the last pane and paste | | The same, then pastes the clipboard there as Ctrl+Shift+V does (with its checks and broadcast). Copy with Ctrl+Shift+C in one pane, then press it to paste into the pane you came from. |
| Grow pane left, right, up, down | Alt+Shift+arrows | Moves the focused pane's edge that way. |
| Broadcast input to this tab's panes | Ctrl+Shift+B | What you type and paste goes to every pane of the tab (see [Broadcast input](everyday-use.md#broadcast-input)). |
| Broadcast input to all tabs | | The same, to every pane in every tab. |
| Leave this pane out of broadcast | | Keeps the focused pane to itself while broadcast is on. |
| Workspaces > Save layout as workspace... | | Saves the open tabs and split panes, each with its connection, directory, session name and own look, as a workspace under the name you give. See [Workspaces](everyday-use.md#workspaces). |
| Workspaces > Open workspace file... | | Opens a workspace file from any folder as new tabs. |
| Workspaces > Open workspaces folder | | Opens `%APPDATA%\FeroSSH\workspaces` in Explorer. |
| Workspaces > Log in automatically | | On (a check mark): a workspace's panes keep the password of a connection that would ask for it, in the vault or the Windows Credential Manager, so it is asked once. Off unless you turn it on. See [Workspaces](everyday-use.md#workspaces). |
| Workspaces > Reopen the last layout at start | | On (a check mark): the tabs and panes open when FeroSSH closes open again at the next start without a host, local shell or workspace on the command line. Only the first FeroSSH running keeps and reopens its layout; a second one opened meanwhile does neither. A layout too big for a workspace file is not kept: the one kept before opens instead. Off unless you turn it on. |
| Workspaces > (a workspace) | | Opens that workspace's tabs next to the open ones; a window that only waits at the connect prompt makes way for them. |
| Standby window (hotkey) > Status | | Says whether the standby hotkey is active, and for which window: on, off, or turned on but taken by another program. Click it for the details. See [Standby window](everyday-use.md#standby-window). |
| Standby window (hotkey) > Standby hotkey on | | Turns the standby hotkey on, or off. ✓ while this FeroSSH window holds the hotkey. Turning it off shows a hidden window again. `Standby`. Off by default. |
| Standby window (hotkey) > Window to show and hide | | This FeroSSH window, or one of the open windows of other programs (listed with their program, such as `notepad.exe`). Picking one turns standby on. FeroSSH remembers the program, so after a restart it uses that program's front window. `StandbyWindow`. |
| Standby window (hotkey) > Press a new hotkey... | | After OK, the next key you press with Ctrl, Alt or Win becomes the hotkey, and standby turns on. Esc cancels. `StandbyHotkey`. |
| Standby window (hotkey) > Win+` … Pause | | Picks one of five hotkeys and turns standby on: Win or Ctrl with the key below Esc (shown as your keyboard prints it), Ctrl+Alt+Space, Scroll Lock or Pause. The one in use is marked. |
| Close pane | Ctrl+Shift+W | |
| Close tab | Ctrl+F4 | Closes every pane in the tab, after asking when it has more than one open SSH session. |

## Session

| Item | What it does |
| --- | --- |
| Log to file | Writes what the focused tab receives to a log file. ✓ while logging. See [Logs](logs.md). |
| Logs and recordings > Log raw output (keep escape codes) | Logs every byte as received, so the log replays with `cat`. ✓ |
| Logs and recordings > Timestamp each log line | Puts the time each line ended in front of it, in plain logs. ✓ `LogTimestamps`. |
| Logs and recordings > Save this pane's output to a log | Writes everything the pane still holds, scrollback and screen, to a new log file. |
| Logs and recordings > Save output of all panes to logs | The same for every pane in every tab, one file each. |
| Logs and recordings > Record this pane (asciicast) | Records what the focused pane shows, with its timing, to a `.cast` file in the log folder, until you choose it again. Only the host's output goes in, never your keystrokes: what you type is in it only as the host echoes it, so a password typed at a prompt that does not echo stays out, but a secret the host echoes or prints (such as `mysql -pSecret` on a command line) is recorded. Off by default; ✓ while recording. See [Recording a pane](logs.md#recording-a-pane-and-playing-it-back). |
| Logs and recordings > Play a recording... | Picks a `.cast` file and plays it in a new tab: Space pauses, Left and Right go 5 seconds back and on (Shift: 30), Up and Down change the speed, Home starts again, End goes to the end. |
| Logs and recordings > Open log folder | Opens `%LOCALAPPDATA%\FeroSSH\logs` in Explorer. |
| Logs and recordings > Check a log for changes... | Picks a log and says whether it is intact, or where it was changed, cut short or added to. |
| Notify when this pane goes quiet | Watches the focused pane: when its output stops for 5 seconds after at least 8 seconds of output, it asks for attention. For coding agents and long jobs that send no notification of their own. ✓ while watching; greyed out while Settings > Desktop notifications is off. |
| Coding agents > Agent dashboard (who waits for you) | Opens the Session manager as the agent dashboard (the filter `is:agent`): every coding agent on every connected host, with its host, tab, what it is doing (working, needs you, quiet, idle), for how long, and the last line it showed, those waiting for you first. See [Coding agents in tmux](everyday-use.md#coding-agents-in-tmux). |
| Coding agents > Show what changed here (side-by-side git diff) | Opens a tab with the changes in the git repository the pane (or the agent in it) is in, compared with the last commit, old and new side by side; untracked new files show as added. In the tab, r looks again, n and p go from file to file. See [Coding agents in tmux](everyday-use.md#coding-agents-in-tmux). |
| Coding agents > Remove this agent's worktree... | From the pane of an agent started in a new git worktree, asks, then runs `git worktree remove` on its folder. git keeps a worktree with uncommitted or untracked changes, and then nothing else happens; files git ignores are deleted with the folder. Once the folder is gone the agent's tmux window closes, and its branch is deleted only if it is merged (`git branch -d`), so the agent's commits stay. See [An agent in a worktree of its own](everyday-use.md#an-agent-in-a-worktree-of-its-own). |
| Coding agents > Paste and drop files: upload and type the path | When ticked, pasting an image or files copied in Explorer, or dropping files on a pane, uploads them to `~/.cache/ferossh/paste/` on the pane's host and types their paths at the cursor, for coding agents. Off: drops ask and upload to the shell's folder. See [Pasting screenshots and files for an agent](everyday-use.md#pasting-screenshots-and-files-for-an-agent). Setting `UploadAndTypePath`. |
| Coding agents > Offer to forward and open web servers on the host | When ticked, an address such as `http://localhost:5173/` in a pane's output shows a bar offering to forward that port of the host to this PC and open it in the browser; each forward is a click. See [Opening a web server on the host](everyday-use.md#opening-a-web-server-on-the-host). Setting `PortPreviews`. |
| Coding agents > Kitty keyboard protocol for programs that ask for it | When ticked, a program may turn on the kitty keyboard protocol's first level, so Escape, Shift+Enter and Ctrl or Alt keys come as unambiguous codes. See [Keys and drawing for agent interfaces](everyday-use.md#keys-and-drawing-for-agent-interfaces). Setting `KittyKeyboard`. |
| Coding agents > Shift+Enter types a new line (sends Esc Enter) | When ticked, Shift+Enter sends `Esc Enter`, which coding agents take as a new line in their input box. Setting `ShiftEnter`. |
| Coding agents > Log and record agent panes (audit trail) | When ticked, every coding agent's pane is logged and recorded (sealed) from when it opens, in files named after the host and the agent. See [Coding agents on Production hosts](everyday-use.md#coding-agents-on-production-hosts). Setting `AgentAudit`. |
| Coding agents > Answer yes-or-no questions from the notification | When ticked, the Windows notification of an agent stopped at a yes-or-no question shows the question, and clicking it asks Yes, No or Cancel in a dialog, without switching tabs. See [Coding agents in tmux](everyday-use.md#coding-agents-in-tmux). Setting `AnswerFromNotification`. |
| Coding agents > Start ... in tmux | Starts that agent command in a new tmux window on the focused pane's server, in the pane's directory: in the tmux session shown for that host, or in session `agents`, attached here. It keeps running when the connection drops. From a WSL pane, the same happens in tmux inside that distro, attached in a new tab. See [Coding agents in tmux](everyday-use.md#coding-agents-in-tmux). The commands are `claude`, `codex` and `aider` unless `AgentCommand` lines list others. |
| Coding agents > Start ... in a new tab | From a local Windows shell (cmd or PowerShell), this replaces Start ... in tmux. It opens a new tab with PowerShell in the shell's folder and runs the agent there, so the shell stays when the agent exits. The tab is an agent pane (dashboard, quiet alerts, audit trail). It ends with its tab or with FeroSSH, since Windows has no tmux. See [Coding agents in WSL and on this PC](everyday-use.md#coding-agents-in-wsl-and-on-this-pc). |
| Coding agents > Start ... in a new git worktree | Like Start ... in tmux, but first makes a git worktree and branch of the repository the focused pane is in, next to its main folder (`git worktree add -b claude ../<repo>-claude`, then `claude-2` and so on), and starts the agent there. The dashboard and the diff tab show its branch. See [An agent in a worktree of its own](everyday-use.md#an-agent-in-a-worktree-of-its-own). |
| tmux > Attach tmux | Types `tmux -CC new-session -A -s main` in the pane and turns tmux windows and panes into native tabs and panes (see [tmux](everyday-use.md#tmux)). |
| tmux > Switch tmux session... | Lists the other tmux sessions on the server and switches to one. |
| tmux > Detach tmux | Leaves tmux, and everything running in it, on the server. Attach again later from this PC or another. |
| tmux > Detach other clients | Detaches the other clients of this tmux session, such as one left behind by a dropped connection; clients of other sessions stay. tmux makes a window only as big as its smallest client, so another client can leave part of the pane empty; FeroSSH offers this itself when it sees that. |
| tmux > Share this session... | Lets others watch (or join) the tmux session: anyone logged in as you, or, on tmux 3.3 and later, the other accounts you name, through a shared tmux server only they can reach. Other accounts are kept watch-only only from tmux 3.9; on older tmux they can only be let in to type, which lets them run commands as you. See [Shared sessions](everyday-use.md#shared-sessions). |
| tmux > Join a shared session... | Watches a tmux session of your own account, or one another account shares: read-only unless you tick Type into it too, and never resizing, closing or rearranging its windows. |
| tmux > Stop sharing | Takes other accounts off the shared tmux server, makes its socket private again, and detaches read-only watchers of this session. |
| tmux > One window per pane (own fonts) | ✓ on: each native pane is its own tmux window, so each can have its own font. Off: tmux's own splits are the panes. |
| tmux > Use tmux keybindings for tabs and panes | ✓ on: in a pane of a tmux session, your tmux prefix then `c`, `%`, `"`, `n`, `p`, an arrow and the other window and pane keys work the tabs and panes; the prefix then `?` lists them (see [tmux](everyday-use.md#tmux)). Off by default. |
| Remote desktop > Open remote desktop | Ctrl+Shift+D. Tunnels the host's RDP port through SSH and opens Windows' Remote Desktop Connection on it (see [Remote desktops](everyday-use.md#remote-desktops)). |
| Remote desktop > Open remote desktop in a tab (RDP) | Only in `ferossh-desktop.exe`: the desktop in a FeroSSH tab. |
| Remote desktop > Open remote desktop in a tab (VNC) | Only in `ferossh-desktop.exe`: a VNC desktop in a FeroSSH tab. |
| Remote desktop > Open virtual desktop (Xvfb) | Starts Xvfb and x11vnc on the server and shows that headless desktop through SSH: in a tab in `ferossh-desktop.exe`, else in the VNC viewer on this PC (see [Virtual desktops](everyday-use.md#virtual-desktops-xvfb)). Again while one runs: another viewer on it. |
| Remote desktop > Stop virtual desktop | Stops the virtual desktop on this connection, and everything running in it. |
| Remote desktop > Clipboard > Both ways, Only Windows to host, Only host to Windows, Off | Which way copied text goes between remote and virtual desktops and Windows; changes the desktops already open at once. Remote Desktop Connection can only share both ways or none. Off by default. `DesktopClipboard` (see [Desktop clipboard](everyday-use.md#desktop-clipboard)). |
| Remote desktop > Clipboard > Also share mouse selections from virtual desktops | ✓ on: text you only select with the mouse in a virtual desktop also goes to the Windows clipboard; for virtual desktops started after the change. Off by default. `DesktopShareSelection`. |
| Remote apps > Run remote app... | Runs a Linux program on the pane's host and shows its windows as native windows here, over the same SSH connection (see [Remote apps](everyday-use.md#remote-apps-native-windows)). |
| Remote apps > Reattach remote app | Brings back the windows of the newest remote app still running on the host. |
| Remote apps > Copy the agent to hosts | ✓ on (the default): copies the FeroStream agent to a host that lacks it. `RemoteAppCopyAgent`. |
| Remote apps > Remove the agent when done | ✓ on: the agent deletes itself from the host when its last program ends. Off by default. `RemoteAppRemoveAgent`. |
| Remote apps > Clipboard > Both ways, Only Windows to host, Only host to Windows, Off | Which way copied text goes between remote apps and Windows; changes the remote apps already running at once. Off by default. `RemoteAppClipboard`. |
| Remote apps > Clipboard > Also share mouse selections from the host | ✓ on: text you only select with the mouse in a remote app (how xterm copies) also goes to the Windows clipboard. Off by default, so a selection does not replace what you copied on Windows. `RemoteAppShareSelection`. |
| Scripts > Run login script again | Runs the connection's login script from the start. |
| Scripts > Record keystrokes as script | Records what you type in the focused pane until you pick it again, then opens the result in the script editor (see [Recording](everyday-use.md#recording-a-script)). ✓ while recording. |
| Scripts > Write built-in scripts to the library | Writes FeroSSH's built-in scripts into your scripts folder, under `Library`. Adds the ones not there yet and asks before overwriting any you already have (see [The built-in library](everyday-use.md#the-built-in-library)). |
| Scripts > Auto-responses from scripts | ✓ on: scripts with a *When* text answer by themselves. Off turns all of them off. `AutoResponses`. |
| Scripts > Confirm scripts and agents on Production hosts | ✓ on: a script about to be typed into a pane with the Production theme asks first, and so do starting a coding agent there and pasting or dropping into an agent's pane there. `ConfirmProduction`. |
| Replace host key... | After a host key change you know the reason for, trusts the new key. It asks twice. To look through or remove stored keys without connecting, use Tools > Known hosts. |
| SSH escape sequences (~. ~C ~? after Enter) | ✓ on: `~.`, `~C`, `~#`, `~R`, `~?` and `~~` typed first on a line work as in OpenSSH (see [Escape sequences](everyday-use.md#escape-sequences-)). Off sends every key to the host in every pane, whatever a host's `EscapeChar` says, and keeps the character for later. `EscapeSequences`; on by default. |
| Copy public key to this server... | Like `ssh-copy-id`: adds a `.pub` file's key to `~/.ssh/authorized_keys` on the focused pane's host, unless it is there already (see [Making a key](everyday-use.md#making-a-key-and-putting-it-on-a-server)). |
| Verify transfers (SHA-256) | Compares SHA-256 sums on both ends after each SFTP transfer (the server needs `sha256sum` and `xargs`). ✓ `VerifyTransfers`. |
| ZMODEM: answer rz and sz | Answers `rz` and `sz` in the terminal (see [ZMODEM](everyday-use.md#zmodem)). ✓ `ZModem`. |
| Show images the host prints (Sixel, kitty, iTerm2) | Shows pictures programs on the host print, such as `img2sixel`, `kitten icat` and `imgcat` (see [Images in the terminal](everyday-use.md#images-in-the-terminal)). ✓ `InlineImages`; off by default. |
| Highlight keywords | Colours errors, warnings, success words and addresses in the output, and your own `HighlightRule`s (see [Highlighting and following output](everyday-use.md#highlighting-and-following-output)). ✓ on. `Highlight`. |
| Save font and theme for this host | Keeps the focused pane's font, text size and theme in the host's profile. |
| Start new tabs and panes in this directory | Keeps the focused pane's directory as the host's start directory (Start dir in the connection form): new tabs, panes and tmux windows on that host start there when the directory of the pane they come from is not known, also after a restart. |
| Send break (serial) | Holds a serial line in break for 300 ms (Cisco ROM monitor, Linux SysRq). |

## Help

| Item | What it does |
| --- | --- |
| Check for updates... | Asks GitHub whether a newer release exists, checks the answer's signature, and offers to open its page. |
| Check for updates weekly | ✓ off: does the same once a week, quietly unless there is a new release. `UpdateCheck weekly`. |
| About FeroSSH | Shows the version and licence. |

## Right-click menu in the connection manager

Right-clicking a host in the connection manager offers Connect (Enter), Open in a split (Shift+Enter), Edit connection... (F2), Delete connection... (Del), Add tag, Remove tag, Move to folder, Change a field and New connection... (Ins). With several connections selected, the menu acts on them all instead, so clear the selection first to act on one.

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
| F2 | Edits the selected connection; on a workspace, opens its file in your text editor. |
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
| Typing | Goes to the search box and filters by host, name, title or kind (SSH, Mosh, SFTP, WinRM, Telnet, serial, tmux, desktop, local), or by `agent`, an agent's name or its state; `is:agent` lists the coding agents only. Coding agents are listed first. |
| Down (in the search box) | Moves into the list. |
| Tab | Moves between the search box, the list and the buttons. |
| Enter or double-click | Goes to the session's tab and pane (from the search box, the first one listed). A pane you hid comes back where it was; a hidden script-run session, or the login of a desktop or remote app connection, is shown in a tab. |
| F2 | Renames the session in place: Enter keeps the name, Esc drops it, an empty name clears it. |
| Del | Closes the session, after asking when it is live. |
| Esc | Clears the search, then leaves. |

The Show, Rename and Close buttons below the list, and the right-click menu on a session (which also has Hide) (or Shift+F10 with the list focused, for example after Down from the search box), do the same.

### File transfer (Ctrl+Shift+E)

In the Local and Remote lists:

| Key | Action |
| --- | --- |
| Enter or double-click | Opens a folder, or sends a file to the other list's folder. |
| Ctrl+Enter | Sends the selection across: uploads from Local, downloads from Remote (folders whole). |
| Backspace | Goes to the parent folder. |
| Ctrl+A | Selects everything in the list. |
| Del | Deletes the selection, after asking: on the server from Remote, to the Recycle Bin from Local. |
| F2 | Renames the selected file or folder. |
| F5 | Refreshes the list. |
| Shift+F10 or right-click | The list's menu. |
| Tab | Moves between the filter, the lists, their buttons and the queue. |
| Escape | Clears the filter; with none, gives the keyboard back to the terminal. |

In a path box, Enter goes to the folder typed. In the queue, Del cancels a transfer and the right-click menu pauses, resumes, retries and clears the finished ones. When a file already exists, Overwrite, Skip and Keep both answer above the queue; For all answers the same for the rest.

Click a column header (Name, Size or Modified) to sort that list on it, and again to reverse it (each list keeps its own sort); the arrow in the header shows which way. The right-click menu's Sort by items do the same. Folders always stay above files.

The arrow buttons between the lists move the selection across, and the gap between them drags to share the room differently. ★ Bookmarks ▾ opens, adds, renames and deletes bookmarks; in its name box Enter saves and Escape cancels. ↻ Refresh reads both lists again (F5 outside the lists). ⇆ Sync turns on Sync browsing, so opening a folder or going up in one list does the same in the other. Layout ▾ arranges the lists side by side or stacked, swaps them, and has Choose local folder, Upload files and Open local folder in Explorer.

### Port forwards (Ctrl+Shift+P)

| Key | Action |
| --- | --- |
| Enter in the form, or Add | Adds the forward the form describes (Type, Listen, To) on the connection in the On box. The Type box has Local, Remote, SOCKS and Server SOCKS; Listen and To also take Unix socket paths. The Listen box also takes `-L 8080:db.internal:5432`, `-R 9000:localhost:3000`, `-D 1080`, `-R 1080`, `-L 2375:/var/run/docker.sock`, or `-KL 8080`, `-KR 9000`, `-KD 1080` to stop one. |
| Up / Down in the list | Selects a forward; the line under the list shows its details. |
| Enter, F2, a double click or Edit | Puts the selected forward in the form to change it; Save keeps the change. |
| Retry | Starts a stopped forward again. |
| Del or Stop | Stops the selected forward. |
| Right click or Shift+F10 | Edit, Retry and Stop for the forward. |
| Esc | Ends a change or clears the form, then leaves. |

### Known hosts (Ctrl+Shift+K)

| Key | Action |
| --- | --- |
| Typing | Searches hosts, kinds, key types, fingerprints, comments and files (in the list it goes to the search box; Down there moves to the list). A host name (`web.example`, `web:2222`, `[10.0.0.5]:2222`) also finds the lines for that host the way a connection looks them up: hashed names, wildcards and `!` exclusions. |
| Shift or Ctrl with a click, Ctrl+A | Picks several entries. |
| Del or Remove... | Removes the picked entries, after asking (twice for a `@revoked` line). |
| Ctrl+C or Copy fingerprint | Copies the entry's SHA-256 fingerprint. |
| F5 or Reload | Reads the files again. |
| Right click or Shift+F10 | Copy fingerprint, Copy line and Remove for the entry. |
| Escape | Clears the search; with none, gives the keyboard back to the terminal. |

### Hosts

| Key | Action |
| --- | --- |
| Typing | Searches with the connection manager's search words; a host shows when one of its connections matches. Plain words also look at the address, OS, kernel and fingerprints. In the list it goes to the search box; Down there moves to the list. |
| A click on a column heading | Sorts by that column; again for the other way. |
| Enter in the list, a double click or Connect | Opens the host's connection used last. |
| Connections | Shows the host's connections in the connection manager (searched with `addr:`). |
| F5 or Reload | Reads the connection log again. |
| Escape | Clears the search; with none, gives the keyboard back to the terminal. |

### Connection log (Ctrl+Shift+I)

| Key | Action |
| --- | --- |
| Typing | Searches every column: plain words (`web01`, `failed`), `-word` to leave entries out, `"two words"`. In the list it goes to the search box; Down there moves to the list. |
| A click on a column heading | Sorts by that column; again for the other way. |
| Enter in the list, a double click or Connect | Opens the picked entry's connection again. |
| Save CSV | Asks where to save the entries shown, in their order (with a vault open, warns first that the CSV is plain text). |
| Delete... or the Delete key | Asks, then deletes the picked entries (Ctrl or Shift picks several). Not there when your organisation keeps every entry by [machine policy](../admin/connection-log-policy.md). |
| F5 or Reload | Reads the connection log again. |
| Escape | Clears the search; with none, gives the keyboard back to the terminal. |

### Credential manager

| Key | Action |
| --- | --- |
| Typing | Searches names, users, hosts and connections; `-word` leaves rows out, `nothing` finds what nothing uses. In the list it goes to the search box; Down there moves to the list. |
| A click on a column heading | Sorts by that column; again for the other way. |
| Enter in the list, a double click or Edit... | Changes or renames the picked credential. |
| New credential... | Makes a credential. |
| Del or Delete... | Deletes the picked row, after asking. |
| Escape | Clears the search; with none, gives the keyboard back to the terminal. |

### Security rules

| Key | Action |
| --- | --- |
| Typing | Searches tags, folders, hosts, limits and connections; `-word` leaves rows out, `nothing` finds rules that cover no connection. In the list it goes to the search box; Down there moves to the list. |
| A click on a column heading | Sorts by that column; again for the other way. |
| Enter in the list, a double click or Edit... | Changes the picked rule. |
| New rule... | Makes a rule. |
| Del or Delete... | Deletes the picked rule, after asking. |
| Escape | Clears the search; with none, gives the keyboard back to the terminal. |

### Theme rules

| Key | Action |
| --- | --- |
| Typing | Searches rules, what they set and their connections; `-word` leaves rows out, `unused` finds rules no connection matches. In the list it goes to the search box; Down there moves to the list. |
| A click on a column heading | Sorts by that column within each group; again for the other way. |
| Enter in the list, a double click or Edit... | Changes the picked rule (the built-in ones cannot change). |
| New rule... | Makes a rule, at the end of the list. |
| Move up, Move down | Moves the picked rule: the first rule that sets something wins. |
| Del or Delete... | Deletes the picked rule, after asking. |
| Escape | Clears the search; with none, gives the keyboard back to the terminal. |

### Theme manager

| Key | Action |
| --- | --- |
| Typing | Searches themes, fonts and the rules that use them; `-word` leaves rows out, `unused` finds what no rule uses. In the list it goes to the search box; Down there moves to the list. |
| A click on a column heading | Sorts by that column within each group; again for the other way. |
| A click on a group heading | Folds or unfolds the group. |
| Enter in the list, a double click or Apply to this pane | Puts the picked theme or font on the pane behind the tools. |
| Set as default | New tabs open with the picked theme or font. |
| Use in a rule... | Opens Theme rules with a new rule that starts with the picked theme or font. |
| Edit colours... | Changes the picked theme's colours (foreground, background, cursor, cursor text, the 16 ANSI colours) with a swatch, `#RRGGBB` field and preview each. Save writes your own theme's file; Save as... makes a new theme file. Built-in themes only have Save as (suggested name "My <theme>"), so they never change. |
| Open themes folder | Opens the folder of your own themes in Explorer. |
| Escape | Clears the search; with none, gives the keyboard back to the terminal. |

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

Settings > Key bindings opens a window with every action of the menus, greyed ones too, plus the scroll keys and the command suggestion keys, which are in no menu. Each row shows the action, its shortcuts now and its default ones; a dot marks the ones you changed. The search box finds an action by name or by key (`ctrl+shift+n`).

- **Change...** (Enter or a double click on a row) waits for a key: press it with Ctrl, Alt or Shift as you want it, and it becomes the action's only shortcut. Esc cancels. **Add key...** adds a second one instead.
- **No shortcut** takes the action's keys away. **Reset to default** gives it its default keys back, and **Reset all** every action.
- A key that another action has moves to this one, after asking. A key that would stop typing working (a letter, Enter, Tab, Shift with a letter, AltGr with a letter) is refused. A key that programs in the terminal use is taken only after a warning: Ctrl with a letter (Ctrl+C stops a program, Ctrl+R searches the shell's history), Alt with a letter (Meta in shells and editors, and the menu letters), plain function keys and the like. Keys with Ctrl+Shift are the safest.
- Nothing changes until OK. The new keys work at once, show in the menus, the palette and the rail, and are saved in `ferossh.conf` as `KeyBinding` lines for the actions you changed.

Some keys only work in some places, whatever they are: the tab keys (Next tab, Previous tab, Move tab, Join tab) while the window has more than one tab, the pane keys (Focus pane, Grow pane) while the tab has more than one pane, and the Suggestions keys (Type the whole suggestion, Type its next word, Show the next or previous match, Forget the one shown) while a command suggestion shows greyed after the cursor. Elsewhere those keys go to the terminal, as before. Since a Suggestions key leaves the terminal alone the rest of the time, the window gives one without the terminal-key warning; the tmux prefix still goes to tmux while tmux keybindings are on. Shift+End also follows new output while the view is scrolled back, and Alt+F4 always closes the window.

In the tools pane's search boxes and lists, a key bound to an action runs it when it has Ctrl+Shift, is a function key, or is Ctrl or Alt with a key the control does not use itself; Ctrl+C, Ctrl+V, Ctrl+A, Ctrl+arrows, Shift+Insert and Alt with a button's letter stay with the control.

**tmux.** With Session > Use tmux keybindings on, the key after the tmux prefix goes to tmux first, before any key binding, as before. The prefix itself is a terminal key: if you give it to an action (Ctrl+B unless your tmux has another), tmux never gets it and the tmux keybindings stop working, so the Key bindings window warns before it takes it. The window knows the prefix of an open tmux session; with none open it assumes Ctrl+B. Likewise Ctrl+A, Ctrl+B and the other Ctrl letters reach tmux or screen running inside a plain SSH pane only while no action has them.

**Scripts and standby.** A script's hotkey (Ctrl+Shift+F1 to F12) runs before a key binding on the hosts the script is for, and the standby window's hotkey goes to Windows before FeroSSH sees it; the window warns about both.

**Other PCs.** In a master password vault, the key bindings travel with the settings: the PC that saves last decides them. File > Import encrypted connections takes in the key bindings of the exported file too, after saying how many actions change.

### Import connections

A standard Windows page in the tools pane: the programs that have something saved, in a list (Enter or a double click opens one; Alt+O Open, Alt+M From a file...), then that program's connections with a tick box each. Space or a click ticks one, Alt+A selects all, Alt+L clears all, Enter or Alt+I imports, Esc, Backspace or Alt+B goes back. Names that are already a host show greyed with no box. File > Import scripts works the same way.
