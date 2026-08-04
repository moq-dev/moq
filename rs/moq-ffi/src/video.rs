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
/// openh264 (software, everywhere). The Linux hardware codecs (NVENC, VAAPI)
/// are a libmoq-only build option, so Linux here is software-only.
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

/// An encoder paired with the track publishing its output.
struct VideoProducer {
	encoder: moq_video::encode::Encoder,
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
		let encoded = self.encoder.encode(&frame)?;
		self.producer.publish(&encoded)?;
		Ok(())
	}

	fn finish(self) -> Result<(), MoqError> {
		let Self {
			encoder, mut producer, ..
		} = self;
		// Drain the codec into the track before ending it, so the last frames land
		// in it rather than being dropped with the encoder.
		let drained = encoder.finish().and_then(|encoded| producer.publish(&encoded));
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
		Ok(producer.encoder.set_bitrate(bitrate)?)
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
	/// codec (`.avc3` / `.hev1`) and its catalog rendition appears once the first
	/// keyframe has been encoded, which is where the resolution and codec string
	/// come from.
	pub fn publish_video(
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

		// Open the encoder first: a config this machine can't encode should fail
		// without leaving a track advertised that will never carry frames.
		let encoder = moq_video::encode::Encoder::new(&config)?;
		let producer = self.with_state(|state| {
			Ok(moq_video::encode::Producer::new(
				state.broadcast.clone(),
				state.catalog.clone(),
				config.codec,
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
