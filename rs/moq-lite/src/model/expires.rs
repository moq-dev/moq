use std::fmt;
use std::ops::Deref;

use tokio::sync::watch;

use crate::{DeliveryConsumer, Error, Time};

#[derive(Debug, Default)]
struct State {
	max_instant: Time,
	max_group: u64,
}

impl State {
	#[allow(dead_code)]
	fn create_frame(&mut self, group: u64, instant: Time, max_latency: Time) -> Result<bool, Error> {
		let new_group = group > self.max_group;
		let new_instant = instant > self.max_instant;

		if new_group {
			self.max_group = group;
		}

		if new_instant {
			self.max_instant = instant;
		}

		if !new_group && !new_instant && instant + max_latency <= self.max_instant {
			return Err(Error::Expired);
		}

		Ok(new_group || new_instant)
	}
}

// TODO Also add a way to expire when too many bytes are cached.
#[derive(Clone)]
pub struct ExpiresProducer {
	state: watch::Sender<State>,

	// TODO expire when max_latency changes without waiting for a new frame/group.
	delivery: DeliveryConsumer,
}

impl fmt::Debug for ExpiresProducer {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("State")
			.field("state", &self.state.borrow().deref())
			.finish()
	}
}

impl ExpiresProducer {
	pub fn new(delivery: DeliveryConsumer) -> Self {
		Self {
			state: Default::default(),
			delivery,
		}
	}

	#[allow(dead_code)]
	pub(super) fn create_frame(&self, group: u64, instant: Time) -> Result<(), Error> {
		let max_latency = self.delivery.current().max_latency;

		let mut result = Ok(false);
		self.state.send_if_modified(|state| {
			result = state.create_frame(group, instant, max_latency);
			result.as_ref().is_ok_and(|modify| *modify)
		});
		result.map(|_| ())
	}

	pub fn consume(&self) -> ExpiresConsumer {
		ExpiresConsumer {
			state: self.state.subscribe(),
			delivery: self.delivery.clone(),
		}
	}
}

#[derive(Clone)]
pub struct ExpiresConsumer {
	state: watch::Receiver<State>,
	delivery: DeliveryConsumer,
}

impl fmt::Debug for ExpiresConsumer {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("ExpiresConsumer")
			.field("state", &self.state.borrow().deref())
			.finish()
	}
}

impl ExpiresConsumer {
	// Blocks until the given group/instant is expired.
	pub async fn wait_expired(&mut self, group: u64, instant: Time) -> Error {
		let mut max_latency = self.delivery.current().max_latency;

		loop {
			tokio::select! {
				state = self
				.state
				.wait_for(|state| state.max_group > group && state.max_instant + max_latency >= instant) => match state {
					Ok(_) => return Error::Expired,
					Err(_) => return Error::Cancel,
				},
				changed = self.delivery.changed() => match changed {
					Some(delivery) => max_latency = delivery.max_latency,
					None => return Error::Cancel,
				},
			}
		}
	}

	pub fn is_expired(&self, group: u64, timestamp: Time) -> bool {
		let max_latency = self.delivery.current().max_latency;
		let state = self.state.borrow();
		state.max_group > group && state.max_instant + max_latency >= timestamp
	}
}
