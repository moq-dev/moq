//! A driver-owned set of poll-driven tasks with per-task wakeups.
//!
//! The poll-native alternative to spawning on an executor or collecting boxed
//! futures in a `FuturesUnordered`: the owner polls the set, each task owns a
//! waker that marks it ready and wakes the owner, so a wakeup re-polls only the
//! task it was aimed at. A driver serving hundreds of children steps one machine
//! per event, not all of them.
//!
//! Work arrives two ways: the owner pushes directly ([`TaskSet::push`]), or
//! anyone holding a cloneable [`Spawner`] submits from outside (including from
//! inside a running task, so nested work needs no executor). The set resolves
//! once every spawner has dropped and the remaining tasks finish; dropping the
//! set instead cancels everything, queued and running.

use std::{
	collections::VecDeque,
	future::Future,
	pin::Pin,
	sync::{
		Arc, Mutex,
		atomic::{AtomicBool, Ordering},
	},
	task::{Context, Poll, Wake, Waker},
};

use crate::waiter::{Park, Waiter};

/// Work drivable by a [`TaskSet`]: a poll function stepped with a [`Waiter`].
///
/// Implemented for every `Future<Output = ()> + Unpin` (so `Pin<Box<dyn Future>>`
/// works directly), letting a set mix boxed futures with hand-rolled machines.
pub trait Task {
	/// Drive the task one step; `Ready` retires it from the set.
	///
	/// The task owns its outcome: log or record errors before returning
	/// `Ready`, the set only removes the entry.
	fn poll(&mut self, waiter: &Waiter) -> Poll<()>;
}

impl<F: Future<Output = ()> + Unpin> Task for F {
	fn poll(&mut self, waiter: &Waiter) -> Poll<()> {
		waiter.poll_future(Pin::new(self))
	}
}

/// A set of [`Task`]s polled by their owner with per-task granularity.
///
/// Created empty via [`TaskSet::new`] (paired with a [`Spawner`]) or
/// [`TaskSet::owned`] (owner-push only). Dropping the set cancels every queued
/// and running task and closes the spawners.
pub struct TaskSet<T> {
	/// Slab of tasks; `None` slots are free and listed in `free`.
	children: Vec<Option<Child<T>>>,
	free: Vec<usize>,
	len: usize,
	shared: Arc<Shared>,
	submit: Arc<Submit<T>>,
}

/// The submitting half of a [`TaskSet`]: clone it into whoever spawns work.
///
/// The set resolves only after every spawner has dropped, so holding one is a
/// promise that more work may still arrive. A task that needs to spawn nested
/// work keeps a clone of its own set's spawner.
pub struct Spawner<T> {
	shared: Arc<Shared>,
	submit: Arc<Submit<T>>,
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

/// Submissions from [`Spawner`]s, adopted into the slab on the next poll.
struct Submit<T> {
	state: Mutex<SubmitState<T>>,
}

struct SubmitState<T> {
	queued: VecDeque<T>,
	/// Live [`Spawner`] handles; the set resolves once this reaches zero and
	/// the queued plus running tasks drain.
	spawners: usize,
	/// The set dropped: submissions are refused.
	closed: bool,
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

impl<T> Spawner<T> {
	/// Submit a task, handing it back if the set has been dropped.
	pub fn push(&self, task: T) -> Result<(), T> {
		{
			let mut state = self.submit.state.lock().unwrap();
			if state.closed {
				return Err(task);
			}
			state.queued.push_back(task);
		}
		self.shared.wake_parent();
		Ok(())
	}

	/// Whether the set has been dropped, so a push would be refused.
	pub fn is_closed(&self) -> bool {
		self.submit.state.lock().unwrap().closed
	}
}

impl<T> Clone for Spawner<T> {
	fn clone(&self) -> Self {
		self.submit.state.lock().unwrap().spawners += 1;
		Self {
			shared: self.shared.clone(),
			submit: self.submit.clone(),
		}
	}
}

impl<T> Drop for Spawner<T> {
	fn drop(&mut self) {
		let last = {
			let mut state = self.submit.state.lock().unwrap();
			state.spawners -= 1;
			state.spawners == 0
		};
		// The last spawner leaving may be what lets the set resolve.
		if last {
			self.shared.wake_parent();
		}
	}
}

impl<T> TaskSet<T> {
	/// Create an empty set and the [`Spawner`] that submits into it.
	pub fn new() -> (Spawner<T>, Self) {
		let set = Self::with_spawners(1);
		let spawner = Spawner {
			shared: set.shared.clone(),
			submit: set.submit.clone(),
		};
		(spawner, set)
	}

	/// Create a set only its owner can push to, for a loop that accepts work and
	/// drives each unit as a task. Resolves whenever the tasks drain.
	pub fn owned() -> Self {
		Self::with_spawners(0)
	}

	fn with_spawners(spawners: usize) -> Self {
		Self {
			children: Vec::new(),
			free: Vec::new(),
			len: 0,
			shared: Arc::new(Shared {
				ready: Mutex::new(Vec::new()),
				parent: Mutex::new(None),
			}),
			submit: Arc::new(Submit {
				state: Mutex::new(SubmitState {
					queued: VecDeque::new(),
					spawners,
					closed: false,
				}),
			}),
		}
	}

	/// The number of running tasks (queued submissions not yet adopted excluded).
	pub fn len(&self) -> usize {
		self.len
	}

	/// Whether no tasks are running.
	pub fn is_empty(&self) -> bool {
		self.len == 0
	}

	/// Add a task directly, queued for its first poll on the next [`poll`](Self::poll).
	///
	/// Wakes the owner, so a push from outside its poll still gets the task
	/// started.
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

impl<T: Task> TaskSet<T> {
	/// Adopt queued submissions, then poll every task woken since the last call,
	/// retiring the finished ones.
	///
	/// `Ready` once every [`Spawner`] has dropped and the tasks drain; an
	/// owner-only set ([`Self::owned`]) reaches it whenever the tasks drain.
	/// `waiter` is registered for the next task wake, submission, or the last
	/// spawner leaving; the tasks themselves park on their own wakers.
	pub fn poll(&mut self, waiter: &Waiter) -> Poll<()> {
		{
			let mut parent = self.shared.parent.lock().unwrap();
			let waker = waiter.waker();
			if !parent.as_ref().is_some_and(|w| w.will_wake(waker)) {
				*parent = Some(waker.clone());
			}
		}

		let spawners = {
			let mut submit = self.submit.state.lock().unwrap();
			let queued = std::mem::take(&mut submit.queued);
			let spawners = submit.spawners;
			drop(submit);
			for task in queued {
				self.push(task);
			}
			spawners
		};

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
			if child.task.poll(child_waiter).is_ready() {
				self.children[index] = None;
				self.free.push(index);
				self.len -= 1;
			}
		}

		match self.len == 0 && spawners == 0 && self.submit.state.lock().unwrap().queued.is_empty() {
			true => Poll::Ready(()),
			false => Poll::Pending,
		}
	}
}

impl<T> Drop for TaskSet<T> {
	fn drop(&mut self) {
		// Refuse and discard submissions; the running tasks drop with the slab.
		let mut state = self.submit.state.lock().unwrap();
		state.closed = true;
		state.queued.clear();
	}
}

#[cfg(all(test, not(loom)))]
mod tests {
	use super::*;
	use std::sync::atomic::AtomicUsize;

	/// Counts its polls; finishes when its gate opens.
	struct Gated {
		gate: crate::Consumer<bool>,
		polls: Arc<AtomicUsize>,
	}

	impl Task for Gated {
		fn poll(&mut self, waiter: &Waiter) -> Poll<()> {
			self.polls.fetch_add(1, Ordering::SeqCst);
			match self.gate.poll(waiter, |open| match **open {
				true => Poll::Ready(()),
				false => Poll::Pending,
			}) {
				Poll::Ready(_) => Poll::Ready(()),
				Poll::Pending => Poll::Pending,
			}
		}
	}

	fn gated(set: &mut TaskSet<Gated>) -> (crate::Producer<bool>, Arc<AtomicUsize>) {
		let gate = crate::Producer::new(false);
		let polls = Arc::new(AtomicUsize::new(0));
		set.push(Gated {
			gate: gate.consume(),
			polls: polls.clone(),
		});
		(gate, polls)
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
		let mut set = TaskSet::owned();
		let (a_gate, a_polls) = gated(&mut set);
		let (_b_gate, b_polls) = gated(&mut set);

		let waiter = Waiter::noop();
		assert!(set.poll(&waiter).is_pending());
		assert_eq!((a_polls.load(Ordering::SeqCst), b_polls.load(Ordering::SeqCst)), (1, 1));

		// Waking a's gate must re-poll a alone.
		open(&a_gate);
		assert!(set.poll(&waiter).is_pending(), "b is still live");
		assert_eq!(a_polls.load(Ordering::SeqCst), 2, "a was woken");
		assert_eq!(b_polls.load(Ordering::SeqCst), 1, "b was not");
	}

	/// Finished tasks retire, their slots are reused, and a drained owned set is
	/// Ready.
	#[test]
	fn finished_tasks_retire_and_slots_recycle() {
		let mut set = TaskSet::owned();
		let (a_gate, _) = gated(&mut set);
		let (b_gate, _) = gated(&mut set);
		assert_eq!(set.len(), 2);

		let waiter = Waiter::noop();
		open(&a_gate);
		assert!(set.poll(&waiter).is_pending());
		assert_eq!(set.len(), 1);

		// The freed slot is reused without growing the slab.
		let (c_gate, _) = gated(&mut set);
		assert_eq!(set.children.len(), 2);

		open(&b_gate);
		open(&c_gate);
		assert!(set.poll(&waiter).is_ready(), "a drained owned set is Ready");
		assert!(set.is_empty());
	}

	/// The set resolves only after the last spawner drops, even with no tasks.
	#[test]
	fn spawners_keep_the_set_alive() {
		let (spawner, mut set) = TaskSet::<Gated>::new();
		let waiter = Waiter::noop();
		assert!(set.poll(&waiter).is_pending(), "a live spawner may still submit");

		let clone = spawner.clone();
		drop(spawner);
		assert!(set.poll(&waiter).is_pending(), "a cloned spawner counts too");

		drop(clone);
		assert!(set.poll(&waiter).is_ready(), "no spawner, no tasks");
	}

	/// A spawner submission runs on the next poll, and a submission from inside a
	/// running task (nested spawn) runs without an executor.
	#[test]
	fn spawner_submissions_run_nested() {
		struct Once {
			ran: Arc<AtomicUsize>,
			nested: Option<Spawner<Once>>,
		}
		impl Task for Once {
			fn poll(&mut self, _waiter: &Waiter) -> Poll<()> {
				self.ran.fetch_add(1, Ordering::SeqCst);
				if let Some(spawner) = self.nested.take() {
					let ran = self.ran.clone();
					let _ = spawner.push(Once { ran, nested: None });
				}
				Poll::Ready(())
			}
		}

		let (spawner, mut set) = TaskSet::new();
		let ran = Arc::new(AtomicUsize::new(0));
		assert!(
			spawner
				.push(Once {
					ran: ran.clone(),
					nested: Some(spawner.clone()),
				})
				.is_ok()
		);
		drop(spawner);

		// The task's clone of the spawner dropped when it finished, so the drain
		// resolves the set: first poll runs the outer (queuing the nested), the
		// second runs the nested and resolves.
		let waiter = Waiter::noop();
		assert!(set.poll(&waiter).is_pending());
		assert!(set.poll(&waiter).is_ready());
		assert_eq!(ran.load(Ordering::SeqCst), 2);
	}

	/// Dropping the set refuses later submissions and hands the task back.
	#[test]
	fn dropping_the_set_closes_spawners() {
		let (spawner, set) = TaskSet::new();
		assert!(!spawner.is_closed());
		drop(set);
		assert!(spawner.is_closed());

		let ran = Arc::new(AtomicUsize::new(0));
		let task = Gated {
			gate: crate::Producer::new(false).consume(),
			polls: ran,
		};
		assert!(spawner.push(task).is_err(), "the set is gone");
	}

	/// Boxed futures are tasks: the blanket impl covers `Pin<Box<dyn Future>>`.
	#[test]
	fn boxed_futures_are_tasks() {
		let ran = Arc::new(AtomicUsize::new(0));
		let mut set: TaskSet<Pin<Box<dyn Future<Output = ()>>>> = TaskSet::owned();
		let counted = ran.clone();
		set.push(Box::pin(async move {
			counted.fetch_add(1, Ordering::SeqCst);
		}));

		let waiter = Waiter::noop();
		assert!(set.poll(&waiter).is_ready());
		assert_eq!(ran.load(Ordering::SeqCst), 1);
	}

	/// The owner is woken by a task wake, a direct push, a spawner submission,
	/// and the last spawner leaving, so none of them is lost between polls.
	#[test]
	fn every_edge_wakes_the_owner() {
		struct Flag(AtomicBool);
		impl Wake for Flag {
			fn wake(self: Arc<Self>) {
				self.0.store(true, Ordering::SeqCst);
			}
		}

		let (spawner, mut set) = TaskSet::new();
		let (gate, _) = gated(&mut set);

		let flag = Arc::new(Flag(AtomicBool::new(false)));
		let waiter = Waiter::new(Waker::from(flag.clone()));
		assert!(set.poll(&waiter).is_pending());

		open(&gate);
		assert!(flag.0.swap(false, Ordering::SeqCst), "the task wake missed the owner");

		let _ = gated(&mut set);
		assert!(flag.0.swap(false, Ordering::SeqCst), "the push missed the owner");

		assert!(
			spawner
				.push(Gated {
					gate: crate::Producer::new(false).consume(),
					polls: Arc::new(AtomicUsize::new(0)),
				})
				.is_ok()
		);
		assert!(flag.0.swap(false, Ordering::SeqCst), "the submission missed the owner");

		drop(spawner);
		assert!(
			flag.0.load(Ordering::SeqCst),
			"the last spawner leaving missed the owner"
		);
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

		struct SelfWaking {
			polls: Arc<AtomicUsize>,
		}
		impl Task for SelfWaking {
			fn poll(&mut self, waiter: &Waiter) -> Poll<()> {
				self.polls.fetch_add(1, Ordering::SeqCst);
				waiter.waker().wake_by_ref();
				Poll::Pending
			}
		}

		let mut set = TaskSet::owned();
		let polls = Arc::new(AtomicUsize::new(0));
		set.push(SelfWaking { polls: polls.clone() });

		let flag = Arc::new(Flag(AtomicBool::new(false)));
		let waiter = Waiter::new(Waker::from(flag.clone()));

		assert!(set.poll(&waiter).is_pending());
		assert_eq!(polls.load(Ordering::SeqCst), 1, "one poll per owner poll");
		assert!(flag.0.swap(false, Ordering::SeqCst), "the re-queue must wake the owner");

		assert!(set.poll(&waiter).is_pending());
		assert_eq!(
			polls.load(Ordering::SeqCst),
			2,
			"the next owner poll runs the task again"
		);
	}
}
