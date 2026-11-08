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

use std::{
	collections::{BTreeMap, BTreeSet, HashMap, VecDeque},
	sync::{Arc, Mutex, MutexGuard},
	task::{Poll, ready},
};

use crate::{Instance, Path};

use rand::RngExt;

use super::{Carried, Error, Id, Link, Metric, Out, QUALIFIED, Reach, Rev, Router, Seqno};

/// One announce stream in one direction.
pub(crate) type Key = u64;

/// A claim's attributes on the route layer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Claim {
	pub instance: Instance,
	pub epoch: u64,
	/// The origin's Announce Cost.
	pub cost: u64,
	/// The claim of the same identity this one took over, at or above its path, and the
	/// instance that claim served: what was resolved through it beneath this path is
	/// this claim's broadcast. Kept while the instance stays, and sent as
	/// ANNOUNCE_TAKEOVER to a peer still holding that claim.
	pub continues: Option<(Path, Instance)>,
	/// This node's own claim, served through a route learned from an upstream peer: it
	/// is never sent to another upstream, so this node never carries traffic between
	/// two. Always unset on a claim off the wire.
	pub upstream: bool,
	/// This node's own claim, served through a route another node announced over an
	/// older version: sent only to a peer this node proxies for, so a node that does
	/// not proxy is never transit. Always unset on a claim off the wire.
	pub learned: bool,
}

/// A shown claim as the origin serves it through the selected route.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Served {
	pub claim: Claim,
	/// The selected route's metric: a path through the claim costs the Announce
	/// Cost plus this.
	pub metric: Metric,
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
	/// Stop serving `claim` of `id` through this stream.
	Unserve { id: Id, claim: Path },
}

/// The identities a node routes to over lite-07, by the lite-06 hop each travels
/// as, and what their usable routes can carry, so its older-version sessions
/// recognize a hop chain that names one.
#[derive(Debug, Default)]
pub(crate) struct Known {
	hops: HashMap<crate::Hop, Vec<Carried>>,
	/// Bumped on every change, for a reader to tell when to look again.
	epoch: u64,
}

impl Known {
	/// Whether `hops` names an identity with a usable lite-07 route that carries a
	/// claim at the absolute `path`.
	pub fn names(&self, hops: &crate::Hops, path: &Path) -> bool {
		(hops.iter().filter_map(|hop| self.hops.get(hop)))
			.any(|carried| carried.iter().any(|route| route.carries(path)))
	}

	pub fn epoch(&self) -> u64 {
		self.epoch
	}
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
	/// Each claim's last Serve or Unserve, read after the rest, in queue order. A
	/// takeover queues its successor's Serve before its predecessor's Unserve, and the
	/// session needs the predecessor still served to carry it on: so a Serve replacing
	/// a queued Serve keeps its place, while an Unserve, or a Serve after one, goes last.
	serves: BTreeMap<u64, Output>,
	/// Where each claim's last change sits in `serves`.
	serving: HashMap<(Id, Path), u64>,
	/// Each route the reader holds, and the claims it was sent under it.
	delivered: HashMap<Id, BTreeSet<Path>>,
	/// ROUTE_REQUESTs to send, by the identity as the peer advertised it. A higher
	/// seqno subsumes a lower one, so a writer that stalls holds one per identity.
	requests: BTreeMap<Id, Seqno>,
}

/// One identity's route changes and claim batches since the reader last took it.
#[derive(Default)]
struct Pending {
	/// A ROUTE_END goes first, ending the route the reader holds and its claims.
	ended: bool,
	/// The last finite reach since, which starts the route if the reader holds none.
	finite: Option<Reach>,
	/// The route was held after that.
	held: bool,
	/// The batches since, merged: each claim's last change, and the last floor.
	batch: Option<(Changes, Option<Rev>)>,
}

/// Each claim's last change, by path.
type Changes = BTreeMap<Path, Option<Claim>>;

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

	/// Queue `output`. Returns the identity of a ROUTE_END the reader will never read,
	/// since it never read the route it ends, for the caller to ack in its place.
	fn push(&mut self, output: Output) -> Option<Id> {
		match output {
			Output::Route { id, reach: None } => {
				let holds = self.delivered.contains_key(&id);
				let entry = self.entry(id);
				// The reader holds the route once it reads what is pending: an end of
				// that keeps any end before, an end of a start it never read is acked.
				let ended = entry.ended;
				let acked = !holds || ended;
				*entry = Pending {
					ended: holds,
					..Pending::default()
				};
				if !holds && let Some((seq, _)) = self.pending.remove(&id) {
					self.order.remove(&seq);
				}
				acked.then_some(id)
			}
			Output::Route {
				id,
				reach: Some(Reach::Held),
			} => {
				self.entry(id).held = true;
				None
			}
			Output::Route {
				id,
				reach: Some(finite),
			} => {
				let entry = self.entry(id);
				entry.finite = Some(finite);
				entry.held = false;
				None
			}
			Output::Batch { id, changes, floor } => {
				let ended = self.entry(id).ended;
				let (_, entry) = self.pending.get_mut(&id).expect("just queued");
				let sent = self.delivered.get(&id);
				let (merged, last) = entry.batch.get_or_insert_default();
				for (path, claim) in changes {
					// The reader holds nothing here once the entry reaches it.
					let unsent = ended || sent.is_none_or(|sent| !sent.contains(&path));
					match claim {
						None if unsent => merged.remove(&path),
						claim => merged.insert(path, claim),
					};
				}
				*last = floor;
				None
			}
			// Each serving change replaces the last: the session acts on where the claim
			// ends up, in place when it keeps its instance.
			Output::Serve { id, ref claim, .. } | Output::Unserve { id, ref claim } => {
				let key = (id, claim.clone());
				if let Some(&seq) = self.serving.get(&key)
					&& matches!(output, Output::Serve { .. })
					&& matches!(self.serves.get(&seq), Some(Output::Serve { .. }))
				{
					self.serves.insert(seq, output);
					return None;
				}
				let seq = self.next;
				self.next += 1;
				if let Some(old) = self.serving.insert(key, seq) {
					self.serves.remove(&old);
				}
				self.serves.insert(seq, output);
				None
			}
			other => {
				self.ready.push_back(other);
				None
			}
		}
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
					if let Output::Serve { id, claim, .. } | Output::Unserve { id, claim } = &output {
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
			// As the router orders a batch: what starts or changes, then what ends.
			let (ends, starts): (Vec<_>, Vec<_>) = merged.into_iter().partition(|(_, claim)| claim.is_none());
			if let Some(sent) = self.delivered.get_mut(&id) {
				for (path, _) in &starts {
					sent.insert(path.clone());
				}
				for (path, _) in &ends {
					sent.remove(path);
				}
			}
			let changes = starts.into_iter().chain(ends).collect();
			self.ready.push_back(Output::Batch { id, changes, floor });
		}
	}
}

struct State {
	router: Router<Key, Claim>,
	next_key: Key,
	queues: Queues,
	/// Each shown claim of another identity, and the streams serving it.
	homes: BTreeMap<(Id, Path), Home>,
	/// The other identities with a usable route, and what those routes carry.
	routed: BTreeMap<Id, Vec<Carried>>,
	known: kio::Shared<Known>,
}

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
	confirmed: bool,
	/// No longer a route to the claim's identity: let go once a wanted stream confirms.
	leaving: bool,
}

impl Home {
	/// Unserve every leaving stream once a wanted one serves the claim.
	fn release(&mut self, queues: &Queues, id: Id, claim: &Path) {
		if !self.keys.values().any(|slot| !slot.leaving && slot.confirmed) {
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
					},
				);
			}
			!slot.leaving
		});
	}
}

impl Node {
	/// A node declaring a fresh identity, `seed` keying its rotation.
	#[cfg(test)]
	pub fn new(id: Id, seed: u64) -> Self {
		Self::publishing(id, seed, Default::default())
	}

	/// A node declaring a fresh identity that keeps `known` current.
	fn publishing(id: Id, seed: u64, known: kio::Shared<Known>) -> Self {
		debug_assert!(
			id != 0 && id & QUALIFIED == 0,
			"a declared identity is non-zero and unqualified"
		);
		let mut router = Router::new(seed);
		router.originate(id, 0);
		Self {
			state: Arc::new(Mutex::new(State {
				router,
				next_key: 1,
				queues: HashMap::new(),
				homes: BTreeMap::new(),
				routed: BTreeMap::new(),
				known,
			})),
		}
	}

	/// A node with a random identity, keeping `known` current.
	pub fn random(known: kio::Shared<Known>) -> Self {
		let mut rng = kio::clock::rng();
		let id = rng.random_range(1..QUALIFIED);
		Self::publishing(id, rng.random(), known)
	}

	fn lock(&self) -> MutexGuard<'_, State> {
		self.state.lock().unwrap_or_else(|poison| poison.into_inner())
	}

	/// The identity this node declares, which a rotation replaces.
	pub fn declared(&self) -> Id {
		self.lock()
			.router
			.declared()
			.expect("a node always declares an identity")
	}

	/// This node's own claim at `path` changed: `None` withdraws it. A claim taking over
	/// the own claim at `predecessor` continues the instance that one serves now.
	pub fn own(&self, path: Path, claim: Option<Claim>, predecessor: Option<&Path>) {
		let mut state = self.lock();
		let changed = match claim {
			Some(mut claim) => {
				let view = state.router.declared().and_then(|id| state.router.view(id));
				let current = view.and_then(|view| view.items.get(&path));
				claim.continues = match predecessor {
					Some(predecessor) => (view.and_then(|view| view.items.get(predecessor)))
						.map(|taken| (predecessor.clone(), taken.instance.clone())),
					// A re-price or a newer epoch of the same instance continues what it did.
					None => current
						.filter(|current| current.instance == claim.instance)
						.and_then(|current| current.continues.clone()),
				};
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
		self.register(link, |router, key| router.asks(key, prefix))
	}

	/// Register a stream answering the peer's request for `prefix`.
	pub fn answer(&self, link: Link, prefix: Path) -> Result<Stream, Error> {
		self.register(link, |router, key| router.peer_interest(key, prefix))
	}

	fn register(
		&self,
		link: Link,
		open: impl FnOnce(&mut Router<Key, Claim>, Key) -> Result<(), Error>,
	) -> Result<Stream, Error> {
		let mut state = self.lock();
		let key = state.next_key;
		state.next_key += 1;
		state.router.connect(key, link)?;
		let queue = kio::Shared::default();
		state.queues.insert(key, queue.clone());
		if let Err(err) = open(&mut state.router, key) {
			state.router.disconnect(key);
			state.queues.remove(&key);
			return Err(err);
		}
		// The node's own set of sessions changed.
		let _ = state.router.bump();
		state.flush();
		Ok(Stream {
			node: self.clone(),
			key,
			queue,
		})
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
		self.with(|router, key| router.route(key, named, reach));
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

	/// This stream's session now serves `claim` of `id`, so a stream that no longer
	/// carries a route to it lets go.
	pub fn serving(&self, id: Id, claim: &Path) {
		let mut state = self.node.lock();
		let State { homes, queues, .. } = &mut *state;
		let Some(home) = homes.get_mut(&(id, claim.clone())) else {
			return;
		};
		let Some(slot) = home.keys.get_mut(&self.key) else {
			return;
		};
		if slot.confirmed {
			return;
		}
		slot.confirmed = true;
		home.release(queues, id, claim);
	}
}

impl Drop for Stream {
	fn drop(&mut self) {
		let mut state = self.node.lock();
		state.queues.remove(&self.key);
		state.router.disconnect(self.key);
		// Its session is gone, and what it served with it.
		for home in state.homes.values_mut() {
			home.keys.remove(&self.key);
		}
		let _ = state.router.bump();
		state.flush();
	}
}

/// Queue `output` for the stream `key`. Returns the identity of a ROUTE_END its
/// reader will never read, whose echo the router is owed.
fn push(queues: &Queues, key: Key, output: Output) -> Option<Id> {
	queues.get(&key).and_then(|queue| queue.lock().push(output))
}

impl State {
	/// Route the router's messages and shown claims to the streams.
	fn flush(&mut self) {
		loop {
			let mut acked = Vec::new();
			for out in self.router.drain() {
				let (key, output) = match out {
					Out::Route { to, id, reach } => (to, Output::Route { id, reach }),
					Out::Batch { to, id, changes, floor } => (to, Output::Batch { id, changes, floor }),
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
				if let Some(id) = push(&self.queues, key, output) {
					acked.push((key, id));
				}
			}
			if acked.is_empty() {
				break;
			}
			// A ROUTE_END coalesced away before its reader read the route: no peer holds it.
			for (key, id) in acked {
				self.router.echo(key, id);
			}
		}
		// The shown claims below are read whole; the events only say which changed.
		self.router.events();
		let changed = self.router.changed();
		let declared = self.router.declared();
		let mut known = false;
		for id in changed {
			if Some(id) == declared {
				continue;
			}
			let carried = self.router.carried(id);
			known |= match carried.is_empty() {
				true => self.routed.remove(&id).is_some(),
				false => self.routed.insert(id, carried.clone()).as_ref() != Some(&carried),
			};
			self.home(id);
		}
		if known {
			let mut published = self.known.lock();
			published.hops.clear();
			for (id, carried) in &self.routed {
				published
					.hops
					.entry(hop(*id))
					.or_default()
					.extend(carried.iter().cloned());
			}
			published.epoch += 1;
		}
	}

	/// Serve each shown claim of `id` through every stream with a usable feasible
	/// route to it that can carry the claim, keeping it where it is, unavailable, while
	/// there is none.
	fn home(&mut self, id: Id) {
		let claims: Vec<(Path, Claim, bool)> = (self.router.shown().range((id, Path::empty())..))
			.take_while(|((shown, _), _)| *shown == id)
			.map(|((_, claim), shown)| (claim.clone(), shown.item.clone(), shown.available))
			.collect();
		let mut gone: BTreeSet<Path> = (self.homes.range((id, Path::empty())..))
			.take_while(|((home, _), _)| *home == id)
			.map(|((_, claim), _)| claim.clone())
			.collect();
		for (claim, item, available) in claims {
			gone.remove(&claim);
			let routes = self.router.routes(id, &claim);
			let home = self.homes.entry((id, claim.clone())).or_default();
			// With no route left, the router no longer knows the name a stream's peer
			// gave the identity, so it keeps the one it was served under.
			let wanted: Vec<(Key, Metric, bool, Id)> = match routes.is_empty() {
				true => (home.keys.iter())
					.map(|(key, slot)| (*key, slot.served.metric, false, slot.named))
					.collect(),
				false => (routes.iter())
					.map(|&(key, metric)| (key, metric, available, self.router.name(key, id)))
					.collect(),
			};
			for slot in home.keys.values_mut() {
				slot.leaving = true;
			}
			for (key, metric, available, named) in wanted {
				let served = Served {
					claim: item.clone(),
					metric,
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
							confirmed: false,
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
			for key in home.keys.into_keys() {
				push(
					&self.queues,
					key,
					Output::Unserve {
						id,
						claim: claim.clone(),
					},
				);
			}
		}
		// Nothing left to recover: let the identity go.
		let shows =
			(self.router.shown().range((id, Path::empty())..).next()).is_some_and(|((shown, _), _)| *shown == id);
		if !self.router.holds(id) && !shows {
			self.router.collect(id);
		}
	}
}

/// The lite-06 hop a router identity travels as, so a relay translating its routes
/// for a lite-06 peer recognizes them coming back.
pub(crate) fn hop(id: Id) -> crate::Hop {
	// FNV-1a over the identity, folded into the 53 bits a hop id keeps.
	let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
	for byte in id.to_le_bytes() {
		hash ^= u64::from(byte);
		hash = hash.wrapping_mul(0x0100_0000_01b3);
	}
	let id = (hash & ((1 << 53) - 1)).max(1);
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
			continues: None,
			upstream: false,
			learned: false,
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

	/// What a node publishes for its older-version sessions: the route it holds to
	/// another identity, by that identity's lite-06 hop, only where that route carries
	/// a claim. A withdrawn claim leaves the route, and losing the route clears it.
	#[test]
	fn known_names_a_routed_identity_where_its_route_carries() {
		let known = kio::Shared::<Known>::default();
		let (origin, relay) = (Node::new(1, 1), Node::publishing(2, 2, known.clone()));
		let answer = origin.answer(Link::default(), p("live")).unwrap();
		let receive = relay.receive(Link::default(), p("live")).unwrap();
		origin.own(p("live/x"), Some(claim(&Instance::random(), 5)), None);
		pump(&answer, &receive);
		let mut chain = crate::Hops::new();
		chain.push(hop(1)).unwrap();
		let names = |path| known.read().names(&chain, &p(path));
		assert!(names("live/x"));
		assert!(names("live/y"), "a route reaches the node, not each of its claims");
		assert!(!names("other/x"), "nothing asked of the peer there");

		origin.own(p("live/x"), None, None);
		pump(&answer, &receive);
		assert!(names("live/x"), "a withdrawal keeps the route");

		let epoch = known.read().epoch();
		drop(receive);
		assert!(!names("live/x"));
		assert!(known.read().epoch() > epoch);
	}

	/// A scoped neighbor serves only what its permit admits, which its regions only
	/// bound, so its routes name nothing, even by the qualified identity's own hop.
	#[test]
	fn known_leaves_out_a_scoped_route() {
		let known = kio::Shared::<Known>::default();
		let (origin, relay) = (Node::new(1, 1), Node::publishing(2, 2, known.clone()));
		let qualify = super::super::qualifier([7; 32]);
		let scoped = Link {
			scope: Some(super::super::Scope {
				regions: vec![p("live")],
				qualify: qualify.clone(),
			}),
			..Link::default()
		};
		let answer = origin.answer(Link::default(), p("live")).unwrap();
		let receive = relay.receive(scoped, p("live")).unwrap();
		origin.own(p("live/x"), Some(claim(&Instance::random(), 5)), None);
		pump(&answer, &receive);
		assert!(
			!served(outputs(&receive)).is_empty(),
			"the scoped route serves the claim"
		);
		let mut chain = crate::Hops::new();
		chain.push(hop(qualify(1) | QUALIFIED)).unwrap();
		assert!(!known.read().names(&chain, &p("live/x")));
	}

	#[test]
	fn a_claim_is_served_through_the_stream_its_route_arrived_on() {
		let instance = Instance::random();
		let (origin, relay) = (Node::new(1, 1), Node::new(2, 2));
		let answer = origin.answer(Link::default(), Path::empty()).unwrap();
		let receive = relay.receive(Link::default(), Path::empty()).unwrap();
		origin.own(p("live/x"), Some(claim(&instance, 5)), None);
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
					available: true,
				},
			}]
		);

		origin.own(p("live/x"), None, None);
		pump(&answer, &receive);
		assert_eq!(
			served(outputs(&receive)),
			[Output::Unserve {
				id: 1,
				claim: p("live/x")
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
		origin.own(p("live/x"), Some(claim(&instance, 0)), None);
		pump(&far_answer, &far);
		pump(&near_answer, &near);
		let metric = |outputs: Vec<Output>| match &served(outputs)[..] {
			[.., Output::Serve { served, .. }] => Some(served.metric),
			_ => None,
		};
		assert_eq!(metric(outputs(&far)), Some(3));
		assert_eq!(metric(outputs(&near)), Some(1));
		far.serving(1, &p("live/x"));
		near.serving(1, &p("live/x"));

		// A stream whose route ends lets go, since the other still serves it.
		far.route(1, None);
		assert_eq!(
			served(outputs(&far)),
			[Output::Unserve {
				id: 1,
				claim: p("live/x")
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
		origin.own(p("live/a/x"), Some(claim(&instance, 0)), None);
		origin.own(p("live/b/y"), Some(claim(&instance, 0)), None);
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

	/// A stalled stream reads a takeover's successor before its predecessor goes, even
	/// once the successor changed again meanwhile, so the session still finds the
	/// predecessor to carry on.
	#[test]
	fn a_stalled_takeover_reads_its_successor_first() {
		let (old, new) = (Instance::random(), Instance::random());
		let origin = Node::new(1, 1);
		let relay = Node::new(2, 2);
		let fast = relay.receive(Link::default(), Path::empty()).unwrap();
		let stalled = relay.receive(Link::default(), Path::empty()).unwrap();
		let fast_answer = origin.answer(Link::default(), Path::empty()).unwrap();
		let stalled_answer = origin.answer(Link::default(), Path::empty()).unwrap();
		origin.own(p("live"), Some(claim(&old, 0)), None);
		pump(&fast_answer, &fast);
		pump(&stalled_answer, &stalled);
		outputs(&fast);
		outputs(&stalled);

		origin.own(p("live/x"), Some(claim(&new, 0)), Some(&p("live")));
		origin.own(p("live"), None, None);
		pump(&fast_answer, &fast);
		origin.own(p("live/x"), Some(claim(&new, 5)), None);
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
		origin.own(p("live/x"), Some(claim(&instance, 0)), None);
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
		origin.own(p("live/x"), Some(claim(&instance, 0)), None);
		pump(&fast_answer, &fast);
		pump(&stalled_answer, &stalled);
		for cost in 1..1000 {
			origin.own(p("live/x"), Some(claim(&instance, cost)), None);
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
			origin.own(p("live/x"), Some(claim(&instance, cost)), None);
			origin.own(p("live/churn"), Some(claim(&instance, cost)), None);
			origin.own(p("live/churn"), None, None);
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
			origin.own(p("live/y"), Some(claim(&instance, cost)), None);
		}
		origin.own(p("live/x"), None, None);
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
			}))
		};
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
		let read = of(outputs(&edge), |id| id >= 10);
		assert_eq!(read, [], "the churned routes");
		assert_eq!(relay.retained().echoes, 0);

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

	#[test]
	fn a_route_lost_with_its_session_leaves_the_others_serving() {
		let instance = Instance::random();
		let origin = Node::new(1, 1);
		let relay = Node::new(2, 2);
		let first = relay.receive(Link::default(), Path::empty()).unwrap();
		let first_answer = origin.answer(Link::default(), Path::empty()).unwrap();
		let second = relay.receive(Link::default(), Path::empty()).unwrap();
		let second_answer = origin.answer(Link::default(), Path::empty()).unwrap();
		origin.own(p("live/x"), Some(claim(&instance, 0)), None);
		pump(&first_answer, &first);
		pump(&second_answer, &second);
		outputs(&first);
		assert!(matches!(served(outputs(&second))[..], [.., Output::Serve { .. }]));
		second.serving(1, &p("live/x"));

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
		};
		receive.route(1, Some(Reach::Finite(advert)));
		receive.batch(1, vec![(p("live/x"), Some(claim(&instance, 0)))], Some(1));
		assert!(matches!(served(outputs(&receive))[..], [Output::Serve { .. }]));
		receive.serving(1, &p("live/x"));

		receive.route(1, Some(Reach::Held));
		assert!(matches!(
			&served(outputs(&receive))[..],
			[Output::Serve { served, .. }] if !served.available
		));
		receive.route(1, None);
		assert!(matches!(served(outputs(&receive))[..], [Output::Unserve { .. }]));
	}
}
