# Keeping the connection log by machine policy

FeroSSH keeps a [connection log](../guide/everyday-use.md#connection-log) on each PC: one entry for every connection made, with when, to which address, through which jump hosts, with which host key, and how it ended. By default users can delete entries from it in **Tools > Connection log**. On PCs you manage, two machine policies decide that for every user instead, and give you a copy of the log that users cannot remove.

## What the policies do

Both live under `HKEY_LOCAL_MACHINE\Software\Policies\FeroSSH`, which only an administrator of the PC can change, beside the [team library policy](team-library-policy.md). FeroSSH reads them at start and again on each Refresh in the connection manager. A portable FeroSSH, another `%APPDATA%` or a settings file does not change them.

- **Allow deleting connection log entries.** Disabled, nobody can delete entries in FeroSSH: the **Delete...** button and the Delete key do nothing, and the page says that the organisation keeps every entry. Enabled or not configured, users can delete entries after a question.
- **Copy the connection log to the Windows event log.** Enabled, FeroSSH also writes every entry to the Windows **Application** event log, source `FeroSSH`, with the user's account. A user who is not an administrator can add to that log but cannot clear or change it, and Windows Event Forwarding or your SIEM agent can collect it. When a user deletes entries from FeroSSH's own log (where that is allowed), the event log says how many.

## What gets logged

| Event | Event ID | Type | When |
|---|---|---|---|
| Connected | 1001 | Information | A login went through (a shell, tunnels only, or file transfer only). |
| Disconnected | 1002 | Information | A connection that was up ended: the user closed it, the shell exited, or the server closed it. Says how long it was up and why. |
| Lost | 1003 | Warning | A connection that was up dropped, and FeroSSH reconnects. |
| Failed | 1004 | Warning | No login: the host could not be reached, refused, the host key changed, or the login was turned down or given up. Says why. |
| Opened | 1005 | Information | Remote Desktop Connection was started directly (no SSH tunnel), or failed to start. |
| Entries deleted | 1010 | Warning | The user deleted entries from FeroSSH's own log. |

Each entry has the connection's name, `user@host:port`, the protocol (`ssh`, `telnet`, `winrm`, `winrm-https`, `rdp`), the jump hosts and proxy on the way, the host key's type and SHA-256 fingerprint, and the details. Text that comes from a server (a disconnect reason) is cleaned of control characters and cut short before it is kept.

## Registering the event source

Run this once per PC, as an administrator, so Event Viewer shows each event's text without a "description cannot be found" note (the text is in the event either way):

```
New-EventLog -LogName Application -Source FeroSSH
```

That is Windows PowerShell 5.1; deployment tools that run as SYSTEM can run it too.

## What it does not do

The policies bind FeroSSH, not the files. A user can still delete FeroSSH's own log file (`%LOCALAPPDATA%\FeroSSH\connections.log`, or `connections.log.sealed` while a master password vault is open), or run another SSH client. The event log copy is the record users cannot remove; use application control (AppLocker or WDAC) if other clients must be blocked too. As with the team library, the policies bind users who are not administrators of their PC.

## Setting it up

Add the template from [`policy/`](policy/) as described in the [team library policy](team-library-policy.md#setting-it-up), then under **Computer Configuration > Administrative Templates > FeroSSH**:

1. Disable **Allow deleting connection log entries**.
2. Enable **Copy the connection log to the Windows event log**.

## Registry values

| Value under `HKLM\Software\Policies\FeroSSH` | Type | Meaning |
|---|---|---|
| `ConnectionLogDelete` | REG_DWORD | `0`: nobody can delete entries in FeroSSH. `1`: everyone can. Not set: up to the user (allowed). |
| `ConnectionLogEventLog` | REG_DWORD | Not `0`: copy every entry to the Application event log. |

For example:

```
reg add HKLM\Software\Policies\FeroSSH /v ConnectionLogDelete /t REG_DWORD /d 0 /f
reg add HKLM\Software\Policies\FeroSSH /v ConnectionLogEventLog /t REG_DWORD /d 1 /f
```

These values do not turn on the team library policy: that needs its own values.
