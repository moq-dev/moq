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
//! one a caller with its own frames writes to. [`Capture::renditions`] adds one per smaller
//! simulcast rendition, each scaling the captured frame on its encoder's thread and chained below
//! the next larger through [`Options::lower`](super::Options::lower).

use moq_mux::catalog::hang::CatalogExt;
use std::task::Poll;

use moq_net::Timestamp;

use crate::{Color, Error, FrameRate, Size, capture};

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
	/// Smaller renditions published beside the captured one, each scaled from the captured frame.
	///
	/// They form a simulcast ladder on [`bandwidth`](Self::bandwidth): the smallest is funded
	/// first, and a larger one, the captured rendition included, stops encoding while the grant
	/// can't fund it. Each encodes only while someone watches it, whatever
	/// [`on_demand`](Self::on_demand) says, and with `on_demand` the source stays open while any
	/// rendition is watched. A name that repeats fails [`publish_capture`].
	pub renditions: Vec<Rendition>,
}

/// A smaller rendition a [`Capture`] scales from its frames, published as `video/<name>`.
///
/// Its bitrate is the default for its size, frame rate, and codec, which must come out below
/// the next larger rendition's, or the capture fails once it learns the source's size.
#[derive(Clone, Debug, PartialEq)]
pub struct Rendition {
	name: String,
	scale: f64,
}

impl Rendition {
	/// A rendition named `name` holding `scale` of the captured pixels, such as 0.25 for half
	/// the width and half the height.
	///
	/// Refuses a scale outside (0, 1), and a name that is empty or holds a `/`.
	pub fn new(name: impl Into<String>, scale: f64) -> Result<Self, Error> {
		let name = name.into();
		if name.is_empty() || name.contains('/') {
			return Err(Error::UnsupportedConfig(format!(
				"rendition name `{name}` must be non-empty without a `/`"
			)));
		}
		// Written so NaN fails too.
		if !(scale > 0.0 && scale < 1.0) {
			return Err(Error::UnsupportedConfig(format!(
				"rendition `{name}` scale {scale} is not between 0 and 1"
			)));
		}
		Ok(Self { name, scale })
	}

	/// The name, which publishes as `video/<name>`.
	pub fn name(&self) -> &str {
		&self.name
	}

	/// The fraction of the captured pixels it holds.
	pub fn scale(&self) -> f64 {
		self.scale
	}

	/// The track it publishes on.
	fn track(&self) -> String {
		format!("video/{}", self.name)
	}

	/// Its size for a `source` frame: each side scaled by the square root, rounded down to even
	/// for 4:2:0 chroma.
	fn size(&self, source: Size) -> Result<Size, Error> {
		let ratio = self.scale.sqrt();
		// Truncating is the rounding down; the product never exceeds the source side.
		let side = |pixels: u32| (f64::from(pixels) * ratio) as u32 & !1;
		let size = Size::new(side(source.width), side(source.height));
		if size.width == 0 || size.height == 0 {
			return Err(Error::UnsupportedConfig(format!(
				"rendition `{}` scales a {source} source to nothing",
				self.name
			)));
		}
		Ok(size)
	}
}

/// Parses `name=scale`, as the `moq` CLI's `--rendition` takes it.
impl std::str::FromStr for Rendition {
	type Err = Error;

	fn from_str(arg: &str) -> Result<Self, Error> {
		let (name, scale) = arg
			.split_once('=')
			.ok_or_else(|| Error::UnsupportedConfig(format!("expected NAME=SCALE, got `{arg}`")))?;
		let scale = scale
			.parse()
			.map_err(|err| Error::UnsupportedConfig(format!("invalid rendition scale `{scale}`: {err}")))?;
		Self::new(name, scale)
	}
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

	/// The encoders for a `source` camera at `framerate` whose frames are in `color`, one per
	/// rendition in ladder order: the scaled ones smallest first, then the captured size.
	///
	/// Fails when a rendition's ceiling isn't below the next one's, since the ladder could
	/// never fund the larger without the smaller.
	fn encoders(
		&self,
		source: Size,
		framerate: FrameRate,
		color: Option<Color>,
	) -> Result<Vec<encoder::Config>, Error> {
		let mut configs = Vec::with_capacity(self.renditions.len() + 1);
		for rendition in &self.renditions {
			let size = rendition.size(source)?;
			let mut config = self.encoder(size.width, size.height, framerate);
			// Derived from the scaled size: the caller's bitrate is the captured rendition's.
			config.bitrate = None;
			// The encoder scales each frame on its own thread or device.
			config.resize = Some(crate::resize::Config::default());
			// A scaled frame keeps its source's space, which the scaled size could infer otherwise.
			config.color = Some(color.unwrap_or_else(|| Color::infer(source)));
			configs.push(config);
		}
		let mut captured = self.encoder(source.width, source.height, framerate);
		captured.color = color;
		configs.push(captured);

		let names = self.renditions.iter().map(Rendition::track);
		let names: Vec<_> = names
			.chain(std::iter::once("the captured rendition".to_string()))
			.collect();
		for (index, pair) in configs.windows(2).enumerate() {
			let (lower, upper) = (pair[0].resolved_bitrate(), pair[1].resolved_bitrate());
			if lower >= upper {
				return Err(Error::UnsupportedConfig(format!(
					"{}'s {} bps at {} is not below {}'s {} bps at {}",
					names[index],
					lower.as_bps(),
					pair[0].size(),
					names[index + 1],
					upper.as_bps(),
					pair[1].size(),
				)));
			}
		}
		Ok(configs)
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

/// Capture a webcam and publish it as a video track, plus one per [`Capture::renditions`].
///
/// Registers the tracks before anything about its source is known, and returns a
/// handle that observes the publication plus the [`Driver`] that captures it. The
/// driver opens the source to probe the mode it negotiates, so the catalog
/// rendition is exact before a frame is published, then captures and encodes
/// whether or not anyone is watching. Set [`Capture::on_demand`] to capture only
/// while a subscriber is watching instead.
pub fn publish_capture<E: CatalogExt>(
	broadcast: moq_net::broadcast::Producer,
	catalog: moq_mux::catalog::Producer<E>,
	mut options: Capture,
) -> Result<(Publication, Driver<E>), Error> {
	let mut names = std::collections::HashSet::new();
	if let Some(repeated) = options
		.renditions
		.iter()
		.find(|rendition| !names.insert(rendition.name()))
	{
		return Err(Error::UnsupportedConfig(format!(
			"rendition `{}` is named twice",
			repeated.name()
		)));
	}
	// Ladder order, which every later step follows.
	options.renditions.sort_by(|a, b| a.scale.total_cmp(&b.scale));
	let tracks = capture_tracks(broadcast, &catalog, &options)?;
	let state = starting();
	let publication = Publication {
		state: state.consume(),
		observed: 0,
	};
	let driver = Driver {
		owned: Owned::Tracks(tracks),
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

/// What a [`Driver`] owns of its tracks, one phase at a time, held in the driver so a
/// dropped [`run`](Driver::run) future still cancels them. Each phase holds one per rendition, in
/// ladder order.
enum Owned {
	/// Registered, until the probe knows their renditions.
	Tracks(Vec<moq_net::track::Producer>),
	/// Probed, and wrapped in the producers that encode and publish them.
	Producers(Vec<Producer>),
	/// Ended or cancelled, so nothing is left to end.
	Done,
}

impl Owned {
	/// End the tracks cleanly, best effort: they are usually already going away.
	async fn finish(self) {
		let ended: Vec<Result<(), Error>> = match self {
			// The caller is done with the tracks, so the finished handles go with them.
			Self::Tracks(tracks) => tracks
				.into_iter()
				.map(|track| track.finish().map(drop).map_err(Error::from))
				.collect(),
			// The capture is gone, so its encoders' tails have nothing to follow. Released, an
			// encoder leaves nothing to drain, so the finish completes without waiting on it.
			Self::Producers(producers) => {
				let mut ended = Vec::with_capacity(producers.len());
				for mut producer in producers {
					ended.push(match producer.release() {
						Ok(()) => producer.finish().await.map(drop),
						Err(err) => Err(err),
					});
				}
				ended
			}
			Self::Done => Vec::new(),
		};
		for err in ended.into_iter().filter_map(Result::err) {
			tracing::debug!(error = %err, "video track finish after capture ended");
		}
	}

	/// End the tracks with `err`. A track that already ended has nobody left to tell.
	fn abort(self, err: moq_net::Error) {
		match self {
			Self::Tracks(tracks) => {
				for track in tracks {
					let _ = track.abort(err.clone());
				}
			}
			Self::Producers(producers) => {
				for producer in producers {
					producer.abort(err.clone());
				}
			}
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

	/// Probe `source`, wrap each track in a producer for its rendition, then run the capture loop.
	async fn capture<S: CaptureSource>(&mut self, mut source: S) -> Result<(), Error> {
		let clock = self.catalog.clock();
		let options = &self.options;

		// Open the camera to find out what it actually negotiated, since a requested size is only a
		// hint (macOS ignores it outright) and the encoders are built from the mode, not the request.
		// This buys renditions that say exactly what the streams will carry, rather than ones every
		// consumer has to treat as provisional, and lets a consumer size itself against them without
		// waiting for an encoder that may not run yet.
		let (renditions, configs, camera) = async {
			let camera = source.open(&options.capture).await?;
			let framerate = options
				.capture
				.framerate
				.or_else(|| camera.framerate())
				.unwrap_or(DEFAULT_FRAMERATE);
			let configs = options.encoders(Size::new(camera.width(), camera.height()), framerate, camera.color())?;
			// One at a time, so only one probe holds a codec session.
			let mut renditions = Vec::with_capacity(configs.len());
			for config in &configs {
				renditions.push(config.probe().await?);
			}
			// The encoders are built from the same defaults, so each probe's join is the one its
			// encoder emits, and its ceiling the one its track first claims.
			Ok::<_, Error>((renditions, configs, camera))
		}
		.await?;

		// Each producer takes a clone, so a setup failure leaves the registered tracks for `run`
		// to abort, and success drops ours so they never outlive the producers'.
		let Owned::Tracks(tracks) = &self.owned else {
			unreachable!("a driver runs once")
		};
		let mut producers: Vec<Producer> = Vec::with_capacity(tracks.len());
		for ((track, config), rendition) in tracks.iter().zip(configs).zip(renditions) {
			// Each rendition is the next rung up from the one before it.
			let lower = producers.last().map(Producer::rung);
			producers.push(Producer::with_track(
				track.clone(),
				&self.catalog,
				config,
				rendition,
				options.bandwidth.clone(),
				lower,
			)?);
		}
		self.owned = Owned::Producers(producers);
		let Owned::Producers(producers) = &mut self.owned else {
			unreachable!("just set")
		};
		// Capturing from the start, the loop keeps the probe's camera rather than reopening it,
		// which could mean a second permission prompt or picker. On demand it closes until a viewer.
		let mut source = Opened {
			stream: (!options.on_demand).then_some(camera),
			source,
		};
		capture_loop(producers, &mut source, options, &clock, &self.state).await
	}
}

/// Register the capture's video tracks in ladder order, before anything about its source is known:
/// one per scaled rendition, then the captured one.
///
/// Takes `broadcast` only to mint the tracks, which outlive it.
fn capture_tracks<E: CatalogExt>(
	broadcast: moq_net::broadcast::Producer,
	catalog: &moq_mux::catalog::Producer<E>,
	options: &Capture,
) -> Result<Vec<moq_net::track::Producer>, Error> {
	// Advertised only when every backend the encoder may open honors a cut, since the info
	// is fixed before the capture learns which one it got.
	let ratio = super::backend::always_cuts(options.codec, &options.kind).then_some(REFRESH);
	let refresh = moq_net::track::Refresh::default().with_ratio(ratio);
	let mut tracks: Vec<moq_net::track::Producer> = Vec::with_capacity(options.renditions.len() + 1);
	// Each rung takes the priority one below the rendition under it, as `Options::lower` does,
	// so the allocator funds the smallest first.
	let mut priority = hang::catalog::PRIORITY.video;
	let names = options.renditions.iter().map(|rendition| Some(rendition.track()));
	for name in names.chain(std::iter::once(None)) {
		let info = catalog.track_info(priority).with_refresh(refresh);
		let created = match name {
			Some(name) => broadcast.create_track(name, info).ok_or(moq_net::Error::Duplicate),
			None => Ok(broadcast.unique_track(suffix(options.codec), info)),
		};
		match created {
			Ok(track) => tracks.push(track),
			Err(err) => {
				// Nobody can have subscribed yet, but say why rather than drop them unfinished.
				for track in tracks {
					let _ = track.abort(err.clone());
				}
				return Err(err.into());
			}
		}
		priority = priority.saturating_sub(1);
	}
	Ok(tracks)
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
/// while some viewer wants any rendition; otherwise it runs, and never releases, while the tracks
/// are open. Errors once a rendition's track closes.
///
/// Takes each rendition's track from [`demands`], so read them again after a write, which can
/// move one.
async fn until_capturing(mut watched: Vec<Watched>, on_demand: bool, run: bool) -> Result<(), moq_net::Error> {
	kio::wait(move |waiter| {
		let mut wanted = false;
		for Watched { demand, moving } in &mut watched {
			// Drained to pending, which arms `waiter` for this track's next change.
			loop {
				match demand.poll_wanted_changed(waiter) {
					Poll::Ready(Ok(_)) => continue,
					// The write still running may have moved it to a new track. Counted as wanted
					// until it returns, which wakes the caller to read the track again.
					Poll::Ready(Err(_)) if *moving => {
						wanted = true;
						break;
					}
					Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
					Poll::Pending => break,
				}
			}
			wanted |= demand.wanted();
		}
		match (wanted || !on_demand) == run {
			true => Poll::Ready(Ok(())),
			false => Poll::Pending,
		}
	})
	.await
}

/// A rendition's track as the capture last read it.
struct Watched {
	demand: moq_net::track::Demand,
	/// Whether a write that may move it to a new track is still running.
	moving: bool,
}

/// The track each rendition publishes on now.
fn demands(producers: &[Producer]) -> Vec<Watched> {
	producers
		.iter()
		.map(|producer| Watched {
			demand: producer.demand(),
			moving: false,
		})
		.collect()
}

// Race source setup against capture releasing its source, so an on-demand capture its last
// viewer left stops waiting promptly. `None` when it released.
async fn wait_capture<T>(
	demands: Vec<Watched>,
	on_demand: bool,
	work: impl std::future::Future<Output = Result<T, Error>>,
) -> Result<Option<T>, Error> {
	tokio::select! {
		biased;
		Ok(()) = until_capturing(demands, on_demand, false) => Ok(None),
		res = work => res.map(Some),
	}
}

/// Run `work` side by side, waiting for all of it, and return the first error.
async fn join_all<F: std::future::Future<Output = Result<(), Error>>>(
	work: impl IntoIterator<Item = F>,
) -> Result<(), Error> {
	let mut work: Vec<_> = work.into_iter().map(|work| Some(Box::pin(work))).collect();
	let mut result = Ok(());
	std::future::poll_fn(|cx| {
		let mut pending = false;
		for slot in &mut work {
			let Some(running) = slot else { continue };
			match running.as_mut().poll(cx) {
				Poll::Ready(done) => {
					if result.is_ok() {
						result = done;
					}
					*slot = None;
				}
				Poll::Pending => pending = true,
			}
		}
		match pending {
			true => Poll::Pending,
			false => Poll::Ready(()),
		}
	})
	.await;
	result
}

/// The first of `steps` to finish, with its index, clearing its slot. Pending while none runs.
fn next_step<F: std::future::Future + Unpin>(
	steps: &mut [Option<F>],
) -> impl std::future::Future<Output = (usize, F::Output)> + '_ {
	std::future::poll_fn(move |cx| {
		for (index, slot) in steps.iter_mut().enumerate() {
			if let Some(step) = slot
				&& let Poll::Ready(done) = std::pin::Pin::new(step).poll(cx)
			{
				*slot = None;
				return Poll::Ready((index, done));
			}
		}
		Poll::Pending
	})
}

/// What a rendition does with one source frame.
enum Work {
	/// Nobody watches it: stop its encoder if one was encoding, and follow the grant unwritten.
	Unwatched { stop: bool },
	/// Encode the frame, cutting a group at it first when asked.
	Write {
		frame: std::sync::Arc<crate::Frame>,
		cut: bool,
	},
}

/// Do `work` on `producer`, handing it back for the next frame.
async fn step(producer: &mut Producer, work: Work) -> (&mut Producer, Result<(), Error>) {
	let result = run_step(&mut *producer, work).await;
	(producer, result)
}

async fn run_step(producer: &mut Producer, work: Work) -> Result<(), Error> {
	match work {
		Work::Unwatched { stop } => {
			// Its last viewer left: drain and close its encoder, so the next opens fresh.
			if stop {
				producer.stop().await?;
			}
			// A rung disabled under congestion loses its viewers to the one below, so it follows
			// the grant meanwhile to re-enable once funded, and they can come back.
			producer.follow_unwritten().await;
		}
		Work::Write { frame, cut } => {
			if cut {
				producer.cut().await?;
			}
			producer.write(frame).await?;
		}
	}
	Ok(())
}

/// One rendition as the capture loop sees it between its steps.
struct Lane {
	/// Whether frames are going to its encoder, so it stops once nobody watches.
	encoding: bool,
	/// Its keyframe asks, fresh for every encoder it opens.
	forced: Cuts,
	/// Whether its open encoder honors a cut, once one has opened.
	cuts: Option<bool>,
	/// The last source frame it took, as its place in the camera's sequence.
	seen: Option<u64>,
	/// Its track as of its last step.
	demand: moq_net::track::Demand,
	/// Whether a write was cancelled mid-call, leaving its encoder refusing every later call.
	cancelled: bool,
}

impl Lane {
	fn new(producer: &Producer) -> Self {
		Self {
			encoding: false,
			forced: Cuts::default(),
			cuts: None,
			seen: None,
			demand: producer.demand(),
			cancelled: false,
		}
	}

	/// What `producer` does with source frame `seq`, as its `watched` rendition or not.
	fn take(&mut self, producer: &Producer, seq: u64, frame: &std::sync::Arc<crate::Frame>, watched: bool) -> Work {
		let seen = self.seen.replace(seq);
		if !watched {
			return Work::Unwatched {
				stop: std::mem::take(&mut self.encoding),
			};
		}
		match (self.encoding, seen) {
			// The frames between went to no encoder of its: the camera replaced them, or this
			// rendition was still busy with an earlier one.
			(true, Some(seen)) => producer.report_skipped(seq - seen - 1),
			(true, None) => {}
			(false, _) => {
				self.encoding = true;
				self.forced = Cuts::default();
				self.cuts = None;
			}
		}
		// A backend that cannot cut leaves every group to its cadence.
		let wanted = self.cuts.unwrap_or(false).then(|| producer.wanted()).flatten();
		Work::Write {
			frame: frame.clone(),
			cut: self.forced.due(wanted),
		}
	}
}

/// Why the capture loop stopped reading frames from its camera.
enum Stopping {
	/// The camera's stream ended, so every rendition finishes the frames already read.
	Ended,
	/// Nobody watches any rendition, so the writes in flight are cancelled.
	Released,
}

/// Async capture/encode loop over `producers`, the renditions in ladder order with the captured
/// one last. Opens the camera and keeps it open, or [`on_demand`](Capture::on_demand), opens it
/// while at least one viewer is watching a rendition and releases it when the last one leaves.
/// Every scaled rendition, and the captured one on demand, encodes only while watched. It never
/// returns on its own; dropping its future stops it.
///
/// Each rendition runs on its own: it takes the newest frame it hasn't seen whenever its last
/// one is done, so an encoder that opens, drains, or encodes slowly misses frames itself rather
/// than holding the others back. A frame it missed counts among its skipped ones.
///
/// Each [`Producer`] holds its encoder, its track's claim on the connection, and rate control,
/// so they outlive a closed camera: a reopen at another mode moves the claim's ceiling rather
/// than lapsing it.
///
/// Cancel safety: every wait here is a real `.await` (a frame read, a demand
/// transition, or an encode), so dropping this future (e.g. on Ctrl+C) drops
/// `camera`, which releases the device (LED off); the encode threads are joined
/// once the producers go. Both the capture and encode threads sit idle between
/// frames, so their joins return promptly unless the underlying device or encoder
/// is itself wedged.
async fn capture_loop<S: CaptureSource>(
	producers: &mut [Producer],
	source: &mut S,
	options: &Capture,
	clock: &moq_mux::Clock,
	state: &kio::Producer<Published>,
) -> Result<(), Error> {
	let capture = &options.capture;
	let on_demand = options.on_demand;
	// Off demand the captured rendition encodes unwatched, so a late viewer finds the recent past.
	let retained = (!on_demand).then(|| producers.len() - 1);
	// The last timestamp sent to an encoder, which a reopened camera's frames must exceed.
	let mut last: Option<Timestamp> = None;

	loop {
		// On demand, idle until a viewer subscribes. The catalog renditions were published when
		// the tracks were created, so a subscriber can get here without a frame ever having been
		// encoded. Either way a track ending is a clean exit.
		if on_demand {
			publish_status(state, Status::Waiting);
		}
		if let Err(err) = until_capturing(demands(producers), on_demand, true).await {
			log_track_ended(err);
			return Ok(());
		}
		publish_status(state, Status::Starting);

		// Open the camera, and size every encoder to its negotiated mode.
		let Some(mut camera) = wait_capture(demands(producers), on_demand, source.open(capture)).await? else {
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
		let configs = options.encoders(Size::new(camera.width(), camera.height()), framerate, camera.color())?;
		// Smallest first, so each rung's ceiling moves before the one above reads it. No encoder
		// opens until a frame is written: a fresh one opens with a keyframe on every backend, so
		// the viewer whose subscription reopened the camera can decode from its first frame, and a
		// backend that cannot cut still captures.
		for (producer, config) in producers.iter_mut().zip(configs) {
			producer.configure(config)?;
		}
		tracing::info!(device = camera.label(), "capturing");
		publish_status(state, Status::Live);

		let mut lanes: Vec<Lane> = producers.iter().map(Lane::new).collect();
		// The camera's skips before its first read are startup, not frames a rendition missed.
		let mut startup = None;
		// Frames read, and the newest of them with its place in the camera's sequence.
		let mut reads: u64 = 0;
		let mut latest: Option<(u64, std::sync::Arc<crate::Frame>)> = None;
		let mut stopping = None;

		{
			// A rendition's producer while it waits for a frame; its step holds it meanwhile.
			let mut idle: Vec<Option<&mut Producer>> = producers.iter_mut().map(Some).collect();
			let mut steps: Vec<Option<std::pin::Pin<Box<_>>>> = idle.iter().map(|_| None).collect();
			loop {
				// Hand the newest frame to every rendition that is free and hasn't taken it.
				if let Some((seq, frame)) = &latest
					&& !matches!(stopping, Some(Stopping::Released))
				{
					for (index, (slot, lane)) in idle.iter_mut().zip(&mut lanes).enumerate() {
						if lane.seen >= Some(*seq) {
							continue;
						}
						let Some(producer) = slot.take() else { continue };
						let watched = retained == Some(index) || producer.demand().wanted();
						let work = lane.take(producer, *seq, frame, watched);
						steps[index] = Some(Box::pin(step(producer, work)));
					}
				}
				if stopping.is_some() && steps.iter().all(Option::is_none) {
					break;
				}

				// Race the next frame against demand, so an on-demand capture releases the
				// camera promptly when the last viewer leaves. `biased` checks demand first so an
				// unwatched on-demand capture stops before reading another frame, then frees a
				// rendition whose step finished so it can take that frame.
				let watching = lanes
					.iter()
					.zip(&steps)
					.map(|(lane, step)| Watched {
						demand: lane.demand.clone(),
						moving: step.is_some(),
					})
					.collect();
				tokio::select! {
					biased;
					released = until_capturing(watching, on_demand, false), if stopping.is_none() => match released {
						Ok(()) => {
							// A write released mid-encode leaves its encoder refusing the next call,
							// so the release below drops it. A rendition stopping finishes.
							for (lane, step) in lanes.iter_mut().zip(&mut steps) {
								if lane.encoding && step.take().is_some() {
									lane.cancelled = true;
								}
							}
							stopping = Some(Stopping::Released);
						}
						Err(err) => {
							log_track_ended(err);
							return Ok(());
						}
					},
					(index, (producer, result)) = next_step(&mut steps) => {
						result?;
						let lane = &mut lanes[index];
						if lane.encoding && lane.cuts.is_none() {
							// A reopen can land on another backend than the probe did, so ask this one.
							lane.cuts = producer.applied().map(|applied| applied.cut);
							if let Some(encoder) = producer.name() {
								tracing::info!(track = %producer.demand().name(), encoder, "encoding");
							}
						}
						// A write can move the rendition to a new track.
						lane.demand = producer.demand();
						idle[index] = Some(producer);
					},
					// A read error is terminal for this selection (the source is gone or was
					// refused); `None` just ends the stream, so reopen below.
					frame = camera.read(), if stopping.is_none() => match frame? {
						None => stopping = Some(Stopping::Ended),
						Some(mut frame) => {
							let skipped = camera.skipped();
							let startup = *startup.get_or_insert(skipped);
							let seq = reads + (skipped - startup);
							reads += 1;
							// Capture timestamps use a private monotonic timeline. The offset to the
							// broadcast clock is sampled per frame, so each queued frame maps by
							// acquisition, not dequeue, time, and the stream follows the clock's slew
							// toward the wall clock and its jump across a suspend. Reads straddling a
							// stall would skew it, so those keep the last offset. The first after an
							// open has none to keep and fixes the reopen lead, so it reads again instead.
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
							// A camera never rewinds its own stream, but a reopened one maps from the
							// shared clock, which the previous stream can lead: a device re-anchor
							// floors just past its last timestamp, ahead of arrival. Shift the new
							// stream past it, keeping its spacing.
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
							latest = Some((seq, std::sync::Arc::new(frame)));
						}
					},
				}
			}
		}

		// Frames the camera replaced or a rendition missed after the last it took: no frame
		// follows to report them.
		if let Some(startup) = startup {
			let next = reads + (camera.skipped() - startup);
			for (producer, lane) in producers.iter().zip(&lanes) {
				if lane.encoding
					&& let Some(seen) = lane.seen
				{
					producer.report_skipped(next - seen - 1);
				}
			}
		}
		// Drop the camera (LED off) and encoders before opening another. The shared clock keeps
		// advancing while capture is stopped, so mark the break before waiting for demand again,
		// so the next timestamp does not stretch the previous frame across the gap. A backend
		// that pipelines (Media Foundation, MediaCodec) still holds the last frames, so drain them
		// into the track first, unless a cancelled encode left the encoder refusing calls. The
		// drained frames can move a rendition to a new track, as any written frame can, which the
		// next `until_capturing` reads.
		drop(camera);
		let stops = producers.iter_mut().zip(&lanes).map(|(producer, lane)| async move {
			match (lane.cancelled, lane.encoding) {
				(true, _) => producer.release(),
				(false, true) => producer.stop().await,
				(false, false) => Ok(()),
			}
		});
		join_all(stops).await?;
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

	/// A rendition takes a fraction strictly between nothing and the whole picture, under a name
	/// that is one track segment.
	#[test]
	fn a_rendition_refuses_what_it_cannot_publish() {
		for (name, scale) in [
			("sd", 0.0),
			("sd", 1.0),
			("sd", -0.5),
			("sd", f64::NAN),
			("", 0.5),
			("a/b", 0.5),
		] {
			assert!(Rendition::new(name, scale).is_err(), "{name:?} at {scale}");
		}
		let parsed: Rendition = "sd=0.25".parse().unwrap();
		assert_eq!(parsed, Rendition::new("sd", 0.25).unwrap());
		assert_eq!(parsed.track(), "video/sd");
		for arg in ["sd", "sd=", "sd=quarter", "=0.25", "sd=2"] {
			assert!(arg.parse::<Rendition>().is_err(), "{arg}");
		}
	}

	/// A scale is a fraction of the pixels, so each side takes its square root, rounded down to
	/// even, and a source too small to scale is refused rather than encoded at nothing.
	#[test]
	fn a_rendition_scales_its_pixels() {
		let quarter = Rendition::new("sd", 0.25).unwrap();
		assert_eq!(quarter.size(Size::new(1920, 1080)).unwrap(), Size::new(960, 540));
		assert_eq!(quarter.size(Size::new(1279, 721)).unwrap(), Size::new(638, 360));
		let half = Rendition::new("hd", 0.5).unwrap();
		assert_eq!(half.size(Size::new(1920, 1080)).unwrap(), Size::new(1356, 762));
		assert!(Rendition::new("tiny", 0.01).unwrap().size(Size::new(10, 10)).is_err());
	}

	/// The ladder's encoders run smallest first with the captured size last, each scaled one at
	/// its own default bitrate in the source's color space, and a captured ceiling that a scaled
	/// one reaches is refused.
	#[test]
	fn the_ladder_derives_each_rung() {
		let framerate = crate::FrameRate::new(30, 1).unwrap();
		let source = Size::new(1920, 1080);
		let mut options = Capture {
			bitrate: Some(moq_net::bandwidth::Rate::from_mbps(5)),
			renditions: vec![Rendition::new("ld", 0.1).unwrap(), Rendition::new("sd", 0.25).unwrap()],
			..Capture::default()
		};
		let configs = options.encoders(source, framerate, None).unwrap();
		let sizes: Vec<_> = configs.iter().map(encoder::Config::size).collect();
		assert_eq!(sizes, [Size::new(606, 340), Size::new(960, 540), source]);
		let sd = &configs[1];
		assert_eq!(sd.bitrate, None);
		assert!(sd.resize.is_some());
		// 540 lines would infer standard definition, but the pixels are the HD source's.
		assert_eq!(sd.color, Some(Color::infer(source)));
		assert_eq!(configs[2].resolved_bitrate(), moq_net::bandwidth::Rate::from_mbps(5));

		// The quarter's default at 540p30 tops this ceiling.
		options.bitrate = Some(moq_net::bandwidth::Rate::from_kbps(500));
		let err = options.encoders(source, framerate, None).unwrap_err();
		assert!(format!("{err}").contains("video/sd"), "{err}");
	}

	/// Two renditions under one name would publish one track twice.
	#[test]
	fn a_repeated_rendition_name_is_refused() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let options = Capture {
			renditions: vec![Rendition::new("sd", 0.25).unwrap(), Rendition::new("sd", 0.5).unwrap()],
			..Capture::default()
		};
		assert!(publish_capture(broadcast, catalog, options).is_err());
	}

	/// The ladder registers in order: each scaled track at `video/<name>`, smallest first and
	/// funded first, then the captured one.
	#[test]
	fn the_ladder_registers_smallest_first() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let mut options = Capture {
			renditions: vec![Rendition::new("sd", 0.25).unwrap(), Rendition::new("ld", 0.1).unwrap()],
			..Capture::default()
		};
		options.renditions.sort_by(|a, b| a.scale.total_cmp(&b.scale));
		let tracks = capture_tracks(broadcast, &catalog, &options).unwrap();
		let names: Vec<_> = tracks.iter().map(|track| track.name().to_string()).collect();
		assert_eq!(&names[..2], ["video/ld", "video/sd"]);
		assert!(names[2].ends_with(".avc3"), "{names:?}");
		let priorities: Vec<_> = tracks.iter().map(|track| track.info().priority).collect();
		let video = hang::catalog::PRIORITY.video;
		assert_eq!(priorities, [video, video - 1, video - 2]);
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
						res = capture_loop(std::slice::from_mut(&mut producer), &mut source, &options, &capturing, &state) => res?,
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
				let track = capture_tracks(broadcast.clone(), &catalog, &options)
					.unwrap()
					.pop()
					.unwrap();
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
			let mut run = std::pin::pin!(capture_loop(
				std::slice::from_mut(&mut producer),
				&mut source,
				&options,
				&clock,
				&state
			));
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

		/// The catalog's video renditions once it lists `count` of them.
		async fn renditions(
			consumer: &moq_net::broadcast::Consumer,
			count: usize,
		) -> std::collections::BTreeMap<String, hang::catalog::VideoConfig> {
			let mut catalog = moq_mux::catalog::Consumer::<()>::new(consumer, moq_mux::catalog::CatalogFormat::Hang)
				.await
				.unwrap();
			loop {
				let snapshot = catalog.next().await.unwrap().expect("a catalog");
				if snapshot.video.renditions.len() == count {
					return snapshot.video.renditions.into_iter().collect();
				}
			}
		}

		/// Subscribe to `name` with room to replay what it holds.
		async fn watch(consumer: &moq_net::broadcast::Consumer, name: &str) -> moq_net::track::Subscriber {
			consumer
				.track(name)
				.unwrap()
				.subscribe(
					moq_net::track::Subscription::default()
						.with_delay(moq_net::track::Delay::default().with_max(Duration::from_secs(60))),
				)
				.await
				.unwrap()
		}

		/// A scaled rendition publishes beside the captured one at its fraction of the pixels, and
		/// encodes only while watched: the captured one keeps its past, the scaled one holds only
		/// what was written while someone watched it.
		#[tokio::test(start_paused = true)]
		async fn a_scaled_rendition_encodes_only_while_watched() {
			local(async {
				let mut broadcast = moq_net::broadcast::Info::new().produce();
				let consumer = broadcast.consume();
				let catalog =
					moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
				let mut options = Capture {
					kind: encoder::Kind::Software,
					renditions: vec![Rendition::new("sd", 0.25).unwrap()],
					..Capture::default()
				};
				// Half a frame per second rounds the default two-second GOP down to one frame, so
				// every frame opens a group starting at its pts.
				options.capture.framerate = Some(crate::FrameRate::new(1, 2).unwrap());
				let (camera, stream) = camera();
				let run = tokio::task::spawn_local(publish_from(
					broadcast.clone(),
					catalog,
					options,
					Streams::new([stream]),
				));

				let renditions = renditions(&consumer, 2).await;
				let sd = &renditions["video/sd"];
				assert_eq!((sd.coded_width, sd.coded_height), (Some(160), Some(120)));
				let (captured, full) = renditions.iter().find(|(name, _)| *name != "video/sd").unwrap();
				assert_eq!((full.coded_width, full.coded_height), (Some(320), Some(240)));
				assert!(sd.bitrate < full.bitrate, "{:?} vs {:?}", sd.bitrate, full.bitrate);

				// Unwatched, only the captured rendition encodes.
				let mut track = consumer.track(captured).unwrap();
				camera.push_native(surface(), us(0));
				let first = track.largest().await.expect("the track is live").group;
				camera.push_native(surface(), us(2_000_000));
				while track.largest().await.expect("the track is live").group < first + 2_000_000 {}

				let mut viewer = watch(&consumer, "video/sd").await;
				camera.push_native(surface(), us(4_000_000));
				let watched = viewer.recv_group().await.unwrap().expect("a group").start;
				assert_eq!(watched, first + 4_000_000, "the scaled rendition encoded unwatched");
				drop(viewer);

				// Its viewer gone, it stops again while the captured one goes on.
				camera.push_native(surface(), us(6_000_000));
				while track.largest().await.expect("the track is live").group < first + 6_000_000 {}
				let mut viewer = watch(&consumer, "video/sd").await;
				camera.push_native(surface(), us(8_000_000));
				let mut starts = Vec::new();
				while starts.last() != Some(&(first + 8_000_000)) {
					starts.push(viewer.recv_group().await.unwrap().expect("a group").start);
				}
				// The break marked when its viewer left sits just past the frame before it, and nothing
				// between: the frame at six seconds went only to the captured rendition.
				assert_eq!(starts.first(), Some(&(first + 4_000_000)), "{starts:?}");
				assert!(
					starts
						.iter()
						.all(|start| !(first + 4_000_002..first + 8_000_000).contains(start)),
					"the scaled rendition encoded unwatched: {starts:?}"
				);

				assert!(!run.is_finished(), "the capture ended early: {:?}", run.await);
				run.abort();
			})
			.await
		}

		/// A capture publishing the captured rendition and `video/sd` from a pipelined probe, which
		/// holds each frame back by one, so a frame reaches its track once the next is written.
		/// Every frame opens a group starting at its pts.
		async fn pipelined(
			on_demand: bool,
		) -> (
			moq_net::broadcast::Consumer,
			Synthetic,
			tokio::task::JoinHandle<Result<(), Error>>,
			String,
		) {
			let mut broadcast = moq_net::broadcast::Info::new().produce();
			let consumer = broadcast.consume();
			let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
			let mut options = Capture {
				kind: encoder::Kind::Named(probe::PIPELINED.into()),
				on_demand,
				renditions: vec![Rendition::new("sd", 0.25).unwrap()],
				..Capture::default()
			};
			options.capture.framerate = Some(crate::FrameRate::new(1, 2).unwrap());
			let (camera, stream) = camera();
			let run = tokio::task::spawn_local(publish_from(broadcast, catalog, options, Streams::new([stream])));
			let captured = renditions(&consumer, 2)
				.await
				.into_keys()
				.find(|name| name != "video/sd")
				.unwrap();
			(consumer, camera, run, captured)
		}

		/// Push a frame at `micros` and wait for the capture `run` to read it.
		async fn push(camera: &Synthetic, run: &tokio::task::JoinHandle<Result<(), Error>>, micros: u64) {
			camera.push_native(surface(), us(micros));
			until(|| {
				assert!(!run.is_finished(), "the capture ended");
				camera.taken()
			})
			.await;
		}

		/// Wait until `track` holds the group a frame at `start` opened.
		async fn reached(track: &mut moq_net::track::Consumer, start: u64) {
			while track.largest().await.expect("the track is live").group < start {}
		}

		/// A rendition whose encoder is slow to open, as a hardware one can be, misses frames
		/// itself rather than holding back the rendition already encoding.
		#[cfg(not(apple))]
		#[tokio::test(start_paused = true)]
		#[expect(
			clippy::await_holding_lock,
			reason = "the guards only stall the process-wide probe, and this runtime has one thread, so holding them across an await blocks nothing"
		)]
		async fn a_slow_rendition_holds_back_only_itself() {
			let _probe = probe::exclusive();
			local(async {
				let (consumer, camera, run, captured) = pipelined(false).await;
				let mut track = consumer.track(&captured).unwrap();
				push(&camera, &run, 0).await;
				camera.push_native(surface(), us(2_000_000));
				let first = track.largest().await.expect("the track is live").group;

				// The scaled rendition's viewer opens its encoder on the next frame, which stalls.
				let opening = probe::hold_opens();
				let mut viewer = watch(&consumer, "video/sd").await;
				push(&camera, &run, 4_000_000).await;
				// The captured rendition still takes the frame after it, publishing the one before.
				push(&camera, &run, 6_000_000).await;
				reached(&mut track, first + 4_000_000).await;

				// Once open, the scaled rendition encodes the frame it was opening for, then the
				// newest, which publishes it.
				drop(opening);
				let start = viewer.recv_group().await.unwrap().expect("a group").start;
				assert_eq!(start, first + 4_000_000);

				assert!(!run.is_finished(), "the capture ended early: {:?}", run.await);
				run.abort();
			})
			.await
		}

		/// A scaled rendition whose encoder fails to drain when its viewer leaves loses only the
		/// frame the encoder held: the captured rendition keeps encoding, and the next viewer
		/// opens a fresh encoder.
		#[tokio::test(start_paused = true)]
		#[expect(
			clippy::await_holding_lock,
			reason = "the guard only keeps other tests off the process-wide probe, and this runtime has one thread, so holding it across an await blocks nothing"
		)]
		async fn a_failed_drain_costs_only_its_rendition() {
			let _probe = probe::exclusive();
			local(async {
				let (consumer, camera, run, captured) = pipelined(false).await;
				let mut track = consumer.track(&captured).unwrap();
				push(&camera, &run, 0).await;
				camera.push_native(surface(), us(2_000_000));
				let first = track.largest().await.expect("the track is live").group;

				let mut viewer = watch(&consumer, "video/sd").await;
				push(&camera, &run, 4_000_000).await;
				push(&camera, &run, 6_000_000).await;
				let start = viewer.recv_group().await.unwrap().expect("a group").start;
				assert_eq!(start, first + 4_000_000);

				// Its viewer gone, the scaled rendition drains on the next frame, losing the one it holds.
				probe::lose_tail();
				drop(viewer);
				push(&camera, &run, 8_000_000).await;
				push(&camera, &run, 10_000_000).await;
				reached(&mut track, first + 8_000_000).await;
				assert!(
					!run.is_finished(),
					"the failed drain ended the capture: {:?}",
					run.await
				);

				let mut viewer = watch(&consumer, "video/sd").await;
				push(&camera, &run, 12_000_000).await;
				camera.push_native(surface(), us(14_000_000));
				let mut starts = Vec::new();
				while starts.last() != Some(&(first + 12_000_000)) {
					starts.push(viewer.recv_group().await.unwrap().expect("a group").start);
				}
				assert!(
					starts
						.iter()
						.all(|start| !(first + 6_000_000..first + 12_000_000).contains(start)),
					"the lost frame or an unwatched one was published: {starts:?}"
				);

				assert!(!run.is_finished(), "the capture ended early: {:?}", run.await);
				run.abort();
			})
			.await
		}

		/// On demand, a viewer of any rendition opens the camera, and only the renditions someone
		/// watches encode.
		#[tokio::test(start_paused = true)]
		async fn on_demand_opens_the_camera_for_any_rendition() {
			local(async {
				let mut broadcast = moq_net::broadcast::Info::new().produce();
				let consumer = broadcast.consume();
				let catalog =
					moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
				let mut options = Capture {
					kind: encoder::Kind::Software,
					on_demand: true,
					renditions: vec![Rendition::new("sd", 0.25).unwrap()],
					..Capture::default()
				};
				options.capture.framerate = Some(crate::FrameRate::new(1, 2).unwrap());
				let (probed, probe) = camera();
				let (camera, stream) = camera();
				let source = Streams::new([probe, stream]);
				let opens = source.opens.clone();
				let run = tokio::task::spawn_local(publish_from(broadcast.clone(), catalog, options, source));

				let renditions = renditions(&consumer, 2).await;
				let captured = renditions.keys().find(|name| *name != "video/sd").unwrap().clone();
				until(|| probed.released()).await;

				let mut viewer = watch(&consumer, "video/sd").await;
				camera.push_native(surface(), us(0));
				let first = viewer.recv_group().await.unwrap().expect("a group").start;
				assert_eq!(
					opens.load(Ordering::SeqCst),
					2,
					"the scaled rendition's viewer opened the camera"
				);

				// The captured rendition encoded nothing while unwatched.
				let mut full = watch(&consumer, &captured).await;
				camera.push_native(surface(), us(2_000_000));
				let joined = full.recv_group().await.unwrap().expect("a group").start;
				assert_eq!(joined, first + 2_000_000);

				drop((viewer, full));
				until(|| camera.released()).await;
				assert!(!run.is_finished(), "the capture ended early: {:?}", run.await);
				run.abort();
			})
			.await
		}

		/// The ladder funds the scaled rendition first: under a tight grant the captured one above
		/// it takes only what is left, and climbs back to its ceiling once the link recovers.
		/// Disabling it needs a probed connection, which `rate`'s and the producer's tests cover.
		#[tokio::test(start_paused = true)]
		#[expect(
			clippy::await_holding_lock,
			reason = "the guard only keeps other tests out of the process-wide probe log, and this runtime has one thread, so holding it across an await blocks nothing"
		)]
		async fn the_scaled_rendition_is_funded_first() {
			use moq_net::bandwidth::Rate;

			let _probe = probe::exclusive();
			let estimate = moq_net::bandwidth::Producer::new();
			estimate.set(Some(Rate::from_mbps(4))).unwrap();
			let bandwidth = moq_net::bandwidth::Allocator::new(estimate.consume());

			let mut broadcast = moq_net::broadcast::Info::new().produce();
			let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
			let options = Capture {
				kind: encoder::Kind::Named(probe::NAME.into()),
				bandwidth: bandwidth.clone(),
				renditions: vec![Rendition::new("sd", 0.25).unwrap()],
				..Capture::default()
			};
			let framerate = crate::FrameRate::new(30, 1).unwrap();
			let configs = options.encoders(Size::new(320, 240), framerate, None).unwrap();
			let ceilings: Vec<_> = configs.iter().map(encoder::Config::resolved_bitrate).collect();
			let mut producers: Vec<Producer> = Vec::new();
			let mut viewers = Vec::new();
			for (name, config) in ["video/sd", "video"].into_iter().zip(configs) {
				let priority = hang::catalog::PRIORITY.video - i8::try_from(producers.len()).unwrap();
				let track = broadcast.create_track(name, catalog.track_info(priority)).unwrap();
				viewers.push(track.subscribe(None));
				let mut probe = config.clone();
				probe.kind = encoder::Kind::Software;
				let rendition = probe.probe().await.unwrap();
				let lower = producers.last().map(Producer::rung);
				producers
					.push(Producer::with_track(track, &catalog, config, rendition, bandwidth.clone(), lower).unwrap());
			}
			let ceilings: Vec<_> = ceilings.iter().map(|rate| rate.as_bps()).collect();

			let (camera, stream) = camera();
			let mut source = Streams::new([stream]);
			let clock = catalog.clock();
			let state = starting();
			let mut run = std::pin::pin!(capture_loop(&mut producers, &mut source, &options, &clock, &state));
			let mut micros = 0;
			let mut accepted = Vec::new();
			let mut until_accepted = async |want: u64| {
				// A minute of frames is far past anything the grant's ramp needs.
				for _ in 0..600 {
					accepted.extend(probe::bitrates());
					if accepted.last() == Some(&want) {
						return std::mem::take(&mut accepted);
					}
					tokio::time::advance(Duration::from_millis(100)).await;
					camera.push_native(surface(), us(micros));
					micros += 100_000;
					// The encoders answer on their own threads, so wait for one to take the frame.
					let mut encodes = 0;
					while encodes == 0 {
						tokio::select! {
							res = &mut run => panic!("the capture ended: {res:?}"),
							() = tokio::task::yield_now() => {}
						}
						encodes += probe::take().iter().filter(|(event, _)| *event == "encode").count();
					}
				}
				panic!("the encoders never settled at {want}: {accepted:?}");
			};

			let tight = 300_000;
			estimate.set(Some(Rate::from_bps(tight))).unwrap();
			// Only the captured rendition retunes: the scaled one keeps its whole ceiling.
			let squeezed = until_accepted(tight - ceilings[0]).await;
			assert_eq!(squeezed, [tight - ceilings[0]]);
			estimate.set(Some(Rate::from_mbps(4))).unwrap();
			let recovered = until_accepted(ceilings[1]).await;
			assert!(recovered.iter().all(|&bps| bps > tight - ceilings[0]), "{recovered:?}");
			drop(viewers);
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
			let mut run = std::pin::pin!(capture_loop(
				std::slice::from_mut(&mut producer),
				&mut source,
				&options,
				&clock,
				&state
			));

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
			let run = capture_loop(
				std::slice::from_mut(&mut producer),
				&mut source,
				&options,
				&clock,
				&state,
			);

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
