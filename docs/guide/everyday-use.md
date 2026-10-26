# Everyday use

This page is a tour. The README describes each feature in full.

## Tabs and splits

The window starts as one pane, with no tab strip. Ctrl+Shift+T opens a tab and the strip appears; Alt+Shift+= and Alt+Shift+- split the pane. A new tab or pane on a host you are already logged in to opens another channel on the same connection, so there is no second login.

- Alt+arrows moves the focus, Alt+Shift+arrows grows the focused pane, and dragging a divider resizes.
- Drag a tab to reorder it. Ctrl+Shift+drag a pane onto another pane's edge to move it there, or onto its middle to swap the two.
- Ctrl+Shift+J joins a tab into the tab before it, as a pane. Dropping a pane on the tab strip makes it a tab again.
- View > Font and View > Theme change only the focused pane. Session > Save font and theme for this host keeps them for next time.

## The session manager

Ctrl+Shift+S shows a side pane with your hosts from `~/.ssh/config`, your saved sessions and your imports. Type to filter, Enter connects in this tab, Shift+Enter in a split, and Ctrl+D marks a favourite. A `# folder: Name` comment above a `Host` line groups hosts in your ssh_config.

Insert (or "+ New session") opens the session form, and F2 edits the selected session. A session can be an SSH terminal, a Remote desktop (RDP) or a VNC desktop. The Password field picks where a password comes from: Windows Credential Manager, 1Password, Bitwarden, KeePassXC or a command of your own. FeroSSH itself never stores a password.

## File transfer

Ctrl+Shift+E shows the focused pane's host over SFTP in the side pane, on the same connection. Enter opens a folder or downloads a file, U uploads, D downloads, Del deletes after asking. You can also drop files from Explorer on a terminal pane. Tab moves to the transfer queue, where Space pauses, R retries and X cancels. A dropped connection pauses the queue, and it carries on where each file stopped after you log in again.

Files are never overwritten without asking, and file names Windows cannot hold are made safe or refused. Session > Verify transfers compares SHA-256 sums on both ends after each file.

## Port forwards

`LocalForward`, `RemoteForward` and `DynamicForward` in your ssh_config start when you log in. View > Port forwards (Ctrl+Shift+P) lists them and adds new ones: type `-L 8080:db.internal:5432`, `-R 9000:localhost:3000` or `-D 1080` and press Enter. A dynamic forward is a SOCKS 4, 4a and 5 proxy. A tab whose connection forwards shows `⇄`, in amber when the server can reach back into your PC.

## tmux

Session > Attach tmux turns tmux windows and panes into native FeroSSH tabs and panes. You get no prefix key and no status bar, and you keep native scrollback, copy and search. Close FeroSSH, or lose the network, and everything keeps running on the server. Attach again, from this PC or another, and the tabs, splits, fonts and themes come back. tmux is never started unless you ask; to attach on every login to a host, set `Tmux` in its profile (see [Settings](settings.md)).

## Remote desktops

Session > Open remote desktop (Ctrl+Shift+D) shows the Linux desktop of the host you are logged in to. The server needs xrdp or GNOME Remote Desktop. FeroSSH tunnels the RDP port through SSH and opens Windows' Remote Desktop Connection on it, so the server's RDP port can stay closed on its firewall.

`ferossh-desktop.exe` can also show the desktop, RDP or VNC, in a FeroSSH tab or split.

## Logs and scrollback

Session > Log to file writes what a tab receives to `%LOCALAPPDATA%\FeroSSH\logs`, one file per tab. Passwords and passphrases never reach the log. Scrollback is 10,000 lines unless you change it (`Scrollback` in [Settings](settings.md)), and Shift+PgUp scrolls back.

## Colour themes

There are twelve themes (Nord by default) and two warning themes, Production (red) and Staging (amber). Rules can pick the theme by host pattern or by ssh_config `Tag`, so production servers always look like production. See `ThemeRule` in [Settings](settings.md).

## Updates

Help > Check for updates says whether a newer release exists and offers to open its page; Help > Check for updates weekly does it once a week, quietly unless there is one. FeroSSH never downloads or installs anything by itself, and without either it never contacts anything but your servers. If you installed FeroSSH with winget, `winget upgrade jkobierczynski.FeroSSH` updates it.
