//! Publish already-encoded video frames as a moq video track.

use moq_mux::catalog::hang::CatalogExt;

use crate::Error;

use super::Encoded;
#[cfg(test)]
use super::encoder;

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
	hint.delay = rendition.delay;
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

/// Publishes already-encoded video frames as a moq track (avc3 / hev1 depending on the
/// codec), for a caller that runs its own encoder.
///
/// The `moq_mux::codec` importer for the codec handles catalog registration and framing.
/// [`Producer`](super::Producer) runs the encoder too, and follows the uplink's grant.
pub struct Publisher {
	codecs: Codecs,
}

impl Publisher {
	/// Publish `rendition` on `track`, registering it under `reserved`, a
	/// [`catalog::Producer::reserve`](moq_mux::catalog::Producer::reserve)
	/// that carries any per-track hint such as the encoder's GOP duration
	/// ([`Reserved::with_interval`](moq_mux::catalog::Reserved::with_interval)).
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
				return Err(Error::codec(anyhow::anyhow!(
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

	/// The group past the largest published while a subscriber asks for one, which the next
	/// frame should open with a keyframe, or `None` while the largest serves.
	#[cfg_attr(not(feature = "capture"), allow(dead_code, reason = "only capture forces keyframes"))]
	pub(crate) fn wanted(&self) -> Option<u64> {
		let track = self.track();
		let asked = track.demand().poll_refresh(&kio::Waiter::noop());
		if !matches!(asked, std::task::Poll::Ready(Ok(()))) {
			return None;
		}
		Some(track.largest().map_or(0, |largest| largest.group.saturating_add(1)))
	}

	/// Publish one already-encoded frame at its own timestamp: one whole access unit
	/// in the producer's codec framing.
	pub fn publish(&mut self, encoded: &Encoded) -> Result<(), Error> {
		let timestamp = Some(encoded.timestamp);
		// The encoder emits one whole access unit per frame, so flush to emit it.
		match &mut self.codecs {
			Codecs::H264 { split, import } => {
				let mut frames = split.decode(&encoded.payload, timestamp)?;
				frames.extend(split.flush(timestamp)?);
				import.decode(frames)?;
			}
			Codecs::H265 { split, import } => {
				let mut frames = split.decode(&encoded.payload, timestamp)?;
				frames.extend(split.flush(timestamp)?);
				import.decode(frames)?;
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

	/// Report capture frames the encoder never saw, in the catalog's stats.
	#[cfg_attr(not(feature = "capture"), allow(dead_code, reason = "only capture skips frames"))]
	pub(crate) fn report_skipped(&self, frames: u64) {
		match &self.codecs {
			Codecs::H264 { import, .. } => import.report_skipped(frames),
			Codecs::H265 { import, .. } => import.report_skipped(frames),
		}
	}

	/// Report the encoder's target bitrate, in the catalog's stats.
	pub(crate) fn report_bitrate(&self, bitrate: moq_net::bandwidth::Rate) {
		match &self.codecs {
			Codecs::H264 { import, .. } => import.report_bitrate(bitrate),
			Codecs::H265 { import, .. } => import.report_bitrate(bitrate),
		}
	}

	/// Finalize the track.
	///
	/// Consumes the publisher, releasing the splitter. The returned [`Finished`] keeps the track
	/// and its catalog rendition published, and can still [`abort`](Finished::abort) them. A
	/// finish that fails ends the track with an error instead.
	pub fn finish(self) -> Result<Finished, Error> {
		// Each publish flushed its access unit, so the splitter holds only input storage.
		let track = match self.codecs {
			Codecs::H264 { import, .. } => import.finish()?,
			Codecs::H265 { import, .. } => import.finish()?,
		};
		Ok(Finished { track })
	}

	/// Abort the track with `err` instead of finishing it cleanly, so subscribers
	/// see the real cause rather than [`moq_net::Error::Dropped`].
	pub fn abort(self, err: moq_net::Error) {
		match self.codecs {
			Codecs::H264 { import, .. } => import.abort(err),
			Codecs::H265 { import, .. } => import.abort(err),
		}
	}

	/// [`abort`](Self::abort) the track only while nobody wants it, handing the publisher back
	/// unchanged when a reader does.
	///
	/// This is how an on-demand publisher ends a track once its [`demand`](Self::demand) reports
	/// nobody wants it: dropping or aborting it instead fails a reader that joined in between.
	/// See [`moq_net::track::Producer::abort_unwanted`].
	#[expect(
		clippy::result_large_err,
		reason = "hand the publisher back to keep serving without allocating on an idle check"
	)]
	pub fn abort_unwanted(self, err: moq_net::Error) -> Result<(), Self> {
		let codecs = match self.codecs {
			Codecs::H264 { split, import } => import
				.abort_unwanted(err)
				.map_err(|import| Codecs::H264 { split, import }),
			Codecs::H265 { split, import } => import
				.abort_unwanted(err)
				.map_err(|import| Codecs::H265 { split, import }),
		};
		codecs.map_err(|codecs| Self { codecs })
	}
}

/// A video track whose [`Producer`](super::Producer) or [`Publisher`] finished it: every frame
/// is published, and it can still be watched or aborted, but not written to.
///
/// Dropping it releases the track like dropping the producer.
///
/// ```compile_fail,E0599
/// # fn cannot_publish(track: moq_video::encode::Finished, encoded: &moq_video::encode::Encoded) {
/// track.publish(encoded).unwrap();
/// # }
/// ```
#[must_use = "dropping it releases the track and its catalog rendition"]
pub struct Finished {
	track: moq_mux::container::Finished<moq_mux::catalog::hang::Container, hang::catalog::VideoConfig>,
}

impl Finished {
	/// A watch-only handle to the track's subscriber demand.
	pub fn demand(&self) -> moq_net::track::Demand {
		self.track.demand()
	}

	/// Abort the track with `err`, so subscribers that have not drained it see the real cause.
	pub fn abort(self, err: moq_net::Error) {
		self.track.abort(err);
	}
}

#[cfg(test)]
mod tests {
	#![cfg_attr(
		not(feature = "openh264"),
		allow(dead_code, unused_imports, reason = "only the `openh264` feature uses it")
	)]

	use moq_mux::catalog::Stream as _;
	use moq_net::Timestamp;

	use super::*;
	use crate::Frame;
	use crate::encode::{Codec, Config, Encoder};

	/// Encode a handful of synthetic frames for `codec` and publish them through a real
	/// [`Publisher`], returning the catalog rendition's track name and config.
	///
	/// Asserts the property the whole design rests on: the rendition published before anything is
	/// encoded is the one the first keyframe resolves. A guessed codec string would be corrected
	/// here; a probed one is confirmed, so the catalog is written once.
	///
	/// `kind` is explicit so the test picks a deterministic encoder rather than `Auto`, which on
	/// Linux CI would try the NVENC backend and panic in cudarc on a GPU-less runner.
	async fn roundtrip_rendition(codec: Codec, kind: encoder::Kind) -> hang::catalog::VideoConfig {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();

		let mut config = Config::new(320, 240, crate::FrameRate::new(30, 1).unwrap());
		config.codec = codec;
		config.kind = kind;

		let mut producer = Publisher::new(
			broadcast
				.create_track("video", catalog.track_info(hang::catalog::PRIORITY.video))
				.unwrap(),
			catalog.reserve().with_interval(config.effective_join()),
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
			for encoded in encoder.encode_all(&frame).unwrap() {
				producer.publish(&encoded).unwrap();
			}
		}
		for encoded in encoder.flush_all().unwrap() {
			producer.publish(&encoded).unwrap();
		}

		let (_, resolved) = rendition(&catalog).expect("the importer should have registered a video rendition");
		// Jitter and delay aside, which are measured from the frames rather than declared by either.
		let (mut before, mut after) = (advertised, resolved.clone());
		(before.jitter, before.delay) = (None, None);
		(after.jitter, after.delay) = (None, None);
		assert_eq!(
			before, after,
			"the first keyframe should confirm the advertised rendition, not correct it"
		);
		resolved
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
		let consumer = track.subscribe(
			moq_net::track::Subscription::default().with_delay(moq_net::track::Delay::default().with_max(replay)),
		);

		let mut config = Config::new(320, 240, crate::FrameRate::new(30, 1).unwrap());
		config.kind = encoder::Kind::Software;
		let mut producer = Publisher::new(
			track,
			catalog.reserve().with_interval(config.effective_join()),
			config.probe().await.unwrap(),
		)
		.unwrap();
		let mut encoder = Encoder::new(&config).unwrap();
		let rgba = vec![0x80u8; 320 * 240 * 4];

		for timestamp in [0, 10_000_000] {
			if timestamp > 0 {
				producer.discontinuity().unwrap();
			}
			encoder.cut().unwrap();
			let surface = crate::Surface::rgba(&rgba, crate::Size::new(320, 240)).unwrap();
			let frame = Frame::new(surface, Timestamp::from_micros(timestamp).unwrap());
			for encoded in encoder.encode_all(&frame).unwrap() {
				producer.publish(&encoded).unwrap();
			}
		}
		let _finished = producer.finish().unwrap();

		assert_eq!(collect_groups(consumer).await, vec![1, 1, 1]);
	}

	#[tokio::test]
	#[cfg(feature = "openh264")]
	async fn source_resize_publishes_a_new_track() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let mut initial = Config::new(320, 240, crate::FrameRate::new(30, 1).unwrap());
		initial.kind = encoder::Kind::Software;
		let mut producer = Publisher::new(
			broadcast
				.create_track("video", catalog.track_info(hang::catalog::PRIORITY.video))
				.unwrap(),
			catalog.reserve().with_interval(initial.effective_join()),
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
			for encoded in encoder.encode_all(&frame).unwrap() {
				producer.publish(&encoded).unwrap();
			}
			producer.discontinuity().unwrap();
		}

		// 640x360 takes a higher level than the probed 320x240, so it is a new track.
		let (name, rendition) = rendition(&catalog).expect("the resized rendition should be published");
		assert_eq!(name, "video/1", "the resize is a new track");
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

		let track = broadcast
			.unique_track(".avc3", catalog.track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let _producer = Publisher::new(track, catalog.reserve(), selected).unwrap();

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
		let _producer = Publisher::new(
			broadcast
				.create_track("video", catalog.track_info(hang::catalog::PRIORITY.video))
				.unwrap(),
			catalog.reserve().with_interval(config.effective_join()),
			config.probe().await.unwrap(),
		)
		.unwrap();

		// Published, not merely staged: this reads the catalog track a subscriber would.
		let mut stream = moq_mux::catalog::Consumer::<()>::new(&consumer, moq_mux::catalog::CatalogFormat::Hang)
			.await
			.unwrap();
		let snapshot = stream.next().await.unwrap().expect("a catalog before any frame");

		let (_, rendition) = snapshot
			.video
			.renditions
			.iter()
			.next()
			.expect("the track must be discoverable before it has encoded anything");

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

	/// Finish returns a handle, so abort can still run.
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
		let mut producer = Publisher::new(
			track,
			catalog.reserve().with_interval(config.effective_join()),
			config.probe().await.unwrap(),
		)
		.unwrap();
		let mut encoder = Encoder::new(&config).unwrap();
		let rgba = vec![0x80u8; 320 * 240 * 4];
		let surface = crate::Surface::rgba(&rgba, crate::Size::new(320, 240)).unwrap();
		let frame = Frame::new(surface, Timestamp::from_micros(0).unwrap());
		for encoded in encoder.encode_all(&frame).unwrap() {
			producer.publish(&encoded).unwrap();
		}

		let finished = producer.finish().unwrap();
		assert!(subscriber.recv_group().await.unwrap().is_some());
		finished.abort(moq_net::Error::Cancel);
	}

	#[tokio::test]
	#[cfg(feature = "openh264")]
	async fn h264_roundtrip_publishes_avc3() {
		// Software (openh264) so the test is deterministic and never touches a
		// hardware backend.
		let config = roundtrip_rendition(Codec::H264, encoder::Kind::Software).await;
		assert_eq!(config.reorder, Some(0));
		assert_eq!(config.coded_width, Some(320));
		assert_eq!(config.coded_height, Some(240));
	}

	/// H.265 has no software encoder, so this only runs where a hardware one
	/// exists (VideoToolbox on macOS, the only hardware backend on this target).
	#[cfg(apple)]
	#[tokio::test]
	async fn h265_roundtrip_publishes_hev1() {
		let config = roundtrip_rendition(Codec::H265, encoder::Kind::Hardware).await;
		assert_eq!(config.coded_width, Some(320));
		assert_eq!(config.coded_height, Some(240));
	}
}

/// A finished [`Publisher`] keeps its track and rendition published, but not its splitter.
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
					let track = broadcast
						.unique_track(".avc3", catalog.track_info(hang::catalog::PRIORITY.video))
						.unwrap();
					let mut producer = Publisher::new(track, catalog.reserve(), rendition.clone()).unwrap();
					for pts in [0, 33_333] {
						let pts = moq_net::Timestamp::from_micros(pts).unwrap();
						producer.publish(&Encoded::new(keyframe(), pts, true)).unwrap();
					}
					producer.finish().unwrap()
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
