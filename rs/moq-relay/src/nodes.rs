//! The local cluster view behind the internal `/nodes` endpoint.
//!
//! Lists the peers this relay dialed and currently holds a session with, keyed
//! by the URL it dialed. Accepted peer sessions are not listed: a peer declares
//! no URL, so there is nothing to name it by.

use std::{
	collections::{BTreeMap, HashMap},
	sync::{Arc, Mutex},
};

use serde::Serialize;

/// Established outbound cluster connections, by `conn` id.
#[derive(Clone, Default)]
pub(crate) struct Nodes {
	connections: Arc<Mutex<HashMap<u64, String>>>,
}

/// The JSON document returned by the internal `/nodes` endpoint.
#[derive(Debug, Default, Serialize)]
pub(crate) struct Snapshot {
	/// Dialed cluster nodes with a live session.
	pub nodes: Vec<Node>,
}

/// One dialed cluster node.
#[derive(Debug, Serialize)]
pub(crate) struct Node {
	/// Canonical URL this relay dialed, without its query.
	pub node: String,
	/// Established connections to this node.
	pub connections: Vec<Connection>,
}

/// An established outbound cluster connection.
#[derive(Debug, Serialize)]
pub(crate) struct Connection {
	/// The session's `conn` id, matching the `conn{id}` span its log lines carry.
	/// Process-local, and only meaningful while the session lasts.
	pub id: u64,
}

/// Removes a live connection from the view when its session ends.
pub(crate) struct ConnectionGuard {
	nodes: Nodes,
	id: u64,
}

impl Nodes {
	/// Record a dial this relay initiated, keyed by the URL it dialed.
	///
	/// `id` is the session's `conn` id from
	/// [`Cluster::next_connection_id`](crate::cluster::Cluster::next_connection_id).
	pub(crate) fn connect_outbound(&self, id: u64, node: impl Into<String>) -> ConnectionGuard {
		self.connections
			.lock()
			.expect("node connection registry poisoned")
			.insert(id, node.into());
		ConnectionGuard {
			nodes: self.clone(),
			id,
		}
	}

	pub(crate) fn snapshot(&self) -> Snapshot {
		let mut nodes = BTreeMap::<String, Vec<Connection>>::new();
		let connections = self.connections.lock().expect("node connection registry poisoned");
		for (&id, node) in connections.iter() {
			nodes
				.entry(crate::cluster::canonicalize_peer_key(node))
				.or_default()
				.push(Connection { id });
		}

		Snapshot {
			nodes: nodes
				.into_iter()
				.map(|(node, mut connections)| {
					connections.sort_by_key(|connection| connection.id);
					Node { node, connections }
				})
				.collect(),
		}
	}
}

impl Drop for ConnectionGuard {
	fn drop(&mut self) {
		self.nodes
			.connections
			.lock()
			.expect("node connection registry poisoned")
			.remove(&self.id);
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn snapshot_groups_live_connections_by_node() {
		let nodes = Nodes::default();
		let _second = nodes.connect_outbound(1, "https://relay-b.example/");
		let _first = nodes.connect_outbound(0, "https://relay-b.example");
		let _other = nodes.connect_outbound(2, "https://relay-a.example/");

		assert_eq!(
			serde_json::to_value(nodes.snapshot()).unwrap(),
			serde_json::json!({
				"nodes": [
					{ "node": "https://relay-a.example/", "connections": [{ "id": 2 }] },
					{ "node": "https://relay-b.example/", "connections": [{ "id": 0 }, { "id": 1 }] }
				]
			}),
		);
	}

	#[test]
	fn snapshot_stops_reporting_closed_outbound_connections() {
		let nodes = Nodes::default();
		let connection = nodes.connect_outbound(0, "https://relay-b.example/");
		assert_eq!(nodes.snapshot().nodes.len(), 1);

		drop(connection);
		assert!(nodes.snapshot().nodes.is_empty());
	}

	#[test]
	fn outbound_node_omits_credentials_from_url() {
		let nodes = Nodes::default();
		let _connection = nodes.connect_outbound(0, "https://relay-b.example/?jwt=secret");

		assert_eq!(nodes.snapshot().nodes[0].node, "https://relay-b.example/");
	}
}
