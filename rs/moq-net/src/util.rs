//! Send-agnostic future boxing.
//!
//! Native transports (Quinn) are `Send`, so boxed futures use the usual
//! `Send`-bound `BoxFuture`. Browser WebTransport is `!Send`, so on wasm we box
//! without the bound via `LocalBoxFuture`. `MaybeSendBox` resolves to the right
//! one per target, and `.maybe_boxed()` picks `boxed()` vs `boxed_local()`.

use std::future::Future;

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

	/// Poll every child while awaiting `future`, returning its output.
	///
	/// `future` is polled in place rather than cancelled and rebuilt each time a
	/// child finishes, so an accept loop can serve its children without assuming the
	/// transport's `accept_*` is cancel-safe (`web_transport_trait` promises nothing).
	pub async fn drive<F: Future>(&mut self, future: F) -> F::Output {
		tokio::pin!(future);

		loop {
			tokio::select! {
				output = &mut future => return output,
				Some(task) = self.rx.next() => self.active.push(task),
				Some(()) = self.active.next(), if !self.active.is_empty() => {},
			}
		}
	}

	/// Drive submitted children until every submission handle is dropped and all
	/// active children finish.
	pub async fn run(mut self) {
		loop {
			tokio::select! {
				Some(task) = self.rx.next() => self.active.push(task),
				Some(()) = self.active.next(), if !self.active.is_empty() => {},
				else => return,
			}
		}
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
}
