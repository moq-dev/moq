//! A shared byte budget for cached groups, repaid by write-time eviction.
//!
//! Every group charges its cached bytes into a [`Pool`] through a crate-internal
//! `Charge`, billed to its track's `Track` account. The pool itself never evicts: it is
//! a handful of atomic counters. While the pool is over capacity, each track accrues
//! eviction debt as it writes (`accrue`),
//! sized proportionally to what it wrote, and pays that debt by aborting its own oldest
//! groups with [`Error::Evicted`](crate::Error::Evicted). Reclamation is therefore
//! distributed across every writing track and converges on the capacity without any
//! global lock, registry, or background task.
//!
//! Cross-track ordering comes from one statistic: the mean last-access time of the
//! evictable population (every cached group except each track's protected latest).
//! A group accessed more recently than that mean is never evicted, so freshly read
//! or fetched content in one track can't die while another track holds staler
//! content, and a track
//! whose oldest group is staler than the mean accrues debt at double rate. Evicting
//! old entries and inserting new ones both advance the mean, so the eviction
//! frontier moves with cache turnover on its own.
//!
//! The pool also owns the wall-clock LRU window ([`Pool::expiry`]): a non-latest
//! group that nobody has read or written for that long is reclaimed by its track's
//! next write, no matter what retention its track advertises. Track retention
//! ([`max_age`](crate::track::Info::max_age)) is measured in media timestamps, so a
//! congestion stall can't age content out; the pool's expiry is the orthogonal
//! wall-clock bound that keeps unwatched content from pinning RAM.
//!
//! A bare pool is inert by default ([`Pool::unbounded`]): publishers and subscribers
//! that never set a capacity or expiry pay only a couple of atomic counters. A
//! standalone [`origin`](crate::origin::Info) enables [`DEFAULT_EXPIRY`], while a
//! relay creates one configured pool and shares it across every origin so the whole
//! process caches into a single policy.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use super::track::TrackState;

/// Fixed bookkeeping charged per cached group on top of its frame payload bytes.
///
/// Covers the group/track slot allocations so a track producing many tiny groups
/// (e.g. one frame per group) is billed roughly for its real footprint instead of
/// just its payload bytes. Also bounds the live group count (`used / 256`), which
/// keeps the access-time sum below u64 (see [`TICK_MS`]).
const ENTRY_OVERHEAD: u64 = 256;

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
/// ticks (6.3e9) times a 64 GiB target's worst-case ~270M groups is ~6.8e18 after
/// that encoding, still below half of `u64::MAX`. A byte-weighted mean would
/// overflow u64 even at whole-second ticks, which is why the mean is
/// count-weighted.
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
	/// A non-latest cached group that nobody reads or writes for this long is
	/// reclaimed by its track's next write, surfacing to any remaining reader as
	/// [`Error::Old`](crate::Error::Old). This is independent of track retention:
	/// [`max_age`](crate::track::Info::max_age) uses media timestamps, while this
	/// window keeps idle content from pinning memory. The value is fixed when the
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
/// plus the mean last-access time of the evictable ones. It never evicts on its own:
/// tracks accrue eviction debt as they write and evict their own oldest groups to
/// pay it, so every operation here is a few atomics with no lock. The capacity is
/// therefore a target usage converges toward, not a hard limit: carried debt, capped
/// payments, and the always-protected live edge all let usage transiently exceed it.
#[derive(Clone)]
pub struct Pool {
	inner: Arc<Inner>,
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
	epoch: crate::runtime::Instant,
	// Sum and count of last-access ticks across the evictable population, giving a
	// count-weighted mean. Tracks add a group when it becomes evictable (demoted
	// from the live edge, or inserted behind it) and remove it when it leaves.
	access_sum: AtomicU64,
	access_count: AtomicU64,
}

impl Pool {
	/// Create a pool from an initial policy.
	///
	/// The budget counts frame payload bytes (plus a small fixed overhead per
	/// group), not process RSS, and is a convergence target rather than a hard
	/// limit; leave headroom when sizing it from real memory. The expiry is fixed,
	/// while the capacity can later be changed with [`Self::resize`].
	pub fn new(config: Config) -> Self {
		let expiry = config.expiry.map_or(u64::MAX, |expiry| {
			let ms = u64::try_from(expiry.as_millis()).unwrap_or(u64::MAX);
			if ms == u64::MAX {
				return u64::MAX;
			}
			ms.max(1).div_ceil(TICK_MS).saturating_mul(TICK_MS)
		});
		Self {
			inner: Arc::new(Inner {
				used: AtomicU64::new(0),
				capacity: AtomicU64::new(config.capacity.unwrap_or(u64::MAX)),
				expiry,
				epoch: crate::model::clock::now(),
				access_sum: AtomicU64::new(0),
				access_count: AtomicU64::new(0),
			}),
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
	pub(crate) fn expiry_ticks(&self) -> u64 {
		match self.inner.expiry {
			u64::MAX => u64::MAX,
			ms => ms / TICK_MS,
		}
	}

	/// How often a reader on a lock-free path should re-stamp its group's access
	/// time: half the LRU window keeps the stamp comfortably inside it while staying
	/// rare on the hot path. Bounded even when expiry is disabled, since the stamp
	/// also protects the group from byte-budget eviction.
	pub(crate) fn refresh_interval(&self) -> Duration {
		self.expiry().unwrap_or(DEFAULT_EXPIRY) / 2
	}

	/// Returns true if both handles share the same underlying pool.
	pub fn same_pool(&self, other: &Self) -> bool {
		Arc::ptr_eq(&self.inner, &other.inner)
	}

	/// Charge `n` more cached bytes.
	pub(crate) fn add(&self, n: u64) {
		self.inner.used.fetch_add(n, Ordering::Relaxed);
	}

	/// Release `n` cached bytes.
	pub(crate) fn sub(&self, n: u64) {
		self.inner.used.fetch_sub(n, Ordering::Relaxed);
	}

	/// Coarse ticks since the pool was created: the clock access timestamps use.
	pub(crate) fn now(&self) -> u64 {
		let elapsed = crate::model::clock::now().duration_since(self.inner.epoch);
		elapsed.as_millis() as u64 / TICK_MS
	}

	/// Encode the current clock tick and an access-priority tie breaker.
	fn stamp(&self, boost: u64) -> u64 {
		self.now().saturating_mul(1 << ACCESS_SHIFT).saturating_add(boost)
	}

	/// Mean last-access tick across the evictable population, or `None` when it is
	/// empty. The sum and count are read separately, so the mean is approximate
	/// under concurrent updates; eviction only needs a rough frontier.
	pub(crate) fn average(&self) -> Option<u64> {
		let count = self.inner.access_count.load(Ordering::Relaxed);
		if count == 0 {
			return None;
		}
		Some(self.inner.access_sum.load(Ordering::Relaxed) / count)
	}

	/// A group with last-access tick `ts` joined the evictable population.
	pub(crate) fn access_insert(&self, ts: u64) {
		self.inner.access_sum.fetch_add(ts, Ordering::Relaxed);
		self.inner.access_count.fetch_add(1, Ordering::Relaxed);
	}

	/// A group with last-access tick `ts` left the evictable population.
	pub(crate) fn access_remove(&self, ts: u64) {
		self.inner.access_sum.fetch_sub(ts, Ordering::Relaxed);
		self.inner.access_count.fetch_sub(1, Ordering::Relaxed);
	}

	/// An evictable group's last-access tick moved from `old` to `new` (a FETCH hit).
	pub(crate) fn access_refresh(&self, old: u64, new: u64) {
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
	pub(crate) fn accrue(&self, written: u64) -> Option<u64> {
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
pub(crate) struct Track {
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
}

impl Track {
	/// Open an account against `pool` for the track behind `state`.
	pub(crate) fn new(pool: Pool, state: kio::Weak<TrackState>) -> Arc<Self> {
		Arc::new(Self {
			pool,
			written: AtomicU64::new(0),
			next_expiry: AtomicU64::new(0),
			expiry_cursor: AtomicUsize::new(0),
			state,
		})
	}

	/// The pool this track caches into.
	pub(crate) fn pool(&self) -> &Pool {
		&self.pool
	}

	/// Charge a new group's fixed overhead, returning its [`Charge`].
	pub(crate) fn charge(self: &Arc<Self>) -> Charge {
		self.pool.add(ENTRY_OVERHEAD);
		self.written.fetch_add(ENTRY_OVERHEAD, Ordering::Relaxed);
		let last = self.pool.stamp(0);
		Charge {
			track: Some(self.clone()),
			bytes: ENTRY_OVERHEAD,
			last: AtomicU64::new(last),
			counted: false,
		}
	}

	/// Take everything written since the last call, to be turned into eviction debt.
	pub(crate) fn take_written(&self) -> u64 {
		self.written.swap(0, Ordering::Relaxed)
	}

	/// Settle eviction debt and expire idle groups from a frame write.
	///
	/// Called with no group lock held (locks are ordered track then group). Cheap
	/// until the byte or time gate crosses: relaxed atomics plus a coarse clock read
	/// when expiry is enabled. This is what makes a track that only appends frames to
	/// open groups, never inserting another group, still pay its debt and age its
	/// idle content out.
	pub(crate) fn settle(&self) {
		let settle_debt = self.written.load(Ordering::Relaxed) >= WRITE_CHARGE_THRESHOLD;
		let scan_expiry = self.expiry_due();
		if !settle_debt && !scan_expiry {
			return;
		}
		// Counts as a producer while it lives, which is why `track::Producer` gates
		// its teardown on its own clone count rather than the state's.
		let Some(state) = self.state.upgrade() else { return };
		let expiry = if scan_expiry {
			let state = state.read();
			let scan = state.expiry_scan();
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
	pub(crate) fn next_expiry_scan(&self, width: usize) -> usize {
		self.expiry_cursor.fetch_add(width, Ordering::Relaxed)
	}

	/// Claim the next write-driven expiry scan when its time gate is due.
	fn expiry_due(&self) -> bool {
		let expiry = self.pool.expiry_ticks();
		if expiry == u64::MAX {
			return false;
		}

		let now = self.pool.now();
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
pub(crate) struct Charge {
	track: Option<Arc<Track>>,
	// Bytes currently charged, including ENTRY_OVERHEAD, released on drop.
	bytes: u64,
	// Tick of the last cache access: creation, every write, and every read (group
	// delivery, frame reads, FETCH hits, a fetched backfill's birth). Eviction
	// protection and age expiry key off this. Atomic so the read paths can stamp
	// it through a shared guard: a kio write guard's release notifies every parked
	// consumer, which a mere access must not do. Accesses are still serialized by
	// the owning state's lock, so `counted` can pair with it as a plain bool.
	last: AtomicU64,
	// Whether `last` is currently a sample in the pool's access mean, i.e. the
	// group is in the evictable population.
	counted: bool,
}

impl Charge {
	/// Charge `n` more payload bytes, counting them as written.
	///
	/// A write is also an access: it restarts the retention clock and keeps an
	/// actively-growing group (a straggler or backfill still being filled) from
	/// being evicted or expired mid-write, even within the same coarse tick as
	/// content that was merely inserted.
	pub(crate) fn add(&mut self, n: u64) {
		if let Some(track) = &self.track {
			track.pool.add(n);
			track.written.fetch_add(n, Ordering::Relaxed);
			self.bytes += n;
		}
		self.touch(WRITE_BOOST);
	}

	/// Release `n` payload bytes (a frame evicted by the group's own cap).
	pub(crate) fn sub(&mut self, n: u64) {
		if let Some(track) = &self.track {
			track.pool.sub(n);
			self.bytes = self.bytes.saturating_sub(n);
		}
	}

	/// The group's full cached footprint: payload bytes plus overhead.
	pub(crate) fn size(&self) -> u64 {
		self.bytes
	}

	/// Tick of the group's last cache access.
	pub(crate) fn accessed(&self) -> u64 {
		self.last.load(Ordering::Relaxed)
	}

	/// Coarse clock tick of the group's last cache access, without its priority bits.
	pub(crate) fn accessed_tick(&self) -> u64 {
		self.accessed() >> ACCESS_SHIFT
	}

	/// Enter the group into the evictable population (demoted from the live edge,
	/// or inserted behind it), sampling its access time into the pool's mean.
	/// Idempotent.
	pub(crate) fn demote(&mut self) {
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
	pub(crate) fn refresh(&self) {
		self.touch(READ_BOOST);
	}

	/// Record a write that charges no new bytes (a chunk written into an
	/// already-charged in-flight frame): restarts the retention clock like any
	/// other write. `&mut self` deliberately: reaching it through a kio write
	/// guard marks the guard modified, so its release wakes parked readers.
	pub(crate) fn record_write(&mut self) {
		self.touch(WRITE_BOOST);
	}

	/// Advance the last-access stamp to the current clock tick with `boost` priority.
	///
	/// The boost breaks ties within one coarse tick: written content outranks
	/// merely-inserted content, and explicitly read content outranks both, so a
	/// same-tick access still reads as strictly newer than the population mean of
	/// weaker accesses. Idempotent within a tick (monotone, never regressing), so
	/// repeated accesses remain idempotent without advancing the expiry clock.
	fn touch(&self, boost: u64) {
		let Some(track) = &self.track else { return };
		let target = track.pool.stamp(boost);
		// `fetch_max` keeps the stamp monotone, and its prior value makes the
		// paired mean update exact even for back-to-back accesses.
		let prev = self.last.fetch_max(target, Ordering::Relaxed);
		if target <= prev {
			return;
		}
		if self.counted {
			track.pool.access_refresh(prev, target);
		}
	}

	/// Release everything this charge holds: bytes, overhead, and the access
	/// sample. Idempotent; used when the group aborts and clears its frames.
	pub(crate) fn clear(&mut self) {
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

#[cfg(test)]
mod test {
	use super::*;

	fn charge(pool: &Pool) -> Charge {
		// No track behind the account: nothing here settles debt, it just accounts.
		Track::new(pool.clone(), kio::Weak::new()).charge()
	}

	fn bounded(capacity: u64) -> Pool {
		let config = Config::default().with_capacity(capacity).with_expiry(DEFAULT_EXPIRY);
		Pool::new(config)
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
	fn config_applies_capacity_and_expiry() {
		let pool = bounded(1000);
		assert_eq!(pool.capacity(), Some(1000));
		assert_eq!(pool.expiry(), Some(DEFAULT_EXPIRY));
	}

	#[test]
	fn accrue_none_under_capacity() {
		let pool = bounded(1000);
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
		charge.sub(40);
		assert_eq!(pool.used(), ENTRY_OVERHEAD + 60);

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
		charge.sub(23);
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
		let track = Track::new(bounded(1000), kio::Weak::new());
		let mut c = track.charge();
		c.add(100);
		c.sub(40); // releases don't refund the gross counter
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
		assert_eq!(c.accessed_tick(), pool.now(), "priority does not advance expiry time");
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
		assert_eq!(pool.refresh_interval(), DEFAULT_EXPIRY / 2);

		let pool = Pool::new(Config::default().with_expiry(Duration::from_secs(1)));
		assert_eq!(pool.expiry(), Some(Duration::from_secs(1)));
		assert_eq!(pool.expiry_ticks(), 10);
		assert_eq!(pool.refresh_interval(), Duration::from_millis(500));

		let pool = Pool::new(Config::default().with_expiry(Duration::from_millis(1)));
		assert_eq!(pool.expiry(), Some(Duration::from_millis(TICK_MS)));
		assert_eq!(pool.expiry_ticks(), 1);

		// Disabled: never reclaimed by idleness, but readers still re-stamp on a
		// bounded cadence for byte-eviction protection.
		let pool = Pool::new(Config::default());
		assert_eq!(pool.expiry(), None);
		assert_eq!(pool.expiry_ticks(), u64::MAX);
		assert_eq!(pool.refresh_interval(), DEFAULT_EXPIRY / 2);
	}

	#[test]
	fn standalone_origin_enables_default_expiry() {
		assert_eq!(crate::origin::Info::default().pool.expiry(), Some(DEFAULT_EXPIRY));
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
