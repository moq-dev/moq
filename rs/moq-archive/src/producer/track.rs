//! One track's recorder: its runs, in order, into duration-cut objects.

use std::collections::{BTreeMap, VecDeque};
use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use bytes::Bytes;
use moq_clock::Instant;
use moq_net::track::{Anchor, Order, Position, Range, Subscription};
use moq_net::{Datagram, StreamError, Timescale, group};
use object_store::path::Path;
use object_store::{ObjectStore, ObjectStoreExt, PutMode, PutPayload};

use super::Shared;
use super::resume::Stored;
use crate::layout::{self, End, Entry, Object};
use crate::segment::Frame;
use crate::{Error, Info, Result};

/// Record each run of the track `name` reported through `queue`, until the queue closes with the
/// broadcast's listing. Returns how the last run ended, or the stored marker when no run did.
pub(super) async fn record<S: ObjectStore>(
	shared: Arc<Shared<S>>,
	name: String,
	queue: kio::Queue<moq_net::track::Info>,
	stored: Option<Stored>,
) -> Result<(String, Option<End>)> {
	let mut track = Track::new(shared, &name, stored)?;
	while let Some(info) = track.idle(&queue).await? {
		track.properties(&info).await?;
		let end = track.run(&info).await?;
		track.end = Some(end);
	}
	Ok((name, track.end))
}

struct Track<S> {
	shared: Arc<Shared<S>>,
	name: String,
	dir: Path,
	/// The stored properties, once checked or written.
	info: Option<Info>,
	/// Whether `info` is known to be stored.
	info_stored: bool,
	timescale: Option<Timescale>,
	/// The first position not stored yet, unknown until something is.
	next: Option<Position>,
	/// The object being filled.
	object: Option<Building>,
	/// The newest stored object, which DVR pruning always keeps.
	newest: Option<Path>,
	/// Objects a newer one superseded, oldest first, with when.
	superseded: VecDeque<(Instant, Path)>,
	/// Writes owed to the store, in order.
	puts: Vec<Put>,
	end: Option<End>,
}

struct Building {
	object: Object,
	/// When it was opened, which cuts an untimed object by wall clock.
	opened: Instant,
	/// The arrival of its oldest frame not stored yet, which bounds the flush.
	unstored: Option<Instant>,
	/// Whether it is in the store under its key, as an untimed object rewritten in place is.
	stored: bool,
	/// On a timed track, the timestamp its last entry's run in this object starts at.
	run_pts: u64,
}

struct Put {
	path: Path,
	bytes: Bytes,
	/// Rewrite an untimed object in place, rather than create an immutable one.
	overwrite: bool,
}

/// A sequence received ahead of the one being stored.
enum Pending {
	Group(Box<group::Consumer>, Instant),
	Datagram(Datagram, Instant),
}

impl Pending {
	fn arrived(&self) -> Instant {
		match self {
			Self::Group(_, at) | Self::Datagram(_, at) => *at,
		}
	}
}

/// One subscription's progress.
struct Run {
	max_delay: Duration,
	/// Received sequences not stored yet, waiting on a lower one.
	pending: BTreeMap<u64, Pending>,
	/// The group being stored, frame by frame.
	current: Option<Box<group::Consumer>>,
	/// When the gap below the lowest pending sequence is given up.
	gap: Option<Instant>,
	/// When the first sequence arrived while nothing was stored, so nothing says where the track
	/// starts: arrival is not sequence order, and a lower one may still be on its way.
	first: Option<Instant>,
	groups_done: bool,
	datagrams_done: bool,
}

enum Event {
	Frame(u64, moq_net::Result<Option<moq_net::frame::Frame>>),
	// Boxed, since a group consumer dwarfs every other event and waits in `Run::pending`.
	Group(moq_net::Result<Option<Box<group::Consumer>>>),
	Datagram(moq_net::Result<Option<Datagram>>),
	Timer,
}

impl<S: ObjectStore> Track<S> {
	fn new(shared: Arc<Shared<S>>, name: &str, stored: Option<Stored>) -> Result<Self> {
		let dir = layout::track_dir(shared.store.prefix(), name)?;
		let mut track = Self {
			shared,
			name: name.to_string(),
			dir,
			info: None,
			info_stored: false,
			timescale: None,
			next: None,
			object: None,
			newest: None,
			superseded: VecDeque::new(),
			puts: Vec::new(),
			end: None,
		};
		let Some(stored) = stored else {
			return Ok(track);
		};

		track.timescale = timescale(&stored.info)?;
		track.info = Some(stored.info);
		track.end = stored.end;
		let now = moq_clock::now();
		let mut objects = stored.objects;
		track.newest = objects.pop();
		if track.shared.config.dvr.is_some() {
			// Their age is unknown, so their window starts now.
			track.superseded = objects.into_iter().map(|path| (now, path)).collect();
		}
		if let Some(newest) = stored.newest {
			track.next = Some(newest.end()?);
			// An untimed object keeps growing in place; a timed one is immutable.
			if track.timescale.is_none() {
				track.object = Some(Building {
					object: newest,
					opened: now,
					unstored: None,
					stored: true,
					run_pts: 0,
				});
			}
		}
		Ok(track)
	}

	/// Wait for the next run, pruning meanwhile: the newest of the queued track infos, or `None`
	/// once the listing ended.
	async fn idle(&mut self, queue: &kio::Queue<moq_net::track::Info>) -> Result<Option<moq_net::track::Info>> {
		loop {
			let mut timer = self.prune_at().map(|at| Box::pin(moq_clock::sleep_until(at)));
			let popped = kio::wait(|waiter| {
				if let Poll::Ready(popped) = queue.poll_pop(waiter) {
					return Poll::Ready(Some(popped));
				}
				ready_at(&mut timer, waiter).map(|()| None)
			})
			.await;
			match popped {
				// A track removed and re-added while a run was still draining queues it twice.
				Some(Ok(mut info)) => {
					while let Ok(Some(newer)) = queue.try_pop() {
						info = newer;
					}
					return Ok(Some(info));
				}
				Some(Err(kio::Closed)) => return Ok(None),
				None => self.prune().await?,
			}
		}
	}

	/// Store the track's properties before its first object, refusing ones that changed.
	async fn properties(&mut self, source: &moq_net::track::Info) -> Result<()> {
		let info = Info::new(source.priority, source.timescale.map(Timescale::as_u64))?;
		if let Some(existing) = &self.info {
			if existing.priority != info.priority {
				return Err(Error::Priority {
					existing: existing.priority,
					intended: info.priority,
				});
			}
			if existing.timescale != info.timescale {
				return Err(Error::TimescaleMismatch {
					existing: existing.timescale,
					intended: info.timescale,
				});
			}
		}
		if !self.info_stored {
			self.shared.store.put_info(&self.name, &info).await?;
			self.info_stored = true;
		}
		self.timescale = source.timescale;
		self.info = Some(info);
		Ok(())
	}

	/// Record one subscription until both its groups and its datagrams end, returning how.
	async fn run(&mut self, info: &moq_net::track::Info) -> Result<End> {
		let config = &self.shared.config;
		let max_delay = info.max_age.map_or(config.max_delay, |age| age.min(config.max_delay));
		let range = match self.next {
			Some(next) => Range {
				start: Anchor::Absolute(next),
				end: None,
			},
			None => Range::oldest(),
		};
		let subscription = Subscription::default()
			.with_order(Order::Ascending)
			.with_max_delay(max_delay)
			.with_ranges([range]);

		let track = match self.shared.broadcast.track(&self.name) {
			Ok(track) => track,
			Err(err) => return Ok(error(&err)),
		};
		let mut subscriber = match track.subscribe(subscription).await {
			Ok(subscriber) => subscriber,
			Err(err) => return Ok(error(&err)),
		};

		let mut run = Run {
			max_delay,
			pending: BTreeMap::new(),
			current: None,
			gap: None,
			first: None,
			groups_done: false,
			datagrams_done: false,
		};
		loop {
			self.advance(&mut run, false)?;
			self.flush_due(moq_clock::now())?;
			self.write().await?;
			self.prune().await?;
			if run.groups_done && run.datagrams_done {
				break;
			}

			let deadlines = [self.flush_at(), run.gap, self.prune_at()];
			let mut timer = deadlines
				.into_iter()
				.flatten()
				.min()
				.map(|at| Box::pin(moq_clock::sleep_until(at)));
			let event = kio::wait(|waiter| {
				if let Some(current) = run.current.as_mut() {
					let index = current.index();
					if let Poll::Ready(frame) = current.poll_read_frame(waiter) {
						return Poll::Ready(Event::Frame(index, frame));
					}
				}
				if !run.groups_done
					&& let Poll::Ready(group) = subscriber.poll_recv_group(waiter)
				{
					return Poll::Ready(Event::Group(group.map(|group| group.map(Box::new))));
				}
				if !run.datagrams_done
					&& let Poll::Ready(datagram) = subscriber.poll_recv_datagram(waiter)
				{
					return Poll::Ready(Event::Datagram(datagram));
				}
				ready_at(&mut timer, waiter).map(|()| Event::Timer)
			})
			.await;

			let now = moq_clock::now();
			match event {
				Event::Frame(index, read) => self.frame(&mut run, index, read, now)?,
				Event::Group(Ok(Some(group))) => {
					if self.next.is_none_or(|next| group.sequence >= next.group) {
						run.pending.insert(group.sequence, Pending::Group(group, now));
					}
				}
				Event::Datagram(Ok(Some(datagram))) => {
					if self.next.is_none_or(|next| datagram.sequence >= next.group) {
						run.pending.insert(datagram.sequence, Pending::Datagram(datagram, now));
					}
				}
				// How the track ended is read from the subscription below.
				Event::Group(_) => run.groups_done = true,
				Event::Datagram(_) => run.datagrams_done = true,
				Event::Timer => {}
			}
		}

		// Everything received is stored, gaps and all, now that nothing more arrives.
		while run.current.is_some() || !run.pending.is_empty() {
			self.advance(&mut run, true)?;
			if let Some(current) = run.current.as_mut() {
				let index = current.index();
				let read = kio::wait(|waiter| current.poll_read_frame(waiter)).await;
				self.frame(&mut run, index, read, moq_clock::now())?;
			}
		}

		let end = match kio::wait(|waiter| subscriber.poll_finished(waiter)).await {
			Ok(end) => End::Finish(end.into()),
			Err(err) => error(&err),
		};
		self.flush()?;
		self.write().await?;
		Ok(end)
	}

	/// Take pending sequences in order while the next one is here or its gap was given up.
	fn advance(&mut self, run: &mut Run, draining: bool) -> Result<()> {
		run.gap = None;
		while run.current.is_none() {
			let Some(entry) = run.pending.first_entry() else {
				return Ok(());
			};
			let sequence = *entry.key();
			let waiting = match self.next {
				Some(next) if sequence < next.group => {
					entry.remove();
					continue;
				}
				Some(next) => (sequence > next.group).then(|| entry.get().arrived()),
				// Nothing is below 0; anything else waits from the first arrival, so a stream of
				// ever lower sequences cannot hold the track back.
				None => {
					let first = *run.first.get_or_insert(entry.get().arrived());
					(sequence > 0).then_some(first)
				}
			};
			if let Some(waiting) = waiting
				&& !draining && moq_clock::now() < waiting + run.max_delay
			{
				run.gap = Some(waiting + run.max_delay);
				return Ok(());
			}
			match entry.remove() {
				Pending::Datagram(datagram, arrived) => {
					let frame = Frame {
						timestamp: self.stamp(datagram.timestamp)?,
						payload: datagram.payload,
					};
					self.push(sequence, 0, frame, true, arrived)?;
					self.next = Some(Position::group(sequence + 1));
				}
				Pending::Group(mut group, _) => {
					// A resumed group continues after its stored frames. The clamp also starts a group
					// whose head is held elsewhere at its first frame, rather than lagging behind it.
					let stored = self
						.next
						.filter(|next| next.group == sequence)
						.map_or(0, |next| next.frame);
					group.set_frames(stored..);
					self.next = Some(Position {
						group: sequence,
						frame: group.index(),
					});
					run.current = Some(group);
				}
			}
		}
		Ok(())
	}

	/// Store the frame read at `index` of the group being stored, or move past that group once it
	/// ended, cleanly or not (aborted, or past the budget), keeping what it stored.
	fn frame(
		&mut self,
		run: &mut Run,
		index: u64,
		read: moq_net::Result<Option<moq_net::frame::Frame>>,
		now: Instant,
	) -> Result<()> {
		let sequence = run.current.as_ref().expect("reading a group").sequence;
		match read {
			Ok(Some(frame)) => {
				let frame = Frame {
					timestamp: self.stamp(frame.timestamp)?,
					payload: frame.payload,
				};
				self.push(sequence, index, frame, false, now)?;
				self.next = Some(Position {
					group: sequence,
					frame: index + 1,
				});
			}
			ended => {
				if let Err(err) = ended {
					tracing::debug!(track = %self.name, sequence, %err, "group ended early");
				}
				run.current = None;
				self.next = Some(Position::group(sequence + 1));
			}
		}
		Ok(())
	}

	/// A frame's timestamp in the track's timescale, which it carries exactly when timed.
	fn stamp(&self, timestamp: Option<moq_net::Timestamp>) -> Result<Option<u64>> {
		match (timestamp, self.timescale) {
			(Some(timestamp), Some(timescale)) => {
				Ok(Some(timestamp.convert(timescale).map_err(|_| Error::Timing)?.value()))
			}
			(None, None) => Ok(None),
			_ => Err(Error::Timing),
		}
	}

	/// Append one frame of `sequence`, or a datagram, cutting the object before it when due.
	fn push(&mut self, sequence: u64, index: u64, frame: Frame, datagram: bool, now: Instant) -> Result<()> {
		let config = &self.shared.config;
		if let Some(building) = &self.object {
			let last = building.object.entries.last().map(Entry::sequence);
			let boundary = last != Some(sequence);
			// Only an object's first entry carries a frame start, so a group starting past frame 0
			// opens its own object.
			let cut = (boundary && index != 0)
				|| match (self.timescale, frame.timestamp) {
					(Some(timescale), Some(pts)) => {
						let duration = units(config.duration, timescale);
						let first = building.object.start()?.pts.expect("a timed object starts timed");
						match boundary {
							true => pts.saturating_sub(first) >= duration,
							// An oversized group splits once its own run here reaches the duration.
							false => pts.saturating_sub(building.run_pts) >= duration,
						}
					}
					_ => boundary && now.saturating_duration_since(building.opened) >= config.duration,
				};
			if cut {
				self.close()?;
			}
		}

		let building = self.object.get_or_insert_with(|| Building {
			object: Object {
				frame_start: index,
				entries: Vec::new(),
			},
			opened: now,
			unstored: None,
			stored: false,
			run_pts: 0,
		});
		let pts = frame.timestamp.unwrap_or(0);
		match building.object.entries.last_mut() {
			Some(Entry::Group { sequence: last, frames }) if *last == sequence && !datagram => frames.push(frame),
			_ => {
				building.run_pts = pts;
				building.object.entries.push(match datagram {
					true => Entry::Datagram { sequence, frame },
					false => Entry::Group {
						sequence,
						frames: vec![frame],
					},
				});
			}
		}
		building.unstored.get_or_insert(now);
		Ok(())
	}

	/// When the object's oldest unstored frame is due in the store.
	fn flush_at(&self) -> Option<Instant> {
		let unstored = self.object.as_ref()?.unstored?;
		Some(unstored + self.shared.config.flush)
	}

	fn flush_due(&mut self, now: Instant) -> Result<()> {
		if self.flush_at().is_some_and(|at| at <= now) {
			self.flush()?;
		}
		Ok(())
	}

	/// Store the object: a timed one is closed, so its group continues in the next; an untimed
	/// one is rewritten in place and stays open.
	fn flush(&mut self) -> Result<()> {
		match self.timescale {
			Some(_) => self.close(),
			None => {
				let Some(building) = self.object.as_mut() else {
					return Ok(());
				};
				if building.unstored.take().is_some() {
					let bytes = building.object.encode(false)?;
					building.stored = true;
					let path = self.object_path()?;
					self.queue(path, bytes, true);
				}
				Ok(())
			}
		}
	}

	/// Store the object for good and start the next one at the following frame.
	fn close(&mut self) -> Result<()> {
		let Some(building) = self.object.take() else {
			return Ok(());
		};
		if building.unstored.is_none() && building.stored {
			return Ok(());
		}
		let timed = self.timescale.is_some();
		let bytes = building.object.encode(timed)?;
		let path = push(&self.dir, &building.object.start()?.name()?)?;
		self.queue(path, bytes, !timed);
		Ok(())
	}

	fn object_path(&self) -> Result<Path> {
		let building = self.object.as_ref().expect("an open object");
		push(&self.dir, &building.object.start()?.name()?)
	}

	fn queue(&mut self, path: Path, bytes: Bytes, overwrite: bool) {
		if self.newest.as_ref() != Some(&path) {
			let superseded = self.newest.replace(path.clone());
			if let Some(superseded) = superseded
				&& self.shared.config.dvr.is_some()
			{
				self.superseded.push_back((moq_clock::now(), superseded));
			}
		}
		self.puts.push(Put { path, bytes, overwrite });
	}

	/// Perform the owed writes in order.
	async fn write(&mut self) -> Result<()> {
		let store = self.shared.store.inner();
		for put in std::mem::take(&mut self.puts) {
			let payload = PutPayload::from(put.bytes.clone());
			if put.overwrite {
				store.put(&put.path, payload).await?;
				continue;
			}
			match store.put_opts(&put.path, payload, PutMode::Create.into()).await {
				Ok(_) => {}
				// Written already, by an earlier attempt that failed to report success.
				Err(object_store::Error::AlreadyExists { .. }) => {
					let existing = store.get(&put.path).await?.bytes().await?;
					if existing != put.bytes {
						return Err(Error::Conflict(put.path.to_string()));
					}
				}
				Err(err) => return Err(err.into()),
			}
		}
		Ok(())
	}

	fn prune_at(&self) -> Option<Instant> {
		let dvr = self.shared.config.dvr.as_ref()?;
		let (superseded, _) = self.superseded.front()?;
		Some(*superseded + dvr.window + dvr.grace)
	}

	/// Delete the superseded objects whose window and grace have passed.
	async fn prune(&mut self) -> Result<()> {
		let now = moq_clock::now();
		while self.prune_at().is_some_and(|at| at <= now) {
			let (_, path) = self.superseded.pop_front().expect("a superseded object is due");
			match self.shared.store.inner().delete(&path).await {
				Ok(()) | Err(object_store::Error::NotFound { .. }) => {}
				Err(err) => return Err(err.into()),
			}
		}
		Ok(())
	}
}

fn push(dir: &Path, name: &str) -> Result<Path> {
	crate::path::push(dir, name)
}

fn timescale(info: &Info) -> Result<Option<Timescale>> {
	info.timescale
		.map(|timescale| Timescale::new(timescale).map_err(|_| Error::Timescale(timescale)))
		.transpose()
}

/// `duration` in `timescale` units, saturating.
fn units(duration: Duration, timescale: Timescale) -> u64 {
	let units = duration.as_nanos() * u128::from(timescale.as_u64()) / 1_000_000_000;
	u64::try_from(units).unwrap_or(u64::MAX)
}

fn error(err: &moq_net::Error) -> End {
	End::Error(StreamError::from(err).to_code())
}

/// Poll an optional timer, pending forever without one.
fn ready_at(timer: &mut Option<Pin<Box<moq_clock::Sleep>>>, waiter: &kio::Waiter) -> Poll<()> {
	match timer.as_mut() {
		Some(timer) => waiter.poll_future(timer.as_mut()),
		None => Poll::Pending,
	}
}
