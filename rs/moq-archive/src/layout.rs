//! The keys and objects of the store layout a [`Producer`](crate::Producer) writes.
//!
//! See the [crate docs](crate#layout) for the layout itself.

use bytes::{Buf, BufMut, Bytes, BytesMut};
use moq_net::track::Position;
use moq_net::varint;
use object_store::path::Path;
use serde::{Deserialize, Serialize};

use crate::path::{check_id, decode_track, encode_track, push};
use crate::{Error, ID_WIDTH, Result, VERSION, VERSION_MIN};

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
	pub(crate) fn parse(name: &str, timed: bool) -> Result<Self> {
		match name {
			TRACK_INFO => Ok(Self::Info),
			TRACK_END => Ok(Self::End),
			name => Ok(Self::Object(Start::parse(name, timed)?)),
		}
	}
}

/// Decode a track directory's segment back into its name, refusing a non-canonical one.
pub(crate) fn parse_track(segment: &str) -> Result<String> {
	decode_track(segment)
}

/// Where an object's content starts, which is its key: the first sequence, the first frame's
/// index within it, and, on a timed track, that frame's timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Start {
	pub group: u64,
	pub frame: u64,
	pub pts: Option<u64>,
}

impl Start {
	/// `<group>-<frame>-<pts>` on a timed track, `<group>-<frame>` on an untimed one,
	/// zero-padded so keys sort in content order.
	pub(crate) fn name(&self) -> Result<String> {
		let (group, frame) = (id(self.group)?, id(self.frame)?);
		match self.pts {
			Some(pts) => Ok(format!("{group}-{frame}-{}", id(pts)?)),
			None => Ok(format!("{group}-{frame}")),
		}
	}

	fn parse(name: &str, timed: bool) -> Result<Self> {
		let fields: Vec<&str> = name.split('-').collect();
		let invalid = || Error::Path(name.to_string());
		match (timed, fields.as_slice()) {
			(true, [group, frame, pts]) => Ok(Self {
				group: parse_id(group).ok_or_else(invalid)??,
				frame: parse_id(frame).ok_or_else(invalid)??,
				pts: Some(parse_id(pts).ok_or_else(invalid)??),
			}),
			(false, [group, frame]) => Ok(Self {
				group: parse_id(group).ok_or_else(invalid)??,
				frame: parse_id(frame).ok_or_else(invalid)??,
				pts: None,
			}),
			_ => Err(invalid()),
		}
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

/// How a track's last run ended, stored in its `end` once the broadcast ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
pub(crate) enum End {
	/// A clean end at this exclusive position.
	Finish(At),
	/// The stream error code it ended with.
	Error(u32),
}

/// A [`Position`] as stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
	pub(crate) fn encode(&self) -> Result<Bytes> {
		Ok(Bytes::from(serde_json::to_vec(self)?))
	}

	pub(crate) fn decode(bytes: &[u8]) -> Result<Self> {
		Ok(serde_json::from_slice(bytes)?)
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

/// The form field version 2 and 3 entries carry: 0 for a group, 1 for a datagram.
const LEGACY_GROUP: u64 = 0;

impl Object {
	/// The key this object is stored under.
	pub(crate) fn start(&self) -> Result<Start> {
		let first = self.entries.first().ok_or(Error::Empty)?;
		Ok(Start {
			group: first.sequence,
			frame: self.frame_start,
			pts: first.frames.first().ok_or(Error::Empty)?.timestamp,
		})
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
		Ok(Position {
			group: last.sequence,
			frame: first.checked_add(last.frames.len() as u64).ok_or(Error::Overflow)?,
		})
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
	/// declares a timescale, which says whether each frame stores a timestamp. A version 2 or 3
	/// object's entries each carry a form, and one holding a datagram is refused: an archive
	/// replays groups only, and skipping the entry would read as a gap the recording never had.
	pub(crate) fn decode(mut buf: impl Buf, timed: bool) -> Result<Self> {
		let version = read(&mut buf)?;
		if !(VERSION_MIN..=VERSION).contains(&version) {
			return Err(Error::Version(version));
		}
		let legacy = version < 4;
		let frame_start = read(&mut buf)?;
		// Each entry is at least a sequence and a frame count, plus a form when legacy.
		let count = read_count(&mut buf, if legacy { 3 } else { 2 })?;

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
			if legacy {
				let form = read(&mut buf)?;
				if form != LEGACY_GROUP {
					return Err(Error::Form(form));
				}
			}
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
	fn timed_keys_sort_in_content_order() {
		let names: Vec<String> = [(0, 0, 9), (0, 3, 2), (1, 0, 0), (10, 0, 0)]
			.into_iter()
			.map(|(group, frame, pts)| {
				Start {
					group,
					frame,
					pts: Some(pts),
				}
				.name()
				.unwrap()
			})
			.collect();
		let mut sorted = names.clone();
		sorted.sort();
		assert_eq!(names, sorted);
		assert_eq!(names[1], "0000000000000000000-0000000000000000003-0000000000000000002");
		assert!(
			names
				.iter()
				.all(|name| name.as_str() < TRACK_END && name.as_str() > TRACK_INFO)
		);
	}

	#[test]
	fn names_roundtrip_and_refuse_the_other_timing() {
		let timed = Start {
			group: 5,
			frame: 2,
			pts: Some(ID_MAX),
		};
		let name = timed.name().unwrap();
		assert_eq!(Name::parse(&name, true).unwrap(), Name::Object(timed));
		assert!(matches!(Name::parse(&name, false), Err(Error::Path(_))));

		let untimed = Start {
			group: 7,
			frame: 2,
			pts: None,
		};
		let name = untimed.name().unwrap();
		assert_eq!(name, "0000000000000000007-0000000000000000002");
		assert_eq!(Name::parse(&name, false).unwrap(), Name::Object(untimed));
		assert!(matches!(Name::parse(&name, true), Err(Error::Path(_))));

		assert_eq!(Name::parse(".info", true).unwrap(), Name::Info);
		assert_eq!(Name::parse("end", false).unwrap(), Name::End);
		for bad in [
			"7",
			"segments",
			"0000000000000000007",
			"0000000000000000007-x",
			"0009007199254740992-0000000000000000000",
		] {
			assert!(Name::parse(bad, false).is_err(), "{bad}");
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
		assert_eq!(
			object.start().unwrap(),
			Start {
				group: 3,
				frame: 4,
				pts: Some(10)
			}
		);
		assert_eq!(object.end().unwrap(), Position { group: 7, frame: 1 });

		let split = Object {
			frame_start: 4,
			entries: vec![object.entries[0].clone()],
		};
		assert_eq!(split.end().unwrap(), Position { group: 3, frame: 6 });
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

	/// Version 2 and 3 entries carry a form: a group reads as today's entry, and a datagram
	/// refuses the whole object rather than reading as a gap.
	#[test]
	fn older_objects_read_groups_and_refuse_datagrams() {
		let object = Object {
			frame_start: 0,
			entries: vec![Entry {
				sequence: 2,
				frames: vec![frame(None, b"x"), frame(None, b"yz")],
			}],
		};
		for version in [2, 3] {
			let group = [version, 0, 1, 2, 0, 2, 1, 2, b'x', b'y', b'z'];
			assert_eq!(Object::decode(&group[..], false).unwrap(), object);
			let datagram = [version, 0, 1, 2, 1, 1, b'x'];
			assert!(matches!(Object::decode(&datagram[..], false), Err(Error::Form(1))));
		}
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
		for version in [1, VERSION as u8 + 1] {
			assert!(matches!(
				Object::decode(&[version, 0, 0][..], false),
				Err(Error::Version(_))
			));
		}
	}
}
