//! The route rule the routes epic's Plan item 27 picked, as one node's state
//! machine: a route per publisher and prefix, believed by its own path.
//!
//! A hop names a relay and the session it learned a route on. A node uses the
//! cheapest route whose path does not name it, at once. When a session closes,
//! the node that lost it names that hop in one [`Dead`] per neighbour, which
//! lists the routes that moved to another path, and every node drops every route
//! through the hop, whoever offered it. A node with no route left holds its
//! claims, unavailable, until each neighbour that learned the route from it has
//! answered. A closed session casts doubt on the node at its far end, since a dead
//! relay is reported one neighbour at a time: no node moves to a route through it
//! until a timer lifts the doubt. There is no distance, seqno, route request, or
//! echo.
//!
//! A route covers a prefix: the one its receiver asked for, or a narrower one a
//! node on its path asked for. A stream answering a prefix carries the sender's
//! routes inside it under their own prefixes, and one for the prefix itself from
//! the narrowest of the sender's routes covering it. A node keeps one view of a
//! publisher's claims per prefix: the newest floor any believed route covering
//! that prefix gave it, kept whole while it holds a route covering the prefix. It
//! shows each claim from the narrowest view above it, available while a live
//! route covers it.
//!
//! `rs/moq-net/verify/route/RouteHandoff.tla` model-checks the rule. Nothing drives
//! this router on the wire yet: sessions run [`super::Router`] until the switch
//! (`quest/m2/cluster-routes/switch.md`).

#![cfg_attr(not(test), allow(dead_code, reason = "only the fixtures drive it until the switch"))]

use std::{
	cmp::Reverse,
	collections::{BTreeMap, BTreeSet},
	sync::Arc,
	time::Duration,
};

use kio::clock::Instant;

use crate::Path;

use super::reader::Reader;
use super::view::{ancestors, below, covering};
use super::{Error, Event, GENERATED, Id, Key, Link, MAX, MAX_HOPS, Metric, QUALIFIED, Rev, Shown, View, within};

/// One node on a route's path, and the session it learned the route on. A
/// publisher names itself with session 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct Hop {
	pub node: Id,
	pub session: u64,
}

/// A route's identity: its publisher, and the prefix it covers.
pub(crate) type RouteId = (Id, Path);

/// What a route says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Offer {
	/// Every node it crossed, the publisher first and the sender last.
	pub path: Arc<[Hop]>,
	/// The cost that ranks it: the sum of configured link costs.
	pub metric: Metric,
	/// An upstream link is on its path, so it is never sent to one.
	pub down_only: bool,
	/// The round trip to the publisher: informational, never read to rank a route.
	pub rtt: Option<Duration>,
}

/// A route's state on a stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Reach {
	Finite(Offer),
	/// The sender has no route left but keeps the claims while one may return.
	Held,
}

/// A route that moved to another path when the session a [`Dead`] names closed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Moved {
	pub id: Id,
	pub prefix: Path,
	pub offer: Offer,
}

/// The session `hop` names closed, so every route through it is dead, whoever
/// offered it. Of the sender's own routes through it, each one `moved` lists
/// continues on its new path; the rest are ended if `gone`, and held if not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Dead {
	pub hop: Hop,
	/// The node at the other end of that session, which the news casts doubt on.
	pub far: Id,
	pub moved: Vec<Moved>,
	pub gone: bool,
}

/// A message for one neighbour.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Out<N, A> {
	/// ROUTE_START or ROUTE_UPDATE, ROUTE_HOLD for a held reach, or ROUTE_END for `None`.
	Route {
		to: N,
		id: Id,
		prefix: Path,
		reach: Option<Reach>,
	},
	/// Changes to a route's claims, as presented at its prefix, closed by the floor
	/// they are complete at.
	Batch {
		to: N,
		id: Id,
		prefix: Path,
		changes: Vec<(Path, Option<A>)>,
		floor: Option<Rev>,
	},
	Dead {
		to: N,
		dead: Dead,
	},
	/// The stream answering the neighbour's interest sent its initial set.
	Synced {
		to: N,
	},
}

/// When a doubt about a node lifts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Lift {
	/// When its timer fires, or sooner once no route names the node: the rule the
	/// model checks.
	Unnamed,
	/// Only when its timer fires.
	Timer,
}

/// How a closed session's doubt works.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Doubt {
	/// How long the doubt lasts.
	pub timer: Duration,
	pub lift: Lift,
}

impl Doubt {
	/// A second, lifted early once no route names the node.
	pub const DEFAULT: Self = Self {
		timer: Duration::from_secs(1),
		lift: Lift::Unnamed,
	};
}

impl Default for Doubt {
	fn default() -> Self {
		Self::DEFAULT
	}
}

/// Where a request for a claim goes from a node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Forward<N> {
	/// The node publishes it.
	Here,
	/// To `next`, naming the route as `next` offered it.
	Next { next: N, named: Id, prefix: Path },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
	Live,
	/// Its path holds a hop this node knows is dead, and the neighbour has not said
	/// what became of it yet.
	Dead,
	/// The neighbour lost the route and holds its claims.
	Held,
}

/// A route a neighbour offered.
#[derive(Clone, Debug)]
struct Heard<A> {
	offer: Offer,
	state: State,
	/// The interest generation of the stream it came on.
	interest: u32,
	/// Its claims as presented at its prefix, with one floor or none yet.
	view: View<A>,
}

/// What this node last sent a neighbour for a route.
#[derive(Clone, Debug)]
struct Sent<A> {
	/// The last offer, or `None` once the route ended there: then kept only for
	/// `src`, while the neighbour may still answer it.
	offer: Option<Offer>,
	held: bool,
	/// The prefix of this node's own route the offer reflects.
	src: Path,
	/// The prefix of the view its claims were presented from.
	from: Path,
	view: View<A>,
}

/// The route a node uses for one [`RouteId`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Route<N> {
	/// The neighbour it goes through, or the one it holds under: `None` at the
	/// publisher, and while held on this node's own account.
	pub next: Option<N>,
	/// What this node offers: `None` while held.
	pub offer: Option<Offer>,
}

impl<N> Route<N> {
	fn live(&self) -> bool {
		self.offer.is_some()
	}
}

struct Session<A> {
	/// How this node names the session in the hops of the routes it learns there.
	number: u64,
	/// How the neighbour names itself in a path.
	hop: Id,
	link: Link,
	rtt: Option<Duration>,
	draining: bool,
	/// The prefix the neighbour asked this node for, once it opened a stream.
	watch: Option<Path>,
	/// The routes it offered, so closing it costs what it held.
	heard: BTreeSet<RouteId>,
	tx: BTreeMap<RouteId, Sent<A>>,
	/// The dead hops it was told of, while a route sent to it names one.
	told: BTreeSet<Hop>,
}

struct Origin<A> {
	view: View<A>,
}

/// What a node's reader view of one publisher depends on besides its claims:
/// while it stays the same, only the claims marked dirty can change.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Ctx {
	/// Each view's prefix, and whether it has a floor yet.
	views: Vec<(Path, bool)>,
	/// Each route's prefix, and whether it is live.
	routes: Vec<(Path, bool)>,
}

#[derive(Default)]
struct Dirty {
	all: bool,
	claims: BTreeSet<Path>,
}

/// The batch an update follows, so a view tracking its route moves by the same changes.
struct Delta<N> {
	route: RouteId,
	from: N,
	/// The route's floor before the batch.
	was: Option<Rev>,
	claims: Vec<Path>,
}

/// The changes that bring what a neighbour was sent for a route to what a node
/// presents there now, with the prefix of the view they come from and its floor.
type Presented<A> = (Path, Vec<(Path, Option<A>)>, Option<Rev>);

/// What a node sends one neighbour for one route.
enum Want {
	Path(Offer, Path),
	Hold,
	End,
	/// No claim meets the stream any more: what was sent there is withdrawn, at the
	/// floor that says so, and then the route ends. Another route the neighbour holds
	/// for the prefix may lag, and must not bring the withdrawn claims back.
	Empty,
	/// Whatever it sent last: the neighbour is the next hop this node holds under,
	/// and waits for this node's answer.
	Keep,
}

/// Retained state, summed over the node, for budgets and leak checks.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Retained {
	/// Claims in views of other publishers.
	pub items: usize,
	pub views: usize,
	/// Claims on routes heard.
	pub heard: usize,
	/// Claims on routes sent.
	pub sent: usize,
	pub routes: usize,
	pub dead: usize,
	pub doubt: usize,
}

pub(crate) struct Router<N, A> {
	hop: Id,
	doubt: Doubt,
	opened: u64,
	sessions: BTreeMap<N, Session<A>>,
	origins: BTreeMap<Id, Origin<A>>,
	rx: BTreeMap<RouteId, BTreeMap<N, Heard<A>>>,
	/// The heard routes whose path holds each hop.
	rx_hops: BTreeMap<Hop, BTreeSet<(RouteId, N)>>,
	/// The sent routes whose path holds each hop.
	tx_hops: BTreeMap<Hop, BTreeSet<(N, RouteId)>>,
	routes: BTreeMap<RouteId, Route<N>>,
	views: BTreeMap<RouteId, View<A>>,
	/// For a key this node qualified, the identity the neighbour named.
	filed: BTreeMap<(Id, N), Id>,
	/// Dead hops, each with the node at the far end of its session.
	dead: BTreeMap<Hop, Id>,
	/// The nodes in doubt.
	doubted: BTreeSet<Id>,
	/// When each doubt's timer fires.
	timers: BTreeMap<Id, Instant>,
	watch: Path,
	interest: u32,
	reader: Reader<A>,
	ctxs: BTreeMap<Id, Ctx>,
	rng: u64,
	#[cfg(test)]
	touched: usize,
	out: Vec<Out<N, A>>,
}

/// Whether `path` names `node`.
fn names(path: &[Hop], node: Id) -> bool {
	path.iter().any(|hop| hop.node == node)
}

/// Whether the node that offered `path` learned it from `node`.
fn child(path: &[Hop], node: Id) -> bool {
	path.len() >= 2 && path[path.len() - 2].node == node
}

/// The narrower of two nested prefixes, or `None` when they are disjoint.
fn meet(a: &Path, b: &Path) -> Option<Path> {
	match (within(a, b), within(b, a)) {
		(true, _) => Some(a.clone()),
		(_, true) => Some(b.clone()),
		_ => None,
	}
}

/// The entries of `map` under identity `id`.
fn of<V>(map: &BTreeMap<RouteId, V>, id: Id) -> impl Iterator<Item = (&RouteId, &V)> {
	map.range((id, Path::empty())..).take_while(move |((key, _), _)| *key == id)
}

impl<N: Key, A: Clone + Eq + std::fmt::Debug> Router<N, A> {
	/// A node with no sessions, naming itself `hop` in the routes it forwards and
	/// minting rotated identities from `seed`.
	pub fn new(hop: Id, seed: u64, doubt: Doubt) -> Self {
		Self {
			hop,
			doubt,
			opened: 0,
			sessions: BTreeMap::new(),
			origins: BTreeMap::new(),
			rx: BTreeMap::new(),
			rx_hops: BTreeMap::new(),
			tx_hops: BTreeMap::new(),
			routes: BTreeMap::new(),
			views: BTreeMap::new(),
			filed: BTreeMap::new(),
			dead: BTreeMap::new(),
			doubted: BTreeSet::new(),
			timers: BTreeMap::new(),
			watch: Path::empty(),
			interest: 0,
			reader: Reader::default(),
			ctxs: BTreeMap::new(),
			rng: seed | 1,
			#[cfg(test)]
			touched: 0,
			out: Vec::new(),
		}
	}

	/// Take the messages queued for neighbours, in order.
	pub fn drain(&mut self) -> Vec<Out<N, A>> {
		std::mem::take(&mut self.out)
	}

	/// Take the reader events, in order.
	pub fn events(&mut self) -> Vec<Event<A>> {
		self.reader.events()
	}

	/// Take the identities recomputed since the last call, each with the claims whose
	/// shown state changed.
	pub fn changed(&mut self) -> BTreeMap<Id, BTreeSet<Path>> {
		self.reader.changed()
	}

	// --- Sessions ---

	/// A session to `neighbor`, which names itself `hop` in a path, opened.
	pub fn connect(&mut self, neighbor: N, hop: Id, link: Link) -> Result<(), Error> {
		if link.cost == 0 {
			return Err(Error::ZeroCost);
		}
		self.opened += 1;
		let session = Session {
			number: self.opened,
			hop,
			link,
			rtt: None,
			draining: false,
			watch: None,
			heard: BTreeSet::new(),
			tx: BTreeMap::new(),
			told: BTreeSet::new(),
		};
		self.sessions.insert(neighbor, session);
		// Nothing is sent until the neighbour asks.
		Ok(())
	}

	/// The sessions to `neighbors` closed at `now`: everything learned or sent on them
	/// goes, and each names its hop to the others.
	pub fn disconnect(&mut self, neighbors: impl IntoIterator<Item = N>, now: Instant) {
		let mut ids = BTreeSet::new();
		let mut hops = Vec::new();
		for neighbor in neighbors {
			let Some(session) = self.sessions.remove(&neighbor) else {
				continue;
			};
			for route in &session.heard {
				ids.insert(route.0);
				self.forget_heard(route, neighbor);
				self.filed.remove(&(route.0, neighbor));
			}
			for (route, sent) in &session.tx {
				ids.insert(route.0);
				if let Some(offer) = &sent.offer {
					self.unindex_sent(neighbor, route, &offer.path);
				}
			}
			let hop = Hop {
				node: self.hop,
				session: session.number,
			};
			ids.extend(self.fail(hop, session.hop, None, Vec::new(), false, now));
			hops.push(hop);
		}
		self.update(ids, hops, None);
	}

	/// `neighbor` asked this node for announcements under `prefix`, or for none: its
	/// stream starts over, a complete snapshot closed by SYNCED.
	pub fn peer_interest(&mut self, neighbor: N, prefix: Option<Path>) -> Result<(), Error> {
		let session = self.sessions.get_mut(&neighbor).ok_or(Error::UnknownNeighbor)?;
		session.watch = prefix;
		let tx = std::mem::take(&mut session.tx);
		let has = session.watch.is_some();
		for (route, sent) in &tx {
			if let Some(offer) = &sent.offer {
				self.unindex_sent(neighbor, route, &offer.path);
			}
		}
		let ids = self.ids();
		self.update(ids, Vec::new(), None);
		if has {
			self.out.push(Out::Synced { to: neighbor });
		}
		Ok(())
	}

	/// `neighbor` sent GOAWAY: it ranks below every other route while its session stays up.
	pub fn goaway(&mut self, neighbor: N) {
		if let Some(session) = self.sessions.get_mut(&neighbor) {
			session.draining = true;
		}
		let ids = self.through(neighbor);
		self.update(ids, Vec::new(), None);
	}

	/// The link to `neighbor` has `rtt`, or `None` unmeasured.
	pub fn rtt(&mut self, neighbor: N, rtt: Option<Duration>) {
		let Some(session) = self.sessions.get_mut(&neighbor) else {
			return;
		};
		if session.rtt == rtt {
			return;
		}
		session.rtt = rtt;
		let ids = self.through(neighbor);
		self.update(ids, Vec::new(), None);
	}

	/// The identities `neighbor` offered a route for.
	fn through(&self, neighbor: N) -> BTreeSet<Id> {
		(self.sessions.get(&neighbor).into_iter())
			.flat_map(|session| session.heard.iter().map(|(id, _)| *id))
			.collect()
	}

	/// Ask every neighbour for announcements under `prefix` instead, returning the new
	/// interest generation. Each answers on a new stream; until that one is SYNCED the
	/// old one's routes stay, but only the new one's claims are believed. What this
	/// node knows outside `prefix` is forgotten.
	pub fn watch(&mut self, prefix: Path) -> u32 {
		self.interest += 1;
		self.watch = prefix.clone();
		let keys: Vec<RouteId> = self.views.keys().cloned().collect();
		for key in keys {
			if within(&key.1, &prefix) {
				continue;
			}
			let view = self.views.remove(&key).expect("a view");
			self.forget(key.0, &key.1, Some(&prefix));
			if within(&prefix, &key.1) {
				let narrow = (key.0, prefix.clone());
				self.views.entry(narrow).or_insert_with(|| view.present(&prefix));
			}
		}
		let ids = self.ids();
		for &id in &ids {
			self.ctxs.remove(&id);
		}
		self.update(ids, Vec::new(), None);
		self.interest
	}

	/// The current interest generation.
	pub fn interest(&self) -> u32 {
		self.interest
	}

	// --- The origin's own identity ---

	/// Start originating `id`, with no claims.
	pub fn originate(&mut self, id: Id) {
		debug_assert_eq!(id & QUALIFIED, 0, "a declared identity is never qualified");
		let mut view = View::default();
		view.floors.insert(Path::empty(), 0);
		self.origins.insert(id, Origin { view });
		let path: Arc<[Hop]> = Arc::new([Hop {
			node: self.hop,
			session: 0,
		}]);
		let route = Route {
			next: None,
			offer: Some(Offer {
				path,
				metric: 0,
				down_only: false,
				rtt: Some(Duration::ZERO),
			}),
		};
		self.routes.insert((id, Path::empty()), route);
		self.ctxs.remove(&id);
		self.update([id].into(), Vec::new(), None);
	}

	/// The identity this node declares.
	pub fn declared(&self) -> Option<Id> {
		self.origins.keys().next().copied()
	}

	/// The declared identity's claim revision.
	pub fn revision(&self) -> Option<Rev> {
		let origin = self.origins.values().next()?;
		origin.view.floors.get(&Path::empty()).copied()
	}

	/// Start or update a claim of the declared identity.
	pub fn announce(&mut self, claim: Path, item: A) -> Result<(), Error> {
		self.change(claim, Some(item))
	}

	/// Withdraw a claim of the declared identity.
	pub fn unannounce(&mut self, claim: Path) -> Result<(), Error> {
		self.change(claim, None)
	}

	fn change(&mut self, claim: Path, item: Option<A>) -> Result<(), Error> {
		let id = self.declared().ok_or(Error::NoIdentity)?;
		let rev = self.origins[&id].view.floors[&Path::empty()] + 1;
		if rev == MAX {
			self.rotate(id);
			return self.change(claim, item);
		}
		let view = &mut self.origins.get_mut(&id).expect("declared").view;
		view.floors.insert(Path::empty(), rev);
		match item {
			Some(item) => view.items.insert(claim.clone(), item),
			None => view.items.remove(&claim),
		};
		let dirty = Dirty {
			all: false,
			claims: BTreeSet::from([claim]),
		};
		self.update([id].into(), Vec::new(), Some((id, dirty)));
		Ok(())
	}

	/// Jump the declared identity's revision to `rev`, as a long-lived origin's claim
	/// changes reach it.
	#[cfg(test)]
	pub fn age(&mut self, rev: Rev) {
		let id = self.declared().expect("declared");
		let view = &mut self.origins.get_mut(&id).expect("declared").view;
		view.floors.insert(Path::empty(), rev);
	}

	/// Retire the declared identity for a fresh one, announcing its claims again there.
	fn rotate(&mut self, old: Id) {
		let origin = self.origins.remove(&old).expect("declared");
		self.routes.remove(&(old, Path::empty()));
		let id = loop {
			// xorshift64: a fresh identity within a generated hop id's range.
			self.rng ^= self.rng << 13;
			self.rng ^= self.rng >> 7;
			self.rng ^= self.rng << 17;
			let id = self.rng & GENERATED;
			if id != 0 && id != old && !self.origins.contains_key(&id) {
				break id;
			}
		};
		let rev = origin.view.items.len() as Rev;
		let mut view = origin.view;
		view.floors = BTreeMap::from([(Path::empty(), rev)]);
		self.ctxs.remove(&old);
		self.originate(id);
		self.origins.get_mut(&id).expect("originated").view = view;
		self.ctxs.remove(&id);
		self.update([old, id].into(), Vec::new(), None);
	}

	// --- Received messages ---

	/// A route `from` sent for `named` under `prefix`, on the stream answering interest
	/// generation `interest`: ROUTE_START or ROUTE_UPDATE, ROUTE_HOLD, or ROUTE_END for `None`.
	pub fn route(&mut self, from: N, named: Id, prefix: Path, reach: Option<Reach>, interest: u32) {
		let Some(id) = self.admit(from, named, &prefix) else {
			return;
		};
		let key = (id, prefix);
		let entry = self.rx.get(&key).and_then(|routes| routes.get(&from));
		// A message from a stream older than the route it names.
		if entry.is_some_and(|entry| entry.interest > interest) {
			return;
		}
		let same = entry.is_some_and(|entry| entry.interest == interest);
		match reach {
			// A path a node could not extend is refused, as if ended.
			Some(Reach::Finite(offer)) if !offer.path.is_empty() && offer.path.len() < MAX_HOPS => {
				let view = match same {
					true => self.rx[&key][&from].view.clone(),
					false => View::default(),
				};
				if entry.is_some() {
					self.forget_heard(&key, from);
				}
				let state = match self.dead_in(&offer.path) {
					true => State::Dead,
					false => State::Live,
				};
				let heard = Heard {
					offer,
					state,
					interest,
					view,
				};
				self.hear(key.clone(), from, heard);
				if id != named {
					self.filed.insert((id, from), named);
				}
			}
			Some(Reach::Held) if same => {
				let heard = self.rx.get_mut(&key).and_then(|routes| routes.get_mut(&from));
				if let Some(heard) = heard {
					heard.state = State::Held;
				}
			}
			Some(Reach::Held) => return,
			Some(Reach::Finite(_)) | None => {
				if entry.is_none() || !same && reach.is_none() {
					return;
				}
				self.forget_heard(&key, from);
			}
		}
		self.update([id].into(), Vec::new(), None);
	}

	/// A batch of claim changes for the route `from` sent for `named` under `prefix`,
	/// closed by its floor, on the stream answering interest generation `interest`.
	pub fn batch(
		&mut self,
		from: N,
		named: Id,
		prefix: Path,
		changes: Vec<(Path, Option<A>)>,
		floor: Option<Rev>,
		interest: u32,
	) {
		let Some(id) = self.admit(from, named, &prefix) else {
			return;
		};
		let key = (id, prefix.clone());
		let Some(heard) = self.rx.get_mut(&key).and_then(|routes| routes.get_mut(&from)) else {
			return;
		};
		if heard.interest != interest {
			return;
		}
		let was = heard.view.floors.get(&prefix).copied();
		let claims: Vec<Path> = changes.iter().map(|(claim, _)| claim.clone()).collect();
		let floors = floor.map(|rev| BTreeMap::from([(prefix, rev)])).unwrap_or_default();
		heard.view.apply(changes, floors);
		let delta = Delta {
			route: key,
			from,
			was,
			claims,
		};
		self.recompute([id].into(), Vec::new(), None, Some(delta));
	}

	/// A [`Dead`] from `from`, at `now`.
	pub fn dead(&mut self, from: N, dead: Dead, now: Instant) {
		if !self.sessions.contains_key(&from) {
			return;
		}
		let Dead { hop, far, moved, gone } = dead;
		let mut filed = Vec::new();
		for moved in moved {
			if let Some(id) = self.admit(from, moved.id, &moved.prefix) {
				filed.push(((id, moved.prefix), moved.offer));
			}
		}
		let ids = self.fail(hop, far, Some(from), filed, gone, now);
		self.update(ids, vec![hop], None);
	}

	/// `from`'s stream answering interest generation `interest` sent its initial set:
	/// what an older stream left is gone.
	pub fn synced(&mut self, from: N, interest: u32) {
		let Some(session) = self.sessions.get(&from) else {
			return;
		};
		let stale: Vec<RouteId> = (session.heard.iter())
			.filter(|key| self.rx[*key][&from].interest < interest)
			.cloned()
			.collect();
		let ids: BTreeSet<Id> = stale.iter().map(|(id, _)| *id).collect();
		for key in &stale {
			self.forget_heard(key, from);
		}
		self.update(ids, Vec::new(), None);
	}

	/// Fire every doubt timer due by `now`.
	pub fn timeout(&mut self, now: Instant) {
		let due: Vec<Id> = (self.timers.iter())
			.filter(|(_, at)| **at <= now)
			.map(|(node, _)| *node)
			.collect();
		if due.is_empty() {
			return;
		}
		let mut ids = BTreeSet::new();
		for node in due {
			self.timers.remove(&node);
			self.doubted.remove(&node);
			ids.extend(self.naming(node));
		}
		self.update(ids, Vec::new(), None);
	}

	/// When the next doubt timer fires.
	pub fn deadline(&self) -> Option<Instant> {
		self.timers.values().min().copied()
	}

	/// Forget `id`, once nothing holds it.
	pub fn collect(&mut self, id: Id) {
		if self.holds(id) || self.origins.contains_key(&id) {
			return;
		}
		let views: Vec<RouteId> = of(&self.views, id).map(|(key, _)| key.clone()).collect();
		for key in views {
			self.views.remove(&key);
			self.forget(id, &key.1, None);
		}
		self.ctxs.remove(&id);
		self.update([id].into(), Vec::new(), None);
	}

	// --- Queries ---

	/// The route this node uses for `id` under `prefix`.
	pub fn route_for(&self, id: Id, prefix: &Path) -> Option<&Route<N>> {
		self.routes.get(&(id, prefix.clone()))
	}

	/// Everything this node holds, for a failure report.
	#[cfg(test)]
	pub fn dump(&self) -> String {
		let mut out = String::new();
		for (key, routes) in &self.rx {
			for (peer, heard) in routes {
				out += &format!(
					"  heard {key:?} from {peer:?}: {:?} {:?} gen {} view {:?}\n",
					heard.state, heard.offer.path, heard.interest, heard.view
				);
			}
		}
		for (key, route) in &self.routes {
			out += &format!("  route {key:?}: {route:?}\n");
		}
		for (key, view) in &self.views {
			out += &format!("  view {key:?}: {view:?}\n");
		}
		out += &format!("  interest {} watch {:?} shown {:?}", self.interest, self.watch, self.reader.shown());
		out
	}

	/// What `from` last offered for `id` under `prefix`, as this node files it.
	#[cfg(test)]
	pub fn heard_offer(&self, id: Id, prefix: &Path, from: N) -> Option<&Offer> {
		let heard = self.rx.get(&(id, prefix.clone()))?.get(&from)?;
		(heard.state == State::Live).then_some(&heard.offer)
	}

	/// Every route this node uses for `id`, by prefix.
	pub fn routes(&self, id: Id) -> impl Iterator<Item = (&Path, &Route<N>)> {
		of(&self.routes, id).map(|((_, prefix), route)| (prefix, route))
	}

	/// Where a request for `claim` of `id` goes: from this node's own reader when
	/// `from` is `None`, else from the neighbour that sent it, naming the route this
	/// node offered it under that prefix. `None` when it stops here.
	///
	/// A reader's request starts on the narrowest live route covering the claim. A
	/// forwarded one follows the route this node offered the neighbour it came from,
	/// and never one whose path names that neighbour.
	pub fn forward(&self, id: Id, claim: &Path, from: Option<(N, Path)>) -> Option<Forward<N>> {
		if self.origins.contains_key(&id) {
			return Some(Forward::Here);
		}
		let (prefix, prev) = match from {
			None => {
				let prefix = ancestors(claim)
					.rev()
					.find(|prefix| self.routes.get(&(id, prefix.clone())).is_some_and(Route::live))?;
				(prefix, None)
			}
			Some((prev, prefix)) => {
				let watch = self.sessions.get(&prev)?.watch.clone()?;
				(self.src(id, &prefix, &watch), Some(prev))
			}
		};
		let route = self.routes.get(&(id, prefix.clone()))?;
		let offer = route.offer.as_ref()?;
		if let Some(prev) = prev
			&& names(&offer.path, self.sessions[&prev].hop)
		{
			return None;
		}
		let next = route.next?;
		Some(Forward::Next {
			next,
			named: self.name(next, id),
			prefix,
		})
	}

	/// The identity this node names for `id` to `next`: the one `next` sent, if this
	/// node qualified it.
	pub fn name(&self, next: N, id: Id) -> Id {
		self.filed.get(&(id, next)).copied().unwrap_or(id)
	}

	/// Whether this node holds any route for `id`, heard or its own.
	pub fn holds(&self, id: Id) -> bool {
		of(&self.rx, id).next().is_some() || of(&self.routes, id).next().is_some()
	}

	/// What this node's reader sees.
	pub fn shown(&self) -> &BTreeMap<(Id, Path), Shown<A>> {
		self.reader.shown()
	}

	/// Whether this node shows `claim` from `id`, and whether it is available.
	#[cfg(test)]
	pub fn shows(&self, id: Id, claim: &Path) -> Option<bool> {
		self.reader.get(id, claim).map(|shown| shown.available)
	}

	/// The number this node names its session to `neighbor` with.
	#[cfg(test)]
	pub fn session(&self, neighbor: N) -> Option<u64> {
		self.sessions.get(&neighbor).map(|session| session.number)
	}

	/// Every view this node keeps of another publisher, by route.
	#[cfg(test)]
	pub fn views(&self) -> impl Iterator<Item = (&RouteId, &View<A>)> {
		self.views.iter()
	}

	/// Every view a route heard carries.
	#[cfg(test)]
	pub fn heard(&self) -> impl Iterator<Item = (&RouteId, &View<A>)> {
		self.rx.iter().flat_map(|(key, routes)| routes.values().map(move |heard| (key, &heard.view)))
	}

	/// The view of `id` this node keeps for `prefix`, or its own.
	pub fn view(&self, id: Id, prefix: &Path) -> Option<&View<A>> {
		match self.origins.get(&id) {
			Some(origin) => Some(&origin.view),
			None => self.views.get(&(id, prefix.clone())),
		}
	}

	/// How many identities this node has recomputed.
	#[cfg(test)]
	pub fn touched(&self) -> usize {
		self.touched
	}

	/// Whether a doubt or a dead hop is still remembered.
	#[cfg(test)]
	pub fn doubts(&self) -> bool {
		!self.doubted.is_empty() || !self.timers.is_empty() || !self.dead.is_empty()
	}

	#[cfg(test)]
	pub fn retained(&self) -> Retained {
		Retained {
			items: self.views.values().map(|view| view.items.len()).sum(),
			views: self.views.len(),
			heard: self.heard().map(|(_, view)| view.items.len()).sum(),
			sent: (self.sessions.values())
				.flat_map(|session| session.tx.values())
				.map(|sent| sent.view.items.len())
				.sum(),
			routes: self.routes.keys().filter(|(id, _)| !self.origins.contains_key(id)).count(),
			dead: self.dead.len(),
			doubt: self.doubted.len() + self.timers.len(),
		}
	}

	// --- Internals: what is heard ---

	/// The key this node files `named` under when `from` sends it a route covering
	/// `prefix`, or `None` when refused: a scoped neighbour's identities are all
	/// qualified, one already qualified is refused, and so is a route outside its scope.
	/// A node also ignores routes to its own identities.
	fn admit(&self, from: N, named: Id, prefix: &Path) -> Option<Id> {
		let session = self.sessions.get(&from)?;
		let id = match &session.link.scope {
			Some(scope) => {
				if named & QUALIFIED != 0 || !scope.regions.iter().any(|region| within(prefix, region)) {
					return None;
				}
				(scope.qualify)(named) | QUALIFIED
			}
			None => named,
		};
		(!self.origins.contains_key(&id)).then_some(id)
	}

	/// Whether `path` holds a hop this node knows is dead.
	fn dead_in(&self, path: &[Hop]) -> bool {
		path.iter().any(|hop| self.dead.contains_key(hop))
	}

	/// Whether `path` crosses a node in doubt.
	fn doubts_path(&self, path: &[Hop]) -> bool {
		path.iter().any(|hop| self.doubted.contains(&hop.node))
	}

	fn hear(&mut self, key: RouteId, from: N, heard: Heard<A>) {
		for hop in heard.offer.path.iter() {
			self.rx_hops.entry(*hop).or_default().insert((key.clone(), from));
		}
		self.sessions.get_mut(&from).expect("a session").heard.insert(key.clone());
		self.rx.entry(key).or_default().insert(from, heard);
	}

	fn forget_heard(&mut self, key: &RouteId, from: N) {
		let Some(routes) = self.rx.get_mut(key) else {
			return;
		};
		let Some(heard) = routes.remove(&from) else {
			return;
		};
		if routes.is_empty() {
			self.rx.remove(key);
		}
		for hop in heard.offer.path.iter() {
			if let Some(set) = self.rx_hops.get_mut(hop) {
				set.remove(&(key.clone(), from));
				if set.is_empty() {
					self.rx_hops.remove(hop);
				}
			}
		}
		if let Some(session) = self.sessions.get_mut(&from) {
			session.heard.remove(key);
			if !session.heard.iter().any(|(id, _)| *id == key.0) {
				self.filed.remove(&(key.0, from));
			}
		}
	}

	/// Re-path a heard route in place, keeping its claims.
	fn repath(&mut self, key: &RouteId, from: N, offer: Offer) {
		let mut heard = self.rx[key][&from].clone();
		self.forget_heard(key, from);
		heard.state = match self.dead_in(&offer.path) {
			true => State::Dead,
			false => State::Live,
		};
		heard.offer = offer;
		self.hear(key.clone(), from, heard);
	}

	/// The identities with a heard route naming `node`.
	fn naming(&self, node: Id) -> BTreeSet<Id> {
		let lo = Hop { node, session: 0 };
		let hi = Hop {
			node,
			session: u64::MAX,
		};
		(self.rx_hops.range(lo..=hi))
			.flat_map(|(_, set)| set.iter().map(|((id, _), _)| *id))
			.collect()
	}

	/// Whether any heard route names `node`.
	fn named(&self, node: Id) -> bool {
		let lo = Hop { node, session: 0 };
		let hi = Hop {
			node,
			session: u64::MAX,
		};
		self.rx_hops.range(lo..=hi).next().is_some()
	}

	/// The session `hop` names closed. `from` reported it, with what became of its own
	/// routes through it, or this node saw it close. It casts doubt on `far`. Returns
	/// the identities touched.
	fn fail(
		&mut self,
		hop: Hop,
		far: Id,
		from: Option<N>,
		moved: Vec<(RouteId, Offer)>,
		gone: bool,
		now: Instant,
	) -> BTreeSet<Id> {
		let mut ids = BTreeSet::new();
		let through: Vec<(RouteId, N)> = self.rx_hops.get(&hop).into_iter().flatten().cloned().collect();
		// The hop is dead before any route moves, so a moved path through it stays dead.
		self.dead.insert(hop, far);
		for (key, peer) in through {
			ids.insert(key.0);
			let reported = from == Some(peer);
			let moved = moved.iter().find(|(named, _)| *named == key).filter(|_| reported);
			if let Some((_, offer)) = moved {
				self.repath(&key, peer, offer.clone());
				continue;
			}
			let me = self.hop;
			if reported {
				if gone {
					self.forget_heard(&key, peer);
				} else {
					let heard = self.rx.get_mut(&key).and_then(|routes| routes.get_mut(&peer)).expect("heard");
					// A neighbour that holds under this node answers once those behind it have.
					if !child(&heard.offer.path, me) {
						heard.state = State::Held;
					}
				}
				continue;
			}
			let heard = self.rx.get_mut(&key).and_then(|routes| routes.get_mut(&peer)).expect("heard");
			if heard.state == State::Live {
				heard.state = State::Dead;
			}
		}
		if far != self.hop {
			self.doubted.insert(far);
			let at = now.saturating_add(self.doubt.timer);
			let timer = self.timers.entry(far).or_insert(at);
			*timer = (*timer).max(at);
			// Any route through that node may have become unusable.
			ids.extend(self.naming(far));
		}
		ids
	}

	// --- Internals: selection ---

	/// Every identity this node keeps any state for.
	fn ids(&self) -> BTreeSet<Id> {
		let mut ids: BTreeSet<Id> = self.rx.keys().map(|(id, _)| *id).collect();
		ids.extend(self.routes.keys().map(|(id, _)| *id));
		ids.extend(self.views.keys().map(|(id, _)| *id));
		ids.extend(self.reader.shown().keys().map(|(id, _)| *id));
		for session in self.sessions.values() {
			ids.extend(session.tx.keys().map(|(id, _)| *id));
		}
		ids
	}

	/// The prefixes `id` has a route heard or used under.
	fn prefixes(&self, id: Id) -> BTreeSet<Path> {
		let mut out: BTreeSet<Path> = of(&self.rx, id).map(|((_, prefix), _)| prefix.clone()).collect();
		out.extend(of(&self.routes, id).map(|((_, prefix), _)| prefix.clone()));
		out
	}

	/// Pick the cheapest usable route for `key`, staying on the next hop among equals.
	/// Doubt keeps this node from moving to a route, not on the one it uses. With none
	/// left, hold or let go.
	fn select(&self, key: &RouteId) -> Option<Route<N>> {
		let was = self.routes.get(key);
		let current = was.and_then(|route| route.next);
		let using = current.filter(|_| was.is_some_and(Route::live));
		type Rank<N> = (bool, Reverse<i8>, Metric, bool, N);
		let mut best: Option<(Rank<N>, Offer)> = None;
		let mut hope = false;
		for (&peer, heard) in self.rx.get(key).into_iter().flatten() {
			if heard.state != State::Live || names(&heard.offer.path, self.hop) {
				continue;
			}
			let session = &self.sessions[&peer];
			let Some(metric) = heard.offer.metric.checked_add(session.link.cost) else {
				continue;
			};
			if Some(peer) != using && self.doubts_path(&heard.offer.path) {
				// It may come back once the doubt lifts.
				hope = true;
				continue;
			}
			let rank = (
				session.draining,
				Reverse(session.link.priority),
				metric,
				Some(peer) != current,
				peer,
			);
			if best.as_ref().is_some_and(|(best, _)| *best <= rank) {
				continue;
			}
			let path: Arc<[Hop]> = (heard.offer.path.iter().copied())
				.chain([Hop {
					node: self.hop,
					session: session.number,
				}])
				.collect();
			let offer = Offer {
				path,
				metric,
				down_only: session.link.upstream || heard.offer.down_only,
				rtt: heard.offer.rtt.zip(session.rtt).and_then(|(a, b)| a.checked_add(b)),
			};
			best = Some((rank, offer));
		}
		if let Some(((.., peer), offer)) = best {
			return Some(Route {
				next: Some(peer),
				offer: Some(offer),
			});
		}
		// Only a node that had the route holds it.
		was?;
		// Its next hop holds, or has yet to say what became of a dead route. A next hop
		// whose own route names this node waits on this node instead.
		let under = current.filter(|next| {
			(self.rx.get(key).and_then(|routes| routes.get(next)))
				.is_some_and(|heard| heard.state != State::Live && !names(&heard.offer.path, self.hop))
		});
		if under.is_none() && !hope && !self.pending(key) {
			return None;
		}
		Some(Route {
			next: under,
			offer: None,
		})
	}

	/// Whether a neighbour that learned this node's route for `key` has yet to answer
	/// its loss.
	fn pending(&self, key: &RouteId) -> bool {
		of(&self.rx, key.0).any(|(heard_key, routes)| {
			routes.iter().any(|(peer, heard)| {
				heard.state != State::Held
					&& child(&heard.offer.path, self.hop)
					&& (self.sessions[peer].tx.get(heard_key)).is_some_and(|sent| sent.src == key.1)
			})
		})
	}

	/// Whether a neighbour offers a route for `key` that only doubt keeps this node off.
	fn hopes(&self, key: &RouteId) -> bool {
		(self.rx.get(key).into_iter().flatten()).any(|(_, heard)| {
			heard.state == State::Live
				&& !names(&heard.offer.path, self.hop)
				&& self.doubts_path(&heard.offer.path)
		})
	}

	/// The prefix of this node's own route that what it offers a stream answering
	/// `watch` under `prefix` reflects: the route itself inside the interest, and at the
	/// interest the narrowest live route covering it, else the narrowest held one.
	fn src(&self, id: Id, prefix: &Path, watch: &Path) -> Path {
		if prefix != watch {
			return prefix.clone();
		}
		let covering = || ancestors(prefix).rev().map(|above| (id, above));
		(covering().find(|key| self.routes.get(key).is_some_and(Route::live)))
			.or_else(|| covering().find(|key| self.routes.contains_key(key)))
			.map_or_else(|| prefix.clone(), |(_, prefix)| prefix)
	}

	// --- Internals: views ---

	/// Whether this node holds a route of `id` covering `prefix`.
	fn presence(&self, id: Id, prefix: &Path) -> bool {
		ancestors(prefix).any(|above| self.routes.contains_key(&(id, above)))
	}

	/// The narrowest view of `id` strictly above `prefix` that knows something: one a
	/// route has given a floor. A view with none yet stands for nothing.
	fn above(&self, id: Id, prefix: &Path) -> Option<(&Path, &View<A>)> {
		ancestors(prefix)
			.rev()
			.filter(|above| above != prefix)
			.find_map(|above| {
				(self.views.get_key_value(&(id, above))).filter(|((_, above), view)| view.floors.contains_key(above))
			})
			.map(|((_, prefix), view)| (prefix, view))
	}

	/// The narrowest view of `id` covering `prefix`, or the publisher's own.
	fn covering_view(&self, id: Id, prefix: &Path) -> Option<(Path, &View<A>)> {
		if let Some(origin) = self.origins.get(&id) {
			return Some((Path::empty(), &origin.view));
		}
		ancestors(prefix)
			.rev()
			.find_map(|above| self.views.get_key_value(&(id, above)))
			.map(|((_, prefix), view)| (prefix.clone(), view))
	}

	/// Whether a route this node heard is believed for its claims: live, its path does
	/// not name this node, and its stream answers the current interest.
	fn believed(&self, heard: &Heard<A>) -> bool {
		heard.state == State::Live && !names(&heard.offer.path, self.hop) && heard.interest == self.interest
	}

	/// Bring every view of `id` up to date: each prefix within the interest that a route
	/// covers keeps the newest floor a believed route covering it gave, kept whole while
	/// a route covers it. Returns whether the views changed shape, and marks changed
	/// claims in `dirty`.
	fn know(&mut self, id: Id, delta: Option<&Delta<N>>, dirty: &mut Dirty) {
		if self.origins.contains_key(&id) {
			return;
		}
		let watch = self.watch.clone();
		let was: Vec<(Path, Option<Rev>)> = (of(&self.views, id))
			.map(|((_, prefix), view)| (prefix.clone(), view.floors.get(prefix).copied()))
			.collect();
		let mut keys: BTreeSet<Path> = of(&self.views, id).map(|((_, prefix), _)| prefix.clone()).collect();
		keys.extend(
			(of(&self.routes, id).map(|((_, prefix), _)| prefix)).filter_map(|prefix| meet(prefix, &watch)),
		);
		for prefix in &keys {
			let key = (id, prefix.clone());
			if !self.presence(id, prefix) {
				if self.views.remove(&key).is_some() {
					self.forget(id, prefix, None);
					dirty.all = true;
				}
				continue;
			}
			let created = !self.views.contains_key(&key);
			let base = match self.views.get(&key) {
				Some(view) => view.clone(),
				None => {
					dirty.all = true;
					self.above(id, prefix).map(|(_, view)| view.present(prefix)).unwrap_or_default()
				}
			};
			let floor = base.floors.get(prefix).copied();
			// The newest floor any believed route covering this prefix gave.
			let mut best: Option<(Rev, RouteId, N)> = None;
			for above in ancestors(prefix) {
				let route = (id, above);
				for (&peer, heard) in self.rx.get(&route).into_iter().flatten() {
					let Some(&rev) = heard.view.floors.get(&route.1) else {
						continue;
					};
					if !self.believed(heard) || Some(rev) <= floor {
						continue;
					}
					if best.as_ref().is_none_or(|(best, ..)| rev > *best) {
						best = Some((rev, route.clone(), peer));
					}
				}
			}
			let Some((_, route, peer)) = best else {
				if created {
					self.views.insert(key, base);
				}
				continue;
			};
			let heard = &self.rx[&route][&peer].view;
			let follows = delta.filter(|delta| delta.route == route && delta.from == peer && delta.was == floor);
			let view = match follows {
				Some(delta) => {
					let mut view = base;
					view.represent(heard, std::slice::from_ref(prefix), &delta.claims);
					#[cfg(test)]
					assert_eq!(view, heard.present(prefix), "incremental view {key:?}");
					dirty.claims.extend(delta.claims.iter().filter(|claim| within(claim, prefix)).cloned());
					dirty.claims.insert(prefix.clone());
					view
				}
				None => {
					let view = heard.present(prefix);
					for (claim, _) in view.diff(&base) {
						dirty.claims.insert(claim);
					}
					view
				}
			};
			self.views.insert(key, view);
		}
		// Which view the reader sees a claim at a view's own prefix from turns on floors.
		let floors: Vec<(Path, Option<Rev>)> = (of(&self.views, id))
			.map(|((_, prefix), view)| (prefix.clone(), view.floors.get(prefix).copied()))
			.collect();
		if floors != was {
			dirty.claims.extend(keys.iter().cloned());
			dirty.claims.extend(floors.into_iter().map(|(prefix, _)| prefix));
		}
		// A view no route keys exactly that says what the one above it says is not kept.
		for prefix in keys.iter().rev() {
			let key = (id, prefix.clone());
			if self.routes.contains_key(&key) {
				continue;
			}
			let Some(view) = self.views.get(&key) else {
				continue;
			};
			let floor = view.floors.get(prefix);
			if self.above(id, prefix).is_some_and(|(above, wider)| wider.floors.get(above) == floor) {
				self.views.remove(&key);
				dirty.all = true;
			}
		}
	}

	/// Count a forget against each claim of `id` this node has shown within `prefix`,
	/// outside `kept`.
	fn forget(&mut self, id: Id, prefix: &Path, kept: Option<&Path>) {
		(self.reader).forget(id, |claim| !within(claim, prefix) || kept.is_some_and(|kept| within(claim, kept)));
	}

	// --- Internals: sending ---

	/// What this node sends `peer`, whose stream answers `watch`, for `id` under `prefix`.
	fn want(&self, id: Id, prefix: &Path, peer: N, watch: &Path) -> Want {
		let session = &self.sessions[&peer];
		if !session.link.proxy && !self.origins.contains_key(&id) {
			return Want::End;
		}
		let src = self.src(id, prefix, watch);
		let Some(route) = self.routes.get(&(id, src.clone())) else {
			return Want::End;
		};
		let want = match &route.offer {
			// A route goes to the node that published it no more than to an upstream link
			// when down-only: neither would use it.
			Some(offer) if session.link.upstream && offer.down_only => Want::End,
			Some(offer) if offer.path[0].node == session.hop => Want::End,
			Some(offer) => Want::Path(offer.clone(), src.clone()),
			// The next hop it holds under hears nothing until those behind this node answered.
			None if route.next == Some(peer) => {
				let key = (id, src.clone());
				match self.pending(&key) || self.hopes(&key) {
					true => return Want::Keep,
					false => Want::End,
				}
			}
			None => Want::Hold,
		};
		// A route goes to a stream only while a claim meets it.
		match (self.meets(id, prefix), want) {
			(true, want) => want,
			(false, Want::Path(..)) => Want::Empty,
			(false, _) => Want::End,
		}
	}

	/// Whether the claims this node presents for `id` at `prefix` hold anything.
	fn meets(&self, id: Id, prefix: &Path) -> bool {
		let Some((_, view)) = self.covering_view(id, prefix) else {
			return false;
		};
		below(&view.items, prefix).next().is_some()
			|| covering(&view.floors, prefix, |claim| view.items.get(claim)).is_some()
	}

	/// The prefixes a stream answering `watch` carries for `id` to `peer`: the interest
	/// itself, each route inside it, and each it was sent and may need to end.
	fn stream(&self, id: Id, peer: N, watch: &Path) -> BTreeSet<Path> {
		let mut out = BTreeSet::from([watch.clone()]);
		out.extend(
			(of(&self.routes, id).map(|((_, prefix), _)| prefix)).filter(|prefix| within(prefix, watch)).cloned(),
		);
		out.extend(of(&self.sessions[&peer].tx, id).map(|((_, prefix), _)| prefix.clone()));
		out
	}

	/// Tell each neighbour that was sent a route through `hop` what became of those
	/// routes, in one [`Dead`] where it is the first word on that hop.
	fn report(&mut self, hop: Hop) {
		let peers: Vec<N> = self.sessions.keys().copied().collect();
		for peer in peers {
			let lo = (peer, (0, Path::empty()));
			let through: Vec<RouteId> = (self.tx_hops.get(&hop).into_iter())
				.flat_map(|set| set.range(lo.clone()..))
				.take_while(|(to, _)| *to == peer)
				.map(|(_, key)| key.clone())
				.collect();
			if through.is_empty() {
				continue;
			}
			let Some(watch) = self.sessions[&peer].watch.clone() else {
				continue;
			};
			let first = !self.sessions[&peer].told.contains(&hop);
			let (mut moved, mut holds, mut parents, mut ends, mut rest) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), 0);
			for key in &through {
				let want = self.want(key.0, &key.1, peer, &watch);
				let sent = &self.sessions[&peer].tx[key];
				match want {
					Want::Path(offer, src) => {
						let old = sent.offer.clone().expect("sent");
						self.unindex_sent(peer, key, &old.path);
						self.index_sent(peer, key, &offer.path);
						let sent = self.sessions.get_mut(&peer).expect("a session").tx.get_mut(key).expect("sent");
						sent.offer = Some(offer.clone());
						sent.held = false;
						sent.src = src;
						moved.push(Moved {
							id: key.0,
							prefix: key.1.clone(),
							offer,
						});
					}
					Want::Hold => {
						if !sent.held {
							// The neighbour it learned the route from takes silence to mean
							// this node holds under it, so it is told outright.
							let parent = child(&sent.offer.as_ref().expect("sent").path, self.sessions[&peer].hop);
							holds.push(key.clone());
							if parent {
								parents.push(key.clone());
							}
							let sent = self.sessions.get_mut(&peer).expect("a session").tx.get_mut(key).expect("sent");
							sent.held = true;
						}
						rest += 1;
					}
					Want::Keep => rest += 1,
					Want::End | Want::Empty => {
						let old = sent.offer.clone().expect("sent");
						self.unindex_sent(peer, key, &old.path);
						let sent = self.sessions.get_mut(&peer).expect("a session").tx.get_mut(key).expect("sent");
						sent.offer = None;
						sent.held = false;
						sent.view = View::default();
						ends.push(key.clone());
					}
				}
			}
			let gone = rest == 0;
			let named = |router: &Self, key: &RouteId| router.name(peer, key.0);
			let route = |router: &Self, key: &RouteId, reach| Out::Route {
				to: peer,
				id: named(router, key),
				prefix: key.1.clone(),
				reach,
			};
			if first || (gone && !ends.is_empty()) {
				let moved = (moved.into_iter())
					.map(|moved| Moved {
						id: self.name(peer, moved.id),
						..moved
					})
					.collect();
				let far = self.dead.get(&hop).copied().expect("a dead hop");
				self.out.push(Out::Dead {
					to: peer,
					dead: Dead { hop, far, moved, gone },
				});
				if !gone {
					for key in &parents {
						let out = route(self, key, Some(Reach::Held));
						self.out.push(out);
					}
					for key in &ends {
						let out = route(self, key, None);
						self.out.push(out);
					}
				}
			} else {
				for moved in moved {
					let key = (moved.id, moved.prefix);
					let out = route(self, &key, Some(Reach::Finite(moved.offer)));
					self.out.push(out);
				}
				for key in &holds {
					let out = route(self, key, Some(Reach::Held));
					self.out.push(out);
				}
				for key in &ends {
					let out = route(self, key, None);
					self.out.push(out);
				}
			}
			self.sessions.get_mut(&peer).expect("a session").told.insert(hop);
		}
	}

	fn index_sent(&mut self, peer: N, key: &RouteId, path: &[Hop]) {
		for hop in path {
			self.tx_hops.entry(*hop).or_default().insert((peer, key.clone()));
		}
	}

	fn unindex_sent(&mut self, peer: N, key: &RouteId, path: &[Hop]) {
		for hop in path {
			if let Some(set) = self.tx_hops.get_mut(hop) {
				set.remove(&(peer, key.clone()));
				if set.is_empty() {
					self.tx_hops.remove(hop);
				}
			}
		}
	}

	/// Bring what each watching neighbour was sent for `id` up to date, with the `dirty`
	/// claims changed since.
	fn advertise(&mut self, id: Id, dirty: &Dirty) {
		let peers: Vec<(N, Path)> = (self.sessions.iter())
			.filter_map(|(peer, session)| Some((*peer, session.watch.clone()?)))
			.collect();
		// Plan every stream against the state as it stands, then send.
		let mut plans = Vec::new();
		for (peer, watch) in &peers {
			for prefix in self.stream(id, *peer, watch) {
				let want = self.want(id, &prefix, *peer, watch);
				let on = (self.sessions[peer].tx.get(&(id, prefix.clone()))).is_some_and(|sent| sent.offer.is_some());
				let batch = match &want {
					Want::Path(..) => Some(self.present(id, *peer, &prefix, dirty)),
					Want::Empty if on && self.covering_view(id, &prefix).is_some() => {
						Some(self.present(id, *peer, &prefix, dirty))
					}
					_ => None,
				};
				plans.push((*peer, prefix, want, batch));
			}
		}
		for (peer, prefix, want, batch) in plans {
			let key = (id, prefix.clone());
			let named = self.name(peer, id);
			let sent = self.sessions[&peer].tx.get(&key);
			let on = sent.is_some_and(|sent| sent.offer.is_some());
			match want {
				Want::Keep => {}
				Want::Hold => {
					if on && !sent.expect("sent").held {
						self.sessions.get_mut(&peer).expect("a session").tx.get_mut(&key).expect("sent").held = true;
						self.out.push(Out::Route {
							to: peer,
							id: named,
							prefix,
							reach: Some(Reach::Held),
						});
					}
				}
				Want::End | Want::Empty => {
					if on {
						if let Some((_, changes, floor)) = batch {
							let was = sent.and_then(|sent| sent.view.floors.get(&prefix).copied());
							if !changes.is_empty() || was != floor {
								self.out.push(Out::Batch {
									to: peer,
									id: named,
									prefix: prefix.clone(),
									changes,
									floor,
								});
							}
						}
						let old = sent.and_then(|sent| sent.offer.clone()).expect("sent");
						self.unindex_sent(peer, &key, &old.path);
						let sent = self.sessions.get_mut(&peer).expect("a session").tx.get_mut(&key).expect("sent");
						sent.offer = None;
						sent.held = false;
						sent.view = View::default();
						self.out.push(Out::Route {
							to: peer,
							id: named,
							prefix,
							reach: None,
						});
					}
				}
				Want::Path(offer, src) => {
					let (from, changes, floor) = batch.expect("a batch");
					let reoffer = sent.is_none_or(|sent| sent.held || sent.offer.as_ref() != Some(&offer));
					if reoffer {
						if let Some(old) = sent.and_then(|sent| sent.offer.clone()) {
							self.unindex_sent(peer, &key, &old.path);
						}
						self.index_sent(peer, &key, &offer.path);
						self.out.push(Out::Route {
							to: peer,
							id: named,
							prefix: prefix.clone(),
							reach: Some(Reach::Finite(offer.clone())),
						});
					}
					let session = self.sessions.get_mut(&peer).expect("a session");
					let sent = session.tx.entry(key).or_insert_with(|| Sent {
						offer: None,
						held: false,
						src: src.clone(),
						from: from.clone(),
						view: View::default(),
					});
					let was = sent.view.floors.get(&prefix).copied();
					let floors = floor.map(|rev| BTreeMap::from([(prefix.clone(), rev)])).unwrap_or_default();
					sent.offer = Some(offer);
					sent.held = false;
					sent.src = src;
					sent.from = from;
					sent.view.apply(changes.clone(), floors);
					if !changes.is_empty() || was != floor {
						self.out.push(Out::Batch {
							to: peer,
							id: named,
							prefix,
							changes,
							floor,
						});
					}
				}
			}
		}
		// Forget what an ended route left once nothing can answer it.
		for (peer, _) in &peers {
			let rx = &self.rx;
			let me = self.hop;
			let session = self.sessions.get_mut(peer).expect("a session");
			session.tx.retain(|key, sent| {
				sent.offer.is_some()
					|| (rx.get(key).and_then(|routes| routes.get(peer)))
						.is_some_and(|heard| heard.state != State::Held && child(&heard.offer.path, me))
			});
		}
	}

	/// The changes that bring what `peer` was sent for `id` under `prefix` to what this
	/// node presents there now, with the view they come from and its floor there.
	fn present(&self, id: Id, peer: N, prefix: &Path, dirty: &Dirty) -> Presented<A> {
		let (from, view) = self.covering_view(id, prefix).expect("a route that meets");
		let floor = view.floors.get(&from).copied();
		let sent = self.sessions[&peer].tx.get(&(id, prefix.clone()));
		let fresh = sent.is_none_or(|sent| sent.offer.is_none() || sent.from != from);
		let changes: Vec<(Path, Option<A>)> = match (sent, dirty.all || fresh) {
			(Some(sent), false) => {
				let mut changes: Vec<(&Path, Option<&A>)> = (dirty.claims.iter())
					.filter(|claim| within(claim, prefix) && *claim != prefix)
					.map(|claim| (claim, view.items.get(claim)))
					.collect();
				changes.push((prefix, covering(&view.floors, prefix, |claim| view.items.get(claim))));
				changes.retain(|(claim, now)| sent.view.items.get(*claim) != *now);
				// Starts and changes, then ends, as a whole snapshot's diff orders them.
				changes.sort_by_key(|(_, now)| now.is_none());
				(changes.into_iter())
					.map(|(claim, now)| (claim.clone(), now.cloned()))
					.collect()
			}
			(sent, _) => {
				let empty = View::default();
				let old = match sent {
					Some(sent) if sent.offer.is_some() => &sent.view,
					_ => &empty,
				};
				view.present(prefix).diff(old)
			}
		};
		#[cfg(test)]
		{
			let mut check = (sent.filter(|sent| sent.offer.is_some()))
				.map(|sent| sent.view.clone())
				.unwrap_or_default();
			let floors = floor.map(|rev| BTreeMap::from([(prefix.clone(), rev)])).unwrap_or_default();
			check.apply(changes.clone(), floors);
			assert_eq!(check, view.present(prefix), "incremental presentation of {id} {prefix:?} to {peer:?}");
		}
		(from, changes, floor)
	}

	// --- Internals: the update ---

	/// Recompute each of `ids`.
	fn update(&mut self, ids: BTreeSet<Id>, reports: Vec<Hop>, claims: Option<(Id, Dirty)>) {
		let mut dirty = BTreeMap::new();
		if let Some((id, claims)) = claims {
			dirty.insert(id, claims);
		}
		self.run(ids, reports, None, dirty);
	}

	/// [`Self::update`] after a batch.
	fn recompute(&mut self, ids: BTreeSet<Id>, reports: Vec<Hop>, claims: Option<(Id, Dirty)>, delta: Option<Delta<N>>) {
		let mut dirty = BTreeMap::new();
		if let Some((id, claims)) = claims {
			dirty.insert(id, claims);
		}
		self.run(ids, reports, delta, dirty);
	}

	/// Select, learn, report each dead hop, advertise, collect, and show: the model's
	/// `Update` for the identities `ids`.
	fn run(
		&mut self,
		mut ids: BTreeSet<Id>,
		reports: Vec<Hop>,
		delta: Option<Delta<N>>,
		mut dirty: BTreeMap<Id, Dirty>,
	) {
		// A report covers every route sent through its hop.
		for hop in &reports {
			ids.extend((self.tx_hops.get(hop).into_iter().flatten()).map(|(_, (id, _))| *id));
		}
		#[cfg(test)]
		{
			self.touched += ids.len();
		}
		for &id in &ids {
			if self.origins.contains_key(&id) {
				continue;
			}
			for prefix in self.prefixes(id) {
				let key = (id, prefix);
				match self.select(&key) {
					Some(route) => self.routes.insert(key, route),
					None => self.routes.remove(&key),
				};
			}
		}
		for &id in &ids {
			let dirty = dirty.entry(id).or_default();
			self.know(id, delta.as_ref(), dirty);
		}
		for hop in reports {
			self.report(hop);
		}
		for &id in &ids {
			let dirty = dirty.remove(&id).unwrap_or_default();
			let ctx = self.ctx(id);
			let dirty = Dirty {
				all: dirty.all || self.ctxs.get(&id) != Some(&ctx),
				claims: dirty.claims,
			};
			self.advertise(id, &dirty);
			self.show(id, &dirty);
			match ctx.views.is_empty() && ctx.routes.is_empty() {
				true => self.ctxs.remove(&id),
				false => self.ctxs.insert(id, ctx),
			};
		}
		self.collect_doubt();
		#[cfg(test)]
		self.check_indexes();
	}

	/// Forget each dead hop no heard route names once its timer fired, each doubt no
	/// heard route names under [`Lift::Unnamed`], and each told hop no sent route names.
	fn collect_doubt(&mut self) {
		let named: BTreeSet<Hop> = self.dead.keys().filter(|hop| self.rx_hops.contains_key(hop)).copied().collect();
		let timers = &self.timers;
		self.dead.retain(|hop, far| named.contains(hop) || timers.contains_key(far));
		if self.doubt.lift == Lift::Unnamed {
			let keep: BTreeSet<Id> = self.doubted.iter().filter(|node| self.named(**node)).copied().collect();
			self.doubted = keep;
		}
		let tx_hops = &self.tx_hops;
		for (peer, session) in self.sessions.iter_mut() {
			session.told.retain(|hop| {
				(tx_hops.get(hop).into_iter().flatten()).any(|(to, _)| to == peer)
			});
		}
	}

	#[cfg(test)]
	fn check_indexes(&self) {
		let mut rx_hops: BTreeMap<Hop, BTreeSet<(RouteId, N)>> = BTreeMap::new();
		for (key, routes) in &self.rx {
			for (peer, heard) in routes {
				for hop in heard.offer.path.iter() {
					rx_hops.entry(*hop).or_default().insert((key.clone(), *peer));
				}
				assert!(self.sessions[peer].heard.contains(key), "heard index");
			}
		}
		assert_eq!(rx_hops, self.rx_hops, "rx hop index");
		let mut tx_hops: BTreeMap<Hop, BTreeSet<(N, RouteId)>> = BTreeMap::new();
		for (peer, session) in &self.sessions {
			for (key, sent) in &session.tx {
				for hop in sent.offer.iter().flat_map(|offer| offer.path.iter()) {
					tx_hops.entry(*hop).or_default().insert((*peer, key.clone()));
				}
			}
		}
		assert_eq!(tx_hops, self.tx_hops, "tx hop index");
	}

	fn ctx(&self, id: Id) -> Ctx {
		Ctx {
			views: (of(&self.views, id))
				.map(|((_, prefix), view)| (prefix.clone(), view.floors.contains_key(prefix)))
				.collect(),
			routes: of(&self.routes, id).map(|((_, prefix), route)| (prefix.clone(), route.live())).collect(),
		}
	}

	// --- Internals: the reader ---

	/// The view the reader sees `claim` of `id` from: the narrowest view above it that
	/// knows something, where the claim sits at its own path. The view at the claim
	/// itself presents there whichever claim covers it, so it is the source only when
	/// none above knows as much: with none above, or a newer floor than theirs.
	fn source(&self, id: Id, claim: &Path) -> Option<(Path, &View<A>)> {
		let at = (self.views.get(&(id, claim.clone()))).filter(|view| view.floors.contains_key(claim));
		match (self.above(id, claim), at) {
			(Some((above, wider)), Some(at)) if at.floors.get(claim) > wider.floors.get(above) => {
				Some((claim.clone(), at))
			}
			(Some((above, wider)), _) => Some((above.clone(), wider)),
			(None, at) => Some((claim.clone(), at.or_else(|| self.views.get(&(id, claim.clone())))?)),
		}
	}

	/// How the reader sees `claim` of `id` now, from its [`Self::source`], available
	/// while a live route covers it.
	fn shown_now(&self, id: Id, claim: &Path) -> Option<Shown<A>> {
		if !within(claim, &self.watch) {
			return None;
		}
		let available = || ancestors(claim).any(|above| self.routes.get(&(id, above)).is_some_and(Route::live));
		if let Some(origin) = self.origins.get(&id) {
			let item = origin.view.items.get(claim)?;
			return Some(Shown {
				item: item.clone(),
				available: true,
				presented: false,
			});
		}
		let (from, view) = self.source(id, claim)?;
		let item = view.items.get(claim)?;
		Some(Shown {
			item: item.clone(),
			available: available(),
			presented: from == *claim,
		})
	}

	/// Recompute what this node's reader sees of `id` and log each change: only the
	/// `dirty` claims, unless any could have changed.
	fn show(&mut self, id: Id, dirty: &Dirty) {
		let claims = match dirty.all {
			true => self.claims(id),
			false => dirty.claims.clone(),
		};
		let now: BTreeMap<Path, Option<Shown<A>>> =
			claims.iter().map(|claim| (claim.clone(), self.shown_now(id, claim))).collect();
		#[cfg(test)]
		if !dirty.all {
			let full = (self.claims(id).into_iter())
				.map(|claim| {
					let shown = self.shown_now(id, &claim);
					(claim, shown)
				})
				.collect();
			self.reader.check(id, &now, full);
		}
		// A claim no longer shown under a route that still covers it was withdrawn.
		let routes = &self.routes;
		let ended = |claim: &Path| ancestors(claim).any(|above| routes.contains_key(&(id, above)));
		self.reader.apply(id, &self.watch, now, ended);
	}

	/// Every claim of `id` the reader sees or could.
	fn claims(&self, id: Id) -> BTreeSet<Path> {
		let mut claims: BTreeSet<Path> = self.reader.claims(id).cloned().collect();
		match self.origins.get(&id) {
			Some(origin) => claims.extend(origin.view.items.keys().cloned()),
			None => {
				for (_, view) in of(&self.views, id) {
					claims.extend(view.items.keys().cloned());
				}
			}
		}
		claims
	}

}
