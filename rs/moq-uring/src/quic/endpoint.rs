//! The multi-connection endpoint: one socket, many QUIC connections.
//!
//! An [`Endpoint`] owns a [`udp::Socket`](crate::udp::Socket) and routes
//! every received datagram to the connection its destination connection id
//! names: dials share the socket with accepted connections, which is what
//! lets one worker socket carry a relay's inbound sessions and its upstream
//! cluster dials at once. A single demux task receives; each connection keeps
//! a driver task of its own for timers and egress.
//!
//! Every connection id this endpoint issues is 8 random bytes, fixed-length
//! because a short header does not encode the id's length. Ids
//! rotate as peers consume them (`NEW_CONNECTION_ID`), so a migrating client
//! stays routable; an Initial for an unsupported version gets a version
//! negotiation packet back.
//!
//! The socket is the endpoint's identity. Its worker is where the demux and
//! every connection driver run, whichever thread built the endpoint. A socket
//! adopted as a member of a steered `SO_REUSEPORT` group (one worker per core
//! on one port; see [`udp::Bound`](crate::udp::Bound)) brings its slot along,
//! and every id the endpoint issues then names that shard
//! (`moq_quic::RandomConnectionIdGenerator::set_shard`), so the kernel keeps
//! delivering a connection's packets to the worker that owns it. Dials
//! through the endpoint carry it too, which is what steers a cluster peer's
//! responses back to the dialing worker.

use super::server;

pub use super::driver::Endpoint;

/// What an endpoint serves, beyond dialing.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Config {
	/// Tracks accepted connections from admission until transport closure.
	pub activity: moq_net::server::Activity,
	/// Accept incoming connections with this configuration. Without it the
	/// endpoint only dials, and inbound handshakes are dropped.
	pub server: Option<server::Config>,

	/// How many incoming connections may await acceptance at once (default 1024).
	///
	/// This bounds both handshakes in flight and completed connections queued
	/// for [`Endpoint::accept`]. An Initial past the cap is dropped; a real
	/// peer retransmits and lands once the application drains the backlog or
	/// a handshake fails.
	pub backlog: usize,

	/// How incoming handshakes are bounded.
	pub handshake: super::handshake::Config,
}

impl Default for Config {
	fn default() -> Self {
		Self {
			server: None,
			activity: Default::default(),
			backlog: 1024,
			handshake: Default::default(),
		}
	}
}

impl Config {
	/// Accept incoming connections with `server`, on top of dialing.
	pub fn with_server(mut self, server: server::Config) -> Self {
		self.server = Some(server);
		self
	}
}
