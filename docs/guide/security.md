# Security

FeroSSH is built so that a bug in the code that reads network data cannot reach your files or your keys. This page explains how, and what you still need to do yourself.

## Separate processes

- **The window** draws the terminal, reads your keyboard, holds your settings and talks to the key agents. It never parses data from the network.
- **The SSH worker** speaks SSH to the server. It runs in a Windows AppContainer that allows network access and nothing else: it cannot read your files, your registry or your private keys. When the server wants a signature, the worker asks the window, and the window asks your agent, Pageant, the key file or your security key. Each SSH connection has a worker process of its own (tabs that share a login share it too), so a server that took over its worker still cannot see your sessions on other servers.

Every sandboxed process also runs in a job object (it dies with the window, cannot start other programs, is capped at 1 GiB of memory and cannot touch the clipboard or other windows) and starts with Windows' exploit mitigations turned on: no Win32k system calls, Control Flow Guard, CET shadow stacks where the CPU has them, no dynamic code and mandatory ASLR. `ferossh.exe --check-sandbox | more` shows what your Windows allows.

The window and each worker talk over a pipe with a small, fuzz-tested message format. The desktop viewer in `ferossh-desktop.exe` runs in a third process, in an AppContainer with no capabilities at all, not even the network: it only sees what comes through the SSH tunnel.

Remote apps (Session > Remote apps) add a decoder process of the same kind: an AppContainer with no capabilities, one per remote app. Everything the host's FeroStream agent sends goes from the worker straight to it; it unpacks the pictures and hands back plain pixels and messages, which the window checks again before it opens or draws a window. A decoder that tried to send anything else (a packed picture, an odd machine name, too long a text) ends the remote app. The agent runs on the host as your user, on the SSH connection you already have; copying it to the host and removing it afterwards are both settings (`RemoteAppCopyAgent`, `RemoteAppRemoveAgent`), and a copied agent is checked by size and checksum before it runs.

Windows does not let AppContainers connect to 127.0.0.1, so for an SSH server on the same PC you start FeroSSH with `--no-sandbox`. Port forwards to and from localhost work in the sandbox, because the window holds those sockets.

## Host keys

A new host shows its SHA-256 fingerprint; check it before you type `yes`. A changed host key fills the window with a warning and cannot be clicked away. If you know why it changed (the server was reinstalled), Session > Replace host key... replaces it after asking twice. Host certificates (`@cert-authority` in `known_hosts`) and `@revoked` keys work as in OpenSSH.

## Keys and passwords

- Private keys stay in the Windows OpenSSH agent, Pageant, a FIDO2 security key or passkey, or a key file you choose. Key file passphrases are asked for each time and never kept.
- FeroSSH never stores a password itself. A saved connection can take its password from Windows Credential Manager, a password manager's command line tool or a command of your own, and FeroSSH asks before running such a command the first time.
- Imports from other clients never bring passwords over.
- Agent forwarding is off unless a host turns it on (`ForwardAgent yes` or *Fwd agent*). Then the sandboxed worker reads the agent protocol and passes on only two requests, the list of keys and a signature, and FeroSSH asks you before every signature, naming the host a login is for. Signatures that are not an SSH login or an SSH signature (`ssh-keygen -Y`, git) are refused, and so is SHA-1 RSA. Only the agent's keys are offered, never your key files.
- Kerberos logins use the tickets of your Windows logon through Windows itself (SSPI); FeroSSH never sees a password or key for them. The window asks Windows for a ticket only for the `host/` service of the host you connected to, requires the server to prove it holds that service's key, and signs only a Kerberos login request for your own user name. The sandboxed worker passes on the opaque exchange and never holds a ticket. Your ticket is passed on to the server only when a host asks for it (`GSSAPIDelegateCredentials yes` or *Fwd ticket*).
- Logs never contain passwords or passphrases.

## Crypto

Only modern algorithms are offered: post-quantum hybrid `mlkem768x25519-sha256` key exchange first, then Curve25519; ChaCha20-Poly1305 and AES-GCM; Ed25519, ECDSA and RSA with SHA-2 (3072 bits or more). SHA-1, CBC ciphers, DSA and `ssh-rsa` signatures are never offered. Old network gear that needs more can be allowed per host with `LegacyAlgorithms yes`, which still excludes SHA-1, CBC and DSA, and the tab then shows an amber ⚠.

## X11 and forwarding

X11 forwarding is off unless a host turns it on, because X programs on the server can see and type into your other X windows. The server only ever gets a fake X cookie. A tab shows `⇄` when its connection forwards ports, in amber when the server can connect back into your PC.

## Telnet

Telnet sends everything in the clear, passwords included. FeroSSH offers it only for old gear that has nothing better: the pane says so when it connects and the tab carries a red **⚠ telnet** badge. The Telnet connection runs in a sandboxed worker like SSH, agrees to only a handful of options, and never sends your user name or environment.

## WinRM

A WinRM connection's password and its login (Kerberos or NTLM, through Windows' SSPI) stay in the FeroSSH window. The sandboxed worker that talks to the server only passes the login's tokens along and asks the window to encrypt each request and decrypt each reply, so it never sees the password or the keys. The login tokens travel in the HTTP `Authorization` header, as WinRM over HTTP sends them; after the login every shell request and reply is encrypted with the login's keys, and a reply that is not is refused, whatever it says. The HTTP, encryption framing, SOAP and XML parsers run in the worker and are fuzz-tested; the XML reader accepts no DTDs. NTLM, which Windows falls back to outside a domain or for a server named by address, does not prove the server's identity: the pane says so. FeroSSH never delegates your credentials to the server (no CredSSP).

Over HTTPS (port 5986) the TLS session is Windows' own SChannel, run in the window with strong ciphers only; the worker carries the encrypted bytes and never holds the TLS keys. The window checks the server's certificate itself, before any login token is sent: Windows' chain and server-name check first, and for a certificate Windows does not trust, the SHA-256 fingerprint the user accepted once (kept in `%APPDATA%\FeroSSH\winrm_certs`). A different certificate later ends the login, as a changed SSH host key does. The Kerberos or NTLM login over HTTPS carries the TLS connection's channel bindings (Extended Protection for Authentication), so it is valid only on that connection and cannot be relayed through another.

## Mosh

A Mosh connection logs in with SSH, with the usual host key check, and only then starts `mosh-server`. The session key it prints comes back over that SSH connection; after that the terminal goes over UDP, encrypted and authenticated with AES-128-OCB, so nobody on the way can read or change it. FeroSSH parses the server's packets and screens in the sandboxed worker, and those parsers are fuzz-tested. The UDP ports have to be open on the host, which widens what the host exposes: narrow them with *UDP ports* if you can.

## Logs

Logs never contain what you type at a password prompt. Each log is sealed with a SHA-256 hash chain, so a later change to it shows; see [Logs](logs.md) for what that does and does not protect against.

## No telemetry

FeroSSH sends nothing anywhere except to the servers you connect to, and to GitHub when you check for updates.

## Update checks

Help > Check for updates, and the weekly check if you turn it on, fetch two small files from the latest GitHub release: `update.txt` and its Ed25519 signature. FeroSSH checks the signature against the public key built into the exe before it reads anything in the file, and the file can only point to a page on the project's Releases. Nothing is downloaded or installed for you: a newer release is opened in your browser, and you download and check it as described in [Verifying a download](verifying-downloads.md). The request carries no identifier, only the usual `User-Agent: FeroSSH/<version>`.

## Reporting a vulnerability

See [SECURITY.md](../../SECURITY.md).
