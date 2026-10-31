//! One served track: its live feed of what the archive gains, and fills of what it holds.

use std::sync::{Arc, Mutex};

use futures::future::{self, Either};
use moq_net::track::{Fill, Position};
use moq_net::{Timescale, Timestamp, group, track};
use object_store::ObjectStore;

use super::Shared;
use super::index::Index;
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
	group: group::Producer,
	sequence: u64,
	/// The index of the next frame it takes.
	next: u64,
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
				if entry.sequence() < from {
					continue;
				}
				if self.next.is_none() {
					// Readers ask fills for everything older.
					self.producer.start_at(entry.sequence())?;
				}
				let first = match index {
					0 => object.frame_start,
					_ => 0,
				};
				self.entry(entry, first)?;
				self.next = Some(entry.sequence() + 1);
			}
		}
		Ok(())
	}

	fn entry(&mut self, entry: &Entry, first: u64) -> Result<()> {
		match entry {
			Entry::Datagram { sequence, frame } => {
				// A later sequence completes the open group.
				self.close()?;
				let timestamp = timestamp(frame.timestamp, self.timescale)?;
				self.producer
					.insert_datagram(*sequence, timestamp, frame.payload.clone())?;
			}
			Entry::Group { sequence, frames } => {
				if self.open.as_ref().is_some_and(|open| open.sequence != *sequence) {
					self.close()?;
				}
				// The group stays in `open` while it is written, so a failure aborts it with
				// the track rather than dropping it.
				let open = match &mut self.open {
					Some(open) => {
						if open.next != first {
							return Err(Error::Sequence);
						}
						open
					}
					None => {
						let group = self.producer.create_group(group::Info { sequence: *sequence })?;
						let open = self.open.insert(Open {
							group,
							sequence: *sequence,
							next: first,
						});
						if first > 0 {
							// Its head was pruned.
							open.group.start_at(first)?;
						}
						open
					}
				};
				for frame in frames {
					let timestamp = timestamp(frame.timestamp, self.timescale)?;
					open.group.write_frame(timestamp, frame.payload.clone())?;
					open.next += 1;
				}
			}
		}
		Ok(())
	}

	fn close(&mut self) -> Result<()> {
		if let Some(mut open) = self.open.take() {
			open.group.finish()?;
		}
		Ok(())
	}

	/// End the feed with `err` when serving the archive failed, the open group included, so
	/// readers see why rather than a dropped producer.
	pub fn abort(self, err: moq_net::Error) {
		if let Some(open) = self.open {
			let _ = open.group.abort(err.clone());
		}
		let _ = self.producer.abort(err);
	}

	/// End the feed as the archive recorded.
	pub fn end(&mut self, end: End) -> Result<()> {
		self.close()?;
		match end {
			End::Finish(at) => {
				self.producer.finish_at(
					Position {
						group: at.group,
						frame: at.frame,
					},
					None,
				)?;
				// An empty group stores no object, yet the end only settles once the live edge
				// reaches it. The sequence just below the end replays as the empty group a
				// source most often ended with, and any before it as dropped.
				let fin = at.group + u64::from(at.frame != 0);
				let from = self.next.unwrap_or(0);
				if from < fin {
					self.producer.drop_groups(from..fin - 1, moq_net::StreamError::NotFound);
					self.producer
						.create_group(group::Info { sequence: fin - 1 })?
						.finish()?;
				}
			}
			End::Error(code) => {
				// After the newest stored sequence, keeping the history an abort would refuse.
				let err = moq_net::StreamError::from_code(code).into();
				self.producer
					.finish_at(Position::group(self.next.unwrap_or(0)), Some(err))?;
			}
		}
		Ok(())
	}
}

/// Serve `fill` from the archive, a group at a time in the fill's order, until every group is
/// answered or nobody waits on it.
pub(super) async fn serve<S: ObjectStore>(shared: Arc<Shared<S>>, index: Arc<Mutex<Index>>, fill: Fill) {
	let mut last = None;
	while let Some(sequence) = fill.wanted()
		&& last != Some(sequence)
	{
		last = Some(sequence);
		serve_group(&shared, &index, &fill, sequence).await;
	}
}

/// Answer group `sequence` of `fill` from the objects holding it, drop the run of sequences
/// around it that the archive does not hold, or refuse it for now when the store fails.
async fn serve_group<S: ObjectStore>(shared: &Shared<S>, index: &Mutex<Index>, fill: &Fill, sequence: u64) {
	let frame = fill.frame_start(sequence);
	let (holding, after, timed, timescale) = {
		let index = index.lock().unwrap();
		let (holding, after) = index.holding(sequence, frame);
		(holding, after, index.timed(), index.timescale)
	};
	let next = after.map_or(u64::MAX, |start| start.0);

	let mut objects = Vec::with_capacity(holding.len());
	for (start, path) in &holding {
		let unwanted = std::pin::pin!(fill.unwanted(sequence));
		let load = std::pin::pin!(super::object(shared, path, *start, timed));
		let loaded = match future::select(unwanted, load).await {
			// Every reader narrowed past it: the fill moves on to the groups it still owes.
			Either::Left(_) => return,
			Either::Right((loaded, _)) => loaded,
		};
		match loaded {
			Ok(object) => objects.push(object),
			Err(Error::NotFound(_)) => {
				// Pruned since it was listed.
				index.lock().unwrap().forget(*start);
				fill.drop_groups(sequence..sequence + 1);
				return;
			}
			Err(err) => {
				// The store still lists it, so a later reader asks again.
				tracing::warn!(%path, sequence, %err, "archived group is unavailable");
				let err = moq_net::Error::Stream(moq_net::StreamError::Internal);
				fill.refuse_groups(sequence..sequence + 1, err);
				return;
			}
		}
	}

	let Some(first) = objects.first() else {
		// Older than the oldest stored object: pruned, or never stored.
		fill.drop_groups(0..next);
		return;
	};
	let Some(position) = first.entries.iter().position(|entry| entry.sequence() >= sequence) else {
		// A gap after the object's last entry.
		let last = first.entries.last().map_or(0, |entry| entry.sequence() + 1);
		fill.drop_groups(last..next);
		return;
	};
	let frames = match &first.entries[position] {
		Entry::Group {
			sequence: found,
			frames,
		} if *found == sequence => frames,
		// A datagram stays lossy and uncached, never a fetched group.
		Entry::Datagram { sequence: found, .. } if *found == sequence => {
			fill.drop_groups(sequence..sequence + 1);
			return;
		}
		// A gap between two entries.
		other => {
			let gap = match position {
				0 => sequence,
				_ => first.entries[position - 1].sequence() + 1,
			};
			fill.drop_groups(gap..other.sequence());
			return;
		}
	};
	let start = match position {
		0 => first.frame_start,
		_ => 0,
	};
	if start > frame {
		// Its head was pruned.
		fill.drop_groups(sequence..sequence + 1);
		return;
	}

	// The group's frames, with their indices, across every object holding it.
	let mut stored: Vec<(u64, &Frame)> = (start..).zip(frames).collect();
	for object in &objects[1..] {
		let next = start + stored.len() as u64;
		match object.entries.first() {
			Some(Entry::Group {
				sequence: found,
				frames,
			}) if *found == sequence && object.frame_start == next => {
				stored.extend((next..).zip(frames));
			}
			_ => {
				// A continuation went missing, so the group can't be served whole.
				fill.drop_groups(sequence..sequence + 1);
				return;
			}
		}
	}

	if !fill.is_wanted(sequence) {
		return;
	}
	let Ok(mut producer) = fill.accept(sequence, None) else {
		return;
	};
	for (index, stored) in stored {
		if index < frame {
			continue;
		}
		let written = timestamp(stored.timestamp, timescale)
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

/// A stored timestamp on its track, which must be timed exactly when the frame is.
fn timestamp(value: Option<u64>, timescale: Option<Timescale>) -> Result<Option<Timestamp>> {
	match (value, timescale) {
		(Some(value), Some(timescale)) => Ok(Some(Timestamp::new(value, timescale).map_err(|_| Error::Id(value))?)),
		(None, None) => Ok(None),
		_ => Err(Error::Timing),
	}
}
