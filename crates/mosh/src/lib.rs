//! The Mosh protocol (mosh.org), without the I/O, for FeroSSH's sandboxed
//! worker: [`crypto`] (AES-128-OCB packets), [`wire`] (fragments, zlib,
//! the protobuf messages), [`screen`] (the terminal state the server's
//! diffs apply to), [`predict`] (local echo of typed text) and [`client`]
//! (state synchronisation, timers and round-trip estimates, given the
//! time by the caller).

pub mod bootstrap;
pub mod client;
pub mod crypto;
pub mod predict;
pub mod screen;
pub mod wire;
