use std::collections::{HashMap, hash_map::Entry};
use std::num::NonZero;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::Error;

/// Opaque resource identifier returned to C code.
///
/// Non-zero u32 value that uniquely identifies a live resource of one kind,
/// such as a session, origin, broadcast, or track. After the counter wraps,
/// resources of different kinds may share a value. Zero is reserved to
/// indicate "none" or optional parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Id(NonZero<u32>);

impl std::fmt::Display for Id {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{}", self.0.get())
	}
}

/// Process-wide counter wrapping from i32::MAX back to 1.
static NEXT_ID: AtomicU32 = AtomicU32::new(1);

/// Map that assigns IDs from a process-wide counter, skipping live entries.
///
/// Freed IDs are not recycled until the counter wraps. This avoids immediately
/// aliasing stale handles while allowing long-running hosts to keep allocating.
pub(crate) struct NonZeroSlab<T> {
	map: HashMap<Id, T>,
}

impl<T> NonZeroSlab<T> {
	pub fn insert(&mut self, value: T) -> Result<Id, Error> {
		self.insert_next(value, &NEXT_ID)
	}

	fn insert_next(&mut self, value: T, next: &AtomicU32) -> Result<Id, Error> {
		if self.map.len() == i32::MAX as usize {
			return Err(Error::InvalidId);
		}

		loop {
			let raw = next
				.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| {
					Some(if id == i32::MAX as u32 { 1 } else { id + 1 })
				})
				.unwrap();
			let id = Id::try_from(raw)?;
			if let Entry::Vacant(entry) = self.map.entry(id) {
				entry.insert(value);
				return Ok(id);
			}
		}
	}

	pub fn get(&self, id: Id) -> Option<&T> {
		self.map.get(&id)
	}

	pub fn get_mut(&mut self, id: Id) -> Option<&mut T> {
		self.map.get_mut(&id)
	}

	pub fn remove(&mut self, id: Id) -> Option<T> {
		self.map.remove(&id)
	}
}

impl TryFrom<i32> for Id {
	type Error = Error;

	fn try_from(value: i32) -> Result<Self, Self::Error> {
		Self::try_from(u32::try_from(value).map_err(|_| Error::InvalidId)?)
	}
}

impl TryFrom<u32> for Id {
	type Error = Error;

	fn try_from(value: u32) -> Result<Self, Self::Error> {
		// IDs must fit in i32 (positive) for the C FFI layer.
		i32::try_from(value).map_err(|_| Error::InvalidId)?;
		NonZero::try_from(value).map(Id).map_err(|_| Error::InvalidId)
	}
}

impl From<Id> for u32 {
	fn from(value: Id) -> Self {
		value.0.get()
	}
}

impl From<Id> for i32 {
	fn from(value: Id) -> Self {
		// Safety: TryFrom<u32> guarantees the value fits in i32.
		value.0.get() as i32
	}
}

impl<T> Default for NonZeroSlab<T> {
	fn default() -> Self {
		Self { map: HashMap::new() }
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn wrap_uses_the_whole_positive_i32_range() {
		let next = AtomicU32::new(i32::MAX as u32);
		let mut slab = NonZeroSlab::default();
		let high = slab.insert_next((), &next).unwrap();
		assert_eq!(i32::from(high), i32::MAX);
		let low = slab.insert_next((), &next).unwrap();
		assert_eq!(i32::from(low), 1);
		assert_eq!(slab.remove(high), Some(()));
		assert_eq!(i32::from(slab.insert_next((), &next).unwrap()), 2);
	}

	#[test]
	fn wrap_skips_live_handles_and_preserves_their_values() {
		let next = AtomicU32::new(i32::MAX as u32 - 1);
		let mut slab = NonZeroSlab::default();
		let low = Id::try_from(1u32).unwrap();
		let high = Id::try_from(i32::MAX).unwrap();
		slab.map.insert(low, "low");
		slab.map.insert(high, "high");

		let before = slab.insert_next("before wrap", &next).unwrap();
		assert_eq!(i32::from(before), i32::MAX - 1);
		let after = slab.insert_next("after wrap", &next).unwrap();
		assert_eq!(i32::from(after), 2);
		assert_eq!(slab.get(low), Some(&"low"));
		assert_eq!(slab.get(high), Some(&"high"));
		assert_eq!(slab.get(before), Some(&"before wrap"));
		assert_eq!(slab.get(after), Some(&"after wrap"));

		assert_eq!(slab.remove(low), Some("low"));
		let following = slab.insert_next("following", &next).unwrap();
		assert_eq!(i32::from(following), 3);
	}

	#[test]
	#[ignore = "microbenchmark; runs in Nightly"]
	fn benchmark_handles() {
		use std::hint::black_box;

		for live in [0, 64, 4096] {
			for wrap in [false, true] {
				let next = AtomicU32::new(live + 1);
				let mut slab = NonZeroSlab::default();
				for raw in 1..=live {
					slab.map.insert(Id::try_from(raw).unwrap(), 0);
				}
				let iterations = 1000;
				let start = kio::clock::Instant::now();
				for _ in 0..iterations {
					if wrap {
						next.store(i32::MAX as u32, Ordering::Relaxed);
						let high = slab.insert_next(black_box(42), &next).unwrap();
						black_box(slab.remove(high).unwrap());
					}
					let id = slab.insert_next(black_box(42), &next).unwrap();
					black_box(slab.remove(id).unwrap());
				}
				eprintln!(
					"handles live={live} wrap={wrap}: {:.0} ns/iteration",
					start.elapsed().as_nanos() as f64 / f64::from(iterations)
				);
			}
		}
	}
}
