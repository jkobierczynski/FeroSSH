//! The part of OpenSSH's `ssh_config` that milestone 3 needs to reach a
//! host: `Host` blocks with `HostName`, `User`, `Port` and `IdentityFile`.
//!
//! The rules follow `ssh_config(5)`: keywords are case-insensitive, the
//! first value found for a keyword wins (except `IdentityFile`, which
//! accumulates), and a `Host` line matches when any of its patterns matches
//! the name typed and no `!` pattern does. `Match` blocks are skipped and
//! `Include` is not followed yet; the session manager (milestone 4) brings
//! the full parser.

use crate::known_hosts::match_pattern_list;

/// Settings for one host after applying the config.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostConfig {
    pub hostname: Option<String>,
    pub user: Option<String>,
    pub port: Option<u16>,
    /// As written, before `~` and `%` expansion; see [`expand_path`].
    pub identity_files: Vec<String>,
}

/// Splits a config line into keyword and arguments, honouring `=` and
/// double quotes.
fn split_line(line: &str) -> Option<(String, Vec<String>)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let end = line
        .find(|c: char| c.is_ascii_whitespace() || c == '=')
        .unwrap_or(line.len());
    let (keyword, rest) = line.split_at(end);
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('=').unwrap_or(rest).trim_start();
    let mut args = Vec::new();
    let mut chars = rest.chars().peekable();
    while chars.peek().is_some() {
        while chars.next_if(char::is_ascii_whitespace).is_some() {}
        let mut arg = String::new();
        let mut quoted = false;
        let mut any = false;
        while let Some(&c) = chars.peek() {
            if c == '"' {
                quoted = !quoted;
                any = true;
                chars.next();
            } else if c.is_ascii_whitespace() && !quoted {
                break;
            } else {
                arg.push(c);
                any = true;
                chars.next();
            }
        }
        if any {
            args.push(arg);
        }
    }
    Some((keyword.to_ascii_lowercase(), args))
}

/// Space-separated `Host` patterns, each possibly negated.
fn host_line_matches(patterns: &[String], name: &str) -> bool {
    let mut matched = false;
    for p in patterns {
        if let Some(neg) = p.strip_prefix('!') {
            if match_pattern_list(neg, name) {
                return false;
            }
        } else if match_pattern_list(p, name) {
            matched = true;
        }
    }
    matched
}

/// Applies the config text to the name the user typed.
pub fn resolve(text: &str, name: &str) -> HostConfig {
    let mut cfg = HostConfig::default();
    // Lines before the first Host apply to every host.
    let mut active = true;
    for line in text.lines() {
        let Some((keyword, args)) = split_line(line) else {
            continue;
        };
        match keyword.as_str() {
            "host" => active = host_line_matches(&args, name),
            "match" => active = false,
            _ if !active => {}
            "hostname" => {
                if cfg.hostname.is_none() {
                    cfg.hostname = args.into_iter().next();
                }
            }
            "user" => {
                if cfg.user.is_none() {
                    cfg.user = args.into_iter().next();
                }
            }
            "port" => {
                if cfg.port.is_none() {
                    cfg.port = args
                        .first()
                        .and_then(|p| p.parse().ok())
                        .filter(|&p| p != 0);
                }
            }
            "identityfile" => cfg.identity_files.extend(args.into_iter().take(1)),
            _ => {}
        }
    }
    cfg
}

/// Expands `~` and the `%d %u %h %r %%` tokens in an `IdentityFile` path.
pub fn expand_path(
    path: &str,
    home: &str,
    local_user: &str,
    host: &str,
    remote_user: &str,
) -> String {
    let path = match path.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with(['/', '\\']) => format!("{home}{rest}"),
        _ => path.to_owned(),
    };
    let mut out = String::with_capacity(path.len());
    let mut chars = path.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('%') => out.push('%'),
            Some('d') => out.push_str(home),
            Some('u') => out.push_str(local_user),
            Some('h') => out.push_str(host),
            Some('r') => out.push_str(remote_user),
            Some(other) => {
                out.push('%');
                out.push(other);
            }
            None => out.push('%'),
        }
    }
    out
}

/// Parses `[user@]host[:port]` as typed on the command line or at the
/// connect prompt. IPv6 literals go in brackets: `[::1]:2222`.
pub fn parse_destination(text: &str) -> Option<(Option<String>, String, Option<u16>)> {
    let text = text.trim();
    let (user, rest) = match text.rsplit_once('@') {
        Some((u, r)) if !u.is_empty() => (Some(u.to_owned()), r),
        Some(_) => return None,
        None => (None, text),
    };
    let (host, port) = if let Some(v6) = rest.strip_prefix('[') {
        let (host, after) = v6.split_once(']')?;
        let port = match after.strip_prefix(':') {
            Some(p) => Some(p.parse().ok()?),
            None if after.is_empty() => None,
            None => return None,
        };
        (host, port)
    } else if rest.matches(':').count() == 1 {
        let (host, port) = rest.split_once(':')?;
        (host, Some(port.parse().ok()?))
    } else {
        (rest, None)
    };
    let valid = !host.is_empty()
        && !host.starts_with('-')
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':' | '%'));
    if !valid || port == Some(0) {
        return None;
    }
    Some((user, host.to_owned(), port))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    const CONFIG: &str = r#"
User global
# routers
Host core-* !core-lab
    HostName %h.net.example
    Port 2222
    IdentityFile ~/.ssh/net_ed25519

Host web
    hostname=10.1.2.3
    User "deploy"
    IdentityFile "C:\Users\me\keys\web key"

Match host other
    User nobody

Host *
    Port 22
    IdentityFile ~/.ssh/id_ed25519
"#;

    #[test]
    fn first_value_wins_and_identities_accumulate() {
        let c = resolve(CONFIG, "core-1");
        assert_eq!(c.hostname.as_deref(), Some("%h.net.example"));
        assert_eq!(c.user.as_deref(), Some("global"));
        assert_eq!(c.port, Some(2222));
        assert_eq!(
            c.identity_files,
            vec!["~/.ssh/net_ed25519", "~/.ssh/id_ed25519"]
        );
    }

    #[test]
    fn negation_quotes_and_equals() {
        let lab = resolve(CONFIG, "core-lab");
        assert_eq!(lab.port, Some(22));
        assert_eq!(lab.hostname, None);
        let web = resolve(CONFIG, "web");
        assert_eq!(web.hostname.as_deref(), Some("10.1.2.3"));
        assert_eq!(web.user.as_deref(), Some("global"));
        assert_eq!(web.identity_files[0], r"C:\Users\me\keys\web key");
    }

    #[test]
    fn match_blocks_are_skipped() {
        assert_eq!(resolve(CONFIG, "other").user.as_deref(), Some("global"));
    }

    #[test]
    fn expands_tokens() {
        assert_eq!(
            expand_path("~/.ssh/%r@%h_%u%%", r"C:\Users\me", "me", "srv", "root"),
            r"C:\Users\me/.ssh/root@srv_me%"
        );
        assert_eq!(expand_path("~user/x", "H", "u", "h", "r"), "~user/x");
    }

    #[test]
    fn destinations() {
        let d = |s| parse_destination(s);
        assert_eq!(d("host"), Some((None, "host".into(), None)));
        assert_eq!(
            d("me@host.example:2222"),
            Some((Some("me".into()), "host.example".into(), Some(2222)))
        );
        assert_eq!(
            d("a@b@host"),
            Some((Some("a@b".into()), "host".into(), None))
        );
        assert_eq!(d("[::1]:22"), Some((None, "::1".into(), Some(22))));
        assert_eq!(d("fe80::1"), Some((None, "fe80::1".into(), None)));
        assert_eq!(d("host:0"), None);
        assert_eq!(d("host:x"), None);
        assert_eq!(d("@host"), None);
        assert_eq!(d("ho st"), None);
        assert_eq!(d("-oProxyCommand=x"), None);
        assert_eq!(d("-host"), None);
    }
}
