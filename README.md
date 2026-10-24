# FeroSSH

*Secure connections. Forged in Rust.*

FeroSSH aims to be the smallest, most secure open-source SSH client for Windows: a native PuTTY replacement with tabs, splits, tmux integration and file transfer, shipped as one signed exe with nothing to install.

> **Status:** milestone 2 (terminal emulation). The exe opens one terminal pane on a local shell through ConPTY, so the emulator, keyboard and mouse handling can be used for real; the first SSH session arrives in milestone 3.

## Trying the milestone 2 build

Download `ferossh-exe` from the latest CI run and start `ferossh.exe`. It runs `cmd.exe`; give it a command to run something else, for example `ferossh.exe wsl` or `ferossh.exe ssh myhost` (Windows OpenSSH), which is the easiest way to try vim, htop, tmux or vttest before milestone 3.

| Shortcut | Action |
| --- | --- |
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

Checks run in CI: `cargo fmt`, `cargo clippy`, `cargo test`, `cargo deny`, `cargo audit`, and fuzz smoke runs of the IPC decoder and the terminal (`cargo +nightly fuzz run --target x86_64-unknown-linux-gnu term_feed`).

On Linux or macOS, `cargo run -p ferossh-term --example vttest -- 1 2 8` drives [vttest](https://invisible-island.net/vttest/) through the emulator and prints every screen as text.

## Licence

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in FeroSSH by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
