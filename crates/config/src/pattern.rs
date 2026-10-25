//! OpenSSH host patterns, as `Host` lines and `Match host` use them.

/// Matches one pattern against `text`: `*` is any run of characters, `?`
/// one character, everything else literal and ASCII case-insensitive.
pub fn match_pattern(text: &str, pattern: &str) -> bool {
    let text: Vec<char> = text.chars().collect();
    let pattern: Vec<char> = pattern.chars().collect();
    // Iterative wildcard match: on a mismatch, let the last `*` swallow one
    // more character. Linear in practice, never exponential.
    let (mut t, mut p) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        match pattern.get(p) {
            Some('*') => {
                star = Some((p, t));
                p += 1;
            }
            Some(&c) if c == '?' || text.get(t).is_some_and(|x| x.eq_ignore_ascii_case(&c)) => {
                t += 1;
                p += 1;
            }
            _ => match star {
                Some((sp, st)) => {
                    p = sp + 1;
                    t = st + 1;
                    star = Some((sp, st + 1));
                }
                None => return false,
            },
        }
    }
    pattern
        .get(p..)
        .is_some_and(|rest| rest.iter().all(|&c| c == '*'))
}

/// Matches a host against a pattern list as a `Host` line does: patterns
/// are separated by whitespace or commas, a `!pattern` that matches rejects
/// the host outright, and otherwise any plain pattern that matches accepts it.
pub fn match_host<'a>(host: &str, patterns: impl IntoIterator<Item = &'a str>) -> bool {
    let mut matched = false;
    for pattern in patterns
        .into_iter()
        .flat_map(|p| p.split([',', ' ', '\t']))
        .filter(|p| !p.is_empty())
    {
        match pattern.strip_prefix('!') {
            Some(negated) if match_pattern(host, negated) => return false,
            Some(_) => {}
            None => matched |= match_pattern(host, pattern),
        }
    }
    matched
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcards() {
        assert!(match_pattern("db1.prod.example.com", "*.prod.example.com"));
        assert!(match_pattern("DB1.Prod.Example.com", "*.prod.example.com"));
        assert!(!match_pattern("prod.example.com", "*.prod.example.com"));
        assert!(match_pattern("web-03", "web-0?"));
        assert!(!match_pattern("web-3", "web-0?"));
        assert!(match_pattern("anything", "*"));
        assert!(match_pattern("", "*"));
        assert!(!match_pattern("", "?"));
        assert!(match_pattern("abcbcd", "a*bcd"));
        assert!(match_pattern("a*b", "a*b"));
    }

    #[test]
    fn no_exponential_backtracking() {
        let text = "a".repeat(10_000);
        assert!(!match_pattern(&text, &format!("{}b", "*a".repeat(50))));
    }

    #[test]
    fn negation_wins() {
        let list = ["prod-* !prod-sandbox", "db-*,!db-test"];
        assert!(match_host("prod-web", list));
        assert!(!match_host("prod-sandbox", list));
        assert!(match_host("db-main", list));
        assert!(!match_host("db-test", list));
        assert!(!match_host("dev-web", list));
        // A list of only negations never matches, as in OpenSSH.
        assert!(!match_host("anything", ["!other"]));
    }
}
