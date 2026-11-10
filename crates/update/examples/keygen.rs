//! Makes the update signing key, once:
//!
//!     cargo run -p ferossh-update --example keygen
//!
//! It writes the public key to crates/update/update-key.pub (commit that
//! file) and prints the private key, for the release workflow's
//! UPDATE_SIGNING_KEY secret. Keep no other copy unless it is safe: anyone
//! with it can tell every FeroSSH that an update exists.

use std::process::ExitCode;

fn main() -> ExitCode {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/update-key.pub");
    if std::fs::read_to_string(path).is_ok_and(|k| !k.trim().is_empty()) {
        eprintln!("{path} already holds a key; empty it first to make a new one.");
        return ExitCode::FAILURE;
    }
    let Ok((private, public)) = ferossh_update::generate() else {
        eprintln!("could not make a key");
        return ExitCode::FAILURE;
    };
    if let Err(e) = std::fs::write(path, format!("{public}\n")) {
        eprintln!("could not write {path}: {e}");
        return ExitCode::FAILURE;
    }
    println!("Public key written to {path}; commit it.");
    println!();
    println!(
        "Private key, for the UPDATE_SIGNING_KEY secret (Settings > Secrets and variables > Actions):"
    );
    println!();
    println!("{private}");
    ExitCode::SUCCESS
}
