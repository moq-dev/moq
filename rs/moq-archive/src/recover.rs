//! Recover a recording so a restarted [`Writer`](crate::Writer) continues it.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::ops::RangeInclusive;

use futures::TryStreamExt;
use hang::timeline::Record;
use moq_json::window::{self, Checkpoint};
use object_store::ObjectStore;

use crate::store::list::Query;
use crate::{Error, Key, Object, Result, Store};

/// What a restarted writer continues from.
pub(crate) struct Recovery {
	/// The retained timeline window, or `None` when no timeline object exists.
	pub checkpoint: Option<Checkpoint<Record>>,
	/// The next stored timeline group sequence, so the stored numbering keeps increasing.
	pub sequence: u64,
	/// Per track, the largest stored group; new groups must exceed it so no object overlaps.
	pub floors: HashMap<String, u64>,
	/// Stored group objects no retained record references. Only collected for a DVR.
	pub orphans: Vec<Key>,
}

/// List the whole recording, then replay its timeline from a retained checkpoint.
///
/// A DVR (`complete`) replays far enough back to recover every retained record and reports the
/// unreferenced group objects; an archive only needs the newest checkpoint. Any listing, GET, or
/// decode failure fails recovery, so a caller never acts on a partial view.
pub(crate) async fn recover<S: ObjectStore>(store: &Store<S>, timeline: &str, complete: bool) -> Result<Recovery> {
	let entries: Vec<_> = store.list(&Query::new()).try_collect().await?;
	let mut segments = Vec::new();
	let mut groups = Vec::new();
	let mut floors = HashMap::new();
	for entry in entries {
		match entry.key {
			Key::Segments { track, segment } if track == timeline => segments.push(segment),
			Key::Groups { track, range } if track != timeline => {
				raise(&mut floors, &track, *range.end());
				groups.push(Key::Groups { track, range });
			}
			_ => {}
		}
	}
	segments.sort_unstable();

	let (checkpoint, sequence) = match (segments.first(), segments.last()) {
		(Some(&first), Some(&last)) => {
			if segments.len() as u64 != last - first + 1 {
				return Err(Error::Timeline(format!(
					"timeline segments {first}..={last} are not contiguous"
				)));
			}
			let (checkpoint, sequence) = replay(store, timeline, first..=last, complete).await?;
			// The newest object is segment `last`, so the window must end on the next one.
			// A shorter window would resume onto that segment and collide with it.
			let next = last.checked_add(1).ok_or(Error::Overflow)?;
			if checkpoint.range.end != next {
				return Err(Error::Timeline(format!(
					"recovered window ends at {}, not segment {next}",
					checkpoint.range.end
				)));
			}
			(Some(checkpoint), sequence)
		}
		_ => (None, 0),
	};

	let mut referenced = HashSet::new();
	for record in checkpoint.iter().flat_map(|checkpoint| &checkpoint.records) {
		for (track, ranges) in &record.tracks {
			if let (Some(first), Some(last)) = (ranges.first(), ranges.last()) {
				raise(&mut floors, track, last.end);
				referenced.insert(Key::groups(track.clone(), first.start..=last.end)?);
			}
		}
	}
	let orphans = match complete {
		true => groups.into_iter().filter(|key| !referenced.contains(key)).collect(),
		false => Vec::new(),
	};

	Ok(Recovery {
		checkpoint,
		sequence,
		floors,
		orphans,
	})
}

fn raise(floors: &mut HashMap<String, u64>, track: &str, group: u64) {
	let floor = floors.entry(track.to_string()).or_insert(group);
	*floor = (*floor).max(group);
}

/// Replay `segments` from the newest checkpoint that restates every record `complete` needs.
///
/// Returns the retained window and the next timeline group sequence.
async fn replay<S: ObjectStore>(
	store: &Store<S>,
	timeline: &str,
	segments: RangeInclusive<u64>,
	complete: bool,
) -> Result<(Checkpoint<Record>, u64)> {
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
			"cannot recover timeline window {range:?} from the retained checkpoints"
		)));
	}

	let last = objects.back().and_then(|object| object.groups.last());
	let sequence = last.map_or(Ok(0), |group| group.sequence.checked_add(1).ok_or(Error::Overflow))?;
	let checkpoint = Checkpoint {
		range,
		records: records.into_values().collect(),
	};
	Ok((checkpoint, sequence))
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
