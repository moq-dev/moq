//! A prototype router that keeps a route free of loops by its path alone.
//!
//! It exists to be measured against [`Router`], which keeps Babel's distance: the
//! comparison in `compare.rs` runs one script on both. Nothing outside the tests
//! uses it.
//!
//! A route is one per publisher and names every relay it crossed, each with the
//! session that relay learned the route on. A relay uses the cheapest route whose
//! path does not name it, at once, with no distance, seqno, request, or echo.
//! [`Rules`] switches on what the comparison showed that leaves wanting:
//!
//! - **Causes.** When a session closes, the relay that learned routes on it names
//!   that hop in a [`Msg::Dead`], and every relay drops every route through the hop,
//!   whichever neighbour offered it. One message per session covers every publisher.
//! - **Holds.** A relay with no route left keeps its claims, unavailable, until the
//!   neighbours that learned the route from it have answered: with another way, or
//!   with nothing once those behind them have answered in turn. Then it lets go.
//! - **Epochs.** A closed session casts doubt on the node at its far end, since a
//!   dead relay is reported one neighbour at a time. No relay moves to a route
//!   through a node in doubt until that node advances its epoch.
//! - **Copies.** A relay with a reader for a broadcast offers a route to its copy,
//!   and pulls each broadcast it has demand for from the cheapest of its publisher's
//!   route and the copies offered, which `cache.rs` proves.
//!
//! Left out, since the comparison does not need them: scoped grants, qualified
//! keys, interests, priorities, upstream marks, and RTT. GOAWAY only moves
//! pulls off a neighbour's copy.

use std::{
	collections::{BTreeMap, BTreeSet, VecDeque},
	sync::Arc,
};

use super::*;

/// One relay on a route's path, and the session it learned the route on. The
/// publisher names itself with session 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Hop {
	pub node: Id,
	pub session: u32,
}

/// Which parts of the prototype run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Rules {
	/// A relay that loses a session names the hop that died, and every relay drops
	/// every route through it. Off, each neighbour's copy lives until that neighbour
	/// withdraws it: a bare path vector.
	pub causes: bool,
	/// One [`Msg::Dead`] says what became of every route through its hop on that
	/// session. Off, each route is ended, held, or moved by its own message.
	pub batch: bool,
	/// A relay offers a neighbour no route whose path names that neighbour.
	pub horizon: bool,
	/// A relay with no route left keeps its claims, unavailable, while something
	/// may still bring the route back, and its neighbours keep theirs.
	pub holds: bool,
	/// A closed session casts doubt on the node at its far end: no relay moves to a
	/// route through that node until the node says it is still there, by advancing
	/// its epoch.
	pub epochs: bool,
	/// The same doubt on a timer: it lifts by itself after this many hops of delay,
	/// with no epoch and so no message. 0 is off.
	pub wait: u64,
	/// A relay on a copy moves onto another only if that copy's carrier ranks below
	/// the one it is on, so relays never trade copies.
	pub ranked: bool,
}

impl Rules {
	/// Paths and nothing else: what lite-06 does per broadcast, here per publisher.
	pub const BARE: Self = Self {
		causes: false,
		batch: false,
		horizon: false,
		holds: false,
		epochs: false,
		wait: 0,
		ranked: true,
	};

	/// The quest's candidate: a withdrawal names the hop that died.
	pub const CAUSES: Self = Self {
		causes: true,
		..Self::BARE
	};

	/// Causes, one message per cause and session, and holds.
	pub const HOLDS: Self = Self {
		batch: true,
		holds: true,
		..Self::CAUSES
	};

	/// Holds, and a doubt that lifts by itself after `wait` hops of delay.
	pub const fn waits(wait: u64) -> Self {
		Self { wait, ..Self::HOLDS }
	}

	/// Holds, and epochs.
	pub const EPOCHS: Self = Self {
		epochs: true,
		..Self::HOLDS
	};
}

/// A message on a session.
#[derive(Clone, Debug)]
pub(super) enum Msg {
	/// Starts a route, or replaces its path and cost.
	Offer { id: Id, path: Arc<[Hop]>, cost: Metric },
	/// The sender has no route left but keeps the claims: it may yet recover.
	Hold { id: Id },
	/// Ends a route, and the claims that rode it.
	End { id: Id },
	/// The session `hop` names closed, so every route through it is dead, whoever
	/// offered it. Of the sender's own routes through it, each one `moved` lists
	/// continues on its new path. The rest are ended if `gone`, and held if not.
	///
	/// `far` is the node at the other end of that session, and its epoch as the
	/// relay that saw the session close knew it.
	Dead {
		hop: Hop,
		far: (Id, u64),
		moved: Vec<(Id, Arc<[Hop]>, Metric)>,
		gone: bool,
	},
	/// `node` is at `epoch`: it was still there after every session it saw close
	/// before that.
	Epoch { node: Id, epoch: u64 },
	/// A copy of one broadcast is `cost` away through the sender, having come from
	/// its publisher along `path`: 0 at its carrier, the relay with a reader where the
	/// copy left the publisher's route.
	Warm {
		cast: Cast,
		path: Arc<[Hop]>,
		cost: Metric,
		carrier: Id,
	},
	/// The sender offers no copy of that broadcast any more.
	Cold { cast: Cast },
	/// Changes to a route's claims, complete as of the publisher's revision `rev`.
	Claims {
		id: Id,
		changes: Vec<(Path, Option<Rev>)>,
		rev: Rev,
	},
}

/// A publisher's claims, each with the revision its life began at.
type Claims = BTreeMap<Path, Rev>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
	Live,
	/// Its path holds a hop this relay knows is dead, and the neighbour has not
	/// said what became of it yet.
	Dead,
	/// The neighbour lost the route and holds its claims.
	Held,
}

/// A route a neighbour offered.
#[derive(Clone, Debug)]
struct Heard {
	/// The last path it offered.
	path: Arc<[Hop]>,
	cost: Metric,
	state: State,
	claims: Claims,
	rev: Rev,
}

/// What this relay last sent a neighbour for a route.
#[derive(Clone, Debug)]
struct Sent {
	/// The last path it was offered.
	path: Arc<[Hop]>,
	cost: Metric,
	held: bool,
	claims: Claims,
	rev: Rev,
}

struct Session {
	id: u32,
	/// How the neighbour names itself in a path.
	hop: Id,
	/// Whether the neighbour passes routes on, as a relay does and a client does not.
	forwards: bool,
	/// Whether the neighbour reads copies: a relay that chooses per broadcast.
	reads: bool,
	cost: Metric,
	rx: BTreeMap<Id, Heard>,
	tx: BTreeMap<Id, Sent>,
	/// The dead hops this neighbour was told of, while a route sent to it names one.
	told: BTreeSet<Hop>,
	/// The epoch this neighbour was told each node is at.
	epochs: BTreeMap<Id, u64>,
	/// The copies this neighbour offers.
	copies: BTreeMap<Cast, Copy>,
	/// The copies this neighbour was offered.
	casts: BTreeMap<Cast, Offer>,
}

/// The route a relay uses for a publisher, and the claims it shows.
#[derive(Clone, Debug)]
struct Route {
	/// The neighbour it goes through, or the one this relay holds under. `None` at
	/// the publisher itself, and at a relay holding on its own account.
	next: Option<Node>,
	/// The path this relay offers: its next hop's, then itself. `None` while held.
	path: Option<Arc<[Hop]>>,
	cost: Metric,
	claims: Claims,
	rev: Rev,
}

/// One broadcast: its publisher, its path, and its instance. Two instances of one
/// path are two broadcasts, and a copy of one never serves the other.
pub(super) type Cast = (Id, Path, u64);

/// A relay's demand for one broadcast, and where it pulls it from.
#[derive(Clone, Debug, Default)]
struct Carry {
	/// A reader of its own, such as a session that forwards nothing: the relay
	/// carries a copy, and offers it.
	reader: bool,
	/// Each neighbour's subscription streams that arrived and were not cancelled. A
	/// neighbour that moves back here opens a new stream, which can arrive before the
	/// old one's cancel.
	downstream: BTreeMap<Node, BTreeSet<u64>>,
	/// The neighbour it pulls through, and whether that joins a copy.
	parent: Option<(Node, bool)>,
	/// The stream of the pull through `parent`.
	stream: u64,
	/// Demand is gone, and the copy stays until the linger runs out.
	lingering: bool,
}

/// The draft's Rendezvous Hash of a broadcast's path and a relay, which ranks two
/// neighbours offering each other a copy.
pub(super) fn rendezvous(path: &Path, node: Id) -> u64 {
	let mut hash: u64 = 0x420C_0DEC_B00B;
	for byte in path.as_str().bytes().chain(node.to_le_bytes()) {
		hash = (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3);
	}
	hash
}

/// A copy of a broadcast offered to a neighbour: the path it took from its
/// publisher, its cost from its carrier, and its carrier.
pub(super) type Offer = (Arc<[Hop]>, Metric, Id);

/// A copy of a broadcast a neighbour offers.
#[derive(Clone, Debug)]
struct Copy {
	/// The path the copy took from its publisher to that neighbour.
	path: Arc<[Hop]>,
	cost: Metric,
	carrier: Id,
	/// Its path holds a hop this relay knows is dead.
	dead: bool,
}

/// Where a relay would pull a broadcast from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Source {
	pub next: Node,
	pub cost: Metric,
	/// The path the broadcast then took to this relay.
	pub path: Arc<[Hop]>,
	/// Whether it joins a copy, rather than follow the publisher's route.
	pub warm: bool,
	/// That copy's carrier.
	pub carrier: Id,
}

/// What a relay sends one neighbour for a route.
enum Want {
	Path(Arc<[Hop]>, Metric),
	Hold,
	End,
	/// Whatever it was sent last: the neighbour is the next hop this relay holds
	/// under, and waits for this relay's answer.
	Keep,
}

pub(super) struct Relay {
	hop: Id,
	rules: Rules,
	/// Sends only its own routes, like a client.
	pub leaf: bool,
	opened: u32,
	sessions: BTreeMap<Node, Session>,
	own: Option<Id>,
	routes: BTreeMap<Id, Route>,
	/// Dead hops, each kept while a neighbour's route still names it.
	dead: BTreeSet<Hop>,
	/// The newest epoch heard of each node a neighbour's route names, its own included.
	epochs: BTreeMap<Id, u64>,
	/// The epoch each such node was at when a session of its was last seen to close.
	doubt: BTreeMap<Id, u64>,
	/// With a timer in place of epochs: when the doubt on each node lifts.
	until: BTreeMap<Id, u64>,
	now: u64,
	/// Each broadcast this relay has demand for, or still pulls while its linger runs.
	carry: BTreeMap<Cast, Carry>,
	/// Never sends or reads a copy: it pulls along its publisher's route alone, as
	/// an endpoint that does not choose per broadcast.
	pub legacy: bool,
	/// Neighbours that sent GOAWAY: no pull joins, or stays on, their copies.
	draining: BTreeSet<Node>,
	/// The source last chosen for each broadcast, pulled or only offered on: its
	/// neighbour, whether it is a copy, and that copy's carrier.
	via: BTreeMap<Cast, (Node, bool, Id)>,
	/// Subscription changes for the request streams: a pull, or a cancel, each on
	/// its stream.
	subs: Vec<(Node, Cast, u64, bool)>,
	/// The last subscription stream opened.
	streams: u64,
	/// A pull moved from one neighbour to another.
	pub moves: usize,
	shown: BTreeMap<(Id, Path), (Rev, bool)>,
	out: Vec<(Node, Msg)>,
	events: Vec<(Id, Path, Kind)>,
}

fn names(path: &[Hop], node: Id) -> bool {
	path.iter().any(|hop| hop.node == node)
}

/// Whether the relay that offered `path` learned it from `node`.
fn child(path: &[Hop], node: Id) -> bool {
	path.len() >= 2 && path[path.len() - 2].node == node
}

fn state(dead: &BTreeSet<Hop>, path: &[Hop]) -> State {
	match path.iter().any(|hop| dead.contains(hop)) {
		true => State::Dead,
		false => State::Live,
	}
}

impl Relay {
	pub fn new(hop: Id, rules: Rules) -> Self {
		Self {
			hop,
			rules,
			leaf: false,
			opened: 0,
			sessions: BTreeMap::new(),
			own: None,
			routes: BTreeMap::new(),
			dead: BTreeSet::new(),
			epochs: BTreeMap::new(),
			doubt: BTreeMap::new(),
			until: BTreeMap::new(),
			now: 0,
			carry: BTreeMap::new(),
			legacy: false,
			draining: BTreeSet::new(),
			via: BTreeMap::new(),
			subs: Vec::new(),
			streams: 0,
			moves: 0,
			shown: BTreeMap::new(),
			out: Vec::new(),
			events: Vec::new(),
		}
	}

	pub fn drain(&mut self) -> Vec<(Node, Msg)> {
		std::mem::take(&mut self.out)
	}

	pub fn events(&mut self) -> Vec<(Id, Path, Kind)> {
		std::mem::take(&mut self.events)
	}

	pub fn connect(&mut self, peer: Node, hop: Id, forwards: bool, reads: bool, cost: Metric) {
		self.opened += 1;
		let session = Session {
			id: self.opened,
			hop,
			forwards,
			reads,
			cost,
			rx: BTreeMap::new(),
			tx: BTreeMap::new(),
			told: BTreeSet::new(),
			epochs: BTreeMap::new(),
			copies: BTreeMap::new(),
			casts: BTreeMap::new(),
		};
		self.sessions.insert(peer, session);
		let ids: Vec<Id> = self.routes.keys().copied().collect();
		for id in ids {
			self.advertise(id);
		}
		self.warm(None);
	}

	/// The session to `peer` closed: every route learned on it is dead.
	pub fn disconnect(&mut self, peer: Node) {
		let Some(session) = self.sessions.remove(&peer) else {
			return;
		};
		let hop = Hop {
			node: self.hop,
			session: session.id,
		};
		let lost = session.rx.keys().copied().collect();
		let far = (session.hop, self.epochs.get(&session.hop).copied().unwrap_or(0));
		self.draining.remove(&peer);
		for carry in self.carry.values_mut() {
			carry.downstream.remove(&peer);
		}
		if self.rules.epochs && session.forwards {
			// This relay is still here, whatever its neighbour tells the others. A
			// neighbour that passes nothing on tells nobody.
			*self.epochs.entry(self.hop).or_default() += 1;
		}
		self.fail(hop, far, None, Vec::new(), false, lost);
		self.spread(self.hop);
	}

	pub fn originate(&mut self, id: Id) {
		self.own = Some(id);
		let path = Arc::from([Hop {
			node: self.hop,
			session: 0,
		}]);
		let route = Route {
			next: None,
			path: Some(path),
			cost: 0,
			claims: Claims::new(),
			rev: 0,
		};
		self.routes.insert(id, route);
		self.advertise(id);
	}

	pub fn announce(&mut self, claim: Path) {
		self.change(claim, true);
	}

	pub fn unannounce(&mut self, claim: Path) {
		self.change(claim, false);
	}

	fn change(&mut self, claim: Path, live: bool) {
		let id = self.own.expect("not a publisher");
		let route = self.routes.get_mut(&id).unwrap();
		route.rev += 1;
		match live {
			true => route.claims.insert(claim, route.rev),
			false => route.claims.remove(&claim),
		};
		self.show(id);
		self.advertise(id);
	}

	pub fn recv(&mut self, from: Node, msg: Msg) {
		let Some(session) = self.sessions.get_mut(&from) else {
			return;
		};
		match msg {
			Msg::Offer { id, path, cost } => {
				let state = state(&self.dead, &path);
				match session.rx.get_mut(&id) {
					Some(heard) => {
						heard.path = path;
						heard.cost = cost;
						heard.state = state;
					}
					None => {
						let heard = Heard {
							path,
							cost,
							state,
							claims: Claims::new(),
							rev: 0,
						};
						session.rx.insert(id, heard);
					}
				}
				self.update(id);
			}
			Msg::Hold { id } => {
				if let Some(heard) = session.rx.get_mut(&id) {
					heard.state = State::Held;
				}
				self.update(id);
			}
			Msg::End { id } => {
				session.rx.remove(&id);
				// A ROUTE_END retires the copies offered under the route.
				session.copies.retain(|cast, _| cast.0 != id);
				self.update(id);
			}
			Msg::Claims { id, changes, rev } => {
				let Some(heard) = session.rx.get_mut(&id) else {
					return;
				};
				for (claim, since) in changes {
					match since {
						Some(since) => heard.claims.insert(claim, since),
						None => heard.claims.remove(&claim),
					};
				}
				heard.rev = rev;
				self.update(id);
			}
			Msg::Dead { hop, far, moved, gone } => self.fail(hop, far, Some(from), moved, gone, BTreeSet::new()),
			Msg::Epoch { node, epoch } => {
				if self.epochs.get(&node).copied().unwrap_or(0) >= epoch {
					return;
				}
				self.epochs.insert(node, epoch);
				self.spread(node);
				for id in self.heard() {
					self.update(id);
				}
				self.warm(None);
			}
			Msg::Warm {
				cast,
				path,
				cost,
				carrier,
			} => {
				if self.legacy {
					return;
				}
				let dead = state(&self.dead, &path) == State::Dead;
				let copy = Copy {
					path,
					cost,
					carrier,
					dead,
				};
				let id = cast.0;
				session.copies.insert(cast, copy);
				self.warm(Some(id));
				self.collect();
			}
			Msg::Cold { cast } => {
				let id = cast.0;
				session.copies.remove(&cast);
				self.warm(Some(id));
				self.collect();
			}
		}
	}

	/// The session `hop` names closed. `from` reported it, with what became of its
	/// own routes, or this relay saw it close and `lost` what it had learned there.
	fn fail(
		&mut self,
		hop: Hop,
		far: (Id, u64),
		from: Option<Node>,
		moved: Vec<(Id, Arc<[Hop]>, Metric)>,
		gone: bool,
		lost: BTreeSet<Id>,
	) {
		let mut touched = lost;
		let (me, rules) = (self.hop, self.rules);
		for (&peer, session) in self.sessions.iter_mut() {
			let reported = from == Some(peer);
			let through: Vec<Id> = (session.rx.iter())
				.filter(|(_, heard)| heard.path.contains(&hop))
				.map(|(id, _)| *id)
				.collect();
			for id in through {
				touched.insert(id);
				let heard = session.rx.get_mut(&id).unwrap();
				let moved = moved.iter().find(|(named, ..)| *named == id).filter(|_| reported);
				if let Some((_, path, cost)) = moved {
					heard.state = state(&self.dead, path);
					heard.path = path.clone();
					heard.cost = *cost;
				} else if reported && rules.batch {
					if gone || !rules.holds {
						session.rx.remove(&id);
						session.copies.retain(|cast, _| cast.0 != id);
					} else if !child(&heard.path, me) {
						heard.state = State::Held;
					}
					// A neighbour that holds under this relay answers once those behind it have.
				} else if heard.state == State::Live {
					heard.state = State::Dead;
				}
			}
		}
		for session in self.sessions.values_mut() {
			for copy in session.copies.values_mut() {
				copy.dead |= copy.path.contains(&hop);
			}
		}
		self.dead.insert(hop);
		if rules.wait > 0 {
			let until = self.until.entry(far.0).or_default();
			*until = (self.now + rules.wait).max(*until);
			touched.extend(self.heard());
		}
		if rules.epochs {
			let doubt = self.doubt.entry(far.0).or_default();
			*doubt = far.1.max(*doubt);
			// Any route through that node may have become unusable.
			touched.extend(self.heard());
		}
		for &id in &touched {
			self.select(id);
			self.show(id);
		}
		if rules.causes {
			self.report(hop, far, from);
		}
		for &id in &touched {
			self.advertise(id);
		}
		self.warm(None);
		self.collect();
	}

	/// Tell each neighbour that was sent a route through `hop` what became of those
	/// routes, in one message where the rules allow.
	fn report(&mut self, hop: Hop, far: (Id, u64), from: Option<Node>) {
		let peers: Vec<Node> = self.sessions.keys().copied().collect();
		for peer in peers {
			let through: Vec<Id> = (self.sessions[&peer].tx.iter())
				.filter(|(_, sent)| sent.path.contains(&hop))
				.map(|(id, _)| *id)
				.collect();
			if through.is_empty() {
				let session = self.sessions.get_mut(&peer).unwrap();
				let copied = session.casts.values().any(|(path, ..)| path.contains(&hop));
				if copied && from != Some(peer) && session.told.insert(hop) {
					let (moved, gone) = (Vec::new(), true);
					self.out.push((peer, Msg::Dead { hop, far, moved, gone }));
				}
				continue;
			}
			let first = self.sessions.get_mut(&peer).unwrap().told.insert(hop);
			if !self.rules.batch {
				// Each route's own message follows, and the reporter knows.
				if first && from != Some(peer) {
					let (moved, gone) = (Vec::new(), false);
					self.out.push((peer, Msg::Dead { hop, far, moved, gone }));
				}
				continue;
			}
			let (mut moved, mut held, mut ended, mut rest) = (Vec::new(), Vec::new(), Vec::new(), 0);
			for id in through {
				let want = self.want(id, peer);
				let session = self.sessions.get_mut(&peer).unwrap();
				match want {
					Want::Path(path, cost) => {
						let sent = session.tx.get_mut(&id).unwrap();
						sent.path = path.clone();
						sent.cost = cost;
						sent.held = false;
						moved.push((id, path, cost));
					}
					Want::Hold => {
						let sent = session.tx.get_mut(&id).unwrap();
						if !sent.held {
							sent.held = true;
							// The neighbour it learned the route from takes silence to mean
							// this relay holds under it, so it is told outright.
							let parent = child(&sent.path, session.hop);
							held.push((id, parent));
						}
						rest += 1;
					}
					Want::Keep => rest += 1,
					Want::End => {
						session.tx.remove(&id);
						session.casts.retain(|cast, _| cast.0 != id);
						ended.push(id);
					}
				}
			}
			let gone = rest == 0;
			let paths: Vec<Arc<[Hop]>> = moved.iter().map(|(_, path, _)| path.clone()).collect();
			if first || (gone && !ended.is_empty()) {
				self.out.push((peer, Msg::Dead { hop, far, moved, gone }));
				if !gone {
					let held = held.into_iter().filter(|(_, parent)| *parent);
					self.out.extend(held.map(|(id, _)| (peer, Msg::Hold { id })));
					self.out.extend(ended.into_iter().map(|id| (peer, Msg::End { id })));
				}
			} else {
				self.out.extend(
					moved
						.into_iter()
						.map(|(id, path, cost)| (peer, Msg::Offer { id, path, cost })),
				);
				self.out
					.extend(held.into_iter().map(|(id, _)| (peer, Msg::Hold { id })));
				self.out.extend(ended.into_iter().map(|id| (peer, Msg::End { id })));
			}
			// An epoch follows the route that names its node, or it would name nothing
			// the neighbour holds yet.
			for path in paths {
				self.tell(peer, &path);
			}
		}
	}

	fn update(&mut self, id: Id) {
		self.select(id);
		self.show(id);
		self.advertise(id);
		self.warm(Some(id));
		self.collect();
	}

	/// A reader for `cast` arrives at this relay, which carries a copy from now on.
	pub fn carry(&mut self, cast: Cast) {
		let id = cast.0;
		self.carry.entry(cast).or_default().reader = true;
		self.warm(Some(id));
	}

	/// The reader for `cast` leaves.
	pub fn uncarry(&mut self, cast: &Cast) {
		if let Some(carry) = self.carry.get_mut(cast) {
			carry.reader = false;
		}
		self.warm(Some(cast.0));
	}

	/// A subscription for `cast` from `from` arrives on `stream`, or its cancel.
	pub fn subscribed(&mut self, from: Node, cast: Cast, stream: u64, pull: bool) {
		if !self.sessions.contains_key(&from) {
			return;
		}
		let id = cast.0;
		let carry = self.carry.entry(cast).or_default();
		let streams = carry.downstream.entry(from).or_default();
		match pull {
			true => streams.insert(stream),
			false => streams.remove(&stream),
		};
		if streams.is_empty() {
			carry.downstream.remove(&from);
		}
		self.warm(Some(id));
	}

	/// `peer` sent GOAWAY: every pull on its copy moves off.
	pub fn goaway(&mut self, peer: Node) {
		if self.sessions.contains_key(&peer) && self.draining.insert(peer) {
			self.warm(None);
		}
	}

	/// The linger for `cast` runs out: a copy with no demand left goes.
	pub fn expire(&mut self, cast: &Cast) {
		if let Some(carry) = self.carry.get_mut(cast)
			&& carry.lingering
		{
			carry.lingering = false;
			if let Some((parent, _)) = carry.parent.take()
				&& self.sessions.contains_key(&parent)
			{
				self.subs.push((parent, cast.clone(), carry.stream, false));
			}
		}
		self.warm(Some(cast.0));
	}

	/// Where this relay pulls `cast` from: the cheapest of its next hop toward the
	/// publisher and each copy offered. Pulling through the next hop goes wherever
	/// that neighbour pulls: its copy when it offers one, else its own route. A copy
	/// whose path names this relay, or a neighbour it serves, would loop.
	///
	/// A relay already on a copy, its own included, joins another, its next hop's
	/// included, only if that copy's carrier ranks below, so copies crossing on their
	/// way never trade places round any cycle. Off a copy, it is bound the same way by
	/// the carriers of the neighbours that pull, or would pull, along its route, since
	/// what it joins they take with it. Left with no source and no such neighbour, it
	/// chooses afresh. It neither joins nor stays on a copy of a neighbour that sent
	/// GOAWAY, unless that is its next hop's. A tie stays on the current source, then
	/// goes to the copy.
	pub fn source(&self, cast: &Cast) -> Option<Source> {
		let carry = self.carry.get(cast);
		let reader = carry.is_some_and(|carry| carry.reader);
		let downstream = carry.map(|carry| &carry.downstream);
		let serves: BTreeSet<Id> = (downstream.into_iter().flat_map(BTreeMap::keys))
			.filter_map(|peer| self.sessions.get(peer))
			.map(|session| session.hop)
			.collect();
		let via = self.via.get(cast).copied();
		let pull = via.map(|(next, warm, _)| (next, warm));
		let rank = |node: Id| rendezvous(&cast.1, node);
		// The carrier of the copy this relay is on: the one it joins while that is still
		// offered, or its own, pulled along the publisher's route for a reader.
		let own = match via {
			Some((next, true, carrier)) => (self.sessions.get(&next))
				.and_then(|session| session.copies.get(cast))
				.filter(|copy| copy.carrier == carrier)
				.map(|_| carrier),
			Some((_, false, _)) if reader => Some(self.hop),
			_ => None,
		};
		let next = self
			.routes
			.get(&cast.0)
			.and_then(|route| route.next.zip(route.path.clone()));
		// Off a copy, the copies it would hand on are those of each neighbour that pulls
		// through it, or would on its route, since that neighbour's next hop's copy is no
		// choice: each offers its copy back along this relay's route. Joining another
		// would hand each of them another carrier, so it joins only one that ranks below
		// them all, and none until each that pulls through it has offered one: an offer
		// along any other path is from before it took the route, and a neighbour that
		// offers none may feed copies further on. `None` joins nothing it chooses.
		let chosen: Option<Option<Id>> = match own {
			Some(own) => Some(Some(own)),
			None => (self.sessions.iter())
				.filter_map(|(peer, session)| {
					let pulls = downstream.is_some_and(|downstream| downstream.contains_key(peer));
					let behind = (session.rx.get(&cast.0)).is_some_and(|heard| child(&heard.path, self.hop));
					let fresh = (session.copies.get(cast))
						.filter(|copy| next.as_ref().is_some_and(|(_, path)| copy.path.starts_with(path)))
						.map(|copy| copy.carrier);
					match fresh {
						Some(carrier) if pulls || behind => Some(Some(carrier)),
						None if pulls => Some(None),
						_ => None,
					}
				})
				.try_fold(None, |low: Option<Id>, carrier| {
					let carrier = carrier?;
					Some(Some(low.filter(|&low| rank(low) < rank(carrier)).unwrap_or(carrier)))
				}),
		};
		let mut options = Vec::new();
		for (&peer, session) in &self.sessions {
			let hop = next.as_ref().filter(|(next, _)| *next == peer);
			let Some(copy) = session.copies.get(cast) else {
				options.extend(hop.map(|(next, path)| Source {
					next: *next,
					cost: self.routes[&cast.0].cost,
					path: path.clone(),
					warm: false,
					carrier: 0,
				}));
				continue;
			};
			// As in `select`, doubt keeps a reader from moving to a copy, not on the one
			// it pulls: that copy's own neighbour says when it goes.
			let doubted = pull != Some((peer, true)) && self.doubted(&copy.path);
			let loops = copy
				.path
				.iter()
				.any(|hop| hop.node == self.hop || serves.contains(&hop.node));
			if copy.dead || loops || doubted {
				continue;
			}
			let ranked = |on: Id| copy.carrier == on || rank(copy.carrier) < rank(on);
			let on = match hop {
				Some(_) => Some(own),
				None => chosen,
			};
			if self.rules.ranked && !on.is_some_and(|on| on.is_none_or(ranked)) {
				continue;
			}
			// The next hop's copy is where its route leads; any other is a choice.
			if hop.is_none() && self.draining.contains(&peer) {
				continue;
			}
			let Some(cost) = copy.cost.checked_add(session.cost) else {
				continue;
			};
			let mut path = copy.path.to_vec();
			path.push(Hop {
				node: self.hop,
				session: session.id,
			});
			options.push(Source {
				next: peer,
				cost,
				path: path.into(),
				warm: true,
				carrier: copy.carrier,
			});
		}
		(options.into_iter()).min_by_key(|source| {
			let stay = pull == Some((source.next, source.warm));
			(source.cost, !stay, !source.warm, source.next)
		})
	}

	/// What this relay offers its neighbours for `cast`, under its route to the
	/// publisher: a copy it would join, at what joining it costs, for as long as that
	/// is cheaper than the publisher's route, or else, with a reader of its own, the
	/// copy it pulls along the publisher's route at no cost. A copy's cost so rises
	/// with every link from where it left the publisher's route.
	fn copy(&self, cast: &Cast) -> Option<Offer> {
		if self.own == Some(cast.0) || self.leaf || self.legacy {
			return None;
		}
		self.routes.get(&cast.0)?.path.as_ref()?;
		let source = self.source(cast)?;
		match self.carry.get(cast).is_some_and(|carry| carry.reader) {
			_ if source.warm => Some((source.path, source.cost, source.carrier)),
			true => Some((source.path, 0, self.hop)),
			false => None,
		}
	}

	/// Follow the demand for `cast`: pull it from its source, move with the source,
	/// and keep the pull while the linger runs once the demand is gone.
	fn pull(&mut self, cast: &Cast) {
		let source = self.source(cast);
		match &source {
			Some(source) => self
				.via
				.insert(cast.clone(), (source.next, source.warm, source.carrier)),
			None => self.via.remove(cast),
		};
		let source = source.map(|source| (source.next, source.warm));
		let Some(carry) = self.carry.get_mut(cast) else {
			return;
		};
		let demand = carry.reader || !carry.downstream.is_empty();
		if demand {
			carry.lingering = false;
		} else if carry.parent.is_some() {
			carry.lingering = true;
		}
		let parent = source.filter(|_| demand || carry.lingering);
		let old = std::mem::replace(&mut carry.parent, parent);
		let (old, new) = (old.map(|(next, _)| next), parent.map(|(next, _)| next));
		let stream = carry.stream;
		if new.is_some() && old != new {
			self.streams += 1;
			carry.stream = self.streams;
		}
		if !demand && parent.is_none() {
			self.carry.remove(cast);
		}
		if old == new {
			return;
		}
		if let Some(old) = old
			&& self.sessions.contains_key(&old)
		{
			self.subs.push((old, cast.clone(), stream, false));
		}
		if let Some(new) = new {
			self.subs.push((new, cast.clone(), self.streams, true));
		}
		if old.is_some() && new.is_some() {
			self.moves += 1;
		}
	}

	/// Bring each pull, and what each neighbour was offered, up to date, for the
	/// copies of `id` or for every copy. A neighbour that reads copies is offered this
	/// relay's, even one whose path names it, under the route it was sent.
	fn warm(&mut self, id: Option<Id>) {
		let mut casts: BTreeSet<Cast> = self.carry.keys().cloned().collect();
		for session in self.sessions.values() {
			casts.extend(session.copies.keys().cloned());
			casts.extend(session.casts.keys().cloned());
		}
		for cast in casts {
			if id.is_some_and(|id| id != cast.0) {
				continue;
			}
			// What this relay offers decides which offers it may join, so the two settle
			// together: joining a neighbour's copy only ends what it offers that neighbour.
			for _ in 0..4 {
				self.pull(&cast);
				if !self.offer(&cast) {
					break;
				}
			}
		}
	}

	/// Bring what each neighbour was offered for `cast` up to date. Whether anything changed.
	fn offer(&mut self, cast: &Cast) -> bool {
		let mut changed = false;
		{
			let cast = cast.clone();
			let copy = self.copy(&cast);
			let peers: Vec<Node> = self.sessions.keys().copied().collect();
			for peer in peers {
				let session = &self.sessions[&peer];
				// Even a copy whose path names the neighbour: it tells the neighbour that
				// pulling through this relay leads back to it.
				let routed = session.tx.contains_key(&cast.0) && session.reads;
				let want = copy.clone().filter(|_| routed);
				if session.casts.get(&cast) == want.as_ref() {
					continue;
				}
				changed = true;
				match want {
					Some((path, cost, carrier)) => {
						let session = self.sessions.get_mut(&peer).unwrap();
						session.casts.insert(cast.clone(), (path.clone(), cost, carrier));
						let msg = Msg::Warm {
							cast: cast.clone(),
							path: path.clone(),
							cost,
							carrier,
						};
						self.out.push((peer, msg));
						self.tell(peer, &path);
					}
					None => {
						self.sessions.get_mut(&peer).unwrap().casts.remove(&cast);
						self.out.push((peer, Msg::Cold { cast: cast.clone() }));
					}
				}
			}
		}
		changed
	}

	pub fn drain_subs(&mut self) -> Vec<(Node, Cast, u64, bool)> {
		std::mem::take(&mut self.subs)
	}

	/// The neighbour this relay pulls `cast` through, if it pulls it.
	pub fn parent(&self, cast: &Cast) -> Option<Node> {
		self.carry.get(cast)?.parent.map(|(next, _)| next)
	}

	pub fn reading(&self, cast: &Cast) -> bool {
		self.carry.get(cast).is_some_and(|carry| carry.reader)
	}

	pub fn downstream(&self, cast: &Cast) -> BTreeSet<Node> {
		self.carry
			.get(cast)
			.map(|carry| carry.downstream.keys().copied().collect())
			.unwrap_or_default()
	}

	/// The subscription streams for `cast` that arrived and were not cancelled.
	pub fn streams(&self, cast: &Cast) -> usize {
		(self.carry.get(cast)).map_or(0, |carry| carry.downstream.values().map(BTreeSet::len).sum())
	}

	/// Whether the copy of `cast` stays only while its linger runs.
	pub fn lingering(&self, cast: &Cast) -> bool {
		self.carry.get(cast).is_some_and(|carry| carry.lingering)
	}

	/// The broadcasts this relay pulls while its linger runs.
	pub fn lingers(&self) -> Vec<Cast> {
		(self.carry.iter())
			.filter(|(_, carry)| carry.lingering)
			.map(|(cast, _)| cast.clone())
			.collect()
	}

	/// The copy of `cast` this relay offers `peer`, and the one `peer` offers it.
	pub fn offers(&self, peer: Node, cast: &Cast) -> (Option<Offer>, Option<Offer>) {
		let Some(session) = self.sessions.get(&peer) else {
			return (None, None);
		};
		let sent = session.casts.get(cast).cloned();
		let heard = (session.copies.get(cast)).map(|copy| (copy.path.clone(), copy.cost, copy.carrier));
		(sent, heard)
	}

	/// Forget each dead hop no neighbour's route names any more.
	fn collect(&mut self) {
		let sessions = &self.sessions;
		self.dead.retain(|hop| {
			(sessions.values()).any(|session| {
				let routes = session.rx.values().any(|heard| heard.path.contains(hop));
				routes || session.copies.values().any(|copy| copy.path.contains(hop))
			})
		});
		let me = self.hop;
		let named = |node: &Id| {
			*node == me
				|| (sessions.values()).any(|session| {
					let routes = session.rx.values().any(|heard| names(&heard.path, *node));
					routes || session.copies.values().any(|copy| names(&copy.path, *node))
				})
		};
		self.epochs.retain(|node, _| named(node));
		self.doubt.retain(|node, _| named(node));
		self.until.retain(|node, _| named(node));
		for session in self.sessions.values_mut() {
			let tx = &session.tx;
			let casts = &session.casts;
			session.told.retain(|hop| {
				tx.values().any(|sent| sent.path.contains(hop)) || casts.values().any(|(path, ..)| path.contains(hop))
			});
			session.epochs.retain(|node, _| {
				tx.values().any(|sent| names(&sent.path, *node)) || casts.values().any(|(path, ..)| names(path, *node))
			});
		}
	}

	/// Every publisher a neighbour offered a route to.
	fn heard(&self) -> BTreeSet<Id> {
		(self.sessions.values())
			.flat_map(|session| session.rx.keys().copied())
			.collect()
	}

	/// Whether `path` crosses a node that a closed session cast doubt on, and that
	/// has not advanced its epoch since.
	fn doubted(&self, path: &[Hop]) -> bool {
		path.iter().any(|hop| {
			let behind =
				(self.doubt.get(&hop.node)).is_some_and(|at| self.epochs.get(&hop.node).copied().unwrap_or(0) <= *at);
			behind || self.until.contains_key(&hop.node)
		})
	}

	/// One hop of delay passes. Whether a doubt is still waiting to lift.
	pub fn tick(&mut self) -> bool {
		self.now += 1;
		let before = self.until.len();
		let now = self.now;
		self.until.retain(|_, until| *until > now);
		if self.until.len() < before {
			for id in self.heard() {
				self.update(id);
			}
			self.warm(None);
		}
		!self.until.is_empty()
	}

	/// Tell `peer` the epoch of each node `path` names, where it has an older one.
	fn tell(&mut self, peer: Node, path: &[Hop]) {
		let session = self.sessions.get_mut(&peer).unwrap();
		for hop in path {
			let epoch = self.epochs.get(&hop.node).copied().unwrap_or(0);
			if epoch > session.epochs.get(&hop.node).copied().unwrap_or(0) {
				session.epochs.insert(hop.node, epoch);
				let node = hop.node;
				self.out.push((peer, Msg::Epoch { node, epoch }));
			}
		}
	}

	/// Pass `node`'s epoch on to each neighbour that was sent a route naming it.
	fn spread(&mut self, node: Id) {
		let epoch = self.epochs.get(&node).copied().unwrap_or(0);
		for (&peer, session) in self.sessions.iter_mut() {
			let named = session.tx.values().any(|sent| names(&sent.path, node))
				|| session.casts.values().any(|(path, ..)| names(path, node));
			if named && epoch > session.epochs.get(&node).copied().unwrap_or(0) {
				session.epochs.insert(node, epoch);
				self.out.push((peer, Msg::Epoch { node, epoch }));
			}
		}
	}

	/// Pick the cheapest live route whose path does not name this relay, and take
	/// the newest claims any such route carries. With none left, hold or let go.
	fn select(&mut self, id: Id) {
		if self.own == Some(id) {
			return;
		}
		let was = self.routes.remove(&id);
		let current = was.as_ref().and_then(|route| route.next);
		let using = current.filter(|_| was.as_ref().is_some_and(|route| route.path.is_some()));
		// Stay on the next hop among equals.
		type Rank = (Metric, bool, Node);
		let mut best: Option<(Rank, Arc<[Hop]>)> = None;
		let mut hope = false;
		for (&peer, session) in &self.sessions {
			let Some(heard) = session.rx.get(&id) else {
				continue;
			};
			if heard.state != State::Live || names(&heard.path, self.hop) {
				continue;
			}
			// Doubt keeps a relay from moving to a route, not on the one it uses: that
			// one's own neighbour says when it goes.
			if Some(peer) != using && self.doubted(&heard.path) {
				// It may come back: the node in doubt has yet to say it is there.
				hope = true;
				continue;
			}
			let Some(cost) = heard.cost.checked_add(session.cost) else {
				continue;
			};
			let rank = (cost, Some(peer) != current, peer);
			if best.as_ref().is_none_or(|(best, _)| rank < *best) {
				let mut path = heard.path.to_vec();
				path.push(Hop {
					node: self.hop,
					session: session.id,
				});
				best = Some((rank, path.into()));
			}
		}
		let had = was.is_some();
		let (mut claims, mut rev) = was.map_or((Claims::new(), 0), |route| (route.claims, route.rev));
		for session in self.sessions.values() {
			if let Some(heard) = session.rx.get(&id)
				&& heard.state == State::Live
				&& !names(&heard.path, self.hop)
				&& heard.rev > rev
			{
				claims = heard.claims.clone();
				rev = heard.rev;
			}
		}
		let route = match best {
			Some(((cost, _, peer), path)) => Route {
				next: Some(peer),
				path: Some(path),
				cost,
				claims,
				rev,
			},
			None => {
				// Only a relay that had the route holds it.
				if !self.rules.holds || !had {
					return;
				}
				// Its next hop holds, or has yet to say what became of a dead route. A next
				// hop whose own route names this relay waits on this relay instead.
				let under = current
					.and_then(|next| self.sessions.get(&next))
					.and_then(|session| session.rx.get(&id))
					.is_some_and(|heard| heard.state != State::Live && !names(&heard.path, self.hop));
				if !under && !hope && !self.pending(id) {
					return;
				}
				Route {
					next: current.filter(|_| under),
					path: None,
					cost: 0,
					claims,
					rev,
				}
			}
		};
		self.routes.insert(id, route);
	}

	/// Whether a neighbour that learned `id` from this relay has yet to answer its loss.
	fn pending(&self, id: Id) -> bool {
		self.sessions.values().any(|session| {
			(session.rx.get(&id)).is_some_and(|heard| heard.state != State::Held && child(&heard.path, self.hop))
		})
	}

	/// Whether a neighbour offers a route to `id` that only doubt keeps this relay off.
	fn hopes(&self, id: Id) -> bool {
		self.sessions.values().any(|session| {
			(session.rx.get(&id)).is_some_and(|heard| {
				heard.state == State::Live && !names(&heard.path, self.hop) && self.doubted(&heard.path)
			})
		})
	}

	fn want(&self, id: Id, peer: Node) -> Want {
		let Some(route) = self.routes.get(&id) else {
			return Want::End;
		};
		if self.leaf && self.own != Some(id) {
			return Want::End;
		}
		match &route.path {
			Some(path) if self.rules.horizon && names(path, self.sessions[&peer].hop) => Want::End,
			Some(path) => Want::Path(path.clone(), route.cost),
			// The next hop it holds under hears nothing until those behind this relay answered.
			None if route.next == Some(peer) => match self.pending(id) || self.hopes(id) {
				true => Want::Keep,
				false => Want::End,
			},
			None => Want::Hold,
		}
	}

	/// Bring what each neighbour was sent for `id` up to date.
	fn advertise(&mut self, id: Id) {
		let peers: Vec<Node> = self.sessions.keys().copied().collect();
		for peer in peers {
			let want = self.want(id, peer);
			let session = self.sessions.get_mut(&peer).unwrap();
			let (path, cost) = match want {
				Want::Path(path, cost) => (path, cost),
				Want::Keep => continue,
				Want::Hold => {
					// A hold follows a route: there is nothing to hold before one.
					if let Some(sent) = session.tx.get_mut(&id)
						&& !sent.held
					{
						sent.held = true;
						self.out.push((peer, Msg::Hold { id }));
					}
					continue;
				}
				Want::End => {
					if session.tx.remove(&id).is_some() {
						session.casts.retain(|cast, _| cast.0 != id);
						self.out.push((peer, Msg::End { id }));
					}
					continue;
				}
			};
			let sent = session.tx.get(&id);
			let offer = sent.is_none_or(|sent| sent.held || sent.path != path || sent.cost != cost);
			let (was, at) = sent.map_or((Claims::new(), 0), |sent| (sent.claims.clone(), sent.rev));
			if offer {
				let offered = path.clone();
				self.out.push((
					peer,
					Msg::Offer {
						id,
						path: offered,
						cost,
					},
				));
				self.tell(peer, &path);
			}
			let route = &self.routes[&id];
			let session = self.sessions.get_mut(&peer).unwrap();
			if was != route.claims || at != route.rev {
				let mut changes: Vec<(Path, Option<Rev>)> = (route.claims.iter())
					.filter(|(claim, since)| was.get(*claim) != Some(*since))
					.map(|(claim, since)| (claim.clone(), Some(*since)))
					.collect();
				let ended = was.keys().filter(|claim| !route.claims.contains_key(*claim));
				changes.extend(ended.map(|claim| (claim.clone(), None)));
				let rev = route.rev;
				self.out.push((peer, Msg::Claims { id, changes, rev }));
			}
			let sent = Sent {
				path,
				cost,
				held: false,
				claims: route.claims.clone(),
				rev: route.rev,
			};
			session.tx.insert(id, sent);
		}
	}

	/// Tell the reader what changed for `id`.
	fn show(&mut self, id: Id) {
		let route = self.routes.get(&id);
		let available = route.is_some_and(|route| route.path.is_some());
		let empty = Claims::new();
		let claims = route.map_or(&empty, |route| &route.claims);
		let was: Vec<(Path, (Rev, bool))> = (self.shown.range((id, Path::empty())..))
			.take_while(|((named, _), _)| *named == id)
			.map(|((_, claim), shown)| (claim.clone(), *shown))
			.collect();
		for (claim, (since, shown)) in was {
			if claims.get(&claim) != Some(&since) {
				let kind = match route.is_some() {
					true => Kind::End,
					false => Kind::Offline,
				};
				self.events.push((id, claim.clone(), kind));
				self.shown.remove(&(id, claim));
			} else if shown != available {
				let kind = match available {
					true => Kind::Available,
					false => Kind::Unavailable,
				};
				self.events.push((id, claim.clone(), kind));
				self.shown.insert((id, claim), (since, available));
			}
		}
		for (claim, since) in claims {
			if !self.shown.contains_key(&(id, claim.clone())) {
				self.events.push((id, claim.clone(), Kind::Start));
				self.shown.insert((id, claim.clone()), (*since, available));
			}
		}
	}

	/// The neighbour this relay routes `id` through: `Some(None)` at the publisher.
	pub fn next(&self, id: Id) -> Option<Option<Node>> {
		let route = self.routes.get(&id)?;
		route.path.as_ref().map(|_| route.next)
	}

	pub fn cost(&self, id: Id) -> Option<Metric> {
		let route = self.routes.get(&id)?;
		route.path.as_ref().map(|_| route.cost)
	}

	/// Whether this relay shows `claim` from `id`, and whether it is available.
	pub fn shows(&self, id: Id, claim: &Path) -> Option<bool> {
		self.shown.get(&(id, claim.clone())).map(|(_, available)| *available)
	}

	/// Dead hops remembered, and routes and copies heard.
	pub fn retained(&self) -> (usize, usize) {
		let heard = (self.sessions.values())
			.map(|session| session.rx.len() + session.copies.len())
			.sum();
		(self.dead.len(), heard)
	}
}

/// Messages sent, as protocol counts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct PathStats {
	/// Offers, holds, ends, and causes.
	pub route_msgs: usize,
	/// Routes those messages name: a cause counts itself and each route it moved.
	pub route_items: usize,
	pub claim_msgs: usize,
	pub claim_items: usize,
	/// Copy offers and their ends, also counted as route messages.
	pub warm_msgs: usize,
	/// Subscriptions and their cancels.
	pub subscribes: usize,
	pub delivered: usize,
}

/// [`Sim`] for the prototype: one [`Relay`] per node, joined by ordered queues.
pub(super) struct PathSim {
	pub nodes: Vec<Relay>,
	rules: Rules,
	restarts: Vec<u32>,
	links: Vec<Vec<Option<Metric>>>,
	up: BTreeSet<(Node, Node)>,
	queues: BTreeMap<(Node, Node), VecDeque<Msg>>,
	/// Subscriptions, each on a stream of its own, unordered against the routes and
	/// each other: true pulls, false cancels.
	subs: BTreeMap<(Node, Node, Cast, u64), VecDeque<bool>>,
	held: BTreeSet<(Node, Node)>,
	/// Nodes whose process is gone and has yet to restart.
	stopped: BTreeSet<Node>,
	pub stats: PathStats,
	/// Copy offers and their ends sent per broadcast.
	pub warm: BTreeMap<Cast, usize>,
	pub events: Vec<(Node, Id, String, Kind)>,
}

impl PathSim {
	pub fn new(nodes: usize, rules: Rules) -> Self {
		Self {
			nodes: (0..nodes).map(|node| Relay::new(hop(node, 0), rules)).collect(),
			rules,
			restarts: vec![0; nodes],
			links: vec![vec![None; nodes]; nodes],
			up: BTreeSet::new(),
			queues: BTreeMap::new(),
			subs: BTreeMap::new(),
			held: BTreeSet::new(),
			stopped: BTreeSet::new(),
			stats: PathStats::default(),
			warm: BTreeMap::new(),
			events: Vec::new(),
		}
	}

	pub fn leaf(&mut self, node: Node) {
		self.nodes[node].leaf = true;
	}

	/// A link costing `ab` at `a` and `ba` at `b`.
	pub fn link(&mut self, a: Node, b: Node, ab: Metric, ba: Metric) {
		self.links[a][b] = Some(ab);
		self.links[b][a] = Some(ba);
		self.connect(a, b);
	}

	pub fn join(&mut self, a: Node, b: Node, cost: Metric) {
		self.link(a, b, cost, cost);
	}

	fn pair(a: Node, b: Node) -> (Node, Node) {
		(a.min(b), a.max(b))
	}

	pub fn connect(&mut self, a: Node, b: Node) {
		assert!(!self.connected(a, b), "{a}-{b} already up");
		self.up.insert(Self::pair(a, b));
		for (x, y) in [(a, b), (b, a)] {
			let cost = self.links[x][y].expect("a configured link");
			let peer = hop(y, self.restarts[y]);
			let forwards = !self.nodes[y].leaf;
			let reads = forwards && !self.nodes[y].legacy;
			self.nodes[x].connect(y, peer, forwards, reads, cost);
		}
		self.flush();
	}

	pub fn connected(&self, a: Node, b: Node) -> bool {
		self.up.contains(&Self::pair(a, b))
	}

	pub fn disconnect(&mut self, a: Node, b: Node) {
		self.up.remove(&Self::pair(a, b));
		for (x, y) in [(a, b), (b, a)] {
			self.queues.remove(&(x, y));
			self.subs.retain(|(from, to, ..), _| (*from, *to) != (x, y));
			self.held.remove(&(x, y));
			self.nodes[y].disconnect(x);
		}
		self.flush();
	}

	/// `peer` notices its session to `node` closed. `node`, whose process is gone,
	/// does nothing.
	pub fn sever(&mut self, node: Node, peer: Node) {
		self.up.remove(&Self::pair(node, peer));
		for key in [(node, peer), (peer, node)] {
			self.queues.remove(&key);
			self.subs.retain(|(from, to, ..), _| (*from, *to) != key);
			self.held.remove(&key);
		}
		self.nodes[peer].disconnect(node);
		self.flush();
	}

	fn peers(&self, node: Node) -> Vec<Node> {
		(0..self.nodes.len())
			.filter(|&peer| self.links[node][peer].is_some() && self.connected(node, peer))
			.collect()
	}

	/// The process at `node` stops: nothing passes between it and `peer` from now on,
	/// though `peer` has yet to notice, and no timer of its runs until `kill`.
	pub fn mute(&mut self, node: Node, peer: Node) {
		self.held.insert((node, peer));
		self.held.insert((peer, node));
		self.stopped.insert(node);
	}

	/// The process at `node` exits: its sessions close and its state is lost. It says
	/// nothing on the way out, so each neighbour notices and it does not.
	pub fn kill(&mut self, node: Node) {
		for peer in self.peers(node) {
			self.sever(node, peer);
		}
		self.stopped.remove(&node);
		self.restarts[node] += 1;
		let leaf = self.nodes[node].leaf;
		self.nodes[node] = Relay::new(hop(node, self.restarts[node]), self.rules);
		self.nodes[node].leaf = leaf;
	}

	pub fn originate(&mut self, node: Node, id: Id) {
		self.nodes[node].originate(id);
		self.flush();
	}

	pub fn announce(&mut self, node: Node, claim: &str) {
		self.nodes[node].announce(p(claim));
		self.flush();
	}

	pub fn unannounce(&mut self, node: Node, claim: &str) {
		self.nodes[node].unannounce(p(claim));
		self.flush();
	}

	/// A reader for `cast` arrives at `node`.
	pub fn carry(&mut self, node: Node, cast: &Cast) {
		self.nodes[node].carry(cast.clone());
		self.flush();
	}

	pub fn uncarry(&mut self, node: Node, cast: &Cast) {
		self.nodes[node].uncarry(cast);
		self.flush();
	}

	/// `node` sends GOAWAY on every session.
	pub fn goaway(&mut self, node: Node) {
		for peer in self.peers(node) {
			self.nodes[peer].goaway(node);
		}
		self.flush();
	}

	/// The linger for `cast` runs out at `node`.
	pub fn expire(&mut self, node: Node, cast: &Cast) {
		self.nodes[node].expire(cast);
		self.flush();
	}

	pub fn hold(&mut self, from: Node, to: Node) {
		self.held.insert((from, to));
	}

	pub fn release(&mut self, from: Node, to: Node) {
		self.held.remove(&(from, to));
	}

	/// The subscription streams with a change to deliver, held or not.
	pub fn pending_subs(&self) -> Vec<(Node, Node, Cast, u64)> {
		self.subs.keys().cloned().collect()
	}

	/// The subscription streams with a change to deliver that are not held.
	pub fn busy_subs(&self) -> Vec<(Node, Node, Cast, u64)> {
		(self.subs.keys())
			.filter(|(from, to, ..)| !self.held.contains(&(*from, *to)))
			.cloned()
			.collect()
	}

	/// Deliver the next change on the oldest stream of `cast`'s subscriptions from
	/// `from` to `to`.
	pub fn deliver_sub(&mut self, from: Node, to: Node, cast: &Cast) -> bool {
		let stream = (self.subs.keys()).find(|key| (key.0, key.1, &key.2) == (from, to, cast));
		match stream.map(|key| key.3) {
			Some(stream) => self.deliver_stream(from, to, cast, stream),
			None => false,
		}
	}

	/// Deliver the next change on one subscription stream from `from` to `to`.
	pub fn deliver_stream(&mut self, from: Node, to: Node, cast: &Cast, stream: u64) -> bool {
		let key = (from, to, cast.clone(), stream);
		let Some(pull) = self.subs.get_mut(&key).and_then(VecDeque::pop_front) else {
			return false;
		};
		if self.subs[&key].is_empty() {
			self.subs.remove(&key);
		}
		self.nodes[to].subscribed(from, cast.clone(), stream, pull);
		self.stats.delivered += 1;
		self.flush();
		true
	}

	/// One hop of delay passes at every running node. Whether any still waits on a timer.
	pub fn tick(&mut self) -> bool {
		let mut waiting = false;
		for (node, relay) in self.nodes.iter_mut().enumerate() {
			if !self.stopped.contains(&node) {
				waiting |= relay.tick();
			}
		}
		self.flush();
		waiting
	}

	pub fn queued(&self, from: Node, to: Node) -> usize {
		self.queues.get(&(from, to)).map_or(0, VecDeque::len)
	}

	/// The queues with a message to deliver that are not held.
	pub fn busy(&self) -> Vec<(Node, Node)> {
		(self.queues.iter())
			.filter(|(key, queue)| !queue.is_empty() && !self.held.contains(key))
			.map(|(key, _)| *key)
			.collect()
	}

	pub fn deliver(&mut self, from: Node, to: Node) -> bool {
		let Some(msg) = self.queues.get_mut(&(from, to)).and_then(VecDeque::pop_front) else {
			return false;
		};
		self.nodes[to].recv(from, msg);
		self.stats.delivered += 1;
		self.flush();
		true
	}

	/// Deliver round-robin until every unheld queue, subscriptions included, is
	/// empty, or give up.
	pub fn run(&mut self) -> bool {
		for _ in 0..10_000 {
			let (ready, subs) = (self.busy(), self.busy_subs());
			if ready.is_empty() && subs.is_empty() {
				return true;
			}
			for (from, to) in ready {
				self.deliver(from, to);
			}
			for (from, to, cast, stream) in subs {
				self.deliver_stream(from, to, &cast, stream);
			}
		}
		false
	}

	fn flush(&mut self) {
		for node in 0..self.nodes.len() {
			for (to, msg) in self.nodes[node].drain() {
				match &msg {
					Msg::Offer { .. } | Msg::Hold { .. } | Msg::End { .. } | Msg::Epoch { .. } => {
						self.stats.route_msgs += 1;
						self.stats.route_items += 1;
					}
					Msg::Warm { cast, .. } | Msg::Cold { cast } => {
						self.stats.route_msgs += 1;
						self.stats.route_items += 1;
						self.stats.warm_msgs += 1;
						*self.warm.entry(cast.clone()).or_default() += 1;
					}
					Msg::Dead { moved, .. } => {
						self.stats.route_msgs += 1;
						self.stats.route_items += 1 + moved.len();
					}
					Msg::Claims { changes, .. } => {
						self.stats.claim_msgs += 1;
						self.stats.claim_items += changes.len();
					}
				}
				self.queues.entry((node, to)).or_default().push_back(msg);
			}
			for (to, cast, stream, pull) in self.nodes[node].drain_subs() {
				self.stats.subscribes += 1;
				self.subs.entry((node, to, cast, stream)).or_default().push_back(pull);
			}
			for (id, claim, kind) in self.nodes[node].events() {
				self.events.push((node, id, claim.as_str().to_string(), kind));
			}
		}
	}

	pub fn next(&self, node: Node, id: Id) -> Option<Option<Node>> {
		self.nodes[node].next(id)
	}

	pub fn shows(&self, node: Node, id: Id, claim: &str) -> Option<bool> {
		self.nodes[node].shows(id, &p(claim))
	}
}

/// A relay on a ring loses its link toward the publisher. The only other way runs
/// through the neighbour that routed through it, so that neighbour moves first.
/// With no holds the relay lets its claims go and gets them back; with holds they
/// turn unavailable and back, and no claim is sent again.
#[test]
fn a_ring_takes_the_other_way_round() {
	for rules in [Rules::BARE, Rules::CAUSES, Rules::HOLDS, Rules::EPOCHS] {
		let origin = 4;
		let mut sim = PathSim::new(5, rules);
		for (a, b) in [(0, 1), (1, 2), (2, 3), (3, 0)] {
			sim.join(a, b, 1);
		}
		sim.join(origin, 0, 1);
		sim.originate(origin, 7);
		sim.announce(origin, "a");
		assert!(sim.run());
		assert_eq!(sim.next(1, 7), Some(Some(0)));
		assert_eq!(sim.next(2, 7), Some(Some(1)));

		let (events, claims) = (sim.events.len(), sim.stats.claim_items);
		sim.disconnect(0, 1);
		assert!(sim.run());
		assert_eq!(sim.next(1, 7), Some(Some(2)), "{rules:?}");
		assert_eq!(sim.nodes[1].cost(7), Some(4));
		assert_eq!(sim.shows(1, 7, "a"), Some(true));
		let seen: Vec<Kind> = (sim.events[events..].iter())
			.filter(|(node, ..)| *node == 1)
			.map(|(.., kind)| *kind)
			.collect();
		match rules.holds {
			true => {
				assert_eq!(seen, [Kind::Unavailable, Kind::Available], "{rules:?}");
				assert_eq!(sim.stats.claim_items, claims, "{rules:?}: no claim is sent again");
			}
			false => assert_eq!(seen, [Kind::Offline, Kind::Start], "{rules:?}"),
		}

		sim.disconnect(origin, 0);
		assert!(sim.run());
		for node in 0..4 {
			assert_eq!(sim.next(node, 7), None, "{rules:?}: node {node}");
			assert_eq!(sim.shows(node, 7, "a"), None, "{rules:?}: node {node}");
			assert_eq!(sim.nodes[node].retained(), (0, 0), "{rules:?}: node {node}");
		}
	}
}

impl Relay {
	pub fn dump(&self, id: Id) -> String {
		let mut out = format!(
			"hop {} route {:?} dead {:?} epochs {:?} doubt {:?}",
			self.hop,
			self.routes.get(&id).map(|r| (r.next, r.path.is_some())),
			self.dead,
			self.epochs,
			self.doubt
		);
		for (peer, session) in &self.sessions {
			out += &format!(
				"\n    {peer}: rx {:?} tx {:?} told {:?}",
				session
					.rx
					.get(&id)
					.map(|h| (h.state, h.path.iter().map(|x| (x.node, x.session)).collect::<Vec<_>>())),
				session
					.tx
					.get(&id)
					.map(|s| (s.held, s.path.iter().map(|x| (x.node, x.session)).collect::<Vec<_>>())),
				session.told
			);
		}
		out
	}
}
