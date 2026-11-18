# Plan: FeroSSH for coding agents on remote hosts

More and more work over SSH is a coding agent (Claude Code, Codex CLI, aider and the like) running in a terminal on a remote machine for minutes or hours at a time. This plan lists six features that would make FeroSSH a good home for that, what FeroSSH already has for each, and what is left to build. Features 1 and 2 are being built first; the others follow when the maintainer picks them.

Written 18 November 2026, against `main` at 3d28477.

## 1. Attention alerts (building now)

**What it is.** When an agent stops to ask a question or finishes its task, the tab gets a badge and Windows shows a notification, so you can work elsewhere and come back when it needs you.

**What FeroSSH has.** A bell (BEL) in a pane that is out of sight turns its tab's dot into a diamond (◆), and the taskbar button flashes when FeroSSH is not the active window. Nothing reads the desktop-notification escape sequences, there are no Windows notifications, and a pane that simply goes quiet raises nothing.

**What to build.**

- Read the notification sequences agents and shells send: OSC 9 (iTerm2 style, used by Codex CLI and by Claude Code with its `iterm2` channel), OSC 777 `notify` (used by Claude Code's `ghostty` channel and by many shell hooks) and OSC 99 (kitty). They are found on the side of the parser, like OSC 7 today, and their text is sanitised like titles. ConEmu's `OSC 9;4` progress reports are not notifications and are left alone.
- A **watch for quiet** per pane: after a stretch of output, a few seconds of silence means the agent is done or waiting. This catches agents that send nothing.
- What happens: the tab shows a coloured badge until you look at the pane, and if FeroSSH is not the active window, or the pane is out of sight, Windows shows a notification with the host, the tab and the message. Clicking it brings FeroSSH forward on that pane. Repeats are throttled per pane.
- **Off by default.** Settings > Desktop notifications turns them on; Session > Notify when this pane goes quiet turns on the quiet watch per pane. The bell keeps its current behaviour, and counts as a notification too unless Settings > Notify on the bell too is turned off.
- Security: the text is the host's, so it is shown as plain text, cut short and stripped of control and bidi characters; it can never run anything. Notifications use the shell's own balloon API (`Shell_NotifyIcon`), which Windows 10 and 11 show as notifications without an install or AppUserModelID.

## 2. Persistent agent sessions (building now)

**What it is.** One click starts an agent in tmux on the server, so it keeps working when the laptop sleeps or the network drops, and comes back by itself when FeroSSH reconnects. A dashboard shows every agent and whether it is working, waiting for you or done.

**What FeroSSH has.** tmux control-mode integration (opt-in per host): every tmux window becomes a native pane, the layout is kept in tmux, and after a dropped connection automatic reconnect attaches the same tmux session again. Workspaces can rebuild a tmux session that is gone. The Session manager lists the sessions open in all tabs.

**What to build.**

- Session > Coding agents > Start … in tmux: Claude Code, Codex, aider, or your own commands (setting `AgentCommand`). It starts the agent in its own tmux window in the pane's directory, in tmux session `agents` (or the session already attached on that host), and marks the window as an agent with the tmux window option `@ferossh-agent`, so it is recognised again after a reattach or from another PC. The shell stays when the agent exits, so it can be restarted in place.
- The quiet watch from feature 1 is on for agent panes, so every agent can raise a notification without setup of its own. Settings > Desktop notifications must be on (it is off by default); with it off, agent panes show their state in the dashboard but raise no badge or notification.
- The Session manager gets a **Coding agents** group: each agent with its host, folder, state (working, waiting for you, quiet) and how long it has been in that state. Enter shows it.
- Nothing runs unless you start it; no setting changes for anyone who does not use it. The SSH side stays in the sandboxed worker: everything is typed at the shell or sent as tmux control commands, as tmux integration does today.

## 3. Paste and drop images and files into the remote

**What FeroSSH has.** Dropping files from Explorer (or from File transfer's Local list) on a terminal pane asks, then uploads them over SFTP to that pane's directory. Ctrl+V pastes text only, bracketed and sanitised.

**What to build.** Pasting an image (a screenshot on the clipboard) or dropping a file uploads it to a temporary folder on the host (`~/.cache/ferossh/paste/`), then types its quoted path at the cursor, which is how agents take images. An option chooses between "upload and type the path" and today's "upload to the pane's folder". The upload uses the existing SFTP queue; nothing new runs in the window process.

## 4. Port-forward previews

**What FeroSSH has.** Local, remote and dynamic forwards, editable while connected (Tools > Port forwards). Links are opened with Ctrl+click only when the host marks them with OSC 8; plain `http://localhost:5173` text is not a link.

**What to build.** Recognise `http://localhost:PORT` and `127.0.0.1:PORT` in a pane's output and show a small bar: "Port 5173 is listening on build-box. Forward and open". One click adds a local forward to a free local port and opens the browser on it; the forward is listed like any other and goes with the session. Off by default, and never automatic: each port is a click.

## 5. Rendering for agent TUIs

**What FeroSSH has.** Synchronized output (DEC mode 2026), OSC 8 links with the real target shown before opening, bracketed paste, true colour, OSC 52 clipboard with a prompt, and fast redraws.

**What to build.**

- **Shift+Enter** sends a sequence the agents read as a new line in their input box (`ESC [13;2u`, or `ESC CR` where that is configured), instead of the plain Enter it sends today. A setting chooses it.
- **Kitty keyboard protocol** (progressive enhancement, at least the "disambiguate" level), which Claude Code, Codex and others use when the terminal answers the query. It is off in the emulator today.
- Check OSC 133 prompt marks and OSC 9;4 progress (shown as a progress bar in the tab and taskbar).

## 6. Safety for agent input

**What FeroSSH has.** Production and Staging themes by host rule or ssh_config `Tag`, a confirmation before scripts run on Production hosts, sealed (tamper-evident) session logs, and asciicast recordings of a pane's output.

**What to build.**

- The Production confirmation also covers pastes and file drops into an agent pane on a Production host, and starting an agent there.
- An **agent audit trail**: recording and logging switch on by themselves for agent panes (a setting), so what an agent did on a server is kept in the sealed logs, named after the agent and folder.
- Optional: warn when an agent pane is about to receive agent forwarding or a forwarded clipboard.

## Order

1 and 2 first, as two pull requests (2 builds on the quiet watch from 1). Then 5 (small, high value for daily agent use), 3, 4 and 6.
