//! Helper library for native MoQ applications.
//!
//! Establishes MoQ connections over:
//! - WebTransport (HTTP/3)
//! - Raw QUIC (with ALPN negotiation)
//! - WebSocket (fallback via [web-transport-ws](https://crates.io/crates/web-transport-ws))
//! - Iroh P2P (requires `iroh` feature)
//!
//! See [`Client`] for connecting to relays and [`Server`] for accepting connections.

/// Default maximum number of concurrent QUIC streams (bidi and uni) per connection.
pub(crate) const DEFAULT_MAX_STREAMS: u64 = 1024;

pub mod bind;
mod client;
mod connect;
mod crypto;
mod error;
#[cfg(feature = "jemalloc")]
pub mod jemalloc;
mod log;
#[cfg(feature = "noq")]
pub mod noq;
#[cfg(feature = "quinn")]
pub mod quinn;
mod reconnect;
mod server;
pub mod tls;
mod util;
#[cfg(feature = "watch")]
pub mod watch;
#[cfg(feature = "websocket")]
pub mod websocket;

pub use client::*;
pub use connect::ConnectError;
pub use error::{Error, Result};
pub use log::*;
pub use reconnect::*;
pub use server::*;

// Re-export these crates.
pub use moq_net;
pub use rustls;

/// Re-exported because [`watch::FileWatcher`] surfaces `notify::Result`/`notify::Error`
/// in its API; a major `notify` bump is therefore a breaking change for this crate.
#[cfg(feature = "watch")]
pub use notify;

#[cfg(feature = "quiche")]
pub mod quiche;

#[cfg(feature = "iroh")]
pub mod iroh;

/// The QUIC backend to use for connections.
#[derive(Clone, Debug, clap::ValueEnum, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum QuicBackend {
	/// [web-transport-quinn](https://crates.io/crates/web-transport-quinn)
	#[cfg(feature = "quinn")]
	Quinn,

	/// [web-transport-quiche](https://crates.io/crates/web-transport-quiche)
	#[cfg(feature = "quiche")]
	Quiche,

	/// [web-transport-noq](https://crates.io/crates/web-transport-noq)
	#[cfg(feature = "noq")]
	Noq,
}
