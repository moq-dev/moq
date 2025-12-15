use crate::ffi;
use crate::state::*;
use crate::Error;

use std::ffi::c_char;
use std::ffi::c_void;
use std::str::FromStr;

use tracing::Level;

#[repr(C)]
pub struct VideoTrack {
	pub name: *const c_char,
	pub name_len: usize,
	pub codec: *const c_char,
	pub codec_len: usize,
	pub description: Option<*const u8>,
	pub description_len: usize,
	pub coded_width: Option<u32>,
	pub coded_height: Option<u32>,
}

#[repr(C)]
pub struct AudioTrack {
	pub name: *const c_char,
	pub name_len: usize,
	pub codec: *const c_char,
	pub codec_len: usize,
	pub description: Option<*const u8>,
	pub description_len: usize,
	pub sample_rate: u32,
	pub channel_count: u32,
}

#[repr(C)]
pub struct Frame {
	pub payload: *const u8,
	pub payload_size: usize,

	// microseconds
	pub pts: u64,

	pub keyframe: bool,
}

#[repr(C)]
pub struct Announced {
	pub path: *const c_char,
	pub path_len: usize,
	pub active: bool,
}

/// Initialize the library with a log level.
///
/// This should be called before any other functions.
/// The log_level is a string: "error", "warn", "info", "debug", "trace"
///
/// Returns a zero on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure that level is a valid null-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn moq_log_level(level: *const c_char) -> i32 {
	ffi::return_code(move || {
		match ffi::parse_str(level)? {
			"" => moq_native::Log::default(),
			level => moq_native::Log {
				level: Level::from_str(level)?,
			},
		}
		.init();

		Ok(())
	})
}

/// Start establishing a connection to a MoQ server.
///
/// This may be called multiple times to connect to different servers.
/// Broadcast may be published before or after the connection is established.
///
/// Returns a non-zero handle to the session on success, or a negative code on (immediate) failure.
/// You should call [moq_session_close], even on error, to free up resources.
///
/// The callback is called on success (status 0) and later when closed (status non-zero).
///
/// # Safety
/// - The caller must ensure that url is a valid null-terminated C string.
/// - The caller must ensure that callback is a valid function pointer, or null.
/// - The caller must ensure that user_data is a valid pointer.
#[no_mangle]
pub unsafe extern "C" fn moq_session_connect(
	url: *const c_char,
	origin_publish: i32,
	origin_consume: i32,
	on_status: Option<extern "C" fn(user_data: *mut c_void, code: i32)>,
	user_data: *mut c_void,
) -> i32 {
	ffi::return_code(move || {
		let url = ffi::parse_url(url)?;
		let origin_publish = ffi::parse_id_optional(origin_publish)?;
		let origin_consume = ffi::parse_id_optional(origin_consume)?;
		let on_status = ffi::OnStatus::new(user_data, on_status);
		State::lock().session_connect(url, origin_publish, origin_consume, on_status)
	})
}

/// Close a connection to a MoQ server.
///
/// Returns a zero on success, or a negative code on failure.
///
/// The [moq_session_connect] callback will be called with [Error::Closed].
#[no_mangle]
pub extern "C" fn moq_session_close(session: i32) -> i32 {
	ffi::return_code(move || {
		let session = ffi::parse_id(session)?;
		State::lock().session_close(session)
	})
}

/// Create an origin for publishing broadcasts.
///
/// Sessions
///
/// Returns a non-zero handle to the origin on success.
#[no_mangle]
pub extern "C" fn moq_origin_create() -> i32 {
	ffi::return_code(move || State::lock().origin_create())
}

#[no_mangle]
pub unsafe extern "C" fn moq_origin_publish(origin: i32, path: *const c_char, broadcast: i32) -> i32 {
	ffi::return_code(move || {
		let origin = ffi::parse_id(origin)?;
		let path = ffi::parse_str(path)?;
		let broadcast = ffi::parse_id(broadcast)?;
		State::lock().origin_publish(origin, path, broadcast)
	})
}

#[no_mangle]
pub unsafe extern "C" fn moq_origin_announced(
	origin: i32,
	on_announce: Option<extern "C" fn(user_data: *mut c_void, announced: i32)>,
	user_data: *mut c_void,
) -> i32 {
	ffi::return_code(move || {
		let origin = ffi::parse_id(origin)?;
		let on_announce = ffi::OnStatus::new(user_data, on_announce);
		State::lock().origin_announced(origin, on_announce)
	})
}

#[no_mangle]
pub unsafe extern "C" fn moq_origin_announced_info(announced: i32, dst: *mut Announced) -> i32 {
	ffi::return_code(move || {
		let announced = ffi::parse_id(announced)?;
		let dst = dst.as_mut().ok_or(Error::InvalidPointer)?;
		State::lock().origin_announced_info(announced, dst)
	})
}

#[no_mangle]
pub extern "C" fn moq_origin_announced_close(announced: i32) -> i32 {
	ffi::return_code(move || {
		let announced = ffi::parse_id(announced)?;
		State::lock().origin_announced_close(announced)
	})
}

#[no_mangle]
pub unsafe extern "C" fn moq_origin_consume(origin: i32, path: *const c_char) -> i32 {
	ffi::return_code(move || {
		let origin = ffi::parse_id(origin)?;
		let path = ffi::parse_str(path)?;
		State::lock().origin_consume(origin, path)
	})
}

#[no_mangle]
pub extern "C" fn moq_origin_close(origin: i32) -> i32 {
	ffi::return_code(move || {
		let origin = ffi::parse_id(origin)?;
		State::lock().origin_close(origin)
	})
}

#[no_mangle]
pub extern "C" fn moq_broadcast_create() -> i32 {
	ffi::return_code(move || State::lock().publish_create())
}

#[no_mangle]
pub extern "C" fn moq_broadcast_close(broadcast: i32) -> i32 {
	ffi::return_code(move || {
		let broadcast = ffi::parse_id(broadcast)?;
		State::lock().publish_close(broadcast)
	})
}

/// Create a new track for a broadcast.
///
/// The encoding of `extra` depends on the `format`.
/// See [hang::import::Generic] for the available formats.
///
/// Returns a non-zero handle to the track on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure that format is a valid null-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn moq_publish_media_init(
	broadcast: i32,
	format: *const c_char,
	init: *const u8,
	init_size: usize,
) -> i32 {
	ffi::return_code(move || {
		let broadcast = ffi::parse_id(broadcast)?;
		let format = ffi::parse_str(format)?;
		let init = ffi::parse_slice(init, init_size)?;

		State::lock().publish_media_init(broadcast, format, init)
	})
}

/// Remove a track from a broadcast.
#[no_mangle]
pub extern "C" fn moq_publish_media_close(export: i32) -> i32 {
	ffi::return_code(move || {
		let export = ffi::parse_id(export)?;
		State::lock().publish_media_close(export)
	})
}

/// Write data to a track.
///
/// The encoding of `data` depends on the track `format`.
/// The timestamp is in microseconds.
///
/// Returns a zero on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure that data is a valid pointer, or null.
#[no_mangle]
pub unsafe extern "C" fn moq_publish_media_frame(media: i32, frame: Frame) -> i32 {
	ffi::return_code(move || {
		let media = ffi::parse_id(media)?;
		State::lock().publish_media_frame(media, frame)
	})
}

/// Create a catalog consumer for a broadcast.
///
/// The callback is called with a catalog ID when a new catalog is available.
/// The catalog ID can be used to query video/audio track information.
#[no_mangle]
pub unsafe extern "C" fn moq_consume_catalog(
	broadcast: i32,
	on_catalog: Option<extern "C" fn(user_data: *mut c_void, catalog: i32)>,
	user_data: *mut c_void,
) -> i32 {
	ffi::return_code(move || {
		let broadcast = ffi::parse_id(broadcast)?;
		let on_catalog = ffi::OnStatus::new(user_data, on_catalog);
		State::lock().consume_catalog(broadcast, on_catalog)
	})
}

#[no_mangle]
pub unsafe extern "C" fn moq_consume_catalog_video(catalog: i32, index: i32, dst: *mut VideoTrack) -> i32 {
	ffi::return_code(move || {
		let catalog = ffi::parse_id(catalog)?;
		let index = index as usize;
		let dst = dst.as_mut().ok_or(Error::InvalidPointer)?;
		State::lock().consume_catalog_video(catalog, index, dst)
	})
}

#[no_mangle]
pub unsafe extern "C" fn moq_consume_catalog_audio(catalog: i32, index: i32, dst: *mut AudioTrack) -> i32 {
	ffi::return_code(move || {
		let catalog = ffi::parse_id(catalog)?;
		let index = index as usize;
		let dst = dst.as_mut().ok_or(Error::InvalidPointer)?;
		State::lock().consume_catalog_audio(catalog, index, dst)
	})
}

#[no_mangle]
pub extern "C" fn moq_consume_catalog_close(catalog: i32) -> i32 {
	ffi::return_code(move || {
		let catalog = ffi::parse_id(catalog)?;
		State::lock().consume_catalog_close(catalog)
	})
}

#[no_mangle]
pub unsafe extern "C" fn moq_consume_video_track(
	broadcast: i32,
	index: i32,
	latency_ms: u64,
	on_frame: Option<extern "C" fn(user_data: *mut c_void, frame: i32)>,
	user_data: *mut c_void,
) -> i32 {
	ffi::return_code(move || {
		let broadcast = ffi::parse_id(broadcast)?;
		let index = index as usize;
		let latency = std::time::Duration::from_millis(latency_ms);
		let on_frame = ffi::OnStatus::new(user_data, on_frame);
		State::lock().consume_video_track(broadcast, index, latency, on_frame)
	})
}

#[no_mangle]
pub extern "C" fn moq_consume_video_track_close(track: i32) -> i32 {
	ffi::return_code(move || {
		let track = ffi::parse_id(track)?;
		State::lock().consume_video_track_close(track)
	})
}

#[no_mangle]
pub unsafe extern "C" fn moq_consume_audio_track(
	broadcast: i32,
	index: i32,
	latency_ms: u64,
	on_frame: Option<extern "C" fn(user_data: *mut c_void, frame: i32)>,
	user_data: *mut c_void,
) -> i32 {
	ffi::return_code(move || {
		let broadcast = ffi::parse_id(broadcast)?;
		let index = index as usize;
		let latency = std::time::Duration::from_millis(latency_ms);
		let on_frame = ffi::OnStatus::new(user_data, on_frame);
		State::lock().consume_audio_track(broadcast, index, latency, on_frame)
	})
}

#[no_mangle]
pub extern "C" fn moq_consume_audio_track_close(track: i32) -> i32 {
	ffi::return_code(move || {
		let track = ffi::parse_id(track)?;
		State::lock().consume_audio_track_close(track)
	})
}

#[no_mangle]
pub unsafe extern "C" fn moq_consume_frame_chunk(frame: i32, index: i32, dst: *mut Frame) -> i32 {
	ffi::return_code(move || {
		let frame = ffi::parse_id(frame)?;
		let index = index as usize;
		let dst = dst.as_mut().ok_or(Error::InvalidPointer)?;
		State::lock().consume_frame_chunk(frame, index, dst)
	})
}

#[no_mangle]
pub extern "C" fn moq_consume_frame_close(frame: i32) -> i32 {
	ffi::return_code(move || {
		let frame = ffi::parse_id(frame)?;
		State::lock().consume_frame_close(frame)
	})
}

#[no_mangle]
pub extern "C" fn moq_consume_close(consume: i32) -> i32 {
	ffi::return_code(move || {
		let consume = ffi::parse_id(consume)?;
		State::lock().consume_close(consume)
	})
}
