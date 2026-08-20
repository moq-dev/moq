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
#[cfg(any(feature = "capture", test))]
use moq_net::Timestamp;

use crate::Error;
#[cfg(any(feature = "capture", test))]
use crate::Frame;
#[cfg(feature = "capture")]
use crate::capture;

use super::Encoded;
#[cfg(feature = "capture")]
use super::Sink;
#[cfg(any(feature = "capture", test))]
use super::encoder;
#[cfg(feature = "capture")]
use super::encoder::Codec;
#[cfg(feature = "capture")]
use super::rate::{Control, Policy};

/// Last-resort framerate when neither the caller nor the camera reports one.
#[cfg(feature = "capture")]
const DEFAULT_FRAMERATE: u32 = 30;

/// Per-codec splitter + importer pair. Each codec frames its packets and resolves
/// its catalog rendition differently, so the producer holds one of these.
enum Codecs<E: CatalogExt> {
	H264 {
		split: moq_mux::codec::h264::Split,
		import: moq_mux::codec::h264::Import<E>,
	},
	H265 {
		split: moq_mux::codec::h265::Split,
		import: moq_mux::codec::h265::Import<E>,
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
	codecs: Codecs<E>,
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
		mut broadcast: moq_net::broadcast::Producer,
		catalog: moq_mux::catalog::Producer<E>,
		rendition: hang::catalog::VideoConfig,
	) -> Result<Self, Error> {
		let codecs = match &rendition.codec {
			hang::catalog::VideoCodec::H264(_) => {
				let track = broadcast.unique_track(".avc3", catalog.track_info())?;
				Codecs::H264 {
					split: moq_mux::codec::h264::Split::new(),
					import: moq_mux::codec::h264::Import::new(track, catalog.reserve(), rendition.into())?,
				}
			}
			hang::catalog::VideoCodec::H265(_) => {
				let track = broadcast.unique_track(".hev1", catalog.track_info())?;
				Codecs::H265 {
					split: moq_mux::codec::h265::Split::new(),
					import: moq_mux::codec::h265::Import::new(track, catalog.reserve(), rendition.into())?,
				}
			}
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

	/// Finalize the track.
	///
	/// Consumes the producer: nothing can be published after the track ends, so
	/// this is the last call rather than one leaving a dead producer in your hands.
	pub fn finish(mut self) -> Result<(), Error> {
		match &mut self.codecs {
			Codecs::H264 { import, .. } => import.finish()?,
			Codecs::H265 { import, .. } => import.finish()?,
		}
		Ok(())
	}

	/// Abort the track with `err` instead of finishing it cleanly, so subscribers
	/// see the real cause rather than [`moq_net::Error::Dropped`].
	///
	/// Consumes the producer, like [`finish`](Self::finish).
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
#[derive(Clone, Default)]
#[non_exhaustive]
#[cfg(feature = "capture")]
pub struct Options {
	/// Target bitrate in bits per second; `None` derives from resolution.
	///
	/// This is a ceiling, not a fixed rate: with [`bandwidth`](Self::bandwidth)
	/// set, the encoder backs off below it while the uplink is congested and
	/// climbs back afterwards, but never exceeds it.
	pub bitrate: Option<u64>,
	/// Output codec. Defaults to [`Codec::H264`].
	pub codec: Codec,
	/// Encoder implementation preference.
	pub kind: encoder::Kind,
	/// The connection's send-bandwidth estimate, from
	/// [`Session::send_bandwidth`](moq_net::Session::send_bandwidth) (or
	/// `moq_tokio::Connection::send_bandwidth`, which survives reconnects).
	///
	/// Set it and the encoder tracks the estimate per the default
	/// [`rate::Policy`](super::rate::Policy), so a closing uplink gets a softer
	/// picture instead of a stalled one. Leave it `None` and the
	/// encoder holds [`bitrate`](Self::bitrate) regardless of congestion, which
	/// is what you want when the estimate isn't meaningful (a local file, a test
	/// harness) or unavailable (a publisher that only accepts inbound sessions).
	pub bandwidth: Option<moq_net::bandwidth::Consumer>,
}

// Hand-written: `bandwidth::Consumer` isn't `Debug`, but its presence is the
// only part worth printing anyway.
#[cfg(feature = "capture")]
impl std::fmt::Debug for Options {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Options")
			.field("bitrate", &self.bitrate)
			.field("codec", &self.codec)
			.field("kind", &self.kind)
			.field("bandwidth", &self.bandwidth.is_some())
			.finish()
	}
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
	// A caller asking for exactly zero is an error; omitting it (None) is
	// fine and resolves to the camera's reported rate once it's open.
	if capture.framerate == Some(0) {
		return Err(Error::InvalidFramerate(0));
	}

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

	let result = capture_loop(&mut producer, &demand, &capture, &encode, &clock).await;

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

/// The live rate control state: the estimate source paired with the policy
/// tracking it. `None` once there's nothing left to track, which is what stops
/// the `select!` arm from spinning on a channel that is permanently ready.
#[cfg(feature = "capture")]
type Rate = Option<(moq_net::bandwidth::Consumer, Control)>;

/// Wait for the next bandwidth estimate, or forever when rate control is off or
/// finished. Cancel-safe: [`Consumer::changed`](moq_net::bandwidth::Consumer::changed)
/// only reads shared state, so losing this race to a frame drops no estimate,
/// it just re-reads the latest one next time round.
#[cfg(feature = "capture")]
async fn next_estimate(rate: &mut Rate) -> Option<Option<u64>> {
	match rate {
		Some((bandwidth, _)) => bandwidth.changed().await.ok(),
		// No estimate source: park this arm forever so `select!` ignores it.
		None => std::future::pending().await,
	}
}

/// Feed an estimate through the policy and retune the encoder if it moved.
///
/// `None` means the producer is gone (the session ended for good), so rate
/// control retires; a `Some(None)` estimate means the value is merely
/// unavailable right now, which the policy holds through.
#[cfg(feature = "capture")]
async fn apply_estimate(encoder: &mut Sink, rate: &mut Rate, estimate: Option<Option<u64>>) {
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
		Ok(()) => tracing::debug!(bitrate, estimate, "adjusted encoder bitrate"),
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
		Err(err) => tracing::warn!(error = %err, bitrate, "failed to adjust encoder bitrate"),
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
async fn capture_loop<E: CatalogExt>(
	producer: &mut Producer<E>,
	demand: &moq_net::track::Demand,
	capture: &capture::Config,
	encode: &Options,
	clock: &moq_mux::Clock,
) -> Result<(), Error> {
	loop {
		// Idle until a viewer subscribes; the track ending is a clean exit. The
		// catalog rendition was published when the track was created, so a
		// subscriber can get here without a frame ever having been encoded.
		if let Err(err) = demand.used().await {
			log_track_ended(err);
			return Ok(());
		}

		// Open the camera and an encoder sized to its negotiated mode.
		let mut camera = capture::open(capture).await?;
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
		let mut encoder = Sink::open(&encoder_config).await?;
		// Force an IDR on the first frame of each (re)open so a viewer subscribing
		// after an idle gap can start decoding immediately.
		let mut force_keyframe = true;
		tracing::info!(encoder = encoder.name(), device = camera.device(), "capturing");

		// Rate control is per encoder: this one opened at the configured bitrate,
		// so the policy's ceiling is that rate and the target starts there. A
		// reopened camera starts optimistic again rather than inheriting the
		// backed-off rate from whatever the link was doing last time.
		let mut rate = encode
			.bandwidth
			.clone()
			.map(|bandwidth| (bandwidth, Control::new(Policy::new(encoder_config.resolved_bitrate()))));

		loop {
			// Race the next frame against the last viewer leaving so we release the
			// camera promptly when demand drops. `biased` checks demand first so an
			// unwatched track stops before reading another frame.
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
				frame = camera.read() => frame,
			};

			let Some(surface) = frame else { break }; // device stopped producing frames

			// Stamp at capture, so a backend that buffers still publishes each
			// access unit at the time the picture was grabbed.
			let frame = Frame::new(surface, Timestamp::from_micros(clock.micros())?);
			if force_keyframe {
				encoder.keyframe();
				force_keyframe = false;
			}
			producer.publish(&encoder.encode(frame).await?)?;
		}

		// Drop the camera (LED off) and encoder before waiting for the next viewer.
		drop(camera);
		tracing::info!("no viewers: released camera");
	}
}

#[cfg(test)]
mod tests {
	use moq_mux::catalog::Stream as _;

	use super::*;
	use crate::encode::{Config, Encoder};

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
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast).unwrap();

		let mut config = Config::new(320, 240, 30);
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

	/// Regression: the rendition has to reach the wire before anything is encoded.
	///
	/// A catalog reservation is held until the rendition resolves, and an unresolved one withholds
	/// the whole catalog from the broadcast. An encoder that runs only while watched then closes a
	/// cycle: the catalog waits on a keyframe, the keyframe waits on a subscriber, and the
	/// subscriber waits on the catalog. Nothing errors on either side; the publisher simply serves
	/// nothing, forever.
	#[tokio::test]
	async fn the_rendition_reaches_the_wire_before_the_first_frame() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast).unwrap();

		let mut config = Config::new(1920, 1080, 30);
		config.bitrate = Some(6_000_000);
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

	#[tokio::test]
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
}
