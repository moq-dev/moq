//! Bandwidth estimation, split into a [Producer] and [Consumer] handle.
//!
//! A [Producer] is used to set the current estimated bitrate, notifying consumers.
//! A [Consumer] can read the current estimate and wait for changes.
//!
//! What a sender does with an estimate is policy and lives with the sender, not
//! here: see `moq_video::encode::rate` for the encoder's.

use std::task::Poll;

use crate::{Error, Result};

#[derive(Default)]
struct State {
	bitrate: Option<u64>,
	abort: Option<Error>,
}

/// Produces bandwidth estimates, notifying consumers when the value changes.
#[derive(Clone)]
pub struct Producer {
	state: kio::Producer<State>,
}

impl Producer {
	/// Create a fresh producer with no current estimate.
	pub fn new() -> Self {
		Self {
			state: kio::Producer::default(),
		}
	}

	/// Set the current bandwidth estimate in bits per second.
	pub fn set(&self, bitrate: Option<u64>) -> Result<()> {
		let mut state = self.modify()?;
		if state.bitrate != bitrate {
			state.bitrate = bitrate;
		}
		Ok(())
	}

	/// Create a new consumer for the bandwidth estimate.
	pub fn consume(&self) -> Consumer {
		Consumer {
			state: self.state.consume(),
			last: None,
		}
	}

	/// Close the producer with an error, notifying all consumers.
	pub fn abort(&self, err: Error) -> Result<()> {
		let mut state = self.modify()?;
		state.abort = Some(err);
		state.close();
		Ok(())
	}

	/// Block until the channel is closed.
	pub async fn closed(&self) {
		self.state.closed().await
	}

	/// Block until there are no active consumers.
	pub async fn unused(&self) -> Result<()> {
		kio::wait(|waiter| self.poll_unused(waiter)).await
	}

	/// Poll until there are no active consumers. Errors if the channel closes first.
	pub fn poll_unused(&self, waiter: &kio::Waiter) -> Poll<Result<()>> {
		self.state.poll_unused(waiter).map(|used| match used {
			Some(()) => Ok(()),
			None => Err(self.close_error()),
		})
	}

	/// Block until there is at least one active consumer.
	pub async fn used(&self) -> Result<()> {
		kio::wait(|waiter| self.poll_used(waiter)).await
	}

	/// Poll until at least one active consumer exists. Errors if the channel closes first.
	pub fn poll_used(&self, waiter: &kio::Waiter) -> Poll<Result<()>> {
		self.state.poll_used(waiter).map(|used| match used {
			Some(()) => Ok(()),
			None => Err(self.close_error()),
		})
	}

	fn modify(&self) -> Result<kio::Mut<'_, State>> {
		self.state
			.write()
			.map_err(|r| r.abort.clone().unwrap_or(Error::Dropped))
	}

	/// The close error, once the channel is closed.
	fn close_error(&self) -> Error {
		self.state.read().abort.clone().unwrap_or(Error::Dropped)
	}
}

impl Default for Producer {
	fn default() -> Self {
		Self::new()
	}
}

/// Consumes bandwidth estimates, allowing reads and async change notifications.
#[derive(Clone)]
pub struct Consumer {
	state: kio::Consumer<State>,
	last: Option<u64>,
}

impl Consumer {
	/// Get the current bandwidth estimate synchronously.
	pub fn peek(&self) -> Option<u64> {
		self.state.read().bitrate
	}

	/// Poll for a bandwidth change without blocking.
	///
	/// `Ok(None)` means the estimate is unavailable *for now*: the backend
	/// stopped reporting one, or the handle spans reconnects and is between
	/// sessions. `Err` means the producer is gone and no further change will ever
	/// arrive. They're distinct because a caller holds its current rate for the
	/// first and stops watching for the second.
	///
	/// A backend with no bandwidth estimation at all yields no [Consumer] in the
	/// first place, so that case never reaches here.
	pub fn poll_changed(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<u64>>> {
		let last = self.last;

		match self.state.poll(waiter, |state| {
			if state.bitrate != last {
				Poll::Ready(state.bitrate)
			} else {
				Poll::Pending
			}
		}) {
			Poll::Ready(Ok(bitrate)) => {
				self.last = bitrate;
				Poll::Ready(Ok(bitrate))
			}
			// Closed, and the value hasn't moved since the last read: report it as
			// terminal. Collapsing this into `Ok(None)` would be indistinguishable
			// from a live-but-unavailable estimate, and since a closed channel is
			// always immediately ready, a `select!` over it would spin forever.
			Poll::Ready(Err(state)) => Poll::Ready(Err(state.abort.clone().unwrap_or(Error::Dropped))),
			Poll::Pending => Poll::Pending,
		}
	}

	/// Block until the bandwidth estimate changes, returning the new value, or
	/// `None` when the estimate has become unavailable.
	///
	/// # Errors
	///
	/// Returns an error once the producer is closed or dropped, so a caller can
	/// stop watching. See [`poll_changed`](Self::poll_changed).
	pub async fn changed(&mut self) -> Result<Option<u64>> {
		kio::wait(|waiter| self.poll_changed(waiter)).await
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// An unavailable estimate and a dead producer must not look alike: a caller
	/// holds its rate for the former and stops watching for the latter.
	/// Reporting closure as `Ok(None)` would spin any `select!` over `changed()`,
	/// because a closed channel is always immediately ready.
	#[tokio::test]
	async fn closed_is_distinct_from_unavailable() {
		let producer = Producer::new();
		let mut consumer = producer.consume();

		producer.set(Some(1_000_000)).unwrap();
		assert_eq!(consumer.changed().await.unwrap(), Some(1_000_000));

		// Live, but the estimate went away (e.g. disconnected): still watchable.
		producer.set(None).unwrap();
		assert_eq!(consumer.changed().await.unwrap(), None);

		// Gone for good.
		producer.abort(Error::Cancel).unwrap();
		assert!(consumer.changed().await.is_err());
		// And it stays terminal rather than flapping back to a value.
		assert!(consumer.changed().await.is_err());
	}
}
