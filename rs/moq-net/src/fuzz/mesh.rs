//! A relay's lite-07 route node between routing neighbours, serving what it hears into a
//! real origin as a session does, for the routes bench.
//!
//! The relay hears every forwarding node behind it from each upstream neighbour, the
//! first cheapest, and tells each downstream neighbour every route and each watcher every
//! claim. An event returns a [`RouteTally`] of what it cost the relay.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::Path;
use crate::route::NodeId;
use crate::route::node::{self, Msg, Output, Stream};
use crate::route::relay::{Change, Edit, Hop, Offer, Port, Role};

use super::{route_ask, route_claim, route_clock, route_id, route_leaf};

/// The shape of a [`RouteMesh`].
#[derive(Clone, Copy, Debug, Default)]
pub struct RouteShape {
	/// Forwarding nodes behind the upstream neighbours, each offered by every one.
	pub relays: usize,
	/// Claims each forwarding node holds.
	pub broadcasts: usize,
	/// Routing neighbours offering a route to every forwarding node, the first cheapest.
	pub upstream: usize,
	/// Routing neighbours asking for every route.
	pub downstream: usize,
	/// Endpoint sessions asking for the whole tree.
	pub watchers: usize,
}

/// What one event cost the relay, by what it sent and wrote.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RouteTally {
	/// Routes, route ends, DEAD and DRAIN sent to the downstream neighbours.
	pub routes: usize,
	/// Claim changes sent to the downstream neighbours.
	pub claims: usize,
	/// Claim changes told to the watchers, each an ANNOUNCE_* message.
	pub told: usize,
	/// Writes into the origin: a claim served, served differently, or let go, each one a
	/// closing session marks unavailable as it leaves it behind, and each such orphan the
	/// node releases.
	pub writes: usize,
}

impl std::ops::AddAssign for RouteTally {
	fn add_assign(&mut self, other: Self) {
		self.routes += other.routes;
		self.claims += other.claims;
		self.told += other.told;
		self.writes += other.writes;
	}
}

/// A claim the origin holds through one session, and the route it was last given.
struct Held {
	claim: crate::origin::Claim,
	route: crate::origin::RouteInfo,
}

type ServedKey = (NodeId, Path, crate::Instance);

/// One upstream session and what it serves into the origin.
struct Upstream {
	stream: Stream,
	served: HashMap<ServedKey, Held>,
}

/// A claim a closed session left behind, held until the node lets it go, which counts as
/// a write into the origin.
struct Left(crate::origin::Claim, Arc<AtomicUsize>);

impl node::Orphan for Left {
	fn release(self: Box<Self>, ended: bool) {
		self.1.fetch_add(1, Ordering::Relaxed);
		if ended {
			self.0.withdrawn();
		}
	}
}

/// A relay's route node shaped as a [`RouteShape`], settled, with the origin it serves.
pub struct RouteMesh {
	// Dropped before the node: a stream disconnects from its node on drop.
	up: Vec<Option<Upstream>>,
	down: Vec<Stream>,
	watchers: Vec<Stream>,
	node: node::Node,
	origin: crate::origin::Producer,
	_driver: crate::origin::Driver,
	/// Orphans the node released since the last [`Self::collect`].
	released: Arc<AtomicUsize>,
	shape: RouteShape,
	instance: crate::Instance,
	next_tag: u64,
	/// The middle hop every route takes, which [`Self::repath`] moves.
	via: u64,
	/// Whether the relay files its own claim as one that crossed a marked link.
	marked: bool,
}

impl RouteMesh {
	/// The middle hop of every route.
	const VIA: u64 = 500;

	/// A relay shaped as `shape`, everything heard, told, and served.
	pub fn new(shape: RouteShape) -> Self {
		let (origin, driver) = crate::origin::Producer::new(crate::origin::Config::default());
		let mut mesh = Self {
			up: Vec::new(),
			down: Vec::new(),
			watchers: Vec::new(),
			node: node::Node::new(route_id(2), true, route_clock()),
			origin,
			_driver: driver,
			released: Arc::default(),
			shape,
			instance: crate::Instance::random(),
			next_tag: 1,
			via: Self::VIA,
			marked: false,
		};
		for j in 0..shape.downstream {
			let port = Port {
				cost: 1,
				priority: 0,
				upstream: false,
				role: Role::Routing(route_id(300 + j as u64)),
			};
			let tag = mesh.tag();
			let stream = mesh.node.connect(tag, port).unwrap();
			stream.recv(Msg::Edited).unwrap();
			stream.recv(route_ask(Path::empty())).unwrap();
			stream.flush();
			mesh.down.push(stream);
		}
		for _ in 0..shape.watchers {
			let tag = mesh.tag();
			let stream = mesh.node.connect(tag, route_leaf()).unwrap();
			stream.recv(route_ask(Path::empty())).unwrap();
			stream.flush();
			mesh.watchers.push(stream);
		}
		for i in 0..shape.upstream {
			mesh.open(i);
		}
		mesh
	}

	fn tag(&mut self) -> u64 {
		self.next_tag += 1;
		self.next_tag
	}

	/// The forwarding node `r` behind the upstream neighbours.
	fn relay(r: usize) -> NodeId {
		route_id(1_000 + r as u64)
	}

	/// The upstream neighbour `i`.
	fn upstream(i: usize) -> NodeId {
		route_id(100 + i as u64)
	}

	/// What upstream neighbour `i` offers of forwarding node `r`.
	fn offer(&self, i: usize, r: usize) -> Offer {
		let path = [
			Hop {
				node: Self::relay(r),
				session: 0,
			},
			Hop {
				node: route_id(self.via),
				session: 1,
			},
			Hop {
				node: Self::upstream(i),
				session: r as u64 + 1,
			},
		];
		Offer {
			path: path.into(),
			draining: false,
			priority: i8::MAX,
			cost: 2,
			marked: false,
		}
	}

	/// Upstream neighbour `i` connects and offers every route with its claims.
	pub fn open(&mut self, i: usize) -> RouteTally {
		let port = Port {
			cost: 1 + i as u64,
			priority: 0,
			upstream: false,
			role: Role::Routing(Self::upstream(i)),
		};
		let tag = self.tag();
		let stream = self.node.connect(tag, port).unwrap();
		// The neighbour answered what the relay asks it for.
		stream.recv(Msg::Edited).unwrap();
		for r in 0..self.shape.relays {
			let origin = Self::relay(r);
			let offer = Some(self.offer(i, r));
			stream.recv(Msg::Route { origin, offer }).unwrap();
			let changes = (0..self.shape.broadcasts)
				.map(|b| {
					let path = Path::new(&format!("live/{r}/{b}")).unwrap();
					((path, self.instance.clone()), Change::Live(route_claim(0, 0)))
				})
				.collect();
			stream
				.recv(Msg::Claims {
					origin,
					changes,
					floor: 1,
				})
				.unwrap();
		}
		stream.flush();
		if self.up.len() <= i {
			self.up.resize_with(i + 1, || None);
		}
		self.up[i] = Some(Upstream {
			stream,
			served: HashMap::new(),
		});
		self.collect()
	}

	/// Upstream neighbour `i`'s session closes, leaving what it served behind, as a
	/// session does.
	pub fn close(&mut self, i: usize) -> RouteTally {
		let mut tally = RouteTally::default();
		let up = self.up[i].take().expect("an open upstream");
		for ((origin, path, instance), held) in up.served {
			let route = held.route.with_available(false);
			held.claim.update(route).unwrap();
			tally.writes += 1;
			up.stream.orphan(
				origin,
				&(path, instance),
				Box::new(Left(held.claim, self.released.clone())),
			);
		}
		drop(up.stream);
		tally += self.collect();
		tally
	}

	/// Upstream neighbour `i` closes its session and opens another.
	pub fn reopen(&mut self, i: usize) -> RouteTally {
		let mut tally = self.close(i);
		tally += self.open(i);
		tally
	}

	/// Upstream neighbour `i` sends GOAWAY.
	pub fn goaway(&mut self, i: usize) -> RouteTally {
		self.up[i].as_ref().expect("an open upstream").stream.goaway();
		self.collect()
	}

	/// Upstream neighbour `i` moves every route onto another path at the same cost.
	pub fn repath(&mut self, i: usize) -> RouteTally {
		self.via = match self.via {
			Self::VIA => Self::VIA + 1,
			_ => Self::VIA,
		};
		let offers: Vec<_> = (0..self.shape.relays)
			.map(|r| (Self::relay(r), self.offer(i, r)))
			.collect();
		let stream = &self.up[i].as_ref().expect("an open upstream").stream;
		for (origin, offer) in offers {
			stream
				.recv(Msg::Route {
					origin,
					offer: Some(offer),
				})
				.unwrap();
		}
		stream.flush();
		self.collect()
	}

	/// The first watcher swaps the whole tree for the first forwarding node's prefix, or
	/// back.
	pub fn swap(&mut self, narrow: bool) -> RouteTally {
		let (whole, part) = (Path::empty(), Path::new("live/0").unwrap());
		let edit = match narrow {
			true => Edit {
				remove: vec![whole],
				add: vec![part],
			},
			false => Edit {
				remove: vec![part],
				add: vec![whole],
			},
		};
		let watcher = self.watchers.first().expect("a watcher");
		watcher.recv(Msg::Ask(edit)).unwrap();
		watcher.flush();
		self.collect()
	}

	/// The relay claims `claims` more broadcasts of its own, beside the one
	/// [`Self::remark`] moves.
	pub fn own(&mut self, claims: usize) -> RouteTally {
		for c in 1..=claims {
			let broadcast = (Path::new(&format!("own/{c}")).unwrap(), self.instance.clone());
			self.node.own(false, broadcast, Change::Live(route_claim(0, 0)));
		}
		self.collect()
	}

	/// The relay's own claim moves between its two identities: filed as having crossed a
	/// marked link, or not.
	pub fn remark(&mut self) -> RouteTally {
		self.marked = !self.marked;
		let broadcast = (Path::new("own/0").unwrap(), self.instance.clone());
		self.node.own(self.marked, broadcast, Change::Live(route_claim(0, 0)));
		self.collect()
	}

	/// Take everything the relay queued, serving into the origin what it asks to serve
	/// and confirming each, until nothing more is queued.
	fn collect(&mut self) -> RouteTally {
		let mut tally = RouteTally::default();
		for stream in &self.down {
			while let Some(msg) = stream.try_wire() {
				match msg {
					Msg::Claims { changes, .. } => tally.claims += changes.len(),
					Msg::Route { .. } | Msg::Dead { .. } | Msg::Drain { .. } => tally.routes += 1,
					_ => {}
				}
			}
		}
		for stream in &self.watchers {
			while let Some(msg) = stream.try_wire() {
				if let Msg::Announce { changes } = msg {
					tally.told += changes.len();
				}
			}
		}
		loop {
			let mut wrote = 0;
			for up in self.up.iter_mut().flatten() {
				while up.stream.try_wire().is_some() {}
				while let Some(output) = up.stream.try_serve() {
					wrote += 1;
					Self::apply(&self.origin, up, output);
				}
			}
			if wrote == 0 {
				break;
			}
			tally.writes += wrote;
		}
		tally.writes += self.released.swap(0, Ordering::Relaxed);
		tally
	}

	/// Serve `output` into the origin as a session does, and confirm it.
	fn apply(origin: &crate::origin::Producer, up: &mut Upstream, output: Output) {
		match output {
			Output::Serve {
				origin: node,
				path,
				instance,
				served,
			} => {
				let lane = crate::origin::Lane {
					primary: served.primary,
					priority: Some(served.priority),
					above: u8::try_from(served.attr.above).unwrap_or(u8::MAX),
				};
				let route = crate::origin::RouteInfo::new()
					.with_instance(instance.clone(), served.attr.epoch)
					.with_cost(served.cost)
					.with_node(Some(node))
					.with_lane(lane)
					.with_draining(served.draining)
					.with_available(served.available)
					.with_marked(served.marked);
				let key = (node, path, instance);
				match up.served.get_mut(&key) {
					Some(held) => {
						held.claim.update(route.clone()).unwrap();
						held.route = route;
					}
					None => {
						let claim = origin.forward(key.1.clone(), route.clone()).unwrap();
						up.served.insert(key.clone(), Held { claim, route });
					}
				}
				let (node, path, instance) = key;
				up.stream.serving(node, &(path, instance), true);
			}
			Output::Unserve {
				origin: node,
				path,
				instance,
				ended,
			} => {
				if let Some(held) = up.served.remove(&(node, path, instance))
					&& ended
				{
					held.claim.withdrawn();
				}
			}
		}
	}
}
