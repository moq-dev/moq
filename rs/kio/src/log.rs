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
//! one-entry log holds one slot. Eviction advances the base and retires each segment
//! wholly below it. A reader holds nothing between reads: it reads through a [`Read`],
//! which pins the log only while it lives. A retired segment is freed once every pin
//! taken before its retirement is gone (epoch-based reclamation), so eviction frees
//! memory whatever readers do, and a reader that stopped reading finds its entries
//! [`Evicted`] when it comes back.

use std::{
	alloc::{self, Layout},
	fmt,
	mem::{MaybeUninit, offset_of},
	panic::{RefUnwindSafe, UnwindSafe},
	ptr::{self, NonNull},
	sync::atomic::Ordering,
};

use crossbeam_epoch as epoch;
use smallvec::SmallVec;

use crate::sync::{Arc, AtomicPtr, AtomicU64, AtomicUsize, UnsafeCell, fence};

/// Slots in the first segment: one, since many logs never hold more.
const FIRST_SEGMENT: usize = 1;

/// Slots in the largest segment, so a long log allocates in steps of this many entries.
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

/// A run of `capacity` slots from `start`, written once each, in order. The slots follow
/// the header in the same allocation, so a segment is only ever handled by its pointer.
#[repr(C)]
struct Segment<T> {
	start: u64,
	capacity: usize,
	/// Slots written so far, from the first. Only the writer stores it, after the slot.
	filled: AtomicUsize,
	/// The segment after this one, set before anything is written into it.
	next: AtomicPtr<Segment<T>>,
	/// Read by every reader access and written by the free, so loom flags a reader that
	/// reaches a segment the collector may have freed.
	#[cfg(loom)]
	alive: UnsafeCell<()>,
	slots: [Slot<T>; 0],
}

// A panic can't leave a slot half-written: `push` writes it whole, then publishes it.
impl<T: RefUnwindSafe> RefUnwindSafe for Segment<T> {}
impl<T: RefUnwindSafe> UnwindSafe for Segment<T> {}

impl<T> Segment<T> {
	/// Heap a segment of `capacity` slots occupies: its header and slots.
	const fn heap(capacity: usize) -> usize {
		(offset_of!(Self, slots) + capacity * size_of::<Slot<T>>()).next_multiple_of(align_of::<Self>())
	}

	fn layout(capacity: usize) -> Layout {
		Layout::from_size_align(Self::heap(capacity), align_of::<Self>()).expect("a segment fits in memory")
	}

	/// An empty segment of `capacity` slots from `start`.
	fn new(start: u64, capacity: usize) -> NonNull<Self> {
		let layout = Self::layout(capacity);
		// SAFETY: the header gives the layout a non-zero size.
		let Some(segment) = NonNull::new(unsafe { alloc::alloc(layout) }.cast::<Self>()) else {
			alloc::handle_alloc_error(layout);
		};
		// SAFETY: freshly allocated for the header and `capacity` slots after it.
		unsafe {
			segment.write(Self {
				start,
				capacity,
				filled: AtomicUsize::new(0),
				next: AtomicPtr::new(ptr::null_mut()),
				#[cfg(loom)]
				alive: UnsafeCell::new(()),
				slots: [],
			});
			for at in 0..capacity {
				Self::slot(segment, at)
					.cast_mut()
					.write(UnsafeCell::new(MaybeUninit::uninit()));
			}
		}
		segment
	}

	/// The slot at `at`, derived from the allocation's pointer so it may reach past the
	/// header.
	///
	/// # Safety
	///
	/// `segment` is live and `at` is below its capacity.
	unsafe fn slot(segment: NonNull<Self>, at: usize) -> *const Slot<T> {
		// SAFETY: in bounds of the allocation, per the caller.
		unsafe { ptr::addr_of!((*segment.as_ptr()).slots).cast::<Slot<T>>().add(at) }
	}

	/// The header of a segment a reader reached.
	///
	/// # Safety
	///
	/// `segment` stays live for `'a`.
	unsafe fn header<'a>(segment: NonNull<Self>) -> &'a Self {
		// SAFETY: live, per the caller.
		let header = unsafe { segment.as_ref() };
		#[cfg(loom)]
		header.alive.with(|_| ());
		header
	}

	/// The written entry at `index`.
	///
	/// # Safety
	///
	/// `segment` stays live for `'a`, and the write that filled `index` happened before
	/// this.
	unsafe fn get<'a>(segment: NonNull<Self>, index: u64) -> &'a T {
		// SAFETY: per the caller, the slot is in bounds, written, and never written again.
		unsafe {
			let at = (index - Self::header(segment).start) as usize;
			let value = (*Self::slot(segment, at)).with(|slot| slot);
			(*value).assume_init_ref()
		}
	}

	/// Drop the written entries and free the segment.
	///
	/// # Safety
	///
	/// No reader can reach `segment` any more, and this is its only free.
	unsafe fn free(segment: NonNull<Self>) {
		// SAFETY: the segment is live until the dealloc below, and nothing else uses it.
		unsafe {
			let header = segment.as_ref();
			#[cfg(loom)]
			header.alive.with_mut(|_| ());
			let (capacity, filled) = (header.capacity, header.filled.load(Ordering::Acquire));
			for at in 0..capacity {
				let slot = Self::slot(segment, at).cast_mut();
				// A write access even to an empty slot, so loom flags any read racing it.
				(*slot).with_mut(|value| {
					if at < filled {
						(*value).assume_init_drop();
					}
				});
				ptr::drop_in_place(slot);
			}
			ptr::drop_in_place(segment.as_ptr());
			// Loom's cells are dropped above, so a racing access fails the model; the
			// memory stays mapped so that failure is a report rather than a crash.
			#[cfg(not(loom))]
			alloc::dealloc(segment.as_ptr().cast(), Self::layout(capacity));
		}
	}
}

/// A retired segment on its way to the epoch collector.
struct Retired<T>(NonNull<Segment<T>>);

// SAFETY: whichever thread frees the segment drops its entries, which `T: Send` allows.
unsafe impl<T: Send> Send for Retired<T> {}

/// The state a writer and its readers share.
struct Shared<T> {
	/// One past the last committed entry, Release-stored after its slot is written. Only
	/// the writer stores it.
	end: AtomicU64,
	/// Entries below this are evicted. Only the writer stores it, before retiring the
	/// segments it passes.
	base: AtomicU64,
	/// The oldest live segment, where a reader without a usable segment starts.
	head: AtomicPtr<Segment<T>>,
}

/// Appends entries to a log and evicts them from its front.
///
/// Not `Clone`: appends take `&mut self`, so one writer orders them.
pub struct Writer<T: Send + Sync + 'static> {
	shared: Arc<Shared<T>>,
	/// The live segments, oldest first. The last is the one the next entry goes into,
	/// until it fills. Inline while there is one, which is all most logs hold.
	segments: SmallVec<[NonNull<Segment<T>>; 1]>,
	/// Bytes of the shared state and the live segments, excluding `segments` itself.
	heap: usize,
}

// SAFETY: the writer owns its segments, which hold `T`s that readers on other threads
// borrow and that the collector may drop on another thread.
unsafe impl<T: Send + Sync + 'static> Send for Writer<T> {}
// SAFETY: `&Writer` only reads committed entries, which only `&mut Writer` retires.
unsafe impl<T: Send + Sync + 'static> Sync for Writer<T> {}

impl<T: Send + Sync + 'static> Writer<T> {
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
				head: AtomicPtr::new(ptr::null_mut()),
			}),
			segments: SmallVec::new(),
			heap: Self::SHARED,
		}
	}

	/// A reader of this log.
	pub fn reader(&self) -> Reader<T> {
		Reader {
			shared: self.shared.clone(),
			hint: None,
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
	/// steps as entries arrive.
	pub fn heap(&self) -> usize {
		let list = match self.segments.spilled() {
			true => self.segments.capacity() * size_of::<NonNull<Segment<T>>>(),
			false => 0,
		};
		self.heap + list
	}

	/// Allocate the slot the next entry goes into, if there is none, without publishing
	/// anything.
	pub fn reserve(&mut self) {
		let end = self.end();
		let capacity = match self.segments.last() {
			// SAFETY: the writer's segments are live.
			Some(tail) => match unsafe { tail.as_ref() } {
				tail if end < tail.start + tail.capacity as u64 => return,
				full => (full.capacity * 2).min(MAX_SEGMENT),
			},
			None => FIRST_SEGMENT,
		};

		let segment = Segment::new(end, capacity);
		match self.segments.last() {
			// Linked before any entry lands in it, so a reader that passes `full` finds it.
			// SAFETY: the writer's segments are live.
			Some(full) => unsafe { full.as_ref() }.next.store(segment.as_ptr(), Ordering::Release),
			// Published before any entry in it, so a reader that sees one can locate it.
			None => self.shared.head.store(segment.as_ptr(), Ordering::Release),
		}
		self.heap += Segment::<T>::heap(capacity);
		self.segments.push(segment);
	}

	/// Append `value` at [`Self::end`], visible to every reader once this returns.
	pub fn push(&mut self, value: T) {
		self.reserve();
		let end = self.end();
		let tail = *self.segments.last().expect("reserved");
		// SAFETY: the tail is live.
		let header = unsafe { tail.as_ref() };
		let at = (end - header.start) as usize;
		// SAFETY: slot `at` is in bounds and past `filled`, so no reader reads it until the
		// stores below publish it, and nothing wrote it before.
		unsafe { (*Segment::slot(tail, at)).with_mut(|slot| (*slot).write(value)) };
		// Release so a reader, or the free, that sees the count sees the slot.
		header.filled.store(at + 1, Ordering::Release);
		self.shared.end.store(end + 1, Ordering::Release);
	}

	/// Evict every entry below `index`, retiring each segment wholly below it. An
	/// `index` past the end skips to it: the next entry gets `index`.
	///
	/// A retired segment is freed once no read that began before this is still running,
	/// whether or not its readers ever read again. The last segment goes only with every
	/// entry, and its unused slots with it: the next entry starts a segment of its own.
	pub fn evict(&mut self, index: u64) {
		if index <= self.base() {
			return;
		}
		// Stored before the segments are retired: a reader that still sees the old base
		// pinned before their retirement, so they outlive its read.
		self.shared.base.store(index, Ordering::Release);
		if index > self.end() {
			self.shared.end.store(index, Ordering::Release);
		}

		// SAFETY: the writer's segments are live.
		let evicted = self.segments.partition_point(|segment| {
			let segment = unsafe { segment.as_ref() };
			segment.start + segment.filled.load(Ordering::Relaxed) as u64 <= index
		});
		if evicted == 0 {
			return;
		}
		let head = self
			.segments
			.get(evicted)
			.map_or(ptr::null_mut(), |segment| segment.as_ptr());
		self.shared.head.store(head, Ordering::Release);
		let retired: SmallVec<[_; 1]> = self.segments.drain(..evicted).collect();
		if self.segments.is_empty() {
			// Free a spilled list too, so an emptied log costs only its shared state.
			self.segments = SmallVec::new();
		}
		for segment in &retired {
			// SAFETY: still live; retired below.
			self.heap -= Segment::<T>::heap(unsafe { segment.as_ref() }.capacity);
		}
		Self::retire(retired);
	}

	/// Hand segments no reader can reach any more to the epoch collector.
	fn retire(segments: impl IntoIterator<Item = NonNull<Segment<T>>>) {
		let guard = epoch::pin();
		for segment in segments {
			let segment = Retired(segment);
			// SAFETY: unlinked from the head and the base, so only a read pinned before
			// this can still reach it, and the collector waits those out. `T: 'static`, so
			// the entries outlive any delay.
			unsafe {
				guard.defer_unchecked(move || {
					let segment = segment;
					Segment::free(segment.0);
				});
			}
		}
		// Hand the garbage to the collector now rather than when this thread's bag fills.
		guard.flush();
	}

	/// The committed entry at `index`, unless it was evicted.
	pub fn get(&self, index: u64) -> Option<&T> {
		if index < self.base() || index >= self.end() {
			return None;
		}
		// SAFETY: the writer's segments are live.
		let at = self.segments.partition_point(|segment| {
			let segment = unsafe { segment.as_ref() };
			segment.start + segment.filled.load(Ordering::Relaxed) as u64 <= index
		});
		let segment = *self.segments.get(at)?;
		// SAFETY: live while `&self` is, since only `&mut self` retires it, and this
		// writer wrote `index`: it is below the end, at or past the base.
		(unsafe { segment.as_ref() }.start <= index).then(|| unsafe { Segment::get(segment, index) })
	}
}

impl<T: Send + Sync + 'static> Drop for Writer<T> {
	/// Evict every entry, so readers that outlive the writer hold nothing.
	fn drop(&mut self) {
		self.shared.base.store(self.end(), Ordering::Release);
		self.shared.head.store(ptr::null_mut(), Ordering::Release);
		if !self.segments.is_empty() {
			Self::retire(std::mem::take(&mut self.segments));
		}
	}
}

impl<T: Send + Sync + 'static> Default for Writer<T> {
	fn default() -> Self {
		Self::new(0)
	}
}

impl<T: Send + Sync + 'static> fmt::Debug for Writer<T> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("Writer")
			.field("base", &self.base())
			.field("end", &self.end())
			.finish_non_exhaustive()
	}
}

/// Reads a log's committed entries by index, without a lock.
///
/// Holds nothing between reads: [`Self::read`] pins the log for as long as its
/// [`Read`] lives.
pub struct Reader<T> {
	shared: Arc<Shared<T>>,
	/// The segment of the last entry read, and its start. Only a hint: it may have been
	/// freed since, so a read trusts it only once the base shows it wasn't retired.
	hint: Option<(NonNull<Segment<T>>, u64)>,
}

// SAFETY: the hint is dereferenced only under a pin that keeps its segment alive, and
// readers only share `&T`.
unsafe impl<T: Send + Sync> Send for Reader<T> {}
// SAFETY: as above; reads take `&mut self`.
unsafe impl<T: Send + Sync> Sync for Reader<T> {}

impl<T> Reader<T> {
	/// Pin the log for a run of reads. Entries read through the returned handle stay
	/// alive until it drops; nothing does once it has.
	pub fn read(&mut self) -> Read<'_, T> {
		let guard = epoch::pin();
		let base = self.shared.base.load(Ordering::Acquire);
		// A segment is retired only once the base passes its last entry, so one whose
		// start the base hasn't reached was not retired before this pin.
		if self.hint.is_some_and(|(_, start)| start < base) {
			self.hint = None;
		}
		Read {
			reader: self,
			_guard: ReadGuard { _pin: guard },
		}
	}
}

impl<T> Clone for Reader<T> {
	fn clone(&self) -> Self {
		Self {
			shared: self.shared.clone(),
			hint: self.hint,
		}
	}
}

impl<T> fmt::Debug for Reader<T> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("Reader")
			.field("segment", &self.hint.map(|(_, start)| start))
			.finish_non_exhaustive()
	}
}

/// A [`Reader`] pinned for a run of reads, from [`Reader::read`].
///
/// No segment it can reach is freed while it lives, so hold it only for the reads and
/// drop it before parking.
pub struct Read<'a, T> {
	reader: &'a mut Reader<T>,
	_guard: ReadGuard,
}

/// The pin behind a [`Read`], released only after the read's accesses.
///
/// A wrapper rather than a `Drop` on `Read` itself, which would hold the `Reader` borrow
/// until the end of the scope instead of the last use.
struct ReadGuard {
	_pin: epoch::Guard,
}

impl Drop for ReadGuard {
	/// Order the read's accesses before whatever the thread pins next, then unpin.
	///
	/// The collector frees a segment once it sees every thread unpinned or pinned anew,
	/// and the free must happen after the reads it waited out. Seeing the unpin, a
	/// Release store, orders them. Seeing a later pin may not: off x86, crossbeam-epoch
	/// 0.9 pins with a Relaxed store, which carries nothing of the pin before it. This
	/// fence makes every later store to the thread's epoch carry these reads to the
	/// collector's Acquire fence. On x86 it compiles to nothing.
	fn drop(&mut self) {
		fence(Ordering::Release);
	}
}

impl<T> Read<'_, T> {
	/// The committed entry at `index`, or `None` when nothing is committed there yet.
	///
	/// Reads at increasing indices stay in the segment last read or follow its link to
	/// the next. Anything else, such as a reader's first read, starts from the log's
	/// oldest segment.
	pub fn get(&mut self, index: u64) -> Result<Option<&T>, Evicted> {
		let shared = &self.reader.shared;
		if index < shared.base.load(Ordering::Acquire) {
			return Err(Evicted);
		}
		// The common read: the segment last read already holds `index`.
		if let Some((segment, start)) = self.reader.hint
			&& start <= index
			// SAFETY: the hint survived the check in `Reader::read`, so this pin keeps it
			// alive. Acquire pairs with the writer's Release of `filled`.
			&& index < start + unsafe { Segment::header(segment) }.filled.load(Ordering::Acquire) as u64
		{
			// SAFETY: as above, and `filled` covers `index`, so its slot was written.
			return Ok(Some(unsafe { Segment::get(segment, index) }));
		}
		// Acquire pairs with the writer's Release: every slot below `end` is written.
		if index >= shared.end.load(Ordering::Acquire) {
			return Ok(None);
		}

		// The hint's chain can end short of `index` when the writer emptied the log and
		// started a fresh segment after this pin's check, so fall back to the head.
		let hinted = match self.reader.hint {
			Some((segment, start)) if start <= index => Self::walk(segment, index),
			_ => None,
		};
		let segment = match hinted {
			Some(segment) => segment,
			// Loaded under the pin: the writer moves the head before retiring what it
			// passes.
			None => NonNull::new(shared.head.load(Ordering::Acquire))
				.and_then(|head| Self::walk(head, index))
				// Evicted since the base was checked.
				.ok_or(Evicted)?,
		};
		// SAFETY: found under this pin, which keeps it alive, so the hint holds only for
		// as long as `Reader::read` keeps checking it.
		self.reader.hint = Some((segment, unsafe { Segment::header(segment) }.start));
		// SAFETY: `filled` covers `index`, so its slot was written.
		Ok(Some(unsafe { Segment::get(segment, index) }))
	}

	/// Follow links from `segment` to the one whose written slots hold `index`, if any.
	fn walk(mut segment: NonNull<Segment<T>>, index: u64) -> Option<NonNull<Segment<T>>> {
		loop {
			// SAFETY: the hint or the head, or linked from one, all reached under this pin:
			// segments retire oldest first, so none of them was retired before it.
			let header = unsafe { Segment::header(segment) };
			if index < header.start {
				return None;
			}
			if index < header.start + header.filled.load(Ordering::Acquire) as u64 {
				return Some(segment);
			}
			// A segment links its successor before anything is written into it, so a
			// segment without a link is the tail, or one evicted early with everything.
			segment = NonNull::new(header.next.load(Ordering::Acquire))?;
		}
	}
}

#[cfg(all(test, not(loom)))]
mod test {
	use std::sync::{Arc as StdArc, Mutex};

	use super::*;

	// Crossbeam shares its collector across logs. Keep unrelated tests from holding
	// pins or queuing garbage while another test checks reclamation. Nextest isolates
	// tests in processes, but libtest runs them concurrently in one process.
	static COLLECTOR: Mutex<()> = Mutex::new(());

	/// The `[start, end)` of each live segment, oldest first.
	fn layout<T: Send + Sync + 'static>(writer: &Writer<T>) -> Vec<(u64, u64)> {
		writer
			.segments
			.iter()
			.map(|segment| unsafe { segment.as_ref() })
			.map(|segment| (segment.start, segment.start + segment.capacity as u64))
			.collect()
	}

	/// Drive reclamation without unrelated pins or garbage, under `COLLECTOR`.
	fn collect() {
		for _ in 0..8 {
			epoch::pin().flush();
		}
	}

	fn get<T: Clone>(reader: &mut Reader<T>, index: u64) -> Result<Option<T>, Evicted> {
		Ok(reader.read().get(index)?.cloned())
	}

	/// Every committed entry reads back at its index, across segment boundaries, from a
	/// reader that started before any of them existed or one that jumps.
	#[test]
	fn reads_every_entry_across_segments() {
		let _collector = COLLECTOR.lock().unwrap();
		let mut writer = Writer::new(0);
		let mut reader = writer.reader();
		assert_eq!(get(&mut reader, 0), Ok(None));
		for i in 0..300u64 {
			writer.push(i);
		}
		let mut read = reader.read();
		for i in 0..300u64 {
			assert_eq!(read.get(i), Ok(Some(&i)));
			assert_eq!(writer.get(i), Some(&i));
		}
		assert_eq!(read.get(300), Ok(None));
		assert_eq!(writer.get(300), None);
		// A fresh reader jumping far walks the links from the oldest segment, and one
		// reading backwards starts over from there.
		let mut fresh = writer.reader();
		assert_eq!(get(&mut fresh, 299), Ok(Some(299)));
		assert_eq!(get(&mut fresh, 5), Ok(Some(5)));
	}

	/// Segments double from one slot to the largest, and `heap` counts each one once it
	/// is allocated.
	#[test]
	fn segments_double_up_to_the_largest() {
		let _collector = COLLECTOR.lock().unwrap();
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
		let list = writer.segments.capacity() * size_of::<NonNull<Segment<u64>>>();
		assert!(writer.segments.spilled());
		assert_eq!(writer.heap(), Writer::<u64>::SHARED + slots + list);
	}

	/// A segment's heap is its allocation's real size.
	#[test]
	fn heap_matches_layout() {
		let _collector = COLLECTOR.lock().unwrap();
		let header = Layout::new::<Segment<StdArc<()>>>();
		let slots = Layout::array::<Slot<StdArc<()>>>(4).unwrap();
		let layout = header.extend(slots).unwrap().0.pad_to_align();
		assert_eq!(Segment::<StdArc<()>>::layout(4), layout);
	}

	/// A log can start past zero; nothing below its start is readable.
	#[test]
	fn starts_past_zero() {
		let _collector = COLLECTOR.lock().unwrap();
		let mut writer = Writer::new(10);
		let mut reader = writer.reader();
		writer.push("a");
		assert_eq!(get(&mut reader, 9), Err(Evicted));
		assert_eq!(get(&mut reader, 10), Ok(Some("a")));
		assert_eq!(writer.end(), 11);
	}

	/// Evicting retires whole segments below the base, and a reader below it sees
	/// [`Evicted`] rather than a stale entry, even in the segment it last read.
	#[test]
	fn eviction_retires_whole_segments() {
		let _collector = COLLECTOR.lock().unwrap();
		let mut writer = Writer::new(0);
		let mut reader = writer.reader();
		for i in 0..10u64 {
			writer.push(i);
		}
		assert_eq!(layout(&writer), [(0, 1), (1, 3), (3, 7), (7, 15)]);
		assert_eq!(get(&mut reader, 1), Ok(Some(1)));

		writer.evict(2);
		assert_eq!(writer.base(), 2);
		assert_eq!(layout(&writer), [(1, 3), (3, 7), (7, 15)], "entry 2 keeps its segment");
		assert_eq!(get(&mut reader, 1), Err(Evicted));
		assert_eq!(writer.get(1), None);
		assert_eq!(get(&mut reader, 2), Ok(Some(2)));

		writer.evict(3);
		assert_eq!(layout(&writer), [(3, 7), (7, 15)]);
		let list = writer.segments.capacity() * size_of::<NonNull<Segment<u64>>>();
		let heap = Writer::<u64>::SHARED + Segment::<u64>::heap(4) + Segment::<u64>::heap(8) + list;
		assert_eq!(writer.heap(), heap);
		// The reader's segment went; it starts over from the head.
		assert_eq!(get(&mut reader, 3), Ok(Some(3)));
		assert_eq!(get(&mut reader, 9), Ok(Some(9)));
	}

	/// Evicting frees the entries even while a reader that read them never reads again,
	/// and that reader finds them evicted if it does.
	#[test]
	fn eviction_frees_whatever_readers_do() {
		let _collector = COLLECTOR.lock().unwrap();
		let entry = StdArc::new(());
		let mut writer = Writer::new(0);
		let mut reader = writer.reader();
		for _ in 0..3 {
			writer.push(entry.clone());
		}
		assert!(get(&mut reader, 1).unwrap().is_some());
		writer.evict(3);
		collect();
		assert_eq!(StdArc::strong_count(&entry), 1, "freed while the reader idles");
		assert_eq!(get(&mut reader, 1), Err(Evicted));
		assert_eq!(get(&mut reader, 3), Ok(None));
	}

	/// A read in progress keeps what it can reach until it ends.
	#[test]
	fn a_read_outlasts_eviction() {
		let _collector = COLLECTOR.lock().unwrap();
		let entry = StdArc::new(());
		let mut writer = Writer::new(0);
		let mut reader = writer.reader();
		writer.push(entry.clone());
		let mut read = reader.read();
		let held = read.get(0).unwrap().unwrap();
		writer.evict(1);
		collect();
		assert_eq!(StdArc::strong_count(held), 2, "pinned by the read");
		drop(read);
		collect();
		assert_eq!(StdArc::strong_count(&entry), 1);
	}

	/// A read that spans the writer emptying the log and appending again finds the new
	/// entry, though the segment it last read was cut off from the new one.
	#[test]
	fn a_read_spans_a_full_eviction() {
		let _collector = COLLECTOR.lock().unwrap();
		let mut writer = Writer::new(0);
		let mut reader = writer.reader();
		writer.push(0u64);
		let mut read = reader.read();
		assert_eq!(read.get(0), Ok(Some(&0)));
		writer.evict(1);
		writer.push(1);
		assert_eq!(read.get(0), Err(Evicted));
		assert_eq!(read.get(1), Ok(Some(&1)));
	}

	/// Evicting everything drops the last segment with its unused slots, and the next
	/// entry starts a fresh segment that a reader whose hint was the old one does not
	/// misread.
	#[test]
	fn evicting_the_tail_starts_a_new_segment() {
		let _collector = COLLECTOR.lock().unwrap();
		let mut writer = Writer::new(0);
		let mut reader = writer.reader();
		for i in 0..2u64 {
			writer.push(i);
		}
		// The second segment holds entry 1 with room for entry 2.
		assert_eq!(layout(&writer), [(0, 1), (1, 3)]);
		assert_eq!(get(&mut reader, 1), Ok(Some(1)));

		writer.evict(2);
		assert_eq!(writer.heap(), Writer::<u64>::SHARED, "the list goes too");
		assert!(writer.segments.is_empty());

		writer.push(2);
		writer.push(3);
		assert_eq!(layout(&writer), [(2, 3), (3, 5)]);
		let mut fresh = writer.reader();
		for reader in [&mut reader, &mut fresh] {
			assert_eq!(get(reader, 1), Err(Evicted));
			assert_eq!(get(reader, 2), Ok(Some(2)));
			assert_eq!(get(reader, 3), Ok(Some(3)));
		}
	}

	/// Evicting past the end skips those indices: the next entry lands at the new base.
	#[test]
	fn evicting_past_the_end_skips() {
		let _collector = COLLECTOR.lock().unwrap();
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
		assert_eq!(get(&mut reader, 99), Err(Evicted));
		assert_eq!(get(&mut reader, 100), Ok(Some("x")));
	}

	/// A clone reads independently from where the original's segment was.
	#[test]
	fn clones_read_independently() {
		let _collector = COLLECTOR.lock().unwrap();
		let mut writer = Writer::new(0);
		let mut reader = writer.reader();
		for i in 0..20u64 {
			writer.push(i);
		}
		assert_eq!(get(&mut reader, 10), Ok(Some(10)));
		let mut clone = reader.clone();
		assert_eq!(get(&mut clone, 11), Ok(Some(11)));
		assert_eq!(get(&mut reader, 19), Ok(Some(19)));
		assert_eq!(get(&mut clone, 12), Ok(Some(12)));
	}

	/// A reader that outlives the writer sees every entry evicted, and nothing stays
	/// alive for it.
	#[test]
	fn reader_outlives_writer() {
		let _collector = COLLECTOR.lock().unwrap();
		let entry = StdArc::new(());
		let mut writer = Writer::new(0);
		let mut reader = writer.reader();
		for _ in 0..20 {
			writer.push(entry.clone());
		}
		assert!(get(&mut reader, 10).unwrap().is_some());
		drop(writer);
		collect();
		assert_eq!(StdArc::strong_count(&entry), 1);
		assert_eq!(get(&mut reader, 10), Err(Evicted));
		assert_eq!(get(&mut reader, 20), Ok(None));
	}

	/// Readers on other threads see every entry the writer published, in order, while
	/// it evicts behind them.
	#[test]
	fn concurrent_readers_see_every_entry() {
		let _collector = COLLECTOR.lock().unwrap();
		let mut writer = Writer::new(0);
		let readers: Vec<_> = (0..4)
			.map(|_| {
				let mut reader = writer.reader();
				std::thread::spawn(move || {
					let mut index = 0;
					let mut evicted = 0;
					while index < 10_000 {
						match reader.read().get(index) {
							Ok(Some(value)) => assert_eq!(*value, index),
							Ok(None) => {
								std::thread::yield_now();
								continue;
							}
							Err(Evicted) => evicted += 1,
						}
						index += 1;
					}
					evicted
				})
			})
			.collect();
		for i in 0..10_000u64 {
			writer.push(i);
			if i % 100 == 0 {
				writer.evict(i.saturating_sub(500));
			}
		}
		for reader in readers {
			reader.join().unwrap();
		}
	}
}
