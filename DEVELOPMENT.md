# FeroSSH development history

This file records how FeroSSH was built: the roadmap milestones that led to 1.0, then the features, security work and releases that followed. It is a map of the project's history for contributors. For what a release changed from a user's point of view, read its notes in [docs/releases](docs/releases); for how to use a feature, read the [user guide](docs/guide/README.md).

> **Kept up to date with each release.** The version bump and release notes pull request for a new tag also adds that release to this file: a row in [Releases at a glance](#releases-at-a-glance), a section under [Releases](#releases) listing the pull requests it covers, and a line under the matching area in [After 1.0, by area](#after-10-by-area) for anything new. See [Updating this file](#updating-this-file).

Dates are the day a pull request was merged (Central European time). Pull request numbers link to GitHub. Last updated for **v1.4.2** (17 November 2026).

## Contents

- [Releases at a glance](#releases-at-a-glance)
- [Before the code](#before-the-code)
- [The road to 1.0: milestones 0 to 10](#the-road-to-10-milestones-0-to-10)
- [After 1.0, by area](#after-10-by-area)
- [Security reviews and hardening](#security-reviews-and-hardening)
- [Releases](#releases)
- [Updating this file](#updating-this-file)

## Releases at a glance

| Release | Date | Headline |
| --- | --- | --- |
| [v0.1.0](docs/releases/v0.1.0.md) | 2026-10-26 | First release: SSH in a sandboxed worker, tabs and splits, SFTP, tmux, forwarding, security keys, remote desktops |
| [v1.0.0-rc1](docs/releases/v1.0.0-rc1.md) | 2026-10-27 | Jump hosts, serial, Telnet, ZMODEM, auto-reconnect, snippets, command palette, hardened sandbox |
| [v1.0.0-rc2](docs/releases/v1.0.0-rc2.md) | 2026-10-30 | First code-signed exes; snippet macros, runs on many hosts, live Session manager |
| [v1.0.0](docs/releases/v1.0.0.md) | 2026-10-30 | FeroSSH 1.0, the first stable release |
| [v1.0.1](docs/releases/v1.0.1.md) | 2026-11-01 | Local PC section in file transfer, follow `cd` without OSC 7 |
| [v1.1.0](docs/releases/v1.1.0.md) | 2026-11-03 | tmux keybindings, same-directory splits, Tools menu, 30 more themes, session renames |
| [v1.2.0](docs/releases/v1.2.0.md) | 2026-11-07 | Virtual desktops on headless servers, standby hotkey window, snippets as files |
| [v1.3.0](docs/releases/v1.3.0.md) | 2026-11-09 | Remote apps as native windows (FeroStream), desktop clipboard, fixes from the 9 November security review |
| [v1.3.1](docs/releases/v1.3.1.md) | 2026-11-10 | Native tools pane and rail, two-list file transfer, Scripts with Expect-style logic, window colours |
| [v1.4.0](docs/releases/v1.4.0.md) | 2026-11-12 | Mosh, WinRM, SFTP-only, proxies, Kerberos, agent forwarding, workspaces, vault, recordings, ARM64 |
| [v1.4.1](docs/releases/v1.4.1.md) | 2026-11-15 | Built-in script library, Ctrl+E/Ctrl+F suggestion keys, standby focus fix |
| [v1.4.2](docs/releases/v1.4.2.md) | 2026-11-17 | Security release closing the findings of a full code base audit |

## Before the code

FeroSSH started on 24 October 2026 from a design spec: a small, security-focused, open-source SSH client for Windows, written in Rust, to replace PuTTY with tabs, splits, tmux integration and file transfer, shipped as one exe with nothing to install. The spec chose russh for SSH, the `windows` crate with Win32, DirectWrite and Direct2D for the UI, and a two-process model: a window that never parses network data and an SSH worker locked in an AppContainer.

Decisions taken before milestone 0:

- **Licence:** MIT or Apache-2.0, at your option.
- **Process split at milestone 3**, not milestone 9 as the spec first had it, so the sandboxed worker exists from the first real SSH session instead of being retrofitted.
- **Crypto backend:** russh with `ring` (a pure RustCrypto backend was not available); post-quantum `mlkem768x25519-sha256` first, SHA-1, CBC, DSA and `ssh-rsa` kept out of the preferred lists and checked by a test.
- **Roadmap reordered** to the maintainer's priorities: SSH (milestone 3), tabs (4), SFTP (5), tmux (6), forwarding and hardware keys (7).
- **UI:** minimal and native: one terminal pane by default, optional side and top panels, font, size and colour theme per tab and pane.

## The road to 1.0: milestones 0 to 10

Milestones 0 to 10 ran from 24 to 30 October 2026. Each one ended in a usable build.

### Milestone 0: scaffold (25 October)

- Cargo workspace, length-prefixed IPC framing between window and worker, the crypto policy and CI on Windows and Linux [#1](https://github.com/jkobierczynski/FeroSSH/pull/1). Fuzz smoke test on the GNU target [#2](https://github.com/jkobierczynski/FeroSSH/pull/2). The empty Win32 exe was 196 KB.

### Milestone 1: window and renderer (25 October)

- Win32 window with a Direct2D glyph-atlas renderer: one atlas per font, size and DPI, per-pane font and theme from the start [#3](https://github.com/jkobierczynski/FeroSSH/pull/3).

### Milestone 2: terminal (25 October)

- Terminal emulation on `alacritty_terminal`, scrollback with search, keyboard and mouse, a local ConPTY shell as the test host, and a vttest driver [#4](https://github.com/jkobierczynski/FeroSSH/pull/4).
- Ten colour themes (Nord by default) and per-host theme rules, including red Production and amber Staging themes [#5](https://github.com/jkobierczynski/FeroSSH/pull/5).

### Milestone 3: first SSH session (25 October)

- Real SSH in one tab from a sandboxed AppContainer worker: `known_hosts`, the Windows OpenSSH agent, Pageant, key files, keyboard-interactive and passwords, plus a log file per tab [#6](https://github.com/jkobierczynski/FeroSSH/pull/6). Quieter Pageant "early eof" note [#7](https://github.com/jkobierczynski/FeroSSH/pull/7).

### Milestone 4: tabs and splits (25 October)

- Tabs, split panes, the session manager on `ssh_config`, and per-host fonts and themes [#8](https://github.com/jkobierczynski/FeroSSH/pull/8). Merge a tab back into another as a pane [#9](https://github.com/jkobierczynski/FeroSSH/pull/9). Tabs in the Windows menu bar colours [#10](https://github.com/jkobierczynski/FeroSSH/pull/10).

### Milestone 5: file transfer (25 October)

- SFTP file transfer in a side pane with a transfer queue [#11](https://github.com/jkobierczynski/FeroSSH/pull/11).

### Milestone 6: tmux (25 October)

- tmux control mode with one tmux window per native pane, so each pane keeps its own font size; the layout and looks are stored in tmux and restored on reattach [#12](https://github.com/jkobierczynski/FeroSSH/pull/12). Starting tmux is opt-in.

### Milestone 7: forwarding and hardware keys (25 October)

- Port forwarding (`-L`, `-R`, `-D` with SOCKS 4, 4a and 5) and X11 forwarding [#13](https://github.com/jkobierczynski/FeroSSH/pull/13).
- FIDO2 security keys (`sk-` keys) through Windows WebAuthn; passkeys on a phone work through the Windows hybrid flow [#27](https://github.com/jkobierczynski/FeroSSH/pull/27).

The same day a hardening pass added a real-server interop harness (OpenSSH, Dropbear and several tmux versions in containers) [#15](https://github.com/jkobierczynski/FeroSSH/pull/15), AES-GCM fallback and opt-in legacy algorithms per host [#16](https://github.com/jkobierczynski/FeroSSH/pull/16), Connect to… in a new tab [#17](https://github.com/jkobierczynski/FeroSSH/pull/17), refitting the grid after a font change [#18](https://github.com/jkobierczynski/FeroSSH/pull/18), a scroll position mark [#19](https://github.com/jkobierczynski/FeroSSH/pull/19), `@cert-authority` host certificates [#20](https://github.com/jkobierczynski/FeroSSH/pull/20), keepalives and connect timeouts from `ssh_config` [#21](https://github.com/jkobierczynski/FeroSSH/pull/21), SFTP name collisions on Windows [#22](https://github.com/jkobierczynski/FeroSSH/pull/22), tab titles restored after full-screen apps [#23](https://github.com/jkobierczynski/FeroSSH/pull/23), agent certificates and ECDSA P-384/P-521 [#24](https://github.com/jkobierczynski/FeroSSH/pull/24), vttest, less and vim checks on real servers [#25](https://github.com/jkobierczynski/FeroSSH/pull/25) and tmux 2.6 to 3.2 coverage [#26](https://github.com/jkobierczynski/FeroSSH/pull/26). Remote Linux desktops followed: Open desktop forwards RDP through SSH and starts `mstsc` [#28](https://github.com/jkobierczynski/FeroSSH/pull/28), and a built-in VNC and RDP viewer tab is an optional Cargo feature in `ferossh-desktop.exe` [#29](https://github.com/jkobierczynski/FeroSSH/pull/29), with desktop tabs that follow their pane [#30](https://github.com/jkobierczynski/FeroSSH/pull/30).

The README got its first screenshot [#14](https://github.com/jkobierczynski/FeroSSH/pull/14). **v0.1.0** was tagged on 26 October.

### Saved sessions and imports (26 October)

- A form for making and editing saved sessions [#31](https://github.com/jkobierczynski/FeroSSH/pull/31), password sources (Windows Credential Manager or a password manager command) [#33](https://github.com/jkobierczynski/FeroSSH/pull/33), and imports from PuTTY, KiTTY and WinSCP [#36](https://github.com/jkobierczynski/FeroSSH/pull/36), then MobaXterm, mRemoteNG and SecureCRT [#37](https://github.com/jkobierczynski/FeroSSH/pull/37).

### Milestone 8: serial and network tools (26 October)

- Serial consoles (`COM3:115200`), broadcast input to several panes, and session logs sealed with a hash chain, with Session > Check a log for changes [#40](https://github.com/jkobierczynski/FeroSSH/pull/40).

### Milestone 9: privilege separation hardening (26 and 27 October)

- ECDSA P-256 keys with a 31-byte scalar [#39](https://github.com/jkobierczynski/FeroSSH/pull/39).
- The window checks what each worker sends: which side may send which message, login order, stream windows [#42](https://github.com/jkobierczynski/FeroSSH/pull/42).
- Process mitigations, job UI limits and a memory cap for the worker, and `ferossh.exe --check-sandbox` run in CI [#45](https://github.com/jkobierczynski/FeroSSH/pull/45).
- New fuzz targets (sign-request check, key files, host config), a weekly campaign and SECURITY.md [#46](https://github.com/jkobierczynski/FeroSSH/pull/46).
- One sandboxed worker per connection instead of one for all [#50](https://github.com/jkobierczynski/FeroSSH/pull/50).
- Win32k lockdown, Control Flow Guard and CET shadow stacks for the worker [#64](https://github.com/jkobierczynski/FeroSSH/pull/64).

### Milestone 10: release 1.0 (26 to 30 October)

- Reproducible release builds checked in CI [#38](https://github.com/jkobierczynski/FeroSSH/pull/38), a release workflow with notes in `docs/releases` [#32](https://github.com/jkobierczynski/FeroSSH/pull/32), a release manifest with build provenance attestations [#41](https://github.com/jkobierczynski/FeroSSH/pull/41), the user guide in `docs/guide` [#43](https://github.com/jkobierczynski/FeroSSH/pull/43), winget manifests [#44](https://github.com/jkobierczynski/FeroSSH/pull/44), a version resource and SignPath signing hooks [#48](https://github.com/jkobierczynski/FeroSSH/pull/48), and an opt-in, Ed25519-signed update check [#49](https://github.com/jkobierczynski/FeroSSH/pull/49), [#68](https://github.com/jkobierczynski/FeroSSH/pull/68), [#69](https://github.com/jkobierczynski/FeroSSH/pull/69), [#70](https://github.com/jkobierczynski/FeroSSH/pull/70).
- SignPath Foundation turned the project down, so README and notes said releases were unsigned [#54](https://github.com/jkobierczynski/FeroSSH/pull/54) until a Certum Open Source certificate arrived; releases are now signed on the maintainer's PC by `scripts/sign-release.ps1` [#82](https://github.com/jkobierczynski/FeroSSH/pull/82), [#85](https://github.com/jkobierczynski/FeroSSH/pull/85).
- The logo became the README header and the exe icon [#67](https://github.com/jkobierczynski/FeroSSH/pull/67); CodeQL skips `fuzz/` [#87](https://github.com/jkobierczynski/FeroSSH/pull/87).

### Also on the way to 1.0 (26 to 30 October)

- **Connections:** Telnet for old network gear with a red badge [#56](https://github.com/jkobierczynski/FeroSSH/pull/56), jump hosts with `ProxyJump` and `ProxyCommand`, one worker per hop [#57](https://github.com/jkobierczynski/FeroSSH/pull/57), live changes to port forwards [#60](https://github.com/jkobierczynski/FeroSSH/pull/60), automatic reconnect [#61](https://github.com/jkobierczynski/FeroSSH/pull/61), ZMODEM `rz`/`sz` in the terminal [#62](https://github.com/jkobierczynski/FeroSSH/pull/62).
- **Snippets and scripting:** login scripts and command snippets [#55](https://github.com/jkobierczynski/FeroSSH/pull/55), arguments and saving a selection [#72](https://github.com/jkobierczynski/FeroSSH/pull/72), macros with steps, variables, hotkeys, host scopes, auto-responses, a Production guard and recording [#74](https://github.com/jkobierczynski/FeroSSH/pull/74), running a snippet on many saved hosts [#75](https://github.com/jkobierczynski/FeroSSH/pull/75), [#76](https://github.com/jkobierczynski/FeroSSH/pull/76).
- **UI:** side pane readability [#52](https://github.com/jkobierczynski/FeroSSH/pull/52), one-click hide [#34](https://github.com/jkobierczynski/FeroSSH/pull/34), Close pane and Close tab in the Window menu [#51](https://github.com/jkobierczynski/FeroSSH/pull/51), the command palette (Ctrl+Shift+A) [#63](https://github.com/jkobierczynski/FeroSSH/pull/63), Open local shell in a new tab [#77](https://github.com/jkobierczynski/FeroSSH/pull/77), New tab moved to the Window menu [#79](https://github.com/jkobierczynski/FeroSSH/pull/79), eight fonts then eighteen [#59](https://github.com/jkobierczynski/FeroSSH/pull/59), [#83](https://github.com/jkobierczynski/FeroSSH/pull/83), download pages for missing fonts [#86](https://github.com/jkobierczynski/FeroSSH/pull/86), and a live Session manager next to the renamed Connection manager [#84](https://github.com/jkobierczynski/FeroSSH/pull/84).
- **Logs:** optional per-line timestamps [#58](https://github.com/jkobierczynski/FeroSSH/pull/58).
- **Fuzzing:** a terminal crash found by the fuzzer [#71](https://github.com/jkobierczynski/FeroSSH/pull/71), then a long campaign that fixed more crashes and a search freeze, added targets and moved to nightly fuzzing [#73](https://github.com/jkobierczynski/FeroSSH/pull/73), with two more rounds of fixes [#78](https://github.com/jkobierczynski/FeroSSH/pull/78), [#80](https://github.com/jkobierczynski/FeroSSH/pull/80).
- **CI:** the real-server interop job made fit to be a required check [#65](https://github.com/jkobierczynski/FeroSSH/pull/65).
- **Docs:** README brought up to date [#53](https://github.com/jkobierczynski/FeroSSH/pull/53), [#35](https://github.com/jkobierczynski/FeroSSH/pull/35), and a new one-liner leading with the AppContainer sandbox [#81](https://github.com/jkobierczynski/FeroSSH/pull/81).

**v1.0.0-rc1** (27 October) and **v1.0.0-rc2** (30 October, the first signed release) led to **FeroSSH 1.0** on 30 October [#66](https://github.com/jkobierczynski/FeroSSH/pull/66), [#88](https://github.com/jkobierczynski/FeroSSH/pull/88).

## After 1.0, by area

### Connections and protocols

- Saved hosts called connections, live ones sessions [#90](https://github.com/jkobierczynski/FeroSSH/pull/90). Imports: refuse bracketed hosts with one colon [#93](https://github.com/jkobierczynski/FeroSSH/pull/93); read an empty forward bind address as `*` [#117](https://github.com/jkobierczynski/FeroSSH/pull/117).
- Virtual desktops on headless servers with Xvfb and x11vnc or Xvnc [#113](https://github.com/jkobierczynski/FeroSSH/pull/113), desktop size per connection and live resizing [#114](https://github.com/jkobierczynski/FeroSSH/pull/114).
- russh 0.65.1 for two denial-of-service fixes [#122](https://github.com/jkobierczynski/FeroSSH/pull/122).
- Agent forwarding with a confirmation per signature, `ssh_config` `Match` and more keywords [#179](https://github.com/jkobierczynski/FeroSSH/pull/179).
- New SSH key and Copy public key to this server [#180](https://github.com/jkobierczynski/FeroSSH/pull/180).
- Kerberos (GSSAPI) login with the Windows logon's tickets [#181](https://github.com/jkobierczynski/FeroSSH/pull/181).
- Mosh connections [#182](https://github.com/jkobierczynski/FeroSSH/pull/182), with its own screen renderer and emulator [#183](https://github.com/jkobierczynski/FeroSSH/pull/183) and local echo over slow links [#190](https://github.com/jkobierczynski/FeroSSH/pull/190).
- SFTP-only connections [#184](https://github.com/jkobierczynski/FeroSSH/pull/184), SOCKS 5 and HTTP proxies [#186](https://github.com/jkobierczynski/FeroSSH/pull/186).
- WinRM sessions (PowerShell or cmd over WS-Management) [#185](https://github.com/jkobierczynski/FeroSSH/pull/185), with password sources and HTTPS [#188](https://github.com/jkobierczynski/FeroSSH/pull/188).
- Workspaces: save tabs and split panes and open them again [#196](https://github.com/jkobierczynski/FeroSSH/pull/196).
- Encrypted connection export and import, a master password vault, and portable mode [#195](https://github.com/jkobierczynski/FeroSSH/pull/195).

### Terminal and tmux

- tmux keybindings for tabs and panes [#101](https://github.com/jkobierczynski/FeroSSH/pull/101) with repeatable keys [#107](https://github.com/jkobierczynski/FeroSSH/pull/107); tmux "off" means no tmux, and a per-host start directory [#109](https://github.com/jkobierczynski/FeroSSH/pull/109), [#112](https://github.com/jkobierczynski/FeroSSH/pull/112).
- New tabs and panes start in the original pane's directory [#99](https://github.com/jkobierczynski/FeroSSH/pull/99), [#116](https://github.com/jkobierczynski/FeroSSH/pull/116).
- Rename live sessions [#103](https://github.com/jkobierczynski/FeroSSH/pull/103), [#106](https://github.com/jkobierczynski/FeroSSH/pull/106), [#177](https://github.com/jkobierczynski/FeroSSH/pull/177).
- Keyword highlighting and stop following at a match [#187](https://github.com/jkobierczynski/FeroSSH/pull/187).
- Server status bar for CPU, memory, disk and network [#191](https://github.com/jkobierczynski/FeroSSH/pull/191).
- Command suggestions from an encrypted per-host history [#192](https://github.com/jkobierczynski/FeroSSH/pull/192), accepted with Ctrl+E or Ctrl+F and configurable keys [#213](https://github.com/jkobierczynski/FeroSSH/pull/213).
- Offer to detach other tmux clients that keep a window small [#206](https://github.com/jkobierczynski/FeroSSH/pull/206).
- Hide a pane without closing it [#208](https://github.com/jkobierczynski/FeroSSH/pull/208).
- Full screen on F11 [#176](https://github.com/jkobierczynski/FeroSSH/pull/176).

### Windows, menus and looks

- Tools menu for the side panes and command palette [#104](https://github.com/jkobierczynski/FeroSSH/pull/104), one-click close of side panes [#108](https://github.com/jkobierczynski/FeroSSH/pull/108).
- 30 more colour themes in dark, light and classic menu columns [#105](https://github.com/jkobierczynski/FeroSSH/pull/105); a dark Theme submenu with dark menus [#193](https://github.com/jkobierczynski/FeroSSH/pull/193).
- Standby window: a global hotkey hides and shows any window [#115](https://github.com/jkobierczynski/FeroSSH/pull/115), and gives the keyboard back when hiding [#211](https://github.com/jkobierczynski/FeroSSH/pull/211).
- Native tools rail with the terminal sliding over the tools pane [#145](https://github.com/jkobierczynski/FeroSSH/pull/145), and native Connection manager [#146](https://github.com/jkobierczynski/FeroSSH/pull/146), Session manager [#147](https://github.com/jkobierczynski/FeroSSH/pull/147), connection form [#148](https://github.com/jkobierczynski/FeroSSH/pull/148), Port forwards [#159](https://github.com/jkobierczynski/FeroSSH/pull/159) and Scripts pages [#160](https://github.com/jkobierczynski/FeroSSH/pull/160), [#161](https://github.com/jkobierczynski/FeroSSH/pull/161), [#162](https://github.com/jkobierczynski/FeroSSH/pull/162), [#178](https://github.com/jkobierczynski/FeroSSH/pull/178).
- Window colours: dark menus, title bar and tools pane [#157](https://github.com/jkobierczynski/FeroSSH/pull/157), and neutral greys [#204](https://github.com/jkobierczynski/FeroSSH/pull/204).
- Native command palette and configurable key bindings [#199](https://github.com/jkobierczynski/FeroSSH/pull/199), [#201](https://github.com/jkobierczynski/FeroSSH/pull/201), native Import connections page [#197](https://github.com/jkobierczynski/FeroSSH/pull/197).
- Tidier menus: Session submenus, Recent connections, greyed items [#205](https://github.com/jkobierczynski/FeroSSH/pull/205).
- Native Windows ARM64 exes [#194](https://github.com/jkobierczynski/FeroSSH/pull/194).

### Scripts (formerly snippets)

- Snippets kept as files in a folder, with categories, imports, keystrokes and `@delay` [#118](https://github.com/jkobierczynski/FeroSSH/pull/118).
- Expect-style logic, and Snippets renamed Scripts [#155](https://github.com/jkobierczynski/FeroSSH/pull/155), [#156](https://github.com/jkobierczynski/FeroSSH/pull/156).
- Login script loops stopped through a secret step [#189](https://github.com/jkobierczynski/FeroSSH/pull/189).
- A built-in library of a few hundred ready scripts [#210](https://github.com/jkobierczynski/FeroSSH/pull/210).

### File transfer

- Local PC section [#96](https://github.com/jkobierczynski/FeroSSH/pull/96), following `cd` without OSC 7 [#94](https://github.com/jkobierczynski/FeroSSH/pull/94).
- Resizable sections and search by name [#110](https://github.com/jkobierczynski/FeroSSH/pull/110).
- Native two-list file transfer with arrow buttons and drag and drop [#150](https://github.com/jkobierczynski/FeroSSH/pull/150), bookmarks, Sync browsing and per-connection folders [#154](https://github.com/jkobierczynski/FeroSSH/pull/154), arrows kept centred [#164](https://github.com/jkobierczynski/FeroSSH/pull/164).
- Resume integrity check [#158](https://github.com/jkobierczynski/FeroSSH/pull/158), file names kept off the remote checksum command line [#165](https://github.com/jkobierczynski/FeroSSH/pull/165), [#166](https://github.com/jkobierczynski/FeroSSH/pull/166).
- Sort by name, size or modified; delete local files to the Recycle Bin [#202](https://github.com/jkobierczynski/FeroSSH/pull/202).

### Logs and recordings

- Save a pane's whole output, or every pane's, to log files [#100](https://github.com/jkobierczynski/FeroSSH/pull/100).
- Session recording in asciicast format with timed replay [#198](https://github.com/jkobierczynski/FeroSSH/pull/198).

### Remote apps and desktops (FeroStream)

FeroStream, a separate project in [jkobierczynski/ferostream](https://github.com/jkobierczynski/ferostream), shows Linux programs from a server as native Windows windows, through SSH, with a small Linux agent.

- Remote apps built into FeroSSH, run through a sandboxed worker [#120](https://github.com/jkobierczynski/FeroSSH/pull/120); smaller pictures with FeroStream protocol 6 [#121](https://github.com/jkobierczynski/FeroSSH/pull/121); clipboard, live changes and mouse selections [#123](https://github.com/jkobierczynski/FeroSSH/pull/123); FeroStream pinned to its main branch [#125](https://github.com/jkobierczynski/FeroSSH/pull/125).
- Clipboard for RDP, VNC and virtual desktops, and desktop and remote-app connections that open on their own [#124](https://github.com/jkobierczynski/FeroSSH/pull/124); the clipboard is off by default [#126](https://github.com/jkobierczynski/FeroSSH/pull/126).
- The Linux agents get build provenance attestations too [#142](https://github.com/jkobierczynski/FeroSSH/pull/142).

### Fuzzing and testing

- Fuzzing nightly pinned to a known-good toolchain [#95](https://github.com/jkobierczynski/FeroSSH/pull/95).
- New fuzz targets for agent forwarding, Mosh start-up, remote names and command history [#200](https://github.com/jkobierczynski/FeroSSH/pull/200), [#203](https://github.com/jkobierczynski/FeroSSH/pull/203).
- Viewer tests take turns on the shared Xvnc [#153](https://github.com/jkobierczynski/FeroSSH/pull/153).

### CI, builds and releases

- Each release submitted to winget automatically [#98](https://github.com/jkobierczynski/FeroSSH/pull/98).
- CI cancels superseded runs, caches dependencies and fuzzes in parallel [#151](https://github.com/jkobierczynski/FeroSSH/pull/151), [#152](https://github.com/jkobierczynski/FeroSSH/pull/152).
- Build provenance verified before local signing [#163](https://github.com/jkobierczynski/FeroSSH/pull/163).
- The MSVC toolset pinned [#212](https://github.com/jkobierczynski/FeroSSH/pull/212) and the linker's Rich header left out [#215](https://github.com/jkobierczynski/FeroSSH/pull/215), so reproducible builds survive runner image and linker updates.

### Documentation

- README and SECURITY.md for 1.0 [#89](https://github.com/jkobierczynski/FeroSSH/pull/89); a user guide that covers every feature, menu by menu [#102](https://github.com/jkobierczynski/FeroSSH/pull/102); README regrouped by feature with FeroStream agent build docs [#207](https://github.com/jkobierczynski/FeroSSH/pull/207); five new screenshots [#209](https://github.com/jkobierczynski/FeroSSH/pull/209).

## Security reviews and hardening

Security work runs through the whole history (see milestone 9 above). Three outside reviews led to dedicated rounds of fixes.

### 9 November 2026: AI-assisted review

An AI-generated review of FeroSSH was checked finding by finding against the code. The review and its triage are published in [docs/security](docs/security).

- Desktop and remote-app clipboard off by default [#126](https://github.com/jkobierczynski/FeroSSH/pull/126).
- RDP viewer, VNC and IronRDP limits, bounded desktop queues, and password and Pageant hygiene, merged as one stack [#130](https://github.com/jkobierczynski/FeroSSH/pull/130), [#131](https://github.com/jkobierczynski/FeroSSH/pull/131) (stacked pull requests #127 to #129 went in with them).
- A re-audit's findings, merged as one stack [#138](https://github.com/jkobierczynski/FeroSSH/pull/138) (with #135 to #137): split DRDYNVC headers, queue budgets by message, plain VNC approval keyed by connection, typed passwords in one buffer.
- Text-direction overrides dropped from VNC dialog names [#140](https://github.com/jkobierczynski/FeroSSH/pull/140).
- The review and its triage published [#133](https://github.com/jkobierczynski/FeroSSH/pull/133).

### 10 November 2026: CodeRabbit whole-code base audit

CodeRabbit, which reviews every pull request since 9 November, also audited the whole code base. The fixes:

- Remote-input work bounded [#172](https://github.com/jkobierczynski/FeroSSH/pull/172), local handling hardened [#173](https://github.com/jkobierczynski/FeroSSH/pull/173), release attestation and security docs tightened [#174](https://github.com/jkobierczynski/FeroSSH/pull/174).
- Release provenance checked before signing [#163](https://github.com/jkobierczynski/FeroSSH/pull/163) and SFTP checksum file names kept off the command line [#165](https://github.com/jkobierczynski/FeroSSH/pull/165), [#166](https://github.com/jkobierczynski/FeroSSH/pull/166).

### 17 November 2026: full code base audit (v1.4.2)

A fresh audit of the whole code base led to the v1.4.2 security release. Its [release notes](docs/releases/v1.4.2.md) describe each change.

- X11 setups checked in the window, with the real X cookie kept out of the worker [#216](https://github.com/jkobierczynski/FeroSSH/pull/216).
- A host key refused when `known_hosts` cannot be read [#217](https://github.com/jkobierczynski/FeroSSH/pull/217), and a key of another type blocked like a changed key [#219](https://github.com/jkobierczynski/FeroSSH/pull/219).
- WinRM over HTTP logs in with Kerberos only unless NTLM is allowed [#218](https://github.com/jkobierczynski/FeroSSH/pull/218).
- Key, certificate and `UserKnownHostsFile` paths on network shares never opened [#220](https://github.com/jkobierczynski/FeroSSH/pull/220), [#225](https://github.com/jkobierczynski/FeroSSH/pull/225).
- `ProxyCommand` and password commands started by their full path [#221](https://github.com/jkobierczynski/FeroSSH/pull/221).
- Downloads marked as from the Internet, and a ZMODEM receive asks first [#222](https://github.com/jkobierczynski/FeroSSH/pull/222).
- Smaller hardening: bidi in dialogs, a grid size cap, OSC 52 bursts, SFTP part files [#223](https://github.com/jkobierczynski/FeroSSH/pull/223).
- CI keeps the update signing key away from compilation, and actions are pinned to commit SHAs [#224](https://github.com/jkobierczynski/FeroSSH/pull/224).
- RDP certificates pinned on first use [#226](https://github.com/jkobierczynski/FeroSSH/pull/226), and forwards only serve the same Windows session [#227](https://github.com/jkobierczynski/FeroSSH/pull/227).

## Releases

Each release's notes are in [docs/releases](docs/releases). The pull requests each one covers:

| Release | Date | Pull requests |
| --- | --- | --- |
| [v0.1.0](docs/releases/v0.1.0.md) | 2026-10-26 | [#1](https://github.com/jkobierczynski/FeroSSH/pull/1) to [#31](https://github.com/jkobierczynski/FeroSSH/pull/31) |
| [v1.0.0-rc1](docs/releases/v1.0.0-rc1.md) | 2026-10-27 | [#32](https://github.com/jkobierczynski/FeroSSH/pull/32) to [#73](https://github.com/jkobierczynski/FeroSSH/pull/73) |
| [v1.0.0-rc2](docs/releases/v1.0.0-rc2.md) | 2026-10-30 | [#74](https://github.com/jkobierczynski/FeroSSH/pull/74) to [#84](https://github.com/jkobierczynski/FeroSSH/pull/84) |
| [v1.0.0](docs/releases/v1.0.0.md) | 2026-10-30 | [#85](https://github.com/jkobierczynski/FeroSSH/pull/85) to [#88](https://github.com/jkobierczynski/FeroSSH/pull/88) |
| [v1.0.1](docs/releases/v1.0.1.md) | 2026-11-01 | [#89](https://github.com/jkobierczynski/FeroSSH/pull/89) to [#97](https://github.com/jkobierczynski/FeroSSH/pull/97) (notes [#97](https://github.com/jkobierczynski/FeroSSH/pull/97)) |
| [v1.1.0](docs/releases/v1.1.0.md) | 2026-11-03 | [#98](https://github.com/jkobierczynski/FeroSSH/pull/98) to [#112](https://github.com/jkobierczynski/FeroSSH/pull/112) (notes [#111](https://github.com/jkobierczynski/FeroSSH/pull/111)) |
| [v1.2.0](docs/releases/v1.2.0.md) | 2026-11-07 | [#113](https://github.com/jkobierczynski/FeroSSH/pull/113) to [#119](https://github.com/jkobierczynski/FeroSSH/pull/119) (notes [#119](https://github.com/jkobierczynski/FeroSSH/pull/119)) |
| [v1.3.0](docs/releases/v1.3.0.md) | 2026-11-09 | [#120](https://github.com/jkobierczynski/FeroSSH/pull/120) to [#141](https://github.com/jkobierczynski/FeroSSH/pull/141) (notes [#141](https://github.com/jkobierczynski/FeroSSH/pull/141)) |
| [v1.3.1](docs/releases/v1.3.1.md) | 2026-11-10 | [#142](https://github.com/jkobierczynski/FeroSSH/pull/142) to [#175](https://github.com/jkobierczynski/FeroSSH/pull/175) (notes [#175](https://github.com/jkobierczynski/FeroSSH/pull/175)) |
| [v1.4.0](docs/releases/v1.4.0.md) | 2026-11-12 | [#176](https://github.com/jkobierczynski/FeroSSH/pull/176) to [#209](https://github.com/jkobierczynski/FeroSSH/pull/209) (notes [#207](https://github.com/jkobierczynski/FeroSSH/pull/207)) |
| [v1.4.1](docs/releases/v1.4.1.md) | 2026-11-15 | [#210](https://github.com/jkobierczynski/FeroSSH/pull/210) to [#214](https://github.com/jkobierczynski/FeroSSH/pull/214) (notes [#214](https://github.com/jkobierczynski/FeroSSH/pull/214)) |
| [v1.4.2](docs/releases/v1.4.2.md) | 2026-11-17 | [#215](https://github.com/jkobierczynski/FeroSSH/pull/215) to [#228](https://github.com/jkobierczynski/FeroSSH/pull/228) (notes [#228](https://github.com/jkobierczynski/FeroSSH/pull/228)) |

How a release is made: a pull request bumps the version in `Cargo.toml` and adds `docs/releases/<tag>.md`; after it merges, the maintainer tags the merge commit, [`release.yml`](.github/workflows/release.yml) builds and attests the exes, and `scripts/sign-release.ps1 -Tag <tag>` signs and publishes them. See [Checks and releases](README.md#checks-and-releases) in the README.

## Updating this file

Update this file in the same pull request that bumps the version and adds the release notes, so the history is always as current as the latest release:

1. Add a row to [Releases at a glance](#releases-at-a-glance) and to the table under [Releases](#releases), with the range of pull requests merged since the previous tag (`git log --first-parent --oneline <previous tag>..HEAD`, leaving out direct commits that aren't pull requests). The version bump pull request isn't merged yet when you write this, so add its own number by hand, and check the range again before tagging.
2. Add each new feature, fix or security change as a line under its area in [After 1.0, by area](#after-10-by-area) or [Security reviews and hardening](#security-reviews-and-hardening), with its pull request number. Start a new area when nothing fits.
3. Change "Last updated for" at the top to the new version and date.

Keep it to public information: what shipped and why, not unpublished findings from private reviews.
