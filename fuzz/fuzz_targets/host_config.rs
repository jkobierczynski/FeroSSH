//! Feeds arbitrary `ssh_config` and `known_hosts` text to their parsers,
//! with an Include that serves the same text again (a config including
//! itself; the unit test `self_including_configs_stay_small` covers the
//! exponential case), and checks arbitrary host keys against known_hosts.
//! The first line also goes to the `sftp://` address parser. None may
//! panic or blow up, a key known_hosts trusts is never also revoked, and
//! an `sftp://` address yields a destination that parses and an absolute
//! folder without control characters.

#![no_main]

use ferossh_config::hostkey;
use ferossh_config::known_hosts::{KnownHosts, Status};
use ferossh_config::ssh_config::{self, Includes};
use libfuzzer_sys::fuzz_target;

struct Again(String);

impl Includes for Again {
    fn read(&self, _: &str) -> Vec<(String, String)> {
        vec![("a".into(), self.0.clone())]
    }
}

fuzz_target!(|data: &[u8]| {
    let Some(at) = data.iter().position(|&b| b == 0) else {
        return;
    };
    let (text, key) = data.split_at(at);
    let key = key.get(1..).unwrap_or_default();
    let text = String::from_utf8_lossy(text);
    let host = text.lines().next().unwrap_or("h").trim();

    let again = Again(text.clone().into_owned());
    let cfg = ssh_config::resolve_in(&text, host, &again);
    let _ = cfg.active_forwards();
    let _ = ssh_config::hosts(&text, &again);
    let _ = ssh_config::parse_destination(host);
    if let Some((destination, folder)) = ssh_config::sftp_url(host) {
        assert!(ssh_config::parse_destination(&destination).is_some());
        if let Some(folder) = folder {
            assert!(folder.starts_with('/') && !folder.chars().any(char::is_control));
        }
    }

    let kh = KnownHosts::parse(&text);
    let port = u16::from(key.first().copied().unwrap_or(22));
    let status = kh.check(host, port, key);
    if status == Status::Trusted {
        assert!(!kh.is_revoked(key));
    }
    let _ = kh.key_types(host, port);
    let _ = kh.is_authority_for(host, port, key);
    let _ = hostkey::describe(key);
    let _ = hostkey::describe_certificate(key);
});
