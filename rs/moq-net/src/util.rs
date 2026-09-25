//! Send-agnostic future boxing and the driver-owned task set.
//!
//! Native transports (Quinn) are `Send`, so boxed futures and tasks carry the
//! `Send` bound. Browser WebTransport is `!Send`, so on wasm we box without it.
//! `MaybeSendBox` / `MaybeSendTask` resolve to the right form per target, and
//! `.maybe_boxed()` / `future_task` pick the matching constructor.

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
#[derive(Clone)]
pub(crate) struct Tasks {
	state: kio::Shared<Submissions>,
	/// The set's lifetime: it finishes once every owning handle, [`Tasks`] or
	/// [`Keepalive`], is gone. A kio token rather than a count under the queue's
	/// lock, so a claim on the lifetime is `Send` even where the queued futures
	/// are not (wasm).
	alive: kio::Producer<()>,
}

/// A claim on a [`TaskSet`]'s lifetime with no submission queue: what a
/// broadcast published on an origin holds, so the driver outlives the producer
/// handles a session was given and dropped.
pub(crate) struct Keepalive {
	_alive: kio::Producer<()>,
}

/// The submission queue between [`Tasks`] handles and their [`TaskSet`].
#[derive(Default)]
struct Submissions {
	queued: VecDeque<MaybeSendBox<'static, ()>>,
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
	/// Queue a future for polling by the associated [`TaskSet`], dropped if the
	/// set (or every owning handle) is already gone.
	pub fn push(&self, task: impl MaybeBoxedExt<'static, Output = ()>) {
		let mut state = self.state.lock();
		if !state.closed && !self.alive.is_closed() {
			state.queued.push_back(task.maybe_boxed());
		}
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
				self.active.push(future_task(task));
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
