# Enforcing the team library by machine policy

By default the [team library](../guide/settings.md#team-library) is something each user turns on in **Settings > Team library**, and can turn off again. On PCs you manage, a machine policy makes it binding: every user of the PC gets the library you name, signed by a key you name, and cannot change or remove it from FeroSSH.

## What the policy does

The policy lives under `HKEY_LOCAL_MACHINE\Software\Policies\FeroSSH`, which only an administrator of the PC can change. While it is set:

- **The policy's library wins.** The user's own `TeamLibrary` and `TeamLibraryKey` settings are ignored. Settings > Team library shows the policy's library and keys, and cannot change them.
- **Team rules cannot be weakened.** As with any team library, its connections cannot be changed, deleted or replaced by a connection of the same name, and the user's own security rules can only add limits to the team's.
- **It fails closed.** With *Require the team library to connect* (on by default), FeroSSH connects nowhere (SSH, SFTP, Telnet, WinRM, Remote Desktop) while it has no good library: not from the share and not from the checked copy of an earlier read. The pane says why. Local shells and serial ports still open. On the first start on a PC, with no checked copy yet, FeroSSH reads the share before it opens the window, so a connection given on the command line is not refused only because the background read had not finished.
- **Optionally, team connections only.** With *Allow only the team library's connections*, FeroSSH opens only the library's connections, each exactly as the library defines it: addresses typed at Connect to..., the user's own saved and imported connections and their `~/.ssh/config` are not used, and every jump host on the way must be a team connection too. A user's own named credential (user, key, password) still applies to a team connection that names it, so nobody needs to share a password or key.
- **No way around it in FeroSSH.** The policy is read from the registry, never from the settings file, so a portable FeroSSH (a `FeroSSH-data` folder beside the exe), another `%APPDATA%`, or a settings file in a master password vault does not change it. Environment variables in the library path are not expanded. FeroSSH reads the policy at start and again on each Refresh in the connection manager.

A policy that names no library file or no signing key leaves nothing FeroSSH can check, so it connects nowhere and says that the policy is incomplete.

## What it does not do

The policy binds users who are **not administrators** of their PC. An administrator can delete the registry key. And nothing in FeroSSH stops someone running another SSH client (`ssh.exe`, PuTTY) or an older FeroSSH that does not read the policy. Where that matters, use application control (AppLocker or Windows Defender Application Control) to allow only the FeroSSH you ship and block other SSH clients. It suits managed company PCs, not someone who controls their own machine.

## Setting it up

1. Export the library from FeroSSH (**File > Import and export > Export team library**), sign it and put both files on a share every user can read but only administrators can write:

   ```
   ssh-keygen -Y sign -f admin_key -n ferossh-library team.ferossh-library
   ```

2. Add the template: copy [`policy/FeroSSH.admx`](policy/FeroSSH.admx) to `C:\Windows\PolicyDefinitions` (or your domain's Central Store) and [`policy/en-US/FeroSSH.adml`](policy/en-US/FeroSSH.adml) to its `en-US` folder. In Intune, import both under **Devices > Configuration > Import ADMX**.

3. Under **Computer Configuration > Administrative Templates > FeroSSH**, enable **Use this team library**. Give the library's full path (`\\server\share\team.ferossh-library`) and, under Signing keys, one public key line per entry, as in the administrator's `.pub` file (`ssh-ed25519 AAAA... admin`). Two keys let you change keys without a gap.

4. Optionally disable **Require the team library to connect** (connect as if there were no library while it cannot be read), or enable **Allow only the team library's connections**.

## Registry values

Without Group Policy or Intune, set these yourself (for example from a deployment script running as SYSTEM):

| Value under `HKLM\Software\Policies\FeroSSH` | Type | Meaning |
|---|---|---|
| `TeamLibrary` | REG_SZ | The library's full path. |
| `TrustedKeys` (subkey) | REG_SZ values, any names | One public key line each. The ADMX list names them `1`, `2`, ... |
| `TrustedKeys` | REG_MULTI_SZ | The same keys as one value, a line each (instead of the subkey, or as well). |
| `RequireTeamLibrary` | REG_DWORD | `0` turns off failing closed; anything else, or not set, leaves it on. |
| `OnlyTeamConnections` | REG_DWORD | `1` allows only the library's connections. |

Any of these values being set turns the policy on. For example:

```
reg add HKLM\Software\Policies\FeroSSH /v TeamLibrary /t REG_SZ /d "\\server\share\team.ferossh-library" /f
reg add HKLM\Software\Policies\FeroSSH\TrustedKeys /v 1 /t REG_SZ /d "ssh-ed25519 AAAA... admin" /f
```
