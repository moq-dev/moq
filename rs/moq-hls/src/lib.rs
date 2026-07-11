//! HLS / LL-HLS <-> MoQ gateway.
//!
//! Bridges HLS (and Low-Latency HLS) and [`moq_net`] broadcasts in both
//! directions, mirroring the WHIP/WHEP split in `moq-rtc`:
//!
//! - [`import`] pulls a remote HLS master/media playlist and publishes its CMAF
//!   segments into MoQ (an HTTP *client* that *publishes*).
//! - [`server`] serves HLS playlists and CMAF segments over HTTP for MoQ
//!   broadcasts (an HTTP *server*). It subscribes only to each broadcast's
//!   catalog and per-rendition timeline tracks; media bytes are FETCHed from
//!   the relay one group at a time, only when a segment is actually requested.
//!   It serves every request by default; gate access with
//!   [`Server::with_authorizer`](server::Server::with_authorizer).
//!
//! All CMAF byte handling (import via [`moq_mux::container::fmp4::Import`],
//! export via [`moq_mux::container::fmp4::Muxer`]) lives in `moq-mux`; this
//! crate owns the HLS manifest generation, the timeline-driven playlist
//! window, and the HTTP surface.

mod error;
pub mod export;
pub mod import;
#[cfg(feature = "server")]
pub mod server;

pub use error::*;
#[cfg(feature = "server")]
pub use server::{Authorizer, Server};
