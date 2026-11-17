//! Mark-of-the-Web for downloaded files.
//!
//! Windows treats a file with a `Zone.Identifier` stream saying
//! `ZoneId=3` (Internet) as downloaded: SmartScreen checks a program
//! before it runs, Office opens documents in Protected View, and
//! Explorer shows *Unblock*. Browsers write it on every download; files
//! fetched over SFTP or ZMODEM are just as much from elsewhere, so they
//! get it too. Where the stream cannot be written (a FAT drive, some
//! network shares) the file stays as it is: the mark is a help, not a
//! gate.

use std::path::Path;

/// The stream's text for a file from `host_url`.
pub fn zone_identifier(host_url: &str) -> String {
    let url: String = host_url
        .chars()
        .filter(|c| !c.is_control())
        .take(2048)
        .collect();
    format!("[ZoneTransfer]\r\nZoneId=3\r\nHostUrl={url}\r\n")
}

/// `sftp://host[:port]/path` for a file at `remote` on the server named
/// by a queue label `user@host:port` (the user is left out, as browsers
/// leave out credentials).
pub fn sftp_url(label: &str, remote: &[u8]) -> String {
    let host = label.rsplit_once('@').map_or(label, |(_, h)| h);
    let host = host.strip_suffix(":22").unwrap_or(host);
    let path = String::from_utf8_lossy(remote);
    let mut url = format!("sftp://{host}");
    if !path.starts_with('/') {
        url.push('/');
    }
    for c in path.chars() {
        match c {
            ' ' => url.push_str("%20"),
            '#' => url.push_str("%23"),
            '%' => url.push_str("%25"),
            '?' => url.push_str("%3F"),
            c => url.push(c),
        }
    }
    url
}

/// Marks `path` as downloaded from `host_url`. Windows only; elsewhere
/// there is no such stream and nothing happens.
pub fn mark(path: &Path, host_url: &str) {
    #[cfg(windows)]
    {
        let mut stream = path.as_os_str().to_owned();
        stream.push(":Zone.Identifier");
        let _ = std::fs::write(stream, zone_identifier(host_url));
    }
    #[cfg(not(windows))]
    let _ = (path, host_url);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_the_internet_zone_with_the_source() {
        let z = zone_identifier("sftp://srv/home/me/a b.exe\r\nZoneId=0");
        assert!(z.starts_with("[ZoneTransfer]\r\nZoneId=3\r\nHostUrl=sftp://srv/"));
        // No line of its own can be smuggled in by a file name.
        assert_eq!(z.matches("ZoneId=").count(), 2);
        assert_eq!(z.lines().count(), 3);
        assert_eq!(
            sftp_url("me@srv.lan:22", b"/tmp/a b#1.exe"),
            "sftp://srv.lan/tmp/a%20b%231.exe"
        );
        assert_eq!(sftp_url("srv:2222", b"x"), "sftp://srv:2222/x");
    }
}
