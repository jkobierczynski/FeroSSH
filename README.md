# FeroSSH

*Secure connections. Forged in Rust.*

FeroSSH aims to be the smallest, most secure open-source SSH client for Windows: a native PuTTY replacement with tabs, splits, tmux integration and file transfer, shipped as one signed exe with nothing to install.

> **Status:** milestone 5 (file transfer over SFTP). FeroSSH connects with russh from a sandboxed worker process, checks host keys against `~/.ssh/known_hosts`, and signs in with the Windows OpenSSH agent, Pageant, key files, keyboard-interactive or a password. Tabs, splits and a session manager came in milestone 4.

## Trying the build

Download `ferossh-exe` from the latest CI run and start it with a host, or without one to be asked:

```
ferossh.exe [user@]host[:port]
ferossh.exe -l user -p 2222 host
ferossh.exe myalias            # a Host from ~/.ssh/config
ferossh.exe --local [command]  # a local shell, as in milestone 2
```

`HostName`, `User`, `Port`, `IdentityFile`, `Tag` and `Include` are read from `~/.ssh/config`.

- **Tabs and splits.** The window starts with one pane and no tab strip. Ctrl+Shift+T opens a tab, Alt+Shift+= and Alt+Shift+- split the pane; the tab strip appears once there are two tabs. A new tab or pane on a host you are already logged in to opens another channel on the same connection, so there is no second login. Drag a divider to resize, drag a tab to reorder, and Ctrl+Shift+drag a pane onto another pane's edge to move it there (or its middle to swap them). To merge a tab back in as a pane, drag the tab down out of the strip onto a pane of the tab that appears, or use Window > Join tab into the tab before it (Ctrl+Shift+J); dropping a pane on the tab strip makes it a tab again.
- **Session manager.** Ctrl+Shift+S shows the hosts from `~/.ssh/config` in a side pane, hidden by default. Type to filter, Enter connects in this tab (Shift+Enter in a split), Ctrl+D marks a favourite. A `# folder: Name` comment above a `Host` groups hosts. View > Side pane on right moves it; drag its edge to resize it.
- **PuTTY import.** File > Import PuTTY sessions reads PuTTY's saved SSH sessions (host, port, user, key file, font) into `%APPDATA%\FeroSSH\putty-sessions.conf` in ssh_config syntax. Your `~/.ssh/config` is never touched, and hosts it already has are skipped. PuTTY `.ppk` key files (v2 and v3) work for Ed25519 and ECDSA P-256; RSA `.ppk` keys work through Pageant.
- **File transfer.** Ctrl+Shift+E shows the focused pane's host over SFTP in the side pane, on the same connection (no second login). Enter opens a folder or downloads a file, D downloads (folders whole), U or Ctrl+Shift+U uploads, Del deletes after asking, Tab moves to the transfer queue (Space pauses or resumes, R retries, X cancels, C clears finished ones). Drop files or folders from Explorer on a pane to upload them to the shell's current folder, after a confirmation. Downloads go to your Downloads folder (`DownloadFolder` in `ferossh.conf` changes it). Three files move at once; a dropped connection pauses the queue and it carries on, from where each file stopped, when you log in again. Session > Verify transfers compares SHA-256 on both ends after each file (the server needs `sha256sum`).
- **Safe file names.** An existing file is never overwritten without asking (Overwrite, Skip or Keep both, Shift for all). A folder download does not follow links that lead outside the folder, and server names Windows cannot hold (`CON`, `a:b`, trailing dots, control characters) are made safe or refused. Partial files are `.part` locally and `.filepart` on the server until complete.
- **Following `cd`.** The file view follows the shell's directory when the shell reports it with OSC 7. fish does by default; for bash, add this to `~/.bashrc` on the server: `PROMPT_COMMAND='printf "\e]7;file://%s%s\e\\" "$HOSTNAME" "$PWD"'`. View > File transfer follows cd (or F in the view) turns it off.
- **Per-host look.** View > Font and View > Theme change the focused pane; Session > Save font and theme for this host keeps them for the next connection.

- **Host keys.** A new host shows its SHA-256 fingerprint and randomart, and you type `yes` to trust it; the key is saved hashed to `known_hosts`. A changed key fills the window with a warning and cannot be bypassed: if you know why it changed, use Session > Replace host key..., which asks twice.
- **Sign-in.** Keys are tried from the OpenSSH agent first, then Pageant, then key files (`id_ed25519` and `id_ecdsa` unless the config names others). Key files may be Ed25519 or ECDSA P-256, encrypted or not; the passphrase is asked in the terminal and never kept. RSA keys work only through an agent, need 3072 bits or more, and sign with SHA-2. Then keyboard-interactive (one-time codes) and passwords.
- **Sandbox.** The worker that talks to the network runs in an AppContainer with network access only: it cannot read your files or keys, and asks the window for every signature. Windows does not let AppContainers reach 127.0.0.1, so use `--no-sandbox` for SSH servers on the same machine.
- **Logs.** Session > Log to file writes what the host sends to `%LOCALAPPDATA%\FeroSSH\logs`, one file per tab named by host, tab and time (`host_tab1_2026-10-24_2215.log`). Plain logs keep the readable text; Session > Log raw output keeps every byte. Passwords and passphrases never reach the log.

| Shortcut | Action |
| --- | --- |
| Ctrl+Shift+N / Ctrl+Shift+R | Connect to another host, reconnect |
| Ctrl+Shift+T / Ctrl+Shift+W | New tab, close the pane |
| Alt+Shift+= / Alt+Shift+- | Split right, split down |
| Alt+arrows / Alt+Shift+arrows | Focus the pane in that direction, grow the pane |
| Ctrl+Tab / Ctrl+Shift+Tab | Next tab, previous tab (Ctrl+Shift+PgUp/PgDn moves it) |
| Ctrl+Shift+J | Join this tab into the tab before it, as a pane |
| Ctrl+Shift+S | Session manager |
| Ctrl+Shift+E / Ctrl+Shift+U | File transfer, upload files |
| Ctrl+Shift+C / Ctrl+Shift+V | Copy the selection, paste (also Ctrl+Insert / Shift+Insert) |
| Ctrl+Shift+F | Find in scrollback (Enter: older match, Shift+Enter: newer, Esc: close) |
| Shift+PgUp / Shift+PgDn | Scroll back a page, forward a page (Ctrl+Shift+Home/End: top, bottom) |
| Mouse drag, double-click, triple-click | Select characters, words, lines; Alt+drag selects a block |
| Shift+drag | Select even when the program has mouse reporting on |
| Ctrl+click | Open a link (OSC 8) after showing its real target |
| Ctrl+= / Ctrl+- / Ctrl+0, Ctrl+wheel | Larger, smaller, default text size |
| Ctrl+Shift+M | Hide the menu bar (Alt shows it again) |
| View > Font, View > Theme | Font family and colour theme for this pane |
| View > Show test pattern | Colours, styles, box drawing, wide and combining characters |

Edit > Copy on select and Edit > Right-click pastes turn on the PuTTY habits; both are off by default.

### Colour themes

Built in: Nord (the default), Dracula, Catppuccin Mocha, Gruvbox Dark, Solarized Dark, Tokyo Night, One Dark, Monokai, Rosé Pine, Everforest, Campbell and One Half Light, plus two warning themes: **Production** (muted red background, red cursor) and **Staging** (amber). `ferossh.exe --theme Production prod-db` opens a session in a given theme, which is handy for desktop shortcuts.

The theme follows the host automatically: a host tagged `production`, `staging` or `development` with ssh_config's `Tag` keyword gets Production, Staging or Nord, and your own rules can match host patterns (`*.prod.example.com !prod-sandbox`) or tags, with an optional red or amber tint over any theme. Rules and per-host profiles live in `%APPDATA%\FeroSSH\ferossh.conf`:

```
ThemeRule
    Hosts *.prod.example.com !prod-sandbox
    Theme Production
ThemeRule
    Tags staging
    Theme Nord
    Tint ffb000 40
Profile web1
    Font Cascadia Mono
    FontSize 12
    Theme Dracula
```

`Tint` mixes a colour (hex) into the theme by 0 to 255. A host's profile wins over the rules, and the rules over `--theme`. The palettes are the themes' published terminal colours; all are MIT licensed except Tokyo Night's terminal port (Apache-2.0).

## Design

- Native Win32 with DirectWrite/Direct2D rendering. No Electron, Qt or Java.
- Two processes: a UI process that never parses network data, and an AppContainer-sandboxed SSH worker that never sees private keys or your files.
- Modern crypto only. Post-quantum `mlkem768x25519-sha256` key exchange first; no SHA-1, CBC, DSA or `ssh-rsa` signatures (see [`crypto_policy.rs`](crates/worker/src/crypto_policy.rs)).
- Keys stay in the Windows OpenSSH agent, Pageant, a FIDO2 token or the TPM. Passwords are never stored.
- OpenSSH `ssh_config` is the session format.

## Workspace

| Crate | Role |
| --- | --- |
| [`app`](crates/app) | Win32 entry point, window, tabs and splits, message loop |
| [`render`](crates/render) | DirectWrite/Direct2D glyph atlas and grid drawing |
| [`term`](crates/term) | VT emulation wrapper, sanitising, paste and clipboard policy |
| [`worker`](crates/worker) | Sandboxed SSH worker: russh sessions, channels, forwards |
| [`ipc`](crates/ipc) | UI/worker message types, framing and fuzz targets |
| [`auth`](crates/auth) | Agent, Pageant, WebAuthn and CNG signing broker |
| [`sftp`](crates/sftp) | SFTP codec, transfer queue and file-tree model |
| [`tmuxcc`](crates/tmuxcc) | tmux control-mode parser and layout mapping |
| [`config`](crates/config) | `ssh_config` parser, known_hosts and PuTTY import |

## Building

Requirements: Rust via rustup (the pinned toolchain in `rust-toolchain.toml` installs itself), Visual Studio Build Tools with the MSVC toolchain, and the Windows SDK. No CMake, NASM or extra C toolchain.

```
cargo build --release -p ferossh
```

The release profile optimises for size and links the C runtime statically, so `target\release\ferossh.exe` runs on a clean Windows 10 1903+ machine.

Checks run in CI: `cargo fmt`, `cargo clippy`, `cargo test`, `cargo deny`, `cargo audit`, SSH tests against a real OpenSSH server on Linux (`cargo test -p ferossh-worker --test sshd`, skipped where `sshd` is not installed), and fuzz smoke runs of the IPC decoder, the SFTP decoder and the terminal (`cargo +nightly fuzz run --target x86_64-unknown-linux-gnu term_feed`).

On Linux or macOS, `cargo run -p ferossh-term --example vttest -- 1 2 8` drives [vttest](https://invisible-island.net/vttest/) through the emulator and prints every screen as text.

## Licence

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in FeroSSH by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
