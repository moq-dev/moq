use std::{
	cell::RefCell,
	fmt,
	marker::PhantomData,
	ops::{Deref, DerefMut},
	// std, not `crate::sync`: [`WeakLock`] needs `Arc::downgrade`, which loom's Arc
	// doesn't have. Loom still models the `Mutex` inside, which is the part that
	// orders every handle against every other. The wait meter is std atomics on
	// purpose: it is not part of the model, and a loom run must not permute it.
	panic::Location,
	rc::Rc,
	sync::{
		Arc, OnceLock, Weak,
		atomic::{AtomicU64, AtomicUsize, Ordering},
	},
	time::Duration,
};

use crate::sync::{Mutex, MutexGuard};

#[cfg(not(loom))]
use std::sync::TryLockError;

/// A cloneable mutex wrapper backed by `Arc<Mutex<T>>`.
///
/// Every kio channel keeps its state in one of these, with its [`WaiterList`]s inside,
/// so a state change and the wake it owes are decided under a single lock.
///
/// Poisoning is not part of the contract: a panic while the lock is held poisons it, and
/// every method here panics in turn rather than handing back a `Result`.
///
/// [`WaiterList`]: crate::WaiterList
pub struct Lock<T> {
	inner: Arc<Mutex<T>>,
}

impl<T> Lock<T> {
	/// Wrap a value.
	pub fn new(value: T) -> Self {
		Self {
			inner: Arc::new(Mutex::new(value)),
		}
	}

	/// Lock the state, blocking until it is free. Panics if the lock was poisoned.
	///
	/// Uncontended this is one `try_lock`. The caller location is read only
	/// after that fails.
	#[inline]
	#[track_caller]
	pub fn lock(&self) -> LockGuard<'_, T> {
		LockGuard { inner: self.acquire() }
	}

	/// Loom keeps the plain `lock`: a `try_lock` in front of it would be a
	/// second modeled atomic on every acquire, so the wait meter stays out of
	/// that model. Debug and release both keep the fast path.
	#[cfg(loom)]
	fn acquire(&self) -> MutexGuard<'_, T> {
		self.inner.lock().expect("mutex poisoned")
	}

	#[cfg(not(loom))]
	#[inline]
	#[track_caller]
	fn acquire(&self) -> MutexGuard<'_, T> {
		match self.inner.try_lock() {
			Ok(guard) => guard,
			Err(TryLockError::Poisoned(_)) => panic!("mutex poisoned"),
			Err(TryLockError::WouldBlock) => self.acquire_slow(),
		}
	}

	/// Cold path. `#[track_caller]` chains through [`Lock::lock`], so the site
	/// is the caller, not this function.
	#[cfg(not(loom))]
	#[inline(never)]
	#[track_caller]
	fn acquire_slow(&self) -> MutexGuard<'_, T> {
		let site = Location::caller();
		// Count before blocking so a scrape during the wait still sees the
		// acquire. The clock runs only here; an uncontended `try_lock` does not.
		let tracking = note_contended();
		let started = std::time::Instant::now();
		let guard = self.inner.lock().expect("mutex poisoned");
		let waited = started.elapsed();
		if tracking {
			note_wait(waited);
		}
		note_site(site, waited);
		guard
	}

	/// Whether both handles point at the same state.
	pub fn is_clone(&self, other: &Self) -> bool {
		Arc::ptr_eq(&self.inner, &other.inner)
	}

	/// Create a handle that doesn't own the value, so it can be stored inside the
	/// value itself without leaking the allocation.
	pub(crate) fn downgrade(&self) -> WeakLock<T> {
		WeakLock {
			inner: Arc::downgrade(&self.inner),
		}
	}
}

/// A [`Lock`] that doesn't own its value, backed by `Weak<Mutex<T>>`.
pub(crate) struct WeakLock<T> {
	inner: Weak<Mutex<T>>,
}

impl<T> WeakLock<T> {
	/// A handle that never upgrades, for a placeholder before a value exists.
	pub(crate) fn new() -> Self {
		Self { inner: Weak::new() }
	}

	/// Recover the owning [`Lock`], or `None` once the value has been dropped.
	pub(crate) fn upgrade(&self) -> Option<Lock<T>> {
		Some(Lock {
			inner: self.inner.upgrade()?,
		})
	}
}

impl<T> Clone for WeakLock<T> {
	fn clone(&self) -> Self {
		Self {
			inner: self.inner.clone(),
		}
	}
}

impl<T> fmt::Debug for WeakLock<T> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_tuple("WeakLock").finish()
	}
}

impl<T> Clone for Lock<T> {
	fn clone(&self) -> Self {
		Self {
			inner: self.inner.clone(),
		}
	}
}

impl<T: Default> Default for Lock<T> {
	fn default() -> Self {
		Self::new(T::default())
	}
}

impl<T: fmt::Debug> fmt::Debug for Lock<T> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self.inner.try_lock() {
			Ok(guard) => f.debug_tuple("Lock").field(&*guard).finish(),
			Err(_) => f.debug_tuple("Lock").field(&"<locked>").finish(),
		}
	}
}

/// A guard providing access to the locked value. Releases the lock on drop.
pub struct LockGuard<'a, T> {
	inner: MutexGuard<'a, T>,
}

impl<T: fmt::Debug> fmt::Debug for LockGuard<'_, T> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_tuple("LockGuard").field(&*self.inner).finish()
	}
}

impl<T> Deref for LockGuard<'_, T> {
	type Target = T;

	fn deref(&self) -> &Self::Target {
		&self.inner
	}
}

impl<T> DerefMut for LockGuard<'_, T> {
	fn deref_mut(&mut self) -> &mut Self::Target {
		&mut self.inner
	}
}

impl<T> Default for WeakLock<T> {
	fn default() -> Self {
		Self::new()
	}
}

thread_local! {
	static BOUND: RefCell<Option<Arc<WaitInner>>> = const { RefCell::new(None) };
}

struct WaitInner {
	wait_ns: AtomicU64,
	contended: AtomicU64,
	/// Bumps on every [`LockWait::bind`], so an older [`LockWaitBind`] does not
	/// clear a newer one for the same meter.
	generation: AtomicU64,
}

impl WaitInner {
	fn new() -> Self {
		Self {
			wait_ns: AtomicU64::new(0),
			contended: AtomicU64::new(0),
			generation: AtomicU64::new(0),
		}
	}
}

/// Time one thread has spent blocked in [`Lock::lock`], and how often.
///
/// Bind it on a worker thread with [`bind`](Self::bind). Only that thread adds
/// to it, and only when `try_lock` fails. Clones share the counters.
#[derive(Clone)]
pub struct LockWait {
	inner: Arc<WaitInner>,
}

impl LockWait {
	/// A zeroed meter.
	pub fn new() -> Self {
		Self {
			inner: Arc::new(WaitInner::new()),
		}
	}

	/// Time blocked, and how many acquires blocked.
	pub fn snapshot(&self) -> (Duration, u64) {
		let nanos = self.inner.wait_ns.load(Ordering::Relaxed);
		let contended = self.inner.contended.load(Ordering::Relaxed);
		(Duration::from_nanos(nanos), contended)
	}

	/// Count this thread's blocking acquires here until the guard drops.
	pub fn bind(&self) -> LockWaitBind {
		let generation = self.inner.generation.fetch_add(1, Ordering::Relaxed) + 1;
		BOUND.with(|slot| *slot.borrow_mut() = Some(Arc::clone(&self.inner)));
		LockWaitBind {
			inner: Arc::clone(&self.inner),
			generation,
			_thread: PhantomData,
		}
	}
}

impl Default for LockWait {
	fn default() -> Self {
		Self::new()
	}
}

impl fmt::Debug for LockWait {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		let (wait, contended) = self.snapshot();
		f.debug_struct("LockWait")
			.field("wait", &wait)
			.field("contended", &contended)
			.finish()
	}
}

/// Clears the binding [`LockWait::bind`] installed, when it is still current.
///
/// Not [`Send`]: dropping it on another thread would unbind the wrong one.
#[must_use = "dropping this unbinds the thread immediately"]
pub struct LockWaitBind {
	inner: Arc<WaitInner>,
	generation: u64,
	_thread: PhantomData<Rc<()>>,
}

impl Drop for LockWaitBind {
	fn drop(&mut self) {
		if self.inner.generation.load(Ordering::Relaxed) != self.generation {
			return;
		}
		BOUND.with(|slot| {
			let mut slot = slot.borrow_mut();
			if slot.as_ref().is_some_and(|current| Arc::ptr_eq(current, &self.inner)) {
				*slot = None;
			}
		});
	}
}

/// `true` when this thread has a meter, which was incremented.
fn note_contended() -> bool {
	BOUND.with(|slot| {
		let slot = slot.borrow();
		let Some(inner) = slot.as_ref() else {
			return false;
		};
		inner.contended.fetch_add(1, Ordering::Relaxed);
		true
	})
}

fn note_wait(waited: Duration) {
	let nanos = u64::try_from(waited.as_nanos()).unwrap_or(u64::MAX);
	if nanos == 0 {
		return;
	}
	BOUND.with(|slot| {
		let slot = slot.borrow();
		if let Some(inner) = slot.as_ref() {
			inner.wait_ns.fetch_add(nanos, Ordering::Relaxed);
		}
	});
}

/// One contended [`Lock::lock`] call site, summed across every thread.
#[derive(Clone, Copy, Debug)]
pub struct LockSite {
	/// Source file of the `lock` call.
	pub file: &'static str,
	/// Line of the `lock` call.
	pub line: u32,
	/// Column of the `lock` call.
	pub column: u32,
	/// Time blocked at this call site.
	pub wait: Duration,
	/// How many acquires at this call site blocked.
	pub contended: u64,
}

const SITE_SLOTS: usize = 128;

struct SiteSlot {
	/// Address of the `&'static Location`, or 0 when the slot is free.
	key: AtomicUsize,
	wait_ns: AtomicU64,
	contended: AtomicU64,
}

impl SiteSlot {
	const fn new() -> Self {
		Self {
			key: AtomicUsize::new(0),
			wait_ns: AtomicU64::new(0),
			contended: AtomicU64::new(0),
		}
	}
}

fn site_table() -> &'static OnceLock<Box<[SiteSlot; SITE_SLOTS]>> {
	static SLOTS: OnceLock<Box<[SiteSlot; SITE_SLOTS]>> = OnceLock::new();
	&SLOTS
}

fn site_slots() -> &'static [SiteSlot; SITE_SLOTS] {
	site_table().get_or_init(|| Box::new(std::array::from_fn(|_| SiteSlot::new())))
}

static SITE_OVERFLOW: AtomicU64 = AtomicU64::new(0);

/// Contended acquires this process could not attribute because the site table filled.
pub fn lock_site_overflow() -> u64 {
	SITE_OVERFLOW.load(Ordering::Relaxed)
}

/// Contended acquires since process start, hottest call site first.
///
/// One entry per source location. `#[track_caller]` gives each generic
/// instantiation its own [`Location`], and those are summed so a metrics
/// scrape does not emit the same labels twice.
pub fn lock_sites() -> Vec<LockSite> {
	let Some(slots) = site_table().get() else {
		return Vec::new();
	};
	let mut sites: Vec<LockSite> = Vec::new();
	for slot in slots.iter() {
		let key = slot.key.load(Ordering::Acquire);
		if key == 0 {
			continue;
		}
		// SAFETY: `note_site` stores the address of a `&'static Location` here.
		let location = unsafe { &*(key as *const Location) };
		let nanos = slot.wait_ns.load(Ordering::Relaxed);
		let contended = slot.contended.load(Ordering::Relaxed);
		if let Some(existing) = sites.iter_mut().find(|site| {
			site.file == location.file() && site.line == location.line() && site.column == location.column()
		}) {
			existing.wait += Duration::from_nanos(nanos);
			existing.contended += contended;
			continue;
		}
		sites.push(LockSite {
			file: location.file(),
			line: location.line(),
			column: location.column(),
			wait: Duration::from_nanos(nanos),
			contended,
		});
	}
	sites.sort_by(|left, right| right.wait.cmp(&left.wait).then(right.contended.cmp(&left.contended)));
	sites
}

fn note_site(site: &'static Location, waited: Duration) {
	let key = site as *const Location as usize;
	if key == 0 {
		return;
	}
	let slots = site_slots();
	let nanos = u64::try_from(waited.as_nanos()).unwrap_or(u64::MAX);
	let start = key % SITE_SLOTS;
	for step in 0..SITE_SLOTS {
		let slot = &slots[(start + step) % SITE_SLOTS];
		let current = slot.key.load(Ordering::Acquire);
		if current == key {
			charge_site(slot, nanos);
			return;
		}
		if current != 0 {
			continue;
		}
		match slot.key.compare_exchange(0, key, Ordering::AcqRel, Ordering::Acquire) {
			Ok(_) => {
				charge_site(slot, nanos);
				return;
			}
			Err(seen) if seen == key => {
				charge_site(slot, nanos);
				return;
			}
			Err(_) => continue,
		}
	}
	SITE_OVERFLOW.fetch_add(1, Ordering::Relaxed);
}

fn charge_site(slot: &SiteSlot, nanos: u64) {
	slot.contended.fetch_add(1, Ordering::Relaxed);
	if nanos > 0 {
		slot.wait_ns.fetch_add(nanos, Ordering::Relaxed);
	}
}

#[cfg(all(test, not(loom)))]
mod tests {
	use std::time::{Duration, Instant};

	use super::*;

	#[test]
	fn uncontended_lock_does_not_charge() {
		let wait = LockWait::new();
		let _bound = wait.bind();
		let lock = Lock::new(1);
		{
			let _guard = lock.lock();
		}
		let _guard = lock.lock();
		assert_eq!(wait.snapshot(), (Duration::ZERO, 0));
	}

	#[test]
	fn contended_lock_charges_only_the_bound_thread() {
		let wait = LockWait::new();
		let lock = Lock::new(1);
		let _hold = lock.lock();

		std::thread::scope(|scope| {
			let lock = lock.clone();
			let bound_on_thread = wait.clone();
			scope.spawn(move || {
				let _bound = bound_on_thread.bind();
				let _guard = lock.lock();
			});

			let started = Instant::now();
			while wait.snapshot().1 == 0 {
				if started.elapsed() > Duration::from_secs(2) {
					panic!("contended acquire was not charged");
				}
				std::thread::yield_now();
			}
			drop(_hold);
		});

		assert_eq!(wait.snapshot().1, 1);
	}

	/// A bind dropped before the acquire must not charge, including when that
	/// acquire then blocks. The flag is the handshake: it is set only after
	/// the guard is gone, and the holder waits for it before releasing.
	#[test]
	fn dropping_the_bind_stops_charging() {
		use std::sync::atomic::{AtomicBool, Ordering};

		let wait = LockWait::new();
		let lock = Lock::new(1);
		let hold = lock.lock();
		let started = std::sync::Arc::new(AtomicBool::new(false));
		let started_flag = std::sync::Arc::clone(&started);
		std::thread::scope(|scope| {
			let lock = lock.clone();
			let wait = wait.clone();
			scope.spawn(move || {
				let bound = wait.bind();
				drop(bound);
				started_flag.store(true, Ordering::Release);
				let _guard = lock.lock();
			});
			let deadline = Instant::now() + Duration::from_secs(2);
			while !started.load(Ordering::Acquire) {
				if Instant::now() > deadline {
					panic!("worker thread did not drop its lock-wait bind");
				}
				std::thread::yield_now();
			}
			drop(hold);
		});
		assert_eq!(wait.snapshot().1, 0);
	}

	/// The contended path names the caller, not `acquire_slow`. The site is
	/// recorded once the acquire returns, so the line is known after the join.
	#[test]
	fn contended_acquire_names_its_call_site() {
		#[inline(never)]
		fn locks_here(lock: &Lock<i32>, wait: &LockWait) -> u32 {
			let _bound = wait.bind();
			let line = line!();
			let _guard = lock.lock();
			line
		}

		let wait = LockWait::new();
		let lock = Lock::new(1);
		let hold = lock.lock();
		let call_line = std::thread::scope(|scope| {
			let lock = lock.clone();
			let thread_wait = wait.clone();
			let handle = scope.spawn(move || locks_here(&lock, &thread_wait));
			let started = Instant::now();
			while wait.snapshot().1 == 0 {
				if started.elapsed() > Duration::from_secs(2) {
					panic!("contended acquire was not charged");
				}
				std::thread::yield_now();
			}
			drop(hold);
			handle.join().expect("site thread")
		});
		let site = lock_sites()
			.into_iter()
			.find(|site| site.file.ends_with("src/lock.rs") && (site.line == call_line || site.line == call_line + 1))
			.expect("contended acquire was not attributed");
		assert!(site.contended >= 1);
		assert!(site.wait > Duration::ZERO);
	}

	/// Two monomorphizations of one `lock` call share a source location, so
	/// they must come back as one row. Otherwise a scrape emits duplicate labels.
	#[test]
	fn monomorphized_sites_sum_into_one_row() {
		#[inline(never)]
		fn locks_here<T>(lock: &Lock<T>, wait: &LockWait) -> u32 {
			let _bound = wait.bind();
			let line = line!();
			let _guard = lock.lock();
			line
		}

		let wait = LockWait::new();
		let ints = Lock::new(1i32);
		let words = Lock::new(1u64);
		let hold_ints = ints.lock();
		let hold_words = words.lock();
		let call_line = std::thread::scope(|scope| {
			let ints = ints.clone();
			let words = words.clone();
			let wait_ints = wait.clone();
			let wait_words = wait.clone();
			let ints_thread = scope.spawn(move || locks_here(&ints, &wait_ints));
			let words_thread = scope.spawn(move || locks_here(&words, &wait_words));
			let started = Instant::now();
			while wait.snapshot().1 < 2 {
				if started.elapsed() > Duration::from_secs(2) {
					panic!("both contended acquires were not charged");
				}
				std::thread::yield_now();
			}
			drop(hold_ints);
			drop(hold_words);
			let line = ints_thread.join().expect("int site thread");
			assert_eq!(line, words_thread.join().expect("word site thread"));
			line
		});
		let matches: Vec<_> = lock_sites()
			.into_iter()
			.filter(|site| site.file.ends_with("src/lock.rs") && (site.line == call_line || site.line == call_line + 1))
			.collect();
		assert_eq!(matches.len(), 1, "monomorphized locations must merge");
		assert!(matches[0].contended >= 2);
	}
}
