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

use super::{Error, Id, Link, Metric, Out, QUALIFIED, Reach, Rev, Router, Seqno};

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
	/// Send ROUTE_REQUEST, naming the identity as the peer advertised it.
	Request { id: Id, seqno: Seqno },
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
/// identity, and a claim the reader was never sent leaves nothing when it ends, so a
/// stream whose reader stalls holds one entry per identity, bounded by its claims,
/// however much the node changes meanwhile.
#[derive(Default)]
struct Outbox {
	/// Identities with route changes or claims pending, in the order they first queued.
	order: VecDeque<Id>,
	pending: HashMap<Id, Pending>,
	/// Outputs to read in order: the other kinds, and a pending entry once taken.
	ready: VecDeque<Output>,
	/// The claims the reader was sent for each identity.
	delivered: HashMap<Id, BTreeSet<Path>>,
}

/// One identity's route changes and claim batches since the reader last took it.
#[derive(Default)]
struct Pending {
	/// A ROUTE_END goes first, retiring every claim sent before.
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
		self.ready.is_empty() && self.order.is_empty()
	}

	/// The pending entry for `id`, queued behind the others when new.
	fn entry<'a>(order: &mut VecDeque<Id>, pending: &'a mut HashMap<Id, Pending>, id: Id) -> &'a mut Pending {
		pending.entry(id).or_insert_with(|| {
			order.push_back(id);
			Pending::default()
		})
	}

	fn push(&mut self, output: Output) {
		match output {
			Output::Route { id, reach } => {
				let entry = Self::entry(&mut self.order, &mut self.pending, id);
				match reach {
					None => {
						*entry = Pending {
							ended: true,
							..Pending::default()
						}
					}
					Some(Reach::Held) => entry.held = true,
					Some(finite) => {
						entry.finite = Some(finite);
						entry.held = false;
					}
				}
			}
			Output::Batch { id, changes, floor } => {
				let entry = Self::entry(&mut self.order, &mut self.pending, id);
				let sent = self.delivered.get(&id);
				let ended = entry.ended;
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
			}
			other => self.ready.push_back(other),
		}
	}

	fn pop(&mut self) -> Option<Output> {
		if self.ready.is_empty() {
			let id = self.order.pop_front()?;
			let pending = self.pending.remove(&id).expect("every queued identity is pending");
			self.take(id, pending);
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
			self.ready.push_back(Output::Route { id, reach: Some(reach) });
		}
		if pending.held {
			self.ready.push_back(Output::Route {
				id,
				reach: Some(Reach::Held),
			});
		}
		if let Some((merged, floor)) = pending.batch {
			let sent = self.delivered.entry(id).or_default();
			// As the router orders a batch: what starts or changes, then what ends.
			let (ends, starts): (Vec<_>, Vec<_>) = merged.into_iter().partition(|(_, claim)| claim.is_none());
			for (path, _) in &starts {
				sent.insert(path.clone());
			}
			for (path, _) in &ends {
				sent.remove(path);
			}
			if sent.is_empty() {
				self.delivered.remove(&id);
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
}

/// Where one shown claim is served: each stream, what it was last sent, and
/// whether it confirmed serving it.
#[derive(Default)]
struct Home {
	keys: BTreeMap<Key, Slot>,
}

struct Slot {
	served: Served,
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
	pub fn new(id: Id, seed: u64) -> Self {
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
			})),
		}
	}

	/// A node with a random identity.
	pub fn random() -> Self {
		let mut rng = kio::clock::rng();
		let id = rng.random_range(1..QUALIFIED);
		Self::new(id, rng.random())
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

fn push(queues: &Queues, key: Key, output: Output) {
	if let Some(queue) = queues.get(&key) {
		queue.lock().push(output);
	}
}

impl State {
	/// Route the router's messages and shown claims to the streams.
	fn flush(&mut self) {
		for out in self.router.drain() {
			let (key, output) = match out {
				Out::Route { to, id, reach } => (to, Output::Route { id, reach }),
				Out::Batch { to, id, changes, floor } => (to, Output::Batch { id, changes, floor }),
				Out::Request { to, id, seqno } => (to, Output::Request { id, seqno }),
				Out::Echo { to, id } => (to, Output::Echo { id }),
			};
			push(&self.queues, key, output);
		}
		// The shown claims below are read whole; the events only say which changed.
		self.router.events();
		let changed = self.router.changed();
		let declared = self.router.declared();
		for id in changed {
			if Some(id) != declared {
				self.home(id);
			}
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
			let wanted: Vec<(Key, Metric, bool)> = match routes.is_empty() {
				true => (home.keys.iter())
					.map(|(key, slot)| (*key, slot.served.metric, false))
					.collect(),
				false => routes.iter().map(|&(key, metric)| (key, metric, available)).collect(),
			};
			for slot in home.keys.values_mut() {
				slot.leaving = true;
			}
			for (key, metric, available) in wanted {
				let served = Served {
					claim: item.clone(),
					metric,
					available,
				};
				match home.keys.get_mut(&key) {
					Some(slot) if slot.served == served => {
						slot.leaving = false;
						continue;
					}
					Some(slot) => {
						slot.served = served.clone();
						slot.leaving = false;
					}
					None => {
						let slot = Slot {
							served: served.clone(),
							confirmed: false,
							leaving: false,
						};
						home.keys.insert(key, slot);
					}
				}
				let named = self.router.name(key, id);
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
	crate::Hop::new(id).expect("a non-zero hop id")
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

	/// A route that starts and ends while the reader stalls reaches it as an end alone,
	/// which the reader acks at once since it never held the route.
	#[test]
	fn a_route_started_and_ended_unread_is_an_end() {
		let instance = Instance::random();
		let origin = Node::new(1, 1);
		let relay = Node::new(2, 2);
		let receive = relay.receive(Link::default(), Path::empty()).unwrap();
		let answer = origin.answer(Link::default(), Path::empty()).unwrap();
		origin.own(p("live/x"), Some(claim(&instance, 0)), None);
		pump(&answer, &receive);
		let edge = relay
			.answer(
				Link {
					proxy: true,
					..Link::default()
				},
				Path::empty(),
			)
			.unwrap();
		drop(receive);
		let read: Vec<Output> = (outputs(&edge).into_iter())
			.filter(|output| matches!(output, Output::Route { id: 1, .. } | Output::Batch { id: 1, .. }))
			.collect();
		assert_eq!(read, [Output::Route { id: 1, reach: None }]);
		assert_eq!(relay.retained().echoes, 1, "the end awaits its echo");
		edge.echo(1);
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
