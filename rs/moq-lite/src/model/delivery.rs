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

impl fmt::Debug for DeliveryProducer {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("DeliveryProducer")
			.field("state", &self.state.borrow().deref())
			.finish()
	}
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

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_delivery_max() {
		let d1 = Delivery {
			priority: 5,
			max_latency: Time::from_millis(100).unwrap(),
			ordered: true,
		};
		let d2 = Delivery {
			priority: 3,
			max_latency: Time::from_millis(200).unwrap(),
			ordered: false,
		};

		let result = d1.max(&d2);
		assert_eq!(result.priority, 5);
		assert_eq!(result.max_latency, Time::from_millis(200).unwrap());
		assert!(!result.ordered); // false if any is false
	}

	#[test]
	fn test_delivery_max_all_ordered() {
		let d1 = Delivery {
			priority: 5,
			max_latency: Time::from_millis(100).unwrap(),
			ordered: true,
		};
		let d2 = Delivery {
			priority: 3,
			max_latency: Time::from_millis(50).unwrap(),
			ordered: true,
		};

		let result = d1.max(&d2);
		assert!(result.ordered); // true only if all are ordered
	}

	#[test]
	fn test_delivery_producer_consumer() {
		let delivery = Delivery {
			priority: 10,
			max_latency: Time::from_millis(500).unwrap(),
			ordered: false,
		};

		let producer = DeliveryProducer::new(delivery);
		let consumer = producer.consume();

		assert_eq!(consumer.current(), delivery);
	}

	#[tokio::test]
	async fn test_delivery_update() {
		let initial = Delivery {
			priority: 5,
			max_latency: Time::from_millis(100).unwrap(),
			ordered: false,
		};

		let producer = DeliveryProducer::new(initial);
		let mut consumer = producer.consume();

		assert_eq!(consumer.current(), initial);

		// Update delivery
		let updated = Delivery {
			priority: 10,
			max_latency: Time::from_millis(200).unwrap(),
			ordered: true,
		};
		producer.update(updated);

		// Consumer should receive the update
		let changed = consumer.changed().await.expect("should receive update");
		assert_eq!(changed, updated);
		assert_eq!(consumer.current(), updated);
	}

	#[tokio::test]
	async fn test_delivery_multiple_consumers() {
		let initial = Delivery::default();
		let producer = DeliveryProducer::new(initial);

		let mut consumer1 = producer.consume();
		let mut consumer2 = producer.consume();

		let updated = Delivery {
			priority: 15,
			max_latency: Time::from_millis(300).unwrap(),
			ordered: true,
		};
		producer.update(updated);

		// Both consumers should receive the update
		assert_eq!(consumer1.changed().await, Some(updated));
		assert_eq!(consumer2.changed().await, Some(updated));
	}

	#[test]
	fn test_delivery_default() {
		let d = Delivery::default();
		assert_eq!(d.priority, 0);
		assert_eq!(d.max_latency, Time::ZERO);
		assert!(!d.ordered);
	}
}
