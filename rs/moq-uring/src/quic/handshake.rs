//! The handshake an accepted connection goes through before it is served.
//!
//! An [`Endpoint`](super::Endpoint) bounds it by [`Config::timeout`] from the
//! connection's first packet, closing a QUIC handshake still running at the
//! deadline. The rest of the deadline is handed to whatever the application
//! layers on top (HTTP/3, MoQ SETUP) as [`Info::deadline`].

use std::time::Duration;

use kio::clock::Instant;

/// How an endpoint bounds the handshakes it accepts.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Config {
	/// How long an incoming connection has from its first packet to finish its
	/// whole handshake, or `None` (the default) for no deadline. Without one,
	/// only inactivity ends a stalled handshake: moq-quic's fixed 10 s
	/// handshake idle timeout until QUIC is established, which
	/// [`transport::Config::idle_timeout`] does not change, then
	/// [`transport::Config::idle_timeout`] through the layers on top.
	///
	/// [`transport::Config::idle_timeout`]: super::transport::Config::idle_timeout
	pub timeout: Option<Duration>,
}

/// An established connection's handshake, from
/// [`Connection::handshake`](super::Connection::handshake).
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct Info {
	/// When the handshake, including whatever the application layers on top of
	/// QUIC, must be done. `None` on a dialed connection or without a
	/// [`Config::timeout`].
	pub deadline: Option<Instant>,
}
