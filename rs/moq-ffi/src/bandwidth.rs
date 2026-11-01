//! Session bandwidth allocator and per-track reservation, mirroring
//! [`moq_net::bandwidth`].

use std::sync::Arc;

use crate::demand::MoqTrackDemand;
use crate::error::MoqError;

/// Divides one connection's send estimate among the tracks sharing it.
///
/// Minted by [`MoqSession::allocator`](crate::session::MoqSession::allocator).
/// Clones share one reservation registry, so two handles from the same session
/// see each other's claims.
#[derive(uniffi::Object)]
pub struct MoqAllocator {
	allocator: moq_net::bandwidth::Allocator,
}

impl MoqAllocator {
	pub(crate) fn new(allocator: moq_net::bandwidth::Allocator) -> Self {
		Self { allocator }
	}

	#[cfg(feature = "audio")]
	pub(crate) fn inner(&self) -> &moq_net::bandwidth::Allocator {
		&self.allocator
	}

	pub(crate) fn reserve_demand(&self, demand: &moq_net::track::Demand, max_bps: u64) -> Arc<MoqReservation> {
		Arc::new(MoqReservation::new(
			self.allocator
				.reserve(demand, moq_net::bandwidth::Rate::from_bps(max_bps)),
		))
	}
}

#[uniffi::export]
impl MoqAllocator {
	/// Reserve up to `max_bps` for the track `demand` watches, returning the reservation.
	///
	/// Every track producer hands out its `demand()`, so raw, media, and JSON
	/// tracks reserve the same way. `max_bps` is a ceiling, not a measurement:
	/// reserve the most the track can ever send. The reservation lasts as long as
	/// the returned handle; drop it to hand the room back.
	/// [`MoqReservation::update`] moves the ceiling without claiming twice.
	pub fn reserve(&self, demand: &MoqTrackDemand, max_bps: u64) -> Arc<MoqReservation> {
		let _guard = crate::ffi::enter();
		self.reserve_demand(demand.inner(), max_bps)
	}
}

/// One track's standing claim on a [`MoqAllocator`].
///
/// [`rate`](Self::rate) is [`moq_net::bandwidth::Reservation::rate`]: `None`
/// means no estimate, so hold the current rate, and `Some(0)` is a real zero rate.
#[derive(uniffi::Object)]
pub struct MoqReservation {
	inner: moq_net::bandwidth::Reservation,
}

impl MoqReservation {
	pub(crate) fn new(inner: moq_net::bandwidth::Reservation) -> Self {
		Self { inner }
	}

	#[cfg(feature = "video")]
	pub(crate) fn consume(&self) -> moq_net::bandwidth::Consumer {
		self.inner.consume()
	}
}

#[uniffi::export]
impl MoqReservation {
	/// This reservation's slice right now, in bits per second.
	///
	/// `None` means no estimate: hold the current rate. `Some(0)` is a real zero
	/// rate. While nothing subscribes to the track, this is the slice it would get
	/// back, without claiming it from the others.
	pub fn rate(&self) -> Option<u64> {
		let _guard = crate::ffi::enter();
		self.inner.rate().map(moq_net::bandwidth::Rate::as_bps)
	}

	/// Change the ceiling, keeping the same claim.
	pub fn update(&self, max_bps: u64) {
		let _guard = crate::ffi::enter();
		self.inner.update(moq_net::bandwidth::Rate::from_bps(max_bps));
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	use crate::producer::{MoqBroadcastProducer, MoqTrackProducer};

	fn allocator(estimate: &moq_net::bandwidth::Producer) -> Arc<MoqAllocator> {
		Arc::new(MoqAllocator::new(moq_net::bandwidth::Allocator::new(
			estimate.consume(),
		)))
	}

	fn named_track(broadcast: &MoqBroadcastProducer, name: &str) -> Arc<MoqTrackProducer> {
		broadcast.publish_track(name.into(), None).unwrap()
	}

	/// Two reservations on one allocator split a 3 Mbps estimate to at most 3 Mbps.
	#[test]
	fn concurrent_reservations_split_the_estimate() {
		let estimate = moq_net::bandwidth::Producer::new();
		let bandwidth = allocator(&estimate);
		let broadcast = MoqBroadcastProducer::new().unwrap();

		let first = named_track(&broadcast, "a");
		let _first_sub = first.consume(None).unwrap();
		let first = bandwidth.reserve(&first.demand().unwrap(), 4_000_000);

		let second = named_track(&broadcast, "b");
		let _second_sub = second.consume(None).unwrap();
		let second = bandwidth.reserve(&second.demand().unwrap(), 2_000_000);

		estimate
			.set(Some(moq_net::bandwidth::Rate::from_bps(3_000_000)))
			.unwrap();

		let a = first.rate().unwrap();
		let b = second.rate().unwrap();
		assert!(a + b <= 3_000_000, "{a} + {b} oversubscribed");
		assert_eq!(a, 1_500_000);
		assert_eq!(b, 1_500_000);
	}

	/// Two allocator handles share one registry, so a reservation on one is
	/// visible to the other.
	#[test]
	fn cloned_handles_see_each_others_reservations() {
		let estimate = moq_net::bandwidth::Producer::new();
		estimate
			.set(Some(moq_net::bandwidth::Rate::from_bps(3_000_000)))
			.unwrap();
		let allocator = moq_net::bandwidth::Allocator::new(estimate.consume());
		let first = Arc::new(MoqAllocator::new(allocator.clone()));
		let second = Arc::new(MoqAllocator::new(allocator));
		let broadcast = MoqBroadcastProducer::new().unwrap();

		let track = named_track(&broadcast, "a");
		let _sub = track.consume(None).unwrap();
		let reserved = first.reserve(&track.demand().unwrap(), 4_000_000);
		assert_eq!(reserved.rate(), Some(3_000_000));

		let other = named_track(&broadcast, "b");
		let _other_sub = other.consume(None).unwrap();
		let other = second.reserve(&other.demand().unwrap(), 4_000_000);
		assert_eq!(reserved.rate(), Some(1_500_000));
		assert_eq!(other.rate(), Some(1_500_000));
	}

	#[test]
	fn dropping_a_reservation_frees_its_share() {
		let estimate = moq_net::bandwidth::Producer::new();
		estimate
			.set(Some(moq_net::bandwidth::Rate::from_bps(3_000_000)))
			.unwrap();
		let bandwidth = allocator(&estimate);
		let broadcast = MoqBroadcastProducer::new().unwrap();

		let first = named_track(&broadcast, "a");
		let _first_sub = first.consume(None).unwrap();
		let first = bandwidth.reserve(&first.demand().unwrap(), 4_000_000);

		let second = named_track(&broadcast, "b");
		let _second_sub = second.consume(None).unwrap();
		let second = bandwidth.reserve(&second.demand().unwrap(), 2_000_000);
		assert_eq!(second.rate(), Some(1_500_000));

		drop(first);
		assert_eq!(second.rate(), Some(2_000_000));
	}

	/// A missing estimate (disconnected) reports `None`; a later value is a grant
	/// again. Reservations survive the gap.
	#[test]
	fn a_reservation_reports_none_while_disconnected() {
		let estimate = moq_net::bandwidth::Producer::new();
		let bandwidth = allocator(&estimate);
		let broadcast = MoqBroadcastProducer::new().unwrap();

		let track = named_track(&broadcast, "a");
		let _sub = track.consume(None).unwrap();
		let reserved = bandwidth.reserve(&track.demand().unwrap(), 4_000_000);

		estimate
			.set(Some(moq_net::bandwidth::Rate::from_bps(2_000_000)))
			.unwrap();
		assert_eq!(reserved.rate(), Some(2_000_000));

		estimate.set(None).unwrap();
		assert_eq!(reserved.rate(), None);

		estimate
			.set(Some(moq_net::bandwidth::Rate::from_bps(1_000_000)))
			.unwrap();
		assert_eq!(reserved.rate(), Some(1_000_000));
	}

	#[cfg(feature = "video")]
	#[test]
	fn a_share_wakes_a_parked_thread() {
		use std::sync::atomic::Ordering;

		let estimate = moq_net::bandwidth::Producer::new();
		let bandwidth = allocator(&estimate);
		let broadcast = MoqBroadcastProducer::new().unwrap();
		let track = named_track(&broadcast, "a");
		let reserved = bandwidth.reserve(&track.demand().unwrap(), 4_000_000);
		let mut consumer = reserved.consume();

		let parked = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
		let parked_flag = parked.clone();
		let handle = std::thread::spawn(move || {
			tokio::runtime::Builder::new_current_thread()
				.enable_all()
				.build()
				.unwrap()
				.block_on(async move {
					parked_flag.store(true, Ordering::SeqCst);
					consumer.changed().await
				})
		});

		while !parked.load(Ordering::SeqCst) {
			std::thread::yield_now();
		}
		// The inner changed() may not have parked yet; give it a turn.
		std::thread::sleep(std::time::Duration::from_millis(50));
		let _sub = track.consume(None).unwrap();
		estimate
			.set(Some(moq_net::bandwidth::Rate::from_bps(1_000_000)))
			.unwrap();
		let grant = handle.join().expect("follow thread panicked").expect("share closed");
		assert_eq!(grant, Some(moq_net::bandwidth::Rate::from_bps(1_000_000)));
	}
}

/// An owned bandwidth estimate; holding it requests estimates until released.
#[derive(uniffi::Object)]
pub struct MoqBandwidthConsumer {
	inner: std::sync::Mutex<Option<moq_net::bandwidth::Consumer>>,
	changed: crate::ffi::Task<()>,
}

impl MoqBandwidthConsumer {
	pub(crate) fn new(inner: moq_net::bandwidth::Consumer) -> Self {
		Self {
			inner: std::sync::Mutex::new(Some(inner)),
			changed: crate::ffi::Task::new(()),
		}
	}
}

#[uniffi::export]
impl MoqBandwidthConsumer {
	/// The current estimate in bits per second, or None while unavailable.
	pub fn rate(&self) -> Option<u64> {
		self.inner
			.lock()
			.unwrap()
			.as_ref()
			.and_then(moq_net::bandwidth::Consumer::peek)
			.map(moq_net::bandwidth::Rate::as_bps)
	}

	/// Wait for the estimate to change and return the new [`rate`](Self::rate), erroring when
	/// its producer closes.
	pub async fn rate_changed(&self) -> Result<Option<u64>, MoqError> {
		self.changed
			.run(|_guard| async move {
				moq_net::kio::wait(|waiter| {
					let mut inner = self.inner.lock().unwrap();
					let inner = inner.as_mut().ok_or(MoqError::Cancelled)?;
					inner.poll_changed(waiter).map(|result| {
						result
							.map(|rate| rate.map(moq_net::bandwidth::Rate::as_bps))
							.map_err(Into::into)
					})
				})
				.await
			})
			.await
	}

	/// Release this consumer's demand, including a pending wait.
	pub fn cancel(&self) {
		self.inner.lock().unwrap().take();
		self.changed.cancel();
	}
}

#[cfg(test)]
mod consumer_tests {
	use super::*;
	use std::{
		future::Future,
		task::{Context, Waker},
	};

	#[tokio::test]
	async fn owned_consumer_reads_while_waiting_and_cancel_releases_demand() {
		tokio::time::pause();
		let producer = moq_net::bandwidth::Producer::new();
		let consumer = MoqBandwidthConsumer::new(producer.consume());
		assert!(producer.demand().wanted());
		let mut changed = std::pin::pin!(consumer.rate_changed());
		assert!(
			changed
				.as_mut()
				.poll(&mut Context::from_waker(Waker::noop()))
				.is_pending()
		);
		producer.set(Some(moq_net::bandwidth::Rate::from_bps(123))).unwrap();
		assert_eq!(consumer.rate(), Some(123));
		assert_eq!(changed.await.unwrap(), Some(123));
		let mut changed = std::pin::pin!(consumer.rate_changed());
		assert!(
			changed
				.as_mut()
				.poll(&mut Context::from_waker(Waker::noop()))
				.is_pending()
		);
		consumer.cancel();
		assert!(!producer.demand().wanted());
		assert_eq!(consumer.rate(), None);
		assert!(matches!(changed.await, Err(MoqError::Cancelled)));
	}

	#[test]
	fn dropping_the_owned_consumer_releases_demand() {
		let producer = moq_net::bandwidth::Producer::new();
		let consumer = MoqBandwidthConsumer::new(producer.consume());
		assert!(producer.demand().wanted());
		drop(consumer);
		assert!(!producer.demand().wanted());
	}

	#[tokio::test]
	async fn producer_closure_ends_changed() {
		tokio::time::pause();
		let producer = moq_net::bandwidth::Producer::new();
		let consumer = MoqBandwidthConsumer::new(producer.consume());
		drop(producer);
		assert!(consumer.rate_changed().await.is_err());
	}
}
