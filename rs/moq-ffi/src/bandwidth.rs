//! Session bandwidth allocator and per-track reservation, mirroring
//! [`moq_net::bandwidth`].

use std::sync::Arc;

use crate::error::MoqError;
use crate::producer::MoqTrackProducer;

/// Divides one connection's send estimate among the tracks sharing it.
///
/// Minted by [`MoqSession::bandwidth`](crate::session::MoqSession::bandwidth).
/// Clones share one reservation registry, so two handles from the same session
/// see each other's claims.
#[derive(uniffi::Object)]
pub struct MoqBandwidth {
	allocator: moq_net::bandwidth::Allocator,
}

impl MoqBandwidth {
	pub(crate) fn new(allocator: moq_net::bandwidth::Allocator) -> Self {
		Self { allocator }
	}

	#[cfg(all(feature = "audio", not(target_arch = "wasm32")))]
	pub(crate) fn allocator(&self) -> &moq_net::bandwidth::Allocator {
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
impl MoqBandwidth {
	/// Reserve up to `max_bps` for `track`, returning the reservation.
	///
	/// `max_bps` is a ceiling, not a measurement: reserve the most the track can
	/// ever send. The reservation lasts as long as the returned handle; drop it
	/// to hand the room back. [`MoqReservation::update`] moves the ceiling
	/// without claiming twice.
	pub fn reserve(&self, track: &MoqTrackProducer, max_bps: u64) -> Result<Arc<MoqReservation>, MoqError> {
		let _guard = crate::ffi::enter();
		Ok(self.reserve_demand(&track.demand()?, max_bps))
	}
}

/// One track's standing claim on a [`MoqBandwidth`].
///
/// [`grant`](Self::grant) is [`moq_net::bandwidth::Reservation::peek`]: `None`
/// means no estimate or no demand, so hold the current rate, and `Some(0)` is a
/// real zero grant.
#[derive(uniffi::Object)]
pub struct MoqReservation {
	inner: moq_net::bandwidth::Reservation,
}

impl MoqReservation {
	pub(crate) fn new(inner: moq_net::bandwidth::Reservation) -> Self {
		Self { inner }
	}

	#[cfg(all(feature = "video", not(target_arch = "wasm32")))]
	pub(crate) fn consumer(&self) -> moq_net::bandwidth::Consumer {
		self.inner.consumer()
	}
}

#[uniffi::export]
impl MoqReservation {
	/// This reservation's slice right now, in bits per second.
	///
	/// `None` means no estimate or no demand: hold the current rate. `Some(0)`
	/// is a real zero grant.
	pub fn grant(&self) -> Option<u64> {
		let _guard = crate::ffi::enter();
		self.inner.peek().map(moq_net::bandwidth::Rate::as_bps)
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
	use std::sync::atomic::Ordering;

	use crate::producer::MoqBroadcastProducer;

	fn bandwidth(estimate: &moq_net::bandwidth::Producer) -> Arc<MoqBandwidth> {
		Arc::new(MoqBandwidth::new(moq_net::bandwidth::Allocator::new(
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
		let bandwidth = bandwidth(&estimate);
		let broadcast = MoqBroadcastProducer::new().unwrap();

		let first = named_track(&broadcast, "a");
		let _first_sub = first.consume(None).unwrap();
		let first = bandwidth.reserve(&first, 4_000_000).unwrap();

		let second = named_track(&broadcast, "b");
		let _second_sub = second.consume(None).unwrap();
		let second = bandwidth.reserve(&second, 2_000_000).unwrap();

		estimate
			.set(Some(moq_net::bandwidth::Rate::from_bps(3_000_000)))
			.unwrap();

		let a = first.grant().unwrap();
		let b = second.grant().unwrap();
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
		let first = Arc::new(MoqBandwidth::new(allocator.clone()));
		let second = Arc::new(MoqBandwidth::new(allocator));
		let broadcast = MoqBroadcastProducer::new().unwrap();

		let track = named_track(&broadcast, "a");
		let _sub = track.consume(None).unwrap();
		let reserved = first.reserve(&track, 4_000_000).unwrap();
		assert_eq!(reserved.grant(), Some(3_000_000));

		let other = named_track(&broadcast, "b");
		let _other_sub = other.consume(None).unwrap();
		let other = second.reserve(&other, 4_000_000).unwrap();
		assert_eq!(reserved.grant(), Some(1_500_000));
		assert_eq!(other.grant(), Some(1_500_000));
	}

	#[test]
	fn dropping_a_reservation_frees_its_share() {
		let estimate = moq_net::bandwidth::Producer::new();
		estimate
			.set(Some(moq_net::bandwidth::Rate::from_bps(3_000_000)))
			.unwrap();
		let bandwidth = bandwidth(&estimate);
		let broadcast = MoqBroadcastProducer::new().unwrap();

		let first = named_track(&broadcast, "a");
		let _first_sub = first.consume(None).unwrap();
		let first = bandwidth.reserve(&first, 4_000_000).unwrap();

		let second = named_track(&broadcast, "b");
		let _second_sub = second.consume(None).unwrap();
		let second = bandwidth.reserve(&second, 2_000_000).unwrap();
		assert_eq!(second.grant(), Some(1_500_000));

		drop(first);
		assert_eq!(second.grant(), Some(2_000_000));
	}

	/// A missing estimate (disconnected) reports `None`; a later value is a grant
	/// again. Reservations survive the gap.
	#[test]
	fn a_reservation_reports_none_while_disconnected() {
		let estimate = moq_net::bandwidth::Producer::new();
		let bandwidth = bandwidth(&estimate);
		let broadcast = MoqBroadcastProducer::new().unwrap();

		let track = named_track(&broadcast, "a");
		let _sub = track.consume(None).unwrap();
		let reserved = bandwidth.reserve(&track, 4_000_000).unwrap();

		estimate
			.set(Some(moq_net::bandwidth::Rate::from_bps(2_000_000)))
			.unwrap();
		assert_eq!(reserved.grant(), Some(2_000_000));

		estimate.set(None).unwrap();
		assert_eq!(reserved.grant(), None);

		estimate
			.set(Some(moq_net::bandwidth::Rate::from_bps(1_000_000)))
			.unwrap();
		assert_eq!(reserved.grant(), Some(1_000_000));
	}

	#[test]
	fn a_share_wakes_a_parked_thread() {
		let estimate = moq_net::bandwidth::Producer::new();
		let bandwidth = bandwidth(&estimate);
		let broadcast = MoqBroadcastProducer::new().unwrap();
		let track = named_track(&broadcast, "a");
		let reserved = bandwidth.reserve(&track, 4_000_000).unwrap();
		let mut consumer = reserved.consumer();

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
