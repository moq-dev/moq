use std::{
	collections::{HashMap, VecDeque},
	fmt,
	sync::{
		Arc, Mutex,
		atomic::{AtomicU64, Ordering},
	},
	task::Poll,
};

use rand::Rng;

use super::BroadcastConsumer;
use crate::{
	AsPath, Broadcast, BroadcastProducer, Path, PathOwned, PathPrefixes,
	coding::{Decode, DecodeError, Encode, EncodeError},
};

/// A relay origin, identified by a 62-bit varint on the wire.
///
/// `id` must be non-zero for a real origin; `id == 0` is reserved as a
/// placeholder for Lite03-style hops where the actual value isn't carried.
/// Encoding a value outside the 62-bit range (>= 2^62) will fail at the
/// varint layer; [`Origin::random`] picks a valid random nonzero id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Origin {
	pub id: u64,
}

impl Origin {
	/// Placeholder for hop entries whose actual id is not on the wire (Lite03).
	/// Never encoded for Lite04+: violates the non-zero invariant and would fail to round-trip.
	pub(crate) const UNKNOWN: Self = Self { id: 0 };

	/// Generate a fresh origin with a random non-zero 62-bit id. Callers
	/// that need a specific id can build one via [`From<u64>`] instead,
	/// but this is rarely what you want.
	pub fn random() -> Self {
		let mut rng = rand::rng();
		let id = rng.random_range(1..(1u64 << 62));
		Self { id }
	}

	/// Consume this [Origin] to create a producer that carries its id.
	pub fn produce(self) -> OriginProducer {
		OriginProducer::new(self)
	}
}

impl From<u64> for Origin {
	fn from(id: u64) -> Self {
		Self { id }
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
pub const MAX_HOPS: usize = 32;

/// Bounded list of [`Origin`] entries, typically the hop chain of a broadcast.
///
/// Guarantees `len() <= MAX_HOPS`. Construct via [`OriginList::new`] +
/// [`OriginList::push`], or fall back to the fallible [`TryFrom<Vec<Origin>>`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct OriginList(Vec<Origin>);

/// Returned when an operation would grow an [`OriginList`] past [`MAX_HOPS`].
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

	/// Returns true if any entry matches `origin`.
	pub fn contains(&self, origin: &Origin) -> bool {
		self.0.contains(origin)
	}

	pub fn len(&self) -> usize {
		self.0.len()
	}

	pub fn is_empty(&self) -> bool {
		self.0.is_empty()
	}

	pub fn iter(&self) -> std::slice::Iter<'_, Origin> {
		self.0.iter()
	}

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

// === Origin tree ===

/// A single update emitted by [`OriginConsumer::announced`].
///
/// `Some(broadcast)` means the path is now active at that broadcast. `None`
/// means the path is no longer active (either because the active closed and no
/// backup was available, or because a new broadcast is about to take over,
/// which arrives as a follow-up `Some` for the same path).
pub type OriginAnnounce = (PathOwned, Option<BroadcastConsumer>);

static NEXT_CONSUMER_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct ConsumerId(u64);

impl ConsumerId {
	fn new() -> Self {
		Self(NEXT_CONSUMER_ID.fetch_add(1, Ordering::Relaxed))
	}
}

/// A path's currently-best broadcast plus a queue of equally-or-longer-hop
/// backups. The active is whichever broadcast has the shortest hop chain
/// (newer wins ties — see [`OriginProducer::publish`]).
struct Entry {
	active: BroadcastConsumer,
	backup: VecDeque<BroadcastConsumer>,
}

/// Per-consumer state stored in [`State::consumers`].
struct ConsumerQueue {
	pending: VecDeque<OriginAnnounce>,
	scope: Scope,
}

/// Permission/visibility scope shared between an [`OriginProducer`] and its
/// derived [`OriginConsumer`].
///
/// `prefixes` is always non-empty (we return `None` from constructors that
/// would produce an empty scope). A wildcard scope contains the empty path
/// `""` which has every other path as a suffix.
#[derive(Debug, Clone)]
struct Scope {
	root: PathOwned,
	prefixes: PathPrefixes,
}

impl Default for Scope {
	fn default() -> Self {
		Self {
			root: PathOwned::default(),
			prefixes: PathPrefixes::new([""]),
		}
	}
}

impl Scope {
	/// Returns the absolute path for `relative` if it is in scope.
	fn check(&self, relative: impl AsPath) -> Option<PathOwned> {
		let relative = relative.as_path();
		if !self.prefixes.iter().any(|p| relative.has_prefix(p)) {
			return None;
		}
		Some(self.root.join(&relative))
	}

	/// Strips `self.root` from `full` and confirms the remainder lies within
	/// one of `self.prefixes`. Returns the relative path on success.
	fn relativize(&self, full: &Path<'_>) -> Option<PathOwned> {
		let rest = full.strip_prefix(&self.root)?;
		if !self.prefixes.iter().any(|p| rest.has_prefix(p)) {
			return None;
		}
		Some(rest.to_owned())
	}

	/// Narrow the scope to only `requested` prefixes (relative to `self.root`).
	/// Returns `None` if no requested prefix overlaps any current prefix.
	fn narrow_prefixes(&self, requested: &PathPrefixes) -> Option<Self> {
		let mut result: Vec<PathOwned> = Vec::new();
		for existing in &self.prefixes {
			for r in requested {
				if existing.has_prefix(r) {
					// existing is at least as specific as r; keep existing.
					result.push(existing.clone());
				} else if r.strip_prefix(existing).is_some() {
					// r is strictly more specific than existing; narrow to r.
					result.push(r.clone());
				}
			}
		}
		if result.is_empty() {
			return None;
		}
		Some(Self {
			root: self.root.clone(),
			prefixes: PathPrefixes::new(result),
		})
	}

	/// Push `prefix` onto `self.root`, narrowing the prefixes to match.
	fn with_more_root(&self, prefix: impl AsPath) -> Option<Self> {
		let prefix = prefix.as_path();
		if prefix.is_empty() {
			return Some(self.clone());
		}

		let mut new_prefixes: Vec<PathOwned> = Vec::new();
		for p in &self.prefixes {
			if let Some(suffix) = p.strip_prefix(&prefix) {
				// Existing prefix is at least as specific as the new root.
				// The relative prefix is whatever's left after the new root.
				new_prefixes.push(suffix.to_owned());
			} else if prefix.has_prefix(p) {
				// Existing prefix subsumes the new root, so anything under the
				// new root is allowed (wildcard-relative-to-new-root).
				new_prefixes.push(PathOwned::default());
			}
			// else: existing prefix doesn't intersect the new root; drop.
		}

		if new_prefixes.is_empty() {
			return None;
		}

		Some(Self {
			root: self.root.join(&prefix),
			prefixes: PathPrefixes::new(new_prefixes),
		})
	}
}

#[derive(Default)]
struct State {
	paths: HashMap<PathOwned, Entry>,
	consumers: HashMap<ConsumerId, ConsumerQueue>,
	/// Number of live [`OriginProducer`]s. When this hits zero we mark
	/// `closed` and wake everyone so [`OriginConsumer::announced`] returns `None`.
	producers: usize,
	closed: bool,
	waiters: conducer::WaiterList,
}

struct Inner {
	state: Mutex<State>,
}

impl Inner {
	fn lock(&self) -> std::sync::MutexGuard<'_, State> {
		self.state.lock().expect("origin state mutex poisoned")
	}
}

/// Distribute `events` (absolute paths) to every consumer queue, translating
/// each event's path to that consumer's scope-relative form.
fn distribute(state: &mut State, events: &[OriginAnnounce]) {
	for queue in state.consumers.values_mut() {
		for (abs, broadcast) in events {
			let Some(rel) = queue.scope.relativize(abs) else {
				continue;
			};
			queue.pending.push_back((rel, broadcast.clone()));
		}
	}
}

/// Scan every entry; for any whose active broadcast has closed, drain dead
/// backups, promote the shortest-hop survivor (or remove the entry) and
/// distribute the resulting `Ended`/`Active` events.
///
/// Returns true if any events were emitted (caller needs to wake waiters).
fn gc_pass(state: &mut State) -> bool {
	let mut events: Vec<OriginAnnounce> = Vec::new();

	state.paths.retain(|path, entry| {
		if !entry.active.is_closed() {
			return true;
		}
		// Drop dead backups so we don't promote one that's already gone.
		entry.backup.retain(|b| !b.is_closed());

		let best = entry
			.backup
			.iter()
			.enumerate()
			.min_by_key(|(_, b)| b.hops.len())
			.map(|(i, _)| i);

		match best {
			Some(idx) => {
				let new_active = entry.backup.remove(idx).expect("index in range");
				entry.active = new_active.clone();
				events.push((path.clone(), None));
				events.push((path.clone(), Some(new_active)));
				true
			}
			None => {
				events.push((path.clone(), None));
				false
			}
		}
	});

	if events.is_empty() {
		return false;
	}
	distribute(state, &events);
	true
}

// === OriginProducer ===

/// Announces broadcasts to subscribers.
///
/// Cheap to clone (each clone shares state and acts as an additional handle
/// keeping the channel open). Drop the last clone to close the channel —
/// every [`OriginConsumer`] derived from it will see [`OriginConsumer::announced`]
/// return `None`.
pub struct OriginProducer {
	info: Origin,
	inner: Arc<Inner>,
	scope: Scope,
}

impl std::ops::Deref for OriginProducer {
	type Target = Origin;

	fn deref(&self) -> &Self::Target {
		&self.info
	}
}

impl OriginProducer {
	pub fn new(info: Origin) -> Self {
		let state = State {
			producers: 1,
			..Default::default()
		};
		Self {
			info,
			inner: Arc::new(Inner {
				state: Mutex::new(state),
			}),
			scope: Scope::default(),
		}
	}

	/// Publish `broadcast` at `path`. Returns false if the path is outside
	/// this producer's scope or the hop chain already includes our id.
	///
	/// On a duplicate (`is_clone` matches an existing active or backup), the
	/// publish is a no-op. When a new broadcast at a known path has hops ≤
	/// the current active, it replaces the active (the previous one is queued
	/// as a backup); strictly longer hops join the backup queue. When the
	/// active later closes, the shortest-hop backup is promoted automatically.
	pub fn publish_broadcast(&self, path: impl AsPath, broadcast: BroadcastConsumer) -> bool {
		// Loop check: refuse broadcasts whose hop chain already contains us.
		if broadcast.hops.contains(&self.info) {
			return false;
		}

		let Some(full) = self.scope.check(path) else {
			return false;
		};

		let mut state = self.inner.lock();
		let mut events: Vec<OriginAnnounce> = Vec::new();

		match state.paths.entry(full.clone()) {
			std::collections::hash_map::Entry::Vacant(slot) => {
				slot.insert(Entry {
					active: broadcast.clone(),
					backup: VecDeque::new(),
				});
				events.push((full, Some(broadcast)));
			}
			std::collections::hash_map::Entry::Occupied(mut slot) => {
				let entry = slot.get_mut();
				// Drop pure duplicates (same underlying broadcast via different paths).
				if entry.active.is_clone(&broadcast) || entry.backup.iter().any(|b| b.is_clone(&broadcast)) {
					return true;
				}
				if broadcast.hops.len() <= entry.active.hops.len() {
					// New is at least as good; demote old to backup, announce new.
					let old = std::mem::replace(&mut entry.active, broadcast.clone());
					entry.backup.push_back(old);
					events.push((full.clone(), None));
					events.push((full, Some(broadcast)));
				} else {
					// Strictly longer; just stash as a backup.
					entry.backup.push_back(broadcast);
				}
			}
		}

		if events.is_empty() {
			return true;
		}

		distribute(&mut state, &events);
		let mut waiters = state.waiters.take();
		drop(state);
		waiters.wake();
		true
	}

	/// Produce a fresh broadcast and publish it at `path`. Returns `None` if
	/// the path is outside this producer's scope.
	pub fn create_broadcast(&self, path: impl AsPath) -> Option<BroadcastProducer> {
		let producer = Broadcast::new().produce();
		if self.publish_broadcast(path, producer.consume()) {
			Some(producer)
		} else {
			None
		}
	}

	/// Subscribe to all broadcasts within this producer's scope.
	///
	/// The returned consumer immediately enumerates currently-active broadcasts
	/// (replay), then yields future updates as they arrive.
	pub fn consume(&self) -> OriginConsumer {
		OriginConsumer::new(self.info, self.inner.clone(), self.scope.clone())
	}

	/// Restrict authorization to broadcasts under `prefixes` (relative to the
	/// current root). Returns `None` if no prefix overlaps the existing scope.
	pub fn scope(&self, prefixes: &[Path<'_>]) -> Option<Self> {
		let prefixes = PathPrefixes::new(prefixes);
		let scope = self.scope.narrow_prefixes(&prefixes)?;
		Some(self.fork(scope))
	}

	/// Auto-strip `prefix` from all paths used by this producer, narrowing the
	/// allowed prefixes accordingly. Returns `None` if `prefix` is outside the
	/// allowed scope.
	pub fn with_root(&self, prefix: impl AsPath) -> Option<Self> {
		let scope = self.scope.with_more_root(prefix)?;
		Some(self.fork(scope))
	}

	fn fork(&self, scope: Scope) -> Self {
		let mut state = self.inner.lock();
		state.producers += 1;
		drop(state);
		Self {
			info: self.info,
			inner: self.inner.clone(),
			scope,
		}
	}

	pub fn root(&self) -> &Path<'_> {
		&self.scope.root
	}

	pub fn allowed(&self) -> &PathPrefixes {
		&self.scope.prefixes
	}

	/// Convert a relative path to an absolute path (joining with the root).
	pub fn absolute(&self, path: impl AsPath) -> Path<'_> {
		self.scope.root.join(path)
	}
}

impl Clone for OriginProducer {
	fn clone(&self) -> Self {
		self.fork(self.scope.clone())
	}
}

impl Drop for OriginProducer {
	fn drop(&mut self) {
		let mut state = self.inner.lock();
		state.producers -= 1;
		if state.producers > 0 || state.closed {
			return;
		}
		state.closed = true;
		let mut waiters = state.waiters.take();
		drop(state);
		waiters.wake();
	}
}

// === OriginConsumer ===

/// Subscribes to announced broadcasts within a scope.
///
/// `Clone` is supported (cheap; shares state, gets a fresh replay) and
/// `Drop` automatically unregisters the consumer's queue.
pub struct OriginConsumer {
	info: Origin,
	inner: Arc<Inner>,
	id: ConsumerId,
	scope: Scope,
}

impl std::ops::Deref for OriginConsumer {
	type Target = Origin;

	fn deref(&self) -> &Self::Target {
		&self.info
	}
}

impl OriginConsumer {
	fn new(info: Origin, inner: Arc<Inner>, scope: Scope) -> Self {
		let id = ConsumerId::new();
		let mut state = inner.lock();
		// Run GC first so we don't replay actives that have already closed.
		// `gc_pass` distributes Ended events to existing consumers; ours hasn't
		// been registered yet, so it sees only the up-to-date `paths` map below.
		let woke = gc_pass(&mut state);
		let mut queue = ConsumerQueue {
			pending: VecDeque::new(),
			scope: scope.clone(),
		};
		for (path, entry) in &state.paths {
			if let Some(rel) = scope.relativize(path) {
				queue.pending.push_back((rel, Some(entry.active.clone())));
			}
		}
		state.consumers.insert(id, queue);
		let waiters = if woke { Some(state.waiters.take()) } else { None };
		drop(state);
		if let Some(mut w) = waiters {
			w.wake();
		}
		Self { info, inner, id, scope }
	}

	/// Block until the next [`OriginAnnounce`]. Returns `None` once every
	/// [`OriginProducer`] has been dropped and the queue is drained.
	pub async fn announced(&mut self) -> Option<OriginAnnounce> {
		conducer::wait(|waiter| self.poll_announced(waiter)).await
	}

	/// Synchronous variant of [`Self::announced`]. Returns `None` if no update is
	/// pending. Use [`Self::is_closed`] to distinguish from channel closure.
	pub fn try_announced(&mut self) -> Option<OriginAnnounce> {
		let waiter = conducer::Waiter::noop();
		match self.poll_announced(&waiter) {
			Poll::Ready(opt) => opt,
			Poll::Pending => None,
		}
	}

	pub fn poll_announced(&mut self, waiter: &conducer::Waiter) -> Poll<Option<OriginAnnounce>> {
		let mut state = self.inner.lock();
		let mut to_wake: Vec<conducer::WaiterList> = Vec::new();

		loop {
			// Phase 1: garbage-collect any closed actives and emit their Ended events.
			if gc_pass(&mut state) {
				to_wake.push(state.waiters.take());
			}

			// Phase 2: drain our queue.
			let queue = state.consumers.get_mut(&self.id).expect("consumer registered");
			if let Some(event) = queue.pending.pop_front() {
				drop(state);
				for mut w in to_wake {
					w.wake();
				}
				return Poll::Ready(Some(event));
			}

			// Phase 3: closed?
			if state.closed {
				drop(state);
				for mut w in to_wake {
					w.wake();
				}
				return Poll::Ready(None);
			}

			// Phase 4: register our waker globally (publishes / GC will wake us).
			waiter.register(&mut state.waiters);

			// Phase 5: also register on each in-scope active broadcast's
			// `closed()` so that any of them closing wakes us directly. If one
			// already closed between Phase 1 and now, loop back and rerun GC.
			let mut close_detected = false;
			for (path, entry) in &state.paths {
				if self.scope.relativize(path).is_none() {
					continue;
				}
				if entry.active.poll_closed(waiter).is_ready() {
					close_detected = true;
					break;
				}
			}
			if close_detected {
				continue;
			}

			drop(state);
			for mut w in to_wake {
				w.wake();
			}
			return Poll::Pending;
		}
	}

	/// Block until a broadcast at exactly `path` is announced. Returns `None`
	/// if the path is outside this consumer's scope or the channel closes
	/// before the broadcast arrives.
	pub async fn announced_broadcast(&self, path: impl AsPath) -> Option<BroadcastConsumer> {
		let path = path.as_path();

		// Scope down to this exact path so we only wake on relevant changes.
		let mut consumer = self.scope(std::slice::from_ref(&path))?;

		// `scope()` keeps narrower permissions intact: asking for `foo` on a
		// consumer limited to `foo/specific` returns a consumer scoped to
		// `foo/specific`, where the exact path `foo` will never arrive. Bail
		// rather than loop forever.
		if !consumer.allowed().iter().any(|allowed| path.has_prefix(allowed)) {
			return None;
		}

		loop {
			match consumer.announced().await? {
				(p, Some(b)) if p.as_path() == path => return Some(b),
				_ => continue,
			}
		}
	}

	/// True if every [`OriginProducer`] has been dropped.
	pub fn is_closed(&self) -> bool {
		self.inner.lock().closed
	}

	/// Block until [`Self::is_closed`] becomes true.
	pub async fn closed(&self) {
		conducer::wait(|waiter| {
			let mut state = self.inner.lock();
			if state.closed {
				return Poll::Ready(());
			}
			waiter.register(&mut state.waiters);
			Poll::Pending
		})
		.await
	}

	/// Restrict consumption to broadcasts under `prefixes` (relative to the
	/// current root). Returns `None` if no prefix overlaps the existing scope.
	pub fn scope(&self, prefixes: &[Path<'_>]) -> Option<Self> {
		let prefixes = PathPrefixes::new(prefixes);
		let scope = self.scope.narrow_prefixes(&prefixes)?;
		Some(Self::new(self.info, self.inner.clone(), scope))
	}

	/// Auto-strip `prefix` from announcements, narrowing the scope accordingly.
	/// Returns `None` if `prefix` is outside the allowed scope.
	pub fn with_root(&self, prefix: impl AsPath) -> Option<Self> {
		let scope = self.scope.with_more_root(prefix)?;
		Some(Self::new(self.info, self.inner.clone(), scope))
	}

	pub fn root(&self) -> &Path<'_> {
		&self.scope.root
	}

	pub fn allowed(&self) -> &PathPrefixes {
		&self.scope.prefixes
	}

	pub fn absolute(&self, path: impl AsPath) -> Path<'_> {
		self.scope.root.join(path)
	}
}

impl Clone for OriginConsumer {
	fn clone(&self) -> Self {
		Self::new(self.info, self.inner.clone(), self.scope.clone())
	}
}

impl Drop for OriginConsumer {
	fn drop(&mut self) {
		let mut state = self.inner.lock();
		state.consumers.remove(&self.id);
	}
}

#[cfg(test)]
use futures::FutureExt;

#[cfg(test)]
impl OriginConsumer {
	pub fn assert_announced(&mut self, expected: impl AsPath, broadcast: &BroadcastConsumer) {
		let expected = expected.as_path();
		let (p, b) = self
			.announced()
			.now_or_never()
			.expect("announced blocked")
			.expect("no announce");
		match b {
			Some(b) => {
				assert_eq!(p, expected, "wrong path");
				assert!(b.is_clone(broadcast), "should be the same broadcast");
			}
			None => panic!("expected Active({expected}), got Ended({p})"),
		}
	}

	pub fn assert_try_announced(&mut self, expected: impl AsPath, broadcast: &BroadcastConsumer) {
		let expected = expected.as_path();
		let (p, b) = self.try_announced().expect("no announce");
		match b {
			Some(b) => {
				assert_eq!(p, expected, "wrong path");
				assert!(b.is_clone(broadcast), "should be the same broadcast");
			}
			None => panic!("expected Active({expected}), got Ended({p})"),
		}
	}

	pub fn assert_announced_ended(&mut self, expected: impl AsPath) {
		let expected = expected.as_path();
		let (p, b) = self
			.announced()
			.now_or_never()
			.expect("announced blocked")
			.expect("no announce");
		match b {
			None => assert_eq!(p, expected, "wrong path"),
			Some(_) => panic!("expected Ended({expected}), got Active({p})"),
		}
	}

	pub fn assert_announced_wait(&mut self) {
		if self.announced().now_or_never().is_some() {
			panic!("announced should block");
		}
	}
}

#[cfg(test)]
mod tests {
	use crate::Broadcast;

	use super::*;

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
	fn origin_list_try_from_vec_enforces_limit() {
		let under: Vec<Origin> = (0..MAX_HOPS).map(|_| Origin::random()).collect();
		assert!(OriginList::try_from(under).is_ok());

		let over: Vec<Origin> = (0..MAX_HOPS + 1).map(|_| Origin::random()).collect();
		assert_eq!(OriginList::try_from(over), Err(TooManyOrigins));
	}

	#[tokio::test]
	async fn announce_unannounce() {
		let origin = Origin::random().produce();
		let broadcast1 = Broadcast::new().produce();
		let broadcast2 = Broadcast::new().produce();

		let mut consumer1 = origin.consume();
		consumer1.assert_announced_wait();

		assert!(origin.publish_broadcast("test1", broadcast1.consume()));
		consumer1.assert_announced("test1", &broadcast1.consume());
		consumer1.assert_announced_wait();

		// Make a new consumer that should replay the existing broadcast.
		let mut consumer2 = origin.consume();

		assert!(origin.publish_broadcast("test2", broadcast2.consume()));
		consumer1.assert_announced("test2", &broadcast2.consume());
		consumer1.assert_announced_wait();
		consumer2.assert_announced("test1", &broadcast1.consume());
		consumer2.assert_announced("test2", &broadcast2.consume());
		consumer2.assert_announced_wait();

		// Closing the broadcast emits Ended on next poll (no spawn / no sleep).
		drop(broadcast1);
		consumer1.assert_announced_ended("test1");
		consumer2.assert_announced_ended("test1");
		consumer1.assert_announced_wait();
		consumer2.assert_announced_wait();

		// A fresh consumer only replays what's currently active.
		let mut consumer3 = origin.consume();
		consumer3.assert_announced("test2", &broadcast2.consume());
		consumer3.assert_announced_wait();

		drop(broadcast2);
		consumer1.assert_announced_ended("test2");
		consumer2.assert_announced_ended("test2");
		consumer3.assert_announced_ended("test2");
	}

	#[tokio::test]
	async fn duplicate_publishes_replace_active() {
		let origin = Origin::random().produce();

		let broadcast1 = Broadcast::new().produce();
		let broadcast2 = Broadcast::new().produce();
		let broadcast3 = Broadcast::new().produce();

		let consumer1 = broadcast1.consume();
		let consumer2 = broadcast2.consume();
		let consumer3 = broadcast3.consume();

		let mut consumer = origin.consume();

		assert!(origin.publish_broadcast("test", consumer1.clone()));
		assert!(origin.publish_broadcast("test", consumer2.clone()));
		assert!(origin.publish_broadcast("test", consumer3.clone()));

		// Equal hop length: each new publish replaces the active (newer wins ties)
		// and emits Ended/Active for downstream subscribers.
		consumer.assert_announced("test", &consumer1);
		consumer.assert_announced_ended("test");
		consumer.assert_announced("test", &consumer2);
		consumer.assert_announced_ended("test");
		consumer.assert_announced("test", &consumer3);

		// Dropping a backup is invisible to consumers (no event).
		drop(broadcast2);
		consumer.assert_announced_wait();

		// Active closes: shortest-hop backup is promoted (only broadcast1 left).
		drop(broadcast3);
		consumer.assert_announced_ended("test");
		consumer.assert_announced("test", &consumer1);

		drop(broadcast1);
		consumer.assert_announced_ended("test");
		consumer.assert_announced_wait();
	}

	#[tokio::test]
	async fn duplicate_reverse_drop_order() {
		let origin = Origin::random().produce();
		let broadcast1 = Broadcast::new().produce();
		let broadcast2 = Broadcast::new().produce();

		assert!(origin.publish_broadcast("test", broadcast1.consume()));
		assert!(origin.publish_broadcast("test", broadcast2.consume()));

		// Drop the most-recent (active) first; backup is promoted.
		drop(broadcast2);
		// A fresh consumer should still see test as active via broadcast1.
		let mut consumer = origin.consume();
		consumer.assert_announced("test", &broadcast1.consume());
		consumer.assert_announced_wait();

		drop(broadcast1);
		consumer.assert_announced_ended("test");
	}

	#[tokio::test]
	async fn double_publish_is_clone_dedupe() {
		let origin = Origin::random().produce();
		let broadcast = Broadcast::new().produce();

		// Publishing the *same* BroadcastConsumer twice is a no-op the second
		// time (is_clone match).
		assert!(origin.publish_broadcast("test", broadcast.consume()));
		assert!(origin.publish_broadcast("test", broadcast.consume()));

		let mut consumer = origin.consume();
		consumer.assert_announced("test", &broadcast.consume());
		consumer.assert_announced_wait();

		drop(broadcast);
		consumer.assert_announced_ended("test");
	}

	// Regression: the original mpsc-based consumer hit a tokio bug where only
	// the first 127 messages were received synchronously. The new design has
	// no mpsc boundary, so this should pass with `next` too.
	#[tokio::test]
	async fn many_publishes_without_blocking() {
		let origin = Origin::random().produce();
		let broadcast = Broadcast::new().produce();

		let mut consumer = origin.consume();
		for i in 0..256 {
			assert!(origin.publish_broadcast(format!("test{i}"), broadcast.consume()));
		}

		for i in 0..256 {
			consumer.assert_announced(format!("test{i}"), &broadcast.consume());
		}
	}

	#[tokio::test]
	async fn with_root_basic() {
		let origin = Origin::random().produce();
		let broadcast = Broadcast::new().produce();

		let foo_producer = origin.with_root("foo").expect("should create root");
		assert_eq!(foo_producer.root().as_str(), "foo");

		let mut consumer = origin.consume();

		assert!(foo_producer.publish_broadcast("bar/baz", broadcast.consume()));
		consumer.assert_announced("foo/bar/baz", &broadcast.consume());

		let mut foo_consumer = foo_producer.consume();
		foo_consumer.assert_announced("bar/baz", &broadcast.consume());
	}

	#[tokio::test]
	async fn with_root_nested() {
		let origin = Origin::random().produce();
		let broadcast = Broadcast::new().produce();

		let foo_producer = origin.with_root("foo").expect("should create foo root");
		let foo_bar_producer = foo_producer.with_root("bar").expect("should create bar root");
		assert_eq!(foo_bar_producer.root().as_str(), "foo/bar");

		let mut consumer = origin.consume();

		assert!(foo_bar_producer.publish_broadcast("baz", broadcast.consume()));
		consumer.assert_announced("foo/bar/baz", &broadcast.consume());

		let mut foo_bar_consumer = foo_bar_producer.consume();
		foo_bar_consumer.assert_announced("baz", &broadcast.consume());
	}

	#[tokio::test]
	async fn scope_allows_only_listed_prefixes() {
		let origin = Origin::random().produce();
		let broadcast = Broadcast::new().produce();

		let limited = origin
			.scope(&["allowed/path1".into(), "allowed/path2".into()])
			.expect("should create limited producer");

		assert!(limited.publish_broadcast("allowed/path1", broadcast.consume()));
		assert!(limited.publish_broadcast("allowed/path1/nested", broadcast.consume()));
		assert!(limited.publish_broadcast("allowed/path2", broadcast.consume()));

		assert!(!limited.publish_broadcast("notallowed", broadcast.consume()));
		assert!(!limited.publish_broadcast("allowed", broadcast.consume()));
		assert!(!limited.publish_broadcast("other/path", broadcast.consume()));
	}

	#[tokio::test]
	async fn scope_empty_input_returns_none() {
		let origin = Origin::random().produce();
		assert!(origin.scope(&[]).is_none());
	}

	#[tokio::test]
	async fn consumer_scope_filters() {
		let origin = Origin::random().produce();
		let broadcast1 = Broadcast::new().produce();
		let broadcast2 = Broadcast::new().produce();
		let broadcast3 = Broadcast::new().produce();

		// Subscribe `all` *before* publishing so the events arrive in publish
		// order rather than (non-deterministic) HashMap-replay order.
		let mut all = origin.consume();

		assert!(origin.publish_broadcast("allowed", broadcast1.consume()));
		assert!(origin.publish_broadcast("allowed/nested", broadcast2.consume()));
		assert!(origin.publish_broadcast("notallowed", broadcast3.consume()));

		let mut limited = origin
			.consume()
			.scope(&["allowed".into()])
			.expect("should create limited consumer");

		// `limited` was created after the publishes, so ordering is HashMap-based; just
		// check membership.
		let a = limited.try_announced().expect("first");
		let b = limited.try_announced().expect("second");
		limited.assert_announced_wait();
		let mut paths: Vec<String> = [&a, &b].iter().map(|u| u.0.to_string()).collect();
		paths.sort();
		assert_eq!(paths, ["allowed", "allowed/nested"]);

		all.assert_announced("allowed", &broadcast1.consume());
		all.assert_announced("allowed/nested", &broadcast2.consume());
		all.assert_announced("notallowed", &broadcast3.consume());
	}

	#[tokio::test]
	async fn consumer_scope_multiple_prefixes() {
		let origin = Origin::random().produce();
		let broadcast1 = Broadcast::new().produce();
		let broadcast2 = Broadcast::new().produce();
		let broadcast3 = Broadcast::new().produce();

		assert!(origin.publish_broadcast("foo/test", broadcast1.consume()));
		assert!(origin.publish_broadcast("bar/test", broadcast2.consume()));
		assert!(origin.publish_broadcast("baz/test", broadcast3.consume()));

		let mut limited = origin
			.consume()
			.scope(&["foo".into(), "bar".into()])
			.expect("should create limited consumer");

		// Initial replay is HashMap-iteration order (not guaranteed); we just
		// check that we see exactly these two and nothing else.
		let a = limited.try_announced().expect("first");
		let b = limited.try_announced().expect("second");
		limited.assert_announced_wait();

		let mut paths: Vec<String> = [&a, &b].iter().map(|u| u.0.to_string()).collect();
		paths.sort();
		assert_eq!(paths, ["bar/test", "foo/test"]);
	}

	#[tokio::test]
	async fn with_root_combined_with_scope() {
		let origin = Origin::random().produce();
		let broadcast = Broadcast::new().produce();

		let foo_producer = origin.with_root("foo").expect("should create foo root");
		let limited = foo_producer
			.scope(&["bar".into(), "goop/pee".into()])
			.expect("should create limited producer");

		let mut consumer = origin.consume();

		assert!(limited.publish_broadcast("bar", broadcast.consume()));
		assert!(limited.publish_broadcast("bar/nested", broadcast.consume()));
		assert!(limited.publish_broadcast("goop/pee", broadcast.consume()));
		assert!(limited.publish_broadcast("goop/pee/nested", broadcast.consume()));

		assert!(!limited.publish_broadcast("baz", broadcast.consume()));
		assert!(!limited.publish_broadcast("goop", broadcast.consume()));
		assert!(!limited.publish_broadcast("goop/other", broadcast.consume()));

		consumer.assert_announced("foo/bar", &broadcast.consume());
		consumer.assert_announced("foo/bar/nested", &broadcast.consume());
		consumer.assert_announced("foo/goop/pee", &broadcast.consume());
		consumer.assert_announced("foo/goop/pee/nested", &broadcast.consume());
	}

	#[tokio::test]
	async fn with_root_unauthorized() {
		let origin = Origin::random().produce();

		let limited = origin.scope(&["allowed".into()]).expect("should create limited");

		assert!(limited.with_root("notallowed").is_none());

		let allowed_root = limited.with_root("allowed").expect("should create allowed root");
		assert_eq!(allowed_root.root().as_str(), "allowed");
	}

	#[tokio::test]
	async fn wildcard_scope() {
		let origin = Origin::random().produce();
		let broadcast = Broadcast::new().produce();

		assert!(origin.publish_broadcast("any/path", broadcast.consume()));
		assert!(origin.publish_broadcast("other/path", broadcast.consume()));

		let foo_producer = origin.with_root("foo").expect("should create any root");
		assert_eq!(foo_producer.root().as_str(), "foo");
	}

	#[tokio::test]
	async fn select_with_empty_prefix() {
		let origin = Origin::random().produce();
		let broadcast1 = Broadcast::new().produce();
		let broadcast2 = Broadcast::new().produce();

		let demo_producer = origin.with_root("demo").expect("should create demo root");
		let limited = demo_producer
			.scope(&["worm-node".into(), "foobar".into()])
			.expect("should create limited producer");

		assert!(limited.publish_broadcast("worm-node/test", broadcast1.consume()));
		assert!(limited.publish_broadcast("foobar/test", broadcast2.consume()));

		let mut consumer = limited
			.consume()
			.scope(&["".into()])
			.expect("should create consumer with empty prefix");

		let a1 = consumer.try_announced().expect("expected first announcement");
		let a2 = consumer.try_announced().expect("expected second announcement");
		consumer.assert_announced_wait();

		let mut paths: Vec<String> = [&a1, &a2].iter().map(|u| u.0.to_string()).collect();
		paths.sort();
		assert_eq!(paths, ["foobar/test", "worm-node/test"]);
	}

	#[tokio::test]
	async fn select_narrowing_scope() {
		let origin = Origin::random().produce();
		let broadcast1 = Broadcast::new().produce();
		let broadcast2 = Broadcast::new().produce();
		let broadcast3 = Broadcast::new().produce();

		let demo_producer = origin.with_root("demo").expect("should create demo root");
		let limited = demo_producer
			.scope(&["worm-node".into(), "foobar".into()])
			.expect("should create limited producer");

		assert!(limited.publish_broadcast("worm-node", broadcast1.consume()));
		assert!(limited.publish_broadcast("worm-node/foo", broadcast2.consume()));
		assert!(limited.publish_broadcast("foobar/bar", broadcast3.consume()));

		let mut worm = limited
			.consume()
			.scope(&["worm-node".into()])
			.expect("should create worm-node consumer");

		// Replay order isn't deterministic across HashMap; both events should
		// be present and only those.
		let a = worm.try_announced().expect("first");
		let b = worm.try_announced().expect("second");
		worm.assert_announced_wait();
		let mut paths: Vec<String> = [&a, &b].iter().map(|u| u.0.to_string()).collect();
		paths.sort();
		assert_eq!(paths, ["worm-node", "worm-node/foo"]);

		let mut foo = limited
			.consume()
			.scope(&["worm-node/foo".into()])
			.expect("should create worm-node/foo consumer");

		foo.assert_announced("worm-node/foo", &broadcast2.consume());
		foo.assert_announced_wait();
	}

	#[tokio::test]
	async fn nested_paths_with_scope() {
		let origin = Origin::random().produce();
		let broadcast = Broadcast::new().produce();

		let limited = origin.scope(&["a/b/c".into()]).expect("should create limited producer");

		assert!(limited.publish_broadcast("a/b/c", broadcast.consume()));
		assert!(limited.publish_broadcast("a/b/c/d", broadcast.consume()));
		assert!(limited.publish_broadcast("a/b/c/d/e", broadcast.consume()));

		assert!(!limited.publish_broadcast("a", broadcast.consume()));
		assert!(!limited.publish_broadcast("a/b", broadcast.consume()));
		assert!(!limited.publish_broadcast("a/b/other", broadcast.consume()));
	}

	#[tokio::test]
	async fn select_with_non_matching_prefix() {
		let origin = Origin::random().produce();

		let limited = origin
			.scope(&["allowed/path".into()])
			.expect("should create limited producer");

		assert!(limited.consume().scope(&["different/path".into()]).is_none());
		assert!(limited.scope(&["other/path".into()]).is_none());
	}

	// Regression: with_root with trailing slash on owned String.
	#[tokio::test]
	async fn with_root_trailing_slash_consumer() {
		let origin = Origin::random().produce();

		let prefix = "some_prefix/".to_string();
		let mut consumer = origin.consume().with_root(prefix).unwrap();

		let b = origin.create_broadcast("some_prefix/test").unwrap();
		consumer.assert_announced("test", &b.consume());
	}

	#[tokio::test]
	async fn with_root_trailing_slash_producer() {
		let origin = Origin::random().produce();

		let prefix = "some_prefix/".to_string();
		let rooted = origin.with_root(prefix).unwrap();

		let b = rooted.create_broadcast("test").unwrap();

		let mut consumer = rooted.consume();
		consumer.assert_announced("test", &b.consume());
	}

	#[tokio::test]
	async fn with_root_trailing_slash_unannounce() {
		let origin = Origin::random().produce();

		let prefix = "some_prefix/".to_string();
		let mut consumer = origin.consume().with_root(prefix).unwrap();

		let b = origin.create_broadcast("some_prefix/test").unwrap();
		consumer.assert_announced("test", &b.consume());

		drop(b);
		consumer.assert_announced_ended("test");
	}

	#[tokio::test]
	async fn duplicate_prefixes_deduped() {
		let origin = Origin::random().produce();
		let broadcast = Broadcast::new().produce();

		let producer = origin
			.scope(&["demo".into(), "demo".into()])
			.expect("should create producer");

		assert!(producer.publish_broadcast("demo/stream", broadcast.consume()));

		let mut consumer = producer.consume();
		consumer.assert_announced("demo/stream", &broadcast.consume());
		consumer.assert_announced_wait();
	}

	#[tokio::test]
	async fn overlapping_prefixes_deduped() {
		let origin = Origin::random().produce();
		let broadcast = Broadcast::new().produce();

		let producer = origin
			.scope(&["demo".into(), "demo/foo".into()])
			.expect("should create producer");

		assert!(producer.publish_broadcast("demo/bar/stream", broadcast.consume()));

		let mut consumer = producer.consume();
		consumer.assert_announced("demo/bar/stream", &broadcast.consume());
		consumer.assert_announced_wait();
	}

	#[tokio::test]
	async fn allowed_returns_deduped_prefixes() {
		let origin = Origin::random().produce();

		let producer = origin
			.scope(&["demo".into(), "demo/foo".into(), "anon".into()])
			.expect("should create producer");

		assert_eq!(producer.allowed().len(), 2);
	}

	#[tokio::test]
	async fn announced_broadcast_already_announced() {
		let origin = Origin::random().produce();
		let broadcast = Broadcast::new().produce();

		assert!(origin.publish_broadcast("test", broadcast.consume()));

		let consumer = origin.consume();
		let result = consumer.announced_broadcast("test").await.expect("should find it");
		assert!(result.is_clone(&broadcast.consume()));
	}

	#[tokio::test]
	async fn announced_broadcast_delayed() {
		let origin = Origin::random().produce();
		let broadcast = Broadcast::new().produce();

		let consumer = origin.consume();

		let wait = tokio::spawn({
			let consumer = consumer.clone();
			async move { consumer.announced_broadcast("test").await }
		});

		tokio::task::yield_now().await;

		assert!(origin.publish_broadcast("test", broadcast.consume()));

		let result = wait.await.unwrap().expect("should find it");
		assert!(result.is_clone(&broadcast.consume()));
	}

	#[tokio::test]
	async fn announced_broadcast_ignores_unrelated_paths() {
		let origin = Origin::random().produce();
		let other = Broadcast::new().produce();
		let target = Broadcast::new().produce();

		let consumer = origin.consume();

		let wait = tokio::spawn({
			let consumer = consumer.clone();
			async move { consumer.announced_broadcast("target").await }
		});

		tokio::task::yield_now().await;

		assert!(origin.publish_broadcast("other", other.consume()));
		tokio::task::yield_now().await;
		assert!(!wait.is_finished(), "must not resolve on unrelated path");

		assert!(origin.publish_broadcast("target", target.consume()));
		let result = wait.await.unwrap().expect("should find target");
		assert!(result.is_clone(&target.consume()));
	}

	#[tokio::test]
	async fn announced_broadcast_skips_nested_paths() {
		let origin = Origin::random().produce();
		let nested = Broadcast::new().produce();
		let exact = Broadcast::new().produce();

		let consumer = origin.consume();

		let wait = tokio::spawn({
			let consumer = consumer.clone();
			async move { consumer.announced_broadcast("foo").await }
		});

		tokio::task::yield_now().await;

		assert!(origin.publish_broadcast("foo/bar", nested.consume()));
		tokio::task::yield_now().await;
		assert!(!wait.is_finished(), "must not resolve on a nested path");

		assert!(origin.publish_broadcast("foo", exact.consume()));
		let result = wait.await.unwrap().expect("should find foo exactly");
		assert!(result.is_clone(&exact.consume()));
	}

	#[tokio::test]
	async fn announced_broadcast_disallowed() {
		let origin = Origin::random().produce();
		let limited = origin
			.consume()
			.scope(&["allowed".into()])
			.expect("should create limited");

		assert!(limited.announced_broadcast("notallowed").await.is_none());
	}

	#[tokio::test]
	async fn announced_broadcast_scope_too_narrow() {
		let origin = Origin::random().produce();
		let limited = origin
			.consume()
			.scope(&["foo/specific".into()])
			.expect("should create limited");

		let result = limited
			.announced_broadcast("foo")
			.now_or_never()
			.expect("must not block");
		assert!(result.is_none());
	}

	#[tokio::test]
	async fn closed_when_last_producer_drops() {
		let origin = Origin::random().produce();
		let mut consumer = origin.consume();

		assert!(!consumer.is_closed());
		consumer.assert_announced_wait();

		drop(origin);

		assert!(consumer.is_closed());
		assert!(consumer.announced().now_or_never().expect("not blocked").is_none());
	}

	#[tokio::test]
	async fn closed_drains_pending_then_returns_none() {
		let origin = Origin::random().produce();
		let broadcast = Broadcast::new().produce();

		let mut consumer = origin.consume();
		assert!(origin.publish_broadcast("test", broadcast.consume()));

		drop(origin);

		// Pending events should still be drainable after close.
		consumer.assert_announced("test", &broadcast.consume());
		// Then the broadcast is reachable, and dropping it generates Ended too.
		drop(broadcast);
		consumer.assert_announced_ended("test");
		assert!(consumer.announced().await.is_none());
	}
}
