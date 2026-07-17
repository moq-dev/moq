//! Helper library for native MoQ applications.
//!
//! Establishes MoQ connections over:
//! - WebTransport (HTTP/3)
//! - Raw QUIC (with ALPN negotiation)
//! - WebSocket (fallback via [web-transport-ws](https://crates.io/crates/web-transport-ws))
//! - Plain TCP via the `tcp://` scheme (qmux, no TLS; requires `tcp` feature)
//! - Unix domain socket via the `unix://` scheme (qmux, peer-credential aware; requires `uds` feature, unix-only)
//! - Iroh P2P (requires `iroh` feature)
//!
//! See [`Client`] for connecting to relays and [`Server`] for accepting connections.

#![warn(missing_docs)]

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
pub mod quic;
#[cfg(feature = "quinn")]
pub mod quinn;
mod reconnect;
mod server;
#[cfg(feature = "tcp")]
pub mod tcp;
pub mod tls;
#[cfg(all(feature = "uds", unix))]
pub mod unix;
mod util;
#[cfg(feature = "watch")]
pub mod watch;
#[cfg(feature = "websocket")]
pub mod websocket;

// Enumerated rather than globbed, so the root surface is a deliberate list and a
// new `pub` item in these modules doesn't silently join it.
pub use client::{Client, ClientConfig};
pub use connect::ConnectError;
pub use error::{Error, Result};
pub use log::Log;
pub use reconnect::{Backoff, ConnectionStatsReader, Reconnect, Status};
pub use server::{Request, Server, ServerConfig, Transport};

/// Spawn the session's protocol driver on the current tokio runtime, handing back
/// the session it drives.
///
/// The driver holds no session clone, so the session still closes when the caller
/// drops their last [`moq_net::Session`] handle, which in turn lets the driver
/// task finish.
pub(crate) fn spawn_session((session, driver): (moq_net::Session, moq_net::Driver)) -> moq_net::Session {
	tokio::spawn(driver);
	session
}

// Re-export these crates.
pub use moq_net;
pub use rustls;

/// Re-exported because [`watch::FileWatcher`] surfaces `notify::Result`/`notify::Error`
/// in its API; a major `notify` bump is therefore a breaking change for this crate.
#[cfg(feature = "watch")]
pub use notify;

/// Re-exported because [`tls::init_android`] takes a `jni::Env` handle; a major
/// `jni` bump is therefore a breaking change for this crate.
#[cfg(target_os = "android")]
pub use jni;

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

fn default_quic_backend() -> QuicBackend {
	#[cfg(feature = "quinn")]
	{
		QuicBackend::Quinn
	}
	#[cfg(all(feature = "noq", not(feature = "quinn")))]
	{
		QuicBackend::Noq
	}
	#[cfg(all(feature = "quiche", not(feature = "quinn"), not(feature = "noq")))]
	{
		QuicBackend::Quiche
	}
	#[cfg(all(not(feature = "quiche"), not(feature = "quinn"), not(feature = "noq")))]
	panic!("no QUIC backend compiled; enable noq, quinn, or quiche feature");
}

#[cfg(test)]
mod tests {
	#[cfg(feature = "quinn")]
	#[test]
	fn quinn_is_the_default_backend() {
		assert!(matches!(super::default_quic_backend(), super::QuicBackend::Quinn));
	}
}
