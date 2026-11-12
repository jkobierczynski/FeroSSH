//! Feeds arbitrary bytes to what reads an encrypted connections file or
//! vault: the sealed file's header (read before any passphrase), the
//! decrypted bundle, and the merge an import does. None may panic; a
//! bundle that parses must write back as one that reads the same, and
//! merging the same connections twice must change nothing the second
//! time.

#![no_main]

use ferossh_config::bundle::{self, Bundle};
use ferossh_config::sealed;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let _ = sealed::header(&text);
    if let Some(b) = Bundle::parse(data) {
        let bytes = b.to_bytes();
        let again = Bundle::parse(&bytes).expect("written bundle reads back");
        assert_eq!(again.files, b.files);
        assert_eq!(again.secrets.len(), b.secrets.len());
        for (x, y) in again.secrets.iter().zip(&b.secrets) {
            assert_eq!(x.target, y.target);
            assert_eq!(x.user, y.user);
            assert_eq!(x.password.as_str(), y.password.as_str());
        }
        assert_eq!(&again.to_bytes()[..], &bytes[..]);
    }
    // Split the input in two: what is here and what the file brings.
    let cut = text
        .char_indices()
        .nth(text.chars().count() / 2)
        .map_or(text.len(), |(i, _)| i);
    let (local, incoming) = text.split_at(cut);
    let (merged, _) = bundle::merge_hosts(local, incoming);
    let (twice, m) = bundle::merge_hosts(&merged, incoming);
    assert!(!m.changed(), "{local:?} + {incoming:?}");
    assert_eq!(bundle::count_hosts(&twice), bundle::count_hosts(&merged));
});
