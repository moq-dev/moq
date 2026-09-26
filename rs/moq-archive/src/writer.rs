//! Record explicitly selected tracks of a [`broadcast::Consumer`] into a [`Store`].
//!
//! The application enrolls each track through a [`Control`]; the writer never parses a catalog.
//! Every track has its own timeline: frames feed the track's segmenter as they arrive, and for
//! each closed record the writer stores the track's object, commits the record through the
//! track's own timeline encoder, and stores that record's timeline groups before committing the
//! track's next one. A timeline therefore only advertises durable objects, and no track waits for
//! another.
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
//! control.track("video").await?;
//! control.track("audio").await?;
//! control.sparse("catalog.json").await?;
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
use hang::timeline::{Position, Record};
use moq_mux::timeline::{self, Segmenter};
use moq_net::{Timescale, Timestamp, broadcast, group, track};
use object_store::ObjectStore;
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;

use crate::recover::{Resume, recover};
use crate::segment::{Frame, Group, Object};
use crate::{Error, Info, Key, Result, Store};

/// Subscribers ask for every cached group; the publisher clamps this to its own max age.
const REPLAY: Duration = Duration::from_secs(u32::MAX as u64);

/// How a [`Writer`] cuts and retains its recording.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Config {
	/// How each track's records are cut.
	pub timeline: timeline::Config,
	/// Expire old records (a DVR), or keep everything when `None` (an archive).
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
	/// Keep at least this much of each track's content, measured from its timeline records.
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
	commands: mpsc::UnboundedReceiver<Command<S>>,
	/// Deadlines for deleting expired or orphaned objects, oldest first.
	deletions: VecDeque<(Instant, Vec<Key>)>,
}

/// Enrolls, cuts, and removes tracks on a [`Writer`].
pub struct Control<S> {
	shared: Arc<Shared<S>>,
	commands: mpsc::UnboundedSender<Command<S>>,
}

impl<S> Clone for Control<S> {
	fn clone(&self) -> Self {
		Self {
			shared: self.shared.clone(),
			commands: self.commands.clone(),
		}
	}
}

struct Shared<S> {
	store: Store<S>,
	source: broadcast::Consumer,
	config: timeline::Config,
	retention: Option<Retention>,
	/// Owns the recording's timeline tracks, read back to store their groups.
	timelines: broadcast::Producer,
	/// Each track's retained timeline from a resumed recording, taken when it enrolls again.
	recovered: Mutex<HashMap<String, Resume>>,
	/// Every name ever enrolled. A name is never reused, so its records stay consecutive.
	enrolled: Mutex<HashSet<String>>,
	/// Enrollments between their first check and their command, so the recording does not end
	/// under one that is about to arrive.
	enrolling: watch::Sender<usize>,
}

enum Command<S> {
	Enroll(Box<Track<S>>),
	Cut(Timestamp),
	Remove(String),
}

impl<S: ObjectStore> Writer<S> {
	/// Start a recording under `store`'s prefix, reading tracks from `source`, or resume the one
	/// already there.
	///
	/// Resuming replays each track's retained timeline, and a re-enrolled track continues at its
	/// next record, refusing anything at or before the end of its newest committed one. A source
	/// whose group sequences restarted therefore needs a new prefix. Objects a crash left before
	/// their commit are deleted now, since the resumed records reuse their keys. A DVR also
	/// deletes, one grace period after recovery, every object its retained records do not
	/// reference, such as interrupted expirations. The writer must own the prefix exclusively.
	/// Fails, deleting nothing, when the recording cannot be listed or a timeline cannot be
	/// replayed.
	pub async fn new(store: Store<S>, source: broadcast::Consumer, config: Config) -> Result<Self> {
		let recovery = recover(&store, config.retention.is_some()).await?;
		for key in &recovery.uncommitted {
			store.delete(key).await?;
		}

		let mut deletions = VecDeque::new();
		if let Some(retention) = &config.retention
			&& !recovery.orphans.is_empty()
		{
			deletions.push_back((Instant::now() + retention.grace, recovery.orphans));
		}

		let shared = Arc::new(Shared {
			store,
			source,
			config: config.timeline,
			retention: config.retention,
			timelines: broadcast::Info::new().produce(),
			recovered: Mutex::new(recovery.tracks),
			enrolled: Mutex::new(HashSet::new()),
			enrolling: watch::Sender::new(0),
		});
		let (commands, receiver) = mpsc::unbounded_channel();
		Ok(Self {
			control: Control { shared, commands },
			commands: receiver,
			deletions,
		})
	}

	/// A handle that enrolls, cuts, and removes tracks, before or during [`run`](Self::run).
	pub fn control(&self) -> Control<S> {
		self.control.clone()
	}

	/// Record until the source broadcast closes and every enrolled track ends.
	///
	/// Then flush each track's final record and finish its timeline. Fails when a timeline cannot
	/// be committed or stored, or an enrolled track delivers a frame the recording cannot
	/// represent: the recording stops at each track's last durable timeline object. Returns the
	/// source's error, after finishing, when the broadcast aborted.
	pub async fn run(self) -> Result<()> {
		let Self {
			control,
			mut commands,
			mut deletions,
		} = self;
		let shared = control.shared.clone();
		let source = shared.source.clone();
		let grace = shared.retention.as_ref().map(|retention| retention.grace);
		let mut enrolling = shared.enrolling.subscribe();
		drop(control);

		let mut tracks: HashMap<String, Box<Track<S>>> = HashMap::new();
		let mut reads = FuturesUnordered::new();
		let mut commits: FuturesUnordered<Commit<S>> = FuturesUnordered::new();
		let mut closed = false;
		// Cleared once the channel yields nothing more: every sender dropped, or it closed and drained.
		let mut accepting = true;

		loop {
			for track in tracks.values_mut() {
				if let Some(commit) = track.commit()? {
					commits.push(commit);
				}
			}
			tracks.retain(|_, track| !track.finish());

			let idle = closed && tracks.is_empty() && commits.is_empty();
			if idle && *enrolling.borrow_and_update() == 0 {
				// Refuse late commands, so an enrollment racing the end fails instead of vanishing.
				commands.close();
				if !accepting {
					break;
				}
			}

			let deadline = deletions.front().map(|(deadline, _)| *deadline);
			tokio::select! {
				biased;
				command = commands.recv(), if accepting => match command {
					None => accepting = false,
					Some(Command::Enroll(mut track)) => {
						let subscriber = track.subscriber.take().expect("an enrolling track carries its subscriber");
						reads.push(guard(track.cancelled.clone(), recv(track.name.clone(), subscriber)).boxed());
						tracks.insert(track.name.clone(), track);
					}
					Some(Command::Cut(pts)) => {
						for track in tracks.values_mut() {
							track.segmenter.cut(pts);
						}
					}
					Some(Command::Remove(name)) => {
						if let Some(track) = tracks.get_mut(&name) {
							track.remove();
						}
					}
				},
				Some(read) = reads.next(), if !reads.is_empty() => {
					handle(read, &mut tracks, &mut reads)?;
				}
				Some((name, committer, result)) = commits.next(), if !commits.is_empty() => {
					if let Some(track) = tracks.get_mut(&name) {
						track.committer = Some(committer);
					}
					let expired = result?;
					if let Some(grace) = grace && !expired.is_empty() {
						deletions.push_back((Instant::now() + grace, expired));
					}
				}
				_ = source.closed(), if !closed => closed = true,
				_ = enrolling.changed(), if idle && accepting => {}
				_ = tokio::time::sleep_until(deadline.unwrap_or_else(Instant::now)), if deadline.is_some() => {
					let (_, keys) = deletions.pop_front().unwrap();
					delete(&shared.store, keys).await;
				}
			}
		}

		// Complete expirations already committed to the timelines; nothing new expires after the
		// final records.
		for (deadline, keys) in deletions {
			tokio::time::sleep_until(deadline).await;
			delete(&shared.store, keys).await;
		}

		// A broadcast ends without a cause unless it was aborted.
		match source.closed().await {
			moq_net::Error::Dropped => Ok(()),
			err => Err(source_error(err)),
		}
	}
}

impl<S: ObjectStore> Control<S> {
	/// Enroll the media track `name`, its records cut by the configured durations.
	///
	/// Subscribes to the track and creates its `.info` and its timeline's before accepting any
	/// group. Fails when the name was already enrolled, names a timeline, or `.info` conflicts.
	pub async fn track(&self, name: &str) -> Result<()> {
		self.enroll(name, self.shared.config.clone()).await
	}

	/// Enroll the sparse track `name`, such as a catalog or metadata track: every group is its own
	/// record, stored as soon as it finishes.
	pub async fn sparse(&self, name: &str) -> Result<()> {
		let config = self.shared.config.clone().with_duration_min(Duration::ZERO);
		self.enroll(name, config).await
	}

	/// Declare a boundary at `pts` on every enrolled track; see [`Segmenter::cut`].
	pub fn cut(&self, pts: Timestamp) -> Result<()> {
		self.send(Command::Cut(pts))
	}

	/// Stop recording `name`, storing what already arrived and dropping groups that could not be
	/// indexed yet. The name cannot be enrolled again.
	pub fn remove(&self, name: &str) -> Result<()> {
		self.send(Command::Remove(name.to_string()))
	}

	async fn enroll(&self, name: &str, config: timeline::Config) -> Result<()> {
		if name.ends_with(hang::timeline::SUFFIX) || !self.shared.enrolled.lock().unwrap().insert(name.to_string()) {
			return Err(Error::Enrolled(name.to_string()));
		}
		self.shared.enrolling.send_modify(|count| *count += 1);
		let result = self.subscribe(name, config).await;
		self.shared.enrolling.send_modify(|count| *count -= 1);
		if result.is_err() {
			self.shared.enrolled.lock().unwrap().remove(name);
		}
		result
	}

	async fn subscribe(&self, name: &str, config: timeline::Config) -> Result<()> {
		let shared = &self.shared;
		let replay = track::Subscription::default().with_max_age(REPLAY);
		let subscriber = shared
			.source
			.track(name)
			.map_err(source_error)?
			.subscribe(replay.clone())
			.await
			.map_err(source_error)?;
		let timescale = subscriber.info().timescale;
		let info = Info::new(subscriber.info().priority, timescale.as_u64())?;
		shared.store.put_info(name, &info).await?;

		let timeline = hang::timeline::default_name(name);
		let output = shared
			.timelines
			.create_track(timeline.as_str(), timeline::Producer::info())
			.map_err(timeline_error_net)?;
		let groups = shared
			.timelines
			.consume()
			.track(&timeline)
			.map_err(timeline_error_net)?
			.subscribe(replay)
			.await
			.map_err(timeline_error_net)?
			.ordered();
		let info = Info::new(groups.info().priority, groups.info().timescale.as_u64())?;
		shared.store.put_info(&timeline, &info).await?;

		let resume = shared.recovered.lock().unwrap().remove(name);
		let (output, segmenter, window, offset, floor) = match resume {
			Some(resume) => {
				let floor = resume.floor();
				let output = timeline::Producer::resume(output, &resume.checkpoint).map_err(timeline_error)?;
				let segmenter = Segmenter::new(config).with_sequence(resume.checkpoint.range.end);
				let window = resume.checkpoint.records.into();
				(output, segmenter, window, resume.sequence, floor)
			}
			None => (
				timeline::Producer::new(output),
				Segmenter::new(config),
				VecDeque::new(),
				0,
				None,
			),
		};

		let (cancel, cancelled) = watch::channel(());
		let track = Track {
			name: name.to_string(),
			timescale,
			segmenter,
			floor,
			largest: None,
			reported: None,
			accepted: BTreeMap::new(),
			frames: VecDeque::new(),
			records: VecDeque::new(),
			subscriber: Some(Box::new(subscriber)),
			subscribed: true,
			closed: false,
			committer: Some(Committer {
				shared: shared.clone(),
				name: name.to_string(),
				timeline,
				output,
				groups,
				window,
				offset,
			}),
			cancel: Some(cancel),
			cancelled,
		};
		self.send(Command::Enroll(Box::new(track)))
	}

	fn send(&self, command: Command<S>) -> Result<()> {
		self.commands.send(command).map_err(|_| Error::Closed)
	}
}

/// A group accepted from the source, not yet fully reported.
#[derive(Default)]
struct Incoming {
	/// The index of the next frame to arrive.
	next: u64,
	/// Frames that arrived but were not reported, with their indices.
	pending: Vec<(u64, Frame)>,
	/// No more frames will arrive.
	finished: bool,
}

/// One enrolled track: its source reads, its segmenter, and its committer.
struct Track<S> {
	name: String,
	timescale: Timescale,
	segmenter: Segmenter,
	/// A resumed track's committed end; earlier content is already stored.
	floor: Option<Position>,
	/// The newest accepted group; later groups must exceed it.
	largest: Option<u64>,
	/// The first-frame timestamp of the newest reported group; later groups must not precede it.
	reported: Option<u64>,
	/// Accepted groups in sequence order. Reported front first, so a later group's frames wait
	/// for every earlier accepted group to finish.
	accepted: BTreeMap<u64, Incoming>,
	/// Reported frames waiting for the record that holds them.
	frames: VecDeque<(Position, Frame)>,
	/// Closed records waiting to be committed, oldest first.
	records: VecDeque<Record>,
	/// Moved into the first read once enrolled.
	subscriber: Option<Box<track::Subscriber>>,
	/// The subscription is still delivering groups.
	subscribed: bool,
	/// The segmenter was closed: every remaining record is queued.
	closed: bool,
	/// `None` while a commit is in flight.
	committer: Option<Committer<S>>,
	/// Dropping the sender cancels every read for this track.
	cancel: Option<watch::Sender<()>>,
	cancelled: watch::Receiver<()>,
}

impl<S: ObjectStore> Track<S> {
	/// Report every frame the segmenter can place: the front group's arrivals, then each later
	/// group once the ones before it finish.
	///
	/// Fails on a group that starts before the previous one, which the timeline cannot place.
	fn report(&mut self) -> Result<()> {
		while let Some(mut entry) = self.accepted.first_entry() {
			let sequence = *entry.key();
			let incoming = entry.get_mut();
			for (index, frame) in incoming.pending.drain(..) {
				if index == 0 {
					if let Some(reported) = self.reported
						&& frame.timestamp < reported
					{
						return Err(malformed(
							&self.name,
							sequence,
							format!("timestamp {} precedes {reported}", frame.timestamp),
						));
					}
					self.reported = Some(frame.timestamp);
				}
				// Frame timestamps were validated while reading, so this conversion succeeds.
				let pts = Timestamp::new(frame.timestamp, self.timescale).map_err(|_| Error::Id(frame.timestamp))?;
				let position = Position::new(sequence, index);
				self.segmenter.frame(position, pts, index == 0);
				self.frames.push_back((position, frame));
			}
			if !incoming.finished {
				break;
			}
			entry.remove();
			self.segmenter.finish_group(sequence);
		}
		Ok(())
	}

	/// Stop reading: drop the groups that were never reported and close the segmenter.
	fn remove(&mut self) {
		self.cancel = None;
		self.subscribed = false;
		self.accepted.clear();
	}

	/// Start committing the next closed record, if the committer is idle.
	fn commit(&mut self) -> Result<Option<Commit<S>>> {
		if !self.subscribed && self.accepted.is_empty() && !self.closed {
			self.segmenter.close();
			self.closed = true;
		}
		self.records.extend(self.segmenter.by_ref());
		if self.committer.is_none() {
			return Ok(None);
		}
		let Some(record) = self.records.pop_front() else {
			return Ok(None);
		};

		let mut groups: Vec<Group> = Vec::new();
		while let Some((position, _)) = self.frames.front()
			&& *position < record.end
		{
			let (position, frame) = self.frames.pop_front().expect("checked above");
			match groups.last_mut() {
				Some(group) if group.sequence == position.group => group.frames.push(frame),
				_ => groups.push(Group {
					sequence: position.group,
					frames: vec![frame],
				}),
			}
		}
		let object = Object {
			frame_start: record.start.frame,
			groups,
		};
		object.check_span(record.start, record.end)?;

		let committer = self.committer.take().expect("checked above");
		Ok(Some(committer.commit(record, object).boxed()))
	}

	/// Finish the timeline once the track ended and every record is durable, returning whether the
	/// track is done.
	fn finish(&mut self) -> bool {
		if !self.closed || !self.records.is_empty() {
			return false;
		}
		let Some(committer) = self.committer.as_mut() else {
			return false;
		};
		if let Err(err) = committer.output.finish() {
			tracing::warn!(track = %self.name, %err, "failed to finish the timeline");
		}
		true
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
	Frame {
		name: String,
		group: Box<group::Consumer>,
		result: moq_net::Result<Option<moq_net::frame::Frame>>,
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

async fn frame(name: String, mut group: Box<group::Consumer>) -> Read {
	let result = group.read_frame().await;
	Read::Frame { name, group, result }
}

/// Resolve to [`Read::Cancelled`] once the track stops reading.
async fn guard(mut cancelled: watch::Receiver<()>, read: impl Future<Output = Read>) -> Read {
	tokio::select! {
		read = read => read,
		_ = cancelled.changed() => Read::Cancelled,
	}
}

/// Fails on malformed source input; a group the network aborted keeps the frames that arrived.
fn handle<S: ObjectStore>(
	read: Read,
	tracks: &mut HashMap<String, Box<Track<S>>>,
	reads: &mut FuturesUnordered<BoxFuture<'static, Read>>,
) -> Result<()> {
	match read {
		Read::Cancelled => Ok(()),
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
					let stored = track.floor.is_some_and(|floor| Position::group(sequence + 1) <= floor);
					if track.largest.is_some_and(|largest| sequence <= largest) || stored {
						tracing::debug!(track = %name, sequence, "refusing a duplicate, decreasing, or stored group");
					} else {
						crate::path::check_id(sequence).map_err(|err| malformed(&name, sequence, err))?;
						track.largest = Some(sequence);
						track.accepted.insert(sequence, Incoming::default());
						reads.push(guard(track.cancelled.clone(), frame(name.clone(), Box::new(group))).boxed());
					}
					reads.push(guard(track.cancelled.clone(), recv(name, subscriber)).boxed());
				}
				Ok(None) => track.subscribed = false,
				Err(err) => {
					tracing::warn!(track = %name, %err, "track ended");
					track.subscribed = false;
				}
			}
			Ok(())
		}
		Read::Frame { name, group, result } => {
			let Some(track) = tracks.get_mut(&name) else {
				return Ok(());
			};
			let sequence = group.sequence;
			let Some(incoming) = track.accepted.get_mut(&sequence) else {
				return Ok(());
			};
			match result {
				Ok(Some(frame)) => {
					let index = incoming.next;
					incoming.next += 1;
					let timestamp = frame
						.timestamp
						.convert(track.timescale)
						.map_err(|_| malformed(&name, sequence, Error::Overflow))?
						.value();
					crate::path::check_id(timestamp).map_err(|err| malformed(&name, sequence, err))?;
					// A resumed track already stored the head of the group it stopped inside.
					if track.floor.is_none_or(|floor| Position::new(sequence, index) >= floor) {
						let frame = Frame {
							timestamp,
							payload: frame.payload,
						};
						incoming.pending.push((index, frame));
					}
					reads.push(guard(track.cancelled.clone(), self::frame(name, group)).boxed());
				}
				Ok(None) => incoming.finished = true,
				Err(err) => {
					tracing::warn!(track = %name, sequence, %err, "keeping the frames of an incomplete group");
					incoming.finished = true;
				}
			}
			track.report()
		}
	}
}

/// An in-flight record commit, returning the track, its committer, and the expired objects.
type Commit<S> = BoxFuture<'static, (String, Committer<S>, Result<Vec<Key>>)>;

/// Commits one track's records in order: the object, the record, retention, then the timeline
/// object.
struct Committer<S> {
	shared: Arc<Shared<S>>,
	name: String,
	/// The track's timeline track.
	timeline: String,
	output: timeline::Producer,
	/// The timeline track read back, to store its complete groups.
	groups: track::Ordered,
	/// Committed records still in the timeline window, oldest first.
	window: VecDeque<Record>,
	/// Added to the timeline track's group sequences, continuing a resumed recording's numbering.
	offset: u64,
}

impl<S: ObjectStore> Committer<S> {
	/// Commit `record` with its `object`.
	async fn commit(mut self, record: Record, object: Object) -> (String, Self, Result<Vec<Key>>) {
		let result = self.commit_inner(record, object).await;
		(self.name.clone(), self, result)
	}

	async fn commit_inner(&mut self, mut record: Record, object: Object) -> Result<Vec<Key>> {
		let shared = self.shared.clone();
		let store = &shared.store;

		// Number by the window rather than the segmenter, so a record dropped below leaves no hole.
		record.sequence = self.output.range().end;
		if let Err(err) = store.put_segments(&self.name, record.sequence, &object).await {
			// The timeline only advertises durable objects, so this span is lost. The key may have
			// landed anyway; clear it so the next record can take it.
			tracing::warn!(track = %self.name, sequence = record.sequence, %err, "dropping a record that was not stored");
			let _ = store.delete(&Key::segments(self.name.clone(), record.sequence)?).await;
			return Ok(Vec::new());
		}

		self.output.push(&record).map_err(timeline_error)?;
		let pts = Timestamp::new(record.pts, TIMESCALE).map_err(|_| Error::Id(record.pts))?;
		self.window.push_back(record.clone());

		let expired = self.expire();
		if !expired.is_empty() {
			self.output.pop(expired.len() as u64).map_err(timeline_error)?;
		}
		self.output.flush().map_err(timeline_error)?;

		let object = self.read_timeline(pts)?;
		store.put_segments(&self.timeline, record.sequence, &object).await?;

		expired
			.iter()
			.map(|record| Key::segments(self.name.clone(), record.sequence))
			.collect()
	}

	/// Pop the oldest records while the rest still cover the retention window. The newest record
	/// always stays, such as a catalog that never changes.
	fn expire(&mut self) -> Vec<Record> {
		let Some(retention) = self.shared.retention.as_ref().map(|retention| retention.window) else {
			return Vec::new();
		};
		let target = (retention.as_micros() * TIMESCALE.as_u64() as u128).div_ceil(1_000_000);
		let mut expired = Vec::new();
		while self.window.len() > 1 {
			let newest = self.window.back().expect("non-empty");
			let end = newest.pts as u128 + newest.duration as u128;
			if end.saturating_sub(self.window[1].pts as u128) < target {
				break;
			}
			expired.push(self.window.pop_front().expect("non-empty"));
		}
		expired
	}

	/// Collect the timeline groups completed since the last record, stamped at `pts`.
	///
	/// The live timeline track stamps frames with the wall clock; storing the record's content time
	/// instead keeps a recording's bytes a function of its content alone.
	fn read_timeline(&mut self, pts: Timestamp) -> Result<Object> {
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
					Poll::Ready(Ok(Some(frame))) => {
						let timestamp = pts.convert(timescale).map_err(|_| Error::Overflow)?.value();
						frames.push(Frame {
							timestamp: crate::path::check_id(timestamp)?,
							payload: frame.payload,
						});
					}
					Poll::Ready(Ok(None)) => break,
					Poll::Ready(Err(err)) => return Err(timeline_error_net(err)),
					// Flushing closed every group, so an open one is a bug.
					Poll::Pending => return Err(Error::Timeline("timeline group is still open".into())),
				}
			}
			let sequence = group.sequence.checked_add(self.offset).ok_or(Error::Overflow)?;
			groups.push(Group {
				sequence: crate::path::check_id(sequence)?,
				frames,
			});
		}
		Ok(Object::new(groups))
	}
}

/// The timescale of record `pts` and `duration`, as the catalog section advertises it.
const TIMESCALE: Timescale = Timescale::MILLI;

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
	use object_store::memory::InMemory;

	use super::*;
	use crate::mock::Mock;
	use crate::store::list::Query;

	fn ms(v: u64) -> Timestamp {
		Timestamp::from_millis(v).unwrap()
	}

	fn timeline(track: &str) -> String {
		hang::timeline::default_name(track)
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

	/// Replay every stored timeline object of `track`, returning the records still in its window.
	async fn window<S: ObjectStore>(store: &Store<S>, track: &str) -> Vec<Record> {
		let timeline = timeline(track);
		let entries: Vec<_> = store
			.list(&Query::segments(&timeline).unwrap())
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
		if let (Some(first), Some(last)) = (segments.first(), segments.last()) {
			assert_eq!(
				segments,
				(*first..=*last).collect::<Vec<_>>(),
				"timeline objects are consecutive"
			);
		}

		let config = moq_json::window::ConsumerConfig::default().with_compression(true);
		let mut decoder = moq_json::window::Decoder::<Record>::new(config);
		let mut records = BTreeMap::new();
		for segment in segments {
			let object = store.get_segments(&timeline, segment).await.unwrap();
			for stored in object.groups {
				let mut group = decoder.group();
				for frame in stored.frames {
					group.decode(&frame.payload).unwrap();
				}
			}
			while let Some(event) = decoder.next_event() {
				match event {
					moq_json::window::Event::Push { index, value } => {
						assert_eq!(index, value.sequence);
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

	/// Each record's groups, as `(first, last)`.
	fn groups(records: &[Record]) -> Vec<(u64, u64)> {
		records
			.iter()
			.map(|record| (*record.groups().start(), *record.groups().end()))
			.collect()
	}

	fn sequences(records: &[Record]) -> Vec<u64> {
		records.iter().map(|record| record.sequence).collect()
	}

	/// Every record resolves to one stored object holding exactly its frames.
	async fn check_objects<S: ObjectStore>(store: &Store<S>, track: &str, records: &[Record]) {
		for record in records {
			let object = store.get_segments(track, record.sequence).await.unwrap();
			object.check_span(record.start, record.end).unwrap();
		}
	}

	/// Every stored media object, across all tracks.
	async fn stored<S: ObjectStore>(store: &Store<S>) -> HashSet<Key> {
		store
			.list(&Query::new())
			.try_filter_map(|entry| async move {
				let media =
					matches!(&entry.key, Key::Segments { track, .. } if !track.ends_with(hang::timeline::SUFFIX));
				Ok(media.then_some(entry.key))
			})
			.try_collect()
			.await
			.unwrap()
	}

	/// The media objects the retained records reference.
	fn referenced(track: &str, records: &[Record]) -> HashSet<Key> {
		records
			.iter()
			.map(|record| Key::segments(track, record.sequence).unwrap())
			.collect()
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
		control.track("video").await.unwrap();
		control.sparse("catalog.json").await.unwrap();

		group(&catalog, 0, &[0]);
		for sequence in 0..6 {
			group(&video, sequence, &[sequence * 1000, sequence * 1000 + 500]);
		}
		video.finish().unwrap();
		catalog.finish().unwrap();
		source.close();

		tokio::spawn(writer.run()).await.unwrap().unwrap();

		let records = window(&store, "video").await;
		assert_eq!(groups(&records), (0..6).map(|s| (s, s)).collect::<Vec<_>>());
		check_objects(&store, "video", &records).await;
		let catalog = window(&store, "catalog.json").await;
		assert_eq!(groups(&catalog), vec![(0, 0)]);
		check_objects(&store, "catalog.json", &catalog).await;
		assert!(window(&store, "ignored").await.is_empty());

		let object = store.get_segments("video", 2).await.unwrap();
		assert_eq!(object.groups[0].frames[1].timestamp, 2500);
		assert_eq!(object.groups[0].frames[1].payload, "2@2500");
		assert_eq!(store.get_info("video").await.unwrap(), Info::new(0, 1000).unwrap());
		store.get_info(&timeline("video")).await.unwrap();
		assert!(store.get_info("ignored").await.is_err());
	}

	#[tokio::test]
	async fn tracks_cut_independently() {
		let source = broadcast::Info::new().produce();
		let video = track(&source, "video");
		let audio = track(&source, "audio");

		let store = Store::new(InMemory::new(), "rec");
		let writer = Writer::new(store.clone(), source.consume(), Config::default())
			.await
			.unwrap();
		let control = writer.control();
		control.track("video").await.unwrap();
		control.track("audio").await.unwrap();

		// Two second GOPs, 400ms audio groups: audio packs by its own minimum, not video's GOPs.
		for sequence in 0..3 {
			group(&video, sequence, &[sequence * 2000, sequence * 2000 + 1000]);
		}
		for sequence in 0..15 {
			group(&audio, sequence, &[sequence * 400]);
		}
		video.finish().unwrap();
		audio.finish().unwrap();
		source.close();
		writer.run().await.unwrap();

		let video = window(&store, "video").await;
		assert_eq!(groups(&video), vec![(0, 0), (1, 1), (2, 2)]);
		let audio = window(&store, "audio").await;
		assert_eq!(groups(&audio), vec![(0, 2), (3, 5), (6, 8), (9, 11), (12, 14)]);
		check_objects(&store, "audio", &audio).await;
	}

	#[tokio::test]
	async fn an_append_only_group_spans_several_objects() {
		let source = broadcast::Info::new().produce();
		let log = track(&source, "log");

		let store = Store::new(InMemory::new(), "rec");
		let config =
			Config::default().with_timeline(timeline::Config::default().with_duration_max(Duration::from_secs(3)));
		let writer = Writer::new(store.clone(), source.consume(), config).await.unwrap();
		writer.control().sparse("log").await.unwrap();
		let run = tokio::spawn(writer.run());

		// One group that never closes, a frame a second.
		let mut open = log.create_group(group::Info { sequence: 0 }).unwrap();
		for second in 0..7 {
			open.write_frame(ms(second * 1000), format!("line {second}")).unwrap();
		}
		// The group is still open, yet its first six seconds are already durable.
		while window(&store, "log").await.len() < 2 {
			tokio::time::sleep(Duration::from_millis(10)).await;
		}
		let records = window(&store, "log").await;
		assert_eq!(
			records
				.iter()
				.map(|record| (record.start, record.end))
				.collect::<Vec<_>>(),
			vec![
				(Position::new(0, 0), Position::new(0, 3)),
				(Position::new(0, 3), Position::new(0, 6)),
			]
		);
		check_objects(&store, "log", &records).await;
		assert_eq!(store.get_segments("log", 1).await.unwrap().frame_start, 3);

		open.finish().unwrap();
		log.finish().unwrap();
		source.close();
		run.await.unwrap().unwrap();

		let records = window(&store, "log").await;
		assert_eq!(records.last().unwrap().start, Position::new(0, 6));
		assert_eq!(records.last().unwrap().end, Position::group(1));
		check_objects(&store, "log", &records).await;
	}

	#[tokio::test]
	async fn a_failed_put_omits_only_that_track() {
		let source = broadcast::Info::new().produce();
		let video = track(&source, "video");
		let audio = track(&source, "audio");

		let mock = Mock::memory();
		mock.fail_puts("/audio/segments/");
		let store = Store::new(mock, "rec");
		let writer = Writer::new(store.clone(), source.consume(), Config::default())
			.await
			.unwrap();
		let control = writer.control();
		control.track("video").await.unwrap();
		control.track("audio").await.unwrap();

		for sequence in 0..3 {
			group(&video, sequence, &[sequence * 1000]);
			group(&audio, sequence, &[sequence * 1000]);
		}
		video.finish().unwrap();
		audio.finish().unwrap();
		source.close();

		writer.run().await.unwrap();

		let records = window(&store, "video").await;
		assert_eq!(groups(&records), vec![(0, 0), (1, 1), (2, 2)]);
		check_objects(&store, "video", &records).await;
		assert!(window(&store, "audio").await.is_empty());
	}

	#[tokio::test]
	async fn retention_pops_then_deletes_expired_objects() {
		let source = broadcast::Info::new().produce();
		let video = track(&source, "video");
		let catalog = track(&source, "catalog.json");

		let store = Store::new(InMemory::new(), "rec");
		let config = Config::default().with_retention(Retention::new(Duration::from_secs(2), Duration::ZERO));
		let writer = Writer::new(store.clone(), source.consume(), config).await.unwrap();
		writer.control().track("video").await.unwrap();
		writer.control().sparse("catalog.json").await.unwrap();

		// A static catalog published once, then six seconds of video.
		group(&catalog, 0, &[0]);
		for sequence in 0..6 {
			group(&video, sequence, &[sequence * 1000, sequence * 1000 + 500]);
		}
		video.finish().unwrap();
		catalog.finish().unwrap();
		source.close();

		writer.run().await.unwrap();

		// Records 3 and 4 hold two seconds; the partial final record 5 is always kept.
		let records = window(&store, "video").await;
		assert_eq!(sequences(&records), vec![3, 4, 5]);
		check_objects(&store, "video", &records).await;

		// The catalog's newest record outlives the video it was published with.
		let catalog = window(&store, "catalog.json").await;
		assert_eq!(sequences(&catalog), vec![0]);
		check_objects(&store, "catalog.json", &catalog).await;

		let expected = &referenced("video", &records) | &referenced("catalog.json", &catalog);
		assert_eq!(stored(&store).await, expected);
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
		control.track("video").await.unwrap();

		for sequence in 0..6 {
			group(&video, sequence, &[sequence * 1000]);
		}
		video.finish().unwrap();
		source.close();

		let run = tokio::spawn(writer.run());
		while window(&store, "video").await.last().map(|record| record.sequence) != Some(5) {
			tokio::time::sleep(Duration::from_millis(10)).await;
		}
		assert!(!run.is_finished());
		assert_eq!(control.remove("video"), Err(Error::Closed));
		run.abort();
	}

	#[tokio::test]
	async fn accepted_groups_may_complete_out_of_order() {
		let source = broadcast::Info::new().produce();
		let video = track(&source, "video");

		let store = Store::new(InMemory::new(), "rec");
		let writer = Writer::new(store.clone(), source.consume(), Config::default())
			.await
			.unwrap();
		writer.control().track("video").await.unwrap();
		let run = tokio::spawn(writer.run());

		let mut first = video.create_group(group::Info { sequence: 0 }).unwrap();
		first.write_frame(ms(0), "0@0").unwrap();
		group(&video, 1, &[1000]);
		group(&video, 2, &[2000]);
		tokio::time::sleep(Duration::from_millis(50)).await;
		assert!(window(&store, "video").await.is_empty(), "group 1 waits for group 0");

		first.write_frame(ms(500), "0@500").unwrap();
		first.finish().unwrap();
		video.finish().unwrap();
		source.close();
		run.await.unwrap().unwrap();

		let records = window(&store, "video").await;
		assert_eq!(groups(&records), vec![(0, 0), (1, 1), (2, 2)]);
		check_objects(&store, "video", &records).await;
		let object = store.get_segments("video", 0).await.unwrap();
		let frames: Vec<_> = object.groups[0].frames.iter().map(|f| f.timestamp).collect();
		assert_eq!(frames, vec![0, 500]);
	}

	#[tokio::test]
	async fn decreasing_arrivals_are_refused() {
		let source = broadcast::Info::new().produce();
		let video = track(&source, "video");

		let store = Store::new(InMemory::new(), "rec");
		let writer = Writer::new(store.clone(), source.consume(), Config::default())
			.await
			.unwrap();
		writer.control().track("video").await.unwrap();

		group(&video, 0, &[0]);
		group(&video, 2, &[1000]);
		group(&video, 1, &[500]);
		group(&video, 3, &[2000]);
		video.finish().unwrap();
		source.close();

		writer.run().await.unwrap();

		// The skipped sequence closes a record, so no record holds a gap.
		let records = window(&store, "video").await;
		assert_eq!(groups(&records), vec![(0, 0), (2, 2), (3, 3)]);
		check_objects(&store, "video", &records).await;
	}

	#[tokio::test]
	async fn an_unrepresentable_group_fails_the_recording() {
		let source = broadcast::Info::new().produce();
		let video = track(&source, "video");

		let store = Store::new(InMemory::new(), "rec");
		let writer = Writer::new(store, source.consume(), Config::default()).await.unwrap();
		writer.control().track("video").await.unwrap();

		group(&video, 0, &[0]);
		// One past the recording's largest group ID.
		group(&video, 1 << 53, &[1000]);
		video.finish().unwrap();
		source.close();

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
		writer.control().track("video").await.unwrap();

		group(&video, 0, &[1000]);
		group(&video, 1, &[500]);
		video.finish().unwrap();
		source.close();

		assert_eq!(
			writer.run().await,
			Err(Error::Source("track video group 1: timestamp 500 precedes 1000".into()))
		);
	}

	#[tokio::test]
	async fn a_stalled_track_holds_only_itself() {
		let source = broadcast::Info::new().produce();
		let video = track(&source, "video");
		let audio = track(&source, "audio");

		let store = Store::new(InMemory::new(), "rec");
		let writer = Writer::new(store.clone(), source.consume(), Config::default())
			.await
			.unwrap();
		let control = writer.control();
		control.track("video").await.unwrap();
		control.track("audio").await.unwrap();
		let run = tokio::spawn(writer.run());

		group(&audio, 0, &[0]);
		// The audio group never completes, so its record never closes on its own.
		let mut stalled = audio.create_group(group::Info { sequence: 1 }).unwrap();
		stalled.write_frame(ms(1000), "stalled").unwrap();
		group(&audio, 2, &[2000]);
		for sequence in 0..4 {
			group(&video, sequence, &[sequence * 1000]);
		}
		video.finish().unwrap();
		source.close();

		// Video commits without waiting for the stalled audio.
		while window(&store, "video").await.len() < 4 {
			tokio::time::sleep(Duration::from_millis(10)).await;
		}
		assert!(!run.is_finished(), "a stalled track holds the recording open");

		control.remove("audio").unwrap();
		run.await.unwrap().unwrap();

		// What arrived of the stalled group is stored; the group queued behind it is dropped.
		let records = window(&store, "audio").await;
		assert_eq!(
			records
				.iter()
				.map(|record| (record.start, record.end))
				.collect::<Vec<_>>(),
			vec![
				(Position::group(0), Position::group(1)),
				(Position::group(1), Position::new(1, 1))
			]
		);
		check_objects(&store, "audio", &records).await;
		drop(stalled);
	}

	#[tokio::test]
	async fn enrollment_is_refused_for_duplicates_and_timelines() {
		let source = broadcast::Info::new().produce();
		let _video = track(&source, "video");

		let store = Store::new(InMemory::new(), "rec");
		let writer = Writer::new(store, source.consume(), Config::default()).await.unwrap();
		let control = writer.control();
		control.track("video").await.unwrap();
		assert_eq!(control.sparse("video").await, Err(Error::Enrolled("video".into())));
		let timeline = timeline("video");
		assert_eq!(control.track(&timeline).await, Err(Error::Enrolled(timeline)));
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
			control.track("video").await,
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
		writer.control().track("video").await.unwrap();
		for sequence in sequences {
			group(&video, sequence, &[sequence * 1000, sequence * 1000 + 500]);
		}
		video.finish().unwrap();
		source.close();
		writer.run().await.unwrap();
	}

	fn orphan(sequence: u64) -> Object {
		Object::new(vec![Group {
			sequence,
			frames: vec![Frame {
				timestamp: sequence * 1000,
				payload: "orphan".into(),
			}],
		}])
	}

	#[tokio::test]
	async fn a_restarted_writer_resumes_the_recording() {
		let store = Store::new(InMemory::new(), "rec");
		record(&store, Config::default(), 0..3).await;
		// The source's cache replays groups the recording already holds.
		record(&store, Config::default(), 0..6).await;

		let records = window(&store, "video").await;
		assert_eq!(sequences(&records), (0..6).collect::<Vec<_>>());
		assert_eq!(groups(&records), (0..6).map(|s| (s, s)).collect::<Vec<_>>());
		check_objects(&store, "video", &records).await;

		// The resumed timeline groups continue the stored numbering.
		let mut numbers = Vec::new();
		for segment in 0..6 {
			let object = store.get_segments(&timeline("video"), segment).await.unwrap();
			numbers.extend(object.groups.iter().map(|group| group.sequence));
		}
		assert!(numbers.windows(2).all(|pair| pair[0] < pair[1]), "{numbers:?}");
	}

	#[tokio::test]
	async fn a_resumed_append_only_group_continues_mid_group() {
		let store = Store::new(InMemory::new(), "rec");
		let config =
			Config::default().with_timeline(timeline::Config::default().with_duration_max(Duration::from_secs(3)));

		// The first run stores frames 0..6 of a group that never closes, then stops.
		let source = broadcast::Info::new().produce();
		let log = track(&source, "log");
		let writer = Writer::new(store.clone(), source.consume(), config.clone())
			.await
			.unwrap();
		writer.control().sparse("log").await.unwrap();
		let mut open = log.create_group(group::Info { sequence: 0 }).unwrap();
		for second in 0..7 {
			open.write_frame(ms(second * 1000), format!("line {second}")).unwrap();
		}
		let run = tokio::spawn(writer.run());
		while window(&store, "log").await.len() < 2 {
			tokio::time::sleep(Duration::from_millis(10)).await;
		}
		run.abort();
		let _ = run.await;
		drop(open);

		// The restarted source replays the whole group; the stored head is skipped.
		let source = broadcast::Info::new().produce();
		let log = track(&source, "log");
		let writer = Writer::new(store.clone(), source.consume(), config).await.unwrap();
		writer.control().sparse("log").await.unwrap();
		let mut replayed = log.create_group(group::Info { sequence: 0 }).unwrap();
		for second in 0..8 {
			replayed
				.write_frame(ms(second * 1000), format!("line {second}"))
				.unwrap();
		}
		replayed.finish().unwrap();
		log.finish().unwrap();
		source.close();
		writer.run().await.unwrap();

		let records = window(&store, "log").await;
		assert_eq!(
			records
				.iter()
				.map(|record| (record.start, record.end))
				.collect::<Vec<_>>(),
			vec![
				(Position::new(0, 0), Position::new(0, 3)),
				(Position::new(0, 3), Position::new(0, 6)),
				(Position::new(0, 6), Position::group(1)),
			]
		);
		check_objects(&store, "log", &records).await;
	}

	#[tokio::test]
	async fn a_restarted_dvr_deletes_unreferenced_objects() {
		let store = Store::new(InMemory::new(), "rec");
		let config = Config::default().with_retention(Retention::new(Duration::from_secs(2), Duration::ZERO));
		record(&store, config.clone(), 0..6).await;

		// An interrupted expiration, an uncommitted upload, and a track no timeline names.
		store.put_segments("video", 1, &orphan(1)).await.unwrap();
		store.put_segments("video", 6, &orphan(7)).await.unwrap();
		store.put_segments("audio", 0, &orphan(0)).await.unwrap();

		record(&store, config, 6..10).await;

		let records = window(&store, "video").await;
		assert_eq!(sequences(&records), vec![7, 8, 9]);
		assert_eq!(groups(&records), vec![(7, 7), (8, 8), (9, 9)]);
		check_objects(&store, "video", &records).await;
		assert_eq!(stored(&store).await, referenced("video", &records));
		store.get_info("video").await.unwrap();
		store.get_info(&timeline("video")).await.unwrap();
		store.get_segments(&timeline("video"), 0).await.unwrap();
	}

	#[tokio::test]
	async fn a_dvr_crash_between_pop_and_delete_is_cleaned_on_restart() {
		let store = Store::new(InMemory::new(), "rec");

		// A grace longer than the test keeps every expired object past the crash.
		let source = broadcast::Info::new().produce();
		let video = track(&source, "video");
		let config =
			Config::default().with_retention(Retention::new(Duration::from_secs(2), Duration::from_secs(3600)));
		let writer = Writer::new(store.clone(), source.consume(), config).await.unwrap();
		writer.control().track("video").await.unwrap();
		let run = tokio::spawn(writer.run());
		for sequence in 0..6 {
			group(&video, sequence, &[sequence * 1000, sequence * 1000 + 500]);
		}
		// Record 5 stays open, so the newest durable timeline object is record 4.
		while window(&store, "video").await.last().map(|record| record.sequence) != Some(4) {
			tokio::time::sleep(Duration::from_millis(10)).await;
		}
		run.abort();
		let _ = run.await;

		let retained = window(&store, "video").await;
		assert_eq!(sequences(&retained), vec![3, 4]);
		let expired: HashSet<_> = (0..3).map(|s| Key::segments("video", s).unwrap()).collect();
		assert_eq!(stored(&store).await, &referenced("video", &retained) | &expired);
		// An upload the crash left uncommitted.
		store.put_segments("video", 5, &orphan(5)).await.unwrap();

		let grace = Duration::from_millis(200);
		let config = Config::default().with_retention(Retention::new(Duration::from_secs(2), grace));
		let source = broadcast::Info::new().produce();
		let video = track(&source, "video");
		let started = Instant::now();
		let writer = Writer::new(store.clone(), source.consume(), config).await.unwrap();
		assert!(
			stored(&store).await.is_superset(&expired),
			"expired objects outlive the grace, for readers holding the old timeline"
		);
		assert!(
			!stored(&store).await.contains(&Key::segments("video", 5).unwrap()),
			"the uncommitted upload is cleared before its key is reused"
		);

		writer.control().track("video").await.unwrap();
		// The source replays groups the recording already holds; they are refused.
		for sequence in 4..9 {
			group(&video, sequence, &[sequence * 1000, sequence * 1000 + 500]);
		}
		video.finish().unwrap();
		source.close();
		writer.run().await.unwrap();
		assert!(started.elapsed() >= grace);

		let records = window(&store, "video").await;
		assert_eq!(groups(&records), vec![(6, 6), (7, 7), (8, 8)]);
		check_objects(&store, "video", &records).await;
		assert_eq!(stored(&store).await, referenced("video", &records));
		for segment in 0..=7 {
			store.get_segments(&timeline("video"), segment).await.unwrap();
		}
	}

	#[tokio::test]
	async fn an_archive_restart_clears_uncommitted_objects() {
		let store = Store::new(InMemory::new(), "rec");
		record(&store, Config::default(), 0..3).await;
		// Media stored after the last timeline commit: a crash before its record.
		store.put_segments("video", 3, &orphan(3)).await.unwrap();

		record(&store, Config::default(), 3..6).await;
		let records = window(&store, "video").await;
		assert_eq!(groups(&records), (0..6).map(|s| (s, s)).collect::<Vec<_>>());
		check_objects(&store, "video", &records).await;
		assert_eq!(stored(&store).await, referenced("video", &records));
	}

	#[tokio::test]
	async fn a_failed_recovery_deletes_nothing() {
		let mock = Mock::memory();
		let store = Store::new(mock.clone(), "rec");
		let config = Config::default().with_retention(Retention::new(Duration::from_secs(2), Duration::ZERO));
		record(&store, config.clone(), 0..6).await;
		store.put_segments("video", 1, &orphan(1)).await.unwrap();
		let before = stored(&store).await;

		let failing = mock.fork();
		failing.fail_lists();
		let source = broadcast::Info::new().produce();
		let result = Writer::new(Store::new(failing, "rec"), source.consume(), config.clone()).await;
		assert!(matches!(result, Err(Error::Store(_))));

		// A missing timeline object leaves the retained window unrecoverable.
		store
			.delete(&Key::segments(timeline("video"), 3).unwrap())
			.await
			.unwrap();
		let result = Writer::new(store.clone(), source.consume(), config).await;
		assert!(matches!(result, Err(Error::Timeline(_))));

		assert_eq!(stored(&store).await, before);
	}

	#[tokio::test]
	async fn a_dvr_window_longer_than_one_checkpoint_is_recovered() {
		let store = Store::new(InMemory::new(), "rec");
		let config = Config::default().with_retention(Retention::new(Duration::from_secs(280), Duration::ZERO));
		record(&store, config, 0..300).await;

		let recovery = recover(&store, true).await.unwrap();
		let resume = &recovery.tracks["video"];
		let records = window(&store, "video").await;
		assert!(records.len() > 256, "the window outgrows one checkpoint");
		assert_eq!(resume.checkpoint.records, records);
		assert_eq!(resume.checkpoint.range.end, 300);
		assert!(recovery.orphans.is_empty());
		assert!(recovery.uncommitted.is_empty());
		assert_eq!(resume.floor(), Some(Position::group(300)));
	}

	#[tokio::test]
	async fn a_timeline_that_does_not_end_at_the_next_segment_fails_recovery() {
		let store = Store::new(InMemory::new(), "rec");
		record(&store, Config::default(), 0..6).await;
		let before = stored(&store).await;

		// `segments/5` still decodes, but it restates an earlier window, so resuming would write the
		// next record on top of it.
		let older = store.get_segments(&timeline("video"), 0).await.unwrap();
		store
			.delete(&Key::segments(timeline("video"), 5).unwrap())
			.await
			.unwrap();
		store.put_segments(&timeline("video"), 5, &older).await.unwrap();

		let source = broadcast::Info::new().produce();
		match Writer::new(store.clone(), source.consume(), Config::default()).await {
			Err(Error::Timeline(message)) => assert!(message.contains("not segment 6"), "{message}"),
			Err(err) => panic!("expected a timeline error, got {err}"),
			Ok(_) => panic!("expected recovery to fail"),
		}
		assert_eq!(stored(&store).await, before);
	}
}
