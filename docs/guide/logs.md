# Logs

FeroSSH can keep what a session shows in a log file, and seals every log so you can tell later whether anyone changed it.

## Logging a tab

Session > Log to file starts a log for the focused tab; pick it again to stop. Each tab gets its own file in `%LOCALAPPDATA%\FeroSSH\logs`, named after the host, the tab and the time it started:

```
web1_tab1_2026-10-24_2215.log
```

Session > Open log folder opens that folder in Explorer.

There are two kinds of log:

- **Plain** (the default) keeps the readable text, as you saw it, without colours or cursor movements. Session > Timestamp each log line puts the local time each line started in front of it, to the millisecond:

  ```
  [2026-10-26 19:24:49.123] $ uptime
  [2026-10-26 19:24:49.131]  19:24:49 up 12 days,  3:02,  1 user,  load average: 0.08, 0.03, 0.01
  ```

  The setting applies to open logs from their next line and stays on for new ones (`LogTimestamps yes` in `ferossh.conf`).
- **Raw**: Session > Log raw output (keep escape codes) keeps every byte the host sent, colours and all, so `cat file.log` in a terminal replays the session. Raw logs are never timestamped, because that would break the replay.

Passwords and passphrases you type never reach a log.

## Saving what a pane already shows

Turned logging on too late? Session > Save this pane's output to a log writes everything the focused pane still holds, its whole scrollback and the screen, as plain text, to a new file ending in `_saved.log`:

```
web1_tab1_2026-10-24_2215_saved.log
```

Session > Save output of all panes to logs does that for every pane in every tab, one file each. A message lists the files it wrote. How much there is depends on the scrollback (10,000 lines unless you set `Scrollback`).

## Sealed logs and chain files

Every log FeroSSH writes, plain, raw or saved, gets a **chain file** next to it, with the same name plus `.chain`:

```
web1_tab1_2026-10-24_2215.log
web1_tab1_2026-10-24_2215.log.chain
```

The chain file is a list of **seals**. A seal is a SHA-256 fingerprint of the log's contents up to that point, combined with the seal before it, so each seal vouches for everything written before it. FeroSSH adds a seal at least every 10 seconds while output comes in (or after each megabyte), and a final one when the log ends. It looks like this:

```
FeroSSH log chain 1
log web1_tab1_2026-10-24_2215.log
seal 4096 9f2c…e1 1761343512
seal 9012 41ab…07 1761343522
end 9530 c3d8…5a 1761343530
```

Each line gives the number of bytes sealed so far, the fingerprint and the time (in Unix seconds).

Because each seal depends on the one before, changing, removing or adding even one character anywhere in the log breaks that seal and every seal after it. Cutting the log short, or adding to it after it ended, shows too.

**The log index.** When a log ends, its last seal is also written to `log-index.chain` in the same folder. Each line there is chained to the line before it in the same way. So someone who edits a log and then rewrites its chain file to match is still caught: the log's last seal no longer agrees with the index.

You never need to open these files yourself. They are plain text, so you can, but leave them where they are and don't edit them.

## Checking a log

Session > Check a log for changes... asks for a log file and tells you what it found:

| Message | Meaning |
| --- | --- |
| *is intact: all N bytes match its N seals. The log index agrees.* | Nothing was changed. |
| *The first N bytes ... match its seals, but the log has not ended* | The log is still open, or FeroSSH stopped before it could close it. What is sealed is fine; the last few seconds are not sealed yet. |
| *FAILED the check: the log was changed between bytes A and B (seal N)* | Something in that part of the file is different from what was written. |
| *FAILED the check: the log was cut short* | Part of the end is missing. |
| *FAILED the check: N bytes were added after the log ended* | Text was appended later. |
| *WARNING: the log index has a different last seal for it* | The log and its chain were rewritten together. |
| *WARNING: the log index was changed at line N* | The index itself was edited. |
| *has no chain file, so it cannot be checked* | The log is from a FeroSSH older than sealed logs, or its `.chain` file was moved or deleted. |

The message ends with the log's last seal, which you can write down or send to someone as a receipt.

**What sealing can and cannot do.** Seals make changes visible; they cannot prevent them. Someone using your Windows account could rewrite a log, its chain and the index together from scratch. When a log may have to hold up later, for an audit or an incident, copy `log-index.chain` (or the last seal from the check) somewhere else, such as an email to yourself or a ticket. Then nobody can quietly rewrite the record on your PC.

## Where the files are

| What | Where |
| --- | --- |
| Logs, chain files and `log-index.chain` | `%LOCALAPPDATA%\FeroSSH\logs` |
| Scrollback per pane | In memory only: 10,000 lines, or `Scrollback` in [Settings](settings.md) |
