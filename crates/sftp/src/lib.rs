//! File transfer over SFTP: the protocol codec the sandboxed worker
//! speaks to the server ([`wire`]), the UI's per-host transfer queue and
//! directory listing ([`remote`]), and the rules that keep a server's
//! file names from doing harm on Windows ([`names`]).

pub mod names;
pub mod remote;
pub mod wire;

pub use remote::{Choice, Job, JobId, Kind, Listing, Remote, State};
