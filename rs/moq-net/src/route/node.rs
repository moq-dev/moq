//! One origin node's router, shared by every lite-07 announce stream on its origin.
//!
//! Each announce stream is its own neighbour key on the [`Router`], one per direction: a
//! stream this node asked for receives routes and claims, and a stream it answers sends
//! them. Every stream of one session shares the session's [`Peer`] tag: a hold on what a
//! stream this node answers was sent waits on the routes the peer sends back on the
//! session's newest stream this node asked for. A stream that ends while its session
//! stays up retires quietly, as ROUTE_ENDs for its routes would; the streams that end
//! with their session report its hop dead.
//!
//! The node does no I/O. A stream's session feeds it what the peer sent and takes
//! [`Output`]s from its queue: wire messages to send, and the shown claims it serves into
//! the origin. A claim is served through each live route covering it, the narrowest one
//! marked primary: a reader here follows that one, and a request forwarded from a peer
//! follows the route it names ([`Node::forward`]). A stream that stops carrying one lets
//! go only once another serves it, so the origin always holds a route for it.
//!
//! A claim of another identity reaches the other streams only once a stream serving it
//! confirmed that instance, so a peer never names an instance this node's origin cannot
//! route yet. Until then its batches hold the claim back and their floors certify
//! nothing.

use std::{
	collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
	sync::{Arc, Mutex, MutexGuard},
	task::{Poll, ready},
	time::Duration,
};

use kio::clock::{Deadlines, Instant};

use crate::{Instance, Path};

use rand::RngExt;

use super::{
	Error, GENERATED, Id, Link, Metric, QUALIFIED, Rev,
	path::{Dead, Doubt, Forward, Offer, Reach, RouteId, Router},
	view::{ancestors, below},
};

/// One announce stream in one direction.
pub(crate) type Key = u64;

/// A claim's attributes on the route layer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Claim {
	pub instance: Instance,
	pub epoch: u64,
	/// The origin's Announce Cost.
	pub cost: u64,
	/// This node's own claim, served through a route learned from an upstream peer: it
	/// is never sent to another upstream, so this node never carries traffic between
	/// two. Always unset on a claim off the wire.
	pub upstream: bool,
	/// This node's own claim, served through a route another node announced over an
	/// older version: sent only to a peer this node proxies for, so a node that does
	/// not proxy is never transit. Always unset on a claim off the wire.
	pub learned: bool,
	/// The hop chain of a claim learned over moq-lite-06 or moq-transport, which
	/// lite-07 carries as the Lite-06 Chain; empty for a native claim. A relay
	/// translating the claim back to lite-06 extends it, so a node the claim reaches
	/// again recognizes its own hop and drops it. Deleted with lite-06 compatibility.
	pub chain: crate::Hops,
}

/// A shown claim as the origin serves it through one live route.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Served {
	pub claim: Claim,
	/// The route's metric: a path through the claim costs the Announce Cost plus this.
	pub metric: Metric,
	/// The round trip to the route's publisher, informational.
	pub rtt: Option<Duration>,
	/// Whether a usable route serves it; a held one leaves it shown but unavailable.
	pub available: bool,
	/// A node on the route's path sent GOAWAY: it ranks below every route avoiding one.
	pub draining: bool,
	/// A route at the narrowest prefix with a live route covering the claim: one a
	/// reader here follows, and a request naming no route.
	pub primary: bool,
	/// The route this node selected for its prefix; another is an alternate, which a
	/// reader fails over to at once when the selected one goes.
	pub selected: bool,
	/// The origin nodes the route's path crosses: a request from one of them never
	/// follows it.
	pub path: Arc<[u64]>,
}

/// What a stream's session acts on, in order. One route's changes and batches coalesce
/// while the session leaves them unread, but never across a DEAD, which applies to the
/// routes the peer holds when it reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Output {
	/// Send ROUTE_START or ROUTE_UPDATE, ROUTE_HOLD (`Held`), or for `None` ROUTE_END,
	/// for the route to `id` covering `prefix`.
	Route {
		id: Id,
		prefix: Path,
		reach: Option<Reach>,
	},
	/// Send these claim changes for the route, closed by ANNOUNCE_FLOOR.
	Batch {
		id: Id,
		prefix: Path,
		changes: Vec<(Path, Option<Claim>)>,
		floor: Option<Rev>,
	},
	/// Send this DEAD.
	Dead(Dead),
	/// Serve `claim` of `id` into the origin through this stream, along its route covering
	/// `prefix`, or update how it is served. A request forwarded through it names `named`,
	/// the identity as the peer advertised it. Confirm with [`Stream::serving`].
	Serve {
		id: Id,
		named: Id,
		claim: Path,
		prefix: Path,
		served: Served,
	},
	/// Stop serving `claim` of `id` along the route covering `prefix` through this stream.
	/// `ended` when the claim's origin withdrew it, as far as this node knows; otherwise
	/// the node let go of it here.
	Unserve {
		id: Id,
		claim: Path,
		prefix: Path,
		ended: bool,
	},
}

/// The session a stream belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Peer {
	/// Shared by every stream of one session.
	pub tag: u64,
	/// How the peer names itself in a path: the Node ID its SETUP declared, or 0 when it
	/// declared none, in which case the router takes it from the first route it sends.
	pub node: Id,
}

/// The node every lite-07 session of one origin shares.
#[derive(Clone)]
pub(crate) struct Node {
	state: Arc<Mutex<State>>,
	/// When the router's next timer fires, for the task that fires it.
	timer: kio::Shared<Option<Instant>>,
}

impl std::fmt::Debug for Node {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Node").finish_non_exhaustive()
	}
}

/// Each stream's outputs, by its key.
type Queues = HashMap<Key, kio::Shared<Outbox>>;

/// What one stream has yet to read. Route changes and claim batches coalesce per route
/// between DEADs, and a route started and ended unread leaves nothing. Serving changes
/// coalesce per claim and route. A stream whose reader stalls holds one entry per route
/// and claim per DEAD queued meanwhile, and its session closes it once it stalls too long.
#[derive(Default)]
struct Outbox {
	/// Route changes pending, the DEAD closing each segment read after its routes.
	segments: VecDeque<Segment>,
	/// Outputs to read in order: a pending entry once taken, or a DEAD.
	ready: VecDeque<Output>,
	next: u64,
	/// Each claim's last Serve or Unserve, read after the rest, in queue order. A job
	/// queues its Serve before the Unserve or restart of the claim it continues, and the
	/// session needs the job's route in place before the claim's goes, so a reader
	/// resolved through the claim carries on: so a Serve replacing a queued Serve of the
	/// same instance and epoch keeps its place, while an Unserve, a restart, or a Serve
	/// after an Unserve goes last.
	serves: BTreeMap<u64, Output>,
	/// Where each claim's last change on each route sits in `serves`.
	serving: HashMap<(Id, Path, Path), u64>,
	/// The routes the reader holds once it reads everything queued.
	known: HashSet<RouteId>,
	/// Claim starts and restarts held back until a stream serving them confirms their
	/// instance, by route, with the route's last floor, which certifies nothing while any
	/// is held. Bounded like the segments: one per claim. The node's `waiting` indexes
	/// them.
	withheld: HashMap<RouteId, (Held, Option<Rev>)>,
}

/// Route changes queued before the next DEAD.
#[derive(Default)]
struct Segment {
	/// Routes with changes pending, by when they first queued.
	order: BTreeMap<u64, RouteId>,
	pending: HashMap<RouteId, (u64, Pending)>,
	/// The DEAD read once the routes are.
	dead: Option<Dead>,
}

/// One route's changes and claim batches since the segment began.
#[derive(Default)]
struct Pending {
	/// The reader held the route when the segment began.
	was: bool,
	/// The route the reader held ended: a ROUTE_END goes first, after `closing`.
	ended: bool,
	/// The last batch for the route the reader held, read just before its ROUTE_END: its
	/// floor certifies the withdrawals that ended the route.
	closing: Option<Batch>,
	/// The last offer since, which starts the route if the reader holds none.
	finite: Option<Offer>,
	/// The route was held after that.
	held: bool,
	/// The batches since, merged: each claim's last change, and the last floor.
	batch: Option<Batch>,
}

impl Pending {
	fn is_empty(&self) -> bool {
		!self.ended && self.finite.is_none() && !self.held && self.batch.is_none()
	}
}

/// Merged batches: each claim's last change, and the last floor.
type Batch = (Changes, Option<Rev>);

/// Each claim's last change, by path.
type Changes = BTreeMap<Path, Option<Claim>>;

/// Claims held back, by path.
type Held = BTreeMap<Path, Claim>;

/// The streams holding back a claim of each identity, by the path each holds it at,
/// with the prefix of the route each holds it under: one entry per held claim. A change
/// to a home is looked up as a range of paths.
type Waiting = HashMap<Id, BTreeMap<Path, BTreeSet<(Key, Path)>>>;

impl Outbox {
	fn is_empty(&self) -> bool {
		self.ready.is_empty() && self.segments.is_empty() && self.serves.is_empty()
	}

	/// The open segment, a new one after a DEAD.
	fn segment(&mut self) -> &mut Segment {
		if self.segments.back().is_none_or(|segment| segment.dead.is_some()) {
			self.segments.push_back(Segment::default());
		}
		self.segments.back_mut().expect("just pushed")
	}

	/// The pending entry for `key` in the open segment, queued behind the others when new.
	fn entry(&mut self, key: &RouteId) -> &mut Pending {
		let was = self.known.contains(key);
		let next = &mut self.next;
		let segment = {
			if self.segments.back().is_none_or(|segment| segment.dead.is_some()) {
				self.segments.push_back(Segment::default());
			}
			self.segments.back_mut().expect("just pushed")
		};
		let Segment { order, pending, .. } = segment;
		let (_, entry) = pending.entry(key.clone()).or_insert_with(|| {
			let seq = *next;
			*next += 1;
			order.insert(seq, key.clone());
			(
				seq,
				Pending {
					was,
					..Pending::default()
				},
			)
		});
		entry
	}

	/// Drop the open segment's entry for `key` once it holds nothing.
	fn prune(&mut self, key: &RouteId) {
		let Some(segment) = self.segments.back_mut() else {
			return;
		};
		if segment.pending.get(key).is_some_and(|(_, pending)| pending.is_empty())
			&& let Some((seq, _)) = segment.pending.remove(key)
		{
			segment.order.remove(&seq);
		}
	}

	/// Queue a serving change: each replaces the last for its claim and route, the session
	/// acting on where the claim ends up, in place when it keeps its instance.
	fn serve(&mut self, output: Output) {
		let (Output::Serve {
			id, claim, prefix, ..
		}
		| Output::Unserve {
			id, claim, prefix, ..
		}) = &output
		else {
			unreachable!("only serving changes are queued here");
		};
		let key = (*id, claim.clone(), prefix.clone());
		if let Some(&seq) = self.serving.get(&key)
			&& let Output::Serve { served, .. } = &output
			&& let Some(Output::Serve { served: queued, .. }) = self.serves.get(&seq)
			&& queued.claim.instance == served.claim.instance
			&& queued.claim.epoch == served.claim.epoch
		{
			self.serves.insert(seq, output);
			return;
		}
		let seq = self.next;
		self.next += 1;
		if let Some(old) = self.serving.insert(key, seq) {
			self.serves.remove(&old);
		}
		self.serves.insert(seq, output);
	}

	/// Queue a route change for the stream `stream`.
	fn route(&mut self, stream: Key, key: RouteId, reach: Option<Reach>, waiting: &mut Waiting) {
		match reach {
			Some(Reach::Finite(offer)) => {
				let entry = self.entry(&key);
				entry.finite = Some(offer);
				entry.held = false;
				self.known.insert(key);
			}
			Some(Reach::Held) => {
				self.entry(&key).held = true;
			}
			None => {
				// The peer drops the route's claims, so nothing held back is owed.
				if let Some((held, _)) = self.withheld.remove(&key) {
					for path in held.keys() {
						unwait(waiting, key.0, path, stream, &key.1);
					}
				}
				self.known.remove(&key);
				let entry = self.entry(&key);
				match entry.was && !entry.ended {
					// The reader holds the route: it ends, after the floor of its last batch.
					true => {
						entry.ended = true;
						entry.closing = entry.batch.take();
						entry.finite = None;
						entry.held = false;
					}
					// A start the reader never read: it reads nothing of it.
					false => {
						entry.finite = None;
						entry.held = false;
						entry.batch = None;
					}
				}
				self.prune(&key);
			}
		}
	}

	/// Queue a batch of the route's claim changes, closed by `floor`, for the stream
	/// `stream`, holding back each start or restart `awaits` a confirmation.
	fn batch(
		&mut self,
		stream: Key,
		key: RouteId,
		(changes, floor): (Vec<(Path, Option<Claim>)>, Option<Rev>),
		awaits: impl Fn(&Path, &Claim) -> Option<bool>,
		waiting: &mut Waiting,
	) {
		let mut withheld = self.withheld.remove(&key);
		let entry = self.entry(&key);
		let (merged, last) = entry.batch.get_or_insert_default();
		for (path, claim) in changes {
			// A newer change replaces one held back.
			if let Some((held, _)) = &mut withheld
				&& held.remove(&path).is_some()
			{
				unwait(waiting, key.0, &path, stream, &key.1);
			}
			if let Some(item) = &claim
				&& awaits(&path, item) == Some(true)
			{
				// The reader keeps what it was sent until then.
				merged.remove(&path);
				waiting
					.entry(key.0)
					.or_default()
					.entry(path.clone())
					.or_default()
					.insert((stream, key.1.clone()));
				withheld.get_or_insert_default().0.insert(path, item.clone());
				continue;
			}
			merged.insert(path, claim);
		}
		*last = floor;
		if let Some(withheld) = withheld.filter(|(held, _)| !held.is_empty()) {
			self.withheld.insert(key, withheld);
		}
	}

	/// Queue the claim of the route held back at `path`, now confirmed.
	fn release(&mut self, key: &RouteId, path: &Path) {
		let Some((held, floor)) = self.withheld.get_mut(key) else {
			return;
		};
		let Some(item) = held.remove(path) else {
			return;
		};
		let floor = *floor;
		if held.is_empty() {
			self.withheld.remove(key);
		}
		// A batch still queued closes with a newer floor.
		let (merged, _) = self.entry(key).batch.get_or_insert_with(|| (Changes::new(), floor));
		merged.insert(path.clone(), Some(item));
	}

	/// Queue a DEAD, read once every route change queued before it is.
	fn dead(&mut self, dead: Dead) {
		self.segment().dead = Some(dead);
	}

	fn pop(&mut self) -> Option<Output> {
		while self.ready.is_empty() {
			let Some(segment) = self.segments.front_mut() else {
				let (_, output) = self.serves.pop_first()?;
				if let Output::Serve {
					id, claim, prefix, ..
				}
				| Output::Unserve {
					id, claim, prefix, ..
				} = &output
				{
					self.serving.remove(&(*id, claim.clone(), prefix.clone()));
				}
				return Some(output);
			};
			match segment.order.pop_first() {
				Some((_, key)) => {
					let (_, pending) = segment.pending.remove(&key).expect("every queued route is pending");
					self.take(key, pending);
				}
				None => {
					let segment = self.segments.pop_front().expect("matched above");
					if let Some(dead) = segment.dead {
						self.ready.push_back(Output::Dead(dead));
					}
				}
			}
		}
		self.ready.pop_front()
	}

	/// Queue one route's pending entry to read, as the router would have sent it.
	fn take(&mut self, (id, prefix): RouteId, pending: Pending) {
		if pending.ended {
			if let Some((changes, floor)) = pending.closing {
				self.ready.push_back(Output::Batch {
					id,
					prefix: prefix.clone(),
					changes: ordered(changes),
					floor,
				});
			}
			self.ready.push_back(Output::Route {
				id,
				prefix: prefix.clone(),
				reach: None,
			});
		}
		if let Some(offer) = pending.finite {
			self.ready.push_back(Output::Route {
				id,
				prefix: prefix.clone(),
				reach: Some(Reach::Finite(offer)),
			});
		}
		if pending.held {
			self.ready.push_back(Output::Route {
				id,
				prefix: prefix.clone(),
				reach: Some(Reach::Held),
			});
		}
		if let Some((merged, floor)) = pending.batch {
			// What is held back leaves the floor certifying nothing until it goes.
			let floor = match self.withheld.get_mut(&(id, prefix.clone())) {
				Some((_, last)) => {
					*last = floor;
					None
				}
				None => floor,
			};
			self.ready.push_back(Output::Batch {
				id,
				prefix,
				changes: ordered(merged),
				floor,
			});
		}
	}
}

/// Whether a claim of `id` at `instance` and `epoch`, presented at `path`, waits before
/// it goes to other streams: on the shown claim there or, for a claim presented at a
/// narrower interest, the one covering it that serves the same instance, until a stream
/// serving it confirms that instance. `None` when none serves it here to wait on.
fn awaits(homes: &Homes, id: Id, path: &Path, instance: &Instance, epoch: u64) -> Option<bool> {
	let same = |claim: &Claim| claim.instance == *instance && claim.epoch == epoch;
	let home = ancestors(path).rev().find_map(|shown| {
		let home = homes.get(&(id, shown))?;
		home.slots.values().any(|slot| same(&slot.served.claim)).then_some(home)
	})?;
	let confirmed =
		(home.slots.values()).any(|slot| (slot.confirmed.as_ref()).is_some_and(|(i, e)| i == instance && *e == epoch));
	Some(!confirmed)
}

/// The stream `stream` no longer holds back a claim of `id` at `path` under the route
/// covering `prefix`.
fn unwait(waiting: &mut Waiting, id: Id, path: &Path, stream: Key, prefix: &Path) {
	let Some(paths) = waiting.get_mut(&id) else {
		return;
	};
	if let Some(keys) = paths.get_mut(path) {
		keys.remove(&(stream, prefix.clone()));
		if keys.is_empty() {
			paths.remove(path);
		}
	}
	if paths.is_empty() {
		waiting.remove(&id);
	}
}

/// Send on what the streams hold back of `id` at or below each of `changed`, the shown
/// claims whose homes changed, once a home serving it confirmed it: only a home at or
/// above a path decides whether a claim there waits. One no home serves any more stays
/// until the router changes or ends it.
fn recheck(homes: &Homes, queues: &Queues, waiting: &mut Waiting, id: Id, changed: &[Path]) {
	let Some(paths) = waiting.get(&id) else {
		return;
	};
	let mut held = BTreeSet::new();
	for claim in changed {
		let at = paths.get_key_value(claim).into_iter();
		for (path, keys) in at.chain(below(paths, claim)) {
			held.extend(keys.iter().map(|(stream, prefix)| (path.clone(), *stream, prefix.clone())));
		}
	}
	for (path, stream, prefix) in held {
		let Some(queue) = queues.get(&stream) else {
			continue;
		};
		let mut outbox = queue.lock();
		let key = (id, prefix);
		let confirmed = (outbox.withheld.get(&key))
			.and_then(|(held, _)| held.get(&path))
			.and_then(|item| awaits(homes, id, &path, &item.instance, item.epoch))
			== Some(false);
		if confirmed {
			outbox.release(&key, &path);
			unwait(waiting, id, &path, stream, &key.1);
		}
	}
}

/// A batch's changes as the router orders them: what starts or changes, then what ends.
fn ordered(changes: Changes) -> Vec<(Path, Option<Claim>)> {
	let (ends, starts): (Vec<_>, Vec<_>) = changes.into_iter().partition(|(_, claim)| claim.is_none());
	starts.into_iter().chain(ends).collect()
}

/// The node's state, locked. A stream's wakes wait until the lock is released: a waker
/// may poll the stream's session inline, and the session calls back into the node.
struct Locked<'a> {
	// Dropped first, being declared first, so the held wakes go out unlocked.
	state: MutexGuard<'a, State>,
	timer: &'a kio::Shared<Option<Instant>>,
	_hold: kio::Hold,
}

impl std::ops::Deref for Locked<'_> {
	type Target = State;

	fn deref(&self) -> &State {
		&self.state
	}
}

impl std::ops::DerefMut for Locked<'_> {
	fn deref_mut(&mut self) -> &mut State {
		&mut self.state
	}
}

impl Drop for Locked<'_> {
	fn drop(&mut self) {
		// Whatever changed, the timer task waits for the router's next deadline.
		let deadline = self.state.router.deadline();
		if *self.timer.read() != deadline {
			*self.timer.lock() = deadline;
		}
	}
}

struct State {
	router: Router<Key, Claim>,
	/// The origin's clock, which dates every input the router times.
	clock: Deadlines,
	next_key: Key,
	queues: Queues,
	/// Each stream's session tag.
	streams: HashMap<Key, u64>,
	/// Each session's streams.
	sessions: HashMap<u64, Session>,
	/// Each shown claim of another identity, and the streams serving it.
	homes: Homes,
	/// What the streams hold back, by the confirmation it waits on.
	waiting: Waiting,
	/// The live routes each identity's claims were last served through: while they stay
	/// the same, only a claim whose shown state changed is served differently.
	routes: HashMap<Id, Routes>,
}

/// One session's streams on the node.
#[derive(Default)]
struct Session {
	/// The newest stream this node asked the peer for, whose routes answer the holds on
	/// what the streams answering the peer were sent.
	receive: Option<Key>,
	/// The streams answering the peer.
	answers: BTreeSet<Key>,
	/// How many streams it has open.
	open: usize,
	/// How many [`Closing`] guards are held: while any is, a dropped stream joins
	/// `closed` instead of retiring on its own.
	closing: usize,
	/// The streams dropped while a guard was held, disconnected together once the last is
	/// released: the session closed.
	closed: Vec<Key>,
}

/// Each shown claim of another identity, and the streams serving it.
type Homes = BTreeMap<(Id, Path), Home>;

/// An identity's live routes, by prefix: the stream each goes through, its offer, and
/// the identity as that stream's peer names it.
type Routes = Vec<(Path, Key, Offer, Id)>;

/// Where one shown claim is served: each stream and route prefix, what it was last
/// sent, and whether it confirmed serving it.
#[derive(Default)]
struct Home {
	slots: BTreeMap<(Key, Path), Slot>,
}

struct Slot {
	served: Served,
	/// The identity as the stream's peer names it, kept while no route to it is left.
	named: Id,
	/// The instance and epoch the stream last confirmed serving.
	confirmed: Option<(Instance, u64)>,
	/// No longer a live route covering the claim: let go once a wanted slot confirms.
	leaving: bool,
}

impl Home {
	/// Unserve every leaving slot once a wanted one serves the claim.
	fn release(&mut self, queues: &Queues, id: Id, claim: &Path) {
		if !self.slots.values().any(|slot| !slot.leaving && slot.confirmed.is_some()) {
			return;
		}
		self.slots.retain(|(stream, prefix), slot| {
			if slot.leaving {
				serve(
					queues,
					*stream,
					Output::Unserve {
						id,
						claim: claim.clone(),
						prefix: prefix.clone(),
						ended: false,
					},
				);
			}
			!slot.leaving
		});
	}
}

impl Node {
	/// A node declaring a fresh identity, which also names it in the paths of the routes
	/// it forwards, `seed` keying its rotation, dating what it hears by `clock`.
	pub fn new(id: Id, seed: u64, clock: Deadlines) -> Self {
		debug_assert!(
			id != 0 && id & QUALIFIED == 0,
			"a declared identity is non-zero and unqualified"
		);
		let mut router = Router::new(id, seed, Doubt::default());
		router.clock(clock.now());
		router.originate(id);
		Self {
			state: Arc::new(Mutex::new(State {
				router,
				clock,
				next_key: 1,
				queues: HashMap::new(),
				streams: HashMap::new(),
				sessions: HashMap::new(),
				homes: BTreeMap::new(),
				waiting: HashMap::new(),
				routes: HashMap::new(),
			})),
			timer: kio::Shared::new(None),
		}
	}

	/// A node declaring the origin's hop id, so the origin has one identity on every
	/// wire. A declared identity is never qualified, so an id carrying that marker
	/// loses it, or is replaced by a random one if nothing is left.
	pub fn origin(hop: std::num::NonZeroU64, clock: Deadlines) -> Self {
		let mut rng = kio::clock::rng();
		let id = declared_for(hop).unwrap_or_else(|| rng.random_range(1..=GENERATED));
		Self::new(id, rng.random(), clock)
	}

	fn lock(&self) -> Locked<'_> {
		let state = self.state.lock().unwrap_or_else(|poison| poison.into_inner());
		Locked {
			_hold: kio::hold(),
			timer: &self.timer,
			state,
		}
	}

	/// The identity this node declares, which a rotation replaces.
	pub fn declared(&self) -> Id {
		self.lock()
			.router
			.declared()
			.expect("a node always declares an identity")
	}

	/// How this node names itself in the paths of the routes it forwards.
	pub fn hop(&self) -> Id {
		self.lock().router.hop()
	}

	/// This node's own claim at `path` changed: `None` withdraws it.
	pub fn own(&self, path: Path, claim: Option<Claim>) {
		let mut state = self.lock();
		let changed = match claim {
			Some(claim) => {
				let declared = state.router.declared();
				let view = declared.and_then(|id| state.router.view(id, &Path::empty()));
				let current = view.and_then(|view| view.items.get(&path));
				let changed = current != Some(&claim);
				changed && state.router.announce(path, claim).is_ok()
			}
			None => state.router.unannounce(path).is_ok(),
		};
		if changed {
			state.flush();
		}
	}

	/// Register a stream this node asked `peer` for, which becomes the session's stream
	/// whose routes answer the holds on what its other streams were sent.
	pub fn receive(&self, peer: Peer, link: Link) -> Result<Stream, Error> {
		let mut state = self.lock();
		let key = state.register(peer, link)?;
		let session = state.sessions.get_mut(&peer.tag).expect("registered");
		session.receive = Some(key);
		let answers: Vec<Key> = session.answers.iter().copied().collect();
		for answer in answers {
			state.router.pair(answer, key);
		}
		state.flush();
		Ok(self.stream(&state, key))
	}

	/// Register a stream answering `peer`'s request for `prefix`.
	pub fn answer(&self, peer: Peer, link: Link, prefix: Path) -> Result<Stream, Error> {
		let mut state = self.lock();
		let key = state.register(peer, link)?;
		let session = state.sessions.get_mut(&peer.tag).expect("registered");
		session.answers.insert(key);
		if let Some(receive) = session.receive {
			state.router.pair(key, receive);
		}
		if let Err(err) = state.router.peer_interest(key, Some(prefix)) {
			state.unregister(key);
			state.router.retire(key);
			return Err(err);
		}
		state.flush();
		Ok(self.stream(&state, key))
	}

	fn stream(&self, state: &State, key: Key) -> Stream {
		Stream {
			node: self.clone(),
			key,
			queue: state.queues[&key].clone(),
		}
	}

	/// Hold back the disconnect of every stream of the session `tag` dropped until the
	/// guard is released, then disconnect them together, reporting the session's hops
	/// dead: the session closed. A stream dropped outside one retires quietly.
	pub fn closing(&self, tag: u64) -> Closing {
		self.lock().sessions.entry(tag).or_default().closing += 1;
		Closing {
			node: self.clone(),
			tag,
		}
	}

	/// Where a request for `claim` of `id` that the session `tag`'s peer sent goes, naming
	/// the route this node offered it covering `prefix`: here, along this node's route
	/// covering a prefix, or `None` when that route is gone or would lead back to the peer.
	pub fn forward(&self, tag: u64, id: Id, claim: &Path, prefix: &Path) -> Option<Via> {
		let state = self.lock();
		let session = state.sessions.get(&tag)?;
		// The stream that was sent the route, else any answering the peer.
		let from = (session.answers.iter())
			.find(|key| state.router.sent(**key, id, prefix))
			.or_else(|| session.answers.first())
			.copied()?;
		match state.router.forward(id, claim, Some((from, prefix.clone())))? {
			Forward::Here => Some(Via::Here),
			Forward::Next { prefix, .. } => Some(Via::Route { prefix }),
		}
	}

	/// Fire the router's timers due by the origin's clock.
	pub fn timeout(&self) {
		let mut state = self.lock();
		let now = state.clock.now();
		state.router.timeout(now);
		state.flush();
	}

	/// When the router's next timer fires, waking `waiter` once that changes from `armed`.
	pub fn poll_deadline(&self, armed: Option<Instant>, waiter: &kio::Waiter) -> Poll<Option<Instant>> {
		let deadline = ready!(self.timer.poll(waiter, |at| match **at != armed {
			true => Poll::Ready(()),
			false => Poll::Pending,
		}));
		Poll::Ready(*deadline)
	}

	#[cfg(test)]
	pub(crate) fn shown(&self) -> BTreeMap<(Id, Path), (Claim, bool)> {
		(self.lock().router.shown().iter())
			.map(|(key, shown)| (key.clone(), (shown.item.clone(), shown.available)))
			.collect()
	}
}

/// Where a request a peer sent goes; see [`Node::forward`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Via {
	/// This node publishes it.
	Here,
	/// Along this node's route covering `prefix`.
	Route { prefix: Path },
}

/// One announce stream's key on the node. Dropping it retires the stream, or disconnects
/// it with its session; see [`Node::closing`].
pub(crate) struct Stream {
	node: Node,
	key: Key,
	queue: kio::Shared<Outbox>,
}

impl std::fmt::Debug for Stream {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Stream").field("key", &self.key).finish_non_exhaustive()
	}
}

impl Stream {
	/// The next output for this stream's session.
	pub fn poll_output(&self, waiter: &kio::Waiter) -> Poll<Output> {
		let ready = |outbox: &kio::Ref<'_, Outbox>| match outbox.is_empty() {
			true => Poll::Pending,
			false => Poll::Ready(()),
		};
		loop {
			// What was queued may have come to nothing, a route started and ended unread,
			// which leaves the outbox empty to wait on.
			if let Some(output) = ready!(self.queue.poll(waiter, ready)).pop() {
				return Poll::Ready(output);
			}
		}
	}

	/// The next output already queued, without waiting.
	pub fn try_output(&self) -> Option<Output> {
		if self.queue.read().is_empty() {
			return None;
		}
		self.queue.lock().pop()
	}

	fn with<R>(&self, f: impl FnOnce(&mut Router<Key, Claim>, Key, Instant) -> R) -> R {
		let mut state = self.node.lock();
		let now = state.clock.now();
		state.router.clock(now);
		let out = f(&mut state.router, self.key, now);
		state.flush();
		out
	}

	/// ROUTE_START or ROUTE_UPDATE, ROUTE_HOLD (`Held`), or ROUTE_END (`None`) for the
	/// route the peer names `named` covering `prefix`, as the peer names it.
	pub fn route(&self, named: Id, prefix: Path, reach: Option<Reach>) {
		self.with(|router, key, _| {
			let interest = router.interest();
			router.route(key, named, prefix, reach, interest)
		});
	}

	/// A batch of claim changes for the route the peer names `named` covering `prefix`,
	/// closed by its floor.
	pub fn batch(&self, named: Id, prefix: Path, changes: Vec<(Path, Option<Claim>)>, floor: Option<Rev>) {
		self.with(|router, key, _| {
			let interest = router.interest();
			router.batch(key, named, prefix, changes, floor, interest)
		});
	}

	/// How the peer names itself in a path, or 0 while the node has not learned it.
	pub fn peer(&self) -> Id {
		self.node.lock().router.hop_of(self.key)
	}

	/// The peer sent DEAD.
	pub fn dead(&self, dead: Dead) {
		self.with(|router, key, now| router.dead(key, dead, now));
	}

	/// The peer sent GOAWAY: its routes rank below every other.
	pub fn goaway(&self) {
		self.with(|router, key, now| router.goaway(key, now));
	}

	/// The link to the peer has `rtt`, or `None` unmeasured.
	pub fn rtt(&self, rtt: Option<Duration>) {
		self.with(|router, key, _| router.rtt(key, rtt));
	}

	/// This stream's session now serves `claim` of `id` along its route covering `prefix`
	/// at `served`'s instance, so the other streams are sent that instance, and a slot
	/// that no longer carries a live route to it lets go.
	pub fn serving(&self, id: Id, claim: &Path, prefix: &Path, served: &Claim) {
		let mut state = self.node.lock();
		let State {
			homes, queues, waiting, ..
		} = &mut *state;
		let Some(home) = homes.get_mut(&(id, claim.clone())) else {
			return;
		};
		let Some(slot) = home.slots.get_mut(&(self.key, prefix.clone())) else {
			return;
		};
		let confirmed = (served.instance.clone(), served.epoch);
		if slot.confirmed.as_ref() == Some(&confirmed) {
			return;
		}
		slot.confirmed = Some(confirmed);
		home.release(queues, id, claim);
		recheck(homes, queues, waiting, id, std::slice::from_ref(claim));
	}
}

impl Drop for Stream {
	fn drop(&mut self) {
		let mut state = self.node.lock();
		let Some(&tag) = state.streams.get(&self.key) else {
			return;
		};
		let session = state.sessions.get_mut(&tag).expect("a registered stream's session");
		match session.closing {
			0 => state.close(vec![self.key], false),
			_ => session.closed.push(self.key),
		}
	}
}

/// Holds back the disconnect of each stream of one session dropped meanwhile, then
/// disconnects them together on drop. See [`Node::closing`].
pub(crate) struct Closing {
	node: Node,
	tag: u64,
}

impl Drop for Closing {
	fn drop(&mut self) {
		let mut state = self.node.lock();
		let Some(session) = state.sessions.get_mut(&self.tag) else {
			return;
		};
		session.closing -= 1;
		if session.closing > 0 {
			return;
		}
		let closed = std::mem::take(&mut session.closed);
		if !closed.is_empty() {
			state.close(closed, true);
		}
		state.forget(self.tag);
	}
}

/// Queue a serving change for the stream `key`.
fn serve(queues: &Queues, key: Key, output: Output) {
	if let Some(queue) = queues.get(&key) {
		queue.lock().serve(output);
	}
}

impl State {
	/// Connect a stream of `peer`'s session.
	fn register(&mut self, peer: Peer, link: Link) -> Result<Key, Error> {
		let key = self.next_key;
		self.next_key += 1;
		let now = self.clock.now();
		self.router.clock(now);
		self.router.connect(key, peer.node, link)?;
		self.queues.insert(key, kio::Shared::default());
		self.streams.insert(key, peer.tag);
		self.sessions.entry(peer.tag).or_default().open += 1;
		Ok(key)
	}

	/// Forget the stream `key` and its place in its session; the router keeps it until
	/// the caller retires or disconnects it.
	fn unregister(&mut self, key: Key) {
		if let Some(queue) = self.queues.remove(&key) {
			for ((id, prefix), (held, _)) in &queue.read().withheld {
				for path in held.keys() {
					unwait(&mut self.waiting, *id, path, key, prefix);
				}
			}
		}
		let Some(tag) = self.streams.remove(&key) else {
			return;
		};
		let session = self.sessions.get_mut(&tag).expect("a registered stream's session");
		session.open -= 1;
		session.answers.remove(&key);
		if session.receive == Some(key) {
			session.receive = None;
		}
		self.forget(tag);
	}

	/// Drop the session `tag` once it has no stream and no guard.
	fn forget(&mut self, tag: u64) {
		if self
			.sessions
			.get(&tag)
			.is_some_and(|session| session.open == 0 && session.closing == 0)
		{
			self.sessions.remove(&tag);
		}
	}

	/// The dropped streams `keys` ended: with their session when `closed`, which reports
	/// their hops dead, else quietly. What they served goes with them.
	fn close(&mut self, keys: Vec<Key>, closed: bool) {
		for &key in &keys {
			self.unregister(key);
		}
		match closed {
			true => {
				let now = self.clock.now();
				self.router.disconnect(keys.iter().copied(), now);
			}
			false => {
				for &key in &keys {
					self.router.retire(key);
				}
			}
		}
		let keys: HashSet<Key> = keys.into_iter().collect();
		for home in self.homes.values_mut() {
			home.slots.retain(|(key, _), _| !keys.contains(key));
		}
		self.flush();
	}

	/// Route the router's messages and shown claims to the streams. Claims are served
	/// before the batches carrying them go, so a batch knows what is not served yet.
	fn flush(&mut self) {
		let outs = self.router.drain();
		self.serve();
		for out in outs {
			match out {
				super::path::Out::Route { to, id, prefix, reach } => {
					if let Some(queue) = self.queues.get(&to) {
						queue.lock().route(to, (id, prefix), reach, &mut self.waiting);
					}
				}
				super::path::Out::Batch {
					to,
					id,
					prefix,
					changes,
					floor,
				} => {
					if let Some(queue) = self.queues.get(&to) {
						let homes = &self.homes;
						let awaits = |path: &Path, item: &Claim| awaits(homes, id, path, &item.instance, item.epoch);
						queue
							.lock()
							.batch(to, (id, prefix), (changes, floor), awaits, &mut self.waiting);
					}
				}
				super::path::Out::Dead { to, dead } => {
					if let Some(queue) = self.queues.get(&to) {
						queue.lock().dead(dead);
					}
				}
				// The stream answering the peer sends its own once its initial set is out.
				super::path::Out::Synced { .. } => {}
			}
		}
	}

	/// Serve the shown claims that changed through the streams.
	fn serve(&mut self) {
		// The shown claims are read in place; the events only say which changed, and
		// whether a claim that went was ended by its origin or let go here.
		let mut ended = BTreeMap::new();
		for event in self.router.events() {
			ended.insert((event.id, event.claim), event.kind == super::Kind::End);
		}
		let changed = self.router.changed();
		let declared = self.router.declared();
		for (id, claims) in changed {
			if Some(id) != declared {
				self.home(id, claims, &ended);
			}
		}
	}

	/// Serve each shown claim of `id` through each live route covering it, keeping it
	/// where it is, unavailable, while there is none. Only `claims`, whose shown state
	/// changed, are served anew, unless the routes changed too.
	fn home(&mut self, id: Id, claims: BTreeSet<Path>, ended: &BTreeMap<(Id, Path), bool>) {
		let routes: Routes = (self.router.routes(id))
			.flat_map(|(prefix, _)| {
				let usable = self.router.usable(&(id, prefix.clone()));
				usable.into_iter().map(|(next, offer)| (prefix.clone(), next, offer, self.router.name(next, id)))
			})
			.collect();
		let last = self.routes.get(&id).map_or(&[][..], Vec::as_slice);
		let claims = match last == routes.as_slice() {
			true => claims,
			false => {
				match routes.is_empty() {
					true => self.routes.remove(&id),
					false => self.routes.insert(id, routes.clone()),
				};
				let shown =
					(self.router.shown().range((id, Path::empty())..)).take_while(|((shown, _), _)| *shown == id);
				let homes = (self.homes.range((id, Path::empty())..)).take_while(|((home, _), _)| *home == id);
				let mut claims: BTreeSet<Path> = shown.map(|((_, claim), _)| claim.clone()).collect();
				claims.extend(homes.map(|((_, claim), _)| claim.clone()));
				claims
			}
		};
		let changed: Vec<Path> = claims.iter().cloned().collect();
		let (shown, gone): (Vec<Path>, Vec<Path>) =
			(claims.into_iter()).partition(|claim| self.router.shown().contains_key(&(id, claim.clone())));
		// Each prefix's usable routes, the selected one first.
		let mut by_prefix: BTreeMap<&Path, Vec<&(Path, Key, Offer, Id)>> = BTreeMap::new();
		for route in &routes {
			by_prefix.entry(&route.0).or_default().push(route);
		}
		// Most specific first: a job continuing its claim's instance is served before
		// the claim moves on, so a reader resolved through the claim finds the job's route.
		for claim in shown.into_iter().rev() {
			let shown = &self.router.shown()[&(id, claim.clone())];
			let (item, available) = (shown.item.clone(), shown.available);
			// The usable routes covering the claim, the narrowest prefix's last.
			let covering: Vec<&Vec<&(Path, Key, Offer, Id)>> =
				ancestors(&claim).filter_map(|prefix| by_prefix.get(&prefix)).collect();
			let home = self.homes.entry((id, claim.clone())).or_default();
			// With no live route left, the router no longer knows the name a stream's peer
			// gave the identity, so each slot keeps the one it was served under.
			let wanted: Vec<((Key, Path), Served, Id)> = match covering.last() {
				None => (home.slots.iter())
					.map(|(at, slot)| {
						let served = Served {
							claim: item.clone(),
							available: false,
							..slot.served.clone()
						};
						(at.clone(), served, slot.named)
					})
					.collect(),
				Some(narrowest) => (covering.iter())
					.flat_map(|routes| routes.iter().enumerate())
					.map(|(rank, (prefix, next, offer, named))| {
						let served = Served {
							claim: item.clone(),
							metric: offer.metric,
							rtt: offer.rtt,
							available,
							draining: offer.draining,
							primary: prefix == &narrowest[0].0,
							selected: rank == 0,
							path: offer.path.iter().map(|hop| hop.node).collect(),
						};
						((*next, prefix.clone()), served, *named)
					})
					.collect(),
			};
			for slot in home.slots.values_mut() {
				slot.leaving = true;
			}
			for (at, served, named) in wanted {
				match home.slots.get_mut(&at) {
					Some(slot) if slot.served == served && slot.named == named => {
						slot.leaving = false;
						continue;
					}
					Some(slot) => {
						slot.served = served.clone();
						slot.named = named;
						slot.leaving = false;
					}
					None => {
						let slot = Slot {
							served: served.clone(),
							named,
							confirmed: None,
							leaving: false,
						};
						home.slots.insert(at.clone(), slot);
					}
				}
				let (stream, prefix) = at;
				let output = Output::Serve {
					id,
					named,
					claim: claim.clone(),
					prefix,
					served,
				};
				serve(&self.queues, stream, output);
			}
			home.release(&self.queues, id, &claim);
		}
		for claim in gone {
			let Some(home) = self.homes.remove(&(id, claim.clone())) else {
				continue;
			};
			let ended = ended.get(&(id, claim.clone())).copied().unwrap_or(false);
			for (stream, prefix) in home.slots.into_keys() {
				let output = Output::Unserve {
					id,
					claim: claim.clone(),
					prefix,
					ended,
				};
				serve(&self.queues, stream, output);
			}
		}
		// A claim held back may now wait on another home, or on none.
		recheck(&self.homes, &self.queues, &mut self.waiting, id, &changed);
		// Nothing left to recover: let the identity go.
		let shows =
			(self.router.shown().range((id, Path::empty())..).next()).is_some_and(|((shown, _), _)| *shown == id);
		if !self.router.holds(id) && !shows {
			self.routes.remove(&id);
			self.router.collect(id);
		}
	}
}

/// The identity a node created for the origin hop `hop` declares, or `None` when nothing
/// is left once the qualified marker is cleared, and it draws a random one instead.
pub(crate) fn declared_for(hop: std::num::NonZeroU64) -> Option<Id> {
	match hop.get() & !QUALIFIED {
		0 => None,
		id => Some(id),
	}
}

/// The lite-06 hop a router identity travels as, so a relay translating its routes
/// for a lite-06 peer recognizes them coming back. A declared identity is the
/// origin's own hop id; a qualified key is folded into the 53 bits a generated hop
/// id keeps.
pub(crate) fn hop(id: Id) -> crate::Hop {
	if id & QUALIFIED == 0 {
		return crate::Hop::from(std::num::NonZeroU64::new(id).expect("a non-zero identity"));
	}
	// FNV-1a over the identity.
	let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
	for byte in id.to_le_bytes() {
		hash ^= u64::from(byte);
		hash = hash.wrapping_mul(0x0100_0000_01b3);
	}
	let id = (hash & GENERATED).max(1);
	crate::Hop::from(std::num::NonZeroU64::new(id).expect("a non-zero hop id"))
}

#[cfg(test)]
mod tests;
