# Everyday use

This page explains each feature in turn. [Menu reference](menus.md) lists every menu item and key, and [Settings](settings.md) every keyword in `ferossh.conf`.

## Tabs and splits

The window starts as one pane, with no tab strip. Ctrl+Shift+T opens a tab and the strip appears; Alt+Shift+= and Alt+Shift+- split the pane right or down. A new tab or pane on a host you are already logged in to opens another channel on the same connection, so there is no second login.

- Alt+arrows moves the focus, Alt+Shift+arrows grows the focused pane, and dragging a divider resizes.
- Ctrl+Tab and Ctrl+Shift+Tab switch tabs. Drag a tab, or press Ctrl+Shift+PgUp and PgDn, to reorder it.
- Ctrl+Shift+drag a pane onto another pane's edge to move it there, or onto its middle to swap the two.
- To merge a tab back in as a pane, drag the tab down out of the strip onto a pane of the tab that appears, or use Window > Join tab into the tab before it (Ctrl+Shift+J). Window > Move pane to its own tab, or dropping a pane on the tab strip, makes it a tab again.
- Ctrl+Shift+W closes the pane, Ctrl+F4 the tab.
- Ctrl+Shift+H (Window > Hide pane) hides the focused pane without closing it: the other panes take its space and its session keeps running. The [session manager](#the-session-manager) lists it as *hidden*; Show there (or Window > Show last hidden pane) puts it back where it was, or in a tab of its own when the surrounding panes have closed. A hidden pane whose login, or login script, asks for a password, secret or host key shows again by itself. Closing the last pane in the tabs shows the pane hidden last instead of closing the window. In tmux the hidden pane stays hidden when you attach again, from any PC with this FeroSSH version or later (older ones show it as a tab; if every window of the session is hidden, they all show as tabs). Saved workspaces leave hidden panes out.

### New tabs and panes start in the same directory

A split (Window > Split right or Split down) and a new tab (Ctrl+Shift+T) start their shell in the directory the focused pane's shell is in.

- In a tmux session, FeroSSH asks tmux where the pane is and opens the new tmux window or pane there.
- In a plain SSH pane, FeroSSH uses the directory the shell reported (see [Following cd](#following-cd)) and types `cd` to it once, right after the new pane logs in.
- A local shell starts in the folder its cmd (`C:\Users\me>`) or PowerShell (`PS C:\Users\me>`) prompt showed last; File > Open local shell does the same from a local pane.
- When the directory is not known (a prompt FeroSSH does not read, a Telnet pane), the new shell starts in the host's start directory if it has one (below), else where it always starts.

## Workspaces

A workspace is a saved set of tabs and split panes, each pane on a connection, that opens again in one go: the three servers you always watch side by side, or a tab per customer.

- Window > Workspaces > Save layout as workspace... saves the tabs you have open now under a name you give. Each pane keeps the connection it is on, the directory its shell is in, its session name, and its theme and font where you changed them from the host's own.
- Open one from Window > Workspaces, from the *Workspaces* folder in the connection manager (Enter or a double click; F2 opens its file in your text editor; Del deletes it), from the command palette (type its name), or from the command line with `ferossh.exe --workspace NAME`. Its tabs open next to the ones you have; a window that only waits at the connect prompt makes way for them.
- Panes on the same SSH host log in once: the first pane logs in, and the others wait for it and open on its connection.
- A workspace names connections: a pane on a saved connection or an ssh_config host is kept by that name, so editing the connection changes every workspace that uses it. A pane you opened by typing an address (`admin@10.0.0.5:2222`, `telnet://sw1`) keeps what you typed. A workspace never holds a key or password, so its file can go to another PC, into git or a backup; look it over first if the addresses in it are private. Logins work as they do anywhere else: keys, the agent, the connection's password source and its login script.
- tmux panes are kept like any other, each with its split, directory, name and look, and marked with their tmux session. Opening the workspace logs in once and attaches that session: if it still runs on the server you get it as it is now, and if it is gone (after a reboot, say) FeroSSH first makes it again with the saved windows, each in its directory, laid out as saved. With Session > tmux > One window per pane turned off, tmux keeps the splits itself and a session is saved as one pane. Remote desktop tabs are left out.
- Window > Workspaces > Log in automatically (off unless you turn it on) makes a workspace's panes log in by themselves. A connection that would ask for its password asks once, and FeroSSH keeps the password once the login works: in the vault when you use a [master password](#backup-and-sync-between-pcs), else in the Windows Credential Manager, the same as a connection whose password source is the Windows Credential Manager. Keys, the agent and password commands work as always. Only panes a workspace opened do this; a connection you open yourself keeps its own password source.
- A pane can run something once it is logged in, its login script has run and its shell is in its directory: `Script NAME` runs a script from the [scripts folder](#scripts) (its arguments take their defaults) and `Command TEXT` types one line as written and presses Enter, in the same pane, once the script is over there (a script that moves on with `@pane` or `@tab` is over in this pane when it moves). Add them by hand: F2 on a workspace in the connection manager opens its file in your text editor. A workspace file may come from anywhere, so the first time a workspace runs a script or command FeroSSH lists them and asks: Yes runs them and does not ask about them again on this PC (until a script changes), No opens the workspace without running anything this time. A pane with the Production theme asks again before running them when Session > Scripts > Confirm scripts and agents on Production hosts is on. They run once, not again after a reconnect, and not in a pane that shows tmux, which keeps its programs running itself. Saving the layout again keeps them.
- Window > Workspaces > Reopen the last layout at start (off unless you turn it on) keeps the tabs and panes you have open when FeroSSH closes (File > Exit, the window's close button, or Windows shutting down) and opens them again the next time you start FeroSSH without a host or workspace on its command line. It is kept like a workspace, as `last-layout.workspace` in `%LOCALAPPDATA%\FeroSSH` (portable FeroSSH: in the `local` folder of its data folder), so tmux panes attach their session again and Log in automatically applies to it. Closing the last pane forgets it. With two FeroSSH windows open, the first one started keeps its layout. A layout too big for a workspace file is not kept, so the one kept before it opens instead.
- Workspaces are files in `%APPDATA%\FeroSSH\workspaces` (Window > Workspaces > Open workspaces folder), one per workspace, in a plain text format you can also write by hand:

```text
Active 1
Tab
    Split right 60
        Pane connection web1
            Directory /var/log
            Name logs
        Split down 50
            Pane connection db1
                Script Check disks
                Command tail -f /var/log/postgresql/main.log
            Pane local
Tab
    Pane prompt
```

`Split right` puts its two halves side by side and `Split down` one above the other, the number being the first half's share in percent; then come the two halves. A pane opens a `connection` (any name or address the connect prompt takes), a `local` shell or the connect `prompt`, and can have a `Directory`, `Name`, `Theme`, `Font`, `FontSize`, `Tmux` (the tmux session it is a window of), `Script` and `Command` (what it runs once logged in) and `Focus` (the tab's focused pane). `Active` is the tab shown first. Window > Workspaces > Open workspace file... opens one from anywhere.

## Connecting

Ctrl+Shift+N (File > Connect to...) asks for a host in a new tab. It takes `user@host`, `host:2222`, a `Host` alias from `~/.ssh/config`, a saved connection's name, `ssh://user@host:2222` (a link, see [Opening ssh:// links](#opening-ssh-links)), `sftp://user@host/folder` (file transfer only, see [SFTP-only connections](#sftp-only-connections)), `telnet://router`, `winrm://server`, `winrms://server` (WinRM over HTTPS) or a COM port such as `COM3:115200`. File > Reconnect (Ctrl+Shift+R) logs in again and File > Disconnect ends the connection.

File > Open local shell opens a Windows command prompt in a new tab. `ferossh.exe --local wsl -d Ubuntu` (or any other program) opens that instead.

### Your ssh_config

FeroSSH reads `~/.ssh/config` as OpenSSH does: the first value for a keyword wins, `Host` and `Match` blocks apply in order, and `Include` pulls in other files. `Match` takes `all`, `host`, `originalhost`, `user`, `localuser`, `tagged`, `canonical` and `final`, with `!` in front to negate; `Match exec`, `localnetwork` and `version` never match, because FeroSSH does not run commands to decide.

Besides `HostName`, `User`, `Port`, `IdentityFile`, `Tag`, `ProxyJump`, `ProxyCommand` and the forwarding keywords, these work:

| Keyword | What it does |
| --- | --- |
| `CertificateFile` | Offers this OpenSSH user certificate with its key (`<key>-cert.pub` next to a key file is picked up too). |
| `IdentitiesOnly yes` | Offers only the `IdentityFile` keys, even from the agent. |
| `IdentityAgent` | Another agent: `none`, a named pipe such as `\\.\pipe\my-agent`, or `SSH_AUTH_SOCK`. |
| `ForwardAgent yes` | Forwards your agent to the server (see [Agent forwarding](#agent-forwarding)). |
| `GSSAPIAuthentication`, `GSSAPIDelegateCredentials`, `GSSAPIServerIdentity` | Kerberos login (see [Kerberos login](#kerberos-login)). |
| `SendEnv`, `SetEnv` | Sends environment variables (the server's `AcceptEnv` must take them). |
| `RemoteCommand`, `RequestTTY` | Runs a command instead of the login shell; `RequestTTY no` asks for no terminal. |
| `SessionType` | `none` logs in for the port forwards only, with no shell, like `ssh -N` (see [Tunnel-only connections](#tunnel-only-connections)). |
| `StrictHostKeyChecking` | `yes` refuses unknown hosts; `accept-new` (or `no`) trusts a new host without asking. A changed key is always refused. |
| `UserKnownHostsFile` | Other known_hosts files, in place of `~/.ssh/known_hosts`. |
| `HostKeyAlias` | Looks the host key up under this name instead of the host's. |
| `VerifyHostKeyDNS` | Also checks the host key against the host's SSHFP records in DNS (see [Host keys in DNS](#host-keys-in-dns-sshfp)). |
| `AddressFamily` | `inet` for IPv4 only, `inet6` for IPv6 only. |
| `ProxyServer` | FeroSSH's own: a SOCKS 5 or HTTP proxy to go through (see [Proxies](#proxies)). OpenSSH does not know it, so put `IgnoreUnknown ProxyServer` above it in a file OpenSSH reads too. |

### Host keys in DNS (SSHFP)

`VerifyHostKeyDNS ask` or `yes` makes FeroSSH look up the host's SSHFP records (what `ssh-keygen -r host` prints, published in the host's DNS zone) while it connects, and compare them with the key the server offers. The question goes from the connection's sandboxed worker to the DNS servers Windows uses, asking for DNSSEC; an IP address is never looked up.

- A new host shows what DNS said under its fingerprint: the key matches a record (validated with DNSSEC or not), DNS has no record for it, the records name another key (in red), or the lookup failed.
- With `yes`, a key that matches a record the DNS server validated with DNSSEC (the AD bit) is trusted without asking, as in OpenSSH, and is not added to `known_hosts`, so DNS is asked again each time. A match that was not validated, and every match with `ask`, still asks.
- A key DNS contradicts is never trusted on its own: `StrictHostKeyChecking accept-new` asks instead. A key `known_hosts` already trusts still connects, with the warning, and a changed key is still refused, with what DNS says on the warning page.

The AD bit is only as trustworthy as the way to the DNS server: anyone between your PC and that server could set it. Use `yes` only with a DNSSEC-validating resolver you trust on a network you trust; `ask` is safe anywhere. The sandbox cannot reach a resolver on this PC (`127.0.0.1`), so with one of those the lookup fails unless FeroSSH runs with `--no-sandbox`.

### Opening ssh:// links

`ssh://[user@]host[:port]` logs in like `user@host:port`, at the connect prompt or on the command line. To have links on web pages, in wikis, inventories or chat open in FeroSSH, turn on Settings > Open links > Open ssh:// links with FeroSSH (and the sftp:// one for [SFTP links](#sftp-only-connections)). FeroSSH has nothing to install, so this is off until you turn it on, and it is written for your Windows account only (under `HKEY_CURRENT_USER`); the same item turns it off again and takes out what it wrote. If you move `ferossh.exe`, turn it on again from the new place.

In a terminal, Ctrl+click an `ssh://` or `sftp://` link, written as plain text (`echo ssh://user@host`) or marked by a program, to open it in a new FeroSSH tab after you confirm. This works without the Settings item, and such a link never goes through Windows. Plain-text `http://` and `https://` links open the same way, in your browser.

A link only ever names a host: a path, a query, options, a second host, a user starting with `-` or anything a shell would read as more is refused, and FeroSSH starts from a link only as `ferossh.exe --url "<link>"`, which takes that one valid link and nothing else. The usual host key check applies, so a link to a host you have never used shows its fingerprint first. When another program already opens links of that kind, FeroSSH leaves it in place and offers itself in Windows Settings > Apps > Default apps, where you pick it; Windows may also ask the first time which program to use.

### Agent forwarding

`ForwardAgent yes` in your ssh_config, or *Fwd agent* in a saved connection's Extras, lets programs on the server (`ssh`, `git`) use the keys in your agent. FeroSSH asks every time one wants a signature, and says what for: a login to a host (with that host's key fingerprint), a `git` commit signature, or another SSH signature. Yes signs once, No refuses, and Cancel refuses everything else from that connection. Only your agent's keys are offered, never key files, and FeroSSH refuses anything else the server asks of the agent (adding, removing or locking keys), and any signature that is not a login or an SSH signature.

### Kerberos login

On a PC that is a member of a Windows domain (Active Directory), FeroSSH logs you in to servers that take Kerberos with the tickets Windows got when you signed in: no password, no key. The server needs `GSSAPIAuthentication yes` in its `sshd_config` and a keytab for its `host/` principal, as for any Kerberos SSH login. FeroSSH tries Kerberos first, before keys and passwords; when there is no ticket for the server, it says so in one line and carries on with keys and passwords.

- **When it is tried.** By default, when Windows signed you in with a domain account. `GSSAPIAuthentication yes` or `no` in your ssh_config, or *Kerberos* (auto, on, off) in a saved connection's Extras, sets it per host.
- **Which service.** The ticket is for `host/` and the host name you connect to (`HostName`). For a server known by another name, such as one behind a load balancer, set `GSSAPIServerIdentity` to the name in its principal.
- **Passing a ticket on.** `GSSAPIDelegateCredentials yes`, or *Fwd ticket* in Extras, gives the server a ticket of yours, so `ssh` and Kerberos file shares work onward from there without signing in again. Only turn it on for servers you trust: whoever runs the server can use that ticket while it is valid.

The server must prove that it holds its own Kerberos key (mutual authentication) before FeroSSH signs anything. Your tickets stay in the window, which talks to Windows; the sandboxed worker only passes the opaque Kerberos messages between the two.

### When the connection drops

If the network drops, the laptop sleeps or the server reboots, the tab stays where it is and FeroSSH logs in again on its own: after 1 second, then 2, 4, 8, 15 and every 30 seconds, for up to 15 minutes. Enter tries at once and Esc stops.

The tab keeps its splits, font, theme, scrollback and log file. Port forwards and the login script start again, tmux reattaches with its tabs and splits, and file transfers carry on where they stopped. Tabs that shared the connection share the new one, so there is still one login. A password you typed yourself is asked again; one from a password source is not.

Only a real drop counts. A shell you exit, a server that ends the session, a login that fails or a changed host key never reconnects. A drop is noticed when the server stops answering keepalives (`ServerAliveInterval` in your ssh_config, 30 seconds by default, `ServerAliveCountMax` 3).

File > Reconnect automatically turns this off for all hosts; `AutoReconnect no` in a host's profile turns it off for that host only.

## The tools pane

The connection manager, session manager, file transfer, port forwards, scripts and known hosts open in the tools pane at the side of the window. The tools rail along the window's edge has an icon for each, plus the command palette and a pin at the bottom; hover an icon for its name and shortcut. A click opens that tool, and a click on the open tool's icon closes it again.

The terminal slides over the tools pane rather than squeezing it. Drag the terminal's edge to cover part of the tool, or all of it so only the rail is left; the tool keeps its width underneath, so nothing in it rewraps. In File transfer, the buttons at the right edge (Bookmarks, Sync, Follow terminal, Layout, Refresh and the path rows' **...**) move left to stay in view, with the boxes beside them getting shorter, until they would run into the host name. When the lists are stacked, the upload and download arrows between them stay centred in the part you can still see, until there is no longer room for both; side by side, they stay where they are. In Port forwards, **Add** (and **Cancel** while editing) moves left the same way, with the connection box getting shorter, and the run report's buttons wrap onto more rows so none hides under the terminal. Drag the edge out from the rail to uncover the last tool again, or past the tool's width to make it wider. A tool you open again shows as much as it did last time.

With the pin off (Settings > Keep tools open), an opened tool is a peek: clicking in the terminal, Esc or connecting somewhere covers it again. Settings > Tools rail hides the rail; the tools then open and close from the Tools menu and their shortcuts only, as before.

## The connection manager

Ctrl+Shift+S shows your hosts in the tools pane: those from `~/.ssh/config`, your saved connections and your imports. They are in a Windows tree view, under a search box, with Connect, New, Edit and Import buttons below. Type to search (typing in the tree jumps to the search box; Down goes back to the tree), Enter or a double click connects in this tab, Shift+Enter in a split, and Ctrl+D marks a favourite. Left and Right close and open folders, and each host shows its `user@host:port` dimmed after its name (the port only when it is not 22; hover for the kind too). Tab moves between the search box, the tree and the buttons. A `# folder: Name` comment above a `Host` line groups hosts in your ssh_config. Settings > Side pane on the right moves the pane.

### Saved connections

Insert or the New button opens the connection form in the tools pane; F2, the Edit button or a right click edits the selected connection. It is a Windows form of text boxes and drop-down lists, with a tab per section:

- **Connection**: **Name, Folder, Type**; the type is an SSH terminal, Mosh terminal, SFTP file transfer only, SSH tunnels only (no shell), WinRM (PowerShell, cmd), Telnet (unencrypted), Serial console, Remote desktop (RDP) or VNC desktop. Then **Host, User, Port, Key file** (with Browse...), the **Password** source (below), the **Jump host** (see [Jump hosts](#jump-hosts)) and the **Proxy** (see [Proxies](#proxies)). A Mosh terminal adds the **Mosh server** program and its **UDP ports** (see [Mosh](#mosh)). A WinRM connection has **Host, User, Port**, the **Password** source and what it **Runs** (see [WinRM](#winrm-powershell-on-windows-servers)). A serial console has the port, speed, framing and flow control instead.
- **Look** (a terminal's tab): theme, font, font size and scrollback.
- **Extras** (a terminal's tab): tmux on or off, the tmux session to attach and its layout, the start directory, File transfer's Local and Remote folders, port forwards (such as `L 8080 localhost:80`), RDP and VNC ports, agent forwarding, Kerberos and passing a ticket on, older crypto, and the login script (see [Login scripts](#login-scripts)).

Empty fields show their default in grey. Enter saves, Shift+Enter saves and connects, Esc cancels. Connections go to `%APPDATA%\FeroSSH\sessions.conf` in ssh_config syntax and their look to `ferossh.conf`. Hosts from `~/.ssh/config` and imports show their connection settings read-only, because FeroSSH never writes those files; their look and extras can still change, and giving one a new name saves an editable copy.

### Passwords without typing

Keys and agents need no password at all. The Windows OpenSSH agent pipe also serves the 1Password and Bitwarden SSH agents, whose keys stay in the vault.

For servers that want a password, the form's Password field picks where it comes from:

- **ask every time** (the default): you type it.
- **Windows Credential Manager**: saved the first time a login with it works, as `FeroSSH:user@host:port`, protected by your Windows sign-in, and filled in from then on. With a master password set, these passwords are kept in the vault instead (see [Backup and sync between PCs](#backup-and-sync-between-pcs)).
- **1Password**, **Bitwarden** or **KeePassXC**: fills in a command for that tool's command line (`op`, `bw`, `keepassxc-cli`) that you can edit.
- **a command of my own**: `%n` is the connection's name, `%h` the host, `%u` the user, `%p` the port.

A command runs without a shell, a window or any input, for at most 60 seconds, and the first line it prints is the password. FeroSSH shows each command and asks before it runs it the first time. If the password from a source is refused, FeroSSH asks you to type it. One-time codes are always typed.

### Importing from other clients

File > Import and export > Import connections lists the clients that have saved sessions on your account: PuTTY and KiTTY (from the registry), WinSCP (registry or `WinSCP.ini`), MobaXterm (`MobaXterm.ini`), mRemoteNG (`confCons.xml`) and SecureCRT (its `Config\Sessions` folder). Pick one to see its sessions, untick what you don't want, and choose Import. *From a file...* reads the same files from a portable install or an export.

Host, user, port, key file, folders, PuTTY and KiTTY port forwards, jump hosts and the SOCKS 5 and HTTP proxies of PuTTY, KiTTY and WinSCP come over, and so do Telnet sessions and (from MobaXterm and mRemoteNG) RDP and VNC sessions. **Passwords never come over**: the preview marks the sessions that had one, so you can give them a password source. The preview also says what else does not come over. Imports go to `%APPDATA%\FeroSSH\imported-<client>.conf`, in a folder named after the client, and importing again from the same client replaces that file. A name already in use is left out.

### Backup and sync between PCs

FeroSSH has no cloud of its own. Your connections travel in an encrypted file that you keep where you like: OneDrive, Dropbox, a git repository or a USB stick.

**Export and import.** File > Import and export > Export connections writes your saved and imported connections, with their FeroSSH settings (type, look, extras, login script), to one `.ferossh` file encrypted with a passphrase you choose. Saved passwords go in only when you tick *Include saved passwords*. On the other PC, File > Import and export > Import encrypted connections asks for the passphrase and says what it would change before it does anything: connections it does not have are added, ones with the same name are replaced by the file's, and the rest of yours stay. Hosts from `~/.ssh/config` are not in the file; copy that file as it is.

**A master password.** Settings > Master password > Set a master password moves your connections, `ferossh.conf` and (if you tick it) the passwords saved in the Windows Credential Manager into one encrypted file, `connections.vault`, and removes the plain files. From then on FeroSSH asks for the master password when it starts, and passwords it saves go into the vault. Change master password and Remove master password (which writes the plain files back) ask for the current one first. There is no way to recover a forgotten master password, so keep an export somewhere safe too.

**Sync.** Master password > Move the vault file puts the vault in a folder you choose, for example in OneDrive. On a second PC, Master password > Use a vault file opens that same file, adds that PC's own connections to it and uses it from then on; where both have a connection of the same name, the vault's wins, so export this PC's connections first if you want to keep its versions. Both PCs now read and write one vault: a change on one shows on the other the next time it reads its connections, once the sync client has brought the file over, and host settings another PC changed meanwhile are kept when this one saves. Changes made on two PCs before the file synced, or at the same moment, can still overwrite each other (the last save wins), so let OneDrive finish syncing before you edit connections on another PC. Window settings (fonts, menus) are in the vault too and follow the PC that saved last. If you change the master password on one PC, the other asks for the new one when it next starts.

The encryption: the passphrase is stretched with Argon2id (64 MiB, 3 passes) into a 256-bit key, and the file is sealed with ChaCha20-Poly1305, so a wrong passphrase or a changed byte is refused rather than half read. The file is text (base64), so git and OneDrive handle it like any other file. Scripts in the scripts folder are not in the vault or the export.

### Portable use

To run FeroSSH from a USB stick on a PC you don't own, make a folder named `FeroSSH-data` next to `ferossh.exe`. FeroSSH then keeps everything it writes in that folder: settings, connections, `known_hosts`, logs, command history and the vault. It saves no passwords in the PC's Credential Manager (set a master password to keep them, in the vault on the stick), it asks again in each run before a password or proxy command runs (approvals are not kept in a folder that travels), and as it quits it removes the AppContainer profiles Windows keeps for its sandbox. It still reads the PC user's `~/.ssh/config`, and Windows itself may keep traces such as recent-files lists and the temporary copy of a file opened with *Edit locally*.

## The session manager

Ctrl+Shift+L lists the sessions that are open right now, in every tab: their host, kind (SSH, Mosh, SFTP, WinRM, Telnet, serial, tmux, desktop or local shell), tab and state (connected, connecting, reconnecting or ended), with notes such as tmux, forwards, logging or a jump host. It also shows the panes you hid (Window > Hide pane), with *hidden* as their tab, and the hidden sessions of a [multi-host script run](#runs-on-many-hosts). It is a Windows list with columns (session, kind, tab, state, details) and updates as sessions connect, drop and reconnect. Type to filter, Enter or a double-click goes to the session's tab and pane, F2 renames it in place, and Del closes it after asking. The Show, Rename and Close buttons below the list, and a right-click on a session, do the same; the right-click menu can also hide a session's pane.

The connection manager is for the hosts you *can* connect to; the session manager for the ones you *are* connected to.

## Jump hosts

To reach a host that is only reachable through another one, give it a jump host: `ProxyJump bastion` in your ssh_config, or the *Jump host* field in the connection form. Several go in turn, separated by commas, as `ssh -J` takes them: `admin@gw:2222,inner`.

Each jump host logs in as if you connected to it, with its own host key check and its own key or password, in a sandboxed worker of its own. The host behind it is then reached through that login, encrypted end to end, so a jump host never sees what you type on the target. If a jump host cannot reach the next one, the pane says which one and why.

`ProxyCommand` in your ssh_config runs a local program (for example `corkscrew proxy.example.org 8080 %h %p`) and sends the connection through it. FeroSSH asks once before it runs a command line it has not run before, and shows what the program printed when it fails. A program given by name alone (`nc`, `corkscrew`) is looked for in the Windows folders and the folders on `PATH` only, never in FeroSSH's own folder or the current one, so a file that landed next to `ferossh.exe` (in Downloads, say) cannot stand in for it; give the full path for a program elsewhere. Password commands are found the same way. `ProxyJump none` or `ProxyCommand none` turns either off for a host.

## Proxies

When the way out of your network is a SOCKS 5 or HTTP proxy, put it in the connection form's **Proxy** field as a URL:

- `socks5://proxy.example.org:1080` (`socks5h://` means the same; the proxy looks the host name up),
- `http://proxy.example.org:3128` (HTTP `CONNECT`),
- with a user to log in to the proxy: `socks5://me@proxy.example.org:1080`.

Without a port, SOCKS 5 uses 1080 and HTTP 8080. In your ssh_config the same URL goes after `ProxyServer`, FeroSSH's own keyword; OpenSSH stops at keywords it does not know, so add `IgnoreUnknown ProxyServer` above it in a file OpenSSH reads too. `ProxyServer none` turns it off for a host. `ProxyJump`, `ProxyCommand` and `ProxyServer` share one place: the first of them a host gets counts.

The usual proxy commands work without the program, so `nc` or `ncat` need not be installed: FeroSSH reads `nc -X 5 -x proxy:1080 %h %p` (SOCKS 5, also without `-X`), `nc -X connect -x proxy:3128 -P me %h %p` (HTTP, user `me`), `ncat --proxy proxy:8080 --proxy-type socks5 %h %p` (HTTP without `--proxy-type`), and `connect -S proxy:1080 %h %p` or `connect -H proxy:3128 %h %p` as a proxy and runs nothing; without a port, `nc` and `ncat` use 1080 for SOCKS 5 and 3128 for HTTP, as they do themselves. Other commands, these with other options, `connect` without a port and lines with `%r` or another token in the proxy's address or user still run as commands.

A proxy with a user asks for its password once, in the pane, before connecting. When the proxy takes it, FeroSSH keeps it in the Windows Credential Manager (as `FeroSSH proxy:me@proxy.example.org:1080`, where you can remove it), so the next connection does not ask. When the proxy refuses a kept password, FeroSSH forgets it and asks the next time. A password never goes in the URL, and the form refuses one there.

The proxy carries only the SSH connection, which stays encrypted end to end: the proxy sees which host and port you connect to, nothing more. The sandboxed worker speaks to the proxy itself, and its replies are parsed there and fuzz-tested. With jump hosts, give the proxy to the first jump host's own connection: the proxy is how FeroSSH reaches that one, and the others are reached through it. Mosh logs in through the proxy, but its UDP goes straight to the host.

## Mosh

Mosh keeps a terminal going when the network does not: on a laptop that moves between Wi-Fi networks, sleeps, or sits on a slow or lossy link. Make a connection of type *Mosh terminal (SSH + UDP)* in the connection form. FeroSSH logs in over SSH as for an SSH terminal (keys, agent, password sources, jump hosts all work), runs `mosh-server` on the host, and from then on talks to it over UDP, as the `mosh` command does.

- The host needs Mosh installed (`apt install mosh`, `dnf install mosh`), and its firewall has to let UDP in on the ports the server uses: 60001 to 60999 unless the form's **UDP ports** field narrows them (`60001` or `60001:60010`). **Mosh server** names the program when it is not on the path, such as `~/bin/mosh-server`.
- When you change network, the next packet simply comes from the new address and the server answers there. When the server stays quiet, FeroSSH tries a new local port in case a router forgot the old one, and a bar on the top line says how long ago the server was last heard. Nothing is lost: the screen catches up when the network comes back.
- Through a jump host, the SSH login goes through the jump host but the UDP goes straight to the host, as with `mosh`, so the host's UDP ports must be reachable from your PC.
- If the server does not answer over UDP within 15 seconds, the pane says so: usually a firewall blocks the ports.
- The SSH connection stays open next to the Mosh session for File transfer and port forwards. A Mosh tab never shares its connection with another tab.
- tmux integration needs an SSH terminal (Mosh sends screens, not the byte stream tmux control mode needs); run `tmux` in a Mosh pane as usual. ZMODEM does not work over Mosh for the same reason. Logs, themes, login scripts, scripts and broadcast input work.
- **Local echo.** On a slow link, what you type shows at once instead of after a round trip, underlined while the server has not yet confirmed it when the round trip is 80 ms or more. FeroSSH guesses only plain typing, Backspace, and Left and Right; after Enter or any other key it waits for the server before guessing again. It checks every guess against what the server then shows, drops them all when one was wrong, and shows a new run of guesses only once one has come true, so text a program hides (a password prompt) never shows. The form's **Local echo** picks when: *on a slow link* (the default: from about 30 ms round trip, or when an echo is late), *always*, or *never* (only what the server sends).

## WinRM (PowerShell on Windows servers)

A **WinRM** connection opens PowerShell (or cmd) on a Windows server through its WinRM service, the one `Enter-PSSession` and `winrs` use, over HTTP on port 5985 or HTTPS on port 5986. Type `winrm://host`, `winrm://LAB\admin@host` or `winrm host` at the connect prompt (`winrms://host`, or port 5986, for HTTPS), give the `winrm://` form on the command line (`ferossh.exe winrm://host`), or make a connection of type *WinRM (PowerShell, cmd)* and pick its *Transport*. *Runs* says what the shell starts: `powershell -NoLogo` unless you give another command, such as `cmd`.

- **Login.** With a user name (`LAB\admin`, or a local account such as `admin`) the pane asks for the password, or a saved connection takes it from its [password source](#passwords-without-typing) (the Windows Credential Manager or a password manager's command); with none, FeroSSH logs in as the Windows user it runs as. Windows uses Kerberos when it can (usually a server in your domain, named by its domain name, with a domain account). Where it cannot, for instance for a local account or a server named by address, it would use NTLM, which does not prove the server's identity: over plain HTTP a machine in the middle could take or relay that login. So over HTTP FeroSSH logs in with Kerberos only and otherwise stops with a message, as Windows' own WinRM client does for hosts outside `TrustedHosts`. Use HTTPS (`winrms://`), which allows NTLM inside TLS, or save a connection whose *Transport* is *HTTP, Kerberos or NTLM* for a server you reach over a network you trust. The pane says which one it used.
- **Encrypted.** Once logged in, every shell request and reply is encrypted with the login's keys, as Windows' own WinRM client does over HTTP; FeroSSH refuses any reply that is not. The login itself (Kerberos or NTLM tokens) travels in the HTTP `Authorization` header before that, as with any WinRM client over HTTP.
- **HTTPS.** Over HTTPS everything, the login included, goes inside TLS, which Windows itself (SChannel) does in the window; the sandboxed worker only carries the encrypted bytes. A certificate Windows trusts for that server name goes through as it does for a browser. Any other certificate, such as the self-signed one `winrm quickconfig -transport:https` setups often use, makes the pane show its SHA-256 fingerprint and ask *(yes/no)* before the login goes on; compare it with the server's (`Get-ChildItem Cert:\LocalMachine\My` on the server shows the thumbprint, `(Get-PfxCertificate file).GetCertHashString('SHA256')` the SHA-256). Yes remembers it in `%APPDATA%\FeroSSH\winrm_certs`, and from then on a different certificate for that host and port that Windows does not trust either ends the login with a warning, like a changed SSH host key; once the remembered certificate expires, the pane asks again. The server needs an HTTPS listener: `New-Item -Path WSMan:\localhost\Listener -Transport HTTPS -Address * -CertificateThumbPrint <thumbprint>`.
- **Lines, not a terminal.** WinRM's remote shell has no terminal, so FeroSSH edits the line you type and sends it on Enter. Backspace, Ctrl+U (clear the line), Up and Down (earlier lines) and Ctrl+C (interrupt the command) work; full-screen programs and tab completion do not. Errors show in red. When the command ends, Enter connects again.
- On the server, WinRM must be on (`Enable-PSRemoting`, or `winrm quickconfig`) and your account allowed to open a shell: a domain administrator, or the built-in Administrator, does out of the box (another local administrator only once `LocalAccountTokenFilterPolicy` is 1 under `HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System`, as for any remote administration of a workgroup PC). Any other account also needs *Execute (Invoke)* in `winrm configSDDL default` (membership of *Remote Management Users* alone lets it log in but not open a shell), and Windows must be able to start a shell for it; when it cannot, Windows' own `winrs` fails the same way, with *Server execution failed*. A server with the Windows OpenSSH server is better reached over SSH, which gives a full terminal.
- Logs, themes, fonts and splits work as for SSH tabs; SFTP, forwards, tmux, login scripts and broadcast input do not.

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

The script runs once after an SSH or Telnet login, or when a serial port opens; tmux attaches after it. Pressing any key stops it, and Session > Scripts > Run login script again starts it over. What it types is never broadcast.

Secrets are never written to a FeroSSH file. The first time, the pane asks for the secret (without echo) and keeps it in Windows Credential Manager as `FeroSSH secret:<name>`. Or a `Secret` block in `ferossh.conf` names a password manager command for it (see [Settings](settings.md#secrets)).

## Scripts

Ctrl+Shift+Space (Tools > Scripts) lists named command lines, grouped under their categories, with each script's hotkey, auto-response text and first line, and the selected script's lines under the list. Type in the search box (or just start typing in the list) to search names, categories and lines, then Enter, a double-click or Run types the script with Enter after each line, or Shift+Enter leaves the last line for you to finish. Left and Right jump between categories, and a right click offers the same as the buttons. With broadcast input on, it goes to every broadcast pane, so one script works several terminals at once.

Insert or New adds a script (what you typed as a search becomes its name), F2 or Edit edits one, Del or Delete removes one, and F5 or On hosts runs it on saved hosts you pick. The editor has a field each for the name, category, hosts, folders, hotkey and *When* text, and a box for the lines below them; Save (or Enter in a field, or Ctrl+Enter in the lines) stores it, and Cancel or Escape leaves it. When something is wrong the reason shows above the buttons and the keyboard goes to the field or line at fault. Edit > Save selection as script starts one from text selected in a pane.

**Categories.** The editor's *Category* field files a script under a category, such as `Linux` or `Network/Cisco` (a `/` makes a category within a category, up to four deep). A new script starts in the category of the one selected. Click a category's heading in the list to fold it away, and click again to open it; FeroSSH remembers which are folded until it closes, a search shows every match, and Left or Right into a folded category opens it.

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

A script with logic needs no extra program: it runs in FeroSSH itself, on what the pane shows, and can only type into its own pane. A loop that runs 1000 steps, or types 8 passwords or secrets, without waiting for the host, a pause or a delay stops with a message, and any key you press stops a script. The editor checks that every `goto` has its label before it saves. Logic and `@pane`/`@tab` moves do not mix in one script; a `@capture` before a move does, and its `${captured}` is filled in after it.

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

A script's *When* text makes it answer by itself whenever the host prints that text, on the hosts it is kept to: for example `@key space` when `--More--` appears, or `@key enter` on `[confirm]`. An auto-response only types lines and keys you wrote, never a password, a secret, the clipboard or arguments, and after 20 answers in one second it pauses until you type. Session > Scripts > Auto-responses from scripts turns them all off.

### Production guard

A script about to be typed into a pane with the red Production theme asks first, listing the hosts and the lines. So does starting a coding agent from such a pane, and pasting text, an image or files into a coding agent's pane on such a host, or dropping files on it (see [Coding agents on Production hosts](#coding-agents-on-production-hosts)). Session > Scripts > Confirm scripts and agents on Production hosts turns all of that off.

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

### The built-in library

FeroSSH comes with a few hundred ready scripts, built into `ferossh.exe`, that you can write into your scripts folder whenever you want them. They land under a `Library` folder in the scripts pane, grouped by category:

- **Linux** — disk, files, processes, memory, users and the shell.
- **System administration** — services, logs, packages, cron, storage, backups and health checks for a Linux server.
- **Windows** — PowerShell one-liners for system, disk, services, events, network, files, repair and security (use them in a local PowerShell tab, a WinRM session, or over SSH to a Windows host whose shell is PowerShell).
- **Networking** — addresses, ports, DNS, path and packet tools, TLS, bandwidth and scanning.
- **Network devices** — Cisco IOS and Juniper Junos show commands, for serial and SSH sessions to switches, routers and firewalls.
- **Security & hardening** — firewalls, SELinux, auditd, fail2ban and file-permission checks.
- **Monitoring & logs** — journalctl, process and I/O stats and log-grep recipes.
- **Storage & backup** — LVM, ZFS, rsync and tar.
- **Databases** — MySQL/MariaDB, PostgreSQL, Redis and MongoDB queries, sizes and dumps.
- **Web servers & TLS** — nginx, Apache and openssl certificate checks.
- **Containers & Kubernetes** — Docker, Podman, Compose, kubectl and Helm.
- **DevOps** — Terraform, Ansible, CI helpers and systemd service units.
- **Cloud** — the AWS, Azure and Google Cloud command lines.
- **Virtualization** — Proxmox, KVM/libvirt (virsh) and VMware ESXi.
- **Development** — git, Python, Node, Rust, Go, build tools, debugging and data wrangling.
- **Hacking** — standard recon, enumeration and listener commands for penetration tests, CTFs and lab practice, to run only on systems you are allowed to test.

The library scripts work like any other: run one, edit it, pick a category, or use its `$1`, `${1:default}` arguments and `${host}` style variables.

**Writing the library.** It is not written until you ask. Session > Scripts > Write built-in scripts to the library writes the shipped scripts into your scripts folder. Any that are not there yet are added; any already there are left exactly as you have them, unless you choose to overwrite them. So **your changes stay yours**: edit a library script and your version is kept, delete one and it stays gone, and writing the library again never touches what you have unless you say so. Run it again after an upgrade to pick up new scripts a release added. To keep a shipped script and still change it, copy its lines into a script of your own with New (the lines box takes pasted text); New on its own starts an empty script, it does not copy the selected one.

**Folded categories.** In the scripts pane the categories start folded, so the list is short; click a category heading to open it, and FeroSSH remembers which you leave open for next time.

### Importing scripts

File > Import and export > Import scripts lists what other programs on this PC have, with a box per script and its first lines, so you can check them before anything is written. Imported scripts get a file each in the scripts folder; one whose name is already taken in its category is left out.

| From | What comes over |
| --- | --- |
| Windows Terminal | `sendInput` actions in `settings.json` (named ones, with nested command menus as categories), and the `snippets` of a `.wt.json` file. |
| SecureCRT | The Send String buttons of its button bars (`ButtonBarV5.ini` or `ButtonBarV4.ini`), a category per bar. `\r` becomes a line, `\p` a one-second `@pause`. |
| MobaXterm | The macros in `MobaXterm.ini`. MobaXterm does not document how it stores them, so check the lines on the import page. |
| A file | Another FeroSSH's `.script` or `.snippet` file, or any text file as one script, a line per line. |

Keystrokes come over as steps: text without an Enter becomes `@type`, Tab, Escape and Ctrl+ keys become `@key`, and a `$` or a leading `@` is written so that it types as it is.

### Recording a script

Session > Scripts > Record keystrokes as script records what you type in the focused pane until you pick it again, then opens it in the script editor to name and check. Backspace, Ctrl+U and Ctrl+W edit lines as a shell does, Tab and Ctrl+ keys are kept as `@key` steps so tab completion replays, and a line the host did not echo (a password) is recorded as `@password`, never as its text.

## Standby window

A hotkey can hide a window and bring it back, so a terminal (or any other program) is one key away without taking a taskbar button. It is off until you turn it on, under Window > Standby window (hotkey):

- **Pick the window.** Window to show and hide lists *This FeroSSH window* and the open windows of other programs, each with its program (`notepad.exe`, `ferossh.exe` for another FeroSSH window). FeroSSH remembers the program, not the single window: after a restart, or when that window closed, it uses the program's front window.
- **Pick the hotkey.** Pick one of the hotkeys listed (Win or Ctrl with the key below Esc, Ctrl+Alt+Space, Scroll Lock, Pause) or choose Press a new hotkey... and press the one you want. A hotkey needs Ctrl, Alt or Win, except Pause, Scroll Lock and F13 to F24; FeroSSH refuses Ctrl+Alt with a letter, because that is AltGr on many keyboards. Picking a window or a hotkey turns standby on.
- **See whether it is active.** The first line of the menu says *on* and for which window, *off*, or *not active, … is taken* when another program (or another FeroSSH window) already has that hotkey. Click it for the details. Standby hotkey on is ticked while this FeroSSH window holds the hotkey.
- **Turn it off.** Click Standby hotkey on again; the hotkey is released at once.

Pressing the hotkey hides the window when it is in front, and brings it to the front when it is hidden, minimised or behind another window. Hiding it gives the keyboard back to the window that was in front when the hotkey brought it up, such as the FeroSSH pane you were typing in. While hidden it has no taskbar button and only the hotkey brings it back; it keeps running. Another program's window is never left hidden: FeroSSH shows it again when you turn standby off, pick another window, or close FeroSSH, and at its next start if it ended without doing so. Windows does not let FeroSSH hide a program run as administrator unless FeroSSH runs as administrator too; FeroSSH says so when that happens.

## Full screen

F11 (or Settings > Full screen) makes the window cover the whole monitor, taskbar too, with no title bar, frame or menu bar; F11 again puts it back as it was, maximized or not. It works wherever the keyboard is: a terminal pane, a remote desktop, the tools pane. Alt still shows the menu bar, which hides again once a menu closes. A program on the server that needs F11 itself can have it: turn off Settings > F11 switches full screen, and F11 goes to the terminal like the other function keys (Full screen stays in the menu and the command palette).

## The command palette

Ctrl+Shift+A (Tools > Command palette) opens a search box over every menu item, every saved connection and every command script, as a list in the tools pane. Type a few letters in any order: `splr` finds Split right, `core` your core switch, `ctrl+shift+n` the item on that key. Up and Down select; Enter (or Run) runs the item, connects or types the script; Shift+Enter connects in a split or types a script without its last Enter. Each item shows its shortcut as it is now (Settings > Key bindings), its check mark, and the menu it is in; greyed-out items are left out, and your last few picks come first. Esc clears the search, then closes the palette.

## Key bindings

Settings > Key bindings lists every action with its shortcuts and its default ones. Select one and press Change (or Enter), then the keys you want; Add key gives it a second one, No shortcut takes them away, Reset to default puts its own back, and Reset all every action's. A key another action has moves after asking, keys that would stop typing are refused, and keys that programs in the terminal use (Ctrl+C, Alt+B, the tmux prefix) need a yes first. Details are in the [menu reference](menus.md#key-bindings).

## Broadcast input

Window > Broadcast input to this tab's panes (Ctrl+Shift+B) sends what you type and paste in the focused pane to the other panes of the tab too; Broadcast input to all tabs reaches every pane. Each pane receiving it is framed in amber and the window title says broadcast is on. Window > Leave this pane out of broadcast keeps one pane to itself.

Nothing typed at a password prompt or the connect prompt is broadcast, and a pane waiting at one never receives other panes' keys. A paste with line breaks always asks first while broadcast is on, and says how many panes it goes to.

## File transfer

Ctrl+Shift+E opens File transfer in the tools pane: a **Local** list of a folder on your PC and a **Remote** list of the focused pane's host over SFTP, on the same connection. Both are Windows file lists with icons, size and date (and the rights on the server). Select several files and folders with Ctrl or Shift as in Explorer, or Ctrl+A for all.

- **Moving files.** The arrow buttons between the lists upload the Local selection or download the Remote selection; they point the way the files go. Ctrl+Enter does the same from either list, and Enter or a double-click on a file sends that file across. You can also drag the selection from one list to the other, onto the list for its folder or onto a folder in it, drag the Local selection onto a terminal pane to upload it to that shell's current folder, and drop files or folders from Explorer on the Remote list (or on a terminal pane, for the shell's current folder), after a confirmation. With Session > Coding agents > Paste and drop files: upload and type the path ticked, a drop on a terminal pane goes to `~/.cache/ferossh/paste/` instead and types the paths (see [Pasting screenshots and files for an agent](#pasting-screenshots-and-files-for-an-agent)). Drag the Remote selection off FeroSSH to copy it to Explorer, the desktop or any program that takes dropped files: when you let go, FeroSSH downloads the files (shown in the queue, with "Downloading for the drop" under the lists) and the drop happens once they are all complete; Escape cancels. They wait in a folder under %TEMP%, so a drop needs room there too; when a later drag starts, FeroSSH removes the drag folders there that are more than a day old.
- **Folders.** Enter or a double-click opens a folder; Backspace or the ↑ button goes up (above a drive comes the list of drives). Type a path in the box above a list and press Enter to go there. ⌂ goes to your home folder on the server, and ... picks the Local folder. Downloads go to the Local list's folder; FeroSSH remembers it (`DownloadFolder`), and until you pick one it is your Downloads folder.
- **Filter.** The box at the top shows only the names in both lists that contain what you type (any case), or match a pattern such as `*.log`. Escape clears it.
- **Bookmarks.** ★ Bookmarks ▾ opens a bookmark: its Local folder in the Local list and its Remote folder in the Remote list. *Add bookmark here...* keeps the two folders shown under a name you type (Enter saves, Escape cancels). Tick *Only for this host* to see it only on this connection; a bookmark with a Remote folder starts out that way. Rename and Delete are in the same menu.
- **Sync browsing.** With ⇆ Sync on, opening a folder in one list opens the folder of the same name in the other, and going up goes up in both. Typing a path or opening a bookmark moves only that list. Both lists move or neither: the Local list waits until the Remote folder has opened. When the other side has no such folder, or the Remote folder will not open, neither list moves (if the Local folder fails to open after the Remote one did, the Remote list goes back) and the line under the lists says so.
- **A connection's folders.** *Local folder* and *Remote folder* in a connection's Extras (`LocalFolder`, `RemoteFolder`) are where File transfer starts for that host: the Remote list opens there the first time, before following cd takes over at the shell's next `cd`, and the Local list opens there whenever File transfer turns to that host (without changing your download folder).
- **Sorting.** Click the Name, Size or Modified column header to sort that list on it; click it again to reverse the order. The Local and Remote lists each keep their own sort. Names start A to Z, sizes largest first and dates newest first; the arrow in the header shows which way. Folders always stay above files. The right-click menu has Sort by name, size and modified date too. FeroSSH remembers it (`TransferSort`).
- **Layout.** Layout ▾ puts the lists side by side or one above the other, and swaps Local and Remote. Drag the gap between the lists to give one more room. FeroSSH remembers it (`TransferLayout`).
- **↻ Refresh** reads both lists again (F5 does too, anywhere in the pane outside the lists; in a list it reads only that list).
- **Right-click** a list for Open, Upload or Download, Delete (after asking; Del too: on the server from the Remote list, to the Recycle Bin from the Local list), New folder, Rename (F2), Permissions, Refresh (F5), Home folder, Upload files... and Open in Explorer. New folder, Rename and Permissions open a box under the filter: Enter does it, Escape cancels. Permissions takes `755`, `rwxr-x---` or chmod's `u+x`, `go-w`, and applies to every file selected.
- **Edit locally.** Right-click a file on the server and choose Edit locally: FeroSSH downloads it into a folder of its own under `%TEMP%\FeroSSH-edit` and opens it in the editor Windows has for that kind of file (Notepad when there is none; never by running it). Each time you save, FeroSSH puts it back over the server's file, keeping its permissions, and the line under the lists says so. It watches until the File transfer connection ends; edit folders older than a week are cleared.

**Queue.** The transfer queue under the lists shows each transfer's progress. Right-click one to pause, resume, cancel (Del too) or retry it, or to clear the finished ones. Three files move at once (`ParallelTransfers`). A dropped connection pauses the queue, and it carries on from where each file stopped when you log in again.

**Safe file names.** An existing file is never overwritten without asking: Overwrite, Skip or Keep both, above the queue, with For all to answer the same for the rest. Server names Windows cannot hold (`CON`, `a:b`, trailing dots) are made safe or refused, and a folder download does not follow links out of the folder. Partial files are `.part` locally and `.filepart` on the server until complete. Session > Verify transfers compares SHA-256 sums on both ends after each file. A resumed transfer is always checked this way before its partial file becomes the file, even with that setting off: a mismatch fails the transfer and deletes the partial file, and if the server cannot give a sum (it needs `xargs -0` and `sha256sum`) the transfer fails and keeps the partial file. Delete it to start over.

### SFTP-only connections

Some accounts can only move files: `ForceCommand internal-sftp` in the server's `sshd_config`, chrooted upload users, many hosting providers. A shell there ends at once (*This service allows sftp connections only*), and the connection with it. For these, make a connection of type *SFTP file transfer only*, or type `sftp://user@host[:port][/folder]` at the connect prompt (WinSCP's and FileZilla's addresses work, `;fingerprint=` and all). A folder starting with `/~` is in the home folder: `sftp://up@files/~/docs` opens `docs` there, and `/~` alone the home folder itself. FeroSSH logs in as usual (keys, agent, passwords, jump hosts, proxies, Kerberos) but asks for no shell: the tab says so, and File transfer opens on the host, in the address's folder or the connection's *Remote folder* when there is one. Close the tab to log out. A dropped connection logs in again on its own, and the queue carries on, as for a terminal. Its Extras hold only what makes sense without a shell: Remote folder, Local folder, Kerberos and legacy algorithms.

### Following cd

The file view, and files dropped on a pane, follow the shell's current directory. Shells that report it with OSC 7 (fish does by default) work anywhere. Otherwise FeroSSH reads it from a `user@host:dir$ ` prompt, bash's default on Debian and Ubuntu. For other prompts, add this to `~/.bashrc` on the server:

```
PROMPT_COMMAND='printf "\e]7;file://%s%s\e\\" "$HOSTNAME" "$PWD"'
```

Settings > File transfer follows cd, or the Follow terminal button in File transfer, turns it off.

### ZMODEM

On a host without SFTP, such as a console server or a Telnet device, you can use ZMODEM (`rz` and `sz`) instead. Turn on Session > ZMODEM: answer rz and sz; it is off by default, so their output shows as it is. Then `sz file` on the server asks whether to receive it (any output can start a send, so FeroSSH never takes files unasked) and saves it in your download folder (as `name (2)` if one is there already), and `rz` opens a file picker for what to send. Progress shows in the pane, and Ctrl+C cancels. Files over 4 GB need SFTP.

## Known hosts

Tools > Known hosts (Ctrl+Shift+K) lists every entry of the `known_hosts` files FeroSSH checks host keys against: your `~/.ssh/known_hosts` (or the portable folder's), and any other file a `UserKnownHostsFile` line in `~/.ssh/config` names. Each row shows the hosts, the kind (Host key, CA for an `@cert-authority` line, Revoked for an `@revoked` one, or Unreadable for a line FeroSSH and OpenSSH skip), the key type and size, its SHA-256 fingerprint, the file and line, and the comment.

Names FeroSSH (and OpenSSH with `HashKnownHosts yes`) saves are hashed, so they show as *(hashed)*: type the host name in the search box, as `web.example`, `web.example:2222` or `[web.example]:2222`, and the hashed lines for it show with that name. A host name also finds the wildcard lines that cover it, such as a `*.example` CA, the way a connection would find them. A name without a dot (`web01`) is searched as text; write it in brackets, `[web01]`, to find the wildcard lines (`web*`) that cover it. Other words search the hosts, key types, fingerprints, comments and file names.

Remove (Del) takes the picked lines out of their files, whole, after asking: a line that lists several hosts, or a wildcard, goes for all of them (Session > Replace host key changes only one host's entries). When no other line holds a key for the host, the next connection asks about its key again, as for a new host. Removing a `@revoked` line asks twice, since its key is then no longer refused. Lines are removed only if they are still as listed: if another program changed any of the files meanwhile, nothing is removed and the list is read again. Lines that stay keep their line ends, and a file that is a link to another stays a link. Copy fingerprint (Ctrl+C) and the right-click menu's Copy line copy an entry. A file that cannot be read is named under the list.

## Port forwards

`LocalForward`, `RemoteForward` and `DynamicForward` in your ssh_config, or forwards in a saved connection's Extras, start when you log in. Tools > Port forwards (Ctrl+Shift+P) opens a pane with a form at the top and a list of every connection's forwards and X11 under it, in columns: connection, type, listen, to and state. The line under the list shows the selected forward's open connections, why it stopped and its last error. Everything there works on a live connection, without reconnecting:

- **Add**: pick the type (Local `-L`, Remote `-R` or SOCKS `-D`), type the port to listen on (`8080`, or `127.0.0.1:8080`) and where it goes (`db.internal:5432`; a SOCKS forward goes anywhere), then Add or Enter. The On box picks the connection; its first entry is the focused tab's. The Listen box also takes a whole `-L 8080:db.internal:5432`, `-R 9000:localhost:3000` or `-D 1080`.
- **Change**: Edit, Enter, F2 or a double click on a forward puts it in the form; Save changes its port or target, and Escape leaves it as it was. A new target on the same port keeps the open connections.
- **Retry**: Retry (or the right-click menu) starts a stopped forward again.
- **Stop**: Stop, Del or the right-click menu on a forward, or `-KL 8080`, `-KR 9000`, `-KD 1080` in the Listen box by port, as `~C` does in OpenSSH.

Changes last through the tab's reconnects; the saved connection is left as it was. A dynamic forward is a SOCKS 4, 4a and 5 proxy. Local listeners take `127.0.0.1` and `::1` unless you give an address (`*:8080`, or `GatewayPorts yes`). A tab whose connection forwards shows `⇄`, in amber when the server can reach back into your PC (remote forwards or X11).

### Tunnel-only connections

Sometimes a login is only there for its forwards: a database behind a bastion, a web console on a private network, a SOCKS proxy. Like `ssh -N`, a tunnel-only connection logs in without asking for a shell, so nothing runs on the server and nothing can be typed by mistake. Three ways to make one:

- a saved connection of type *SSH tunnels only (no shell)*, with its forwards under Extras;
- `SessionType none` for the host in your ssh_config, with its `LocalForward`, `RemoteForward` and `DynamicForward` lines;
- `ferossh.exe -N -L 5432:db.internal:5432 bastion` on the command line (a desktop shortcut works well). `-L`, `-R` and `-D` take the forwards as `ssh` writes them.

The tab says it holds tunnels only and lists the forwards; with none yet, Port forwards opens so you can add them. The forwards stay up while the tab is open, also when other tabs on the same host close, since a tunnel-only connection never shares its login with a shell. A dropped connection logs in again on its own and the forwards come back. Close the tab to log out. Its Extras hold only what makes sense without a shell: the forwards, Kerberos and legacy algorithms. PuTTY sessions with *Don't start a shell or command at all* import as tunnel-only connections.

## X11 forwarding

`ForwardX11 yes` for a host in your ssh_config sends X programs on the server to an X server on Windows, such as VcXsrv or X410; FeroSSH ships none, and the X server must listen on TCP. The display is `X11Display` in `ferossh.conf`, else `DISPLAY`, else `localhost:0`. The server only gets a fake cookie; FeroSSH puts the real one from `%XAUTHORITY%` or `~\.Xauthority` in its place. X11 is off unless a host turns it on, because X programs on the server can see and type into your other X windows.

## tmux

Session > tmux > Attach tmux types `tmux -CC new-session -A -s main` at the pane's shell (typing `tmux -CC` yourself works too). tmux's windows and panes become native FeroSSH tabs and panes: no status bar, no prefix key, and native scrollback (with tmux's history in it), copy, paste and search. Splits, new tabs and closing panes go to tmux.

Session > tmux > Detach tmux, closing FeroSSH or losing the network leaves everything running on the server. Attach again, from this PC or another, and the tabs, splits, fonts and themes come back; after a dropped connection it reattaches by itself. Session > tmux > Switch tmux session lists the other sessions on the server (also offered right after attaching).

tmux makes a window only as big as the smallest client attached to it. If another client (another PC, or one left behind by a dropped connection) keeps a window smaller than the pane, part of the pane stays empty; FeroSSH notices and offers to detach the other clients, and Session > tmux > Detach other clients does the same later.

There are two layouts, kept per session:

- **One window per pane** (the default; tmux 3.3 or later): every native pane is its own tmux window, so each pane can have its own font, text size and theme. FeroSSH keeps the splits and looks in the session. A plain tmux client shows the panes as separate windows.
- **tmux's own splits** (untick Session > tmux > One window per pane, or older tmux): each tmux window is a tab and tmux's splits are its panes, one font per window.

tmux is never started unless you ask. To attach on every login to a host, set `Tmux` (and optionally `TmuxLayout`) in its profile, or switch tmux on in the connection form's Extras (click it, or press Left or Right) and optionally give a tmux name (`main` when left empty). GNU screen has no control mode and runs as an ordinary terminal.

To have a host's shells start in one server directory, `cd` there and use Session > Start new tabs and panes in this directory (or fill in Start dir in the connection form's Extras). FeroSSH types the `cd` after every login and uses it for the first window of a new tmux session. New tabs and panes opened from a pane follow that pane's directory instead, and use the start directory only when that one is not known. Clear Start dir in the form to go back to the home folder.

If your fingers know tmux's keys, tick Session > tmux > Use tmux keybindings for tabs and panes. In a pane of a tmux session, your tmux prefix (Ctrl+B unless your tmux config sets another; FeroSSH asks tmux) followed by `c`, `%`, `"`, `n`, `p`, `0`-`9`, `o`, an arrow, `x`, `&`, `!`, `d` or `s` works the native tabs and panes, which are tmux windows and panes. Arrows, Ctrl or Alt+arrows (resize), `[`, PageUp and PageDown repeat as in tmux: after the first one, press them again without the prefix within tmux's `repeat-time` (half a second unless your tmux config sets another), or hold the key down. Press the prefix twice to send it to the program, and the prefix then `?` for the full list. It is off unless you turn it on.

### Coding agents in tmux

Session > Coding agents starts a coding agent (Claude Code, Codex, aider, or your own command) in tmux on the server the focused pane is logged in to, so it keeps working when your PC sleeps or the network drops, and comes back by itself when FeroSSH reconnects. Each agent gets a tmux window of its own, a new tab in the pane's directory, named after the agent; FeroSSH types the agent's command at that window's shell, so the shell stays when the agent exits and you can start it again in place. When tmux already shows on that host, the agent joins that session; otherwise it goes into a tmux session named `agents`, made when needed, which FeroSSH then attaches. The agent's window is marked in tmux (the window option `@ferossh-agent`), so it is recognised again after a reconnect or from another PC. It needs an SSH pane with tmux on the server; Mosh and SFTP-only connections cannot start one.

**Agent dashboard** (the menu's first item) turns the Session manager into one view of every coding agent in FeroSSH, on every host you are connected to, one row each: the host, the agent, its tab, what it is doing (*working* while it prints, *needs you* when it asked for attention, *quiet* once its output stopped after a stretch of work, or *idle*), for how long, and the last line it showed. The agents that wait for you come first, the one waiting longest on top; Enter or a double click goes to its pane. The last line is read from the bottom of the agent's screen, skipping the box around its input and its key hints, so it is a good guess at what it last said rather than a reading of it. Type after `is:agent` in the search box to narrow it (`is:agent web1`); clear the box for all sessions again. The dashboard shows the agents in tmux windows FeroSSH shows; one in a tmux session nobody attached is not in it until you attach it. Every agent pane has the quiet watch of [Notifications](#notifications-coding-agents-and-long-jobs) on, so with Settings > Desktop notifications on you are told when an agent stops, even one that sends no notification of its own.

**Answering from the notification.** With Session > Coding agents > Answer yes-or-no questions from the notification ticked (and Settings > Desktop notifications on), an agent that stops at a yes-or-no question says the question in its Windows notification. Clicking it brings FeroSSH forward with a small dialog instead of switching tabs: *Yes* or *No* answers the agent, *Cancel* shows its pane. FeroSSH recognises two kinds of question at the bottom of the agent's screen: a numbered choice with a `1. Yes` and a `No` line (as Claude Code and Codex ask; the answer is that line's number), and a line ending in `[y/n]`, `(y/n)` or `(Y)es/(N)o` (as aider and most command-line tools ask; the answer is `y` or `n` and Enter). Before typing, it checks the agent still asks the same question; if it moved on, nothing is typed. The dialog says so when the host has the Production theme. It is off unless you turn it on (`AnswerFromNotification`).

**Seeing what an agent changed.** Session > Coding agents > Show what changed here (side-by-side git diff), from an agent's pane, opens a tab with the changes in the git repository the agent works in, compared with the last commit: a summary and the list of changed files first, then each file with the old version on the left and the new one on the right, line numbers on both sides, removed lines tinted red and added lines green, a line that replaced another next to it. New files git does not track yet show as added (up to 500, and none over 1 MiB). Long lines wrap; in a narrow pane the old lines go above the new ones. In the tab, `r` asks git again, `n` and `p` go to the next and previous file, and the arrow keys, PgUp/PgDn (or Space and `b`) and Home/End scroll; nothing typed there goes to the server. For an agent in tmux, FeroSSH asks tmux which folder the agent is in; from any other SSH pane it uses the folder the shell is in (FeroSSH knows it from OSC 7 or the prompt). git runs on the server over the pane's own SSH connection, with `sh`, so the server needs both (Linux and macOS servers have them). The folder goes to git on its input, never in the command, and every character of the answer is shown as text, so a file cannot send the terminal escape sequences. The tab takes the host's colours, so a Production host stays red. Very large changes show in part (8 MiB of diff, 200,000 lines), and the tab says so.

The menu offers `claude`, `codex` and `aider` until you list your own: one `AgentCommand` line each in `ferossh.conf`, such as `AgentCommand claude --model opus` (see [Settings](settings.md)). The line is typed at the server's shell exactly as written.

### Pasting screenshots and files for an agent

Agents on a server take images and files by path, and cannot read the clipboard of your PC. Tick Session > Coding agents > Paste and drop files: upload and type the path, and then:

- **Paste** (Ctrl+Shift+V, Shift+Insert, or a right click with right-click paste on) with an image on the clipboard, such as a screenshot from Win+Shift+S, saves it as a PNG named after the time (`image-20261118-171530.png`), uploads it, and types its path at the cursor.
- **Paste** with files copied in Explorer uploads those files and types their paths.
- **Dropping** files from Explorer, or File transfer's Local list, on a terminal pane does the same without asking, instead of uploading to the shell's folder. The exception is a coding agent's pane on a Production-themed host while Confirm scripts and agents on Production hosts is on: there FeroSSH asks before it uploads (see [Coding agents on Production hosts](#coding-agents-on-production-hosts)).

Files go to `~/.cache/ferossh/paste/` on the pane's host, made when needed; a name already there is kept and the new file numbered, `image (2).png`. The paths are typed once every file of the paste or drop is up, quoted for the shell where needed and with a space after each, as one bracketed paste, the way a terminal types dropped files; nothing is pressed after them. Text on the clipboard pastes as text, as before. Only files go this way: use File transfer for folders. The uploads run in the transfer queue over SFTP like any other. When one fails, FeroSSH says so with the host and the reason, and its path is not typed; File transfer's queue for that host can retry it (a pasted image is kept for a day for that). FeroSSH does not clean the folder on the server; delete old files there when you like.

It works on panes logged in over SSH, including tmux panes; local shells, Telnet and serial panes have nowhere to upload to. It is off unless you turn it on: dropping on a pane then asks and uploads to the shell's folder, and pasting an image does nothing.

### Opening a web server on the host

Development servers and agents print where they listen, such as `http://localhost:5173/`, but that address is on the server, not on your PC. Tick Session > Coding agents > Offer to forward and open web servers on the host, and when a pane's output shows the address of a web server on the host itself (`http://` or `https://` with `localhost`, `127.0.0.1`, `0.0.0.0`, `[::1]` or `[::]` and a port), a small bar appears in the pane's top right corner: *localhost:5173 is listening on user@build-box.* **Forward and open**.

Click **Forward and open** to forward a port on this PC's loopback to that port on the host (the same number when it is free here, else any free one) and open your browser on it, with the address's path, so `http://localhost:8888/lab?token=…` opens with its token. The forward is listed in Tools > Port forwards like any you add there: stop it there, and a reconnect starts it again. If the host already has a forward to that port, the browser opens on it. Click ✕ to turn the offer down. A port forwarded or turned down is not offered again on that connection; a bar nobody clicks goes after a minute, and comes back if the address is printed again.

Nothing is forwarded without a click. Addresses inside links and window titles are not offered, nor other hosts' addresses. It works on panes logged in over SSH, including tmux panes. It is off unless you turn it on (`PortPreviews`).

### Keys and drawing for agent interfaces

Coding agents draw full-screen interfaces that redraw often. FeroSSH shows each redraw whole: a program that brackets its redraws with synchronized output (mode 2026) is drawn once per redraw, never half done, and a program that asks whether this is supported is told it is. Links a program marks with OSC 8 open with Ctrl+click, after showing where they really go.

Two options in Session > Coding agents make the keys work as these programs expect. Both are off unless you turn them on.

- **Shift+Enter types a new line (sends Esc Enter).** Agents read Enter as "send" and need another key for a new line in their input box. With this on, Shift+Enter sends `Esc Enter`, which Claude Code, Codex and others take as a new line, and which reaches them through tmux too. `ShiftEnter csi-u` in `ferossh.conf` sends `ESC [13;2u` instead.
- **Kitty keyboard protocol for programs that ask for it.** A program can then turn on the kitty keyboard protocol's first level ("disambiguate escape codes"), as many agents do when the terminal has it. While it is on, Escape, Shift+Enter, Ctrl+Enter, Alt+Enter, Shift+Tab, Ctrl+Backspace and Ctrl or Alt with a letter come as unambiguous `CSI … u` codes, so Escape is never mistaken for the start of a key, and Shift+Enter is always `ESC [13;2u`. It ends when the program exits, and at once if you turn the option off. A program asking for more of the protocol is told only this level is on. tmux does not pass the protocol on, so programs inside tmux get xterm's keys and the Shift+Enter option.

### Coding agents on Production hosts

A coding agent runs commands on the host with your login's rights, so on a host with the red Production theme FeroSSH asks before it gets work:

- **Starting it.** Session > Coding agents > Start ... in tmux from a Production pane asks first.
- **Pasting into it.** Text pasted into an agent's pane on a Production host shows in the question before it goes, and so does text that broadcast input would carry to one. Pasted images and files, and files dropped on its pane, ask before they upload.

What you type key by key goes as always. These questions come with the [Production guard](#production-guard), and Session > Scripts > Confirm scripts and agents on Production hosts turns them off with it.

**An audit trail.** With Session > Coding agents > Log and record agent panes (audit trail) ticked, every coding agent's pane is logged and recorded from the moment it opens, so what the agent did on the host is kept: a sealed log (see [Logs](logs.md)) and a sealed recording you can play back with Session > Logs and recordings > Play a recording. Both are named after the host and the agent, such as `web1_tab3_2026-11-19_0930-claude.log` and `.cast`, and their heading says which agent it was. Turning it on starts them for the agents already running too; stop one with Session > Logs and recordings as usual. It is off unless you turn it on (`AgentAudit`). The recording keeps what the agent and the host showed, not what you typed.

## Remote desktops

Session > Remote desktop > Open remote desktop (Ctrl+Shift+D) shows the Linux desktop (X11 or Wayland) of the host you are logged in to. The server needs an RDP server: xrdp (`sudo apt install xrdp`) or GNOME Remote Desktop. FeroSSH checks that something answers on the server's RDP port (3389, or `DesktopPort`), forwards a loopback port to it through SSH (33900 to 33999, the same for the same host each time) and opens Windows' Remote Desktop Connection there. The server's RDP port can stay closed on its firewall, and the tunnel closes with the connection.

A saved connection of type *Remote desktop (RDP)* without *Via SSH* opens Remote Desktop Connection straight to the machine, for Windows PCs and servers. With *Via SSH* set to another saved connection, FeroSSH logs in there first and tunnels the desktop through it.

Saved desktop and remote app connections (types *Remote desktop (RDP)*, *VNC desktop*, *Virtual desktop (Xvfb)* and *Remote app* with *Via SSH*) open on their own: you get only the desktop or the program's windows, no terminal tab. The SSH login to the *Via* host runs out of sight; its terminal shows in a tab only when the login needs you (a password, a new or changed host key, a failure), and closes once the desktop or program is up. The connection stays while the desktop tab, Remote Desktop Connection or the program runs, and ends with the last of them. Until then the Session manager lists the login and can show it.

`ferossh-desktop.exe` adds Session > Remote desktop > Open remote desktop in a tab (RDP) and (VNC), which show the desktop in a FeroSSH tab or split, keyboard and mouse included. For RDP the tab asks for the user name and password; for VNC the server needs a VNC server such as x11vnc, TigerVNC or wayvnc. The viewer runs in a sandbox with no network access of its own and only connects through the SSH tunnel. The built-in RDP viewer only opens desktops on the SSH server itself; a saved RDP connection to another machine behind it opens in Remote Desktop Connection, which checks that machine's certificate. The first time a desktop opens, the built-in viewer pins the RDP server's certificate (one per SSH host and port, in `rdp_certs` in FeroSSH's settings folder); later, a server with another certificate is not logged in to and the tab says so. If the server really got a new certificate, remove its line from `rdp_certs`. When a VNC server takes a password, FeroSSH uses it even if the server would also let it in without one; a VNC server that asks for none gets a warning in the tab, and a virtual desktop's, which FeroSSH started with a password, is not opened at all, since something between may have removed the password from its offer. A saved VNC connection whose *Host* is another machine than its *Via* SSH server asks before it opens, once each time FeroSSH runs and again when its host, port or SSH server changes: SSH encrypts only the way to the SSH server, and VNC travels unencrypted from there. The desktop takes the size of its pane where the server can change it, and is scaled otherwise.

#### Desktop clipboard

Copied text can go both ways between remote desktops and Windows, text only, up to 1 MiB: what you copy on the desktop lands on the Windows clipboard, and what you copy on Windows reaches the desktop when you click or type in its tab (only the desktop you are working in sees it). It is **off until you turn it on**: copied text can carry passwords and other secrets, and a desktop can hand Windows anything, so choose *Both ways* or one way in Session > Remote desktop > Clipboard. The change applies at once to the desktops already open; a saved connection can narrow it further, never widen it, with *Desk clip* in the connection form (`DesktopClipboard`). It covers:

- **Desktops in a tab** (`ferossh-desktop.exe`): RDP over the clipboard channel (plain Unicode text, no files or pictures), VNC as the protocol has it (Latin-1: other characters arrive as `?`). The sandboxed viewer reads what the server sends; the window takes text only the ways the setting allows.
- **Virtual desktops**: the server is told too (x11vnc `-seldir`/`-nosel`, Xvnc `-AcceptCutText`/`-SendCutText`), so it holds back what should not leave it, whatever viewer you use. *Also share mouse selections from virtual desktops* in the same menu (off by default) sends text you only select there too (how xterm copies). TigerVNC's Xvnc, which a virtual desktop that follows its pane uses, shares the clipboard reliably both ways; x11vnc's own clipboard handling is less dependable.
- **Remote Desktop Connection** (Open remote desktop): it shares the clipboard both ways or not at all, so any setting other than *Both ways* turns its clipboard off. FeroSSH then opens it with a connection file that says so.
- **Your own VNC viewer** for a virtual desktop in `ferossh.exe`: TigerVNC Viewer gets the same ways; for the others the server's limits apply.

### Virtual desktops (Xvfb)

Session > Remote desktop > Open virtual desktop (Xvfb) gives you a graphical desktop on a server that has no screen and no RDP server: a headless box, a VM, a build machine. Over the SSH connection FeroSSH starts Xvfb (a virtual X display that exists only in memory) and x11vnc on the server, runs a desktop session in that display, and shows it through the SSH tunnel. Nothing is installed or left behind on your PC, and no X server runs on Windows.

The server needs `Xvfb`, `x11vnc` and `xauth`. If one is missing, FeroSSH says which and how to install it (Debian and Ubuntu: `sudo apt install xvfb x11vnc xauth`; Fedora: `sudo dnf install xorg-x11-server-Xvfb x11vnc xorg-x11-xauth`, with EPEL on RHEL; Arch: `sudo pacman -S xorg-server-xvfb x11vnc xorg-xauth`).

What runs in the display is your `~/.xsession` if you have one, else the first desktop it finds (Xfce, MATE, LXQt, LXDE, Plasma, `x-session-manager`), else an `xterm`. To run something else, such as one program, set *Virtual desk* in the connection's settings (F2), for example `firefox` or `startxfce4`; `VirtualDesktop` in [Settings](settings.md). The display takes the size of the pane you opened it from (see [Desktop size](#desktop-size)).

A virtual desktop can also be a saved connection: in the Connection manager, make a new connection of type *Virtual desktop (Xvfb)*, pick the SSH connection it runs on under *Via SSH*, and optionally the program, size and clipboard. Opening it logs in there and starts the desktop, with no terminal tab (see [Remote desktops](#remote-desktops)).

It is locked down on the server too: the display has its own random cookie, so other users there cannot draw on it or read its keys; x11vnc listens on the server's loopback only and asks for a fresh random password, which FeroSSH gives it on its standard input, so it never shows in `ps`. In `ferossh-desktop.exe` the desktop opens in a FeroSSH tab and the password is sent for you. In the standard `ferossh.exe` FeroSSH opens the VNC viewer installed on your PC (TigerVNC, TightVNC, RealVNC or UltraVNC) on the tunnel and prints the password in the terminal for you to type; without a viewer it prints the address and password.

The virtual desktop stops, and Xvfb, x11vnc and everything started in the display end, when you close its tab, choose Session > Remote desktop > Stop virtual desktop, close the connection, or end the desktop session (log out, or close the one program). Choosing Open virtual desktop again while one runs opens another viewer on the same desktop.

### Remote apps (native windows)

Session > Remote apps > Run remote app... starts one Linux program on the host you are logged in to and shows its windows as ordinary windows on your Windows desktop, not as a desktop in a tab: each has its own taskbar button, moves and resizes like a local window, and copy and paste can work both ways once you turn the clipboard on (see below). It uses [FeroStream](https://github.com/jkobierczynski/FeroStream). The command palette opens with this connection's program, the ones you ran lately and `xterm`; type any other command line (`gimp ~/a.png`, `xterm -T 'my term'`) and press Enter. Only quotes are understood, no other shell syntax: the program runs directly.

There is no second login. The FeroStream agent runs on the same SSH connection as the pane, as one more channel of it, and runs the program in an Xvfb display on the host (`sudo apt install xvfb`; Fedora: `xorg-x11-server-Xvfb`). When the connection drops while the program still runs, its windows stay and fill again once FeroSSH has reconnected; when you close the pane or its connection, the program keeps running on the host, and Session > Remote apps > Reattach remote app brings its windows back, from this PC or another.

The host needs the agent. When it has none, or another version, and Session > Remote apps > Copy the agent to hosts is ticked (the default), FeroSSH copies its own over the same channel to `~/.local/bin/ferostream-agent` (`RemoteAppAgentPath`), checked by size and checksum, readable only by you. FeroSSH takes the agent from the folder `ferossh.exe` is in: download `ferostream-agent` (for x86-64 hosts) and `ferostream-agent-aarch64` (64-bit ARM, such as a Raspberry Pi) from the same release page and keep them there. They are static Linux programs, built from the FeroStream code this FeroSSH was built with, and listed in the release's `SHA256SUMS.txt`. For other hosts, build the agent yourself and name it `ferostream-agent-<arch>` as `uname -m` names the machine; [Building the FeroStream agents](../../README.md#ferostream-agents) shows how, on Windows too. Turn copying off to use only an agent installed on the host. With Session > Remote apps > Remove the agent when done ticked (off by default), the agent deletes itself when its last program has ended, so nothing is left on the host. Both choices can be set per connection too (*Copy agent* and *Clean up* under Extras in the connection form).

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

The window around the panes (the menu bar and its menus, the title bar, the tab strip, the tools rail and the tools pane: Connection manager, Session manager, the connection form, File transfer, Port forwards, Scripts, Known hosts) has Windows' light look unless you pick something else in Settings > Window colours: the focused pane's colours (dark for a dark theme, red for Production), Windows' own light or dark mode, one of seven neutral greys from Soft grey to Black, or one of eight tinted neutrals that are not grey: Sand, Sage, Mist and Taupe (light), Slate, Moss, Espresso and Heather (dark). The menus and their submenus take the menu bar's colours. Dark buttons and scroll bars need Windows 10 version 1903 or later; the title bar takes the theme's colour on Windows 11.

Resting the mouse on a theme, a custom theme or a window colour in these menus shows it right away, in the focused pane and the window around it. Pick it to keep it; close the menu without a choice (Esc, or a click elsewhere) and the old look comes back. Fonts are not shown this way, because a new font size would resize the remote terminal; a font, and a custom theme's font, change when you pick them.

### Custom themes

Settings > Custom themes lists your own themes. **Save this look as a theme...** writes the focused pane's colours, font and size to a file in the `themes` folder next to the settings; the file name is the theme's name (without `#` or `"`). The window colours go in too when they are one of the fixed looks (a grey or a tinted one); with Light or Follow Windows none are saved, and with Follow the terminal theme only a custom theme's own window colours are. Picking a custom theme gives the focused pane its colours, its font if it names one that is installed, and its window colours if it has some. Host rules, `Profile` themes and `--theme` can name a custom theme like a built-in one; they give the pane its colours and font, and its own window colours (`WindowBackground` and the rest) show while Window colours follow the terminal theme, but a `Window sand` line only takes effect when you pick the theme from the menu. Saving a pane that has the Production or Staging theme keeps that theme on the pane, so its warnings stay.

Each theme is a small text file you can change in any editor; the menu reads the folder again each time it opens. Every line is optional except that it needs a name (the file name serves). `Base` names a built-in theme:

```
Name       Sandstorm
Base       Nord             # start from a built-in theme
Font       Cascadia Mono
FontSize   11
Foreground #D8DEE9
Background #2E3440
Cursor     #88C0D0
CursorText #2E3440
Color0     #3B4252          # Color0 to Color15: the ANSI colours
Window     sand             # a grey or tinted look: softgrey ... black, sand ... heather
```

Instead of `Window`, a theme can give its own window colours:

```
WindowBackground #ECE5D8
WindowText       #2A241C
WindowAccent     #8C5A1E          # optional; the text colour when left out
```

`Window` takes only the fixed looks (the seven greys and eight tints), not `light`, `windows` or `theme`. When a file has both, `WindowBackground` and `WindowText` win over `Window`.

A file that cannot be used at all (a colour that is not `#RRGGBB`, a name that is taken) shows greyed in the menu with the reason. A smaller mistake (an unknown line, a font size outside 6 to 72) leaves the theme selectable but marked "(see file)", with the mistaken line ignored. Window colours whose text reads below 4.5:1 on the window, the menu bar or the tabs are left out, and an accent below 3:1 is replaced by the text colour; both also mark the theme "(see file)".

## Server status bar

Settings > Server status bar adds one quiet row under the panes with the load of the server the focused pane is logged in to: CPU, memory in use, how full the root file system is, and network traffic in and out (every interface but loopback). A figure at 90% or more turns amber. It is off unless you turn it on.

While it is on, FeroSSH runs one small read-only command per server on a channel of its own (no terminal, nothing in your shell's history): every 2 seconds it reads `/proc/stat`, `/proc/meminfo`, `/proc/net/dev` and `/proc/uptime` and runs `df -kP /`. The sandboxed worker reads that output and passes on only the numbers. Linux servers show everything; a server without `/proc` (macOS, the BSDs) shows only its disk, and one that cannot run `sh` (a Windows server, an SFTP-only account) says it does not report its load. Turning the bar off stops the command on every server.

## Notifications (coding agents and long jobs)

Settings > Desktop notifications lets a pane tell you when it wants you, so you can leave a coding agent (Claude Code, Codex, aider) or a long build running in a tab and work elsewhere. It is off unless you turn it on. When a pane asks for attention and you are not looking at it, its tab gets an amber diamond (◆) until you go to it, and unless the pane is in sight in the window in front, Windows shows a notification with the pane's name and the message. Click it to bring FeroSSH forward on that pane. The FeroSSH icon stays in the notification area while something waits for you; clicking it goes to a pane that wants you, and it goes when you come back to FeroSSH. Windows 10 also keeps missed notifications in its notification centre; Windows 11 may not keep them once they time out, so look for the tab badges there. One pane raises at most one notification every 10 seconds.

A pane asks for attention when:

- The host sends a notification escape sequence: OSC 9 (iTerm2's, used by Codex CLI and by Claude Code), OSC 777 `notify` (Ghostty's and urxvt's) or OSC 99 (kitty's). Claude Code sends one when its notification channel is iTerm2: run `/config` in Claude Code and set Notifications to iTerm2 (the `preferredNotifChannel` setting, value `iterm2`). Codex sends one when its notification method is OSC 9, since it does not recognise FeroSSH by itself: in `~/.codex/config.toml`, under `[tui]`, set `notification_method = "osc9"` (and leave `notifications` on, its default). A shell hook can send one too: `printf '\e]777;notify;Build;done\a'`. With FeroSSH's tmux integration these reach FeroSSH from every tmux pane; inside tmux run by hand, tmux keeps them to itself by default (its `allow-passthrough` option can let some through), so use the bell or the quiet watch there.
- The bell rings (Settings > Notify on the bell too, on by default while notifications are on).
- Its output goes quiet (Session > Notify when this pane goes quiet, per pane): after at least 8 seconds of output, 5 seconds of silence mean the job ended or is waiting for an answer. This catches an agent or a script that sends nothing. What you type starts the watch over, so your own typing does not count.

The message is the host's text, shown as plain text: control and hidden formatting characters are taken out and it is cut at 256 characters (Windows may shorten a long message a little more, and a long heading too). Notifications use Windows' notification-area balloons, which Windows 10 and 11 show as notifications, so nothing has to be installed or registered. Windows' Do not disturb (Focus assist) holds them back like any other notification.

## Images in the terminal

Programs on the host can print pictures straight into the terminal: plots, thumbnails and previews from tools such as `img2sixel`, `chafa`, `timg`, `kitten icat`, `imgcat`, gnuplot's `sixelgd` terminal or matplotlib's sixel and kitty backends. Turn it on with Session > Show images the host prints (`InlineImages yes` in `ferossh.conf`); it is off by default, and then such output is ignored as before.

FeroSSH understands three ways of sending a picture:

- **Sixel** (`img2sixel`, `chafa -f sixel`, `lsix`, gnuplot). With images on, FeroSSH says it has Sixel when a program asks (device attributes), which is how `chafa` and `lsix` find out.
- **The kitty graphics protocol** (`kitten icat`, `timg -pk`, `chafa -f kitty`): pictures sent in the escape sequence, raw or as PNG, compressed or not, in chunks; shown at once or kept by id and placed later, cropped or scaled to a number of cells, and deleted again. FeroSSH never reads a file on your PC that the host names: transmission by file, temporary file or shared memory is refused, and `kitten icat` then sends the data itself.
- **iTerm2 inline images** (`imgcat`, `timg -pi`): PNG only, with the width and height in cells, pixels or percent. A file the host offers for download (`inline=0`) is ignored.

A picture covers the cells under it, from the cursor: it scrolls with the text, goes into the scrollback, and text written over it replaces that part, as in xterm. A new font size keeps a picture on its cells, so it scales with them.

Inside FeroSSH's tmux integration pictures arrive too, since tmux passes a pane's output on unchanged; programs that wrap them for tmux (`ESC P tmux;`) are unwrapped. Restoring a tmux session later shows only its text.

Each picture is decoded by FeroSSH's own small decoders, in safe Rust and fuzz-tested, and is limited to 8192 pixels a side, 4 million pixels and 24 MB of encoded data; a terminal keeps 64 MB of pictures at most, dropping the oldest first. A picture over the limits is refused, never cut down from a huge allocation.

## Selecting, copying and scrollback

| To | Do |
| --- | --- |
| Select characters, words, lines | Drag, double-click, triple-click |
| Select a block | Alt+drag |
| Select while a program uses the mouse | Shift+drag |
| Copy / paste | Ctrl+Shift+C / Ctrl+Shift+V (or Ctrl+Insert / Shift+Insert) |
| Open a link | Ctrl+click on a marked or plain-text link (FeroSSH shows its real target first; `ssh://` and `sftp://` open in a new tab) |
| Scroll back / forward a page | Shift+PgUp / Shift+PgDn |
| Jump to the top / bottom | Ctrl+Shift+Home / Ctrl+Shift+End |
| Find in scrollback | Ctrl+Shift+F, then Enter for older, Shift+Enter for newer |
| Stop following at a match | Edit > Stop following at a match... (see [Highlighting and following output](#highlighting-and-following-output)) |

While you are scrolled back, a thin bar at the right edge shows where you are. Edit > Copy on select and Edit > Right-click pastes turn on the PuTTY habits. Scrollback is 10,000 lines unless you change it (`Scrollback`); Edit > Clear scrollback forgets it.

## Command suggestions

Edit > Suggest commands as you type (off until you turn it on) shows, greyed after the cursor, the most recent command you ran on this host that starts with what you have typed. When the host has none yet, a short list of common commands (`git status`, `systemctl status `, `docker compose up -d` and so on) fills in after two characters.

| To | Press |
| --- | --- |
| Type the whole suggestion | Right arrow, End, Ctrl+E or Ctrl+F |
| Type its next word | Ctrl+Right |
| See another match | Ctrl+Up, Ctrl+Down |
| Forget the one shown (a built-in one until FeroSSH restarts) | Shift+Delete |

These are the default keys: Settings > Key bindings lists them under Suggestions, where you can change, add or take away keys. They only work while a suggestion shows; the rest of the time the same keys go to the host as usual (End and Ctrl+E still move to the end of the line).

Any other key goes to the host as usual, and typing on simply changes or hides the suggestion. A command you forgot with Shift+Delete comes back once you run it again. On a slow link, editing keys (Backspace, Tab, arrows) pressed before the host has answered the previous key turn suggestions off for the rest of that line, since FeroSSH can no longer tell which line is final. Nothing shows while you are scrolled back, in a full-screen program such as vim or less, or when the shell already shows its own suggestion after the cursor (fish, zsh-autosuggestions, PowerShell's PSReadLine).

FeroSSH reads the command from the screen, from the end of the prompt to the end of the line, once Enter runs it, so Tab completion, the shell's own history and editing keys all count. It keeps it in a history for that host (user, host and port; a tmux pane shares its connection's), up to 1,000 commands. What it never keeps:

- anything the host does not echo, such as a password typed at `Password:`, since it never appears on the screen;
- lines typed at a prompt that asks for a password, passphrase, PIN, token or code, even when they are echoed;
- commands that match common secret patterns: `PASSWORD=...`, `password = '...'`, `--token x`, `api_key: ...`, `mysql -pSecret`, `curl -u user:pass`, `sshpass -p`, `https://user:pass@host` (a secret in another form, such as a bare argument, can still be kept: start such a line with a space);
- lines that start with a space, as bash's `ignorespace` does.

The histories stay on this PC in `%LOCALAPPDATA%\FeroSSH\history`, one file per host, encrypted with your Windows sign-in (DPAPI): another Windows account, or the files copied to another PC, cannot read them. Edit > Clear command history for this host, or for all hosts, deletes them. Turning suggestions off stops reading what you type straight away.

## Highlighting and following output

Keywords in the output show in colour, as in MobaXterm: errors and failures in red, warnings in yellow, success in green, IP addresses in cyan and MAC addresses in magenta. Only plain text is recoloured, so a program's own colours win, and full-screen programs such as vim and mc are left alone. Session > Highlight keywords turns it off and on; `Highlight no` in a host's `Profile` turns it off for that host only. Your own rules (`HighlightRule` in [Settings](settings.md#highlight-rules)) take a list of words or a regular expression, a colour, bold or underline, and the hosts they are for. They come before the standard set, which `HighlightStandard no` drops.

Following the output stops when you scroll back (Shift+PgUp, the mouse wheel) and starts again when you get back to the bottom: Shift+PgDn or the wheel down to the end, Ctrl+Shift+End or Shift+End, or End or any other key you type. Edit > Stop following at a match... also stops it on its own: type a regular expression in the bar at the bottom of the pane and press Enter. When a line of new output matches, the view follows until that line reaches the middle row of the pane and then holds still there with the match selected, a badge in the top right corner says so, and the output carries on into the scrollback below. Get back to the bottom in any of the ways above to follow again; the next match stops it again. Open the bar again to change the pattern, or empty it and press Enter to stop watching. A `HighlightRule` with `Stop yes` stops at its matches too, for every pane of its hosts. Matches are looked for only while you follow the output, on the main screen, and the view can hold only as far back as the scrollback reaches.

Patterns are regular expressions in [Rust's syntax](https://docs.rs/regex/latest/regex/#syntax), matched one screen line at a time. `\b`, `\w` and `(?i)` know ASCII letters only; accented words still match as written. They run on an engine that takes time in proportion to the line, whatever the pattern, so a pattern cannot hang the window, and patterns over 1,000 characters or too big once compiled are refused.

## Logs

Session > Log to file writes what a tab receives to `%LOCALAPPDATA%\FeroSSH\logs`, one file per tab, optionally with a timestamp on each line, and Session > Logs and recordings > Save this pane's output to a log saves what a pane already holds. Every log is sealed with a chain file, and Session > Logs and recordings > Check a log for changes tells you whether it was changed later. Session > Logs and recordings > Record this pane keeps what a pane shows with its timing, as an asciicast `.cast` file, and Session > Logs and recordings > Play a recording plays one back in a tab, with pause, seeking and speed. [Logs](logs.md) explains all of this, including what the `.chain` files are.

## Making a key and putting it on a server

Tools > New SSH key... makes a key without OpenSSH installed. Choose what kind:

- **Ed25519 key file**, the usual kind, like `ssh-keygen -t ed25519`. Give it a passphrase to have it encrypted, or leave the passphrase empty.
- **Security key or phone** (ECDSA or Ed25519) or **Windows Hello**, like `ssh-keygen -t ecdsa-sk`. Windows asks you to touch the key, scan the QR code with your phone or confirm with Windows Hello. The file holds no secret.

The file goes in `%USERPROFILE%\.ssh` under the name OpenSSH tries by itself (`id_ed25519`, `id_ecdsa_sk`, `id_ed25519_sk`), or `id_ed25519_2` and so on when that is taken; Browse... picks another place. FeroSSH never writes over an existing key. Afterwards it shows the public key line, and when the focused pane is logged in to an SSH host it offers to copy the key there.

Session > Copy public key to this server... does what `ssh-copy-id` does for the host the focused pane is logged in to: pick a `.pub` file and FeroSSH adds its line to `~/.ssh/authorized_keys` there, unless the key is there already. It makes `~/.ssh` (700) and the file (600) when missing. The key line goes to the server as data, never as part of a command, so nothing in it runs. The server needs a Unix shell (`sh`); Windows servers are not supported.

## Security keys and passkeys

FIDO2 security keys and passkeys work through Windows' own dialog: touch a USB or NFC key, scan the QR code with your phone, or confirm with Windows Hello. Use key files such as `id_ecdsa_sk` or `id_ed25519_sk` (made with Tools > New SSH key, `ssh-keygen -t ecdsa-sk` or `ferossh.exe --new-security-key`), or keys the OpenSSH agent holds. See [Getting started](getting-started.md#command-line).

## Updates

Help > Check for updates says whether a newer release exists and offers to open its page; Help > Check for updates weekly does it once a week, quietly unless there is one. FeroSSH never downloads or installs anything by itself, and without either it never contacts anything but your servers. If you installed FeroSSH with winget, `winget upgrade jkobierczynski.FeroSSH` updates it. Each release is code signed; [Verifying a download](verifying-downloads.md) shows how to check it.
