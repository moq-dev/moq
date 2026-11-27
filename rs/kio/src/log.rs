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
//! one-entry log holds one slot. Eviction advances the base, retires each segment
//! wholly below it, and hands the evicted entries of a segment it leaves live to the
//! collector, which drops them in place. A reader holds nothing between reads: it reads
//! through a [`Read`], which pins the log only while it lives. A retired segment is
//! freed, and an evicted entry of a live one dropped, once every pin taken before its
//! eviction is gone (epoch-based reclamation). So whatever readers do, eviction frees an
//! entry's own allocations once it drops and its slot once its segment retires, and a
//! reader that stopped reading finds its entries [`Evicted`] when it comes back.
//!
//! # Verification
//!
//! The append, evict, and read paths carry a Verus proof, and `just verify` checks it: a
//! reader only returns a slot the writer fully wrote and published, in the segment and
//! at the offset its index names, and a slot's address never changes once written. It
//! does not prove the converse, that every index below the committed end reads as
//! present.
//!
//! The proof is written in Verus's attribute syntax, so this file is plain Rust to
//! every other build: contracts sit in `#[cfg_attr(verus_keep_ghost, verus_spec(..))]`,
//! proof steps in `proof!`, `proof_decl!`, and `proof_with!`, and the ghost types and
//! lemmas in `verus!` blocks under `#[cfg(verus_keep_ghost)]`. Only Verus sets that cfg,
//! and only the `verify/` package, which builds these sources for `just verify`, depends
//! on `vstd`, so other builds compile no Verus crate; `log/erased.rs` stands in for its
//! macros.
//!
//! The proof trusts what it does not check, each marked `TRUSTED` below. The main
//! assumptions:
//!
//! - the atomics' orderings (`log/atomic.rs`): Verus reasons as if every access were
//!   sequentially consistent;
//! - a segment's allocation and layout (`Segment::allocate`, `Segment::slot`);
//! - epoch reclamation (`pinned`): a segment a pinned read reached stays allocated
//!   until the pin drops. Retiring and freeing segments, the base, and the hint check in
//!   [`Reader::read`] sit outside the proof, so nothing proves an evicted segment
//!   unreachable to a pinned reader.
//!
//! The rest are narrower: the specs of the `SmallVec`, `UnsafeCell`, and epoch types
//! the proof names, and `drain_front`'s split of the segment list (`log/specs.rs`); the
//! heap-byte count's bound in [`Writer::reserve`]; and the unverified helpers marked
//! `external_body`.

use std::{
	alloc::{self, Layout},
	cell::UnsafeCell,
	fmt,
	mem::{MaybeUninit, offset_of},
	panic::{RefUnwindSafe, UnwindSafe},
	ptr::{self, NonNull},
	sync::{
		Arc,
		atomic::{AtomicU64, AtomicUsize, Ordering, fence},
	},
};

use crossbeam_epoch as epoch;
use smallvec::SmallVec;
#[cfg(verus_keep_ghost)]
use vstd::{
	invariant::{AtomicInvariant, InvariantPredicate},
	open_atomic_invariant,
	prelude::*,
	raw_ptr::PointsTo,
	resource::{
		Loc,
		ghost_var::{GhostVar, GhostVarAuth},
		map::{GhostMapAuth, GhostPersistentPointsTo},
	},
};

#[cfg(not(verus_keep_ghost))]
#[macro_use]
mod erased;

mod atomic;
mod specs;

use specs::drain_front;
#[cfg(verus_keep_ghost)]
use specs::items;

#[cfg(verus_keep_ghost)]
use vstd::view::View as _;

// A written slot or a segment's header, frozen: readers share it, nobody writes it again.
#[cfg(verus_only)]
use vstd::shared::Shared as Frozen;
// Verus builds that don't verify erase ghost state; this only names the erased type.
#[cfg(all(verus_keep_ghost, not(verus_only)))]
type Frozen<T> = std::marker::PhantomData<T>;

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

/// Slots in the first segment: one, since many logs never hold more.
#[cfg_attr(verus_keep_ghost, verus_verify)]
const FIRST_SEGMENT: usize = 1;

/// Slots in the largest segment, so a long log allocates in steps of this many entries.
#[cfg_attr(verus_keep_ghost, verus_verify)]
const MAX_SEGMENT: usize = 64;

/// A run of `capacity` slots from `start`, written once each, in order. The slots follow
/// the header in the same allocation, so a segment is only ever handled by its pointer.
#[repr(C)]
#[cfg_attr(verus_keep_ghost, verus_verify)]
struct Segment<T> {
	start: u64,
	capacity: usize,
	/// Slots written so far, from the first. Only the writer stores it, after the slot.
	filled: atomic::Usize,
	/// Leading slots whose entries eviction already handed to the collector to drop, while
	/// the segment still held later ones. Only the writer stores it.
	taken: AtomicUsize,
	/// Owners of the allocation: the writer until it retires the segment, and each
	/// pending drop of entries evicted from it. The last one frees it.
	owners: AtomicUsize,
	/// The segment after this one, set before anything is written into it.
	next: atomic::Ptr<Segment<T>>,
	slots: [Slot<T>; 0],
}

#[cfg(verus_keep_ghost)]
verus! {

/// Where slot `at` of the segment at `segment` holds its entry.
uninterp spec fn slot_ptr<T>(segment: *mut Segment<T>, at: int) -> *mut T;

/// Permissions for a segment's slots, by position.
type Slots<T> = Map<nat, PointsTo<T>>;

/// A segment's lasting entry in the ledger's registry: its pointer and header.
type Mark<T> = GhostPersistentPointsTo<nat, (*mut Segment<T>, Segment<T>)>;

/// A segment just allocated: its header and every slot, none written.
tracked struct Fresh<T> {
	header: PointsTo<Segment<T>>,
	filled: atomic::UsizePerm,
	next: atomic::PtrPerm<Segment<T>>,
	slots: Slots<T>,
}

impl<T> Segment<T> {
	spec fn wf(&self) -> bool {
		&&& 1 <= self.capacity <= MAX_SEGMENT
		&&& self.start + self.capacity <= u64::MAX
	}
}

} // verus!

/// TRUSTED: a segment a pinned read reached stays allocated until the pin drops, which
/// epoch reclamation guarantees and the proof does not model; the reference lives for
/// `'a`, which the caller ties to its pin.
///
/// # Safety
///
/// `ptr` stays allocated for `'a`: a reader's pin or the writer's list holds its segment.
#[cfg_attr(verus_keep_ghost, verus_verify(external_body), verus_spec(value =>
	with
		Tracked(perm): Tracked<&Frozen<PointsTo<X>>>
	requires
		perm@.ptr() == ptr,
		perm@.is_init(),
	ensures
		*value == perm@.value(),
))]
unsafe fn pinned<'a, X>(ptr: *mut X) -> &'a X {
	// SAFETY: initialized, per the permission, and alive for `'a`, per the caller.
	unsafe { &*ptr }
}

/// TRUSTED: `vstd::raw_ptr::ptr_mut_write`, with its contract, which plain builds can't
/// call without `vstd`.
///
/// # Safety
///
/// `ptr` is allocated and aligned for an `X`, and nothing else reads or writes it.
#[cfg_attr(verus_keep_ghost, verus_verify(external_body), verus_spec(
	with
		Tracked(perm): Tracked<&mut PointsTo<X>>
	requires
		old(perm).ptr() == ptr,
	ensures
		final(perm).ptr() == ptr,
		final(perm).is_init(),
		final(perm).value() == value,
	opens_invariants none
	no_unwind
))]
#[inline]
unsafe fn write<X>(ptr: *mut X, value: X) {
	// SAFETY: allocated, aligned, and exclusive, per the caller.
	unsafe { ptr.write(value) }
}

/// Fail loud once the index space is spent: the next segment's indices would pass
/// `u64::MAX`.
#[cfg_attr(verus_keep_ghost, verus_verify(external_body))]
fn exhausted() -> ! {
	panic!("log index overflow")
}

/// Bytes a log's shared state occupies.
#[cfg_attr(verus_keep_ghost, verus_verify(external_body))]
const fn shared_heap<T>() -> usize {
	crate::arc_heap::<Shared<T>>()
}

#[cfg_attr(verus_keep_ghost, verus_verify)]
impl<T> Segment<T> {
	/// TRUSTED: allocate a segment of `capacity` slots, the header first, and hand out the
	/// permission for each.
	#[cfg_attr(verus_keep_ghost, verifier::external_body)]
	#[cfg_attr(verus_keep_ghost, verus_spec(segment =>
		with
			-> perms: Tracked<(PointsTo<Self>, Slots<T>)>
		ensures
			perms@.0.ptr() == segment.as_ptr(),
			perms@.0.is_uninit(),
			forall|at: nat| at < capacity ==> {
				&&& #[trigger] perms@.1.contains_key(at)
				&&& perms@.1[at].ptr() == slot_ptr(segment.as_ptr(), at as int)
				&&& perms@.1[at].is_uninit()
			},
	))]
	fn allocate(capacity: usize) -> NonNull<Self> {
		let layout = Self::layout(capacity);
		// SAFETY: the header gives the layout a non-zero size.
		let Some(segment) = NonNull::new(unsafe { alloc::alloc(layout) }.cast::<Self>()) else {
			alloc::handle_alloc_error(layout);
		};
		proof_with!(|= Tracked::assume_new());
		segment
	}

	/// An empty segment of `capacity` slots from `start`.
	#[cfg_attr(verus_keep_ghost, verus_spec(segment =>
		with
			-> fresh: Tracked<Fresh<T>>
		requires
			1 <= capacity <= MAX_SEGMENT,
			start + capacity <= u64::MAX,
		ensures
			fresh@.header.ptr() == segment.as_ptr(),
			fresh@.header.is_init(),
			fresh@.header.value().wf(),
			fresh@.header.value().start == start,
			fresh@.header.value().capacity == capacity,
			fresh@.filled.id() == fresh@.header.value().filled.id(),
			fresh@.filled.value() == 0,
			fresh@.next.id() == fresh@.header.value().next.id(),
			fresh@.next.value()@.addr == 0,
			forall|at: nat| at < capacity ==> {
				&&& #[trigger] fresh@.slots.contains_key(at)
				&&& fresh@.slots[at].ptr() == slot_ptr(segment.as_ptr(), at as int)
				&&& fresh@.slots[at].is_uninit()
			},
	))]
	fn new(start: u64, capacity: usize) -> NonNull<Self> {
		proof_with!(=> Tracked(perms));
		let segment = Self::allocate(capacity);
		proof_with!(=> Tracked(filled_perm));
		let filled = atomic::Usize::new(0);
		proof_with!(=> Tracked(next_perm));
		let next = atomic::Ptr::new(ptr::null_mut());
		proof_decl! {
			let tracked (mut header, slots) = perms;
		}
		let value = Self {
			start,
			capacity,
			filled,
			taken: AtomicUsize::new(0),
			owners: AtomicUsize::new(1),
			next,
			slots: [],
		};
		// SAFETY: the header of the allocation just made, which no one else holds yet.
		unsafe {
			proof_with!(Tracked(&mut header));
			write(segment.as_ptr(), value);
		}
		proof_decl! {
			let tracked fresh = Fresh { header, filled: filled_perm, next: next_perm, slots };
		}
		proof_with!(|= Tracked(fresh));
		segment
	}

	/// TRUSTED: the slot at `at`, derived from the allocation's pointer so it may reach past
	/// the header.
	///
	/// # Safety
	///
	/// `segment` is live and `at` is below its capacity.
	#[cfg_attr(verus_keep_ghost, verifier::external_body)]
	#[cfg_attr(verus_keep_ghost, verus_spec(slot =>
		with
			Tracked(header): Tracked<&Frozen<PointsTo<Self>>>
		requires
			header@.ptr() == segment.as_ptr(),
			header@.is_init(),
			at < header@.value().capacity,
		ensures
			slot == slot_ptr(segment.as_ptr(), at as int),
	))]
	unsafe fn slot(segment: NonNull<Self>, at: usize) -> *mut T {
		// SAFETY: in bounds of the allocation, per the caller, and `MaybeUninit` and
		// `UnsafeCell` share their content's layout.
		unsafe {
			ptr::addr_of!((*segment.as_ptr()).slots)
				.cast::<Slot<T>>()
				.add(at)
				.cast_mut()
				.cast::<T>()
		}
	}

	/// The header of a segment a reader reached.
	///
	/// # Safety
	///
	/// `segment` stays live for `'a`.
	#[cfg_attr(verus_keep_ghost, verus_spec(header =>
		with
			Tracked(perm): Tracked<&Frozen<PointsTo<Self>>>
		requires
			perm@.ptr() == segment.as_ptr(),
			perm@.is_init(),
		ensures
			*header == perm@.value(),
	))]
	unsafe fn header<'a>(segment: NonNull<Self>) -> &'a Self {
		// SAFETY: live for `'a`, per the caller.
		unsafe {
			proof_with!(Tracked(perm));
			pinned(segment.as_ptr())
		}
	}

	/// Heap a segment of `capacity` slots occupies, as [`Self::heap`].
	#[cfg_attr(verus_keep_ghost, verifier::external_body)]
	fn bytes(capacity: usize) -> usize {
		Self::heap(capacity)
	}

	/// The written entry at `index`.
	///
	/// # Safety
	///
	/// `segment` stays live for `'a`, `index` is in its range, and the entry at `index` was
	/// written before this and is not dropped for `'a`.
	#[cfg_attr(verus_keep_ghost, verus_spec(entry =>
		with
			Tracked(header): Tracked<&Frozen<PointsTo<Self>>>,
			Tracked(slot): Tracked<&Frozen<PointsTo<T>>>,
		requires
			header@.ptr() == segment.as_ptr(),
			header@.is_init(),
			header@.value().start <= index < header@.value().start + header@.value().capacity,
			slot@.ptr() == slot_ptr(segment.as_ptr(), index - header@.value().start),
			slot@.is_init(),
		ensures
			*entry == slot@.value(),
	))]
	unsafe fn get<'a>(segment: NonNull<Self>, index: u64) -> &'a T {
		// SAFETY: live for `'a`, per the caller.
		let first = unsafe {
			proof_with!(Tracked(header));
			Self::header(segment)
		};
		let at = (index - first.start) as usize;
		// SAFETY: live and `index` in range, per the caller.
		let slot_at = unsafe {
			proof_with!(Tracked(header));
			Self::slot(segment, at)
		};
		// SAFETY: written, and live and undropped for `'a`, per the caller.
		unsafe {
			proof_with!(Tracked(slot));
			pinned(slot_at)
		}
	}
}

#[cfg(verus_keep_ghost)]
verus! {

/// What the writer has published, as the proof sees it: the committed end, the first
/// segment a reader may start from, and each segment it allocated, by an id that is
/// never reused.
ghost struct View<T> {
	end: u64,
	head: Option<nat>,
	segments: Map<nat, SegmentView<T>>,
	ids: nat,
}

ghost struct SegmentView<T> {
	ptr: *mut Segment<T>,
	header: Segment<T>,
	filled: usize,
	next: Option<nat>,
}

/// Which atomics and ghost resources a log's ledger tracks.
ghost struct Key {
	head: int,
	end: int,
	view: Loc,
	registry: Loc,
}

/// Everything readers share, held in one invariant that each atomic access opens: the
/// permission for every atomic, a frozen copy of each segment's header and each written
/// slot, and the half of the writer's view that keeps the ledger in step with it.
tracked struct Ledger<T> {
	head: atomic::PtrPerm<Segment<T>>,
	end: atomic::U64Perm,
	headers: Map<nat, Frozen<PointsTo<Segment<T>>>>,
	filled: Map<nat, atomic::UsizePerm>,
	next: Map<nat, atomic::PtrPerm<Segment<T>>>,
	slots: Map<(nat, nat), Frozen<PointsTo<T>>>,
	/// Each segment's pointer and header, for good: what lets a reader holding a segment
	/// between two accesses find it in the ledger again.
	registry: GhostMapAuth<nat, (*mut Segment<T>, Segment<T>)>,
	marks: Map<nat, Mark<T>>,
	view: GhostVar<View<T>>,
}

struct LedgerPred;

impl<T> InvariantPredicate<Key, Ledger<T>> for LedgerPred {
	closed spec fn inv(key: Key, ledger: Ledger<T>) -> bool {
		let view = ledger.view@;
		&&& ledger.head.id() == key.head
		&&& ledger.end.id() == key.end
		&&& ledger.view.id() == key.view
		&&& ledger.registry.id() == key.registry
		&&& ledger.end.value() == view.end
		&&& match view.head {
			None => ledger.head.value()@.addr == 0,
			Some(id) => view.segments.contains_key(id) && ledger.head.value() == view.segments[id].ptr,
		}
		&&& forall|id: nat| #[trigger] view.segments.contains_key(id) ==> id < view.ids
		&&& ledger.registry@.dom() == view.segments.dom()
		&&& forall|id: nat| #[trigger] view.segments.contains_key(id) ==> segment_inv(ledger, id)
	}
}

/// Segment `id`'s part of the ledger: its header and atomics match the writer's view,
/// and every slot below `filled` is written.
spec fn segment_inv<T>(ledger: Ledger<T>, id: nat) -> bool {
	let view = ledger.view@;
	let segment = view.segments[id];
	&&& ledger.registry@[id] == (segment.ptr, segment.header)
	&&& ledger.marks.contains_key(id)
	&&& ledger.marks[id].id() == ledger.registry.id()
	&&& ledger.marks[id]@ == (id, (segment.ptr, segment.header))
	&&& ledger.headers.contains_key(id)
	&&& ledger.headers[id]@.ptr() == segment.ptr
	&&& ledger.headers[id]@.is_init()
	&&& ledger.headers[id]@.value() == segment.header
	&&& segment.header.wf()
	&&& ledger.filled.contains_key(id)
	&&& ledger.filled[id].id() == segment.header.filled.id()
	&&& ledger.filled[id].value() == segment.filled
	&&& segment.filled <= segment.header.capacity
	&&& ledger.next.contains_key(id)
	&&& ledger.next[id].id() == segment.header.next.id()
	&&& match segment.next {
		None => ledger.next[id].value()@.addr == 0,
		Some(next) => view.segments.contains_key(next) && ledger.next[id].value() == view.segments[next].ptr,
	}
	&&& forall|at: nat| at < segment.filled ==> {
		&&& #[trigger] ledger.slots.contains_key((id, at))
		&&& ledger.slots[(id, at)]@.ptr() == slot_ptr(segment.ptr, at as int)
		&&& ledger.slots[(id, at)]@.is_init()
	}
}

/// A segment someone reached, with what it needs to find it in the ledger again.
tracked struct Reached<T> {
	ghost id: nat,
	header: Frozen<PointsTo<Segment<T>>>,
	mark: Mark<T>,
}

impl<T> Reached<T> {
	spec fn wf_at(&self, key: Key, ptr: *mut Segment<T>) -> bool {
		&&& self.header@.ptr() == ptr
		&&& self.header@.is_init()
		&&& self.header@.value().wf()
		&&& self.mark.id() == key.registry
		&&& self.mark@ == (self.id, (ptr, self.header@.value()))
	}

	spec fn wf(&self, key: Key, segment: NonNull<Segment<T>>) -> bool {
		self.wf_at(key, segment.as_ptr())
	}

	proof fn copy(tracked &self) -> (tracked copy: Self)
		ensures
			forall|key: Key, ptr: *mut Segment<T>| self.wf_at(key, ptr) ==> copy.wf_at(key, ptr),
			copy.id == self.id,
			copy.header@ == self.header@,
	{
		Reached { id: self.id, header: self.header.clone(), mark: self.mark.duplicate() }
	}

	/// Segment `id` as the ledger holds it.
	proof fn of(tracked ledger: &Ledger<T>, key: Key, id: nat) -> (tracked reached: Self)
		requires
			LedgerPred::inv(key, *ledger),
			ledger.view@.segments.contains_key(id),
		ensures
			reached.id == id,
			reached.wf_at(key, ledger.view@.segments[id].ptr),
			reached.header@.value() == ledger.view@.segments[id].header,
	{
		assert(segment_inv(*ledger, id));
		Reached { id, header: ledger.headers.tracked_borrow(id).clone(), mark: ledger.marks.tracked_borrow(id).duplicate() }
	}

	/// What the ledger holds for the segment this reached.
	proof fn find(tracked &self, tracked ledger: &Ledger<T>, key: Key, ptr: *mut Segment<T>)
		requires
			LedgerPred::inv(key, *ledger),
			self.wf_at(key, ptr),
		ensures
			ledger.view@.segments.contains_key(self.id),
			segment_inv(*ledger, self.id),
			ledger.view@.segments[self.id].ptr == ptr,
			ledger.view@.segments[self.id].header == self.header@.value(),
	{
		self.mark.agree(&ledger.registry);
		assert(ledger.registry@.contains_key(self.id));
		assert(segment_inv(*ledger, self.id));
	}
}

spec const LEDGER: int = 0;

impl<T> Shared<T> {
	spec fn key(&self) -> Key {
		self.ledger@.constant()
	}

	spec fn wf(&self) -> bool {
		&&& self.key().head == self.head.id()
		&&& self.key().end == self.end.id()
		&&& self.ledger@.namespace() == LEDGER
	}
}

/// The writer's ghost state: its half of the view, each live segment, and the tail's
/// unwritten slots.
tracked struct Pen<T> {
	view: GhostVarAuth<View<T>>,
	ghost ids: Seq<nat>,
	live: Map<nat, Reached<T>>,
	unwritten: Slots<T>,
}

/// The view once the writer adds a segment at `ptr`, linked from `link` or, without one,
/// published as the head.
spec fn registered<T>(view: View<T>, ptr: *mut Segment<T>, header: Segment<T>, link: Option<nat>) -> View<T> {
	let id = view.ids;
	let segments = view.segments.insert(id, SegmentView { ptr, header, filled: 0, next: None });
	match link {
		Some(tail) => View {
			segments: segments.insert(tail, SegmentView { next: Some(id), ..segments[tail] }),
			ids: id + 1,
			..view
		},
		None => View { head: Some(id), segments, ids: id + 1, ..view },
	}
}

/// The ledger, `before` the store that links a new segment at `ptr` from `link` or
/// publishes it as the head, plus that store, gains the segment.
proof fn register<T>(
	tracked ledger: &mut Ledger<T>,
	tracked auth: &mut GhostVarAuth<View<T>>,
	key: Key,
	before: Ledger<T>,
	ptr: *mut Segment<T>,
	tracked header: Frozen<PointsTo<Segment<T>>>,
	tracked filled: atomic::UsizePerm,
	tracked next: atomic::PtrPerm<Segment<T>>,
	link: Option<nat>,
) -> (tracked added: Reached<T>)
	requires
		LedgerPred::inv(key, before),
		old(auth).id() == key.view,
		old(auth)@ == before.view@,
		header@.ptr() == ptr,
		header@.is_init(),
		header@.value().wf(),
		filled.id() == header@.value().filled.id(),
		filled.value() == 0,
		next.id() == header@.value().next.id(),
		next.value()@.addr == 0,
		old(ledger).view == before.view,
		old(ledger).registry == before.registry,
		old(ledger).marks == before.marks,
		old(ledger).headers == before.headers,
		old(ledger).filled == before.filled,
		old(ledger).slots == before.slots,
		old(ledger).end == before.end,
		match link {
			Some(tail) => {
				&&& before.view@.segments.contains_key(tail)
				&&& old(ledger).head == before.head
				&&& old(ledger).next.dom() == before.next.dom()
				&&& forall|id: nat| id != tail ==> old(ledger).next[id] == before.next[id]
				&&& old(ledger).next[tail].id() == before.next[tail].id()
				&&& old(ledger).next[tail].value() == ptr
			},
			None => {
				&&& old(ledger).next == before.next
				&&& old(ledger).head.id() == before.head.id()
				&&& old(ledger).head.value() == ptr
			},
		},
	ensures
		LedgerPred::inv(key, *final(ledger)),
		final(auth).id() == key.view,
		final(auth)@ == registered(before.view@, ptr, header@.value(), link),
		added.id == before.view@.ids,
		added.wf_at(key, ptr),
		added.header@ == header@,
{
	let ghost view = before.view@;
	let ghost id = view.ids;
	let ghost new_view = registered(view, ptr, header@.value(), link);
	assert(forall|n: nat| #[trigger] view.segments.contains_key(n) ==> n < id);
	let tracked mark = ledger.registry.insert(id, (ptr, header@.value())).persist();
	ledger.marks.tracked_insert(id, mark.duplicate());
	ledger.headers.tracked_insert(id, header.clone());
	ledger.filled.tracked_insert(id, filled);
	ledger.next.tracked_insert(id, next);
	auth.update(&mut ledger.view, new_view);
	assert forall|i: nat| #[trigger] new_view.segments.contains_key(i) implies segment_inv(*ledger, i) by {
		if i == id {
			assert(new_view.segments[i].next is None);
			assert(new_view.segments[i].filled == 0);
			let seg = new_view.segments[i];
			assert(ledger.registry@[i] == (seg.ptr, seg.header));
			assert(ledger.marks.contains_key(i));
			assert(ledger.marks[i].id() == ledger.registry.id());
			assert(ledger.marks[i]@ == (i, (seg.ptr, seg.header)));
			assert(ledger.headers[i]@.ptr() == seg.ptr);
			assert(ledger.headers[i]@.value() == seg.header);
			assert(ledger.filled[i].id() == seg.header.filled.id());
			assert(ledger.next[i].id() == seg.header.next.id());
			assert(ledger.next[i].value()@.addr == 0);
			assert(segment_inv(*ledger, i));
		} else {
			assert(view.segments.contains_key(i));
			assert(segment_inv(before, i));
			assert(ledger.registry@[i] == before.registry@[i]);
			assert(ledger.marks[i] == before.marks[i]);
			assert(ledger.headers[i] == before.headers[i]);
			assert(ledger.filled[i] == before.filled[i]);
			assert(new_view.segments[i].filled == view.segments[i].filled);
			assert(new_view.segments[i].header == view.segments[i].header);
			assert(new_view.segments[i].ptr == view.segments[i].ptr);
			assert(ledger.slots == before.slots);
			assert(ledger.registry.id() == before.registry.id());
			if link == Some(i) {
				assert(ledger.next[i].value() == ptr);
				assert(ledger.next[i].id() == before.next[i].id());
				assert(ledger.slots == before.slots);
				assert(segment_inv(*ledger, i));
			} else {
				assert(new_view.segments[i].next == view.segments[i].next);
				assert(ledger.next[i] == before.next[i]);
				if let Some(n) = view.segments[i].next {
					assert(view.segments.contains_key(n));
					assert(n != id);
					assert(new_view.segments[n].ptr == view.segments[n].ptr);
				}
				assert(segment_inv(*ledger, i));
			}
		}
	}
	assert(ledger.registry@.dom() =~= new_view.segments.dom());
	Reached { id, header, mark }
}

} // verus!

/// The state a writer and its readers share.
#[cfg_attr(verus_keep_ghost, verus_verify)]
struct Shared<T> {
	/// One past the last committed entry, Release-stored after its slot is written. Only
	/// the writer stores it.
	end: atomic::U64,
	/// Entries below this are evicted. Only the writer stores it, before retiring the
	/// segments it passes.
	base: AtomicU64,
	/// The oldest live segment, where a reader without a usable segment starts.
	head: atomic::Ptr<Segment<T>>,
	#[cfg(verus_keep_ghost_body)]
	ledger: Tracked<AtomicInvariant<Key, Ledger<T>, LedgerPred>>,
	#[cfg(test)]
	collector: Option<epoch::Collector>,
}

#[cfg_attr(verus_keep_ghost, verus_verify)]
impl<T> Shared<T> {
	#[cfg_attr(verus_keep_ghost, verifier::external_body)]
	fn pin(&self) -> epoch::Guard {
		#[cfg(test)]
		if let Some(collector) = &self.collector {
			return collector.register().pin();
		}
		epoch::pin()
	}
}

/// Appends entries to a log and evicts them from its front.
///
/// Not `Clone`: appends take `&mut self`, so one writer orders them.
#[cfg_attr(verus_keep_ghost, verus_verify(reject_recursive_types(T)))]
pub struct Writer<T: Send + Sync + 'static> {
	shared: Arc<Shared<T>>,
	/// The live segments, oldest first. The last is the one the next entry goes into,
	/// until it fills. Inline while there is one, which is all most logs hold.
	segments: SmallVec<[NonNull<Segment<T>>; 1]>,
	/// Bytes of the shared state and the live segments, excluding `segments` itself.
	heap: usize,
	#[cfg(verus_keep_ghost_body)]
	pen: Tracked<Pen<T>>,
}

#[cfg(verus_keep_ghost)]
verus! {

impl<T: Send + Sync + 'static> Writer<T> {
	closed spec fn key(&self) -> Key {
		self.shared.key()
	}

	closed spec fn view(&self) -> View<T> {
		self.pen@.view@
	}

	/// The segment the list holds at `i`, as the writer's view sees it.
	closed spec fn live(&self, i: int) -> SegmentView<T> {
		self.view().segments[self.pen@.ids[i]]
	}

	/// The committed end, as the writer's view holds it.
	pub closed spec fn committed(&self) -> u64 {
		self.view().end
	}

	/// The tail has a slot for the next entry.
	pub closed spec fn has_room(&self) -> bool {
		let list = items(&self.segments);
		&&& list.len() > 0
		&&& self.view().end < self.live(list.len() - 1).header.start + self.live(list.len() - 1).header.capacity
	}

	/// The listed segment at `i` is in the view, as the writer's copy has it, and holds
	/// nothing past the end.
	closed spec fn listed_at(&self, i: int) -> bool {
		let view = self.view();
		let list = items(&self.segments);
		let id = self.pen@.ids[i];
		&&& view.segments.contains_key(id)
		&&& self.pen@.live.contains_key(id)
		&&& self.pen@.live[id].id == id
		&&& self.pen@.live[id].wf(self.key(), list[i])
		&&& view.segments[id].ptr == list[i].as_ptr()
		&&& view.segments[id].header == self.pen@.live[id].header@.value()
		&&& view.segments[id].header.start + view.segments[id].filled <= view.end
	}

	closed spec fn listed(&self) -> bool {
		&&& self.shared.wf()
		&&& self.pen@.view.id() == self.key().view
		&&& self.pen@.ids.len() == items(&self.segments).len()
		&&& forall|i: int, j: int| 0 <= i < j < self.pen@.ids.len() ==> self.pen@.ids[i] < self.pen@.ids[j]
		&&& forall|i: int| 0 <= i < self.pen@.ids.len() ==> self.pen@.ids[i] < self.view().ids
		&&& forall|i: int| 0 <= i < items(&self.segments).len() ==> #[trigger] self.listed_at(i)
	}

	/// The tail, the last listed segment, is the one being written: nothing links past
	/// it, its written slots end at the committed end, and the rest are the writer's.
	closed spec fn tail_ok(&self) -> bool {
		let list = items(&self.segments);
		list.len() > 0 ==> {
			let tail = self.live(list.len() - 1);
			&&& tail.next is None
			&&& tail.header.start + tail.filled == self.view().end
			&&& forall|at: nat| tail.filled <= at < tail.header.capacity ==> {
				&&& #[trigger] self.pen@.unwritten.contains_key(at)
				&&& self.pen@.unwritten[at].ptr() == slot_ptr(tail.ptr, at as int)
				&&& self.pen@.unwritten[at].is_uninit()
			}
		}
	}

	/// The writer's state agrees with what it published.
	pub closed spec fn wf(&self) -> bool {
		self.listed() && self.tail_ok()
	}
}

} // verus!

#[cfg_attr(verus_keep_ghost, verus_verify)]
impl<T: Send + Sync + 'static> Writer<T> {
	/// An empty log whose first entry gets index `start`.
	#[cfg_attr(verus_keep_ghost, verus_spec(writer =>
		ensures
			writer.wf(),
	))]
	pub fn new(start: u64) -> Self {
		proof_with!(=> Tracked(end_perm));
		let end = atomic::U64::new(start);
		proof_with!(=> Tracked(head_perm));
		let head = atomic::Ptr::new(ptr::null_mut());
		proof_decl! {
			let tracked (view_auth, view) = GhostVarAuth::new(View { end: start, head: None, segments: Map::empty(), ids: 0 });
			let tracked (registry, _) = GhostMapAuth::new(Map::empty());
			let ghost key = Key { head: head.id(), end: end.id(), view: view.id(), registry: registry.id() };
			let tracked ledger = Ledger {
				head: head_perm,
				end: end_perm,
				headers: Map::tracked_empty(),
				filled: Map::tracked_empty(),
				next: Map::tracked_empty(),
				slots: Map::tracked_empty(),
				registry,
				marks: Map::tracked_empty(),
				view,
			};
		}
		proof! {
			assert(ledger.registry@.dom() =~= ledger.view@.segments.dom());
		}
		proof_decl! {
			let tracked ledger = AtomicInvariant::new(key, ledger, LEDGER);
			let tracked pen = Pen { view: view_auth, ids: Seq::empty(), live: Map::tracked_empty(), unwritten: Map::tracked_empty() };
		}
		proof_with!(ledger: Tracked(ledger));
		let shared = Shared {
			end,
			base: AtomicU64::new(start),
			head,
			#[cfg(test)]
			collector: None,
		};
		proof_with!(pen: Tracked(pen));
		Self {
			shared: Arc::new(shared),
			segments: SmallVec::new(),
			heap: shared_heap::<T>(),
		}
	}

	/// A reader of this log.
	#[cfg_attr(verus_keep_ghost, verus_spec(reader =>
		requires
			self.wf(),
		ensures
			reader.wf(),
	))]
	pub fn reader(&self) -> Reader<T> {
		proof_with!(reached: Tracked(None));
		Reader {
			shared: self.shared.clone(),
			hint: None,
		}
	}

	/// One past the last committed entry: the index the next append gets.
	///
	/// Once the last index, `u64::MAX - 1`, is appended, this is `u64::MAX`: the end of
	/// the index space, which no entry can take.
	#[cfg_attr(verus_keep_ghost, verus_spec(end =>
		requires
			self.wf(),
		ensures
			end == self.committed(),
	))]
	pub fn end(&self) -> u64 {
		let end;
		open_atomic_invariant!(self.shared.ledger.borrow() => ledger => {
			proof! { self.pen.borrow().view.agree(&ledger.view); }
			// Relaxed: only this writer stores it.
			#[cfg_attr(verus_keep_ghost, verus_spec(with Tracked(&ledger.end)))]
			let loaded = self.shared.end.load(Ordering::Relaxed);
			end = loaded;
		});
		end
	}

	/// Allocate the slot the next entry goes into, if there is none, without publishing
	/// anything.
	///
	/// # Panics
	///
	/// With "log index overflow" when there is no slot and [`Self::end`] is already
	/// `u64::MAX`: the index space is spent.
	#[cfg_attr(verus_keep_ghost, verus_spec(
		requires
			old(self).wf(),
		ensures
			final(self).wf(),
			final(self).committed() == old(self).committed(),
			final(self).has_room(),
	))]
	pub fn reserve(&mut self) {
		let end = self.end();
		let list: &[NonNull<Segment<T>>] = &self.segments;
		proof_decl! {
			let ghost tail_id = self.pen@.ids.last();
		}
		proof! {
			assert(list@ == items(&self.segments));
			if list.len() > 0 {
				assert(self.listed_at(list.len() - 1));
			}
		}
		let full = if !list.is_empty() {
			proof_decl! {
				let tracked live = self.pen.borrow().live.tracked_borrow(tail_id);
			}
			// SAFETY: the writer's list holds the segment while `self` is borrowed.
			let tail = unsafe {
				proof_with!(Tracked(&live.header));
				Segment::header(list[list.len() - 1])
			};
			if end < tail.start + tail.capacity as u64 {
				return;
			}
			Some(tail)
		} else {
			None
		};
		let capacity = match full {
			Some(full) => (full.capacity * 2).min(MAX_SEGMENT),
			None => FIRST_SEGMENT,
		};
		// Clamped to the indices left, so every index below `u64::MAX` stays usable.
		let room = u64::MAX - end;
		if room == 0 {
			exhausted();
		}
		let capacity = if capacity as u64 > room {
			room as usize
		} else {
			capacity
		};

		proof_with!(=> Tracked(fresh));
		let segment = Segment::new(end, capacity);
		proof_decl! {
			let tracked Fresh { header, filled, next, slots } = fresh;
			let tracked header = Frozen::new(header);
		}
		let ptr = segment.as_ptr();
		proof_decl! {
			let ghost link = if full.is_some() { Some(tail_id) } else { None };
			let ghost key = self.key();
			let ghost mid = *self;
			let tracked mut added = None;
		}
		match full {
			// Linked before any entry lands in it, so a reader that passes the full tail
			// finds it.
			Some(full) => {
				open_atomic_invariant!(self.shared.ledger.borrow() => ledger => {
					proof_decl! {
						let ghost before = ledger;
					}
					proof! {
						self.pen.borrow().view.agree(&ledger.view);
						self.pen.borrow().live.tracked_borrow(tail_id).find(&ledger, key, list[list.len() - 1].as_ptr());
						assert(*full == before.view@.segments[tail_id].header);
						assert(before.next[tail_id].id() == full.next.id());
					}
					#[cfg_attr(verus_keep_ghost, verus_spec(with Tracked(ledger.next.tracked_borrow_mut(tail_id))))]
					full.next.store(ptr, Ordering::Release);
					proof! {
						assert(before.view@.segments.contains_key(tail_id));
						assert(ledger.head == before.head);
						assert(ledger.next.dom() == before.next.dom());
						assert(ledger.next[tail_id].id() == before.next[tail_id].id());
						assert(ledger.next[tail_id].value() == ptr);
						let tracked pen = self.pen.borrow_mut();
						added = Some(register(&mut ledger, &mut pen.view, key, before, ptr, header, filled, next, link));
					}
				});
			}
			// Published before any entry in it, so a reader that sees one can locate it.
			None => {
				open_atomic_invariant!(self.shared.ledger.borrow() => ledger => {
					proof_decl! {
						let ghost before = ledger;
					}
					proof! { self.pen.borrow().view.agree(&ledger.view); }
					#[cfg_attr(verus_keep_ghost, verus_spec(with Tracked(&mut ledger.head)))]
					self.shared.head.store(ptr, Ordering::Release);
					proof! {
						let tracked pen = self.pen.borrow_mut();
						added = Some(register(&mut ledger, &mut pen.view, key, before, ptr, header, filled, next, link));
					}
				});
			}
		}
		let bytes = Segment::<T>::bytes(capacity);
		proof! {
			let tracked added = added.tracked_unwrap();
			let tracked pen = self.pen.borrow_mut();
			pen.ids = pen.ids.push(added.id);
			pen.live.tracked_insert(added.id, added);
			pen.unwritten = slots;
			// TRUSTED: a log never counts more bytes than the address space holds.
			assume(self.heap + bytes <= usize::MAX);
		}
		self.heap += bytes;
		self.segments.push(segment);
		proof! {
			let len = items(&self.segments).len();
			let new_id = mid.view().ids;
			assert(self.view() == registered(mid.view(), ptr, self.pen@.live[new_id].header@.value(), link));
			assert(self.pen@.ids.last() == new_id);
			assert(self.view().segments[new_id].ptr == ptr);
			assert forall|i: int| 0 <= i < len implies #[trigger] self.listed_at(i) by {
				if i < len - 1 {
					assert(mid.listed_at(i));
					assert(self.pen@.ids[i] == mid.pen@.ids[i]);
					assert(self.pen@.ids[i] < new_id);
				}
			}
			let tail = self.live(len - 1);
			assert(tail.next is None);
			assert(tail.header.start == end);
			assert(tail.filled == 0);
			assert(tail.header.capacity == capacity);
			assert forall|at: nat| tail.filled <= at < tail.header.capacity implies {
				&&& #[trigger] self.pen@.unwritten.contains_key(at)
				&&& self.pen@.unwritten[at].ptr() == slot_ptr(tail.ptr, at as int)
				&&& self.pen@.unwritten[at].is_uninit()
			} by {
				assert(slots.contains_key(at));
			}
			// Spelled out: Verus fails to unfold `tail_ok` here on its own.
			assert({
				let list = items(&self.segments);
				list.len() > 0 ==> {
					let tail = self.live(list.len() - 1);
					&&& tail.next is None
					&&& tail.header.start + tail.filled == self.view().end
					&&& forall|at: nat| tail.filled <= at < tail.header.capacity ==> {
						&&& #[trigger] self.pen@.unwritten.contains_key(at)
						&&& self.pen@.unwritten[at].ptr() == slot_ptr(tail.ptr, at as int)
						&&& self.pen@.unwritten[at].is_uninit()
					}
				}
			});
			assert(self.tail_ok());
		}
	}

	/// Append `value` at [`Self::end`], visible to every reader once this returns.
	///
	/// # Panics
	///
	/// With "log index overflow" when [`Self::end`] is already `u64::MAX`: the index
	/// space is spent.
	#[cfg_attr(verus_keep_ghost, verus_spec(
		requires
			old(self).wf(),
		ensures
			final(self).wf(),
	))]
	pub fn push(&mut self, value: T) {
		self.reserve();
		proof_decl! {
			let ghost mid = *self;
		}
		let end = self.end();
		let list: &[NonNull<Segment<T>>] = &self.segments;
		let tail = list[list.len() - 1];
		proof_decl! {
			let ghost id = self.pen@.ids.last();
		}
		proof! {
			assert(list@ == items(&self.segments));
			assert(self.listed_at(list.len() - 1));
		}
		proof_decl! {
			let tracked live = self.pen.borrow().live.tracked_borrow(id).copy();
		}
		// SAFETY: the writer's list holds the segment while `self` is borrowed.
		let header = unsafe {
			proof_with!(Tracked(&live.header));
			Segment::header(tail)
		};
		let at = (end - header.start) as usize;
		proof! {
			assert(*header == self.live(list.len() - 1).header);
			assert(self.pen@.unwritten.contains_key(at as nat));
		}
		proof_decl! {
			let tracked mut slot = self.pen.borrow_mut().unwritten.tracked_remove(at as nat);
		}
		// SAFETY: the writer's list holds the segment, and `reserve` left `end` inside it.
		let place = unsafe {
			proof_with!(Tracked(&live.header));
			Segment::slot(tail, at)
		};
		// SAFETY: a slot of a live segment, which no reader sees until `filled` covers it.
		unsafe {
			proof_with!(Tracked(&mut slot));
			write(place, value);
		}
		proof_decl! {
			let tracked slot = Frozen::new(slot);
		}
		open_atomic_invariant!(self.shared.ledger.borrow() => ledger => {
			proof_decl! {
				let ghost before = ledger;
			}
			proof! {
				self.pen.borrow().view.agree(&ledger.view);
				live.find(&ledger, self.key(), tail.as_ptr());
			}
			// Release so a reader, or the free, that sees the count sees the slot.
			#[cfg_attr(verus_keep_ghost, verus_spec(with Tracked(ledger.filled.tracked_borrow_mut(id))))]
			header.filled.store(at + 1, Ordering::Release);
			proof! {
				ledger.slots.tracked_insert((id, at as nat), slot);
				let view = before.view@;
				let new_view = View {
					segments: view.segments.insert(id, SegmentView { filled: (at + 1) as usize, ..view.segments[id] }),
					..view
				};
				self.pen.borrow_mut().view.update(&mut ledger.view, new_view);
				assert forall|i: nat| #[trigger] new_view.segments.contains_key(i) implies segment_inv(ledger, i) by {
					assert(segment_inv(before, i));
					// Spelled out: Verus proves each conjunct of `segment_inv` but not the whole.
					let segment = new_view.segments[i];
					assert forall|a: nat| a < segment.filled implies {
						&&& #[trigger] ledger.slots.contains_key((i, a))
						&&& ledger.slots[(i, a)]@.ptr() == slot_ptr(segment.ptr, a as int)
						&&& ledger.slots[(i, a)]@.is_init()
					} by {
						if i != id || a < at {
							assert(before.slots.contains_key((i, a)));
						}
					}
				}
				assert(ledger.registry@.dom() =~= new_view.segments.dom());
			}
		});
		open_atomic_invariant!(self.shared.ledger.borrow() => ledger => {
			proof_decl! {
				let ghost before = ledger;
			}
			proof! { self.pen.borrow().view.agree(&ledger.view); }
			#[cfg_attr(verus_keep_ghost, verus_spec(with Tracked(&mut ledger.end)))]
			self.shared.end.store(end + 1, Ordering::Release);
			proof! {
				let new_view = View { end: (end + 1) as u64, ..before.view@ };
				self.pen.borrow_mut().view.update(&mut ledger.view, new_view);
				assert forall|i: nat| #[trigger] new_view.segments.contains_key(i) implies segment_inv(ledger, i) by {
					assert(segment_inv(before, i));
				}
			}
		});
		proof! {
			assert(self.pen@.ids == mid.pen@.ids);
			assert(self.view().segments[id].filled == at + 1);
			assert(self.view().end == end + 1);
			assert forall|i: int| 0 <= i < items(&self.segments).len() implies #[trigger] self.listed_at(i) by {
				assert(mid.listed_at(i));
			}
			assert(self.tail_ok());
		}
	}

	/// Evict every entry below `index`, retiring each segment wholly below it. An
	/// `index` past the end skips to it: the next entry gets `index`.
	///
	/// A retired segment is freed once no read that began before this is still running,
	/// whether or not its readers ever read again. The last segment goes only with every
	/// entry, and its unused slots with it: the next entry starts a segment of its own.
	#[cfg_attr(verus_keep_ghost, verus_spec(
		requires
			old(self).wf(),
		ensures
			final(self).wf(),
	))]
	pub fn evict(&mut self, index: u64) {
		// Evicting to the end goes on even when nothing is left to evict, so a reserved
		// segment no entry reached goes with it.
		if index <= self.base() && index < self.end() {
			return;
		}
		// Stored before the segments are retired: a reader that still sees the old base
		// pinned before their retirement, so they outlive its read.
		self.shared.base.store(index, Ordering::Release);
		let skip = index > self.end();
		if skip {
			open_atomic_invariant!(self.shared.ledger.borrow() => ledger => {
				proof_decl! {
					let ghost before = ledger;
				}
				proof! { self.pen.borrow().view.agree(&ledger.view); }
				#[cfg_attr(verus_keep_ghost, verus_spec(with Tracked(&mut ledger.end)))]
				self.shared.end.store(index, Ordering::Release);
				proof! {
					let new_view = View { end: index, ..before.view@ };
					self.pen.borrow_mut().view.update(&mut ledger.view, new_view);
					assert forall|i: nat| #[trigger] new_view.segments.contains_key(i) implies segment_inv(ledger, i) by {
						assert(segment_inv(before, i));
					}
				}
			});
			proof! {
				assert forall|i: int| 0 <= i < items(&self.segments).len() implies #[trigger] self.listed_at(i) by {
					assert(old(self).listed_at(i));
				}
			}
		}

		// The segments whose written entries all fall below `index`: a prefix, since
		// segments are in order.
		let list: &[NonNull<Segment<T>>] = &self.segments;
		proof! { assert(list@ == items(&self.segments)); }
		let mut evicted = 0;
		#[cfg_attr(verus_keep_ghost, verus_spec(
			invariant
				self.listed(),
				skip ==> index == self.committed(),
				!skip ==> self.tail_ok(),
				list@ == items(&self.segments),
				evicted <= list.len(),
			ensures
				skip ==> evicted == list.len(),
			decreases list.len() - evicted,
		))]
		while evicted < list.len() {
			proof! { assert(self.listed_at(evicted as int)); }
			proof_decl! {
				let ghost id = self.pen@.ids[evicted as int];
				let tracked live = self.pen.borrow().live.tracked_borrow(id);
			}
			// SAFETY: the writer's list holds the segment while `self` is borrowed.
			let segment = unsafe {
				proof_with!(Tracked(&live.header));
				Segment::header(list[evicted])
			};
			let filled;
			open_atomic_invariant!(self.shared.ledger.borrow() => ledger => {
				proof! {
					self.pen.borrow().view.agree(&ledger.view);
					live.find(&ledger, self.key(), list[evicted as int].as_ptr());
				}
				// Relaxed: only this writer stores it.
				#[cfg_attr(verus_keep_ghost, verus_spec(with Tracked(ledger.filled.tracked_borrow(id))))]
				let loaded = segment.filled.load(Ordering::Relaxed);
				filled = loaded;
				proof! { assert(filled == self.view().segments[id].filled); }
			});
			if segment.start + filled as u64 > index {
				break;
			}
			evicted += 1;
		}
		if evicted == 0 {
			self.take_evicted(index);
			return;
		}
		let head = if evicted < list.len() {
			list[evicted].as_ptr()
		} else {
			ptr::null_mut()
		};
		proof_decl! {
			let ghost next = if evicted < list.len() { Some(self.pen@.ids[evicted as int]) } else { None };
		}
		proof! {
			if evicted < list.len() {
				assert(self.listed_at(evicted as int));
			}
		}
		proof_decl! {
			let ghost pre = *self;
		}
		open_atomic_invariant!(self.shared.ledger.borrow() => ledger => {
			proof_decl! {
				let ghost before = ledger;
			}
			proof! { self.pen.borrow().view.agree(&ledger.view); }
			#[cfg_attr(verus_keep_ghost, verus_spec(with Tracked(&mut ledger.head)))]
			self.shared.head.store(head, Ordering::Release);
			proof! {
				let new_view = View { head: next, ..before.view@ };
				self.pen.borrow_mut().view.update(&mut ledger.view, new_view);
				assert forall|i: nat| #[trigger] new_view.segments.contains_key(i) implies segment_inv(ledger, i) by {
					assert(segment_inv(before, i));
				}
			}
		});
		proof! {
			assert forall|i: int| 0 <= i < items(&self.segments).len() implies #[trigger] self.listed_at(i) by {
				assert(pre.listed_at(i));
			}
		}
		proof_decl! {
			let ghost mid = *self;
		}
		let retired = drain_front(&mut self.segments, evicted);
		proof! {
			let tracked pen = self.pen.borrow_mut();
			pen.ids = pen.ids.subrange(evicted as int, pen.ids.len() as int);
		}
		if self.segments.is_empty() {
			// Free a spilled list too, so an emptied log costs only its shared state.
			self.segments = SmallVec::new();
		}
		self.uncount(&retired);
		self.retire(retired);
		self.take_evicted(index);
		proof! {
			let len = items(&self.segments).len();
			assert forall|i: int| 0 <= i < len implies #[trigger] self.listed_at(i) by {
				assert(mid.listed_at(i + evicted));
				assert(items(&self.segments)[i] == items(&mid.segments)[i + evicted]);
				assert(self.pen@.ids[i] == mid.pen@.ids[i + evicted]);
			}
			if len > 0 {
				assert(self.pen@.ids[len - 1] == mid.pen@.ids[mid.pen@.ids.len() - 1]);
				assert(items(&self.segments)[len - 1] == items(&mid.segments)[mid.pen@.ids.len() - 1]);
			}
		}
	}

	/// The first entry not evicted.
	#[cfg_attr(verus_keep_ghost, verifier::external_body)]
	pub fn base(&self) -> u64 {
		// Relaxed: only this writer stores it.
		self.shared.base.load(Ordering::Relaxed)
	}

	/// Stop counting the heap of segments leaving the log.
	#[cfg_attr(verus_keep_ghost, verifier::external_body)]
	#[cfg_attr(verus_keep_ghost, verus_spec(
		ensures
			final(self).shared == old(self).shared,
			final(self).segments == old(self).segments,
			final(self).pen == old(self).pen,
	))]
	fn uncount(&mut self, segments: &[NonNull<Segment<T>>]) {
		for segment in segments {
			// SAFETY: still live; retired after this.
			self.heap -= Segment::<T>::heap(unsafe { segment.as_ref() }.capacity);
		}
	}

	/// Hand the entries evicted from the oldest live segment to the epoch collector,
	/// dropping them in place once no read pinned before the base moved can reach them,
	/// rather than when the whole segment retires: an entry can be large, and the segment
	/// holds up to 64. They stay in their slots until then, so a read still borrowing one
	/// sees it whole, interior mutability included.
	#[cfg_attr(verus_keep_ghost, verifier::external_body)]
	fn take_evicted(&self, index: u64) {
		let Some(&segment) = self.segments.first() else {
			return;
		};
		// SAFETY: the writer's segments are live.
		let header = unsafe { segment.as_ref() };
		let taken = header.taken.load(Ordering::Relaxed);
		let end = (index.saturating_sub(header.start) as usize).min(header.filled.unverified().load(Ordering::Relaxed));
		if end <= taken {
			return;
		}
		// The pending drop owns the segment too, so a retirement before it runs can't free
		// the slots under it. Relaxed: the writer still owns it.
		header.owners.fetch_add(1, Ordering::Relaxed);
		header.taken.store(end, Ordering::Release);
		let guard = self.shared.pin();
		let entries = Taken(segment, taken..end);
		// SAFETY: below the base, which is stored first, so only a read pinned before this
		// can still reach them, and the collector waits those out. The segment's free
		// skips them, and the ownership taken above keeps it allocated. `T: 'static`.
		unsafe {
			guard.defer_unchecked(move || {
				let Taken(segment, range) = entries;
				Segment::drop_entries(segment, range);
				Segment::release(segment);
			});
		}
		// Hand the garbage to the collector now rather than when this thread's bag fills.
		guard.flush();
	}

	/// Hand segments no reader can reach any more to the epoch collector.
	#[cfg_attr(verus_keep_ghost, verifier::external_body)]
	fn retire(&self, segments: SmallVec<[NonNull<Segment<T>>; 1]>) {
		let guard = self.shared.pin();
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
}

/// Reads a log's committed entries by index, without a lock.
///
/// Holds nothing between reads: [`Self::read`] pins the log for as long as its
/// [`Read`] lives.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub struct Reader<T> {
	shared: Arc<Shared<T>>,
	/// The segment of the last entry read, and its start. Only a hint: it may have been
	/// freed since, so a read trusts it only once the base shows it wasn't retired.
	hint: Option<(NonNull<Segment<T>>, u64)>,
	#[cfg(verus_keep_ghost_body)]
	reached: Tracked<Option<Reached<T>>>,
}

/// A [`Reader`] pinned for a run of reads, from [`Reader::read`].
///
/// No segment it can reach is freed while it lives, so hold it only for the reads and
/// drop it before parking.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub struct Read<'a, T> {
	reader: &'a mut Reader<T>,
	_guard: ReadGuard,
}

/// The pin behind a [`Read`], released only after the read's accesses.
///
/// A wrapper rather than a `Drop` on `Read` itself, which would hold the `Reader` borrow
/// until the end of the scope instead of the last use.
#[cfg_attr(verus_keep_ghost, verus_verify)]
struct ReadGuard {
	_pin: epoch::Guard,
}

#[cfg(verus_keep_ghost)]
verus! {

impl<T> Reader<T> {
	/// The reader's hint, if any, names a segment the writer published.
	pub closed spec fn wf(&self) -> bool {
		&&& self.shared.wf()
		&&& match self.hint {
			None => true,
			Some((segment, start)) => self.reached@ matches Some(reached) && {
				&&& reached.wf(self.shared.key(), segment)
				&&& reached.header@.value().start == start
			},
		}
	}
}

/// A segment whose written slots hold the entry a read wants, and that entry's slot.
tracked struct Found<T> {
	reached: Reached<T>,
	slot: Frozen<PointsTo<T>>,
}

impl<T> Found<T> {
	spec fn wf(&self, key: Key, segment: NonNull<Segment<T>>, index: u64) -> bool {
		let header = self.reached.header@.value();
		&&& self.reached.wf(key, segment)
		&&& header.start <= index < header.start + header.capacity
		&&& self.slot@.ptr() == slot_ptr(segment.as_ptr(), index - header.start)
		&&& self.slot@.is_init()
	}
}

impl<T> Read<'_, T> {
	/// The pinned reader is well formed.
	pub closed spec fn wf(&self) -> bool {
		self.reader.wf()
	}
}

} // verus!

#[cfg_attr(verus_keep_ghost, verus_verify)]
impl<T> Reader<T> {
	/// Pin the log for a run of reads. Entries read through the returned handle stay
	/// alive until it drops; nothing does once it has.
	#[expect(clippy::collapsible_if, reason = "Verus does not parse let chains")]
	#[cfg_attr(verus_keep_ghost, verus_spec(read =>
		requires
			old(self).wf(),
		ensures
			read.wf(),
	))]
	pub fn read(&mut self) -> Read<'_, T> {
		let guard = self.shared.pin();
		let base = self.shared.base.load(Ordering::Acquire);
		// A segment is retired only once the base passes its last entry, so one whose
		// start the base hasn't reached was not retired before this pin.
		if let Some((_, start)) = self.hint {
			if start < base {
				self.hint = None;
			}
		}
		Read {
			reader: self,
			_guard: ReadGuard { _pin: guard },
		}
	}
}

#[cfg_attr(verus_keep_ghost, verus_verify)]
impl<T> Read<'_, T> {
	/// The committed entry at `index`, or `None` when nothing is committed there yet.
	///
	/// Reads at increasing indices stay in the segment last read or follow its link to
	/// the next. Anything else, such as a reader's first read, starts from the log's
	/// oldest segment.
	#[expect(clippy::collapsible_if, reason = "Verus does not parse let chains")]
	#[cfg_attr(verus_keep_ghost, verus_spec(entry =>
		requires
			old(self).wf(),
		ensures
			final(self).wf(),
	))]
	pub fn get(&mut self, index: u64) -> Result<Option<&T>, Evicted> {
		let shared = &*self.reader.shared;
		if index < shared.base.load(Ordering::Acquire) {
			return Err(Evicted);
		}
		// The common read: the segment last read already holds `index`.
		if let Some((segment, start)) = self.reader.hint {
			if start <= index {
				proof_decl! {
					let tracked reached = self.reader.reached.borrow().tracked_borrow();
				}
				// SAFETY: this read's pin holds the hinted segment, per `Reader::read`'s check.
				let header = unsafe {
					proof_with!(Tracked(&reached.header));
					Segment::header(segment)
				};
				let filled;
				proof_decl! {
					let tracked mut slot = None;
				}
				open_atomic_invariant!(shared.ledger.borrow() => ledger => {
					proof! { reached.find(&ledger, shared.key(), segment.as_ptr()); }
					// Acquire pairs with the writer's Release of `filled`.
					#[cfg_attr(verus_keep_ghost, verus_spec(with Tracked(ledger.filled.tracked_borrow(reached.id))))]
					let loaded = header.filled.load(Ordering::Acquire);
					filled = loaded;
					proof! {
						assert(filled <= header.capacity);
						if index < start + filled {
							let at = (index - start) as nat;
							assert(ledger.slots.contains_key((reached.id, at)));
							slot = Some(ledger.slots.tracked_borrow((reached.id, at)).clone());
						}
					}
				});
				if index < start + filled as u64 {
					proof_decl! {
						let tracked slot = slot.tracked_unwrap();
					}
					// `filled` covers `index`, so its slot was written.
					// SAFETY: as above.
					let entry = unsafe {
						proof_with!(Tracked(&reached.header), Tracked(&slot));
						Segment::get(segment, index)
					};
					return Ok(Some(entry));
				}
			}
		}
		// Acquire pairs with the writer's Release: every slot below `end` is written.
		let end;
		open_atomic_invariant!(shared.ledger.borrow() => ledger => {
			#[cfg_attr(verus_keep_ghost, verus_spec(with Tracked(&ledger.end)))]
			let loaded = shared.end.load(Ordering::Acquire);
			end = loaded;
		});
		if index >= end {
			return Ok(None);
		}

		// The hint's chain can end short of `index` when the writer emptied the log and
		// started a fresh segment after this pin's check, so fall back to the head.
		let mut hinted = None;
		proof_decl! {
			let tracked mut found: Option<Found<T>> = None;
		}
		if let Some((segment, start)) = self.reader.hint {
			if start <= index {
				proof_decl! {
					let tracked reached = self.reader.reached.borrow().tracked_borrow();
				}
				proof_with!(Tracked(shared.ledger.borrow()), Tracked(reached) => Tracked(walked));
				let stop = Self::walk(segment, index);
				hinted = stop;
				proof! { found = walked; }
			}
		}
		let segment = match hinted {
			Some(segment) => segment,
			None => {
				// Loaded under the pin: the writer moves the head before retiring what it
				// passes.
				let head;
				proof_decl! {
					let tracked mut first = None;
				}
				open_atomic_invariant!(shared.ledger.borrow() => ledger => {
					#[cfg_attr(verus_keep_ghost, verus_spec(with Tracked(&ledger.head)))]
					let loaded = shared.head.load(Ordering::Acquire);
					head = loaded;
					proof! {
						if let Some(id) = ledger.view@.head {
							first = Some(Reached::of(&ledger, shared.key(), id));
						}
					}
				});
				let Some(head) = NonNull::new(head) else {
					// Evicted since the base was checked.
					return Err(Evicted);
				};
				proof_decl! {
					let tracked first = first.tracked_unwrap();
				}
				proof_with!(Tracked(shared.ledger.borrow()), Tracked(&first) => Tracked(walked));
				let stop = Self::walk(head, index);
				proof! { found = walked; }
				match stop {
					Some(segment) => segment,
					// Evicted since the base was checked.
					None => return Err(Evicted),
				}
			}
		};
		proof_decl! {
			let tracked Found { reached, slot } = found.tracked_unwrap();
		}
		// SAFETY: found under this read's pin, which holds it.
		let header = unsafe {
			proof_with!(Tracked(&reached.header));
			Segment::header(segment)
		};
		// Only as good as this pin's, which `Reader::read` checks before trusting it.
		self.reader.hint = Some((segment, header.start));
		proof! {
			self.reader.reached = Tracked(Some(reached.copy()));
		}
		// `filled` covers `index`, so its slot was written.
		// SAFETY: as above.
		let entry = unsafe {
			proof_with!(Tracked(&reached.header), Tracked(&slot));
			Segment::get(segment, index)
		};
		Ok(Some(entry))
	}

	/// Follow links from `segment` to the one whose written slots hold `index`, if any.
	#[cfg_attr(verus_keep_ghost, verifier::exec_allows_no_decreases_clause)]
	#[cfg_attr(verus_keep_ghost, verus_spec(hit =>
		with
			Tracked(ledger): Tracked<&AtomicInvariant<Key, Ledger<T>, LedgerPred>>,
			Tracked(reached): Tracked<&Reached<T>>
			-> found: Tracked<Option<Found<T>>>
		requires
			ledger.namespace() == LEDGER,
			reached.wf(ledger.constant(), segment),
		ensures
			hit matches Some(stop) ==> found@ matches Some(f) && f.wf(ledger.constant(), stop, index),
	))]
	fn walk(segment: NonNull<Segment<T>>, index: u64) -> Option<NonNull<Segment<T>>> {
		let mut segment = segment;
		let mut hit = None;
		proof_decl! {
			let tracked mut reached = reached.copy();
			let tracked mut found: Option<Found<T>> = None;
		}
		#[cfg_attr(verus_keep_ghost, verus_spec(
			invariant
				ledger.namespace() == LEDGER,
				reached.wf(ledger.constant(), segment),
			invariant_except_break
				hit is None,
			ensures
				hit matches Some(stop) ==> found matches Some(f) && f.wf(ledger.constant(), stop, index),
		))]
		loop {
			// SAFETY: linked from a segment the caller reached under its pin, which holds it.
			let header = unsafe {
				proof_with!(Tracked(&reached.header));
				Segment::header(segment)
			};
			if index < header.start {
				break;
			}
			let filled;
			proof_decl! {
				let tracked mut slot = None;
			}
			open_atomic_invariant!(ledger => inner => {
				proof! { reached.find(&inner, ledger.constant(), segment.as_ptr()); }
				// Acquire pairs with the writer's Release of `filled`.
				#[cfg_attr(verus_keep_ghost, verus_spec(with Tracked(inner.filled.tracked_borrow(reached.id))))]
				let loaded = header.filled.load(Ordering::Acquire);
				filled = loaded;
				proof! {
					assert(filled <= header.capacity);
					if index < header.start + filled {
						let at = (index - header.start) as nat;
						assert(inner.slots.contains_key((reached.id, at)));
						slot = Some(inner.slots.tracked_borrow((reached.id, at)).clone());
					}
				}
			});
			if index < header.start + filled as u64 {
				proof! {
					found = Some(Found { reached: reached.copy(), slot: slot.tracked_unwrap() });
				}
				hit = Some(segment);
				break;
			}
			// A segment links its successor before anything is written into it, so a
			// segment without a link is the tail, or one evicted early with everything.
			let next;
			proof_decl! {
				let tracked mut successor = None;
			}
			open_atomic_invariant!(ledger => inner => {
				proof! { reached.find(&inner, ledger.constant(), segment.as_ptr()); }
				#[cfg_attr(verus_keep_ghost, verus_spec(with Tracked(inner.next.tracked_borrow(reached.id))))]
				let loaded = header.next.load(Ordering::Acquire);
				next = loaded;
				proof! {
					if let Some(id) = inner.view@.segments[reached.id].next {
						successor = Some(Reached::of(&inner, ledger.constant(), id));
					}
				}
			});
			match NonNull::new(next) {
				Some(next) => segment = next,
				None => break,
			}
			proof! {
				reached = successor.tracked_unwrap();
			}
		}
		proof_with!(|= Tracked(found));
		hit
	}
}

impl<T> Segment<T> {
	/// Heap a segment of `capacity` slots occupies: its header and slots.
	const fn heap(capacity: usize) -> usize {
		(offset_of!(Self, slots) + capacity * size_of::<Slot<T>>()).next_multiple_of(align_of::<Self>())
	}

	fn layout(capacity: usize) -> Layout {
		Layout::from_size_align(Self::heap(capacity), align_of::<Self>()).expect("a segment fits in memory")
	}

	/// The written entry at `index`, for the writer, which is outside the proof.
	///
	/// # Safety
	///
	/// `segment` stays live for `'a`, and the write that filled `index` happened before
	/// this.
	unsafe fn get_unverified<'a>(segment: NonNull<Self>, index: u64) -> &'a T {
		// SAFETY: per the caller, the slot is in bounds, written, and never written again.
		unsafe {
			let at = (index - segment.as_ref().start) as usize;
			&*Self::slot(segment, at)
		}
	}

	/// Drop the written entries in `range`, in place.
	///
	/// # Safety
	///
	/// `segment` is live, every slot in `range` is written, no reader can reach them any
	/// more, and this is their only drop.
	unsafe fn drop_entries(segment: NonNull<Self>, range: std::ops::Range<usize>) {
		for at in range {
			// SAFETY: written and unreachable, per the caller.
			unsafe { Self::slot(segment, at).drop_in_place() };
		}
	}

	/// Drop the entries eviction didn't already hand over, and give up the writer's
	/// ownership.
	///
	/// # Safety
	///
	/// No reader can reach `segment` any more, and this is its only retirement.
	unsafe fn free(segment: NonNull<Self>) {
		// SAFETY: live until the release below, and the writer stored both counts before
		// retiring it.
		unsafe {
			let header = segment.as_ref();
			let (taken, filled) = (
				header.taken.load(Ordering::Acquire),
				header.filled.unverified().load(Ordering::Acquire),
			);
			Self::drop_entries(segment, taken..filled);
			Self::release(segment);
		}
	}

	/// Give up one ownership, freeing the segment if it was the last. Its entries were
	/// all dropped already.
	///
	/// # Safety
	///
	/// The caller owns `segment` and touches it no more.
	unsafe fn release(segment: NonNull<Self>) {
		// SAFETY: live while the caller owns it.
		let header = unsafe { segment.as_ref() };
		// Release so the last owner's free comes after every other owner's drops.
		if header.owners.fetch_sub(1, Ordering::Release) != 1 {
			return;
		}
		// An Acquire load, as std's `Arc` does, rather than `fence(Acquire)`: the same ordering,
		// but ThreadSanitizer models it and not fences.
		header.owners.load(Ordering::Acquire);
		// The header holds only atomics and the slots `MaybeUninit`s whose entries are
		// already dropped, so nothing needs dropping before the free.
		let capacity = header.capacity;
		// SAFETY: the last owner, so nothing else uses the segment.
		unsafe { alloc::dealloc(segment.as_ptr().cast(), Self::layout(capacity)) };
	}
}

// A panic can't leave a slot half-written: `push` writes it whole, then publishes it.
impl<T: RefUnwindSafe> RefUnwindSafe for Segment<T> {}
impl<T: RefUnwindSafe> UnwindSafe for Segment<T> {}

/// A retired segment on its way to the epoch collector.
struct Retired<T>(NonNull<Segment<T>>);

/// Entries evicted from a live segment, on their way to the epoch collector, which
/// drops them in place and then gives up their hold on the segment.
struct Taken<T>(NonNull<Segment<T>>, std::ops::Range<usize>);

// SAFETY: whichever thread runs the collector drops the entries, which `T: Send` allows.
unsafe impl<T: Send> Send for Taken<T> {}

// SAFETY: whichever thread frees the segment drops its entries, which `T: Send` allows.
unsafe impl<T: Send> Send for Retired<T> {}

// SAFETY: the writer owns its segments, which hold `T`s that readers on other threads
// borrow and that the collector may drop on another thread.
unsafe impl<T: Send + Sync + 'static> Send for Writer<T> {}
// SAFETY: `&Writer` only reads committed entries, which only `&mut Writer` retires.
unsafe impl<T: Send + Sync + 'static> Sync for Writer<T> {}

impl<T: Send + Sync + 'static> Writer<T> {
	/// Bytes the shared state occupies.
	const SHARED: usize = shared_heap::<T>();

	/// Bytes a log occupies once its first entry is appended: the shared state and the
	/// first segment.
	pub const HEAP: usize = Self::SHARED + Segment::<T>::heap(FIRST_SEGMENT);

	/// Bytes the largest segment occupies: the most one eviction frees at once.
	pub const SEGMENT_HEAP: usize = Segment::<T>::heap(MAX_SEGMENT);

	/// Bytes this log occupies: its shared state and the segments it holds, which grow in
	/// steps as entries arrive.
	pub fn heap(&self) -> usize {
		let list = match self.segments.spilled() {
			true => self.segments.capacity() * size_of::<NonNull<Segment<T>>>(),
			false => 0,
		};
		self.heap + list
	}

	/// The committed entry at `index`, unless it was evicted.
	pub fn get(&self, index: u64) -> Option<&T> {
		if index < self.base() || index >= self.end() {
			return None;
		}
		// SAFETY: the writer's segments are live.
		let at = self.segments.partition_point(|segment| {
			let segment = unsafe { segment.as_ref() };
			segment.start + segment.filled.unverified().load(Ordering::Relaxed) as u64 <= index
		});
		let segment = *self.segments.get(at)?;
		// SAFETY: live while `&self` is, since only `&mut self` retires it, and this
		// writer wrote `index`: it is below the end, at or past the base.
		(unsafe { segment.as_ref() }.start <= index).then(|| unsafe { Segment::get_unverified(segment, index) })
	}
}

impl<T: Send + Sync + 'static> Drop for Writer<T> {
	/// Evict every entry, so readers that outlive the writer hold nothing.
	fn drop(&mut self) {
		// Through the verified eviction, so the head moves with the proof's ledger.
		self.evict(self.end());
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

// SAFETY: the hint is dereferenced only under a pin that keeps its segment alive, and
// readers only share `&T`.
unsafe impl<T: Send + Sync> Send for Reader<T> {}
// SAFETY: as above; reads take `&mut self`.
unsafe impl<T: Send + Sync> Sync for Reader<T> {}

impl<T> Clone for Reader<T> {
	fn clone(&self) -> Self {
		Self {
			shared: self.shared.clone(),
			hint: self.hint,
			// TRUSTED: outside the proof; a clone holds the same segment, so the same ghost
			// state.
			#[cfg(verus_keep_ghost_body)]
			reached: Tracked::assume_new(),
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

#[cfg(test)]
mod test {
	use std::sync::{Arc as StdArc, Mutex};

	use super::*;

	/// Reclamation assertions use their own domain, so unrelated log reads cannot pin it.
	fn isolated<T: Send + Sync + 'static>(start: u64) -> (Writer<T>, epoch::Collector) {
		let collector = epoch::Collector::new();
		let mut writer = Writer::new(start);
		Arc::get_mut(&mut writer.shared).unwrap().collector = Some(collector.clone());
		(writer, collector)
	}

	/// The `[start, end)` of each live segment, oldest first.
	fn layout<T: Send + Sync + 'static>(writer: &Writer<T>) -> Vec<(u64, u64)> {
		writer
			.segments
			.iter()
			.map(|segment| unsafe { segment.as_ref() })
			.map(|segment| (segment.start, segment.start + segment.capacity as u64))
			.collect()
	}

	/// Advance the local domain beyond the retirement epoch and drain its garbage.
	fn collect(collector: &epoch::Collector) {
		let handle = collector.register();
		for _ in 0..8 {
			handle.pin().flush();
		}
	}

	fn get<T: Clone>(reader: &mut Reader<T>, index: u64) -> Result<Option<T>, Evicted> {
		Ok(reader.read().get(index)?.cloned())
	}

	/// Every committed entry reads back at its index, across segment boundaries, from a
	/// reader that started before any of them existed or one that jumps.
	#[test]
	fn reads_every_entry_across_segments() {
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
		let header = Layout::new::<Segment<StdArc<()>>>();
		let slots = Layout::array::<Slot<StdArc<()>>>(4).unwrap();
		let layout = header.extend(slots).unwrap().0.pad_to_align();
		assert_eq!(Segment::<StdArc<()>>::layout(4), layout);
	}

	/// A log can start past zero; nothing below its start is readable.
	#[test]
	fn starts_past_zero() {
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
		let entry = StdArc::new(());
		let (mut writer, collector) = isolated(0);
		let mut reader = writer.reader();
		for _ in 0..3 {
			writer.push(entry.clone());
		}
		assert!(get(&mut reader, 1).unwrap().is_some());
		writer.evict(3);
		collect(&collector);
		assert_eq!(StdArc::strong_count(&entry), 1, "freed while the reader idles");
		assert_eq!(get(&mut reader, 1), Err(Evicted));
		assert_eq!(get(&mut reader, 3), Ok(None));
	}

	/// A read in progress keeps what it can reach until it ends.
	#[test]
	fn a_read_outlasts_eviction() {
		let entry = StdArc::new(());
		let (mut writer, collector) = isolated(0);
		let mut reader = writer.reader();
		writer.push(entry.clone());
		let mut read = reader.read();
		let held = read.get(0).unwrap().unwrap();
		writer.evict(1);
		collect(&collector);
		assert_eq!(StdArc::strong_count(held), 2, "pinned by the read");
		drop(read);
		collect(&collector);
		assert_eq!(StdArc::strong_count(&entry), 1);
	}

	/// A read that spans the writer emptying the log and appending again finds the new
	/// entry, though the segment it last read was cut off from the new one.
	#[test]
	fn a_read_spans_a_full_eviction() {
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

	/// Evicting part of a segment drops those entries once no read can reach them, not
	/// when the whole segment retires, and never drops one twice.
	#[test]
	fn evicting_inside_a_segment_drops_its_entries() {
		let (mut writer, collector) = isolated(0);
		let entries: Vec<_> = (0..6).map(Arc::new).collect();
		for entry in &entries {
			writer.push(entry.clone());
		}
		// Segments of 1, 2, and 4 slots: the last holds entries 3 to 5 of its 4.
		writer.evict(4);
		writer.evict(5);
		collect(&collector);
		for (index, entry) in entries.iter().enumerate() {
			let held = usize::from(index >= 5);
			assert_eq!(Arc::strong_count(entry), 1 + held, "entry {index}");
		}
		drop(writer);
		collect(&collector);
		assert!(entries.iter().all(|entry| Arc::strong_count(entry) == 1));
	}

	/// A read that still holds an evicted entry may mutate it through interior
	/// mutability, and the entry dropped later is the one it mutated, not a stale copy.
	#[test]
	fn an_evicted_entry_a_read_still_holds_drops_as_mutated() {
		let (mut writer, collector) = isolated(0);
		let held = StdArc::new(());
		for _ in 0..3 {
			writer.push(Mutex::new(vec![held.clone()]));
		}
		let mut reader = writer.reader();
		let mut read = reader.read();
		// Segments of 1 and 2 slots: entry 1 shares the second with entry 2.
		let entry = read.get(1).unwrap().unwrap();
		writer.evict(2);
		let taken = std::mem::take(&mut *entry.lock().unwrap());
		drop(read);
		drop(taken);
		collect(&collector);
		assert_eq!(StdArc::strong_count(&held), 2, "only entry 2 still holds it");
		drop(writer);
		collect(&collector);
		assert_eq!(StdArc::strong_count(&held), 1);
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
		assert_eq!(get(&mut reader, 99), Err(Evicted));
		assert_eq!(get(&mut reader, 100), Ok(Some("x")));
	}

	/// Every index below `u64::MAX` takes an entry.
	#[test]
	fn the_last_index_is_usable() {
		let mut writer = Writer::new(u64::MAX - 2);
		let mut reader = writer.reader();
		writer.push("a");
		writer.push("b");
		assert_eq!(writer.end(), u64::MAX);
		assert_eq!(get(&mut reader, u64::MAX - 1), Ok(Some("b")));
	}

	/// No index is left once the end reaches `u64::MAX`.
	#[test]
	#[should_panic(expected = "log index overflow")]
	fn pushing_past_the_last_index_panics() {
		let mut writer = Writer::new(u64::MAX);
		writer.push("a");
	}

	/// Evicting to the end, as dropping the writer does, frees a segment reserved but
	/// never written.
	#[test]
	fn evicting_to_the_end_frees_a_reservation() {
		let mut writer = Writer::<&str>::new(0);
		writer.reserve();
		writer.evict(0);
		assert_eq!(
			writer.heap(),
			Writer::<&str>::SHARED,
			"the reservation went with the eviction"
		);
	}

	/// A clone reads independently from where the original's segment was.
	#[test]
	fn clones_read_independently() {
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
		let _unrelated = epoch::pin();
		let entry = StdArc::new(());
		let (mut writer, collector) = isolated(0);
		let mut reader = writer.reader();
		for _ in 0..20 {
			writer.push(entry.clone());
		}
		assert!(get(&mut reader, 10).unwrap().is_some());
		drop(writer);
		collect(&collector);
		assert_eq!(StdArc::strong_count(&entry), 1);
		assert_eq!(get(&mut reader, 10), Err(Evicted));
		assert_eq!(get(&mut reader, 20), Ok(None));
	}

	/// Readers on other threads see every entry the writer published, in order, while
	/// it evicts behind them.
	#[test]
	fn concurrent_readers_see_every_entry() {
		// Miri takes minutes over the full run; a tenth still evicts under the readers.
		const ENTRIES: u64 = if cfg!(miri) { 1_000 } else { 10_000 };
		let mut writer = Writer::new(0);
		let readers: Vec<_> = (0..4)
			.map(|_| {
				let mut reader = writer.reader();
				std::thread::spawn(move || {
					let mut index = 0;
					let mut evicted = 0;
					while index < ENTRIES {
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
		for i in 0..ENTRIES {
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
