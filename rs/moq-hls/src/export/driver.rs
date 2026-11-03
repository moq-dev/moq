//! The export's background work, as `kio` tasks one [`Driver`] polls.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;

/// Runs a [`Broadcaster`](super::Broadcaster)'s background work: its catalog watcher, each
/// rendition's record watcher, and the init builds.
///
/// Poll it, or [`run`](Self::run) it, for as long as the broadcaster serves. It finishes once
/// the broadcaster and every rendition it created have dropped and their tasks ended, so a
/// recording cursor that outlives the broadcaster keeps its rendition's records flowing.
/// Dropping it early stops all of that work.
#[must_use = "the broadcaster does nothing until its driver runs"]
pub struct Driver {
	/// The catalog watcher, polled before the other tasks in every pass: a reconfigure must
	/// retire its rendition before that rendition's watcher reads records published after it.
	lead: Option<Job>,
	inbox: kio::Queue<Job>,
	tasks: kio::Tasks<Job>,
	/// The inbox closed: every owner that could start a task has dropped.
	closed: bool,
}

impl Driver {
	/// Advance every task, ready once nothing is left to run.
	pub fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		if let Some(lead) = &mut self.lead
			&& kio::Task::poll(lead, waiter).is_ready()
		{
			self.lead = None;
		}
		while !self.closed {
			match self.inbox.poll_pop(waiter) {
				Poll::Ready(Ok(job)) => self.tasks.push(job),
				Poll::Ready(Err(kio::Closed)) => self.closed = true,
				Poll::Pending => break,
			}
		}
		match self.tasks.poll(waiter) {
			Poll::Ready(()) if self.closed && self.lead.is_none() => Poll::Ready(()),
			_ => Poll::Pending,
		}
	}

	/// Run `task` ahead of every other task (see [`lead`](Self::lead)), until it finishes or the
	/// returned [`Owner`] drops.
	pub(crate) fn lead(&mut self, task: impl Future<Output = ()> + Send + 'static) -> Owner {
		let (job, owner) = Job::new(task);
		self.lead = Some(job);
		owner
	}

	/// [`poll`](Self::poll) until it is ready.
	pub async fn run(mut self) {
		kio::wait(|waiter| self.poll(waiter)).await
	}
}

impl Drop for Driver {
	fn drop(&mut self) {
		// Later starts are refused instead of queueing work nothing will run, and queued ones drop
		// now, releasing what they hold (such as an init build's lock).
		self.inbox.close();
		while let Ok(Some(_)) = self.inbox.try_pop() {}
	}
}

/// Starts tasks on its [`Driver`]. Clones share it; the driver's inbox closes with the last.
#[derive(Clone)]
pub(crate) struct Spawner {
	inbox: Arc<Inbox>,
}

struct Inbox(kio::Queue<Job>);

impl Drop for Inbox {
	fn drop(&mut self) {
		self.0.close();
	}
}

impl Spawner {
	/// A spawner and the driver that runs what it starts.
	pub fn new() -> (Self, Driver) {
		let inbox = kio::Queue::new();
		let driver = Driver {
			lead: None,
			inbox: inbox.clone(),
			tasks: kio::Tasks::new(),
			closed: false,
		};
		(
			Self {
				inbox: Arc::new(Inbox(inbox)),
			},
			driver,
		)
	}

	/// Run `task` on the driver until it finishes or the returned [`Owner`] drops.
	pub fn spawn(&self, task: impl Future<Output = ()> + Send + 'static) -> Owner {
		let (job, owner) = Job::new(task);
		// Refused once the driver is gone, which drops the task unrun.
		let _ = self.inbox.0.try_push(job);
		owner
	}
}

/// Ends its task when dropped.
pub(crate) struct Owner(#[expect(dead_code, reason = "held only to close on drop")] kio::Producer<()>);

/// A task and the [`Owner`] that ends it.
struct Job {
	owner: kio::Consumer<()>,
	task: Pin<Box<dyn Future<Output = ()> + Send>>,
}

impl Job {
	fn new(task: impl Future<Output = ()> + Send + 'static) -> (Self, Owner) {
		let owner = kio::Producer::new(());
		let job = Self {
			owner: owner.consume(),
			task: Box::pin(task),
		};
		(job, Owner(owner))
	}
}

impl kio::Task for Job {
	type Output = ();

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		if self.owner.poll_closed(waiter).is_ready() {
			return Poll::Ready(());
		}
		waiter.poll_future(self.task.as_mut())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	// The driver finishes once every spawner and task is gone, and an owner's drop ends its task
	// without waiting for the task to finish on its own.
	#[test]
	fn tasks_end_with_their_owner() {
		let (spawner, mut driver) = Spawner::new();
		let waiter = kio::Waiter::noop();
		let forever = spawner.spawn(std::future::pending());
		let lead = driver.lead(std::future::pending());
		drop(spawner);
		assert!(driver.poll(&waiter).is_pending(), "the owned tasks still run");
		drop(lead);
		assert!(driver.poll(&waiter).is_pending(), "the owned task still runs");
		drop(forever);
		assert!(driver.poll(&waiter).is_ready(), "the task ended with its owner");
	}

	// Dropping the driver drops the tasks it never polled, so nothing they hold leaks.
	#[test]
	fn a_dropped_driver_drops_queued_tasks() {
		let (spawner, driver) = Spawner::new();
		let held = Arc::new(());
		let _owner = spawner.spawn({
			let held = held.clone();
			async move { drop(held) }
		});
		drop(driver);
		assert_eq!(Arc::strong_count(&held), 1, "the queued task was dropped");
	}
}
