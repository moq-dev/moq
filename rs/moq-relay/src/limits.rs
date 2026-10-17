//! Request caps and QUIC limits selected after the relay authorizes a session.

use anyhow::Context;
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

/// Request caps and QUIC limits for authenticated cluster peer sessions.
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
	/// QUIC limits a peer session widens to (`[peer.quic]`).
	#[usage(flatten)]
	pub quic: Quic,
}

impl Default for Peer {
	fn default() -> Self {
		Self {
			announces: 100000,
			subscriptions: 10000,
			fetches: 10000,
			quic: Quic::default(),
		}
	}
}

impl Peer {
	pub(crate) fn resolve(&self) -> moq_net::session::Limits {
		resolve(self.announces, self.subscriptions, self.fetches)
	}
}

/// QUIC limits for peer sessions and the links the relay dials, keyed like `[quic]`.
#[derive(usage::Args, Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct Quic {
	/// Maximum concurrent QUIC streams the peer may open, per direction.
	#[usage(
		long = "peer-quic-max-streams",
		env = "MOQ_PEER_QUIC_MAX_STREAMS",
		default = "100000",
		setting = "peer.quic.max_streams"
	)]
	pub max_streams: std::num::NonZeroU64,
	/// Connection-wide QUIC receive window, in bytes.
	#[usage(
		long = "peer-quic-receive-window",
		env = "MOQ_PEER_QUIC_RECEIVE_WINDOW",
		default = "671088640",
		setting = "peer.quic.receive_window"
	)]
	pub receive_window: std::num::NonZeroU64,
	/// Cap on unacknowledged outgoing QUIC data, in bytes.
	#[usage(
		long = "peer-quic-send-window",
		env = "MOQ_PEER_QUIC_SEND_WINDOW",
		default = "671088640",
		setting = "peer.quic.send_window"
	)]
	pub send_window: std::num::NonZeroU64,
}

impl Default for Quic {
	fn default() -> Self {
		// An order of magnitude above the `[quic]` defaults, which are sized for
		// one viewer rather than a link carrying a whole cluster's traffic.
		let window = std::num::NonZeroU64::new(10 * moq_tokio::quic::DEFAULT_RECEIVE_WINDOW).expect("non-zero");
		Self {
			max_streams: std::num::NonZeroU64::new(10 * moq_tokio::quic::DEFAULT_MAX_STREAMS).expect("non-zero"),
			receive_window: window,
			send_window: window,
		}
	}
}

impl Quic {
	/// Refuses limits QUIC cannot advertise, as `[quic]` does, so the accept and
	/// dial paths never disagree about them.
	pub(crate) fn resolve(&self) -> anyhow::Result<moq_tokio::quic::Limits> {
		let limits = moq_tokio::quic::Limits {
			max_streams: self.max_streams,
			receive_window: self.receive_window,
			send_window: self.send_window,
		};
		limits.validate().context("invalid [peer.quic]")?;
		Ok(limits)
	}
}

fn resolve(announces: usize, subscriptions: usize, fetches: usize) -> moq_net::session::Limits {
	let mut limits = moq_net::session::Limits::default();
	limits.announces = announces;
	limits.subscriptions = subscriptions;
	limits.fetches = fetches;
	limits
}
