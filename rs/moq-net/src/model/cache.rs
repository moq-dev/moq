//! A shared byte budget for cached groups, repaid by write-time eviction.
//!
//! Every group charges its cached bytes into a [`Pool`] through a crate-internal
//! `Charge`, billed to its track's `Track` account. The pool itself never evicts: it is
//! a handful of atomic counters. While the pool is over capacity, each track accrues
//! eviction debt as it writes (`accrue`),
//! sized proportionally to what it wrote, and pays that debt by aborting its own oldest
//! groups with [`Error::Evicted`](crate::Error::Evicted). Reclamation is therefore
//! distributed across every writing track and converges on the capacity without any
//! global eviction task. A capacity below what the pool pins (each live track's newest
//! group) converges on the pins instead: nothing owes debt for bytes no eviction can free.
//!
//! Cross-track ordering comes from one statistic: the mean last-access time of the
//! evictable population (every cached group except each live track's protected latest).
//! A group accessed more recently than that mean is never evicted, so freshly read
//! or fetched content in one track can't die while another track holds staler
//! content, and a track
//! whose oldest group is staler than the mean accrues debt at double rate. Evicting
//! old entries and inserting new ones both advance the mean, so the eviction
//! frontier moves with cache turnover on its own.
//!
//! The pool also owns the wall-clock LRU window ([`Pool::expiry`]): a group that
//! nobody has read or written for that long is reclaimed, no matter what retention
//! its track advertises. Only a live track's latest group is exempt: once a track
//! ends, a stale consumer can't pin any of it. Track retention
//! ([`max_age`](crate::track::Info::max_age)) decides which groups readers skip as
//! stale; the pool's expiry is the orthogonal bound that frees the bytes of content
//! nobody touches.
//!
//! Expiry is driven by [`Pool::poll`], also called by each origin driver.
//! Reads and writes clear the expiration timestamp without reading a clock.
//! The next cleanup pass dates that activity at its supplied instant. Delayed
//! cleanup extends retention; standalone pools must call `poll` too.
//! Byte-pressure eviction still runs inline on writes.
//!
//! The pool never evicts a live track's latest group, so a session's live-edge budget
//! (`session::Limits::live_edges`) bounds what those hold in bytes: every track a session
//! ingests bills its live edge there, and a session past its budget pays with its own
//! oldest live edges, never another session's.
//!
//! A bare pool is inert by default ([`Pool::unbounded`]): publishers and subscribers
//! that never set a capacity or expiry pay only a couple of atomic counters, and
//! register nothing. A standalone [`origin`](crate::origin::Config) enables
//! [`DEFAULT_EXPIRY`], while a relay creates one configured pool and shares it across
//! every origin so the whole process caches into a single policy.

use crate::util::SendCell;
use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock, PoisonError, Weak};
use std::task::{Poll, ready};
use std::time::Duration;

use super::track::{self, Position, TrackState};
use super::{aborts, expiry, flate, group};

/// Fixed bookkeeping charged per cached group on top of its frame payload bytes.
///
/// A group that holds one small frame is almost entirely bookkeeping: the kio channel
/// carrying its state, the containers the track indexes it by, and the frame slots
/// themselves dwarf a chat-sized payload. Billing payload alone lets such a track cache
/// millions of groups while the pool believes it is inside budget, so the process is
/// killed before anything is evicted.
///
/// Derived from `size_of` rather than pasted from a measured process, so it follows the
/// structs instead of rotting: each half lives beside the types it sizes, in
/// [`group::CACHE_OVERHEAD`] and [`track::CACHE_OVERHEAD`]. It excludes allocator rounding. Frame-slot growth past the initial capacity is
/// charged separately as each group grows.
///
/// Also bounds the live group count (`used / ENTRY_OVERHEAD`), which keeps the
/// access-time sum below u64 (see [`TICK_MS`]).
pub(super) const ENTRY_OVERHEAD: u64 = group::CACHE_OVERHEAD + track::CACHE_OVERHEAD;

/// Sub-tick boosts applied to the last-access stamp, breaking ties within one
/// coarse tick: a frame write outranks merely-inserted content, and a read (a
/// delivered or fetched group, a frame read, a backfill's birth) outranks both.
const WRITE_BOOST: u64 = 1;
const READ_BOOST: u64 = 2;
const ACCESS_SHIFT: u32 = 2;

/// Milliseconds per tick of the coarse clock behind access timestamps.
///
/// Coarse ticks keep the count-weighted timestamp sum far from u64 overflow: the
/// sum is bounded by `elapsed_ticks * live_groups`, plus two low tie-breaking
/// bits. Live groups are bounded by `used / ENTRY_OVERHEAD`, and twenty years of
/// ticks (6.3e9) times a 64 GiB target's worst case of ~70M groups is ~1.8e18 after
/// that encoding, a tenth of `u64::MAX`. A byte-weighted mean would overflow u64
/// even at whole-second ticks, which is why the mean is count-weighted.
const TICK_MS: u64 = 100;

/// Default idle window for standalone origins and relays.
pub const DEFAULT_EXPIRY: Duration = Duration::from_secs(30);

/// The initial policy for a [`Pool`].
///
/// The default is inert: no byte target and no idle expiry. Use
/// [`Self::with_capacity`] and [`Self::with_expiry`] before creating the pool.
#[derive(Clone, Debug, Default)]
pub struct Config {
	capacity: Option<u64>,
	expiry: Option<Duration>,
}

impl Config {
	/// Set the initial byte target. `None` leaves it unbounded.
	pub fn with_capacity(mut self, capacity: impl Into<Option<u64>>) -> Self {
		self.capacity = capacity.into();
		self
	}

	/// Set the wall-clock LRU window. `None` disables idle reclamation.
	///
	/// A cached group (other than a live track's latest) that nobody reads or writes
	/// for this long is reclaimed, surfacing to any remaining reader as
	/// [`Error::Old`](crate::Error::Old). This is independent of track retention:
	/// [`max_age`](crate::track::Info::max_age) decides what readers skip, while this
	/// window keeps idle content from pinning memory. Reclaiming without a write behind
	/// it needs [`Pool::poll`] called periodically (origins do this automatically).
	/// The value is fixed when the
	/// pool is created. Values are rounded up to the pool's 100 ms clock tick, with
	/// 100 ms as the minimum effective window.
	pub fn with_expiry(mut self, expiry: impl Into<Option<Duration>>) -> Self {
		self.expiry = expiry.into();
		self
	}
}

/// A shared cache policy and byte budget; cloning shares both.
///
/// The pool tracks how many payload bytes are cached across every registered group,
/// plus the mean last-access time of the evictable groups. It never
/// evicts on its own: tracks accrue eviction debt as they write and evict their own
/// oldest groups to pay it. Idle expiration runs during [`Self::poll`]. The capacity is
/// therefore a target usage converges toward, not a hard limit: carried debt, capped
/// payments, and the always-protected live edge all let usage transiently exceed it.
#[derive(Clone)]
pub struct Pool {
	// Every track and handoff registry reaches back into group state, so the cell
	// proves `Send` once here instead of inside every origin and broadcast handle.
	inner: Arc<SendCell<Inner>>,
}

impl Default for Pool {
	fn default() -> Self {
		Self::unbounded()
	}
}

struct Inner {
	// Total bytes currently charged, including per-entry overhead.
	used: AtomicU64,
	// u64::MAX means unbounded.
	capacity: AtomicU64,
	// The part of `used` no eviction can reclaim while its track lives: each track's live
	// edge (see [`Footprint`]) and spare encoder. Signed: a footprint that flips while one of its bytes is
	// mid-update pins it an instant early or late, which may dip this below zero.
	pinned: AtomicI64,
	// Wall-clock LRU window in milliseconds; u64::MAX means never expire by idleness.
	expiry: u64,
	// Reference point for the coarse tick clock.
	clock: Mutex<Option<Clock>>,
	tick: AtomicU64,
	// Sum and count of last-access ticks across the evictable population, giving a
	// count-weighted mean. Tracks add a group when it becomes evictable (demoted
	// from the live edge, or inserted behind it) and remove it when it leaves.
	access_sum: AtomicU64,
	access_count: AtomicU64,
	// Live track accounts, so [`Pool::sweep`] can expire idle groups in a track that
	// has stopped writing. Empty and never touched when expiry is disabled: the byte
	// budget needs no registry, since a track that never writes never grows the pool.
	// Weak, because a track owns its account and the account must not outlive it.
	tracks: kio::Lock<slab::Slab<Weak<Track>>>,
	// The first instant a driver supplied, and the latest as micros past it, so reading
	// the wall clock never runs behind the time the drivers keep; see [`Pool::now_wall`].
	// Unset until a driver collects, which is also what arms wall-clock deadlines.
	wall_epoch: OnceLock<crate::time::Instant>,
	wall_micros: AtomicU64,
	// Tracks with reads parked on a wall-clock deadline, by their earliest one.
	wall: kio::Lock<WallDeadlines>,
	// Hand-offs holding a failed group's head for a successor route, oldest first. The
	// heads sit outside every track's eviction order, so the pool reclaims them itself:
	// first under capacity pressure, and once its idle window passes.
	held: Mutex<Vec<Weak<group::Handoff>>>,
}

/// The pool-wide index of wall-clock deadlines that [`Pool::poll`] fires.
///
/// One entry per track and earliest deadline: the reads themselves park in the track's
/// own `expiry::Wakes`, so arming one takes this lock only when it moves its track's
/// earliest deadline forward in time, never per read.
#[derive(Default)]
struct WallDeadlines {
	// Keyed by the track account's address too, so tracks sharing an instant coexist.
	due: BTreeMap<(crate::time::Instant, usize), Weak<Track>>,
	// The drivers collecting this pool, woken when a deadline earlier than any they
	// were told about is armed.
	drivers: kio::WaiterList,
	// Reads that met a deadline before any driver collected, woken by the first
	// collection to arm it. Holds live waiters only, so an undriven pool stays bounded.
	undriven: kio::WaiterList,
}

struct Clock {
	epoch: crate::time::Instant,
	now: crate::time::Instant,
	sweep: Option<crate::time::Instant>,
}

impl Pool {
	/// Create a pool from an initial policy.
	///
	/// The budget counts frame payload bytes plus a fixed cost per cached group, which
	/// is most of what a group carrying one small frame occupies. It is not process
	/// RSS, and it is a convergence target rather than a hard limit; leave headroom
	/// when sizing it from real memory. The expiry is fixed, while the capacity can
	/// later be changed with [`Self::resize`].
	pub fn new(config: Config) -> Self {
		let expiry = config.expiry.map_or(u64::MAX, |expiry| {
			let ms = u64::try_from(expiry.as_millis()).unwrap_or(u64::MAX);
			if ms == u64::MAX {
				return u64::MAX;
			}
			ms.max(1).div_ceil(TICK_MS).saturating_mul(TICK_MS)
		});
		Self {
			inner: Arc::new(SendCell::new(Inner {
				used: AtomicU64::new(0),
				capacity: AtomicU64::new(config.capacity.unwrap_or(u64::MAX)),
				pinned: AtomicI64::new(0),
				expiry,
				clock: Mutex::new(None),
				tick: AtomicU64::new(0),
				access_sum: AtomicU64::new(0),
				access_count: AtomicU64::new(0),
				tracks: kio::Lock::new(slab::Slab::new()),
				wall_epoch: OnceLock::new(),
				wall_micros: AtomicU64::new(0),
				wall: kio::Lock::default(),
				held: Mutex::default(),
			})),
		}
	}

	/// Create a pool that never evicts. This is the [`Default`].
	pub fn unbounded() -> Self {
		Self::new(Config::default())
	}

	/// The configured byte target, or `None` when unbounded.
	pub fn capacity(&self) -> Option<u64> {
		match self.inner.capacity.load(Ordering::Relaxed) {
			u64::MAX => None,
			capacity => Some(capacity),
		}
	}

	/// Bytes currently cached across every registered group.
	pub fn used(&self) -> u64 {
		self.inner.used.load(Ordering::Relaxed)
	}

	/// Bytes no eviction can reclaim while their tracks live: every live track's newest group
	/// and spare encoder.
	pub(super) fn pinned(&self) -> u64 {
		self.inner.pinned.load(Ordering::Relaxed).max(0).unsigned_abs()
	}

	fn pin(&self, n: u64) {
		self.inner.pinned.fetch_add(n.cast_signed(), Ordering::Relaxed);
	}

	fn unpin(&self, n: u64) {
		self.inner.pinned.fetch_sub(n.cast_signed(), Ordering::Relaxed);
	}

	/// What usage converges toward: the capacity, or the pinned bytes when they are more,
	/// since nothing can reclaim those.
	fn target(&self) -> u64 {
		self.inner.capacity.load(Ordering::Relaxed).max(self.pinned())
	}

	/// How far usage is past [`Self::target`], if it is: all that eviction, shedding, and
	/// readers folding could still free toward it.
	pub(super) fn excess(&self) -> Option<u64> {
		self.used().checked_sub(self.target()).filter(|excess| *excess > 0)
	}

	/// What the capacity leaves beside the pinned bytes, for everything the pool may reclaim.
	/// Zero in a pool held past its capacity by what it pins.
	pub(super) fn room(&self) -> u64 {
		self.inner
			.capacity
			.load(Ordering::Relaxed)
			.saturating_sub(self.pinned())
	}

	/// Change the capacity. `None` makes the pool unbounded.
	///
	/// Takes effect as tracks write: a shrink leaves the pool over budget, which every
	/// subsequent write pays down proportionally. Nothing is reclaimed synchronously.
	pub fn resize(&self, capacity: impl Into<Option<u64>>) {
		let capacity = capacity.into().unwrap_or(u64::MAX);
		self.inner.capacity.store(capacity, Ordering::Relaxed);
	}

	/// The wall-clock LRU window, or `None` when idle content is never reclaimed.
	pub fn expiry(&self) -> Option<Duration> {
		match self.inner.expiry {
			u64::MAX => None,
			ms => Some(Duration::from_millis(ms)),
		}
	}

	/// The LRU window in coarse ticks; effectively infinite when disabled.
	pub(super) fn expiry_ticks(&self) -> u64 {
		match self.inner.expiry {
			u64::MAX => u64::MAX,
			ms => ms / TICK_MS,
		}
	}

	/// Sample recency periodically while either cache policy is enabled.
	///
	/// Expiry is approximate: activity is dated on the following cleanup pass,
	/// and passes run at half the idle window.
	pub(super) fn sweep_interval(&self) -> Option<Duration> {
		self.expiry()
			.or_else(|| self.capacity().map(|_| DEFAULT_EXPIRY))
			.map(|window| window / 2)
	}

	/// Have the pool drop `handoff`'s held head under capacity pressure, oldest first, or
	/// once its idle window passes.
	pub(super) fn hold_head(&self, handoff: &Arc<group::Handoff>) {
		let mut held = self.inner.held.lock().unwrap_or_else(PoisonError::into_inner);
		// Forget the hand-offs done with before growing, so a burst of failed groups whose
		// heads recovery adopts at once grows the registry no further than the heads held.
		if held.len() == held.capacity() {
			self.prune_held(&mut held);
		}
		held.push(Arc::downgrade(handoff));
	}

	/// Drop held heads, oldest first, while the pool is past its target.
	pub(super) fn reclaim_held(&self) {
		if self.excess().is_some() {
			let mut held = self.inner.held.lock().unwrap_or_else(PoisonError::into_inner);
			self.prune_held(&mut held);
		}
	}

	/// Drop the held heads past the idle window, then the oldest while the pool is past its
	/// target, and forget the hand-offs holding none.
	fn prune_held(&self, held: &mut Vec<Weak<group::Handoff>>) {
		held.retain(|handoff| {
			let Some(handoff) = handoff.upgrade() else {
				return false;
			};
			!handoff.expire(self.excess().is_some())
		});
	}

	#[cfg(test)]
	pub(super) fn held_capacity(&self) -> usize {
		self.inner
			.held
			.lock()
			.unwrap_or_else(PoisonError::into_inner)
			.capacity()
	}

	/// Expire idle groups and held heads across the registered tracks.
	pub(super) fn sweep(&self) {
		{
			let mut held = self.inner.held.lock().unwrap_or_else(PoisonError::into_inner);
			self.prune_held(&mut held);
			// Give back what a burst of held heads grew it to, which the pool no longer bills.
			let len = held.len();
			held.shrink_to(len * 2);
		}
		// Upgrade outside each track's lock: dropping its last account unregisters it.
		let tracks: Vec<_> = self
			.inner
			.tracks
			.lock()
			.iter()
			.filter_map(|(_, track)| track.upgrade())
			.collect();
		for track in tracks {
			track.sweep();
		}
	}

	/// Collect idle cache entries and return the next cleanup time, like
	/// [`Driver::poll`](crate::time::Driver::poll).
	///
	/// Call at the returned deadline, and again whenever `waiter` wakes, including when
	/// idle. Calls before that deadline only advance the pool's sampled clock. A due
	/// pass visits every cached group, dating accesses since the last pass and
	/// reclaiming idle groups except each live track's latest. Delayed calls extend
	/// retention. Shared pools use the latest supplied instant.
	///
	/// `None` means both cache policies are disabled and no read waits on a wall-clock
	/// deadline. After enabling a capacity
	/// with [`Self::resize`], call this again to resume periodic clock sampling.
	///
	/// It also releases reads parked on a wall-clock deadline (a group's successor
	/// arriving plus the reader's budget), so the returned deadline includes the earliest
	/// of those, and `waiter` wakes when a read arms one earlier than it. Until a pool is
	/// first collected, no deadline fires; that first collection wakes the reads waiting
	/// on one so they arm it.
	pub fn poll(&self, now: crate::time::Instant, waiter: &kio::Waiter) -> Option<crate::time::Instant> {
		self.advance(now, true, Some(waiter))
	}

	fn advance(
		&self,
		now: crate::time::Instant,
		sweep: bool,
		driver: Option<&kio::Waiter>,
	) -> Option<crate::time::Instant> {
		let first;
		let next = {
			// Shared origins must not run overlapping collection passes.
			let mut clock = self.inner.clock.lock().unwrap_or_else(PoisonError::into_inner);
			let clock = clock.get_or_insert(Clock {
				epoch: now,
				now,
				sweep: None,
			});
			let now = now.max(clock.now);
			let tick = u64::try_from(now.duration_since(clock.epoch).as_millis() / u128::from(TICK_MS))
				.expect("cache clock overflow");
			self.inner.tick.store(tick, Ordering::Relaxed);
			clock.now = now;
			// Collection passes are serialized by the clock lock, so only one sees the first.
			first = self.inner.wall_epoch.get().is_none();
			let epoch = *self.inner.wall_epoch.get_or_init(|| now);
			let micros = u64::try_from(now.duration_since(epoch).as_micros()).unwrap_or(u64::MAX);
			// The clock lock already orders drivers and clamps their supplied time.
			self.inner.wall_micros.store(micros, Ordering::Relaxed);
			if self.sweep_interval().is_none() {
				clock.sweep = None;
			} else if sweep && clock.sweep.is_none_or(|at| at <= now) {
				self.sweep();
				clock.sweep = self.sweep_interval().and_then(|interval| now.checked_add(interval));
			}
			clock.sweep
		};
		if first {
			let mut undriven = self.inner.wall.lock().undriven.take();
			undriven.wake();
		}
		let wall = self.release_due(driver);
		next.into_iter().chain(wall).min()
	}

	/// Wake every read whose wall-clock deadline has passed, returning the
	/// earliest left and registering `driver` to hear of an earlier one.
	fn release_due(&self, driver: Option<&kio::Waiter>) -> Option<crate::time::Instant> {
		let mut wall = self.inner.wall.lock();
		if wall.due.is_empty() {
			if let Some(driver) = driver {
				driver.register(&mut wall.drivers);
			}
			return None;
		}
		// With no parked read there is nothing to age. Otherwise the ambient clock
		// still matters, even when a caller supplies an instant behind its deadline.
		// Keep one sample across tracks that rearm while this pass releases them.
		let now = self.now_wall();
		loop {
			let mut due = Vec::new();
			while let Some(entry) = wall.due.first_entry()
				&& entry.key().0 <= now
			{
				due.push(entry.remove());
			}
			if due.is_empty() {
				if let Some(driver) = driver {
					driver.register(&mut wall.drivers);
				}
				return wall.due.first_key_value().map(|((at, _), _)| *at);
			}
			drop(wall);
			// Outside the lock: a track re-arms its next deadline here.
			for track in due.iter().filter_map(Weak::upgrade) {
				if let Some(next) = track.wakes.release_wall(now) {
					self.schedule(next, None, &track);
				}
			}
			wall = self.inner.wall.lock();
		}
	}

	/// The instant wall-clock staleness is judged at: the model's clock, never behind the
	/// latest instant a driver collected this pool at.
	///
	/// Both, because the clock alone runs behind a driver that is supplied time some other
	/// way (a paused test runtime), which would leave a read woken at a deadline judging
	/// it not yet reached and parking on it again.
	pub(super) fn now_wall(&self) -> crate::time::Instant {
		let wall = super::clock::wall();
		match self.inner.wall_epoch.get() {
			Some(epoch) => {
				let driven =
					epoch.saturating_add(Duration::from_micros(self.inner.wall_micros.load(Ordering::Relaxed)));
				wall.max(driven)
			}
			None => wall,
		}
	}

	/// Whether a driver collects this pool, so a wall-clock deadline armed in it fires.
	/// If not, `waiter` wakes on the first collection, to arm its deadline then.
	fn watch_driven(&self, waiter: &kio::Waiter) -> bool {
		// Checked under the lock the first collection drains under, so none slips between.
		let mut wall = self.inner.wall.lock();
		if self.inner.wall_epoch.get().is_some() {
			return true;
		}
		waiter.register(&mut wall.undriven);
		false
	}

	/// Index `track`'s earliest wall-clock deadline `at` in place of `replaced`, waking the
	/// drivers if it is now the pool's earliest.
	fn schedule(&self, at: crate::time::Instant, replaced: Option<crate::time::Instant>, track: &Arc<Track>) {
		let mut wall = self.inner.wall.lock();
		let address = Arc::as_ptr(track) as usize;
		if let Some(replaced) = replaced {
			wall.due.remove(&(replaced, address));
		}
		let key = (at, address);
		wall.due.insert(key, Arc::downgrade(track));
		if wall.due.first_key_value().is_some_and(|(first, _)| *first == key) {
			let mut drivers = std::mem::take(&mut wall.drivers);
			drop(wall);
			drivers.wake();
		}
	}

	/// Move the pool's sampled clock forward by `duration`, dating accesses on either
	/// side without collecting, as if that much time passed between two passes.
	#[cfg(test)]
	pub(super) fn step(&self, duration: Duration) {
		let now = self
			.inner
			.clock
			.lock()
			.unwrap_or_else(PoisonError::into_inner)
			.as_ref()
			.map(|clock| clock.now);
		let now = now.unwrap_or_else(super::clock::wall);
		self.date(now);
		self.date(now.saturating_add(duration));
	}

	#[cfg(test)]
	fn date(&self, now: crate::time::Instant) {
		self.advance(now, false, None);
		let tracks: Vec<_> = self
			.inner
			.tracks
			.lock()
			.iter()
			.filter_map(|(_, track)| track.upgrade())
			.collect();
		for track in tracks {
			if let Some(state) = track.state.upgrade() {
				state.read().date_cache_accesses(self.now());
			}
		}
	}

	/// Enter a track account into the sweep registry, returning its key. `None` when
	/// idle reclamation is off, which is what keeps a bare pool free of bookkeeping.
	fn register(&self, track: &Arc<Track>) -> Option<usize> {
		self.expiry()?;
		Some(self.inner.tracks.lock().insert(Arc::downgrade(track)))
	}

	/// Drop a track account from the sweep registry.
	fn unregister(&self, key: usize) {
		self.inner.tracks.lock().remove(key);
	}

	/// Returns true if both handles share the same underlying pool.
	#[cfg(test)]
	pub(super) fn same_pool(&self, other: &Self) -> bool {
		Arc::ptr_eq(&self.inner, &other.inner)
	}

	/// A handle that reaches this budget without keeping it alive.
	pub fn downgrade(&self) -> PoolWeak {
		PoolWeak {
			inner: Arc::downgrade(&self.inner),
		}
	}

	/// Charge `n` more cached bytes.
	pub(super) fn add(&self, n: u64) {
		self.inner.used.fetch_add(n, Ordering::Relaxed);
	}

	/// Release `n` cached bytes.
	pub(super) fn sub(&self, n: u64) {
		self.inner.used.fetch_sub(n, Ordering::Relaxed);
	}

	/// Coarse ticks since the first cleanup call.
	pub(super) fn now(&self) -> u64 {
		self.inner.tick.load(Ordering::Relaxed)
	}

	/// Encode the current clock tick and an access-priority tie breaker.
	fn stamp(&self, boost: u64) -> u64 {
		self.now().saturating_mul(1 << ACCESS_SHIFT).saturating_add(boost)
	}

	/// Mean last-access tick across the evictable population, or `None` when it is
	/// empty. The sum and count are read separately, so the mean is approximate
	/// under concurrent updates; eviction only needs a rough frontier.
	pub(super) fn average(&self) -> Option<u64> {
		let count = self.inner.access_count.load(Ordering::Relaxed);
		if count == 0 {
			return None;
		}
		Some(self.inner.access_sum.load(Ordering::Relaxed) / count)
	}

	/// A group with last-access tick `ts` joined the evictable population.
	pub(super) fn access_insert(&self, ts: u64) {
		self.inner.access_sum.fetch_add(ts, Ordering::Relaxed);
		self.inner.access_count.fetch_add(1, Ordering::Relaxed);
	}

	/// A group with last-access tick `ts` left the evictable population.
	pub(super) fn access_remove(&self, ts: u64) {
		self.inner.access_sum.fetch_sub(ts, Ordering::Relaxed);
		self.inner.access_count.fetch_sub(1, Ordering::Relaxed);
	}

	/// An evictable group's last-access tick moved from `old` to `new` (a FETCH hit).
	pub(super) fn access_refresh(&self, old: u64, new: u64) {
		// A single wrapping add keeps the sum exact even under racing refreshes.
		self.inner
			.access_sum
			.fetch_add(new.wrapping_sub(old), Ordering::Relaxed);
	}

	/// The eviction debt a track takes on by writing `written` bytes, or `None` while
	/// the pool is within its [`Self::target`] (the caller should forget any outstanding
	/// debt).
	///
	/// The debt is `written * used / target`, so paying it evicts slightly more
	/// than was written and the overshoot decays toward the target. Tracks double
	/// it when their oldest content is staler than [`Self::average`]. Saturates: a
	/// tiny capacity must not wrap a huge debt into a small one.
	pub(super) fn accrue(&self, written: u64) -> Option<u64> {
		let used = self.used();
		let target = self.target();
		if used <= target {
			return None;
		}
		let debt = written as u128 * used as u128 / target.max(1) as u128;
		Some(u64::try_from(debt).unwrap_or(u64::MAX))
	}
}

impl std::fmt::Debug for Pool {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Pool")
			.field("used", &self.used())
			.field("capacity", &self.capacity())
			.field("expiry", &self.expiry())
			.finish()
	}
}

/// A handle to a [`Pool`] that does not keep the budget alive.
///
/// [`upgrade`](Self::upgrade) stops returning a [`Pool`] once every strong handle has
/// dropped, which is how a background resizer learns the budget it manages is gone and
/// nothing can cache into it any more.
#[derive(Clone)]
pub struct PoolWeak {
	inner: std::sync::Weak<SendCell<Inner>>,
}

impl PoolWeak {
	/// Recover a [`Pool`], or `None` once every strong handle has dropped.
	pub fn upgrade(&self) -> Option<Pool> {
		self.inner.upgrade().map(|inner| Pool { inner })
	}
}

impl std::fmt::Debug for PoolWeak {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self.upgrade() {
			Some(pool) => pool.fmt(f),
			None => f.debug_struct("PoolWeak").finish_non_exhaustive(),
		}
	}
}

/// Entries the live-edge order keeps for edges that moved on before it drops them.
const EDGE_SLACK: usize = 64;

/// What one session's tracks may keep of their live edges, which the [`Pool`] never evicts.
///
/// Each track a session ingests bills its live edge here: the newest group, which a live
/// track keeps whatever the pool's budget, so a publisher holding many quiet tracks would
/// otherwise hold one group each past any capacity. A frame write that leaves the session
/// past its limit sheds the frames of the session's live edges in the order they last
/// moved, oldest first, and then the writer's own older frames, so the cost stays with the session
/// causing it. The default is unbounded and costs nothing.
#[derive(Clone, Default)]
pub(crate) struct Edges(Option<Arc<EdgesInner>>);

struct EdgesInner {
	limit: u64,
	// Bytes billed by the live edges of this session's tracks. Signed: a footprint that flips
	// while one of its bytes is mid-update bills the session an instant early or late, which
	// may dip it below zero, never a reason to reclaim.
	used: AtomicI64,
	// The tracks in the order their live edges moved, oldest first, each with the move it
	// was queued at, so an entry for an edge that moved on since is skipped.
	order: Mutex<VecDeque<(Weak<Track>, u64)>>,
	// The tracks billing here, so the order knows how many of its entries can be current.
	tracks: AtomicUsize,
}

impl Edges {
	/// A budget of `limit` bytes, or unbounded for `None`.
	pub(crate) fn new(limit: Option<u64>) -> Self {
		Self(limit.map(|limit| {
			Arc::new(EdgesInner {
				limit,
				used: AtomicI64::new(0),
				order: Mutex::default(),
				tracks: AtomicUsize::new(0),
			})
		}))
	}

	/// Bytes the session's live edges bill now.
	#[cfg(test)]
	pub(super) fn used(&self) -> u64 {
		self.0
			.as_ref()
			.map_or(0, |inner| inner.used.load(Ordering::Relaxed).max(0).unsigned_abs())
	}

	fn add(&self, n: u64) {
		if let Some(inner) = &self.0 {
			inner.used.fetch_add(n.cast_signed(), Ordering::Relaxed);
		}
	}

	fn sub(&self, n: u64) {
		if let Some(inner) = &self.0 {
			inner.used.fetch_sub(n.cast_signed(), Ordering::Relaxed);
		}
	}

	/// How far past its limit the session is, if it is.
	fn excess(&self) -> Option<u64> {
		let inner = self.0.as_ref()?;
		let used = inner.used.load(Ordering::Relaxed).max(0).unsigned_abs();
		used.checked_sub(inner.limit).filter(|excess| *excess > 0)
	}

	/// Pay the session's excess with its oldest live edges other than `writer`'s, then with
	/// `writer`'s own older frames. Takes track locks: call with none held.
	fn reclaim(&self, writer: &Track) {
		let Some(inner) = &self.0 else {
			return;
		};
		// Each entry is offered once per call, so an order holding only edges this write
		// can't take never spins.
		let mut offers = inner.order.lock().unwrap_or_else(PoisonError::into_inner).len();
		let mut kept = None;
		while offers > 0 && self.excess().is_some() {
			offers -= 1;
			let Some((weak, moves)) = inner.order.lock().unwrap_or_else(PoisonError::into_inner).pop_front() else {
				break;
			};
			let Some(track) = weak.upgrade() else {
				continue;
			};
			if !track.edge_is(moves) {
				continue;
			}
			// The writer's edge is the one being written: it pays last, and only in part.
			if std::ptr::eq(Arc::as_ptr(&track), writer) {
				kept = Some((weak, moves));
				continue;
			}
			let Some(state) = track.state.upgrade() else {
				continue;
			};
			// Unqueued before the shed, so whatever the edge bills after it queues it again.
			track.queued.store(false, Ordering::Relaxed);
			if let Ok(mut state) = state.write() {
				state.shed_edge_all(moves);
			}
		}
		if let Some(entry) = kept {
			inner
				.order
				.lock()
				.unwrap_or_else(PoisonError::into_inner)
				.push_front(entry);
		}
		if let Some(excess) = self.excess()
			&& let Some(state) = writer.state.upgrade()
			&& let Ok(mut state) = state.write()
		{
			state.shed_edge(excess);
		}
	}

	/// Queue `track`'s live edge as the session's newest, after its `moves`th move.
	fn moved(&self, track: &Arc<Track>, moves: u64) {
		let Some(inner) = &self.0 else {
			return;
		};
		let mut order = inner.order.lock().unwrap_or_else(PoisonError::into_inner);
		order.push_back((Arc::downgrade(track), moves));
		// Each move leaves its track's earlier entry behind: drop those once they clearly
		// outnumber the current ones, which costs a constant per move.
		if order.len() > 2 * inner.tracks.load(Ordering::Relaxed) + EDGE_SLACK {
			order.retain(|(track, moves)| track.upgrade().is_some_and(|track| track.edge_is(*moves)));
		}
	}
}

impl std::fmt::Debug for Edges {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match &self.0 {
			Some(inner) => f
				.debug_struct("Edges")
				.field("used", &inner.used.load(Ordering::Relaxed))
				.field("limit", &inner.limit)
				.finish(),
			None => f.debug_struct("Edges").finish_non_exhaustive(),
		}
	}
}

/// Gross bytes a track writes before a frame write settles its eviction debt itself,
/// so a track appending frames to open groups (never inserting another group) still
/// pays. Coarse: the cost is one track-state lock per threshold crossing.
const WRITE_CHARGE_THRESHOLD: u64 = 256 * 1024;

/// Maximum cadence for write-driven expiry scans, in the pool's coarse ticks.
///
/// Byte debt settles only after enough data accumulates, but expiry is a time
/// policy and must also run for low-bitrate tracks. Limiting that extra track lock
/// to once per second keeps the write hot path cheap while a bounded scan drains
/// stale backlogs steadily.
const EXPIRY_SCAN_TICKS: u64 = 1000 / TICK_MS;

/// One track's account against the [`Pool`], shared with every group it creates.
///
/// Groups charge their bytes here (through a [`Charge`]) rather than straight into the
/// pool, so the track can drain what its own groups wrote into eviction debt and pay it
/// off by evicting them. The link back to the track is a [`kio::Weak`] because the track
/// owns its cached groups and each of those owns this account: anything stronger would
/// make a track's cache immortal.
///
/// The default account is detached: an unbounded pool and no track, so every operation
/// is a no-op.
#[derive(Default)]
pub(super) struct Track {
	pool: Pool,

	// Gross bytes charged by this track's groups (payload plus overhead), never
	// decremented here: the track swaps it out as it accrues debt.
	written: AtomicU64,

	// Earliest coarse tick when a frame write may run another expiry scan.
	next_expiry: AtomicU64,

	// Rotating position of the expiry scan over the track's eviction order.
	expiry_cursor: AtomicUsize,

	// The track that pays this account off, holding the groups being charged.
	state: kio::Weak<TrackState>,

	// This account's slot in the pool's sweep registry, absent when the pool has no
	// expiry window (nothing is registered) or for the detached default account.
	sweep: OnceLock<usize>,

	// The track's reads parked on their drift budget. Here because this account is the
	// link every group's frame writes already follow back to the track.
	wakes: expiry::Wakes,
	// Allocated only when a subscriber parks groups behind a serving cap.
	aborts: OnceLock<Arc<aborts::Aborts>>,

	// Here rather than on the track state so a frame write advances it without the track
	// lock, and wakes only the readers following it.
	largest: kio::Shared<Largest>,

	// Whether the track ever declared an end, set under the track lock and never cleared,
	// so a reader looking for one skips that lock until there is one to read.
	declared: AtomicBool,

	// What the frames this track's groups are still receiving may allocate up front,
	// shared with every track minted from the same origin handle.
	frames: super::frame::Budget,

	// What the session ingesting this track may keep of its tracks' live edges, which this
	// track's undemoted groups bill.
	edges: Edges,
	// How many times the live edge moved to a newer group, naming its entry in `edges`.
	moves: AtomicU64,
	// Whether the live edge has an entry in its session's order, so one that a reclaim shed
	// queues again once it grows.
	queued: AtomicBool,

	// One finished local group's encoder, kept until another group needs it or the
	// account drops. Raw codec only: an Extra here would keep this account alive.
	encoder: Mutex<Option<flate::Encoder>>,

	// What this track's readers' records bill, how many bill anything, the tail ledgers' bills,
	// and the eviction debt the track could not pay itself, which those readers pay; see
	// [`Reader`].
	readers: Readers,
}

/// What a track's readers bill for their records, and the debt they owe.
#[derive(Default)]
struct Readers {
	// Records only: the mean [`Reader::ends`] compares against, which a tail never skews.
	bytes: AtomicU64,
	count: AtomicU64,
	// Tail ledgers: room for the debt like the records, but never one to end.
	tails: AtomicU64,
	debt: AtomicU64,
}

/// The greatest location a track has written or its source reported, followed by the track
/// handles' `largest`: each new group at its head, then each frame written to it.
#[derive(Default)]
struct Largest {
	position: Option<Position>,
	// The largest group's content may still grow: no copy of the whole group finished. An
	// aborted one stays open, since it can be recreated or fetched.
	open: bool,
	// The track ended, so no newer position can follow.
	ended: bool,
}

impl Track {
	/// Raise the track's largest location to `position`, waking only its own waiters.
	pub(super) fn advance_largest(&self, position: Position) {
		let mut largest = self.largest.lock();
		if largest.position.is_none_or(|current| position > current) {
			largest.position = Some(position);
			largest.open = true;
		}
	}

	/// Group `key` holds every frame it will have: a whole copy finished. A group name
	/// fixes its content, so no legal write raises the largest past it.
	pub(super) fn finish_largest(&self, key: u64) {
		let mut largest = self.largest.lock();
		if largest.position.is_some_and(|position| position.group == key) {
			largest.open = false;
		}
	}

	/// The greatest location written or reported, without waiting.
	pub(super) fn largest(&self) -> Option<Position> {
		self.largest.read().position
	}

	/// Whether the largest group's content may still grow; see [`Self::finish_largest`].
	pub(super) fn largest_open(&self) -> bool {
		self.largest.read().open
	}

	/// Whether `position` is the largest and its group ended there; see [`Self::finish_largest`].
	pub(super) fn largest_finished(&self, position: Position) -> bool {
		let largest = self.largest.read();
		largest.position == Some(position) && !largest.open
	}

	/// The source reported `position` as its largest, its group ended there when `finished`.
	pub(super) fn report_largest(&self, position: Position, finished: bool) {
		let mut largest = self.largest.lock();
		if largest.position.is_none_or(|current| position > current) {
			largest.position = Some(position);
			largest.open = true;
		}
		if finished && largest.position == Some(position) {
			largest.open = false;
		}
	}

	/// End the largest stream once the track ends: no newer frame can follow.
	pub(super) fn end_largest(&self) {
		let mut largest = self.largest.lock();
		if !largest.ended {
			largest.ended = true;
		}
	}

	/// Poll for a largest location within `bounds` newer than `seen`, recording it there.
	/// `None` once the track ended with nothing newer.
	pub(super) fn poll_largest(
		&self,
		seen: &mut Option<Position>,
		bounds: track::Bounds,
		waiter: &kio::Waiter,
	) -> Poll<Option<Position>> {
		let newer = |largest: &Largest| {
			largest
				.position
				.filter(|position| bounds.contains(position.group) && seen.is_none_or(|seen| *position > seen))
		};
		let largest = ready!(
			self.largest
				.poll(waiter, |largest| match newer(largest).is_some() || largest.ended {
					true => Poll::Ready(()),
					false => Poll::Pending,
				})
		);
		let next = newer(&largest);
		if next.is_some() {
			*seen = next;
		}
		Poll::Ready(next)
	}

	/// The largest frame within `bounds` newer than `seen` without waiting, recording it there.
	pub(super) fn take_largest(&self, seen: &mut Option<Position>, bounds: track::Bounds) -> Option<Position> {
		let next = self
			.largest
			.read()
			.position
			.filter(|position| bounds.contains(position.group) && seen.is_none_or(|seen| *position > seen));
		if next.is_some() {
			*seen = next;
		}
		next
	}

	/// Open an account against `pool` for the track behind `state`, its frames
	/// allocating up front within `frames` and its live edge billed to `edges`.
	pub(super) fn new(
		pool: Pool,
		frames: super::frame::Budget,
		edges: Edges,
		state: kio::Weak<TrackState>,
	) -> Arc<Self> {
		if let Some(inner) = &edges.0 {
			inner.tracks.fetch_add(1, Ordering::Relaxed);
		}
		let track = Arc::new(Self {
			pool,
			frames,
			edges,
			moves: AtomicU64::new(0),
			queued: AtomicBool::new(false),
			written: AtomicU64::new(0),
			next_expiry: AtomicU64::new(0),
			expiry_cursor: AtomicUsize::new(0),
			state,
			sweep: OnceLock::new(),
			wakes: Default::default(),
			aborts: OnceLock::new(),
			largest: Default::default(),
			declared: AtomicBool::new(false),
			encoder: Mutex::default(),
			readers: Readers::default(),
		});
		if let Some(key) = track.pool.register(&track) {
			let _ = track.sweep.set(key);
		}
		track
	}

	/// The pool this track caches into.
	pub(super) fn pool(&self) -> &Pool {
		&self.pool
	}

	/// What this track's frames may allocate up front.
	pub(super) fn frames(&self) -> &super::frame::Budget {
		&self.frames
	}

	/// The track holding this account's groups, while it is still open.
	pub(super) fn track(&self) -> Option<kio::Producer<TrackState>> {
		self.state.upgrade()
	}

	/// The live edge moved to a newer group: queue it as its session's newest. Called under
	/// the track lock, which orders the moves.
	pub(super) fn edge_moved(self: &Arc<Self>) {
		let moves = self.moves.fetch_add(1, Ordering::Relaxed) + 1;
		self.queued.store(true, Ordering::Relaxed);
		self.edges.moved(self, moves);
	}

	/// The live edge billed more: queue it again if a reclaim took its entry.
	fn edge_grew(self: &Arc<Self>) {
		if !self.queued.swap(true, Ordering::Relaxed) {
			self.edges.moved(self, self.moves.load(Ordering::Relaxed));
		}
	}

	/// How many times the live edge moved.
	#[cfg(all(test, feature = "async"))]
	pub(super) fn moves(&self) -> u64 {
		self.moves.load(Ordering::Relaxed)
	}

	/// Whether the live edge is still the one its `moves`th move queued.
	pub(super) fn edge_is(&self, moves: u64) -> bool {
		self.moves.load(Ordering::Relaxed) == moves
	}

	/// The track's reads parked on their drift budget.
	pub(super) fn wakes(&self) -> &expiry::Wakes {
		&self.wakes
	}

	/// The track declared an end. Called under the track lock.
	pub(super) fn declare_end(&self) {
		self.declared.store(true, Ordering::Release);
	}

	/// Whether the track ever declared an end, so the end may be worth the track lock.
	pub(super) fn end_declared(&self) -> bool {
		self.declared.load(Ordering::Acquire)
	}

	/// Watch every group the publisher cuts, rather than chosen keys.
	pub(super) fn watch_cuts(&self) -> aborts::Watch {
		self.aborts.get_or_init(Arc::default).watch_cuts()
	}

	/// Notify watchers that group `key` aborted, if the publisher cut it.
	pub(super) fn aborted(&self, key: u64, cut: bool) {
		if cut && let Some(aborts) = self.aborts.get() {
			aborts.notify(key);
		}
	}

	/// Park `waiter` until the pool's wall clock reaches `at`. While no driver collects the
	/// pool, nothing would release it, so `waiter` parks on the first collection instead.
	pub(super) fn watch_wall(self: &Arc<Self>, at: crate::time::Instant, waiter: &kio::Waiter) {
		if !self.pool.watch_driven(waiter) {
			return;
		}
		if let Some(replaced) = self.wakes.watch_wall(at, waiter) {
			self.pool.schedule(at, replaced, self);
		}
	}

	/// Charge a new group's fixed overhead, returning its [`Charge`].
	pub(super) fn charge(self: &Arc<Self>) -> Charge {
		self.pool.add(ENTRY_OVERHEAD);
		self.written.fetch_add(ENTRY_OVERHEAD, Ordering::Relaxed);
		let access = Arc::new(Access::new(self.pool.stamp(0)));
		Charge {
			track: Some(self.clone()),
			bytes: ENTRY_OVERHEAD,
			access,
			counted: false,
			footprint: Arc::new(Footprint {
				content: AtomicU64::new(0),
				fixed: AtomicU64::new(ENTRY_OVERHEAD),
			}),
			content: 0,
			edge: false,
			whole: false,
		}
	}

	/// Open an empty bill against this account, for bookkeeping the track keeps beside its
	/// groups, such as its drop set.
	pub(super) fn extra(self: &Arc<Self>) -> Extra {
		Extra {
			track: Some(self.clone()),
			footprint: None,
			bytes: AtomicU64::new(0),
			state: AtomicU64::new(0),
		}
	}

	/// Open an empty bill against this account for one reader's record.
	pub(super) fn reader(self: &Arc<Self>) -> Reader {
		Reader {
			track: self.clone(),
			bytes: 0,
			record: true,
		}
	}

	/// Open an empty bill against this account for a subscription's tail ledger: it pays the
	/// readers' debt by folding, but never ends, since a publisher's drops are final.
	pub(super) fn tail(self: &Arc<Self>) -> Reader {
		Reader {
			track: self.clone(),
			bytes: 0,
			record: false,
		}
	}

	/// Hand the part of `debt` this track could not pay to its readers, up to what their
	/// records and tails bill beyond what they already owe.
	pub(super) fn owe_readers(&self, debt: &mut u64) {
		let readers = &self.readers;
		let mut shifted = 0;
		let _ = readers.debt.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |owed| {
			let billed = readers.bytes.load(Ordering::Relaxed) + readers.tails.load(Ordering::Relaxed);
			let room = billed.saturating_sub(owed);
			shifted = room.min(*debt);
			(shifted > 0).then_some(owed + shifted)
		});
		*debt -= shifted;
	}

	/// Bill `bytes` of a failed group's head while a hand-off holds it for a successor route.
	///
	/// Readers that stop polling can hold a hand-off indefinitely, so the head is billed
	/// like cached content and bounded the same way: `None` when it would take the pool past
	/// its target, and [`Pool::hold_head`] has the pool reclaim it like idle content.
	pub(super) fn hold(self: &Arc<Self>, bytes: u64) -> Option<Bill> {
		if self.pool.used().saturating_add(bytes) > self.pool.target() {
			return None;
		}
		self.pool.add(bytes);
		Some(Bill {
			track: self.clone(),
			bytes,
			at: self.pool.now(),
		})
	}

	/// Take everything written since the last call, to be turned into eviction debt.
	pub(super) fn take_written(&self) -> u64 {
		self.written.swap(0, Ordering::Relaxed)
	}

	/// Settle eviction debt and expire idle groups from a frame write.
	///
	/// Called with no group lock held (locks are ordered track then group). Cheap
	/// until the byte or time gate crosses: relaxed atomics plus a coarse clock read
	/// when expiry is enabled. This is what makes a track that only appends frames to
	/// open groups, never inserting another group, still pay its debt and age its
	/// idle content out.
	///
	/// `now` is the coarse tick a cache access on this path just sampled (see
	/// [`Charge::add`]), reused so a frame write reads the clock once instead of
	/// twice. `None` leaves the gate to sample it, and only if it needs it.
	pub(super) fn settle(&self, now: Option<u64>) {
		self.settle_inner(now, false);
	}

	/// Settle eviction debt now if the pool is past its target, without waiting for the
	/// write threshold: a reader's record grows by runs, never by enough bytes to cross it.
	/// Takes the track lock: call with none held.
	pub(super) fn charge_over(&self) {
		if self.pool.excess().is_none() {
			return;
		}
		if let Some(state) = self.state.upgrade()
			&& let Ok(mut state) = state.write()
		{
			state.charge_debt();
		}
	}

	/// Settle from [`Pool::sweep`], dating activity and expiring every idle candidate.
	pub(super) fn sweep(&self) {
		self.settle_inner(None, true);
	}

	fn settle_inner(&self, now: Option<u64>, full: bool) {
		if self.edges.excess().is_some() {
			self.edges.reclaim(self);
		}
		let settle_debt = self.written.load(Ordering::Relaxed) >= WRITE_CHARGE_THRESHOLD;
		let scan_expiry = if full {
			self.pool.expiry().is_some()
		} else {
			self.expiry_due(now)
		};
		if !settle_debt && !scan_expiry {
			return;
		}
		// Counts as a producer while it lives, which is why `track::Producer` gates
		// its teardown on its own clone count rather than the state's.
		let Some(state) = self.state.upgrade() else {
			// An ended track's channel is closed, but a stale consumer can still hold its
			// groups: the sweep expires them in place, or they would never go.
			if full && scan_expiry {
				self.state.read(|state| state.expire_closed(state.expiry_scan_drain()));
			}
			return;
		};
		let expiry = if scan_expiry {
			let state = state.read();
			let scan = if full {
				state.expiry_scan_drain()
			} else {
				state.expiry_scan()
			};
			state.expiry_mutation_due(scan).then_some(scan)
		} else {
			None
		};
		if !settle_debt && expiry.is_none() {
			return;
		}
		if let Ok(mut state) = state.write() {
			if settle_debt {
				state.charge_debt();
			}
			if let Some(scan) = expiry {
				state.evict_expired_scan(scan);
			}
		}
	}

	/// Claim the next rotating window in the track's eviction order.
	pub(super) fn next_expiry_scan(&self, width: usize) -> usize {
		self.expiry_cursor.fetch_add(width, Ordering::Relaxed)
	}

	/// Claim the next write-driven expiry scan when its time gate is due.
	fn expiry_due(&self, now: Option<u64>) -> bool {
		let expiry = self.pool.expiry_ticks();
		if expiry == u64::MAX {
			return false;
		}

		// Sampled here, below the gate: a pool with no expiry window returns above
		// without ever reading the clock, whether or not a caller had a tick.
		let now = now.unwrap_or_else(|| self.pool.now());
		let interval = expiry.clamp(1, EXPIRY_SCAN_TICKS);
		let deadline = now.saturating_add(interval);
		let next = self.next_expiry.load(Ordering::Relaxed);
		if now < next {
			return false;
		}

		self.next_expiry
			.compare_exchange(next, deadline, Ordering::Relaxed, Ordering::Relaxed)
			.is_ok()
	}
}

impl Drop for Track {
	fn drop(&mut self) {
		if let Some(inner) = &self.edges.0 {
			inner.tracks.fetch_sub(1, Ordering::Relaxed);
		}
		if self
			.encoder
			.get_mut()
			.unwrap_or_else(PoisonError::into_inner)
			.take()
			.is_some()
		{
			self.pool.sub(flate::ENCODER_HEAP);
			self.pool.unpin(flate::ENCODER_HEAP);
		}
		if let Some(key) = self.sweep.get() {
			self.pool.unregister(*key);
		}
		// Its deadline would otherwise linger in a shared pool until it came due.
		if let Some(at) = self.wakes.wall_armed() {
			let address = std::ptr::from_ref::<Self>(self) as usize;
			self.pool.inner.wall.lock().due.remove(&(at, address));
		}
	}
}

/// The bytes a held head of a failed group bills to its track's
/// pool, released on drop.
pub(super) struct Bill {
	track: Arc<Track>,
	bytes: u64,
	// The coarse tick it was billed at.
	at: u64,
}

impl Bill {
	/// Whether the pool's idle window passed since it was billed.
	pub(super) fn expired(&self) -> bool {
		let pool = &self.track.pool;
		pool.now().saturating_sub(self.at) >= pool.expiry_ticks()
	}
}

impl Drop for Bill {
	fn drop(&mut self) {
		self.track.pool.sub(self.bytes);
	}
}

/// The flag bit in a [`Footprint`]'s counts: set while its bytes are its track's live edge.
const EDGE: u64 = 1 << 63;
/// The flag bit in a [`Footprint`]'s content: set once its group finished.
const FINAL: u64 = 1 << 62;
/// The flag bit in a [`Footprint`]'s counts: set while a fill holds its group for a reader.
const HELD: u64 = 1 << 61;
/// The byte count under a [`Footprint`]'s flags.
const BYTES: u64 = !(EDGE | FINAL | HELD);

/// Whether content counted under `flags` is pinned: a whole live edge, or a held group.
fn pins_content(flags: u64) -> bool {
	flags & HELD != 0 || flags & (EDGE | FINAL) == EDGE | FINAL
}

/// Whether fixed bytes counted under `flags` are pinned: a live edge's, or a held group's.
fn pins_fixed(flags: u64) -> bool {
	flags & (EDGE | HELD) != 0
}

/// The bytes one cached group bills, across its [`Charge`] and the [`Extra`]s it hands out
/// (shared content, inflated copies, codec state), and whether they are its track's live
/// edge, which its session's [`Edges`] bill and the [`Pool`] pins.
///
/// Its content bills the session, and is pinned once its group finished: a live edge still
/// growing sheds its front under pool debt, while a whole one is what a new reader starts
/// from. Its fixed costs, the [`ENTRY_OVERHEAD`] and codec state, are always pinned and
/// never bill the session: they scale with the tracks a session holds, which its stream
/// credit bounds, and shedding can never free them.
///
/// Each count is one atomic holding its flags in its top bits (no group bills 2^62 bytes),
/// so a byte added while a flag flips is counted exactly once, by whichever came last.
#[derive(Default)]
pub(super) struct Footprint {
	content: AtomicU64,
	fixed: AtomicU64,
}

impl Footprint {
	/// Count `n` more content bytes, returning whether they bill the session.
	fn add(&self, track: &Track, n: u64) -> bool {
		let prior = self.content.fetch_add(n, Ordering::Relaxed);
		let edge = prior & EDGE != 0;
		if edge {
			track.edges.add(n);
		}
		if pins_content(prior) {
			track.pool.pin(n);
		}
		edge
	}

	/// Count `n` fewer content bytes, no more than it holds.
	fn sub(&self, track: &Track, n: u64) {
		let (prior, taken) = Self::take(&self.content, n);
		if prior & EDGE != 0 {
			track.edges.sub(taken);
		}
		if pins_content(prior) {
			track.pool.unpin(taken);
		}
	}

	/// Count `n` more fixed bytes.
	fn add_fixed(&self, track: &Track, n: u64) {
		if pins_fixed(self.fixed.fetch_add(n, Ordering::Relaxed)) {
			track.pool.pin(n);
		}
	}

	/// Count `n` fewer fixed bytes, no more than it holds.
	fn sub_fixed(&self, track: &Track, n: u64) {
		let (prior, taken) = Self::take(&self.fixed, n);
		if pins_fixed(prior) {
			track.pool.unpin(taken);
		}
	}

	/// The group finished, so its content no longer grows.
	fn finish(&self, track: &Track) {
		self.flag_content(track, FINAL, true);
	}

	fn promote(&self, track: &Track) {
		self.flag(track, EDGE, true);
	}

	fn demote(&self, track: &Track) {
		self.flag(track, EDGE, false);
	}

	/// Pin every byte while a fill holds the group for a reader, or stop.
	fn hold(&self, track: &Track, held: bool) {
		self.flag(track, HELD, held);
	}

	/// Set or clear `flag` on both counts, moving their bytes in or out of the session's
	/// edges and the pool's pins as they change.
	fn flag(&self, track: &Track, flag: u64, on: bool) {
		self.flag_content(track, flag, on);
		let prior = Self::set(&self.fixed, flag, on);
		let now = Self::flagged(prior, flag, on);
		match (pins_fixed(prior), pins_fixed(now)) {
			(false, true) => track.pool.pin(prior & BYTES),
			(true, false) => track.pool.unpin(prior & BYTES),
			_ => {}
		}
	}

	fn flag_content(&self, track: &Track, flag: u64, on: bool) {
		let prior = Self::set(&self.content, flag, on);
		let now = Self::flagged(prior, flag, on);
		let bytes = prior & BYTES;
		match (prior & EDGE != 0, now & EDGE != 0) {
			(false, true) => track.edges.add(bytes),
			(true, false) => track.edges.sub(bytes),
			_ => {}
		}
		match (pins_content(prior), pins_content(now)) {
			(false, true) => track.pool.pin(bytes),
			(true, false) => track.pool.unpin(bytes),
			_ => {}
		}
	}

	/// Set or clear `flag` on `count`, returning its prior value.
	fn set(count: &AtomicU64, flag: u64, on: bool) -> u64 {
		match on {
			true => count.fetch_or(flag, Ordering::Relaxed),
			false => count.fetch_and(!flag, Ordering::Relaxed),
		}
	}

	fn flagged(prior: u64, flag: u64, on: bool) -> u64 {
		match on {
			true => prior | flag,
			false => prior & !flag,
		}
	}

	/// Take up to `n` from `count`, returning its prior value and what it took.
	fn take(count: &AtomicU64, n: u64) -> (u64, u64) {
		let mut taken = 0;
		let prior = count
			.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |prior| {
				taken = n.min(prior & BYTES);
				Some(prior - taken)
			})
			.expect("always updates");
		(prior, taken)
	}
}

/// The [`Footprint`]s of every copy of one group that shares its content, which bill one
/// another's frames: a frame bills whichever copy wrote it first, and any copy may hold it.
///
/// So they bill the session together: while one copy is itself its track's cached live
/// edge, they all do, a copy that aborted included, since its frames may still be what the
/// live copy holds; once none is, none does. Once one finished the whole group, they are all
/// whole.
#[derive(Default)]
pub(super) struct Siblings(Mutex<Vec<Sibling>>);

struct Sibling {
	track: Arc<Track>,
	footprint: Arc<Footprint>,
	// Whether this copy is itself the live edge, not billed for a sibling.
	edge: bool,
	// Whether this copy finished the whole group, not just a range a transport bound cut.
	whole: bool,
}

impl Siblings {
	/// Record whether `charge`'s copy is itself the live edge, adding it if new, and bill
	/// every copy to match: after a copy joins, is promoted, leaves the cache, or is cleared.
	pub(super) fn update(&self, charge: &Charge) {
		let Some(track) = &charge.track else {
			return;
		};
		let mut siblings = self.0.lock().unwrap_or_else(PoisonError::into_inner);
		match siblings
			.iter_mut()
			.find(|sibling| Arc::ptr_eq(&sibling.footprint, &charge.footprint))
		{
			Some(sibling) => {
				sibling.edge = charge.edge;
				sibling.whole = charge.whole;
			}
			None => siblings.push(Sibling {
				track: track.clone(),
				footprint: charge.footprint.clone(),
				edge: charge.edge,
				whole: charge.whole,
			}),
		}
		Self::bill(&mut siblings);
	}

	/// Stop billing every copy to the session: one leaving the live edge, as its slot does,
	/// takes them all.
	pub(super) fn demote(&self) {
		let mut siblings = self.0.lock().unwrap_or_else(PoisonError::into_inner);
		for sibling in siblings.iter_mut() {
			sibling.edge = false;
		}
		Self::bill(&mut siblings);
	}

	/// Bill every copy while any is the live edge, and pin every copy's content once any
	/// finished the whole group, since a copy's frames may be what that sibling holds. Forgets
	/// copies nothing bills any more.
	fn bill(siblings: &mut Vec<Sibling>) {
		siblings.retain(|sibling| Arc::strong_count(&sibling.footprint) > 1);
		let edge = siblings.iter().any(|sibling| sibling.edge);
		if siblings.iter().any(|sibling| sibling.whole) {
			for sibling in siblings.iter() {
				sibling.footprint.finish(&sibling.track);
			}
		}
		for sibling in siblings.iter() {
			match edge {
				true => sibling.footprint.promote(&sibling.track),
				false => sibling.footprint.demote(&sibling.track),
			}
		}
	}
}

/// The RAII byte accounting for one cached group, owned by the group's state.
///
/// `add`/`sub` mirror the group's cached payload bytes into the pool with plain
/// atomics, and every charged byte (overhead included) is also accumulated into the
/// track's account, which the track drains into eviction debt on its next write. The
/// charge also owns the group's sample in the pool's access mean, so the sample lives
/// exactly as long as the cached bytes do: aborting or dropping the group removes both,
/// no matter who does it or when. The default charge is detached: it belongs to no
/// account and every operation is a no-op.
#[derive(Default)]
pub(super) struct Charge {
	track: Option<Arc<Track>>,
	// Bytes currently charged, including ENTRY_OVERHEAD, released on drop.
	bytes: u64,
	// The group's last-access stamp, shared with the handles that read it during a
	// track scan. Only ever written here, under the group's state lock.
	access: Arc<Access>,
	// Whether `access` is currently a sample in the pool's access mean, i.e. the
	// group is in the evictable population. A plain bool: it is only read while
	// updating the mean, which the owning state's lock already serializes.
	counted: bool,
	// Every byte the group bills, its extras' included, and whether they bill its session's
	// live edges.
	footprint: Arc<Footprint>,
	// The part of `bytes` in `footprint`: content, its fixed overhead left out.
	content: u64,
	// Whether this group is itself its track's cached live edge, as opposed to billing the
	// session only for a sibling that is: see [`Siblings`].
	edge: bool,
	// Whether this copy finished the whole group, which makes its siblings whole too.
	whole: bool,
}

/// The tick of one cached group's last cache access: its creation, every write, and
/// every read (group delivery, frame reads, FETCH hits, a fetched backfill's birth).
/// Eviction protection and age expiry both key off it.
///
/// Shared, so a track scan can read it without entering the group's state. The
/// eviction and expiry walks run under the track lock and weigh every candidate
/// against [`Pool::average`], so reaching this through the group lock would nest one
/// lock inside the other once per candidate.
///
/// Atomic for a second reason on the write side: a kio write guard's release notifies
/// every parked consumer, and a mere cache access must not wake anyone, so [`Charge`]
/// stamps this through a shared guard.
#[derive(Default)]
pub(super) struct Access {
	stamp: AtomicU64,
	expires: AtomicU64,
}

impl Access {
	fn new(stamp: u64) -> Self {
		Self {
			stamp: AtomicU64::new(stamp),
			expires: AtomicU64::new(u64::MAX),
		}
	}

	/// The stamp, tie-breaking bits included.
	pub(super) fn get(&self) -> u64 {
		self.stamp.load(Ordering::Relaxed)
	}

	/// Clear the expiration timestamp until a cleanup pass observes this access.
	pub(super) fn touch(&self) {
		self.expires.store(u64::MAX, Ordering::Relaxed);
	}

	/// The last access tick, assigning undated activity only during cleanup.
	pub(super) fn tick(&self, now: Option<u64>) -> Option<u64> {
		let tick = match now {
			Some(now) => match self
				.expires
				.compare_exchange(u64::MAX, now, Ordering::Relaxed, Ordering::Relaxed)
			{
				Ok(_) => now,
				Err(tick) => tick,
			},
			None => self.expires.load(Ordering::Relaxed),
		};
		(tick != u64::MAX).then_some(tick)
	}

	/// Advance to `target` if it is newer, returning the previous stamp.
	fn bump(&self, target: u64) -> u64 {
		// `fetch_max` keeps the stamp monotone, and its prior value makes the
		// paired mean update exact even for back-to-back accesses.
		self.stamp.fetch_max(target, Ordering::Relaxed)
	}
}

impl Charge {
	/// Charge `n` more payload bytes, counting them as written.
	///
	/// A write is also an access: it restarts the retention clock and keeps an
	/// actively-growing group (a straggler or backfill still being filled) from
	/// being evicted or expired mid-write, even within the same coarse tick as
	/// content that was merely inserted.
	///
	/// Returns the coarse tick it stamped, which the caller hands to
	/// [`Track::settle`] so the write path reads the clock once rather than twice.
	/// `None` when the charge is detached and stamped nothing.
	pub(super) fn add(&mut self, n: u64) -> Option<u64> {
		if let Some(track) = &self.track {
			track.pool.add(n);
			track.written.fetch_add(n, Ordering::Relaxed);
			if self.footprint.add(track, n) {
				track.edge_grew();
			}
			self.bytes += n;
			self.content += n;
		}
		self.touch(WRITE_BOOST)
	}

	/// Release `n` charged bytes the group dropped while still cached, without an access.
	pub(super) fn sub(&mut self, n: u64) {
		if let Some(track) = &self.track {
			let n = n.min(self.bytes);
			track.pool.sub(n);
			let content = n.min(self.content);
			self.footprint.sub(track, content);
			self.footprint.sub_fixed(track, n - content);
			self.content -= content;
			self.bytes -= n;
		}
	}

	/// The group's full cached footprint: payload bytes plus overhead.
	pub(super) fn size(&self) -> u64 {
		self.bytes
	}

	/// Move already-accounted payload to a shared content owner without recording a write.
	pub(super) fn transfer(&mut self, bytes: u64) -> Extra {
		if self.track.is_none() {
			return Extra::default();
		}
		self.bytes = self
			.bytes
			.checked_sub(bytes)
			.expect("transferred payload must already be charged");
		// Still in the footprint, now through the extra.
		self.content = self.content.saturating_sub(bytes);
		Extra {
			track: self.track.clone(),
			footprint: Some(self.footprint.clone()),
			bytes: AtomicU64::new(bytes),
			state: AtomicU64::new(0),
		}
	}

	/// The shared handle to this group's last-access stamp, so the group can read it
	/// without taking the state lock this charge lives behind.
	pub(super) fn access(&self) -> Arc<Access> {
		self.access.clone()
	}

	/// An [`Extra`] against this charge's account, detached when the charge is.
	pub(super) fn extra(&self) -> Extra {
		Extra {
			track: self.track.clone(),
			footprint: Some(self.footprint.clone()),
			bytes: AtomicU64::new(0),
			state: AtomicU64::new(0),
		}
	}

	/// Tick of the group's last cache access.
	pub(super) fn accessed(&self) -> u64 {
		self.access.get()
	}

	/// Enter the group into the evictable population (demoted from the live edge,
	/// or inserted behind it), sampling its access time into the pool's mean.
	/// Idempotent.
	pub(super) fn demote(&mut self) {
		if let Some(track) = &self.track
			&& !self.counted
		{
			track.pool.access_insert(self.accessed());
			self.counted = true;
		}
		self.leave_edge();
	}

	/// Stop billing the session, as the group leaves the cache, without entering the
	/// evictable population it is no longer part of.
	pub(super) fn leave_edge(&mut self) {
		if let Some(track) = &self.track {
			self.footprint.demote(track);
		}
		self.edge = false;
	}

	/// Bill the group, its extras included, to its session's [`Edges`] as its track's cached
	/// live edge, which eviction never takes while the track lives. Idempotent; a no-op once
	/// demoted.
	pub(super) fn promote(&mut self) {
		if let Some(track) = &self.track
			&& !self.counted
		{
			self.footprint.promote(track);
			self.edge = true;
		}
	}

	/// Pin every byte the group bills while a fill holds it for a reader, which eviction and
	/// shedding pass over until the reader takes it, or stop.
	pub(super) fn hold(&self, held: bool) {
		if let Some(track) = &self.track {
			self.footprint.hold(track, held);
		}
	}

	/// This copy finished, the `whole` group or only a range a transport bound cut: pin its
	/// content while it is the live edge, which no longer grows.
	pub(super) fn finish(&mut self, whole: bool) {
		if let Some(track) = &self.track {
			self.footprint.finish(track);
		}
		self.whole |= whole;
	}

	/// Whether this copy finished the whole group.
	#[cfg(test)]
	pub(super) fn is_whole(&self) -> bool {
		self.whole
	}

	/// Record a cache read: a delivered or fetched group, a frame read, or a
	/// fetched backfill's birth. `&self` so the read paths can stamp through a
	/// shared guard without waking parked consumers.
	pub(super) fn refresh(&self) {
		self.touch(READ_BOOST);
	}

	/// Record a write that charges no new bytes (a chunk written into an
	/// already-charged in-flight frame): restarts the retention clock like any
	/// other write. `&mut self` deliberately: reaching it through a kio write
	/// guard marks the guard modified, so its release wakes parked readers. Returns
	/// the stamped tick like [`Self::add`].
	pub(super) fn record_write(&mut self) -> Option<u64> {
		self.touch(WRITE_BOOST)
	}

	/// Advance the last-access stamp to the current clock tick with `boost` priority.
	///
	/// The boost breaks ties within one coarse tick: written content outranks
	/// merely-inserted content, and explicitly read content outranks both, so a
	/// same-tick access still reads as strictly newer than the population mean of
	/// weaker accesses. Idempotent within a tick (monotone, never regressing), so
	/// repeated accesses remain idempotent without advancing the expiry clock.
	/// Returns the tick it read, or `None` when the charge is detached.
	fn touch(&self, boost: u64) -> Option<u64> {
		let track = self.track.as_ref()?;
		// Cleanup assigns the next supplied timestamp to this access.
		self.access.touch();
		let target = track.pool.stamp(boost);
		let prev = self.access.bump(target);
		if target > prev && self.counted {
			track.pool.access_refresh(prev, target);
		}
		Some(target >> ACCESS_SHIFT)
	}

	/// The track account this charge bills, unless it is detached.
	pub(super) fn account(&self) -> Option<&Arc<Track>> {
		self.track.as_ref()
	}

	/// Release everything this charge holds: bytes, overhead, and the access
	/// sample. Idempotent; used when the group aborts and clears its frames.
	pub(super) fn clear(&mut self) {
		self.leave_edge();
		if let Some(track) = &self.track {
			self.footprint.sub(track, self.content);
			self.footprint.sub_fixed(track, self.bytes - self.content);
			self.content = 0;
			track.pool.sub(self.bytes);
			self.bytes = 0;
			if self.counted {
				track.pool.access_remove(self.accessed());
				self.counted = false;
			}
		}
	}
}

impl Drop for Charge {
	fn drop(&mut self) {
		self.clear();
	}
}

/// What one reader's record, or one subscription's tail ledger, bills to its track's account,
/// and its share of the debt the track could not pay. Released on drop.
///
/// Only a record can [`Self::ends`]; a tail pays by folding, so it stays out of the mean
/// that decides which record to end.
pub(crate) struct Reader {
	track: Arc<Track>,
	bytes: u64,
	record: bool,
}

impl Reader {
	/// Bill `bytes` in all, settling the track's debt after a growth: call with no track lock
	/// held.
	pub(crate) fn set(&mut self, bytes: u64) {
		if bytes == self.bytes {
			return;
		}
		let readers = &self.track.readers;
		let billed = if self.record { &readers.bytes } else { &readers.tails };
		if self.record {
			match (self.bytes, bytes) {
				(0, _) => readers.count.fetch_add(1, Ordering::Relaxed),
				(_, 0) => readers.count.fetch_sub(1, Ordering::Relaxed),
				_ => 0,
			};
		}
		if bytes > self.bytes {
			let grown = bytes - self.bytes;
			billed.fetch_add(grown, Ordering::Relaxed);
			self.track.pool.add(grown);
			self.track.written.fetch_add(grown, Ordering::Relaxed);
			self.bytes = bytes;
			self.track.charge_over();
		} else {
			// Whatever a record frees pays toward its readers' debt, however it shrank.
			let shrunk = self.bytes - bytes;
			billed.fetch_sub(shrunk, Ordering::Relaxed);
			let _ = readers.debt.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |debt| {
				(debt > 0).then(|| debt.saturating_sub(shrunk))
			});
			self.track.pool.sub(shrunk);
			self.bytes = bytes;
		}
	}

	/// The debt the track's readers owe, forgiven once the pool is back within its target:
	/// nothing needs the room any more, as the track forgives its own debt.
	pub(crate) fn debt(&self) -> u64 {
		let readers = &self.track.readers;
		if self.track.pool.excess().is_none() {
			readers.debt.store(0, Ordering::Relaxed);
			return 0;
		}
		readers.debt.load(Ordering::Relaxed)
	}

	/// Whether this record is one to end when folding leaves a debt: it bills more than a
	/// cached group's overhead, so ending it frees more than evicting a group would, at least
	/// its readers' average, so a reader holding little never ends for one holding much, and
	/// the pool has [`Pool::room`] beside its pins: in a pool held past its capacity by what
	/// it pins, ending a reader frees room nothing may use, and every key a reader is owed
	/// resolves (a fill holds what it fetched until taken), so its record folds instead.
	pub(super) fn ends(&self) -> bool {
		let readers = &self.track.readers;
		let count = readers.count.load(Ordering::Relaxed);
		self.record
			&& self.bytes > ENTRY_OVERHEAD
			&& u128::from(self.bytes) * u128::from(count) >= u128::from(readers.bytes.load(Ordering::Relaxed))
			&& self.track.pool.room() > 0
	}
}

impl std::fmt::Debug for Reader {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Reader")
			.field("bytes", &self.bytes)
			.finish_non_exhaustive()
	}
}

impl Drop for Reader {
	fn drop(&mut self) {
		self.set(0);
	}
}

/// Bytes a group keeps beside its cached frames, billed to the same pool and account as
/// its [`Charge`] but reachable outside the group's write lock: a compressed group's
/// inflated copy, which its readers build, and its codec state. Released on [`Self::clear`] or drop.
///
/// Its owner serializes the changes; the count is atomic so the eviction walk can size
/// the group without that owner's lock.
#[derive(Default)]
pub(super) struct Extra {
	track: Option<Arc<Track>>,
	// The footprint of the group it was opened from, so its bytes bill that group's live
	// edge; `None` for the track's own bookkeeping.
	footprint: Option<Arc<Footprint>>,
	bytes: AtomicU64,
	// The part of `bytes` that is codec state rather than content, kept out of the footprint:
	// a fixed cost per group, like its entry overhead, so it bills the pool but not the
	// session's live edges, which shedding could never bring back under budget.
	state: AtomicU64,
}

impl Extra {
	/// Count `n` content bytes in, or out of, the owning group's footprint.
	fn footprint(&self, track: &Arc<Track>, n: u64, add: bool) {
		if let Some(footprint) = &self.footprint {
			match add {
				true => {
					if footprint.add(track, n) {
						track.edge_grew();
					}
				}
				false => footprint.sub(track, n),
			}
		}
	}

	/// Count `n` bytes of codec state in, or out of, the owning group's footprint.
	fn footprint_fixed(&self, track: &Track, n: u64, add: bool) {
		if let Some(footprint) = &self.footprint {
			match add {
				true => footprint.add_fixed(track, n),
				false => footprint.sub_fixed(track, n),
			}
		}
	}

	/// Take this track's spare encoder with its charge, or bill a new one.
	pub(super) fn take_encoder(&self) -> flate::Encoder {
		if let Some(track) = &self.track {
			let encoder = track.encoder.lock().unwrap_or_else(PoisonError::into_inner).take();
			if let Some(mut encoder) = encoder {
				// Moving existing heap is not a write and must not accrue eviction debt.
				self.bytes.fetch_add(flate::ENCODER_HEAP, Ordering::Relaxed);
				self.state.fetch_add(flate::ENCODER_HEAP, Ordering::Relaxed);
				track.pool.unpin(flate::ENCODER_HEAP);
				self.footprint_fixed(track, flate::ENCODER_HEAP, true);
				encoder.reset();
				return encoder;
			}
		}
		self.add_state(flate::ENCODER_HEAP);
		flate::Encoder::new()
	}

	/// Park a finished encoder and its charge, dropping it if the track has a spare.
	pub(super) fn park_encoder(&self, encoder: flate::Encoder) {
		if let Some(track) = &self.track {
			let mut spare = track.encoder.lock().unwrap_or_else(PoisonError::into_inner);
			if spare.is_none() {
				self.bytes.fetch_sub(flate::ENCODER_HEAP, Ordering::Relaxed);
				self.state.fetch_sub(flate::ENCODER_HEAP, Ordering::Relaxed);
				self.footprint_fixed(track, flate::ENCODER_HEAP, false);
				// Only the track's end frees a spare.
				track.pool.pin(flate::ENCODER_HEAP);
				*spare = Some(encoder);
				return;
			}
		}
		self.sub_state(flate::ENCODER_HEAP);
	}

	/// Bill `n` more bytes of codec state, which the session's live edges do not count.
	pub(super) fn add_state(&self, n: u64) {
		if let Some(track) = &self.track {
			track.pool.add(n);
			track.written.fetch_add(n, Ordering::Relaxed);
			self.bytes.fetch_add(n, Ordering::Relaxed);
			self.state.fetch_add(n, Ordering::Relaxed);
			self.footprint_fixed(track, n, true);
		}
	}

	/// Release `n` billed bytes of codec state.
	pub(super) fn sub_state(&self, n: u64) {
		if let Some(track) = &self.track {
			let prior = self
				.state
				.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |state| {
					Some(state.saturating_sub(n))
				})
				.expect("always updates");
			self.bytes.fetch_sub(prior.min(n), Ordering::Relaxed);
			track.pool.sub(prior.min(n));
			self.footprint_fixed(track, prior.min(n), false);
		}
	}

	/// Bill `n` more bytes.
	pub(super) fn add(&self, n: u64) {
		if let Some(track) = &self.track {
			track.pool.add(n);
			track.written.fetch_add(n, Ordering::Relaxed);
			self.bytes.fetch_add(n, Ordering::Relaxed);
			self.footprint(track, n, true);
		}
	}

	/// Settle the track's eviction debt, taking its lock: call with no group lock held.
	pub(super) fn settle(&self) {
		if let Some(track) = &self.track {
			track.settle(None);
		}
	}

	/// Bytes currently billed.
	pub(super) fn size(&self) -> u64 {
		self.bytes.load(Ordering::Relaxed)
	}

	/// Release `n` billed bytes.
	pub(super) fn sub(&self, n: u64) {
		if let Some(track) = &self.track {
			let prior = self
				.bytes
				.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |bytes| {
					Some(bytes.saturating_sub(n))
				})
				.expect("always updates");
			track.pool.sub(prior.min(n));
			self.footprint(track, prior.min(n), false);
		}
	}

	/// Release every billed byte.
	pub(super) fn clear(&self) {
		if let Some(track) = &self.track {
			let bytes = self.bytes.swap(0, Ordering::Relaxed);
			let state = self.state.swap(0, Ordering::Relaxed);
			track.pool.sub(bytes);
			self.footprint(track, bytes.saturating_sub(state), false);
			self.footprint_fixed(track, state, false);
		}
	}
}

impl Drop for Extra {
	fn drop(&mut self) {
		self.clear();
	}
}

#[cfg(test)]
mod test {
	use super::*;

	fn charge(pool: &Pool) -> Charge {
		// No track behind the account: nothing here settles debt, it just accounts.
		Track::new(pool.clone(), Default::default(), Default::default(), kio::Weak::new()).charge()
	}

	fn bounded(capacity: u64) -> Pool {
		let config = Config::default().with_capacity(capacity).with_expiry(DEFAULT_EXPIRY);
		Pool::new(config)
	}

	/// Reuse transfers existing heap, concurrent users allocate separately, and only
	/// one finished encoder stays parked until the account's final owner drops.
	#[test]
	fn encoder_spare_transfers_its_charge_without_write_debt() {
		let pool = Pool::unbounded();
		let charge = charge(&pool);
		let first = charge.extra();
		let second = charge.extra();
		let account = charge.track.as_ref().unwrap();
		account.take_written();
		let encoder = first.take_encoder();
		let concurrent = second.take_encoder();
		assert_eq!(pool.used(), ENTRY_OVERHEAD + 2 * flate::ENCODER_HEAP);
		assert_eq!(account.take_written(), 2 * flate::ENCODER_HEAP);

		first.park_encoder(encoder);
		assert_eq!(first.size(), 0);
		assert_eq!(pool.used(), ENTRY_OVERHEAD + 2 * flate::ENCODER_HEAP);
		let encoder = first.take_encoder();
		assert_eq!(first.size(), flate::ENCODER_HEAP);
		assert_eq!(account.take_written(), 0, "reuse must not inflate write debt");
		first.park_encoder(encoder);
		second.park_encoder(concurrent);
		assert_eq!(second.size(), 0);
		assert_eq!(pool.used(), ENTRY_OVERHEAD + flate::ENCODER_HEAP, "only one spare");

		// A group and its inflated copy may outlive the track. They still own its
		// account, so the parked encoder stays billed until that last owner drops.
		drop(charge);
		assert_eq!(pool.used(), flate::ENCODER_HEAP);
		drop(first);
		assert_eq!(pool.used(), flate::ENCODER_HEAP);
		drop(second);
		assert_eq!(pool.used(), 0);
	}

	struct Flag(std::sync::atomic::AtomicBool);

	impl std::task::Wake for Flag {
		fn wake(self: Arc<Self>) {
			self.0.store(true, Ordering::SeqCst);
		}
	}

	fn flag() -> (Arc<Flag>, kio::Waiter) {
		let flag = Arc::new(Flag(Default::default()));
		let waiter = kio::Waiter::new(std::task::Waker::from(flag.clone()));
		(flag, waiter)
	}

	/// A wall-clock deadline armed after the driver collected wakes it to collect again by
	/// then, and that collection releases the read parked on it. Before any collection,
	/// nothing would release it, so the read waits for the first one to arm it then.
	#[test]
	fn wall_deadlines_wake_the_driver_and_then_the_read() {
		let pool = Pool::unbounded();
		let track = Track::new(pool.clone(), Default::default(), Default::default(), kio::Weak::new());
		let now = super::super::clock::wall();
		let at = now.saturating_add(Duration::from_secs(1));
		let (read, reading) = flag();
		track.watch_wall(at, &reading);

		let (driver, driving) = flag();
		assert_eq!(pool.poll(now, &driving), None, "an undriven pool indexes nothing");
		assert!(
			read.0.load(Ordering::SeqCst),
			"the first collection never woke the read"
		);
		read.0.store(false, Ordering::SeqCst);
		track.watch_wall(at, &reading);
		assert!(
			driver.0.load(Ordering::SeqCst),
			"the driver slept through an earlier deadline"
		);
		assert_eq!(pool.poll(now, &driving), Some(at));

		pool.poll(at.saturating_sub(Duration::from_millis(1)), &driving);
		assert!(!read.0.load(Ordering::SeqCst), "released early");
		assert_eq!(pool.poll(at, &driving), None);
		assert!(read.0.load(Ordering::SeqCst), "the read was never released");
	}

	/// A fresh driver must hear deadlines armed after an empty collection, including
	/// after the previous driver was dropped or a prior deadline drained its wake list.
	#[test]
	fn replacement_drivers_hear_new_deadlines() {
		let pool = Pool::unbounded();
		let now = super::super::clock::wall();
		pool.poll(now, &kio::Waiter::noop());
		let track = Track::new(pool.clone(), Default::default(), Default::default(), kio::Weak::new());
		let (read, reading) = flag();
		let (driver, driving) = flag();
		assert_eq!(pool.poll(now, &driving), None);
		let at = now.saturating_add(Duration::from_secs(1));
		track.watch_wall(at, &reading);
		assert!(driver.0.swap(false, Ordering::SeqCst));
		assert_eq!(pool.poll(at, &driving), None);
		assert!(read.0.swap(false, Ordering::SeqCst));
		track.watch_wall(at.saturating_add(Duration::from_secs(1)), &reading);
		assert!(driver.0.load(Ordering::SeqCst), "the drained list was never rearmed");
	}

	/// Wall time can lead a lagging driver's sample, while another driver can supply
	/// future time. Neither a new driver nor a backward sample may rewind expiration.
	#[kio::test]
	async fn wall_deadlines_use_the_later_of_ambient_and_driven_time() {
		let pool = Pool::unbounded();
		let now = kio::clock::now();
		let waiter = kio::Waiter::noop();
		pool.poll(now, &waiter);
		let track = Track::new(pool.clone(), Default::default(), Default::default(), kio::Weak::new());
		let (read, reading) = flag();
		let at = now.saturating_add(Duration::from_secs(1));
		track.watch_wall(at, &reading);
		kio::clock::sleep(Duration::from_secs(1)).await;
		assert_eq!(pool.poll(now, &waiter), None);
		assert!(read.0.swap(false, Ordering::SeqCst), "ambient time was ignored");

		let future = at.saturating_add(Duration::from_secs(1));
		track.watch_wall(future, &reading);
		assert_eq!(pool.poll(future, &waiter), None);
		assert!(read.0.swap(false, Ordering::SeqCst), "driven time was ignored");
		let tick = pool.now();
		pool.poll(now, &waiter);
		assert_eq!(pool.now_wall(), future);
		assert_eq!(pool.now(), tick);
	}

	/// A track indexes one wall-clock deadline, its earliest, and takes it along when dropped.
	#[test]
	fn a_dropped_track_leaves_the_wall_index() {
		let pool = Pool::unbounded();
		let now = super::super::clock::wall();
		pool.poll(now, &kio::Waiter::noop());
		let track = Track::new(pool.clone(), Default::default(), Default::default(), kio::Weak::new());
		let (_read, reading) = flag();
		track.watch_wall(now.saturating_add(Duration::from_secs(2)), &reading);
		track.watch_wall(now.saturating_add(Duration::from_secs(1)), &reading);
		assert_eq!(pool.inner.wall.lock().due.len(), 1, "the later deadline lingers");

		drop(track);
		assert!(pool.inner.wall.lock().due.is_empty(), "the dropped track lingers");
	}

	#[test]
	fn unbounded_never_accrues() {
		let pool = Pool::unbounded();
		let mut charge = charge(&pool);
		charge.add(1 << 40);
		assert_eq!(pool.accrue(1 << 30), None);
		assert_eq!(pool.used(), (1 << 40) + ENTRY_OVERHEAD);
		drop(charge);
		assert_eq!(pool.used(), 0);
	}

	#[test]
	fn weak_follows_the_last_strong_handle() {
		let pool = bounded(1000);
		let clone = pool.clone();
		let weak = pool.downgrade();

		drop(pool);
		let upgraded = weak.upgrade().expect("a strong handle remains");
		assert!(upgraded.same_pool(&clone));

		drop(upgraded);
		drop(clone);
		assert!(weak.upgrade().is_none());
	}

	#[test]
	fn accrue_none_under_capacity() {
		let pool = bounded(ENTRY_OVERHEAD + 1000);
		let mut charge = charge(&pool);
		charge.add(500);
		assert_eq!(pool.accrue(100), None);
	}

	#[test]
	fn accrue_proportional_over_capacity() {
		let pool = bounded(1000);
		let mut charge = charge(&pool);
		charge.add(2000 - ENTRY_OVERHEAD); // used = 2000, twice the capacity

		// Debt exceeds what was written by the overshoot ratio, so the pool drains.
		assert_eq!(pool.accrue(100), Some(200));
		// Zero written accrues zero: an idle track takes on no debt.
		assert_eq!(pool.accrue(0), Some(0));
	}

	#[test]
	fn average_tracks_evictable_population() {
		let pool = bounded(1000);
		assert_eq!(pool.average(), None);

		pool.access_insert(10);
		pool.access_insert(20);
		assert_eq!(pool.average(), Some(15));

		// A refresh moves one member's contribution, exactly.
		pool.access_refresh(10, 40);
		assert_eq!(pool.average(), Some(30));

		pool.access_remove(40);
		assert_eq!(pool.average(), Some(20));
		pool.access_remove(20);
		assert_eq!(pool.average(), None);
	}

	#[test]
	fn charge_raii() {
		let pool = bounded(1000);
		let mut charge = charge(&pool);
		assert_eq!(pool.used(), ENTRY_OVERHEAD);

		charge.add(100);
		assert_eq!(pool.used(), ENTRY_OVERHEAD + 100);

		charge.clear();
		assert_eq!(pool.used(), 0);
		// Idempotent: a second clear (and the eventual drop) releases nothing more.
		charge.clear();
		drop(charge);
		assert_eq!(pool.used(), 0);
	}

	#[test]
	fn detached_charge_is_noop() {
		let mut charge = Charge::default();
		charge.add(123);
		charge.clear();
	}

	#[test]
	fn accrue_saturates() {
		// A huge overshoot against a tiny capacity must saturate, not wrap.
		let pool = bounded(1);
		let mut c = charge(&pool);
		c.add(1 << 40);
		assert_eq!(pool.accrue(1 << 40), Some(u64::MAX));
	}

	/// A live edge pins its fixed costs at once and its content once it finished, moving the
	/// pins with every byte it or its extras gain or shed, until it is demoted. A pool held past
	/// its capacity by pins alone has no excess, and no room to give.
	#[test]
	fn a_live_edge_pins_what_no_eviction_reclaims() {
		let pool = bounded(0);
		let track = Track::new(pool.clone(), Default::default(), Default::default(), kio::Weak::new());

		let mut charge = track.charge();
		charge.add(1_000);
		assert_eq!(pool.pinned(), 0, "not the live edge yet");
		charge.promote();
		assert_eq!(
			pool.pinned(),
			ENTRY_OVERHEAD,
			"a growing edge may still shed its content"
		);
		let extra = charge.extra();
		extra.add(500);
		extra.add_state(100);
		assert_eq!(pool.pinned(), ENTRY_OVERHEAD + 100);
		charge.finish(true);
		assert_eq!(pool.pinned(), ENTRY_OVERHEAD + 1_600);
		assert_eq!(pool.pinned(), pool.used());
		assert_eq!(pool.excess(), None, "held past its capacity by pins alone");
		assert_eq!(pool.room(), 0);
		assert_eq!(pool.accrue(1_000), None);

		charge.sub(400);
		extra.sub_state(100);
		assert_eq!(pool.pinned(), ENTRY_OVERHEAD + 1_100);
		let mut cold = track.charge();
		cold.add(200);
		cold.demote();
		assert_eq!(
			pool.excess(),
			Some(ENTRY_OVERHEAD + 200),
			"only what eviction can reclaim"
		);

		charge.demote();
		assert_eq!(pool.pinned(), 0);
		drop(extra);
		drop(charge);
		drop(cold);
		assert_eq!(pool.used(), 0);
		assert_eq!(pool.pinned(), 0);
	}

	/// A copy's content pins once any sibling sharing it finished the whole group, since its
	/// frames may be what the finished copy holds; a copy a transport bound cut does not.
	#[test]
	fn a_finished_sibling_pins_the_shared_content() {
		let pool = bounded(0);
		let track = Track::new(pool.clone(), Default::default(), Default::default(), kio::Weak::new());
		let siblings = Siblings::default();
		let mut live = track.charge();
		live.add(1_000);
		live.promote();
		siblings.update(&live);
		let mut head = track.charge();
		siblings.update(&head);
		assert_eq!(pool.pinned(), 2 * ENTRY_OVERHEAD, "the shared frames still grow");
		head.finish(false);
		siblings.update(&head);
		assert_eq!(
			pool.pinned(),
			2 * ENTRY_OVERHEAD,
			"a copy a transport bound cut finished only its range"
		);
		head.finish(true);
		siblings.update(&head);
		assert_eq!(pool.pinned(), 2 * ENTRY_OVERHEAD + 1_000);
		assert_eq!(pool.excess(), None);
	}

	/// A live edge bills its session for every byte it holds, its extras included (shared
	/// content a transfer moved out, inflated copies, codec state), whichever order they and
	/// the promotion come in, and stops on demotion or abort, its extras outliving it or not.
	#[test]
	fn a_live_edge_bills_its_extras_to_the_session() {
		let edges = Edges::new(Some(1 << 30));
		let track = Track::new(Pool::unbounded(), Default::default(), edges.clone(), kio::Weak::new());

		let mut charge = track.charge();
		charge.add(100);
		let early = charge.extra();
		early.add(10);
		assert_eq!(edges.used(), 0, "not the live edge yet");
		charge.promote();
		assert_eq!(edges.used(), 110, "the overhead is left out");
		let shared = charge.transfer(60);
		let late = charge.extra();
		late.add(5);
		assert_eq!(edges.used(), 115, "a transfer keeps its bytes billed");
		shared.sub(20);
		assert_eq!(edges.used(), 95);
		charge.demote();
		assert_eq!(edges.used(), 0);
		drop((shared, late));
		assert_eq!(edges.used(), 0);

		// Aborted while its extras live on: they stop billing with it.
		let mut charge = track.charge();
		let extra = charge.extra();
		extra.add(10);
		charge.promote();
		charge.clear();
		assert_eq!(edges.used(), 0);
		drop(extra);
		assert_eq!(edges.used(), 0);
	}

	#[test]
	fn charge_counts_gross_writes() {
		let track = Track::new(bounded(1000), Default::default(), Default::default(), kio::Weak::new());
		let mut c = track.charge();
		c.add(100);
		assert_eq!(track.take_written(), ENTRY_OVERHEAD + 100);
		assert_eq!(track.take_written(), 0, "taking it drains the counter");
	}

	#[test]
	fn charge_owns_access_sample() {
		let pool = bounded(1000);
		let mut c = charge(&pool);
		assert_eq!(pool.average(), None, "not evictable until demoted");

		c.demote();
		c.demote(); // idempotent
		assert!(pool.average().is_some());

		// Clearing (an abort, from anyone) removes the sample with the bytes.
		c.clear();
		assert_eq!(pool.average(), None, "aborted groups leave no ghost sample");
		drop(c);
		assert_eq!(pool.average(), None);
	}

	#[test]
	fn refresh_updates_a_counted_sample() {
		let pool = bounded(1000);
		let mut c = charge(&pool);
		c.demote();
		c.refresh();
		// The sample in the pool mean moved with the stamp, so releasing the charge
		// removes exactly what was inserted and leaves no residue.
		assert_eq!(pool.average(), Some(c.accessed()));
		c.clear();
		assert_eq!(pool.average(), None);
	}

	#[test]
	fn refresh_protects_within_a_tick() {
		let pool = bounded(1000);
		let mut c = charge(&pool);
		c.demote();
		let average = pool.average().unwrap();
		// A refresh in the same coarse tick still lifts the group above the mean.
		c.refresh();
		assert!(c.accessed() > average);
		assert_eq!(c.access().tick(None), None, "undated access is protected until cleanup");
		assert_eq!(
			c.access().tick(Some(pool.now())),
			Some(pool.now()),
			"cleanup dates the access"
		);
		// Repeated same-tick refreshes are idempotent, not runaway.
		let stamped = c.accessed();
		c.refresh();
		assert_eq!(c.accessed(), stamped);
	}

	#[test]
	fn expiry_config() {
		// A bare pool preserves the unbounded contract in both dimensions.
		let pool = Pool::unbounded();
		assert_eq!(pool.expiry(), None);

		let pool = Pool::new(Config::default().with_expiry(Duration::from_secs(1)));
		assert_eq!(pool.expiry(), Some(Duration::from_secs(1)));
		assert_eq!(pool.expiry_ticks(), 10);

		let pool = Pool::new(Config::default().with_expiry(Duration::from_millis(1)));
		assert_eq!(pool.expiry(), Some(Duration::from_millis(TICK_MS)));
		assert_eq!(pool.expiry_ticks(), 1);

		// Disabled: never reclaimed by idleness, but readers still re-stamp on a
		// bounded cadence for byte-eviction protection.
		let pool = Pool::new(Config::default());
		assert_eq!(pool.expiry(), None);
		assert_eq!(pool.expiry_ticks(), u64::MAX);
	}

	#[test]
	fn expiry_gate_reuses_a_supplied_tick() {
		let pool = Pool::new(Config::default().with_expiry(Duration::from_secs(1)));
		let track = Track::new(pool, Default::default(), Default::default(), kio::Weak::new());
		// A supplied tick drives the gate on its own: claimed immediately, closed
		// until the interval elapses, claimable again on the tick it reopens.
		assert!(track.expiry_due(Some(0)));
		assert!(!track.expiry_due(Some(9)));
		assert!(track.expiry_due(Some(10)));
		// Without one the gate samples the pool clock, frozen here at tick 0, so it
		// stays closed rather than inheriting the caller's tick 10.
		assert!(!track.expiry_due(None));
	}

	#[test]
	fn sweep_interval_is_half_the_window() {
		assert_eq!(Pool::unbounded().sweep_interval(), None);
		let pool = Pool::new(Config::default().with_expiry(Duration::from_secs(4)));
		assert_eq!(pool.sweep_interval(), Some(Duration::from_secs(2)));
	}

	#[test]
	fn the_sweep_registry_follows_account_lifetime() {
		let pool = bounded(1000);
		assert!(pool.inner.tracks.lock().is_empty());

		let track = Track::new(pool.clone(), Default::default(), Default::default(), kio::Weak::new());
		assert_eq!(pool.inner.tracks.lock().len(), 1);

		// A registered account whose track is already gone is swept harmlessly.
		pool.sweep();

		drop(track);
		assert!(pool.inner.tracks.lock().is_empty(), "a dropped account leaves no entry");
	}

	#[test]
	fn an_inert_pool_registers_nothing() {
		// No window means no sweep, so a bare pool pays no registry cost.
		let pool = Pool::unbounded();
		let track = Track::new(pool.clone(), Default::default(), Default::default(), kio::Weak::new());
		assert!(pool.inner.tracks.lock().is_empty());
		pool.sweep();
		drop(track);
	}

	#[test]
	fn expiry_gate_stays_closed_without_a_window() {
		// No window means no time gate, and no clock read to reach it.
		let track = Track::new(
			Pool::unbounded(),
			Default::default(),
			Default::default(),
			kio::Weak::new(),
		);
		assert!(!track.expiry_due(None));
		assert!(!track.expiry_due(Some(u64::MAX)));
	}

	#[test]
	fn collecting_before_the_deadline_does_not_postpone_it() {
		let pool = Pool::new(Config::default().with_expiry(Duration::from_secs(2)));
		let now = crate::time::Instant::now();
		let deadline = pool.poll(now, &kio::Waiter::noop());
		assert_eq!(
			pool.poll(now.saturating_add(Duration::from_millis(500)), &kio::Waiter::noop()),
			deadline
		);
	}

	#[test]
	fn bounded_pools_sample_recency_without_expiration() {
		let pool = Pool::unbounded();
		let now = crate::time::Instant::now();
		assert_eq!(pool.poll(now, &kio::Waiter::noop()), None);
		pool.resize(1024);
		assert_eq!(
			pool.poll(now, &kio::Waiter::noop()),
			Some(now.saturating_add(DEFAULT_EXPIRY / 2))
		);
		pool.poll(now.saturating_add(DEFAULT_EXPIRY), &kio::Waiter::noop());
		assert!(pool.now() > 0);
		pool.resize(None);
		assert_eq!(
			pool.poll(now.saturating_add(DEFAULT_EXPIRY), &kio::Waiter::noop()),
			None
		);
	}

	#[test]
	fn resize() {
		let pool = Pool::unbounded();
		assert_eq!(pool.capacity(), None);

		let mut charge = charge(&pool);
		charge.add(1000);

		// Shrinking doesn't reclaim anything synchronously; writers accrue debt instead.
		pool.resize(100);
		assert_eq!(pool.capacity(), Some(100));
		assert!(pool.used() > 100);
		assert!(pool.accrue(50).unwrap() > 50);

		pool.resize(None);
		assert_eq!(pool.capacity(), None);
		assert_eq!(pool.accrue(50), None);
	}
}
