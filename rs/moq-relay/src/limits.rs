//! Request caps selected after the relay authorizes a session.

use serde::{Deserialize, Serialize};

/// Request caps for ordinary client sessions.
#[derive(usage::Args, Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct Session {
	/// Maximum simultaneously announced broadcasts or namespaces.
	#[usage(
		long = "session-announces",
		env = "MOQ_SESSION_ANNOUNCES",
		default = "1000",
		setting = "session.announces"
	)]
	pub announces: usize,
	/// Maximum simultaneously held subscriptions.
	#[usage(
		long = "session-subscriptions",
		env = "MOQ_SESSION_SUBSCRIPTIONS",
		default = "1000",
		setting = "session.subscriptions"
	)]
	pub subscriptions: usize,
	/// Maximum simultaneously held FETCH requests.
	#[usage(
		long = "session-fetches",
		env = "MOQ_SESSION_FETCHES",
		default = "10000",
		setting = "session.fetches"
	)]
	pub fetches: usize,
}

impl Default for Session {
	fn default() -> Self {
		Self {
			announces: 1000,
			subscriptions: 1000,
			fetches: 10000,
		}
	}
}

impl Session {
	pub(crate) fn resolve(&self) -> moq_net::session::Limits {
		resolve(self.announces, self.subscriptions, self.fetches)
	}
}

/// Request caps for authenticated cluster peer sessions.
#[derive(usage::Args, Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct Peer {
	/// Maximum simultaneously announced broadcasts or namespaces.
	#[usage(
		long = "peer-announces",
		env = "MOQ_PEER_ANNOUNCES",
		default = "100000",
		setting = "peer.announces"
	)]
	pub announces: usize,
	/// Maximum simultaneously held subscriptions.
	#[usage(
		long = "peer-subscriptions",
		env = "MOQ_PEER_SUBSCRIPTIONS",
		default = "10000",
		setting = "peer.subscriptions"
	)]
	pub subscriptions: usize,
	/// Maximum simultaneously held FETCH requests.
	#[usage(
		long = "peer-fetches",
		env = "MOQ_PEER_FETCHES",
		default = "10000",
		setting = "peer.fetches"
	)]
	pub fetches: usize,
}

impl Default for Peer {
	fn default() -> Self {
		Self {
			announces: 100000,
			subscriptions: 10000,
			fetches: 10000,
		}
	}
}

impl Peer {
	pub(crate) fn resolve(&self) -> moq_net::session::Limits {
		resolve(self.announces, self.subscriptions, self.fetches)
	}
}

fn resolve(announces: usize, subscriptions: usize, fetches: usize) -> moq_net::session::Limits {
	let mut limits = moq_net::session::Limits::default();
	limits.announces = announces;
	limits.subscriptions = subscriptions;
	limits.fetches = fetches;
	limits
}
