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

use std::{
	sync::{
		Arc, Mutex,
		atomic::{AtomicBool, Ordering},
	},
	task::{Context, Poll, Wake, Waker},
};

use crate::waiter::{Park, Waiter};

/// A set of poll-closure tasks polled by their owner with per-task granularity.
///
/// [`poll`](Self::poll) drives the tasks that are new or woken and reports
/// `Ready` when the set is empty; [`push`](Self::push) wakes the owner, so a
/// task added between polls gets started. Dropping the set cancels every
/// remaining task.
pub struct Tasks<T> {
	/// Slab of tasks; `None` slots are free and listed in `free`.
	children: Vec<Option<Child<T>>>,
	free: Vec<usize>,
	len: usize,
	shared: Arc<Shared>,
}

struct Child<T> {
	task: T,
	/// This task's waker, marking it ready and waking the owner.
	waker: Waker,
	flag: Arc<ChildWaker>,
	/// Retains the task's kio registrations between polls.
	park: Park,
}

/// Waker state shared with every child; deliberately `T`-free so the wakers
/// stay `Send + Sync` even when the tasks themselves are not.
struct Shared {
	/// Indices needing a poll. A task appears at most once (see [`ChildWaker::dirty`]).
	ready: Mutex<Vec<usize>>,
	/// The waker of whoever polls the set, re-registered on every poll.
	parent: Mutex<Option<Waker>>,
}

impl Shared {
	fn wake_parent(&self) {
		let parent = self.parent.lock().unwrap().clone();
		if let Some(waker) = parent {
			waker.wake();
		}
	}
}

/// The per-task waker: queues the task's index and wakes the owner.
struct ChildWaker {
	index: usize,
	/// Set while the task is queued, so a burst of wakes queues it once. Cleared
	/// just before the task is polled, so a wake racing the poll re-queues it.
	dirty: AtomicBool,
	shared: Arc<Shared>,
}

impl Wake for ChildWaker {
	fn wake(self: Arc<Self>) {
		self.wake_by_ref();
	}

	fn wake_by_ref(self: &Arc<Self>) {
		if self.dirty.swap(true, Ordering::AcqRel) {
			return;
		}
		self.shared.ready.lock().unwrap().push(self.index);
		self.shared.wake_parent();
	}
}

impl<T> Tasks<T> {
	/// Create an empty set.
	pub fn new() -> Self {
		Self {
			children: Vec::new(),
			free: Vec::new(),
			len: 0,
			shared: Arc::new(Shared {
				ready: Mutex::new(Vec::new()),
				parent: Mutex::new(None),
			}),
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
				self.children.push(None);
				self.children.len() - 1
			}
		};
		let flag = Arc::new(ChildWaker {
			index,
			dirty: AtomicBool::new(true),
			shared: self.shared.clone(),
		});
		self.children[index] = Some(Child {
			task,
			waker: Waker::from(flag.clone()),
			flag,
			park: Park::default(),
		});
		self.len += 1;
		self.shared.ready.lock().unwrap().push(index);
		self.shared.wake_parent();
	}
}

impl<T: FnMut(&Waiter) -> Poll<()>> Tasks<T> {
	/// Poll every task that is new or was woken since the last call, retiring
	/// the ones that return `Ready`.
	///
	/// `Ready` when the set is empty, like `FuturesUnordered` reporting `None`.
	/// For a long-lived driver arm that keeps pushing, treat it as "drained for
	/// now" rather than an exit condition. `waiter` is registered for the next
	/// task wake or push; the tasks themselves park on their own wakers.
	pub fn poll(&mut self, waiter: &Waiter) -> Poll<()> {
		{
			let mut parent = self.shared.parent.lock().unwrap();
			let waker = waiter.waker();
			if !parent.as_ref().is_some_and(|w| w.will_wake(waker)) {
				*parent = Some(waker.clone());
			}
		}

		// One snapshot per owner poll: a task woken during the pass (including a
		// self-wake before returning Pending) re-queues itself and has already
		// woken the owner through its ChildWaker, so the executor re-polls this
		// set. Looping here instead would let a self-waking task spin without
		// ever yielding, starving the caller's other arms.
		let ready = std::mem::take(&mut *self.shared.ready.lock().unwrap());
		for index in ready {
			// A stale index (the task finished, maybe its slot was reused) is
			// skipped or costs one spurious poll; both are harmless.
			let Some(child) = self.children.get_mut(index).and_then(Option::as_mut) else {
				continue;
			};
			// Cleared before polling: a wake landing mid-poll re-queues the task
			// rather than being lost.
			child.flag.dirty.store(false, Ordering::Release);
			let cx = Context::from_waker(&child.waker);
			let child_waiter = child.park.hold(&cx);
			if (child.task)(child_waiter).is_ready() {
				self.children[index] = None;
				self.free.push(index);
				self.len -= 1;
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
	/// it happens between polls.
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

		tasks.push(gated(
			crate::Producer::new(false).consume(),
			Arc::new(AtomicUsize::new(0)),
		));
		assert!(flag.0.load(Ordering::SeqCst), "the push missed the owner");
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
