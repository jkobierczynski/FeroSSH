# FeroSSH development history

This file records how FeroSSH was built. It covers the roadmap milestones that led to 1.0, then the features, security work and releases that followed. Along the way it notes why each decision was made, which options were turned down, and what testing on a real Windows PC found. It is a map of the project's history for contributors. For what a release changed from a user's point of view, read its notes in [docs/releases](docs/releases). For how to use a feature, read the [user guide](docs/guide/README.md).

> **Kept up to date with each release.** The pull request that bumps the version and adds a new tag's release notes also adds that release here: a row in [Releases at a glance](#releases-at-a-glance), a row under [Releases](#releases) listing the pull requests it covers, and a line under the matching area in [After 1.0, by area](#after-10-by-area) for anything new. See [Updating this file](#updating-this-file).

Dates are the day a pull request was merged, in Central European time. Pull request numbers link to GitHub. Last updated for **v1.4.2** (17 November 2026).

## Contents

- [Releases at a glance](#releases-at-a-glance)
- [How FeroSSH is built](#how-ferossh-is-built)
- [Before the code](#before-the-code)
- [The road to 1.0: milestones 0 to 10](#the-road-to-10-milestones-0-to-10)
- [After 1.0, by area](#after-10-by-area)
- [Security reviews and hardening](#security-reviews-and-hardening)
- [What testing on Windows found](#what-testing-on-windows-found)
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

## How FeroSSH is built

FeroSSH has one maintainer, who sets the direction, tests every build on a real Windows PC and merges. Much of the code is written with an AI coding assistant (Claude Code), which works on one feature per branch and opens a pull request for each change. Nothing reaches `main` without the required checks, and those checks run against real servers:

- **Required checks on `main`** (a branch ruleset since milestone 0): Build and test (Windows), `cargo deny`, `cargo audit` and a fuzz smoke run of every fuzz target, later joined by the real-server interop job [#65](https://github.com/jkobierczynski/FeroSSH/pull/65).
- **Real servers in CI:** OpenSSH, Dropbear and several tmux versions in containers [#15](https://github.com/jkobierczynski/FeroSSH/pull/15), xrdp and Xvnc for the desktop viewers, and `--check-sandbox` on the release exe.
- **Fuzzing:** a smoke run on every pull request, a nightly campaign, and long local campaigns run by the maintainer in WSL (see [Fuzzing and testing](#fuzzing-and-testing)).
- **Review:** CodeRabbit reviews every pull request since 9 November 2026, and its findings are checked against the code before anything is changed.
- **Windows:** the Win32 parts can be type-checked from Linux, but they only run on Windows. Many bugs were found only when the maintainer tried a build there (see [What testing on Windows found](#what-testing-on-windows-found)).

Parallel pull requests that touch the same files conflict. The rule is to merge them one at a time and to merge `main` into the later ones.

## Before the code

FeroSSH started on 24 October 2026 with a brief and a design spec. The brief put security first and set the order of work: core SSH sessions, then tabs, file transfer, tmux and screen integration, then port forwarding. The interface was to be elegant and minimal, with optional draggable icon bars, optional side panels for settings or file transfer, and optional top panels, so that nothing gets in the user's way. The spec chose Rust, russh for SSH, the `windows` crate with Win32, DirectWrite and Direct2D for the UI, and a two-process model: a window that never parses network data and an SSH worker locked in an AppContainer. It ended in a ten-milestone roadmap.

Decisions taken before milestone 0:

- **Licence: MIT or Apache-2.0, at your option.** This is the Rust ecosystem's standard, so code can move between FeroSSH and other crates freely. Apache-2.0 adds a patent grant, and MIT stays compatible with GPLv2 projects. Apache-2.0 alone, as russh uses, was the alternative.
- **The worker split moved to milestone 3.** The spec only split the app into a window and a sandboxed worker at milestone 9. Doing it at milestone 3 meant the security design was there from the first real SSH connection and nothing had to be reworked later, at the cost of some speed early on.
- **Crypto backend:** russh with `ring`, since a pure RustCrypto backend was not available. Post-quantum `mlkem768x25519-sha256` comes first. SHA-1, CBC, DSA and `ssh-rsa` stay out of the preferred lists, and a test checks this.
- **Roadmap reordered** to the maintainer's priorities: SSH (milestone 3), tabs (4), SFTP (5), tmux (6), then forwarding and hardware keys (7).
- **UI:** one terminal pane by default, since a split pane that is always visible was unwanted. Every tab and pane gets its own font, size and colour theme. Scrollback is configurable (10,000 lines by default), and every tab has its own log file. Per-tab logs were pulled forward from the network tools milestone to milestone 3.

## The road to 1.0: milestones 0 to 10

Milestones 0 to 10 ran from 24 to 30 October 2026. Each one ended in a build the maintainer downloaded from the pull request's CI run and tried on Windows before merging.

### Milestone 0: scaffold (25 October)

- Cargo workspace, length-prefixed IPC framing between window and worker, the crypto policy, and CI on Windows and Linux [#1](https://github.com/jkobierczynski/FeroSSH/pull/1). Fuzz smoke test on the GNU target [#2](https://github.com/jkobierczynski/FeroSSH/pull/2). The empty Win32 exe was 196 KB.

### Milestone 1: window and renderer (25 October)

- Win32 window with a Direct2D glyph-atlas renderer, with one atlas per font, size and DPI [#3](https://github.com/jkobierczynski/FeroSSH/pull/3). Per-pane fonts and themes were designed in from the start, because the maintainer asked for them while this milestone was being built. The exe was 246 KB.

### Milestone 2: terminal (25 October)

- Terminal emulation on `alacritty_terminal`, scrollback with search, keyboard and mouse, a local ConPTY shell as the test host, and a vttest driver [#4](https://github.com/jkobierczynski/FeroSSH/pull/4).
- Ten colour themes, Nord by default [#5](https://github.com/jkobierczynski/FeroSSH/pull/5). The maintainer picked the list (Nord, Dracula, Catppuccin Mocha, Gruvbox, Solarized Dark, Tokyo Night, One Dark, Monokai, Rosé Pine, Everforest). The maintainer also asked for a theme per host group: a red-tinted Production theme and an amber Staging theme give a constant warning about where you are typing.

### Milestone 3: first SSH session (25 October)

- Real SSH in one tab from a sandboxed AppContainer worker, with `known_hosts`, the Windows OpenSSH agent, Pageant, key files, keyboard-interactive and passwords, plus a log file per tab [#6](https://github.com/jkobierczynski/FeroSSH/pull/6).
- The first login on Windows reached the server through the sandbox, but sign-in failed. Pageant wasn't running and no key was offered, so the messages for that case were made clearer [#7](https://github.com/jkobierczynski/FeroSSH/pull/7).
- russh, `ring` and tokio made this the largest jump in exe size, to about 3.1 MB (a zip of about 1.4 MB). Since then the exe has grown only a few KB per feature. The release profile already optimises for size, and fonts are deliberately not bundled, to keep it small.

### Milestone 4: tabs and splits (25 October)

- Tabs, split panes, the session manager on `ssh_config`, and per-host fonts and themes [#8](https://github.com/jkobierczynski/FeroSSH/pull/8).
- Merging a tab back into another tab as a pane, which the maintainer asked for after trying the build [#9](https://github.com/jkobierczynski/FeroSSH/pull/9). Larger tabs in the Windows menu bar colour [#10](https://github.com/jkobierczynski/FeroSSH/pull/10).

### Milestone 5: file transfer (25 October)

- SFTP file transfer in a side pane with a transfer queue, reusing the terminal's SSH connection so there is no second login [#11](https://github.com/jkobierczynski/FeroSSH/pull/11).

### Milestone 6: tmux (25 October)

- tmux control mode with one tmux window per native pane [#12](https://github.com/jkobierczynski/FeroSSH/pull/12). The idea came from the maintainer during milestone 4. tmux sizes every pane in a window on one shared grid, so panes with different font sizes can't line up. Giving each FeroSSH pane its own tmux window lets each pane keep its own font size. The layout and each pane's looks are stored in the tmux session, so reattaching from any PC restores them.
- A survey of other clients found none that offers per-pane font sizes with tmux restore. iTerm2 on macOS comes closest, and no client integrates screen, which has no control mode.
- Starting tmux is opt-in and off by default, at the maintainer's request.

### Milestone 7: forwarding and hardware keys (25 October)

- Port forwarding (`-L`, `-R`, `-D` with SOCKS 4, 4a and 5) and X11 forwarding [#13](https://github.com/jkobierczynski/FeroSSH/pull/13).
- FIDO2 security keys (`sk-` keys) through Windows WebAuthn [#27](https://github.com/jkobierczynski/FeroSSH/pull/27). The maintainer owns no hardware keys, so this was built and tested with a software authenticator and OpenSSH's `sk-dummy`. A real login then worked on Windows with the maintainer's phone as a passkey, through the Windows hybrid (QR code) flow.

#### Hardening the core (25 October)

Before any more features, the maintainer asked to finish and harden the core SSH experience. That covered interoperability, terminal emulation, host-key verification, reconnection, agent and Pageant compatibility, tmux robustness and SFTP edge cases, each with real-world integration tests:

- A real-server interop harness with OpenSSH, Dropbear and several tmux versions in containers [#15](https://github.com/jkobierczynski/FeroSSH/pull/15). It caught two bugs at once: OpenSSH 7.6 servers were refused, and tmux 2.6 showed a blank screen.
- AES-GCM fallback for older OpenSSH, and opt-in older algorithms per host [#16](https://github.com/jkobierczynski/FeroSSH/pull/16). The strict crypto policy locked out older Dropbear servers and most switches and routers. The decision was a per-host compatibility setting, off by default, that still never allows SHA-1, CBC or DSA.
- `@cert-authority` host certificates [#20](https://github.com/jkobierczynski/FeroSSH/pull/20), keepalives and connect timeouts from `ssh_config` with a reason when a connection dies [#21](https://github.com/jkobierczynski/FeroSSH/pull/21), SFTP names that collide on Windows [#22](https://github.com/jkobierczynski/FeroSSH/pull/22), agent certificates and ECDSA P-384/P-521 [#24](https://github.com/jkobierczynski/FeroSSH/pull/24), vttest, less and vim on real servers [#25](https://github.com/jkobierczynski/FeroSSH/pull/25), and tmux 2.6 to 3.2 with arrow keys after reattach [#26](https://github.com/jkobierczynski/FeroSSH/pull/26).
- From trying builds: Connect to… opens a new tab instead of replacing the current shell [#17](https://github.com/jkobierczynski/FeroSSH/pull/17), the grid refits after a font change, which had hidden the bottom rows [#18](https://github.com/jkobierczynski/FeroSSH/pull/18), a scroll position mark on each pane, shown only while scrolled back [#19](https://github.com/jkobierczynski/FeroSSH/pull/19), and the tab title comes back when a full-screen app such as Midnight Commander quits [#23](https://github.com/jkobierczynski/FeroSSH/pull/23).

#### Remote Linux desktops (25 and 26 October)

The question was whether FeroSSH could show a Linux desktop (X11 or Wayland). Three ways were weighed:

1. Tunnel a remote desktop over SSH and open Windows' own Remote Desktop client. It adds almost nothing to the exe and works with X11 and Wayland.
2. A built-in VNC or RDP viewer in a tab. It adds 0.5 to 2 MB and a new protocol parser.
3. A built-in X server or Wayland compositor, like MobaXterm. This was **turned down**: it would add tens of MB, a large attack surface and a lot of upkeep, which clashes with "smallest, most secure".

Both 1 and 2 were built. Open desktop forwards RDP through SSH and starts `mstsc` [#28](https://github.com/jkobierczynski/FeroSSH/pull/28). The viewer is a Cargo feature that is off by default, so its code is not in the standard `ferossh.exe` at all. It ships as a separate `ferossh-desktop.exe`, where it runs in its own sandboxed process, only through the SSH tunnel, and is fuzzed [#29](https://github.com/jkobierczynski/FeroSSH/pull/29). Desktop tabs then learned to follow their pane's size, move into any split and keep the connection [#30](https://github.com/jkobierczynski/FeroSSH/pull/30). Both viewers were confirmed on Windows against xrdp and VNC.

The README got its first screenshot [#14](https://github.com/jkobierczynski/FeroSSH/pull/14). **v0.1.0** was tagged on 26 October, with a release workflow built for it [#32](https://github.com/jkobierczynski/FeroSSH/pull/32).

### Saved sessions and imports (26 October)

- The maintainer found the session manager hard to use without remembering every setting. A form now makes and edits saved sessions [#31](https://github.com/jkobierczynski/FeroSSH/pull/31). It brought password sources (Windows Credential Manager, or a command for 1Password, Bitwarden or KeePassXC) [#33](https://github.com/jkobierczynski/FeroSSH/pull/33), and imports from PuTTY, KiTTY and WinSCP [#36](https://github.com/jkobierczynski/FeroSSH/pull/36), then MobaXterm, mRemoteNG and SecureCRT [#37](https://github.com/jkobierczynski/FeroSSH/pull/37).

### Milestone 8: serial and network tools (26 October)

- Serial consoles (`COM3:115200`), broadcast input to several panes, and session logs sealed with a hash chain, with Session > Check a log for changes [#40](https://github.com/jkobierczynski/FeroSSH/pull/40). The `.chain` file is a checksum, not a signature: it catches accidental damage or a casual edit, not someone who rewrites both files.

### Milestone 9: privilege separation hardening (26 and 27 October)

The worker split had already landed in milestone 3, so this milestone hardened the link between the processes and fuzzed more.

- ECDSA P-256 keys with a 31-byte scalar [#39](https://github.com/jkobierczynski/FeroSSH/pull/39).
- The window checks what each worker sends: which side may send which message, the login order, and stream windows [#42](https://github.com/jkobierczynski/FeroSSH/pull/42).
- Process mitigations, job UI limits and a memory cap for the worker, and `ferossh.exe --check-sandbox` run in CI [#45](https://github.com/jkobierczynski/FeroSSH/pull/45). It passed on the maintainer's PC.
- New fuzz targets (sign-request check, key files, host config), a weekly campaign, and SECURITY.md with GitHub's private vulnerability reporting [#46](https://github.com/jkobierczynski/FeroSSH/pull/46). The host config target found an exponential blow-up in `ssh_config` `Include`.
- **One sandboxed worker per connection** instead of one for all [#50](https://github.com/jkobierczynski/FeroSSH/pull/50). A discussion about the threat model showed that a hijacked shared worker could have seen the user's other sessions.
- Win32k lockdown, Control Flow Guard and CET shadow stacks for the worker [#64](https://github.com/jkobierczynski/FeroSSH/pull/64).

### Milestone 10: release 1.0 (26 to 30 October)

- Reproducible release builds checked in CI [#38](https://github.com/jkobierczynski/FeroSSH/pull/38), a release manifest with build provenance attestations [#41](https://github.com/jkobierczynski/FeroSSH/pull/41), the user guide in `docs/guide` [#43](https://github.com/jkobierczynski/FeroSSH/pull/43), winget manifests [#44](https://github.com/jkobierczynski/FeroSSH/pull/44), a version resource and SignPath signing hooks [#48](https://github.com/jkobierczynski/FeroSSH/pull/48), and an opt-in, Ed25519-signed update check [#49](https://github.com/jkobierczynski/FeroSSH/pull/49). The update signing key's public half is committed [#68](https://github.com/jkobierczynski/FeroSSH/pull/68), and the release refuses to build when the secret and the public key don't match [#69](https://github.com/jkobierczynski/FeroSSH/pull/69).
- **Code signing:** the SignPath Foundation turned FeroSSH down on 26 October because the project wasn't yet publicly visible enough. Azure Artifact Signing is open only to individuals in the USA and Canada, and SignPath's paid plan only issues certificates to organisations. The maintainer chose to ship 1.0 unsigned if needed, with checksums and attestations, and to buy a Certum Open Source certificate (about €49 a year), so the README and notes stopped naming SignPath [#54](https://github.com/jkobierczynski/FeroSSH/pull/54). The Certum certificate was issued on 30 October. Releases are signed on the maintainer's PC by `scripts/sign-release.ps1` [#82](https://github.com/jkobierczynski/FeroSSH/pull/82), [#85](https://github.com/jkobierczynski/FeroSSH/pull/85), and the first signed release was rc2. SmartScreen still warns about a brand-new certificate until it has built up a reputation.
- **Tagging rc1** took several attempts, and each one led to a fix. The first tag predated the version bump [#66](https://github.com/jkobierczynski/FeroSSH/pull/66). The second predated the update key. The third was caught by the new key check because the secret had been pasted wrongly. The fourth failed at the very last step because `gh` received `--prerelease` one character at a time [#70](https://github.com/jkobierczynski/FeroSSH/pull/70). A fuzzer crash was then fixed and rc1 re-tagged with it [#71](https://github.com/jkobierczynski/FeroSSH/pull/71).
- The logo became the README header and the exe icon [#67](https://github.com/jkobierczynski/FeroSSH/pull/67). CodeQL skips `fuzz/` [#87](https://github.com/jkobierczynski/FeroSSH/pull/87).

### Also on the way to 1.0 (26 to 30 October)

After milestone 7, the maintainer asked which features network engineers and pentesters want most in an SSH client. From that list the maintainer picked jump hosts, login macros with snippets, and Telnet, then reconnect, log timestamps, live forwards and ZMODEM, then the command palette, the stronger sandbox and interop as a required check.

- **Connections:** Telnet for old network gear, with a red badge [#56](https://github.com/jkobierczynski/FeroSSH/pull/56). Jump hosts with `ProxyJump` and `ProxyCommand`, with one sandboxed worker per hop [#57](https://github.com/jkobierczynski/FeroSSH/pull/57). Live changes to port forwards [#60](https://github.com/jkobierczynski/FeroSSH/pull/60). Automatic reconnect, which worked when tried on Windows [#61](https://github.com/jkobierczynski/FeroSSH/pull/61). ZMODEM `rz`/`sz` in the terminal [#62](https://github.com/jkobierczynski/FeroSSH/pull/62).
- **Snippets and scripting:** login scripts and command snippets [#55](https://github.com/jkobierczynski/FeroSSH/pull/55), arguments such as `$1` and `${1:Gi0/1}` and saving a selection as a snippet [#72](https://github.com/jkobierczynski/FeroSSH/pull/72). Macros with steps, variables, hotkeys, host scopes, auto-responses, a Production guard and recording [#74](https://github.com/jkobierczynski/FeroSSH/pull/74). Running a snippet on many saved hosts with a live report [#75](https://github.com/jkobierczynski/FeroSSH/pull/75). Its design was agreed first: four hosts at a time, open connections reused, host keys never accepted during a run, and a typed password never saved. The first try on Windows logged in but never typed the command, which was fixed [#76](https://github.com/jkobierczynski/FeroSSH/pull/76).
- **UI:** side pane readability [#52](https://github.com/jkobierczynski/FeroSSH/pull/52), hiding the side pane in one click [#34](https://github.com/jkobierczynski/FeroSSH/pull/34), Close pane and Close tab together at the bottom of the Window menu so nothing closes by accident [#51](https://github.com/jkobierczynski/FeroSSH/pull/51), the command palette (Ctrl+Shift+A) [#63](https://github.com/jkobierczynski/FeroSSH/pull/63), Open local shell in a new tab, since it used to end the pane's SSH and tmux session [#77](https://github.com/jkobierczynski/FeroSSH/pull/77), and New tab moved to the Window menu [#79](https://github.com/jkobierczynski/FeroSSH/pull/79). Eight fonts, then eighteen [#59](https://github.com/jkobierczynski/FeroSSH/pull/59), [#83](https://github.com/jkobierczynski/FeroSSH/pull/83), with download pages for those not installed [#86](https://github.com/jkobierczynski/FeroSSH/pull/86). A live Session manager next to the renamed Connection manager, and View renamed to Settings [#84](https://github.com/jkobierczynski/FeroSSH/pull/84).
- **Logs:** optional timestamps on each line, for change records and pentest reports [#58](https://github.com/jkobierczynski/FeroSSH/pull/58).
- **CI:** the real-server interop job made fit to be a required check [#65](https://github.com/jkobierczynski/FeroSSH/pull/65).
- **Docs:** the README brought up to date [#53](https://github.com/jkobierczynski/FeroSSH/pull/53), [#35](https://github.com/jkobierczynski/FeroSSH/pull/35), and a new one-liner leading with the AppContainer sandbox [#81](https://github.com/jkobierczynski/FeroSSH/pull/81). A claim to be "the very first" AppContainer SSH client was dropped: Token2Shell/MD ran in an AppContainer in 2016. What FeroSSH can claim is a separate network-only AppContainer for each connection, with the keys kept in the window.

**v1.0.0-rc1** came out on 27 October. Most of what had landed that day had run only in CI, so a release candidate came before 1.0. **v1.0.0-rc2** followed on 30 October as the first signed release, and **FeroSSH 1.0** came out the same evening [#88](https://github.com/jkobierczynski/FeroSSH/pull/88). The go decision followed a short test on Windows: a normal login, a login through a jump host, and automatic reconnect.

## After 1.0, by area

### Connections and protocols

- Saved hosts are called connections, and live ones sessions [#90](https://github.com/jkobierczynski/FeroSSH/pull/90). Import fixes for a bracketed host with one colon [#93](https://github.com/jkobierczynski/FeroSSH/pull/93) and an empty forward bind address read as `*` [#117](https://github.com/jkobierczynski/FeroSSH/pull/117), both found by the nightly fuzzer.
- **Virtual desktops** on headless servers with Xvfb and x11vnc, or Xvnc [#113](https://github.com/jkobierczynski/FeroSSH/pull/113), plus desktop size per connection and live resizing [#114](https://github.com/jkobierczynski/FeroSSH/pull/114). These run on the server and open in the existing VNC viewer, so they fit the decision against a built-in X server.
- russh 0.65.1 for two denial-of-service fixes (unbounded `EXT_INFO` allocations and a zero maximum packet size) [#122](https://github.com/jkobierczynski/FeroSSH/pull/122).
- Agent forwarding with a confirmation for each signature, safer than OpenSSH's default, plus `ssh_config` `Match` and more keywords [#179](https://github.com/jkobierczynski/FeroSSH/pull/179). These came from a list of SSH features FeroSSH was missing compared with other clients.
- New SSH key and Copy public key to this server [#180](https://github.com/jkobierczynski/FeroSSH/pull/180), confirmed working on Windows.
- Kerberos (GSSAPI) login with the Windows logon's tickets [#181](https://github.com/jkobierczynski/FeroSSH/pull/181).
- **Mosh** connections [#182](https://github.com/jkobierczynski/FeroSSH/pull/182), about 115 KB of exe. The first version's screen code had been translated closely from mosh's GPL-3 source, which doesn't fit FeroSSH's licence. It was found before any release and rewritten from scratch [#183](https://github.com/jkobierczynski/FeroSSH/pull/183). Local echo for slow links followed [#190](https://github.com/jkobierczynski/FeroSSH/pull/190).
- SFTP-only connections for accounts without a shell [#184](https://github.com/jkobierczynski/FeroSSH/pull/184), and SOCKS 5 and HTTP proxies for the connection itself, about 10 to 25 KB [#186](https://github.com/jkobierczynski/FeroSSH/pull/186).
- **WinRM** sessions (PowerShell or cmd over WS-Management, in line mode, about 100 to 150 KB) [#185](https://github.com/jkobierczynski/FeroSSH/pull/185), with password sources and HTTPS [#188](https://github.com/jkobierczynski/FeroSSH/pull/188). Full PowerShell Remoting was left out as two to three times larger.
- Workspaces: save tabs and split panes and open them again [#196](https://github.com/jkobierczynski/FeroSSH/pull/196). A workspace is a small layout file that points to saved connections.
- Encrypted connection export and import, a master password vault, and portable mode [#195](https://github.com/jkobierczynski/FeroSSH/pull/195). These were the two most requested features missing compared with Termius, Tabby, Royal TS, SecureCRT and MobaXterm. Sync goes through a file the user keeps in OneDrive or git, with no FeroSSH cloud. Portable mode keeps everything in a `FeroSSH-data` folder next to the exe, for example on a USB stick.

### Terminal and tmux

- tmux keybindings for tabs and panes [#101](https://github.com/jkobierczynski/FeroSSH/pull/101). On Windows, resize and scroll keys registered only once, so they now repeat without the prefix, as tmux does [#107](https://github.com/jkobierczynski/FeroSSH/pull/107). Typing "off" in the tmux field started a session named "off", which was found before the v1.1.0 tag and fixed together with a per-host start directory [#109](https://github.com/jkobierczynski/FeroSSH/pull/109). The field became an on/off switch [#112](https://github.com/jkobierczynski/FeroSSH/pull/112).
- New tabs and panes start in the original pane's directory [#99](https://github.com/jkobierczynski/FeroSSH/pull/99), [#116](https://github.com/jkobierczynski/FeroSSH/pull/116).
- Rename live sessions [#103](https://github.com/jkobierczynski/FeroSSH/pull/103), [#106](https://github.com/jkobierczynski/FeroSSH/pull/106). Renaming stopped after one character in the native Session manager until [#177](https://github.com/jkobierczynski/FeroSSH/pull/177).
- Keyword highlighting, and stopping the output from following at a regex match [#187](https://github.com/jkobierczynski/FeroSSH/pull/187).
- Server status bar for CPU, memory, disk and network [#191](https://github.com/jkobierczynski/FeroSSH/pull/191).
- Command suggestions from an encrypted per-host history [#192](https://github.com/jkobierczynski/FeroSSH/pull/192), accepted with Ctrl+E or Ctrl+F as in fish and zsh, with configurable keys [#213](https://github.com/jkobierczynski/FeroSSH/pull/213).
- An empty band at the bottom of the terminal came from an old tmux client left behind by a reconnect, which kept the window small. FeroSSH now offers to detach other clients [#206](https://github.com/jkobierczynski/FeroSSH/pull/206).
- Hide a pane without closing it, so it stays listed in the Session manager [#208](https://github.com/jkobierczynski/FeroSSH/pull/208).
- Full screen on F11 [#176](https://github.com/jkobierczynski/FeroSSH/pull/176).

### Windows, menus and looks

- A Tools menu for the side panes and command palette [#104](https://github.com/jkobierczynski/FeroSSH/pull/104), and side panes that close on the first click [#108](https://github.com/jkobierczynski/FeroSSH/pull/108).
- 30 more colour themes in dark, light and classic menu columns [#105](https://github.com/jkobierczynski/FeroSSH/pull/105). Windows skips dark mode for menus with columns, so FeroSSH draws the Theme submenu itself [#193](https://github.com/jkobierczynski/FeroSSH/pull/193).
- **Standby window** [#115](https://github.com/jkobierczynski/FeroSSH/pull/115). The maintainer set three requirements: choose the window and the hotkey, see whether the hotkey is active, and switch it off. It uses `RegisterHotKey` as a toggle. A hold-to-show mode would need a low-level keyboard hook, which antivirus tools look at harder. When hiding, the keyboard goes back to the previous window [#211](https://github.com/jkobierczynski/FeroSSH/pull/211).
- **Native tools pane.** The maintainer asked for the drawn side panels to become a native Windows pane that the terminal slides over, so the tools can stay partly visible. Mockups came first. Then came the native tools rail [#145](https://github.com/jkobierczynski/FeroSSH/pull/145), Connection manager [#146](https://github.com/jkobierczynski/FeroSSH/pull/146), Session manager [#147](https://github.com/jkobierczynski/FeroSSH/pull/147), connection form [#148](https://github.com/jkobierczynski/FeroSSH/pull/148), Port forwards [#159](https://github.com/jkobierczynski/FeroSSH/pull/159), and Scripts pages [#160](https://github.com/jkobierczynski/FeroSSH/pull/160), [#161](https://github.com/jkobierczynski/FeroSSH/pull/161), [#162](https://github.com/jkobierczynski/FeroSSH/pull/162), [#178](https://github.com/jkobierczynski/FeroSSH/pull/178). The pin works like GitHub's padlock on a panel: with the pin off, clicking outside folds the tools away. Buttons slide left as the terminal covers the pane, so they stay visible [#164](https://github.com/jkobierczynski/FeroSSH/pull/164).
- Window colours: dark menus, title bar and tools pane, because the bright Windows UI was painful next to a dark terminal [#157](https://github.com/jkobierczynski/FeroSSH/pull/157), and neutral greys [#204](https://github.com/jkobierczynski/FeroSSH/pull/204). The mockups' look (Nord, Cascadia Mono, light window colours) became the default.
- A native command palette and configurable key bindings, each with a reset to its default [#199](https://github.com/jkobierczynski/FeroSSH/pull/199), [#201](https://github.com/jkobierczynski/FeroSSH/pull/201), and a native Import connections page [#197](https://github.com/jkobierczynski/FeroSSH/pull/197). After these, no drawn panes are left. Prompts inside the terminal, the Find bar and the tab strip stay drawn on purpose.
- Tidier menus with Session submenus, Recent connections and greyed items [#205](https://github.com/jkobierczynski/FeroSSH/pull/205). This came from an outside proposal, checked against the real menus. A View menu and menu icons were left out.
- Native Windows ARM64 exes, checked on a real ARM64 runner with the sandbox working [#194](https://github.com/jkobierczynski/FeroSSH/pull/194).

### Scripts (formerly snippets)

- Snippets are kept as files in a folder that can be moved to another PC, with categories, search, imports from other clients, keystrokes, `@delay` and an optional Enter [#118](https://github.com/jkobierczynski/FeroSSH/pull/118).
- **Choosing a scripting language.** Embedding a full language, as SecureCRT does, was weighed against extending the existing `@` steps. The choice was Expect-style control steps (wait for one of several texts, branches, loops, timeouts), much like Tera Term's macro language, and Snippets became Scripts [#155](https://github.com/jkobierczynski/FeroSSH/pull/155), [#156](https://github.com/jkobierczynski/FeroSSH/pull/156).
- A login script that looped through a secret step could type the secret forever. The fuzzer found it [#189](https://github.com/jkobierczynski/FeroSSH/pull/189).
- A built-in library of about 270 ready scripts in seven categories, plus Cloud [#210](https://github.com/jkobierczynski/FeroSSH/pull/210). At the maintainer's request, it is written to disk only when the user chooses Session > Scripts > Write built-in scripts. A suggestion to shell-escape every script argument was declined, because it would change every existing script.

### File transfer

- A Local PC section [#96](https://github.com/jkobierczynski/FeroSSH/pull/96). Following `cd` without OSC 7 [#94](https://github.com/jkobierczynski/FeroSSH/pull/94): Debian's stock bash never reports its directory, so FeroSSH reads it from the prompt.
- Resizable sections and search by name [#110](https://github.com/jkobierczynski/FeroSSH/pull/110).
- **Native two-list file transfer** with arrow buttons and drag and drop [#150](https://github.com/jkobierczynski/FeroSSH/pull/150), named bookmarks, synchronized browsing (as in WinSCP) and saved folders per connection [#154](https://github.com/jkobierczynski/FeroSSH/pull/154), with the arrows kept centred in the visible part [#164](https://github.com/jkobierczynski/FeroSSH/pull/164). Bookmarks that didn't always open and a missing Refresh button were found on Windows [#159](https://github.com/jkobierczynski/FeroSSH/pull/159).
- A resume integrity check [#158](https://github.com/jkobierczynski/FeroSSH/pull/158), and file names kept off the remote checksum command line [#165](https://github.com/jkobierczynski/FeroSSH/pull/165), [#166](https://github.com/jkobierczynski/FeroSSH/pull/166).
- Sorting by name, size or date modified, and deleting local files to the Recycle Bin rather than for good [#202](https://github.com/jkobierczynski/FeroSSH/pull/202).

### Logs and recordings

- Save a pane's whole output, or every pane's, to log files [#100](https://github.com/jkobierczynski/FeroSSH/pull/100).
- Session recording with timed replay [#198](https://github.com/jkobierczynski/FeroSSH/pull/198). asciinema's asciicast format was chosen because it is the de facto standard: its files play in asciinema and web players.

### Remote apps and desktops (FeroStream)

The maintainer asked whether a single remote app could appear as a local window. X11 forwarding already does that, but it needs an X server on Windows. Instead of building one in, a standalone client and server came first. FeroStream ([jkobierczynski/ferostream](https://github.com/jkobierczynski/ferostream)) is a native Win32 and Rust client with a small Linux agent, over SSH. Once it worked on its own, it was built into FeroSSH, with the sandbox on both sides and the agent's automatic copy and removal made optional.

- Remote apps built into FeroSSH through a sandboxed worker [#120](https://github.com/jkobierczynski/FeroSSH/pull/120), smaller pictures with FeroStream protocol 6 [#121](https://github.com/jkobierczynski/FeroSSH/pull/121), clipboard, live changes and mouse selections [#123](https://github.com/jkobierczynski/FeroSSH/pull/123), and FeroStream pinned to its main branch [#125](https://github.com/jkobierczynski/FeroSSH/pull/125).
- Clipboard for RDP, VNC and virtual desktops, and desktop and remote-app connections that open without a terminal tab [#124](https://github.com/jkobierczynski/FeroSSH/pull/124). The clipboard became off by default after the security review [#126](https://github.com/jkobierczynski/FeroSSH/pull/126).
- The Linux agents get build provenance attestations too [#142](https://github.com/jkobierczynski/FeroSSH/pull/142).

### Fuzzing and testing

- **Local campaigns.** After the first fuzzer crash [#71](https://github.com/jkobierczynski/FeroSSH/pull/71), the maintainer ran long campaigns in WSL with ten parallel jobs. A campaign of more than 300 million inputs across all 13 targets fixed more crashes and a search freeze, added targets and started nightly fuzzing in CI [#73](https://github.com/jkobierczynski/FeroSSH/pull/73). A 4-hour run found a copy crash and a search freeze [#78](https://github.com/jkobierczynski/FeroSSH/pull/78), and round two found another freeze and a crash on a colour escape code [#80](https://github.com/jkobierczynski/FeroSSH/pull/80). Rounds three and four, the latter 8 hours long, came back clean. Local campaigns stopped on 30 October, when every target had saturated, and nightly CI fuzzing took over.
- The fuzzing nightly is pinned to a known-good toolchain [#95](https://github.com/jkobierczynski/FeroSSH/pull/95).
- New fuzz targets for agent forwarding, Mosh start-up, remote names and command history [#200](https://github.com/jkobierczynski/FeroSSH/pull/200), [#203](https://github.com/jkobierczynski/FeroSSH/pull/203). A 6-hour campaign on them found no FeroSSH bugs. Its 66 crashes came from a mistake in the new test.
- Viewer tests take turns on the shared Xvnc [#153](https://github.com/jkobierczynski/FeroSSH/pull/153).

### CI, builds and releases

- A workflow that sends each later full release to winget by itself [#98](https://github.com/jkobierczynski/FeroSSH/pull/98). It skips pre-releases, needs the `WINGET_TOKEN` secret and does nothing without it, and only works once the first release has been submitted to winget by hand.
- CI cancels superseded runs, caches dependencies and fuzzes in parallel, which cut a pull request run from about 25 minutes to about 10 [#151](https://github.com/jkobierczynski/FeroSSH/pull/151), [#152](https://github.com/jkobierczynski/FeroSSH/pull/152).
- Build provenance is verified before local signing [#163](https://github.com/jkobierczynski/FeroSSH/pull/163). Signing needs GitHub CLI 2.93 or later because of a security advisory for older versions.
- The MSVC toolset is pinned [#212](https://github.com/jkobierczynski/FeroSSH/pull/212) and the linker's Rich header is left out [#215](https://github.com/jkobierczynski/FeroSSH/pull/215), because two runner images carried different builds of the same linker and that number ended up in the exes.
- Lessons from releasing: make the tag with git, not on GitHub's website (a v1.3.0 tag made on the website never started the release workflow), and the signing script can resume a release that is already signed [#142](https://github.com/jkobierczynski/FeroSSH/pull/142).

### Documentation

- README and SECURITY.md for 1.0 [#89](https://github.com/jkobierczynski/FeroSSH/pull/89), a user guide that covers every feature, menu by menu [#102](https://github.com/jkobierczynski/FeroSSH/pull/102), the README regrouped by feature with FeroStream agent build docs [#207](https://github.com/jkobierczynski/FeroSSH/pull/207), and five new screenshots [#209](https://github.com/jkobierczynski/FeroSSH/pull/209).

## Security reviews and hardening

Security work runs through the whole history (see milestone 9 above). Three outside reviews led to their own rounds of fixes. Each finding was checked against the code before anything was fixed.

### 9 November 2026: AI-assisted review

An AI-generated (ChatGPT) review of FeroSSH was checked finding by finding. A re-audit of the first fixes followed the same day. Both reviews and their triage are published in [docs/security](docs/security).

- Desktop and remote-app clipboard off by default [#126](https://github.com/jkobierczynski/FeroSSH/pull/126).
- RDP viewer, VNC and IronRDP limits, bounded desktop queues, and password and Pageant hygiene [#130](https://github.com/jkobierczynski/FeroSSH/pull/130), [#131](https://github.com/jkobierczynski/FeroSSH/pull/131). These were merged as one stack, and the stacked pull requests #127 to #129 went in with them.
- The re-audit's findings [#138](https://github.com/jkobierczynski/FeroSSH/pull/138), merged with #135 to #137: RDP dynamic-channel headers split across fragments, queue budgets counted by message, plain VNC approval tied to the connection, and typed passwords kept in one buffer. A virtual desktop that drops its password is refused.
- Text-direction overrides removed from VNC dialog names [#140](https://github.com/jkobierczynski/FeroSSH/pull/140).
- The reviews and their triage published [#133](https://github.com/jkobierczynski/FeroSSH/pull/133).
- The v1.3.0 version bump was held back and redone after these fixes [#141](https://github.com/jkobierczynski/FeroSSH/pull/141). A known leftover is that IronRDP keeps its own copy of an RDP password.

### 10 November 2026: CodeRabbit whole-code-base audit

CodeRabbit only reviews changes, so the whole code base was shown to it as new: pull requests from `main` into an empty branch, which were never merged. It found 13 real issues, none of which escapes the sandbox or runs code remotely. The fixes:

- Bounded work on remote input [#172](https://github.com/jkobierczynski/FeroSSH/pull/172), hardened local handling [#173](https://github.com/jkobierczynski/FeroSSH/pull/173), and tighter release attestation and security docs [#174](https://github.com/jkobierczynski/FeroSSH/pull/174).
- Release provenance checked before signing [#163](https://github.com/jkobierczynski/FeroSSH/pull/163), and SFTP checksum file names kept off the command line [#165](https://github.com/jkobierczynski/FeroSSH/pull/165), [#166](https://github.com/jkobierczynski/FeroSSH/pull/166).

### 17 November 2026: full code base audit (v1.4.2)

A fresh audit of the whole code base led to the v1.4.2 security release. Its [release notes](docs/releases/v1.4.2.md) describe each change.

- X11 setups are checked in the window, and the real X cookie is kept out of the worker [#216](https://github.com/jkobierczynski/FeroSSH/pull/216).
- A host key is refused when `known_hosts` can't be read [#217](https://github.com/jkobierczynski/FeroSSH/pull/217), and a key of another type is blocked like a changed key [#219](https://github.com/jkobierczynski/FeroSSH/pull/219).
- WinRM over HTTP logs in with Kerberos only, unless NTLM is allowed [#218](https://github.com/jkobierczynski/FeroSSH/pull/218).
- Key, certificate and `UserKnownHostsFile` paths on network shares are never opened [#220](https://github.com/jkobierczynski/FeroSSH/pull/220), [#225](https://github.com/jkobierczynski/FeroSSH/pull/225).
- `ProxyCommand` and password commands are started by their full path [#221](https://github.com/jkobierczynski/FeroSSH/pull/221).
- Downloads are marked as from the Internet, and a ZMODEM receive asks first [#222](https://github.com/jkobierczynski/FeroSSH/pull/222).
- Smaller hardening: bidi characters in dialogs, a grid size cap, OSC 52 bursts, and SFTP part files [#223](https://github.com/jkobierczynski/FeroSSH/pull/223).
- CI keeps the update signing key away from compilation, and actions are pinned to commit SHAs [#224](https://github.com/jkobierczynski/FeroSSH/pull/224).
- RDP certificates are pinned on first use [#226](https://github.com/jkobierczynski/FeroSSH/pull/226), and forwards only serve the same Windows session [#227](https://github.com/jkobierczynski/FeroSSH/pull/227).

## What testing on Windows found

CI runs on Windows, but nothing in it clicks through the UI. Trying each build on a real PC found bugs that no test caught, and settled several questions:

- **Fixed after trying a build:** stray mouse reports after reattaching tmux and leaving htop [#12](https://github.com/jkobierczynski/FeroSSH/pull/12), a forwards panel that lost its pane when typed into [#13](https://github.com/jkobierczynski/FeroSSH/pull/13), hidden bottom rows [#18](https://github.com/jkobierczynski/FeroSSH/pull/18), a stale tab title after Midnight Commander [#23](https://github.com/jkobierczynski/FeroSSH/pull/23), a multi-host run that never typed its command [#76](https://github.com/jkobierczynski/FeroSSH/pull/76), Open local shell ending the pane's tmux session [#77](https://github.com/jkobierczynski/FeroSSH/pull/77), tmux "off" read as a session name [#109](https://github.com/jkobierczynski/FeroSSH/pull/109), tmux keys that didn't repeat [#107](https://github.com/jkobierczynski/FeroSSH/pull/107), a Bookmarks button that didn't always open [#159](https://github.com/jkobierczynski/FeroSSH/pull/159), renaming that stopped after one character [#177](https://github.com/jkobierczynski/FeroSSH/pull/177), and the empty band from an old tmux client [#206](https://github.com/jkobierczynski/FeroSSH/pull/206).
- **Not FeroSSH bugs:**
  - "A" and "B" lines on arrow keys in vim come from Debian's `vim.tiny`, which runs in vi-compatible mode. `set nocompatible` or the full vim fixes it.
  - Selecting text with the mouse entering visual mode is full vim's `mouse=a`. Shift+drag selects locally.
  - The exe "growing to 3.1 MB" was the zip size being compared with the exe size.
- **Antivirus:** Bitdefender flagged `--check-sandbox` as Gen:Suspicious.Cloud, a reputation verdict for a new file, not a detection of what the code does. The answer was a false-positive report, with no code change. Windows also won't delete the exe while a worker process is still running.
- **WinRM on Windows Home** needed several setup steps: a login sent out through another machine and back hits Windows' NTLM loopback check, the account needs Execute (Invoke) in `winrm configSDDL`, and "Server execution failed" came from the account itself. Windows' own `winrs` failed the same way, which showed that FeroSSH was not at fault.
- **Confirmed working on Windows:** the first SSH login, the sandbox self-test, both desktop viewers, FIDO2 with a phone passkey, automatic reconnect, snippets, jump hosts, the native tools pane and key generation.

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

How a release is made: a pull request bumps the version in `Cargo.toml`, adds `docs/releases/<tag>.md` and updates this file. After it merges, the maintainer tags the merge commit with git. [`release.yml`](.github/workflows/release.yml) then builds and attests the exes, and `scripts/sign-release.ps1 -Tag <tag>` signs and publishes them. See [Checks and releases](README.md#checks-and-releases) in the README.

## Updating this file

Update this file in the same pull request that bumps the version and adds the release notes, so the history is always as current as the latest release:

1. Add a row to [Releases at a glance](#releases-at-a-glance) and to the table under [Releases](#releases), with the range of pull requests merged since the previous tag (`git fetch origin && git log --first-parent --oneline <previous tag>..origin/main`, skipping direct commits that aren't pull requests). The version bump pull request isn't merged yet when you write this, so add its own number by hand. After it merges, run the same command against main up to the commit you will tag; if another pull request merged in between, add it to both tables before tagging.
2. Add each new feature, fix or security change as a line under its area in [After 1.0, by area](#after-10-by-area) or [Security reviews and hardening](#security-reviews-and-hardening), with its pull request number. Give the reason for a decision and any option that was turned down, and add to [What testing on Windows found](#what-testing-on-windows-found) when trying a build found something. Start a new area when nothing fits.
3. Change "Last updated for" at the top to the new version and date.

Keep it to public information: what shipped and why, not unpublished findings from private reviews, and no host names, addresses or credentials from testing.
