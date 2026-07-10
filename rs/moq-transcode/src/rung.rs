//! Per-rung serving: just-in-time encoding of one output rendition.
//!
//! Nothing is encoded until someone asks, via the two demand paths moq-net
//! exposes on the output track:
//!
//! - A live subscription (`used`) starts a live session that subscribes to the
//!   source track (mirroring the aggregate subscription) and transcodes group
//!   for group until the track goes `unused` again.
//! - A fetch of a specific group (`requested_group`) fetches that same group
//!   from the source and transcodes just that group with a fresh encoder.
//!
//! Both paths are driven by one serving loop ([`Serve`]) that owns every
//! output sequence, so the two can never race to write the same group.
//!
//! Output groups mirror the source group sequence numbers 1:1, so a fetch for
//! output group N maps to source group N and a player switching renditions
//! lands on the same content.

use std::collections::HashMap;
use std::sync::Arc;

use bytes::Bytes;
use hang::catalog::VideoConfig;
use moq_mux::container::Container as _;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::Error;
use crate::catalog::Resolved;
use crate::scale::Scaler;

/// Cap on transcode pipelines a single rung builds concurrently for on-demand
/// group fetches. Each pipeline holds a decoder + encoder session, and hardware
/// encoders expose only a few simultaneous sessions, so an unbounded fetch burst
/// (a rendition-switching player requesting many past groups at once) would
/// exhaust them and fail live viewers too. Global admission across rungs and
/// nodes is the fleet's concern; this is the local backstop.
const MAX_CONCURRENT_FETCHES: usize = 4;

/// Everything a rung needs to build transcoding pipelines on demand.
#[derive(Clone)]
pub(crate) struct Rung {
	pub info: Resolved,
	/// The source media track (not yet subscribed; demand drives that).
	pub source: moq_net::track::Consumer,
	/// The source broadcast, to notice it closing while idle.
	pub broadcast: moq_net::broadcast::Consumer,
	/// The source rendition's catalog entry (codec + container).
	pub config: VideoConfig,
	/// Which encoder implementation to use.
	pub encoder: moq_video::encode::Kind,
	/// Which decoder implementation to use.
	pub decoder: moq_video::decode::Kind,
}

impl Rung {
	fn pipeline(&self) -> Result<Pipeline, Error> {
		Pipeline::new(self)
	}

	fn container(&self) -> Result<moq_mux::catalog::hang::Container, Error> {
		Ok(moq_mux::catalog::hang::Container::try_from(&self.config.container)?)
	}
}

/// Serve one requested rung track until it closes or the source ends.
pub(crate) async fn serve(rung: Rung, request: moq_net::track::Request) -> Result<(), Error> {
	// Grab the group-request handle before accepting: a Request is dynamic from
	// birth, so a fetch racing the acceptance queues instead of failing.
	let dynamic = request.dynamic();
	let info = moq_net::track::Info::default().with_timescale(hang::container::TIMESCALE);
	let producer = request.accept(info);

	Serve {
		demand: producer.demand(),
		producer,
		dynamic,
		rung,
		live: None,
		live_latest: None,
		fetches: JoinSet::new(),
		fetching: HashMap::new(),
		limit: Arc::new(Semaphore::new(MAX_CONCURRENT_FETCHES)),
		parked: Vec::new(),
	}
	.run()
	.await
}

/// The serving loop for one rung's output track.
///
/// Live transcoding and on-demand fetches write into the same track, so this
/// loop is the single owner of every output sequence: it decides, per group,
/// whether the live session or a fetch task produces it, and nothing writes a
/// sequence it wasn't granted. That leaves no two-writer race:
///
/// - A fetch request for a sequence the live session is heading toward (or one
///   already granted to a fetch task) is parked instead of served twice; it
///   resolves from the shared track cache once the group exists.
/// - When the live session reaches a sequence granted to an in-flight fetch,
///   it defers that group until the fetch reports back: on success the fetched
///   group already serves every subscriber through the cache, and on failure
///   the live session re-creates the aborted group and transcodes it itself.
struct Serve {
	rung: Rung,
	/// The only producer handle for the output track.
	producer: moq_net::track::Producer,
	/// Surfaces consumer fetches of uncached groups.
	dynamic: moq_net::track::Dynamic,
	/// Watches whether anyone consumes the output track.
	demand: moq_net::track::Demand,

	/// The live path's state; `None` while nobody subscribes.
	live: Option<Live>,
	/// The highest sequence the live path has handled, across sessions.
	live_latest: Option<u64>,

	/// In-flight fetch tasks, local (not detached) so dropping the set on
	/// teardown aborts them all: none keep a source subscription or an encoder
	/// session alive past the track.
	fetches: JoinSet<bool>,
	/// The sequence each in-flight fetch task was granted, by task id.
	fetching: HashMap<tokio::task::Id, u64>,
	/// Bounds concurrent fetch pipelines (decoder + encoder sessions).
	limit: Arc<Semaphore>,

	/// Fetch requests whose group someone else already owns (the live session
	/// ahead of them, or an in-flight fetch for the same sequence). They resolve
	/// from the cache when the group appears, or become real fetches if the
	/// owner skips or fails.
	parked: Vec<moq_net::track::GroupRequest>,
}

/// The live path's state, driven by subscriber demand on the output track.
enum Live {
	/// Waiting for the source subscription to resolve.
	Subscribing(moq_net::kio::Pending<moq_net::track::Subscribe>),
	/// Subscribed and transcoding group for group. Boxed: a [`Session`] is an
	/// order of magnitude bigger than the other variant.
	Active(Box<Session>),
}

/// One live demand session. The pipeline persists across groups so rate
/// control carries over, while every group still opens with a forced IDR.
struct Session {
	subscriber: moq_net::track::Subscriber,
	pipeline: Pipeline,
	container: moq_mux::catalog::hang::Container,
	state: SessionState,
}

enum SessionState {
	/// Waiting for the next source group.
	Idle,
	/// The next source group's sequence is granted to an in-flight fetch;
	/// waiting for its verdict before skipping (success) or transcoding it
	/// (failure).
	Deferred(moq_net::group::Consumer),
	/// Transcoding a source group into an output group.
	Transcoding {
		source: moq_net::group::Consumer,
		output: moq_net::group::Producer,
		/// True until the first frame is processed (drives the forced IDR).
		first: bool,
	},
}

/// One actionable input to the serving loop.
// One short-lived value per loop turn; boxing the big variants buys nothing.
#[allow(clippy::large_enum_variant)]
enum Event {
	/// A consumer appeared while idle: start a live session.
	Demand,
	/// The last consumer left: tear the live session down.
	Unused,
	/// The source subscription resolved (or failed).
	Subscribed(Result<moq_net::track::Subscriber, moq_net::Error>),
	/// The live session's next source group (`None`: the source track ended).
	Group(Result<Option<moq_net::group::Consumer>, moq_net::Error>),
	/// Frames from the live session's current group (`None`: the group ended).
	Frames(Result<Option<Vec<moq_mux::container::Frame>>, Error>),
	/// A consumer requested a group that isn't cached.
	Request(moq_net::track::GroupRequest),
	/// A fetch task reported back for its granted sequence.
	Fetched { sequence: u64, success: bool },
	/// The source broadcast went away while idle.
	SourceClosed(moq_net::Error),
	/// The output track closed; nothing more to serve.
	Closed,
}

impl Serve {
	async fn run(mut self) -> Result<(), Error> {
		let result = self.events().await;
		// Never leave a live group dangling: downstream must see an incomplete
		// group as aborted, not silently short.
		self.abort_live_group();
		if result.is_err() {
			// End the track so subscribers see an error rather than a stall.
			let _ = self.producer.abort(moq_net::Error::Cancel);
		}
		result
	}

	/// Dispatch events until the track closes, the source ends, or an error.
	async fn events(&mut self) -> Result<(), Error> {
		loop {
			match self.next_event().await {
				Event::Demand => {
					// Mirror the downstream demand upstream (priority, ordering, start).
					let subscription = self.producer.subscription().unwrap_or_default();
					self.live = Some(Live::Subscribing(self.rung.source.subscribe(subscription)));
				}
				Event::Unused => self.stop_live(),
				Event::Subscribed(subscriber) => {
					self.live = Some(Live::Active(Box::new(Session {
						subscriber: subscriber?,
						pipeline: self.rung.pipeline()?,
						container: self.rung.container()?,
						state: SessionState::Idle,
					})));
				}
				Event::Group(group) => match group? {
					Some(source) => self.on_group(source)?,
					None => {
						// The source track ended: the derivative ends with it.
						self.producer.finish()?;
						return Ok(());
					}
				},
				Event::Frames(frames) => self.on_frames(frames)?,
				Event::Request(request) => self.on_request(request),
				Event::Fetched { sequence, success } => self.on_fetched(sequence, success)?,
				Event::SourceClosed(err) => {
					// The source went away while idle; end the rung with it.
					self.producer.abort(err)?;
					return Ok(());
				}
				Event::Closed => return Ok(()),
			}
		}
	}

	/// Wait for the next event across every input: output-track demand, the
	/// live session's next step, fetch requests, and fetch completions.
	async fn next_event(&mut self) -> Event {
		let Self {
			rung,
			demand,
			dynamic,
			live,
			fetches,
			fetching,
			..
		} = self;

		// The live path waits on exactly one thing, depending on its state.
		let live_event = async {
			let Some(live) = live else {
				// Nobody is subscribed: wait for demand, or the source dying.
				return tokio::select! {
					used = demand.used() => match used {
						Ok(()) => Event::Demand,
						Err(_) => Event::Closed,
					},
					err = rung.broadcast.closed() => Event::SourceClosed(err),
				};
			};

			let step = async {
				match live {
					Live::Subscribing(pending) => Event::Subscribed(pending.await),
					Live::Active(session) => match &mut session.state {
						SessionState::Idle => Event::Group(session.subscriber.next_group().await),
						// Waiting on a fetch verdict; it arrives as `Fetched` below.
						SessionState::Deferred(_) => std::future::pending::<Event>().await,
						SessionState::Transcoding { source, .. } => {
							Event::Frames(session.container.read(source).await.map_err(Error::from))
						}
					},
				}
			};
			tokio::select! {
				unused = demand.unused() => match unused {
					Ok(()) => Event::Unused,
					Err(_) => Event::Closed,
				},
				event = step => event,
			}
		};

		tokio::select! {
			event = live_event => event,
			request = dynamic.requested_group() => match request {
				Ok(request) => Event::Request(request),
				Err(_) => Event::Closed,
			},
			Some(task) = fetches.join_next_with_id(), if !fetches.is_empty() => {
				let (id, success) = match task {
					Ok((id, success)) => (id, success),
					// The task panicked; treat its group as failed.
					Err(err) => (err.id(), false),
				};
				let sequence = fetching.remove(&id).expect("fetch task not tracked");
				Event::Fetched { sequence, success }
			}
		}
	}

	/// A new source group arrived on the live session.
	fn on_group(&mut self, source: moq_net::group::Consumer) -> Result<(), Error> {
		if self.fetching_sequence(source.sequence) {
			// The sequence is granted to an in-flight fetch. Defer rather than
			// skip: if the fetch fails, the group gets transcoded here instead
			// of leaving an aborted GOP behind.
			self.session().state = SessionState::Deferred(source);
			return Ok(());
		}
		self.start_group(source)
	}

	/// Create the output group for a live source group and start transcoding.
	fn start_group(&mut self, source: moq_net::group::Consumer) -> Result<(), Error> {
		let sequence = source.sequence;
		// Mirror the source sequence so fetches and rendition switches map 1:1.
		let output = match self.producer.create_group(moq_net::group::Info { sequence }) {
			Ok(output) => Some(output),
			// A completed fetch already produced this group (an aborted one
			// would have been replaced in place): every subscriber reads it
			// from the shared cache, so don't transcode it a second time.
			Err(moq_net::Error::Duplicate) => None,
			Err(err) => return Err(err.into()),
		};

		self.live_latest = Some(self.live_latest.unwrap_or(0).max(sequence));
		self.session().state = match output {
			Some(output) => SessionState::Transcoding {
				source,
				output,
				first: true,
			},
			None => SessionState::Idle,
		};
		self.unpark(sequence);
		Ok(())
	}

	/// Frames arrived on the live session's current group (or it ended/failed).
	fn on_frames(&mut self, frames: Result<Option<Vec<moq_mux::container::Frame>>, Error>) -> Result<(), Error> {
		let session = self.session();
		let SessionState::Transcoding {
			source,
			mut output,
			mut first,
		} = std::mem::replace(&mut session.state, SessionState::Idle)
		else {
			unreachable!("frames event without a live group");
		};

		let fed = (|| {
			let Some(frames) = frames? else {
				// The group ended cleanly.
				return Ok(None);
			};
			for frame in &frames {
				process_frame(&mut session.pipeline, &mut output, frame, first)?;
				first = false;
			}
			Ok(Some(()))
		})();

		match fed {
			Ok(Some(())) => {
				// More frames to come: put the group back.
				session.state = SessionState::Transcoding { source, output, first };
				Ok(())
			}
			Ok(None) => Ok(output.finish()?),
			Err(err) => {
				let _ = output.abort(moq_net::Error::Cancel);
				Err(err)
			}
		}
	}

	/// Decide who serves a requested group: an existing owner (park until its
	/// group lands in the cache), the live session (park until it gets there),
	/// or a new fetch task.
	fn on_request(&mut self, request: moq_net::track::GroupRequest) {
		let sequence = request.sequence();
		let live_ahead = self.live.is_some() && self.live_latest.is_none_or(|latest| sequence > latest);
		if self.fetching_sequence(sequence) || live_ahead {
			self.parked.push(request);
		} else {
			self.spawn_fetch(request);
		}
	}

	/// A fetch task reported back for its granted sequence.
	fn on_fetched(&mut self, sequence: u64, success: bool) -> Result<(), Error> {
		// A deferred live group first: the verdict decides who transcodes it.
		if let Some(Live::Active(session)) = self.live.as_mut()
			&& matches!(&session.state, SessionState::Deferred(source) if source.sequence == sequence)
		{
			let SessionState::Deferred(source) = std::mem::replace(&mut session.state, SessionState::Idle) else {
				unreachable!()
			};
			if success {
				// The fetched group serves every subscriber via the cache; the
				// live session skips it and moves on.
				self.live_latest = Some(self.live_latest.unwrap_or(0).max(sequence));
				self.unpark(sequence);
			} else {
				// The fetch failed and aborted its group: take the sequence
				// back and transcode it live (`create_group` replaces an
				// aborted group in place).
				self.start_group(source)?;
			}
			return Ok(());
		}

		if success {
			// The group is in the cache; parked requests resolve from there.
			self.parked.retain(|request| request.sequence() != sequence);
		} else if let Some(at) = self.parked.iter().position(|request| request.sequence() == sequence) {
			// The fetch failed but consumers still wait; retry with one of them.
			let request = self.parked.remove(at);
			self.spawn_fetch(request);
		}
		Ok(())
	}

	/// Re-route parked requests once the live session has handled `reached`:
	/// a request at it resolves from the cache (dropping the request is fine,
	/// the cache wins over its auto-rejection), older ones the live session
	/// skipped become real fetches, newer or already-granted ones keep waiting.
	fn unpark(&mut self, reached: u64) {
		for request in std::mem::take(&mut self.parked) {
			let sequence = request.sequence();
			if sequence > reached || self.fetching_sequence(sequence) {
				self.parked.push(request);
			} else if sequence < reached {
				self.spawn_fetch(request);
			}
			// sequence == reached: dropped, the group is in the cache.
		}
	}

	/// Tear the live session down (the last subscriber left), aborting any
	/// group it was mid-transcode and re-routing requests that waited on it.
	fn stop_live(&mut self) {
		self.abort_live_group();
		self.live = None;

		for request in std::mem::take(&mut self.parked) {
			if self.fetching_sequence(request.sequence()) {
				self.parked.push(request);
			} else {
				// Nobody will produce this group live anymore; fetch it.
				self.spawn_fetch(request);
			}
		}
	}

	/// Abort the live session's in-progress output group, if any, so demand
	/// loss (or teardown) mid-group reads as aborted downstream, not finished
	/// short.
	fn abort_live_group(&mut self) {
		if let Some(Live::Active(session)) = self.live.as_mut()
			&& let SessionState::Transcoding { output, .. } = &mut session.state
		{
			let _ = output.abort(moq_net::Error::Cancel);
		}
	}

	/// Grant `request`'s sequence to a new fetch task; it owns the sequence
	/// until it reports back via [`Event::Fetched`].
	fn spawn_fetch(&mut self, request: moq_net::track::GroupRequest) {
		let rung = self.rung.clone();
		let limit = self.limit.clone();
		let sequence = request.sequence();
		let task = self.fetches.spawn(async move {
			// Take a slot before any real work, so a burst queues here instead
			// of building unbounded pipelines. The semaphore is never closed,
			// so acquire only fails if the whole rung is torn down first.
			let Ok(_permit) = limit.acquire_owned().await else {
				return false;
			};
			match fetch(rung, request).await {
				Ok(()) => true,
				Err(err) => {
					tracing::warn!(%err, sequence, "transcode fetch failed");
					false
				}
			}
		});
		self.fetching.insert(task.id(), sequence);
	}

	/// Whether `sequence` is granted to an in-flight fetch task.
	fn fetching_sequence(&self, sequence: u64) -> bool {
		self.fetching.values().any(|&granted| granted == sequence)
	}

	/// The active live session; only called from events that imply one exists.
	fn session(&mut self) -> &mut Session {
		match self.live.as_mut() {
			Some(Live::Active(session)) => session,
			_ => unreachable!("live event without an active session"),
		}
	}
}

/// Transcode one specifically requested group, fetching it from the source.
///
/// Every early exit rejects the request with a real error: dropping a
/// `GroupRequest` auto-rejects with [`moq_net::Error::Dropped`], which reads as
/// "the handler vanished" and hides the actual decode/encode/source failure from
/// the waiting consumer.
async fn fetch(rung: Rung, request: moq_net::track::GroupRequest) -> Result<(), Error> {
	let options = moq_net::group::Fetch::default().with_priority(request.priority());
	let mut source = match rung.source.fetch_group(request.sequence(), options).await {
		Ok(source) => source,
		Err(err) => {
			request.reject(err.clone());
			return Err(err.into());
		}
	};

	// A fresh pipeline per fetched group: groups are independently decodable,
	// so the encoder starts clean at the group's keyframe.
	let (mut pipeline, container) = match rung.pipeline().and_then(|p| rung.container().map(|c| (p, c))) {
		Ok(built) => built,
		Err(err) => {
			request.reject(moq_net::Error::Cancel);
			return Err(err);
		}
	};

	let mut output = match request.accept(None) {
		Ok(output) => output,
		// Someone else produced the group between the request queueing and the
		// grant (the live session racing the queue): it's in the cache and the
		// waiting consumer reads it from there, so there's nothing to do.
		Err(moq_net::Error::Duplicate) => return Ok(()),
		Err(err) => return Err(err.into()),
	};
	transcode_group(&mut pipeline, &container, &mut source, &mut output).await
}

/// Transcode one fetched source group into one output group, start to finish:
/// the group is finished (and the encoder drained) on success, aborted on error.
async fn transcode_group(
	pipeline: &mut Pipeline,
	container: &moq_mux::catalog::hang::Container,
	source: &mut moq_net::group::Consumer,
	output: &mut moq_net::group::Producer,
) -> Result<(), Error> {
	match transcode_group_inner(pipeline, container, source, output).await {
		Ok(()) => Ok(output.finish()?),
		Err(err) => {
			let _ = output.abort(moq_net::Error::Cancel);
			Err(err)
		}
	}
}

async fn transcode_group_inner(
	pipeline: &mut Pipeline,
	container: &moq_mux::catalog::hang::Container,
	source: &mut moq_net::group::Consumer,
	output: &mut moq_net::group::Producer,
) -> Result<(), Error> {
	let mut first = true;
	let mut last_timestamp = 0u64;

	while let Some(frames) = container.read(source).await? {
		for frame in &frames {
			last_timestamp = last_timestamp.max(process_frame(pipeline, output, frame, first)?);
			first = false;
		}
	}

	// One-shot group: drain whatever the encoder still buffers.
	write(output, pipeline.finish(last_timestamp)?)?;
	Ok(())
}

/// Transcode one media frame into the output group, returning its timestamp in
/// microseconds.
///
/// A group opens on a keyframe by construction, so the first frame is an IDR.
/// The low-level `Container::read` the transcoder uses does not reconstruct the
/// keyframe bit for legacy sources (that lives in the higher-level container
/// consumer), so `first` is the reliable signal; OR in the container's own flag
/// so CMAF mid-group keyframes still force an output IDR. This flag drives both
/// the decoder (keyframe gating + parameter-set injection) and the encoder
/// (forced IDR).
fn process_frame(
	pipeline: &mut Pipeline,
	output: &mut moq_net::group::Producer,
	frame: &moq_mux::container::Frame,
	first: bool,
) -> Result<u64, Error> {
	let timestamp: u64 = frame
		.timestamp
		.as_micros()
		.try_into()
		.map_err(|_| moq_net::TimeOverflow)?;
	let keyframe = frame.keyframe || first;
	write(output, pipeline.process(&frame.payload, timestamp, keyframe)?)?;
	Ok(timestamp)
}

/// Append encoded packets to the output group in the legacy hang framing.
fn write(output: &mut moq_net::group::Producer, packets: Vec<(u64, Bytes)>) -> Result<(), Error> {
	for (timestamp, payload) in packets {
		let frame = hang::container::Frame {
			timestamp: moq_net::Timestamp::from_micros(timestamp)?,
			payload,
		};
		frame.encode(output)?;
	}
	Ok(())
}

/// Decode -> scale -> encode for one rung.
///
/// The decoder is asked to emit frames at the rung's resolution
/// (`decode::Config::resize`). A decoder with a hardware scaler (NVDEC) does,
/// and its GPU frames feed the encoder in place: the NVDEC -> NVENC path never
/// touches the CPU. Frames that come back at any other size (software decode,
/// or a hardware decoder without a scaler) fall back to the CPU [`Scaler`].
struct Pipeline {
	decoder: moq_video::decode::Decoder,
	scaler: Scaler,
	encoder: moq_video::encode::Encoder,
	width: u32,
	height: u32,
}

impl Pipeline {
	fn new(rung: &Rung) -> Result<Self, Error> {
		let mut decode = moq_video::decode::Config::new();
		decode.kind = rung.decoder.clone();
		decode.resize = Some((rung.info.width, rung.info.height));
		let decoder = moq_video::decode::Decoder::new(&rung.config, &decode)?;

		let mut config = moq_video::encode::Config::new(rung.info.width, rung.info.height, rung.info.framerate);
		config.bitrate = Some(rung.info.bitrate);
		config.kind = rung.encoder.clone();
		// Keyframes are forced at every group boundary; the GOP is only a
		// backstop against pathologically long source groups.
		config.gop = rung.info.framerate.saturating_mul(8).max(1);
		let encoder = moq_video::encode::Encoder::new(&config)?;

		Ok(Self {
			decoder,
			scaler: Scaler::new(rung.info.width, rung.info.height),
			encoder,
			width: rung.info.width,
			height: rung.info.height,
		})
	}

	/// Transcode one container payload into zero or more encoded packets, each
	/// paired with its presentation timestamp in microseconds.
	fn process(&mut self, payload: &Bytes, timestamp: u64, keyframe: bool) -> Result<Vec<(u64, Bytes)>, Error> {
		let mut packets = Vec::new();
		for raw in self.decoder.decode(payload, timestamp, keyframe)? {
			let raw_timestamp = raw.timestamp_us;
			let encoded = if (raw.width, raw.height) == (self.width, self.height) {
				// Already at the rung size (the decoder scaled): feed the frame
				// through as-is, keeping a GPU frame on the GPU.
				self.encoder.encode(&raw, keyframe)?
			} else {
				let (width, height) = (raw.width, raw.height);
				let scaled = self.scaler.scale(&raw.into_i420()?, width, height)?;
				self.encoder.encode_i420(scaled, self.width, self.height, keyframe)?
			};
			for packet in encoded {
				packets.push((raw_timestamp, packet));
			}
		}
		Ok(packets)
	}

	/// Drain the encoder, pairing any buffered packets with `timestamp`.
	fn finish(&mut self, timestamp: u64) -> Result<Vec<(u64, Bytes)>, Error> {
		Ok(self
			.encoder
			.finish()?
			.into_iter()
			.map(|packet| (timestamp, packet))
			.collect())
	}
}
