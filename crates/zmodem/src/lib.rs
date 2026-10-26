//! ZMODEM, the protocol behind `rz` and `sz`, without the I/O: spotting a
//! transfer's start in terminal output ([`Detector`]), the framing
//! ([`frame`]), and the two ends of a transfer ([`Receiver`] when the
//! server runs `sz`, [`Sender`] when it runs `rz`).
//!
//! Everything here reads bytes a server chose, so it runs in the
//! sandboxed worker, never panics, keeps at most one subpacket in memory
//! and is fuzzed (`zmodem_feed`). Files stay with the caller: the receiver
//! hands their bytes out and the sender asks for them, so the worker never
//! opens a file. Only binary transfers are offered (no newline
//! conversion), and the receiver refuses ZCOMMAND, the request to run a
//! command, outright.
//!
//! Positions in ZMODEM are 32-bit, so a file over 4 GiB cannot be sent.

mod detect;
pub mod frame;
mod receive;
mod send;

pub use detect::{Detector, Found, Start};
pub use receive::{FileInfo, Receiver, RxEvent};
pub use send::{MAX_READ, Sender, TxEvent, Upload};

/// The largest file ZMODEM can send.
pub const MAX_FILE: u64 = u32::MAX as u64;
