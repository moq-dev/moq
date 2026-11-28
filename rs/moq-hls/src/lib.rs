//! HLS and DASH <-> MoQ gateway.
//!
//! Bridges HLS and [`moq_net`] broadcasts in both directions, mirroring the
//! WHIP/WHEP split in `moq-webrtc`:
//!
//! - [`import`] pulls a remote HLS master/media playlist and publishes its CMAF
//!   segments into MoQ (an HTTP *client* that *publishes*).
//! - [`server`] serves HLS playlists, DASH manifests, and CMAF segments over
//!   HTTP for MoQ broadcasts (an HTTP *server*). It subscribes only to each
//!   broadcast's catalog and numbers segments on its `sync.interval` grid; media
//!   bytes are fetched from the relay in one bounded subscription, only when a segment
//!   is actually requested. Until the catalog declares `sync.interval`, a
//!   non-lazy server also holds its video and audio tracks subscribed, so an
//!   on-demand publisher produces the media its interval is measured from.
//!   It serves every request; gate access by layering your own middleware onto
//!   [`Server::router`](server::Server::router), or parse and authorize each request
//!   yourself and answer it with [`Server::respond`](server::Server::respond).
//!
//! All CMAF byte handling (import via [`moq_mux::container::fmp4::Import`],
//! export via [`moq_mux::container::fmp4::Muxer`]) lives in `moq-mux`; this
//! crate owns the HLS playlist and DASH manifest generation, the segment
//! grid, and the HTTP surface.
//!
//! The gateway drives its HTTP fetches and background watchers as async code, so [`import`] and
//! [`export`] need the `async` feature. The default `server` feature enables it.

#![warn(missing_docs)]
#![cfg_attr(
	not(feature = "async"),
	allow(
		rustdoc::broken_intra_doc_links,
		reason = "the docs link the gateways, which a poll-only build leaves out"
	)
)]

mod error;
#[cfg(feature = "async")]
pub mod export;
#[cfg(feature = "async")]
pub mod import;
#[cfg(feature = "server")]
pub mod server;

pub use error::*;
#[cfg(feature = "server")]
pub use server::Server;

/// Test helper: a [`moq_mux::Source`] for the broadcast at `path`, resolved through `origin`.
#[cfg(test)]
pub(crate) async fn resolved(origin: &moq_net::origin::Producer, path: &str) -> moq_mux::Source {
	let consumer = origin.consume();
	let broadcast = consumer.request(path, None).await.expect("the broadcast resolves");
	moq_mux::Source::new(consumer, broadcast)
}
