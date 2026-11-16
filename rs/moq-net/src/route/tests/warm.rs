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
	collections::{BTreeMap, BTreeSet, VecDeque},
};

use super::cache::rendezvous;
use super::compare::Arm as Rule;
use super::multi::{Broadcast, MultiSim, Next, Tie, Tune};
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
	/// A warm relay tells every neighbour, and each change, on its announce stream.
	Advertise,
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
}

impl Arm {
	pub fn name(self) -> String {
		let mode = match self.mode {
			Mode::Path => "path".to_string(),
			Mode::Ask(usize::MAX) => "ask_all".into(),
			Mode::Ask(count) => format!("ask_{count}"),
			Mode::Advertise => "advertise".into(),
		};
		let hops = if self.hops { "+hops" } else { "" };
		let merge = if self.merge { "+merge" } else { "" };
		format!("{mode}{hops}{merge}")
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

/// What a relay offers a neighbour: its summed cost and, with [`Arm::hops`], the relays
/// the data crossed, the publisher's own relay first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Signal {
	pub cost: Metric,
	pub path: Vec<Node>,
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
}

#[derive(Clone, Debug)]
enum Msg {
	Subscribe {
		stream: u64,
		open: bool,
	},
	/// A parent's answer and each change to it: its offer, or that it lost its upstream.
	Answer {
		stream: u64,
		offer: Option<Signal>,
	},
	/// The metadata-only subscription that asks, opening or closing.
	Ask(bool),
	/// The asked neighbour's word and each change to it.
	Told(Option<Signal>),
	Advert(Option<Signal>),
}

/// The stream a message travels on: each subscription and query is a stream of its
/// own, unordered against the rest, and adverts share a session's announce stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Lane {
	Pull,
	Ask,
	Announce,
}

/// A queue: sender, receiver, lane, and broadcast.
type Key = (Node, Node, Lane, usize);

/// One relay's state for one broadcast.
#[derive(Clone, Debug, Default)]
struct Carry {
	reader: bool,
	/// The round its reader started, until its first frame.
	since: Option<usize>,
	/// Neighbours pulling through this relay, by stream.
	down: BTreeMap<Node, u64>,
	up: Option<Pull>,
	/// A move under way: it replaces `up` once answered.
	next: Option<Pull>,
	/// What each neighbour pulling through it was last answered.
	answered: BTreeMap<Node, Option<Signal>>,
	/// Neighbours it asks, with each one's latest word.
	asks: BTreeMap<Node, Option<Signal>>,
	/// Those yet to answer: a relay with no pull waits for them before it starts one.
	unanswered: BTreeSet<Node>,
	asked: BTreeSet<Node>,
	told: BTreeMap<Node, Option<Signal>>,
	heard: BTreeMap<Node, Signal>,
	advertised: BTreeMap<Node, Signal>,
}

impl Carry {
	fn demand(&self) -> bool {
		self.reader || !self.down.is_empty()
	}

	/// Demand besides `peer`'s.
	fn others(&self, peer: Node) -> bool {
		self.reader || self.down.keys().any(|&child| child != peer)
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
}

impl Sent {
	pub fn total(self) -> usize {
		self.subscribes + self.answers + self.asks + self.told + self.adverts
	}

	/// What only a warm signal costs: queries, their answers, and adverts.
	pub fn warm(self) -> usize {
		self.asks + self.told + self.adverts
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

/// The tune of the route rule the bake-off picked.
fn kept() -> Tune {
	match Rule::KEPT {
		Rule::Multi(tune) => tune,
		Rule::Path(_) => unreachable!(),
	}
}

fn pair(a: Node, b: Node) -> (Node, Node) {
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
	open: BTreeSet<(Node, Node)>,
	casts: Vec<Broadcast>,
	carry: Vec<BTreeMap<usize, Carry>>,
	queues: BTreeMap<Key, VecDeque<(usize, Msg)>>,
	streams: u64,
	pub sent: Sent,
	pub watch: Watch,
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
			net: MultiSim::new(kept(), nodes),
			relays,
			nodes,
			endpoints: BTreeMap::new(),
			homes: BTreeMap::new(),
			prices: BTreeMap::new(),
			open: BTreeSet::new(),
			casts: Vec::new(),
			carry: vec![BTreeMap::new(); nodes],
			queues: BTreeMap::new(),
			streams: 0,
			sent: Sent::default(),
			watch: Watch::default(),
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
		self.open.insert(pair(a, b));
		self.net.link(a, b, price.cold, tie);
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
		self.open.insert(pair(node, home));
		self.net.link(node, home, 1, Tie::Peer);
		self.net.publish(node, id);
		(0..claims)
			.map(|claim| {
				let path = format!("live/{claim}");
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
		(self.open.iter())
			.filter_map(|&(a, b)| match node {
				_ if a == node => Some(b),
				_ if b == node => Some(a),
				_ => None,
			})
			.collect()
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
			Msg::Answer { .. } => self.sent.answers += 1,
			Msg::Ask(_) => self.sent.asks += 1,
			Msg::Told(_) => self.sent.told += 1,
			Msg::Advert(_) => self.sent.adverts += 1,
		}
		// The announce stream is one per session, whatever the broadcast.
		let stream = match lane {
			Lane::Announce => usize::MAX,
			Lane::Pull | Lane::Ask => cast,
		};
		self.queues
			.entry((from, to, lane, stream))
			.or_default()
			.push_back((cast, msg));
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
			});
		}
		let signal = self.live(node, cast)?;
		let carry = self.carry(node, cast)?;
		let price = self.price(carry.up.as_ref()?.via, node);
		let cost = match (self.arm.mode, carry.others(peer)) {
			// A join along the route needs no cost, only the confirmation.
			(Mode::Path, _) => 0,
			(_, true) => signal.cost + price.warm,
			(_, false) => signal.cost + price.cold,
		};
		let path = match self.arm.hops {
			true => signal.path.iter().copied().chain([node]).collect(),
			false => Vec::new(),
		};
		Some(Signal { cost, path })
	}

	/// The warm signal `node` gives `peer`: its offer, while it has demand besides `peer`.
	fn warm(&self, node: Node, cast: usize, peer: Node) -> Option<Signal> {
		self.carry(node, cast).filter(|carry| carry.others(peer))?;
		self.offer(node, cast, peer)
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
			Mode::Advertise => carry.heard.iter().map(|(&peer, signal)| (peer, signal)).collect(),
		};
		for (via, signal) in heard {
			if !self.is_open(node, via)
				|| carry.down.contains_key(&via)
				|| (self.arm.hops && signal.path.contains(&node))
			{
				continue;
			}
			out.push(Choice {
				via,
				cost: signal.cost + self.price(via, node).cold,
				offered: Some(signal.cost),
			});
		}
		out
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
		self.answer(node, cast);
		self.tell(node, cast);
		self.advertise(node, cast);
	}

	/// Open and close the metadata-only subscriptions that ask.
	fn queries(&mut self, node: Node, cast: usize) {
		let Mode::Ask(count) = self.arm.mode else {
			return;
		};
		let carry = self.entry(node, cast).clone();
		let want: BTreeSet<Node> = match () {
			_ if carry.demand() => {
				let mut peers: Vec<Node> = self.peers(node).into_iter().filter(|&peer| self.relay(peer)).collect();
				peers.sort_by_key(|&peer| (self.price(node, peer).cold, Reverse(self.hash(cast, peer)), peer));
				peers.into_iter().take(count).collect()
			}
			// Asked with no demand of its own, it forwards the subscription along its route.
			_ if !carry.asked.is_empty() => (self.route(node, cast).into_iter())
				.map(|(next, _)| next)
				.filter(|&next| self.relay(next) && !carry.asked.contains(&next))
				.collect(),
			_ => BTreeSet::new(),
		};
		for &gone in carry.asks.keys().filter(|peer| !want.contains(peer)) {
			let carry = self.entry(node, cast);
			carry.asks.remove(&gone);
			carry.unanswered.remove(&gone);
			self.send(node, gone, Lane::Ask, cast, Msg::Ask(false));
		}
		for &new in want.iter().filter(|peer| !carry.asks.contains_key(peer)) {
			let carry = self.entry(node, cast);
			carry.asks.insert(new, None);
			carry.unanswered.insert(new);
			self.send(node, new, Lane::Ask, cast, Msg::Ask(true));
		}
	}

	fn subscribe(&mut self, node: Node, cast: usize, via: Node) -> Pull {
		self.streams += 1;
		let stream = self.streams;
		self.send(node, via, Lane::Pull, cast, Msg::Subscribe { stream, open: true });
		Pull {
			via,
			stream,
			state: State::Pending,
			merged: false,
		}
	}

	fn unsubscribe(&mut self, node: Node, cast: usize, pull: Option<Pull>) {
		if let Some(pull) = pull {
			let stream = pull.stream;
			self.send(node, pull.via, Lane::Pull, cast, Msg::Subscribe { stream, open: false });
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
				self.entry(node, cast).up = Some(pull);
			}
			return;
		};
		let moving = carry.next.as_ref().map(|next| next.via);
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
		for (&child, &stream) in &carry.down {
			let offer = self.offer(node, cast, child);
			let was = carry.answered.get(&child);
			// Unanswered until confirmed, then every change.
			if (was.is_none() && offer.is_none()) || was == Some(&offer) {
				continue;
			}
			self.entry(node, cast).answered.insert(child, offer.clone());
			self.send(node, child, Lane::Pull, cast, Msg::Answer { stream, offer });
		}
	}

	/// Answer each neighbour asking `node`, and each change.
	fn tell(&mut self, node: Node, cast: usize) {
		let carry = self.entry(node, cast).clone();
		for &asker in &carry.asked {
			let word = self.warm(node, cast, asker);
			if carry.told.get(&asker) == Some(&word) {
				continue;
			}
			self.entry(node, cast).told.insert(asker, word.clone());
			self.send(node, asker, Lane::Ask, cast, Msg::Told(word));
		}
	}

	/// Tell every neighbour whether `node` is warm, on a change.
	fn advertise(&mut self, node: Node, cast: usize) {
		if self.arm.mode != Mode::Advertise {
			return;
		}
		let peers: Vec<Node> = self.peers(node).into_iter().filter(|&peer| self.relay(peer)).collect();
		for peer in peers {
			let want = self.warm(node, cast, peer);
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

	/// Every relay reacts to what changed.
	fn step(&mut self) {
		for node in 0..self.relays {
			for cast in 0..self.casts.len() {
				self.react(node, cast);
			}
		}
	}

	fn recv(&mut self, from: Node, to: Node, cast: usize, msg: Msg) {
		// A publisher answers at once.
		if let Msg::Subscribe { stream, open: true } = msg
			&& !self.relay(to)
		{
			let offer = self.offer(to, cast, from);
			self.send(to, from, Lane::Pull, cast, Msg::Answer { stream, offer });
		}
		let carry = self.entry(to, cast);
		match msg {
			Msg::Subscribe { stream, open: true } => {
				carry.down.insert(from, stream);
				carry.answered.remove(&from);
			}
			Msg::Subscribe { stream, open: false } => {
				if carry.down.get(&from) == Some(&stream) {
					carry.down.remove(&from);
					carry.answered.remove(&from);
				}
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
			Msg::Ask(true) => {
				carry.asked.insert(from);
			}
			Msg::Ask(false) => {
				carry.asked.remove(&from);
				carry.told.remove(&from);
			}
			Msg::Told(word) => {
				if let Some(heard) = carry.asks.get_mut(&from) {
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
		let carry = self.entry(node, cast);
		carry.reader = reading;
		carry.since = reading.then_some(round);
		self.step();
	}

	pub fn reading(&self, node: Node, cast: usize) -> bool {
		self.carry(node, cast).is_some_and(|carry| carry.reader)
	}

	/// The session between `a` and `b` closes, and both notice.
	pub fn close(&mut self, a: Node, b: Node) {
		self.open.remove(&pair(a, b));
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
				carry.answered.remove(&y);
				carry.asks.remove(&y);
				carry.unanswered.remove(&y);
				carry.asked.remove(&y);
				carry.told.remove(&y);
				carry.heard.remove(&y);
				carry.advertised.remove(&y);
			}
		}
		self.step();
	}

	pub fn reopen(&mut self, a: Node, b: Node) {
		self.open.insert(pair(a, b));
		self.net.open(a, b);
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
		for peer in peers {
			self.reopen(node, peer);
		}
	}

	/// Deliver everything queued now, in a seeded random order: a hop of delay. False
	/// if nothing was.
	pub fn round(&mut self) -> bool {
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
		for (queue, count) in ready {
			for _ in 0..count {
				match queue {
					Queue::Route(from, to) => {
						self.net.deliver(from, to);
					}
					Queue::Data(key) => {
						let Some((cast, msg)) = self.queues.get_mut(&key).and_then(VecDeque::pop_front) else {
							break;
						};
						self.recv(key.0, key.1, cast, msg);
					}
				}
				self.step();
				self.observe();
			}
		}
		self.net.flush();
		self.step();
		self.round += 1;
		self.tally();
		true
	}

	/// Deliver in rounds until nothing is left. False if that never happens.
	pub fn settle(&mut self) -> bool {
		for _ in 0..2_000 {
			if !self.round() {
				return true;
			}
		}
		false
	}

	/// Deliver one message from a queue `pick` chooses, as churn does. False if none waits.
	pub fn deliver(&mut self, pick: &mut XorShift) -> bool {
		let routes = self.net.ready();
		let data: Vec<Key> = (self.queues.iter())
			.filter(|(_, queue)| !queue.is_empty())
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
			}
			false => {
				let key = data[at - routes.len()];
				let (cast, msg) = self
					.queues
					.get_mut(&key)
					.and_then(VecDeque::pop_front)
					.expect("a message");
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
			let held = self
				.carry(pull.via, cast)
				.is_some_and(|carry| carry.down.get(&at) == Some(&pull.stream));
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
		let mut at = node;
		for _ in 0..=self.nodes {
			match self.parent(at, cast) {
				Some(next) => at = next,
				None => return false,
			}
		}
		true
	}

	/// Whether the cycle `node`'s pulls for `cast` run into holds a pull the merge made.
	fn merged(&self, node: Node, cast: usize) -> bool {
		let mut at = node;
		for _ in 0..=self.nodes {
			match self.parent(at, cast) {
				Some(next) => at = next,
				None => return false,
			}
		}
		let start = at;
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

	/// Count a cycle after a delivery.
	fn observe(&mut self) {
		self.watch.steps += 1;
		let cycle = (0..self.casts.len()).any(|cast| (0..self.relays).any(|node| self.circles(node, cast)));
		let starved = cycle && self.readers().into_iter().any(|(node, cast)| self.circles(node, cast));
		let merged = cycle
			&& (0..self.casts.len())
				.any(|cast| (0..self.relays).any(|node| self.circles(node, cast) && self.merged(node, cast)));
		self.watch.cycles += usize::from(cycle);
		self.watch.merged += usize::from(merged);
		self.watch.starved += usize::from(starved);
		self.watch.run = if cycle { self.watch.run + 1 } else { 0 };
		self.watch.longest = self.watch.longest.max(self.watch.run);
	}

	/// The relays a path over open links joins to `cast`'s publisher.
	fn reach(&self, cast: usize) -> BTreeSet<Node> {
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

	/// Count unfed readers as a round ends, and the first frames that arrived.
	fn tally(&mut self) {
		let round = self.round;
		let mut reach: BTreeMap<usize, BTreeSet<Node>> = BTreeMap::new();
		for (node, cast) in self.readers() {
			let fed = self.fed(node, cast) && self.live(node, cast).is_some();
			let path = reach.entry(cast).or_insert_with(|| self.reach(cast)).contains(&node);
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

	/// Every reader with a path to its publisher is fed, no pulls circle, and nothing is
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
				if self.reading(node, cast)
					&& reach.contains(&node)
					&& !(self.fed(node, cast) && self.live(node, cast).is_some())
				{
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

	/// The cheapest tree joining `cast`'s publisher to every relay reading it, over open
	/// links at their cold prices: Dreyfus and Wagner's exact dynamic program.
	pub fn cheapest(&self, cast: usize) -> Metric {
		let endpoint = self.endpoints[&self.casts[cast].0];
		let mut terminals = vec![endpoint];
		terminals.extend((0..self.relays).filter(|&node| self.reading(node, cast)));
		let n = self.nodes;
		let mut dist = vec![vec![Metric::MAX / 4; n]; n];
		for (node, row) in dist.iter_mut().enumerate() {
			row[node] = 0;
		}
		for &(a, b) in &self.open {
			let cost = self.price(a, b).cold;
			// Only relays forward.
			dist[a][b] = dist[a][b].min(cost);
			dist[b][a] = dist[b][a].min(cost);
		}
		for via in (0..n).filter(|&node| self.relay(node)) {
			for a in 0..n {
				for b in 0..n {
					dist[a][b] = dist[a][b].min(dist[a][via] + dist[via][b]);
				}
			}
		}
		let count = terminals.len();
		let full = (1usize << count) - 1;
		let mut tree = vec![vec![Metric::MAX / 4; n]; full + 1];
		for (index, &terminal) in terminals.iter().enumerate() {
			tree[1 << index][..n].copy_from_slice(&dist[terminal]);
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
			let row = tree[mask].clone();
			for at in 0..n {
				tree[mask][at] = (0..n).map(|from| row[from] + dist[from][at]).min().unwrap_or(row[at]);
			}
		}
		tree[full][endpoint]
	}
}
