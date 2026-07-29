use std::{
	collections::{BTreeMap, HashMap},
	sync::{
		Arc, Mutex,
		atomic::{AtomicU64, Ordering},
	},
};

use moq_net::{Origin, origin};
use serde::Serialize;

/// Namespace containing the empty broadcasts used to advertise cluster nodes.
pub(crate) const MESH_PREFIX: &str = ".internal/origins";

/// A local view of announced cluster nodes and established direct connections.
#[derive(Clone)]
pub(crate) struct Nodes {
	origin: origin::Producer,
	connections: Arc<Connections>,
}

#[derive(Default)]
struct Connections {
	next_id: AtomicU64,
	entries: Mutex<HashMap<u64, ConnectionRecord>>,
}

#[derive(Clone)]
struct ConnectionRecord {
	direction: Direction,
	target: ConnectionTarget,
}

#[derive(Clone)]
enum ConnectionTarget {
	Node(String),
	Origin(Origin),
}

/// The JSON document returned by the internal `/nodes` endpoint.
#[derive(Debug, Default, Serialize)]
pub(crate) struct Snapshot {
	/// Announced or directly connected cluster nodes.
	pub nodes: Vec<Node>,
}

/// One known cluster node in the local topology view.
#[derive(Debug, Serialize)]
pub(crate) struct Node {
	/// Canonical URL advertised for the node.
	pub node: String,
	/// Origin identity from the node's announcement, when available.
	pub origin_id: Option<String>,
	/// Selected route for the node's internal advertisement.
	pub announced: Option<Announcement>,
	/// Established direct connections to this node.
	pub connections: Vec<Connection>,
}

/// The selected route for a node advertisement.
#[derive(Debug, Serialize)]
pub(crate) struct Announcement {
	/// Origin identities in traversal order, oldest first.
	pub hops: Vec<String>,
	/// Number of hops in the selected route.
	pub hop_count: usize,
	/// Accumulated cost of the selected route.
	pub cost: u64,
}

/// An established direct cluster connection.
#[derive(Debug, Serialize)]
pub(crate) struct Connection {
	/// Process-local connection identifier.
	pub id: u64,
	/// Whether this relay accepted or initiated the connection.
	pub direction: Direction,
}

/// Direction of an established cluster connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Direction {
	/// A connection accepted from another relay.
	Inbound,
	/// A connection initiated by this relay.
	Outbound,
}

struct NodeBuilder {
	origin_id: Option<String>,
	announced: Option<Announcement>,
	connections: Vec<Connection>,
}

/// Removes a live connection from the topology view when its session ends.
pub(crate) struct ConnectionGuard {
	nodes: Nodes,
	id: u64,
}

impl Nodes {
	pub(crate) fn new(origin: origin::Producer) -> Self {
		Self {
			origin,
			connections: Arc::default(),
		}
	}

	pub(crate) fn with_origin(mut self, origin: origin::Producer) -> Self {
		self.origin = origin;
		self
	}

	pub(crate) fn connect_outbound(&self, node: impl Into<String>) -> ConnectionGuard {
		self.connect(Direction::Outbound, ConnectionTarget::Node(node.into()))
	}

	pub(crate) fn connect_inbound(&self, origin: Origin) -> ConnectionGuard {
		self.connect(Direction::Inbound, ConnectionTarget::Origin(origin))
	}

	fn connect(&self, direction: Direction, target: ConnectionTarget) -> ConnectionGuard {
		let id = self.connections.next_id.fetch_add(1, Ordering::Relaxed);
		self.connections
			.entries
			.lock()
			.expect("node connection registry poisoned")
			.insert(id, ConnectionRecord { direction, target });
		ConnectionGuard {
			nodes: self.clone(),
			id,
		}
	}

	pub(crate) fn snapshot(&self) -> Snapshot {
		let mut nodes = BTreeMap::<String, NodeBuilder>::new();
		let mut origins = HashMap::<u64, Option<String>>::new();

		if let Some(consumer) = self.origin.consume().with_root(MESH_PREFIX) {
			let mut announced = consumer.announced();
			while let Some(moq_net::announce::Update {
				path,
				broadcast: Some(broadcast),
			}) = announced.try_next()
			{
				let key = canonical_announced_node(path.as_str());
				let route = broadcast.route();
				let hop_ids = route.hops.iter().map(|origin| origin.id()).collect::<Vec<_>>();
				let origin_id = hop_ids.first().copied().unwrap_or_else(|| self.origin.id());

				origins
					.entry(origin_id)
					.and_modify(|current| {
						if current.as_deref() != Some(&key) {
							*current = None;
						}
					})
					.or_insert_with(|| Some(key.clone()));

				nodes.insert(
					key,
					NodeBuilder {
						origin_id: Some(origin_id.to_string()),
						announced: Some(Announcement {
							hop_count: hop_ids.len(),
							hops: hop_ids.into_iter().map(|origin| origin.to_string()).collect(),
							cost: route.cost,
						}),
						connections: Vec::new(),
					},
				);
			}
		}

		let connections = self
			.connections
			.entries
			.lock()
			.expect("node connection registry poisoned");
		let mut connections = connections.iter().collect::<Vec<_>>();
		connections.sort_by_key(|(id, _)| **id);
		for (&id, connection) in connections {
			let key = match &connection.target {
				ConnectionTarget::Node(node) => canonical_node(node),
				ConnectionTarget::Origin(origin) => {
					let Some(Some(node)) = origins.get(&origin.id()) else {
						continue;
					};
					node.clone()
				}
			};
			let node = nodes.entry(key).or_insert_with(|| NodeBuilder {
				origin_id: None,
				announced: None,
				connections: Vec::new(),
			});
			node.connections.push(Connection {
				id,
				direction: connection.direction,
			});
		}

		Snapshot {
			nodes: nodes
				.into_iter()
				.map(|(key, node)| Node {
					node: key,
					origin_id: node.origin_id,
					announced: node.announced,
					connections: node.connections,
				})
				.collect(),
		}
	}
}

impl Drop for ConnectionGuard {
	fn drop(&mut self) {
		self.nodes
			.connections
			.entries
			.lock()
			.expect("node connection registry poisoned")
			.remove(&self.id);
	}
}

fn canonical_node(node: &str) -> String {
	crate::cluster::canonicalize_peer_key(node)
}

fn canonical_announced_node(node: &str) -> String {
	canonical_node(&crate::cluster::advertised_node_url(node))
}

#[cfg(test)]
mod tests {
	use super::*;
	use moq_net::{Origin, OriginList, broadcast};

	async fn announced_node(
		origin: &moq_net::origin::Producer,
		node: &str,
		hops: &[u64],
		cost: u64,
	) -> broadcast::Producer {
		let hops = OriginList::try_from(
			hops.iter()
				.map(|id| Origin::new(*id).expect("valid test origin"))
				.collect::<Vec<_>>(),
		)
		.unwrap();
		let path = moq_net::Path::new(MESH_PREFIX).join(node);
		let broadcast = origin
			.create_broadcast(&path, broadcast::Route::announced().with_hops(hops).with_cost(cost))
			.unwrap();
		origin
			.consume()
			.announced_broadcast(&path)
			.await
			.expect("test node announced");
		broadcast
	}

	#[tokio::test]
	async fn snapshot_combines_announcements_and_live_connections() {
		const REMOTE_ID: u64 = 9_007_199_254_740_993;

		let origin = Origin::new(100).unwrap().produce();
		let nodes = Nodes::new(origin.clone());
		let _remote = announced_node(&origin, "https://relay-b.example/", &[REMOTE_ID], 7).await;
		let _outbound = nodes.connect_outbound("https://relay-b.example/");
		let _inbound = nodes.connect_inbound(Origin::new(REMOTE_ID).unwrap());

		let snapshot = nodes.snapshot();
		assert_eq!(snapshot.nodes.len(), 1);
		let node = &snapshot.nodes[0];
		assert_eq!(node.node, "https://relay-b.example/");
		assert_eq!(node.origin_id.as_deref(), Some("9007199254740993"));
		assert_eq!(node.announced.as_ref().unwrap().hops, vec!["9007199254740993"]);
		assert_eq!(node.announced.as_ref().unwrap().hop_count, 1);
		assert_eq!(node.announced.as_ref().unwrap().cost, 7);
		assert_eq!(node.connections.len(), 2);
		assert_eq!(node.connections[0].direction, Direction::Outbound);
		assert_eq!(node.connections[1].direction, Direction::Inbound);
		assert_eq!(
			serde_json::to_value(&snapshot).unwrap(),
			serde_json::json!({
				"nodes": [{
					"node": "https://relay-b.example/",
					"origin_id": "9007199254740993",
					"announced": { "hops": ["9007199254740993"], "hop_count": 1, "cost": 7 },
					"connections": [
						{ "id": 0, "direction": "outbound" },
						{ "id": 1, "direction": "inbound" }
					]
				}]
			}),
		);
	}

	#[test]
	fn snapshot_omits_unresolved_inbound_connections() {
		let origin = Origin::new(100).unwrap().produce();
		let nodes = Nodes::new(origin);
		let _inbound = nodes.connect_inbound(Origin::new(200).unwrap());

		assert!(nodes.snapshot().nodes.is_empty());
	}

	#[test]
	fn snapshot_stops_reporting_closed_outbound_connections() {
		let origin = Origin::new(100).unwrap().produce();
		let nodes = Nodes::new(origin);
		let connection = nodes.connect_outbound("https://relay-b.example/");
		assert_eq!(nodes.snapshot().nodes.len(), 1);

		drop(connection);
		assert!(nodes.snapshot().nodes.is_empty());
	}

	#[test]
	fn outbound_node_omits_credentials_from_url() {
		let origin = Origin::new(100).unwrap().produce();
		let nodes = Nodes::new(origin);
		let _connection = nodes.connect_outbound("https://relay-b.example/?jwt=secret");

		assert_eq!(nodes.snapshot().nodes[0].node, "https://relay-b.example/");
	}

	#[tokio::test]
	async fn duplicate_origin_ids_do_not_resolve_inbound_connections() {
		let origin = Origin::new(100).unwrap().produce();
		let nodes = Nodes::new(origin.clone());
		let _first = announced_node(&origin, "https://relay-b.example/", &[200], 1).await;
		let _second = announced_node(&origin, "https://relay-c.example/", &[200], 1).await;
		let _inbound = nodes.connect_inbound(Origin::new(200).unwrap());

		let snapshot = nodes.snapshot();
		assert_eq!(snapshot.nodes.len(), 2);
		assert!(snapshot.nodes.iter().all(|node| node.connections.is_empty()));
	}
}
