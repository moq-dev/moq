use std::{
	cmp::Ordering,
	collections::BTreeSet,
	task::{Poll, Waker},
};

use slab::Slab;

use crate::track::Order;

// Hybrid priority queue that provides strict priority ordering for the top 255 items.
//
// Design:
// - Top 255 items are stored in a sorted Vec where index maps directly to priority (0 = highest)
// - Items beyond 255 go into a BTreeSet overflow and all report u8::MAX
// - On insert: binary search into Vec if room, else check if higher priority than lowest in Vec
// - On remove from Vec: pop highest priority item from overflow to backfill
// - On remove from overflow: remove its priority-and-id key in O(log n)
//
// Priority ordering: higher track value = higher priority, then the publisher's priority,
// then subscription, then group sequence in that subscription's own direction.

/// A group stream's rank: its subscription's track priority, the publisher's priority for
/// the track, the subscription it belongs to, its group sequence in that subscription's
/// [`Order`], and the first object it carries.
///
/// Higher `track` always wins, and a higher `publisher` breaks a tie between two
/// subscriptions at the same `track`, so hang audio goes ahead of video when a subscriber
/// asks for both at one priority. `group` breaks ties only *within* one `subscribe`:
/// newest first by default, so a congested track sheds its backlog rather than its live
/// edge, or oldest first for an ascending subscription draining one.
///
/// Scoping the tie-break to a subscription is what keeps group sequence from deciding
/// between tracks. The draft gives that job to `Priority` alone, so a session carrying
/// two tracks at equal priority interleaves them rather than draining one first.
///
/// `start` only splits two streams of one group: the one starting earlier goes first,
/// since the rest of a group is undecodable without its start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Priority {
	track: i8,
	publisher: i8,
	subscribe: u64,
	order: Order,
	group: u64,
	start: u64,
}

impl Priority {
	/// Rank a group for transmission: the newest group of a track transmits first.
	pub fn new(track: i8, subscribe: u64, group: u64) -> Self {
		Self {
			track,
			publisher: 0,
			subscribe,
			order: Order::Descending,
			group,
			start: 0,
		}
	}

	/// Break a tie between subscriptions at the same track priority by the publisher's.
	pub fn with_publisher(mut self, publisher: i8) -> Self {
		self.publisher = publisher;
		self
	}

	/// Rank groups of the same subscription in `order` instead.
	pub fn with_order(mut self, order: Order) -> Self {
		self.order = order;
		self
	}

	/// Rank a stream that starts at object `start` of its group, behind any earlier start.
	pub fn with_start(mut self, start: u64) -> Self {
		self.start = start;
		self
	}
}

impl Ord for Priority {
	fn cmp(&self, other: &Self) -> Ordering {
		// Reverse ordering so highest priority sorts first (index 0)
		other
			.track
			.cmp(&self.track)
			.then(other.publisher.cmp(&self.publisher))
			// Which subscription wins a priority tie is arbitrary, but it has to be
			// stable and independent of either one's direction. The older (lower id)
			// subscription goes first.
			.then(self.subscribe.cmp(&other.subscribe))
			// Only an update in flight leaves one subscription's groups in two orders, and
			// keeping the two apart keeps the comparison total meanwhile.
			.then((self.order == Order::Ascending).cmp(&(other.order == Order::Ascending)))
			// The subscription's own direction: newest first sheds a congested backlog
			// rather than the live edge, oldest first drains it.
			.then(match self.order {
				Order::Descending => other.group.cmp(&self.group),
				Order::Ascending => self.group.cmp(&other.group),
			})
			// Two streams of one group, such as a joining prefix beside the live rest: the
			// earlier objects first, whatever the order.
			.then(self.start.cmp(&other.start))
	}
}

impl PartialOrd for Priority {
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

/// A stream's send order within its subscription's send group, where higher goes first.
///
/// The [`Priority`] ranking minus the parts the group itself carries: the group sequence in
/// the subscription's `order`, then a group's first stream ahead of a later `start` in the
/// same group. The low bit only splits a stream from the start of its group (`start == 0`)
/// from the rest, which is the joining prefix beside the live stream; any other two streams
/// of one group share. Sequences are wire varints below 2^62, so doubling one fits.
pub fn in_group(group: u64, start: u64, order: Order) -> i64 {
	let group = i64::try_from(group).unwrap_or(i64::MAX).saturating_mul(2);
	let group = match order {
		Order::Descending => group,
		Order::Ascending => -group,
	};
	group.saturating_add(i64::from(start == 0))
}

/// A subscription's send group priority: its subscriber priority, then the publisher's.
///
/// Packed so the subscriber's always dominates, exactly as [`Priority`] ranks them.
pub fn group(subscriber: i8, publisher: i8) -> i64 {
	i64::from(subscriber) * 256 + i64::from(publisher)
}

#[derive(Debug, Clone)]
struct PriorityItem {
	id: usize,
	priority: Priority,
}

impl PartialEq for PriorityItem {
	fn eq(&self, other: &Self) -> bool {
		self.priority == other.priority && self.id == other.id
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
		self.priority.cmp(&other.priority).then(self.id.cmp(&other.id))
	}
}

// One lock for the whole queue, and the wakes are the entries' own business, so this
// is `kio::Lock` rather than a watch channel: there is no single "the state changed"
// event to publish. A reorder shifts a run of entries but changes the rank of only
// some of them, and each entry wakes its own group when its rank actually moves.
#[derive(Clone)]
pub struct PriorityQueue {
	state: kio::Lock<PriorityState>,
}

impl Default for PriorityQueue {
	fn default() -> Self {
		Self {
			state: kio::Lock::new(PriorityState::default()),
		}
	}
}

impl PriorityQueue {
	// TODO Implement some sort of round robin between tracks with the same priority.
	pub fn insert(&self, priority: Priority) -> PriorityHandle {
		self.lock().insert(priority, self.clone())
	}

	/// Lock the queue for a mutation, waking whatever it reranked once the lock is back
	/// open. Every caller goes through this, so a rerank can't leave a group parked.
	fn lock(&self) -> Guard<'_> {
		Guard {
			lock: &self.state,
			state: Some(self.state.lock()),
		}
	}
}

/// A locked queue whose drop wakes the groups the mutation reranked.
///
/// The wake has to happen with the lock open. A [`Waker`] is allowed to poll its task
/// inline, and that task's first move is [`PriorityHandle::poll_next`], which takes this
/// same non-reentrant lock. kio's own channels drain before waking for the same reason.
struct Guard<'a> {
	lock: &'a kio::Lock<PriorityState>,
	// An Option so the lock can be released before the wakers run.
	state: Option<kio::LockGuard<'a, PriorityState>>,
}

impl Drop for Guard<'_> {
	fn drop(&mut self) {
		let mut state = self.state.take().expect("guard already dropped");
		if state.pending.is_empty() {
			return;
		}

		let mut pending = std::mem::take(&mut state.pending);
		drop(state);

		for waker in pending.drain(..) {
			waker.wake();
		}

		// Hand the buffer back so the next reorder reuses its capacity.
		self.lock.lock().pending = pending;
	}
}

impl std::ops::Deref for Guard<'_> {
	type Target = PriorityState;

	fn deref(&self) -> &Self::Target {
		self.state.as_ref().expect("guard already dropped")
	}
}

impl std::ops::DerefMut for Guard<'_> {
	fn deref_mut(&mut self) -> &mut Self::Target {
		self.state.as_mut().expect("guard already dropped")
	}
}

const MAX_VEC_SIZE: usize = 255;

enum Location {
	Vec(usize), // Index in the sorted vec
	Overflow,   // In the overflow set
}

struct PriorityEntry {
	location: Location,
	priority: Priority,
	/// The rank this entry last published, so a reorder that shifts an entry without
	/// changing its rank wakes nobody.
	rank: u8,
	/// The group serving this entry, parked until its rank moves. One waker rather than
	/// a `kio::WaiterList`, because exactly one `GroupServe` ever polls an entry, and a
	/// `kio::Producer` per queued group would be an allocation and a second mutex each.
	waker: Option<Waker>,
}

#[derive(Default)]
struct PriorityState {
	// Sorted vec for top 255 items (index 0 = highest priority)
	vec: Vec<PriorityItem>,
	// Sorted overflow items, so arbitrary removal and promoting the highest-ranked
	// item are both O(log n).
	overflow: BTreeSet<PriorityItem>,
	// Track location and priority for each reusable ID.
	entries: Slab<PriorityEntry>,
	// Wakers the mutation in progress pulled out, woken by `Guard::drop` once the lock
	// is open. Kept here so its capacity survives across reorders.
	pending: Vec<Waker>,
}

impl PriorityState {
	pub fn insert(&mut self, priority: Priority, myself: PriorityQueue) -> PriorityHandle {
		let id = self.entries.insert(PriorityEntry {
			location: Location::Overflow,
			priority,
			rank: u8::MAX,
			waker: None,
		});
		self.place(PriorityItem { id, priority });

		PriorityHandle {
			id,
			track: priority.track,
			order: priority.order,
			seen: u8::MAX,
			queue: myself,
		}
	}

	fn update_indices_from(&mut self, start: usize) {
		for (idx, item) in self.vec.iter().enumerate().skip(start) {
			Self::update_location(&mut self.entries, &mut self.pending, item.id, Location::Vec(idx));
		}
	}

	/// Move an entry, queueing its group for a wake if that changed the rank it reports.
	///
	/// Takes the fields rather than `&mut self` so a shift can walk `vec` while writing
	/// through `entries`. A reorder moves a run of entries and only queues the ones whose
	/// rank lands somewhere new, which is what keeps a shift from re-polling the session's
	/// every stream machine.
	fn update_location(entries: &mut Slab<PriorityEntry>, pending: &mut Vec<Waker>, id: usize, location: Location) {
		let entry = entries.get_mut(id).expect("item not in entries");
		entry.location = location;

		let rank = Self::rank_of(&entry.location);
		if entry.rank != rank {
			entry.rank = rank;
			pending.extend(entry.waker.take());
		}
	}

	fn rank_of(location: &Location) -> u8 {
		match location {
			Location::Vec(idx) => (*idx).try_into().unwrap_or(u8::MAX),
			Location::Overflow => u8::MAX,
		}
	}

	fn rank(&self, id: usize) -> u8 {
		Self::rank_of(&self.entries.get(id).expect("item not in entries").location)
	}

	// Place an item into vec or overflow based on its priority, updating the entry
	// location and waking whatever that reranked. The item's id must already be present
	// in `self.entries`; the entry's location is overwritten here.
	fn place(&mut self, item: PriorityItem) {
		let id = item.id;
		self.entries[id].priority = item.priority;

		if self.vec.len() < MAX_VEC_SIZE {
			// Note: Ord is reversed (higher priority = "less than"), so `top < item`
			// means `top` has higher priority. If an overflow item outranks the one
			// we're placing, swap them so the higher-priority item lands in vec.
			// This case only arises via a re-rank: a fresh insert can't reach
			// here with non-empty overflow because the invariant "every overflow
			// item has lower priority than every vec item" is maintained on insert.
			if let Some(top) = self.overflow.first()
				&& *top < item
			{
				let promoted = self.overflow.pop_first().unwrap();
				assert!(self.overflow.insert(item));
				Self::update_location(&mut self.entries, &mut self.pending, id, Location::Overflow);

				let insert_pos = self.vec.binary_search(&promoted).unwrap_or_else(|pos| pos);
				let promoted_id = promoted.id;
				self.vec.insert(insert_pos, promoted);
				Self::update_location(
					&mut self.entries,
					&mut self.pending,
					promoted_id,
					Location::Vec(insert_pos),
				);
				self.update_indices_from(insert_pos + 1);
				return;
			}

			let insert_pos = self.vec.binary_search(&item).unwrap_or_else(|pos| pos);
			self.vec.insert(insert_pos, item);
			Self::update_location(&mut self.entries, &mut self.pending, id, Location::Vec(insert_pos));
			self.update_indices_from(insert_pos + 1);
			return;
		}

		// Note: Ord is reversed for sorting (higher priority = "less than"),
		// so item > lowest_in_vec means item has lower priority than the tail.
		let lowest_in_vec = self.vec.last().unwrap();
		if item > *lowest_in_vec {
			assert!(self.overflow.insert(item));
			Self::update_location(&mut self.entries, &mut self.pending, id, Location::Overflow);
			return;
		}

		// Higher priority than the tail of vec: demote the tail into overflow.
		let removed = self.vec.pop().unwrap();
		let removed_id = removed.id;
		assert!(self.overflow.insert(removed));
		Self::update_location(&mut self.entries, &mut self.pending, removed_id, Location::Overflow);

		let insert_pos = self.vec.binary_search(&item).unwrap_or_else(|pos| pos);
		self.vec.insert(insert_pos, item);
		Self::update_location(&mut self.entries, &mut self.pending, id, Location::Vec(insert_pos));
		self.update_indices_from(insert_pos + 1);
	}

	// Pull an item out of vec/overflow, returning it. The slab entry is left in place;
	// callers must either drop it (true removal) or call `place` again (reinsertion).
	fn extract(&mut self, id: usize) -> PriorityItem {
		let location = &self.entries.get(id).expect("item not in entries").location;

		match location {
			Location::Vec(idx) => {
				let idx = *idx;
				let item = self.vec.remove(idx);
				self.update_indices_from(idx);
				item
			}
			Location::Overflow => {
				let priority = self.entries[id].priority;
				self.overflow
					.take(&PriorityItem { id, priority })
					.expect("item not found in overflow set")
			}
		}
	}

	/// Change one item's track priority and order, leaving the rest of its rank alone.
	fn steer(&mut self, id: usize, track: i8, order: Order) {
		let mut item = self.extract(id);
		item.priority.track = track;
		item.priority.order = order;
		self.place(item);
	}

	fn remove(&mut self, id: usize) {
		let was_in_vec = matches!(
			self.entries.get(id).map(|entry| &entry.location),
			Some(Location::Vec(_))
		);
		self.extract(id);
		self.entries.remove(id);

		// If we removed from vec, promote the highest-priority overflow item to backfill.
		// The overflow item still has lower priority than every existing vec entry, so it
		// belongs at the tail and the vec stays sorted.
		if was_in_vec && let Some(overflow_item) = self.overflow.pop_first() {
			let overflow_id = overflow_item.id;
			self.vec.push(overflow_item);
			let tail = self.vec.len() - 1;
			Self::update_location(&mut self.entries, &mut self.pending, overflow_id, Location::Vec(tail));
		}
	}
}

pub struct PriorityHandle {
	id: usize,
	/// This item's track priority and order, the only parts of its rank a handle owns;
	/// the rest is fixed when the item is queued.
	track: i8,
	order: Order,
	/// Last value observed via [`current`](Self::current)/[`next`](Self::next), so
	/// `next` fires only on a real change.
	seen: u8,
	queue: PriorityQueue,
}

impl Drop for PriorityHandle {
	fn drop(&mut self) {
		self.queue.lock().remove(self.id);
	}
}

impl PriorityHandle {
	pub fn current(&mut self) -> u8 {
		let rank = self.queue.state.lock().rank(self.id);
		self.seen = rank;
		rank
	}

	/// A queue rank as a transport send order, where HIGHER values are transmitted
	/// first (the W3C `sendOrder` / quinn convention, and the model's
	/// `Subscription::priority` semantics). The queue rank is the opposite
	/// (0 = most urgent), so this is the one place the two conventions meet; every
	/// other rank this handle hands out stays in the queue's own direction.
	pub fn send_order_of(rank: u8) -> u8 {
		u8::MAX - rank
	}

	/// This item's current rank as a send order.
	pub fn send_order(&mut self) -> u8 {
		Self::send_order_of(self.current())
	}

	/// Poll for a rank change since the last observed value.
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<u8> {
		// A plain read lock: parking stores a waker, which is not a rerank, so nothing
		// here can queue a wake and the `Guard` would have nothing to do.
		let mut state = self.queue.state.lock();
		let entry = state.entries.get_mut(self.id).expect("item not in entries");

		if entry.rank == self.seen {
			let waker = waiter.waker();
			if !entry.waker.as_ref().is_some_and(|parked| parked.will_wake(waker)) {
				entry.waker = Some(waker.clone());
			}
			return Poll::Pending;
		}

		let rank = entry.rank;
		drop(state);

		self.seen = rank;
		Poll::Ready(rank)
	}

	#[cfg(test)]
	pub async fn next(&mut self) -> u8 {
		kio::wait(|waiter| self.poll_next(waiter)).await
	}

	/// Change this item's track priority and order and re-sort the queue, returning its
	/// new rank. No-op if neither changed.
	pub fn steer(&mut self, track: i8, order: Order) -> u8 {
		if self.track == track && self.order == order {
			return self.current();
		}
		self.track = track;
		self.order = order;

		let rank = {
			let mut state = self.queue.lock();
			state.steer(self.id, track, order);
			state.rank(self.id)
		};

		self.seen = rank;
		rank
	}
}

/// The receive priority of a control stream, kept over every other stream when the
/// receive window fills: nothing downstream proceeds without it, and it is small.
///
/// A stream not yet identified keeps the transport's default of 0, which every group
/// stream sits below.
pub const RECV_CONTROL: i64 = i64::MAX;

/// The receive priority of a group stream: always below a control stream or an
/// unidentified one, ranked by its subscription's `priority` and then its `order`, like the
/// sender ranks it. A [`Order::Descending`] subscription gives up its oldest group first,
/// since a live track that falls behind skips to its newest anyway; an [`Order::Ascending`]
/// one drains a backlog in sequence, so its newest group goes first.
pub fn recv_group(priority: i8, sequence: u64, order: Order) -> i64 {
	// Saturating the sequence at 2^48 keeps the band negative; no track gets that far.
	const SEQUENCE: u32 = 48;
	const MAX: i64 = (1 << SEQUENCE) - 1;
	let sequence = (sequence.min(MAX as u64)) as i64;
	let kept = match order {
		Order::Descending => sequence,
		Order::Ascending => MAX - sequence,
	};
	i64::MIN / 2 + (i64::from(priority) << SEQUENCE) + kept
}

#[cfg(test)]
mod tests {
	use std::sync::{
		Arc,
		atomic::{AtomicUsize, Ordering as AtomicOrdering},
	};
	use std::task::Wake;

	use super::*;

	/// A live subscription's group rank, on the one subscription these tests share.
	/// Group order within a subscription is what most of them exercise, so scoping
	/// them together is what keeps them testing it.
	fn live(track: i8, group: u64) -> Priority {
		Priority::new(track, 0, group)
	}

	struct WakeCount(AtomicUsize);

	impl Wake for WakeCount {
		fn wake(self: Arc<Self>) {
			self.0.fetch_add(1, AtomicOrdering::Relaxed);
		}
	}

	/// Park every handle on a waiter of its own, the way `kio::Tasks` gives each
	/// group serve its own slot waker. A shared waiter would dedup the wakes and hide
	/// exactly what these assert.
	struct Parked {
		handles: Vec<PriorityHandle>,
		wakes: Vec<Arc<WakeCount>>,
		waiters: Vec<kio::Waiter>,
	}

	impl Parked {
		fn new(handles: Vec<PriorityHandle>) -> Self {
			let wakes: Vec<_> = handles
				.iter()
				.map(|_| Arc::new(WakeCount(AtomicUsize::new(0))))
				.collect();
			let waiters = wakes
				.iter()
				.map(|wake| kio::Waiter::new(std::task::Waker::from(wake.clone())))
				.collect();

			let mut parked = Self {
				handles,
				wakes,
				waiters,
			};
			parked.park();
			parked
		}

		fn park(&mut self) {
			for (handle, waiter) in self.handles.iter_mut().zip(&self.waiters) {
				handle.current();
				assert!(handle.poll_next(waiter).is_pending());
			}
		}

		fn counts(&self) -> Vec<usize> {
			self.wakes.iter().map(|w| w.0.load(AtomicOrdering::Relaxed)).collect()
		}
	}

	/// Every group sits below an unidentified stream, and so below a control stream, with
	/// more urgent subscriptions and then newer groups kept longer.
	#[test]
	fn recv_group_stays_below_unidentified_streams() {
		for order in [Order::Descending, Order::Ascending] {
			assert!(recv_group(0, u64::MAX, order) < recv_group(1, 0, order));
			assert!(recv_group(0, 0, order) < recv_group(1, u64::MAX, order));
			assert!(recv_group(i8::MIN, u64::MAX, order) < recv_group(-1, 0, order));
			assert!(recv_group(i8::MAX, 0, order) < 0);
			assert!(recv_group(i8::MAX, u64::MAX, order) < 0);
			assert!(recv_group(i8::MIN, 0, order) > i64::MIN);
			assert!(recv_group(i8::MIN, u64::MAX, order) > i64::MIN);
		}
		// The group the subscription would deliver last is the first to go.
		assert!(recv_group(0, 0, Order::Descending) < recv_group(0, 1, Order::Descending));
		assert!(recv_group(0, 1, Order::Ascending) < recv_group(0, 0, Order::Ascending));
	}

	/// A group whose rank didn't move must not be re-polled. Every queued group parks on
	/// its own waker, so a queue-wide notification would drag every stream machine in the
	/// session through a pass that finds nothing changed.
	#[test]
	fn a_reorder_wakes_only_the_ranks_that_moved() {
		let queue = PriorityQueue::default();
		let parked = Parked::new((0..64).map(|group| queue.insert(live(-28, group))).collect());

		// Lowest priority yet, so it lands at the tail and shifts nobody.
		let tail = queue.insert(live(-78, 0));
		assert_eq!(parked.counts(), vec![0; 64], "a tail insert moved no other rank");

		// Highest priority yet, so every existing entry shifts down one.
		let front = queue.insert(live(i8::MAX, u64::MAX));
		assert_eq!(parked.counts(), vec![1; 64], "a front insert moved every other rank");

		drop((tail, front));
	}

	/// Overflow entries all report `u8::MAX`, so removing one changes nobody's rank.
	#[test]
	fn an_overflow_removal_wakes_nobody() {
		let queue = PriorityQueue::default();
		let fillers = (0..MAX_VEC_SIZE as u64).map(|group| queue.insert(live(72, group)));
		let overflow = (0..2).map(|group| queue.insert(live(-28, group)));
		let mut parked = Parked::new(fillers.chain(overflow).collect());

		let doomed = parked.handles.pop().expect("overflow handle");
		assert_eq!(doomed.seen, u8::MAX, "the dropped entry was in overflow");
		drop(doomed);

		let survivors = &parked.counts()[..parked.handles.len()];
		assert!(
			survivors.iter().all(|&count| count == 0),
			"an overflow removal woke {survivors:?}"
		);
	}

	#[test]
	fn test_single_item() {
		let queue = PriorityQueue::default();
		let mut handle = queue.insert(live(-28, 5));
		assert_eq!(handle.current(), 0); // First item is always index 0
	}

	/// The transport sends HIGHER values first, so the most urgent rank (0) must map
	/// to the highest send order and overflow (u8::MAX) to the lowest. Emitting the
	/// raw rank would transmit the stalest stream first.
	#[test]
	fn test_send_order_inverts_rank() {
		let queue = PriorityQueue::default();
		let mut top = queue.insert(live(72, 0));
		let mut low = queue.insert(live(-28, 0));

		assert_eq!(top.current(), 0);
		assert_eq!(top.send_order(), 255, "most urgent rank gets the highest send order");
		assert_eq!(low.current(), 1);
		assert_eq!(low.send_order(), 254);
	}

	#[test]
	fn test_track_priority_ordering() {
		let queue = PriorityQueue::default();

		// Insert items with different track priorities
		let mut low = queue.insert(live(-78, 0));
		let mut high = queue.insert(live(127, 0));
		let mut mid = queue.insert(live(-28, 0));

		// With sorted vec, indices map exactly to priority order
		assert_eq!(high.current(), 0); // Highest priority
		assert_eq!(mid.current(), 1); // Middle priority
		assert_eq!(low.current(), 2); // Lowest priority
	}

	#[test]
	fn test_group_priority_on_same_track() {
		let queue = PriorityQueue::default();

		// Same track priority, different groups
		let mut group10 = queue.insert(live(-28, 10));
		let mut group5 = queue.insert(live(-28, 5));
		let mut group1 = queue.insert(live(-28, 1));

		// Exact index mapping for sorted vec
		assert_eq!(group10.current(), 0);
		assert_eq!(group5.current(), 1);
		assert_eq!(group1.current(), 2);
	}

	/// `Ord` has to hold for every value: the queue sorts a shared vec, and a
	/// comparator that says `a < b` and `b < a` is free to panic `sort`.
	#[test]
	fn test_ord_is_total() {
		let mixed = [
			Priority::new(-28, 7, 1),
			Priority::new(-28, 7, 2),
			Priority::new(-28, 7, 1).with_order(Order::Ascending),
			Priority::new(-28, 7, 2).with_order(Order::Ascending),
			Priority::new(-28, 8, 1),
			Priority::new(-28, 8, 1).with_publisher(5),
			Priority::new(72, 7, 1),
		];

		for a in mixed {
			for b in mixed {
				// Antisymmetric, and agreeing with Eq on what equality means.
				assert_eq!(a.cmp(&b), b.cmp(&a).reverse(), "{a:?} vs {b:?}");
				assert_eq!(a.cmp(&b) == Ordering::Equal, a == b, "{a:?} vs {b:?}");
				for c in mixed {
					if a.cmp(&b) != Ordering::Greater && b.cmp(&c) != Ordering::Greater {
						assert_ne!(a.cmp(&c), Ordering::Greater, "{a:?} <= {b:?} <= {c:?}");
					}
				}
			}
		}
	}

	#[test]
	fn test_ascending_ranks_oldest_first() {
		let queue = PriorityQueue::default();
		let ascending = |group| Priority::new(-28, 0, group).with_order(Order::Ascending);
		let mut group5 = queue.insert(ascending(5));
		let mut group1 = queue.insert(ascending(1));
		let mut group10 = queue.insert(ascending(10));

		assert_eq!(group1.current(), 0);
		assert_eq!(group5.current(), 1);
		assert_eq!(group10.current(), 2);

		// Steering the subscription back to descending flips it once every group moved.
		for group in [&mut group1, &mut group5, &mut group10] {
			group.steer(-28, Order::Descending);
		}
		assert_eq!(group10.current(), 0);
		assert_eq!(group5.current(), 1);
		assert_eq!(group1.current(), 2);
	}

	/// Two streams of one group rank by the object each starts at, earliest first in either
	/// order, and only after the group itself.
	#[test]
	fn test_earlier_start_ranks_first_within_a_group() {
		for order in [Order::Descending, Order::Ascending] {
			let queue = PriorityQueue::default();
			let stream = |group, start| Priority::new(-28, 0, group).with_order(order).with_start(start);
			// Queued first, so the slab id alone would rank it ahead.
			let mut rest = queue.insert(stream(5, 3));
			let mut head = queue.insert(stream(5, 0));
			let mut other = queue.insert(stream(4, 0));

			let expected = match order {
				Order::Descending => (0, 1, 2),
				Order::Ascending => (1, 2, 0),
			};
			assert_eq!(
				(head.current(), rest.current(), other.current()),
				expected,
				"{order:?}: the start of a group goes first, after the group order"
			);
		}
	}

	/// The publisher's priority breaks a tie between subscriptions at one track priority, and
	/// only a tie: the subscriber's priority still wins outright.
	#[test]
	fn test_publisher_priority_breaks_ties() {
		let queue = PriorityQueue::default();
		// Older subscriptions win a full tie, so the tiebreak has to beat that.
		let mut video = queue.insert(Priority::new(0, 1, 9).with_publisher(20));
		let mut audio = queue.insert(Priority::new(0, 2, 1).with_publisher(40));
		let mut urgent = queue.insert(Priority::new(1, 3, 1).with_publisher(i8::MIN));

		assert_eq!(urgent.current(), 0, "subscriber priority dominates");
		assert_eq!(audio.current(), 1, "a higher publisher priority wins the tie");
		assert_eq!(video.current(), 2);
	}

	#[test]
	fn test_track_priority_overrides_group() {
		let queue = PriorityQueue::default();

		// Lower track priority but higher group
		let mut low_track_high_group = queue.insert(live(-78, 1000));
		// Higher track priority but lower group
		let mut high_track_low_group = queue.insert(live(127, 1));

		// Track priority should take precedence
		assert_eq!(high_track_low_group.current(), 0);
		assert_eq!(low_track_high_group.current(), 1);
	}

	#[test]
	fn test_removal_on_drop() {
		let queue = PriorityQueue::default();

		let mut first = queue.insert(live(127, 0));
		let mut second = queue.insert(live(-28, 0));
		let mut third = queue.insert(live(-78, 0));

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

		let mut first = queue.insert(live(127, 0));
		let mut second = queue.insert(live(-28, 0));

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

		let mut first = queue.insert(live(127, 0));
		let mut second = queue.insert(live(-28, 0));

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
		let mut handles: Vec<_> = (0..10).rev().map(|i| queue.insert(live(-28, i))).collect();

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
		let mut handles: Vec<_> = (0..300).rev().map(|i| queue.insert(live(-28, i))).collect();

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
		let mut high_track_high_group = queue.insert(live(127, 10));
		let mut high_track_low_group = queue.insert(live(127, 1));
		let mut mid_track_high_group = queue.insert(live(-28, 5));
		let mut mid_track_low_group = queue.insert(live(-28, 1));
		let mut low_track_high_group = queue.insert(live(-78, 100));

		// Exact index mapping with sorted vec
		assert_eq!(high_track_high_group.current(), 0); // track=127, group=10
		assert_eq!(high_track_low_group.current(), 1); // track=127, group=1
		assert_eq!(mid_track_high_group.current(), 2); // track=-28, group=5
		assert_eq!(mid_track_low_group.current(), 3); // track=-28, group=1
		assert_eq!(low_track_high_group.current(), 4); // track=-78, group=100
	}

	#[kio::test]
	async fn test_watch_notification_on_overflow_promotion() {
		let queue = PriorityQueue::default();

		// Fill vec to capacity
		let mut fillers: Vec<_> = (0..255).rev().map(|i| queue.insert(live(-28, i + 100))).collect();

		// This goes to overflow
		let mut overflow_item = queue.insert(live(-28, 50));
		assert_eq!(overflow_item.current(), u8::MAX);

		// Spawn task to wait for promotion from overflow
		let task = kio::clock::sim::spawn(async move { overflow_item.next().await });

		// Give the task time to start waiting
		kio::clock::sleep(std::time::Duration::from_millis(10)).await;

		// Drop highest priority item, which should promote from overflow
		fillers.remove(0);

		// Task should complete with new priority (not u8::MAX anymore)
		let result = task.await.unwrap();
		assert!(result < u8::MAX, "Should be promoted from overflow");
	}

	#[test]
	fn test_interleaved_insertions_and_removals() {
		let queue = PriorityQueue::default();

		let mut h1 = queue.insert(live(72, 0));
		let h2 = queue.insert(live(22, 0));
		let mut h3 = queue.insert(live(-28, 0));

		// h1 has highest priority
		assert_eq!(h1.current(), 0);

		drop(h2);

		// h1 should still be at top
		assert_eq!(h1.current(), 0);
		// h3 should have moved up
		assert!(h3.current() < 2);

		let mut h4 = queue.insert(live(122, 0));

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
		let mut h1 = queue.insert(live(-28, 5));
		let mut h2 = queue.insert(live(-28, 5));
		let mut h3 = queue.insert(live(-28, 5));

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
		let mut root = queue.insert(live(127, 0));
		let left = queue.insert(live(-28, 0));
		let mut right = queue.insert(live(-28, 0));

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
			queue.insert(live(-28, 5)),
			queue.insert(live(72, 3)),
			queue.insert(live(-78, 10)),
			queue.insert(live(72, 8)),
			queue.insert(live(-28, 1)),
		];

		// Verify highest priority is at index 0
		// track=72, group=8 should be highest
		assert_eq!(handles[3].current(), 0);

		// Remove highest priority
		drop(handles.remove(3));

		// Next highest should now be at 0 (track=72, group=3)
		assert_eq!(handles[1].current(), 0);
	}

	#[kio::test]
	async fn test_notification_on_demotion_to_overflow() {
		let queue = PriorityQueue::default();

		// Fill vec to capacity - 1
		let _fillers: Vec<_> = (0..254).map(|i| queue.insert(live(-28, i + 100))).collect();

		// Insert one more that will be at the edge
		let mut at_edge = queue.insert(live(-28, 50));
		assert_eq!(at_edge.current(), 254);

		// Spawn task to wait for demotion notification
		let task = kio::clock::sim::spawn(async move { at_edge.next().await });

		kio::clock::sleep(std::time::Duration::from_millis(10)).await;

		// Insert very high priority item, kicking at_edge to overflow
		let _high = queue.insert(live(127, 1000));

		let new_priority = task.await.unwrap();
		assert_eq!(new_priority, u8::MAX, "Should be demoted to overflow");
	}

	#[test]
	fn test_empty_after_all_removed() {
		let queue = PriorityQueue::default();

		let h1 = queue.insert(live(-28, 0));
		let h2 = queue.insert(live(72, 0));
		let h3 = queue.insert(live(-78, 0));

		drop(h1);
		drop(h2);
		drop(h3);

		// Queue should be empty, next insert should get index 0
		let mut h4 = queue.insert(live(-28, 0));
		assert_eq!(h4.current(), 0);
	}

	#[test]
	fn test_steer_track_reorders() {
		let queue = PriorityQueue::default();

		// Subscription 1 (track=127), Subscription 2 (track=-73)
		let mut s1_g1 = queue.insert(live(127, 1));
		let mut s1_g2 = queue.insert(live(127, 2));
		let mut s2_g1 = queue.insert(live(-73, 1));
		let mut s2_g2 = queue.insert(live(-73, 2));

		assert_eq!(s1_g2.current(), 0); // s1 highest
		assert_eq!(s1_g1.current(), 1);
		assert_eq!(s2_g2.current(), 2); // s2 lowest
		assert_eq!(s2_g1.current(), 3);

		// Swap track priorities for each handle individually.
		s1_g1.steer(-73, Order::Descending);
		s1_g2.steer(-73, Order::Descending);
		s2_g1.steer(127, Order::Descending);
		s2_g2.steer(127, Order::Descending);

		assert_eq!(s2_g2.current(), 0); // s2 now highest
		assert_eq!(s2_g1.current(), 1);
		assert_eq!(s1_g2.current(), 2); // s1 now lowest
		assert_eq!(s1_g1.current(), 3);
	}

	#[kio::test]
	async fn test_steer_track_notifies_other_handles() {
		let queue = PriorityQueue::default();

		// h_low at index 1, will be promoted to 0 when h_high is demoted.
		let mut h_high = queue.insert(live(127, 1));
		let mut h_low = queue.insert(live(-78, 1));

		assert_eq!(h_low.current(), 1);

		// Wait for a change notification on h_low while another handle's steer runs.
		let task = kio::clock::sim::spawn(async move { h_low.next().await });
		kio::clock::sleep(std::time::Duration::from_millis(10)).await;

		// Demote h_high below h_low.
		h_high.steer(-118, Order::Descending);

		let new_priority = task.await.unwrap();
		assert_eq!(new_priority, 0, "h_low should be promoted to the top");
	}

	#[test]
	fn test_steer_track_self() {
		let queue = PriorityQueue::default();

		let mut h_high = queue.insert(live(127, 1));
		let mut h_mid = queue.insert(live(-28, 1));
		let mut h_low = queue.insert(live(-78, 1));

		assert_eq!(h_high.current(), 0);
		assert_eq!(h_mid.current(), 1);
		assert_eq!(h_low.current(), 2);

		// Demote h_high below the others.
		h_high.steer(-118, Order::Descending);

		assert_eq!(h_mid.current(), 0);
		assert_eq!(h_low.current(), 1);
		assert_eq!(h_high.current(), 2);
	}

	#[test]
	fn test_steer_track_swaps_demoted_vec_item_with_overflow() {
		let queue = PriorityQueue::default();

		// Fill vec with 255 items at track=-28, groups 1..=255.
		// f1 (group=1) is the vec tail (lowest priority of the fillers).
		let mut fillers: Vec<_> = (1..=255u64).map(|g| queue.insert(live(-28, g))).collect();

		// Insert a higher-track item; this kicks f1 out of vec into overflow.
		let mut top = queue.insert(live(72, 0));
		assert_eq!(top.current(), 0);
		assert_eq!(fillers[0].current(), u8::MAX, "f1 was kicked into overflow");

		// Lower top's track below every filler. Without the swap, top would land
		// in vec at the tail while f1 stays in overflow despite having higher
		// priority, breaking the "every overflow item < every vec item" invariant.
		top.steer(-128, Order::Descending);

		assert!(fillers[0].current() < u8::MAX, "f1 should be promoted back into vec");
		assert_eq!(top.current(), u8::MAX, "demoted top should land in overflow");
	}

	#[test]
	fn test_steer_track_lowered_within_vec_no_overflow_disruption() {
		let queue = PriorityQueue::default();

		// Three items, all in vec; no overflow involvement.
		let mut a = queue.insert(live(72, 0));
		let mut b = queue.insert(live(-28, 0));
		let mut c = queue.insert(live(-78, 0));
		assert_eq!(a.current(), 0);
		assert_eq!(b.current(), 1);
		assert_eq!(c.current(), 2);

		// Lowering A's priority below B but above C should leave A at index 1.
		a.steer(-53, Order::Descending);
		assert_eq!(b.current(), 0);
		assert_eq!(a.current(), 1);
		assert_eq!(c.current(), 2);
	}

	#[test]
	fn test_remove_promotes_highest_priority_overflow_item() {
		let queue = PriorityQueue::default();

		// Fill vec to capacity with track=72.
		let fillers: Vec<_> = (100..355u64).map(|g| queue.insert(live(72, g))).collect();

		// Three overflow items with distinct priorities (same track, different groups).
		let mut low = queue.insert(live(-28, 1));
		let mut mid = queue.insert(live(-28, 2));
		let mut high = queue.insert(live(-28, 3));
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

	#[kio::test]
	async fn test_steer_track_notifies_swapped_overflow_item() {
		let queue = PriorityQueue::default();

		// Fill vec, then insert top, kicking f1 (filler at group=1) into overflow.
		let mut fillers: Vec<_> = (1..=255u64).map(|g| queue.insert(live(-28, g))).collect();
		let mut top = queue.insert(live(72, 0));
		assert_eq!(top.current(), 0);

		// Take ownership of f1 so we can await its promotion notification.
		let mut f1 = fillers.remove(0);
		assert_eq!(f1.current(), u8::MAX);

		let task = kio::clock::sim::spawn(async move { f1.next().await });
		kio::clock::sim::yield_now().await;

		// Demoting top below every filler swaps it with f1 in overflow.
		top.steer(-128, Order::Descending);

		let promoted = task.await.unwrap();
		assert!(promoted < u8::MAX, "f1 should be notified of promotion");
	}
}
