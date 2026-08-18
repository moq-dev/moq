//! A set of poll-driven tasks with per-task wakeups.
//!
//! The poll-native `FuturesUnordered`: the owner polls the set, each task owns
//! a waker that marks it ready and wakes the owner, so a wakeup re-polls only
//! the task it was aimed at (plus any newly pushed ones). A driver serving
//! hundreds of children steps one machine per event, not all of them.
//!
//! A task is a plain poll closure, the same `FnMut(&Waiter) -> Poll<()>` shape
//! [`wait`](crate::wait) takes: no trait to implement, state lives in the
//! captures. `Send` is therefore purely inferred: a [`Tasks`] of `Send`
//! closures is `Send` and drives from any thread, one capturing an `Rc` is not
//! and drives locally, and the same code compiles either way. One set holds one
//! closure type, which one construction site provides naturally; a set that
//! must mix shapes boxes them (`Box<dyn FnMut(&Waiter) -> Poll<()>>`, adding
//! `+ Send` only if it crosses threads), and a future adapts with one line:
//!
//! ```
//! let mut tasks = kio::Tasks::new();
//! let mut fut = Box::pin(async { /* .. */ });
//! tasks.push(move |waiter: &kio::Waiter| waiter.poll_future(fut.as_mut()));
//! ```
//!
//! Dropping the set cancels every remaining task.
//!
//! # Design
//!
//! Wakes land in a chunked atomic bitset rather than a queue (the approach
//! [unicycle](https://github.com/udoprog/unicycle) proved out against
//! `FuturesUnordered`'s intrusive list): a wake is one `fetch_or`, lock-free,
//! and inherently deduplicated since setting a set bit is a no-op. Each poll
//! snapshots a word at a time with `swap(0)`, so a task woken mid-pass lands in
//! the next pass instead of spinning this one. Because tasks are closures, not
//! pinned futures, a slot's identity is stable and its [`Waker`] is minted once
//! and reused across occupants: the per-task allocation `FuturesUnordered`
//! pays per future (and futures-buffered removes with an unsafe arena) is a
//! per-slot cost here, amortized to zero in a long-lived set. Safe Rust
//! throughout.

use std::{
	sync::{
		Arc, Mutex,
		atomic::{AtomicBool, AtomicU64, Ordering},
	},
	task::{Context, Poll, Wake, Waker},
};

use crate::waiter::{Park, Waiter};

/// Wake-bits per chunk word and words per chunk: one chunk covers 1024 slots.
const WORD_BITS: usize = 64;
const CHUNK_WORDS: usize = 16;
const CHUNK_SLOTS: usize = WORD_BITS * CHUNK_WORDS;

/// One fixed block of the wake bitset. Chunks never move or grow, so a waker
/// can hold its chunk directly and set its bit without any lock.
struct Chunk([AtomicU64; CHUNK_WORDS]);

impl Chunk {
	fn new() -> Arc<Self> {
		Arc::new(Self(std::array::from_fn(|_| AtomicU64::new(0))))
	}
}

/// A set of poll-closure tasks polled by their owner with per-task granularity.
///
/// [`poll`](Self::poll) drives the tasks that are new or woken and reports
/// `Ready` when the set is empty; [`push`](Self::push) wakes the owner, so a
/// task added between polls gets started. Dropping the set cancels every
/// remaining task.
pub struct Tasks<T> {
	/// Slab of tasks; `None` slots are free and listed in `free`.
	children: Vec<Option<Occupant<T>>>,
	/// Per-slot wakers, parallel to `children`. A slot's bit position never
	/// changes, so its waker is created once and reused by every occupant.
	wakers: Vec<Waker>,
	/// The wake bitset, chunked so it grows without moving: slot `i` is bit
	/// `i % 64` of word `(i % 1024) / 64` in chunk `i / 1024`.
	chunks: Vec<Arc<Chunk>>,
	free: Vec<usize>,
	len: usize,
	shared: Arc<Shared>,
	/// The readiness snapshot each poll dispatches from, taken before any task
	/// runs so a mid-pass wake lands in the next pass wherever its slot sits.
	/// Retained so steady-state polls don't allocate.
	scratch: Vec<u64>,
}

/// A slot's current task and its retained kio registrations.
struct Occupant<T> {
	task: T,
	park: Park,
}

/// Waker state shared with every slot; deliberately `T`-free so the wakers
/// stay `Send + Sync` even when the tasks themselves are not.
struct Shared {
	/// The waker of whoever polls the set, re-registered on every poll.
	parent: Mutex<Option<Waker>>,
	/// Set by the first wake since the last poll began, so a burst of wakes
	/// costs one parent wake, not one each.
	pending: AtomicBool,
}

impl Shared {
	fn wake_parent(&self) {
		if self.pending.swap(true, Ordering::AcqRel) {
			return;
		}
		let parent = self.parent.lock().unwrap().clone();
		if let Some(waker) = parent {
			waker.wake();
		}
	}
}

/// The per-slot waker: sets the slot's wake bit and nudges the owner.
struct SlotWaker {
	chunk: Arc<Chunk>,
	word: usize,
	bit: u64,
	shared: Arc<Shared>,
}

impl Wake for SlotWaker {
	fn wake(self: Arc<Self>) {
		self.wake_by_ref();
	}

	fn wake_by_ref(self: &Arc<Self>) {
		// An already-set bit means the slot is queued for the next pass and the
		// owner was already nudged: a burst of wakes is one bit and one nudge.
		if self.chunk.0[self.word].fetch_or(self.bit, Ordering::AcqRel) & self.bit == 0 {
			self.shared.wake_parent();
		}
	}
}

impl<T> Tasks<T> {
	/// Create an empty set.
	pub fn new() -> Self {
		Self {
			children: Vec::new(),
			wakers: Vec::new(),
			chunks: Vec::new(),
			free: Vec::new(),
			len: 0,
			shared: Arc::new(Shared {
				parent: Mutex::new(None),
				pending: AtomicBool::new(false),
			}),
			scratch: Vec::new(),
		}
	}

	/// The number of tasks still running.
	pub fn len(&self) -> usize {
		self.len
	}

	/// Whether no tasks are running.
	pub fn is_empty(&self) -> bool {
		self.len == 0
	}

	/// Add a task, queued for its first poll.
	///
	/// Wakes the owner, so a task pushed between polls (from another arm of the
	/// owner's own poll, say) still gets started.
	pub fn push(&mut self, task: T) {
		let index = match self.free.pop() {
			Some(index) => index,
			None => {
				let index = self.children.len();
				if index.is_multiple_of(CHUNK_SLOTS) {
					self.chunks.push(Chunk::new());
				}
				self.children.push(None);
				self.wakers.push(Waker::from(Arc::new(SlotWaker {
					chunk: self.chunks[index / CHUNK_SLOTS].clone(),
					word: (index % CHUNK_SLOTS) / WORD_BITS,
					bit: 1 << (index % WORD_BITS),
					shared: self.shared.clone(),
				})));
				index
			}
		};
		self.children[index] = Some(Occupant {
			task,
			park: Park::default(),
		});
		self.len += 1;
		// Queue the first poll exactly the way any wake would.
		self.wakers[index].wake_by_ref();
	}
}

impl<T: FnMut(&Waiter) -> Poll<()>> Tasks<T> {
	/// Poll every task that is new or was woken since the last call, retiring
	/// the ones that return `Ready`.
	///
	/// `Ready` when the set is empty, like `FuturesUnordered` reporting `None`.
	/// A long-lived driver composes at the poll level: call this as one arm of
	/// its own poll function and treat `Ready` as "drained for now" (the parent
	/// waker is registered even then, so a later [`push`](Self::push) wakes the
	/// owner). Do NOT wrap an empty set in [`wait`](crate::wait) inside a select
	/// loop: the future completes immediately and the loop spins, the same
	/// footgun as selecting on an empty `FuturesUnordered`'s `next()`; guard
	/// such an arm with [`is_empty`](Self::is_empty).
	/// `waiter` is registered for the next task wake or push; the tasks
	/// themselves park on their own wakers.
	pub fn poll(&mut self, waiter: &Waiter) -> Poll<()> {
		{
			let mut parent = self.shared.parent.lock().unwrap();
			let waker = waiter.waker();
			if !parent.as_ref().is_some_and(|w| w.will_wake(waker)) {
				*parent = Some(waker.clone());
			}
		}
		// Re-arm before sweeping: a wake landing mid-pass must nudge the owner
		// again, since its word will already have been snapshotted.
		self.shared.pending.store(false, Ordering::Release);

		// One snapshot per owner poll, taken in full before any task runs:
		// `swap(0)` takes the woken bits and clears them, so a task woken
		// during the pass (by another task or a self-wake) lands in the next
		// pass wherever its slot sits, never this one. Dispatching straight off
		// the words instead would run a forward wake chain in a single pass,
		// starving the caller's other arms.
		self.scratch.clear();
		for chunk in &self.chunks {
			for word in &chunk.0 {
				self.scratch.push(word.swap(0, Ordering::AcqRel));
			}
		}

		for w in 0..self.scratch.len() {
			let mut bits = self.scratch[w];
			while bits != 0 {
				let bit = bits.trailing_zeros() as usize;
				bits &= bits - 1;
				let index = w * WORD_BITS + bit;
				// A stale bit (the occupant finished; maybe the slot was
				// reused) is skipped or costs one spurious poll; both are
				// harmless.
				let Some(occupant) = self.children.get_mut(index).and_then(Option::as_mut) else {
					continue;
				};
				let cx = Context::from_waker(&self.wakers[index]);
				let child_waiter = occupant.park.hold(&cx);
				if (occupant.task)(child_waiter).is_ready() {
					self.children[index] = None;
					self.free.push(index);
					self.len -= 1;
				}
			}
		}

		match self.len {
			0 => Poll::Ready(()),
			_ => Poll::Pending,
		}
	}
}

impl<T> Default for Tasks<T> {
	fn default() -> Self {
		Self::new()
	}
}

#[cfg(all(test, not(loom)))]
mod tests {
	use super::*;
	use std::sync::atomic::AtomicUsize;

	/// A task counting its polls, finishing when its gate opens. A factory
	/// returning `impl FnMut` mints one closure type, so every task from it
	/// shares a set without boxing.
	fn gated(gate: crate::Consumer<bool>, polls: Arc<AtomicUsize>) -> impl FnMut(&Waiter) -> Poll<()> {
		move |waiter| {
			polls.fetch_add(1, Ordering::SeqCst);
			match gate.poll(waiter, |open| match **open {
				true => Poll::Ready(()),
				false => Poll::Pending,
			}) {
				Poll::Ready(_) => Poll::Ready(()),
				Poll::Pending => Poll::Pending,
			}
		}
	}

	fn open(gate: &crate::Producer<bool>) {
		let Ok(mut state) = gate.write() else {
			panic!("gate closed")
		};
		*state = true;
	}

	/// A wake aimed at one task polls only that task.
	#[test]
	fn a_wake_polls_only_its_task() {
		let mut tasks = Tasks::new();
		let (a_gate, a_polls) = (crate::Producer::new(false), Arc::new(AtomicUsize::new(0)));
		tasks.push(gated(a_gate.consume(), a_polls.clone()));
		let (_b_gate, b_polls) = (crate::Producer::new(false), Arc::new(AtomicUsize::new(0)));
		tasks.push(gated(_b_gate.consume(), b_polls.clone()));

		let waiter = Waiter::noop();
		assert!(tasks.poll(&waiter).is_pending());
		assert_eq!((a_polls.load(Ordering::SeqCst), b_polls.load(Ordering::SeqCst)), (1, 1));

		// Waking a's gate must re-poll a alone.
		open(&a_gate);
		assert!(tasks.poll(&waiter).is_pending(), "b is still live");
		assert_eq!(a_polls.load(Ordering::SeqCst), 2, "a was woken");
		assert_eq!(b_polls.load(Ordering::SeqCst), 1, "b was not");
	}

	/// Finished tasks retire, their slots are reused, and an empty set is Ready.
	#[test]
	fn finished_tasks_retire_and_slots_recycle() {
		let mut tasks = Tasks::new();
		let a_gate = crate::Producer::new(false);
		tasks.push(gated(a_gate.consume(), Arc::new(AtomicUsize::new(0))));
		let b_gate = crate::Producer::new(false);
		tasks.push(gated(b_gate.consume(), Arc::new(AtomicUsize::new(0))));
		assert_eq!(tasks.len(), 2);

		let waiter = Waiter::noop();
		open(&a_gate);
		assert!(tasks.poll(&waiter).is_pending());
		assert_eq!(tasks.len(), 1);

		// The freed slot is reused without growing the slab.
		let c_gate = crate::Producer::new(false);
		tasks.push(gated(c_gate.consume(), Arc::new(AtomicUsize::new(0))));
		assert_eq!(tasks.children.len(), 2);

		open(&b_gate);
		open(&c_gate);
		assert!(tasks.poll(&waiter).is_ready(), "an empty set is Ready");
		assert!(tasks.is_empty());
	}

	/// A boxed set mixes shapes: a plain closure and an adapted future.
	#[test]
	fn boxed_tasks_mix_closures_and_futures() {
		type Boxed = Box<dyn FnMut(&Waiter) -> Poll<()>>;

		let ran = Arc::new(AtomicUsize::new(0));
		let mut tasks: Tasks<Boxed> = Tasks::new();

		let counted = ran.clone();
		tasks.push(Box::new(move |_waiter| {
			counted.fetch_add(1, Ordering::SeqCst);
			Poll::Ready(())
		}));

		let counted = ran.clone();
		let mut fut = Box::pin(async move {
			counted.fetch_add(1, Ordering::SeqCst);
		});
		tasks.push(Box::new(move |waiter: &Waiter| waiter.poll_future(fut.as_mut())));

		let waiter = Waiter::noop();
		assert!(tasks.poll(&waiter).is_ready());
		assert_eq!(ran.load(Ordering::SeqCst), 2);
	}

	/// The owner is woken by a task wake and by a push, so neither is lost when
	/// it happens between polls. A burst of edges before the next poll coalesces
	/// into one nudge; the outstanding nudge covers them all.
	#[test]
	fn task_wakes_and_pushes_reach_the_owner() {
		struct Flag(AtomicBool);
		impl Wake for Flag {
			fn wake(self: Arc<Self>) {
				self.0.store(true, Ordering::SeqCst);
			}
		}

		let mut tasks = Tasks::new();
		let gate = crate::Producer::new(false);
		tasks.push(gated(gate.consume(), Arc::new(AtomicUsize::new(0))));

		let flag = Arc::new(Flag(AtomicBool::new(false)));
		let waiter = Waiter::new(Waker::from(flag.clone()));
		assert!(tasks.poll(&waiter).is_pending());

		open(&gate);
		assert!(flag.0.swap(false, Ordering::SeqCst), "the task wake missed the owner");

		// A second edge before the poll is covered by the outstanding nudge.
		let b_gate = crate::Producer::new(false);
		tasks.push(gated(b_gate.consume(), Arc::new(AtomicUsize::new(0))));
		assert!(!flag.0.load(Ordering::SeqCst), "coalesced into the outstanding nudge");

		// Once the owner polls, the next edge nudges again.
		assert!(tasks.poll(&waiter).is_pending());
		let c_gate = crate::Producer::new(false);
		tasks.push(gated(c_gate.consume(), Arc::new(AtomicUsize::new(0))));
		assert!(flag.0.load(Ordering::SeqCst), "the push missed the owner");
	}

	/// A wake landing mid-pass runs in the NEXT owner poll, wherever the woken
	/// slot sits: the snapshot is taken in full before any task runs, so a task
	/// in an early slot waking one in a later slot (here, across a word
	/// boundary) cannot chain execution into a single pass.
	#[test]
	fn a_mid_pass_wake_lands_in_the_next_pass() {
		type Boxed = Box<dyn FnMut(&Waiter) -> Poll<()>>;

		let mut tasks: Tasks<Boxed> = Tasks::new();

		// Slot 0: when its gate opens, it opens B's gate and finishes.
		let a_gate = crate::Producer::new(false);
		let b_gate = crate::Producer::new(false);
		let a_consumer = a_gate.consume();
		let b_opener = b_gate.clone();
		tasks.push(Box::new(move |waiter: &Waiter| {
			match a_consumer.poll(waiter, |open| match **open {
				true => Poll::Ready(()),
				false => Poll::Pending,
			}) {
				Poll::Ready(_) => {
					if let Ok(mut open) = b_opener.write() {
						*open = true;
					}
					Poll::Ready(())
				}
				Poll::Pending => Poll::Pending,
			}
		}));

		// Fillers so B lands in the next bitset word (slot 64).
		let fillers: Vec<_> = (1..WORD_BITS)
			.map(|_| {
				let gate = crate::Producer::new(false);
				let mut task = gated(gate.consume(), Arc::new(AtomicUsize::new(0)));
				tasks.push(Box::new(move |waiter: &Waiter| task(waiter)));
				gate
			})
			.collect();

		let b_polls = Arc::new(AtomicUsize::new(0));
		let mut b = gated(b_gate.consume(), b_polls.clone());
		tasks.push(Box::new(move |waiter: &Waiter| b(waiter)));

		let waiter = Waiter::noop();
		assert!(tasks.poll(&waiter).is_pending());
		assert_eq!(b_polls.load(Ordering::SeqCst), 1, "the initial poll parks B");

		// Fire A: its pass opens B's gate, but B runs next pass, not this one.
		open(&a_gate);
		assert!(tasks.poll(&waiter).is_pending());
		assert_eq!(b_polls.load(Ordering::SeqCst), 1, "B's wake lands in the next pass");

		assert!(tasks.poll(&waiter).is_pending());
		assert_eq!(b_polls.load(Ordering::SeqCst), 2, "the next pass runs B");
		drop(fillers);
	}

	/// A retired occupant's leftover waker firing into a reused slot costs at
	/// most one spurious poll of the new occupant, never a double poll: the
	/// slot's single bit coalesces the stale wake with any real one.
	#[test]
	fn a_stale_wake_costs_at_most_one_spurious_poll() {
		let mut tasks = Tasks::new();

		// The first occupant parks, capturing its slot waker, then finishes.
		let a_gate = crate::Producer::new(false);
		tasks.push(gated(a_gate.consume(), Arc::new(AtomicUsize::new(0))));
		let waiter = Waiter::noop();
		assert!(tasks.poll(&waiter).is_pending());
		let stale = tasks.wakers[0].clone();
		open(&a_gate);
		assert!(tasks.poll(&waiter).is_ready(), "the first occupant retired");

		// The slot is reused; the stale waker fires alongside the push.
		let b_gate = crate::Producer::new(false);
		let b_polls = Arc::new(AtomicUsize::new(0));
		tasks.push(gated(b_gate.consume(), b_polls.clone()));
		stale.wake();

		assert!(tasks.poll(&waiter).is_pending());
		assert_eq!(b_polls.load(Ordering::SeqCst), 1, "one poll, not two");
	}

	/// A task is allowed to self-wake before returning Pending. Such a task must
	/// be polled once per owner poll, not spun on inside it, or it starves the
	/// caller's other arms; the re-queue instead wakes the owner for the next
	/// round.
	#[test]
	fn a_self_waking_task_yields_to_the_owner() {
		struct Flag(AtomicBool);
		impl Wake for Flag {
			fn wake(self: Arc<Self>) {
				self.0.store(true, Ordering::SeqCst);
			}
		}

		let mut tasks = Tasks::new();
		let polls = Arc::new(AtomicUsize::new(0));
		let counted = polls.clone();
		tasks.push(move |waiter: &Waiter| {
			counted.fetch_add(1, Ordering::SeqCst);
			waiter.waker().wake_by_ref();
			Poll::Pending
		});

		let flag = Arc::new(Flag(AtomicBool::new(false)));
		let waiter = Waiter::new(Waker::from(flag.clone()));

		assert!(tasks.poll(&waiter).is_pending());
		assert_eq!(polls.load(Ordering::SeqCst), 1, "one poll per owner poll");
		assert!(flag.0.swap(false, Ordering::SeqCst), "the re-queue must wake the owner");

		assert!(tasks.poll(&waiter).is_pending());
		assert_eq!(
			polls.load(Ordering::SeqCst),
			2,
			"the next owner poll runs the task again"
		);
	}

	/// Drive a set to completion as a future. Generic on purpose: the two tests
	/// below hand this same code to `tokio::spawn` (which demands `Send`) and to
	/// `spawn_local` with `!Send` tasks. Send-ness is inferred from the task
	/// type, never demanded by kio; if either test stops compiling, we failed.
	async fn drive<T: FnMut(&Waiter) -> Poll<()> + Unpin>(mut tasks: Tasks<T>) {
		crate::wait(move |waiter| tasks.poll(waiter)).await
	}

	/// `Send` tasks make a `Send` set: `tokio::spawn` accepts it.
	#[tokio::test]
	async fn send_tasks_drive_on_spawn() {
		let done = Arc::new(AtomicUsize::new(0));

		let mut tasks = Tasks::new();
		let counted = done.clone();
		tasks.push(move |_: &Waiter| {
			counted.fetch_add(1, Ordering::SeqCst);
			Poll::Ready(())
		});

		tokio::spawn(drive(tasks)).await.unwrap();
		assert_eq!(done.load(Ordering::SeqCst), 1);
	}

	/// `!Send` tasks (an `Rc` capture) make a `!Send` set: `spawn_local` drives
	/// the exact same code.
	#[tokio::test]
	async fn local_tasks_drive_on_spawn_local() {
		use std::{cell::Cell, rc::Rc};

		let local = tokio::task::LocalSet::new();
		local
			.run_until(async {
				let done = Rc::new(Cell::new(0));

				let mut tasks = Tasks::new();
				let counted = done.clone();
				tasks.push(move |_: &Waiter| {
					counted.set(counted.get() + 1);
					Poll::Ready(())
				});

				tokio::task::spawn_local(drive(tasks)).await.unwrap();
				assert_eq!(done.get(), 1);
			})
			.await;
	}
}
