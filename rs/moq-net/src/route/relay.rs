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
//! extending a route never improves it. Among equal ranks it takes one with no marked link
//! on its path, which it may send to more neighbours, and then stays on its next hop.
//!
//! A closed session is named once per neighbour in a fixed-size [`Msg::Dead`], which ends
//! every route its sender offered through it until the sender offers it again or ends
//! it. Per pair of relays a node keeps, for good, the highest session it knows closed
//! (the session floor), and refuses any path naming that pair at or below it: a session
//! orders by its node's start and then its number, so a restart replaces the pair's
//! entry. A node that sent GOAWAY is named once per neighbour in a [`Msg::Drain`], and
//! every route through it ranks as draining. A node advertises once per burst of input,
//! when the caller drains it. There is no hold, no doubt, and no routing timer: the only
//! clock is the reader's debounce from unavailable to offline, which runs from when the
//! reader first showed the claim unavailable. The route exchange between routing nodes
//! never waits on it. An endpoint or scoped neighbour is told what the reader shows, so
//! it hears a lost claim withdrawn only when the debounce runs out, and a domain past a
//! scoped grant, which showed the claim unavailable from the start, lets it go then too
//! rather than a debounce later.
//!
//! A forwarding node's claims are ordered by its revision and certified by floors: a node
//! keeps each neighbour's copy and the newest floor wins, so no stale copy revives a
//! withdrawal. A claims message carries its floor even with nothing changed, and a
//! withdrawal says whether the publisher ended the claim or its announcer lost it. Across
//! a scoped grant only claims travel: the receiver announces each again under its own
//! identity with the chain of relays that did so, and refuses one whose chain names it or
//! would grow past the longest path.
//!
//! Each neighbour asks for a set of prefixes edited in place, as under the path rule: a
//! routing neighbour forwards for the whole domain, so it asks for the whole tree, and
//! an endpoint or scoped neighbour is told what this node's reader shows per broadcast
//! within what it asks, a broader claim presented at the prefix it asks for. That is the
//! closest claim while one is reachable, and the claim marked unavailable, never
//! withdrawn, while none is and while a lost claim waits out the debounce. A scoped
//! neighbour that took the claim announces it again unavailable, so its domain sees the
//! claim stay; a run it is first told unavailable, which nothing can serve, it takes only
//! once told it available. A request
//! follows the claim at the most specific path covering it that the reader shows, at its
//! claimant too, and fails closed while that one is unavailable or lost. Among the claims
//! of one Instance at one path, only the one shown closest to its own path, at the newest
//! epoch among those, may serve, reachable or not, as `model::origin` ranks them; a
//! route's rank chooses among those alone. A route whose cost a link would carry past the
//! metric is never used, and a claim's cost saturates there.
//!
//! The rule is the bake-off's pick (moq-dev/moq#8880): `tests/multi.rs` with `floor`,
//! `epoch`, `batch`, `drain`, and `claimed`, which `tests/multicast.rs` compares it
//! against. `tests/multicast_search.rs` searches every delivery order on small meshes,
//! and `tests/multicast_cost.rs` sweeps what one event costs.

use std::{
	cmp::Reverse,
	collections::{BTreeMap, BTreeSet, VecDeque},
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

/// How claims of one precedence order, lower first: the rank of the route and the claim
/// together, then the shorter chain, and the claimant.
type ClaimOrder = (Rank, usize, NodeId);

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
	/// No way to the publisher reaches it from the node that told it, which keeps it, as
	/// its reader shows it, while the claim may return. Set only on a claim told to an
	/// endpoint or scoped neighbour, and on one a scoped neighbour's adopter announces.
	pub unavailable: bool,
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

/// Which claims of one Instance at one path may serve it, lower first: the one closest to
/// its own path, then the newest epoch. A route's rank chooses only among claims that tie
/// here, so an older run never stands in for a newer one that lost its route.
fn precedence<A: Item>(item: &A) -> (u64, Reverse<u64>) {
	(item.above(), Reverse(item.epoch()))
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
	/// Its first marker arrived: what it sends now answers what this node asked.
	answering: bool,
	/// What it answers of what this node asked it for, each with whether its initial set
	/// arrived.
	asked: BTreeMap<Path, bool>,
	/// The edits of that set it has yet to answer, oldest first.
	asking: VecDeque<Edit>,
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
	told: BTreeMap<Path, BTreeMap<I, Told<A>>>,
}

/// What a neighbour was told at one path.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Told<A> {
	claim: Claim<A>,
	/// The path of the claim it stands for: the one told at, or a broader one presented
	/// there.
	from: Path,
}

/// The claim a lost broadcast showed, kept while it waits out the debounce so a
/// neighbour that asks meanwhile is told it as one that asked before is.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Gone<N, A> {
	claimant: NodeId,
	claim: Claim<A>,
	/// The neighbour that gave this node's own claim, which is never told it back.
	source: Option<N>,
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

/// How candidate routes order, lower first: rank, then one with no marked link on its
/// path, the next hop in use, the shorter path, the neighbour's Node ID, and its session.
type Order<N> = (bool, Reverse<i8>, Metric, bool, bool, usize, NodeId, N);

/// What changed at a router since its caller last asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Changes<I> {
	/// The forwarding nodes whose selected route changed.
	pub routes: BTreeSet<NodeId>,
	/// The broadcasts whose claims, or what the reader shows of them, may have changed.
	pub broadcasts: BTreeSet<Broadcast<I>>,
}

impl<I> Default for Changes<I> {
	fn default() -> Self {
		Self {
			routes: BTreeSet::new(),
			broadcasts: BTreeSet::new(),
		}
	}
}

/// One claim a request for a broadcast may follow from a node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Way<N, A> {
	/// The forwarding node that announces the claim.
	pub origin: NodeId,
	/// Where a request following it goes next.
	pub next: Next<N>,
	/// The claim as it stands at this node: its cost, priority, and drain joined with
	/// the route's.
	pub claim: Claim<A>,
	/// The one a request naming no node takes.
	pub closest: bool,
}

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
	/// Per forwarding node, the broadcasts its route's rank reaches past whether a reader
	/// can reach them: one another node claims too, whose closest claim the rank decides,
	/// and one an endpoint or scoped neighbour is shown, which is told the claim's cost.
	ranked: BTreeMap<NodeId, BTreeSet<Broadcast<I>>>,
	/// The endpoints and scoped neighbours asking for each prefix.
	askers: BTreeMap<Path, BTreeSet<N>>,
	/// Broadcasts whose last removal was the publisher's own withdrawal.
	ended: BTreeSet<Broadcast<I>>,
	dirty: BTreeSet<Broadcast<I>>,
	/// Claims shown unavailable since they lost every claimant, by when they go offline.
	waiting: BTreeMap<Broadcast<I>, Instant>,
	/// When the reader began showing each claim unavailable, which the debounce runs from.
	since: BTreeMap<Broadcast<I>, Instant>,
	/// What each waiting claim showed.
	gone: BTreeMap<Path, BTreeMap<I, Gone<N, A>>>,
	/// The claims lost in this input, with their claimants, until `refresh` keeps what
	/// a waiting claim showed.
	dropped: BTreeMap<Broadcast<I>, Vec<Gone<N, A>>>,
	deadlines: BTreeSet<(Instant, Broadcast<I>)>,
	reader: Reader<A, I>,
	out: Vec<(N, Msg<I, A>)>,
	/// What changed since [`Self::changes`] last took it, once a caller follows it.
	log: Option<Changes<I>>,
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
			ranked: BTreeMap::new(),
			askers: BTreeMap::new(),
			ended: BTreeSet::new(),
			dirty: BTreeSet::new(),
			waiting: BTreeMap::new(),
			since: BTreeMap::new(),
			gone: BTreeMap::new(),
			dropped: BTreeMap::new(),
			deadlines: BTreeSet::new(),
			reader: Reader::default(),
			out: Vec::new(),
			log: None,
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
			self.since.remove(&broadcast);
			self.ungone(&broadcast);
			let (path, instance) = broadcast;
			due.entry(instance).or_default().insert(path, None);
		}
		let mut gone = Vec::new();
		for (instance, claims) in due {
			gone.extend(claims.keys().map(|path| (path.clone(), instance.clone())));
			self.reader.apply(instance, |_| true, claims, |_| false);
		}
		// As in `refresh`: nothing reads which claims the reader recomputed.
		self.reader.changed();
		// An endpoint or scoped neighbour hears it lost only now.
		for broadcast in &gone {
			self.exports(broadcast);
		}
		if let Some(log) = &mut self.log {
			log.broadcasts.extend(gone);
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
				asked: BTreeMap::new(),
				asking: VecDeque::new(),
				asks: BTreeSet::new(),
				offers: BTreeSet::new(),
				sent: BTreeMap::new(),
				given: BTreeMap::new(),
				told: BTreeMap::new(),
			},
		);
		// A scoped neighbour whose grant covers nothing is asked for nothing.
		if ask.is_empty() {
			return Ok(());
		}
		let edit = Edit {
			remove: Vec::new(),
			add: ask.into_iter().collect(),
		};
		self.peers.get_mut(&peer).expect("just connected").asking.push_back(edit.clone());
		self.out.push((peer, Msg::Ask(edit)));
		Ok(())
	}

	/// Edit what this node asks `peer` for, an endpoint or scoped neighbour: a node that
	/// never forwards asks only for what its readers want. The edit takes effect at the
	/// neighbour's marker, where this node drops what the removed prefixes covered.
	pub fn ask(&mut self, peer: N, edit: Edit) -> Result<(), Error> {
		let state = self.peers.get_mut(&peer).ok_or(Error::UnknownNeighbor)?;
		if state.routing().is_some() {
			return Err(Error::Partial);
		}
		// As the set stands once every edit before this one is answered.
		let mut set: BTreeSet<Path> = state.asked.keys().cloned().collect();
		for pending in &state.asking {
			for prefix in &pending.remove {
				set.remove(prefix);
			}
			set.extend(pending.add.iter().cloned());
		}
		for prefix in &edit.remove {
			if !set.remove(prefix) {
				return Err(Error::UnknownPrefix);
			}
		}
		for prefix in &edit.add {
			if within_any(prefix, &set) || within_set(&set, prefix).next().is_some() {
				return Err(Error::Nested);
			}
			set.insert(prefix.clone());
		}
		state.asking.push_back(edit.clone());
		self.out.push((peer, Msg::Ask(edit)));
		Ok(())
	}

	/// What this node asks `peer` for, once the neighbour answers every edit.
	pub fn asking(&self, peer: N) -> BTreeSet<Path> {
		let Some(state) = self.peers.get(&peer) else {
			return BTreeSet::new();
		};
		let mut set: BTreeSet<Path> = state.asked.keys().cloned().collect();
		for pending in &state.asking {
			for prefix in &pending.remove {
				set.remove(prefix);
			}
			set.extend(pending.add.iter().cloned());
		}
		set
	}

	/// Whether `peer` has answered every edit of what this node asks it for.
	pub fn answered(&self, peer: N) -> bool {
		self.peers.get(&peer).is_some_and(|state| state.asking.is_empty())
	}

	/// Follow what changes from here on, for [`Self::changes`].
	pub fn follow(&mut self) {
		self.log.get_or_insert_default();
	}

	/// Take what changed since the last call, if [`Self::follow`] was called.
	pub fn changes(&mut self) -> Changes<I> {
		self.log.as_mut().map(std::mem::take).unwrap_or_default()
	}

	/// The route this node uses to `origin`: the neighbour, and the offer as it stands here.
	pub fn best(&self, origin: NodeId) -> Option<(N, &Offer)> {
		self.best.get(&origin).map(|best| (best.via, &best.offer))
	}

	/// How `peer`'s link is configured.
	pub fn port(&self, peer: N) -> Option<Port> {
		self.peers.get(&peer).map(|state| state.port.clone())
	}

	/// The broadcasts `origin` claims, as far as this node knows.
	pub fn claimed(&self, origin: NodeId) -> impl Iterator<Item = &Broadcast<I>> {
		self.known.get(&origin).into_iter().flat_map(|known| known.items.keys())
	}

	/// Whether a request `peer` sent may follow `origin`'s claim of `broadcast` from here:
	/// never one that reached this node over a marked link when `peer`'s is marked too, as
	/// `peer` is never told of it.
	pub fn admits(&self, peer: N, origin: NodeId, broadcast: &Broadcast<I>) -> bool {
		let marked = self.peers.get(&peer).is_some_and(|peer| peer.port.upstream);
		!(marked && self.crossed_mark(origin, broadcast))
	}

	/// How many routes and claims `peer` holds of what this node sent it: what a neighbour
	/// that lost everything would be sent again.
	pub fn held(&self, peer: N) -> usize {
		let Some(state) = self.peers.get(&peer) else {
			return 0;
		};
		let claims = |origin: &NodeId| self.known.get(origin).map_or(0, |known| known.items.len());
		let routes: usize = state.sent.keys().map(|origin| 1 + claims(origin)).sum();
		routes + state.told.values().map(BTreeMap::len).sum::<usize>()
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
			let askers = self.askers.entry(prefix.clone()).or_default();
			askers.insert(peer);
			if askers.len() == 1 {
				self.rewatch(prefix);
			}
			// Every broadcast within it, at its own path, claimed or a lost one waiting out
			// the debounce.
			let claimed = (self.claimants.get_key_value(prefix).into_iter())
				.chain(below(&self.claimants, prefix))
				.flat_map(|(path, claims)| claims.keys().map(move |instance| (path.clone(), instance.clone())));
			let waiting = (self.gone.get_key_value(prefix).into_iter())
				.chain(below(&self.gone, prefix))
				.flat_map(|(path, gone)| gone.keys().map(move |instance| (path.clone(), instance.clone())));
			let inside: BTreeSet<(Path, I)> = claimed.chain(waiting).collect();
			// Every Instance claimed or waiting above it, presented at the prefix itself.
			let above: BTreeSet<I> = (ancestors(prefix).filter(|above| above != prefix))
				.flat_map(|above| {
					let claimed = self.claimants.get(&above).into_iter().flat_map(BTreeMap::keys);
					claimed.chain(self.gone.get(&above).into_iter().flat_map(BTreeMap::keys))
				})
				.cloned()
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
		let peer = self.peers.get(&peer)?;
		let pending = || peer.asking.iter().any(|edit| edit.add.contains(prefix));
		(peer.asked.get(prefix).copied()).or_else(|| pending().then_some(false))
	}

	/// Take in a message `from` sent. An error is a message the rule never sends, which
	/// closes the session; nothing of it is applied.
	pub fn recv(&mut self, from: N, msg: Msg<I, A>) -> Result<(), Error> {
		let peer = self.peers.get_mut(&from).ok_or(Error::UnknownNeighbor)?;
		let routing = peer.routing().is_some();
		match msg {
			Msg::Ask(edit) => return self.edit(from, edit),
			Msg::Edited => {
				// It answers this node's oldest edit, which takes effect here for both.
				let edit = peer.asking.pop_front().ok_or(Error::Unasked)?;
				peer.answering = true;
				for prefix in &edit.add {
					peer.asked.insert(prefix.clone(), false);
				}
				let mut lost = Vec::new();
				for prefix in &edit.remove {
					peer.asked.remove(prefix);
					lost.extend(peer.given.keys().filter(|(path, _)| super::view::within(path, prefix)).cloned());
				}
				for broadcast in lost {
					self.peers.get_mut(&from).expect("a neighbour").given.remove(&broadcast);
					self.ungive(&broadcast, from);
					self.restate(broadcast, Change::Lost);
				}
				self.done();
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
			for (broadcast, claim) in known.items {
				self.unclaim(&broadcast, origin);
				self.dirty.insert(broadcast.clone());
				self.drop_claim(broadcast, origin, claim, None);
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
			// A route no link could extend is past every other, so it is never used.
			let Some(cost) = offer.cost.checked_add(peer.port.cost) else {
				continue;
			};
			let draining =
				offer.draining || peer.draining || offer.path.iter().any(|hop| self.draining.contains(&hop.node));
			let mine = Best {
				via,
				offer: Offer {
					path: offer.path.clone(),
					draining,
					priority: offer.priority.min(peer.port.priority),
					cost,
					marked: offer.marked || peer.port.upstream,
				},
			};
			let (draining, priority, cost) = mine.offer.rank();
			// Among equal ranks one with no marked link, which more neighbours may be sent,
			// and then the next hop in use.
			let key = (
				draining,
				priority,
				cost,
				mine.offer.marked,
				current != Some(via),
				offer.path.len(),
				offer.path.last().expect("a heard route names its sender").node,
				via,
			);
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
		if let Some(log) = &mut self.log {
			log.routes.insert(origin);
		}
		// Whether its claims are reachable follows the route, and so does the rank of each
		// that the rank reaches.
		if flipped && let Some(known) = self.known.get(&origin) {
			self.count(known.items.len());
			self.dirty.extend(known.items.keys().cloned());
		} else if moved && let Some(ranked) = self.ranked.get(&origin) {
			self.count(ranked.len());
			self.dirty.extend(ranked.iter().cloned());
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
		// Each message moves the sender's copy to a newer revision, once per claim.
		if floor <= heard.floor || !well_built(&changes) {
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
				let ended: BTreeSet<&Broadcast<I>> = (changes.iter())
					.filter(|(_, change)| *change == Change::Ended)
					.map(|(broadcast, _)| broadcast)
					.collect();
				diff.extend(
					(known.items.keys())
						.filter(|broadcast| !heard.items.contains_key(*broadcast))
						.map(|broadcast| match ended.contains(broadcast) {
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
		let mut lost = Vec::new();
		for (broadcast, change) in &changes {
			match change {
				Change::Live(claim) => {
					if known.items.insert(broadcast.clone(), claim.clone()).is_none() {
						claimed.push(broadcast.clone());
					}
					self.ended.remove(broadcast);
				}
				Change::Ended | Change::Lost => {
					if let Some(claim) = known.items.remove(broadcast) {
						unclaimed.push(broadcast.clone());
						// `restate` keeps its own claim's source.
						if *change == Change::Lost && origin != self.me {
							lost.push((broadcast.clone(), claim));
						}
					}
					if *change == Change::Ended {
						self.ended.insert(broadcast.clone());
					}
				}
			}
			self.dirty.insert(broadcast.clone());
		}
		for (broadcast, claim) in lost {
			self.drop_claim(broadcast, origin, claim, None);
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

	fn claim(&mut self, broadcast: &Broadcast<I>, origin: NodeId) {
		let (path, instance) = broadcast;
		let origins = (self.claimants.entry(path.clone()).or_default())
			.entry(instance.clone())
			.or_default();
		if !origins.insert(origin) {
			return;
		}
		let shared = origins.len() >= 2;
		// The claimant it now shares the broadcast with ranks it too.
		let other = (origins.len() == 2).then(|| *origins.iter().find(|other| **other != origin).expect("another"));
		if shared || self.watched(path) {
			self.rank(origin, broadcast, true);
		}
		if let Some(other) = other {
			self.rank(other, broadcast, true);
		}
	}

	/// `claimant` lost its claim of `broadcast`, which `source` gave this node if its own.
	fn drop_claim(&mut self, broadcast: Broadcast<I>, claimant: NodeId, claim: Claim<A>, source: Option<N>) {
		let claim = Claim {
			unavailable: true,
			..claim
		};
		let gone = Gone {
			claimant,
			claim,
			source,
		};
		self.dropped.entry(broadcast).or_default().push(gone);
	}

	fn unclaim(&mut self, broadcast: &Broadcast<I>, origin: NodeId) {
		let (path, instance) = broadcast;
		let Some(claims) = self.claimants.get_mut(path) else {
			return;
		};
		let Some(origins) = claims.get_mut(instance) else {
			return;
		};
		if !origins.remove(&origin) {
			return;
		}
		let left = (origins.len() == 1).then(|| *origins.first().expect("one"));
		if origins.is_empty() {
			claims.remove(instance);
			if claims.is_empty() {
				self.claimants.remove(path);
			}
		}
		self.rank(origin, broadcast, false);
		// The one claimant left ranks it only if it is shown.
		if let Some(left) = left {
			let watched = self.watched(path);
			self.rank(left, broadcast, watched);
		}
	}

	/// File `broadcast` among the ones `origin`'s rank reaches, or not.
	fn rank(&mut self, origin: NodeId, broadcast: &Broadcast<I>, ranked: bool) {
		self.visit();
		match ranked {
			true => {
				self.ranked.entry(origin).or_default().insert(broadcast.clone());
			}
			false => {
				if let Some(set) = self.ranked.get_mut(&origin) {
					set.remove(broadcast);
					if set.is_empty() {
						self.ranked.remove(&origin);
					}
				}
			}
		}
	}

	/// Whether an endpoint or scoped neighbour is shown the claims at `path`: one asks at
	/// or above it, or below it, where the most specific claim above is presented.
	fn watched(&self, path: &Path) -> bool {
		ancestors(path).any(|above| self.askers.contains_key(&above)) || below(&self.askers, path).next().is_some()
	}

	/// `prefix` gained its first asker or lost its last: file again every broadcast whose
	/// claims it is shown, at, below, or above it.
	fn rewatch(&mut self, prefix: &Path) {
		let near: Vec<Broadcast<I>> = (ancestors(prefix).filter_map(|above| self.claimants.get_key_value(&above)))
			.chain(below(&self.claimants, prefix))
			.flat_map(|(path, claims)| claims.keys().map(move |instance| (path.clone(), instance.clone())))
			.collect();
		for broadcast in near {
			let (path, instance) = &broadcast;
			let origins: Vec<NodeId> = self.claimants[path][instance].iter().copied().collect();
			let ranked = origins.len() >= 2 || self.watched(path);
			for origin in origins {
				self.rank(origin, &broadcast, ranked);
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
		if !well_built(&changes) {
			return Err(Error::Malformed);
		}
		let (cost, priority) = (peer.port.cost, peer.port.priority);
		let scoped = matches!(peer.port.role, Role::Scoped(_));
		for (broadcast, change) in changes {
			self.visit();
			let change = match change {
				// A claim that already crossed this relay came round a ring of grants, and
				// one whose chain this relay would carry past the longest path is never
				// taken, as a route past the metric is never used.
				Change::Live(claim)
					if claim.chain.contains(&self.me.relay) || (scoped && claim.chain.len() + 1 >= MAX_HOPS) =>
				{
					Change::Lost
				}
				// Unavailable is only ever news of a run this neighbour gave before: one first
				// seen unavailable, which nothing can serve, is not there until it is
				// available. A neighbour that moved to it no longer gives the run it gave.
				Change::Live(claim)
					if claim.unavailable
						&& !(self.peers[&from].given.get(&broadcast))
							.is_some_and(|had| precedence(&had.item) == precedence(&claim.item)) =>
				{
					match self.peers[&from].given.contains_key(&broadcast) {
						true => Change::Lost,
						false => continue,
					}
				}
				Change::Live(mut claim) => {
					// Saturating, as `model::origin`'s claim cost does, so a claim's cost
					// never decides whether it can serve.
					claim.cost = claim.cost.saturating_add(cost);
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
			// Precedence first and then rank, as `closest`, an unavailable claim last.
			.min_by_key(|(claim, peer)| {
				(
					precedence(&claim.item),
					claim.unavailable,
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
				let source = self.sources.remove(&broadcast);
				let Some(was) = was.cloned() else {
					return;
				};
				if cause == Change::Lost {
					self.drop_claim(broadcast.clone(), me, was, source);
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

	/// The forwarding node a request for `broadcast` names from here: the closest claim
	/// at the most specific path covering it that the reader shows, reachable or not, as a
	/// neighbour asking for its path is shown. `None` fails it closed while that claim is
	/// unavailable.
	pub fn pick(&self, (path, instance): &Broadcast<I>) -> Option<NodeId> {
		let from = (ancestors(path).rev()).find(|above| self.reader.get(instance.clone(), above).is_some())?;
		self.closest(&(from, instance.clone())).map(|(origin, _)| origin)
	}

	/// Where a request for `broadcast` naming `origin` goes from here. `None` fails it.
	pub fn step(&self, origin: NodeId, (path, instance): &Broadcast<I>) -> Option<Next<N>> {
		match origin == self.me {
			// As `pick`, the claim at the most specific path covering it that the reader
			// shows: the neighbour behind it if it is this node's own, available, and of the
			// best precedence there, and closed while it is unavailable, lost, outranked,
			// or another node's.
			true => {
				let from = (ancestors(path).rev()).find(|above| self.reader.get(instance.clone(), above).is_some())?;
				let claim = (from, instance.clone());
				let source = *self.sources.get(&claim)?;
				let own = self.known.get(&self.me)?.items.get(&claim)?;
				// Only a claim of the best precedence there may serve, reachable or not, as
				// in `closest`; among those, this node's own serves whatever its rank.
				let best = (self.claims_of(&claim)).map(|(_, claim)| precedence(&claim.item)).min();
				(!own.unavailable && best == Some(precedence(&own.item))).then_some(Next::Local(source))
			}
			false => self.best.get(&origin).map(|best| Next::Via(best.via)),
		}
	}

	/// Every claim of `broadcast` this node knows, with its claimant, reachable or not.
	fn claims_of<'a>(
		&'a self,
		broadcast: &Broadcast<I>,
	) -> impl Iterator<Item = (NodeId, &'a Claim<A>)> + use<'a, N, I, A> {
		let (path, instance) = broadcast;
		let origins = self.claimants.get(path).and_then(|claims| claims.get(instance));
		let broadcast = broadcast.clone();
		(origins.into_iter().flatten()).filter_map(move |&origin| {
			self.visit();
			let claim = self.known.get(&origin)?.items.get(&broadcast)?;
			Some((origin, claim))
		})
	}

	/// Every claim of `broadcast` a request may follow from here, as [`Self::closest`]
	/// chooses among them: of the best precedence, available, and reached by a route.
	pub fn ways(&self, broadcast: &Broadcast<I>) -> Vec<Way<N, A>> {
		let closest = self.closest(broadcast).map(|(origin, _)| origin);
		let top = (self.claims_of(broadcast)).map(|(_, claim)| precedence(&claim.item)).min();
		let mut ways = Vec::new();
		for (origin, claim) in self.claims_of(broadcast) {
			if Some(precedence(&claim.item)) != top || claim.unavailable {
				continue;
			}
			// Every neighbour that gave this node its own claim serves it, so a reader
			// already holds another copy when the one it follows closes.
			if origin == self.me
				&& let Some(givers) = self.givers.get(broadcast)
			{
				let source = self.sources.get(broadcast);
				for (peer, given) in givers {
					self.visit();
					if precedence(&given.item) != precedence(&claim.item) || given.unavailable {
						continue;
					}
					let mut given = given.clone();
					given.draining |= self.peers.get(peer).is_some_and(|peer| peer.draining);
					ways.push(Way {
						origin,
						next: Next::Local(*peer),
						claim: given,
						closest: closest == Some(origin) && source == Some(peer),
					});
				}
				continue;
			}
			let way = match origin == self.me {
				true => self.sources.get(broadcast).map(|source| (Next::Local(*source), claim.clone())),
				false => self.best.get(&origin).map(|best| {
					let claim = Claim {
						cost: claim.cost.saturating_add(best.offer.cost),
						priority: claim.priority.min(best.offer.priority),
						draining: claim.draining || best.offer.draining,
						..claim.clone()
					};
					(Next::Via(best.via), claim)
				}),
			};
			if let Some((next, claim)) = way {
				ways.push(Way {
					origin,
					next,
					claim,
					closest: closest == Some(origin),
				});
			}
		}
		ways
	}

	/// Whether `origin`'s claim of `broadcast` reached this node over a marked link: its
	/// route crossed one, or a marked neighbour gave this node its own.
	fn crossed_mark(&self, origin: NodeId, broadcast: &Broadcast<I>) -> bool {
		match origin == self.me {
			true => (self.sources.get(broadcast))
				.and_then(|source| self.peers.get(source))
				.is_some_and(|source| source.port.upstream),
			false => self.best.get(&origin).is_some_and(|best| best.offer.marked),
		}
	}

	/// The claim of `broadcast` a request takes from here, with its claimant: among the
	/// claims of the best precedence, reachable or not, the best rank of the route and the
	/// claim together that a route reaches.
	fn closest(&self, broadcast: &Broadcast<I>) -> Option<(NodeId, &Claim<A>)> {
		let mut top = None;
		let mut pick: Option<(ClaimOrder, &Claim<A>)> = None;
		for (origin, claim) in self.claims_of(broadcast) {
			let precedence = precedence(&claim.item);
			if top.is_some_and(|top| precedence > top) {
				continue;
			}
			// A better precedence outranks whatever was picked, reachable or not.
			if top != Some(precedence) {
				top = Some(precedence);
				pick = None;
			}
			// Its announcer has no way to the publisher, whatever route reaches it.
			if claim.unavailable {
				continue;
			}
			let rank = match origin == self.me {
				true => claim.rank(),
				false => {
					let Some(best) = self.best.get(&origin) else {
						continue;
					};
					let (draining, Reverse(priority), cost) = best.offer.rank();
					(
						draining || claim.draining,
						Reverse(priority.min(claim.priority)),
						cost.saturating_add(claim.cost),
					)
				}
			};
			let key = (rank, claim.chain.len(), origin);
			if pick.as_ref().is_none_or(|(best, _)| key < *best) {
				pick = Some((key, claim));
			}
		}
		pick.map(|((_, _, origin), claim)| (origin, claim))
	}

	// --- Readers, endpoints, and scoped neighbours ---

	/// Bring what the reader sees, and what endpoints and scoped neighbours are told, up to
	/// date for every broadcast that may have changed.
	fn refresh(&mut self) {
		let dirty = std::mem::take(&mut self.dirty);
		if let Some(log) = &mut self.log {
			log.broadcasts.extend(dirty.iter().cloned());
		}
		let mut now: BTreeMap<I, BTreeMap<Path, Option<Shown<A>>>> = BTreeMap::new();
		for broadcast in &dirty {
			self.visit();
			let (path, instance) = broadcast;
			let closest = self.closest(broadcast).map(|(_, claim)| claim.item.clone());
			let claimed = (self.claimants.get(path)).is_some_and(|claims| claims.contains_key(instance));
			let shown = match claimed {
				true => {
					self.unwait(broadcast);
					let available = closest.is_some();
					if available {
						self.since.remove(broadcast);
					} else {
						// Unavailable since the reader first showed it so, lost or not.
						self.since.entry(broadcast.clone()).or_insert(self.now);
					}
					// Unreachable, it still shows what the claim of the best precedence says.
					let item = closest.or_else(|| {
						(self.claims_of(broadcast))
							.min_by_key(|(origin, claim)| (precedence(&claim.item), *origin))
							.map(|(_, claim)| claim.item.clone())
					});
					Some(item.map(|item| Shown {
						item,
						available,
						presented: false,
					}))
				}
				false if self.ended.contains(broadcast) || mutated!(self, LostEnds) => {
					self.unwait(broadcast);
					self.since.remove(broadcast);
					Some(None)
				}
				// Lost: shown unavailable for the debounce, in case a claimant returns. The
				// debounce runs from when the reader first showed it unavailable, so a claim
				// a scoped neighbour told unavailable and then lost goes offline about when
				// that neighbour's reader did.
				false => match self.reader.get(instance.clone(), path) {
					Some(_) if self.waiting.contains_key(broadcast) => None,
					Some(was) => {
						let since = self.since.get(broadcast).copied().unwrap_or(self.now);
						let at = since.saturating_add(self.debounce);
						if at <= self.now {
							self.since.remove(broadcast);
							Some(None)
						} else {
							self.since.insert(broadcast.clone(), since);
							let item = was.item.clone();
							// The claim it showed: of those just lost, one with its item, by
							// precedence and claimant as an unreachable one is shown.
							let gone = (self.dropped.get(broadcast).into_iter().flatten())
								.min_by_key(|gone| {
									(gone.claim.item != item, precedence(&gone.claim.item), gone.claimant)
								})
								.cloned();
							if let Some(gone) = gone {
								(self.gone.entry(path.clone()).or_default()).insert(instance.clone(), gone);
							}
							self.waiting.insert(broadcast.clone(), at);
							self.deadlines.insert((at, broadcast.clone()));
							Some(Some(Shown {
								item,
								available: false,
								presented: false,
							}))
						}
					}
					None => {
						self.since.remove(broadcast);
						None
					}
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
		// A request finds its way by `pick` and `step`, never by which claims the reader
		// recomputed, so nothing keeps that list.
		reader.changed();
		for broadcast in &dirty {
			self.exports(broadcast);
		}
		for broadcast in dirty {
			self.ended.remove(&broadcast);
		}
		self.dropped.clear();
	}

	fn unwait(&mut self, broadcast: &Broadcast<I>) {
		if let Some(at) = self.waiting.remove(broadcast) {
			self.deadlines.remove(&(at, broadcast.clone()));
		}
		self.ungone(broadcast);
	}

	fn ungone(&mut self, (path, instance): &Broadcast<I>) {
		if let Some(gone) = self.gone.get_mut(path) {
			gone.remove(instance);
			if gone.is_empty() {
				self.gone.remove(path);
			}
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
		let now = self.shown_to(peer, path, instance);
		let state = self.peers.get_mut(&peer).expect("a neighbour");
		let told = state.told.get(path).and_then(|told| told.get(instance));
		if told.map(|told| &told.claim) == now.as_ref().map(|now| &now.claim) {
			// The same claim, perhaps standing for another: nothing to say.
			if let Some(now) = now {
				let told = state.told.get_mut(path).and_then(|told| told.get_mut(instance));
				*told.expect("told") = now;
			}
			return;
		}
		let change = match now {
			Some(now) => {
				let claim = now.claim.clone();
				state
					.told
					.entry(path.clone())
					.or_default()
					.insert(instance.clone(), now);
				Change::Live(claim)
			}
			None => {
				let told = state.told.get_mut(path).expect("told");
				let gone = told.remove(instance).expect("told");
				if told.is_empty() {
					state.told.remove(path);
				}
				// It ended only if the publisher of the claim it stood for ended it.
				match self.ended.contains(&(gone.from, instance.clone())) {
					true => Change::Ended,
					false => Change::Lost,
				}
			}
		};
		let changes = vec![((path.clone(), instance.clone()), change)];
		self.out.push((peer, Msg::Announce { changes }));
	}

	/// What `peer` is told of `instance` at `path`: what this node's reader shows of the
	/// claim there, or at a prefix it asks for, of the most specific claim above that the
	/// reader shows, lowered to it.
	fn shown_to(&self, peer: N, path: &Path, instance: &I) -> Option<Told<A>> {
		let asks = &self.peers[&peer].asks;
		if !within_any(path, asks) {
			return None;
		}
		if let Some(told) = self.stands(peer, path, path, instance) {
			return Some(told);
		}
		if !asks.contains(path) {
			return None;
		}
		(ancestors(path).rev().skip(1)).find_map(|above| self.stands(peer, path, &above, instance))
	}

	/// What `peer`, told at `path`, is shown of the claim of `instance` at `from`, if this
	/// node's reader shows one there and it did not come from `peer`. It is the closest
	/// claim while one is reachable; else the claim of the best precedence, unavailable;
	/// else, while the reader waits to show it offline, the claim it showed, unavailable.
	/// Never a withdrawal while the reader still shows the claim. A claim that reached this
	/// node over a marked link is never told over another, as a route is never sent.
	fn stands(&self, peer: N, path: &Path, from: &Path, instance: &I) -> Option<Told<A>> {
		self.reader.get(instance.clone(), from)?;
		let broadcast = (from.clone(), instance.clone());
		let marked = self.peers[&peer].port.upstream;
		let source = |origin: NodeId| {
			(origin == self.me && self.sources.get(&broadcast) == Some(&peer))
				|| (marked && self.crossed_mark(origin, &broadcast))
		};
		let claim = match self.closest(&broadcast) {
			Some((origin, _)) if source(origin) => return None,
			Some((origin, claim)) if origin == self.me => claim.clone(),
			Some((origin, claim)) => {
				let route = &self.best[&origin].offer;
				Claim {
					item: claim.item.clone(),
					cost: claim.cost.saturating_add(route.cost),
					priority: claim.priority.min(route.priority),
					draining: claim.draining || route.draining,
					unavailable: false,
					chain: claim.chain.clone(),
				}
			}
			None => match self
				.claims_of(&broadcast)
				.min_by_key(|(origin, claim)| (precedence(&claim.item), *origin))
			{
				Some((origin, _)) if source(origin) => return None,
				Some((_, claim)) => Claim {
					unavailable: true,
					..claim.clone()
				},
				// Lost, and waiting out the debounce: what it showed, to every asker alike.
				None => {
					let gone = self.gone.get(from)?.get(instance)?;
					let crossed = (gone.source.as_ref())
						.and_then(|source| self.peers.get(source))
						.is_some_and(|source| marked && source.port.upstream);
					if gone.source == Some(peer) || crossed {
						return None;
					}
					gone.claim.clone()
				}
			},
		};
		let by = (path.parts().count() - from.parts().count()) as u64;
		Some(Told {
			claim: claim.lowered(by),
			from: from.clone(),
		})
	}

	fn unask(&mut self, prefix: &Path, peer: N) {
		if let Some(peers) = self.askers.get_mut(prefix) {
			peers.remove(&peer);
			if peers.is_empty() {
				self.askers.remove(prefix);
				self.rewatch(prefix);
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

/// Whether the rule could have built `changes`: no chain past the longest path, and no
/// claim changed twice.
fn well_built<I: Ord, A>(changes: &[(Broadcast<I>, Change<A>)]) -> bool {
	let mut seen = BTreeSet::new();
	changes.iter().all(|(broadcast, change)| {
		let built = match change {
			Change::Live(claim) => claim.chain.len() < MAX_HOPS,
			Change::Ended | Change::Lost => true,
		};
		built && seen.insert(broadcast)
	})
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

	/// The forwarding nodes this node routes to.
	pub fn routes(&self) -> Vec<NodeId> {
		self.best.keys().copied().collect()
	}

	/// Whether it offers a route to itself: it has announced a claim.
	pub fn announced(&self) -> bool {
		self.announced
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
			ranked: _,
			askers: _,
			ended,
			dirty,
			waiting,
			since,
			gone,
			dropped: _,
			deadlines: _,
			reader,
			out,
			log: _,
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
		// How long a claim has shown unavailable matters only up to the debounce.
		let since: BTreeMap<&Broadcast<I>, Duration> = (since.iter())
			.map(|(broadcast, at)| (broadcast, now.saturating_duration_since(*at).min(*debounce)))
			.collect();
		let state = (
			(me, debounce, sessions, peers, routing, fresh, heard, unnamed),
			(dead, closed, draining, stale, best, known, rev, announced),
			(sources, ended, dirty, waiting, since, gone, reader.shown(), out),
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
		let mut ranked: BTreeMap<NodeId, BTreeSet<Broadcast<I>>> = BTreeMap::new();
		for (path, claims) in &claimants {
			for (instance, origins) in claims {
				if origins.len() >= 2 || self.watched(path) {
					for origin in origins {
						ranked
							.entry(*origin)
							.or_default()
							.insert((path.clone(), instance.clone()));
					}
				}
			}
		}
		assert_eq!(self.claimants, claimants, "claimants");
		assert_eq!(self.ranked, ranked, "ranked claims");
		let deadlines: BTreeSet<(Instant, Broadcast<I>)> = (self.waiting.iter())
			.map(|(broadcast, at)| (*at, broadcast.clone()))
			.collect();
		assert_eq!(self.deadlines, deadlines, "deadlines");
		for (path, gone) in &self.gone {
			assert!(!gone.is_empty(), "gone");
			for instance in gone.keys() {
				assert!(self.waiting.contains_key(&(path.clone(), instance.clone())), "gone");
			}
		}
		assert!(self.dropped.is_empty(), "dropped");
		for (path, instance) in self.since.keys() {
			let shown = self.reader.get(instance.clone(), path);
			assert!(shown.is_some_and(|shown| !shown.available), "unavailable since");
		}
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
	fn open<A: Item>(router: &mut Router<usize, u64, A>, peer: usize, role: Role) {
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
			unavailable: false,
			chain: Vec::new(),
		})
	}

	/// A route from routing neighbour `via`, as `node(via)`, to forwarding node `origin`.
	fn route(origin: Id, via: Id, cost: Metric) -> Msg<u64, Run> {
		let path = vec![
			Hop {
				node: node(origin),
				session: 0,
			},
			Hop {
				node: node(via),
				session: 1,
			},
		];
		Msg::Route {
			origin: node(origin),
			offer: Some(Offer {
				path: path.into(),
				draining: false,
				priority: i8::MAX,
				cost,
				marked: false,
			}),
		}
	}

	/// Attributes with an epoch and a distance, as a restarted publisher's claims, or a
	/// broader claim presented at a narrower path, carry.
	#[derive(Clone, Copy, Debug, PartialEq, Eq)]
	struct Run {
		epoch: u64,
		above: u64,
	}

	impl Item for Run {
		fn lowered(&self, by: u64) -> Self {
			Self {
				above: self.above + by,
				..*self
			}
		}

		fn above(&self) -> u64 {
			self.above
		}

		fn epoch(&self) -> u64 {
			self.epoch
		}
	}

	/// A claim exactly at its path, at `epoch`.
	fn run(epoch: u64) -> Run {
		Run { epoch, above: 0 }
	}

	fn claim(item: Run, cost: Metric) -> Change<Run> {
		Change::Live(Claim {
			item,
			cost,
			priority: i8::MAX,
			draining: false,
			unavailable: false,
			chain: Vec::new(),
		})
	}

	/// A request for a path under a claim follows the most specific claim covering it, as
	/// a neighbour asking for that path is shown.
	#[test]
	fn a_request_follows_the_claim_covering_it() {
		let mut router = relay(1);
		open(&mut router, 2, Role::Endpoint);
		open(&mut router, 3, Role::Endpoint);
		let announce = |path: &str| Msg::Announce {
			changes: vec![((p(path), 7), live(0))],
		};
		router.recv(2, announce("live")).unwrap();
		router.recv(3, announce("live/a")).unwrap();
		let me = router.me();
		for (path, endpoint) in [("live", 2), ("live/b/x", 2), ("live/a", 3), ("live/a/x", 3)] {
			let broadcast = (p(path), 7);
			assert_eq!(router.pick(&broadcast), Some(me), "{path}");
			assert_eq!(router.step(me, &broadcast), Some(Next::Local(endpoint)), "{path}");
		}
		for broadcast in [(p("other"), 7), (p("live/a"), 8)] {
			assert_eq!(router.pick(&broadcast), None, "{broadcast:?}");
			assert_eq!(router.step(me, &broadcast), None, "{broadcast:?}");
		}
		// The narrower claim lost, a request under it fails closed, even one already
		// naming this node, until the reader lets it go.
		router
			.recv(
				3,
				Msg::Announce {
					changes: vec![((p("live/a"), 7), Change::Lost)],
				},
			)
			.unwrap();
		let under = (p("live/a/x"), 7);
		assert_eq!(router.pick(&under), None);
		assert_eq!(router.step(me, &under), None);
		router.timeout(router.deadline().expect("a debounce"));
		assert_eq!(router.pick(&under), Some(me));
		assert_eq!(router.step(me, &under), Some(Next::Local(2)));
	}

	/// Among claims of one Instance at one path, one exactly there outranks a broader one
	/// shown there, and among those a newer epoch outranks an older one however much
	/// cheaper the older is, or whether a route reaches the newer: among a node's
	/// endpoints and among forwarding nodes alike.
	#[test]
	fn the_newest_run_of_the_closest_claim_serves() {
		let mut router = Router::<usize, u64, Run>::new(node(1), debounce(HOP));
		open(&mut router, 2, Role::Endpoint);
		open(&mut router, 3, Role::Endpoint);
		let broadcast = (p("live"), 7);
		let announce = |change| Msg::Announce {
			changes: vec![(broadcast.clone(), change)],
		};
		router.recv(2, announce(claim(run(1), 0))).unwrap();
		router.recv(3, announce(claim(run(2), 9))).unwrap();
		let me = router.me();
		assert_eq!(
			router.step(me, &broadcast),
			Some(Next::Local(3)),
			"a newer epoch, however dear"
		);
		assert_eq!(router.own().1[&broadcast].item, run(2));
		router.recv(2, announce(claim(Run { epoch: 3, above: 1 }, 0))).unwrap();
		assert_eq!(
			router.step(me, &broadcast),
			Some(Next::Local(3)),
			"a broader claim, however new"
		);
		open(&mut router, 4, Role::Routing(node(4)));
		router.recv(4, route(5, 4, 1)).unwrap();
		let claims = |item, floor| Msg::Claims {
			origin: node(5),
			changes: vec![(broadcast.clone(), claim(item, 0))],
			floor,
		};
		router.recv(4, claims(run(1), 1)).unwrap();
		assert_eq!(router.pick(&broadcast), Some(me), "an older claim, however close");
		router.recv(4, claims(run(2), 2)).unwrap();
		assert_eq!(router.pick(&broadcast), Some(node(5)), "the same run, closer");
		assert_eq!(
			router.step(me, &broadcast),
			Some(Next::Local(3)),
			"named, its own run serves"
		);
		router.recv(4, claims(Run { epoch: 4, above: 1 }, 3)).unwrap();
		assert_eq!(router.pick(&broadcast), Some(me), "a broader claim, however new");
		router.recv(4, claims(run(3), 4)).unwrap();
		assert_eq!(router.pick(&broadcast), Some(node(5)), "a newer claim");
		assert_eq!(router.step(me, &broadcast), None, "named, its older run never serves");
		// The newer claim's route goes while its claim stays known: the older run never
		// stands in for it.
		router.recv(4, route(5, 4, Metric::MAX)).unwrap();
		assert_eq!(router.best(node(5)), None);
		assert_eq!(router.pick(&broadcast), None);
		assert_eq!(router.step(me, &broadcast), None, "named, still closed");
		let shown = &router.shown()[&(7, p("live"))];
		assert_eq!((shown.item, shown.available), (run(3), false));
	}

	/// A route whose cost a link would carry past the metric is never used, and never wraps
	/// round to a cheap one. A claim's cost saturates there instead, so it never decides
	/// whether the claim can serve.
	#[test]
	fn a_route_past_the_metric_is_never_used() {
		let mut router = Router::<usize, u64, Run>::new(node(1), debounce(HOP));
		open(&mut router, 2, Role::Routing(node(2)));
		router.recv(2, route(5, 2, Metric::MAX)).unwrap();
		assert_eq!(router.best(node(5)), None);
		router.recv(2, route(5, 2, 1)).unwrap();
		assert!(router.best(node(5)).is_some());
		let far = (p("far"), 7);
		let claims = Msg::Claims {
			origin: node(5),
			changes: vec![(far.clone(), claim(run(0), Metric::MAX - 1))],
			floor: 1,
		};
		router.recv(2, claims).unwrap();
		assert!(router.shown()[&(7, p("far"))].available);
		assert_eq!(router.pick(&far), Some(node(5)));
		open(&mut router, 3, Role::Endpoint);
		router.drain();
		let edit = Edit {
			remove: Vec::new(),
			add: vec![p(ROOT)],
		};
		router.edit(3, edit).unwrap();
		let told = router.drain().into_iter().any(|(to, msg)| {
			to == 3
				&& matches!(&msg, Msg::Announce { changes } if changes.iter().any(|(broadcast, change)| {
					*broadcast == far && matches!(change, Change::Live(claim) if claim.cost == Metric::MAX)
				}))
		});
		assert!(told, "told at the metric's ceiling");
		let announce = Msg::Announce {
			changes: vec![((p("near"), 7), claim(run(0), Metric::MAX))],
		};
		router.recv(3, announce).unwrap();
		assert_eq!(router.own().1[&(p("near"), 7)].cost, Metric::MAX, "an endpoint's claim");
	}

	/// A viewer is withdrawn with the cause of the claim it was shown, never with a broader
	/// claim's: one shown the exact claim at its prefix hears it lost even as a broader
	/// claim above ends in the same batch, and one shown only the broader claim hears it
	/// ended. With a debounce, the lost claim is first shown unavailable, and lost when the
	/// relay's reader goes offline.
	#[test]
	fn a_withdrawal_carries_the_cause_of_the_claim_shown() {
		let viewer = |router: &mut Relay, peer: usize, prefix: &str| {
			open(router, peer, Role::Endpoint);
			let edit = Edit {
				remove: Vec::new(),
				add: vec![p(prefix)],
			};
			router.edit(peer, edit).unwrap();
		};
		let told = |out: &[(usize, Msg<u64, u64>)], peer: usize| -> Vec<(Path, Change<u64>)> {
			(out.iter())
				.filter(|(to, _)| *to == peer)
				.filter_map(|(_, msg)| match msg {
					Msg::Announce { changes } => Some(changes.clone()),
					_ => None,
				})
				.flatten()
				.map(|((path, _), change)| (path, change))
				.collect()
		};
		let batch = Msg::Announce {
			changes: vec![((p("a"), 7), Change::Ended), ((p("a/b"), 7), Change::Lost)],
		};
		for debounce in [Duration::ZERO, super::debounce(HOP)] {
			let mut router: Relay = Router::new(node(1), debounce);
			open(&mut router, 2, Role::Endpoint);
			let announce = Msg::Announce {
				changes: vec![((p("a"), 7), live(0)), ((p("a/b"), 7), live(0))],
			};
			router.recv(2, announce).unwrap();
			viewer(&mut router, 3, "a/b");
			viewer(&mut router, 4, "a/c");
			router.drain();
			router.recv(2, batch.clone()).unwrap();
			let out = router.drain();
			let (exact, broader) = (told(&out, 3), told(&out, 4));
			assert_eq!(broader, [(p("a/c"), Change::Ended)], "{debounce:?}");
			let lost = (p("a/b"), Change::Lost);
			if debounce.is_zero() {
				assert_eq!(exact, [lost], "{debounce:?}");
				continue;
			}
			assert!(
				matches!(exact.as_slice(), [(path, Change::Live(claim))] if *path == p("a/b") && claim.unavailable),
				"{exact:?}"
			);
			router.timeout(router.deadline().expect("a debounce"));
			assert_eq!(told(&router.drain(), 3), [lost]);
		}
	}

	/// A neighbour that asks while a lost claim waits out the debounce is told what one
	/// that asked before is: the lost claim, unavailable, not the broader claim above it,
	/// which the reader does not show there yet, nor nothing when no claim is left. Both
	/// are shown the broader one, or the loss, once the lost claim goes offline.
	#[test]
	fn an_ask_during_the_debounce_is_told_what_an_earlier_one_is() {
		let ask = |router: &mut Relay, peer: usize| {
			open(router, peer, Role::Endpoint);
			let edit = Edit {
				remove: Vec::new(),
				add: vec![p("a/b")],
			};
			router.edit(peer, edit).unwrap();
		};
		let told = |out: &[(usize, Msg<u64, u64>)], peer: usize| -> Vec<Change<u64>> {
			(out.iter())
				.filter(|(to, _)| *to == peer)
				.filter_map(|(_, msg)| match msg {
					Msg::Announce { changes } => Some(changes.clone()),
					_ => None,
				})
				.flatten()
				.inspect(|((path, _), _)| assert_eq!(*path, p("a/b")))
				.map(|(_, change)| change)
				.collect()
		};
		let mut router = relay(1);
		open(&mut router, 2, Role::Endpoint);
		let announce = Msg::Announce {
			changes: vec![((p("a"), 7), live(0)), ((p("a/b"), 7), live(0))],
		};
		router.recv(2, announce).unwrap();
		ask(&mut router, 3);
		router.drain();
		let lost = Msg::Announce {
			changes: vec![((p("a/b"), 7), Change::Lost)],
		};
		router.recv(2, lost).unwrap();
		ask(&mut router, 4);
		let out = router.drain();
		let (before, after) = (told(&out, 3), told(&out, 4));
		assert!(
			matches!(before.as_slice(), [Change::Live(claim)] if claim.unavailable),
			"{before:?}"
		);
		assert_eq!(after, before, "the same unavailable claim");
		router.timeout(router.deadline().expect("a debounce"));
		let out = router.drain();
		let (before, after) = (told(&out, 3), told(&out, 4));
		assert!(
			matches!(before.as_slice(), [Change::Live(claim)] if !claim.unavailable),
			"{before:?}"
		);
		assert_eq!(after, before, "the broader claim");

		// With nothing above it, a neighbour asking a prefix over the lost claim is told it
		// too, and lost with the earlier asker when it goes.
		let mut router = relay(1);
		open(&mut router, 2, Role::Endpoint);
		router
			.recv(
				2,
				Msg::Announce {
					changes: vec![((p("a/b"), 7), live(0))],
				},
			)
			.unwrap();
		ask(&mut router, 3);
		router
			.recv(
				2,
				Msg::Announce {
					changes: vec![((p("a/b"), 7), Change::Lost)],
				},
			)
			.unwrap();
		router.drain();
		open(&mut router, 4, Role::Endpoint);
		let edit = Edit {
			remove: Vec::new(),
			add: vec![p("a")],
		};
		router.edit(4, edit).unwrap();
		let after = told(&router.drain(), 4);
		assert!(
			matches!(after.as_slice(), [Change::Live(claim)] if claim.unavailable),
			"{after:?}"
		);
		router.timeout(router.deadline().expect("a debounce"));
		let out = router.drain();
		assert_eq!(told(&out, 3), [Change::Lost]);
		assert_eq!(told(&out, 4), [Change::Lost]);
	}

	/// A scoped neighbour's claim whose chain this relay would carry to the longest path
	/// is not taken, and the message carrying it stands: this relay never sends a chain a
	/// neighbour refuses.
	#[test]
	fn a_chain_is_never_carried_past_the_longest_path() {
		let mut router = relay(1);
		open(&mut router, 2, Role::Scoped(vec![p(ROOT)]));
		open(&mut router, 3, Role::Routing(node(3)));
		router.drain();
		let chained = |len: usize| {
			Change::Live(Claim {
				chain: (100..100 + len as Id).collect(),
				..match live(0) {
					Change::Live(claim) => claim,
					_ => unreachable!(),
				}
			})
		};
		let announce = Msg::Announce {
			changes: vec![
				((p("short"), 7), chained(MAX_HOPS - 2)),
				((p("long"), 7), chained(MAX_HOPS - 1)),
			],
		};
		router.recv(2, announce).unwrap();
		let own = router.own().1;
		assert_eq!(own[&(p("short"), 7)].chain.len(), MAX_HOPS - 1);
		assert!(!own.contains_key(&(p("long"), 7)));
		for (_, msg) in router.drain() {
			if let Msg::Claims { changes, .. } | Msg::Announce { changes } = &msg {
				assert!(well_built(changes), "{msg:?}");
			}
		}
	}

	/// A message that changes one claim twice is one the rule never sends, so it is
	/// refused whole, from a routing neighbour or an endpoint.
	#[test]
	fn a_claim_changed_twice_in_one_message_is_refused() {
		let mut router = relay(1);
		open(&mut router, 2, Role::Routing(node(2)));
		open(&mut router, 3, Role::Endpoint);
		let offer = Offer {
			path: vec![
				Hop {
					node: node(5),
					session: 0,
				},
				Hop {
					node: node(2),
					session: 1,
				},
			]
			.into(),
			draining: false,
			priority: i8::MAX,
			cost: 1,
			marked: false,
		};
		let route = Msg::Route {
			origin: node(5),
			offer: Some(offer),
		};
		router.recv(2, route).unwrap();
		let broadcast = (p("live"), 7);
		let claims = |changes, floor| Msg::Claims {
			origin: node(5),
			changes,
			floor,
		};
		router.recv(2, claims(vec![(broadcast.clone(), live(0))], 1)).unwrap();
		let twice = vec![(broadcast.clone(), Change::Ended), (broadcast.clone(), live(0))];
		assert_eq!(router.recv(2, claims(twice.clone(), 2)), Err(Error::Malformed));
		assert_eq!(router.pick(&broadcast), Some(node(5)), "nothing of it applied");
		router
			.recv(2, claims(vec![(broadcast.clone(), Change::Ended)], 2))
			.unwrap();
		assert_eq!(router.pick(&broadcast), None);
		let announce = Msg::Announce { changes: twice };
		assert_eq!(router.recv(3, announce), Err(Error::Malformed));
		assert!(router.own().1.is_empty());
	}

	/// The reader keeps nothing of claims that came and went, ended or lost.
	#[test]
	fn a_claim_that_went_leaves_nothing_in_the_reader() {
		let mut router = relay(1);
		open(&mut router, 2, Role::Endpoint);
		let announce = |path: &str, change| Msg::Announce {
			changes: vec![((p(path), 7), change)],
		};
		router.recv(2, announce("ended", live(0))).unwrap();
		router.recv(2, announce("ended", Change::Ended)).unwrap();
		router.recv(2, announce("lost", live(0))).unwrap();
		router.recv(2, announce("lost", Change::Lost)).unwrap();
		router.timeout(router.deadline().expect("a debounce"));
		router.events();
		assert!(router.shown().is_empty());
		assert!(router.reader.changed().is_empty(), "nothing reads which claims changed");
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

	/// A neighbour's answer to a prefix this node asks for is synced once it says so, and
	/// a prefix not asked for has no answer to wait on.
	#[test]
	fn a_prefix_is_synced_once_its_answer_says_so() {
		let mut router = relay(1);
		router
			.connect(2, port(Role::Scoped(vec![p("a"), p("a/b"), p("c")])))
			.unwrap();
		let asked: Vec<Msg<u64, u64>> = router.drain().into_iter().map(|(_, msg)| msg).collect();
		let edit = Edit {
			remove: Vec::new(),
			add: vec![p("a"), p("c")],
		};
		assert_eq!(asked, vec![Msg::Ask(edit)], "the wider of two nested regions");
		assert_eq!(router.is_synced(2, &p("a")), Some(false));
		assert_eq!(
			router.recv(2, Msg::Synced(p("a"))),
			Err(Error::Unasked),
			"before the marker"
		);
		router.recv(2, Msg::Edited).unwrap();
		router.recv(2, Msg::Synced(p("a"))).unwrap();
		assert_eq!(router.is_synced(2, &p("a")), Some(true));
		assert_eq!(router.is_synced(2, &p("c")), Some(false));
		assert_eq!(router.is_synced(2, &p("a/b")), None);
		assert_eq!(router.recv(2, Msg::Synced(p("d"))), Err(Error::Unasked));
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

	/// A claim a scoped neighbour tells unavailable stays, and a request naming this node
	/// fails closed meanwhile. Lost, it waits only what is left of the debounce from when
	/// the reader first showed it unavailable, so a domain past a grant lets it go about
	/// when the neighbour's reader did, not a debounce later.
	#[test]
	fn a_claim_told_unavailable_waits_from_then() {
		use super::super::Kind;
		let mut router = relay(1);
		open(&mut router, 2, Role::Scoped(vec![p(ROOT)]));
		let claim = |change| Msg::Announce {
			changes: vec![((p("live"), 7), change)],
		};
		let unavailable = match live(0) {
			Change::Live(claim) => Change::Live(Claim {
				unavailable: true,
				..claim
			}),
			_ => unreachable!(),
		};
		let kinds = |router: &mut Relay| router.events().into_iter().map(|event| event.kind).collect::<Vec<_>>();
		let request = (p("live/x"), 7);
		router.recv(2, claim(live(0))).unwrap();
		assert_eq!(router.step(node(1), &request), Some(Next::Local(2)));
		let since = Instant::ZERO;
		router.recv(2, claim(unavailable.clone())).unwrap();
		assert_eq!(kinds(&mut router), [Kind::Start, Kind::Unavailable]);
		assert_eq!(router.step(node(1), &request), None, "fails closed");
		assert_eq!(router.pick(&request), None, "fails closed");
		assert_eq!(router.deadline(), None, "a claim kept waits for nothing");
		router.timeout(since.saturating_add(debounce(HOP) / 2));
		router.recv(2, claim(Change::Lost)).unwrap();
		assert_eq!(router.deadline(), Some(since.saturating_add(debounce(HOP))));
		router.timeout(since.saturating_add(debounce(HOP)));
		assert_eq!(kinds(&mut router), [Kind::Offline]);
		// Unavailable for the whole debounce before it is lost, it goes at once.
		router.recv(2, claim(live(0))).unwrap();
		router.recv(2, claim(unavailable)).unwrap();
		let since = since.saturating_add(debounce(HOP));
		router.timeout(since.saturating_add(debounce(HOP)));
		router.recv(2, claim(Change::Lost)).unwrap();
		assert_eq!(kinds(&mut router), [Kind::Start, Kind::Unavailable, Kind::Offline]);
		assert_eq!(router.deadline(), None);
	}

	/// A scoped neighbour's claim first told unavailable is not there: nothing can serve
	/// it, so it is not taken until it is told available. Of the claims neighbours gave at
	/// one precedence, an available one is announced and serves, and with every one
	/// unavailable a request naming this node fails closed. A newer run first told
	/// unavailable leaves an older one standing, as one never told would, whether a new
	/// neighbour tells it or one that gave the older run, which no longer gives that run.
	/// Taken, the newer run outranks the older one, unavailable or not.
	#[test]
	fn a_claim_first_told_unavailable_is_not_there() {
		use super::super::Kind;
		let mut router = Router::<usize, u64, Run>::new(node(1), debounce(HOP));
		for peer in [2, 3, 4] {
			open(&mut router, peer, Role::Scoped(vec![p(ROOT)]));
		}
		let broadcast = (p("live"), 7);
		let told = |epoch, unavailable| {
			let Change::Live(claim) = claim(run(epoch), 0) else {
				unreachable!()
			};
			Msg::Announce {
				changes: vec![(broadcast.clone(), Change::Live(Claim { unavailable, ..claim }))],
			}
		};
		let me = router.me();
		let own = |router: &Router<usize, u64, Run>| {
			(router.own().1.get(&broadcast)).map(|claim| (claim.item.epoch, claim.unavailable))
		};

		router.recv(2, told(1, true)).unwrap();
		assert_eq!(own(&router), None, "first told unavailable");
		assert_eq!(router.pick(&broadcast), None);
		assert!(router.events().is_empty());
		router.recv(2, told(1, false)).unwrap();
		assert_eq!(own(&router), Some((1, false)), "then available");
		assert_eq!(router.step(me, &broadcast), Some(Next::Local(2)));

		// Two neighbours give the run; the one still available serves.
		router.recv(3, told(1, false)).unwrap();
		router.recv(2, told(1, true)).unwrap();
		assert_eq!(own(&router), Some((1, false)));
		assert_eq!(router.step(me, &broadcast), Some(Next::Local(3)));
		router.recv(3, told(1, true)).unwrap();
		assert_eq!(own(&router), Some((1, true)), "every one unavailable");
		assert_eq!(router.step(me, &broadcast), None, "fails closed");
		assert_eq!(router.pick(&broadcast), None, "fails closed");
		router.recv(2, told(1, false)).unwrap();
		assert_eq!(router.step(me, &broadcast), Some(Next::Local(2)));
		assert_eq!(
			(router.events().into_iter().map(|event| event.kind)).collect::<Vec<_>>(),
			[Kind::Start, Kind::Unavailable, Kind::Available]
		);

		// A newer run first told unavailable is not there either, from a neighbour that
		// gave the older run, which it no longer gives, or from a new one.
		router.recv(3, told(1, false)).unwrap();
		router.recv(2, told(2, true)).unwrap();
		assert_eq!(own(&router), Some((1, false)), "the older run stands");
		assert_eq!(
			router.step(me, &broadcast),
			Some(Next::Local(3)),
			"where it is still given"
		);
		router.recv(4, told(2, true)).unwrap();
		assert_eq!(own(&router), Some((1, false)), "the older run stands");
		assert_eq!(router.step(me, &broadcast), Some(Next::Local(3)));
		router.recv(4, told(2, false)).unwrap();
		assert_eq!(own(&router), Some((2, false)), "taken, the newer run outranks");
		assert_eq!(router.step(me, &broadcast), Some(Next::Local(4)));
		router.recv(4, told(2, true)).unwrap();
		assert_eq!(own(&router), Some((2, true)), "unavailable or not");
		assert_eq!(router.step(me, &broadcast), None);
		// The last neighbour giving the older run moves to the newer one unavailable: the
		// older run is lost, and the newer one, which neighbour 4 gave, stays.
		router.recv(3, told(2, true)).unwrap();
		assert_eq!(own(&router), Some((2, true)));
		let lost = Msg::Announce {
			changes: vec![(broadcast.clone(), Change::Lost)],
		};
		router.recv(4, lost).unwrap();
		assert_eq!(own(&router), None, "no neighbour gives either run");
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
