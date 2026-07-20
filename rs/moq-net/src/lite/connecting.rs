//! Tracks a session's connection progress so `connect()` can block until it's done.
//!
//! Today "connecting" means every announce-prefix stream has received its initial
//! set (AnnounceInit for Lite01/02, AnnounceOk + N for Lite05). It's deliberately
//! generic so future work (e.g. extension negotiation) can register additional
//! steps that must finish before a session is considered connected.
//!
//! Backed by `kio`: each in-flight step holds a [`ConnectingProducer`], and the
//! session is connected once they've all been dropped (which closes the channel).
//! A step drops its producer when it finishes (or, on an early error, when it goes
//! out of scope), so a failed step can't hang `connect()`. Exposes both a synchronous
//! poll API and an async one; prefer `kio` over `tokio` primitives for new async state
//! so we keep both available.

use std::task::Poll;

use kio::{Consumer, Producer, Waiter};

/// Producer side: hold one per in-flight connection step (e.g. one per announce
/// prefix). Clone it to add a step; drop it to mark that step done. The session is
/// connected once every producer has been dropped.
///
/// The inner producer exists purely for its `Clone` (adds a step) and `Drop` (closes
/// the channel when the last one goes); it is never read, hence the allow.
#[derive(Clone)]
pub(super) struct ConnectingProducer(#[allow(dead_code)] Producer<()>);

/// Consumer side: returned by [`crate::lite::start`] and awaited by `connect()`.
pub(crate) struct Connecting(Consumer<()>);

impl Connecting {
	/// Create a producer/consumer pair. The consumer reports "connected" once every
	/// [`ConnectingProducer`] (the original plus any clones) has been dropped.
	pub(super) fn new() -> (ConnectingProducer, Self) {
		let producer = Producer::new(());
		let consumer = producer.consume();
		(ConnectingProducer(producer), Self(consumer))
	}

	/// Poll for connection completion: ready once every step's producer has dropped.
	pub(crate) fn poll_ready(&self, waiter: &Waiter) -> Poll<()> {
		self.0.poll_closed(waiter)
	}

	/// Await connection completion. Resolves immediately when there are no steps.
	pub(crate) async fn ready(&self) {
		kio::wait(|waiter| self.poll_ready(waiter)).await;
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn ready_once_every_producer_dropped() {
		let (producer, connecting) = Connecting::new();
		let noop = kio::Waiter::noop();
		let second = producer.clone();
		assert!(
			connecting.poll_ready(&noop).is_pending(),
			"not ready while a producer lives"
		);
		drop(producer);
		assert!(connecting.poll_ready(&noop).is_pending(), "still a clone outstanding");
		drop(second);
		assert!(
			connecting.poll_ready(&noop).is_ready(),
			"ready once the last producer drops"
		);
	}

	#[test]
	fn ready_when_sole_producer_dropped() {
		// No steps registered (a version with no initial-set boundary, or an empty
		// origin): dropping the only producer resolves immediately.
		let (producer, connecting) = Connecting::new();
		drop(producer);
		assert!(connecting.poll_ready(&kio::Waiter::noop()).is_ready());
	}
}
