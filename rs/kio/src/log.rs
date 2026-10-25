//! An append-only log whose readers take committed entries by index without a lock.
//!
//! A [`Writer`] appends entries and evicts them from the front. Any number of
//! [`Reader`]s read them: one whose index is below the committed end reads the slot
//! directly, so readers never contend with each other or hold up the writer.
//!
//! The log does not park. Pair it with a channel ([`crate::Producer`]) whose state says
//! whether more entries may come, and append while holding that channel's write guard.
//! A reader that finds no entry re-checks under the channel's lock before parking on
//! it, so it either sees the entry or is woken by the guard that appended it.
//!
//! Entries live in segments whose slots never move, doubling from one slot to 64, so a
//! one-entry log holds one slot. Eviction advances the base and frees each segment
//! wholly below it. A reader keeps the segment it last read alive, and segments link to
//! the next by `Weak`, so a reader that stops reading pins only its own segment.

use std::{
	fmt,
	mem::MaybeUninit,
	panic::{RefUnwindSafe, UnwindSafe},
	sync::atomic::Ordering,
	sync::{Arc as StdArc, OnceLock, PoisonError, Weak},
};

use smallvec::SmallVec;

use crate::sync::{Arc, AtomicU64, AtomicUsize, Mutex, UnsafeCell};

/// Slots in the first segment: one, since many logs never hold more.
const FIRST_SEGMENT: usize = 1;

/// Slots in the largest segment: the most entries a reader that stops reading pins.
const MAX_SEGMENT: usize = 64;

/// The entry was evicted before the reader got to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Evicted;

impl fmt::Display for Evicted {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "log entry evicted")
	}
}

impl std::error::Error for Evicted {}

/// One entry's storage, written once.
type Slot<T> = UnsafeCell<MaybeUninit<T>>;

/// A run of slots from `start`, written once each, in order.
///
/// Built with a fixed array of slots and used through a slice of them, so the header and
/// slots share one allocation.
struct Segment<T, S: ?Sized + AsRef<[Slot<T>]> = [Slot<T>]> {
	start: u64,
	/// Slots written so far, from the first. Only the writer stores it, after the slot.
	filled: AtomicUsize,
	/// The segment after this one, set before anything is written into it.
	next: OnceLock<Weak<Segment<T>>>,
	slots: S,
}

// SAFETY: a slot is written once, by the writer, before `filled` and the log's end
// publish it; afterwards it is only read, from any thread, and dropped with the segment.
unsafe impl<T: Send + Sync, S: ?Sized + AsRef<[Slot<T>]>> Send for Segment<T, S> {}
// SAFETY: as above.
unsafe impl<T: Send + Sync, S: ?Sized + AsRef<[Slot<T>]>> Sync for Segment<T, S> {}

// A panic can't leave a slot half-written: `push` writes it whole, then publishes it.
impl<T: RefUnwindSafe, S: ?Sized + AsRef<[Slot<T>]>> RefUnwindSafe for Segment<T, S> {}
impl<T: RefUnwindSafe, S: ?Sized + AsRef<[Slot<T>]>> UnwindSafe for Segment<T, S> {}

impl<T> Segment<T> {
	/// An empty segment of `capacity` slots, a power of two up to [`MAX_SEGMENT`].
	fn new(start: u64, capacity: usize) -> StdArc<Self> {
		match capacity {
			1 => Self::sized::<1>(start),
			2 => Self::sized::<2>(start),
			4 => Self::sized::<4>(start),
			8 => Self::sized::<8>(start),
			16 => Self::sized::<16>(start),
			32 => Self::sized::<32>(start),
			64 => Self::sized::<64>(start),
			_ => unreachable!("segments double from {FIRST_SEGMENT} to {MAX_SEGMENT} slots"),
		}
	}

	fn sized<const N: usize>(start: u64) -> StdArc<Self> {
		StdArc::new(Segment::<T, [Slot<T>; N]> {
			start,
			filled: AtomicUsize::new(0),
			next: OnceLock::new(),
			slots: std::array::from_fn(|_| UnsafeCell::new(MaybeUninit::uninit())),
		})
	}

	/// Heap a segment of `capacity` slots occupies: its `Arc` counts, header, and slots.
	const fn heap(capacity: usize) -> usize {
		match capacity {
			1 => crate::arc_heap::<Segment<T, [Slot<T>; 1]>>(),
			2 => crate::arc_heap::<Segment<T, [Slot<T>; 2]>>(),
			4 => crate::arc_heap::<Segment<T, [Slot<T>; 4]>>(),
			8 => crate::arc_heap::<Segment<T, [Slot<T>; 8]>>(),
			16 => crate::arc_heap::<Segment<T, [Slot<T>; 16]>>(),
			32 => crate::arc_heap::<Segment<T, [Slot<T>; 32]>>(),
			64 => crate::arc_heap::<Segment<T, [Slot<T>; 64]>>(),
			_ => panic!("not a segment capacity"),
		}
	}

	/// One past the last slot written.
	fn end(&self) -> u64 {
		self.start + self.filled.load(Ordering::Relaxed) as u64
	}

	/// The entry at `index`.
	///
	/// # Safety
	///
	/// `index` is in `start..end()`, with the write that filled it happening before this.
	unsafe fn get(&self, index: u64) -> &T {
		let slot = self.slots[(index - self.start) as usize].with(|slot| slot);
		// SAFETY: the caller guarantees the slot was written, and it is never written again.
		unsafe { (*slot).assume_init_ref() }
	}

	/// Follow links from `segment` to the one whose written slots hold `index`, if it is
	/// still reachable: `None` once `index` or a segment on the way was evicted.
	fn walk(mut segment: StdArc<Self>, index: u64) -> Option<StdArc<Self>> {
		loop {
			if index < segment.start {
				return None;
			}
			// Relaxed: a caller only asks for an index below an end it loaded with
			// Acquire, which orders every `filled` store for slots below that end first.
			let filled = segment.filled.load(Ordering::Relaxed);
			if index < segment.start + filled as u64 {
				return Some(segment);
			}
			// A full segment links its successor before anything is written into it. One
			// the writer evicted early links nothing.
			if filled < segment.slots.len() {
				return None;
			}
			segment = segment.next.get()?.upgrade()?;
		}
	}
}

impl<T, S: ?Sized + AsRef<[Slot<T>]>> Drop for Segment<T, S> {
	fn drop(&mut self) {
		// Acquire pairs with the writer's Release, ordering this after every slot write.
		let filled = self.filled.load(Ordering::Acquire);
		for slot in &self.slots.as_ref()[..filled] {
			// SAFETY: the first `filled` slots were written, and no reader is left.
			slot.with_mut(|slot| unsafe { (*slot).assume_init_drop() });
		}
	}
}

/// The state a writer and its readers share.
struct Shared<T> {
	/// One past the last committed entry, Release-stored after its slot is written. Only
	/// the writer stores it.
	end: AtomicU64,
	/// Entries below this are evicted. Only the writer stores it.
	base: AtomicU64,
	/// The oldest live segment, where a reader that can't reach its index by link starts.
	head: Mutex<Option<StdArc<Segment<T>>>>,
}

/// Appends entries to a log and evicts them from its front.
///
/// Not `Clone`: appends take `&mut self`, so one writer orders them.
pub struct Writer<T> {
	shared: Arc<Shared<T>>,
	/// The live segments, oldest first, holding the only strong references the log keeps.
	/// The last is the one the next entry goes into, until it fills. Inline while there is
	/// one, which is all most logs hold.
	segments: SmallVec<[StdArc<Segment<T>>; 1]>,
	/// Bytes of the shared state and the live segments, excluding `segments` itself.
	heap: usize,
}

impl<T> Writer<T> {
	/// Bytes the shared state occupies.
	const SHARED: usize = crate::arc_heap::<Shared<T>>();

	/// Bytes a log occupies once its first entry is appended: the shared state and the
	/// first segment.
	pub const HEAP: usize = Self::SHARED + Segment::<T>::heap(FIRST_SEGMENT);

	/// An empty log whose first entry gets index `start`.
	pub fn new(start: u64) -> Self {
		Self {
			shared: Arc::new(Shared {
				end: AtomicU64::new(start),
				base: AtomicU64::new(start),
				head: Mutex::new(None),
			}),
			segments: SmallVec::new(),
			heap: Self::SHARED,
		}
	}

	/// A reader of this log.
	pub fn reader(&self) -> Reader<T> {
		Reader {
			shared: self.shared.clone(),
			segment: None,
		}
	}

	/// The first entry not evicted.
	pub fn base(&self) -> u64 {
		// Relaxed: only this writer stores it.
		self.shared.base.load(Ordering::Relaxed)
	}

	/// One past the last committed entry: the index the next append gets.
	pub fn end(&self) -> u64 {
		// Relaxed: only this writer stores it.
		self.shared.end.load(Ordering::Relaxed)
	}

	/// Bytes this log occupies: its shared state and the segments it holds, which grow in
	/// steps as entries arrive. Excludes an evicted segment a reader still holds.
	pub fn heap(&self) -> usize {
		let list = match self.segments.spilled() {
			true => self.segments.capacity() * size_of::<StdArc<Segment<T>>>(),
			false => 0,
		};
		self.heap + list
	}

	/// Allocate the slot the next entry goes into, if there is none, without publishing
	/// anything.
	pub fn reserve(&mut self) {
		let end = self.end();
		let capacity = match self.segments.last() {
			Some(tail) if end < tail.start + tail.slots.len() as u64 => return,
			Some(full) => (full.slots.len() * 2).min(MAX_SEGMENT),
			None => FIRST_SEGMENT,
		};

		let segment = Segment::new(end, capacity);
		match self.segments.last() {
			// Linked before any entry lands in it, so a reader that passes `full` finds it.
			Some(full) => {
				let linked = full.next.set(StdArc::downgrade(&segment));
				debug_assert!(linked.is_ok(), "a segment has one successor");
			}
			// Published before any entry in it, so a reader that sees one can locate it.
			None => *self.head() = Some(segment.clone()),
		}
		self.heap += Segment::<T>::heap(capacity);
		self.segments.push(segment);
	}

	/// Append `value` at [`Self::end`], visible to every reader once this returns.
	pub fn push(&mut self, value: T) {
		self.reserve();
		let end = self.end();
		let tail = self.segments.last().expect("reserved");
		let at = (end - tail.start) as usize;
		// SAFETY: slot `at` is past `filled`, so no reader reads it until the stores below
		// publish it, and nothing wrote it before.
		tail.slots[at].with_mut(|slot| unsafe { (*slot).write(value) });
		// Release so a drop on another thread sees every slot it counts.
		tail.filled.store(at + 1, Ordering::Release);
		self.shared.end.store(end + 1, Ordering::Release);
	}

	/// Evict every entry below `index`, freeing each segment wholly below it once no
	/// reader holds it. An `index` past the end skips to it: the next entry gets `index`.
	///
	/// The last segment goes only with every entry, and its unused slots with it: the
	/// next entry starts a segment of its own.
	pub fn evict(&mut self, index: u64) {
		if index <= self.base() {
			return;
		}
		self.shared.base.store(index, Ordering::Release);
		if index > self.end() {
			self.shared.end.store(index, Ordering::Release);
		}

		let evicted = self.segments.partition_point(|segment| segment.end() <= index);
		if evicted == 0 {
			return;
		}
		for segment in self.segments.drain(..evicted) {
			self.heap -= Segment::<T>::heap(segment.slots.len());
		}
		if self.segments.is_empty() {
			// Free a spilled list too, so an emptied log costs only its shared state.
			self.segments = SmallVec::new();
		}
		*self.head() = self.segments.first().cloned();
	}

	/// The committed entry at `index`, unless it was evicted.
	pub fn get(&self, index: u64) -> Option<&T> {
		if index < self.base() || index >= self.end() {
			return None;
		}
		let at = self.segments.partition_point(|segment| segment.end() <= index);
		let segment = self.segments.get(at).filter(|segment| segment.start <= index)?;
		// SAFETY: `index` is in the segment's written range, and this writer wrote it.
		Some(unsafe { segment.get(index) })
	}

	fn head(&self) -> crate::sync::MutexGuard<'_, Option<StdArc<Segment<T>>>> {
		self.shared.head.lock().unwrap_or_else(PoisonError::into_inner)
	}
}

impl<T> Drop for Writer<T> {
	/// Evict every entry, so a reader that outlives the writer pins at most its own
	/// segment.
	fn drop(&mut self) {
		self.shared.base.store(self.end(), Ordering::Release);
		*self.head() = None;
	}
}

impl<T> Default for Writer<T> {
	fn default() -> Self {
		Self::new(0)
	}
}

impl<T> fmt::Debug for Writer<T> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("Writer")
			.field("base", &self.base())
			.field("end", &self.end())
			.finish_non_exhaustive()
	}
}

/// Reads a log's committed entries by index, without a lock.
pub struct Reader<T> {
	shared: Arc<Shared<T>>,
	/// The segment of the last entry read, so the next read finds its slot directly.
	segment: Option<StdArc<Segment<T>>>,
}

impl<T> Reader<T> {
	/// The committed entry at `index`, or `None` when nothing is committed there yet.
	///
	/// Reads at increasing indices stay in the segment they last read or follow its link
	/// to the next. Anything else, such as a reader's first read, takes the lock on the
	/// log's oldest segment once and follows links from there.
	pub fn get(&mut self, index: u64) -> Result<Option<&T>, Evicted> {
		let base = self.shared.base.load(Ordering::Acquire);
		// The common read: the segment last read already holds `index`. Acquire pairs with
		// the writer's Release of `filled`, which follows the slot's write.
		let held = self.segment.as_ref().is_some_and(|segment| {
			segment.start <= index
				&& base <= index
				&& index < segment.start + segment.filled.load(Ordering::Acquire) as u64
		});
		if held {
			let segment = self.segment.as_deref().expect("checked");
			// SAFETY: `filled` covers `index`, so its slot was written before that load.
			return Ok(Some(unsafe { segment.get(index) }));
		}
		// Stop pinning a segment that holds nothing readable any more, even from a reader
		// parked at the end, where a release of every entry leaves it.
		if self.segment.as_ref().is_some_and(|segment| segment.end() <= base) {
			self.segment = None;
		}
		if index < base {
			return Err(Evicted);
		}
		// Acquire pairs with the writer's Release: every slot below `end` is written.
		if index >= self.shared.end.load(Ordering::Acquire) {
			return Ok(None);
		}
		let segment = self.seek(index)?;
		// SAFETY: `seek` found a segment whose filled slots cover `index`, so it was
		// written. Every end the writer stores, including an eviction's skip, comes after
		// each slot below it that is ever written, so the end loaded above orders that
		// write before this read.
		Ok(Some(unsafe { segment.get(index) }))
	}

	/// The segment holding committed entry `index`, kept as the one last read.
	fn seek(&mut self, index: u64) -> Result<&Segment<T>, Evicted> {
		let held = self.segment.take().filter(|segment| segment.start <= index);
		let found = match held.and_then(|held| Segment::walk(held, index)) {
			Some(found) => found,
			None => {
				let head = self.shared.head.lock().unwrap_or_else(PoisonError::into_inner).clone();
				// Evicted since this read checked the base.
				head.and_then(|head| Segment::walk(head, index)).ok_or(Evicted)?
			}
		};
		Ok(self.segment.insert(found))
	}
}

impl<T> Clone for Reader<T> {
	fn clone(&self) -> Self {
		Self {
			shared: self.shared.clone(),
			segment: self.segment.clone(),
		}
	}
}

impl<T> fmt::Debug for Reader<T> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("Reader")
			.field("segment", &self.segment.as_ref().map(|segment| segment.start))
			.finish_non_exhaustive()
	}
}

#[cfg(all(test, not(loom)))]
mod test {
	use super::*;

	/// The `[start, end)` of each live segment, oldest first.
	fn layout<T>(writer: &Writer<T>) -> Vec<(u64, u64)> {
		writer
			.segments
			.iter()
			.map(|segment| (segment.start, segment.start + segment.slots.len() as u64))
			.collect()
	}

	/// Every committed entry reads back at its index, across segment boundaries, from a
	/// reader that started before any of them existed or one that jumps.
	#[test]
	fn reads_every_entry_across_segments() {
		let mut writer = Writer::new(0);
		let mut reader = writer.reader();
		assert_eq!(reader.get(0), Ok(None));
		for i in 0..300u64 {
			writer.push(i);
		}
		for i in 0..300u64 {
			assert_eq!(reader.get(i), Ok(Some(&i)));
			assert_eq!(writer.get(i), Some(&i));
		}
		assert_eq!(reader.get(300), Ok(None));
		assert_eq!(writer.get(300), None);
		// A fresh reader jumping far walks the links from the oldest segment, and one
		// reading backwards starts over from there.
		let mut fresh = writer.reader();
		assert_eq!(fresh.get(299), Ok(Some(&299)));
		assert_eq!(fresh.get(5), Ok(Some(&5)));
	}

	/// Segments double from one slot to the largest, and `heap` counts each one once it
	/// is allocated.
	#[test]
	fn segments_double_up_to_the_largest() {
		let mut writer = Writer::<u64>::new(0);
		assert_eq!(writer.heap(), Writer::<u64>::SHARED);
		writer.reserve();
		assert_eq!(writer.heap(), Writer::<u64>::HEAP);
		// Reserving again with the slot still free allocates nothing.
		writer.reserve();
		assert_eq!(writer.heap(), Writer::<u64>::HEAP);

		for i in 0..200u64 {
			writer.push(i);
		}
		assert_eq!(
			layout(&writer),
			[
				(0, 1),
				(1, 3),
				(3, 7),
				(7, 15),
				(15, 31),
				(31, 63),
				(63, 127),
				(127, 191),
				(191, 255)
			]
		);
		let slots: usize = [1, 2, 4, 8, 16, 32, 64, 64, 64]
			.into_iter()
			.map(Segment::<u64>::heap)
			.sum();
		let list = writer.segments.capacity() * size_of::<StdArc<Segment<u64>>>();
		assert!(writer.segments.spilled());
		assert_eq!(writer.heap(), Writer::<u64>::SHARED + slots + list);
	}

	/// A log can start past zero; nothing below its start is readable.
	#[test]
	fn starts_past_zero() {
		let mut writer = Writer::new(10);
		let mut reader = writer.reader();
		writer.push("a");
		assert_eq!(reader.get(9), Err(Evicted));
		assert_eq!(reader.get(10), Ok(Some(&"a")));
		assert_eq!(writer.end(), 11);
	}

	/// Evicting frees whole segments below the base, and a reader below it sees
	/// [`Evicted`] rather than a stale entry, even one in a segment it still holds.
	#[test]
	fn eviction_frees_whole_segments() {
		let mut writer = Writer::new(0);
		let mut reader = writer.reader();
		for i in 0..10u64 {
			writer.push(i);
		}
		assert_eq!(layout(&writer), [(0, 1), (1, 3), (3, 7), (7, 15)]);
		assert_eq!(reader.get(1), Ok(Some(&1)));
		let held = writer.segments[1].clone();

		writer.evict(2);
		assert_eq!(writer.base(), 2);
		assert_eq!(layout(&writer), [(1, 3), (3, 7), (7, 15)], "entry 2 keeps its segment");
		assert_eq!(reader.get(1), Err(Evicted));
		assert_eq!(writer.get(1), None);
		assert_eq!(reader.get(2), Ok(Some(&2)));

		writer.evict(3);
		assert_eq!(layout(&writer), [(3, 7), (7, 15)]);
		let list = writer.segments.capacity() * size_of::<StdArc<Segment<u64>>>();
		let heap = Writer::<u64>::SHARED + Segment::<u64>::heap(4) + Segment::<u64>::heap(8) + list;
		assert_eq!(writer.heap(), heap);
		assert_eq!(
			writer.head().as_ref().map(|segment| segment.start),
			Some(3),
			"readers start past it too"
		);
		// The reader still holds the evicted segment; it moves on by link and lets it go.
		assert_eq!(StdArc::strong_count(&held), 2);
		assert_eq!(reader.get(3), Ok(Some(&3)));
		assert_eq!(StdArc::strong_count(&held), 1, "nothing but the test pins it");
	}

	/// A reader that observes its index was evicted drops the segment it held.
	#[test]
	fn evicted_reader_stops_pinning() {
		let mut writer = Writer::new(0);
		let mut reader = writer.reader();
		for i in 0..3u64 {
			writer.push(i);
		}
		assert_eq!(reader.get(1), Ok(Some(&1)));
		let held = StdArc::downgrade(&writer.segments[1]);
		writer.evict(3);
		assert!(held.upgrade().is_some(), "the reader pins its segment");
		assert_eq!(reader.get(2), Err(Evicted));
		assert!(held.upgrade().is_none(), "freed once the reader saw the eviction");
	}

	/// A reader parked at the end lets its segment go once everything below it is
	/// evicted, without reading anything evicted.
	#[test]
	fn parked_reader_stops_pinning() {
		let mut writer = Writer::new(0);
		let mut reader = writer.reader();
		writer.push(0u64);
		assert_eq!(reader.get(0), Ok(Some(&0)));
		assert_eq!(reader.get(1), Ok(None));
		let held = StdArc::downgrade(&writer.segments[0]);
		writer.evict(1);
		assert!(held.upgrade().is_some(), "the reader pins its segment");
		assert_eq!(reader.get(1), Ok(None));
		assert!(held.upgrade().is_none(), "freed once the reader saw the eviction");
	}

	/// A reader that outlives the writer sees every entry evicted and pins nothing it
	/// did not already hold.
	#[test]
	fn reader_outlives_writer() {
		let entry = StdArc::new(());
		let mut writer = Writer::new(0);
		let mut reader = writer.reader();
		let mut idle = writer.reader();
		for _ in 0..3 {
			writer.push(entry.clone());
		}
		assert!(reader.get(0).unwrap().is_some());
		drop(writer);
		assert_eq!(StdArc::strong_count(&entry), 2, "only the reader's segment survives");
		assert_eq!(idle.get(1).err(), Some(Evicted));
		assert!(reader.get(3).unwrap().is_none());
		assert_eq!(StdArc::strong_count(&entry), 1);
	}

	/// Evicting everything drops the last segment with its unused slots, and the next
	/// entry starts a fresh segment that a reader holding the old one does not misread.
	#[test]
	fn evicting_the_tail_starts_a_new_segment() {
		let mut writer = Writer::new(0);
		let mut reader = writer.reader();
		for i in 0..2u64 {
			writer.push(i);
		}
		// The second segment holds entry 1 with room for entry 2.
		assert_eq!(layout(&writer), [(0, 1), (1, 3)]);
		assert_eq!(reader.get(1), Ok(Some(&1)));

		writer.evict(2);
		assert_eq!(writer.heap(), Writer::<u64>::SHARED, "the list goes too");
		assert!(writer.segments.is_empty());

		writer.push(2);
		writer.push(3);
		assert_eq!(layout(&writer), [(2, 3), (3, 5)]);
		let mut fresh = writer.reader();
		for reader in [&mut reader, &mut fresh] {
			assert_eq!(reader.get(1), Err(Evicted));
			assert_eq!(reader.get(2), Ok(Some(&2)));
			assert_eq!(reader.get(3), Ok(Some(&3)));
		}
	}

	/// Evicting past the end skips those indices: the next entry lands at the new base.
	#[test]
	fn evicting_past_the_end_skips() {
		let mut writer = Writer::new(0);
		let mut reader = writer.reader();
		writer.reserve();
		writer.evict(100);
		assert_eq!((writer.base(), writer.end()), (100, 100));
		assert_eq!(
			writer.heap(),
			Writer::<&str>::SHARED,
			"the reserved segment went with the skip"
		);
		writer.push("x");
		assert_eq!(reader.get(99), Err(Evicted));
		assert_eq!(reader.get(100), Ok(Some(&"x")));
	}

	/// A clone reads independently from where the original's segment was.
	#[test]
	fn clones_read_independently() {
		let mut writer = Writer::new(0);
		let mut reader = writer.reader();
		for i in 0..20u64 {
			writer.push(i);
		}
		assert_eq!(reader.get(10), Ok(Some(&10)));
		let mut clone = reader.clone();
		assert_eq!(clone.get(11), Ok(Some(&11)));
		assert_eq!(reader.get(19), Ok(Some(&19)));
		assert_eq!(clone.get(12), Ok(Some(&12)));
	}

	/// Dropping the log drops every entry exactly once, including those a reader pins
	/// past eviction.
	#[test]
	fn drops_every_entry_once() {
		let entry = StdArc::new(());
		let mut writer = Writer::new(0);
		let mut reader = writer.reader();
		for _ in 0..20 {
			writer.push(entry.clone());
		}
		assert_eq!(layout(&writer), [(0, 1), (1, 3), (3, 7), (7, 15), (15, 31)]);
		assert!(reader.get(1).unwrap().is_some());
		// The first three segments go, and the reader pins the second's two entries.
		writer.evict(10);
		assert_eq!(StdArc::strong_count(&entry), 1 + 20 - 5);
		drop(reader);
		assert_eq!(StdArc::strong_count(&entry), 1 + 20 - 7);
		drop(writer);
		assert_eq!(StdArc::strong_count(&entry), 1);
	}

	/// Readers on other threads see every entry the writer published, in order.
	#[test]
	fn concurrent_readers_see_every_entry() {
		let mut writer = Writer::new(0);
		let readers: Vec<_> = (0..4)
			.map(|_| {
				let mut reader = writer.reader();
				std::thread::spawn(move || {
					let mut index = 0;
					while index < 10_000 {
						match reader.get(index) {
							Ok(Some(value)) => {
								assert_eq!(*value, index);
								index += 1;
							}
							Ok(None) => std::thread::yield_now(),
							Err(Evicted) => panic!("nothing is evicted"),
						}
					}
				})
			})
			.collect();
		for i in 0..10_000u64 {
			writer.push(i);
		}
		for reader in readers {
			reader.join().unwrap();
		}
	}
}
