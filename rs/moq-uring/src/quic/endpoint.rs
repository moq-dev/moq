//! The multi-connection endpoint: one socket, many QUIC connections.
//!
//! An [`Endpoint`] owns a [`udp::Socket`](crate::udp::Socket) and routes
//! every received datagram to the connection its destination connection id
//! names: dials share the socket with accepted connections, which is what
//! lets one worker socket carry a relay's inbound sessions and its upstream
//! cluster dials at once. A single demux task receives; each connection keeps
//! a driver task of its own for timers and egress.
//!
//! Every connection id this endpoint issues is [`CID_LEN`] bytes, which is
//! what lets a short header (which does not encode the id's length) be
//! parsed at all. Ids rotate as peers consume them (`NEW_CONNECTION_ID`), so
//! a migrating client stays routable; an Initial for an unsupported version
//! gets a version negotiation packet back.
//!
//! An endpoint whose socket is a member of a steered `SO_REUSEPORT` group
//! (one worker per core on one port) sets [`Config::shard`], and every id it
//! issues then carries the [`moq_sock::shard::cid_prefix`] steering byte, so
//! the kernel keeps delivering a connection's packets to the worker that owns
//! it. Dials through the endpoint carry it too, which is what steers a
//! cluster peer's responses back to the dialing worker.

use super::server;

pub use super::backend::Endpoint;

/// Every connection id this endpoint issues is this long: long enough to be
/// unguessable per socket, and fixed because a short header does not encode
/// the id's length, so parsing one assumes it.
pub const CID_LEN: usize = 8;

/// What an endpoint serves, beyond dialing.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Config {
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

	/// This socket's slot in a steered `SO_REUSEPORT` group, if it is in one.
	///
	/// Every connection id the endpoint issues then leads with the slot's
	/// [`cid_prefix`](moq_sock::shard::cid_prefix) byte, which is what the
	/// group's filter selects on. Set it if and only if the socket was bound
	/// into a group with this shard; a lone socket leaves it `None` and keeps
	/// the whole id random.
	pub shard: Option<moq_sock::shard::Shard>,
}

impl Default for Config {
	fn default() -> Self {
		Self {
			server: None,
			backlog: 1024,
			shard: None,
		}
	}
}

impl Config {
	/// Accept incoming connections with `server`, on top of dialing.
	pub fn with_server(mut self, server: server::Config) -> Self {
		self.server = Some(server);
		self
	}

	/// Issue connection ids steering to `shard`'s slot of a reuseport group.
	pub fn with_shard(mut self, shard: moq_sock::shard::Shard) -> Self {
		self.shard = Some(shard);
		self
	}
}

/// A fresh [`CID_LEN`]-byte connection id, leading with the steering prefix
/// when the endpoint sits in a reuseport group.
pub(crate) fn cid(shard: Option<moq_sock::shard::Shard>) -> [u8; CID_LEN] {
	let mut cid: [u8; CID_LEN] = rand::random();
	if let Some(shard) = shard {
		cid[0] = moq_sock::shard::cid_prefix(shard);
	}
	cid
}
