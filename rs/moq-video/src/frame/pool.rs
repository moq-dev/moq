//! A bounded pool of device allocations for the GPU frame path.
//!
//! Every frame the GPU conversion or resize produces draws its buffer from
//! here, so the memory a stream can hold at once is a fixed number of buffers
//! rather than however many frames an encoder falls behind by. A buffer goes
//! back when its last frame drops, and a later frame of the same or a smaller
//! size reuses it; `cuMemFree` synchronizes the whole device, so recycling
//! also keeps a free off the per-frame path.

use std::num::NonZeroUsize;
use std::sync::Mutex;

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
	/// Buffers handed out and not yet returned.
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

	/// A buffer of at least `len` bytes: the smallest idle one that fits, else a
	/// fresh allocation while under capacity, else an idle one too small for the
	/// job freed to make room. With every buffer live, the pool is full and this
	/// fails rather than growing.
	pub(crate) fn take(&self, len: usize) -> Result<A::Buffer, Error> {
		let mut state = self.state.lock().expect("GPU frame pool poisoned");
		let fits = state
			.idle
			.iter()
			.enumerate()
			.filter(|(_, (have, _))| *have >= len)
			.min_by_key(|(_, (have, _))| *have)
			.map(|(index, _)| index);
		if let Some(index) = fits {
			let (_, buffer) = state.idle.swap_remove(index);
			state.live += 1;
			return Ok(buffer);
		}
		if state.live + state.idle.len() >= self.capacity {
			if state.live >= self.capacity {
				return Err(Error::Unsupported(format!(
					"GPU frame pool capacity {} exhausted; drop a frame before converting another",
					self.capacity
				)));
			}
			// Every idle buffer is too small: free the smallest and allocate.
			let smallest = state
				.idle
				.iter()
				.enumerate()
				.min_by_key(|(_, (have, _))| *have)
				.map(|(index, _)| index)
				.expect("an idle buffer exists when live is under capacity");
			state.idle.swap_remove(smallest);
		}
		let buffer = self.alloc.alloc(len)?;
		state.live += 1;
		Ok(buffer)
	}

	/// Return a buffer of `len` bytes taken from this pool.
	pub(crate) fn put(&self, len: usize, buffer: A::Buffer) {
		let mut state = self.state.lock().expect("GPU frame pool poisoned");
		state.live -= 1;
		state.idle.push((len, buffer));
	}
}

#[cfg(test)]
mod tests {
	use std::cell::Cell;

	use super::*;

	/// Counts allocations; each buffer is its length.
	struct Counting(Cell<usize>);

	impl Alloc for Counting {
		type Buffer = usize;

		fn alloc(&self, len: usize) -> Result<usize, Error> {
			self.0.set(self.0.get() + 1);
			Ok(len)
		}
	}

	fn pool(capacity: usize) -> Pool<Counting> {
		Pool::new(Counting(Cell::new(0)), NonZeroUsize::new(capacity).unwrap())
	}

	#[test]
	fn a_full_pool_refuses_rather_than_grows() {
		let pool = pool(2);
		assert_eq!(pool.capacity(), 2);
		let a = pool.take(100).unwrap();
		let _b = pool.take(100).unwrap();
		let err = pool.take(100).unwrap_err();
		assert!(matches!(err, Error::Unsupported(_)), "{err}");
		assert_eq!(pool.alloc.0.get(), 2);

		pool.put(100, a);
		pool.take(100).unwrap();
		assert_eq!(pool.alloc.0.get(), 2, "a returned buffer is reused, not reallocated");
	}

	#[test]
	fn reuse_picks_the_smallest_buffer_that_fits() {
		let pool = pool(3);
		let small = pool.take(10).unwrap();
		let medium = pool.take(50).unwrap();
		let large = pool.take(100).unwrap();
		pool.put(10, small);
		pool.put(50, medium);
		pool.put(100, large);

		assert_eq!(pool.take(40).unwrap(), 50);
		assert_eq!(pool.take(40).unwrap(), 100, "the next fit, not a fresh allocation");
		assert_eq!(pool.alloc.0.get(), 3);
	}

	#[test]
	fn an_idle_buffer_too_small_is_replaced_within_capacity() {
		let pool = pool(2);
		let _live = pool.take(10).unwrap();
		let idle = pool.take(10).unwrap();
		pool.put(10, idle);

		// Capacity is reached (one live, one idle), but the idle buffer cannot
		// serve a bigger frame: it is freed and a fresh one takes its place.
		assert_eq!(pool.take(100).unwrap(), 100);
		assert_eq!(pool.alloc.0.get(), 3);
		assert!(pool.take(10).is_err(), "both buffers are live now");
	}

	#[test]
	fn an_allocation_failure_leaves_the_count_intact() {
		struct Failing;

		impl Alloc for Failing {
			type Buffer = ();

			fn alloc(&self, _len: usize) -> Result<(), Error> {
				Err(Error::Codec(anyhow::anyhow!("out of device memory")))
			}
		}

		let pool = Pool::new(Failing, NonZeroUsize::new(1).unwrap());
		assert!(matches!(pool.take(8), Err(Error::Codec(_))));
		assert_eq!(pool.state.lock().unwrap().live, 0);
	}
}
