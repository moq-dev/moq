//! Send-agnostic future boxing.
//!
//! Native transports (Quinn) are `Send`, so boxed futures use the usual
//! `Send`-bound `BoxFuture`. Browser WebTransport is `!Send`, so on wasm we box
//! without the bound via `LocalBoxFuture`. `MaybeSendBox` resolves to the right
//! one per target, and `.maybe_boxed()` picks `boxed()` vs `boxed_local()`.

use std::{future::Future, task::Poll};

use futures::{FutureExt, StreamExt, channel::mpsc, stream::FuturesUnordered};

#[cfg(not(target_family = "wasm"))]
pub(crate) type MaybeSendBox<'a, T> = futures::future::BoxFuture<'a, T>;
#[cfg(target_family = "wasm")]
pub(crate) type MaybeSendBox<'a, T> = futures::future::LocalBoxFuture<'a, T>;

#[cfg(not(target_family = "wasm"))]
pub(crate) trait MaybeBoxedExt<'a>: Future + Send + Sized + 'a {
	fn maybe_boxed(self) -> MaybeSendBox<'a, Self::Output> {
		self.boxed()
	}
}
#[cfg(not(target_family = "wasm"))]
impl<'a, F: Future + Send + 'a> MaybeBoxedExt<'a> for F {}

#[cfg(target_family = "wasm")]
pub(crate) trait MaybeBoxedExt<'a>: Future + Sized + 'a {
	fn maybe_boxed(self) -> MaybeSendBox<'a, Self::Output> {
		self.boxed_local()
	}
}
#[cfg(target_family = "wasm")]
impl<'a, F: Future + 'a> MaybeBoxedExt<'a> for F {}

/// The winner of a [`race2`] between two futures.
pub(crate) enum Race<A, B> {
	First(A),
	Second(B),
}

/// Await whichever future completes first, dropping the loser.
///
/// Biased: `a` is polled before `b` on every wake, so when both are ready the
/// first argument wins deterministically.
pub(crate) async fn race2<A: Future, B: Future>(a: A, b: B) -> Race<A::Output, B::Output> {
	let mut a = std::pin::pin!(a);
	let mut b = std::pin::pin!(b);
	kio::wait(move |waiter| {
		if let Poll::Ready(output) = waiter.poll_future(a.as_mut()) {
			return Poll::Ready(Race::First(output));
		}
		if let Poll::Ready(output) = waiter.poll_future(b.as_mut()) {
			return Poll::Ready(Race::Second(output));
		}
		Poll::Pending
	})
	.await
}

/// The winner of a [`race3`] between three futures.
pub(crate) enum Race3<A, B, C> {
	First(A),
	Second(B),
	Third(C),
}

/// Await whichever of three futures completes first, dropping the losers.
///
/// Biased like [`race2`]: earlier arguments win ties.
pub(crate) async fn race3<A: Future, B: Future, C: Future>(a: A, b: B, c: C) -> Race3<A::Output, B::Output, C::Output> {
	let mut a = std::pin::pin!(a);
	let mut b = std::pin::pin!(b);
	let mut c = std::pin::pin!(c);
	kio::wait(move |waiter| {
		if let Poll::Ready(output) = waiter.poll_future(a.as_mut()) {
			return Poll::Ready(Race3::First(output));
		}
		if let Poll::Ready(output) = waiter.poll_future(b.as_mut()) {
			return Poll::Ready(Race3::Second(output));
		}
		if let Poll::Ready(output) = waiter.poll_future(c.as_mut()) {
			return Poll::Ready(Race3::Third(output));
		}
		Poll::Pending
	})
	.await
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
	tx: mpsc::UnboundedSender<MaybeSendBox<'static, ()>>,
}

impl Tasks {
	/// Queue a future for polling by the associated [`TaskSet`].
	///
	/// A task submitted after the set is gone is dropped: the driver has torn down,
	/// so the task would have been cancelled anyway.
	pub fn push(&self, task: impl MaybeBoxedExt<'static, Output = ()>) {
		let _ = self.tx.unbounded_send(task.maybe_boxed());
	}
}

/// A dynamic set of child futures polled by its parent driver.
///
/// Unlike an executor spawn, dropping this value cancels every queued and active
/// child. [`Tasks`] handles may be cloned into those children so nested protocol
/// state can submit more work without choosing an async runtime.
pub(crate) struct TaskSet {
	rx: mpsc::UnboundedReceiver<MaybeSendBox<'static, ()>>,
	active: FuturesUnordered<MaybeSendBox<'static, ()>>,
}

impl TaskSet {
	/// Create a task submission handle and its driver-owned receiver.
	pub fn new() -> (Tasks, Self) {
		let (tx, rx) = mpsc::unbounded();
		(
			Tasks { tx },
			Self {
				rx,
				active: FuturesUnordered::new(),
			},
		)
	}

	/// Create a set that only its owner can push to, for a loop that accepts streams
	/// and serves each one as a child.
	pub fn owned() -> Self {
		let (_, set) = Self::new();
		set
	}

	/// Queue a future for polling by this set.
	pub fn push(&mut self, task: impl MaybeBoxedExt<'static, Output = ()>) {
		self.active.push(task.maybe_boxed());
	}

	/// Poll every queued submission into the active set, then poll the children.
	///
	/// `Ready` once every submission handle is dropped and all children finish;
	/// an owner-only set ([`Self::owned`]) reaches it when its children drain.
	pub fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		let mut cx = std::task::Context::from_waker(waiter.waker());

		let mut submissions_done = false;
		loop {
			match self.rx.poll_next_unpin(&mut cx) {
				Poll::Ready(Some(task)) => self.active.push(task),
				Poll::Ready(None) => {
					submissions_done = true;
					break;
				}
				Poll::Pending => break,
			}
		}

		// Finished children just drop; `Ready(None)` means the set is empty, which
		// only ends the poll once no new submissions can arrive.
		loop {
			match self.active.poll_next_unpin(&mut cx) {
				Poll::Ready(Some(())) => {}
				Poll::Ready(None) if submissions_done => return Poll::Ready(()),
				Poll::Ready(None) | Poll::Pending => return Poll::Pending,
			}
		}
	}

	/// Poll every child while awaiting `future`, returning its output.
	///
	/// `future` is polled in place rather than cancelled and rebuilt each time a
	/// child finishes, so an accept loop can serve its children without assuming the
	/// transport's `accept_*` is cancel-safe (`web_transport_trait` promises nothing).
	pub async fn drive<F: Future>(&mut self, future: F) -> F::Output {
		let mut future = std::pin::pin!(future);
		kio::wait(|waiter| {
			if let Poll::Ready(output) = waiter.poll_future(future.as_mut()) {
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
	/// active children finish.
	pub async fn run(mut self) {
		kio::wait(|waiter| self.poll(waiter)).await
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

		// The driven future only resolves after the child ran, proving drive()
		// interleaves both without an executor.
		let gate = completed.clone();
		let output = futures::executor::block_on(set.drive(std::future::poll_fn(move |cx| {
			if gate.load(Ordering::SeqCst) == 1 {
				std::task::Poll::Ready(42)
			} else {
				cx.waker().wake_by_ref();
				std::task::Poll::Pending
			}
		})));

		assert_eq!(output, 42);
	}
}
