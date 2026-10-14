//! Long-lived tasks the relay cannot serve without.
//!
//! A session's task may panic and end only that session (see
//! [`moq_tokio::panic`]). These tasks are different: an origin driver or a
//! certificate reload that dies leaves a relay that still accepts connections but
//! serves nothing, which is worse than exiting. So their panic is not contained;
//! [`Relay::run`](crate::Relay::run) returns it.

use std::{future::Future, panic::AssertUnwindSafe, task::Poll};

use futures::FutureExt;
use moq_net::kio;

/// Spawns long-lived tasks and reports the first one to panic.
#[derive(Clone, Default)]
pub(crate) struct Critical {
	/// The first panic, as `"<task> panicked: <message>"`.
	failed: kio::Producer<Option<String>>,
}

impl Critical {
	/// Run `task` on the ambient runtime, reporting a panic to [`Self::failed`].
	pub(crate) fn spawn(&self, name: &'static str, task: impl Future<Output = ()> + Send + 'static) {
		let failed = self.failed.clone();
		tokio::spawn(async move {
			let Err(payload) = AssertUnwindSafe(task).catch_unwind().await else {
				return;
			};
			let panic = moq_tokio::panic::message(payload.as_ref());
			tracing::error!(task = name, panic, "critical task panicked");
			if let Ok(mut failed) = failed.write() {
				failed.get_or_insert_with(|| format!("{name} panicked: {panic}"));
			}
		});
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
	async fn the_first_panic_is_reported() {
		let critical = Critical::default();
		critical.spawn("quiet", async {});
		critical.spawn("broken", async { panic!("injected") });

		let err = critical.failed().await;
		assert_eq!(err.to_string(), "broken panicked: injected");
	}
}
