//! A bounded pool of device allocations for the GPU frame path.
//!
//! Every frame the GPU conversion or resize produces draws its buffer from
//! here, so the memory a stream can hold at once is a fixed number of buffers
//! rather than however many frames an encoder falls behind by. A buffer goes
//! back when its last frame drops, and a later frame of the same or a smaller
//! size reuses it; `cuMemFree` synchronizes the whole device, so recycling
//! also keeps a free off the per-frame path.

use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};

use crate::Error;

/// Allocates the pool's buffers. Injected so the policy is tested without a
/// device.
pub(crate) trait Alloc {
	/// A device allocation, freed on drop.
	type Buffer;

	/// Allocate `len` bytes.
	fn alloc(&self, len: usize) -> Result<Self::Buffer, Error>;
}

pub(crate) struct Pool<A: Alloc> {
	alloc: A,
	capacity: usize,
	state: Mutex<State<A::Buffer>>,
}

struct State<B> {
	/// Reservations held, filled or not.
	live: usize,
	/// Returned buffers with their lengths, ready for reuse.
	idle: Vec<(usize, B)>,
}

impl<A: Alloc> Pool<A> {
	/// A pool holding at most `capacity` buffers, live and idle together.
	pub(crate) fn new(alloc: A, capacity: NonZeroUsize) -> Self {
		Self {
			alloc,
			capacity: capacity.get(),
			state: Mutex::new(State {
				live: 0,
				idle: Vec::new(),
			}),
		}
	}

	/// The most buffers this pool holds at once.
	pub(crate) fn capacity(&self) -> usize {
		self.capacity
	}

	/// Hold one of the pool's buffers, or `None` while every one is live. The
	/// buffer itself is picked by [`Reservation::fill`], once its size is known.
	pub(crate) fn reserve(self: &Arc<Self>) -> Option<Reservation<A>> {
		let mut state = self.state.lock().expect("GPU frame pool poisoned");
		if state.live >= self.capacity {
			return None;
		}
		state.live += 1;
		Some(Reservation(Held {
			pool: self.clone(),
			buffer: None,
		}))
	}
}

/// A place in the pool, returned on drop.
pub(crate) struct Reservation<A: Alloc>(Held<A>);

impl<A: Alloc> Reservation<A> {
	/// A buffer of at least `len` bytes: the smallest idle one that fits, else a
	/// fresh allocation while under capacity, else an idle one too small for the
	/// job freed to make room. Only a failed allocation is an error, and it
	/// releases the reservation.
	pub(crate) fn fill(self, len: usize) -> Result<Lease<A>, Error> {
		let mut held = self.0;
		let pool = &held.pool;
		let mut state = pool.state.lock().expect("GPU frame pool poisoned");
		let fits = state
			.idle
			.iter()
			.enumerate()
			.filter(|(_, (have, _))| *have >= len)
			.min_by_key(|(_, (have, _))| *have)
			.map(|(index, _)| index);
		let buffer = match fits {
			Some(index) => state.idle.swap_remove(index),
			None => {
				// `live` counts this reservation, so an idle buffer exists
				// whenever the two together exceed capacity.
				if state.live + state.idle.len() > pool.capacity {
					let smallest = state
						.idle
						.iter()
						.enumerate()
						.min_by_key(|(_, (have, _))| *have)
						.map(|(index, _)| index)
						.expect("an idle buffer exists past capacity");
					state.idle.swap_remove(smallest);
				}
				(len, pool.alloc.alloc(len)?)
			}
		};
		drop(state);
		held.buffer = Some(buffer);
		Ok(Lease(held))
	}
}

/// A reservation holding its buffer, returned to the pool for reuse on drop.
pub(crate) struct Lease<A: Alloc>(Held<A>);

impl<A: Alloc> Lease<A> {
	/// The pool this buffer came from.
	pub(crate) fn pool(&self) -> &Arc<Pool<A>> {
		&self.0.pool
	}
}

impl<A: Alloc> std::ops::Deref for Lease<A> {
	type Target = A::Buffer;

	fn deref(&self) -> &A::Buffer {
		&self.0.buffer.as_ref().expect("a lease holds its buffer").1
	}
}

/// What both handles release: the place, and the buffer once filled.
struct Held<A: Alloc> {
	pool: Arc<Pool<A>>,
	buffer: Option<(usize, A::Buffer)>,
}

impl<A: Alloc> Drop for Held<A> {
	fn drop(&mut self) {
		let mut state = self.pool.state.lock().expect("GPU frame pool poisoned");
		state.live -= 1;
		state.idle.extend(self.buffer.take());
	}
}

#[cfg(test)]
mod tests {
	use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

	use super::*;

	/// Counts allocations; each buffer is its length.
	struct Counting(AtomicUsize);

	impl Alloc for Counting {
		type Buffer = usize;

		fn alloc(&self, len: usize) -> Result<usize, Error> {
			self.0.fetch_add(1, Ordering::Relaxed);
			Ok(len)
		}
	}

	fn pool(capacity: usize) -> Arc<Pool<Counting>> {
		Arc::new(Pool::new(
			Counting(AtomicUsize::new(0)),
			NonZeroUsize::new(capacity).unwrap(),
		))
	}

	fn take(pool: &Arc<Pool<Counting>>, len: usize) -> Lease<Counting> {
		pool.reserve().expect("a free place").fill(len).unwrap()
	}

	#[test]
	fn reserve_yields_capacity_places_then_none() {
		let pool = pool(2);
		assert_eq!(pool.capacity(), 2);
		let a = pool.reserve().unwrap();
		let _b = pool.reserve().unwrap();
		assert!(pool.reserve().is_none(), "a full pool refuses rather than grows");
		assert_eq!(
			pool.alloc.0.load(Ordering::Relaxed),
			0,
			"a reservation alone allocates nothing"
		);

		drop(a);
		assert!(pool.reserve().is_some(), "an unfilled reservation frees its place");
		assert_eq!(pool.alloc.0.load(Ordering::Relaxed), 0);
	}

	#[test]
	fn a_dropped_lease_returns_its_buffer() {
		let pool = pool(2);
		let a = take(&pool, 100);
		let _b = take(&pool, 100);
		assert!(Arc::ptr_eq(a.pool(), &pool));
		assert!(pool.reserve().is_none());
		assert_eq!(pool.alloc.0.load(Ordering::Relaxed), 2);

		// A filled lease dropped unused, as a failed conversion drops its
		// destination, frees its place and keeps its buffer for the next frame.
		drop(a);
		let _c = take(&pool, 100);
		assert_eq!(
			pool.alloc.0.load(Ordering::Relaxed),
			2,
			"a returned buffer is reused, not reallocated"
		);
		assert_eq!(pool.state.lock().unwrap().live, 2);
	}

	#[test]
	fn reuse_picks_the_smallest_buffer_that_fits() {
		let pool = pool(3);
		drop((take(&pool, 10), take(&pool, 50), take(&pool, 100)));

		let medium = take(&pool, 40);
		assert_eq!(*medium, 50);
		assert_eq!(*take(&pool, 40), 100, "the next fit, not a fresh allocation");
		assert_eq!(pool.alloc.0.load(Ordering::Relaxed), 3);
	}

	#[test]
	fn an_idle_buffer_too_small_is_replaced_within_capacity() {
		let pool = pool(2);
		let _live = take(&pool, 10);
		drop(take(&pool, 10));

		// Capacity is reached (one live, one idle), but the idle buffer cannot
		// serve a bigger frame: it is freed and a fresh one takes its place.
		let big = take(&pool, 100);
		assert_eq!(*big, 100);
		assert_eq!(pool.alloc.0.load(Ordering::Relaxed), 3);
		assert!(pool.reserve().is_none(), "both buffers are live now");
	}

	#[test]
	fn a_failed_allocation_releases_the_reservation() {
		/// Fails its first allocation, then succeeds.
		struct Flaky(AtomicBool);

		impl Alloc for Flaky {
			type Buffer = ();

			fn alloc(&self, _len: usize) -> Result<(), Error> {
				if self.0.swap(false, Ordering::Relaxed) {
					return Err(Error::Codec(anyhow::anyhow!("out of device memory")));
				}
				Ok(())
			}
		}

		let pool = Arc::new(Pool::new(Flaky(AtomicBool::new(true)), NonZeroUsize::new(1).unwrap()));
		assert!(matches!(pool.reserve().unwrap().fill(8), Err(Error::Codec(_))));
		assert_eq!(pool.state.lock().unwrap().live, 0);
		pool.reserve()
			.expect("the failed fill released its place")
			.fill(8)
			.unwrap();
	}
}
