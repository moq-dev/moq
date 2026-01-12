use std::fmt;

use std::ops::Deref;
use tokio::sync::watch;

use crate::Time;

/// Delivery information for a track.
///
/// Both the publisher and subscriber can set their own values.
#[derive(Clone, Debug, PartialEq, Eq, Copy, Default)]
pub struct Delivery {
	/// Higher priority tracks will be served first during congestion.
	pub priority: u8,

	/// Groups will be dropped if they are this much older than the latest group.
	pub max_latency: Time,

	/// Try to deliver groups in sequence order (for VOD).
	pub ordered: bool,
}

impl Delivery {
	pub fn max(&self, other: &Self) -> Self {
		Self {
			priority: self.priority.max(other.priority),
			max_latency: self.max_latency.max(other.max_latency),
			// Only use ordered if all are ordered.
			ordered: self.ordered && other.ordered,
		}
	}
}

#[derive(Clone)]
pub struct DeliveryProducer {
	state: watch::Sender<Delivery>,
}

impl DeliveryProducer {
	pub fn new(delivery: Delivery) -> Self {
		Self {
			state: watch::Sender::new(delivery),
		}
	}

	pub fn consume(&self) -> DeliveryConsumer {
		DeliveryConsumer {
			state: self.state.subscribe(),
		}
	}

	pub fn update(&self, delivery: Delivery) {
		self.state.send_modify(|state| *state = delivery);
	}
}

#[derive(Clone)]
pub struct DeliveryConsumer {
	state: watch::Receiver<Delivery>,
}

impl fmt::Debug for DeliveryConsumer {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("DeliveryConsumer")
			.field("state", &self.state.borrow().deref())
			.finish()
	}
}

impl DeliveryConsumer {
	pub fn current(&self) -> Delivery {
		*self.state.borrow()
	}

	pub async fn changed(&mut self) -> Option<Delivery> {
		self.state.changed().await.ok()?;
		Some(*self.state.borrow_and_update())
	}
}
