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

use crate::bandwidth::{MoqBandwidth, MoqReservation};
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
	pub framerate: u32,
}

/// Codec-side configuration.
#[derive(uniffi::Record)]
pub struct MoqVideoEncoderOutput {
	/// The codec used to encode this rendition.
	pub codec: MoqVideoCodec,
	/// Track name for a broadcast-bound producer. `None` derives one from the codec.
	#[uniffi(default = None)]
	pub track: Option<String>,
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
	let framerate = moq_video::Rate::new(input.framerate, 1)
		.map_err(|_| MoqError::from(moq_video::Error::InvalidFramerate(input.framerate)))?;
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
	mut producer: moq_video::encode::Producer<moq_mux::catalog::hang::Extra>,
	drained: Result<(), moq_video::Error>,
	finished: &crate::producer::Finished,
) -> Result<(), MoqError> {
	match drained {
		Ok(()) => {
			producer.finish()?;
			finished.keep(producer);
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
	encoder: moq_video::encode::Sink,
	producer: moq_video::encode::Producer<moq_mux::catalog::hang::Extra>,
	format: MoqVideoPixelFormat,
	/// The encoded resolution, from the publish config. Frames carry only pixels,
	/// so this is what says how to read them.
	size: moq_video::Size,
	/// The manual ceiling [`MoqVideoProducer::set_bitrate`] last applied, which caps
	/// every grant. Behind the encoder's lock, so the follower never reads an old,
	/// higher one and retunes over a newer cap.
	ceiling: moq_net::bandwidth::Rate,
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

		let frame = moq_video::Frame::new(surface, moq_net::Timestamp::from_micros(frame.timestamp_us)?);
		// A backend that pipelines hands back an earlier frame's output, so this is
		// zero or more access units rather than one per call.
		let encoded = block_on(self.encoder.encode(frame))?;
		self.producer.publish(&encoded)?;
		Ok(())
	}

	fn finish(self, finished: &crate::producer::Finished) -> Result<(), MoqError> {
		let Self {
			encoder, mut producer, ..
		} = self;
		// Drain the codec into the track before ending it, so the last frames land
		// in it rather than being dropped with the encoder.
		let drained = block_on(encoder.finish()).and_then(|encoded| producer.publish(&encoded));
		finalize(producer, drained, finished)
	}
}

/// Producer for a raw-video track.
///
/// Constructed from a broadcast's media catalog. Each
/// [`write`](Self::write) accepts a [`MoqVideoFrame`] whose `data` is in the
/// pixel format declared by the [`MoqVideoEncoderInput`] passed at publish time.
#[derive(uniffi::Object)]
pub struct MoqVideoProducer {
	inner: Arc<std::sync::Mutex<Option<VideoProducer>>>,
	reservation: std::sync::Mutex<Option<Arc<MoqReservation>>>,
	follow: std::sync::Mutex<Option<Follow>>,
	/// Last bitrate applied by the follow loop or [`set_bitrate`](Self::set_bitrate).
	applied: Arc<AtomicU64>,
	/// Held so the reservation's registry outlives extra bandwidth handles.
	_bandwidth: Option<Arc<MoqBandwidth>>,
	finished: crate::producer::Finished,
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
	/// call that emits nothing on the wire is normal rather than an error.
	pub fn write(&self, frame: MoqVideoFrame) -> Result<(), MoqError> {
		self.check()?;
		let _guard = crate::ffi::runtime().enter();
		let mut guard = self.inner.lock().map_err(|_| panic_error())?;
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
	///
	/// Fails when the selected encoder cannot force a keyframe (a V4L2 driver
	/// without the control): nothing is queued, and groups keep falling at the
	/// configured interval.
	pub fn cut(&self) -> Result<(), MoqError> {
		self.check()?;
		let mut guard = self.inner.lock().map_err(|_| panic_error())?;
		let producer = guard.as_mut().ok_or(MoqError::Closed)?;
		// A keyframe is what a cut is on the wire: the importer closes the open
		// group and starts a new one at it.
		block_on(producer.encoder.cut())?;
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
	/// When this producer was published against a [`MoqBandwidth`], the reservation
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
			block_on(producer.encoder.set_bitrate(bitrate))?;
			// Under the same lock as the retune, so the follower sees both or neither.
			producer.ceiling = bitrate;
		}
		if let Some(reservation) = self.reservation.lock().unwrap().as_ref() {
			reservation.update(bitrate);
		}
		self.applied.store(bitrate, Ordering::SeqCst);
		Ok(())
	}

	/// This encoder's bandwidth reservation, if it was published against a
	/// [`MoqBandwidth`]. Dropping the handle does not release the claim; the
	/// producer holds it until [`finish`](Self::finish).
	pub fn reservation(&self) -> Option<Arc<MoqReservation>> {
		self.reservation.lock().unwrap().clone()
	}

	/// Flush any frames the codec is still holding and finalize the track.
	///
	/// The broadcast keeps the finished track until it ends, so its finish still holds it.
	pub fn finish(&self) -> Result<(), MoqError> {
		let _guard = crate::ffi::runtime().enter();
		let producer = self
			.inner
			.lock()
			.map_err(|_| panic_error())?
			.take()
			.ok_or(MoqError::Closed)?;
		// Stop following and release the share before draining, so siblings can
		// take the room while the last frames go out.
		let followed = self
			.follow
			.lock()
			.unwrap()
			.take()
			.map_or(Ok(()), |mut follow| follow.stop());
		self.reservation.lock().unwrap().take();
		// A follower panic only stops rate adaptation, so the encoded frames still
		// end the track cleanly before the panic is reported.
		producer.finish(&self.finished)?;
		followed
	}
}

#[uniffi::export]
impl MoqVideoProducer {
	/// Open a video track on the catalog's broadcast, encoding the raw frames written to
	/// it.
	///
	/// The encoder opens here, so an unsupported codec, resolution, or backend
	/// fails now rather than on the first frame. [`MoqVideoEncoderOutput::track`]
	/// chooses the track name; `None` derives one from the codec. The catalog
	/// rendition is published immediately so a subscriber can discover the track
	/// before a frame is written to it.
	///
	/// Pass `bandwidth` to reserve this track's configured bitrate against the
	/// session's allocator and follow the grant with the same policy the Rust
	/// capture encoder uses. [`set_bitrate`](MoqVideoProducer::set_bitrate) is
	/// the manual ceiling: it retunes the encoder and moves the reservation.
	#[uniffi::constructor(default(bandwidth = None))]
	pub fn new(
		catalog: &crate::media::MoqMediaCatalogProducer,
		input: MoqVideoEncoderInput,
		output: MoqVideoEncoderOutput,
		bandwidth: Option<Arc<MoqBandwidth>>,
	) -> Result<Arc<MoqVideoProducer>, MoqError> {
		let _guard = crate::ffi::runtime().enter();
		let broadcast = catalog.broadcast()?;

		let framerate = moq_video::Rate::new(input.framerate, 1)
			.map_err(|_| MoqError::from(moq_video::Error::InvalidFramerate(input.framerate)))?;
		let mut config = moq_video::encode::Config::new(input.width, input.height, framerate);
		config.codec = output.codec.into();
		config.kind = output.kind.into();
		config.bitrate = output.bitrate.map(moq_net::bandwidth::Rate::from_bps);
		if let Some(gop) = output.gop {
			config.gop = gop.into();
		}

		// Both before the track exists: a config this machine can't encode should fail
		// without leaving a track advertised that will never carry frames. The probe
		// closes its encoder before this one opens, so only one codec session is live.
		let rendition = block_on(config.probe())?;
		let encoder = block_on(moq_video::encode::Sink::open(&config))?;
		let gop = config.gop.duration(config.framerate);
		let ceiling = video_ceiling(&config, &rendition);
		let finished = broadcast.with_state(|state| Ok(state.finished.clone()))?;
		let producer = broadcast.with_state(|state| match output.track {
			Some(name) => {
				let track = state
					.broadcast
					.create_track(name, state.catalog.track_info(hang::catalog::PRIORITY.video))?;
				Ok(moq_video::encode::Producer::with_track(
					track,
					state.catalog.reserve().with_timeline_hint(gop),
					rendition,
				)?)
			}
			None => Ok(moq_video::encode::Producer::new(
				state.broadcast.clone(),
				state.catalog.reserve().with_timeline_hint(gop),
				rendition,
			)?),
		})?;

		let inner = Arc::new(std::sync::Mutex::new(Some(VideoProducer {
			encoder,
			producer,
			format: input.format,
			size: config.size(),
			ceiling,
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
		let follow = reserved
			.as_ref()
			.map(|reservation| spawn_follow(inner.clone(), reservation.consumer(), ceiling, applied.clone()));

		Ok(Arc::new(MoqVideoProducer {
			inner,
			reservation: std::sync::Mutex::new(reserved),
			follow: std::sync::Mutex::new(follow),
			applied,
			_bandwidth: bandwidth,
			finished,
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

fn spawn_follow(
	inner: Arc<std::sync::Mutex<Option<VideoProducer>>>,
	consumer: moq_net::bandwidth::Consumer,
	ceiling: moq_net::bandwidth::Rate,
	applied: Arc<AtomicU64>,
) -> Follow {
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
							_ = follow_reservation(inner, consumer, ceiling, applied) => {}
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

/// Feed each grant through the same policy moq-video uses, and retune the live
/// encoder. Retires on [`BitrateUnsupported`](moq_video::Error::BitrateUnsupported).
async fn follow_reservation(
	inner: Arc<std::sync::Mutex<Option<VideoProducer>>>,
	mut consumer: moq_net::bandwidth::Consumer,
	ceiling: moq_net::bandwidth::Rate,
	applied: Arc<AtomicU64>,
) {
	let mut follower = Follower::new(ceiling);
	while let Some(estimate) = next_grant(&mut consumer, follower.deadline()).await {
		let mut guard = inner.lock().unwrap();
		let Some(producer) = guard.as_mut() else {
			return;
		};
		let ceiling = producer.ceiling;
		let retune = |bitrate| block_on(producer.encoder.set_bitrate(bitrate));
		match follower.step(ceiling, estimate, moq_clock::now(), retune) {
			Ok(Some(bitrate)) => applied.store(bitrate.as_bps(), Ordering::SeqCst),
			Ok(None) => {}
			Err(_) => return,
		}
	}
}

/// The next grant, or the current one again at `deadline`; `None` once the grant
/// is gone for good.
async fn next_grant(
	consumer: &mut moq_net::bandwidth::Consumer,
	deadline: Option<moq_clock::Instant>,
) -> Option<Option<moq_net::bandwidth::Rate>> {
	// A raise climbs over several updates, and a settled grant sends no change to
	// climb on.
	if let Some(at) = deadline {
		tokio::select! {
			estimate = consumer.changed() => return estimate.ok(),
			() = moq_clock::sleep_until(at) => {}
		}
		return Some(consumer.peek());
	}
	consumer.changed().await.ok()
}

/// The grant policy for one encoder, restarted at the manual ceiling whenever
/// [`MoqVideoProducer::set_bitrate`] moves it.
struct Follower {
	ceiling: moq_net::bandwidth::Rate,
	control: moq_mux::rate::Control,
}

impl Follower {
	fn new(ceiling: moq_net::bandwidth::Rate) -> Self {
		Self {
			ceiling,
			control: moq_mux::rate::Control::new(moq_mux::rate::Policy::new(ceiling)),
		}
	}

	/// When to feed the current grant again, even if it has not changed.
	fn deadline(&self) -> Option<moq_clock::Instant> {
		self.control.deadline()
	}

	/// Feed `estimate` through the policy under `ceiling`, retuning through `retune`
	/// when the target moves. Returns the bitrate applied, or `Err` with the name of
	/// an encoder that cannot follow at all.
	fn step(
		&mut self,
		ceiling: moq_net::bandwidth::Rate,
		estimate: Option<moq_net::bandwidth::Rate>,
		now: moq_clock::Instant,
		retune: impl FnOnce(moq_net::bandwidth::Rate) -> Result<(), moq_video::Error>,
	) -> Result<Option<moq_net::bandwidth::Rate>, &'static str> {
		if ceiling != self.ceiling {
			*self = Self::new(ceiling);
		}
		let Some(bitrate) = self.control.update(estimate, now) else {
			return Ok(None);
		};
		match retune(bitrate) {
			Ok(()) => {
				tracing::debug!(bitrate = bitrate.as_bps(), "adjusted encoder bitrate");
				Ok(Some(bitrate))
			}
			Err(moq_video::Error::BitrateUnsupported(name)) => {
				tracing::warn!(encoder = name, "encoder cannot follow the bandwidth estimate");
				Err(name)
			}
			// The policy takes the target back and retries it at its deadline, so a
			// quiet grant still recovers.
			Err(err) => {
				tracing::warn!(error = %err, bitrate = bitrate.as_bps(), "failed to adjust encoder bitrate");
				self.control.failed(now);
				Ok(None)
			}
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
	/// Upper bound on buffering before skipping a stalled group, in
	/// microseconds. Same knob as
	/// [`MoqAudioDecoderOutput::max_delay_us`](crate::audio::MoqAudioDecoderOutput::max_delay_us).
	/// `None` keeps the moq-mux default of zero (skip aggressively).
	#[uniffi(default = None)]
	pub max_delay_us: Option<u64>,
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

struct VideoConsumerInner {
	consumer: moq_video::decode::Consumer,
}

impl VideoConsumerInner {
	async fn next(&mut self) -> Result<Option<Arc<MoqVideoDecodedFrame>>, MoqError> {
		let frame = self.consumer.read().await?;
		Ok(frame.map(|frame| Arc::new(MoqVideoDecodedFrame { frame })))
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
	pub async fn next(&self) -> Result<Option<Arc<MoqVideoDecodedFrame>>, MoqError> {
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
		options.max_delay = output
			.max_delay_us
			.map(std::time::Duration::from_micros)
			.unwrap_or_default();

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
		assert!(matches!(error, MoqError::Unsupported));
	}
}

#[cfg(test)]
mod finish_tests {
	use super::*;

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
				framerate: 30,
			},
			MoqVideoEncoderOutput {
				codec: MoqVideoCodec::H264,
				track: None,
				bitrate: Some(500_000),
				gop: None,
				kind: MoqVideoEncoderKind::Software,
			},
		)
		.unwrap();
		let picture = |i: u64| MoqVideoFrame {
			timestamp_us: i * 33_333,
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
		assert_eq!(keyframes, [0, 3 * 33_333], "keyframes at the start and the cut");
		assert!(packets.iter().all(|p| !p.data.is_empty()), "an empty access unit");
		assert!(matches!(encoder.encode(picture(4)), Err(MoqError::Closed)));
	}

	#[tokio::test]
	async fn finish_ends_the_track_after_a_rate_follower_panic() {
		let estimate = moq_net::bandwidth::Producer::new();
		let bandwidth = Arc::new(MoqBandwidth::new(moq_net::bandwidth::Allocator::new(
			estimate.consume(),
		)));
		let broadcast = crate::producer::MoqBroadcastProducer::new().unwrap();
		let input = MoqVideoEncoderInput {
			format: MoqVideoPixelFormat::Rgba,
			width: 320,
			height: 240,
			framerate: 30,
		};
		let output = MoqVideoEncoderOutput {
			codec: MoqVideoCodec::H264,
			track: Some("camera".into()),
			bitrate: Some(4_000_000),
			gop: None,
			kind: MoqVideoEncoderKind::Software,
		};
		let video = crate::video::MoqVideoProducer::new(
			&crate::media::MoqMediaCatalogProducer::new(broadcast.clone()).unwrap(),
			input,
			output,
			Some(bandwidth),
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

	use moq_net::bandwidth::Rate;

	use super::*;

	/// Feed `estimate` unchanged at every deadline until the follower settles.
	fn quiet(
		follower: &mut Follower,
		ceiling: Rate,
		estimate: Rate,
		retune: &mut impl FnMut(Rate) -> Result<(), moq_video::Error>,
	) {
		for _ in 0..1_000 {
			let Some(at) = follower.deadline() else { return };
			follower.step(ceiling, Some(estimate), at, &mut *retune).unwrap();
		}
		panic!("the deadline never settled");
	}

	/// The grant recovers once and never changes again, and the final raise fails once:
	/// the deadline alone still carries the encoder back to its ceiling.
	#[test]
	fn a_quiet_grant_retries_a_failed_raise() {
		let ceiling = Rate::from_mbps(4);
		let mut follower = Follower::new(ceiling);
		let mut applied = Vec::new();
		let mut refuse = Some(ceiling);
		let mut retune = |bitrate| {
			if refuse.take_if(|refused| *refused == bitrate).is_some() {
				return Err(moq_video::Error::Unsupported("a transient refusal".into()));
			}
			applied.push(bitrate);
			Ok(())
		};

		let start = moq_clock::now();
		follower
			.step(ceiling, Some(Rate::from_mbps(1)), start, &mut retune)
			.unwrap();
		follower
			.step(ceiling, Some(ceiling), start + Duration::from_secs(1), &mut retune)
			.unwrap();
		quiet(&mut follower, ceiling, ceiling, &mut retune);

		assert_eq!(refuse, None, "the final raise was never refused");
		assert_eq!(applied.last(), Some(&ceiling), "{applied:?}");
	}

	/// `set_bitrate` lowers the ceiling while a raise toward the old one is due. The
	/// follower reads the ceiling under the encoder lock, so the deadline sees the new
	/// cap and never retunes above it.
	#[test]
	fn a_lowered_ceiling_beats_a_due_deadline() {
		let old = Rate::from_mbps(4);
		let lowered = Rate::from_mbps(2);
		let mut follower = Follower::new(old);
		let mut applied = Vec::new();
		let mut retune = |bitrate| {
			applied.push(bitrate);
			Ok(())
		};

		let start = moq_clock::now();
		follower
			.step(old, Some(Rate::from_mbps(1)), start, &mut retune)
			.unwrap();
		follower
			.step(old, Some(old), start + Duration::from_secs(1), &mut retune)
			.unwrap();
		let due = follower.deadline().expect("a raise toward the old ceiling");

		// `set_bitrate` already retuned the encoder to the new cap.
		assert_eq!(follower.step(lowered, Some(old), due, &mut retune), Ok(None));
		assert_eq!(follower.deadline(), None);

		// A later attack and recovery climbs back to the new cap, not the old one.
		let later = due + Duration::from_secs(1);
		follower
			.step(lowered, Some(Rate::from_mbps(1)), later, &mut retune)
			.unwrap();
		follower
			.step(lowered, Some(old), later + Duration::from_secs(1), &mut retune)
			.unwrap();
		quiet(&mut follower, lowered, old, &mut retune);

		assert_eq!(applied.last(), Some(&lowered), "{applied:?}");
		assert!(applied[2..].iter().all(|bitrate| *bitrate <= lowered), "{applied:?}");
	}
}
