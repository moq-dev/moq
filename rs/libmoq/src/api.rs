use crate::Error;
use crate::{ffi, CONSUME, ORIGIN, PUBLISH, SESSION};

use std::ffi::c_char;
use std::ffi::c_void;
use std::str::FromStr;

use tracing::Level;

/// Information about a video rendition in the catalog.
#[repr(C)]
#[allow(non_camel_case_types)]
pub struct moq_video_track {
	/// The name of the track, NOT NULL terminated.
	pub name: *const c_char,
	pub name_len: usize,

	/// The codec of the track, NOT NULL terminated
	pub codec: *const c_char,
	pub codec_len: usize,

	/// The description of the track, or NULL if not used.
	/// This is codec specific, for example H264:
	///   - NULL: annex.b encoded
	///   - Non-NULL: AVCC encoded
	pub description: *const u8,
	pub description_len: usize,

	/// The encoded width/height of the media, or NULL if not available
	pub coded_width: *const u32,
	pub coded_height: *const u32,
}

/// Information about an audio rendition in the catalog.
#[repr(C)]
#[allow(non_camel_case_types)]
pub struct moq_audio_track {
	/// The name of the track, NOT NULL terminated
	pub name: *const c_char,
	pub name_len: usize,

	/// The codec of the track, NOT NULL terminated
	pub codec: *const c_char,
	pub codec_len: usize,

	/// The description of the track, or NULL if not used.
	pub description: *const u8,
	pub description_len: usize,

	/// The sample rate of the track in Hz
	pub sample_rate: u32,

	/// The number of channels in the track
	pub channel_count: u32,
}

/// Information about a frame of media.
#[repr(C)]
#[allow(non_camel_case_types)]
pub struct moq_frame {
	/// The payload of the frame, or NULL/0 if the stream has ended
	pub payload: *const u8,
	pub payload_size: usize,

	// The presentation timestamp of the frame in microseconds
	pub timestamp_us: u64,

	/// Whether the frame is a keyframe (meaningless for audio)
	pub keyframe: bool,
}

/// Information about a broadcast announced by an origin.
#[repr(C)]
#[allow(non_camel_case_types)]
pub struct moq_announced {
	/// The path of the broadcast, NOT NULL terminated
	pub path: *const c_char,
	pub path_len: usize,

	/// Whether the broadcast is active or has ended
	/// This MUST toggle between true and false over the lifetime of the broadcast
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
/// - The caller must ensure that level is a valid pointer to level_len bytes of data.
#[no_mangle]
pub unsafe extern "C" fn moq_log_level(level: *const c_char, level_len: usize) -> i32 {
	ffi::return_code(move || {
		match unsafe { ffi::parse_str(level, level_len)? } {
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
/// Takes origin handles, which are used for publishing and consuming broadcasts respectively.
/// - Any broadcasts in `origin_publish` will be announced to the server.
/// - Any broadcasts announced by the server will be available in `origin_consume`.
/// - If an origin handle is 0, that functionality is completely disabled.
///
/// This may be called multiple times to connect to different servers.
/// Origins can be shared across sessions, useful for fanout or relaying.
///
/// Returns a non-zero handle to the session on success, or a negative code on (immediate) failure.
/// You should call [moq_session_close], even on error, to free up resources.
///
/// The callback is called on success (status 0) and later when closed (status non-zero).
///
/// # Safety
/// - The caller must ensure that url is a valid pointer to url_len bytes of data.
/// - The caller must ensure that `on_status` is valid until [moq_session_close] is called.
#[no_mangle]
pub unsafe extern "C" fn moq_session_connect(
	url: *const c_char,
	url_len: usize,
	origin_publish: i32,
	origin_consume: i32,
	on_status: Option<extern "C" fn(user_data: *mut c_void, code: i32)>,
	user_data: *mut c_void,
) -> i32 {
	ffi::return_code(move || {
		let url = ffi::parse_url(url, url_len)?;

		let origin = ORIGIN.lock();
		let publish = ffi::parse_id_optional(origin_publish)?
			.map(|id| origin.get(id))
			.transpose()?
			.map(|origin: &moq_lite::OriginProducer| origin.consume());
		let consume = ffi::parse_id_optional(origin_consume)?
			.map(|id| origin.get(id))
			.transpose()?
			.cloned();

		let on_status = ffi::OnStatus::new(user_data, on_status);
		SESSION.lock().connect(url, publish, consume, on_status)
	})
}

/// Close a connection to a MoQ server.
///
/// Returns a zero on success, or a negative code on failure.
///
/// The [moq_session_connect] `on_status` callback will be called with [Error::Closed].
#[no_mangle]
pub extern "C" fn moq_session_close(session: i32) -> i32 {
	ffi::return_code(move || {
		let session = ffi::parse_id(session)?;
		SESSION.lock().close(session)
	})
}

/// Create an origin for publishing broadcasts.
///
/// Origins contain any number of broadcasts addressed by path.
/// The same broadcast can be published to multiple origins under different paths.
///
/// [moq_origin_announced] can be used to discover broadcasts published to this origin.
/// This is extremely useful for discovering what is available on the server to [moq_origin_consume].
///
/// Returns a non-zero handle to the origin on success.
#[no_mangle]
pub extern "C" fn moq_origin_create() -> i32 {
	ffi::return_code(move || ORIGIN.lock().create())
}

/// Publish a broadcast to an origin.
///
/// The broadcast will be announced to any origin consumers, such as over the network.
///
/// Returns a zero on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure that path is a valid pointer to path_len bytes of data.
#[no_mangle]
pub unsafe extern "C" fn moq_origin_publish(origin: i32, path: *const c_char, path_len: usize, broadcast: i32) -> i32 {
	ffi::return_code(move || {
		let origin = ffi::parse_id(origin)?;
		let path = unsafe { ffi::parse_str(path, path_len)? };
		let broadcast = ffi::parse_id(broadcast)?;
		let broadcast = PUBLISH.lock().get(broadcast)?.consume();
		ORIGIN.lock().publish(origin, path, broadcast)
	})
}

/// Learn about all broadcasts published to an origin.
///
/// The callback is called with an announced ID when a new broadcast is published.
///
/// - [moq_origin_announced_info] is used to query information about the broadcast.
/// - [moq_origin_announced_close] is used to stop receiving announcements.
///
/// Returns a non-zero handle on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure that `on_announce` is valid until [moq_origin_announced_close] is called.
#[no_mangle]
pub unsafe extern "C" fn moq_origin_announced(
	origin: i32,
	on_announce: Option<extern "C" fn(user_data: *mut c_void, announced: i32)>,
	user_data: *mut c_void,
) -> i32 {
	ffi::return_code(move || {
		let origin = ffi::parse_id(origin)?;
		let on_announce = ffi::OnStatus::new(user_data, on_announce);
		ORIGIN.lock().announced(origin, on_announce)
	})
}

/// Query information about a broadcast discovered by [moq_origin_announced].
///
/// The destination is filled with the broadcast information.
///
/// Returns a zero on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure that `dst` is a valid pointer to a [moq_announced] struct.
#[no_mangle]
pub unsafe extern "C" fn moq_origin_announced_info(announced: i32, dst: *mut moq_announced) -> i32 {
	ffi::return_code(move || {
		let announced = ffi::parse_id(announced)?;
		let dst = dst.as_mut().ok_or(Error::InvalidPointer)?;
		ORIGIN.lock().announced_info(announced, dst)
	})
}

/// Stop receiving announcements for broadcasts published to an origin.
///
/// Returns a zero on success, or a negative code on failure.
#[no_mangle]
pub extern "C" fn moq_origin_announced_close(announced: i32) -> i32 {
	ffi::return_code(move || {
		let announced = ffi::parse_id(announced)?;
		ORIGIN.lock().announced_close(announced)
	})
}

/// Consume a broadcast from an origin by path.
///
/// Returns a non-zero handle to the broadcast on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure that path is a valid pointer to path_len bytes of data.
#[no_mangle]
pub unsafe extern "C" fn moq_origin_consume(origin: i32, path: *const c_char, path_len: usize) -> i32 {
	ffi::return_code(move || {
		let origin = ffi::parse_id(origin)?;
		let path = unsafe { ffi::parse_str(path, path_len)? };
		let broadcast = ORIGIN.lock().consume(origin, path)?;
		Ok(CONSUME.lock().start(broadcast.into()))
	})
}

/// Close an origin and clean up its resources.
///
/// Returns a zero on success, or a negative code on failure.
#[no_mangle]
pub extern "C" fn moq_origin_close(origin: i32) -> i32 {
	ffi::return_code(move || {
		let origin = ffi::parse_id(origin)?;
		ORIGIN.lock().close(origin)
	})
}

/// Create a new broadcast for publishing media tracks.
///
/// Returns a non-zero handle to the broadcast on success, or a negative code on failure.
#[no_mangle]
pub extern "C" fn moq_publish_create() -> i32 {
	ffi::return_code(move || PUBLISH.lock().create())
}

/// Close a broadcast and clean up its resources.
///
/// Returns a zero on success, or a negative code on failure.
#[no_mangle]
pub extern "C" fn moq_publish_close(broadcast: i32) -> i32 {
	ffi::return_code(move || {
		let broadcast = ffi::parse_id(broadcast)?;
		PUBLISH.lock().close(broadcast)
	})
}

/// Create a new track for a broadcast.
///
/// The encoding of `init` depends on the `format` string.
///
/// Returns a non-zero handle to the track on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure that format is a valid pointer to format_len bytes of data.
/// - The caller must ensure that init is a valid pointer to init_size bytes of data.
#[no_mangle]
pub unsafe extern "C" fn moq_publish_media_init(
	broadcast: i32,
	format: *const c_char,
	format_len: usize,
	init: *const u8,
	init_size: usize,
) -> i32 {
	ffi::return_code(move || {
		let broadcast = ffi::parse_id(broadcast)?;
		let format = unsafe { ffi::parse_str(format, format_len)? };
		let init = unsafe { ffi::parse_slice(init, init_size)? };

		PUBLISH.lock().media_init(broadcast, format, init)
	})
}

/// Remove a track from a broadcast.
///
/// Returns a zero on success, or a negative code on failure.
#[no_mangle]
pub extern "C" fn moq_publish_media_close(export: i32) -> i32 {
	ffi::return_code(move || {
		let export = ffi::parse_id(export)?;
		PUBLISH.lock().media_close(export)
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
/// - The caller must ensure that payload is a valid pointer to payload_size bytes of data.
#[no_mangle]
pub unsafe extern "C" fn moq_publish_media_frame(
	media: i32,
	payload: *const u8,
	payload_size: usize,
	timestamp_us: u64,
) -> i32 {
	ffi::return_code(move || {
		let media = ffi::parse_id(media)?;
		let payload = unsafe { ffi::parse_slice(payload, payload_size)? };
		let timestamp = hang::Timestamp::from_micros(timestamp_us)?;
		PUBLISH.lock().media_frame(media, payload, timestamp)
	})
}

/// Create a catalog consumer for a broadcast.
///
/// The callback is called with a catalog ID when a new catalog is available.
/// The catalog ID can be used to query video/audio track information.
///
/// Returns a non-zero handle on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure that `on_catalog` is valid until [moq_consume_catalog_close] is called.
#[no_mangle]
pub unsafe extern "C" fn moq_consume_catalog(
	broadcast: i32,
	on_catalog: Option<extern "C" fn(user_data: *mut c_void, catalog: i32)>,
	user_data: *mut c_void,
) -> i32 {
	ffi::return_code(move || {
		let broadcast = ffi::parse_id(broadcast)?;
		let on_catalog = ffi::OnStatus::new(user_data, on_catalog);
		CONSUME.lock().catalog(broadcast, on_catalog)
	})
}

/// Query information about a video track in a catalog.
///
/// The destination is filled with the video track information.
///
/// Returns a zero on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure that `dst` is a valid pointer to a [moq_video_track] struct.
/// - The caller must ensure that `dst` is not used after [moq_consume_catalog_close] is called.
#[no_mangle]
pub unsafe extern "C" fn moq_consume_catalog_video(catalog: i32, index: i32, dst: *mut moq_video_track) -> i32 {
	ffi::return_code(move || {
		let catalog = ffi::parse_id(catalog)?;
		let index = index as usize;
		let dst = dst.as_mut().ok_or(Error::InvalidPointer)?;
		CONSUME.lock().catalog_video(catalog, index, dst)
	})
}

/// Query information about an audio track in a catalog.
///
/// The destination is filled with the audio track information.
///
/// Returns a zero on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure that `dst` is a valid pointer to a [moq_audio_track] struct.
/// - The caller must ensure that `dst` is not used after [moq_consume_catalog_close] is called.
#[no_mangle]
pub unsafe extern "C" fn moq_consume_catalog_audio(catalog: i32, index: i32, dst: *mut moq_audio_track) -> i32 {
	ffi::return_code(move || {
		let catalog = ffi::parse_id(catalog)?;
		let index = index as usize;
		let dst = dst.as_mut().ok_or(Error::InvalidPointer)?;
		CONSUME.lock().catalog_audio(catalog, index, dst)
	})
}

/// Close a catalog consumer and clean up its resources.
///
/// Returns a zero on success, or a negative code on failure.
#[no_mangle]
pub extern "C" fn moq_consume_catalog_close(catalog: i32) -> i32 {
	ffi::return_code(move || {
		let catalog = ffi::parse_id(catalog)?;
		CONSUME.lock().catalog_close(catalog)
	})
}

/// Consume a video track from a broadcast.
///
/// - `max_buffer_ms` controls the maximum amount of buffering allowed before skipping a GoP.
/// - `on_frame` is called with a frame ID when a new frame is available.
///
/// Returns a non-zero handle to the track on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure that `on_frame` is valid until [moq_consume_video_track_close] is called.
#[no_mangle]
pub unsafe extern "C" fn moq_consume_video_track(
	broadcast: i32,
	index: i32,
	max_buffer_ms: u64,
	on_frame: Option<extern "C" fn(user_data: *mut c_void, frame: i32)>,
	user_data: *mut c_void,
) -> i32 {
	ffi::return_code(move || {
		let broadcast = ffi::parse_id(broadcast)?;
		let index = index as usize;
		let max_buffer = std::time::Duration::from_millis(max_buffer_ms);
		let on_frame = ffi::OnStatus::new(user_data, on_frame);
		CONSUME.lock().video_track(broadcast, index, max_buffer, on_frame)
	})
}

/// Close a video track consumer and clean up its resources.
///
/// Returns a zero on success, or a negative code on failure.
#[no_mangle]
pub extern "C" fn moq_consume_video_track_close(track: i32) -> i32 {
	ffi::return_code(move || {
		let track = ffi::parse_id(track)?;
		CONSUME.lock().video_track_close(track)
	})
}

/// Consume an audio track from a broadcast.
///
/// The callback is called with a frame ID when a new frame is available.
/// The latency_ms parameter controls how much buffering to apply.
///
/// Returns a non-zero handle to the track on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure that `on_frame` is valid until [moq_consume_audio_track_close] is called.
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
		CONSUME.lock().audio_track(broadcast, index, latency, on_frame)
	})
}

/// Close an audio track consumer and clean up its resources.
///
/// Returns a zero on success, or a negative code on failure.
#[no_mangle]
pub extern "C" fn moq_consume_audio_track_close(track: i32) -> i32 {
	ffi::return_code(move || {
		let track = ffi::parse_id(track)?;
		CONSUME.lock().audio_track_close(track)
	})
}

/// Get a chunk of a frame's payload.
///
/// Frames may be split into multiple chunks. Call this multiple times with increasing
/// index values to get all chunks. The destination is filled with the frame chunk information.
///
/// Returns a zero on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure that `dst` is a valid pointer to a [moq_frame] struct.
#[no_mangle]
pub unsafe extern "C" fn moq_consume_frame_chunk(frame: i32, index: i32, dst: *mut moq_frame) -> i32 {
	ffi::return_code(move || {
		let frame = ffi::parse_id(frame)?;
		let index = index as usize;
		let dst = dst.as_mut().ok_or(Error::InvalidPointer)?;
		CONSUME.lock().frame_chunk(frame, index, dst)
	})
}

/// Close a frame and clean up its resources.
///
/// Returns a zero on success, or a negative code on failure.
#[no_mangle]
pub extern "C" fn moq_consume_frame_close(frame: i32) -> i32 {
	ffi::return_code(move || {
		let frame = ffi::parse_id(frame)?;
		CONSUME.lock().frame_close(frame)
	})
}

/// Close a broadcast consumer and clean up its resources.
///
/// Returns a zero on success, or a negative code on failure.
#[no_mangle]
pub extern "C" fn moq_consume_close(consume: i32) -> i32 {
	ffi::return_code(move || {
		let consume = ffi::parse_id(consume)?;
		CONSUME.lock().close(consume)
	})
}
