//! The sandboxed SSH worker: russh sessions, channels and forwards.
//!
//! In M0 this crate holds only the crypto policy; sessions arrive in M3.

pub mod crypto_policy;
