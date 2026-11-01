//! A shared byte budget for cached groups and buffered datagrams, repaid by write-time eviction.
//!
//! Every group charges its cached bytes into a [`Pool`] through a crate-internal
//! `Charge`, billed to its track's `Track` account. A track's datagram send buffer bills
//! each datagram it holds the same way, both payload forms of a compressed one included,
//! and pays debt by dropping its oldest once the groups' payment falls short. The pool itself never evicts: it is
//! a handful of atomic counters. While the pool is over capacity, each track accrues
//! eviction debt as it writes (`accrue`),
//! sized proportionally to what it wrote, and pays that debt by aborting its own oldest
//! groups with [`Error::Evicted`](crate::Error::Evicted). Reclamation is therefore
//! distributed across every writing track and converges on the capacity without any
//! global eviction task.
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
//! A bare pool is inert by default ([`Pool::unbounded`]): publishers and subscribers
//! that never set a capacity or expiry pay only a couple of atomic counters, and
//! register nothing. A standalone [`origin`](crate::origin::Config) enables
//! [`DEFAULT_EXPIRY`], while a relay creates one configured pool and shares it across
//! every origin so the whole process caches into a single policy.

use crate::util::SendCell;
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock, PoisonError, Weak};
use std::task::{Poll, ready};
use std::time::Duration;

use super::track::{self, Position, TrackState};
use super::{aborts, expiry, group};

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
/// The pool tracks how many payload bytes are cached across every registered group and
/// datagram buffer, plus the mean last-access time of the evictable groups. It never
/// evicts on its own: tracks accrue eviction debt as they write and evict their own
/// oldest groups and datagrams to pay it. Idle expiration runs during [`Self::poll`]. The capacity is
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

	/// Bytes currently cached across every registered group and datagram buffer.
	pub fn used(&self) -> u64 {
		self.inner.used.load(Ordering::Relaxed)
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

	/// Drop held heads, oldest first, while the pool is over capacity.
	pub(super) fn reclaim_held(&self) {
		if self.used() > self.inner.capacity.load(Ordering::Relaxed) {
			let mut held = self.inner.held.lock().unwrap_or_else(PoisonError::into_inner);
			self.prune_held(&mut held);
		}
	}

	/// Drop the held heads past the idle window, then the oldest while the pool is over
	/// capacity, and forget the hand-offs holding none.
	fn prune_held(&self, held: &mut Vec<Weak<group::Handoff>>) {
		held.retain(|handoff| {
			let Some(handoff) = handoff.upgrade() else {
				return false;
			};
			let over = self.used() > self.inner.capacity.load(Ordering::Relaxed);
			!handoff.expire(over)
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
				let driven = *epoch + Duration::from_micros(self.inner.wall_micros.load(Ordering::Relaxed));
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
		self.date(now + duration);
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
	/// the pool is under capacity (the caller should forget any outstanding debt).
	///
	/// The debt is `written * used / capacity`, so paying it evicts slightly more
	/// than was written and the overshoot decays toward the capacity. Tracks double
	/// it when their oldest content is staler than [`Self::average`]. Saturates: a
	/// tiny capacity must not wrap a huge debt into a small one.
	pub(super) fn accrue(&self, written: u64) -> Option<u64> {
		let used = self.inner.used.load(Ordering::Relaxed);
		let capacity = self.inner.capacity.load(Ordering::Relaxed);
		if used <= capacity {
			return None;
		}
		let debt = written as u128 * used as u128 / capacity.max(1) as u128;
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

	// What the frames this track's groups are still receiving may allocate up front,
	// shared with every track minted from the same origin handle.
	frames: super::frame::Budget,
}

/// The greatest location a track has written or its source reported, followed by the track
/// handles' `largest`: each new group or datagram at its head, then each frame written to it.
#[derive(Default)]
struct Largest {
	position: Option<Position>,
	// The largest group's content may still grow: no copy of the whole group finished, and it
	// is not a datagram. An aborted one stays open, since it can be recreated or fetched.
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

	/// Group `sequence` holds every frame it will have: a whole copy finished, or it is a
	/// datagram. A group name fixes its content, so no legal write raises the largest past it.
	pub(super) fn finish_largest(&self, sequence: u64) {
		let mut largest = self.largest.lock();
		if largest.position.is_some_and(|position| position.group == sequence) {
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
	/// allocating up front within `frames`.
	pub(super) fn new(pool: Pool, frames: super::frame::Budget, state: kio::Weak<TrackState>) -> Arc<Self> {
		let track = Arc::new(Self {
			pool,
			frames,
			written: AtomicU64::new(0),
			next_expiry: AtomicU64::new(0),
			expiry_cursor: AtomicUsize::new(0),
			state,
			sweep: OnceLock::new(),
			wakes: Default::default(),
			aborts: OnceLock::new(),
			largest: Default::default(),
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

	/// The track's reads parked on their drift budget.
	pub(super) fn wakes(&self) -> &expiry::Wakes {
		&self.wakes
	}

	/// Watch every group the publisher cuts, rather than chosen sequences.
	pub(super) fn watch_cuts(&self) -> aborts::Watch {
		self.aborts.get_or_init(Arc::default).watch_cuts()
	}

	/// Notify watchers that group `sequence` aborted, if the publisher cut it.
	pub(super) fn aborted(&self, sequence: u64, cut: bool) {
		if cut && let Some(aborts) = self.aborts.get() {
			aborts.notify(sequence);
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
		}
	}

	/// Bill `bytes` of a failed group's head while a hand-off holds it for a successor route.
	///
	/// Readers that stop polling can hold a hand-off indefinitely, so the head is billed
	/// like cached content and bounded the same way: `None` when it would take the pool past
	/// its capacity, and [`Pool::hold_head`] has the pool reclaim it like idle content.
	pub(super) fn hold(self: &Arc<Self>, bytes: u64) -> Option<Bill> {
		if self
			.pool
			.capacity()
			.is_some_and(|capacity| self.pool.used().saturating_add(bytes) > capacity)
		{
			return None;
		}
		self.pool.add(bytes);
		Some(Bill {
			track: self.clone(),
			bytes,
			at: self.pool.now(),
		})
	}

	/// Bill `bytes` of a buffered datagram, which accrue eviction debt like a group's frames.
	pub(super) fn bill(self: &Arc<Self>, bytes: u64) -> Bill {
		self.pool.add(bytes);
		self.written.fetch_add(bytes, Ordering::Relaxed);
		Bill {
			track: self.clone(),
			bytes,
			at: self.pool.now(),
		}
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

	/// Settle from [`Pool::sweep`], dating activity and expiring every idle candidate.
	pub(super) fn sweep(&self) {
		self.settle_inner(None, true);
	}

	fn settle_inner(&self, now: Option<u64>, full: bool) {
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
			// groups and datagrams: the sweep expires them in place, or they would never go.
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

/// The bytes a held head of a failed group or a buffered datagram bills to its track's
/// pool, released on drop.
pub(super) struct Bill {
	track: Arc<Track>,
	bytes: u64,
	// The coarse tick it was billed at.
	at: u64,
}

impl Bill {
	/// What it bills the pool.
	pub(super) fn bytes(&self) -> u64 {
		self.bytes
	}

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
			self.bytes += n;
		}
		self.touch(WRITE_BOOST)
	}

	/// Release `n` charged bytes the group dropped while still cached, without an access.
	pub(super) fn sub(&mut self, n: u64) {
		if let Some(track) = &self.track {
			let n = n.min(self.bytes);
			track.pool.sub(n);
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
		Extra {
			track: self.track.clone(),
			bytes: AtomicU64::new(bytes),
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
			bytes: AtomicU64::new(0),
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
		if let Some(track) = &self.track {
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

/// Bytes a group keeps beside its cached frames, billed to the same pool and account as
/// its [`Charge`] but reachable outside the group's write lock: a compressed group's
/// inflated copy, which its readers build, and its codec state. Released on [`Self::clear`] or drop.
///
/// Its owner serializes the changes; the count is atomic so the eviction walk can size
/// the group without that owner's lock.
#[derive(Default)]
pub(super) struct Extra {
	track: Option<Arc<Track>>,
	bytes: AtomicU64,
}

impl Extra {
	/// Bill `n` more bytes.
	pub(super) fn add(&self, n: u64) {
		if let Some(track) = &self.track {
			track.pool.add(n);
			track.written.fetch_add(n, Ordering::Relaxed);
			self.bytes.fetch_add(n, Ordering::Relaxed);
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
		}
	}

	/// Release every billed byte.
	pub(super) fn clear(&self) {
		if let Some(track) = &self.track {
			track.pool.sub(self.bytes.swap(0, Ordering::Relaxed));
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
		Track::new(pool.clone(), Default::default(), kio::Weak::new()).charge()
	}

	fn bounded(capacity: u64) -> Pool {
		let config = Config::default().with_capacity(capacity).with_expiry(DEFAULT_EXPIRY);
		Pool::new(config)
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
		let track = Track::new(pool.clone(), Default::default(), kio::Weak::new());
		let now = super::super::clock::wall();
		let at = now + Duration::from_secs(1);
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

		pool.poll(at - Duration::from_millis(1), &driving);
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
		let track = Track::new(pool.clone(), Default::default(), kio::Weak::new());
		let (read, reading) = flag();
		let (driver, driving) = flag();
		assert_eq!(pool.poll(now, &driving), None);
		let at = now + Duration::from_secs(1);
		track.watch_wall(at, &reading);
		assert!(driver.0.swap(false, Ordering::SeqCst));
		assert_eq!(pool.poll(at, &driving), None);
		assert!(read.0.swap(false, Ordering::SeqCst));
		track.watch_wall(at + Duration::from_secs(1), &reading);
		assert!(driver.0.load(Ordering::SeqCst), "the drained list was never rearmed");
	}

	/// Wall time can lead a lagging driver's sample, while another driver can supply
	/// future time. Neither a new driver nor a backward sample may rewind expiration.
	#[moq_clock::test]
	async fn wall_deadlines_use_the_later_of_ambient_and_driven_time() {
		let pool = Pool::unbounded();
		let now = moq_clock::now();
		let waiter = kio::Waiter::noop();
		pool.poll(now, &waiter);
		let track = Track::new(pool.clone(), Default::default(), kio::Weak::new());
		let (read, reading) = flag();
		let at = now + Duration::from_secs(1);
		track.watch_wall(at, &reading);
		moq_clock::sleep(Duration::from_secs(1)).await;
		assert_eq!(pool.poll(now, &waiter), None);
		assert!(read.0.swap(false, Ordering::SeqCst), "ambient time was ignored");

		let future = at + Duration::from_secs(1);
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
		let track = Track::new(pool.clone(), Default::default(), kio::Weak::new());
		let (_read, reading) = flag();
		track.watch_wall(now + Duration::from_secs(2), &reading);
		track.watch_wall(now + Duration::from_secs(1), &reading);
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

	#[test]
	fn charge_counts_gross_writes() {
		let track = Track::new(bounded(1000), Default::default(), kio::Weak::new());
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
		let track = Track::new(pool, Default::default(), kio::Weak::new());
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

		let track = Track::new(pool.clone(), Default::default(), kio::Weak::new());
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
		let track = Track::new(pool.clone(), Default::default(), kio::Weak::new());
		assert!(pool.inner.tracks.lock().is_empty());
		pool.sweep();
		drop(track);
	}

	#[test]
	fn expiry_gate_stays_closed_without_a_window() {
		// No window means no time gate, and no clock read to reach it.
		let track = Track::new(Pool::unbounded(), Default::default(), kio::Weak::new());
		assert!(!track.expiry_due(None));
		assert!(!track.expiry_due(Some(u64::MAX)));
	}

	#[test]
	fn collecting_before_the_deadline_does_not_postpone_it() {
		let pool = Pool::new(Config::default().with_expiry(Duration::from_secs(2)));
		let now = crate::time::Instant::now();
		let deadline = pool.poll(now, &kio::Waiter::noop());
		assert_eq!(
			pool.poll(now + Duration::from_millis(500), &kio::Waiter::noop()),
			deadline
		);
	}

	#[test]
	fn bounded_pools_sample_recency_without_expiration() {
		let pool = Pool::unbounded();
		let now = crate::time::Instant::now();
		assert_eq!(pool.poll(now, &kio::Waiter::noop()), None);
		pool.resize(1024);
		assert_eq!(pool.poll(now, &kio::Waiter::noop()), Some(now + DEFAULT_EXPIRY / 2));
		pool.poll(now + DEFAULT_EXPIRY, &kio::Waiter::noop());
		assert!(pool.now() > 0);
		pool.resize(None);
		assert_eq!(pool.poll(now + DEFAULT_EXPIRY, &kio::Waiter::noop()), None);
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
