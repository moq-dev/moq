//! Per-rung serving: just-in-time encoding of one output rendition.
//!
//! Nothing is encoded until someone asks, via the two demand paths moq-net
//! exposes on the output track:
//!
//! - Any demand (`used`), including a fetch, starts a live loop that subscribes
//!   to the source track (mirroring the aggregate subscription) and transcodes
//!   group for group until the track goes `unused` again. The group open at
//!   that point is finished first, so a later reader of that sequence gets the
//!   whole group rather than an aborted one. Under a session budget only a
//!   subscriber reaching the live edge counts, so a fetch never holds a session
//!   its own encode then waits behind.
//! - A fetch of a specific group (`requested_fill`) fetches that same group
//!   from the source and transcodes just that group with a fresh encoder. A
//!   reader's time start fetches the source group containing that time, which
//!   may start below it. A fetch that starts mid-group (at a frame past the
//!   first) is refused: a fresh encode's frames are only
//!   valid after the head that same encode produced. A fetch every caller
//!   leaves before it is accepted is dropped, so a later one starts clean.
//!
//! Output groups mirror the source groups 1:1: the output track takes the source
//! track's timescale, so each output group has its source group's key. A fetch for
//! an output group fetches that key from the source, and a player switching
//! renditions, the source's included, lands on the same content.

use std::collections::VecDeque;
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
///
/// The same one-shot flag also carries the source catalog's end and a rung's fetches draining.
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

	/// Whether it has fired.
	fn is_fired(&self) -> bool {
		*self.0.borrow()
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
	/// The source track's timescale, which the output track takes so each output group has its
	/// source group's key.
	pub scale: moq_net::Timescale,
	/// The codec implementations, devices, and resize the transcode was configured with.
	pub transcode: crate::Config,
	/// Where to report that this rung is encoding.
	pub active: crate::active::Producer,
	/// The handle this rung's output is counted on, the one a caller bills.
	pub rendition: crate::active::Rendition,
	/// Fires when the ladder resizes past this rung.
	pub retire: Retire,
	/// Fires once the source catalog has finished, shared by every rung of the ladder. An idle
	/// rung then ends rather than waiting on demand, and its fetches drain as on retirement,
	/// while a serving one carries on until its source track ends.
	pub ending: Retire,
}

impl Rung {
	async fn pipeline(&self, session: Option<tokio::sync::OwnedSemaphorePermit>) -> Result<Pipeline, Error> {
		Pipeline::new(self, session).await
	}

	fn container(&self) -> Result<moq_mux::catalog::hang::Container, Error> {
		Ok(moq_mux::catalog::hang::Container::try_from(&self.config)?)
	}

	/// An encoder producing this rung's rendition, scaling what it is fed to the
	/// rung size.
	///
	/// The scale runs on the sink's own thread rather than the caller's: frames
	/// arrive at the source's native size (one decode feeds every rung), and a
	/// CPU resize per rung per frame on the rung task would pin runtime workers.
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
	///
	/// Opens holding `session`, handed back with the result; see [`crate::config::opening`].
	async fn encode(
		&self,
		color: moq_video::Color,
		session: Option<tokio::sync::OwnedSemaphorePermit>,
	) -> (
		Result<moq_video::encode::Sink, Error>,
		Option<tokio::sync::OwnedSemaphorePermit>,
	) {
		let framerate = self.info.framerate.unwrap_or(moq_video::FrameRate::new(30, 1).unwrap());
		let mut config = self.transcode.encoder(self.info.size, framerate);
		config.bitrate = Some(self.info.bitrate);
		config.color = Some(color);
		config.resize = Some(self.transcode.resize.clone());
		// Every source group boundary is a cut; the join is only a backstop
		// against pathologically long source groups.
		config.join = std::time::Duration::from_secs(8);
		let (sink, session) =
			crate::config::opening(session, async move { moq_video::encode::Sink::open(&config).await }).await;
		(sink.map_err(Error::from), session)
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
		self.0.take().expect("owned group").finish().map(drop)
	}

	fn finish_at(mut self, end: u64) -> moq_net::Result<()> {
		self.0.take().expect("owned group").finish_at(end).map(drop)
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
	// In the source's timescale, so each output group has its source group's key.
	let info = hang::container::track_info(hang::catalog::PRIORITY.video).with_timescale(rung.scale);
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
				// Released with `serving`, the last clone, which then no longer aborts it.
				drop(producer.finish()?);
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
	while demand.wanted_changed().await? != wanted {}
	Ok(())
}

/// Who keeps the live path running: any demand on the track, or under a session budget
/// only a subscriber that reaches the live edge. A fetch is demand too, and so is a
/// subscription over past groups only, which a lite-07 relay sends for a run of fills: a
/// live encode either started would hold the serving session its fills then wait behind.
struct Want {
	demand: moq_net::track::Demand,
	/// The track, watched for subscribers that reach the live edge, when a budget is set.
	subscribers: Option<moq_net::track::Producer>,
}

impl Want {
	fn new(rung: &Rung, producer: &moq_net::track::Producer) -> Self {
		Self {
			demand: producer.demand(),
			subscribers: rung.transcode.sessions.is_set().then(|| producer.clone()),
		}
	}

	/// Whether the live path is wanted now, with `edge` the newest source group it has seen.
	fn now(&self, edge: Option<u64>) -> bool {
		match &self.subscribers {
			Some(track) => track.subscription().is_some_and(|sub| reaches_live(&sub, edge)),
			None => self.demand.wanted(),
		}
	}

	/// Wait until the live path is wanted, or not, returning at once if that already holds.
	///
	/// Under a budget it wakes on subscription changes, not on the edge moving. The edge only
	/// grows, which never makes a subscription reach it, and the live loop checks [`Self::now`]
	/// on every item for one the edge has passed.
	async fn until(&self, wanted: bool, edge: Option<u64>) -> moq_net::Result<()> {
		let Some(track) = &self.subscribers else {
			return until_wanted(&self.demand, wanted).await;
		};
		// Read through the change cursor alone, so a change after any read is reported. A clone
		// copies it, and nothing polls the accepted track's, so it starts at no subscriber.
		let mut cursor = track.clone();
		let mut subscription = None;
		let noop = kio::Waiter::noop();
		while let std::task::Poll::Ready(changed) = cursor.poll_subscription_changed(&noop) {
			subscription = changed?;
		}
		while subscription.as_ref().is_some_and(|sub| reaches_live(sub, edge)) != wanted {
			subscription = cursor.subscription_changed().await?;
		}
		Ok(())
	}
}

/// Whether `subscription` reaches the live edge, the newest source group `edge`: some range is
/// open or ends at or past it. As moq-net judges a copy's feed,
/// ranges wholly below it read only history, which fills serve.
///
/// Before any group, only an open range or a live join does. A paused source's feed may never
/// deliver a group to place the edge by, so a bounded history range counted as live would hold
/// the serving session its own fills then wait behind.
fn reaches_live(subscription: &moq_net::track::Subscription, edge: Option<u64>) -> bool {
	subscription.ranges.iter().any(|range| match (range.end, edge) {
		(None, _) => true,
		// A time end is the last key it reads.
		(Some(last), Some(edge)) => last >= edge,
		// A live join starts at the edge, wherever it turns out to be.
		(Some(_), None) => matches!(range.start, moq_net::track::Start::Live { .. }),
	})
}

/// The live path: wait for demand, attach to the shared decode [`Feed`], and
/// encode its frames group for group until demand goes away. The heavy lifting
/// (subscription, decode) is shared with every other active rung of this
/// source; only the per-rung resize and encode are this rung's, and both run on
/// its encoder's thread.
///
/// Reports how it ended rather than ending the track: [`serve`] owns that, once
/// the fetches in flight can no longer add a group.
async fn live(rung: &Rung, producer: &mut moq_net::track::Producer) -> Result<Ended, Error> {
	let want = Want::new(rung, producer);
	// The newest source group this rung has seen start, which a subscription has to reach.
	let mut edge = None;
	let mut retire = rung.retire.clone();
	let mut ending = rung.ending.clone();
	// Set once the ladder retires this rung. The track ends at the next group
	// boundary rather than mid-group, so the last thing a subscriber gets is a
	// complete group.
	let mut retiring = false;

	loop {
		if retiring {
			return Ok(Ended::Unfinished);
		}

		tokio::select! {
			wanted = want.until(true, edge) => if wanted.is_err() {
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
			() = ending.fired() => return Ok(Ended::Unfinished),
		}

		// The session's encoder holds its place in the budget for as long as it serves,
		// so wait for one before attaching to the feed. Demand that leaves, a retirement or
		// a closing broadcast gives up the wait.
		let session = tokio::select! {
			session = rung.transcode.sessions.serve() => session,
			unused = want.until(false, edge) => {
				if unused.is_err() {
					return Ok(Ended::Unfinished);
				}
				continue;
			}
			() = rung.broadcast.closed() => {
				producer.clone().abort(moq_net::Error::Cancel)?;
				return Ok(Ended::Unfinished);
			}
			() = retire.fired() => {
				retiring = true;
				continue;
			}
			// Before attaching a new feed: a source ending may never end the wait for a session.
			() = ending.fired() => return Ok(Ended::Unfinished),
		};
		// The wait is unbiased, so a session freed as the catalog ends can still win it.
		if ending.is_fired() {
			return Ok(Ended::Unfinished);
		}

		// Attach the meter here rather than at the first frame: it counts nothing
		// until this pipeline encodes one, so a subscriber waiting on a stalled
		// source is not billed for a session that produced nothing.
		let active = rung.active.attach(&rung.rendition);

		// One listener + encoder per demand session: rate control persists
		// across groups, while every group still opens with a forced IDR.
		// Dropping them once demand is gone releases the shared decode (if last)
		// and the encoder session until someone asks again.
		let mut listener = rung.feed.listen();
		let mut encoder = Encoder::new(session);

		// The output group currently being written, if the feed is mid-group.
		let mut current: Option<Writing> = None;
		// Set while demand is gone but a group is still open. A group name always
		// means the same content, so the group is finished for whoever reads that
		// sequence later instead of aborted, and the session ends at its boundary.
		let mut leaving = false;

		'session: loop {
			let item = tokio::select! {
				item = listener.recv() => item,
				unused = want.until(false, edge), if !leaving => {
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
				used = want.until(true, edge), if leaving => {
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

			// A subscription over past groups only reaches no group from here on, so it
			// leaves before opening this one rather than holding the session through it.
			if let Some(Item::Group(sequence)) = &item {
				edge = edge.max(Some(*sequence));
			}
			// Demand can change without the `unused` or `used` branch seeing it,
			// when the select picked this item instead. A returning viewer keeps
			// the session rather than missing the next group while a new one
			// attaches, and a departed one never opens another group.
			leaving = !want.now(edge);

			match item {
				Some(Item::Skip(groups)) => {
					// The source never produces these starts, so neither does this rung: a
					// lite-07 reader of the track would otherwise wait on them for good.
					producer.skip_groups(groups);
					if leaving && current.is_none() {
						break 'session;
					}
				}
				Some(Item::Group(start)) => {
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
					while encoder.receive()?.is_some() {}
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
					if !want.now(edge) {
						break 'session;
					}
					// Mirror the source group so fetches and rendition
					// switches map 1:1.
					let info = moq_net::group::Info { start };
					// `None`: a fetch task is already serving this group (a
					// consumer fetched a group at the live edge before the live
					// loop reached it). The fetch is authoritative and its group
					// reaches every subscriber through the shared track cache, so
					// skip it here. Residual: if that fetch then fails and aborts
					// the group, this rung skips one GOP until the next keyframe.
					//
					// The two paths stay separate writers on purpose. This one
					// rides the shared feed (one decoder for every rung) and keeps
					// its encoder's rate control across groups; a fetch decodes one
					// past group on its own, at whatever pace the source serves it,
					// with a fresh codec pair. Folding the fetch into this loop
					// would either stall the live edge behind a past group's decode
					// or give up the shared decode, and the track cache already
					// settles who writes a group: the first `claim_group` or
					// `accept` wins, and the other gets `None`.
					current = producer.claim_group(info)?.map(|output| Writing(Some(output)));
				}
				Some(Item::Frame(frame)) => {
					// No open group: attached mid-group, skipped a duplicate, or
					// recovering from a lost group. Wait for the next boundary, unless
					// nobody is left to serve it.
					let Some(output) = &mut current else {
						if leaving {
							break 'session;
						}
						continue;
					};

					// The feed decodes at the source's native size; the encoder sizes
					// this rung's copy on its own thread. Supported GPU paths feed the
					// encoder without touching the CPU. The resize carries the source's
					// color space across, which is what the encoder signals.
					encoder.encode(rung, frame).await?;
					drain(output, &active, &mut encoder)?;
				}
				Some(Item::End(end)) => {
					if let Some(mut output) = current.take() {
						// The source group is complete, so this one has to be too: a
						// hardware encoder is still holding its last frames.
						encoder.flush().await?;
						drain(&mut output, &active, &mut encoder)?;
						match end {
							Some(end) => output.finish_at(end)?,
							None => output.finish()?,
						}
					}
					// Or during the flush.
					leaving = !want.now(edge);
					if retiring || leaving {
						break 'session;
					}
				}
				Some(Item::Lost) => {
					// The source lost this group, or this rung fell behind the feed:
					// abandon it and resume at the next boundary.
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
					// The feed died mid-stream (a source error, or a decoder that would not open).
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
/// detached. Retirement, or the live path ending after the source catalog did, takes
/// whatever the handler already had queued, closes admission by dropping it, and drains
/// those fetches before the track ends. Until then each fill keeps pace with its readers,
/// even once the catalog has ended, since the live path may still be serving. Other
/// teardown aborts them, so no source subscription or encoder session survives a failed
/// track. A semaphore bounds how many groups transcode at once; a fill waiting on its
/// readers between groups holds none.
///
/// Owns the handler rather than borrowing it, since closing admission is
/// dropping it.
async fn fetches(
	rung: &Rung,
	dynamic: moq_net::track::Requests,
	finishing: &mut tokio::sync::watch::Receiver<bool>,
) -> Result<(), Error> {
	let limit = Arc::new(Semaphore::new(MAX_CONCURRENT_FETCHES));
	// Fires as admission closes, so the fills serve what they owe without waiting on readers.
	let (drainer, draining) = Retire::channel();
	let mut tasks = tokio::task::JoinSet::new();
	let mut retire = rung.retire.clone();
	let mut retired = false;

	loop {
		// Reap finished fetches so the set doesn't grow without bound.
		while tasks.try_join_next().is_some() {}

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

		spawn_fill(&mut tasks, rung.clone(), fill, limit.clone(), draining.clone());
	}

	// A rung ending with its source catalog has its live path done, so it drains what it owes.
	if retired || rung.ending.is_fired() {
		drainer.send_replace(true);
		// A consumer that asked for a group before retirement is already waiting on
		// it, whether or not this loop had reached it yet: the fill queues on the
		// dynamic handler the moment the fetch is made. Take what is queued now, so
		// dropping the handler below does not cancel a fetch that beat retirement.
		//
		// Taken in one pass, before waiting on anything, and then the handler is
		// dropped. A consumer holding the retired track can keep fetching, and the
		// handler admits a cache miss for as long as it is alive, so a drain that
		// waited with it still open would keep taking those in too and the rung
		// would never finish. Dropping it closes admission and releases whatever
		// arrived after the pass; the fills already popped are served here, so
		// they are unaffected.
		loop {
			tokio::select! {
				biased;
				fill = dynamic.requested_fill() => match fill {
					Ok(fill) => spawn_fill(&mut tasks, rung.clone(), fill, limit.clone(), draining.clone()),
					// The output track closed; nothing left to serve.
					Err(_) => break,
				},
				// Nothing queued: everything that beat retirement is spoken for.
				() = std::future::ready(()) => break,
			}
		}
		drop(dynamic);

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

/// Spawn one fill, taking a slot of `limit` for each group it transcodes, once the source
/// group is in hand: waiting on the source holds no encoder session. It stops waiting on its
/// readers once `draining` fires.
fn spawn_fill(
	tasks: &mut tokio::task::JoinSet<()>,
	rung: Rung,
	mut fill: moq_net::track::FillGroups,
	limit: Arc<Semaphore>,
	mut draining: Retire,
) {
	tasks.spawn(async move {
		let mut last = None;
		loop {
			// A group at a time as its readers pick them up, so a wide fetch never evicts its
			// own earlier groups before they are read. The slot is released before waiting on
			// them, so a reader that stops reading never holds an encoder session.
			//
			// Once draining, the track ends only after this drain, so serve what is still owed
			// without waiting on a reader that may never read again. Each group once: one this
			// fill left unanswered is not asked for again.
			let sequence = tokio::select! {
				biased;
				() = draining.fired() => fill.wanted().filter(|sequence| last != Some(*sequence)),
				sequence = fill.wanted_changed() => sequence,
			};
			let Some(sequence) = sequence else { break };
			last = Some(sequence);
			if let Err(err) = fetch(&rung, &fill, sequence, &limit).await {
				tracing::warn!(%err, sequence, "transcode fetch failed");
			}
		}
	});
}

/// Transcode group `sequence` of a fill, fetching it from the source, or for a reader's time
/// start the source group containing that time.
///
/// The group is dropped only once the source says it no longer has it. Any other
/// failure while someone still wants the group refuses it for those readers alone,
/// so a later reader asks again. Once nobody wants the group, it is left unanswered
/// instead, releasing the slot for the groups the fill still owes: its last reader
/// already left it, and accepting afterwards would cache the group a fresh fill is
/// about to serve.
async fn fetch(rung: &Rung, fill: &moq_net::track::FillGroups, sequence: u64, limit: &Semaphore) -> Result<(), Error> {
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
	// A reader's time start asks for the group containing the time, which may start below it, as
	// far down as the fill's floor. The key alone names no group there, so the source places it.
	let time = fill.is_time_start() && sequence == fill.start().group && fill.floor() < sequence;
	let mut retire = rung.retire.clone();
	let located = tokio::select! {
		biased;
		_ = fill.unwanted(sequence) => return abandon(sequence),
		located = locate(&track, fill, sequence, time) => located,
		// Placing a time waits on the source's next group while it holds none, which a retired
		// rung's drain can't. Nothing says the source lacks it, so it is refused, not dropped.
		() = retire.fired(), if time => {
			refuse(moq_net::Error::Cancel);
			return Ok(());
		}
	};
	// The output group has its source group's key. The read placing a time stays held until
	// the group is transcoded, as the interest its group arrived on.
	let (key, mut source, _placing) = match located {
		Ok(Some(found)) => found,
		// No group starts from the floor through the time.
		Ok(None) if time => {
			fill.drop_groups(sequence..sequence + 1);
			return Ok(());
		}
		Ok(None) => return drop_gap(rung, &track, fill, sequence).await,
		Err(err) => {
			refuse(err.clone());
			return Err(err.into());
		}
	};

	let _permit = tokio::select! {
		biased;
		_ = fill.unwanted(sequence) => return abandon(sequence),
		permit = limit.acquire() => permit.expect("the semaphore stays open"),
	};

	// A fresh pipeline per fetched group: groups are independently decodable,
	// so the encoder starts clean at the group's keyframe. The open is not
	// raced against demand: a threaded codec keeps opening its session after
	// the future drops, so cancelling it would free this fetch's slot while
	// that session still exists. The check before accept covers a caller who
	// leaves meanwhile.
	// Its encoder takes a session of its own. A reader who leaves while it waits for one
	// gets the group left unanswered, encoding nothing.
	let session = tokio::select! {
		biased;
		_ = fill.unwanted(sequence) => return abandon(sequence),
		session = rung.transcode.sessions.serve() => session,
	};
	let pipeline = match rung.pipeline(session).await {
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

	// Already written, as by the live path: nothing left to serve.
	let Some(output) = fill.accept(key, None)? else {
		return Ok(());
	};
	// A fetch builds its own pipeline, so it is billable work even when the
	// live path is idle. Reference counted, so overlapping the live session
	// bills the rendition once rather than twice.
	let active = rung.active.attach(&rung.rendition);
	transcode_group(pipeline, &container, &mut source, Writing(Some(output)), &active).await?;
	Ok(())
}

/// The source group fill key `sequence` names, its start, and for a `time` start the read that
/// placed it, or `None` when the source has none: no group at the key, or for a time, none
/// containing it from the fill's floor.
async fn locate(
	track: &moq_net::track::Consumer,
	fill: &moq_net::track::FillGroups,
	sequence: u64,
	time: bool,
) -> moq_net::Result<Option<(u64, moq_net::group::Consumer, Option<moq_net::track::Ordered>)>> {
	if !time {
		let options = moq_net::group::Fetch::default().with_priority(fill.priority());
		return match track.fetch_group(sequence, options).await {
			Ok(group) => Ok(Some((sequence, group, None))),
			Err(moq_net::Error::NotFound) => Ok(None),
			Err(err) => Err(err),
		};
	}
	Ok(match containing(track, sequence, fill.priority()).await? {
		Some((group, placing)) if (fill.floor()..=sequence).contains(&group.start) => {
			Some((group.start, group, Some(placing)))
		}
		// One below the floor is older than the group the reader's cache holds there, which then
		// contains the time.
		_ => None,
	})
}

/// The source group containing `time`, placed as any reader of the source places it: the newest
/// starting at or before it, unless that group ended whole before it. Returned with the read
/// holding it, or `None` when no group contains the time.
async fn containing(
	track: &moq_net::track::Consumer,
	time: u64,
	priority: i8,
) -> moq_net::Result<Option<(moq_net::group::Consumer, moq_net::track::Ordered)>> {
	use moq_net::track::{Order, Range, Start, Subscription};

	let subscription = Subscription::default()
		.with_ranges([Range {
			start: Start::Time(time),
			end: Some(time),
		}])
		.with_order(Order::Ascending)
		.with_priority(priority);
	let mut read = track.subscribe(subscription).await?.ordered();
	Ok(read.next_group().await?.map(|group| (group, read)))
}

/// Drop the run of output groups around `sequence`, which the source doesn't have, up to the
/// source groups on either side of it.
///
/// Keys are media times, so most of them name no group between two of the source's. Fetching
/// each would cost one request per tick of the gap; one bounded read of the source, in key order,
/// finds the groups around the miss instead, and the span between them is dropped at once.
/// The read starts at the group containing the miss. Ascending, that group was already answered
/// and the read goes on to the next one. Descending, it is the newest source group below the
/// miss, so the read reaches no further back; only when no group contains the miss, as when the
/// newest one below ended whole before it, is the span below read from the fill's first group.
///
/// The miss itself is dropped first. A source holding no group at all resolves no time until it
/// holds one, as it would for any reader of it, so the lookup for the rest then waits for the
/// source's next group, the next group's readers leaving, or the rung retiring. A retired rung's
/// drain can't wait on the source's media, so one whose lookup can't answer at once refuses the
/// rest of the fill, the way it is being filled, in one call: nothing says the source lost it.
async fn drop_gap(
	rung: &Rung,
	track: &moq_net::track::Consumer,
	fill: &moq_net::track::FillGroups,
	sequence: u64,
) -> Result<(), Error> {
	use moq_net::track::{Order, Range, Start, Subscription};

	// The miss is the source's answer, so its readers have it at once, whatever the lookup for
	// the rest of the gap waits on.
	fill.drop_groups(sequence..sequence + 1);

	let first = fill.start().group;
	let end = fill.end();
	let below = matches!(fill.order(), Order::Descending);
	// The rest of the gap, the way the fill is being filled, starting from the group next to
	// the miss. A fill with none past it has nothing more to find.
	let (rest, next) = match below {
		true => (first..sequence, sequence.checked_sub(1).filter(|next| *next >= first)),
		false => (sequence + 1..end, Some(sequence + 1).filter(|next| *next < end)),
	};
	let Some(next) = next else {
		return Ok(());
	};
	// The source groups next to the miss, read in key order from the group containing `from`
	// through the one containing `last`: the newest starting below the miss, and the first at or
	// past it.
	let read = |from: u64, last: u64| async move {
		let subscription = Subscription::default()
			.with_ranges([Range {
				start: Start::Time(from),
				end: Some(last),
			}])
			.with_order(Order::Ascending)
			.with_priority(fill.priority());
		let mut groups = track.subscribe(subscription).await?.ordered();
		let mut before = None;
		// A group given up on is no source group.
		while let Some(group) = groups.next_group().await? {
			if group.start >= sequence {
				return Ok::<_, moq_net::Error>((before, Some(group.start)));
			}
			before = Some(group.start);
		}
		Ok((before, None))
	};
	let around = async {
		if !below {
			// The fill holds the miss, so it has a last key.
			return read(sequence, end - 1).await;
		}
		let held = containing(track, sequence, fill.priority()).await?;
		match held.map(|(group, _)| group.start).filter(|start| *start < sequence) {
			Some(before) => Ok((Some(before), None)),
			// The fill holds a key below the miss, so there is one to read through.
			None => read(first, sequence - 1).await,
		}
	};
	let mut retire = rung.retire.clone();
	let (before, after) = tokio::select! {
		biased;
		// Nobody waits on the group next to the miss any more: the fill moves on to what it
		// still owes, and a miss there looks again.
		_ = fill.unwanted(next) => return abandon(next),
		around = around => match around {
			Ok(around) => around,
			// The miss is settled; the rest is left to the fill, which asks for it next.
			Err(err) => return Err(err.into()),
		},
		() = retire.fired() => {
			// Nothing says the source lost the rest, so it is refused rather than named lost: a
			// later reader asks the rung that replaces this one.
			fill.refuse_groups(rest, moq_net::Error::Cancel);
			return Ok(());
		}
	};

	let span = match below {
		true => before.map_or(first, |before| before.saturating_add(1)).max(first)..sequence,
		false => rest.start..after.map_or(end, |after| after.min(end)),
	};
	fill.drop_groups(span);
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
			match source.end() {
				Some(end) => output.finish_at(end)?,
				None => output.finish()?,
			}
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

			pipeline.process(frame.payload, timestamp, keyframe).await?;
			drain(output, active, &mut pipeline.encoder)?;
		}
	}

	// One-shot group: drain both codec stages. Each packet keeps the timestamp of
	// the frame it was encoded from, so the tail stays in step.
	pipeline.flush().await?;
	drain(output, active, &mut pipeline.encoder)?;
	Ok(())
}

/// Write everything the encoder has ready to the output group.
fn drain(
	output: &mut moq_net::group::Producer,
	active: &crate::active::Guard,
	encoder: &mut Encoder,
) -> Result<(), Error> {
	while let Some(encoded) = encoder.receive()? {
		write(output, active, encoded)?;
	}
	Ok(())
}

/// Append one encoded frame to the output group in the legacy hang framing,
/// metering it once it reached the track.
///
/// Metered per frame, so the frames written before a failure are banked anyway:
/// they are on the group and a consumer may already have read them, so dropping
/// them from the meters would understate the bill exactly when something went wrong.
fn write(
	output: &mut moq_net::group::Producer,
	active: &crate::active::Guard,
	encoded: moq_video::encode::Encoded,
) -> Result<(), Error> {
	let size = encoded.payload.len() as u64;
	let frame = hang::container::Frame {
		timestamp: encoded.timestamp,
		payload: encoded.payload,
	};
	frame.write_to(output)?;
	active.produced(1, size);
	Ok(())
}

/// Decode -> resize -> encode for one fetched group of one rung.
///
/// Unless CPU output is forced, the decoder is asked to emit frames at the
/// rung's resolution (`decode::Config::scale_hint`). A decoder with a hardware
/// scaler (NVDEC) does, and its GPU frames feed the encoder in place: the NVDEC
/// -> NVENC path never touches the CPU. Frames that come back at any other size
/// are resized by the encoder, on its own thread.
struct Pipeline {
	decoder: moq_video::decode::Sink,
	encoder: Encoder,
	rung: Rung,
}

impl Pipeline {
	async fn new(rung: &Rung, session: Option<tokio::sync::OwnedSemaphorePermit>) -> Result<Self, Error> {
		let mut decode = rung.transcode.decoder();
		decode.scale_hint = scale_hint(rung.info.size, rung.transcode.resize.output);
		// A `Sink` for the same reason as the encoder above: a fetch task holds this
		// pipeline across a group read, so the codec must own its thread.
		let decoder = moq_video::decode::Sink::open(&rung.config, &decode).await?;

		Ok(Self {
			decoder,
			encoder: Encoder::new(session),
			rung: rung.clone(),
		})
	}

	/// Transcode one container payload, readying zero or more encoded frames on
	/// the encoder, each carrying the presentation time of the picture it came from.
	async fn process(&mut self, payload: Bytes, timestamp: moq_net::Timestamp, keyframe: bool) -> Result<(), Error> {
		// This group opens on an IDR. The encoder holds the request until a picture
		// actually arrives, which matters because a decoder that buffers returns
		// nothing for the access unit that asked for one.
		if keyframe {
			self.encoder.cut().await?;
		}

		self.decoder.decode(payload, timestamp, keyframe).await?;
		self.transfer().await
	}

	/// Flush the decoder and then the encoder, keeping every frame's timestamp:
	/// a one-shot group's pipeline is done once both codec stages are drained.
	async fn flush(&mut self) -> Result<(), Error> {
		self.decoder.flush().await?;
		self.transfer().await?;
		self.encoder.flush().await
	}

	/// Hand every decoded frame to the encoder.
	async fn transfer(&mut self) -> Result<(), Error> {
		while let Some(raw) = self.decoder.receive()? {
			self.encoder.encode(&self.rung, raw).await?;
		}
		Ok(())
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
	open: Option<(moq_video::encode::Sink, moq_video::Color)>,
	/// The tail of encoders closed by a switch, received ahead of the open one's output.
	tail: VecDeque<moq_video::encode::Encoded>,
	/// A cut asked for while no encoder was open, applied when one opens.
	pending_cut: bool,
	/// This encoder's place in the session budget, taken by its owner before the first open
	/// and kept across a reopen, which closes first. Declared last, so it is released only
	/// once the encoder has dropped, and lent to each open, which hands it back.
	session: Option<tokio::sync::OwnedSemaphorePermit>,
}

impl Encoder {
	/// An encoder holding `session`, its place in the budget.
	fn new(session: Option<tokio::sync::OwnedSemaphorePermit>) -> Self {
		Self {
			session,
			..Self::default()
		}
	}

	/// Start the next frame with an IDR.
	async fn cut(&mut self) -> Result<(), Error> {
		match &mut self.open {
			Some((sink, _)) => sink.cut().await?,
			None => self.pending_cut = true,
		}
		Ok(())
	}

	/// Encode one frame, at any size, reopening first if its space changed.
	///
	/// The previous encoder's tail is received ahead of the frame, in order.
	async fn encode(&mut self, rung: &Rung, frame: impl Into<Arc<moq_video::Frame>>) -> Result<(), Error> {
		let frame = frame.into();
		// The space the encoder will see, after it scales the frame: an unlabelled
		// source means what its own size implies, and the resize labels it so.
		// Left to the encoder's own guess, an HD frame scaled below 576 lines would
		// arrive as BT.709 at an encoder signalling BT.601 and be refused.
		let color = frame
			.surface
			.color()
			.unwrap_or_else(|| moq_video::Color::infer(frame.size()));
		if let Some((_, signalled)) = &self.open
			&& *signalled != color
		{
			let (mut sink, _) = self.open.take().expect("checked above");
			// The new parameter sets only reach a decoder on an IDR. Owed before the old encoder
			// drains, so a drain that fails still leaves the next encoder owing it.
			self.pending_cut = true;
			sink.flush().await?;
			while let Some(encoded) = sink.receive()? {
				self.tail.push_back(encoded);
			}
			// Closed before its replacement opens, so a switch never needs a second hardware session.
			drop(sink);
		}
		let sink = match &mut self.open {
			Some((sink, _)) => sink,
			None => {
				let (sink, session) = rung.encode(color, self.session.take()).await;
				self.session = session;
				let mut sink = sink?;
				rung.rendition.opened(sink.applied());
				// Cleared only once applied: a cut that fails is still owed to the next encoder.
				if self.pending_cut {
					sink.cut().await?;
					self.pending_cut = false;
				}
				&mut self.open.insert((sink, color)).0
			}
		};
		sink.encode(frame).await?;
		Ok(())
	}

	/// Pop the next encoded frame: a closed encoder's tail first, then the open one's.
	fn receive(&mut self) -> Result<Option<moq_video::encode::Encoded>, Error> {
		if let Some(encoded) = self.tail.pop_front() {
			return Ok(Some(encoded));
		}
		match &mut self.open {
			Some((sink, _)) => Ok(sink.receive()?),
			None => Ok(None),
		}
	}

	/// Ready the frames the encoder still holds.
	async fn flush(&mut self) -> Result<(), Error> {
		if let Some((sink, _)) = &mut self.open {
			sink.flush().await?;
		}
		Ok(())
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
		// A catalog that never finishes: a dropped sender fires, so this one lives for the test.
		let (catalog, ending) = Retire::channel();
		std::mem::forget(catalog);
		let info = Resolved {
			name: "video/120p".into(),
			short_side: 120,
			size: moq_video::Size::new(160, 120),
			bitrate: moq_net::bandwidth::Rate::from_bps(100_000),
			framerate: None,
		};
		let active = crate::active::Producer::default();
		let rung = Rung {
			rendition: active.rendition(&info),
			info,
			source: "video".into(),
			feed: Feed::new(
				broadcast.clone(),
				"video".into(),
				config.clone(),
				moq_video::decode::Config::new(),
			),
			broadcast,
			config,
			scale: hang::container::TIMESCALE,
			transcode: crate::Config {
				encoder: moq_video::encode::Kind::Software,
				decoder: moq_video::decode::Kind::Software,
				..Default::default()
			},
			active,
			retire,
			ending,
		};
		(rung, source, trigger)
	}

	/// A group every reader narrowed past releases the fill's one-at-a-time fetch rather than
	/// holding it until the group is transcoded.
	#[tokio::test(start_paused = true)]
	async fn a_group_nobody_reads_releases_its_fill() {
		use std::task::Poll;
		let (rung, source, _retire) = idle_rung();
		// A rung's media max age, as `serve` accepts it, so its history is still fresh to fill.
		let timed =
			hang::container::track_info(hang::catalog::PRIORITY.video).with_timescale(moq_net::Timescale::MILLI);
		let source_fills = source.create_track("video", timed.clone()).unwrap().requests();

		let output = moq_net::broadcast::Info::default().produce();
		let mut track = output.create_track("video/120p", timed).unwrap();
		let fills = track.requests();
		let ascending = |from| {
			moq_net::track::Subscription::default()
				.with_ranges([moq_net::track::Range {
					start: moq_net::track::Start::Time(from),
					end: None,
				}])
				.with_order(moq_net::track::Order::Ascending)
		};
		let mut reader = track.subscribe(ascending(7));
		track.start_at(10).unwrap();
		let mut live = track.create_group(moq_net::group::Info { start: 10 }).unwrap();
		live.write_frame(moq_net::Timestamp::from_millis(10).unwrap(), &b"x"[..])
			.unwrap();
		live.finish().unwrap();
		let waiter = kio::Waiter::noop();
		while let Poll::Ready(Ok(Some(_))) = reader.poll_recv_group(&waiter) {}
		// The reader first asks for the group containing 7. A group starting at 6 covers it, so
		// the reader starts there and asks for the groups after it.
		let start = fills.requested_fill().await.unwrap();
		assert_eq!((start.start().group, start.floor()), (7, 0));
		start.accept(6, None).unwrap().expect("a fresh group").finish().unwrap();
		while let Poll::Ready(Ok(Some(_))) = reader.poll_recv_group(&waiter) {}
		let fill = fills.requested_fill().await.unwrap();
		assert_eq!((fill.start().group, fill.end()), (7, 10));

		let mut tasks = tokio::task::JoinSet::new();
		let (_drainer, draining) = Retire::channel();
		spawn_fill(&mut tasks, rung, fill, Arc::new(Semaphore::new(1)), draining);
		let wait = std::time::Duration::from_secs(5);
		let first = kio::clock::timeout(wait, source_fills.requested_fill())
			.await
			.expect("group 7 is fetched from the source")
			.unwrap();
		assert_eq!(first.start().group, 7);

		// The reader narrows to the live group. A group starting anywhere after 7 could still be
		// group 7's, so only a start the cache holds narrows past it.
		reader.update(ascending(10)).unwrap();
		while let Poll::Ready(Ok(Some(_))) = reader.poll_recv_group(&waiter) {}
		kio::clock::timeout(wait, tasks.join_next())
			.await
			.expect("the fill waited behind group 7, which nobody reads")
			.unwrap()
			.unwrap();
		assert!(
			kio::clock::timeout(wait, source_fills.requested_fill()).await.is_err(),
			"nothing the fill owed is still wanted"
		);
	}

	/// A fill spanning a long stretch without source groups drops it in one go, rather than
	/// asking the source for every key in it.
	#[tokio::test(start_paused = true)]
	async fn a_fill_drops_a_gap_between_source_groups_at_once() {
		fill_across_a_gap(moq_net::track::Order::Ascending, false).await;
	}

	/// Filling newest first, the miss is the gap's top, and the span below it goes at once too.
	#[tokio::test(start_paused = true)]
	async fn a_descending_fill_drops_a_gap_between_source_groups_at_once() {
		fill_across_a_gap(moq_net::track::Order::Descending, false).await;
	}

	/// A retired rung's drain drops the gap at once too, rather than a key at a time.
	#[tokio::test(start_paused = true)]
	async fn a_retired_fill_drops_a_gap_between_source_groups_at_once() {
		fill_across_a_gap(moq_net::track::Order::Ascending, true).await;
	}

	/// A fill whose source holds no group yet can't place the gap after a miss, so it waits for
	/// the source's next group, as any reader of the source would, and then drops the gap.
	#[tokio::test(start_paused = true)]
	async fn a_fill_over_an_empty_source_drops_the_gap_once_the_source_has_a_group() {
		fill_across(moq_net::track::Order::Ascending, false, Held::Empty).await;
	}

	/// Filling newest first past a source group that ended whole long before the miss, no group
	/// contains the miss, so the span below it is read from the fill's first group instead.
	#[tokio::test(start_paused = true)]
	async fn a_descending_fill_drops_a_gap_past_a_paused_group_at_once() {
		fill_across(moq_net::track::Order::Descending, false, Held::Paused).await;
	}

	async fn fill_across_a_gap(order: moq_net::track::Order, retired: bool) {
		fill_across(order, retired, Held::Groups).await;
	}

	/// What the source holds below the fill's live group.
	#[derive(PartialEq)]
	enum Held {
		/// A group at 0.
		Groups,
		/// A group at 0 that ended whole a second in, long before the gap's top.
		Paused,
		/// Nothing, until the fill has waited on the source.
		Empty,
	}

	/// A one-group fetch the source answers with a miss is answered at once, even from a source
	/// holding nothing to place a gap by: the fill has no other group to look for.
	#[tokio::test(start_paused = true)]
	async fn a_missing_group_fetched_alone_is_answered_at_once() {
		let (rung, source, _retire) = idle_rung();
		let video = source
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();

		let output = moq_net::broadcast::Info::default().produce();
		let track = output
			.create_track("video/120p", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let fills = track.requests();
		let consumer = track.consume();
		let fetching = tokio::spawn(async move { consumer.fetch_group(1, None).await.map(drop) });
		let fill = fills.requested_fill().await.unwrap();
		assert_eq!((fill.start().group, fill.end()), (1, 2));

		let mut tasks = tokio::task::JoinSet::new();
		let (_drainer, draining) = Retire::channel();
		spawn_fill(&mut tasks, rung, fill, Arc::new(Semaphore::new(1)), draining);
		let wait = std::time::Duration::from_secs(5);
		let fetched = kio::clock::timeout(wait, fetching)
			.await
			.expect("the miss is answered at once")
			.unwrap();
		assert!(fetched.is_err(), "the source doesn't have it");
		kio::clock::timeout(wait, tasks.join_next())
			.await
			.expect("the fill is done")
			.unwrap()
			.unwrap();
		drop(video);
	}

	/// Write `count` gray 320x240 frames at 30 fps from frame `first` into `group`, opening on a
	/// keyframe, each stamped at its index in microseconds.
	fn write_frames(group: &mut moq_net::group::Producer, first: u64, count: u64) {
		let mut encoder = moq_video::encode::Encoder::new(&{
			let mut config = moq_video::encode::Config::new(320, 240, moq_video::FrameRate::new(30, 1).unwrap());
			config.kind = moq_video::encode::Kind::Software;
			config
		})
		.unwrap();
		let gray = vec![0x80u8; 320 * 240 * 4];
		let mut write = |encoder: &mut moq_video::encode::Encoder| {
			while let Some(encoded) = encoder.receive().unwrap() {
				hang::container::Frame {
					timestamp: encoded.timestamp,
					payload: encoded.payload,
				}
				.write_to(group)
				.unwrap();
			}
		};
		for index in first..first + count {
			let surface = moq_video::Surface::rgba(&gray, moq_video::Size::new(320, 240)).unwrap();
			let timestamp = moq_net::Timestamp::from_micros(index * FRAME).unwrap();
			encoder.encode(&moq_video::Frame::new(surface, timestamp)).unwrap();
			write(&mut encoder);
		}
		encoder.flush().unwrap();
		write(&mut encoder);
	}

	/// One frame at 30 fps, in the source's microseconds.
	const FRAME: u64 = 33_333;

	/// A reader's time start inside a source group gets that whole group, from its start, rather
	/// than a drop because no group starts at the time itself, which would start it past the group.
	#[tokio::test]
	async fn a_time_start_inside_a_group_serves_the_group_containing_it() {
		use moq_net::track::{Order, Range, Start, Subscription};
		let (rung, source, _retire) = idle_rung();
		let info = hang::container::track_info(hang::catalog::PRIORITY.video);
		let video = source.create_track("video", info.clone()).unwrap();
		// Media-timed groups: three frames at 0, then the live group two seconds on.
		let mut first = video.append_group(moq_net::Timestamp::from_micros(0).unwrap()).unwrap();
		write_frames(&mut first, 0, 3);
		first.finish().unwrap();
		let mut live = video
			.append_group(moq_net::Timestamp::from_micros(60 * FRAME).unwrap())
			.unwrap();
		write_frames(&mut live, 60, 1);
		live.finish().unwrap();

		let output = moq_net::broadcast::Info::default().produce();
		let mut track = output.create_track("video/120p", info).unwrap();
		let fills = track.requests();
		// Between the first group's second and third frames.
		let time = FRAME + FRAME / 2;
		let mut reader = track.subscribe(
			Subscription::default()
				.with_ranges([Range {
					start: Start::Time(time),
					end: None,
				}])
				.with_order(Order::Ascending),
		);
		track.start_at(60 * FRAME).unwrap();
		track
			.create_group(moq_net::group::Info { start: 60 * FRAME })
			.unwrap()
			.finish()
			.unwrap();
		let waiter = kio::Waiter::noop();
		while let std::task::Poll::Ready(Ok(Some(_))) = reader.poll_recv_group(&waiter) {}
		let fill = fills.requested_fill().await.unwrap();
		assert!(fill.is_time_start());
		assert_eq!((fill.start().group, fill.floor(), fill.end()), (time, 0, time + 1));

		let mut tasks = tokio::task::JoinSet::new();
		let (_drainer, draining) = Retire::channel();
		spawn_fill(&mut tasks, rung, fill, Arc::new(Semaphore::new(1)), draining);
		let mut group = reader
			.recv_group()
			.await
			.unwrap()
			.expect("the group containing the time");
		assert_eq!(group.start, 0, "starts at the source group containing the time");
		let mut timestamps = Vec::new();
		while let Some(frame) = group.read_frame().await.unwrap() {
			let frame = hang::container::Frame::decode(frame.payload).unwrap();
			timestamps.push(frame.timestamp.as_micros().unwrap() as u64);
		}
		assert_eq!(
			timestamps,
			[0, FRAME, 2 * FRAME],
			"the whole group, from its first frame"
		);
		drop(video);
	}

	/// Filling newest first, the gap below a miss is placed from the source group containing the
	/// miss, rather than by reading the fill's history up from its first group: the source's
	/// handler is never asked for what lies below that group before the fill reaches it.
	#[tokio::test]
	async fn a_descending_gap_is_placed_from_the_miss() {
		use moq_net::track::{Order, Range, Start, Subscription};
		use std::task::Poll;
		const HOUR: u64 = 3_600_000_000;
		let (rung, source, _retire) = idle_rung();
		let info = hang::container::track_info(hang::catalog::PRIORITY.video);
		let video = source.create_track("video", info.clone()).unwrap();
		let source_fills = video.requests();
		// Media-timed groups at two seconds and an hour, with nothing below two seconds held.
		let mut held = video.create_group(moq_net::group::Info { start: 60 * FRAME }).unwrap();
		write_frames(&mut held, 60, 1);
		held.finish().unwrap();
		video
			.create_group(moq_net::group::Info { start: HOUR })
			.unwrap()
			.finish()
			.unwrap();

		let output = moq_net::broadcast::Info::default().produce();
		let mut track = output.create_track("video/120p", info).unwrap();
		let fills = track.requests();
		let mut reader = track.subscribe(
			Subscription::default()
				.with_ranges([Range {
					start: Start::Time(1),
					end: None,
				}])
				.with_order(Order::Descending),
		);
		track.start_at(HOUR).unwrap();
		track
			.create_group(moq_net::group::Info { start: HOUR })
			.unwrap()
			.finish()
			.unwrap();
		let waiter = kio::Waiter::noop();
		while let Poll::Ready(Ok(Some(_))) = reader.poll_recv_group(&waiter) {}
		let start = fills.requested_fill().await.unwrap();
		assert_eq!((start.start().group, start.floor()), (1, 0));
		start.accept(0, None).unwrap().expect("a fresh group").finish().unwrap();
		while let Poll::Ready(Ok(Some(_))) = reader.poll_recv_group(&waiter) {}
		let fill = fills.requested_fill().await.unwrap();
		assert_eq!(
			(fill.start().group, fill.end(), fill.order()),
			(1, HOUR, Order::Descending)
		);

		let mut tasks = tokio::task::JoinSet::new();
		let (_drainer, draining) = Retire::channel();
		spawn_fill(&mut tasks, rung, fill, Arc::new(Semaphore::new(1)), draining);
		let miss = source_fills.requested_fill().await.unwrap();
		assert_eq!((miss.start().group, miss.is_time_start()), (HOUR - 1, false));
		miss.drop_groups(HOUR - 1..HOUR);

		// The span down to the group at two seconds is dropped at once, and that group is next.
		// Placing the miss asks the source only between its group at two seconds and the miss, which
		// its handler answers with nothing there.
		let place = source_fills.requested_fill().await.unwrap();
		assert!(place.is_time_start());
		assert_eq!((place.start().group, place.floor()), (HOUR - 1, 60 * FRAME + 1));
		place.drop_groups(HOUR - 1..HOUR);
		let next = reader
			.recv_group()
			.await
			.unwrap()
			.expect("the source's group below the miss");
		assert_eq!(next.start, 60 * FRAME);
		drop(video);
	}

	/// Fill output starts 1 to an hour, with source groups at 0 and an hour, or, when `held` is
	/// empty, none until the fill has waited on the source.
	async fn fill_across(order: moq_net::track::Order, retired: bool, held: Held) {
		use std::task::Poll;
		const HOUR: u64 = 3_600_000_000;
		let (rung, source, retire) = idle_rung();
		let video = source
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let empty = held == Held::Empty;
		if !empty {
			let first = video.append_group(moq_net::Timestamp::from_micros(0).unwrap()).unwrap();
			match held {
				Held::Paused => drop(first.finish_at(1_000_000).unwrap()),
				_ => drop(first.finish().unwrap()),
			}
			video
				.append_group(moq_net::Timestamp::from_micros(HOUR).unwrap())
				.unwrap()
				.finish()
				.unwrap();
		}

		let output = moq_net::broadcast::Info::default().produce();
		let mut track = output
			.create_track("video/120p", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let fills = track.requests();
		let mut reader = track.subscribe(
			moq_net::track::Subscription::default()
				.with_ranges([moq_net::track::Range {
					start: moq_net::track::Start::Time(1),
					end: None,
				}])
				.with_order(order),
		);
		track.start_at(HOUR).unwrap();
		track
			.create_group(moq_net::group::Info { start: HOUR })
			.unwrap()
			.finish()
			.unwrap();
		let waiter = kio::Waiter::noop();
		while let Poll::Ready(Ok(Some(_))) = reader.poll_recv_group(&waiter) {}
		let start = fills.requested_fill().await.unwrap();
		assert_eq!((start.start().group, start.floor()), (1, 0));
		start.accept(0, None).unwrap().expect("a fresh group").finish().unwrap();
		while let Poll::Ready(Ok(Some(_))) = reader.poll_recv_group(&waiter) {}
		let fill = fills.requested_fill().await.unwrap();
		assert_eq!((fill.start().group, fill.end()), (1, HOUR));
		assert_eq!(fill.order(), order);

		// A retired rung's fetches drain too.
		let (drainer, draining) = Retire::channel();
		if retired {
			retire.send(true).unwrap();
			drainer.send(true).unwrap();
		}
		let mut tasks = tokio::task::JoinSet::new();
		spawn_fill(&mut tasks, rung, fill, Arc::new(Semaphore::new(1)), draining);
		let wait = std::time::Duration::from_secs(5);
		if empty {
			assert!(
				kio::clock::timeout(wait, tasks.join_next()).await.is_err(),
				"nothing places the gap yet"
			);
			// As a media producer's first group does, saying nothing came before it.
			video.skip_groups(0..HOUR + 1);
			video
				.append_group(moq_net::Timestamp::from_micros(HOUR + 1).unwrap())
				.unwrap()
				.finish()
				.unwrap();
		}
		kio::clock::timeout(wait, tasks.join_next())
			.await
			.expect("the gap is dropped whole")
			.unwrap()
			.unwrap();
		drop(video);
	}

	/// Retirement's drain finishes a fill whose reader stopped reading: the groups still owed
	/// are served without waiting for that reader to pick up the last one.
	#[tokio::test(start_paused = true)]
	async fn retirement_serves_a_fill_past_a_reader_that_stopped() {
		drain_past_a_reader_that_stopped(false).await;
	}

	/// The source catalog ending drains a fill the same way once the live path is done, or the
	/// rung would wait on that reader before its track could end. Until then the fill keeps
	/// pace with its readers, or a wide fill would evict groups a slow reader has yet to read.
	#[tokio::test(start_paused = true)]
	async fn a_finished_catalog_serves_a_fill_past_a_reader_that_stopped() {
		drain_past_a_reader_that_stopped(true).await;
	}

	/// Drain a fill whose reader stopped, on retirement or, when `ending`, the catalog's end.
	async fn drain_past_a_reader_that_stopped(ending: bool) {
		use std::task::Poll;
		let (mut rung, source, retire) = idle_rung();
		let (catalog, signal) = Retire::channel();
		rung.ending = signal;
		let timed =
			hang::container::track_info(hang::catalog::PRIORITY.video).with_timescale(moq_net::Timescale::MILLI);
		let source_fills = source.create_track("video", timed.clone()).unwrap().requests();

		let output = moq_net::broadcast::Info::default().produce();
		let mut track = output.create_track("video/120p", timed).unwrap();
		let fills = track.requests();
		let mut reader = track.subscribe(
			moq_net::track::Subscription::default()
				.with_ranges([moq_net::track::Range {
					start: moq_net::track::Start::Time(7),
					end: None,
				}])
				.with_order(moq_net::track::Order::Ascending),
		);
		track.start_at(10).unwrap();
		let mut live = track.create_group(moq_net::group::Info { start: 10 }).unwrap();
		live.write_frame(moq_net::Timestamp::from_millis(10).unwrap(), &b"x"[..])
			.unwrap();
		live.finish().unwrap();
		let waiter = kio::Waiter::noop();
		while let Poll::Ready(Ok(Some(_))) = reader.poll_recv_group(&waiter) {}
		// The reader first asks for the group containing 7. A group starting at 6 covers it, so
		// the reader starts there and asks for the groups after it.
		let start = fills.requested_fill().await.unwrap();
		assert_eq!((start.start().group, start.floor()), (7, 0));
		start.accept(6, None).unwrap().expect("a fresh group").finish().unwrap();
		while let Poll::Ready(Ok(Some(_))) = reader.poll_recv_group(&waiter) {}

		// The live path delivers group 7, and the reader never reads again to pick it up.
		let mut seven = track.create_group(moq_net::group::Info { start: 7 }).unwrap();
		seven
			.write_frame(moq_net::Timestamp::from_millis(7).unwrap(), &b"x"[..])
			.unwrap();
		seven.finish().unwrap();

		let (finished, mut finishing) = tokio::sync::watch::channel(false);
		let mut running = tokio::spawn(async move { fetches(&rung, fills, &mut finishing).await });
		let wait = std::time::Duration::from_secs(5);
		assert!(
			kio::clock::timeout(wait, source_fills.requested_fill()).await.is_err(),
			"group 8 waits for a reader to pick up group 7"
		);

		match ending {
			true => {
				catalog.send(true).unwrap();
				assert!(
					kio::clock::timeout(wait, source_fills.requested_fill()).await.is_err(),
					"group 8 still waits while the live path serves"
				);
				finished.send(true).unwrap();
			}
			false => retire.send(true).unwrap(),
		}
		let requested = kio::clock::timeout(wait, source_fills.requested_fill())
			.await
			.expect("a draining rung serves what its fill still owes")
			.unwrap();
		assert_eq!(requested.start().group, 8);
		if ending {
			// The source's media is still there to transcode, which is the drain's business from here.
			running.abort();
			return;
		}
		// The source no longer has it. It holds no group to place the gap after it by, and the
		// drain can't wait for one, so the rest of the fill is refused with it, not named lost.
		requested.drop_groups(8..9);
		kio::clock::timeout(wait, &mut running)
			.await
			.expect("the drain finishes")
			.unwrap()
			.unwrap();
		assert!(
			kio::clock::timeout(wait, source_fills.requested_fill()).await.is_err(),
			"the rest of the fill is refused at once, not fetched a group at a time"
		);
		drop(finished);
	}

	/// A session freed as the catalog ends goes unused by a rung waiting for one: it ends
	/// rather than attach a feed the source no longer stands behind. Which ready branch the
	/// wait takes is random, so the race runs many times.
	#[tokio::test(start_paused = true)]
	async fn a_session_freed_as_the_catalog_ends_attaches_no_feed() {
		use std::future::Future;
		for _ in 0..64 {
			let (mut rung, source, _retire) = idle_rung();
			let (catalog, ending) = Retire::channel();
			rung.ending = ending;
			rung.transcode.sessions = crate::Sessions::new(std::num::NonZeroUsize::MIN);
			let held = rung.transcode.sessions.serve().await;
			let video = source.create_track("video", None).unwrap();

			let output = moq_net::broadcast::Info::default().produce();
			let mut track = output.create_track("video/120p", None).unwrap();
			let _viewer = track.subscribe(None);
			let mut running = std::pin::pin!(live(&rung, &mut track));
			assert!(
				running
					.as_mut()
					.poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
					.is_pending(),
				"the rung waits for the held session"
			);

			drop(held);
			catalog.send(true).unwrap();
			let ended = kio::clock::timeout(std::time::Duration::from_secs(5), running)
				.await
				.expect("an ending rung does not serve");
			assert!(matches!(ended, Ok(Ended::Unfinished)));
			assert!(!video.demand().wanted(), "no feed subscribed to the source");
			assert_eq!(rung.transcode.sessions.available(), 1);
		}
	}

	/// A restarted source cancels the serving future even while a rung waits for media.
	/// Its subscribers must see that cause, rather than an unfinished producer drop.
	#[tokio::test(start_paused = true)]
	async fn cancelling_an_idle_rung_reports_cancel() {
		let (rung, source, _retire) = idle_rung();
		// The subscriber below starts the live path. Without a source track its feed fails
		// on its own and ends the serve before the abort lands.
		let video = source.create_track("video", None).unwrap();
		let output = moq_net::broadcast::Info::default().produce();
		let mut dynamic = output.requests();
		let view = output.consume();
		let subscribing = tokio::spawn(async move { view.track("rung").unwrap().subscribe(None).await.unwrap() });
		let request = dynamic.requested_track().await.unwrap();
		let running = tokio::spawn(serve(rung, request));
		let mut track = subscribing.await.unwrap();
		// The track is admitted before `serve` is polled; the feed's subscription proves the
		// live path is parked waiting for media rather than never started.
		let mut demand = video.demand();
		while !demand.wanted_changed().await.unwrap() {}
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
		encoder.encode(&rung, frame(Color::BT709, 0)).await.unwrap();
		encoder.encode(&rung, frame(Color::BT709, 33_333)).await.unwrap();
		encoder.encode(&rung, frame(pq, 66_666)).await.unwrap();
		encoder.flush().await.unwrap();
		let mut encoded = Vec::new();
		while let Some(unit) = encoder.receive().unwrap() {
			encoded.push(unit);
		}

		let timestamps: Vec<u64> = encoded
			.iter()
			.map(|e| e.timestamp.as_micros().unwrap() as u64)
			.collect();
		assert_eq!(
			timestamps,
			[0, 33_333, 66_666],
			"the old tail comes out ahead of the switch"
		);
		assert!(encoded[0].keyframe);
		assert!(encoded[2].keyframe, "the switched frame");
	}

	/// An unlabelled HD frame means BT.709, and scaling it to an SD rung keeps
	/// that label. The rung's encoder has to signal it too, rather than the
	/// BT.601 its own size implies, or it refuses the frame it scaled.
	#[tokio::test]
	#[cfg(feature = "openh264")]
	async fn an_unlabelled_hd_frame_encodes_on_an_sd_rung() {
		let (rung, _source, _retire) = idle_rung();
		let hd = moq_video::Size::new(1280, 720);
		let i420 = moq_video::I420::new(hd, vec![128; hd.pixels() as usize * 3 / 2]).unwrap();
		let frame = moq_video::Frame::new(
			moq_video::Surface::I420(i420),
			moq_net::Timestamp::from_micros(0).unwrap(),
		);

		let mut encoder = Encoder::default();
		encoder.cut().await.unwrap();
		assert_eq!(rung.rendition.applied(), None);
		encoder.encode(&rung, frame).await.unwrap();
		encoder.flush().await.unwrap();
		assert!(encoder.receive().unwrap().expect("the frame").keyframe);
		// The rendition reports the opened encoder's preset, the one the caller requested by default.
		let applied = rung.rendition.applied().expect("the encoder opened");
		assert_eq!(applied.preset, Some(moq_video::encode::Preset::LowLatency));
	}

	/// Before the feed has placed the edge, a range reaches it only if it is open or joins live:
	/// a bounded history range would hold a session through a paused source.
	#[test]
	fn before_any_group_only_open_ranges_and_live_joins_reach_live() {
		use moq_net::track::{Range, Start, Subscription};
		let reaches =
			|start, end, edge| reaches_live(&Subscription::default().with_ranges([Range { start, end }]), edge);
		let live = Start::Live { min: None, max: None };
		assert!(reaches(Start::Time(3), None, None));
		assert!(!reaches(Start::Time(3), Some(4), None));
		assert!(reaches(live, Some(4), None));
		assert!(reaches(Start::Time(3), Some(4), Some(4)));
		assert!(!reaches(Start::Time(3), Some(4), Some(5)));
	}

	/// A fetched NVDEC group reaches the encoder at native size when
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
		let mut group = track.create_group(moq_net::group::Info { start: 0 }).unwrap();
		let guard = active.attach(&active.rendition(&rung));

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

		write(&mut group, &guard, good).unwrap();
		assert!(write(&mut group, &guard, bad).is_err());
		assert_eq!(rendition.frames(), 1);
		assert_eq!(rendition.bytes(), 5);
		// One frame is still a start, so the rendition reads as encoding.
		assert!(cursor.try_next().expect("edge").encoding);
	}
}
