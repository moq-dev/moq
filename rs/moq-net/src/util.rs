//! Send-agnostic future boxing and the driver-owned task set.
//!
//! Native transports (Quinn) are `Send`, so boxed futures and tasks carry the
//! `Send` bound. Browser WebTransport is `!Send`, so on wasm we box without it.
//! `MaybeSendBox` / `MaybeSendTask` resolve to the right form per target, and
//! `.maybe_boxed()` / [`poll_task`] pick the matching constructor.

use std::{collections::VecDeque, future::Future, pin::Pin, task::Poll};

#[cfg(not(target_family = "wasm"))]
pub(crate) type MaybeSendBox<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
#[cfg(target_family = "wasm")]
pub(crate) type MaybeSendBox<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// A boxed [`kio::Tasks`] task: hand-rolled machines and adapted futures alike.
#[cfg(not(target_family = "wasm"))]
pub(crate) type MaybeSendTask = Box<dyn FnMut(&kio::Waiter) -> Poll<()> + Send>;
#[cfg(target_family = "wasm")]
pub(crate) type MaybeSendTask = Box<dyn FnMut(&kio::Waiter) -> Poll<()>>;

#[cfg(not(target_family = "wasm"))]
pub(crate) trait MaybeBoxedExt<'a>: Future + Send + Sized + 'a {
	fn maybe_boxed(self) -> MaybeSendBox<'a, Self::Output> {
		Box::pin(self)
	}
}
#[cfg(not(target_family = "wasm"))]
impl<'a, F: Future + Send + 'a> MaybeBoxedExt<'a> for F {}

#[cfg(target_family = "wasm")]
pub(crate) trait MaybeBoxedExt<'a>: Future + Sized + 'a {
	fn maybe_boxed(self) -> MaybeSendBox<'a, Self::Output> {
		Box::pin(self)
	}
}
#[cfg(target_family = "wasm")]
impl<'a, F: Future + 'a> MaybeBoxedExt<'a> for F {}

/// Box a poll closure into a [`MaybeSendTask`], for pushing a hand-rolled
/// machine into a [`kio::Tasks`] stored behind the type-erased alias.
#[cfg(not(target_family = "wasm"))]
pub(crate) fn poll_task(task: impl FnMut(&kio::Waiter) -> Poll<()> + Send + 'static) -> MaybeSendTask {
	Box::new(task)
}
#[cfg(target_family = "wasm")]
pub(crate) fn poll_task(task: impl FnMut(&kio::Waiter) -> Poll<()> + 'static) -> MaybeSendTask {
	Box::new(task)
}

/// Adapt a boxed future into a [`kio::Tasks`] task.
fn future_task(mut task: MaybeSendBox<'static, ()>) -> MaybeSendTask {
	Box::new(move |waiter| waiter.poll_future(task.as_mut()))
}

/// Resolve with the error of a fallible future, parking forever on success.
///
/// The building block for racing "watchdog" futures whose clean completion should
/// not end the race (only their failure should).
pub(crate) async fn err_only<E>(fut: impl Future<Output = Result<(), E>>) -> E {
	match fut.await {
		Err(err) => err,
		Ok(()) => std::future::pending().await,
	}
}

/// Cloneable handle for submitting futures to a driver-owned [`TaskSet`].
pub(crate) struct Tasks {
	state: kio::Shared<Submissions>,
}

/// The submission queue between [`Tasks`] handles and their [`TaskSet`],
/// following kio's convention for [`kio::Shared`] liveness: the handle count
/// and the set's closure are plain fields maintained under the same lock that
/// queues the work.
#[derive(Default)]
struct Submissions {
	queued: VecDeque<MaybeSendBox<'static, ()>>,
	/// Live [`Tasks`] handles; the set finishes once this reaches zero and the
	/// remaining work drains.
	senders: usize,
	/// The set dropped: submissions are discarded, as the driver that would
	/// have polled them has torn down.
	closed: bool,
}

impl Tasks {
	/// Queue a future for polling by the associated [`TaskSet`].
	///
	/// A task submitted after the set is gone is dropped: the driver has torn down,
	/// so the task would have been cancelled anyway.
	pub fn push(&self, task: impl MaybeBoxedExt<'static, Output = ()>) {
		let mut state = self.state.lock();
		if !state.closed {
			state.queued.push_back(task.maybe_boxed());
		}
	}
}

impl Clone for Tasks {
	fn clone(&self) -> Self {
		self.state.lock().senders += 1;
		Self {
			state: self.state.clone(),
		}
	}
}

impl Drop for Tasks {
	fn drop(&mut self) {
		// The mutation wakes a parked set, which may be what lets it finish.
		self.state.lock().senders -= 1;
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
	active: kio::Tasks<MaybeSendTask>,
}

impl TaskSet {
	/// Create a task submission handle and its driver-owned set.
	pub fn new() -> (Tasks, Self) {
		let state = kio::Shared::new(Submissions {
			queued: VecDeque::new(),
			senders: 1,
			closed: false,
		});
		let tasks = Tasks { state: state.clone() };
		(
			tasks,
			Self {
				state,
				active: kio::Tasks::new(),
			},
		)
	}

	/// Create a set that only its owner can push to, for a loop that accepts streams
	/// and serves each one as a child.
	pub fn owned() -> Self {
		Self {
			state: kio::Shared::default(),
			active: kio::Tasks::new(),
		}
	}

	/// Queue a future for polling by this set.
	pub fn push(&mut self, task: impl MaybeBoxedExt<'static, Output = ()>) {
		self.active.push(future_task(task.maybe_boxed()));
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
		while let Poll::Ready(mut state) = self.state.poll(waiter, |state| {
			if state.queued.is_empty() && state.senders > 0 {
				Poll::Pending
			} else {
				Poll::Ready(())
			}
		}) {
			while let Some(task) = state.queued.pop_front() {
				self.active.push(future_task(task));
			}
			// No handles left: nothing can ever submit again, so no
			// registration is needed.
			if state.senders == 0 {
				submissions_done = true;
				break;
			}
		}

		match self.active.poll(waiter) {
			Poll::Ready(()) if submissions_done => Poll::Ready(()),
			_ => Poll::Pending,
		}
	}

	/// Poll every child while polling `f`, returning its output.
	///
	/// `f` is a poll function rather than a future, so an accept loop polls the
	/// transport directly and there is no accept future to cancel or rebuild when
	/// a child finishes.
	pub async fn drive<T>(&mut self, mut f: impl FnMut(&kio::Waiter) -> Poll<T> + Unpin) -> T {
		kio::wait(|waiter| {
			if let Poll::Ready(output) = f(waiter) {
				return Poll::Ready(output);
			}
			// The children never end the drive; a `Ready` here just means they're
			// drained until the next submission.
			let _ = self.poll(waiter);
			Poll::Pending
		})
		.await
	}

	/// Drive submitted children until every submission handle is dropped and all
	/// active children finish. The async form of [`Self::poll`], for callers with
	/// nothing else to poll.
	#[cfg(test)]
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

#[cfg(test)]
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
		tasks.push(async move {
			outer_completed.fetch_add(1, Ordering::SeqCst);

			let inner_completed = outer_completed.clone();
			nested_tasks.push(async move {
				inner_completed.fetch_add(1, Ordering::SeqCst);
			});
		});
		drop(tasks);

		futures::executor::block_on(task_set.run());
		assert_eq!(completed.load(Ordering::SeqCst), 2);
	}

	#[test]
	fn drive_polls_children_alongside_the_accept_future() {
		let (tasks, mut set) = TaskSet::new();
		let completed = Arc::new(AtomicUsize::new(0));

		let child_completed = completed.clone();
		tasks.push(async move {
			child_completed.fetch_add(1, Ordering::SeqCst);
		});

		// The driven poll only resolves after the child ran, proving drive()
		// interleaves both without an executor.
		let gate = completed.clone();
		let output = futures::executor::block_on(set.drive(move |waiter| {
			if gate.load(Ordering::SeqCst) == 1 {
				std::task::Poll::Ready(42)
			} else {
				waiter.waker().wake_by_ref();
				std::task::Poll::Pending
			}
		}));

		assert_eq!(output, 42);
	}
}
