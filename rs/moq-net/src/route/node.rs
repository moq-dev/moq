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
//! serves into the origin as the next hop of their selected route. A claim moves
//! to another stream's session when its identity's selected route does, and the
//! old session lets go only once the new one serves it, so the origin always
//! holds a route for it.

use std::{
	collections::{BTreeMap, BTreeSet, HashMap},
	sync::{Arc, Mutex, MutexGuard},
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

/// What a stream's session acts on, in order.
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

struct State {
	router: Router<Key, Claim>,
	next_key: Key,
	queues: HashMap<Key, kio::Queue<Output>>,
	/// Each shown claim of another identity, and the streams serving it.
	homes: BTreeMap<(Id, Path), Home>,
}

/// Where one shown claim is served.
#[derive(Default)]
struct Home {
	/// The stream it is served through, and what it was last sent.
	key: Option<(Key, Served)>,
	/// Streams that served it before and let go once `key` confirms.
	leaving: BTreeSet<Key>,
	/// Whether `key` confirmed it serves the claim.
	confirmed: bool,
}

impl Node {
	/// A node declaring a fresh identity, `seed` keying its rotation.
	pub fn new(id: Id, seed: u64) -> Self {
		debug_assert!(id != 0 && id & QUALIFIED == 0, "a declared identity is non-zero and unqualified");
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
		self.lock().router.declared().expect("a node always declares an identity")
	}

	/// This node's own claim at `path` changed: `None` withdraws it.
	pub fn own(&self, path: Path, claim: Option<Claim>) {
		let mut state = self.lock();
		let changed = match claim {
			Some(claim) => {
				let current = state.router.declared().and_then(|id| state.router.view(id)?.items.get(&path).cloned());
				current.as_ref() != Some(&claim) && state.router.announce(path, claim).is_ok()
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
		let queue = kio::Queue::new();
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
	queue: kio::Queue<Output>,
}

impl std::fmt::Debug for Stream {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Stream").field("key", &self.key).finish_non_exhaustive()
	}
}

impl Stream {
	/// The next output for this stream's session.
	pub fn poll_output(&self, waiter: &kio::Waiter) -> std::task::Poll<Output> {
		match self.queue.poll_pop(waiter) {
			std::task::Poll::Ready(Ok(output)) => std::task::Poll::Ready(output),
			// The queue closes only once the stream dropped.
			_ => std::task::Poll::Pending,
		}
	}

	/// The next output already queued, without waiting.
	pub fn try_output(&self) -> Option<Output> {
		self.queue.try_pop().ok().flatten()
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

	/// This stream's session now serves `claim` of `id`, so wherever it was served
	/// before lets go.
	pub fn serving(&self, id: Id, claim: &Path) {
		let mut state = self.node.lock();
		let State { homes, queues, .. } = &mut *state;
		let Some(home) = homes.get_mut(&(id, claim.clone())) else {
			return;
		};
		if home.key.as_ref().is_none_or(|(key, _)| *key != self.key) || home.confirmed {
			return;
		}
		home.confirmed = true;
		for key in std::mem::take(&mut home.leaving) {
			push(queues, key, Output::Unserve { id, claim: claim.clone() });
		}
	}
}

impl Drop for Stream {
	fn drop(&mut self) {
		self.queue.close();
		let mut state = self.node.lock();
		state.queues.remove(&self.key);
		state.router.disconnect(self.key);
		for home in state.homes.values_mut() {
			home.leaving.remove(&self.key);
			if home.key.as_ref().is_some_and(|(key, _)| *key == self.key) {
				// Served nowhere until the router moves it: nothing is left here to wait for.
				home.key = None;
				home.confirmed = false;
			}
		}
		let _ = state.router.bump();
		state.flush();
	}
}

fn push(queues: &HashMap<Key, kio::Queue<Output>>, key: Key, output: Output) {
	if let Some(queue) = queues.get(&key) {
		let _ = queue.try_push(output);
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

	/// Serve each shown claim of `id` through the stream its selected route
	/// goes through, keeping it where it is while no route is selected.
	fn home(&mut self, id: Id) {
		let selected = self.router.selected(id);
		let claims: Vec<(Path, Claim, bool)> = (self.router.shown().range((id, Path::empty())..))
			.take_while(|((shown, _), _)| *shown == id)
			.map(|((_, claim), shown)| (claim.clone(), shown.item.clone(), shown.available))
			.collect();
		let mut gone: BTreeSet<Path> = self
			.homes
			.range((id, Path::empty())..)
			.take_while(|((home, _), _)| *home == id)
			.map(|((_, claim), _)| claim.clone())
			.collect();
		for (claim, item, available) in claims {
			gone.remove(&claim);
			let home = self.homes.entry((id, claim.clone())).or_default();
			let current = home.key.as_ref().map(|(key, served)| (*key, served.metric));
			let Some((key, metric)) = selected
				.and_then(|route| Some((route.next?, route.metric)))
				.or(current)
			else {
				continue;
			};
			let served = Served {
				claim: item,
				metric,
				available,
			};
			match &home.key {
				Some((at, sent)) if *at == key && *sent == served => continue,
				Some((at, _)) if *at == key => {}
				Some((at, _)) => {
					if home.confirmed {
						home.leaving.insert(*at);
					}
					home.confirmed = false;
				}
				None => home.confirmed = false,
			}
			home.key = Some((key, served.clone()));
			let named = self.router.name(key, id);
			push(
				&self.queues,
				key,
				Output::Serve {
					id,
					named,
					claim,
					served,
				},
			);
		}
		for claim in gone {
			let Some(home) = self.homes.remove(&(id, claim.clone())) else {
				continue;
			};
			for key in home.key.map(|(key, _)| key).into_iter().chain(home.leaving) {
				push(&self.queues, key, Output::Unserve { id, claim: claim.clone() });
			}
		}
		// Nothing left to recover: let the identity go.
		let shows = (self.router.shown().range((id, Path::empty())..).next()).is_some_and(|((shown, _), _)| *shown == id);
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
				claim: p("live/x")
			}]
		);
	}

	#[test]
	fn a_claim_moves_to_a_cheaper_stream_once_that_one_serves_it() {
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
		origin.own(p("live/x"), Some(claim(&instance, 0)));
		pump(&far_answer, &far);
		assert!(matches!(served(outputs(&far))[..], [Output::Serve { .. }]));
		far.serving(1, &p("live/x"));

		let near = relay.receive(Link::default(), Path::empty()).unwrap();
		let near_answer = origin.answer(Link::default(), Path::empty()).unwrap();
		pump(&far_answer, &far);
		pump(&near_answer, &near);
		assert!(matches!(&served(outputs(&near))[..], [Output::Serve { served, .. }] if served.metric == 1));
		assert!(served(outputs(&far)).is_empty(), "the old stream serves until the new one does");
		near.serving(1, &p("live/x"));
		assert_eq!(
			served(outputs(&far)),
			[Output::Unserve {
				id: 1,
				claim: p("live/x")
			}]
		);
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
