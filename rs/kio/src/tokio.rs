//! kio integration for tokio.
//!
//! Behind the `tokio` feature. Wraps a [`tokio::time::Sleep`] so a `poll_*` function can
//! drive it against a [`Waiter`], keeping the rest of kio runtime-free.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use crate::Waiter;

/// A tokio sleep driven by kio's poll model.
///
/// Construct it once for a deadline (`Sleep::new(tokio::time::sleep_until(deadline))`), then
/// [`poll`](Self::poll) it against a [`Waiter`] each time your `poll_*` runs. Reading the
/// clock through `tokio::time` means a `tokio::time::pause()` test advances it in step.
pub struct Sleep {
	inner: Pin<Box<::tokio::time::Sleep>>,
}

impl Sleep {
	/// Wrap a tokio sleep future.
	pub fn new(sleep: ::tokio::time::Sleep) -> Self {
		Self { inner: Box::pin(sleep) }
	}

	/// Poll the sleep, registering `waiter` so the poll re-fires once it elapses.
	pub fn poll(&mut self, waiter: &Waiter) -> Poll<()> {
		let mut cx = Context::from_waker(waiter.waker());
		self.inner.as_mut().poll(&mut cx)
	}

	/// Wait until the sleep elapses.
	pub async fn wait(&mut self) {
		crate::wait(|waiter| self.poll(waiter)).await
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use ::tokio::time::Instant;
	use std::time::Duration;

	#[::tokio::test(start_paused = true)]
	async fn sleep_fires_at_its_deadline() {
		let deadline = Instant::now() + Duration::from_millis(50);
		let mut sleep = Sleep::new(::tokio::time::sleep_until(deadline));
		sleep.wait().await;
		assert!(Instant::now() >= deadline, "returned before the deadline");
	}
}
