//! The route rule every lite-07 session runs, as one node's state machine.
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
//! on its path, which it may send to more neighbours, and then stays on its next hop. A
//! claim a node takes over a marked link, or one told it marked across a scoped grant, it
//! announces under its marked Node ID, the same relay with the Marked bit set,
//! whose route is marked from the start: the mark then keeps the claim off every other
//! marked link, while each copy of a node's claims stays whole. The marked Node ID holds
//! no session, so it files no session floor, and a path names it only as its first hop,
//! followed by the node itself; that hop counts toward [`MAX_HOPS`] like any other.
//!
//! A closed session is named once per neighbour in a fixed-size [`Msg::Dead`], which ends
//! every route its sender offered through it until the sender offers it again or ends
//! it. A node names its own session only if none it numbered lower to the same relay is
//! live when it closes; one that closes while such a session lives is never named, and
//! its routes go as any route change does, so no name covers a live session. Per pair of relays a node keeps the
//! highest session it knows closed (the session floor), and refuses any path naming that
//! pair at or below it: a Relay ID names one run of a relay, whose sessions it numbers in
//! order, and a restarted relay is a new one. A floor only refuses stale routes sooner, so a node
//! keeps every floor of a pair a heard route joins and, of the rest, those heard routes
//! stopped joining last, while they number no more than those pairs: the table is bounded
//! by the heard routes. A node that sent GOAWAY is named once per neighbour in a
//! [`Msg::Drain`], and every route through it ranks as draining. A node advertises once per burst of input,
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
//! identity. A claim's chain names each relay it entered or left a routing domain by, the
//! announcer, then each grant's exporter and adopter, and a node takes no claim whose
//! chain, with it appended, names a relay twice or grows past the longest path: that claim
//! left a relay's domain and came back. Floors order one claimant's claims, not the copies
//! two grants announced again under two claimants, so a node that hears a claim's publisher
//! ended it holds back every copy of it standing then (the same announcer, first in the
//! chain, from the same prefix, at no newer epoch, at whatever path it is presented)
//! until each copy's own end arrives: no copy shows a moment between the two ends.
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
//! claimant too, and fails closed while that one is unavailable or lost. It leaves toward
//! another node only through a neighbour whose own copy of that node's claims holds the
//! claim: a route can arrive before the claims behind it, and a neighbour refuses a request
//! for a claim it has not heard. While none does, and the node still routes to that node and
//! knows the claim available, the request waits: the node shows the claim available, so it
//! never answers it unroutable. Among the claims
//! of one Instance at one path, only the one shown closest to its own path, at the newest
//! epoch among those, may serve, reachable or not; a route's rank chooses among those
//! alone. `model::origin` and the draft instead rank every filing of one claim as one tier. A route whose cost a link would carry past the
//! metric is never used, and a claim's cost saturates there.
//!
//! The rule is the bake-off's pick (moq-dev/moq#8880): `tests/multi.rs` with `floor`,
//! `batch`, `drain`, and `claimed`, which `tests/multicast.rs` compares it against. It
//! leaves out the pick's `epoch`, which bounded the floors by ordering a relay's runs: a
//! Relay ID names one run, and the heard routes bound the floors.
//! `tests/multicast_search.rs` searches every delivery order on small meshes, and
//! `tests/multicast_cost.rs` sweeps what one event costs.

use std::{
	cmp::Reverse,
	collections::{BTreeMap, BTreeSet, VecDeque},
	fmt,
	sync::Arc,
	time::Duration,
};

use kio::clock::Instant;

use crate::Path;

use super::prefix::{ancestors, below, within_any, within_set};
use super::reader::Reader;
use super::{Error, Event, Id, Item, Key, MAX_COST, MAX_HOPS, Metric, Rev, Shown};

/// How a forwarding node names itself on a path: its Relay ID, which names one run of a
/// relay's process, so a restarted relay is a new node.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct NodeId {
	pub relay: Id,
	/// The node's marked identity, which it announces what crossed a marked link under:
	/// the same process, holding no session of its own.
	pub marked: bool,
}

impl NodeId {
	/// The node relay `relay` names itself as.
	pub fn new(relay: Id) -> Self {
		Self { relay, marked: false }
	}
}

/// One node on a route's path, and the session it learned the route on. The forwarding
/// node a route leads to names itself with session 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct Hop {
	pub node: NodeId,
	pub session: u64,
}

/// A route's rank, lower first: draining, then the lowest priority, then the cost.
pub(crate) type Rank = (bool, Reverse<i8>, Metric);

/// How claims of one precedence order, as [`claim_order`] keys them.
pub(crate) type ClaimOrder = (Rank, bool, usize, NodeId);

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
	pub(crate) fn rank(&self) -> Rank {
		(self.draining, Reverse(self.priority), self.cost)
	}

	/// The route node `me` offers to its own identity `origin`, itself or its marked one,
	/// over a link it marked when `upstream`. The marked identity's route leads through
	/// `me`, as if learned on a marked link, so it is never sent over another.
	pub(crate) fn own(me: NodeId, origin: NodeId, upstream: bool) -> Option<Self> {
		let hop = Hop { node: me, session: 0 };
		let path: Arc<[Hop]> = match origin.marked {
			true => Arc::new([
				Hop {
					node: origin,
					session: 0,
				},
				hop,
			]),
			false => Arc::new([hop]),
		};
		let own = Self {
			path,
			draining: false,
			priority: i8::MAX,
			cost: 0,
			marked: origin.marked,
		};
		(!(own.marked && upstream)).then_some(own)
	}

	/// This route, as node `me` holds it over its session `session`, sent on to routing
	/// neighbour `to` over a link `me` marked when `upstream`: never to a relay already on
	/// its path, or from one marked link to another.
	pub(crate) fn onward(&self, me: NodeId, session: u64, to: NodeId, upstream: bool) -> Option<Self> {
		if names(&self.path, to.relay) || (self.marked && upstream) {
			return None;
		}
		let path: Arc<[Hop]> = self.path.iter().copied().chain([Hop { node: me, session }]).collect();
		Some(Self { path, ..self.clone() })
	}
}

/// Everything a node holds that decides whether it uses a route a neighbour offers, and
/// how it ranks it.
pub(crate) struct Here<'a> {
	/// Per pair of relays, the highest session known closed.
	pub closed: &'a BTreeMap<(Id, Id), u64>,
	/// Closed sessions a live heard route still names.
	pub dead: &'a BTreeMap<Hop, NodeId>,
	/// Nodes named as draining.
	pub draining: &'a BTreeSet<NodeId>,
}

impl Here<'_> {
	/// `offer`, heard on `port` from a neighbour that sent GOAWAY when `draining`, as it
	/// stands here: priced and ranked over the link. `None` for one this node never uses:
	/// at the longest path, through a session known closed, or past the metric.
	pub fn arrive(&self, port: &Port, draining: bool, offer: &Offer) -> Option<Offer> {
		let floored = pairs(&offer.path).any(|(pair, at)| self.closed.get(&pair).is_some_and(|highest| at <= *highest));
		if offer.path.len() >= MAX_HOPS || floored || offer.path.iter().any(|hop| self.dead.contains_key(hop)) {
			return None;
		}
		Some(Offer {
			path: offer.path.clone(),
			draining: offer.draining || draining || offer.path.iter().any(|hop| self.draining.contains(&hop.node)),
			priority: offer.priority.min(port.priority),
			// A route no link could extend is past every other, so it is never used.
			cost: offer.cost.checked_add(port.cost)?,
			marked: offer.marked || port.upstream,
		})
	}
}

/// How a candidate route orders, lower first: rank, then one with no marked link on its
/// path, which more neighbours may be sent, the next hop in use (`current`), the shorter
/// path, the neighbour's Node ID, and its session.
pub(crate) fn order<N: Key>(best: &Best<N>, current: Option<N>) -> Order<N> {
	let (draining, priority, cost) = best.offer.rank();
	(
		draining,
		priority,
		cost,
		best.offer.marked,
		current != Some(best.via),
		best.offer.path.len(),
		best.offer.path.last().expect("a heard route names its sender").node,
		best.via,
	)
}

/// The Node ID a node announces what it took over a marked link under.
fn marked_id(me: NodeId) -> NodeId {
	NodeId { marked: true, ..me }
}

/// Whether `path` crosses `relay`, either of its identities.
fn names(path: &[Hop], relay: Id) -> bool {
	path.iter().any(|hop| hop.node.relay == relay)
}

/// Whether a path names a marked identity only as the node it leads to, and then next
/// the node itself, unmarked, as its route begins: the identity holds no session.
fn marked_well(path: &[Hop]) -> bool {
	let rest = match path {
		[first, second, rest @ ..] if first.node.marked => {
			let own = Hop {
				node: NodeId {
					marked: false,
					..first.node
				},
				session: 0,
			};
			if *second != own {
				return false;
			}
			rest
		}
		[first, ..] if first.node.marked => return false,
		[_, rest @ ..] => rest,
		[] => path,
	};
	!rest.iter().any(|hop| hop.node.marked)
}

/// The pairs of relays a path joins, each with its session's number: the node that learned
/// the route over a session, the node at its far end, and the first node's number for it.
fn pairs(path: &[Hop]) -> impl Iterator<Item = ((Id, Id), u64)> + '_ {
	path.windows(2).map(|pair| floor_of(pair[1], pair[0].node))
}

/// The pair of relays `hop`'s session joins, as the floor files it, and the session's
/// number.
pub(crate) fn floor_of(hop: Hop, far: NodeId) -> ((Id, Id), u64) {
	((hop.node.relay, far.relay), hop.session)
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
	/// Each relay it entered or left a routing domain by, the first first: the one that
	/// announced it, then each grant's exporter and adopter, a relay that is both named once.
	pub chain: Vec<Id>,
	/// It crossed a marked link on its way to the node that told it, so it is never told
	/// over another, as a marked route is never sent: set where a node takes a claim over a
	/// marked link, and kept wherever the claim travels from there.
	pub marked: bool,
}

impl<A: Item> Claim<A> {
	fn rank(&self) -> Rank {
		(self.draining, Reverse(self.priority), self.cost)
	}

	pub(crate) fn lowered(&self, by: u64) -> Self {
		Self {
			item: self.item.lowered(by),
			..self.clone()
		}
	}

	/// The rank and mark of this claim, announced by another node of this node's routing
	/// domain, joined with `route`, this node's route to that node.
	fn joined(&self, route: &Offer) -> (Rank, bool) {
		let (draining, Reverse(priority), cost) = route.rank();
		let rank = (
			draining || self.draining,
			Reverse(priority.min(self.priority)),
			cost.saturating_add(self.cost).min(MAX_COST),
		);
		(rank, self.marked || route.marked)
	}

	/// This claim, announced by another node of this node's routing domain, as it stands
	/// here over `route`, this node's route to that node.
	pub(crate) fn over(&self, route: &Offer) -> Self {
		let ((draining, Reverse(priority), cost), marked) = self.joined(route);
		Self {
			draining,
			priority,
			cost,
			marked,
			..self.clone()
		}
	}

	/// This claim, as it stands at relay `me`, told across a grant over a link `me` marked
	/// when `upstream`: the chain names `me` as the claim leaves its routing domain, unless
	/// it does not `forward`. `None` where it is told to no one: one that crossed a marked
	/// link goes over no other, and one that would reach the longest path goes nowhere.
	pub(crate) fn exported(&self, me: Id, forwards: bool, upstream: bool) -> Option<Self> {
		if upstream && self.marked {
			return None;
		}
		let mut claim = self.clone();
		stamp(&mut claim.chain, me, forwards);
		(claim.chain.len() < MAX_HOPS).then_some(claim)
	}

	/// This claim, given by a neighbour on `port`, as relay `me` takes it: priced over the
	/// link, marked if `port` is, and its chain naming `me` as it enters `me`'s routing
	/// domain. `None` where `me` refuses it: the chain, with `me` appended, names a relay
	/// twice or reaches the longest path.
	pub(crate) fn adopted(&self, me: Id, forwards: bool, port: &Port) -> Option<Self> {
		if !takes(&self.chain, me, forwards) {
			return None;
		}
		let mut claim = self.clone();
		// Saturating, as `model::origin`'s claim cost does, so a claim's cost never
		// decides whether it can serve.
		claim.cost = claim.cost.saturating_add(port.cost).min(MAX_COST);
		claim.priority = claim.priority.min(port.priority);
		claim.marked |= port.upstream;
		stamp(&mut claim.chain, me, forwards);
		Some(claim)
	}
}

/// Whether relay `me` takes a claim whose chain is `chain` from a peer it exchanges no
/// routes with. One naming `me`, or any relay twice, left the routing domain of that relay
/// and came back to it, and one `me` would carry to the longest path is never taken, as a
/// route past the metric is never used.
fn takes(chain: &[Id], me: Id, forwards: bool) -> bool {
	// At most 31 relays (`well_built`), so a quadratic scan beats allocating a set.
	let once = (chain.iter().enumerate()).all(|(at, relay)| *relay != me && !chain[..at].contains(relay));
	once && chain.len() + usize::from(forwards) < MAX_HOPS
}

/// Name relay `me` at the end of a claim's chain as the claim enters or leaves its routing
/// domain, unless it ends the chain already, as a relay that adopted a claim across one
/// grant and exports it across the next does, or it does not `forward`.
fn stamp(chain: &mut Vec<Id>, me: Id, forwards: bool) {
	if forwards && chain.last() != Some(&me) {
		chain.push(me);
	}
}

/// How claims of one precedence order at a node, lower first: the rank of the claim
/// joined with `route`, the route to its announcer `origin`, or none for the node's own;
/// then one no marked link reached, which more neighbours may be told, as a route is
/// chosen; the fewer relays that took it on its way here; and the claimant. It reads only
/// the route's rank and mark, which [`Ways`] records, so a route change that moves the
/// closest claim is logged ([`Router::relog`]).
pub(crate) fn claim_order<A: Item>(claim: &Claim<A>, route: Option<&Offer>, origin: NodeId) -> ClaimOrder {
	let (rank, marked) = match route {
		Some(route) => claim.joined(route),
		None => (claim.rank(), claim.marked),
	};
	// Counted as the relays that took the claim on its way here: this node's own claim
	// names it already, and another's lacks the step from here to its announcer.
	let taken = claim.chain.len() + usize::from(route.is_some());
	(rank, marked, taken, origin)
}

/// Which claims of one Instance at one path may serve it, lower first: the one closest to
/// its own path, then the newest epoch. A route's rank chooses only among claims that tie
/// here, so an older run never stands in for a newer one that lost its route.
pub(crate) fn precedence<A: Item>(item: &A) -> (u64, Reverse<u64>) {
	(item.above(), Reverse(item.epoch()))
}

/// The relay that first announced `claim`, which `origin` claims: the first its chain
/// names, or `origin`'s relay for a claim that crossed no grant.
fn announcer<A>(claim: &Claim<A>, origin: NodeId) -> Id {
	claim.chain.first().copied().unwrap_or(origin.relay)
}

/// Which claim a copy is of: its [`announcer`], and how far above the broadcast's path
/// the claim sits ([`Item::above`]). One announcer can stand at a path with an exact
/// claim and a broader one presented there, and ending one leaves the other.
type Of = (Id, u64);

/// What `claim`, which `origin` claims, is a copy of.
fn of<A: Item>(claim: &Claim<A>, origin: NodeId) -> Of {
	(announcer(claim, origin), claim.item.above())
}

/// What `claim`, which a neighbour gave, is a copy of: none for one with no chain.
fn given_of<A: Item>(claim: &Claim<A>) -> Option<Of> {
	claim.chain.first().map(|by| (*by, claim.item.above()))
}

/// The copies of an ended claim that stood when its end arrived: past a grant, each was
/// announced again under another claimant, so its end comes later by its own way. Each
/// is held back until its holder ends it; a holder ends its copy before it claims again,
/// as a claimant's floors and a neighbour's stream order them, so a claim announced
/// again is never held back.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Ended<N> {
	/// The newest epoch ended.
	epoch: u64,
	/// The claimants whose claim is a copy.
	origins: BTreeSet<NodeId>,
	/// The neighbours whose given claim is a copy.
	peers: BTreeSet<N>,
}

/// Who holds a copy of a claim at a node: a claimant it knows through a route, or a
/// neighbour that gave it across a grant.
#[derive(Clone, Copy, Debug)]
enum Holder<N> {
	Origin(NodeId),
	Peer(N),
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
	/// Strictly positive to a forwarding node, so extending a route never leaves it as
	/// cheap.
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

/// Pairs of relays in the order they were added, each once.
#[derive(Clone, Debug, Default)]
struct Loose {
	next: u64,
	by_age: BTreeMap<u64, (Id, Id)>,
	age: BTreeMap<(Id, Id), u64>,
}

impl Loose {
	fn len(&self) -> usize {
		self.age.len()
	}

	fn insert(&mut self, pair: (Id, Id)) {
		if let std::collections::btree_map::Entry::Vacant(entry) = self.age.entry(pair) {
			entry.insert(self.next);
			self.by_age.insert(self.next, pair);
			self.next += 1;
		}
	}

	fn remove(&mut self, pair: &(Id, Id)) {
		if let Some(age) = self.age.remove(pair) {
			self.by_age.remove(&age);
		}
	}

	fn pop_oldest(&mut self) -> Option<(Id, Id)> {
		let (_, pair) = self.by_age.pop_first()?;
		self.age.remove(&pair);
		Some(pair)
	}
}

/// The route this node uses, as it stands here: the heard offer with the link added.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Best<N> {
	pub via: N,
	pub offer: Offer,
}

/// How candidate routes order, lower first, as [`order`] keys them.
pub(crate) type Order<N> = (bool, Reverse<i8>, Metric, bool, bool, usize, NodeId, N);

/// What a forwarding node's usable routes give the ways to its claims ([`Router::ways`]):
/// each one's next hop, rank, and mark, and the next hop of the one in use.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Ways<N> {
	routes: Vec<(N, Rank, bool)>,
	best: Option<N>,
}

/// What changed at a router since its caller last asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Changes<I> {
	/// The forwarding nodes whose selected route changed.
	pub routes: BTreeSet<NodeId>,
	/// The broadcasts whose claims, or what the reader shows of them, may have changed.
	pub broadcasts: BTreeSet<Broadcast<I>>,
	/// Claims a neighbour gave whose chain this node never takes, each taken as lost.
	pub looped: u64,
}

impl<I> Default for Changes<I> {
	fn default() -> Self {
		Self {
			routes: BTreeSet::new(),
			broadcasts: BTreeSet::new(),
			looped: 0,
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
	/// Pairs of relays with a session known closed, as the floors keep them.
	pub closed: usize,
	/// Claims shown unavailable while the reader waits to show them offline.
	pub waiting: usize,
}

/// One node's router: `N` keys its neighbours, `I` is the Instance a claim serves, and
/// `A` what else a publisher says of a claim.
#[derive(Clone)]
pub(crate) struct Router<N, I, A> {
	me: NodeId,
	/// The Node ID it announces the claims it took over a marked link under: `me` with the
	/// Marked bit, whose route is marked from the start, so no such claim ever crosses
	/// another marked link while every neighbour still gets each copy whole.
	me_marked: NodeId,
	/// Whether it carries claims between its peers, as a relay does: one that does names
	/// itself in the chain of every claim it takes from, or tells, a peer it exchanges no
	/// routes with. An application publishing its own claims names itself in no chain.
	forwards: bool,
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
	/// Per pair of relays, the highest session known closed, as the near relay numbered it.
	closed: BTreeMap<(Id, Id), u64>,
	/// The floors of pairs no heard route joins, oldest first.
	loose: Loose,
	/// Nodes named as draining that a live heard route still crosses.
	draining: BTreeSet<NodeId>,
	/// Forwarding nodes whose route changed since the neighbours were last told.
	stale: BTreeSet<NodeId>,
	best: BTreeMap<NodeId, Best<N>>,
	/// What each forwarding node's usable routes give the ways to its claims.
	ways: BTreeMap<NodeId, Ways<N>>,
	/// The forwarding nodes whose last selected route was marked, kept while no route
	/// reaches them and their claims stay known, so the claims keep the mark.
	marks: BTreeSet<NodeId>,
	/// The sent routes, live, whose path holds each hop.
	tx_hops: BTreeMap<Hop, BTreeMap<N, BTreeSet<NodeId>>>,
	/// The neighbours sent a route to each forwarding node, ended or not.
	sent_to: BTreeMap<NodeId, BTreeSet<N>>,
	known: BTreeMap<NodeId, Known<I, A>>,
	/// The revision of each of its identities' claims, from the first claim it announced
	/// under it, when it begins to offer a route to it.
	revs: BTreeMap<NodeId, Rev>,
	/// Per broadcast, the claims each endpoint or scoped neighbour gave this node.
	givers: BTreeMap<Broadcast<I>, BTreeMap<N, Claim<A>>>,
	/// Per broadcast, each claim ([`Of`]) its publisher ended, with the copies of it that
	/// other ways brought and still stand ([`Self::stale_copy`]).
	withdrawn: BTreeMap<Broadcast<I>, BTreeMap<Of, Ended<N>>>,
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
	/// Each identity's claims at each revision, for the search's truth.
	#[cfg(test)]
	history: BTreeMap<NodeId, Vec<Claims<I, A>>>,
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
		debug_assert!(!me.marked, "a node names itself unmarked");
		Self {
			me,
			me_marked: marked_id(me),
			forwards: true,
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
			loose: Loose::default(),
			draining: BTreeSet::new(),
			stale: BTreeSet::new(),
			best: BTreeMap::new(),
			ways: BTreeMap::new(),
			marks: BTreeSet::new(),
			tx_hops: BTreeMap::new(),
			sent_to: BTreeMap::new(),
			known: BTreeMap::new(),
			revs: BTreeMap::new(),
			givers: BTreeMap::new(),
			withdrawn: BTreeMap::new(),
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
			history: [me, marked_id(me)].map(|id| (id, vec![BTreeMap::new()])).into(),
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

	/// Carry no claim between peers, as an application publishing its own does: it then
	/// names itself in no chain. A node forwards until told this, before any session.
	pub fn endpoint(&mut self) {
		debug_assert!(self.peers.is_empty(), "set before any session");
		self.forwards = false;
	}

	/// How it names itself as the node announcing what it took over a marked link.
	#[cfg(test)]
	pub fn me_marked(&self) -> NodeId {
		self.me_marked
	}

	/// Whether `origin` is one of this node's own identities.
	fn is_self(&self, origin: NodeId) -> bool {
		origin == self.me || origin == self.me_marked
	}

	/// The identities this node offers a route to: those it announced a claim under.
	fn announcing(&self) -> Vec<NodeId> {
		self.revs.keys().copied().collect()
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
		// An endpoint forwards nothing, so nothing loops through a free link to one.
		if port.cost == 0 && port.role != Role::Endpoint {
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
		self.peers
			.get_mut(&peer)
			.expect("just connected")
			.asking
			.push_back(edit.clone());
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
	#[cfg(test)]
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
		// A floor covers every session of its pair at or below it, so a session closing while
		// an older one to the same relay lives is never named: its floor would cover the
		// older, live one, and every route still crossing it. Its routes go by replacement.
		if let Role::Routing(far) = gone.port.role {
			let older = (self.peers.values())
				.any(|peer| peer.session < gone.session && peer.routing().is_some_and(|id| id.relay == far.relay));
			if !older {
				let hop = Hop {
					node: self.me,
					session: gone.session,
				};
				self.closed(hop, far, None, &mut touched);
			}
		}
		self.forget();
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
	/// this node sends first: both sides drop what the removed prefixes covered there, and
	/// what the added prefixes cover follows, closed by SYNCED for each. A routing neighbour asks for the whole
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
		// As the edit names them, not as the sets differ: a prefix removed and added again
		// is another prefix to the peer, which dropped what the first one covered.
		for prefix in &edit.remove {
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
		let added = edit.add;
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
	#[cfg(test)]
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
				// Removed first, as the peer applies it: a prefix removed and added again
				// is asked for afresh.
				let edit = peer.asking.pop_front().ok_or(Error::Unasked)?;
				peer.answering = true;
				let mut lost = Vec::new();
				for prefix in &edit.remove {
					peer.asked.remove(prefix);
					lost.extend(
						peer.given
							.keys()
							.filter(|(path, _)| super::prefix::within(path, prefix))
							.cloned(),
					);
				}
				for prefix in &edit.add {
					peer.asked.insert(prefix.clone(), false);
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
			let origins: Vec<NodeId> = self.best.keys().copied().chain(self.announcing()).collect();
			self.stale.extend(origins);
		}
		let sender = self.peers[&from].routing().expect("a routing neighbour");
		match offer {
			Some(offer) => {
				// The sender extended a route that led to `origin` and did not name this
				// relay, by its own hop, and never past the longest path.
				let path = &offer.path;
				let built = path.len() >= 2 || offer.cost == 0;
				if path.first()
					!= Some(&Hop {
						node: origin,
						session: 0,
					}) || path.last().map(|hop| hop.node) != Some(sender)
					|| path.len() > MAX_HOPS
					|| names(path, self.me.relay)
					|| !built || !marked_well(path)
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
				self.unhold(&broadcast, |_, ended| _ = ended.origins.remove(&origin));
				self.dirty.insert(broadcast.clone());
				self.drop_claim(broadcast, origin, claim, None);
			}
			// Without any copy, this node no longer knows how new a claim of `origin` is:
			// a stale one may start it again, until the newer floor behind it arrives.
			#[cfg(test)]
			{
				self.forgets += 1;
			}
		}
		// Its claims kept their mark as they went; nothing else reads it.
		self.marks.remove(&origin);
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
			self.loose.remove(&pair);
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
					if self.closed.contains_key(&pair) {
						self.loose.insert(pair);
					}
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

	/// Forget each dead hop and draining node no live heard route names, and the oldest
	/// floors no heard route joins while they outnumber the pairs heard routes join.
	fn forget(&mut self) {
		// Bounded by the heard routes, as the floors they join are.
		while self.loose.len() > self.rx_pairs.len() {
			self.visit();
			let pair = self.loose.pop_oldest().expect("a loose floor");
			self.closed.remove(&pair);
		}
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

	/// Every route to `origin` a neighbour offers that this node may use, as it would
	/// hold it: priced and ranked over the link it came on.
	pub(crate) fn usable(&self, origin: NodeId) -> Vec<Best<N>> {
		let here = Here {
			closed: &self.closed,
			dead: &self.dead,
			draining: &self.draining,
		};
		let mut usable = Vec::new();
		for (&via, heard) in self.heard.get(&origin).into_iter().flatten() {
			self.visit();
			let peer = &self.peers[&via];
			if heard.ended {
				continue;
			}
			if let Some(offer) = here.arrive(&peer.port, peer.draining, &heard.offer) {
				usable.push(Best { via, offer });
			}
		}
		usable
	}

	/// Pick the route to `origin`, and mark it stale for the neighbours if it changed.
	fn reselect(&mut self, origin: NodeId) {
		let current = self.best.get(&origin).map(|best| best.via);
		let mut pick: Option<(Best<N>, Order<N>)> = None;
		let usable = self.usable(origin);
		let mut ways = Ways {
			routes: (usable.iter())
				.map(|mine| (mine.via, mine.offer.rank(), mine.offer.marked))
				.collect(),
			best: None,
		};
		for mine in usable {
			let key = order(&mine, current);
			if pick.as_ref().is_none_or(|(_, best)| key < *best) {
				pick = Some((mine, key));
			}
		}
		let pick = pick.map(|(best, _)| best);
		ways.best = pick.as_ref().map(|best| best.via);
		self.relog(origin, ways);
		let was = self.best.get(&origin);
		if was == pick.as_ref() {
			return;
		}
		let flipped = was.is_some() != pick.is_some();
		let moved = was.map(|best| best.offer.rank()) != pick.as_ref().map(|best| best.offer.rank());
		// Every claim shown through the route carries its mark, whatever the rank.
		let remarked = (was.zip(pick.as_ref())).is_some_and(|(was, pick)| was.offer.marked != pick.offer.marked);
		match &pick {
			Some(best) if best.offer.marked => _ = self.marks.insert(origin),
			Some(_) => _ = self.marks.remove(&origin),
			None if !self.known.contains_key(&origin) => _ = self.marks.remove(&origin),
			None => {}
		}
		match pick {
			Some(best) => self.best.insert(origin, best),
			None => self.best.remove(&origin),
		};
		// Whether its claims are reachable follows the route, and so does the rank of each
		// that the rank reaches.
		if (flipped || remarked)
			&& let Some(known) = self.known.get(&origin)
		{
			self.count(known.items.len());
			self.dirty.extend(known.items.keys().cloned());
		} else if moved && let Some(ranked) = self.ranked.get(&origin) {
			self.count(ranked.len());
			self.dirty.extend(ranked.iter().cloned());
		}
		self.stale.insert(origin);
	}

	/// Log `origin`'s route as changed once what its ways carry does. A route that is not
	/// the best still serves the origin's claims, so a change to any usable route counts,
	/// but one that leaves every way's next hop, rank, and mark alone, such as a new path
	/// at the same cost, changes nothing a reader follows.
	fn relog(&mut self, origin: NodeId, ways: Ways<N>) {
		let changed = match ways.routes.is_empty() {
			true => self.ways.remove(&origin).is_some(),
			false => self.ways.insert(origin, ways.clone()).as_ref() != Some(&ways),
		};
		if changed && let Some(log) = &mut self.log {
			log.routes.insert(origin);
		}
	}

	/// What `peer` should hear of `origin` now.
	fn offer_for(&self, origin: NodeId, peer: N) -> Option<Offer> {
		let to = self.peers.get(&peer)?;
		let to_id = to.routing()?;
		if self.is_self(origin) {
			// Nothing routes to an identity that announces nothing.
			if !self.revs.contains_key(&origin) {
				return None;
			}
			return Offer::own(self.me, origin, to.port.upstream);
		}
		let best = self.best.get(&origin)?;
		// Never back over the session it came on.
		if best.via == peer {
			return None;
		}
		(best.offer).onward(self.me, self.peers[&best.via].session, to_id, to.port.upstream)
	}

	/// Tell every routing neighbour of each route that changed, and a neighbour that newly
	/// asked of every route.
	fn advertise(&mut self) {
		for peer in std::mem::take(&mut self.fresh) {
			let origins: BTreeSet<NodeId> = (self.best.keys().copied()).chain(self.announcing()).collect();
			for origin in origins {
				self.advertise_to(origin, peer);
			}
			self.out.push((peer, Msg::Synced(Path::empty())));
		}
		self.advertise_stale(None);
	}

	/// Send each route that changed, but `except`'s, to every routing neighbour now, rather
	/// than at the next drain.
	fn advertise_stale(&mut self, except: Option<NodeId>) {
		let stale: Vec<NodeId> = (self.stale.iter().copied())
			.filter(|origin| Some(*origin) != except)
			.collect();
		if stale.is_empty() {
			return;
		}
		let peers: Vec<N> = self.routing.iter().copied().collect();
		for origin in stale {
			self.stale.remove(&origin);
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
		if started && let Some(known) = self.known.get(&origin) {
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
		// What a request may follow through the sender moved with its copy, even when this
		// node's view of `origin` did not.
		self.dirty
			.extend(changes.iter().map(|(broadcast, _)| broadcast.clone()));
		let known = self.known.entry(origin).or_default();
		// The newest floor wins, so an older copy never revives what a newer one withdrew.
		if floor <= known.floor && !mutated!(self, OldFloor) {
			return Ok(());
		}
		let mut visited = 0;
		// Two or more revisions skipped, here or by a relay that passed them on as one, can
		// hide an end and the same claim announced again, which differ in nothing.
		let lagged = floor.saturating_sub(known.floor) >= 2;
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
		// `origin` holds no copy it held before.
		if lagged {
			self.unhold_origin(origin);
		}
		Ok(())
	}

	/// Apply `changes` to this node's view of `origin` and pass them, with its floor, to
	/// every neighbour it offers `origin` to.
	fn publish(&mut self, origin: NodeId, changes: Vec<(Broadcast<I>, Change<A>)>) {
		// A withdrawal goes after every other route that started before it, so a claim
		// moving to another node, such as this node's other identity, arrives before it
		// leaves, at every hop it is forwarded. `origin`'s own route stays for the drain:
		// its floor already moved, so one that starts now would bring the change twice.
		let withdraws = (changes.iter()).any(|(_, change)| !matches!(change, Change::Live(_)));
		if withdraws && self.sent_to.contains_key(&origin) {
			self.advertise_stale(Some(origin));
		}
		// A neighbour that holds `origin`'s route hears what changed of it before another
		// claim of it: an upstream loses a route that turned marked, and any other neighbour
		// hears the mark, so neither passes the claim on as unmarked. Only a start brings
		// claims, and a neighbour that holds the route gets no start, so this can go now.
		if self.stale.contains(&origin) {
			let held: Vec<N> = self.sent_to.get(&origin).into_iter().flatten().copied().collect();
			for peer in held {
				self.advertise_to(origin, peer);
			}
		}
		let own = self.is_self(origin);
		let known = self.known.entry(origin).or_default();
		let floor = known.floor;
		let mut claimed = Vec::new();
		let mut unclaimed = Vec::new();
		let mut lost = Vec::new();
		let mut ended = Vec::new();
		// What each claim this message lost was a copy of, and at which epoch.
		let mut gone = Vec::new();
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
						match change {
							Change::Ended => ended.push((broadcast.clone(), of(&claim, origin), claim.item.epoch())),
							_ => gone.push((broadcast.clone(), of(&claim, origin), claim.item.epoch())),
						}
						// `restate` keeps its own claim's source.
						if *change == Change::Lost && !own {
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
		// The late end of a copy already held back says nothing new.
		let ended: Vec<(Broadcast<I>, Of, u64)> = (ended.into_iter())
			.filter(|(broadcast, of, epoch)| {
				let held = self.withdrawn.get(broadcast).and_then(|held| held.get(of));
				!held.is_some_and(|ended| ended.origins.contains(&origin) && *epoch <= ended.epoch)
			})
			.collect();
		// A claimant that let go of its copy, or now claims another announcer's claim or a
		// newer epoch, holds none.
		for (broadcast, change) in &changes {
			match change {
				Change::Live(claim) => {
					let (by, epoch) = (of(claim, origin), claim.item.epoch());
					self.unhold(broadcast, |of, ended| {
						if by != of || epoch > ended.epoch {
							ended.origins.remove(&origin);
						}
					});
				}
				_ => self.unhold(broadcast, |_, ended| _ = ended.origins.remove(&origin)),
			}
		}
		// Its publisher ended the claim: every copy of it standing now is stale, and this
		// node's own, taken across a grant from one, ends with it.
		for (broadcast, of, epoch) in ended {
			for held in self.withdraw(&broadcast, of, epoch, &gone) {
				if self.givers.contains_key(&held) {
					self.restate(held, Change::Ended);
				}
			}
		}
	}

	/// Hold back each copy standing now of the claim of `broadcast` its publisher ended,
	/// `ended`, at no newer than `epoch`, at every path a copy is presented at: at or below
	/// the ended one's path, lowered by as many segments. Returns the broadcasts where
	/// any stands. `lost` are the claims the same message lost, each with what it was a
	/// copy of and its epoch: a copy among them ends with the claim, held back or not.
	fn withdraw(
		&mut self,
		broadcast: &Broadcast<I>,
		ended: Of,
		epoch: u64,
		lost: &[(Broadcast<I>, Of, u64)],
	) -> Vec<Broadcast<I>> {
		let (path, instance) = broadcast;
		let (announcer, above) = ended;
		let depth = path.parts().count();
		let Some(base) = usize::try_from(above).ok().and_then(|above| depth.checked_sub(above)) else {
			return Vec::new();
		};
		// The end reaches only where its own presentation does: a claimant, such as a scoped
		// neighbour's adopter, may present a claim whose prefix lies outside what it may
		// claim, so the prefix alone would let it end copies anywhere under it.
		// A copy a neighbour gives is this node's own claim there, unless already held.
		let mut paths: BTreeSet<Path> = below(&self.claimants, path).map(|(path, _)| path.clone()).collect();
		// A copy this message lost has no claimant left to find it by.
		let lost_at = (lost.iter()).filter(|((at, lost), ..)| lost == instance && super::prefix::within(at, path));
		paths.extend(lost_at.map(|((at, _), ..)| at.clone()));
		paths.insert(path.clone());
		self.count(paths.len());
		let mut held = Vec::new();
		for at in paths {
			let of = (announcer, (at.parts().count() - base) as u64);
			let copy = |by: Of, claim: &Claim<A>| by == of && claim.item.epoch() <= epoch;
			let here = (at, instance.clone());
			// The reader there showed a copy, a claim that stands, or one that went with this
			// end: a lost claim that was no copy of the ended one waits out the debounce.
			let other =
				|(at, by, at_epoch): &(Broadcast<I>, Of, u64)| *at == here && !(*by == of && *at_epoch <= epoch);
			let lost_copy =
				|(at, by, at_epoch): &(Broadcast<I>, Of, u64)| *at == here && *by == of && *at_epoch <= epoch;
			let origins: BTreeSet<NodeId> = (self.claimants.get(&here.0).and_then(|claims| claims.get(instance)))
				.into_iter()
				.flatten()
				.filter(|origin| {
					let claim = self.known.get(origin).and_then(|known| known.items.get(&here));
					claim.is_some_and(|claim| copy(self::of(claim, **origin), claim))
				})
				.copied()
				.collect();
			let peers: BTreeSet<N> = (self.givers.get(&here).into_iter().flatten())
				.filter(|(_, claim)| given_of(claim).is_some_and(|by| copy(by, claim)))
				.map(|(peer, _)| *peer)
				.collect();
			self.count(origins.len() + peers.len());
			if origins.is_empty() && peers.is_empty() {
				// Nothing left to hold back, but the copy lost here ends with its claim.
				if lost.iter().any(lost_copy) && !lost.iter().any(other) {
					self.ended.insert(here.clone());
					self.dirty.insert(here);
				}
				continue;
			}
			let record = self.withdrawn.entry(here.clone()).or_default();
			let record = record.entry(of).or_insert_with(|| Ended {
				epoch,
				origins: BTreeSet::new(),
				peers: BTreeSet::new(),
			});
			record.epoch = record.epoch.max(epoch);
			record.origins.extend(origins);
			record.peers.extend(peers);
			if !lost.iter().any(other) {
				self.ended.insert(here.clone());
			}
			self.dirty.insert(here.clone());
			held.push(here);
		}
		held
	}

	/// A holder let go of its copy of an ended claim of `broadcast`, as `release` says:
	/// forget each ended claim no copy of stands any more.
	fn unhold(&mut self, broadcast: &Broadcast<I>, release: impl Fn(Of, &mut Ended<N>)) {
		let Some(held) = self.withdrawn.get_mut(broadcast) else {
			return;
		};
		held.retain(|of, ended| {
			release(*of, ended);
			!(ended.origins.is_empty() && ended.peers.is_empty())
		});
		if held.is_empty() {
			self.withdrawn.remove(broadcast);
		}
	}

	/// `origin` holds no copy of any ended claim.
	fn unhold_origin(&mut self, origin: NodeId) {
		let held: Vec<Broadcast<I>> = (self.withdrawn.iter())
			.filter(|(_, held)| held.values().any(|ended| ended.origins.contains(&origin)))
			.map(|(broadcast, _)| broadcast.clone())
			.collect();
		self.count(held.len());
		for broadcast in held {
			self.unhold(&broadcast, |_, ended| _ = ended.origins.remove(&origin));
			self.dirty.insert(broadcast);
		}
	}

	/// Whether `claim` of `broadcast`, held by `holder`, is a copy of a claim its publisher
	/// ended that stood when the end arrived ([`Ended`]): it would show a moment before its
	/// own end arrives by its own way.
	fn stale_copy(&self, broadcast: &Broadcast<I>, claim: &Claim<A>, holder: Holder<N>) -> bool {
		let Some(held) = self.withdrawn.get(broadcast) else {
			return false;
		};
		let by = match holder {
			Holder::Origin(origin) => Some(of(claim, origin)),
			Holder::Peer(_) => given_of(claim),
		};
		let Some(ended) = by.and_then(|by| held.get(&by)) else {
			return false;
		};
		let holds = match holder {
			Holder::Origin(origin) => ended.origins.contains(&origin),
			Holder::Peer(peer) => ended.peers.contains(&peer),
		};
		holds && claim.item.epoch() <= ended.epoch
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
		// It keeps the mark of the route it came on, which no neighbour it is shown to
		// knows of.
		let routed = !self.is_self(claimant) && self.marks.contains(&claimant);
		let claim = Claim {
			unavailable: true,
			marked: claim.marked || routed,
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
		for (broadcast, change) in changes {
			self.visit();
			let change = match change {
				Change::Live(claim) if !takes(&claim.chain, self.me.relay, self.forwards) => {
					if let Some(log) = &mut self.log {
						log.looped += 1;
					}
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
				Change::Live(claim) => {
					let port = &self.peers[&from].port;
					Change::Live(claim.adopted(self.me.relay, self.forwards, port).expect("taken"))
				}
				change => change,
			};
			let peer = self.peers.get_mut(&from).expect("a neighbour");
			match change {
				Change::Live(claim) => {
					peer.given.insert(broadcast.clone(), claim.clone());
					let (by, epoch) = (given_of(&claim), claim.item.epoch());
					self.givers.entry(broadcast.clone()).or_default().insert(from, claim);
					// A neighbour that gives another announcer's claim, or a newer epoch, holds
					// no copy of an older end.
					self.unhold(&broadcast, |of, ended| {
						if by != Some(of) || epoch > ended.epoch {
							ended.peers.remove(&from);
						}
					});
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
		self.unhold(broadcast, |_, ended| _ = ended.peers.remove(&peer));
	}

	/// Announce the cheapest claim any neighbour gave this node for `broadcast`, or
	/// withdraw it for `cause` when none is left. One that crossed a marked link goes under
	/// the marked identity, and moves between the two as its source does.
	fn restate(&mut self, broadcast: Broadcast<I>, cause: Change<A>) {
		let cheapest = (self.givers.get(&broadcast).into_iter().flatten())
			.filter(|(peer, claim)| !self.stale_copy(&broadcast, claim, Holder::Peer(**peer)))
			.map(|(&peer, claim)| {
				self.visit();
				// A claim from a neighbour that sent GOAWAY is draining from here on.
				let mut claim = claim.clone();
				claim.draining |= self.peers.get(&peer).is_some_and(|peer| peer.draining);
				(claim, peer)
			})
			// Precedence first and then the order `closest` takes, an unavailable claim last.
			.min_by_key(|(claim, peer)| {
				let owner = match claim.marked {
					true => self.me_marked,
					false => self.me,
				};
				(
					precedence(&claim.item),
					claim.unavailable,
					claim_order(claim, None, owner),
					*peer,
				)
			});
		let filed = [self.me, self.me_marked].into_iter().find(|id| {
			self.known
				.get(id)
				.is_some_and(|known| known.items.contains_key(&broadcast))
		});
		match cheapest {
			Some((claim, peer)) => {
				self.sources.insert(broadcast.clone(), peer);
				let owner = match claim.marked {
					true => self.me_marked,
					false => self.me,
				};
				let was = filed.and_then(|id| self.known[&id].items.get(&broadcast));
				if filed == Some(owner) && was == Some(&claim) {
					// The source alone moved, which a scoped neighbour must hear of.
					self.dirty.insert(broadcast);
					return;
				}
				self.revise(owner, broadcast.clone(), Change::Live(claim));
				// A claim that moved between the identities leaves the old one after the new
				// one holds it, and `publish` sends the new one's route first.
				if let Some(old) = filed.filter(|old| *old != owner) {
					self.revise(old, broadcast, Change::Lost);
				}
			}
			None => {
				let source = self.sources.remove(&broadcast);
				let Some(old) = filed else {
					return;
				};
				if cause == Change::Lost {
					let was = self.known[&old].items[&broadcast].clone();
					self.drop_claim(broadcast.clone(), old, was, source);
				}
				self.revise(old, broadcast, cause);
			}
		}
	}

	/// Change the claims this node announces under its identity `id`, at a new revision.
	fn revise(&mut self, id: NodeId, broadcast: Broadcast<I>, change: Change<A>) {
		let rev = self.revs.get(&id).copied();
		if rev.is_none() {
			// Its first claim: the route starts, and brings the claim with it.
			self.stale.insert(id);
		}
		let rev = rev.unwrap_or_default() + 1;
		self.revs.insert(id, rev);
		self.known.entry(id).or_default().floor = rev;
		self.publish(id, vec![(broadcast, change)]);
		#[cfg(test)]
		self.history.entry(id).or_default().push(self.known[&id].items.clone());
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

	/// Where a request for `broadcast` naming `origin` goes from here, never back to the
	/// neighbour `asker` that sent it. `None` fails it.
	pub fn step(&self, origin: NodeId, (path, instance): &Broadcast<I>, asker: Option<N>) -> Option<Next<N>> {
		match self.is_self(origin) {
			// As `pick`, the claim at the most specific path covering it that the reader
			// shows: the neighbour behind it if it is this node's own, available, and of the
			// best precedence there, and closed while it is unavailable, lost, outranked,
			// or another node's.
			true => {
				let from = (ancestors(path).rev()).find(|above| self.reader.get(instance.clone(), above).is_some())?;
				let claim = (from, instance.clone());
				let source = *self.sources.get(&claim)?;
				let own = self.known.get(&origin)?.items.get(&claim)?;
				// Only a claim of the best precedence there may serve, reachable or not, as
				// in `closest`; among those, this node's own serves whatever its rank.
				let best = (self.claims_of(&claim)).map(|(_, claim)| precedence(&claim.item)).min();
				(!own.unavailable && best == Some(precedence(&own.item))).then_some(Next::Local(source))
			}
			// The best route whose neighbour holds the claim: one that has yet to hear it
			// would refuse the request.
			false => {
				let known = self.known.get(&origin);
				let claim = (ancestors(path).rev())
					.map(|above| (above, instance.clone()))
					.find(|claim| known.is_some_and(|known| known.items.contains_key(claim)));
				match claim {
					Some(claim) => self.lead(origin, &claim, asker).map(Next::Via),
					None => (self.best.get(&origin))
						.filter(|best| Some(best.via) != asker)
						.map(|best| Next::Via(best.via)),
				}
			}
		}
	}

	/// Whether a request for `broadcast` naming `origin` from `asker`, which [`Self::step`]
	/// has nowhere to send, waits for a way rather than fails: this node routes to `origin`
	/// other than through `asker` and knows its claim available and of the best precedence,
	/// so it shows the claim, but no other neighbour's copy holds it yet. A route arrives
	/// before the claims behind it, and a copy that held the claim can leave with its session
	/// while this node's own copy stays, so nothing it shows changes when a way appears: a
	/// request it refused now would never be asked again.
	pub fn awaits(&self, origin: NodeId, (path, instance): &Broadcast<I>, asker: Option<N>) -> bool {
		if self.is_self(origin) {
			return false;
		}
		// The asker's own copy is no way, so a route only through it leads nowhere.
		let routed = match self.best.get(&origin) {
			None => false,
			Some(best) if Some(best.via) != asker => true,
			Some(_) => self.usable(origin).iter().any(|route| Some(route.via) != asker),
		};
		if !routed {
			return false;
		}
		let Some(known) = self.known.get(&origin) else {
			return false;
		};
		(ancestors(path).rev())
			.map(|above| (above, instance.clone()))
			.find(|claim| known.items.contains_key(claim))
			.is_some_and(|claim| {
				// Only a claim of the best precedence there is shown, as in `closest`.
				let best = (self.claims_of(&claim)).map(|(_, claim)| precedence(&claim.item)).min();
				(self.claims_of(&claim))
					.any(|(by, claim)| by == origin && !claim.unavailable && best == Some(precedence(&claim.item)))
			})
	}

	/// Whether `via`'s copy of `origin`'s claims holds `broadcast` as this node knows it:
	/// available, and of the same precedence. A neighbour routes to `origin` before its claims
	/// reach it, and refuses a request for a claim it has not heard, or heard only unavailable
	/// or in an older revision.
	fn holds(&self, origin: NodeId, via: N, broadcast: &Broadcast<I>) -> bool {
		let Some(known) = (self.known.get(&origin)).and_then(|known| known.items.get(broadcast)) else {
			return false;
		};
		(self.heard.get(&origin))
			.and_then(|offers| offers.get(&via))
			.and_then(|heard| heard.items.get(broadcast))
			.is_some_and(|heard| !heard.unavailable && precedence(&heard.item) == precedence(&known.item))
	}

	/// The neighbour a request for `origin`'s claim of `broadcast` from `asker` leaves by:
	/// the route to `origin` when its neighbour holds the claim, else the best usable route
	/// whose does, never `asker`'s.
	fn lead(&self, origin: NodeId, broadcast: &Broadcast<I>, asker: Option<N>) -> Option<N> {
		let best = self.best.get(&origin)?;
		let leads = |via: N| Some(via) != asker && self.holds(origin, via, broadcast);
		if leads(best.via) {
			return Some(best.via);
		}
		(self.usable(origin).into_iter())
			.filter(|route| leads(route.via))
			.min_by_key(|route| order(route, Some(best.via)))
			.map(|route| route.via)
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
			(!self.stale_copy(&broadcast, claim, Holder::Origin(origin))).then_some((origin, claim))
		})
	}

	/// Every claim of `broadcast` a request may follow from here, as [`Self::closest`]
	/// chooses among them: of the best precedence, available, and reached by a route.
	pub fn ways(&self, broadcast: &Broadcast<I>) -> Vec<Way<N, A>> {
		let closest = self.closest(broadcast).map(|(origin, _)| origin);
		let top = (self.claims_of(broadcast))
			.map(|(_, claim)| precedence(&claim.item))
			.min();
		let mut ways = Vec::new();
		for (origin, claim) in self.claims_of(broadcast) {
			if Some(precedence(&claim.item)) != top || claim.unavailable {
				continue;
			}
			// Every neighbour that gave this node its own claim serves it, so a reader
			// already holds another copy when the one it follows closes. Each leads to
			// the same claimant, so none ranks ahead here: the reader takes the cheapest,
			// and the newest of those, so a peer that reconnects serves the moment its
			// new session gives the claim.
			if self.is_self(origin) {
				for (peer, given) in self.givers.get(broadcast).into_iter().flatten() {
					self.visit();
					if precedence(&given.item) != precedence(&claim.item)
						|| given.unavailable
						|| self.stale_copy(broadcast, given, Holder::Peer(*peer))
					{
						continue;
					}
					let mut given = given.clone();
					given.draining |= self.peers.get(peer).is_some_and(|peer| peer.draining);
					ways.push(Way {
						origin,
						next: Next::Local(*peer),
						claim: given,
						closest: closest == Some(origin),
					});
				}
				continue;
			}
			// Every usable route to its claimant whose neighbour holds the claim serves it,
			// for the same reason.
			let lead = self.lead(origin, broadcast, None);
			for route in self.usable(origin) {
				if !self.holds(origin, route.via, broadcast) {
					continue;
				}
				ways.push(Way {
					origin,
					next: Next::Via(route.via),
					claim: claim.over(&route.offer),
					closest: closest == Some(origin) && lead == Some(route.via),
				});
			}
		}
		ways
	}

	/// Whether `origin`'s claim of `broadcast` crossed a marked link on its way here: the
	/// claim says so, as one a node took over a marked link does wherever it travels, or
	/// the route to another node's claim crossed one.
	fn crossed_mark(&self, origin: NodeId, broadcast: &Broadcast<I>) -> bool {
		let claim = (self.known.get(&origin))
			.and_then(|known| known.items.get(broadcast))
			.is_some_and(|claim| claim.marked);
		claim || (!self.is_self(origin) && self.marks.contains(&origin))
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
			let route = match self.is_self(origin) {
				true => None,
				false => match self.best.get(&origin) {
					Some(best) => Some(&best.offer),
					None => continue,
				},
			};
			let key = claim_order(claim, route, origin);
			if pick.as_ref().is_none_or(|(best, _)| key < *best) {
				pick = Some((key, claim));
			}
		}
		pick.map(|((_, _, _, origin), claim)| (origin, claim))
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
			// A copy of a claim its publisher ended claims nothing.
			let claimed = (self.claimants.get(path)).is_some_and(|claims| claims.contains_key(instance))
				&& self.claims_of(broadcast).next().is_some();
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
		let source = |origin: NodeId| self.is_self(origin) && self.sources.get(&broadcast) == Some(&peer);
		// Each claim carries whether it crossed a marked link on its way here, which
		// `exported` reads.
		let claim = match self.closest(&broadcast) {
			Some((origin, _)) if source(origin) => return None,
			Some((origin, claim)) if self.is_self(origin) => claim.clone(),
			Some((origin, claim)) => claim.over(&self.best[&origin].offer),
			None => match self
				.claims_of(&broadcast)
				.min_by_key(|(origin, claim)| (precedence(&claim.item), *origin))
			{
				Some((origin, _)) if source(origin) => return None,
				Some((origin, claim)) => Claim {
					unavailable: true,
					marked: self.crossed_mark(origin, &broadcast),
					..claim.clone()
				},
				// Lost, and waiting out the debounce: what it showed, to every asker alike.
				None => {
					let gone = self.gone.get(from)?.get(instance)?;
					if gone.source == Some(peer) {
						return None;
					}
					gone.claim.clone()
				}
			},
		};
		let claim = claim.exported(self.me.relay, self.forwards, self.peers[&peer].port.upstream)?;
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

	/// The claims this node announced under its identity `id` at each revision, the first
	/// empty.
	pub fn history(&self, id: NodeId) -> Option<&[Claims<I, A>]> {
		self.history.get(&id).map(Vec::as_slice)
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

	/// The forwarding nodes some neighbour offers this node a route to.
	pub fn heard(&self) -> Vec<NodeId> {
		self.heard.keys().copied().collect()
	}

	/// The forwarding nodes this node routes to.
	pub fn routes(&self) -> Vec<NodeId> {
		self.best.keys().copied().collect()
	}

	/// Whether it offers a route to itself: it has announced a claim under its own Node ID.
	pub fn announced(&self) -> bool {
		self.revs.contains_key(&self.me)
	}

	/// This node's revision of the claims it announces under its own Node ID, and each of
	/// them as it stands.
	pub fn own(&self) -> (Rev, Claims<I, A>) {
		self.own_as(self.me)
	}

	/// Of the claims it announces under its identity `id`, the revision and each as it
	/// stands.
	pub fn own_as(&self, id: NodeId) -> (Rev, Claims<I, A>) {
		let items = self.known.get(&id).map(|known| known.items.clone());
		(
			self.revs.get(&id).copied().unwrap_or_default(),
			items.unwrap_or_default(),
		)
	}

	/// Every copy of a forwarding node's claims this node holds, with its floor: each
	/// neighbour's, and the merged view.
	pub fn copies(&self) -> Vec<(NodeId, Rev, &Claims<I, A>)> {
		let heard = (self.heard.iter())
			.flat_map(|(origin, offers)| offers.values().map(move |heard| (*origin, heard.floor, &heard.items)));
		let known = (self.known.iter())
			.filter(|(origin, _)| !self.is_self(**origin))
			.map(|(origin, known)| (*origin, known.floor, &known.items));
		heard.chain(known).collect()
	}

	/// Hash what decides this node's future as of its clock, leaving out the indexes,
	/// which follow from the rest, and the work counter. Its output and reader events
	/// must already be taken.
	pub fn fingerprint(&self, hasher: &mut impl std::hash::Hasher) {
		let Self {
			me,
			me_marked: _,
			forwards,
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
			loose,
			draining,
			stale,
			best,
			ways: _,
			marks,
			tx_hops: _,
			sent_to: _,
			known,
			revs,
			givers: _,
			withdrawn,
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
		// Only the order the loose floors go in matters, not the count behind it.
		let loose: Vec<&(Id, Id)> = loose.by_age.values().collect();
		let state = (
			(me, forwards, debounce, sessions, peers, routing, fresh, heard, unnamed),
			(dead, closed, loose, draining, stale, best, marks, known, revs),
			(sources, ended, dirty, waiting, since, gone, reader.shown(), out),
			withdrawn,
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
		let loose: BTreeSet<&(Id, Id)> = self
			.closed
			.keys()
			.filter(|pair| !rx_pairs.contains_key(*pair))
			.collect();
		assert_eq!(self.loose.age.keys().collect::<BTreeSet<_>>(), loose, "loose floors");
		assert_eq!(self.loose.by_age.len(), loose.len(), "loose order");
		assert!(loose.len() <= rx_pairs.len(), "loose floors outnumber the joined pairs");

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
		// Each holder of an ended claim's copy still holds one.
		for (broadcast, held) in &self.withdrawn {
			assert!(!held.is_empty(), "an empty withdrawal");
			for (ended_of, ended) in held {
				assert!(
					!(ended.origins.is_empty() && ended.peers.is_empty()),
					"a withdrawal no copy holds"
				);
				let copy = |by: Of, claim: &Claim<A>| by == *ended_of && claim.item.epoch() <= ended.epoch;
				for origin in &ended.origins {
					let claim = self.known.get(origin).and_then(|known| known.items.get(broadcast));
					assert!(
						claim.is_some_and(|claim| copy(of(claim, *origin), claim)),
						"held by {origin:?}"
					);
				}
				for peer in &ended.peers {
					let claim = givers.get(broadcast).and_then(|claims| claims.get(peer));
					assert!(
						claim.is_some_and(|claim| given_of(claim).is_some_and(|by| copy(by, claim))),
						"held by {peer:?}"
					);
				}
			}
		}
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
		// Rebuilt with what the work count leaves out.
		let work = self.work.get();
		let origins: BTreeSet<&NodeId> = self.heard.keys().chain(self.ways.keys()).collect();
		for origin in origins {
			let routes: Vec<_> = (self.usable(*origin).iter())
				.map(|mine| (mine.via, mine.offer.rank(), mine.offer.marked))
				.collect();
			let ways = (!routes.is_empty()).then(|| Ways {
				routes,
				best: self.best.get(origin).map(|best| best.via),
			});
			assert_eq!(self.ways.get(origin), ways.as_ref(), "the ways to {origin:?}");
		}
		self.work.set(work);
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
		for (origin, best) in &self.best {
			assert_eq!(self.marks.contains(origin), best.offer.marked, "marks");
		}
		// A claim is marked exactly when it is filed under the marked identity, whose route
		// is marked, so a marked claim never travels on an unmarked route.
		for (id, marked) in [(self.me, false), (self.me_marked, true)] {
			let items = self.known.get(&id).into_iter().flat_map(|known| known.items.values());
			for claim in items {
				assert_eq!(claim.marked, marked, "a claim under the wrong identity");
			}
		}
		for origin in &self.marks {
			assert!(
				self.best.contains_key(origin) || self.known.contains_key(origin),
				"marks"
			);
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
		NodeId { relay, marked: false }
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
			marked: false,
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
			marked: false,
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
			assert_eq!(router.step(me, &broadcast, None), Some(Next::Local(endpoint)), "{path}");
		}
		for broadcast in [(p("other"), 7), (p("live/a"), 8)] {
			assert_eq!(router.pick(&broadcast), None, "{broadcast:?}");
			assert_eq!(router.step(me, &broadcast, None), None, "{broadcast:?}");
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
		assert_eq!(router.step(me, &under, None), None);
		router.timeout(router.deadline().expect("a debounce"));
		assert_eq!(router.pick(&under), Some(me));
		assert_eq!(router.step(me, &under, None), Some(Next::Local(2)));
	}

	/// A neighbour routes to a node before that node's claims reach it, and refuses a request
	/// for one it has not heard. A request follows a claim only through a neighbour whose own
	/// copy holds it, and through the cheaper one once that copy catches up.
	#[test]
	fn a_request_follows_only_a_neighbour_that_holds_the_claim() {
		let mut router = Router::<usize, u64, Run>::new(node(1), debounce(HOP));
		open(&mut router, 2, Role::Routing(node(2)));
		open(&mut router, 3, Role::Routing(node(3)));
		let broadcast = (p("live"), 7);
		let claims = |floor| Msg::Claims {
			origin: node(5),
			changes: vec![(broadcast.clone(), claim(run(1), 0))],
			floor,
		};
		// Both route to node 5, the cheaper through 2, whose copy of its claims is empty.
		router.recv(2, route(5, 2, 1)).unwrap();
		router.recv(3, route(5, 3, 2)).unwrap();
		router.recv(3, claims(1)).unwrap();
		assert_eq!(router.best(node(5)).map(|(via, _)| via), Some(2));
		let ways = |router: &Router<usize, u64, Run>| {
			(router.ways(&broadcast).into_iter())
				.map(|way| (way.next, way.closest))
				.collect::<Vec<_>>()
		};
		assert_eq!(ways(&router), [(Next::Via(3), true)]);
		assert_eq!(router.step(node(5), &broadcast, None), Some(Next::Via(3)));
		router.recv(2, claims(1)).unwrap();
		assert_eq!(ways(&router), [(Next::Via(2), true), (Next::Via(3), false)]);
		assert_eq!(router.step(node(5), &broadcast, None), Some(Next::Via(2)));
	}

	/// A neighbour whose copy holds the claim only unavailable refuses a request for it too,
	/// though this node knows it available from another. Its copy catching up makes it a way.
	#[test]
	fn a_request_skips_a_neighbour_that_holds_the_claim_unavailable() {
		let mut router = Router::<usize, u64, Run>::new(node(1), debounce(HOP));
		open(&mut router, 2, Role::Routing(node(2)));
		open(&mut router, 3, Role::Routing(node(3)));
		let broadcast = (p("live"), 7);
		let claims = |unavailable, floor| Msg::Claims {
			origin: node(5),
			changes: vec![(
				broadcast.clone(),
				match claim(run(1), 0) {
					Change::Live(claim) => Change::Live(Claim { unavailable, ..claim }),
					other => other,
				},
			)],
			floor,
		};
		router.recv(2, route(5, 2, 1)).unwrap();
		router.recv(3, route(5, 3, 2)).unwrap();
		router.recv(2, claims(true, 1)).unwrap();
		router.recv(3, claims(false, 2)).unwrap();
		let ways = |router: &Router<usize, u64, Run>| {
			(router.ways(&broadcast).into_iter())
				.map(|way| (way.next, way.closest))
				.collect::<Vec<_>>()
		};
		assert_eq!(ways(&router), [(Next::Via(3), true)]);
		assert_eq!(router.step(node(5), &broadcast, None), Some(Next::Via(3)));
		router.recv(2, claims(false, 3)).unwrap();
		assert_eq!(ways(&router), [(Next::Via(2), true), (Next::Via(3), false)]);
		assert_eq!(router.step(node(5), &broadcast, None), Some(Next::Via(2)));
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
			router.step(me, &broadcast, None),
			Some(Next::Local(3)),
			"a newer epoch, however dear"
		);
		assert_eq!(router.own().1[&broadcast].item, run(2));
		router.recv(2, announce(claim(Run { epoch: 3, above: 1 }, 0))).unwrap();
		assert_eq!(
			router.step(me, &broadcast, None),
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
			router.step(me, &broadcast, None),
			Some(Next::Local(3)),
			"named, its own run serves"
		);
		router.recv(4, claims(Run { epoch: 4, above: 1 }, 3)).unwrap();
		assert_eq!(router.pick(&broadcast), Some(me), "a broader claim, however new");
		router.recv(4, claims(run(3), 4)).unwrap();
		assert_eq!(router.pick(&broadcast), Some(node(5)), "a newer claim");
		assert_eq!(
			router.step(me, &broadcast, None),
			None,
			"named, its older run never serves"
		);
		// The newer claim's route goes while its claim stays known: the older run never
		// stands in for it.
		router.recv(4, route(5, 4, Metric::MAX)).unwrap();
		assert_eq!(router.best(node(5)), None);
		assert_eq!(router.pick(&broadcast), None);
		assert_eq!(router.step(me, &broadcast, None), None, "named, still closed");
		let shown = &router.shown()[&(7, p("live"))];
		assert_eq!((shown.item, shown.available), (run(3), false));
	}

	/// A route whose cost a link would carry past the metric is never used, and never wraps
	/// round to a cheap one. A claim's cost saturates at the wire ceiling instead, so it
	/// never decides whether the claim can serve.
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
					*broadcast == far && matches!(change, Change::Live(claim) if claim.cost == MAX_COST)
				}))
		});
		assert!(told, "told at the metric's ceiling");
		let announce = Msg::Announce {
			changes: vec![((p("near"), 7), claim(run(0), Metric::MAX))],
		};
		router.recv(3, announce).unwrap();
		assert_eq!(router.own().1[&(p("near"), 7)].cost, MAX_COST, "an endpoint's claim");
	}

	/// A route that moves onto another path at the same rank changes no way to its claims,
	/// so its caller relanes none of them; one that costs more does.
	#[test]
	fn a_route_logs_a_change_only_when_its_ways_do() {
		let mut router = Router::<usize, u64, Run>::new(node(1), debounce(HOP));
		router.follow();
		open(&mut router, 2, Role::Routing(node(2)));
		router.recv(2, route(5, 2, 1)).unwrap();
		assert_eq!(router.changes().routes, BTreeSet::from([node(5)]), "a new route");
		let mut moved = route(5, 2, 1);
		if let Msg::Route { offer: Some(offer), .. } = &mut moved {
			let mut path = offer.path.to_vec();
			path.insert(
				1,
				Hop {
					node: node(4),
					session: 1,
				},
			);
			offer.path = path.into();
		}
		router.recv(2, moved).unwrap();
		assert!(router.changes().routes.is_empty(), "the same rank on another path");
		router.recv(2, route(5, 2, 2)).unwrap();
		assert_eq!(router.changes().routes, BTreeSet::from([node(5)]), "a dearer route");
	}

	/// A claim reached over a route ranks as the route does too: draining if the route is,
	/// at the lower of the two priorities, and at the sum of the two costs. Beside it, this
	/// node's own copy leads to the neighbour that gave it.
	#[test]
	fn a_way_ranks_with_the_route_it_takes() {
		let mut router = Router::<usize, u64, Run>::new(node(1), debounce(HOP));
		open(&mut router, 2, Role::Routing(node(2)));
		let Msg::Route {
			origin,
			offer: Some(offer),
		} = route(5, 2, 3)
		else {
			unreachable!("a live route");
		};
		let offer = Offer {
			draining: true,
			priority: -3,
			..offer
		};
		router
			.recv(
				2,
				Msg::Route {
					origin,
					offer: Some(offer),
				},
			)
			.unwrap();
		let broadcast = (p("live"), 7);
		let claims = Msg::Claims {
			origin: node(5),
			changes: vec![(broadcast.clone(), claim(run(0), 2))],
			floor: 1,
		};
		router.recv(2, claims).unwrap();
		open(&mut router, 3, Role::Endpoint);
		let announce = Msg::Announce {
			changes: vec![(broadcast.clone(), claim(run(0), 0))],
		};
		router.recv(3, announce).unwrap();
		let ways = router.ways(&broadcast);
		let [own, routed] = ways.as_slice() else {
			panic!("two ways: {ways:?}");
		};
		assert_eq!((own.origin, &own.next), (node(1), &Next::Local(3)));
		assert_eq!((routed.origin, &routed.next), (node(5), &Next::Via(2)));
		let claim = &routed.claim;
		assert_eq!((claim.draining, claim.priority, claim.cost), (true, -3, 6));
	}

	/// Of the neighbours that gave this node a claim, only those whose own copy is of the
	/// best precedence and available serve it.
	#[test]
	fn a_way_leads_only_to_a_giver_that_can_serve() {
		let mut router = Router::<usize, u64, Run>::new(node(1), debounce(HOP));
		let broadcast = (p("live"), 7);
		let give = |router: &mut Router<usize, u64, Run>, peer, item, unavailable| {
			open(router, peer, Role::Endpoint);
			let Change::Live(claim) = claim(item, 0) else {
				unreachable!("a live claim");
			};
			let change = Change::Live(Claim { unavailable, ..claim });
			let announce = Msg::Announce {
				changes: vec![(broadcast.clone(), change)],
			};
			router.recv(peer, announce).unwrap();
		};
		give(&mut router, 2, run(1), false);
		give(&mut router, 3, run(0), false);
		give(&mut router, 4, run(1), true);
		let next: Vec<_> = (router.ways(&broadcast).into_iter()).map(|way| way.next).collect();
		assert_eq!(next, [Next::Local(2)]);
	}

	/// A route as long as the longest path is held, but never used: no link could extend
	/// it. One hop shorter, it is.
	#[test]
	fn a_route_at_the_longest_path_is_never_used() {
		let mut router = Router::<usize, u64, Run>::new(node(1), debounce(HOP));
		open(&mut router, 2, Role::Routing(node(2)));
		let route = |hops: usize| {
			let between = (10..).take(hops - 2).map(|relay| Hop {
				node: node(relay),
				session: 1,
			});
			let path: Vec<Hop> = std::iter::once(Hop {
				node: node(5),
				session: 0,
			})
			.chain(between)
			.chain([Hop {
				node: node(2),
				session: 1,
			}])
			.collect();
			let offer = Offer {
				path: path.into(),
				draining: false,
				priority: i8::MAX,
				cost: 1,
				marked: false,
			};
			Msg::Route {
				origin: node(5),
				offer: Some(offer),
			}
		};
		router.recv(2, route(MAX_HOPS)).unwrap();
		assert_eq!(router.best(node(5)), None);
		router.recv(2, route(MAX_HOPS - 1)).unwrap();
		assert!(router.best(node(5)).is_some());
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
				marked: false,
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

	/// A live claim with `chain`.
	fn chained(chain: Vec<Id>) -> Change<Run> {
		let Change::Live(claim) = claim(run(0), 0) else {
			unreachable!("a live claim");
		};
		Change::Live(Claim { chain, ..claim })
	}

	/// What `peer` was told of each broadcast's chain.
	fn chains(out: Vec<(usize, Msg<u64, Run>)>, peer: usize) -> BTreeMap<Broadcast<u64>, Vec<Id>> {
		(out.into_iter())
			.filter(|(to, _)| *to == peer)
			.flat_map(|(_, msg)| match msg {
				Msg::Announce { changes } => changes,
				_ => Vec::new(),
			})
			.filter_map(|(broadcast, change)| match change {
				Change::Live(claim) => Some((broadcast, claim.chain)),
				_ => None,
			})
			.collect()
	}

	/// A claim's chain names the relay that announced it and, for each grant it crossed,
	/// the relay that exported it and the one that adopted it: a relay names itself as a
	/// claim enters its routing domain and as it leaves, once where it does both.
	#[test]
	fn a_chain_names_each_relay_a_claim_entered_or_left_a_domain_by() {
		let mut router = Router::<usize, u64, Run>::new(node(1), debounce(HOP));
		open(&mut router, 2, Role::Endpoint);
		open(&mut router, 3, Role::Scoped(vec![p(ROOT)]));
		open(&mut router, 4, Role::Routing(node(4)));
		open(&mut router, 5, Role::Scoped(vec![p(ROOT)]));
		router
			.recv(
				2,
				Msg::Announce {
					changes: vec![((p("own"), 7), claim(run(0), 0))],
				},
			)
			.unwrap();
		router
			.recv(
				3,
				Msg::Announce {
					changes: vec![((p("adopted"), 7), chained(vec![8, 9]))],
				},
			)
			.unwrap();
		router.recv(4, route(4, 4, 1)).unwrap();
		router
			.recv(
				4,
				Msg::Claims {
					origin: node(4),
					changes: vec![((p("routed"), 7), chained(vec![4]))],
					floor: 1,
				},
			)
			.unwrap();
		let own = router.own().1;
		assert_eq!(own[&(p("own"), 7)].chain, [1], "the announcing relay");
		assert_eq!(own[&(p("adopted"), 7)].chain, [8, 9, 1], "the adopter");
		router.drain();
		let edit = Edit {
			remove: Vec::new(),
			add: vec![p(ROOT)],
		};
		router.edit(5, edit).unwrap();
		let told = chains(router.drain(), 5);
		assert_eq!(
			told,
			BTreeMap::from([
				((p("own"), 7), vec![1]),
				((p("adopted"), 7), vec![8, 9, 1]),
				((p("routed"), 7), vec![4, 1]),
			]),
			"the exporter, once where it announced the claim"
		);
	}

	/// A node across a grant takes no claim that crossed it before, in whichever role,
	/// nor one naming another relay twice: such a claim left that relay's routing domain
	/// and came back. A relay is named by its Relay, so the same holds where the grant is
	/// marked and the claim would go under the marked Node ID.
	#[test]
	fn a_claim_naming_a_relay_twice_is_not_taken() {
		for upstream in [false, true] {
			let mut router = Router::<usize, u64, Run>::new(node(1), debounce(HOP));
			let port = Port {
				upstream,
				..port(Role::Scoped(vec![p(ROOT)]))
			};
			router.connect(2, port).unwrap();
			router.recv(2, Msg::Edited).unwrap();
			let refused = [
				("announced", vec![1, 5, 6]),
				("exported", vec![4, 1, 6, 7]),
				("adopted", vec![4, 5, 1, 7]),
				("revisits", vec![4, 5, 6, 5, 8]),
			];
			let changes = (refused.iter().cloned())
				.chain([("fresh", vec![4, 5, 6])])
				.map(|(path, chain)| ((p(path), 7), chained(chain)))
				.collect();
			router.recv(2, Msg::Announce { changes }).unwrap();
			let own: Vec<Broadcast<u64>> = (router.own().1.into_keys())
				.chain(router.own_as(router.me_marked()).1.into_keys())
				.collect();
			assert_eq!(own, [(p("fresh"), 7)], "upstream {upstream}");
		}
	}

	/// Two grants announce one publisher's claim again under two claimants, so the two
	/// copies reach a node as different claims. Once its publisher ends the claim the
	/// node hears of first, the other copy, the same announcer at no newer epoch, is never
	/// shown, nor this node's own taken from it; a newer epoch from that announcer is.
	#[test]
	fn a_copy_of_an_ended_claim_is_never_shown() {
		let mut router = Router::<usize, u64, Run>::new(node(1), debounce(HOP));
		open(&mut router, 2, Role::Routing(node(2)));
		open(&mut router, 4, Role::Routing(node(4)));
		open(&mut router, 5, Role::Scoped(vec![p(ROOT)]));
		router.recv(2, route(2, 2, 1)).unwrap();
		router.recv(4, route(4, 4, 2)).unwrap();
		let copy = |chain: Vec<Id>, epoch: u64| {
			let Change::Live(claim) = chained(chain) else {
				unreachable!("a live claim");
			};
			Change::Live(Claim {
				item: run(epoch),
				..claim
			})
		};
		let broadcast = (p("live"), 7);
		let claims = |origin: Id, change: Change<Run>, floor: Rev| Msg::Claims {
			origin: node(origin),
			changes: vec![(broadcast.clone(), change)],
			floor,
		};
		router.recv(2, claims(2, copy(vec![9, 2], 0), 1)).unwrap();
		router.recv(4, claims(4, copy(vec![9, 4], 0), 1)).unwrap();
		let given = vec![(broadcast.clone(), copy(vec![9, 8], 0))];
		router.recv(5, Msg::Announce { changes: given }).unwrap();
		let shown = |router: &Router<usize, u64, Run>| router.shown().contains_key(&(7, p("live")));
		assert!(shown(&router));

		router.recv(2, claims(2, Change::Ended, 2)).unwrap();
		assert!(!shown(&router), "the copies are of the ended claim");
		assert!(router.own().1.is_empty(), "nor is the node's own taken from one");

		router.recv(4, claims(4, copy(vec![9, 4], 1), 2)).unwrap();
		assert!(shown(&router), "a newer epoch is a new claim");

		// That newer claim ends too: node 4 held a copy of the older end, which says
		// nothing of the newer one, and node 2's copy of it is held back.
		router.recv(2, claims(2, copy(vec![9, 2], 1), 3)).unwrap();
		router.recv(4, claims(4, Change::Ended, 3)).unwrap();
		assert!(!shown(&router), "node 2's copy is of the newer ended claim");
	}

	/// A claimant whose last route goes holds no copy of an ended claim any more, so one
	/// it claims again once it returns, at the same epoch, shows.
	#[test]
	fn a_holder_that_returns_claims_again() {
		let mut router = Router::<usize, u64, Run>::new(node(1), debounce(HOP));
		open(&mut router, 2, Role::Routing(node(2)));
		open(&mut router, 4, Role::Routing(node(4)));
		router.recv(2, route(2, 2, 1)).unwrap();
		router.recv(4, route(4, 4, 2)).unwrap();
		let broadcast = (p("live"), 7);
		let claims = |origin: Id, change: Change<Run>, floor: Rev| Msg::Claims {
			origin: node(origin),
			changes: vec![(broadcast.clone(), change)],
			floor,
		};
		router.recv(2, claims(2, chained(vec![9, 2]), 1)).unwrap();
		router.recv(4, claims(4, chained(vec![9, 4]), 1)).unwrap();
		router.recv(2, claims(2, Change::Ended, 2)).unwrap();
		let shown = |router: &Router<usize, u64, Run>| router.shown().contains_key(&(7, p("live")));
		assert!(!shown(&router), "node 4's copy is of the ended claim");

		// Moving to another announcer's claim and back at the same epoch: no copy any more.
		router.recv(4, claims(4, chained(vec![10, 4]), 2)).unwrap();
		router.recv(4, claims(4, chained(vec![9, 4]), 3)).unwrap();
		assert!(shown(&router), "node 4 let go of its copy");
		router.recv(2, claims(2, chained(vec![9, 2]), 3)).unwrap();
		router.recv(2, claims(2, Change::Ended, 4)).unwrap();
		assert!(!shown(&router), "node 4 holds a copy again");

		router.disconnect(4);
		open(&mut router, 4, Role::Routing(node(4)));
		router.recv(4, route(4, 4, 2)).unwrap();
		router.recv(4, claims(4, chained(vec![9, 4]), 1)).unwrap();
		assert!(shown(&router), "claimed again after it let go");
	}

	/// A claimant that restates its copy of an ended claim, the same announcer at the same
	/// epoch, still holds that copy: only another announcer's claim or a newer epoch lets
	/// it go.
	#[test]
	fn a_holder_restating_its_copy_still_holds_it() {
		let mut router = Router::<usize, u64, Run>::new(node(1), debounce(HOP));
		open(&mut router, 2, Role::Routing(node(2)));
		open(&mut router, 4, Role::Routing(node(4)));
		router.recv(2, route(2, 2, 1)).unwrap();
		router.recv(4, route(4, 4, 2)).unwrap();
		let broadcast = (p("live"), 7);
		let claims = |origin: Id, change: Change<Run>, floor: Rev| Msg::Claims {
			origin: node(origin),
			changes: vec![(broadcast.clone(), change)],
			floor,
		};
		let copy = |chain: Vec<Id>, epoch: u64, cost: Metric| {
			let Change::Live(claim) = chained(chain) else {
				unreachable!("a live claim");
			};
			Change::Live(Claim {
				item: run(epoch),
				cost,
				..claim
			})
		};
		router.recv(2, claims(2, copy(vec![9, 2], 1, 0), 1)).unwrap();
		router.recv(4, claims(4, copy(vec![9, 4], 1, 0), 1)).unwrap();
		router.recv(2, claims(2, Change::Ended, 2)).unwrap();
		let shown = |router: &Router<usize, u64, Run>| router.shown().contains_key(&(7, p("live")));
		assert!(!shown(&router), "node 4's copy is of the ended claim");

		router.recv(4, claims(4, copy(vec![9, 4], 1, 5), 2)).unwrap();
		assert!(!shown(&router), "a restated copy is still the copy");
		router.recv(4, claims(4, copy(vec![9, 4], 0, 5), 3)).unwrap();
		assert!(!shown(&router), "an older epoch is a copy too");

		// A newer epoch lets the copy go, so the ended epoch claimed again after it shows.
		router.recv(4, claims(4, copy(vec![9, 4], 2, 5), 4)).unwrap();
		router.recv(4, claims(4, copy(vec![9, 4], 1, 5), 5)).unwrap();
		assert!(shown(&router), "node 4 let go of its copy");
	}

	/// The same for a copy a scoped neighbour gives: restated, it is still held back, and
	/// another announcer's claim or a newer epoch lets it go.
	#[test]
	fn a_neighbour_restating_its_copy_still_holds_it() {
		let mut router = Router::<usize, u64, Run>::new(node(1), debounce(HOP));
		open(&mut router, 2, Role::Routing(node(2)));
		open(&mut router, 5, Role::Scoped(vec![p(ROOT)]));
		router.recv(2, route(2, 2, 1)).unwrap();
		let broadcast = (p("live"), 7);
		let claims = |change: Change<Run>, floor: Rev| Msg::Claims {
			origin: node(2),
			changes: vec![(broadcast.clone(), change)],
			floor,
		};
		let give = |chain: Vec<Id>, epoch: u64, cost: Metric| {
			let Change::Live(claim) = chained(chain) else {
				unreachable!("a live claim");
			};
			let change = Change::Live(Claim {
				item: run(epoch),
				cost,
				..claim
			});
			Msg::Announce {
				changes: vec![(broadcast.clone(), change)],
			}
		};
		let shown = |router: &Router<usize, u64, Run>| router.shown().contains_key(&(7, p("live")));
		router.recv(2, claims(chained(vec![9, 2]), 1)).unwrap();
		router.recv(5, give(vec![9, 8], 0, 0)).unwrap();
		router.recv(2, claims(Change::Ended, 2)).unwrap();
		assert!(!shown(&router), "the given copy is of the ended claim");

		router.recv(5, give(vec![9, 8], 0, 5)).unwrap();
		assert!(!shown(&router), "a restated copy is still the copy");

		// Another announcer's claim lets the copy go.
		router.recv(5, give(vec![10, 8], 0, 5)).unwrap();
		router.recv(5, give(vec![9, 8], 0, 5)).unwrap();
		assert!(shown(&router), "the neighbour let go of its copy");

		// So does a newer epoch.
		router.recv(2, claims(chained(vec![9, 2]), 3)).unwrap();
		router.recv(2, claims(Change::Ended, 4)).unwrap();
		assert!(!shown(&router), "the neighbour holds a copy again");
		router.recv(5, give(vec![9, 8], 1, 5)).unwrap();
		router.recv(5, give(vec![9, 8], 0, 5)).unwrap();
		assert!(shown(&router), "the neighbour let go of its copy");
	}

	/// A claimant's copy that a newer route's first claims overtake holds no copy any more:
	/// the revisions it skipped can hide an end and the same claim announced again, which
	/// the claims alone cannot tell from no change at all.
	#[test]
	fn a_holder_overtaken_by_a_newer_route_claims_again() {
		let mut router = Router::<usize, u64, Run>::new(node(1), debounce(HOP));
		open(&mut router, 2, Role::Routing(node(2)));
		open(&mut router, 4, Role::Routing(node(4)));
		open(&mut router, 5, Role::Routing(node(5)));
		router.recv(2, route(2, 2, 1)).unwrap();
		router.recv(4, route(4, 4, 2)).unwrap();
		router.recv(5, route(4, 5, 3)).unwrap();
		let broadcast = (p("live"), 7);
		let claims = |origin: Id, change: Change<Run>, floor: Rev| Msg::Claims {
			origin: node(origin),
			changes: vec![(broadcast.clone(), change)],
			floor,
		};
		router.recv(2, claims(2, chained(vec![9, 2]), 1)).unwrap();
		router.recv(4, claims(4, chained(vec![9, 4]), 1)).unwrap();
		router.recv(2, claims(2, Change::Ended, 2)).unwrap();
		let shown = |router: &Router<usize, u64, Run>| router.shown().contains_key(&(7, p("live")));
		assert!(!shown(&router), "node 4's copy is of the ended claim");

		// One skipped revision changes each claim once at most: an unchanged copy is still
		// the one held.
		router.recv(5, claims(4, chained(vec![9, 4]), 2)).unwrap();
		assert!(!shown(&router), "one revision cannot end and claim again");

		// Node 4 ended its copy at 3 and claimed it again at 4; the route through 6 brings 4
		// before the route through 4 brings 3.
		open(&mut router, 6, Role::Routing(node(6)));
		router.recv(6, route(4, 6, 4)).unwrap();
		router.recv(6, claims(4, chained(vec![9, 4]), 4)).unwrap();
		assert!(shown(&router), "node 4 may have claimed it again");
	}

	/// An end reaches only where its own presentation does: a scoped neighbour that names
	/// another announcer, with a prefix above its grant, and ends that claim, holds back no
	/// copy outside the grant.
	#[test]
	fn an_end_reaches_no_copy_outside_its_presentation() {
		let mut router = Router::<usize, u64, Run>::new(node(1), debounce(HOP));
		open(&mut router, 2, Role::Routing(node(2)));
		open(&mut router, 3, Role::Scoped(vec![p("a/x")]));
		router.recv(2, route(2, 2, 1)).unwrap();
		let at = |chain: Vec<Id>, above: u64| {
			let Change::Live(claim) = chained(chain) else {
				unreachable!("a live claim");
			};
			Change::Live(Claim {
				item: Run { epoch: 0, above },
				..claim
			})
		};
		// Announcer 9's claim at `a`, presented at `a/v`.
		let changes = vec![((p("a/v"), 7), at(vec![9, 2], 1))];
		router
			.recv(
				2,
				Msg::Claims {
					origin: node(2),
					changes,
					floor: 1,
				},
			)
			.unwrap();
		assert!(router.shown().contains_key(&(7, p("a/v"))));

		let given = |change| Msg::Announce {
			changes: vec![((p("a/x/y"), 7), change)],
		};
		router.recv(3, given(at(vec![9], 2))).unwrap();
		router.recv(3, given(Change::Ended)).unwrap();
		assert!(router.shown().contains_key(&(7, p("a/v"))), "outside the grant");
	}

	/// A relay that let go of a holder it was overtaken on passes the skipped revisions on
	/// as one: a neighbour whose copy was in step lets go of that holder too, rather than
	/// hold back for good a claim the relay shows.
	#[test]
	fn a_holder_overtaken_upstream_claims_again_downstream() {
		let mut router = Router::<usize, u64, Run>::new(node(1), debounce(HOP));
		open(&mut router, 2, Role::Routing(node(2)));
		open(&mut router, 4, Role::Routing(node(4)));
		router.recv(2, route(2, 2, 1)).unwrap();
		router.recv(4, route(4, 4, 2)).unwrap();
		let live = (p("live"), 7);
		let claims = |origin: Id, changes, floor: Rev| Msg::Claims {
			origin: node(origin),
			changes,
			floor,
		};
		router
			.recv(2, claims(2, vec![(live.clone(), chained(vec![9, 2]))], 1))
			.unwrap();
		router
			.recv(4, claims(4, vec![(live.clone(), chained(vec![9, 4]))], 1))
			.unwrap();
		router
			.recv(2, claims(2, vec![(live.clone(), Change::Ended)], 2))
			.unwrap();
		let shown = |router: &Router<usize, u64, Run>| router.shown().contains_key(&(7, p("live")));
		assert!(!shown(&router), "node 4's copy is of the ended claim");

		// The neighbour skipped node 4's end at 2 and its claim again at 3, and passes on
		// what differs at 4: only another broadcast.
		let other = vec![((p("other"), 7), chained(vec![8, 4]))];
		router.recv(4, claims(4, other, 4)).unwrap();
		assert!(shown(&router), "node 4 may have claimed it again");
	}

	/// Ending an announcer's exact claim leaves its broader claim presented at the same
	/// path: that one is no copy of what ended.
	#[test]
	fn ending_an_exact_claim_leaves_a_broader_one() {
		let mut router = Router::<usize, u64, Run>::new(node(1), debounce(HOP));
		open(&mut router, 2, Role::Routing(node(2)));
		open(&mut router, 4, Role::Routing(node(4)));
		router.recv(2, route(2, 2, 1)).unwrap();
		router.recv(4, route(4, 4, 2)).unwrap();
		let broadcast = (p("live"), 7);
		let claims = |origin: Id, change: Change<Run>, floor: Rev| Msg::Claims {
			origin: node(origin),
			changes: vec![(broadcast.clone(), change)],
			floor,
		};
		let Change::Live(broader) = chained(vec![9, 4]) else {
			unreachable!("a live claim");
		};
		let broader = Claim {
			item: Run { epoch: 0, above: 1 },
			..broader
		};
		router.recv(2, claims(2, chained(vec![9, 2]), 1)).unwrap();
		router.recv(4, claims(4, Change::Live(broader), 1)).unwrap();
		router.recv(2, claims(2, Change::Ended, 2)).unwrap();
		let shown = router.shown();
		let shown = shown.get(&(7, p("live"))).expect("the broader claim shows");
		assert_eq!(shown.item.above, 1);
	}

	/// A copy of an ended claim presented at a deeper path, lowered by as many segments,
	/// is held back there too, while another claim of the announcer at that path is not.
	#[test]
	fn a_copy_presented_below_an_ended_claim_is_never_shown() {
		let mut router = Router::<usize, u64, Run>::new(node(1), debounce(HOP));
		open(&mut router, 2, Role::Routing(node(2)));
		open(&mut router, 4, Role::Routing(node(4)));
		router.recv(2, route(2, 2, 1)).unwrap();
		router.recv(4, route(4, 4, 2)).unwrap();
		let at = |path: &str, origin: Id, above: u64, floor: Rev| {
			let Change::Live(claim) = chained(vec![9, origin]) else {
				unreachable!("a live claim");
			};
			Msg::Claims {
				origin: node(origin),
				changes: vec![(
					(p(path), 7),
					Change::Live(Claim {
						item: Run { epoch: 0, above },
						..claim
					}),
				)],
				floor,
			}
		};
		router.recv(2, at("live", 2, 0, 1)).unwrap();
		router.recv(4, at("live/hd", 4, 1, 1)).unwrap();
		router.recv(4, at("live/hd/a", 4, 0, 2)).unwrap();
		let shown = |router: &Router<usize, u64, Run>, path: &str| router.shown().contains_key(&(7, p(path)));
		assert!(shown(&router, "live/hd"));

		let ended = Msg::Claims {
			origin: node(2),
			changes: vec![((p("live"), 7), Change::Ended)],
			floor: 2,
		};
		router.recv(2, ended).unwrap();
		assert!(!shown(&router, "live/hd"), "node 4's copy is of the ended claim");
		assert!(shown(&router, "live/hd/a"), "node 4's own claim there is no copy");
	}

	/// A reader below an ended claim that shows another claim there, lost in the same
	/// message, shows it unavailable for the debounce rather than ending it, while the
	/// ended claim's copy there stays held back.
	#[test]
	fn a_claim_lost_beside_an_ended_one_waits_out_the_debounce() {
		let mut router = Router::<usize, u64, Run>::new(node(1), debounce(HOP));
		open(&mut router, 2, Role::Routing(node(2)));
		open(&mut router, 4, Role::Routing(node(4)));
		router.recv(2, route(2, 2, 1)).unwrap();
		router.recv(4, route(4, 4, 2)).unwrap();
		let live = |path: &str, chain: Vec<Id>, above: u64| {
			let Change::Live(claim) = chained(chain) else {
				unreachable!("a live claim");
			};
			let item = Run { epoch: 0, above };
			((p(path), 7), Change::Live(Claim { item, ..claim }))
		};
		let claims = |origin: Id, changes, floor: Rev| Msg::Claims {
			origin: node(origin),
			changes,
			floor,
		};
		let changes = vec![live("live", vec![9, 2], 0), live("live/hd", vec![8, 2], 0)];
		router.recv(2, claims(2, changes, 1)).unwrap();
		router
			.recv(4, claims(4, vec![live("live/hd", vec![9, 4], 1)], 1))
			.unwrap();
		let shown = |router: &Router<usize, u64, Run>| router.shown().get(&(7, p("live/hd"))).cloned();
		assert_eq!(shown(&router).map(|shown| shown.item.above), Some(0));

		let changes = vec![((p("live"), 7), Change::Ended), ((p("live/hd"), 7), Change::Lost)];
		router.recv(2, claims(2, changes, 2)).unwrap();
		let shown = shown(&router).expect("the lost claim shows for the debounce");
		assert_eq!((shown.item.above, shown.available), (0, false));
	}

	/// As [`Self::a_claim_lost_beside_an_ended_one_waits_out_the_debounce`], with the lost
	/// claim another announcer's at the copy's own distance: still lost, not ended.
	#[test]
	fn a_claim_lost_beside_an_ended_one_at_the_same_distance_waits_out_the_debounce() {
		let mut router = Router::<usize, u64, Run>::new(node(1), debounce(HOP));
		open(&mut router, 2, Role::Routing(node(2)));
		open(&mut router, 4, Role::Routing(node(4)));
		router.recv(2, route(2, 2, 1)).unwrap();
		router.recv(4, route(4, 4, 2)).unwrap();
		let live = |path: &str, chain: Vec<Id>, above: u64| {
			let Change::Live(claim) = chained(chain) else {
				unreachable!("a live claim");
			};
			let item = Run { epoch: 0, above };
			((p(path), 7), Change::Live(Claim { item, ..claim }))
		};
		let claims = |origin: Id, changes, floor: Rev| Msg::Claims {
			origin: node(origin),
			changes,
			floor,
		};
		let changes = vec![live("live", vec![9, 2], 0), live("live/hd", vec![8, 2], 1)];
		router.recv(2, claims(2, changes, 1)).unwrap();
		router
			.recv(4, claims(4, vec![live("live/hd", vec![9, 4], 1)], 1))
			.unwrap();
		assert!(router.shown().contains_key(&(7, p("live/hd"))));

		let changes = vec![((p("live"), 7), Change::Ended), ((p("live/hd"), 7), Change::Lost)];
		router.recv(2, claims(2, changes, 2)).unwrap();
		let shown = router.shown().get(&(7, p("live/hd"))).cloned();
		let shown = shown.expect("the lost claim shows for the debounce");
		assert!(!shown.available);
	}

	/// A copy of an ended claim that the same message loses ends with it: the publisher's
	/// end covers every copy.
	#[test]
	fn a_copy_lost_beside_its_ended_claim_ends() {
		let mut router = Router::<usize, u64, Run>::new(node(1), debounce(HOP));
		open(&mut router, 2, Role::Routing(node(2)));
		open(&mut router, 4, Role::Routing(node(4)));
		router.recv(2, route(2, 2, 1)).unwrap();
		router.recv(4, route(4, 4, 2)).unwrap();
		let live = |path: &str, chain: Vec<Id>, above: u64| {
			let Change::Live(claim) = chained(chain) else {
				unreachable!("a live claim");
			};
			let item = Run { epoch: 0, above };
			((p(path), 7), Change::Live(Claim { item, ..claim }))
		};
		let claims = |origin: Id, changes, floor: Rev| Msg::Claims {
			origin: node(origin),
			changes,
			floor,
		};
		let changes = vec![live("live", vec![9, 2], 0), live("live/hd", vec![9, 2], 1)];
		router.recv(2, claims(2, changes, 1)).unwrap();
		router
			.recv(4, claims(4, vec![live("live/hd", vec![9, 4], 1)], 1))
			.unwrap();
		assert!(router.shown().contains_key(&(7, p("live/hd"))));

		let changes = vec![((p("live"), 7), Change::Ended), ((p("live/hd"), 7), Change::Lost)];
		router.recv(2, claims(2, changes, 2)).unwrap();
		assert!(!router.shown().contains_key(&(7, p("live/hd"))), "ended, not lost");
	}

	/// As [`Self::a_copy_lost_beside_its_ended_claim_ends`], with no other holder of the
	/// copy: nothing is left to hold back, and the reader still ends.
	#[test]
	fn a_copy_lost_beside_its_ended_claim_ends_with_no_other_holder() {
		let mut router = Router::<usize, u64, Run>::new(node(1), debounce(HOP));
		open(&mut router, 2, Role::Routing(node(2)));
		router.recv(2, route(2, 2, 1)).unwrap();
		let live = |path: &str, above: u64| {
			let Change::Live(claim) = chained(vec![9, 2]) else {
				unreachable!("a live claim");
			};
			let item = Run { epoch: 0, above };
			((p(path), 7), Change::Live(Claim { item, ..claim }))
		};
		let claims = |changes, floor: Rev| Msg::Claims {
			origin: node(2),
			changes,
			floor,
		};
		router
			.recv(2, claims(vec![live("live", 0), live("live/hd", 1)], 1))
			.unwrap();
		assert!(router.shown().contains_key(&(7, p("live/hd"))));

		let changes = vec![((p("live"), 7), Change::Ended), ((p("live/hd"), 7), Change::Lost)];
		router.recv(2, claims(changes, 2)).unwrap();
		assert!(!router.shown().contains_key(&(7, p("live/hd"))), "ended, not lost");
	}

	/// A copy the ending message loses ends only its own path: another announcer's claim lost
	/// earlier under the ended one still waits out its debounce.
	#[test]
	fn a_lost_copy_beside_an_ended_claim_ends_no_other_path() {
		let mut router = Router::<usize, u64, Run>::new(node(1), debounce(HOP));
		open(&mut router, 2, Role::Routing(node(2)));
		open(&mut router, 4, Role::Routing(node(4)));
		router.recv(2, route(2, 2, 1)).unwrap();
		router.recv(4, route(4, 4, 2)).unwrap();
		let live = |path: &str, instance: u64, chain: Vec<Id>, above: u64| {
			let Change::Live(claim) = chained(chain) else {
				unreachable!("a live claim");
			};
			let item = Run { epoch: 0, above };
			((p(path), instance), Change::Live(Claim { item, ..claim }))
		};
		let claims = |origin: Id, changes, floor: Rev| Msg::Claims {
			origin: node(origin),
			changes,
			floor,
		};
		let changes = vec![live("live", 7, vec![9, 2], 0), live("live/sd", 7, vec![9, 2], 1)];
		router.recv(2, claims(2, changes, 1)).unwrap();
		// Another announcer's claim, and another instance that keeps its path claimed.
		let changes = vec![live("live/hd", 7, vec![10, 4], 0), live("live/hd", 8, vec![10, 4], 0)];
		router.recv(4, claims(4, changes, 1)).unwrap();
		router
			.recv(4, claims(4, vec![((p("live/hd"), 7), Change::Lost)], 2))
			.unwrap();
		let hd = (7, p("live/hd"));
		assert!(!router.shown()[&hd].available, "lost, waiting out the debounce");

		let changes = vec![((p("live"), 7), Change::Ended), ((p("live/sd"), 7), Change::Lost)];
		router.recv(2, claims(2, changes, 2)).unwrap();
		assert!(!router.shown().contains_key(&(7, p("live/sd"))), "ended, not lost");
		assert!(router.shown().contains_key(&hd), "still waiting out the debounce");
	}

	/// A publisher that withdraws its claim and announces it again at the same epoch, as
	/// `origin::Route::withdraw` and `announce` do, is shown at once: only the copies that
	/// stood when the end arrived are held back.
	#[test]
	fn a_claim_announced_again_at_the_same_epoch_shows_at_once() {
		let mut router = Router::<usize, u64, Run>::new(node(1), debounce(HOP));
		open(&mut router, 2, Role::Routing(node(2)));
		open(&mut router, 3, Role::Scoped(vec![p(ROOT)]));
		router.recv(2, route(2, 2, 1)).unwrap();
		let broadcast = (p("live"), 7);
		let given = |change: Change<Run>| Msg::Announce {
			changes: vec![(broadcast.clone(), change)],
		};
		let claims = |change: Change<Run>, floor: Rev| Msg::Claims {
			origin: node(2),
			changes: vec![(broadcast.clone(), change)],
			floor,
		};
		router.recv(3, given(chained(vec![9]))).unwrap();
		router.recv(2, claims(chained(vec![9, 2]), 1)).unwrap();
		let own = |router: &Router<usize, u64, Run>| router.own().1.contains_key(&broadcast);
		assert!(own(&router));

		router.recv(3, given(Change::Ended)).unwrap();
		assert!(
			!router.shown().contains_key(&(7, p("live"))),
			"node 2's copy is of the ended claim"
		);
		router.recv(3, given(chained(vec![9]))).unwrap();
		assert!(own(&router), "announced again, the claim is no copy");
		router.recv(2, claims(Change::Ended, 2)).unwrap();
		assert!(own(&router));
	}

	/// A relay never tells a claim whose chain its own Relay would carry to the longest
	/// path, which no node could send.
	#[test]
	fn a_chain_is_never_told_past_the_longest_path() {
		let mut router = Router::<usize, u64, Run>::new(node(1), debounce(HOP));
		open(&mut router, 2, Role::Routing(node(2)));
		open(&mut router, 3, Role::Scoped(vec![p(ROOT)]));
		router.recv(2, route(2, 2, 1)).unwrap();
		let changes = [("short", MAX_HOPS - 2), ("long", MAX_HOPS - 1)]
			.map(|(path, len)| ((p(path), 7), chained((100..100 + len as Id - 1).chain([2]).collect())))
			.to_vec();
		router
			.recv(
				2,
				Msg::Claims {
					origin: node(2),
					changes,
					floor: 1,
				},
			)
			.unwrap();
		router.drain();
		let edit = Edit {
			remove: Vec::new(),
			add: vec![p(ROOT)],
		};
		router.edit(3, edit).unwrap();
		let told = chains(router.drain(), 3);
		assert_eq!(told.keys().cloned().collect::<Vec<_>>(), [(p("short"), 7)]);
		assert_eq!(told[&(p("short"), 7)].len(), MAX_HOPS - 1);
	}

	/// An endpoint names itself in no chain, so it takes and tells a claim one hop longer
	/// than a relay does: from a scoped neighbour, and on to one.
	#[test]
	fn an_endpoint_names_itself_in_no_chain() {
		let mut router = Router::<usize, u64, Run>::new(node(1), debounce(HOP));
		router.endpoint();
		open(&mut router, 4, Role::Scoped(vec![p(ROOT)]));
		let longest: Vec<Id> = (100..100 + MAX_HOPS as Id - 1).collect();
		let changes = vec![((p("taken"), 7), chained(longest.clone()))];
		router.recv(4, Msg::Announce { changes }).unwrap();
		let own = router.own().1;
		assert!(
			matches!(own.get(&(p("taken"), 7)), Some(claim) if claim.chain == longest),
			"{own:?}"
		);

		let mut router = Router::<usize, u64, Run>::new(node(1), debounce(HOP));
		router.endpoint();
		open(&mut router, 2, Role::Routing(node(2)));
		open(&mut router, 3, Role::Scoped(vec![p(ROOT)]));
		router.recv(2, route(2, 2, 1)).unwrap();
		let long: Vec<Id> = (100..100 + MAX_HOPS as Id - 2).chain([2]).collect();
		let changes = vec![((p("long"), 7), chained(long.clone()))];
		router
			.recv(
				2,
				Msg::Claims {
					origin: node(2),
					changes,
					floor: 1,
				},
			)
			.unwrap();
		router.drain();
		let edit = Edit {
			remove: Vec::new(),
			add: vec![p(ROOT)],
		};
		router.edit(3, edit).unwrap();
		let told = chains(router.drain(), 3);
		assert_eq!(told.get(&(p("long"), 7)), Some(&long));
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
	}

	/// A neighbour asking a prefix under a broader claim is told it there, lowered by the
	/// segments between the two.
	#[test]
	fn a_broader_claim_is_told_lowered_to_the_prefix_asked() {
		let mut router = Router::<usize, u64, Run>::new(node(1), debounce(HOP));
		open(&mut router, 2, Role::Endpoint);
		let announce = Msg::Announce {
			changes: vec![((p("a"), 7), claim(run(1), 0))],
		};
		router.recv(2, announce).unwrap();
		open(&mut router, 3, Role::Endpoint);
		router.drain();
		let edit = Edit {
			remove: Vec::new(),
			add: vec![p("a/b/c")],
		};
		router.edit(3, edit).unwrap();
		let told: Vec<_> = (router.drain().into_iter())
			.filter(|(to, _)| *to == 3)
			.filter_map(|(_, msg)| match msg {
				Msg::Announce { changes } => Some(changes),
				_ => None,
			})
			.flatten()
			.collect();
		assert!(
			matches!(told.as_slice(), [((path, 7), Change::Live(claim))] if *path == p("a/b/c") && claim.item == Run { epoch: 1, above: 2 }),
			"{told:?}"
		);
	}

	/// A prefix nested in one already asked for, above or below it, is refused, whichever
	/// side asks.
	#[test]
	fn a_nested_prefix_is_refused() {
		let mut router = relay(1);
		router.connect(2, port(Role::Scoped(Vec::new()))).unwrap();
		let add = |path: &str| Edit {
			remove: Vec::new(),
			add: vec![p(path)],
		};
		router.edit(2, add("a/b")).unwrap();
		assert_eq!(router.edit(2, add("a")), Err(Error::Nested), "above");
		assert_eq!(router.edit(2, add("a/b/c")), Err(Error::Nested), "below");
		router.edit(2, add("a/c")).unwrap();
		router.ask(2, add("x/y")).unwrap();
		assert_eq!(router.ask(2, add("x")), Err(Error::Nested), "above");
		assert_eq!(router.ask(2, add("x/y/z")), Err(Error::Nested), "below");
		router.ask(2, add("x/z")).unwrap();
	}

	/// A prefix this node removes and adds again in one edit is asked for afresh once
	/// answered: what it covered goes, and the peer's SYNCED for it is expected again.
	#[test]
	fn a_prefix_asked_again_in_one_edit_is_asked_afresh() {
		let mut router = relay(1);
		router.connect(2, port(Role::Scoped(Vec::new()))).unwrap();
		let room = (p("live/room"), 7);
		router
			.ask(
				2,
				Edit {
					remove: Vec::new(),
					add: vec![p("live")],
				},
			)
			.unwrap();
		router.recv(2, Msg::Edited).unwrap();
		let announce = Msg::Announce {
			changes: vec![(room.clone(), live(0))],
		};
		router.recv(2, announce.clone()).unwrap();
		router.recv(2, Msg::Synced(p("live"))).unwrap();
		assert_eq!(router.is_synced(2, &p("live")), Some(true));
		assert!(router.peers[&2].given.contains_key(&room));

		router
			.ask(
				2,
				Edit {
					remove: vec![p("live")],
					add: vec![p("live")],
				},
			)
			.unwrap();
		router.recv(2, Msg::Edited).unwrap();
		assert_eq!(router.is_synced(2, &p("live")), Some(false));
		assert!(!router.peers[&2].given.contains_key(&room), "the peer dropped it too");
		router.recv(2, announce).unwrap();
		router.recv(2, Msg::Synced(p("live"))).unwrap();
		assert_eq!(router.is_synced(2, &p("live")), Some(true));
		assert!(router.peers[&2].given.contains_key(&room));
	}

	/// A request from a marked neighbour never follows a claim that reached this node
	/// over a marked link, which that neighbour is never told; another neighbour's may.
	#[test]
	fn a_marked_request_never_follows_a_marked_claim() {
		let mut router = relay(1);
		let marked = |role| Port {
			upstream: true,
			..port(role)
		};
		router.connect(2, marked(Role::Endpoint)).unwrap();
		router.recv(2, Msg::Edited).unwrap();
		router.connect(3, marked(Role::Endpoint)).unwrap();
		open(&mut router, 4, Role::Endpoint);
		let room = (p("room"), 7);
		let announce = Msg::Announce {
			changes: vec![(room.clone(), live(0))],
		};
		router.recv(2, announce).unwrap();
		// A claim taken over a marked link is filed under the marked identity.
		let marked_me = marked_id(router.me());
		assert!(!router.admits(3, marked_me, &room));
		assert!(router.admits(4, marked_me, &room));
	}

	/// What a neighbour holds of what this node sent it: each route with its origin's
	/// claims, and each claim told.
	#[test]
	fn a_neighbour_holds_each_route_and_claim_sent() {
		let mut router = relay(1);
		open(&mut router, 2, Role::Endpoint);
		let changes = ["a", "b", "c"].map(|path| ((p(path), 7), live(0)));
		router
			.recv(
				2,
				Msg::Announce {
					changes: changes.into(),
				},
			)
			.unwrap();
		open(&mut router, 3, Role::Endpoint);
		let edit = Edit {
			remove: Vec::new(),
			add: vec![p(ROOT)],
		};
		router.edit(3, edit).unwrap();
		open(&mut router, 4, Role::Routing(node(4)));
		router.drain();
		assert_eq!(router.held(3), 3, "three claims told");
		assert_eq!(router.held(4), 4, "its route to this node, with three claims");
		assert_eq!(router.held(5), 0, "no such neighbour");
	}

	/// A lost claim waiting out the debounce, or one no route reaches, is shown unavailable
	/// to a neighbour that asks, even one unmarked when its source was marked, but never
	/// back to its source, marked or not.
	#[test]
	fn an_unavailable_claim_is_shown_to_every_asker_but_its_source() {
		let mut router = relay(1);
		let marked = Port {
			upstream: true,
			..port(Role::Endpoint)
		};
		router.connect(2, marked).unwrap();
		router.recv(2, Msg::Edited).unwrap();
		open(&mut router, 3, Role::Endpoint);
		let all = || Edit {
			remove: Vec::new(),
			add: vec![p(ROOT)],
		};
		router.edit(2, all()).unwrap();
		router.edit(3, all()).unwrap();
		let announce = |path: &str, change| Msg::Announce {
			changes: vec![((p(path), 7), change)],
		};
		router.recv(2, announce("room", live(0))).unwrap();
		router.recv(3, announce("hall", live(0))).unwrap();
		router.recv(3, announce("yard", live(0))).unwrap();
		router.drain();
		router.recv(2, announce("room", Change::Lost)).unwrap();
		router.recv(3, announce("hall", Change::Lost)).unwrap();
		let told = |to: usize, out: &[(usize, Msg<u64, u64>)]| -> Vec<Change<u64>> {
			(out.iter())
				.filter(|(peer, _)| *peer == to)
				.filter_map(|(_, msg)| match msg {
					Msg::Announce { changes } => Some(changes.clone()),
					_ => None,
				})
				.flatten()
				.map(|(_, change)| change)
				.collect()
		};
		let out = router.drain();
		assert!(
			matches!(told(2, &out).as_slice(), [Change::Live(claim)] if claim.unavailable),
			"hall, to a marked neighbour: {out:?}"
		);
		assert!(
			matches!(told(3, &out).as_slice(), [Change::Live(claim)] if claim.unavailable),
			"room, to an unmarked neighbour: {out:?}"
		);
		// So is a claim its source tells unavailable, which no route reaches.
		let Change::Live(unavailable) = live(0) else {
			unreachable!("a live claim");
		};
		let unavailable = Change::Live(Claim {
			unavailable: true,
			..unavailable
		});
		router.recv(3, announce("yard", unavailable)).unwrap();
		let out = router.drain();
		assert!(told(3, &out).is_empty(), "{out:?}");
		assert!(
			matches!(told(2, &out).as_slice(), [Change::Live(claim)] if claim.unavailable),
			"{out:?}"
		);
	}

	/// A message outside the sender's role is refused, and so is one a routing neighbour
	/// sends before it answered what this node asked.
	#[test]
	fn a_message_outside_its_role_is_refused() {
		let mut router = Router::<usize, u64, Run>::new(node(1), debounce(HOP));
		router.connect(2, port(Role::Routing(node(2)))).unwrap();
		assert_eq!(router.recv(2, route(5, 2, 1)), Err(Error::Unasked));
		router.recv(2, Msg::Edited).unwrap();
		let announce = Msg::Announce {
			changes: vec![((p("room"), 7), claim(run(0), 0))],
		};
		assert_eq!(router.recv(2, announce), Err(Error::Role));
		open(&mut router, 3, Role::Endpoint);
		let claims = Msg::Claims {
			origin: node(5),
			changes: Vec::new(),
			floor: 1,
		};
		let dead = Msg::Dead {
			hop: Hop {
				node: node(3),
				session: 1,
			},
			far: node(5),
		};
		for msg in [route(5, 3, 1), claims, dead, Msg::Drain { node: node(5) }] {
			assert_eq!(router.recv(3, msg.clone()), Err(Error::Role), "{msg:?}");
		}
	}

	/// A routing neighbour that stops asking holds nothing this node sent it, and is sent
	/// nothing more.
	#[test]
	fn a_routing_neighbour_that_stops_asking_holds_nothing() {
		let mut router = relay(1);
		open(&mut router, 2, Role::Endpoint);
		let announce = |path: &str| Msg::Announce {
			changes: vec![((p(path), 7), live(0))],
		};
		router.recv(2, announce("room")).unwrap();
		open(&mut router, 3, Role::Routing(node(3)));
		router.drain();
		assert_eq!(router.held(3), 2, "this node's route, with its claim");
		let edit = Edit {
			remove: vec![p(ROOT)],
			add: Vec::new(),
		};
		router.edit(3, edit).unwrap();
		assert_eq!(router.held(3), 0);
		router.drain();
		router.recv(2, announce("hall")).unwrap();
		let out = router.drain();
		assert!(out.iter().all(|(to, _)| *to != 3), "{out:?}");
	}

	/// A claim chain as long as the longest path is malformed: the rule never builds one.
	#[test]
	fn a_chain_at_the_longest_path_is_malformed() {
		let mut router = relay(1);
		open(&mut router, 2, Role::Endpoint);
		let announce = |path: &str, hops: usize| {
			let Change::Live(claim) = live(0) else {
				unreachable!("a live claim");
			};
			let chain = (10..).take(hops).collect();
			Msg::Announce {
				changes: vec![((p(path), 7), Change::Live(Claim { chain, ..claim }))],
			}
		};
		assert_eq!(router.recv(2, announce("long", MAX_HOPS)), Err(Error::Malformed));
		router.recv(2, announce("short", MAX_HOPS - 1)).unwrap();
	}

	/// A copy that lagged and then overtakes another tells what it lacks as lost, unless
	/// its message ended it, which shows at once.
	#[test]
	fn a_lagging_copy_ends_only_what_its_message_ends() {
		use super::super::Kind;
		let mut router = Router::<usize, u64, Run>::new(node(1), debounce(HOP));
		open(&mut router, 2, Role::Routing(node(2)));
		open(&mut router, 3, Role::Routing(node(3)));
		router.recv(2, route(5, 2, 1)).unwrap();
		router.recv(3, route(5, 3, 2)).unwrap();
		let changes = ["a", "b"].map(|path| ((p(path), 7), claim(run(0), 0)));
		let claims = Msg::Claims {
			origin: node(5),
			changes: changes.into(),
			floor: 1,
		};
		router.recv(2, claims).unwrap();
		router.events();
		let claims = Msg::Claims {
			origin: node(5),
			changes: vec![((p("b"), 7), Change::Ended)],
			floor: 2,
		};
		router.recv(3, claims).unwrap();
		let kinds: Vec<_> = (router.events().into_iter())
			.map(|event| (event.claim.to_string(), event.kind))
			.collect();
		assert_eq!(
			kinds,
			[("a".to_string(), Kind::Unavailable), ("b".to_string(), Kind::End)]
		);
	}

	/// A lost claim goes offline when its debounce fires, never earlier because a later
	/// input touched it after its deadline passed.
	#[test]
	fn a_lost_claim_goes_offline_only_when_its_timer_fires() {
		use super::super::Kind;
		let mut router = Router::<usize, u64, Run>::new(node(1), debounce(HOP));
		open(&mut router, 2, Role::Endpoint);
		open(&mut router, 3, Role::Routing(node(3)));
		router.recv(3, route(5, 3, 1)).unwrap();
		let room = (p("room"), 7);
		let announce = |change| Msg::Announce {
			changes: vec![(room.clone(), change)],
		};
		router.recv(2, announce(claim(run(0), 0))).unwrap();
		router.recv(2, announce(Change::Lost)).unwrap();
		router.events();
		let due = router.deadline().expect("a debounce");
		router.clock(due);
		let claims = Msg::Claims {
			origin: node(5),
			changes: vec![(room.clone(), Change::Lost)],
			floor: 1,
		};
		router.recv(3, claims).unwrap();
		assert!(router.events().is_empty());
		router.timeout(due);
		let kinds: Vec<_> = router.events().into_iter().map(|event| event.kind).collect();
		assert_eq!(kinds, [Kind::Offline]);
	}

	/// A neighbour is shown, while a lost broadcast waits out its debounce, the claim the
	/// reader showed, even when a claim of lower precedence was lost with it.
	#[test]
	fn a_lost_broadcast_shows_the_claim_the_reader_showed() {
		let mut router = Router::<usize, u64, Run>::new(node(1), debounce(HOP));
		open(&mut router, 2, Role::Routing(node(2)));
		router.recv(2, route(5, 2, 1)).unwrap();
		router.recv(2, route(6, 2, 1)).unwrap();
		let room = (p("room"), 7);
		let claims = |origin, epoch| Msg::Claims {
			origin: node(origin),
			changes: vec![(room.clone(), claim(run(epoch), 0))],
			floor: 1,
		};
		router.recv(2, claims(5, 1)).unwrap();
		router.recv(2, claims(6, 0)).unwrap();
		open(&mut router, 3, Role::Endpoint);
		let edit = Edit {
			remove: Vec::new(),
			add: vec![p(ROOT)],
		};
		router.edit(3, edit).unwrap();
		router.drain();
		router.disconnect(2);
		let told: Vec<_> = (router.drain().into_iter())
			.filter(|(to, _)| *to == 3)
			.filter_map(|(_, msg)| match msg {
				Msg::Announce { changes } => Some(changes),
				_ => None,
			})
			.flatten()
			.collect();
		assert!(
			matches!(told.as_slice(), [(_, Change::Live(claim))] if claim.unavailable && claim.item == run(1)),
			"{told:?}"
		);
	}

	/// A claim lost while it waits out its debounce keeps the mark of the route it came on:
	/// a marked neighbour is never shown it unavailable when it came over a marked link,
	/// and is shown it when it came over a plain one.
	#[test]
	fn a_lost_claim_keeps_the_mark_of_its_route() {
		for marked in [false, true] {
			let mut router = Router::<usize, u64, Run>::new(node(1), debounce(HOP));
			let port = |role, upstream| Port { upstream, ..port(role) };
			router.connect(2, port(Role::Routing(node(2)), marked)).unwrap();
			router.recv(2, Msg::Edited).unwrap();
			router.recv(2, route(5, 2, 1)).unwrap();
			let claims = Msg::Claims {
				origin: node(5),
				changes: vec![((p("room"), 7), claim(run(1), 0))],
				floor: 1,
			};
			router.recv(2, claims).unwrap();
			let ask = |router: &mut Router<usize, u64, Run>, peer| {
				router.recv(peer, Msg::Edited).unwrap();
				let edit = Edit {
					remove: Vec::new(),
					add: vec![p(ROOT)],
				};
				router.edit(peer, edit).unwrap();
			};
			router.connect(3, port(Role::Endpoint, false)).unwrap();
			ask(&mut router, 3);
			router.connect(4, port(Role::Scoped(vec![p(ROOT)]), true)).unwrap();
			ask(&mut router, 4);
			router.drain();

			router.disconnect(2);
			let told = |out: &[(usize, Msg<u64, Run>)], peer| {
				(out.iter())
					.filter(|(to, _)| *to == peer)
					.filter_map(|(_, msg)| match msg {
						Msg::Announce { changes } => Some(changes.clone()),
						_ => None,
					})
					.flatten()
					.collect::<Vec<_>>()
			};
			let out = router.drain();
			let unavailable = |told: &[(Broadcast<u64>, Change<Run>)]| matches!(told, [(_, Change::Live(claim))] if claim.unavailable);
			assert!(unavailable(&told(&out, 3)), "marked={marked}: {out:?}");
			assert_eq!(unavailable(&told(&out, 4)), !marked, "marked={marked}: {out:?}");
		}
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
			node: NodeId {
				relay: 1,
				marked: false,
			},
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
		let dead = Msg::Dead {
			hop: sender,
			far: node(5),
		};
		router.recv(2, dead.clone()).unwrap();
		// It ended the route: no live one crosses the hop or names the node now.
		assert_eq!(router.recv(2, dead), Err(Error::Unrouted));
		assert_eq!(router.recv(2, Msg::Drain { node: node(5) }), Err(Error::Unrouted));
	}

	/// Relays exchanging messages until none is left. Each keys a neighbour as
	/// `10 * relay + link`, so two relays may hold more than one session.
	struct Mesh(BTreeMap<Id, Relay>);

	impl Mesh {
		fn new(relays: &[Id]) -> Self {
			Self(relays.iter().map(|&id| (id, relay(id))).collect())
		}

		fn link(&mut self, a: Id, b: Id, link: usize) {
			for (x, y) in [(a, b), (b, a)] {
				let (key, router) = (10 * y as usize + link, self.0.get_mut(&x).unwrap());
				router.connect(key, port(Role::Routing(node(y)))).unwrap();
			}
			self.settle();
		}

		fn close(&mut self, a: Id, b: Id, link: usize) {
			for (x, y) in [(a, b), (b, a)] {
				self.0.get_mut(&x).unwrap().disconnect(10 * y as usize + link);
			}
			self.settle();
		}

		/// An endpoint at `relay` claims `live`, so the relay offers a route to itself.
		fn claim(&mut self, relay: Id) {
			let router = self.0.get_mut(&relay).unwrap();
			open(router, 0, Role::Endpoint);
			let changes = vec![((p("live"), 7), live(0))];
			router.recv(0, Msg::Announce { changes }).unwrap();
			self.settle();
		}

		fn settle(&mut self) {
			loop {
				let mut sent = Vec::new();
				for (&id, router) in &mut self.0 {
					for (key, msg) in router.drain() {
						sent.push((key / 10, 10 * id as usize + key % 10, msg));
					}
				}
				let mut delivered = false;
				for (to, from, msg) in sent {
					if let Some(router) = self.0.get_mut(&(to as Id)) {
						router.recv(from, msg).unwrap();
						delivered = true;
					}
				}
				if !delivered {
					return;
				}
			}
		}

		fn routes(&self, at: Id, to: Id) -> bool {
			self.0[&at].best(node(to)).is_some()
		}
	}

	/// Two sessions between the same pair of relays: whichever closes first, even the one
	/// the route crosses, a relay that reaches the far one only through them keeps its
	/// route.
	#[test]
	fn a_second_session_keeps_the_route() {
		for (older, used) in [(1, 1), (1, 2), (2, 1), (2, 2)] {
			let mut mesh = Mesh::new(&[1, 2, 3]);
			mesh.link(1, 2, older);
			mesh.link(1, 2, 3 - older);
			mesh.link(1, 3, 1);
			mesh.claim(2);
			assert!(mesh.routes(3, 2), "routed before any close");
			// Close the link the route crosses, or the other one, first.
			let via = mesh.0[&1].best(node(2)).expect("a route").0 % 10;
			let first = if used == 1 { via } else { 3 - via };
			mesh.close(1, 2, first);
			assert!(mesh.routes(3, 2), "closing link {first} first keeps the route");
			mesh.close(1, 2, 3 - first);
			assert!(!mesh.routes(3, 2), "both closed");
		}
	}

	/// A routing neighbour naming made-up relays on its routes, and each of their sessions
	/// closed, grows the floors only by what its live routes hold, and they go with it.
	#[test]
	fn a_peer_naming_made_up_relays_leaves_no_floors() {
		let mut router = relay(1);
		open(&mut router, 2, Role::Routing(node(2)));
		for made_up in 100..300 {
			let through = Hop {
				node: node(made_up),
				session: 1,
			};
			let path = vec![
				Hop {
					node: node(5),
					session: 0,
				},
				through,
				Hop {
					node: node(2),
					session: 1,
				},
			];
			let offer = Offer {
				path: path.into(),
				draining: false,
				priority: i8::MAX,
				cost: 2,
				marked: false,
			};
			let route = Msg::Route {
				origin: node(5),
				offer: Some(offer),
			};
			router.recv(2, route).unwrap();
			let dead = Msg::Dead {
				hop: through,
				far: node(5),
			};
			router.recv(2, dead).unwrap();
			// The pairs one route joins, and as many it stopped joining.
			assert!(router.kept().closed <= 4, "{:?}", router.kept());
		}
		router.disconnect(2);
		assert_eq!(router.kept().closed, 0);
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
		assert_eq!(router.step(node(1), &request, None), Some(Next::Local(2)));
		let since = Instant::ZERO;
		router.recv(2, claim(unavailable.clone())).unwrap();
		assert_eq!(kinds(&mut router), [Kind::Start, Kind::Unavailable]);
		assert_eq!(router.step(node(1), &request, None), None, "fails closed");
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
		assert_eq!(router.step(me, &broadcast, None), Some(Next::Local(2)));

		// Two neighbours give the run; the one still available serves.
		router.recv(3, told(1, false)).unwrap();
		router.recv(2, told(1, true)).unwrap();
		assert_eq!(own(&router), Some((1, false)));
		assert_eq!(router.step(me, &broadcast, None), Some(Next::Local(3)));
		router.recv(3, told(1, true)).unwrap();
		assert_eq!(own(&router), Some((1, true)), "every one unavailable");
		assert_eq!(router.step(me, &broadcast, None), None, "fails closed");
		assert_eq!(router.pick(&broadcast), None, "fails closed");
		router.recv(2, told(1, false)).unwrap();
		assert_eq!(router.step(me, &broadcast, None), Some(Next::Local(2)));
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
			router.step(me, &broadcast, None),
			Some(Next::Local(3)),
			"where it is still given"
		);
		router.recv(4, told(2, true)).unwrap();
		assert_eq!(own(&router), Some((1, false)), "the older run stands");
		assert_eq!(router.step(me, &broadcast, None), Some(Next::Local(3)));
		router.recv(4, told(2, false)).unwrap();
		assert_eq!(own(&router), Some((2, false)), "taken, the newer run outranks");
		assert_eq!(router.step(me, &broadcast, None), Some(Next::Local(4)));
		router.recv(4, told(2, true)).unwrap();
		assert_eq!(own(&router), Some((2, true)), "unavailable or not");
		assert_eq!(router.step(me, &broadcast, None), None);
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
		// Removing a prefix and adding it again in one edit, as a peer changing what it
		// asks of the prefix does, tells what it covers again: the peer dropped it.
		router
			.edit(
				3,
				Edit {
					remove: vec![p("live/a")],
					add: vec![p("live/a")],
				},
			)
			.unwrap();
		let out = router.drain();
		let told: Vec<&Msg<u64, u64>> = (out.iter()).filter(|(to, _)| *to == 3).map(|(_, msg)| msg).collect();
		assert_eq!(told.first(), Some(&&Msg::Edited));
		assert!(
			told.iter().any(|msg| matches!(msg, Msg::Announce { changes } if changes.iter().any(|((at, id), change)| *at == p("live/a/x") && *id == 8 && matches!(change, Change::Live(_))))),
			"the claim inside is told again: {told:?}"
		);
		assert_eq!(told.last(), Some(&&Msg::Synced(p("live/a"))));
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

	/// A claim moving to an identity that never announced reaches a routing neighbour
	/// under the new identity before it leaves the old one.
	#[test]
	fn a_claim_moving_identity_is_routed_before_it_leaves() {
		let mut router = relay(1);
		router
			.connect(
				2,
				Port {
					upstream: true,
					..port(Role::Endpoint)
				},
			)
			.unwrap();
		router.recv(2, Msg::Edited).unwrap();
		open(&mut router, 3, Role::Routing(node(3)));
		open(&mut router, 5, Role::Endpoint);
		router.drain();

		let broadcast = (p("live"), 7);
		let give = |change| Msg::Announce {
			changes: vec![(broadcast.clone(), change)],
		};
		router.recv(5, give(live(0))).unwrap();
		router.recv(2, give(live(0))).unwrap();
		router.drain();
		assert!(
			router.own().1.contains_key(&broadcast),
			"the unmarked giver is preferred"
		);

		router.recv(5, give(Change::Lost)).unwrap();
		let out: Vec<_> = (router.drain().into_iter())
			.filter(|(to, _)| *to == 3)
			.map(|(_, msg)| msg)
			.collect();
		let me = router.me();
		let me_marked = router.me_marked();
		let routed = out
			.iter()
			.position(|msg| matches!(msg, Msg::Route { origin, offer: Some(_) } if *origin == me_marked));
		let left = out.iter().position(|msg| {
			matches!(msg, Msg::Claims { origin, changes, .. }
				if *origin == me && changes.iter().any(|(_, change)| *change == Change::Lost))
		});
		let (Some(routed), Some(left)) = (routed, left) else {
			panic!("the move was not told: {out:?}");
		};
		assert!(routed < left, "the old identity left first: {out:?}");
		assert!(router.own_as(me_marked).1.contains_key(&broadcast));
	}

	/// A route that starts in the same batch as another node's withdrawal is forwarded
	/// ahead of it, as a claim moving between a node's two identities needs at each hop.
	#[test]
	fn a_route_started_before_a_withdrawal_is_forwarded_before_it() {
		let mut router = relay(1);
		open(&mut router, 2, Role::Routing(node(2)));
		open(&mut router, 3, Role::Routing(node(3)));
		router.drain();

		let route = |origin: Id| Msg::Route {
			origin: node(origin),
			offer: Some(Offer {
				path: vec![
					Hop {
						node: node(origin),
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
			}),
		};
		let broadcast = (p("live"), 7);
		let claims = |origin: Id, change, floor| Msg::Claims {
			origin: node(origin),
			changes: vec![(broadcast.clone(), change)],
			floor,
		};
		router.recv(2, route(2)).unwrap();
		router.recv(2, claims(2, live(0), 1)).unwrap();
		router.drain();

		router.recv(2, route(20)).unwrap();
		router.recv(2, claims(20, live(0), 1)).unwrap();
		router.recv(2, claims(2, Change::Lost, 2)).unwrap();
		let out: Vec<_> = (router.drain().into_iter())
			.filter(|(to, _)| *to == 3)
			.map(|(_, msg)| msg)
			.collect();
		let routed = out
			.iter()
			.position(|msg| matches!(msg, Msg::Route { origin, offer: Some(_) } if *origin == node(20)));
		let left = out.iter().position(|msg| {
			matches!(msg, Msg::Claims { origin, changes, .. }
				if *origin == node(2) && changes.iter().any(|(_, change)| *change == Change::Lost))
		});
		let (Some(routed), Some(left)) = (routed, left) else {
			panic!("the move was not forwarded: {out:?}");
		};
		assert!(routed < left, "the withdrawal went first: {out:?}");
		assert!(
			out[routed..left].iter().any(
				|msg| matches!(msg, Msg::Claims { origin, changes, .. } if *origin == node(20) && !changes.is_empty())
			),
			"the new route came without its claim: {out:?}"
		);
	}

	/// A route that turns marked ends at an upstream before the upstream hears another
	/// claim of it.
	#[test]
	fn a_route_turning_marked_ends_at_an_upstream_before_its_claims() {
		let mut router = relay(1);
		let upstream = |router: &mut Relay, peer: usize| {
			router
				.connect(
					peer,
					Port {
						upstream: true,
						..port(Role::Routing(node(peer as Id)))
					},
				)
				.unwrap();
			router.recv(peer, Msg::Edited).unwrap();
			router
				.edit(
					peer,
					Edit {
						remove: Vec::new(),
						add: vec![p(ROOT)],
					},
				)
				.unwrap();
		};
		open(&mut router, 3, Role::Routing(node(3)));
		upstream(&mut router, 4);
		upstream(&mut router, 6);
		router.drain();

		let route = |via: Id| Msg::Route {
			origin: node(9),
			offer: Some(Offer {
				path: vec![
					Hop {
						node: node(9),
						session: 0,
					},
					Hop {
						node: node(via),
						session: 1,
					},
				]
				.into(),
				draining: false,
				priority: i8::MAX,
				cost: 1,
				marked: false,
			}),
		};
		let claims = |broadcast: (Path, u64), floor| Msg::Claims {
			origin: node(9),
			changes: vec![(broadcast, live(0))],
			floor,
		};
		router.recv(3, route(3)).unwrap();
		router.recv(4, route(4)).unwrap();
		router.recv(3, claims((p("live"), 7), 1)).unwrap();
		let out = router.drain();
		assert!(
			(out.iter()).any(|(to, msg)| *to == 6 && matches!(msg, Msg::Route { offer: Some(_), .. })),
			"the unmarked route reaches the other upstream: {out:?}"
		);

		router
			.recv(
				3,
				Msg::Route {
					origin: node(9),
					offer: None,
				},
			)
			.unwrap();
		router.recv(4, claims((p("more"), 7), 2)).unwrap();
		let out: Vec<_> = (router.drain().into_iter())
			.filter(|(to, _)| *to == 6)
			.map(|(_, msg)| msg)
			.collect();
		let ended = out
			.iter()
			.position(|msg| matches!(msg, Msg::Route { origin, offer: None } if *origin == node(9)));
		let told = out.iter().position(
			|msg| matches!(msg, Msg::Claims { origin, changes, .. } if *origin == node(9) && !changes.is_empty()),
		);
		assert!(ended.is_some(), "the marked route stays at the upstream: {out:?}");
		assert!(
			told.is_none_or(|told| told > ended.unwrap()),
			"told a claim first: {out:?}"
		);
	}

	/// A neighbour that keeps a route that turned marked hears the mark before another
	/// claim of it, so it never passes the claim to its own upstream as unmarked.
	#[test]
	fn a_route_turning_marked_is_remarked_before_its_claims() {
		let mut router = relay(1);
		open(&mut router, 3, Role::Routing(node(3)));
		open(&mut router, 5, Role::Routing(node(5)));
		router
			.connect(
				4,
				Port {
					upstream: true,
					..port(Role::Routing(node(4)))
				},
			)
			.unwrap();
		router.recv(4, Msg::Edited).unwrap();
		router
			.edit(
				4,
				Edit {
					remove: Vec::new(),
					add: vec![p(ROOT)],
				},
			)
			.unwrap();
		router.drain();

		let route = |via: Id| Msg::Route {
			origin: node(9),
			offer: Some(Offer {
				path: vec![
					Hop {
						node: node(9),
						session: 0,
					},
					Hop {
						node: node(via),
						session: 1,
					},
				]
				.into(),
				draining: false,
				priority: i8::MAX,
				cost: 1,
				marked: false,
			}),
		};
		let claims = |broadcast: (Path, u64), floor| Msg::Claims {
			origin: node(9),
			changes: vec![(broadcast, live(0))],
			floor,
		};
		router.recv(3, route(3)).unwrap();
		router.recv(4, route(4)).unwrap();
		router.recv(3, claims((p("live"), 7), 1)).unwrap();
		router.drain();

		router
			.recv(
				3,
				Msg::Route {
					origin: node(9),
					offer: None,
				},
			)
			.unwrap();
		router.recv(4, claims((p("more"), 7), 2)).unwrap();
		let out: Vec<_> = (router.drain().into_iter())
			.filter(|(to, _)| *to == 5)
			.map(|(_, msg)| msg)
			.collect();
		let remarked = out.iter().position(
			|msg| matches!(msg, Msg::Route { origin, offer: Some(offer) } if *origin == node(9) && offer.marked),
		);
		let told = out.iter().position(
			|msg| matches!(msg, Msg::Claims { origin, changes, .. } if *origin == node(9) && !changes.is_empty()),
		);
		let (Some(remarked), Some(told)) = (remarked, told) else {
			panic!("the change was not told: {out:?}");
		};
		assert!(remarked < told, "told a claim before the mark: {out:?}");
	}

	/// A marked route that stops reaching its node leaves the node's claims marked, so an
	/// upstream is not told them, even unavailable, and a plain neighbour is told them
	/// unavailable and marked. While the route reaches them, each way to them is marked.
	#[test]
	fn claims_keep_their_mark_once_no_route_reaches_them() {
		let mut router = relay(1);
		let upstream = |role| Port {
			upstream: true,
			..port(role)
		};
		router.connect(4, upstream(Role::Routing(node(4)))).unwrap();
		router.recv(4, Msg::Edited).unwrap();
		router.connect(6, upstream(Role::Scoped(vec![p(ROOT)]))).unwrap();
		router.recv(6, Msg::Edited).unwrap();
		router
			.edit(
				6,
				Edit {
					remove: Vec::new(),
					add: vec![p(ROOT)],
				},
			)
			.unwrap();
		open(&mut router, 7, Role::Scoped(vec![p(ROOT)]));
		router
			.edit(
				7,
				Edit {
					remove: Vec::new(),
					add: vec![p(ROOT)],
				},
			)
			.unwrap();
		router.drain();

		let sender = Hop {
			node: node(4),
			session: 1,
		};
		let route = Msg::Route {
			origin: node(9),
			offer: Some(Offer {
				path: vec![
					Hop {
						node: node(9),
						session: 0,
					},
					sender,
				]
				.into(),
				draining: false,
				priority: i8::MAX,
				cost: 1,
				marked: false,
			}),
		};
		router.recv(4, route).unwrap();
		let claims = Msg::Claims {
			origin: node(9),
			changes: vec![((p("live"), 7), live(0))],
			floor: 1,
		};
		router.recv(4, claims).unwrap();
		let ways = router.ways(&(p("live"), 7));
		assert!(!ways.is_empty() && ways.iter().all(|way| way.claim.marked), "{ways:?}");
		router
			.recv(
				4,
				Msg::Dead {
					hop: sender,
					far: node(9),
				},
			)
			.unwrap();
		let out = router.drain();
		let told: Vec<_> = (out.iter())
			.filter(|(to, msg)| *to == 6 && matches!(msg, Msg::Announce { changes } if !changes.is_empty()))
			.collect();
		assert!(told.is_empty(), "the upstream was told the claim: {told:?}");
		let plain: Vec<_> = (out.iter())
			.filter(|(to, _)| *to == 7)
			.filter_map(|(_, msg)| match msg {
				Msg::Announce { changes } => Some(changes),
				_ => None,
			})
			.flatten()
			.collect();
		assert!(
			matches!(plain.last(), Some((_, Change::Live(claim))) if claim.unavailable && claim.marked),
			"{plain:?}"
		);
	}

	/// A path names a marked Node ID only as the node it leads to, followed by that node
	/// itself, unmarked, at session 0; anything else is refused. The marked hop counts
	/// toward the longest path like any other.
	#[test]
	fn a_marked_node_id_leads_a_path_and_counts_as_a_hop() {
		let mut router = relay(1);
		open(&mut router, 2, Role::Routing(node(2)));
		let marked = NodeId {
			marked: true,
			..node(5)
		};
		let hop = |node: NodeId, session| Hop { node, session };
		let offer = |path: Vec<Hop>| Offer {
			path: path.into(),
			draining: false,
			priority: i8::MAX,
			cost: 1,
			marked: true,
		};
		let send = |router: &mut Relay, origin: NodeId, path: Vec<Hop>| {
			router.recv(
				2,
				Msg::Route {
					origin,
					offer: Some(offer(path)),
				},
			)
		};
		let sender = hop(node(2), 1);
		// Alone, followed by another node, by itself at a session, or past the first hop.
		for path in [
			vec![hop(marked, 0), sender],
			vec![hop(marked, 0), hop(node(6), 0), sender],
			vec![hop(marked, 0), hop(node(5), 3), sender],
		] {
			assert_eq!(
				send(&mut router, marked, path.clone()),
				Err(Error::Malformed),
				"{path:?}"
			);
		}
		assert_eq!(
			send(&mut router, node(5), vec![hop(node(5), 0), hop(marked, 0), sender]),
			Err(Error::Malformed)
		);
		send(&mut router, marked, vec![hop(marked, 0), hop(node(5), 0), sender]).unwrap();
		assert!(router.best(marked).is_some());

		// A marked identity holds no session, so a neighbour naming itself by one is refused
		// the route to itself, the marked hop alone.
		let named = NodeId {
			marked: true,
			..node(3)
		};
		open(&mut router, 3, Role::Routing(named));
		let own = Msg::Route {
			origin: named,
			offer: Some(Offer {
				cost: 0,
				..offer(vec![hop(named, 0)])
			}),
		};
		assert_eq!(router.recv(3, own), Err(Error::Malformed));

		// Through relays 10 and up, as long as the path may be, the marked hop included,
		// it is heard and never used, as any route at the longest path.
		let through = |len: usize| {
			let mut path = vec![hop(marked, 0), hop(node(5), 0)];
			path.extend((10..).take(len - 3).map(|relay| hop(node(relay), 1)));
			path.push(sender);
			path
		};
		send(&mut router, marked, through(MAX_HOPS - 1)).unwrap();
		assert!(router.best(marked).is_some(), "one hop short of the longest path");
		send(&mut router, marked, through(MAX_HOPS)).unwrap();
		assert!(
			router.best(marked).is_none(),
			"the marked hop counts toward the longest path"
		);
		assert_eq!(send(&mut router, marked, through(MAX_HOPS + 1)), Err(Error::Malformed));
	}

	/// A route that turns marked at the same rank re-tells a scoped neighbour each claim
	/// it reaches, now marked.
	#[test]
	fn a_route_turning_marked_at_equal_rank_retells_its_claims() {
		let mut router = relay(1);
		open(&mut router, 3, Role::Routing(node(3)));
		router
			.connect(
				4,
				Port {
					upstream: true,
					..port(Role::Routing(node(4)))
				},
			)
			.unwrap();
		router.recv(4, Msg::Edited).unwrap();
		router
			.edit(
				4,
				Edit {
					remove: Vec::new(),
					add: vec![p(ROOT)],
				},
			)
			.unwrap();
		open(&mut router, 5, Role::Scoped(vec![p(ROOT)]));
		router
			.edit(
				5,
				Edit {
					remove: Vec::new(),
					add: vec![p(ROOT)],
				},
			)
			.unwrap();
		router.drain();

		let route = |via: Id| Msg::Route {
			origin: node(9),
			offer: Some(Offer {
				path: vec![
					Hop {
						node: node(9),
						session: 0,
					},
					Hop {
						node: node(via),
						session: 1,
					},
				]
				.into(),
				draining: false,
				priority: i8::MAX,
				cost: 1,
				marked: false,
			}),
		};
		router.recv(3, route(3)).unwrap();
		router.recv(4, route(4)).unwrap();
		let broadcast = (p("live"), 7);
		let claims = Msg::Claims {
			origin: node(9),
			changes: vec![(broadcast.clone(), live(0))],
			floor: 1,
		};
		router.recv(3, claims).unwrap();
		let told = |out: Vec<(usize, Msg<u64, u64>)>| {
			(out.into_iter()).find_map(|(to, msg)| match msg {
				Msg::Announce { changes } if to == 5 => (changes.into_iter())
					.find(|(told, _)| told.0 == broadcast.0)
					.and_then(|(_, change)| match change {
						Change::Live(claim) => Some(claim),
						_ => None,
					}),
				_ => None,
			})
		};
		let first = told(router.drain()).expect("the scoped neighbour is told the claim");
		assert!(!first.marked, "the unmarked route is preferred");

		router
			.recv(
				3,
				Msg::Route {
					origin: node(9),
					offer: None,
				},
			)
			.unwrap();
		let again = told(router.drain()).expect("the scoped neighbour is told the claim again");
		assert!(again.marked, "now reached over a marked link");
	}

	/// A claim taken over a marked link goes under the node's marked identity, whose route
	/// is marked: an unmarked routing neighbour gets the route with the claim, a marked one
	/// neither, and the node's own copy stays whole for every neighbour. A scoped neighbour
	/// is told the claim with its mark, and files it under its own marked identity.
	#[test]
	fn a_claim_taken_over_a_marked_link_goes_under_the_marked_identity() {
		let mut router = relay(1);
		let marked = Port {
			upstream: true,
			..port(Role::Endpoint)
		};
		router.connect(2, marked.clone()).unwrap();
		router.recv(2, Msg::Edited).unwrap();
		open(&mut router, 3, Role::Routing(node(3)));
		router
			.connect(
				4,
				Port {
					role: Role::Routing(node(4)),
					..marked
				},
			)
			.unwrap();
		router.recv(4, Msg::Edited).unwrap();
		router
			.edit(
				4,
				Edit {
					remove: Vec::new(),
					add: vec![p(ROOT)],
				},
			)
			.unwrap();
		open(&mut router, 5, Role::Scoped(vec![p(ROOT)]));
		router
			.edit(
				5,
				Edit {
					remove: Vec::new(),
					add: vec![p(ROOT)],
				},
			)
			.unwrap();
		router.drain();

		let local = Msg::Announce {
			changes: vec![((p("local"), 8), live(0))],
		};
		router.recv(5, local).unwrap();
		router.drain();
		let upstream = Msg::Announce {
			changes: vec![((p("live"), 7), live(0))],
		};
		router.recv(2, upstream).unwrap();
		let out = router.drain();

		let me_marked = router.me_marked();
		assert_ne!(me_marked, router.me());
		assert!(
			!router.own().1.contains_key(&(p("live"), 7)),
			"filed under the node's own ID"
		);
		assert!(router.own().1.contains_key(&(p("local"), 8)));
		let (_, marked_claims) = router.own_as(me_marked);
		assert!(marked_claims[&(p("live"), 7)].marked);

		let to = |peer: usize| -> Vec<&Msg<u64, u64>> {
			(out.iter()).filter(|(to, _)| *to == peer).map(|(_, msg)| msg).collect()
		};
		let routed = |msgs: &[&Msg<u64, u64>]| {
			msgs.iter()
				.any(|msg| matches!(msg, Msg::Route { origin, offer: Some(_) } if *origin == me_marked))
		};
		let unmarked = to(3);
		assert!(
			routed(&unmarked),
			"an unmarked neighbour is offered the marked identity: {unmarked:?}"
		);
		let offer = unmarked.iter().find_map(|msg| match msg {
			Msg::Route {
				origin,
				offer: Some(offer),
			} if *origin == me_marked => Some(offer),
			_ => None,
		});
		assert!(offer.expect("the route").marked, "the route is marked from the start");
		let upstream = to(4);
		assert!(!routed(&upstream), "a marked neighbour is offered it: {upstream:?}");
		assert!(
			!upstream
				.iter()
				.any(|msg| matches!(msg, Msg::Claims { changes, .. } if !changes.is_empty())),
			"a marked neighbour heard a claim: {upstream:?}"
		);
		let scoped = to(5);
		let told = scoped.iter().find_map(|msg| match msg {
			Msg::Announce { changes } => changes.iter().find(|((path, _), _)| *path == p("live")),
			_ => None,
		});
		let Some((_, Change::Live(told))) = told else {
			panic!("the scoped neighbour was not told the claim: {scoped:?}");
		};
		assert!(told.marked, "the scoped neighbour is told the mark");

		// Across the grant, the claim lands under the receiver's marked identity, never
		// offered to its own marked neighbour.
		let mut far = relay(10);
		open(&mut far, 11, Role::Scoped(vec![p(ROOT)]));
		far.connect(
			12,
			Port {
				upstream: true,
				..port(Role::Routing(node(12)))
			},
		)
		.unwrap();
		far.recv(12, Msg::Edited).unwrap();
		far.edit(
			12,
			Edit {
				remove: Vec::new(),
				add: vec![p(ROOT)],
			},
		)
		.unwrap();
		far.drain();
		let changes = vec![((p("live"), 7), Change::Live(told.clone()))];
		far.recv(11, Msg::Announce { changes }).unwrap();
		let out = far.drain();
		assert!(far.own().1.is_empty());
		assert!(far.own_as(far.me_marked()).1.contains_key(&(p("live"), 7)));
		assert!(
			!out.iter()
				.any(|(to, msg)| *to == 12 && matches!(msg, Msg::Route { offer: Some(_), .. } | Msg::Claims { .. })),
			"a marked neighbour heard of it: {out:?}"
		);
	}

	/// Among claims of equal rank, one no marked link reached is closest, as a route is
	/// chosen, so an upstream is told it rather than nothing. Node 4's claim reaches this
	/// node over a link it marked and node 5's over one it did not, at the same rank, and
	/// node 4 names the lower Node ID.
	#[test]
	fn an_unmarked_claim_is_closest_among_equal_ranks() {
		let mut router: Router<usize, u64, Run> = Router::new(node(1), debounce(HOP));
		let upstream = |role| Port {
			upstream: true,
			..port(role)
		};
		router.connect(2, upstream(Role::Routing(node(2)))).unwrap();
		router.recv(2, Msg::Edited).unwrap();
		open(&mut router, 3, Role::Routing(node(3)));
		router.connect(6, upstream(Role::Scoped(vec![p(ROOT)]))).unwrap();
		router.recv(6, Msg::Edited).unwrap();
		let all = Edit {
			remove: Vec::new(),
			add: vec![p(ROOT)],
		};
		router.edit(2, all.clone()).unwrap();
		router.edit(6, all).unwrap();
		router.drain();

		let room = (p("live"), 7);
		for (origin, via) in [(4, 2), (5, 3)] {
			router.recv(via, route(origin, via as Id, 1)).unwrap();
			let claims = Msg::Claims {
				origin: node(origin),
				changes: vec![(room.clone(), claim(run(0), 0))],
				floor: 1,
			};
			router.recv(via, claims).unwrap();
		}
		let out = router.drain();
		let told = (out.iter()).any(|(to, msg)| {
			*to == 6 && matches!(msg, Msg::Announce { changes } if changes.iter().any(|(at, change)| *at == room && matches!(change, Change::Live(_))))
		});
		assert!(told, "the upstream was told nothing: {out:?}");
		assert_eq!(router.pick(&room), Some(node(5)), "the unmarked claim is closest");
	}

	/// A route that turns marked at the same rank, through the same session and path,
	/// changes which of two equal-rank claims is closest, so it is logged as a change to
	/// the ways (#9114 logs only those) and the closest way moves with it. Node 4's claim
	/// reaches this node over a link it marked and node 5's over one it did not; then node
	/// 5's route turns marked, and node 4's gains an unmarked one.
	#[test]
	fn a_route_flipping_which_claim_is_unmarked_reaches_its_followers() {
		let mut router: Router<usize, u64, Run> = Router::new(node(1), debounce(HOP));
		router.follow();
		router
			.connect(
				2,
				Port {
					upstream: true,
					..port(Role::Routing(node(2)))
				},
			)
			.unwrap();
		router.recv(2, Msg::Edited).unwrap();
		open(&mut router, 3, Role::Routing(node(3)));
		router.drain();

		let room = (p("live"), 7);
		for (origin, via) in [(4, 2), (5, 3)] {
			router.recv(via, route(origin, via as Id, 1)).unwrap();
			let claims = Msg::Claims {
				origin: node(origin),
				changes: vec![(room.clone(), claim(run(0), 0))],
				floor: 1,
			};
			router.recv(via, claims).unwrap();
		}
		router.drain();
		router.changes();
		assert_eq!(router.pick(&room), Some(node(5)), "the unmarked claim is closest");

		let closest = |router: &Router<usize, u64, Run>| {
			(router.ways(&room).into_iter())
				.filter(|way| way.closest)
				.map(|way| (way.origin, way.next, way.claim.marked))
				.collect::<Vec<_>>()
		};
		let mut marked = route(5, 3, 1);
		if let Msg::Route { offer: Some(offer), .. } = &mut marked {
			offer.marked = true;
		}
		router.recv(3, marked).unwrap();
		router.drain();
		assert_eq!(
			router.changes().routes,
			BTreeSet::from([node(5)]),
			"the mark alone moved"
		);
		assert_eq!(router.pick(&room), Some(node(4)), "both marked: the lower Node ID");
		assert_eq!(closest(&router), vec![(node(4), Next::Via(2), true)]);

		// A route that starts brings the claims as they stand.
		router.recv(3, route(4, 3, 1)).unwrap();
		let claims = Msg::Claims {
			origin: node(4),
			changes: vec![(room.clone(), claim(run(0), 0))],
			floor: 1,
		};
		router.recv(3, claims).unwrap();
		router.drain();
		assert_eq!(router.changes().routes, BTreeSet::from([node(4)]), "an unmarked route");
		assert_eq!(router.pick(&room), Some(node(4)), "the unmarked claim is closest");
		assert_eq!(closest(&router), vec![(node(4), Next::Via(3), false)]);
	}
}
