//! Cache-aware routing over [`path::Router`], as `quest/m3/cache-routing/README.md`
//! decides it (Plan item 14) and this module proves it: a warm route offers a
//! copy of one broadcast, propagates, and the pulls it draws settle free of loops.
//!
//! The rule has three layers, each settled before the next depends on it:
//!
//! 1. **Routes.** [`path::Router`] selects each relay's route to the publisher, which
//!    the copies never change.
//! 2. **Offers.** A relay with a local reader is its copy's carrier: it offers every
//!    neighbour its copy at no cost, whatever it pulls. A relay with no reader offers
//!    each neighbour the best of its route and the copies offered to it whose hops
//!    name neither relay, ranked as routes are (drain, then priority, then cost), when
//!    that best is a copy. Carriers' offers are constant and extending one strictly
//!    worsens its rank, so the offers settle as any monotone path vector does, with no
//!    regard for who pulls what.
//! 3. **Pulls.** A carrier pulls through its best source among its route and the copies
//!    whose carrier ranks below its own by the Rendezvous Hash, whoever pulls from it,
//!    so its pull is a function of the settled offers. A relay with no reader pulls
//!    through its best source whose hops name no neighbour it serves, so long as that
//!    is, for each neighbour it serves, the copy it offered or one of a lower-ranked
//!    carrier. A subscription says whether it joins the copy offered or follows the
//!    subscriber's route, and a relay a neighbour pulls along its route pulls along its
//!    own, so a pull that follows routes reaches the publisher. A relay with no reader
//!    that cannot follows its route too.
//!
//! Once quiet, the pulls are free of loops: each relay that joins a copy gets one whose
//! carrier, then whose rank, is strictly better than what it gives each relay joined to
//! it (a carrier its own copy, a relay with no reader the copy it offered), and a pull
//! along routes follows the routes' tree. [`Mesh`] checks after every step whether a
//! broadcast's pulls form a cycle, which stale offers allow until the change crossing a
//! link is delivered, and once quiet that every reader is fed and the offers are the
//! fixed point computed centrally.

use std::{
	cmp::Reverse,
	collections::{BTreeMap, BTreeSet, VecDeque},
	sync::Arc,
};

use super::*;

/// One broadcast: its publisher, its path, and its instance. Two instances of one
/// path are two broadcasts, and a copy of one never serves the other.
pub(super) type Cast = (Id, Path, u64);

/// The draft's Rendezvous Hash of a broadcast's path and a relay, which ranks carriers.
pub(super) fn rendezvous(path: &Path, node: Id) -> u64 {
	let mut hash: u64 = 0x420C_0DEC_B00B;
	for byte in path.as_str().bytes().chain(node.to_le_bytes()) {
		hash = (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3);
	}
	hash
}

/// How a relay names itself in hops: no relay in a [`Mesh`] restarts.
fn id(node: Node) -> Id {
	hop(node, 0)
}

/// How a source ranks, lower first, as [`path::Offer`] ranks a route.
type Rank = (bool, Reverse<i8>, Metric);

/// A copy one relay offers another: WARM_START and WARM_UPDATE.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Copy {
	/// The relays it crossed, its carrier first and the sender last.
	hops: Arc<[Id]>,
	draining: bool,
	priority: i8,
	cost: Metric,
}

/// A source a relay can pull a broadcast from.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Choice {
	next: Node,
	rank: Rank,
	/// The relays the broadcast crossed before this one: the copy's hops, or the
	/// route's path.
	hops: Arc<[Id]>,
	/// Whether it joins a copy, rather than follow the route.
	warm: bool,
}

impl Choice {
	fn names(&self, node: Id) -> bool {
		self.hops.contains(&node)
	}

	fn carrier(&self) -> Option<Id> {
		self.warm.then(|| self.hops[0])
	}

	/// What a relay pulling this offers onward.
	fn copy(&self, me: Id) -> Copy {
		Copy {
			hops: self.hops.iter().copied().chain([me]).collect(),
			draining: self.rank.0,
			priority: self.rank.1.0,
			cost: self.rank.2,
		}
	}
}

/// A message about copies on a session's announce stream.
#[derive(Clone, Debug)]
enum Note {
	Offer(Cast, Copy),
	/// WARM_END.
	End(Cast),
}

/// A relay's demand for one broadcast, and where it pulls it from.
#[derive(Clone, Debug, Default)]
struct Carry {
	reader: bool,
	/// Each neighbour's subscription streams that arrived and were not cancelled,
	/// with whether each joins this relay's copy. A neighbour that moves back here
	/// opens a new stream, which can arrive before the old one's cancel.
	downstream: BTreeMap<Node, BTreeMap<u64, bool>>,
	/// The neighbour it pulls through, and whether that joins its copy.
	parent: Option<(Node, bool)>,
	/// The stream of the pull through `parent`.
	stream: u64,
	/// Demand is gone since then, and the pull stays until the linger runs out.
	lingering: Option<Instant>,
}

/// How long a pull with no demand stays: as long as an idle track does.
const LINGER: Duration = crate::track::IDLE_LINGER;

/// One relay's copies, beside its [`path::Router`].
#[derive(Default)]
struct Relay {
	heard: BTreeMap<(Node, Cast), Copy>,
	sent: BTreeMap<(Node, Cast), Copy>,
	carry: BTreeMap<Cast, Carry>,
	/// Neighbours that sent GOAWAY.
	draining: BTreeSet<Node>,
}

/// A queue the mesh delivers from.
#[derive(Clone, Debug)]
enum Queue {
	Route(Node, Node),
	Copy(Node, Node),
	/// One subscription stream: from, to, broadcast, stream.
	Sub(Node, Node, Cast, u64),
}

pub(super) struct Mesh {
	pub sim: Sim,
	relays: Vec<Relay>,
	links: Vec<Vec<Option<SimLink>>>,
	legacy: Vec<bool>,
	/// Each publisher's node.
	origins: BTreeMap<Id, Node>,
	casts: Vec<Cast>,
	held: BTreeSet<(Node, Node)>,
	notes: BTreeMap<(Node, Node), VecDeque<Note>>,
	/// Subscription streams, unordered against everything else: `Some` pulls or
	/// updates whether it joins the copy, `None` cancels.
	subs: BTreeMap<(Node, Node, Cast, u64), VecDeque<Option<bool>>>,
	streams: u64,
	/// A carrier joins only a copy whose carrier ranks below its own.
	pub ranked: bool,
	/// Copy offers and their ends sent, per broadcast and in all.
	warm: BTreeMap<Cast, usize>,
	pub warm_msgs: usize,
	pub subscribes: usize,
	/// Steps after which some broadcast's pulls formed a cycle.
	pub loops: usize,
	pub steps: usize,
	/// Of those, steps after which a reader's pulls led into the cycle.
	pub starved: usize,
	/// The most deliveries in a row after which a cycle remained, and the current run.
	pub longest: usize,
	run: usize,
	delivered: bool,
}

impl Mesh {
	pub fn new(nodes: usize) -> Self {
		Self {
			sim: Sim::with(Rule::PATH, nodes),
			relays: (0..nodes).map(|_| Relay::default()).collect(),
			links: vec![vec![None; nodes]; nodes],
			legacy: vec![false; nodes],
			origins: BTreeMap::new(),
			casts: Vec::new(),
			held: BTreeSet::new(),
			notes: BTreeMap::new(),
			subs: BTreeMap::new(),
			streams: 0,
			ranked: true,
			warm: BTreeMap::new(),
			warm_msgs: 0,
			subscribes: 0,
			loops: 0,
			steps: 0,
			starved: 0,
			longest: 0,
			run: 0,
			delivered: false,
		}
	}

	fn nodes(&self) -> usize {
		self.relays.len()
	}

	/// A publisher at `node`, a leaf that originates `id` and claims `live`. Called
	/// before its links.
	pub fn origin(&mut self, node: Node, id: Id) {
		self.sim.leaf(node);
		self.sim.originate(node, id, 0);
		self.sim.announce(node, "live", 0, None);
		self.origins.insert(id, node);
	}

	fn publisher(&self, node: Node) -> bool {
		self.origins.values().any(|&origin| origin == node)
	}

	/// `node` never sends or reads a copy: it pulls along its route, as a relay that
	/// does not choose per broadcast does.
	pub fn legacy(&mut self, node: Node) {
		self.legacy[node] = true;
	}

	/// Whether `node` reads copies.
	fn reads(&self, node: Node) -> bool {
		!self.publisher(node) && !self.legacy[node]
	}

	/// A broadcast at `path` of the publisher `id`, under `instance`.
	pub fn broadcast(&mut self, id: Id, path: &str, instance: u64) -> Cast {
		let cast = (id, p(path), instance);
		self.casts.push(cast.clone());
		cast
	}

	pub fn link(&mut self, a: Node, b: Node, ab: SimLink, ba: SimLink) {
		self.links[a][b] = Some(ab);
		self.links[b][a] = Some(ba);
		self.sim.link(a, b, ab, ba);
		self.step();
	}

	pub fn join(&mut self, a: Node, b: Node, cost: Metric) {
		self.link(a, b, SimLink::cost(cost), SimLink::cost(cost));
	}

	pub fn connect(&mut self, a: Node, b: Node) {
		self.sim.connect(a, b);
		self.step();
	}

	pub fn disconnect(&mut self, a: Node, b: Node) {
		self.sim.disconnect(a, b);
		for (x, y) in [(a, b), (b, a)] {
			self.notes.remove(&(x, y));
			self.subs.retain(|(from, to, ..), _| (*from, *to) != (x, y));
			self.held.remove(&(x, y));
			let relay = &mut self.relays[x];
			relay.heard.retain(|(peer, _), _| *peer != y);
			relay.sent.retain(|(peer, _), _| *peer != y);
			relay.draining.remove(&y);
			for carry in relay.carry.values_mut() {
				carry.downstream.remove(&y);
				// The pull went with the session.
				if carry.parent.is_some_and(|(next, _)| next == y) {
					carry.parent = None;
				}
			}
		}
		self.step();
	}

	pub fn connected(&self, a: Node, b: Node) -> bool {
		self.sim.connected(a, b)
	}

	fn peers(&self, node: Node) -> impl Iterator<Item = Node> + '_ {
		(0..self.nodes()).filter(move |&peer| self.links[node][peer].is_some() && self.connected(node, peer))
	}

	/// `node` sends GOAWAY on every session.
	pub fn goaway(&mut self, node: Node) {
		self.sim.goaway(node);
		let peers: Vec<Node> = self.peers(node).collect();
		for peer in peers {
			self.relays[peer].draining.insert(node);
		}
		self.step();
	}

	/// A reader at `node` starts or stops reading `cast`.
	pub fn read(&mut self, node: Node, cast: &Cast, reading: bool) {
		match reading {
			true => self.relays[node].carry.entry(cast.clone()).or_default().reader = true,
			false => {
				if let Some(carry) = self.relays[node].carry.get_mut(cast) {
					carry.reader = false;
				}
			}
		}
		self.step();
	}

	pub fn reading(&self, node: Node, cast: &Cast) -> bool {
		self.relays[node].carry.get(cast).is_some_and(|carry| carry.reader)
	}

	pub fn hold(&mut self, from: Node, to: Node) {
		self.held.insert((from, to));
		self.sim.hold(from, to);
	}

	pub fn release(&mut self, from: Node, to: Node) {
		self.held.remove(&(from, to));
		self.sim.release(from, to);
	}

	/// The linger for `cast` at `node` runs out: a pull with no demand left goes.
	pub fn expire(&mut self, node: Node, cast: &Cast) {
		let carry = self.relays[node].carry.get_mut(cast);
		if let Some(carry) = carry
			&& carry.lingering.is_some()
		{
			carry.lingering = None;
			if let Some((parent, _)) = carry.parent.take() {
				let stream = carry.stream;
				self.send_sub(node, parent, cast, stream, None);
			}
		}
		self.step();
	}

	/// One hop of delay passes everywhere. Whether a doubt still waits to lift.
	pub fn tick(&mut self) -> bool {
		self.sim.wait(HOP);
		self.step();
		self.sim.deadline().is_some()
	}

	/// The subscription streams from `from` to `to` with a change to deliver, oldest first.
	pub fn streams(&self, from: Node, to: Node) -> Vec<u64> {
		(self.subs.keys())
			.filter(|(f, t, ..)| (*f, *t) == (from, to))
			.map(|(.., stream)| *stream)
			.collect()
	}

	/// Deliver the next change on one subscription stream from `from` to `to`.
	pub fn deliver_stream(&mut self, from: Node, to: Node, cast: &Cast, stream: u64) -> bool {
		let delivered = self.recv_sub(from, to, cast, stream);
		self.step();
		delivered
	}

	/// Deliver the next change on the oldest stream of `cast` from `from` to `to`.
	pub fn deliver_sub(&mut self, from: Node, to: Node, cast: &Cast) -> bool {
		let stream = (self.subs.keys()).find(|key| (key.0, key.1, &key.2) == (from, to, cast));
		match stream.map(|key| key.3) {
			Some(stream) => self.deliver_stream(from, to, cast, stream),
			None => false,
		}
	}

	/// Every unheld queue with something to deliver.
	fn busy(&self) -> Vec<Queue> {
		let routes = (self.sim.busy().into_iter()).map(|(from, to)| Queue::Route(from, to));
		let notes = (self.notes.iter())
			.filter(|(_, queue)| !queue.is_empty())
			.map(|(&(from, to), _)| Queue::Copy(from, to));
		let subs = (self.subs.keys()).map(|(from, to, cast, stream)| Queue::Sub(*from, *to, cast.clone(), *stream));
		(routes.chain(notes).chain(subs))
			.filter(|queue| match queue {
				Queue::Route(from, to) | Queue::Copy(from, to) | Queue::Sub(from, to, ..) => {
					!self.held.contains(&(*from, *to))
				}
			})
			.collect()
	}

	fn deliver_one(&mut self, queue: &Queue) {
		self.delivered = true;
		match queue {
			Queue::Route(from, to) => {
				self.sim.deliver(*from, *to);
			}
			Queue::Copy(from, to) => {
				if let Some(note) = self.notes.get_mut(&(*from, *to)).and_then(VecDeque::pop_front) {
					self.recv_note(*from, *to, note);
				}
			}
			Queue::Sub(from, to, cast, stream) => {
				self.recv_sub(*from, *to, cast, *stream);
			}
		}
		self.step();
	}

	fn recv_note(&mut self, from: Node, to: Node, note: Note) {
		let heard = &mut self.relays[to].heard;
		match note {
			Note::Offer(cast, copy) => heard.insert((from, cast), copy),
			Note::End(cast) => heard.remove(&(from, cast)),
		};
	}

	fn recv_sub(&mut self, from: Node, to: Node, cast: &Cast, stream: u64) -> bool {
		let key = (from, to, cast.clone(), stream);
		let Some(change) = self.subs.get_mut(&key).and_then(VecDeque::pop_front) else {
			return false;
		};
		if self.subs[&key].is_empty() {
			self.subs.remove(&key);
		}
		let carry = self.relays[to].carry.entry(cast.clone()).or_default();
		let streams = carry.downstream.entry(from).or_default();
		match change {
			Some(warm) => streams.insert(stream, warm),
			None => streams.remove(&stream),
		};
		if streams.is_empty() {
			carry.downstream.remove(&from);
		}
		true
	}

	fn send_sub(&mut self, from: Node, to: Node, cast: &Cast, stream: u64, change: Option<bool>) {
		if !self.connected(from, to) {
			return;
		}
		self.subscribes += 1;
		let key = (from, to, cast.clone(), stream);
		self.subs.entry(key).or_default().push_back(change);
	}

	/// Deliver round-robin until every unheld queue is empty, each round taking a hop
	/// of delay, so a long exchange lets doubts lift. False if it never empties.
	pub fn run(&mut self) -> bool {
		self.rounds(10_000)
	}

	/// Deliver until quiet, failing after `rounds` busy rounds. A pull lingering with no
	/// demand can hold a copy its parent promised elsewhere, and the pulls around it
	/// flip until the linger runs out, so a round with a linger pending is not counted,
	/// up to four lingers' worth.
	fn rounds(&mut self, rounds: usize) -> bool {
		let lingers = 4 * (LINGER.as_millis() / HOP.as_millis()) as usize;
		let (mut counted, mut total) = (0, 0);
		while counted < rounds && total < rounds + lingers {
			let ready = self.busy();
			if ready.is_empty() {
				return true;
			}
			for queue in &ready {
				self.deliver_one(queue);
			}
			self.sim.wait(HOP);
			self.step();
			// A linger runs out on its timer, however busy the mesh is.
			let now = self.sim.now();
			let lingering: Vec<(Node, Cast, Instant)> = (0..self.nodes())
				.flat_map(|node| {
					(self.relays[node].carry.iter())
						.filter_map(move |(cast, carry)| Some((node, cast.clone(), carry.lingering?)))
				})
				.collect();
			let mut pending = false;
			for (node, cast, since) in lingering {
				match now.saturating_duration_since(since) >= LINGER {
					true => self.expire(node, &cast),
					false => pending = true,
				}
			}
			total += 1;
			counted += usize::from(!pending);
		}
		false
	}

	/// Run, let time pass until every doubt lifts, and let every linger run out,
	/// until nothing changes. False if it never settles.
	pub fn settle(&mut self) -> bool {
		for _ in 0..100 {
			if !self.rounds(300) {
				return false;
			}
			if let Some(at) = self.sim.deadline() {
				let now = self.sim.now();
				self.sim.wait(at.saturating_duration_since(now));
				self.step();
				continue;
			}
			let lingering: Vec<(Node, Cast)> = (0..self.nodes())
				.flat_map(|node| {
					(self.relays[node].carry.iter())
						.filter(|(_, carry)| carry.lingering.is_some())
						.map(move |(cast, _)| (node, cast.clone()))
				})
				.collect();
			if lingering.is_empty() {
				return true;
			}
			for (node, cast) in lingering {
				self.expire(node, &cast);
			}
		}
		false
	}

	// --- The rule ---

	/// Where `node` pulls `cast` along its route, unless it publishes it.
	fn cold(&self, node: Node, cast: &Cast) -> Option<Choice> {
		if self.publisher(node) {
			return None;
		}
		let router = self.sim.path(node);
		let path::Forward::Next { next, prefix, .. } = router.forward(cast.0, &cast.1, None)? else {
			return None;
		};
		let offer = router.route_for(cast.0, &prefix)?.offer()?;
		let (_, before) = offer.path.split_last()?;
		Some(Choice {
			next,
			rank: (offer.draining, Reverse(offer.priority), offer.metric),
			hops: before.iter().map(|hop| hop.node).collect(),
			warm: false,
		})
	}

	/// Joining `copy`, heard from `peer`, at `node`: what `node` would pull, or
	/// nothing if its hops name `node`.
	fn extend(&self, node: Node, peer: Node, copy: &Copy) -> Option<Choice> {
		if copy.hops.contains(&id(node)) {
			return None;
		}
		let link = self.links[node][peer]?;
		let draining = copy.draining || self.relays[node].draining.contains(&peer);
		Some(Choice {
			next: peer,
			rank: (
				draining,
				Reverse(copy.priority.min(link.priority)),
				copy.cost.checked_add(link.cost)?,
			),
			hops: copy.hops.clone(),
			warm: true,
		})
	}

	/// Every source `node` has for `cast`: its route, and each copy offered to it.
	fn options(&self, node: Node, cast: &Cast) -> Vec<Choice> {
		let mut options: Vec<Choice> = self.cold(node, cast).into_iter().collect();
		if self.reads(node) {
			for ((peer, heard), copy) in &self.relays[node].heard {
				if heard == cast && self.connected(node, *peer) {
					options.extend(self.extend(node, *peer, copy));
				}
			}
		}
		options
	}

	/// The best of `options`: by rank, then a copy over the route, then the lower
	/// carrier, then the lower neighbour, so every relay that knows the same offers
	/// picks the same.
	fn pick(cast: &Cast, options: impl Iterator<Item = Choice>) -> Option<Choice> {
		options.min_by_key(|choice| {
			let carrier = choice.carrier().map_or(0, |carrier| rendezvous(&cast.1, carrier));
			(choice.rank, !choice.warm, carrier, choice.next)
		})
	}

	/// The best source of `options` at `node`: its route, or a copy whose hops name none
	/// of `exclude` and, for a carrier, whose carrier ranks below it. The route stays
	/// usable through a neighbour this relay serves, since following it makes that
	/// neighbour follow its own.
	fn best(&self, node: Node, cast: &Cast, options: &[Choice], exclude: &BTreeSet<Id>) -> Option<Choice> {
		let carrier = self.ranked && self.reading(node, cast);
		let own = rendezvous(&cast.1, id(node));
		let usable = (options.iter())
			.filter(|choice| !choice.warm || !exclude.iter().any(|&node| choice.names(node)))
			.filter(|choice| !carrier || choice.carrier().is_none_or(|by| rendezvous(&cast.1, by) < own));
		Self::pick(cast, usable.cloned())
	}

	/// The copy `node` offers `peer` for `cast`, from `options`.
	fn offer_from(&self, node: Node, peer: Node, cast: &Cast, options: &[Choice]) -> Option<Copy> {
		if !self.reads(node) || !self.reads(peer) {
			return None;
		}
		// A warm route travels under the route the relay sends.
		self.cold(node, cast)?;
		if self.reading(node, cast) {
			return Some(Copy {
				hops: Arc::from([id(node)]),
				draining: false,
				priority: i8::MAX,
				cost: 0,
			});
		}
		let best = (options.iter()).filter(|choice| !choice.names(id(peer)));
		Self::pick(cast, best.cloned())
			.filter(|choice| choice.warm)
			.map(|choice| choice.copy(id(node)))
	}

	fn offer_to(&self, node: Node, peer: Node, cast: &Cast) -> Option<Copy> {
		self.offer_from(node, peer, cast, &self.options(node, cast))
	}

	/// Where `node` would pull `cast` from, given who pulls through it.
	fn decide(&self, node: Node, cast: &Cast) -> Option<Choice> {
		let carry = self.relays[node].carry.get(cast);
		let downstream = carry.map(|carry| &carry.downstream);
		let streams = || downstream.into_iter().flatten();
		// A neighbour that follows its route through this relay makes it follow its own.
		let forced = streams().any(|(_, streams)| streams.values().any(|warm| !warm));
		if self.legacy[node] || forced {
			return self.cold(node, cast);
		}
		let options = self.options(node, cast);
		// A carrier's choice depends on the offers alone: a relay with no reader that it
		// joins yields instead, so two neighbours never trade who pulls whom.
		if self.reading(node, cast) {
			return self.best(node, cast, &options, &BTreeSet::new());
		}
		let serves: BTreeSet<Id> = streams().map(|(&peer, _)| id(peer)).collect();
		let choice = self.best(node, cast, &options, &serves);
		// Each neighbour that joined this relay's copy gets what this relay pulls, so
		// it pulls a copy only while, for each, that is the copy it offered or one whose
		// carrier ranks below that copy's. Upstream along joined copies, the carrier's
		// rank then the copy's rank strictly falls, so the joins form no cycle.
		let pulled = choice
			.as_ref()
			.filter(|choice| choice.warm)
			.map(|choice| choice.copy(id(node)));
		let below = |offered: &Copy| {
			let rank = |copy: &Copy| rendezvous(&cast.1, copy.hops[0]);
			pulled.as_ref().is_some_and(|pulled| rank(pulled) < rank(offered))
		};
		let kept = streams().all(|(&peer, _)| match self.offer_from(node, peer, cast, &options) {
			None => true,
			Some(offered) => pulled.as_ref() == Some(&offered) || below(&offered),
		});
		match kept {
			true => choice,
			false => self.cold(node, cast),
		}
	}

	/// Follow the demand for `cast` at `node`: pull from its source and move with it.
	/// Once the demand is gone the pull stays where it is until the linger runs out.
	fn pull(&mut self, node: Node, cast: &Cast) {
		let choice = self.decide(node, cast).map(|choice| (choice.next, choice.warm));
		let now = self.sim.now();
		let Some(carry) = self.relays[node].carry.get_mut(cast) else {
			return;
		};
		let demand = carry.reader || !carry.downstream.is_empty();
		match !demand && carry.parent.is_some() {
			true => {
				carry.lingering.get_or_insert(now);
				return;
			}
			false => carry.lingering = None,
		}
		let parent = choice.filter(|_| demand);
		let old = std::mem::replace(&mut carry.parent, parent);
		let stream = carry.stream;
		if !demand && parent.is_none() {
			self.relays[node].carry.remove(cast);
		}
		if old == parent {
			return;
		}
		match (old, parent) {
			(Some((was, _)), Some((next, warm))) if was == next => self.send_sub(node, next, cast, stream, Some(warm)),
			(old, new) => {
				if let Some((was, _)) = old {
					self.send_sub(node, was, cast, stream, None);
				}
				if let Some((next, warm)) = new {
					self.streams += 1;
					let stream = self.streams;
					self.relays[node].carry.get_mut(cast).unwrap().stream = stream;
					self.send_sub(node, next, cast, stream, Some(warm));
				}
			}
		}
	}

	/// Bring what `node` offers each neighbour for `cast` up to date.
	fn offer(&mut self, node: Node, cast: &Cast) {
		let options = self.options(node, cast);
		let peers: Vec<Node> = self.peers(node).collect();
		for peer in peers {
			let want = self.offer_from(node, peer, cast, &options);
			let key = (peer, cast.clone());
			if self.relays[node].sent.get(&key) == want.as_ref() {
				continue;
			}
			let note = match want {
				Some(copy) => {
					self.relays[node].sent.insert(key, copy.clone());
					Note::Offer(cast.clone(), copy)
				}
				None => {
					self.relays[node].sent.remove(&key);
					Note::End(cast.clone())
				}
			};
			self.notes.entry((node, peer)).or_default().push_back(note);
			self.warm_msgs += 1;
			*self.warm.entry(cast.clone()).or_default() += 1;
		}
	}

	/// Each relay reacts to what changed, then the mesh counts a cycle.
	fn step(&mut self) {
		for node in 0..self.nodes() {
			for cast in self.casts.clone() {
				self.pull(node, &cast);
				self.offer(node, &cast);
			}
		}
		self.steps += 1;
		let cycle = self.cycle().is_some();
		self.loops += usize::from(cycle);
		self.run = match cycle {
			true => self.run + usize::from(std::mem::take(&mut self.delivered)),
			false => 0,
		};
		self.delivered = false;
		self.longest = self.longest.max(self.run);
		self.starved += usize::from(cycle && self.starving());
	}

	// --- Queries ---

	/// What each relay pulls and offers, and what is in flight, for a failure report.
	pub fn dump(&self) -> String {
		let mut out = String::new();
		for (node, relay) in self.relays.iter().enumerate() {
			for (cast, carry) in &relay.carry {
				out += &format!("  {node} {cast:?}: {carry:?}\n");
			}
			for ((peer, cast), copy) in &relay.sent {
				out += &format!("  {node} offers {peer} {cast:?}: {copy:?}\n");
			}
		}
		out += &format!("  busy {:?}\n  subs {:?}\n", self.busy(), self.subs);
		out
	}

	pub fn parent(&self, node: Node, cast: &Cast) -> Option<Node> {
		self.relays[node].carry.get(cast)?.parent.map(|(next, _)| next)
	}

	/// Whether `node` pulls `cast`.
	pub fn carries(&self, node: Node, cast: &Cast) -> bool {
		self.parent(node, cast).is_some()
	}

	/// The relays pulling `cast` straight from its publisher.
	pub fn copies(&self, cast: &Cast) -> usize {
		let origin = self.origins[&cast.0];
		(0..self.nodes())
			.filter(|&node| self.parent(node, cast) == Some(origin))
			.count()
	}

	/// Copy offers and their ends sent for `cast`.
	pub fn warm_sent(&self, cast: &Cast) -> usize {
		self.warm.get(cast).copied().unwrap_or(0)
	}

	/// Whether `node` is offered a copy of `cast` by `from`.
	pub fn offered(&self, node: Node, from: Node, cast: &Cast) -> bool {
		self.relays[node].heard.contains_key(&(from, cast.clone()))
	}

	/// The carrier of the copy `from` offers `node`.
	pub fn carrier(&self, node: Node, from: Node, cast: &Cast) -> Option<Node> {
		let copy = self.relays[node].heard.get(&(from, cast.clone()))?;
		(0..self.nodes()).find(|&relay| id(relay) == copy.hops[0])
	}

	/// The pairs of relays and broadcasts where a relay with no reader serves
	/// neighbours it offered different copies, and so follows its route.
	pub fn split(&self) -> usize {
		let mut split = 0;
		for node in 0..self.nodes() {
			for (cast, carry) in &self.relays[node].carry {
				let offered: BTreeSet<Option<Arc<[Id]>>> = (carry.downstream.keys())
					.map(|&peer| self.offer_to(node, peer, cast).map(|copy| copy.hops))
					.collect();
				split += usize::from(!carry.reader && offered.len() > 1);
			}
		}
		split
	}

	/// Deliver `rounds` more rounds, logging each delivery and the pulls it moved: what
	/// keeps a mesh that never settles busy.
	pub fn trace(&mut self, rounds: usize) -> String {
		let mut out = String::new();
		for _ in 0..rounds {
			for queue in self.busy() {
				let before: Vec<Vec<Option<(Node, bool)>>> = (self.relays.iter())
					.map(|relay| {
						self.casts
							.iter()
							.map(|cast| relay.carry.get(cast).and_then(|c| c.parent))
							.collect()
					})
					.collect();
				let what = match &queue {
					Queue::Copy(from, to) => format!("{:?}", self.notes.get(&(*from, *to)).and_then(|q| q.front())),
					Queue::Sub(from, to, cast, stream) => {
						format!(
							"{:?}",
							self.subs
								.get(&(*from, *to, cast.clone(), *stream))
								.and_then(|q| q.front())
						)
					}
					Queue::Route(..) => String::new(),
				};
				out += &format!("  deliver {queue:?} {what}\n");
				self.deliver_one(&queue);
				for (node, relay) in self.relays.iter().enumerate() {
					for (i, cast) in self.casts.iter().enumerate() {
						let after = relay.carry.get(cast).and_then(|c| c.parent);
						if after != before[node][i] {
							out += &format!("    {node} {cast:?}: {:?} -> {after:?}\n", before[node][i]);
						}
					}
				}
			}
			self.sim.wait(HOP);
			self.step();
		}
		out
	}

	/// Whether some reader's pulls lead into a cycle.
	fn starving(&self) -> bool {
		self.casts.iter().any(|cast| {
			(0..self.nodes()).filter(|&node| self.reading(node, cast)).any(|node| {
				let mut at = node;
				for _ in 0..=self.nodes() {
					match self.parent(at, cast) {
						Some(next) => at = next,
						None => return false,
					}
				}
				true
			})
		})
	}

	/// A broadcast whose pulls form a cycle, and each relay's parent for it.
	fn cycle(&self) -> Option<(Cast, Vec<Option<Node>>)> {
		for cast in &self.casts {
			for node in 0..self.nodes() {
				let mut at = node;
				for hop in 0.. {
					let Some(next) = self.parent(at, cast) else {
						break;
					};
					if hop > self.nodes() {
						let parents = (0..self.nodes()).map(|n| self.parent(n, cast)).collect();
						return Some((cast.clone(), parents));
					}
					at = next;
				}
			}
		}
		None
	}

	/// The offers for `cast` once they settle, computed centrally: every relay offers
	/// from what the others offered it the round before, until a round changes nothing.
	fn fixed_point(&self, cast: &Cast) -> BTreeMap<(Node, Node), Copy> {
		let links: Vec<(Node, Node)> = (0..self.nodes())
			.flat_map(|node| self.peers(node).map(move |peer| (node, peer)))
			.collect();
		let mut offers = BTreeMap::new();
		for _ in 0..4 * self.nodes() {
			let mut next = BTreeMap::new();
			for &(node, peer) in &links {
				let mut options: Vec<Choice> = self.cold(node, cast).into_iter().collect();
				if self.reads(node) {
					for &(from, to) in &links {
						if let (true, Some(copy)) = (to == node, offers.get(&(from, to))) {
							options.extend(self.extend(node, from, copy));
						}
					}
				}
				if let Some(copy) = self.offer_from(node, peer, cast, &options) {
					next.insert((node, peer), copy);
				}
			}
			if next == offers {
				return offers;
			}
			offers = next;
		}
		panic!("the offers for {cast:?} never settle");
	}

	/// The steady state once demand and topology stop changing: every reader is fed
	/// from its publisher, subscriptions and copies offered match what their senders
	/// hold and what the rule computes centrally, no copy lingers, and each relay pulls
	/// exactly what it has demand for, from the source the rule picks.
	pub fn steady(&mut self, label: &str) {
		assert!(self.busy().is_empty(), "{label}: still busy");
		assert_eq!(self.cycle(), None, "{label}");
		for cast in &self.casts {
			let origin = self.origins[&cast.0];
			let fixed = self.fixed_point(cast);
			for node in 0..self.nodes() {
				let relay = &self.relays[node];
				if self.reading(node, cast) {
					let mut at = node;
					while let Some(next) = self.parent(at, cast) {
						at = next;
					}
					assert_eq!(at, origin, "{label}: {cast:?} reaches node {node}");
				}
				let feeds: BTreeMap<Node, bool> = (0..self.nodes())
					.filter_map(|n| {
						let (next, warm) = self.relays[n].carry.get(cast)?.parent?;
						(next == node).then_some((n, warm))
					})
					.collect();
				let carry = relay.carry.get(cast);
				let downstream: BTreeMap<Node, bool> = (carry.into_iter())
					.flat_map(|carry| &carry.downstream)
					.map(|(&peer, streams)| {
						assert_eq!(streams.len(), 1, "{label}: {cast:?} at node {node}: one stream each");
						(peer, *streams.values().next().unwrap())
					})
					.collect();
				assert_eq!(downstream, feeds, "{label}: {cast:?} at node {node}");
				assert!(
					!carry.is_some_and(|carry| carry.lingering.is_some()),
					"{label}: {cast:?} at node {node}"
				);
				if node != origin {
					let demand = self.reading(node, cast) || !feeds.is_empty();
					let source = self.decide(node, cast).map(|choice| (choice.next, choice.warm));
					let parent = carry.and_then(|carry| carry.parent);
					assert_eq!(parent, source.filter(|_| demand), "{label}: {cast:?} at node {node}");
				}
				for peer in self.peers(node) {
					let sent = relay.sent.get(&(peer, cast.clone()));
					let heard = self.relays[peer].heard.get(&(node, cast.clone()));
					assert_eq!(sent, heard, "{label}: {cast:?} offered from {node} to {peer}");
					assert_eq!(
						sent,
						fixed.get(&(node, peer)),
						"{label}: {cast:?} from {node} to {peer}, against the fixed point"
					);
				}
			}
		}
		let before = (self.sim.stats, self.warm_msgs, self.subscribes);
		self.tick();
		let after = (self.sim.stats, self.warm_msgs, self.subscribes);
		assert_eq!(before, after, "{label}: something changed at quiescence");
	}
}

/// A broadcast path under which `low` ranks below `high` as a carrier.
fn ranking(low: Node, high: Node) -> String {
	(0..)
		.map(|i| format!("live/x{i}"))
		.find(|path| rendezvous(&p(path), id(low)) < rendezvous(&p(path), id(high)))
		.unwrap()
}

/// An origin behind relay `a`, then relays `a`, `b`, `c` in a chain priced 3, with a
/// skip from `a` to `c` priced `skip`, and one broadcast at `path`.
fn chain(skip: Metric, path: &str) -> (Mesh, Cast) {
	let (o, a, b, c) = (0, 1, 2, 3);
	let mut mesh = Mesh::new(4);
	mesh.origin(o, 7);
	mesh.join(o, a, 1);
	mesh.join(a, b, 3);
	mesh.join(b, c, 3);
	mesh.join(a, c, skip);
	let x = mesh.broadcast(7, path, 0);
	assert!(mesh.settle(), "{}", mesh.dump());
	(mesh, x)
}

/// A cold broadcast takes the skip. A reader on the skipped relay `b` makes `c` join
/// its copy, and when that reader leaves, `c` returns to the skip. Where `c` ranks
/// lower, `b` joins `c`'s copy instead, though `c` is farther from the publisher, and
/// in whichever order the readers arrive.
#[test]
fn a_skipped_relay_draws_its_neighbour_while_it_has_a_reader() {
	let (a, b, c) = (1, 2, 3);
	let (mut mesh, x) = chain(5, &ranking(b, c));
	mesh.read(c, &x, true);
	assert!(mesh.settle(), "{}", mesh.dump());
	assert_eq!(mesh.parent(c, &x), Some(a), "a cold broadcast takes the skip");
	assert!(!mesh.carries(b, &x));
	mesh.steady("cold");

	mesh.read(b, &x, true);
	assert!(mesh.settle(), "{}", mesh.dump());
	assert_eq!(mesh.parent(c, &x), Some(b), "c joins b's copy");
	assert_eq!(mesh.parent(b, &x), Some(a));
	mesh.steady("c joins b");

	mesh.read(b, &x, false);
	assert!(mesh.settle(), "{}", mesh.dump());
	assert_eq!(mesh.parent(c, &x), Some(a), "back to the skip");
	assert!(!mesh.carries(b, &x));
	mesh.steady("cold again");
	assert_eq!(mesh.loops, 0);

	for first in [b, c] {
		let (mut mesh, x) = chain(5, &ranking(c, b));
		mesh.read(first, &x, true);
		assert!(mesh.settle(), "{}", mesh.dump());
		mesh.read(b + c - first, &x, true);
		assert!(mesh.settle(), "{}", mesh.dump());
		assert_eq!(mesh.parent(b, &x), Some(c), "b joins c's copy, though c is farther");
		assert_eq!(mesh.parent(c, &x), Some(a));
		mesh.steady(&format!("b joins c, {first} first"));
		assert_eq!(mesh.loops, 0);
	}
}

/// A relay whose only consumer is `c` keeps pulling for it once its own reader goes
/// only while it is `c`'s route: with a cheaper skip, `c` bypasses it.
#[test]
fn a_relay_feeding_one_consumer_stays_only_on_its_cheapest_route() {
	let (a, b, c) = (1, 2, 3);
	for (skip, stays) in [(5, false), (9, true)] {
		let (mut mesh, x) = chain(skip, &ranking(b, c));
		mesh.read(b, &x, true);
		mesh.read(c, &x, true);
		assert!(mesh.settle(), "{}", mesh.dump());
		assert_eq!(mesh.parent(c, &x), Some(b), "skip {skip}");
		mesh.read(b, &x, false);
		assert!(mesh.settle(), "{}", mesh.dump());
		assert_eq!(mesh.parent(c, &x), Some(if stays { b } else { a }), "skip {skip}");
		assert_eq!(mesh.carries(b, &x), stays, "skip {skip}");
		mesh.steady(&format!("skip {skip}"));
	}
}

/// `r2` pulls along its route, through `r1` and `r3`. `r1` would rather join `r2`'s
/// own copy, which it offers `r3`, but a neighbour following its route through `r1`
/// makes `r1` follow its own, so the pull reaches the publisher.
#[test]
fn a_pull_along_the_route_stays_on_the_route() {
	let (o, r1, r2, r3) = (0, 1, 2, 3);
	let mut mesh = Mesh::new(4);
	mesh.origin(o, 7);
	mesh.join(o, r3, 1);
	mesh.join(r3, r1, 1);
	mesh.join(r1, r2, 1);
	mesh.join(r2, r3, 3);
	let x = mesh.broadcast(7, "live/x", 0);
	assert!(mesh.settle(), "{}", mesh.dump());
	mesh.read(r2, &x, true);
	assert!(mesh.settle(), "{}", mesh.dump());
	assert_eq!(mesh.carrier(r3, r1, &x), Some(r2), "r1 offers r3 the copy of r2");
	assert!(!mesh.offered(r2, r1, &x), "and r2 nothing that leads back");
	assert_eq!(mesh.parent(r2, &x), Some(r1));
	assert_eq!(mesh.parent(r1, &x), Some(r3), "r1 follows its route");
	mesh.steady("route");
	assert_eq!(mesh.loops, 0);
}

const PATHS: [&str; 8] = [
	"live/0", "live/1", "live/2", "live/3", "live/4", "live/5", "live/6", "live/7",
];

/// Two relays at equal cost, each with a reader of every broadcast, whose readers
/// arrive in the same instant: per broadcast only the lower hash keeps its uplink and
/// the other joins it, so the pair pulls one copy, and the owner differs across
/// broadcasts. Unranked, each joins the other and neither is fed.
#[test]
fn equal_relays_pull_one_copy_owned_by_the_lower_hash() {
	let (o, x, y) = (0, 1, 2);
	let build = |ranked: bool| {
		let mut mesh = Mesh::new(3);
		mesh.ranked = ranked;
		mesh.origin(o, 7);
		mesh.join(o, x, 3);
		mesh.join(o, y, 3);
		mesh.join(x, y, 1);
		let casts: Vec<Cast> = PATHS.iter().map(|path| mesh.broadcast(7, path, 0)).collect();
		for cast in &casts {
			mesh.relays[x].carry.entry(cast.clone()).or_default().reader = true;
			mesh.relays[y].carry.entry(cast.clone()).or_default().reader = true;
		}
		mesh.step();
		(mesh, casts)
	};
	let (mut mesh, casts) = build(true);
	assert!(mesh.settle(), "{}", mesh.dump());
	let mut owners = BTreeSet::new();
	for cast in &casts {
		let rank = |node: Node| rendezvous(&cast.1, id(node));
		let (low, high) = match rank(x) < rank(y) {
			true => (x, y),
			false => (y, x),
		};
		assert_eq!(mesh.parent(low, cast), Some(o), "{cast:?}");
		assert_eq!(mesh.parent(high, cast), Some(low), "{cast:?}");
		assert_eq!(mesh.copies(cast), 1, "{cast:?}");
		owners.insert(low);
	}
	assert_eq!(owners.len(), 2, "the hash spreads the owner");
	mesh.steady("pair");
	assert_eq!(mesh.loops, 0);

	// Unranked, each takes the other's copy and the pair settles into a cycle that
	// feeds neither: the rank is what breaks the tie.
	let (mut mesh, casts) = build(false);
	assert!(mesh.settle(), "{}", mesh.dump());
	for cast in &casts {
		assert_eq!(
			(mesh.parent(x, cast), mesh.parent(y, cast)),
			(Some(y), Some(x)),
			"{cast:?}"
		);
	}
}

/// A ring of relays, each with its own link to the publisher and a reader, loses all
/// but one of those links while every link of the ring is held, so each relay acts on
/// what it last heard. Delivered in any order, once quiet every reader is fed through
/// the one link left, and the pulls formed a cycle only while a change was still
/// crossing a link.
#[test]
fn a_ring_of_stale_offers_settles_on_the_link_left() {
	const RING: usize = 5;
	let o = 0;
	let relays: Vec<Node> = (1..=RING).collect();
	let mut loops = 0;
	for seed in 0..20u64 {
		let mut mesh = Mesh::new(RING + 1);
		mesh.origin(o, 7);
		for (i, &relay) in relays.iter().enumerate() {
			mesh.join(o, relay, 1);
			mesh.join(relay, relays[(i + 1) % RING], 1);
		}
		let x = mesh.broadcast(7, "live/x", 0);
		for &relay in &relays {
			mesh.read(relay, &x, true);
		}
		assert!(mesh.settle(), "{}", mesh.dump());
		for (i, &relay) in relays.iter().enumerate() {
			let next = relays[(i + 1) % RING];
			mesh.hold(relay, next);
			mesh.hold(next, relay);
		}
		for &relay in &relays[1..] {
			mesh.disconnect(o, relay);
		}
		for (i, &relay) in relays.iter().enumerate() {
			let next = relays[(i + 1) % RING];
			mesh.release(relay, next);
			mesh.release(next, relay);
		}
		let mut rng = XorShift(0x5717_e000 + seed);
		loop {
			let busy = mesh.busy();
			if busy.is_empty() {
				break;
			}
			mesh.deliver_one(&busy[rng.next(busy.len() as u64) as usize]);
		}
		assert!(mesh.settle(), "seed {seed}\n{}", mesh.dump());
		mesh.steady(&format!("seed {seed}"));
		assert_eq!(mesh.copies(&x), 1, "seed {seed}");
		assert_eq!(mesh.parent(relays[0], &x), Some(o), "seed {seed}");
		loops += mesh.loops;
	}
	assert!(loops <= 40, "{loops} steps with a cycle across 20 seeds");
}

/// `b` withdraws its copy while `c`'s subscription to it is on its way. The
/// subscription reaches `b` with no copy left, and `b`, which may not pull through the
/// neighbour it serves, follows its route; `c` then moves back to the skip.
#[test]
fn a_request_crossing_a_withdrawal_is_forwarded_without_a_loop() {
	let (a, b, c) = (1, 2, 3);
	let (mut mesh, x) = chain(5, &ranking(b, c));
	mesh.read(b, &x, true);
	assert!(mesh.settle(), "{}", mesh.dump());
	assert!(mesh.offered(c, b, &x));
	mesh.hold(c, b);
	mesh.hold(b, c);
	mesh.read(c, &x, true);
	assert!(mesh.run());
	assert_eq!(mesh.parent(c, &x), Some(b), "c's subscription waits");

	mesh.read(b, &x, false);
	mesh.expire(b, &x);
	assert!(mesh.run());
	assert!(!mesh.carries(b, &x), "b let its copy go");
	assert!(mesh.offered(c, b, &x), "its withdrawal waits");

	assert!(mesh.deliver_sub(c, b, &x));
	assert_eq!(
		mesh.parent(b, &x),
		Some(a),
		"b forwards the request along its own route"
	);
	mesh.release(c, b);
	mesh.release(b, c);
	assert!(mesh.settle(), "{}", mesh.dump());
	assert_eq!(mesh.parent(c, &x), Some(a));
	assert!(!mesh.carries(b, &x));
	mesh.steady("crossed");
	assert_eq!(mesh.loops, 0);
}

/// `c` moves off `b` and back while its subscriptions to `b` wait. The new stream's
/// pull reaches `b` before the old stream's cancel, and `b` keeps feeding `c`.
#[test]
fn a_replacement_pull_overtaking_the_old_cancel_keeps_its_feed() {
	let (a, b, c) = (1, 2, 3);
	let (mut mesh, x) = chain(5, &ranking(b, c));
	mesh.read(b, &x, true);
	mesh.read(c, &x, true);
	assert!(mesh.settle(), "{}", mesh.dump());
	assert_eq!(mesh.parent(c, &x), Some(b));
	mesh.hold(c, b);

	mesh.read(b, &x, false);
	assert!(mesh.run());
	assert_eq!(mesh.parent(c, &x), Some(a), "c leaves for the skip");
	mesh.read(b, &x, true);
	assert!(mesh.run());
	assert_eq!(mesh.parent(c, &x), Some(b), "c comes back");

	let streams = mesh.streams(c, b);
	assert_eq!(streams.len(), 2, "the old cancel and the new pull wait");
	assert!(mesh.deliver_stream(c, b, &x, streams[1]));
	assert!(mesh.deliver_stream(c, b, &x, streams[0]));
	assert_eq!(mesh.relays[b].carry[&x].downstream.len(), 1);
	mesh.release(c, b);
	assert!(mesh.settle(), "{}", mesh.dump());
	assert_eq!(mesh.parent(c, &x), Some(b));
	mesh.steady("overtaken");
	assert_eq!(mesh.loops, 0);
}

/// A copy of another instance of the same path is never joined: `c`'s reader of the
/// new instance takes the skip although `b` offers the old one.
#[test]
fn a_copy_of_another_instance_is_never_joined() {
	let (a, b, c) = (1, 2, 3);
	let (mut mesh, old) = chain(5, &ranking(b, c));
	let new = mesh.broadcast(7, old.1.as_str(), 1);
	mesh.read(b, &old, true);
	mesh.read(c, &new, true);
	assert!(mesh.settle(), "{}", mesh.dump());
	assert!(mesh.offered(c, b, &old));
	assert_eq!(mesh.parent(c, &new), Some(a));
	assert!(!mesh.carries(b, &new));
	mesh.steady("instances");
}

/// `k` relays on a LAN, each with its own uplink and a reader of every broadcast. In
/// a full mesh each broadcast crosses one uplink. In a ring each relay joins its
/// lower-ranked neighbour, so a relay ranked below both of its own keeps an uplink.
#[test]
fn lan_relays_share_uplink_copies() {
	const K: usize = 6;
	let o = 0;
	let relays: Vec<Node> = (1..=K).collect();
	for full in [true, false] {
		let mut mesh = Mesh::new(K + 1);
		mesh.origin(o, 7);
		for &relay in &relays {
			mesh.join(o, relay, 3);
		}
		for i in 0..K {
			for j in i + 1..K {
				if full || j == i + 1 || (i == 0 && j == K - 1) {
					mesh.join(relays[i], relays[j], 1);
				}
			}
		}
		let casts: Vec<Cast> = PATHS.iter().map(|path| mesh.broadcast(7, path, 0)).collect();
		for cast in &casts {
			for &relay in &relays {
				mesh.read(relay, cast, true);
			}
		}
		assert!(mesh.settle(), "{}", mesh.dump());
		let label = if full { "full mesh" } else { "ring" };
		mesh.steady(label);
		for cast in &casts {
			let rank = |relay: Node| rendezvous(&cast.1, id(relay));
			let minima = (0..K)
				.filter(|&i| {
					rank(relays[i]) < rank(relays[(i + 1) % K]) && rank(relays[i]) < rank(relays[(i + K - 1) % K])
				})
				.count();
			assert_eq!(mesh.copies(cast), if full { 1 } else { minima }, "{label}: {cast:?}");
		}
		let total: usize = casts.iter().map(|cast| mesh.copies(cast)).sum();
		// Recorded: readers arriving everywhere at once cross their offers on the way.
		assert_eq!((total, mesh.loops), if full { (8, 0) } else { (16, 0) }, "{label}");
	}
}

/// While a broadcast is pulled, a relay loses its link toward the publisher, and
/// later a relay drains: GOAWAY ranks every source through it last.
#[test]
fn a_lost_route_and_a_drain_move_pulls_without_a_loop() {
	let (a, b, c) = (1, 2, 3);
	let (mut mesh, x) = chain(5, &ranking(b, c));
	mesh.read(b, &x, true);
	mesh.read(c, &x, true);
	assert!(mesh.settle(), "{}", mesh.dump());
	assert_eq!(mesh.parent(c, &x), Some(b));

	mesh.disconnect(a, b);
	assert!(mesh.settle(), "{}", mesh.dump());
	assert_eq!(mesh.parent(c, &x), Some(a));
	assert_eq!(mesh.parent(b, &x), Some(c), "b reaches the publisher only through c");
	mesh.steady("route loss");

	mesh.connect(a, b);
	assert!(mesh.settle(), "{}", mesh.dump());
	mesh.steady("reconnected");
	mesh.goaway(b);
	assert!(mesh.settle(), "{}", mesh.dump());
	assert_eq!(mesh.parent(c, &x), Some(a), "c leaves the draining relay");
	mesh.steady("drain");
	// The pull along b's new route crosses c's join of b's copy.
	assert!(mesh.loops <= 2, "{} steps with a cycle", mesh.loops);
}

/// A copy ranks by its links' priority before its cost, as a route does: `c` joins
/// `b`'s copy over a cheap link only while that link's priority is no lower than the
/// skip's.
#[test]
fn a_copy_ranks_by_priority_before_cost() {
	let (o, a, b, c) = (0, 1, 2, 3);
	for (low, joins) in [(false, true), (true, false)] {
		let mut mesh = Mesh::new(4);
		mesh.origin(o, 7);
		mesh.join(o, a, 1);
		mesh.join(a, b, 3);
		let priority = if low { -1 } else { 0 };
		mesh.link(
			b,
			c,
			SimLink::cost(1).priority(priority),
			SimLink::cost(1).priority(priority),
		);
		mesh.join(a, c, 5);
		let x = mesh.broadcast(7, &ranking(b, c), 0);
		mesh.read(b, &x, true);
		mesh.read(c, &x, true);
		assert!(mesh.settle(), "{}", mesh.dump());
		assert_eq!(mesh.parent(c, &x), Some(if joins { b } else { a }), "low {low}");
		mesh.steady(&format!("low {low}"));
	}
}

const HOME: Node = 0;
const NEAR: Node = 1;
const FAR: Node = 2;
const TAIL: Node = 3;
const ORIGIN: Node = 4;
const SIDE: Node = 5;

/// Two relays with an uplink each to the publisher's relay, `NEAR`'s the cheaper, and
/// a cheap link between them. `TAIL` hangs off `FAR`, and `SIDE` off `NEAR`.
fn uplinks(path: &str) -> (Mesh, Cast) {
	let mut mesh = Mesh::new(6);
	mesh.origin(ORIGIN, 7);
	mesh.join(HOME, NEAR, 10);
	mesh.join(HOME, FAR, 12);
	mesh.join(NEAR, FAR, 3);
	mesh.join(FAR, TAIL, 1);
	mesh.join(NEAR, SIDE, 1);
	mesh.join(ORIGIN, HOME, 1);
	let x = mesh.broadcast(7, path, 0);
	assert!(mesh.settle(), "{}", mesh.dump());
	(mesh, x)
}

/// `FAR` carries the broadcast and is farther from the publisher than `NEAR`. A reader
/// at `SIDE` pulls through `NEAR`, which joins `FAR`'s copy over the cheap link rather
/// than pull its own over its uplink, though it offers its other neighbours `SIDE`'s.
/// Babel's distance cannot allow this: `FAR`'s cost to the publisher is not below
/// `NEAR`'s.
#[test]
fn a_relay_joins_a_copy_farther_from_the_publisher() {
	let (mut mesh, x) = uplinks(&ranking(FAR, SIDE));
	mesh.read(FAR, &x, true);
	assert!(mesh.settle(), "{}", mesh.dump());
	mesh.read(SIDE, &x, true);
	assert!(mesh.settle(), "{}", mesh.dump());
	assert_eq!(mesh.parent(SIDE, &x), Some(NEAR));
	assert_eq!(mesh.parent(NEAR, &x), Some(FAR), "NEAR joins the farther copy");
	assert_eq!(mesh.parent(FAR, &x), Some(HOME));
	assert_eq!(mesh.carrier(SIDE, NEAR, &x), Some(FAR));
	assert_eq!(mesh.carrier(HOME, NEAR, &x), Some(SIDE));
	assert_eq!(
		mesh.sim
			.path(NEAR)
			.route_for(7, &Path::empty())
			.unwrap()
			.offer()
			.unwrap()
			.metric,
		11
	);
	mesh.steady("joined");

	mesh.read(FAR, &x, false);
	assert!(mesh.settle(), "{}", mesh.dump());
	assert_eq!(mesh.parent(NEAR, &x), Some(HOME));
	assert!(!mesh.carries(FAR, &x));
	mesh.steady("FAR's reader left");
	assert_eq!(mesh.loops, 0);
}

/// Both relays carry a copy, the higher-ranked one pulling the other's. When `FAR`'s
/// uplink closes the two swap if they must, each still fed from the publisher.
#[test]
fn two_copies_never_feed_each_other_once_quiet() {
	for path in [ranking(NEAR, FAR), ranking(FAR, NEAR)] {
		let (mut mesh, x) = uplinks(&path);
		mesh.read(FAR, &x, true);
		assert!(mesh.settle(), "{}", mesh.dump());
		mesh.read(NEAR, &x, true);
		mesh.read(TAIL, &x, true);
		assert!(mesh.settle(), "{}", mesh.dump());
		mesh.steady(&path);
		assert_eq!(mesh.loops, 0, "{path}");

		mesh.disconnect(HOME, FAR);
		assert!(mesh.settle(), "{}", mesh.dump());
		assert_eq!(mesh.parent(NEAR, &x), Some(HOME), "{path}");
		assert_eq!(mesh.parent(FAR, &x), Some(NEAR), "{path}");
		assert_eq!(mesh.parent(TAIL, &x), Some(FAR), "{path}");
		mesh.steady(&format!("{path}: FAR's uplink closed"));
		assert!(mesh.loops <= 3, "{path}: {} steps with a cycle", mesh.loops);
	}
}

/// The publisher leaves while two relays carry a copy. Every copy goes with its
/// route, and nothing is left pulling or offered.
#[test]
fn a_copy_that_goes_takes_its_offers_along() {
	let (mut mesh, x) = uplinks("live/x");
	mesh.read(FAR, &x, true);
	mesh.read(NEAR, &x, true);
	mesh.read(TAIL, &x, true);
	assert!(mesh.settle(), "{}", mesh.dump());

	mesh.disconnect(ORIGIN, HOME);
	assert!(mesh.settle(), "{}", mesh.dump());
	for relay in [HOME, NEAR, FAR, TAIL, SIDE] {
		assert!(!mesh.carries(relay, &x), "relay {relay}");
		assert!(mesh.relays[relay].heard.is_empty(), "relay {relay}");
	}
}

/// Random relay meshes with two publishers under churn: readers come and go,
/// sessions drop and return, messages are held and delivered out of step, lingers
/// run out, time passes, and relays drain, with links held or not and some links at
/// a lower priority. Once churn stops every mesh settles on the steady state, and a
/// broadcast nobody reads has sent no copy offer. The pulls form a cycle only while a
/// change crosses a link, which the sweep counts.
#[test]
fn sweep_cache_routing() {
	for holds in [true, false] {
		sweep(SEED, 100, holds);
	}
}

/// The gate's seed, and the long sweep's unless `MOQ_ROUTE_SEED` names another.
const SEED: u64 = 0xcac4_e5ee;

/// The sweep at length from `MOQ_ROUTE_SEED` over `MOQ_ROUTE_CASES` scenarios each way,
/// which prints what it measured.
#[test]
#[ignore = "slow: nightly"]
fn sweep_cache_routing_long() {
	let setting = |name: &str, fallback: u64| std::env::var(name).map_or(fallback, |value| value.parse().expect(name));
	let seed = setting("MOQ_ROUTE_SEED", SEED);
	let cases = setting("MOQ_ROUTE_CASES", 3000) as usize;
	println!("MOQ_ROUTE_SEED={seed} MOQ_ROUTE_CASES={cases}");
	for holds in [true, false] {
		let out = sweep(seed, cases, holds);
		println!("holds {holds}: {out:?}");
	}
}

/// What a sweep measured.
#[derive(Debug, Default)]
struct Sweep {
	/// Steps after which some broadcast's pulls formed a cycle, of all steps.
	loops: usize,
	steps: usize,
	/// Scenarios with any such step.
	looped: usize,
	/// Steps after which a reader's pulls led into a cycle.
	starved: usize,
	/// The most deliveries in a row after which a cycle remained.
	longest: usize,
	/// Copy offers and their ends sent, and reader changes, during churn.
	warm: usize,
	reads: usize,
	/// Relays left following their route because they offered the neighbours they
	/// serve different copies, once quiet.
	split: usize,
}

fn sweep(seed: u64, scenarios: usize, holds: bool) -> Sweep {
	const RELAYS: usize = 6;
	let mut rng = XorShift(seed);
	let mut out = Sweep::default();
	for scenario in 0..scenarios {
		let label = format!("seed {seed} holds {holds} scenario {scenario}");
		let origins = [RELAYS, RELAYS + 1];
		let mut mesh = Mesh::new(RELAYS + 2);
		mesh.origin(origins[0], 100);
		mesh.origin(origins[1], 200);
		if rng.chance(30) {
			mesh.legacy(rng.next(RELAYS as u64) as usize);
		}
		let mut links: Vec<(Node, Node)> = (0..RELAYS).map(|relay| (relay, (relay + 1) % RELAYS)).collect();
		for _ in 0..2 {
			let (a, b) = (rng.next(RELAYS as u64) as usize, rng.next(RELAYS as u64) as usize);
			if a != b && !links.contains(&(a, b)) && !links.contains(&(b, a)) {
				links.push((a, b));
			}
		}
		for origin in origins {
			let a = rng.next(RELAYS as u64) as usize;
			links.push((origin, a));
			links.push((origin, (a + 1 + rng.next(RELAYS as u64 - 1) as usize) % RELAYS));
		}
		for &(a, b) in &links {
			let cost = 1 + rng.next(5);
			let mut side = || SimLink::cost(cost).priority(if rng.chance(15) { -1 } else { 0 });
			let (ab, ba) = (side(), side());
			mesh.link(a, b, ab, ba);
		}
		let read = [
			mesh.broadcast(100, "live/a", 0),
			mesh.broadcast(100, "live/b", 0),
			mesh.broadcast(200, "live/a", 0),
			// Another instance of the first.
			mesh.broadcast(100, "live/a", 1),
		];
		let idle = mesh.broadcast(100, "live/idle", 0);
		assert!(mesh.settle(), "{label}\n{}", mesh.dump());

		let warm = mesh.warm_msgs;
		for _ in 0..60 {
			match rng.next(12) {
				0..=2 => {
					let (relay, cast) = (rng.next(RELAYS as u64) as usize, &read[rng.next(4) as usize]);
					let reading = mesh.reading(relay, cast);
					mesh.read(relay, cast, !reading);
					out.reads += 1;
				}
				3 => {
					let (a, b) = links[rng.next(links.len() as u64) as usize];
					match mesh.connected(a, b) {
						true => mesh.disconnect(a, b),
						false => mesh.connect(a, b),
					}
				}
				4 => {
					let (a, b) = links[rng.next(links.len() as u64) as usize];
					let (from, to) = if rng.chance(50) { (a, b) } else { (b, a) };
					match rng.chance(70) {
						true if holds => mesh.hold(from, to),
						true => {}
						false => mesh.release(from, to),
					}
				}
				5 => mesh.expire(rng.next(RELAYS as u64) as usize, &read[rng.next(4) as usize]),
				6 if rng.chance(50) => {
					mesh.tick();
				}
				6 if rng.chance(20) => mesh.goaway(rng.next(RELAYS as u64) as usize),
				_ => {
					for _ in 0..rng.next(8) {
						let busy = mesh.busy();
						if busy.is_empty() {
							break;
						}
						mesh.deliver_one(&busy[rng.next(busy.len() as u64) as usize]);
					}
				}
			}
		}
		out.warm += mesh.warm_msgs - warm;

		for &(a, b) in &links {
			mesh.release(a, b);
			mesh.release(b, a);
			if !mesh.connected(a, b) {
				mesh.connect(a, b);
			}
		}
		if !mesh.settle() {
			panic!("{label}\n{}{}", mesh.dump(), mesh.trace(3));
		}
		mesh.steady(&label);
		assert_eq!(mesh.warm_sent(&idle), 0, "{label}");
		out.loops += mesh.loops;
		out.steps += mesh.steps;
		out.looped += usize::from(mesh.loops > 0);
		out.starved += mesh.starved;
		out.longest = out.longest.max(mesh.longest);
		out.split += mesh.split();
	}
	out
}
