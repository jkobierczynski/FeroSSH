# Security policy

## Supported versions

FeroSSH is young: only the latest release gets security fixes. Fixes go
out as a new release, and its notes say what was fixed.

## Reporting a vulnerability

Please report vulnerabilities privately, through **Report a vulnerability**
on the repository's Security tab (GitHub private vulnerability
reporting). Do not open a public issue for them. Expect a first answer
within a week. Once a fix is released, the advisory is published with
credit to you, unless you prefer otherwise.

Useful to include: the FeroSSH version (or commit), the Windows version,
what an attacker controls (a server, the network, a file the user opens,
another program on the PC), and steps or a file that reproduce it.

## How FeroSSH is built to contain bugs

FeroSSH assumes that whatever comes from the network may be hostile, and
splits the program so that a bug in the code that reads it reaches as
little as possible. One exe runs as three kinds of process:

- **The window** (the UI process) holds everything worth having: your
  files, keys and agents, `known_hosts`, saved passwords, the clipboard,
  logs and local sockets. It never speaks SSH itself.
- **The SSH workers** hold the network connections and run the SSH
  protocol (russh), one worker process for each SSH connection: tabs
  that share a login share its worker, but another host or user gets a
  worker of its own. Each runs in an AppContainer that may use the
  network and nothing else: none of your files, registry or other
  processes. It asks the window for everything else over a pipe.
- **The desktop viewer**, only in the builds with the built-in VNC or RDP
  viewer, decodes remote screens in an AppContainer with no capabilities
  at all: no network, files or registry.

Every sandboxed process runs in a job object that kills it with the
window, lets it start no other process, caps its memory at 1 GiB and
keeps it away from the clipboard, the desktop and other processes'
windows. They also start with process mitigations: no dynamic code, no
legacy extension points, no non-system fonts, mandatory ASLR, strict
handle checks and DLLs from System32 first. `ferossh.exe --check-sandbox`
reports what your Windows allows.

The window treats both as if an attacker had taken them over:

- A worker only reaches the shells, file transfers and forwards of its
  own connection; anything it sends about another connection's is
  dropped.
- Every frame on the pipes is checked strictly: sizes are capped, unknown
  messages are errors, and a message only the window sends ends the
  worker's pipe.
- The worker must follow the order of a real login: one host key check,
  then keys, signatures and prompts only after the host key is trusted,
  and none of these once the shell is open. Out of turn, the window
  closes the connection.
- The window signs only a well-formed SSH user-authentication request for
  the user and key of that login, with an allowed algorithm, so the
  worker cannot use your keys to sign anything else. Host keys are
  checked against `known_hosts` in the window.
- Forwarded connections must stay within their flow-control window.
- Text from the server or the viewer (banners, prompts, titles) is
  stripped of control and bidirectional-override characters before it is
  shown.

The parsers for untrusted input (the IPC decoders, the terminal, SFTP,
SOCKS and X11, tmux control mode, VNC and RDP, key files, sign requests,
`ssh_config`, `known_hosts` and session imports) are fuzzed: briefly on
every pull request, and for longer every week.

## Known limits

- `--no-sandbox` runs the worker as a normal process, without the
  AppContainer or the mitigations. It exists because an AppContainer
  cannot reach `localhost`; use it only for such hosts.
- A taken-over worker still sees what you type into a shell and what the
  server sends back, and it can relay a login it was given to another
  server it can reach. The checks above stop it from doing more with
  your keys, files or passwords than the connection you opened needs.
- Release exes are not code-signed yet.
