//! One served track: its live feed of what the archive gains, and fills of what it holds.

use std::sync::{Arc, Mutex};

use futures::future::{self, Either};
use moq_net::track::{FillGroups, Position};
use moq_net::{Timescale, Timestamp, group, track};
use object_store::ObjectStore;

use super::Shared;
use super::index::{self, Index, Start};
use crate::layout::{End, Entry, Frame, Object};
use crate::{Error, Info, Result};

/// A track's live feed: its newest stored sequence when found, then everything stored after.
pub(super) struct Live {
	producer: track::Producer,
	timescale: Option<Timescale>,
	/// The newest group, kept open since a later object may continue it.
	open: Option<Open>,
	/// One past the newest sequence fed, once the feed declared where it starts; fills serve
	/// everything before that start.
	next: Option<u64>,
}

struct Open {
	feed: Feed,
	sequence: u64,
	/// The index of the next frame it takes.
	next: u64,
}

/// Where the newest stored group stands in the live feed.
enum Feed {
	/// Its frame 0, which creates the group, has yet to be read.
	Unopened,
	Open(group::Producer),
	/// Refused, its head pruned or its stored timestamps unusable, so the rest of it is skipped.
	Refused,
}

impl Feed {
	fn take(&mut self) -> Option<group::Producer> {
		match std::mem::replace(self, Self::Refused) {
			Self::Open(group) => Some(group),
			Self::Unopened | Self::Refused => None,
		}
	}
}

impl Live {
	pub fn new(producer: track::Producer) -> Self {
		let timescale = producer.info().timescale;
		Self {
			producer,
			timescale,
			open: None,
			next: None,
		}
	}

	pub fn is_started(&self) -> bool {
		self.next.is_some()
	}

	/// Feed `objects`' entries from sequence `from` on, in order.
	pub fn push(&mut self, objects: &[Arc<Object>], from: u64) -> Result<()> {
		for object in objects {
			for (index, entry) in object.entries.iter().enumerate() {
				if entry.sequence < from {
					continue;
				}
				if self.next.is_none() {
					// Readers ask fills for everything older.
					self.producer.start_at(entry.sequence)?;
				}
				let first = match index {
					0 => object.frame_start,
					_ => 0,
				};
				self.entry(entry, first)?;
				self.next = Some(entry.sequence + 1);
			}
		}
		Ok(())
	}

	fn entry(&mut self, entry: &Entry, first: u64) -> Result<()> {
		let sequence = entry.sequence;
		if self.open.as_ref().is_some_and(|open| open.sequence != sequence) {
			self.close()?;
		}
		let producer = &self.producer;
		let drop = |err: moq_net::Error| -> Result<()> {
			let end = sequence.checked_add(1).ok_or(Error::Overflow)?;
			producer.drop_groups(sequence..end, moq_net::StreamError::from(&err));
			Ok(())
		};
		// The group stays in `open` while it is written, so a failure aborts it with the track
		// rather than dropping it.
		let open = match &mut self.open {
			Some(open) => {
				if open.next != first {
					return Err(Error::Sequence);
				}
				open
			}
			None => {
				// A group is created from its frame 0, so one whose head was pruned is gone, as a
				// fill reads it.
				let feed = match first {
					0 => Feed::Unopened,
					_ => {
						drop(moq_net::Error::NotFound)?;
						Feed::Refused
					}
				};
				self.open.insert(Open {
					feed,
					sequence,
					next: first,
				})
			}
		};
		for frame in &entry.frames {
			open.next += 1;
			let timestamp = match open.feed {
				Feed::Refused => continue,
				_ => timestamp(frame.timestamp, self.timescale, entry.sequence)?,
			};
			match &mut open.feed {
				Feed::Refused => {}
				// A group starts at its frame 0, so one stored under another key can't be served
				// under the key the archive indexes it by.
				Feed::Unopened if timestamp.value() != sequence => {
					tracing::warn!(sequence, "archived group's first frame is not at its key, refusing it");
					drop(moq_net::Error::TimestampMismatch)?;
					open.feed = Feed::Refused;
				}
				Feed::Unopened => open.feed = Feed::Open(producer.create_group(timestamp, frame.payload.clone())?),
				Feed::Open(group) if decreases(timestamp, group.latest()) => {
					tracing::warn!(sequence, "archived group's timestamps decrease, refusing it");
					if let Some(group) = open.feed.take() {
						let _ = group.abort(moq_net::Error::TimestampMismatch);
					}
				}
				Feed::Open(group) => group.write_frame(timestamp, frame.payload.clone())?,
			}
		}
		Ok(())
	}

	fn close(&mut self) -> Result<()> {
		if let Some(group) = self.open.take().and_then(|mut open| open.feed.take()) {
			group.finish()?;
		}
		Ok(())
	}

	/// End the feed with `err` when serving the archive failed, the open group included, so
	/// readers see why rather than a dropped producer.
	pub fn abort(self, err: moq_net::Error) {
		if let Some(group) = self.open.and_then(|mut open| open.feed.take()) {
			let _ = group.abort(err.clone());
		}
		let _ = self.producer.abort(err);
	}

	/// End the feed as the archive recorded.
	pub fn end(&mut self, end: End) -> Result<()> {
		self.close()?;
		match end {
			End::Finish(at) => {
				self.producer.finish_at(Position::new(at.group, at.frame))?;
				// An empty group stores no object, so the replay declares it never produces what
				// the archive holds nothing for up to the end, and the end settles over it.
				let fin = at.group.checked_add(u64::from(at.frame != 0)).ok_or(Error::Overflow)?;
				let from = self.next.unwrap_or(0);
				self.producer.skip_groups(from..fin);
			}
			End::Error(code) => {
				// After the newest stored sequence, keeping the history an abort would refuse.
				let err = moq_net::StreamError::from_code(code).into();
				drop(self.producer.clone().fail(err)?);
			}
		}
		Ok(())
	}
}

/// Serve `fill` from the archive, a group at a time in the fill's order as its readers pick
/// them up, until every group is answered or nobody waits on it.
pub(super) async fn serve<S: ObjectStore>(shared: Arc<Shared<S>>, index: Arc<Mutex<Index>>, mut fill: FillGroups) {
	while let Some(sequence) = fill.wanted_changed().await {
		match sequence == fill.start().group && fill.floor() < sequence {
			true => look_up(&shared, &index, &fill, sequence).await,
			false => serve_group(&shared, &index, &fill, sequence, sequence).await,
		}
	}
}

/// Answer `fill`'s start at `pts`, a time in the track's timescale, with the group containing
/// it: the newest group starting at or before it, found among the objects whose keys say they
/// may hold it. A group starts at its key, as the live cache resolves it. Dropped when the
/// archive holds none from the fill's floor through it, and refused, failing the reader until
/// its ranges change, when the store fails or the group found can't be served.
async fn look_up<S: ObjectStore>(shared: &Shared<S>, index: &Mutex<Index>, fill: &FillGroups, pts: u64) {
	loop {
		match search(shared, index, pts).await {
			Search::Found(sequence) if sequence >= fill.floor() => {
				serve_group(shared, index, fill, pts, sequence).await;
				break;
			}
			// The cache holds a newer group at or before it than the archive does.
			Search::Found(_) | Search::None => {
				fill.drop_groups(fill.floor()..pts + 1);
				return;
			}
			// The index dropped what the store pruned, at least the object found gone, which no
			// listing adds back, so the search starts over on what it retains. Each retry drops an
			// object for good, so only a store pruning what it lists faster than a GET keeps
			// this going, at one LIST and GET per object it writes.
			Search::Pruned => {}
			Search::Failed => break,
		}
	}
	// Left unanswered while a reader still waits, as when the group found was pruned since.
	if fill.is_wanted(pts) {
		fill.refuse_groups(pts..pts + 1, moq_net::Error::Stream(moq_net::StreamError::Internal));
	}
}

/// How a search for the group containing a time ended.
enum Search {
	/// At the group starting at this sequence.
	Found(u64),
	/// With no group starting at or before it.
	None,
	/// At an object the store pruned since it was listed.
	Pruned,
	/// With a store failure.
	Failed,
}

/// Search the index for the group containing `pts`; see [`look_up`].
async fn search<S: ObjectStore>(shared: &Shared<S>, index: &Mutex<Index>, pts: u64) -> Search {
	let (around, timed) = {
		let index = index.lock().unwrap();
		(index.around(pts), index.timed())
	};
	// Newest first, skipping a group whose head was pruned, which can't be served from its first
	// frame.
	for (start, path) in around {
		let object = match load(shared, index, start, &path, timed).await {
			Ok(object) => object,
			Err(ended) => return ended,
		};
		let found = object
			.entries
			.iter()
			.enumerate()
			.rev()
			.filter(|(position, _)| *position > 0 || object.frame_start == 0)
			.find(|(_, entry)| !entry.frames.is_empty() && entry.sequence <= pts)
			.map(|(_, entry)| entry.sequence);
		if let Some(sequence) = found {
			return Search::Found(sequence);
		}
	}
	Search::None
}

/// The object a search reads, or how the search ends without it.
async fn load<S: ObjectStore>(
	shared: &Shared<S>,
	index: &Mutex<Index>,
	start: Start,
	path: &object_store::path::Path,
	timed: bool,
) -> std::result::Result<Arc<Object>, Search> {
	let err = match super::object(shared, path, start, timed).await {
		Ok(object) => return Ok(object),
		// Pruned since it was listed.
		Err(Error::NotFound(_)) => match index::pruned(shared.store.inner(), index, start).await {
			Ok(()) => return Err(Search::Pruned),
			Err(err) => err,
		},
		Err(err) => err,
	};
	tracing::warn!(%path, %err, "archived object is unavailable for a time lookup");
	Err(Search::Failed)
}

/// Answer `fill`'s `key` with group `sequence` from the objects holding it, drop the run of
/// sequences around it that the archive does not hold, or refuse it for now when the store
/// fails. The group is the key's own, or one below it containing a time start.
async fn serve_group<S: ObjectStore>(
	shared: &Shared<S>,
	index: &Mutex<Index>,
	fill: &FillGroups,
	key: u64,
	sequence: u64,
) {
	let frame = fill.frame_start(sequence);
	let (holding, after, timed, timescale) = {
		let index = index.lock().unwrap();
		// A timed group is read from its first object even for a tail: one whose stored
		// timestamps decrease anywhere is refused, which only the whole stored prefix shows.
		// An untimed one's frames all share its key, so a tail needs only what holds it.
		let timed = index.timed();
		let (holding, after) = index.holding(sequence, if timed { 0 } else { frame });
		(holding, after, timed, index.timescale)
	};
	let next = after.map_or(u64::MAX, |start| start.0);

	let mut objects = Vec::with_capacity(holding.len());
	for (start, path) in &holding {
		let load = async {
			let loaded = super::object(shared, path, *start, timed).await;
			if let Err(Error::NotFound(_)) = loaded {
				// Pruned since it was listed, and likely not alone.
				if let Err(err) = index::pruned(shared.store.inner(), index, *start).await {
					tracing::warn!(%path, %err, "archived track is unavailable to list");
				}
			}
			loaded
		};
		let unwanted = std::pin::pin!(fill.unwanted(key));
		let loaded = match future::select(unwanted, std::pin::pin!(load)).await {
			// Every reader narrowed past it: the fill moves on to the groups it still owes.
			Either::Left(_) => return,
			Either::Right((loaded, _)) => loaded,
		};
		match loaded {
			Ok(object) => objects.push(object),
			Err(Error::NotFound(_)) => {
				fill.drop_groups(sequence..sequence + 1);
				return;
			}
			Err(err) => {
				// The store still lists it, so a later reader asks again.
				tracing::warn!(%path, sequence, %err, "archived group is unavailable");
				let err = moq_net::Error::Stream(moq_net::StreamError::Internal);
				fill.refuse_groups(key..key + 1, err);
				return;
			}
		}
	}

	let Some(first) = objects.first() else {
		// Older than the oldest stored object: pruned, or never stored.
		fill.drop_groups(0..next);
		return;
	};
	let Some(position) = first.entries.iter().position(|entry| entry.sequence >= sequence) else {
		// A gap after the object's last entry.
		let last = first.entries.last().map_or(0, |entry| entry.sequence + 1);
		fill.drop_groups(last..next);
		return;
	};
	let found = &first.entries[position];
	if found.sequence != sequence {
		// A gap between two entries.
		let gap = match position {
			0 => sequence,
			_ => first.entries[position - 1].sequence + 1,
		};
		fill.drop_groups(gap..found.sequence);
		return;
	}
	let start = match position {
		0 => first.frame_start,
		_ => 0,
	};
	// Its head was pruned. A timed group's timestamps then can't be shown never to decrease,
	// even for a tail that starts past it; an untimed one's all share its key.
	if start > frame || (timed && start > 0) {
		fill.drop_groups(sequence..sequence + 1);
		return;
	}

	// The group's frames, with their indices, across every object holding it.
	let mut stored: Vec<(u64, &Frame)> = (start..).zip(&found.frames).collect();
	for object in &objects[1..] {
		let next = start + stored.len() as u64;
		match object.entries.first() {
			Some(entry) if entry.sequence == sequence && object.frame_start == next => {
				stored.extend((next..).zip(&entry.frames));
			}
			_ => {
				// A continuation went missing, so the group can't be served whole.
				fill.drop_groups(sequence..sequence + 1);
				return;
			}
		}
	}

	if !fill.is_wanted(key) {
		return;
	}
	// Checked from the group's first stored frame on, skipped ones included, so a read
	// starting mid-group refuses what a whole read does, as the live feed does: a group starts
	// at its frame 0, and its timestamps never decrease.
	let mut latest = None;
	for (index, stored) in &stored {
		let Ok(timestamp) = timestamp(stored.timestamp, timescale, sequence) else {
			break;
		};
		if *index == 0 && timestamp.value() != sequence {
			tracing::warn!(sequence, "archived group's first frame is not at its key, refusing it");
			fill.refuse_groups(key..key + 1, moq_net::Error::TimestampMismatch);
			return;
		}
		if decreases(timestamp, latest) {
			tracing::warn!(sequence, "archived group's timestamps decrease, refusing it");
			fill.refuse_groups(key..key + 1, moq_net::Error::TimestampMismatch);
			return;
		}
		latest = Some(timestamp);
	}
	let Ok(Some(mut producer)) = fill.accept(sequence, None) else {
		return;
	};
	for (index, stored) in stored {
		if index < frame {
			continue;
		}
		let written = timestamp(stored.timestamp, timescale, sequence)
			.map_err(|_| moq_net::Error::ProtocolViolation)
			.and_then(|timestamp| producer.write_frame(timestamp, stored.payload.clone()));
		if let Err(err) = written {
			tracing::warn!(sequence, %err, "archived group is malformed");
			let _ = producer.abort(err);
			return;
		}
	}
	let _ = producer.finish();
}

/// The served properties of a track stored with `info`: no max age, since the archive's
/// retention bounds what it holds and the source's would expire archived history.
pub(super) fn info(info: &Info) -> Result<track::Info> {
	Ok(track::Info::default()
		.with_priority(info.priority)
		.with_max_age(None)
		.with_timescale(timescale(info)?))
}

/// A stored track's timescale, `None` when untimed.
pub(super) fn timescale(info: &Info) -> Result<Option<Timescale>> {
	info.timescale
		.map(|timescale| Timescale::new(timescale).map_err(|_| Error::Timescale(timescale)))
		.transpose()
}

/// Whether a stored frame's timestamp is below the newest before it in its group. An archive
/// recorded before groups refused that, or from an unchecked lite-05/06 copy, may hold one;
/// its group is refused rather than replayed with times the transport cannot carry.
fn decreases(timestamp: Timestamp, latest: Option<Timestamp>) -> bool {
	latest.is_some_and(|latest| timestamp < latest)
}

/// A stored timestamp on its track, which must be timed exactly when the frame is.
fn timestamp(value: Option<u64>, timescale: Option<Timescale>, sequence: u64) -> Result<Timestamp> {
	match (value, timescale) {
		(Some(value), Some(timescale)) => Timestamp::new(value, timescale).map_err(|_| Error::Id(value)),
		// A scale-less frame's timestamp is its group's key.
		(None, None) => Ok(Timestamp::unscaled(sequence)),
		_ => Err(Error::Timing),
	}
}
