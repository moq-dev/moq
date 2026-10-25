#[cfg(test)]
use std::cmp::Ordering;
use std::{
	cmp,
	collections::{BTreeMap, btree_map},
	ops::{
		Bound::{Excluded, Included},
		Range,
	},
};

/// A set of u64 values optimized for long runs and random insert/delete/contains
#[derive(Debug, Default, Clone)]
pub(crate) struct RangeSet(BTreeMap<u64, u64>);

impl RangeSet {
	#[cfg(test)]
	pub(crate) fn new() -> Self {
		Default::default()
	}

	#[cfg(test)]
	pub(super) fn contains(&self, x: u64) -> bool {
		self.pred(x).is_some_and(|(_, end)| end > x)
	}

	#[cfg(test)]
	pub(super) fn insert_one(&mut self, x: u64) -> bool {
		if let Some((start, end)) = self.pred(x) {
			match end.cmp(&x) {
				// Wholly contained
				Ordering::Greater => {
					return false;
				}
				Ordering::Equal => {
					// Extend existing
					self.0.remove(&start);
					let mut new_end = x + 1;
					if let Some((next_start, next_end)) = self.succ(x)
						&& next_start == new_end
					{
						self.0.remove(&next_start);
						new_end = next_end;
					}
					self.0.insert(start, new_end);
					return true;
				}
				_ => {}
			}
		}
		let mut new_end = x + 1;
		if let Some((next_start, next_end)) = self.succ(x)
			&& next_start == new_end
		{
			self.0.remove(&next_start);
			new_end = next_end;
		}
		self.0.insert(x, new_end);
		true
	}

	pub(crate) fn insert(&mut self, mut x: Range<u64>) -> bool {
		if x.is_empty() {
			return false;
		}
		if let Some((start, end)) = self.pred(x.start) {
			if end >= x.end {
				// Wholly contained
				return false;
			} else if end >= x.start {
				// Extend overlapping predecessor
				self.0.remove(&start);
				x.start = start;
			}
		}
		while let Some((next_start, next_end)) = self.succ(x.start) {
			if next_start > x.end {
				break;
			}
			// Overlaps with successor
			self.0.remove(&next_start);
			x.end = cmp::max(next_end, x.end);
		}
		self.0.insert(x.start, x.end);
		true
	}

	/// Find closest range to `x` that begins at or before it
	fn pred(&self, x: u64) -> Option<(u64, u64)> {
		self.0
			.range((Included(0), Included(x)))
			.next_back()
			.map(|(&x, &y)| (x, y))
	}

	/// Find the closest range to `x` that begins after it
	fn succ(&self, x: u64) -> Option<(u64, u64)> {
		self.0
			.range((Excluded(x), Included(u64::MAX)))
			.next()
			.map(|(&x, &y)| (x, y))
	}

	pub(crate) fn remove(&mut self, x: Range<u64>) -> bool {
		if x.is_empty() {
			return false;
		}

		let before = match self.pred(x.start) {
			Some((start, end)) if end > x.start => {
				self.0.remove(&start);
				if start < x.start {
					self.0.insert(start, x.start);
				}
				if end > x.end {
					self.0.insert(x.end, end);
				}
				// Short-circuit if we cannot possibly overlap with another range
				if end >= x.end {
					return true;
				}
				true
			}
			Some(_) | None => false,
		};
		let mut after = false;
		while let Some((start, end)) = self.succ(x.start) {
			if start >= x.end {
				break;
			}
			after = true;
			self.0.remove(&start);
			if end > x.end {
				self.0.insert(x.end, end);
				break;
			}
		}
		before || after
	}

	#[cfg(test)]
	pub(super) fn add(&mut self, other: &Self) {
		for (&start, &end) in &other.0 {
			self.insert(start..end);
		}
	}

	#[cfg(test)]
	pub(super) fn subtract(&mut self, other: &Self) {
		for (&start, &end) in &other.0 {
			self.remove(start..end);
		}
	}

	pub(crate) fn is_empty(&self) -> bool {
		self.0.is_empty()
	}

	pub(crate) fn min(&self) -> Option<u64> {
		self.0.first_key_value().map(|(&start, _)| start)
	}

	#[cfg(test)]
	pub(super) fn max(&self) -> Option<u64> {
		self.0.last_key_value().map(|(_, &end)| end - 1)
	}

	#[cfg(test)]
	pub(super) fn len(&self) -> usize {
		self.0.len()
	}

	pub(crate) fn iter(&self) -> Iter<'_> {
		Iter(self.0.iter())
	}

	/// The ranges overlapping `x`, in order, without visiting those wholly before it
	pub(crate) fn overlapping(&self, x: Range<u64>) -> impl Iterator<Item = Range<u64>> + '_ {
		let first = self
			.pred(x.start)
			.filter(|&(_, end)| end > x.start)
			.map_or(x.start, |(start, _)| start);
		let end = if x.is_empty() { first } else { x.end };
		self.0.range(first..end).map(|(&start, &end)| start..end)
	}

	#[cfg(test)]
	pub(super) fn elts(&self) -> EltIter<'_> {
		EltIter {
			inner: self.0.iter(),
			next: 0,
			end: 0,
		}
	}

	pub(crate) fn peek_min(&self) -> Option<Range<u64>> {
		let (&start, &end) = self.0.iter().next()?;
		Some(start..end)
	}

	pub(crate) fn pop_min(&mut self) -> Option<Range<u64>> {
		let result = self.peek_min()?;
		self.0.remove(&result.start);
		Some(result)
	}
}

pub(crate) struct Iter<'a>(btree_map::Iter<'a, u64, u64>);

impl Iterator for Iter<'_> {
	type Item = Range<u64>;
	fn next(&mut self) -> Option<Range<u64>> {
		let (&start, &end) = self.0.next()?;
		Some(start..end)
	}
}

impl DoubleEndedIterator for Iter<'_> {
	fn next_back(&mut self) -> Option<Range<u64>> {
		let (&start, &end) = self.0.next_back()?;
		Some(start..end)
	}
}

impl<'a> IntoIterator for &'a RangeSet {
	type Item = Range<u64>;
	type IntoIter = Iter<'a>;
	fn into_iter(self) -> Iter<'a> {
		self.iter()
	}
}

#[cfg(test)]
pub(crate) struct EltIter<'a> {
	inner: btree_map::Iter<'a, u64, u64>,
	next: u64,
	end: u64,
}

#[cfg(test)]
impl Iterator for EltIter<'_> {
	type Item = u64;
	fn next(&mut self) -> Option<u64> {
		if self.next == self.end {
			let (&start, &end) = self.inner.next()?;
			self.next = start;
			self.end = end;
		}
		let x = self.next;
		self.next += 1;
		Some(x)
	}
}

#[cfg(test)]
impl DoubleEndedIterator for EltIter<'_> {
	fn next_back(&mut self) -> Option<u64> {
		if self.next == self.end {
			let (&start, &end) = self.inner.next_back()?;
			self.next = start;
			self.end = end;
		}
		self.end -= 1;
		Some(self.end)
	}
}
