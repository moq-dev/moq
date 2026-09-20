//! Run MoQ drivers with the runtime clock and timer.

use std::task::Poll;

/// Run an explicit-time driver on the runtime's clock and timer.
pub async fn run<D: moq_net::time::Driver>(mut driver: D) -> D::Output {
	let mut timer = None;
	moq_net::kio::wait(|waiter| {
		loop {
			let now = web_async::time::Instant::now();
			if let Poll::Ready(result) = driver.poll(now, waiter) {
				return Poll::Ready(result);
			}
			let Some(at) = driver.timeout() else {
				timer = None;
				return Poll::Pending;
			};
			let sleep = timer.get_or_insert_with(|| Box::pin(web_async::time::sleep_until(at)));
			if sleep.deadline() != at {
				sleep.as_mut().reset(at);
			}
			if waiter.poll_future(sleep.as_mut()).is_pending() {
				return Poll::Pending;
			}
		}
	})
	.await
}
