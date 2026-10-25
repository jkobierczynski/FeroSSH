//! Agent, Pageant, WebAuthn and CNG signing broker.
//!
//! Runs in the UI process. The sandboxed worker never sees a private key:
//! it asks for public keys and for signatures over IPC, and the UI answers
//! through this crate after checking that each request is a user-auth
//! request for the session the user started ([`userauth::check`]).
//! FIDO2 through WebAuthn and TPM keys through CNG arrive in milestone 6.

pub mod agent;
pub mod broker;
pub mod keyfile;
pub mod userauth;

pub use broker::{Broker, Identity, Job, KeyFileSpec, Reply, SignError, Source};
