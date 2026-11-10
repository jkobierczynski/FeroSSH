//! A small JSON reader for the settings files of other programs (Windows
//! Terminal's `settings.json`), which allow `//` and `/* */` comments and
//! trailing commas. It only reads; numbers are kept as text.

/// A JSON value.
#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    /// As written.
    Number(String),
    Text(String),
    List(Vec<Json>),
    /// Keys in the order written.
    Object(Vec<(String, Json)>),
}

/// How deep lists and objects may nest.
const MAX_DEPTH: usize = 64;

impl Json {
    /// The value at `key`, for an object.
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn text(&self) -> Option<&str> {
        match self {
            Json::Text(t) => Some(t),
            _ => None,
        }
    }

    pub fn list(&self) -> &[Json] {
        match self {
            Json::List(l) => l,
            _ => &[],
        }
    }
}

struct Reader<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
}

impl Reader<'_> {
    fn skip(&mut self) -> Result<(), String> {
        loop {
            match self.chars.peek() {
                Some(c) if c.is_whitespace() || *c == '\u{feff}' => {
                    self.chars.next();
                }
                Some('/') => {
                    self.chars.next();
                    match self.chars.next() {
                        Some('/') => {
                            for c in self.chars.by_ref() {
                                if c == '\n' {
                                    break;
                                }
                            }
                        }
                        Some('*') => {
                            let mut star = false;
                            loop {
                                match self.chars.next() {
                                    Some('/') if star => break,
                                    Some(c) => star = c == '*',
                                    None => return Err("a comment is not closed".into()),
                                }
                            }
                        }
                        _ => return Err("a stray /".into()),
                    }
                }
                _ => return Ok(()),
            }
        }
    }

    fn value(&mut self, depth: usize) -> Result<Json, String> {
        if depth > MAX_DEPTH {
            return Err("it nests too deep".into());
        }
        self.skip()?;
        match self.chars.peek().copied() {
            Some('{') => {
                self.chars.next();
                let mut fields = Vec::new();
                loop {
                    self.skip()?;
                    match self.chars.peek() {
                        Some('}') => {
                            self.chars.next();
                            return Ok(Json::Object(fields));
                        }
                        Some('"') => {
                            self.chars.next();
                            let key = self.string()?;
                            self.skip()?;
                            if self.chars.next() != Some(':') {
                                return Err("a : is missing".into());
                            }
                            let v = self.value(depth + 1)?;
                            fields.push((key, v));
                            self.skip()?;
                            match self.chars.next() {
                                Some(',') => {}
                                Some('}') => return Ok(Json::Object(fields)),
                                _ => return Err("a , or } is missing".into()),
                            }
                        }
                        _ => return Err("a key is missing".into()),
                    }
                }
            }
            Some('[') => {
                self.chars.next();
                let mut items = Vec::new();
                loop {
                    self.skip()?;
                    if self.chars.peek() == Some(&']') {
                        self.chars.next();
                        return Ok(Json::List(items));
                    }
                    items.push(self.value(depth + 1)?);
                    self.skip()?;
                    match self.chars.next() {
                        Some(',') => {}
                        Some(']') => return Ok(Json::List(items)),
                        _ => return Err("a , or ] is missing".into()),
                    }
                }
            }
            Some('"') => {
                self.chars.next();
                self.string().map(Json::Text)
            }
            Some(c) if c == '-' || c.is_ascii_digit() => {
                let mut n = String::new();
                while let Some(&c) = self.chars.peek() {
                    if c.is_ascii_digit() || "+-.eE".contains(c) {
                        n.push(c);
                        self.chars.next();
                    } else {
                        break;
                    }
                }
                Ok(Json::Number(n))
            }
            Some(c) if c.is_ascii_alphabetic() => {
                let mut word = String::new();
                while let Some(&c) = self.chars.peek() {
                    if !c.is_ascii_alphabetic() {
                        break;
                    }
                    word.push(c);
                    self.chars.next();
                }
                match word.as_str() {
                    "true" => Ok(Json::Bool(true)),
                    "false" => Ok(Json::Bool(false)),
                    "null" => Ok(Json::Null),
                    _ => Err(format!("{word} is not a value")),
                }
            }
            _ => Err("a value is missing".into()),
        }
    }

    /// The rest of a string whose `"` was read.
    fn string(&mut self) -> Result<String, String> {
        let mut out = String::new();
        loop {
            match self.chars.next() {
                None => return Err("a string is not closed".into()),
                Some('"') => return Ok(out),
                Some('\\') => match self.chars.next() {
                    Some('n') => out.push('\n'),
                    Some('r') => out.push('\r'),
                    Some('t') => out.push('\t'),
                    Some('b') => out.push('\u{8}'),
                    Some('f') => out.push('\u{c}'),
                    Some('u') => {
                        let unit = self.hex4()?;
                        let c = if (0xD800..0xDC00).contains(&unit) {
                            // A surrogate pair.
                            if self.chars.next() != Some('\\') || self.chars.next() != Some('u') {
                                return Err("half a surrogate pair".into());
                            }
                            let low = self.hex4()?;
                            if !(0xDC00..0xE000).contains(&low) {
                                return Err("half a surrogate pair".into());
                            }
                            char::from_u32(0x10000 + ((unit - 0xD800) << 10) + (low - 0xDC00))
                        } else {
                            char::from_u32(unit)
                        };
                        out.push(c.unwrap_or('\u{fffd}'));
                    }
                    Some(c) => out.push(c),
                    None => return Err("a string is not closed".into()),
                },
                Some(c) => out.push(c),
            }
        }
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let mut v = 0;
        for _ in 0..4 {
            let d = self
                .chars
                .next()
                .and_then(|c| c.to_digit(16))
                .ok_or("a \\u escape needs four hex digits")?;
            v = v * 16 + d;
        }
        Ok(v)
    }
}

/// Reads `text` as one JSON value (comments and trailing commas allowed).
pub fn parse(text: &str) -> Result<Json, String> {
    let mut r = Reader {
        chars: text.chars().peekable(),
    };
    let v = r.value(0)?;
    r.skip()?;
    if r.chars.next().is_some() {
        return Err("there is more after the value".into());
    }
    Ok(v)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn reads_json_with_comments() {
        let v = parse(
            "\u{feff}// top\n{ \"a\": [1, -2.5e3, true, null, ], /* x */ \"b\": \"q\\\"\\n\\u00e9\\ud83d\\ude00\", }",
        )
        .unwrap();
        assert_eq!(v.get("a").unwrap().list().len(), 4);
        assert_eq!(v.get("b").unwrap().text(), Some("q\"\né😀"));
        assert!(parse("{\"a\" 1}").is_err());
        assert!(parse("[1] x").is_err());
        assert!(parse(&"[".repeat(1000)).is_err());
        assert!(parse("\"abc").is_err());
        assert!(parse("\"\\ud800x\"").is_err());
    }
}
