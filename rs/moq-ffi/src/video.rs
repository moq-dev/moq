//! Raw-video publishing via [`moq_video`].
//!
//! Sibling to [`audio`](crate::audio)'s producer, and the video counterpart to
//! [`producer::MoqMediaProducer`](crate::producer::MoqMediaProducer): that one
//! takes already-encoded frames, this one takes raw pictures and runs the H.264
//! / H.265 encode inside the FFI boundary (VideoToolbox on macOS, Media
//! Foundation on Windows, openh264 as the software fallback; no ffmpeg).
//!
//! Pixel format, resolution, and framerate are fixed at publish time via
//! [`MoqVideoEncoderInput`], so each [`MoqVideoFrame`] carries only pixels and a
//! timestamp.

use std::sync::Arc;

use crate::consumer::MoqBroadcastConsumer;
use crate::error::MoqError;
use crate::producer::MoqBroadcastProducer;

/// Pixel layout of the raw frames passed to [`MoqVideoProducer::write`].
#[derive(Clone, Copy, uniffi::Enum)]
pub enum MoqVideoPixelFormat {
	/// Tightly-packed planar I420: Y, then U, then V, no row padding
	/// (`width * height * 3 / 2` bytes).
	I420,
	/// Tightly-packed RGBA, `width * height * 4` bytes, no row padding.
	Rgba,
}

/// Output video codec.
///
/// Not every codec has a backend on every machine: H.265 is hardware-only, so
/// publishing it fails where no hardware encoder is available.
#[derive(Clone, Copy, uniffi::Enum)]
pub enum MoqVideoCodec {
	/// H.264 / AVC, published as an `avc3` track.
	H264,
	/// H.265 / HEVC, published as a `hev1` track.
	H265,
}

impl From<MoqVideoCodec> for moq_video::encode::Codec {
	fn from(c: MoqVideoCodec) -> Self {
		match c {
			MoqVideoCodec::H264 => Self::H264,
			MoqVideoCodec::H265 => Self::H265,
		}
	}
}

/// Which encoder implementation to use.
///
/// These bindings compile VideoToolbox (macOS), Media Foundation (Windows), and
/// openh264 (software, everywhere). NVENC/NVDEC are a libmoq-only build option
/// and VAAPI is opt-in everywhere, so Linux here is software-only.
#[derive(Clone, uniffi::Enum)]
pub enum MoqVideoEncoderKind {
	/// Prefer a platform hardware encoder, falling back to software. On Linux
	/// that fallback is the only option these bindings have.
	Auto,
	/// Hardware only; fails if none is available, which on Linux is always.
	Hardware,
	/// Software only (openh264, H.264 only).
	Software,
	/// A specific backend that moq-ffi compiles: `"videotoolbox"` (macOS),
	/// `"mediafoundation"` (Windows), or `"openh264"` (software, everywhere).
	/// Naming one this build lacks fails with a no-encoder error, so reach for
	/// this only when [`Auto`](Self::Auto) picks the wrong one.
	Named { name: String },
}

impl From<MoqVideoEncoderKind> for moq_video::encode::Kind {
	fn from(k: MoqVideoEncoderKind) -> Self {
		match k {
			MoqVideoEncoderKind::Auto => Self::Auto,
			MoqVideoEncoderKind::Hardware => Self::Hardware,
			MoqVideoEncoderKind::Software => Self::Software,
			MoqVideoEncoderKind::Named { name } => Self::Named(name),
		}
	}
}

/// Raw frame layout the caller will pass to [`MoqVideoProducer::write`], plus
/// the resolution and rate the encoder is opened at. Every written frame must
/// match `width` x `height`; scale before writing if your source moves.
#[derive(uniffi::Record)]
pub struct MoqVideoEncoderInput {
	pub format: MoqVideoPixelFormat,
	/// Encoded width in pixels. Must be even (I420 chroma is subsampled 2x2).
	pub width: u32,
	/// Encoded height in pixels. Must be even.
	pub height: u32,
	/// Nominal frames per second, used for the codec time base and the default
	/// bitrate and keyframe interval. Must be non-zero.
	pub framerate: u32,
}

/// Codec-side configuration.
#[derive(uniffi::Record)]
pub struct MoqVideoEncoderOutput {
	pub codec: MoqVideoCodec,
	/// Target bitrate in bits per second. `None` derives one from the resolution
	/// and framerate.
	#[uniffi(default = None)]
	pub bitrate: Option<u64>,
	/// Keyframe interval in frames: a subscriber joining mid-stream waits at most
	/// this many frames before it can decode. `None` uses ~2 seconds.
	#[uniffi(default = None)]
	pub gop: Option<u32>,
	/// Encoder implementation preference. Pass
	/// [`MoqVideoEncoderKind::Auto`] unless you need a specific backend.
	pub kind: MoqVideoEncoderKind,
}

/// One raw video frame: pixels plus a presentation timestamp.
///
/// The pixel format and resolution are fixed by [`MoqVideoEncoderInput`] at
/// publish time, so a frame carries neither. `data` is exactly one picture in
/// that layout.
#[derive(uniffi::Record)]
pub struct MoqVideoFrame {
	/// Presentation timestamp, in microseconds.
	pub timestamp_us: u64,
	/// The pixels, in the configured layout.
	pub data: Vec<u8>,
}

/// End a video track, given the result of draining its encoder into it.
///
/// A clean finish is a promise that the track holds everything the publisher
/// produced, so a lost tail has to end the track as an abort instead. Finishing
/// anyway would leave a truncated stream indistinguishable from a complete one,
/// and only the local caller would ever learn otherwise.
fn finalize(
	producer: moq_video::encode::Producer<moq_mux::catalog::hang::Extra>,
	drained: Result<(), moq_video::Error>,
) -> Result<(), MoqError> {
	match drained {
		Ok(()) => Ok(producer.finish()?),
		Err(err) => {
			producer.abort(moq_net::Error::Transport(err.to_string()));
			Err(err.into())
		}
	}
}

/// Wait out an encode-thread round trip from a synchronous export.
///
/// The producer methods are sync, matching every other write path here
/// ([`MoqAudioProducer::write`](crate::audio::MoqAudioProducer::write) and the
/// json ones), so this is where [`Sink`](moq_video::encode::Sink)'s futures stop.
/// Blocking is also what paces the caller: a raw frame is megabytes, so a `write`
/// free to run ahead of the codec would queue pictures without bound.
///
/// `pollster` rather than a tokio helper because those panic when the calling
/// thread is driving a runtime, which the one dispatching a uniffi callback is.
fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
	pollster::block_on(future)
}

/// An encoder paired with the track publishing its output.
///
/// The encoder is a [`Sink`](moq_video::encode::Sink) rather than a bare
/// `Encoder` because this object is shared across threads: uniffi hands it to
/// whichever thread the caller writes from, and an `Encoder` built on one thread
/// and dropped on another unbalances the per-thread COM apartment the Windows
/// backend opens. The sink owns the thread instead, so every caller is welcome.
struct VideoProducer {
	encoder: moq_video::encode::Sink,
	producer: moq_video::encode::Producer<moq_mux::catalog::hang::Extra>,
	format: MoqVideoPixelFormat,
	/// The encoded resolution, from the publish config. Frames carry only pixels,
	/// so this is what says how to read them.
	size: moq_video::Size,
}

impl VideoProducer {
	fn write(&mut self, frame: MoqVideoFrame) -> Result<(), MoqError> {
		// A buffer that isn't one picture at the configured size is rejected here,
		// by the surface constructors, rather than reinterpreted.
		let surface = match self.format {
			MoqVideoPixelFormat::I420 => {
				moq_video::Surface::I420(moq_video::I420::new(self.size.width, self.size.height, frame.data)?)
			}
			MoqVideoPixelFormat::Rgba => moq_video::Surface::rgba(&frame.data, self.size)?,
		};

		let frame = moq_video::Frame::new(surface, moq_net::Timestamp::from_micros(frame.timestamp_us)?);
		// A backend that pipelines hands back an earlier frame's output, so this is
		// zero or more access units rather than one per call.
		let encoded = block_on(self.encoder.encode(frame))?;
		self.producer.publish(&encoded)?;
		Ok(())
	}

	fn finish(self) -> Result<(), MoqError> {
		let Self {
			encoder, mut producer, ..
		} = self;
		// Drain the codec into the track before ending it, so the last frames land
		// in it rather than being dropped with the encoder.
		let drained = block_on(encoder.finish()).and_then(|encoded| producer.publish(&encoded));
		finalize(producer, drained)
	}
}

/// Producer for a raw-video track.
///
/// Built via [`MoqBroadcastProducer::publish_video`]. Each
/// [`write`](Self::write) accepts a [`MoqVideoFrame`] whose `data` is in the
/// pixel format declared by the [`MoqVideoEncoderInput`] passed at publish time.
#[derive(uniffi::Object)]
pub struct MoqVideoProducer {
	inner: std::sync::Mutex<Option<VideoProducer>>,
}

#[uniffi::export]
impl MoqVideoProducer {
	/// Encode and publish one raw frame.
	///
	/// A backend that pipelines publishes an earlier frame's output here, so a
	/// call that emits nothing on the wire is normal rather than an error.
	pub fn write(&self, frame: MoqVideoFrame) -> Result<(), MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let mut guard = self.inner.lock().unwrap();
		let producer = guard.as_mut().ok_or(MoqError::Closed)?;
		producer.write(frame)
	}

	/// Cut a new group at the next written frame.
	///
	/// Optional. The encoder already keyframes every
	/// [`gop`](MoqVideoEncoderOutput::gop) frames, and each of those cuts a
	/// group, so a subscriber can always join without you calling this. Reach for
	/// it only to place the boundaries yourself: aligning groups with something
	/// the encoder cannot see, such as a scene change, a source switch, or
	/// resuming after an idle gap.
	///
	/// The next frame is encoded as a keyframe, which closes the open group and
	/// starts a new one at it. Calling this repeatedly before that frame arrives
	/// cuts once, not several times.
	pub fn cut(&self) -> Result<(), MoqError> {
		let mut guard = self.inner.lock().unwrap();
		let producer = guard.as_mut().ok_or(MoqError::Closed)?;
		// A keyframe is what a cut is on the wire: the importer closes the open
		// group and starts a new one at it.
		producer.encoder.keyframe();
		Ok(())
	}

	/// Retune the live encoder to `bitrate` bits per second, taking effect from
	/// roughly the next frame. No keyframe is forced, so this is cheap enough to
	/// drive from a congestion controller.
	///
	/// The configured bitrate is a ceiling on some backends (openh264 rejects a
	/// raise above the rate it opened at), so set
	/// [`bitrate`](MoqVideoEncoderOutput::bitrate) to the highest you will ask for
	/// and adapt downwards from there.
	///
	/// Errors if this backend cannot retune while running. That is not fatal: the
	/// encoder keeps running at its current rate, so stop adapting rather than
	/// stop publishing.
	pub fn set_bitrate(&self, bitrate: u64) -> Result<(), MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let mut guard = self.inner.lock().unwrap();
		let producer = guard.as_mut().ok_or(MoqError::Closed)?;
		Ok(block_on(producer.encoder.set_bitrate(bitrate))?)
	}

	/// Flush any frames the codec is still holding and finalize the track.
	pub fn finish(&self) -> Result<(), MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let producer = self.inner.lock().unwrap().take().ok_or(MoqError::Closed)?;
		producer.finish()
	}
}

#[uniffi::export]
impl MoqBroadcastProducer {
	/// Open a video track on this broadcast, encoding the raw frames written to
	/// it.
	///
	/// The encoder opens here, so an unsupported codec, resolution, or backend
	/// fails now rather than on the first frame. The track is named after the
	/// codec (`.avc3` / `.hev1`) and its catalog rendition is published
	/// immediately, read out of the encoder rather than guessed, so a subscriber
	/// can find the track before a frame is written to it.
	pub fn encode_video(
		&self,
		input: MoqVideoEncoderInput,
		output: MoqVideoEncoderOutput,
	) -> Result<Arc<MoqVideoProducer>, MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();

		let mut config = moq_video::encode::Config::new(input.width, input.height, input.framerate);
		config.codec = output.codec.into();
		config.kind = output.kind.into();
		config.bitrate = output.bitrate;
		if let Some(gop) = output.gop {
			config.gop = gop;
		}

		// Both before the track exists: a config this machine can't encode should fail
		// without leaving a track advertised that will never carry frames. The probe
		// closes its encoder before this one opens, so only one codec session is live.
		let rendition = block_on(config.probe())?;
		let encoder = block_on(moq_video::encode::Sink::open(&config))?;
		let producer = self.with_state(|state| {
			Ok(moq_video::encode::Producer::new(
				state.broadcast.clone(),
				state.catalog.clone(),
				rendition,
			)?)
		})?;

		Ok(Arc::new(MoqVideoProducer {
			inner: std::sync::Mutex::new(Some(VideoProducer {
				encoder,
				producer,
				format: input.format,
				size: config.size(),
			})),
		}))
	}
}

/// How a subscriber wants decoded video delivered.
///
/// Frames always arrive as tightly-packed I420 (Y, then U, then V, no row
/// padding), so there is no pixel format to choose: a decoder's native output is
/// flattened to CPU I420 at delivery, since the FFI boundary can't hand back a
/// GPU surface.
#[derive(Clone, uniffi::Record)]
pub struct MoqVideoDecoderOutput {
	/// Ask the decoder to emit frames at this size instead of the stream's
	/// native one. Best effort: a backend with a built-in scaler honors it for
	/// free, others ignore it, so read each frame's own dimensions rather than
	/// assuming this took. Both dimensions must be even.
	#[uniffi(default = None)]
	pub resize: Option<crate::media::MoqDimensions>,
	/// Upper bound on buffering before skipping a stalled group, in
	/// milliseconds. Same knob as
	/// [`MoqAudioDecoderOutput::max_age_ms`](crate::audio::MoqAudioDecoderOutput::max_age_ms).
	/// `None` keeps the moq-mux default of zero (skip aggressively).
	#[uniffi(default = None)]
	pub max_age_ms: Option<u64>,
}

/// One decoded video frame: packed I420 plus the size it actually decoded to.
///
/// Unlike [`MoqVideoFrame`] on the publish side, this carries dimensions: there
/// they are fixed by the encoder config, here they are whatever the stream
/// turned out to be, and `resize` is only best effort.
#[derive(uniffi::Record)]
pub struct MoqVideoDecodedFrame {
	/// Presentation timestamp, in microseconds.
	pub timestamp_us: u64,
	/// Frame width in pixels.
	pub width: u32,
	/// Frame height in pixels.
	pub height: u32,
	/// Tightly-packed I420: Y, then U, then V.
	pub data: Vec<u8>,
}

struct VideoConsumerInner {
	consumer: moq_video::decode::Consumer,
}

impl VideoConsumerInner {
	async fn next(&mut self) -> Result<Option<MoqVideoDecodedFrame>, MoqError> {
		let Some(frame) = self.consumer.read().await? else {
			return Ok(None);
		};

		let size = frame.size();
		// The surface may still live on the GPU, so flatten before crossing the
		// boundary: uniffi has no handle type to hand back a texture with.
		let data = frame
			.surface
			.into_i420()
			.map_err(|err| MoqError::Codec(err.to_string()))?;

		Ok(Some(MoqVideoDecodedFrame {
			timestamp_us: frame.timestamp.as_micros() as u64,
			width: size.width,
			height: size.height,
			data: data.to_vec(),
		}))
	}
}

/// Consumer for a video track decoded inside the bindings.
#[derive(uniffi::Object)]
pub struct MoqVideoConsumer {
	task: crate::ffi::Task<VideoConsumerInner>,
}

#[uniffi::export]
impl MoqVideoConsumer {
	/// The next decoded frame, or `None` once the track ends.
	pub async fn next(&self) -> Result<Option<MoqVideoDecodedFrame>, MoqError> {
		self.task.run(|mut state| async move { state.next().await }).await
	}

	/// Make current and future reads return `Cancelled`.
	///
	/// Terminal: the decoder session is released here, not when the handle is.
	pub fn cancel(&self) {
		self.task.cancel();
	}
}

/// Rebuild the catalog rendition the decoder needs from what the FFI catalog handed out.
///
/// The inverse of the conversion in [`crate::media::convert_catalog`]. An unrecognized codec name
/// is rejected here; a recognized one the native backends can't open is rejected when the decoder
/// opens, which is still before the first frame.
fn video_config(catalog_video: crate::media::MoqVideo) -> Result<hang::catalog::VideoConfig, MoqError> {
	let codec: hang::catalog::VideoCodec = catalog_video.codec.parse().map_err(|_| MoqError::Unsupported)?;
	// Parsing is total: an unrecognized name becomes `Unknown` rather than failing. Reject it here
	// so a typo in the catalog is an error at subscribe, not an opaque backend failure later.
	if matches!(codec, hang::catalog::VideoCodec::Unknown(_)) {
		return Err(MoqError::Unsupported);
	}

	let mut config = hang::catalog::VideoConfig::new(codec);
	config.label = catalog_video.label;
	config.description = catalog_video.description.map(Into::into);
	if let Some(coded) = catalog_video.coded {
		config.coded_width = Some(coded.width);
		config.coded_height = Some(coded.height);
	}
	if let Some(aspect) = catalog_video.display_aspect {
		config.display_aspect_width = Some(aspect.width);
		config.display_aspect_height = Some(aspect.height);
	}
	config.bitrate = catalog_video.bitrate;
	config.framerate = catalog_video.framerate;
	config.stalled = Some(catalog_video.stalled);
	config.container = catalog_video.container.into();
	Ok(config)
}

#[uniffi::export]
impl MoqBroadcastConsumer {
	/// Subscribe to a video track and decode it inside the bindings.
	///
	/// `catalog_video` comes from the catalog (see
	/// [`MoqCatalogConsumer::next`](crate::consumer::MoqCatalogConsumer::next)); the codec is read
	/// from it. Errors if no native backend handles that codec, rather than failing on the first
	/// frame.
	///
	/// A rendition whose [`broadcast`](crate::media::MoqVideo::broadcast) names another broadcast
	/// is subscribed there, so `name` is always read from the broadcast the catalog points at.
	pub async fn decode_video(
		&self,
		name: String,
		catalog_video: crate::media::MoqVideo,
		output: MoqVideoDecoderOutput,
	) -> Result<Arc<MoqVideoConsumer>, MoqError> {
		// Reject the codec before resolving: resolving reaches the origin, which can invoke a
		// dynamic handler and open an upstream subscription we would immediately drop.
		let reference = catalog_video.broadcast.clone();
		let cfg = video_config(catalog_video)?;
		let broadcast = self.resolve_inner(reference.as_deref()).await?;

		let mut config = moq_video::decode::Config::default();
		config.resize = output.resize.map(|size| moq_video::Size::new(size.width, size.height));
		config.max_age = output
			.max_age_ms
			.map(std::time::Duration::from_millis)
			.unwrap_or_default();

		let consumer = moq_video::decode::Consumer::new(&broadcast, &cfg, name, config).await?;

		Ok(Arc::new(MoqVideoConsumer {
			task: crate::ffi::Task::new(VideoConsumerInner { consumer }),
		}))
	}
}

#[cfg(test)]
mod decode_tests {
	use super::*;
	use crate::media::{MoqContainer, MoqDimensions, MoqVideo};

	fn catalog_video(codec: &str) -> MoqVideo {
		MoqVideo {
			label: None,
			broadcast: None,
			codec: codec.to_string(),
			description: None,
			coded: Some(MoqDimensions {
				width: 1280,
				height: 720,
			}),
			display_aspect: None,
			bitrate: None,
			stalled: false,
			framerate: Some(30.0),
			container: MoqContainer::Legacy,
		}
	}

	#[test]
	fn video_config_round_trips_the_catalog_fields() {
		let config = video_config(catalog_video("avc1.64001f")).unwrap();
		assert_eq!(config.coded_width, Some(1280));
		assert_eq!(config.coded_height, Some(720));
		assert_eq!(config.framerate, Some(30.0));
	}

	#[test]
	fn video_config_rejects_an_unknown_codec() {
		let error = video_config(catalog_video("nope")).unwrap_err();
		assert!(matches!(error, MoqError::Unsupported));
	}
}
