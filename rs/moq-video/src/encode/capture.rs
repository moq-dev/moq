//! Capture a source, encode it, and publish it as a moq video track.
//!
//! The track and its catalog rendition are advertised immediately (the rendition is probed from
//! the encoder, since nothing has been encoded yet). Capture opens its camera at startup to learn
//! the mode it negotiates, then keeps it open and encodes whether or not anyone is watching, so a
//! late subscriber can reach the recent past within its `max_delay`. With
//! `Capture::on_demand` it instead closes the camera between viewers, encoding only while
//! [`Demand::wanted`](moq_net::track::Demand::wanted): a subscriber is watching.
//!
//! Capture advertises [`track::Info::refresh`](moq_net::track::Info::refresh) when its encoder
//! is sure to honor a cut, and forces a keyframe whenever a joining subscriber asks for a group
//! past the largest. The encoder's GOP stays the upper bound between keyframes.
//!
//! The capture loop feeds a [`Producer`], which owns the encoder and follows the grant, the same
//! one a caller with its own frames writes to.

use moq_mux::catalog::hang::CatalogExt;
use moq_net::Timestamp;

use crate::{Error, FrameRate, capture};

use super::cuts::Cuts;
use super::encoder::{self, Codec};
use super::producer::{Producer, suffix};

/// Last-resort framerate when neither the caller nor the camera reports one.
const DEFAULT_FRAMERATE: FrameRate = FrameRate::integer(30);

/// The [`track::Info::refresh`](moq_net::track::Info::refresh) ratio a capture advertises: a
/// joiner forces a keyframe once a group's deltas outweigh twice its keyframe, so later joiners
/// start near one. The joiner that asks still reads the cached group meanwhile.
///
/// Measured on realtime CBR encodes (two-second GOPs at 2 to 4 Mbps), where a GOP's deltas run
/// three to five keyframes: one join a second halves the mean joiner download for 10 to 13%
/// more bitrate, and one every ten seconds costs about 1%. A ratio scales with the keyframe,
/// so it holds for every rendition.
const REFRESH: u32 = 2;

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
	/// Open the source and encode only while a subscriber wants the track, closing it between
	/// viewers.
	///
	/// Off by default: capture runs for the whole publish, so a late subscriber or fetch finds
	/// the recent past in the track's cache. On demand saves the capture and encode while nobody
	/// watches, and leaves nothing to fetch from before the first viewer arrived.
	pub on_demand: bool,
	/// The hardware the encoder opens; see [`Config::device`](encoder::Config::device).
	pub device: crate::Device,
}

impl Capture {
	/// The encoder for a `width` x `height` source at `framerate`, with these options.
	fn encoder(&self, width: u32, height: u32, framerate: FrameRate) -> encoder::Config {
		let mut config = encoder::Config::new(width, height, framerate);
		config.bitrate = self.bitrate;
		config.codec = self.codec;
		config.kind = self.kind.clone();
		config.device = self.device.clone();
		config
	}
}

/// The capture publication's current lifecycle state.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum Status {
	/// On demand, the source is closed until a subscriber wants the track.
	Waiting,
	/// The source and its encoder are opening.
	Starting,
	/// Frames are being captured, encoded, and published.
	Live,
	/// The capture, encoder, or track failed with this error, which [`Driver::run`] returns.
	Failed(Error),
	/// The MoQ track ended, so the driver has stopped.
	Ended,
	/// The [`Driver`] or its [`run`](Driver::run) future was dropped before it
	/// finished, cancelling the capture.
	Cancelled,
}

/// A snapshot of a capture publication's lifecycle.
#[derive(Clone, Debug)]
pub struct State {
	status: Status,
}

impl State {
	/// The publication's current lifecycle state.
	pub fn status(&self) -> &Status {
		&self.status
	}
}

#[derive(Clone, Debug)]
struct Published {
	state: State,
	revision: u64,
}

/// A new publication's state, before its driver runs.
fn starting() -> kio::Producer<Published> {
	kio::Producer::new(Published {
		state: State {
			status: Status::Starting,
		},
		revision: 0,
	})
}

/// Publish a lifecycle transition, skipping one that changes nothing.
fn publish_status(state: &kio::Producer<Published>, status: Status) {
	let Ok(mut published) = state.write() else { return };
	// A repeated status changes nothing an observer can see; a failure always carries news.
	if matches!(status, Status::Failed(_))
		|| std::mem::discriminant(&published.state.status) != std::mem::discriminant(&status)
	{
		published.state.status = status;
		published.revision = published.revision.wrapping_add(1);
	}
}

/// Capture a webcam and publish it as a video track.
///
/// Registers the track before anything about its source is known, and returns a
/// handle that observes the publication plus the [`Driver`] that captures it. The
/// driver opens the source to probe the mode it negotiates, so the catalog
/// rendition is exact before a frame is published, then captures and encodes
/// whether or not anyone is watching. Set [`Capture::on_demand`] to capture only
/// while a subscriber is watching instead.
pub fn publish_capture<E: CatalogExt>(
	broadcast: moq_net::broadcast::Producer,
	catalog: moq_mux::catalog::Producer<E>,
	options: Capture,
) -> Result<(Publication, Driver<E>), Error> {
	let track = capture_track(broadcast, &catalog, &options)?;
	let state = starting();
	let publication = Publication {
		state: state.consume(),
		observed: 0,
	};
	let driver = Driver {
		owned: Owned::Track(track),
		catalog,
		options,
		state,
	};
	Ok((publication, driver))
}

/// A handle that observes a capture publication.
///
/// Clones observe the same publication. Dropping it changes nothing: the
/// [`Driver`] owns the publication.
pub struct Publication {
	state: kio::Consumer<Published>,
	observed: u64,
}

impl Clone for Publication {
	fn clone(&self) -> Self {
		Self {
			state: self.state.clone(),
			observed: self.state.read().revision,
		}
	}
}

impl std::fmt::Debug for Publication {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Publication")
			.field("state", &self.state.read().state)
			.finish_non_exhaustive()
	}
}

impl Publication {
	/// Return the latest state without waiting.
	pub fn state(&self) -> State {
		self.state.read().state.clone()
	}

	/// Wait for a lifecycle change, or return `None` after the driver exits.
	pub async fn changed(&mut self) -> Option<State> {
		let observed = self.observed;
		let published = self
			.state
			.wait(move |published| {
				if published.revision != observed {
					std::task::Poll::Ready((**published).clone())
				} else {
					std::task::Poll::Pending
				}
			})
			.await
			.ok()?;
		self.observed = published.revision;
		Some(published.state)
	}

	/// Whether the driver has exited.
	pub fn is_finished(&self) -> bool {
		self.state.is_closed()
	}
}

/// The task that captures, encodes, and publishes one video track.
///
/// Dropping it, or its [`run`](Self::run) future, stops the capture, which also
/// abandons a camera open or permission prompt still in flight.
pub struct Driver<E: CatalogExt = ()> {
	owned: Owned,
	catalog: moq_mux::catalog::Producer<E>,
	options: Capture,
	state: kio::Producer<Published>,
}

/// What a [`Driver`] owns of its track, one phase at a time, held in the driver so a
/// dropped [`run`](Driver::run) future still cancels it.
enum Owned {
	/// Registered, until the probe knows its rendition.
	Track(moq_net::track::Producer),
	/// Probed, and wrapped in the producer that encodes and publishes it.
	Producer(Box<Producer>),
	/// Ended or cancelled, so nothing is left to end.
	Done,
}

impl Owned {
	/// End the track cleanly, best effort: it is usually already going away.
	async fn finish(self) {
		let ended = match self {
			// The caller is done with the track, so the finished handle goes with it.
			Self::Track(track) => track.finish().map(drop).map_err(Error::from),
			// The capture is gone, so its encoder's tail has nothing to follow. Released, the encoder
			// leaves nothing to drain, so the finish completes without waiting on it.
			Self::Producer(mut producer) => match producer.release() {
				Ok(()) => producer.finish().await.map(drop),
				Err(err) => Err(err),
			},
			Self::Done => Ok(()),
		};
		if let Err(err) = ended {
			tracing::debug!(error = %err, "video track finish after capture ended");
		}
	}

	/// End the track with `err`. A track that already ended has nobody left to tell.
	fn abort(self, err: moq_net::Error) {
		match self {
			Self::Track(track) => {
				let _ = track.abort(err);
			}
			Self::Producer(producer) => producer.abort(err),
			Self::Done => {}
		}
	}
}

impl<E: CatalogExt> std::fmt::Debug for Driver<E> {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Driver").finish_non_exhaustive()
	}
}

impl<E: CatalogExt> Drop for Driver<E> {
	fn drop(&mut self) {
		// Dropped before `run` finished: say so rather than `Dropped`.
		std::mem::replace(&mut self.owned, Owned::Done).abort(moq_net::Error::Cancel);
		// A no-op once `run` closed the state on its own terminal status.
		publish_status(&self.state, Status::Cancelled);
	}
}

impl<E: CatalogExt> Driver<E> {
	/// Capture until the capture fails or its track ends. Closing or dropping the broadcast
	/// leaves the track running; drop the [`Driver`] or this future to stop it.
	///
	/// Frames are stamped on the catalog's clock, so a concurrent audio publish into the
	/// same catalog stays aligned. Off macOS and iOS the future is `Send` and can be
	/// spawned; on macOS the capture session is `!Send`, so await it on a local task there.
	pub async fn run(self) -> Result<(), Error> {
		self.run_from(DeviceSource).await
	}

	/// [`run`](Self::run) from `source`, so the fixtures can drive it from a synthetic or
	/// stalled one.
	async fn run_from<S: CaptureSource>(mut self, source: S) -> Result<(), Error> {
		let result = self.capture(source).await;
		// This runs only when the capture ends on its own; dropping the future leaves the
		// track to `Drop` instead, since async `Drop` can't finalize it.
		let owned = std::mem::replace(&mut self.owned, Owned::Done);
		match &result {
			Ok(()) => owned.finish().await,
			// Abort with the real cause so subscribers see it.
			Err(err) => owned.abort(moq_net::Error::Transport(err.to_string())),
		}
		publish_status(
			&self.state,
			match &result {
				Ok(()) => Status::Ended,
				Err(err) => Status::Failed(err.clone()),
			},
		);
		let _ = self.state.close();
		result
	}

	/// Probe `source`, wrap the track in a producer for its rendition, then run the capture loop.
	async fn capture<S: CaptureSource>(&mut self, mut source: S) -> Result<(), Error> {
		let clock = self.catalog.clock();
		let options = &self.options;

		// Open the camera to find out what it actually negotiated, since a requested size is only a
		// hint (macOS ignores it outright) and the encoder is built from the mode, not the request.
		// This buys a rendition that says exactly what the stream will carry, rather than one every
		// consumer has to treat as provisional, and lets a consumer size itself against it without
		// waiting for an encoder that may not run yet.
		let (rendition, config, camera) = async {
			let camera = source.open(&options.capture).await?;
			let mut probe_config = options.encoder(
				camera.width(),
				camera.height(),
				options
					.capture
					.framerate
					.or_else(|| camera.framerate())
					.unwrap_or(DEFAULT_FRAMERATE),
			);
			probe_config.color = camera.color();
			let rendition = probe_config.probe().await?;
			// The encoder is built from the same defaults, so the probe's join is the one it emits,
			// and its ceiling the one the track first claims.
			Ok::<_, Error>((rendition, probe_config, camera))
		}
		.await?;

		// The producer takes a clone, so a setup failure leaves the registered track for `run`
		// to abort, and success drops ours so it never outlives the producer's.
		let Owned::Track(track) = &self.owned else {
			unreachable!("a driver runs once")
		};
		let producer = Producer::with_track(
			track.clone(),
			&self.catalog,
			config,
			rendition,
			options.bandwidth.clone(),
			None,
		)?;
		self.owned = Owned::Producer(Box::new(producer));
		let Owned::Producer(producer) = &mut self.owned else {
			unreachable!("just set")
		};
		// Capturing from the start, the loop keeps the probe's camera rather than reopening it,
		// which could mean a second permission prompt or picker. On demand it closes until a viewer.
		let mut source = Opened {
			stream: (!options.on_demand).then_some(camera),
			source,
		};
		capture_loop(producer, &mut source, options, &clock, &self.state).await
	}
}

/// Register the capture's video track, before anything about its source is known.
///
/// Takes `broadcast` only to mint the track, which outlives it.
fn capture_track<E: CatalogExt>(
	broadcast: moq_net::broadcast::Producer,
	catalog: &moq_mux::catalog::Producer<E>,
	options: &Capture,
) -> Result<moq_net::track::Producer, Error> {
	// Advertised only when every backend the encoder may open honors a cut, since the info
	// is fixed before the capture learns which one it got.
	let ratio = super::backend::always_cuts(options.codec, &options.kind).then_some(REFRESH);
	let refresh = moq_net::track::Refresh::default().with_ratio(ratio);
	let info = catalog.track_info(hang::catalog::PRIORITY.video).with_refresh(refresh);
	Ok(broadcast.unique_track(suffix(options.codec), info))
}

/// Off macOS and iOS, [`Driver::run`]'s future must stay `Send` so a server can
/// `tokio::spawn` it: the encoder runs on its own thread and the capture guard
/// is `Send` there. This is never called; it exists only to fail compilation if
/// the future ever regains a `!Send` component. Apple platforms are exempt (the inline encoder and objc
/// capture session are `!Send`).
#[cfg(not(apple))]
#[allow(dead_code, reason = "a compile-time `Send` assertion, never called")]
fn assert_driver_send(driver: Driver) {
	fn is_send<T: Send>(_: &T) {}
	is_send(&driver.run());
}

/// Where the capture opens its camera. Kept apart from the device backends so the
/// fixtures can drive the real capture from a synthetic or stalled source.
trait CaptureSource {
	async fn open(&mut self, config: &capture::Config) -> Result<capture::Stream, Error>;
}

struct DeviceSource;

/// Hands out a stream that is already open before opening `source` again.
struct Opened<S> {
	stream: Option<capture::Stream>,
	source: S,
}

impl<S: CaptureSource> CaptureSource for Opened<S> {
	async fn open(&mut self, config: &capture::Config) -> Result<capture::Stream, Error> {
		match self.stream.take() {
			Some(stream) => Ok(stream),
			None => self.source.open(config).await,
		}
	}
}

impl CaptureSource for DeviceSource {
	async fn open(&mut self, config: &capture::Config) -> Result<capture::Stream, Error> {
		capture::open(config).await
	}
}

/// An application dropping or closing its producer ends publishing normally;
/// a specific abort (such as a transport reset) stays visible in diagnostics.
fn log_track_ended(err: moq_net::Error) {
	if matches!(err, moq_net::Error::Dropped | moq_net::Error::Closed) {
		tracing::debug!("video track no longer announced; stopping capture");
	} else {
		tracing::warn!(error = %err, "video track aborted; stopping capture");
	}
}

/// Wait until capture should run (`run`) or release its source (`!run`). On demand it runs
/// while some viewer wants the track; otherwise it runs, and never releases, while the track
/// is open. Errors once the track closes.
async fn until_capturing(demand: &moq_net::track::Demand, on_demand: bool, run: bool) -> Result<(), moq_net::Error> {
	let mut demand = demand.clone();
	loop {
		let wanted = demand.wanted_changed().await?;
		if (wanted || !on_demand) == run {
			return Ok(());
		}
	}
}

// Race source setup or an encode against capture releasing its source, so an on-demand track
// its last viewer left, or a track that ended, stops waiting promptly. `None` when it released;
// the next `until_capturing` reports a track that ended.
async fn wait_capture<T>(
	demand: &moq_net::track::Demand,
	on_demand: bool,
	work: impl std::future::Future<Output = Result<T, Error>>,
) -> Result<Option<T>, Error> {
	tokio::select! {
		biased;
		_ = until_capturing(demand, on_demand, false) => Ok(None),
		res = work => res.map(Some),
	}
}

/// Async capture/encode loop. Opens the camera and keeps it open, or
/// [`on_demand`](Capture::on_demand), opens it while at least one viewer is
/// watching and releases it when the last one leaves. It never returns on its
/// own; dropping its future stops it.
///
/// The [`Producer`] holds the encoder, the track's claim on the connection, and
/// rate control, so they outlive a closed camera: a reopen at another mode moves
/// the claim's ceiling rather than lapsing it.
///
/// Cancel safety: every wait here is a real `.await` (a frame read, a demand
/// transition, or an encode), so dropping this future (e.g. on Ctrl+C) drops
/// `camera`, which releases the device (LED off); the encode thread is joined
/// once the producer goes. Both the capture and encode threads sit idle between
/// frames, so their joins return promptly unless the underlying device or encoder
/// is itself wedged.
async fn capture_loop<S: CaptureSource>(
	producer: &mut Producer,
	source: &mut S,
	options: &Capture,
	clock: &moq_mux::Clock,
	state: &kio::Producer<Published>,
) -> Result<(), Error> {
	let capture = &options.capture;
	let on_demand = options.on_demand;
	// The published track's demand, followed onto the next track when a frame moves the rendition.
	let mut demand = producer.demand();
	// The last timestamp sent to an encoder, which a reopened camera's frames must exceed.
	let mut last: Option<Timestamp> = None;

	loop {
		// On demand, idle until a viewer subscribes. The catalog rendition was published when
		// the track was created, so a subscriber can get here without a frame ever having been
		// encoded. Either way the track ending is a clean exit.
		if on_demand {
			publish_status(state, Status::Waiting);
		}
		if let Err(err) = until_capturing(&demand, on_demand, true).await {
			log_track_ended(err);
			return Ok(());
		}
		publish_status(state, Status::Starting);

		// Open the camera and an encoder sized to its negotiated mode.
		let Some(mut camera) = wait_capture(&demand, on_demand, source.open(capture)).await? else {
			continue;
		};
		// The broadcast clock's lead over this stream's timeline, in microseconds, sampled per frame.
		let mut offset: Option<i128> = None;
		// How far this stream is shifted past the previous one's last timestamp, set by its first frame.
		let mut lead: Option<i128> = None;
		// Prefer an explicit --fps, otherwise the camera's reported rate, falling
		// back only if the backend doesn't expose one.
		let framerate = capture
			.framerate
			.or_else(|| camera.framerate())
			.unwrap_or(DEFAULT_FRAMERATE);
		let mut encoder_config = options.encoder(camera.width(), camera.height(), framerate);
		encoder_config.color = camera.color();
		// Off macOS and iOS this opens the encoder on a dedicated thread; see `sink`.
		// No cut on reopen: a fresh encoder opens with a keyframe on every backend,
		// so the viewer whose subscription reopened the camera can decode from the
		// first frame regardless, and a backend that cannot cut still captures.
		if wait_capture(&demand, on_demand, producer.open(encoder_config))
			.await?
			.is_none()
		{
			continue;
		}
		// A reopen can land on another backend than the probe did, so ask this one.
		let cuts = producer.applied().is_some_and(|applied| applied.cut);
		tracing::info!(encoder = producer.name(), device = camera.label(), "capturing");
		publish_status(state, Status::Live);

		// Per encoder, since a fresh one opens with a keyframe that serves every earlier ask.
		let mut forced = Cuts::default();
		let mut watch = demand.clone();
		// The camera's skips already accounted for, from its first read on: frames it replaced
		// while this encoder was still opening are startup, not frames the encoder fell behind on.
		let mut reported = None;
		// An encode released mid-call leaves the encoder refusing every later call, so it can't drain.
		let mut cancelled = false;

		loop {
			// Race the next frame against demand, so an on-demand capture releases the
			// camera promptly when the last viewer leaves. `biased` checks demand first
			// so an unwatched on-demand track stops before reading another frame.
			let frame = tokio::select! {
				biased;
				wanted = watch.wanted_changed() => match wanted {
					Ok(wanted) if on_demand && !wanted => break, // no viewers: release the camera
					Ok(_) => continue,
					Err(err) => {
						log_track_ended(err);
						return Ok(());
					}
				},
				// A read error is terminal for this selection (the source is gone
				// or was refused); `None` just ends the stream, so reopen below.
				frame = camera.read() => frame?,
			};

			let Some(mut frame) = frame else { break };
			// Frames the camera replaced while this loop was encoding or retuning.
			let skipped = camera.skipped();
			if let Some(reported) = reported {
				producer.report_skipped(skipped - reported);
			}
			reported = Some(skipped);
			// Capture timestamps use a private monotonic timeline. The offset to the broadcast clock is
			// sampled per frame, so each queued frame maps by acquisition, not dequeue, time, and the
			// stream follows the clock's slew toward the wall clock and its jump across a suspend.
			// Reads straddling a stall would skew it, so those keep the last offset. The first after
			// an open has none to keep and fixes the reopen lead, so it reads again instead.
			let mut sample = clock_offset(clock, &camera)?;
			for _ in 0..RESAMPLES {
				if sample.is_some() || offset.is_some() {
					break;
				}
				sample = clock_offset(clock, &camera)?;
			}
			if let Some(sample) = sample {
				offset = Some(sample);
			} else if offset.is_none() {
				offset = Some(clock_offset_unchecked(clock, &camera)?);
			}
			let mut micros = frame.timestamp.as_micros()? as i128 + offset.unwrap_or_default();
			// A camera never rewinds its own stream, but a reopened one maps from the shared clock,
			// which the previous stream can lead: a device re-anchor floors just past its last
			// timestamp, ahead of arrival. Shift the new stream past it, keeping its spacing.
			let last_micros = last.map(|last| last.as_micros()).transpose()?.map(|last| last as i128);
			let lead = *lead.get_or_insert_with(|| match last_micros {
				Some(last) if micros <= last => last - micros + 1,
				_ => 0,
			});
			micros += lead;
			// Nor does a frame land at or below the last, which only that frame is held to.
			if let Some(last) = last_micros {
				micros = micros.max(last + 1);
			}
			let mapped = map_capture_timestamp(micros)?;
			last = Some(mapped);
			frame.timestamp = mapped;
			// A backend that cannot cut leaves every group to its cadence.
			let wanted = cuts.then(|| producer.wanted()).flatten();
			if forced.due(wanted) {
				producer.cut().await?;
			}
			// Released mid-encode, the encoder refuses the next call, so the release below drops it.
			if wait_capture(&demand, on_demand, producer.write(frame)).await?.is_none() {
				cancelled = true;
				break;
			}
			// A frame whose decoder identity differs from the probed one (a reopen onto another
			// mode or backend) publishes under a new name and ends this track, and the producer's
			// claim follows it there. Nobody watches the new track yet: its viewers find it in the
			// catalog, so on demand the camera closes until one subscribes there.
			if producer.demand().name() != demand.name() {
				demand = producer.demand();
				tracing::info!(track = %demand.name(), "capture moved to a new track");
				if on_demand {
					break;
				}
				watch = demand.clone();
			}
		}

		// Frames the camera replaced after the last read, before demand or the stream ended
		// the loop: no read follows to report them.
		if let Some(reported) = reported {
			producer.report_skipped(camera.skipped() - reported);
		}
		// Drop the camera (LED off) and encoder before opening another. The shared clock keeps
		// advancing while capture is stopped, so mark the break before waiting for demand again,
		// so the next timestamp does not stretch the previous frame across the gap. A backend
		// that pipelines (Media Foundation, MediaCodec) still holds the last frames, so drain them
		// into the track first, unless a cancelled encode left the encoder refusing calls.
		drop(camera);
		let stopped = match cancelled {
			true => producer.release(),
			false => producer.discontinuity().await,
		};
		// The drained frames can move the rendition to a new track, as any written frame can.
		if producer.demand().name() != demand.name() {
			demand = producer.demand();
			tracing::info!(track = %demand.name(), "capture moved to a new track");
		}
		stopped?;
		tracing::info!("capture stopped; released source");
	}
}

/// The most two camera readings may straddle for the broadcast clock read between them to place it.
const STALL: i128 = 1_000;

/// How many more times a stream's first offset is read while every read straddles a stall.
const RESAMPLES: usize = 8;

/// The broadcast clock's lead over `camera`'s timeline in microseconds, read between two camera
/// readings, or `None` when they straddle a stall.
fn clock_offset(clock: &moq_mux::Clock, camera: &capture::Stream) -> Result<Option<i128>, Error> {
	let before = camera.now().as_micros()? as i128;
	let media = clock.now().as_micros()? as i128;
	let after = camera.now().as_micros()? as i128;
	Ok((after - before <= STALL).then_some(media - (before + after) / 2))
}

/// [`clock_offset`] however long its reads straddle, for a host that never reads cleanly.
fn clock_offset_unchecked(clock: &moq_mux::Clock, camera: &capture::Stream) -> Result<i128, Error> {
	Ok(clock.now().as_micros()? as i128 - camera.now().as_micros()? as i128)
}

/// A broadcast clock reading in microseconds, zero before PTS zero.
fn map_capture_timestamp(micros: i128) -> Result<Timestamp, Error> {
	Ok(Timestamp::from_micros(
		u64::try_from(micros.max(0)).unwrap_or(u64::MAX),
	)?)
}

#[cfg(test)]
mod tests {
	#![cfg_attr(
		not(feature = "openh264"),
		allow(dead_code, unused_imports, reason = "only the `openh264` feature uses it")
	)]

	use moq_mux::catalog::Stream as _;

	use super::*;
	use crate::encode::Config;

	/// A producer for `config` on `track`, publishing the rendition the software encoder probes,
	/// with no encoder open until the capture opens one.
	async fn producer(
		track: moq_net::track::Producer,
		catalog: &moq_mux::catalog::Producer,
		config: Config,
		bandwidth: moq_net::bandwidth::Allocator,
	) -> Producer {
		let mut probe = config.clone();
		probe.kind = encoder::Kind::Software;
		let rendition = probe.probe().await.unwrap();
		Producer::with_track(track, catalog, config, rendition, bandwidth, None).unwrap()
	}

	#[test]
	fn a_capture_before_pts_zero_maps_to_zero() {
		assert_eq!(map_capture_timestamp(-5).unwrap().as_micros().unwrap(), 0);
	}

	/// Clock fixtures: the real capture loop, fed by a synthetic camera against a pinned
	/// broadcast clock, graded on the timestamps a subscriber reads back. The fixture installs a
	/// manual clock, since paused tokio time would skip ahead while the encoder thread works:
	/// time passes only where a test advances it, so a slow runner moves nothing.
	///
	/// Each expectation is the acquisition instant measured on the broadcast clock. The loop
	/// samples the broadcast clock and then the camera's timeline for each frame, so a published
	/// timestamp may land up to `SAMPLING` early, never late.
	#[cfg(feature = "openh264")]
	mod clock {
		use kio::clock::Wall;
		use std::time::Duration;

		use kio::clock::Instant;

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
			time: kio::clock::Installed,
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
				Self::start_on(behind, wall, encoder::Kind::Software).await
			}

			/// [`start`](Self::start), capturing through the encoder `kind`.
			async fn start_on(behind: Duration, wall: Wall, kind: encoder::Kind) -> Self {
				let time = kio::clock::Manual::new().install(&tokio::runtime::Handle::current());
				time.advance(behind).unwrap();
				let epoch = kio::clock::now()
					.checked_sub(behind)
					.expect("a monotonic clock that far back");
				let clock = moq_mux::Clock::at(epoch, wall).unwrap();
				let mut broadcast = moq_net::broadcast::Info::new().produce();
				let consumer = broadcast.consume();
				let config = moq_mux::catalog::Config::default()
					.with_clock(clock.clone())
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
				let mut producer =
					super::producer(track, &catalog, probe, moq_net::bandwidth::Allocator::unlimited()).await;

				let (opens, rx) = tokio::sync::mpsc::unbounded_channel();
				let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
				// Local: a capture stream is `!Send` on macOS, so each test runs on a `LocalSet`.
				let capturing = clock.clone();
				let task = tokio::task::spawn_local(async move {
					let mut source = Opens(rx);
					// Each subscription opens the camera, so a fixture can idle and restart it.
					let options = Capture {
						kind,
						on_demand: true,
						..Capture::default()
					};
					let state = starting();
					tokio::select! {
						res = capture_loop(&mut producer, &mut source, &options, &capturing, &state) => res?,
						_ = stopped => {}
					}
					producer.finish().await.map(drop)
				});

				Self {
					time,
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
					.subscribe(
						moq_net::track::Subscription::default()
							.with_delay(moq_net::track::Delay::default().with_max(RETAIN)),
					)
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
					return u64::try_from(frame.timestamp.as_micros().unwrap()).unwrap();
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

		/// Each frame maps onto the broadcast clock as it reads then, so a stream follows the clock's
		/// slew toward a stepped wall clock rather than the offset it had when the camera opened.
		#[tokio::test]
		async fn frames_follow_the_broadcast_clocks_slew() {
			tokio::task::LocalSet::new()
				.run_until(async {
					let fixture = Fixture::start(Duration::from_secs(5), kio::clock::wall()).await;
					let mut track = fixture.subscribe().await;
					let camera = fixture.camera();
					camera.push_at(surface(), Instant::now());
					let first = read(&mut track).await;

					// Ten seconds of wall step, slewed at 500 ppm: 50 ms over the next 100 s.
					fixture.time.step_wall(fixture.time.wall() + Duration::from_secs(10));
					fixture.time.advance(Duration::from_secs(100)).unwrap();
					camera.push_at(surface(), Instant::now());
					let published = read(&mut track).await;

					assert!(
						(published - first).abs_diff(100_050_000) <= ROUNDING,
						"{}us apart",
						published - first
					);
					fixture.finish().await;
				})
				.await
		}

		/// A camera whose first frame arrives long after the broadcast began stamps it at its
		/// acquisition: not zero, and not the later instant the loop dequeued it.
		#[tokio::test]
		async fn a_late_first_frame_publishes_its_acquisition() {
			tokio::task::LocalSet::new()
				.run_until(async {
					let fixture = Fixture::start(Duration::from_secs(5), kio::clock::wall()).await;
					let mut track = fixture.subscribe().await;
					let camera = fixture.camera();

					let captured = Instant::now();
					// Delivered well after acquisition: dequeue time must not leak into the timestamp.
					fixture.time.advance(Duration::from_millis(50)).unwrap();
					camera.push_at(surface(), captured);
					let published = read(&mut track).await;

					assert!(published >= 4_000_000, "{published}us restarted the broadcast at zero");
					fixture.assert_acquired(published, captured);
					fixture.finish().await;
				})
				.await
		}

		/// The catalog's stats carry the encoder's target and every frame the camera replaced
		/// before the loop read it, once the loop has read its first.
		#[tokio::test]
		async fn stats_report_the_target_and_skipped_frames() {
			tokio::task::LocalSet::new()
				.run_until(async {
					let fixture = Fixture::start(Duration::from_secs(1), kio::clock::wall()).await;
					let mut track = fixture.subscribe().await;
					let camera = fixture.camera();

					// Replaced before the first read, while the encoder was still opening: not a skip.
					camera.push_at(surface(), Instant::now());
					camera.push_at(surface(), Instant::now());
					read(&mut track).await;
					// Back to back once encoding, so the loop only ever sees the second.
					camera.push_at(surface(), Instant::now());
					camera.push_at(surface(), Instant::now());
					read(&mut track).await;

					let stats = fixture.catalog.stats().remove("video").expect("the video rendition");
					assert_eq!(stats.skipped, 1);
					let ceiling = Config::new(320, 240, crate::FrameRate::new(30, 1).unwrap()).resolved_bitrate();
					assert_eq!(stats.bitrate, Some(ceiling.as_bps()));
					fixture.finish().await;
				})
				.await
		}

		/// Frames the camera replaced after the last read still count when the last viewer
		/// leaves before another read.
		#[tokio::test]
		async fn stats_report_skips_left_when_the_viewer_leaves() {
			tokio::task::LocalSet::new()
				.run_until(async {
					let fixture = Fixture::start(Duration::from_secs(1), kio::clock::wall()).await;
					let mut track = fixture.subscribe().await;
					let camera = fixture.camera();
					camera.push_at(surface(), Instant::now());
					read(&mut track).await;

					// The viewer leaves, then the camera replaces a frame before the loop runs again.
					drop(track);
					camera.push_at(surface(), Instant::now());
					camera.push_at(surface(), Instant::now());
					while !camera.released() {
						tokio::task::yield_now().await;
					}

					let stats = fixture.catalog.stats().remove("video").expect("the video rendition");
					assert_eq!(stats.skipped, 1);
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
					let fixture = Fixture::start(Duration::from_secs(1), kio::clock::wall()).await;
					let mut track = fixture.subscribe().await;
					let camera = fixture.camera();

					// The device numbers from zero, and the clock keeps pace with it.
					camera.push_native(surface(), us(0));
					let first = read(&mut track).await;
					fixture.time.advance(Duration::from_millis(40)).unwrap();
					camera.push_native(surface(), us(40_000));
					let second = read(&mut track).await;
					assert_eq!(second - first, 40_000, "the device's spacing survives");

					// The device restarts its clock without the stream ending, re-anchoring just past
					// the last timestamp and so ahead of the clock.
					camera.push_native(surface(), us(0));
					let restarted = read(&mut track).await;
					assert!(restarted >= second, "{restarted}us rewound behind {second}us");
					fixture.time.advance(Duration::from_millis(40)).unwrap();
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
		/// frame after a resume lands after the idle gap, at its own acquisition.
		#[tokio::test]
		async fn a_restart_after_idle_keeps_the_gap() {
			tokio::task::LocalSet::new()
				.run_until(async {
					let idle = Duration::from_millis(300);
					let fixture = Fixture::start(Duration::from_secs(1), kio::clock::wall()).await;

					let mut track = fixture.subscribe().await;
					let camera = fixture.camera();
					let captured = Instant::now();
					camera.push_at(surface(), captured);
					let before = read(&mut track).await;
					fixture.assert_acquired(before, captured);
					drop(track);
					// The loop releases the camera before the idle gap starts.
					while !camera.released() {
						tokio::task::yield_now().await;
					}
					drop(camera);

					fixture.time.advance(idle).unwrap();

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

		/// A frame that only the drain at a source end returns can still move the rendition to a
		/// new track, and capture follows it there rather than watching the ended one.
		#[tokio::test]
		async fn a_drained_frame_that_moves_the_track_is_followed() {
			tokio::task::LocalSet::new()
				.run_until(async {
					let kind = encoder::Kind::Named(crate::encode::backend::probe::PIPELINED.into());
					let fixture = Fixture::start_on(Duration::from_secs(1), kio::clock::wall(), kind).await;
					let mut track = fixture.subscribe().await;
					let camera = fixture.camera();
					camera.push_native(surface(), us(0));
					camera.close();
					read(&mut track).await;

					// Reopened four times the size, its one frame comes out of the drain alone.
					fixture.time.advance(Duration::from_millis(40)).unwrap();
					let camera = fixture.camera_sized(640, 480);
					camera.push_native(surface_sized(640, 480), us(40_000));
					camera.close();
					while track.read().await.unwrap().is_some() {}

					let mut moved = fixture.subscribe().await;
					let first = read(&mut moved).await;
					// A viewer of the new track opens the camera again.
					fixture.time.advance(Duration::from_millis(40)).unwrap();
					let camera = fixture.camera_sized(640, 480);
					camera.push_native(surface_sized(640, 480), us(80_000));
					camera.close();
					let second = read_new(&mut moved, &[first]).await;
					assert!(second > first, "the new track carries the reopened camera");
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
					let fixture = Fixture::start(Duration::from_secs(1), kio::clock::wall()).await;
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
					// Reopening takes the device a while, so its first frame is newer than the last.
					fixture.time.advance(Duration::from_millis(40)).unwrap();
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
					// Another reopen, another while.
					fixture.time.advance(Duration::from_millis(40)).unwrap();
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
					let now = kio::clock::wall().checked_duration_since(Wall::UNIX_EPOCH).unwrap();
					let wall = Wall::UNIX_EPOCH + Duration::from_secs(now.as_secs() - 3600);
					let fixture = Fixture::start(Duration::from_secs(1), wall).await;
					let advertised = fixture.catalog.snapshot().sync.clock;
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
					assert_eq!(fixture.catalog.snapshot().sync.clock, advertised);
					fixture.finish().await;
				})
				.await
		}
	}

	/// Publish fixtures: the real capture publish against sources that answer, or never do, on cue.
	#[cfg(feature = "openh264")]
	mod publish {
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

		/// [`publish_capture`] from `source` rather than a device.
		async fn publish_from(
			broadcast: moq_net::broadcast::Producer,
			catalog: moq_mux::catalog::Producer,
			options: Capture,
			source: impl CaptureSource,
		) -> Result<(), Error> {
			let (_, driver) = publish_capture(broadcast, catalog, options)?;
			driver.run_from(source).await
		}

		async fn status(publication: &mut Publication, reached: impl Fn(&Status) -> bool) {
			while !reached(publication.state().status()) {
				publication.changed().await.expect("driver still alive");
			}
		}

		/// What the capture is fed next, one at a time.
		enum Step {
			/// A frame 10ms after the previous one, waited on until the first subscriber has it.
			Frame(crate::frame::Surface),
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

		/// Drive the real capture publish through `steps`, returning the info the track advertised and the
		/// payload size of every frame the first subscriber read, grouped as it read them.
		async fn capture_groups(steps: Vec<Step>) -> (moq_net::track::Info, Vec<Vec<usize>>) {
			use moq_mux::container::Container as _;

			let started = kio::clock::Instant::now();
			let result = local(async {
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
				// Ten nominal fps makes the default two-second GOP twenty frames, which every
				// script below stays inside. The capture keeps the probe's camera.
				let rate = crate::FrameRate::new(10, 1).unwrap();
				let (camera, stream) = Synthetic::open(crate::Size::new(320, 240), rate);
				let run = tokio::task::spawn_local(publish_from(
					broadcast.clone(),
					catalog,
					options,
					Streams::new([stream]),
				));
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

				// Each picture the subscriber reads, with the group it arrived in.
				let (published, mut received) = tokio::sync::mpsc::unbounded_channel();
				let collect = tokio::task::spawn_local(async move {
					while let Some(mut group) = subscriber.recv_group().await.unwrap() {
						while let Some(wire) = group.read_frame().await.unwrap() {
							for frame in format.decode(wire).unwrap() {
								// A duration marker closes the group but is not an encoded picture.
								if format.end(&frame).is_none() {
									published.send((group.start, frame.payload.len())).unwrap();
								}
							}
						}
					}
				});

				let mut groups: Vec<Vec<usize>> = Vec::new();
				let mut sequence = None;
				let mut joined = Vec::new();
				let mut millis = 0;
				for step in steps {
					match step {
						Step::Frame(surface) => {
							camera.push_native(surface, us(millis * 1000));
							millis += 10;
							// Wait for delivery before the next step, which also keeps the
							// capture's latest slot from being replaced.
							let (group, len) = received.recv().await.expect("the frame reached the subscriber");
							if sequence != Some(group) {
								sequence = Some(group);
								groups.push(Vec::new());
							}
							groups.last_mut().unwrap().push(len);
						}
						Step::Join => joined.push(consumer.track(&name).unwrap().subscribe(None).await.unwrap()),
					}
				}
				assert!(!run.is_finished(), "the capture ended early: {:?}", run.await);
				run.abort();
				collect.abort();
				(info, groups)
			})
			.await;
			assert_eq!(started.elapsed(), Duration::ZERO);
			result
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
			assert_eq!(info.refresh.ratio, Some(REFRESH));

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
		/// the track info is fixed before the capture learns which one it got.
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
				let track = capture_track(broadcast.clone(), &catalog, &options).unwrap();
				assert_eq!(track.info().refresh.ratio, refresh);
			}
		}

		/// Run `test` on a `LocalSet`, since capture streams are `!Send` on macOS.
		///
		/// Off Apple, encoding waits on an OS thread, so a blocking task keeps Tokio from
		/// auto-advancing the paused clock to a timer deadline while that thread answers.
		async fn local<T>(test: impl std::future::Future<Output = T>) -> T {
			let (hold, released) = tokio::sync::oneshot::channel::<()>();
			let clock = tokio::task::spawn_blocking(move || {
				let _ = released.blocking_recv();
			});
			let result = tokio::task::LocalSet::new().run_until(test).await;
			drop(hold);
			clock.await.unwrap();
			result
		}

		/// The name of the first video rendition `consumer`'s catalog publishes.
		async fn rendition_name(consumer: &moq_net::broadcast::Consumer) -> String {
			let mut catalog = moq_mux::catalog::Consumer::<()>::new(consumer, moq_mux::catalog::CatalogFormat::Hang)
				.await
				.unwrap();
			loop {
				let snapshot = catalog.next().await.unwrap().expect("a catalog");
				if let Some(name) = snapshot.video.renditions.keys().next() {
					return name.clone();
				}
			}
		}

		/// Nobody watching, the capture still encodes, from the probe's camera rather than a
		/// second open, and a subscriber arriving later reads what it wrote meanwhile.
		#[tokio::test(start_paused = true)]
		async fn an_unwatched_capture_keeps_its_past() {
			local(async {
				let mut broadcast = moq_net::broadcast::Info::new().produce();
				let consumer = broadcast.consume();
				let catalog =
					moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
				let mut options = Capture {
					kind: encoder::Kind::Software,
					..Capture::default()
				};
				// Half a frame per second rounds the default two-second GOP down to one frame, so
				// every frame opens a group.
				options.capture.framerate = Some(crate::FrameRate::new(1, 2).unwrap());
				let (camera, stream) = camera();
				let source = Streams::new([stream]);
				let opens = source.opens.clone();
				let run = tokio::task::spawn_local(publish_from(broadcast.clone(), catalog, options, source));

				let name = rendition_name(&consumer).await;
				let mut track = consumer.track(&name).unwrap();
				// Each frame opens a group of its own, starting at the frame's pts.
				camera.push_native(surface(), us(0));
				let first = track.largest().await.expect("the track is live").group;
				camera.push_native(surface(), us(2_000_000));
				while track.largest().await.expect("the track is live").group < first + 2_000_000 {}
				assert_eq!(
					opens.load(Ordering::SeqCst),
					1,
					"the capture reopened the probe's camera"
				);
				assert!(!camera.released(), "the capture released its camera");

				let mut subscriber = consumer
					.track(&name)
					.unwrap()
					.subscribe(
						moq_net::track::Subscription::default()
							.with_delay(moq_net::track::Delay::default().with_max(Duration::from_secs(5))),
					)
					.await
					.unwrap();
				// The groups start their frames' pts apart, wherever the capture's timeline begins.
				let first = subscriber.recv_group().await.unwrap().expect("a cached group").start;
				let second = subscriber.recv_group().await.unwrap().expect("a cached group").start;
				assert_eq!(second - first, 2_000_000);

				assert!(!run.is_finished(), "the capture ended early: {:?}", run.await);
				run.abort();
			})
			.await
		}

		/// On demand, the probe's camera closes at once, and only a viewer opens it again.
		#[tokio::test(start_paused = true)]
		async fn on_demand_opens_the_camera_for_a_viewer() {
			local(async {
				let mut broadcast = moq_net::broadcast::Info::new().produce();
				let consumer = broadcast.consume();
				let catalog =
					moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
				let options = Capture {
					kind: encoder::Kind::Software,
					on_demand: true,
					..Capture::default()
				};
				let (probed, probe) = camera();
				let (camera, stream) = camera();
				let source = Streams::new([probe, stream]);
				let opens = source.opens.clone();
				let (mut publication, driver) = publish_capture(broadcast.clone(), catalog, options).unwrap();
				assert!(matches!(publication.state().status(), Status::Starting));
				let run = tokio::task::spawn_local(driver.run_from(source));

				let name = rendition_name(&consumer).await;
				until(|| probed.released()).await;
				assert_eq!(opens.load(Ordering::SeqCst), 1, "only the probe opened the camera");
				status(&mut publication, |status| matches!(status, Status::Waiting)).await;

				let mut subscriber = consumer.track(&name).unwrap().subscribe(None).await.unwrap();
				camera.push_native(surface(), us(0));
				subscriber.recv_group().await.unwrap().expect("a group");
				assert_eq!(opens.load(Ordering::SeqCst), 2, "the viewer opened the camera");
				assert!(matches!(publication.state().status(), Status::Live));

				drop(subscriber);
				until(|| camera.released()).await;
				status(&mut publication, |status| matches!(status, Status::Waiting)).await;
				assert!(!run.is_finished(), "the capture ended early: {:?}", run.await);
				let mut subscriber = consumer.track(&name).unwrap().subscribe(None).await.unwrap();
				run.abort();
				assert!(run.await.unwrap_err().is_cancelled());
				// Dropping the driver's future cancels the publication and its track.
				let error = loop {
					match subscriber.recv_group().await {
						Ok(Some(_)) => {}
						Ok(None) => panic!("the cancelled track ended cleanly"),
						Err(error) => break error,
					}
				};
				assert!(format!("{error:?}").contains("Cancel"), "{error:?}");
				let mut last = None;
				while let Some(state) = publication.changed().await {
					last = Some(state);
				}
				assert!(matches!(last.as_ref().map(State::status), Some(Status::Cancelled)));
				assert!(publication.is_finished());
			})
			.await
		}

		/// Feed `camera` a frame at a time and drive `run` until the bitrates the probe encoder
		/// accepted satisfy `done`, returning them. The producer follows its grant between frames.
		async fn settle(
			mut run: std::pin::Pin<&mut impl std::future::Future<Output = Result<(), Error>>>,
			camera: &Synthetic,
			micros: &mut u64,
			done: impl Fn(&[u64]) -> bool,
		) -> Vec<u64> {
			let mut accepted = Vec::new();
			loop {
				accepted.extend(probe::bitrates());
				if done(&accepted) {
					return accepted;
				}
				camera.push_native(surface(), us(*micros));
				*micros += 33_333;
				tokio::select! {
					res = &mut run => panic!("the capture ended: {res:?}"),
					() = tokio::task::yield_now() => {}
				}
			}
		}

		/// A watched capture follows its grant. Once nobody watches, it returns to its ceiling
		/// and ignores the grant, and the next viewer's grant applies afresh.
		#[tokio::test(start_paused = true)]
		#[expect(
			clippy::await_holding_lock,
			reason = "the guard only keeps other tests out of the process-wide probe log, and this runtime has one thread, so holding it across an await blocks nothing"
		)]
		async fn an_unwatched_capture_rests_at_its_ceiling() {
			use moq_net::bandwidth::Rate;

			let _probe = probe::exclusive();
			let ceiling = Rate::from_mbps(4);
			let estimate = moq_net::bandwidth::Producer::new();
			estimate.set(Some(Rate::from_mbps(1))).unwrap();
			let bandwidth = moq_net::bandwidth::Allocator::new(estimate.consume());

			let mut broadcast = moq_net::broadcast::Info::new().produce();
			let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
			let track = broadcast
				.create_track("video", catalog.track_info(hang::catalog::PRIORITY.video))
				.unwrap();
			let watched = track.clone();
			let mut rendition = Config::new(320, 240, crate::FrameRate::new(30, 1).unwrap());
			rendition.bitrate = Some(ceiling);
			let mut producer = producer(track, &catalog, rendition, bandwidth.clone()).await;

			let (camera, stream) = camera();
			let mut source = Streams::new([stream]);
			let options = Capture {
				kind: encoder::Kind::Named(probe::NAME.into()),
				bitrate: Some(ceiling),
				bandwidth,
				..Capture::default()
			};
			let clock = catalog.clock();
			let state = starting();
			let mut run = std::pin::pin!(capture_loop(&mut producer, &mut source, &options, &clock, &state));
			let mut micros = 0;

			let viewer = watched.subscribe(None);
			let followed = settle(run.as_mut(), &camera, &mut micros, |set| !set.is_empty()).await;
			assert!(followed.iter().all(|&bps| bps < ceiling.as_bps()), "{followed:?}");

			drop(viewer);
			let rested = settle(run.as_mut(), &camera, &mut micros, |set| {
				set.contains(&ceiling.as_bps())
			})
			.await;
			assert_eq!(rested, [ceiling.as_bps()]);

			// Unwatched, a grant change retunes nothing, however long it holds and however many
			// frames are written meanwhile.
			estimate.set(Some(Rate::from_kbps(500))).unwrap();
			for _ in 0..3 {
				camera.push_native(surface(), us(micros));
				micros += 33_333;
				tokio::select! {
					res = &mut run => panic!("the capture ended: {res:?}"),
					() = kio::clock::sleep(Duration::from_secs(20)) => {}
				}
			}
			assert_eq!(probe::bitrates(), Vec::<u64>::new());

			let _viewer = watched.subscribe(None);
			let resumed = settle(run.as_mut(), &camera, &mut micros, |set| !set.is_empty()).await;
			assert!(resumed.iter().all(|&bps| bps <= 500_000), "{resumed:?}");
		}

		/// A backend that cannot cut still captures, leaving every group to its cadence.
		#[tokio::test]
		#[expect(
			clippy::await_holding_lock,
			reason = "the guard only keeps other tests out of the process-wide probe log, and this runtime has one thread, so holding it across an await blocks nothing"
		)]
		async fn a_backend_that_cannot_cut_still_captures() {
			let _probe = probe::exclusive();

			let mut broadcast = moq_net::broadcast::Info::new().produce();
			let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
			let track = broadcast
				.create_track("video", catalog.track_info(hang::catalog::PRIORITY.video))
				.unwrap();
			let _subscriber = track.subscribe(None);
			let rendition = Config::new(320, 240, crate::FrameRate::new(30, 1).unwrap());
			let mut producer = producer(track, &catalog, rendition, moq_net::bandwidth::Allocator::unlimited()).await;

			let (camera, stream) = camera();
			let mut source = Streams::new([stream]);
			let options = Capture {
				kind: encoder::Kind::Named(probe::NO_CUT.into()),
				..Capture::default()
			};
			let clock = catalog.clock();
			let state = starting();
			let mut run = std::pin::pin!(capture_loop(&mut producer, &mut source, &options, &clock, &state));

			// The probe holds its first frame back, so this one reaches the codec but not the track.
			camera.push_native(surface(), us(0));
			tokio::select! {
				res = &mut run => panic!("the capture ended: {res:?}"),
				() = until(|| probe::take().iter().any(|(event, _)| *event == "encode")) => {}
			}
		}

		/// A camera that ends drains the encoder into the track before dropping it, so the
		/// frame a pipelining backend still holds is published rather than lost.
		#[tokio::test]
		async fn a_source_end_publishes_the_frames_the_encoder_holds() {
			let mut broadcast = moq_net::broadcast::Info::new().produce();
			let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
			let track = broadcast
				.create_track("video", catalog.track_info(hang::catalog::PRIORITY.video))
				.unwrap();
			let rendition = Config::new(320, 240, crate::FrameRate::new(30, 1).unwrap());
			let mut producer = producer(track, &catalog, rendition, moq_net::bandwidth::Allocator::unlimited()).await;
			let snapshot = catalog.snapshot();
			let container =
				moq_mux::catalog::hang::Container::try_from(snapshot.video.renditions.get("video").unwrap()).unwrap();
			let subscriber = broadcast
				.consume()
				.track("video")
				.unwrap()
				.subscribe(
					moq_net::track::Subscription::default()
						.with_delay(moq_net::track::Delay::default().with_max(Duration::from_secs(600))),
				)
				.await
				.unwrap();
			let mut published = moq_mux::container::Consumer::new(subscriber, container);

			let (camera, stream) = camera();
			let mut source = Streams::new([stream]);
			let opens = source.opens.clone();
			let options = Capture {
				kind: encoder::Kind::Named(probe::PIPELINED.into()),
				..Capture::default()
			};
			let clock = catalog.clock();
			let state = starting();
			let run = capture_loop(&mut producer, &mut source, &options, &clock, &state);

			// Held back by one, the only frame reaches the track through the drain alone.
			camera.push_native(surface(), us(0));
			camera.close();
			// The loop reopening the camera has handled the end.
			tokio::select! {
				res = run => panic!("the capture ended: {res:?}"),
				() = until(|| opens.load(Ordering::SeqCst) == 2) => {}
			}
			let _finished = producer.finish().await.unwrap();
			loop {
				match published.read().await.unwrap() {
					Some(moq_mux::container::Read::Frame { .. }) => break,
					Some(_) => {}
					None => panic!("the encoder's last frame was never published"),
				}
			}
		}
	}
}
