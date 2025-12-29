use std::fmt;
use std::ops::Deref;

use tokio::sync::watch;

use crate::{Error, Time};

#[derive(Debug, Default)]
struct State {
	max_latency: Time,
	max_timestamp: Time,
	max_group: u64,
}

impl State {
	fn new(max_latency: Time) -> Self {
		Self {
			max_latency,
			max_timestamp: Time::ZERO,
			max_group: 0,
		}
	}

	fn create_frame(&mut self, group: u64, timestamp: Time) -> Result<bool, Error> {
		let new_group = group > self.max_group;
		let new_timestamp = timestamp > self.max_timestamp;

		if new_group {
			self.max_group = group;
		}

		if new_timestamp {
			self.max_timestamp = timestamp;
		}

		if !new_group && !new_timestamp {
			if timestamp + self.max_latency <= self.max_timestamp {
				return Err(Error::Expired);
			}
		}

		Ok(new_group || new_timestamp)
	}
}

// TODO Also add a way to expire when too many bytes are cached.
#[derive(Clone, Default)]
pub struct ExpiresProducer {
	state: watch::Sender<State>,
}

impl fmt::Debug for ExpiresProducer {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("State")
			.field("state", &self.state.borrow().deref())
			.finish()
	}
}

impl ExpiresProducer {
	pub fn new(max_latency: Time) -> Self {
		Self {
			state: watch::Sender::new(State::new(max_latency)),
		}
	}

	pub fn create_frame(&self, group: u64, timestamp: Time) -> Result<(), Error> {
		let mut result = Ok(false);
		self.state.send_if_modified(|state| {
			result = state.create_frame(group, timestamp);
			result.as_ref().is_ok_and(|modify| *modify)
		});
		result.map(|_| ())
	}

	pub fn set_max_latency(&self, max_latency: Time) {
		self.state.send_modify(|state| state.max_latency = max_latency);
	}

	pub fn consume(&self) -> ExpiresConsumer {
		ExpiresConsumer {
			state: self.state.subscribe(),
		}
	}
}

#[derive(Clone)]
pub struct ExpiresConsumer {
	state: watch::Receiver<State>,
}

impl fmt::Debug for ExpiresConsumer {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("ExpiresConsumer")
			.field("state", &self.state.borrow().deref())
			.finish()
	}
}

impl ExpiresConsumer {
	// Blocks until the given group/max_timestamp is expired.
	pub async fn expired(&mut self, group: u64, max_timestamp: Time) -> Error {
		match self
			.state
			.wait_for(|state| state.max_group >= group && state.max_timestamp + state.max_latency >= max_timestamp)
			.await
		{
			Ok(_) => Error::Expired,
			Err(_) => Error::Cancel,
		}
	}
}
