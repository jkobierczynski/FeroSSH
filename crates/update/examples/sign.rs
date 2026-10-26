//! Writes and signs a release's update file, for the release workflow:
//!
//!     UPDATE_SIGNING_KEY=... cargo run -p ferossh-update --example sign -- 1.0.1 2026-11-02 dist
//!
//! It writes dist/update.txt and dist/update.txt.sig, and refuses when the
//! key does not match crates/update/update-key.pub.

use std::process::ExitCode;

use ferossh_update::{Version, file_for, public_key, sign, verify};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [version, released, dir] = args.as_slice() else {
        eprintln!("usage: sign VERSION YYYY-MM-DD DIR");
        return ExitCode::FAILURE;
    };
    let Some(version) = Version::parse(version) else {
        eprintln!("not a version: {version}");
        return ExitCode::FAILURE;
    };
    let Ok(private) = std::env::var("UPDATE_SIGNING_KEY") else {
        eprintln!("UPDATE_SIGNING_KEY is not set");
        return ExitCode::FAILURE;
    };
    let Some(public) = public_key() else {
        eprintln!("crates/update/update-key.pub holds no key");
        return ExitCode::FAILURE;
    };
    let file = file_for(&version, released);
    let Ok(signature) = sign(file.as_bytes(), &private) else {
        eprintln!("UPDATE_SIGNING_KEY is not a valid key");
        return ExitCode::FAILURE;
    };
    if verify(file.as_bytes(), signature.as_bytes(), &public).is_err() {
        eprintln!("UPDATE_SIGNING_KEY does not match update-key.pub");
        return ExitCode::FAILURE;
    }
    let dir = std::path::Path::new(dir);
    let written = std::fs::write(dir.join("update.txt"), &file)
        .and_then(|()| std::fs::write(dir.join("update.txt.sig"), format!("{signature}\n")));
    if let Err(e) = written {
        eprintln!("could not write the update file: {e}");
        return ExitCode::FAILURE;
    }
    print!("{file}");
    ExitCode::SUCCESS
}
