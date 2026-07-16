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

pub use client::*;
pub use connect::ConnectError;
pub use error::{Error, Result};
pub use log::*;
pub use reconnect::*;
pub use server::*;

/// An established MoQ session, driven by a background tokio task.
///
/// Returned by [`Client::connect`] and [`Request::ok`]. Exposes the observer
/// surface of [`moq_net::SessionHandle`] (stats, bandwidth, waiting on close) and
/// closes the session when dropped, so the connection's lifetime follows this
/// value even though the protocol work runs on its own task.
pub struct Session {
	handle: moq_net::SessionHandle,
}

impl Session {
	/// Drive the session on the current tokio runtime, keeping its observer handle.
	pub(crate) fn spawn(mut session: moq_net::Session) -> Self {
		let handle = session.handle();
		tokio::spawn(async move {
			let _ = session.run().await;
		});
		Self { handle }
	}

	/// Returns the negotiated protocol version.
	pub fn version(&self) -> moq_net::Version {
		self.handle.version()
	}

	/// A cheap cloneable observer for this session; see [`moq_net::SessionHandle`].
	pub fn handle(&self) -> moq_net::SessionHandle {
		self.handle.clone()
	}

	/// Returns a snapshot of the current connection statistics.
	pub fn stats(&self) -> moq_net::ConnectionStats {
		self.handle.stats()
	}

	/// Returns a consumer for the estimated send bitrate, if the QUIC backend reports one.
	pub fn send_bandwidth(&self) -> Option<moq_net::bandwidth::Consumer> {
		self.handle.send_bandwidth()
	}

	/// Returns a consumer for the estimated receive bitrate, if the negotiated version supports PROBE.
	pub fn recv_bandwidth(&self) -> Option<moq_net::bandwidth::Consumer> {
		self.handle.recv_bandwidth()
	}

	/// Close the session with the given error. Dropping this value does the same
	/// with [`moq_net::Error::Cancel`].
	pub fn close(&self, err: moq_net::Error) {
		self.handle.close(err);
	}

	/// Block until the session is closed.
	pub async fn closed(&self) -> std::result::Result<(), moq_net::Error> {
		self.handle.closed().await
	}
}

impl Drop for Session {
	fn drop(&mut self) {
		self.handle.close(moq_net::Error::Cancel);
	}
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
