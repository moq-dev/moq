//! The multicast epic's route rule as one node's state machine, beside
//! [`super::path::Router`] until the switch puts lite-07 sessions on it
//! (`quest/m2/multicast/switch.md`).
//!
//! A route leads to a forwarding node that announces claims, never to a publisher: an
//! endpoint's claims belong to the relay it connects to, which announces them under its
//! own [`NodeId`] and revision. A node offers a route to itself once it has announced a
//! claim, and from then on until it restarts. A route carries its path, drain, priority,
//! and cost. A node never uses one whose path names its relay, sends none back over the
//! session it learned it on or to a neighbour already on its path, and never sends one
//! learned over a marked link to another marked link. It ranks routes by drain, then the
//! lowest priority on the path, then the summed cost, each link adding at least 1, so
//! extending a route never improves it; among equal ranks it stays on its next hop.
//!
//! A closed session is named once per neighbour in a fixed-size [`Msg::Dead`], which ends
//! every route its sender offered through it until the sender offers it again or ends
//! it. Per pair of relays a node keeps, for good, the highest session it knows closed
//! (the session floor), and refuses any path naming that pair at or below it: a session
//! orders by its node's start and then its number, so a restart replaces the pair's
//! entry. A node that sent GOAWAY is named once per neighbour in a [`Msg::Drain`], and
//! every route through it ranks as draining. A node advertises once per burst of input,
//! when the caller drains it. There is no hold, no doubt, and no routing timer: the only
//! clock is the reader's debounce from unavailable to offline, which never reaches the
//! wire.
//!
//! A forwarding node's claims are ordered by its revision and certified by floors: a node
//! keeps each neighbour's copy and the newest floor wins, so no stale copy revives a
//! withdrawal. A claims message carries its floor even with nothing changed, and a
//! withdrawal says whether the publisher ended the claim or its announcer lost it. Across
//! a scoped grant only claims travel: the receiver announces each again under its own
//! identity with the chain of relays that did so, and refuses one whose chain names it.
//!
//! Each neighbour asks for a set of prefixes edited in place, as under the path rule: a
//! routing neighbour forwards for the whole domain, so it asks for the whole tree, and
//! an endpoint or scoped neighbour is told the closest claim per broadcast within what it
//! asks, a broader claim presented at the prefix it asks for.
//!
//! The rule is the bake-off's pick (moq-dev/moq#8880): `tests/multi.rs` with `floor`,
//! `epoch`, `batch`, `drain`, and `claimed`, which `tests/relay.rs` compares it against.

use std::{
	cmp::Reverse,
	collections::{BTreeMap, BTreeSet},
	fmt,
	sync::Arc,
	time::Duration,
};

use kio::clock::Instant;

use crate::Path;

use super::reader::Reader;
use super::view::{ancestors, below, within_any, within_set};
use super::{Error, Event, Id, Item, Key, MAX_HOPS, Metric, Rev, Shown};

/// How a forwarding node names itself on a path: its relay, and which start of that
/// relay's process. A start only grows; one that repeats or goes back gets the relay's
/// new sessions refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct NodeId {
	pub relay: Id,
	pub start: u64,
}

/// One node on a route's path, and the session it learned the route on. The forwarding
/// node a route leads to names itself with session 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct Hop {
	pub node: NodeId,
	pub session: u64,
}

/// A route's rank, lower first: draining, then the lowest priority, then the cost.
type Rank = (bool, Reverse<i8>, Metric);

/// What a route says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Offer {
	/// Every node it crossed, the forwarding node it leads to first and the sender last.
	pub path: Arc<[Hop]>,
	/// A node on its path sent GOAWAY: it ranks below every route that avoids one.
	pub draining: bool,
	/// The lowest link priority on its path.
	pub priority: i8,
	/// The summed link costs, each at least 1.
	pub cost: Metric,
	/// A marked link is on its path, so it is never sent over another.
	pub marked: bool,
}

impl Offer {
	fn rank(&self) -> Rank {
		(self.draining, Reverse(self.priority), self.cost)
	}
}

/// Whether `path` crosses any start of `relay`.
fn names(path: &[Hop], relay: Id) -> bool {
	path.iter().any(|hop| hop.node.relay == relay)
}

/// The pairs of relays a path joins, each with where its session stands in the pair's
/// order: the node that learned the route over a session, the node at its far end, and
/// that session as the first node's start and number.
fn pairs(path: &[Hop]) -> impl Iterator<Item = ((Id, Id), (u64, u64))> + '_ {
	path.windows(2).map(|pair| floor_of(pair[1], pair[0].node))
}

/// The pair of relays `hop`'s session joins, as the floor files it, and where the session
/// stands in that pair's order.
fn floor_of(hop: Hop, far: NodeId) -> ((Id, Id), (u64, u64)) {
	((hop.node.relay, far.relay), (hop.node.start, hop.session))
}

/// Every hop of `node`, whatever its session.
fn hops_of(node: NodeId) -> std::ops::RangeInclusive<Hop> {
	Hop { node, session: 0 }..=Hop {
		node,
		session: u64::MAX,
	}
}

/// A claim as a forwarding node announces it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Claim<A> {
	/// What the publisher says of it, such as its Epoch.
	pub item: A,
	/// From the announcing node to the publisher: 0 for its own endpoint.
	pub cost: Metric,
	/// The lowest link priority between the two.
	pub priority: i8,
	/// The way to the publisher crosses a node that sent GOAWAY.
	pub draining: bool,
	/// The relays that announced it again across a scoped grant, the first first.
	pub chain: Vec<Id>,
}

impl<A: Item> Claim<A> {
	fn rank(&self) -> Rank {
		(self.draining, Reverse(self.priority), self.cost)
	}

	fn lowered(&self, by: u64) -> Self {
		Self {
			item: self.item.lowered(by),
			..self.clone()
		}
	}
}

/// A broadcast: a path, and the Instance a claim there serves.
pub(crate) type Broadcast<I> = (Path, I);

/// Claims by broadcast.
pub(crate) type Claims<I, A> = BTreeMap<Broadcast<I>, Claim<A>>;

/// A change to one claim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Change<A> {
	Live(Claim<A>),
	/// The publisher withdrew it.
	Ended,
	/// Its announcer lost it without the publisher saying so, as when a session closes.
	Lost,
}

/// An edit of the prefixes a node asks a neighbour for: the live ones it removes, then the
/// ones it adds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Edit {
	pub remove: Vec<Path>,
	pub add: Vec<Path>,
}

/// A message on the announce stream, either way.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Msg<I, A> {
	/// A route to forwarding node `origin`, or its end.
	Route { origin: NodeId, offer: Option<Offer> },
	/// Changes to `origin`'s claims, complete as of its revision `floor`.
	Claims {
		origin: NodeId,
		changes: Vec<(Broadcast<I>, Change<A>)>,
		floor: Rev,
	},
	/// The session `hop` names closed, which ends every route its sender offered through
	/// it. `far` is the node at its other end.
	Dead { hop: Hop, far: NodeId },
	/// `node` sent GOAWAY.
	Drain { node: NodeId },
	/// Claims with no route: an endpoint's own, or the closest claims across a scoped grant.
	Announce { changes: Vec<(Broadcast<I>, Change<A>)> },
	/// ANNOUNCE_REQUEST: an edit of what the sender asks for.
	Ask(Edit),
	/// The sender's oldest unanswered edit takes effect here, for both sides.
	Edited,
	/// The sender sent the initial set of `prefix`, which the receiver added.
	Synced(Path),
}

/// What a neighbour is to this node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Role {
	/// A forwarding node trusted with the whole tree, named so: routes are exchanged.
	Routing(NodeId),
	/// A forwarding node across a scoped grant covering these prefixes: claims are
	/// exchanged, with a chain, and it may give claims only within them.
	Scoped(Vec<Path>),
	/// A node that never forwards: its claims are this node's own.
	Endpoint,
}

/// One side's configuration of a link.
#[derive(Clone, Debug)]
pub(crate) struct Port {
	/// Strictly positive, so extending a route never leaves it as cheap.
	pub cost: Metric,
	/// The application's local priority for the link: a path ranks by the lowest on it,
	/// higher first, before cost.
	pub priority: i8,
	/// The one mark: a route learned here is never sent to another marked neighbour.
	pub upstream: bool,
	pub role: Role,
}

/// Where a request for a broadcast goes next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Next<N> {
	/// Toward the forwarding node the request names.
	Via(N),
	/// To the endpoint or scoped neighbour whose claim this node announces.
	Local(N),
}

/// The hop delay the bake-off simulated: one round of deliveries.
pub(crate) const HOP: Duration = Duration::from_millis(25);

/// How long a reader shows a claim unavailable before offline: the longest path a route
/// may take, times a hop's delay, since a reroute settles within that.
pub(crate) const fn debounce(hop: Duration) -> Duration {
	hop.saturating_mul(MAX_HOPS as u32)
}

#[derive(Clone, Debug)]
struct Peer<I, A> {
	port: Port,
	/// How this node numbers the session in the hops of the routes it learns there.
	session: u64,
	/// It sent GOAWAY.
	draining: bool,
	/// Its marker arrived: what it sends now answers what this node asked.
	answering: bool,
	/// What this node asked it for, each with whether its initial set arrived.
	asked: BTreeMap<Path, bool>,
	/// What it asks this node for, none inside another.
	asks: BTreeSet<Path>,
	/// The forwarding nodes it offers a route to.
	offers: BTreeSet<NodeId>,
	/// What it was offered of each forwarding node: `None` once a closed session this node
	/// named ended the route, until its next offer or explicit end.
	sent: BTreeMap<NodeId, Option<Offer>>,
	/// Claims it gave this node, as an endpoint or across a grant.
	given: Claims<I, A>,
	/// Claims this node announced to it, by path, as an endpoint or across a grant.
	told: BTreeMap<Path, BTreeMap<I, Claim<A>>>,
}

impl<I, A> Peer<I, A> {
	fn routing(&self) -> Option<NodeId> {
		match self.port.role {
			Role::Routing(id) => Some(id),
			Role::Scoped(_) | Role::Endpoint => None,
		}
	}
}

/// What one neighbour offers for one forwarding node, and its copy of that node's claims.
#[derive(Clone, Debug)]
struct Heard<I, A> {
	offer: Offer,
	/// The neighbour named a closed session on this path, which ends the route until it
	/// offers another.
	ended: bool,
	floor: Rev,
	items: Claims<I, A>,
}

/// A forwarding node's claims as this node knows them, complete as of `floor`.
#[derive(Clone, Debug)]
struct Known<I, A> {
	floor: Rev,
	items: Claims<I, A>,
}

impl<I, A> Default for Known<I, A> {
	fn default() -> Self {
		Self {
			floor: 0,
			items: BTreeMap::new(),
		}
	}
}

/// The route this node uses, as it stands here: the heard offer with the link added.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Best<N> {
	via: N,
	offer: Offer,
}

/// How candidate routes order, lower first: rank, then the next hop in use, the shorter
/// path, and the neighbour.
type Order<N> = (bool, Reverse<i8>, Metric, bool, usize, N);

/// What a router retains, for budgets and leak checks.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Kept {
	/// Routes heard, one per neighbour offering a forwarding node.
	pub routes: usize,
	/// Routes selected, one per forwarding node.
	pub used: usize,
	/// Hops on the heard routes' paths.
	pub hops: usize,
	/// Routes sent, one per neighbour and forwarding node.
	pub sent: usize,
	/// Claims in the merged view of each forwarding node.
	pub items: usize,
	/// Claims in each neighbour's copy.
	pub heard: usize,
	/// Claims told to endpoints and scoped neighbours.
	pub told: usize,
	/// Dead hops and draining nodes.
	pub pending: usize,
	/// Pairs of relays with a session known closed, kept for good.
	pub closed: usize,
	/// Claims shown unavailable while the reader waits to show them offline.
	pub waiting: usize,
}

/// One node's router: `N` keys its neighbours, `I` is the Instance a claim serves, and
/// `A` what else a publisher says of a claim.
#[derive(Clone)]
pub(crate) struct Router<N, I, A> {
	me: NodeId,
	/// How long the reader shows a lost claim unavailable before offline.
	debounce: Duration,
	now: Instant,
	sessions: u64,
	peers: BTreeMap<N, Peer<I, A>>,
	/// The routing neighbours asking for the whole tree, which routes go to.
	routing: BTreeSet<N>,
	/// Routing neighbours that newly asked, owed every route as it stands.
	fresh: BTreeSet<N>,
	heard: BTreeMap<NodeId, BTreeMap<N, Heard<I, A>>>,
	/// The heard routes whose path holds each hop, ended or not.
	rx_hops: BTreeMap<Hop, BTreeMap<N, BTreeSet<NodeId>>>,
	/// The heard routes whose path joins each pair of relays, as the floor files them.
	rx_pairs: BTreeMap<(Id, Id), BTreeSet<(NodeId, N)>>,
	/// How many heard routes that are not ended hold each hop.
	live: BTreeMap<Hop, usize>,
	/// Hops and nodes no live heard route may name any more, for [`Self::forget`].
	unnamed: (BTreeSet<Hop>, BTreeSet<NodeId>),
	/// Closed sessions a live heard route still names, each with its far end.
	dead: BTreeMap<Hop, NodeId>,
	/// Per pair of relays, the highest session known closed, as the near relay's start
	/// and its session number.
	closed: BTreeMap<(Id, Id), (u64, u64)>,
	/// Nodes named as draining that a live heard route still crosses.
	draining: BTreeSet<NodeId>,
	/// Forwarding nodes whose route changed since the neighbours were last told.
	stale: BTreeSet<NodeId>,
	best: BTreeMap<NodeId, Best<N>>,
	/// The sent routes, live, whose path holds each hop.
	tx_hops: BTreeMap<Hop, BTreeMap<N, BTreeSet<NodeId>>>,
	/// The neighbours sent a route to each forwarding node, ended or not.
	sent_to: BTreeMap<NodeId, BTreeSet<N>>,
	known: BTreeMap<NodeId, Known<I, A>>,
	/// This node's revision of its own claims.
	rev: Rev,
	/// It has announced a claim, so it offers a route to itself.
	announced: bool,
	/// Per broadcast, the claims each endpoint or scoped neighbour gave this node.
	givers: BTreeMap<Broadcast<I>, BTreeMap<N, Claim<A>>>,
	/// The neighbour behind each of this node's own claims.
	sources: BTreeMap<Broadcast<I>, N>,
	/// The forwarding nodes claiming each broadcast, by path.
	claimants: BTreeMap<Path, BTreeMap<I, BTreeSet<NodeId>>>,
	/// Per forwarding node, how many of its broadcasts another node claims too.
	shared: BTreeMap<NodeId, usize>,
	/// The endpoints and scoped neighbours asking for each prefix.
	askers: BTreeMap<Path, BTreeSet<N>>,
	/// Broadcasts whose last removal was the publisher's own withdrawal.
	ended: BTreeSet<Broadcast<I>>,
	dirty: BTreeSet<Broadcast<I>>,
	/// Claims shown unavailable since they lost every claimant, by when they go offline.
	waiting: BTreeMap<Broadcast<I>, Instant>,
	deadlines: BTreeSet<(Instant, Broadcast<I>)>,
	reader: Reader<A, I>,
	out: Vec<(N, Msg<I, A>)>,
	/// Routes, claims, hops, and neighbours visited, for per-change work checks.
	#[cfg(test)]
	work: std::cell::Cell<usize>,
	/// This node's own claims at each revision, for the search's truth.
	#[cfg(test)]
	history: Vec<Claims<I, A>>,
	/// How many copies of a forwarding node's claims this node dropped.
	#[cfg(test)]
	forgets: u32,
	/// A defect a fixture plants, to show a check catches it.
	#[cfg(test)]
	mutant: Option<Mutant>,
	/// Whether each input ends by rebuilding every index to compare: on but where a
	/// benchmark's mesh is too large for it.
	#[cfg(test)]
	checked: bool,
}

/// Whether a fixture planted the defect `$mutant` in `$router`: never outside tests.
macro_rules! mutated {
	($router:expr, $mutant:ident) => {{
		#[cfg(test)]
		let planted = $router.mutant == Some(Mutant::$mutant);
		#[cfg(not(test))]
		let planted = false;
		planted
	}};
}

/// A defect a fixture plants in a router, to show the property it breaks is checked.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mutant {
	/// A claim its announcer lost shows as ended (NoFalseEnd).
	LostEnds,
	/// An older copy of a node's claims is merged into a newer one (NoRevival).
	OldFloor,
	/// A copy that lagged is applied as if it were current (Truthful).
	Lagging,
	/// A closed session is never forgotten (QuietIsSettled).
	KeepDead,
	/// Every input re-advertises every route, changed or not (Budget).
	Chatter,
}

impl<N: Key, I: Ord + Clone + fmt::Debug, A: Item> Router<N, I, A> {
	/// A node with no sessions, naming itself `me`, whose reader waits `debounce` before
	/// showing a lost claim offline.
	pub fn new(me: NodeId, debounce: Duration) -> Self {
		Self {
			me,
			debounce,
			now: Instant::ZERO,
			sessions: 0,
			peers: BTreeMap::new(),
			routing: BTreeSet::new(),
			fresh: BTreeSet::new(),
			heard: BTreeMap::new(),
			rx_hops: BTreeMap::new(),
			rx_pairs: BTreeMap::new(),
			live: BTreeMap::new(),
			unnamed: (BTreeSet::new(), BTreeSet::new()),
			dead: BTreeMap::new(),
			closed: BTreeMap::new(),
			draining: BTreeSet::new(),
			stale: BTreeSet::new(),
			best: BTreeMap::new(),
			tx_hops: BTreeMap::new(),
			sent_to: BTreeMap::new(),
			known: BTreeMap::new(),
			rev: 0,
			announced: false,
			givers: BTreeMap::new(),
			sources: BTreeMap::new(),
			claimants: BTreeMap::new(),
			shared: BTreeMap::new(),
			askers: BTreeMap::new(),
			ended: BTreeSet::new(),
			dirty: BTreeSet::new(),
			waiting: BTreeMap::new(),
			deadlines: BTreeSet::new(),
			reader: Reader::default(),
			out: Vec::new(),
			#[cfg(test)]
			work: std::cell::Cell::new(0),
			#[cfg(test)]
			history: vec![BTreeMap::new()],
			#[cfg(test)]
			forgets: 0,
			#[cfg(test)]
			mutant: None,
			#[cfg(test)]
			checked: true,
		}
	}

	/// How this node names itself on a path.
	pub fn me(&self) -> NodeId {
		self.me
	}

	/// What to send, with every route as it stands now: a burst of input is read first.
	pub fn drain(&mut self) -> Vec<(N, Msg<I, A>)> {
		self.advertise();
		std::mem::take(&mut self.out)
	}

	/// Take the reader events, in order.
	pub fn events(&mut self) -> Vec<Event<A, I>> {
		self.reader.events()
	}

	/// How the reader sees each broadcast.
	pub fn shown(&self) -> &BTreeMap<(I, Path), Shown<A>> {
		self.reader.shown()
	}

	/// The time is `now`: a claim lost from here waits its debounce from it.
	pub fn clock(&mut self, now: Instant) {
		self.now = self.now.max(now);
	}

	/// When the reader next shows a waiting claim offline.
	pub fn deadline(&self) -> Option<Instant> {
		self.deadlines.first().map(|(at, _)| *at)
	}

	/// It is `now`: show offline each claim that waited out its debounce.
	pub fn timeout(&mut self, now: Instant) {
		self.clock(now);
		let mut due: BTreeMap<I, BTreeMap<Path, Option<Shown<A>>>> = BTreeMap::new();
		while let Some((at, _)) = self.deadlines.first()
			&& *at <= self.now
		{
			let (_, (path, instance)) = self.deadlines.pop_first().expect("a deadline");
			self.visit();
			let broadcast = (path, instance);
			self.waiting.remove(&broadcast);
			let (path, instance) = broadcast;
			due.entry(instance).or_default().insert(path, None);
		}
		for (instance, claims) in due {
			self.reader.apply(instance, |_| true, claims, |_| false);
		}
	}

	// --- Sessions ---

	/// A session to `peer` opened. This node asks it at once, and answers nothing until it
	/// asks in turn.
	pub fn connect(&mut self, peer: N, port: Port) -> Result<(), Error> {
		if port.cost == 0 {
			return Err(Error::ZeroCost);
		}
		if self.peers.contains_key(&peer) {
			return Err(Error::Duplicate);
		}
		self.sessions += 1;
		let ask: BTreeSet<Path> = match &port.role {
			// Of a scoped neighbour, only what its grant covers, the wider of two nested.
			Role::Scoped(regions) => (regions.iter())
				.filter(|region| !ancestors(region).any(|above| above != **region && regions.contains(&above)))
				.cloned()
				.collect(),
			Role::Routing(_) | Role::Endpoint => BTreeSet::from([Path::empty()]),
		};
		self.peers.insert(
			peer,
			Peer {
				port,
				session: self.sessions,
				draining: false,
				answering: false,
				asked: ask.iter().map(|prefix| (prefix.clone(), false)).collect(),
				asks: BTreeSet::new(),
				offers: BTreeSet::new(),
				sent: BTreeMap::new(),
				given: BTreeMap::new(),
				told: BTreeMap::new(),
			},
		);
		let edit = Edit {
			remove: Vec::new(),
			add: ask.into_iter().collect(),
		};
		self.out.push((peer, Msg::Ask(edit)));
		Ok(())
	}

	/// The session to `peer` closed: everything learned or sent on it goes, and a routing
	/// neighbour's session is named to the neighbours this node sent a route through it.
	pub fn disconnect(&mut self, peer: N) {
		let Some(gone) = self.peers.remove(&peer) else {
			return;
		};
		self.routing.remove(&peer);
		self.fresh.remove(&peer);
		// Nothing still unsent goes to a session that closed.
		self.out.retain(|(to, _)| *to != peer);
		for (origin, offer) in &gone.sent {
			self.unsend(peer, *origin, offer.as_ref());
			if let Some(peers) = self.sent_to.get_mut(origin) {
				peers.remove(&peer);
				if peers.is_empty() {
					self.sent_to.remove(origin);
				}
			}
		}
		let mut touched = BTreeSet::new();
		for &origin in &gone.offers {
			self.unhear(origin, peer);
			touched.insert(origin);
		}
		if gone.routing().is_none() {
			for prefix in &gone.asks {
				self.unask(prefix, peer);
			}
		}
		for broadcast in gone.given.into_keys() {
			self.ungive(&broadcast, peer);
			self.restate(broadcast, Change::Lost);
		}
		// The session is named before any route it carried is withdrawn: a neighbour that
		// hears only the withdrawal still takes the routes others offer through it.
		if let Role::Routing(far) = gone.port.role {
			let hop = Hop {
				node: self.me,
				session: gone.session,
			};
			self.closed(hop, far, None, &mut touched);
		}
		for &origin in &gone.offers {
			self.unheard(origin);
		}
		for origin in touched {
			self.reselect(origin);
		}
		self.done();
	}

	/// `peer` sent GOAWAY: every route through it ranks as draining, and so does every
	/// claim it gave.
	pub fn goaway(&mut self, peer: N) {
		let Some(state) = self.peers.get_mut(&peer) else {
			return;
		};
		state.draining = true;
		let Some(node) = state.routing() else {
			// The claims it gave this node now lead through a draining neighbour.
			let broadcasts: Vec<Broadcast<I>> = state.given.keys().cloned().collect();
			for broadcast in broadcasts {
				self.restate(broadcast, Change::Lost);
			}
			self.done();
			return;
		};
		let mut touched = BTreeSet::new();
		self.drains(node, None, &mut touched);
		for origin in touched {
			self.reselect(origin);
		}
		self.done();
	}

	/// `peer` edited what it asks this node for. The edit takes effect behind the marker
	/// this node sends first: what both sets cover stays, and what the added prefixes newly
	/// cover follows, closed by SYNCED for each. A routing neighbour asks for the whole
	/// tree or nothing, since it forwards for every node behind it.
	pub fn edit(&mut self, peer: N, edit: Edit) -> Result<(), Error> {
		let state = self.peers.get(&peer).ok_or(Error::UnknownNeighbor)?;
		let old = state.asks.clone();
		let mut new = old.clone();
		for prefix in &edit.remove {
			if !new.remove(prefix) {
				return Err(Error::UnknownPrefix);
			}
		}
		for prefix in &edit.add {
			if within_any(prefix, &new) || within_set(&new, prefix).next().is_some() {
				return Err(Error::Nested);
			}
			new.insert(prefix.clone());
		}
		let routing = state.routing().is_some();
		if routing && !new.is_empty() && new != BTreeSet::from([Path::empty()]) {
			return Err(Error::Partial);
		}
		self.out.push((peer, Msg::Edited));
		let state = self.peers.get_mut(&peer).expect("a neighbour");
		state.asks = new.clone();
		if routing {
			match (old.is_empty(), new.is_empty()) {
				(true, false) => {
					self.routing.insert(peer);
					self.fresh.insert(peer);
				}
				(false, true) => {
					// Both sides drop the routes at the marker.
					self.routing.remove(&peer);
					self.fresh.remove(&peer);
					let sent = std::mem::take(&mut state.sent);
					for (origin, offer) in sent {
						self.unsend(peer, origin, offer.as_ref());
						if let Some(peers) = self.sent_to.get_mut(&origin) {
							peers.remove(&peer);
							if peers.is_empty() {
								self.sent_to.remove(&origin);
							}
						}
					}
				}
				_ => {}
			}
			self.done();
			return Ok(());
		}
		for prefix in old.difference(&new) {
			// Both sides drop what it covered at the marker.
			let state = self.peers.get_mut(&peer).expect("a neighbour");
			let inside: Vec<Path> = (state.told.get_key_value(prefix).map(|(path, _)| path).into_iter())
				.chain(below(&state.told, prefix).map(|(path, _)| path))
				.cloned()
				.collect();
			for path in inside {
				state.told.remove(&path);
			}
			self.unask(prefix, peer);
		}
		let added: Vec<Path> = new.difference(&old).cloned().collect();
		for prefix in &added {
			self.askers.entry(prefix.clone()).or_default().insert(peer);
			// Every broadcast within it, at its own path.
			let inside: Vec<(Path, I)> = (self.claimants.get_key_value(prefix).into_iter())
				.chain(below(&self.claimants, prefix))
				.flat_map(|(path, claims)| claims.keys().map(move |instance| (path.clone(), instance.clone())))
				.collect();
			// Every Instance claimed above it, presented at the prefix itself.
			let above: BTreeSet<I> = (ancestors(prefix).filter(|above| above != prefix))
				.filter_map(|above| self.claimants.get(&above))
				.flat_map(|claims| claims.keys().cloned())
				.collect();
			for (path, instance) in inside {
				self.export(peer, &path, &instance);
			}
			for instance in above {
				self.export(peer, prefix, &instance);
			}
		}
		for prefix in added {
			self.out.push((peer, Msg::Synced(prefix)));
		}
		self.done();
		Ok(())
	}

	/// Whether `peer` sent the initial set of `prefix`, which this node asks it for, or
	/// `None` when it does not ask for it.
	pub fn is_synced(&self, peer: N, prefix: &Path) -> Option<bool> {
		self.peers.get(&peer)?.asked.get(prefix).copied()
	}

	/// Take in a message `from` sent. An error is a message the rule never sends, which
	/// closes the session; nothing of it is applied.
	pub fn recv(&mut self, from: N, msg: Msg<I, A>) -> Result<(), Error> {
		let peer = self.peers.get_mut(&from).ok_or(Error::UnknownNeighbor)?;
		let routing = peer.routing().is_some();
		match msg {
			Msg::Ask(edit) => return self.edit(from, edit),
			Msg::Edited => {
				peer.answering = true;
				return Ok(());
			}
			Msg::Synced(prefix) => {
				let synced = peer
					.asked
					.get_mut(&prefix)
					.filter(|_| peer.answering)
					.ok_or(Error::Unasked)?;
				*synced = true;
				return Ok(());
			}
			_ if !peer.answering => return Err(Error::Unasked),
			Msg::Route { origin, offer } if routing => self.route(from, origin, offer)?,
			Msg::Claims { origin, changes, floor } if routing => self.claims(from, origin, changes, floor)?,
			Msg::Dead { hop, far } if routing => {
				self.check_dead(from, hop, far)?;
				let mut touched = BTreeSet::new();
				self.closed(hop, far, Some(from), &mut touched);
				for origin in touched {
					self.reselect(origin);
				}
			}
			Msg::Drain { node } if routing => {
				// It names a node only on a live route it offered.
				let named = (self.rx_hops.range(hops_of(node)))
					.filter_map(|(_, by)| by.get(&from))
					.flatten()
					.any(|origin| !self.heard[origin][&from].ended);
				if !named {
					return Err(Error::Unrouted);
				}
				let mut touched = BTreeSet::new();
				self.drains(node, Some(from), &mut touched);
				for origin in touched {
					self.reselect(origin);
				}
			}
			Msg::Announce { changes } if !routing => self.adopt(from, changes)?,
			_ => return Err(Error::Role),
		}
		self.done();
		Ok(())
	}

	/// An input is done: show readers, and announce to endpoints and scoped neighbours.
	/// Routes wait for [`Self::drain`].
	fn done(&mut self) {
		self.refresh();
		#[cfg(test)]
		if self.checked {
			self.check();
		}
	}

	// --- Routes ---

	fn route(&mut self, from: N, origin: NodeId, offer: Option<Offer>) -> Result<(), Error> {
		self.visit();
		if mutated!(self, Chatter) {
			self.stale
				.extend(self.best.keys().copied().chain(self.announced.then_some(self.me)));
		}
		let sender = self.peers[&from].routing().expect("a routing neighbour");
		match offer {
			Some(offer) => {
				// The sender extended a route that led to `origin` and did not name this
				// relay, by its own hop, and never past the longest path.
				let path = &offer.path;
				let built = path.len() >= 2 || offer.cost == 0;
				if origin.relay == self.me.relay
					|| path.first()
						!= Some(&Hop {
							node: origin,
							session: 0,
						}) || path.last().map(|hop| hop.node) != Some(sender)
					|| path.len() > MAX_HOPS
					|| names(path, self.me.relay)
					|| !built
				{
					return Err(Error::Malformed);
				}
				self.hear(origin, from, offer);
			}
			None => {
				if !self.heard.get(&origin).is_some_and(|offers| offers.contains_key(&from)) {
					return Err(Error::Unrouted);
				}
				self.unhear(origin, from);
				self.unheard(origin);
			}
		}
		self.forget();
		self.reselect(origin);
		Ok(())
	}

	/// The neighbour heard from names a closed session only on a live route it offered,
	/// and the node it names at the far end is the one before the session's hop there.
	fn check_dead(&self, from: N, hop: Hop, far: NodeId) -> Result<(), Error> {
		let origins = self.rx_hops.get(&hop).and_then(|by| by.get(&from));
		let mut live = (origins.into_iter().flatten())
			.map(|origin| &self.heard[origin][&from])
			.filter(|heard| !heard.ended)
			.peekable();
		if live.peek().is_none() {
			return Err(Error::Unrouted);
		}
		for heard in live {
			let at = heard.offer.path.iter().position(|named| *named == hop);
			if at.is_none_or(|at| at == 0 || heard.offer.path[at - 1].node != far) {
				return Err(Error::Malformed);
			}
		}
		Ok(())
	}

	/// File `offer` as what `from` says of `origin`, keeping its copy of the claims.
	fn hear(&mut self, origin: NodeId, from: N, offer: Offer) {
		let offers = self.heard.entry(origin).or_default();
		let old = match offers.get_mut(&from) {
			Some(heard) => {
				let ended = std::mem::replace(&mut heard.ended, false);
				Some((std::mem::replace(&mut heard.offer, offer.clone()), ended))
			}
			None => {
				offers.insert(
					from,
					Heard {
						offer: offer.clone(),
						ended: false,
						floor: 0,
						items: BTreeMap::new(),
					},
				);
				self.peers.get_mut(&from).expect("a neighbour").offers.insert(origin);
				None
			}
		};
		if let Some((old, ended)) = old {
			self.untrack(origin, from, &old.path, !ended);
		}
		self.track(origin, from, &offer.path);
	}

	/// `from` no longer offers `origin`: its copy of the claims goes with the route.
	fn unhear(&mut self, origin: NodeId, from: N) {
		let Some(offers) = self.heard.get_mut(&origin) else {
			return;
		};
		let Some(heard) = offers.remove(&from) else {
			return;
		};
		if let Some(peer) = self.peers.get_mut(&from) {
			peer.offers.remove(&origin);
		}
		self.untrack(origin, from, &heard.offer.path, !heard.ended);
	}

	/// The neighbour's word ends `from`'s route to `origin`.
	fn end_heard(&mut self, origin: NodeId, from: N) {
		let heard = self.heard.get_mut(&origin).and_then(|offers| offers.get_mut(&from));
		let Some(heard) = heard.filter(|heard| !heard.ended) else {
			return;
		};
		heard.ended = true;
		let path = heard.offer.path.clone();
		for hop in path.iter() {
			self.unlive(*hop);
		}
	}

	/// No neighbour offers `origin` any more: its claims go, and so does every route this
	/// node sent to it, at once. A copy of its claims learned later may be older than the
	/// one this node passed on, so it travels only on a route that starts afresh.
	fn unheard(&mut self, origin: NodeId) {
		if !self.heard.get(&origin).is_some_and(BTreeMap::is_empty) {
			return;
		}
		self.heard.remove(&origin);
		if let Some(known) = self.known.remove(&origin) {
			self.count(known.items.len());
			for broadcast in known.items.into_keys() {
				self.unclaim(&broadcast, origin);
				self.dirty.insert(broadcast);
			}
			// Without any copy, this node no longer knows how new a claim of `origin` is:
			// a stale one may start it again, until the newer floor behind it arrives.
			#[cfg(test)]
			{
				self.forgets += u32::from(known.floor > 0);
			}
		}
		for peer in self.sent_to.remove(&origin).into_iter().flatten() {
			self.visit();
			let sent = (self.peers.get_mut(&peer)).and_then(|state| state.sent.remove(&origin));
			self.unsend(peer, origin, sent.flatten().as_ref());
			self.out.push((peer, Msg::Route { origin, offer: None }));
		}
	}

	fn track(&mut self, origin: NodeId, from: N, path: &[Hop]) {
		for hop in path {
			self.visit();
			self.rx_hops
				.entry(*hop)
				.or_default()
				.entry(from)
				.or_default()
				.insert(origin);
			*self.live.entry(*hop).or_default() += 1;
		}
		for (pair, _) in pairs(path) {
			self.rx_pairs.entry(pair).or_default().insert((origin, from));
		}
	}

	fn untrack(&mut self, origin: NodeId, from: N, path: &[Hop], live: bool) {
		for hop in path {
			self.visit();
			if let Some(by) = self.rx_hops.get_mut(hop) {
				if let Some(origins) = by.get_mut(&from) {
					origins.remove(&origin);
					if origins.is_empty() {
						by.remove(&from);
					}
				}
				if by.is_empty() {
					self.rx_hops.remove(hop);
				}
			}
			if live {
				self.unlive(*hop);
			}
		}
		for (pair, _) in pairs(path) {
			if let Some(routes) = self.rx_pairs.get_mut(&pair) {
				routes.remove(&(origin, from));
				if routes.is_empty() {
					self.rx_pairs.remove(&pair);
				}
			}
		}
	}

	fn unlive(&mut self, hop: Hop) {
		let count = self.live.get_mut(&hop).expect("a live hop");
		*count -= 1;
		if *count == 0 {
			self.live.remove(&hop);
			self.unnamed.0.insert(hop);
			self.unnamed.1.insert(hop.node);
		}
	}

	/// Forget each dead hop and draining node no live heard route names.
	fn forget(&mut self) {
		for hop in std::mem::take(&mut self.unnamed.0) {
			self.visit();
			if !self.live.contains_key(&hop) && !mutated!(self, KeepDead) {
				self.dead.remove(&hop);
			}
		}
		for node in std::mem::take(&mut self.unnamed.1) {
			self.visit();
			if self.live.range(hops_of(node)).next().is_none() {
				self.draining.remove(&node);
			}
		}
	}

	/// A session closed: this node's own when `from` is `None`. The origins whose routes
	/// it may change go in `touched`.
	fn closed(&mut self, hop: Hop, far: NodeId, from: Option<N>, touched: &mut BTreeSet<NodeId>) {
		self.visit();
		// The neighbour's word ends every route it offered through the session.
		if let Some(from) = from {
			let origins: Vec<NodeId> = (self.rx_hops.get(&hop).and_then(|by| by.get(&from)))
				.into_iter()
				.flatten()
				.copied()
				.collect();
			for origin in origins {
				self.end_heard(origin, from);
				touched.insert(origin);
			}
		}
		// A path naming this relay is refused anyway, so its own sessions need no floor.
		if hop.node.relay != self.me.relay {
			let (pair, at) = floor_of(hop, far);
			let highest = self.closed.entry(pair).or_default();
			if at > *highest {
				*highest = at;
				let routes = self.rx_pairs.get(&pair).into_iter().flatten();
				touched.extend(routes.map(|(origin, _)| *origin));
			}
		}
		let named = self.live.contains_key(&hop);
		let tell: Vec<N> = (self.tx_hops.get(&hop).into_iter())
			.flat_map(BTreeMap::keys)
			.filter(|peer| Some(**peer) != from)
			.copied()
			.collect();
		if !self.dead.contains_key(&hop) && (named || !tell.is_empty() || from.is_none()) {
			if named {
				self.dead.insert(hop, far);
				let routes = self.rx_hops.get(&hop).into_iter().flat_map(BTreeMap::values).flatten();
				touched.extend(routes.copied());
			}
			for peer in tell {
				self.out.push((peer, Msg::Dead { hop, far }));
				let origins = self.tx_hops[&hop][&peer].clone();
				for origin in origins {
					let sent = (self.peers.get_mut(&peer))
						.and_then(|state| state.sent.get_mut(&origin))
						.and_then(Option::take);
					self.unsend(peer, origin, sent.as_ref());
					// Owed its next offer, or its explicit end.
					self.stale.insert(origin);
				}
			}
		}
		self.forget();
	}

	/// `node` is draining: `from` said so, or its own GOAWAY when `None`.
	fn drains(&mut self, node: NodeId, from: Option<N>, touched: &mut BTreeSet<NodeId>) {
		self.visit();
		if !self.draining.insert(node) {
			return;
		}
		let tell: BTreeSet<N> = (self.tx_hops.range(hops_of(node)))
			.flat_map(|(_, by)| by.keys())
			.filter(|peer| Some(**peer) != from)
			.copied()
			.collect();
		for peer in tell {
			self.out.push((peer, Msg::Drain { node }));
		}
		self.unnamed.1.insert(node);
		self.forget();
		let routes = (self.rx_hops.range(hops_of(node))).flat_map(|(_, by)| by.values().flatten());
		touched.extend(routes.copied());
	}

	/// Whether a session on `path` is at or below its pair's floor.
	fn floored(&self, path: &[Hop]) -> bool {
		pairs(path).any(|(pair, at)| self.closed.get(&pair).is_some_and(|highest| at <= *highest))
	}

	/// Pick the route to `origin`, and mark it stale for the neighbours if it changed.
	fn reselect(&mut self, origin: NodeId) {
		let current = self.best.get(&origin).map(|best| best.via);
		let mut pick: Option<(Best<N>, Order<N>)> = None;
		for (&via, heard) in self.heard.get(&origin).into_iter().flatten() {
			self.visit();
			let peer = &self.peers[&via];
			let offer = &heard.offer;
			if heard.ended
				|| offer.path.len() >= MAX_HOPS
				|| self.floored(&offer.path)
				|| offer.path.iter().any(|hop| self.dead.contains_key(hop))
			{
				continue;
			}
			let draining =
				offer.draining || peer.draining || offer.path.iter().any(|hop| self.draining.contains(&hop.node));
			let mine = Best {
				via,
				offer: Offer {
					path: offer.path.clone(),
					draining,
					priority: offer.priority.min(peer.port.priority),
					cost: offer.cost + peer.port.cost,
					marked: offer.marked || peer.port.upstream,
				},
			};
			let (draining, priority, cost) = mine.offer.rank();
			// Among equal ranks a node stays on its next hop.
			let key = (draining, priority, cost, current != Some(via), offer.path.len(), via);
			if pick.as_ref().is_none_or(|(_, best)| key < *best) {
				pick = Some((mine, key));
			}
		}
		let pick = pick.map(|(best, _)| best);
		let was = self.best.get(&origin);
		if was == pick.as_ref() {
			return;
		}
		let flipped = was.is_some() != pick.is_some();
		let moved = was.map(|best| best.offer.rank()) != pick.as_ref().map(|best| best.offer.rank());
		match pick {
			Some(best) => self.best.insert(origin, best),
			None => self.best.remove(&origin),
		};
		// Whether its claims are reachable follows the route, and so does their cost,
		// which an endpoint or scoped neighbour is told and which may move the closest
		// claim of a broadcast another node claims too.
		let exported = !self.askers.is_empty() || self.shared.get(&origin).is_some_and(|count| *count > 0);
		if (flipped || (moved && exported))
			&& let Some(known) = self.known.get(&origin)
		{
			self.count(known.items.len());
			self.dirty.extend(known.items.keys().cloned());
		}
		self.stale.insert(origin);
	}

	/// What `peer` should hear of `origin` now.
	fn offer_for(&self, origin: NodeId, peer: N) -> Option<Offer> {
		let to = self.peers.get(&peer)?;
		let to_id = to.routing()?;
		if origin == self.me {
			// Nothing routes to a node that announces nothing.
			return self.announced.then(|| Offer {
				path: Arc::new([Hop {
					node: self.me,
					session: 0,
				}]),
				draining: false,
				priority: i8::MAX,
				cost: 0,
				marked: false,
			});
		}
		let best = self.best.get(&origin)?;
		// Never back over the session it came on, to a node already on its path, or from
		// one marked link to another.
		if best.via == peer || names(&best.offer.path, to_id.relay) || (best.offer.marked && to.port.upstream) {
			return None;
		}
		let mut path = best.offer.path.to_vec();
		path.push(Hop {
			node: self.me,
			session: self.peers[&best.via].session,
		});
		Some(Offer {
			path: path.into(),
			..best.offer.clone()
		})
	}

	/// Tell every routing neighbour of each route that changed, and a neighbour that newly
	/// asked of every route.
	fn advertise(&mut self) {
		for peer in std::mem::take(&mut self.fresh) {
			let origins: BTreeSet<NodeId> = (self.best.keys().copied())
				.chain(self.announced.then_some(self.me))
				.collect();
			for origin in origins {
				self.advertise_to(origin, peer);
			}
			self.out.push((peer, Msg::Synced(Path::empty())));
		}
		let peers: Vec<N> = self.routing.iter().copied().collect();
		while let Some(origin) = self.stale.pop_first() {
			for &peer in &peers {
				self.advertise_to(origin, peer);
			}
		}
	}

	fn advertise_to(&mut self, origin: NodeId, peer: N) {
		self.visit();
		let offer = self.offer_for(origin, peer);
		let state = self.peers.get_mut(&peer).expect("a routing neighbour");
		let was = state.sent.get(&origin);
		// Unchanged, unless a closed session ended what it was sent, which it is owed.
		let unchanged = was.is_none_or(Option::is_some) && was.and_then(Option::as_ref) == offer.as_ref();
		if unchanged && !(mutated!(self, Chatter) && offer.is_some()) {
			return;
		}
		let fresh = was.is_none();
		let old = match &offer {
			Some(offer) => state.sent.insert(origin, Some(offer.clone())),
			None => state.sent.remove(&origin),
		};
		self.unsend(peer, origin, old.flatten().as_ref());
		match &offer {
			Some(offer) => {
				for hop in offer.path.iter() {
					let by = self.tx_hops.entry(*hop).or_default();
					by.entry(peer).or_default().insert(origin);
				}
				self.sent_to.entry(origin).or_default().insert(peer);
			}
			None => {
				if let Some(peers) = self.sent_to.get_mut(&origin) {
					peers.remove(&peer);
					if peers.is_empty() {
						self.sent_to.remove(&origin);
					}
				}
			}
		}
		let started = fresh && offer.is_some();
		self.out.push((peer, Msg::Route { origin, offer }));
		// A route that starts brings the claims as they stand, even none: the floor alone
		// outranks an older copy of what was withdrawn.
		if started
			&& let Some(known) = self.known.get(&origin)
			&& known.floor > 0
		{
			self.count(known.items.len());
			let changes = (known.items.iter())
				.map(|(broadcast, claim)| (broadcast.clone(), Change::Live(claim.clone())))
				.collect();
			let floor = known.floor;
			self.out.push((peer, Msg::Claims { origin, changes, floor }));
		}
	}

	/// Unindex a live route sent to `peer`.
	fn unsend(&mut self, peer: N, origin: NodeId, offer: Option<&Offer>) {
		let Some(offer) = offer else {
			return;
		};
		for hop in offer.path.iter() {
			self.visit();
			if let Some(by) = self.tx_hops.get_mut(hop) {
				if let Some(origins) = by.get_mut(&peer) {
					origins.remove(&origin);
					if origins.is_empty() {
						by.remove(&peer);
					}
				}
				if by.is_empty() {
					self.tx_hops.remove(hop);
				}
			}
		}
	}

	// --- Claims ---

	fn claims(
		&mut self,
		from: N,
		origin: NodeId,
		changes: Vec<(Broadcast<I>, Change<A>)>,
		floor: Rev,
	) -> Result<(), Error> {
		self.count(changes.len());
		let heard = (self.heard.get_mut(&origin))
			.and_then(|offers| offers.get_mut(&from))
			.ok_or(Error::Unrouted)?;
		// Each message moves the sender's copy to a newer revision.
		if floor <= heard.floor || changes.iter().any(|(_, change)| !well_built(change)) {
			return Err(Error::Malformed);
		}
		let was = heard.floor;
		for (broadcast, change) in &changes {
			match change {
				Change::Live(claim) => heard.items.insert(broadcast.clone(), claim.clone()),
				Change::Ended | Change::Lost => heard.items.remove(broadcast),
			};
		}
		heard.floor = floor;
		let known = self.known.entry(origin).or_default();
		// The newest floor wins, so an older copy never revives what a newer one withdrew.
		if floor <= known.floor && !mutated!(self, OldFloor) {
			return Ok(());
		}
		let mut visited = 0;
		let changes = match was == known.floor || mutated!(self, Lagging) {
			// The copy matched this node's at the same revision, so the message's changes
			// bring it to the new one.
			true => changes,
			// A copy that lagged overtook it: what differs is the difference.
			false => {
				let heard = &self.heard[&origin][&from];
				let mut diff: Vec<(Broadcast<I>, Change<A>)> = (heard.items.iter())
					.filter(|(broadcast, claim)| known.items.get(*broadcast) != Some(claim))
					.map(|(broadcast, claim)| (broadcast.clone(), Change::Live(claim.clone())))
					.collect();
				// What the newer copy lacks is lost, unless this message says its
				// publisher ended it.
				let ended = |broadcast: &Broadcast<I>| {
					(changes.iter()).any(|(named, change)| named == broadcast && *change == Change::Ended)
				};
				diff.extend(
					(known.items.keys())
						.filter(|broadcast| !heard.items.contains_key(*broadcast))
						.map(|broadcast| match ended(broadcast) {
							true => (broadcast.clone(), Change::Ended),
							false => (broadcast.clone(), Change::Lost),
						}),
				);
				visited = heard.items.len() + known.items.len();
				diff
			}
		};
		self.count(visited);
		let known = self.known.get_mut(&origin).expect("known");
		known.floor = known.floor.max(floor);
		self.publish(origin, changes);
		Ok(())
	}

	/// Apply `changes` to this node's view of `origin` and pass them, with its floor, to
	/// every neighbour it offers `origin` to.
	fn publish(&mut self, origin: NodeId, changes: Vec<(Broadcast<I>, Change<A>)>) {
		let known = self.known.entry(origin).or_default();
		let floor = known.floor;
		let mut claimed = Vec::new();
		let mut unclaimed = Vec::new();
		for (broadcast, change) in &changes {
			match change {
				Change::Live(claim) => {
					if known.items.insert(broadcast.clone(), claim.clone()).is_none() {
						claimed.push(broadcast.clone());
					}
					self.ended.remove(broadcast);
				}
				Change::Ended | Change::Lost => {
					if known.items.remove(broadcast).is_some() {
						unclaimed.push(broadcast.clone());
					}
					if *change == Change::Ended {
						self.ended.insert(broadcast.clone());
					}
				}
			}
			self.dirty.insert(broadcast.clone());
		}
		for broadcast in &claimed {
			self.claim(broadcast, origin);
		}
		for broadcast in &unclaimed {
			self.unclaim(broadcast, origin);
		}
		// Even with nothing changed: the floor alone outranks an older copy elsewhere.
		for &peer in self.sent_to.get(&origin).into_iter().flatten() {
			self.visit();
			let changes = changes.clone();
			self.out.push((peer, Msg::Claims { origin, changes, floor }));
		}
	}

	fn claim(&mut self, (path, instance): &Broadcast<I>, origin: NodeId) {
		let origins = (self.claimants.entry(path.clone()).or_default())
			.entry(instance.clone())
			.or_default();
		if !origins.insert(origin) {
			return;
		}
		if origins.len() == 2 {
			let other = *origins.iter().find(|other| **other != origin).expect("another");
			*self.shared.entry(other).or_default() += 1;
		}
		if origins.len() >= 2 {
			*self.shared.entry(origin).or_default() += 1;
		}
	}

	fn unclaim(&mut self, (path, instance): &Broadcast<I>, origin: NodeId) {
		let Some(claims) = self.claimants.get_mut(path) else {
			return;
		};
		let Some(origins) = claims.get_mut(instance) else {
			return;
		};
		let shared = origins.len() >= 2;
		if !origins.remove(&origin) {
			return;
		}
		let mut unshare = |node: NodeId| {
			let count = self.shared.get_mut(&node).expect("a shared count");
			*count -= 1;
			if *count == 0 {
				self.shared.remove(&node);
			}
		};
		if shared {
			unshare(origin);
		}
		if origins.len() == 1 {
			unshare(*origins.first().expect("one"));
		}
		if origins.is_empty() {
			claims.remove(instance);
			if claims.is_empty() {
				self.claimants.remove(path);
			}
		}
	}

	/// Claims from an endpoint or across a scoped grant become this node's own.
	fn adopt(&mut self, from: N, changes: Vec<(Broadcast<I>, Change<A>)>) -> Result<(), Error> {
		let peer = &self.peers[&from];
		// Only within what this node asked it for.
		let asked: BTreeSet<Path> = peer.asked.keys().cloned().collect();
		if changes.iter().any(|((path, _), _)| !within_any(path, &asked)) {
			return Err(Error::Unasked);
		}
		if changes.iter().any(|(_, change)| !well_built(change)) {
			return Err(Error::Malformed);
		}
		let (cost, priority) = (peer.port.cost, peer.port.priority);
		let scoped = matches!(peer.port.role, Role::Scoped(_));
		for (broadcast, change) in changes {
			self.visit();
			let change = match change {
				// A claim that already crossed this relay came round a ring of grants.
				Change::Live(claim) if claim.chain.contains(&self.me.relay) => Change::Lost,
				Change::Live(mut claim) => {
					claim.cost += cost;
					claim.priority = claim.priority.min(priority);
					if scoped {
						claim.chain.push(self.me.relay);
					}
					Change::Live(claim)
				}
				change => change,
			};
			let peer = self.peers.get_mut(&from).expect("a neighbour");
			match change {
				Change::Live(claim) => {
					peer.given.insert(broadcast.clone(), claim.clone());
					self.givers.entry(broadcast.clone()).or_default().insert(from, claim);
					self.restate(broadcast, Change::Lost);
				}
				cause => {
					if peer.given.remove(&broadcast).is_some() {
						self.ungive(&broadcast, from);
						self.restate(broadcast, cause);
					}
				}
			}
		}
		Ok(())
	}

	fn ungive(&mut self, broadcast: &Broadcast<I>, peer: N) {
		if let Some(givers) = self.givers.get_mut(broadcast) {
			givers.remove(&peer);
			if givers.is_empty() {
				self.givers.remove(broadcast);
			}
		}
	}

	/// Announce the cheapest claim any neighbour gave this node for `broadcast`, or
	/// withdraw it for `cause` when none is left.
	fn restate(&mut self, broadcast: Broadcast<I>, cause: Change<A>) {
		let cheapest = (self.givers.get(&broadcast).into_iter().flatten())
			.map(|(&peer, claim)| {
				self.visit();
				// A claim from a neighbour that sent GOAWAY is draining from here on.
				let mut claim = claim.clone();
				claim.draining |= self.peers.get(&peer).is_some_and(|peer| peer.draining);
				(claim, peer)
			})
			.min_by_key(|(claim, peer)| {
				(
					claim.draining,
					Reverse(claim.priority),
					claim.cost,
					claim.chain.len(),
					*peer,
				)
			});
		let me = self.me;
		let was = self.known.get(&me).and_then(|known| known.items.get(&broadcast));
		let change = match cheapest {
			Some((claim, peer)) => {
				self.sources.insert(broadcast.clone(), peer);
				if was == Some(&claim) {
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
		if !self.announced {
			// Its first claim: the route starts, and brings the claim with it.
			self.announced = true;
			self.stale.insert(me);
		}
		self.publish(me, vec![(broadcast, change)]);
		#[cfg(test)]
		self.history.push(self.known[&me].items.clone());
	}

	// --- Requests ---

	/// The forwarding node a request for `broadcast` names from here: the closest claim.
	pub fn pick(&self, broadcast: &Broadcast<I>) -> Option<NodeId> {
		self.closest(broadcast).map(|(origin, _)| origin)
	}

	/// Where a request for `broadcast` naming `origin` goes from here. `None` fails it.
	pub fn step(&self, origin: NodeId, broadcast: &Broadcast<I>) -> Option<Next<N>> {
		match origin == self.me {
			true => self.sources.get(broadcast).copied().map(Next::Local),
			false => self.best.get(&origin).map(|best| Next::Via(best.via)),
		}
	}

	fn closest(&self, (path, instance): &Broadcast<I>) -> Option<(NodeId, &Claim<A>)> {
		let origins = self.claimants.get(path)?.get(instance)?;
		let broadcast = (path.clone(), instance.clone());
		(origins.iter())
			.filter_map(|&origin| {
				self.visit();
				let claim = self.known.get(&origin)?.items.get(&broadcast)?;
				let rank = match origin == self.me {
					true => claim.rank(),
					false => {
						let (draining, Reverse(priority), cost) = self.best.get(&origin)?.offer.rank();
						(
							draining || claim.draining,
							Reverse(priority.min(claim.priority)),
							cost + claim.cost,
						)
					}
				};
				Some(((rank, claim.chain.len(), origin), claim))
			})
			.min_by(|(a, _), (b, _)| a.cmp(b))
			.map(|((_, _, origin), claim)| (origin, claim))
	}

	// --- Readers, endpoints, and scoped neighbours ---

	/// Bring what the reader sees, and what endpoints and scoped neighbours are told, up to
	/// date for every broadcast that may have changed.
	fn refresh(&mut self) {
		let dirty = std::mem::take(&mut self.dirty);
		let mut now: BTreeMap<I, BTreeMap<Path, Option<Shown<A>>>> = BTreeMap::new();
		for broadcast in &dirty {
			self.visit();
			let (path, instance) = broadcast;
			let closest = self.closest(broadcast).map(|(_, claim)| claim.item.clone());
			let claimed = (self.claimants.get(path).and_then(|claims| claims.get(instance)))
				.map(|origins| origins.first().copied());
			let shown = match claimed {
				Some(first) => {
					self.unwait(broadcast);
					let available = closest.is_some();
					// Unreachable, it still shows what a claimant says.
					let item = closest.or_else(|| {
						let origin = first?;
						Some(self.known.get(&origin)?.items.get(broadcast)?.item.clone())
					});
					Some(item.map(|item| Shown {
						item,
						available,
						presented: false,
					}))
				}
				None if self.ended.contains(broadcast) || mutated!(self, LostEnds) => {
					self.unwait(broadcast);
					Some(None)
				}
				// Lost: shown unavailable for the debounce, in case a claimant returns.
				None => match self.reader.get(instance.clone(), path) {
					Some(_) if self.waiting.contains_key(broadcast) => None,
					Some(_) if self.debounce.is_zero() => Some(None),
					Some(was) => {
						let item = was.item.clone();
						let at = self.now.saturating_add(self.debounce);
						self.waiting.insert(broadcast.clone(), at);
						self.deadlines.insert((at, broadcast.clone()));
						Some(Some(Shown {
							item,
							available: false,
							presented: false,
						}))
					}
					None => None,
				},
			};
			if let Some(shown) = shown {
				now.entry(instance.clone()).or_default().insert(path.clone(), shown);
			}
		}
		let lost_ends = mutated!(self, LostEnds);
		let Self { reader, ended, .. } = self;
		for (instance, claims) in now {
			let ended = |claim: &Path| lost_ends || ended.contains(&(claim.clone(), instance.clone()));
			reader.apply(instance.clone(), |_| true, claims, ended);
		}
		for broadcast in &dirty {
			self.exports(broadcast);
		}
		for broadcast in dirty {
			self.ended.remove(&broadcast);
		}
	}

	fn unwait(&mut self, broadcast: &Broadcast<I>) {
		if let Some(at) = self.waiting.remove(broadcast) {
			self.deadlines.remove(&(at, broadcast.clone()));
		}
	}

	/// Tell each endpoint and scoped neighbour asking for `broadcast` what it shows there.
	fn exports(&mut self, (path, instance): &Broadcast<I>) {
		if self.askers.is_empty() {
			return;
		}
		// A neighbour asking at or above the claim is shown it at its own path.
		let at: Vec<N> = (ancestors(path).filter_map(|above| self.askers.get(&above)))
			.flatten()
			.copied()
			.collect();
		for peer in at {
			self.export(peer, path, instance);
		}
		// A neighbour asking below it is shown what covers its prefix.
		let under: Vec<(Path, N)> = (below(&self.askers, path))
			.flat_map(|(prefix, peers)| peers.iter().map(move |peer| (prefix.clone(), *peer)))
			.collect();
		for (prefix, peer) in under {
			self.export(peer, &prefix, instance);
		}
	}

	/// Tell `peer` what this node shows it of `instance` at `path` now.
	fn export(&mut self, peer: N, path: &Path, instance: &I) {
		self.visit();
		let claim = self.shown_to(peer, path, instance);
		let state = self.peers.get_mut(&peer).expect("a neighbour");
		let told = state.told.get(path).and_then(|told| told.get(instance));
		if told == claim.as_ref() {
			return;
		}
		let change = match claim {
			Some(claim) => {
				let told = state.told.entry(path.clone()).or_default();
				told.insert(instance.clone(), claim.clone());
				Change::Live(claim)
			}
			None => {
				let told = state.told.get_mut(path).expect("told");
				told.remove(instance);
				if told.is_empty() {
					state.told.remove(path);
				}
				// It ended if the publisher of the claim it stood for ended it.
				let ended = ancestors(path).any(|above| self.ended.contains(&(above, instance.clone())));
				match ended {
					true => Change::Ended,
					false => Change::Lost,
				}
			}
		};
		let changes = vec![((path.clone(), instance.clone()), change)];
		self.out.push((peer, Msg::Announce { changes }));
	}

	/// What `peer` is shown of `instance` at `path`: the closest claim there, or at a
	/// prefix it asks for, the most specific claim above, lowered to it.
	fn shown_to(&self, peer: N, path: &Path, instance: &I) -> Option<Claim<A>> {
		let asks = &self.peers[&peer].asks;
		if !within_any(path, asks) {
			return None;
		}
		if let Some(claim) = self.exported(peer, &(path.clone(), instance.clone())) {
			return Some(claim);
		}
		if !asks.contains(path) {
			return None;
		}
		(ancestors(path).rev().skip(1).zip(1..))
			.find_map(|(above, by)| Some(self.exported(peer, &(above, instance.clone()))?.lowered(by)))
	}

	/// The closest claim for `broadcast` as a neighbour of this node sees it, unless it came
	/// from `peer`.
	fn exported(&self, peer: N, broadcast: &Broadcast<I>) -> Option<Claim<A>> {
		let (origin, claim) = self.closest(broadcast)?;
		if origin == self.me {
			return match self.sources.get(broadcast) == Some(&peer) {
				true => None,
				false => Some(claim.clone()),
			};
		}
		let route = &self.best[&origin].offer;
		Some(Claim {
			item: claim.item.clone(),
			cost: claim.cost + route.cost,
			priority: claim.priority.min(route.priority),
			draining: claim.draining || route.draining,
			chain: claim.chain.clone(),
		})
	}

	fn unask(&mut self, prefix: &Path, peer: N) {
		if let Some(peers) = self.askers.get_mut(prefix) {
			peers.remove(&peer);
			if peers.is_empty() {
				self.askers.remove(prefix);
			}
		}
	}

	// --- Accounting ---

	/// Count one visit.
	fn visit(&self) {
		#[cfg(test)]
		self.work.set(self.work.get() + 1);
	}

	/// Count `count` visits.
	#[cfg_attr(not(test), allow(unused_variables, reason = "only tests count work"))]
	fn count(&self, count: usize) {
		#[cfg(test)]
		self.work.set(self.work.get() + count);
	}
}

/// A claim the rule never builds: a chain past the longest path.
fn well_built<A>(change: &Change<A>) -> bool {
	match change {
		Change::Live(claim) => claim.chain.len() < MAX_HOPS,
		Change::Ended | Change::Lost => true,
	}
}

#[cfg(test)]
impl<N: Key, I: Ord + Clone + fmt::Debug, A: Item> Router<N, I, A> {
	/// How many routes, claims, hops, and neighbours this node has visited.
	pub fn work_done(&self) -> usize {
		self.work.get()
	}

	pub fn kept(&self) -> Kept {
		let heard = || self.heard.values().flat_map(BTreeMap::values);
		Kept {
			routes: heard().count(),
			used: self.best.len(),
			hops: heard().map(|heard| heard.offer.path.len()).sum(),
			sent: self.peers.values().map(|peer| peer.sent.len()).sum(),
			items: self.known.values().map(|known| known.items.len()).sum(),
			heard: heard().map(|heard| heard.items.len()).sum(),
			told: (self.peers.values().flat_map(|peer| peer.told.values()))
				.map(BTreeMap::len)
				.sum(),
			pending: self.dead.len() + self.draining.len(),
			closed: self.closed.len(),
			waiting: self.waiting.len(),
		}
	}

	/// Plant `mutant`.
	pub fn mutate(&mut self, mutant: Mutant) {
		self.mutant = Some(mutant);
	}

	/// Stop rebuilding every index after each input, for a benchmark's large mesh.
	pub fn unchecked(&mut self) {
		self.checked = false;
	}

	/// This node's own claims at each revision, the first empty.
	pub fn history(&self) -> &[Claims<I, A>] {
		&self.history
	}

	/// How many copies of a forwarding node's claims this node dropped, after each of which
	/// a stale copy may start a claim it saw end.
	pub fn forgets(&self) -> u32 {
		self.forgets
	}

	/// Whether anything waits for [`Self::drain`].
	pub fn pending(&self) -> bool {
		!self.out.is_empty() || !self.stale.is_empty() || !self.fresh.is_empty()
	}

	/// The route this node uses to `origin`: the neighbour, and the offer as it stands here.
	pub fn best(&self, origin: NodeId) -> Option<(N, &Offer)> {
		self.best.get(&origin).map(|best| (best.via, &best.offer))
	}

	/// The forwarding nodes this node routes to.
	pub fn routes(&self) -> Vec<NodeId> {
		self.best.keys().copied().collect()
	}

	/// What each neighbour offers for `origin`, and whether a closed session ended it.
	pub fn heard(&self, origin: NodeId) -> Vec<(N, &Offer, bool)> {
		(self.heard.get(&origin).into_iter().flatten())
			.map(|(peer, heard)| (*peer, &heard.offer, heard.ended))
			.collect()
	}

	/// The pairs of relays with a session known closed, and where each floor stands.
	pub fn floors(&self) -> &BTreeMap<(Id, Id), (u64, u64)> {
		&self.closed
	}

	/// This node's revision of its own claims, and each of its claims as it stands.
	pub fn own(&self) -> (Rev, Claims<I, A>) {
		let items = self.known.get(&self.me).map(|known| known.items.clone());
		(self.rev, items.unwrap_or_default())
	}

	/// Every copy of a forwarding node's claims this node holds, with its floor: each
	/// neighbour's, and the merged view.
	pub fn copies(&self) -> Vec<(NodeId, Rev, &Claims<I, A>)> {
		let heard = (self.heard.iter())
			.flat_map(|(origin, offers)| offers.values().map(move |heard| (*origin, heard.floor, &heard.items)));
		let known = (self.known.iter())
			.filter(|(origin, _)| **origin != self.me)
			.map(|(origin, known)| (*origin, known.floor, &known.items));
		heard.chain(known).collect()
	}

	/// Hash what decides this node's future as of its clock, leaving out the indexes,
	/// which follow from the rest, and the work counter. Its output and reader events
	/// must already be taken.
	pub fn fingerprint(&self, hasher: &mut impl std::hash::Hasher) {
		let Self {
			me,
			debounce,
			now,
			sessions,
			peers,
			routing,
			fresh,
			heard,
			rx_hops: _,
			rx_pairs: _,
			live: _,
			unnamed,
			dead,
			closed,
			draining,
			stale,
			best,
			tx_hops: _,
			sent_to: _,
			known,
			rev,
			announced,
			givers: _,
			sources,
			claimants: _,
			shared: _,
			askers: _,
			ended,
			dirty,
			waiting,
			deadlines: _,
			reader,
			out,
			work: _,
			history: _,
			forgets: _,
			mutant: _,
			checked: _,
		} = self;
		// A deadline counts by how far off it is; one due counts as zero.
		let waiting: BTreeMap<&Broadcast<I>, Duration> = (waiting.iter())
			.map(|(broadcast, at)| (broadcast, at.saturating_duration_since(*now)))
			.collect();
		let state = (
			(me, debounce, sessions, peers, routing, fresh, heard, unnamed),
			(dead, closed, draining, stale, best, known, rev, announced),
			(sources, ended, dirty, waiting, reader.shown(), out),
		);
		super::digest(hasher, &state);
	}

	/// Rebuild every index from what it indexes, and compare.
	fn check(&self) {
		let mut rx_hops: BTreeMap<Hop, BTreeMap<N, BTreeSet<NodeId>>> = BTreeMap::new();
		let mut rx_pairs: BTreeMap<(Id, Id), BTreeSet<(NodeId, N)>> = BTreeMap::new();
		let mut live: BTreeMap<Hop, usize> = BTreeMap::new();
		for (origin, offers) in &self.heard {
			assert!(!offers.is_empty(), "an empty route entry for {origin:?}");
			for (peer, heard) in offers {
				assert!(self.peers[peer].offers.contains(origin), "offers index");
				for hop in heard.offer.path.iter() {
					rx_hops
						.entry(*hop)
						.or_default()
						.entry(*peer)
						.or_default()
						.insert(*origin);
					if !heard.ended {
						*live.entry(*hop).or_default() += 1;
					}
				}
				for (pair, _) in pairs(&heard.offer.path) {
					rx_pairs.entry(pair).or_default().insert((*origin, *peer));
				}
			}
		}
		assert_eq!(self.rx_hops, rx_hops, "heard hops");
		assert_eq!(self.rx_pairs, rx_pairs, "heard pairs");
		assert_eq!(self.live, live, "live hops");
		let mut tx_hops: BTreeMap<Hop, BTreeMap<N, BTreeSet<NodeId>>> = BTreeMap::new();
		let mut sent_to: BTreeMap<NodeId, BTreeSet<N>> = BTreeMap::new();
		let mut givers: BTreeMap<Broadcast<I>, BTreeMap<N, Claim<A>>> = BTreeMap::new();
		let mut askers: BTreeMap<Path, BTreeSet<N>> = BTreeMap::new();
		for (peer, state) in &self.peers {
			for (origin, offer) in &state.sent {
				sent_to.entry(*origin).or_default().insert(*peer);
				for hop in offer.iter().flat_map(|offer| offer.path.iter()) {
					tx_hops
						.entry(*hop)
						.or_default()
						.entry(*peer)
						.or_default()
						.insert(*origin);
				}
			}
			for (broadcast, claim) in &state.given {
				givers
					.entry(broadcast.clone())
					.or_default()
					.insert(*peer, claim.clone());
			}
			if state.routing().is_none() {
				for prefix in &state.asks {
					askers.entry(prefix.clone()).or_default().insert(*peer);
				}
			}
			assert_eq!(
				self.routing.contains(peer),
				state.routing().is_some() && !state.asks.is_empty(),
				"routing index"
			);
		}
		assert_eq!(self.tx_hops, tx_hops, "sent hops");
		assert_eq!(self.sent_to, sent_to, "sent routes");
		assert_eq!(self.givers, givers, "given claims");
		assert_eq!(self.askers, askers, "askers");
		let mut claimants: BTreeMap<Path, BTreeMap<I, BTreeSet<NodeId>>> = BTreeMap::new();
		for (origin, known) in &self.known {
			for (path, instance) in known.items.keys() {
				let claims = claimants.entry(path.clone()).or_default();
				claims.entry(instance.clone()).or_default().insert(*origin);
			}
		}
		let mut shared: BTreeMap<NodeId, usize> = BTreeMap::new();
		for origins in claimants.values().flat_map(BTreeMap::values) {
			if origins.len() >= 2 {
				for origin in origins {
					*shared.entry(*origin).or_default() += 1;
				}
			}
		}
		assert_eq!(self.claimants, claimants, "claimants");
		assert_eq!(self.shared, shared, "shared claims");
		let deadlines: BTreeSet<(Instant, Broadcast<I>)> = (self.waiting.iter())
			.map(|(broadcast, at)| (*at, broadcast.clone()))
			.collect();
		assert_eq!(self.deadlines, deadlines, "deadlines");
		assert!(self.dirty.is_empty(), "refreshed");
	}
}

impl<N: fmt::Debug, I: fmt::Debug, A: fmt::Debug> fmt::Debug for Router<N, I, A> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("Router")
			.field("me", &self.me)
			.field("best", &self.best)
			.field("closed", &self.closed)
			.finish_non_exhaustive()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	const ROOT: &str = "";

	fn p(path: &str) -> Path {
		Path::new(path).expect("a path")
	}

	fn node(relay: Id) -> NodeId {
		NodeId { relay, start: 1 }
	}

	fn port(role: Role) -> Port {
		Port {
			cost: 1,
			priority: 0,
			upstream: false,
			role,
		}
	}

	type Relay = Router<usize, u64, u64>;

	fn relay(me: Id) -> Relay {
		Router::new(node(me), debounce(HOP))
	}

	/// Connect `peer` and exchange asks, as the session's first messages do.
	fn open(router: &mut Relay, peer: usize, role: Role) {
		router.connect(peer, port(role.clone())).unwrap();
		router.recv(peer, Msg::Edited).unwrap();
		let ask = match role {
			Role::Routing(_) => vec![p(ROOT)],
			Role::Scoped(_) | Role::Endpoint => Vec::new(),
		};
		if !ask.is_empty() {
			router
				.edit(
					peer,
					Edit {
						remove: Vec::new(),
						add: ask,
					},
				)
				.unwrap();
		}
	}

	fn live(cost: Metric) -> Change<u64> {
		Change::Live(Claim {
			item: 0,
			cost,
			priority: i8::MAX,
			draining: false,
			chain: Vec::new(),
		})
	}

	/// A claim outside what this node asked a scoped neighbour for is refused whole.
	#[test]
	fn a_claim_outside_the_grant_is_refused() {
		let mut router = relay(1);
		open(&mut router, 2, Role::Scoped(vec![p("a")]));
		let inside = ((p("a/x"), 7), live(0));
		let outside = ((p("b/x"), 7), live(0));
		let err = router.recv(
			2,
			Msg::Announce {
				changes: vec![inside.clone(), outside],
			},
		);
		assert_eq!(err, Err(Error::Unasked));
		assert!(router.shown().is_empty(), "nothing of it applied");
		router.recv(2, Msg::Announce { changes: vec![inside] }).unwrap();
		assert_eq!(router.shown().len(), 1);
	}

	/// A routing neighbour forwards for the whole domain, so it asks for all of it.
	#[test]
	fn a_routing_neighbour_asks_for_the_whole_tree() {
		let mut router = relay(1);
		router.connect(2, port(Role::Routing(node(2)))).unwrap();
		let edit = Edit {
			remove: Vec::new(),
			add: vec![p("a")],
		};
		assert_eq!(router.edit(2, edit), Err(Error::Partial));
	}

	/// Nothing a neighbour sends counts until its answer begins at the marker.
	#[test]
	fn nothing_counts_before_the_marker() {
		let mut router = relay(1);
		router.connect(2, port(Role::Endpoint)).unwrap();
		let changes = vec![((p("a"), 7), live(0))];
		assert_eq!(router.recv(2, Msg::Announce { changes }), Err(Error::Unasked));
	}

	/// Claims and news must name a route the neighbour offered, and a route must lead to
	/// its origin through its sender.
	#[test]
	fn news_about_no_route_is_refused() {
		let mut router = relay(1);
		open(&mut router, 2, Role::Routing(node(2)));
		let hop = Hop {
			node: node(3),
			session: 4,
		};
		assert_eq!(router.recv(2, Msg::Dead { hop, far: node(5) }), Err(Error::Unrouted));
		assert_eq!(router.recv(2, Msg::Drain { node: node(5) }), Err(Error::Unrouted));
		let claims = Msg::Claims {
			origin: node(5),
			changes: Vec::new(),
			floor: 1,
		};
		assert_eq!(router.recv(2, claims), Err(Error::Unrouted));
		let offer = |path: Vec<Hop>| Offer {
			path: path.into(),
			draining: false,
			priority: i8::MAX,
			cost: 1,
			marked: false,
		};
		let origin = Hop {
			node: node(5),
			session: 0,
		};
		let sender = Hop {
			node: node(2),
			session: 1,
		};
		// Not through its sender.
		let route = Msg::Route {
			origin: node(5),
			offer: Some(offer(vec![origin, hop])),
		};
		assert_eq!(router.recv(2, route), Err(Error::Malformed));
		// Through this relay.
		let me = Hop {
			node: NodeId { relay: 1, start: 0 },
			session: 3,
		};
		let route = Msg::Route {
			origin: node(5),
			offer: Some(offer(vec![origin, me, sender])),
		};
		assert_eq!(router.recv(2, route), Err(Error::Malformed));
		let route = Msg::Route {
			origin: node(5),
			offer: Some(offer(vec![origin, sender])),
		};
		router.recv(2, route).unwrap();
		// A dead hop the route crosses, with the wrong node at its far end.
		assert_eq!(
			router.recv(
				2,
				Msg::Dead {
					hop: sender,
					far: node(9)
				}
			),
			Err(Error::Malformed)
		);
		router
			.recv(
				2,
				Msg::Dead {
					hop: sender,
					far: node(5),
				},
			)
			.unwrap();
	}

	/// A claim lost with nothing to replace it shows unavailable for the debounce, then
	/// offline; one that returns meanwhile shows available again, and never went away.
	#[test]
	fn a_lost_claim_waits_out_the_debounce() {
		let mut router = relay(1);
		open(&mut router, 2, Role::Endpoint);
		let claim = |change| Msg::Announce {
			changes: vec![((p("live"), 7), change)],
		};
		let kinds = |router: &mut Relay| router.events().into_iter().map(|event| event.kind).collect::<Vec<_>>();
		router.recv(2, claim(live(0))).unwrap();
		assert_eq!(kinds(&mut router), [super::super::Kind::Start]);
		router.recv(2, claim(Change::Lost)).unwrap();
		assert_eq!(kinds(&mut router), [super::super::Kind::Unavailable]);
		let half = Instant::ZERO.saturating_add(debounce(HOP) / 2);
		router.timeout(half);
		router.recv(2, claim(live(0))).unwrap();
		assert_eq!(kinds(&mut router), [super::super::Kind::Available]);
		assert_eq!(router.deadline(), None);
		router.recv(2, claim(Change::Lost)).unwrap();
		let due = router.deadline().expect("a deadline");
		assert_eq!(due, half.saturating_add(debounce(HOP)));
		router.timeout(due.saturating_sub(Duration::from_millis(1)));
		assert_eq!(kinds(&mut router), [super::super::Kind::Unavailable]);
		router.timeout(due);
		assert_eq!(kinds(&mut router), [super::super::Kind::Offline]);
		// A publisher ending its claim shows at once.
		router.recv(2, claim(live(0))).unwrap();
		router.recv(2, claim(Change::Ended)).unwrap();
		let kinds = kinds(&mut router);
		assert_eq!(kinds, [super::super::Kind::Start, super::super::Kind::End]);
		assert_eq!(router.deadline(), None);
	}

	/// An endpoint asking for a prefix under a broader claim is shown that claim at the
	/// prefix, and an edit swaps what it is shown in place.
	#[test]
	fn an_endpoint_is_shown_what_covers_its_prefix() {
		let mut router = relay(1);
		open(&mut router, 2, Role::Endpoint);
		router
			.recv(
				2,
				Msg::Announce {
					changes: vec![((p("live"), 7), live(0)), ((p("live/a/x"), 8), live(0))],
				},
			)
			.unwrap();
		open(&mut router, 3, Role::Endpoint);
		router.drain();
		router
			.edit(
				3,
				Edit {
					remove: Vec::new(),
					add: vec![p("live/a")],
				},
			)
			.unwrap();
		let out = router.drain();
		let told: Vec<&Msg<u64, u64>> = (out.iter()).filter(|(to, _)| *to == 3).map(|(_, msg)| msg).collect();
		let shown = |path: &str, instance: u64| {
			told.iter().any(|msg| {
				matches!(msg, Msg::Announce { changes } if changes.iter().any(|((at, id), change)| *at == p(path) && *id == instance && matches!(change, Change::Live(_))))
			})
		};
		assert!(shown("live/a", 7), "the broader claim at the prefix: {told:?}");
		assert!(shown("live/a/x", 8), "the claim inside: {told:?}");
		assert_eq!(told.first(), Some(&&Msg::Edited));
		assert_eq!(told.last(), Some(&&Msg::Synced(p("live/a"))));
		// Withdrawing the broader claim withdraws what stood for it.
		router
			.recv(
				2,
				Msg::Announce {
					changes: vec![((p("live"), 7), Change::Ended)],
				},
			)
			.unwrap();
		let out = router.drain();
		assert!(out.iter().any(|(to, msg)| *to == 3
			&& matches!(msg, Msg::Announce { changes } if changes == &vec![((p("live/a"), 7), Change::Ended)])));
		// Swapping its prefix in place drops what the old one covered, silently.
		router
			.edit(
				3,
				Edit {
					remove: vec![p("live/a")],
					add: vec![p("other")],
				},
			)
			.unwrap();
		let out = router.drain();
		let told: Vec<&Msg<u64, u64>> = (out.iter()).filter(|(to, _)| *to == 3).map(|(_, msg)| msg).collect();
		assert_eq!(told, [&Msg::Edited, &Msg::Synced(p("other"))]);
		assert_eq!(router.kept().told, 0);
	}
}
