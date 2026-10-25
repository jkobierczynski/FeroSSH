# FeroSSH

*Secure connections. Forged in Rust.*

FeroSSH aims to be the smallest, most secure open-source SSH client for Windows: a native PuTTY replacement with tabs, splits, tmux integration and file transfer, shipped as one signed exe with nothing to install.

> **Status:** milestone 3 (real SSH in one tab). FeroSSH connects with russh from a sandboxed worker process, checks host keys against `~/.ssh/known_hosts`, and signs in with the Windows OpenSSH agent, Pageant, key files, keyboard-interactive or a password. Tabs arrive in milestone 4.

## Trying the milestone 3 build

Download `ferossh-exe` from the latest CI run and start it with a host, or without one to be asked:

```
ferossh.exe [user@]host[:port]
ferossh.exe -l user -p 2222 host
ferossh.exe myalias            # a Host from ~/.ssh/config
ferossh.exe --local [command]  # a local shell, as in milestone 2
```

`HostName`, `User`, `Port` and `IdentityFile` are read from `~/.ssh/config`.

- **Host keys.** A new host shows its SHA-256 fingerprint and randomart, and you type `yes` to trust it; the key is saved hashed to `known_hosts`. A changed key fills the window with a warning and cannot be bypassed: if you know why it changed, use Session > Replace host key..., which asks twice.
- **Sign-in.** Keys are tried from the OpenSSH agent first, then Pageant, then key files (`id_ed25519` and `id_ecdsa` unless the config names others). Key files may be Ed25519 or ECDSA P-256, encrypted or not; the passphrase is asked in the terminal and never kept. RSA keys work only through an agent, need 3072 bits or more, and sign with SHA-2. Then keyboard-interactive (one-time codes) and passwords.
- **Sandbox.** The worker that talks to the network runs in an AppContainer with network access only: it cannot read your files or keys, and asks the window for every signature. Windows does not let AppContainers reach 127.0.0.1, so use `--no-sandbox` for SSH servers on the same machine.
- **Logs.** Session > Log to file writes what the host sends to `%LOCALAPPDATA%\FeroSSH\logs`, one file per tab named by host, tab and time (`host_tab1_2026-10-24_2215.log`). Plain logs keep the readable text; Session > Log raw output keeps every byte. Passwords and passphrases never reach the log.

| Shortcut | Action |
| --- | --- |
| Ctrl+Shift+N / Ctrl+Shift+R | Connect to another host, reconnect |
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

Built in: Nord (the default), Dracula, Catppuccin Mocha, Gruvbox Dark, Solarized Dark, Tokyo Night, One Dark, Monokai, Rosé Pine, Everforest, Campbell and One Half Light, plus two warning themes: **Production** (muted red background, red cursor) and **Staging** (amber). `ferossh.exe --theme Production prod-db` opens a session in a given theme, which is handy for desktop shortcuts until session profiles arrive.

From milestone 4, the theme follows the host automatically: a host tagged `production`, `staging` or `development` with ssh_config's `Tag` keyword gets Production, Staging or Nord, and your own rules can match host patterns (`*.prod.example.com !prod-sandbox`) or tags, with an optional red or amber tint over any theme. The palettes are the themes' published terminal colours; all are MIT licensed except Tokyo Night's terminal port (Apache-2.0).

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
| [`sftp`](crates/sftp) | Transfer queue and file-tree model |
| [`tmuxcc`](crates/tmuxcc) | tmux control-mode parser and layout mapping |
| [`config`](crates/config) | `ssh_config` parser, known_hosts and PuTTY import |

## Building

Requirements: Rust via rustup (the pinned toolchain in `rust-toolchain.toml` installs itself), Visual Studio Build Tools with the MSVC toolchain, and the Windows SDK. No CMake, NASM or extra C toolchain.

```
cargo build --release -p ferossh
```

The release profile optimises for size and links the C runtime statically, so `target\release\ferossh.exe` runs on a clean Windows 10 1903+ machine.

Checks run in CI: `cargo fmt`, `cargo clippy`, `cargo test`, `cargo deny`, `cargo audit`, SSH tests against a real OpenSSH server on Linux (`cargo test -p ferossh-worker --test sshd`, skipped where `sshd` is not installed), and fuzz smoke runs of the IPC decoder and the terminal (`cargo +nightly fuzz run --target x86_64-unknown-linux-gnu term_feed`).

On Linux or macOS, `cargo run -p ferossh-term --example vttest -- 1 2 8` drives [vttest](https://invisible-island.net/vttest/) through the emulator and prints every screen as text.

## Licence

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in FeroSSH by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
