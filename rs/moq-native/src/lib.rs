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
mod server;

#[cfg(any(feature = "noq", feature = "quinn"))]
mod tls;

#[cfg(feature = "noq")]
mod noq;
#[cfg(feature = "quiche")]
mod quiche;
#[cfg(feature = "quinn")]
mod quinn;

#[cfg(feature = "websocket")]
pub mod ws;

#[cfg(feature = "iroh")]
pub mod iroh;

// Re-export core types at root
pub use client::{Client, ClientConfig, ClientTls};
pub use log::Log;
pub use server::{Request, Server, ServerConfig, ServerId, ServerTlsConfig, ServerTlsInfo};

// Re-export dependency crates
pub use moq_lite;
pub use rustls;

#[cfg(feature = "noq")]
pub use web_transport_noq;
#[cfg(feature = "quiche")]
pub use web_transport_quiche;
#[cfg(feature = "quinn")]
pub use web_transport_quinn;

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
