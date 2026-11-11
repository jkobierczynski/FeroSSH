//! Just enough XML for WS-Management replies, parsed from untrusted
//! bytes in the sandboxed worker.
//!
//! Elements, attributes, text, CDATA, comments and processing
//! instructions; the five predefined entities and character references.
//! No DTD: a `<!DOCTYPE` is refused, so there are no entity definitions to
//! expand. Names are kept without their namespace prefix, which is all a
//! reader of a few known WinRS elements needs. Depth, element count and
//! input length are capped.

/// Elements nested deeper than this are refused.
const MAX_DEPTH: usize = 32;
/// Elements in one document; a WinRS reply holds a few dozen.
const MAX_ELEMENTS: usize = 20_000;

/// Why a document was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub &'static str);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "malformed XML ({})", self.0)
    }
}

/// One element, its name and attribute names without prefixes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Element {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<Node>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    Element(Element),
    Text(String),
}

impl Element {
    /// The value of attribute `name` (without prefix).
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    /// The child elements.
    pub fn elements(&self) -> impl Iterator<Item = &Element> {
        self.children.iter().filter_map(|n| match n {
            Node::Element(e) => Some(e),
            Node::Text(_) => None,
        })
    }

    /// The first child element called `name`.
    pub fn child(&self, name: &str) -> Option<&Element> {
        self.elements().find(|e| e.name == name)
    }

    /// The element at `path` of child names below this one.
    pub fn at(&self, path: &[&str]) -> Option<&Element> {
        path.iter().try_fold(self, |e, name| e.child(name))
    }

    /// Every element called `name` below this one, in document order.
    pub fn find_all(&self, name: &str) -> Vec<&Element> {
        let mut found = Vec::new();
        let mut stack: Vec<&Element> = vec![self];
        while let Some(e) = stack.pop() {
            if e.name == name {
                found.push(e);
            }
            let start = stack.len();
            stack.extend(e.elements());
            // Children in document order come off the stack first.
            if let Some(added) = stack.get_mut(start..) {
                added.reverse();
            }
        }
        found
    }

    /// The first element called `name` at or below this one.
    pub fn find(&self, name: &str) -> Option<&Element> {
        self.find_all(name).into_iter().next()
    }

    /// The element's own text (not its children's), trimmed.
    pub fn text(&self) -> String {
        let mut t = String::new();
        for n in &self.children {
            if let Node::Text(s) = n {
                t.push_str(s);
            }
        }
        t.trim().to_owned()
    }
}

/// Parses a whole document into its root element.
pub fn parse(input: &[u8]) -> Result<Element, Error> {
    let input = std::str::from_utf8(input).map_err(|_| Error("not UTF-8"))?;
    let input = input.strip_prefix('\u{feff}').unwrap_or(input);
    let mut p = Parser { rest: input };
    // Open elements; the root ends up alone at the bottom.
    let mut stack: Vec<Element> = Vec::new();
    let mut root: Option<Element> = None;
    let mut count = 0usize;
    loop {
        if p.rest.is_empty() {
            break;
        }
        if let Some(after) = p.rest.strip_prefix("<?") {
            let end = after.find("?>").ok_or(Error("unclosed <?"))?;
            p.rest = after.get(end + 2..).unwrap_or_default();
        } else if let Some(after) = p.rest.strip_prefix("<!--") {
            let end = after.find("-->").ok_or(Error("unclosed comment"))?;
            p.rest = after.get(end + 3..).unwrap_or_default();
        } else if let Some(after) = p.rest.strip_prefix("<![CDATA[") {
            let end = after.find("]]>").ok_or(Error("unclosed CDATA"))?;
            let text = after.get(..end).unwrap_or_default();
            let parent = stack.last_mut().ok_or(Error("text outside the root"))?;
            parent.children.push(Node::Text(text.to_owned()));
            p.rest = after.get(end + 3..).unwrap_or_default();
        } else if p.rest.starts_with("<!") {
            return Err(Error("DTDs are not allowed"));
        } else if let Some(after) = p.rest.strip_prefix("</") {
            p.rest = after;
            let name = p.name()?;
            p.skip_space();
            p.rest = p.rest.strip_prefix('>').ok_or(Error("bad end tag"))?;
            let done = stack.pop().ok_or(Error("end tag without start"))?;
            if done.name != local(name) {
                return Err(Error("mismatched end tag"));
            }
            match stack.last_mut() {
                Some(parent) => parent.children.push(Node::Element(done)),
                None => root = Some(done),
            }
        } else if let Some(after) = p.rest.strip_prefix('<') {
            if root.is_some() {
                return Err(Error("content after the root"));
            }
            count += 1;
            if count > MAX_ELEMENTS {
                return Err(Error("too many elements"));
            }
            if stack.len() >= MAX_DEPTH {
                return Err(Error("nested too deep"));
            }
            p.rest = after;
            let mut e = Element {
                name: local(p.name()?).to_owned(),
                ..Element::default()
            };
            let empty = loop {
                p.skip_space();
                if let Some(r) = p.rest.strip_prefix("/>") {
                    p.rest = r;
                    break true;
                }
                if let Some(r) = p.rest.strip_prefix('>') {
                    p.rest = r;
                    break false;
                }
                let name = p.name()?;
                p.skip_space();
                p.rest = p
                    .rest
                    .strip_prefix('=')
                    .ok_or(Error("attribute without value"))?;
                p.skip_space();
                let value = p.quoted()?;
                let is_ns = name == "xmlns" || name.starts_with("xmlns:");
                if !is_ns {
                    e.attrs.push((local(name).to_owned(), value));
                }
            };
            if empty {
                match stack.last_mut() {
                    Some(parent) => parent.children.push(Node::Element(e)),
                    None => root = Some(e),
                }
            } else {
                stack.push(e);
            }
        } else {
            let end = p.rest.find('<').unwrap_or(p.rest.len());
            let raw = p.rest.get(..end).unwrap_or_default();
            p.rest = p.rest.get(end..).unwrap_or_default();
            match stack.last_mut() {
                Some(parent) => parent.children.push(Node::Text(unescape(raw)?)),
                None if raw.trim().is_empty() => {}
                None => return Err(Error("text outside the root")),
            }
        }
    }
    if !stack.is_empty() {
        return Err(Error("unclosed element"));
    }
    root.ok_or(Error("no root element"))
}

/// A name without its namespace prefix.
fn local(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

struct Parser<'a> {
    rest: &'a str,
}

impl<'a> Parser<'a> {
    fn skip_space(&mut self) {
        self.rest = self.rest.trim_start_matches([' ', '\t', '\r', '\n']);
    }

    fn name(&mut self) -> Result<&'a str, Error> {
        let end = self
            .rest
            .find(|c: char| {
                c.is_ascii_whitespace() || matches!(c, '/' | '>' | '=' | '<' | '"' | '\'')
            })
            .unwrap_or(self.rest.len());
        if end == 0 {
            return Err(Error("missing name"));
        }
        let (name, rest) = self.rest.split_at(end);
        self.rest = rest;
        Ok(name)
    }

    fn quoted(&mut self) -> Result<String, Error> {
        let quote = self.rest.chars().next().ok_or(Error("missing value"))?;
        if quote != '"' && quote != '\'' {
            return Err(Error("unquoted value"));
        }
        let body = self.rest.get(1..).unwrap_or_default();
        let end = body.find(quote).ok_or(Error("unclosed value"))?;
        let raw = body.get(..end).unwrap_or_default();
        if raw.contains('<') {
            return Err(Error("< in a value"));
        }
        self.rest = body.get(end + 1..).unwrap_or_default();
        unescape(raw)
    }
}

/// Text with its entity and character references replaced.
pub fn unescape(raw: &str) -> Result<String, Error> {
    if !raw.contains('&') {
        return Ok(raw.to_owned());
    }
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(i) = rest.find('&') {
        out.push_str(rest.get(..i).unwrap_or_default());
        let after = rest.get(i + 1..).unwrap_or_default();
        let end = after.find(';').ok_or(Error("unterminated reference"))?;
        let name = after.get(..end).unwrap_or_default();
        let c = match name {
            "lt" => '<',
            "gt" => '>',
            "amp" => '&',
            "quot" => '"',
            "apos" => '\'',
            _ => {
                let code = if let Some(hex) = name.strip_prefix("#x") {
                    u32::from_str_radix(hex, 16).ok()
                } else if let Some(dec) = name.strip_prefix('#') {
                    dec.parse::<u32>().ok()
                } else {
                    None
                };
                code.and_then(char::from_u32)
                    .ok_or(Error("unknown reference"))?
            }
        };
        out.push(c);
        rest = after.get(end + 1..).unwrap_or_default();
    }
    out.push_str(rest);
    Ok(out)
}

/// `text` escaped for an element's content or a quoted attribute.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn reads_elements_attributes_and_text() {
        let doc = br#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://www.w3.org/2003/05/soap-envelope" xmlns:rsp="x">
  <!-- a comment -->
  <s:Body>
    <rsp:ReceiveResponse>
      <rsp:Stream Name="stdout" CommandId='C1'>aGk=</rsp:Stream>
      <rsp:Stream Name="stderr" CommandId="C1" End="true"/>
      <rsp:Note>a &lt;b&gt; &amp; &#65;&#x42; <![CDATA[<raw>]]></rsp:Note>
    </rsp:ReceiveResponse>
  </s:Body>
</s:Envelope>"#;
        let root = parse(doc).unwrap();
        assert_eq!(root.name, "Envelope");
        let streams = root.find_all("Stream");
        assert_eq!(streams.len(), 2);
        assert_eq!(streams[0].attr("Name"), Some("stdout"));
        assert_eq!(streams[0].attr("CommandId"), Some("C1"));
        assert_eq!(streams[0].text(), "aGk=");
        assert_eq!(streams[1].attr("End"), Some("true"));
        assert_eq!(root.find("Note").unwrap().text(), "a <b> & AB <raw>");
        assert!(root.at(&["Body", "ReceiveResponse"]).is_some());
    }

    #[test]
    fn refuses_what_it_does_not_read() {
        for bad in [
            &b"<!DOCTYPE x [<!ENTITY a \"b\">]><x>&a;</x>"[..],
            b"<a><b></a></b>",
            b"<a>",
            b"<a/><b/>",
            b"<a>&bogus;</a>",
            b"<a>&#xD800;</a>",
            b"<a x=1/>",
            b"<a x=\"<\"/>",
            b"text",
            b"",
            b"\xff",
        ] {
            assert!(parse(bad).is_err(), "{:?}", String::from_utf8_lossy(bad));
        }
    }

    #[test]
    fn caps_the_depth() {
        let deep = "<a>".repeat(MAX_DEPTH + 1) + &"</a>".repeat(MAX_DEPTH + 1);
        assert!(parse(deep.as_bytes()).is_err());
        let ok = "<a>".repeat(MAX_DEPTH) + &"</a>".repeat(MAX_DEPTH);
        assert!(parse(ok.as_bytes()).is_ok());
    }

    #[test]
    fn escapes_round_trip() {
        let s = "a<b>&\"c'";
        assert_eq!(unescape(&escape(s)).unwrap(), s);
    }
}
