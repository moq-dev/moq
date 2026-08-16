//! A dynamic set of poll-driven children with per-child wakeups.
//!
//! The poll-native counterpart of `FuturesUnordered`: each child owns a
//! [`Waker`] that marks it ready and wakes the parent, so a wakeup re-polls only
//! the child it was aimed at. A session serving hundreds of subscriptions polls
//! one machine per event, not all of them.

use std::{
	sync::{
		Arc, Mutex,
		atomic::{AtomicBool, Ordering},
	},
	task::{Context, Poll, Wake, Waker},
};

/// A child state machine drivable by a [`PollSet`].
pub(crate) trait Machine {
	/// Drive this child one step; `Ready` retires it from the set.
	///
	/// The machine owns its outcome: log or record errors before returning
	/// `Ready`, the set only removes the entry.
	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()>;
}

/// A set of [`Machine`]s polled with per-child granularity.
pub(crate) struct PollSet<T> {
	/// Slab of children; `None` slots are free and listed in `free`.
	children: Vec<Option<Child<T>>>,
	free: Vec<usize>,
	len: usize,
	shared: Arc<Shared>,
}

struct Child<T> {
	machine: T,
	/// This child's waker, marking it ready and waking the parent.
	waker: Waker,
	flag: Arc<ChildWaker>,
	/// Retains the child's kio registrations between polls.
	park: kio::Park,
}

/// State shared with every child waker.
struct Shared {
	/// Indices needing a poll. A child appears at most once (see [`ChildWaker::dirty`]).
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

/// The per-child waker: queues the child's index and wakes the parent.
struct ChildWaker {
	index: usize,
	/// Set while the child is queued, so a burst of wakes queues it once. Cleared
	/// just before the child is polled, so a wake racing the poll re-queues it.
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

impl<T: Machine> PollSet<T> {
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

	/// The number of live children.
	#[cfg(test)]
	pub fn len(&self) -> usize {
		self.len
	}

	pub fn is_empty(&self) -> bool {
		self.len == 0
	}

	/// Add a child, queued for its first poll. Wakes the parent so a push from
	/// outside its poll still gets the child started.
	pub fn push(&mut self, machine: T) {
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
			machine,
			waker: Waker::from(flag.clone()),
			flag,
			park: kio::Park::default(),
		});
		self.len += 1;
		self.shared.ready.lock().unwrap().push(index);
		self.shared.wake_parent();
	}

	/// Poll every child that has been woken since the last call, retiring the
	/// finished ones. `Ready` when the set is empty.
	///
	/// `waiter` is registered for the next child wake (and for pushes); the
	/// children themselves park on their own wakers.
	pub fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		{
			let mut parent = self.shared.parent.lock().unwrap();
			let waker = waiter.waker();
			if !parent.as_ref().is_some_and(|w| w.will_wake(waker)) {
				*parent = Some(waker.clone());
			}
		}

		// One snapshot per parent poll: a child woken during the pass (including a
		// self-wake before returning Pending) re-queues itself and has already
		// woken the parent through its ChildWaker, so the executor re-polls this
		// set. Looping here instead would let a self-waking child spin without
		// ever yielding, starving the caller's other arms.
		let ready = std::mem::take(&mut *self.shared.ready.lock().unwrap());
		for index in ready {
			// A stale index (the child finished, maybe its slot was reused) is
			// skipped or costs one spurious poll; both are harmless.
			let Some(child) = self.children.get_mut(index).and_then(Option::as_mut) else {
				continue;
			};
			// Cleared before polling: a wake landing mid-poll re-queues the child
			// rather than being lost.
			child.flag.dirty.store(false, Ordering::Release);
			let cx = Context::from_waker(&child.waker);
			let child_waiter = child.park.hold(&cx);
			if child.machine.poll(child_waiter).is_ready() {
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

impl<T: Machine> Default for PollSet<T> {
	fn default() -> Self {
		Self::new()
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::sync::atomic::AtomicUsize;

	/// Counts its polls; finishes when its gate opens.
	struct Gated {
		gate: kio::Consumer<bool>,
		polls: Arc<AtomicUsize>,
	}

	impl Machine for Gated {
		fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
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

	fn gated(set: &mut PollSet<Gated>) -> (kio::Producer<bool>, Arc<AtomicUsize>) {
		let gate = kio::Producer::new(false);
		let polls = Arc::new(AtomicUsize::new(0));
		set.push(Gated {
			gate: gate.consume(),
			polls: polls.clone(),
		});
		(gate, polls)
	}

	fn open(gate: &kio::Producer<bool>) {
		let Ok(mut state) = gate.write() else {
			panic!("gate closed")
		};
		*state = true;
	}

	/// A wake aimed at one child polls only that child.
	#[test]
	fn a_wake_polls_only_its_child() {
		let mut set = PollSet::new();
		let (a_gate, a_polls) = gated(&mut set);
		let (_b_gate, b_polls) = gated(&mut set);

		let waiter = kio::Waiter::noop();
		assert!(set.poll(&waiter).is_pending());
		assert_eq!((a_polls.load(Ordering::SeqCst), b_polls.load(Ordering::SeqCst)), (1, 1));

		// Waking a's gate must re-poll a alone.
		open(&a_gate);
		assert!(set.poll(&waiter).is_pending(), "b is still live");
		assert_eq!(a_polls.load(Ordering::SeqCst), 2, "a was woken");
		assert_eq!(b_polls.load(Ordering::SeqCst), 1, "b was not");
	}

	/// Finished children retire, their slots are reused, and an empty set is Ready.
	#[test]
	fn finished_children_retire_and_slots_recycle() {
		let mut set = PollSet::new();
		let (a_gate, _) = gated(&mut set);
		let (b_gate, _) = gated(&mut set);
		assert_eq!(set.len(), 2);

		let waiter = kio::Waiter::noop();
		open(&a_gate);
		assert!(set.poll(&waiter).is_pending());
		assert_eq!(set.len(), 1);

		// The freed slot is reused without growing the slab.
		let (c_gate, _) = gated(&mut set);
		assert_eq!(set.children.len(), 2);

		open(&b_gate);
		open(&c_gate);
		assert!(set.poll(&waiter).is_ready(), "an empty set is Ready");
		assert!(set.is_empty());
	}

	/// A wake for a child that already finished is skipped, even after its slot
	/// has been handed to a newcomer.
	#[test]
	fn a_stale_wake_is_harmless() {
		let mut set = PollSet::new();
		let (gate, _) = gated(&mut set);
		let stale = set.children[0].as_ref().unwrap().flag.clone();

		let waiter = kio::Waiter::noop();
		open(&gate);
		assert!(set.poll(&waiter).is_ready());

		// The dead child's waker fires (its slot is empty): nothing to poll.
		Waker::from(stale).wake();
		assert!(set.poll(&waiter).is_ready());
	}

	/// The parent is woken by a child wake and by a push, so neither is lost when
	/// it happens between polls.
	#[test]
	fn child_wakes_and_pushes_reach_the_parent() {
		struct Flag(AtomicBool);
		impl Wake for Flag {
			fn wake(self: Arc<Self>) {
				self.0.store(true, Ordering::SeqCst);
			}
		}

		let mut set = PollSet::new();
		let (gate, _) = gated(&mut set);

		let flag = Arc::new(Flag(AtomicBool::new(false)));
		let waiter = kio::Waiter::new(Waker::from(flag.clone()));
		assert!(set.poll(&waiter).is_pending());

		open(&gate);
		assert!(flag.0.swap(false, Ordering::SeqCst), "the child wake missed the parent");

		let _ = gated(&mut set);
		assert!(flag.0.load(Ordering::SeqCst), "the push missed the parent");
	}

	/// A transport is allowed to self-wake before returning Pending. Such a child
	/// must be polled once per parent poll, not spun on inside it, or it starves
	/// the caller's other arms; the re-queue instead wakes the parent for the
	/// next round.
	#[test]
	fn a_self_waking_child_yields_to_the_parent() {
		struct Flag(AtomicBool);
		impl Wake for Flag {
			fn wake(self: Arc<Self>) {
				self.0.store(true, Ordering::SeqCst);
			}
		}

		struct SelfWaking {
			polls: Arc<AtomicUsize>,
		}
		impl Machine for SelfWaking {
			fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
				self.polls.fetch_add(1, Ordering::SeqCst);
				waiter.waker().wake_by_ref();
				Poll::Pending
			}
		}

		let mut set = PollSet::new();
		let polls = Arc::new(AtomicUsize::new(0));
		set.push(SelfWaking { polls: polls.clone() });

		let flag = Arc::new(Flag(AtomicBool::new(false)));
		let waiter = kio::Waiter::new(Waker::from(flag.clone()));

		assert!(set.poll(&waiter).is_pending());
		assert_eq!(polls.load(Ordering::SeqCst), 1, "one poll per parent poll");
		assert!(
			flag.0.swap(false, Ordering::SeqCst),
			"the re-queue must wake the parent"
		);

		assert!(set.poll(&waiter).is_pending());
		assert_eq!(
			polls.load(Ordering::SeqCst),
			2,
			"the next parent poll runs the child again"
		);
	}
}
