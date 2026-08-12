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
//! See [`Client`] for connecting to relays and [`Server`] for accepting
//! connections. [`mdns`] finds peers to connect to on the local network.

#![warn(missing_docs)]

pub mod accept;
pub mod bind;
mod client;
pub mod connect;
mod connection;
mod crypto;
mod error;
#[cfg(any(feature = "quinn", feature = "noq", feature = "quiche", feature = "tcp"))]
pub mod failover;
#[cfg(feature = "jemalloc")]
pub mod jemalloc;
pub mod listen;
mod log;
#[cfg(feature = "noq")]
pub mod noq;
pub mod quic;
#[cfg(feature = "quinn")]
pub mod quinn;
mod server;
#[cfg(feature = "tcp")]
pub mod tcp;
pub mod tls;
pub mod transport;
#[cfg(all(feature = "uds", unix))]
pub mod unix;
// Resolving a `host:port` bind string is a QUIC-listener concern; the stream
// listeners take a `SocketAddr`/path straight from their config.
#[cfg(any(feature = "noq", feature = "quinn", feature = "quiche"))]
mod util;
#[cfg(feature = "watch")]
pub mod watch;
#[cfg(feature = "websocket")]
pub mod websocket;

// Enumerated rather than globbed, so the root surface is a deliberate list and a
// new `pub` item in these modules doesn't silently join it.
pub use client::Client;
pub use connect::{Addrs, ConnectError};
pub use connection::{Backoff, Connection, ConnectionStatsReader, GoawayConfig, Redirect, Status};
pub use error::{Error, Result};
pub use log::Log;
pub use server::{Listener, Request, Server, Transport};

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

fn version_parser() -> impl clap::builder::TypedValueParser<Value = moq_net::Version> {
	use clap::builder::TypedValueParser;

	clap::builder::PossibleValuesParser::new(moq_net::Version::names())
		.map(|name| name.parse().expect("possible version names must parse"))
}

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

#[cfg(feature = "mdns")]
pub mod mdns;

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

/// Parses the same spellings the CLI and TOML accept (`quinn`, `quiche`, `noq`),
/// case-insensitively. A backend this build was compiled without is an error, since
/// its variant doesn't exist.
impl std::str::FromStr for QuicBackend {
	type Err = String;

	fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
		<Self as clap::ValueEnum>::from_str(s, true)
	}
}

impl QuicBackend {
	/// Every backend this build was compiled with, spelled the way [`FromStr`] accepts.
	///
	/// The variants are feature-gated, so this is the only honest answer to "what can I
	/// pass here". A caller building a menu should read it rather than listing the three
	/// names, which would offer options that cannot parse.
	///
	/// [`FromStr`]: std::str::FromStr
	pub fn compiled() -> &'static [Self] {
		&[
			#[cfg(feature = "quinn")]
			Self::Quinn,
			#[cfg(feature = "quiche")]
			Self::Quiche,
			#[cfg(feature = "noq")]
			Self::Noq,
		]
	}

	/// The name [`FromStr`] accepts for this backend.
	///
	/// [`FromStr`]: std::str::FromStr
	pub fn as_str(&self) -> &'static str {
		match *self {
			#[cfg(feature = "quinn")]
			Self::Quinn => "quinn",
			#[cfg(feature = "quiche")]
			Self::Quiche => "quiche",
			#[cfg(feature = "noq")]
			Self::Noq => "noq",
		}
	}
}

/// Whether this build can capture qlog traces, which the `qlog` feature gates.
///
/// Setting a qlog directory without it is an error at dial time, so a caller offering
/// the knob should check here rather than surfacing an option that cannot work.
pub fn qlog_supported() -> bool {
	cfg!(feature = "qlog")
}

/// The backend a config without an explicit `--*-backend` gets.
///
/// Only compiled when there is one to pick: a build with no QUIC backend never
/// reaches for a default, since `QuicBackend` has no variants there.
#[cfg(any(feature = "noq", feature = "quinn", feature = "quiche"))]
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
}

#[cfg(test)]
mod tests {
	#[cfg(feature = "quinn")]
	#[test]
	fn quinn_is_the_default_backend() {
		assert!(matches!(super::default_quic_backend(), super::QuicBackend::Quinn));
	}
}
