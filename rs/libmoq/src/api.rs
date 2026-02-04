use crate::{Error, State, ffi};

use std::ffi::c_char;
use std::ffi::c_void;
use std::str::FromStr;

use tracing::Level;

/// Information about a video rendition in the catalog.
#[repr(C)]
#[allow(non_camel_case_types)]
pub struct moq_video_config {
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
pub struct moq_audio_config {
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

	/// The presentation timestamp of the frame in microseconds
	pub timestamp_us: u64,

	/// Whether the frame is a keyframe, aka the start of a new group.
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
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_log_level(level: *const c_char, level_len: usize) -> i32 {
	ffi::enter(move || {
		match unsafe { ffi::parse_str(level, level_len)? } {
			"" => moq_native::Log::default(),
			level => moq_native::Log::new(Level::from_str(level)?),
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
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_session_connect(
	url: *const c_char,
	url_len: usize,
	origin_publish: u32,
	origin_consume: u32,
	on_status: Option<extern "C" fn(user_data: *mut c_void, code: i32)>,
	user_data: *mut c_void,
) -> i32 {
	ffi::enter(move || {
		let url = ffi::parse_url(url, url_len)?;

		let mut state = State::lock();
		let publish = ffi::parse_id_optional(origin_publish)?
			.map(|id| state.origin.get(id))
			.transpose()?
			.map(|origin: &moq_lite::OriginProducer| origin.consume());
		let consume = ffi::parse_id_optional(origin_consume)?
			.map(|id| state.origin.get(id))
			.transpose()?
			.cloned();

		let on_status = unsafe { ffi::OnStatus::new(user_data, on_status) };
		state.session.connect(url, publish, consume, on_status)
	})
}

/// Close a connection to a MoQ server.
///
/// Returns a zero on success, or a negative code on failure.
///
/// The [moq_session_connect] `on_status` callback will be called with [Error::Closed].
#[unsafe(no_mangle)]
pub extern "C" fn moq_session_close(session: u32) -> i32 {
	ffi::enter(move || {
		let session = ffi::parse_id(session)?;
		State::lock().session.close(session)
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
#[unsafe(no_mangle)]
pub extern "C" fn moq_origin_create() -> i32 {
	ffi::enter(move || State::lock().origin.create())
}

/// Publish a broadcast to an origin.
///
/// The broadcast will be announced to any origin consumers, such as over the network.
///
/// Returns a zero on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure that path is a valid pointer to path_len bytes of data.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_origin_publish(origin: u32, path: *const c_char, path_len: usize, broadcast: u32) -> i32 {
	ffi::enter(move || {
		let origin = ffi::parse_id(origin)?;
		let path = unsafe { ffi::parse_str(path, path_len)? };
		let broadcast = ffi::parse_id(broadcast)?;

		let mut state = State::lock();
		let broadcast = state.publish.get(broadcast)?.consume();
		state.origin.publish(origin, path, broadcast)
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
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_origin_announced(
	origin: u32,
	on_announce: Option<extern "C" fn(user_data: *mut c_void, announced: i32)>,
	user_data: *mut c_void,
) -> i32 {
	ffi::enter(move || {
		let origin = ffi::parse_id(origin)?;
		let on_announce = unsafe { ffi::OnStatus::new(user_data, on_announce) };
		State::lock().origin.announced(origin, on_announce)
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
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_origin_announced_info(announced: u32, dst: *mut moq_announced) -> i32 {
	ffi::enter(move || {
		let announced = ffi::parse_id(announced)?;
		let dst = unsafe { dst.as_mut() }.ok_or(Error::InvalidPointer)?;
		State::lock().origin.announced_info(announced, dst)
	})
}

/// Stop receiving announcements for broadcasts published to an origin.
///
/// Returns a zero on success, or a negative code on failure.
#[unsafe(no_mangle)]
pub extern "C" fn moq_origin_announced_close(announced: u32) -> i32 {
	ffi::enter(move || {
		let announced = ffi::parse_id(announced)?;
		State::lock().origin.announced_close(announced)
	})
}

/// Consume a broadcast from an origin by path.
///
/// Returns a non-zero handle to the broadcast on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure that path is a valid pointer to path_len bytes of data.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_origin_consume(origin: u32, path: *const c_char, path_len: usize) -> i32 {
	ffi::enter(move || {
		let origin = ffi::parse_id(origin)?;
		let path = unsafe { ffi::parse_str(path, path_len)? };

		let mut state = State::lock();
		let broadcast = state.origin.consume(origin, path)?;
		Ok(state.consume.start(broadcast))
	})
}

/// Close an origin and clean up its resources.
///
/// Returns a zero on success, or a negative code on failure.
#[unsafe(no_mangle)]
pub extern "C" fn moq_origin_close(origin: u32) -> i32 {
	ffi::enter(move || {
		let origin = ffi::parse_id(origin)?;
		State::lock().origin.close(origin)
	})
}

/// Create a new broadcast for publishing media tracks.
///
/// Returns a non-zero handle to the broadcast on success, or a negative code on failure.
#[unsafe(no_mangle)]
pub extern "C" fn moq_publish_create() -> i32 {
	ffi::enter(move || State::lock().publish.create())
}

/// Close a broadcast and clean up its resources.
///
/// Returns a zero on success, or a negative code on failure.
#[unsafe(no_mangle)]
pub extern "C" fn moq_publish_close(broadcast: u32) -> i32 {
	ffi::enter(move || {
		let broadcast = ffi::parse_id(broadcast)?;
		State::lock().publish.close(broadcast)
	})
}

/// Create a new media track for a broadcast
///
/// All frames in [moq_publish_media_frame] must be written in decode order.
/// The `format` controls the encoding, both of `init` and frame payloads.
///
/// Returns a non-zero handle to the track on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure that format is a valid pointer to format_len bytes of data.
/// - The caller must ensure that init is a valid pointer to init_size bytes of data.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_publish_media_ordered(
	broadcast: u32,
	format: *const c_char,
	format_len: usize,
	init: *const u8,
	init_size: usize,
) -> i32 {
	ffi::enter(move || {
		let broadcast = ffi::parse_id(broadcast)?;
		let format = unsafe { ffi::parse_str(format, format_len)? };
		let init = unsafe { ffi::parse_slice(init, init_size)? };

		State::lock().publish.media_ordered(broadcast, format, init)
	})
}

/// Remove a track from a broadcast.
///
/// Returns a zero on success, or a negative code on failure.
#[unsafe(no_mangle)]
pub extern "C" fn moq_publish_media_close(export: u32) -> i32 {
	ffi::enter(move || {
		let export = ffi::parse_id(export)?;
		State::lock().publish.media_close(export)
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
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_publish_media_frame(
	media: u32,
	payload: *const u8,
	payload_size: usize,
	timestamp_us: u64,
) -> i32 {
	ffi::enter(move || {
		let media = ffi::parse_id(media)?;
		let payload = unsafe { ffi::parse_slice(payload, payload_size)? };
		let timestamp = hang::container::Timestamp::from_micros(timestamp_us)?;
		State::lock().publish.media_frame(media, payload, timestamp)
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
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_consume_catalog(
	broadcast: u32,
	on_catalog: Option<extern "C" fn(user_data: *mut c_void, catalog: i32)>,
	user_data: *mut c_void,
) -> i32 {
	ffi::enter(move || {
		let broadcast = ffi::parse_id(broadcast)?;
		let on_catalog = unsafe { ffi::OnStatus::new(user_data, on_catalog) };
		State::lock().consume.catalog(broadcast, on_catalog)
	})
}

/// Close a catalog consumer and clean up its resources.
///
/// Returns a zero on success, or a negative code on failure.
#[unsafe(no_mangle)]
pub extern "C" fn moq_consume_catalog_close(catalog: u32) -> i32 {
	ffi::enter(move || {
		let catalog = ffi::parse_id(catalog)?;
		State::lock().consume.catalog_close(catalog)
	})
}

/// Query information about a video track in a catalog.
///
/// The destination is filled with the video track information.
///
/// Returns a zero on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure that `dst` is a valid pointer to a [moq_video_config] struct.
/// - The caller must ensure that `dst` is not used after [moq_consume_catalog_close] is called.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_consume_video_config(catalog: u32, index: u32, dst: *mut moq_video_config) -> i32 {
	ffi::enter(move || {
		let catalog = ffi::parse_id(catalog)?;
		let index = index as usize;
		let dst = unsafe { dst.as_mut() }.ok_or(Error::InvalidPointer)?;
		State::lock().consume.video_config(catalog, index, dst)
	})
}

/// Query information about an audio track in a catalog.
///
/// The destination is filled with the audio track information.
///
/// Returns a zero on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure that `dst` is a valid pointer to a [moq_audio_config] struct.
/// - The caller must ensure that `dst` is not used after [moq_consume_catalog_close] is called.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_consume_audio_config(catalog: u32, index: u32, dst: *mut moq_audio_config) -> i32 {
	ffi::enter(move || {
		let catalog = ffi::parse_id(catalog)?;
		let index = index as usize;
		let dst = unsafe { dst.as_mut() }.ok_or(Error::InvalidPointer)?;
		State::lock().consume.audio_config(catalog, index, dst)
	})
}

/// Consume a video track from a broadcast, delivering frames in order.
///
/// - `max_latency_ms` controls the maximum amount of buffering allowed before skipping a GoP.
/// - `on_frame` is called with a frame ID when a new frame is available.
///
/// Returns a non-zero handle to the track on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure that `on_frame` is valid until the track is closed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_consume_video_ordered(
	broadcast: u32,
	index: u32,
	max_latency_ms: u64,
	on_frame: Option<extern "C" fn(user_data: *mut c_void, frame: i32)>,
	user_data: *mut c_void,
) -> i32 {
	ffi::enter(move || {
		let broadcast = ffi::parse_id(broadcast)?;
		let index = index as usize;
		let max_latency = std::time::Duration::from_millis(max_latency_ms);
		let on_frame = unsafe { ffi::OnStatus::new(user_data, on_frame) };
		State::lock()
			.consume
			.video_ordered(broadcast, index, max_latency, on_frame)
	})
}

/// Close a video track consumer and clean up its resources.
///
/// Returns a zero on success, or a negative code on failure.
#[unsafe(no_mangle)]
pub extern "C" fn moq_consume_video_close(track: u32) -> i32 {
	ffi::enter(move || {
		let track = ffi::parse_id(track)?;
		State::lock().consume.video_close(track)
	})
}

/// Consume an audio track from a broadcast, emitting the frames in order.
///
/// The callback is called with a frame ID when a new frame is available.
/// The `max_latency_ms` parameter controls how long to wait before skipping frames.
///
/// Returns a non-zero handle to the track on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure that `on_frame` is valid until [moq_consume_audio_close] is called.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_consume_audio_ordered(
	broadcast: u32,
	index: u32,
	max_latency_ms: u64,
	on_frame: Option<extern "C" fn(user_data: *mut c_void, frame: i32)>,
	user_data: *mut c_void,
) -> i32 {
	ffi::enter(move || {
		let broadcast = ffi::parse_id(broadcast)?;
		let index = index as usize;
		let max_latency = std::time::Duration::from_millis(max_latency_ms);
		let on_frame = unsafe { ffi::OnStatus::new(user_data, on_frame) };
		State::lock()
			.consume
			.audio_ordered(broadcast, index, max_latency, on_frame)
	})
}

/// Close an audio track consumer and clean up its resources.
///
/// Returns a zero on success, or a negative code on failure.
#[unsafe(no_mangle)]
pub extern "C" fn moq_consume_audio_close(track: u32) -> i32 {
	ffi::enter(move || {
		let track = ffi::parse_id(track)?;
		State::lock().consume.audio_close(track)
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
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_consume_frame_chunk(frame: u32, index: u32, dst: *mut moq_frame) -> i32 {
	ffi::enter(move || {
		let frame = ffi::parse_id(frame)?;
		let index = index as usize;
		let dst = unsafe { dst.as_mut() }.ok_or(Error::InvalidPointer)?;
		State::lock().consume.frame_chunk(frame, index, dst)
	})
}

/// Close a frame and clean up its resources.
///
/// Returns a zero on success, or a negative code on failure.
#[unsafe(no_mangle)]
pub extern "C" fn moq_consume_frame_close(frame: u32) -> i32 {
	ffi::enter(move || {
		let frame = ffi::parse_id(frame)?;
		State::lock().consume.frame_close(frame)
	})
}

/// Close a broadcast consumer and clean up its resources.
///
/// Returns a zero on success, or a negative code on failure.
#[unsafe(no_mangle)]
pub extern "C" fn moq_consume_close(consume: u32) -> i32 {
	ffi::enter(move || {
		let consume = ffi::parse_id(consume)?;
		State::lock().consume.close(consume)
	})
}
