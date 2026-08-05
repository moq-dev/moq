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
//!   It serves every request; gate access by layering your own middleware onto
//!   [`Server::router`](server::Server::router).
//!
//! All CMAF byte handling (import via [`moq_mux::container::fmp4::Import`],
//! export via [`moq_mux::container::fmp4::Muxer`]) lives in `moq-mux`; this
//! crate owns the HLS manifest generation, the timeline-driven playlist
//! window, and the HTTP surface.

#![warn(missing_docs)]

mod error;
pub mod export;
pub mod import;
#[cfg(feature = "server")]
pub mod server;

pub use error::*;
#[cfg(feature = "server")]
pub use server::Server;

/// Re-export of the HTTP stack behind the export server, so consumers can name the
/// types that surface through [`Server::router`] (and layer their own middleware on
/// it) without adding their own axum dependency and risking a version mismatch.
/// `axum::http` covers the `http` crate types too. A major axum bump is therefore a
/// breaking change for this crate.
#[cfg(feature = "server")]
pub use axum;

/// Re-export of the HTTP client used by [`import`], so consumers can name the
/// [`reqwest::Error`] carried by [`Error::Reqwest`] without adding their own reqwest
/// dependency. A major reqwest bump is therefore a breaking change for this crate.
pub use reqwest;

/// Re-export of the URL parser, so consumers can name the [`url::Url`] and
/// [`url::ParseError`] carried by [`Error`] without adding their own url dependency.
/// A major url bump is therefore a breaking change for this crate.
pub use url;
