//! The groups fills hold for their readers, which eviction, shedding, and expiry pass over.
//!
//! A cache only makes reads faster, so a group a fill fetched because a reader missed it is
//! no cache entry until that reader takes it: a pool held past its capacity, which evicts
//! anything else at its next write, would otherwise evict the group before the reader gets
//! to it, and the reader would ask for it again, forever. So a taken fill holds a group from
//! the moment it lands until no reader of the fill still wants it.
//!
//! A fill fetches its next group only once a reader took every group it answered (see
//! [`super::fill`]), so it holds at most one group: the first in its order that it answered
//! and no reader took yet, or, while it waits on a run, the first group to land in that run.

use std::{
	collections::{BTreeMap, HashMap},
	ops::Range,
};

use rangemap::RangeMap;

use super::Order;

/// The holds of every fill a handler took, by fill id.
#[derive(Default)]
pub(super) struct Holds {
	fills: HashMap<u64, Fill>,
	/// The fill awaiting each key, so a group landing there is held as it lands.
	awaiting: RangeMap<u64, u64>,
	/// The fills holding each key.
	held: BTreeMap<u64, Vec<u64>>,
}

/// What one fill holds.
#[derive(Default)]
struct Fill {
	/// The first group in its order that it answered and no reader took yet.
	accepted: Option<u64>,
	/// The run it fetches next, and in which order.
	awaiting: Option<(Range<u64>, Order)>,
	/// The first group to land in `awaiting`, first in its order.
	landed: Option<u64>,
}

impl Fill {
	fn keys(&self) -> impl Iterator<Item = u64> + '_ {
		self.accepted.into_iter().chain(self.landed)
	}
}

/// A key whose hold began (`true`) or ended (`false`), for the track to pin or unpin its group.
pub(super) type Flip = (u64, bool);

impl Holds {
	/// Whether a fill holds `key`.
	pub fn holds(&self, key: u64) -> bool {
		self.held.contains_key(&key)
	}

	/// The fills holding `key`.
	pub fn holders(&self, key: u64) -> &[u64] {
		self.held.get(&key).map_or(&[], Vec::as_slice)
	}

	/// Whether fill `id` holds `accepted` and awaits `awaiting` already, so setting them would
	/// change nothing.
	pub fn is(&self, id: u64, accepted: Option<u64>, awaiting: &Option<(Range<u64>, Order)>) -> bool {
		match self.fills.get(&id) {
			Some(fill) => fill.accepted == accepted && fill.awaiting == *awaiting,
			None => accepted.is_none() && awaiting.is_none(),
		}
	}

	/// Fill `id` holds `accepted` and awaits `awaiting` from now on. The group that landed in
	/// what it awaited stays held while it is still in the run, or until the fill accepts it.
	/// Returns the keys whose hold changed.
	pub fn set(&mut self, id: u64, accepted: Option<u64>, awaiting: Option<(Range<u64>, Order)>) -> Vec<Flip> {
		let mut fill = self.fills.remove(&id).unwrap_or_default();
		let before: Vec<u64> = fill.keys().collect();
		if let Some((run, _)) = &fill.awaiting {
			self.awaiting.remove(run.clone());
		}
		fill.landed = fill.landed.filter(|landed| {
			accepted != Some(*landed) && awaiting.as_ref().is_some_and(|(run, _)| run.contains(landed))
		});
		fill.accepted = accepted;
		fill.awaiting = awaiting;
		if let Some((run, _)) = &fill.awaiting
			&& !run.is_empty()
		{
			self.awaiting.insert(run.clone(), id);
		}
		let after: Vec<u64> = fill.keys().collect();
		if !after.is_empty() || fill.awaiting.is_some() {
			self.fills.insert(id, fill);
		}
		self.count(id, &before, &after)
	}

	/// A group landed at `key`: the fill awaiting it holds it, if it holds nothing landed
	/// that comes before it. Returns the keys whose hold changed.
	pub fn land(&mut self, key: u64) -> Vec<Flip> {
		let Some(&id) = self.awaiting.get(&key) else {
			return Vec::new();
		};
		let fill = self.fills.get_mut(&id).expect("an awaited key names a fill");
		let order = fill.awaiting.as_ref().expect("awaited").1;
		let first = fill.landed.is_none_or(|landed| match order {
			Order::Ascending => key < landed,
			Order::Descending => key > landed,
		});
		if !first || fill.landed == Some(key) {
			return Vec::new();
		}
		let before: Vec<u64> = fill.keys().collect();
		fill.landed = Some(key);
		let after: Vec<u64> = fill.keys().collect();
		self.count(id, &before, &after)
	}

	/// Fill `id` holds nothing any more. Returns the keys whose hold ended.
	pub fn release(&mut self, id: u64) -> Vec<Flip> {
		self.set(id, None, None)
	}

	/// Move fill `id` from holding `before` to holding `after`, returning the keys whose
	/// first holder came or last one left.
	fn count(&mut self, id: u64, before: &[u64], after: &[u64]) -> Vec<Flip> {
		let mut flips = Vec::new();
		for key in after.iter().filter(|key| !before.contains(key)) {
			let holders = self.held.entry(*key).or_default();
			holders.push(id);
			if holders.len() == 1 {
				flips.push((*key, true));
			}
		}
		for key in before.iter().filter(|key| !after.contains(key)) {
			let Some(holders) = self.held.get_mut(key) else {
				continue;
			};
			holders.retain(|holder| *holder != id);
			if holders.is_empty() {
				self.held.remove(key);
				flips.push((*key, false));
			}
		}
		flips
	}

	/// How many groups fill `id` holds.
	#[cfg(test)]
	pub fn held_by(&self, id: u64) -> usize {
		self.fills.get(&id).map_or(0, |fill| fill.keys().count())
	}

	/// Every held key, with the fills holding it.
	#[cfg(all(test, feature = "async"))]
	pub fn held(&self) -> impl Iterator<Item = (u64, &[u64])> {
		self.held.iter().map(|(key, holders)| (*key, holders.as_slice()))
	}

	/// How many groups every fill holds.
	#[cfg(test)]
	pub fn len(&self) -> usize {
		self.held.len()
	}
}

#[cfg(test)]
mod test {
	use super::*;

	/// The first group to land in an awaited run is held, a later one in the fill's order is
	/// not, and an earlier one takes the hold over.
	#[test]
	fn the_first_group_to_land_is_held() {
		let mut holds = Holds::default();
		assert!(holds.set(1, None, Some((10..20, Order::Ascending))).is_empty());
		assert_eq!(holds.land(15), vec![(15, true)]);
		assert!(holds.land(17).is_empty(), "later in the fill's order");
		assert_eq!(holds.land(12), vec![(12, true), (15, false)]);
		assert!(holds.land(30).is_empty(), "awaited by no fill");
		assert_eq!(holds.held_by(1), 1);

		// Accepted, it stays held; a fill awaiting nothing holds no landed group.
		assert!(holds.set(1, Some(12), None).is_empty());
		assert_eq!(holds.release(1), vec![(12, false)]);
		assert_eq!(holds.len(), 0);
	}
}
