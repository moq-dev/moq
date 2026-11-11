//! A prototype router that keeps a route free of loops by its path alone.
//!
//! It exists to be measured against [`Router`], which keeps Babel's distance: the
//! comparison in `compare.rs` runs one script on both. Nothing outside the tests
//! uses it.
//!
//! A route is one per publisher and names every relay it crossed, each with the
//! session that relay learned the route on. A relay uses the cheapest route whose
//! path does not name it, at once, with no distance, seqno, request, or echo. When
//! a session closes, the relay that learned routes on it names that hop in a
//! [`Msg::Dead`], and every relay drops every route through the hop, whichever
//! neighbour offered it. [`Rules`] switches each part off to show what it buys.
//!
//! Left out, since the comparison does not need them: scoped grants, qualified
//! keys, interests, GOAWAY, priorities, and RTT.

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
}

impl Rules {
	/// Paths and nothing else: what lite-06 does per broadcast, here per publisher.
	pub const BARE: Self = Self {
		causes: false,
		batch: false,
		horizon: false,
		holds: false,
		epochs: false,
	};

	/// The quest's candidate: a withdrawal names the hop that died.
	pub const CAUSES: Self = Self { causes: true, ..Self::BARE };

	/// Causes, one message per cause and session, and holds.
	pub const HOLDS: Self = Self {
		batch: true,
		holds: true,
		..Self::CAUSES
	};

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
	cost: Metric,
	rx: BTreeMap<Id, Heard>,
	tx: BTreeMap<Id, Sent>,
	/// The dead hops this neighbour was told of, while a route sent to it names one.
	told: BTreeSet<Hop>,
	/// The epoch this neighbour was told each node is at.
	epochs: BTreeMap<Id, u64>,
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

	pub fn connect(&mut self, peer: Node, hop: Id, cost: Metric) {
		self.opened += 1;
		let session = Session {
			id: self.opened,
			hop,
			cost,
			rx: BTreeMap::new(),
			tx: BTreeMap::new(),
			told: BTreeSet::new(),
			epochs: BTreeMap::new(),
		};
		self.sessions.insert(peer, session);
		let ids: Vec<Id> = self.routes.keys().copied().collect();
		for id in ids {
			self.advertise(id);
		}
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
		if self.rules.epochs {
			// This relay is still here, whatever its neighbour concludes.
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
					} else if !child(&heard.path, me) {
						heard.state = State::Held;
					}
					// A neighbour that holds under this relay answers once those behind it have.
				} else if heard.state == State::Live {
					heard.state = State::Dead;
				}
			}
		}
		self.dead.insert(hop);
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
							held.push(id);
						}
						rest += 1;
					}
					Want::Keep => rest += 1,
					Want::End => {
						session.tx.remove(&id);
						ended.push(id);
					}
				}
			}
			let gone = rest == 0;
			for (_, path, _) in &moved {
				self.tell(peer, path);
			}
			if first || (gone && !ended.is_empty()) {
				self.out.push((peer, Msg::Dead { hop, far, moved, gone }));
				if !gone {
					self.out.extend(ended.into_iter().map(|id| (peer, Msg::End { id })));
				}
			} else {
				self.out
					.extend(moved.into_iter().map(|(id, path, cost)| (peer, Msg::Offer { id, path, cost })));
				self.out.extend(held.into_iter().map(|id| (peer, Msg::Hold { id })));
				self.out.extend(ended.into_iter().map(|id| (peer, Msg::End { id })));
			}
		}
	}

	fn update(&mut self, id: Id) {
		self.select(id);
		self.show(id);
		self.advertise(id);
		self.collect();
	}

	/// Forget each dead hop no neighbour's route names any more.
	fn collect(&mut self) {
		let sessions = &self.sessions;
		self.dead.retain(|hop| {
			(sessions.values()).any(|session| session.rx.values().any(|heard| heard.path.contains(hop)))
		});
		let me = self.hop;
		let named = |node: &Id| {
			*node == me || (sessions.values()).any(|session| session.rx.values().any(|heard| names(&heard.path, *node)))
		};
		self.epochs.retain(|node, _| named(node));
		self.doubt.retain(|node, _| named(node));
		for session in self.sessions.values_mut() {
			let tx = &session.tx;
			session.told.retain(|hop| tx.values().any(|sent| sent.path.contains(hop)));
			session.epochs.retain(|node, _| tx.values().any(|sent| names(&sent.path, *node)));
		}
	}

	/// Every publisher a neighbour offered a route to.
	fn heard(&self) -> BTreeSet<Id> {
		(self.sessions.values()).flat_map(|session| session.rx.keys().copied()).collect()
	}

	/// Whether `path` crosses a node that a closed session cast doubt on, and that
	/// has not advanced its epoch since.
	fn doubted(&self, path: &[Hop]) -> bool {
		path.iter().any(|hop| {
			(self.doubt.get(&hop.node)).is_some_and(|at| self.epochs.get(&hop.node).copied().unwrap_or(0) <= *at)
		})
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
			let named = session.tx.values().any(|sent| names(&sent.path, node));
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
			if self.doubted(&heard.path) {
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
			None if route.next == Some(peer) => match self.pending(id) {
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
						self.out.push((peer, Msg::End { id }));
					}
					continue;
				}
			};
			let sent = session.tx.get(&id);
			let offer = sent.is_none_or(|sent| sent.held || sent.path != path || sent.cost != cost);
			let (was, at) = sent.map_or((Claims::new(), 0), |sent| (sent.claims.clone(), sent.rev));
			if offer {
				self.tell(peer, &path);
				let path = path.clone();
				self.out.push((peer, Msg::Offer { id, path, cost }));
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

	/// Whether this relay holds `id` with no route to it.
	pub fn held(&self, id: Id) -> bool {
		self.routes.get(&id).is_some_and(|route| route.path.is_none())
	}

	/// Whether this relay shows `claim` from `id`, and whether it is available.
	pub fn shows(&self, id: Id, claim: &Path) -> Option<bool> {
		self.shown.get(&(id, claim.clone())).map(|(_, available)| *available)
	}

	/// Dead hops remembered, and routes heard.
	pub fn retained(&self) -> (usize, usize) {
		let heard = self.sessions.values().map(|session| session.rx.len()).sum();
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
	held: BTreeSet<(Node, Node)>,
	pub stats: PathStats,
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
			held: BTreeSet::new(),
			stats: PathStats::default(),
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
			self.nodes[x].connect(y, peer, cost);
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
			self.held.remove(&(x, y));
			self.nodes[y].disconnect(x);
		}
		self.flush();
	}

	fn peers(&self, node: Node) -> Vec<Node> {
		(0..self.nodes.len())
			.filter(|&peer| self.links[node][peer].is_some() && self.connected(node, peer))
			.collect()
	}

	/// The process at `node` exits: its sessions close and its state is lost.
	pub fn kill(&mut self, node: Node) {
		for peer in self.peers(node) {
			self.disconnect(node, peer);
		}
		self.restarts[node] += 1;
		let leaf = self.nodes[node].leaf;
		self.nodes[node] = Relay::new(hop(node, self.restarts[node]), self.rules);
		self.nodes[node].leaf = leaf;
	}

	/// Re-open every configured link of `node`.
	pub fn reconnect(&mut self, node: Node) {
		let peers: Vec<Node> = (0..self.nodes.len())
			.filter(|&peer| self.links[node][peer].is_some() && !self.connected(node, peer))
			.collect();
		for peer in peers {
			self.connect(node, peer);
		}
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

	pub fn hold(&mut self, from: Node, to: Node) {
		self.held.insert((from, to));
	}

	pub fn release(&mut self, from: Node, to: Node) {
		self.held.remove(&(from, to));
	}

	pub fn front(&self, from: Node, to: Node) -> String {
		format!("{:?}", self.queues.get(&(from, to)).and_then(|queue| queue.front()))
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

	/// Deliver round-robin until every unheld queue is empty, or give up.
	pub fn run(&mut self) -> bool {
		for _ in 0..10_000 {
			let ready = self.busy();
			if ready.is_empty() {
				return true;
			}
			for (from, to) in ready {
				self.deliver(from, to);
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

/// A chain delivers a publisher's route and claims, a closed link ends them, and
/// the relay behind it takes the other way round at once.
#[test]
fn a_ring_takes_the_other_way_round_at_once() {
	for rules in [Rules::BARE, Rules::CAUSES] {
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
		assert_eq!(sim.shows(2, 7, "a"), Some(true));

		sim.disconnect(0, 1);
		assert_eq!(sim.next(1, 7), Some(Some(2)), "relay 2 had offered its own way");
		assert!(sim.run());
		assert_eq!(sim.nodes[1].cost(7), Some(4));
		assert_eq!(sim.shows(1, 7, "a"), Some(true));

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
		let mut out = format!("route {:?} dead {:?}", self.routes.get(&id).map(|r| (r.next, r.path.is_some())), self.dead);
		for (peer, session) in &self.sessions {
			out += &format!(
				"\n    {peer}: rx {:?} tx {:?} told {:?}",
				session.rx.get(&id).map(|h| (h.state, h.path.iter().map(|x| (x.node, x.session)).collect::<Vec<_>>())),
				session.tx.get(&id).map(|s| (s.held, s.path.iter().map(|x| (x.node, x.session)).collect::<Vec<_>>())),
				session.told
			);
		}
		out
	}
}

#[test]
fn debug_ring_home() {
	use super::compare::*;
	let mesh = Mesh::ring(6);
	let mut lab = Lab::new(Rule::Holds, &mesh);
	lab.trace = true;
	let cost = lab.measure(&|lab| lab.crash(0));
	eprintln!("{cost:?} {:?}", lab.check());
	for relay in 0..6 {
		eprintln!("relay {relay}: {}", lab.net.dump(relay, 100));
	}
}
