//! One origin node's router, shared by every lite-07 announce stream on its origin.
//!
//! Each announce stream is its own neighbor key, one per direction: a stream this
//! node asked for (it receives routes and claims, and sends ROUTE_REQUEST) and a
//! stream it answers (it sends routes and claims, and receives ROUTE_REQUEST and
//! the ROUTE_END echo). Keying by stream lets a replacement stream open before
//! the old one closes, so no route drops in between.
//!
//! The node does no I/O. A stream's session feeds it what the peer sent and takes
//! [`Output`]s from its queue: wire messages to send, and the shown claims it
//! serves into the origin as the next hop of a feasible route. A claim is served
//! through every stream with a usable feasible route to its identity, each at its
//! own metric, so the origin ranks them and fails over at once when one goes; any
//! feasible route forwards without a loop. A stream that stops carrying one lets go
//! only once another serves it, so the origin always holds a route for it.
//!
//! A claim of another identity reaches the other streams only once a stream serving
//! it confirmed that instance, so a peer never names an instance this node's origin
//! cannot route yet. Until then its batches hold the claim back and their floors
//! certify nothing.

use std::{
	collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
	sync::{Arc, Mutex, MutexGuard},
	task::{Poll, ready},
};

use crate::{Instance, Path};

use rand::RngExt;

use super::{
	Error, GENERATED, Id, Link, Metric, Out, QUALIFIED, Reach, Rev, Router, Seqno,
	view::{above, ancestors, below},
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

/// A shown claim as the origin serves it through the selected route.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Served {
	pub claim: Claim,
	/// The selected route's metric: a path through the claim costs the Announce
	/// Cost plus this.
	pub metric: Metric,
	/// The round trip to the route's origin node, informational.
	pub rtt: Option<std::time::Duration>,
	/// Whether a usable route serves it; a held one leaves it shown but unavailable.
	pub available: bool,
}

/// What a stream's session acts on, in order. One identity's route changes and
/// batches coalesce while the session leaves them unread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Output {
	/// Send ROUTE_START, ROUTE_UPDATE, ROUTE_HOLD, or for `None` ROUTE_END.
	Route { id: Id, reach: Option<Reach> },
	/// Send these claim changes for the route, closed by ANNOUNCE_FLOOR.
	Batch {
		id: Id,
		changes: Vec<(Path, Option<Claim>)>,
		floor: Option<Rev>,
	},
	/// Echo the peer's ROUTE_END for the identity it named `id`.
	Echo { id: Id },
	/// Serve `claim` of `id` into the origin through this stream, or update how it
	/// is served. A request forwarded through it names `named`, the identity as
	/// the peer advertised it. Confirm with [`Stream::serving`].
	Serve {
		id: Id,
		named: Id,
		claim: Path,
		served: Served,
	},
	/// Stop serving `claim` of `id` through this stream. `ended` when the claim's origin
	/// withdrew it, as far as this node knows; otherwise the node let go of it here.
	Unserve { id: Id, claim: Path, ended: bool },
}

/// The node every lite-07 session of one origin shares.
#[derive(Clone)]
pub(crate) struct Node {
	state: Arc<Mutex<State>>,
}

impl std::fmt::Debug for Node {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Node").finish_non_exhaustive()
	}
}

/// Each stream's outputs, by its key.
type Queues = HashMap<Key, kio::Shared<Outbox>>;

/// What one stream has yet to read. Route changes and claim batches coalesce per
/// identity, and what the reader was never sent leaves nothing when it ends: a route
/// started and ended unread is acked to the router at once, as the reader would.
/// Serving changes coalesce per claim. A stream whose reader stalls holds one entry
/// per route it holds or will start and per claim it serves, however much the node
/// changes meanwhile.
#[derive(Default)]
struct Outbox {
	/// Identities with route changes or claims pending, by when they first queued.
	order: BTreeMap<u64, Id>,
	next: u64,
	pending: HashMap<Id, (u64, Pending)>,
	/// Outputs to read in order: the other kinds, and a pending entry once taken.
	ready: VecDeque<Output>,
	/// Each claim's last Serve or Unserve, read after the rest, in queue order. A job
	/// queues its Serve before the Unserve or restart of the claim it continues, and the
	/// session needs the job's route in place before the claim's goes, so a reader
	/// resolved through the claim carries on: so a Serve replacing a queued Serve of the
	/// same instance and epoch keeps its place, while an Unserve, a restart, or a Serve
	/// after an Unserve goes last.
	serves: BTreeMap<u64, Output>,
	/// Where each claim's last change sits in `serves`.
	serving: HashMap<(Id, Path), u64>,
	/// Each route the reader holds, and the claims it was sent under it.
	delivered: HashMap<Id, BTreeSet<Path>>,
	/// Claim starts and restarts held back until a stream serving them confirms their
	/// instance, by identity, with the identity's last floor, which certifies nothing
	/// while any is held. Bounded like `pending`: one per claim. The node's `waiting`
	/// indexes them.
	withheld: HashMap<Id, (Held, Option<Rev>)>,
	/// ROUTE_REQUESTs to send, by the identity as the peer advertised it. A higher
	/// seqno subsumes a lower one, so a writer that stalls holds one per identity.
	requests: BTreeMap<Id, Seqno>,
}

/// One identity's route changes and claim batches since the reader last took it.
#[derive(Default)]
struct Pending {
	/// A ROUTE_END goes first, ending the route the reader holds and its claims.
	ended: bool,
	/// The last batch for the route the reader holds, read just before its ROUTE_END:
	/// its floor certifies the withdrawals that ended the route.
	closing: Option<Batch>,
	/// The last finite reach since, which starts the route if the reader holds none.
	finite: Option<Reach>,
	/// The route was held after that.
	held: bool,
	/// The batches since, merged: each claim's last change, and the last floor.
	batch: Option<Batch>,
}

/// Merged batches: each claim's last change, and the last floor.
type Batch = (Changes, Option<Rev>);

/// Each claim's last change, by path.
type Changes = BTreeMap<Path, Option<Claim>>;

/// Claims held back, by path.
type Held = BTreeMap<Path, Claim>;

/// The streams holding back a claim of each identity, by the path each holds it at:
/// one entry per held claim. A change to a home is looked up as a range of paths.
type Waiting = HashMap<Id, BTreeMap<Path, BTreeSet<Key>>>;

impl Outbox {
	fn is_empty(&self) -> bool {
		self.ready.is_empty() && self.order.is_empty() && self.serves.is_empty()
	}

	/// The pending entry for `id`, queued behind the others when new.
	fn entry(&mut self, id: Id) -> &mut Pending {
		let (order, next) = (&mut self.order, &mut self.next);
		let (_, entry) = self.pending.entry(id).or_insert_with(|| {
			let seq = *next;
			*next += 1;
			order.insert(seq, id);
			(seq, Pending::default())
		});
		entry
	}

	/// Queue `output`.
	fn push(&mut self, output: Output) {
		match output {
			Output::Route { reach: None, .. } => unreachable!("a route end is queued with Outbox::end"),
			Output::Route {
				id,
				reach: Some(Reach::Held),
			} => {
				self.entry(id).held = true;
			}
			Output::Route {
				id,
				reach: Some(finite),
			} => {
				let entry = self.entry(id);
				entry.finite = Some(finite);
				entry.held = false;
			}
			Output::Batch { .. } => unreachable!("a batch is queued with Outbox::batch"),
			// Each serving change replaces the last: the session acts on where the claim
			// ends up, in place when it keeps its instance.
			Output::Serve { id, ref claim, .. } | Output::Unserve { id, ref claim, .. } => {
				let key = (id, claim.clone());
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
			Output::Echo { .. } => {
				self.ready.push_back(output);
			}
		}
	}

	/// Queue the end of `id`'s route, for the stream `key`. Returns `id` when the reader
	/// will never read it, since it never read the route it ends, for the caller to ack
	/// in its place.
	fn end(&mut self, key: Key, id: Id, waiting: &mut Waiting) -> Option<Id> {
		// The peer drops the route's claims, so nothing held back is owed.
		let held = self.withheld.remove(&id);
		for path in held.iter().flat_map(|(held, _)| held.keys()) {
			unwait(waiting, id, path, key);
		}
		let holds = self.delivered.contains_key(&id);
		let entry = self.entry(id);
		// The reader holds the route once it reads what is pending: an end of that keeps
		// any end before, an end of a start it never read is acked.
		let ended = entry.ended;
		let acked = !holds || ended;
		let closing = match ended {
			true => entry.closing.take(),
			// Its floor certifies nothing if a claim was held back.
			false => {
				(entry.batch.take().filter(|_| holds)).map(|(merged, floor)| (merged, floor.filter(|_| held.is_none())))
			}
		};
		*entry = Pending {
			ended: holds,
			closing,
			..Pending::default()
		};
		if !holds && let Some((seq, _)) = self.pending.remove(&id) {
			self.order.remove(&seq);
		}
		acked.then_some(id)
	}

	/// Queue a batch of `id`'s claim changes, closed by `floor`, for the stream `key`,
	/// holding back each start or restart `awaits` a confirmation.
	fn batch(
		&mut self,
		key: Key,
		id: Id,
		(changes, floor): (Vec<(Path, Option<Claim>)>, Option<Rev>),
		awaits: impl Fn(&Path, &Claim) -> Option<bool>,
		waiting: &mut Waiting,
	) {
		let ended = self.entry(id).ended;
		let Self {
			pending,
			delivered,
			withheld,
			..
		} = self;
		let (_, entry) = pending.get_mut(&id).expect("just queued");
		let sent = delivered.get(&id);
		let (merged, last) = entry.batch.get_or_insert_default();
		for (path, claim) in changes {
			// A newer change replaces one held back.
			if let Some((held, _)) = withheld.get_mut(&id)
				&& held.remove(&path).is_some()
			{
				unwait(waiting, id, &path, key);
			}
			if let Some(item) = &claim
				&& awaits(&path, item) == Some(true)
			{
				// The reader keeps what it was sent until then.
				merged.remove(&path);
				waiting
					.entry(id)
					.or_default()
					.entry(path.clone())
					.or_default()
					.insert(key);
				withheld.entry(id).or_default().0.insert(path, item.clone());
				continue;
			}
			// The reader holds nothing here once the entry reaches it.
			let unsent = ended || sent.is_none_or(|sent| !sent.contains(&path));
			match claim {
				None if unsent => merged.remove(&path),
				claim => merged.insert(path, claim),
			};
		}
		*last = floor;
		if withheld.get(&id).is_some_and(|(held, _)| held.is_empty()) {
			withheld.remove(&id);
		}
	}

	/// Queue the claim of `id` held back at `path`, now confirmed.
	fn release(&mut self, id: Id, path: &Path) {
		let Some((held, floor)) = self.withheld.get_mut(&id) else {
			return;
		};
		let Some(item) = held.remove(path) else {
			return;
		};
		let floor = *floor;
		if held.is_empty() {
			self.withheld.remove(&id);
		}
		// A batch still queued closes with a newer floor.
		let (merged, _) = self.entry(id).batch.get_or_insert_with(|| (Changes::new(), floor));
		merged.insert(path.clone(), Some(item));
	}

	fn pop(&mut self) -> Option<Output> {
		if self.ready.is_empty() {
			match self.order.pop_first() {
				Some((_, id)) => {
					let (_, pending) = self.pending.remove(&id).expect("every queued identity is pending");
					self.take(id, pending);
				}
				None => {
					let (_, output) = self.serves.pop_first()?;
					if let Output::Serve { id, claim, .. } | Output::Unserve { id, claim, .. } = &output {
						self.serving.remove(&(*id, claim.clone()));
					}
					return Some(output);
				}
			}
		}
		self.ready.pop_front()
	}

	/// Queue one identity's pending entry to read, as the router would have sent it.
	fn take(&mut self, id: Id, pending: Pending) {
		if pending.ended {
			if let Some(closing) = pending.closing {
				self.ready.push_back(Output::Batch {
					id,
					changes: ordered(closing.0),
					floor: closing.1,
				});
			}
			self.delivered.remove(&id);
			self.ready.push_back(Output::Route { id, reach: None });
		}
		if let Some(reach) = pending.finite {
			self.delivered.entry(id).or_default();
			self.ready.push_back(Output::Route { id, reach: Some(reach) });
		}
		if pending.held {
			self.ready.push_back(Output::Route {
				id,
				reach: Some(Reach::Held),
			});
		}
		if let Some((merged, floor)) = pending.batch {
			// What is held back leaves the floor certifying nothing until it goes.
			let floor = match self.withheld.get_mut(&id) {
				Some((_, last)) => {
					*last = floor;
					None
				}
				None => floor,
			};
			let changes = ordered(merged);
			if let Some(sent) = self.delivered.get_mut(&id) {
				for (path, claim) in &changes {
					match claim {
						Some(_) => sent.insert(path.clone()),
						None => sent.remove(path),
					};
				}
			}
			self.ready.push_back(Output::Batch { id, changes, floor });
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
		home.keys.values().any(|slot| same(&slot.served.claim)).then_some(home)
	})?;
	let confirmed =
		(home.keys.values()).any(|slot| (slot.confirmed.as_ref()).is_some_and(|(i, e)| i == instance && *e == epoch));
	Some(!confirmed)
}

/// The stream `key` no longer holds back a claim of `id` at `path`.
fn unwait(waiting: &mut Waiting, id: Id, path: &Path, key: Key) {
	let Some(paths) = waiting.get_mut(&id) else {
		return;
	};
	if let Some(keys) = paths.get_mut(path) {
		keys.remove(&key);
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
			held.extend(keys.iter().map(|key| (path.clone(), *key)));
		}
	}
	for (path, key) in held {
		let Some(queue) = queues.get(&key) else {
			continue;
		};
		let mut outbox = queue.lock();
		let confirmed = (outbox.withheld.get(&id))
			.and_then(|(held, _)| held.get(&path))
			.and_then(|item| awaits(homes, id, &path, &item.instance, item.epoch))
			== Some(false);
		if confirmed {
			outbox.release(id, &path);
			unwait(waiting, id, &path, key);
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

struct State {
	router: Router<Key, Claim>,
	next_key: Key,
	queues: Queues,
	/// Each shown claim of another identity, and the streams serving it.
	homes: Homes,
	/// What the streams hold back, by the confirmation it waits on.
	waiting: Waiting,
	/// The usable feasible routes each identity's claims were last served through,
	/// with the name each stream's peer gives it: while they stay the same, only a
	/// claim whose shown state changed is served differently.
	routes: HashMap<Id, Routes>,
	/// The streams whose close changes a path to this node, which then advances its
	/// seqno so a node remembering a shorter path accepts the longer one without a
	/// ROUTE_REQUEST: one whose peer forwarded it a route, or, on a node that does
	/// not forward, one this node's route goes out on. A new stream lengthens no path.
	links: HashSet<Key>,
	/// Answer streams of a node that does not forward, which join `links` once this
	/// node's route goes out on them.
	origins: HashSet<Key>,
	/// How many [`Closing`] guards are held: while any is, a dropped stream joins
	/// `closed` instead of disconnecting on its own.
	closing: usize,
	/// The streams dropped while a [`Closing`] guard was held, disconnected together
	/// once the last is released.
	closed: Vec<Key>,
}

/// Each shown claim of another identity, and the streams serving it.
type Homes = BTreeMap<(Id, Path), Home>;

/// An identity's usable feasible routes: each stream, its metric and RTT, and the
/// peer's name.
type Routes = Vec<(Key, Metric, Option<std::time::Duration>, Id)>;

/// Where one shown claim is served: each stream, what it was last sent, and
/// whether it confirmed serving it.
#[derive(Default)]
struct Home {
	keys: BTreeMap<Key, Slot>,
}

struct Slot {
	served: Served,
	/// The identity as the stream's peer names it, kept while no route to it is left.
	named: Id,
	/// The instance and epoch the stream last confirmed serving.
	confirmed: Option<(Instance, u64)>,
	/// No longer a route to the claim's identity: let go once a wanted stream confirms.
	leaving: bool,
}

impl Home {
	/// Unserve every leaving stream once a wanted one serves the claim.
	fn release(&mut self, queues: &Queues, id: Id, claim: &Path) {
		if !self.keys.values().any(|slot| !slot.leaving && slot.confirmed.is_some()) {
			return;
		}
		self.keys.retain(|key, slot| {
			if slot.leaving {
				push(
					queues,
					*key,
					Output::Unserve {
						id,
						claim: claim.clone(),
						ended: false,
					},
				);
			}
			!slot.leaving
		});
	}
}

impl Node {
	/// A node declaring a fresh identity, which also names it in the hops of the routes
	/// it forwards, `seed` keying its rotation.
	pub fn new(id: Id, seed: u64) -> Self {
		debug_assert!(
			id != 0 && id & QUALIFIED == 0,
			"a declared identity is non-zero and unqualified"
		);
		let mut router = Router::new(id, seed);
		router.originate(id, 0);
		Self {
			state: Arc::new(Mutex::new(State {
				router,
				next_key: 1,
				queues: HashMap::new(),
				homes: BTreeMap::new(),
				waiting: HashMap::new(),
				routes: HashMap::new(),
				links: HashSet::new(),
				origins: HashSet::new(),
				closing: 0,
				closed: Vec::new(),
			})),
		}
	}

	/// A node declaring the origin's hop id, so the origin has one identity on every
	/// wire. A declared identity is never qualified, so an id carrying that marker
	/// loses it, or is replaced by a random one if nothing is left.
	pub fn origin(hop: std::num::NonZeroU64) -> Self {
		let mut rng = kio::clock::rng();
		let id = declared_for(hop).unwrap_or_else(|| rng.random_range(1..=GENERATED));
		Self::new(id, rng.random())
	}

	fn lock(&self) -> Locked<'_> {
		Locked {
			_hold: kio::hold(),
			state: self.state.lock().unwrap_or_else(|poison| poison.into_inner()),
		}
	}

	/// The identity this node declares, which a rotation replaces.
	pub fn declared(&self) -> Id {
		self.lock()
			.router
			.declared()
			.expect("a node always declares an identity")
	}

	/// This node's own claim at `path` changed: `None` withdraws it.
	pub fn own(&self, path: Path, claim: Option<Claim>) {
		let mut state = self.lock();
		let changed = match claim {
			Some(claim) => {
				let view = state.router.declared().and_then(|id| state.router.view(id));
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

	/// Register a stream this node asked the peer for under `prefix`.
	pub fn receive(&self, link: Link, prefix: Path) -> Result<Stream, Error> {
		self.register(link, false, |router, key| router.asks(key, prefix))
	}

	/// Register a stream answering the peer's request for `prefix`.
	pub fn answer(&self, link: Link, prefix: Path) -> Result<Stream, Error> {
		// A node that forwards nothing knows no better than that every stream carrying
		// its route is a path to it; one whose prefix matches no claim carries none yet.
		let origin = !link.proxy;
		self.register(link, origin, |router, key| router.peer_interest(key, prefix))
	}

	fn register(
		&self,
		link: Link,
		origin: bool,
		open: impl FnOnce(&mut Router<Key, Claim>, Key) -> Result<(), Error>,
	) -> Result<Stream, Error> {
		let mut state = self.lock();
		let key = state.next_key;
		state.next_key += 1;
		state.router.connect(key, link)?;
		let queue = kio::Shared::default();
		state.queues.insert(key, queue.clone());
		if origin {
			state.origins.insert(key);
		}
		if let Err(err) = open(&mut state.router, key) {
			state.router.disconnect([key]);
			state.queues.remove(&key);
			state.origins.remove(&key);
			state.links.remove(&key);
			return Err(err);
		}
		state.flush();
		Ok(Stream {
			node: self.clone(),
			key,
			queue,
		})
	}

	/// Hold back the disconnect of every stream dropped until the guard is released,
	/// then disconnect them together: a session closing many streams at once costs the
	/// route layer one recompute rather than one per stream.
	pub fn closing(&self) -> Closing {
		self.lock().closing += 1;
		Closing { node: self.clone() }
	}

	#[cfg(test)]
	pub(crate) fn retained(&self) -> super::Retained {
		self.lock().router.retained()
	}

	#[cfg(test)]
	pub(crate) fn shown(&self) -> BTreeMap<(Id, Path), (Claim, bool)> {
		(self.lock().router.shown().iter())
			.map(|(key, shown)| (key.clone(), (shown.item.clone(), shown.available)))
			.collect()
	}
}

/// One announce stream's key on the node. Dropping it disconnects the stream.
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
		let mut outbox = ready!(self.queue.poll(waiter, ready));
		Poll::Ready(outbox.pop().expect("an outbox holding something pops it"))
	}

	/// The next output already queued, without waiting.
	pub fn try_output(&self) -> Option<Output> {
		if self.queue.read().is_empty() {
			return None;
		}
		self.queue.lock().pop()
	}

	/// The ROUTE_REQUESTs queued since the last call, each identity's highest seqno.
	/// Take them only once the writer has room, so they keep coalescing meanwhile.
	pub fn poll_requests(&self, waiter: &kio::Waiter) -> Poll<BTreeMap<Id, Seqno>> {
		let ready = |outbox: &kio::Ref<'_, Outbox>| match outbox.requests.is_empty() {
			true => Poll::Pending,
			false => Poll::Ready(()),
		};
		let mut outbox = ready!(self.queue.poll(waiter, ready));
		Poll::Ready(std::mem::take(&mut outbox.requests))
	}

	fn with<R>(&self, f: impl FnOnce(&mut Router<Key, Claim>, Key) -> R) -> R {
		let mut state = self.node.lock();
		let out = f(&mut state.router, self.key);
		state.flush();
		out
	}

	/// ROUTE_START, ROUTE_UPDATE, ROUTE_HOLD (`Held`), or ROUTE_END (`None`) for
	/// the identity the peer names `named`.
	pub fn route(&self, named: Id, reach: Option<Reach>) {
		let mut state = self.node.lock();
		// Only the peer's own route has metric 0: any other means the peer forwards.
		if matches!(&reach, Some(Reach::Finite(advert)) if advert.metric > 0) {
			state.links.insert(self.key);
		}
		state.router.route(self.key, named, reach);
		state.flush();
	}

	/// `next`, of the same session, takes this stream's place: it inherits whether
	/// this one was a route link, so closing this one changes no path.
	pub fn replaced_by(&self, next: &Stream) {
		let mut state = self.node.lock();
		if state.links.remove(&self.key) {
			state.links.insert(next.key);
		}
		if state.origins.remove(&self.key) {
			state.origins.insert(next.key);
		}
	}

	/// A batch of claim changes for the route the peer names `named`, closed by
	/// its floor.
	pub fn batch(&self, named: Id, changes: Vec<(Path, Option<Claim>)>, floor: Option<Rev>) {
		self.with(|router, key| {
			let interest = router.interest();
			router.batch(key, named, changes, floor, interest)
		});
	}

	/// The peer sent ROUTE_REQUEST for `id`, as this node advertised it.
	pub fn request(&self, id: Id, seqno: Seqno) {
		self.with(|router, key| router.request(key, id, seqno));
	}

	/// The peer echoed this node's ROUTE_END for `id`.
	pub fn echo(&self, id: Id) {
		self.with(|router, key| router.echo(key, id));
	}

	/// The peer sent GOAWAY: its routes rank below every other.
	pub fn goaway(&self) {
		self.with(|router, key| router.goaway(key));
	}

	/// The link to the peer has `rtt`, or `None` unmeasured.
	pub fn rtt(&self, rtt: Option<std::time::Duration>) {
		self.with(|router, key| router.rtt(key, rtt));
	}

	/// This stream's session now serves `claim` of `id` at `served`'s instance, so the
	/// other streams are sent that instance, and a stream that no longer carries a route
	/// to it lets go.
	pub fn serving(&self, id: Id, claim: &Path, served: &Claim) {
		let mut state = self.node.lock();
		let State {
			homes, queues, waiting, ..
		} = &mut *state;
		let Some(home) = homes.get_mut(&(id, claim.clone())) else {
			return;
		};
		let Some(slot) = home.keys.get_mut(&self.key) else {
			return;
		};
		let confirmed = (served.instance.clone(), served.epoch);
		if slot.confirmed.as_ref() == Some(&confirmed) {
			return;
		}
		slot.confirmed = Some(confirmed.clone());
		home.release(queues, id, claim);
		recheck(homes, queues, waiting, id, std::slice::from_ref(claim));
	}
}

impl Drop for Stream {
	fn drop(&mut self) {
		let mut state = self.node.lock();
		match state.closing {
			0 => state.close(vec![self.key]),
			_ => state.closed.push(self.key),
		}
	}
}

/// Holds back the disconnect of each stream dropped meanwhile, then disconnects them
/// together on drop. See [`Node::closing`].
pub(crate) struct Closing {
	node: Node,
}

impl Drop for Closing {
	fn drop(&mut self) {
		let mut state = self.node.lock();
		state.closing -= 1;
		if state.closing == 0 && !state.closed.is_empty() {
			let closed = std::mem::take(&mut state.closed);
			state.close(closed);
		}
	}
}

/// Queue `output` for the stream `key`.
fn push(queues: &Queues, key: Key, output: Output) {
	if let Some(queue) = queues.get(&key) {
		queue.lock().push(output);
	}
}

impl State {
	/// Route the router's messages and shown claims to the streams. Claims are served
	/// before the batches carrying them go, so a batch knows what is not served yet.
	/// Disconnect the dropped streams `keys`: their sessions are gone, and what they
	/// served with them.
	fn close(&mut self, keys: Vec<Key>) {
		for &key in &keys {
			if let Some(queue) = self.queues.remove(&key) {
				for (id, (held, _)) in &queue.read().withheld {
					for path in held.keys() {
						unwait(&mut self.waiting, *id, path, key);
					}
				}
			}
		}
		self.router.disconnect(keys.iter().copied());
		let keys: HashSet<Key> = keys.into_iter().collect();
		for home in self.homes.values_mut() {
			home.keys.retain(|key, _| !keys.contains(key));
		}
		self.origins.retain(|key| !keys.contains(key));
		let linked = self.links.len();
		self.links.retain(|key| !keys.contains(key));
		if self.links.len() != linked {
			let _ = self.router.bump();
		}
		self.flush();
	}

	fn flush(&mut self) {
		loop {
			let outs = self.router.drain();
			self.serve();
			let mut acked = Vec::new();
			for out in outs {
				let (key, output) = match out {
					Out::Route { to, id, reach: None } => {
						if let Some(queue) = self.queues.get(&to)
							&& let Some(id) = queue.lock().end(to, id, &mut self.waiting)
						{
							acked.push((to, id));
						}
						continue;
					}
					Out::Route { to, id, reach } => {
						if Some(id) == self.router.declared() && self.origins.contains(&to) {
							self.links.insert(to);
						}
						(to, Output::Route { id, reach })
					}
					Out::Batch { to, id, changes, floor } => {
						if let Some(queue) = self.queues.get(&to) {
							let homes = &self.homes;
							let awaits =
								|path: &Path, item: &Claim| awaits(homes, id, path, &item.instance, item.epoch);
							queue.lock().batch(to, id, (changes, floor), awaits, &mut self.waiting);
						}
						continue;
					}
					Out::Request { to, id, seqno } => {
						if let Some(queue) = self.queues.get(&to) {
							let mut outbox = queue.lock();
							let want = outbox.requests.entry(id).or_insert(seqno);
							*want = (*want).max(seqno);
						}
						continue;
					}
					Out::Echo { to, id } => (to, Output::Echo { id }),
				};
				push(&self.queues, key, output);
			}
			if acked.is_empty() {
				break;
			}
			// A ROUTE_END coalesced away before its reader read the route: no peer holds it.
			for (key, id) in acked {
				self.router.echo(key, id);
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

	/// Serve each shown claim of `id` through every stream with a usable feasible
	/// route to it that can carry the claim, keeping it where it is, unavailable, while
	/// there is none. Only `claims`, whose shown state changed, are served anew, unless
	/// the routes changed too.
	fn home(&mut self, id: Id, claims: BTreeSet<Path>, ended: &BTreeMap<(Id, Path), bool>) {
		let routes: Routes = (self.router.routes(id).into_iter())
			.map(|(key, metric, rtt)| (key, metric, rtt, self.router.name(key, id)))
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
		// Each route by the prefix its stream asked for, so a claim looks up the routes
		// that may carry it per segment rather than checking every route.
		let mut asked: BTreeMap<Path, Vec<usize>> = BTreeMap::new();
		for (index, (key, ..)) in routes.iter().enumerate() {
			asked.entry(self.router.asked(*key)).or_default().push(index);
		}
		// Most specific first: a job continuing its claim's instance is served before
		// the claim moves on, so a reader resolved through the claim finds the job's route.
		for claim in shown.into_iter().rev() {
			let shown = &self.router.shown()[&(id, claim.clone())];
			let (item, available) = (shown.item.clone(), shown.available);
			let mut indices: Vec<usize> = above(&asked, &claim)
				.flat_map(|(_, indices)| indices)
				.copied()
				.collect();
			indices.sort_unstable();
			let carrying: Vec<(Key, Metric, Option<std::time::Duration>, Id)> = (indices.into_iter())
				.map(|index| routes[index])
				.filter(|(key, ..)| self.router.carries(*key, &claim))
				.collect();
			let home = self.homes.entry((id, claim.clone())).or_default();
			// With no route left, the router no longer knows the name a stream's peer
			// gave the identity, so it keeps the one it was served under.
			let wanted: Vec<(Key, Metric, Option<std::time::Duration>, bool, Id)> = match carrying.is_empty() {
				true => (home.keys.iter())
					.map(|(key, slot)| (*key, slot.served.metric, slot.served.rtt, false, slot.named))
					.collect(),
				false => (carrying.into_iter())
					.map(|(key, metric, rtt, named)| (key, metric, rtt, available, named))
					.collect(),
			};
			for slot in home.keys.values_mut() {
				slot.leaving = true;
			}
			for (key, metric, rtt, available, named) in wanted {
				let served = Served {
					claim: item.clone(),
					metric,
					rtt,
					available,
				};
				match home.keys.get_mut(&key) {
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
						home.keys.insert(key, slot);
					}
				}
				let serve = Output::Serve {
					id,
					named,
					claim: claim.clone(),
					served,
				};
				push(&self.queues, key, serve);
			}
			home.release(&self.queues, id, &claim);
		}
		for claim in gone {
			let Some(home) = self.homes.remove(&(id, claim.clone())) else {
				continue;
			};
			let ended = ended.get(&(id, claim.clone())).copied().unwrap_or(false);
			for key in home.keys.into_keys() {
				push(
					&self.queues,
					key,
					Output::Unserve {
						id,
						claim: claim.clone(),
						ended,
					},
				);
			}
		}
		// A claim held back may now wait on another home, or on none.
		recheck(&self.homes, &self.queues, &mut self.waiting, id, &changed);
		#[cfg(test)]
		self.check(id, &routes);
		// Nothing left to recover: let the identity go.
		let shows =
			(self.router.shown().range((id, Path::empty())..).next()).is_some_and(|((shown, _), _)| *shown == id);
		if !self.router.holds(id) && !shows {
			self.routes.remove(&id);
			self.router.collect(id);
		}
	}

	/// Every claim of `id` is served as serving it anew would: only shown claims have
	/// a home, and each is served through every route carrying it, or kept unavailable.
	#[cfg(test)]
	fn check(&self, id: Id, routes: &Routes) {
		let homes = (self.homes.range((id, Path::empty())..)).take_while(|((home, _), _)| *home == id);
		for ((_, claim), _) in homes {
			assert!(
				self.router.shown().contains_key(&(id, claim.clone())),
				"{claim:?} has a home but is not shown"
			);
		}
		let shown = (self.router.shown().range((id, Path::empty())..)).take_while(|((shown, _), _)| *shown == id);
		for ((_, claim), shown) in shown {
			let home = self.homes.get(&(id, claim.clone()));
			let slots = home.into_iter().flat_map(|home| home.keys.iter());
			let carrying: Vec<_> = (routes.iter())
				.filter(|(key, ..)| self.router.carries(*key, claim))
				.collect();
			if carrying.is_empty() {
				for (_, slot) in slots {
					assert_eq!(slot.served.claim, shown.item, "{claim:?} kept as shown");
					assert!(!slot.served.available, "{claim:?} kept unavailable");
				}
				continue;
			}
			let slots: BTreeMap<Key, &Slot> = slots.filter(|(_, slot)| !slot.leaving).map(|(k, s)| (*k, s)).collect();
			let want: BTreeMap<Key, (Served, Id)> = (carrying.iter())
				.map(|&&(key, metric, rtt, named)| {
					let served = Served {
						claim: shown.item.clone(),
						metric,
						rtt,
						available: shown.available,
					};
					(key, (served, named))
				})
				.collect();
			let got: BTreeMap<Key, (Served, Id)> = (slots.iter())
				.map(|(key, slot)| (*key, (slot.served.clone(), slot.named)))
				.collect();
			assert_eq!(got, want, "{claim:?} served through every route carrying it");
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
mod tests {
	use super::*;
	use crate::route::Advert;

	fn p(path: &'static str) -> Path {
		Path::from_static(path)
	}

	fn claim(instance: &Instance, cost: u64) -> Claim {
		Claim {
			instance: instance.clone(),
			epoch: 0,
			cost,
			upstream: false,
			learned: false,
			chain: crate::Hops::new(),
		}
	}

	fn outputs(stream: &Stream) -> Vec<Output> {
		std::iter::from_fn(|| stream.try_output()).collect()
	}

	/// Deliver everything `from` would send on the wire to the stream receiving it,
	/// returning what else it queued.
	fn pump(from: &Stream, to: &Stream) -> Vec<Output> {
		let mut rest = Vec::new();
		for output in outputs(from) {
			match output {
				Output::Route { id, reach } => to.route(id, reach),
				Output::Batch { id, changes, floor } => to.batch(id, changes, floor),
				other => rest.push(other),
			}
		}
		rest
	}

	/// The serve commands among a stream's outputs.
	fn served(outputs: Vec<Output>) -> Vec<Output> {
		(outputs.into_iter())
			.filter(|output| matches!(output, Output::Serve { .. } | Output::Unserve { .. }))
			.collect()
	}

	#[test]
	fn a_claim_is_served_through_the_stream_its_route_arrived_on() {
		let instance = Instance::random();
		let (origin, relay) = (Node::new(1, 1), Node::new(2, 2));
		let answer = origin.answer(Link::default(), Path::empty()).unwrap();
		let receive = relay.receive(Link::default(), Path::empty()).unwrap();
		origin.own(p("live/x"), Some(claim(&instance, 5)));
		pump(&answer, &receive);
		assert_eq!(
			served(outputs(&receive)),
			[Output::Serve {
				id: 1,
				named: 1,
				claim: p("live/x"),
				served: Served {
					claim: claim(&instance, 5),
					metric: 1,
					rtt: None,
					available: true,
				},
			}]
		);

		origin.own(p("live/x"), None);
		pump(&answer, &receive);
		assert_eq!(
			served(outputs(&receive)),
			[Output::Unserve {
				id: 1,
				claim: p("live/x"),
				ended: true
			}]
		);
	}

	#[test]
	fn a_claim_is_served_through_every_feasible_route_at_its_own_metric() {
		let instance = Instance::random();
		let origin = Node::new(1, 1);
		let relay = Node::new(2, 2);
		let far = relay
			.receive(
				Link {
					cost: 3,
					..Link::default()
				},
				Path::empty(),
			)
			.unwrap();
		let far_answer = origin.answer(Link::default(), Path::empty()).unwrap();
		let near = relay.receive(Link::default(), Path::empty()).unwrap();
		let near_answer = origin.answer(Link::default(), Path::empty()).unwrap();
		origin.own(p("live/x"), Some(claim(&instance, 0)));
		pump(&far_answer, &far);
		pump(&near_answer, &near);
		let metric = |outputs: Vec<Output>| match &served(outputs)[..] {
			[.., Output::Serve { served, .. }] => Some(served.metric),
			_ => None,
		};
		assert_eq!(metric(outputs(&far)), Some(3));
		assert_eq!(metric(outputs(&near)), Some(1));
		far.serving(1, &p("live/x"), &claim(&instance, 0));

		// A stream whose route ends lets go only once the other confirms it serves the claim.
		far.route(1, None);
		assert!(served(outputs(&far)).is_empty(), "nothing else serves it yet");
		near.serving(1, &p("live/x"), &claim(&instance, 0));
		assert_eq!(
			served(outputs(&far)),
			[Output::Unserve {
				id: 1,
				claim: p("live/x"),
				ended: false
			}]
		);
		assert!(served(outputs(&near)).is_empty(), "the remaining route serves on");
	}

	/// Two streams reach one origin node for different prefixes: each claim is served
	/// only through the streams that asked for it, however cheap the other.
	#[test]
	fn a_claim_is_served_only_through_streams_that_carry_it() {
		let instance = Instance::random();
		let origin = Node::new(1, 1);
		let relay = Node::new(2, 2);
		let a = relay.receive(Link::default(), p("live/a")).unwrap();
		let a_answer = origin.answer(Link::default(), p("live/a")).unwrap();
		let costly = Link {
			cost: 3,
			..Link::default()
		};
		let b = relay.receive(costly, p("live/b")).unwrap();
		let b_answer = origin.answer(Link::default(), p("live/b")).unwrap();
		origin.own(p("live/a/x"), Some(claim(&instance, 0)));
		origin.own(p("live/b/y"), Some(claim(&instance, 0)));
		pump(&a_answer, &a);
		pump(&b_answer, &b);
		let claims = |outputs: Vec<Output>| -> Vec<Path> {
			(served(outputs).into_iter())
				.filter_map(|output| match output {
					Output::Serve { claim, .. } => Some(claim),
					_ => None,
				})
				.collect()
		};
		assert_eq!(claims(outputs(&a)), [p("live/a/x")]);
		assert_eq!(claims(outputs(&b)), [p("live/b/y")]);
	}

	/// A stalled stream reads a job's Serve before the Unserve of the claim it
	/// continues, even once the job changed again meanwhile, so a reader resolved
	/// through the claim finds the job's route before the claim's goes.
	#[test]
	fn a_stalled_stream_reads_a_job_before_its_claim_goes() {
		let instance = Instance::random();
		let origin = Node::new(1, 1);
		let relay = Node::new(2, 2);
		let fast = relay.receive(Link::default(), Path::empty()).unwrap();
		let stalled = relay.receive(Link::default(), Path::empty()).unwrap();
		let fast_answer = origin.answer(Link::default(), Path::empty()).unwrap();
		let stalled_answer = origin.answer(Link::default(), Path::empty()).unwrap();
		origin.own(p("live"), Some(claim(&instance, 0)));
		pump(&fast_answer, &fast);
		pump(&stalled_answer, &stalled);
		outputs(&fast);
		outputs(&stalled);

		origin.own(p("live/x"), Some(claim(&instance, 0)));
		origin.own(p("live"), None);
		pump(&fast_answer, &fast);
		origin.own(p("live/x"), Some(claim(&instance, 5)));
		pump(&fast_answer, &fast);
		let read: Vec<(Path, Option<u64>)> = (served(outputs(&stalled)).into_iter())
			.map(|output| match output {
				Output::Serve { claim, served, .. } => (claim, Some(served.claim.cost)),
				Output::Unserve { claim, .. } => (claim, None),
				other => panic!("not a serving change: {other:?}"),
			})
			.collect();
		assert_eq!(read, [(p("live/x"), Some(5)), (p("live"), None)]);
	}

	/// A job and its claim's restart arriving together are read job first, even past a
	/// change to the claim still queued, so a reader resolved through the claim's old
	/// instance finds the job's route before the claim's moves on.
	#[test]
	fn a_job_is_read_before_its_claims_restart() {
		let (old, new) = (Instance::random(), Instance::random());
		let origin = Node::new(1, 1);
		let relay = Node::new(2, 2);
		let fast = relay.receive(Link::default(), Path::empty()).unwrap();
		let stalled = relay.receive(Link::default(), Path::empty()).unwrap();
		let fast_answer = origin.answer(Link::default(), Path::empty()).unwrap();
		let stalled_answer = origin.answer(Link::default(), Path::empty()).unwrap();
		origin.own(p("live"), Some(claim(&old, 0)));
		pump(&fast_answer, &fast);
		pump(&stalled_answer, &stalled);
		outputs(&fast);
		outputs(&stalled);

		// A change to the claim the stalled stream leaves queued.
		origin.own(p("live"), Some(claim(&old, 1)));
		pump(&fast_answer, &fast);
		pump(&stalled_answer, &stalled);
		outputs(&fast);

		origin.own(p("live/x"), Some(claim(&old, 0)));
		origin.own(p("live"), Some(claim(&new, 0)));
		pump(&fast_answer, &fast);
		pump(&stalled_answer, &stalled);
		for stream in [&fast, &stalled] {
			let read: Vec<(Path, Instance)> = (served(outputs(stream)).into_iter())
				.map(|output| match output {
					Output::Serve { claim, served, .. } => (claim, served.claim.instance),
					other => panic!("not a serve: {other:?}"),
				})
				.collect();
			assert_eq!(read, [(p("live/x"), old.clone()), (p("live"), new.clone())]);
		}
	}

	/// A claim of a scoped peer left with no usable route stays where it is served,
	/// under the name that peer gave its identity, so the requests it serves meanwhile
	/// still name it as the peer does.
	#[test]
	fn a_claim_without_a_route_keeps_the_peers_name() {
		let instance = Instance::random();
		let origin = Node::new(1, 1);
		let relay = Node::new(2, 2);
		let scoped = || Link {
			scope: Some(super::super::Scope {
				regions: vec![Path::empty()],
				qualify: super::super::qualifier([7; 32]),
			}),
			..Link::default()
		};
		let first = relay.receive(scoped(), Path::empty()).unwrap();
		let second = relay.receive(scoped(), Path::empty()).unwrap();
		let first_answer = origin.answer(Link::default(), Path::empty()).unwrap();
		let second_answer = origin.answer(Link::default(), Path::empty()).unwrap();
		origin.own(p("live/x"), Some(claim(&instance, 0)));
		pump(&first_answer, &first);
		pump(&second_answer, &second);
		second.route(1, Some(Reach::Held));
		let names = |outputs: Vec<Output>| -> Vec<(Id, bool)> {
			(served(outputs).into_iter())
				.filter_map(|output| match output {
					Output::Serve { named, served, .. } => Some((named, served.available)),
					_ => None,
				})
				.collect()
		};
		assert_eq!(names(outputs(&first)), [(1, true)]);

		// The second still holds a route it may recover through.
		first.route(1, None);
		assert_eq!(names(outputs(&first)), [(1, false)]);
	}

	/// A stream whose reader stalls holds one serving change per claim, however often
	/// the claim changes through another stream meanwhile.
	#[test]
	fn a_stalled_stream_holds_one_serve_per_claim() {
		let instance = Instance::random();
		let origin = Node::new(1, 1);
		let relay = Node::new(2, 2);
		let fast = relay.receive(Link::default(), Path::empty()).unwrap();
		let stalled = relay.receive(Link::default(), Path::empty()).unwrap();
		let fast_answer = origin.answer(Link::default(), Path::empty()).unwrap();
		let stalled_answer = origin.answer(Link::default(), Path::empty()).unwrap();
		origin.own(p("live/x"), Some(claim(&instance, 0)));
		pump(&fast_answer, &fast);
		pump(&stalled_answer, &stalled);
		for cost in 1..1000 {
			origin.own(p("live/x"), Some(claim(&instance, cost)));
			pump(&fast_answer, &fast);
			outputs(&fast);
		}
		pump(&stalled_answer, &stalled);
		let read = served(outputs(&stalled));
		assert_eq!(read.len(), 1, "{read:?}");
		assert!(
			matches!(&read[0], Output::Serve { served, .. } if served.claim.cost == 999),
			"{read:?}"
		);
	}

	/// A reader that stops reading holds one entry for the route, however often its
	/// claims change: the last change to each, and none for a claim it was never sent.
	#[test]
	fn a_stalled_reader_holds_the_net_change() {
		let instance = Instance::random();
		let origin = Node::new(1, 1);
		let answer = origin.answer(Link::default(), Path::empty()).unwrap();
		outputs(&answer);
		for cost in 0..1000 {
			origin.own(p("live/x"), Some(claim(&instance, cost)));
			origin.own(p("live/churn"), Some(claim(&instance, cost)));
			origin.own(p("live/churn"), None);
		}
		let read = outputs(&answer);
		let batch = |read: &[Output]| match read {
			[.., Output::Batch { changes, .. }] => changes.clone(),
			_ => panic!("no batch: {read:?}"),
		};
		assert!(read.len() <= 2, "{read:?}");
		assert_eq!(batch(&read), [(p("live/x"), Some(claim(&instance, 999)))]);

		// Once sent, a claim's end is kept, behind what started.
		for cost in 0..1000 {
			origin.own(p("live/y"), Some(claim(&instance, cost)));
		}
		origin.own(p("live/x"), None);
		let read = outputs(&answer);
		assert_eq!(read.len(), 1, "{read:?}");
		assert_eq!(
			batch(&read),
			[(p("live/y"), Some(claim(&instance, 999))), (p("live/x"), None)]
		);
	}

	/// Routes that start and end while the reader stalls leave nothing for it, and the
	/// router is acked at once as the reader would: churning fresh identities holds
	/// nothing. A route the reader holds ends once, however often it restarted unread.
	#[test]
	fn routes_ended_unread_leave_nothing() {
		let instance = Instance::random();
		let relay = Node::new(2, 2);
		let edge = relay
			.answer(
				Link {
					proxy: true,
					..Link::default()
				},
				Path::empty(),
			)
			.unwrap();
		outputs(&edge);
		let advert = |seqno| {
			Some(Reach::Finite(Advert {
				seqno,
				metric: 0,
				down_only: false,
				rtt: None,
				hops: [].into(),
			}))
		};
		// An identity that stays, after the churned ones in the node's order.
		let standing = relay.receive(Link::default(), Path::empty()).unwrap();
		standing.route(200, advert(0));
		standing.batch(200, vec![(p("live/s"), Some(claim(&instance, 0)))], Some(1));
		standing.serving(200, &p("live/s"), &claim(&instance, 0));
		outputs(&edge);
		let kept = relay.retained();
		for origin in 10..110 {
			let receive = relay.receive(Link::default(), Path::empty()).unwrap();
			receive.route(origin, advert(0));
			receive.batch(origin, vec![(p("live/x"), Some(claim(&instance, 0)))], Some(1));
			drop(receive);
		}
		let of = |read: Vec<Output>, of: fn(Id) -> bool| -> Vec<Output> {
			(read.into_iter())
				.filter(|output| matches!(output, Output::Route { id, .. } | Output::Batch { id, .. } if of(*id)))
				.collect()
		};
		let read = of(outputs(&edge), |id| (10..110).contains(&id));
		assert_eq!(read, [], "the churned routes");
		assert_eq!(relay.retained(), kept, "nothing of the churned identities is kept");

		// The reader holds a route, which ends, restarts, and ends again unread.
		let receive = relay.receive(Link::default(), Path::empty()).unwrap();
		receive.route(7, advert(0));
		receive.batch(7, vec![(p("live/x"), Some(claim(&instance, 0)))], Some(1));
		assert_eq!(of(outputs(&edge), |id| id == 7).len(), 2, "the route and its batch");
		receive.route(7, None);
		receive.route(7, advert(1));
		receive.batch(7, vec![(p("live/x"), Some(claim(&instance, 0)))], Some(2));
		receive.route(7, None);
		let read = of(outputs(&edge), |id| id == 7);
		assert_eq!(read, [Output::Route { id: 7, reach: None }]);
		assert_eq!(
			relay.retained().echoes,
			1,
			"only the end the reader reads awaits its echo"
		);
		edge.echo(7);
		assert_eq!(relay.retained().echoes, 0);
	}

	/// A route goes on a stream only while it has a claim the stream's interest
	/// meets. Once the last one is withdrawn, the reader reads the floor certifying
	/// the withdrawal before the route's end, even when both queued unread.
	#[test]
	fn a_route_ends_after_the_floor_certifying_its_last_claim() {
		let instance = Instance::random();
		let origin = Node::new(1, 1);
		let edge = origin.answer(Link::default(), Path::empty()).unwrap();
		let other = origin.answer(Link::default(), p("other")).unwrap();
		assert_eq!(outputs(&edge), [], "no claim, no route");

		origin.own(p("live/x"), Some(claim(&instance, 0)));
		assert_eq!(outputs(&edge).len(), 2, "the route, then its claim");
		assert_eq!(outputs(&other), [], "the interest meets no claim");

		origin.own(p("live/x"), None);
		assert_eq!(
			outputs(&edge),
			[
				Output::Batch {
					id: 1,
					changes: vec![(p("live/x"), None)],
					floor: Some(2),
				},
				Output::Route { id: 1, reach: None },
			]
		);
	}

	/// A peer that withholds ROUTE_END echoes holds one end per identity however
	/// often its only claim flaps: the route starts again only once the end is echoed.
	#[test]
	fn a_route_restarts_only_once_its_end_is_echoed() {
		let instance = Instance::random();
		let origin = Node::new(1, 1);
		let edge = origin.answer(Link::default(), Path::empty()).unwrap();
		origin.own(p("live/x"), Some(claim(&instance, 0)));
		outputs(&edge);
		// The peer reads everything, and echoes nothing.
		let mut ends = 0;
		for _ in 0..100 {
			origin.own(p("live/x"), None);
			origin.own(p("live/x"), Some(claim(&instance, 0)));
			ends += (outputs(&edge).into_iter())
				.filter(|output| matches!(output, Output::Route { reach: None, .. }))
				.count();
		}
		assert_eq!(ends, 1);
		assert_eq!(origin.retained().echoes, 1);

		edge.echo(1);
		let read = outputs(&edge);
		assert!(matches!(read[0], Output::Route { id: 1, reach: Some(_) }), "{read:?}");
		assert_eq!(read.len(), 2, "the route, then its claim");
	}

	/// A relay sends another stream a claim only once a stream serving it confirmed
	/// its instance, so a request naming that instance always finds a route: first when
	/// the claim starts, again when it restarts at a new instance, and also where a
	/// narrower interest is shown the claim covering it.
	#[test]
	fn a_claim_goes_on_only_once_served() {
		let (old, new) = (Instance::random(), Instance::random());
		let origin = Node::new(1, 1);
		let relay = Node::new(2, 2);
		let receive = relay.receive(Link::default(), Path::empty()).unwrap();
		let answer = origin.answer(Link::default(), Path::empty()).unwrap();
		let edge = relay.answer(Link::default(), Path::empty()).unwrap();
		let narrow = relay.answer(Link::default(), p("live/x")).unwrap();
		let both = || [instances(&edge), instances(&narrow)];

		origin.own(p("live"), Some(claim(&old, 0)));
		pump(&answer, &receive);
		assert_eq!(both(), [[(vec![], None)], [(vec![], None)]], "held until served");
		receive.serving(1, &p("live"), &claim(&old, 0));
		assert_eq!(
			both(),
			[
				[(vec![(p("live"), Some(old.clone()))], Some(1))],
				[(vec![(p("live/x"), Some(old.clone()))], Some(1))]
			]
		);

		origin.own(p("live"), Some(claim(&new, 0)));
		pump(&answer, &receive);
		assert_eq!(
			both(),
			[[(vec![], None)], [(vec![], None)]],
			"the old instance stays until the new one is served, the floor certifying nothing"
		);
		receive.serving(1, &p("live"), &claim(&old, 0));
		assert_eq!(both(), [[], []], "a stale confirmation releases nothing");
		receive.serving(1, &p("live"), &claim(&new, 0));
		assert_eq!(
			both(),
			[
				[(vec![(p("live"), Some(new.clone()))], Some(2))],
				[(vec![(p("live/x"), Some(new.clone()))], Some(2))]
			]
		);

		// A route that ends with a claim held back certifies nothing on its way out.
		origin.own(p("live"), Some(claim(&old, 1)));
		pump(&answer, &receive);
		receive.route(1, None);
		assert!(matches!(
			&outputs(&edge)[..],
			[Output::Batch { floor: None, .. }, Output::Route { reach: None, .. }]
		));
	}

	/// A claim presented at a narrower interest waits on the shown claim covering it,
	/// whichever that is by the time it is confirmed: a confirmed one that covers it
	/// again once the claim between goes, and one that moves to a narrower path before
	/// it is confirmed.
	#[test]
	fn a_held_claim_follows_the_claim_covering_it() {
		let (first, second) = (Instance::random(), Instance::random());
		let origin = Node::new(1, 1);
		let relay = Node::new(2, 2);
		let receive = relay.receive(Link::default(), Path::empty()).unwrap();
		let answer = origin.answer(Link::default(), Path::empty()).unwrap();
		origin.own(p("live"), Some(claim(&first, 0)));
		pump(&answer, &receive);
		receive.serving(1, &p("live"), &claim(&first, 0));
		origin.own(p("live/x"), Some(claim(&first, 0)));
		pump(&answer, &receive);
		let narrow = relay.answer(Link::default(), p("live/x/y")).unwrap();
		assert_eq!(instances(&narrow), [(vec![], None)], "held on the unconfirmed live/x");
		origin.own(p("live/x"), None);
		pump(&answer, &receive);
		assert_eq!(
			instances(&narrow),
			[(vec![(p("live/x/y"), Some(first.clone()))], Some(3))],
			"covered by the confirmed live again"
		);

		origin.own(p("live"), Some(claim(&second, 0)));
		pump(&answer, &receive);
		assert_eq!(instances(&narrow), [(vec![], None)]);
		origin.own(p("live/x"), Some(claim(&second, 0)));
		origin.own(p("live"), None);
		pump(&answer, &receive);
		assert_eq!(
			instances(&narrow),
			[(vec![], None)],
			"now held on the unconfirmed live/x"
		);
		receive.serving(1, &p("live/x"), &claim(&second, 0));
		assert_eq!(
			instances(&narrow),
			[(vec![(p("live/x/y"), Some(second.clone()))], Some(6))]
		);
	}

	/// A batch's changes, each claim by its instance, and its floor.
	type Instances = (Vec<(Path, Option<Instance>)>, Option<Rev>);

	/// The batches for identity 1 a stream would send.
	fn instances(stream: &Stream) -> Vec<Instances> {
		(outputs(stream).into_iter())
			.filter_map(|output| match output {
				Output::Batch { id: 1, changes, floor } => {
					let changes = (changes.into_iter())
						.map(|(path, claim)| (path, claim.map(|claim| claim.instance)))
						.collect();
					Some((changes, floor))
				}
				_ => None,
			})
			.collect()
	}

	/// The seqnos of the finite ROUTE messages for `id` a stream would send.
	fn routes(stream: &Stream, id: Id) -> Vec<Seqno> {
		(outputs(stream).into_iter())
			.filter_map(|output| match output {
				Output::Route { id: to, reach } if to == id => match reach {
					Some(Reach::Finite(advert)) => Some(advert.seqno),
					reach => panic!("unexpected reach {reach:?}"),
				},
				_ => None,
			})
			.collect()
	}

	fn finite(seqno: Seqno, metric: Metric) -> Option<Reach> {
		Some(Reach::Finite(Advert {
			seqno,
			metric,
			down_only: false,
			rtt: None,
			hops: [].into(),
		}))
	}

	/// A route that already crossed as many nodes as one may is not forwarded, since this
	/// node's hop would take it past what any peer can decode, so a session never fails to
	/// encode one. One hop fewer forwards at exactly the limit.
	#[test]
	fn a_route_at_the_hop_limit_is_not_forwarded() {
		use crate::route::MAX_HOPS;
		for (hops, forwarded) in [(MAX_HOPS - 1, Some(MAX_HOPS)), (MAX_HOPS, None)] {
			let relay = Node::new(2, 2);
			let to_b = relay.answer(Link::default(), Path::empty()).unwrap();
			let from_a = relay.receive(Link::default(), Path::empty()).unwrap();
			from_a.route(
				5,
				Some(Reach::Finite(Advert {
					seqno: 1,
					metric: 1,
					down_only: false,
					rtt: None,
					hops: (100..100 + hops as u64).collect(),
				})),
			);
			// A route goes on the wire only with a claim it shows, once that is served.
			let live = claim(&Instance::random(), 0);
			from_a.batch(5, vec![(p("live"), Some(live.clone()))], Some(0));
			from_a.serving(5, &p("live"), &live);
			let sent: Vec<_> = (outputs(&to_b).into_iter())
				.filter_map(|output| match output {
					Output::Route {
						id: 5,
						reach: Some(Reach::Finite(advert)),
					} => Some(advert),
					_ => None,
				})
				.collect();
			assert_eq!(
				sent.iter().map(|advert| advert.hops.len()).max(),
				forwarded,
				"{hops} hops"
			);
			for advert in sent {
				let start = crate::lite::AnnounceBroadcast::RouteStart { node: 5, advert };
				crate::coding::Encode::encode_bytes(&start, crate::lite::Version::Lite07).unwrap();
			}
		}
	}

	/// A relay's own seqno advances only when a session over which another node
	/// learned a path to it closes: a client that forwards nothing costs no ROUTE.
	#[test]
	fn a_relay_bumps_only_when_a_route_link_closes() {
		let relay = Node::new(2, 2);
		relay.own(p("live/r"), Some(claim(&Instance::random(), 0)));
		let forwarding = Link::default();
		// Two peer relays, each forwarding a route of a third node.
		let to_a = relay.answer(forwarding.clone(), Path::empty()).unwrap();
		let from_a = relay.receive(Link::default(), Path::empty()).unwrap();
		let to_b = relay.answer(forwarding.clone(), Path::empty()).unwrap();
		let from_b = relay.receive(Link::default(), Path::empty()).unwrap();
		from_a.route(5, finite(0, 1));
		from_b.route(6, finite(0, 1));
		assert_eq!(routes(&to_b, 2), [0]);
		outputs(&to_a);

		// A client sending only its own route connects and leaves.
		let to_client = relay.answer(forwarding, Path::empty()).unwrap();
		let from_client = relay.receive(Link::default(), Path::empty()).unwrap();
		from_client.route(9, finite(0, 0));
		drop((to_client, from_client));
		assert!(routes(&to_a, 2).is_empty());
		assert!(routes(&to_b, 2).is_empty());

		// A permit change refiling a peer's stream is no link change either.
		// The refile replays no forwarded route, yet the session stays a route link.
		let refiled = relay.receive(Link::default(), Path::empty()).unwrap();
		from_a.replaced_by(&refiled);
		drop(from_a);
		assert!(routes(&to_b, 2).is_empty());

		// Losing a forwarding peer does change a path.
		drop((to_a, refiled));
		assert_eq!(routes(&to_b, 2), [1]);
		drop(from_b);
	}

	/// A node that forwards nothing advances its seqno whenever a session carrying
	/// its route closes, so moving to a farther relay needs no ROUTE_REQUEST.
	#[test]
	fn a_publisher_bumps_when_any_session_closes() {
		let publisher = Node::new(9, 9);
		publisher.own(p("live/p"), Some(claim(&Instance::random(), 0)));
		let origin = Link {
			proxy: false,
			..Link::default()
		};
		let first = publisher.answer(origin.clone(), Path::empty()).unwrap();
		let second = publisher.answer(origin, Path::empty()).unwrap();
		assert_eq!(routes(&second, 9), [0]);
		drop(first);
		assert_eq!(routes(&second, 9), [1]);

		// A session whose prefix matches no claim carries no route, so its close moves no path.
		let unmatched = publisher
			.answer(
				Link {
					proxy: false,
					..Link::default()
				},
				p("other"),
			)
			.unwrap();
		assert!(outputs(&unmatched).is_empty());
		// One whose prefix a claim meets only later, after another session closed.
		let later = publisher
			.answer(
				Link {
					proxy: false,
					..Link::default()
				},
				p("later"),
			)
			.unwrap();
		drop(unmatched);
		assert!(routes(&second, 9).is_empty());

		// It carries the route from then on, so its close does move a path.
		publisher.own(p("later/p"), Some(claim(&Instance::random(), 0)));
		assert_eq!(routes(&later, 9), [1]);
		drop(later);
		assert_eq!(routes(&second, 9), [2]);
	}

	#[test]
	fn a_route_lost_with_its_session_leaves_the_others_serving() {
		let instance = Instance::random();
		let origin = Node::new(1, 1);
		let relay = Node::new(2, 2);
		let first = relay.receive(Link::default(), Path::empty()).unwrap();
		let first_answer = origin.answer(Link::default(), Path::empty()).unwrap();
		let second = relay.receive(Link::default(), Path::empty()).unwrap();
		let second_answer = origin.answer(Link::default(), Path::empty()).unwrap();
		origin.own(p("live/x"), Some(claim(&instance, 0)));
		pump(&first_answer, &first);
		pump(&second_answer, &second);
		outputs(&first);
		assert!(matches!(served(outputs(&second))[..], [.., Output::Serve { .. }]));
		second.serving(1, &p("live/x"), &claim(&instance, 0));

		drop(first);
		assert!(served(outputs(&second)).is_empty(), "already served where it still is");
		assert!(relay.shown().contains_key(&(1, p("live/x"))));
	}

	#[test]
	fn a_held_route_keeps_serving_where_it_was_but_unavailable() {
		let instance = Instance::random();
		let relay = Node::new(2, 2);
		let receive = relay.receive(Link::default(), Path::empty()).unwrap();
		let advert = Advert {
			seqno: 0,
			metric: 0,
			down_only: false,
			rtt: None,
			hops: [].into(),
		};
		receive.route(1, Some(Reach::Finite(advert)));
		receive.batch(1, vec![(p("live/x"), Some(claim(&instance, 0)))], Some(1));
		assert!(matches!(served(outputs(&receive))[..], [Output::Serve { .. }]));
		receive.serving(1, &p("live/x"), &claim(&instance, 0));

		receive.route(1, Some(Reach::Held));
		assert!(matches!(
			&served(outputs(&receive))[..],
			[Output::Serve { served, .. }] if !served.available
		));
		receive.route(1, None);
		assert!(matches!(served(outputs(&receive))[..], [Output::Unserve { .. }]));
	}

	/// What a stream holds back leaves nothing waiting once it goes, whether withdrawn,
	/// confirmed, or closed with the stream, so nothing builds up as claims churn.
	#[test]
	fn a_held_claim_leaves_nothing_waiting_once_it_goes() {
		let instance = Instance::random();
		let origin = Node::new(1, 1);
		let relay = Node::new(2, 2);
		let receive = relay.receive(Link::default(), Path::empty()).unwrap();
		let answer = origin.answer(Link::default(), Path::empty()).unwrap();
		let waiting = || -> usize {
			(relay.lock().waiting.values())
				.flat_map(BTreeMap::values)
				.map(BTreeSet::len)
				.sum()
		};

		let edge = relay.answer(Link::default(), Path::empty()).unwrap();
		origin.own(p("live"), Some(claim(&instance, 0)));
		pump(&answer, &receive);
		assert_eq!(waiting(), 1);
		receive.serving(1, &p("live"), &claim(&instance, 0));
		assert_eq!(waiting(), 0, "confirmed");

		// The route stays up through the confirmed claim while the one below churns.
		origin.own(p("live/a"), Some(claim(&instance, 0)));
		pump(&answer, &receive);
		assert_eq!(waiting(), 1);
		origin.own(p("live/a"), None);
		pump(&answer, &receive);
		assert_eq!(waiting(), 0, "withdrawn");

		origin.own(p("live/a"), Some(claim(&instance, 0)));
		pump(&answer, &receive);
		assert_eq!(waiting(), 1);
		drop(edge);
		assert_eq!(waiting(), 0, "closed");
	}

	/// A peer's GOAWAY moves the route this node forwards onto another stream at once.
	#[test]
	fn a_goaway_moves_the_forwarded_route_to_another_stream() {
		let instance = Instance::random();
		let relay = Node::new(2, 2);
		let edge = relay.answer(Link::default(), Path::empty()).unwrap();
		let first = relay.receive(Link::default(), Path::empty()).unwrap();
		let second = relay.receive(Link::default(), Path::empty()).unwrap();
		for (stream, hop) in [(&first, 10), (&second, 20)] {
			let advert = Advert {
				seqno: 0,
				metric: 1,
				down_only: false,
				rtt: None,
				hops: [hop].into(),
			};
			stream.route(5, Some(Reach::Finite(advert)));
			stream.batch(5, vec![(p("live/x"), Some(claim(&instance, 0)))], Some(1));
		}
		let hops = |stream: &Stream| -> Vec<Vec<Id>> {
			(outputs(stream).into_iter())
				.filter_map(|output| match output {
					Output::Route {
						id: 5,
						reach: Some(Reach::Finite(advert)),
					} => Some(advert.hops.to_vec()),
					_ => None,
				})
				.collect()
		};
		assert_eq!(hops(&edge), [vec![10, 2]]);

		first.goaway();
		assert_eq!(hops(&edge), [vec![20, 2]]);
	}

	/// Streams dropped while a session's streams close disconnect together, once the
	/// last guard held over them goes.
	#[test]
	fn dropped_streams_disconnect_once_the_last_closing_guard_goes() {
		let instance = Instance::random();
		let relay = Node::new(2, 2);
		let receive = relay.receive(Link::default(), Path::empty()).unwrap();
		receive.route(5, finite(0, 0));
		receive.batch(5, vec![(p("live/x"), Some(claim(&instance, 0)))], Some(1));
		let (outer, inner) = (relay.closing(), relay.closing());
		drop(receive);
		drop(inner);
		assert!(relay.shown().contains_key(&(5, p("live/x"))), "another guard is held");
		drop(outer);
		assert!(relay.shown().is_empty());
	}

	/// The origin declares its hop id, without the qualified marker, or a random
	/// identity when nothing is left of it.
	#[test]
	fn the_origin_declares_its_hop_id_unqualified() {
		let hop = |id| std::num::NonZeroU64::new(id).unwrap();
		assert_eq!(Node::origin(hop(5)).declared(), 5);
		assert_eq!(Node::origin(hop(QUALIFIED | 5)).declared(), 5);
		let drawn = Node::origin(hop(QUALIFIED)).declared();
		assert!((1..=GENERATED).contains(&drawn), "{drawn}");
	}

	/// A declared identity travels as itself on a lite-06 wire, and each qualified key
	/// as its own hop id within the bits a generated one keeps, so a relay recognizes
	/// only its own routes coming back.
	#[test]
	fn an_identity_travels_as_its_own_hop() {
		assert_eq!(hop(5).id(), 5);
		assert_eq!(hop(GENERATED + 1).id(), GENERATED + 1);
		let hops: BTreeSet<u64> = (0..256).map(|key| hop(QUALIFIED | key).id()).collect();
		assert_eq!(hops.len(), 256, "one hop per key");
		assert!(hops.iter().all(|id| (1..=GENERATED).contains(id)), "{hops:?}");
	}
}
