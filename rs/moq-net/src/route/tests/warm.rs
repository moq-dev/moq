//! A data plane over the multicast prototype ([`MultiSim`]), to measure how a relay
//! learns that a neighbour already has demand for a broadcast
//! (`quest/m3/cache-routing/select.md`). Nothing here ships; `warmth.rs` holds the
//! shapes, the pins, and the tables.
//!
//! A relay has demand for a broadcast while it has a reader or a neighbour's payload
//! subscription, and pulls it through one parent. It is warm while it has demand and
//! its parent's answer confirms the pull. Every link has a cold price, what a copy
//! across it costs and what the route layer ranks by, and a warm price no higher and at
//! least 1. Costs are summed from the publisher along the path the data takes:
//!
//! - A relay's own cost is its parent's offer plus the warm price of the link between.
//! - What it offers a neighbour is that cost while it has demand besides that
//!   neighbour, and otherwise its parent's offer plus the link's cold price, since the
//!   copy exists only for the asker. An offer is never below its sender's own cost.
//! - With no demand a relay prices its route at the route layer's summed cold cost,
//!   and a warm neighbour at its offer plus the cold price of the link to it.
//! - A pulling relay moves only to a source strictly cheaper than its own cost. Two
//!   relays then never each prefer the other: with `o(x)` an offer and `d(x)` an own
//!   cost, that needs `o(b) + link < d(a) <= o(a)` and `o(a) + link < d(b) <= o(b)`.
//!
//! What a relay hears of its neighbours is the [`Mode`]. Every move is make before
//! break: the old parent feeds until the new one's answer confirms. A relay never
//! pulls through a neighbour that pulls through it.
//!
//! The route layer runs underneath, unchanged, under the bake-off's pick
//! ([`super::compare::Arm::KEPT`]), its messages interleaved with these at random.

use std::{
	cmp::Reverse,
	collections::{BTreeMap, BTreeSet, BinaryHeap, VecDeque},
};

use super::compare::Arm as Rule;

/// A rendezvous hash of a broadcast's path and a relay, which ranks its carriers. The
/// basis is the one the measurements here were taken with.
fn rendezvous(path: &Path, node: Id) -> u64 {
	let mut hash: u64 = 0x420C_0DEC_B00B;
	for byte in path.as_str().bytes().chain(node.to_le_bytes()) {
		hash = (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3);
	}
	hash
}
use super::multi::{Broadcast, MultiSim, Next, Tie};
use super::*;

/// How a relay hears that a neighbour has demand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mode {
	/// Not at all: a join follows the route and stops at the first relay with demand,
	/// equal next hops picked by the rendezvous hash.
	Path,
	/// A relay with demand holds a metadata-only subscription to this many neighbours,
	/// the cheapest links first. Each answers, and updates, whether it is warm and at
	/// what cost. A neighbour with no demand forwards the subscription along its route,
	/// as a metadata-only subscription does.
	Ask(usize),
	/// A warm relay tells neighbours, and each change, on its announce stream: which
	/// ones is the [`Audience`].
	Advertise(Audience),
}

/// Which neighbours a warm relay advertises to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Audience {
	/// Every relay neighbour.
	All,
	/// Only a neighbour for whom its offer plus the link beats that neighbour's route,
	/// or ties it through another next hop, since a tie prefers a warm copy. Nothing
	/// else could ever take it: a relay at rest costs no more than its own route, and
	/// a tie through this relay is the path join already. The neighbour's route is
	/// the one it last offered this relay, never its current one.
	Useful,
	/// Only once it has demand from two sources besides the neighbour.
	Shared,
	/// Never over a link marked upstream.
	Down,
	/// As [`Audience::Useful`], but never to a neighbour whose route runs through this
	/// relay: its path join lands here anyway, though it then prices this relay cold
	/// against another warm neighbour.
	NotNext,
	/// As [`Audience::Useful`], but never over a link the neighbour marked upstream: an
	/// edge relies on its path join and the rendezvous hash among equal upstreams. Nor
	/// up a marked link with a copy that came over one, as the route layer never passes
	/// a route from one marked link to another: an edge carries nothing between cores.
	NotDown,
	/// As [`Audience::NotDown`], and a relay with demand whose route ties between
	/// upstreams asks them on subscribe, without waiting for their answers.
	NotDownAsk,
	/// As [`Audience::NotDown`], and a relay with demand asks every upstream besides the
	/// one its path join takes, on subscribe, without waiting for their answers.
	NotDownAskUp,
	/// As [`Audience::NotDownAskUp`], but each ask is one request and one answer: the
	/// upstream answers its warm offer once and keeps nothing, and the relay drops the
	/// answer once a pull from that upstream is answered, live or lost.
	NotDownQuery,
}

/// One way to find a warm copy, as measured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Arm {
	pub mode: Mode,
	/// An offer carries the relays its data crossed, and a relay never pulls from one
	/// that names it. Without it a stale cost is all a cycle has to count up through.
	pub hops: bool,
	/// Relays holding equal-cost copies merge: the lower rendezvous hash joins the
	/// higher, when nothing else is cheaper once it has.
	pub merge: bool,
	/// A subscription names the relays whose demand alone it carries, and an offer
	/// leaves out the demand that passes through its receiver. Without it, the rule
	/// that stops the same loop: a relay never takes a warm signal from a relay named
	/// on its own pull's path, whose warmth may be this relay's own demand.
	pub chain: bool,
	/// A settled pull stays: a relay chooses where to pull when its demand starts, and
	/// once more on each query answer from that join, then moves only when its parent
	/// says it lost its upstream, the pull runs through itself, or the session closes.
	/// No price a neighbour offers later moves it, so no relay's move is another's
	/// reason to move, and no rule about whose warmth it is applies.
	pub stay: bool,
	/// With [`Arm::stay`], a relay already pulling for others chooses again, once, when
	/// a reader of its own starts: an outside event, like a join.
	pub rejoin: bool,
	/// With [`Arm::stay`], a relay chooses again, once, each time its route changes:
	/// the route layer's routes follow links and claims, never demand, so no relay's
	/// pull is another's reason to move.
	pub reroute: bool,
	/// Readers are ranked, and a relay takes a warm copy only from a neighbour feeding
	/// a reader ranked below every one it feeds itself: such a copy is there whatever
	/// this relay does, so it costs the cold price of the link alone. A relay heads for
	/// the cheapest such copy or its route whenever either changes, so a pull follows a
	/// copy that turns warm later.
	pub order: Option<Order>,
	/// With [`Arm::order`], a copy costs what it does from the publisher, its offer plus
	/// the link, rather than the link alone, and a live pull stays unless another
	/// source is strictly cheaper than it, priced at the warm link it flows over.
	pub summed: bool,
}

/// How [`Arm::order`] ranks a reader, then by its relay.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Order {
	/// Its relay's route cost: a relay nearer the publisher ranks first.
	Route,
	/// The rendezvous hash of its relay and the broadcast, which no route changes.
	Hash,
}

/// A reader's rank under [`Arm::order`]: lower ranks first.
pub(super) type Rank = (u64, Node);

impl Arm {
	pub fn name(self) -> String {
		let mode = match self.mode {
			Mode::Path => "path".to_string(),
			Mode::Ask(usize::MAX) => "ask_all".into(),
			Mode::Ask(count) => format!("ask_{count}"),
			Mode::Advertise(Audience::All) => "advertise".into(),
			Mode::Advertise(Audience::Useful) => "advertise_useful".into(),
			Mode::Advertise(Audience::Shared) => "advertise_shared".into(),
			Mode::Advertise(Audience::Down) => "advertise_down".into(),
			Mode::Advertise(Audience::NotNext) => "advertise_not_next".into(),
			Mode::Advertise(Audience::NotDown) => "advertise_not_down".into(),
			Mode::Advertise(Audience::NotDownAsk) => "advertise_not_down_ask".into(),
			Mode::Advertise(Audience::NotDownAskUp) => "advertise_not_down_ask_up".into(),
			Mode::Advertise(Audience::NotDownQuery) => "advertise_not_down_query".into(),
		};
		let hops = if self.hops { "+hops" } else { "" };
		let merge = if self.merge { "+merge" } else { "" };
		let chain = if self.chain { "+chain" } else { "" };
		let stay = if self.stay { "+stay" } else { "" };
		let rejoin = if self.rejoin { "+rejoin" } else { "" };
		let reroute = if self.reroute { "+reroute" } else { "" };
		let order = match self.order {
			None => "",
			Some(Order::Route) => "+order_route",
			Some(Order::Hash) => "+order_hash",
		};
		let summed = if self.summed { "+summed" } else { "" };
		format!("{mode}{hops}{merge}{chain}{stay}{rejoin}{reroute}{order}{summed}")
	}
}

/// A link's two prices: a copy starting across it, and one that already crosses it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Price {
	pub cold: Metric,
	pub warm: Metric,
}

impl Price {
	pub fn new(cold: Metric, warm: Metric) -> Self {
		assert!(1 <= warm && warm <= cold, "a warm price of {warm} against {cold}");
		Self { cold, warm }
	}

	pub fn flat(cost: Metric) -> Self {
		Self::new(cost, cost)
	}
}

/// What a relay offers a neighbour: its summed cost, with [`Arm::hops`] the relays the
/// data crossed, the publisher's own relay first, and whether the data came over a link
/// its receiver marked upstream, the bit a route's offer carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Signal {
	pub cost: Metric,
	pub path: Vec<Node>,
	pub marked: bool,
	/// With [`Arm::order`], the lowest rank among the readers the sender feeds besides
	/// its receiver.
	pub rank: Option<Rank>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum State {
	/// Sent, not yet answered.
	Pending,
	Live(Signal),
	/// The parent lost its own upstream.
	Lost,
}

/// A payload subscription this relay holds.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Pull {
	via: Node,
	stream: u64,
	state: State,
	/// The merge made it, so a cycle through it is one the merge added.
	merged: bool,
	/// The relays whose demand alone it carries, as last told the parent.
	chain: Vec<Node>,
	/// With [`Arm::order`], the lowest rank among the readers it feeds, as last told
	/// the parent.
	rank: Option<Rank>,
}

#[derive(Clone, Debug)]
enum Msg {
	/// A payload subscription opening, with the relays whose demand alone it carries,
	/// or closing.
	Subscribe {
		stream: u64,
		open: bool,
		chain: Vec<Node>,
		rank: Option<Rank>,
	},
	/// A change to the lowest rank a payload subscription feeds, with [`Arm::order`].
	Rank {
		stream: u64,
		rank: Option<Rank>,
	},
	/// A change to the relays whose demand alone a payload subscription carries.
	Chain {
		stream: u64,
		chain: Vec<Node>,
	},
	/// A parent's answer and each change to it: its offer, or that it lost its upstream.
	Answer {
		stream: u64,
		offer: Option<Signal>,
	},
	/// The metadata-only subscription that asks, opening or closing.
	Ask {
		stream: u64,
		open: bool,
	},
	/// The asked neighbour's word and each change to it.
	Told {
		stream: u64,
		word: Option<Signal>,
	},
	Advert(Option<Signal>),
}

/// The stream a message travels on: each subscription and query is a stream of its
/// own, unordered against the rest, and adverts share a session's announce stream,
/// in order with the route layer's messages.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Lane {
	Pull,
	Ask,
	Announce,
}

/// A queue: sender, receiver, lane, and the stream: a subscription's or query's own,
/// or `usize::MAX` for the session's announce stream.
type Key = (Node, Node, Lane, usize);

/// One relay's state for one broadcast.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Carry {
	reader: bool,
	/// The round its reader started, until its first frame.
	since: Option<usize>,
	/// Neighbours pulling through this relay, and their subscriptions: a neighbour's
	/// new one may arrive before its old one closes.
	down: BTreeMap<Node, BTreeSet<u64>>,
	up: Option<Pull>,
	/// A move under way: it replaces `up` once answered.
	next: Option<Pull>,
	/// What each subscription through it was last answered.
	answered: BTreeMap<(Node, u64), Option<Signal>>,
	/// The relays whose demand alone each subscription through it carries.
	chains: BTreeMap<(Node, u64), Vec<Node>>,
	/// The lowest rank each subscription through it feeds, with [`Arm::order`].
	ranks: BTreeMap<(Node, u64), Option<Rank>>,
	/// Neighbours it asks, with each one's latest word.
	asks: BTreeMap<Node, Option<Signal>>,
	/// The query it holds open to each.
	asking: BTreeMap<Node, u64>,
	/// Those yet to answer: a relay with no pull waits for them before it starts one.
	unanswered: BTreeSet<Node>,
	/// Neighbours asking it, and their queries.
	asked: BTreeMap<Node, BTreeSet<u64>>,
	told: BTreeMap<(Node, u64), Option<Signal>>,
	heard: BTreeMap<Node, Signal>,
	advertised: BTreeMap<Node, Signal>,
	/// Upstreams whose query answer arrived since the relay last judged one, with
	/// [`Arm::stay`].
	fresh: BTreeSet<Node>,
	/// A reader of its own started while it already pulled, with [`Arm::rejoin`].
	rejoin: bool,
	/// The route it last chose by, with [`Arm::reroute`].
	route: Option<(Node, Metric)>,
}

impl Carry {
	fn demand(&self) -> bool {
		self.reader || !self.down.is_empty()
	}

	/// Demand besides `peer`'s, counting a pull that carries only demand passing
	/// through `peer` as `peer`'s: otherwise `peer`'s own pull through another neighbour
	/// would make this relay's offer to `peer` warm, and `peer` would move here and back
	/// for good.
	fn others(&self, peer: Node) -> bool {
		self.reader
			|| (self.down.iter()).any(|(&child, streams)| {
				child != peer
					&& (streams.iter()).any(|&stream| {
						!self
							.chains
							.get(&(child, stream))
							.is_some_and(|chain| chain.contains(&peer))
					})
			})
	}

	/// The relays whose demand alone `node`'s pull carries: each below it with one
	/// neighbour pulling and no reader, down to the first with a reader or more than one,
	/// then `node`.
	fn chain(&self, node: Node) -> Vec<Node> {
		let mut children = self.down.iter();
		let mut chain = match (self.reader, children.next(), children.next()) {
			(false, Some((&child, streams)), None) => (streams.last())
				.and_then(|&stream| self.chains.get(&(child, stream)).cloned())
				.unwrap_or_else(|| vec![child]),
			_ => Vec::new(),
		};
		chain.push(node);
		chain
	}
}

/// Messages this layer sent, by kind.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Sent {
	/// Payload subscriptions opened and closed.
	pub subscribes: usize,
	/// Answers to them and their updates.
	pub answers: usize,
	/// Metadata-only subscriptions opened and closed.
	pub asks: usize,
	/// Their answers and updates.
	pub told: usize,
	pub adverts: usize,
	/// Updates to the relays whose demand alone a payload subscription carries.
	pub chains: usize,
	/// Updates to the lowest rank a payload subscription feeds, with [`Arm::order`].
	pub ranks: usize,
}

impl Sent {
	pub fn total(self) -> usize {
		self.subscribes + self.answers + self.warm()
	}

	/// What only a warm signal costs: queries, their answers, adverts, and chain and
	/// rank updates.
	pub fn warm(self) -> usize {
		self.asks + self.told + self.adverts + self.chains + self.ranks
	}
}

impl std::ops::Add for Sent {
	type Output = Self;

	fn add(self, more: Self) -> Self {
		Self {
			subscribes: self.subscribes + more.subscribes,
			answers: self.answers + more.answers,
			asks: self.asks + more.asks,
			told: self.told + more.told,
			adverts: self.adverts + more.adverts,
			chains: self.chains + more.chains,
			ranks: self.ranks + more.ranks,
		}
	}
}

impl std::ops::Sub for Sent {
	type Output = Self;

	fn sub(self, was: Self) -> Self {
		Self {
			subscribes: self.subscribes - was.subscribes,
			answers: self.answers - was.answers,
			asks: self.asks - was.asks,
			told: self.told - was.told,
			adverts: self.adverts - was.adverts,
			chains: self.chains - was.chains,
			ranks: self.ranks - was.ranks,
		}
	}
}

/// What went wrong while messages moved.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Watch {
	/// Deliveries observed.
	pub steps: usize,
	/// Deliveries after which some broadcast's pulls formed a cycle.
	pub cycles: usize,
	/// Of those, deliveries after which a reader's pulls led into it.
	pub starved: usize,
	/// Of those, deliveries after which a cycle ran through a pull the merge made.
	pub merged: usize,
	/// The most deliveries in a row a cycle lasted.
	pub longest: usize,
	run: usize,
	/// Readers, summed over rounds, unfed while a path to the publisher existed.
	pub unfed: usize,
	/// Copies past the first leaving the publisher's relay, summed over rounds: the
	/// cost of a race left standing.
	pub extra: usize,
}

impl Watch {
	/// What changed since `was`, with the longest cycle so far.
	pub fn since(self, was: Self) -> Self {
		Self {
			steps: self.steps - was.steps,
			cycles: self.cycles - was.cycles,
			starved: self.starved - was.starved,
			merged: self.merged - was.merged,
			longest: self.longest,
			run: 0,
			unfed: self.unfed - was.unfed,
			extra: self.extra - was.extra,
		}
	}
}

/// A broadcast's tree once it settled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Tree {
	/// Links carrying it, parent first.
	pub edges: Vec<(Node, Node)>,
	/// Its links' cold prices summed: what the copies cost.
	pub cost: Metric,
	/// The most copies of it one link carries, both ways counted.
	pub copies: usize,
	/// Copies leaving the publisher's relay.
	pub uplinks: usize,
}

/// Close `stream` from `peer`, forgetting `peer` once none is left.
fn close(streams: &mut BTreeMap<Node, BTreeSet<u64>>, peer: Node, stream: u64) {
	if let Some(open) = streams.get_mut(&peer) {
		open.remove(&stream);
		if open.is_empty() {
			streams.remove(&peer);
		}
	}
}

pub(super) fn pair(a: Node, b: Node) -> (Node, Node) {
	(a.min(b), a.max(b))
}

/// A source a relay can pull from.
#[derive(Clone, Debug)]
struct Choice {
	via: Node,
	cost: Metric,
	/// The neighbour's offer, when it is a warm copy rather than the route.
	offered: Option<Metric>,
}

pub(super) struct WarmSim {
	pub arm: Arm,
	pub net: MultiSim,
	relays: usize,
	nodes: usize,
	/// Each publisher's endpoint node, and the relay it joined.
	endpoints: BTreeMap<Id, Node>,
	homes: BTreeMap<Id, Node>,
	prices: BTreeMap<(Node, Node), Price>,
	/// Links marked upstream.
	marked: BTreeSet<(Node, Node)>,
	/// The same, as the relay that marked it and its upstream.
	upstream: BTreeSet<(Node, Node)>,
	open: BTreeSet<(Node, Node)>,
	/// Each node's open links.
	adjacent: Vec<BTreeSet<Node>>,
	casts: Vec<Broadcast>,
	carry: Vec<BTreeMap<usize, Carry>>,
	/// Each message with the route layer's clock when it was queued: an advert keeps its
	/// place among the route messages on the same session's announce stream.
	queues: BTreeMap<Key, VecDeque<(u64, usize, Msg)>>,
	streams: u64,
	/// What must react before the next delivery: a relay and broadcast whose inputs
	/// changed. Nothing else can act, so nothing else is asked to.
	dirty: BTreeSet<(Node, usize)>,
	/// Broadcasts whose pulls changed since cycles were last looked for.
	changed: BTreeSet<usize>,
	/// The relays holding state for each broadcast, as of the last step: only they
	/// can pull, so only they can circle.
	members: BTreeMap<usize, BTreeSet<Node>>,
	/// Broadcasts whose pulls circle: whether a reader's lead into it, and whether it
	/// holds a pull the merge made.
	circling: BTreeMap<usize, (bool, bool)>,
	pub sent: Sent,
	pub watch: Watch,
	/// The deliveries [`WarmSim::settle`] may spend before it gives up, if bounded: a
	/// storm that only grows would cost unbounded work.
	pub budget: Option<usize>,
	pub round: usize,
	order: XorShift,
	/// Rounds each reader waited for its first frame.
	pub waits: Vec<usize>,
}

impl WarmSim {
	/// `relays` relays, numbered first, and `publishers` endpoints after them.
	pub fn new(arm: Arm, relays: usize, publishers: usize, seed: u64) -> Self {
		let nodes = relays + publishers;
		Self {
			arm,
			net: MultiSim::new(Rule::TUNE, nodes),
			relays,
			nodes,
			endpoints: BTreeMap::new(),
			homes: BTreeMap::new(),
			prices: BTreeMap::new(),
			marked: BTreeSet::new(),
			upstream: BTreeSet::new(),
			open: BTreeSet::new(),
			adjacent: vec![BTreeSet::new(); nodes],
			casts: Vec::new(),
			carry: vec![BTreeMap::new(); nodes],
			queues: BTreeMap::new(),
			streams: 0,
			dirty: BTreeSet::new(),
			changed: BTreeSet::new(),
			members: BTreeMap::new(),
			circling: BTreeMap::new(),
			sent: Sent::default(),
			watch: Watch::default(),
			budget: None,
			round: 0,
			order: XorShift(seed | 1),
			waits: Vec::new(),
		}
	}

	fn relay(&self, node: Node) -> bool {
		node < self.relays
	}

	pub fn link(&mut self, a: Node, b: Node, price: Price, tie: Tie) {
		self.prices.insert(pair(a, b), price);
		if tie == Tie::Up {
			self.marked.insert(pair(a, b));
			self.upstream.insert((a, b));
		}
		self.join(a, b);
		self.net.link(a, b, price.cold, tie);
	}

	fn join(&mut self, a: Node, b: Node) {
		self.open.insert(pair(a, b));
		self.adjacent[a].insert(b);
		self.adjacent[b].insert(a);
	}

	fn part(&mut self, a: Node, b: Node) {
		self.open.remove(&pair(a, b));
		self.adjacent[a].remove(&b);
		self.adjacent[b].remove(&a);
	}

	/// Every broadcast `node` holds state for reacts next, as when its routes change.
	fn stir(&mut self, node: Node) {
		let casts: Vec<usize> = self.carry[node].keys().copied().collect();
		self.dirty.extend(casts.into_iter().map(|cast| (node, cast)));
	}

	/// Publisher `index` joins relay `home` and announces `claims` broadcasts. Returns
	/// each broadcast's index.
	pub fn publish(&mut self, index: usize, home: Node, claims: usize) -> Vec<usize> {
		let node = self.relays + index;
		let id = 100 + index as Id;
		self.net.client(node);
		self.endpoints.insert(id, node);
		self.homes.insert(id, home);
		self.prices.insert(pair(node, home), Price::flat(1));
		self.join(node, home);
		self.net.link(node, home, 1, Tie::Peer);
		self.net.publish(node, id);
		(0..claims)
			.map(|_| {
				// Numbered across publishers, so no two announce one path.
				let path = format!("live/{}", self.casts.len());
				self.net.claim(node, &path, true);
				self.casts.push((id, p(&path)));
				self.casts.len() - 1
			})
			.collect()
	}

	pub fn casts(&self) -> usize {
		self.casts.len()
	}

	fn price(&self, a: Node, b: Node) -> Price {
		self.prices[&pair(a, b)]
	}

	pub fn is_open(&self, a: Node, b: Node) -> bool {
		self.open.contains(&pair(a, b))
	}

	fn peers(&self, node: Node) -> Vec<Node> {
		self.adjacent[node].iter().copied().collect()
	}

	fn hash(&self, cast: usize, node: Node) -> u64 {
		rendezvous(&self.casts[cast].1, node as Id)
	}

	fn carry(&self, node: Node, cast: usize) -> Option<&Carry> {
		self.carry[node].get(&cast)
	}

	fn entry(&mut self, node: Node, cast: usize) -> &mut Carry {
		self.carry[node].entry(cast).or_default()
	}

	fn send(&mut self, from: Node, to: Node, lane: Lane, cast: usize, msg: Msg) {
		if !self.is_open(from, to) {
			return;
		}
		match &msg {
			Msg::Subscribe { .. } => self.sent.subscribes += 1,
			Msg::Chain { .. } => self.sent.chains += 1,
			Msg::Rank { .. } => self.sent.ranks += 1,
			Msg::Answer { .. } => self.sent.answers += 1,
			Msg::Ask { .. } => self.sent.asks += 1,
			Msg::Told { .. } => self.sent.told += 1,
			Msg::Advert(_) => self.sent.adverts += 1,
		}
		// The announce stream is one per session, whatever the broadcast.
		let stream = match &msg {
			Msg::Subscribe { stream, .. }
			| Msg::Chain { stream, .. }
			| Msg::Rank { stream, .. }
			| Msg::Answer { stream, .. }
			| Msg::Ask { stream, .. }
			| Msg::Told { stream, .. } => *stream as usize,
			Msg::Advert(_) => usize::MAX,
		};
		let at = self.net.stamp();
		self.queues
			.entry((from, to, lane, stream))
			.or_default()
			.push_back((at, cast, msg));
	}

	/// When the next advert from `from` to `to` was queued, if one waits.
	fn advert(&self, from: Node, to: Node) -> Option<u64> {
		let queue = self.queues.get(&(from, to, Lane::Announce, usize::MAX))?;
		queue.front().map(|&(at, ..)| at)
	}

	/// Whether `key`'s next message waits behind an older route message: adverts and
	/// routes share the session's one announce stream.
	fn blocked(&self, key: Key) -> bool {
		let (from, to, lane, _) = key;
		let route = self.net.head(from, to);
		lane == Lane::Announce && route.is_some_and(|route| self.advert(from, to).is_some_and(|at| route < at))
	}

	/// Whether the next route message from `from` to `to` waits behind an older advert.
	fn behind(&self, from: Node, to: Node) -> bool {
		let route = self.net.head(from, to);
		route.is_some_and(|route| self.advert(from, to).is_some_and(|at| at < route))
	}

	// --- What a relay knows ---

	/// The route layer's next hop for `cast` at `node`, the equal ones split by the
	/// rendezvous hash, and its summed cold cost.
	fn route(&self, node: Node, cast: usize) -> Option<(Node, Metric)> {
		let router = self.net.router(node)?;
		let broadcast = &self.casts[cast];
		let origin = router.pick(broadcast)?;
		let cost = router.cost(broadcast)?;
		let next = match router.step(origin, broadcast)? {
			Next::Local(endpoint) => endpoint,
			Next::Via(next) => (router.ties(origin).into_iter())
				.max_by_key(|&tie| (self.hash(cast, tie), tie))
				.unwrap_or(next),
		};
		Some((next, cost))
	}

	/// The confirmed offer `node` pulls on, unless it names `node`.
	fn live(&self, node: Node, cast: usize) -> Option<&Signal> {
		match &self.carry(node, cast)?.up.as_ref()?.state {
			State::Live(signal) if !(self.arm.hops && signal.path.contains(&node)) => Some(signal),
			_ => None,
		}
	}

	/// What `node` offers `peer` for `cast`, if it is fed.
	fn offer(&self, node: Node, cast: usize, peer: Node) -> Option<Signal> {
		if !self.relay(node) {
			return Some(Signal {
				cost: 0,
				path: Vec::new(),
				marked: false,
				rank: None,
			});
		}
		let signal = self.live(node, cast)?;
		let carry = self.carry(node, cast)?;
		let price = self.price(carry.up.as_ref()?.via, node);
		let cost = match (self.arm.mode, carry.others(peer)) {
			// A join along the route needs no cost, only the confirmation.
			(Mode::Path, _) => 0,
			// The copy is there whoever asks: what it costs reads no relay's demand, so
			// no rank's choice moves another's price.
			_ if self.arm.order.is_some() => signal.cost + price.warm,
			(_, true) => signal.cost + price.warm,
			(_, false) => signal.cost + price.cold,
		};
		let path = match self.arm.hops {
			true => signal.path.iter().copied().chain([node]).collect(),
			false => Vec::new(),
		};
		let marked = signal.marked || self.upstream.contains(&(node, carry.up.as_ref()?.via));
		let rank = self.arm.order.and_then(|_| self.rank(node, cast, Some(peer)));
		Some(Signal {
			cost,
			path,
			marked,
			rank,
		})
	}

	/// The warm signal `node` gives `peer`: its offer, while it has demand besides `peer`.
	fn warm(&self, node: Node, cast: usize, peer: Node) -> Option<Signal> {
		self.carry(node, cast).filter(|carry| carry.others(peer))?;
		self.offer(node, cast, peer)
	}

	/// The rank of a reader at `node`, with [`Arm::order`].
	fn own(&self, node: Node, cast: usize) -> Option<Rank> {
		let order = self.arm.order?;
		self.carry(node, cast).filter(|carry| carry.reader)?;
		Some(match order {
			// A reader whose relay has no route yet ranks last among them.
			Order::Route => (self.route(node, cast).map_or(Metric::MAX, |(_, cost)| cost), node),
			Order::Hash => (self.hash(cast, node), node),
		})
	}

	/// The lowest rank among the readers `node` feeds, its own and each subscription's
	/// through it, leaving out `besides`'s.
	fn rank(&self, node: Node, cast: usize, besides: Option<Node>) -> Option<Rank> {
		let carry = self.carry(node, cast)?;
		let below = (carry.down.iter())
			.filter(|&(&child, _)| Some(child) != besides)
			.flat_map(|(&child, streams)| streams.iter().map(move |&stream| (child, stream)))
			.filter_map(|key| carry.ranks.get(&key).copied().flatten());
		self.own(node, cast).into_iter().chain(below).min()
	}

	/// Every source `node` could pull `cast` from: its route, and each warm neighbour it
	/// heard of. Never a neighbour that pulls through it.
	fn options(&self, node: Node, cast: usize) -> Vec<Choice> {
		let Some(carry) = self.carry(node, cast) else {
			return Vec::new();
		};
		let mut out = Vec::new();
		if let Some((via, cost)) = self.route(node, cast)
			&& !carry.down.contains_key(&via)
		{
			out.push(Choice {
				via,
				cost,
				offered: None,
			});
		}
		let heard: Vec<(Node, &Signal)> = match self.arm.mode {
			Mode::Path => Vec::new(),
			Mode::Ask(_) => (carry.asks.iter())
				.filter_map(|(&peer, signal)| Some((peer, signal.as_ref()?)))
				.collect(),
			// Adverts, and what any upstream it asked said.
			Mode::Advertise(_) => (carry.heard.iter().map(|(&peer, signal)| (peer, signal)))
				.chain((carry.asks.iter()).filter_map(|(&peer, signal)| Some((peer, signal.as_ref()?))))
				.collect(),
		};
		// Without the chain, an ancestor's warmth may be this relay's own demand.
		// A relay that never moves for price has no reason to fence its own warmth off.
		let ancestors: Vec<Node> = match self.arm.chain || self.arm.stay || self.arm.order.is_some() {
			true => Vec::new(),
			false => self
				.live(node, cast)
				.map_or_else(Vec::new, |signal| signal.path.clone()),
		};
		// Only a copy that feeds a reader ranked below every one this relay feeds, which
		// is there whatever this relay does. Its parent's answer counts as much as an
		// advert: a parent need not advertise to a relay already pulling from it.
		let mine = self.arm.order.and_then(|_| self.rank(node, cast, None));
		let mut heard = heard;
		if self.arm.order.is_some()
			&& let Some(pull) = &carry.up
			&& let State::Live(signal) = &pull.state
			&& !heard.iter().any(|&(via, _)| via == pull.via)
		{
			heard.push((pull.via, signal));
		}
		for (via, signal) in heard {
			if !self.is_open(node, via)
				|| carry.down.contains_key(&via)
				|| ancestors.contains(&via)
				|| (self.arm.hops && signal.path.contains(&node))
				|| (self.arm.order.is_some() && !signal.rank.is_some_and(|rank| Some(rank) < mine))
			{
				continue;
			}
			out.push(Choice {
				via,
				cost: self.copy(signal, via, node),
				offered: Some(signal.cost),
			});
		}
		out
	}

	/// What `signal`'s copy at `via` costs `node`: its offer and the cold link, or with
	/// [`Arm::order`] and not [`Arm::summed`] the link alone.
	fn copy(&self, signal: &Signal, via: Node, node: Node) -> Metric {
		let link = self.price(via, node).cold;
		match self.arm.order.is_some() && !self.arm.summed {
			true => link,
			false => signal.cost + link,
		}
	}

	/// The cheapest of `options`, a warm copy before the route, then the higher hash.
	fn best<'a>(&self, cast: usize, options: &'a [Choice]) -> Option<&'a Choice> {
		(options.iter()).min_by_key(|choice| {
			(
				choice.cost,
				choice.offered.is_none(),
				Reverse(self.hash(cast, choice.via)),
				choice.via,
			)
		})
	}

	// --- What a relay does ---

	/// Bring `node`'s state for `cast` up to date with what it knows.
	fn react(&mut self, node: Node, cast: usize) {
		if !self.relay(node) {
			return;
		}
		self.queries(node, cast);
		self.pull(node, cast);
		self.chained(node, cast);
		self.ranked(node, cast);
		self.answer(node, cast);
		self.tell(node, cast);
		self.advertise(node, cast);
	}

	/// Open and close the metadata-only subscriptions that ask.
	fn queries(&mut self, node: Node, cast: usize) {
		let (count, patient) = match self.arm.mode {
			Mode::Ask(count) => (count, true),
			Mode::Advertise(Audience::NotDownAsk) => (0, false),
			Mode::Advertise(Audience::NotDownAskUp | Audience::NotDownQuery) => (usize::MAX, false),
			_ => return,
		};
		let once = self.arm.mode == Mode::Advertise(Audience::NotDownQuery);
		// A one-shot answer is spent once a pull from that upstream is answered, live or
		// lost: its subscribe answer says the rest.
		if once {
			let carry = self.entry(node, cast);
			let answered: Vec<Node> = ([&carry.up, &carry.next].into_iter().flatten())
				.filter(|pull| pull.state != State::Pending)
				.map(|pull| pull.via)
				.collect();
			for via in answered {
				if let Some(word) = carry.asks.get_mut(&via) {
					*word = None;
				}
			}
		}
		let carry = self.entry(node, cast).clone();
		let want: BTreeSet<Node> = match () {
			// Its upstreams, when its route ties between more than one.
			_ if !patient => match (carry.demand(), count) {
				(false, _) => BTreeSet::new(),
				(true, 0) => self.tied(node, cast),
				(true, _) => self.upstreams(node, cast),
			},
			_ if carry.demand() => {
				let mut peers: Vec<Node> = self.peers(node).into_iter().filter(|&peer| self.relay(peer)).collect();
				peers.sort_by_key(|&peer| (self.price(node, peer).cold, Reverse(self.hash(cast, peer)), peer));
				peers.into_iter().take(count).collect()
			}
			// Asked with no demand of its own, it forwards the subscription along its route.
			_ if !carry.asked.is_empty() => (self.route(node, cast).into_iter())
				.map(|(next, _)| next)
				.filter(|&next| self.relay(next) && !carry.asked.contains_key(&next))
				.collect(),
			_ => BTreeSet::new(),
		};
		for &gone in carry.asks.keys().filter(|peer| !want.contains(peer)) {
			let carry = self.entry(node, cast);
			carry.asks.remove(&gone);
			carry.unanswered.remove(&gone);
			// The upstream keeps nothing for a one-shot query, so there is nothing to close.
			if let Some(stream) = carry.asking.remove(&gone)
				&& !once
			{
				self.send(node, gone, Lane::Ask, cast, Msg::Ask { stream, open: false });
			}
		}
		for &new in want.iter().filter(|peer| !carry.asks.contains_key(peer)) {
			self.streams += 1;
			let stream = self.streams;
			let carry = self.entry(node, cast);
			carry.asks.insert(new, None);
			carry.asking.insert(new, stream);
			if patient {
				carry.unanswered.insert(new);
			}
			self.send(node, new, Lane::Ask, cast, Msg::Ask { stream, open: true });
		}
	}

	/// The upstreams `node`'s route to `cast` ties between, if more than one.
	fn tied(&self, node: Node, cast: usize) -> BTreeSet<Node> {
		let Some(router) = self.net.router(node) else {
			return BTreeSet::new();
		};
		let Some(origin) = router.pick(&self.casts[cast]) else {
			return BTreeSet::new();
		};
		let tied: BTreeSet<Node> = (router.ties(origin).into_iter())
			.filter(|&peer| self.upstream.contains(&(node, peer)))
			.collect();
		match tied.len() > 1 {
			true => tied,
			false => BTreeSet::new(),
		}
	}

	/// The upstreams of `node` besides the one its path join to `cast` takes.
	fn upstreams(&self, node: Node, cast: usize) -> BTreeSet<Node> {
		let next = self.route(node, cast).map(|(next, _)| next);
		(self.peers(node).into_iter())
			.filter(|&peer| self.upstream.contains(&(node, peer)) && Some(peer) != next)
			.collect()
	}

	fn subscribe(&mut self, node: Node, cast: usize, via: Node) -> Pull {
		self.streams += 1;
		let stream = self.streams;
		let chain = match self.arm.chain {
			true => (self.carry(node, cast)).map_or_else(|| vec![node], |carry| carry.chain(node)),
			false => Vec::new(),
		};
		let rank = self.rank(node, cast, Some(via));
		let open = Msg::Subscribe {
			stream,
			open: true,
			chain: chain.clone(),
			rank,
		};
		self.send(node, via, Lane::Pull, cast, open);
		Pull {
			via,
			stream,
			state: State::Pending,
			merged: false,
			chain,
			rank,
		}
	}

	/// Tell each parent when the lowest rank its pull feeds changes, with [`Arm::order`].
	fn ranked(&mut self, node: Node, cast: usize) {
		if self.arm.order.is_none() {
			return;
		}
		let carry = self.entry(node, cast);
		let pulls: Vec<(Node, u64, Option<Rank>)> = (carry.up.iter().chain(carry.next.iter()))
			.map(|pull| (pull.via, pull.stream, pull.rank))
			.collect();
		for (via, stream, was) in pulls {
			let rank = self.rank(node, cast, Some(via));
			if rank == was {
				continue;
			}
			let carry = self.entry(node, cast);
			for pull in [&mut carry.up, &mut carry.next].into_iter().flatten() {
				if pull.stream == stream {
					pull.rank = rank;
				}
			}
			self.send(node, via, Lane::Pull, cast, Msg::Rank { stream, rank });
		}
	}

	/// Tell each parent when the relays whose demand alone its pull carries change.
	fn chained(&mut self, node: Node, cast: usize) {
		// A join along the route prices nothing, so it needs no chain.
		if self.arm.mode == Mode::Path || !self.arm.chain {
			return;
		}
		let carry = self.entry(node, cast);
		let chain = carry.chain(node);
		let mut told = Vec::new();
		for pull in [&mut carry.up, &mut carry.next].into_iter().flatten() {
			if pull.chain != chain {
				pull.chain = chain.clone();
				told.push((pull.via, pull.stream));
			}
		}
		for (via, stream) in told {
			let chain = chain.clone();
			self.send(node, via, Lane::Pull, cast, Msg::Chain { stream, chain });
		}
	}

	fn unsubscribe(&mut self, node: Node, cast: usize, pull: Option<Pull>) {
		if let Some(pull) = pull {
			let stream = pull.stream;
			let close = Msg::Subscribe {
				stream,
				open: false,
				chain: Vec::new(),
				rank: None,
			};
			self.send(node, pull.via, Lane::Pull, cast, close);
		}
	}

	/// Pull from the source the rule picks, moving make before break.
	fn pull(&mut self, node: Node, cast: usize) {
		let hops = self.arm.hops;
		let carry = self.entry(node, cast);
		if !carry.demand() {
			let (up, next) = (carry.up.take(), carry.next.take());
			self.unsubscribe(node, cast, up);
			self.unsubscribe(node, cast, next);
			return;
		}
		// A move completes once answered, and is dropped if refused.
		if let Some(next) = &carry.next {
			match &next.state {
				State::Live(signal) if !(hops && signal.path.contains(&node)) => {
					let old = std::mem::replace(&mut carry.up, carry.next.take());
					self.unsubscribe(node, cast, old);
				}
				State::Pending => {}
				_ => {
					let next = carry.next.take();
					self.unsubscribe(node, cast, next);
				}
			}
		}
		let options = self.options(node, cast);
		let best = self.best(cast, &options).cloned();
		let carry = self.entry(node, cast);
		let Some(up) = carry.up.clone() else {
			// It asks before it pulls, so the first answers can steer the first pull.
			if let Some(best) = best.filter(|_| carry.unanswered.is_empty()) {
				let pull = self.subscribe(node, cast, best.via);
				let route = self.route(node, cast);
				let carry = self.entry(node, cast);
				carry.up = Some(pull);
				carry.fresh.clear();
				carry.route = route;
			}
			return;
		};
		if self.arm.stay {
			self.stay(node, cast, &up, &options, best);
			return;
		}
		if self.arm.order.is_some() {
			self.ordered(node, cast, &up, &options, best);
			return;
		}
		let moving = self.entry(node, cast).next.as_ref().map(|next| next.via);
		let target = match &up.state {
			// Nothing flows yet, so a better source replaces it outright.
			State::Pending => {
				let current = (options.iter())
					.find(|choice| choice.via == up.via)
					.map(|choice| choice.cost);
				match best {
					Some(best) if best.via != up.via && current.is_none_or(|cost| best.cost < cost) => {
						let pull = self.subscribe(node, cast, best.via);
						let old = self.entry(node, cast).up.replace(pull);
						self.unsubscribe(node, cast, old);
					}
					// A request whose source is gone fails closed: two relays that asked
					// each other at once would otherwise wait on each other for good.
					None => {
						let old = self.entry(node, cast).up.take();
						self.unsubscribe(node, cast, old);
					}
					Some(_) => {}
				}
				return;
			}
			State::Live(signal) if !(hops && signal.path.contains(&node)) => match self.arm.mode {
				// A join follows its route wherever it goes.
				Mode::Path => best.map(|best| best.via),
				_ => {
					let own = signal.cost + self.price(up.via, node).warm;
					let back = signal.cost + self.price(up.via, node).cold;
					let merge = self.merge(node, cast, &options, own, back);
					// A move under way stays while it is no dearer: its own subscription
					// shares the old parent, which lowers what staying costs, and dropping
					// it for that would raise it again.
					let moving = (moving)
						.and_then(|via| options.iter().find(|choice| choice.via == via))
						.filter(|choice| choice.cost <= own)
						.map(|choice| choice.via);
					match (best, merge) {
						(Some(best), _) if best.cost < own => Some(best.via),
						(_, Some(merge)) => {
							self.toward(node, cast, merge, true);
							return;
						}
						_ => moving.or(Some(up.via)),
					}
				}
			},
			// Lost, or fed through itself: anything usable beats it.
			_ => best.map(|best| best.via),
		};
		self.toward(node, cast, target.unwrap_or(up.via), false);
	}

	/// The relay `node` merges onto: a warm neighbour whose copy costs what its own does,
	/// with a higher hash, once nothing else, its old parent included, is cheaper. With
	/// [`Arm::hops`], never a sibling under a parent other than the publisher's relay:
	/// leaving that parent raises its offer to the sibling, which the merge relied on.
	fn merge(&self, node: Node, cast: usize, options: &[Choice], own: Metric, back: Metric) -> Option<Node> {
		if !self.arm.merge {
			return None;
		}
		let mine = self.hash(cast, node);
		let up = self.carry(node, cast)?.up.as_ref()?.via;
		let home = self.home(cast);
		let carry = self.carry(node, cast)?;
		let sibling = |via: Node| {
			let signal = match self.arm.mode {
				Mode::Ask(_) => carry.asks.get(&via).cloned().flatten(),
				_ => carry.heard.get(&via).cloned(),
			};
			up != home && signal.is_some_and(|signal| signal.path.contains(&up))
		};
		let target = (options.iter())
			.filter(|choice| choice.via != up && choice.offered == Some(own) && self.hash(cast, choice.via) > mine)
			.filter(|choice| !sibling(choice.via))
			.max_by_key(|choice| self.hash(cast, choice.via))?;
		let after = own + self.price(target.via, node).warm;
		let stays = after <= back && (options.iter()).all(|choice| choice.via == target.via || after <= choice.cost);
		stays.then_some(target.via)
	}

	/// [`Arm::stay`]'s rule for a relay already pulling: a pending pull waits for its
	/// answer, failing closed only if its source left; a live one moves only for a
	/// query answer from its join, or a choice [`Arm::rejoin`] or [`Arm::reroute`]
	/// grants, to something strictly cheaper, judged once, keeping the old pull until
	/// the new one is live; a lost one, or one through itself, is replaced by a fresh
	/// choice.
	fn stay(&mut self, node: Node, cast: usize, up: &Pull, options: &[Choice], best: Option<Choice>) {
		let hops = self.arm.hops;
		let reroute = self.arm.reroute;
		let route = self.route(node, cast);
		let carry = self.entry(node, cast);
		let moving = carry.next.as_ref().map(|next| next.via);
		let fresh = std::mem::take(&mut carry.fresh);
		let mut rejoin = std::mem::take(&mut carry.rejoin);
		if reroute && carry.route != route && up.state != State::Pending {
			carry.route = route;
			rejoin = true;
		}
		match &up.state {
			State::Pending => {
				carry.fresh = fresh;
				carry.rejoin = rejoin;
				if !options.iter().any(|choice| choice.via == up.via) {
					let old = self.entry(node, cast).up.take();
					self.unsubscribe(node, cast, old);
				}
			}
			State::Live(signal) if !(hops && signal.path.contains(&node)) => {
				let own = signal.cost + self.price(up.via, node).warm;
				let answer = (options.iter())
					.filter(|choice| fresh.contains(&choice.via) && choice.offered.is_some() && choice.cost < own)
					.min_by_key(|choice| (choice.cost, Reverse(self.hash(cast, choice.via)), choice.via));
				// A reader starting is a join: the best source now, if strictly cheaper.
				let rejoined = best.filter(|best| rejoin && best.via != up.via && best.cost < own);
				let target = (answer.map(|choice| choice.via))
					.or(rejoined.map(|best| best.via))
					.or(moving)
					.unwrap_or(up.via);
				self.toward(node, cast, target, false);
			}
			// Nothing flows from a lost pull, so a new one replaces it outright, and fails
			// closed like any pending pull: two relays that each turn to the other on stale
			// adverts would otherwise wait on each other for good.
			_ => {
				let (old, next) = (carry.up.take(), carry.next.take());
				self.unsubscribe(node, cast, old);
				self.unsubscribe(node, cast, next);
				if let Some(best) = best {
					let pull = self.subscribe(node, cast, best.via);
					let carry = self.entry(node, cast);
					carry.up = Some(pull);
					carry.route = route;
				}
			}
		}
	}

	/// [`Arm::order`]'s rule for a relay already pulling, from its route and the copies
	/// feeding a lower rank. A live pull heads for the best of them, and with
	/// [`Arm::summed`] stays while still among them unless another is strictly cheaper
	/// than it, priced at the warm link it already flows over; it keeps flowing until
	/// the new one is live. A pending
	/// pull is replaced outright by a better one, and a lost one, or one through itself, by
	/// a fresh choice. With no source left it lets go, so a neighbour pulling through it
	/// that is its only way out chooses again.
	fn ordered(&mut self, node: Node, cast: usize, up: &Pull, options: &[Choice], best: Option<Choice>) {
		let hops = self.arm.hops;
		match (&up.state, best) {
			(State::Live(signal), Some(best)) if !(hops && signal.path.contains(&node)) => {
				let own = signal.cost + self.price(up.via, node).warm;
				let kept = self.arm.summed && options.iter().any(|choice| choice.via == up.via);
				let target = match kept && best.cost >= own {
					true => up.via,
					false => best.via,
				};
				self.toward(node, cast, target, false);
			}
			(State::Pending, Some(best)) if best.via == up.via => {}
			(_, best) => {
				let carry = self.entry(node, cast);
				let (old, next) = (carry.up.take(), carry.next.take());
				self.unsubscribe(node, cast, old);
				self.unsubscribe(node, cast, next);
				if let Some(best) = best {
					let pull = self.subscribe(node, cast, best.via);
					self.entry(node, cast).up = Some(pull);
				}
			}
		}
	}

	/// Move toward `target` unless already there or on the way, marking a new pull the
	/// merge asked for.
	fn toward(&mut self, node: Node, cast: usize, target: Node, merged: bool) {
		let carry = self.entry(node, cast);
		let up = carry.up.as_ref().map(|pull| pull.via);
		let next = carry.next.as_ref().map(|pull| pull.via);
		if up == Some(target) {
			let next = carry.next.take();
			self.unsubscribe(node, cast, next);
			return;
		}
		if next == Some(target) {
			return;
		}
		let old = self.entry(node, cast).next.take();
		self.unsubscribe(node, cast, old);
		let pull = Pull {
			merged,
			..self.subscribe(node, cast, target)
		};
		self.entry(node, cast).next = Some(pull);
	}

	/// Tell each neighbour pulling through `node` what it offers now.
	fn answer(&mut self, node: Node, cast: usize) {
		let carry = self.entry(node, cast).clone();
		// Neither pulling nor trying to, nor waiting on asks: a new subscriber hears so
		// at once.
		let stuck = carry.unanswered.is_empty()
			&& carry.next.is_none()
			&& carry.up.as_ref().is_none_or(|pull| pull.state == State::Lost);
		for (&child, streams) in &carry.down {
			let offer = self.offer(node, cast, child);
			for &stream in streams {
				let was = carry.answered.get(&(child, stream));
				// Unanswered until confirmed or lost, then every change.
				if (was.is_none() && offer.is_none() && !stuck) || was == Some(&offer) {
					continue;
				}
				self.entry(node, cast).answered.insert((child, stream), offer.clone());
				let offer = offer.clone();
				self.send(node, child, Lane::Pull, cast, Msg::Answer { stream, offer });
			}
		}
	}

	/// Answer each neighbour asking `node`, and each change.
	fn tell(&mut self, node: Node, cast: usize) {
		let carry = self.entry(node, cast).clone();
		for (&asker, streams) in &carry.asked {
			let word = self.warm(node, cast, asker);
			for &stream in streams {
				if carry.told.get(&(asker, stream)) == Some(&word) {
					continue;
				}
				self.entry(node, cast).told.insert((asker, stream), word.clone());
				let word = word.clone();
				self.send(node, asker, Lane::Ask, cast, Msg::Told { stream, word });
				// A one-shot query is answered once and forgotten.
				if self.arm.mode == Mode::Advertise(Audience::NotDownQuery) {
					let carry = self.entry(node, cast);
					close(&mut carry.asked, asker, stream);
					carry.told.remove(&(asker, stream));
				}
			}
		}
	}

	/// Tell every neighbour whether `node` is warm, on a change.
	fn advertise(&mut self, node: Node, cast: usize) {
		let Mode::Advertise(audience) = self.arm.mode else {
			return;
		};
		let peers: Vec<Node> = self.peers(node).into_iter().filter(|&peer| self.relay(peer)).collect();
		for peer in peers {
			let want = self
				.warm(node, cast, peer)
				.filter(|signal| self.scoped(audience, node, cast, peer, signal));
			let was = self.entry(node, cast).advertised.get(&peer).cloned();
			if was == want {
				continue;
			}
			match &want {
				Some(signal) => self.entry(node, cast).advertised.insert(peer, signal.clone()),
				None => self.entry(node, cast).advertised.remove(&peer),
			};
			self.send(node, peer, Lane::Announce, cast, Msg::Advert(want));
		}
	}

	/// Whether `audience` lets `node` advertise `signal` to `peer`.
	fn scoped(&self, audience: Audience, node: Node, cast: usize, peer: Node, signal: &Signal) -> bool {
		// Priced from the publisher, even where the taker prices it at the link alone: a
		// copy at the link alone beats nearly any route, so a warm core would tell every
		// core it meshes with. A relay already pulling keeps its parent by its answer.
		let price = signal.cost + self.price(node, peer).cold;
		let heard = self
			.net
			.router(node)
			.and_then(|router| router.heard(&self.casts[cast], peer));
		let useful = || match heard {
			Some((cost, false)) => price <= cost,
			// Its route runs through this relay, which a neighbour offers nobody back: it
			// costs this relay's route and the link, so only an offer below that route
			// can beat it.
			_ => self.route(node, cast).is_none_or(|(_, cost)| signal.cost < cost),
		};
		match audience {
			Audience::All => true,
			Audience::Useful => useful(),
			Audience::NotNext => matches!(heard, Some((cost, false)) if price <= cost),
			Audience::NotDown | Audience::NotDownAsk | Audience::NotDownAskUp | Audience::NotDownQuery => {
				!self.upstream.contains(&(peer, node))
					&& !(self.upstream.contains(&(node, peer)) && signal.marked)
					&& useful()
			}
			Audience::Shared => self.carry(node, cast).is_some_and(|carry| {
				let children = carry.down.keys().filter(|&&child| child != peer).count();
				usize::from(carry.reader) + children >= 2
			}),
			Audience::Down => !self.marked.contains(&pair(node, peer)),
		}
	}

	/// Each relay whose inputs changed reacts, in relay order, until it settles; a
	/// relay with no state for a broadcast has nothing to do for it.
	fn step(&mut self) {
		while let Some((node, cast)) = self.dirty.pop_first() {
			self.changed.insert(cast);
			for _ in 0..8 {
				let was = self.carry(node, cast).cloned();
				self.react(node, cast);
				if self.carry(node, cast) == was.as_ref() {
					break;
				}
			}
			if self.carry(node, cast).is_some_and(|carry| *carry == Carry::default()) {
				self.carry[node].remove(&cast);
			}
			let members = self.members.entry(cast).or_default();
			match self.carry[node].contains_key(&cast) {
				true => members.insert(node),
				false => members.remove(&node),
			};
		}
	}

	fn recv(&mut self, from: Node, to: Node, cast: usize, msg: Msg) {
		self.dirty.insert((to, cast));
		let stay = self.arm.stay;
		// A publisher answers at once.
		if let Msg::Subscribe { stream, open: true, .. } = msg
			&& !self.relay(to)
		{
			let offer = self.offer(to, cast, from);
			self.send(to, from, Lane::Pull, cast, Msg::Answer { stream, offer });
		}
		let carry = self.entry(to, cast);
		match msg {
			Msg::Subscribe {
				stream,
				open: true,
				chain,
				rank,
			} => {
				carry.down.entry(from).or_default().insert(stream);
				carry.chains.insert((from, stream), chain);
				carry.ranks.insert((from, stream), rank);
			}
			Msg::Rank { stream, rank } => {
				if carry.down.get(&from).is_some_and(|streams| streams.contains(&stream)) {
					carry.ranks.insert((from, stream), rank);
				}
			}
			// Only while the subscription is open: it may have closed since.
			Msg::Chain { stream, chain } => {
				if carry.down.get(&from).is_some_and(|streams| streams.contains(&stream)) {
					carry.chains.insert((from, stream), chain);
				}
			}
			Msg::Subscribe {
				stream, open: false, ..
			} => {
				close(&mut carry.down, from, stream);
				carry.answered.remove(&(from, stream));
				carry.chains.remove(&(from, stream));
				carry.ranks.remove(&(from, stream));
			}
			Msg::Answer { stream, offer } => {
				for pull in [&mut carry.up, &mut carry.next].into_iter().flatten() {
					if (pull.via, pull.stream) == (from, stream) {
						pull.state = match &offer {
							Some(signal) => State::Live(signal.clone()),
							None => State::Lost,
						};
					}
				}
			}
			Msg::Ask { stream, open: true } => {
				carry.asked.entry(from).or_default().insert(stream);
			}
			Msg::Ask { stream, open: false } => {
				close(&mut carry.asked, from, stream);
				carry.told.remove(&(from, stream));
			}
			Msg::Told { stream, word } => {
				if carry.asking.get(&from) == Some(&stream)
					&& let Some(heard) = carry.asks.get_mut(&from)
				{
					if stay && word.is_some() {
						carry.fresh.insert(from);
					}
					*heard = word;
					carry.unanswered.remove(&from);
				}
			}
			Msg::Advert(Some(signal)) => {
				carry.heard.insert(from, signal);
			}
			Msg::Advert(None) => {
				carry.heard.remove(&from);
			}
		}
	}

	// --- What the network does ---

	/// A reader at `node` starts or stops reading broadcast `cast`.
	pub fn read(&mut self, node: Node, cast: usize, reading: bool) {
		let round = self.round;
		let rejoin = self.arm.rejoin;
		let carry = self.entry(node, cast);
		carry.rejoin |= rejoin && reading && !carry.reader && carry.up.is_some();
		carry.reader = reading;
		carry.since = reading.then_some(round);
		// A reader at a relay already fed has its first frame at once, whether or not
		// anything is sent.
		if reading && self.feeds(node, cast) {
			self.entry(node, cast).since = None;
			self.waits.push(0);
		}
		self.dirty.insert((node, cast));
		self.step();
	}

	pub fn reading(&self, node: Node, cast: usize) -> bool {
		self.carry(node, cast).is_some_and(|carry| carry.reader)
	}

	/// The session between `a` and `b` closes, and both notice.
	pub fn close(&mut self, a: Node, b: Node) {
		self.part(a, b);
		self.net.close(a, b);
		self.queues.retain(|&(from, to, ..), _| pair(from, to) != pair(a, b));
		for (x, y) in [(a, b), (b, a)] {
			for carry in self.carry[x].values_mut() {
				if carry.up.as_ref().is_some_and(|pull| pull.via == y) {
					// A move under way becomes the pull; otherwise it went with the session.
					carry.up = carry.next.take();
				}
				if carry.next.as_ref().is_some_and(|pull| pull.via == y) {
					carry.next = None;
				}
				carry.down.remove(&y);
				carry.answered.retain(|&(child, _), _| child != y);
				carry.chains.retain(|&(child, _), _| child != y);
				carry.ranks.retain(|&(child, _), _| child != y);
				carry.asks.remove(&y);
				carry.unanswered.remove(&y);
				carry.asking.remove(&y);
				carry.asked.remove(&y);
				carry.told.retain(|&(asker, _), _| asker != y);
				carry.heard.remove(&y);
				carry.advertised.remove(&y);
			}
			self.stir(x);
		}
		self.step();
	}

	pub fn reopen(&mut self, a: Node, b: Node) {
		self.join(a, b);
		self.net.open(a, b);
		self.stir(a);
		self.stir(b);
		self.step();
	}

	/// `node` restarts: every session of its closes, it forgets everything but who reads
	/// there, and its sessions reopen.
	pub fn restart(&mut self, node: Node) {
		let peers = self.peers(node);
		for &peer in &peers {
			self.close(node, peer);
		}
		self.net.reset(node);
		for carry in self.carry[node].values_mut() {
			*carry = Carry {
				reader: carry.reader,
				since: carry.since,
				..Carry::default()
			};
		}
		self.stir(node);
		for peer in peers {
			self.reopen(node, peer);
		}
	}

	/// Deliver everything queued now, in a seeded random order: a hop of delay. False
	/// if nothing was.
	pub fn round(&mut self) -> bool {
		#[derive(Clone, Copy)]
		enum Queue {
			Route(Node, Node),
			Data(Key),
		}
		let mut ready: Vec<(Queue, usize)> = (self.net.ready().into_iter())
			.map(|((from, to), count)| (Queue::Route(from, to), count))
			.chain(
				(self.queues.iter())
					.filter(|(_, queue)| !queue.is_empty())
					.map(|(&key, queue)| (Queue::Data(key), queue.len())),
			)
			.collect();
		if ready.is_empty() {
			return false;
		}
		for i in (1..ready.len()).rev() {
			ready.swap(i, self.order.next(i as u64 + 1) as usize);
		}
		// A queue whose next message waits behind an older one on the same announce
		// stream takes its turn on a later pass, once that one is delivered.
		loop {
			let mut moved = false;
			for (queue, count) in &mut ready {
				while *count > 0 {
					let blocked = match *queue {
						Queue::Route(from, to) => self.behind(from, to),
						Queue::Data(key) => self.blocked(key),
					};
					if blocked {
						break;
					}
					*count -= 1;
					moved = true;
					match *queue {
						Queue::Route(from, to) => {
							self.net.deliver(from, to);
							self.stir(to);
						}
						Queue::Data(key) => {
							let Some((cast, msg)) = self.pop(key) else {
								*count = 0;
								break;
							};
							self.recv(key.0, key.1, cast, msg);
						}
					}
					self.step();
					self.observe();
				}
			}
			if !moved {
				break;
			}
		}
		self.net.flush();
		self.step();
		self.round += 1;
		self.tally();
		true
	}

	/// The next message on a queue, dropping the queue once empty.
	fn pop(&mut self, key: Key) -> Option<(usize, Msg)> {
		let queue = self.queues.get_mut(&key)?;
		let next = queue.pop_front().map(|(_, cast, msg)| (cast, msg));
		if queue.is_empty() {
			self.queues.remove(&key);
		}
		next
	}

	/// Deliver in rounds until nothing is left. False if that never happens, or once
	/// it spends [`WarmSim::budget`].
	pub fn settle(&mut self) -> bool {
		let start = self.watch.steps;
		for _ in 0..2_000 {
			if self.budget.is_some_and(|budget| self.watch.steps - start > budget) {
				return false;
			}
			if !self.round() {
				return true;
			}
		}
		false
	}

	/// Deliver one message from a queue `pick` chooses, as churn does. False if none waits.
	pub fn deliver(&mut self, pick: &mut XorShift) -> bool {
		let routes: Vec<_> = (self.net.ready().into_iter())
			.filter(|&((from, to), _)| !self.behind(from, to))
			.collect();
		let data: Vec<Key> = (self.queues.iter())
			.filter(|&(&key, queue)| !queue.is_empty() && !self.blocked(key))
			.map(|(&key, _)| key)
			.collect();
		let total = routes.len() + data.len();
		if total == 0 {
			return false;
		}
		let at = pick.next(total as u64) as usize;
		match at < routes.len() {
			true => {
				let ((from, to), _) = routes[at];
				self.net.deliver(from, to);
				self.net.flush();
				self.stir(to);
			}
			false => {
				let key = data[at - routes.len()];
				let (cast, msg) = self.pop(key).expect("a message");
				self.recv(key.0, key.1, cast, msg);
			}
		}
		self.step();
		self.observe();
		true
	}

	/// A hop of delay passes for churn, which delivers one message at a time: count
	/// unfed readers and first frames.
	pub fn tick(&mut self) {
		self.round += 1;
		self.tally();
	}

	/// Each relay's state for every broadcast, for a failure report.
	pub fn dump(&self) -> String {
		let mut out = String::new();
		for node in 0..self.relays {
			for (cast, carry) in &self.carry[node] {
				let route = self.route(node, *cast);
				out += &format!("  {node} #{cast} route {route:?}: {carry:?}\n");
			}
		}
		out += &format!("  open {:?}\n", self.open);
		for node in 0..self.relays {
			let router = self.net.router(node).expect("a relay");
			let picks: Vec<_> = self.casts.iter().map(|cast| router.pick(cast)).collect();
			out += &format!("  {node} picks {picks:?}\n");
		}
		out
	}

	// --- What is true ---

	/// Whether `node` is fed `cast` from a publisher still live.
	pub fn feeds(&self, node: Node, cast: usize) -> bool {
		self.fed(node, cast) && self.live(node, cast).is_some()
	}

	/// Whether `node` is fed `cast`: its pulls lead to the publisher, each received.
	pub fn fed(&self, node: Node, cast: usize) -> bool {
		let mut at = node;
		for _ in 0..=self.nodes {
			if !self.relay(at) {
				return true;
			}
			let Some(pull) = self.carry(at, cast).and_then(|carry| carry.up.as_ref()) else {
				return false;
			};
			let held = self.carry(pull.via, cast).is_some_and(|carry| {
				carry
					.down
					.get(&at)
					.is_some_and(|streams| streams.contains(&pull.stream))
			});
			if !held || !self.is_open(at, pull.via) {
				return false;
			}
			at = pull.via;
		}
		false
	}

	fn parent(&self, node: Node, cast: usize) -> Option<Node> {
		Some(self.carry(node, cast)?.up.as_ref()?.via)
	}

	/// Whether following `node`'s pulls for `cast` goes round a cycle.
	fn circles(&self, node: Node, cast: usize) -> bool {
		self.cycle(node, cast).is_some()
	}

	/// A relay on the cycle `node`'s pulls for `cast` run into, if they do.
	fn cycle(&self, node: Node, cast: usize) -> Option<Node> {
		let mut seen = BTreeSet::new();
		let mut at = node;
		while seen.insert(at) {
			at = self.parent(at, cast)?;
		}
		Some(at)
	}

	/// Whether the cycle `node`'s pulls for `cast` run into holds a pull the merge made.
	fn merged(&self, node: Node, cast: usize) -> bool {
		let Some(start) = self.cycle(node, cast) else {
			return false;
		};
		let mut at = start;
		loop {
			let Some(pull) = self.carry(at, cast).and_then(|carry| carry.up.as_ref()) else {
				return false;
			};
			if pull.merged {
				return true;
			}
			at = pull.via;
			if at == start {
				return false;
			}
		}
	}

	fn readers(&self) -> Vec<(Node, usize)> {
		(0..self.relays)
			.flat_map(|node| {
				self.carry[node]
					.iter()
					.filter(|(_, carry)| carry.reader)
					.map(move |(&cast, _)| (node, cast))
			})
			.collect()
	}

	/// Count a cycle after a delivery, looking again only at broadcasts whose pulls
	/// changed.
	fn observe(&mut self) {
		self.watch.steps += 1;
		for cast in std::mem::take(&mut self.changed) {
			let members = self.members.get(&cast).into_iter().flatten();
			let circling: Vec<Node> = members.copied().filter(|&node| self.circles(node, cast)).collect();
			match circling.is_empty() {
				true => self.circling.remove(&cast),
				false => {
					let starved = circling.iter().any(|&node| self.reading(node, cast));
					let merged = circling.iter().any(|&node| self.merged(node, cast));
					self.circling.insert(cast, (starved, merged))
				}
			};
		}
		let cycle = !self.circling.is_empty();
		let starved = self.circling.values().any(|&(starved, _)| starved);
		let merged = self.circling.values().any(|&(_, merged)| merged);
		self.watch.cycles += usize::from(cycle);
		self.watch.merged += usize::from(merged);
		self.watch.starved += usize::from(starved);
		self.watch.run = if cycle { self.watch.run + 1 } else { 0 };
		self.watch.longest = self.watch.longest.max(self.watch.run);
	}

	/// The relays joined to `cast`'s publisher over open links through relays, whether
	/// or not their route has arrived yet.
	fn connected(&self, cast: usize) -> BTreeSet<Node> {
		let start = self.endpoints[&self.casts[cast].0];
		let mut seen = BTreeSet::from([start]);
		let mut queue = vec![start];
		while let Some(at) = queue.pop() {
			for peer in self.peers(at) {
				if self.relay(peer) && seen.insert(peer) {
					queue.push(peer);
				}
			}
		}
		seen
	}

	/// The relays the route layer gives a route to `cast`'s publisher, which a pull
	/// can follow once settled. A path over open links is not enough: a relay announces
	/// only its best route, and never one that came down a marked link back up one.
	fn reach(&self, cast: usize) -> BTreeSet<Node> {
		let start = self.endpoints[&self.casts[cast].0];
		(0..self.relays)
			.filter(|&node| node == start || self.route(node, cast).is_some())
			.collect()
	}

	/// Count unfed readers as a round ends, and the first frames that arrived. A reader
	/// counts while joined over open links, so a late route is an outage as much as a
	/// late copy; one the route layer never routes counts alike for every arm.
	fn tally(&mut self) {
		let round = self.round;
		let mut reach: BTreeMap<Node, BTreeSet<Node>> = BTreeMap::new();
		for (node, cast) in self.readers() {
			let fed = self.feeds(node, cast);
			let start = self.endpoints[&self.casts[cast].0];
			let path = reach
				.entry(start)
				.or_insert_with(|| self.connected(cast))
				.contains(&node);
			self.watch.unfed += usize::from(path && !fed);
			let carry = self.entry(node, cast);
			if fed && let Some(since) = carry.since.take() {
				self.waits.push(round - since);
			}
		}
		for cast in 0..self.casts.len() {
			let home = self.home(cast);
			let copies = self.carry(home, cast).map_or(0, |carry| carry.down.len());
			self.watch.extra += copies.saturating_sub(1);
		}
	}

	/// Every reader with a route to its publisher is fed, no pulls circle, and nothing is
	/// left to deliver.
	pub fn check(&self) -> Result<(), String> {
		if self.queues.values().any(|queue| !queue.is_empty()) || !self.net.ready().is_empty() {
			return Err("still busy".into());
		}
		for cast in 0..self.casts.len() {
			let reach = self.reach(cast);
			for node in 0..self.relays {
				if self.circles(node, cast) {
					return Err(format!("{}: broadcast {cast} circles at relay {node}", self.arm.name()));
				}
				if self.reading(node, cast) && reach.contains(&node) && !self.feeds(node, cast) {
					return Err(format!("{}: relay {node} unfed for broadcast {cast}", self.arm.name()));
				}
				let carry = self.carry(node, cast);
				if carry.is_some_and(|carry| !carry.demand() && carry.up.is_some()) {
					return Err(format!("{}: relay {node} pulls {cast} for nobody", self.arm.name()));
				}
			}
		}
		Ok(())
	}

	/// The warm state `node` keeps about its neighbours across every broadcast: adverts
	/// it heard and sent, or metadata-only subscriptions it holds and serves.
	pub fn held(&self, node: Node) -> usize {
		(self.carry[node].values())
			.map(|carry| carry.heard.len() + carry.advertised.len() + carry.asks.len() + carry.asked.len())
			.sum()
	}

	/// The most relay neighbours any relay has.
	pub fn degree(&self) -> usize {
		(0..self.relays)
			.map(|node| self.adjacent[node].iter().filter(|&&peer| self.relay(peer)).count())
			.max()
			.unwrap_or(0)
	}

	/// Relays besides the publisher's pulling `cast` with no reader of their own: what
	/// other relays' joins cost them.
	pub fn carriers(&self, cast: usize) -> Vec<Node> {
		let home = self.home(cast);
		(0..self.relays)
			.filter(|&node| node != home)
			.filter(|&node| {
				self.carry(node, cast)
					.is_some_and(|carry| !carry.reader && carry.up.is_some())
			})
			.collect()
	}

	/// The relay publishing `cast` is joined to.
	pub fn home(&self, cast: usize) -> Node {
		self.homes[&self.casts[cast].0]
	}

	pub fn tree(&self, cast: usize) -> Tree {
		let home = self.home(cast);
		let edges: Vec<(Node, Node)> = (0..self.relays)
			.filter(|&node| self.fed(node, cast))
			.filter_map(|node| Some((self.parent(node, cast)?, node)))
			.collect();
		let mut links: BTreeMap<(Node, Node), usize> = BTreeMap::new();
		for &(a, b) in &edges {
			*links.entry(pair(a, b)).or_default() += 1;
		}
		Tree {
			cost: edges.iter().map(|&(a, b)| self.price(a, b).cold).sum(),
			copies: links.values().copied().max().unwrap_or(0),
			uplinks: edges.iter().filter(|&&(parent, _)| parent == home).count(),
			edges,
		}
	}

	/// Lower each node's cost in `costs` to the cheapest reached from any node over open
	/// links at their cold prices: Dijkstra from every node at once.
	fn relax(&self, costs: &mut [Metric]) {
		let mut heap: BinaryHeap<Reverse<(Metric, Node)>> = (costs.iter().enumerate())
			.filter(|&(_, &cost)| cost < Metric::MAX / 4)
			.map(|(node, &cost)| Reverse((cost, node)))
			.collect();
		let start = costs.to_vec();
		while let Some(Reverse((cost, at))) = heap.pop() {
			// Only relays carry a copy on; an endpoint only starts or ends one.
			if cost > costs[at] || (!self.relay(at) && cost != start[at]) {
				continue;
			}
			for &peer in &self.adjacent[at] {
				let next = cost + self.price(at, peer).cold;
				if next < costs[peer] {
					costs[peer] = next;
					heap.push(Reverse((next, peer)));
				}
			}
		}
	}

	/// The cheapest tree joining `cast`'s publisher to every relay reading it that has a
	/// route or a copy, over open links at their cold prices: Dreyfus and Wagner's exact
	/// dynamic program.
	pub fn cheapest(&self, cast: usize) -> Metric {
		let endpoint = self.endpoints[&self.casts[cast].0];
		let reach = self.reach(cast);
		let mut terminals = vec![endpoint];
		terminals.extend(
			(0..self.relays)
				.filter(|&node| self.reading(node, cast) && (reach.contains(&node) || self.fed(node, cast))),
		);
		let n = self.nodes;
		let count = terminals.len();
		let full = (1usize << count) - 1;
		let mut tree = vec![vec![Metric::MAX / 4; n]; full + 1];
		for (index, &terminal) in terminals.iter().enumerate() {
			let row = &mut tree[1 << index];
			row[terminal] = 0;
			self.relax(row);
		}
		for mask in 1..=full {
			if mask.count_ones() < 2 {
				continue;
			}
			let mut sub = (mask - 1) & mask;
			while sub > 0 {
				let joined: Vec<Metric> = tree[sub].iter().zip(&tree[mask ^ sub]).map(|(a, b)| a + b).collect();
				for (slot, joined) in tree[mask].iter_mut().zip(joined) {
					*slot = (*slot).min(joined);
				}
				sub = (sub - 1) & mask;
			}
			let mut row = std::mem::take(&mut tree[mask]);
			self.relax(&mut row);
			tree[mask] = row;
		}
		tree[full][endpoint]
	}
}
