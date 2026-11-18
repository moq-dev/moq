//! One origin node's router, shared by every lite-07 session on its origin.
//!
//! Each session is one neighbour key on the [`Router`], for both directions: it sends
//! everything it says about routing on one stream and hears the peer on another, so the
//! node sees one ordered queue each way. The session's [`Stream`] lives as long as the
//! session, and dropping it closes the session for the router. A handle that reads part
//! of what the peer asks for through a mount answers that part under a key of its own
//! ([`Stream::mount`]), which only sends.
//!
//! The node does no I/O. A session feeds it what the peer sent and takes from its queue
//! the messages to send and the claims to serve into the origin. A claim is served
//! through the session a request for it leaves on, once per node claiming it that a
//! route reaches. A session lets go of a claim only once another serves it, and a session
//! that closes leaves what it served behind ([`Orphan`]) until then, so the origin holds
//! the claim for as long as the router's reader shows it.
//!
//! The origin's own claims, everything in its table the route layer did not put there,
//! reach the router as an endpoint's would: through a neighbour that is the origin.

use std::{
	collections::{BTreeMap, BTreeSet, HashMap, VecDeque},
	sync::{Arc, Mutex, MutexGuard},
	task::{Poll, ready},
};

use kio::clock::{Deadlines, Instant};

use crate::{Instance, Path};

use super::{
	Error, Id, Item, Kind, Metric,
	prefix::ancestors,
	relay::{self, Next, NodeId, Port, Role, Router},
};

/// One session, or one mount a session answers through.
pub(crate) type Key = u64;

/// The origin's own claims, which any neighbour may be told.
const LOCAL: Key = 0;

/// The origin's own claims that crossed a marked link before they reached it, which a
/// marked neighbour is never told.
const LOCAL_MARKED: Key = 1;

/// What a claim's publisher says of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Attr {
	pub epoch: u64,
	/// How many segments the claim's own prefix lacks of the path it is announced at: 0
	/// for a claim exactly there (see [`Item::lowered`]).
	pub above: u64,
}

impl Item for Attr {
	fn lowered(&self, by: u64) -> Self {
		Self {
			above: self.above.saturating_add(by),
			..self.clone()
		}
	}

	fn above(&self) -> u64 {
		self.above
	}

	fn epoch(&self) -> u64 {
		self.epoch
	}
}

/// A claim on the route layer.
pub(crate) type Claim = relay::Claim<Attr>;

/// A change to a claim on the route layer.
pub(crate) type Change = relay::Change<Attr>;

/// A message on a session's announce stream.
pub(crate) type Msg = relay::Msg<Instance, Attr>;

/// A broadcast on the route layer.
pub(crate) type Broadcast = relay::Broadcast<Instance>;

/// A claim as the origin serves it through one session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Served {
	pub attr: Attr,
	/// From this node to the publisher.
	pub cost: Metric,
	/// The lowest link priority on the way.
	pub priority: i8,
	/// The way crosses a node that sent GOAWAY.
	pub draining: bool,
	/// Whether a route reaches it; while none does it stays shown, and serves nothing.
	pub available: bool,
	/// The one a reader here follows, and a request naming no node.
	pub primary: bool,
	/// Each relay it entered or left a routing domain by, as [`relay::Claim::chain`].
	pub chain: Vec<Id>,
	/// The way to it crossed a marked link, so the origin never offers it to an upstream.
	pub marked: bool,
	/// The node a request through the session names, or `None` when the peer selects:
	/// it gave this node the claim itself.
	pub named: Option<NodeId>,
}

/// A change to what a session serves into the origin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Output {
	/// Serve `origin`'s claim of the broadcast through this session, or update how it is
	/// served. Answer with [`Stream::serving`].
	Serve {
		origin: NodeId,
		path: Path,
		instance: Instance,
		served: Served,
	},
	/// Stop serving it, for the reason `gone` gives.
	Unserve {
		origin: NodeId,
		path: Path,
		instance: Instance,
		gone: Gone,
	},
}

/// Why a session stops serving a claim.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Gone {
	/// Another session serves it now.
	Moved,
	/// The node let go of it here: every copy the origin holds goes, offline.
	Lost,
	/// Its publisher withdrew it, as far as this node knows: every copy the origin holds
	/// ends.
	Ended,
}

/// A served claim's place in the origin, which outlives the session that served it until
/// another session serves the claim or the reader lets it go.
pub(crate) trait Orphan: Send {
	/// Let go of it, for the reason `gone` gives.
	fn release(self: Box<Self>, gone: Gone);
}

/// Where a request a peer sent goes; see [`Node::poll_forward`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Via {
	/// The origin serves it itself.
	Here,
	/// Along the claim the origin serves for `node`.
	Route { node: NodeId },
}

/// How many messages a session may leave unread beyond twice what it holds.
const SLACK: usize = 1024;

/// What a session has yet to act on.
#[derive(Default)]
struct Outbox {
	wire: VecDeque<Msg>,
	serve: VecDeque<Output>,
	/// The session is leaving: it sends nothing more.
	muted: bool,
	/// The most messages a reader that keeps up leaves unread.
	limit: usize,
}

/// Whether a session serves a claim it was handed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Confirm {
	Waiting,
	Serving,
	/// It may not carry the claim.
	Refused,
}

/// One claim served through one session.
struct Slot {
	served: Served,
	confirm: Confirm,
	/// Unwanted, and kept until a wanted one serves the claim.
	leaving: bool,
	/// What its session left behind when it closed.
	orphan: Option<Box<dyn Orphan>>,
}

/// A slot's claimant and session.
type SlotKey = (NodeId, Key);

struct State {
	router: Router<Key, Instance, Attr>,
	clock: Deadlines,
	next_key: Key,
	queues: HashMap<Key, kio::Shared<Outbox>>,
	/// Each session's own key, by the tag every stream of the session shares.
	tags: HashMap<u64, Key>,
	/// The mounts of each session's own key.
	mounts: HashMap<Key, BTreeSet<Key>>,
	/// How the origin serves each broadcast the reader shows.
	lanes: BTreeMap<Broadcast, BTreeMap<SlotKey, Slot>>,
	/// The broadcasts each session has a slot for.
	serving: HashMap<Key, BTreeSet<Broadcast>>,
	/// Orphans to release once the lock is.
	released: Vec<(Box<dyn Orphan>, Gone)>,
	/// Whether each of the origin's own claims is filed as having crossed a marked link.
	own: BTreeMap<Broadcast, bool>,
}

/// The node every lite-07 session of one origin shares.
#[derive(Clone)]
pub(crate) struct Node {
	state: Arc<Mutex<State>>,
	/// When the router's next timer fires, for the task that fires it.
	timer: kio::Shared<Option<Instant>>,
	/// Counts the times what the origin serves changed, for requests waiting on it.
	moved: kio::Shared<u64>,
}

impl std::fmt::Debug for Node {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Node").finish_non_exhaustive()
	}
}

/// The node's state, locked. A stream's wakes wait until the lock is released: a waker
/// may poll the stream's session inline, and the session calls back into the node.
struct Locked<'a> {
	state: Option<MutexGuard<'a, State>>,
	node: &'a Node,
	_hold: kio::Hold,
}

impl std::ops::Deref for Locked<'_> {
	type Target = State;

	fn deref(&self) -> &State {
		self.state.as_ref().expect("locked")
	}
}

impl std::ops::DerefMut for Locked<'_> {
	fn deref_mut(&mut self) -> &mut State {
		self.state.as_mut().expect("locked")
	}
}

impl Drop for Locked<'_> {
	fn drop(&mut self) {
		let mut state = self.state.take().expect("locked");
		// Whatever changed, the timer task waits for the router's next deadline. Published
		// under the lock, so a deadline read before another thread's change never replaces
		// the one read after it.
		let deadline = state.router.deadline();
		if *self.node.timer.read() != deadline {
			*self.node.timer.lock() = deadline;
		}
		let released = std::mem::take(&mut state.released);
		drop(state);
		// An orphan's release reaches into the origin, which may be feeding this node.
		for (orphan, gone) in released {
			orphan.release(gone);
		}
	}
}

impl Node {
	/// A node naming itself `me`, dating what it hears by `clock`, that carries claims
	/// between its peers when it `forwards`, as a relay does.
	pub fn new(me: NodeId, forwards: bool, clock: Deadlines) -> Self {
		debug_assert!(me.relay != 0, "a node names a relay");
		let mut router = Router::new(me, relay::debounce(relay::HOP));
		if !forwards {
			router.endpoint();
		}
		router.clock(clock.now());
		router.follow();
		for (key, upstream) in [(LOCAL, false), (LOCAL_MARKED, true)] {
			// Free, so a claim costs what the links it crossed do.
			let port = Port {
				cost: 0,
				priority: i8::MAX,
				upstream,
				role: Role::Endpoint,
			};
			router.connect(key, port).expect("a fresh key");
			// The origin answers at once: its table is what it holds.
			router.recv(key, Msg::Edited).expect("the origin was asked");
			router
				.recv(key, Msg::Synced(Path::empty()))
				.expect("the origin was asked");
		}
		router.drain();
		Self {
			state: Arc::new(Mutex::new(State {
				router,
				clock,
				next_key: LOCAL_MARKED + 1,
				queues: HashMap::new(),
				tags: HashMap::new(),
				mounts: HashMap::new(),
				lanes: BTreeMap::new(),
				serving: HashMap::new(),
				released: Vec::new(),
				own: BTreeMap::new(),
			})),
			timer: kio::Shared::new(None),
			moved: kio::Shared::new(0),
		}
	}

	/// A node for the origin whose node is `relay`, named by it and by when it started,
	/// which only grows across the restarts of one relay, that `forwards` as the origin does.
	pub fn origin(relay: std::num::NonZeroU64, forwards: bool, clock: Deadlines) -> Self {
		let started = kio::clock::Wall::now().saturating_duration_since(kio::clock::Wall::UNIX_EPOCH);
		let me = NodeId {
			relay: relay.get(),
			start: u64::try_from(started.as_micros()).unwrap_or(u64::MAX),
			marked: false,
		};
		Self::new(me, forwards, clock)
	}

	/// Whether `other` is this node, rather than another origin's.
	pub fn same(&self, other: &Self) -> bool {
		Arc::ptr_eq(&self.state, &other.state)
	}

	fn lock(&self) -> Locked<'_> {
		let state = self.state.lock().unwrap_or_else(|poison| poison.into_inner());
		Locked {
			_hold: kio::hold(),
			node: self,
			state: Some(state),
		}
	}

	/// How this node names itself.
	pub fn me(&self) -> NodeId {
		self.lock().router.me()
	}

	/// The origin's own claim of `broadcast` changed. `marked` files it among the claims
	/// that crossed a marked link, which a marked neighbour is never told.
	pub fn own(&self, marked: bool, broadcast: Broadcast, change: Change) {
		let mut state = self.lock();
		let now = state.clock.now();
		state.router.clock(now);
		let key = |marked| match marked {
			true => LOCAL_MARKED,
			false => LOCAL,
		};
		let was = match &change {
			Change::Live(_) => state.own.insert(broadcast.clone(), marked),
			Change::Ended | Change::Lost => state.own.remove(&broadcast),
		};
		let live = matches!(change, Change::Live(_));
		// An end goes to the key that holds the claim.
		let to = key(if live { marked } else { was.unwrap_or(marked) });
		let changes = vec![(broadcast.clone(), change)];
		let _ = state.router.recv(to, Msg::Announce { changes });
		// A claim that moved between the keys leaves the old one after the new one holds it.
		if live && was.is_some_and(|was| was != marked) {
			let changes = vec![(broadcast, Change::Lost)];
			let _ = state.router.recv(key(!marked), Msg::Announce { changes });
		}
		state.flush(self);
	}

	/// Register the session `tag` names, which this node asks at once as `port` says.
	pub fn connect(&self, tag: u64, port: Port) -> Result<Stream, Error> {
		let mut state = self.lock();
		let key = state.register(port)?;
		state.tags.insert(tag, key);
		state.mounts.insert(key, BTreeSet::new());
		state.flush(self);
		Ok(Stream {
			node: self.clone(),
			key,
			main: None,
			queue: state.queues[&key].clone(),
			clock: None,
		})
	}

	/// Where a request for `broadcast` goes that the peer of the session `tag` names sent
	/// naming `named`, or no node: here, along a claim the origin serves, or `None` when
	/// nothing reaches it or the way would lead back to the peer. Waits while the session
	/// it leaves on has yet to serve the claim.
	pub fn poll_forward(
		&self,
		tag: u64,
		named: Option<NodeId>,
		broadcast: &Broadcast,
		waiter: &kio::Waiter,
	) -> Poll<Option<Via>> {
		loop {
			let seen = *self.moved.read();
			let state = self.lock();
			let from = state.tags.get(&tag).copied();
			if let Some(via) = state.forward(from, named, broadcast) {
				return Poll::Ready(via);
			}
			drop(state);
			ready!(self.moved.poll(waiter, |moved| match **moved != seen {
				true => Poll::Ready(()),
				false => Poll::Pending,
			}));
		}
	}

	/// Fire the router's timers due by the origin's clock.
	pub fn timeout(&self) {
		let mut state = self.lock();
		let now = state.clock.now();
		state.router.clock(now);
		state.router.timeout(now);
		state.flush(self);
	}

	/// When the router's next timer fires, waking `waiter` once that changes from `armed`.
	pub fn poll_deadline(&self, armed: Option<Instant>, waiter: &kio::Waiter) -> Poll<Option<Instant>> {
		let deadline = ready!(self.timer.poll(waiter, |at| match **at != armed {
			true => Poll::Ready(()),
			false => Poll::Pending,
		}));
		Poll::Ready(*deadline)
	}
}

/// One session's key on the node, or a mount's ([`Self::mount`]). Dropping a session's
/// closes it and every mount of it still held. Dropping a mount's while its session stays
/// up retires it quietly.
pub(crate) struct Stream {
	node: Node,
	key: Key,
	/// The session's own key, for a mount.
	main: Option<Key>,
	queue: kio::Shared<Outbox>,
	/// The clock of the session the stream belongs to. The origin's clock moves only as
	/// its driver polls, so an idle origin reads an old instant: what a session hands the
	/// node is dated by the later of the two.
	clock: Option<Deadlines>,
}

impl std::fmt::Debug for Stream {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Stream").field("key", &self.key).finish_non_exhaustive()
	}
}

impl Stream {
	/// The session's key on the node.
	pub fn key(&self) -> Key {
		self.main.unwrap_or(self.key)
	}

	/// Date what the stream hands the node by its session's clock too.
	pub fn with_clock(mut self, clock: Deadlines) -> Self {
		self.clock = Some(clock);
		self
	}

	/// The next message to send the peer.
	pub fn poll_wire(&self, waiter: &kio::Waiter) -> Poll<Msg> {
		let ready = |outbox: &kio::Ref<'_, Outbox>| match outbox.wire.is_empty() {
			true => Poll::Pending,
			false => Poll::Ready(()),
		};
		let mut outbox = ready!(self.queue.poll(waiter, ready));
		Poll::Ready(outbox.wire.pop_front().expect("checked above"))
	}

	/// The next message to send the peer already queued, without waiting.
	#[cfg(any(test, feature = "fuzz"))]
	pub fn try_wire(&self) -> Option<Msg> {
		// Checked under the read lock, which wakes no waiter when nothing is queued.
		let empty = self.queue.read().wire.is_empty();
		match empty {
			true => None,
			false => self.queue.lock().wire.pop_front(),
		}
	}

	/// The next change to what the session serves into the origin.
	pub fn poll_serve(&self, waiter: &kio::Waiter) -> Poll<Output> {
		let ready = |outbox: &kio::Ref<'_, Outbox>| match outbox.serve.is_empty() {
			true => Poll::Pending,
			false => Poll::Ready(()),
		};
		let mut outbox = ready!(self.queue.poll(waiter, ready));
		Poll::Ready(outbox.serve.pop_front().expect("checked above"))
	}

	/// The next change to what the session serves already queued, without waiting.
	#[cfg(any(test, feature = "fuzz"))]
	pub fn try_serve(&self) -> Option<Output> {
		// Checked under the read lock, which wakes no waiter when nothing is queued.
		let empty = self.queue.read().serve.is_empty();
		match empty {
			true => None,
			false => self.queue.lock().serve.pop_front(),
		}
	}

	/// Ready once the peer left unread more than twice what it holds: it reads a trickle
	/// while what it is sent changes faster, so the session cuts it off, as it does a
	/// reader that stops.
	pub fn poll_overrun(&self, waiter: &kio::Waiter) -> Poll<()> {
		let overrun = |outbox: &kio::Ref<'_, Outbox>| match outbox.wire.len() > outbox.limit {
			true => Poll::Ready(()),
			false => Poll::Pending,
		};
		self.queue.poll(waiter, overrun).map(|_| ())
	}

	/// A key answering part of what the peer asks for through a mount: the peer's edits
	/// of that part go to it, and it is asked for nothing.
	pub fn mount(&self) -> Result<Stream, Error> {
		let mut state = self.node.lock();
		let main = self.key();
		let port = state.router.port(main).ok_or(Error::UnknownNeighbor)?;
		let port = Port {
			role: Role::Scoped(Vec::new()),
			..port
		};
		let key = state.register(port)?;
		state.mounts.entry(main).or_default().insert(key);
		Ok(Stream {
			node: self.node.clone(),
			key,
			main: Some(main),
			queue: state.queues[&key].clone(),
			clock: self.clock.clone(),
		})
	}

	/// The later of the origin's clock and the session's.
	fn now(&self, state: &State) -> Instant {
		let origin = state.clock.now();
		self.clock.as_ref().map_or(origin, |clock| origin.max(clock.now()))
	}

	/// Take in a message the peer sent. An error closes the session. What it changes
	/// reaches the other sessions at the next [`Self::flush`], so a burst of messages
	/// is advertised once.
	pub fn recv(&self, msg: Msg) -> Result<(), Error> {
		let mut state = self.node.lock();
		let now = self.now(&state);
		state.router.clock(now);
		state.router.recv(self.key, msg)
	}

	/// Tell the other sessions what the messages taken in changed.
	pub fn flush(&self) {
		self.node.lock().flush(&self.node);
	}

	/// Edit what this node asks the peer for, which never forwards for it.
	pub fn ask(&self, edit: relay::Edit) -> Result<(), Error> {
		let mut state = self.node.lock();
		state.router.ask(self.key, edit)?;
		state.flush(&self.node);
		Ok(())
	}

	/// What this node asks the peer for, once the peer answers every edit.
	pub fn asking(&self) -> BTreeSet<Path> {
		self.node.lock().router.asking(self.key)
	}

	/// Whether the peer answered every edit of what this node asks it for.
	pub fn answered(&self) -> bool {
		self.node.lock().router.answered(self.key)
	}

	/// The peer sent GOAWAY: what it gave ranks below everything else.
	pub fn goaway(&self) {
		let mut state = self.node.lock();
		let now = self.now(&state);
		state.router.clock(now);
		state.router.goaway(self.key);
		state.flush(&self.node);
	}

	/// The session is leaving: nothing more is queued for the peer.
	pub fn mute(&self) {
		let _state = self.node.lock();
		let mut outbox = self.queue.lock();
		outbox.muted = true;
		outbox.wire.clear();
	}

	/// The peer finished its stream: it is leaving, so everything it gave goes, and it is
	/// sent nothing more.
	pub fn finished(&self) {
		let mut state = self.node.lock();
		let now = self.now(&state);
		state.router.clock(now);
		state.close(self.key);
		state.flush(&self.node);
	}

	/// Whether this session now serves `origin`'s claim of `broadcast` as it was last
	/// handed it, or may not carry it.
	pub fn serving(&self, origin: NodeId, broadcast: &Broadcast, ok: bool) {
		let mut state = self.node.lock();
		let key = (origin, self.key);
		let Some(slot) = (state.lanes.get_mut(broadcast)).and_then(|slots| slots.get_mut(&key)) else {
			return;
		};
		slot.confirm = match ok {
			true => Confirm::Serving,
			false => Confirm::Refused,
		};
		state.settle(broadcast, false);
		*self.node.moved.lock() += 1;
	}

	/// Leave what this session serves of `origin`'s claim of `broadcast` behind as it
	/// closes, for the node to release once another session serves it.
	pub fn orphan(&self, origin: NodeId, broadcast: &Broadcast, orphan: Box<dyn Orphan>) {
		let mut state = self.node.lock();
		let key = (origin, self.key);
		match (state.lanes.get_mut(broadcast)).and_then(|slots| slots.get_mut(&key)) {
			Some(slot) => slot.orphan = Some(orphan),
			None => state.released.push((orphan, Gone::Moved)),
		}
	}
}

impl Drop for Stream {
	fn drop(&mut self) {
		let mut state = self.node.lock();
		let now = self.now(&state);
		state.router.clock(now);
		for mount in state.mounts.remove(&self.key).unwrap_or_default() {
			state.close(mount);
			state.queues.remove(&mount);
		}
		if let Some(main) = self.main
			&& let Some(mounts) = state.mounts.get_mut(&main)
		{
			mounts.remove(&self.key);
		}
		state.close(self.key);
		state.queues.remove(&self.key);
		let key = self.key;
		state.tags.retain(|_, session| *session != key);
		state.flush(&self.node);
	}
}

impl State {
	fn register(&mut self, port: Port) -> Result<Key, Error> {
		let key = self.next_key;
		let now = self.clock.now();
		self.router.clock(now);
		self.router.connect(key, port)?;
		self.next_key += 1;
		self.queues.insert(key, kio::Shared::default());
		Ok(key)
	}

	/// The session `key` is gone for the router: what it gave goes, and what it served
	/// stays only where it left an orphan.
	fn close(&mut self, key: Key) {
		self.router.disconnect(key);
		if let Some(queue) = self.queues.get(&key) {
			let mut outbox = queue.lock();
			outbox.muted = true;
			outbox.wire.clear();
			outbox.serve.clear();
		}
		for broadcast in self.serving.remove(&key).unwrap_or_default() {
			let Some(slots) = self.lanes.get_mut(&broadcast) else {
				continue;
			};
			let gone: Vec<SlotKey> = (slots.iter())
				.filter(|((_, session), slot)| *session == key && slot.orphan.is_none())
				.map(|(slot, _)| *slot)
				.collect();
			for slot in gone {
				slots.remove(&slot);
			}
			// An orphan serves nothing, and waits for a session that does.
			for ((_, session), slot) in slots.iter_mut() {
				if *session == key {
					slot.served.available = false;
					slot.leaving = true;
				}
			}
			if slots.is_empty() {
				self.lanes.remove(&broadcast);
			}
		}
	}

	/// Queue what the router has to say, and bring what the origin serves in line with
	/// what its reader shows.
	fn flush(&mut self, node: &Node) {
		let mut sent = BTreeSet::new();
		for (to, msg) in self.router.drain() {
			let Some(queue) = self.queues.get(&to) else {
				continue;
			};
			let mut outbox = queue.lock();
			if !outbox.muted {
				outbox.wire.push_back(msg);
				sent.insert(to);
			}
		}
		for to in sent {
			let limit = self.router.held(to).saturating_mul(2).saturating_add(SLACK);
			if let Some(queue) = self.queues.get(&to)
				&& queue.read().limit != limit
			{
				queue.lock().limit = limit;
			}
		}
		let ended: BTreeSet<Broadcast> = (self.router.events().into_iter())
			.filter(|event| event.kind == Kind::End)
			.map(|event| (event.claim, event.id))
			.collect();
		let changes = self.router.changes();
		let mut dirty = changes.broadcasts;
		for origin in changes.routes {
			dirty.extend(self.router.claimed(origin).cloned());
		}
		let moved = !dirty.is_empty();
		// What the reader shows first, so the instance that replaces another at a path is
		// served before the one it replaces goes.
		let shown = |(path, instance): &Broadcast| self.router.shown().contains_key(&(instance.clone(), path.clone()));
		let (staying, going): (Vec<Broadcast>, Vec<Broadcast>) = dirty.into_iter().partition(shown);
		for broadcast in staying.into_iter().chain(going) {
			self.relane(&broadcast, ended.contains(&broadcast));
		}
		if moved {
			*node.moved.lock() += 1;
		}
	}

	/// Queue a serving change for the session `key`.
	fn serve(&self, key: Key, output: Output) {
		if let Some(queue) = self.queues.get(&key) {
			queue.lock().serve.push_back(output);
		}
	}

	/// Bring how the origin serves `broadcast` in line with the router.
	fn relane(&mut self, broadcast: &Broadcast, ended: bool) {
		let (path, instance) = broadcast;
		let shown = self.router.shown().contains_key(&(instance.clone(), path.clone()));
		let mut wanted: BTreeMap<SlotKey, Served> = BTreeMap::new();
		if shown {
			for way in self.router.ways(broadcast) {
				let (session, named) = match way.next {
					Next::Via(session) => (session, Some(way.origin)),
					Next::Local(session) => (session, None),
				};
				// The origin's own table already holds its own claims.
				if session == LOCAL || session == LOCAL_MARKED {
					continue;
				}
				let served = Served {
					attr: way.claim.item,
					cost: way.claim.cost,
					priority: way.claim.priority,
					draining: way.claim.draining,
					available: true,
					primary: way.closest,
					chain: way.claim.chain,
					marked: way.claim.marked,
					named,
				};
				wanted.insert((way.origin, session), served);
			}
		}
		let mut outputs = Vec::new();
		let slots = self.lanes.entry(broadcast.clone()).or_default();
		if shown && wanted.is_empty() {
			// No route reaches it, and the reader still shows it: what serves it stays,
			// marked so, in case one returns.
			for ((origin, session), slot) in slots.iter_mut() {
				if std::mem::replace(&mut slot.served.available, false) && slot.orphan.is_none() {
					outputs.push((*session, slot.output(*origin, broadcast)));
				}
			}
		} else {
			for slot in slots.values_mut() {
				slot.leaving = true;
			}
			for (key, served) in wanted {
				let (origin, session) = key;
				self.serving.entry(session).or_default().insert(broadcast.clone());
				match slots.get_mut(&key) {
					Some(slot) => {
						slot.leaving = false;
						if slot.served != served {
							slot.served = served;
							outputs.push((session, slot.output(origin, broadcast)));
						}
					}
					None => {
						let slot = Slot {
							served,
							confirm: Confirm::Waiting,
							leaving: false,
							orphan: None,
						};
						outputs.push((session, slot.output(origin, broadcast)));
						slots.insert(key, slot);
					}
				}
			}
		}
		for (session, output) in outputs {
			self.serve(session, output);
		}
		self.settle(broadcast, ended);
	}

	/// Release every slot of `broadcast` that is leaving, once a wanted one serves it or
	/// the reader no longer shows it. With `ended`, its publisher withdrew it.
	fn settle(&mut self, broadcast: &Broadcast, ended: bool) {
		let (path, instance) = broadcast;
		let shown = self.router.shown().contains_key(&(instance.clone(), path.clone()));
		let Some(slots) = self.lanes.get_mut(broadcast) else {
			return;
		};
		if slots.is_empty() {
			self.lanes.remove(broadcast);
			return;
		}
		let staying = || slots.values().filter(|slot| !slot.leaving);
		// Served elsewhere, or by nothing that may carry it.
		let served = staying().any(|slot| slot.confirm == Confirm::Serving)
			|| (staying().next().is_some() && staying().all(|slot| slot.confirm == Confirm::Refused));
		if shown && !served {
			return;
		}
		// Unshown, every slot leaves at once, so each tells its copy all of them go: the
		// origin then drops them together rather than showing the rest in turn.
		let gone = match (shown, ended) {
			(_, true) => Gone::Ended,
			(false, false) => Gone::Lost,
			(true, false) => Gone::Moved,
		};
		let leaving: Vec<SlotKey> = (slots.iter())
			.filter(|(_, slot)| slot.leaving)
			.map(|(key, _)| *key)
			.collect();
		let mut outputs = Vec::new();
		for key in leaving {
			let slot = slots.remove(&key).expect("listed above");
			let (origin, session) = key;
			if let Some(serving) = self.serving.get_mut(&session) {
				serving.remove(broadcast);
				if serving.is_empty() {
					self.serving.remove(&session);
				}
			}
			match slot.orphan {
				Some(orphan) => self.released.push((orphan, gone)),
				None => {
					let (path, instance) = broadcast.clone();
					outputs.push((
						session,
						Output::Unserve {
							origin,
							path,
							instance,
							gone,
						},
					));
				}
			}
		}
		// A session keeps the broadcast listed while another slot of it stays.
		for (_, session) in slots.keys() {
			self.serving.entry(*session).or_default().insert(broadcast.clone());
		}
		if slots.is_empty() {
			self.lanes.remove(broadcast);
		}
		for (session, output) in outputs {
			self.serve(session, output);
		}
	}

	/// Where a request goes ([`Node::poll_forward`]), or `None` to wait.
	fn forward(&self, from: Option<Key>, named: Option<NodeId>, broadcast: &Broadcast) -> Option<Option<Via>> {
		let Some(origin) = named.or_else(|| self.router.pick(broadcast)) else {
			return Some(None);
		};
		let Some(next) = self.router.step(origin, broadcast) else {
			return Some(None);
		};
		let (Next::Via(session) | Next::Local(session)) = next;
		let (path, instance) = broadcast;
		// The claim it follows: the most specific one covering the path.
		let claim = |above: Path| (above, instance.clone());
		// A session not filed here yet is told nothing, so any claim may serve it.
		let admits = |claim: &Broadcast| from.is_none_or(|from| self.router.admits(from, origin, claim));
		if session == LOCAL || session == LOCAL_MARKED {
			let admitted = (ancestors(path).rev()).any(|above| admits(&claim(above)));
			return Some(admitted.then_some(Via::Here));
		}
		if Some(session) == from {
			return Some(None);
		}
		let slot = (ancestors(path).rev()).find_map(|above| {
			let claim = claim(above);
			let slot = self.lanes.get(&claim)?.get(&(origin, session))?;
			Some((admits(&claim), slot))
		});
		match slot {
			Some((true, slot)) => match slot.confirm {
				Confirm::Serving if slot.served.available => Some(Some(Via::Route { node: origin })),
				Confirm::Serving | Confirm::Refused => Some(None),
				Confirm::Waiting => None,
			},
			Some((false, _)) | None => Some(None),
		}
	}
}

impl Slot {
	fn output(&self, origin: NodeId, (path, instance): &Broadcast) -> Output {
		Output::Serve {
			origin,
			path: path.clone(),
			instance: instance.clone(),
			served: self.served.clone(),
		}
	}
}

/// The lite-06 hop a relay travels as, so a relay translating a claim for a lite-06
/// peer recognizes it coming back.
pub(crate) fn hop(relay: Id) -> Option<crate::Hop> {
	std::num::NonZeroU64::new(relay).map(crate::Hop::from)
}

#[cfg(test)]
mod tests;
