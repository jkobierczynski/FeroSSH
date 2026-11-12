# Getting started

## Download

Every release on the [Releases page](https://github.com/jkobierczynski/FeroSSH/releases) has two exes, each in an x64 and an ARM64 build:

| File | Pick it when |
| --- | --- |
| `ferossh.exe` | You want an SSH client. This is the one most people want. |
| `ferossh-desktop.exe` | You also want to see Linux desktops (RDP or VNC) inside a FeroSSH tab. It is the same client plus a sandboxed desktop viewer, and about 2.6 MB larger. |
| `ferossh-arm64.exe`, `ferossh-desktop-arm64.exe` | The same two for Windows on ARM (Snapdragon laptops, Surface Pro X, Windows in a VM on an Apple silicon Mac). They run natively, so faster and with less battery than the x64 exe under emulation. |

There is no installer. Put the exe wherever you like, for example `%LOCALAPPDATA%\Programs\FeroSSH`, and pin it to the Start menu or taskbar. It runs on Windows 10 1903 or later and Windows 11, on x64, and on ARM64 with the `-arm64` exes. To carry it on a USB stick, make a folder named `FeroSSH-data` next to the exe: FeroSSH then keeps its settings, connections and other data in that folder, though Windows itself may still keep some traces on the PC (see [Portable use](everyday-use.md#portable-use)).

Before you run a download, you can check that it is the one the project built: see [Verifying a download](verifying-downloads.md).

## First connection

Start `ferossh.exe` and type a host when it asks, or give one on the command line:

```
ferossh.exe user@host
ferossh.exe user@host:2222
ferossh.exe -l user -p 2222 host
ferossh.exe myalias                    # a Host from ~/.ssh/config
```

The first time you connect to a server, FeroSSH shows its SHA-256 fingerprint and randomart. Compare the fingerprint with the one your administrator gave you (on the server, `ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub` prints it), and type `yes` to trust it. The key is saved, hashed, in `~/.ssh/known_hosts`, the same file OpenSSH uses.

FeroSSH then signs in with the first method that works:

1. keys in the Windows OpenSSH agent (this also covers the 1Password and Bitwarden SSH agents);
2. keys in Pageant;
3. key files: `~/.ssh/id_ed25519` and `~/.ssh/id_ecdsa`, or the `IdentityFile` lines of your ssh_config (it asks for the passphrase and never keeps it);
4. FIDO2 security keys and passkeys, through Windows' own dialog;
5. keyboard-interactive (one-time codes), then a password.

## Using your OpenSSH config

If you already use OpenSSH, FeroSSH reads `~/.ssh/config` as it is: `HostName`, `User`, `Port`, `IdentityFile`, `Include`, `Tag`, `ProxyJump`, `ProxyCommand` and the forwarding keywords, and FeroSSH's own `ProxyServer` for a SOCKS 5 or HTTP proxy. It never writes to that file. Your hosts show up in the connection manager (Ctrl+Shift+S).

If you don't have an ssh_config, open the connection manager, press Insert ("+ New connection") and fill in the form. Saved connections go to `%APPDATA%\FeroSSH\sessions.conf`.

Coming from PuTTY, KiTTY, WinSCP or another client? File > Import connections brings your saved sessions over, without their passwords.

## Command line

| Command | What it does |
| --- | --- |
| `ferossh.exe` | Asks for a host. |
| `ferossh.exe [user@]host[:port]` | Connects to a host. |
| `ferossh.exe -l user -p port host` | The same, OpenSSH style. |
| `ferossh.exe COM3:115200` | Opens a serial console, optionally with speed, framing and flow control (`COM3:9600,7E1,xonxoff`). |
| `ferossh.exe sftp://user@host[:port][/folder]` | Opens File transfer on the host without a shell, for SFTP-only accounts. |
| `ferossh.exe telnet://host[:port]` | Opens a Telnet session (unencrypted; port 23 unless given). |
| `ferossh.exe winrm://[user@]host[:port]` | Opens PowerShell on a Windows server over WinRM (port 5985 unless given; asks the password when a user is given). `winrms://` uses HTTPS, port 5986. |
| `ferossh.exe --theme NAME host` | Connects with a colour theme, for example `--theme Production`. Handy for desktop shortcuts. |
| `ferossh.exe --no-sandbox host` | Runs the SSH worker without its sandbox. Needed only for an SSH server on the same PC (see [Troubleshooting](troubleshooting.md)). |
| `ferossh.exe --workspace NAME` | Opens a saved workspace: its tabs and split panes, each on its connection (see [Workspaces](everyday-use.md#workspaces)). A path to a `.workspace` file works too. |
| `ferossh.exe --local [command]` | Opens a local shell or command instead of SSH, for example `--local wsl -d Ubuntu`. |
| `ferossh.exe --check-sandbox` | Reports what the sandbox can do on this PC. Pipe it through `more` to read it all. |
| `ferossh.exe --new-security-key [--hello] [--ed25519] FILE` | Makes a new FIDO2 key file, like `ssh-keygen -t ecdsa-sk`. `--hello` uses Windows Hello, so no hardware key is needed. |

## The first shortcuts to learn

| Shortcut | Action |
| --- | --- |
| Ctrl+Shift+S | Connection manager |
| Ctrl+Shift+L | Session manager (open sessions) |
| Ctrl+Shift+N | Connect to another host in a new tab |
| Ctrl+Shift+T | New tab on the same host, with no second login |
| Alt+Shift+= / Alt+Shift+- | Split the pane right or down |
| Ctrl+Shift+E | File transfer for this pane's host |
| Ctrl+Shift+C / Ctrl+Shift+V | Copy and paste |
| Ctrl+Shift+F | Find in scrollback |
| Ctrl+Shift+A | Command palette: every menu item, connection and script |

Every other menu item and key is in the [Menu reference](menus.md).
