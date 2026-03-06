//! UniFFI bindings for [`moq_lite`].
//!
//! Provides a Kotlin/Swift-compatible API for real-time pub/sub over QUIC,
//! mirroring the semantics of the C API in `api.rs` but using Rust-idiomatic types.
//!
//! After building, generate language bindings with:
//! ```bash
//! uniffi-bindgen-cli generate --library target/debug/libmoq_ffi.dylib --language kotlin --out-dir out/
//! uniffi-bindgen-cli generate --library target/debug/libmoq_ffi.dylib --language swift --out-dir out/
//! ```

use url::Url;

use crate::{Error, State, ffi};

// ---- Error type ----

/// Error returned by all UniFFI-exported functions.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum MoqError {
	#[error("{msg}")]
	Error { msg: String },
}

impl From<Error> for MoqError {
	fn from(err: Error) -> Self {
		MoqError::Error { msg: err.to_string() }
	}
}

// ---- Data structs ----

/// Information about a video rendition in the catalog.
#[derive(uniffi::Record)]
pub struct VideoConfig {
	pub name: String,
	pub codec: String,
	pub description: Option<Vec<u8>>,
	pub coded_width: Option<u32>,
	pub coded_height: Option<u32>,
}

/// Information about an audio rendition in the catalog.
#[derive(uniffi::Record)]
pub struct AudioConfig {
	pub name: String,
	pub codec: String,
	pub description: Option<Vec<u8>>,
	pub sample_rate: u32,
	pub channel_count: u32,
}

/// A decoded media frame.
#[derive(uniffi::Record)]
pub struct FrameData {
	pub payload: Vec<u8>,
	pub timestamp_us: u64,
	pub keyframe: bool,
}

/// A broadcast announced by an origin.
#[derive(uniffi::Record)]
pub struct AnnouncedInfo {
	pub path: String,
	pub active: bool,
}

// ---- Callback interfaces ----

/// Callback invoked when a session's status changes.
#[uniffi::export(callback_interface)]
pub trait SessionCallback: Send {
	fn on_status(&self, code: i32);
}

/// Callback invoked when a new catalog is available on a broadcast.
#[uniffi::export(callback_interface)]
pub trait CatalogCallback: Send {
	fn on_catalog(&self, catalog_id: i32);
}

/// Callback invoked when a new frame is available on a track.
#[uniffi::export(callback_interface)]
pub trait FrameCallback: Send {
	fn on_frame(&self, frame_id: i32);
}

/// Callback invoked when a broadcast is announced by an origin.
#[uniffi::export(callback_interface)]
pub trait AnnounceCallback: Send {
	fn on_announce(&self, announced_id: i32);
}

// ---- Runtime helper ----

/// Enter the Tokio runtime context and run `f`, mapping errors to [`MoqError`].
fn run<T, F: FnOnce() -> Result<T, Error>>(f: F) -> Result<T, MoqError> {
	let handle = ffi::RUNTIME.lock().unwrap();
	let _guard = handle.enter();
	match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
		Ok(ret) => ret.map_err(Into::into),
		Err(_) => Err(MoqError::Error {
			msg: "panic in libmoq".to_string(),
		}),
	}
}

// ---- Logging ----

/// Initialize the library with a log level.
///
/// Should be called before any other functions.
/// The `level` string may be: `"error"`, `"warn"`, `"info"`, `"debug"`, or `"trace"`.
#[uniffi::export]
pub fn moq_log_level(level: String) -> Result<(), MoqError> {
	run(|| {
		use std::str::FromStr;
		use tracing::Level;
		match level.as_str() {
			"" => moq_native::Log::default(),
			s => moq_native::Log::new(Level::from_str(s)?),
		}
		.init();
		Ok(())
	})
}

// ---- Session ----

/// Start establishing a connection to a MoQ server.
///
/// Returns a non-zero handle to the session on success.
/// `origin_publish` and `origin_consume` are origin handles (0 = disabled).
/// The callback receives status 0 on connect and a non-zero code on close/error.
#[uniffi::export]
pub fn moq_session_connect(
	url: String,
	origin_publish: u32,
	origin_consume: u32,
	callback: Box<dyn SessionCallback>,
) -> Result<u32, MoqError> {
	run(|| {
		let url = Url::parse(&url)?;
		let mut state = State::lock();
		let publish = ffi::parse_id_optional(origin_publish)?
			.map(|id| state.origin.get(id))
			.transpose()?
			.map(|origin: &moq_lite::OriginProducer| origin.consume());
		let consume = ffi::parse_id_optional(origin_consume)?
			.map(|id| state.origin.get(id))
			.transpose()?
			.cloned();
		let on_status = ffi::OnStatus::from_fn(move |code| callback.on_status(code));
		state.session.connect(url, publish, consume, on_status).map(u32::from)
	})
}

/// Close a session and free its resources.
#[uniffi::export]
pub fn moq_session_close(session: u32) -> Result<(), MoqError> {
	run(|| {
		let session = ffi::parse_id(session)?;
		State::lock().session.close(session)
	})
}

// ---- Origin ----

/// Create an origin for publishing and/or consuming broadcasts.
///
/// Returns a non-zero handle to the origin.
#[uniffi::export]
pub fn moq_origin_create() -> Result<u32, MoqError> {
	run(|| Ok(u32::from(State::lock().origin.create())))
}

/// Publish a broadcast to an origin under the given path.
#[uniffi::export]
pub fn moq_origin_publish(origin: u32, path: String, broadcast: u32) -> Result<(), MoqError> {
	run(|| {
		let origin = ffi::parse_id(origin)?;
		let broadcast = ffi::parse_id(broadcast)?;
		let mut state = State::lock();
		let broadcast = state.publish.get(broadcast)?.consume();
		state.origin.publish(origin, path.as_str(), broadcast)
	})
}

/// Subscribe to broadcast announcements on an origin.
///
/// The callback receives an `announced_id` for each new announcement.
/// Returns a non-zero handle that can be passed to [`moq_origin_announced_close`].
#[uniffi::export]
pub fn moq_origin_announced(origin: u32, callback: Box<dyn AnnounceCallback>) -> Result<u32, MoqError> {
	run(|| {
		let origin = ffi::parse_id(origin)?;
		let on_announce = ffi::OnStatus::from_fn(move |code| {
			callback.on_announce(code);
		});
		State::lock().origin.announced(origin, on_announce).map(u32::from)
	})
}

/// Query information about a discovered broadcast announcement.
#[uniffi::export]
pub fn moq_origin_announced_info(announced: u32) -> Result<AnnouncedInfo, MoqError> {
	run(|| {
		let announced = ffi::parse_id(announced)?;
		let (path, active) = State::lock().origin.announced_info_owned(announced)?;
		Ok(AnnouncedInfo { path, active })
	})
}

/// Stop receiving announcements for broadcasts published to an origin.
#[uniffi::export]
pub fn moq_origin_announced_close(announced: u32) -> Result<(), MoqError> {
	run(|| {
		let announced = ffi::parse_id(announced)?;
		State::lock().origin.announced_close(announced)
	})
}

/// Consume a broadcast from an origin by path.
///
/// Returns a non-zero handle to the broadcast consumer.
#[uniffi::export]
pub fn moq_origin_consume(origin: u32, path: String) -> Result<u32, MoqError> {
	run(|| {
		let origin = ffi::parse_id(origin)?;
		let mut state = State::lock();
		let broadcast = state.origin.consume(origin, path.as_str())?;
		Ok(u32::from(state.consume.start(broadcast)))
	})
}

/// Close an origin and clean up its resources.
#[uniffi::export]
pub fn moq_origin_close(origin: u32) -> Result<(), MoqError> {
	run(|| {
		let origin = ffi::parse_id(origin)?;
		State::lock().origin.close(origin)
	})
}

// ---- Publish ----

/// Create a new broadcast for publishing media tracks.
///
/// Returns a non-zero handle to the broadcast.
#[uniffi::export]
pub fn moq_publish_create() -> Result<u32, MoqError> {
	run(|| State::lock().publish.create().map(u32::from))
}

/// Close a broadcast and clean up its resources.
#[uniffi::export]
pub fn moq_publish_close(broadcast: u32) -> Result<(), MoqError> {
	run(|| {
		let broadcast = ffi::parse_id(broadcast)?;
		State::lock().publish.close(broadcast)
	})
}

/// Create a new media track for a broadcast.
///
/// `format` controls the encoding of `init` and frame payloads.
/// Returns a non-zero handle to the media track.
#[uniffi::export]
pub fn moq_publish_media_ordered(broadcast: u32, format: String, init: Vec<u8>) -> Result<u32, MoqError> {
	run(|| {
		let broadcast = ffi::parse_id(broadcast)?;
		State::lock()
			.publish
			.media_ordered(broadcast, format.as_str(), &init)
			.map(u32::from)
	})
}

/// Remove a track from a broadcast and clean up its resources.
#[uniffi::export]
pub fn moq_publish_media_close(media: u32) -> Result<(), MoqError> {
	run(|| {
		let media = ffi::parse_id(media)?;
		State::lock().publish.media_close(media)
	})
}

/// Write a frame to a media track.
///
/// `timestamp_us` is the presentation timestamp in microseconds.
#[uniffi::export]
pub fn moq_publish_media_frame(media: u32, payload: Vec<u8>, timestamp_us: u64) -> Result<(), MoqError> {
	run(|| {
		let media = ffi::parse_id(media)?;
		let timestamp = hang::container::Timestamp::from_micros(timestamp_us)?;
		State::lock().publish.media_frame(media, &payload, timestamp)
	})
}

// ---- Consume ----

/// Create a catalog consumer for a broadcast.
///
/// The callback receives a `catalog_id` when a new catalog becomes available.
/// Returns a non-zero handle that can be passed to [`moq_consume_catalog_close`].
#[uniffi::export]
pub fn moq_consume_catalog(broadcast: u32, callback: Box<dyn CatalogCallback>) -> Result<u32, MoqError> {
	run(|| {
		let broadcast = ffi::parse_id(broadcast)?;
		let on_catalog = ffi::OnStatus::from_fn(move |code| {
			callback.on_catalog(code);
		});
		State::lock().consume.catalog(broadcast, on_catalog).map(u32::from)
	})
}

/// Close a catalog consumer and cancel its background task.
#[uniffi::export]
pub fn moq_consume_catalog_close(catalog: u32) -> Result<(), MoqError> {
	run(|| {
		let catalog = ffi::parse_id(catalog)?;
		State::lock().consume.catalog_close(catalog)
	})
}

/// Close a catalog snapshot received via the catalog callback.
#[uniffi::export]
pub fn moq_consume_catalog_snapshot_close(catalog: u32) -> Result<(), MoqError> {
	run(|| {
		let catalog = ffi::parse_id(catalog)?;
		State::lock().consume.catalog_snapshot_close(catalog)
	})
}

/// Query information about a video track in a catalog.
#[uniffi::export]
pub fn moq_consume_video_config(catalog: u32, index: u32) -> Result<VideoConfig, MoqError> {
	run(|| {
		let catalog = ffi::parse_id(catalog)?;
		let data = State::lock().consume.video_config_data(catalog, index as usize)?;
		Ok(VideoConfig {
			name: data.name,
			codec: data.codec,
			description: data.description,
			coded_width: data.coded_width,
			coded_height: data.coded_height,
		})
	})
}

/// Query information about an audio track in a catalog.
#[uniffi::export]
pub fn moq_consume_audio_config(catalog: u32, index: u32) -> Result<AudioConfig, MoqError> {
	run(|| {
		let catalog = ffi::parse_id(catalog)?;
		let data = State::lock().consume.audio_config_data(catalog, index as usize)?;
		Ok(AudioConfig {
			name: data.name,
			codec: data.codec,
			description: data.description,
			sample_rate: data.sample_rate,
			channel_count: data.channel_count,
		})
	})
}

/// Consume a video track from a catalog, delivering frames in decode order.
///
/// `max_latency_ms` controls the maximum buffering before skipping a GoP.
/// The callback receives a `frame_id` when each frame is available.
/// Returns a non-zero handle that can be passed to [`moq_consume_video_close`].
#[uniffi::export]
pub fn moq_consume_video_ordered(
	catalog: u32,
	index: u32,
	max_latency_ms: u64,
	callback: Box<dyn FrameCallback>,
) -> Result<u32, MoqError> {
	run(|| {
		let catalog = ffi::parse_id(catalog)?;
		let max_latency = std::time::Duration::from_millis(max_latency_ms);
		let on_frame = ffi::OnStatus::from_fn(move |code| {
			callback.on_frame(code);
		});
		State::lock()
			.consume
			.video_ordered(catalog, index as usize, max_latency, on_frame)
			.map(u32::from)
	})
}

/// Close a video track consumer and clean up its resources.
#[uniffi::export]
pub fn moq_consume_video_close(track: u32) -> Result<(), MoqError> {
	run(|| {
		let track = ffi::parse_id(track)?;
		State::lock().consume.video_close(track)
	})
}

/// Consume an audio track from a catalog, delivering frames in decode order.
///
/// `max_latency_ms` controls the maximum buffering before skipping frames.
/// The callback receives a `frame_id` when each frame is available.
/// Returns a non-zero handle that can be passed to [`moq_consume_audio_close`].
#[uniffi::export]
pub fn moq_consume_audio_ordered(
	catalog: u32,
	index: u32,
	max_latency_ms: u64,
	callback: Box<dyn FrameCallback>,
) -> Result<u32, MoqError> {
	run(|| {
		let catalog = ffi::parse_id(catalog)?;
		let max_latency = std::time::Duration::from_millis(max_latency_ms);
		let on_frame = ffi::OnStatus::from_fn(move |code| {
			callback.on_frame(code);
		});
		State::lock()
			.consume
			.audio_ordered(catalog, index as usize, max_latency, on_frame)
			.map(u32::from)
	})
}

/// Close an audio track consumer and clean up its resources.
#[uniffi::export]
pub fn moq_consume_audio_close(track: u32) -> Result<(), MoqError> {
	run(|| {
		let track = ffi::parse_id(track)?;
		State::lock().consume.audio_close(track)
	})
}

/// Retrieve the full payload and metadata for a frame.
///
/// Returns a [`FrameData`] with the complete frame payload allocated as a `Vec<u8>`.
/// Call [`moq_consume_frame_close`] when done.
#[uniffi::export]
pub fn moq_consume_frame(frame: u32) -> Result<FrameData, MoqError> {
	run(|| {
		let frame = ffi::parse_id(frame)?;
		let (payload, timestamp_us, keyframe) = State::lock().consume.frame_data(frame)?;
		Ok(FrameData {
			payload,
			timestamp_us,
			keyframe,
		})
	})
}

/// Close a frame and free its resources.
#[uniffi::export]
pub fn moq_consume_frame_close(frame: u32) -> Result<(), MoqError> {
	run(|| {
		let frame = ffi::parse_id(frame)?;
		State::lock().consume.frame_close(frame)
	})
}

/// Close a broadcast consumer and clean up its resources.
#[uniffi::export]
pub fn moq_consume_close(consume: u32) -> Result<(), MoqError> {
	run(|| {
		let consume = ffi::parse_id(consume)?;
		State::lock().consume.close(consume)
	})
}
