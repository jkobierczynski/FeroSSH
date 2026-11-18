//! Port-forward previews: a pane's output is watched for the address of
//! a web server on its host (`http://localhost:5173/`, as development
//! servers print it), and a small bar offers to forward that port and
//! open it in the browser here. This module holds the parts that need no
//! window: finding the addresses in the output, the address to open, and
//! the bar's cells.

use ferossh_render::{Cell, Color, Flags, Grid, Rgb};
use unicode_width::UnicodeWidthChar;

use crate::tabstrip::Palette;

/// The longest run of text without a space that is kept while waiting
/// for the rest of an address; anything longer is not one.
const MAX_WORD: usize = 2048;
/// The longest path kept from an address.
const MAX_PATH: usize = 1024;

/// A web server's address on the host, as the host printed it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Url {
    pub https: bool,
    /// `localhost`, `127.0.0.1`, `0.0.0.0`, `::1` or `::` (no brackets).
    pub host: String,
    pub port: u16,
    /// What follows the port: empty or starting with `/`, `?` or `#`.
    pub path: String,
}

impl Url {
    /// Where the host connects to reach the server: an address that
    /// listens on every interface is reached on loopback.
    pub fn target(&self) -> &str {
        match self.host.as_str() {
            "0.0.0.0" => "127.0.0.1",
            "::" => "::1",
            h => h,
        }
    }

    /// The address to open here once the forward listens at `addr`
    /// (`127.0.0.1:port`, `[::1]:port`): the same scheme and path. A
    /// listener on 127.0.0.1 is opened as `localhost` where the host
    /// printed that name (cookies and sign-in callbacks are often set for
    /// it), and one on every interface on loopback; any other address is
    /// opened as it is.
    pub fn local(&self, addr: &str) -> Option<String> {
        use std::net::{IpAddr, Ipv4Addr, SocketAddr};
        let at: SocketAddr = addr.trim().parse().ok()?;
        let port = at.port();
        let named = self.host == "localhost";
        let host = match at.ip() {
            IpAddr::V4(ip) if ip == Ipv4Addr::LOCALHOST || ip.is_unspecified() => {
                if named { "localhost" } else { "127.0.0.1" }.to_owned()
            }
            IpAddr::V4(ip) => ip.to_string(),
            IpAddr::V6(ip) if ip.is_unspecified() => "[::1]".to_owned(),
            IpAddr::V6(ip) => format!("[{ip}]"),
        };
        let scheme = if self.https { "https" } else { "http" };
        Some(format!("{scheme}://{host}:{port}{}", self.path))
    }

    /// How the bar names the address: `localhost:5173`.
    pub fn shown(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

/// Characters an address's path may hold (RFC 3986's unreserved and
/// reserved characters and `%`); the path ends at any other.
fn path_char(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || matches!(
            c,
            '-' | '.'
                | '_'
                | '~'
                | ':'
                | '/'
                | '?'
                | '#'
                | '['
                | ']'
                | '@'
                | '!'
                | '$'
                | '&'
                | '\''
                | '('
                | ')'
                | '*'
                | '+'
                | ','
                | ';'
                | '='
                | '%'
        )
}

/// The address in `word` (text without spaces), if it holds one of a
/// server on the host itself.
pub fn parse(word: &str) -> Option<Url> {
    let lower = word.to_ascii_lowercase();
    let (at, https) = match (lower.find("http://"), lower.find("https://")) {
        (Some(a), Some(b)) if b < a => (b + 8, true),
        (Some(a), _) => (a + 7, false),
        (None, Some(b)) => (b + 8, true),
        (None, None) => return None,
    };
    // The scheme starts the word or follows something that is not part
    // of a name (`Local:http://…`, `(http://…`), so `xhttp://` is not one.
    let before = lower.get(..at - if https { 8 } else { 7 })?;
    if before
        .chars()
        .next_back()
        .is_some_and(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.')
    {
        return None;
    }
    let rest = word.get(at..)?;
    let lower_rest = lower.get(at..)?;
    let (host, after) = ["localhost", "127.0.0.1", "0.0.0.0", "[::1]", "[::]"]
        .iter()
        .find_map(|h| {
            lower_rest
                .strip_prefix(h)
                .map(|_| (h.trim_matches(['[', ']']).to_owned(), rest.get(h.len()..)))
        })?;
    let after = after?.strip_prefix(':')?;
    let digits = after.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 || digits > 5 {
        return None;
    }
    let port = after
        .get(..digits)?
        .parse::<u16>()
        .ok()
        .filter(|&p| p != 0)?;
    let tail = after.get(digits..)?;
    // `localhost:80800`, `localhost:3000x`: not a port.
    if tail.starts_with(|c: char| c.is_ascii_alphanumeric()) {
        return None;
    }
    let mut path: String = if tail.starts_with(['/', '?', '#']) {
        tail.chars().take_while(|&c| path_char(c)).collect()
    } else {
        String::new()
    };
    // Punctuation that ends a sentence is not part of it, nor a bracket
    // or quote closing one around the address: a closing bracket only
    // when the path opens none, a quote only when one came before the
    // address.
    let quoted = before.ends_with(['\'', '"']);
    while let Some(last) = path.chars().next_back() {
        let strip = match last {
            '.' | ',' | ';' | ':' | '!' | '?' => true,
            ')' => path.matches('(').count() < path.matches(')').count(),
            ']' => path.matches('[').count() < path.matches(']').count(),
            '\'' => quoted,
            _ => false,
        };
        if !strip {
            break;
        }
        path.pop();
    }
    if path.len() > MAX_PATH {
        path = String::from("/");
    }
    Some(Url {
        https,
        host,
        port,
        path,
    })
}

/// Where the scanner is in an escape sequence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
enum Esc {
    #[default]
    None,
    /// After ESC.
    Start,
    /// In a control sequence (ESC [), up to its final byte.
    Csi,
    /// In a string (OSC, DCS, APC, PM, SOS), up to BEL or ESC \.
    Text,
    /// ESC inside a string: `\` ends it.
    TextEnd,
}

/// Finds web server addresses in a pane's output as it comes, in pieces
/// cut anywhere, through colours and other escape sequences
/// (development servers print the port in bold).
#[derive(Debug, Default)]
pub struct Scanner {
    esc: Esc,
    /// The text since the last space, waiting for its end.
    word: String,
    /// The word grew past [`MAX_WORD`]: skipped to the next space.
    skipping: bool,
    /// A UTF-8 character cut between pieces.
    partial: Vec<u8>,
}

impl Scanner {
    /// The addresses that ended in `bytes`, in order.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Url> {
        let mut found = Vec::new();
        for &b in bytes {
            match self.esc {
                Esc::Start => {
                    self.esc = match b {
                        b'[' => Esc::Csi,
                        b']' | b'P' | b'_' | b'^' | b'X' => Esc::Text,
                        _ => Esc::None,
                    };
                    if self.esc != Esc::Csi {
                        // Anything but colours may move the cursor: a
                        // word ends there.
                        self.end(&mut found);
                    }
                    continue;
                }
                Esc::Csi => {
                    if (0x40..=0x7e).contains(&b) {
                        self.esc = Esc::None;
                        // Colours and styles (SGR) do not split a word;
                        // anything else, such as a cursor move, does.
                        if b != b'm' {
                            self.end(&mut found);
                        }
                    }
                    continue;
                }
                Esc::Text => {
                    match b {
                        0x07 => self.esc = Esc::None,
                        0x1b => self.esc = Esc::TextEnd,
                        _ => {}
                    }
                    continue;
                }
                Esc::TextEnd => {
                    self.esc = if b == b'\\' { Esc::None } else { Esc::Text };
                    continue;
                }
                Esc::None => {}
            }
            if b == 0x1b {
                self.partial.clear();
                self.esc = Esc::Start;
                continue;
            }
            if b < 0x20 || b == 0x7f || b == b' ' {
                self.partial.clear();
                self.end(&mut found);
                continue;
            }
            if b < 0x80 {
                self.partial.clear();
                self.push(char::from(b), &mut found);
                continue;
            }
            self.partial.push(b);
            match std::str::from_utf8(&self.partial) {
                Ok(s) => {
                    let c = s.chars().next();
                    self.partial.clear();
                    if let Some(c) = c {
                        self.push(c, &mut found);
                    }
                }
                Err(e) if e.error_len().is_some() || self.partial.len() >= 4 => {
                    // Not UTF-8: a separator.
                    self.partial.clear();
                    self.end(&mut found);
                }
                Err(_) => {}
            }
        }
        found
    }

    fn push(&mut self, c: char, found: &mut Vec<Url>) {
        if c.is_whitespace() {
            self.end(found);
            return;
        }
        if self.skipping {
            return;
        }
        if self.word.len() >= MAX_WORD {
            self.word.clear();
            self.skipping = true;
            return;
        }
        self.word.push(c);
    }

    fn end(&mut self, found: &mut Vec<Url>) {
        self.skipping = false;
        if self.word.is_empty() {
            return;
        }
        if let Some(url) = parse(&self.word) {
            found.push(url);
        }
        self.word.clear();
    }
}

/// What a click on the bar does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Open,
    Dismiss,
}

/// The bar's text: `(offer, button, close)`.
fn parts(url: &Url, host: &str) -> (String, &'static str, &'static str) {
    (
        format!(" {} is listening on {host}. ", url.shown()),
        " Forward and open ",
        " ✕ ",
    )
}

/// The bar's width in cells for `url` on `host`, at most `max`.
pub fn width(url: &Url, host: &str, max: usize) -> usize {
    let (a, b, c) = parts(url, host);
    let cells: usize = [a.as_str(), b, c]
        .iter()
        .flat_map(|s| s.chars())
        .map(|c| c.width().unwrap_or(0))
        .sum();
    cells.min(max)
}

/// What is at column `col` of a bar of `cols` cells: the close button
/// at its end, the rest opens.
pub fn hit(col: usize, cols: usize) -> Hit {
    if col + 3 >= cols {
        Hit::Dismiss
    } else {
        Hit::Open
    }
}

/// Draws the bar into `grid`'s first row: the offer cut to fit, then
/// the button and the close button, which always show.
pub fn draw(grid: &mut Grid, url: &Url, host: &str, palette: &Palette) {
    let cols = grid.cols();
    let (offer, button, close) = parts(url, host);
    let cell = |c: char, fg: Rgb, bg: Rgb, flags: Flags| {
        Cell::new(c, Color::Rgb(fg), Color::Rgb(bg), flags)
    };
    let fixed = button.chars().count() + close.chars().count();
    let room = cols.saturating_sub(fixed);
    let mut cells: Vec<Cell> = Vec::with_capacity(cols);
    let shown = crate::tabstrip::fit(&offer, room);
    for c in shown.chars() {
        let w = c.width().unwrap_or(0);
        if w != 1 || cells.len() >= room {
            continue;
        }
        cells.push(cell(c, palette.text, palette.strip, Flags::empty()));
    }
    while cells.len() < room {
        cells.push(cell(' ', palette.text, palette.strip, Flags::empty()));
    }
    for c in button.chars() {
        cells.push(cell(c, palette.strip, palette.accent, Flags::BOLD));
    }
    for c in close.chars() {
        cells.push(cell(c, palette.dim, palette.strip, Flags::empty()));
    }
    cells.truncate(cols);
    grid.set_row(0, cells);
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn url(host: &str, port: u16, path: &str) -> Url {
        Url {
            https: false,
            host: host.into(),
            port,
            path: path.into(),
        }
    }

    #[test]
    fn addresses_of_servers_on_the_host_are_found() {
        assert_eq!(
            parse("http://localhost:5173/"),
            Some(url("localhost", 5173, "/"))
        );
        assert_eq!(
            parse("http://127.0.0.1:8000"),
            Some(url("127.0.0.1", 8000, ""))
        );
        assert_eq!(
            parse("http://LOCALHOST:8888/tree?token=ab12"),
            Some(url("localhost", 8888, "/tree?token=ab12"))
        );
        assert_eq!(
            parse("http://0.0.0.0:3000/"),
            Some(url("0.0.0.0", 3000, "/"))
        );
        assert_eq!(parse("http://[::1]:4000/x"), Some(url("::1", 4000, "/x")));
        assert_eq!(parse("http://[::]:4000"), Some(url("::", 4000, "")));
        let tls = parse("https://localhost:8443/").unwrap();
        assert!(tls.https);
        // Glued to a label, in brackets, at the end of a sentence.
        assert_eq!(
            parse("Local:http://localhost:5173/"),
            Some(url("localhost", 5173, "/"))
        );
        assert_eq!(
            parse("(http://localhost:3000)."),
            Some(url("localhost", 3000, ""))
        );
        assert_eq!(
            parse("<http://localhost:3000/app>"),
            Some(url("localhost", 3000, "/app"))
        );
        assert_eq!(
            parse("'http://localhost:3000/'"),
            Some(url("localhost", 3000, "/"))
        );
        // Brackets and quotes that belong to the path stay.
        assert_eq!(
            parse("http://localhost:3000/wiki/A_(b)"),
            Some(url("localhost", 3000, "/wiki/A_(b)"))
        );
        assert_eq!(
            parse("http://localhost:3000/it's"),
            Some(url("localhost", 3000, "/it's"))
        );
    }

    #[test]
    fn other_addresses_are_not() {
        for word in [
            "http://example.com:3000/",
            "http://localhost/",
            "http://localhost:/",
            "http://localhost:0/",
            "http://localhost:65536/",
            "http://localhost:3000x",
            "http://localhost.evil.com:80/",
            "http://127.0.0.10:80/",
            "postgres://localhost:5432/db",
            "xhttp://localhost:80/",
            "localhost:3000",
            "8080:localhost:5432",
        ] {
            assert_eq!(parse(word), None, "{word}");
        }
    }

    #[test]
    fn the_local_address_keeps_the_scheme_path_and_name() {
        let u = parse("http://localhost:8888/lab?token=x").unwrap();
        assert_eq!(
            u.local("127.0.0.1:18888").as_deref(),
            Some("http://localhost:18888/lab?token=x")
        );
        // An existing forward listening elsewhere is opened there.
        assert_eq!(
            u.local("[::1]:18000").as_deref(),
            Some("http://[::1]:18000/lab?token=x")
        );
        assert_eq!(
            u.local("127.0.0.2:18000").as_deref(),
            Some("http://127.0.0.2:18000/lab?token=x")
        );
        assert_eq!(
            u.local("0.0.0.0:18000").as_deref(),
            Some("http://localhost:18000/lab?token=x")
        );
        assert_eq!(
            parse("http://127.0.0.1:80/")
                .unwrap()
                .local("[::]:81")
                .as_deref(),
            Some("http://[::1]:81/")
        );
        assert_eq!(u.local("server port 80"), None);
        let u = parse("https://0.0.0.0:3000").unwrap();
        assert_eq!(u.target(), "127.0.0.1");
        assert_eq!(
            u.local("127.0.0.1:3000").as_deref(),
            Some("https://127.0.0.1:3000")
        );
        assert_eq!(parse("http://[::]:80/").unwrap().target(), "::1");
        assert_eq!(parse("http://[::1]:80/").unwrap().shown(), "[::1]:80");
    }

    #[test]
    fn the_scanner_reads_through_colours_and_pieces() {
        // As Vite prints it: the port in bold, the line in colour.
        let vite = b"  \x1b[32m\xe2\x9e\x9c\x1b[39m  \x1b[1mLocal\x1b[22m:   \
            \x1b[36mhttp://localhost:\x1b[1m5173\x1b[22m/\x1b[39m\r\n";
        let mut s = Scanner::default();
        assert_eq!(s.feed(vite), vec![url("localhost", 5173, "/")]);
        // The same, one byte at a time.
        let mut s = Scanner::default();
        let found: Vec<Url> = vite.iter().flat_map(|b| s.feed(&[*b])).collect();
        assert_eq!(found, vec![url("localhost", 5173, "/")]);
        // Not until it ends: more digits could follow.
        let mut s = Scanner::default();
        assert!(s.feed(b"at http://127.0.0.1:80").is_empty());
        assert_eq!(s.feed(b"00/ ok"), vec![url("127.0.0.1", 8000, "/")]);
    }

    #[test]
    fn the_scanner_skips_links_titles_and_cursor_moves() {
        let mut s = Scanner::default();
        // An OSC 8 link's target and a window title are not output.
        let found = s.feed(
            b"\x1b]8;;http://localhost:1111/\x1b\\docs\x1b]8;;\x1b\\ \
              \x1b]0;http://localhost:2222/\x07\n",
        );
        assert!(found.is_empty());
        // A cursor move splits words.
        assert!(s.feed(b"http://local\x1b[3Chost:3000/\n").is_empty());
        // A word too long to be an address is dropped, and the next one
        // is still found.
        let long = vec![b'a'; MAX_WORD + 10];
        assert!(s.feed(&long).is_empty());
        assert_eq!(
            s.feed(b" http://localhost:3000/\n"),
            vec![url("localhost", 3000, "/")]
        );
    }

    #[test]
    fn the_bar_keeps_its_buttons_when_narrow() {
        let u = url("localhost", 5173, "/");
        let wide = width(&u, "build-box", 200);
        assert!(wide > 40);
        assert_eq!(width(&u, "build-box", 30), 30);
        let mut grid = Grid::new(30, 1);
        draw(
            &mut grid,
            &u,
            "build-box",
            &Palette::from_colours(
                Rgb::new(0, 0, 0),
                Rgb::new(255, 255, 255),
                Rgb::new(0, 128, 255),
            ),
        );
        let text: String = grid.row(0).unwrap().iter().map(|c| c.ch).collect();
        assert!(text.ends_with(" Forward and open  ✕ "), "{text}");
        assert_eq!(hit(29, 30), Hit::Dismiss);
        assert_eq!(hit(27, 30), Hit::Dismiss);
        assert_eq!(hit(26, 30), Hit::Open);
        assert_eq!(hit(0, 30), Hit::Open);
    }
}
