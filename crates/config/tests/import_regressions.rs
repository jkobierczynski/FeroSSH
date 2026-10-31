//! Inputs the `import_parse` fuzz target once failed on, run the way it
//! runs them. They are the files in `fuzz/regressions/import_parse`.

#![allow(clippy::unwrap_used)]

use ferossh_config::import::{self, Client};
use ferossh_config::{mobaxterm, mremoteng, putty, saved, securecrt, winscp};

/// What an import writes reads back as the same hosts.
fn round_trips(data: &[u8]) {
    let text = String::from_utf8_lossy(data);
    let mut sessions = winscp::from_ini(&text);
    sessions.extend(mobaxterm::from_ini(&text));
    sessions.extend(mremoteng::from_xml(&text).unwrap_or_default());
    sessions.extend(securecrt::session(&text, &text));
    sessions.extend(putty::file_session(&text, &text));
    if let Some((name, rest)) = text.split_once('\n') {
        sessions.extend(putty::file_session(name, rest));
    }
    import::distinct(&mut sessions);
    let all: Vec<&import::Session> = sessions.iter().collect();
    let back = saved::parse(&import::to_config(Client::WinScp, &all));
    let expected: Vec<&import::Session> = all
        .into_iter()
        .filter(|s| saved::valid_alias(&s.host.alias))
        .collect();
    assert_eq!(back.len(), expected.len());
    for (b, s) in back.iter().zip(expected) {
        assert_eq!(b.alias, s.host.alias);
        assert_eq!(b.hostname, s.host.hostname);
        assert_eq!(b.port, s.host.port);
        assert_eq!(b.forwards, s.host.forwards);
    }
}

#[test]
fn fuzz_regressions_round_trip() {
    let dir = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fuzz/regressions/import_parse"
    );
    let mut count = 0;
    for entry in std::fs::read_dir(dir).unwrap() {
        round_trips(&std::fs::read(entry.unwrap().path()).unwrap());
        count += 1;
    }
    assert!(count > 0);
}

/// A bracketed host with one colon (`[a:b]`) is not an IPv6 address, and
/// written out it would read back as `a:b` = host `a`, port `b`.
#[test]
fn bracketed_single_colon_host_is_refused() {
    assert!(putty::file_session("box", "HostName\\[Book:s_]\\\n").is_none());
    round_trips(b"HostName\\[Book:s_]\n");
    let s = putty::file_session("v6", "HostName\\[fe80::1]\\\n").unwrap();
    assert_eq!(s.host.hostname.as_deref(), Some("fe80::1"));
    round_trips(b"HostName\\[fe80::1]\n");
}
