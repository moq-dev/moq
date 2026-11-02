//! One track's recorder: its runs, in order, into duration-cut objects.

use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use bytes::Bytes;
use kio::clock::Instant;
use moq_net::track::{Anchor, Order, Position, Range, Subscription};
use moq_net::{StreamError, Timescale, group};
use object_store::path::Path;
use object_store::{ObjectStore, ObjectStoreExt, PutMode, PutPayload};

use super::Shared;
use super::ordered::{Item, Ordered};
use super::resume::Stored;
use crate::layout::{self, End, Entry, Frame, Object};
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
	puts: Vec<(Path, Bytes)>,
	end: Option<End>,
}

struct Building {
	object: Object,
	/// When it was opened, which cuts an untimed object by wall clock.
	opened: Instant,
	/// The arrival of its oldest frame, which bounds the flush.
	unstored: Instant,
	/// Where its last entry's run in this object starts: the timestamp on a timed track, the
	/// arrival on an untimed one, which splits an oversized group.
	run_pts: u64,
	run_at: Instant,
}

enum Event {
	Frame(u64, moq_net::Result<Option<moq_net::frame::Frame>>),
	Next(Option<(u64, Item)>),
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
		track.next = stored.next;
		let mut objects = stored.objects;
		track.newest = objects.pop();
		if track.shared.config.dvr.is_some() {
			// Their age is unknown, so their window starts now.
			let now = kio::clock::now();
			track.superseded = objects.into_iter().map(|path| (now, path)).collect();
		}
		Ok(track)
	}

	/// Wait for the next run, pruning meanwhile: the queued track info, which the listing keeps
	/// to the newest, or `None` once the listing ended.
	async fn idle(&mut self, queue: &kio::Queue<moq_net::track::Info>) -> Result<Option<moq_net::track::Info>> {
		loop {
			let mut timer = self.prune_at().map(|at| Box::pin(kio::clock::sleep_until(at)));
			let popped = kio::wait(|waiter| {
				if let Poll::Ready(popped) = queue.poll_pop(waiter) {
					return Poll::Ready(Some(popped));
				}
				ready_at(&mut timer, waiter).map(|()| None)
			})
			.await;
			match popped {
				Some(Ok(info)) => return Ok(Some(info)),
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
		let shared = self.shared.clone();
		let config = &shared.config;
		let range = match self.next {
			Some(next) => Range {
				start: Anchor::Absolute(next.group),
				end: None,
			},
			None => Range::oldest(),
		};
		// The cap, since `info` may describe an older instance than the one subscribed, and a
		// backlog a smaller budget skipped is not sent again. The wait for a gap is narrowed below.
		let subscription = Subscription::default()
			.with_order(Order::Ascending)
			.with_max_delay(config.max_delay)
			.with_ranges([range]);

		let subscribed = match self.shared.broadcast.track(&self.name) {
			Ok(track) => track.subscribe(subscription).await,
			Err(err) => Err(err),
		};
		let subscriber = match subscribed {
			Ok(subscriber) => subscriber,
			Err(err) => {
				// The track keeps its `.info` beside the `end` this run leaves.
				self.properties(info).await?;
				return Ok(error(&err));
			}
		};
		// The listing's info may describe an instance the name no longer resolves to, so the
		// subscribed one's properties are what its frames are stored under.
		self.properties(subscriber.info()).await?;
		let max_delay = delay(config, subscriber.info());

		let mut ordered = Ordered::new(subscriber, max_delay, self.next.map(|next| next.group));
		// The group being stored, frame by frame.
		let mut current: Option<Box<group::Consumer>> = None;
		loop {
			self.flush_due(kio::clock::now())?;
			self.write().await?;
			self.prune().await?;

			let mut timer = [self.flush_at(), self.prune_at()]
				.into_iter()
				.flatten()
				.min()
				.map(|at| Box::pin(kio::clock::sleep_until(at)));
			let event = kio::wait(|waiter| {
				match current.as_mut() {
					Some(current) => {
						let index = current.index();
						if let Poll::Ready(frame) = current.poll_read_frame(waiter) {
							return Poll::Ready(Event::Frame(index, frame));
						}
						// Arrival times decide the waits, so arrivals are taken meanwhile.
						ordered.poll_arrivals(waiter);
					}
					None => {
						if let Poll::Ready(next) = ordered.poll_next(waiter) {
							return Poll::Ready(Event::Next(next));
						}
					}
				}
				ready_at(&mut timer, waiter).map(|()| Event::Timer)
			})
			.await;

			let now = kio::clock::now();
			match event {
				Event::Frame(index, read) => self.frame(&mut current, index, read, now)?,
				Event::Next(Some((sequence, Item::Datagram(datagram)))) => {
					let frame = Frame {
						timestamp: self.stamp(datagram.timestamp)?,
						payload: datagram.payload,
					};
					self.push(sequence, 0, frame, true, now)?;
					self.next = Some(Position::group(sequence + 1));
				}
				Event::Next(Some((sequence, Item::Group(group)))) => {
					// A resumed group continues after its stored frames. The clamp also starts a
					// group whose head is held elsewhere at its first frame, rather than lagging
					// behind it.
					let stored = self
						.next
						.filter(|next| next.group == sequence)
						.map_or(0, |next| next.frame);
					let group = Box::new(group.with_range(stored..));
					self.next = Some(Position {
						group: sequence,
						frame: group.index(),
					});
					current = Some(group);
				}
				Event::Next(None) => break,
				Event::Timer => {}
			}
		}

		let end = match kio::wait(|waiter| ordered.poll_finished(waiter)).await {
			Ok(end) => End::Finish(end.into()),
			Err(err) => error(&err),
		};
		self.close()?;
		self.write().await?;
		Ok(end)
	}

	/// Store the frame read at `index` of the group being stored, or move past that group once it
	/// ended, cleanly or not (aborted, or past the budget), keeping what it stored.
	fn frame(
		&mut self,
		current: &mut Option<Box<group::Consumer>>,
		index: u64,
		read: moq_net::Result<Option<moq_net::frame::Frame>>,
		now: Instant,
	) -> Result<()> {
		let sequence = current.as_ref().expect("reading a group").sequence;
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
				*current = None;
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
					_ => {
						let since = match boundary {
							true => building.opened,
							false => building.run_at,
						};
						now.saturating_duration_since(since) >= config.duration
					}
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
			unstored: now,
			run_pts: 0,
			run_at: now,
		});
		let pts = frame.timestamp.unwrap_or(0);
		match building.object.entries.last_mut() {
			Some(Entry::Group { sequence: last, frames }) if *last == sequence && !datagram => frames.push(frame),
			_ => {
				building.run_pts = pts;
				building.run_at = now;
				building.object.entries.push(match datagram {
					true => Entry::Datagram { sequence, frame },
					false => Entry::Group {
						sequence,
						frames: vec![frame],
					},
				});
			}
		}
		Ok(())
	}

	/// When the object's oldest frame is due in the store.
	fn flush_at(&self) -> Option<Instant> {
		Some(self.object.as_ref()?.unstored + self.shared.config.flush)
	}

	/// Store the object once its oldest frame is due, so its group continues in the next.
	fn flush_due(&mut self, now: Instant) -> Result<()> {
		if self.flush_at().is_some_and(|at| at <= now) {
			self.close()?;
		}
		Ok(())
	}

	/// Store the object for good and start the next one at the following frame.
	fn close(&mut self) -> Result<()> {
		let Some(building) = self.object.take() else {
			return Ok(());
		};
		let bytes = building.object.encode(self.timescale.is_some())?;
		let path = push(&self.dir, &building.object.start()?.name()?)?;
		if let Some(superseded) = self.newest.replace(path.clone())
			&& self.shared.config.dvr.is_some()
		{
			self.superseded.push_back((kio::clock::now(), superseded));
		}
		self.puts.push((path, bytes));
		Ok(())
	}

	/// Perform the owed writes in order.
	async fn write(&mut self) -> Result<()> {
		let store = self.shared.store.inner();
		for (path, bytes) in std::mem::take(&mut self.puts) {
			let payload = PutPayload::from(bytes.clone());
			match store.put_opts(&path, payload, PutMode::Create.into()).await {
				Ok(_) => {}
				// Written already, by an earlier attempt that failed to report success.
				Err(object_store::Error::AlreadyExists { .. }) => {
					let existing = store.get(&path).await?.bytes().await?;
					if existing != bytes {
						return Err(Error::Conflict(path.to_string()));
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
		let now = kio::clock::now();
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

/// How long a missing group is waited for: the configured cap, or the publisher's max age below it.
fn delay(config: &super::Config, info: &moq_net::track::Info) -> Duration {
	info.max_age.map_or(config.max_delay, |age| age.min(config.max_delay))
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
fn ready_at(timer: &mut Option<Pin<Box<kio::clock::Sleep>>>, waiter: &kio::Waiter) -> Poll<()> {
	match timer.as_mut() {
		Some(timer) => waiter.poll_future(timer.as_mut()),
		None => Poll::Pending,
	}
}
