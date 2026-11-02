//! Per-rung serving: just-in-time encoding of one output rendition.
//!
//! Nothing is encoded until someone asks, via the two demand paths moq-net
//! exposes on the output track:
//!
//! - Any demand (`used`), including a fetch, starts a live loop that subscribes
//!   to the source track (mirroring the aggregate subscription) and transcodes
//!   group for group until the track goes `unused` again. The group open at
//!   that point is finished first, so a later reader of that sequence gets the
//!   whole group rather than an aborted one.
//! - A fetch of a specific group (`requested_fill`) fetches that same group
//!   from the source and transcodes just that group with a fresh encoder. A
//!   fetch that starts mid-group is refused: a fresh encode's frames are only
//!   valid after the head that same encode produced. A fetch every caller
//!   leaves before it is accepted is dropped, so a later one starts clean.
//!
//! Output groups mirror the source group sequence numbers 1:1, so a fetch for
//! output group N maps to source group N and a player switching renditions
//! lands on the same content.

use std::future::Future;
use std::sync::Arc;

use bytes::Bytes;
use hang::catalog::VideoConfig;
use moq_mux::container::Container as _;
use tokio::sync::Semaphore;

use crate::Error;
use crate::catalog::Resolved;
use crate::feed::{Feed, Item};

/// Cap on transcode pipelines a single rung builds concurrently for on-demand
/// group fetches. Each pipeline holds a decoder + encoder session, and hardware
/// encoders expose only a few simultaneous sessions, so an unbounded fetch burst
/// (a rendition-switching player requesting many past groups at once) would
/// exhaust them and fail live viewers too. Global admission across rungs and
/// nodes is the fleet's concern; this is the local backstop.
const MAX_CONCURRENT_FETCHES: usize = 4;

/// A rung's retirement signal: the receiving half of a flag the transcoder sets
/// when the ladder no longer has room for this rung.
///
/// Retiring aborts the track at the next group boundary rather than dropping the
/// task mid-group, so a subscriber reads out the group it is in. It aborts rather
/// than finishes because only a source finish ends a rung for good (see [`serve`]).
/// The abort waits on the fetches still in flight, so a group a consumer asked for
/// before retirement still reaches it.
#[derive(Clone)]
pub(crate) struct Retire(tokio::sync::watch::Receiver<bool>);

impl Retire {
	/// The two halves of one rung's signal.
	pub(crate) fn channel() -> (tokio::sync::watch::Sender<bool>, Self) {
		let (sender, receiver) = tokio::sync::watch::channel(false);
		(sender, Self(receiver))
	}

	/// Resolve once this rung is retired, immediately if it already was.
	async fn fired(&mut self) {
		// An error means the transcoder dropped the sending half, which only
		// happens on its way out: retire rather than park here forever.
		let _ = self.0.wait_for(|retired| *retired).await;
	}
}

/// Everything a rung needs to build transcoding pipelines on demand.
#[derive(Clone)]
pub(crate) struct Rung {
	pub info: Resolved,
	/// The source media track's name, looked up per group fetch rather than held:
	/// a held track handle is interest in the source all the way to its publisher,
	/// and an idle rung has none.
	pub source: String,
	/// The shared live decode of the source, for the live path.
	pub feed: Feed,
	/// The source broadcast, to fetch from and to notice it closing while idle.
	pub broadcast: moq_net::broadcast::Consumer,
	/// The source rendition's catalog entry (codec + container).
	pub config: VideoConfig,
	/// Which encoder implementation to use.
	pub encoder: moq_video::encode::Kind,
	/// Which decoder implementation to use.
	pub decoder: moq_video::decode::Kind,
	/// How to resize decoded frames.
	pub resize: moq_video::resize::Config,
	/// Where to report that this rung is encoding.
	pub active: crate::active::Producer,
	/// Fires when the ladder resizes past this rung.
	pub retire: Retire,
}

impl Rung {
	async fn pipeline(&self) -> Result<Pipeline, Error> {
		Pipeline::new(self).await
	}

	fn container(&self) -> Result<moq_mux::catalog::hang::Container, Error> {
		Ok(moq_mux::catalog::hang::Container::try_from(&self.config)?)
	}

	/// An encoder producing this rung's rendition.
	///
	/// `color` is the space the frames reaching it are in, as the decoder
	/// labelled them. A rung below 576 lines fed by an HD source carries the
	/// source's space, not the one its own size implies, so leaving this to the
	/// encoder's size guess would label the rung wrongly.
	///
	/// A [`Sink`](moq_video::encode::Sink) rather than a bare `Encoder`: both
	/// callers hold it across `.await`, so on a multi-thread runtime the future
	/// can migrate workers between opening the codec and dropping it. The Windows
	/// backend's COM apartment is per-thread, so that would leak the opening
	/// worker's initialization and uninitialize COM on one that never initialized
	/// it. The sink owns a thread and stays on it.
	async fn encode(&self, color: Option<moq_video::Color>) -> Result<moq_video::encode::Sink, Error> {
		let framerate = self.info.framerate.unwrap_or(moq_video::FrameRate::new(30, 1).unwrap());
		let mut config = moq_video::encode::Config::new(self.info.size.width, self.info.size.height, framerate);
		config.bitrate = Some(self.info.bitrate);
		config.kind = self.encoder.clone();
		config.color = color;
		// Every source group boundary is a cut; the keyframe interval is only a
		// backstop against pathologically long source groups.
		config.gop = moq_video::encode::Gop::keyframe_every(std::time::Duration::from_secs(8), framerate);
		Ok(moq_video::encode::Sink::open(&config).await?)
	}
}

/// Keeps a cancelled serving future from dropping an unfinished output track.
struct Serving(Option<moq_net::track::Producer>);

impl Drop for Serving {
	fn drop(&mut self) {
		if let Some(producer) = self.0.take() {
			let _ = producer.abort(moq_net::Error::Cancel);
		}
	}
}

/// An incomplete group is cancelled even when its async writer is abandoned.
struct Writing(Option<moq_net::group::Producer>);

impl Writing {
	fn finish(mut self) -> moq_net::Result<()> {
		self.0.take().expect("owned group").finish()
	}

	fn abort(mut self, err: moq_net::Error) -> moq_net::Result<()> {
		self.0.take().expect("owned group").abort(err)
	}
}

impl std::ops::Deref for Writing {
	type Target = moq_net::group::Producer;
	fn deref(&self) -> &Self::Target {
		self.0.as_ref().expect("owned group")
	}
}

impl std::ops::DerefMut for Writing {
	fn deref_mut(&mut self) -> &mut Self::Target {
		self.0.as_mut().expect("owned group")
	}
}

impl Drop for Writing {
	fn drop(&mut self) {
		if let Some(producer) = self.0.take() {
			let _ = producer.abort(moq_net::Error::Cancel);
		}
	}
}

/// How the live path ended, which decides how the output track ends.
enum Ended {
	/// The source track finished: finish this one too, once nothing else can add a group.
	Finished,
	/// Anything else (retired, closed under us, or the source gone without finishing): abort.
	Unfinished,
}

/// Serve one requested rung track until it closes or the source ends.
pub(crate) fn serve(rung: Rung, request: moq_net::track::Request) -> impl Future<Output = Result<(), Error>> {
	// Grab the group-request handle before accepting: a Request is dynamic from
	// birth, so a fetch racing the acceptance queues instead of failing.
	let dynamic = request.requests();
	let info = hang::container::track_info(hang::catalog::PRIORITY.video);
	let mut producer = request.accept(info);
	let serving = Serving(Some(producer.clone()));
	async move {
		let mut serving = serving;
		let (finished, mut finishing) = tokio::sync::watch::channel(false);

		let live = async {
			let result = live(&rung, &mut producer).await;
			let _ = finished.send(true);
			result
		};
		let (live, fetches) = tokio::join!(live, fetches(&rung, dynamic, &mut finishing));

		// Only a source that finished finishes the track; every other ending aborts it.
		// A rung retired, closed, or cut off from its source can have its name served
		// again, and that serve starts over, so a finish here would declare an end below
		// groups a relay still caches for the name. An abort declares no end.
		//
		// Finished here rather than in `live`, because the boundary is only known once
		// no fetch can still claim a group. `finish` sets the exclusive boundary one
		// past the highest group produced so far (0 when none was), and a group at or
		// above it is refused. A fetch still opening its decoder has produced nothing
		// to count, so finishing then would reject the very group it was fetching.
		match (live, fetches) {
			(Ok(Ended::Finished), Ok(())) => {
				producer.finish()?;
				serving.0 = None;
				Ok(())
			}
			// `serving` aborts the track on drop, so subscribers see an end rather than a stall.
			(live, fetches) => live.map(|_| ()).and(fetches),
		}
	}
}

/// Wait until some subscriber wants the track, or none does, returning at once if
/// that already holds. A fresh clone reports the current value first, so a
/// change seen through [`moq_net::track::Demand::wanted`] instead is never missed.
async fn until_wanted(demand: &moq_net::track::Demand, wanted: bool) -> moq_net::Result<()> {
	let mut demand = demand.clone();
	while demand.wanted_changed().await?.is_some() != wanted {}
	Ok(())
}

/// The live path: wait for demand, attach to the shared decode [`Feed`], and
/// resize + encode its frames group for group until demand goes away. The
/// heavy lifting (subscription, decode) is shared with every other active rung
/// of this source; only the per-rung resize and encode happen here.
///
/// Reports how it ended rather than ending the track: [`serve`] owns that, once
/// the fetches in flight can no longer add a group.
async fn live(rung: &Rung, producer: &mut moq_net::track::Producer) -> Result<Ended, Error> {
	let demand = producer.demand();
	let mut retire = rung.retire.clone();
	// Set once the ladder retires this rung. The track ends at the next group
	// boundary rather than mid-group, so the last thing a subscriber gets is a
	// complete group.
	let mut retiring = false;

	loop {
		if retiring {
			return Ok(Ended::Unfinished);
		}

		tokio::select! {
			wanted = until_wanted(&demand, true) => if wanted.is_err() {
				// The output track closed; nothing more to serve.
				return Ok(Ended::Unfinished);
			},
			() = rung.broadcast.closed() => {
				// A broadcast end carries no error; stop the idle rung explicitly.
				producer.clone().abort(moq_net::Error::Cancel)?;
				return Ok(Ended::Unfinished);
			}
			() = retire.fired() => {
				// Retired while idle, which is the common case: the ladder is
				// resized long before anyone asks for the rung it dropped.
				retiring = true;
				continue;
			}
		}

		// Attach the meter here rather than at the first frame: it counts nothing
		// until this pipeline encodes one, so a subscriber waiting on a stalled
		// source is not billed for a session that produced nothing.
		let active = rung.active.attach(&rung.info);

		// One listener + encoder per demand session: rate control persists
		// across groups, while every group still opens with a forced IDR.
		// Dropping them once demand is gone releases the shared decode (if last)
		// and the encoder session until someone asks again.
		let mut listener = rung.feed.listen();
		let mut encoder = Encoder::default();

		// The output group currently being written, if the feed is mid-group.
		let mut current: Option<Writing> = None;
		// Set while demand is gone but a group is still open. A group name always
		// means the same content, so the group is finished for whoever reads that
		// sequence later instead of aborted, and the session ends at its boundary.
		let mut leaving = false;

		'session: loop {
			let item = tokio::select! {
				item = listener.recv() => item,
				unused = until_wanted(&demand, false), if !leaving => {
					if unused.is_ok() && current.is_some() {
						leaving = true;
						continue;
					}
					// Between groups, or the track closed: nothing to finish.
					if let Some(output) = current.take() {
						output.abort(moq_net::Error::Cancel)?;
					}
					break 'session;
				}
				used = until_wanted(&demand, true), if leaving => {
					if used.is_ok() {
						// Back before the group ended: carry on as if never gone.
						leaving = false;
						continue;
					}
					// The track closed under the group.
					if let Some(output) = current.take() {
						output.abort(moq_net::Error::Cancel)?;
					}
					break 'session;
				}
				() = retire.fired(), if !retiring => {
					retiring = true;
					// Mid-group: ride it out and end the track when it closes.
					// Nothing new is opened, so this costs at most one group.
					if current.is_none() {
						break 'session;
					}
					continue;
				}
			};

			// Demand can change without the `unused` or `used` branch seeing it,
			// when the select picked this item instead. A returning viewer keeps
			// the session rather than missing the next group while a new one
			// attaches, and a departed one never opens another group.
			leaving = demand.wanted().is_none();

			match item {
				Some(Item::Group(sequence)) => {
					if retiring || leaving {
						// The group we were riding out never ended, so it is
						// incomplete however long we wait.
						if let Some(output) = current.take() {
							output.abort(moq_net::Error::Cancel)?;
						}
						break 'session;
					}
					// Empty the codec before opening the next group even though this one
					// is being abandoned: a pipelined encoder still holding the previous
					// group's tail would otherwise emit it into the new group, ahead of
					// the keyframe requested just below.
					encoder.flush().await?;
					if let Some(output) = current.take() {
						// A group boundary without an end: treat as incomplete.
						output.abort(moq_net::Error::Cancel)?;
					}
					// A subscriber has to be able to start at this group, so its first
					// frame must be an IDR. The request waits for the next frame, so a
					// rung that skips this group simply carries it forward. A backend
					// that cannot cut fails the rung here: its groups could never
					// mirror the source's, which is what this rung promises.
					encoder.cut().await?;
					// Demand can also leave while the codec works above. Nothing is
					// open yet, so there is nothing to finish.
					if demand.wanted().is_none() {
						break 'session;
					}
					// Mirror the source sequence so fetches and rendition
					// switches map 1:1.
					let info = moq_net::group::Info { sequence };
					current = match producer.create_group(info) {
						Ok(output) => Some(Writing(Some(output))),
						// A fetch task is already serving this sequence (a consumer
						// fetched a group at the live edge before the live loop
						// reached it). The fetch is authoritative and its group
						// reaches every subscriber through the shared track cache,
						// so skip it here. Residual: if that fetch then fails and
						// aborts the group, this rung skips one GOP until the next
						// keyframe. Unifying live + fetch into one cache-backed
						// serving loop (like the relay) would remove the two-writer
						// race entirely; tracked as a follow-up.
						Err(moq_net::Error::Duplicate) => None,
						Err(err) => return Err(err.into()),
					};
				}
				Some(Item::Frame(frame)) => {
					// No open group: attached mid-group, skipped a duplicate, or
					// recovering from a lag. Wait for the next boundary, unless
					// nobody is left to serve it.
					let Some(output) = &mut current else {
						if leaving {
							break 'session;
						}
						continue;
					};

					// The feed decodes at the source's native size; size this rung's copy
					// here. Supported GPU paths feed the encoder without touching the
					// CPU. The resize carries the source's color space across, which is
					// what the encoder signals.
					let frame: Arc<moq_video::Frame> = match frame.size() == rung.info.size {
						true => frame,
						false => Arc::new(frame.resize(rung.info.size, &rung.resize)?),
					};
					write(output, &active, encoder.encode(rung, frame).await?)?;
				}
				Some(Item::End) => {
					if let Some(mut output) = current.take() {
						// The source group is complete, so this one has to be too: a
						// hardware encoder is still holding its last frames.
						write(&mut output, &active, encoder.flush().await?)?;
						output.finish()?;
					}
					// Or during the flush.
					leaving = demand.wanted().is_none();
					if retiring || leaving {
						break 'session;
					}
				}
				Some(Item::Lagged) => {
					// Fell behind the feed: abandon the group and resume at the
					// next boundary rather than stalling other rungs.
					if let Some(output) = current.take() {
						output.abort(moq_net::Error::Cancel)?;
					}
					if retiring || leaving {
						break 'session;
					}
				}
				Some(Item::Finished) => {
					// The source track ended: the derivative ends with it.
					if let Some(output) = current.take() {
						output.abort(moq_net::Error::Cancel)?;
					}
					return Ok(Ended::Finished);
				}
				None => {
					// The feed died mid-stream (source or decode error).
					if let Some(output) = current.take() {
						let _ = output.abort(moq_net::Error::Cancel);
					}
					producer.clone().abort(moq_net::Error::Cancel)?;
					return Ok(Ended::Unfinished);
				}
			}
		}
		// listener and encoder drop here, releasing the shared decode session
		// (when this was the last rung) and the encoder.
	}
}

/// The fetch path: serve fills of specific (past) groups, each one group at a time.
///
/// Fill tasks run under a local [`JoinSet`](tokio::task::JoinSet) rather than
/// detached. Retirement takes whatever the handler already had queued, closes
/// admission by dropping it, and drains those fetches before the track is
/// aborted. Other teardown aborts them, so no source subscription or encoder
/// session survives a failed track. A semaphore bounds how many run at once.
///
/// Owns the handler rather than borrowing it, since closing admission is
/// dropping it.
async fn fetches(
	rung: &Rung,
	dynamic: moq_net::track::Requests,
	finishing: &mut tokio::sync::watch::Receiver<bool>,
) -> Result<(), Error> {
	let limit = Arc::new(Semaphore::new(MAX_CONCURRENT_FETCHES));
	let mut tasks = tokio::task::JoinSet::new();
	let mut retire = rung.retire.clone();
	let mut retired = false;

	loop {
		// Reap finished fetches so the set doesn't grow without bound.
		while tasks.try_join_next().is_some() {}

		// Take a slot before popping a fill, so retirement never strands one in
		// the handler while all slots are busy.
		let permit = tokio::select! {
			biased;
			() = retire.fired() => {
				retired = true;
				break;
			},
			_ = finishing.wait_for(|finished| *finished) => break,
			permit = limit.clone().acquire_owned() => permit.expect("the semaphore stays open"),
		};

		let fill = tokio::select! {
			biased;
			() = retire.fired() => {
				retired = true;
				break;
			},
			_ = finishing.wait_for(|finished| *finished) => break,
			fill = dynamic.requested_fill() => match fill {
				Ok(fill) => fill,
				// The output track closed; nothing more to serve.
				Err(_) => break,
			},
		};

		spawn_fill(&mut tasks, rung.clone(), fill, permit);
	}

	if retired {
		// A consumer that asked for a group before retirement is already waiting on
		// it, whether or not this loop had reached it yet: the fill queues on the
		// dynamic handler the moment the fetch is made. Take what is queued now, so
		// dropping the handler below does not cancel a fetch that beat retirement.
		//
		// Taken in one pass, before waiting on anything, and then the handler is
		// dropped. A consumer holding the retired track can keep fetching, and the
		// handler admits a cache miss for as long as it is alive, so a drain that
		// waited for a slot with it still open would keep taking those in too and
		// the rung would never finish. Dropping it closes admission and releases
		// whatever arrived after the pass; the fills already popped are held
		// here, so they are unaffected.
		let mut queued = Vec::new();
		loop {
			tokio::select! {
				biased;
				fill = dynamic.requested_fill() => match fill {
					Ok(fill) => queued.push(fill),
					// The output track closed; nothing left to serve.
					Err(_) => break,
				},
				// Nothing queued: everything that beat retirement is spoken for.
				() = std::future::ready(()) => break,
			}
		}
		drop(dynamic);

		for fill in queued {
			let permit = limit.clone().acquire_owned().await.expect("the semaphore stays open");
			spawn_fill(&mut tasks, rung.clone(), fill, permit);
		}

		// Finish every group the handler accepted, or its producer stays open and
		// the fetch stalls forever. `serve` ends the track only once this returns,
		// which is what leaves every one of these groups writable.
		while let Some(result) = tasks.join_next().await {
			if let Err(err) = result {
				tracing::warn!(%err, "transcode fetch task panicked");
			}
		}
	} else {
		// A closed or failed track has no result left to preserve.
		tasks.shutdown().await;
	}

	Ok(())
}

/// Spawn one fill, holding its concurrency slot for the life of the task.
fn spawn_fill(
	tasks: &mut tokio::task::JoinSet<()>,
	rung: Rung,
	fill: moq_net::track::Fill,
	permit: tokio::sync::OwnedSemaphorePermit,
) {
	tasks.spawn(async move {
		let _permit = permit;
		let mut last = None;
		while let Some(sequence) = fill.wanted()
			&& last != Some(sequence)
		{
			last = Some(sequence);
			if let Err(err) = fetch(&rung, &fill, sequence).await {
				tracing::warn!(%err, sequence, "transcode fetch failed");
			}
		}
	});
}

/// Transcode group `sequence` of a fill, fetching it from the source.
///
/// The group is dropped only once the source says it no longer has it. Any other
/// failure while someone still wants the group refuses it for those readers alone,
/// so a later reader asks again. Once nobody wants the group, it is left unanswered
/// instead, releasing the permit for the groups the fill still owes: its last reader
/// already left it, and accepting afterwards would cache the group a fresh fill is
/// about to serve.
async fn fetch(rung: &Rung, fill: &moq_net::track::Fill, sequence: u64) -> Result<(), Error> {
	let groups = sequence..sequence + 1;
	let refuse = |err: moq_net::Error| fill.refuse_groups(groups.clone(), err);
	let failed = || refuse(moq_net::Error::Stream(moq_net::StreamError::Internal));
	// A fresh encode of this group need not match the bytes the reader's head
	// came from (the live encoder, or an earlier fetch), so its tail cannot
	// continue that head. Refuse it so the reader moves on to the next group
	// boundary instead of decoding a splice; a reader of the whole group still
	// gets it.
	let frame = fill.frame_start(sequence);
	if frame != 0 {
		tracing::debug!(sequence, frame, "refusing a fetch that starts mid-group");
		refuse(moq_net::Error::Unsupported);
		return Ok(());
	}

	let track = match rung.broadcast.track(&rung.source) {
		Ok(track) => track,
		Err(err) => {
			refuse(err.clone());
			return Err(err.into());
		}
	};
	let options = moq_net::group::Fetch::default().with_priority(fill.priority());
	let mut source = tokio::select! {
		biased;
		_ = fill.unwanted(sequence) => return abandon(sequence),
		source = track.fetch_group(sequence, options) => match source {
			Ok(source) => source,
			Err(moq_net::Error::NotFound) => {
				fill.drop_groups(groups);
				return Err(moq_net::Error::NotFound.into());
			}
			Err(err) => {
				refuse(err.clone());
				return Err(err.into());
			}
		},
	};

	// A fresh pipeline per fetched group: groups are independently decodable,
	// so the encoder starts clean at the group's keyframe. The open is not
	// raced against demand: a threaded codec keeps opening its session after
	// the future drops, so cancelling it would free this fetch's slot while
	// that session still exists. The check before accept covers a caller who
	// leaves meanwhile.
	let pipeline = match rung.pipeline().await {
		Ok(pipeline) => pipeline,
		Err(err) => {
			failed();
			return Err(err);
		}
	};

	let container = match rung.container() {
		Ok(container) => container,
		Err(err) => {
			failed();
			return Err(err);
		}
	};

	// Accepting is what publishes the group. One last look, so a caller who
	// left while the pipeline was opening does not race a fresh fill.
	if !fill.is_wanted(sequence) {
		return abandon(sequence);
	}

	let output = match fill.accept(sequence, None) {
		Ok(output) => output,
		Err(err) => return Err(err.into()),
	};
	// A fetch builds its own pipeline, so it is billable work even when the
	// live path is idle. Reference counted, so overlapping the live session
	// bills the rendition once rather than twice.
	let active = rung.active.attach(&rung.info);
	transcode_group(pipeline, &container, &mut source, Writing(Some(output)), &active).await?;
	Ok(())
}

/// Leave a group nobody is waiting on: its fill no longer owes it, so this encode must not
/// publish the group a fresh fill is about to serve.
fn abandon(sequence: u64) -> Result<(), Error> {
	tracing::debug!(sequence, "leaving a fetch nobody wants");
	Ok(())
}

/// Transcode one fetched source group to completion into one output group,
/// draining the decoder and encoder at the end. (The live path rides the shared
/// feed instead; see [`live`].)
async fn transcode_group(
	pipeline: Pipeline,
	container: &moq_mux::catalog::hang::Container,
	source: &mut moq_net::group::Consumer,
	mut output: Writing,
	active: &crate::active::Guard,
) -> Result<(), Error> {
	match transcode_group_inner(pipeline, container, source, &mut output, active).await {
		Ok(()) => {
			output.finish()?;
			Ok(())
		}
		Err(err) => {
			let _ = output.abort(moq_net::Error::Cancel);
			Err(err)
		}
	}
}

async fn transcode_group_inner(
	mut pipeline: Pipeline,
	container: &moq_mux::catalog::hang::Container,
	source: &mut moq_net::group::Consumer,
	output: &mut moq_net::group::Producer,
	active: &crate::active::Guard,
) -> Result<(), Error> {
	let mut first = true;

	while let Some(wire) = source.read_frame().await? {
		for frame in container.decode(wire)? {
			let timestamp = frame.timestamp;

			// A group opens on a keyframe by construction, so the first frame is
			// an IDR. The low-level `Container::decode` the transcoder uses does not
			// reconstruct the keyframe bit for legacy sources (that lives in the
			// higher-level container consumer), so `first` is the reliable signal;
			// OR in the container's own flag so CMAF mid-group keyframes still
			// force an output IDR. This flag drives both the decoder (keyframe
			// gating + parameter-set injection) and the encoder (forced IDR).
			let keyframe = frame.keyframe || first;
			first = false;

			write(
				output,
				active,
				pipeline.process(frame.payload, timestamp, keyframe).await?,
			)?;
		}
	}

	// One-shot group: drain both codec stages. Each packet keeps the timestamp of
	// the frame it was encoded from, so the tail stays in step.
	write(output, active, pipeline.finish().await?)?;
	Ok(())
}

/// Append encoded frames to the output group in the legacy hang framing, metering
/// what reached the track.
///
/// The frames written before a failure are banked anyway: they are on the group
/// and a consumer may already have read them, so dropping them from the meters
/// would understate the bill exactly when something went wrong.
fn write(
	output: &mut moq_net::group::Producer,
	active: &crate::active::Guard,
	encoded: Vec<moq_video::encode::Encoded>,
) -> Result<(), Error> {
	let mut frames = 0;
	let mut bytes = 0;

	let result: Result<(), Error> = (|| {
		for encoded in encoded {
			let size = encoded.payload.len() as u64;
			let frame = hang::container::Frame {
				timestamp: encoded.timestamp,
				payload: encoded.payload,
			};
			frame.write_to(output)?;
			frames += 1;
			bytes += size;
		}
		Ok(())
	})();

	active.produced(frames, bytes);
	result
}

/// Decode -> resize -> encode for one fetched group of one rung.
///
/// Unless CPU output is forced, the decoder is asked to emit frames at the
/// rung's resolution (`decode::Config::scale_hint`). A decoder with a hardware
/// scaler (NVDEC) does, and its GPU frames feed the encoder in place: the NVDEC
/// -> NVENC path never touches the CPU. Frames that come back at any other size
/// get `Frame::resize` instead.
struct Pipeline {
	decoder: moq_video::decode::Sink,
	encoder: Encoder,
	rung: Rung,
	size: moq_video::Size,
}

impl Pipeline {
	async fn new(rung: &Rung) -> Result<Self, Error> {
		let mut decode = moq_video::decode::Config::new();
		decode.kind = rung.decoder.clone();
		decode.output = rung.resize.output;
		decode.scale_hint = scale_hint(rung.info.size, rung.resize.output);
		// A `Sink` for the same reason as the encoder above: a fetch task holds this
		// pipeline across a group read, so the codec must own its thread.
		let decoder = moq_video::decode::Sink::open(&rung.config, &decode).await?;

		Ok(Self {
			decoder,
			encoder: Encoder::default(),
			rung: rung.clone(),
			size: rung.info.size,
		})
	}

	/// Transcode one container payload into zero or more encoded frames, each
	/// carrying the presentation time of the picture it came from.
	async fn process(
		&mut self,
		payload: Bytes,
		timestamp: moq_net::Timestamp,
		keyframe: bool,
	) -> Result<Vec<moq_video::encode::Encoded>, Error> {
		// This group opens on an IDR. The encoder holds the request until a picture
		// actually arrives, which matters because a decoder that buffers returns
		// nothing for the access unit that asked for one.
		if keyframe {
			self.encoder.cut().await?;
		}

		let mut encoded = Vec::new();
		for raw in self.decoder.decode(payload, timestamp, keyframe).await? {
			encoded.extend(self.encode_frame(raw).await?);
		}
		Ok(encoded)
	}

	/// Resize and encode one decoded frame.
	async fn encode_frame(&mut self, raw: moq_video::Frame) -> Result<Vec<moq_video::encode::Encoded>, Error> {
		// Already at the rung size (the decoder scaled): feed the frame through
		// as-is, keeping a GPU frame on the GPU.
		let raw = match raw.size() == self.size {
			true => raw,
			false => raw.resize(self.size, &self.rung.resize)?,
		};
		self.encoder.encode(&self.rung, raw).await
	}

	/// Drain the decoder and then the encoder, keeping every frame's timestamp.
	///
	/// Consumes the pipeline, since finishing the encoder consumes it: a one-shot
	/// group's pipeline is done once both codec stages are drained.
	async fn finish(mut self) -> Result<Vec<moq_video::encode::Encoded>, Error> {
		let mut encoded = Vec::new();
		for raw in self.decoder.flush().await? {
			encoded.extend(self.encode_frame(raw).await?);
		}

		encoded.extend(self.encoder.finish().await?);
		Ok(encoded)
	}
}

/// A rung's encoder, opened from the first frame it is handed: it writes that
/// frame's color space into the bitstream, so it cannot open before one exists.
///
/// Reopened whenever a frame arrives in another space. A source can switch at
/// a keyframe (an SDR insert in an HDR stream), and the encoder refuses a frame
/// it would mislabel, so the rung would otherwise fail on the switch.
#[derive(Default)]
struct Encoder {
	/// The open encoder and the space it signals.
	open: Option<(moq_video::encode::Sink, Option<moq_video::Color>)>,
	/// A cut asked for while no encoder was open, applied when one opens.
	pending_cut: bool,
}

impl Encoder {
	/// Start the next frame with an IDR.
	async fn cut(&mut self) -> Result<(), Error> {
		match &mut self.open {
			Some((sink, _)) => sink.cut().await?,
			None => self.pending_cut = true,
		}
		Ok(())
	}

	/// Encode one frame at the rung size, reopening first if its space changed.
	///
	/// The previous encoder's tail comes out ahead of the frame, in order.
	async fn encode(
		&mut self,
		rung: &Rung,
		frame: impl Into<Arc<moq_video::Frame>>,
	) -> Result<Vec<moq_video::encode::Encoded>, Error> {
		let frame = frame.into();
		let color = frame.surface.color();
		let mut encoded = Vec::new();
		if let Some((_, signalled)) = &self.open
			&& *signalled != color
		{
			let (sink, _) = self.open.take().expect("checked above");
			encoded = sink.finish().await?;
			// The new parameter sets only reach a decoder on an IDR.
			self.pending_cut = true;
		}
		let sink = match &mut self.open {
			Some((sink, _)) => sink,
			None => {
				let mut sink = rung.encode(color).await?;
				if std::mem::take(&mut self.pending_cut) {
					sink.cut().await?;
				}
				&mut self.open.insert((sink, color)).0
			}
		};
		encoded.extend(sink.encode(frame).await?);
		Ok(encoded)
	}

	/// Drain the frames the encoder still holds.
	async fn flush(&mut self) -> Result<Vec<moq_video::encode::Encoded>, Error> {
		match &mut self.open {
			Some((sink, _)) => Ok(sink.flush().await?),
			None => Ok(Vec::new()),
		}
	}

	/// Drain and close the encoder.
	async fn finish(self) -> Result<Vec<moq_video::encode::Encoded>, Error> {
		match self.open {
			Some((sink, _)) => Ok(sink.finish().await?),
			None => Ok(Vec::new()),
		}
	}
}

/// Let a hardware decoder scale only when the caller has not forced the CPU.
fn scale_hint(size: moq_video::Size, output: moq_video::Output) -> Option<moq_video::Size> {
	(output != moq_video::Output::Cpu).then_some(size)
}

#[cfg(test)]
mod tests {
	use super::*;
	use moq_video::Output;

	/// A 160x120 software rung of an idle source, with the source and the
	/// retirement trigger that keep it alive.
	fn idle_rung() -> (Rung, moq_net::broadcast::Producer, tokio::sync::watch::Sender<bool>) {
		let source = moq_net::broadcast::Info::default().produce();
		let broadcast = source.consume();
		let config = hang::catalog::VideoConfig::new(hang::catalog::H264 {
			inline: true,
			profile: 0x42,
			constraints: 0,
			level: 30,
		});
		let (trigger, retire) = Retire::channel();
		let rung = Rung {
			info: Resolved {
				name: "video/120p".into(),
				short_side: 120,
				size: moq_video::Size::new(160, 120),
				bitrate: moq_net::bandwidth::Rate::from_bps(100_000),
				framerate: None,
			},
			source: "video".into(),
			feed: Feed::new(
				broadcast.clone(),
				"video".into(),
				config.clone(),
				moq_video::decode::Config::new(),
			),
			broadcast,
			config,
			encoder: moq_video::encode::Kind::Software,
			decoder: moq_video::decode::Kind::Software,
			resize: Default::default(),
			active: Default::default(),
			retire,
		};
		(rung, source, trigger)
	}

	/// A group every reader narrowed past releases the fill's one-at-a-time fetch, so the
	/// groups the fill still owes are fetched rather than waiting behind it.
	#[tokio::test(start_paused = true)]
	async fn a_group_nobody_reads_releases_its_fill() {
		use std::task::Poll;
		let (rung, source, _retire) = idle_rung();
		let source_fills = source.create_track("video", None).unwrap().requests();

		let output = moq_net::broadcast::Info::default().produce();
		let timed = moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI);
		let mut track = output.create_track("video/120p", timed).unwrap();
		let fills = track.requests();
		let ascending = |groups| {
			moq_net::track::Subscription::default()
				.with_max_delay(None)
				.with_range(groups)
				.with_order(moq_net::track::Order::Ascending)
		};
		let mut reader = track.subscribe(ascending(7..));
		track.start_at(10).unwrap();
		let mut live = track.create_group(moq_net::group::Info { sequence: 10 }).unwrap();
		live.write_frame(moq_net::Timestamp::from_millis(10).unwrap(), &b"x"[..])
			.unwrap();
		live.finish().unwrap();
		let waiter = kio::Waiter::noop();
		while let Poll::Ready(Ok(Some(_))) = reader.poll_recv_group(&waiter) {}
		let fill = fills.requested_fill().await.unwrap();
		assert_eq!((fill.start().group, fill.end()), (7, 10));

		let mut tasks = tokio::task::JoinSet::new();
		let permit = Arc::new(Semaphore::new(1)).try_acquire_owned().unwrap();
		spawn_fill(&mut tasks, rung, fill, permit);
		let wait = std::time::Duration::from_secs(5);
		let first = kio::clock::timeout(wait, source_fills.requested_fill())
			.await
			.expect("group 7 is fetched from the source")
			.unwrap();
		assert_eq!(first.start().group, 7);

		reader.update(ascending(8..)).unwrap();
		assert!(reader.poll_recv_group(&waiter).is_pending());
		let next = kio::clock::timeout(wait, source_fills.requested_fill())
			.await
			.expect("the fill waited behind group 7, which nobody reads")
			.unwrap();
		assert_eq!(next.start().group, 8);
	}

	/// A restarted source cancels the serving future even while a rung waits for media.
	/// Its subscribers must see that cause, rather than an unfinished producer drop.
	#[tokio::test(start_paused = true)]
	async fn cancelling_an_idle_rung_reports_cancel() {
		let (rung, _source, _retire) = idle_rung();
		let output = moq_net::broadcast::Info::default().produce();
		let mut dynamic = output.requests();
		let view = output.consume();
		let subscribing = tokio::spawn(async move { view.track("rung").unwrap().subscribe(None).await.unwrap() });
		let request = dynamic.requested_track().await.unwrap();
		let running = tokio::spawn(serve(rung, request));
		let mut track = subscribing.await.unwrap();
		running.abort();
		assert!(running.await.unwrap_err().is_cancelled());
		assert!(matches!(track.recv_group().await, Err(moq_net::Error::Cancel)));
	}

	#[tokio::test(start_paused = true)]
	async fn internal_idle_source_end_is_cancel() {
		use std::future::Future;
		let (rung, source, _retire) = idle_rung();
		let output = moq_net::broadcast::Info::default().produce();
		let mut track = output.create_track("video/120p", None).unwrap();
		let observed = track.clone();
		let mut running = std::pin::pin!(live(&rung, &mut track));
		assert!(
			running
				.as_mut()
				.poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
				.is_pending()
		);
		source.close();
		assert!(matches!(running.await, Ok(Ended::Unfinished)));
		assert!(matches!(observed.closed().await, Err(moq_net::Error::Cancel)));
	}

	/// The track is admitted before the serving future is spawned, so it also owns cleanup
	/// when its caller abandons that future without polling it at all.
	#[tokio::test(start_paused = true)]
	async fn an_unpolled_rung_reports_cancel() {
		let (rung, _source, _retire) = idle_rung();
		let output = moq_net::broadcast::Info::default().produce();
		let mut dynamic = output.requests();
		let view = output.consume();
		let subscribing = tokio::spawn(async move { view.track("rung").unwrap().subscribe(None).await.unwrap() });
		let request = dynamic.requested_track().await.unwrap();
		let running = serve(rung, request);
		let mut track = subscribing.await.unwrap();
		drop(running);
		assert!(matches!(track.recv_group().await, Err(moq_net::Error::Cancel)));
	}

	/// A source switching color space mid-stream (an SDR insert in an HDR
	/// stream) reopens the rung encoder rather than failing on the mismatch, and
	/// the new space starts on an IDR so a decoder sees its parameter sets.
	#[tokio::test]
	#[cfg(feature = "openh264")]
	async fn a_color_switch_reopens_the_encoder() {
		use moq_video::Color;
		use moq_video::color::{Matrix, Primaries, Range, Transfer};

		let (rung, _source, _retire) = idle_rung();
		let size = rung.info.size;
		let frame = |color: Color, micros| {
			let i420 = moq_video::I420::new(size, vec![128; size.pixels() as usize * 3 / 2]).unwrap();
			moq_video::Frame::new(
				moq_video::Surface::I420(i420.with_color(color)),
				moq_net::Timestamp::from_micros(micros).unwrap(),
			)
		};
		let pq = Color {
			matrix: Matrix::Bt2020,
			range: Range::Limited,
			primaries: Primaries::Bt2020,
			transfer: Transfer::Pq,
		};

		let mut encoder = Encoder::default();
		encoder.cut().await.unwrap();
		let mut encoded = encoder.encode(&rung, frame(Color::BT709, 0)).await.unwrap();
		encoded.extend(encoder.encode(&rung, frame(Color::BT709, 33_333)).await.unwrap());
		let switched = encoder.encode(&rung, frame(pq, 66_666)).await.unwrap();
		encoded.extend(encoder.finish().await.unwrap());

		assert!(encoded.first().expect("the first frame").keyframe);
		assert!(switched.last().expect("the switched frame").keyframe);
	}

	/// A fetched NVDEC group reaches `Frame::resize` at native size when
	/// CPU output is forced, rather than being resized in the decoder first.
	#[test]
	fn forced_cpu_skips_the_decoder_scaler() {
		let size = moq_video::Size::new(160, 120);

		assert_eq!(scale_hint(size, Output::Cpu), None);
		assert_eq!(scale_hint(size, Output::Native), Some(size));
	}

	/// A frame that reached the group is billable even when a later frame in the
	/// same batch fails: it is on the track and a consumer may already have read
	/// it, so the meters must not lose it with the error.
	#[test]
	fn write_banks_the_frames_that_reached_the_group() {
		let rung = Resolved {
			name: "video/120p".to_string(),
			short_side: 120,
			size: moq_video::Size::new(160, 120),
			bitrate: moq_net::bandwidth::Rate::from_bps(100_000),
			framerate: Some(moq_video::FrameRate::new(30, 1).unwrap()),
		};

		let active = crate::active::Producer::default();
		active.declare(std::slice::from_ref(&rung));
		let mut cursor = active.consume();
		let rendition = cursor.try_next().expect("ladder").rendition;

		let broadcast = moq_net::broadcast::Info::default().produce();
		let track = broadcast
			.create_track("video/120p", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let mut group = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		let guard = active.attach(&rung);

		// The second frame cannot be expressed in the container's timescale, so it
		// fails to write while the first is already on the group.
		let good = moq_video::encode::Encoded::new(
			Bytes::from_static(b"hello"),
			moq_net::Timestamp::from_micros(0).unwrap(),
			true,
		);
		let bad = moq_video::encode::Encoded::new(
			Bytes::from_static(b"world"),
			moq_net::Timestamp::from_secs(1 << 60).unwrap(),
			false,
		);

		assert!(write(&mut group, &guard, vec![good, bad]).is_err());
		assert_eq!(rendition.frames(), 1);
		assert_eq!(rendition.bytes(), 5);
		// One frame is still a start, so the rendition reads as encoding.
		assert!(cursor.try_next().expect("edge").encoding);
	}
}
