//! Session bandwidth allocator and per-track reservation, and the receive-side
//! viewers, mirroring [`moq_net::bandwidth`].

use std::sync::Arc;

use crate::{Error, Id, NonZeroSlab, State, ffi};

/// Allocators minted from a session, plus the reservations taken against them, and
/// the same for the receive side's viewers.
#[derive(Default)]
pub(crate) struct Bandwidth {
	allocators: NonZeroSlab<moq_net::bandwidth::Allocator>,
	reservations: NonZeroSlab<Arc<moq_net::bandwidth::Reservation>>,
	consumers: NonZeroSlab<Arc<Consumer>>,
	viewers: NonZeroSlab<moq_net::bandwidth::Viewers>,
	viewer: NonZeroSlab<moq_net::bandwidth::Viewer>,
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

	pub fn insert_viewers(&mut self, viewers: moq_net::bandwidth::Viewers) -> Result<Id, Error> {
		self.viewers.insert(viewers)
	}
}

/// The session's bandwidth allocator. Clones share one reservation registry.
///
/// Returns a non-zero handle, or a negative error if the session is unknown.
#[unsafe(no_mangle)]
pub extern "C" fn moq_session_allocator(session: u32) -> i32 {
	ffi::enter(move || {
		let session = ffi::parse_id(session)?;
		let mut state = State::lock();
		let allocator = state.session.allocator(session)?;
		state.bandwidth.insert(allocator)
	})
}

/// Release an allocator handle. Reservations taken against it stay until they
/// themselves are closed (or the session's allocator is gone).
#[unsafe(no_mangle)]
pub extern "C" fn moq_allocator_close(allocator: u32) -> i32 {
	ffi::enter(move || {
		let allocator = ffi::parse_id(allocator)?;
		State::lock().bandwidth.close(allocator)
	})
}

/// Reserve up to `max_bps` for `track`, returning a reservation handle.
///
/// `max_bps` is a ceiling, not a measurement: reserve the most the track can
/// ever send. Drop the reservation with [`moq_reservation_close`] to hand the
/// room back.
#[unsafe(no_mangle)]
pub extern "C" fn moq_allocator_reserve(allocator: u32, track: u32, max_bps: u64) -> i32 {
	ffi::enter(move || {
		let allocator = ffi::parse_id(allocator)?;
		let track = ffi::parse_id(track)?;
		let mut state = State::lock();
		let demand = state.publish.track_demand(track)?;
		state.bandwidth.reserve(allocator, &demand, max_bps)
	})
}

/// This reservation's slice right now, in bits per second.
///
/// `present` is false when there is no estimate: hold the current rate. `present`
/// true and `bps` 0 is a real zero rate. While nothing subscribes to the track,
/// `bps` is the slice it would get back, without claiming it from the others.
///
/// # Safety
/// - `bps` and `present` must be valid pointers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_reservation_rate(reservation: u32, bps: *mut u64, present: *mut bool) -> i32 {
	ffi::enter(move || {
		let reservation = ffi::parse_id(reservation)?;
		let bps = unsafe { bps.as_mut() }.ok_or(Error::InvalidPointer)?;
		let present = unsafe { present.as_mut() }.ok_or(Error::InvalidPointer)?;
		match State::lock().bandwidth.reservation(reservation)?.rate() {
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

/// The session's viewers, dividing the receive estimate among watched broadcasts.
///
/// Clones share one registry, and the session sends the viewers' ceilings summed as its
/// PROBE target. Returns a non-zero handle, or a negative error if the session is unknown.
#[unsafe(no_mangle)]
pub extern "C" fn moq_session_viewers(session: u32) -> i32 {
	ffi::enter(move || {
		let session = ffi::parse_id(session)?;
		let mut state = State::lock();
		let viewers = state.session.viewers(session)?;
		state.bandwidth.insert_viewers(viewers)
	})
}

/// Release a viewers handle. Viewers watched through it stay until each is closed (or
/// the session's viewers are gone).
#[unsafe(no_mangle)]
pub extern "C" fn moq_viewers_close(viewers: u32) -> i32 {
	ffi::enter(move || {
		let viewers = ffi::parse_id(viewers)?;
		State::lock().bandwidth.viewers.remove(viewers).ok_or(Error::NotFound)?;
		Ok(())
	})
}

/// Watch a broadcast receiving at most `max_bps`, returning a viewer handle.
///
/// `max_bps` is a ceiling: what the largest rendition you would switch to needs, not
/// what you are receiving. Holding the viewer requests the receive estimate, like a
/// [`moq_session_recv_bandwidth`] consumer. Close it with [`moq_viewer_close`] once the
/// broadcast stops being watched.
#[unsafe(no_mangle)]
pub extern "C" fn moq_viewers_watch(viewers: u32, max_bps: u64) -> i32 {
	ffi::enter(move || {
		let viewers = ffi::parse_id(viewers)?;
		let mut state = State::lock();
		let viewer = state
			.bandwidth
			.viewers
			.get(viewers)
			.ok_or(Error::NotFound)?
			.watch(moq_net::bandwidth::Rate::from_bps(max_bps));
		state.bandwidth.viewer.insert(viewer)
	})
}

/// Send `target_bps` as the PROBE target instead of the viewers' sum.
///
/// `present` false follows the viewers again. `present` true and `target_bps` 0 sends
/// no target.
#[unsafe(no_mangle)]
pub extern "C" fn moq_viewers_set_target(viewers: u32, target_bps: u64, present: bool) -> i32 {
	ffi::enter(move || {
		let viewers = ffi::parse_id(viewers)?;
		State::lock()
			.bandwidth
			.viewers
			.get(viewers)
			.ok_or(Error::NotFound)?
			.set_target(present.then(|| moq_net::bandwidth::Rate::from_bps(target_bps)));
		Ok(())
	})
}

/// This viewer's slice of the receive estimate right now, in bits per second.
///
/// `present` is false when there is no estimate: keep the current rendition. `present`
/// true and `bps` 0 is a real zero rate.
///
/// # Safety
/// - `bps` and `present` must be valid pointers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_viewer_rate(viewer: u32, bps: *mut u64, present: *mut bool) -> i32 {
	ffi::enter(move || {
		let viewer = ffi::parse_id(viewer)?;
		let bps = unsafe { bps.as_mut() }.ok_or(Error::InvalidPointer)?;
		let present = unsafe { present.as_mut() }.ok_or(Error::InvalidPointer)?;
		let rate = State::lock()
			.bandwidth
			.viewer
			.get(viewer)
			.ok_or(Error::NotFound)?
			.rate();
		*bps = rate.map_or(0, moq_net::bandwidth::Rate::as_bps);
		*present = rate.is_some();
		Ok(())
	})
}

/// Change the viewer's ceiling, keeping the same claim.
#[unsafe(no_mangle)]
pub extern "C" fn moq_viewer_update(viewer: u32, max_bps: u64) -> i32 {
	ffi::enter(move || {
		let viewer = ffi::parse_id(viewer)?;
		State::lock()
			.bandwidth
			.viewer
			.get(viewer)
			.ok_or(Error::NotFound)?
			.update(moq_net::bandwidth::Rate::from_bps(max_bps));
		Ok(())
	})
}

/// Release a viewer, handing its share back to the others and lowering the PROBE target.
#[unsafe(no_mangle)]
pub extern "C" fn moq_viewer_close(viewer: u32) -> i32 {
	ffi::enter(move || {
		let viewer = ffi::parse_id(viewer)?;
		// Released outside the lock.
		let viewer = State::lock().bandwidth.viewer.remove(viewer).ok_or(Error::NotFound)?;
		drop(viewer);
		Ok(())
	})
}

/// One owned estimate and, optionally, its single outstanding changed call.
struct Consumer {
	inner: std::sync::Mutex<Option<moq_net::bandwidth::Consumer>>,
	changed: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
}

/// Hold receive-estimate demand, returning zero if the version has no PROBE.
#[unsafe(no_mangle)]
pub extern "C" fn moq_session_recv_bandwidth(session: u32) -> i32 {
	ffi::enter(move || {
		let session = ffi::parse_id(session)?;
		let mut state = State::lock();
		let Some(inner) = state.session.recv_bandwidth(session)? else {
			return Ok(0);
		};
		let id = state.bandwidth.consumers.insert(Arc::new(Consumer {
			inner: std::sync::Mutex::new(Some(inner)),
			changed: Default::default(),
		}))?;
		Ok::<_, Error>(i32::from(id))
	})
}

/// Read this consumer's estimate in bits per second.
///
/// # Safety
/// - `bps` and `present` must be valid pointers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_bandwidth_consumer_peek(consumer: u32, bps: *mut u64, present: *mut bool) -> i32 {
	ffi::enter(move || {
		let consumer = ffi::parse_id(consumer)?;
		let bps = unsafe { bps.as_mut() }.ok_or(Error::InvalidPointer)?;
		let present = unsafe { present.as_mut() }.ok_or(Error::InvalidPointer)?;
		let consumer = State::lock()
			.bandwidth
			.consumers
			.get(consumer)
			.cloned()
			.ok_or(Error::NotFound)?;
		let rate = consumer
			.inner
			.lock()
			.unwrap()
			.as_ref()
			.and_then(moq_net::bandwidth::Consumer::peek);
		*bps = rate.map_or(0, moq_net::bandwidth::Rate::as_bps);
		*present = rate.is_some();
		Ok(())
	})
}

/// Wait for a change, invoking `on_status` once with 1 or a negative error.
///
/// Read the value with `moq_bandwidth_consumer_peek` from the callback. Only one
/// wait may be outstanding; a second returns InvalidConfig. Closing cancels the wait.
///
/// # Safety
/// - `user_data` must remain valid until the callback returns.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_bandwidth_consumer_changed(
	consumer: u32,
	user_data: *mut std::ffi::c_void,
	on_status: ffi::moq_status_callback,
) -> i32 {
	ffi::enter(move || {
		let consumer = ffi::parse_id(consumer)?;
		let callback = unsafe { ffi::OnStatus::new(user_data, on_status)? };
		let consumer = State::lock()
			.bandwidth
			.consumers
			.get(consumer)
			.cloned()
			.ok_or(Error::NotFound)?;
		let (cancel, cancelled) = tokio::sync::oneshot::channel();
		{
			let mut pending = consumer.changed.lock().unwrap();
			if pending.is_some() {
				return Err(Error::InvalidConfig(
					"a bandwidth change wait is already pending".into(),
				));
			}
			*pending = Some(cancel);
		}
		tokio::spawn(async move {
			// The cleanup runs inside `ffi::run` too, so a panic there still reaches the callback,
			// and the wait has its own so the slot is cleared even when the wait panics.
			let result = ffi::run(async {
				let result = ffi::run(async {
					tokio::select! {
						_ = cancelled => Err(moq_net::Error::Cancel.into()),
						result = kio::wait(|waiter| {
							let mut inner = consumer.inner.lock().unwrap();
							let Some(inner) = inner.as_mut() else { return std::task::Poll::Ready(Err(Error::NotFound)); };
							inner.poll_changed(waiter).map(|result| result.map(|_| 1).map_err(Into::into))
						}) => result,
					}
				})
				.await;
				consumer.changed.lock().unwrap().take();
				result
			})
			.await;
			callback.call(result);
		});
		Ok(())
	})
}

/// Release this consumer's demand and cancel any outstanding wait.
///
/// The outstanding `moq_bandwidth_consumer_changed` callback still fires once.
/// Release its `user_data` in that callback, not when this function returns.
#[unsafe(no_mangle)]
pub extern "C" fn moq_bandwidth_consumer_close(consumer: u32) -> i32 {
	ffi::enter(move || {
		let consumer = ffi::parse_id(consumer)?;
		let consumer = State::lock()
			.bandwidth
			.consumers
			.remove(consumer)
			.ok_or(Error::NotFound)?;
		consumer.inner.lock().unwrap().take();
		consumer.changed.lock().unwrap().take();
		Ok(())
	})
}

#[cfg(test)]
mod consumer_tests {
	use super::*;
	use crate::ffi::ReturnCode;

	extern "C" fn changed(user_data: *mut std::ffi::c_void, code: i32) {
		let sender = unsafe { Box::from_raw(user_data as *mut tokio::sync::mpsc::UnboundedSender<i32>) };
		sender.send(code).unwrap();
	}

	#[tokio::test]
	async fn consumer_close_releases_demand_and_completes_pending_callback() {
		tokio::time::pause();
		let producer = moq_net::bandwidth::Producer::new();
		let id = State::lock()
			.bandwidth
			.consumers
			.insert(Arc::new(Consumer {
				inner: std::sync::Mutex::new(Some(producer.consume())),
				changed: Default::default(),
			}))
			.unwrap();
		let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
		let user_data = || Box::into_raw(Box::new(sender.clone())) as *mut std::ffi::c_void;
		assert!(producer.is_used());
		assert_eq!(
			unsafe { moq_bandwidth_consumer_changed(id.into(), user_data(), Some(changed)) },
			0
		);
		producer.set(Some(moq_net::bandwidth::Rate::from_bps(321))).unwrap();
		assert_eq!(receiver.recv().await, Some(1));
		let (mut bps, mut present) = (0, false);
		assert_eq!(
			unsafe { moq_bandwidth_consumer_peek(id.into(), &mut bps, &mut present) },
			0
		);
		assert!(present);
		assert_eq!(bps, 321);
		assert_eq!(
			unsafe { moq_bandwidth_consumer_changed(id.into(), user_data(), Some(changed)) },
			0
		);
		assert_eq!(moq_bandwidth_consumer_close(id.into()), 0);
		assert!(!producer.is_used(), "closing releases demand before the callback runs");
		assert!(receiver.recv().await.unwrap() < 0);
	}

	#[tokio::test]
	async fn changed_cleanup_panic_still_delivers_callback() {
		let producer = moq_net::bandwidth::Producer::new();
		let consumer = Arc::new(Consumer {
			inner: std::sync::Mutex::new(Some(producer.consume())),
			changed: Default::default(),
		});
		let id = State::lock().bandwidth.consumers.insert(consumer.clone()).unwrap();
		let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
		let user_data = Box::into_raw(Box::new(sender)) as *mut std::ffi::c_void;
		assert_eq!(
			unsafe { moq_bandwidth_consumer_changed(id.into(), user_data, Some(changed)) },
			0
		);
		// Poison the slot the wait clears once it completes.
		let poisoner = consumer.clone();
		std::thread::spawn(move || {
			let _pending = poisoner.changed.lock().unwrap();
			panic!("poison the pending wait");
		})
		.join()
		.unwrap_err();
		producer.set(Some(moq_net::bandwidth::Rate::from_bps(321))).unwrap();
		assert_eq!(receiver.recv().await, Some(Error::Panic.code()));
		State::lock().bandwidth.consumers.remove(id);
	}
}
