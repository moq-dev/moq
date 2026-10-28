//! Publish encoded video frames as a moq video track, with optional capture.
//!
//! Encoding is strictly on demand: the track and its catalog rendition are
//! advertised immediately (the rendition is probed from the encoder, since
//! nothing has been encoded yet), and the encoder itself only runs while a
//! subscriber is watching, or reading its HLS records. Capture opens its camera once at startup to learn
//! the mode it negotiates, then keeps it closed between viewers. This mirrors
//! `moq-boy`, which pauses its emulator while
//! [`Demand::wanted`](moq_net::track::Demand::wanted) is `None`.
//!
//! Capture advertises [`track::Info::refresh`](moq_net::track::Info::refresh) when its encoder
//! is sure to honor a cut, and forces a keyframe whenever a joining subscriber asks for a group
//! past the largest. The encoder's GOP stays the upper bound between keyframes.

use moq_mux::catalog::hang::CatalogExt;
#[cfg(feature = "capture")]
use moq_mux::rate;
#[cfg(any(feature = "capture", test))]
use moq_net::Timestamp;

use crate::Error;
#[cfg(feature = "capture")]
use crate::FrameRate;
#[cfg(feature = "capture")]
use crate::capture;

use super::Encoded;
#[cfg(feature = "capture")]
use super::Sink;
#[cfg(feature = "capture")]
use super::cuts::Cuts;
#[cfg(any(feature = "capture", test))]
use super::encoder;
#[cfg(feature = "capture")]
use super::encoder::Codec;

/// Last-resort framerate when neither the caller nor the camera reports one.
#[cfg(feature = "capture")]
const DEFAULT_FRAMERATE: FrameRate = FrameRate::integer(30);

/// The [`track::Info::refresh`](moq_net::track::Info::refresh) ratio a capture advertises: a
/// joiner forces a keyframe once a group's deltas outweigh twice its keyframe, so later joiners
/// start near one. The joiner that asks still reads the cached group meanwhile.
///
/// Measured on realtime CBR encodes (two-second GOPs at 2 to 4 Mbps), where a GOP's deltas run
/// three to five keyframes: one join a second halves the mean joiner download for 10 to 13%
/// more bitrate, and one every ten seconds costs about 1%. A ratio scales with the keyframe,
/// so it holds for every rendition.
#[cfg(feature = "capture")]
const REFRESH: u32 = 2;

/// Convert the probed rendition into the importer hint published before the first frame.
fn rendition_hint(rendition: hang::catalog::VideoConfig) -> moq_mux::catalog::VideoHint {
	let mut hint = moq_mux::catalog::VideoHint::default();
	hint.codec = Some(rendition.codec);
	hint.coded_width = rendition.coded_width;
	hint.coded_height = rendition.coded_height;
	hint.display_aspect_width = rendition.display_aspect_width;
	hint.display_aspect_height = rendition.display_aspect_height;
	hint.color = rendition.color;
	hint.framerate = rendition.framerate;
	hint.reorder = rendition.reorder;
	hint.bitrate = rendition.bitrate;
	hint.optimize_for_latency = rendition.optimize_for_latency;
	// Authoritative for both the catalog entry and the wire, so dropping it would silently
	// downgrade a caller's selection to the default.
	hint.container = rendition.container;
	hint.preference = rendition.preference;
	hint
}

/// Per-codec splitter + importer pair. Each codec frames its packets and resolves
/// its catalog rendition differently, so the producer holds one of these.
enum Codecs {
	H264 {
		split: moq_mux::codec::h264::Split,
		import: moq_mux::codec::h264::Import,
	},
	H265 {
		split: moq_mux::codec::h265::Split,
		import: moq_mux::codec::h265::Import,
	},
}

/// Publishes encoded video frames as a moq track (avc3 / hev1 depending on the
/// codec).
///
/// Built on the async side so the track is advertised (and the catalog
/// registered) before the camera opens; this is what lets a subscriber
/// trigger capture on demand. The `moq_mux::codec` importer for the codec
/// handles catalog registration and framing.
pub struct Producer {
	codecs: Codecs,
}

impl Producer {
	/// Publish a track carrying `rendition` into `broadcast`, registering it under
	/// `reserved`, a [`catalog::Producer::reserve`](moq_mux::catalog::Producer::reserve)
	/// that carries any per-track hint such as the encoder's GOP duration
	/// ([`Reserved::with_hls_hint`](moq_mux::catalog::Reserved::with_hls_hint)).
	/// The frames fed to [`publish`](Self::publish) must be in that
	/// codec's framing, which is what the [`Encoder`](super::Encoder) the
	/// rendition was probed from emits.
	///
	/// `rendition` comes from [`Config::probe`](super::Config::probe), so it is
	/// what the encoder will actually emit rather than a guess. It is published
	/// immediately, before anything is encoded, which is what lets a subscriber
	/// discover a track an on-demand encoder has not run for yet; because it
	/// already says what the first keyframe says, that keyframe confirms the
	/// catalog instead of correcting it.
	pub fn new<E: CatalogExt>(
		broadcast: moq_net::broadcast::Producer,
		reserved: moq_mux::catalog::Reserved<E>,
		rendition: hang::catalog::VideoConfig,
	) -> Result<Self, Error> {
		let suffix = match &rendition.codec {
			hang::catalog::VideoCodec::H264(_) => ".avc3",
			hang::catalog::VideoCodec::H265(_) => ".hev1",
			other => {
				return Err(Error::Codec(anyhow::anyhow!(
					"{other} is not a codec this producer can publish"
				)));
			}
		};
		let track = broadcast.unique_track(suffix, reserved.track_info(hang::catalog::PRIORITY.video))?;
		Self::with_track(track, reserved, rendition)
	}

	/// Publish `rendition` on an existing track, registering it under `reserved`.
	///
	/// Use this when the caller owns the track name. [`new`](Self::new) derives a
	/// unique name from the codec instead.
	pub fn with_track<E: CatalogExt>(
		track: moq_net::track::Producer,
		reserved: moq_mux::catalog::Reserved<E>,
		rendition: hang::catalog::VideoConfig,
	) -> Result<Self, Error> {
		let codecs = match &rendition.codec {
			hang::catalog::VideoCodec::H264(_) => Codecs::H264 {
				split: moq_mux::codec::h264::Split::new(),
				import: moq_mux::codec::h264::Import::new(track, reserved, rendition_hint(rendition))?,
			},
			hang::catalog::VideoCodec::H265(_) => Codecs::H265 {
				split: moq_mux::codec::h265::Split::new(),
				import: moq_mux::codec::h265::Import::new(track, reserved, rendition_hint(rendition))?,
			},
			// Unreachable via `Config::probe`, which only encodes what `Codec` covers.
			other => {
				return Err(Error::Codec(anyhow::anyhow!(
					"{other} is not a codec this producer can publish"
				)));
			}
		};
		Ok(Self { codecs })
	}

	/// A watch-only handle to the track's subscriber demand, created eagerly so
	/// subscription state is observable before any frames arrive. Watch it via
	/// [`wanted`](moq_net::track::Demand::wanted) / [`wanted_changed`](moq_net::track::Demand::wanted_changed).
	pub fn demand(&self) -> moq_net::track::Demand {
		self.track().demand()
	}

	/// The published track. A frame that changes the decoder identity, such as a reopen onto
	/// another mode or backend, moves the rendition to a newly named one.
	fn track(&self) -> &moq_net::track::Producer {
		match &self.codecs {
			Codecs::H264 { import, .. } => import.track(),
			Codecs::H265 { import, .. } => import.track(),
		}
	}

	/// The group a subscriber wants past the largest published, which the next frame should
	/// open with a keyframe, or `None` while the largest serves.
	#[cfg(feature = "capture")]
	fn wanted(&self) -> Option<u64> {
		let track = self.track();
		let wanted = track.demand().wanted()?;
		let largest = track.largest().map(|largest| largest.group);
		largest.is_none_or(|largest| wanted > largest).then_some(wanted)
	}

	/// Publish already-encoded frames, each at its own timestamp. Each frame is one
	/// whole access unit in the producer's codec framing.
	pub fn publish(&mut self, encoded: &[Encoded]) -> Result<(), Error> {
		for frame in encoded {
			let timestamp = Some(frame.timestamp);
			// The encoder emits one whole access unit per frame, so flush to emit it.
			match &mut self.codecs {
				Codecs::H264 { split, import } => {
					let mut frames = split.decode(&frame.payload, timestamp)?;
					frames.extend(split.flush(timestamp)?);
					import.decode(frames)?;
				}
				Codecs::H265 { split, import } => {
					let mut frames = split.decode(&frame.payload, timestamp)?;
					frames.extend(split.flush(timestamp)?);
					import.decode(frames)?;
				}
			}
		}
		Ok(())
	}

	/// Mark a break in the published timeline: whatever is published next does not continue
	/// what came before.
	///
	/// Call this when the encoder stops rather than merely pausing between frames -- a
	/// capture that goes idle, a source switch, anything that will resume on a re-anchored
	/// clock. See [`Producer::discontinuity`](moq_mux::container::Producer::discontinuity)
	/// for what the marker buys a consumer.
	pub fn discontinuity(&mut self) -> Result<(), Error> {
		match &mut self.codecs {
			Codecs::H264 { import, .. } => import.discontinuity()?,
			Codecs::H265 { import, .. } => import.discontinuity()?,
		}
		Ok(())
	}

	/// Advertise whether this rendition is being encoded, as the catalog's
	/// [`enabled`](hang::catalog::VideoConfig::enabled).
	///
	/// Disable it when the encoder stops for lack of bandwidth (a
	/// [`rate::Target::Disabled`](moq_mux::rate::Target::Disabled)), so viewers select a
	/// lower rendition instead of waiting for frames, and mark the
	/// [`discontinuity`](Self::discontinuity) too. The advertised rendition is otherwise
	/// unchanged, its maximum bitrate included. Enable it again with the keyframe that
	/// resumes it.
	pub fn set_enabled(&mut self, enabled: bool) -> Result<(), Error> {
		match &mut self.codecs {
			Codecs::H264 { import, .. } => import.set_enabled(enabled)?,
			Codecs::H265 { import, .. } => import.set_enabled(enabled)?,
		}
		Ok(())
	}

	/// Finalize the track.
	///
	/// Borrows rather than consumes, so a later [`abort`](Self::abort) can still
	/// run after a successful finish.
	pub fn finish(&mut self) -> Result<(), Error> {
		// Each publish flushed its access unit, so the splitter holds only input storage, which
		// ends here. The import stays, keeping the finished track and its rendition published.
		match &mut self.codecs {
			Codecs::H264 { split, import } => {
				*split = Default::default();
				import.finish()?
			}
			Codecs::H265 { split, import } => {
				*split = Default::default();
				import.finish()?
			}
		}
		Ok(())
	}

	/// Abort the track with `err` instead of finishing it cleanly, so subscribers
	/// see the real cause rather than [`moq_net::Error::Dropped`].
	///
	/// Consumes the producer. Still callable after [`finish`](Self::finish).
	pub fn abort(self, err: moq_net::Error) {
		match self.codecs {
			Codecs::H264 { import, .. } => import.abort(err),
			Codecs::H265 { import, .. } => import.abort(err),
		}
	}
}

/// Capture and encode settings for [`publish_capture`].
///
/// The encoded geometry (width / height / framerate) comes from the capture
/// source, not the caller. For the bring-your-own-frames
/// [`Encoder`](super::Encoder) path, where you specify geometry, use
/// [`Config`](super::Config) instead.
///
/// Frames are stamped on the catalog's [`clock`](moq_mux::catalog::Producer::clock), the one
/// its consumers are told about, so a concurrent audio capture on the same catalog stays aligned.
///
/// `#[non_exhaustive]`: construct via [`Capture::default`] and set fields, so
/// new settings can be added without breaking callers.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
#[cfg(feature = "capture")]
pub struct Capture {
	/// The source to capture.
	pub capture: capture::Config,
	/// Target bitrate; `None` derives one from the resolution.
	///
	/// This is a ceiling, not a fixed rate: with [`bandwidth`](Self::bandwidth)
	/// set, the encoder backs off below it while the uplink is congested and
	/// climbs back afterwards, but never exceeds it.
	pub bitrate: Option<moq_net::bandwidth::Rate>,
	/// Output codec. Defaults to [`Codec::H264`].
	pub codec: Codec,
	/// Encoder implementation preference.
	pub kind: encoder::Kind,
	/// The connection's bandwidth, as an allocator over
	/// [`Session::send_bandwidth`](moq_net::Session::send_bandwidth) (or
	/// `moq_tokio::Connection::send_bandwidth`, which survives reconnects).
	///
	/// Set it and the encoder reserves this track's ceiling, then tracks its share of
	/// the estimate per the default [`moq_mux::rate::Policy`], so a closing
	/// uplink gets a softer picture instead of a stalled one. Pass the same allocator
	/// to every sender on the connection, including the audio side: that's what keeps
	/// their bitrates summing to the uplink instead of each matching it.
	///
	/// Defaults to [`Allocator::unlimited`](moq_net::bandwidth::Allocator::unlimited),
	/// which holds [`bitrate`](Self::bitrate) regardless of congestion. That's what you
	/// want when the estimate isn't meaningful (a local file, a test harness) or
	/// unavailable (a publisher that only accepts inbound sessions).
	pub bandwidth: moq_net::bandwidth::Allocator,
}

/// The keyframe requests shared between the [`Control`]s and the [`Driver`].
///
/// One lock covers both fields, so a request is either counted before the driver
/// learns an encoder refuses cuts (and that encoder's opening keyframe serves it),
/// or refused outright. None can land unserved in between.
#[derive(Debug, Default)]
#[cfg(feature = "capture")]
struct Requests {
	/// A running count, so the driver coalesces any number between two frames into one.
	count: u64,
	/// The backend that cannot force a keyframe, once the driver has opened it.
	refused: Option<&'static str>,
}

/// A handle for controlling a running capture publish.
///
/// Clones control the same track. Dropping the final clone stops the [`Driver`]
/// and ends the track, even while it is still opening the source or an encoder.
#[derive(Clone, Debug)]
#[cfg(feature = "capture")]
pub(crate) struct Control {
	requests: kio::Producer<Requests>,
}

#[cfg(feature = "capture")]
impl Control {
	/// Register one video track and return its control handle and driver.
	///
	/// The track is registered here, but its catalog rendition describes the mode
	/// the source negotiates, so the driver opens the source once to probe it
	/// before publishing the rendition. Off macOS the driver's future is `Send`
	/// and can be spawned; on macOS the capture session is `!Send`, so await
	/// [`Driver::run`] on a local task there. [`Control`] itself is `Send + Sync`,
	/// so it can live anywhere.
	pub(crate) fn new<E: CatalogExt>(
		broadcast: moq_net::broadcast::Producer,
		catalog: moq_mux::catalog::Producer<E>,
		options: Capture,
	) -> Result<(Self, Driver<E>), Error> {
		let suffix = match options.codec {
			Codec::H264 => ".avc3",
			Codec::H265 => ".hev1",
		};
		// Advertised only when every backend the encoder may open honors a cut, since the info
		// is fixed before the driver learns which one it got.
		let refresh = super::backend::always_cuts(options.codec, &options.kind).then_some(REFRESH);
		let info = catalog.track_info(hang::catalog::PRIORITY.video).with_refresh(refresh);
		let track = broadcast.unique_track(suffix, info)?;
		let requests = kio::Producer::new(Requests::default());
		let driver = Driver {
			track,
			catalog,
			options,
			requests: requests.weak(),
		};
		Ok((Self { requests }, driver))
	}

	/// Request a keyframe, opening a new group at a frame no earlier than this call.
	///
	/// For a resume, a recording cut, or a known tune-in moment; [`Config::gop`](super::Config::gop)
	/// is the longest a group runs, and a joining subscriber already opens one through
	/// [`track::Info::refresh`](moq_net::track::Info::refresh). Requests between two frames
	/// coalesce into one keyframe on the next, with no spacing: a caller in a loop pins the
	/// encoder at all-IDR. A request while nothing is watching is served by the keyframe every
	/// fresh encoder opens with.
	///
	/// # Errors
	///
	/// Returns [`Error::CutUnsupported`] once the driver knows its backend cannot force a
	/// keyframe, and queues nothing: groups keep falling on the cadence and the publish carries
	/// on. The startup probe learns this before the first viewer, and each encoder the driver
	/// reopens updates it, since a reopen can land on a different backend. Before the probe
	/// finishes nothing is known, and the request is accepted because it is served anyway: no
	/// frame has been published yet, and the first one out of any encoder is a keyframe.
	#[cfg_attr(
		not(all(test, feature = "openh264")),
		allow(dead_code, reason = "only tests ask for a keyframe until a publisher needs to")
	)]
	pub(crate) fn cut(&self) -> Result<(), Error> {
		// Only a `close` fails the write, and nothing closes it; the last handle dropping does.
		let Ok(mut requests) = self.requests.write() else {
			return Ok(());
		};
		if let Some(name) = requests.refused {
			return Err(Error::CutUnsupported(name));
		}
		requests.count = requests.count.wrapping_add(1);
		Ok(())
	}
}

/// The task that captures, encodes, and publishes the track.
///
/// Runs until the track ends, the capture fails, or the final [`Control`] drops.
#[cfg(feature = "capture")]
pub(crate) struct Driver<E: CatalogExt = ()> {
	track: moq_net::track::Producer,
	catalog: moq_mux::catalog::Producer<E>,
	options: Capture,
	/// Weak, so the driver never keeps the controls' channel open itself.
	requests: kio::ProducerWeak<Requests>,
}

#[cfg(feature = "capture")]
impl<E: CatalogExt> std::fmt::Debug for Driver<E> {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Driver").finish_non_exhaustive()
	}
}

#[cfg(feature = "capture")]
impl<E: CatalogExt> Driver<E> {
	/// Run capture until the final control handle drops or the MoQ track ends.
	///
	/// The source is opened once at startup to probe the mode it negotiates, then released until a
	/// subscriber arrives and reopened for as long as one is watching. That one open is what lets
	/// the catalog rendition be exact before a single frame is published, so a consumer can size
	/// itself against it without waiting for an encoder that may never run.
	///
	/// Dropping the final [`Control`] ends it promptly wherever it is waiting, including a camera
	/// open or permission prompt still in flight, which is abandoned rather than finished.
	pub(crate) async fn run(self) -> Result<(), Error> {
		self.run_with(DeviceSource).await
	}

	async fn run_with<S: CaptureSource>(self, mut source: S) -> Result<(), Error> {
		let Self {
			track,
			catalog,
			options,
			requests,
		} = self;
		let clock = catalog.clock();
		let controls = requests.consume();

		// Open the camera once to find out what it actually negotiated, since a requested size is
		// only a hint (macOS ignores it outright) and the encoder is built from the mode, not the
		// request. It closes again immediately: this costs one camera open at startup and buys a
		// rendition that says exactly what the stream will carry, rather than one every consumer has
		// to treat as provisional.
		let rendition = async {
			let camera = source.open(&options.capture).await?;
			let mut probe_config = encoder::Config::new(
				camera.width(),
				camera.height(),
				options
					.capture
					.framerate
					.or_else(|| camera.framerate())
					.unwrap_or(DEFAULT_FRAMERATE),
			);
			probe_config.bitrate = options.bitrate;
			probe_config.codec = options.codec;
			probe_config.kind = options.kind.clone();
			probe_config.color = camera.color();
			let (rendition, mut sink) = probe_config.probe_sink().await?;
			// Learned here so a caller finds out before the first viewer, not on its first cut.
			learn(&requests, refusal(&mut sink).await?);
			// The encoder is built from the same defaults, so the probe's GOP is the one it emits.
			Ok::<_, Error>((rendition, probe_config.gop.duration(probe_config.framerate)))
		};
		// Every wait below races the controls closing, so an abandoned publish never holds a camera
		// or a permission prompt open for an owner that is gone.
		let rendition = tokio::select! {
			biased;
			() = controls.closed() => {
				// A track that already ended has nobody left to tell.
				let _ = track.finish();
				return Ok(());
			}
			rendition = rendition => rendition,
		};
		let (rendition, gop) = match rendition {
			Ok(probed) => probed,
			Err(err) => {
				// A track that already ended has nobody left to tell.
				let _ = track.abort(moq_net::Error::Transport(err.to_string()));
				return Err(err);
			}
		};

		let mut producer = Producer::with_track(track, catalog.reserve().with_hls_hint(gop), rendition)?;

		let result = tokio::select! {
			biased;
			() = controls.closed() => Ok(()),
			result = capture_loop(&mut producer, &mut source, &options, &clock, &requests) => result,
		};

		// This runs only when the loop ends on its own (the track is usually already
		// going away by then); a Ctrl+C cancels the future before this point, since
		// async `Drop` can't finalize the track.
		match &result {
			// Clean end (the track was dropped): best-effort finish.
			Ok(()) => {
				if let Err(err) = producer.finish() {
					tracing::debug!(error = %err, "video track finish after capture ended");
				}
			}
			// The capture loop failed: abort with the real cause so subscribers see it.
			Err(err) => producer.abort(moq_net::Error::Transport(err.to_string())),
		}
		result
	}
}

/// Capture a webcam and publish it as an on-demand video track.
///
/// The source is opened once at startup to probe the mode it negotiates, so the
/// catalog rendition is exact before a frame is published, then reopened only
/// while a subscriber is watching. Drop the future to stop it.
///
/// Returns when the broadcast is dropped (the track stops being announced) or
/// the capture fails. Frames are stamped on the catalog's clock, so a
/// concurrent audio publish into the same catalog stays aligned.
#[cfg(feature = "capture")]
pub async fn publish_capture<E: CatalogExt>(
	broadcast: moq_net::broadcast::Producer,
	catalog: moq_mux::catalog::Producer<E>,
	options: Capture,
) -> Result<(), Error> {
	// Held, not dropped: the driver ends as soon as the last control handle goes.
	let (_control, driver) = Control::new(broadcast, catalog, options)?;
	driver.run().await
}

/// Off macOS and iOS, [`publish_capture`]'s future must stay `Send` so a server can
/// `tokio::spawn` it: the encoder runs on its own thread and the capture guard
/// is `Send` there. This is never called; it exists only to fail compilation if
/// the future ever regains a `!Send` component. Apple platforms are exempt (the inline encoder and objc
/// capture session are `!Send`).
#[cfg(all(feature = "capture", not(apple)))]
#[allow(dead_code, reason = "a compile-time `Send` assertion, never called")]
fn assert_publish_capture_send(
	broadcast: moq_net::broadcast::Producer,
	catalog: moq_mux::catalog::Producer,
	options: Capture,
) {
	fn is_send<T: Send>(_: &T) {}
	is_send(&publish_capture(broadcast, catalog, options));
}

/// Where the driver opens its camera. Kept apart from the device backends so the
/// fixtures can drive the real driver from a synthetic or stalled source.
#[cfg(feature = "capture")]
trait CaptureSource {
	async fn open(&mut self, config: &capture::Config) -> Result<capture::Stream, Error>;
}

#[cfg(feature = "capture")]
struct DeviceSource;

#[cfg(feature = "capture")]
impl CaptureSource for DeviceSource {
	async fn open(&mut self, config: &capture::Config) -> Result<capture::Stream, Error> {
		capture::open(config).await
	}
}

/// The live rate control state. `None` once it has *retired*, which is the only
/// thing absence means now that every encoder has a share to read: an allocator
/// with nothing to divide grants `None` rather than being absent. Retiring stops
/// the `select!` arm from spinning on a channel that is permanently ready.
#[cfg(feature = "capture")]
type RateControl = Option<rate::Follower>;

/// Wait for the next target bitrate, or forever when rate control is off or finished.
///
/// Cancel-safe: [`Follower::poll_next`](rate::Follower::poll_next) keeps its state in
/// the follower, so losing this race to a frame drops no target.
#[cfg(feature = "capture")]
async fn next_target(rate: &mut RateControl) -> Result<rate::Target, moq_net::Error> {
	match rate {
		Some(follower) => follower.next().await,
		// Retired: park this arm forever so `select!` ignores it.
		None => std::future::pending().await,
	}
}

/// Retune the encoder to the next target.
///
/// An error means the grant is gone (the session ended for good), so rate control
/// retires and the encoder holds its current bitrate.
#[cfg(feature = "capture")]
async fn apply_target(encoder: &mut Sink, rate: &mut RateControl, target: Result<rate::Target, moq_net::Error>) {
	let Some(follower) = rate.as_mut() else { return };

	let Ok(target) = target else {
		tracing::debug!("bandwidth estimate ended; holding the current encoder bitrate");
		*rate = None;
		return;
	};
	let rate::Target::Enabled(bitrate) = target else {
		unreachable!("a capture follows its grant with no lower rendition, so it never disables")
	};

	match encoder.set_bitrate(bitrate).await {
		Ok(()) => tracing::debug!(bitrate = bitrate.as_bps(), "adjusted encoder bitrate"),
		// The encoder can't retune, so keep encoding at the rate it opened with
		// and stop asking. Dropping the source also stops the estimate arm, which
		// would otherwise wake this loop for nothing on every change.
		Err(Error::BitrateUnsupported(name)) => {
			tracing::warn!(encoder = name, "encoder cannot follow the bandwidth estimate");
			*rate = None;
		}
		// A transient failure: the follower takes the target back and retries it,
		// so a quiet grant still recovers.
		Err(err) => {
			tracing::warn!(error = %err, bitrate = bitrate.as_bps(), "failed to adjust encoder bitrate");
			follower.failed();
		}
	}
}

/// Whether `sink` refuses cuts, as the name [`Control::cut`] reports it under.
#[cfg(feature = "capture")]
async fn refusal(sink: &mut Sink) -> Result<Option<&'static str>, Error> {
	match sink.check_cut().await {
		Ok(()) => Ok(None),
		Err(Error::CutUnsupported(name)) => Ok(Some(name)),
		Err(err) => Err(err),
	}
}

/// Record whether the encoder just opened refuses cuts, returning the request count
/// its opening keyframe already serves.
#[cfg(feature = "capture")]
fn learn(requests: &kio::ProducerWeak<Requests>, refused: Option<&'static str>) -> u64 {
	// No controls left, so nobody can ask and the driver is on its way out.
	let Some(producer) = requests.produce() else { return 0 };
	let Ok(mut requests) = producer.write() else { return 0 };
	requests.refused = refused;
	requests.count
}

/// An application dropping or closing its producer ends publishing normally;
/// a specific abort (such as a transport reset) stays visible in diagnostics.
#[cfg(feature = "capture")]
fn log_track_ended(err: moq_net::Error) {
	if matches!(err, moq_net::Error::Dropped | moq_net::Error::Closed) {
		tracing::debug!("video track no longer announced; stopping capture");
	} else {
		tracing::warn!(error = %err, "video track aborted; stopping capture");
	}
}

#[cfg(any(feature = "capture", all(test, feature = "openh264")))]
fn capture_stopped(producer: &mut Producer) -> Result<(), Error> {
	// The shared clock keeps advancing while capture is stopped. Mark the break before waiting
	// for demand again so the next timestamp does not stretch the previous frame across the gap.
	producer.discontinuity()
}

/// Wait until some viewer wants the track, or none does.
#[cfg(feature = "capture")]
async fn until_wanted(demand: &moq_net::track::Demand, wanted: bool) -> Result<(), moq_net::Error> {
	let mut demand = demand.clone();
	while demand.wanted_changed().await?.is_some() != wanted {}
	Ok(())
}

// Race source setup or an encode against the last viewer leaving, so an unwatched track
// stops waiting promptly.
#[cfg(feature = "capture")]
async fn wait_capture<T>(
	demand: &moq_net::track::Demand,
	work: impl std::future::Future<Output = Result<T, Error>>,
) -> Result<Option<T>, Error> {
	tokio::select! {
		biased;
		res = until_wanted(demand, false) => {
			if let Err(err) = res {
				log_track_ended(err);
			}
			Ok(None)
		}
		res = work => res.map(Some),
	}
}

/// Async capture/encode loop. Opens the camera while at least one viewer is
/// watching and releases it when the last one leaves. It never returns on its
/// own when the controls close; [`Driver::run`] races it against that instead.
///
/// Cancel safety: every wait here is a real `.await` (a frame read, a demand
/// transition, or an encode), so dropping this future (e.g. on Ctrl+C) drops
/// `camera` and `encoder`, which release the device (LED off) and join the
/// encode thread. Both the capture and encode threads sit idle between frames,
/// so their joins return promptly unless the underlying device or encoder is
/// itself wedged.
#[cfg(feature = "capture")]
async fn capture_loop<S: CaptureSource>(
	producer: &mut Producer,
	source: &mut S,
	options: &Capture,
	clock: &moq_mux::Clock,
	requests: &kio::ProducerWeak<Requests>,
) -> Result<(), Error> {
	// This track's claim on the connection. Taken on the first open, because the
	// negotiated mode is what finally says how much this encoder can ever send, and
	// held across reopens so the claim doesn't lapse while the camera is closed.
	let mut reservation: Option<moq_net::bandwidth::Reservation> = None;
	let counted = requests.consume();
	let capture = &options.capture;
	// The published track's demand, followed onto the next track when a frame moves the rendition.
	let mut demand = producer.demand();
	let demand = &mut demand;

	loop {
		// Idle until a viewer subscribes; the track ending is a clean exit. The
		// catalog rendition was published when the track was created, so a
		// subscriber can get here without a frame ever having been encoded.
		if let Err(err) = until_wanted(demand, true).await {
			log_track_ended(err);
			return Ok(());
		}

		// Open the camera and an encoder sized to its negotiated mode.
		let Some(mut camera) = wait_capture(demand, source.open(capture)).await? else {
			continue;
		};
		// Capture timestamps use a private monotonic timeline. Sample both clocks
		// once at open so every queued frame maps to the shared broadcast epoch
		// without mistaking dequeue time for acquisition time.
		let capture_epoch =
			u64::try_from(clock.now().as_micros().saturating_sub(camera.now().as_micros())).unwrap_or(u64::MAX);
		// Prefer an explicit --fps, otherwise the camera's reported rate, falling
		// back only if the backend doesn't expose one.
		let framerate = capture
			.framerate
			.or_else(|| camera.framerate())
			.unwrap_or(DEFAULT_FRAMERATE);
		let mut encoder_config = encoder::Config::new(camera.width(), camera.height(), framerate);
		encoder_config.bitrate = options.bitrate;
		encoder_config.codec = options.codec;
		encoder_config.kind = options.kind.clone();
		encoder_config.color = camera.color();
		// Off macOS and iOS this opens the encoder on a dedicated thread; see `sink`.
		// No cut on reopen: a fresh encoder opens with a keyframe on every backend,
		// so the viewer whose subscription reopened the camera can decode from the
		// first frame regardless, and a backend that cannot cut still captures.
		let Some(mut encoder) = wait_capture(demand, Sink::open(&encoder_config)).await? else {
			continue;
		};
		// A reopen can land on another backend than the probe did, so the controls
		// learn this one's answer before any request is counted against it.
		let refused = refusal(&mut encoder).await?;
		let served = learn(requests, refused);
		tracing::info!(encoder = encoder.name(), device = camera.label(), "capturing");

		// A reopen can negotiate a different mode (a display resized while nothing was
		// subscribed), and the claim follows it: the old ceiling would otherwise cap a
		// larger mode below what it can send, or keep claiming room a smaller one no
		// longer needs.
		let ceiling = encoder_config.resolved_bitrate();
		let claim = reservation.get_or_insert_with(|| options.bandwidth.reserve(demand, ceiling));
		claim.update(ceiling);

		// Rate control is per encoder: this one opened at the configured bitrate,
		// so the policy's ceiling is that rate and the target starts there. A
		// reopened camera starts optimistic again rather than inheriting the
		// backed-off rate from whatever the link was doing last time.
		let mut rate = Some(rate::Follower::new(claim.consume(), rate::Policy::new(ceiling)));
		// Per encoder, since a fresh one opens with a keyframe that serves every earlier ask.
		let mut forced = Cuts::new(served);
		// Set when a frame moves the rendition to a newly named track.
		let mut moved = false;

		loop {
			// Race the next frame against the last viewer leaving so we release the
			// camera promptly when demand drops. `biased` checks demand first so an
			// unwatched track stops before reading another frame.
			let frame = tokio::select! {
				biased;
				res = until_wanted(demand, false) => {
					if let Err(err) = res {
						log_track_ended(err);
						return Ok(());
					}
					break; // no viewers: release the camera, then wait for one
				}
				// Retune between frames rather than mid-encode, and only when
				// the policy says the target actually moved.
				target = next_target(&mut rate) => {
					apply_target(&mut encoder, &mut rate, target).await;
					continue;
				}
				// A read error is terminal for this selection (the source is gone
				// or was refused); `None` just ends the stream, so reopen below.
				frame = camera.read() => frame?,
			};

			let Some(mut frame) = frame else { break };
			frame.timestamp = map_capture_timestamp(capture_epoch, frame.timestamp)?;
			let count = counted.read().count;
			// A backend that cannot cut leaves every group to its cadence.
			let wanted = refused.is_none().then(|| producer.wanted()).flatten();
			if forced.due(count, wanted) {
				// Never refused: `Control::cut` stops counting once this encoder is known to.
				encoder.cut().await?;
			}
			let Some(encoded) = wait_capture(demand, encoder.encode(frame)).await? else {
				break;
			};
			producer.publish(&encoded)?;
			// A frame whose decoder identity differs from the probed one (a reopen onto another
			// mode or backend) publishes under a new name and ends this track. Its viewers find
			// the new name in the catalog, so idle until one subscribes there.
			if producer.track().name() != demand.name() {
				moved = true;
				break;
			}
		}

		// Drop the camera (LED off) and encoder before waiting for the next viewer.
		drop(camera);
		drop(encoder);
		capture_stopped(producer)?;
		tracing::info!("capture stopped; released source");
		if moved {
			*demand = producer.demand();
			tracing::info!(track = %demand.name(), "capture moved to a new track");
			// The claim watches the old track's demand, so take a new one against the new track.
			reservation = None;
		}
	}
}

#[cfg(feature = "capture")]
fn map_capture_timestamp(epoch_micros: u64, timestamp: Timestamp) -> Result<Timestamp, Error> {
	let capture_micros = u64::try_from(timestamp.as_micros()).unwrap_or(u64::MAX);
	Ok(Timestamp::from_micros(epoch_micros.saturating_add(capture_micros))?)
}

#[cfg(test)]
mod tests {
	#![cfg_attr(
		not(feature = "openh264"),
		allow(dead_code, unused_imports, reason = "only the `openh264` feature uses it")
	)]
	#![cfg_attr(
		all(feature = "capture", feature = "openh264"),
		expect(
			clippy::disallowed_methods,
			clippy::disallowed_types,
			reason = "the capture tests read the host clock directly"
		)
	)]

	use moq_mux::catalog::Stream as _;

	use super::*;
	use crate::Frame;
	use crate::encode::{Codec, Config, Encoder};

	#[cfg(feature = "capture")]
	#[test]
	fn capture_clock_mapping_is_monotonic() {
		let first = map_capture_timestamp(10_000, Timestamp::from_micros(2_000).unwrap()).unwrap();
		let second = map_capture_timestamp(10_000, Timestamp::from_micros(2_001).unwrap()).unwrap();
		assert!(second > first);
	}

	/// Encode a handful of synthetic frames for `codec` and publish them through a real
	/// [`Producer`], returning the catalog rendition's track name and config.
	///
	/// Asserts the property the whole design rests on: the rendition published before anything is
	/// encoded is the one the first keyframe resolves. A guessed codec string would be corrected
	/// here; a probed one is confirmed, so the catalog is written once.
	///
	/// `kind` is explicit so the test picks a deterministic encoder rather than `Auto`, which on
	/// Linux CI would try the NVENC backend and panic in cudarc on a GPU-less runner.
	async fn roundtrip_rendition(codec: Codec, kind: encoder::Kind) -> (String, hang::catalog::VideoConfig) {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();

		let mut config = Config::new(320, 240, crate::FrameRate::new(30, 1).unwrap());
		config.codec = codec;
		config.kind = kind;

		let mut producer = Producer::new(
			broadcast,
			catalog.reserve().with_hls_hint(config.gop.duration(config.framerate)),
			config.probe().await.unwrap(),
		)
		.unwrap();
		let advertised = rendition(&catalog).expect("the rendition publishes before any frame").1;

		let mut encoder = Encoder::new(&config).unwrap();
		assert_eq!(encoder.codec(), codec);

		let rgba = vec![0x80u8; 320 * 240 * 4];
		for i in 0..10u64 {
			let surface = crate::Surface::rgba(&rgba, crate::Size::new(320, 240)).unwrap();
			let frame = Frame::new(surface, Timestamp::from_micros(i * 33_333).unwrap());
			producer.publish(&encoder.encode(&frame).unwrap()).unwrap();
		}
		producer.publish(&encoder.finish().unwrap()).unwrap();

		let (name, resolved) = rendition(&catalog).expect("the importer should have registered a video rendition");
		// Jitter and delay aside, which are measured from the frames rather than declared by either.
		let (mut before, mut after) = (advertised, resolved.clone());
		(before.jitter, before.delay) = (None, None);
		(after.jitter, after.delay) = (None, None);
		assert_eq!(
			before, after,
			"the first keyframe should confirm the advertised rendition, not correct it"
		);
		(name, resolved)
	}

	/// The catalog's single video rendition, if it has one yet.
	fn rendition(catalog: &moq_mux::catalog::Producer) -> Option<(String, hang::catalog::VideoConfig)> {
		let snapshot = catalog.snapshot();
		let (name, config) = snapshot.video.renditions.iter().next()?;
		Some((name.clone(), config.clone()))
	}

	async fn collect_groups(mut consumer: moq_net::track::Subscriber) -> Vec<usize> {
		let mut groups = Vec::new();
		while let Some(mut group) = consumer.recv_group().await.unwrap() {
			let mut frames = 0;
			while group.next_frame().await.unwrap().is_some() {
				frames += 1;
			}
			groups.push(frames);
		}
		groups
	}

	/// An on-demand capture resumes on the same wall clock after releasing its camera and encoder,
	/// so the idle transition must publish a marker group between the two runs. This uses synthetic
	/// frames and the software encoder to exercise the transition without capture hardware.
	#[tokio::test]
	#[cfg(feature = "openh264")]
	async fn idle_capture_publishes_a_discontinuity_before_resume() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		// The synthetic clock jumps ten seconds. Keep every fixture group readable until the
		// assertion instead of letting the default five-second publisher window evict the marker.
		let replay = std::time::Duration::from_secs(11);
		let track = broadcast
			.create_track(
				"video",
				catalog.track_info(hang::catalog::PRIORITY.video).with_max_age(replay),
			)
			.unwrap();
		let consumer = track.subscribe(moq_net::track::Subscription::default().with_max_delay(replay));

		let mut config = Config::new(320, 240, crate::FrameRate::new(30, 1).unwrap());
		config.kind = encoder::Kind::Software;
		let mut producer = Producer::with_track(
			track,
			catalog.reserve().with_hls_hint(config.gop.duration(config.framerate)),
			config.probe().await.unwrap(),
		)
		.unwrap();
		let mut encoder = Encoder::new(&config).unwrap();
		let rgba = vec![0x80u8; 320 * 240 * 4];

		for timestamp in [0, 10_000_000] {
			if timestamp > 0 {
				capture_stopped(&mut producer).unwrap();
			}
			encoder.cut().unwrap();
			let surface = crate::Surface::rgba(&rgba, crate::Size::new(320, 240)).unwrap();
			let frame = Frame::new(surface, Timestamp::from_micros(timestamp).unwrap());
			producer.publish(&encoder.encode(&frame).unwrap()).unwrap();
		}
		producer.finish().unwrap();

		assert_eq!(collect_groups(consumer).await, vec![1, 1, 1]);
	}

	#[tokio::test]
	#[cfg(feature = "openh264")]
	async fn source_resize_publishes_a_new_track() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let mut initial = Config::new(320, 240, crate::FrameRate::new(30, 1).unwrap());
		initial.kind = encoder::Kind::Software;
		let mut producer = Producer::new(
			broadcast,
			catalog.reserve().with_hls_hint(initial.gop.duration(initial.framerate)),
			initial.probe().await.unwrap(),
		)
		.unwrap();

		for (timestamp, config) in [
			(0, initial),
			(33_333, Config::new(640, 360, crate::FrameRate::new(30, 1).unwrap())),
		] {
			let mut config = config;
			config.kind = encoder::Kind::Software;
			let mut encoder = Encoder::new(&config).unwrap();
			encoder.cut().unwrap();
			let rgba = vec![0x80u8; usize::try_from(config.width * config.height * 4).unwrap()];
			let surface = crate::Surface::rgba(&rgba, crate::Size::new(config.width, config.height)).unwrap();
			let frame = Frame::new(surface, Timestamp::from_micros(timestamp).unwrap());
			producer.publish(&encoder.encode(&frame).unwrap()).unwrap();
			capture_stopped(&mut producer).unwrap();
		}

		// 640x360 takes a higher level than the probed 320x240, so it is a new track.
		let (name, rendition) = rendition(&catalog).expect("the resized rendition should be published");
		assert!(name.ends_with(".avc3/1"), "{name} is not a new track");
		assert_eq!(rendition.coded_width, Some(640));
		assert_eq!(rendition.coded_height, Some(360));
	}

	/// Regression: a caller's container and preference have to survive the config -> hint conversion.
	///
	/// [`VideoHint::container`](moq_mux::catalog::VideoHint::container) is authoritative for both the
	/// track writer and the published rendition, so a conversion that drops it silently downgrades
	/// the caller's selection to Legacy while the catalog still claims whatever it defaulted to. A
	/// dropped preference would turn a compatibility transcode into a peer of its source.
	#[tokio::test]
	#[cfg(feature = "openh264")]
	async fn a_selected_container_and_preference_survive_the_rendition_hint() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();

		let mut config = Config::new(320, 240, crate::FrameRate::new(30, 1).unwrap());
		// Software (openh264) so the test is deterministic and never touches a hardware backend.
		config.kind = encoder::Kind::Software;
		let mut selected = config.probe().await.unwrap();
		selected.container = hang::catalog::Container::Loc;
		selected.preference = -1;

		let _producer = Producer::new(broadcast, catalog.reserve(), selected).unwrap();

		let (_, published) = rendition(&catalog).expect("the rendition publishes before any frame");
		assert_eq!(published.container, hang::catalog::Container::Loc);
		assert_eq!(published.preference, -1);
	}

	/// Regression: the rendition has to reach the wire before anything is encoded.
	///
	/// A catalog reservation is held until the rendition resolves, and an unresolved one withholds
	/// the whole catalog from the broadcast. An encoder that runs only while watched then closes a
	/// cycle: the catalog waits on a keyframe, the keyframe waits on a subscriber, and the
	/// subscriber waits on the catalog. Nothing errors on either side; the publisher simply serves
	/// nothing, forever.
	#[tokio::test]
	#[cfg(feature = "openh264")]
	async fn the_rendition_reaches_the_wire_before_the_first_frame() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();

		let mut config = Config::new(1920, 1080, crate::FrameRate::new(30, 1).unwrap());
		config.bitrate = Some(moq_net::bandwidth::Rate::from_mbps(6));
		// Software (openh264) so the test is deterministic and never touches a hardware backend.
		config.kind = encoder::Kind::Software;
		let _producer = Producer::new(
			broadcast,
			catalog.reserve().with_hls_hint(config.gop.duration(config.framerate)),
			config.probe().await.unwrap(),
		)
		.unwrap();

		// Published, not merely staged: this reads the catalog track a subscriber would.
		let mut stream = moq_mux::catalog::Consumer::<()>::new(&consumer, moq_mux::catalog::CatalogFormat::Hang)
			.await
			.unwrap();
		let snapshot = stream.next().await.unwrap().expect("a catalog before any frame");

		let (name, rendition) = snapshot
			.video
			.renditions
			.iter()
			.next()
			.expect("the track must be discoverable before it has encoded anything");
		assert!(name.ends_with(".avc3"));

		// Read out of the encoder rather than guessed: the avc3 shape (parameter sets in band) and
		// the geometry it was opened at, which is what its first keyframe will carry.
		let hang::catalog::VideoCodec::H264(h264) = &rendition.codec else {
			panic!("expected H.264, got {}", rendition.codec)
		};
		assert!(h264.inline, "an avc3 track carries its parameter sets in band");
		assert_eq!(rendition.coded_width, Some(1920));
		assert_eq!(rendition.coded_height, Some(1080));
		// Neither is in the bitstream, so both come from the config that was probed.
		assert_eq!(rendition.framerate, Some(30.0));
		assert_eq!(rendition.bitrate, Some(6_000_000));
	}

	/// Finish leaves the handle, so abort can still run.
	#[tokio::test]
	#[cfg(feature = "openh264")]
	async fn abort_after_finish() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let mut config = Config::new(320, 240, crate::FrameRate::new(30, 1).unwrap());
		config.kind = encoder::Kind::Software;
		let track = broadcast
			.create_track("video", catalog.track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let mut subscriber = track.subscribe(None);
		let mut producer = Producer::with_track(
			track,
			catalog.reserve().with_hls_hint(config.gop.duration(config.framerate)),
			config.probe().await.unwrap(),
		)
		.unwrap();
		let mut encoder = Encoder::new(&config).unwrap();
		let rgba = vec![0x80u8; 320 * 240 * 4];
		let surface = crate::Surface::rgba(&rgba, crate::Size::new(320, 240)).unwrap();
		let frame = Frame::new(surface, Timestamp::from_micros(0).unwrap());
		producer.publish(&encoder.encode(&frame).unwrap()).unwrap();

		producer.finish().unwrap();
		assert!(subscriber.recv_group().await.unwrap().is_some());
		producer.abort(moq_net::Error::Cancel);
	}

	#[tokio::test]
	#[cfg(feature = "openh264")]
	async fn h264_roundtrip_publishes_avc3() {
		// Software (openh264) so the test is deterministic and never touches a
		// hardware backend.
		let (name, config) = roundtrip_rendition(Codec::H264, encoder::Kind::Software).await;
		assert_eq!(config.reorder, Some(0));
		assert!(name.ends_with(".avc3"));
		assert_eq!(config.coded_width, Some(320));
		assert_eq!(config.coded_height, Some(240));
	}

	/// H.265 has no software encoder, so this only runs where a hardware one
	/// exists (VideoToolbox on macOS, the only hardware backend on this target).
	#[cfg(apple)]
	#[tokio::test]
	async fn h265_roundtrip_publishes_hev1() {
		let (name, config) = roundtrip_rendition(Codec::H265, encoder::Kind::Hardware).await;
		assert!(name.ends_with(".hev1"));
		assert_eq!(config.coded_width, Some(320));
		assert_eq!(config.coded_height, Some(240));
	}

	/// Clock fixtures: the real capture loop, fed by a synthetic camera against a pinned
	/// broadcast clock, graded on the timestamps a subscriber reads back.
	///
	/// Each expectation is the acquisition instant measured on the broadcast clock. The loop
	/// samples the broadcast clock and then the camera's timeline when it opens a camera, so a
	/// published timestamp may land up to `SAMPLING` early, never late.
	#[cfg(all(feature = "capture", feature = "openh264"))]
	mod clock {
		use moq_clock::Wall;
		use std::time::Duration;

		use moq_clock::Instant;

		use super::*;
		use crate::capture::Synthetic;

		/// How early a mapped timestamp may land: the gap between the loop's two clock samples.
		const SAMPLING: Duration = Duration::from_millis(250);
		/// Rounding slack on the late side: each clock reading truncates to a microsecond.
		const ROUNDING: u64 = 2;
		/// Retain every fixture group, so a slow runner never evicts one before it is read.
		const RETAIN: Duration = Duration::from_secs(600);

		/// Hands the loop one fixture-supplied stream per camera open.
		struct Opens(tokio::sync::mpsc::UnboundedReceiver<capture::Stream>);

		impl CaptureSource for Opens {
			async fn open(&mut self, _config: &capture::Config) -> Result<capture::Stream, Error> {
				self.0
					.recv()
					.await
					.ok_or_else(|| Error::SourceUnavailable("the fixture stopped opening cameras".to_string()))
			}
		}

		struct Fixture {
			epoch: Instant,
			clock: moq_mux::Clock,
			catalog: moq_mux::catalog::Producer,
			consumer: moq_net::broadcast::Consumer,
			_broadcast: moq_net::broadcast::Producer,
			opens: tokio::sync::mpsc::UnboundedSender<capture::Stream>,
			stop: Option<tokio::sync::oneshot::Sender<()>>,
			task: tokio::task::JoinHandle<Result<(), Error>>,
		}

		impl Fixture {
			/// Start the capture loop on a broadcast whose clock began `behind` ago, at `wall`.
			async fn start(behind: Duration, wall: Wall) -> Self {
				let epoch = moq_clock::now()
					.checked_sub(behind)
					.expect("a monotonic clock that far back");
				let clock = moq_mux::Clock::at(epoch, wall).unwrap();
				let mut broadcast = moq_net::broadcast::Info::new().produce();
				let consumer = broadcast.consume();
				let config = moq_mux::catalog::Config::default()
					.with_clock(clock)
					.with_max_age(RETAIN);
				let catalog = moq_mux::catalog::Producer::new(&mut broadcast, config).unwrap();
				let track = broadcast
					.create_track(
						"video",
						catalog.track_info(hang::catalog::PRIORITY.video).with_max_age(RETAIN),
					)
					.unwrap();

				let mut probe = Config::new(320, 240, crate::FrameRate::new(30, 1).unwrap());
				probe.kind = encoder::Kind::Software;
				let mut producer = Producer::with_track(
					track,
					catalog.reserve().with_hls_hint(probe.gop.duration(probe.framerate)),
					probe.probe().await.unwrap(),
				)
				.unwrap();

				let (opens, rx) = tokio::sync::mpsc::unbounded_channel();
				let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
				// Local: a capture stream is `!Send` on macOS, so each test runs on a `LocalSet`.
				let task = tokio::task::spawn_local(async move {
					let mut source = Opens(rx);
					let options = Capture {
						kind: encoder::Kind::Software,
						..Capture::default()
					};
					// Held, so the loop has a channel to report each encoder's cut support into.
					let controls = kio::Producer::new(Requests::default());
					let requests = controls.weak();
					tokio::select! {
						res = capture_loop(
							&mut producer,
							&mut source,
							&options,
							&clock,
							&requests,
						) => res?,
						_ = stopped => {}
					}
					producer.finish()
				});

				Self {
					epoch,
					clock,
					catalog,
					consumer,
					_broadcast: broadcast,
					opens,
					stop: Some(stop),
					task,
				}
			}

			/// Subscribe to the video track, which is what opens the camera.
			async fn subscribe(&self) -> moq_mux::container::Consumer<moq_mux::catalog::hang::Container> {
				let snapshot = self.catalog.snapshot();
				let (name, rendition) = snapshot.video.renditions.iter().next().expect("the probed rendition");
				let container = moq_mux::catalog::hang::Container::try_from(rendition).unwrap();
				let track = self
					.consumer
					.track(name)
					.unwrap()
					.subscribe(moq_net::track::Subscription::default().with_max_delay(RETAIN))
					.await
					.unwrap();
				moq_mux::container::Consumer::new(track, container)
			}

			/// Plug in the camera the loop opens next.
			fn camera(&self) -> Synthetic {
				self.camera_sized(320, 240)
			}

			/// Plug in a camera of another mode for the loop to open next.
			fn camera_sized(&self, width: u32, height: u32) -> Synthetic {
				let (camera, stream) =
					Synthetic::open(crate::Size::new(width, height), crate::FrameRate::new(30, 1).unwrap());
				self.opens.send(stream).unwrap();
				camera
			}

			/// `instant` on the broadcast clock, in microseconds.
			fn at(&self, instant: Instant) -> u64 {
				u64::try_from(instant.duration_since(self.epoch).as_micros()).unwrap()
			}

			/// Stop the loop and finalize the track, as a clean end of capture does.
			async fn finish(mut self) -> (moq_mux::catalog::Producer, moq_net::broadcast::Consumer) {
				let _ = self.stop.take().expect("finished once").send(());
				self.task.await.unwrap().unwrap();
				(self.catalog, self.consumer)
			}

			/// A frame acquired at `captured` publishes at that instant on the broadcast clock.
			fn assert_acquired(&self, published: u64, captured: Instant) {
				let exact = self.at(captured);
				let early = u64::try_from(SAMPLING.as_micros()).unwrap();
				assert!(
					published + early >= exact && published <= exact + ROUNDING,
					"published {published}us, acquired at {exact}us on the broadcast clock"
				);
			}
		}

		pub(super) fn surface() -> crate::frame::Surface {
			surface_sized(320, 240)
		}

		fn surface_sized(width: u32, height: u32) -> crate::frame::Surface {
			crate::frame::Surface::I420(crate::frame::I420 {
				width,
				height,
				data: vec![0x80; usize::try_from(width * height * 3 / 2).unwrap()],
				color: None,
			})
		}

		pub(super) fn us(micros: u64) -> Timestamp {
			Timestamp::from_micros(micros).unwrap()
		}

		async fn read(track: &mut moq_mux::container::Consumer<moq_mux::catalog::hang::Container>) -> u64 {
			loop {
				let read = track.read().await.unwrap().expect("a published frame");
				if let moq_mux::container::Read::Frame { frame, .. } = read {
					return u64::try_from(frame.timestamp.as_micros()).unwrap();
				}
			}
		}

		/// Read the next frame not already in `seen`: a resubscription replays retained groups first.
		async fn read_new(
			track: &mut moq_mux::container::Consumer<moq_mux::catalog::hang::Container>,
			seen: &[u64],
		) -> u64 {
			loop {
				let timestamp = read(track).await;
				if !seen.contains(&timestamp) {
					return timestamp;
				}
			}
		}

		/// A camera whose first frame arrives long after the broadcast began stamps it at its
		/// acquisition: not zero, and not the later instant the loop dequeued it.
		#[tokio::test]
		async fn a_late_first_frame_publishes_its_acquisition() {
			tokio::task::LocalSet::new()
				.run_until(async {
					let fixture = Fixture::start(Duration::from_secs(5), moq_clock::wall()).await;
					let mut track = fixture.subscribe().await;
					let camera = fixture.camera();

					let captured = Instant::now();
					// Delivered well after acquisition: dequeue time must not leak into the timestamp.
					tokio::time::sleep(Duration::from_millis(50)).await;
					camera.push_at(surface(), captured);
					let published = read(&mut track).await;

					assert!(published >= 4_000_000, "{published}us restarted the broadcast at zero");
					fixture.assert_acquired(published, captured);
					fixture.finish().await;
				})
				.await
		}

		/// A device clock that restarts at zero, mid-stream or across a reopen, continues the
		/// broadcast forward with the device's spacing instead of rewinding it.
		#[tokio::test]
		async fn a_device_clock_restart_continues_forward() {
			tokio::task::LocalSet::new()
				.run_until(async {
					let fixture = Fixture::start(Duration::from_secs(1), moq_clock::wall()).await;
					let mut track = fixture.subscribe().await;
					let camera = fixture.camera();

					// The device numbers from zero, and real time keeps pace with it.
					camera.push_native(surface(), us(0));
					let first = read(&mut track).await;
					tokio::time::sleep(Duration::from_millis(40)).await;
					camera.push_native(surface(), us(40_000));
					let second = read(&mut track).await;
					assert_eq!(second - first, 40_000, "the device's spacing survives");

					// The device restarts its clock without the stream ending.
					camera.push_native(surface(), us(0));
					let restarted = read(&mut track).await;
					assert!(restarted >= second, "{restarted}us rewound behind {second}us");
					tokio::time::sleep(Duration::from_millis(40)).await;
					camera.push_native(surface(), us(40_000));
					let resumed = read(&mut track).await;
					assert_eq!(resumed - restarted, 40_000, "the device's spacing resumes");

					// The device goes away and comes back numbering from zero again.
					camera.close();
					let camera = fixture.camera();
					let pushed = Instant::now();
					camera.push_native(surface(), us(0));
					let reopened = read(&mut track).await;
					let arrived = fixture.at(Instant::now());
					assert!(reopened >= resumed, "{reopened}us rewound across the reopen");
					let early = u64::try_from(SAMPLING.as_micros()).unwrap();
					assert!(reopened + early >= fixture.at(pushed) && reopened <= arrived + ROUNDING);
					fixture.finish().await;
				})
				.await
		}

		/// Releasing the camera while nobody watches keeps the broadcast clock running: the
		/// frame after a resume lands after the real idle gap, at its own acquisition.
		#[tokio::test]
		async fn a_restart_after_idle_keeps_the_gap() {
			tokio::task::LocalSet::new()
				.run_until(async {
					let idle = Duration::from_millis(300);
					let fixture = Fixture::start(Duration::from_secs(1), moq_clock::wall()).await;

					let mut track = fixture.subscribe().await;
					let camera = fixture.camera();
					let captured = Instant::now();
					camera.push_at(surface(), captured);
					let before = read(&mut track).await;
					fixture.assert_acquired(before, captured);
					drop(track);
					drop(camera);

					tokio::time::sleep(idle).await;

					let mut track = fixture.subscribe().await;
					let camera = fixture.camera();
					let captured = Instant::now();
					camera.push_at(surface(), captured);
					let after = read_new(&mut track, &[before]).await;
					fixture.assert_acquired(after, captured);
					assert!(
						after - before >= u64::try_from(idle.as_micros()).unwrap(),
						"the {idle:?} idle gap collapsed to {}us",
						after - before
					);
					fixture.finish().await;
				})
				.await
		}

		/// A reopen onto a mode the probed level cannot carry is a new codec string, so it moves
		/// the rendition to a new track: the old one ends, and a viewer who follows the catalog
		/// there reopens the camera on it.
		#[tokio::test]
		async fn a_reopen_onto_a_new_level_follows_the_new_track() {
			tokio::task::LocalSet::new()
				.run_until(async {
					let fixture = Fixture::start(Duration::from_secs(1), moq_clock::wall()).await;
					let probed = fixture
						.catalog
						.snapshot()
						.video
						.renditions
						.keys()
						.next()
						.unwrap()
						.clone();
					let mut track = fixture.subscribe().await;
					let camera = fixture.camera();
					camera.push_native(surface(), us(0));
					read(&mut track).await;

					// The device goes away and comes back in a mode four times the size.
					camera.close();
					let camera = fixture.camera_sized(640, 480);
					camera.push_native(surface_sized(640, 480), us(40_000));
					// The probed track ends without the reopened camera's frames.
					while let Some(read) = track.read().await.unwrap() {
						assert!(
							!matches!(read, moq_mux::container::Read::Frame { .. }),
							"the reopened camera publishes on the probed track"
						);
					}

					let mut moved = fixture.subscribe().await;
					let name = fixture
						.catalog
						.snapshot()
						.video
						.renditions
						.keys()
						.next()
						.unwrap()
						.clone();
					assert_eq!(name, format!("{probed}/1"));
					let camera = fixture.camera_sized(640, 480);
					camera.push_native(surface_sized(640, 480), us(80_000));
					let first = read(&mut moved).await;
					let second = read_new(&mut moved, &[first]).await;
					assert!(second > first, "the new track carries the reopened camera");
					fixture.finish().await;
				})
				.await
		}

		/// The wall mapping is pinned when the broadcast clock is built. A system clock stepped
		/// an hour since then retimes neither the published timestamps nor the advertised mapping.
		#[tokio::test]
		async fn a_system_wall_adjustment_retimes_nothing() {
			tokio::task::LocalSet::new()
				.run_until(async {
					// Whole seconds, so the advertised mapping holds it exactly.
					let now = moq_clock::wall().checked_duration_since(Wall::UNIX_EPOCH).unwrap();
					let wall = Wall::UNIX_EPOCH + Duration::from_secs(now.as_secs() - 3600);
					let fixture = Fixture::start(Duration::from_secs(1), wall).await;
					let advertised = fixture.catalog.snapshot().clock;
					assert_eq!(advertised, fixture.clock.wall());

					let mut track = fixture.subscribe().await;
					let camera = fixture.camera();
					let captured = Instant::now();
					camera.push_at(surface(), captured);
					let published = read(&mut track).await;

					// Timestamps follow the monotonic epoch and map to walls under the pinned mapping.
					fixture.assert_acquired(published, captured);
					let mapped = advertised.unwrap().wall_clock(us(published)).unwrap();
					assert_eq!(mapped, wall + Duration::from_micros(published));
					assert_eq!(fixture.catalog.snapshot().clock, advertised);
					fixture.finish().await;
				})
				.await
		}

		/// HLS lists what the live edge published: the records carry the live timestamps across an
		/// idle restart, with the idle gap left out of every segment.
		#[tokio::test]
		async fn hls_records_keep_the_live_timestamps() {
			tokio::task::LocalSet::new()
				.run_until(async {
					let fixture = Fixture::start(Duration::from_secs(1), moq_clock::wall()).await;
					// The encoder's GOP hint declares the duration up front.
					let hls = fixture.catalog.snapshot().video.renditions["video"]
						.hls
						.clone()
						.expect("the video track advertises its HLS records");

					let mut live = Vec::new();
					for _ in 0..2 {
						let mut track = fixture.subscribe().await;
						let camera = fixture.camera();
						let captured = Instant::now();
						camera.push_at(surface(), captured);
						let published = read_new(&mut track, &live).await;
						fixture.assert_acquired(published, captured);
						live.push(published);
						drop(track);
						// Idle past the minimum segment, so each run is listed as its own segment.
						tokio::time::sleep(moq_mux::hls::DEFAULT_DURATION + Duration::from_millis(100)).await;
					}

					let (mut catalog, consumer) = fixture.finish().await;
					catalog.finish().unwrap();
					// Read only now: a record reader is demand too, and would have kept the capture
					// from idling between the runs.
					let mut records = moq_mux::hls::Consumer::subscribe(&consumer, &hls).await.unwrap();
					let mut listed = Vec::new();
					while let Some(event) = records.next().await.unwrap() {
						match event {
							moq_mux::hls::Event::Push { entry, .. } => listed.push(entry),
							other => panic!("unexpected record event {other:?}"),
						}
					}

					// Each run is one segment, stopped where its content reached.
					assert_eq!(listed.len(), 2 * live.len(), "a segment and a stop per run: {listed:?}");
					for (pair, live) in listed.chunks(2).zip(&live) {
						assert!(!pair[0].is_stop() && pair[1].is_stop(), "{listed:?}");
						// The records keep millisecond precision.
						assert_eq!(pair[0].pts.as_micros() / 1000, u128::from(*live / 1000), "{listed:?}");
					}
					assert!(
						listed[2].pts.as_micros() >= listed[1].pts.as_micros(),
						"the resumed segment overlaps the one before it: {listed:?}"
					);
				})
				.await
		}
	}

	/// Control fixtures: the real driver against sources that answer, or never do, on cue.
	#[cfg(all(feature = "capture", feature = "openh264"))]
	mod control {
		use std::collections::VecDeque;
		use std::sync::Arc;
		use std::sync::atomic::{AtomicUsize, Ordering};
		use std::time::Duration;

		use super::clock::{surface, us};
		use super::*;
		use crate::capture::Synthetic;
		use crate::encode::backend::probe;

		/// Hands out one queued stream per open, then stalls like a permission prompt nobody answers.
		struct Streams {
			queued: VecDeque<capture::Stream>,
			opens: Arc<AtomicUsize>,
		}

		impl Streams {
			fn new(queued: impl IntoIterator<Item = capture::Stream>) -> Self {
				Self {
					queued: queued.into_iter().collect(),
					opens: Arc::default(),
				}
			}
		}

		impl CaptureSource for Streams {
			async fn open(&mut self, _config: &capture::Config) -> Result<capture::Stream, Error> {
				self.opens.fetch_add(1, Ordering::SeqCst);
				match self.queued.pop_front() {
					Some(stream) => Ok(stream),
					None => std::future::pending().await,
				}
			}
		}

		fn camera() -> (Synthetic, capture::Stream) {
			Synthetic::open(crate::Size::new(320, 240), crate::FrameRate::new(30, 1).unwrap())
		}

		async fn until(done: impl Fn() -> bool) {
			while !done() {
				tokio::task::yield_now().await;
			}
		}

		/// What the driver is fed next, one at a time.
		enum Step {
			/// A frame 10ms after the previous one, waited on until the first subscriber has it.
			Frame(crate::frame::Surface),
			/// A keyframe request through the public control.
			Cut,
			/// Another subscriber joining at the live edge, held until the end.
			Join,
		}

		/// A frame of seeded noise, which no encoder can predict from the one before it.
		fn noise(seed: u32) -> crate::frame::Surface {
			let mut state = seed.wrapping_mul(2_654_435_761) | 1;
			let data = (0..320 * 240 * 3 / 2)
				.map(|_| {
					state ^= state << 13;
					state ^= state >> 17;
					state ^= state << 5;
					120 + (state as u8 >> 4)
				})
				.collect();
			crate::frame::Surface::I420(crate::frame::I420 {
				width: 320,
				height: 240,
				data,
				color: None,
			})
		}

		/// Drive the real driver through `steps`, returning the info the track advertised and the
		/// payload size of every frame the first subscriber read, grouped as it read them.
		async fn capture_groups(steps: Vec<Step>) -> (moq_net::track::Info, Vec<Vec<usize>>) {
			use moq_mux::container::Container as _;

			// Off Apple, encoding waits on an OS thread. Keep Tokio from auto-advancing
			// the paused clock to a timer deadline while that thread answers.
			let (hold, released) = tokio::sync::oneshot::channel::<()>();
			let clock = tokio::task::spawn_blocking(move || {
				let _ = released.blocking_recv();
			});
			let started = tokio::time::Instant::now();
			// Capture streams are !Send on macOS.
			let result = tokio::task::LocalSet::new()
				.run_until(async {
					let mut broadcast = moq_net::broadcast::Info::new().produce();
					let consumer = broadcast.consume();
					let catalog =
						moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
					let options = Capture {
						kind: encoder::Kind::Software,
						// Room for noise, which rate control would otherwise skip frames of.
						bitrate: Some(moq_net::bandwidth::Rate::from_mbps(4)),
						..Capture::default()
					};
					let (control, driver) = Control::new(broadcast.clone(), catalog, options).unwrap();

					// Ten nominal fps makes the default two-second GOP twenty frames, which every
					// script below stays inside.
					let rate = crate::FrameRate::new(10, 1).unwrap();
					let (_probe, probe) = Synthetic::open(crate::Size::new(320, 240), rate);
					let (camera, stream) = Synthetic::open(crate::Size::new(320, 240), rate);
					let run = tokio::task::spawn_local(driver.run_with(Streams::new([probe, stream])));
					let mut catalog =
						moq_mux::catalog::Consumer::<()>::new(&consumer, moq_mux::catalog::CatalogFormat::Hang)
							.await
							.unwrap();
					let (name, mut subscriber, format) = loop {
						let snapshot = catalog.next().await.unwrap().expect("a catalog");
						if let Some((name, rendition)) = snapshot.video.renditions.iter().next() {
							break (
								name.clone(),
								consumer.track(name).unwrap().subscribe(None).await.unwrap(),
								moq_mux::catalog::hang::Container::try_from(rendition).unwrap(),
							);
						}
					};
					let info = subscriber.info().clone();

					let (published, mut received) = tokio::sync::mpsc::unbounded_channel();
					let collect = tokio::task::spawn_local(async move {
						let mut groups = Vec::new();
						while let Some(mut group) = subscriber.recv_group().await.unwrap() {
							let mut frames = Vec::new();
							while let Some(wire) = group.read_frame().await.unwrap() {
								for frame in format.decode(wire).unwrap() {
									// A duration marker closes the group but is not an encoded picture.
									if format.end(&frame).is_none() {
										frames.push(frame.payload.len());
										published.send(()).unwrap();
									}
								}
							}
							groups.push(frames);
						}
						groups
					});

					let mut joined = Vec::new();
					let mut millis = 0;
					for step in steps {
						match step {
							Step::Frame(surface) => {
								camera.push_native(surface, us(millis * 1000));
								millis += 10;
								// Wait for delivery before the next step, which also keeps the
								// capture's latest slot from being replaced.
								received.recv().await.expect("the frame reached the subscriber");
							}
							Step::Cut => control.cut().unwrap(),
							Step::Join => joined.push(consumer.track(&name).unwrap().subscribe(None).await.unwrap()),
						}
					}
					drop(control);
					run.await.unwrap().unwrap();
					(info, collect.await.unwrap())
				})
				.await;
			assert_eq!(started.elapsed(), Duration::ZERO);
			drop(hold);
			clock.await.unwrap();
			result
		}

		/// Each request opens a group at the next frame, with no spacing between them.
		#[tokio::test(start_paused = true)]
		async fn capture_cuts_land_on_the_next_frame() {
			let steps = vec![
				Step::Frame(surface()),
				Step::Cut,
				Step::Frame(surface()),
				Step::Frame(surface()),
				Step::Cut,
				Step::Frame(surface()),
				Step::Frame(surface()),
			];
			let (_, groups) = capture_groups(steps).await;
			let lengths: Vec<_> = groups.iter().map(Vec::len).collect();
			assert_eq!(lengths, [1, 2, 2]);
		}

		/// A capture whose encoder always cuts advertises the refresh ratio, and a join opens a
		/// group at the next frame only once the deltas outweigh it, well within 500ms of the
		/// keyframe before.
		#[tokio::test(start_paused = true)]
		async fn a_join_over_the_refresh_ratio_forces_a_keyframe() {
			let mut steps = vec![
				Step::Frame(noise(0)),
				Step::Frame(surface()),
				Step::Frame(surface()),
				// Two flat deltas cost next to nothing against a noise keyframe.
				Step::Join,
				Step::Frame(surface()),
			];
			steps.extend((1..=10).map(|seed| Step::Frame(noise(seed))));
			steps.extend([Step::Join, Step::Frame(surface()), Step::Frame(surface())]);
			let (info, groups) = capture_groups(steps).await;
			assert_eq!(info.refresh, Some(REFRESH));

			let lengths: Vec<_> = groups.iter().map(Vec::len).collect();
			assert_eq!(lengths, [14, 2], "only the second join cut, at the frame after it");
			let ratio = usize::try_from(REFRESH).unwrap();
			let first = &groups[0];
			assert!(
				first[1..3].iter().sum::<usize>() <= ratio * first[0],
				"the first join was under the ratio: {first:?}"
			);
			assert!(
				first[1..].iter().sum::<usize>() > ratio * first[0],
				"the second join was over the ratio: {first:?}"
			);
		}

		/// Refresh is advertised only when every backend the encoder may open honors a cut, since
		/// the track info is fixed before the driver learns which one it got.
		#[test]
		fn a_backend_that_may_refuse_cuts_advertises_no_refresh() {
			for (kind, refresh) in [
				(encoder::Kind::Software, Some(REFRESH)),
				(encoder::Kind::Named(probe::NO_CUT.into()), None),
			] {
				let mut broadcast = moq_net::broadcast::Info::new().produce();
				let catalog =
					moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
				let options = Capture {
					kind,
					..Capture::default()
				};
				let (_control, driver) = Control::new(broadcast.clone(), catalog, options).unwrap();
				assert_eq!(driver.track.info().refresh, refresh);
			}
		}

		/// An HLS export subscribes only to a rendition's records, never its media, yet that alone
		/// opens the camera and records the segment it captures.
		#[tokio::test(start_paused = true)]
		async fn an_hls_only_viewer_starts_capture() {
			// Off Apple, encoding waits on an OS thread. Keep Tokio from auto-advancing
			// the paused clock to a timer deadline while that thread answers.
			let (hold, released) = tokio::sync::oneshot::channel::<()>();
			let clock = tokio::task::spawn_blocking(move || {
				let _ = released.blocking_recv();
			});
			// Capture streams are !Send on macOS.
			tokio::task::LocalSet::new()
				.run_until(async {
					let mut broadcast = moq_net::broadcast::Info::new().produce();
					let consumer = broadcast.consume();
					let catalog =
						moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
					let options = Capture {
						kind: encoder::Kind::Software,
						..Capture::default()
					};
					let (control, driver) = Control::new(broadcast.clone(), catalog, options).unwrap();

					let rate = crate::FrameRate::new(10, 1).unwrap();
					let (_probe, probe) = Synthetic::open(crate::Size::new(320, 240), rate);
					let (camera, stream) = Synthetic::open(crate::Size::new(320, 240), rate);
					let source = Streams::new([probe, stream]);
					let opens = source.opens.clone();
					let run = tokio::task::spawn_local(driver.run_with(source));

					// The capture is hinted, so the catalog names its records before any segment.
					let mut catalog =
						moq_mux::catalog::Consumer::<()>::new(&consumer, moq_mux::catalog::CatalogFormat::Hang)
							.await
							.unwrap();
					let hls = loop {
						let snapshot = catalog.next().await.unwrap().expect("a catalog");
						if let Some(hls) = snapshot.video.renditions.values().find_map(|config| config.hls.clone()) {
							break hls;
						}
					};
					assert_eq!(opens.load(Ordering::SeqCst), 1, "only the probe opened the camera");

					let mut records = moq_mux::hls::Consumer::subscribe(&consumer, &hls).await.unwrap();
					camera.push_native(surface(), us(0));
					let event = records.next().await.unwrap().expect("a record");
					assert!(
						matches!(event, moq_mux::hls::Event::Push { index: 0, .. }),
						"unexpected event {event:?}"
					);
					assert_eq!(opens.load(Ordering::SeqCst), 2, "the records' reader opened the camera");

					drop(control);
					run.await.unwrap().unwrap();
				})
				.await;
			drop(hold);
			clock.await.unwrap();
		}

		/// Drop the controls while the driver sits in its `opens`th open, which never finishes.
		async fn abandon(
			control: Control,
			run: impl std::future::Future<Output = Result<(), Error>>,
			opens: &AtomicUsize,
			expected: usize,
		) {
			let mut run = std::pin::pin!(run);
			tokio::select! {
				res = &mut run => panic!("the driver ended before the stalled open: {res:?}"),
				() = until(|| opens.load(Ordering::SeqCst) == expected) => {}
			}
			// Nothing has refused a cut: the probe never finished, or its backend can force one.
			control.cut().unwrap();
			drop(control);
			tokio::time::timeout(Duration::from_secs(1), run)
				.await
				.expect("dropping the controls ends the driver")
				.unwrap();
		}

		/// A startup probe stuck in its camera open still ends with the controls.
		#[tokio::test(start_paused = true)]
		async fn dropping_the_controls_abandons_the_probe() {
			let mut broadcast = moq_net::broadcast::Info::new().produce();
			let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
			let (control, driver) = Control::new(broadcast.clone(), catalog, Capture::default()).unwrap();

			let source = Streams::new([]);
			let opens = source.opens.clone();
			abandon(control, driver.run_with(source), &opens, 1).await;
		}

		/// A subscriber's camera open stuck in flight still ends with the controls.
		#[tokio::test(start_paused = true)]
		async fn dropping_the_controls_abandons_an_open() {
			let mut broadcast = moq_net::broadcast::Info::new().produce();
			let consumer = broadcast.consume();
			let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
			let options = Capture {
				kind: encoder::Kind::Software,
				..Capture::default()
			};
			let (control, driver) = Control::new(broadcast.clone(), catalog.clone(), options).unwrap();

			// The probe gets a camera; the subscriber's open after it never finishes.
			let (_camera, stream) = camera();
			let source = Streams::new([stream]);
			let opens = source.opens.clone();
			let mut run = std::pin::pin!(driver.run_with(source));

			let subscribe = async {
				let mut stream =
					moq_mux::catalog::Consumer::<()>::new(&consumer, moq_mux::catalog::CatalogFormat::Hang)
						.await
						.unwrap();
				loop {
					let snapshot = stream.next().await.unwrap().expect("a catalog");
					if let Some(name) = snapshot.video.renditions.keys().next() {
						return consumer.track(name).unwrap().subscribe(None).await.unwrap();
					}
				}
			};
			let _subscriber = tokio::select! {
				res = &mut run => panic!("the driver ended before a subscriber arrived: {res:?}"),
				subscriber = subscribe => subscriber,
			};

			abandon(control, run, &opens, 2).await;
		}

		/// A keyframe request the backend cannot force is refused to the caller, and the capture
		/// carries on at its cadence rather than ending or dropping the request without a word.
		#[tokio::test]
		#[expect(
			clippy::await_holding_lock,
			reason = "the guard only keeps other tests out of the process-wide probe log, and this runtime has one thread, so holding it across an await blocks nothing"
		)]
		async fn a_cut_the_backend_cannot_force_is_refused() {
			let _probe = probe::exclusive();

			let mut broadcast = moq_net::broadcast::Info::new().produce();
			let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
			let track = broadcast
				.create_track("video", catalog.track_info(hang::catalog::PRIORITY.video))
				.unwrap();
			let _subscriber = track.subscribe(None);
			let mut rendition = Config::new(320, 240, crate::FrameRate::new(30, 1).unwrap());
			rendition.kind = encoder::Kind::Software;
			let mut producer = Producer::with_track(
				track,
				catalog
					.reserve()
					.with_hls_hint(rendition.gop.duration(rendition.framerate)),
				rendition.probe().await.unwrap(),
			)
			.unwrap();

			let (camera, stream) = camera();
			let mut source = Streams::new([stream]);
			let options = Capture {
				kind: encoder::Kind::Named(probe::NO_CUT.into()),
				..Capture::default()
			};
			let control = Control {
				requests: kio::Producer::new(Requests::default()),
			};
			let requests = control.requests.weak();
			let clock = catalog.clock();
			let mut run = std::pin::pin!(capture_loop(&mut producer, &mut source, &options, &clock, &requests,));

			// Nothing is known before an encoder opens, and its opening keyframe serves the request.
			control.cut().unwrap();
			tokio::select! {
				res = &mut run => panic!("the capture ended before its encoder opened: {res:?}"),
				() = until(|| control.requests.read().refused.is_some()) => {}
			}
			let err = control.cut().expect_err("the backend cannot cut");
			assert!(
				matches!(err, Error::CutUnsupported(name) if name == probe::NO_CUT),
				"unexpected error: {err:?}"
			);

			// The refusal ends nothing: the next frame still reaches the codec.
			camera.push_native(surface(), us(0));
			tokio::select! {
				res = &mut run => panic!("the capture ended after a refused cut: {res:?}"),
				() = until(|| probe::take().iter().any(|(event, _)| *event == "encode")) => {}
			}
		}
	}

	/// Rate control against the probe backend, which logs every bitrate it accepts.
	#[cfg(feature = "capture")]
	mod rate_control {
		use std::time::Duration;

		use moq_net::bandwidth::Rate;

		use super::*;
		use crate::encode::backend::probe;

		/// After congestion clears, the encoder climbs back to its ceiling although the grant
		/// never changes again, and a failed step is retried rather than waiting for one.
		#[tokio::test(start_paused = true)]
		#[expect(
			clippy::await_holding_lock,
			reason = "the guard only keeps other tests out of the process-wide probe log, and this runtime has one thread, so holding it across an await blocks nothing"
		)]
		async fn a_quiet_grant_still_recovers_the_ceiling() {
			let _probe = probe::exclusive();
			let ceiling = Rate::from_mbps(4);
			let mut config = encoder::Config::new(320, 240, crate::FrameRate::new(30, 1).unwrap());
			config.kind = encoder::Kind::Named(probe::NAME.into());
			config.bitrate = Some(ceiling);
			let mut sink = Sink::open(&config).await.unwrap();

			let estimate = moq_net::bandwidth::Producer::new();
			let mut rate = Some(rate::Follower::new(estimate.consume(), rate::Policy::new(ceiling)));

			estimate.set(Some(Rate::from_mbps(1))).unwrap();
			let next = next_target(&mut rate).await;
			apply_target(&mut sink, &mut rate, next).await;

			// The last grant change: the link recovers a second later.
			tokio::time::advance(Duration::from_secs(1)).await;
			estimate.set(Some(ceiling)).unwrap();
			probe::refuse_once(ceiling.as_bps());

			// A minute without a wake is far past anything the ramp still needs.
			while let Ok(next) = moq_clock::timeout(Duration::from_secs(60), next_target(&mut rate)).await {
				apply_target(&mut sink, &mut rate, next).await;
			}
			let bitrates = probe::bitrates();
			assert_eq!(bitrates.first(), Some(&1_000_000), "{bitrates:?}");
			assert_eq!(
				bitrates.last(),
				Some(&ceiling.as_bps()),
				"the ramp stalled: {bitrates:?}"
			);
			assert!(
				probe::take()
					.iter()
					.filter(|(event, _)| *event == "set_bitrate")
					.count() > bitrates.len()
			);
			assert!(rate.is_some(), "a transient failure keeps rate control running");
		}
	}
}

/// A finished [`Producer`] keeps its track and rendition published, but not its splitter.
///
/// Bindings keep finished video producers until their broadcast ends, so a long-lived broadcast
/// that retires many encoders would otherwise hold each one's largest access unit. The counting
/// allocator serves this crate's whole unit-test binary, but it counts only on a thread inside
/// [`released`], so other tests are unaffected.
#[cfg(test)]
mod finished {
	use std::alloc::{GlobalAlloc, Layout, System};
	use std::cell::Cell;

	use super::*;

	/// The access unit each source publishes, far above the bookkeeping a finished producer keeps.
	const PAD: usize = 1 << 20;

	thread_local! {
		/// Bytes this thread freed minus those it allocated while measuring. Per thread so concurrent
		/// tests don't mix, and `const` so the allocator can read it without allocating.
		static RELEASED: Cell<Option<isize>> = const { Cell::new(None) };
	}

	fn count(bytes: isize) {
		// `try_with` because TLS is unavailable while a thread is being torn down.
		let _ = RELEASED.try_with(|released| released.set(released.get().map(|n| n + bytes)));
	}

	/// Counts on the measuring thread; every call forwards to the system allocator unchanged.
	struct Counting;

	unsafe impl GlobalAlloc for Counting {
		unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
			count(-(layout.size() as isize));
			unsafe { System.alloc(layout) }
		}

		unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
			count(layout.size() as isize);
			unsafe { System.dealloc(ptr, layout) }
		}

		unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
			count(layout.size() as isize - new_size as isize);
			unsafe { System.realloc(ptr, layout, new_size) }
		}
	}

	#[global_allocator]
	static ALLOCATOR: Counting = Counting;

	/// The heap `f` gives back, net of what it allocates on the way.
	fn released(f: impl FnOnce()) -> isize {
		RELEASED.with(|released| released.set(Some(0)));
		f();
		RELEASED.with(|released| released.replace(None).unwrap())
	}

	/// An H.264 IDR access unit carrying its SPS and PPS, padded to [`PAD`] with filler data.
	fn keyframe() -> bytes::Bytes {
		let mut au = vec![0, 0, 0, 1];
		au.extend_from_slice(&[
			0x67, 0x64, 0x00, 0x1f, 0xac, 0x24, 0x84, 0x01, 0x40, 0x16, 0xec, 0x04, 0x40, 0x00, 0x00, 0x03, 0x00, 0x40,
			0x00, 0x00, 0x0c, 0x23, 0xc6, 0x0c, 0x92,
		]);
		au.extend_from_slice(&[0, 0, 0, 1, 0x68, 0xee, 0x32, 0xc8, 0xb0]);
		au.extend_from_slice(&[0, 0, 0, 1, 0x65, 0x88, 0x84, 0x21]);
		au.extend_from_slice(&[0, 0, 0, 1, 0x0c]);
		au.resize(au.len() + PAD, 0xff);
		au.push(0x80);
		au.into()
	}

	#[tokio::test(start_paused = true)]
	async fn finished_producer_releases_its_splitter() {
		for sources in [1, 4] {
			let mut broadcast = moq_net::broadcast::Info::new().produce();
			let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
			let consumer = broadcast.consume();

			let rendition = hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::H264(hang::catalog::H264 {
				inline: true,
				profile: 0x64,
				constraints: 0,
				level: 0x1f,
			}));
			let finished: Vec<_> = (0..sources)
				.map(|_| {
					let mut producer = Producer::new(broadcast.clone(), catalog.reserve(), rendition.clone()).unwrap();
					for pts in [0, 33_333] {
						let pts = moq_net::Timestamp::from_micros(pts).unwrap();
						producer.publish(&[Encoded::new(keyframe(), pts, true)]).unwrap();
					}
					producer.finish().unwrap();
					producer
				})
				.collect();

			// Late readers keep the published media, so only what the producers own is measured.
			let snapshot = catalog.snapshot();
			assert_eq!(snapshot.video.renditions.len(), sources);
			let _tracks: Vec<_> = snapshot
				.video
				.renditions
				.keys()
				.map(|name| consumer.track(name).unwrap())
				.collect();

			let released = released(|| drop(finished));
			assert!(
				released < (sources * PAD / 4) as isize,
				"{sources} finished producers still owned {released} bytes"
			);
		}
	}
}
