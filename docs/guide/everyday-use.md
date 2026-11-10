# Everyday use

This page explains each feature in turn. [Menu reference](menus.md) lists every menu item and key, and [Settings](settings.md) every keyword in `ferossh.conf`.

## Tabs and splits

The window starts as one pane, with no tab strip. Ctrl+Shift+T opens a tab and the strip appears; Alt+Shift+= and Alt+Shift+- split the pane right or down. A new tab or pane on a host you are already logged in to opens another channel on the same connection, so there is no second login.

- Alt+arrows moves the focus, Alt+Shift+arrows grows the focused pane, and dragging a divider resizes.
- Ctrl+Tab and Ctrl+Shift+Tab switch tabs. Drag a tab, or press Ctrl+Shift+PgUp and PgDn, to reorder it.
- Ctrl+Shift+drag a pane onto another pane's edge to move it there, or onto its middle to swap the two.
- To merge a tab back in as a pane, drag the tab down out of the strip onto a pane of the tab that appears, or use Window > Join tab into the tab before it (Ctrl+Shift+J). Window > Move pane to its own tab, or dropping a pane on the tab strip, makes it a tab again.
- Ctrl+Shift+W closes the pane, Ctrl+F4 the tab.

### New tabs and panes start in the same directory

A split (Window > Split right or Split down) and a new tab (Ctrl+Shift+T) start their shell in the directory the focused pane's shell is in.

- In a tmux session, FeroSSH asks tmux where the pane is and opens the new tmux window or pane there.
- In a plain SSH pane, FeroSSH uses the directory the shell reported (see [Following cd](#following-cd)) and types `cd` to it once, right after the new pane logs in.
- A local shell starts in the folder its cmd (`C:\Users\me>`) or PowerShell (`PS C:\Users\me>`) prompt showed last; File > Open local shell does the same from a local pane.
- When the directory is not known (a prompt FeroSSH does not read, a Telnet pane), the new shell starts in the host's start directory if it has one (below), else where it always starts.

## Connecting

Ctrl+Shift+N (File > Connect to...) asks for a host in a new tab. It takes `user@host`, `host:2222`, a `Host` alias from `~/.ssh/config`, a saved connection's name, `telnet://router` or a COM port such as `COM3:115200`. File > Reconnect (Ctrl+Shift+R) logs in again and File > Disconnect ends the connection.

File > Open local shell opens a Windows command prompt in a new tab. `ferossh.exe --local wsl -d Ubuntu` (or any other program) opens that instead.

### When the connection drops

If the network drops, the laptop sleeps or the server reboots, the tab stays where it is and FeroSSH logs in again on its own: after 1 second, then 2, 4, 8, 15 and every 30 seconds, for up to 15 minutes. Enter tries at once and Esc stops.

The tab keeps its splits, font, theme, scrollback and log file. Port forwards and the login script start again, tmux reattaches with its tabs and splits, and file transfers carry on where they stopped. Tabs that shared the connection share the new one, so there is still one login. A password you typed yourself is asked again; one from a password source is not.

Only a real drop counts. A shell you exit, a server that ends the session, a login that fails or a changed host key never reconnects. A drop is noticed when the server stops answering keepalives (`ServerAliveInterval` in your ssh_config, 30 seconds by default, `ServerAliveCountMax` 3).

File > Reconnect automatically turns this off for all hosts; `AutoReconnect no` in a host's profile turns it off for that host only.

## The tools pane

The connection manager, session manager, file transfer, port forwards and scripts open in the tools pane at the side of the window. The tools rail along the window's edge has an icon for each, plus the command palette and a pin at the bottom; hover an icon for its name and shortcut. A click opens that tool, and a click on the open tool's icon closes it again.

The terminal slides over the tools pane rather than squeezing it. Drag the terminal's edge to cover part of the tool, or all of it so only the rail is left; the tool keeps its width underneath, so nothing in it rewraps. In File transfer, the buttons at the right edge (Bookmarks, Sync, Follow terminal, Layout, Refresh and the path rows' **...**) move left to stay in view, with the boxes beside them getting shorter, until they would run into the host name. Drag the edge out from the rail to uncover the last tool again, or past the tool's width to make it wider. A tool you open again shows as much as it did last time.

With the pin off (Settings > Keep tools open), an opened tool is a peek: clicking in the terminal, Esc or connecting somewhere covers it again. Settings > Tools rail hides the rail; the tools then open and close from the Tools menu and their shortcuts only, as before.

## The connection manager

Ctrl+Shift+S shows your hosts in the tools pane: those from `~/.ssh/config`, your saved connections and your imports. They are in a Windows tree view, under a search box, with Connect, New, Edit and Import buttons below. Type to search (typing in the tree jumps to the search box; Down goes back to the tree), Enter or a double click connects in this tab, Shift+Enter in a split, and Ctrl+D marks a favourite. Left and Right close and open folders, and hovering a host shows its `user@host:port`. Tab moves between the search box, the tree and the buttons. A `# folder: Name` comment above a `Host` line groups hosts in your ssh_config. Settings > Side pane on the right moves the pane.

### Saved connections

Insert or the New button opens the connection form in the tools pane; F2, the Edit button or a right click edits the selected connection. It is a Windows form of text boxes and drop-down lists, with a tab per section:

- **Connection**: **Name, Folder, Type**; the type is an SSH terminal, Telnet (unencrypted), Serial console, Remote desktop (RDP) or VNC desktop. Then **Host, User, Port, Key file** (with Browse...), the **Password** source (below) and the **Jump host** (see [Jump hosts](#jump-hosts)). A serial console has the port, speed, framing and flow control instead.
- **Look** (a terminal's tab): theme, font, font size and scrollback.
- **Extras** (a terminal's tab): tmux on or off, the tmux session to attach and its layout, the start directory, File transfer's Local and Remote folders, port forwards (such as `L 8080 localhost:80`), RDP and VNC ports, older crypto, and the login script (see [Login scripts](#login-scripts)).

Empty fields show their default in grey. Enter saves, Shift+Enter saves and connects, Esc cancels. Connections go to `%APPDATA%\FeroSSH\sessions.conf` in ssh_config syntax and their look to `ferossh.conf`. Hosts from `~/.ssh/config` and imports show their connection settings read-only, because FeroSSH never writes those files; their look and extras can still change, and giving one a new name saves an editable copy.

### Passwords without typing

Keys and agents need no password at all. The Windows OpenSSH agent pipe also serves the 1Password and Bitwarden SSH agents, whose keys stay in the vault.

For servers that want a password, the form's Password field picks where it comes from:

- **ask every time** (the default): you type it.
- **Windows Credential Manager**: saved the first time a login with it works, as `FeroSSH:user@host:port`, protected by your Windows sign-in, and filled in from then on.
- **1Password**, **Bitwarden** or **KeePassXC**: fills in a command for that tool's command line (`op`, `bw`, `keepassxc-cli`) that you can edit.
- **a command of my own**: `%n` is the connection's name, `%h` the host, `%u` the user, `%p` the port.

A command runs without a shell, a window or any input, for at most 60 seconds, and the first line it prints is the password. FeroSSH shows each command and asks before it runs it the first time. If the password from a source is refused, FeroSSH asks you to type it. One-time codes are always typed.

### Importing from other clients

File > Import connections lists the clients that have saved sessions on your account: PuTTY and KiTTY (from the registry), WinSCP (registry or `WinSCP.ini`), MobaXterm (`MobaXterm.ini`), mRemoteNG (`confCons.xml`) and SecureCRT (its `Config\Sessions` folder). Pick one to see its sessions, untick what you don't want, and choose Import. *From a file...* reads the same files from a portable install or an export.

Host, user, port, key file, folders, PuTTY and KiTTY port forwards and jump hosts come over, and so do Telnet sessions and (from MobaXterm and mRemoteNG) RDP and VNC sessions. **Passwords never come over**: the preview marks the sessions that had one, so you can give them a password source. The preview also says what else does not come over. Imports go to `%APPDATA%\FeroSSH\imported-<client>.conf`, in a folder named after the client, and importing again from the same client replaces that file. A name already in use is left out.

## The session manager

Ctrl+Shift+L lists the sessions that are open right now, in every tab: their host, kind (SSH, Telnet, serial, tmux, desktop or local shell), tab and state (connected, connecting, reconnecting or ended), with notes such as tmux, forwards, logging or a jump host. It also shows the hidden sessions of a [multi-host script run](#runs-on-many-hosts). It is a Windows list with columns (session, kind, tab, state, details) and updates as sessions connect, drop and reconnect. Type to filter, Enter or a double-click goes to the session's tab and pane, F2 renames it in place, and Del closes it after asking. The Show, Rename and Close buttons below the list, and a right-click on a session, do the same.

The connection manager is for the hosts you *can* connect to; the session manager for the ones you *are* connected to.

## Jump hosts

To reach a host that is only reachable through another one, give it a jump host: `ProxyJump bastion` in your ssh_config, or the *Jump host* field in the connection form. Several go in turn, separated by commas, as `ssh -J` takes them: `admin@gw:2222,inner`.

Each jump host logs in as if you connected to it, with its own host key check and its own key or password, in a sandboxed worker of its own. The host behind it is then reached through that login, encrypted end to end, so a jump host never sees what you type on the target. If a jump host cannot reach the next one, the pane says which one and why.

`ProxyCommand` in your ssh_config runs a local program (for example `ncat --proxy proxy.example.org:8080 %h %p`) and sends the connection through it. FeroSSH asks once before it runs a command line it has not run before, and shows what the program printed when it fails. `ProxyJump none` or `ProxyCommand none` turns either off for a host.

## Telnet and serial consoles

**Telnet** is for old switches, routers and console servers that have nothing better. Type `telnet://host[:port]` or `telnet host port` at the connect prompt, or make a connection of type *Telnet (unencrypted)* (port 23 unless you give one). Telnet sends everything in the clear, passwords included, so the pane says so when it connects and the tab carries a red **⚠ telnet** badge. It runs in a sandboxed worker like SSH. SFTP, port forwards, tmux and remote desktops need SSH; ZMODEM, logs, themes, login scripts and broadcast input work.

**Serial consoles**: type a COM port at the connect prompt (`COM3`, `COM3:115200`, `COM3:9600,7E1,xonxoff`) or on the command line, or make a connection of type *Serial console* with its port, speed, framing (8N1, 8E1, 8O1, 7E1, 7O1, 8N2) and flow control (none, XON/XOFF, RTS/CTS, DSR/DTR). Without them it uses 9600 8N1 and no flow control, the usual console default. Session > Send break holds the line in break for 300 ms (Cisco ROM monitor, Linux SysRq). Unplugging a USB adapter ends the session with a note; Enter opens the port again.

## Login scripts

Some devices want answers after the login: a second user name, `enable`, `terminal length 0`. A connection's login script, in the Extras section of the form, does that. One step per line:

| Step | Does |
| --- | --- |
| `expect Username:` | Waits up to 15 seconds for that text. |
| `send terminal length 0` | Types the line and Enter (`send` alone is just Enter). |
| `password` | Types the connection's login password, from its password source. |
| `secret enable` | Types a named secret (see below). |
| `pause 2` | Waits 2 seconds. |

A login script can use the logic steps of a script, without the `@`: `on`, `goto`, `if`, `timeout` and the others below (see [Logic](#logic-expect-style)), so one script can answer an `enable` password or a second login only when the device asks. It types into its own pane only, so `pane` and `tab` moves are for scripts, a line to type needs `send` in front, and variables such as `${host}` are not filled in.

The script runs once after an SSH or Telnet login, or when a serial port opens; tmux attaches after it. Pressing any key stops it, and Session > Run login script again starts it over. What it types is never broadcast.

Secrets are never written to a FeroSSH file. The first time, the pane asks for the secret (without echo) and keeps it in Windows Credential Manager as `FeroSSH secret:<name>`. Or a `Secret` block in `ferossh.conf` names a password manager command for it (see [Settings](settings.md#secrets)).

## Scripts

Ctrl+Shift+Space (Tools > Scripts) lists named command lines, grouped under their categories, with each script's hotkey, auto-response text and first line, and the selected script's lines under the list. Type in the search box (or just start typing in the list) to search names, categories and lines, then Enter, a double-click or Run types the script with Enter after each line, or Shift+Enter leaves the last line for you to finish. Left and Right jump between categories, and a right click offers the same as the buttons. With broadcast input on, it goes to every broadcast pane, so one script works several terminals at once.

Insert or New adds a script (what you typed as a search becomes its name), F2 or Edit edits one, Del or Delete removes one, and F5 or On hosts runs it on saved hosts you pick. The editor has a field each for the name, category, hosts, folders, hotkey and *When* text, and a box for the lines below them; Save (or Enter in a field, or Ctrl+Enter in the lines) stores it, and Cancel or Escape leaves it. When something is wrong the reason shows above the buttons and the keyboard goes to the field or line at fault. Edit > Save selection as script starts one from text selected in a pane.

**Categories.** The editor's *Category* field files a script under a category, such as `Linux` or `Network/Cisco` (a `/` makes a category within a category, up to four deep). A new script starts in the category of the one selected.

**Enter or not, per line.** In the editor, Enter ends a line, and that line is typed with Enter after it. Shift+Enter ends a line that is typed without Enter: it becomes an `@type` line, so a script can fill in a command and leave it for you to check, or type a password prompt's answer only after a `@key`.

**Arguments.** `$1` to `$9`, or `${1:Gi0/1}` with a default, are asked for in the side pane before the script is typed, prefilled with the defaults, with a preview of what will be typed. Each argument has its own box: Enter goes to the next box and runs the script from the last one, Shift+Enter runs it from any box, and Escape cancels. `$$` types a plain `$`; other `$` text such as `$HOME` is left alone.

**Variables** fill themselves in: `${session}`, `${host}`, `${user}`, `${port}`, `${date}`, `${time}`, `${stamp}` (safe in file names) and `${clipboard}`. For example `copy run tftp://10.0.0.5/${host}-${stamp}.cfg`.

**Steps.** A line starting with `@` is a step rather than text:

| Step | Does |
| --- | --- |
| `@expect (config)#` | Waits for the host to print that. |
| `@capture Version *,` | Keeps the value where the `*` is from the reply, for the report. |
| `@pause 2` | Waits 2 seconds (`@pause 500ms` is a delay). |
| `@delay 300ms` | Waits 300 milliseconds (`@delay 1.5s` works too; 10 ms to 60 s), for timing between keys and panes. |
| `@key enter` | Presses keys: `enter`, `space`, `tab`, `escape`, `backspace`, `up`, `down`, `left`, `right`, `home`, `end`, `insert`, `delete`, `pageup`, `pagedown`, `f1` to `f12`, `ctrl+z`, `alt+x`, or a single character. `ctrl+`, `alt+` and `shift+` combine with the arrows, Home, End, Insert, Delete, Page Up/Down and F-keys (`alt+right`, `ctrl+left`, `ctrl+shift+f5`), and `shift+tab` works too. Several keys go in one step, and a number repeats the key before it: `@key down 3 enter`, or `@key ctrl+b d` for tmux. |
| `@type y` | Types text without Enter. |
| `@password`, `@secret enable` | Types the login password or a named secret. |
| `@pane right` | Moves to the pane `left`, `right`, `up` or `down` of this one, like Alt+arrow; the rest of the script is typed there. `@key alt+right` instead sends Alt+Right to the host. |
| `@tab next` | Moves to the `next` or `prev` tab; the rest of the script is typed in its focused pane. |

`@@` types a plain `@`.

### Logic (Expect-style)

Like Expect or Tera Term macros, a script can wait for one of several answers, branch, loop and keep values. These steps never type anything themselves:

| Step | Does |
| --- | --- |
| `@on [confirm] goto confirm` | Waits for one of several texts: the `@on` lines in a row wait together, and the first text the host prints goes to its label. An `@on` without `goto` goes on after the last of them. |
| `@label confirm` | A place to go to. Labels are letters, digits, `_` and `-`, each used once. |
| `@goto login` | Goes on at that label. |
| `@timeout 60` | How long the `@expect`, `@capture` and `@on` steps after it wait (15 seconds until set, up to 3600). |
| `@timeout 30 goto slow` | The same, and a step that times out goes to the label instead of stopping the script. |
| `@set model ${captured}` | Sets a variable, used as `${model}`. `"..."` keeps spaces or sets it empty. |
| `@add tries 1` | Adds a whole number (an unset variable is 0). |
| `@if ${tries} > 3 goto give_up` | Goes to the label when the comparison holds: `==`, `!=`, `<`, `<=`, `>`, `>=` (numbers) or `contains`. |
| `@show Done on ${host}` | Shows a line in the pane, dimmed; nothing is typed. |
| `@stop` | Ends the script. |
| `@fail no route to ${target}` | Ends it as failed: the reason shows in the pane and in the report. |

`@capture` keeps its value in `${captured}` too, so a later line can type it or test it. A variable holds one line of text: what the host prints can fill in a value but never becomes a key, a step or a second line.

For example, to back up a switch's configuration, answering its questions in whatever order they come and saying how it went:

```
@timeout 30 goto slow
copy running-config tftp://10.0.0.5/${host}.cfg
@label ask
@on Address or name of remote host goto confirm
@on Destination filename goto confirm
@on bytes copied goto done
@on Error goto failed
@label confirm
@add answers 1
@if ${answers} > 5 goto failed
@key enter
@goto ask
@label done
@show Saved ${host}
@stop
@label failed
@fail the copy failed on ${host}
@label slow
@fail no answer within 30 seconds
```

A script with logic needs no extra program: it runs in FeroSSH itself, on what the pane shows, and can only type into its own pane. A loop that runs 1000 steps without waiting for the host, a pause or a delay stops with a message, and any key you press stops a script. The editor checks that every `goto` has its label before it saves. Logic and `@pane`/`@tab` moves do not mix in one script; a `@capture` before a move does, and its `${captured}` is filled in after it.

For example, to start a capture on one host and a ping from the host beside it:

```
tcpdump -ni eth0 icmp
@delay 1s
@pane right
ping -c 3 10.0.0.1
```

A script with `@pane` or `@tab` runs from one pane: with broadcast input on it says so instead, and a run on picked hosts (F5) skips those steps.

**Hosts, folders and hotkeys.** In the editor a script can be kept to some hosts (`sw-* core-?`) or connection-manager folders. Scripts for the focused pane's host are listed first and the others dimmed; the command palette shows only the ones that apply. A script can have a hotkey, Ctrl+Shift+F1 to F12.

**Report.** Run on several panes, or with `@capture` steps, a script shows a live report in the side pane: one row per host with its state and captured values. Enter shows that host's pane, C copies the report as a table for a spreadsheet, and S saves it as CSV in the download folder.

### Runs on many hosts

F5 on a script picks saved SSH connections to run it on, without opening them first. Tick the connections in the list, grouped by folder with each folder's picked count; Space ticks the selected one, the search box narrows the list, and Pick shown or Clear shown ticks or clears every connection the search keeps. The hosts the script is kept to come picked. Enter or Run starts the run, Escape goes back to the scripts. *At once* sets how many run together (4 by default, 1 to 16), and a password typed for the run is kept in memory only.

Each host logs in in a session of its own that no tab shows, with its own sandboxed worker (or a new channel on a connection already open). The report shows each host queued, logging in, running, done or failed, and Enter on a row brings that session into a tab, live. A run never accepts a host key and never asks a question: a host with an unknown key, or a login that wants a password or a code, fails with what it asked, and you can show it and answer by hand. Each host gets 2 minutes. Esc or Stop run stops the run, R or Retry failed runs the failed hosts again, C or Copy copies the report as a table and S or Save CSV saves it, and closing the report ends the sessions it opened. Production hosts in a run are confirmed once, all listed.

### Auto-responses

A script's *When* text makes it answer by itself whenever the host prints that text, on the hosts it is kept to: for example `@key space` when `--More--` appears, or `@key enter` on `[confirm]`. An auto-response only types lines and keys you wrote, never a password, a secret, the clipboard or arguments, and after 20 answers in one second it pauses until you type. Session > Auto-responses from scripts turns them all off.

### Production guard

A script about to be typed into a pane with the red Production theme asks first, listing the hosts and the lines. Session > Confirm scripts on Production hosts turns that off.

### The scripts folder

Each script is a file of its own in `%APPDATA%\FeroSSH\scripts` (Tools > Open scripts folder), named after the script and ending in `.script`, and the folder it is in is its category:

```
scripts\
    Disk use.script
    Network\
        Show interfaces.script
        Cisco\
            Save config.script
```

Scripts used to be called snippets. The first time this version starts, it renames the old `%APPDATA%\FeroSSH\snippets` folder to `scripts` (when a `scripts` folder is there already, the old files move into it, and one whose name is taken there stays behind in `snippets`). `.snippet` files are still read as they are; one is saved as `.script` the next time you edit it.

To take scripts to another PC, copy the files or folders into that PC's scripts folder; FeroSSH reads the folder again each time the scripts pane opens. The files are plain text (see [Settings](settings.md#scripts)), so they can go in git or be shared with a team. Scripts an older FeroSSH kept in `ferossh.conf` move to the folder by themselves at the first start, and a copy of the old file stays as `ferossh.conf.before-snippets-folder`.

### Importing scripts

File > Import scripts lists what other programs on this PC have, with a box per script and its first lines, so you can check them before anything is written. Imported scripts get a file each in the scripts folder; one whose name is already taken in its category is left out.

| From | What comes over |
| --- | --- |
| Windows Terminal | `sendInput` actions in `settings.json` (named ones, with nested command menus as categories), and the `snippets` of a `.wt.json` file. |
| SecureCRT | The Send String buttons of its button bars (`ButtonBarV5.ini` or `ButtonBarV4.ini`), a category per bar. `\r` becomes a line, `\p` a one-second `@pause`. |
| MobaXterm | The macros in `MobaXterm.ini`. MobaXterm does not document how it stores them, so check the lines on the import page. |
| A file | Another FeroSSH's `.script` or `.snippet` file, or any text file as one script, a line per line. |

Keystrokes come over as steps: text without an Enter becomes `@type`, Tab, Escape and Ctrl+ keys become `@key`, and a `$` or a leading `@` is written so that it types as it is.

### Recording a script

Edit > Record keystrokes as script records what you type in the focused pane until you pick it again, then opens it in the script editor to name and check. Backspace, Ctrl+U and Ctrl+W edit lines as a shell does, Tab and Ctrl+ keys are kept as `@key` steps so tab completion replays, and a line the host did not echo (a password) is recorded as `@password`, never as its text.

## Standby window

A hotkey can hide a window and bring it back, so a terminal (or any other program) is one key away without taking a taskbar button. It is off until you turn it on, under Window > Standby window (hotkey):

- **Pick the window.** Window to show and hide lists *This FeroSSH window* and the open windows of other programs, each with its program (`notepad.exe`, `ferossh.exe` for another FeroSSH window). FeroSSH remembers the program, not the single window: after a restart, or when that window closed, it uses the program's front window.
- **Pick the hotkey.** Pick one of the hotkeys listed (Win or Ctrl with the key below Esc, Ctrl+Alt+Space, Scroll Lock, Pause) or choose Press a new hotkey... and press the one you want. A hotkey needs Ctrl, Alt or Win, except Pause, Scroll Lock and F13 to F24; FeroSSH refuses Ctrl+Alt with a letter, because that is AltGr on many keyboards. Picking a window or a hotkey turns standby on.
- **See whether it is active.** The first line of the menu says *on* and for which window, *off*, or *not active, … is taken* when another program (or another FeroSSH window) already has that hotkey. Click it for the details. Standby hotkey on is ticked while this FeroSSH window holds the hotkey.
- **Turn it off.** Click Standby hotkey on again; the hotkey is released at once.

Pressing the hotkey hides the window when it is in front, and brings it to the front when it is hidden, minimised or behind another window. While hidden it has no taskbar button and only the hotkey brings it back; it keeps running. Another program's window is never left hidden: FeroSSH shows it again when you turn standby off, pick another window, or close FeroSSH, and at its next start if it ended without doing so. Windows does not let FeroSSH hide a program run as administrator unless FeroSSH runs as administrator too; FeroSSH says so when that happens.

## The command palette

Ctrl+Shift+A (Tools > Command palette) is one search box over every menu item, every saved connection and every command script. Type a few letters in any order: `splr` finds Split right, `core` your core switch, `ctrl+shift+n` the item on that key. Enter runs the item, connects or types the script; Shift+Enter connects in a split or types a script without its last Enter. Each item shows its shortcut and check mark, greyed-out items are left out, and your last few picks come first.

## Broadcast input

Window > Broadcast input to this tab's panes (Ctrl+Shift+B) sends what you type and paste in the focused pane to the other panes of the tab too; Broadcast input to all tabs reaches every pane. Each pane receiving it is framed in amber and the window title says broadcast is on. Window > Leave this pane out of broadcast keeps one pane to itself.

Nothing typed at a password prompt or the connect prompt is broadcast, and a pane waiting at one never receives other panes' keys. A paste with line breaks always asks first while broadcast is on, and says how many panes it goes to.

## File transfer

Ctrl+Shift+E opens File transfer in the tools pane: a **Local** list of a folder on your PC and a **Remote** list of the focused pane's host over SFTP, on the same connection. Both are Windows file lists with icons, size and date (and the rights on the server). Select several files and folders with Ctrl or Shift as in Explorer, or Ctrl+A for all.

- **Moving files.** The arrow buttons between the lists upload the Local selection or download the Remote selection; they point the way the files go. Ctrl+Enter does the same from either list, and Enter or a double-click on a file sends that file across. You can also drag the selection from one list to the other, onto the list for its folder or onto a folder in it, drag the Local selection onto a terminal pane to upload it to that shell's current folder, and drop files or folders from Explorer on the Remote list (or on a terminal pane, for the shell's current folder), after a confirmation.
- **Folders.** Enter or a double-click opens a folder; Backspace or the ↑ button goes up (above a drive comes the list of drives). Type a path in the box above a list and press Enter to go there. ⌂ goes to your home folder on the server, and ... picks the Local folder. Downloads go to the Local list's folder; FeroSSH remembers it (`DownloadFolder`), and until you pick one it is your Downloads folder.
- **Filter.** The box at the top shows only the names in both lists that contain what you type (any case), or match a pattern such as `*.log`. Escape clears it.
- **Bookmarks.** ★ Bookmarks ▾ opens a bookmark: its Local folder in the Local list and its Remote folder in the Remote list. *Add bookmark here...* keeps the two folders shown under a name you type (Enter saves, Escape cancels). Tick *Only for this host* to see it only on this connection; a bookmark with a Remote folder starts out that way. Rename and Delete are in the same menu.
- **Sync browsing.** With ⇆ Sync on, opening a folder in one list opens the folder of the same name in the other, and going up goes up in both. Typing a path or opening a bookmark moves only that list. Both lists move or neither: the Local list waits until the Remote folder has opened. When the other side has no such folder, or the Remote folder will not open, neither list moves (if the Local folder fails to open after the Remote one did, the Remote list goes back) and the line under the lists says so.
- **A connection's folders.** *Local folder* and *Remote folder* in a connection's Extras (`LocalFolder`, `RemoteFolder`) are where File transfer starts for that host: the Remote list opens there the first time, before following cd takes over at the shell's next `cd`, and the Local list opens there whenever File transfer turns to that host (without changing your download folder).
- **Layout.** Layout ▾ puts the lists side by side or one above the other, and swaps Local and Remote. Drag the gap between the lists to give one more room. FeroSSH remembers it (`TransferLayout`).
- **↻ Refresh** reads both lists again (F5 does too, anywhere in the pane outside the lists; in a list it reads only that list).
- **Right-click** a list for Open, Upload or Download, Delete (on the server, after asking; Del too), Refresh (F5), Home folder, Upload files... and Open in Explorer.

**Queue.** The transfer queue under the lists shows each transfer's progress. Right-click one to pause, resume, cancel (Del too) or retry it, or to clear the finished ones. Three files move at once (`ParallelTransfers`). A dropped connection pauses the queue, and it carries on from where each file stopped when you log in again.

**Safe file names.** An existing file is never overwritten without asking: Overwrite, Skip or Keep both, above the queue, with For all to answer the same for the rest. Server names Windows cannot hold (`CON`, `a:b`, trailing dots) are made safe or refused, and a folder download does not follow links out of the folder. Partial files are `.part` locally and `.filepart` on the server until complete. Session > Verify transfers compares SHA-256 sums on both ends after each file. A resumed transfer is always checked this way before its partial file becomes the file, even with that setting off: a mismatch fails the transfer and deletes the partial file, and if the server cannot give a sum (it needs `sha256sum` and `xargs`) the transfer fails and keeps the partial file. Delete it to start over.

### Following cd

The file view, and files dropped on a pane, follow the shell's current directory. Shells that report it with OSC 7 (fish does by default) work anywhere. Otherwise FeroSSH reads it from a `user@host:dir$ ` prompt, bash's default on Debian and Ubuntu. For other prompts, add this to `~/.bashrc` on the server:

```
PROMPT_COMMAND='printf "\e]7;file://%s%s\e\\" "$HOSTNAME" "$PWD"'
```

Settings > File transfer follows cd, or the Follow terminal button in File transfer, turns it off.

### ZMODEM

On a host without SFTP, such as a console server or a Telnet device, you can use ZMODEM (`rz` and `sz`) instead. Turn on Session > ZMODEM: answer rz and sz; it is off by default, so their output shows as it is. Then `sz file` on the server saves the file in your download folder (as `name (2)` if one is there already), and `rz` opens a file picker for what to send. Progress shows in the pane, and Ctrl+C cancels. Files over 4 GB need SFTP.

## Port forwards

`LocalForward`, `RemoteForward` and `DynamicForward` in your ssh_config, or forwards in a saved connection's Extras, start when you log in. Tools > Port forwards (Ctrl+Shift+P) opens a pane with a form at the top and a list of every connection's forwards and X11 under it, in columns: connection, type, listen, to and state. The line under the list shows the selected forward's open connections, why it stopped and its last error. Everything there works on a live connection, without reconnecting:

- **Add**: pick the type (Local `-L`, Remote `-R` or SOCKS `-D`), type the port to listen on (`8080`, or `127.0.0.1:8080`) and where it goes (`db.internal:5432`; a SOCKS forward goes anywhere), then Add or Enter. The On box picks the connection; its first entry is the focused tab's. The Listen box also takes a whole `-L 8080:db.internal:5432`, `-R 9000:localhost:3000` or `-D 1080`.
- **Change**: Edit, Enter, F2 or a double click on a forward puts it in the form; Save changes its port or target, and Escape leaves it as it was. A new target on the same port keeps the open connections.
- **Retry**: Retry (or the right-click menu) starts a stopped forward again.
- **Stop**: Stop, Del or the right-click menu on a forward, or `-KL 8080`, `-KR 9000`, `-KD 1080` in the Listen box by port, as `~C` does in OpenSSH.

Changes last through the tab's reconnects; the saved connection is left as it was. A dynamic forward is a SOCKS 4, 4a and 5 proxy. Local listeners take `127.0.0.1` and `::1` unless you give an address (`*:8080`, or `GatewayPorts yes`). A tab whose connection forwards shows `⇄`, in amber when the server can reach back into your PC (remote forwards or X11).

## X11 forwarding

`ForwardX11 yes` for a host in your ssh_config sends X programs on the server to an X server on Windows, such as VcXsrv or X410; FeroSSH ships none, and the X server must listen on TCP. The display is `X11Display` in `ferossh.conf`, else `DISPLAY`, else `localhost:0`. The server only gets a fake cookie; FeroSSH puts the real one from `%XAUTHORITY%` or `~\.Xauthority` in its place. X11 is off unless a host turns it on, because X programs on the server can see and type into your other X windows.

## tmux

Session > Attach tmux types `tmux -CC new-session -A -s main` at the pane's shell (typing `tmux -CC` yourself works too). tmux's windows and panes become native FeroSSH tabs and panes: no status bar, no prefix key, and native scrollback (with tmux's history in it), copy, paste and search. Splits, new tabs and closing panes go to tmux.

Session > Detach tmux, closing FeroSSH or losing the network leaves everything running on the server. Attach again, from this PC or another, and the tabs, splits, fonts and themes come back; after a dropped connection it reattaches by itself. Session > Switch tmux session lists the other sessions on the server (also offered right after attaching).

There are two layouts, kept per session:

- **One window per pane** (the default; tmux 3.3 or later): every native pane is its own tmux window, so each pane can have its own font, text size and theme. FeroSSH keeps the splits and looks in the session. A plain tmux client shows the panes as separate windows.
- **tmux's own splits** (untick Session > tmux: one window per pane, or older tmux): each tmux window is a tab and tmux's splits are its panes, one font per window.

tmux is never started unless you ask. To attach on every login to a host, set `Tmux` (and optionally `TmuxLayout`) in its profile, or switch tmux on in the connection form's Extras (click it, or press Left or Right) and optionally give a tmux name (`main` when left empty). GNU screen has no control mode and runs as an ordinary terminal.

To have a host's shells start in one server directory, `cd` there and use Session > Start new tabs and panes in this directory (or fill in Start dir in the connection form's Extras). FeroSSH types the `cd` after every login and uses it for the first window of a new tmux session. New tabs and panes opened from a pane follow that pane's directory instead, and use the start directory only when that one is not known. Clear Start dir in the form to go back to the home folder.

If your fingers know tmux's keys, tick Session > Use tmux keybindings for tabs and panes. In a pane of a tmux session, your tmux prefix (Ctrl+B unless your tmux config sets another; FeroSSH asks tmux) followed by `c`, `%`, `"`, `n`, `p`, `0`-`9`, `o`, an arrow, `x`, `&`, `!`, `d` or `s` works the native tabs and panes, which are tmux windows and panes. Arrows, Ctrl or Alt+arrows (resize), `[`, PageUp and PageDown repeat as in tmux: after the first one, press them again without the prefix within tmux's `repeat-time` (half a second unless your tmux config sets another), or hold the key down. Press the prefix twice to send it to the program, and the prefix then `?` for the full list. It is off unless you turn it on.

## Remote desktops

Session > Open remote desktop (Ctrl+Shift+D) shows the Linux desktop (X11 or Wayland) of the host you are logged in to. The server needs an RDP server: xrdp (`sudo apt install xrdp`) or GNOME Remote Desktop. FeroSSH checks that something answers on the server's RDP port (3389, or `DesktopPort`), forwards a loopback port to it through SSH (33900 to 33999, the same for the same host each time) and opens Windows' Remote Desktop Connection there. The server's RDP port can stay closed on its firewall, and the tunnel closes with the connection.

A saved connection of type *Remote desktop (RDP)* without *Via SSH* opens Remote Desktop Connection straight to the machine, for Windows PCs and servers. With *Via SSH* set to another saved connection, FeroSSH logs in there first and tunnels the desktop through it.

Saved desktop and remote app connections (types *Remote desktop (RDP)*, *VNC desktop*, *Virtual desktop (Xvfb)* and *Remote app* with *Via SSH*) open on their own: you get only the desktop or the program's windows, no terminal tab. The SSH login to the *Via* host runs out of sight; its terminal shows in a tab only when the login needs you (a password, a new or changed host key, a failure), and closes once the desktop or program is up. The connection stays while the desktop tab, Remote Desktop Connection or the program runs, and ends with the last of them. Until then the Session manager lists the login and can show it.

`ferossh-desktop.exe` adds Session > Open remote desktop in a tab (RDP) and (VNC), which show the desktop in a FeroSSH tab or split, keyboard and mouse included. For RDP the tab asks for the user name and password; for VNC the server needs a VNC server such as x11vnc, TigerVNC or wayvnc. The viewer runs in a sandbox with no network access of its own and only connects through the SSH tunnel. The built-in RDP viewer only opens desktops on the SSH server itself; a saved RDP connection to another machine behind it opens in Remote Desktop Connection, which checks that machine's certificate. When a VNC server takes a password, FeroSSH uses it even if the server would also let it in without one; a VNC server that asks for none gets a warning in the tab, and a virtual desktop's, which FeroSSH started with a password, is not opened at all, since something between may have removed the password from its offer. A saved VNC connection whose *Host* is another machine than its *Via* SSH server asks before it opens, once each time FeroSSH runs and again when its host, port or SSH server changes: SSH encrypts only the way to the SSH server, and VNC travels unencrypted from there. The desktop takes the size of its pane where the server can change it, and is scaled otherwise.

#### Desktop clipboard

Copied text can go both ways between remote desktops and Windows, text only, up to 1 MiB: what you copy on the desktop lands on the Windows clipboard, and what you copy on Windows reaches the desktop when you click or type in its tab (only the desktop you are working in sees it). It is **off until you turn it on**: copied text can carry passwords and other secrets, and a desktop can hand Windows anything, so choose *Both ways* or one way in Session > Desktop clipboard. The change applies at once to the desktops already open; a saved connection can narrow it further, never widen it, with *Desk clip* in the connection form (`DesktopClipboard`). It covers:

- **Desktops in a tab** (`ferossh-desktop.exe`): RDP over the clipboard channel (plain Unicode text, no files or pictures), VNC as the protocol has it (Latin-1: other characters arrive as `?`). The sandboxed viewer reads what the server sends; the window takes text only the ways the setting allows.
- **Virtual desktops**: the server is told too (x11vnc `-seldir`/`-nosel`, Xvnc `-AcceptCutText`/`-SendCutText`), so it holds back what should not leave it, whatever viewer you use. *Also share mouse selections from virtual desktops* in the same menu (off by default) sends text you only select there too (how xterm copies). TigerVNC's Xvnc, which a virtual desktop that follows its pane uses, shares the clipboard reliably both ways; x11vnc's own clipboard handling is less dependable.
- **Remote Desktop Connection** (Open remote desktop): it shares the clipboard both ways or not at all, so any setting other than *Both ways* turns its clipboard off. FeroSSH then opens it with a connection file that says so.
- **Your own VNC viewer** for a virtual desktop in `ferossh.exe`: TigerVNC Viewer gets the same ways; for the others the server's limits apply.

### Virtual desktops (Xvfb)

Session > Open virtual desktop (Xvfb) gives you a graphical desktop on a server that has no screen and no RDP server: a headless box, a VM, a build machine. Over the SSH connection FeroSSH starts Xvfb (a virtual X display that exists only in memory) and x11vnc on the server, runs a desktop session in that display, and shows it through the SSH tunnel. Nothing is installed or left behind on your PC, and no X server runs on Windows.

The server needs `Xvfb`, `x11vnc` and `xauth`. If one is missing, FeroSSH says which and how to install it (Debian and Ubuntu: `sudo apt install xvfb x11vnc xauth`; Fedora: `sudo dnf install xorg-x11-server-Xvfb x11vnc xorg-x11-xauth`, with EPEL on RHEL; Arch: `sudo pacman -S xorg-server-xvfb x11vnc xorg-xauth`).

What runs in the display is your `~/.xsession` if you have one, else the first desktop it finds (Xfce, MATE, LXQt, LXDE, Plasma, `x-session-manager`), else an `xterm`. To run something else, such as one program, set *Virtual desk* in the connection's settings (F2), for example `firefox` or `startxfce4`; `VirtualDesktop` in [Settings](settings.md). The display takes the size of the pane you opened it from (see [Desktop size](#desktop-size)).

A virtual desktop can also be a saved connection: in the Connection manager, make a new connection of type *Virtual desktop (Xvfb)*, pick the SSH connection it runs on under *Via SSH*, and optionally the program, size and clipboard. Opening it logs in there and starts the desktop, with no terminal tab (see [Remote desktops](#remote-desktops)).

It is locked down on the server too: the display has its own random cookie, so other users there cannot draw on it or read its keys; x11vnc listens on the server's loopback only and asks for a fresh random password, which FeroSSH gives it on its standard input, so it never shows in `ps`. In `ferossh-desktop.exe` the desktop opens in a FeroSSH tab and the password is sent for you. In the standard `ferossh.exe` FeroSSH opens the VNC viewer installed on your PC (TigerVNC, TightVNC, RealVNC or UltraVNC) on the tunnel and prints the password in the terminal for you to type; without a viewer it prints the address and password.

The virtual desktop stops, and Xvfb, x11vnc and everything started in the display end, when you close its tab, choose Session > Stop virtual desktop, close the connection, or end the desktop session (log out, or close the one program). Choosing Open virtual desktop again while one runs opens another viewer on the same desktop.

### Remote apps (native windows)

Session > Remote apps > Run remote app... starts one Linux program on the host you are logged in to and shows its windows as ordinary windows on your Windows desktop, not as a desktop in a tab: each has its own taskbar button, moves and resizes like a local window, and copy and paste can work both ways once you turn the clipboard on (see below). It uses [FeroStream](https://github.com/jkobierczynski/FeroStream). The command palette opens with this connection's program, the ones you ran lately and `xterm`; type any other command line (`gimp ~/a.png`, `xterm -T 'my term'`) and press Enter. Only quotes are understood, no other shell syntax: the program runs directly.

There is no second login. The FeroStream agent runs on the same SSH connection as the pane, as one more channel of it, and runs the program in an Xvfb display on the host (`sudo apt install xvfb`; Fedora: `xorg-x11-server-Xvfb`). When the connection drops while the program still runs, its windows stay and fill again once FeroSSH has reconnected; when you close the pane or its connection, the program keeps running on the host, and Session > Remote apps > Reattach remote app brings its windows back, from this PC or another.

The host needs the agent. When it has none, or another version, and Session > Remote apps > Copy the agent to hosts is ticked (the default), FeroSSH copies its own over the same channel to `~/.local/bin/ferostream-agent` (`RemoteAppAgentPath`), checked by size and checksum, readable only by you. FeroSSH takes the agent from the folder `ferossh.exe` is in: download `ferostream-agent` (for x86-64 hosts) and `ferostream-agent-aarch64` (64-bit ARM, such as a Raspberry Pi) from the same release page and keep them there. They are static Linux programs, built from the FeroStream code this FeroSSH was built with, and listed in the release's `SHA256SUMS.txt`. For other hosts, build the agent yourself and name it `ferostream-agent-<arch>` as `uname -m` names the machine. Turn copying off to use only an agent installed on the host. With Session > Remote apps > Remove the agent when done ticked (off by default), the agent deletes itself when its last program has ended, so nothing is left on the host. Both choices can be set per connection too (*Copy agent* and *Clean up* under Extras in the connection form).

Copied text can go both ways, up to 1 MiB, text only: what a remote program copies lands on the Windows clipboard, and what you copy on Windows reaches the host when you switch to one of its windows (Ctrl+V there, or middle click and Shift+Insert in xterm). It is off until you choose *Both ways* or one way in Session > Remote apps > Clipboard, which applies at once to the remote apps already running; when copied text should not leave the host, the agent does not send it at all. xterm copies by selecting rather than with a Copy command; tick *Also share mouse selections from the host* in the same menu to get those selections on Windows too (off by default, since every selection would replace the Windows clipboard). A connection can narrow the clipboard further, never widen it: *App clip* under Extras in the connection form, for example off for a host you trust less.

A remote app can also be a saved connection: in the Connection manager, make a new connection of type *Remote app*, pick the SSH connection it runs on under *Via SSH*, and the program under *Remote app*. Opening it logs in there and starts the program, with only its windows on screen (the login shows in a tab only when it needs you).

The host's stream is never read by FeroSSH's window: it is unpacked in a separate decoder process with no rights at all (see [Security](security.md)), and the window checks what comes back again before it draws it.

### Desktop size

Each connection's *Desk size* (F2 in the Connection manager; `DesktopSize` in [Settings](settings.md)) chooses how big its desktops are:

- **Empty or `follow` (the default): the desktop follows its pane.** It starts at the pane's size and is redrawn at the new size when you resize the window, split, or drag a divider. RDP does this with xrdp and Windows; VNC with TigerVNC's Xvnc. Servers that cannot change size (x11vnc) show the picture scaled to the pane. A virtual desktop that follows uses TigerVNC's Xvnc when the server has it (`sudo apt install tigervnc-standalone-server`), since Xvfb's size is fixed; without Xvnc it uses Xvfb at the pane's size and says so.
- **A fixed size such as `1920x1080`:** the desktop always has that size, and the tab scales it to fit the pane. Open remote desktop passes it to Remote Desktop Connection too (`/w:` and `/h:`); without one, Remote Desktop Connection uses its own Display settings.

Saved RDP, VNC and virtual desktop connections take their own *Desk size*; Session menu desktops take the one of the SSH connection they come from.

## Fonts and colour themes

Settings > Font and Settings > Theme change only the focused pane; Ctrl+= and Ctrl+- change its text size, Ctrl+0 resets it. Session > Save font and theme for this host keeps them for next time.

The font menu lists eighteen monospace fonts. Cascadia Mono, Cascadia Code, Consolas, Lucida Console and Courier New come with Windows. DejaVu Sans Mono, Fira Code, Geist Mono, Hack, IBM Plex Mono, Inconsolata, Iosevka, JetBrains Mono, Monaspace Neon, Noto Sans Mono, Roboto Mono, Source Code Pro and Victor Mono are free downloads: a missing one reads "(download)" and opens its official page, and FeroSSH picks it up after a restart. None are bundled, so the exe stays small.

There are forty-two themes, in three columns of Settings > Theme. The dark ones are Nord (the default), Ayu Mirage, Carbonfox, Catppuccin Frappé, Macchiato and Mocha, Dracula, Everforest, GitHub Dark, Gruvbox Dark, Gruvbox Material, Kanagawa Dragon and Wave, Monokai, Night Owl, Nightfox, One Dark, Rosé Pine and Rosé Pine Moon, Snazzy, Solarized Dark, Tokyo Night and Tokyo Night Storm, Tomorrow Night and Tomorrow Night Eighties. The light ones are Ayu Light, Catppuccin Latte, Dawnfox, Everforest Light, GitHub Light, Gruvbox Light, Kanagawa Lotus, Rosé Pine Dawn, Solarized Light, Tokyo Night Day and Tomorrow. The classics are Campbell, One Half Dark, One Half Light, Tango Dark and Tango Light from Windows Terminal, and PuTTY's own colours. Every theme's text reads at 4.5:1 contrast or better. Two more are warnings: **Production** (red) and **Staging** (amber). Rules pick the theme by host pattern or ssh_config `Tag`, so production servers always look like production; hosts tagged `production`, `staging` or `development` get Production, Staging or Nord without any rule. `ferossh.exe --theme Production prod-db` opens a session in a theme, handy for desktop shortcuts. See `ThemeRule` and `Profile` in [Settings](settings.md).

The window around the panes (the menu bar and its menus, the title bar, the tab strip, the tools rail and the tools pane: Connection manager, Session manager, the connection form, File transfer, Port forwards, Scripts) has Windows' light look unless you pick something else in Settings > Window colours: the focused pane's colours (dark for a dark theme, red for Production), Windows' own light or dark mode, or a neutral dark grey. Dark menus, buttons and scroll bars need Windows 10 version 1903 or later; the title bar takes the theme's colour on Windows 11.

## Selecting, copying and scrollback

| To | Do |
| --- | --- |
| Select characters, words, lines | Drag, double-click, triple-click |
| Select a block | Alt+drag |
| Select while a program uses the mouse | Shift+drag |
| Copy / paste | Ctrl+Shift+C / Ctrl+Shift+V (or Ctrl+Insert / Shift+Insert) |
| Open a link | Ctrl+click (FeroSSH shows its real target first) |
| Scroll back / forward a page | Shift+PgUp / Shift+PgDn |
| Jump to the top / bottom | Ctrl+Shift+Home / Ctrl+Shift+End |
| Find in scrollback | Ctrl+Shift+F, then Enter for older, Shift+Enter for newer |

While you are scrolled back, a thin bar at the right edge shows where you are. Edit > Copy on select and Edit > Right-click pastes turn on the PuTTY habits. Scrollback is 10,000 lines unless you change it (`Scrollback`); Edit > Clear scrollback forgets it.

## Logs

Session > Log to file writes what a tab receives to `%LOCALAPPDATA%\FeroSSH\logs`, one file per tab, optionally with a timestamp on each line, and Session > Save this pane's output to a log saves what a pane already holds. Every log is sealed with a chain file, and Session > Check a log for changes tells you whether it was changed later. [Logs](logs.md) explains all of this, including what the `.chain` files are.

## Security keys and passkeys

FIDO2 security keys and passkeys work through Windows' own dialog: touch a USB or NFC key, scan the QR code with your phone, or confirm with Windows Hello. Use key files such as `id_ecdsa_sk` or `id_ed25519_sk` (made with `ssh-keygen -t ecdsa-sk` or `ferossh.exe --new-security-key`), or keys the OpenSSH agent holds. See [Getting started](getting-started.md#command-line).

## Updates

Help > Check for updates says whether a newer release exists and offers to open its page; Help > Check for updates weekly does it once a week, quietly unless there is one. FeroSSH never downloads or installs anything by itself, and without either it never contacts anything but your servers. If you installed FeroSSH with winget, `winget upgrade jkobierczynski.FeroSSH` updates it. Each release is code signed; [Verifying a download](verifying-downloads.md) shows how to check it.
