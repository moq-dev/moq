//! Independent live datagram track handles.

use std::ffi::{c_char, c_void};

use crate::{Error, State, ffi, moq_datagram, moq_string};

/// Fixed publisher properties of a datagram track; zeroed means untimed, priority zero.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct moq_datagram_info {
	/// Publisher priority, breaking ties between equal subscriber priorities.
	pub priority: i8,
	/// Timestamp ticks per second, or zero for an untimed track.
	pub timescale: u64,
}

impl TryFrom<moq_datagram_info> for moq_net::datagram::Info {
	type Error = Error;

	fn try_from(info: moq_datagram_info) -> Result<Self, Error> {
		let timescale = match info.timescale {
			0 => None,
			scale => Some(moq_net::Timescale::new(scale)?),
		};
		Ok(Self::default().with_timescale(timescale).with_priority(info.priority))
	}
}

unsafe fn info(info: *const moq_datagram_info) -> Result<moq_net::datagram::Info, Error> {
	unsafe { info.as_ref() }.copied().unwrap_or_default().try_into()
}

/// Create a live datagram producer, independent of any stream track with the same name.
///
/// Returns its positive handle, or a negative error. NULL info uses the datagram defaults.
///
/// # Safety
/// `name` must point to `name_len` bytes; `info` must be NULL or a valid record.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_publish_datagrams(
	broadcast: u32,
	name: *const c_char,
	name_len: usize,
	properties: *const moq_datagram_info,
) -> i32 {
	ffi::enter(|| {
		let broadcast = ffi::parse_id(broadcast)?;
		let name = unsafe { ffi::parse_str(name, name_len)? };
		let info = unsafe { info(properties)? };
		State::lock().publish.datagrams(broadcast, name, info)
	})
}

/// Insert a datagram at its explicit sequence; later appends continue past the highest one.
///
/// # Safety
/// `datagram` must point to a record whose payload and optional timestamp are valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_publish_datagrams_insert(producer: u32, datagram: *const moq_datagram) -> i32 {
	ffi::enter(|| {
		let producer = ffi::parse_id(producer)?;
		let datagram = unsafe { datagram.as_ref() }.ok_or(Error::InvalidPointer)?;
		let payload = unsafe { ffi::parse_slice(datagram.payload, datagram.payload_size)? };
		let timestamp = unsafe { ffi::parse_timestamp(datagram.timestamp)? };
		State::lock().publish.datagrams_insert(
			producer,
			moq_net::Datagram {
				sequence: datagram.sequence,
				timestamp,
				payload: bytes::Bytes::copy_from_slice(payload),
			},
		)
	})
}

/// Finish the datagram track and release its producer; existing readers drain unread datagrams.
#[unsafe(no_mangle)]
pub extern "C" fn moq_publish_datagrams_finish(producer: u32) -> i32 {
	ffi::enter(|| State::lock().publish.datagrams_finish(ffi::parse_id(producer)?))
}

/// Abort the datagram track with an application error and release its producer.
#[unsafe(no_mangle)]
pub extern "C" fn moq_publish_datagrams_abort(producer: u32, error_code: u16) -> i32 {
	ffi::enter(|| {
		State::lock()
			.publish
			.datagrams_abort(ffi::parse_id(producer)?, error_code)
	})
}

unsafe fn priority(value: Option<i8>, present: *mut bool, out: *mut i8) -> Result<(), Error> {
	let present = unsafe { present.as_mut() }.ok_or(Error::InvalidPointer)?;
	let out = unsafe { out.as_mut() }.ok_or(Error::InvalidPointer)?;
	*present = value.is_some();
	*out = value.unwrap_or_default();
	Ok(())
}

/// Read the highest subscriber priority; `present` is false while nobody consumes the track.
///
/// # Safety
/// `present` and `out` must point to writable values.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_publish_datagrams_priority(producer: u32, present: *mut bool, out: *mut i8) -> i32 {
	ffi::enter(|| {
		let value = State::lock().publish.datagrams_priority(ffi::parse_id(producer)?)?;
		unsafe { priority(value, present, out) }
	})
}

/// Watch aggregate priority changes, receiving WANTED or UNWANTED, then one terminal status.
///
/// Read the priority with `moq_datagram_demand_priority` from a change callback. Closing the
/// producer or watcher ends it with zero. Release user_data only in the terminal callback.
///
/// # Safety
/// `user_data` must remain valid until the callback receives its terminal status.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_publish_datagrams_demand(
	producer: u32,
	callback: ffi::moq_status_callback,
	user_data: *mut c_void,
) -> i32 {
	ffi::enter(|| {
		let producer = ffi::parse_id(producer)?;
		let callback = unsafe { ffi::OnStatus::new(user_data, callback)? };
		State::lock().publish.datagrams_demand(producer, callback)
	})
}

/// Read a datagram demand watcher's current aggregate priority.
///
/// # Safety
/// `present` and `out` must point to writable values.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_datagram_demand_priority(watcher: u32, present: *mut bool, out: *mut i8) -> i32 {
	ffi::enter(|| {
		let value = State::lock()
			.publish
			.datagram_demand_priority(ffi::parse_id(watcher)?)?;
		unsafe { priority(value, present, out) }
	})
}

/// Cancel a datagram demand watcher; its terminal callback still fires.
#[unsafe(no_mangle)]
pub extern "C" fn moq_datagram_demand_cancel(watcher: u32) -> i32 {
	ffi::enter(|| State::lock().publish.datagram_demand_close(ffi::parse_id(watcher)?))
}

/// Serve on-demand datagram tracks, yielding request handles and then one terminal status.
///
/// This handler is independent of stream-track requests. Cancel with `moq_publish_requests_cancel`.
///
/// # Safety
/// `user_data` must remain valid until the callback receives its terminal status.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_publish_datagrams_requests(
	broadcast: u32,
	callback: ffi::moq_status_callback,
	user_data: *mut c_void,
) -> i32 {
	ffi::enter(|| {
		let broadcast = ffi::parse_id(broadcast)?;
		let callback = unsafe { ffi::OnStatus::new(user_data, callback)? };
		State::lock().publish.datagrams_dynamic(broadcast, callback)
	})
}

/// Borrow a datagram request's name until it is accepted, aborted, or freed.
///
/// # Safety
/// `dst` must point to a writable string record.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_datagram_request_name(request: u32, dst: *mut moq_string) -> i32 {
	ffi::enter(|| {
		let request = ffi::parse_id(request)?;
		let dst = unsafe { dst.as_mut() }.ok_or(Error::InvalidPointer)?;
		State::lock().publish.datagram_request_name(request, dst)
	})
}

/// Read the highest priority among consumers waiting for this datagram request.
///
/// # Safety
/// `present` and `out` must point to writable values.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_datagram_request_priority(request: u32, present: *mut bool, out: *mut i8) -> i32 {
	ffi::enter(|| {
		let value = State::lock()
			.publish
			.datagram_request_priority(ffi::parse_id(request)?)?;
		unsafe { priority(value, present, out) }
	})
}

/// Accept a datagram request, releasing it and returning its positive producer handle.
///
/// # Safety
/// `properties` must be NULL or point to a valid datagram info record.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_datagram_request_accept(request: u32, properties: *const moq_datagram_info) -> i32 {
	ffi::enter(|| {
		let request = ffi::parse_id(request)?;
		let info = unsafe { info(properties)? };
		State::lock().publish.datagram_request_accept(request, info)
	})
}

/// Reject a datagram request with an application error and release it.
#[unsafe(no_mangle)]
pub extern "C" fn moq_datagram_request_abort(request: u32, error_code: u16) -> i32 {
	ffi::enter(|| {
		State::lock()
			.publish
			.datagram_request_abort(ffi::parse_id(request)?, error_code)
	})
}

/// Release a datagram request, rejecting consumers still waiting for it.
#[unsafe(no_mangle)]
pub extern "C" fn moq_datagram_request_free(request: u32) -> i32 {
	ffi::enter(|| State::lock().publish.datagram_request_free(ffi::parse_id(request)?))
}

/// Change a datagram consumer's subscriber priority, higher first.
#[unsafe(no_mangle)]
pub extern "C" fn moq_consume_datagrams_set_priority(consumer: u32, priority: i8) -> i32 {
	ffi::enter(|| {
		State::lock()
			.consume
			.datagram_track_priority(ffi::parse_id(consumer)?, priority)
	})
}

/// Read accepted publisher properties, returning 1 when available or 0 while still pending.
///
/// # Safety
/// `dst` must point to a writable datagram info record.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_consume_datagrams_info(consumer: u32, dst: *mut moq_datagram_info) -> i32 {
	ffi::enter(|| {
		let dst = unsafe { dst.as_mut() }.ok_or(Error::InvalidPointer)?;
		let info = State::lock().consume.datagram_track_info(ffi::parse_id(consumer)?)?;
		match info {
			Some(info) => {
				*dst = moq_datagram_info {
					priority: info.priority,
					timescale: info.timescale.map(|scale| scale.as_u64()).unwrap_or_default(),
				};
				Ok(1)
			}
			None => Ok(0),
		}
	})
}
