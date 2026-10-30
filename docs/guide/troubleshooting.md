# Troubleshooting

## Windows or my browser warns about the download

Until releases are code-signed, Windows SmartScreen shows "Windows protected your PC" the first time you run the exe: choose More info > Run anyway. Firefox and Chrome can block the download itself because few people have downloaded it yet; in Firefox, open the Downloads panel, right-click the file and choose Allow download. Check the file first, as described in [Verifying a download](verifying-downloads.md).

## I can't connect to an SSH server on my own PC

Windows does not let the sandboxed SSH worker connect to 127.0.0.1. Start FeroSSH with `--no-sandbox` for such servers, for example a local VM reached through a forwarded port. Other servers keep the sandbox.

## An old switch or router will not connect

FeroSSH only offers modern crypto by default, and it says so when a server needs older algorithms. Add `LegacyAlgorithms yes` to that host's profile in `ferossh.conf`:

```
Profile core-switch
    LegacyAlgorithms yes
```

Devices that only know SHA-1, CBC ciphers or DSA keys are not supported; update their firmware or SSH settings if you can.

## My RSA key is refused

RSA keys work through an agent only (the Windows OpenSSH agent or Pageant), and need 3072 bits or more. Load the key with `ssh-add` or into Pageant, or move to an Ed25519 key (`ssh-keygen -t ed25519`). RSA `.ppk` files work through Pageant too.

## The host key changed

FeroSSH refuses to connect and does not offer a way around the warning, because a changed key can mean someone is in the middle. Ask the server's administrator whether the server was reinstalled or its keys were replaced. If it was, use Session > Replace host key....

## Keys on a remote system don't work in vim or another program

Check `TERM` on the server (`echo $TERM`); FeroSSH sets `xterm-256color`. Some minimal vi builds, such as Debian's `vim.tiny`, start in vi-compatible mode and show letters like `A` and `B` for the arrow keys in insert mode. Install the full `vim` package, or put `set nocompatible` in `~/.vimrc`.

## Where are my settings and logs?

| What | Where |
| --- | --- |
| Settings, saved and imported connections | `%APPDATA%\FeroSSH` |
| Logs, password-command approvals | `%LOCALAPPDATA%\FeroSSH` |
| Known hosts, ssh_config, key files | `%USERPROFILE%\.ssh` |

To move FeroSSH to another PC, copy `%APPDATA%\FeroSSH` and your `.ssh` folder. Password command approvals are deliberately not copied, so a copied settings file can't run commands without asking.
