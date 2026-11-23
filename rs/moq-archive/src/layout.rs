//! The keys and objects of the store layout a [`Producer`](crate::Producer) writes.
//!
//! See the [crate docs](crate#layout) for the layout itself.

use bytes::{Buf, BufMut, Bytes, BytesMut};
use moq_net::track::Position;
use moq_net::varint;
use object_store::path::Path;
use serde::{Deserialize, Serialize};

use crate::path::{check_id, decode_track, encode_track, push};
use crate::{Error, ID_MAX, ID_WIDTH, Result, VERSION};

/// The broadcast's completion marker, beside its track segments.
pub(crate) const BROADCAST_END: &str = ".end";
/// A track's properties, which sort before every object key.
pub(crate) const TRACK_INFO: &str = ".info";
/// A track's completion marker, which sorts after every object key.
pub(crate) const TRACK_END: &str = "end";

/// A track's directory under a broadcast prefix: its name as one canonical segment.
pub(crate) fn track_dir(prefix: &Path, track: &str) -> Result<Path> {
	push(prefix, &encode_track(track)?)
}

/// What one entry under a track's directory is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Name {
	/// `.info`
	Info,
	/// `end`
	End,
	/// An object, named by where its content starts.
	Object(Start),
}

impl Name {
	/// Parse the last segment of a key under a track's directory, refusing anything else.
	pub(crate) fn parse(name: &str) -> Result<Self> {
		match name {
			TRACK_INFO => Ok(Self::Info),
			TRACK_END => Ok(Self::End),
			name => Ok(Self::Object(Start::parse(name)?)),
		}
	}
}

/// Decode a track directory's segment back into its name, refusing a non-canonical one.
pub(crate) fn parse_track(segment: &str) -> Result<String> {
	decode_track(segment)
}

/// Where an object's content starts, which is its key: its first group's start, and the index
/// of the first frame within that group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Start {
	pub group: u64,
	pub frame: u64,
}

impl Start {
	/// `<group>-<frame>`, zero-padded so keys sort in content order.
	pub(crate) fn name(&self) -> Result<String> {
		Ok(format!("{}-{}", id(self.group)?, id(self.frame)?))
	}

	fn parse(name: &str) -> Result<Self> {
		let invalid = || Error::Path(name.to_string());
		let (group, frame) = name.split_once('-').ok_or_else(invalid)?;
		Ok(Self {
			group: parse_id(group).ok_or_else(invalid)??,
			frame: parse_id(frame).ok_or_else(invalid)??,
		})
	}
}

fn id(value: u64) -> Result<String> {
	Ok(format!("{:0width$}", check_id(value)?, width = ID_WIDTH))
}

/// `None` when the field is not exactly [`ID_WIDTH`] digits.
fn parse_id(field: &str) -> Option<Result<u64>> {
	if field.len() != ID_WIDTH || !field.bytes().all(|b| b.is_ascii_digit()) {
		return None;
	}
	Some(
		field
			.parse()
			.map_err(|_| Error::Path(field.to_string()))
			.and_then(check_id),
	)
}

/// How a track's last run ended, stored in its `end` once the broadcast ended. An unknown kind
/// is refused, since skipping it would misreport how the track ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum End {
	/// A clean end at this exclusive position.
	Finish(At),
	/// The stream error code it ended with.
	Error(u32),
}

/// A [`Position`] as stored, ignoring members it does not know.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct At {
	pub group: u64,
	pub frame: u64,
}

impl From<Position> for At {
	fn from(position: Position) -> Self {
		Self {
			group: position.group,
			frame: position.frame,
		}
	}
}

impl End {
	/// Refused when its position is one the archive can't replay, so a producer fails writing it
	/// rather than leaving a marker every later read refuses.
	pub(crate) fn encode(&self) -> Result<Bytes> {
		self.validate()?;
		Ok(Bytes::from(serde_json::to_vec(self)?))
	}

	pub(crate) fn decode(bytes: &[u8]) -> Result<Self> {
		let end: Self = serde_json::from_slice(bytes)?;
		end.validate()?;
		Ok(end)
	}

	/// A finish within the IDs, or just past the last group an ID names.
	fn validate(&self) -> Result<()> {
		match *self {
			Self::Finish(At { group, frame: 0 }) if group == ID_MAX + 1 => Ok(()),
			Self::Finish(At { group, frame }) => check_id(group).and(check_id(frame)).map(drop),
			Self::Error(_) => Ok(()),
		}
	}
}

/// One frame's timestamp and original payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Frame {
	/// Absolute timestamp in the track's timescale, or `None` on an untimed track.
	pub timestamp: Option<u64>,
	/// Original frame payload, including any group-scoped compression.
	pub payload: Bytes,
}

/// One stored group: all of its frames, or the run of them this object holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Entry {
	pub sequence: u64,
	pub frames: Vec<Frame>,
}

/// One object: entries in ascending sequence order, the first starting at `frame_start`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Object {
	/// The index of the first entry's first stored frame.
	pub frame_start: u64,
	pub entries: Vec<Entry>,
}

impl Object {
	/// The key this object is stored under.
	pub(crate) fn start(&self) -> Result<Start> {
		let first = self.entries.first().ok_or(Error::Empty)?;
		Ok(Start {
			group: first.sequence,
			frame: self.frame_start,
		})
	}

	/// The first stored frame's timestamp, `None` when empty or untimed.
	pub(crate) fn timestamp(&self) -> Option<u64> {
		self.entries.first()?.frames.first()?.timestamp
	}

	/// Its payload bytes plus a small per-frame overhead, for a byte-bounded cache.
	pub(crate) fn weight(&self) -> u64 {
		let frames = self.entries.iter().flat_map(|entry| &entry.frames);
		frames.map(|frame| frame.payload.len() as u64 + 32).sum::<u64>() + 64
	}

	/// The position just past the last stored frame.
	pub(crate) fn end(&self) -> Result<Position> {
		let last = self.entries.last().ok_or(Error::Empty)?;
		let first = match self.entries.len() {
			1 => self.frame_start,
			_ => 0,
		};
		Ok(Position::new(
			last.sequence,
			first.checked_add(last.frames.len() as u64).ok_or(Error::Overflow)?,
		))
	}

	/// The version, the first frame index, then each entry's sequence (the first absolute, the
	/// rest `current - previous - 1`) and frame table, followed by every payload in order.
	pub(crate) fn encode(&self, timed: bool) -> Result<Bytes> {
		self.validate(timed)?;

		let mut table = BytesMut::new();
		write(&mut table, VERSION)?;
		write(&mut table, self.frame_start)?;
		write(&mut table, self.entries.len() as u64)?;

		let mut payload = BytesMut::new();
		let mut prev: Option<u64> = None;
		for entry in &self.entries {
			let delta = match prev {
				None => entry.sequence,
				Some(prev) => entry.sequence - prev - 1,
			};
			prev = Some(entry.sequence);
			write(&mut table, delta)?;
			write(&mut table, entry.frames.len() as u64)?;
			for frame in &entry.frames {
				if let Some(timestamp) = frame.timestamp {
					write(&mut table, timestamp)?;
				}
				write(&mut table, frame.payload.len() as u64)?;
				payload.extend_from_slice(&frame.payload);
			}
		}

		table.extend_from_slice(&payload);
		Ok(table.freeze())
	}

	/// Decode a whole table before slicing any payload; `timed` is whether the track's `.info`
	/// declares a timescale, which says whether each frame stores a timestamp.
	pub(crate) fn decode(mut buf: impl Buf, timed: bool) -> Result<Self> {
		let version = read(&mut buf)?;
		if version != VERSION {
			return Err(Error::Version(version));
		}
		let frame_start = read(&mut buf)?;
		// Each entry is at least a sequence and a frame count.
		let count = read_count(&mut buf, 2)?;

		let per_frame = if timed { 2 } else { 1 };
		let mut lengths = Vec::new();
		let mut raw = Vec::with_capacity(count);
		let mut prev: Option<u64> = None;
		for _ in 0..count {
			let delta = read(&mut buf)?;
			let sequence = match prev {
				None => delta,
				Some(prev) => prev
					.checked_add(1)
					.and_then(|next| next.checked_add(delta))
					.ok_or(Error::Overflow)?,
			};
			check_id(sequence)?;
			prev = Some(sequence);
			let frames = read_count(&mut buf, per_frame)?;
			let mut timestamps = Vec::with_capacity(frames);
			for _ in 0..frames {
				timestamps.push(match timed {
					true => Some(check_id(read(&mut buf)?)?),
					false => None,
				});
				lengths.push(read(&mut buf)?);
			}
			raw.push((sequence, timestamps));
		}

		let mut payload = buf.copy_to_bytes(buf.remaining());
		let total = lengths
			.iter()
			.try_fold(0u64, |sum, len| sum.checked_add(*len))
			.ok_or(Error::Overflow)?;
		if total != payload.len() as u64 {
			return Err(Error::Table);
		}

		let mut lengths = lengths.into_iter();
		let entries = raw
			.into_iter()
			.map(|(sequence, timestamps)| Entry {
				sequence,
				frames: timestamps
					.into_iter()
					.map(|timestamp| {
						let len = lengths.next().expect("one length per frame") as usize;
						Frame {
							timestamp,
							payload: payload.split_to(len),
						}
					})
					.collect(),
			})
			.collect();

		let object = Self { frame_start, entries };
		object.validate(timed)?;
		Ok(object)
	}

	fn validate(&self, timed: bool) -> Result<()> {
		if self.entries.is_empty() {
			return Err(Error::Empty);
		}
		check_id(self.frame_start)?;
		let mut prev = None;
		for entry in &self.entries {
			let sequence = check_id(entry.sequence)?;
			if prev.is_some_and(|prev| sequence <= prev) {
				return Err(Error::Sequence);
			}
			prev = Some(sequence);
			if entry.frames.is_empty() {
				return Err(Error::Empty);
			}
			for frame in &entry.frames {
				match frame.timestamp {
					Some(timestamp) if timed => {
						check_id(timestamp)?;
					}
					None if !timed => {}
					_ => return Err(Error::Timing),
				}
			}
		}
		Ok(())
	}
}

fn write(buf: &mut impl BufMut, value: u64) -> Result<()> {
	varint::encode_quic(value, buf).map_err(|_| Error::Overflow)
}

fn read(mut buf: &mut (impl Buf + ?Sized)) -> Result<u64> {
	varint::decode_quic(&mut buf).map_err(|_| Error::Table)
}

/// A count of entries at least `min` bytes each, refused when the rest of the buffer can't hold it.
fn read_count(buf: &mut impl Buf, min: usize) -> Result<usize> {
	let count = usize::try_from(read(buf)?).map_err(|_| Error::Overflow)?;
	if count > buf.remaining() / min {
		return Err(Error::Table);
	}
	Ok(count)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::ID_MAX;

	fn frame(timestamp: Option<u64>, payload: &'static [u8]) -> Frame {
		Frame {
			timestamp,
			payload: Bytes::from_static(payload),
		}
	}

	#[test]
	fn keys_sort_in_content_order() {
		let names: Vec<String> = [(0, 0), (0, 3), (1, 0), (10, 0), (10, 11)]
			.into_iter()
			.map(|(group, frame)| Start { group, frame }.name().unwrap())
			.collect();
		let mut sorted = names.clone();
		sorted.sort();
		assert_eq!(names, sorted);
		assert_eq!(names[1], "0000000000000000000-0000000000000000003");
		assert!(
			names
				.iter()
				.all(|name| name.as_str() < TRACK_END && name.as_str() > TRACK_INFO)
		);
	}

	#[test]
	fn names_roundtrip() {
		let start = Start {
			group: ID_MAX,
			frame: 2,
		};
		let name = start.name().unwrap();
		assert_eq!(Name::parse(&name).unwrap(), Name::Object(start));

		assert_eq!(Name::parse(".info").unwrap(), Name::Info);
		assert_eq!(Name::parse("end").unwrap(), Name::End);
		for bad in [
			"7",
			"segments",
			"0000000000000000007",
			"0000000000000000007-x",
			"0009007199254740992-0000000000000000000",
			// Main builds before the format shipped added the first frame's timestamp.
			"0000000000000000007-0000000000000000002-0000000000000000009",
		] {
			assert!(
				matches!(Name::parse(bad), Err(Error::Path(_)) | Err(Error::Id(_))),
				"{bad}"
			);
		}
	}

	#[test]
	fn end_markers_are_json() {
		let finish = End::Finish(At { group: 5, frame: 3 });
		assert_eq!(
			finish.encode().unwrap().as_ref(),
			br#"{"finish":{"group":5,"frame":3}}"#
		);
		assert_eq!(End::decode(&finish.encode().unwrap()).unwrap(), finish);
		let error = End::Error(0x34);
		assert_eq!(error.encode().unwrap().as_ref(), br#"{"error":52}"#);
		assert_eq!(End::decode(br#"{"error":52}"#).unwrap(), error);
		assert!(End::decode(br#"{"finish":{"group":5}}"#).is_err());
		assert!(End::decode(br#"{"pause":{"group":5,"frame":3}}"#).is_err());
		// A member a later writer adds to the position is skipped.
		assert_eq!(
			End::decode(br#"{"finish":{"group":5,"frame":3,"at":1}}"#).unwrap(),
			finish
		);
	}

	#[test]
	fn groups_roundtrip() {
		let object = Object {
			frame_start: 4,
			entries: vec![
				Entry {
					sequence: 3,
					frames: vec![frame(Some(10), b"a"), frame(Some(9), b"bb")],
				},
				Entry {
					sequence: 7,
					frames: vec![frame(Some(12), b"ccc")],
				},
			],
		};
		let bytes = object.encode(true).unwrap();
		assert_eq!(Object::decode(bytes.clone(), true).unwrap(), object);
		assert!(Object::decode(bytes, false).is_err());
		assert_eq!(object.start().unwrap(), Start { group: 3, frame: 4 });
		assert_eq!(object.timestamp(), Some(10));
		assert_eq!(object.end().unwrap(), Position::new(7, 1));

		let split = Object {
			frame_start: 4,
			entries: vec![object.entries[0].clone()],
		};
		assert_eq!(split.end().unwrap(), Position::new(3, 6));
	}

	#[test]
	fn an_untimed_object_stores_no_timestamps() {
		let object = Object {
			frame_start: 0,
			entries: vec![Entry {
				sequence: 2,
				frames: vec![frame(None, b"x"), frame(None, b"yz")],
			}],
		};
		let bytes = object.encode(false).unwrap();
		// version, frame start, one entry: sequence 2 with two frames, then the payloads.
		assert_eq!(bytes.as_ref(), &[VERSION as u8, 0, 1, 2, 2, 1, 2, b'x', b'y', b'z']);
		assert_eq!(Object::decode(bytes.clone(), false).unwrap(), object);
		assert!(matches!(object.encode(true), Err(Error::Timing)));
	}

	#[test]
	fn malformed_objects_are_refused() {
		let empty = Object::default();
		assert!(matches!(empty.encode(true), Err(Error::Empty)));

		let descending = Object {
			frame_start: 0,
			entries: vec![
				Entry {
					sequence: 5,
					frames: vec![frame(None, b"a")],
				},
				Entry {
					sequence: 5,
					frames: vec![frame(None, b"b")],
				},
			],
		};
		assert!(matches!(descending.encode(false), Err(Error::Sequence)));

		let no_frames = Object {
			frame_start: 0,
			entries: vec![Entry {
				sequence: 5,
				frames: Vec::new(),
			}],
		};
		assert!(matches!(no_frames.encode(false), Err(Error::Empty)));

		let good = Object {
			frame_start: 0,
			entries: vec![Entry {
				sequence: 1,
				frames: vec![frame(None, b"abc")],
			}],
		}
		.encode(false)
		.unwrap();
		// Truncated payload, trailing bytes, and an absurd count.
		assert!(Object::decode(&good[..good.len() - 1], false).is_err());
		let mut trailing = good.to_vec();
		trailing.push(0);
		assert!(Object::decode(&trailing[..], false).is_err());
		assert!(matches!(
			Object::decode(&[VERSION as u8, 0, 0x7f][..], false),
			Err(Error::Table)
		));
		// Release wrote version 1, and main builds before the format shipped wrote 3 and 4.
		for version in [1, 3, 4] {
			assert!(matches!(
				Object::decode(&[version, 0, 0][..], false),
				Err(Error::Version(v)) if v == version as u64
			));
		}

		// A first frame index past the format's IDs.
		let mut past = Object {
			frame_start: ID_MAX,
			entries: vec![Entry {
				sequence: 1,
				frames: vec![frame(None, b"abc")],
			}],
		};
		let at_max = past.encode(false).unwrap();
		assert_eq!(Object::decode(at_max, false).unwrap(), past);
		past.frame_start = ID_MAX + 1;
		assert!(matches!(past.encode(false), Err(Error::Id(_))));
		let mut table = BytesMut::new();
		for value in [VERSION, ID_MAX + 1, 1, 1, 1, 3] {
			write(&mut table, value).unwrap();
		}
		table.extend_from_slice(b"abc");
		assert!(matches!(Object::decode(table.freeze(), false), Err(Error::Id(_))));
	}

	/// An end position past the format's IDs is refused, so replaying it can't overflow.
	#[test]
	fn end_positions_are_ids() {
		let at = |group: u64, frame: u64| format!(r#"{{"finish":{{"group":{group},"frame":{frame}}}}}"#);
		// Within the IDs, and the clean end just past a last group at `ID_MAX`.
		for (group, frame) in [(ID_MAX, ID_MAX), (ID_MAX + 1, 0)] {
			let end = End::decode(at(group, frame).as_bytes()).unwrap();
			assert_eq!(End::decode(&end.encode().unwrap()).unwrap(), end);
		}
		// Refused both ways, so a producer never writes a marker a replay refuses.
		for (group, frame) in [(ID_MAX + 1, 1), (ID_MAX + 2, 0), (0, ID_MAX + 1), (u64::MAX, 1)] {
			assert!(matches!(End::decode(at(group, frame).as_bytes()), Err(Error::Id(_))));
			let end = End::Finish(At { group, frame });
			assert!(matches!(end.encode(), Err(Error::Id(_))), "{end:?}");
		}
	}
}
