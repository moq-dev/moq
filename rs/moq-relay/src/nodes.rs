//! The local cluster view behind the internal `/nodes` endpoint.
//!
//! Lists every peer this relay is configured to dial, whether or not a session
//! is up, with its live sessions and when one last connected. A peer's history
//! is keyed by its dial identity, so it survives failed attempts and a dial
//! restarted for a new cost or credential, and is retired only when the peer
//! leaves the dial set. Accepted peer sessions are not listed: a peer declares
//! no URL, so there is nothing to name it by.

use std::{
	collections::BTreeMap,
	sync::{Arc, Mutex, PoisonError},
};

use serde::Serialize;

/// Outbound cluster peers by dial identity.
#[derive(Clone, Default)]
pub(crate) struct Nodes {
	state: Arc<Mutex<State>>,
}

#[derive(Default)]
struct State {
	/// Keyed by the dial set's peer identity, which may still carry userinfo.
	peers: BTreeMap<String, Peer>,
	/// Distinguishes the dials of one peer, so a replaced dial clears only its own session.
	next_dial: u64,
}

#[derive(Default)]
struct Peer {
	/// Live sessions by `conn` id, each with the dial that owns it.
	connections: BTreeMap<u64, (u64, moq_tokio::Transport)>,
	/// The latest successful handshake of any dial of this peer.
	last_connected: Option<kio::clock::Instant>,
}

/// The JSON document returned by the internal `/nodes` endpoint.
#[derive(Debug, Default, Serialize)]
pub(crate) struct Snapshot {
	/// Configured outbound peers, connected or not.
	pub nodes: Vec<Node>,
}

/// One configured outbound peer.
#[derive(Debug, Serialize)]
pub(crate) struct Node {
	/// The peer's canonical identity, with query, userinfo, and repeated slashes
	/// removed: its dial URL, or for a LAN peer that advertised no node URL, its
	/// per-run mDNS instance in URL form.
	pub node: String,
	/// Live sessions to this peer; empty while it connects or retries.
	pub connections: Vec<Connection>,
	/// Seconds since a session to this peer last connected, including sessions
	/// already closed. Null until the first successful handshake.
	pub last_connected_seconds_ago: Option<f64>,
}

/// A live outbound cluster session.
#[serde_with::serde_as]
#[derive(Debug, Serialize)]
pub(crate) struct Connection {
	/// The session's `conn` id, matching the `conn{id}` span its log lines carry.
	/// Process-local, and only meaningful while the session lasts.
	pub id: u64,
	/// The transport the session runs over.
	#[serde_as(as = "serde_with::DisplayFromStr")]
	pub transport: moq_tokio::Transport,
}

/// One dial task's handle on its peer's entry. Dropping it clears the session it
/// holds, but keeps the peer's history: [`Nodes::retain`] retires that.
pub(crate) struct Dial {
	nodes: Nodes,
	peer: String,
	id: u64,
}

impl Nodes {
	fn lock(&self) -> std::sync::MutexGuard<'_, State> {
		self.state.lock().unwrap_or_else(PoisonError::into_inner)
	}

	/// Register a dial of `peer`, the dial set's identity for it, keeping any
	/// history an earlier dial of the same peer left.
	pub(crate) fn dial(&self, peer: &str) -> Dial {
		let mut state = self.lock();
		state.peers.entry(peer.to_string()).or_default();
		let id = state.next_dial;
		state.next_dial += 1;
		Dial {
			nodes: self.clone(),
			peer: peer.to_string(),
			id,
		}
	}

	/// Retire the history of every peer `keep` rejects, once it leaves the dial set.
	pub(crate) fn retain(&self, mut keep: impl FnMut(&str) -> bool) {
		self.lock().peers.retain(|peer, _| keep(peer));
	}

	pub(crate) fn snapshot(&self) -> Snapshot {
		let state = self.lock();
		// Two identities can share a label once userinfo is removed, so merge them.
		let mut nodes = BTreeMap::<String, Node>::new();
		for (peer, entry) in &state.peers {
			let label = crate::cluster::canonicalize_peer_key(peer);
			let node = nodes.entry(label.clone()).or_insert_with(|| Node {
				node: label,
				connections: Vec::new(),
				last_connected_seconds_ago: None,
			});
			node.connections.extend(
				entry
					.connections
					.iter()
					.map(|(&id, &(_, transport))| Connection { id, transport }),
			);
			if let Some(age) = entry.last_connected.map(|at| at.elapsed().as_secs_f64()) {
				node.last_connected_seconds_ago =
					Some(node.last_connected_seconds_ago.map_or(age, |prev| prev.min(age)));
			}
		}
		for node in nodes.values_mut() {
			node.connections.sort_by_key(|connection| connection.id);
		}
		Snapshot {
			nodes: nodes.into_values().collect(),
		}
	}
}

impl Dial {
	/// Apply one lifecycle observation of session `id`, which this dial owns.
	pub(crate) fn observe(&self, id: u64, observation: &moq_tokio::connection::Observation) {
		let mut state = self.nodes.lock();
		// A retired peer's dial is being cancelled; it records nothing more.
		let Some(peer) = state.peers.get_mut(&self.peer) else {
			return;
		};
		if let Some(at) = observation.last_connected {
			peer.last_connected = Some(peer.last_connected.map_or(at, |prev| prev.max(at)));
		}
		match observation.transport {
			Some(transport) => {
				peer.connections.insert(id, (self.id, transport));
			}
			None => {
				peer.connections.remove(&id);
			}
		}
	}

	/// Clear this dial's session once it ends, keeping the peer's history.
	pub(crate) fn disconnected(&self) {
		if let Some(peer) = self.nodes.lock().peers.get_mut(&self.peer) {
			peer.connections.retain(|_, (dial, _)| *dial != self.id);
		}
	}
}

impl Drop for Dial {
	fn drop(&mut self) {
		self.disconnected();
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn observation(transport: Option<moq_tokio::Transport>) -> moq_tokio::connection::Observation {
		moq_tokio::connection::Observation {
			revision: 1,
			phase: match transport {
				Some(_) => moq_tokio::connection::Phase::Connected,
				None => moq_tokio::connection::Phase::Disconnected,
			},
			connects: 1,
			failure: None,
			version: None,
			last_connected: Some(kio::clock::now()),
			transport,
		}
	}

	#[test]
	fn configured_peers_list_before_they_connect_without_credentials() {
		let nodes = Nodes::default();
		let _dial = nodes.dial("https://user:secret@peer.example//relay/");

		assert_eq!(
			serde_json::to_value(nodes.snapshot()).unwrap(),
			serde_json::json!({
				"nodes": [{ "node": "https://peer.example/relay", "connections": [], "last_connected_seconds_ago": null }]
			}),
		);
	}

	#[test]
	fn history_survives_disconnects_and_a_replaced_dial() {
		let nodes = Nodes::default();
		let dial = nodes.dial("https://peer.example/");
		dial.observe(2, &observation(Some(moq_tokio::Transport::WebSocket)));
		assert_eq!(
			serde_json::to_value(&nodes.snapshot().nodes[0].connections).unwrap(),
			serde_json::json!([{ "id": 2, "transport": "websocket" }]),
		);

		// A connect and disconnect may both happen before the observer polls.
		dial.observe(2, &observation(None));
		dial.disconnected();
		let snapshot = nodes.snapshot();
		assert!(snapshot.nodes[0].connections.is_empty());
		assert!(snapshot.nodes[0].last_connected_seconds_ago.is_some());

		// A new cost or credential replaces the dial: the new one starts before
		// the old one is dropped or after, and the history stays either way.
		let replacement = nodes.dial("https://peer.example/");
		replacement.observe(4, &observation(Some(moq_tokio::Transport::Quic)));
		drop(dial);
		let snapshot = nodes.snapshot();
		assert_eq!(snapshot.nodes.len(), 1);
		assert_eq!(
			snapshot.nodes[0].connections.len(),
			1,
			"dropping one dial keeps the other's session"
		);
		assert_eq!(snapshot.nodes[0].connections[0].id, 4);

		drop(replacement);
		let snapshot = nodes.snapshot();
		assert!(snapshot.nodes[0].connections.is_empty());
		assert!(snapshot.nodes[0].last_connected_seconds_ago.is_some());
	}

	#[test]
	fn retired_peers_drop_their_history() {
		let nodes = Nodes::default();
		let kept = nodes.dial("https://kept.example/");
		let retired = nodes.dial("https://retired.example/");
		retired.observe(1, &observation(Some(moq_tokio::Transport::Quic)));

		drop(retired);
		nodes.retain(|peer| peer == "https://kept.example/");
		let snapshot = nodes.snapshot();
		assert_eq!(snapshot.nodes.len(), 1);
		assert_eq!(snapshot.nodes[0].node, "https://kept.example/");

		// A dial cancelled after its peer retired records nothing.
		kept.observe(2, &observation(Some(moq_tokio::Transport::Quic)));
		nodes.retain(|_| false);
		kept.observe(3, &observation(Some(moq_tokio::Transport::Quic)));
		assert!(nodes.snapshot().nodes.is_empty());
	}

	#[test]
	fn identities_sharing_a_label_merge() {
		let nodes = Nodes::default();
		let first = nodes.dial("https://a@peer.example/");
		let second = nodes.dial("https://b@peer.example/");
		second.observe(7, &observation(Some(moq_tokio::Transport::Quic)));
		first.observe(3, &observation(Some(moq_tokio::Transport::Quic)));

		let snapshot = nodes.snapshot();
		assert_eq!(snapshot.nodes.len(), 1);
		let ids: Vec<u64> = snapshot.nodes[0].connections.iter().map(|c| c.id).collect();
		assert_eq!(ids, [3, 7]);
	}
}
