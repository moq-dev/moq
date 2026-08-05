use crate::{broadcast, cache, stats, track};
use kio::Pollable;
use std::{
	cmp::Reverse,
	collections::{BTreeMap, HashMap, HashSet},
	fmt,
	sync::Arc,
	sync::atomic::{AtomicU64, Ordering},
	task::{Poll, ready},
	time::Duration,
};

use rand::RngExt;
use web_async::Lock;

use super::{Requests, WeakCache};
use crate::{
	AsPath, Error, Path, PathOwned, PathPrefixes,
	coding::{BoundsExceeded, Decode, DecodeError, Encode, EncodeError},
};

/// A relay origin, identified by a 62-bit varint on the wire.
///
/// Local origins are built with [`Origin::new`] or [`Origin::random`], both of
/// which guarantee a non-zero id so loop detection can work. Remote peers may
/// still send `0`; it is legal on the wire but cannot be used for loop detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Origin {
	/// 62-bit identifier. Encoded as a QUIC varint on the wire.
	id: u64,
}

/// Returned when a local origin id is zero or outside the 62-bit wire range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct InvalidOrigin;

impl fmt::Display for InvalidOrigin {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "local origin id must be non-zero and below 2^62")
	}
}

impl std::error::Error for InvalidOrigin {}

impl Origin {
	/// Placeholder for hop entries whose actual id is not on the wire (Lite03).
	/// Also used for remote peers that choose the legal but loop-blind id 0.
	pub(crate) const UNKNOWN: Self = Self { id: 0 };

	/// Build an origin from a stable id.
	///
	/// The id must be non-zero and fit in the 62-bit QUIC varint range. Wire
	/// decode accepts remote id 0, but local origins should not use it because
	/// downstream peers cannot exclude it for loop detection.
	pub fn new(id: u64) -> Result<Self, InvalidOrigin> {
		if id == 0 || id >= 1u64 << 62 {
			return Err(InvalidOrigin);
		}
		Ok(Self { id })
	}

	/// Generate a fresh origin with a random non-zero id. Use this for any
	/// origin that does not need a stable identity across restarts.
	///
	/// TEMPORARY: the wire format allows 62 bits, but older `@moq/lite` JS
	/// clients decode `AnnounceInterest.exclude_hop` as a u53 (number) and
	/// throw on anything > 2^53-1. To keep those clients alive against
	/// fresh relays, we cap the random id at 53 bits. Restore to 62 bits
	/// once the JS u62 fix has propagated to deployed bundles.
	pub fn random() -> Self {
		let mut rng = rand::rng();
		let id = rng.random_range(1..(1u64 << 53));
		Self { id }
	}

	/// Return the origin's wire id.
	pub fn id(self) -> u64 {
		self.id
	}

	/// Consume this [Origin] to create a producer that carries its id, with an
	/// unbounded cache pool. Use [`Info::produce`] to configure the pool.
	pub fn produce(self) -> Producer {
		Info::new(self).produce()
	}
}

/// An origin's identity plus the cache pool its broadcasts inherit.
///
/// Doubles as the construction config for an [origin `Producer`](Producer) and as the
/// parent handle every broadcast carries ([`broadcast::Info::origin`]): the origin owns
/// the [`cache::Pool`] every group in the tree charges into, so a relay configures one
/// bounded pool here and every broadcast, track, and group beneath it reaches that single
/// budget by walking up the ownership chain. Defaults to an unbounded pool
/// ([`Origin::produce`] is the shorthand for that). Cheap to clone (a `Copy` id plus an
/// `Arc`-handle bump), so it's stored by value rather than behind another `Arc`.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Info {
	/// The origin's wire identity, appended to broadcast hop chains for loop
	/// detection and shortest-path routing.
	pub id: Origin,

	/// The cache pool broadcasts under this origin charge their groups into. It flows
	/// down the ownership chain (origin -> broadcast -> track -> group): a track opens
	/// an account against it, and its groups charge through that. Unbounded by
	/// default; a relay sets a bounded one (via [`Self::with_pool`]) so cached groups
	/// across the whole process share one memory budget.
	pub pool: cache::Pool,

	/// Ceiling on how long any non-latest group under this origin is retained. Each
	/// track's own [`latency_max`](track::Info::latency_max) window is clamped down to
	/// this when the track binds, so a group is never held longer than this regardless
	/// of what a publisher advertises. The age budget alongside [`Self::pool`]'s byte
	/// budget: a relay bounds memory by both. [`Duration::MAX`] (the default) imposes no
	/// ceiling, leaving each track's own window in force.
	pub cache_duration: Duration,

	/// The retention window given to a track whose publisher advertises none.
	///
	/// moq-lite 05+ carries [`latency_max`](track::Info::latency_max) in TRACK_INFO, so a
	/// track relayed over it keeps the window its publisher chose. Every moq-transport
	/// draft and moq-lite 01-04 have no such wire property, so a track arriving over one
	/// of them lands here instead. Raise it on a relay fronting a segmented egress
	/// (HLS/DASH), which needs a playlist window's worth of history rather than the live
	/// edge. Defaults to [`track::DEFAULT_LATENCY_MAX`], and [`Self::cache_duration`]
	/// still caps it.
	pub latency_default: Duration,

	/// How long a broadcast under this origin outlives the *ungraceful* loss of its
	/// last source before closing. Within the window the path stays announced and a
	/// source re-attaching at it (a session reconnecting, a publisher re-announcing)
	/// splices in seamlessly, so consumers never observe the gap. A source that ends
	/// deliberately ([`broadcast::Producer::finish`], a clean unannounce from a peer)
	/// closes the broadcast immediately regardless. Zero (the default) closes
	/// immediately either way; a duration too large to represent as a deadline
	/// (e.g. [`Duration::MAX`]) lingers indefinitely.
	pub linger: Duration,
}

impl Default for Info {
	/// An unknown origin (id `0`, no loop detection) with an unbounded pool. This is
	/// what a standalone broadcast (no relay origin) inherits.
	fn default() -> Self {
		Self {
			id: Origin::UNKNOWN,
			pool: cache::Pool::default(),
			cache_duration: Duration::MAX,
			latency_default: track::DEFAULT_LATENCY_MAX,
			linger: Duration::ZERO,
		}
	}
}

impl Info {
	/// Config for the given origin id with an unbounded cache pool.
	pub fn new(id: Origin) -> Self {
		Self { id, ..Self::default() }
	}

	/// Set the cache pool this origin's broadcasts inherit, returning `self` for chaining.
	pub fn with_pool(mut self, pool: cache::Pool) -> Self {
		self.pool = pool;
		self
	}

	/// Set the retention ceiling (see [`Self::cache_duration`]) applied to every track
	/// under this origin, returning `self` for chaining.
	pub fn with_cache_duration(mut self, cache_duration: Duration) -> Self {
		self.cache_duration = cache_duration;
		self
	}

	/// Set the retention window (see [`Self::latency_default`]) used for tracks whose
	/// publisher advertises none, returning `self` for chaining.
	pub fn with_latency_default(mut self, latency_default: Duration) -> Self {
		self.latency_default = latency_default;
		self
	}

	/// Set how long a broadcast survives ungracefully losing its last source (see
	/// [`Self::linger`]), returning `self` for chaining.
	pub fn with_linger(mut self, linger: Duration) -> Self {
		self.linger = linger;
		self
	}

	/// Consume this config to create an origin [`Producer`].
	pub fn produce(self) -> Producer {
		Producer::new(self)
	}
}

impl TryFrom<u64> for Origin {
	type Error = InvalidOrigin;

	fn try_from(id: u64) -> Result<Self, Self::Error> {
		Self::new(id)
	}
}

impl fmt::Display for Origin {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		self.id.fmt(f)
	}
}

impl<V: Copy> Encode<V> for Origin
where
	u64: Encode<V>,
{
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: V) -> Result<(), EncodeError> {
		self.id.encode(w, version)
	}
}

impl<V: Copy> Decode<V> for Origin
where
	u64: Decode<V>,
{
	fn decode<R: bytes::Buf>(r: &mut R, version: V) -> Result<Self, DecodeError> {
		let id = u64::decode(r, version)?;
		if id >= 1u64 << 62 {
			return Err(DecodeError::InvalidValue);
		}
		Ok(Self { id })
	}
}

/// Maximum number of origins (hops) an [`OriginList`] can hold.
///
/// Caps pathological or loop-induced announcements at a reasonable cluster
/// diameter; appending past this limit returns [`TooManyOrigins`] rather than
/// silently truncating.
pub(crate) const MAX_HOPS: usize = 32;

/// Bounded list of [`Origin`] entries, typically the hop chain of a broadcast.
///
/// Guarantees `len() <= MAX_HOPS`. Construct via [`OriginList::new`] +
/// [`OriginList::push`], or fall back to the fallible [`TryFrom<Vec<Origin>>`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OriginList(Vec<Origin>);

/// Returned when an operation would grow an [`OriginList`] past its hop-count cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct TooManyOrigins;

impl fmt::Display for TooManyOrigins {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "too many origins (max {MAX_HOPS})")
	}
}

impl std::error::Error for TooManyOrigins {}

impl From<TooManyOrigins> for DecodeError {
	fn from(_: TooManyOrigins) -> Self {
		DecodeError::BoundsExceeded
	}
}

impl OriginList {
	/// Create an empty list.
	pub fn new() -> Self {
		Self(Vec::new())
	}

	/// Append an [`Origin`]. Returns [`TooManyOrigins`] if the list is full.
	pub fn push(&mut self, origin: Origin) -> Result<(), TooManyOrigins> {
		if self.0.len() >= MAX_HOPS {
			return Err(TooManyOrigins);
		}
		self.0.push(origin);
		Ok(())
	}

	/// Replace the first entry equal to `target` with `replacement`, returning
	/// true if a match was found. The length is unchanged.
	pub fn replace_first(&mut self, target: Origin, replacement: Origin) -> bool {
		for entry in &mut self.0 {
			if *entry == target {
				*entry = replacement;
				return true;
			}
		}
		false
	}

	/// Returns true if any entry matches `origin`.
	pub fn contains(&self, origin: &Origin) -> bool {
		self.0.contains(origin)
	}

	/// Number of entries currently in the list (always `<= MAX_HOPS`).
	pub fn len(&self) -> usize {
		self.0.len()
	}

	/// Whether the list contains no entries.
	pub fn is_empty(&self) -> bool {
		self.0.is_empty()
	}

	/// Iterate over the entries in hop order (oldest first).
	pub fn iter(&self) -> std::slice::Iter<'_, Origin> {
		self.0.iter()
	}

	/// Borrow the entries as a slice.
	pub fn as_slice(&self) -> &[Origin] {
		&self.0
	}
}

impl TryFrom<Vec<Origin>> for OriginList {
	type Error = TooManyOrigins;

	fn try_from(v: Vec<Origin>) -> Result<Self, Self::Error> {
		if v.len() > MAX_HOPS {
			return Err(TooManyOrigins);
		}
		Ok(Self(v))
	}
}

impl<'a> IntoIterator for &'a OriginList {
	type Item = &'a Origin;
	type IntoIter = std::slice::Iter<'a, Origin>;

	fn into_iter(self) -> Self::IntoIter {
		self.iter()
	}
}

impl<V: Copy> Encode<V> for OriginList
where
	u64: Encode<V>,
	Origin: Encode<V>,
{
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: V) -> Result<(), EncodeError> {
		(self.0.len() as u64).encode(w, version)?;
		for origin in &self.0 {
			origin.encode(w, version)?;
		}
		Ok(())
	}
}

impl<V: Copy> Decode<V> for OriginList
where
	u64: Decode<V>,
	Origin: Decode<V>,
{
	fn decode<R: bytes::Buf>(r: &mut R, version: V) -> Result<Self, DecodeError> {
		let count = u64::decode(r, version)? as usize;
		if count > MAX_HOPS {
			return Err(DecodeError::BoundsExceeded);
		}
		let mut list = Vec::with_capacity(count);
		for _ in 0..count {
			list.push(Origin::decode(r, version)?);
		}
		Ok(Self(list))
	}
}

static NEXT_CONSUMER_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct ConsumerId(u64);

impl ConsumerId {
	fn new() -> Self {
		Self(NEXT_CONSUMER_ID.fetch_add(1, Ordering::Relaxed))
	}
}

// The origin-owned broadcast at a leaf: the spliced broadcast consumers see,
// the table of sources feeding it, and whether the path is currently announced.
// `announced` is gated on the best source's `live` flag; a non-announced entry
// is still returned by lookups, so an offline broadcast stays reachable for
// subscribes and fetches.
struct OriginBroadcast {
	path: PathOwned,
	/// The shared, spliced broadcast; its `consume()` is what consumers get.
	broadcast: broadcast::Producer,
	/// The source table, shared with every source watcher and the front task.
	/// Also the broadcast's identity for stale-teardown checks.
	state: kio::Producer<FrontState>,
	announced: bool,
}

/// Ordering key used to pick the active route among broadcasts at the same path.
///
/// Lower wins. Shorter hop chains sort first (routing prefers the shortest path);
/// remaining ties break on a deterministic hash of the broadcast name and hop
/// chain. Every node in the cluster, given the same candidate routes, converges
/// on the same winner: the hops are forwarded unchanged, and the hash is
/// build-stable. Mixing the name in spreads equal routes across different
/// upstreams rather than funneling onto one.
fn route_key(name: &Path, hops: &OriginList) -> (usize, u64) {
	(hops.len(), fnv_key(name, hops.iter().copied()))
}

/// FNV-1a over the broadcast name and a sequence of origin ids.
///
/// FNV-1a, not the std hasher: its output is fixed across Rust versions and
/// builds, which matters when nodes run mismatched binaries during a rolling
/// deploy and still need to agree on the same route. SEED is a custom basis
/// (any nonzero u64 works, the textbook one is just as arbitrary); FNV_PRIME is
/// the standard FNV-64 prime and should stay put.
///
/// Two callers, two different id sequences: [`route_key`] hashes a route's hop
/// chain to pick among *routes*, and [`FrontState::handover_allowed`] hashes a
/// single relay's origin to pick among *relays*. Mixing the name in spreads
/// equal candidates across different winners rather than funneling onto one.
fn fnv_key(name: &Path, origins: impl IntoIterator<Item = Origin>) -> u64 {
	const SEED: u64 = 0x420C0DECB00B; // 420 C0DEC B00B
	const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

	let mut hash = SEED;
	for &byte in name.as_str().as_bytes() {
		hash = (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME);
	}
	for origin in origins {
		for &byte in &origin.id().to_le_bytes() {
			hash = (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME);
		}
	}

	hash
}

/// Full ordering key for an attached route: announced routes first (an actively
/// published source beats an offline one), then the marginal cost of pulling via
/// the route, then the [`route_key`] hop ordering, and finally the newest source
/// to attach. Lower wins.
///
/// Hop length stays the tie-break below cost, so peers that never carry a cost
/// (pre-lite-06, or a plain local publish) rank exactly as they did before route
/// cost existed, and equal-cost warm copies resolve to the closest one, which
/// bounds same-datacenter chains to a single hop.
///
/// Recency is the last word, so it only separates routes that are identical in
/// every advertised respect: same hop chain, same cost. That is a publisher
/// reconnecting over a fresh session while its old one is still being kept alive
/// by the transport, and the new session is the one actually carrying frames, so
/// it wins the moment it attaches instead of after the QUIC idle timeout finally
/// retires the corpse. Local attach order never leaks into cluster convergence:
/// routes it can reorder are indistinguishable downstream, since what is
/// forwarded is the chain and cost, which are equal by construction here.
fn route_order(name: &Path, route: &FrontRoute) -> (bool, u64, usize, u64, Reverse<u64>) {
	let (len, hash) = route_key(name, &route.route.hops);
	(!route.route.announce, route.route.cost, len, hash, Reverse(route.id))
}

/// One coalesced update queued for an `AnnounceConsumer`.
///
/// At most one entry exists per path, so a slow consumer's pending set is bounded
/// by the number of distinct paths. `UnannounceAnnounce` preserves the signal
/// that a broadcast genuinely went away and a different one took its place (the
/// consumer must see the `None` before the `Some`), while a stale `Announce`
/// cancels with a subsequent `unannounce` because the consumer has not yet
/// observed it.
enum PendingUpdate {
	Announce(broadcast::Consumer),
	Unannounce,
	UnannounceAnnounce(broadcast::Consumer),
}

/// Pending updates keyed by path. `BTreeMap` keeps memory strictly bounded by
/// the number of distinct paths with outstanding work (collapsed pairs are
/// fully erased) and gives a deterministic lexicographic delivery order so
/// tests can predict it.
#[derive(Default)]
struct OriginConsumerState {
	pending: BTreeMap<PathOwned, PendingUpdate>,
}

impl OriginConsumerState {
	fn apply_announce(&mut self, path: PathOwned, broadcast: broadcast::Consumer) {
		let new = match self.pending.remove(&path) {
			// First announce, or a stale announce being replaced.
			None | Some(PendingUpdate::Announce(_)) => PendingUpdate::Announce(broadcast),
			// Consumer needs to observe the unannounce before this announce.
			Some(PendingUpdate::Unannounce | PendingUpdate::UnannounceAnnounce(_)) => {
				PendingUpdate::UnannounceAnnounce(broadcast)
			}
		};
		self.pending.insert(path, new);
	}

	fn apply_unannounce(&mut self, path: PathOwned) {
		match self.pending.remove(&path) {
			// Consumer has not seen the pending announce; drop both entirely.
			Some(PendingUpdate::Announce(_)) => {}
			None | Some(PendingUpdate::Unannounce) => {
				self.pending.insert(path, PendingUpdate::Unannounce);
			}
			// The embedded announce cancels with this unannounce; the consumer still
			// needs the leading unannounce.
			Some(PendingUpdate::UnannounceAnnounce(_)) => {
				self.pending.insert(path, PendingUpdate::Unannounce);
			}
		}
	}

	/// Take one update to deliver to the consumer, if any.
	fn take(&mut self) -> Option<OriginAnnounce> {
		let path = self.pending.keys().next()?.clone();
		let broadcast = match self.pending.remove(&path).unwrap() {
			PendingUpdate::Announce(broadcast) => Some(broadcast),
			PendingUpdate::Unannounce => None,
			PendingUpdate::UnannounceAnnounce(broadcast) => {
				// Deliver the unannounce now; leave the trailing announce pending so
				// the next take returns it for the same path.
				self.pending.insert(path.clone(), PendingUpdate::Announce(broadcast));
				None
			}
		};
		Some(OriginAnnounce { path, broadcast })
	}
}

#[derive(Clone)]
struct AnnounceConsumerNotify {
	root: PathOwned,
	state: kio::Producer<OriginConsumerState>,
}

impl AnnounceConsumerNotify {
	fn announce(&self, path: impl AsPath, broadcast: broadcast::Consumer) {
		let path = path.as_path().strip_prefix(&self.root).unwrap().to_owned();
		self.state
			.write()
			.ok()
			.expect("consumer closed")
			.apply_announce(path, broadcast);
	}

	fn unannounce(&self, path: impl AsPath) {
		let path = path.as_path().strip_prefix(&self.root).unwrap().to_owned();
		self.state.write().ok().expect("consumer closed").apply_unannounce(path);
	}
}

struct NotifyNode {
	parent: Option<Lock<NotifyNode>>,

	// Consumers that are subscribed to this node.
	// We store a consumer ID so we can remove it easily when it closes.
	consumers: HashMap<ConsumerId, AnnounceConsumerNotify>,
}

impl NotifyNode {
	fn new(parent: Option<Lock<NotifyNode>>) -> Self {
		Self {
			parent,
			consumers: HashMap::new(),
		}
	}

	fn announce(&mut self, path: impl AsPath, broadcast: &broadcast::Consumer) {
		for consumer in self.consumers.values() {
			consumer.announce(path.as_path(), broadcast.clone());
		}

		if let Some(parent) = &self.parent {
			parent.lock().announce(path, broadcast);
		}
	}

	fn unannounce(&mut self, path: impl AsPath) {
		for consumer in self.consumers.values() {
			consumer.unannounce(path.as_path());
		}

		if let Some(parent) = &self.parent {
			parent.lock().unannounce(path);
		}
	}
}

/// Keeps a peer registered in [`FrontState::excluded`] for as long as it holds the
/// shared front, so the front stays off routes that flow back through it.
///
/// Carried by the [`broadcast::Consumer`] handed to that peer and shared by its
/// clones, so the registration ends when the last of them drops. A guard whose
/// front has already closed is inert.
pub(crate) struct ExclusionGuard {
	state: kio::Producer<FrontState>,
	peer: Origin,
}

impl ExclusionGuard {
	/// Register `peer` and return the guard that releases it, or `None` if the
	/// front is closing (nothing left to keep off a route).
	fn new(state: &kio::Producer<FrontState>, peer: Origin) -> Option<Arc<Self>> {
		let mut s = state.write().ok()?;
		if s.closed {
			return None;
		}
		*s.excluded.entry(peer).or_default() += 1;
		drop(s);
		Some(Arc::new(Self {
			state: state.clone(),
			peer,
		}))
	}
}

impl Drop for ExclusionGuard {
	fn drop(&mut self) {
		let Ok(mut state) = self.state.write() else { return };
		if let std::collections::hash_map::Entry::Occupied(mut entry) = state.excluded.entry(self.peer) {
			match entry.get() {
				1 => drop(entry.remove()),
				n => *entry.get_mut() = n - 1,
			}
		}
	}
}

/// How a path resolves against the announce tree for one consumer.
///
/// `Excluded` is deliberately not folded into `Missing`: they mean opposite
/// things to a caller holding a dynamic handler. Missing is "nobody here, go
/// ask"; excluded is "here, but not for you", and asking a handler to route it
/// anyway is how a split-horizon violation gets in through the back door.
enum Resolved {
	/// A broadcast this consumer may read: the shared front, or a single source
	/// pinned because the front holds a route back through the requester.
	Found(broadcast::Consumer),
	/// The path is live, but every route to it flows through the requester.
	Excluded,
	/// Nothing is published at the path, or it is outside the consumer's scope.
	Missing,
}

struct OriginNode {
	// The origin-owned broadcast published at this node, if any (see
	// [`Producer::create_broadcast`]).
	broadcast: Option<OriginBroadcast>,

	// Nested nodes, one level down the tree.
	nested: HashMap<String, Lock<OriginNode>>,

	// Unfortunately, to notify consumers we need to traverse back up the tree.
	notify: Lock<NotifyNode>,
}

impl OriginNode {
	fn new(parent: Option<Lock<NotifyNode>>) -> Self {
		Self {
			broadcast: None,
			nested: HashMap::new(),
			notify: Lock::new(NotifyNode::new(parent)),
		}
	}

	fn leaf(&mut self, path: &Path) -> Lock<OriginNode> {
		let (dir, rest) = path.next_part().expect("leaf called with empty path");

		let next = self.entry(dir);
		if rest.is_empty() { next } else { next.lock().leaf(&rest) }
	}

	fn entry(&mut self, dir: &str) -> Lock<OriginNode> {
		match self.nested.get(dir) {
			Some(next) => next.clone(),
			None => {
				let next = Lock::new(OriginNode::new(Some(self.notify.clone())));
				self.nested.insert(dir.to_string(), next.clone());
				next
			}
		}
	}

	/// Toggle the announce state of this leaf's broadcast, notifying consumers on
	/// a change. The identity check keeps a stale front from toggling its
	/// successor.
	fn set_announced(&mut self, expect: &kio::Producer<FrontState>, announce: bool) {
		let Some(existing) = &mut self.broadcast else { return };
		if !existing.state.same_channel(expect) || existing.announced == announce {
			return;
		}
		existing.announced = announce;
		let path = existing.path.clone();
		let consumer = existing.broadcast.consume();
		let mut notify = self.notify.lock();
		if announce {
			notify.announce(&path, &consumer);
		} else {
			notify.unannounce(&path);
		}
	}

	fn consume(&mut self, id: ConsumerId, mut notify: AnnounceConsumerNotify) {
		self.consume_initial(&mut notify);
		self.notify.lock().consumers.insert(id, notify);
	}

	fn consume_initial(&mut self, notify: &mut AnnounceConsumerNotify) {
		// Only announced (live) broadcasts replay; offline ones are reachable by
		// exact path but never advertised.
		if let Some(broadcast) = &self.broadcast
			&& broadcast.announced
		{
			notify.announce(&broadcast.path, broadcast.broadcast.consume());
		}

		// Recursively subscribe to all nested nodes.
		for nested in self.nested.values() {
			nested.lock().consume_initial(notify);
		}
	}

	fn resolve_broadcast(&self, rest: impl AsPath, exclude: Option<Origin>) -> Resolved {
		let rest = rest.as_path();

		if let Some((dir, rest)) = rest.next_part() {
			let Some(node) = self.nested.get(dir) else {
				return Resolved::Missing;
			};
			let node = node.lock();
			return node.resolve_broadcast(&rest, exclude);
		}

		let Some(broadcast) = self.broadcast.as_ref() else {
			return Resolved::Missing;
		};
		let Some(origin) = exclude else {
			return Resolved::Found(broadcast.broadcast.consume());
		};

		// Data-plane split horizon: never serve a requester from a source whose
		// chain flows through them (they'd receive their own bytes back, or worse,
		// a subscription cycle).
		//
		// The shared spliced front is safe only while *no* attached route is
		// tainted for the requester: the front picks per track and re-picks on
		// failover, so a tainted route anywhere in the table is one the front may
		// serve them from. Checking the whole table rather than just the active
		// route is what keeps that honest at this instant; the guard registered
		// below keeps it honest afterwards, holding the front off a route through
		// this peer for as long as they are reading it. Otherwise pin them to the
		// best clean source. A pinned broadcast skips the front's re-splicing,
		// which is fine: the requester is itself a relay (only a forwarder's origin
		// can appear in a chain) and re-splices via its own front when the pinned
		// source dies.
		let state = broadcast.state.read();
		if !state.routes.iter().any(|r| r.route.hops.contains(&origin)) {
			drop(state);
			let shared = broadcast.broadcast.consume();
			return match ExclusionGuard::new(&broadcast.state, origin) {
				Some(guard) => Resolved::Found(shared.with_exclusion(guard)),
				// The front closed between the lookup and the registration; it
				// serves nothing now, so there is nothing to keep off a route.
				None => Resolved::Found(shared),
			};
		}
		match state
			.dispatch(Some(origin))
			.and_then(|clean| state.routes.iter().find(|r| r.id == clean))
		{
			Some(route) => Resolved::Found(route.source.clone()),
			None => Resolved::Excluded,
		}
	}

	fn unconsume(&mut self, id: ConsumerId) {
		self.notify.lock().consumers.remove(&id).expect("consumer not found");
		if self.is_empty() {
			//tracing::warn!("TODO: empty node; memory leak");
			// This happens when consuming a path that is not being broadcasted.
		}
	}

	/// Remove the broadcast at `relative` if it is `expect`, unannouncing it if
	/// needed and pruning empty nodes on the way back up. The identity check
	/// keeps a stale teardown from clobbering a replacement.
	fn remove(&mut self, expect: &kio::Producer<FrontState>, relative: impl AsPath) {
		let relative = relative.as_path();

		if let Some((dir, relative)) = relative.next_part() {
			let Some(nested) = self.nested.get(dir) else { return };
			let nested = nested.clone();
			let mut locked = nested.lock();
			locked.remove(expect, &relative);

			if locked.is_empty() {
				drop(locked);
				self.nested.remove(dir);
			}
		} else if let Some(existing) = &self.broadcast
			&& existing.state.same_channel(expect)
		{
			let existing = self.broadcast.take().expect("checked above");
			if existing.announced {
				self.notify.lock().unannounce(&existing.path);
			}
		}
	}

	fn is_empty(&self) -> bool {
		self.broadcast.is_none() && self.nested.is_empty() && self.notify.lock().consumers.is_empty()
	}
}

#[derive(Clone)]
struct OriginNodes {
	nodes: Vec<(PathOwned, Lock<OriginNode>)>,
}

impl OriginNodes {
	// Returns nested roots that match the prefixes.
	// PathPrefixes guarantees no duplicates or overlapping prefixes.
	pub fn select(&self, prefixes: &PathPrefixes) -> Option<Self> {
		let mut roots = Vec::new();

		for (root, state) in &self.nodes {
			for prefix in prefixes {
				if root.has_prefix(prefix) {
					// Keep the existing node if we're allowed to access it.
					roots.push((root.to_owned(), state.clone()));
					continue;
				}

				if let Some(suffix) = prefix.strip_prefix(root) {
					// If the requested prefix is larger than the allowed prefix, then we further scope it.
					let nested = state.lock().leaf(&suffix);
					roots.push((prefix.to_owned(), nested));
				}
			}
		}

		if roots.is_empty() {
			None
		} else {
			Some(Self { nodes: roots })
		}
	}

	pub fn root(&self, new_root: impl AsPath) -> Option<Self> {
		let new_root = new_root.as_path();
		let mut roots = Vec::new();

		if new_root.is_empty() {
			return Some(self.clone());
		}

		for (root, state) in &self.nodes {
			if let Some(suffix) = root.strip_prefix(&new_root) {
				// If the old root is longer than the new root, shorten the keys.
				roots.push((suffix.to_owned(), state.clone()));
			} else if let Some(suffix) = new_root.strip_prefix(root) {
				// If the new root is longer than the old root, add a new root.
				// NOTE: suffix can't be empty
				let nested = state.lock().leaf(&suffix);
				roots.push(("".into(), nested));
			}
		}

		if roots.is_empty() {
			None
		} else {
			Some(Self { nodes: roots })
		}
	}

	// Returns the root that has this prefix.
	pub fn get(&self, path: impl AsPath) -> Option<(Lock<OriginNode>, PathOwned)> {
		let path = path.as_path();

		for (root, state) in &self.nodes {
			if let Some(suffix) = path.strip_prefix(root) {
				return Some((state.clone(), suffix.to_owned()));
			}
		}

		None
	}
}

impl Default for OriginNodes {
	fn default() -> Self {
		Self {
			nodes: vec![("".into(), Lock::new(OriginNode::new(None)))],
		}
	}
}

/// A path and the broadcast now available there, delivered by [`AnnounceConsumer`].
#[derive(Clone)]
pub struct OriginAnnounce {
	/// The path of the broadcast, relative to the consuming cursor's root.
	pub path: PathOwned,
	/// The broadcast now available at that path, or `None` if it is no longer available.
	///
	/// A replacement (a relay failover, or a shorter hop path arriving) is delivered as a
	/// `None` followed by a `Some`, never as a swap in place. A route change alone is invisible here (the handles stay
	/// valid); observe it via [`broadcast::Consumer::route_changed`].
	pub broadcast: Option<broadcast::Consumer>,
}

/// Announces broadcasts to consumers over the network.
#[derive(Clone)]
pub struct Producer {
	// Identity for this origin. Appended to broadcast hops when
	// re-announcing so downstream relays can detect loops and prefer the
	// shortest path.
	info: Origin,

	// The roots of the tree that we are allowed to publish.
	// A path of "" means we can publish anything.
	nodes: OriginNodes,

	// The prefix that is automatically stripped from all paths.
	root: PathOwned,

	// Fallback request queue, shared with every derived consumer. Separate from
	// `nodes` because dynamic broadcasts are never announced: they only resolve a
	// consumer's `request_broadcast` when no live announcement exists.
	dynamic: kio::Shared<OriginDynamicState>,

	// The cache pool inherited by broadcasts created under this origin (sessions
	// mint their remote broadcasts with it). Unbounded by default.
	pool: cache::Pool,

	// Retention ceiling inherited by broadcasts created under this origin (see
	// [`Info::cache_duration`]). `Duration::MAX` (no ceiling) by default.
	cache_duration: Duration,

	// Retention window for a track whose publisher advertises none (see
	// [`Info::latency_default`]).
	latency_default: Duration,

	// How long a broadcast outlives ungracefully losing its last source (see
	// [`Info::linger`]). Zero by default.
	linger: Duration,

	// Ingress stats context. Broadcasts created through this producer are attributed
	// to it (writes counted on the subscriber/ingress side). Empty (no-op) unless a
	// session tagged this handle via [`Self::with_stats`].
	stats: stats::Session,
}

impl std::ops::Deref for Producer {
	type Target = Origin;

	fn deref(&self) -> &Self::Target {
		&self.info
	}
}

impl Producer {
	/// Build a producer from an [`Info`] (identity + cache pool) with no scoped
	/// prefix and no pre-existing broadcasts. Prefer [`Info::produce`] /
	/// [`Origin::produce`].
	pub fn new(info: Info) -> Self {
		Self {
			info: info.id,
			nodes: OriginNodes::default(),
			root: PathOwned::default(),
			dynamic: kio::Shared::default(),
			pool: info.pool,
			cache_duration: info.cache_duration,
			latency_default: info.latency_default,
			linger: info.linger,
			stats: stats::Session::default(),
		}
	}

	/// Attach an ingress stats context: broadcasts created through this handle (and
	/// any handle derived from it) are attributed to `session` on the subscriber
	/// (ingress) side. Pass [`stats::Session::default`] to opt out.
	pub fn with_stats(mut self, session: stats::Session) -> Self {
		self.stats = session;
		self
	}

	/// Set the linger (see [`Info::linger`]) for broadcasts created through this
	/// handle and any handle derived from it.
	///
	/// A broadcast adopts the window of the handle whose source *created* it (the
	/// first source at the path), so set this before handing the producer to
	/// whatever attaches sources through it (e.g. a session). Lets one supplier
	/// declare its own recovery promise, like a reconnecting client lingering for
	/// as long as its retry loop keeps trying, without reconfiguring the origin.
	pub fn with_linger(mut self, linger: Duration) -> Self {
		self.linger = linger;
		self
	}

	/// This origin's [`Info`] (identity + cache pool), the parent handle a broadcast
	/// created under this origin carries (see [`broadcast::Info::origin`]).
	pub fn info(&self) -> Info {
		Info {
			id: self.info,
			pool: self.pool.clone(),
			cache_duration: self.cache_duration,
			latency_default: self.latency_default,
			linger: self.linger,
		}
	}

	// The retention window for a track whose publisher advertises none (see
	// [`Info::latency_default`]). Cheaper than `info()`, which clones the pool.
	pub(crate) fn latency_default(&self) -> Duration {
		self.latency_default
	}

	/// A producer with *no* allowed prefixes: it can't publish anything and
	/// advertises no subscribe interest (its `allowed()` is empty, so the
	/// subscriber issues no ANNOUNCE_PLEASE). Used to fill an unset session half
	/// so both the publisher and subscriber loops still run.
	pub(crate) fn empty(info: Origin) -> Self {
		Self {
			info,
			nodes: OriginNodes { nodes: Vec::new() },
			root: PathOwned::default(),
			dynamic: kio::Shared::default(),
			pool: cache::Pool::default(),
			cache_duration: Duration::MAX,
			latency_default: track::DEFAULT_LATENCY_MAX,
			linger: Duration::ZERO,
			stats: stats::Session::default(),
		}
	}

	/// Create a broadcast at `path`, fed through the returned producer.
	///
	/// This is the sole way content enters an origin. The returned
	/// [`broadcast::Producer`] is a route source: the origin owns the broadcast
	/// consumers actually see, and splices its tracks across every source created
	/// at the same path (other local publishers, or sessions attaching announces
	/// from the network), always serving from the best [`broadcast::Route`] (live
	/// first, then lowest cost, then shortest hops with a deterministic
	/// tie-break, and the newest source among otherwise equal routes). When the
	/// best source changes, tracks resume from the replacement at the first
	/// missing group; consumers never observe the swap.
	///
	/// Splicing requires the same content identity: every source at a path must
	/// share the first hop of its route, which is a promise that they produce
	/// interchangeable tracks. An *announced* source arriving with a different
	/// first hop is a replacement instead: it takes the path immediately and
	/// consumers see an unannounce followed by an announce, rather than unrelated
	/// content spliced into a live subscription. So a publisher reconnecting
	/// under a fresh identity displaces the session it replaced right away,
	/// without waiting for the transport to notice the old one is gone. An
	/// offline source never displaces anything: it ranks below every announced
	/// route, so it waits invisibly for the incumbent to end.
	///
	/// `route` is the source's initial metadata; update it with
	/// [`broadcast::Producer::set_route`]. The [`broadcast::Route::announce`] flag
	/// controls whether the path is announced: a non-live broadcast is invisible
	/// to [`Consumer::announced`] but stays reachable by exact path for
	/// subscribes and fetches (e.g. serving cached or on-demand content), so
	/// toggling `live` announces or unannounces without touching the broadcast.
	///
	/// The broadcast becomes visible to consumers asynchronously, shortly after
	/// this returns. Create tracks and register a
	/// [`broadcast::Producer::dynamic`] handler before awaiting, so the first
	/// consumer finds them.
	///
	/// End the broadcast with [`broadcast::Producer::finish`]; dropping it
	/// without finishing also works, but logs a warning. A finish closes and
	/// unannounces the path immediately once it was the last source. An unfinished
	/// drop is treated as an outage: the path survives for the origin's
	/// [`Info::linger`] (zero by default), so a replacement source attaching within
	/// that window splices in without consumers noticing.
	///
	/// Fails with [`Error::Unauthorized`] if `path` is outside the prefixes this
	/// producer may publish under (after [`scope`](Self::scope) /
	/// [`with_root`](Self::with_root)), or [`Error::BoundsExceeded`] if the full
	/// rooted path exceeds [`Path::MAX_PARTS`]. Must be called with a runtime
	/// available (it spawns the broadcast's lifecycle task). Callers must not use
	/// a route whose hop chain contains this origin's id (it would form a routing
	/// loop); relays filter such reflections before they reach here, checked by a
	/// `debug_assert`.
	pub fn create_broadcast(&self, path: impl AsPath, route: broadcast::Route) -> Result<broadcast::Producer, Error> {
		let path = path.as_path();

		debug_assert!(
			!route.hops.contains(&self.info),
			"create_broadcast called with a looping hop chain",
		);

		let (node, rest) = self.nodes.get(&path).ok_or(Error::Unauthorized)?;
		let full = self.root.join(&path).to_owned();

		// A decoded announce prefix and suffix are each within the wire limit, but their
		// join might not be. Enforcing here bounds the tree depth and guarantees the path
		// can be re-encoded when forwarded.
		if full.parts().count() > Path::MAX_PARTS {
			return Err(BoundsExceeded.into());
		}

		// Resolve the ingress counters once, keyed by the absolute broadcast path.
		// The source producer tags its tracks; run_source drives the announce guard
		// off route transitions.
		let ingress = self.stats.ingress(&full);

		let mut source = broadcast::Info { origin: self.info() }
			.produce()
			.with_stats(ingress.clone());
		source.set_route(route).expect("fresh producer");

		web_async::spawn(run_source(self.info(), node, full, rest, source.consume(), ingress));

		Ok(source)
	}

	/// Returns a new Producer restricted to publishing under one of `prefixes`.
	///
	/// Returns None if there are no legal prefixes (the requested prefixes are
	/// disjoint from this producer's current scope).
	// TODO accept PathPrefixes instead of &[Path]
	pub fn scope(&self, prefixes: &[Path]) -> Option<Producer> {
		let prefixes = PathPrefixes::new(prefixes);
		Some(Producer {
			info: self.info,
			nodes: self.nodes.select(&prefixes)?,
			root: self.root.clone(),
			dynamic: self.dynamic.clone(),
			pool: self.pool.clone(),
			cache_duration: self.cache_duration,
			latency_default: self.latency_default,
			linger: self.linger,
			stats: self.stats.clone(),
		})
	}

	/// Create a dynamic handler that picks up [`Consumer::request_broadcast`]
	/// calls for paths that are not announced.
	///
	/// This is the origin-level analogue of [`broadcast::Producer::dynamic`]: it serves
	/// broadcasts on demand rather than tracks. Crucially the served broadcasts are
	/// *not* announced, so [`Consumer::announced`] never sees them; they exist
	/// only as a fallback for a consumer that asks for an exact path with no live
	/// announcement. Drop the handler (and every clone) to reject pending requests.
	pub fn dynamic(&self) -> Dynamic {
		Dynamic::new(self.info, self.root.clone(), self.dynamic.clone())
	}

	/// Cheap read handle over this origin's broadcast tree.
	///
	/// Use [`Consumer::announced`] to register interest and start receiving
	/// announcement events; the consumer itself does not allocate any channels.
	pub fn consume(&self) -> Consumer {
		// Untagged: a session tags the egress consumer separately via
		// `origin::Consumer::with_stats` (ingress and egress are distinct sides).
		Consumer::new(
			self.info,
			self.root.clone(),
			self.nodes.clone(),
			self.dynamic.clone(),
			stats::Session::default(),
		)
	}

	/// Handle to the announcement stream for this producer's subtree.
	///
	/// Symmetric counterpart to [`Self::consume`]; call
	/// [`AnnounceProducer::consume`] to get an [`AnnounceConsumer`] that
	/// receives announce / unannounce events.
	pub fn announces(&self) -> AnnounceProducer {
		AnnounceProducer::new(self.root.clone(), self.nodes.clone())
	}

	/// Returns a new Producer that automatically strips out the provided prefix.
	///
	/// Returns None if the provided root is not authorized; when [`Self::scope`]
	/// was already used without a wildcard.
	pub fn with_root(&self, prefix: impl AsPath) -> Option<Self> {
		let prefix = prefix.as_path();

		Some(Self {
			info: self.info,
			root: self.root.join(&prefix).to_owned(),
			nodes: self.nodes.root(&prefix)?,
			dynamic: self.dynamic.clone(),
			pool: self.pool.clone(),
			cache_duration: self.cache_duration,
			latency_default: self.latency_default,
			linger: self.linger,
			stats: self.stats.clone(),
		})
	}

	/// Returns the root that is automatically stripped from all paths.
	pub fn root(&self) -> &Path<'_> {
		&self.root
	}

	/// Iterate over the path prefixes this handle is permitted to publish or subscribe under.
	// TODO return PathPrefixes
	pub fn allowed(&self) -> impl Iterator<Item = &Path<'_>> {
		self.nodes.nodes.iter().map(|(root, _)| root)
	}

	/// Converts a relative path to an absolute path.
	pub fn absolute(&self, path: impl AsPath) -> Path<'_> {
		self.root.join(path)
	}
}

/// How long a spliced track stays warm after its last reader leaves.
///
/// Within the window a returning viewer, or the next of a run of back-to-back
/// fetches, reuses the source's copy: no new track request, and no second round
/// trip for its `TRACK_INFO`. After it, the copy is released so an idle track
/// costs nothing upstream.
///
/// Sized above the fetch cadence of a segmented consumer: HLS polls every
/// `TARGETDURATION` seconds, commonly 6 or 10, so a shorter window would drop the
/// copy between every segment and re-request the track each time. A warm copy
/// holds no upstream subscription (that is canceled as soon as demand ends), so
/// waiting longer costs cached state, not a viewer.
const TRACK_IDLE_LINGER: Duration = Duration::from_secs(30);

/// One attached source in a [`FrontState`] table.
struct FrontRoute {
	id: u64,
	/// The source's latest [`broadcast::Route`], mirrored from its
	/// `route_changed` stream; picks the active source and gates the announce.
	route: broadcast::Route,
	/// The source broadcast tracks are served from.
	source: broadcast::Consumer,
}

/// Shared state behind a [`Front`]: the attached sources and which one is active.
struct FrontState {
	/// Absolute path of the broadcast, mixed into the route tie-break hash.
	path: PathOwned,
	/// The local origin's identity, the other half of the handover key gate.
	self_origin: Origin,
	/// Content identity: the original publisher (first hop) shared by every
	/// attached source, or `None` for a broadcast produced locally (no hops).
	/// Fixed for the front's lifetime; a source with a different first hop is new
	/// content, not an alternate route, so it replaces this front (or waits for
	/// it, when offline) rather than joining it (see [`attach_source`]). This is
	/// the same rule the session layer applies to a restart whose first hop
	/// changed.
	publisher: Option<Origin>,
	/// Attach counter, handed to each [`FrontRoute`] so [`route_order`] can break
	/// an exact tie toward the newest source.
	next_route: u64,
	routes: Vec<FrontRoute>,
	/// Peers currently reading this front through the shared broadcast, refcounted
	/// by live [`ExclusionGuard`]s. The resolve-time check only proves the table is
	/// clean for a requester at that instant; the front picks per track and re-picks
	/// on failover, so without this a route tainted for an attached peer could be
	/// adopted underneath them and hand them back their own bytes. Routes through
	/// these origins are avoided while any clean alternative exists.
	excluded: HashMap<Origin, usize>,
	/// The source tracks are dispatched to. Backups park until promoted.
	active: Option<u64>,
	/// How long the front outlives ungracefully losing its last source (see
	/// [`Info::linger`]). [`run_front`] arms the countdown when the table empties.
	linger: Duration,
	/// Terminal: no more sources may attach and every poller stops. Set
	/// synchronously by the detach that empties the table or by an
	/// [`attach_source`] takeover, and by [`run_front`] when the linger window
	/// expires without a replacement.
	closed: bool,
}

impl FrontState {
	/// The source new track requests should dispatch to: live first, then lowest
	/// cost, then shortest hop chain with a deterministic hash tie-break and the
	/// newest source last, skipping routes that flow through a peer currently
	/// reading the front while any other route remains (see
	/// [`Self::prefer_untainted`]).
	fn best_route(&self) -> Option<u64> {
		let candidates: Vec<&FrontRoute> = self.routes.iter().collect();
		self.prefer_untainted(&candidates)
			.into_iter()
			.min_by_key(|r| route_order(&self.path.as_path(), r))
			.map(|r| r.id)
	}

	/// The source a subscription from `exclude` should dispatch to: the best
	/// route whose hop chain does not contain the requester. This is the same
	/// selection a session uses to pick what it announces to that peer, and the
	/// two being one computation is the loop-freedom invariant: chains stay
	/// truthful, so any would-be cycle surfaces the requester's own origin in
	/// the candidate chain and is filtered here, at any cycle length.
	fn dispatch(&self, exclude: Option<Origin>) -> Option<u64> {
		self.routes
			.iter()
			.filter(|r| exclude.is_none_or(|origin| !r.route.hops.contains(&origin)))
			.min_by_key(|r| route_order(&self.path.as_path(), r))
			.map(|r| r.id)
	}

	/// Whether serving from `route` would hand an attached peer its own bytes back.
	fn taints_a_reader(&self, route: &broadcast::Route) -> bool {
		route.hops.iter().any(|hop| self.excluded.contains_key(hop))
	}

	/// Narrow `candidates` to the routes clean for every peer currently reading the
	/// shared front, unless that would leave nothing.
	///
	/// Keeping the front off a tainted route is what makes the resolve-time
	/// split-horizon check hold for the life of a subscription rather than just at
	/// request time. Falling back when every route is tainted is deliberate: the
	/// alternative is starving readers the route is perfectly good for, and a peer
	/// whose only path runs back through itself has nothing to be served from
	/// anyway. It re-resolves to [`Error::Unroutable`] on its next request.
	fn prefer_untainted<'a>(&self, candidates: &[&'a FrontRoute]) -> Vec<&'a FrontRoute> {
		if self.excluded.is_empty() {
			return candidates.to_vec();
		}
		let clean: Vec<&FrontRoute> = candidates
			.iter()
			.copied()
			.filter(|r| !self.taints_a_reader(&r.route))
			.collect();
		match clean.is_empty() {
			true => candidates.to_vec(),
			false => clean,
		}
	}

	/// The source one track should be served from: the front's active source
	/// unless `skip` rules it out, then the next-best route that survives.
	///
	/// Whether a source carries a given track is a per-track property (a standby
	/// that has not created it yet, a publisher whose encoder is still starting),
	/// so a source refusing one track is ruled out of that track only, never out
	/// of the front. Preferring `active` keeps a servable track on exactly the
	/// route [`Self::reselect`] chose, handover gate included.
	fn serve_route(&self, skip: impl Fn(u64) -> bool) -> Option<u64> {
		if let Some(active) = self.active
			&& !skip(active)
			&& let Some(route) = self.routes.iter().find(|r| r.id == active)
			&& !self.taints_a_reader(&route.route)
		{
			return Some(active);
		}
		let candidates: Vec<&FrontRoute> = self.routes.iter().filter(|r| !skip(r.id)).collect();
		self.prefer_untainted(&candidates)
			.into_iter()
			.min_by_key(|r| route_order(&self.path.as_path(), r))
			.map(|r| r.id)
	}

	/// Re-pick the active source after the table changed. Serve tasks watch
	/// `active` and re-splice on their own, so a cheaper route takes over
	/// seamlessly at a group boundary.
	///
	/// The one exception is the simultaneous-activation race: two nodes that
	/// each pulled the broadcast before seeing the other both advertise zero
	/// cost, so each sees the other as cheaper than its own source, and
	/// re-parenting onto each other at once leaves the broadcast with no
	/// upstream at all. That hazard only exists when both sides are actively
	/// carrying, so the gate is scoped to exactly that: while `carrying` (the
	/// front has live demand), a cheaper route whose announcing relay is itself
	/// carrying (it advertised zero from a chain of two or more hops; a chain of
	/// one is the original publisher, which can never adopt a route to its own
	/// broadcast) displaces an announced incumbent only when
	/// [`Self::handover_allowed`] says so. Every other cheaper route, e.g. a
	/// forwarder path or an upstream that repriced itself down, is taken
	/// immediately.
	fn reselect(&mut self, carrying: bool) {
		let best = self.best_route();
		if carrying
			&& let (Some(best_id), Some(cur_id)) = (best, self.active)
			&& best_id != cur_id
			&& let Some(candidate) = self.routes.iter().find(|r| r.id == best_id)
			&& let Some(incumbent) = self.routes.iter().find(|r| r.id == cur_id)
			&& incumbent.route.announce
			&& candidate.route.cost < incumbent.route.cost
			&& candidate.route.advertised == 0
			&& candidate.route.hops.len() >= 2
			&& !self.handover_allowed(&candidate.route)
		{
			// We won the key comparison: keep our source and let the peer come to us.
			return;
		}
		self.active = best;
	}

	/// Whether re-parenting onto `route` is allowed while actively carrying: the
	/// announcing peer (the chain's last hop) must hash strictly below our own
	/// origin for this broadcast name.
	///
	/// Both sides compute the same two keys (the hash is build-stable and the
	/// inputs are shared), so the comparison resolves the same way everywhere:
	/// the lower-keyed node keeps its source, the higher-keyed one re-parents.
	/// A strict total order has no cycles, so mutual pulls cannot happen. Mixing
	/// the broadcast name in spreads ownership across a region's relays instead
	/// of funneling every broadcast onto the lowest-keyed one. A route with no
	/// hops is a local publish and always allowed.
	fn handover_allowed(&self, route: &broadcast::Route) -> bool {
		let name = self.path.as_path();
		match route.hops.iter().last() {
			Some(peer) => fnv_key(&name, [*peer]) < fnv_key(&name, [self.self_origin]),
			None => true,
		}
	}

	/// Every attached route in preference order with the active one first,
	/// mirrored onto the front's broadcast so sessions can advertise (and be
	/// served) a different route per peer. The active route leads even when the
	/// handover gate kept it over a lower-ordered candidate, so `routes[0]` is
	/// always what this node is actually serving from.
	fn routes_snapshot(&self) -> Vec<broadcast::Route> {
		let mut routes: Vec<&FrontRoute> = self.routes.iter().collect();
		routes.sort_by_key(|r| route_order(&self.path.as_path(), r));
		routes.sort_by_key(|r| Some(r.id) != self.active);
		routes.into_iter().map(|r| r.route.clone()).collect()
	}
}

/// Refresh the front's public face after a table change: advertise the best
/// source's route on the spliced broadcast and gate the path's announcement on
/// its `live` flag.
///
/// Re-reads the table at apply time (rather than applying a value computed under
/// an earlier lock) so concurrent attach/detach/update calls converge on the
/// latest winner regardless of the order their applies land in. An empty table
/// leaves the advert and announce state alone; the front task is closing and
/// unannounces on its way out.
fn sync_front(state: &kio::Producer<FrontState>, broadcast: &broadcast::Producer, leaf: &Lock<OriginNode>) {
	// Snapshot and apply under the leaf lock: two concurrent syncs would
	// otherwise race their applies, letting a stale snapshot land last and
	// leave the announce flag (or advert) contradicting the current table.
	// Lock order (leaf, then table, then broadcast) matches attach_source.
	let mut leaf_guard = leaf.lock();
	let routes = state.read().routes_snapshot();
	if let Some(advert) = routes.first() {
		let announce = advert.announce;
		broadcast.clone().set_routes(routes);
		leaf_guard.set_announced(state, announce);
	}
}

/// Detach source `id`, promoting the next-best source; the tracks it was serving
/// re-splice on their own. Idempotent.
///
/// `graceful` says how the source ended: a deliberate finish (a publisher done
/// publishing, a peer's clean unannounce) or an abrupt loss (a session dying).
/// Detaching the last source *gracefully* closes the broadcast synchronously,
/// which guarantees a following create at the path is a *new* broadcast rather
/// than splicing new content into this one. An abrupt last detach instead leaves
/// the front open for the configured linger ([`Info::linger`]), so a source
/// re-attaching within the window (a reconnect) splices in seamlessly;
/// [`run_front`] closes it if the window expires empty. A zero linger closes
/// abrupt detaches synchronously too.
fn detach_source(
	state: &kio::Producer<FrontState>,
	broadcast: &broadcast::Producer,
	leaf: &Lock<OriginNode>,
	id: u64,
	graceful: bool,
) {
	let close = {
		let carrying = broadcast.demand().is_used();
		let Ok(mut s) = state.write() else { return };
		let Some(pos) = s.routes.iter().position(|r| r.id == id) else {
			return;
		};
		s.routes.remove(pos);
		s.reselect(carrying);
		if s.routes.is_empty() && !s.closed && (graceful || s.linger.is_zero()) {
			// Last one out: close now. The front task observes `closed` and
			// finishes the teardown (unpublish).
			s.closed = true;
			true
		} else {
			false
		}
	};
	if close {
		broadcast.abort_spliced(Error::Dropped);
	}
	sync_front(state, broadcast, leaf);
}

/// Owns one source's lifecycle: attaches it to the front at its path on the first
/// route observation, forwards route updates, and detaches it when the source
/// closes. Spawned by [`Producer::create_broadcast`].
///
/// An announced source whose original publisher (first hop) differs from the
/// live front's takes the path over as a fresh broadcast rather than joining; an
/// offline one parks until the front closes. A route update that changes the
/// source's own first hop likewise detaches it and re-runs the attach, so a
/// publisher swap is always a replacement, never a silent splice.
async fn run_source(
	origin: Info,
	node: Lock<OriginNode>,
	full: PathOwned,
	rest: PathOwned,
	mut source: broadcast::Consumer,
	ingress: stats::Scope,
) {
	let ctx = AttachContext {
		origin: &origin,
		node: &node,
		full: &full,
		rest: &rest,
	};

	// The first `route_changed` yields the current route immediately; nothing is
	// visible to consumers until this attach, giving the creator a window to set
	// up tracks and dynamic handlers.
	let Ok(mut route) = source.route_changed().await else {
		// Closed before ever attaching; nothing became visible.
		return;
	};

	// Ingress announce guard: held while this source's route is announced. Opening
	// bumps `announced` + `announced_bytes`; dropping (route offline, or the source
	// closing below) bumps `announced_closed` + `announced_bytes`. Empty scope =
	// no-op.
	let mut announce = route.announce.then(|| ingress.announce());

	'attach: loop {
		// Re-resolved every attempt: between attaches the previous front's
		// teardown may have pruned the (then-empty) leaf from the tree, and
		// attaching to the stale lock would publish into an orphan that lookups
		// can no longer reach.
		let leaf = if rest.is_empty() {
			node.clone()
		} else {
			node.lock().leaf(&rest)
		};

		let (state, broadcast, id) = match attach_source(&ctx, &leaf, &source, route.clone()) {
			Attach::Ready(state, broadcast, id) => (state, broadcast, id),
			Attach::Parked(incumbent) => {
				tracing::warn!(
					broadcast = %full,
					"path already live with a different publisher; parking an offline source until it ends",
				);
				// Wait for the incumbent front to close, or for our own route to
				// change: announcing ourselves is what earns the takeover, and our
				// source closing means giving up.
				let update = kio::wait(|waiter| {
					if let Poll::Ready(update) = source.poll_route_changed(waiter) {
						return Poll::Ready(Some(update));
					}
					// Ready on either the closed flag or the channel itself dying;
					// both mean the incumbent is gone.
					match incumbent.poll(waiter, |s| if s.closed { Poll::Ready(()) } else { Poll::Pending }) {
						Poll::Ready(_) => Poll::Ready(None),
						Poll::Pending => Poll::Pending,
					}
				})
				.await;
				match update {
					// Our route moved; recompute the guard and retry with it.
					Some(Ok(update)) => {
						match (update.announce, announce.is_some()) {
							(true, false) => announce = Some(ingress.announce()),
							(false, true) => announce = None,
							_ => {}
						}
						route = update;
					}
					// The source closed while parked; it was never visible.
					Some(Err(_)) => return,
					// The incumbent is gone; retry, creating a fresh front.
					None => {}
				}
				continue 'attach;
			}
		};
		let publisher = route.hops.iter().next().copied();

		loop {
			match source.route_changed().await {
				Ok(update) => {
					let announced = update.announce;
					// A different first hop is new content: this source can no
					// longer feed the front it attached to. Detach deliberately
					// (a linger would only stall the replacement) and re-attach.
					if update.hops.iter().next().copied() != publisher {
						detach_source(&state, &broadcast, &leaf, id, true);
						match (announced, announce.is_some()) {
							(true, false) => announce = Some(ingress.announce()),
							(false, true) => announce = None,
							_ => {}
						}
						route = update;
						continue 'attach;
					}
					{
						let carrying = broadcast.demand().is_used();
						let Ok(mut s) = state.write() else { return };
						let Some(entry) = s.routes.iter_mut().find(|r| r.id == id) else {
							return;
						};
						if entry.route == update {
							continue;
						}
						entry.route = update;
						s.reselect(carrying);
					}
					// Toggle the ingress announce guard on a live/offline transition.
					match (announced, announce.is_some()) {
						(true, false) => announce = Some(ingress.announce()),
						(false, true) => announce = None,
						_ => {}
					}
					sync_front(&state, &broadcast, &leaf);
				}
				Err(_) => {
					// A deliberate finish closes the front immediately; an abrupt loss
					// (dropped producer, dead session) may linger for a replacement.
					detach_source(&state, &broadcast, &leaf, id, source.is_finished());
					return;
				}
			}
		}
	}
}

/// The outcome of [`attach_source`].
enum Attach {
	/// The source joined (or created) the front at its path, yielding the shared
	/// source table, the spliced broadcast, and the source's table id.
	Ready(kio::Producer<FrontState>, broadcast::Producer, u64),
	/// The path's live front belongs to a different original publisher and this
	/// source is offline, so it would rank below every route the front holds and
	/// must not take the path from it. The caller parks on the returned table
	/// until the front closes.
	Parked(kio::Producer<FrontState>),
}

/// Everything about a source's attach that does not change between attempts.
struct AttachContext<'a> {
	origin: &'a Info,
	node: &'a Lock<OriginNode>,
	/// Absolute path, for the front's identity and log lines.
	full: &'a PathOwned,
	/// Path relative to `node`, for locating (and later pruning) the leaf.
	rest: &'a PathOwned,
}

/// Attach a source to the broadcast at `leaf`, creating (and publishing) the
/// broadcast if none is live. One lock acquisition covers the whole
/// join-or-create decision, so concurrent attaches cannot race each other.
///
/// Joining requires the same content identity (first hop). A source whose
/// original publisher differs takes the path over instead: the incumbent front
/// closes and a fresh one is created below, so consumers observe an unannounce
/// followed by an announce. That mirrors the session layer's rule that a restart
/// with a different first hop is a replacement, never a standby, and it is what
/// keeps a reconnect from waiting on the transport to retire the session it
/// replaced.
///
/// Taking over requires announcing, which keeps the rule consistent with
/// [`route_order`]: an offline source ranks below every announced route, so it
/// waits ([`Attach::Parked`]) rather than unannouncing a live broadcast and
/// cutting its subscribers for content nobody has advertised.
/// Whether two sources carry the same content and may therefore splice.
///
/// [`Origin::UNKNOWN`] identifies nothing: it is what a peer that declared no
/// identity, or that does not speak the hops extension at all, contributes as a
/// first hop. Two such sources are not interchangeable even though their first
/// hops compare equal, so splicing them would cut one publisher's subscribers
/// over to an unrelated publisher's content. Every other id compares normally,
/// including `None` for a locally produced broadcast with no hops.
fn same_publisher(a: Option<Origin>, b: Option<Origin>) -> bool {
	if a == Some(Origin::UNKNOWN) || b == Some(Origin::UNKNOWN) {
		return false;
	}
	a == b
}

fn attach_source(
	ctx: &AttachContext,
	leaf: &Lock<OriginNode>,
	source: &broadcast::Consumer,
	route: broadcast::Route,
) -> Attach {
	let publisher = route.hops.iter().next().copied();
	let mut leaf_guard = leaf.lock();

	// Join the live broadcast if the leaf already has one. A closed one (torn
	// down, awaiting teardown, or evicted just below) is replaced instead.
	if let Some(existing) = &leaf_guard.broadcast {
		let mut joined = None;
		let carrying = existing.broadcast.demand().is_used();
		if let Ok(mut s) = existing.state.write()
			&& !s.closed
		{
			if same_publisher(s.publisher, publisher) {
				let id = s.next_route;
				s.next_route += 1;
				s.routes.push(FrontRoute {
					id,
					route: route.clone(),
					source: source.clone(),
				});
				s.reselect(carrying);
				joined = Some(id);
			} else if !route.announce {
				return Attach::Parked(existing.state.clone());
			} else {
				// New content at a live path: the newest publisher wins it. Closing
				// the incumbent here (rather than letting the newcomer wait it out)
				// is what makes a reconnect immediate, and routing the takeover
				// through the replacement path below keeps the guarantee that
				// unrelated content is never spliced into live subscribers. The
				// incumbent's own task observes the flag and finishes its teardown,
				// finding the leaf slot already taken.
				s.closed = true;
				tracing::warn!(broadcast = %ctx.full, "replacing a live broadcast from a different publisher");
			}
		}
		if let Some(id) = joined {
			let state = existing.state.clone();
			let broadcast = existing.broadcast.clone();
			drop(leaf_guard);
			sync_front(&state, &broadcast, leaf);
			return Attach::Ready(state, broadcast, id);
		}
	}

	// First source: create the broadcast and publish it into the tree.
	let announce = route.announce;
	let broadcast = broadcast::Producer::new_spliced(broadcast::Info {
		origin: ctx.origin.clone(),
	});
	let _ = broadcast.clone().set_route(route.clone());
	let state = kio::Producer::new(FrontState {
		path: ctx.full.clone(),
		self_origin: ctx.origin.id,
		publisher,
		next_route: 1,
		excluded: HashMap::new(),
		routes: vec![FrontRoute {
			id: 0,
			route,
			source: source.clone(),
		}],
		active: Some(0),
		linger: ctx.origin.linger,
		closed: false,
	});

	// Replacing a stale (closed) entry counts as an unannounce, so consumers
	// observe the replacement rather than a silent swap; its own teardown task
	// then finds the slot already taken and leaves it alone.
	if let Some(stale) = leaf_guard.broadcast.take()
		&& stale.announced
	{
		leaf_guard.notify.lock().unannounce(&stale.path);
	}
	let entry = OriginBroadcast {
		path: ctx.full.clone(),
		broadcast: broadcast.clone(),
		state: state.clone(),
		announced: announce,
	};
	if entry.announced {
		leaf_guard.notify.lock().announce(ctx.full, &broadcast.consume());
	}
	leaf_guard.broadcast = Some(entry);
	drop(leaf_guard);

	web_async::spawn(run_front(
		state.clone(),
		broadcast.clone(),
		ctx.node.clone(),
		ctx.rest.clone(),
	));

	Attach::Ready(state, broadcast, 0)
}

/// Owns a front's lifecycle: dispatches each requested track to a serve task
/// until the last source detaches, then unpublishes the broadcast.
async fn run_front(
	state: kio::Producer<FrontState>,
	mut broadcast: broadcast::Producer,
	node: Lock<OriginNode>,
	rest: PathOwned,
) {
	enum Step {
		Serve(Arc<str>, super::resume::Producer),
		/// The source table emptied or refilled: re-arm the linger countdown.
		Changed,
		/// The linger window expired: close if the table is still empty.
		Expired,
		Closed,
	}

	let linger = state.read().linger;
	// Armed while the table is ungracefully empty: the instant the front gives up
	// waiting for a replacement source. A graceful close never gets here (the
	// detach sets `closed` synchronously), so a running countdown always means a
	// reconnect is welcome.
	let mut deadline = kio::time::Deadline::new();

	loop {
		let empty = {
			let s = state.read();
			!s.closed && s.routes.is_empty()
		};
		deadline.set(match (empty, deadline.deadline()) {
			// An unrepresentable deadline (e.g. `Duration::MAX`) lingers forever:
			// no timer, only a re-attach or teardown moves the front on.
			(true, None) => web_async::time::Instant::now().checked_add(linger),
			(true, at) => at,
			(false, _) => None,
		});

		let step = {
			kio::wait(|waiter| {
				if let Poll::Ready((name, resume)) = broadcast.poll_spliced_assigned(waiter) {
					return Poll::Ready(Step::Serve(name, resume));
				}
				// Watch for the close, and for the table changing shape so the
				// countdown re-arms (a detach emptying it, a reconnect refilling it).
				match state.poll(waiter, |s| {
					if s.closed || s.routes.is_empty() != empty {
						Poll::Ready(())
					} else {
						Poll::Pending
					}
				}) {
					Poll::Ready(Ok(guard)) => {
						return Poll::Ready(if guard.closed { Step::Closed } else { Step::Changed });
					}
					Poll::Ready(Err(_)) => return Poll::Ready(Step::Closed),
					Poll::Pending => {}
				}
				deadline.poll(waiter).map(|_| Step::Expired)
			})
			.await
		};

		match step {
			Step::Serve(name, resume) => {
				// Serve tasks self-terminate when the track completes or the
				// front closes.
				web_async::spawn(serve_track(state.clone(), name, resume));
			}
			Step::Changed => {}
			Step::Expired => {
				// Close only if the table is still empty: a source that re-attached
				// as the window expired wins the write-lock race and keeps the
				// broadcast alive.
				let close = {
					let Ok(mut s) = state.write() else { break };
					if !s.closed && s.routes.is_empty() {
						s.closed = true;
						true
					} else {
						false
					}
				};
				if close {
					break;
				}
			}
			Step::Closed => break,
		}
	}

	// Abort the logical tracks (releasing their subscribers) and unpublish.
	broadcast.abort_spliced(Error::Dropped);

	// Deliberate end; suppresses the dropped-without-finish warning.
	broadcast.finish();

	// Remove the broadcast from the tree (identity-checked, so a replacement is
	// untouched) and prune empty nodes.
	node.lock().remove(&state, &rest);
}

/// Serves one spliced logical track: splices in the best source's copy of the
/// track, re-splicing on handover or failure, until the track completes or the
/// front closes. A source that refuses the track (rejects it, or hands over a
/// copy that already ended in an error) aborts the logical track immediately:
/// which tracks a broadcast carries is the publisher's contract (announce after
/// the tracks exist), so the dispatched source's answer is authoritative and
/// there is nothing to retry. Failures after a splice (a serving session dying
/// mid-stream) are normal failover and re-splice from the next source at the
/// first missing group; a source that errors while *closing* is likewise a
/// corpse to fail over past (its watcher is about to detach it), not a verdict
/// on the track.
async fn serve_track(state: kio::Producer<FrontState>, name: Arc<str>, mut resume: super::resume::Producer) {
	enum Step {
		Closed,
		Splice(u64, broadcast::Consumer),
		Complete,
		Failed,
		/// The route we were serving from left the table with nothing servable
		/// to replace it (an empty table, or only corpses awaiting detach):
		/// drop our handle and park for the table to move on.
		NoRoute,
		/// The linger expired with the track still unread: release the segment.
		Idle,
		/// A reader arrived or the last one left: recompute the demand gate.
		Demand,
	}

	// The source whose copy is currently spliced in, and that copy.
	let mut serving: Option<(u64, track::Consumer)> = None;
	// Sources whose splice failed because they had already closed. Their watchers
	// are about to detach them, so wait for the table to move on rather than
	// treating a corpse's error as the track's fate (ids are never reused, so
	// this cannot wedge).
	let mut dead: HashSet<u64> = HashSet::new();
	// When the spliced segment stopped being read, starting the release countdown.
	let mut idle_since: Option<web_async::time::Instant> = None;
	let mut deadline = kio::time::Deadline::new();

	loop {
		let serving_id = serving.as_ref().map(|(id, _)| *id);

		// Drop corpses the table has detached, so a source that reattaches (under
		// a fresh id) is dispatchable while a still-detaching one stays skipped.
		{
			let s = state.read();
			dead.retain(|id| s.routes.iter().any(|r| r.id == *id));
		}

		// Demand gates both directions: an unread track never splices a source in,
		// and a spliced one is released once the linger expires. Both sides use the
		// same signal, so a release can't immediately re-splice and spin.
		//
		// The countdown keys off the segment, not our handle on the route that
		// produced it: a route that leaves (or a copy that dies) drops the handle
		// while the segment stays spliced, and that segment is exactly what the
		// release exists to reclaim. Keying off the handle strands it until the front
		// closes, which pins the departed source's cached groups for a linger that
		// may never expire and leaves a dead segment's edge behind for the next
		// takeover to splice above.
		let used = resume.is_used();
		idle_since = match (resume.is_spliced(), used) {
			(true, false) => idle_since.or_else(|| Some(web_async::time::Instant::now())),
			_ => None,
		};
		deadline.set(idle_since.and_then(|at| at.checked_add(TRACK_IDLE_LINGER)));

		let step = {
			let skip = |id: u64| dead.contains(&id);
			kio::wait(|waiter| {
				// Watch the source table: the front closing, a better servable
				// source than the one spliced in (skipping corpses awaiting
				// detach), or the served route leaving the table. Splicing waits
				// for a reader.
				match state.poll(waiter, |s| {
					let gone = serving_id.is_some_and(|id| !s.routes.iter().any(|r| r.id == id));
					if s.closed
						|| (used && (gone || matches!(s.serve_route(skip), Some(next) if Some(next) != serving_id)))
					{
						Poll::Ready(())
					} else {
						Poll::Pending
					}
				}) {
					Poll::Ready(Ok(guard)) => {
						if guard.closed {
							return Poll::Ready(Step::Closed);
						}
						let Some(next) = guard.serve_route(skip) else {
							return Poll::Ready(Step::NoRoute);
						};
						let source = guard
							.routes
							.iter()
							.find(|r| r.id == next)
							.expect("servable source in table")
							.source
							.clone();
						return Poll::Ready(Step::Splice(next, source));
					}
					Poll::Ready(Err(_)) => return Poll::Ready(Step::Closed),
					Poll::Pending => {}
				}

				// Watch the demand edge in whichever direction is unmet. This has to end
				// the wait, not just wake it: `used` and the countdown are computed by
				// the outer loop, so a wake that stayed inside would re-poll with the
				// stale value and never arm (or cancel) the linger.
				let edge = match used {
					true => resume.poll_unused(waiter),
					false => resume.poll_used(waiter),
				};
				if edge.is_ready() {
					return Poll::Ready(Step::Demand);
				}

				// Watch the spliced copy for its end: complete means the logical
				// track is over; anything else means the serving source died.
				if let Some((_, track)) = &serving
					&& let Poll::Ready(result) = track.poll_complete(waiter)
				{
					return Poll::Ready(match result {
						Ok(()) => Step::Complete,
						Err(_) => Step::Failed,
					});
				}

				deadline.poll(waiter).map(|_| Step::Idle)
			})
			.await
		};

		match step {
			// The front's teardown aborts the logical track.
			Step::Closed => return,
			Step::Complete => {
				let _ = resume.finish();
				return;
			}
			Step::Failed => {
				// The spliced copy died mid-serve: failover. Re-splice from the
				// (possibly same) active source.
				serving = None;
			}
			// The outer loop recomputes `used` and the countdown on the next pass.
			Step::Demand => {}
			// Forget which route we were serving from, or the `gone` edge that woke
			// us keeps firing: the id stays absent from the table, the wait returns
			// Ready at once, and the loop spins on a full core without ever parking.
			// The segment itself stays spliced into `resume` (readers keep whatever
			// it delivered) until a replacement is proven servable.
			Step::NoRoute => serving = None,
			Step::Idle => {
				// Nobody has read the track for the linger: drop the source's copy so
				// its session can release the track (and the cached `track::Info` that
				// came with it). The logical track stays alive and re-splices on the
				// next reader, so a returning viewer or a follow-up fetch resumes.
				if resume.release().is_err() {
					// Finished or aborted meanwhile; the track is over either way.
					return;
				}
				serving = None;
			}
			Step::Splice(id, source) => {
				// Ask the source for its copy and wait for the info to resolve,
				// proving it servable, before splicing it in. Bail out early if
				// the table moves on while waiting.
				let attempt = match source.track(&name) {
					Ok(track) => {
						// `into_inner` sheds the `Pending` future wrapper so only
						// the pollable (which is `Sync`) is held across the await.
						let query = track.info().into_inner();
						let skip = |id: u64| dead.contains(&id);
						// The route-change edge is checked BEFORE the query result:
						// a rejection is only authoritative while the source is
						// still the dispatched route, and the two can resolve in
						// the same wake. Taking a stale rejection here would turn
						// a seamless handover into a terminal abort.
						let info = kio::wait(|waiter| {
							match state.poll(waiter, |s| {
								if s.closed || s.serve_route(skip) != Some(id) {
									Poll::Ready(())
								} else {
									Poll::Pending
								}
							}) {
								Poll::Ready(_) => return Poll::Ready(None),
								Poll::Pending => {}
							}
							query.poll(waiter).map(Some)
						})
						.await;
						match info {
							// The table changed under us; re-pick from the top.
							None => continue,
							// A copy that is already aborted can't be spliced;
							// its error is the source's answer for the track.
							Some(Ok(_)) => match track.poll_complete(&kio::Waiter::noop()) {
								Poll::Ready(Err(err)) => Err(err),
								_ => Ok(track),
							},
							Some(Err(err)) => Err(err),
						}
					}
					Err(err) => Err(err),
				};

				match attempt {
					Ok(track) => {
						if resume.takeover(&track).is_err() {
							// The logical track already ended (finished or
							// aborted); nothing left to serve.
							return;
						}
						dead.clear();
						serving = Some((id, track));
					}
					// The source itself closed or deliberately ended: not a
					// verdict on the track. Park until its watcher detaches
					// it and the table promotes a replacement.
					Err(_) if source.is_closing() => {
						dead.insert(id);
						serving = None;
					}
					// The dispatched source does not carry the track, and its
					// answer is authoritative: a publisher announces a broadcast
					// only once its tracks exist, so there is no later sweep that
					// would find the track and nothing to retry. Fail now, loudly,
					// with the source's own error.
					Err(err) => {
						tracing::debug!(name = %name, source = id, %err, "source refused track; aborting");
						let _ = resume.abort(err);
						return;
					}
				}
			}
		}
	}
}

/// Shared fallback request queue for an origin.
///
/// Lives off to the side of the announce tree because dynamically served broadcasts
/// are never announced. Carried in a [`kio::Shared`], so consumers enqueue and handlers
/// drain under one lock. Mirrors the fetch state of the track model.
#[derive(Default)]
struct OriginDynamicState {
	// Result channels for pending requests, keyed by absolute path so concurrent
	// `request_broadcast` calls for the same path coalesce onto one channel.
	requests: Requests<PathOwned, kio::Producer<PendingBroadcast>>,

	// Broadcasts a handler has already served, kept weakly so a repeat request for the
	// same path resolves to a shared clone instead of re-invoking the handler (which would
	// open a duplicate upstream subscription). Weak so a served broadcast still closes once
	// its real consumers drop. The cache reclaims closed entries incrementally on insert, so a
	// long-lived origin serving many distinct one-shot paths stays bounded by the live count.
	served: WeakCache<PathOwned, broadcast::WeakConsumer>,
}

/// One-shot result of a dynamic broadcast request.
///
/// Stays `None` until a handler [`accept`](Request::accept)s (yielding the served
/// broadcast) or [`reject`](Request::reject)s (yielding an error). The producer is
/// dropped right after writing, closing the channel; kio checks the value before the closed
/// flag, so an awaiting requester still observes the final result.
#[derive(Default)]
struct PendingBroadcast {
	resolved: Option<Result<broadcast::Consumer, Error>>,
}

/// Picks up [`Consumer::request_broadcast`] calls for paths that are not announced.
///
/// The origin-level analogue of [`broadcast::Dynamic`]: where that serves tracks on
/// demand within a broadcast, this serves whole broadcasts on demand within an origin. A
/// relay uses it as a fallback router, fetching a broadcast from upstream only when a
/// downstream consumer asks for an exact path that nobody announced.
///
/// Served broadcasts are deliberately *not* announced, so they never appear in
/// [`Consumer::announced`]. Drop this handle (and every clone) to reject the
/// requests still waiting to be served.
pub struct Dynamic {
	info: Origin,
	root: PathOwned,
	state: kio::Shared<OriginDynamicState>,
}

impl Clone for Dynamic {
	fn clone(&self) -> Self {
		// Mirror `new`: count each live handle. Without this, dropping a clone would
		// decrement past `new`'s increment and prematurely flip the handler count to
		// zero, making future `request_broadcast` calls return `Unroutable`.
		self.state.lock().requests.add_handler();

		Self {
			info: self.info,
			root: self.root.clone(),
			state: self.state.clone(),
		}
	}
}

impl Dynamic {
	fn new(info: Origin, root: PathOwned, state: kio::Shared<OriginDynamicState>) -> Self {
		state.lock().requests.add_handler();

		Self { info, root, state }
	}

	/// The origin this handler belongs to.
	pub fn info(&self) -> &Origin {
		&self.info
	}

	/// Poll for the next requested broadcast, without blocking.
	pub fn poll_requested_broadcast(&mut self, waiter: &kio::Waiter) -> Poll<Result<Request, Error>> {
		let mut state = ready!(self.state.poll(waiter, |state| {
			if state.requests.has_queued() {
				Poll::Ready(())
			} else {
				Poll::Pending
			}
		}));

		let path = state.requests.pop().expect("predicate guaranteed a request");
		// The popped request stays pending, so a repeat request in the window between
		// hand-off and accept coalesces onto it instead of re-invoking the handler. The
		// producer is a shared clone; `Request::{accept, reject, drop}` removes the
		// entry. This mirrors how `poll_requested_track` keeps a served track
		// discoverable via the weak cache across the same window.
		let producer = state.requests.get(&path).expect("popped key must be pending").clone();
		Poll::Ready(Ok(Request {
			path,
			producer,
			state: self.state.clone(),
		}))
	}

	/// Block until a consumer requests an unannounced broadcast, returning a
	/// [`Request`] to serve.
	pub async fn requested_broadcast(&mut self) -> Result<Request, Error> {
		kio::wait(|waiter| self.poll_requested_broadcast(waiter)).await
	}

	/// Returns the prefix that is automatically stripped from requested paths.
	pub fn root(&self) -> &Path<'_> {
		&self.root
	}
}

impl Drop for Dynamic {
	fn drop(&mut self) {
		// Decrement and reject under one lock, so a `request_broadcast` that saw a
		// live handler through the same lock can't slip a request past the rejection.
		let mut state = self.state.lock();
		if state.requests.remove_handler() {
			// No handlers left to pop queued requests; drop them, closing their result
			// channels so awaiting requesters resolve to `Unroutable`. A request already
			// handed to a handler stays, resolved by its `Request` instead.
			state.requests.drain_queued();
		}
	}
}

/// A pending request for a broadcast that was not announced.
///
/// Yielded by [`Dynamic::requested_broadcast`]. The requester is awaiting inside
/// [`Consumer::request_broadcast`]; [`accept`](Self::accept) resolves it with a live
/// broadcast (which the handler keeps producing into) and [`reject`](Self::reject) resolves
/// it with an error. Dropping the request without either rejects it.
pub struct Request {
	// Absolute path that was requested.
	path: PathOwned,

	// Result channel back to the awaiting requester(s). Writing `resolved` and dropping
	// this wakes them with the outcome.
	producer: kio::Producer<PendingBroadcast>,

	// Shared dynamic state, so `accept` can cache the served broadcast for repeat requests.
	state: kio::Shared<OriginDynamicState>,
}

impl Request {
	/// The absolute path that was requested.
	pub fn path(&self) -> &Path<'_> {
		&self.path
	}

	/// Accept the request, resolving every awaiting requester with `broadcast`.
	///
	/// The caller keeps producing into `broadcast` (e.g. a relay proxying tracks from
	/// upstream); the requesters receive a consumer for it. The broadcast is *not*
	/// announced.
	pub fn accept(self, broadcast: impl Consume<broadcast::Consumer>) {
		let broadcast = broadcast.consume();

		// Move the entry out of the in-flight queue and into the weak `served` cache, so repeat
		// requests for this path share the same broadcast instead of asking the handler to serve
		// (and subscribe upstream) again. Re-check under the lock: if a live broadcast was already
		// served for this path while we were fetching upstream, dedup onto it and drop ours rather
		// than replace a good entry with a duplicate subscription.
		let resolved = {
			let mut state = self.state.lock();
			let existing = state.served.insert(self.path.clone(), broadcast.weak());
			state
				.requests
				.remove_if(&self.path, |producer| producer.same_channel(&self.producer));
			existing.map(|weak| weak.consume()).unwrap_or(broadcast)
		};

		if let Ok(mut pending) = self.producer.write() {
			pending.resolved = Some(Ok(resolved));
		}
		// `self.producer` drops here, closing the channel; the value is still observable.
	}

	/// Reject the request, resolving every awaiting requester with `err`.
	pub fn reject(self, err: Error) {
		self.state
			.lock()
			.requests
			.remove_if(&self.path, |producer| producer.same_channel(&self.producer));
		if let Ok(mut state) = self.producer.write() {
			state.resolved = Some(Err(err));
		}
	}
}

impl Drop for Request {
	fn drop(&mut self) {
		// Handed off but neither accepted nor rejected: drop the still-pending entry so its
		// producer clone (plus this one) closes the channel, resolving coalesced requesters to
		// `Unroutable` rather than hanging.
		//
		// The identity guard matters: `accept`/`reject` already removed our entry and released
		// the lock before we run, so a concurrent request for the same path may have registered
		// a *new* one here. Removing unconditionally would clobber it, stranding its requesters.
		self.state
			.lock()
			.requests
			.remove_if(&self.path, |producer| producer.same_channel(&self.producer));
	}
}

/// The pollable result of [`Consumer::request_broadcast`].
///
/// Awaited via the [`kio::Pending`] wrapper; resolves to the [`broadcast::Consumer`]
/// immediately when the broadcast was already announced, or once an [`Dynamic`]
/// handler serves the request. Resolves to an error if the request is rejected or every
/// handler drops before serving it.
pub struct Requesting {
	inner: RequestState,
	// Egress scope applied to the resolved broadcast, so its reads are attributed.
	// Empty (no-op) for an untagged consumer.
	stats: stats::Scope,
}

enum RequestState {
	// Already announced: resolves immediately with a clone of this broadcast.
	Ready(broadcast::Consumer),
	// Unroutable at request time: resolves immediately with this error. Baked in so
	// `request_broadcast` itself stays infallible.
	Failed(Error),
	// Awaiting a handler: resolves when the request's result channel is written.
	Pending(kio::Consumer<PendingBroadcast>),
}

impl Requesting {
	fn ready(broadcast: broadcast::Consumer) -> Self {
		Self {
			inner: RequestState::Ready(broadcast),
			stats: stats::Scope::default(),
		}
	}

	fn failed(error: Error) -> Self {
		Self {
			inner: RequestState::Failed(error),
			stats: stats::Scope::default(),
		}
	}

	fn pending(consumer: kio::Consumer<PendingBroadcast>) -> Self {
		Self {
			inner: RequestState::Pending(consumer),
			stats: stats::Scope::default(),
		}
	}

	fn with_stats(mut self, scope: stats::Scope) -> Self {
		self.stats = scope;
		self
	}

	/// Poll for the requested broadcast without blocking.
	pub fn poll_ok(&self, waiter: &kio::Waiter) -> Poll<Result<broadcast::Consumer, Error>> {
		match &self.inner {
			RequestState::Ready(broadcast) => Poll::Ready(Ok(broadcast.clone().with_stats(self.stats.clone()))),
			RequestState::Failed(error) => Poll::Ready(Err(error.clone())),
			RequestState::Pending(consumer) => Poll::Ready(
				match ready!(consumer.poll(waiter, |state| match &state.resolved {
					Some(result) => Poll::Ready(result.clone()),
					None => Poll::Pending,
				})) {
					Ok(result) => result.map(|broadcast| broadcast.with_stats(self.stats.clone())),
					// Every handler dropped without resolving: nobody could route it.
					Err(_closed) => Err(Error::Unroutable),
				},
			),
		}
	}
}

impl kio::Pollable for Requesting {
	type Output = Result<broadcast::Consumer, Error>;

	fn poll(&self, waiter: &kio::Waiter) -> Poll<Self::Output> {
		self.poll_ok(waiter)
	}
}

/// Derive a read view from a handle.
///
/// Lets APIs accept either a producer or a consumer (e.g.
/// [`Client::with_publisher`](crate::Client::with_publisher),
/// [`Request::accept`]). The blanket `&T` impl means you can
/// pass by value (`foo(x)`) to hand off ownership, or by reference (`foo(&x)`)
/// to keep it, without spelling out `.consume()`.
pub trait Consume<T> {
	/// Derive a read view (a consumer) from this handle.
	fn consume(&self) -> T;
}

impl<T, U: Consume<T>> Consume<T> for &U {
	fn consume(&self) -> T {
		(**self).consume()
	}
}

impl Consume<Consumer> for Producer {
	fn consume(&self) -> Consumer {
		// Mirrors the inherent `Producer::consume`; inlined to avoid the
		// inherent-vs-trait `consume` ambiguity. Untagged: egress is tagged
		// separately from ingress.
		Consumer::new(
			self.info,
			self.root.clone(),
			self.nodes.clone(),
			self.dynamic.clone(),
			stats::Session::default(),
		)
	}
}

impl Consume<Consumer> for Consumer {
	fn consume(&self) -> Consumer {
		self.clone()
	}
}

impl Consume<broadcast::Consumer> for broadcast::Producer {
	fn consume(&self) -> broadcast::Consumer {
		// The inherent `consume` shadows this trait method, so this delegates.
		self.consume()
	}
}

impl Consume<broadcast::Consumer> for broadcast::Consumer {
	fn consume(&self) -> broadcast::Consumer {
		self.clone()
	}
}

impl Consume<track::Consumer> for track::Producer {
	fn consume(&self) -> track::Consumer {
		self.consume()
	}
}

impl Consume<track::Consumer> for track::Consumer {
	fn consume(&self) -> track::Consumer {
		self.clone()
	}
}

/// Cheap read handle over an origin's broadcast tree.
///
/// Clones share the underlying tree state without allocating any per-cursor
/// resources. To actually receive announce / unannounce events, call
/// [`Self::announced`] to obtain an [`AnnounceConsumer`].
#[derive(Clone)]
pub struct Consumer {
	// Identity of the origin this consumer was derived from.
	info: Origin,
	nodes: OriginNodes,

	// A prefix that is automatically stripped from all paths.
	root: PathOwned,

	// Shared fallback request queue, fed to any `Dynamic` handler on the
	// producer side. Used only by `request_broadcast`; announced lookups ignore it.
	dynamic: kio::Shared<OriginDynamicState>,

	// Egress stats context. Broadcasts handed out through this consumer (and any
	// handle derived from them) are attributed to it (reads counted on the
	// publisher/egress side). Empty (no-op) unless a session tagged this handle.
	stats: stats::Session,

	// Data-plane split horizon: broadcasts resolved through this handle are
	// served from a source whose hop chain excludes this origin (the requesting
	// peer). `None` (the default) serves from the active source as usual.
	exclude: Option<Origin>,
}

impl std::ops::Deref for Consumer {
	type Target = Origin;

	fn deref(&self) -> &Self::Target {
		&self.info
	}
}

impl Consumer {
	fn new(
		info: Origin,
		root: PathOwned,
		nodes: OriginNodes,
		dynamic: kio::Shared<OriginDynamicState>,
		stats: stats::Session,
	) -> Self {
		Self {
			info,
			nodes,
			root,
			dynamic,
			stats,
			exclude: None,
		}
	}

	/// A clone that never serves the given peer its own data: broadcasts resolve
	/// to a source whose hop chain excludes `peer`, matching what the announce
	/// loop advertises to them. Sessions apply this once they learn the peer's
	/// origin id.
	pub(crate) fn excluding(mut self, peer: Origin) -> Self {
		self.exclude = Some(peer);
		self
	}

	/// Attach an egress stats context: broadcasts handed out through this handle (and
	/// any handle derived from it) are attributed to `session` on the publisher
	/// (egress) side. Pass [`stats::Session::default`] to opt out.
	pub fn with_stats(mut self, session: stats::Session) -> Self {
		self.stats = session;
		self
	}

	/// A clone of this consumer with its stats context cleared, so an internal
	/// lookup stream (e.g. [`Self::announced_broadcast`]) doesn't drive the egress
	/// announce guards; the caller re-attributes the result itself.
	fn untagged(&self) -> Self {
		Self {
			stats: stats::Session::default(),
			..self.clone()
		}
	}

	/// A view with this consumer's identity and root but no broadcasts:
	/// [`announced`](Self::announced) yields nothing. Used to answer a peer's
	/// announce-interest for a prefix outside our scope by announcing nothing,
	/// rather than tearing the stream down.
	pub(crate) fn empty(&self) -> Self {
		Self {
			info: self.info,
			nodes: OriginNodes { nodes: Vec::new() },
			root: self.root.clone(),
			dynamic: self.dynamic.clone(),
			stats: self.stats.clone(),
			exclude: self.exclude,
		}
	}

	/// Subscribe to announce / unannounce events for this consumer's subtree.
	///
	/// Allocates a per-cursor coalescing buffer, registers it with each root
	/// in this consumer's scope, and replays the currently active broadcast
	/// set as initial announcements. Drop the returned [`AnnounceConsumer`]
	/// to unregister.
	pub fn announced(&self) -> AnnounceConsumer {
		AnnounceConsumer::new(self.root.clone(), self.nodes.clone(), self.stats.clone())
	}

	/// Returns a cheap duplicate of this read handle.
	pub fn consume(&self) -> Self {
		self.clone()
	}

	/// Internal synchronous lookup: how the broadcast at `path` resolves for this
	/// consumer, telling "announced but every route loops back through you" apart
	/// from "nothing here".
	///
	/// Races announcement gossip (a freshly-connected consumer sees `Missing` even when
	/// the broadcast is about to arrive), so it is not public. [`Self::request_broadcast`]
	/// is the public lookup: it builds on this for the announced case, then falls back to
	/// a dynamic handler. [`Self::announced_broadcast`] waits for a future announcement.
	fn resolve(&self, path: impl AsPath) -> Resolved {
		let path = path.as_path();
		let Some((root, rest)) = self.nodes.get(&path) else {
			return Resolved::Missing;
		};
		let state = root.lock();
		state.resolve_broadcast(&rest, self.exclude)
	}

	/// [`Self::resolve`] reduced to "can I read it": the peek the tests assert on.
	#[cfg(test)]
	pub(crate) fn get_broadcast(&self, path: impl AsPath) -> Option<broadcast::Consumer> {
		match self.resolve(path) {
			Resolved::Found(broadcast) => Some(broadcast),
			Resolved::Excluded | Resolved::Missing => None,
		}
	}

	/// Block until a broadcast with the given path is announced and return it.
	///
	/// Returns `None` if the path is outside this consumer's allowed prefixes or if the consumer
	/// is closed before the broadcast is announced. The returned broadcast may itself be closed
	/// later. Subscribers should watch [`broadcast::Consumer::closed`] to react to that.
	///
	/// Prefer this over [`Self::request_broadcast`] when you know the exact path you want but
	/// cannot guarantee the announcement has already been received. With moq-lite-05 (and
	/// the older Lite01/02) `connect()` already blocks until the initial announce set lands,
	/// so [`Self::request_broadcast`] is race-free for broadcasts that were live at connect time;
	/// this method is still needed to wait for a broadcast that comes online *after* connect.
	pub async fn announced_broadcast(&self, path: impl AsPath) -> Option<broadcast::Consumer> {
		let path = path.as_path();

		// Scope a fresh consumer down to this path so we only wake up for relevant announcements.
		let consumer = self.scope(std::slice::from_ref(&path))?;

		// `scope` keeps narrower permissions intact: if we ask for `foo` on a consumer limited
		// to `foo/specific`, `scope` returns a consumer scoped to `foo/specific`. No
		// announcement at the exact path `foo` can ever arrive. Bail rather than loop forever.
		if !consumer.allowed().any(|allowed| path.has_prefix(allowed)) {
			return None;
		}

		// Use an untagged stream: this is a lookup, not egress announce forwarding, so
		// it must not drive the announce guards. The matched result is attributed
		// with the egress scope instead.
		let mut announced = consumer.untagged().announced();
		let scope = self.stats.egress(self.root.join(&path).to_owned());
		loop {
			let OriginAnnounce {
				path: announced_path,
				broadcast,
			} = announced.next().await?;
			// `scope` narrows by prefix, but we only want an exact-path match.
			if announced_path.as_path() == path
				&& let Some(broadcast) = broadcast
			{
				return Some(broadcast.with_stats(scope));
			}
		}
	}

	/// Returns a new Consumer restricted to broadcasts under one of `prefixes`.
	///
	/// Returns None if there are no legal prefixes (the requested prefixes are
	/// disjoint from this consumer's current scope, so it would always return None).
	// TODO accept PathPrefixes instead of &[Path]
	pub fn scope(&self, prefixes: &[Path]) -> Option<Consumer> {
		let prefixes = PathPrefixes::new(prefixes);
		Some(Consumer {
			info: self.info,
			root: self.root.clone(),
			nodes: self.nodes.select(&prefixes)?,
			dynamic: self.dynamic.clone(),
			stats: self.stats.clone(),
			exclude: self.exclude,
		})
	}

	/// Get a broadcast by path, falling back to a dynamic request when it is not announced.
	///
	/// Returns a [`kio::Pending`] future (resolved synchronously for an announced broadcast,
	/// otherwise once a handler serves it), mirroring [`track::Consumer::fetch_group`](track::Consumer::fetch_group).
	/// The lookup order is: an already-announced broadcast resolves
	/// immediately; otherwise, if an [`Dynamic`] handler is live (see
	/// [`Producer::dynamic`]), a fallback request is registered and the future resolves
	/// when the handler [`accept`](Request::accept)s it (or errors if it
	/// [`reject`](Request::reject)s or every handler drops). Concurrent requests for
	/// the same unannounced path coalesce onto one handler request, and once served the
	/// broadcast is cached weakly so *later* requests for that path also share it (rather
	/// than re-invoking the handler and opening a duplicate upstream subscription) for as
	/// long as it stays live; a closed one is re-served on the next request.
	///
	/// The returned future resolves to [`Error::Unroutable`] when the path is not announced and no
	/// dynamic handler exists. A request that is registered while a handler is live but then loses
	/// every handler before being served also resolves to [`Error::Unroutable`]. Unlike an announced
	/// broadcast, a dynamically served one is never visible to [`Self::announced`].
	pub fn request_broadcast(&self, path: impl AsPath) -> kio::Pending<Requesting> {
		let path = path.as_path();

		// Key requests by absolute path so a scoped/rooted consumer and the handler
		// (which may have a different root) agree on the same entry, and so the egress
		// counters resolve against the same broadcast the ingress side wrote.
		let absolute = self.root.join(&path).to_owned();
		let scope = self.stats.egress(&absolute);

		// Prefer a live announcement when one is present; the dynamic queue is only a
		// fallback for a path we hold nothing for. A broadcast we do hold but cannot
		// serve this requester is unroutable, not missing: a handler resolves paths
		// with no route chain to check, so falling through would let it route around
		// the split horizon and rebuild the loop.
		match self.resolve(&path) {
			Resolved::Found(broadcast) => return kio::Pending::new(Requesting::ready(broadcast).with_stats(scope)),
			Resolved::Excluded => return kio::Pending::new(Requesting::failed(Error::Unroutable)),
			Resolved::Missing => {}
		}

		let mut state = self.dynamic.lock();

		// Reuse a still-live broadcast a handler already served for this path, so repeat
		// requests share one upstream subscription. A closed entry is stale; `get` drops it
		// and returns `None`, so we fall through and re-serve below.
		if let Some(weak) = state.served.get(&absolute) {
			return kio::Pending::new(Requesting::ready(weak.consume()).with_stats(scope));
		}

		// Coalesce onto a pending request for the same path; otherwise register a new
		// one, unless there is no handler alive to serve it.
		let consumer = if let Some(producer) = state.requests.join(&absolute) {
			producer.consume()
		} else {
			let producer = kio::Producer::<PendingBroadcast>::default();
			let consumer = producer.consume();
			if state.requests.insert(absolute, producer).is_err() {
				return kio::Pending::new(Requesting::failed(Error::Unroutable));
			}
			consumer
		};

		kio::Pending::new(Requesting::pending(consumer).with_stats(scope))
	}

	/// Returns a new Consumer that automatically strips out the provided prefix.
	///
	/// Returns None if the provided root is not authorized; when [`Self::scope`] was
	/// already used without a wildcard.
	pub fn with_root(&self, prefix: impl AsPath) -> Option<Self> {
		let prefix = prefix.as_path();

		Some(Self {
			info: self.info,
			root: self.root.join(&prefix).to_owned(),
			nodes: self.nodes.root(&prefix)?,
			dynamic: self.dynamic.clone(),
			stats: self.stats.clone(),
			exclude: self.exclude,
		})
	}

	/// Returns the prefix that is automatically stripped from all paths.
	pub fn root(&self) -> &Path<'_> {
		&self.root
	}

	/// Iterate over the path prefixes this handle is permitted to publish or subscribe under.
	// TODO return PathPrefixes
	pub fn allowed(&self) -> impl Iterator<Item = &Path<'_>> {
		self.nodes.nodes.iter().map(|(root, _)| root)
	}

	/// Converts a relative path to an absolute path.
	pub fn absolute(&self, path: impl AsPath) -> Path<'_> {
		self.root.join(path)
	}
}

/// Handle to the announcement stream for a subtree.
///
/// Symmetric counterpart of [`AnnounceConsumer`]. Cheap to clone; call
/// [`Self::consume`] to obtain an [`AnnounceConsumer`] that receives events.
#[derive(Clone)]
pub struct AnnounceProducer {
	nodes: OriginNodes,
	root: PathOwned,
}

impl AnnounceProducer {
	fn new(root: PathOwned, nodes: OriginNodes) -> Self {
		Self { nodes, root }
	}

	/// Subscribe to announce / unannounce events for this subtree.
	///
	/// Allocates a per-cursor coalescing buffer and replays the currently active broadcast set
	/// as initial announcements. Drop the returned [`AnnounceConsumer`] to
	/// unregister.
	pub fn consume(&self) -> AnnounceConsumer {
		// Untagged: `AnnounceProducer` is used for internal announce plumbing, not
		// egress attribution (which flows through `origin::Consumer::announced`).
		AnnounceConsumer::new(self.root.clone(), self.nodes.clone(), stats::Session::default())
	}

	/// Returns the prefix that is automatically stripped from announced paths.
	pub fn root(&self) -> &Path<'_> {
		&self.root
	}
}

/// Receives announce / unannounce events for a subtree.
///
/// Created by [`Consumer::announced`] or [`AnnounceProducer::consume`].
/// Drop to unregister.
pub struct AnnounceConsumer {
	id: ConsumerId,
	nodes: OriginNodes,
	root: PathOwned,

	// Pending updates queued for this cursor. Coalesced so a slow consumer
	// can't accumulate redundant announce/unannounce pairs.
	state: kio::Producer<OriginConsumerState>,

	// Egress stats context (empty for an untagged stream). Announce events drive the
	// per-broadcast announce guards below and tag the broadcasts handed out.
	stats: stats::Session,

	// Live egress announce guards, keyed by absolute broadcast path. An announce
	// opens one (bumping `announced` + `announced_bytes`); the matching unannounce
	// drops it (bumping `announced_closed` + `announced_bytes`).
	guards: HashMap<PathOwned, stats::Announce>,
}

impl AnnounceConsumer {
	fn new(root: PathOwned, nodes: OriginNodes, stats: stats::Session) -> Self {
		let state = kio::Producer::<OriginConsumerState>::default();
		let id = ConsumerId::new();

		for (_, node) in &nodes.nodes {
			let notify = AnnounceConsumerNotify {
				root: root.clone(),
				state: state.clone(),
			};
			node.lock().consume(id, notify);
		}

		Self {
			id,
			nodes,
			root,
			state,
			stats,
			guards: HashMap::new(),
		}
	}

	/// Drive the egress announce guards and tag the broadcast for one update.
	///
	/// An announce opens a guard (keyed by absolute path) and tags the yielded
	/// broadcast with the egress scope; an unannounce drops the guard. A no-op for
	/// an untagged stream.
	fn attribute(&mut self, update: OriginAnnounce) -> OriginAnnounce {
		let OriginAnnounce { path, broadcast } = update;
		let absolute = self.root.join(&path).to_owned();
		match broadcast {
			Some(broadcast) => {
				let scope = self.stats.egress(&absolute);
				self.guards.entry(absolute).or_insert_with(|| scope.announce());
				OriginAnnounce {
					path,
					broadcast: Some(broadcast.with_stats(scope)),
				}
			}
			None => {
				self.guards.remove(&absolute);
				OriginAnnounce { path, broadcast: None }
			}
		}
	}

	/// Returns the next (un)announced broadcast and its path relative to this
	/// cursor's root.
	///
	/// The broadcast will only be announced if it was previously unannounced.
	/// The same path won't be announced/unannounced twice in a row; instead it
	/// toggles. Returns None if the cursor is closed.
	pub async fn next(&mut self) -> Option<OriginAnnounce> {
		kio::wait(|waiter| self.poll_next(waiter)).await
	}

	/// Poll for the next (un)announced broadcast, without blocking.
	///
	/// Returns `Poll::Ready(Some(_))` for an update, `Poll::Ready(None)` if the
	/// cursor is closed, or `Poll::Pending` after registering `waiter` to be
	/// notified when the next update arrives.
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<Option<OriginAnnounce>> {
		let update = {
			let mut state = match ready!(self.state.poll(waiter, |state| {
				if state.pending.is_empty() {
					Poll::Pending
				} else {
					Poll::Ready(())
				}
			})) {
				Ok(state) => state,
				// Closed: discard the Ref so its MutexGuard doesn't escape this call.
				Err(_) => return Poll::Ready(None),
			};
			state.take().expect("predicate guaranteed an update")
		};
		Poll::Ready(Some(self.attribute(update)))
	}

	/// Returns the next (un)announced broadcast without blocking.
	///
	/// Returns None if there is no update available; NOT because the cursor is closed.
	/// Use [`Self::is_closed`] to check if the cursor is closed.
	pub fn try_next(&mut self) -> Option<OriginAnnounce> {
		let update = self.state.write().ok()?.take()?;
		Some(self.attribute(update))
	}

	/// Returns true if the cursor is closed (no more updates will arrive).
	pub fn is_closed(&self) -> bool {
		self.state.write().is_err()
	}

	/// Returns the prefix that is automatically stripped from emitted paths.
	pub fn root(&self) -> &Path<'_> {
		&self.root
	}

	/// Converts a relative path to an absolute path.
	pub fn absolute(&self, path: impl AsPath) -> Path<'_> {
		self.root.join(path)
	}
}

impl Drop for AnnounceConsumer {
	fn drop(&mut self) {
		for (_, root) in &self.nodes.nodes {
			root.lock().unconsume(self.id);
		}
	}
}

#[cfg(test)]
use futures::FutureExt;

#[cfg(test)]
#[allow(missing_docs)] // test-only assertion helpers
impl AnnounceConsumer {
	pub fn assert_next(&mut self, expected: impl AsPath, broadcast: &broadcast::Consumer) {
		let expected = expected.as_path();
		let announce = self.next().now_or_never().expect("next blocked").expect("no next");
		assert_eq!(announce.path, expected, "wrong path");
		let announced = announce.broadcast.expect("should be an active announce");
		assert!(announced.is_clone(broadcast), "should be the same broadcast");
	}

	/// An announce for `expected`, without asserting which broadcast backs it
	/// (the origin owns the announced broadcast, not the publisher). Returns the
	/// announced consumer.
	pub fn assert_next_some(&mut self, expected: impl AsPath) -> broadcast::Consumer {
		let expected = expected.as_path();
		let announce = self.next().now_or_never().expect("next blocked").expect("no next");
		assert_eq!(announce.path, expected, "wrong path");
		announce.broadcast.expect("should be an active announce")
	}

	pub fn assert_try_next(&mut self, expected: impl AsPath, broadcast: &broadcast::Consumer) {
		let expected = expected.as_path();
		let announce = self.try_next().expect("no next");
		assert_eq!(announce.path, expected, "wrong path");
		let announced = announce.broadcast.expect("should be an active announce");
		assert!(announced.is_clone(broadcast), "should be the same broadcast");
	}

	/// The `try_next` counterpart of [`Self::assert_next_some`].
	pub fn assert_try_next_some(&mut self, expected: impl AsPath) -> broadcast::Consumer {
		let expected = expected.as_path();
		let announce = self.try_next().expect("no next");
		assert_eq!(announce.path, expected, "wrong path");
		announce.broadcast.expect("should be an active announce")
	}

	pub fn assert_next_none(&mut self, expected: impl AsPath) {
		let expected = expected.as_path();
		let announce = self.next().now_or_never().expect("next blocked").expect("no next");
		assert_eq!(announce.path, expected, "wrong path");
		assert!(announce.broadcast.is_none(), "should be unannounced");
	}

	pub fn assert_next_wait(&mut self) {
		if let Some(res) = self.next().now_or_never() {
			panic!("next should block: got {:?}", res.map(|a| a.path));
		}
	}

	/*
	pub fn assert_next_closed(&mut self) {
		assert!(
			self.next().now_or_never().expect("next blocked").is_none(),
			"next should be closed"
		);
	}
	*/
}

#[cfg(test)]
mod tests {
	use crate::coding::Decode;
	use crate::group;

	use super::*;

	/// An announced direct route.
	fn announce() -> broadcast::Route {
		broadcast::Route::new().with_announce(true)
	}

	/// The first origin whose handover key for `name` sits above (`true`) or below
	/// (`false`) the peer's, so tests exercising the carrying gate are
	/// deterministic instead of hinging on a random id winning a hash comparison.
	/// Starts searching above the small ids the tests use in hop chains, so the
	/// result never collides with a hop (a looping chain trips a debug_assert).
	fn origin_keyed(name: &str, peer: Origin, above: bool) -> Origin {
		let name = Path::new(name);
		let peer_key = fnv_key(&name, [peer]);
		(100u64..)
			.map(|id| Origin::new(id).unwrap())
			.find(|origin| (fnv_key(&name, [*origin]) > peer_key) == above)
			.unwrap()
	}

	/// A front table for reselect tests: routes get ids in order, the first is
	/// the incumbent.
	fn front_state(self_origin: Origin, routes: Vec<broadcast::Route>) -> FrontState {
		let source = broadcast::Info::new().produce().consume();
		FrontState {
			path: Path::new("test").to_owned(),
			self_origin,
			publisher: routes.first().and_then(|r| r.hops.iter().next().copied()),
			next_route: routes.len() as u64,
			excluded: HashMap::new(),
			routes: routes
				.into_iter()
				.enumerate()
				.map(|(id, route)| FrontRoute {
					id: id as u64,
					route,
					source: source.clone(),
				})
				.collect(),
			active: Some(0),
			linger: Duration::ZERO,
			closed: false,
		}
	}

	/// A route as a warm sibling would announce it: zero cost, chain ending at
	/// the announcing peer.
	fn sibling_route(peer: Origin) -> broadcast::Route {
		let hops = OriginList::try_from(vec![Origin::new(90).unwrap(), peer]).unwrap();
		announce().with_hops(hops)
	}

	/// A route as the upstream announces it: priced, one hop.
	fn upstream_route(cost: u64) -> broadcast::Route {
		let hops = OriginList::try_from(vec![Origin::new(90).unwrap()]).unwrap();
		announce().with_hops(hops).with_cost(cost)
	}

	/// While carrying, a strictly cheaper route from a peer that hashes above us
	/// must not displace the incumbent; the same table re-parents freely once
	/// idle, or when the peer hashes below us.
	#[test]
	fn test_carrying_gate_keys() {
		let peer = Origin::new(3).unwrap();

		// We lose the key comparison: stay put while carrying, migrate when idle.
		let mut lost = front_state(
			origin_keyed("test", peer, false),
			vec![upstream_route(10), sibling_route(peer)],
		);
		lost.reselect(true);
		assert_eq!(
			lost.active,
			Some(0),
			"carrying front re-parented onto a higher-keyed peer"
		);
		lost.reselect(false);
		assert_eq!(lost.active, Some(1), "idle front must take the cheaper route");

		// We win the key comparison: re-parent even while carrying.
		let mut won = front_state(
			origin_keyed("test", peer, true),
			vec![upstream_route(10), sibling_route(peer)],
		);
		won.reselect(true);
		assert_eq!(won.active, Some(1), "carrying front must follow a lower-keyed peer");
	}

	/// The simultaneous-activation race: two relays that each pulled the same
	/// broadcast independently see each other's zero-cost route. Exactly one of
	/// them re-parents; the other keeps its upstream, so the broadcast is never
	/// left without a source.
	#[test]
	fn test_carrying_gate_symmetric_race() {
		let a = Origin::new(1).unwrap();
		let b = Origin::new(2).unwrap();

		let mut a_view = front_state(a, vec![upstream_route(10), sibling_route(b)]);
		let mut b_view = front_state(b, vec![upstream_route(10), sibling_route(a)]);
		a_view.reselect(true);
		b_view.reselect(true);

		let a_moved = a_view.active == Some(1);
		let b_moved = b_view.active == Some(1);
		assert!(
			a_moved != b_moved,
			"exactly one side must re-parent (a: {a_moved}, b: {b_moved})"
		);
	}

	/// The gate is scoped to warm siblings: a cheaper route via a relay that is
	/// not itself carrying (advertised nonzero), or directly from the original
	/// publisher (single-hop chain), is taken immediately even while carrying
	/// and even when we would lose the key comparison.
	#[test]
	fn test_carrying_switches_to_benign_routes() {
		let peer = Origin::new(3).unwrap();
		let lost = origin_keyed("test", peer, false);

		// A cheaper forwarder path: the relay advertised its accumulated cost.
		let mut forwarder = sibling_route(peer).with_cost(4);
		forwarder.advertised = 4;
		let mut state = front_state(lost, vec![upstream_route(10), forwarder]);
		state.reselect(true);
		assert_eq!(
			state.active,
			Some(1),
			"a cheaper forwarder path must win while carrying"
		);

		// Directly from the original publisher: single-hop chain, advertised zero.
		let direct = announce().with_hops(OriginList::try_from(vec![peer]).unwrap());
		let mut state = front_state(lost, vec![upstream_route(10), direct]);
		state.reselect(true);
		assert_eq!(
			state.active,
			Some(1),
			"a direct publisher route must win while carrying"
		);

		// The peer's session reconnecting: same chain, same cost, so the gate's
		// strictly-cheaper test does not apply and recency decides. Loosening that
		// test to `<=` would hold a carrying front on the dead session until the
		// transport timed it out, which is the whole point of the recency order.
		let mut state = front_state(lost, vec![sibling_route(peer), sibling_route(peer)]);
		state.reselect(true);
		assert_eq!(
			state.active,
			Some(1),
			"a reconnect on an identical chain must win while carrying"
		);
	}

	/// The gate only protects an announced incumbent: one that lost its announce
	/// (the upstream retracted) is displaced regardless of the key comparison.
	#[test]
	fn test_carrying_gate_ignores_unannounced_incumbent() {
		let peer = Origin::new(3).unwrap();
		let unannounced = upstream_route(10).with_announce(false);
		let mut state = front_state(
			origin_keyed("test", peer, false),
			vec![unannounced, sibling_route(peer)],
		);
		state.reselect(true);
		assert_eq!(
			state.active,
			Some(1),
			"an unannounced incumbent must always be displaced"
		);
	}

	/// Let the spawned origin tasks (source watchers, front dispatch) run. The
	/// tests pause tokio time, so this advances the clock instantly.
	async fn settle() {
		tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
	}

	/// Serve one requested track from a source like a session would: wait for the
	/// origin to dispatch it, then accept with default info.
	async fn accept_track(dynamic: &mut broadcast::Dynamic, name: &str) -> track::Producer {
		let request = tokio::time::timeout(std::time::Duration::from_secs(1), dynamic.requested_track())
			.await
			.expect("timed out waiting for a track request")
			.expect("source closed");
		assert_eq!(request.name(), name, "unexpected track dispatched");
		request.accept(None)
	}

	/// Tagging both origin handles with one context attributes the full model path:
	/// ingress writes on the subscriber side, egress reads on the publisher side,
	/// each counter landing exactly once (the model-layer silent-zero guard).
	#[tokio::test]
	async fn test_stats_tagged_end_to_end() {
		use crate::Timestamp;
		use crate::stats::{Config, Registry, Tier};
		use bytes::Bytes;

		tokio::time::pause();

		let registry = Registry::new(Config::new());
		let ctx = registry.tier(Tier::default()).session("acme");

		let origin = Origin::random().produce();
		let ingress = origin.clone().with_stats(ctx.clone());
		let egress = origin.consume().with_stats(ctx.clone());

		// Egress announce stream: this is the tagged stream that drives the egress
		// announce guard.
		let mut announced = egress.announced();

		// Ingress publishes an announced broadcast.
		let source = ingress.create_broadcast("demo", announce()).unwrap();
		let mut dynamic = source.dynamic();
		settle().await;
		settle().await;

		// Egress observes the announce and gets the tagged broadcast.
		let update = announced.next().await.unwrap();
		assert_eq!(update.path.as_str(), "demo");
		let broadcast = update.broadcast.unwrap();

		// Egress subscribes; the ingress side serves the track on demand.
		let subscribing = broadcast.track("video").unwrap().subscribe(None);
		let mut producer = accept_track(&mut dynamic, "video").await;
		settle().await;
		let mut sub = subscribing.await.unwrap();

		// Ingress writes one group with two 5-byte frames.
		let mut group = producer.append_group().unwrap();
		group
			.write_frame(Timestamp::ZERO, Bytes::from_static(b"hello"))
			.unwrap();
		group
			.write_frame(Timestamp::ZERO, Bytes::from_static(b"world"))
			.unwrap();
		group.finish().unwrap();

		// Egress reads the group and both frames.
		let mut group_c = sub.recv_group().await.unwrap().unwrap();
		let mut frames = 0;
		while let Some(frame) = group_c.read_frame().await.unwrap() {
			assert_eq!(frame.payload.len(), 5);
			frames += 1;
		}
		assert_eq!(frames, 2);
		settle().await;

		let report = registry.report();
		let entry = report
			.traffic
			.iter()
			.find(|e| e.path.as_str() == "demo")
			.expect("demo tracked");
		let path_len = "demo".len() as u64;

		// Egress (publisher side): reads out of the model.
		let egress = &entry.publisher;
		assert_eq!(egress.announced, 1, "one egress announce");
		assert_eq!(egress.announced_bytes, path_len);
		assert_eq!(egress.subscriptions, 1, "one egress subscription");
		assert_eq!(egress.broadcasts, 1, "one viewer");
		assert_eq!(egress.groups, 1);
		assert_eq!(egress.frames, 2);
		assert_eq!(egress.bytes, 10);
		assert_eq!(egress.fetches, 0);

		// Ingress (subscriber side): writes into the model.
		let ingress = &entry.subscriber;
		assert_eq!(ingress.announced, 1, "one ingress announce");
		assert_eq!(ingress.announced_bytes, path_len);
		assert_eq!(ingress.subscriptions, 1, "one ingress track");
		assert_eq!(ingress.broadcasts, 0, "ingress has no viewer refcount");
		assert_eq!(ingress.groups, 1);
		assert_eq!(ingress.frames, 2);
		assert_eq!(ingress.bytes, 10);

		// A fetch bumps only `fetches` on the egress side, plus the delivered group.
		let fetched = broadcast.track("video").unwrap().fetch_group(0, None).await.unwrap();
		let _ = fetched;
		settle().await;
		let report = registry.report();
		let entry = report.traffic.iter().find(|e| e.path.as_str() == "demo").unwrap();
		assert_eq!(entry.publisher.fetches, 1, "one fetch");
		assert_eq!(entry.publisher.subscriptions, 1, "fetch does not bump subscriptions");
		assert_eq!(entry.publisher.broadcasts, 1, "fetch does not bump the viewer refcount");
		// `fetches` is egress-only for the same structural reason as `broadcasts`:
		// only a `track::Consumer` can fetch, and the ingress scope never reaches one
		// (`broadcast::Producer::consume` hands out an untagged consumer).
		assert_eq!(entry.subscriber.fetches, 0, "ingress cannot fetch");
	}

	/// `Subscriber::read_frame` collapses a group to its first frame. The paths it
	/// delegates to (plain and spliced) build their own *unmetered* group consumers,
	/// so the wrapper is the only place that can attribute the read: exactly one
	/// group, one frame, and the payload bytes, counted once each.
	#[tokio::test]
	async fn test_stats_read_frame_counts_once() {
		use crate::Timestamp;
		use crate::stats::{Config, Registry, Tier};
		use bytes::Bytes;

		tokio::time::pause();

		let registry = Registry::new(Config::new());
		let ctx = registry.tier(Tier::default()).session("acme");

		let origin = Origin::random().produce();
		let ingress = origin.clone().with_stats(ctx.clone());
		let egress = origin.consume().with_stats(ctx.clone());

		let mut announced = egress.announced();
		let source = ingress.create_broadcast("demo", announce()).unwrap();
		let mut dynamic = source.dynamic();
		settle().await;
		settle().await;

		let broadcast = announced.next().await.unwrap().broadcast.unwrap();
		let subscribing = broadcast.track("video").unwrap().subscribe(None);
		let mut producer = accept_track(&mut dynamic, "video").await;
		settle().await;
		let mut sub = subscribing.await.unwrap();

		// A single-frame group, read back through the collapsing helper.
		producer
			.write_frame(Timestamp::ZERO, Bytes::from_static(b"hello"))
			.unwrap();

		let frame = sub.read_frame().await.unwrap().expect("frame");
		assert_eq!(frame.payload.len(), 5);
		settle().await;

		let report = registry.report();
		let entry = report
			.traffic
			.iter()
			.find(|e| e.path.as_str() == "demo")
			.expect("demo tracked");
		assert_eq!(entry.publisher.groups, 1, "one group, counted once");
		assert_eq!(entry.publisher.frames, 1, "one frame, counted once");
		assert_eq!(
			entry.publisher.bytes, 5,
			"payload counted once, not zero and not doubled"
		);
	}

	/// Datagrams bypass the group/frame handles entirely, so they're metered at the
	/// producer (ingress write) and the subscriber (egress read). Each one counts as
	/// the single-frame group it stands in for, plus the `datagrams` breakout.
	#[tokio::test]
	async fn test_stats_datagrams_counted_both_sides() {
		use crate::Timestamp;
		use crate::stats::{Config, Registry, Tier};

		tokio::time::pause();

		let registry = Registry::new(Config::new());
		let ctx = registry.tier(Tier::default()).session("acme");

		let origin = Origin::random().produce();
		let ingress = origin.clone().with_stats(ctx.clone());
		let egress = origin.consume().with_stats(ctx.clone());

		let mut announced = egress.announced();
		let source = ingress.create_broadcast("demo", announce()).unwrap();
		let mut dynamic = source.dynamic();
		settle().await;
		settle().await;

		let broadcast = announced.next().await.unwrap().broadcast.unwrap();
		let subscribing = broadcast.track("video").unwrap().subscribe(None);
		let mut producer = accept_track(&mut dynamic, "video").await;
		settle().await;
		let mut sub = subscribing.await.unwrap();

		producer.append_datagram(Timestamp::ZERO, &b"hello"[..]).unwrap();
		let datagram = sub.recv_datagram().await.unwrap().expect("datagram");
		assert_eq!(&datagram.payload[..], b"hello");
		settle().await;

		let report = registry.report();
		let entry = report
			.traffic
			.iter()
			.find(|e| e.path.as_str() == "demo")
			.expect("demo tracked");

		for (side, traffic) in [("egress", &entry.publisher), ("ingress", &entry.subscriber)] {
			assert_eq!(traffic.datagrams, 1, "{side}: one datagram");
			assert_eq!(traffic.groups, 1, "{side}: counted as its single-frame group");
			assert_eq!(traffic.frames, 1, "{side}: one frame");
			assert_eq!(traffic.bytes, 5, "{side}: payload counted once");
		}
	}

	#[test]
	fn origin_rejects_reserved_ids() {
		assert!(Origin::new(0).is_err());
		assert!(Origin::new(1u64 << 62).is_err());
		assert_eq!(Origin::new(1).unwrap().id(), 1);

		let mut zero = [0u8].as_slice();
		assert_eq!(
			Origin::decode(&mut zero, crate::lite::Version::Lite05).unwrap(),
			Origin::UNKNOWN
		);
	}

	#[test]
	fn origin_list_push_fails_at_limit() {
		let mut list = OriginList::new();
		for _ in 0..MAX_HOPS {
			list.push(Origin::random()).unwrap();
		}
		assert_eq!(list.len(), MAX_HOPS);
		assert_eq!(list.push(Origin::random()), Err(TooManyOrigins));
	}

	#[test]
	fn origin_list_replace_first() {
		let mut list = OriginList::new();
		for _ in 0..3 {
			list.push(Origin::UNKNOWN).unwrap();
		}

		// Rewrites only the first placeholder, keeping the length the same.
		assert!(list.replace_first(Origin::UNKNOWN, Origin::new(7).unwrap()));
		assert_eq!(
			list.as_slice(),
			&[Origin::new(7).unwrap(), Origin::UNKNOWN, Origin::UNKNOWN]
		);

		// No match leaves the list untouched.
		assert!(!list.replace_first(Origin::new(99).unwrap(), Origin::new(8).unwrap()));
		assert_eq!(list.len(), 3);
	}

	#[test]
	fn origin_list_try_from_vec_enforces_limit() {
		let under: Vec<Origin> = (0..MAX_HOPS).map(|_| Origin::random()).collect();
		assert!(OriginList::try_from(under).is_ok());

		let over: Vec<Origin> = (0..MAX_HOPS + 1).map(|_| Origin::random()).collect();
		assert_eq!(OriginList::try_from(over), Err(TooManyOrigins));
	}

	#[tokio::test]
	async fn test_announce() {
		tokio::time::pause();

		let origin = Origin::random().produce();

		let mut consumer1 = origin.consume().announced();
		consumer1.assert_next_wait();

		// Publish the first broadcast; it becomes visible asynchronously.
		let mut broadcast1 = origin.create_broadcast("test1", announce()).unwrap();
		settle().await;

		consumer1.assert_next_some("test1");
		consumer1.assert_next_wait();

		// Make a new consumer that should get the existing broadcast.
		// But we don't consume it yet.
		let mut consumer2 = origin.consume().announced();

		// Publish the second broadcast.
		let mut broadcast2 = origin.create_broadcast("test2", announce()).unwrap();
		settle().await;

		consumer1.assert_next_some("test2");
		consumer1.assert_next_wait();

		consumer2.assert_next_some("test1");
		consumer2.assert_next_some("test2");
		consumer2.assert_next_wait();

		// Finish the first broadcast: a graceful end unannounces immediately.
		broadcast1.finish();
		settle().await;

		// All consumers should get a None now.
		consumer1.assert_next_none("test1");
		consumer2.assert_next_none("test1");
		consumer1.assert_next_wait();
		consumer2.assert_next_wait();

		// And a new consumer only gets the last broadcast.
		let mut consumer3 = origin.consume().announced();
		consumer3.assert_next_some("test2");
		consumer3.assert_next_wait();

		broadcast2.finish();
		settle().await;

		consumer1.assert_next_none("test2");
		consumer2.assert_next_none("test2");
		consumer3.assert_next_none("test2");
	}

	/// Multiple sources created at one path feed a single origin-owned broadcast:
	/// one announce, no churn as sources come and go, and an unannounce only when
	/// the last source leaves.
	#[tokio::test]
	async fn test_duplicate() {
		tokio::time::pause();

		let origin = Origin::random().produce();
		let consumer = origin.consume();
		let mut announced = consumer.announced();

		let mut broadcast1 = origin.create_broadcast("test", announce()).unwrap();
		let mut broadcast2 = origin.create_broadcast("test", announce()).unwrap();
		let mut broadcast3 = origin.create_broadcast("test", announce()).unwrap();
		settle().await;
		assert!(consumer.get_broadcast("test").is_some());

		announced.assert_next_some("test");
		announced.assert_next_wait();

		// A standby source finishing changes nothing.
		broadcast2.finish();
		settle().await;
		assert!(consumer.get_broadcast("test").is_some());
		announced.assert_next_wait();

		// The active source finishing hands over to a survivor, invisibly.
		broadcast1.finish();
		settle().await;
		assert!(consumer.get_broadcast("test").is_some());
		announced.assert_next_wait();

		// The last source finishing unannounces and removes the broadcast.
		broadcast3.finish();
		settle().await;
		assert!(consumer.get_broadcast("test").is_none());

		announced.assert_next_none("test");
		announced.assert_next_wait();
	}

	/// A source dying mid-serve fails over: the track re-splices from the standby
	/// source and resumes exactly at the first missing group.
	#[tokio::test]
	async fn test_route_failover() {
		tokio::time::pause();

		let origin = Origin::random().produce();
		let consumer = origin.consume();
		let mut announced = consumer.announced();

		// Both routes share the first hop (the original publisher): only
		// interchangeable content may join as a standby.
		let hops_a = OriginList::try_from(vec![Origin::new(1).unwrap()]).unwrap();
		let hops_b = OriginList::try_from(vec![Origin::new(1).unwrap(), Origin::new(3).unwrap()]).unwrap();

		// The first source announces the broadcast.
		let source_a = origin.create_broadcast("test", announce().with_hops(hops_a)).unwrap();
		let mut dynamic_a = source_a.dynamic();
		settle().await;
		settle().await;
		let broadcast = consumer.request_broadcast("test").await.unwrap();
		announced.assert_next_some("test");

		// A second (longer) source joins silently as a standby.
		let source_b = origin.create_broadcast("test", announce().with_hops(hops_b)).unwrap();
		let mut dynamic_b = source_b.dynamic();
		settle().await;
		settle().await;
		announced.assert_next_wait();

		// Subscribing dispatches the track to the best source (A).
		let subscribing = broadcast.track("video").unwrap().subscribe(None);
		let mut producer = accept_track(&mut dynamic_a, "video").await;
		settle().await;
		dynamic_b.assert_no_request();

		let mut sub = subscribing.await.unwrap();
		// Demand registers as the subscriber polls; a fresh segment carries no
		// boundary, so the demand is the subscriber's own.
		sub.assert_no_group();
		assert_eq!(producer.subscription().unwrap().group_start, None);

		producer.append_group().unwrap();
		producer.append_group().unwrap();
		assert_eq!(sub.assert_group().sequence, 0);
		assert_eq!(sub.assert_group().sequence, 1);

		// Source A dies (session loss): the track re-splices from B and nothing
		// is announced.
		// abort() consumes the producer, so this both aborts and drops it.
		producer.abort(Error::Dropped).unwrap();
		source_a.abort(Error::Dropped).unwrap();
		drop(dynamic_a);
		settle().await;
		announced.assert_next_wait();

		// The new copy resumes one past the spliced groups: its demand starts at
		// the boundary, and groups the old source already delivered are filtered.
		let mut producer = accept_track(&mut dynamic_b, "video").await;
		settle().await;
		sub.assert_no_group();
		assert_eq!(producer.subscription().unwrap().group_start, Some(2));
		producer.create_group(group::Info { sequence: 1 }).unwrap();
		producer.create_group(group::Info { sequence: 2 }).unwrap();
		assert_eq!(sub.assert_group().sequence, 2, "groups below the boundary are filtered");
		sub.assert_not_closed();
	}

	/// `route_changed` yields the current route first, then each change; equal
	/// updates coalesce, and the watch errors once every producer is gone.
	#[tokio::test]
	async fn test_broadcast_route_watch() {
		let mut producer = broadcast::Info::new().produce();
		let mut consumer = producer.consume();

		// Initial value: the default route.
		assert_eq!(consumer.route_changed().await.unwrap(), broadcast::Route::default());

		// An equal update is a no-op.
		producer.set_route(broadcast::Route::default()).unwrap();
		assert!(consumer.route_changed().now_or_never().is_none());

		let mut hops = OriginList::new();
		hops.push(Origin::new(7).unwrap()).unwrap();
		let route = broadcast::Route::new().with_hops(hops).with_cost(3);
		producer.set_route(route.clone()).unwrap();
		assert_eq!(consumer.route_changed().await.unwrap(), route);

		// A fresh consumer sees the current value immediately.
		let mut fresh = producer.consume();
		assert_eq!(fresh.route_changed().await.unwrap(), route);

		drop(producer);
		assert!(matches!(consumer.route_changed().await.unwrap_err(), Error::Dropped));
	}

	/// A cost update that flips the winning source hands live tracks over at a
	/// group boundary and re-advertises the broadcast's route, without announce
	/// churn.
	#[tokio::test]
	async fn test_route_cost_update() {
		tokio::time::pause();

		// The takeover happens while a subscriber is live (carrying), so the local
		// origin must win the handover key comparison against B's announcing hop
		// (origin 3); a random id would flake on the hash.
		let origin = Info::new(origin_keyed("test", Origin::new(3).unwrap(), true)).produce();
		let consumer = origin.consume();
		let mut announced = consumer.announced();

		// Both routes share the first hop (the original publisher): only
		// interchangeable content may join as a standby.
		let hops_a = OriginList::try_from(vec![Origin::new(1).unwrap()]).unwrap();
		let hops_b = OriginList::try_from(vec![Origin::new(1).unwrap(), Origin::new(3).unwrap()]).unwrap();

		// A (shorter chain) wins at equal cost.
		let mut source_a = origin
			.create_broadcast("test", announce().with_hops(hops_a.clone()))
			.unwrap();
		let mut dynamic_a = source_a.dynamic();
		settle().await;
		let broadcast = consumer.request_broadcast("test").await.unwrap();
		announced.assert_next_some("test");

		let mut watch = broadcast.clone();
		assert_eq!(watch.route_changed().await.unwrap().hops, hops_a);

		let mut source_b = origin
			.create_broadcast("test", announce().with_hops(hops_b.clone()))
			.unwrap();
		let mut dynamic_b = source_b.dynamic();
		settle().await;
		assert!(
			watch.route_changed().now_or_never().is_none(),
			"a losing standby must not change the advertised route"
		);

		// Dispatch the track to A and deliver a group.
		let subscribing = broadcast.track("video").unwrap().subscribe(None);
		let mut producer = accept_track(&mut dynamic_a, "video").await;
		settle().await;
		let mut sub = subscribing.await.unwrap();
		producer.append_group().unwrap();
		assert_eq!(sub.assert_group().sequence, 0);

		// A's cost rises above B's: B takes over at the boundary and the
		// broadcast re-advertises B's route. No announce events.
		source_a
			.set_route(announce().with_hops(hops_a.clone()).with_cost(10))
			.unwrap();
		settle().await;
		assert_eq!(watch.route_changed().await.unwrap().hops, hops_b);
		announced.assert_next_wait();

		let mut producer_b = accept_track(&mut dynamic_b, "video").await;
		settle().await;
		// Demand registers as the subscriber polls; the new segment starts at the
		// splice boundary.
		sub.assert_no_group();
		assert_eq!(producer_b.subscription().unwrap().group_start, Some(1));
		producer_b.create_group(group::Info { sequence: 1 }).unwrap();
		assert_eq!(sub.assert_group().sequence, 1);
		sub.assert_not_closed();

		// The active source updating its own metadata re-advertises in place.
		source_b
			.set_route(announce().with_hops(hops_b.clone()).with_cost(5))
			.unwrap();
		settle().await;
		let advertised = watch.route_changed().await.unwrap();
		assert_eq!(advertised.hops, hops_b);
		assert_eq!(advertised.cost, 5);
		announced.assert_next_wait();
	}

	/// A track completed for good must survive later source churn: it is never
	/// re-dispatched, and late subscribers still see a clean end.
	#[tokio::test]
	async fn test_completed_track_survives_route_churn() {
		tokio::time::pause();

		let origin = Origin::random().produce();
		let consumer = origin.consume();

		// Shared first hop, so B is a standby rather than a parked replacement.
		let hops_a = OriginList::try_from(vec![Origin::new(1).unwrap()]).unwrap();
		let hops_b = OriginList::try_from(vec![Origin::new(1).unwrap(), Origin::new(3).unwrap()]).unwrap();

		let source_a = origin.create_broadcast("test", announce().with_hops(hops_a)).unwrap();
		let mut dynamic_a = source_a.dynamic();
		settle().await;
		let source_b = origin.create_broadcast("test", announce().with_hops(hops_b)).unwrap();
		let mut dynamic_b = source_b.dynamic();
		settle().await;
		settle().await;
		let broadcast = consumer.request_broadcast("test").await.unwrap();

		// Serve the track via A and end it for good.
		let subscribing = broadcast.track("video").unwrap().subscribe(None);
		let mut producer = accept_track(&mut dynamic_a, "video").await;
		settle().await;
		let mut sub = subscribing.await.unwrap();
		producer.append_group().unwrap();
		assert_eq!(sub.assert_group().sequence, 0);
		producer.finish().unwrap();
		drop(producer);
		settle().await;
		sub.assert_closed();

		// A detaching must not re-dispatch the finished track to B.
		source_a.abort(Error::Dropped).unwrap();
		drop(dynamic_a);
		settle().await;
		dynamic_b.assert_no_request();

		// A late subscriber sees the same clean end, not an abort.
		let mut late = broadcast.track("video").unwrap().subscribe(None).await.unwrap();
		late.assert_closed();
	}

	/// A source rejecting a track ends the logical track immediately, with the
	/// source's own error: which tracks a broadcast carries is the publisher's
	/// contract, so there is no sweep of other routes and no retry budget.
	#[tokio::test]
	async fn test_refused_track_aborts_instantly() {
		tokio::time::pause();

		let origin = Origin::random().produce();
		let consumer = origin.consume();

		let hops = OriginList::try_from(vec![Origin::new(1).unwrap()]).unwrap();
		let source = origin.create_broadcast("test", announce().with_hops(hops)).unwrap();
		let mut dynamic = source.dynamic();
		settle().await;
		settle().await;
		let broadcast = consumer.request_broadcast("test").await.unwrap();

		let subscribing = broadcast.track("video").unwrap().subscribe(None);
		let request = dynamic.requested_track().await.unwrap();
		request.reject(Error::NotFound);
		settle().await;

		// One refusal is the verdict; the source is never re-asked.
		assert!(matches!(subscribing.await, Err(Error::NotFound)));
		dynamic.assert_no_request();
	}

	/// A rejection is only authoritative while its source is still the
	/// dispatched route. Here a better route takes over while the original
	/// source's info request is pending, and the stale source rejects at the
	/// same moment: the handover must win, not the stale rejection.
	#[tokio::test]
	async fn test_stale_rejection_does_not_abort_a_handover() {
		tokio::time::pause();

		let origin = Origin::random().produce();
		let consumer = origin.consume();

		let publisher = Origin::new(1).unwrap();
		let peer = Origin::new(5).unwrap();
		let via_peer = OriginList::try_from(vec![publisher, peer]).unwrap();
		let local = OriginList::try_from(vec![publisher]).unwrap();

		// The only route: dispatch parks on its pending info request.
		let source_remote = origin
			.create_broadcast("test", announce().with_hops(via_peer).with_cost(2))
			.unwrap();
		let mut dynamic_remote = source_remote.dynamic();
		settle().await;
		settle().await;
		let broadcast = consumer.request_broadcast("test").await.unwrap();
		let subscribing = broadcast.track("video").unwrap().subscribe(None);
		let request_remote = dynamic_remote.requested_track().await.unwrap();

		// A better route attaches (taking dispatch) while the old source's
		// request is still pending; the old source rejects in the same window.
		let source_local = origin.create_broadcast("test", announce().with_hops(local)).unwrap();
		let mut dynamic_local = source_local.dynamic();
		request_remote.reject(Error::NotFound);
		settle().await;

		// The subscription rides the handover onto the new route.
		let mut producer_local = accept_track(&mut dynamic_local, "video").await;
		settle().await;
		let mut sub = subscribing
			.await
			.expect("the handover must win over the stale rejection");
		producer_local.append_group().unwrap();
		assert_eq!(sub.assert_group().sequence, 0);
		sub.assert_not_closed();
	}

	/// A better source attaching mid-subscription takes the track over at an
	/// explicit group boundary: the old copy's demand is capped, the new copy
	/// starts at the boundary, and the subscriber reads a seamless sequence.
	#[tokio::test]
	async fn test_route_handover() {
		tokio::time::pause();

		let origin = Origin::random().produce();
		let consumer = origin.consume();
		let mut announced = consumer.announced();

		// Shared first hop, so the short route joins as an interchangeable source.
		let hops_long = OriginList::try_from(vec![Origin::new(1).unwrap(), Origin::new(3).unwrap()]).unwrap();
		let hops_short = OriginList::try_from(vec![Origin::new(1).unwrap()]).unwrap();

		let source_a = origin
			.create_broadcast("test", announce().with_hops(hops_long))
			.unwrap();
		let mut dynamic_a = source_a.dynamic();
		settle().await;
		settle().await;
		let broadcast = consumer.request_broadcast("test").await.unwrap();
		announced.assert_next_some("test");

		let subscribing = broadcast.track("video").unwrap().subscribe(None);
		let mut producer_a = accept_track(&mut dynamic_a, "video").await;
		settle().await;
		let mut sub = subscribing.await.unwrap();
		producer_a.append_group().unwrap();
		producer_a.append_group().unwrap();
		assert_eq!(sub.assert_group().sequence, 0);
		assert_eq!(sub.assert_group().sequence, 1);

		// A strictly shorter source attaches: the live track is handed over with
		// no announce churn.
		let source_b = origin
			.create_broadcast("test", announce().with_hops(hops_short))
			.unwrap();
		let mut dynamic_b = source_b.dynamic();
		settle().await;
		settle().await;
		announced.assert_next_wait();

		let mut producer_b = accept_track(&mut dynamic_b, "video").await;
		settle().await;

		// The old copy's demand is capped at the boundary; the new copy's starts
		// there. Both propagate as the subscriber polls.
		sub.assert_no_group();
		assert_eq!(producer_a.subscription().unwrap().group_end, Some(1));
		assert_eq!(producer_b.subscription().unwrap().group_start, Some(2));

		// The old copy racing past its cap is filtered; the new copy serves on.
		producer_a.create_group(group::Info { sequence: 2 }).unwrap();
		producer_b.create_group(group::Info { sequence: 2 }).unwrap();
		producer_b.create_group(group::Info { sequence: 3 }).unwrap();
		assert_eq!(sub.assert_group().sequence, 2);
		assert_eq!(sub.assert_group().sequence, 3);
		sub.assert_no_group();
		sub.assert_not_closed();
	}

	/// A graceful detach (deliberate unannounce) closes immediately: no linger, so
	/// the unannounce propagates promptly and a re-create is a fresh broadcast.
	#[tokio::test(start_paused = true)]
	async fn test_route_unannounce_immediate() {
		let origin = Origin::random().produce();
		let consumer = origin.consume();
		let mut announced = consumer.announced();

		let hops = OriginList::try_from(vec![Origin::new(1).unwrap()]).unwrap();
		let mut source = origin
			.create_broadcast("test", announce().with_hops(hops.clone()))
			.unwrap();
		settle().await;
		let broadcast = consumer.request_broadcast("test").await.unwrap();
		announced.assert_next_some("test");

		// The peer deliberately unannounced: no reconnect window, the broadcast is
		// gone as soon as the teardown task observes the close.
		source.finish();
		settle().await;
		announced.assert_next_none("test");

		// A re-create at the same path is a brand-new broadcast.
		let _source = origin.create_broadcast("test", announce().with_hops(hops)).unwrap();
		settle().await;
		let fresh = consumer.request_broadcast("test").await.unwrap();
		announced.assert_next_some("test");
		assert!(
			!fresh.is_clone(&broadcast),
			"re-create must not splice the old broadcast"
		);
	}

	/// With the default zero linger, a dying source (a session drop, not a
	/// deliberate unannounce) closes the broadcast just as promptly as a graceful
	/// one: no reconnect window, the tracks abort, and a re-create is a fresh
	/// broadcast rather than a splice.
	#[tokio::test(start_paused = true)]
	async fn test_route_detach_immediate() {
		let origin = Origin::random().produce();
		let consumer = origin.consume();
		let mut announced = consumer.announced();

		let hops = OriginList::try_from(vec![Origin::new(1).unwrap()]).unwrap();
		let source = origin
			.create_broadcast("test", announce().with_hops(hops.clone()))
			.unwrap();
		let mut dynamic = source.dynamic();
		settle().await;
		settle().await;
		let broadcast = consumer.request_broadcast("test").await.unwrap();
		announced.assert_next_some("test");

		let subscribing = broadcast.track("video").unwrap().subscribe(None);
		let producer = accept_track(&mut dynamic, "video").await;
		settle().await;
		let mut sub = subscribing.await.unwrap();

		// The session dies without unannouncing.
		drop(producer);
		source.abort(Error::Dropped).unwrap();
		drop(dynamic);

		settle().await;
		announced.assert_next_none("test");
		sub.assert_error();

		// A reconnecting session gets a brand-new broadcast, not a splice into
		// the old one.
		let _source = origin.create_broadcast("test", announce().with_hops(hops)).unwrap();
		settle().await;
		settle().await;
		let fresh = consumer.request_broadcast("test").await.unwrap();
		announced.assert_next_some("test");
		assert!(
			!fresh.is_clone(&broadcast),
			"re-create must not splice the old broadcast"
		);
	}

	/// A track nobody reads keeps the source's copy for [`TRACK_IDLE_LINGER`], then
	/// releases it. Crucially, the release must not immediately re-splice: the same
	/// demand signal gates both directions, so an idle track settles instead of
	/// re-requesting the track (and its info) every linger.
	#[tokio::test(start_paused = true)]
	async fn test_idle_track_releases_without_respinning() {
		let origin = Info::new(Origin::random()).produce();
		let consumer = origin.consume();

		let hops = OriginList::try_from(vec![Origin::new(1).unwrap()]).unwrap();
		let source = origin.create_broadcast("test", announce().with_hops(hops)).unwrap();
		let mut dynamic = source.dynamic();
		settle().await;
		let broadcast = consumer.request_broadcast("test").await.unwrap();

		let subscribing = broadcast.track("video").unwrap().subscribe(None);
		let producer = accept_track(&mut dynamic, "video").await;
		settle().await;
		let sub = subscribing.await.unwrap();

		// The reader leaves, but the copy stays warm inside the window so a viewer
		// coming back (or a follow-up fetch) reuses it.
		drop(sub);
		tokio::time::sleep(TRACK_IDLE_LINGER / 2).await;
		settle().await;
		assert!(
			producer.poll_unused(&kio::Waiter::noop()).is_pending(),
			"the copy must stay spliced inside the linger",
		);

		// Past the window the segment is released, so the serving session sees its
		// copy go unused and can drop it (along with the track info).
		tokio::time::sleep(TRACK_IDLE_LINGER).await;
		settle().await;
		assert!(
			producer.poll_unused(&kio::Waiter::noop()).is_ready(),
			"an idle copy must be released after the linger",
		);

		// The anti-spin property: the release must not re-arm the splice. Ungated,
		// the loop re-attaches the copy immediately and drops it again every linger,
		// re-requesting the track (and its info) from the session each time it dies.
		for _ in 0..3 {
			tokio::time::sleep(TRACK_IDLE_LINGER).await;
			settle().await;
			assert!(
				producer.poll_unused(&kio::Waiter::noop()).is_ready(),
				"an unread copy must stay released, not be re-spliced",
			);
		}
		assert!(
			dynamic.requested_track().now_or_never().is_none(),
			"an unread track must not be re-requested",
		);
		drop(producer);

		// A returning reader re-splices: the origin asks the source for a fresh copy.
		let subscribing = broadcast.track("video").unwrap().subscribe(None);
		let mut producer = accept_track(&mut dynamic, "video").await;
		settle().await;
		let mut sub = subscribing.await.unwrap();
		producer.append_group().unwrap();
		assert_eq!(sub.assert_group().sequence, 0);
	}

	/// Back-to-back fetches reuse the source's copy: only the first asks the source
	/// for the track, so a fetch-driven consumer (HLS pulling segment after segment)
	/// doesn't re-request the track, and its `TRACK_INFO`, for every group.
	#[tokio::test(start_paused = true)]
	async fn test_back_to_back_fetches_reuse_the_track() {
		let origin = Info::new(Origin::random()).produce();
		let consumer = origin.consume();

		let hops = OriginList::try_from(vec![Origin::new(1).unwrap()]).unwrap();
		let source = origin.create_broadcast("test", announce().with_hops(hops)).unwrap();
		let mut dynamic = source.dynamic();
		settle().await;
		let broadcast = consumer.request_broadcast("test").await.unwrap();

		// The first fetch has to ask the source for the track.
		let fetching = broadcast.track("video").unwrap().fetch_group(0, None);
		let mut producer = accept_track(&mut dynamic, "video").await;
		producer.append_group().unwrap().finish().unwrap();
		settle().await;
		let first = fetching.await.expect("first fetch");
		drop(first);

		// A second fetch inside the linger reuses the copy already spliced in.
		settle().await;
		let fetching = broadcast.track("video").unwrap().fetch_group(0, None);
		settle().await;
		assert!(
			dynamic.requested_track().now_or_never().is_none(),
			"a fetch inside the linger must reuse the track, not re-request it",
		);
		drop(fetching.await.expect("second fetch"));

		// Once the fetches stop, the copy is released like any other idle track.
		tokio::time::sleep(TRACK_IDLE_LINGER * 2).await;
		settle().await;
		assert!(
			producer.poll_unused(&kio::Waiter::noop()).is_ready(),
			"the copy must be released once the fetches stop",
		);
		drop(producer);

		// And a later fetch re-requests it: `accept_track` times out if it doesn't.
		settle().await;
		let fetching = broadcast.track("video").unwrap().fetch_group(0, None);
		let mut producer = accept_track(&mut dynamic, "video").await;
		producer.append_group().unwrap().finish().unwrap();
		settle().await;
		fetching.await.expect("fetch after the linger");
	}

	/// With a linger configured, an ungraceful source loss keeps the broadcast
	/// alive and announced; a source re-attaching within the window splices in and
	/// consumers resume at the group boundary, never observing the outage.
	#[tokio::test(start_paused = true)]
	async fn test_linger_reconnect_splices() {
		let origin = Info::new(Origin::random())
			.with_linger(Duration::from_secs(5))
			.produce();
		let consumer = origin.consume();
		let mut announced = consumer.announced();

		let hops = OriginList::try_from(vec![Origin::new(1).unwrap()]).unwrap();
		let source = origin
			.create_broadcast("test", announce().with_hops(hops.clone()))
			.unwrap();
		let mut dynamic = source.dynamic();
		settle().await;
		settle().await;
		let broadcast = consumer.request_broadcast("test").await.unwrap();
		announced.assert_next_some("test");

		let subscribing = broadcast.track("video").unwrap().subscribe(None);
		let mut producer = accept_track(&mut dynamic, "video").await;
		settle().await;
		let mut sub = subscribing.await.unwrap();

		producer.append_group().unwrap();
		producer.append_group().unwrap();
		assert_eq!(sub.assert_group().sequence, 0);
		assert_eq!(sub.assert_group().sequence, 1);

		// The session dies without unannouncing: the broadcast enters the linger
		// window instead of closing.
		drop(producer);
		source.abort(Error::Dropped).unwrap();
		drop(dynamic);
		settle().await;

		// No unannounce, no track error: the outage is invisible so far.
		announced.assert_next_wait();
		sub.assert_no_group();
		sub.assert_not_closed();

		// A consumer arriving mid-outage still resolves the lingering broadcast.
		let during = consumer.request_broadcast("test").await.unwrap();
		assert!(during.is_clone(&broadcast), "the lingering broadcast still resolves");

		// The session reconnects within the window: the new source splices into
		// the same broadcast.
		let source = origin.create_broadcast("test", announce().with_hops(hops)).unwrap();
		let mut dynamic = source.dynamic();
		settle().await;
		settle().await;
		announced.assert_next_wait();
		let again = consumer.request_broadcast("test").await.unwrap();
		assert!(again.is_clone(&broadcast), "the reconnect must splice, not replace");

		// The pending track re-splices from the new source and resumes one past
		// the groups the old source already delivered. Demand registers as the
		// subscriber polls.
		let mut producer = accept_track(&mut dynamic, "video").await;
		settle().await;
		sub.assert_no_group();
		assert_eq!(producer.subscription().unwrap().group_start, Some(2));
		producer.create_group(group::Info { sequence: 2 }).unwrap();
		assert_eq!(sub.assert_group().sequence, 2);
		sub.assert_not_closed();
	}

	/// The linger window expiring without a replacement closes the broadcast: the
	/// path unannounces, tracks abort, and a later re-create is a fresh broadcast.
	#[tokio::test(start_paused = true)]
	async fn test_linger_expiry_closes() {
		let origin = Info::new(Origin::random())
			.with_linger(Duration::from_secs(5))
			.produce();
		let consumer = origin.consume();
		let mut announced = consumer.announced();

		let hops = OriginList::try_from(vec![Origin::new(1).unwrap()]).unwrap();
		let source = origin
			.create_broadcast("test", announce().with_hops(hops.clone()))
			.unwrap();
		let mut dynamic = source.dynamic();
		settle().await;
		settle().await;
		let broadcast = consumer.request_broadcast("test").await.unwrap();
		announced.assert_next_some("test");

		let subscribing = broadcast.track("video").unwrap().subscribe(None);
		let producer = accept_track(&mut dynamic, "video").await;
		settle().await;
		let mut sub = subscribing.await.unwrap();

		drop(producer);
		source.abort(Error::Dropped).unwrap();
		drop(dynamic);
		settle().await;
		announced.assert_next_wait();

		// Nobody comes back: the window expires and the broadcast closes.
		tokio::time::sleep(std::time::Duration::from_secs(6)).await;
		settle().await;
		announced.assert_next_none("test");
		sub.assert_error();

		// A session reconnecting after the window gets a brand-new broadcast.
		let _source = origin.create_broadcast("test", announce().with_hops(hops)).unwrap();
		settle().await;
		settle().await;
		let fresh = consumer.request_broadcast("test").await.unwrap();
		announced.assert_next_some("test");
		assert!(
			!fresh.is_clone(&broadcast),
			"a late re-create must not splice the expired broadcast"
		);
	}

	/// A linger too large to represent as a deadline never expires: the broadcast
	/// outlives an arbitrarily long outage (a reconnect loop with no give-up
	/// timeout promises to retry forever).
	#[tokio::test(start_paused = true)]
	async fn test_linger_forever() {
		let origin = Info::new(Origin::random()).with_linger(Duration::MAX).produce();
		let consumer = origin.consume();
		let mut announced = consumer.announced();

		let hops = OriginList::try_from(vec![Origin::new(1).unwrap()]).unwrap();
		let source = origin
			.create_broadcast("test", announce().with_hops(hops.clone()))
			.unwrap();
		settle().await;
		let broadcast = consumer.request_broadcast("test").await.unwrap();
		announced.assert_next_some("test");

		source.abort(Error::Dropped).unwrap();
		settle().await;

		// Days later the broadcast is still announced and still splices a reconnect.
		tokio::time::sleep(std::time::Duration::from_secs(60 * 60 * 24 * 3)).await;
		announced.assert_next_wait();
		let source = origin.create_broadcast("test", announce().with_hops(hops)).unwrap();
		settle().await;
		settle().await;
		let again = consumer.request_broadcast("test").await.unwrap();
		assert!(again.is_clone(&broadcast), "the reconnect must splice, not replace");
		drop(source);
	}

	/// A lingering broadcast with a live subscription parks, then resumes on the
	/// reconnect.
	///
	/// An ungraceful loss empties the route table while the front waits out its
	/// linger, so the route a track is spliced from is gone with no replacement to
	/// serve it. The task has to park for the whole window on the strength of
	/// dropping the departed route, because nothing else bounds it: a "route gone"
	/// edge that keeps firing yields a wait that is Ready on every poll, a full
	/// core per subscribed track, and the runtime that has to deliver the
	/// reconnect starved along with it.
	///
	/// Under a paused clock that spin never yields, so a regression hangs here
	/// rather than failing.
	#[tokio::test(start_paused = true)]
	async fn test_linger_parks_a_live_subscription() {
		let origin = Info::new(Origin::random())
			.with_linger(Duration::from_secs(5))
			.produce();
		let consumer = origin.consume();

		let hops = OriginList::try_from(vec![Origin::new(1).unwrap()]).unwrap();
		let source = origin
			.create_broadcast("test", announce().with_hops(hops.clone()))
			.unwrap();
		let mut dynamic = source.dynamic();
		settle().await;
		settle().await;
		let broadcast = consumer.request_broadcast("test").await.unwrap();

		// A viewer reading a track that is written once and then stays quiet, like a
		// catalog.
		let subscribing = broadcast.track("catalog.json").unwrap().subscribe(None);
		let mut producer = accept_track(&mut dynamic, "catalog.json").await;
		settle().await;
		let mut sub = subscribing.await.unwrap();
		producer.append_group().unwrap();
		assert_eq!(sub.assert_group().sequence, 0);

		// The publisher's session dies abruptly: the table empties and the front
		// lingers, holding the subscription open for a reconnect. The track copy
		// outlives the broadcast (closes don't cascade), so nothing reports the
		// spliced segment as ended: the departed route is the only edge left.
		source.abort(Error::Dropped).unwrap();
		settle().await;
		settle().await;
		sub.assert_not_closed();

		// Most of the window with no source at all: the subscription stays parked
		// rather than being cut, which is what the linger promises a reconnect.
		tokio::time::sleep(Duration::from_secs(4)).await;
		settle().await;
		sub.assert_not_closed();

		// The reconnect inside the window resumes the same subscription.
		let source = origin.create_broadcast("test", announce().with_hops(hops)).unwrap();
		let mut dynamic = source.dynamic();
		settle().await;
		let mut producer = accept_track(&mut dynamic, "catalog.json").await;
		settle().await;
		producer.create_group(group::Info { sequence: 1 }).unwrap();
		assert_eq!(sub.assert_group().sequence, 1);
		sub.assert_not_closed();
	}

	/// An idle segment is released even once the route that produced it is gone.
	///
	/// A departed route drops the loop's handle on it, so the idle countdown has to
	/// key off the segment itself. Otherwise the segment is stranded for the life of
	/// the front, pinning the dead source's cached groups.
	///
	/// Asserted through the boundary a stranded segment would leave behind, since
	/// `resume` is private and delivery is what a viewer actually feels: a released
	/// segment set splices the next source unbounded, so its first group arrives,
	/// while a retained one caps it below that edge.
	#[tokio::test(start_paused = true)]
	async fn test_idle_release_survives_the_route_leaving() {
		let origin = Info::new(Origin::random())
			.with_linger(Duration::from_secs(600))
			.produce();
		let consumer = origin.consume();

		let hops = OriginList::try_from(vec![Origin::new(1).unwrap()]).unwrap();
		let source = origin
			.create_broadcast("test", announce().with_hops(hops.clone()))
			.unwrap();
		let mut dynamic = source.dynamic();
		settle().await;
		settle().await;
		let broadcast = consumer.request_broadcast("test").await.unwrap();

		let subscribing = broadcast.track("catalog.json").unwrap().subscribe(None);
		let mut producer = accept_track(&mut dynamic, "catalog.json").await;
		settle().await;
		let mut sub = subscribing.await.unwrap();
		producer.append_group().unwrap();
		assert_eq!(sub.assert_group().sequence, 0);

		// The publisher's session dies, then the viewer gives up: nothing holds the
		// segment, and nothing is left to serve the track.
		source.abort(Error::Dropped).unwrap();
		settle().await;
		settle().await;
		drop(sub);
		drop(producer);
		drop(dynamic);
		settle().await;
		tokio::time::sleep(TRACK_IDLE_LINGER + Duration::from_secs(1)).await;
		settle().await;

		// The publisher reconnects under the same identity, so it joins the front as
		// a route rather than replacing it, and restarts its group numbering.
		let source = origin.create_broadcast("test", announce().with_hops(hops)).unwrap();
		let mut dynamic = source.dynamic();
		settle().await;
		settle().await;
		let broadcast = consumer.request_broadcast("test").await.unwrap();
		let subscribing = broadcast.track("catalog.json").unwrap().subscribe(None);
		let mut producer = accept_track(&mut dynamic, "catalog.json").await;
		settle().await;
		let mut sub = subscribing.await.unwrap();
		producer.append_group().unwrap();
		assert_eq!(
			sub.assert_group().sequence,
			0,
			"the reconnect's first group must not be filtered by a stale boundary"
		);
	}

	/// A deliberate finish never lingers, even with a linger configured: a clean
	/// unannounce from a peer must propagate immediately.
	#[tokio::test(start_paused = true)]
	async fn test_linger_skipped_on_finish() {
		let origin = Info::new(Origin::random())
			.with_linger(Duration::from_secs(5))
			.produce();
		let consumer = origin.consume();
		let mut announced = consumer.announced();

		let hops = OriginList::try_from(vec![Origin::new(1).unwrap()]).unwrap();
		let mut source = origin
			.create_broadcast("test", announce().with_hops(hops.clone()))
			.unwrap();
		settle().await;
		let broadcast = consumer.request_broadcast("test").await.unwrap();
		announced.assert_next_some("test");

		// A clean unannounce: the broadcast is gone as soon as the teardown task
		// observes the close, with no reconnect window.
		source.finish();
		settle().await;
		announced.assert_next_none("test");

		// A re-create at the same path is a brand-new broadcast.
		let _source = origin.create_broadcast("test", announce().with_hops(hops)).unwrap();
		settle().await;
		let fresh = consumer.request_broadcast("test").await.unwrap();
		announced.assert_next_some("test");
		assert!(
			!fresh.is_clone(&broadcast),
			"a finish must not leave a lingering broadcast to splice into"
		);
	}

	/// A non-live broadcast is reachable by exact path but never announced;
	/// toggling `live` announces and unannounces without touching the broadcast.
	#[tokio::test]
	async fn test_announce_toggle() {
		tokio::time::pause();

		let origin = Origin::random().produce();
		let consumer = origin.consume();
		let mut announced = consumer.announced();

		let mut source = origin.create_broadcast("test", broadcast::Route::new()).unwrap();
		settle().await;

		// Routable but not announced.
		announced.assert_next_wait();
		let broadcast = consumer
			.get_broadcast("test")
			.expect("offline broadcast is still routable");
		assert!(!broadcast.route().announce);

		// request_broadcast resolves the offline broadcast too.
		let requested = consumer.request_broadcast("test").await.unwrap();
		assert!(requested.is_clone(&broadcast));

		// Going live announces.
		source.set_route(announce()).unwrap();
		settle().await;
		let face = announced.assert_next_some("test");
		assert!(face.is_clone(&broadcast));

		// A fresh consumer replays only announced broadcasts.
		let mut fresh = origin.consume().announced();
		fresh.assert_next_some("test");
		fresh.assert_next_wait();

		// Going offline unannounces but stays routable.
		source.set_route(broadcast::Route::new()).unwrap();
		settle().await;
		announced.assert_next_none("test");
		assert!(consumer.get_broadcast("test").is_some());
		let mut fresh = origin.consume().announced();
		fresh.assert_next_wait();

		source.finish();
		settle().await;
		assert!(consumer.get_broadcast("test").is_none());
	}

	/// An announced source outranks a cheaper offline one, so the broadcast
	/// stays announced and serves from it.
	#[tokio::test]
	async fn test_announce_beats_offline() {
		tokio::time::pause();

		let origin = Origin::random().produce();
		let consumer = origin.consume();
		let mut announced = consumer.announced();

		// An unannounced source with the best cost.
		let _offline = origin.create_broadcast("test", broadcast::Route::new()).unwrap();
		settle().await;
		announced.assert_next_wait();

		// An announced source with a worse cost still wins: the path announces
		// and advertises its route.
		let mut announced_source = origin.create_broadcast("test", announce().with_cost(10)).unwrap();
		settle().await;
		announced.assert_next_some("test");
		let face = consumer.get_broadcast("test").unwrap();
		assert!(face.route().announce);
		assert_eq!(face.route().cost, 10);

		// The announced source leaving falls back to the offline one: the path
		// unannounces but stays routable.
		announced_source.finish();
		settle().await;
		announced.assert_next_none("test");
		assert!(consumer.get_broadcast("test").is_some());
	}

	/// A better source attaching does not churn announces: the broadcast identity
	/// is origin-owned, so the swap is invisible to consumers.
	#[tokio::test]
	async fn test_better_source_no_churn() {
		tokio::time::pause();

		let origin = Origin::random().produce();
		let mut announced = origin.consume().announced();

		// `a` carries two hops; `b` reaches the same publisher in one, so `b`
		// wins dispatch when it joins.
		let hops_a = OriginList::try_from(vec![Origin::new(1).unwrap(), Origin::new(3).unwrap()]).unwrap();
		let hops_b = OriginList::try_from(vec![Origin::new(1).unwrap()]).unwrap();
		let _a = origin.create_broadcast("test", announce().with_hops(hops_a)).unwrap();
		settle().await;
		let face = announced.assert_next_some("test");

		let _b = origin
			.create_broadcast("test", announce().with_hops(hops_b.clone()))
			.unwrap();
		settle().await;
		announced.assert_next_wait();
		let current = origin.consume().get_broadcast("test").unwrap();
		assert!(current.is_clone(&face), "the broadcast identity must not change");
		// The face now advertises the winning (shorter) route.
		assert_eq!(current.route().hops, hops_b);
	}

	/// A second source with a different original publisher (first hop) is new
	/// content, not a standby: it must not splice into the incumbent's
	/// subscribers. It takes the path over immediately, as a real unannounce +
	/// announce, rather than waiting out an incumbent whose session may only be
	/// alive because the transport has not timed it out yet.
	#[tokio::test]
	async fn test_publisher_mismatch_replaces() {
		tokio::time::pause();

		let origin = Origin::random().produce();
		let consumer = origin.consume();
		let mut announced = consumer.announced();

		let hops_a = OriginList::try_from(vec![Origin::new(1).unwrap()]).unwrap();
		let hops_b = OriginList::try_from(vec![Origin::new(2).unwrap()]).unwrap();

		let mut source_a = origin
			.create_broadcast("test", announce().with_hops(hops_a.clone()))
			.unwrap();
		settle().await;
		let face_a = announced.assert_next_some("test");

		// A different publisher at the same path: the newcomer wins it now, while
		// the incumbent is still attached and still believes it is publishing.
		let _source_b = origin
			.create_broadcast("test", announce().with_hops(hops_b.clone()))
			.unwrap();
		settle().await;
		settle().await;
		announced.assert_next_none("test");
		let face_b = announced.assert_next_some("test");
		assert!(!face_b.is_clone(&face_a), "a replacement, never a splice");
		assert_eq!(consumer.get_broadcast("test").unwrap().route().hops, hops_b);
		// The displaced front is torn down, not merely unpublished: leaving it
		// running would strand its subscribers and its source watchers on a face
		// nothing can reach.
		assert!(face_a.is_closed(), "the displaced front must close");

		// The displaced incumbent ending is invisible: it no longer owns the path.
		source_a.finish();
		settle().await;
		settle().await;
		announced.assert_next_wait();
		assert_eq!(consumer.get_broadcast("test").unwrap().route().hops, hops_b);
	}

	/// A publisher reconnecting under the same identity attaches as a second
	/// route with an identical hop chain and cost. The new session is the one
	/// actually carrying frames, so it must win selection immediately rather than
	/// waiting for the transport to retire the old one.
	#[tokio::test]
	async fn test_reconnect_wins_over_stale_route() {
		tokio::time::pause();

		let origin = Origin::random().produce();
		let consumer = origin.consume();

		let publisher = Origin::new(1).unwrap();
		let hops = OriginList::try_from(vec![publisher]).unwrap();

		// The original session, still attached: its QUIC connection has not been
		// declared dead yet.
		let stale = origin
			.create_broadcast("test", announce().with_hops(hops.clone()))
			.unwrap();
		let mut stale_dynamic = stale.dynamic();
		settle().await;

		// The same publisher reconnecting over a fresh session.
		let fresh = origin
			.create_broadcast("test", announce().with_hops(hops.clone()))
			.unwrap();
		let mut fresh_dynamic = fresh.dynamic();
		settle().await;
		settle().await;

		// Track requests dispatch to the reconnect, not the corpse.
		let broadcast = consumer.request_broadcast("test").await.unwrap();
		let subscribing = broadcast.track("video").unwrap().subscribe(None);
		settle().await;
		let _producer = accept_track(&mut fresh_dynamic, "video").await;
		settle().await;
		subscribing.await.unwrap();
		stale_dynamic.assert_no_request();
	}

	/// The same reconnect, but arriving while a subscription is already spliced
	/// onto the stale route: the live track must re-splice onto the new session
	/// rather than ride the dead one until the transport gives up. The gate
	/// [`FrontState::reselect`] applies while carrying is pinned separately, in
	/// [`test_carrying_switches_to_benign_routes`].
	#[tokio::test]
	async fn test_carrying_reconnect_switches_immediately() {
		tokio::time::pause();

		let origin = Origin::random().produce();
		let consumer = origin.consume();

		let hops = OriginList::try_from(vec![Origin::new(1).unwrap()]).unwrap();

		let stale = origin
			.create_broadcast("test", announce().with_hops(hops.clone()))
			.unwrap();
		let mut stale_dynamic = stale.dynamic();
		settle().await;

		// A live subscription riding the original session.
		let broadcast = consumer.request_broadcast("test").await.unwrap();
		let subscribing = broadcast.track("video").unwrap().subscribe(None);
		settle().await;
		let _stale_producer = accept_track(&mut stale_dynamic, "video").await;
		settle().await;
		// Held: the splice only follows the active route while the track is read.
		let _subscription = subscribing.await.unwrap();

		// The reconnect arrives with the front already carrying.
		let fresh = origin
			.create_broadcast("test", announce().with_hops(hops.clone()))
			.unwrap();
		let mut fresh_dynamic = fresh.dynamic();
		settle().await;
		settle().await;

		// The carrying front re-splices onto the reconnect rather than waiting for
		// the stale session to die.
		let _fresh_producer = accept_track(&mut fresh_dynamic, "video").await;
	}

	/// Taking the path over is scoped to a newcomer that would actually outrank
	/// the incumbent. An offline source (a cache, or an on-demand handler) is
	/// ranked below every announced route, so arriving under a different
	/// publisher must not unannounce a live broadcast and cut its subscribers.
	///
	/// The tail of this test covers the park's *exit*: the parked source has to
	/// wake and attach once the incumbent ends. Nothing else drives that wait, so
	/// without this a lost wakeup would strand the source invisibly forever
	/// rather than failing anything.
	#[tokio::test]
	async fn test_offline_mismatch_never_evicts_a_live_front() {
		tokio::time::pause();

		let origin = Origin::random().produce();
		let consumer = origin.consume();
		let mut announced = consumer.announced();

		let hops_live = OriginList::try_from(vec![Origin::new(1).unwrap()]).unwrap();
		let hops_cache = OriginList::try_from(vec![Origin::new(2).unwrap()]).unwrap();

		let mut live = origin
			.create_broadcast("test", announce().with_hops(hops_live.clone()))
			.unwrap();
		let mut live_dynamic = live.dynamic();
		settle().await;
		let face = announced.assert_next_some("test");

		// A subscriber riding the live broadcast, which the eviction would cut.
		let broadcast = consumer.request_broadcast("test").await.unwrap();
		let subscribing = broadcast.track("video").unwrap().subscribe(None);
		settle().await;
		let _producer = accept_track(&mut live_dynamic, "video").await;
		settle().await;
		subscribing.await.unwrap();

		// An offline source for different content at the same path: it stays
		// invisible rather than displacing the live one.
		let cache = origin
			.create_broadcast("test", broadcast::Route::new().with_hops(hops_cache.clone()))
			.unwrap();
		settle().await;
		settle().await;
		announced.assert_next_wait();
		assert!(!face.is_closed(), "the live front must survive");
		assert_eq!(consumer.get_broadcast("test").unwrap().route().hops, hops_live);

		// The incumbent ending hands the path to the parked source, which is the
		// only thing that ever wakes that wait.
		live.finish();
		settle().await;
		settle().await;
		announced.assert_next_none("test");
		let taken = consumer
			.get_broadcast("test")
			.expect("the parked source must take over");
		assert_eq!(taken.route().hops, hops_cache);
		// Offline, so it holds the path without being advertised.
		announced.assert_next_wait();
		drop(cache);
	}

	/// A subscription from a peer the active chain flows through is served from
	/// the best clean source directly (data-plane split horizon), while every
	/// other consumer keeps the shared spliced broadcast fed by the active
	/// source. The two selections match what the announce loop advertises, so
	/// the data plane keeps the control plane's promise.
	#[tokio::test]
	async fn test_dispatch_excludes_requester() {
		tokio::time::pause();

		let origin = Origin::random().produce();
		let consumer = origin.consume();

		let peer = Origin::new(5).unwrap();
		let publisher = Origin::new(1).unwrap();
		// The route through the peer is cheaper, so it is the active source.
		let tainted = OriginList::try_from(vec![publisher, peer]).unwrap();
		let clean = OriginList::try_from(vec![publisher]).unwrap();

		let source_a = origin.create_broadcast("test", announce().with_hops(tainted)).unwrap();
		let mut dynamic_a = source_a.dynamic();
		settle().await;
		let source_b = origin
			.create_broadcast("test", announce().with_hops(clean).with_cost(5))
			.unwrap();
		let mut dynamic_b = source_b.dynamic();
		settle().await;
		settle().await;

		// An ordinary consumer rides the shared front, dispatched to the active
		// (peer-tainted) source.
		let shared = consumer.request_broadcast("test").await.unwrap();
		let subscribing = shared.track("video").unwrap().subscribe(None);
		let _producer_a = accept_track(&mut dynamic_a, "video").await;
		settle().await;
		subscribing.await.unwrap();

		// The peer is pinned to the clean source. Crucially, nothing reaches the
		// via-peer source: a track request on it is what a session would forward
		// upstream as a SUBSCRIBE, and forwarding this one would send the peer's
		// own subscription back to them.
		let scoped = consumer.clone().excluding(peer);
		let pinned = scoped.request_broadcast("test").await.unwrap();
		let subscribing = pinned.track("video").unwrap().subscribe(None);
		let _producer_b = accept_track(&mut dynamic_b, "video").await;
		settle().await;
		subscribing.await.unwrap();
		dynamic_a.assert_no_request();
	}

	/// Two publishers that never declared an identity both arrive with a first
	/// hop of UNKNOWN. They are unrelated content, so the second MUST replace the
	/// first rather than joining it as an interchangeable standby: splicing them
	/// would cut one publisher's subscribers over to the other's stream.
	#[tokio::test]
	async fn test_unknown_publishers_do_not_splice() {
		tokio::time::pause();

		let origin = Origin::random().produce();
		let consumer = origin.consume();
		let mut announced = consumer.announced();

		let unknown_a = OriginList::try_from(vec![Origin::UNKNOWN]).unwrap();
		let unknown_b = OriginList::try_from(vec![Origin::UNKNOWN]).unwrap();

		let source_a = origin
			.create_broadcast("test", announce().with_hops(unknown_a))
			.unwrap();
		settle().await;
		settle().await;
		announced.assert_next_some("test");

		// Same first hop by value, but it identifies nothing, so this is a
		// replacement: the path is unannounced and re-announced rather than
		// silently gaining a standby.
		let source_b = origin
			.create_broadcast("test", announce().with_hops(unknown_b))
			.unwrap();
		settle().await;
		settle().await;
		announced.assert_next_none("test");
		announced.assert_next_some("test");

		drop(source_a);
		drop(source_b);
	}

	/// The same two sources under a real shared publisher id do splice, which is
	/// what keeps the rule above about UNKNOWN rather than about hop chains.
	#[tokio::test]
	async fn test_known_publishers_still_splice() {
		tokio::time::pause();

		let origin = Origin::random().produce();
		let consumer = origin.consume();
		let mut announced = consumer.announced();

		let publisher = Origin::new(1).unwrap();
		let hops_a = OriginList::try_from(vec![publisher]).unwrap();
		let hops_b = OriginList::try_from(vec![publisher, Origin::new(3).unwrap()]).unwrap();

		let source_a = origin.create_broadcast("test", announce().with_hops(hops_a)).unwrap();
		settle().await;
		settle().await;
		announced.assert_next_some("test");

		// Shares the publisher, so it joins silently as a standby: no churn.
		let source_b = origin.create_broadcast("test", announce().with_hops(hops_b)).unwrap();
		settle().await;
		settle().await;
		announced.assert_next_wait();

		drop(source_a);
		drop(source_b);
	}

	/// A local standby with the same original publisher joining a front that is
	/// carrying the broadcast from a peer must splice the live subscription onto
	/// the new source, never tear it down (#2473, e2e finding 2). Redundant
	/// publishers sharing an origin id MUST produce the same tracks, so the
	/// splice resumes seamlessly at the group boundary.
	#[tokio::test]
	async fn test_standby_join_splices_live_subscriber() {
		tokio::time::pause();

		let origin = Origin::random().produce();
		let consumer = origin.consume();

		let publisher = Origin::new(1).unwrap();
		let peer = Origin::new(5).unwrap();
		let via_peer = OriginList::try_from(vec![publisher, peer]).unwrap();
		let local = OriginList::try_from(vec![publisher]).unwrap();

		// Carrying via the peer, with a live subscriber mid-stream.
		let source_remote = origin
			.create_broadcast("test", announce().with_hops(via_peer).with_cost(2))
			.unwrap();
		let mut dynamic_remote = source_remote.dynamic();
		settle().await;
		settle().await;
		let broadcast = consumer.request_broadcast("test").await.unwrap();
		let subscribing = broadcast.track("video").unwrap().subscribe(None);
		let mut producer_remote = accept_track(&mut dynamic_remote, "video").await;
		settle().await;
		let mut sub = subscribing.await.unwrap();
		producer_remote.append_group().unwrap();
		assert_eq!(sub.assert_group().sequence, 0);

		// The local standby joins with the same first hop and a cheaper route:
		// it wins dispatch and the live track re-splices at the boundary.
		let source_local = origin.create_broadcast("test", announce().with_hops(local)).unwrap();
		let mut dynamic_local = source_local.dynamic();
		settle().await;
		let mut producer_local = accept_track(&mut dynamic_local, "video").await;
		settle().await;
		sub.assert_no_group();
		assert_eq!(producer_local.subscription().unwrap().group_start, Some(1));
		producer_local.create_group(group::Info { sequence: 1 }).unwrap();
		assert_eq!(sub.assert_group().sequence, 1);
		sub.assert_not_closed();
	}

	/// A standby that wins dispatch without carrying the track ends the logical
	/// track immediately, even over an incumbent that was serving it: which
	/// tracks a broadcast carries is the publisher's contract (announce after
	/// the tracks exist), so the newly-dispatched source's refusal is the
	/// track's verdict rather than the start of a retry sweep.
	#[tokio::test]
	async fn test_standby_missing_track_aborts() {
		tokio::time::pause();

		let origin = Origin::random().produce();
		let consumer = origin.consume();

		let publisher = Origin::new(1).unwrap();
		let peer = Origin::new(5).unwrap();
		let via_peer = OriginList::try_from(vec![publisher, peer]).unwrap();
		let local = OriginList::try_from(vec![publisher]).unwrap();

		// Carrying via the peer, with a live subscriber mid-stream.
		let source_remote = origin
			.create_broadcast("test", announce().with_hops(via_peer).with_cost(2))
			.unwrap();
		let mut dynamic_remote = source_remote.dynamic();
		settle().await;
		settle().await;
		let broadcast = consumer.request_broadcast("test").await.unwrap();
		let subscribing = broadcast.track("audio").unwrap().subscribe(None);
		let mut producer_remote = accept_track(&mut dynamic_remote, "audio").await;
		settle().await;
		let mut sub = subscribing.await.unwrap();
		producer_remote.append_group().unwrap();
		assert_eq!(sub.assert_group().sequence, 0);

		// The standby joins and wins dispatch, but has not created "audio" yet:
		// its refusal ends the logical track, incumbent notwithstanding.
		let source_local = origin.create_broadcast("test", announce().with_hops(local)).unwrap();
		let mut dynamic_local = source_local.dynamic();
		settle().await;
		let request = dynamic_local.requested_track().await.unwrap();
		assert_eq!(request.name(), "audio");
		request.reject(Error::NotFound);
		settle().await;
		sub.assert_closed();
	}

	/// A refused track aborts, but the verdict is not cached: it belongs to the
	/// request that received it, so a later request re-asks the source. Otherwise
	/// one early request for a track the publisher had not created yet leaves the
	/// name dead for the life of the front.
	#[tokio::test]
	async fn test_unservable_track_retried_by_a_later_request() {
		tokio::time::pause();

		let origin = Origin::random().produce();
		let consumer = origin.consume();

		let source = origin.create_broadcast("test", announce()).unwrap();
		let mut dynamic = source.dynamic();
		settle().await;
		settle().await;
		let broadcast = consumer.request_broadcast("test").await.unwrap();

		// Nothing serves it: the refusal aborts the track with the source's error.
		let subscribing = broadcast.track("audio").unwrap().subscribe(None);
		let request = dynamic.requested_track().await.unwrap();
		request.reject(Error::NotFound);
		settle().await;
		assert!(matches!(subscribing.await, Err(Error::NotFound)));

		// The publisher has the track now; a fresh request must reach it.
		let retry = broadcast.track("audio").unwrap().subscribe(None);
		let mut producer = accept_track(&mut dynamic, "audio").await;
		settle().await;
		let mut sub = retry.await.expect("a fresh request must reach the source");
		producer.append_group().unwrap();
		assert_eq!(sub.assert_group().sequence, 0);
	}

	/// The front picks a source per track and re-picks on failover, so a route
	/// tainted for a peer is one the front may serve them from even when the
	/// active route is clean. Sharing the front therefore has to check the whole
	/// table, not just the active route: here the clean route is active but cannot
	/// carry the track, and the fallback is the peer's own route.
	#[tokio::test]
	async fn test_per_track_fallback_respects_exclusion() {
		tokio::time::pause();

		let origin = Origin::random().produce();
		let consumer = origin.consume();

		let publisher = Origin::new(1).unwrap();
		let peer = Origin::new(5).unwrap();
		let via_peer = OriginList::try_from(vec![publisher, peer]).unwrap();
		let local = OriginList::try_from(vec![publisher]).unwrap();

		// The route through the peer has the track.
		let source_tainted = origin
			.create_broadcast("test", announce().with_hops(via_peer).with_cost(2))
			.unwrap();
		let mut dynamic_tainted = source_tainted.dynamic();
		settle().await;
		settle().await;

		// The clean route is cheaper, so it is active, but its publisher has not
		// created the track yet.
		let source_clean = origin.create_broadcast("test", announce().with_hops(local)).unwrap();
		let mut dynamic_clean = source_clean.dynamic();
		settle().await;

		let scoped = consumer.clone().excluding(peer);
		let broadcast = scoped.request_broadcast("test").await.unwrap();
		let _subscribing = broadcast.track("video").unwrap().subscribe(None);
		settle().await;

		// The peer is pinned to the clean source, so the fallback the front would
		// take for everyone else is not reachable from their subscription: the
		// refusal aborts the track rather than asking the tainted route.
		let request = dynamic_clean.requested_track().await.unwrap();
		request.reject(Error::NotFound);
		settle().await;
		dynamic_tainted.assert_no_request();
	}

	/// The same guarantee under failover: the peer resolves while a clean route is
	/// active, then that route dies. Its subscription must not migrate onto the
	/// route that flows back through it.
	#[tokio::test]
	async fn test_exclusion_survives_failover_onto_a_tainted_route() {
		tokio::time::pause();

		let origin = Origin::random().produce();
		let consumer = origin.consume();

		let publisher = Origin::new(1).unwrap();
		let peer = Origin::new(5).unwrap();
		let via_peer = OriginList::try_from(vec![publisher, peer]).unwrap();
		let local = OriginList::try_from(vec![publisher]).unwrap();

		let source_tainted = origin
			.create_broadcast("test", announce().with_hops(via_peer).with_cost(2))
			.unwrap();
		let mut dynamic_tainted = source_tainted.dynamic();
		settle().await;
		settle().await;
		let source_clean = origin.create_broadcast("test", announce().with_hops(local)).unwrap();
		let mut dynamic_clean = source_clean.dynamic();
		settle().await;

		let scoped = consumer.clone().excluding(peer);
		let broadcast = scoped.request_broadcast("test").await.unwrap();
		let _subscribing = broadcast.track("video").unwrap().subscribe(None);
		let _clean = accept_track(&mut dynamic_clean, "video").await;
		settle().await;

		// The clean route dies, so the front's only remaining route is the peer's.
		source_clean.abort(Error::Dropped).unwrap();
		settle().await;
		settle().await;
		dynamic_tainted.assert_no_request();

		// A fresh request now has nowhere clean to go, and says so rather than
		// silently serving the peer their own route.
		assert!(matches!(scoped.request_broadcast("test").await, Err(Error::Unroutable)));
	}

	/// The resolve-time check only proves the table is clean for a peer at that
	/// instant. A route through them attaching *afterwards* must not be adopted
	/// underneath their live subscription: they hold the shared front, so the front
	/// stays off that route while a clean one remains.
	#[tokio::test]
	async fn test_exclusion_holds_when_a_tainted_route_attaches_later() {
		tokio::time::pause();

		let origin = Origin::random().produce();
		let consumer = origin.consume();

		let publisher = Origin::new(1).unwrap();
		let peer = Origin::new(5).unwrap();
		let local = OriginList::try_from(vec![publisher]).unwrap();
		let via_peer = OriginList::try_from(vec![publisher, peer]).unwrap();

		// Only a clean route exists, so the peer legitimately gets the shared front.
		// Priced above the route that arrives later, so the front genuinely prefers
		// that one and staying put is the guard's doing, not the tie-break's.
		let source_clean = origin
			.create_broadcast("test", announce().with_hops(local).with_cost(5))
			.unwrap();
		let mut dynamic_clean = source_clean.dynamic();
		settle().await;
		settle().await;
		let scoped = consumer.clone().excluding(peer);
		let broadcast = scoped.request_broadcast("test").await.unwrap();
		let subscribing = broadcast.track("video").unwrap().subscribe(None);
		let mut producer_clean = accept_track(&mut dynamic_clean, "video").await;
		settle().await;
		let mut sub = subscribing.await.unwrap();
		producer_clean.append_group().unwrap();
		assert_eq!(sub.assert_group().sequence, 0);

		// A cheaper route back through the peer attaches. Without the registration
		// the front would re-splice onto it and hand the peer its own bytes.
		// `advertised` non-zero says the announcing relay is not itself carrying, which
		// keeps the simultaneous-activation handover gate (whose key comparison is
		// hash-random) out of this test: the front takes the cheaper route outright
		// unless the exclusion stops it.
		let mut tainted = announce().with_hops(via_peer.clone()).with_cost(0);
		tainted.advertised = 1;
		let mut source_tainted = origin.create_broadcast("test", tainted).unwrap();
		let mut dynamic_tainted = source_tainted.dynamic();
		settle().await;
		settle().await;
		dynamic_tainted.assert_no_request();
		producer_clean.append_group().unwrap();
		assert_eq!(sub.assert_group().sequence, 1);
		sub.assert_not_closed();

		// The registration ends with the last handle. The next table change is free
		// to take the cheaper route, which is what proves it was released.
		drop(sub);
		drop(broadcast);
		drop(scoped);
		settle().await;
		let mut bumped = announce().with_hops(via_peer).with_cost(1);
		bumped.advertised = 1;
		source_tainted.set_route(bumped).unwrap();
		settle().await;
		let plain = consumer.request_broadcast("test").await.unwrap();
		let _plain_track = plain.track("video").unwrap().subscribe(None);
		settle().await;
		settle().await;
		assert!(
			dynamic_tainted.requested_track().now_or_never().is_some(),
			"the front must be free to use the route again once the peer is gone"
		);
	}

	/// An announced path every route of which loops through the requester is
	/// unroutable, not missing: the dynamic handler resolves paths with no route
	/// chain to check, so consulting it would route around the split horizon.
	#[tokio::test]
	async fn test_excluded_path_never_reaches_the_dynamic_handler() {
		tokio::time::pause();

		let origin = Origin::random().produce();
		let consumer = origin.consume();
		let mut dynamic = origin.dynamic();

		let peer = Origin::new(5).unwrap();
		let tainted = OriginList::try_from(vec![Origin::new(1).unwrap(), peer]).unwrap();
		let _source = origin.create_broadcast("test", announce().with_hops(tainted)).unwrap();
		settle().await;
		settle().await;

		let scoped = consumer.clone().excluding(peer);
		assert!(matches!(scoped.request_broadcast("test").await, Err(Error::Unroutable)));
		assert!(
			dynamic.requested_broadcast().now_or_never().is_none(),
			"the dynamic handler was asked to route around the exclusion"
		);

		// An unannounced path still reaches the handler, exclusion or not.
		let _pending = scoped.request_broadcast("other");
		settle().await;
		assert!(
			dynamic.requested_broadcast().now_or_never().is_some(),
			"a genuinely missing path must still fall back"
		);
	}

	/// When every route flows through the requester, the path is unroutable for
	/// them: serving it would hand them their own bytes back.
	#[tokio::test]
	async fn test_dispatch_all_tainted_unroutable() {
		tokio::time::pause();

		let origin = Origin::random().produce();
		let consumer = origin.consume();

		let peer = Origin::new(5).unwrap();
		let tainted = OriginList::try_from(vec![Origin::new(1).unwrap(), peer]).unwrap();
		let _source = origin.create_broadcast("test", announce().with_hops(tainted)).unwrap();
		settle().await;
		settle().await;

		let scoped = consumer.clone().excluding(peer);
		match scoped.request_broadcast("test").await {
			Err(Error::Unroutable) => {}
			Err(err) => panic!("expected Unroutable, got {err:?}"),
			Ok(_) => panic!("expected Unroutable, got a broadcast"),
		}

		// Everyone else still resolves the shared front.
		consumer.request_broadcast("test").await.unwrap();
	}

	#[tokio::test]
	async fn test_duplicate_reverse() {
		tokio::time::pause();

		let origin = Origin::random().produce();

		let mut broadcast1 = origin.create_broadcast("test", announce()).unwrap();
		let mut broadcast2 = origin.create_broadcast("test", announce()).unwrap();
		settle().await;
		assert!(origin.consume().get_broadcast("test").is_some());

		// This is harder, finishing the newer source first.
		broadcast2.finish();
		settle().await;
		assert!(origin.consume().get_broadcast("test").is_some());

		broadcast1.finish();
		settle().await;
		assert!(origin.consume().get_broadcast("test").is_none());
	}

	#[tokio::test]
	async fn test_deterministic_tiebreak() {
		tokio::time::pause();

		fn hops(ids: &[u64]) -> OriginList {
			OriginList::try_from(
				ids.iter()
					.copied()
					.map(|id| Origin::new(id).unwrap())
					.collect::<Vec<_>>(),
			)
			.unwrap()
		}

		// Resolve the advertised route for "test" after creating both sources in
		// the given order.
		async fn winner(first: &[u64], second: &[u64]) -> OriginList {
			let origin = Origin::random().produce();
			let _a = origin
				.create_broadcast("test", announce().with_hops(hops(first)))
				.unwrap();
			let _b = origin
				.create_broadcast("test", announce().with_hops(hops(second)))
				.unwrap();
			settle().await;
			origin.consume().get_broadcast("test").unwrap().route().hops
		}

		// Two routes with equal hop counts but distinct chains (sharing the first
		// hop, so both may attach). The winner is decided by the deterministic
		// key, not arrival order, so both publish orders converge.
		let forward = winner(&[5, 20], &[5, 40]).await;
		let reverse = winner(&[5, 40], &[5, 20]).await;
		assert_eq!(forward, reverse, "tie-break must not depend on publish order");

		// A strictly shorter chain always wins regardless of the hash.
		assert_eq!(winner(&[5, 20], &[5]).await.len(), 1);
		assert_eq!(winner(&[5], &[5, 20]).await.len(), 1);
	}

	// A previous mpsc-based implementation could only deliver the first 127 broadcasts
	// instantly via `assert_next` (which uses `now_or_never`). The kio-backed
	// implementation polls synchronously and can deliver all of them without yielding.
	// Names are zero-padded so lexicographic delivery order matches the loop index.
	#[tokio::test]
	async fn test_many_announces() {
		let origin = Origin::random().produce();

		let mut consumer = origin.consume().announced();
		// Held for the duration: a dropped source unannounces immediately.
		let mut broadcasts = Vec::new();
		for i in 0..256 {
			broadcasts.push(origin.create_broadcast(format!("test{i:03}"), announce()).unwrap());
			settle().await;
		}

		for i in 0..256 {
			consumer.assert_next_some(format!("test{i:03}"));
		}
		consumer.assert_next_wait();
	}

	#[tokio::test]
	async fn test_many_announces_try() {
		let origin = Origin::random().produce();

		let mut consumer = origin.consume().announced();
		// Held for the duration: a dropped source unannounces immediately.
		let mut broadcasts = Vec::new();
		for i in 0..256 {
			broadcasts.push(origin.create_broadcast(format!("test{i:03}"), announce()).unwrap());
			settle().await;
		}

		for i in 0..256 {
			consumer.assert_try_next_some(format!("test{i:03}"));
		}
	}

	#[tokio::test]
	async fn test_with_root_basic() {
		let origin = Origin::random().produce();

		// Create a producer with root "/foo"
		let foo_producer = origin.with_root("foo").expect("should create root");
		assert_eq!(foo_producer.root().as_str(), "foo");

		let mut consumer = origin.consume().announced();

		// When publishing to "bar/baz", it should actually publish to "foo/bar/baz"
		let _broadcast = foo_producer
			.create_broadcast("bar/baz", announce())
			.expect("publish allowed");
		settle().await;
		// The original consumer should see the full path
		consumer.assert_next_some("foo/bar/baz");

		// A consumer created from the rooted producer should see the stripped path
		let mut foo_consumer = foo_producer.consume().announced();
		foo_consumer.assert_next_some("bar/baz");
	}

	#[tokio::test]
	async fn test_with_root_nested() {
		let origin = Origin::random().produce();

		// Create nested roots
		let foo_producer = origin.with_root("foo").expect("should create foo root");
		let foo_bar_producer = foo_producer.with_root("bar").expect("should create bar root");
		assert_eq!(foo_bar_producer.root().as_str(), "foo/bar");

		let mut consumer = origin.consume().announced();

		// Publishing to "baz" should actually publish to "foo/bar/baz"
		let _broadcast = foo_bar_producer
			.create_broadcast("baz", announce())
			.expect("publish allowed");
		settle().await;
		// The original consumer sees the full path
		consumer.assert_next_some("foo/bar/baz");

		// Consumer from foo_bar_producer sees just "baz"
		let mut foo_bar_consumer = foo_bar_producer.consume().announced();
		foo_bar_consumer.assert_next_some("baz");
	}

	#[tokio::test]
	async fn test_publish_scope_allows() {
		let origin = Origin::random().produce();

		// Create a producer that can only publish to "allowed" paths
		let limited_producer = origin
			.scope(&["allowed/path1".into(), "allowed/path2".into()])
			.expect("should create limited producer");

		// Should be able to publish to allowed paths
		let _broadcast = limited_producer
			.create_broadcast("allowed/path1", announce())
			.expect("publish allowed");
		let _keep2 = limited_producer
			.create_broadcast("allowed/path1/nested", announce())
			.expect("publish allowed");
		let _keep3 = limited_producer
			.create_broadcast("allowed/path2", announce())
			.expect("publish allowed");
		settle().await;

		// Should not be able to publish to disallowed paths
		assert!(limited_producer.create_broadcast("notallowed", announce()).is_err());
		assert!(limited_producer.create_broadcast("allowed", announce()).is_err()); // Parent of allowed path
		assert!(limited_producer.create_broadcast("other/path", announce()).is_err());
	}

	#[tokio::test]
	async fn test_publish_max_parts() {
		let origin = Origin::random().produce();

		let at_limit = (0..Path::MAX_PARTS)
			.map(|i| i.to_string())
			.collect::<Vec<_>>()
			.join("/");
		let _broadcast = origin
			.create_broadcast(at_limit.as_str(), announce())
			.expect("publish allowed");
		settle().await;

		let too_deep = format!("{at_limit}/extra");
		assert!(origin.create_broadcast(too_deep.as_str(), announce()).is_err());

		// The root counts toward the limit; a joined path past 32 parts is rejected.
		let rooted = origin.with_root("root").expect("wildcard allows any root");
		assert!(rooted.create_broadcast(at_limit.as_str(), announce()).is_err());
	}

	#[tokio::test]
	async fn test_publish_scope_empty() {
		let origin = Origin::random().produce();

		// Creating a producer with no allowed paths should return None
		assert!(origin.scope(&[]).is_none());
	}

	#[tokio::test]
	async fn test_consume_scope_filters() {
		let origin = Origin::random().produce();

		let mut consumer = origin.consume().announced();

		// Publish to different paths
		let _broadcast1 = origin.create_broadcast("allowed", announce()).unwrap();
		let _broadcast2 = origin.create_broadcast("allowed/nested", announce()).unwrap();
		let _broadcast3 = origin.create_broadcast("notallowed", announce()).unwrap();
		settle().await;

		// Create a consumer that only sees "allowed" paths
		let mut limited_consumer = origin
			.consume()
			.scope(&["allowed".into()])
			.expect("should create limited consumer")
			.announced();

		// Should only receive broadcasts under "allowed"
		limited_consumer.assert_next_some("allowed");
		limited_consumer.assert_next_some("allowed/nested");
		limited_consumer.assert_next_wait(); // Should not see "notallowed"

		// Unscoped consumer should see all
		consumer.assert_next_some("allowed");
		consumer.assert_next_some("allowed/nested");
		consumer.assert_next_some("notallowed");
	}

	#[tokio::test]
	async fn test_consume_scope_multiple_prefixes() {
		let origin = Origin::random().produce();

		let _broadcast1 = origin.create_broadcast("foo/test", announce()).unwrap();
		let _broadcast2 = origin.create_broadcast("bar/test", announce()).unwrap();
		let _broadcast3 = origin.create_broadcast("baz/test", announce()).unwrap();
		settle().await;

		// Consumer that only sees "foo" and "bar" paths
		let mut limited_consumer = origin
			.consume()
			.scope(&["foo".into(), "bar".into()])
			.expect("should create limited consumer")
			.announced();

		// Order depends on PathPrefixes canonical sort (lexicographic for same length)
		limited_consumer.assert_next_some("bar/test");
		limited_consumer.assert_next_some("foo/test");
		limited_consumer.assert_next_wait(); // Should not see "baz/test"
	}

	#[tokio::test]
	async fn test_with_root_and_publish_scope() {
		let origin = Origin::random().produce();

		// User connects to /foo root
		let foo_producer = origin.with_root("foo").expect("should create foo root");

		// Limit them to publish only to "bar" and "goop/pee" within /foo
		let limited_producer = foo_producer
			.scope(&["bar".into(), "goop/pee".into()])
			.expect("should create limited producer");

		let mut consumer = origin.consume().announced();

		// Should be able to publish to foo/bar and foo/goop/pee (but user sees as bar and goop/pee)
		let _broadcast = limited_producer
			.create_broadcast("bar", announce())
			.expect("publish allowed");
		let _keep2 = limited_producer
			.create_broadcast("bar/nested", announce())
			.expect("publish allowed");
		let _keep3 = limited_producer
			.create_broadcast("goop/pee", announce())
			.expect("publish allowed");
		let _keep4 = limited_producer
			.create_broadcast("goop/pee/nested", announce())
			.expect("publish allowed");
		settle().await;

		// Should not be able to publish outside allowed paths
		assert!(limited_producer.create_broadcast("baz", announce()).is_err());
		assert!(limited_producer.create_broadcast("goop", announce()).is_err()); // Parent of allowed
		assert!(limited_producer.create_broadcast("goop/other", announce()).is_err());

		// Original consumer sees full paths
		consumer.assert_next_some("foo/bar");
		consumer.assert_next_some("foo/bar/nested");
		consumer.assert_next_some("foo/goop/pee");
		consumer.assert_next_some("foo/goop/pee/nested");
	}

	#[tokio::test]
	async fn test_with_root_and_consume_scope() {
		let origin = Origin::random().produce();

		// Publish broadcasts
		let _broadcast1 = origin.create_broadcast("foo/bar/test", announce()).unwrap();
		let _broadcast2 = origin.create_broadcast("foo/goop/pee/test", announce()).unwrap();
		let _broadcast3 = origin.create_broadcast("foo/other/test", announce()).unwrap();
		settle().await;

		// User connects to /foo root
		let foo_producer = origin.with_root("foo").expect("should create foo root");

		// Create consumer limited to "bar" and "goop/pee" within /foo
		let mut limited_consumer = foo_producer
			.consume()
			.scope(&["bar".into(), "goop/pee".into()])
			.expect("should create limited consumer")
			.announced();

		// Should only see allowed paths (without foo prefix)
		limited_consumer.assert_next_some("bar/test");
		limited_consumer.assert_next_some("goop/pee/test");
		limited_consumer.assert_next_wait(); // Should not see "other/test"
	}

	#[tokio::test]
	async fn test_with_root_unauthorized() {
		let origin = Origin::random().produce();

		// First limit the producer to specific paths
		let limited_producer = origin
			.scope(&["allowed".into()])
			.expect("should create limited producer");

		// Trying to create a root outside allowed paths should fail
		assert!(limited_producer.with_root("notallowed").is_none());

		// But creating a root within allowed paths should work
		let allowed_root = limited_producer
			.with_root("allowed")
			.expect("should create allowed root");
		assert_eq!(allowed_root.root().as_str(), "allowed");
	}

	#[tokio::test]
	async fn test_wildcard_permission() {
		let origin = Origin::random().produce();

		// Producer with root access (empty string means wildcard)
		let root_producer = origin.clone();

		// Should be able to publish anywhere
		let _broadcast = root_producer
			.create_broadcast("any/path", announce())
			.expect("publish allowed");
		let _keep2 = root_producer
			.create_broadcast("other/path", announce())
			.expect("publish allowed");
		settle().await;

		// Can create any root
		let foo_producer = root_producer.with_root("foo").expect("should create any root");
		assert_eq!(foo_producer.root().as_str(), "foo");
	}

	#[tokio::test]
	async fn test_consume_broadcast_with_permissions() {
		let origin = Origin::random().produce();

		let _broadcast1 = origin.create_broadcast("allowed/test", announce()).unwrap();
		let _broadcast2 = origin.create_broadcast("notallowed/test", announce()).unwrap();
		settle().await;

		// Create limited consumer
		let limited_consumer = origin
			.consume()
			.scope(&["allowed".into()])
			.expect("should create limited consumer");

		// Should be able to get allowed broadcast
		let result = limited_consumer.get_broadcast("allowed/test");
		assert!(result.is_some());
		assert!(
			result
				.unwrap()
				.is_clone(&origin.consume().get_broadcast("allowed/test").unwrap())
		);

		// Should not be able to get disallowed broadcast
		assert!(limited_consumer.get_broadcast("notallowed/test").is_none());

		// Original consumer can get both
		let consumer = origin.consume();
		assert!(consumer.get_broadcast("allowed/test").is_some());
		assert!(consumer.get_broadcast("notallowed/test").is_some());
	}

	#[tokio::test]
	async fn test_nested_paths_with_permissions() {
		let origin = Origin::random().produce();

		// Create producer limited to "a/b/c"
		let limited_producer = origin.scope(&["a/b/c".into()]).expect("should create limited producer");

		// Should be able to publish to exact path and nested paths
		let _broadcast = limited_producer
			.create_broadcast("a/b/c", announce())
			.expect("publish allowed");
		let _keep2 = limited_producer
			.create_broadcast("a/b/c/d", announce())
			.expect("publish allowed");
		let _keep3 = limited_producer
			.create_broadcast("a/b/c/d/e", announce())
			.expect("publish allowed");
		settle().await;

		// Should not be able to publish to parent or sibling paths
		assert!(limited_producer.create_broadcast("a", announce()).is_err());
		assert!(limited_producer.create_broadcast("a/b", announce()).is_err());
		assert!(limited_producer.create_broadcast("a/b/other", announce()).is_err());
	}

	#[tokio::test]
	async fn test_multiple_consumers_with_different_permissions() {
		let origin = Origin::random().produce();

		// Publish to different paths
		let _broadcast1 = origin.create_broadcast("foo/test", announce()).unwrap();
		let _broadcast2 = origin.create_broadcast("bar/test", announce()).unwrap();
		let _broadcast3 = origin.create_broadcast("baz/test", announce()).unwrap();
		settle().await;

		// Create consumers with different permissions
		let mut foo_consumer = origin
			.consume()
			.scope(&["foo".into()])
			.expect("should create foo consumer")
			.announced();

		let mut bar_consumer = origin
			.consume()
			.scope(&["bar".into()])
			.expect("should create bar consumer")
			.announced();

		let mut foobar_consumer = origin
			.consume()
			.scope(&["foo".into(), "bar".into()])
			.expect("should create foobar consumer")
			.announced();

		// Each consumer should only see their allowed paths
		foo_consumer.assert_next_some("foo/test");
		foo_consumer.assert_next_wait();

		bar_consumer.assert_next_some("bar/test");
		bar_consumer.assert_next_wait();

		foobar_consumer.assert_next_some("bar/test");
		foobar_consumer.assert_next_some("foo/test");
		foobar_consumer.assert_next_wait();
	}

	#[tokio::test]
	async fn test_select_with_empty_prefix() {
		let origin = Origin::random().produce();

		// User with root "demo" allowed to subscribe to "worm-node" and "foobar"
		let demo_producer = origin.with_root("demo").expect("should create demo root");
		let limited_producer = demo_producer
			.scope(&["worm-node".into(), "foobar".into()])
			.expect("should create limited producer");

		// Publish some broadcasts
		let _broadcast1 = limited_producer
			.create_broadcast("worm-node/test", announce())
			.expect("publish allowed");
		let _broadcast2 = limited_producer
			.create_broadcast("foobar/test", announce())
			.expect("publish allowed");
		settle().await;

		// scope with empty prefix should keep the exact same "worm-node" and "foobar" nodes
		let mut consumer = limited_producer
			.consume()
			.scope(&["".into()])
			.expect("should create consumer with empty prefix")
			.announced();

		// Should see both broadcasts (order depends on PathPrefixes sort)
		let a1 = consumer.try_next().expect("expected first announcement");
		let a2 = consumer.try_next().expect("expected second announcement");
		consumer.assert_next_wait();

		let mut paths: Vec<_> = [&a1, &a2].iter().map(|a| a.path.to_string()).collect();
		paths.sort();
		assert_eq!(paths, ["foobar/test", "worm-node/test"]);
	}

	#[tokio::test]
	async fn test_select_narrowing_scope() {
		let origin = Origin::random().produce();

		// User with root "demo" allowed to subscribe to "worm-node" and "foobar"
		let demo_producer = origin.with_root("demo").expect("should create demo root");
		let limited_producer = demo_producer
			.scope(&["worm-node".into(), "foobar".into()])
			.expect("should create limited producer");

		// Publish broadcasts at different levels
		let _broadcast1 = limited_producer
			.create_broadcast("worm-node", announce())
			.expect("publish allowed");
		let _broadcast2 = limited_producer
			.create_broadcast("worm-node/foo", announce())
			.expect("publish allowed");
		let _broadcast3 = limited_producer
			.create_broadcast("foobar/bar", announce())
			.expect("publish allowed");
		settle().await;

		// Test 1: scope("worm-node") should result in a single "" node with contents of "worm-node" ONLY
		let mut worm_consumer = limited_producer
			.consume()
			.scope(&["worm-node".into()])
			.expect("should create worm-node consumer")
			.announced();

		// Should see worm-node content with paths stripped to ""
		worm_consumer.assert_next_some("worm-node");
		worm_consumer.assert_next_some("worm-node/foo");
		worm_consumer.assert_next_wait(); // Should NOT see foobar content

		// Test 2: scope("worm-node/foo") should result in a "" node with contents of "worm-node/foo"
		let mut foo_consumer = limited_producer
			.consume()
			.scope(&["worm-node/foo".into()])
			.expect("should create worm-node/foo consumer")
			.announced();

		foo_consumer.assert_next_some("worm-node/foo");
		foo_consumer.assert_next_wait(); // Should NOT see other content
	}

	#[tokio::test]
	async fn test_select_multiple_roots_with_empty_prefix() {
		let origin = Origin::random().produce();

		// Producer with multiple allowed roots
		let limited_producer = origin
			.scope(&["app1".into(), "app2".into(), "shared".into()])
			.expect("should create limited producer");

		// Publish to each root
		let _broadcast1 = limited_producer
			.create_broadcast("app1/data", announce())
			.expect("publish allowed");
		let _broadcast2 = limited_producer
			.create_broadcast("app2/config", announce())
			.expect("publish allowed");
		let _broadcast3 = limited_producer
			.create_broadcast("shared/resource", announce())
			.expect("publish allowed");
		settle().await;

		// scope with empty prefix should maintain all roots
		let mut consumer = limited_producer
			.consume()
			.scope(&["".into()])
			.expect("should create consumer with empty prefix")
			.announced();

		// Should see all broadcasts from all roots
		consumer.assert_next_some("app1/data");
		consumer.assert_next_some("app2/config");
		consumer.assert_next_some("shared/resource");
		consumer.assert_next_wait();
	}

	#[tokio::test]
	async fn test_publish_scope_with_empty_prefix() {
		let origin = Origin::random().produce();

		// Producer with specific allowed paths
		let limited_producer = origin
			.scope(&["services/api".into(), "services/web".into()])
			.expect("should create limited producer");

		// scope with empty prefix should keep the same restrictions
		let same_producer = limited_producer
			.scope(&["".into()])
			.expect("should create producer with empty prefix");

		// Should still have the same publishing restrictions
		let _broadcast = same_producer
			.create_broadcast("services/api", announce())
			.expect("publish allowed");
		let _keep2 = same_producer
			.create_broadcast("services/web", announce())
			.expect("publish allowed");
		assert!(same_producer.create_broadcast("services/db", announce()).is_err());
		assert!(same_producer.create_broadcast("other", announce()).is_err());
	}

	#[tokio::test]
	async fn test_select_narrowing_to_deeper_path() {
		let origin = Origin::random().produce();

		// Producer with broad permission
		let limited_producer = origin.scope(&["org".into()]).expect("should create limited producer");

		// Publish at various depths
		let _broadcast1 = limited_producer
			.create_broadcast("org/team1/project1", announce())
			.expect("publish allowed");
		let _broadcast2 = limited_producer
			.create_broadcast("org/team1/project2", announce())
			.expect("publish allowed");
		let _broadcast3 = limited_producer
			.create_broadcast("org/team2/project1", announce())
			.expect("publish allowed");
		settle().await;

		// Narrow down to team2 only
		let mut team2_consumer = limited_producer
			.consume()
			.scope(&["org/team2".into()])
			.expect("should create team2 consumer")
			.announced();

		team2_consumer.assert_next_some("org/team2/project1");
		team2_consumer.assert_next_wait(); // Should NOT see team1 content

		// Further narrow down to team1/project1
		let mut project1_consumer = limited_producer
			.consume()
			.scope(&["org/team1/project1".into()])
			.expect("should create project1 consumer")
			.announced();

		// Should only see project1 content at root
		project1_consumer.assert_next_some("org/team1/project1");
		project1_consumer.assert_next_wait();
	}

	#[tokio::test]
	async fn test_select_with_non_matching_prefix() {
		let origin = Origin::random().produce();

		// Producer with specific allowed paths
		let limited_producer = origin
			.scope(&["allowed/path".into()])
			.expect("should create limited producer");

		// Trying to scope with a completely different prefix should return None
		assert!(limited_producer.consume().scope(&["different/path".into()]).is_none());

		// Similarly for scope
		assert!(limited_producer.scope(&["other/path".into()]).is_none());
	}

	// Regression test for https://github.com/moq-dev/moq/issues/910
	// with_root panics when String has trailing slash (AsPath for String skips normalization)
	#[tokio::test]
	async fn test_with_root_trailing_slash_consumer() {
		let origin = Origin::random().produce();

		// Use an owned String so the trailing slash is NOT normalized away.
		let prefix = "some_prefix/".to_string();
		let mut consumer = origin.consume().with_root(prefix).unwrap().announced();

		let _b = origin.create_broadcast("some_prefix/test", announce()).unwrap();
		settle().await;
		consumer.assert_next_some("test");
	}

	// Same issue but for the producer side of with_root
	#[tokio::test]
	async fn test_with_root_trailing_slash_producer() {
		let origin = Origin::random().produce();

		// Use an owned String so the trailing slash is NOT normalized away.
		let prefix = "some_prefix/".to_string();
		let rooted = origin.with_root(prefix).unwrap();

		let _b = rooted.create_broadcast("test", announce()).unwrap();
		settle().await;

		let mut consumer = rooted.consume().announced();
		consumer.assert_next_some("test");
	}

	// Verify unannounce also doesn't panic with trailing slash
	#[tokio::test]
	async fn test_with_root_trailing_slash_unannounce() {
		tokio::time::pause();

		let origin = Origin::random().produce();

		let prefix = "some_prefix/".to_string();
		let mut consumer = origin.consume().with_root(prefix).unwrap().announced();

		let mut b = origin.create_broadcast("some_prefix/test", announce()).unwrap();
		settle().await;
		consumer.assert_next_some("test");

		// Finish the broadcast to trigger an immediate unannounce.
		b.finish();
		settle().await;

		// unannounce also calls strip_prefix(&self.root).unwrap()
		consumer.assert_next_none("test");
	}

	#[tokio::test]
	async fn test_select_maintains_access_with_wider_prefix() {
		let origin = Origin::random().produce();

		// Setup: user with root "demo" allowed to subscribe to specific paths
		let demo_producer = origin.with_root("demo").expect("should create demo root");
		let user_producer = demo_producer
			.scope(&["worm-node".into(), "foobar".into()])
			.expect("should create user producer");

		// Publish some data
		let _broadcast1 = user_producer
			.create_broadcast("worm-node/data", announce())
			.expect("publish allowed");
		let _broadcast2 = user_producer
			.create_broadcast("foobar", announce())
			.expect("publish allowed");
		settle().await;

		// Key test: scope with "" should maintain access to allowed roots
		let mut consumer = user_producer
			.consume()
			.scope(&["".into()])
			.expect("scope with empty prefix should not fail when user has specific permissions")
			.announced();

		// Should still receive broadcasts from allowed paths (order not guaranteed)
		let a1 = consumer.try_next().expect("expected first announcement");
		let a2 = consumer.try_next().expect("expected second announcement");
		consumer.assert_next_wait();

		let mut paths: Vec<_> = [&a1, &a2].iter().map(|a| a.path.to_string()).collect();
		paths.sort();
		assert_eq!(paths, ["foobar", "worm-node/data"]);

		// Also test that we can still narrow the scope
		let mut narrow_consumer = user_producer
			.consume()
			.scope(&["worm-node".into()])
			.expect("should be able to narrow scope to worm-node")
			.announced();

		narrow_consumer.assert_next_some("worm-node/data");
		narrow_consumer.assert_next_wait(); // Should not see foobar
	}

	#[tokio::test]
	async fn test_duplicate_prefixes_deduped() {
		let origin = Origin::random().produce();

		// scope with duplicate prefixes should work (deduped internally)
		let producer = origin
			.scope(&["demo".into(), "demo".into()])
			.expect("should create producer");

		let _broadcast = producer
			.create_broadcast("demo/stream", announce())
			.expect("publish allowed");
		settle().await;

		let mut consumer = producer.consume().announced();
		consumer.assert_next_some("demo/stream");
		consumer.assert_next_wait();
	}

	#[tokio::test]
	async fn test_overlapping_prefixes_deduped() {
		let origin = Origin::random().produce();

		// "demo" and "demo/foo". "demo/foo" is redundant, only "demo" should remain
		let producer = origin
			.scope(&["demo".into(), "demo/foo".into()])
			.expect("should create producer");

		// Can still publish under "demo/bar" since "demo" covers everything
		let _broadcast = producer
			.create_broadcast("demo/bar/stream", announce())
			.expect("publish allowed");
		settle().await;

		let mut consumer = producer.consume().announced();
		consumer.assert_next_some("demo/bar/stream");
		consumer.assert_next_wait();
	}

	#[tokio::test]
	async fn test_overlapping_prefixes_no_duplicate_announcements() {
		let origin = Origin::random().produce();

		// Both "demo" and "demo/foo" are requested. Should only have one node
		let producer = origin
			.scope(&["demo".into(), "demo/foo".into()])
			.expect("should create producer");

		let _broadcast = producer
			.create_broadcast("demo/foo/stream", announce())
			.expect("publish allowed");
		settle().await;

		let mut consumer = producer.consume().announced();
		// Should only get ONE announcement (not two from overlapping nodes)
		consumer.assert_next_some("demo/foo/stream");
		consumer.assert_next_wait();
	}

	#[tokio::test]
	async fn test_allowed_returns_deduped_prefixes() {
		let origin = Origin::random().produce();

		let producer = origin
			.scope(&["demo".into(), "demo/foo".into(), "anon".into()])
			.expect("should create producer");

		let allowed: Vec<_> = producer.allowed().collect();
		assert_eq!(allowed.len(), 2, "demo/foo should be subsumed by demo");
	}

	#[tokio::test]
	async fn test_announced_broadcast_already_announced() {
		let origin = Origin::random().produce();

		let _broadcast = origin.create_broadcast("test", announce()).unwrap();
		settle().await;

		let consumer = origin.consume();
		let result = consumer.announced_broadcast("test").await.expect("should find it");
		assert!(result.is_clone(&consumer.get_broadcast("test").unwrap()));
	}

	#[tokio::test]
	async fn test_announced_broadcast_delayed() {
		tokio::time::pause();

		let origin = Origin::random().produce();

		let consumer = origin.consume();

		// Start waiting before it's announced.
		let wait = tokio::spawn({
			let consumer = consumer.clone();
			async move { consumer.announced_broadcast("test").await }
		});

		// Give the spawned task a chance to subscribe.
		tokio::task::yield_now().await;

		let _broadcast = origin.create_broadcast("test", announce()).unwrap();
		settle().await;

		let result = wait.await.unwrap().expect("should find it");
		assert!(result.is_clone(&consumer.get_broadcast("test").unwrap()));
	}

	#[tokio::test]
	async fn test_announced_broadcast_ignores_unrelated_paths() {
		tokio::time::pause();

		let origin = Origin::random().produce();

		let consumer = origin.consume();

		let wait = tokio::spawn({
			let consumer = consumer.clone();
			async move { consumer.announced_broadcast("target").await }
		});

		tokio::task::yield_now().await;

		// Publish an unrelated broadcast first. announced_broadcast should skip it.
		let _other = origin.create_broadcast("other", announce()).unwrap();
		settle().await;
		tokio::task::yield_now().await;
		assert!(!wait.is_finished(), "must not resolve on unrelated path");

		let _target = origin.create_broadcast("target", announce()).unwrap();
		settle().await;
		let result = wait.await.unwrap().expect("should find target");
		assert!(result.is_clone(&consumer.get_broadcast("target").unwrap()));
	}

	#[tokio::test]
	async fn test_announced_broadcast_skips_nested_paths() {
		tokio::time::pause();

		let origin = Origin::random().produce();

		let consumer = origin.consume();

		let wait = tokio::spawn({
			let consumer = consumer.clone();
			async move { consumer.announced_broadcast("foo").await }
		});

		tokio::task::yield_now().await;

		// "foo/bar" is under the prefix scope, but it's not the exact path. Skip it.
		let _nested = origin.create_broadcast("foo/bar", announce()).unwrap();
		settle().await;
		tokio::task::yield_now().await;
		assert!(!wait.is_finished(), "must not resolve on a nested path");

		let _exact = origin.create_broadcast("foo", announce()).unwrap();
		settle().await;
		let result = wait.await.unwrap().expect("should find foo exactly");
		assert!(result.is_clone(&consumer.get_broadcast("foo").unwrap()));
	}

	#[tokio::test]
	async fn test_announced_broadcast_disallowed() {
		let origin = Origin::random().produce();
		let limited = origin
			.consume()
			.scope(&["allowed".into()])
			.expect("should create limited");

		// Path is outside allowed prefixes. Should return None immediately.
		assert!(limited.announced_broadcast("notallowed").await.is_none());
	}

	#[tokio::test]
	async fn test_announced_broadcast_scope_too_narrow() {
		// Consumer's scope is narrower than the requested path: asking for `foo` on a consumer
		// limited to `foo/specific` can never resolve. Must return None, not loop forever.
		let origin = Origin::random().produce();
		let limited = origin
			.consume()
			.scope(&["foo/specific".into()])
			.expect("should create limited");

		// now_or_never so we fail fast instead of hanging if the guard regresses.
		let result = limited
			.announced_broadcast("foo")
			.now_or_never()
			.expect("must not block");
		assert!(result.is_none());
	}

	// Coalescing tests: a slow cursor that doesn't drain between updates
	// should observe a bounded number of deliveries.

	#[tokio::test]
	async fn test_coalesce_announce_then_unannounce() {
		// announce + unannounce that the cursor hasn't observed yet collapses to nothing.
		tokio::time::pause();

		let origin = Origin::random().produce();
		let mut announced = origin.consume().announced();

		let mut broadcast = origin.create_broadcast("test", announce()).unwrap();
		settle().await;
		broadcast.finish();

		settle().await;

		announced.assert_next_wait();
	}

	#[tokio::test]
	async fn test_coalesce_announce_unannounce_announce() {
		// announce, unannounce, announce that the cursor hasn't drained collapses
		// to a single Announce of the latest broadcast.
		tokio::time::pause();

		let origin = Origin::random().produce();
		let mut announced = origin.consume().announced();

		let mut broadcast1 = origin.create_broadcast("test", announce()).unwrap();
		settle().await;
		broadcast1.finish();
		settle().await;
		let _broadcast2 = origin.create_broadcast("test", announce()).unwrap();
		settle().await;

		announced.assert_next_some("test");
		announced.assert_next_wait();
	}

	#[tokio::test]
	async fn test_coalesce_unannounce_announce_preserved() {
		// unannounce followed by announce of a different broadcast must be preserved
		// as two deliveries so the cursor learns the origin changed.
		tokio::time::pause();

		let origin = Origin::random().produce();
		let mut broadcast1 = origin.create_broadcast("test", announce()).unwrap();
		settle().await;

		let mut announced = origin.consume().announced();
		announced.assert_next_some("test");

		// Finish, then publish a fresh broadcast at the same path.
		broadcast1.finish();
		settle().await;

		let _broadcast2 = origin.create_broadcast("test", announce()).unwrap();
		settle().await;

		// The cursor must see the unannounce before the new announce.
		announced.assert_next_none("test");
		announced.assert_next_some("test");
		announced.assert_next_wait();
	}

	#[tokio::test]
	async fn test_coalesce_unannounce_announce_unannounce() {
		// unannounce + announce + unannounce collapses to a single unannounce: the
		// embedded announce was never observed.
		tokio::time::pause();

		let origin = Origin::random().produce();
		let mut broadcast1 = origin.create_broadcast("test", announce()).unwrap();
		settle().await;

		let mut announced = origin.consume().announced();
		announced.assert_next_some("test");

		broadcast1.finish();
		settle().await;

		let mut broadcast2 = origin.create_broadcast("test", announce()).unwrap();
		settle().await;
		broadcast2.finish();
		settle().await;

		announced.assert_next_none("test");
		announced.assert_next_wait();
	}

	#[tokio::test]
	async fn test_coalesce_churn_bounded() {
		// A churn loop on a single path should keep the pending set bounded.
		// Backup promotion during cleanup can leave the cursor with zero or one
		// pending update for "test" depending on the order tasks run; we only
		// require that churn doesn't accumulate across iterations.
		tokio::time::pause();

		let origin = Origin::random().produce();
		let mut announced = origin.consume().announced();

		for _ in 0..1000 {
			let mut broadcast = origin.create_broadcast("test", announce()).unwrap();
			settle().await;
			broadcast.finish();
		}
		settle().await;

		let mut collected = Vec::new();
		while let Some(update) = announced.try_next() {
			collected.push(update);
		}
		assert!(
			collected.len() <= 1,
			"expected at most one pending update, got {}",
			collected.len()
		);
		assert!(
			collected.iter().all(|a| a.path == Path::new("test")),
			"unexpected path in pending updates",
		);
	}

	// Consumer should be cheap to clone: cloning must NOT drain any
	// other cursor's announce channel. A freshly-built AnnounceConsumer
	// still receives the active backlog.
	#[tokio::test]
	async fn test_consumer_clone_is_side_effect_free() {
		let origin = Origin::random().produce();

		let _broadcast1 = origin.create_broadcast("test1", announce()).unwrap();
		let _broadcast2 = origin.create_broadcast("test2", announce()).unwrap();
		settle().await;

		let consumer = origin.consume();
		let mut announced = consumer.announced();

		// Cloning the Consumer many times and looking up broadcasts
		// must not consume any events from the existing cursor.
		for _ in 0..16 {
			let cloned = consumer.clone();
			assert!(cloned.get_broadcast("test1").is_some());
			assert!(cloned.get_broadcast("test2").is_some());
		}

		// The original cursor still sees both announcements in their
		// natural order, undisturbed by the clones above.
		let a1 = announced.try_next().expect("first announcement");
		let a2 = announced.try_next().expect("second announcement");
		announced.assert_next_wait();

		let mut paths: Vec<_> = [&a1, &a2].iter().map(|a| a.path.to_string()).collect();
		paths.sort();
		assert_eq!(paths, ["test1", "test2"]);

		// A freshly-built AnnounceConsumer still receives the active backlog.
		let mut fresh = consumer.announced();
		let b1 = fresh.try_next().expect("backlog: first");
		let b2 = fresh.try_next().expect("backlog: second");
		fresh.assert_next_wait();

		let mut paths: Vec<_> = [&b1, &b2].iter().map(|a| a.path.to_string()).collect();
		paths.sort();
		assert_eq!(paths, ["test1", "test2"]);
	}

	// With no Dynamic handler, an unannounced path resolves to Unroutable.
	#[tokio::test]
	async fn dynamic_request_unroutable_without_handler() {
		let origin = Origin::random().produce();
		let consumer = origin.consume();
		assert!(matches!(
			consumer.request_broadcast("missing").await,
			Err(Error::Unroutable)
		));
	}

	// A dynamically served broadcast resolves the requester and serves tracks, but is
	// never announced.
	#[tokio::test(start_paused = true)]
	async fn dynamic_request_served_not_announced() {
		let origin = Origin::random().produce();
		let mut dynamic = origin.dynamic();
		let consumer = origin.consume();

		// A separate announce cursor must never observe the dynamic broadcast.
		let mut announced = origin.consume().announced();
		announced.assert_next_wait();

		let served = broadcast::Info::new().produce();
		// Request a path that nobody announced; the future stays pending until served.
		// Registration happens up front, so the handler sees the request immediately.
		let request_fut = consumer.request_broadcast("fallback");

		// The handler serves it with a live broadcast it keeps producing into.
		let mut served_dynamic = served.dynamic();

		let request = dynamic.requested_broadcast().await.unwrap();
		assert_eq!(request.path(), &Path::new("fallback"));
		request.accept(&served);

		let broadcast = request_fut.await.unwrap();
		assert!(broadcast.is_clone(&served.consume()));

		// The served broadcast is live: a track subscription resolves via its handler.
		let track_fut = broadcast.track("video").unwrap().subscribe(None);
		let mut producer = served_dynamic.requested_track().await.unwrap().accept(None);
		let mut track = track_fut.await.unwrap();
		producer.append_group().unwrap();
		track.assert_group();

		// Still nothing announced.
		announced.assert_next_wait();
	}

	// Concurrent requests for the same queued path coalesce onto one handler request.
	#[tokio::test(start_paused = true)]
	async fn dynamic_request_coalesces() {
		let origin = Origin::random().produce();
		let mut dynamic = origin.dynamic();
		let consumer = origin.consume();

		// Both register before the handler drains either.
		let f1 = consumer.request_broadcast("dup");
		let f2 = consumer.request_broadcast("dup");

		// Exactly one request reaches the handler.
		let request = dynamic.requested_broadcast().await.unwrap();
		assert_eq!(request.path(), &Path::new("dup"));
		assert!(
			dynamic.requested_broadcast().now_or_never().is_none(),
			"a coalesced request must not be served twice"
		);

		// Accepting resolves both awaiting requesters with the same broadcast.
		let served = broadcast::Info::new().produce();
		request.accept(&served);
		assert!(f1.await.unwrap().is_clone(&served.consume()));
		assert!(f2.await.unwrap().is_clone(&served.consume()));
	}

	// A repeat request for an already-served, still-live path shares the same broadcast
	// instead of asking the handler again (no duplicate upstream subscription).
	#[tokio::test(start_paused = true)]
	async fn dynamic_request_dedups_served() {
		let origin = Origin::random().produce();
		let mut dynamic = origin.dynamic();
		let consumer = origin.consume();

		let request_fut = consumer.request_broadcast("fallback");
		let request = dynamic.requested_broadcast().await.unwrap();
		let served = broadcast::Info::new().produce();
		request.accept(&served);
		let first = request_fut.await.unwrap();
		assert!(first.is_clone(&served.consume()));

		// The repeat resolves immediately to the same broadcast...
		let second = consumer.request_broadcast("fallback").await.unwrap();
		assert!(second.is_clone(&served.consume()));

		// ...and the handler never sees a second request.
		assert!(
			dynamic.requested_broadcast().now_or_never().is_none(),
			"a still-live served broadcast must not be re-requested from the handler"
		);
	}

	// Once a served broadcast closes, its cache entry is stale, so the next request re-serves.
	#[tokio::test(start_paused = true)]
	async fn dynamic_request_reserves_after_close() {
		let origin = Origin::random().produce();
		let mut dynamic = origin.dynamic();
		let consumer = origin.consume();

		let request_fut = consumer.request_broadcast("fallback");
		let request = dynamic.requested_broadcast().await.unwrap();
		let served = broadcast::Info::new().produce();
		request.accept(&served);
		request_fut.await.unwrap();

		// Close the first served broadcast; the weak cache entry goes stale.
		drop(served);

		// A fresh request must reach the handler again and resolve to the new broadcast.
		let request_fut = consumer.request_broadcast("fallback");
		let request = dynamic.requested_broadcast().await.unwrap();
		assert_eq!(request.path(), &Path::new("fallback"));
		let served = broadcast::Info::new().produce();
		request.accept(&served);
		assert!(request_fut.await.unwrap().is_clone(&served.consume()));
	}

	// Serving many distinct one-shot paths that each close must not grow the `served` cache
	// unboundedly: the amortized GC on `accept` reclaims the stale entries left by closed ones.
	#[tokio::test(start_paused = true)]
	async fn dynamic_request_served_cache_bounded() {
		let origin = Origin::random().produce();
		let mut dynamic = origin.dynamic();
		let consumer = origin.consume();

		for i in 0..100 {
			let path = format!("one-shot/{i}");
			let request_fut = consumer.request_broadcast(&path);
			let request = dynamic.requested_broadcast().await.unwrap();
			let served = broadcast::Info::new().produce();
			request.accept(&served);
			request_fut.await.unwrap();
			// Close the served broadcast; its cache entry is now stale.
			drop(served);
		}

		// The GC keeps the map bounded by the live count (zero here) plus a small probe window,
		// rather than one entry per distinct path.
		assert!(
			origin.dynamic.read().served.len() <= 4,
			"stale served entries must be reclaimed, not accumulate per distinct path: {}",
			origin.dynamic.read().served.len()
		);
	}

	// A repeat request in the window after the handler picks one up but before it accepts
	// coalesces onto the in-flight request instead of queuing a duplicate.
	#[tokio::test(start_paused = true)]
	async fn dynamic_request_coalesces_after_handoff() {
		let origin = Origin::random().produce();
		let mut dynamic = origin.dynamic();
		let consumer = origin.consume();

		let f1 = consumer.request_broadcast("fallback");
		// Handler drains the request but has not accepted yet.
		let request = dynamic.requested_broadcast().await.unwrap();

		// A second request in this window must not queue another handler request.
		let f2 = consumer.request_broadcast("fallback");
		assert!(
			dynamic.requested_broadcast().now_or_never().is_none(),
			"a repeat request during hand-off must coalesce, not re-queue"
		);

		// Accepting resolves both awaiting requesters with the same broadcast.
		let served = broadcast::Info::new().produce();
		request.accept(&served);
		assert!(f1.await.unwrap().is_clone(&served.consume()));
		assert!(f2.await.unwrap().is_clone(&served.consume()));
	}

	// Dropping a handed-off request without accept/reject rejects every coalesced requester.
	#[tokio::test(start_paused = true)]
	async fn dynamic_request_dropped_after_handoff() {
		let origin = Origin::random().produce();
		let mut dynamic = origin.dynamic();
		let consumer = origin.consume();

		let f1 = consumer.request_broadcast("fallback");
		let request = dynamic.requested_broadcast().await.unwrap();
		let f2 = consumer.request_broadcast("fallback");

		// Abandon it; both requesters resolve to Unroutable instead of hanging.
		drop(request);
		assert!(matches!(f1.await, Err(Error::Unroutable)));
		assert!(matches!(f2.await, Err(Error::Unroutable)));
	}

	// Rejecting a request resolves the requester with the error.
	#[tokio::test(start_paused = true)]
	async fn dynamic_request_rejected() {
		let origin = Origin::random().produce();
		let mut dynamic = origin.dynamic();
		let consumer = origin.consume();

		let request_fut = consumer.request_broadcast("fallback");

		let request = dynamic.requested_broadcast().await.unwrap();
		request.reject(Error::Cancel);

		assert!(matches!(request_fut.await, Err(Error::Cancel)));
	}

	// After a rejected hand-off, a fresh request for the same path reaches the handler again:
	// the rejected `Request`'s removal + `Drop` leave the request queue consistent
	// (a stale/clobbered entry would strand this request or panic the handler).
	#[tokio::test(start_paused = true)]
	async fn dynamic_request_rerequest_after_reject() {
		let origin = Origin::random().produce();
		let mut dynamic = origin.dynamic();
		let consumer = origin.consume();

		let f1 = consumer.request_broadcast("fallback");
		dynamic.requested_broadcast().await.unwrap().reject(Error::Unroutable);
		assert!(matches!(f1.await, Err(Error::Unroutable)));

		let served = broadcast::Info::new().produce();
		// A fresh request re-reaches the handler and can be served.
		let f2 = consumer.request_broadcast("fallback");
		let request = dynamic.requested_broadcast().await.unwrap();
		assert_eq!(request.path(), &Path::new("fallback"));
		request.accept(&served);
		assert!(f2.await.unwrap().is_clone(&served.consume()));
	}

	// Dropping the last handler resolves queued requests with an error and reverts to
	// resolving Unroutable.
	#[tokio::test(start_paused = true)]
	async fn dynamic_request_handler_dropped() {
		let origin = Origin::random().produce();
		let dynamic = origin.dynamic();
		let consumer = origin.consume();

		let request_fut = consumer.request_broadcast("fallback");
		drop(dynamic);
		assert!(matches!(request_fut.await, Err(Error::Unroutable)));

		// With no handler left, a fresh request resolves Unroutable.
		assert!(matches!(
			consumer.request_broadcast("again").await,
			Err(Error::Unroutable)
		));
	}

	// `accept` is decoupled from the dynamic count: once a handler has picked a request up,
	// it can still serve it even if every handler (including itself) drops first, flipping the
	// count to zero. The in-flight request must not be rejected as `Unroutable`.
	#[tokio::test(start_paused = true)]
	async fn dynamic_request_accept_after_handler_dropped() {
		let origin = Origin::random().produce();
		let mut dynamic = origin.dynamic();
		let consumer = origin.consume();

		let request_fut = consumer.request_broadcast("fallback");

		// The handler picks the request up, then every handler drops (count -> 0).
		let request = dynamic.requested_broadcast().await.unwrap();
		drop(dynamic);

		let served = broadcast::Info::new().produce();
		// Accept still resolves the awaiting requester with the served broadcast.
		request.accept(&served);
		assert!(request_fut.await.unwrap().is_clone(&served.consume()));
	}

	// A published broadcast wins over the dynamic fallback; no request is queued.
	#[tokio::test(start_paused = true)]
	async fn dynamic_request_prefers_announced() {
		let origin = Origin::random().produce();
		let mut dynamic = origin.dynamic();
		let consumer = origin.consume();

		let _broadcast = origin.create_broadcast("live", announce()).unwrap();
		settle().await;

		let got = consumer.request_broadcast("live").await.unwrap();
		assert!(
			got.is_clone(&consumer.get_broadcast("live").unwrap()),
			"should return the published broadcast"
		);
		assert!(
			dynamic.requested_broadcast().now_or_never().is_none(),
			"a published path must not queue a fallback request"
		);
	}

	// Cloning a handler and dropping the clone must not flip the count to zero.
	#[tokio::test(start_paused = true)]
	async fn dynamic_clone_keeps_alive() {
		let origin = Origin::random().produce();
		let dynamic = origin.dynamic();
		let consumer = origin.consume();

		drop(dynamic.clone());

		// The original handle is still live, so the request registers (stays pending)
		// instead of resolving Unroutable.
		let request_fut = consumer.request_broadcast("fallback");
		assert!(
			request_fut.now_or_never().is_none(),
			"request should stay pending until served"
		);
	}
}
