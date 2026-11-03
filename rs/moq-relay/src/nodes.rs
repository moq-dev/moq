//! Outbound peer state behind the internal `/nodes` endpoint.
//! Entries live as long as their configured dial, including failed attempts.

use serde::Serialize;
use std::{
	collections::BTreeMap,
	sync::{Arc, Mutex, PoisonError},
};

#[derive(Clone, Default)]
pub(crate) struct Nodes {
	peers: Arc<Mutex<BTreeMap<u64, Peer>>>,
}

struct Peer {
	node: String,
	connection: Option<Connection>,
	last_connected: Option<kio::clock::Instant>,
}

#[derive(Debug, Default, Serialize)]
pub(crate) struct Snapshot {
	pub nodes: Vec<Node>,
}

#[derive(Debug, Serialize)]
pub(crate) struct Node {
	/// Canonical dial URL with credentials removed.
	pub node: String,
	pub connections: Vec<Connection>,
	/// Age of the latest successful handshake, including sessions already closed.
	/// Null means this configured dial has never connected in this process.
	pub last_connected_seconds_ago: Option<f64>,
}

#[serde_with::serde_as]
#[derive(Clone, Debug, Serialize)]
pub(crate) struct Connection {
	pub id: u64,
	#[serde_as(as = "serde_with::DisplayFromStr")]
	pub transport: moq_tokio::Transport,
}

/// Removing a configured dial also removes its history.
pub(crate) struct Dial {
	nodes: Nodes,
	id: u64,
}

impl Nodes {
	pub(crate) fn dial(&self, id: u64, node: &str) -> Dial {
		self.peers.lock().unwrap_or_else(PoisonError::into_inner).insert(
			id,
			Peer {
				node: crate::cluster::canonicalize_peer_key(node),
				connection: None,
				last_connected: None,
			},
		);
		Dial {
			nodes: self.clone(),
			id,
		}
	}

	pub(crate) fn snapshot(&self) -> Snapshot {
		let peers = self.peers.lock().unwrap_or_else(PoisonError::into_inner);
		let mut nodes = BTreeMap::<String, Node>::new();
		for peer in peers.values() {
			let age = peer.last_connected.map(|at| at.elapsed().as_secs_f64());
			let node = nodes.entry(peer.node.clone()).or_insert_with(|| Node {
				node: peer.node.clone(),
				connections: Vec::new(),
				last_connected_seconds_ago: None,
			});
			if let Some(age) = age {
				node.last_connected_seconds_ago =
					Some(node.last_connected_seconds_ago.map_or(age, |prev| prev.min(age)));
			}
			if let Some(connection) = &peer.connection {
				node.connections.push(connection.clone());
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
	pub(crate) fn observe(&self, id: u64, observation: &moq_tokio::connection::Observation) {
		let mut peers = self.nodes.peers.lock().unwrap_or_else(PoisonError::into_inner);
		let peer = peers.get_mut(&self.id).expect("dial guard owns registration");
		if let Some(at) = observation.last_connected {
			peer.last_connected = Some(at);
		}
		peer.connection = observation.transport.map(|transport| Connection { id, transport });
	}

	pub(crate) fn disconnected(&self) {
		self.nodes
			.peers
			.lock()
			.unwrap_or_else(PoisonError::into_inner)
			.get_mut(&self.id)
			.expect("dial guard owns registration")
			.connection = None;
	}
}

impl Drop for Dial {
	fn drop(&mut self) {
		self.nodes
			.peers
			.lock()
			.unwrap_or_else(PoisonError::into_inner)
			.remove(&self.id);
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn configured_peers_survive_failures_and_retire_with_their_dial() {
		let nodes = Nodes::default();
		let dial = nodes.dial(1, "https://peer.example/?jwt=secret");
		assert_eq!(nodes.snapshot().nodes[0].node, "https://peer.example/");
		assert!(nodes.snapshot().nodes[0].connections.is_empty());
		assert!(nodes.snapshot().nodes[0].last_connected_seconds_ago.is_none());
		let mut observation = moq_tokio::connection::Observation {
			revision: 1,
			phase: moq_tokio::connection::Phase::Connected,
			connects: 1,
			failure: None,
			version: None,
			last_connected: Some(kio::clock::now()),
			transport: Some(moq_tokio::Transport::WebSocket),
		};
		dial.observe(2, &observation);
		assert_eq!(
			nodes.snapshot().nodes[0].connections[0].transport,
			moq_tokio::Transport::WebSocket
		);
		// A connect and disconnect may both happen before the observer polls.
		observation.transport = None;
		observation.phase = moq_tokio::connection::Phase::Disconnected;
		dial.observe(2, &observation);
		dial.disconnected();
		assert!(nodes.snapshot().nodes[0].connections.is_empty());
		assert!(nodes.snapshot().nodes[0].last_connected_seconds_ago.is_some());
		drop(dial);
		assert!(nodes.snapshot().nodes.is_empty());
	}
}
