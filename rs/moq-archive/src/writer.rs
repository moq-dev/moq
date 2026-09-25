//! Record explicitly selected tracks of a [`broadcast::Consumer`] into a [`Store`].
//!
//! The application enrolls each track as pacing or non-pacing through a [`Control`]; the writer
//! never parses a catalog. Every complete group feeds the timeline segmenter. For each closed
//! segment the writer stores one object per participating track, omits any track whose object
//! failed, commits the record through its own timeline encoder, and stores that segment's timeline
//! groups before starting the next one. The timeline therefore only advertises durable objects.
//!
//! A writer started on a prefix that already holds a recording resumes it: see [`Writer::new`].
//!
//! ```no_run
//! # async fn example(source: moq_net::broadcast::Consumer) -> moq_archive::Result<()> {
//! use moq_archive::object_store::memory::InMemory;
//!
//! let store = moq_archive::Store::new(InMemory::new(), "recordings/demo");
//! let writer = moq_archive::Writer::new(store, source, Default::default()).await?;
//! let control = writer.control();
//! control.pacing_track("video").await?;
//! control.pacing_track("audio").await?;
//! control.track("catalog.json").await?;
//! writer.run().await?;
//! # Ok(())
//! # }
//! ```

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::Duration;

use futures::future::BoxFuture;
use futures::stream::FuturesUnordered;
use futures::{FutureExt, StreamExt};
use hang::timeline::Record;
use moq_mux::timeline::{self, Deferred, DeferredDrain, Pending, Recorder, Reserved};
use moq_net::{Timescale, Timestamp, broadcast, group, track};
use object_store::ObjectStore;
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;

use crate::recover::recover;
use crate::segment::{Frame, Group, Object};
use crate::{Error, Info, Key, Result, Store};

/// Subscribers ask for every cached group; the publisher clamps this to its own max age.
const REPLAY: Duration = Duration::from_secs(u32::MAX as u64);

/// How a [`Writer`] segments and retains its recording.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Config {
	/// Segment pacing for the recording's timeline.
	pub timeline: timeline::Config,
	/// Expire old segments (a DVR), or keep everything when `None` (an archive).
	pub retention: Option<Retention>,
}

impl Config {
	/// Set [`timeline`](Self::timeline).
	pub fn with_timeline(mut self, timeline: timeline::Config) -> Self {
		self.timeline = timeline;
		self
	}

	/// Set [`retention`](Self::retention).
	pub fn with_retention(mut self, retention: impl Into<Option<Retention>>) -> Self {
		self.retention = retention.into();
		self
	}
}

/// A DVR retention policy.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Retention {
	/// Keep at least this much content, measured from timeline records.
	pub window: Duration,
	/// How long expired objects outlive the timeline that stopped advertising them, so readers
	/// holding an older timeline can finish their GETs.
	pub grace: Duration,
}

impl Retention {
	/// Keep `window` of content and delete expired objects `grace` after they leave the timeline.
	pub fn new(window: Duration, grace: Duration) -> Self {
		Self { window, grace }
	}
}

/// Records enrolled tracks until the source broadcast closes.
///
/// Enroll tracks through [`control`](Self::control), then drive the recording with
/// [`run`](Self::run).
pub struct Writer<S> {
	control: Control<S>,
	commands: mpsc::UnboundedReceiver<Command>,
	committer: Committer<S>,
	grace: Option<Duration>,
	/// Deadlines for deleting expired or orphaned objects, oldest first.
	deletions: VecDeque<(Instant, Vec<Key>)>,
	// Owns the recording's timeline track.
	_timeline: broadcast::Producer,
}

/// Enrolls, cuts, and removes tracks on a [`Writer`].
pub struct Control<S> {
	shared: Arc<Shared<S>>,
	deferred: Deferred,
	commands: mpsc::UnboundedSender<Command>,
}

impl<S> Clone for Control<S> {
	fn clone(&self) -> Self {
		Self {
			shared: self.shared.clone(),
			deferred: self.deferred.clone(),
			commands: self.commands.clone(),
		}
	}
}

/// Withholds segment commits until dropped.
pub struct Reservation {
	_inner: Reserved,
}

struct Shared<S> {
	store: Store<S>,
	source: broadcast::Consumer,
	/// The recording-owned timeline track, which no source track may shadow.
	timeline: String,
	/// Every name ever enrolled. A name is never reused, so its object ranges stay increasing.
	enrolled: Mutex<HashSet<String>>,
	/// Per track, the largest group a resumed recording already stored.
	floors: HashMap<String, u64>,
}

enum Command {
	Enroll {
		name: String,
		subscriber: Box<track::Subscriber>,
		recorder: Recorder,
		timescale: Timescale,
	},
	Remove(String),
	/// A cut may have completed a segment.
	Poke,
}

impl<S: ObjectStore> Writer<S> {
	/// Start a recording under `store`'s prefix, reading tracks from `source`, or resume the one
	/// already there.
	///
	/// Resuming replays the retained timeline and continues at the next segment. A track refuses
	/// any group at or below the largest one stored for it, so a source whose group sequences
	/// restarted needs a new prefix. A DVR also deletes, one grace period after recovery, every
	/// group object its retained records do not reference, such as interrupted expirations and
	/// uploads. The writer must own the prefix exclusively. Fails, deleting nothing, when the
	/// recording cannot be listed or its timeline cannot be replayed.
	pub async fn new(store: Store<S>, source: broadcast::Consumer, config: Config) -> Result<Self> {
		let section = timeline::Segmenter::new(config.timeline.clone()).section();
		let recovery = recover(&store, &section.track, config.retention.is_some()).await?;

		let broadcast = broadcast::Info::new().produce();
		let timeline = match &recovery.checkpoint {
			Some(checkpoint) => {
				timeline::Producer::resume(&broadcast, config.timeline, checkpoint).map_err(timeline_error)?
			}
			None => timeline::Producer::new(&broadcast, config.timeline),
		};
		let deferred = timeline.deferred().map_err(timeline_error)?;

		let replay = track::Subscription::default().with_max_age(REPLAY);
		let groups = broadcast
			.consume()
			.track(&section.track)
			.map_err(source_error)?
			.subscribe(replay)
			.await
			.map_err(source_error)?
			.ordered();
		let info = Info::new(groups.info().priority, groups.info().timescale.as_u64())?;
		store.put_info(&section.track, &info).await?;

		let shared = Arc::new(Shared {
			store,
			source,
			timeline: section.track,
			enrolled: Mutex::new(HashSet::new()),
			floors: recovery.floors,
		});
		let (commands, receiver) = mpsc::unbounded_channel();
		let retention = config.retention;
		let mut deletions = VecDeque::new();
		if let Some(retention) = &retention
			&& !recovery.orphans.is_empty()
		{
			deletions.push_back((Instant::now() + retention.grace, recovery.orphans));
		}
		Ok(Self {
			control: Control {
				shared: shared.clone(),
				deferred,
				commands,
			},
			commands: receiver,
			committer: Committer {
				shared,
				timeline,
				groups,
				timescale: section.timescale.into(),
				retention: retention.as_ref().map(|r| r.window),
				window: recovery.checkpoint.map(|c| c.records.into()).unwrap_or_default(),
				sequence: recovery.sequence,
			},
			grace: retention.map(|r| r.grace),
			deletions,
			_timeline: broadcast,
		})
	}

	/// A handle that enrolls, cuts, and removes tracks, before or during [`run`](Self::run).
	pub fn control(&self) -> Control<S> {
		self.control.clone()
	}

	/// Record until the source broadcast closes and every enrolled track ends.
	///
	/// Then flush the final segment and finish the timeline. Fails when the timeline cannot be
	/// committed or stored, or an enrolled track delivers a group the recording cannot represent:
	/// the recording stops at its last durable timeline object. Returns the source's error, after
	/// finishing, when the broadcast aborted.
	pub async fn run(self) -> Result<()> {
		let Self {
			control,
			mut commands,
			committer,
			grace,
			mut deletions,
			_timeline,
		} = self;
		let shared = control.shared.clone();
		let source = shared.source.clone();
		let mut segments = Segments::Live(control.deferred.clone());
		drop(control);

		let mut tracks: HashMap<String, TrackState> = HashMap::new();
		// Reported groups waiting for the record that names them, kept past a track's removal.
		let mut buffered: HashMap<String, BTreeMap<u64, Group>> = HashMap::new();
		let mut reads = FuturesUnordered::new();
		let mut committer = Some(committer);
		let mut commit: Option<Commit<S>> = None;
		let mut closed = false;
		// Cleared once the channel yields nothing more: every sender dropped, or it closed and drained.
		let mut accepting = true;

		loop {
			if commit.is_none()
				&& let Some(pending) = segments.next()
			{
				let objects = take_objects(&pending, &mut buffered);
				let committer = committer.take().expect("idle committer");
				commit = Some(committer.commit(pending, objects).boxed());
			}

			if commit.is_none() && closed && tracks.is_empty() {
				// Refuse late commands, so an enrollment racing the end fails instead of vanishing.
				commands.close();
				if !accepting {
					match segments {
						Segments::Live(deferred) => {
							segments = Segments::Drain(deferred.finish());
							continue;
						}
						Segments::Drain(_) => break,
					}
				}
			}

			let deadline = deletions.front().map(|(deadline, _)| *deadline);
			tokio::select! {
				biased;
				command = commands.recv(), if accepting => match command {
					None => accepting = false,
					Some(Command::Enroll { name, subscriber, recorder, timescale }) => {
						let (cancel, cancelled) = watch::channel(());
						let largest = shared.floors.get(&name).copied();
						reads.push(guard(cancelled.clone(), recv(name.clone(), subscriber)).boxed());
						tracks.insert(name, TrackState {
							recorder,
							timescale,
							largest,
							reported: None,
							accepted: BTreeMap::new(),
							subscribed: true,
							_cancel: cancel,
							cancelled,
						});
					}
					Some(Command::Remove(name)) => {
						tracks.remove(&name);
					}
					Some(Command::Poke) => {}
				},
				Some(read) = reads.next(), if !reads.is_empty() => {
					handle(read, &mut tracks, &mut buffered, &mut reads)?;
				}
				(done, result) = async { commit.as_mut().unwrap().await }, if commit.is_some() => {
					commit = None;
					committer = Some(done);
					let expired = result?;
					if let Some(grace) = grace && !expired.is_empty() {
						deletions.push_back((Instant::now() + grace, expired));
					}
				}
				_ = source.closed(), if !closed => closed = true,
				_ = tokio::time::sleep_until(deadline.unwrap_or_else(Instant::now)), if deadline.is_some() => {
					let (_, keys) = deletions.pop_front().unwrap();
					delete(&shared.store, keys).await;
				}
			}
		}

		let Segments::Drain(drain) = segments else {
			unreachable!("the loop only ends after draining");
		};
		drain.result().map_err(timeline_error)?;
		let mut committer = committer.expect("idle committer");
		committer.timeline.finish().map_err(timeline_error)?;

		// Complete expirations already committed to the timeline; nothing new expires after the
		// final segment.
		for (deadline, keys) in deletions {
			tokio::time::sleep_until(deadline).await;
			delete(&shared.store, keys).await;
		}

		if source.is_finished() {
			Ok(())
		} else {
			Err(source_error(source.closed().await))
		}
	}
}

impl<S: ObjectStore> Control<S> {
	/// Enroll `name` without letting it influence segmentation, such as a catalog or metadata track.
	///
	/// Subscribes to the track and creates its `.info` before accepting any group. Fails when the
	/// name was already enrolled, is the recording's timeline, or `.info` conflicts.
	pub async fn track(&self, name: &str) -> Result<()> {
		self.enroll(name, false).await
	}

	/// Enroll the continuously publishing track `name` as a pacing track.
	///
	/// It votes on segment boundaries and holds every segment back until its groups are known. A
	/// pacing track that stalls without closing stalls the recording by design; apply a deadline
	/// with [`cut`](Self::cut) or [`remove`](Self::remove).
	pub async fn pacing_track(&self, name: &str) -> Result<()> {
		self.enroll(name, true).await
	}

	/// Declare a segment boundary at `pts`, overriding the configured minimum-duration pacing.
	///
	/// The first cut takes over boundaries for good, as with [`timeline::Producer::cut`].
	pub fn cut(&self, pts: Timestamp) -> Result<()> {
		self.deferred.cut(pts).map_err(timeline_error)?;
		self.send(Command::Poke)
	}

	/// Hold segment commits back until this guard drops, so a batch of tracks can enroll first.
	#[must_use = "dropping the reservation releases segment commits"]
	pub fn reserve(&self) -> Reservation {
		Reservation {
			_inner: self.deferred.reserve(),
		}
	}

	/// Stop recording `name`, dropping its incomplete groups. The name cannot be enrolled again.
	pub fn remove(&self, name: &str) -> Result<()> {
		self.send(Command::Remove(name.to_string()))
	}

	async fn enroll(&self, name: &str, pacing: bool) -> Result<()> {
		if name == self.shared.timeline || !self.shared.enrolled.lock().unwrap().insert(name.to_string()) {
			return Err(Error::Enrolled(name.to_string()));
		}
		let result = self.subscribe(name, pacing).await;
		if result.is_err() {
			self.shared.enrolled.lock().unwrap().remove(name);
		}
		result
	}

	async fn subscribe(&self, name: &str, pacing: bool) -> Result<()> {
		let replay = track::Subscription::default().with_max_age(REPLAY);
		let subscriber = self
			.shared
			.source
			.track(name)
			.map_err(source_error)?
			.subscribe(replay)
			.await
			.map_err(source_error)?;
		let timescale = subscriber.info().timescale;
		let info = Info::new(subscriber.info().priority, timescale.as_u64())?;
		self.shared.store.put_info(name, &info).await?;

		let recorder = match pacing {
			true => self.deferred.pacing_track(name),
			false => self.deferred.track(name),
		};
		self.send(Command::Enroll {
			name: name.to_string(),
			subscriber: Box::new(subscriber),
			recorder,
			timescale,
		})
	}

	fn send(&self, command: Command) -> Result<()> {
		self.commands.send(command).map_err(|_| Error::Closed)
	}
}

/// One enrolled track's accepted groups.
struct TrackState {
	recorder: Recorder,
	timescale: Timescale,
	/// The newest group accepted or already stored; later arrivals must exceed it.
	largest: Option<u64>,
	/// The first-frame timestamp of the newest reported group; later groups must not precede it.
	reported: Option<u64>,
	/// Accepted groups in sequence order: `None` while reading, `Some` once complete. Reported in
	/// order, so a complete group waits for every earlier accepted one.
	accepted: BTreeMap<u64, Option<Group>>,
	/// The subscription is still delivering groups.
	subscribed: bool,
	/// Dropping the sender cancels every read for this track.
	_cancel: watch::Sender<()>,
	cancelled: watch::Receiver<()>,
}

impl TrackState {
	/// Report every complete group at the front of the accepted queue.
	///
	/// Fails on a group that starts before the previous one, which the timeline cannot place.
	fn report(&mut self, name: &str, buffered: &mut HashMap<String, BTreeMap<u64, Group>>) -> Result<()> {
		while let Some(entry) = self.accepted.first_entry() {
			if entry.get().is_none() {
				break;
			}
			let group = entry.remove().expect("checked above");
			let (Some(first), Some(last)) = (group.frames.first(), group.frames.last()) else {
				continue;
			};
			if let Some(reported) = self.reported
				&& first.timestamp < reported
			{
				return Err(malformed(
					name,
					group.sequence,
					format!("timestamp {} precedes {reported}", first.timestamp),
				));
			}
			self.reported = Some(first.timestamp);
			// Frame timestamps were validated while reading, so these conversions succeed.
			let (Ok(first), Ok(last)) = (
				Timestamp::new(first.timestamp, self.timescale),
				Timestamp::new(last.timestamp, self.timescale),
			) else {
				continue;
			};
			self.recorder.record(group.sequence, first, true);
			self.recorder.end(last);
			buffered
				.entry(name.to_string())
				.or_default()
				.insert(group.sequence, group);
		}
		Ok(())
	}
}

// Each read is already boxed as a future, so the variant sizes cost nothing extra.
#[allow(clippy::large_enum_variant)]
enum Read {
	Group {
		name: String,
		subscriber: Box<track::Subscriber>,
		result: moq_net::Result<Option<group::Consumer>>,
	},
	Frames {
		name: String,
		sequence: u64,
		result: moq_net::Result<Vec<moq_net::frame::Frame>>,
	},
	Cancelled,
}

async fn recv(name: String, mut subscriber: Box<track::Subscriber>) -> Read {
	let result = subscriber.recv_group().await;
	Read::Group {
		name,
		subscriber,
		result,
	}
}

async fn frames(name: String, mut group: group::Consumer) -> Read {
	let sequence = group.sequence;
	let mut frames = Vec::new();
	let result = loop {
		match group.read_frame().await {
			Ok(Some(frame)) => frames.push(frame),
			Ok(None) => break Ok(frames),
			Err(err) => break Err(err),
		}
	};
	Read::Frames { name, sequence, result }
}

/// Resolve to [`Read::Cancelled`] once the track's state drops.
async fn guard(mut cancelled: watch::Receiver<()>, read: impl Future<Output = Read>) -> Read {
	tokio::select! {
		read = read => read,
		_ = cancelled.changed() => Read::Cancelled,
	}
}

/// Fails on malformed source input; a group the network aborted is dropped instead.
fn handle(
	read: Read,
	tracks: &mut HashMap<String, TrackState>,
	buffered: &mut HashMap<String, BTreeMap<u64, Group>>,
	reads: &mut FuturesUnordered<BoxFuture<'static, Read>>,
) -> Result<()> {
	let name = match read {
		Read::Cancelled => return Ok(()),
		Read::Group {
			name,
			subscriber,
			result,
		} => {
			let Some(track) = tracks.get_mut(&name) else {
				return Ok(());
			};
			match result {
				Ok(Some(group)) => {
					let sequence = group.sequence;
					if track.largest.is_some_and(|largest| sequence <= largest) {
						tracing::debug!(track = %name, sequence, "refusing a duplicate or decreasing group");
					} else {
						track.largest = Some(sequence);
						track.accepted.insert(sequence, None);
						reads.push(guard(track.cancelled.clone(), frames(name.clone(), group)).boxed());
					}
					reads.push(guard(track.cancelled.clone(), recv(name.clone(), subscriber)).boxed());
				}
				Ok(None) => track.subscribed = false,
				Err(err) => {
					tracing::warn!(track = %name, %err, "track ended");
					track.subscribed = false;
				}
			}
			name
		}
		Read::Frames { name, sequence, result } => {
			let Some(track) = tracks.get_mut(&name) else {
				return Ok(());
			};
			match result {
				Ok(frames) => {
					let group =
						convert(sequence, frames, track.timescale).map_err(|err| malformed(&name, sequence, err))?;
					if group.frames.is_empty() {
						track.accepted.remove(&sequence);
					} else {
						track.accepted.insert(sequence, Some(group));
					}
				}
				Err(err) => {
					tracing::warn!(track = %name, sequence, %err, "dropping an incomplete group");
					track.accepted.remove(&sequence);
				}
			}
			track.report(&name, buffered)?;
			name
		}
	};

	// A track whose subscription ended closes once its last accepted group settles.
	if tracks
		.get(&name)
		.is_some_and(|track| !track.subscribed && track.accepted.is_empty())
	{
		tracks.remove(&name);
	}
	Ok(())
}

/// Convert a complete group's frames into the track's timescale.
fn convert(sequence: u64, frames: Vec<moq_net::frame::Frame>, timescale: Timescale) -> Result<Group> {
	let frames = frames
		.into_iter()
		.map(|frame| {
			let timestamp = frame.timestamp.convert(timescale).map_err(|_| Error::Overflow)?.value();
			crate::path::check_id(timestamp)?;
			Ok(Frame {
				timestamp,
				payload: frame.payload,
			})
		})
		.collect::<Result<_>>()?;
	crate::path::check_id(sequence)?;
	Ok(Group { sequence, frames })
}

/// Take the buffered groups a pending record names, one object per track.
///
/// A track missing any advertised group cannot produce an object whose table matches its ranges,
/// so it gets `None` and is omitted.
fn take_objects(
	pending: &Pending,
	buffered: &mut HashMap<String, BTreeMap<u64, Group>>,
) -> Vec<(String, Option<Object>)> {
	pending
		.tracks
		.iter()
		.map(|(name, ranges)| {
			let track = buffered.entry(name.clone()).or_default();
			let mut groups = Vec::new();
			let mut complete = true;
			for range in ranges {
				for sequence in range.start..=range.end {
					match track.remove(&sequence) {
						Some(group) => groups.push(group),
						None => complete = false,
					}
				}
			}
			(name.clone(), complete.then_some(Object { groups }))
		})
		.collect()
}

/// An in-flight segment commit, returning the committer and the expired objects to delete.
type Commit<S> = BoxFuture<'static, (Committer<S>, Result<Vec<Key>>)>;

/// Deferred records to commit: live, then the terminal drain.
enum Segments {
	Live(Deferred),
	Drain(DeferredDrain),
}

impl Segments {
	fn next(&self) -> Option<Pending> {
		match self {
			Self::Live(deferred) => deferred.next(),
			Self::Drain(drain) => drain.next(),
		}
	}
}

/// Commits one segment at a time: media objects, the record, retention, then the timeline object.
struct Committer<S> {
	shared: Arc<Shared<S>>,
	timeline: timeline::Producer,
	/// The recording's own timeline track, read back to store its complete groups.
	groups: track::Ordered,
	/// Units per second of record `pts` and `duration`.
	timescale: u64,
	retention: Option<Duration>,
	/// Committed records still in the timeline window, oldest first.
	window: VecDeque<Record>,
	/// Added to the timeline track's group sequences, continuing a resumed recording's numbering.
	sequence: u64,
}

impl<S: ObjectStore> Committer<S> {
	/// Commit `pending`, returning the committer and the expired objects to delete after the grace.
	async fn commit(mut self, pending: Pending, objects: Vec<(String, Option<Object>)>) -> (Self, Result<Vec<Key>>) {
		let result = self.commit_inner(pending, objects).await;
		(self, result)
	}

	async fn commit_inner(&mut self, mut pending: Pending, objects: Vec<(String, Option<Object>)>) -> Result<Vec<Key>> {
		let shared = self.shared.clone();
		let store = &shared.store;
		let puts = objects.iter().map(|(name, object)| async move {
			let result = match object {
				Some(object) => store.put_groups(name, object).await.map(|_| ()),
				None => Err(Error::Timeline("advertised groups are not buffered".into())),
			};
			(name, result)
		});
		for (name, result) in futures::future::join_all(puts).await {
			if let Err(err) = result {
				tracing::warn!(track = %name, segment = pending.segment, %err, "omitting a track that was not stored");
				pending = pending.omit(name);
			}
		}

		let segment = pending.segment;
		let record = (*pending).clone();
		self.timeline.push(pending).map_err(timeline_error)?;
		self.window.push_back(record);

		let expired = self.expire();
		if !expired.is_empty() {
			self.timeline.pop(expired.len() as u64).map_err(timeline_error)?;
		}
		self.timeline.flush().map_err(timeline_error)?;

		let object = self.read_timeline()?;
		store.put_segments(&shared.timeline, segment, &object).await?;

		let mut keys = Vec::new();
		for record in expired {
			for (name, ranges) in &record.tracks {
				if let (Some(first), Some(last)) = (ranges.first(), ranges.last()) {
					keys.push(Key::groups(name.clone(), first.start..=last.end)?);
				}
			}
		}
		Ok(keys)
	}

	/// Pop the oldest records while the rest still cover the retention window.
	fn expire(&mut self) -> Vec<Record> {
		let Some(retention) = self.retention else {
			return Vec::new();
		};
		let target = (retention.as_micros() * self.timescale as u128).div_ceil(1_000_000);
		let mut total: u128 = self.window.iter().map(|record| record.duration as u128).sum();
		let mut expired = Vec::new();
		while self.window.len() > 1 {
			let front = self.window.front().expect("non-empty").duration as u128;
			if total - front < target {
				break;
			}
			total -= front;
			expired.push(self.window.pop_front().expect("non-empty"));
		}
		expired
	}

	/// Collect the timeline groups completed since the last segment.
	fn read_timeline(&mut self) -> Result<Object> {
		let waiter = kio::Waiter::noop();
		let timescale = self.groups.info().timescale;
		let mut groups = Vec::new();
		while let Poll::Ready(result) = self.groups.poll_next_group(&waiter) {
			let Some(mut group) = result.map_err(timeline_error_net)? else {
				break;
			};
			let mut frames = Vec::new();
			loop {
				match group.poll_read_frame(&waiter) {
					Poll::Ready(Ok(Some(frame))) => frames.push(frame),
					Poll::Ready(Ok(None)) => break,
					Poll::Ready(Err(err)) => return Err(timeline_error_net(err)),
					// Flushing closed every group, so an open one is a bug.
					Poll::Pending => return Err(Error::Timeline("timeline group is still open".into())),
				}
			}
			let sequence = group.sequence.checked_add(self.sequence).ok_or(Error::Overflow)?;
			groups.push(convert(sequence, frames, timescale)?);
		}
		Ok(Object { groups })
	}
}

async fn delete<S: ObjectStore>(store: &Store<S>, keys: Vec<Key>) {
	let deletes = keys.iter().map(|key| async move { (key, store.delete(key).await) });
	for (key, result) in futures::future::join_all(deletes).await {
		// An object that outlives its expiry advertises nothing; recovery cleans it up.
		if let Err(err) = result {
			tracing::warn!(?key, %err, "failed to delete an expired object");
		}
	}
}

fn timeline_error(err: moq_mux::Error) -> Error {
	Error::Timeline(err.to_string())
}

fn timeline_error_net(err: moq_net::Error) -> Error {
	Error::Timeline(err.to_string())
}

fn source_error(err: moq_net::Error) -> Error {
	Error::Source(err.to_string())
}

fn malformed(track: &str, sequence: u64, err: impl std::fmt::Display) -> Error {
	Error::Source(format!("track {track} group {sequence}: {err}"))
}

#[cfg(test)]
mod tests {

	use futures::TryStreamExt;
	use futures::stream::BoxStream;
	use object_store::memory::InMemory;
	use object_store::path::Path;
	use object_store::{
		CopyOptions, GetOptions, GetResult, ListResult, MultipartUpload, ObjectMeta, PutMultipartOptions, PutOptions,
		PutPayload, PutResult,
	};

	use super::*;
	use crate::store::list::Query;

	/// An in-memory store whose group PUTs fail for one track, and whose listings fail at the end.
	#[derive(Debug, Clone)]
	struct Failing {
		inner: Arc<InMemory>,
		track: &'static str,
		list: bool,
	}

	impl std::fmt::Display for Failing {
		fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
			write!(f, "Failing")
		}
	}

	#[async_trait::async_trait]
	impl ObjectStore for Failing {
		async fn put_opts(
			&self,
			location: &Path,
			payload: PutPayload,
			opts: PutOptions,
		) -> object_store::Result<PutResult> {
			if location.as_ref().contains(&format!("/{}/groups/", self.track)) {
				return Err(object_store::Error::NotImplemented {
					operation: "put".into(),
					implementer: "Failing".into(),
				});
			}
			self.inner.put_opts(location, payload, opts).await
		}

		async fn put_multipart_opts(
			&self,
			location: &Path,
			opts: PutMultipartOptions,
		) -> object_store::Result<Box<dyn MultipartUpload>> {
			self.inner.put_multipart_opts(location, opts).await
		}

		async fn get_opts(&self, location: &Path, options: GetOptions) -> object_store::Result<GetResult> {
			self.inner.get_opts(location, options).await
		}

		fn delete_stream(
			&self,
			locations: BoxStream<'static, object_store::Result<Path>>,
		) -> BoxStream<'static, object_store::Result<Path>> {
			self.inner.delete_stream(locations)
		}

		fn list(&self, prefix: Option<&Path>) -> BoxStream<'static, object_store::Result<ObjectMeta>> {
			let listed = self.inner.list(prefix);
			match self.list {
				true => listed
					.chain(futures::stream::once(async {
						Err(object_store::Error::NotImplemented {
							operation: "list".into(),
							implementer: "Failing".into(),
						})
					}))
					.boxed(),
				false => listed,
			}
		}

		async fn list_with_delimiter(&self, prefix: Option<&Path>) -> object_store::Result<ListResult> {
			self.inner.list_with_delimiter(prefix).await
		}

		async fn copy_opts(&self, from: &Path, to: &Path, options: CopyOptions) -> object_store::Result<()> {
			self.inner.copy_opts(from, to, options).await
		}
	}

	const TIMELINE: &str = hang::timeline::DEFAULT_NAME;

	fn ms(v: u64) -> Timestamp {
		Timestamp::from_millis(v).unwrap()
	}

	/// A millisecond track that keeps an hour of history, so enrollment sees every group.
	fn track(broadcast: &broadcast::Producer, name: &str) -> track::Producer {
		let info = track::Info::default()
			.with_timescale(Timescale::MILLI)
			.with_max_age(Duration::from_secs(3600));
		broadcast.create_track(name, info).unwrap()
	}

	/// Write one complete group with a frame at each timestamp.
	fn group(track: &track::Producer, sequence: u64, timestamps: &[u64]) {
		let mut group = track.create_group(group::Info { sequence }).unwrap();
		for &timestamp in timestamps {
			group
				.write_frame(ms(timestamp), format!("{sequence}@{timestamp}"))
				.unwrap();
		}
		group.finish().unwrap();
	}

	/// Replay every stored timeline object, returning the records still in the window.
	async fn window<S: ObjectStore>(store: &Store<S>) -> Vec<Record> {
		let entries: Vec<_> = store
			.list(&Query::segments(TIMELINE).unwrap())
			.try_collect()
			.await
			.unwrap();
		let mut segments: Vec<u64> = entries
			.into_iter()
			.map(|entry| match entry.key {
				Key::Segments { segment, .. } => segment,
				key => panic!("unexpected {key:?}"),
			})
			.collect();
		segments.sort();
		assert_eq!(
			segments,
			(0..segments.len() as u64).collect::<Vec<_>>(),
			"timeline objects are consecutive"
		);

		let config = moq_json::window::ConsumerConfig::default().with_compression(true);
		let mut decoder = moq_json::window::Decoder::<Record>::new(config);
		let mut records = BTreeMap::new();
		for segment in segments {
			let object = store.get_segments(TIMELINE, segment).await.unwrap();
			for stored in object.groups {
				let mut group = decoder.group();
				for frame in stored.frames {
					group.decode(&frame.payload).unwrap();
				}
			}
			while let Some(event) = decoder.next_event() {
				match event {
					moq_json::window::Event::Push { index, value } => {
						records.insert(index, value);
					}
					moq_json::window::Event::Pop(range) | moq_json::window::Event::Skip(range) => {
						for index in range {
							records.remove(&index);
						}
					}
					_ => unreachable!(),
				}
			}
		}
		records.into_values().collect()
	}

	/// The groups each track contributes, from the records.
	fn ranges(records: &[Record], name: &str) -> Vec<(u64, u64)> {
		records
			.iter()
			.flat_map(|record| record.tracks.get(name).into_iter().flatten())
			.map(|range| (range.start, range.end))
			.collect()
	}

	/// Every range in the records resolves to one stored object with exactly those groups.
	async fn check_objects<S: ObjectStore>(store: &Store<S>, records: &[Record]) {
		for record in records {
			for (name, ranges) in &record.tracks {
				let first = ranges.first().unwrap().start;
				let last = ranges.last().unwrap().end;
				let object = store.get_groups(name, first..=last).await.unwrap();
				let advertised: Vec<u64> = ranges.iter().flat_map(|range| range.start..=range.end).collect();
				let stored: Vec<u64> = object.groups.iter().map(|group| group.sequence).collect();
				assert_eq!(stored, advertised, "{name} segment {}", record.segment);
			}
		}
	}

	#[tokio::test]
	async fn records_selected_tracks() {
		let source = broadcast::Info::new().produce();
		let video = track(&source, "video");
		let catalog = track(&source, "catalog.json");
		let _ignored = track(&source, "ignored");

		let store = Store::new(InMemory::new(), "rec");
		let writer = Writer::new(store.clone(), source.consume(), Config::default())
			.await
			.unwrap();
		let control = writer.control();
		control.pacing_track("video").await.unwrap();
		control.track("catalog.json").await.unwrap();

		group(&catalog, 0, &[0]);
		for sequence in 0..6 {
			group(&video, sequence, &[sequence * 1000, sequence * 1000 + 500]);
		}
		video.finish().unwrap();
		catalog.finish().unwrap();
		source.finish();

		tokio::spawn(writer.run()).await.unwrap().unwrap();

		let records = window(&store).await;
		assert_eq!(records.len(), 6);
		assert_eq!(ranges(&records, "video"), (0..6).map(|s| (s, s)).collect::<Vec<_>>());
		assert_eq!(ranges(&records, "catalog.json"), vec![(0, 0)]);
		assert!(records.iter().all(|record| !record.tracks.contains_key("ignored")));
		check_objects(&store, &records).await;

		let object = store.get_groups("video", 2..=2).await.unwrap();
		assert_eq!(object.groups[0].frames[1].timestamp, 2500);
		assert_eq!(object.groups[0].frames[1].payload, "2@2500");
		assert_eq!(store.get_info("video").await.unwrap(), Info::new(0, 1000).unwrap());
		store.get_info(TIMELINE).await.unwrap();
		assert!(store.get_info("ignored").await.is_err());
	}

	#[tokio::test]
	async fn a_failed_put_omits_only_that_track() {
		let source = broadcast::Info::new().produce();
		let video = track(&source, "video");
		let audio = track(&source, "audio");

		let failing = Failing {
			inner: Arc::new(InMemory::new()),
			track: "audio",
			list: false,
		};
		let store = Store::new(failing, "rec");
		let writer = Writer::new(store.clone(), source.consume(), Config::default())
			.await
			.unwrap();
		let control = writer.control();
		control.pacing_track("video").await.unwrap();
		control.pacing_track("audio").await.unwrap();

		for sequence in 0..3 {
			group(&video, sequence, &[sequence * 1000]);
			group(&audio, sequence, &[sequence * 1000]);
		}
		video.finish().unwrap();
		audio.finish().unwrap();
		source.finish();

		writer.run().await.unwrap();

		let records = window(&store).await;
		assert_eq!(records.len(), 3);
		assert_eq!(ranges(&records, "video"), vec![(0, 0), (1, 1), (2, 2)]);
		assert!(ranges(&records, "audio").is_empty());
		check_objects(&store, &records).await;
	}

	#[tokio::test]
	async fn retention_pops_then_deletes_expired_objects() {
		let source = broadcast::Info::new().produce();
		let video = track(&source, "video");

		let store = Store::new(InMemory::new(), "rec");
		let config = Config::default().with_retention(Retention::new(Duration::from_secs(2), Duration::ZERO));
		let writer = Writer::new(store.clone(), source.consume(), config).await.unwrap();
		writer.control().pacing_track("video").await.unwrap();

		for sequence in 0..6 {
			group(&video, sequence, &[sequence * 1000, sequence * 1000 + 500]);
		}
		video.finish().unwrap();
		source.finish();

		writer.run().await.unwrap();

		// Segments 3 and 4 hold two seconds; the partial final segment 5 is always kept.
		let records = window(&store).await;
		assert_eq!(
			records.iter().map(|record| record.segment).collect::<Vec<_>>(),
			vec![3, 4, 5]
		);
		check_objects(&store, &records).await;

		let stored: HashSet<_> = store
			.list(&Query::groups("video").unwrap())
			.map_ok(|entry| entry.key)
			.try_collect()
			.await
			.unwrap();
		let expected: HashSet<_> = (3..6).map(|s| Key::groups("video", s..=s).unwrap()).collect();
		assert_eq!(stored, expected);
	}

	#[tokio::test]
	async fn commands_are_refused_once_the_recording_ends() {
		let source = broadcast::Info::new().produce();
		let video = track(&source, "video");

		let store = Store::new(InMemory::new(), "rec");
		// A long grace keeps `run` waiting on deletions after the timeline finishes.
		let config =
			Config::default().with_retention(Retention::new(Duration::from_secs(2), Duration::from_secs(3600)));
		let writer = Writer::new(store.clone(), source.consume(), config).await.unwrap();
		let control = writer.control();
		control.pacing_track("video").await.unwrap();

		for sequence in 0..6 {
			group(&video, sequence, &[sequence * 1000]);
		}
		video.finish().unwrap();
		source.finish();

		let run = tokio::spawn(writer.run());
		while window(&store).await.last().map(|record| record.segment) != Some(5) {
			tokio::time::sleep(Duration::from_millis(10)).await;
		}
		assert!(!run.is_finished());
		assert_eq!(control.remove("video"), Err(Error::Closed));
		run.abort();
	}

	#[tokio::test]
	async fn decreasing_arrivals_are_refused() {
		let source = broadcast::Info::new().produce();
		let video = track(&source, "video");

		let store = Store::new(InMemory::new(), "rec");
		let writer = Writer::new(store.clone(), source.consume(), Config::default())
			.await
			.unwrap();
		writer.control().pacing_track("video").await.unwrap();

		group(&video, 0, &[0]);
		group(&video, 2, &[1000]);
		group(&video, 1, &[500]);
		group(&video, 3, &[2000]);
		video.finish().unwrap();
		source.finish();

		writer.run().await.unwrap();

		let records = window(&store).await;
		assert_eq!(ranges(&records, "video"), vec![(0, 0), (2, 2), (3, 3)]);
		check_objects(&store, &records).await;
	}

	#[tokio::test]
	async fn an_unrepresentable_group_fails_the_recording() {
		let source = broadcast::Info::new().produce();
		let video = track(&source, "video");

		let store = Store::new(InMemory::new(), "rec");
		let writer = Writer::new(store, source.consume(), Config::default()).await.unwrap();
		writer.control().pacing_track("video").await.unwrap();

		group(&video, 0, &[0]);
		// One past the recording's largest group ID.
		group(&video, 1 << 53, &[1000]);
		video.finish().unwrap();
		source.finish();

		assert_eq!(
			writer.run().await,
			Err(Error::Source(format!(
				"track video group {}: {}",
				1u64 << 53,
				Error::Id(1 << 53)
			)))
		);
	}

	#[tokio::test]
	async fn a_decreasing_timestamp_fails_the_recording() {
		let source = broadcast::Info::new().produce();
		let video = track(&source, "video");

		let store = Store::new(InMemory::new(), "rec");
		let writer = Writer::new(store, source.consume(), Config::default()).await.unwrap();
		writer.control().pacing_track("video").await.unwrap();

		group(&video, 0, &[1000]);
		group(&video, 1, &[500]);
		video.finish().unwrap();
		source.finish();

		assert_eq!(
			writer.run().await,
			Err(Error::Source("track video group 1: timestamp 500 precedes 1000".into()))
		);
	}

	#[tokio::test]
	async fn removing_a_stalled_pacing_track_releases_the_recording() {
		let source = broadcast::Info::new().produce();
		let video = track(&source, "video");
		let audio = track(&source, "audio");

		let store = Store::new(InMemory::new(), "rec");
		let writer = Writer::new(store.clone(), source.consume(), Config::default())
			.await
			.unwrap();
		let control = writer.control();
		control.pacing_track("video").await.unwrap();
		control.pacing_track("audio").await.unwrap();
		let run = tokio::spawn(writer.run());

		group(&audio, 0, &[0]);
		// The audio group never completes, so audio never reports past it.
		let mut stalled = audio.create_group(group::Info { sequence: 1 }).unwrap();
		stalled.write_frame(ms(1000), "stalled").unwrap();
		for sequence in 0..4 {
			group(&video, sequence, &[sequence * 1000]);
		}
		video.finish().unwrap();
		source.finish();

		tokio::time::sleep(Duration::from_millis(50)).await;
		assert!(!run.is_finished(), "a stalled pacing track holds the recording open");
		assert!(window(&store).await.is_empty(), "and holds every segment back");

		control.remove("audio").unwrap();
		run.await.unwrap().unwrap();

		let records = window(&store).await;
		assert_eq!(ranges(&records, "video"), vec![(0, 0), (1, 1), (2, 2), (3, 3)]);
		assert_eq!(
			ranges(&records, "audio"),
			vec![(0, 0)],
			"the incomplete group is dropped"
		);
		check_objects(&store, &records).await;
		drop(stalled);
	}

	#[tokio::test]
	async fn enrollment_is_refused_for_duplicates_and_the_timeline() {
		let source = broadcast::Info::new().produce();
		let _video = track(&source, "video");

		let store = Store::new(InMemory::new(), "rec");
		let writer = Writer::new(store, source.consume(), Config::default()).await.unwrap();
		let control = writer.control();
		control.pacing_track("video").await.unwrap();
		assert_eq!(control.track("video").await, Err(Error::Enrolled("video".into())));
		assert_eq!(control.track(TIMELINE).await, Err(Error::Enrolled(TIMELINE.into())));
	}

	#[tokio::test]
	async fn a_conflicting_info_fails_enrollment() {
		let source = broadcast::Info::new().produce();
		let _video = track(&source, "video");

		let store = Store::new(InMemory::new(), "rec");
		store.put_info("video", &Info::new(7, 1000).unwrap()).await.unwrap();
		let writer = Writer::new(store, source.consume(), Config::default()).await.unwrap();
		let control = writer.control();
		assert_eq!(
			control.pacing_track("video").await,
			Err(Error::Priority {
				existing: 7,
				intended: 0
			})
		);
	}

	/// Record `video` groups `sequences`, one per second, until the source ends.
	async fn record<S: ObjectStore + Clone>(store: &Store<S>, config: Config, sequences: std::ops::Range<u64>) {
		let source = broadcast::Info::new().produce();
		let video = track(&source, "video");
		let writer = Writer::new(store.clone(), source.consume(), config).await.unwrap();
		writer.control().pacing_track("video").await.unwrap();
		for sequence in sequences {
			group(&video, sequence, &[sequence * 1000, sequence * 1000 + 500]);
		}
		video.finish().unwrap();
		source.finish();
		writer.run().await.unwrap();
	}

	/// Every stored group object, across all tracks.
	async fn stored_groups<S: ObjectStore>(store: &Store<S>) -> HashSet<Key> {
		store
			.list(&Query::new())
			.try_filter_map(|entry| async move { Ok(matches!(entry.key, Key::Groups { .. }).then_some(entry.key)) })
			.try_collect()
			.await
			.unwrap()
	}

	/// The group objects the retained records advertise.
	fn referenced(records: &[Record]) -> HashSet<Key> {
		records
			.iter()
			.flat_map(|record| &record.tracks)
			.map(|(name, ranges)| Key::groups(name.clone(), ranges[0].start..=ranges.last().unwrap().end).unwrap())
			.collect()
	}

	fn orphan(sequence: u64) -> Object {
		Object {
			groups: vec![Group {
				sequence,
				frames: vec![Frame {
					timestamp: sequence * 1000,
					payload: "orphan".into(),
				}],
			}],
		}
	}

	#[tokio::test]
	async fn a_restarted_writer_resumes_the_recording() {
		let store = Store::new(InMemory::new(), "rec");
		record(&store, Config::default(), 0..3).await;
		// The source's cache replays groups the recording already holds.
		record(&store, Config::default(), 0..6).await;

		let records = window(&store).await;
		assert_eq!(
			records.iter().map(|record| record.segment).collect::<Vec<_>>(),
			(0..6).collect::<Vec<_>>()
		);
		assert_eq!(ranges(&records, "video"), (0..6).map(|s| (s, s)).collect::<Vec<_>>());
		check_objects(&store, &records).await;

		// The resumed timeline groups continue the stored numbering.
		let mut sequences = Vec::new();
		for segment in 0..6 {
			let object = store.get_segments(TIMELINE, segment).await.unwrap();
			sequences.extend(object.groups.iter().map(|group| group.sequence));
		}
		assert!(sequences.windows(2).all(|pair| pair[0] < pair[1]), "{sequences:?}");
	}

	#[tokio::test]
	async fn a_restarted_dvr_deletes_unreferenced_groups() {
		let store = Store::new(InMemory::new(), "rec");
		let config = Config::default().with_retention(Retention::new(Duration::from_secs(2), Duration::ZERO));
		record(&store, config.clone(), 0..6).await;

		// An interrupted expiration, an uncommitted upload, and a track no retained record names.
		store.put_groups("video", &orphan(1)).await.unwrap();
		store.put_groups("video", &orphan(7)).await.unwrap();
		store.put_groups("audio", &orphan(0)).await.unwrap();

		// Groups at or below the uncommitted upload are refused, so nothing overlaps it.
		record(&store, config, 6..10).await;

		let records = window(&store).await;
		assert_eq!(
			records.iter().map(|record| record.segment).collect::<Vec<_>>(),
			vec![5, 6, 7]
		);
		assert_eq!(ranges(&records, "video"), vec![(5, 5), (8, 8), (9, 9)]);
		check_objects(&store, &records).await;
		assert_eq!(stored_groups(&store).await, referenced(&records));
		store.get_info("video").await.unwrap();
		store.get_info(TIMELINE).await.unwrap();
		store.get_segments(TIMELINE, 0).await.unwrap();
	}

	#[tokio::test]
	async fn a_failed_recovery_deletes_nothing() {
		let inner = Arc::new(InMemory::new());
		let store = Store::new(inner.clone(), "rec");
		let config = Config::default().with_retention(Retention::new(Duration::from_secs(2), Duration::ZERO));
		record(&store, config.clone(), 0..6).await;
		store.put_groups("video", &orphan(1)).await.unwrap();
		let before = stored_groups(&store).await;

		let failing = Failing {
			inner: inner.clone(),
			track: "",
			list: true,
		};
		let source = broadcast::Info::new().produce();
		let result = Writer::new(Store::new(failing, "rec"), source.consume(), config.clone()).await;
		assert!(matches!(result, Err(Error::Store(_))));

		// A missing timeline object leaves the retained window unrecoverable.
		store.delete(&Key::segments(TIMELINE, 3).unwrap()).await.unwrap();
		let result = Writer::new(store.clone(), source.consume(), config).await;
		assert!(matches!(result, Err(Error::Timeline(_))));

		assert_eq!(stored_groups(&store).await, before);
	}

	#[tokio::test]
	async fn a_dvr_window_longer_than_one_checkpoint_is_recovered() {
		let store = Store::new(InMemory::new(), "rec");
		let config = Config::default().with_retention(Retention::new(Duration::from_secs(280), Duration::ZERO));
		record(&store, config, 0..300).await;

		let recovery = recover(&store, TIMELINE, true).await.unwrap();
		let checkpoint = recovery.checkpoint.unwrap();
		let records = window(&store).await;
		assert!(records.len() > 256, "the window outgrows one checkpoint");
		assert_eq!(checkpoint.records, records);
		assert_eq!(checkpoint.range.end, 300);
		assert!(recovery.orphans.is_empty());
		assert_eq!(recovery.floors["video"], 299);
	}

	#[tokio::test]
	async fn a_timeline_that_does_not_end_at_the_next_segment_fails_recovery() {
		let store = Store::new(InMemory::new(), "rec");
		record(&store, Config::default(), 0..6).await;
		let before = stored_groups(&store).await;

		// `segments/5` still decodes, but it restates an earlier window, so resuming
		// would write the next segment on top of it.
		let older = store.get_segments(TIMELINE, 0).await.unwrap();
		store.delete(&Key::segments(TIMELINE, 5).unwrap()).await.unwrap();
		store.put_segments(TIMELINE, 5, &older).await.unwrap();

		let source = broadcast::Info::new().produce();
		match Writer::new(store.clone(), source.consume(), Config::default()).await {
			Err(Error::Timeline(message)) => assert!(message.contains("not segment 6"), "{message}"),
			Err(err) => panic!("expected a timeline error, got {err}"),
			Ok(_) => panic!("expected recovery to fail"),
		}
		assert_eq!(stored_groups(&store).await, before);
	}
}
