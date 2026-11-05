//! Future boxing and the driver-owned task set.

use std::{collections::VecDeque, task::Poll};
#[cfg(feature = "async")]
use std::{future::Future, pin::Pin};

mod cell;
mod idle;
#[cfg(feature = "async")]
mod later;
mod wake;
pub(crate) use cell::SendCell;
pub(crate) use idle::Idle;
#[cfg(feature = "async")]
pub(crate) use later::{Later, Learning};
pub(crate) use wake::QueueWake;

/// A boxed `Send` future, the same type as `futures::future::BoxFuture`.
#[cfg(feature = "async")]
pub(crate) type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A boxed [`kio::Tasks`] task: hand-rolled machines and adapted futures alike.
pub(crate) type BoxTask = Box<dyn FnMut(&kio::Waiter) -> Poll<()> + Send>;

/// Adapt a future into a [`kio::Tasks`] task, for a caller not yet written as a poll.
#[cfg(feature = "async")]
pub(crate) fn future_task(task: impl Future<Output = ()> + Send + 'static) -> BoxTask {
	let mut task: BoxFuture<'static, ()> = Box::pin(task);
	Box::new(move |waiter| waiter.poll_future(task.as_mut()))
}

/// Cloneable handle for submitting tasks to a driver-owned [`TaskSet`].
#[derive(Clone)]
pub(crate) struct Tasks {
	state: kio::Shared<Submissions>,
	/// The set's lifetime: it finishes once every owning handle, [`Tasks`] or
	/// [`Keepalive`], is gone. A kio token rather than a count under the queue's
	/// lock.
	alive: kio::Producer<()>,
}

/// A claim on a [`TaskSet`]'s lifetime with no submission queue: what a
/// broadcast published on an origin and an `origin::Requests` hold, so the
/// driver outlives the producer handles a session was given and dropped.
pub(crate) struct Keepalive {
	_alive: kio::Producer<()>,
}

/// The submission queue between [`Tasks`] handles and their [`TaskSet`].
#[derive(Default)]
struct Submissions {
	queued: VecDeque<BoxTask>,
	/// The set dropped: submissions are discarded, as the driver that would
	/// have polled them has torn down.
	closed: bool,
}

impl Tasks {
	/// Queue a task for polling by the associated [`TaskSet`].
	///
	/// A task submitted after the set is gone is dropped: the driver has torn down,
	/// so the task would have been cancelled anyway.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub fn push(&self, mut task: impl kio::Task<Output = ()> + Send + 'static) {
		let mut state = self.state.lock();
		if !state.closed {
			state
				.queued
				.push_back(Box::new(move |waiter: &kio::Waiter| task.poll(waiter)));
		}
	}
}

impl Tasks {
	/// A non-owning submission handle: pushes work while the driver lives, but
	/// neither keeps the set from finishing nor counts as an owner. For read
	/// handles, whose existence must not extend the origin's lifecycle.
	pub fn downgrade(&self) -> TasksWeak {
		TasksWeak {
			state: self.state.clone(),
			alive: self.alive.consume(),
		}
	}

	/// A claim on the set's lifetime alone; see [`Keepalive`].
	pub fn keepalive(&self) -> Keepalive {
		Keepalive {
			_alive: self.alive.clone(),
		}
	}
}

/// Non-owning [`Tasks`]: same submissions, no claim on the set's lifetime, so
/// [`TaskSet::poll`] still finishes once the owning handles drop.
#[derive(Clone)]
pub(crate) struct TasksWeak {
	state: kio::Shared<Submissions>,
	alive: kio::Consumer<()>,
}

impl TasksWeak {
	/// Queue a task for polling by the associated [`TaskSet`], dropped if the
	/// set (or every owning handle) is already gone.
	pub fn push(&self, mut task: impl kio::Task<Output = ()> + Send + 'static) {
		let mut state = self.state.lock();
		if !state.closed && !self.alive.is_closed() {
			state
				.queued
				.push_back(Box::new(move |waiter: &kio::Waiter| task.poll(waiter)));
		}
	}

	/// Poll for every owning handle being gone, after which the set finishes as soon
	/// as its children do.
	pub fn poll_orphaned(&self, waiter: &kio::Waiter) -> Poll<()> {
		self.alive.poll_closed(waiter)
	}
}

/// A dynamic set of child tasks polled by its parent driver, built on
/// [`kio::Tasks`] (per-child wakeups, drop cancels).
///
/// Unlike an executor spawn, dropping this value cancels every queued and active
/// child. [`Tasks`] handles may be cloned into those children so nested protocol
/// state can submit more work without choosing an async runtime.
pub(crate) struct TaskSet {
	state: kio::Shared<Submissions>,
	active: kio::Tasks<BoxTask>,
	/// Closed once every owning handle is gone; see [`Tasks::alive`].
	alive: kio::Consumer<()>,
}

impl TaskSet {
	/// Create a task submission handle and its driver-owned set.
	pub fn new() -> (Tasks, Self) {
		let state = kio::Shared::<Submissions>::default();
		let alive = kio::Producer::<()>::default();
		let set = Self {
			state: state.clone(),
			active: kio::Tasks::new(),
			alive: alive.consume(),
		};
		(Tasks { state, alive }, set)
	}

	/// Create a set that only its owner can push to, for a loop that accepts streams
	/// and serves each one as a child.
	pub fn owned() -> Self {
		Self {
			state: kio::Shared::default(),
			active: kio::Tasks::new(),
			// No owning handle ever exists, so the set finishes when its children drain.
			alive: kio::Producer::<()>::default().consume(),
		}
	}

	/// Queue a task for polling by this set.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub fn push(&mut self, mut task: impl kio::Task<Output = ()> + Send + 'static) {
		self.active.push(Box::new(move |waiter: &kio::Waiter| task.poll(waiter)));
	}

	/// Adopt every queued submission into the active set, then poll the children.
	///
	/// `Ready` once every submission handle is dropped and all children finish;
	/// an owner-only set ([`Self::owned`]) reaches it when its children drain.
	pub fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		// Adopt submissions until the queue reports Pending, which is what
		// registers `waiter` on it. The registration must land before the
		// children run below: a child pushing through a `Tasks` clone mid-pass
		// wakes whoever is registered, and without it that push would be lost.
		let mut submissions_done = false;
		loop {
			// Registers `waiter` for the last owning handle dropping, which may be
			// what lets the set finish.
			let orphaned = self.alive.poll_closed(waiter).is_ready();
			let Poll::Ready(mut state) = self.state.poll(waiter, |state| {
				if state.queued.is_empty() && !orphaned {
					Poll::Pending
				} else {
					Poll::Ready(())
				}
			}) else {
				break;
			};
			while let Some(task) = state.queued.pop_front() {
				self.active.push(task);
			}
			// No handles left: nothing can ever submit again.
			if orphaned {
				submissions_done = true;
				break;
			}
		}

		match self.active.poll(waiter) {
			Poll::Ready(()) if submissions_done => Poll::Ready(()),
			_ => Poll::Pending,
		}
	}

	/// Drive submitted children until every submission handle is dropped and all
	/// active children finish. The async form of [`Self::poll`], for callers with
	/// nothing else to poll.
	#[cfg(test)]
	#[cfg(feature = "async")]
	pub async fn run(mut self) {
		kio::wait(|waiter| self.poll(waiter)).await
	}
}

impl Drop for TaskSet {
	fn drop(&mut self) {
		// Refuse and discard submissions; the running children drop with the set.
		// The queue is taken out and dropped after the lock releases: a queued
		// future may itself hold a `Tasks` clone, whose Drop takes this lock.
		let queued = {
			let mut state = self.state.lock();
			state.closed = true;
			std::mem::take(&mut state.queued)
		};
		drop(queued);
	}
}

/// Ready once nothing wants the track, or it closed; otherwise arm `waiter` for a change.
#[cfg(feature = "async")]
pub(crate) fn poll_unwanted(demand: &mut crate::track::Demand, waiter: &kio::Waiter) -> Poll<()> {
	// Drain the changes so the waiter stays registered, then read where demand settled.
	while let Poll::Ready(Ok(Some(_))) = demand.poll_wanted_changed(waiter) {}
	match demand.wanted() {
		None => Poll::Ready(()),
		Some(_) => Poll::Pending,
	}
}

#[cfg(all(test, feature = "async"))]
mod tests {
	use std::sync::{
		Arc,
		atomic::{AtomicUsize, Ordering},
	};

	use super::*;

	#[test]
	fn task_set_runs_nested_work_without_a_runtime() {
		let (tasks, task_set) = TaskSet::new();
		let completed = Arc::new(AtomicUsize::new(0));

		let nested_tasks = tasks.clone();
		let outer_completed = completed.clone();
		tasks.push(move |_: &kio::Waiter| {
			outer_completed.fetch_add(1, Ordering::SeqCst);

			let inner_completed = outer_completed.clone();
			nested_tasks.push(move |_: &kio::Waiter| {
				inner_completed.fetch_add(1, Ordering::SeqCst);
				Poll::Ready(())
			});
			Poll::Ready(())
		});
		drop(tasks);

		futures::executor::block_on(task_set.run());
		assert_eq!(completed.load(Ordering::SeqCst), 2);
	}
}
