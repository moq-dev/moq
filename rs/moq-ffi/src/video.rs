//! Raw-video publishing via [`moq_video`].
//!
//! Sibling to [`audio`](crate::audio)'s producer, and the video counterpart to
//! [`media::MoqMediaTrackProducer`](crate::media::MoqMediaTrackProducer): that one
//! takes already-encoded frames, this one takes raw pictures and runs the H.264
//! / H.265 encode inside the FFI boundary (VideoToolbox on macOS and iOS, Media
//! Foundation on Windows, openh264 as the software fallback; no ffmpeg).
//!
//! Pixel format, resolution, and framerate are fixed at publish time via
//! [`MoqVideoEncoderInput`], so each [`MoqVideoFrame`] carries only pixels and a
//! timestamp.

use std::sync::Arc;

use crate::bandwidth::{MoqAllocator, MoqReservation};
use crate::consumer::MoqBroadcastConsumer;
use crate::demand::MoqTrackDemand;
use crate::error::MoqError;
use crate::media::MoqTimestamp;

/// A CPU pixel layout: what [`MoqVideoProducer::write`] is fed, and what
/// [`MoqVideoDecodedFrame::pixels`] converts to.
///
/// An immutable object so adding a layout later does not break callers
/// switching over a closed enum.
#[derive(Debug, PartialEq, Eq, Hash, uniffi::Object)]
#[uniffi::export(Debug, Eq, Hash)]
pub struct MoqVideoPixelFormat(PixelFormat);

/// The layouts [`MoqVideoPixelFormat`] names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum PixelFormat {
	I420,
	Rgba,
}

#[uniffi::export]
impl MoqVideoPixelFormat {
	/// Tightly-packed planar I420: Y, then U, then V, no row padding
	/// (`width * height * 3 / 2` bytes).
	#[uniffi::constructor]
	pub fn i420() -> Arc<Self> {
		Arc::new(Self(PixelFormat::I420))
	}

	/// Tightly-packed RGBA, `width * height * 4` bytes, no row padding.
	#[uniffi::constructor]
	pub fn rgba() -> Arc<Self> {
		Arc::new(Self(PixelFormat::Rgba))
	}

	/// The layout's lowercase name, such as `i420`.
	pub fn name(&self) -> String {
		match self.0 {
			PixelFormat::I420 => "i420",
			PixelFormat::Rgba => "rgba",
		}
		.to_string()
	}
}

impl PixelFormat {
	/// Wrap one picture of `size` in this layout, refusing a buffer of any other length.
	fn surface(self, size: moq_video::Size, data: Vec<u8>) -> Result<moq_video::Surface, MoqError> {
		Ok(match self {
			Self::I420 => moq_video::Surface::I420(moq_video::I420::new(size, data)?),
			Self::Rgba => moq_video::Surface::rgba(&data, size)?,
		})
	}
}

/// Output video codec.
///
/// Not every codec has a backend on every machine: H.265 is hardware-only, so
/// publishing it fails where no hardware encoder is available.
///
/// An immutable object so adding a codec later does not break callers
/// switching over a closed enum.
#[derive(Debug, PartialEq, Eq, Hash, uniffi::Object)]
#[uniffi::export(Debug, Eq, Hash)]
pub struct MoqVideoCodec {
	inner: moq_video::encode::Codec,
}

#[uniffi::export]
impl MoqVideoCodec {
	/// H.264 / AVC, published as an `avc3` track.
	#[uniffi::constructor]
	pub fn h264() -> Arc<Self> {
		Arc::new(Self {
			inner: moq_video::encode::Codec::H264,
		})
	}

	/// H.265 / HEVC, published as a `hev1` track.
	#[uniffi::constructor]
	pub fn h265() -> Arc<Self> {
		Arc::new(Self {
			inner: moq_video::encode::Codec::H265,
		})
	}

	/// The codec's lowercase name, such as `h264`.
	pub fn name(&self) -> String {
		self.inner.as_str().to_string()
	}
}

/// Which encoder implementation to use.
///
/// These bindings compile VideoToolbox (macOS, iOS), Media Foundation (Windows),
/// openh264 (software, everywhere), and on Linux NVENC and VAAPI, which dlopen
/// their driver at runtime and drop out of `Auto` when it is absent.
#[derive(Clone, uniffi::Enum)]
pub enum MoqVideoEncoderKind {
	/// Prefer a platform hardware encoder, falling back to software.
	Auto,
	/// Hardware only; fails if none is available.
	Hardware,
	/// Software only (openh264, H.264 only).
	Software,
	/// A specific backend that moq-ffi compiles: `"videotoolbox"` (macOS, iOS),
	/// `"mediafoundation"` (Windows), `"nvenc"` / `"vaapi"` (Linux), or
	/// `"openh264"` (software, everywhere).
	/// Naming one this build lacks fails with a no-encoder error, so reach for
	/// this only when [`Auto`](Self::Auto) picks the wrong one.
	Named {
		/// The encoder backend name to open.
		name: String,
	},
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

/// An exact video frame rate in frames per second.
#[derive(Clone, Copy, uniffi::Record)]
pub struct MoqVideoFrameRate {
	/// Frames per second numerator; must be nonzero.
	pub numerator: u32,
	/// Frames per second denominator; must be nonzero.
	pub denominator: u32,
}

impl TryFrom<MoqVideoFrameRate> for moq_video::FrameRate {
	type Error = MoqError;

	fn try_from(rate: MoqVideoFrameRate) -> Result<Self, Self::Error> {
		Self::new(rate.numerator, rate.denominator).map_err(|err| MoqError::Video {
			reason: format!("invalid framerate: {err}"),
		})
	}
}

/// Raw frame layout the caller will pass to [`MoqVideoProducer::write`], plus
/// the resolution and rate the encoder is opened at. Every written frame must
/// match `width` x `height`; scale before writing if your source moves.
#[derive(uniffi::Record)]
pub struct MoqVideoEncoderInput {
	/// The raw pixel layout.
	pub format: Arc<MoqVideoPixelFormat>,
	/// Encoded width in pixels. Must be even (I420 chroma is subsampled 2x2).
	pub width: u32,
	/// Encoded height in pixels. Must be even.
	pub height: u32,
	/// Nominal frames per second, used for the codec time base, the default
	/// bitrate, and rounding the join to whole frames. Must be non-zero.
	pub framerate: MoqVideoFrameRate,
}

/// Codec-side configuration.
#[derive(uniffi::Record)]
pub struct MoqVideoEncoderOutput {
	/// The codec used to encode this rendition.
	pub codec: Arc<MoqVideoCodec>,
	/// Target bitrate in bits per second. `None` derives one from the resolution
	/// and framerate.
	#[uniffi(default = None)]
	pub bitrate: Option<u64>,
	/// The time between join points: a keyframe opens a group this often,
	/// rounded to whole frames. `None` keyframes every two seconds; zero is
	/// refused.
	#[uniffi(default = None)]
	pub join: Option<std::time::Duration>,
	/// Encoder implementation preference. Pass
	/// [`MoqVideoEncoderKind::Auto`] unless you need a specific backend.
	pub kind: MoqVideoEncoderKind,
}

/// Input, codec settings, and publication ownership for a video producer.
#[derive(uniffi::Record)]
pub struct MoqVideoProducerOptions {
	/// The raw pictures the caller writes.
	pub input: MoqVideoEncoderInput,
	/// The codec settings used to encode those pictures.
	pub output: MoqVideoEncoderOutput,
	/// Published track name; `None` derives one from the codec.
	#[uniffi(default = None)]
	pub track: Option<String>,
	/// Session bandwidth allocator; absent keeps the configured bitrate.
	#[uniffi(default = None)]
	pub bandwidth: Option<Arc<MoqAllocator>>,
	/// The next lower rendition of the same source, when this one is encoded beside it.
	///
	/// Makes this rendition a simulcast rung: when the bandwidth grant can no longer fund
	/// it, it stops encoding and advertises itself disabled, so the lower renditions keep
	/// their share, and it resumes once the grant recovers. It takes the track priority
	/// one below the lower rendition's, so the allocator fills the lower one first. Needs
	/// [`bandwidth`](Self::bandwidth), and a ceiling above the lower rendition's.
	#[uniffi(default = None)]
	pub lower: Option<Arc<MoqVideoProducer>>,
}

/// One raw video frame: pixels plus a presentation timestamp.
///
/// The pixel format and resolution are fixed by [`MoqVideoEncoderInput`] at
/// publish time, so a frame carries neither. `data` is exactly one picture in
/// that layout.
#[derive(uniffi::Record)]
pub struct MoqVideoFrame {
	/// Presentation timestamp.
	pub timestamp: MoqTimestamp,
	/// The pixels, in the configured layout.
	pub data: Vec<u8>,
}

/// One access unit emitted by a codec-only video encoder.
#[derive(uniffi::Record)]
pub struct MoqVideoPacket {
	/// Presentation timestamp.
	pub timestamp: MoqTimestamp,
	/// Encoded access-unit payload.
	pub data: Vec<u8>,
	/// Whether the access unit starts from a keyframe.
	pub keyframe: bool,
}

impl From<moq_video::encode::Encoded> for MoqVideoPacket {
	fn from(packet: moq_video::encode::Encoded) -> Self {
		Self {
			timestamp: packet.timestamp.into(),
			data: packet.payload.to_vec(),
			keyframe: packet.keyframe,
		}
	}
}

fn video_encoder_config(
	input: &MoqVideoEncoderInput,
	output: &MoqVideoEncoderOutput,
) -> Result<moq_video::encode::Config, MoqError> {
	let framerate = input.framerate.try_into()?;
	let mut config = moq_video::encode::Config::new(input.width, input.height, framerate);
	config.codec = output.codec.inner;
	config.kind = output.kind.clone().into();
	config.bitrate = output.bitrate.map(moq_net::bandwidth::Rate::from_bps);
	if let Some(join) = output.join {
		config.join = join;
	}
	Ok(config)
}

/// A codec-only video encoder, independent of any broadcast.
///
/// Submit pictures with [`encode`](Self::encode) and pop access units with
/// [`receive`](Self::receive) until it returns none. At the end of input,
/// [`flush`](Self::flush) and receive the rest before releasing it.
#[derive(uniffi::Object)]
pub struct MoqVideoEncoder {
	inner: std::sync::Mutex<moq_video::encode::Sink>,
	format: PixelFormat,
	size: moq_video::Size,
}

#[uniffi::export]
impl MoqVideoEncoder {
	/// Create a codec-only encoder from input and output settings.
	#[uniffi::constructor]
	pub fn new(input: MoqVideoEncoderInput, output: MoqVideoEncoderOutput) -> Result<Arc<Self>, MoqError> {
		let config = video_encoder_config(&input, &output)?;
		let encoder = block_on(moq_video::encode::Sink::open(&config))?;
		Ok(Arc::new(Self {
			inner: std::sync::Mutex::new(encoder),
			format: input.format.0,
			size: config.size(),
		}))
	}

	/// Submit one raw picture, whose access units `receive` returns.
	pub fn encode(&self, frame: MoqVideoFrame) -> Result<(), MoqError> {
		let surface = self.format.surface(self.size, frame.data)?;
		let frame = moq_video::Frame::new(surface, frame.timestamp.try_into()?);
		let mut guard = self.inner.lock().map_err(|_| panic_error())?;
		block_on(guard.encode(frame))?;
		Ok(())
	}

	/// Pop the oldest ready access unit, or none while the codec holds the rest; call until none.
	pub fn receive(&self) -> Result<Option<MoqVideoPacket>, MoqError> {
		let mut guard = self.inner.lock().map_err(|_| panic_error())?;
		Ok(guard.receive()?.map(MoqVideoPacket::from))
	}

	/// Force the next encoded picture to start a new group.
	pub fn cut(&self) -> Result<(), MoqError> {
		let mut guard = self.inner.lock().map_err(|_| panic_error())?;
		block_on(guard.cut())?;
		Ok(())
	}

	/// Retune the live encoder in bits per second.
	pub fn set_bitrate(&self, bitrate: u64) -> Result<(), MoqError> {
		let mut guard = self.inner.lock().map_err(|_| panic_error())?;
		block_on(guard.set_bitrate(moq_net::bandwidth::Rate::from_bps(bitrate)))?;
		Ok(())
	}

	/// Mark the end of input, so `receive` returns every access unit the codec still
	/// holds. The encoder stays open: the next picture starts new input.
	pub fn flush(&self) -> Result<(), MoqError> {
		let mut guard = self.inner.lock().map_err(|_| panic_error())?;
		block_on(guard.flush())?;
		Ok(())
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

fn panic_error() -> MoqError {
	crate::session::map_closed_error(moq_tokio::Error::Panic)
}

/// Producer for a raw-video track.
///
/// Constructed from a broadcast's media catalog. Each [`write`](Self::write) accepts a
/// [`MoqVideoFrame`] whose `data` is in the pixel format declared by the [`MoqVideoEncoderInput`]
/// passed at publish time.
///
/// Released before its finish, it ends its track cleanly while nobody wants it; a reader that still
/// does is served by the broadcast's next request for the track, or reads `Dropped` (seen elsewhere
/// as `Internal`) with no request handler, or when this producer served such a request and no
/// reader joined since. Still open when its broadcast closes, it ends it with `Unroutable`, which a
/// moq-transport reader sees as `Internal`: its wire has no such code.
#[derive(uniffi::Object)]
pub struct MoqVideoProducer {
	/// `None` once finished or aborted.
	///
	/// The encoder lives in a [`Sink`](moq_video::encode::Sink) inside the producer, which owns
	/// the thread it runs on: uniffi hands this object to whichever thread the caller writes
	/// from, and an encoder built on one thread and dropped on another unbalances the
	/// per-thread COM apartment the Windows backend opens.
	inner: std::sync::Mutex<Option<moq_video::encode::Producer>>,
	format: PixelFormat,
	/// The encoded resolution, from the publish config. Frames carry only pixels, so this is
	/// what says how to read them.
	size: moq_video::Size,
	/// The allocator the producer reserves against, `None` when published without one. Held
	/// so the reservation's registry outlives extra bandwidth handles.
	bandwidth: Option<Arc<MoqAllocator>>,
	children: crate::producer::Children,
}

impl MoqVideoProducer {
	/// Run `f` on the open producer, failing with `Closed` once it finished.
	fn with<R>(&self, f: impl FnOnce(&mut moq_video::encode::Producer) -> Result<R, MoqError>) -> Result<R, MoqError> {
		let mut guard = self.inner.lock().map_err(|_| panic_error())?;
		let producer = guard.as_mut().ok_or(MoqError::Closed)?;
		f(producer)
	}

	/// The bitrate the encoder produces at, or 0 while disabled.
	#[cfg(test)]
	pub(crate) fn applied_bitrate(&self) -> u64 {
		self.with(|producer| Ok(producer.bitrate().map_or(0, moq_net::bandwidth::Rate::as_bps)))
			.unwrap()
	}
}

// Closed with its broadcast before its finish, the track ends with the close's cause.
impl crate::producer::Child for MoqVideoProducer {
	fn abort(&self, err: moq_net::Error) {
		let _guard = crate::ffi::runtime().enter();
		let producer = self
			.inner
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner)
			.take();
		if let Some(producer) = producer {
			producer.abort(err);
		}
	}
}

impl Drop for MoqVideoProducer {
	fn drop(&mut self) {
		let _guard = crate::ffi::runtime().enter();
		drop(
			self.inner
				.get_mut()
				.unwrap_or_else(std::sync::PoisonError::into_inner)
				.take(),
		);
	}
}

#[uniffi::export]
impl MoqVideoProducer {
	/// A watch-only handle to whether this video track has subscribers.
	pub fn demand(&self) -> Result<Arc<MoqTrackDemand>, MoqError> {
		Ok(MoqTrackDemand::new(self.with(|producer| Ok(producer.demand()))?))
	}

	/// Encode and publish one raw frame.
	///
	/// A backend that pipelines publishes an earlier frame's output here, so a
	/// call that emits nothing on the wire is normal rather than an error. While a
	/// [`lower`](MoqVideoProducerOptions::lower) rung is disabled for lack of
	/// bandwidth, the frame is dropped without encoding.
	///
	/// Published against a [`MoqAllocator`], the encoder follows the bandwidth grant here,
	/// before encoding, so it retunes between frames rather than mid-encode.
	pub fn write(&self, frame: MoqVideoFrame) -> Result<(), MoqError> {
		let _guard = crate::ffi::runtime().enter();
		// A buffer that isn't one picture at the configured size is rejected here,
		// by the surface constructors, rather than reinterpreted.
		let surface = self.format.surface(self.size, frame.data)?;
		let frame = moq_video::Frame::new(surface, frame.timestamp.try_into()?);
		self.with(|producer| Ok(block_on(producer.write(frame))?))
	}

	/// Cut a new group at the next written frame.
	///
	/// Optional. The encoder already keyframes every
	/// [`join`](MoqVideoEncoderOutput::join), and each of those cuts a
	/// group, so a subscriber can always join without you calling this. Reach for
	/// it only to place the boundaries yourself: aligning groups with something
	/// the encoder cannot see, such as a scene change, a source switch, or
	/// resuming after an idle gap.
	///
	/// The next frame is encoded as a keyframe, which closes the open group and
	/// starts a new one at it. Calling this repeatedly before that frame arrives
	/// cuts once, not several times.
	///
	/// Fails when the selected encoder cannot force a keyframe (a V4L2 driver
	/// without the control): nothing is queued, and groups keep falling at the
	/// configured interval.
	pub fn cut(&self) -> Result<(), MoqError> {
		self.with(|producer| Ok(block_on(producer.cut())?))
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
	/// When this producer was published against a [`MoqAllocator`], the reservation
	/// and the grant's ceiling move with it, so a later grant cannot retune above this
	/// value.
	///
	/// Errors if the backend refuses the rate: some cannot retune while running, and
	/// openh264 cannot raise past its opening rate. That is not fatal: the encoder
	/// keeps running at its current rate, so stop adapting rather than stop
	/// publishing. An error saying the codec session ended means the encoder stopped.
	pub fn set_bitrate(&self, bitrate: u64) -> Result<(), MoqError> {
		let _guard = crate::ffi::runtime().enter();
		self.with(|producer| {
			Ok(block_on(
				producer.set_bitrate(moq_net::bandwidth::Rate::from_bps(bitrate)),
			)?)
		})
	}

	/// This encoder's bandwidth reservation, if it was published against a
	/// [`MoqAllocator`]. The producer owns the claim, not the handle: dropping the
	/// handle keeps it, and [`finish`](Self::finish) releases it however many handles
	/// remain, after which they read no rate. A frame that changes the decoder
	/// identity publishes under a new track name and reserves against that track
	/// instead, releasing the old claim, so read this again after [`write`](Self::write).
	pub fn reservation(&self) -> Option<Arc<MoqReservation>> {
		self.bandwidth.as_ref()?;
		let guard = self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
		Some(Arc::new(MoqReservation::borrowed(guard.as_ref()?.reservation())))
	}

	/// Flush any frames the codec is still holding and finalize the track.
	///
	/// The broadcast keeps the finished track until it ends, so its finish still holds it.
	pub fn finish(&self) -> Result<(), MoqError> {
		let _guard = crate::ffi::runtime().enter();
		// Held through the drain, so a racing broadcast close waits it out instead of
		// missing a track that still publishes.
		let mut inner = self.inner.lock().map_err(|_| panic_error())?;
		let producer = inner.take().ok_or(MoqError::Closed)?;
		// A failed drain already ended the track with an error, so readers see the failure
		// rather than a truncated stream that looks complete.
		self.children.keep(block_on(producer.finish())?);
		Ok(())
	}
}

#[uniffi::export]
impl MoqVideoProducer {
	/// Open a video track on the catalog's broadcast, encoding the raw frames written to
	/// it.
	///
	/// The encoder opens here, so an unsupported codec, resolution, or backend
	/// fails now rather than on the first frame. [`MoqVideoProducerOptions::track`]
	/// chooses the track name; `None` derives one from the codec. The catalog
	/// rendition is published immediately so a subscriber can discover the track
	/// before a frame is written to it.
	///
	/// Set `options.bandwidth` to reserve this track's configured bitrate against the
	/// session's allocator and follow the grant between frames with the same policy the
	/// Rust capture encoder uses. [`set_bitrate`](MoqVideoProducer::set_bitrate) is
	/// the manual ceiling: it retunes the encoder and moves the reservation. Set
	/// `options.lower` too for each rendition above the lowest of one source.
	#[uniffi::constructor]
	pub fn new(
		catalog: &crate::media::MoqMediaCatalogProducer,
		options: MoqVideoProducerOptions,
	) -> Result<Arc<MoqVideoProducer>, MoqError> {
		Self::open(catalog, None, options)
	}

	/// Open a video track on the track a subscriber's `request` asks for, as [`new`](Self::new)
	/// does on a track of its own.
	///
	/// This is how an on-demand publisher serves a track asked for again after its producer was
	/// released while read: serve it with the options it was first published with. Options that
	/// change the track info its readers hold end the track with `MalformedTrack` and fail here;
	/// nothing checks the codec settings, which its readers decode with, so keep those too. The
	/// request names the track, so [`MoqVideoProducerOptions::track`] must be `None`, and it
	/// must belong to the catalog's broadcast. A config this machine can't encode rejects the
	/// request with `NotFound`.
	#[uniffi::constructor]
	pub fn requested(
		catalog: &crate::media::MoqMediaCatalogProducer,
		request: Arc<crate::producer::MoqTrackRequest>,
		options: MoqVideoProducerOptions,
	) -> Result<Arc<MoqVideoProducer>, MoqError> {
		Self::open(catalog, Some(request), options)
	}
}

impl MoqVideoProducer {
	fn open(
		catalog: &crate::media::MoqMediaCatalogProducer,
		request: Option<Arc<crate::producer::MoqTrackRequest>>,
		options: MoqVideoProducerOptions,
	) -> Result<Arc<MoqVideoProducer>, MoqError> {
		let _guard = crate::ffi::runtime().enter();
		let broadcast = catalog.broadcast()?;

		let MoqVideoProducerOptions {
			input,
			output,
			track,
			bandwidth,
			lower,
		} = options;
		let config = video_encoder_config(&input, &output)?;
		let size = config.size();
		let mut options = moq_video::encode::Options::new(config);
		options.track = track;
		if let Some(bandwidth) = &bandwidth {
			options.bandwidth = bandwidth.inner().clone();
		}
		if let Some(lower) = lower {
			if bandwidth.is_none() {
				return Err(MoqError::Video {
					reason: "a lower rendition needs a bandwidth allocator to follow".into(),
				});
			}
			options.lower = Some(lower.with(|lower| Ok(lower.rung()))?);
		}

		// Out of the broadcast's lock, which the probe and the encoder open would hold for as
		// long as the codec takes to start.
		let (target, catalog, children) = broadcast
			.with_state(|state| Ok((state.broadcast.clone(), state.catalog.clone(), state.children.clone())))?;
		let producer = match request {
			None => block_on(moq_video::encode::Producer::new(&target, catalog, options))?,
			Some(request) => request.answer_in(&children, |request| {
				Ok(block_on(moq_video::encode::Producer::requested(
					request, catalog, options,
				))?)
			})?,
		};
		children.adopt(Arc::new(MoqVideoProducer {
			inner: std::sync::Mutex::new(Some(producer)),
			format: input.format.0,
			size,
			bandwidth,
			children: children.clone(),
		}))
	}
}

/// How a subscriber wants decoded video delivered.
#[derive(Clone, Default, uniffi::Record)]
pub struct MoqVideoDecoderOutput {
	/// Ask the decoder to emit frames at this size instead of the stream's
	/// native one. Best effort: only NVDEC has a built-in scaler and honors it for
	/// free; VideoToolbox, Media Foundation, MediaCodec, VAAPI, V4L2, and openh264
	/// ignore it and decode at the stream's native size. Read each frame's own
	/// dimensions rather than assuming this took. Both dimensions must be even.
	#[uniffi(default = None)]
	pub resize: Option<crate::media::MoqDimensions>,
	/// How far behind the live edge to play: where a cold join starts and when a
	/// stalled group is skipped. Same knob as
	/// [`MoqAudioDecoderOutput::delay`](crate::audio::MoqAudioDecoderOutput::delay).
	/// `None` keeps the moq-mux default of zero for both (start at the newest group,
	/// skip aggressively).
	#[uniffi(default = None)]
	pub delay: Option<crate::consumer::MoqDelay>,
	/// Keep each frame in the surface its decoder produced, for
	/// [`MoqVideoDecodedFrame::surface`]. `false`, the default, downloads every
	/// frame to CPU memory as it is decoded, so
	/// [`MoqVideoDecodedFrame::pixels`] never meets a surface it cannot read.
	///
	/// Only a platform with a [`MoqVideoSurface`] variant accepts it (macOS and iOS
	/// today); [`MoqVideoConsumer::subscribe`] fails with
	/// [`MoqError::Unsupported`] elsewhere.
	#[uniffi(default = false)]
	pub surface: bool,
	/// Where to start on a track that already holds groups, or `None` for the oldest. Same
	/// knob as [`MoqAudioDecoderOutput::start`](crate::audio::MoqAudioDecoderOutput::start).
	#[uniffi(default = None)]
	pub start: Option<crate::media::MoqDecodeStart>,
}

/// A borrowed platform handle to a decoded frame's surface, from
/// [`MoqVideoDecodedFrame::surface`].
///
/// The handle is valid while the frame it came from is alive and no longer.
/// Keep the frame until every GPU command reading the surface has completed:
/// the surface belongs to the decoder's pool, and releasing the frame is what
/// lets the decoder reuse it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum MoqVideoSurface {
	/// An Apple `CVPixelBufferRef` from VideoToolbox, IOSurface-backed NV12.
	PixelBuffer {
		/// The borrowed CVPixelBuffer address, valid while the frame is held.
		pointer: u64,
	},
	/// A platform surface this version of the bindings cannot name yet.
	Unknown,
}

/// One decoded video frame, owning the surface it was decoded into.
///
/// Dropping (or destroying) the frame releases that surface to the decoder's
/// pool, so hold only the frames you are still using: a consumer that keeps
/// many stalls decoding once the pool runs dry. The frame outlives its
/// [`MoqVideoConsumer`], including after [`cancel`](MoqVideoConsumer::cancel),
/// and may be used and released from any thread.
#[derive(uniffi::Object)]
pub struct MoqVideoDecodedFrame {
	frame: moq_video::Frame,
}

#[uniffi::export]
impl MoqVideoDecodedFrame {
	/// Presentation timestamp.
	pub fn timestamp(&self) -> MoqTimestamp {
		self.frame.timestamp.into()
	}

	/// Frame width in pixels: what the stream decoded to, since `resize` is only
	/// a hint.
	pub fn width(&self) -> u32 {
		self.frame.size().width
	}

	/// Frame height in pixels.
	pub fn height(&self) -> u32 {
		self.frame.size().height
	}

	/// The pixels, converted to `format` on each call: I420 is Y, then U, then V
	/// (`width * height * 3 / 2` bytes); RGBA is `width * height * 4` bytes.
	/// Neither has row padding.
	///
	/// A retained surface is downloaded first.
	pub fn pixels(&self, format: Arc<MoqVideoPixelFormat>) -> Result<Vec<u8>, MoqError> {
		let surface = &self.frame.surface;
		match format.0 {
			PixelFormat::I420 => surface.to_i420().map(|i420| i420.into_owned().into_data()),
			PixelFormat::Rgba => surface
				.to_rgba(&moq_video::convert::Config::default())
				.map(|rgba| rgba.into_data()),
		}
		.map_err(|err| MoqError::Codec {
			reason: err.to_string(),
		})
	}

	/// A borrowed handle to the decoder's surface, or `None` when the frame is
	/// in CPU memory.
	///
	/// Only a [`surface`](MoqVideoDecoderOutput::surface) decode produces one.
	pub fn surface(&self) -> Option<MoqVideoSurface> {
		match &self.frame.surface {
			moq_video::Surface::I420(_) => None,
			#[cfg(any(target_os = "macos", target_os = "ios"))]
			moq_video::Surface::PixelBuffer(pixels) => Some(MoqVideoSurface::PixelBuffer {
				pointer: std::ptr::from_ref(pixels.buffer()).addr() as u64,
			}),
			#[allow(unreachable_patterns, reason = "a platform whose every surface has a variant")]
			_ => Some(MoqVideoSurface::Unknown),
		}
	}
}

/// One item of a [`MoqVideoConsumer`] read, in delivery order: a decoded picture, or a
/// break between them.
///
/// Mirrors `moq_video::decode::Read`, the same shape as
/// [`MoqAudioRead`](crate::audio::MoqAudioRead), so a player handles loss and ends alike
/// for every media type.
#[derive(uniffi::Enum)]
pub enum MoqVideoRead {
	/// A decoded picture.
	Frame {
		/// The decoded frame, holding a decoder buffer until it is released.
		frame: Arc<MoqVideoDecodedFrame>,
	},
	/// Media after what was already read was lost: a group was skipped or never arrived.
	/// Every picture from before the loss precedes it, and what follows does not continue
	/// what came before.
	Gap {
		/// The furthest presentation point already read.
		timestamp: MoqTimestamp,
		/// An upper bound on how long until the next picture resumes.
		duration: std::time::Duration,
	},
	/// The publisher stopped its encoder: it paused, switched source, or finished.
	///
	/// The stopped encoder's last pictures precede it; what follows comes from a fresh
	/// encoder.
	End {
		/// Where the stopped encoder's media stops.
		end: MoqTimestamp,
	},
}

impl From<moq_video::decode::Read> for MoqVideoRead {
	fn from(read: moq_video::decode::Read) -> Self {
		match read {
			moq_video::decode::Read::Frame(frame) => Self::Frame {
				frame: Arc::new(MoqVideoDecodedFrame { frame }),
			},
			moq_video::decode::Read::Gap { timestamp, duration } => Self::Gap {
				timestamp: timestamp.into(),
				duration: crate::ffi::gap(duration),
			},
			moq_video::decode::Read::End { end } => Self::End { end: end.into() },
		}
	}
}

struct VideoConsumerInner {
	consumer: moq_video::decode::Consumer,
}

impl VideoConsumerInner {
	async fn next(&mut self) -> Result<Option<MoqVideoRead>, MoqError> {
		Ok(self.consumer.read().await?.map(MoqVideoRead::from))
	}
}

/// Consumer for a video track decoded inside the bindings.
#[derive(uniffi::Object)]
pub struct MoqVideoConsumer {
	task: crate::ffi::Task<VideoConsumerInner>,
}

#[uniffi::export]
impl MoqVideoConsumer {
	/// The next decoded frame, gap, or encoder end, or `None` once the track ends.
	pub async fn next(&self) -> Result<Option<MoqVideoRead>, MoqError> {
		self.task.run(|mut state| async move { state.next().await }).await
	}

	/// Make current and future reads return `Cancelled`.
	///
	/// Terminal: the decoder session is released here, not when the handle is.
	/// Frames already returned stay valid until they are released.
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
	if !catalog_video.enabled {
		return Err(MoqError::NotFound);
	}
	let codec = catalog_video
		.codec
		.parse::<hang::catalog::VideoCodec>()
		.map_err(|err| MoqError::UnsupportedConfig {
			reason: format!("{err}"),
		})?;

	if matches!(codec, hang::catalog::VideoCodec::Unknown(_)) {
		return Err(MoqError::UnsupportedConfig {
			reason: format!("unknown video codec: {codec}"),
		});
	}

	let mut config = hang::catalog::VideoConfig::new(codec);
	config.label = catalog_video.label;
	config.track = catalog_video.track;
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
	config.preference = catalog_video.preference;
	config.framerate = catalog_video.framerate;
	config.container = catalog_video.container.try_into()?;
	Ok(config)
}

/// Whether [`MoqVideoSurface`] has a variant on this platform, so a frame can retain its surface.
const HAS_SURFACE: bool = cfg!(any(target_os = "macos", target_os = "ios"));

/// Where the decoder puts each picture. A caller that did not ask for the surface reads CPU
/// pixels, so let a backend that can decode straight to system memory do that rather than hand
/// out a surface to download later. A surface the platform has no variant for is refused, since
/// the caller could neither view it through `surface()` nor always read it through `pixels()`.
fn decoder_output(surface: bool, has_surface: bool) -> Result<moq_video::Output, MoqError> {
	match (surface, has_surface) {
		(false, _) => Ok(moq_video::Output::Cpu),
		(true, true) => Ok(moq_video::Output::Native),
		(true, false) => Err(MoqError::Unsupported),
	}
}

#[uniffi::export]
impl MoqVideoConsumer {
	/// Subscribe to a video track and decode it inside the bindings.
	///
	/// `catalog_video` comes from the catalog (see
	/// [`MoqMediaCatalogConsumer::next`](crate::media::MoqMediaCatalogConsumer::next)); the codec
	/// is read from it. A codec or description this build cannot decode, or a stream every
	/// decoder's device refuses, returns [`MoqError::UnsupportedConfig`] before decoding starts.
	/// Also fails with [`MoqError::Unsupported`] when [`surface`](MoqVideoDecoderOutput::surface)
	/// is set on a platform with no surface to expose.
	///
	/// A [disabled](crate::media::MoqVideo::enabled) rendition returns [`MoqError::NotFound`]: no
	/// frames are coming.
	///
	/// `id` is the rendition's catalog key. Its [`track`](crate::media::MoqVideo::track), or `id`
	/// when unset, is subscribed in the broadcast its
	/// [`broadcast`](crate::media::MoqVideo::broadcast) reference names.
	#[uniffi::constructor]
	pub async fn subscribe(
		broadcast: &MoqBroadcastConsumer,
		id: String,
		catalog_video: crate::media::MoqVideo,
		output: MoqVideoDecoderOutput,
	) -> Result<Arc<MoqVideoConsumer>, MoqError> {
		// Reject the codec and output before resolving: resolving reaches the origin, which can
		// invoke a dynamic handler and open an upstream subscription we would immediately drop.
		let reference = catalog_video.broadcast.clone();
		let identity = catalog_video.identity.clone();
		let cfg = video_config(catalog_video)?;
		let mut options = moq_video::decode::Options::default();
		options.decoder.output = decoder_output(output.surface, HAS_SURFACE)?;
		let resolved = broadcast
			.resolve_inner(reference.as_deref(), identity.as_deref())
			.await?;

		options.decoder.scale_hint = output.resize.map(|size| moq_video::Size::new(size.width, size.height));
		options.delay = output.delay.map(Into::into).unwrap_or_default();
		options.start = match output.start.unwrap_or_default() {
			crate::media::MoqDecodeStart::Oldest => moq_video::decode::Start::Oldest,
			crate::media::MoqDecodeStart::Latest => moq_video::decode::Start::Latest,
		};

		let consumer = broadcast
			.driver
			.watch(async { Ok(moq_video::decode::Consumer::new(&resolved, &cfg, id, options).await?) })
			.await?;

		Ok(Arc::new(MoqVideoConsumer {
			task: crate::ffi::Task::new(VideoConsumerInner { consumer }).supervised(broadcast.driver.clone()),
		}))
	}
}

#[cfg(test)]
mod decode_tests {
	use super::*;
	use crate::media::{MoqContainer, MoqDimensions, MoqVideo};

	/// Codecs and pixel layouts compare and hash by value, and their names are distinct.
	#[test]
	fn selectors_name_compare_and_hash_by_value() {
		use std::collections::HashSet;

		assert_eq!(MoqVideoCodec::h264().name(), "h264");
		assert_eq!(MoqVideoCodec::h265().name(), "h265");
		assert_eq!(HashSet::from([MoqVideoCodec::h264(), MoqVideoCodec::h264()]).len(), 1);
		assert_ne!(MoqVideoCodec::h264(), MoqVideoCodec::h265());

		assert_eq!(MoqVideoPixelFormat::i420().name(), "i420");
		assert_eq!(MoqVideoPixelFormat::rgba().name(), "rgba");
		assert_eq!(
			HashSet::from([MoqVideoPixelFormat::rgba(), MoqVideoPixelFormat::rgba()]).len(),
			1
		);
		assert_ne!(MoqVideoPixelFormat::i420(), MoqVideoPixelFormat::rgba());
	}

	/// A catalog rendition whose container the bindings cannot name is refused, not decoded
	/// as some other container.
	#[test]
	fn unknown_container_is_unsupported() {
		let mut video = catalog_video("avc1.64001f");
		video.container = MoqContainer::Unknown;
		assert!(matches!(video_config(video), Err(MoqError::UnsupportedConfig { .. })));
	}

	fn catalog_video(codec: &str) -> MoqVideo {
		MoqVideo {
			preference: 0,
			label: None,
			broadcast: None,
			identity: None,
			track: None,
			codec: codec.to_string(),
			description: None,
			coded: Some(MoqDimensions {
				width: 1280,
				height: 720,
			}),
			display_aspect: None,
			bitrate: None,
			enabled: true,
			framerate: Some(30.0),
			container: MoqContainer::Legacy,
		}
	}

	#[test]
	fn video_config_refuses_disabled_renditions() {
		let mut config = catalog_video("avc1.64001f");
		config.enabled = false;
		config.preference = -1;
		assert!(matches!(video_config(config.clone()), Err(MoqError::NotFound)));
		config.enabled = true;
		assert_eq!(video_config(config).unwrap().preference, -1);
	}

	#[test]
	fn surface_is_refused_where_no_variant_exists() {
		assert!(matches!(decoder_output(true, false), Err(MoqError::Unsupported)));
		assert_eq!(decoder_output(true, true).unwrap(), moq_video::Output::Native);
		// The CPU path never depends on a surface variant.
		assert_eq!(decoder_output(false, false).unwrap(), moq_video::Output::Cpu);
		assert_eq!(decoder_output(false, true).unwrap(), moq_video::Output::Cpu);
	}

	#[test]
	fn video_config_rejects_an_unknown_codec() {
		let error = video_config(catalog_video("nope")).unwrap_err();
		assert!(matches!(error, MoqError::UnsupportedConfig { .. }));
	}
}

#[cfg(test)]
mod finish_tests {
	use super::*;

	#[test]
	fn encoder_config_preserves_exact_rates_and_rejects_invalid_ratios() {
		let output = MoqVideoEncoderOutput {
			codec: MoqVideoCodec::h264(),
			bitrate: None,
			join: None,
			kind: MoqVideoEncoderKind::Software,
		};
		for (numerator, denominator) in [(30_000, 1001), (60_000, 2002), (24_000, 1001)] {
			let config = video_encoder_config(
				&MoqVideoEncoderInput {
					format: MoqVideoPixelFormat::i420(),
					width: 64,
					height: 48,
					framerate: MoqVideoFrameRate { numerator, denominator },
				},
				&output,
			)
			.unwrap();
			assert_eq!(
				config.framerate,
				moq_video::FrameRate::new(numerator, denominator).unwrap()
			);
			assert_eq!(config.framerate.denominator(), 1001);
		}
		for (numerator, denominator) in [(0, 1), (30_000, 0), (1_000_001, 1)] {
			let error = video_encoder_config(
				&MoqVideoEncoderInput {
					format: MoqVideoPixelFormat::i420(),
					width: 64,
					height: 48,
					framerate: MoqVideoFrameRate { numerator, denominator },
				},
				&output,
			)
			.unwrap_err();
			assert!(matches!(error, MoqError::Video { .. }));
		}
	}

	/// The codec-only encoder every wrapper exposes (`moq.video.Encoder` and
	/// friends): a keyframe opens the stream, `cut` forces the next one, and
	/// `flush` readies the tail without closing it.
	#[test]
	fn encoder_emits_keyframes_where_asked() {
		let (width, height) = (64u32, 48u32);
		let encoder = MoqVideoEncoder::new(
			MoqVideoEncoderInput {
				format: MoqVideoPixelFormat::i420(),
				width,
				height,
				framerate: crate::video::MoqVideoFrameRate {
					numerator: 30_000,
					denominator: 1001,
				},
			},
			MoqVideoEncoderOutput {
				codec: MoqVideoCodec::h264(),
				bitrate: Some(500_000),
				join: None,
				kind: MoqVideoEncoderKind::Software,
			},
		)
		.unwrap();
		// NTSC frame times, which no microsecond count holds exactly.
		let ntsc = |i: u64| MoqTimestamp {
			value: i * 1001,
			scale: Some(30_000),
		};
		let picture = |i: u64| MoqVideoFrame {
			timestamp: ntsc(i),
			data: vec![(i * 40) as u8; (width * height * 3 / 2) as usize],
		};

		let mut packets = Vec::new();
		let receive = |packets: &mut Vec<MoqVideoPacket>| {
			while let Some(packet) = encoder.receive().unwrap() {
				packets.push(packet);
			}
		};
		for i in 0..3 {
			encoder.encode(picture(i)).unwrap();
			receive(&mut packets);
		}
		encoder.flush().unwrap();
		receive(&mut packets);
		encoder.set_bitrate(250_000).unwrap();
		encoder.cut().unwrap();
		encoder.encode(picture(3)).unwrap();
		encoder.flush().unwrap();
		receive(&mut packets);

		let keyframes: Vec<MoqTimestamp> = packets.iter().filter(|p| p.keyframe).map(|p| p.timestamp).collect();
		assert_eq!(packets.len(), 4, "one access unit per picture");
		assert_eq!(keyframes, [ntsc(0), ntsc(3)], "keyframes at the start and the cut");
		assert!(packets.iter().all(|p| !p.data.is_empty()), "an empty access unit");
		assert!(encoder.receive().unwrap().is_none(), "flush readied everything");
	}
}

#[cfg(test)]
mod rung_tests {
	use super::*;

	fn input() -> MoqVideoEncoderInput {
		MoqVideoEncoderInput {
			format: MoqVideoPixelFormat::i420(),
			width: 320,
			height: 240,
			framerate: MoqVideoFrameRate {
				numerator: 30,
				denominator: 1,
			},
		}
	}

	fn output(bitrate: u64) -> MoqVideoEncoderOutput {
		MoqVideoEncoderOutput {
			codec: MoqVideoCodec::h264(),
			bitrate: Some(bitrate),
			join: None,
			kind: MoqVideoEncoderKind::Software,
		}
	}

	struct Ladder {
		broadcast: Arc<crate::producer::MoqBroadcastProducer>,
		low: Arc<MoqVideoProducer>,
		high: Arc<MoqVideoProducer>,
	}

	fn ladder() -> Ladder {
		let estimate = moq_net::bandwidth::Producer::new();
		let bandwidth = Arc::new(MoqAllocator::new(moq_net::bandwidth::Allocator::new(
			estimate.consume(),
		)));
		let broadcast = crate::producer::MoqBroadcastProducer::new().unwrap();
		let catalog = crate::media::MoqMediaCatalogProducer::new(broadcast.clone()).unwrap();
		let publish = |track: &str, bitrate, lower| {
			MoqVideoProducer::new(
				&catalog,
				MoqVideoProducerOptions {
					input: input(),
					output: output(bitrate),
					track: Some(track.into()),
					bandwidth: Some(bandwidth.clone()),
					lower,
				},
			)
		};
		let low = publish("low", 500_000, None).unwrap();
		let high = publish("high", 2_000_000, Some(low.clone())).unwrap();
		Ladder { broadcast, low, high }
	}

	async fn priority(broadcast: &crate::producer::MoqBroadcastProducer, track: &str) -> i8 {
		let track = broadcast
			.consume()
			.unwrap()
			.subscribe_track(track.into(), None)
			.await
			.unwrap();
		track.info().priority
	}

	/// A rung sits one priority below its lower rendition, so the allocator fills that first.
	#[tokio::test]
	async fn a_rung_takes_the_priority_below_its_lower() {
		let ladder = ladder();
		assert_eq!(priority(&ladder.broadcast, "low").await, hang::catalog::PRIORITY.video);
		assert_eq!(
			priority(&ladder.broadcast, "high").await,
			hang::catalog::PRIORITY.video - 1
		);
	}

	/// A rung needs an allocator to follow and a ceiling above its lower rendition's.
	#[tokio::test]
	async fn a_rung_without_a_band_or_a_grant_is_refused() {
		let ladder = ladder();
		let catalog = crate::media::MoqMediaCatalogProducer::new(ladder.broadcast.clone()).unwrap();
		let below = MoqVideoProducer::new(
			&catalog,
			MoqVideoProducerOptions {
				input: input(),
				output: output(400_000),
				track: Some("below".into()),
				bandwidth: Some(Arc::new(MoqAllocator::new(moq_net::bandwidth::Allocator::unlimited()))),
				lower: Some(ladder.low.clone()),
			},
		);
		assert!(below.is_err());
		let ungranted = MoqVideoProducer::new(
			&catalog,
			MoqVideoProducerOptions {
				input: input(),
				output: output(4_000_000),
				track: Some("ungranted".into()),
				bandwidth: None,
				lower: Some(ladder.high.clone()),
			},
		);
		assert!(ungranted.is_err());
		let names = ladder
			.broadcast
			.with_state(|state| Ok(state.catalog.snapshot().video.renditions.len()))
			.unwrap();
		assert_eq!(names, 2, "a refused rung advertised a track");
	}
}
