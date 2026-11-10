//! Feeds arbitrary text to the parsers for other clients' saved sessions
//! (`WinSCP.ini`, `MobaXterm.ini`, mRemoteNG's `confCons.xml`, SecureCRT
//! and KiTTY session files), which read files the user picks. None may panic, and what an import writes must read back
//! as the same hosts: no name or value can add a line of its own.
//!
//! The same text goes to the snippet imports (Windows Terminal JSON,
//! SecureCRT button bars, MobaXterm macros, text files): every line they
//! make must be a plain line or a valid step, and a snippet's file must
//! read back as the same lines.

#![no_main]

use ferossh_config::import::{self, Client};
use ferossh_config::login::line_step;
use ferossh_config::{
    mobaxterm, mremoteng, putty, saved, securecrt, snippet_import, snippet_store, winscp,
};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let mut snippets = snippet_import::windows_terminal(&text).unwrap_or_default();
    snippets.extend(snippet_import::securecrt_buttons(&text));
    snippets.extend(snippet_import::mobaxterm_macros(&text));
    snippets.extend(snippet_import::text_file(&text, &text));
    for s in &snippets {
        for l in &s.lines {
            assert!(!matches!(line_step(l), Some(Err(_))), "{l:?}");
        }
        let file = snippet_store::file_for(s);
        assert!(!file.split('/').any(|p| p.is_empty() || p == ".."), "{file}");
        let back = snippet_store::parse_file(&snippet_store::file_text(s), &s.category).unwrap();
        let trimmed: Vec<&str> = s.lines.iter().map(|l| l.trim()).collect();
        assert_eq!(back.lines.iter().map(String::as_str).collect::<Vec<_>>(), trimmed);
    }
    let mut sessions = winscp::from_ini(&text);
    sessions.extend(mobaxterm::from_ini(&text));
    sessions.extend(mremoteng::from_xml(&text).unwrap_or_default());
    if let Some(s) = securecrt::session(&text, &text) {
        sessions.push(s);
    }
    if let Some(s) = putty::file_session(&text, &text) {
        sessions.push(s);
    }
    if let Some((name, rest)) = text.split_once('\n')
        && let Some(s) = putty::file_session(name, rest)
    {
        sessions.push(s);
    }
    import::distinct(&mut sessions);
    let all: Vec<&import::Session> = sessions.iter().collect();
    let written = import::to_config(Client::WinScp, &all);
    let back = saved::parse(&written);
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
});
