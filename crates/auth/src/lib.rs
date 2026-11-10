//! Agent, Pageant, WebAuthn and CNG signing broker.
//!
//! Runs in the UI process. The sandboxed worker never sees a private key:
//! it asks for public keys and for signatures over IPC, and the UI answers
//! through this crate after checking that each request is a user-auth
//! request for the session the user started ([`userauth::check`]).
//! FIDO2 security keys in key files sign through an [`sk::Authenticator`]:
//! Windows' WebAuthn ([`webauthn`]) in the window. TPM keys through CNG
//! are still to come.

pub mod agent;
pub mod broker;
pub mod keyfile;
pub mod sk;
pub mod userauth;
#[cfg(windows)]
pub mod webauthn;

pub use broker::{Broker, Identity, Job, KeyFileSpec, Reply, SignError, Source};
