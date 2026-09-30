//! The relay graph a cluster shares, kept apart from routes.
//!
//! Every relay reports the liveness and cost of each of its own links, and its
//! cluster sessions flood those reports (moq-lite-07+, on the Topology stream).
//! So every relay learns every link once, however many routes cross it, and
//! computes its shortest path to every other relay from a [`Graph`].
//!
//! Build one [`Database`] per relay and attach it to each cluster session with
//! [`Client::with_topology`](crate::Client::with_topology) or
//! [`Server::with_topology`](crate::Server::with_topology). A session that
//! negotiates an older version, or moq-transport, takes no part.

use std::{
	cmp::Reverse,
	collections::{BTreeMap, BinaryHeap, HashMap, btree_map},
	sync::{Arc, Mutex},
	task::Poll,
	time::Duration,
};

use crate::Hop;

/// How long a session holds the reports it sends, its own and those it
/// forwards, so a burst of changes (a relay restarting drops and restores every
/// one of its links) crosses each session as one message rather than one each.
pub(crate) const HOLD_DOWN: Duration = Duration::from_millis(50);

/// A link as its reporter last described it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
	/// Orders the reporter's reports of this link within one incarnation.
	pub seq: u64,
	/// The link's cost while it is up, `None` once it is down.
	pub cost: Option<u64>,
}

/// Everything one relay reported about its links, scoped to its incarnation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Reported<T> {
	pub incarnation: u64,
	pub links: BTreeMap<u64, T>,
}

/// Link reports by reporting relay: the unit sessions exchange.
pub(crate) type Reports = BTreeMap<u64, Reported<Entry>>;

/// What one relay already holds: each reporter's incarnation and the sequence of
/// each link it reported, so the peer sends only what is missing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Digest {
	/// The sender's own id, which must match the Hop ID in its SETUP.
	pub node: u64,
	pub reporters: BTreeMap<u64, Reported<u64>>,
}

/// One relay's link-state database, shared by all of its cluster sessions.
///
/// Cheap to clone: every clone is the same database.
#[derive(Clone)]
pub struct Database {
	state: kio::Shared<State>,
	/// The last graph computed, tagged with the generation it reflects. Kept apart
	/// from `state` so reading the graph never wakes a parked session.
	graph: Arc<Mutex<Option<(u64, Graph)>>>,
}

impl std::fmt::Debug for Database {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		let state = self.state.read();
		f.debug_struct("Database")
			.field("node", &state.node)
			.field("incarnation", &state.incarnation)
			.field("reporters", &state.reporters.len())
			.field("ports", &state.ports.len())
			.finish()
	}
}

struct State {
	node: u64,
	incarnation: u64,
	/// Every relay's latest reports, this one's included.
	reporters: BTreeMap<u64, Reported<Entry>>,
	/// The cost of each live session to a peer, by peer: a link is up while any
	/// session to it is, at the cheapest of their costs.
	sessions: BTreeMap<u64, Vec<u64>>,
	/// Synced sessions, each with the reports it has yet to send.
	ports: HashMap<u64, Outbox>,
	next_port: u64,
	/// Bumped on every change to `reporters`, so a cached graph knows it is stale.
	generation: u64,
}

struct Outbox {
	/// The neighbour this session reaches, which never needs its own reports back.
	peer: u64,
	pending: Reports,
}

impl Database {
	/// A database for the relay `node`, starting its `incarnation`.
	///
	/// `incarnation` must grow each time the same node restarts, so its new
	/// reports supersede the ones its last run left behind instead of looking
	/// older. The start time in milliseconds since the Unix epoch works.
	pub fn new(node: Hop, incarnation: u64) -> Self {
		let state = State {
			node: node.id(),
			incarnation,
			reporters: BTreeMap::new(),
			sessions: BTreeMap::new(),
			ports: HashMap::new(),
			next_port: 0,
			generation: 0,
		};
		Self {
			state: kio::Shared::new(state),
			graph: Arc::default(),
		}
	}

	/// This relay's id.
	pub(crate) fn node(&self) -> Hop {
		hop(self.state.read().node)
	}

	/// Every known relay, its links, and this relay's shortest path to it.
	pub fn graph(&self) -> Graph {
		let state = self.state.read();
		let mut cached = self.graph.lock().expect("topology graph poisoned");
		if let Some((generation, graph)) = cached.as_ref()
			&& *generation == state.generation
		{
			return graph.clone();
		}
		let graph = state.graph();
		*cached = Some((state.generation, graph.clone()));
		graph
	}

	/// What this relay holds, for a session that just opened.
	pub(crate) fn digest(&self) -> Digest {
		let state = self.state.read();
		Digest {
			node: state.node,
			reporters: state
				.reporters
				.iter()
				.map(|(node, reported)| {
					let links = reported.links.iter().map(|(peer, entry)| (*peer, entry.seq)).collect();
					(
						*node,
						Reported {
							incarnation: reported.incarnation,
							links,
						},
					)
				})
				.collect(),
		}
	}

	/// Bring up a link to `peer` once both sides exchanged digests, queueing
	/// everything the peer's `digest` lacks for the returned port.
	pub(crate) fn attach(&self, peer: u64, cost: u64, digest: &Digest) -> Neighbor {
		let mut state = self.state.lock();
		state.sessions.entry(peer).or_default().push(cost);
		// Report the link first, so the diff below already counts it: sending the
		// database first would replay our report from when the link went down,
		// and the peer would drop the link it is using.
		state.report(peer);
		let pending = state.diff(digest);
		let id = state.next_port;
		state.next_port += 1;
		state.ports.insert(id, Outbox { peer, pending });
		Neighbor {
			database: self.clone(),
			id,
			peer,
			cost,
		}
	}
}

impl State {
	/// Re-report our link to `peer` if its liveness or cost changed.
	fn report(&mut self, peer: u64) {
		let cost = self.sessions.get(&peer).and_then(|costs| costs.iter().min().copied());
		let own = self.reporters.entry(self.node).or_insert_with(|| Reported {
			incarnation: self.incarnation,
			links: BTreeMap::new(),
		});
		let seq = match own.links.get(&peer) {
			Some(entry) if entry.cost == cost => return,
			Some(entry) => entry.seq + 1,
			// Nothing to withdraw from a link we never reported.
			None if cost.is_none() => return,
			None => 1,
		};
		let entry = Entry { seq, cost };
		own.links.insert(peer, entry);
		let incarnation = own.incarnation;
		self.generation += 1;
		let report = Reports::from([(
			self.node,
			Reported {
				incarnation,
				links: BTreeMap::from([(peer, entry)]),
			},
		)]);
		self.flood(None, &report);
	}

	/// Queue `reports` on every port but the one they arrived on, leaving out
	/// each neighbour's own reports: it is their authority and ignores them.
	fn flood(&mut self, from: Option<u64>, reports: &Reports) {
		for (id, port) in &mut self.ports {
			if Some(*id) == from {
				continue;
			}
			let peer = port.peer;
			match reports.contains_key(&peer) {
				true => {
					let others = reports
						.iter()
						.filter(|(node, _)| **node != peer)
						.map(|(node, reported)| (*node, reported.clone()))
						.collect();
					merge(&mut port.pending, &others);
				}
				false => merge(&mut port.pending, reports),
			}
		}
	}

	/// Keep the reports newer than what we hold and forward them.
	fn apply(&mut self, from: u64, reports: Reports) {
		let mut accepted = Reports::new();
		for (node, reported) in reports {
			if node == self.node {
				self.reported_self(reported.incarnation);
				continue;
			}
			let held = match self.reporters.entry(node) {
				btree_map::Entry::Vacant(vacant) => vacant.insert(Reported {
					incarnation: reported.incarnation,
					links: BTreeMap::new(),
				}),
				btree_map::Entry::Occupied(occupied) => occupied.into_mut(),
			};
			if reported.incarnation < held.incarnation {
				continue;
			}
			// A restarted relay's links supersede everything its last run reported.
			if reported.incarnation > held.incarnation {
				*held = Reported {
					incarnation: reported.incarnation,
					links: BTreeMap::new(),
				};
			}
			let mut changed = BTreeMap::new();
			for (peer, entry) in reported.links {
				if held.links.get(&peer).is_some_and(|old| old.seq >= entry.seq) {
					continue;
				}
				held.links.insert(peer, entry);
				changed.insert(peer, entry);
			}
			if !changed.is_empty() {
				accepted.insert(
					node,
					Reported {
						incarnation: reported.incarnation,
						links: changed,
					},
				);
			}
		}
		if !accepted.is_empty() {
			self.generation += 1;
			self.flood(Some(from), &accepted);
		}
	}

	/// A report claiming to be ours came back. Our own incarnation is simply an
	/// echo, but a greater one can only come from a previous run of this node
	/// whose clock was ahead: move past it and report every link afresh, or our
	/// reports would lose to its stale ones forever.
	fn reported_self(&mut self, incarnation: u64) {
		if incarnation <= self.incarnation {
			return;
		}
		tracing::warn!(
			node = self.node,
			ours = self.incarnation,
			theirs = incarnation,
			"a previous run of this relay reported a newer incarnation; moving past it"
		);
		self.incarnation = incarnation.saturating_add(1);
		let links: BTreeMap<u64, Entry> = self
			.sessions
			.iter()
			.map(|(peer, costs)| {
				let cost = costs.iter().min().copied();
				(*peer, Entry { seq: 1, cost })
			})
			.collect();
		let own = Reported {
			incarnation: self.incarnation,
			links,
		};
		self.reporters.insert(self.node, own.clone());
		self.generation += 1;
		self.flood(None, &Reports::from([(self.node, own)]));
	}

	/// Everything we hold that `digest` lacks, leaving out the peer's own reports:
	/// it is the authority on its links.
	fn diff(&self, digest: &Digest) -> Reports {
		let mut missing = Reports::new();
		for (node, reported) in &self.reporters {
			if *node == digest.node {
				continue;
			}
			let theirs = digest.reporters.get(node);
			let links: BTreeMap<u64, Entry> = match theirs {
				Some(theirs) if theirs.incarnation > reported.incarnation => continue,
				Some(theirs) if theirs.incarnation == reported.incarnation => reported
					.links
					.iter()
					.filter(|(peer, entry)| theirs.links.get(peer).is_none_or(|seq| *seq < entry.seq))
					.map(|(peer, entry)| (*peer, *entry))
					.collect(),
				_ => reported.links.clone(),
			};
			if !links.is_empty() {
				missing.insert(
					*node,
					Reported {
						incarnation: reported.incarnation,
						links,
					},
				);
			}
		}
		missing
	}

	/// Shortest paths from this relay, by cost and then hop count, over the links
	/// both ends report up.
	fn graph(&self) -> Graph {
		// Two-way check: a link counts only once both ends report it up, so a
		// relay that is gone, or has not yet seen the link, cannot attract paths.
		let up = |from: u64, to: u64| {
			self.reporters
				.get(&from)
				.and_then(|reported| reported.links.get(&to))
				.and_then(|entry| entry.cost)
		};

		// (distance, first hop) per settled relay. Equal distances break toward
		// the lowest first hop, then the lowest relay id, so every run agrees.
		let mut settled: BTreeMap<u64, (Distance, u64)> = BTreeMap::new();
		let mut queue = BinaryHeap::new();
		queue.push(Reverse((Distance { cost: 0, hops: 0 }, self.node, self.node)));
		while let Some(Reverse((distance, first, node))) = queue.pop() {
			if settled.contains_key(&node) {
				continue;
			}
			settled.insert(node, (distance, first));
			let Some(reported) = self.reporters.get(&node) else {
				continue;
			};
			for (peer, entry) in &reported.links {
				let Some(cost) = entry.cost else { continue };
				if settled.contains_key(peer) || up(*peer, node).is_none() {
					continue;
				}
				let next = Distance {
					cost: distance.cost.saturating_add(cost),
					hops: distance.hops + 1,
				};
				let first = match node == self.node {
					true => *peer,
					false => first,
				};
				queue.push(Reverse((next, first, *peer)));
			}
		}

		let mut nodes: BTreeMap<u64, Node> = BTreeMap::new();
		nodes.insert(
			self.node,
			Node {
				id: hop(self.node),
				incarnation: self.incarnation,
				distance: None,
				next: None,
				links: Vec::new(),
			},
		);
		for (node, reported) in &self.reporters {
			let entry = nodes.entry(*node).or_insert_with(|| Node {
				id: hop(*node),
				incarnation: reported.incarnation,
				distance: None,
				next: None,
				links: Vec::new(),
			});
			entry.incarnation = reported.incarnation;
			entry.links = reported
				.links
				.iter()
				.map(|(peer, link)| Link {
					peer: hop(*peer),
					cost: link.cost,
				})
				.collect();
		}
		for (node, (distance, first)) in settled {
			if let Some(entry) = nodes.get_mut(&node) {
				entry.distance = Some(distance);
				entry.next = (node != self.node).then(|| hop(first));
			}
		}

		Graph {
			node: hop(self.node),
			nodes: nodes.into_values().collect(),
		}
	}
}

/// Fold `reports` into `pending`, keeping only the newest per link.
fn merge(pending: &mut Reports, reports: &Reports) {
	for (node, reported) in reports {
		match pending.entry(*node) {
			btree_map::Entry::Vacant(vacant) => {
				vacant.insert(reported.clone());
			}
			btree_map::Entry::Occupied(mut occupied) => {
				let queued = occupied.get_mut();
				if reported.incarnation > queued.incarnation {
					*queued = reported.clone();
				} else if reported.incarnation == queued.incarnation {
					queued
						.links
						.extend(reported.links.iter().map(|(peer, entry)| (*peer, *entry)));
				}
			}
		}
	}
}

/// An id the database validated on the way in.
fn hop(id: u64) -> Hop {
	Hop::from_wire(id).expect("topology ids are validated on decode")
}

/// One session's link to a neighbour, up from [`Database::attach`] until dropped.
pub(crate) struct Neighbor {
	database: Database,
	id: u64,
	peer: u64,
	cost: u64,
}

impl Neighbor {
	/// Ready once reports are queued for this session.
	pub fn poll_pending(&self, waiter: &kio::Waiter) -> Poll<()> {
		let port = self.id;
		let _ready = std::task::ready!(self.database.state.poll(waiter, |state| {
			match state.ports.get(&port).is_some_and(|port| !port.pending.is_empty()) {
				true => Poll::Ready(()),
				false => Poll::Pending,
			}
		}));
		Poll::Ready(())
	}

	/// Take the reports queued for this session.
	pub fn take(&self) -> Reports {
		let mut state = self.database.state.lock();
		state
			.ports
			.get_mut(&self.id)
			.map(|port| std::mem::take(&mut port.pending))
			.unwrap_or_default()
	}

	/// Apply reports the peer sent, forwarding what was new to every other session.
	pub fn apply(&self, reports: Reports) {
		self.database.state.lock().apply(self.id, reports);
	}
}

impl Drop for Neighbor {
	fn drop(&mut self) {
		let mut state = self.database.state.lock();
		state.ports.remove(&self.id);
		if let Some(costs) = state.sessions.get_mut(&self.peer) {
			if let Some(index) = costs.iter().position(|cost| *cost == self.cost) {
				costs.swap_remove(index);
			}
			if costs.is_empty() {
				state.sessions.remove(&self.peer);
			}
		}
		state.report(self.peer);
	}
}

/// A snapshot of the relay graph from one relay's point of view.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Graph {
	/// The relay this graph was computed at.
	pub node: Hop,
	/// Every relay known, this one included, ordered by id.
	pub nodes: Vec<Node>,
}

impl Graph {
	/// The relay with this id, if any has reported it.
	pub fn get(&self, node: Hop) -> Option<&Node> {
		self.nodes
			.binary_search_by_key(&node.id(), |entry| entry.id.id())
			.ok()
			.map(|index| &self.nodes[index])
	}
}

/// One relay in a [`Graph`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Node {
	/// The relay's id, the Hop ID it declares in SETUP.
	pub id: Hop,
	/// The run of the relay its reports come from.
	pub incarnation: u64,
	/// The shortest path to this relay, or `None` while it is unreachable.
	pub distance: Option<Distance>,
	/// The neighbour the shortest path leaves through, or `None` for this relay
	/// and for an unreachable one.
	pub next: Option<Hop>,
	/// Every link this relay reported, up or down.
	pub links: Vec<Link>,
}

/// A link as the relay at its near end reported it.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Link {
	/// The relay at the far end.
	pub peer: Hop,
	/// The link's cost while it is up, `None` once it is down.
	pub cost: Option<u64>,
}

/// The length of a path through the relay graph: its summed link cost, then
/// its hop count, so every hop lengthens a path even across free links.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub struct Distance {
	/// The summed cost of the path's links, saturating.
	pub cost: u64,
	/// The number of links on the path.
	pub hops: u64,
}

#[cfg(test)]
mod tests {
	use super::*;

	fn database(node: u64, incarnation: u64) -> Database {
		Database::new(Hop::new(node).unwrap(), incarnation)
	}

	/// The digest of a peer that holds nothing.
	fn empty(node: u64) -> Digest {
		Digest {
			node,
			..Default::default()
		}
	}

	fn report(node: u64, incarnation: u64, links: &[(u64, u64, Option<u64>)]) -> Reports {
		let links = links
			.iter()
			.map(|(peer, seq, cost)| (*peer, Entry { seq: *seq, cost: *cost }))
			.collect();
		Reports::from([(node, Reported { incarnation, links })])
	}

	fn distance(graph: &Graph, node: u64) -> Option<(u64, u64, Option<u64>)> {
		let node = graph.get(Hop::new(node).unwrap())?;
		let distance = node.distance?;
		Some((distance.cost, distance.hops, node.next.map(Hop::id)))
	}

	/// A link counts once both ends report it up, and not before.
	#[test]
	fn links_need_both_ends() {
		let a = database(1, 0);
		let b = a.attach(2, 5, &empty(2));
		assert_eq!(distance(&a.graph(), 2), None, "only our end reported the link");

		b.apply(report(2, 0, &[(1, 1, Some(5))]));
		assert_eq!(distance(&a.graph(), 2), Some((5, 1, Some(2))));

		// The far end reporting the link down withdraws it, whatever we say.
		b.apply(report(2, 0, &[(1, 2, None)]));
		assert_eq!(distance(&a.graph(), 2), None);
	}

	/// Distance is cost first, then hop count, so a free link still costs a hop
	/// and equal paths break toward the lowest first hop.
	#[test]
	fn shortest_path_prefers_cost_then_hops() {
		let a = database(1, 0);
		let _b = a.attach(2, 0, &empty(2));
		let c = a.attach(3, 0, &empty(3));
		c.apply(report(2, 0, &[(1, 1, Some(0)), (4, 1, Some(0))]));
		c.apply(report(3, 0, &[(1, 1, Some(0)), (4, 1, Some(0)), (5, 1, Some(10))]));
		c.apply(report(4, 0, &[(2, 1, Some(0)), (3, 1, Some(0)), (5, 1, Some(1))]));
		c.apply(report(5, 0, &[(3, 1, Some(10)), (4, 1, Some(1))]));

		let graph = a.graph();
		assert_eq!(distance(&graph, 1), Some((0, 0, None)));
		assert_eq!(distance(&graph, 2), Some((0, 1, Some(2))));
		// Through 2 or 3 at equal distance: the lowest first hop wins.
		assert_eq!(distance(&graph, 4), Some((0, 2, Some(2))));
		// Three free-ish hops beat one expensive one.
		assert_eq!(distance(&graph, 5), Some((1, 3, Some(2))));
	}

	/// A restarted relay's reports replace its last run's, and a straggler from
	/// the last run changes nothing.
	#[test]
	fn a_new_incarnation_supersedes_the_old() {
		let a = database(1, 0);
		let b = a.attach(2, 1, &empty(2));
		b.apply(report(2, 10, &[(1, 1, Some(1)), (3, 7, Some(1))]));
		b.apply(report(2, 11, &[(1, 1, Some(1))]));

		let graph = a.graph();
		let node = graph.get(Hop::new(2).unwrap()).unwrap();
		assert_eq!(node.incarnation, 11);
		assert_eq!(node.links.len(), 1, "the old run's link to 3 is gone");

		b.apply(report(2, 10, &[(3, 8, Some(1))]));
		assert_eq!(a.graph().get(Hop::new(2).unwrap()).unwrap().links.len(), 1);
	}

	/// A peer's digest pulls exactly what it lacks, never its own reports, and
	/// our report of the link that just came up rather than the stale one from
	/// when it last went down.
	#[test]
	fn attach_sends_what_the_digest_lacks() {
		let a = database(1, 0);
		let c = a.attach(3, 1, &empty(3));
		c.apply(report(3, 0, &[(1, 1, Some(1)), (4, 2, Some(1))]));
		c.apply(report(2, 0, &[(1, 4, None)]));

		// The link to 2 went down once already (our seq 2), and 2 still holds our
		// seq 1 from when it was up.
		let b = a.attach(2, 1, &empty(2));
		drop(b);
		let digest = Digest {
			node: 2,
			reporters: BTreeMap::from([
				(
					1,
					Reported {
						incarnation: 0,
						links: BTreeMap::from([(2, 1), (3, 1)]),
					},
				),
				(
					3,
					Reported {
						incarnation: 0,
						links: BTreeMap::from([(1, 1)]),
					},
				),
			]),
		};
		let b = a.attach(2, 1, &digest);
		let pending = b.take();
		assert_eq!(
			pending,
			BTreeMap::from([
				(
					1,
					Reported {
						incarnation: 0,
						links: BTreeMap::from([(2, Entry { seq: 3, cost: Some(1) })]),
					}
				),
				(
					3,
					Reported {
						incarnation: 0,
						links: BTreeMap::from([(4, Entry { seq: 2, cost: Some(1) })]),
					}
				),
			]),
			"2's own reports stay out, and our link to it is the fresh one"
		);
	}

	/// What one session learns floods to every other, never back, and a link
	/// reported twice before it is sent goes out once, as the newest.
	#[test]
	fn reports_flood_to_other_sessions_only() {
		let a = database(1, 0);
		let b = a.attach(2, 1, &empty(2));
		let c = a.attach(3, 1, &empty(3));
		b.take();
		c.take();

		b.apply(report(2, 0, &[(4, 1, Some(1))]));
		b.apply(report(2, 0, &[(4, 2, None)]));
		assert!(b.take().is_empty(), "nothing echoes back");
		assert_eq!(c.take(), report(2, 0, &[(4, 2, None)]));

		// Stale or repeated reports go nowhere.
		b.apply(report(2, 0, &[(4, 2, None)]));
		assert!(c.take().is_empty());

		// Losing the session to 2 reports the link down to 3.
		drop(b);
		assert_eq!(c.take(), report(1, 0, &[(2, 2, None)]));
	}

	/// Two sessions to one peer are one link, up until the last goes, at the
	/// cheaper cost.
	#[test]
	fn parallel_sessions_are_one_link() {
		let a = database(1, 0);
		let watch = a.attach(9, 1, &empty(9));
		watch.take();

		let first = a.attach(2, 5, &empty(2));
		assert_eq!(watch.take(), report(1, 0, &[(2, 1, Some(5))]));
		let second = a.attach(2, 3, &empty(2));
		assert_eq!(watch.take(), report(1, 0, &[(2, 2, Some(3))]));

		drop(second);
		assert_eq!(watch.take(), report(1, 0, &[(2, 3, Some(5))]));
		drop(first);
		assert_eq!(watch.take(), report(1, 0, &[(2, 4, None)]));
	}

	/// A report from a previous run of this relay with a later incarnation (its
	/// clock was ahead) moves us past it rather than leaving our reports to lose.
	#[test]
	fn a_newer_self_report_moves_our_incarnation_past_it() {
		let a = database(1, 5);
		let b = a.attach(2, 1, &empty(2));
		b.take();

		b.apply(report(1, 9, &[(3, 4, Some(1))]));
		assert_eq!(b.take(), report(1, 10, &[(2, 1, Some(1))]));
		assert_eq!(a.graph().get(Hop::new(1).unwrap()).unwrap().incarnation, 10);

		// An older run's report is simply stale, and our own echoing back through
		// a cycle changes nothing.
		b.apply(report(1, 3, &[(3, 4, Some(1))]));
		b.apply(report(1, 10, &[(2, 1, Some(1))]));
		assert!(b.take().is_empty());
		assert_eq!(a.graph().get(Hop::new(1).unwrap()).unwrap().incarnation, 10);
	}

	/// A neighbour is never sent its own reports, which it would only ignore.
	#[test]
	fn a_neighbor_never_gets_its_own_reports() {
		let a = database(1, 0);
		let b = a.attach(2, 1, &empty(2));
		let c = a.attach(3, 1, &empty(3));
		b.take();
		c.take();

		c.apply(report(2, 0, &[(3, 1, Some(1))]));
		assert!(b.take().is_empty(), "2's report is not sent back to 2");
	}
}
