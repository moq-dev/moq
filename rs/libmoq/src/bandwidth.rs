//! Session bandwidth allocator and per-track reservation, mirroring
//! [`moq_net::bandwidth`].

use std::sync::Arc;

use crate::{Error, Id, NonZeroSlab, State, ffi};

/// Allocators minted from a session, plus the reservations taken against them.
#[derive(Default)]
pub(crate) struct Bandwidth {
	allocators: NonZeroSlab<moq_net::bandwidth::Allocator>,
	reservations: NonZeroSlab<Arc<moq_net::bandwidth::Reservation>>,
}

impl Bandwidth {
	pub fn insert(&mut self, allocator: moq_net::bandwidth::Allocator) -> Result<Id, Error> {
		self.allocators.insert(allocator)
	}

	pub fn allocator(&self, id: Id) -> Result<moq_net::bandwidth::Allocator, Error> {
		self.allocators.get(id).cloned().ok_or(Error::NotFound)
	}

	pub fn close(&mut self, id: Id) -> Result<(), Error> {
		self.allocators.remove(id).ok_or(Error::NotFound)?;
		Ok(())
	}

	pub fn reserve(&mut self, allocator: Id, demand: &moq_net::track::Demand, max_bps: u64) -> Result<Id, Error> {
		let allocator = self.allocator(allocator)?;
		let reservation = allocator.reserve(demand, moq_net::bandwidth::Rate::from_bps(max_bps));
		self.reservations.insert(Arc::new(reservation))
	}

	pub fn hold(&mut self, reservation: Arc<moq_net::bandwidth::Reservation>) -> Result<Id, Error> {
		self.reservations.insert(reservation)
	}

	pub fn reservation(&self, id: Id) -> Result<Arc<moq_net::bandwidth::Reservation>, Error> {
		self.reservations.get(id).cloned().ok_or(Error::NotFound)
	}

	pub fn reservation_close(&mut self, id: Id) -> Result<(), Error> {
		self.reservations.remove(id).ok_or(Error::NotFound)?;
		Ok(())
	}
}

/// The session's bandwidth allocator. Clones share one reservation registry.
///
/// Returns a non-zero handle, or a negative error if the session is unknown.
#[unsafe(no_mangle)]
pub extern "C" fn moq_session_bandwidth(session: u32) -> i32 {
	ffi::enter(move || {
		let session = ffi::parse_id(session)?;
		let mut state = State::lock();
		let allocator = state.session.bandwidth(session)?;
		state.bandwidth.insert(allocator)
	})
}

/// Release a bandwidth handle. Reservations taken against it stay until they
/// themselves are closed (or the session's allocator is gone).
#[unsafe(no_mangle)]
pub extern "C" fn moq_bandwidth_close(bandwidth: u32) -> i32 {
	ffi::enter(move || {
		let bandwidth = ffi::parse_id(bandwidth)?;
		State::lock().bandwidth.close(bandwidth)
	})
}

/// Reserve up to `max_bps` for `track`, returning a reservation handle.
///
/// `max_bps` is a ceiling, not a measurement: reserve the most the track can
/// ever send. Drop the reservation with [`moq_reservation_close`] to hand the
/// room back.
#[unsafe(no_mangle)]
pub extern "C" fn moq_bandwidth_reserve(bandwidth: u32, track: u32, max_bps: u64) -> i32 {
	ffi::enter(move || {
		let bandwidth = ffi::parse_id(bandwidth)?;
		let track = ffi::parse_id(track)?;
		let mut state = State::lock();
		let demand = state.publish.track_demand(track)?;
		state.bandwidth.reserve(bandwidth, &demand, max_bps)
	})
}

/// This reservation's slice right now, in bits per second.
///
/// `present` is false when there is no estimate or no demand: hold the current
/// rate. `present` true and `bps` 0 is a real zero grant.
///
/// # Safety
/// - `bps` and `present` must be valid pointers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_reservation_grant(reservation: u32, bps: *mut u64, present: *mut bool) -> i32 {
	ffi::enter(move || {
		let reservation = ffi::parse_id(reservation)?;
		let bps = unsafe { bps.as_mut() }.ok_or(Error::InvalidPointer)?;
		let present = unsafe { present.as_mut() }.ok_or(Error::InvalidPointer)?;
		match State::lock().bandwidth.reservation(reservation)?.peek() {
			Some(rate) => {
				*bps = rate.as_bps();
				*present = true;
			}
			None => {
				*bps = 0;
				*present = false;
			}
		}
		Ok(())
	})
}

/// Change the ceiling, keeping the same claim.
#[unsafe(no_mangle)]
pub extern "C" fn moq_reservation_update(reservation: u32, max_bps: u64) -> i32 {
	ffi::enter(move || {
		let reservation = ffi::parse_id(reservation)?;
		State::lock()
			.bandwidth
			.reservation(reservation)?
			.update(moq_net::bandwidth::Rate::from_bps(max_bps));
		Ok(())
	})
}

/// Release a reservation, handing its share back to siblings.
///
/// Closing an accessor returned by a video or audio producer does not release
/// the encoder's claim; that lasts until the producer is finished.
#[unsafe(no_mangle)]
pub extern "C" fn moq_reservation_close(reservation: u32) -> i32 {
	ffi::enter(move || {
		let reservation = ffi::parse_id(reservation)?;
		State::lock().bandwidth.reservation_close(reservation)
	})
}
