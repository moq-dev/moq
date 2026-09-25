//! Publish encoded video frames as a moq video track, with optional capture.
//!
//! Encoding is strictly on demand: the track and its catalog rendition are
//! advertised immediately (the rendition is probed from the encoder, since
//! nothing has been encoded yet), and the encoder itself only runs while a
//! subscriber is watching. Capture opens its camera once at startup to learn
//! the mode it negotiates, then keeps it closed between viewers. This mirrors
//! `moq-boy`, which pauses its emulator on `track::Producer::used()` /
//! `unused()`.

#[cfg(feature = "capture")]
use std::time::Instant;

use moq_mux::catalog::hang::CatalogExt;
#[cfg(feature = "capture")]
use moq_mux::rate::{Control, Policy};
#[cfg(any(feature = "capture", test))]
use moq_net::Timestamp;

use crate::Error;
#[cfg(feature = "capture")]
use crate::Rate;
#[cfg(feature = "capture")]
use crate::capture;

use super::Encoded;
#[cfg(feature = "capture")]
use super::Sink;
#[cfg(any(feature = "capture", test))]
use super::encoder;
#[cfg(feature = "capture")]
use super::encoder::Codec;

/// Last-resort framerate when neither the caller nor the camera reports one.
#[cfg(feature = "capture")]
const DEFAULT_FRAMERATE: Rate = Rate::integer(30);

/// Convert the probed rendition into the importer hint published before the first frame.
fn rendition_hint(rendition: hang::catalog::VideoConfig) -> moq_mux::catalog::VideoHint {
	let mut hint = moq_mux::catalog::VideoHint::default();
	hint.codec = Some(rendition.codec);
	hint.coded_width = rendition.coded_width;
	hint.coded_height = rendition.coded_height;
	hint.display_aspect_width = rendition.display_aspect_width;
	hint.display_aspect_height = rendition.display_aspect_height;
	hint.framerate = rendition.framerate;
	hint.bitrate = rendition.bitrate;
	hint.optimize_for_latency = rendition.optimize_for_latency;
	// Authoritative for both the catalog entry and the wire, so dropping it would silently
	// downgrade a caller's selection to the default.
	hint.container = rendition.container;
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
/// `E` is the catalog's application extension, defaulting to none. A host
/// carrying its own catalog sections (the FFI bindings use `hang::Extra`)
/// publishes into a catalog of the same shape.
pub struct Producer<E: CatalogExt = ()> {
	codecs: Codecs,
	_ext: std::marker::PhantomData<fn() -> E>,
}

impl<E: CatalogExt> Producer<E> {
	/// Publish a track carrying `rendition` into `broadcast`, registering it in
	/// `catalog`. The frames fed to [`publish`](Self::publish) must be in that
	/// codec's framing, which is what the [`Encoder`](super::Encoder) the
	/// rendition was probed from emits.
	///
	/// `rendition` comes from [`Config::probe`](super::Config::probe), so it is
	/// what the encoder will actually emit rather than a guess. It is published
	/// immediately, before anything is encoded, which is what lets a subscriber
	/// discover a track an on-demand encoder has not run for yet; because it
	/// already says what the first keyframe says, that keyframe confirms the
	/// catalog instead of correcting it.
	pub fn new(
		broadcast: moq_net::broadcast::Producer,
		catalog: moq_mux::catalog::Producer<E>,
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
		let track = broadcast.unique_track(suffix, catalog.track_info(hang::catalog::PRIORITY.video))?;
		Self::with_track(track, catalog, rendition)
	}

	/// Publish `rendition` on an existing track, registering it in `catalog`.
	///
	/// Use this when the caller owns the track name. [`new`](Self::new) derives a
	/// unique name from the codec instead.
	pub fn with_track(
		track: moq_net::track::Producer,
		catalog: moq_mux::catalog::Producer<E>,
		rendition: hang::catalog::VideoConfig,
	) -> Result<Self, Error> {
		let codecs = match &rendition.codec {
			hang::catalog::VideoCodec::H264(_) => Codecs::H264 {
				split: moq_mux::codec::h264::Split::new(),
				import: moq_mux::codec::h264::Import::new(track, catalog.reserve(), rendition_hint(rendition))?,
			},
			hang::catalog::VideoCodec::H265(_) => Codecs::H265 {
				split: moq_mux::codec::h265::Split::new(),
				import: moq_mux::codec::h265::Import::new(track, catalog.reserve(), rendition_hint(rendition))?,
			},
			// Unreachable via `Config::probe`, which only encodes what `Codec` covers.
			other => {
				return Err(Error::Codec(anyhow::anyhow!(
					"{other} is not a codec this producer can publish"
				)));
			}
		};
		Ok(Self {
			codecs,
			_ext: std::marker::PhantomData,
		})
	}

	/// A watch-only handle to the track's subscriber demand, created eagerly so
	/// subscription state is observable before any frames arrive. Watch it via
	/// [`used`](moq_net::track::Demand::used) / [`unused`](moq_net::track::Demand::unused).
	pub fn demand(&self) -> moq_net::track::Demand {
		match &self.codecs {
			Codecs::H264 { import, .. } => import.demand(),
			Codecs::H265 { import, .. } => import.demand(),
		}
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
					import.flush(frame.timestamp, std::time::Instant::now())?;
				}
				Codecs::H265 { split, import } => {
					let mut frames = split.decode(&frame.payload, timestamp)?;
					frames.extend(split.flush(timestamp)?);
					import.decode(frames)?;
					import.flush(frame.timestamp, std::time::Instant::now())?;
				}
			}
		}
		Ok(())
	}

	/// Record the encode duration before publishing its frames so the catalog can report a stall.
	pub fn observe_lag(&mut self, lag: std::time::Duration) -> Result<(), Error> {
		match &mut self.codecs {
			Codecs::H264 { import, .. } => import.observe_lag(lag)?,
			Codecs::H265 { import, .. } => import.observe_lag(lag)?,
		}
		Ok(())
	}

	/// Re-evaluate stall from source silence while waiting for the next frame.
	pub fn tick(&mut self) -> Result<(), Error> {
		match &mut self.codecs {
			Codecs::H264 { import, .. } => import.tick()?,
			Codecs::H265 { import, .. } => import.tick()?,
		}
		Ok(())
	}

	/// The camera is released; this rendition is never stalled while idle.
	pub fn idle(&mut self) -> Result<(), Error> {
		match &mut self.codecs {
			Codecs::H264 { import, .. } => import.idle()?,
			Codecs::H265 { import, .. } => import.idle()?,
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

	/// Finalize the track.
	///
	/// Borrows rather than consumes, so a later [`abort`](Self::abort) can still
	/// run after a successful finish.
	pub fn finish(&mut self) -> Result<(), Error> {
		match &mut self.codecs {
			Codecs::H264 { import, .. } => import.finish()?,
			Codecs::H265 { import, .. } => import.finish()?,
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

/// Source-agnostic encode knobs for [`publish_capture`], where the geometry
/// (width / height / framerate) comes from the capture source, not the caller.
/// For the bring-your-own-frames [`Encoder`](super::Encoder) path, where you
/// must specify geometry, use [`Config`](super::Config) instead.
///
/// `#[non_exhaustive]`: construct via [`Options::default`] and set fields, so
/// new knobs can be added without breaking callers.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
#[cfg(feature = "capture")]
pub struct Options {
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

/// Capture a webcam and publish it as an on-demand video track.
///
/// Returns when the broadcast is dropped (the track stops being announced)
/// or the capture loop fails. Frames are stamped from `clock`, so passing the
/// same [`Clock`](moq_mux::Clock) to a concurrent audio publish keeps the two
/// tracks aligned.
///
/// The camera is opened once at startup to probe the mode it negotiates, then released until a
/// subscriber arrives and reopened for as long as one is watching. That one open is what lets the
/// catalog rendition be exact before a single frame is published, so a consumer can size itself
/// against it (and discover the track at all) without waiting for an encoder that may never run.
#[cfg(feature = "capture")]
pub async fn publish_capture<E: CatalogExt>(
	broadcast: moq_net::broadcast::Producer,
	catalog: moq_mux::catalog::Producer<E>,
	capture: capture::Config,
	encode: Options,
	clock: moq_mux::Clock,
) -> Result<(), Error> {
	// Open the camera once to find out what it actually negotiated, since a requested size is only a
	// hint (macOS ignores it outright) and the encoder is built from the mode, not the request. It
	// closes again immediately: this costs one camera open at startup and buys a rendition that says
	// exactly what the stream will carry, rather than one every consumer has to treat as provisional.
	let rendition = {
		let camera = capture::open(&capture).await?;
		let mut probe_config = encoder::Config::new(
			camera.width(),
			camera.height(),
			capture
				.framerate
				.or_else(|| camera.framerate())
				.unwrap_or(DEFAULT_FRAMERATE),
		);
		probe_config.bitrate = encode.bitrate;
		probe_config.codec = encode.codec;
		probe_config.kind = encode.kind.clone();
		probe_config.color = camera.color();
		probe_config.probe().await?
	};

	let mut producer = Producer::new(broadcast, catalog, rendition)?;
	let demand = producer.demand();

	let result = capture_loop(&mut producer, &demand, &mut DeviceSource, &capture, &encode, &clock).await;

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

/// Off macOS, [`publish_capture`]'s future must stay `Send` so a server can
/// `tokio::spawn` it: the encoder runs on its own thread and the capture guard
/// is `Send` there. This is never called; it exists only to fail compilation if
/// the future ever regains a `!Send` component. macOS is exempt (the objc
/// capture session is `!Send`).
#[cfg(all(feature = "capture", not(target_os = "macos")))]
#[allow(dead_code)]
fn assert_publish_capture_send(
	broadcast: moq_net::broadcast::Producer,
	catalog: moq_mux::catalog::Producer,
	capture: capture::Config,
	encode: Options,
	clock: moq_mux::Clock,
) {
	fn is_send<T: Send>(_: &T) {}
	is_send(&publish_capture(broadcast, catalog, capture, encode, clock));
}

/// Where the capture loop opens its camera. Kept apart from the device backends so
/// the clock fixtures can drive the real loop from a synthetic source.
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

/// The live rate control state: the estimate source paired with the policy tracking
/// it. `None` once it has *retired*, which is the only thing absence means now that
/// every encoder has a share to read: an allocator with nothing to divide grants
/// `None` rather than being absent. Retiring stops the `select!` arm from spinning on
/// a channel that is permanently ready.
#[cfg(feature = "capture")]
type RateControl = Option<(moq_net::bandwidth::Consumer, Control)>;

/// Wait for the next bandwidth estimate, or forever when rate control is off or
/// finished. Cancel-safe: [`Consumer::changed`](moq_net::bandwidth::Consumer::changed)
/// only reads shared state, so losing this race to a frame drops no estimate,
/// it just re-reads the latest one next time round.
#[cfg(feature = "capture")]
async fn next_estimate(rate: &mut RateControl) -> Option<Option<moq_net::bandwidth::Rate>> {
	match rate {
		Some((bandwidth, _)) => bandwidth.changed().await.ok(),
		// Retired: park this arm forever so `select!` ignores it.
		None => std::future::pending().await,
	}
}

/// Feed an estimate through the policy and retune the encoder if it moved.
///
/// `None` means the producer is gone (the session ended for good), so rate
/// control retires; a `Some(None)` estimate means the value is merely
/// unavailable right now, which the policy holds through.
#[cfg(feature = "capture")]
async fn apply_estimate(
	encoder: &mut Sink,
	rate: &mut RateControl,
	estimate: Option<Option<moq_net::bandwidth::Rate>>,
) {
	let Some((_, control)) = rate.as_mut() else { return };

	let Some(estimate) = estimate else {
		tracing::debug!("bandwidth estimate ended; holding the current encoder bitrate");
		*rate = None;
		return;
	};

	let Some(bitrate) = control.update(estimate, Instant::now()) else {
		return;
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
		// A transient failure: keep the policy running so the next change retries.
		// The policy already moved its target, so a persistent failure just means
		// the encoder trails it; that's better than giving up on the first blip.
		Err(err) => tracing::warn!(error = %err, bitrate = bitrate.as_bps(), "failed to adjust encoder bitrate"),
	}
}

/// A dropped or closed track is the normal end of a publish; any other cause is
/// a real abort (e.g. a transport reset) worth surfacing rather than treating as
/// a clean exit.
#[cfg(feature = "capture")]
fn log_track_ended(err: moq_net::Error) {
	if matches!(err, moq_net::Error::Dropped | moq_net::Error::Closed) {
		tracing::debug!("video track no longer announced; stopping capture");
	} else {
		tracing::warn!(error = %err, "video track aborted; stopping capture");
	}
}

#[cfg(any(feature = "capture", all(test, feature = "openh264")))]
fn capture_stopped<E: CatalogExt>(producer: &mut Producer<E>) -> Result<(), Error> {
	// The shared clock keeps advancing while capture is stopped. Mark the break before waiting
	// for demand again so the next timestamp does not stretch the previous frame across the gap.
	producer.discontinuity()
}

// Keep observing silence while source setup or an encode is pending. The work
// future stays pinned across ticks, so a slow operation is never restarted.
#[cfg(feature = "capture")]
async fn wait_capture<E: CatalogExt, T>(
	producer: &mut Producer<E>,
	demand: &moq_net::track::Demand,
	work: impl std::future::Future<Output = Result<T, Error>>,
) -> Result<Option<T>, Error> {
	let mut work = std::pin::pin!(work);
	let mut timer = tokio::time::interval(hang::catalog::stalled::DEFAULT_INTERVAL);
	loop {
		tokio::select! {
			biased;
			res = demand.unused() => {
				if let Err(err) = res {
					log_track_ended(err);
				}
				producer.idle()?;
				return Ok(None);
			}
			_ = timer.tick() => producer.tick()?,
			res = &mut work => return res.map(Some),
		}
	}
}

/// Async capture/encode loop. Opens the camera while at least one viewer is
/// watching and releases it when the last one leaves.
///
/// Cancel safety: every wait here is a real `.await` (a frame read, a demand
/// transition, or an encode), so dropping this future (e.g. on Ctrl+C) drops
/// `camera` and `encoder`, which release the device (LED off) and join the
/// encode thread. Both the capture and encode threads sit idle between frames,
/// so their joins return promptly unless the underlying device or encoder is
/// itself wedged.
#[cfg(feature = "capture")]
async fn capture_loop<E: CatalogExt, S: CaptureSource>(
	producer: &mut Producer<E>,
	demand: &moq_net::track::Demand,
	source: &mut S,
	capture: &capture::Config,
	encode: &Options,
	clock: &moq_mux::Clock,
) -> Result<(), Error> {
	// This track's claim on the connection. Taken on the first open, because the
	// negotiated mode is what finally says how much this encoder can ever send, and
	// held across reopens so the claim doesn't lapse while the camera is closed.
	let mut reservation: Option<moq_net::bandwidth::Reservation> = None;

	loop {
		// Idle until a viewer subscribes; the track ending is a clean exit. The
		// catalog rendition was published when the track was created, so a
		// subscriber can get here without a frame ever having been encoded.
		if let Err(err) = demand.used().await {
			log_track_ended(err);
			return Ok(());
		}

		// Open the camera and an encoder sized to its negotiated mode.
		let Some(mut camera) = wait_capture(producer, demand, source.open(capture)).await? else {
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
		encoder_config.bitrate = encode.bitrate;
		encoder_config.codec = encode.codec;
		encoder_config.kind = encode.kind.clone();
		encoder_config.color = camera.color();
		// Off macOS this opens the encoder on a dedicated thread; see `sink`.
		// No cut on reopen: a fresh encoder opens with a keyframe on every backend,
		// so the viewer whose subscription reopened the camera can decode from the
		// first frame regardless, and a backend that cannot cut still captures.
		let Some(mut encoder) = wait_capture(producer, demand, Sink::open(&encoder_config)).await? else {
			continue;
		};
		tracing::info!(encoder = encoder.name(), device = camera.label(), "capturing");

		// A reopen can negotiate a different mode (a display resized while nothing was
		// subscribed), and the claim follows it: the old ceiling would otherwise cap a
		// larger mode below what it can send, or keep claiming room a smaller one no
		// longer needs.
		let ceiling = encoder_config.resolved_bitrate();
		let reservation = reservation.get_or_insert_with(|| encode.bandwidth.reserve(demand, ceiling));
		reservation.update(ceiling);

		// Rate control is per encoder: this one opened at the configured bitrate,
		// so the policy's ceiling is that rate and the target starts there. A
		// reopened camera starts optimistic again rather than inheriting the
		// backed-off rate from whatever the link was doing last time.
		let mut rate = Some((reservation.consumer(), Control::new(Policy::new(ceiling))));

		loop {
			// Race the next frame against the last viewer leaving so we release the
			// camera promptly when demand drops. `biased` checks demand first so an
			// unwatched track stops before reading another frame.
			let interval = hang::catalog::stalled::interval_from_fps(Some(framerate.as_f64()));
			let frame = tokio::select! {
				biased;
				res = demand.unused() => {
					if let Err(err) = res {
						log_track_ended(err);
						return Ok(());
					}
					break; // no viewers: release the camera, then wait for one
				}
				// Retune between frames rather than mid-encode, and only when
				// the policy says the target actually moved.
				estimate = next_estimate(&mut rate) => {
					apply_estimate(&mut encoder, &mut rate, estimate).await;
					continue;
				}
				// A read error is terminal for this selection (the source is gone
				// or was refused); `None` just ends the stream, so reopen below.
				// Timing out is a quiet camera: mark the rendition stalled and wait again.
				frame = tokio::time::timeout(interval, camera.read()) => match frame {
					Ok(frame) => frame?,
					Err(_) => {
						producer.tick()?;
						continue;
					}
				},
			};

			let Some(mut frame) = frame else { break };
			frame.timestamp = map_capture_timestamp(capture_epoch, frame.timestamp)?;
			let started = Instant::now();
			let Some(encoded) = wait_capture(producer, demand, encoder.encode(frame)).await? else {
				break;
			};
			let lag = started.elapsed();
			producer.observe_lag(lag)?;
			producer.publish(&encoded)?;
		}

		// Drop the camera (LED off) and encoder before waiting for the next viewer.
		drop(camera);
		drop(encoder);
		producer.idle()?;
		capture_stopped(producer)?;
		tracing::info!("capture stopped; released source");
	}
}

#[cfg(feature = "capture")]
fn map_capture_timestamp(epoch_micros: u64, timestamp: Timestamp) -> Result<Timestamp, Error> {
	let capture_micros = u64::try_from(timestamp.as_micros()).unwrap_or(u64::MAX);
	Ok(Timestamp::from_micros(epoch_micros.saturating_add(capture_micros))?)
}

#[cfg(test)]
mod tests {
	#![cfg_attr(not(feature = "openh264"), allow(dead_code, unused_imports))]

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

		let mut config = Config::new(320, 240, crate::Rate::new(30, 1).unwrap());
		config.codec = codec;
		config.kind = kind;

		let mut producer = Producer::new(broadcast, catalog.clone(), config.probe().await.unwrap()).unwrap();
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
		// Jitter aside, which is measured from the frames rather than declared by either.
		let (mut before, mut after) = (advertised, resolved.clone());
		before.jitter = None;
		after.jitter = None;
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
		let consumer = track.subscribe(moq_net::track::Subscription::default().with_max_age(replay));

		let mut config = Config::new(320, 240, crate::Rate::new(30, 1).unwrap());
		config.kind = encoder::Kind::Software;
		let mut producer = Producer::with_track(track, catalog, config.probe().await.unwrap()).unwrap();
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
	async fn source_resize_updates_the_published_rendition() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let mut initial = Config::new(320, 240, crate::Rate::new(30, 1).unwrap());
		initial.kind = encoder::Kind::Software;
		let mut producer = Producer::new(broadcast, catalog.clone(), initial.probe().await.unwrap()).unwrap();

		for (timestamp, config) in [
			(0, initial),
			(33_333, Config::new(640, 360, crate::Rate::new(30, 1).unwrap())),
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

		let (_, rendition) = rendition(&catalog).expect("the resized rendition should be published");
		assert_eq!(rendition.coded_width, Some(640));
		assert_eq!(rendition.coded_height, Some(360));
	}

	/// Regression: a caller's container selection has to survive the config -> hint conversion.
	///
	/// [`VideoHint::container`](moq_mux::catalog::VideoHint::container) is authoritative for both the
	/// track writer and the published rendition, so a conversion that drops it silently downgrades
	/// the caller's selection to Legacy while the catalog still claims whatever it defaulted to.
	#[tokio::test]
	#[cfg(feature = "openh264")]
	async fn a_selected_container_survives_the_rendition_hint() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();

		let mut config = Config::new(320, 240, crate::Rate::new(30, 1).unwrap());
		// Software (openh264) so the test is deterministic and never touches a hardware backend.
		config.kind = encoder::Kind::Software;
		let mut selected = config.probe().await.unwrap();
		selected.container = hang::catalog::Container::Loc;

		let _producer = Producer::new(broadcast, catalog.clone(), selected).unwrap();

		let (_, published) = rendition(&catalog).expect("the rendition publishes before any frame");
		assert_eq!(published.container, hang::catalog::Container::Loc);
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

		let mut config = Config::new(1920, 1080, crate::Rate::new(30, 1).unwrap());
		config.bitrate = Some(moq_net::bandwidth::Rate::from_mbps(6));
		// Software (openh264) so the test is deterministic and never touches a hardware backend.
		config.kind = encoder::Kind::Software;
		let _producer = Producer::new(broadcast, catalog, config.probe().await.unwrap()).unwrap();

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
		let mut config = Config::new(320, 240, crate::Rate::new(30, 1).unwrap());
		config.kind = encoder::Kind::Software;
		let track = broadcast
			.create_track("video", catalog.track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let mut subscriber = track.subscribe(None);
		let mut producer = Producer::with_track(track, catalog, config.probe().await.unwrap()).unwrap();
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
		assert!(name.ends_with(".avc3"));
		assert_eq!(config.coded_width, Some(320));
		assert_eq!(config.coded_height, Some(240));
	}

	/// H.265 has no software encoder, so this only runs where a hardware one
	/// exists (VideoToolbox on macOS, the only hardware backend on this target).
	#[cfg(target_os = "macos")]
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
		use std::time::{Duration, Instant, SystemTime};

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
			async fn start(behind: Duration, wall: SystemTime) -> Self {
				let epoch = Instant::now()
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

				let mut probe = Config::new(320, 240, crate::Rate::new(30, 1).unwrap());
				probe.kind = encoder::Kind::Software;
				let mut producer = Producer::with_track(track, catalog.clone(), probe.probe().await.unwrap()).unwrap();
				let demand = producer.demand();

				let (opens, rx) = tokio::sync::mpsc::unbounded_channel();
				let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
				let task = tokio::spawn(async move {
					let mut source = Opens(rx);
					let options = Options {
						kind: encoder::Kind::Software,
						..Options::default()
					};
					let config = capture::Config::default();
					tokio::select! {
						res = capture_loop(&mut producer, &demand, &mut source, &config, &options, &clock) => res?,
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
					.subscribe(moq_net::track::Subscription::default().with_max_age(RETAIN))
					.await
					.unwrap();
				moq_mux::container::Consumer::new(track, container)
			}

			/// Plug in the camera the loop opens next.
			fn camera(&self) -> Synthetic {
				let (camera, stream) = Synthetic::open(crate::Size::new(320, 240), crate::Rate::new(30, 1).unwrap());
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

		fn surface() -> crate::frame::Surface {
			crate::frame::Surface::I420(crate::frame::I420 {
				width: 320,
				height: 240,
				data: vec![0x80; 320 * 240 * 3 / 2],
				color: None,
			})
		}

		fn us(micros: u64) -> Timestamp {
			Timestamp::from_micros(micros).unwrap()
		}

		async fn read(track: &mut moq_mux::container::Consumer<moq_mux::catalog::hang::Container>) -> u64 {
			let frame = track.read().await.unwrap().expect("a published frame");
			u64::try_from(frame.timestamp.as_micros()).unwrap()
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
			let fixture = Fixture::start(Duration::from_secs(5), SystemTime::now()).await;
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
		}

		/// A device clock that restarts at zero, mid-stream or across a reopen, continues the
		/// broadcast forward with the device's spacing instead of rewinding it.
		#[tokio::test]
		async fn a_device_clock_restart_continues_forward() {
			let fixture = Fixture::start(Duration::from_secs(1), SystemTime::now()).await;
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
		}

		/// Releasing the camera while nobody watches keeps the broadcast clock running: the
		/// frame after a resume lands after the real idle gap, at its own acquisition.
		#[tokio::test]
		async fn a_restart_after_idle_keeps_the_gap() {
			let idle = Duration::from_millis(300);
			let fixture = Fixture::start(Duration::from_secs(1), SystemTime::now()).await;

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
		}

		/// The wall mapping is pinned when the broadcast clock is built. A system clock stepped
		/// an hour since then retimes neither the published timestamps nor the advertised mapping.
		#[tokio::test]
		async fn a_system_wall_adjustment_retimes_nothing() {
			// Whole seconds, so the advertised mapping holds it exactly.
			let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap();
			let wall = SystemTime::UNIX_EPOCH + Duration::from_secs(now.as_secs() - 3600);
			let fixture = Fixture::start(Duration::from_secs(1), wall).await;
			let advertised = fixture.catalog.snapshot().clock;
			assert_eq!(advertised, Some(fixture.clock.wall()));

			let mut track = fixture.subscribe().await;
			let camera = fixture.camera();
			let captured = Instant::now();
			camera.push_at(surface(), captured);
			let published = read(&mut track).await;

			// Timestamps follow the monotonic epoch and map to walls under the pinned mapping.
			fixture.assert_acquired(published, captured);
			let mapped = advertised.unwrap().wall_clock(us(published)).unwrap();
			// The catalog maps to walls at millisecond precision.
			assert_eq!(mapped, wall + Duration::from_millis(published / 1000));
			assert_eq!(fixture.catalog.snapshot().clock, advertised);
			fixture.finish().await;
		}

		/// A recording replays what the live edge published: the archive's segment records
		/// carry the live timestamps across an idle restart, with the idle gap left in.
		#[tokio::test]
		async fn retained_archive_playback_keeps_the_live_timestamps() {
			let fixture = Fixture::start(Duration::from_secs(1), SystemTime::now()).await;
			let section = fixture
				.catalog
				.snapshot()
				.archive
				.expect("the video track enrolls an archive");
			let mut timeline = moq_mux::timeline::Consumer::<()>::subscribe(&fixture.consumer, &section)
				.await
				.unwrap();

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
				// Idle past the minimum segment, so each run is archived as its own segment.
				tokio::time::sleep(moq_mux::timeline::DEFAULT_DURATION_MIN + Duration::from_millis(100)).await;
			}

			let (catalog, _consumer) = fixture.finish().await;
			catalog.timeline().finish().unwrap();
			let mut archived = Vec::new();
			while let Some(event) = timeline.next().await.unwrap() {
				match event {
					moq_mux::timeline::Event::Push { entry, .. } => archived.push(entry),
					other => panic!("unexpected timeline event {other:?}"),
				}
			}

			assert_eq!(archived.len(), live.len(), "one segment per capture run: {archived:?}");
			for (entry, live) in archived.iter().zip(&live) {
				// The archive keeps millisecond precision.
				assert_eq!(entry.pts.as_micros() / 1000, u128::from(*live / 1000), "{archived:?}");
				assert!(entry.tracks.contains_key("video"), "{archived:?}");
			}
			let first = &archived[0];
			assert!(
				archived[1].pts.as_micros() >= first.pts.as_micros() + first.duration.as_micros(),
				"the resumed segment overlaps the one before it: {archived:?}"
			);
		}
	}
}
