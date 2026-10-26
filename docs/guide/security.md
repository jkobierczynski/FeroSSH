# Security

FeroSSH is built so that a bug in the code that reads network data cannot reach your files or your keys. This page explains how, and what you still need to do yourself.

## Two processes

- **The window** draws the terminal, reads your keyboard, holds your settings and talks to the key agents. It never parses data from the network.
- **The SSH worker** speaks SSH to the server. It runs in a Windows AppContainer that allows network access and nothing else: it cannot read your files, your registry or your private keys. When the server wants a signature, the worker asks the window, and the window asks your agent, Pageant, the key file or your security key.

The two talk over a pipe with a small, fuzz-tested message format. The desktop viewer in `ferossh-desktop.exe` runs in a third process, in an AppContainer with no capabilities at all, not even the network: it only sees what comes through the SSH tunnel.

Windows does not let AppContainers connect to 127.0.0.1, so for an SSH server on the same PC you start FeroSSH with `--no-sandbox`. Port forwards to and from localhost work in the sandbox, because the window holds those sockets.

## Host keys

A new host shows its SHA-256 fingerprint; check it before you type `yes`. A changed host key fills the window with a warning and cannot be clicked away. If you know why it changed (the server was reinstalled), Session > Replace host key... replaces it after asking twice. Host certificates (`@cert-authority` in `known_hosts`) and `@revoked` keys work as in OpenSSH.

## Keys and passwords

- Private keys stay in the Windows OpenSSH agent, Pageant, a FIDO2 security key or passkey, or a key file you choose. Key file passphrases are asked for each time and never kept.
- FeroSSH never stores a password itself. A session can take its password from Windows Credential Manager, a password manager's command line tool or a command of your own, and FeroSSH asks before running such a command the first time.
- Imports from other clients never bring passwords over.
- Logs never contain passwords or passphrases.

## Crypto

Only modern algorithms are offered: post-quantum hybrid `mlkem768x25519-sha256` key exchange first, then Curve25519; ChaCha20-Poly1305 and AES-GCM; Ed25519, ECDSA and RSA with SHA-2 (3072 bits or more). SHA-1, CBC ciphers, DSA and `ssh-rsa` signatures are never offered. Old network gear that needs more can be allowed per host with `LegacyAlgorithms yes`, which still excludes SHA-1, CBC and DSA, and the tab then shows an amber ⚠.

## X11 and forwarding

X11 forwarding is off unless a host turns it on, because X programs on the server can see and type into your other X windows. The server only ever gets a fake X cookie. A tab shows `⇄` when its connection forwards ports, in amber when the server can connect back into your PC.

## No telemetry

FeroSSH sends nothing anywhere except to the servers you connect to, and to GitHub when you check for updates.

## Update checks

Help > Check for updates, and the weekly check if you turn it on, fetch two small files from the latest GitHub release: `update.txt` and its Ed25519 signature. FeroSSH checks the signature against the public key built into the exe before it reads anything in the file, and the file can only point to a page on the project's Releases. Nothing is downloaded or installed for you: a newer release is opened in your browser, and you download and check it as described in [Verifying a download](verifying-downloads.md). The request carries no identifier, only the usual `User-Agent: FeroSSH/<version>`.

## Reporting a vulnerability

See [SECURITY.md](../../SECURITY.md).
