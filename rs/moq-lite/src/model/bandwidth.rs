//! Bandwidth estimation, split into a [BandwidthProducer] and [BandwidthConsumer] handle.
//!
//! A [BandwidthProducer] is used to set the current estimated bitrate, notifying consumers.
//! A [BandwidthConsumer] can read the current estimate and wait for changes.

use std::task::Poll;

use crate::{Error, Result};

#[derive(Default)]
struct State {
	bitrate: Option<u64>,
	abort: Option<Error>,
}

/// Produces bandwidth estimates, notifying consumers when the value changes.
#[derive(Clone)]
pub struct BandwidthProducer {
	state: conducer::Producer<State>,
}

impl BandwidthProducer {
	pub fn new() -> Self {
		Self {
			state: conducer::Producer::default(),
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
	pub fn consume(&self) -> BandwidthConsumer {
		BandwidthConsumer {
			state: self.state.consume(),
			last: None,
		}
	}

	/// Close the producer with an error, notifying all consumers.
	pub fn close(&self, err: Error) -> Result<()> {
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
		self.state
			.unused()
			.await
			.map_err(|r| r.abort.clone().unwrap_or(Error::Dropped))
	}

	/// Block until there is at least one active consumer.
	pub async fn used(&self) -> Result<()> {
		self.state
			.used()
			.await
			.map_err(|r| r.abort.clone().unwrap_or(Error::Dropped))
	}

	fn modify(&self) -> Result<conducer::Mut<'_, State>> {
		self.state
			.write()
			.map_err(|r| r.abort.clone().unwrap_or(Error::Dropped))
	}
}

impl Default for BandwidthProducer {
	fn default() -> Self {
		Self::new()
	}
}

/// Consumes bandwidth estimates, allowing reads and async change notifications.
#[derive(Clone)]
pub struct BandwidthConsumer {
	state: conducer::Consumer<State>,
	last: Option<u64>,
}

impl BandwidthConsumer {
	/// Get the current bandwidth estimate synchronously.
	pub fn peek(&self) -> Option<u64> {
		self.state.read().bitrate
	}

	/// Poll for a bandwidth change without blocking.
	pub fn poll_changed(&mut self, waiter: &conducer::Waiter) -> Poll<Option<u64>> {
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
				Poll::Ready(bitrate)
			}
			// Channel closed
			Poll::Ready(Err(_)) => Poll::Ready(None),
			Poll::Pending => Poll::Pending,
		}
	}

	/// Block until the bandwidth estimate changes. Returns the new value.
	/// Returns `None` if the producer is dropped or the estimate is unavailable.
	pub async fn changed(&mut self) -> Option<u64> {
		conducer::wait(|waiter| self.poll_changed(waiter)).await
	}
}
