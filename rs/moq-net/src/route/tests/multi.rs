//! The multicast epic's route rule as a test-only prototype, measured against
//! [`path::Router`] by `bakeoff.rs`. Nothing here ships.
//!
//! A route leads to a forwarding node, never to a publisher: an endpoint's claims
//! belong to the relay it connects to, which announces them under its own Node ID
//! and revision. A route carries its path, drain, priority, and cost, and a node
//! never uses one whose path names it. A closed session is named once per
//! neighbour in a fixed-size [`Msg::Dead`], which ends every route its sender offered
//! through that hop; one another neighbour offered is unusable until that neighbour
//! says what became of it. There is no hold, no doubt, and no timer: [`Router`] has
//! no clock.
//!
//! [`Tune`] holds what the bake-off tried on top of the rule as written, each
//! switched on its own so its cost and gain are measured apart. None is a timer.
//!
//! Across a scoped grant only claims travel: the receiver announces each again
//! under its own Node ID with the chain of nodes that did so, and refuses one whose
//! chain names it.

use std::{
	cmp::Reverse,
	collections::{BTreeMap, BTreeSet},
};

use super::*;

/// The Instance a claim serves: the publisher's identity in the fixtures.
pub(super) type Instance = Id;

/// What a reader asks for: an Instance at a path.
pub(super) type Broadcast = (Instance, Path);

/// What a closed session says about the node at its far end: every route through
/// it ranks as draining until that node's own word about the session arrives, or a
/// newer session to it shows on a path.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Suspect {
	/// Nothing: the epic's rule as written.
	#[default]
	Never,
	/// The rank travels in the offer, as a drain does.
	Flagged,
	/// Each node ranks by what it heard itself, and offers say nothing of it.
	Local,
}

/// What the prototype adds to the epic's rule as written, each measured on its own.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Tune {
	pub suspect: Suspect,
	/// A node that knows a closed session sends no route until every neighbour that
	/// offered one through it has said so too: it uses its new routes at once and
	/// advertises them once the news has settled around it.
	pub quiet: bool,
	/// A node keeps, per pair of nodes it heard a closed session for, the highest
	/// session known closed, and refuses any path naming that pair at or below it,
	/// for good: sessions between a pair are numbered upward, so the fact carries
	/// its own order and no stale offer revives the path.
	pub floor: bool,
	/// A node advertises once it has read everything delivered to it, not after
	/// every message.
	pub batch: bool,
	/// A route through a neighbour is refused while that neighbour's own route to
	/// the same node says otherwise.
	pub agree: bool,
	/// A node that sent GOAWAY is named once per neighbour, as a closed session is,
	/// so every route through it ranks as draining at once.
	pub drain: bool,
}

/// One node on a route's path, and the session it learned the route on. The
/// forwarding node a route leads to names itself with session 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Hop {
	pub node: Id,
	pub session: u64,
}

/// What a route says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Offer {
	/// Every node it crossed, the forwarding node it leads to first and the sender last.
	pub path: Vec<Hop>,
	pub draining: bool,
	/// The lowest link priority on the path.
	pub priority: i8,
	/// The summed link costs, each at least 1.
	pub cost: Metric,
	/// A marked link is on its path, so it is never sent over another.
	pub marked: bool,
}

impl Offer {
	fn names(&self, node: Id) -> bool {
		self.path.iter().any(|hop| hop.node == node)
	}

	/// Lower first. Crossing a link never improves drain or priority and strictly
	/// raises the cost.
	fn rank(&self) -> (bool, Reverse<i8>, Metric) {
		(self.draining, Reverse(self.priority), self.cost)
	}
}

/// A claim as a forwarding node announces it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Claim {
	/// From the announcing node to the publisher: 0 for its own endpoint.
	pub cost: Metric,
	/// The lowest link priority between the two.
	pub priority: i8,
	/// The nodes that announced it again across a scoped grant, the first first.
	pub chain: Vec<Id>,
}

/// A change to one claim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Change {
	Live(Claim),
	/// The publisher withdrew it.
	Ended,
	/// Its announcer lost it without the publisher saying so, as when a session closes.
	Lost,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Msg {
	/// A route to forwarding node `origin`, or its end.
	Route { origin: Id, offer: Option<Offer> },
	/// Changes to `origin`'s claims, complete as of its revision `floor`.
	Claims {
		origin: Id,
		changes: Vec<(Broadcast, Change)>,
		floor: Rev,
	},
	/// The session `hop` names closed. `far` is the node at its other end.
	Dead { hop: Hop, far: Id },
	/// `node` sent GOAWAY.
	Drain { node: Id },
	/// Claims from an endpoint, or across a scoped grant: no route comes with them.
	Foreign { changes: Vec<(Broadcast, Change)> },
}

/// What a neighbour is to this node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Role {
	/// A forwarding node trusted with the whole tree: routes are exchanged.
	Routing,
	/// A forwarding node across a scoped grant: claims are exchanged, with a chain.
	Scoped,
	/// A node that never forwards: its claims are this node's own.
	Endpoint,
}

/// One side's configuration of a link.
#[derive(Clone, Copy, Debug)]
pub(super) struct Port {
	pub cost: Metric,
	pub priority: i8,
	/// The one mark: a route learned here is never sent to another marked neighbour.
	pub upstream: bool,
	pub role: Role,
}

#[derive(Clone, Debug)]
struct Peer {
	id: Id,
	port: Port,
	session: u64,
	/// It sent GOAWAY.
	draining: bool,
}

/// What one neighbour offers for one forwarding node.
#[derive(Clone, Debug)]
struct Heard {
	offer: Offer,
	/// The neighbour named a closed session on this path, which ends the route until
	/// it offers another.
	ended: bool,
	floor: Rev,
	items: BTreeMap<Broadcast, Claim>,
}

/// The route this node uses, as it stands here: the heard offer with the link added.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Best<N> {
	via: N,
	offer: Offer,
	/// It crosses a node this node suspects, which no offer says.
	suspect: bool,
}

impl<N> Best<N> {
	fn rank(&self) -> (bool, Reverse<i8>, Metric) {
		let (draining, priority, cost) = self.offer.rank();
		(draining || self.suspect, priority, cost)
	}
}

/// A forwarding node's claims as this node knows them, complete as of `floor`.
#[derive(Clone, Debug)]
struct Known<N> {
	floor: Rev,
	items: BTreeMap<Broadcast, Claim>,
	/// The neighbour whose copy this mirrors, so its next changes apply as they are.
	source: Option<N>,
}

impl<N> Default for Known<N> {
	fn default() -> Self {
		Self {
			floor: 0,
			items: BTreeMap::new(),
			source: None,
		}
	}
}

/// How candidate routes order, lower first: rank, then the next hop in use, the
/// shorter path, and the neighbour.
type Order<N> = (bool, Reverse<i8>, Metric, bool, usize, N);

/// Where a request for a broadcast goes next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Next<N> {
	/// Toward the forwarding node the request names.
	Via(N),
	/// To the endpoint or scoped neighbour whose claim this node announces.
	Local(N),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Event {
	pub broadcast: Broadcast,
	pub kind: Kind,
}

/// What a router retains.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Kept {
	/// Routes heard, one per neighbour offering a forwarding node.
	pub routes: usize,
	/// Hops on those routes' paths.
	pub hops: usize,
	/// Routes sent, one per neighbour and forwarding node.
	pub sent: usize,
	/// Claims in the merged view of each forwarding node.
	pub items: usize,
	/// Claims in each neighbour's copy.
	pub heard: usize,
	/// Claims sent across scoped grants.
	pub foreign: usize,
	/// Dead hops and suspected nodes.
	pub pending: usize,
	/// Pairs of nodes with a session known closed, kept for good.
	pub closed: usize,
}

#[derive(Clone, Debug)]
pub(super) struct Router<N> {
	me: Id,
	tune: Tune,
	sessions: u64,
	peers: BTreeMap<N, Peer>,
	heard: BTreeMap<Id, BTreeMap<N, Heard>>,
	/// Closed sessions some heard route still names, each with its far end.
	dead: BTreeMap<Hop, Id>,
	/// Nodes at the far end of a closed session, by the near ends that said so.
	suspects: BTreeMap<Id, BTreeSet<Hop>>,
	/// The highest session known closed that the first node held to the second.
	closed: BTreeMap<(Id, Id), u64>,
	/// Nodes named as draining that some heard route still crosses.
	draining: BTreeSet<Id>,
	/// Forwarding nodes whose route changed since the neighbours were last told.
	stale: BTreeSet<Id>,
	best: BTreeMap<Id, Best<N>>,
	/// What each neighbour was offered: `None` once a closed session this node named
	/// to it ended the route, until the next offer or its explicit end.
	sent: BTreeMap<N, BTreeMap<Id, Option<Offer>>>,
	known: BTreeMap<Id, Known<N>>,
	/// This node's revision of its own claims.
	rev: Rev,
	/// Claims heard from endpoints and scoped neighbours, by neighbour.
	local: BTreeMap<N, BTreeMap<Broadcast, Claim>>,
	/// The neighbour behind each of this node's own claims.
	sources: BTreeMap<Broadcast, N>,
	/// Claims sent to each scoped neighbour.
	foreign: BTreeMap<N, BTreeMap<Broadcast, Claim>>,
	/// The forwarding nodes claiming each broadcast, as far as this node knows.
	claimants: BTreeMap<Broadcast, BTreeSet<Id>>,
	shown: BTreeMap<Broadcast, bool>,
	dirty: BTreeSet<Broadcast>,
	/// Broadcasts whose last removal was the publisher's own withdrawal.
	ended: BTreeSet<Broadcast>,
	out: Vec<(N, Msg)>,
	events: Vec<Event>,
	/// Routes and claims visited, to compare work.
	work: usize,
}

impl<N: Key> Router<N> {
	pub fn new(me: Id, tune: Tune) -> Self {
		let mut router = Self {
			me,
			tune,
			sessions: 0,
			peers: BTreeMap::new(),
			heard: BTreeMap::new(),
			dead: BTreeMap::new(),
			suspects: BTreeMap::new(),
			closed: BTreeMap::new(),
			draining: BTreeSet::new(),
			stale: BTreeSet::new(),
			best: BTreeMap::new(),
			sent: BTreeMap::new(),
			known: BTreeMap::new(),
			rev: 0,
			local: BTreeMap::new(),
			sources: BTreeMap::new(),
			foreign: BTreeMap::new(),
			claimants: BTreeMap::new(),
			shown: BTreeMap::new(),
			dirty: BTreeSet::new(),
			ended: BTreeSet::new(),
			out: Vec::new(),
			events: Vec::new(),
			work: 0,
		};
		router.known.insert(me, Known::default());
		router
	}

	/// What to send, with every route as it stands now.
	pub fn drain(&mut self) -> Vec<(N, Msg)> {
		self.advertise();
		std::mem::take(&mut self.out)
	}

	/// Tell every neighbour of each route that changed.
	fn advertise(&mut self) {
		if self.tune.quiet && !self.dead.is_empty() {
			return;
		}
		let peers: Vec<N> = self.peers.keys().copied().collect();
		while let Some(origin) = self.stale.pop_first() {
			for &peer in &peers {
				self.advertise_to(origin, peer);
			}
		}
	}

	/// An input is done: advertise unless a burst is read first, and show readers.
	fn done(&mut self) {
		if !self.tune.batch {
			self.advertise();
		}
		self.refresh();
	}

	pub fn events(&mut self) -> Vec<Event> {
		std::mem::take(&mut self.events)
	}

	pub fn work(&self) -> usize {
		self.work
	}

	pub fn shown(&self, broadcast: &Broadcast) -> Option<bool> {
		self.shown.get(broadcast).copied()
	}

	pub fn connect(&mut self, peer: N, id: Id, port: Port) {
		assert!(port.cost > 0, "a zero-cost link");
		self.sessions += 1;
		let session = self.sessions;
		self.peers.insert(
			peer,
			Peer {
				id,
				port,
				session,
				draining: false,
			},
		);
		// A session to a suspected node is its own word that it lives.
		if let Some(nears) = self.suspects.get_mut(&id) {
			let me = self.me;
			nears.retain(|near| near.node != me);
			if nears.is_empty() {
				self.suspects.remove(&id);
			}
			self.reselect_all();
		}
		match port.role {
			Role::Routing => {
				let me = self.me;
				self.stale.extend(self.best.keys().copied().chain([me]));
			}
			Role::Scoped => {
				let broadcasts: Vec<Broadcast> = self.shown.keys().cloned().collect();
				for broadcast in broadcasts {
					self.export(&broadcast, peer);
				}
			}
			Role::Endpoint => {}
		}
		self.done();
	}

	pub fn disconnect(&mut self, peer: N) {
		let Some(gone) = self.peers.remove(&peer) else {
			return;
		};
		let mut lost = Vec::new();
		for (&origin, offers) in &mut self.heard {
			if offers.remove(&peer).is_some() {
				lost.push(origin);
			}
		}
		for origin in lost {
			self.unheard(origin, peer);
		}
		self.sent.remove(&peer);
		self.foreign.remove(&peer);
		if let Some(claims) = self.local.remove(&peer) {
			for broadcast in claims.into_keys() {
				self.restate(broadcast, Change::Lost);
			}
		}
		match gone.port.role {
			Role::Routing => self.closed(
				Hop {
					node: self.me,
					session: gone.session,
				},
				gone.id,
				None,
			),
			Role::Scoped | Role::Endpoint => {}
		}
		self.done();
	}

	/// `peer` sent GOAWAY: every route through it ranks as draining.
	pub fn goaway(&mut self, peer: N) {
		let Some(peer) = self.peers.get_mut(&peer) else {
			return;
		};
		peer.draining = true;
		let node = peer.id;
		match self.tune.drain {
			true => self.drains(node, None),
			false => self.reselect_all(),
		}
		self.done();
	}

	/// `node` is draining: `from` said so, or its own GOAWAY when `None`.
	fn drains(&mut self, node: Id, from: Option<N>) {
		self.work += 1;
		if !self.draining.insert(node) {
			return;
		}
		let tell: Vec<N> = (self.sent.iter())
			.filter(|(peer, sent)| Some(**peer) != from && sent.values().flatten().any(|offer| offer.names(node)))
			.map(|(&peer, _)| peer)
			.collect();
		for peer in tell {
			self.out.push((peer, Msg::Drain { node }));
		}
		self.forget();
		self.reselect_all();
	}

	pub fn recv(&mut self, from: N, msg: Msg) {
		let Some(peer) = self.peers.get(&from) else {
			return;
		};
		let role = peer.port.role;
		match msg {
			Msg::Route { origin, offer } if role == Role::Routing => self.route(from, origin, offer),
			Msg::Claims { origin, changes, floor } if role == Role::Routing => {
				self.claims(from, origin, changes, floor)
			}
			Msg::Dead { hop, far } if role == Role::Routing => self.closed(hop, far, Some(from)),
			Msg::Drain { node } if role == Role::Routing => self.drains(node, Some(from)),
			Msg::Foreign { changes } if role != Role::Routing => self.adopt(from, changes),
			// A neighbour sends only what its role allows; anything else would close the session.
			other => panic!("{other:?} from a {role:?} neighbour"),
		}
		self.done();
	}

	fn route(&mut self, from: N, origin: Id, offer: Option<Offer>) {
		self.work += 1;
		if origin == self.me {
			return;
		}
		match offer {
			Some(offer) => {
				self.vouch(&offer);
				let offers = self.heard.entry(origin).or_default();
				match offers.get_mut(&from) {
					Some(heard) => {
						heard.offer = offer;
						heard.ended = false;
					}
					None => {
						offers.insert(
							from,
							Heard {
								offer,
								ended: false,
								floor: 0,
								items: BTreeMap::new(),
							},
						);
					}
				}
			}
			None => {
				if let Some(offers) = self.heard.get_mut(&origin) {
					offers.remove(&from);
				}
				self.unheard(origin, from);
			}
		}
		self.forget();
		self.reselect(origin);
	}

	/// A path that crosses a suspected node and then a newer session of the near end
	/// that cast the suspicion shows the two met again.
	fn vouch(&mut self, offer: &Offer) {
		if self.suspects.is_empty() {
			return;
		}
		let mut lifted = false;
		for pair in offer.path.windows(2) {
			let (far, near) = (pair[0].node, pair[1]);
			if let Some(nears) = self.suspects.get_mut(&far) {
				let before = nears.len();
				nears.retain(|cast| cast.node != near.node || cast.session >= near.session);
				lifted |= nears.len() != before;
				if nears.is_empty() {
					self.suspects.remove(&far);
				}
			}
		}
		if lifted {
			self.reselect_all();
		}
	}

	/// `from` no longer offers `origin`: its copy of the claims went with the route.
	fn unheard(&mut self, origin: Id, from: N) {
		if let Some(known) = self.known.get_mut(&origin)
			&& known.source == Some(from)
		{
			known.source = None;
		}
		if self.heard.get(&origin).is_some_and(BTreeMap::is_empty) {
			self.heard.remove(&origin);
			if let Some(known) = self.known.remove(&origin) {
				for broadcast in known.items.into_keys() {
					self.unclaim(origin, broadcast);
				}
			}
		}
	}

	fn claims(&mut self, from: N, origin: Id, changes: Vec<(Broadcast, Change)>, floor: Rev) {
		let Some(heard) = self.heard.get_mut(&origin).and_then(|offers| offers.get_mut(&from)) else {
			return;
		};
		self.work += changes.len();
		for (broadcast, change) in &changes {
			match change {
				Change::Live(claim) => heard.items.insert(broadcast.clone(), claim.clone()),
				Change::Ended | Change::Lost => heard.items.remove(broadcast),
			};
		}
		heard.floor = floor;
		let known = self.known.entry(origin).or_default();
		// The newest floor wins, so an older copy never revives what a newer one withdrew.
		if floor <= known.floor {
			return;
		}
		let changes = match known.source == Some(from) {
			true => changes,
			false => {
				let mut diff: Vec<(Broadcast, Change)> = (heard.items.iter())
					.filter(|(broadcast, claim)| known.items.get(*broadcast) != Some(claim))
					.map(|(broadcast, claim)| (broadcast.clone(), Change::Live(claim.clone())))
					.collect();
				diff.extend(
					(known.items.keys())
						.filter(|broadcast| !heard.items.contains_key(*broadcast))
						.map(|broadcast| (broadcast.clone(), Change::Lost)),
				);
				self.work += heard.items.len();
				diff
			}
		};
		known.source = Some(from);
		known.floor = floor;
		self.publish(origin, changes);
	}

	/// Apply `changes` to this node's view of `origin` and pass them to every
	/// neighbour it offers `origin` to.
	fn publish(&mut self, origin: Id, changes: Vec<(Broadcast, Change)>) {
		let known = self.known.entry(origin).or_default();
		let floor = known.floor;
		for (broadcast, change) in &changes {
			match change {
				Change::Live(claim) => {
					known.items.insert(broadcast.clone(), claim.clone());
					self.claimants.entry(broadcast.clone()).or_default().insert(origin);
					self.ended.remove(broadcast);
					self.dirty.insert(broadcast.clone());
				}
				Change::Ended | Change::Lost => {
					known.items.remove(broadcast);
					if *change == Change::Ended {
						self.ended.insert(broadcast.clone());
					}
					if let Some(origins) = self.claimants.get_mut(broadcast) {
						origins.remove(&origin);
						if origins.is_empty() {
							self.claimants.remove(broadcast);
						}
					}
					self.dirty.insert(broadcast.clone());
				}
			}
		}
		if changes.is_empty() {
			return;
		}
		for (&peer, sent) in &self.sent {
			if sent.contains_key(&origin) {
				self.out.push((
					peer,
					Msg::Claims {
						origin,
						changes: changes.clone(),
						floor,
					},
				));
			}
		}
	}

	fn unclaim(&mut self, origin: Id, broadcast: Broadcast) {
		if let Some(origins) = self.claimants.get_mut(&broadcast) {
			origins.remove(&origin);
			if origins.is_empty() {
				self.claimants.remove(&broadcast);
			}
		}
		self.dirty.insert(broadcast);
	}

	/// Claims from an endpoint or across a scoped grant become this node's own.
	fn adopt(&mut self, from: N, changes: Vec<(Broadcast, Change)>) {
		let peer = &self.peers[&from];
		let (cost, priority, scoped) = (peer.port.cost, peer.port.priority, peer.port.role == Role::Scoped);
		for (broadcast, change) in changes {
			self.work += 1;
			let change = match change {
				// A claim that already crossed this node came round a ring of grants.
				Change::Live(claim) if claim.chain.contains(&self.me) => Change::Lost,
				Change::Live(mut claim) => {
					claim.cost += cost;
					claim.priority = claim.priority.min(priority);
					if scoped {
						claim.chain.push(self.me);
					}
					Change::Live(claim)
				}
				change => change,
			};
			let claims = self.local.entry(from).or_default();
			match change {
				Change::Live(claim) => {
					claims.insert(broadcast.clone(), claim);
					self.restate(broadcast, Change::Lost);
				}
				cause => {
					if claims.remove(&broadcast).is_some() {
						self.restate(broadcast, cause);
					}
				}
			}
		}
	}

	/// Announce the cheapest claim any neighbour gave this node for `broadcast`, or
	/// withdraw it for `cause` when none is left.
	fn restate(&mut self, broadcast: Broadcast, cause: Change) {
		let cheapest = (self.local.iter())
			.filter_map(|(&peer, claims)| Some((claims.get(&broadcast)?, peer)))
			.min_by_key(|(claim, peer)| (Reverse(claim.priority), claim.cost, claim.chain.len(), *peer))
			.map(|(claim, peer)| (claim.clone(), peer));
		let me = self.me;
		let was = self.known.entry(me).or_default().items.get(&broadcast).cloned();
		let change = match cheapest {
			Some((claim, peer)) => {
				self.sources.insert(broadcast.clone(), peer);
				if was.as_ref() == Some(&claim) {
					// The source alone moved, which a scoped neighbour must hear of.
					self.dirty.insert(broadcast);
					return;
				}
				Change::Live(claim)
			}
			None => {
				self.sources.remove(&broadcast);
				if was.is_none() {
					return;
				}
				cause
			}
		};
		self.rev += 1;
		self.known.entry(me).or_default().floor = self.rev;
		self.publish(me, vec![(broadcast, change)]);
	}

	/// A session closed: this node's own when `from` is `None`.
	fn closed(&mut self, hop: Hop, far: Id, from: Option<N>) {
		self.work += 1;
		// The neighbour's word ends every route it offered through the session.
		if let Some(from) = from {
			for heard in self.heard.values_mut().filter_map(|offers| offers.get_mut(&from)) {
				heard.ended |= heard.offer.path.contains(&hop);
			}
		}
		if self.tune.floor {
			let highest = self.closed.entry((hop.node, far)).or_default();
			*highest = (*highest).max(hop.session);
		}
		let named = (self.heard.values())
			.flat_map(BTreeMap::values)
			.any(|heard| !heard.ended && heard.offer.path.contains(&hop));
		let tell: Vec<N> = (self.sent.iter())
			.filter(|(peer, sent)| {
				Some(**peer) != from && sent.values().flatten().any(|offer| offer.path.contains(&hop))
			})
			.map(|(&peer, _)| peer)
			.collect();
		// The far end's own word about a session lifts what its near end cast on it.
		if let Some(nears) = self.suspects.get_mut(&hop.node) {
			nears.retain(|near| near.node != far);
			if nears.is_empty() {
				self.suspects.remove(&hop.node);
			}
		}
		let news = !self.dead.contains_key(&hop) && (named || !tell.is_empty() || from.is_none());
		if news {
			if named {
				self.dead.insert(hop, far);
			}
			for peer in tell {
				self.out.push((peer, Msg::Dead { hop, far }));
				for (origin, offer) in self.sent.get_mut(&peer).into_iter().flatten() {
					if offer.as_ref().is_some_and(|offer| offer.path.contains(&hop)) {
						*offer = None;
						// Owed its next offer, or its explicit end.
						self.stale.insert(*origin);
					}
				}
			}
			let answered = self
				.dead
				.iter()
				.any(|(word, about)| word.node == far && *about == hop.node);
			if self.tune.suspect != Suspect::Never && !answered {
				self.suspects.entry(far).or_default().insert(hop);
			}
		}
		self.forget();
		self.reselect_all();
	}

	/// Forget each dead hop and suspected node no heard route names.
	fn forget(&mut self) {
		if self.dead.is_empty() && self.suspects.is_empty() && self.draining.is_empty() {
			return;
		}
		let mut hops = BTreeSet::new();
		let mut nodes = BTreeSet::new();
		for heard in self
			.heard
			.values()
			.flat_map(BTreeMap::values)
			.filter(|heard| !heard.ended)
		{
			for hop in &heard.offer.path {
				hops.insert(*hop);
				nodes.insert(hop.node);
			}
		}
		self.work += hops.len();
		self.dead.retain(|hop, _| hops.contains(hop));
		self.suspects.retain(|node, _| nodes.contains(node));
		self.draining.retain(|node| nodes.contains(node));
	}

	fn reselect_all(&mut self) {
		let origins: BTreeSet<Id> = self.heard.keys().chain(self.best.keys()).copied().collect();
		for origin in origins {
			self.reselect(origin);
		}
	}

	/// Pick the route to `origin` and tell every neighbour what changed.
	fn reselect(&mut self, origin: Id) {
		let current = self.best.get(&origin).map(|best| best.via);
		let mut pick: Option<(Best<N>, Order<N>)> = None;
		for (&via, heard) in self.heard.get(&origin).into_iter().flatten() {
			self.work += 1;
			let Some(peer) = self.peers.get(&via) else {
				continue;
			};
			let offer = &heard.offer;
			let floored = offer.path.windows(2).any(|pair| {
				(self.closed.get(&(pair[1].node, pair[0].node))).is_some_and(|highest| pair[1].session <= *highest)
			});
			if heard.ended || floored {
				continue;
			}
			if offer.names(self.me)
				|| offer.path.len() >= MAX_HOPS
				|| offer.path.iter().any(|hop| self.dead.contains_key(hop))
			{
				continue;
			}
			if self.tune.agree && !self.agrees(origin, via, offer) {
				continue;
			}
			let named = |nodes: &dyn Fn(&Id) -> bool| offer.path.iter().any(|hop| nodes(&hop.node));
			let suspect = named(&|node| self.suspects.contains_key(node));
			let flagged = suspect && self.tune.suspect == Suspect::Flagged;
			let mine = Best {
				via,
				offer: Offer {
					path: offer.path.clone(),
					draining: offer.draining || peer.draining || flagged || named(&|node| self.draining.contains(node)),
					priority: offer.priority.min(peer.port.priority),
					cost: offer.cost + peer.port.cost,
					marked: offer.marked || peer.port.upstream,
				},
				suspect,
			};
			let (draining, priority, cost) = mine.rank();
			// Among equal ranks a node stays on its next hop.
			let key = (
				draining,
				priority,
				cost,
				current != Some(via),
				mine.offer.path.len(),
				via,
			);
			if pick.as_ref().is_none_or(|(_, best)| key < *best) {
				pick = Some((mine, key));
			}
		}
		let pick = pick.map(|(best, _)| best);
		if self.best.get(&origin) == pick.as_ref() {
			return;
		}
		let (was, now) = (self.best.contains_key(&origin), pick.is_some());
		let moved = self.best.get(&origin).map(Best::rank) != pick.as_ref().map(Best::rank);
		match pick {
			Some(best) => self.best.insert(origin, best),
			None => self.best.remove(&origin),
		};
		// Whether its claims are reachable, and at what cost, follows the route.
		if (was != now || moved)
			&& let Some(known) = self.known.get(&origin)
		{
			self.work += known.items.len();
			self.dirty.extend(known.items.keys().cloned());
		}
		self.stale.insert(origin);
	}

	/// Whether `offer`, heard from `via`, crosses no other neighbour whose own route
	/// to `origin` differs: a neighbour's word about its route is newer than anyone's
	/// copy of it.
	fn agrees(&self, origin: Id, via: N, offer: &Offer) -> bool {
		let Some(offers) = self.heard.get(&origin) else {
			return true;
		};
		(offers.iter())
			.filter(|(peer, own)| **peer != via && !own.ended)
			.all(|(peer, own)| {
				let id = self.peers[peer].id;
				match offer.path.iter().position(|hop| hop.node == id) {
					Some(at) => offer.path[..=at] == own.offer.path[..],
					None => true,
				}
			})
	}

	/// What `peer` should hear of `origin` now.
	fn offer_for(&self, origin: Id, peer: N) -> Option<Offer> {
		let to = self.peers.get(&peer).filter(|to| to.port.role == Role::Routing)?;
		if origin == self.me {
			return Some(Offer {
				path: vec![Hop {
					node: self.me,
					session: 0,
				}],
				draining: false,
				priority: i8::MAX,
				cost: 0,
				marked: false,
			});
		}
		let best = self.best.get(&origin)?;
		// Never back over the session it came on, to a node already on its path, or
		// from one marked link to another.
		if best.via == peer || best.offer.names(to.id) || (best.offer.marked && to.port.upstream) {
			return None;
		}
		let mut offer = best.offer.clone();
		offer.path.push(Hop {
			node: self.me,
			session: self.peers[&best.via].session,
		});
		Some(offer)
	}

	fn advertise_to(&mut self, origin: Id, peer: N) {
		if !self.peers.get(&peer).is_some_and(|to| to.port.role == Role::Routing) {
			return;
		}
		let offer = self.offer_for(origin, peer);
		let sent = self.sent.entry(peer).or_default();
		let was = sent.get(&origin);
		if was.is_none_or(|was| was.is_some()) && was.and_then(Option::as_ref) == offer.as_ref() {
			return;
		}
		let fresh = was.is_none();
		match &offer {
			Some(offer) => sent.insert(origin, Some(offer.clone())),
			None => sent.remove(&origin),
		};
		let started = fresh && offer.is_some();
		self.out.push((peer, Msg::Route { origin, offer }));
		// A route that starts brings the claims as they stand.
		// Even an empty one: its floor outranks an older copy of what was withdrawn.
		if started
			&& let Some(known) = self.known.get(&origin)
			&& known.floor > 0
		{
			self.work += known.items.len();
			let changes = (known.items.iter())
				.map(|(broadcast, claim)| (broadcast.clone(), Change::Live(claim.clone())))
				.collect();
			self.out.push((
				peer,
				Msg::Claims {
					origin,
					changes,
					floor: known.floor,
				},
			));
		}
	}

	/// The forwarding node a request for `broadcast` names from here: the closest claim.
	pub fn pick(&self, broadcast: &Broadcast) -> Option<Id> {
		self.closest(broadcast).map(|(origin, _)| origin)
	}

	fn closest(&self, broadcast: &Broadcast) -> Option<(Id, &Claim)> {
		(self.claimants.get(broadcast)?.iter())
			.filter_map(|&origin| {
				let claim = self.known.get(&origin)?.items.get(broadcast)?;
				let rank = match origin == self.me {
					true => (false, Reverse(claim.priority), claim.cost),
					false => {
						let (draining, Reverse(priority), cost) = self.best.get(&origin)?.rank();
						(draining, Reverse(priority.min(claim.priority)), cost + claim.cost)
					}
				};
				Some(((rank, claim.chain.len(), origin), claim))
			})
			.min_by(|(a, _), (b, _)| a.cmp(b))
			.map(|((_, _, origin), claim)| (origin, claim))
	}

	/// Where a request for `broadcast` naming `origin` goes from here. `None` fails it.
	pub fn step(&self, origin: Id, broadcast: &Broadcast) -> Option<Next<N>> {
		match origin == self.me {
			true => self.sources.get(broadcast).copied().map(Next::Local),
			false => self.best.get(&origin).map(|best| Next::Via(best.via)),
		}
	}

	/// Bring what a reader sees, and what scoped neighbours are told, up to date.
	fn refresh(&mut self) {
		while let Some(broadcast) = self.dirty.pop_first() {
			self.work += 1;
			let known = self.claimants.contains_key(&broadcast);
			let available = self.closest(&broadcast).is_some();
			let mut emit = |kind| {
				self.events.push(Event {
					broadcast: broadcast.clone(),
					kind,
				});
			};
			match (self.shown.get(&broadcast).copied(), known) {
				(None, true) => {
					emit(Kind::Start);
					if !available {
						emit(Kind::Unavailable);
					}
				}
				(Some(_), false) => emit(match self.ended.contains(&broadcast) {
					true => Kind::End,
					false => Kind::Offline,
				}),
				(Some(was), true) if was != available => emit(match available {
					true => Kind::Available,
					false => Kind::Unavailable,
				}),
				_ => {}
			}
			match known {
				true => self.shown.insert(broadcast.clone(), available),
				false => self.shown.remove(&broadcast),
			};
			let scoped: Vec<N> = (self.peers.iter())
				.filter(|(_, peer)| peer.port.role == Role::Scoped)
				.map(|(&peer, _)| peer)
				.collect();
			for peer in scoped {
				self.export(&broadcast, peer);
			}
			self.ended.remove(&broadcast);
		}
	}

	/// Tell scoped `peer` this node's closest claim for `broadcast`, unless it came
	/// from that neighbour.
	fn export(&mut self, broadcast: &Broadcast, peer: N) {
		let claim = self.closest(broadcast).and_then(|(origin, claim)| {
			let (cost, priority) = match origin == self.me {
				true if self.sources.get(broadcast) == Some(&peer) => return None,
				true => (claim.cost, claim.priority),
				false => {
					let offer = &self.best[&origin].offer;
					(claim.cost + offer.cost, claim.priority.min(offer.priority))
				}
			};
			Some(Claim {
				cost,
				priority,
				chain: claim.chain.clone(),
			})
		});
		let sent = self.foreign.entry(peer).or_default();
		if sent.get(broadcast) == claim.as_ref() {
			return;
		}
		let change = match claim {
			Some(claim) => {
				sent.insert(broadcast.clone(), claim.clone());
				Change::Live(claim)
			}
			None => {
				sent.remove(broadcast);
				match self.ended.contains(broadcast) {
					true => Change::Ended,
					false => Change::Lost,
				}
			}
		};
		self.out.push((
			peer,
			Msg::Foreign {
				changes: vec![(broadcast.clone(), change)],
			},
		));
	}

	pub fn kept(&self) -> Kept {
		let heard = || self.heard.values().flat_map(BTreeMap::values);
		Kept {
			routes: heard().count(),
			hops: heard().map(|heard| heard.offer.path.len()).sum(),
			sent: self.sent.values().map(BTreeMap::len).sum(),
			items: self.known.values().map(|known| known.items.len()).sum(),
			heard: heard().map(|heard| heard.items.len()).sum(),
			foreign: self.foreign.values().map(BTreeMap::len).sum(),
			pending: self.dead.len() + self.suspects.len() + self.draining.len(),
			closed: self.closed.len(),
		}
	}
}

/// What sits at a simulated node.
#[derive(Clone, Debug)]
enum Spot {
	Relay(Box<Router<Node>>),
	/// A publisher: an endpoint that never forwards.
	Endpoint {
		id: Option<Id>,
		claims: BTreeSet<Path>,
	},
}

/// How one side of a link relates to the other.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Tie {
	Peer,
	/// The first node marks the second as its upstream.
	Up,
	/// A scoped grant each way: a trust boundary.
	Scoped,
}

/// A reader event with what was true when it showed.
#[derive(Clone, Debug)]
pub(super) struct Seen {
	pub node: Node,
	pub broadcast: Broadcast,
	pub kind: Kind,
	/// How many times the publisher had started this broadcast.
	pub life: u64,
	/// Whether a publisher with an open session claimed it.
	pub live: bool,
}

/// A network of [`Router`]s and endpoints, with [`Sim`]'s queues.
#[derive(Clone)]
pub(super) struct MultiSim {
	tune: Tune,
	spots: Vec<Spot>,
	restarts: Vec<u32>,
	ports: BTreeMap<(Node, Node), Port>,
	up: BTreeSet<(Node, Node)>,
	queues: BTreeMap<(Node, Node), VecDeque<Msg>>,
	held: BTreeSet<(Node, Node)>,
	lives: BTreeMap<Broadcast, u64>,
	pub stats: Stats,
	pub events: Vec<Seen>,
	spent: usize,
}

impl MultiSim {
	pub fn new(tune: Tune, nodes: usize) -> Self {
		Self {
			tune,
			spots: (0..nodes)
				.map(|node| Spot::Relay(Box::new(Router::new(hop(node, 0), tune))))
				.collect(),
			restarts: vec![0; nodes],
			ports: BTreeMap::new(),
			up: BTreeSet::new(),
			queues: BTreeMap::new(),
			held: BTreeSet::new(),
			lives: BTreeMap::new(),
			stats: Stats::default(),
			events: Vec::new(),
			spent: 0,
		}
	}

	fn pair(a: Node, b: Node) -> (Node, Node) {
		(a.min(b), a.max(b))
	}

	pub fn router(&self, node: Node) -> Option<&Router<Node>> {
		match &self.spots[node] {
			Spot::Relay(router) => Some(router),
			Spot::Endpoint { .. } => None,
		}
	}

	fn relay(&mut self, node: Node) -> Option<&mut Router<Node>> {
		match &mut self.spots[node] {
			Spot::Relay(router) => Some(router),
			Spot::Endpoint { .. } => None,
		}
	}

	fn peers(&self, node: Node) -> Vec<Node> {
		(self.up.iter())
			.filter_map(|&(a, b)| match node {
				_ if node == a => Some(b),
				_ if node == b => Some(a),
				_ => None,
			})
			.collect()
	}

	pub fn link(&mut self, a: Node, b: Node, cost: Metric, tie: Tie) {
		let role = |node: Node, spots: &[Spot]| match (&spots[node], tie) {
			(Spot::Endpoint { .. }, _) => Role::Endpoint,
			(_, Tie::Scoped) => Role::Scoped,
			_ => Role::Routing,
		};
		let port = |upstream, role| Port {
			cost,
			priority: 0,
			upstream,
			role,
		};
		// Each side's port describes the node at the other end.
		self.ports.insert((a, b), port(tie == Tie::Up, role(b, &self.spots)));
		self.ports.insert((b, a), port(false, role(a, &self.spots)));
		self.open(a, b);
	}

	pub fn open(&mut self, a: Node, b: Node) {
		assert!(self.up.insert(Self::pair(a, b)), "{a}-{b} already up");
		for (x, y) in [(a, b), (b, a)] {
			let port = self.ports[&(x, y)];
			let named = hop(y, self.restarts[y]);
			let changes: Vec<(Broadcast, Change)> = match &mut self.spots[x] {
				Spot::Relay(router) => {
					router.connect(y, named, port);
					continue;
				}
				Spot::Endpoint { id, claims } => (claims.iter())
					.map(|path| {
						let claim = Claim {
							cost: 0,
							priority: i8::MAX,
							chain: Vec::new(),
						};
						((id.expect("a publisher"), path.clone()), Change::Live(claim))
					})
					.collect(),
			};
			if !changes.is_empty() {
				self.send(x, y, Msg::Foreign { changes });
			}
		}
		self.flush();
	}

	pub fn close(&mut self, a: Node, b: Node) {
		self.up.remove(&Self::pair(a, b));
		for (x, y) in [(a, b), (b, a)] {
			self.queues.remove(&(x, y));
			self.held.remove(&(x, y));
			if let Some(router) = self.relay(y) {
				router.disconnect(x);
			}
		}
		self.flush();
	}

	/// `peer` notices its session to `node` closed, which `node` has yet to.
	pub fn sever(&mut self, node: Node, peer: Node) {
		self.up.remove(&Self::pair(node, peer));
		for key in [(node, peer), (peer, node)] {
			self.queues.remove(&key);
			self.held.remove(&key);
		}
		if let Some(router) = self.relay(peer) {
			router.disconnect(node);
		}
		self.flush();
	}

	/// The process at `node` exits: its sessions close and its state is lost.
	pub fn reset(&mut self, node: Node) {
		for peer in self.peers(node) {
			self.close(node, peer);
		}
		self.restarts[node] += 1;
		if let Spot::Relay(router) = &self.spots[node] {
			self.spent += router.work();
			self.spots[node] = Spot::Relay(Box::new(Router::new(hop(node, self.restarts[node]), self.tune)));
		}
	}

	pub fn goaway(&mut self, node: Node) {
		for peer in self.peers(node) {
			if let Some(router) = self.relay(peer) {
				router.goaway(node);
			}
		}
		self.flush();
	}

	pub fn client(&mut self, node: Node) {
		self.spots[node] = Spot::Endpoint {
			id: None,
			claims: BTreeSet::new(),
		};
	}

	pub fn publish(&mut self, node: Node, named: Id) {
		let Spot::Endpoint { id, .. } = &mut self.spots[node] else {
			panic!("node {node} is a relay");
		};
		*id = Some(named);
	}

	pub fn claim(&mut self, node: Node, claim: &str, live: bool) {
		let claim = p(claim);
		let Spot::Endpoint { id, claims } = &mut self.spots[node] else {
			panic!("node {node} is a relay");
		};
		let broadcast = (id.expect("a publisher"), claim.clone());
		let change = match live {
			true if claims.insert(claim.clone()) => {
				*self.lives.entry(broadcast.clone()).or_default() += 1;
				Change::Live(Claim {
					cost: 0,
					priority: i8::MAX,
					chain: Vec::new(),
				})
			}
			false if claims.remove(&claim) => Change::Ended,
			_ => return,
		};
		for peer in self.peers(node) {
			self.send(
				node,
				peer,
				Msg::Foreign {
					changes: vec![(broadcast.clone(), change.clone())],
				},
			);
		}
	}

	fn send(&mut self, from: Node, to: Node, msg: Msg) {
		match &msg {
			Msg::Route { .. } | Msg::Dead { .. } | Msg::Drain { .. } => {
				self.stats.route_msgs += 1;
				self.stats.route_items += 1;
			}
			Msg::Claims { changes, .. } | Msg::Foreign { changes } => {
				self.stats.claim_msgs += 1;
				self.stats.claim_items += changes.len();
			}
		}
		self.queues.entry((from, to)).or_default().push_back(msg);
	}

	pub fn hold(&mut self, from: Node, to: Node) {
		self.held.insert((from, to));
	}

	pub fn ready(&self) -> Vec<((Node, Node), usize)> {
		(self.queues.iter())
			.filter(|(key, queue)| !queue.is_empty() && !self.held.contains(key))
			.map(|(key, queue)| (*key, queue.len()))
			.collect()
	}

	pub fn deliver(&mut self, from: Node, to: Node) -> bool {
		let Some(msg) = self.queues.get_mut(&(from, to)).and_then(VecDeque::pop_front) else {
			return false;
		};
		if let Some(router) = self.relay(to) {
			router.recv(from, msg);
		}
		self.stats.delivered += 1;
		// A relay that reads a burst first answers when the round ends.
		match self.tune.batch {
			true => self.show(),
			false => self.flush(),
		}
		true
	}

	fn live(&self, broadcast: &Broadcast) -> bool {
		self.spots.iter().enumerate().any(|(node, spot)| match spot {
			Spot::Endpoint { id, claims } => {
				*id == Some(broadcast.0) && claims.contains(&broadcast.1) && !self.peers(node).is_empty()
			}
			Spot::Relay(_) => false,
		})
	}

	/// Move every router's output onto the queues and log its reader events.
	pub fn flush(&mut self) {
		for node in 0..self.spots.len() {
			let Some(router) = self.relay(node) else {
				continue;
			};
			for (to, msg) in router.drain() {
				self.send(node, to, msg);
			}
		}
		self.show();
	}

	fn show(&mut self) {
		for node in 0..self.spots.len() {
			let Some(router) = self.relay(node) else {
				continue;
			};
			for event in router.events() {
				let seen = Seen {
					node,
					live: self.live(&event.broadcast),
					life: self.lives.get(&event.broadcast).copied().unwrap_or(0),
					broadcast: event.broadcast,
					kind: event.kind,
				};
				self.events.push(seen);
			}
		}
	}

	/// The nodes a request from `from` for `broadcast` visits, and whether it ended
	/// at a publisher. `None` when `from` knows no claim it can reach. A chain longer
	/// than the network loops.
	pub fn chase(&self, from: Node, broadcast: &Broadcast) -> Option<(Vec<Node>, bool)> {
		let (mut at, mut named, mut hops) = (from, None, Vec::new());
		while hops.len() <= self.spots.len() {
			let router = match &self.spots[at] {
				Spot::Endpoint { id, claims } => {
					return Some((hops, *id == Some(broadcast.0) && claims.contains(&broadcast.1)));
				}
				Spot::Relay(router) => router,
			};
			let Some(origin) = named.or_else(|| router.pick(broadcast)) else {
				return (!hops.is_empty()).then_some((hops, false));
			};
			match router.step(origin, broadcast) {
				// A request names the forwarding node its first hop chose.
				Some(Next::Via(next)) => {
					named = Some(origin);
					hops.push(next);
					at = next;
				}
				// Past a claim's announcer the next node chooses again.
				Some(Next::Local(next)) => {
					named = None;
					hops.push(next);
					at = next;
				}
				None => return Some((hops, false)),
			}
		}
		Some((hops, false))
	}

	pub fn work(&self) -> usize {
		self.spent
			+ (0..self.spots.len())
				.filter_map(|node| self.router(node))
				.map(Router::work)
				.sum::<usize>()
	}

	pub fn kept(&self) -> Kept {
		let mut out = Kept::default();
		for router in (0..self.spots.len()).filter_map(|node| self.router(node)) {
			let kept = router.kept();
			out.routes += kept.routes;
			out.hops += kept.hops;
			out.sent += kept.sent;
			out.items += kept.items;
			out.heard += kept.heard;
			out.foreign += kept.foreign;
			out.pending += kept.pending;
			out.closed += kept.closed;
		}
		out
	}
}

/// Starts of a broadcast's life that the same node had already shown ending.
pub(super) fn revivals(events: &[Seen]) -> usize {
	let mut ended = BTreeSet::new();
	let mut count = 0;
	for event in events {
		let life = (event.node, event.broadcast.clone(), event.life);
		match event.kind {
			Kind::End => {
				ended.insert(life);
			}
			Kind::Start if ended.contains(&life) => count += 1,
			_ => {}
		}
	}
	count
}

/// Ends shown while a connected publisher still claimed the broadcast.
pub(super) fn false_ends(events: &[Seen]) -> usize {
	events
		.iter()
		.filter(|event| event.kind == Kind::End && event.live)
		.count()
}
