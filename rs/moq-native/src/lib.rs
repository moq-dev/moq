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

mod client;
mod crypto;
mod log;
#[cfg(feature = "quinn")]
mod quinn;
mod server;
#[cfg(feature = "websocket")]
mod websocket;

pub use client::*;
pub use log::*;
pub use server::*;
#[cfg(feature = "websocket")]
pub use websocket::*;

// Re-export these crates.
pub use moq_lite;
pub use rustls;

#[cfg(feature = "quinn")]
pub use web_transport_quinn;

#[cfg(feature = "quiche")]
mod quiche;
#[cfg(feature = "quiche")]
pub use web_transport_quiche;

#[cfg(feature = "iroh")]
mod iroh;
#[cfg(feature = "iroh")]
pub use iroh::*;

/// The QUIC backend to use for connections.
#[derive(Clone, Debug, clap::ValueEnum, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QuicBackend {
	/// [web-transport-quinn](https://crates.io/crates/web-transport-quinn)
	#[cfg(feature = "quinn")]
	Quinn,

	/// [web-transport-quiche](https://crates.io/crates/web-transport-quiche)
	#[cfg(feature = "quiche")]
	Quiche,
}
