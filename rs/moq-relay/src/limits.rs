//! Request caps and QUIC limits selected after the relay authorizes a session.

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
	/// Maximum concurrent QUIC streams the peer may open, per direction.
	#[usage(
		long = "peer-max-streams",
		env = "MOQ_PEER_MAX_STREAMS",
		default = "100000",
		setting = "peer.max_streams"
	)]
	pub max_streams: u64,
	/// Connection-wide QUIC receive window, in bytes.
	#[usage(
		long = "peer-receive-window",
		env = "MOQ_PEER_RECEIVE_WINDOW",
		default = "671088640",
		setting = "peer.receive_window"
	)]
	pub receive_window: u64,
	/// Cap on unacknowledged outgoing QUIC data, in bytes.
	#[usage(
		long = "peer-send-window",
		env = "MOQ_PEER_SEND_WINDOW",
		default = "671088640",
		setting = "peer.send_window"
	)]
	pub send_window: u64,
}

impl Default for Peer {
	fn default() -> Self {
		Self {
			announces: 100000,
			subscriptions: 10000,
			fetches: 10000,
			// An order of magnitude above the `--quic-*` defaults, which are sized
			// for one viewer rather than a link carrying a whole cluster's traffic.
			max_streams: 10 * moq_tokio::quic::DEFAULT_MAX_STREAMS,
			receive_window: 10 * moq_tokio::quic::DEFAULT_RECEIVE_WINDOW,
			send_window: 10 * moq_tokio::quic::DEFAULT_RECEIVE_WINDOW,
		}
	}
}

impl Peer {
	pub(crate) fn resolve(&self) -> moq_net::session::Limits {
		resolve(self.announces, self.subscriptions, self.fetches)
	}

	/// The QUIC limits a peer session runs with, refusing any below what an
	/// ordinary session gets from `client`.
	///
	/// Applying a smaller value would shrink the connection rather than widen
	/// it. An unset client window is held to the larger default, 64 MiB, since
	/// the backends default it differently.
	pub(crate) fn quic(&self, client: &moq_tokio::quic::Config) -> anyhow::Result<moq_tokio::quic::Limits> {
		let window = moq_tokio::quic::DEFAULT_RECEIVE_WINDOW;
		for (name, peer, floor) in [
			("max_streams", self.max_streams, client.resolve().max_streams),
			(
				"receive_window",
				self.receive_window,
				client.receive_window.unwrap_or(window),
			),
			("send_window", self.send_window, client.send_window.unwrap_or(window)),
		] {
			anyhow::ensure!(
				peer >= floor,
				"peer.{name} ({peer}) is below quic.{name} ({floor}), so it would shrink peer sessions"
			);
		}
		Ok(moq_tokio::quic::Limits {
			max_streams: self.max_streams,
			receive_window: self.receive_window,
			send_window: self.send_window,
		})
	}
}

/// `client` with the peer limits applied, for the links the cluster dials:
/// every one of them is a peer, so it gets them from the handshake on.
pub(crate) fn dial(client: &moq_tokio::quic::Config, peer: moq_tokio::quic::Limits) -> moq_tokio::quic::Config {
	let mut dial = client.clone();
	dial.max_streams = Some(peer.max_streams);
	dial.receive_window = Some(peer.receive_window);
	dial.send_window = Some(peer.send_window);
	dial
}

fn resolve(announces: usize, subscriptions: usize, fetches: usize) -> moq_net::session::Limits {
	let mut limits = moq_net::session::Limits::default();
	limits.announces = announces;
	limits.subscriptions = subscriptions;
	limits.fetches = fetches;
	limits
}

#[cfg(test)]
mod tests {
	use super::*;

	/// The defaults widen every client default, explicit client values are
	/// the floor, and a peer value under it is refused rather than applied.
	#[test]
	#[allow(clippy::field_reassign_with_default)]
	fn peer_quic_limits_never_shrink_the_client() {
		let mut client = moq_tokio::quic::Config::default();
		let limits = Peer::default().quic(&client).expect("defaults widen");
		assert_eq!(limits.max_streams, 100_000);
		assert_eq!(limits.receive_window, 640 << 20);
		assert_eq!(limits.send_window, 640 << 20);

		let mut peer = Peer::default();
		peer.max_streams = moq_tokio::quic::DEFAULT_MAX_STREAMS - 1;
		let err = peer.quic(&client).unwrap_err().to_string();
		assert!(err.contains("peer.max_streams"), "{err}");

		// An unset client window is held to the larger backend default.
		let mut peer = Peer::default();
		peer.send_window = 32 << 20;
		let err = peer.quic(&client).unwrap_err().to_string();
		assert!(err.contains("peer.send_window"), "{err}");

		client.receive_window = Some(1 << 30);
		let err = Peer::default().quic(&client).unwrap_err().to_string();
		assert!(err.contains("peer.receive_window"), "{err}");

		client.receive_window = Some(16 << 20);
		client.send_window = Some(16 << 20);
		client.max_streams = Some(100);
		let mut peer = Peer::default();
		peer.max_streams = 100;
		peer.receive_window = 16 << 20;
		peer.send_window = 16 << 20;
		assert!(peer.quic(&client).is_ok(), "equal to the client is not a shrink");
	}
}
