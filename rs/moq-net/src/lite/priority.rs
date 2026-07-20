use std::{
	cmp::{Ordering, Reverse},
	collections::{BinaryHeap, HashMap},
	sync::{Arc, Mutex},
	task::Poll,
};

// Hybrid priority queue that provides strict priority ordering for the top 255 items.
//
// Design:
// - Top 255 items are stored in a sorted Vec where index maps directly to priority (0 = highest)
// - Items beyond 255 go into a BinaryHeap overflow and all report u8::MAX
// - On insert: binary search into Vec if room, else check if higher priority than lowest in Vec
// - On remove from Vec: pop highest priority item from overflow heap to backfill
// - On remove from overflow: rebuild heap (rare case, acceptable O(n) cost)
//
// Priority ordering: higher track value = higher priority, then higher group value = higher priority

/// A priority composed of a track-level priority and a group sequence number.
/// Higher `track` is always preferred; `group` only breaks ties within the same track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Priority {
	pub track: u8,
	pub group: u64,
}

impl Priority {
	pub fn new(track: u8, group: u64) -> Self {
		Self { track, group }
	}
}

impl Ord for Priority {
	fn cmp(&self, other: &Self) -> Ordering {
		// Reverse ordering so highest priority sorts first (index 0)
		other.track.cmp(&self.track).then(other.group.cmp(&self.group))
	}
}

impl PartialOrd for Priority {
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

#[derive(Debug, Clone)]
struct PriorityItem {
	id: usize,
	priority: Priority,
}

impl PartialEq for PriorityItem {
	fn eq(&self, other: &Self) -> bool {
		self.priority == other.priority
	}
}

impl Eq for PriorityItem {}

impl PartialOrd for PriorityItem {
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

impl Ord for PriorityItem {
	fn cmp(&self, other: &Self) -> Ordering {
		self.priority.cmp(&other.priority)
	}
}

#[derive(Clone, Default)]
pub struct PriorityQueue {
	state: Arc<Mutex<PriorityState>>,
}

impl PriorityQueue {
	// TODO Implement some sort of round robin between tracks with the same priority.
	// The Group ID should only be used to break ties within the same track.
	pub fn insert(&self, priority: Priority) -> PriorityHandle {
		self.state.lock().unwrap().insert(priority, self.clone())
	}
}

const MAX_VEC_SIZE: usize = 255;

enum Location {
	Vec(usize), // Index in the sorted vec
	Overflow,   // In the overflow heap
}

#[derive(Default)]
struct PriorityState {
	// Sorted vec for top 255 items (index 0 = highest priority)
	vec: Vec<PriorityItem>,
	// Binary heap for overflow items (all report u8::MAX). Wrapped in `Reverse`
	// because PriorityItem's Ord is itself reversed (higher priority sorts as
	// less); BinaryHeap is a max-heap, so without the wrapper `pop()` would
	// return the *lowest*-priority overflow item, not the highest. With the
	// wrapper, `pop()` returns the next item that should be promoted into vec.
	overflow: BinaryHeap<Reverse<PriorityItem>>,
	// Track location and notification channel for each ID
	indexes: HashMap<usize, (Location, kio::Producer<u8>)>,
	next_id: usize,
}

impl PriorityState {
	pub fn insert(&mut self, priority: Priority, myself: PriorityQueue) -> PriorityHandle {
		let id = self.next_id;
		self.next_id += 1;

		// Pre-register the channel so `place` can update it via `update_location`.
		// The initial value is overwritten as soon as `place` decides where the item lands.
		let tx = kio::Producer::new(u8::MAX);
		let rx = tx.consume();
		self.indexes.insert(id, (Location::Overflow, tx));
		self.place(PriorityItem { id, priority });

		PriorityHandle {
			id,
			priority,
			rx,
			seen: u8::MAX,
			queue: myself,
		}
	}

	fn update_indices_from(&mut self, start: usize) {
		for (idx, item) in self.vec.iter().enumerate().skip(start) {
			Self::update_location(&mut self.indexes, item.id, Location::Vec(idx));
		}
	}

	fn update_location(indexes: &mut HashMap<usize, (Location, kio::Producer<u8>)>, id: usize, location: Location) {
		let (loc, tx) = indexes.get_mut(&id).expect("item not in indexes");
		*loc = location;

		let new_priority = match loc {
			Location::Vec(idx) => (*idx).try_into().unwrap_or(u8::MAX),
			Location::Overflow => u8::MAX,
		};

		// Only touch the write guard on a real change, so unchanged items don't wake.
		// Read first and drop the guard: the write below takes the same lock.
		let current = *tx.read();
		if current != new_priority
			&& let Ok(mut value) = tx.write()
		{
			*value = new_priority;
		}
	}

	// Place an item into vec or overflow based on its priority, updating the HashMap
	// location and notifying watch channels. The item's id must already be present in
	// `self.indexes`; the entry's location is overwritten here.
	fn place(&mut self, item: PriorityItem) {
		let id = item.id;

		if self.vec.len() < MAX_VEC_SIZE {
			// Note: Ord is reversed (higher priority = "less than"), so `top < item`
			// means `top` has higher priority. If an overflow item outranks the one
			// we're placing, swap them so the higher-priority item lands in vec.
			// This case only arises via `set_priority`: a fresh insert can't reach
			// here with non-empty overflow because the invariant "every overflow
			// item has lower priority than every vec item" is maintained on insert.
			if let Some(Reverse(top)) = self.overflow.peek()
				&& *top < item
			{
				let Reverse(promoted) = self.overflow.pop().unwrap();
				self.overflow.push(Reverse(item));
				Self::update_location(&mut self.indexes, id, Location::Overflow);

				let insert_pos = self.vec.binary_search(&promoted).unwrap_or_else(|pos| pos);
				let promoted_id = promoted.id;
				self.vec.insert(insert_pos, promoted);
				Self::update_location(&mut self.indexes, promoted_id, Location::Vec(insert_pos));
				self.update_indices_from(insert_pos + 1);
				return;
			}

			let insert_pos = self.vec.binary_search(&item).unwrap_or_else(|pos| pos);
			self.vec.insert(insert_pos, item);
			Self::update_location(&mut self.indexes, id, Location::Vec(insert_pos));
			self.update_indices_from(insert_pos + 1);
			return;
		}

		// Note: Ord is reversed for sorting (higher priority = "less than"),
		// so item > lowest_in_vec means item has lower priority than the tail.
		let lowest_in_vec = self.vec.last().unwrap();
		if item > *lowest_in_vec {
			self.overflow.push(Reverse(item));
			Self::update_location(&mut self.indexes, id, Location::Overflow);
			return;
		}

		// Higher priority than the tail of vec: demote the tail into overflow.
		let removed = self.vec.pop().unwrap();
		Self::update_location(&mut self.indexes, removed.id, Location::Overflow);
		self.overflow.push(Reverse(removed));

		let insert_pos = self.vec.binary_search(&item).unwrap_or_else(|pos| pos);
		self.vec.insert(insert_pos, item);
		Self::update_location(&mut self.indexes, id, Location::Vec(insert_pos));
		self.update_indices_from(insert_pos + 1);
	}

	// Pull an item out of vec/overflow, returning it. The HashMap entry is left in place;
	// callers must either drop it (true removal) or call `place` again (reinsertion).
	fn extract(&mut self, id: usize) -> PriorityItem {
		let (location, _) = self.indexes.get(&id).expect("item not in indexes");

		match location {
			Location::Vec(idx) => {
				let idx = *idx;
				let item = self.vec.remove(idx);
				self.update_indices_from(idx);
				item
			}
			Location::Overflow => {
				// BinaryHeap has no O(log N) random removal, so drain and rebuild.
				// Acceptable because overflow removal is rare (only when handle drops
				// or a set_priority targets an item that has been demoted past index 254).
				let mut found = None;
				let drained: Vec<_> = self.overflow.drain().collect();
				for Reverse(entry) in drained {
					if entry.id == id && found.is_none() {
						found = Some(entry);
					} else {
						self.overflow.push(Reverse(entry));
					}
				}
				found.expect("item not found in overflow heap")
			}
		}
	}

	fn set_priority(&mut self, id: usize, new_priority: Priority) {
		let mut item = self.extract(id);
		item.priority = new_priority;
		self.place(item);
	}

	fn remove(&mut self, id: usize) {
		let was_in_vec = matches!(self.indexes.get(&id), Some((Location::Vec(_), _)));
		self.extract(id);
		self.indexes.remove(&id);

		// If we removed from vec, promote the highest-priority overflow item to backfill.
		// The overflow item still has lower priority than every existing vec entry, so it
		// belongs at the tail and the vec stays sorted.
		if was_in_vec && let Some(Reverse(overflow_item)) = self.overflow.pop() {
			let overflow_id = overflow_item.id;
			self.vec.push(overflow_item);
			Self::update_location(&mut self.indexes, overflow_id, Location::Vec(self.vec.len() - 1));
		}
	}
}

pub struct PriorityHandle {
	id: usize,
	priority: Priority,
	rx: kio::Consumer<u8>,
	/// Last value observed via [`current`](Self::current)/[`next`](Self::next), so
	/// `next` fires only on a real change.
	seen: u8,
	queue: PriorityQueue,
}

impl Drop for PriorityHandle {
	fn drop(&mut self) {
		self.queue.state.lock().unwrap().remove(self.id);
	}
}

impl PriorityHandle {
	pub fn current(&mut self) -> u8 {
		self.seen = *self.rx.read();
		self.seen
	}

	/// Poll for a priority change since the last observed value.
	///
	/// The queue holds the producer while this handle is registered, so closure is
	/// unreachable; it parks rather than spinning a caller's poll loop.
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<u8> {
		let seen = self.seen;
		match self.rx.poll(waiter, |value| {
			if **value != seen {
				Poll::Ready(**value)
			} else {
				Poll::Pending
			}
		}) {
			Poll::Ready(Ok(value)) => {
				self.seen = value;
				Poll::Ready(value)
			}
			Poll::Ready(Err(_)) | Poll::Pending => Poll::Pending,
		}
	}

	#[cfg(test)]
	pub async fn next(&mut self) -> u8 {
		kio::wait(|waiter| self.poll_next(waiter)).await
	}

	/// Change this item's track priority and re-sort the queue.
	/// No-op if the track value hasn't changed.
	pub fn set_track(&mut self, new_track: u8) {
		if self.priority.track == new_track {
			return;
		}
		self.priority.track = new_track;
		self.queue.state.lock().unwrap().set_priority(self.id, self.priority);
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_single_item() {
		let queue = PriorityQueue::default();
		let mut handle = queue.insert(Priority::new(100, 5));
		assert_eq!(handle.current(), 0); // First item is always index 0
	}

	#[test]
	fn test_track_priority_ordering() {
		let queue = PriorityQueue::default();

		// Insert items with different track priorities
		let mut low = queue.insert(Priority::new(50, 0));
		let mut high = queue.insert(Priority::new(255, 0));
		let mut mid = queue.insert(Priority::new(100, 0));

		// With sorted vec, indices map exactly to priority order
		assert_eq!(high.current(), 0); // Highest priority
		assert_eq!(mid.current(), 1); // Middle priority
		assert_eq!(low.current(), 2); // Lowest priority
	}

	#[test]
	fn test_group_priority_on_same_track() {
		let queue = PriorityQueue::default();

		// Same track priority, different groups
		let mut group10 = queue.insert(Priority::new(100, 10));
		let mut group5 = queue.insert(Priority::new(100, 5));
		let mut group1 = queue.insert(Priority::new(100, 1));

		// Exact index mapping for sorted vec
		assert_eq!(group10.current(), 0);
		assert_eq!(group5.current(), 1);
		assert_eq!(group1.current(), 2);
	}

	#[test]
	fn test_track_priority_overrides_group() {
		let queue = PriorityQueue::default();

		// Lower track priority but higher group
		let mut low_track_high_group = queue.insert(Priority::new(50, 1000));
		// Higher track priority but lower group
		let mut high_track_low_group = queue.insert(Priority::new(255, 1));

		// Track priority should take precedence
		assert_eq!(high_track_low_group.current(), 0);
		assert_eq!(low_track_high_group.current(), 1);
	}

	#[test]
	fn test_removal_on_drop() {
		let queue = PriorityQueue::default();

		let mut first = queue.insert(Priority::new(255, 0));
		let mut second = queue.insert(Priority::new(100, 0));
		let mut third = queue.insert(Priority::new(50, 0));

		assert_eq!(first.current(), 0);
		assert_eq!(second.current(), 1);
		assert_eq!(third.current(), 2);

		// Drop the middle item
		drop(second);

		// Remaining items should reorder
		assert_eq!(first.current(), 0);
		assert_eq!(third.current(), 1);
	}

	#[test]
	fn test_removal_of_highest_priority() {
		let queue = PriorityQueue::default();

		let mut first = queue.insert(Priority::new(255, 0));
		let mut second = queue.insert(Priority::new(100, 0));

		assert_eq!(first.current(), 0);
		assert_eq!(second.current(), 1);

		// Drop highest priority item
		drop(first);

		// Second should become index 0
		assert_eq!(second.current(), 0);
	}

	#[test]
	fn test_removal_of_lowest_priority() {
		let queue = PriorityQueue::default();

		let mut first = queue.insert(Priority::new(255, 0));
		let mut second = queue.insert(Priority::new(100, 0));

		assert_eq!(first.current(), 0);
		assert_eq!(second.current(), 1);

		// Drop lowest priority item
		drop(second);

		// First should remain at index 0
		assert_eq!(first.current(), 0);
	}

	#[test]
	fn test_many_items_with_same_priority() {
		let queue = PriorityQueue::default();

		// Insert items from high to low group to make them ordered in heap
		let mut handles: Vec<_> = (0..10).rev().map(|i| queue.insert(Priority::new(100, i))).collect();

		// Highest group (9, at handles[0]) should be at heap index 0
		assert_eq!(handles[0].current(), 0);

		// All items should have valid indices
		for handle in handles.iter_mut() {
			assert!(handle.current() < 10);
		}
	}

	#[test]
	fn test_max_priority_value_overflow() {
		let queue = PriorityQueue::default();

		// Insert more than 255 items (insert high to low so first item is highest priority)
		let mut handles: Vec<_> = (0..300).rev().map(|i| queue.insert(Priority::new(100, i))).collect();

		// Highest priority item (group=299, handles[0]) should be at heap index 0
		assert_eq!(handles[0].current(), 0);

		// Items beyond heap index 255 should report u8::MAX
		let mut low_priority_count = 0;
		for handle in handles.iter_mut() {
			if handle.current() == u8::MAX {
				low_priority_count += 1;
			}
		}
		assert!(low_priority_count > 0, "Should have some items beyond u8::MAX index");
		assert_eq!(low_priority_count, 45, "Exactly 45 items should overflow (300-255)");
	}

	#[test]
	fn test_complex_ordering() {
		let queue = PriorityQueue::default();

		// Mix of different track priorities and groups
		let mut high_track_high_group = queue.insert(Priority::new(255, 10));
		let mut high_track_low_group = queue.insert(Priority::new(255, 1));
		let mut mid_track_high_group = queue.insert(Priority::new(100, 5));
		let mut mid_track_low_group = queue.insert(Priority::new(100, 1));
		let mut low_track_high_group = queue.insert(Priority::new(50, 100));

		// Exact index mapping with sorted vec
		assert_eq!(high_track_high_group.current(), 0); // track=255, group=10
		assert_eq!(high_track_low_group.current(), 1); // track=255, group=1
		assert_eq!(mid_track_high_group.current(), 2); // track=100, group=5
		assert_eq!(mid_track_low_group.current(), 3); // track=100, group=1
		assert_eq!(low_track_high_group.current(), 4); // track=50, group=100
	}

	#[tokio::test]
	async fn test_watch_notification_on_overflow_promotion() {
		let queue = PriorityQueue::default();

		// Fill vec to capacity
		let mut fillers: Vec<_> = (0..255)
			.rev()
			.map(|i| queue.insert(Priority::new(100, i + 100)))
			.collect();

		// This goes to overflow
		let mut overflow_item = queue.insert(Priority::new(100, 50));
		assert_eq!(overflow_item.current(), u8::MAX);

		// Spawn task to wait for promotion from overflow
		let task = tokio::spawn(async move { overflow_item.next().await });

		// Give the task time to start waiting
		tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

		// Drop highest priority item, which should promote from overflow
		fillers.remove(0);

		// Task should complete with new priority (not u8::MAX anymore)
		let result = task.await.unwrap();
		assert!(result < u8::MAX, "Should be promoted from overflow");
	}

	#[test]
	fn test_interleaved_insertions_and_removals() {
		let queue = PriorityQueue::default();

		let mut h1 = queue.insert(Priority::new(200, 0));
		let h2 = queue.insert(Priority::new(150, 0));
		let mut h3 = queue.insert(Priority::new(100, 0));

		// h1 has highest priority
		assert_eq!(h1.current(), 0);

		drop(h2);

		// h1 should still be at top
		assert_eq!(h1.current(), 0);
		// h3 should have moved up
		assert!(h3.current() < 2);

		let mut h4 = queue.insert(Priority::new(250, 0));

		// h4 has highest priority now
		assert_eq!(h4.current(), 0);
		// h1 should have shifted to index 1
		assert_eq!(h1.current(), 1);

		drop(h4);

		// h1 should be back at top
		assert_eq!(h1.current(), 0);
	}

	#[test]
	fn test_same_track_and_group() {
		let queue = PriorityQueue::default();

		// Items with identical track and group should still be ordered consistently
		let mut h1 = queue.insert(Priority::new(100, 5));
		let mut h2 = queue.insert(Priority::new(100, 5));
		let mut h3 = queue.insert(Priority::new(100, 5));

		// All three should have valid indices
		let indices = [h1.current(), h2.current(), h3.current()];
		assert_eq!(indices.len(), 3);
		assert!(indices.contains(&0));
		assert!(indices.contains(&1));
		assert!(indices.contains(&2));
	}

	#[test]
	fn test_removal_updates_siblings() {
		let queue = PriorityQueue::default();

		// Create a heap with known structure
		let mut root = queue.insert(Priority::new(255, 0));
		let left = queue.insert(Priority::new(100, 0));
		let mut right = queue.insert(Priority::new(100, 0));

		assert_eq!(root.current(), 0);

		// Remove left child
		drop(left);

		// Root should stay at 0
		assert_eq!(root.current(), 0);
		// Right child should have shifted to index 1
		assert_eq!(right.current(), 1);
	}

	#[test]
	fn test_heap_property_maintained() {
		let queue = PriorityQueue::default();

		// Insert in random order
		let mut handles = vec![
			queue.insert(Priority::new(100, 5)),
			queue.insert(Priority::new(200, 3)),
			queue.insert(Priority::new(50, 10)),
			queue.insert(Priority::new(200, 8)),
			queue.insert(Priority::new(100, 1)),
		];

		// Verify highest priority is at index 0
		// track=200, group=8 should be highest
		assert_eq!(handles[3].current(), 0);

		// Remove highest priority
		drop(handles.remove(3));

		// Next highest should now be at 0 (track=200, group=3)
		assert_eq!(handles[1].current(), 0);
	}

	#[tokio::test]
	async fn test_notification_on_demotion_to_overflow() {
		let queue = PriorityQueue::default();

		// Fill vec to capacity - 1
		let _fillers: Vec<_> = (0..254).map(|i| queue.insert(Priority::new(100, i + 100))).collect();

		// Insert one more that will be at the edge
		let mut at_edge = queue.insert(Priority::new(100, 50));
		assert_eq!(at_edge.current(), 254);

		// Spawn task to wait for demotion notification
		let task = tokio::spawn(async move { at_edge.next().await });

		tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

		// Insert very high priority item, kicking at_edge to overflow
		let _high = queue.insert(Priority::new(255, 1000));

		let new_priority = task.await.unwrap();
		assert_eq!(new_priority, u8::MAX, "Should be demoted to overflow");
	}

	#[test]
	fn test_empty_after_all_removed() {
		let queue = PriorityQueue::default();

		let h1 = queue.insert(Priority::new(100, 0));
		let h2 = queue.insert(Priority::new(200, 0));
		let h3 = queue.insert(Priority::new(50, 0));

		drop(h1);
		drop(h2);
		drop(h3);

		// Queue should be empty, next insert should get index 0
		let mut h4 = queue.insert(Priority::new(100, 0));
		assert_eq!(h4.current(), 0);
	}

	#[test]
	fn test_set_track_reorders() {
		let queue = PriorityQueue::default();

		// Subscription 1 (track=255), Subscription 2 (track=55)
		let mut s1_g1 = queue.insert(Priority::new(255, 1));
		let mut s1_g2 = queue.insert(Priority::new(255, 2));
		let mut s2_g1 = queue.insert(Priority::new(55, 1));
		let mut s2_g2 = queue.insert(Priority::new(55, 2));

		assert_eq!(s1_g2.current(), 0); // s1 highest
		assert_eq!(s1_g1.current(), 1);
		assert_eq!(s2_g2.current(), 2); // s2 lowest
		assert_eq!(s2_g1.current(), 3);

		// Swap track priorities for each handle individually.
		s1_g1.set_track(55);
		s1_g2.set_track(55);
		s2_g1.set_track(255);
		s2_g2.set_track(255);

		assert_eq!(s2_g2.current(), 0); // s2 now highest
		assert_eq!(s2_g1.current(), 1);
		assert_eq!(s1_g2.current(), 2); // s1 now lowest
		assert_eq!(s1_g1.current(), 3);
	}

	#[tokio::test]
	async fn test_set_track_notifies_other_handles() {
		let queue = PriorityQueue::default();

		// h_low at index 1, will be promoted to 0 when h_high is demoted.
		let mut h_high = queue.insert(Priority::new(255, 1));
		let mut h_low = queue.insert(Priority::new(50, 1));

		assert_eq!(h_low.current(), 1);

		// Wait for a change notification on h_low while another handle's set_track runs.
		let task = tokio::spawn(async move { h_low.next().await });
		tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

		// Demote h_high below h_low.
		h_high.set_track(10);

		let new_priority = task.await.unwrap();
		assert_eq!(new_priority, 0, "h_low should be promoted to the top");
	}

	#[test]
	fn test_set_track_self() {
		let queue = PriorityQueue::default();

		let mut h_high = queue.insert(Priority::new(255, 1));
		let mut h_mid = queue.insert(Priority::new(100, 1));
		let mut h_low = queue.insert(Priority::new(50, 1));

		assert_eq!(h_high.current(), 0);
		assert_eq!(h_mid.current(), 1);
		assert_eq!(h_low.current(), 2);

		// Demote h_high below the others.
		h_high.set_track(10);

		assert_eq!(h_mid.current(), 0);
		assert_eq!(h_low.current(), 1);
		assert_eq!(h_high.current(), 2);
	}

	#[test]
	fn test_set_track_swaps_demoted_vec_item_with_overflow() {
		let queue = PriorityQueue::default();

		// Fill vec with 255 items at track=100, groups 1..=255.
		// f1 (group=1) is the vec tail (lowest priority of the fillers).
		let mut fillers: Vec<_> = (1..=255u64).map(|g| queue.insert(Priority::new(100, g))).collect();

		// Insert a higher-track item; this kicks f1 out of vec into overflow.
		let mut top = queue.insert(Priority::new(200, 0));
		assert_eq!(top.current(), 0);
		assert_eq!(fillers[0].current(), u8::MAX, "f1 was kicked into overflow");

		// Lower top's track below every filler. Without the swap, top would land
		// in vec at the tail while f1 stays in overflow despite having higher
		// priority, breaking the "every overflow item < every vec item" invariant.
		top.set_track(0);

		assert!(fillers[0].current() < u8::MAX, "f1 should be promoted back into vec");
		assert_eq!(top.current(), u8::MAX, "demoted top should land in overflow");
	}

	#[test]
	fn test_set_track_lowered_within_vec_no_overflow_disruption() {
		let queue = PriorityQueue::default();

		// Three items, all in vec; no overflow involvement.
		let mut a = queue.insert(Priority::new(200, 0));
		let mut b = queue.insert(Priority::new(100, 0));
		let mut c = queue.insert(Priority::new(50, 0));
		assert_eq!(a.current(), 0);
		assert_eq!(b.current(), 1);
		assert_eq!(c.current(), 2);

		// Lowering A's priority below B but above C should leave A at index 1.
		a.set_track(75);
		assert_eq!(b.current(), 0);
		assert_eq!(a.current(), 1);
		assert_eq!(c.current(), 2);
	}

	#[test]
	fn test_remove_promotes_highest_priority_overflow_item() {
		let queue = PriorityQueue::default();

		// Fill vec to capacity with track=200.
		let fillers: Vec<_> = (100..355u64).map(|g| queue.insert(Priority::new(200, g))).collect();

		// Three overflow items with distinct priorities (same track, different groups).
		let mut low = queue.insert(Priority::new(100, 1));
		let mut mid = queue.insert(Priority::new(100, 2));
		let mut high = queue.insert(Priority::new(100, 3));
		assert_eq!(low.current(), u8::MAX);
		assert_eq!(mid.current(), u8::MAX);
		assert_eq!(high.current(), u8::MAX);

		// Drop every vec item; overflow items must move into vec in priority order
		// (highest first).
		drop(fillers);

		assert_eq!(
			high.current(),
			0,
			"highest-priority overflow item should land at index 0"
		);
		assert_eq!(mid.current(), 1);
		assert_eq!(low.current(), 2);
	}

	#[tokio::test]
	async fn test_set_track_notifies_swapped_overflow_item() {
		tokio::time::pause();
		let queue = PriorityQueue::default();

		// Fill vec, then insert top, kicking f1 (filler at group=1) into overflow.
		let mut fillers: Vec<_> = (1..=255u64).map(|g| queue.insert(Priority::new(100, g))).collect();
		let mut top = queue.insert(Priority::new(200, 0));
		assert_eq!(top.current(), 0);

		// Take ownership of f1 so we can await its promotion notification.
		let mut f1 = fillers.remove(0);
		assert_eq!(f1.current(), u8::MAX);

		let task = tokio::spawn(async move { f1.next().await });
		tokio::task::yield_now().await;

		// Demoting top below every filler swaps it with f1 in overflow.
		top.set_track(0);

		let promoted = task.await.unwrap();
		assert!(promoted < u8::MAX, "f1 should be notified of promotion");
	}
}
