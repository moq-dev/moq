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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use tokio::sync::oneshot;

use crate::bandwidth::{MoqAllocator, MoqReservation};
use crate::consumer::MoqBroadcastConsumer;
use crate::demand::MoqTrackDemand;
use crate::error::MoqError;

/// A CPU pixel layout: what [`MoqVideoProducer::write`] is fed, and what
/// [`MoqVideoDecodedFrame::pixels`] converts to.
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
	pub format: MoqVideoPixelFormat,
	/// Encoded width in pixels. Must be even (I420 chroma is subsampled 2x2).
	pub width: u32,
	/// Encoded height in pixels. Must be even.
	pub height: u32,
	/// Nominal frames per second, used for the codec time base and the default
	/// bitrate and keyframe interval. Must be non-zero.
	pub framerate: MoqVideoFrameRate,
}

/// Codec-side configuration.
#[derive(uniffi::Record)]
pub struct MoqVideoEncoderOutput {
	/// The codec used to encode this rendition.
	pub codec: MoqVideoCodec,
	/// Target bitrate in bits per second. `None` derives one from the resolution
	/// and framerate.
	#[uniffi(default = None)]
	pub bitrate: Option<u64>,
	/// How encoded pictures are divided into independently decodable groups.
	#[uniffi(default = None)]
	pub gop: Option<MoqVideoGop>,
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

/// How the encoded stream is divided into independently decodable groups.
#[derive(Clone, Copy, uniffi::Enum)]
pub enum MoqVideoGop {
	/// Start a group with a keyframe every `interval` frames.
	Keyframe {
		/// The number of frames between keyframes.
		interval: u32,
	},
}

impl From<MoqVideoGop> for moq_video::encode::Gop {
	fn from(gop: MoqVideoGop) -> Self {
		match gop {
			MoqVideoGop::Keyframe { interval } => Self::Keyframe { interval },
		}
	}
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

/// One access unit emitted by a codec-only video encoder.
#[derive(uniffi::Record)]
pub struct MoqVideoPacket {
	/// Presentation timestamp in microseconds.
	pub timestamp_us: u64,
	/// Encoded access-unit payload.
	pub data: Vec<u8>,
	/// Whether the access unit starts from a keyframe.
	pub keyframe: bool,
}

fn video_packets(packets: Vec<moq_video::encode::Encoded>) -> Vec<MoqVideoPacket> {
	packets
		.into_iter()
		.map(|packet| MoqVideoPacket {
			timestamp_us: packet.timestamp.as_micros() as u64,
			data: packet.payload.to_vec(),
			keyframe: packet.keyframe,
		})
		.collect()
}

fn video_encoder_config(
	input: &MoqVideoEncoderInput,
	output: &MoqVideoEncoderOutput,
) -> Result<moq_video::encode::Config, MoqError> {
	let framerate = input.framerate.try_into()?;
	let mut config = moq_video::encode::Config::new(input.width, input.height, framerate);
	config.codec = output.codec.into();
	config.kind = output.kind.clone().into();
	config.bitrate = output.bitrate.map(moq_net::bandwidth::Rate::from_bps);
	if let Some(gop) = output.gop {
		config.gop = gop.into();
	}
	Ok(config)
}

/// A codec-only video encoder, independent of any broadcast.
#[derive(uniffi::Object)]
pub struct MoqVideoEncoder {
	inner: std::sync::Mutex<Option<moq_video::encode::Sink>>,
	format: MoqVideoPixelFormat,
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
			inner: std::sync::Mutex::new(Some(encoder)),
			format: input.format,
			size: config.size(),
		}))
	}

	/// Encode one raw picture and return any access units now available.
	pub fn encode(&self, frame: MoqVideoFrame) -> Result<Vec<MoqVideoPacket>, MoqError> {
		let surface = match self.format {
			MoqVideoPixelFormat::I420 => moq_video::Surface::I420(moq_video::I420::new(self.size, frame.data)?),
			MoqVideoPixelFormat::Rgba => moq_video::Surface::rgba(&frame.data, self.size)?,
		};
		let frame = moq_video::Frame::new(surface, moq_net::Timestamp::from_micros(frame.timestamp_us)?);
		let mut guard = self.inner.lock().map_err(|_| panic_error())?;
		Ok(video_packets(block_on(
			guard.as_mut().ok_or(MoqError::Closed)?.encode(frame),
		)?))
	}

	/// Force the next encoded picture to start a new group.
	pub fn cut(&self) -> Result<(), MoqError> {
		let mut guard = self.inner.lock().map_err(|_| panic_error())?;
		block_on(guard.as_mut().ok_or(MoqError::Closed)?.cut())?;
		Ok(())
	}

	/// Retune the live encoder in bits per second.
	pub fn set_bitrate(&self, bitrate: u64) -> Result<(), MoqError> {
		let mut guard = self.inner.lock().map_err(|_| panic_error())?;
		block_on(
			guard
				.as_mut()
				.ok_or(MoqError::Closed)?
				.set_bitrate(moq_net::bandwidth::Rate::from_bps(bitrate)),
		)?;
		Ok(())
	}

	/// Drain buffered access units while keeping the encoder open.
	pub fn flush(&self) -> Result<Vec<MoqVideoPacket>, MoqError> {
		let mut guard = self.inner.lock().map_err(|_| panic_error())?;
		Ok(video_packets(block_on(
			guard.as_mut().ok_or(MoqError::Closed)?.flush(),
		)?))
	}

	/// Drain and close the encoder.
	pub fn finish(&self) -> Result<Vec<MoqVideoPacket>, MoqError> {
		let encoder = self
			.inner
			.lock()
			.map_err(|_| panic_error())?
			.take()
			.ok_or(MoqError::Closed)?;
		Ok(video_packets(block_on(encoder.finish())?))
	}
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
	children: &crate::producer::Children,
) -> Result<(), MoqError> {
	match drained {
		Ok(()) => {
			producer.finish()?;
			children.keep(producer);
			Ok(())
		}
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
	/// `None` while the rendition is disabled for lack of bandwidth.
	encoder: Option<moq_video::encode::Sink>,
	producer: moq_video::encode::Producer,
	/// The encoder settings, to reopen at [`ceiling`](Self::ceiling) when a disabled
	/// rendition resumes.
	config: moq_video::encode::Config,
	format: MoqVideoPixelFormat,
	/// The encoded resolution, from the publish config. Frames carry only pixels,
	/// so this is what says how to read them.
	size: moq_video::Size,
	/// The manual ceiling [`MoqVideoProducer::set_bitrate`] last applied, which caps
	/// every grant. Behind the encoder's lock, so the follower never reads an old,
	/// higher one and retunes over a newer cap. A channel, so a rung above this
	/// rendition wakes when it moves and when this rendition ends.
	ceiling: kio::Producer<moq_net::bandwidth::Rate>,
	/// The track priority this rendition was published at; a higher rung takes one below.
	priority: i8,
}

/// Stops the follow thread when the producer is finished or dropped.
///
/// `close` participates in the follower's `select!`, so a stop wakes a thread
/// parked in `consumer.changed()` instead of leaking it until the allocator dies.
struct Follow {
	close: Option<oneshot::Sender<()>>,
	thread: Option<std::thread::JoinHandle<()>>,
	failed: Arc<AtomicBool>,
}

impl Follow {
	fn check(&self) -> Result<(), MoqError> {
		if self.failed.load(Ordering::Acquire) {
			return Err(panic_error());
		}
		Ok(())
	}

	fn stop(&mut self) -> Result<(), MoqError> {
		drop(self.close.take());
		if let Some(thread) = self.thread.take() {
			thread.join().map_err(|_| panic_error())?;
		}
		self.check()
	}
}

fn panic_error() -> MoqError {
	crate::session::map_closed_error(moq_tokio::Error::Panic)
}

impl Drop for Follow {
	fn drop(&mut self) {
		let _ = self.stop();
	}
}

impl VideoProducer {
	fn write(&mut self, frame: MoqVideoFrame) -> Result<(), MoqError> {
		// A buffer that isn't one picture at the configured size is rejected here,
		// by the surface constructors, rather than reinterpreted.
		let surface = match self.format {
			MoqVideoPixelFormat::I420 => moq_video::Surface::I420(moq_video::I420::new(self.size, frame.data)?),
			MoqVideoPixelFormat::Rgba => moq_video::Surface::rgba(&frame.data, self.size)?,
		};

		// Disabled: nothing is encoded until the grant resumes the rendition.
		let Some(encoder) = self.encoder.as_mut() else {
			return Ok(());
		};
		let frame = moq_video::Frame::new(surface, moq_net::Timestamp::from_micros(frame.timestamp_us)?);
		// A backend that pipelines hands back an earlier frame's output, so this is
		// zero or more access units rather than one per call.
		let encoded = block_on(encoder.encode(frame))?;
		self.producer.publish(&encoded)?;
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

	fn finish(self, children: &crate::producer::Children) -> Result<(), MoqError> {
		let Self {
			encoder,
			mut producer,
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
		finalize(producer, drained, children)
	}
}

/// Producer for a raw-video track.
///
/// Constructed from a broadcast's media catalog. Each
/// [`write`](Self::write) accepts a [`MoqVideoFrame`] whose `data` is in the
/// pixel format declared by the [`MoqVideoEncoderInput`] passed at publish time.
///
/// Released before its finish, or still open when its broadcast closes, it ends its track with
/// `Unroutable`, which readers take as the publisher leaving.
#[derive(uniffi::Object)]
pub struct MoqVideoProducer {
	inner: Arc<std::sync::Mutex<Option<VideoProducer>>>,
	reservation: std::sync::Mutex<Option<Arc<MoqReservation>>>,
	follow: std::sync::Mutex<Option<Follow>>,
	/// Last bitrate applied by the follow loop or [`set_bitrate`](Self::set_bitrate).
	applied: Arc<AtomicU64>,
	/// The next lower rendition's ceiling, kept to follow a new track with.
	lower: Option<kio::Consumer<moq_net::bandwidth::Rate>>,
	/// Held so the reservation's registry outlives extra bandwidth handles, and to reserve
	/// against the track a decoder identity change moves the rendition to.
	bandwidth: Option<Arc<MoqAllocator>>,
	children: crate::producer::Children,
}

impl MoqVideoProducer {
	/// Move the bandwidth claim and its follower onto `demand`, the track a decoder identity change
	/// moved the rendition to: the old track's demand ended with it.
	///
	/// The follower takes the producer's lock, so call this without holding it.
	fn follow_track(&self, demand: &moq_net::track::Demand, ceiling: moq_net::bandwidth::Rate) -> Result<(), MoqError> {
		let Some(bandwidth) = &self.bandwidth else {
			return Ok(());
		};
		let stopped = self
			.follow
			.lock()
			.unwrap()
			.take()
			.map_or(Ok(()), |mut follow| follow.stop());
		let reservation = bandwidth.reserve_demand(demand, ceiling.as_bps());
		let follow = spawn_follow(Followed {
			inner: self.inner.clone(),
			lower: self.lower.clone(),
			consumer: reservation.consume(),
			ceiling,
			applied: self.applied.clone(),
		});
		*self.reservation.lock().unwrap() = Some(reservation);
		*self.follow.lock().unwrap() = Some(follow);
		stopped
	}
}

// Dropped or closed before its finish, the track ends the way a publisher leaving does.
impl crate::producer::Child for MoqVideoProducer {
	fn abort(&self, err: moq_net::Error) {
		let _guard = crate::ffi::runtime().enter();
		let Some(producer) = self
			.inner
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner)
			.take()
		else {
			return;
		};
		if let Some(mut follow) = self.follow.lock().unwrap().take() {
			let _ = follow.stop();
		}
		self.reservation.lock().unwrap().take();
		producer.producer.abort(err);
	}
}

impl Drop for MoqVideoProducer {
	fn drop(&mut self) {
		crate::producer::Child::abort(self, moq_net::Error::Unroutable);
	}
}

impl MoqVideoProducer {
	fn check(&self) -> Result<(), MoqError> {
		if let Some(follow) = self.follow.lock().unwrap().as_ref() {
			follow.check()?;
		}
		Ok(())
	}

	fn track_demand(&self) -> Result<moq_net::track::Demand, MoqError> {
		self.check()?;
		let guard = self.inner.lock().map_err(|_| panic_error())?;
		let producer = guard.as_ref().ok_or(MoqError::Closed)?;
		Ok(producer.producer.demand())
	}

	#[cfg(test)]
	pub(crate) fn applied_bitrate(&self) -> u64 {
		self.applied.load(Ordering::SeqCst)
	}
}

#[uniffi::export]
impl MoqVideoProducer {
	/// A watch-only handle to whether this video track has subscribers.
	pub fn demand(&self) -> Result<Arc<MoqTrackDemand>, MoqError> {
		Ok(MoqTrackDemand::new(self.track_demand()?))
	}

	/// Encode and publish one raw frame.
	///
	/// A backend that pipelines publishes an earlier frame's output here, so a
	/// call that emits nothing on the wire is normal rather than an error. While a
	/// [`lower`](MoqVideoProducerOptions::lower) rung is disabled for lack of
	/// bandwidth, the frame is dropped without encoding.
	pub fn write(&self, frame: MoqVideoFrame) -> Result<(), MoqError> {
		self.check()?;
		let _guard = crate::ffi::runtime().enter();
		let moved = {
			let mut guard = self.inner.lock().map_err(|_| panic_error())?;
			let producer = guard.as_mut().ok_or(MoqError::Closed)?;
			let before = producer.producer.demand();
			producer.write(frame)?;
			// A frame whose decoder identity changed, such as a resume at a ceiling past the
			// declared level, moved the rendition to a newly named track.
			let after = producer.producer.demand();
			(after.name() != before.name()).then(|| (after, *producer.ceiling.read()))
		};
		match moved {
			Some((demand, ceiling)) => self.follow_track(&demand, ceiling),
			None => Ok(()),
		}
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
	///
	/// Fails when the selected encoder cannot force a keyframe (a V4L2 driver
	/// without the control): nothing is queued, and groups keep falling at the
	/// configured interval.
	pub fn cut(&self) -> Result<(), MoqError> {
		self.check()?;
		let mut guard = self.inner.lock().map_err(|_| panic_error())?;
		let producer = guard.as_mut().ok_or(MoqError::Closed)?;
		// A keyframe is what a cut is on the wire: the importer closes the open
		// group and starts a new one at it. A disabled rung resumes on a keyframe anyway.
		if let Some(encoder) = producer.encoder.as_mut() {
			block_on(encoder.cut())?;
		}
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
	/// When this producer was published against a [`MoqAllocator`], the reservation
	/// and follower ceiling move with it, so a later grant cannot retune above this
	/// value.
	///
	/// Errors if this backend cannot retune while running. That is not fatal: the
	/// encoder keeps running at its current rate, so stop adapting rather than
	/// stop publishing.
	pub fn set_bitrate(&self, bitrate: u64) -> Result<(), MoqError> {
		self.check()?;
		let _guard = crate::ffi::runtime().enter();
		{
			let mut guard = self.inner.lock().map_err(|_| panic_error())?;
			let producer = guard.as_mut().ok_or(MoqError::Closed)?;
			let bitrate = moq_net::bandwidth::Rate::from_bps(bitrate);
			// A disabled rung has no encoder to retune; it reopens at the new ceiling.
			if let Some(encoder) = producer.encoder.as_mut() {
				block_on(encoder.set_bitrate(bitrate))?;
			}
			// Under the same lock as the retune, so the follower sees both or neither.
			let Ok(mut ceiling) = producer.ceiling.write() else {
				unreachable!("only dropping the producer closes its ceiling")
			};
			*ceiling = bitrate;
		}
		if let Some(reservation) = self.reservation.lock().unwrap().as_ref() {
			reservation.update(bitrate);
		}
		self.applied.store(bitrate, Ordering::SeqCst);
		Ok(())
	}

	/// This encoder's bandwidth reservation, if it was published against a
	/// [`MoqAllocator`]. Dropping the handle does not release the claim; the
	/// producer holds it until [`finish`](Self::finish). A frame that changes the
	/// decoder identity publishes under a new track name and reserves against that
	/// track instead, so read this again after [`write`](Self::write).
	pub fn reservation(&self) -> Option<Arc<MoqReservation>> {
		self.reservation.lock().unwrap().clone()
	}

	/// Flush any frames the codec is still holding and finalize the track.
	///
	/// The broadcast keeps the finished track until it ends, so its finish still holds it.
	pub fn finish(&self) -> Result<(), MoqError> {
		let _guard = crate::ffi::runtime().enter();
		// Stop following and release the share before draining, so siblings can
		// take the room while the last frames go out. The follower takes the
		// producer's lock, so it stops before the drain holds it.
		let followed = self
			.follow
			.lock()
			.unwrap()
			.take()
			.map_or(Ok(()), |mut follow| follow.stop());
		self.reservation.lock().unwrap().take();
		// Held through the drain, so a racing broadcast close waits it out instead of
		// missing a track that still publishes.
		let mut inner = self.inner.lock().map_err(|_| panic_error())?;
		let producer = inner.take().ok_or(MoqError::Closed)?;
		// A follower panic only stops rate adaptation, so the encoded frames still
		// end the track cleanly before the panic is reported.
		producer.finish(&self.children)?;
		followed
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
	/// session's allocator and follow the grant with the same policy the Rust
	/// capture encoder uses. [`set_bitrate`](MoqVideoProducer::set_bitrate) is
	/// the manual ceiling: it retunes the encoder and moves the reservation. Set
	/// `options.lower` too for each rendition above the lowest of one source.
	#[uniffi::constructor]
	pub fn new(
		catalog: &crate::media::MoqMediaCatalogProducer,
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
		// The lower rendition's handle, its ceiling, and the priority this one takes below it.
		let lower = match lower {
			Some(lower) => {
				if bandwidth.is_none() {
					return Err(MoqError::Video {
						reason: "a lower rendition needs a bandwidth allocator to follow".into(),
					});
				}
				let guard = lower.inner.lock().map_err(|_| panic_error())?;
				let below = guard.as_ref().ok_or(MoqError::Closed)?;
				Some((
					below.ceiling.consume(),
					*below.ceiling.read(),
					below.priority.saturating_sub(1),
				))
			}
			None => None,
		};
		let priority = lower
			.as_ref()
			.map_or(hang::catalog::PRIORITY.video, |(_, _, priority)| *priority);

		// Both before the track exists: a config this machine can't encode should fail
		// without leaving a track advertised that will never carry frames. The probe
		// closes its encoder before this one opens, so only one codec session is live.
		let rendition = block_on(config.probe())?;
		let encoder = block_on(moq_video::encode::Sink::open(&config))?;
		let gop = config.gop.duration(config.framerate);
		let ceiling = video_ceiling(&config, &rendition);
		if let Some((_, below, _)) = &lower
			&& *below >= ceiling
		{
			return Err(MoqError::Video {
				reason: format!(
					"the lower rendition's {} bps is not below this one's {} bps",
					below.as_bps(),
					ceiling.as_bps()
				),
			});
		}
		// The suffix `moq_video::encode::Producer::new` would derive, built here so the
		// track can take this rendition's priority.
		let suffix = match &rendition.codec {
			hang::catalog::VideoCodec::H264(_) => ".avc3",
			hang::catalog::VideoCodec::H265(_) => ".hev1",
			other => {
				return Err(MoqError::Video {
					reason: format!("{other} is not a codec this producer can publish"),
				});
			}
		};
		let children = broadcast.with_state(|state| Ok(state.children.clone()))?;
		let producer = broadcast.with_state(|state| {
			let info = state.catalog.track_info(priority);
			let track = match track {
				Some(name) => state.broadcast.create_track(name, info)?,
				None => state.broadcast.unique_track(suffix, info)?,
			};
			Ok(moq_video::encode::Producer::with_track(
				track,
				state.catalog.reserve().with_hls_hint(gop),
				rendition,
			)?)
		})?;

		let inner = Arc::new(std::sync::Mutex::new(Some(VideoProducer {
			encoder: Some(encoder),
			producer,
			format: input.format,
			size: config.size(),
			config,
			ceiling: kio::Producer::new(ceiling),
			priority,
		})));
		let applied = Arc::new(AtomicU64::new(ceiling.as_bps()));
		let reserved = bandwidth.as_ref().map(|bandwidth| {
			let demand = inner
				.lock()
				.unwrap()
				.as_ref()
				.expect("just constructed")
				.producer
				.demand();
			bandwidth.reserve_demand(&demand, ceiling.as_bps())
		});
		let lower = lower.map(|(lower, _, _)| lower);
		let follow = reserved.as_ref().map(|reservation| {
			spawn_follow(Followed {
				inner: inner.clone(),
				lower: lower.clone(),
				consumer: reservation.consume(),
				ceiling,
				applied: applied.clone(),
			})
		});

		children.adopt(Arc::new(MoqVideoProducer {
			inner,
			reservation: std::sync::Mutex::new(reserved),
			follow: std::sync::Mutex::new(follow),
			applied,
			lower,
			bandwidth,
			children: children.clone(),
		}))
	}
}

/// The bitrate this encoder reserved, which is the configured ceiling or the
/// one [`Config::probe`](moq_video::encode::Config::probe) filled in.
fn video_ceiling(
	config: &moq_video::encode::Config,
	rendition: &hang::catalog::VideoConfig,
) -> moq_net::bandwidth::Rate {
	config
		.bitrate
		.or_else(|| rendition.bitrate.map(moq_net::bandwidth::Rate::from_bps))
		.unwrap_or_else(|| {
			// Same 0.07 bits/pixel/s default moq-video uses when neither is set.
			moq_net::bandwidth::Rate::from_bps(
				(config.size().pixels() as f64 * config.framerate.as_f64() * 0.07) as u64,
			)
		})
}

/// What the follow thread reads and retunes.
struct Followed {
	inner: Arc<std::sync::Mutex<Option<VideoProducer>>>,
	/// The next lower rendition's ceiling, which sets this one's band. A consumer, so a rung
	/// never keeps the rendition below it publishing.
	lower: Option<kio::Consumer<moq_net::bandwidth::Rate>>,
	consumer: moq_net::bandwidth::Consumer,
	ceiling: moq_net::bandwidth::Rate,
	applied: Arc<AtomicU64>,
}

fn spawn_follow(followed: Followed) -> Follow {
	let (close, closed) = oneshot::channel();
	let failed = Arc::new(AtomicBool::new(false));
	let failure = failed.clone();
	// A dedicated thread rather than the FFI current-thread runtime: this loop
	// `pollster::block_on`s `set_bitrate`, which would stall that runtime.
	let thread = std::thread::Builder::new()
		.name("moq-ffi-rate".into())
		.spawn(move || {
			let completed = pollster::block_on(moq_tokio::panic::isolate(async move {
				tokio::runtime::Builder::new_current_thread()
					.enable_all()
					.build()
					.expect("rate-control runtime")
					.block_on(async move {
						tokio::select! {
							biased;
							_ = closed => {}
							_ = follow_reservation(followed) => {}
						}
					})
			}));
			if completed.is_none() {
				failure.store(true, Ordering::Release);
			}
		})
		.expect("failed to spawn rate-control thread");
	Follow {
		close: Some(close),
		thread: Some(thread),
		failed,
	}
}

/// The policy a rendition follows: its manual `ceiling`, above the `lower` rendition's
/// current ceiling, if any.
fn video_policy(ceiling: moq_net::bandwidth::Rate, lower: Option<moq_net::bandwidth::Rate>) -> moq_mux::rate::Policy {
	let mut policy = moq_mux::rate::Policy::new(ceiling);
	policy.lower = lower;
	policy
}

/// The lower rendition's current ceiling, or `None` once it finished or was dropped,
/// which leaves this one nothing to fall back to, so it never disables. Arms `waiter`
/// for either change.
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
async fn follow_reservation(followed: Followed) {
	let Followed {
		inner,
		lower,
		consumer,
		ceiling,
		applied,
	} = followed;
	let noop = kio::Waiter::noop();
	let mut follower = moq_mux::rate::Follower::new(consumer, video_policy(ceiling, lower_ceiling(&lower, &noop)));
	loop {
		// Both ceilings are read on every wake, before the follower filters a grant against
		// them. This one's `set_bitrate` moves its reservation after moving its ceiling,
		// which wakes this poll; the lower one's ceiling wakes it directly, wherever that
		// rendition reserves. The lower one is read first, so no two encoder locks are
		// ever held at once.
		let next = kio::wait(|waiter| {
			let below = lower_ceiling(&lower, waiter);
			let guard = inner.lock().unwrap();
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
		let mut guard = inner.lock().unwrap();
		let Some(producer) = guard.as_mut() else {
			return;
		};
		let policy = video_policy(*producer.ceiling.read(), below);
		match retune(&mut follower, policy, target, |target| producer.apply(target)) {
			Ok(Some(moq_mux::rate::Target::Enabled(bitrate))) => applied.store(bitrate.as_bps(), Ordering::SeqCst),
			Ok(Some(moq_mux::rate::Target::Disabled)) => applied.store(0, Ordering::SeqCst),
			Ok(None) => {}
			Err(_) => return,
		}
	}
}

/// Apply the follower's next `target` through `apply`, unless the `policy` moved since it
/// was asked for: then follow under the new policy, whose manual ceiling
/// `MoqVideoProducer::set_bitrate` already applied, instead. Returns the target applied,
/// or `Err` with the name of an encoder that cannot follow at all.
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
	/// Upper bound on buffering before skipping a stalled group. Same knob as
	/// [`MoqAudioDecoderOutput::max_delay`](crate::audio::MoqAudioDecoderOutput::max_delay).
	/// `None` keeps the moq-mux default of zero (skip aggressively).
	#[uniffi(default = None)]
	pub max_delay: Option<std::time::Duration>,
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
#[derive(Clone, Copy, uniffi::Enum)]
pub enum MoqVideoSurface {
	/// An Apple `CVPixelBufferRef` from VideoToolbox, IOSurface-backed NV12.
	PixelBuffer {
		/// The borrowed CVPixelBuffer address, valid while the frame is held.
		pointer: u64,
	},
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
	/// Presentation timestamp, in microseconds.
	pub fn timestamp_us(&self) -> u64 {
		// A decoded Timestamp is bounded by a QUIC varint, so its microseconds fit.
		self.frame.timestamp.as_micros() as u64
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
	pub fn pixels(&self, format: MoqVideoPixelFormat) -> Result<Vec<u8>, MoqError> {
		let surface = &self.frame.surface;
		match format {
			MoqVideoPixelFormat::I420 => surface.to_i420().map(|i420| i420.into_owned().into_data()),
			MoqVideoPixelFormat::Rgba => surface
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
			#[cfg(any(target_os = "macos", target_os = "ios"))]
			moq_video::Surface::PixelBuffer(pixels) => Some(MoqVideoSurface::PixelBuffer {
				pointer: std::ptr::from_ref(pixels.buffer()).addr() as u64,
			}),
			_ => None,
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
		/// The furthest presentation point already read, in microseconds.
		timestamp_us: u64,
		/// An upper bound on how long until the next picture resumes.
		duration: std::time::Duration,
	},
	/// The publisher stopped its encoder: it paused, switched source, or finished.
	///
	/// The stopped encoder's last pictures precede it; what follows comes from a fresh
	/// encoder.
	End {
		/// Where the stopped encoder's media stops, in microseconds.
		end_us: u64,
	},
}

impl TryFrom<moq_video::decode::Read> for MoqVideoRead {
	type Error = MoqError;

	fn try_from(read: moq_video::decode::Read) -> Result<Self, MoqError> {
		use crate::consumer::timestamp_us;
		Ok(match read {
			moq_video::decode::Read::Frame(frame) => Self::Frame {
				frame: Arc::new(MoqVideoDecodedFrame { frame }),
			},
			moq_video::decode::Read::Gap { timestamp, duration } => Self::Gap {
				timestamp_us: timestamp_us(timestamp)?,
				duration: crate::ffi::duration(duration.into()),
			},
			moq_video::decode::Read::End { end } => Self::End {
				end_us: timestamp_us(end)?,
			},
		})
	}
}

struct VideoConsumerInner {
	consumer: moq_video::decode::Consumer,
}

impl VideoConsumerInner {
	async fn next(&mut self) -> Result<Option<MoqVideoRead>, MoqError> {
		self.consumer.read().await?.map(MoqVideoRead::try_from).transpose()
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
	let codec: hang::catalog::VideoCodec = catalog_video.codec.parse().map_err(|_| MoqError::Unsupported)?;
	// Parsing is total: an unrecognized name becomes `Unknown` rather than failing. Reject it here
	// so a typo in the catalog is an error at subscribe, not an opaque backend failure later.
	if matches!(codec, hang::catalog::VideoCodec::Unknown(_)) {
		return Err(MoqError::Unsupported);
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
	config.container = catalog_video.container.into();
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
	/// [`MoqMediaCatalogConsumer::next`](crate::media::MoqMediaCatalogConsumer::next)); the codec is read
	/// from it. Errors if no native backend handles that codec, rather than failing on the first
	/// frame. Also fails with [`MoqError::Unsupported`] when
	/// [`surface`](MoqVideoDecoderOutput::surface) is set on a platform with no surface to expose.
	///
	/// A [disabled](crate::media::MoqVideo::enabled) rendition subscribes too, and decodes once its
	/// publisher re-enables it. Don't pick one, but keep one already playing while no enabled
	/// rendition is on offer.
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
		let epoch = catalog_video.epoch.clone();
		let cfg = video_config(catalog_video)?;
		let mut options = moq_video::decode::Options::default();
		options.decoder.output = decoder_output(output.surface, HAS_SURFACE)?;
		let resolved = broadcast.resolve_inner(reference.as_deref(), epoch.as_deref()).await?;

		options.decoder.scale_hint = output.resize.map(|size| moq_video::Size::new(size.width, size.height));
		options.max_delay = output.max_delay.unwrap_or_default();
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

	fn catalog_video(codec: &str) -> MoqVideo {
		MoqVideo {
			preference: 0,
			label: None,
			broadcast: None,
			epoch: None,
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

	/// A consumer holds a disabled rendition it already plays, so one converts like any other.
	#[test]
	fn video_config_accepts_disabled_renditions() {
		let mut config = catalog_video("avc1.64001f");
		config.enabled = false;
		config.preference = -1;
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
		assert!(matches!(error, MoqError::Unsupported));
	}
}

#[cfg(test)]
mod finish_tests {
	use super::*;

	#[test]
	fn encoder_config_preserves_exact_rates_and_rejects_invalid_ratios() {
		let output = MoqVideoEncoderOutput {
			codec: MoqVideoCodec::H264,
			bitrate: None,
			gop: None,
			kind: MoqVideoEncoderKind::Software,
		};
		for (numerator, denominator) in [(30_000, 1001), (60_000, 2002), (24_000, 1001)] {
			let config = video_encoder_config(
				&MoqVideoEncoderInput {
					format: MoqVideoPixelFormat::I420,
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
					format: MoqVideoPixelFormat::I420,
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
	/// `finish` closes it. No test in any language had driven it.
	#[test]
	fn encoder_emits_keyframes_where_asked() {
		let (width, height) = (64u32, 48u32);
		let encoder = MoqVideoEncoder::new(
			MoqVideoEncoderInput {
				format: MoqVideoPixelFormat::I420,
				width,
				height,
				framerate: crate::video::MoqVideoFrameRate {
					numerator: 30_000,
					denominator: 1001,
				},
			},
			MoqVideoEncoderOutput {
				codec: MoqVideoCodec::H264,
				bitrate: Some(500_000),
				gop: None,
				kind: MoqVideoEncoderKind::Software,
			},
		)
		.unwrap();
		let picture = |i: u64| MoqVideoFrame {
			timestamp_us: i * 1_001_000 / 30,
			data: vec![(i * 40) as u8; (width * height * 3 / 2) as usize],
		};

		let mut packets = Vec::new();
		for i in 0..3 {
			packets.extend(encoder.encode(picture(i)).unwrap());
		}
		packets.extend(encoder.flush().unwrap());
		encoder.set_bitrate(250_000).unwrap();
		encoder.cut().unwrap();
		packets.extend(encoder.encode(picture(3)).unwrap());
		packets.extend(encoder.finish().unwrap());

		let keyframes: Vec<u64> = packets.iter().filter(|p| p.keyframe).map(|p| p.timestamp_us).collect();
		assert_eq!(packets.len(), 4, "one access unit per picture");
		assert_eq!(keyframes, [0, 3 * 1_001_000 / 30], "keyframes at the start and the cut");
		assert!(packets.iter().all(|p| !p.data.is_empty()), "an empty access unit");
		assert!(matches!(encoder.encode(picture(4)), Err(MoqError::Closed)));
	}

	#[tokio::test]
	async fn finish_ends_the_track_after_a_rate_follower_panic() {
		let estimate = moq_net::bandwidth::Producer::new();
		let bandwidth = Arc::new(MoqAllocator::new(moq_net::bandwidth::Allocator::new(
			estimate.consume(),
		)));
		let broadcast = crate::producer::MoqBroadcastProducer::new().unwrap();
		let input = MoqVideoEncoderInput {
			format: MoqVideoPixelFormat::Rgba,
			width: 320,
			height: 240,
			framerate: crate::video::MoqVideoFrameRate {
				numerator: 30,
				denominator: 1,
			},
		};
		let output = MoqVideoEncoderOutput {
			codec: MoqVideoCodec::H264,
			bitrate: Some(4_000_000),
			gop: None,
			kind: MoqVideoEncoderKind::Software,
		};
		let video = crate::video::MoqVideoProducer::new(
			&crate::media::MoqMediaCatalogProducer::new(broadcast.clone()).unwrap(),
			crate::video::MoqVideoProducerOptions {
				input,
				output,
				track: Some("camera".into()),
				bandwidth: Some(bandwidth),
				lower: None,
			},
		)
		.unwrap();
		let track = broadcast
			.consume()
			.unwrap()
			.subscribe_track("camera".into(), None)
			.await
			.unwrap();
		// Stand in for a follower thread that panicked.
		video
			.follow
			.lock()
			.unwrap()
			.as_ref()
			.expect("published against an allocator")
			.failed
			.store(true, Ordering::Release);

		let result = video.finish();
		assert!(matches!(result, Err(MoqError::Task { .. })), "{result:?}");
		drop(video);
		let result = track.recv_group().await.map(|group| group.is_some());
		assert!(matches!(result, Ok(false)), "the track should end cleanly: {result:?}");
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

	/// `set_bitrate` lowers the ceiling while a raise toward the old one is
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

		// `set_bitrate` already retuned the encoder to the new cap.
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

#[cfg(test)]
mod rung_tests {
	use moq_mux::rate::Target;
	use moq_net::bandwidth::Rate;

	use super::*;

	fn input() -> MoqVideoEncoderInput {
		MoqVideoEncoderInput {
			format: MoqVideoPixelFormat::I420,
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
			codec: MoqVideoCodec::H264,
			bitrate: Some(bitrate),
			gop: None,
			kind: MoqVideoEncoderKind::Software,
		}
	}

	fn frame(i: u64) -> MoqVideoFrame {
		MoqVideoFrame {
			timestamp_us: i * 33_333,
			data: vec![(i * 40) as u8; 320 * 240 * 3 / 2],
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

	fn enabled(broadcast: &crate::producer::MoqBroadcastProducer, track: &str) -> bool {
		broadcast
			.with_state(|state| Ok(state.catalog.snapshot().video.renditions[track].enabled))
			.unwrap()
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
		assert!(matches!(below, Err(MoqError::Video { .. })));
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
		assert!(matches!(ungranted, Err(MoqError::Video { .. })));
	}

	/// A rung's follower wakes on its lower rendition's own ceiling, whether that moves or
	/// ends, so it notices even when the lower rendition reserves nothing it shares.
	#[tokio::test]
	async fn a_rung_wakes_when_its_unreserved_lower_moves_or_ends() {
		struct Flag(AtomicBool);
		impl std::task::Wake for Flag {
			fn wake(self: Arc<Self>) {
				self.0.store(true, Ordering::SeqCst);
			}
		}

		let broadcast = crate::producer::MoqBroadcastProducer::new().unwrap();
		let catalog = crate::media::MoqMediaCatalogProducer::new(broadcast.clone()).unwrap();
		let low = MoqVideoProducer::new(
			&catalog,
			MoqVideoProducerOptions {
				input: input(),
				output: output(500_000),
				track: Some("low".into()),
				bandwidth: None,
				lower: None,
			},
		)
		.unwrap();
		let lower = Some(low.inner.lock().unwrap().as_ref().unwrap().ceiling.consume());

		let flag = Arc::new(Flag(AtomicBool::new(false)));
		let waiter = kio::Waiter::new(std::task::Waker::from(flag.clone()));
		assert_eq!(lower_ceiling(&lower, &waiter), Some(Rate::from_kbps(500)));

		low.set_bitrate(400_000).unwrap();
		assert!(flag.0.swap(false, Ordering::SeqCst), "a moved ceiling wakes the rung");
		assert_eq!(lower_ceiling(&lower, &waiter), Some(Rate::from_kbps(400)));

		low.finish().unwrap();
		assert!(flag.0.load(Ordering::SeqCst), "an ended lower rendition wakes the rung");
		assert_eq!(lower_ceiling(&lower, &waiter), None);
	}

	/// A rung resumed at a ceiling past its declared level publishes a new track, and its
	/// bandwidth claim moves there with it rather than watching the track that ended.
	#[tokio::test]
	async fn a_rung_resumed_past_its_level_reserves_on_the_new_track() {
		let ladder = ladder();
		ladder.high.write(frame(0)).unwrap();
		let before = ladder.high.reservation().unwrap();
		let apply = |target| {
			let mut guard = ladder.high.inner.lock().unwrap();
			guard.as_mut().unwrap().apply(target).unwrap();
		};
		apply(Target::Disabled);
		// 3 Mbps takes H.264 level 2.1, past the 2.0 the 2 Mbps rung declared.
		ladder.high.set_bitrate(3_000_000).unwrap();
		apply(Target::Enabled(Rate::from_kbps(3_000)));
		ladder.high.write(frame(1)).unwrap();

		let names = ladder
			.broadcast
			.with_state(|state| {
				Ok(state
					.catalog
					.snapshot()
					.video
					.renditions
					.into_keys()
					.collect::<Vec<_>>())
			})
			.unwrap();
		assert!(names.contains(&"high/1".to_string()), "{names:?} has no new track");
		let after = ladder.high.reservation().unwrap();
		assert!(!Arc::ptr_eq(&before, &after), "the claim still watches the ended track");
		ladder.high.finish().unwrap();
	}

	/// A disabled rung advertises itself disabled, drops the frames written to it, and
	/// resumes on a keyframe in a new group, its advertised maximum untouched throughout.
	#[tokio::test]
	async fn a_disabled_rung_stops_encoding_until_it_resumes() {
		let ladder = ladder();
		let track = ladder
			.broadcast
			.consume()
			.unwrap()
			// Every group, however old, since the track is read only once it has ended.
			.subscribe_track(
				"high".into(),
				Some(crate::consumer::MoqSubscription {
					priority: 0,
					max_delay: None,
					ranges: None,
					order: None,
				}),
			)
			.await
			.unwrap();
		ladder.high.write(frame(0)).unwrap();
		let bitrate = |ladder: &Ladder| {
			ladder
				.broadcast
				.with_state(|state| Ok(state.catalog.snapshot().video.renditions["high"].bitrate))
				.unwrap()
		};
		let advertised = bitrate(&ladder);

		let apply = |target| {
			let mut guard = ladder.high.inner.lock().unwrap();
			guard.as_mut().unwrap().apply(target).unwrap();
		};
		apply(Target::Disabled);
		assert!(!enabled(&ladder.broadcast, "high"));
		assert!(enabled(&ladder.broadcast, "low"), "only the rung disables");
		for i in 1..10 {
			ladder.high.write(frame(i)).unwrap();
		}
		ladder.high.cut().unwrap();
		// Within the level the rung declared: a ceiling past it reopens the encoder at a new
		// level, which is a new codec string and so a new track.
		ladder.high.set_bitrate(1_500_000).unwrap();

		apply(Target::Enabled(Rate::from_kbps(1_500)));
		assert!(enabled(&ladder.broadcast, "high"));
		ladder.high.write(frame(10)).unwrap();
		ladder.high.finish().unwrap();
		assert_eq!(bitrate(&ladder), advertised, "the advertised maximum never moves");

		// The opening keyframe, the disable's discontinuity marker, then the resumed
		// keyframe, each its own group: nothing written while disabled was encoded.
		let mut groups = Vec::new();
		while let Some(group) = track.recv_group().await.unwrap() {
			let mut frames = 0;
			while group.read_frame().await.unwrap().is_some() {
				frames += 1;
			}
			groups.push(frames);
		}
		assert_eq!(groups, [1, 1, 1]);
	}
}
