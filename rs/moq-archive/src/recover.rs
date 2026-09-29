//! Recover a recording so a restarted [`Writer`](crate::Writer) continues it.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::ops::RangeInclusive;

use futures::TryStreamExt;
use hang::timeline::{Position, Record};
use moq_json::window::{self, Checkpoint};
use object_store::ObjectStore;

use crate::store::list::Query;
use crate::{Error, Key, Object, Result, Store};

/// What a restarted writer continues from.
#[derive(Default)]
pub(crate) struct Recovery {
	/// Each recorded track's retained timeline, keyed by the recorded track's name.
	pub tracks: HashMap<String, Resume>,
	/// Committed objects no retained record references, such as an interrupted expiration. Only
	/// collected for a DVR, which deletes them after its grace.
	pub orphans: Vec<Key>,
	/// Objects at or past a track's next record: uploads a crash left before their commit. No
	/// timeline ever referenced them, and the resumed writer reuses their keys.
	pub uncommitted: Vec<Key>,
}

/// One track's retained timeline.
pub(crate) struct Resume {
	/// The retained window.
	pub checkpoint: Checkpoint<Record>,
	/// The next stored timeline group sequence, so the stored numbering keeps increasing.
	pub sequence: u64,
}

impl Resume {
	/// Where the newest committed record ended: the resumed track refuses anything before it.
	pub fn floor(&self) -> Option<Position> {
		self.checkpoint.records.last().map(|record| record.end)
	}
}

/// List the whole recording, then replay each track's timeline from a retained checkpoint.
///
/// A DVR (`complete`) replays far enough back to recover every retained record and reports the
/// unreferenced objects; an archive only needs the newest checkpoint. Any listing, GET, or decode
/// failure fails recovery, so a caller never acts on a partial view.
pub(crate) async fn recover<S: ObjectStore>(store: &Store<S>, complete: bool) -> Result<Recovery> {
	let entries: Vec<_> = store.list(&Query::new()).try_collect().await?;
	let mut segments: BTreeMap<String, Vec<u64>> = BTreeMap::new();
	for entry in entries {
		if let Key::Segments { track, segment } = entry.key {
			segments.entry(track).or_default().push(segment);
		}
	}

	let mut recovery = Recovery::default();
	for (timeline, ids) in &mut segments {
		let Some(track) = timeline.strip_suffix(hang::timeline::SUFFIX) else {
			continue;
		};
		ids.sort_unstable();
		let (first, last) = (ids[0], ids[ids.len() - 1]);
		if ids.len() as u64 != last - first + 1 {
			return Err(Error::Timeline(format!(
				"{timeline} segments {first}..={last} are not contiguous"
			)));
		}
		let resume = replay(store, timeline, first..=last, complete).await?;
		// The newest object is segment `last`, so the window must end on the next one. A shorter
		// window would resume onto that segment and collide with it.
		let next = last.checked_add(1).ok_or(Error::Overflow)?;
		if resume.checkpoint.range.end != next {
			return Err(Error::Timeline(format!(
				"{timeline} window ends at {}, not segment {next}",
				resume.checkpoint.range.end
			)));
		}
		recovery.tracks.insert(track.to_string(), resume);
	}

	for (track, ids) in segments {
		if track.ends_with(hang::timeline::SUFFIX) {
			continue;
		}
		let resume = recovery.tracks.get(&track);
		let next = resume.map_or(0, |resume| resume.checkpoint.range.end);
		let retained: HashSet<u64> = resume
			.iter()
			.flat_map(|resume| &resume.checkpoint.records)
			.map(|record| record.sequence)
			.collect();
		for id in ids {
			let key = Key::segments(track.clone(), id)?;
			if id >= next {
				recovery.uncommitted.push(key);
			} else if complete && !retained.contains(&id) {
				recovery.orphans.push(key);
			}
		}
	}

	Ok(recovery)
}

/// Replay `segments` from the newest checkpoint that restates every record `complete` needs.
async fn replay<S: ObjectStore>(
	store: &Store<S>,
	timeline: &str,
	segments: RangeInclusive<u64>,
	complete: bool,
) -> Result<Resume> {
	// Every object opens with a checkpoint. The newest one's offset bounds what is still retained,
	// so walk back until a checkpoint restates from there.
	let mut objects = VecDeque::new();
	let mut needed = None;
	for segment in segments.rev() {
		let object = store.get_segments(timeline, segment).await?;
		let (offset, start) = checkpoint(&object)?;
		let needed = *needed.get_or_insert(offset);
		objects.push_front(object);
		if !complete || start <= needed {
			break;
		}
	}

	let mut decoder = decoder();
	let mut records = BTreeMap::new();
	for object in &objects {
		for stored in &object.groups {
			let mut group = decoder.group();
			for frame in &stored.frames {
				group.decode(&frame.payload).map_err(json_error)?;
			}
		}
		while let Some(event) = decoder.next_event() {
			match event {
				window::Event::Push { index, value } => {
					records.insert(index, value);
				}
				window::Event::Pop(range) | window::Event::Skip(range) => {
					let tail = records.split_off(&range.end);
					records.retain(|index, _| *index < range.start);
					records.extend(tail);
				}
				_ => {}
			}
		}
	}

	// The records must be a contiguous suffix of the window, and all of it for a DVR.
	let range = decoder.range();
	let start = records.keys().next().copied().unwrap_or(range.end);
	if records.len() as u64 != range.end - start || (complete && start != range.start) {
		return Err(Error::Timeline(format!(
			"cannot recover {timeline} window {range:?} from the retained checkpoints"
		)));
	}

	let last = objects.back().and_then(|object| object.groups.last());
	let sequence = last.map_or(Ok(0), |group| group.sequence.checked_add(1).ok_or(Error::Overflow))?;
	let checkpoint = Checkpoint {
		range,
		records: records.into_values().collect(),
	};
	Ok(Resume { checkpoint, sequence })
}

/// The retained offset and first restated index of the checkpoint opening `object`.
fn checkpoint(object: &Object) -> Result<(u64, u64)> {
	let frame = object
		.groups
		.first()
		.and_then(|group| group.frames.first())
		.ok_or_else(|| Error::Timeline("timeline object has no checkpoint".into()))?;
	let mut decoder = decoder();
	decoder.group().decode(&frame.payload).map_err(json_error)?;
	let offset = decoder.range().start;
	let start = match decoder.next_event() {
		Some(window::Event::Skip(skipped)) => skipped.end,
		_ => offset,
	};
	Ok((offset, start))
}

fn decoder() -> window::Decoder<Record> {
	window::Decoder::new(window::ConsumerConfig::default().with_compression(true))
}

fn json_error(err: moq_json::Error) -> Error {
	Error::Timeline(err.to_string())
}
