//! HLS / LL-HLS <-> MoQ gateway.
//!
//! Bridges HLS (and Low-Latency HLS) and [`moq_net`] broadcasts in both
//! directions, mirroring the WHIP/WHEP split in `moq-rtc`:
//!
//! - [`import`] pulls a remote HLS master/media playlist and publishes its CMAF
//!   segments into MoQ (an HTTP *client* that *publishes*).
//! - [`server`] subscribes to a MoQ broadcast and serves HLS + LL-HLS playlists
//!   and CMAF segments over HTTP (an HTTP *server* that *subscribes*).
//!
//! All CMAF byte handling (import via [`moq_mux::container::fmp4::Import`],
//! export via [`moq_mux::container::fmp4::Export`]) lives in `moq-mux`; this
//! crate owns the HLS manifest generation, segment/part windowing, and the HTTP
//! surface.

mod error;
pub mod export;
pub mod import;
#[cfg(feature = "server")]
pub mod server;

pub use error::*;
#[cfg(feature = "server")]
pub use server::Server;

/// The HTTP client library used by [`import`], re-exported so callers can supply their
/// own client via [`import::Config::client`] (e.g. to reach an authenticated origin).
///
/// A major version bump of this dependency is a breaking change for that field.
pub use reqwest;
