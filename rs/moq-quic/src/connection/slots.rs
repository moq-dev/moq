use std::collections::VecDeque;
use std::ops::{Bound, RangeBounds};

/// Slots per chunk, one bit each in [`Chunk::mask`].
pub(super) const CHUNK: usize = 64;

/// Entries keyed by increasing packet numbers, in chunks of [`CHUNK`] consecutive slots.
///
/// A port of [quinn-rs/quinn#2942](https://github.com/quinn-rs/quinn/pull/2942). Lookups near
/// either end are O(1), the rest O(log chunks). An emptied chunk is freed wherever it sits, so an
/// entry outliving its neighbors holds at most one chunk, rather than every slot after it as one
/// ring of packet numbers would.
pub(super) struct Slots<T> {
	chunks: VecDeque<Chunk<T>>,
}

struct Chunk<T> {
	/// The packet number of slot 0.
	offset: u64,
	slots: Box<[Option<T>; CHUNK]>,
	/// Bit `i` is set when slot `i` holds an entry; never zero.
	mask: u64,
}

impl<T> Chunk<T> {
	fn slot(&self, pn: u64) -> Option<usize> {
		let slot = usize::try_from(pn.checked_sub(self.offset)?).ok()?;
		(slot < CHUNK).then_some(slot)
	}
}

impl<T> Default for Slots<T> {
	fn default() -> Self {
		Self {
			chunks: VecDeque::new(),
		}
	}
}

impl<T> Slots<T> {
	/// Insert `value` at `pn`, which must exceed every packet number inserted before.
	pub(super) fn insert(&mut self, pn: u64, value: T) {
		debug_assert!(self.last().is_none_or(|last| pn > last), "insertions must increase");
		if let Some(chunk) = self.chunks.back_mut()
			&& let Some(slot) = chunk.slot(pn)
		{
			chunk.slots[slot] = Some(value);
			chunk.mask |= 1 << slot;
			return;
		}
		let mut slots = Box::new([const { None }; CHUNK]);
		slots[0] = Some(value);
		self.chunks.push_back(Chunk {
			offset: pn,
			slots,
			mask: 1,
		});
	}

	/// The lowest packet number present, and its entry.
	pub(super) fn first(&self) -> Option<(u64, &T)> {
		let chunk = self.chunks.front()?;
		let slot = chunk.mask.trailing_zeros() as usize;
		Some((chunk.offset + slot as u64, chunk.slots[slot].as_ref()?))
	}

	/// The highest packet number present.
	pub(super) fn last(&self) -> Option<u64> {
		let chunk = self.chunks.back()?;
		Some(chunk.offset + u64::from(63 - chunk.mask.leading_zeros()))
	}

	/// Remove and return the entry with the lowest packet number.
	pub(super) fn pop_first(&mut self) -> Option<T> {
		let pn = self.first()?.0;
		self.remove(pn)
	}

	pub(super) fn get(&self, pn: u64) -> Option<&T> {
		let chunk = &self.chunks[self.chunk(pn)?];
		chunk.slots[chunk.slot(pn)?].as_ref()
	}

	pub(super) fn remove(&mut self, pn: u64) -> Option<T> {
		let index = self.chunk(pn)?;
		let chunk = &mut self.chunks[index];
		let slot = chunk.slot(pn)?;
		let value = chunk.slots[slot].take()?;
		chunk.mask &= !(1 << slot);
		if chunk.mask == 0 {
			self.chunks.remove(index);
		}
		Some(value)
	}

	pub(super) fn is_empty(&self) -> bool {
		self.chunks.is_empty()
	}

	/// Present entries in `range`, in increasing packet-number order.
	pub(super) fn range(&self, range: impl RangeBounds<u64>) -> impl Iterator<Item = (u64, &T)> + '_ {
		let low = match range.start_bound() {
			Bound::Included(&pn) => pn,
			Bound::Excluded(&pn) => pn.saturating_add(1),
			Bound::Unbounded => 0,
		};
		let high = match range.end_bound() {
			Bound::Included(&pn) => pn.saturating_add(1),
			Bound::Excluded(&pn) => pn,
			Bound::Unbounded => u64::MAX,
		};
		let start = match self.chunks.front() {
			Some(chunk) if low < chunk.offset + CHUNK as u64 => 0,
			_ => self
				.chunks
				.partition_point(|chunk| chunk.offset <= low)
				.saturating_sub(1),
		};
		self.chunks
			.range(start..)
			.take_while(move |chunk| chunk.offset < high)
			.flat_map(move |chunk| {
				// The occupied slots at or above `low`.
				let from = low.saturating_sub(chunk.offset).min(CHUNK as u64) as u32;
				let mut mask = chunk.mask.checked_shr(from).map_or(0, |mask| mask << from);
				std::iter::from_fn(move || {
					if mask == 0 {
						return None;
					}
					let slot = mask.trailing_zeros() as usize;
					mask &= mask - 1;
					Some((chunk.offset + slot as u64, chunk.slots[slot].as_ref()?))
				})
			})
			.take_while(move |&(pn, _)| pn < high)
	}

	/// Mutably iterate present entries in increasing packet-number order.
	pub(super) fn values_mut(&mut self) -> impl Iterator<Item = &mut T> + '_ {
		self.chunks
			.iter_mut()
			.flat_map(|chunk| chunk.slots.iter_mut().filter_map(Option::as_mut))
	}

	/// Consume the slots, yielding present entries in increasing packet-number order.
	pub(super) fn into_values(self) -> impl Iterator<Item = T> {
		self.chunks
			.into_iter()
			.flat_map(|chunk| chunk.slots.into_iter().flatten())
	}

	/// Slots allocated, present or not.
	#[cfg(test)]
	pub(super) fn slots(&self) -> usize {
		self.chunks.len() * CHUNK
	}

	/// The index of the only chunk that may hold `pn`, checking the newest and oldest first.
	fn chunk(&self, pn: u64) -> Option<usize> {
		let last = self.chunks.len().checked_sub(1)?;
		if pn >= self.chunks[last].offset {
			return Some(last);
		}
		if pn < self.chunks[0].offset + CHUNK as u64 {
			return Some(0);
		}
		Some(self.chunks.partition_point(|chunk| chunk.offset <= pn) - 1)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	const N: u64 = CHUNK as u64;

	/// Entries present, as `(pn, value)` pairs in order.
	fn entries(slots: &Slots<u64>, range: impl RangeBounds<u64>) -> Vec<(u64, u64)> {
		slots.range(range).map(|(pn, &value)| (pn, value)).collect()
	}

	#[test]
	fn holes_cost_at_most_a_chunk() {
		let mut slots = Slots::default();
		slots.insert(0, 0);
		for pn in 1..100 * N {
			slots.insert(pn, pn);
			slots.remove(pn);
		}
		assert_eq!(slots.slots(), CHUNK);
		assert_eq!(entries(&slots, ..), vec![(0, 0)]);
	}

	#[test]
	fn emptied_middle_chunk_is_freed() {
		let mut slots = Slots::default();
		for pn in 0..3 * N {
			slots.insert(pn, pn);
		}
		for pn in N..2 * N {
			assert_eq!(slots.remove(pn), Some(pn));
		}
		assert_eq!(slots.slots(), 2 * CHUNK);
		assert!(slots.get(N).is_none());
		let expected: Vec<_> = (0..N).chain(2 * N..3 * N).map(|pn| (pn, pn)).collect();
		assert_eq!(entries(&slots, ..), expected);
	}

	#[test]
	fn gaps_between_and_within_chunks() {
		let mut slots = Slots::default();
		for pn in [0, 1, 10, 10 * N, 10 * N + 3, 30 * N] {
			slots.insert(pn, pn);
		}
		assert_eq!(slots.slots(), 3 * CHUNK);
		for pn in [0, 1, 10, 10 * N, 10 * N + 3, 30 * N] {
			assert_eq!(slots.get(pn), Some(&pn));
		}
		for pn in [2, 9, 11, N, 9 * N, 10 * N + 1, 20 * N, 31 * N] {
			assert!(slots.get(pn).is_none());
			assert!(slots.remove(pn).is_none());
		}
		assert_eq!(slots.last(), Some(30 * N));
		assert_eq!(entries(&slots, 5..=10 * N), vec![(10, 10), (10 * N, 10 * N)]);
		assert_eq!(
			entries(&slots, 10 * N + 1..),
			vec![(10 * N + 3, 10 * N + 3), (30 * N, 30 * N)]
		);
		assert_eq!(entries(&slots, 11..10 * N), vec![]);
		assert_eq!(entries(&slots, 31 * N..), vec![]);
	}

	#[test]
	fn removal_order_and_ends() {
		let mut slots = Slots::default();
		for pn in 0..500 {
			slots.insert(pn, pn);
		}
		for pn in (0..500).step_by(3) {
			assert_eq!(slots.remove(pn), Some(pn));
		}
		let expected: Vec<_> = (0..500).filter(|pn| pn % 3 != 0).map(|pn| (pn, pn)).collect();
		assert_eq!(entries(&slots, ..), expected);
		assert_eq!(slots.first(), Some((1, &1)));
		assert_eq!(slots.last(), Some(499));

		assert_eq!(slots.pop_first(), Some(1));
		assert_eq!(slots.pop_first(), Some(2));
		assert_eq!(slots.first(), Some((4, &4)));
		for value in slots.values_mut() {
			*value += 1000;
		}
		assert_eq!(slots.get(4), Some(&1004));
		let values: Vec<_> = slots.into_values().collect();
		assert_eq!(
			values,
			expected[2..].iter().map(|&(pn, _)| pn + 1000).collect::<Vec<_>>()
		);
	}

	#[test]
	fn drains_to_empty_and_reuses() {
		let mut slots = Slots::default();
		for pn in 0..2 * N {
			slots.insert(pn, pn);
		}
		while slots.pop_first().is_some() {}
		assert!(slots.is_empty());
		assert_eq!(slots.last(), None);
		slots.insert(5 * N, 1);
		assert_eq!(entries(&slots, ..), vec![(5 * N, 1)]);
	}
}
