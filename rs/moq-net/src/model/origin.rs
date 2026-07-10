use crate::{broadcast, cache, track};
use std::{
	collections::{BTreeMap, HashMap, VecDeque},
	fmt,
	sync::atomic::{AtomicU64, Ordering},
	task::{Poll, ready},
};

use rand::RngExt;
use web_async::Lock;

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
/// the [`cache::Pool`] every group in the tree registers with, so a relay configures one
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

	/// The cache pool broadcasts under this origin register their groups with. It
	/// flows down the ownership chain (origin -> broadcast -> track -> group), so a
	/// group reaches it via `track.broadcast.origin.pool`. Unbounded by default; a
	/// relay sets a bounded one (via [`Self::with_pool`]) so cached groups across the
	/// whole process share one memory budget.
	pub pool: cache::Pool,
}

impl Default for Info {
	/// An unknown origin (id `0`, no loop detection) with an unbounded pool. This is
	/// what a standalone broadcast (no relay origin) inherits.
	fn default() -> Self {
		Self {
			id: Origin::UNKNOWN,
			pool: cache::Pool::default(),
		}
	}
}

impl Info {
	/// Config for the given origin id with an unbounded cache pool.
	pub fn new(id: Origin) -> Self {
		Self {
			id,
			pool: cache::Pool::default(),
		}
	}

	/// Set the cache pool this origin's broadcasts inherit, returning `self` for chaining.
	pub fn with_pool(mut self, pool: cache::Pool) -> Self {
		self.pool = pool;
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

// If there are multiple broadcasts with the same path, we keep the oldest active and queue the others.
struct OriginBroadcast {
	path: PathOwned,
	active: broadcast::Consumer,
	backup: VecDeque<broadcast::Consumer>,
}

/// Ordering key used to pick the active route among broadcasts at the same path.
///
/// Lower wins. Shorter hop chains sort first (routing prefers the shortest path);
/// remaining ties break on a deterministic hash of the broadcast name and hop
/// chain. Every node in the cluster, given the same candidate routes, converges
/// on the same winner: the hops are forwarded unchanged, and the hash is
/// build-stable. Mixing the name in spreads equal routes across different
/// upstreams rather than funneling onto one.
fn route_key(name: &Path, info: &broadcast::Info) -> (usize, u64) {
	// FNV-1a, not the std hasher: its output is fixed across Rust versions and
	// builds, which matters when nodes run mismatched binaries during a rolling
	// deploy and still need to agree on the same route. SEED is a custom basis
	// (any nonzero u64 works, the textbook one is just as arbitrary); FNV_PRIME is
	// the standard FNV-64 prime and should stay put.
	const SEED: u64 = 0x420C0DECB00B; // 420 C0DEC B00B
	const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

	let mut hash = SEED;
	for &byte in name.as_str().as_bytes() {
		hash = (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME);
	}
	for hop in &info.hops {
		for &byte in &hop.id().to_le_bytes() {
			hash = (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME);
		}
	}

	(info.hops.len(), hash)
}

/// One coalesced update queued for an `AnnounceConsumer`.
///
/// At most one entry exists per path, so a slow consumer's pending set is bounded
/// by the number of distinct paths. `UnannounceAnnounce` preserves the signal
/// that a broadcast genuinely went away and a different one took its place (the
/// consumer must see [`Announced::Ended`] before [`Announced::Active`]), while a
/// stale `Announce` cancels with a subsequent `unannounce` because the consumer
/// has not yet observed it. `Restart` is the atomic replacement: the broadcast
/// at the path never became unavailable, so it collapses to a single
/// [`Announced::Restart`] delivery.
enum PendingUpdate {
	Announce(broadcast::Consumer),
	Unannounce,
	UnannounceAnnounce(broadcast::Consumer),
	Restart(broadcast::Consumer),
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
			// A restart the consumer hasn't drained yet; just swap in the newer broadcast.
			Some(PendingUpdate::Restart(_)) => PendingUpdate::Restart(broadcast),
		};
		self.pending.insert(path, new);
	}

	fn apply_restart(&mut self, path: PathOwned, broadcast: broadcast::Consumer) {
		let new = match self.pending.remove(&path) {
			// Consumer has already drained the prior active; replace it atomically.
			None => PendingUpdate::Restart(broadcast),
			// Consumer hasn't seen the original announce yet; keep it a fresh announce.
			Some(PendingUpdate::Announce(_)) => PendingUpdate::Announce(broadcast),
			// Consumer saw the original active; collapse everything into one restart.
			Some(PendingUpdate::Unannounce | PendingUpdate::UnannounceAnnounce(_) | PendingUpdate::Restart(_)) => {
				PendingUpdate::Restart(broadcast)
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
			// The embedded/replacement announce cancels with this unannounce; the
			// consumer still needs the leading unannounce.
			Some(PendingUpdate::UnannounceAnnounce(_) | PendingUpdate::Restart(_)) => {
				self.pending.insert(path, PendingUpdate::Unannounce);
			}
		}
	}

	/// Take one update to deliver to the consumer, if any.
	fn take(&mut self) -> Option<OriginAnnounce> {
		let path = self.pending.keys().next()?.clone();
		Some(match self.pending.remove(&path).unwrap() {
			PendingUpdate::Announce(broadcast) => (path, Announced::Active(broadcast)),
			PendingUpdate::Unannounce => (path, Announced::Ended),
			PendingUpdate::UnannounceAnnounce(broadcast) => {
				// Deliver the unannounce now; leave the trailing announce pending so
				// the next take returns it for the same path.
				self.pending.insert(path.clone(), PendingUpdate::Announce(broadcast));
				(path, Announced::Ended)
			}
			PendingUpdate::Restart(broadcast) => (path, Announced::Restart(broadcast)),
		})
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

	fn restart(&self, path: impl AsPath, broadcast: broadcast::Consumer) {
		let path = path.as_path().strip_prefix(&self.root).unwrap().to_owned();
		self.state
			.write()
			.ok()
			.expect("consumer closed")
			.apply_restart(path, broadcast);
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

	fn restart(&mut self, path: impl AsPath, broadcast: &broadcast::Consumer) {
		for consumer in self.consumers.values() {
			consumer.restart(path.as_path(), broadcast.clone());
		}

		if let Some(parent) = &self.parent {
			parent.lock().restart(path, broadcast);
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

struct OriginNode {
	// The broadcast that is published to this node.
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

	fn publish(&mut self, full: impl AsPath, broadcast: &broadcast::Consumer, relative: impl AsPath) {
		let full = full.as_path();
		let rest = relative.as_path();

		// If the path has a directory component, then publish it to the nested node.
		if let Some((dir, relative)) = rest.next_part() {
			// Not using entry to avoid allocating a string most of the time.
			self.entry(dir).lock().publish(&full, broadcast, &relative);
		} else if let Some(existing) = &mut self.broadcast {
			// This node is a leaf with an existing broadcast. Prefer the route with the
			// lower ordering key (shorter hop chain, deterministic hash on ties), so every
			// node converges on the same route regardless of the order announces arrive.
			//
			// Drop duplicates (same underlying broadcast delivered via multiple links) so the
			// backup queue can't accumulate clones of the active entry and trigger redundant
			// re-announces when a peer churns.
			if existing.active.is_clone(broadcast) || existing.backup.iter().any(|b| b.is_clone(broadcast)) {
				return;
			}

			if route_key(&full, broadcast.info()) < route_key(&full, existing.active.info()) {
				let old = existing.active.clone();
				existing.active = broadcast.clone();
				existing.backup.push_back(old);

				self.notify.lock().restart(full, broadcast);
			} else {
				// Loses the ordering (longer path, or the tie-break): keep as a backup
				// in case the active one drops.
				existing.backup.push_back(broadcast.clone());
			}
		} else {
			// This node is a leaf with no existing broadcast.
			self.broadcast = Some(OriginBroadcast {
				path: full.to_owned(),
				active: broadcast.clone(),
				backup: VecDeque::new(),
			});
			self.notify.lock().announce(full, broadcast);
		}
	}

	fn consume(&mut self, id: ConsumerId, mut notify: AnnounceConsumerNotify) {
		self.consume_initial(&mut notify);
		self.notify.lock().consumers.insert(id, notify);
	}

	fn consume_initial(&mut self, notify: &mut AnnounceConsumerNotify) {
		if let Some(broadcast) = &self.broadcast {
			notify.announce(&broadcast.path, broadcast.active.clone());
		}

		// Recursively subscribe to all nested nodes.
		for nested in self.nested.values() {
			nested.lock().consume_initial(notify);
		}
	}

	fn consume_broadcast(&self, rest: impl AsPath) -> Option<broadcast::Consumer> {
		let rest = rest.as_path();

		if let Some((dir, rest)) = rest.next_part() {
			let node = self.nested.get(dir)?.lock();
			node.consume_broadcast(&rest)
		} else {
			self.broadcast.as_ref().map(|b| b.active.clone())
		}
	}

	fn unconsume(&mut self, id: ConsumerId) {
		self.notify.lock().consumers.remove(&id).expect("consumer not found");
		if self.is_empty() {
			//tracing::warn!("TODO: empty node; memory leak");
			// This happens when consuming a path that is not being broadcasted.
		}
	}

	// Returns true if the broadcast should be unannounced.
	fn remove(&mut self, full: impl AsPath, broadcast: broadcast::Consumer, relative: impl AsPath) {
		let full = full.as_path();
		let relative = relative.as_path();

		if let Some((dir, relative)) = relative.next_part() {
			let nested = self.entry(dir);
			let mut locked = nested.lock();
			locked.remove(&full, broadcast, &relative);

			if locked.is_empty() {
				drop(locked);
				self.nested.remove(dir);
			}
		} else {
			let entry = match &mut self.broadcast {
				Some(existing) => existing,
				None => return,
			};

			// See if we can remove the broadcast from the backup list.
			let pos = entry.backup.iter().position(|b| b.is_clone(&broadcast));
			if let Some(pos) = pos {
				entry.backup.remove(pos);
				// Nothing else to do
				return;
			}

			// Okay so it must be the active broadcast or else we fucked up.
			assert!(entry.active.is_clone(&broadcast));

			// Promote the backup with the lowest ordering key, the same rule used when
			// publishing, so the route a node heals to still matches its peers.
			let best = entry
				.backup
				.iter()
				.enumerate()
				.min_by_key(|(_, b)| route_key(&full, b.info()))
				.map(|(i, _)| i);
			if let Some(idx) = best {
				let active = entry.backup.remove(idx).expect("index in range");
				entry.active = active;
				self.notify.lock().restart(full, &entry.active);
			} else {
				// No more backups, so remove the entry.
				self.broadcast = None;
				self.notify.lock().unannounce(full);
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

/// A path and what happened to the broadcast there, delivered by [`AnnounceConsumer`].
pub type OriginAnnounce = (PathOwned, Announced);

/// What happened to a broadcast at a path.
#[derive(Clone)]
pub enum Announced {
	/// A broadcast became available.
	Active(broadcast::Consumer),
	/// The broadcast was replaced without an interruption in availability (e.g. a
	/// relay failover or a shorter hop path arriving).
	///
	/// Carries the replacement broadcast. On the wire this is a duplicate ANNOUNCE
	/// (an active announcement for a path that is already announced, with no
	/// intervening unannounce); there is no distinct status byte.
	Restart(broadcast::Consumer),
	/// The broadcast is no longer available.
	Ended,
}

impl Announced {
	/// The broadcast consumer, or `None` if the broadcast ended.
	///
	/// Both [`Active`](Self::Active) and [`Restart`](Self::Restart) carry a
	/// broadcast; [`Ended`](Self::Ended) does not. This is the legacy
	/// `Option<broadcast::Consumer>` view for callers that don't distinguish a fresh
	/// announce from a restart.
	pub fn broadcast(self) -> Option<broadcast::Consumer> {
		match self {
			Self::Active(broadcast) | Self::Restart(broadcast) => Some(broadcast),
			Self::Ended => None,
		}
	}
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
	dynamic: kio::Producer<OriginDynamicState>,

	// The cache pool inherited by broadcasts created under this origin (sessions
	// mint their remote broadcasts with it). Unbounded by default.
	pool: cache::Pool,
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
			dynamic: kio::Producer::default(),
			pool: info.pool,
		}
	}

	/// This origin's [`Info`] (identity + cache pool), the parent handle a broadcast
	/// created under this origin carries (see [`broadcast::Info::origin`]).
	pub fn info(&self) -> Info {
		Info {
			id: self.info,
			pool: self.pool.clone(),
		}
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
			dynamic: kio::Producer::default(),
			pool: cache::Pool::default(),
		}
	}

	/// Create and publish a new broadcast.
	///
	/// This is a helper method when you only want to publish a broadcast to a single origin.
	/// The returned [`Broadcast`] derefs to a [`broadcast::Producer`]; dropping it
	/// unannounces the broadcast. See [`publish_broadcast`](Self::publish_broadcast) for the
	/// error cases.
	pub fn create_broadcast(&self, path: impl AsPath) -> Result<Broadcast, Error> {
		let producer = broadcast::Info {
			origin: self.info(),
			..Default::default()
		}
		.produce();
		let publish = self.publish_broadcast(path, &producer)?;
		Ok(Broadcast { producer, publish })
	}

	/// Publish a broadcast, announcing it to all consumers.
	///
	/// Returns an [`Publish`] guard that keeps the broadcast announced. Drop it (or call
	/// [`Publish::unannounce`]) to remove the broadcast. The announcement is independent
	/// of the broadcast's own lifetime, so dropping the guard unannounces even while the
	/// [`broadcast::Producer`] keeps serving tracks.
	///
	/// Fails with [`Error::Unauthorized`] if `path` is outside the prefixes this producer may
	/// publish under (after [`scope`](Self::scope) / [`with_root`](Self::with_root)). A full-scope
	/// producer (the default from [`Origin::produce`]) never fails. Fails with
	/// [`Error::BoundsExceeded`] if the full rooted path exceeds [`Path::MAX_PARTS`]. Callers must
	/// not publish a broadcast whose hop chain already contains this origin's id (it would form a
	/// routing loop); relays filter such reflections before they reach here, checked by a
	/// `debug_assert`.
	///
	/// If there is already a broadcast with the same path, the new one replaces the active only
	/// if it has a shorter hop path, or an equal-length path that wins a deterministic tie-break
	/// (a hash of the broadcast name and hop chain); otherwise it is queued as a backup. The
	/// tie-break is identical on every node, so a cluster converges on the same route.
	/// When the active guard is dropped, the backup that wins the same ordering is promoted and
	/// reannounced. Backups whose guards drop before being promoted are silently removed.
	#[must_use = "the broadcast is unannounced as soon as the returned guard is dropped"]
	pub fn publish_broadcast(
		&self,
		path: impl AsPath,
		broadcast: impl Consume<broadcast::Consumer>,
	) -> Result<Publish, Error> {
		let broadcast = broadcast.consume();
		let path = path.as_path();

		// Callers must filter reflections (a hop chain already containing our id) before publishing;
		// relays do this on the announce path. Re-announcing one here would form a routing loop.
		debug_assert!(
			!broadcast.info().hops.contains(&self.info),
			"publish_broadcast called with a looping hop chain",
		);

		let (node, rest) = self.nodes.get(&path).ok_or(Error::Unauthorized)?;
		let full = self.root.join(&path).to_owned();

		// A decoded announce prefix and suffix are each within the wire limit, but their
		// join might not be. Enforcing here bounds the tree depth and guarantees the path
		// can be re-encoded when forwarded.
		if full.parts().count() > Path::MAX_PARTS {
			return Err(BoundsExceeded.into());
		}

		node.lock().publish(&full, &broadcast, &rest);
		Ok(Publish {
			node,
			full,
			rest,
			broadcast: Some(broadcast),
		})
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
		Consumer::new(self.info, self.root.clone(), self.nodes.clone(), self.dynamic.consume())
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

/// Keeps a broadcast announced in the origin tree.
///
/// Returned by [`Producer::publish_broadcast`]. While held, the broadcast stays
/// announced to all consumers. Dropping it (or calling [`Self::unannounce`]) removes the
/// broadcast from the tree: the origin promotes the best remaining backup route (emitting a
/// restart) or, if none remain, unannounces the path. The guard is independent of the
/// broadcast's own lifetime, so it can outlive or be dropped before the broadcast itself.
#[must_use = "the broadcast is unannounced as soon as this guard is dropped"]
pub struct Publish {
	node: Lock<OriginNode>,
	full: PathOwned,
	rest: PathOwned,
	// `Option` so `Drop` can take ownership to hand the consumer to `remove`.
	broadcast: Option<broadcast::Consumer>,
}

impl Publish {
	/// Unannounce the broadcast. Equivalent to dropping the guard, but spells out the intent.
	pub fn unannounce(self) {}
}

impl Drop for Publish {
	fn drop(&mut self) {
		if let Some(broadcast) = self.broadcast.take() {
			self.node.lock().remove(&self.full, broadcast, &self.rest);
		}
	}
}

/// A [`broadcast::Producer`] paired with its [`Publish`] announcement guard.
///
/// Returned by [`Producer::create_broadcast`]. Derefs to the underlying
/// [`broadcast::Producer`] so you can add tracks directly; dropping it unannounces the broadcast.
pub struct Broadcast {
	producer: broadcast::Producer,
	// Held only for its Drop, which unannounces the broadcast.
	#[allow(dead_code)]
	publish: Publish,
}

impl Broadcast {
	/// Stop announcing the broadcast but keep producing into it, returning the bare producer.
	pub fn unannounce(self) -> broadcast::Producer {
		self.producer
	}
}

impl std::ops::Deref for Broadcast {
	type Target = broadcast::Producer;

	fn deref(&self) -> &Self::Target {
		&self.producer
	}
}

impl std::ops::DerefMut for Broadcast {
	fn deref_mut(&mut self) -> &mut Self::Target {
		&mut self.producer
	}
}

/// Shared fallback request queue for an origin.
///
/// Lives off to the side of the announce tree because dynamically served broadcasts
/// are never announced. Mirrors the `dynamic`/`requests`/`request_order` fields of the
/// broadcast and track models.
#[derive(Default)]
struct OriginDynamicState {
	// Result channels for queued requests, keyed by absolute path. Concurrent
	// `request_broadcast` calls for the same path coalesce onto the same channel while
	// it is queued. The producer is moved out (and the entry removed) when the handler
	// picks the request up via [`Dynamic::requested_broadcast`].
	requests: HashMap<PathOwned, kio::Producer<PendingBroadcast>>,

	// Requested paths in FIFO order for the handler to drain.
	request_order: VecDeque<PathOwned>,

	// Broadcasts a handler has already served, kept weakly so a repeat request for the
	// same path resolves to a shared clone instead of re-invoking the handler (which would
	// open a duplicate upstream subscription). Weak so a served broadcast still closes once
	// its real consumers drop; a stale (closed) entry is evicted lazily on the next request.
	served: HashMap<PathOwned, broadcast::WeakConsumer>,

	// The number of live `Dynamic` handlers. While zero, `request_broadcast`
	// fails fast with `Unroutable` rather than queueing a request nobody will serve.
	dynamic: usize,
}

impl OriginDynamicState {
	/// Drop every queued request, closing its result channel so awaiting requesters
	/// resolve to an error. Called when the last handler goes away.
	fn reject_requests(&mut self) {
		self.requests.clear();
		self.request_order.clear();
	}
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
	state: kio::Producer<OriginDynamicState>,
}

impl Clone for Dynamic {
	fn clone(&self) -> Self {
		// Mirror `new`: bump `dynamic` so each live handle is counted. Without this,
		// dropping a clone would decrement past `new`'s increment and prematurely flip
		// `dynamic` to zero, making future `request_broadcast` calls return `Unroutable`.
		if let Ok(mut state) = self.state.write() {
			state.dynamic += 1;
		}

		Self {
			info: self.info,
			root: self.root.clone(),
			state: self.state.clone(),
		}
	}
}

impl Dynamic {
	fn new(info: Origin, root: PathOwned, state: kio::Producer<OriginDynamicState>) -> Self {
		if let Ok(mut state) = state.write() {
			state.dynamic += 1;
		}

		Self { info, root, state }
	}

	/// The origin this handler belongs to.
	pub fn info(&self) -> &Origin {
		&self.info
	}

	// Gate readiness on a queued request; mutate through the returned `Mut`.
	fn poll<F>(&self, waiter: &kio::Waiter, f: F) -> Poll<Result<kio::Mut<'_, OriginDynamicState>, Error>>
	where
		F: FnMut(&kio::Ref<'_, OriginDynamicState>) -> Poll<()>,
	{
		Poll::Ready(match ready!(self.state.poll(waiter, f)) {
			Ok(state) => Ok(state),
			Err(_) => Err(Error::Dropped),
		})
	}

	/// Poll for the next requested broadcast, without blocking.
	pub fn poll_requested_broadcast(&mut self, waiter: &kio::Waiter) -> Poll<Result<Request, Error>> {
		let mut state = ready!(self.poll(waiter, |state| {
			if state.request_order.is_empty() {
				Poll::Pending
			} else {
				Poll::Ready(())
			}
		}))?;

		let path = state.request_order.pop_front().expect("predicate guaranteed a request");
		// Leave the request in `requests` (only drain it from `request_order`) so a repeat
		// request in the window between hand-off and accept coalesces onto it instead of
		// re-invoking the handler. The producer is a shared clone; `Request::{accept, reject,
		// drop}` removes the entry. This mirrors how `poll_requested_track` keeps a served
		// track discoverable via the weak cache across the same window.
		let producer = state.requests.get(&path).expect("request_order out of sync").clone();
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
		if let Ok(mut state) = self.state.write() {
			// Saturating sub so `Producer::dynamic` can stay infallible.
			state.dynamic = state.dynamic.saturating_sub(1);
			if state.dynamic == 0 {
				// No handlers left to fulfill queued requests; close them.
				state.reject_requests();
			}
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
	state: kio::Producer<OriginDynamicState>,
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
		// (and subscribe upstream) again.
		if let Ok(mut state) = self.state.write() {
			state.served.insert(self.path.clone(), broadcast.weak());
			state.requests.remove(&self.path);
		}

		if let Ok(mut pending) = self.producer.write() {
			pending.resolved = Some(Ok(broadcast));
		}
		// `self.producer` drops here, closing the channel; the value is still observable.
	}

	/// Reject the request, resolving every awaiting requester with `err`.
	pub fn reject(self, err: Error) {
		if let Ok(mut state) = self.state.write() {
			state.requests.remove(&self.path);
		}
		if let Ok(mut state) = self.producer.write() {
			state.resolved = Some(Err(err));
		}
	}
}

impl Drop for Request {
	fn drop(&mut self) {
		// Handed off but neither accepted nor rejected: drop the still-queued entry so its
		// producer clone (plus this one) closes the channel, resolving coalesced requesters to
		// `Unroutable` rather than hanging.
		//
		// Guard on channel identity: `accept`/`reject` already removed our entry and released the
		// lock before we run, so a concurrent request for the same path may have registered a
		// *new* one here. Removing unconditionally would clobber it (stranding its requesters and
		// desyncing `request_order` from `requests`), so only remove while it's still ours.
		if let Ok(mut state) = self.state.write()
			&& state
				.requests
				.get(&self.path)
				.is_some_and(|producer| producer.same_channel(&self.producer))
		{
			state.requests.remove(&self.path);
		}
	}
}

/// The pollable result of [`Consumer::request_broadcast`].
///
/// Awaited via the [`kio::Pending`] wrapper; resolves to the [`broadcast::Consumer`]
/// immediately when the broadcast was already announced, or once an [`Dynamic`]
/// handler serves the request. Resolves to an error if the request is rejected or every
/// handler drops before serving it.
pub struct Requested {
	inner: RequestState,
}

enum RequestState {
	// Already announced: resolves immediately with a clone of this broadcast.
	Ready(broadcast::Consumer),
	// Unroutable at request time, or the origin was already dropped: resolves immediately
	// with this error. Baked in so `request_broadcast` itself stays infallible.
	Failed(Error),
	// Awaiting a handler: resolves when the request's result channel is written.
	Pending(kio::Consumer<PendingBroadcast>),
}

impl Requested {
	fn ready(broadcast: broadcast::Consumer) -> Self {
		Self {
			inner: RequestState::Ready(broadcast),
		}
	}

	fn failed(error: Error) -> Self {
		Self {
			inner: RequestState::Failed(error),
		}
	}

	fn pending(consumer: kio::Consumer<PendingBroadcast>) -> Self {
		Self {
			inner: RequestState::Pending(consumer),
		}
	}

	/// Poll for the requested broadcast without blocking.
	pub fn poll_ok(&self, waiter: &kio::Waiter) -> Poll<Result<broadcast::Consumer, Error>> {
		match &self.inner {
			RequestState::Ready(broadcast) => Poll::Ready(Ok(broadcast.clone())),
			RequestState::Failed(error) => Poll::Ready(Err(error.clone())),
			RequestState::Pending(consumer) => Poll::Ready(
				match ready!(consumer.poll(waiter, |state| match &state.resolved {
					Some(result) => Poll::Ready(result.clone()),
					None => Poll::Pending,
				})) {
					Ok(result) => result,
					// Every handler dropped without resolving: nobody could route it.
					Err(_closed) => Err(Error::Unroutable),
				},
			),
		}
	}
}

impl kio::Future for Requested {
	type Output = Result<broadcast::Consumer, Error>;

	fn poll(&self, waiter: &kio::Waiter) -> Poll<Self::Output> {
		self.poll_ok(waiter)
	}
}

/// Cheap read handle over an origin's broadcast tree.
///
/// Derive a read view from a handle.
///
/// Lets APIs accept either a producer or a consumer (e.g.
/// [`Client::with_publisher`](crate::Client::with_publisher),
/// [`Producer::publish_broadcast`]). The blanket `&T` impl means you can
/// pass by value (`foo(x)`) to hand off ownership, or by reference (`foo(&x)`)
/// to keep it, without spelling out `.consume()`.
pub trait Consume<T> {
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
		// inherent-vs-trait `consume` ambiguity.
		Consumer::new(self.info, self.root.clone(), self.nodes.clone(), self.dynamic.consume())
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
	dynamic: kio::Consumer<OriginDynamicState>,
}

impl std::ops::Deref for Consumer {
	type Target = Origin;

	fn deref(&self) -> &Self::Target {
		&self.info
	}
}

impl Consumer {
	fn new(info: Origin, root: PathOwned, nodes: OriginNodes, dynamic: kio::Consumer<OriginDynamicState>) -> Self {
		Self {
			info,
			nodes,
			root,
			dynamic,
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
		}
	}

	/// Subscribe to announce / unannounce events for this consumer's subtree.
	///
	/// Allocates a per-cursor coalescing buffer, registers it with each root
	/// in this consumer's scope, and replays the currently active broadcast
	/// set as initial announcements. Drop the returned [`AnnounceConsumer`]
	/// to unregister.
	pub fn announced(&self) -> AnnounceConsumer {
		AnnounceConsumer::new(self.root.clone(), self.nodes.clone())
	}

	/// Returns a cheap duplicate of this read handle.
	pub fn consume(&self) -> Self {
		self.clone()
	}

	/// Internal synchronous peek: the broadcast at `path` if it is *already* announced.
	///
	/// Races announcement gossip (a freshly-connected consumer sees `None` even when the
	/// broadcast is about to arrive), so it is not public. [`Self::request_broadcast`] is the
	/// public lookup: it builds on this for the announced case, then falls back to a dynamic
	/// handler. [`Self::announced_broadcast`] waits for a future announcement.
	fn get_broadcast(&self, path: impl AsPath) -> Option<broadcast::Consumer> {
		let path = path.as_path();
		let (root, rest) = self.nodes.get(&path)?;
		let state = root.lock();
		state.consume_broadcast(&rest)
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

		let mut announced = consumer.announced();
		loop {
			let (announced_path, event) = announced.next().await?;
			// `scope` narrows by prefix, but we only want an exact-path match.
			if announced_path.as_path() == path {
				if let Some(broadcast) = event.broadcast() {
					return Some(broadcast);
				}
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
		Some(Consumer::new(
			self.info,
			self.root.clone(),
			self.nodes.select(&prefixes)?,
			self.dynamic.clone(),
		))
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
	/// dynamic handler exists, or [`Error::Dropped`] once the origin is gone. A request that is
	/// registered while a handler is live but then loses every handler before being served also
	/// resolves to [`Error::Unroutable`]. Unlike an announced broadcast, a dynamically served one
	/// is never visible to [`Self::announced`].
	pub fn request_broadcast(&self, path: impl AsPath) -> kio::Pending<Requested> {
		let path = path.as_path();

		// Prefer a live announcement when one is present; the dynamic queue is only a fallback.
		if let Some(broadcast) = self.get_broadcast(&path) {
			return kio::Pending::new(Requested::ready(broadcast));
		}

		// Key requests by absolute path so a scoped/rooted consumer and the handler
		// (which may have a different root) agree on the same entry.
		let absolute = self.root.join(&path).to_owned();

		let Ok(mut state) = self.dynamic.write() else {
			return kio::Pending::new(Requested::failed(Error::Dropped));
		};

		// Reuse a still-live broadcast a handler already served for this path, so repeat
		// requests share one upstream subscription. A closed entry is stale; drop it and
		// re-serve below.
		if let Some(weak) = state.served.get(&absolute) {
			if !weak.is_closed() {
				return kio::Pending::new(Requested::ready(weak.consume()));
			}
			state.served.remove(&absolute);
		}

		// Coalesce onto a queued request for the same path; otherwise register a new one.
		let consumer = if let Some(producer) = state.requests.get(&absolute) {
			producer.consume()
		} else {
			if state.dynamic == 0 {
				return kio::Pending::new(Requested::failed(Error::Unroutable));
			}

			let producer = kio::Producer::<PendingBroadcast>::default();
			let consumer = producer.consume();
			state.requests.insert(absolute.clone(), producer);
			state.request_order.push_back(absolute);
			consumer
		};

		kio::Pending::new(Requested::pending(consumer))
	}

	/// Returns a new Consumer that automatically strips out the provided prefix.
	///
	/// Returns None if the provided root is not authorized; when [`Self::scope`] was
	/// already used without a wildcard.
	pub fn with_root(&self, prefix: impl AsPath) -> Option<Self> {
		let prefix = prefix.as_path();

		Some(Self::new(
			self.info,
			self.root.join(&prefix).to_owned(),
			self.nodes.root(&prefix)?,
			self.dynamic.clone(),
		))
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
		AnnounceConsumer::new(self.root.clone(), self.nodes.clone())
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
}

impl AnnounceConsumer {
	fn new(root: PathOwned, nodes: OriginNodes) -> Self {
		let state = kio::Producer::<OriginConsumerState>::default();
		let id = ConsumerId::new();

		for (_, node) in &nodes.nodes {
			let notify = AnnounceConsumerNotify {
				root: root.clone(),
				state: state.clone(),
			};
			node.lock().consume(id, notify);
		}

		Self { id, nodes, root, state }
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
		Poll::Ready(Some(state.take().expect("predicate guaranteed an update")))
	}

	/// Returns the next (un)announced broadcast without blocking.
	///
	/// Returns None if there is no update available; NOT because the cursor is closed.
	/// Use [`Self::is_closed`] to check if the cursor is closed.
	pub fn try_next(&mut self) -> Option<OriginAnnounce> {
		self.state.write().ok()?.take()
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
impl AnnounceConsumer {
	pub fn assert_next(&mut self, expected: impl AsPath, broadcast: &broadcast::Consumer) {
		let expected = expected.as_path();
		let (path, event) = self.next().now_or_never().expect("next blocked").expect("no next");
		assert!(matches!(event, Announced::Active(_)), "should be an active announce");
		assert_eq!(path, expected, "wrong path");
		assert!(
			event.broadcast().unwrap().is_clone(broadcast),
			"should be the same broadcast"
		);
	}

	pub fn assert_next_restart(&mut self, expected: impl AsPath, broadcast: &broadcast::Consumer) {
		let expected = expected.as_path();
		let (path, event) = self.next().now_or_never().expect("next blocked").expect("no next");
		assert!(matches!(event, Announced::Restart(_)), "should be a restart");
		assert_eq!(path, expected, "wrong path");
		assert!(
			event.broadcast().unwrap().is_clone(broadcast),
			"should be the same broadcast"
		);
	}

	pub fn assert_try_next(&mut self, expected: impl AsPath, broadcast: &broadcast::Consumer) {
		let expected = expected.as_path();
		let (path, event) = self.try_next().expect("no next");
		assert_eq!(path, expected, "wrong path");
		assert!(
			event.broadcast().unwrap().is_clone(broadcast),
			"should be the same broadcast"
		);
	}

	pub fn assert_next_none(&mut self, expected: impl AsPath) {
		let expected = expected.as_path();
		let (path, event) = self.next().now_or_never().expect("next blocked").expect("no next");
		assert_eq!(path, expected, "wrong path");
		assert!(event.broadcast().is_none(), "should be unannounced");
	}

	pub fn assert_next_wait(&mut self) {
		if let Some(res) = self.next().now_or_never() {
			panic!("next should block: got {:?}", res.map(|(path, _)| path));
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
impl Producer {
	/// Test helper that reproduces the legacy fire-and-forget contract: publish, then
	/// auto-unannounce when the broadcast closes (every producer dropped) via a spawned
	/// watcher that drops the [`Publish`] guard. Returns whether the publish was
	/// accepted. Exercises [`Publish`]'s `Drop` on the close path.
	fn publish_broadcast_spawn(&self, path: impl AsPath, broadcast: broadcast::Consumer) -> bool {
		match self.publish_broadcast(path, &broadcast) {
			Ok(publish) => {
				web_async::spawn(async move {
					broadcast.closed().await;
					drop(publish);
				});
				true
			}
			Err(_) => false,
		}
	}
}

#[cfg(test)]
mod tests {
	use crate::coding::Decode;

	use super::*;

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
		let broadcast1 = broadcast::Info::new().produce();
		let broadcast2 = broadcast::Info::new().produce();

		let mut consumer1 = origin.consume().announced();
		// Make a new consumer that should get it.
		consumer1.assert_next_wait();

		// Publish the first broadcast.
		origin.publish_broadcast_spawn("test1", broadcast1.consume());

		consumer1.assert_next("test1", &broadcast1.consume());
		consumer1.assert_next_wait();

		// Make a new consumer that should get the existing broadcast.
		// But we don't consume it yet.
		let mut consumer2 = origin.consume().announced();

		// Publish the second broadcast.
		origin.publish_broadcast_spawn("test2", broadcast2.consume());

		consumer1.assert_next("test2", &broadcast2.consume());
		consumer1.assert_next_wait();

		consumer2.assert_next("test1", &broadcast1.consume());
		consumer2.assert_next("test2", &broadcast2.consume());
		consumer2.assert_next_wait();

		// Close the first broadcast.
		drop(broadcast1);

		// Wait for the async task to run.
		tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;

		// All consumers should get a None now.
		consumer1.assert_next_none("test1");
		consumer2.assert_next_none("test1");
		consumer1.assert_next_wait();
		consumer2.assert_next_wait();

		// And a new consumer only gets the last broadcast.
		let mut consumer3 = origin.consume().announced();
		consumer3.assert_next("test2", &broadcast2.consume());
		consumer3.assert_next_wait();

		// Close the other producer and make sure it cleans up
		drop(broadcast2);

		// Wait for the async task to run.
		tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;

		consumer1.assert_next_none("test2");
		consumer2.assert_next_none("test2");
		consumer3.assert_next_none("test2");

		/* TODO close the origin consumer when the producer is dropped
		consumer1.assert_next_closed();
		consumer2.assert_next_closed();
		consumer3.assert_next_closed();
		*/
	}

	#[tokio::test]
	async fn test_duplicate() {
		tokio::time::pause();

		let origin = Origin::random().produce();

		let broadcast1 = broadcast::Info::new().produce();
		let broadcast2 = broadcast::Info::new().produce();
		let broadcast3 = broadcast::Info::new().produce();

		let consumer1 = broadcast1.consume();
		let consumer2 = broadcast2.consume();
		let consumer3 = broadcast3.consume();

		let consumer = origin.consume();
		let mut announced = consumer.announced();

		origin.publish_broadcast_spawn("test", consumer1.clone());
		origin.publish_broadcast_spawn("test", consumer2.clone());
		origin.publish_broadcast_spawn("test", consumer3.clone());
		assert!(consumer.get_broadcast("test").is_some());

		// Identical (empty) hop chains tie on the deterministic key, so the first publish
		// stays active and the rest queue as backups. No churn, no restart.
		announced.assert_next("test", &consumer1);
		announced.assert_next_wait();

		// Drop a backup, nothing should change.
		drop(broadcast2);

		// Wait for the async task to run.
		tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;

		assert!(consumer.get_broadcast("test").is_some());
		announced.assert_next_wait();

		// Drop the active, we should restart with the remaining backup.
		drop(broadcast1);

		// Wait for the async task to run.
		tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;

		assert!(consumer.get_broadcast("test").is_some());
		announced.assert_next_restart("test", &consumer3);

		// Drop the final broadcast, we should unannounce.
		drop(broadcast3);

		// Wait for the async task to run.
		tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
		assert!(consumer.get_broadcast("test").is_none());

		announced.assert_next_none("test");
		announced.assert_next_wait();
	}

	#[tokio::test]
	async fn test_restart_after_drain() {
		// A strictly-better route (shorter hop chain) replacing the active broadcast
		// after the consumer has drained the original announce is delivered as a
		// single atomic restart.
		let origin = Origin::random().produce();
		// `a` carries one hop; `b` has none, so `b` wins the route and replaces it.
		let a = broadcast::Info {
			hops: OriginList::try_from(vec![Origin::new(1u64).unwrap()]).unwrap(),
			..Default::default()
		}
		.produce();
		let b = broadcast::Info::new().produce();

		let mut announced = origin.consume().announced();
		origin.publish_broadcast_spawn("test", a.consume());
		announced.assert_next("test", &a.consume());

		origin.publish_broadcast_spawn("test", b.consume());
		announced.assert_next_restart("test", &b.consume());
		announced.assert_next_wait();
	}

	#[tokio::test]
	async fn test_restart_undrained_stays_active() {
		// If the consumer hasn't observed the original announce yet, a winning route
		// just swaps in the newer broadcast and is still delivered as a fresh Active.
		let origin = Origin::random().produce();
		// `a` carries one hop; `b` has none, so `b` wins the route and replaces it.
		let a = broadcast::Info {
			hops: OriginList::try_from(vec![Origin::new(1u64).unwrap()]).unwrap(),
			..Default::default()
		}
		.produce();
		let b = broadcast::Info::new().produce();

		let mut announced = origin.consume().announced();
		origin.publish_broadcast_spawn("test", a.consume());
		origin.publish_broadcast_spawn("test", b.consume());

		announced.assert_next("test", &b.consume());
		announced.assert_next_wait();
	}

	#[tokio::test]
	async fn test_duplicate_reverse() {
		tokio::time::pause();

		let origin = Origin::random().produce();
		let broadcast1 = broadcast::Info::new().produce();
		let broadcast2 = broadcast::Info::new().produce();

		origin.publish_broadcast_spawn("test", broadcast1.consume());
		origin.publish_broadcast_spawn("test", broadcast2.consume());
		assert!(origin.consume().get_broadcast("test").is_some());

		// This is harder, dropping the new broadcast first.
		drop(broadcast2);

		// Wait for the cleanup async task to run.
		tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
		assert!(origin.consume().get_broadcast("test").is_some());

		drop(broadcast1);

		// Wait for the cleanup async task to run.
		tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
		assert!(origin.consume().get_broadcast("test").is_none());
	}

	#[tokio::test]
	async fn test_deterministic_tiebreak() {
		tokio::time::pause();

		fn route(ids: &[u64]) -> broadcast::Producer {
			let hops = OriginList::try_from(
				ids.iter()
					.copied()
					.map(|id| Origin::new(id).unwrap())
					.collect::<Vec<_>>(),
			)
			.unwrap();
			broadcast::Info {
				hops,
				..Default::default()
			}
			.produce()
		}

		// Resolve the active route for "test" after publishing both routes in the given order.
		fn winner(first: &[u64], second: &[u64]) -> OriginList {
			let origin = Origin::random().produce();
			let a = route(first);
			let b = route(second);
			origin.publish_broadcast_spawn("test", a.consume());
			origin.publish_broadcast_spawn("test", b.consume());
			let hops = origin.consume().get_broadcast("test").unwrap().info().hops.clone();
			// Keep the producers alive until after we read the active route.
			drop((a, b));
			hops
		}

		// Two routes with equal hop counts but distinct chains. The winner is decided by
		// the deterministic key, not arrival order, so both publish orders converge.
		let forward = winner(&[10, 20], &[30, 40]);
		let reverse = winner(&[30, 40], &[10, 20]);
		assert_eq!(forward, reverse, "tie-break must not depend on publish order");

		// A strictly shorter chain always wins regardless of the hash.
		assert_eq!(winner(&[10, 20], &[30]).len(), 1);
		assert_eq!(winner(&[30], &[10, 20]).len(), 1);
	}

	#[tokio::test]
	async fn test_double_publish() {
		tokio::time::pause();

		let origin = Origin::random().produce();
		let broadcast = broadcast::Info::new().produce();

		// Ensure it doesn't crash.
		origin.publish_broadcast_spawn("test", broadcast.consume());
		origin.publish_broadcast_spawn("test", broadcast.consume());

		assert!(origin.consume().get_broadcast("test").is_some());

		drop(broadcast);

		// Wait for the async task to run.
		tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
		assert!(origin.consume().get_broadcast("test").is_none());
	}
	// A previous mpsc-based implementation could only deliver the first 127 broadcasts
	// instantly via `assert_next` (which uses `now_or_never`). The kio-backed
	// implementation polls synchronously and can deliver all of them without yielding.
	// Names are zero-padded so lexicographic delivery order matches the loop index.
	#[tokio::test]
	async fn test_many_announces() {
		let origin = Origin::random().produce();
		let broadcast = broadcast::Info::new().produce();

		let mut consumer = origin.consume().announced();
		for i in 0..256 {
			origin.publish_broadcast_spawn(format!("test{i:03}"), broadcast.consume());
		}

		for i in 0..256 {
			consumer.assert_next(format!("test{i:03}"), &broadcast.consume());
		}
		consumer.assert_next_wait();
	}

	#[tokio::test]
	async fn test_many_announces_try() {
		let origin = Origin::random().produce();
		let broadcast = broadcast::Info::new().produce();

		let mut consumer = origin.consume().announced();
		for i in 0..256 {
			origin.publish_broadcast_spawn(format!("test{i:03}"), broadcast.consume());
		}

		for i in 0..256 {
			consumer.assert_try_next(format!("test{i:03}"), &broadcast.consume());
		}
	}

	#[tokio::test]
	async fn test_with_root_basic() {
		let origin = Origin::random().produce();
		let broadcast = broadcast::Info::new().produce();

		// Create a producer with root "/foo"
		let foo_producer = origin.with_root("foo").expect("should create root");
		assert_eq!(foo_producer.root().as_str(), "foo");

		let mut consumer = origin.consume().announced();

		// When publishing to "bar/baz", it should actually publish to "foo/bar/baz"
		assert!(foo_producer.publish_broadcast_spawn("bar/baz", broadcast.consume()));
		// The original consumer should see the full path
		consumer.assert_next("foo/bar/baz", &broadcast.consume());

		// A consumer created from the rooted producer should see the stripped path
		let mut foo_consumer = foo_producer.consume().announced();
		foo_consumer.assert_next("bar/baz", &broadcast.consume());
	}

	#[tokio::test]
	async fn test_with_root_nested() {
		let origin = Origin::random().produce();
		let broadcast = broadcast::Info::new().produce();

		// Create nested roots
		let foo_producer = origin.with_root("foo").expect("should create foo root");
		let foo_bar_producer = foo_producer.with_root("bar").expect("should create bar root");
		assert_eq!(foo_bar_producer.root().as_str(), "foo/bar");

		let mut consumer = origin.consume().announced();

		// Publishing to "baz" should actually publish to "foo/bar/baz"
		assert!(foo_bar_producer.publish_broadcast_spawn("baz", broadcast.consume()));
		// The original consumer sees the full path
		consumer.assert_next("foo/bar/baz", &broadcast.consume());

		// Consumer from foo_bar_producer sees just "baz"
		let mut foo_bar_consumer = foo_bar_producer.consume().announced();
		foo_bar_consumer.assert_next("baz", &broadcast.consume());
	}

	#[tokio::test]
	async fn test_publish_scope_allows() {
		let origin = Origin::random().produce();
		let broadcast = broadcast::Info::new().produce();

		// Create a producer that can only publish to "allowed" paths
		let limited_producer = origin
			.scope(&["allowed/path1".into(), "allowed/path2".into()])
			.expect("should create limited producer");

		// Should be able to publish to allowed paths
		assert!(limited_producer.publish_broadcast_spawn("allowed/path1", broadcast.consume()));
		assert!(limited_producer.publish_broadcast_spawn("allowed/path1/nested", broadcast.consume()));
		assert!(limited_producer.publish_broadcast_spawn("allowed/path2", broadcast.consume()));

		// Should not be able to publish to disallowed paths
		assert!(!limited_producer.publish_broadcast_spawn("notallowed", broadcast.consume()));
		assert!(!limited_producer.publish_broadcast_spawn("allowed", broadcast.consume())); // Parent of allowed path
		assert!(!limited_producer.publish_broadcast_spawn("other/path", broadcast.consume()));
	}

	#[tokio::test]
	async fn test_publish_max_parts() {
		let origin = Origin::random().produce();
		let broadcast = broadcast::Info::new().produce();

		let at_limit = (0..Path::MAX_PARTS)
			.map(|i| i.to_string())
			.collect::<Vec<_>>()
			.join("/");
		assert!(origin.publish_broadcast_spawn(at_limit.as_str(), broadcast.consume()));

		let too_deep = format!("{at_limit}/extra");
		assert!(!origin.publish_broadcast_spawn(too_deep.as_str(), broadcast.consume()));

		// The root counts toward the limit; a joined path past 32 parts is rejected.
		let rooted = origin.with_root("root").expect("wildcard allows any root");
		assert!(!rooted.publish_broadcast_spawn(at_limit.as_str(), broadcast.consume()));
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
		let broadcast1 = broadcast::Info::new().produce();
		let broadcast2 = broadcast::Info::new().produce();
		let broadcast3 = broadcast::Info::new().produce();

		let mut consumer = origin.consume().announced();

		// Publish to different paths
		origin.publish_broadcast_spawn("allowed", broadcast1.consume());
		origin.publish_broadcast_spawn("allowed/nested", broadcast2.consume());
		origin.publish_broadcast_spawn("notallowed", broadcast3.consume());

		// Create a consumer that only sees "allowed" paths
		let mut limited_consumer = origin
			.consume()
			.scope(&["allowed".into()])
			.expect("should create limited consumer")
			.announced();

		// Should only receive broadcasts under "allowed"
		limited_consumer.assert_next("allowed", &broadcast1.consume());
		limited_consumer.assert_next("allowed/nested", &broadcast2.consume());
		limited_consumer.assert_next_wait(); // Should not see "notallowed"

		// Unscoped consumer should see all
		consumer.assert_next("allowed", &broadcast1.consume());
		consumer.assert_next("allowed/nested", &broadcast2.consume());
		consumer.assert_next("notallowed", &broadcast3.consume());
	}

	#[tokio::test]
	async fn test_consume_scope_multiple_prefixes() {
		let origin = Origin::random().produce();
		let broadcast1 = broadcast::Info::new().produce();
		let broadcast2 = broadcast::Info::new().produce();
		let broadcast3 = broadcast::Info::new().produce();

		origin.publish_broadcast_spawn("foo/test", broadcast1.consume());
		origin.publish_broadcast_spawn("bar/test", broadcast2.consume());
		origin.publish_broadcast_spawn("baz/test", broadcast3.consume());

		// Consumer that only sees "foo" and "bar" paths
		let mut limited_consumer = origin
			.consume()
			.scope(&["foo".into(), "bar".into()])
			.expect("should create limited consumer")
			.announced();

		// Order depends on PathPrefixes canonical sort (lexicographic for same length)
		limited_consumer.assert_next("bar/test", &broadcast2.consume());
		limited_consumer.assert_next("foo/test", &broadcast1.consume());
		limited_consumer.assert_next_wait(); // Should not see "baz/test"
	}

	#[tokio::test]
	async fn test_with_root_and_publish_scope() {
		let origin = Origin::random().produce();
		let broadcast = broadcast::Info::new().produce();

		// User connects to /foo root
		let foo_producer = origin.with_root("foo").expect("should create foo root");

		// Limit them to publish only to "bar" and "goop/pee" within /foo
		let limited_producer = foo_producer
			.scope(&["bar".into(), "goop/pee".into()])
			.expect("should create limited producer");

		let mut consumer = origin.consume().announced();

		// Should be able to publish to foo/bar and foo/goop/pee (but user sees as bar and goop/pee)
		assert!(limited_producer.publish_broadcast_spawn("bar", broadcast.consume()));
		assert!(limited_producer.publish_broadcast_spawn("bar/nested", broadcast.consume()));
		assert!(limited_producer.publish_broadcast_spawn("goop/pee", broadcast.consume()));
		assert!(limited_producer.publish_broadcast_spawn("goop/pee/nested", broadcast.consume()));

		// Should not be able to publish outside allowed paths
		assert!(!limited_producer.publish_broadcast_spawn("baz", broadcast.consume()));
		assert!(!limited_producer.publish_broadcast_spawn("goop", broadcast.consume())); // Parent of allowed
		assert!(!limited_producer.publish_broadcast_spawn("goop/other", broadcast.consume()));

		// Original consumer sees full paths
		consumer.assert_next("foo/bar", &broadcast.consume());
		consumer.assert_next("foo/bar/nested", &broadcast.consume());
		consumer.assert_next("foo/goop/pee", &broadcast.consume());
		consumer.assert_next("foo/goop/pee/nested", &broadcast.consume());
	}

	#[tokio::test]
	async fn test_with_root_and_consume_scope() {
		let origin = Origin::random().produce();
		let broadcast1 = broadcast::Info::new().produce();
		let broadcast2 = broadcast::Info::new().produce();
		let broadcast3 = broadcast::Info::new().produce();

		// Publish broadcasts
		origin.publish_broadcast_spawn("foo/bar/test", broadcast1.consume());
		origin.publish_broadcast_spawn("foo/goop/pee/test", broadcast2.consume());
		origin.publish_broadcast_spawn("foo/other/test", broadcast3.consume());

		// User connects to /foo root
		let foo_producer = origin.with_root("foo").expect("should create foo root");

		// Create consumer limited to "bar" and "goop/pee" within /foo
		let mut limited_consumer = foo_producer
			.consume()
			.scope(&["bar".into(), "goop/pee".into()])
			.expect("should create limited consumer")
			.announced();

		// Should only see allowed paths (without foo prefix)
		limited_consumer.assert_next("bar/test", &broadcast1.consume());
		limited_consumer.assert_next("goop/pee/test", &broadcast2.consume());
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
		let broadcast = broadcast::Info::new().produce();

		// Producer with root access (empty string means wildcard)
		let root_producer = origin.clone();

		// Should be able to publish anywhere
		assert!(root_producer.publish_broadcast_spawn("any/path", broadcast.consume()));
		assert!(root_producer.publish_broadcast_spawn("other/path", broadcast.consume()));

		// Can create any root
		let foo_producer = root_producer.with_root("foo").expect("should create any root");
		assert_eq!(foo_producer.root().as_str(), "foo");
	}

	#[tokio::test]
	async fn test_consume_broadcast_with_permissions() {
		let origin = Origin::random().produce();
		let broadcast1 = broadcast::Info::new().produce();
		let broadcast2 = broadcast::Info::new().produce();

		origin.publish_broadcast_spawn("allowed/test", broadcast1.consume());
		origin.publish_broadcast_spawn("notallowed/test", broadcast2.consume());

		// Create limited consumer
		let limited_consumer = origin
			.consume()
			.scope(&["allowed".into()])
			.expect("should create limited consumer");

		// Should be able to get allowed broadcast
		let result = limited_consumer.get_broadcast("allowed/test");
		assert!(result.is_some());
		assert!(result.unwrap().is_clone(&broadcast1.consume()));

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
		let broadcast = broadcast::Info::new().produce();

		// Create producer limited to "a/b/c"
		let limited_producer = origin.scope(&["a/b/c".into()]).expect("should create limited producer");

		// Should be able to publish to exact path and nested paths
		assert!(limited_producer.publish_broadcast_spawn("a/b/c", broadcast.consume()));
		assert!(limited_producer.publish_broadcast_spawn("a/b/c/d", broadcast.consume()));
		assert!(limited_producer.publish_broadcast_spawn("a/b/c/d/e", broadcast.consume()));

		// Should not be able to publish to parent or sibling paths
		assert!(!limited_producer.publish_broadcast_spawn("a", broadcast.consume()));
		assert!(!limited_producer.publish_broadcast_spawn("a/b", broadcast.consume()));
		assert!(!limited_producer.publish_broadcast_spawn("a/b/other", broadcast.consume()));
	}

	#[tokio::test]
	async fn test_multiple_consumers_with_different_permissions() {
		let origin = Origin::random().produce();
		let broadcast1 = broadcast::Info::new().produce();
		let broadcast2 = broadcast::Info::new().produce();
		let broadcast3 = broadcast::Info::new().produce();

		// Publish to different paths
		origin.publish_broadcast_spawn("foo/test", broadcast1.consume());
		origin.publish_broadcast_spawn("bar/test", broadcast2.consume());
		origin.publish_broadcast_spawn("baz/test", broadcast3.consume());

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
		foo_consumer.assert_next("foo/test", &broadcast1.consume());
		foo_consumer.assert_next_wait();

		bar_consumer.assert_next("bar/test", &broadcast2.consume());
		bar_consumer.assert_next_wait();

		foobar_consumer.assert_next("bar/test", &broadcast2.consume());
		foobar_consumer.assert_next("foo/test", &broadcast1.consume());
		foobar_consumer.assert_next_wait();
	}

	#[tokio::test]
	async fn test_select_with_empty_prefix() {
		let origin = Origin::random().produce();
		let broadcast1 = broadcast::Info::new().produce();
		let broadcast2 = broadcast::Info::new().produce();

		// User with root "demo" allowed to subscribe to "worm-node" and "foobar"
		let demo_producer = origin.with_root("demo").expect("should create demo root");
		let limited_producer = demo_producer
			.scope(&["worm-node".into(), "foobar".into()])
			.expect("should create limited producer");

		// Publish some broadcasts
		assert!(limited_producer.publish_broadcast_spawn("worm-node/test", broadcast1.consume()));
		assert!(limited_producer.publish_broadcast_spawn("foobar/test", broadcast2.consume()));

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

		let mut paths: Vec<_> = [&a1, &a2].iter().map(|(p, _)| p.to_string()).collect();
		paths.sort();
		assert_eq!(paths, ["foobar/test", "worm-node/test"]);
	}

	#[tokio::test]
	async fn test_select_narrowing_scope() {
		let origin = Origin::random().produce();
		let broadcast1 = broadcast::Info::new().produce();
		let broadcast2 = broadcast::Info::new().produce();
		let broadcast3 = broadcast::Info::new().produce();

		// User with root "demo" allowed to subscribe to "worm-node" and "foobar"
		let demo_producer = origin.with_root("demo").expect("should create demo root");
		let limited_producer = demo_producer
			.scope(&["worm-node".into(), "foobar".into()])
			.expect("should create limited producer");

		// Publish broadcasts at different levels
		assert!(limited_producer.publish_broadcast_spawn("worm-node", broadcast1.consume()));
		assert!(limited_producer.publish_broadcast_spawn("worm-node/foo", broadcast2.consume()));
		assert!(limited_producer.publish_broadcast_spawn("foobar/bar", broadcast3.consume()));

		// Test 1: scope("worm-node") should result in a single "" node with contents of "worm-node" ONLY
		let mut worm_consumer = limited_producer
			.consume()
			.scope(&["worm-node".into()])
			.expect("should create worm-node consumer")
			.announced();

		// Should see worm-node content with paths stripped to ""
		worm_consumer.assert_next("worm-node", &broadcast1.consume());
		worm_consumer.assert_next("worm-node/foo", &broadcast2.consume());
		worm_consumer.assert_next_wait(); // Should NOT see foobar content

		// Test 2: scope("worm-node/foo") should result in a "" node with contents of "worm-node/foo"
		let mut foo_consumer = limited_producer
			.consume()
			.scope(&["worm-node/foo".into()])
			.expect("should create worm-node/foo consumer")
			.announced();

		foo_consumer.assert_next("worm-node/foo", &broadcast2.consume());
		foo_consumer.assert_next_wait(); // Should NOT see other content
	}

	#[tokio::test]
	async fn test_select_multiple_roots_with_empty_prefix() {
		let origin = Origin::random().produce();
		let broadcast1 = broadcast::Info::new().produce();
		let broadcast2 = broadcast::Info::new().produce();
		let broadcast3 = broadcast::Info::new().produce();

		// Producer with multiple allowed roots
		let limited_producer = origin
			.scope(&["app1".into(), "app2".into(), "shared".into()])
			.expect("should create limited producer");

		// Publish to each root
		assert!(limited_producer.publish_broadcast_spawn("app1/data", broadcast1.consume()));
		assert!(limited_producer.publish_broadcast_spawn("app2/config", broadcast2.consume()));
		assert!(limited_producer.publish_broadcast_spawn("shared/resource", broadcast3.consume()));

		// scope with empty prefix should maintain all roots
		let mut consumer = limited_producer
			.consume()
			.scope(&["".into()])
			.expect("should create consumer with empty prefix")
			.announced();

		// Should see all broadcasts from all roots
		consumer.assert_next("app1/data", &broadcast1.consume());
		consumer.assert_next("app2/config", &broadcast2.consume());
		consumer.assert_next("shared/resource", &broadcast3.consume());
		consumer.assert_next_wait();
	}

	#[tokio::test]
	async fn test_publish_scope_with_empty_prefix() {
		let origin = Origin::random().produce();
		let broadcast = broadcast::Info::new().produce();

		// Producer with specific allowed paths
		let limited_producer = origin
			.scope(&["services/api".into(), "services/web".into()])
			.expect("should create limited producer");

		// scope with empty prefix should keep the same restrictions
		let same_producer = limited_producer
			.scope(&["".into()])
			.expect("should create producer with empty prefix");

		// Should still have the same publishing restrictions
		assert!(same_producer.publish_broadcast_spawn("services/api", broadcast.consume()));
		assert!(same_producer.publish_broadcast_spawn("services/web", broadcast.consume()));
		assert!(!same_producer.publish_broadcast_spawn("services/db", broadcast.consume()));
		assert!(!same_producer.publish_broadcast_spawn("other", broadcast.consume()));
	}

	#[tokio::test]
	async fn test_select_narrowing_to_deeper_path() {
		let origin = Origin::random().produce();
		let broadcast1 = broadcast::Info::new().produce();
		let broadcast2 = broadcast::Info::new().produce();
		let broadcast3 = broadcast::Info::new().produce();

		// Producer with broad permission
		let limited_producer = origin.scope(&["org".into()]).expect("should create limited producer");

		// Publish at various depths
		assert!(limited_producer.publish_broadcast_spawn("org/team1/project1", broadcast1.consume()));
		assert!(limited_producer.publish_broadcast_spawn("org/team1/project2", broadcast2.consume()));
		assert!(limited_producer.publish_broadcast_spawn("org/team2/project1", broadcast3.consume()));

		// Narrow down to team2 only
		let mut team2_consumer = limited_producer
			.consume()
			.scope(&["org/team2".into()])
			.expect("should create team2 consumer")
			.announced();

		team2_consumer.assert_next("org/team2/project1", &broadcast3.consume());
		team2_consumer.assert_next_wait(); // Should NOT see team1 content

		// Further narrow down to team1/project1
		let mut project1_consumer = limited_producer
			.consume()
			.scope(&["org/team1/project1".into()])
			.expect("should create project1 consumer")
			.announced();

		// Should only see project1 content at root
		project1_consumer.assert_next("org/team1/project1", &broadcast1.consume());
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

		let b = origin.create_broadcast("some_prefix/test").unwrap();
		consumer.assert_next("test", &b.consume());
	}

	// Same issue but for the producer side of with_root
	#[tokio::test]
	async fn test_with_root_trailing_slash_producer() {
		let origin = Origin::random().produce();

		// Use an owned String so the trailing slash is NOT normalized away.
		let prefix = "some_prefix/".to_string();
		let rooted = origin.with_root(prefix).unwrap();

		let b = rooted.create_broadcast("test").unwrap();

		let mut consumer = rooted.consume().announced();
		consumer.assert_next("test", &b.consume());
	}

	// Verify unannounce also doesn't panic with trailing slash
	#[tokio::test]
	async fn test_with_root_trailing_slash_unannounce() {
		tokio::time::pause();

		let origin = Origin::random().produce();

		let prefix = "some_prefix/".to_string();
		let mut consumer = origin.consume().with_root(prefix).unwrap().announced();

		let b = origin.create_broadcast("some_prefix/test").unwrap();
		consumer.assert_next("test", &b.consume());

		// Drop the broadcast producer to trigger unannounce
		drop(b);
		tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;

		// unannounce also calls strip_prefix(&self.root).unwrap()
		consumer.assert_next_none("test");
	}

	#[tokio::test]
	async fn test_select_maintains_access_with_wider_prefix() {
		let origin = Origin::random().produce();
		let broadcast1 = broadcast::Info::new().produce();
		let broadcast2 = broadcast::Info::new().produce();

		// Setup: user with root "demo" allowed to subscribe to specific paths
		let demo_producer = origin.with_root("demo").expect("should create demo root");
		let user_producer = demo_producer
			.scope(&["worm-node".into(), "foobar".into()])
			.expect("should create user producer");

		// Publish some data
		assert!(user_producer.publish_broadcast_spawn("worm-node/data", broadcast1.consume()));
		assert!(user_producer.publish_broadcast_spawn("foobar", broadcast2.consume()));

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

		let mut paths: Vec<_> = [&a1, &a2].iter().map(|(p, _)| p.to_string()).collect();
		paths.sort();
		assert_eq!(paths, ["foobar", "worm-node/data"]);

		// Also test that we can still narrow the scope
		let mut narrow_consumer = user_producer
			.consume()
			.scope(&["worm-node".into()])
			.expect("should be able to narrow scope to worm-node")
			.announced();

		narrow_consumer.assert_next("worm-node/data", &broadcast1.consume());
		narrow_consumer.assert_next_wait(); // Should not see foobar
	}

	#[tokio::test]
	async fn test_duplicate_prefixes_deduped() {
		let origin = Origin::random().produce();
		let broadcast = broadcast::Info::new().produce();

		// scope with duplicate prefixes should work (deduped internally)
		let producer = origin
			.scope(&["demo".into(), "demo".into()])
			.expect("should create producer");

		assert!(producer.publish_broadcast_spawn("demo/stream", broadcast.consume()));

		let mut consumer = producer.consume().announced();
		consumer.assert_next("demo/stream", &broadcast.consume());
		consumer.assert_next_wait();
	}

	#[tokio::test]
	async fn test_overlapping_prefixes_deduped() {
		let origin = Origin::random().produce();
		let broadcast = broadcast::Info::new().produce();

		// "demo" and "demo/foo". "demo/foo" is redundant, only "demo" should remain
		let producer = origin
			.scope(&["demo".into(), "demo/foo".into()])
			.expect("should create producer");

		// Can still publish under "demo/bar" since "demo" covers everything
		assert!(producer.publish_broadcast_spawn("demo/bar/stream", broadcast.consume()));

		let mut consumer = producer.consume().announced();
		consumer.assert_next("demo/bar/stream", &broadcast.consume());
		consumer.assert_next_wait();
	}

	#[tokio::test]
	async fn test_overlapping_prefixes_no_duplicate_announcements() {
		let origin = Origin::random().produce();
		let broadcast = broadcast::Info::new().produce();

		// Both "demo" and "demo/foo" are requested. Should only have one node
		let producer = origin
			.scope(&["demo".into(), "demo/foo".into()])
			.expect("should create producer");

		assert!(producer.publish_broadcast_spawn("demo/foo/stream", broadcast.consume()));

		let mut consumer = producer.consume().announced();
		// Should only get ONE announcement (not two from overlapping nodes)
		consumer.assert_next("demo/foo/stream", &broadcast.consume());
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
		let broadcast = broadcast::Info::new().produce();

		origin.publish_broadcast_spawn("test", broadcast.consume());

		let consumer = origin.consume();
		let result = consumer.announced_broadcast("test").await.expect("should find it");
		assert!(result.is_clone(&broadcast.consume()));
	}

	#[tokio::test]
	async fn test_announced_broadcast_delayed() {
		tokio::time::pause();

		let origin = Origin::random().produce();
		let broadcast = broadcast::Info::new().produce();

		let consumer = origin.consume();

		// Start waiting before it's announced.
		let wait = tokio::spawn({
			let consumer = consumer.clone();
			async move { consumer.announced_broadcast("test").await }
		});

		// Give the spawned task a chance to subscribe.
		tokio::task::yield_now().await;

		origin.publish_broadcast_spawn("test", broadcast.consume());

		let result = wait.await.unwrap().expect("should find it");
		assert!(result.is_clone(&broadcast.consume()));
	}

	#[tokio::test]
	async fn test_announced_broadcast_ignores_unrelated_paths() {
		tokio::time::pause();

		let origin = Origin::random().produce();
		let other = broadcast::Info::new().produce();
		let target = broadcast::Info::new().produce();

		let consumer = origin.consume();

		let wait = tokio::spawn({
			let consumer = consumer.clone();
			async move { consumer.announced_broadcast("target").await }
		});

		tokio::task::yield_now().await;

		// Publish an unrelated broadcast first. announced_broadcast should skip it.
		origin.publish_broadcast_spawn("other", other.consume());
		tokio::task::yield_now().await;
		assert!(!wait.is_finished(), "must not resolve on unrelated path");

		origin.publish_broadcast_spawn("target", target.consume());
		let result = wait.await.unwrap().expect("should find target");
		assert!(result.is_clone(&target.consume()));
	}

	#[tokio::test]
	async fn test_announced_broadcast_skips_nested_paths() {
		tokio::time::pause();

		let origin = Origin::random().produce();
		let nested = broadcast::Info::new().produce();
		let exact = broadcast::Info::new().produce();

		let consumer = origin.consume();

		let wait = tokio::spawn({
			let consumer = consumer.clone();
			async move { consumer.announced_broadcast("foo").await }
		});

		tokio::task::yield_now().await;

		// "foo/bar" is under the prefix scope, but it's not the exact path. Skip it.
		origin.publish_broadcast_spawn("foo/bar", nested.consume());
		tokio::task::yield_now().await;
		assert!(!wait.is_finished(), "must not resolve on a nested path");

		origin.publish_broadcast_spawn("foo", exact.consume());
		let result = wait.await.unwrap().expect("should find foo exactly");
		assert!(result.is_clone(&exact.consume()));
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

		let broadcast = broadcast::Info::new().produce();
		origin.publish_broadcast_spawn("test", broadcast.consume());
		drop(broadcast);

		tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;

		announced.assert_next_wait();
	}

	#[tokio::test]
	async fn test_coalesce_announce_unannounce_announce() {
		// announce, unannounce, announce that the cursor hasn't drained collapses
		// to a single Announce of the latest broadcast.
		tokio::time::pause();

		let origin = Origin::random().produce();
		let mut announced = origin.consume().announced();

		let broadcast1 = broadcast::Info::new().produce();
		let broadcast2 = broadcast::Info::new().produce();

		origin.publish_broadcast_spawn("test", broadcast1.consume());
		drop(broadcast1);
		tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
		origin.publish_broadcast_spawn("test", broadcast2.consume());

		announced.assert_next("test", &broadcast2.consume());
		announced.assert_next_wait();
	}

	#[tokio::test]
	async fn test_coalesce_unannounce_announce_preserved() {
		// unannounce followed by announce of a different broadcast must be preserved
		// as two deliveries so the cursor learns the origin changed.
		tokio::time::pause();

		let origin = Origin::random().produce();
		let broadcast1 = broadcast::Info::new().produce();
		origin.publish_broadcast_spawn("test", broadcast1.consume());

		let mut announced = origin.consume().announced();
		announced.assert_next("test", &broadcast1.consume());

		// Drop, then publish a fresh broadcast at the same path.
		drop(broadcast1);
		tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;

		let broadcast2 = broadcast::Info::new().produce();
		origin.publish_broadcast_spawn("test", broadcast2.consume());

		// The cursor must see the unannounce before the new announce.
		announced.assert_next_none("test");
		announced.assert_next("test", &broadcast2.consume());
		announced.assert_next_wait();
	}

	#[tokio::test]
	async fn test_coalesce_unannounce_announce_unannounce() {
		// unannounce + announce + unannounce collapses to a single unannounce: the
		// embedded announce was never observed.
		tokio::time::pause();

		let origin = Origin::random().produce();
		let broadcast1 = broadcast::Info::new().produce();
		origin.publish_broadcast_spawn("test", broadcast1.consume());

		let mut announced = origin.consume().announced();
		announced.assert_next("test", &broadcast1.consume());

		drop(broadcast1);
		tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;

		let broadcast2 = broadcast::Info::new().produce();
		origin.publish_broadcast_spawn("test", broadcast2.consume());
		drop(broadcast2);
		tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;

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
			let broadcast = broadcast::Info::new().produce();
			origin.publish_broadcast_spawn("test", broadcast.consume());
			drop(broadcast);
		}
		tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;

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
			collected.iter().all(|(path, _)| path == &Path::new("test")),
			"unexpected path in pending updates",
		);
	}

	// Consumer should be cheap to clone: cloning must NOT drain any
	// other cursor's announce channel. A freshly-built AnnounceConsumer
	// still receives the active backlog.
	#[tokio::test]
	async fn test_consumer_clone_is_side_effect_free() {
		let origin = Origin::random().produce();
		let broadcast1 = broadcast::Info::new().produce();
		let broadcast2 = broadcast::Info::new().produce();

		origin.publish_broadcast_spawn("test1", broadcast1.consume());
		origin.publish_broadcast_spawn("test2", broadcast2.consume());

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

		let mut paths: Vec<_> = [&a1, &a2].iter().map(|(p, _)| p.to_string()).collect();
		paths.sort();
		assert_eq!(paths, ["test1", "test2"]);

		// A freshly-built AnnounceConsumer still receives the active backlog.
		let mut fresh = consumer.announced();
		let b1 = fresh.try_next().expect("backlog: first");
		let b2 = fresh.try_next().expect("backlog: second");
		fresh.assert_next_wait();

		let mut paths: Vec<_> = [&b1, &b2].iter().map(|(p, _)| p.to_string()).collect();
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

		// Request a path that nobody announced; the future stays pending until served.
		// Registration happens up front, so the handler sees the request immediately.
		let request_fut = consumer.request_broadcast("fallback");

		// The handler serves it with a live broadcast it keeps producing into.
		let served = broadcast::Info::new().produce();
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
		let served = broadcast::Info::new().produce();
		let request = dynamic.requested_broadcast().await.unwrap();
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
		let served = broadcast::Info::new().produce();
		let request = dynamic.requested_broadcast().await.unwrap();
		request.accept(&served);
		request_fut.await.unwrap();

		// Close the first served broadcast; the weak cache entry goes stale.
		drop(served);

		// A fresh request must reach the handler again and resolve to the new broadcast.
		let request_fut = consumer.request_broadcast("fallback");
		let served = broadcast::Info::new().produce();
		let request = dynamic.requested_broadcast().await.unwrap();
		assert_eq!(request.path(), &Path::new("fallback"));
		request.accept(&served);
		assert!(request_fut.await.unwrap().is_clone(&served.consume()));
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
	// the rejected `Request`'s removal + `Drop` leave `requests` and `request_order` consistent
	// (a stale/clobbered entry would strand this request or panic the handler).
	#[tokio::test(start_paused = true)]
	async fn dynamic_request_rerequest_after_reject() {
		let origin = Origin::random().produce();
		let mut dynamic = origin.dynamic();
		let consumer = origin.consume();

		let f1 = consumer.request_broadcast("fallback");
		dynamic.requested_broadcast().await.unwrap().reject(Error::Unroutable);
		assert!(matches!(f1.await, Err(Error::Unroutable)));

		// A fresh request re-reaches the handler and can be served.
		let f2 = consumer.request_broadcast("fallback");
		let request = dynamic.requested_broadcast().await.unwrap();
		assert_eq!(request.path(), &Path::new("fallback"));
		let served = broadcast::Info::new().produce();
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

		// Accept still resolves the awaiting requester with the served broadcast.
		let served = broadcast::Info::new().produce();
		request.accept(&served);
		assert!(request_fut.await.unwrap().is_clone(&served.consume()));
	}

	// A live announcement wins over the dynamic fallback; no request is queued.
	#[tokio::test(start_paused = true)]
	async fn dynamic_request_prefers_announced() {
		let origin = Origin::random().produce();
		let mut dynamic = origin.dynamic();
		let consumer = origin.consume();

		let broadcast = broadcast::Info::new().produce();
		let _publish = origin.publish_broadcast("live", &broadcast).unwrap();

		let got = consumer.request_broadcast("live").await.unwrap();
		assert!(
			got.is_clone(&broadcast.consume()),
			"should return the announced broadcast"
		);
		assert!(
			dynamic.requested_broadcast().now_or_never().is_none(),
			"an announced path must not queue a fallback request"
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
