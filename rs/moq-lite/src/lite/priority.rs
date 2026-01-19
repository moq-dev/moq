use std::{
	cmp::Ordering,
	collections::{BinaryHeap, HashMap},
	sync::{Arc, Mutex},
};

use tokio::sync::watch;

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
#[derive(Debug, Clone)]
struct PriorityItem {
	id: usize,
	track: u8,
	group: u64,
}

impl PartialEq for PriorityItem {
	fn eq(&self, other: &Self) -> bool {
		self.track == other.track && self.group == other.group
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
		// Higher track = higher priority, then higher group = higher priority
		// Reverse ordering so highest priority sorts first (index 0)
		other.track.cmp(&self.track).then(other.group.cmp(&self.group))
	}
}

#[derive(Clone, Default)]
pub struct PriorityQueue {
	state: Arc<Mutex<PriorityState>>,
}

impl PriorityQueue {
	// TODO Implement some sort of round robin between tracks with the same priority.
	// The Group ID should only be used to break ties within the same track.
	pub fn insert(&self, track: u8, group: u64) -> PriorityHandle {
		self.state.lock().unwrap().insert(track, group, self.clone())
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
	// Binary heap for overflow items (all report u8::MAX)
	overflow: BinaryHeap<PriorityItem>,
	// Track location and watch channel for each ID
	indexes: HashMap<usize, (Location, watch::Sender<u8>)>,
	next_id: usize,
}

impl PriorityState {
	pub fn insert(&mut self, track: u8, group: u64, myself: PriorityQueue) -> PriorityHandle {
		let id = self.next_id;
		self.next_id += 1;

		let item = PriorityItem { track, group, id };

		if self.vec.len() < MAX_VEC_SIZE {
			// Room in vec - binary search for insertion point
			let insert_pos = self.vec.binary_search(&item).unwrap_or_else(|pos| pos);
			let initial_priority = insert_pos.try_into().unwrap_or(u8::MAX);
			let (tx, rx) = watch::channel(initial_priority);

			self.vec.insert(insert_pos, item);
			self.indexes.insert(id, (Location::Vec(insert_pos), tx));

			// Update indices for items after the insertion point (their indices shifted by 1)
			self.update_indices_from(insert_pos + 1);

			return PriorityHandle { id, rx, queue: myself };
		}

		// Vec is full - check if this item should go in vec or overflow
		let lowest_in_vec = self.vec.last().unwrap();

		// Note: Ord is reversed for sorting (higher priority = "less than")
		// So item > lowest means item has LOWER priority
		if item > *lowest_in_vec {
			// Lower priority - goes to overflow
			let (tx, rx) = watch::channel(u8::MAX);

			self.overflow.push(item);
			self.indexes.insert(id, (Location::Overflow, tx));

			return PriorityHandle { id, rx, queue: myself };
		}

		// Higher priority than lowest in vec - replace lowest
		let removed = self.vec.pop().unwrap();
		Self::update_location(&mut self.indexes, removed.id, Location::Overflow);
		self.overflow.push(removed);

		let insert_pos = self.vec.binary_search(&item).unwrap_or_else(|pos| pos);
		let initial_priority = insert_pos.try_into().expect("only 255 items allowed");
		let (tx, rx) = watch::channel(initial_priority);

		self.vec.insert(insert_pos, item);
		self.indexes.insert(id, (Location::Vec(insert_pos), tx));

		// Update indices for items after the insertion point (their indices shifted by 1)
		self.update_indices_from(insert_pos + 1);

		PriorityHandle { id, rx, queue: myself }
	}

	fn update_indices_from(&mut self, start: usize) {
		for (idx, item) in self.vec.iter().enumerate().skip(start) {
			Self::update_location(&mut self.indexes, item.id, Location::Vec(idx));
		}
	}

	fn update_location(indexes: &mut HashMap<usize, (Location, watch::Sender<u8>)>, id: usize, location: Location) {
		let (loc, tx) = indexes.get_mut(&id).expect("item not in indexes");
		*loc = location;

		let new_priority = match loc {
			Location::Vec(idx) => (*idx).try_into().unwrap_or(u8::MAX),
			Location::Overflow => u8::MAX,
		};

		let _ = tx.send_if_modified(|p| {
			if *p != new_priority {
				*p = new_priority;
				true
			} else {
				false
			}
		});
	}

	fn remove(&mut self, id: usize) {
		let (location, _) = self.indexes.remove(&id).expect("item not in indexes");

		if let Location::Vec(pos) = location {
			self.vec.remove(pos);

			// Try to promote from overflow
			if let Some(overflow_item) = self.overflow.pop() {
				let overflow_id = overflow_item.id;
				self.vec.push(overflow_item);
				// Vec is still sorted because overflow item has lowest priority
				Self::update_location(&mut self.indexes, overflow_id, Location::Vec(self.vec.len() - 1));
			}

			// Update indices for items from removal point onward
			self.update_indices_from(pos);
		} else {
			// Not in vec, must be in overflow - need to remove from heap
			// BinaryHeap doesn't have retain, so rebuild it
			let original_len = self.overflow.len();
			self.overflow = self.overflow.drain().filter(|item| item.id != id).collect();

			assert_eq!(self.overflow.len(), original_len - 1, "item not found in overflow heap");
		}
	}
}

pub struct PriorityHandle {
	id: usize,
	rx: watch::Receiver<u8>,
	queue: PriorityQueue,
}

impl Drop for PriorityHandle {
	fn drop(&mut self) {
		self.queue.state.lock().unwrap().remove(self.id);
	}
}

impl PriorityHandle {
	pub fn current(&mut self) -> u8 {
		*self.rx.borrow_and_update()
	}

	pub async fn next(&mut self) -> u8 {
		let _ = self.rx.changed().await;
		*self.rx.borrow_and_update()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_single_item() {
		let queue = PriorityQueue::default();
		let mut handle = queue.insert(100, 5);
		assert_eq!(handle.current(), 0); // First item is always index 0
	}

	#[test]
	fn test_track_priority_ordering() {
		let queue = PriorityQueue::default();

		// Insert items with different track priorities
		let mut low = queue.insert(50, 0);
		let mut high = queue.insert(255, 0);
		let mut mid = queue.insert(100, 0);

		// With sorted vec, indices map exactly to priority order
		assert_eq!(high.current(), 0); // Highest priority
		assert_eq!(mid.current(), 1); // Middle priority
		assert_eq!(low.current(), 2); // Lowest priority
	}

	#[test]
	fn test_group_priority_on_same_track() {
		let queue = PriorityQueue::default();

		// Same track priority, different groups
		let mut group10 = queue.insert(100, 10);
		let mut group5 = queue.insert(100, 5);
		let mut group1 = queue.insert(100, 1);

		// Exact index mapping for sorted vec
		assert_eq!(group10.current(), 0);
		assert_eq!(group5.current(), 1);
		assert_eq!(group1.current(), 2);
	}

	#[test]
	fn test_track_priority_overrides_group() {
		let queue = PriorityQueue::default();

		// Lower track priority but higher group
		let mut low_track_high_group = queue.insert(50, 1000);
		// Higher track priority but lower group
		let mut high_track_low_group = queue.insert(255, 1);

		// Track priority should take precedence
		assert_eq!(high_track_low_group.current(), 0);
		assert_eq!(low_track_high_group.current(), 1);
	}

	#[test]
	fn test_removal_on_drop() {
		let queue = PriorityQueue::default();

		let mut first = queue.insert(255, 0);
		let mut second = queue.insert(100, 0);
		let mut third = queue.insert(50, 0);

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

		let mut first = queue.insert(255, 0);
		let mut second = queue.insert(100, 0);

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

		let mut first = queue.insert(255, 0);
		let mut second = queue.insert(100, 0);

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
		let mut handles: Vec<_> = (0..10).rev().map(|i| queue.insert(100, i)).collect();

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
		let mut handles: Vec<_> = (0..300).rev().map(|i| queue.insert(100, i)).collect();

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
		let mut high_track_high_group = queue.insert(255, 10);
		let mut high_track_low_group = queue.insert(255, 1);
		let mut mid_track_high_group = queue.insert(100, 5);
		let mut mid_track_low_group = queue.insert(100, 1);
		let mut low_track_high_group = queue.insert(50, 100);

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
		let mut fillers: Vec<_> = (0..255).rev().map(|i| queue.insert(100, i + 100)).collect();

		// This goes to overflow
		let mut overflow_item = queue.insert(100, 50);
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

		let mut h1 = queue.insert(200, 0);
		let h2 = queue.insert(150, 0);
		let mut h3 = queue.insert(100, 0);

		// h1 has highest priority
		assert_eq!(h1.current(), 0);

		drop(h2);

		// h1 should still be at top
		assert_eq!(h1.current(), 0);
		// h3 should have moved up
		assert!(h3.current() < 2);

		let mut h4 = queue.insert(250, 0);

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
		let mut h1 = queue.insert(100, 5);
		let mut h2 = queue.insert(100, 5);
		let mut h3 = queue.insert(100, 5);

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
		let mut root = queue.insert(255, 0);
		let left = queue.insert(100, 0);
		let mut right = queue.insert(100, 0);

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
			queue.insert(100, 5),
			queue.insert(200, 3),
			queue.insert(50, 10),
			queue.insert(200, 8),
			queue.insert(100, 1),
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
		let _fillers: Vec<_> = (0..254).map(|i| queue.insert(100, i + 100)).collect();

		// Insert one more that will be at the edge
		let mut at_edge = queue.insert(100, 50);
		assert_eq!(at_edge.current(), 254);

		// Spawn task to wait for demotion notification
		let task = tokio::spawn(async move { at_edge.next().await });

		tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

		// Insert very high priority item, kicking at_edge to overflow
		let _high = queue.insert(255, 1000);

		let new_priority = task.await.unwrap();
		assert_eq!(new_priority, u8::MAX, "Should be demoted to overflow");
	}

	#[test]
	fn test_empty_after_all_removed() {
		let queue = PriorityQueue::default();

		let h1 = queue.insert(100, 0);
		let h2 = queue.insert(200, 0);
		let h3 = queue.insert(50, 0);

		drop(h1);
		drop(h2);
		drop(h3);

		// Queue should be empty, next insert should get index 0
		let mut h4 = queue.insert(100, 0);
		assert_eq!(h4.current(), 0);
	}
}
