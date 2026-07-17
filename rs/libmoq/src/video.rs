//! Native video decode via [`moq_video`].
//!
//! The video counterpart to [`audio`](crate::audio)'s decoder: subscribe to an
//! H.264 track and hand back decoded raw frames, with the decode happening
//! inside the FFI boundary (VideoToolbox on macOS, openh264 elsewhere; no
//! ffmpeg). Sibling to `moq_consume_video`, which delivers the
//! still-encoded frames for a caller that brings its own decoder.
//!
//! Only H.264 is supported. A non-H.264 rendition fails the subscribe with a
//! terminal error on the callback.

use std::ffi::c_void;
use std::time::Duration;

use tokio::sync::oneshot;

use crate::ffi::OnStatus;
use crate::{Error, Id, NonZeroSlab, State, ffi};

// ---- C-visible types ----

/// Decode-side configuration the caller passes to [`moq_consume_video_raw`].
///
/// Output is always tightly-packed I420 (see [`moq_video_frame`]); there is no
/// format/resolution knob yet. The struct exists so future options (a pixel
/// format, a target size) stay additive.
#[repr(C)]
#[allow(non_camel_case_types)]
pub struct moq_video_decoder_output {
	/// Upper bound on buffering before skipping a stalled group, in
	/// milliseconds. Same congestion-control knob as
	/// `moq_consume_video`'s `max_latency_ms`. 0 = skip aggressively
	/// (the moq-mux default); set to your playout buffer for a softer skip.
	pub latency_max_ms: u64,
}

/// One decoded video frame: packed I420 plus a presentation timestamp.
///
/// `data` is `width * height * 3 / 2` bytes: the Y plane (`width * height`),
/// then U, then V (`width/2 * height/2` each), no row padding. It's BT.601
/// limited range. `width` and `height` are even. `data` is owned by the consume
/// slab and stays valid until the same id is released with
/// [`moq_consume_video_raw_frame_free`].
#[repr(C)]
#[allow(non_camel_case_types)]
pub struct moq_video_frame {
	pub timestamp_us: u64,
	pub width: u32,
	pub height: u32,
	pub data: *const u8,
	pub data_size: usize,
}

// ---- State extension (used internally by lib.rs) ----

/// Raw-video consume state: decoder tasks plus their buffered decoded frames.
#[derive(Default)]
pub struct Video {
	consumer_tasks: NonZeroSlab<Option<VideoTaskEntry>>,
	frames: NonZeroSlab<VideoFrame>,
}

/// A delivered frame, flattened to CPU I420 at delivery time: the C ABI hands
/// out a stable byte pointer, so a GPU-decoded frame (e.g. NVDEC) is downloaded
/// exactly once here.
struct VideoFrame {
	timestamp_us: u64,
	width: u32,
	height: u32,
	data: bytes::Bytes,
}

/// A spawned task entry: `close` signals shutdown, `callback` delivers status.
///
/// Same lifetime contract as the audio decoder: the task delivers one final
/// terminal callback and then removes itself, so `user_data` stays valid until
/// that callback fires. `close` is an `Option` so `consume_close` can drop just
/// the sender without removing the entry.
struct VideoTaskEntry {
	close: Option<oneshot::Sender<()>>,
	callback: OnStatus,
}

impl Video {
	pub fn consume(
		&mut self,
		broadcast: &moq_net::broadcast::Consumer,
		catalog: &hang::catalog::VideoConfig,
		name: &str,
		config: moq_video::decode::Config,
		on_frame: OnStatus,
	) -> Result<Id, Error> {
		let broadcast = broadcast.clone();
		let catalog = catalog.clone();
		let name = name.to_string();

		let channel = oneshot::channel();
		let entry = VideoTaskEntry {
			close: Some(channel.0),
			callback: on_frame,
		};
		let id = self.consumer_tasks.insert(Some(entry))?;

		// `Consumer::new` subscribes (blocking on SUBSCRIBE_OK), so run it inside
		// the task to keep this entrypoint non-blocking.
		tokio::spawn(async move {
			let res = async move {
				let consumer = moq_video::decode::Consumer::new(&broadcast, &catalog, name, config).await?;
				Self::run(on_frame, consumer, channel.1).await
			}
			.await;

			// Deliver one final terminal callback (code <= 0), then drop the entry.
			// Pull it out from under the lock so the callback never runs while held.
			let entry = State::lock().video.consumer_tasks.remove(id).flatten();
			if let Some(entry) = entry {
				entry.callback.call(res);
			}
		});

		Ok(id)
	}

	async fn run(
		callback: OnStatus,
		mut consumer: moq_video::decode::Consumer,
		mut close: oneshot::Receiver<()>,
	) -> Result<(), Error> {
		loop {
			// `biased` so a pending close always wins over a ready frame.
			let frame = tokio::select! {
				biased;
				_ = &mut close => return Ok(()),
				frame = consumer.read() => match frame? {
					Some(frame) => frame,
					None => return Ok(()),
				},
			};

			// Flatten to CPU bytes outside the lock (a GPU frame downloads here),
			// then hold the lock only to buffer it; release before the callback.
			let frame = VideoFrame {
				// The C ABI carries microseconds; the decoded frame's Timestamp is
				// constrained to a QUIC VarInt, so the microsecond value fits a u64.
				timestamp_us: frame.timestamp.as_micros() as u64,
				width: frame.size.width,
				height: frame.size.height,
				data: frame.into_i420()?,
			};
			let frame_id = State::lock().video.frames.insert(frame)?;
			callback.call(Ok(frame_id));
		}
	}

	pub fn consume_close(&mut self, id: Id) -> Result<(), Error> {
		// Signal shutdown; the task delivers a final callback and removes itself.
		self.consumer_tasks
			.get_mut(id)
			.and_then(|entry| entry.as_mut())
			.ok_or(Error::TrackNotFound)?
			.close
			.take()
			.ok_or(Error::TrackNotFound)?;
		Ok(())
	}

	pub fn frame_info(&self, id: Id, dst: &mut moq_video_frame) -> Result<(), Error> {
		let frame = self.frames.get(id).ok_or(Error::FrameNotFound)?;
		*dst = moq_video_frame {
			timestamp_us: frame.timestamp_us,
			width: frame.width,
			height: frame.height,
			data: frame.data.as_ptr(),
			data_size: frame.data.len(),
		};
		Ok(())
	}

	pub fn frame_free(&mut self, id: Id) -> Result<(), Error> {
		self.frames.remove(id).ok_or(Error::FrameNotFound)?;
		Ok(())
	}
}

// ---- C entry points ----

/// Subscribe to a video track and decode it into raw I420 frames.
///
/// The catalog `index` selects which video rendition to subscribe to, matching
/// the existing `moq_consume_video` selection model. Only H.264 is
/// supported; a non-H.264 rendition fails on the terminal callback.
///
/// Returns a non-zero handle on success or a negative error code.
///
/// `on_frame` is called with a positive frame id per decoded frame, then exactly
/// once more with a terminal code: `0` (closed cleanly) or a negative error.
/// After the terminal (`<= 0`) callback, `on_frame` is never called again and
/// `user_data` is never touched again, so release `user_data` there. The terminal
/// callback fires even after [`moq_consume_video_raw_close`].
///
/// # Safety
/// - `output` must point to a valid [`moq_video_decoder_output`].
/// - `user_data` must stay valid until the terminal (`<= 0`) `on_frame` callback.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_consume_video_raw(
	catalog: u32,
	index: u32,
	output: *const moq_video_decoder_output,
	on_frame: Option<extern "C" fn(user_data: *mut c_void, frame: i32)>,
	user_data: *mut c_void,
) -> i32 {
	ffi::enter(move || {
		let catalog = ffi::parse_id(catalog)?;
		let raw = unsafe { output.as_ref() }.ok_or(Error::InvalidPointer)?;

		let mut config = moq_video::decode::Config::new();
		config.latency_max = if raw.latency_max_ms == 0 {
			None
		} else {
			Some(Duration::from_millis(raw.latency_max_ms))
		};
		let on_frame = unsafe { OnStatus::new(user_data, on_frame) };

		let mut state = State::lock();
		let (broadcast, video_cfg, name) = state.consume.video_rendition(catalog, index as usize)?;

		let State { video, .. } = &mut *state;
		video.consume(&broadcast, &video_cfg, &name, config, on_frame)
	})
}

/// Stop a video (raw) consumer's background task.
///
/// Returns immediately: zero on success, or a negative code if already closed.
/// Does NOT free `user_data`; the on-frame callback still fires once more with a
/// terminal `0` (or a negative error), which is where `user_data` should be
/// released. Frame ids already delivered are likewise not freed; release each
/// with [`moq_consume_video_raw_frame_free`].
#[unsafe(no_mangle)]
pub extern "C" fn moq_consume_video_raw_close(consumer: u32) -> i32 {
	ffi::enter(move || {
		let consumer = ffi::parse_id(consumer)?;
		State::lock().video.consume_close(consumer)
	})
}

/// Copy a delivered frame's metadata into `dst`.
///
/// The written `dst->data` pointer remains valid until the same `id` is released
/// with [`moq_consume_video_raw_frame_free`].
///
/// # Safety
/// - `dst` must point to a writable [`moq_video_frame`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_consume_video_raw_frame(id: u32, dst: *mut moq_video_frame) -> i32 {
	ffi::enter(move || {
		let id = ffi::parse_id(id)?;
		let dst = unsafe { dst.as_mut() }.ok_or(Error::InvalidPointer)?;
		State::lock().video.frame_info(id, dst)
	})
}

/// Free a frame previously delivered through the consume callback. Required for
/// every delivered frame id; closing the parent consumer is not enough.
#[unsafe(no_mangle)]
pub extern "C" fn moq_consume_video_raw_frame_free(id: u32) -> i32 {
	ffi::enter(move || {
		let id = ffi::parse_id(id)?;
		State::lock().video.frame_free(id)
	})
}
