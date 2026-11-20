# Plan: managing many hosts

FeroSSH started as a client for a handful of servers. Managing dozens or hundreds of hosts needs a way to label them, find them, act on many at once, keep credentials under rules, and share one set of connections. This plan lists the next ten features for that, most useful first, what FeroSSH already has for each, and what is left to build.

Written 20 November 2026, against `main` at 5039c82.

Already built, so not listed: folders (`# folder:` comments), favourites, search over name, `user@host` and folder, imports from seven clients, password sources (ask, Windows Credential Manager, password-manager commands, the vault), the master-password vault synced through OneDrive, encrypted export and import, workspaces, scripts run on many picked hosts with a CSV report, broadcast input, and the known hosts manager.

| # | Feature | Status |
| --- | --- | --- |
| 1 | Several tags per connection | Done (#254) |
| 2 | Search by field, and saved searches | Done (#255) |
| 3 | Select several connections and act on them all | Done (#256) |
| 4 | Change one field on many connections at once | Built |
| 5 | Named credentials shared by many connections | To do |
| 6 | Credential and security rules by tag or folder | To do |
| 7 | Inventory columns with CSV export | To do |
| 8 | Run a script or broadcast to a tag or saved search | To do |
| 9 | A shared, read-only connection library for a team | To do |
| 10 | A machine-wide admin policy file | To do |

## 1. Several tags per connection (built)

**What FeroSSH had.** One ssh_config `Tag` per host, which picked a colour theme and worked with OpenSSH's `Match tagged`. The connection form could not set it, and the search did not look at it.

**What was built.** A **Tags** box in the connection form and a `Tags` keyword in the host's profile in `ferossh.conf`, so hosts from `~/.ssh/config` can be tagged without touching that file. The ssh_config `Tag` comes first, then the connection's own tags, each kept once. Tags show as `#prod #web` after the `user@host` detail and in the tooltip, the command palette and the script host picker, and the search finds hosts by tag. The theme rules match every tag, so tagging a connection `production` makes it red and turns on the Production confirmations.

## 2. Search by field, and saved searches (built)

**What FeroSSH has.** Plain words matched against the name, `user@host`, folder and (since item 1) tags.

**What was built.** Field words in the connection manager's search: `tag:prod`, `folder:db`, `name:web`, `user:root`, `host:10.0.`, `type:rdp`, `port:2222`, with `-` to leave out (`tag:prod -tag:legacy`), `*` and `?` in tags and types, and double quotes for spaces. Plain words work as before. The right-click menu saves the typed search as a smart folder (*Search: tag:prod*) at the top of the tree, closed at first, kept in `ferossh.conf` (`SavedSearch`); right-clicking it removes it. The script host picker and the command palette get the same language with item 8.

## 3. Select several connections and act on them all (built)

**What FeroSSH has.** The Connection manager and its right-click menu act on one connection at a time.

**What was built.** Ctrl+click, Shift+click, Ctrl+A and Ctrl+Space select several connections in the tree, highlighted until a plain click or Escape. The right-click menu on them offers: connect to all (a tab each, asking above eight), add a tag already in use, remove a connection's own tag, move to a folder of the saved connections or to no folder, delete (one confirmation listing them; Del too), and clear the selection. Moving and deleting change saved connections only; hosts from `~/.ssh/config` or imports are named and left alone. Still open for later items: typing a brand-new tag for many, opening them as one tiled tab, and running a script on them (item 8).

## 4. Change one field on many connections at once (built)

**What FeroSSH had.** Since item 3: tags, folders and deletes on a selection. Every other field meant opening each connection's form.

**What was built.** **Change a field** in the right-click menu on a selection. It covers:

- **Login fields** (`sessions.conf`): user, key file, jump hosts, proxy.
- **Profile fields** (`ferossh.conf`): password source, theme, font, font size, tmux session, start directory.

How it works:

- One box opens, starting with the value the connections share, or empty when they hold several. An empty box clears the field.
- The value is checked as the connection form checks it.
- **Preview** lists each connection's old and new value, and which connections are left alone and why. Nothing changes until you say yes.
- Login fields change saved connections only. Profile fields change every host.
- A connection is left alone when:
  - its type has no such field
  - a host would be its own jump host
  - it already has a proxy and you set jump hosts, or the other way round
  - the user has an `@` and it is not WinRM

Fields that need the whole form (host, port, forwards, login script) stay one connection at a time. A shared password source by name is item 5.

## 5. Named credentials

**What FeroSSH has.** Each connection stores its own user, key file and password source.

**What to build.** A credential is a named user plus a key file, agent or password source (Windows, vault, password-manager command). Connections point to it by name (`Credential ops-admin`), and changing it changes every connection that uses it. Kept in `ferossh.conf` and the vault; secrets stay where they are today.

## 6. Credential and security rules by tag or folder

**What FeroSSH has.** Separate per-host switches: the Production confirmation for scripts, old crypto per host, agent-forwarding confirmation.

**What to build.** Rules like theme rules, matched by tag, folder or host pattern: require a key, agent or security key (no password); never save a password; no agent forwarding; no clipboard; confirm before connecting; log the session. A rule can only tighten what a connection asks for.

## 7. Inventory columns with CSV export

**What FeroSSH has.** The status bar shows live CPU, memory and disk for the focused pane; script runs give a CSV report.

**What to build.** A list view in the connection manager with columns: last connected, host key type and fingerprint, and, when switched on per host, OS and kernel collected after login. Export the list as CSV.

## 8. Run a script or broadcast to a tag or saved search

**What FeroSSH has.** The script host picker with search, folder groups and tick boxes.

**What to build.** Pick by tag or saved search in one step, and send broadcast input to every open pane with a tag.

## 9. A shared, read-only connection library for a team

**What FeroSSH has.** ssh_config `Include` shows each included file as its own folder; vault sync and encrypted export serve one person.

**What to build.** Subscribe to a connection file on a network share or HTTPS, checked against a signature, read-only, refreshed on start. Local changes (favourites, tags, looks) sit on top and survive refreshes.

## 10. A machine-wide admin policy file

**What to build.** A policy file an administrator places (beside the exe or under `%ProgramData%`) that FeroSSH reads first and a user cannot override: no saved passwords, clipboard sharing off, old crypto blocked, allowed jump hosts, required host key checking.
