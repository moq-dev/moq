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
		.init()
		.map_err(|err| Error::InitFailed(std::sync::Arc::new(err)))?;

		Ok(())
	})
}

/// Human-readable reason for the most recent failed call on the calling thread.
///
/// libmoq functions return only a negative code; this exposes the matching message
/// (including detail the code can't carry, e.g. which URL failed to parse or why a
/// decode failed). The string is only meaningful after a call returned a negative
/// code; check the code first.
///
/// Returns a NUL-terminated, UTF-8 pointer valid until the next libmoq call **on the
/// same thread**, or NULL if no error has been recorded on this thread. Copy it if you
/// need it to outlive the next call. Errors delivered through status callbacks carry
/// their code directly; read this from inside the callback to get their reason.
#[unsafe(no_mangle)]
pub extern "C" fn moq_error() -> *const c_char {
	ffi::last_error_ptr()
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
/// The session reconnects automatically with exponential backoff if the connection drops.
/// Published broadcasts are re-announced and consumers re-subscribed on each reconnect,
/// since the origins outlive the underlying connection.
///
/// `on_status` reports the session lifecycle through its status code:
/// - `> 0` on every (re)connect, carrying the connection epoch (`1` = first connect,
///   `2` = first reconnect, and so on), so a reconnect is distinguishable from the
///   initial connect. May fire repeatedly. Transient disconnects are not reported.
/// - `0` when the session is closed cleanly via [moq_session_close] (terminal).
/// - a negative error code if reconnection permanently gives up, e.g. the backoff
///   timeout is exceeded (terminal).
///
/// After a terminal (`<= 0`) status, `on_status` is never called again and `user_data`
/// is never touched again, so that final callback is the point to release `user_data`.
/// The terminal `0` fires even after [moq_session_close], so do not free `user_data` on
/// the close call itself.
///
/// # Safety
/// - The caller must ensure that url is a valid pointer to url_len bytes of data.
/// - The caller must keep `user_data` valid until the terminal (`<= 0`) `on_status` callback.
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
			.cloned();
		let consume = ffi::parse_id_optional(origin_consume)?
			.map(|id| state.origin.get(id))
			.transpose()?
			.cloned();

		let on_status = unsafe { ffi::OnStatus::new(user_data, on_status) };
		state.session.connect(url, publish, consume, on_status)
	})
}

/// Request that a session shut down.
///
/// Returns immediately: zero on success, or a negative code if the session is
/// unknown or already closing. Does NOT free `user_data`. The
/// [moq_session_connect] `on_status` callback still fires once more with a
/// terminal `0` (or a negative error), and that final callback is where
/// `user_data` should be released. Safe to call from any thread, including from
/// within `on_status`.
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
/// Returns a positive publish handle on success, or a negative code on failure. The broadcast
/// stays announced until the handle is passed to [moq_origin_unpublish]; closing the broadcast
/// itself does not unannounce it.
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

/// Unannounce a broadcast previously published with [moq_origin_publish].
///
/// Takes the publish handle returned by [moq_origin_publish]. Returns zero on success, or a
/// negative code on failure.
#[unsafe(no_mangle)]
pub extern "C" fn moq_origin_unpublish(publish: u32) -> i32 {
	ffi::enter(move || {
		let publish = ffi::parse_id(publish)?;
		State::lock().origin.unpublish(publish)
	})
}

/// Learn about all broadcasts published to an origin.
///
/// `on_announce` is invoked with a positive announced ID for each broadcast,
/// then exactly once more with a terminal code: `0` (stopped cleanly) or a
/// negative error. After the terminal (`<= 0`) callback, `on_announce` is never
/// called again and `user_data` is never touched again, so release `user_data`
/// there. The terminal callback fires even after [moq_origin_announced_close].
///
/// - [moq_origin_announced_info] is used to query information about the broadcast.
/// - [moq_origin_announced_close] is used to stop receiving announcements.
///
/// Returns a non-zero handle on success, or a negative code on failure.
///
/// # Safety
/// - The caller must keep `user_data` valid until the terminal (`<= 0`) `on_announce` callback.
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
/// Returns immediately: zero on success, or a negative code if already closed.
/// Does NOT free `user_data`. The [moq_origin_announced] `on_announce` callback
/// still fires once more with a terminal `0` (or a negative error), and that
/// final callback is where `user_data` should be released.
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
		state.consume.start(broadcast)
	})
}

/// Consume a broadcast from an origin by path, waiting until it is announced.
///
/// Unlike [moq_origin_consume], which fails immediately with a not-found code when the broadcast
/// has not been announced yet, this waits for the announcement to arrive (e.g. over the network)
/// and then delivers the broadcast handle via `on_broadcast`. Use it right after [moq_session_connect]
/// to avoid racing announcement gossip, instead of polling [moq_origin_consume] in a retry loop.
///
/// `on_broadcast` is invoked with a positive broadcast handle once announced, then exactly once
/// more with a terminal code: `0` (the wait finished, including after
/// [moq_origin_consume_announced_close]) or a negative error. After the terminal (`<= 0`) callback,
/// `on_broadcast` is never called again and `user_data` is never touched again, so release
/// `user_data` there. The broadcast handle is usable with [moq_consume_catalog] / [moq_consume_track]
/// and must be freed separately with [moq_consume_close].
///
/// Returns a non-zero handle to the wait on success, or a negative code on (immediate) failure.
///
/// # Safety
/// - The caller must ensure that path is a valid pointer to path_len bytes of data.
/// - The caller must keep `user_data` valid until the terminal (`<= 0`) `on_broadcast` callback.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_origin_consume_announced(
	origin: u32,
	path: *const c_char,
	path_len: usize,
	on_broadcast: Option<extern "C" fn(user_data: *mut c_void, broadcast: i32)>,
	user_data: *mut c_void,
) -> i32 {
	ffi::enter(move || {
		let origin = ffi::parse_id(origin)?;
		let path = unsafe { ffi::parse_str(path, path_len)? }.to_string();
		let on_broadcast = unsafe { ffi::OnStatus::new(user_data, on_broadcast) };
		State::lock().origin.consume_announced(origin, path, on_broadcast)
	})
}

/// Abort a wait started by [moq_origin_consume_announced].
///
/// Returns immediately: zero on success, or a negative code if already closed. Does NOT free
/// `user_data`. The [moq_origin_consume_announced] `on_broadcast` callback still fires once more
/// with a terminal `0` (or a negative error), and that final callback is where `user_data` should
/// be released. Any broadcast handle already delivered is unaffected and must still be freed with
/// [moq_consume_close].
#[unsafe(no_mangle)]
pub extern "C" fn moq_origin_consume_announced_close(task: u32) -> i32 {
	ffi::enter(move || {
		let task = ffi::parse_id(task)?;
		State::lock().origin.consume_announced_close(task)
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

/// Add or replace a video rendition in a broadcast's catalog.
///
/// This is the producer counterpart to [moq_consume_video_config]: instead of
/// reading a rendition out of a catalog, it writes one into the catalog of a
/// broadcast created with [moq_publish_create]. The rendition is keyed by
/// `config.name`; calling this again with the same name replaces it. The
/// updated catalog is published to subscribers automatically.
///
/// The struct fields are read as inputs:
/// - `name` / `codec` are required (NOT NULL terminated) string slices.
/// - `description` may be NULL to omit it.
/// - `coded_width` / `coded_height` may be NULL to omit them.
///
/// Returns a zero on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure that `config` points to a valid [moq_video_config].
/// - The caller must ensure each non-NULL pointer inside `config` is valid for its length.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_publish_video_config(broadcast: u32, config: *const moq_video_config) -> i32 {
	ffi::enter(move || {
		let broadcast = ffi::parse_id(broadcast)?;
		let config = unsafe { config.as_ref() }.ok_or(Error::InvalidPointer)?;

		let name = unsafe { ffi::parse_str(config.name, config.name_len)? };
		let codec = unsafe { ffi::parse_str(config.codec, config.codec_len)? };
		let codec = hang::catalog::VideoCodec::from_str(codec).map_err(Error::Hang)?;

		let mut video = hang::catalog::VideoConfig::new(codec);
		if !config.description.is_null() {
			let description = unsafe { ffi::parse_slice(config.description, config.description_len)? };
			video.description = Some(bytes::Bytes::copy_from_slice(description));
		}
		video.coded_width = unsafe { config.coded_width.as_ref() }.copied();
		video.coded_height = unsafe { config.coded_height.as_ref() }.copied();

		State::lock().publish.video_config(broadcast, name, video)
	})
}

/// Add or replace an audio rendition in a broadcast's catalog.
///
/// This is the producer counterpart to [moq_consume_audio_config]. The rendition
/// is keyed by `config.name`; calling this again with the same name replaces it.
/// The updated catalog is published to subscribers automatically.
///
/// The struct fields are read as inputs:
/// - `name` / `codec` are required (NOT NULL terminated) string slices.
/// - `sample_rate` / `channel_count` are required.
/// - `description` may be NULL to omit it.
///
/// Returns a zero on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure that `config` points to a valid [moq_audio_config].
/// - The caller must ensure each non-NULL pointer inside `config` is valid for its length.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_publish_audio_config(broadcast: u32, config: *const moq_audio_config) -> i32 {
	ffi::enter(move || {
		let broadcast = ffi::parse_id(broadcast)?;
		let config = unsafe { config.as_ref() }.ok_or(Error::InvalidPointer)?;

		let name = unsafe { ffi::parse_str(config.name, config.name_len)? };
		let codec = unsafe { ffi::parse_str(config.codec, config.codec_len)? };
		let codec = hang::catalog::AudioCodec::from_str(codec).map_err(Error::Hang)?;

		let mut audio = hang::catalog::AudioConfig::new(codec, config.sample_rate, config.channel_count);
		if !config.description.is_null() {
			let description = unsafe { ffi::parse_slice(config.description, config.description_len)? };
			audio.description = Some(bytes::Bytes::copy_from_slice(description));
		}

		State::lock().publish.audio_config(broadcast, name, audio)
	})
}

/// Remove a video rendition from a broadcast's catalog by name.
///
/// This is a no-op if no rendition with that name exists. The updated catalog is
/// published to subscribers automatically.
///
/// Returns a zero on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure that name is a valid pointer to name_len bytes of data.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_publish_video_remove(broadcast: u32, name: *const c_char, name_len: usize) -> i32 {
	ffi::enter(move || {
		let broadcast = ffi::parse_id(broadcast)?;
		let name = unsafe { ffi::parse_str(name, name_len)? };
		State::lock().publish.video_remove(broadcast, name)
	})
}

/// Remove an audio rendition from a broadcast's catalog by name.
///
/// This is a no-op if no rendition with that name exists. The updated catalog is
/// published to subscribers automatically.
///
/// Returns a zero on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure that name is a valid pointer to name_len bytes of data.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_publish_audio_remove(broadcast: u32, name: *const c_char, name_len: usize) -> i32 {
	ffi::enter(move || {
		let broadcast = ffi::parse_id(broadcast)?;
		let name = unsafe { ffi::parse_str(name, name_len)? };
		State::lock().publish.audio_remove(broadcast, name)
	})
}

/// Create a raw track on a broadcast for arbitrary byte payloads.
///
/// Unlike [moq_publish_media_ordered], this is the bare moq-net primitive: no
/// codec, container, or catalog framing. Frames written to it are delivered
/// as-is to subscribers using [moq_consume_track]. Use it for non-media tracks
/// (control channels, JSON metadata, etc.), or pair it with
/// [moq_publish_video_config] / [moq_publish_audio_config] to also describe the
/// track in the catalog.
///
/// Returns a non-zero handle to the track on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure that name is a valid pointer to name_len bytes of data.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_publish_track(broadcast: u32, name: *const c_char, name_len: usize) -> i32 {
	ffi::enter(move || {
		let broadcast = ffi::parse_id(broadcast)?;
		let name = unsafe { ffi::parse_str(name, name_len)? };
		State::lock().publish.track(broadcast, name)
	})
}

/// Append a new group to a raw track, returning a group producer.
///
/// Groups are delivered independently and each may contain any number of frames
/// written via [moq_publish_group_frame]. Sequence numbers auto-increment.
///
/// Returns a non-zero handle to the group on success, or a negative code on failure.
#[unsafe(no_mangle)]
pub extern "C" fn moq_publish_track_group(track: u32) -> i32 {
	ffi::enter(move || {
		let track = ffi::parse_id(track)?;
		State::lock().publish.track_group(track)
	})
}

/// Write a single-frame group to a raw track.
///
/// Convenience for the common one-frame-per-group pattern. Equivalent to
/// appending a group, writing one frame, and finishing it.
///
/// Returns a zero on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure that payload is a valid pointer to payload_size bytes of data.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_publish_track_frame(track: u32, payload: *const u8, payload_size: usize) -> i32 {
	ffi::enter(move || {
		let track = ffi::parse_id(track)?;
		let payload = unsafe { ffi::parse_slice(payload, payload_size)? };
		State::lock().publish.track_frame(track, payload)
	})
}

/// Finish a raw track. No more groups or frames can be written.
///
/// Returns a zero on success, or a negative code on failure.
#[unsafe(no_mangle)]
pub extern "C" fn moq_publish_track_close(track: u32) -> i32 {
	ffi::enter(move || {
		let track = ffi::parse_id(track)?;
		State::lock().publish.track_finish(track)
	})
}

/// Write a frame into a raw group created by [moq_publish_track_group].
///
/// Returns a zero on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure that payload is a valid pointer to payload_size bytes of data.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_publish_group_frame(group: u32, payload: *const u8, payload_size: usize) -> i32 {
	ffi::enter(move || {
		let group = ffi::parse_id(group)?;
		let payload = unsafe { ffi::parse_slice(payload, payload_size)? };
		State::lock().publish.group_frame(group, payload)
	})
}

/// Finish a raw group. No more frames can be written.
///
/// Returns a zero on success, or a negative code on failure.
#[unsafe(no_mangle)]
pub extern "C" fn moq_publish_group_close(group: u32) -> i32 {
	ffi::enter(move || {
		let group = ffi::parse_id(group)?;
		State::lock().publish.group_finish(group)
	})
}

/// Create a catalog consumer for a broadcast.
///
/// `on_catalog` is invoked with a positive catalog ID for each catalog update
/// (usable to query video/audio track information), then exactly once more with
/// a terminal code: `0` (closed cleanly) or a negative error. After the terminal
/// (`<= 0`) callback, `on_catalog` is never called again and `user_data` is never
/// touched again, so release `user_data` there. The terminal callback fires even
/// after [moq_consume_catalog_close].
///
/// Returns a non-zero handle on success, or a negative code on failure.
///
/// # Safety
/// - The caller must keep `user_data` valid until the terminal (`<= 0`) `on_catalog` callback.
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

/// Stop a catalog consumer's background subscription.
///
/// Returns immediately: zero on success, or a negative code if already closed.
/// Does NOT free `user_data`; the [moq_consume_catalog] callback still fires once
/// more with a terminal `0` (or a negative error), which is where `user_data`
/// should be released. Catalog snapshots previously delivered via the callback
/// remain valid until freed with [moq_consume_catalog_free].
#[unsafe(no_mangle)]
pub extern "C" fn moq_consume_catalog_close(catalog: u32) -> i32 {
	ffi::enter(move || {
		let catalog = ffi::parse_id(catalog)?;
		State::lock().consume.catalog_close(catalog)
	})
}

/// Free a catalog snapshot received via the [moq_consume_catalog] callback.
///
/// This releases the snapshot and invalidates any borrowed references (e.g. pointers
/// returned by [moq_consume_video_config] or [moq_consume_audio_config]).
///
/// Returns a zero on success, or a negative code on failure.
#[unsafe(no_mangle)]
pub extern "C" fn moq_consume_catalog_free(catalog: u32) -> i32 {
	ffi::enter(move || {
		let catalog = ffi::parse_id(catalog)?;
		State::lock().consume.catalog_free(catalog)
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
/// - The caller must ensure that `dst` is not used after [moq_consume_catalog_free] is called.
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
/// - The caller must ensure that `dst` is not used after [moq_consume_catalog_free] is called.
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
/// - `on_frame` is called with a positive frame ID per frame, then exactly once
///   more with a terminal code: `0` (closed cleanly) or a negative error. After
///   the terminal (`<= 0`) callback, `on_frame` is never called again and
///   `user_data` is never touched again, so release `user_data` there. The
///   terminal callback fires even after [moq_consume_video_close].
///
/// Returns a non-zero handle to the track on success, or a negative code on failure.
///
/// # Safety
/// - The caller must keep `user_data` valid until the terminal (`<= 0`) `on_frame` callback.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_consume_video_ordered(
	catalog: u32,
	index: u32,
	max_latency_ms: u64,
	on_frame: Option<extern "C" fn(user_data: *mut c_void, frame: i32)>,
	user_data: *mut c_void,
) -> i32 {
	ffi::enter(move || {
		let catalog = ffi::parse_id(catalog)?;
		let index = index as usize;
		let max_latency = std::time::Duration::from_millis(max_latency_ms);
		let on_frame = unsafe { ffi::OnStatus::new(user_data, on_frame) };
		State::lock()
			.consume
			.video_ordered(catalog, index, max_latency, on_frame)
	})
}

/// Stop a video track consumer's background task.
///
/// Returns immediately: zero on success, or a negative code if already closed.
/// Does NOT free `user_data`; the [moq_consume_video_ordered] `on_frame` callback
/// still fires once more with a terminal `0` (or a negative error), which is
/// where `user_data` should be released.
#[unsafe(no_mangle)]
pub extern "C" fn moq_consume_video_close(track: u32) -> i32 {
	ffi::enter(move || {
		let track = ffi::parse_id(track)?;
		State::lock().consume.track_close(track)
	})
}

/// Consume an audio track from a broadcast, emitting the frames in order.
///
/// `on_frame` is called with a positive frame ID per frame, then exactly once
/// more with a terminal code: `0` (closed cleanly) or a negative error. After
/// the terminal (`<= 0`) callback, `on_frame` is never called again and
/// `user_data` is never touched again, so release `user_data` there. The
/// terminal callback fires even after [moq_consume_audio_close].
/// The `max_latency_ms` parameter controls how long to wait before skipping frames.
///
/// Returns a non-zero handle to the track on success, or a negative code on failure.
///
/// # Safety
/// - The caller must keep `user_data` valid until the terminal (`<= 0`) `on_frame` callback.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_consume_audio_ordered(
	catalog: u32,
	index: u32,
	max_latency_ms: u64,
	on_frame: Option<extern "C" fn(user_data: *mut c_void, frame: i32)>,
	user_data: *mut c_void,
) -> i32 {
	ffi::enter(move || {
		let catalog = ffi::parse_id(catalog)?;
		let index = index as usize;
		let max_latency = std::time::Duration::from_millis(max_latency_ms);
		let on_frame = unsafe { ffi::OnStatus::new(user_data, on_frame) };
		State::lock()
			.consume
			.audio_ordered(catalog, index, max_latency, on_frame)
	})
}

/// Stop an audio track consumer's background task.
///
/// Returns immediately: zero on success, or a negative code if already closed.
/// Does NOT free `user_data`; the [moq_consume_audio_ordered] `on_frame` callback
/// still fires once more with a terminal `0` (or a negative error), which is
/// where `user_data` should be released.
#[unsafe(no_mangle)]
pub extern "C" fn moq_consume_audio_close(track: u32) -> i32 {
	ffi::enter(move || {
		let track = ffi::parse_id(track)?;
		State::lock().consume.track_close(track)
	})
}

/// Get a chunk of a frame's payload.
///
/// Read the payload of a frame as a single contiguous slice.
///
/// Frames are not chunked; the entire payload is delivered through `dst.payload` /
/// `dst.payload_size` in one call. The pointer is valid until [`moq_consume_frame_close`]
/// is called for this frame.
///
/// Returns a zero on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure that `dst` is a valid pointer to a [moq_frame] struct.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_consume_frame(frame: u32, dst: *mut moq_frame) -> i32 {
	ffi::enter(move || {
		let frame = ffi::parse_id(frame)?;
		let dst = unsafe { dst.as_mut() }.ok_or(Error::InvalidPointer)?;
		State::lock().consume.frame(frame, dst)
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

/// Subscribe to a raw track by name, delivering each frame's payload as-is.
///
/// This is the counterpart to [moq_publish_track]: no catalog lookup or
/// container parsing. `on_frame` is called with a positive raw frame ID for each
/// frame in arrival order, then exactly once more with a terminal code: `0`
/// (closed cleanly) or a negative error. After the terminal (`<= 0`) callback,
/// `on_frame` is never called again and `user_data` is never touched again, so
/// release `user_data` there. The terminal callback fires even after
/// [moq_consume_track_close]. Read each frame with [moq_consume_track_frame] and
/// release it with [moq_consume_track_frame_close].
///
/// Returns a non-zero handle to the track on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure that name is a valid pointer to name_len bytes of data.
/// - The caller must keep `user_data` valid until the terminal (`<= 0`) `on_frame` callback.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_consume_track(
	broadcast: u32,
	name: *const c_char,
	name_len: usize,
	on_frame: Option<extern "C" fn(user_data: *mut c_void, frame: i32)>,
	user_data: *mut c_void,
) -> i32 {
	ffi::enter(move || {
		let broadcast = ffi::parse_id(broadcast)?;
		let name = unsafe { ffi::parse_str(name, name_len)? };
		let on_frame = unsafe { ffi::OnStatus::new(user_data, on_frame) };
		State::lock().consume.raw_track(broadcast, name, on_frame)
	})
}

/// Read a raw frame's payload delivered via the [moq_consume_track] callback.
///
/// Fills `dst.payload` / `dst.payload_size`; the pointer is valid until the
/// frame is released with [moq_consume_frame_close]. `dst.timestamp_us` and
/// `dst.keyframe` are reported as 0 / false (not meaningful for raw tracks).
///
/// Returns a zero on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure that `dst` is a valid pointer to a [moq_frame] struct.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_consume_track_frame(frame: u32, dst: *mut moq_frame) -> i32 {
	ffi::enter(move || {
		let frame = ffi::parse_id(frame)?;
		let dst = unsafe { dst.as_mut() }.ok_or(Error::InvalidPointer)?;
		State::lock().consume.raw_frame(frame, dst)
	})
}

/// Close a raw frame and clean up its resources.
///
/// Returns a zero on success, or a negative code on failure.
#[unsafe(no_mangle)]
pub extern "C" fn moq_consume_track_frame_close(frame: u32) -> i32 {
	ffi::enter(move || {
		let frame = ffi::parse_id(frame)?;
		State::lock().consume.raw_frame_close(frame)
	})
}

/// Stop a raw track consumer's background task.
///
/// Returns immediately: zero on success, or a negative code if already closed.
/// Does NOT free `user_data`; the [moq_consume_track] `on_frame` callback still
/// fires once more with a terminal `0` (or a negative error), which is where
/// `user_data` should be released. Frames already delivered via the callback
/// remain valid until released with [moq_consume_track_frame_close].
#[unsafe(no_mangle)]
pub extern "C" fn moq_consume_track_close(track: u32) -> i32 {
	ffi::enter(move || {
		let track = ffi::parse_id(track)?;
		State::lock().consume.raw_track_close(track)
	})
}
