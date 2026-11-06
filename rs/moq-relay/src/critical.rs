//! Long-lived tasks the relay cannot serve without.
//!
//! A session's task may panic and end only that session (see
//! [`moq_tokio::panic`]). These tasks are different: an origin driver that dies
//! leaves a relay that still accepts connections but serves nothing, which is worse
//! than exiting. So their panic is not contained; [`Relay::run`](crate::Relay::run)
//! returns it.
//!
//! [`Child`] guards such a task polled as a [`kio::Tasks`] child by its owner, and
//! [`Critical`] collects the first failure.

use std::{future::Future, panic::AssertUnwindSafe, task::Poll};

use futures::{FutureExt, future::BoxFuture};
use moq_net::kio;

/// A named future polled as a [`kio::Tasks`] child, whose panic becomes an error
/// instead of unwinding through the owner's poll.
pub(crate) struct Child<T> {
	name: &'static str,
	future: BoxFuture<'static, T>,
}

impl<T> Child<T> {
	pub(crate) fn new(name: &'static str, future: impl Future<Output = T> + Send + 'static) -> Self {
		Self {
			name,
			future: future.boxed(),
		}
	}
}

impl<T> kio::Task for Child<T> {
	/// The future's output, or `"<name> panicked: <message>"` once it panics.
	type Output = anyhow::Result<T>;

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Self::Output> {
		match std::panic::catch_unwind(AssertUnwindSafe(|| waiter.poll_future(self.future.as_mut()))) {
			Ok(poll) => poll.map(Ok),
			Err(payload) => Poll::Ready(Err(panicked(self.name, payload.as_ref()))),
		}
	}
}

/// Log a critical task's panic and describe it as `"<task> panicked: <message>"`.
fn panicked(name: &'static str, payload: &(dyn std::any::Any + Send)) -> anyhow::Error {
	let panic = moq_tokio::panic::message(payload);
	tracing::error!(task = name, panic, "critical task panicked");
	anyhow::anyhow!("{name} panicked: {panic}")
}

/// Reports the first long-lived task to fail.
#[derive(Clone, Default)]
pub(crate) struct Critical {
	/// The first failure, such as `"<task> panicked: <message>"`.
	failed: kio::Producer<Option<String>>,
}

impl Critical {
	/// Report a failure from a task polled elsewhere.
	pub(crate) fn fail(&self, err: anyhow::Error) {
		if let Ok(mut failed) = self.failed.write() {
			failed.get_or_insert_with(|| err.to_string());
		}
	}

	/// Resolve once any task panics, with that panic.
	pub(crate) async fn failed(&self) -> anyhow::Error {
		let failed = self.failed.consume();
		// Never closes: this handle holds a producer.
		let panic = failed
			.wait(|failed| match &**failed {
				Some(panic) => Poll::Ready(panic.clone()),
				None => Poll::Pending,
			})
			.await;
		match panic {
			Ok(panic) => anyhow::anyhow!(panic),
			Err(_) => std::future::pending().await,
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn the_first_failure_is_reported() {
		let critical = Critical::default();
		critical.fail(anyhow::anyhow!("first"));
		critical.fail(anyhow::anyhow!("second"));

		let err = critical.failed().await;
		assert_eq!(err.to_string(), "first");
	}

	/// A child's panic resolves it with the error instead of unwinding the owner.
	#[test]
	fn a_child_panic_becomes_its_output() {
		let mut tasks = kio::Tasks::new();
		tasks.push(Child::new("quiet", async {}));
		tasks.push(Child::new("broken", async { panic!("injected") }));

		let mut errors = Vec::new();
		let poll = tasks.poll_each(&kio::Waiter::noop(), |_, res| {
			if let Err(err) = res {
				errors.push(err.to_string());
			}
		});
		assert!(poll.is_ready(), "both children finished");
		assert_eq!(errors, ["broken panicked: injected"]);
	}
}
