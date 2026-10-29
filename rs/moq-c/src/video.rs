//! Native video encode/decode via [`moq_video`].
//!
//! The video counterpart to [`audio`](crate::audio): publish raw pictures as an
//! encoded video track, and subscribe to one and hand back decoded raw frames,
//! with the codec running inside the FFI boundary (VideoToolbox on macOS, Media
//! Foundation on Windows, NVENC/NVDEC on Linux, openh264 as the software
//! fallback; no ffmpeg). Siblings to `moq_publish_media_*` /
//! `moq_consume_video`, which carry already-encoded frames for a caller that
//! brings its own codec.
//!
//! Decode is H.264 only; a non-H.264 rendition fails the subscribe with a
//! terminal error on the callback. Encode covers H.264 and H.265 (see
//! [`moq_video_codec`]).

use std::ffi::{c_char, c_void};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use tokio::sync::oneshot;

use crate::decode::Read;
use crate::ffi::OnStatus;
use crate::task::Task;
use crate::{Error, Id, NonZeroSlab, Shared, State, ffi, moq_decode_gap, moq_decode_read_kind};

// ---- C-visible types ----

/// Pixel layout of the raw frames handed to [`moq_encode_video_frame`].
///
/// The enum is exposed in the C header for readability, but ABI fields that
/// carry it are typed `u32`. A C caller passing an unknown discriminant gets
/// `Error::InvalidCode` instead of UB.
#[repr(C)]
#[expect(non_camel_case_types, reason = "C ABI type names match moq.h")]
#[derive(Clone, Copy, Debug)]
pub enum moq_video_pixel_format {
	/// Tightly-packed planar I420: Y, then U, then V, no row padding.
	/// `width * height * 3 / 2` bytes, the same layout [`moq_decode_video`]
	/// hands back.
	MOQ_VIDEO_PIXEL_FORMAT_I420 = 0,
	/// Tightly-packed RGBA, `width * height * 4` bytes, no row padding.
	MOQ_VIDEO_PIXEL_FORMAT_RGBA = 1,
}

/// Output video codec for [`moq_encode_video`].
///
/// Not every codec has a backend on every machine: H.265 is hardware-only, so
/// publishing it fails where no hardware encoder is available.
#[repr(C)]
#[expect(non_camel_case_types, reason = "C ABI type names match moq.h")]
#[derive(Clone, Copy, Debug)]
pub enum moq_video_codec {
	/// H.264 / AVC, published as an `avc3` track.
	MOQ_VIDEO_CODEC_H264 = 0,
	/// H.265 / HEVC, published as a `hev1` track.
	MOQ_VIDEO_CODEC_H265 = 1,
}

/// Which encoder implementation [`moq_encode_video`] should use.
#[repr(C)]
#[expect(non_camel_case_types, reason = "C ABI type names match moq.h")]
#[derive(Clone, Copy, Debug)]
pub enum moq_video_encoder_kind {
	/// Prefer a platform hardware encoder, falling back to software.
	MOQ_VIDEO_ENCODER_KIND_AUTO = 0,
	/// Hardware only; fails if none is available.
	MOQ_VIDEO_ENCODER_KIND_HARDWARE = 1,
	/// Software only (openh264, H.264 only).
	MOQ_VIDEO_ENCODER_KIND_SOFTWARE = 2,
	/// A specific backend, named by `moq_video_encoder_output::encoder`.
	MOQ_VIDEO_ENCODER_KIND_NAMED = 3,
}

/// An exact video frame rate in frames per second.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct moq_video_frame_rate {
	/// Frames per second numerator; must be nonzero.
	pub numerator: u32,
	/// Frames per second denominator; must be nonzero.
	pub denominator: u32,
}

/// Raw frame layout the caller hands to [`moq_encode_video_frame`], plus
/// the resolution and rate the encoder is opened at. Every published frame must
/// match `width` x `height`; scale before publishing if your source moves.
#[repr(C)]
pub struct moq_video_encoder_input {
	/// `moq_video_pixel_format` discriminant.
	pub format: u32,
	/// Encoded width in pixels. Must be even (I420 chroma is subsampled 2x2).
	pub width: u32,
	/// Encoded height in pixels. Must be even.
	pub height: u32,
	/// Nominal frames per second, used for the codec time base and the default
	/// bitrate and keyframe interval. Must be non-zero.
	pub framerate: moq_video_frame_rate,
}

/// Codec-side configuration for [`moq_encode_video`]. Every knob spells
/// "unset" as 0.
#[repr(C)]
pub struct moq_video_encoder_output {
	/// `moq_video_codec` discriminant.
	pub codec: u32,
	/// Target bitrate in bits per second. 0 derives one from the resolution and
	/// framerate.
	pub bitrate: u64,
	/// Keyframe interval in frames: a subscriber joining mid-stream waits at
	/// most this many frames before it can decode. 0 uses ~2 seconds.
	pub gop: u32,
	/// `moq_video_encoder_kind` discriminant.
	pub kind: u32,
	/// Backend name, UTF-8, e.g. `"videotoolbox"`, `"nvenc"`, `"mediafoundation"`,
	/// `"openh264"`. Read only when `kind` is `MOQ_VIDEO_ENCODER_KIND_NAMED`.
	pub encoder: *const c_char,
	/// The length in bytes of `encoder`.
	pub encoder_len: usize,
}

/// Rate control for [`moq_encode_video`].
///
/// NULL or zeroed holds the configured bitrate regardless of congestion. New fields
/// always append, so a zeroed struct keeps meaning the defaults.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct moq_video_encode_options {
	/// A handle from [`crate::moq_session_allocator`] whose grant the encoder follows,
	/// or 0 to hold the configured bitrate.
	pub bandwidth: u32,
	/// The [`moq_encode_video`] handle of the next lower rendition of the same source,
	/// or 0. It makes this encoder a simulcast rung: while its grant cannot fund it, it
	/// stops encoding (frames published to it are dropped) and advertises itself
	/// disabled, so the lower renditions keep their share, then resumes on a keyframe
	/// once the grant recovers. It takes the track priority one below `lower`'s, so the
	/// allocator fills that one first. It needs a `bandwidth`, and a bitrate above
	/// `lower`'s.
	pub lower: u32,
}

/// One raw frame handed to [`moq_encode_video_frame`].
///
/// Pixel format and resolution are fixed by [`moq_video_encoder_input`] at
/// publish time, so a frame carries neither: `data` is exactly one picture in
/// that layout, borrowed for the duration of the call (the encoder copies before
/// returning). The decode side has its own [`moq_video_frame`], which does carry
/// dimensions, since there they are what the stream turned out to be.
#[repr(C)]
pub struct moq_video_encoder_frame {
	/// Presentation timestamp, in microseconds.
	pub timestamp_us: u64,
	/// The raw frame bytes in the configured layout.
	pub data: *const u8,
	/// The length in bytes of `data`.
	pub data_size: usize,
}

/// Decode-side configuration the caller passes to [`moq_decode_video`].
///
/// `format` selects the CPU pixel layout of each [`moq_video_frame`] (`I420`
/// is `width * height * 3 / 2` bytes, `RGBA` is `width * height * 4` bytes),
/// and `width`/`height` select its size: zero both for the stream's native
/// size, otherwise both must be even and non-zero.
///
/// This struct is versioned by recompilation, not by reserved fields: adding a
/// field changes its layout, so rebuild callers against the `moq.h` that ships
/// with the `libmoq.a` they link.
#[repr(C)]
pub struct moq_video_decoder_output {
	/// Upper bound on buffering before skipping a stalled group, in
	/// microseconds. Same congestion-control knob as
	/// `moq_consume_video`'s `max_delay_us`. 0 = skip aggressively
	/// (the moq-mux default); set to your playout buffer for a softer skip.
	pub max_delay_us: u64,
	/// `moq_video_pixel_format` discriminant. Unknown values fail
	/// [`moq_decode_video`] rather than decoding into an assumed layout.
	pub format: u32,
	/// Target width in pixels. 0 with `height` 0 means the native size.
	pub width: u32,
	/// Target height in pixels. 0 with `width` 0 means the native size.
	pub height: u32,
}

/// One decoded video frame from [`moq_decode_video`]: pixels plus a
/// presentation timestamp.
///
/// The pixel layout is what [`moq_video_decoder_output`]'s `format` asked
/// for: I420 is the Y plane (`width * height`), then U, then V (`width/2 *
/// height/2` each), no row padding, BT.601 limited range; RGBA is tightly
/// packed `width * height * 4` bytes, no row padding.
///
/// The read id owns the decoded picture, and the pixels are produced from it
/// on the first [`moq_decode_video_read`] call for that id. `data` stays valid
/// until the same id is released with [`moq_decode_video_read_free`].
///
/// The publish side has its own [`moq_video_encoder_frame`], which carries no
/// dimensions because the encoder already fixed them.
#[repr(C)]
pub struct moq_video_frame {
	/// The presentation timestamp in microseconds.
	pub timestamp_us: u64,
	/// The decoded width in pixels.
	pub width: u32,
	/// The decoded height in pixels.
	pub height: u32,
	/// The raw frame bytes in the configured layout.
	pub data: *const u8,
	/// The length in bytes of `data`.
	pub data_size: usize,
}

/// One read from [`moq_decode_video`]: a decoded picture, or a break between them.
/// `kind` says which field is filled; the others are zeroed.
#[repr(C)]
pub struct moq_video_read {
	/// Which kind of read this is.
	pub kind: moq_decode_read_kind,
	/// The decoded picture, for [moq_decode_read_kind::MOQ_DECODE_READ_FRAME].
	pub frame: moq_video_frame,
	/// The lost span, for [moq_decode_read_kind::MOQ_DECODE_READ_GAP]. Every picture from
	/// before the loss precedes it, and what follows does not continue what came before.
	pub gap: moq_decode_gap,
	/// Where the stopped encoder's media stops in microseconds, for
	/// [moq_decode_read_kind::MOQ_DECODE_READ_END]. The stopped encoder's last pictures
	/// precede it; what follows comes from a fresh encoder.
	pub end_us: u64,
}

// ---- State extension (used internally by lib.rs) ----

/// Raw-video state: encoders being published, plus decoder tasks and the
/// reads they delivered.
#[derive(Default)]
pub struct Video {
	producers: NonZeroSlab<Shared<VideoEncoder>>,
	consumer_tasks: NonZeroSlab<Task>,
	reads: NonZeroSlab<Read<Arc<VideoFrame>>>,
}

/// Wait out an encode-thread round trip from a C entry point.
///
/// The C ABI hands back a status code, so there is no executor to yield to and
/// this is where [`Sink`](moq_video::encode::Sink)'s futures stop. Blocking is
/// also what paces the caller: a raw frame is megabytes, so a publish free to run
/// ahead of the codec would queue pictures without bound.
///
/// `pollster` rather than a tokio helper because those panic when the calling
/// thread is driving a runtime, which the one dispatching a callback is.
fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
	pollster::block_on(future)
}

fn video_ceiling(
	config: &moq_video::encode::Config,
	rendition: &hang::catalog::VideoConfig,
) -> moq_net::bandwidth::Rate {
	config
		.bitrate
		.or_else(|| rendition.bitrate.map(moq_net::bandwidth::Rate::from_bps))
		.unwrap_or_else(|| {
			moq_net::bandwidth::Rate::from_bps(
				(config.size().pixels() as f64 * config.framerate.as_f64() * 0.07) as u64,
			)
		})
}

/// The policy a rendition follows: its manual `ceiling`, above the `lower` rendition's
/// current ceiling, if any.
fn video_policy(ceiling: moq_net::bandwidth::Rate, lower: Option<moq_net::bandwidth::Rate>) -> moq_mux::rate::Policy {
	let mut policy = moq_mux::rate::Policy::new(ceiling);
	policy.lower = lower;
	policy
}

/// The lower rendition's current ceiling, or `None` once it finished, which leaves
/// this one nothing to fall back to, so it never disables. Arms `waiter` for either
/// change.
fn lower_ceiling(
	lower: &Option<kio::Consumer<moq_net::bandwidth::Rate>>,
	waiter: &kio::Waiter,
) -> Option<moq_net::bandwidth::Rate> {
	let lower = lower.as_ref()?;
	match lower.poll(waiter, |_| std::task::Poll::<()>::Pending) {
		std::task::Poll::Pending => Some(*lower.read()),
		// The closure never completes, so this is the channel closing.
		std::task::Poll::Ready(_) => None,
	}
}

/// Feed each grant through the same policy moq-video uses, and retune the live
/// encoder, disabling and resuming a rung. Retires on
/// [`BitrateUnsupported`](moq_video::Error::BitrateUnsupported).
async fn follow_reservation(
	inner: Shared<VideoEncoder>,
	lower: Option<kio::Consumer<moq_net::bandwidth::Rate>>,
	consumer: moq_net::bandwidth::Consumer,
	ceiling: moq_net::bandwidth::Rate,
) {
	let noop = kio::Waiter::noop();
	let mut follower = moq_mux::rate::Follower::new(consumer, video_policy(ceiling, lower_ceiling(&lower, &noop)));
	loop {
		// Both ceilings are read on every wake, before the follower filters a grant against
		// them. This one's `moq_encode_video_bitrate` moves its reservation after moving its
		// ceiling, which wakes this poll; the lower one's ceiling wakes it directly, wherever
		// that rendition reserves. The lower one is read first, so no two encoder locks are
		// ever held at once.
		let next = kio::wait(|waiter| {
			let below = lower_ceiling(&lower, waiter);
			let guard = inner.lock();
			let Some(producer) = guard.as_ref() else {
				return std::task::Poll::Ready(None);
			};
			follower.set_policy(video_policy(*producer.ceiling.read(), below));
			follower.poll_next(waiter).map(Some)
		})
		.await;
		let Some(Ok(target)) = next else {
			return;
		};
		let below = lower_ceiling(&lower, &noop);
		let mut guard = inner.lock();
		let Some(producer) = guard.as_mut() else {
			return;
		};
		let policy = video_policy(*producer.ceiling.read(), below);
		if retune(&mut follower, policy, target, |target| producer.apply(target)).is_err() {
			return;
		}
	}
}

/// Apply the follower's next `target` through `apply`, unless the `policy` moved since it
/// was asked for: then follow under the new policy, whose manual ceiling
/// `moq_encode_video_bitrate` already applied, instead. Returns the target applied, or
/// `Err` with the name of an encoder that cannot follow at all.
fn retune(
	follower: &mut moq_mux::rate::Follower,
	policy: moq_mux::rate::Policy,
	target: moq_mux::rate::Target,
	apply: impl FnOnce(moq_mux::rate::Target) -> Result<(), moq_video::Error>,
) -> Result<Option<moq_mux::rate::Target>, &'static str> {
	if *follower.policy() != policy {
		// The target was never applied, so take it back first: a policy that keeps the
		// follower's state (a moved lower rendition) would otherwise count it applied.
		follower.failed();
		follower.set_policy(policy);
		return Ok(None);
	}
	match apply(target) {
		Ok(()) => {
			tracing::debug!(?target, "adjusted encoder bitrate");
			Ok(Some(target))
		}
		Err(moq_video::Error::BitrateUnsupported(name)) => {
			tracing::warn!(encoder = name, "encoder cannot follow the bandwidth estimate");
			Err(name)
		}
		// The follower takes the target back and retries it, so a quiet grant still
		// recovers.
		Err(err) => {
			tracing::warn!(error = %err, ?target, "failed to adjust encoder bitrate");
			follower.failed();
			Ok(None)
		}
	}
}

/// An encoder paired with the track publishing its output, plus the pixel format
/// its caller feeds it (fixed at publish time, so a frame carries only pixels and
/// a timestamp).
///
/// The encoder is a [`Sink`](moq_video::encode::Sink) rather than a bare
/// `Encoder` because a C caller drives a handle from whichever thread it likes,
/// so a bare `Encoder` would be built on one thread and dropped on another,
/// unbalancing the per-thread COM apartment the Windows backend opens. The sink
/// owns the thread instead, so every caller is welcome.
pub(crate) struct VideoEncoder {
	/// `None` while the rendition is disabled for lack of bandwidth.
	encoder: Option<moq_video::encode::Sink>,
	producer: moq_video::encode::Producer,
	/// The encoder settings, to reopen at [`ceiling`](Self::ceiling) when a disabled
	/// rendition resumes.
	config: moq_video::encode::Config,
	/// The track priority this rendition was published at; a higher rung takes one below.
	priority: i8,
	/// The next lower rendition's ceiling, set when this one is a simulcast rung. A
	/// consumer, so a rung never keeps the rendition below it publishing.
	lower: Option<kio::Consumer<moq_net::bandwidth::Rate>>,
	format: moq_video_pixel_format,
	/// The encoded resolution, from the publish config. Frames carry only pixels,
	/// so this is what says how to read them.
	size: moq_video::Size,
	/// The allocator [`follow`](VideoPublish::follow) reserved against, kept to reserve again
	/// when a decoder identity change moves the rendition to a new track.
	allocator: Option<moq_net::bandwidth::Allocator>,
	reservation: Option<Arc<moq_net::bandwidth::Reservation>>,
	follow: Option<oneshot::Sender<()>>,
	followed: Option<crate::ffi::Background>,
	/// The manual ceiling [`moq_encode_video_bitrate`] last applied, which caps every
	/// grant. Behind the encoder's lock, so the follower never reads an old, higher
	/// one and retunes over a newer cap. A channel, so a rung above this rendition
	/// wakes when it moves and when this rendition ends.
	pub(crate) ceiling: kio::Producer<moq_net::bandwidth::Rate>,
	children: crate::publish::Children,
}

impl crate::publish::Abort for Shared<VideoEncoder> {
	fn abort(self, err: moq_net::Error) {
		// Dropping the follow sender stops the rate follower; the encoder closes with it.
		if let Some(encoder) = self.take() {
			encoder.producer.abort(err);
		}
	}
}

/// A delivered frame: the decoded picture, retained until its id is freed, and
/// the packed pixels [`moq_decode_video_read`] produces from it on first use.
///
/// Converting on first use rather than at delivery keeps the work off the
/// delivery task and skips it for a frame the caller frees unread. The pixels
/// are kept once produced, since the C ABI hands out a pointer that must stay
/// put until the id is freed.
struct VideoFrame {
	frame: moq_video::Frame,
	output: DecoderOutput,
	pixels: OnceLock<Pixels>,
}

/// A frame's pixels in the layout and size [`moq_decode_video`] asked for.
struct Pixels {
	width: u32,
	height: u32,
	data: Vec<u8>,
}

impl VideoFrame {
	fn pixels(&self) -> Result<&Pixels, Error> {
		if let Some(pixels) = self.pixels.get() {
			return Ok(pixels);
		}

		// The decoder's scale hint is best effort: a backend without a scaler
		// ignores it, so enforce the requested size here rather than trusting it.
		let resized;
		let frame = match self.output.size {
			Some(size) if self.frame.size() != size => {
				resized = self.frame.resize(size, &moq_video::resize::Config::default())?;
				&resized
			}
			_ => &self.frame,
		};
		let size = frame.size();
		let data = match self.output.format {
			moq_video_pixel_format::MOQ_VIDEO_PIXEL_FORMAT_I420 => frame.surface.to_i420()?.into_owned().into_data(),
			moq_video_pixel_format::MOQ_VIDEO_PIXEL_FORMAT_RGBA => frame
				.surface
				.to_rgba(&moq_video::convert::Config::default())?
				.into_data(),
		};

		// Two threads racing on one id both convert; the first result is kept.
		Ok(self.pixels.get_or_init(|| Pixels {
			width: size.width,
			height: size.height,
			data,
		}))
	}
}

/// What [`moq_decode_video`] delivers per frame: the requested CPU pixel format
/// and target size, validated up front so the delivery loop never second-guesses.
#[derive(Clone, Copy)]
pub struct DecoderOutput {
	format: moq_video_pixel_format,
	size: Option<moq_video::Size>,
}

/// End a video track, given the result of draining its encoder into it.
///
/// A clean finish is a promise that the track holds everything the publisher
/// produced, so a lost tail has to end the track as an abort instead. Finishing
/// anyway would leave a truncated stream indistinguishable from a complete one,
/// and only the local caller would ever learn otherwise.
fn finalize(
	mut producer: moq_video::encode::Producer,
	drained: Result<(), moq_video::Error>,
	children: &crate::publish::Children,
	child: crate::publish::Child,
) -> Result<(), Error> {
	match drained {
		Ok(()) => {
			producer.finish()?;
			// The broadcast keeps the finished track, so its finish still holds it.
			children.keep(child, producer);
			Ok(())
		}
		Err(err) => {
			children.forget(child);
			producer.abort(moq_net::Error::Transport(err.to_string()));
			Err(err.into())
		}
	}
}

/// Reserve the encoder's ceiling against its current track and follow the grant, replacing any
/// earlier claim and its follower: a decoder identity change moves the rendition to a new track,
/// and the claim has to watch that track's demand.
fn follow_grant(shared: &Shared<VideoEncoder>) -> Result<(), Error> {
	let (reservation, ceiling, lower) = {
		let mut guard = shared.lock();
		let encoder = guard.as_mut().ok_or(Error::MediaNotFound)?;
		let Some(allocator) = &encoder.allocator else {
			return Ok(());
		};
		let ceiling = *encoder.ceiling.read();
		let reservation = Arc::new(allocator.reserve(&encoder.producer.demand(), ceiling));
		encoder.reservation = Some(reservation.clone());
		// Dropping the sender stops the previous follower.
		encoder.follow = None;
		(reservation, ceiling, encoder.lower.clone())
	};
	let (close, closed) = oneshot::channel();
	shared.lock().as_mut().ok_or(Error::MediaNotFound)?.follow = Some(close);
	let follower = shared.clone();
	let background = crate::ffi::Background::new(async move {
		tokio::select! {
			biased;
			_ = closed => {}
			_ = follow_reservation(follower, lower, reservation.consume(), ceiling) => {}
		}
		Ok(())
	});
	shared.lock().as_mut().ok_or(Error::MediaNotFound)?.followed = Some(background);
	Ok(())
}

impl VideoEncoder {
	/// Encode and publish one picture, returning whether it moved the rendition to a new track.
	fn publish_frame(&mut self, timestamp_us: u64, data: &[u8]) -> Result<bool, Error> {
		if let Some(followed) = &self.followed {
			followed.check()?;
		}
		// A buffer that isn't one picture at the configured size is rejected here,
		// by the surface constructors, rather than reinterpreted.
		let size = self.size;
		let surface = match self.format {
			moq_video_pixel_format::MOQ_VIDEO_PIXEL_FORMAT_I420 => {
				moq_video::Surface::I420(moq_video::I420::new(size, data.to_vec())?)
			}
			moq_video_pixel_format::MOQ_VIDEO_PIXEL_FORMAT_RGBA => moq_video::Surface::rgba(data, size)?,
		};

		// Disabled: nothing is encoded until the grant resumes the rendition.
		let Some(encoder) = self.encoder.as_mut() else {
			return Ok(false);
		};
		let frame = moq_video::Frame::new(surface, moq_net::Timestamp::from_micros(timestamp_us)?);
		// A backend that pipelines hands back an earlier frame's output, so this is
		// zero or more access units rather than one per call.
		let encoded = block_on(encoder.encode(frame))?;
		let before = self.producer.demand();
		self.producer.publish(&encoded)?;
		// A frame whose decoder identity changed, such as a resume at a ceiling past the declared
		// level, moved the rendition to a newly named track.
		Ok(self.producer.demand().name() != before.name())
	}

	fn publish_cut(&mut self) -> Result<(), Error> {
		if let Some(followed) = &self.followed {
			followed.check()?;
		}
		// A keyframe is what a cut is on the wire: the importer closes the open
		// group and starts a new one at it. A disabled rung resumes on a keyframe anyway.
		match self.encoder.as_mut() {
			Some(encoder) => Ok(block_on(encoder.cut())?),
			None => Ok(()),
		}
	}

	fn publish_bitrate(&mut self, bitrate: u64) -> Result<(), Error> {
		if let Some(followed) = &self.followed {
			followed.check()?;
		}
		let bitrate = moq_net::bandwidth::Rate::from_bps(bitrate);
		// A disabled rung has no encoder to retune; it reopens at the new ceiling.
		if let Some(encoder) = self.encoder.as_mut() {
			block_on(encoder.set_bitrate(bitrate))?;
		}
		let Ok(mut ceiling) = self.ceiling.write() else {
			unreachable!("only dropping the encoder closes its ceiling")
		};
		*ceiling = bitrate;
		drop(ceiling);
		if let Some(reservation) = &self.reservation {
			reservation.update(bitrate);
		}
		Ok(())
	}

	/// Apply a rate-control target, leaving the rendition as it was on an error.
	///
	/// A disable drains the encoder into the track and closes it; a resume reopens one
	/// at the ceiling, whose first frame is a keyframe on every backend, and retunes it
	/// to the target. An encoder that cannot retune still resumes, at its ceiling, and
	/// reports [`BitrateUnsupported`](moq_video::Error::BitrateUnsupported) so rate
	/// control retires.
	fn apply(&mut self, target: moq_mux::rate::Target) -> Result<(), moq_video::Error> {
		let bitrate = match target {
			moq_mux::rate::Target::Enabled(bitrate) => bitrate,
			moq_mux::rate::Target::Disabled => {
				if let Some(encoder) = self.encoder.take() {
					// The encoder is gone either way, so a lost tail only costs those frames.
					let drained = block_on(encoder.finish()).and_then(|encoded| self.producer.publish(&encoded));
					if let Err(err) = drained {
						tracing::warn!(error = %err, "failed to drain a disabled rendition's encoder");
					}
					self.producer.discontinuity()?;
				}
				return self.producer.set_enabled(false);
			}
		};
		if let Some(encoder) = self.encoder.as_mut() {
			return block_on(encoder.set_bitrate(bitrate));
		}

		let ceiling = *self.ceiling.read();
		let mut config = self.config.clone();
		config.bitrate = Some(ceiling);
		let mut encoder = block_on(moq_video::encode::Sink::open(&config))?;
		let retuned = match bitrate < ceiling {
			true => block_on(encoder.set_bitrate(bitrate)),
			false => Ok(()),
		};
		if let Err(err) = &retuned
			&& !matches!(err, moq_video::Error::BitrateUnsupported(_))
		{
			return retuned;
		}
		self.producer.set_enabled(true)?;
		self.encoder = Some(encoder);
		retuned
	}

	/// Drain and finish the encoder behind handle `id`.
	fn publish_finish(self, id: Id) -> Result<(), Error> {
		let VideoEncoder {
			encoder,
			mut producer,
			followed,
			children,
			ceiling,
			..
		} = self;
		// Ended before the drain, so a rung above stops counting on this rendition now.
		drop(ceiling);
		// Drain the codec into the track before ending it, so the last frames land
		// in it rather than being dropped with the encoder.
		let drained = match encoder {
			Some(encoder) => block_on(encoder.finish()).and_then(|encoded| producer.publish(&encoded)),
			None => Ok(()),
		};
		finalize(producer, drained, &children, crate::publish::Child::Video(id))?;
		// A follower panic only stops rate adaptation, so the track still ends
		// cleanly before the panic is reported.
		followed.map_or(Ok(()), |followed| followed.check())
	}
}

impl Video {
	/// Advertise a track for an already-opened encoder.
	///
	/// The encoder and the rendition it will emit are both resolved by the caller,
	/// and before this, so a config this machine can't encode fails without leaving
	/// a track advertised that will never carry frames.
	///
	/// A `lower` encoder makes this one a simulcast rung above it: it takes the track
	/// priority one below, and disables when its bandwidth grant can no longer fund it.
	pub fn publish(
		&mut self,
		target: crate::publish::Target<'_>,
		format: moq_video_pixel_format,
		config: &moq_video::encode::Config,
		rendition: hang::catalog::VideoConfig,
		encoder: moq_video::encode::Sink,
		lower: Option<Id>,
	) -> Result<Id, Error> {
		let reserved = target
			.catalog
			.reserve()
			.with_hls_hint(config.gop.duration(config.framerate));
		let ceiling = video_ceiling(config, &rendition);
		let (lower, priority) = match lower {
			Some(lower) => {
				let lower = self.producer(lower)?;
				let guard = lower.lock();
				let below = guard.as_ref().ok_or(Error::MediaNotFound)?;
				let below_ceiling = *below.ceiling.read();
				if below_ceiling >= ceiling {
					return Err(Error::InvalidConfig(format!(
						"the lower rendition's {} bps is not below this one's {} bps",
						below_ceiling.as_bps(),
						ceiling.as_bps()
					)));
				}
				(Some(below.ceiling.consume()), below.priority.saturating_sub(1))
			}
			None => (None, hang::catalog::PRIORITY.video),
		};
		// The suffix `moq_video::encode::Producer::new` would derive, built here so the
		// track can take this rendition's priority.
		let suffix = match &rendition.codec {
			hang::catalog::VideoCodec::H264(_) => ".avc3",
			hang::catalog::VideoCodec::H265(_) => ".hev1",
			other => {
				return Err(Error::InvalidConfig(format!(
					"{other} is not a codec this producer can publish"
				)));
			}
		};
		let track = target.producer.unique_track(suffix, reserved.track_info(priority))?;
		let producer = moq_video::encode::Producer::with_track(track, reserved, rendition)?;
		let encoder = Shared::new(VideoEncoder {
			encoder: Some(encoder),
			producer,
			config: config.clone(),
			priority,
			lower,
			format,
			size: config.size(),
			allocator: None,
			reservation: None,
			follow: None,
			followed: None,
			ceiling: kio::Producer::new(ceiling),
			children: target.children.clone(),
		});
		target
			.children
			.insert(&mut self.producers, encoder, crate::publish::Child::Video)
	}

	/// Reserve the encoder's ceiling against `allocator` and follow the grant.
	pub(crate) fn follow(&self, id: Id, allocator: &moq_net::bandwidth::Allocator) -> Result<(), Error> {
		let shared = self.producer(id)?;
		shared.lock().as_mut().ok_or(Error::MediaNotFound)?.allocator = Some(allocator.clone());
		follow_grant(&shared)
	}

	pub(crate) fn reservation(&self, id: Id) -> Result<Option<Arc<moq_net::bandwidth::Reservation>>, Error> {
		Ok(self
			.producer(id)?
			.lock()
			.as_ref()
			.ok_or(Error::MediaNotFound)?
			.reservation
			.clone())
	}

	/// A watch-only handle to the encoded track's subscriber demand.
	pub(crate) fn demand(&self, id: Id) -> Result<moq_net::track::Demand, Error> {
		Ok(self
			.producer(id)?
			.lock()
			.as_ref()
			.ok_or(Error::MediaNotFound)?
			.producer
			.demand())
	}

	/// Resolve a producer handle, so the caller can encode with the global lock
	/// released.
	///
	/// Bind the result before locking it: a temporary [`State`] guard lives to the
	/// end of the statement that created it, so resolving and locking in one
	/// expression would put the encode back under the global lock.
	pub(crate) fn producer(&self, id: Id) -> Result<Shared<VideoEncoder>, Error> {
		self.producers.get(id).cloned().ok_or(Error::MediaNotFound)
	}

	/// Resolve a producer and drop its id, so nothing can be published to it after.
	pub(crate) fn remove(&mut self, id: Id) -> Result<Shared<VideoEncoder>, Error> {
		self.producers.remove(id).ok_or(Error::MediaNotFound)
	}

	/// Decode a rendition and deliver read handles to the callback.
	pub fn consume(
		&mut self,
		rendition: crate::consume::Rendition<hang::catalog::VideoConfig>,
		options: moq_video::decode::Options,
		output: DecoderOutput,
		on_frame: OnStatus,
	) -> Result<Id, Error> {
		let crate::consume::Rendition {
			broadcast,
			config: catalog,
			name,
			driver,
		} = rendition;

		// `Consumer::new` subscribes (blocking on SUBSCRIBE_OK), so run it inside
		// the task to keep this entrypoint non-blocking.
		self.consumer_tasks.spawn(
			on_frame,
			|state| &mut state.video.consumer_tasks,
			|close| async move {
				driver
					.watch(async move {
						let consumer = moq_video::decode::Consumer::new(&broadcast, &catalog, name, options).await?;
						Self::run(on_frame, consumer, close, output).await
					})
					.await
			},
		)
	}

	async fn run(
		callback: OnStatus,
		mut consumer: moq_video::decode::Consumer,
		mut close: oneshot::Receiver<()>,
		output: DecoderOutput,
	) -> Result<(), Error> {
		loop {
			// `biased` so a pending close always wins over a ready read.
			let read = tokio::select! {
				biased;
				_ = &mut close => return Ok(()),
				read = consumer.read() => match read? {
					Some(moq_video::decode::Read::Frame(frame)) => Read::Frame(Arc::new(VideoFrame {
						frame,
						output,
						pixels: OnceLock::new(),
					})),
					Some(moq_video::decode::Read::Gap { timestamp, duration }) => Read::gap(timestamp, duration)?,
					Some(moq_video::decode::Read::End { end }) => Read::end(end)?,
					None => return Ok(()),
				},
			};

			let read_id = State::lock().video.reads.insert(read)?;
			callback.call(Ok(read_id));
		}
	}

	/// Stop decoding and release the consumer handle.
	pub fn consume_close(&mut self, id: Id) -> Result<(), Error> {
		self.consumer_tasks.close(id, Error::TrackNotFound)
	}

	/// Resolve a read handle, so the caller can convert its frame with the global
	/// lock released.
	fn read(&self, id: Id) -> Result<Read<Arc<VideoFrame>>, Error> {
		self.reads.get(id).cloned().ok_or(Error::FrameNotFound)
	}

	/// Release a delivered read handle.
	pub fn read_free(&mut self, id: Id) -> Result<(), Error> {
		self.reads.remove(id).ok_or(Error::FrameNotFound)?;
		Ok(())
	}
}

// ---- C entry points ----

fn pixel_format_from_u32(value: u32) -> Result<moq_video_pixel_format, Error> {
	Ok(match value {
		v if v == moq_video_pixel_format::MOQ_VIDEO_PIXEL_FORMAT_I420 as u32 => {
			moq_video_pixel_format::MOQ_VIDEO_PIXEL_FORMAT_I420
		}
		v if v == moq_video_pixel_format::MOQ_VIDEO_PIXEL_FORMAT_RGBA as u32 => {
			moq_video_pixel_format::MOQ_VIDEO_PIXEL_FORMAT_RGBA
		}
		_ => return Err(Error::InvalidCode),
	})
}

/// Parse [`moq_video_decoder_output`]'s `width`/`height` into the target size:
/// 0x0 is the stream's native size, anything else must be even and non-zero
/// (I420 chroma is subsampled 2x2) and small enough for the packed byte math.
fn decoder_size(width: u32, height: u32) -> Result<Option<moq_video::Size>, Error> {
	if width == 0 && height == 0 {
		return Ok(None);
	}
	if width == 0 || height == 0 || !width.is_multiple_of(2) || !height.is_multiple_of(2) {
		return Err(Error::InvalidConfig(format!(
			"decode size {width}x{height}: use 0x0 for the native size or even non-zero dimensions"
		)));
	}
	let size = moq_video::Size::new(width, height);
	if size
		.pixels()
		.checked_mul(4)
		.is_none_or(|bytes| usize::try_from(bytes).is_err())
	{
		return Err(Error::InvalidConfig(format!(
			"decode size {width}x{height}: dimensions too large to represent"
		)));
	}
	Ok(Some(size))
}

fn codec_from_u32(value: u32) -> Result<moq_video::encode::Codec, Error> {
	use moq_video::encode::Codec;
	Ok(match value {
		v if v == moq_video_codec::MOQ_VIDEO_CODEC_H264 as u32 => Codec::H264,
		v if v == moq_video_codec::MOQ_VIDEO_CODEC_H265 as u32 => Codec::H265,
		_ => return Err(Error::InvalidCode),
	})
}

/// # Safety
/// - `output->encoder` must point to `output->encoder_len` bytes of UTF-8 when
///   `output->kind` is `MOQ_VIDEO_ENCODER_KIND_NAMED`.
unsafe fn encoder_kind(output: &moq_video_encoder_output) -> Result<moq_video::encode::Kind, Error> {
	use moq_video::encode::Kind;
	Ok(match output.kind {
		v if v == moq_video_encoder_kind::MOQ_VIDEO_ENCODER_KIND_AUTO as u32 => Kind::Auto,
		v if v == moq_video_encoder_kind::MOQ_VIDEO_ENCODER_KIND_HARDWARE as u32 => Kind::Hardware,
		v if v == moq_video_encoder_kind::MOQ_VIDEO_ENCODER_KIND_SOFTWARE as u32 => Kind::Software,
		v if v == moq_video_encoder_kind::MOQ_VIDEO_ENCODER_KIND_NAMED as u32 => {
			Kind::Named(unsafe { ffi::parse_str(output.encoder, output.encoder_len)? }.to_string())
		}
		_ => return Err(Error::InvalidCode),
	})
}

/// Open a video track on a broadcast, encoding the raw frames you publish to it.
///
/// The encoder is opened here, so an unsupported codec, resolution, or backend
/// fails now rather than on the first frame. The track is named after the codec
/// (`.avc3` / `.hev1`) and its catalog rendition is published immediately, read
/// out of the encoder rather than guessed, so a subscriber can find the track
/// before a frame is written to it.
///
/// Returns a non-zero handle on success or a negative error code.
///
/// # Safety
/// - `input` / `output` must point to fully populated structs.
/// - `output->encoder` must point to `output->encoder_len` bytes of UTF-8 when
///   `output->kind` is `MOQ_VIDEO_ENCODER_KIND_NAMED`.
/// - `options` may be NULL, or must point at a readable [`moq_video_encode_options`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_encode_video(
	broadcast: u32,
	input: *const moq_video_encoder_input,
	output: *const moq_video_encoder_output,
	options: *const moq_video_encode_options,
) -> i32 {
	ffi::enter(move || {
		let broadcast = ffi::parse_id(broadcast)?;
		let raw_input = unsafe { input.as_ref() }.ok_or(Error::InvalidPointer)?;
		let raw_output = unsafe { output.as_ref() }.ok_or(Error::InvalidPointer)?;
		let options = unsafe { options.as_ref() }.copied().unwrap_or_default();
		let bandwidth = ffi::parse_id_optional(options.bandwidth)?;
		let lower = ffi::parse_id_optional(options.lower)?;
		if lower.is_some() && bandwidth.is_none() {
			return Err(Error::InvalidConfig(
				"a lower rendition needs a bandwidth allocator to follow".into(),
			));
		}

		let format = pixel_format_from_u32(raw_input.format)?;

		let framerate = moq_video::FrameRate::new(raw_input.framerate.numerator, raw_input.framerate.denominator)
			.map_err(|err| Error::InvalidConfig(format!("invalid framerate: {err}")))?;
		let mut config = moq_video::encode::Config::new(raw_input.width, raw_input.height, framerate);
		config.codec = codec_from_u32(raw_output.codec)?;
		config.kind = unsafe { encoder_kind(raw_output)? };
		// The C ABI spells an unset knob as 0, which neither field accepts as a real
		// value: a zero bitrate or GOP is the default, not a request.
		config.bitrate = (raw_output.bitrate != 0).then(|| moq_net::bandwidth::Rate::from_bps(raw_output.bitrate));
		if raw_output.gop != 0 {
			config.gop = moq_video::encode::Gop::Keyframe {
				interval: raw_output.gop,
			};
		}

		// Both before the global lock is taken: bringing up a hardware encoder is slow
		// enough that every other call would wait behind it. The probe runs first and
		// closes its encoder before this one opens, so only one codec session is live.
		let rendition = block_on(config.probe())?;
		let encoder = block_on(moq_video::encode::Sink::open(&config))?;

		let mut state = State::lock();
		let allocator = bandwidth.map(|id| state.bandwidth.allocator(id)).transpose()?;
		let State { publish, video, .. } = &mut *state;
		let id = video.publish(
			publish.target(broadcast)?,
			format,
			&config,
			rendition.clone(),
			encoder,
			lower,
		)?;
		if let Some(allocator) = allocator.as_ref() {
			video.follow(id, allocator)?;
		}
		Ok(id)
	})
}

/// This encoder's bandwidth reservation, or 0 if it was published without one.
///
/// Closing the returned handle does not release the encoder's claim; that lasts
/// until [moq_encode_video_finish].
#[unsafe(no_mangle)]
pub extern "C" fn moq_encode_video_reservation(producer: u32) -> i32 {
	ffi::enter(move || {
		let producer = ffi::parse_id(producer)?;
		let mut state = State::lock();
		match state.video.reservation(producer)? {
			Some(reservation) => Ok(i32::from(state.bandwidth.hold(reservation)?)),
			None => Ok(0),
		}
	})
}

/// Watch whether the encoded video track has subscribers, so the camera and encoder
/// run only while someone watches. See [`crate::moq_publish_media_demand`] for the
/// callback contract.
///
/// Returns a non-zero watcher handle on success, or a negative code on failure.
///
/// # Safety
/// - `on_demand` must be non-NULL.
/// - The caller must keep `user_data` valid until the terminal (`<= 0`) `on_demand` callback.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_encode_video_demand(
	producer: u32,
	on_demand: crate::moq_status_callback,
	user_data: *mut c_void,
) -> i32 {
	ffi::enter(move || {
		let producer = ffi::parse_id(producer)?;
		let on_demand = unsafe { OnStatus::new(user_data, on_demand)? };
		let mut state = State::lock();
		let demand = state.video.demand(producer)?;
		state.publish.demand(demand, on_demand)
	})
}

/// Encode and publish one raw frame.
///
/// `frame->data` is borrowed for the duration of the call and must be exactly one
/// picture in the pixel format and at the resolution declared by
/// [`moq_video_encoder_input`].
/// A backend that pipelines publishes an earlier frame's output here, so a call
/// that emits nothing is normal rather than an error. While a rung published with a
/// `lower` rendition is disabled for lack of bandwidth, the frame is dropped
/// without encoding.
///
/// # Safety
/// - `frame` must point to a valid [`moq_video_encoder_frame`].
/// - `frame->data` must point to `frame->data_size` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_encode_video_frame(producer: u32, frame: *const moq_video_encoder_frame) -> i32 {
	ffi::enter(move || {
		let producer = ffi::parse_id(producer)?;
		let frame = unsafe { frame.as_ref() }.ok_or(Error::InvalidPointer)?;
		let data = unsafe { ffi::parse_slice(frame.data, frame.data_size)? };

		let producer = State::lock().video.producer(producer)?;
		let moved = producer
			.lock()
			.as_mut()
			.ok_or(Error::MediaNotFound)?
			.publish_frame(frame.timestamp_us, data)?;
		// The follower takes the encoder's lock, so it moves with the track after the encode.
		match moved {
			true => crate::video::follow_grant(&producer),
			false => Ok(()),
		}
	})
}

/// Cut a new group at the next published frame.
///
/// Optional. The encoder already keyframes every `moq_video_encoder_output.gop`
/// frames, and each of those cuts a group, so a subscriber can always join
/// without you calling this. Reach for it only to place the boundaries yourself:
/// aligning groups with something the encoder cannot see, such as a scene change,
/// a source switch, or resuming after an idle gap.
///
/// The next frame is encoded as a keyframe, which closes the open group and
/// starts a new one at it. Calling this repeatedly before that frame arrives cuts
/// once, not several times.
///
/// Fails when the selected encoder cannot force a keyframe (a V4L2 driver
/// without the control): nothing is queued, and groups keep falling every
/// `gop` frames.
#[unsafe(no_mangle)]
pub extern "C" fn moq_encode_video_cut(producer: u32) -> i32 {
	ffi::enter(move || {
		let producer = ffi::parse_id(producer)?;
		let producer = State::lock().video.producer(producer)?;
		producer.lock().as_mut().ok_or(Error::MediaNotFound)?.publish_cut()
	})
}

/// Retune a live encoder to `bitrate` bits per second, taking effect from
/// roughly the next frame. No keyframe is forced, so this is cheap enough to
/// drive from a congestion controller.
///
/// The configured bitrate is a ceiling on some backends (openh264 rejects a raise
/// above the rate it opened at), so set `bitrate` to the highest you will ask
/// for and adapt downwards from there.
///
/// When this encoder was published against a bandwidth allocator, the reservation
/// and follower ceiling move with it, so a later grant cannot retune above this
/// value.
///
/// Returns a negative code if this backend cannot retune while running. That is
/// not fatal: the encoder keeps running at its current rate, so stop adapting
/// rather than stop publishing.
#[unsafe(no_mangle)]
pub extern "C" fn moq_encode_video_bitrate(producer: u32, bitrate: u64) -> i32 {
	ffi::enter(move || {
		let producer = ffi::parse_id(producer)?;
		let producer = State::lock().video.producer(producer)?;
		producer
			.lock()
			.as_mut()
			.ok_or(Error::MediaNotFound)?
			.publish_bitrate(bitrate)
	})
}

/// Flush any frames the codec is still holding and finalize the video track.
///
/// The handle is released, so nothing can be published to it afterwards.
/// The broadcast keeps the finished track, catalog rendition included, until it closes, so a
/// late subscriber can still drain it.
#[unsafe(no_mangle)]
pub extern "C" fn moq_encode_video_finish(producer: u32) -> i32 {
	ffi::enter(move || {
		let id = ffi::parse_id(producer)?;
		// The id stays until the drain ends, so a racing moq_publish_close waits it out
		// instead of missing a track that still publishes. A call queued behind the drain
		// finds the encoder gone.
		let shared = State::lock().video.producer(id)?;
		let finished = {
			let mut guard = shared.lock();
			guard.take().ok_or(Error::MediaNotFound)?.publish_finish(id)
		};
		// A racing close may have released the id already.
		let _ = State::lock().video.remove(id);
		finished
	})
}

/// Subscribe to a video track and decode it into raw frames in the requested
/// CPU pixel format and size (see [`moq_video_decoder_output`]).
///
/// The catalog `index` selects which video rendition to subscribe to, matching
/// the existing `moq_consume_video` selection model. Only H.264 is
/// supported; a non-H.264 rendition fails on the terminal callback.
///
/// An unknown `output->format` or an invalid `output->width`/`height` fails
/// here, before subscribing. Each frame is produced in the requested layout by
/// [`moq_decode_video_read`], which fails for that frame rather than deliver
/// another layout.
///
/// Returns a non-zero handle on success or a negative error code.
///
/// `on_read` is called with a positive read id per decoded frame, gap, or encoder
/// end (see [`moq_decode_video_read`]), then exactly once more with a terminal
/// code: `0` (closed cleanly) or a negative error. After the terminal (`<= 0`)
/// callback, `on_read` is never called again and `user_data` is never touched
/// again, so release `user_data` there. The terminal callback fires even after
/// [`moq_decode_video_cancel`].
///
/// Starts at the newest cached group so reopening live playback skips the backlog.
///
/// # Safety
/// - `output` must point to a valid [`moq_video_decoder_output`].
/// - `user_data` must stay valid until the terminal (`<= 0`) `on_read` callback.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_decode_video(
	catalog: u32,
	index: u32,
	output: *const moq_video_decoder_output,
	on_read: crate::moq_status_callback,
	user_data: *mut c_void,
) -> i32 {
	ffi::enter(move || {
		let raw = unsafe { output.as_ref() }.ok_or(Error::InvalidPointer)?;

		// Validate the request before resolving anything: a C caller can probe
		// support without a live track, and a bad request never opens a
		// subscription it would immediately drop.
		let format = pixel_format_from_u32(raw.format)?;
		let size = decoder_size(raw.width, raw.height)?;
		let catalog = ffi::parse_id(catalog)?;

		let mut options = moq_video::decode::Options::new();
		options.start = moq_video::decode::Start::Latest;
		options.max_delay = Duration::from_micros(raw.max_delay_us);
		// The C caller takes packed pixels, so let a backend that can decode
		// straight to the CPU do that rather than downloading afterwards.
		options.decoder.output = moq_video::Output::Cpu;
		// A backend with a hardware scaler (NVDEC) honors this for free; the
		// delivery loop still enforces it, since other backends ignore it.
		options.decoder.scale_hint = size;
		let output = DecoderOutput { format, size };
		let on_read = unsafe { OnStatus::new(user_data, on_read)? };

		let mut state = State::lock();
		let rendition = state.consume.rendition(catalog, index as usize)?;

		let State { video, .. } = &mut *state;
		video.consume(rendition, options, output, on_read)
	})
}

/// Stop a video (raw) consumer's background task.
///
/// Returns immediately: zero on success, or a negative code if already closed.
/// Does NOT free `user_data`; the on-read callback still fires once more with a
/// terminal `0` (or a negative error), which is where `user_data` should be
/// released. Read ids already delivered are likewise not freed: each stays
/// readable after the terminal callback until released with
/// [`moq_decode_video_read_free`].
#[unsafe(no_mangle)]
pub extern "C" fn moq_decode_video_cancel(consumer: u32) -> i32 {
	ffi::enter(move || {
		let consumer = ffi::parse_id(consumer)?;
		State::lock().video.consume_close(consumer)
	})
}

/// Write a delivered read into `dst`: its kind, and the frame's pixels, the gap,
/// or the end it carries.
///
/// The first call for a frame's `id` converts the decoded picture to the layout
/// and size [`moq_decode_video`] asked for; later calls return the same pixels. A
/// conversion failure returns a negative code for this frame only, leaving the
/// id to be freed as usual. Safe to call from any thread, including inside the
/// read callback.
///
/// The written `dst->frame.data` pointer remains valid until the same `id` is
/// released with [`moq_decode_video_read_free`].
///
/// # Safety
/// - `dst` must point to a writable [`moq_video_read`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_decode_video_read(id: u32, dst: *mut moq_video_read) -> i32 {
	ffi::enter(move || {
		let id = ffi::parse_id(id)?;
		let dst = unsafe { dst.as_mut() }.ok_or(Error::InvalidPointer)?;
		let read = State::lock().video.read(id)?;
		let (kind, gap, end_us) = read.info();
		let frame = match &read {
			Read::Frame(frame) => {
				let pixels = frame.pixels()?;
				moq_video_frame {
					// The decoded Timestamp is bounded by a QUIC varint, so its microseconds fit.
					timestamp_us: frame.frame.timestamp.as_micros() as u64,
					width: pixels.width,
					height: pixels.height,
					// The slab keeps its `Arc` until the id is freed, and the pixels are
					// never replaced once set, so the pointer outlives this call.
					data: pixels.data.as_ptr(),
					data_size: pixels.data.len(),
				}
			}
			_ => moq_video_frame {
				timestamp_us: 0,
				width: 0,
				height: 0,
				data: std::ptr::null(),
				data_size: 0,
			},
		};
		*dst = moq_video_read {
			kind,
			frame,
			gap,
			end_us,
		};
		Ok(())
	})
}

/// Free a read previously delivered through the decode callback, releasing a
/// frame's decoded picture back to the decoder. Required for every delivered read
/// id; closing the parent consumer is not enough, and a decoder whose frames are
/// all held stalls until some are freed.
#[unsafe(no_mangle)]
pub extern "C" fn moq_decode_video_read_free(id: u32) -> i32 {
	ffi::enter(move || {
		let id = ffi::parse_id(id)?;
		State::lock().video.read_free(id)
	})
}

#[cfg(test)]
mod tests {
	use super::*;

	/// A video track wired up without an encoder, plus a subscriber on it: enough
	/// to pin what [`finalize`] shows the far end.
	async fn track_under_test() -> (moq_video::encode::Producer, moq_net::track::Subscriber) {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let config = moq_mux::catalog::Config::default()
			.with_catalog(moq_mux::catalog::hang::Catalog::<moq_mux::catalog::hang::Extra>::default());
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, config).unwrap();
		let consumer = broadcast.consume();
		// Probed rather than hand-built, so the test track carries what a real one would.
		let encode = moq_video::encode::Config::new(320, 240, moq_video::FrameRate::new(30, 1).unwrap());
		let rendition = encode.probe().await.unwrap();
		let reserved = catalog.reserve().with_hls_hint(encode.gop.duration(encode.framerate));
		let producer = moq_video::encode::Producer::new(broadcast, reserved, rendition).unwrap();

		let name = producer.demand().name().to_string();
		let track = consumer.track(&name).unwrap().subscribe(None).await.unwrap();
		(producer, track)
	}

	/// A resume that reopens the encoder onto a larger picture publishes a new track (a higher
	/// level is a new codec string), and the bandwidth claim moves there with it rather than
	/// watching the track that ended.
	#[tokio::test]
	async fn a_resume_onto_a_new_level_reserves_on_the_new_track() {
		let (producer, _track) = track_under_test().await;
		let first = producer.demand().name().to_string();
		let probed = moq_video::encode::Config::new(320, 240, moq_video::FrameRate::new(30, 1).unwrap());
		let mut config = moq_video::encode::Config::new(640, 480, moq_video::FrameRate::new(30, 1).unwrap());
		config.kind = moq_video::encode::Kind::Software;
		let mut opened = probed.clone();
		opened.kind = moq_video::encode::Kind::Software;
		let estimate = moq_net::bandwidth::Producer::new();
		let shared = Shared::new(VideoEncoder {
			encoder: Some(moq_video::encode::Sink::open(&opened).await.unwrap()),
			producer,
			config: config.clone(),
			priority: hang::catalog::PRIORITY.video,
			lower: None,
			format: moq_video_pixel_format::MOQ_VIDEO_PIXEL_FORMAT_RGBA,
			size: probed.size(),
			allocator: Some(moq_net::bandwidth::Allocator::new(estimate.consume())),
			reservation: None,
			follow: None,
			followed: None,
			ceiling: kio::Producer::new(moq_net::bandwidth::Rate::from_mbps(1)),
			children: Default::default(),
		});
		let moved = tokio::task::spawn_blocking(move || {
			follow_grant(&shared).unwrap();
			let before = shared.lock().as_ref().unwrap().reservation.clone().unwrap();
			let small = vec![0x80; 320 * 240 * 4];
			assert!(!shared.lock().as_mut().unwrap().publish_frame(0, &small).unwrap());

			// Disabled, then resumed onto the larger picture.
			{
				let mut guard = shared.lock();
				let video = guard.as_mut().unwrap();
				video.apply(moq_mux::rate::Target::Disabled).unwrap();
				video.size = config.size();
				video
					.apply(moq_mux::rate::Target::Enabled(moq_net::bandwidth::Rate::from_mbps(1)))
					.unwrap();
			}
			let large = vec![0x80; 640 * 480 * 4];
			let moved = shared.lock().as_mut().unwrap().publish_frame(66_666, &large).unwrap();
			assert!(moved, "the larger picture is a new codec string");
			follow_grant(&shared).unwrap();
			let guard = shared.lock();
			let video = guard.as_ref().unwrap();
			let after = video.reservation.clone().unwrap();
			assert!(!Arc::ptr_eq(&before, &after), "the claim still watches the ended track");
			video.producer.demand().name().to_string()
		})
		.await
		.unwrap();
		assert_eq!(moved, format!("{first}/1"));
	}

	/// A clean finish reaches the subscriber as the end of the track, which is what
	/// makes the abort case below meaningful rather than vacuous.
	#[tokio::test]
	async fn a_successful_drain_ends_the_track_cleanly() {
		let (producer, mut track) = track_under_test().await;
		finalize(
			producer,
			Ok(()),
			&Default::default(),
			crate::publish::Child::Video(Id::try_from(1u32).unwrap()),
		)
		.unwrap();
		assert!(matches!(track.recv_group().await, Ok(None)), "expected a clean end");
	}

	/// Regression: a lost tail must reach the subscriber as an abort. Finishing the
	/// track anyway would report a truncated stream as a complete one, and only the
	/// publisher would ever know otherwise.
	#[tokio::test]
	async fn a_failed_drain_aborts_the_track() {
		let (producer, mut track) = track_under_test().await;
		let err = moq_video::Error::Codec(anyhow::anyhow!("the codec lost the tail"));
		finalize(
			producer,
			Err(err),
			&Default::default(),
			crate::publish::Child::Video(Id::try_from(1u32).unwrap()),
		)
		.unwrap_err();

		let Err(err) = track.recv_group().await else {
			panic!("expected an abort, not a clean end");
		};
		assert!(
			err.to_string().contains("the codec lost the tail"),
			"the abort should carry the drain failure: {err}"
		);
	}

	/// A rate follower panic stops adaptation, not the track: finishing still ends
	/// it cleanly, then reports the panic.
	#[tokio::test]
	async fn finish_ends_the_track_after_a_rate_follower_panic() {
		let (producer, mut track) = track_under_test().await;
		let mut config = moq_video::encode::Config::new(320, 240, moq_video::FrameRate::new(30, 1).unwrap());
		config.kind = moq_video::encode::Kind::Software;
		let encoder = moq_video::encode::Sink::open(&config).await.unwrap();
		let followed = crate::ffi::Background::new(async { panic!("injected rate follower panic") });
		assert!(matches!(followed.finished().await, Err(Error::Panic)));
		let video = VideoEncoder {
			encoder: Some(encoder),
			producer,
			config: config.clone(),
			priority: hang::catalog::PRIORITY.video,
			lower: None,
			format: moq_video_pixel_format::MOQ_VIDEO_PIXEL_FORMAT_I420,
			size: config.size(),
			allocator: None,
			reservation: None,
			follow: None,
			followed: Some(followed),
			ceiling: kio::Producer::new(moq_net::bandwidth::Rate::from_mbps(1)),
			children: Default::default(),
		};

		let id = Id::try_from(1u32).unwrap();
		let result = tokio::task::spawn_blocking(move || video.publish_finish(id))
			.await
			.unwrap();
		assert!(matches!(result, Err(Error::Panic)), "{result:?}");
		assert!(matches!(track.recv_group().await, Ok(None)), "expected a clean end");
	}
}

#[cfg(test)]
mod follow_tests {
	use std::time::Duration;

	use moq_mux::rate::{Follower, Policy, Target};
	use moq_net::bandwidth::Rate;

	use super::*;

	/// Retune through `set`, which only ever sees an enabled target: these policies set no
	/// lower rendition, so nothing disables.
	fn rate(
		follower: &mut Follower,
		ceiling: Rate,
		target: Target,
		set: impl FnOnce(Rate) -> Result<(), moq_video::Error>,
	) -> Result<Option<Target>, &'static str> {
		retune(follower, Policy::new(ceiling), target, |target| match target {
			Target::Enabled(bitrate) => set(bitrate),
			Target::Disabled => unreachable!("no lower rendition"),
		})
	}

	/// A lower rendition moving between a target and its apply drops that target, and the
	/// follower asks for it again under the new policy rather than counting it applied.
	#[tokio::test(start_paused = true)]
	async fn a_moved_lower_retries_the_dropped_target() {
		let policy = |lower| {
			let mut policy = Policy::new(Rate::from_mbps(4));
			policy.lower = Some(lower);
			policy
		};
		let grant = moq_net::bandwidth::Producer::new();
		let mut follower = Follower::new(grant.consume(), policy(Rate::from_mbps(1)));
		grant.set(Some(Rate::from_mbps(2))).unwrap();
		let dropped = follower.next().await.unwrap();
		let moved = retune(&mut follower, policy(Rate::from_kbps(900)), dropped, |_| {
			unreachable!("a target under a stale policy is never applied")
		});
		assert_eq!(moved, Ok(None));

		let retried = moq_clock::timeout(Duration::from_secs(60), follower.next())
			.await
			.expect("the dropped target is asked for again");
		assert_eq!(retried.unwrap(), Target::Enabled(Rate::from_mbps(2)));
	}

	fn bitrate(target: Target) -> Rate {
		let Target::Enabled(bitrate) = target else {
			unreachable!("no lower rendition")
		};
		bitrate
	}

	/// Retune at every target until the follower goes a minute without one.
	async fn quiet(follower: &mut Follower, ceiling: Rate, mut set: impl FnMut(Rate) -> Result<(), moq_video::Error>) {
		while let Ok(bitrate) = moq_clock::timeout(Duration::from_secs(60), follower.next()).await {
			rate(follower, ceiling, bitrate.unwrap(), &mut set).unwrap();
		}
	}

	/// The grant recovers once and never changes again, and the final raise fails once:
	/// the follower still carries the encoder back to its ceiling.
	#[tokio::test(start_paused = true)]
	async fn a_quiet_grant_retries_a_failed_raise() {
		let ceiling = Rate::from_mbps(4);
		let grant = moq_net::bandwidth::Producer::new();
		let mut follower = Follower::new(grant.consume(), Policy::new(ceiling));
		let mut applied = Vec::new();
		let mut refuse = Some(ceiling);
		let mut set = |bitrate| {
			if refuse.take_if(|refused| *refused == bitrate).is_some() {
				return Err(moq_video::Error::Unsupported("a transient refusal".into()));
			}
			applied.push(bitrate);
			Ok(())
		};

		grant.set(Some(Rate::from_mbps(1))).unwrap();
		let bitrate = follower.next().await.unwrap();
		rate(&mut follower, ceiling, bitrate, &mut set).unwrap();
		tokio::time::advance(Duration::from_secs(1)).await;
		grant.set(Some(ceiling)).unwrap();
		quiet(&mut follower, ceiling, &mut set).await;

		assert_eq!(refuse, None, "the final raise was never refused");
		assert_eq!(applied.last(), Some(&ceiling), "{applied:?}");
	}

	/// `moq_encode_video_bitrate` lowers the ceiling while a raise toward the old one is
	/// due. The ceiling is read under the encoder lock, so the raise is dropped rather
	/// than retuning above the new cap, and the encoder recovers to the new cap.
	#[tokio::test(start_paused = true)]
	async fn a_lowered_ceiling_beats_a_due_raise() {
		let old = Rate::from_mbps(4);
		let lowered = Rate::from_mbps(2);
		let grant = moq_net::bandwidth::Producer::new();
		let mut follower = Follower::new(grant.consume(), Policy::new(old));
		let applied = std::cell::RefCell::new(Vec::new());
		let set = |bitrate| {
			applied.borrow_mut().push(bitrate);
			Ok(())
		};

		grant.set(Some(Rate::from_mbps(1))).unwrap();
		assert_eq!(follower.next().await.unwrap(), Target::Enabled(Rate::from_mbps(1)));
		tokio::time::advance(Duration::from_secs(1)).await;
		grant.set(Some(old)).unwrap();
		let first = follower.next().await.unwrap();
		rate(&mut follower, old, first, set).unwrap();

		// `moq_encode_video_bitrate` already retuned the encoder to the new cap.
		let due = follower.next().await.unwrap();
		assert!(bitrate(due) > bitrate(first), "a raise toward the old ceiling");
		assert_eq!(rate(&mut follower, lowered, due, set), Ok(None));
		assert_eq!(follower.policy().max, lowered);
		quiet(&mut follower, lowered, set).await;
		assert_eq!(*applied.borrow(), [bitrate(first)], "nothing retuned past the new cap");

		// A later attack and recovery climbs back to the new cap, not the old one.
		grant.set(Some(Rate::from_mbps(1))).unwrap();
		let bitrate = follower.next().await.unwrap();
		rate(&mut follower, lowered, bitrate, set).unwrap();
		tokio::time::advance(Duration::from_secs(1)).await;
		grant.set(Some(old)).unwrap();
		quiet(&mut follower, lowered, set).await;

		let applied = applied.into_inner();
		assert_eq!(applied.last(), Some(&lowered), "{applied:?}");
		assert!(applied.iter().all(|bitrate| *bitrate <= lowered), "{applied:?}");
	}
}
