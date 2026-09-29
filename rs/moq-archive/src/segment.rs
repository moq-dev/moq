use bytes::{Buf, BufMut, Bytes, BytesMut};
use hang::timeline::Position;
use moq_net::VarInt;

use crate::path::check_id;
use crate::{Error, Result, VERSION};

/// One group's stored frames in a segment object, in sequence order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
	/// Group sequence number.
	pub sequence: u64,
	/// Frames in their original order within the group.
	pub frames: Vec<Frame>,
}

/// One frame's timestamp and original payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
	/// Absolute timestamp in the track's timescale.
	pub timestamp: u64,
	/// Original frame payload, including any group-scoped compression.
	pub payload: Bytes,
}

/// A versioned group/frame table followed by concatenated payload bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Object {
	/// The index of the first group's first stored frame within that group; later groups start at
	/// frame zero.
	pub frame_start: u64,
	/// Groups in strictly ascending sequence order.
	pub groups: Vec<Group>,
}

impl Object {
	/// An object whose first group starts at frame zero.
	pub fn new(groups: Vec<Group>) -> Self {
		Self { frame_start: 0, groups }
	}

	/// The position of the first stored frame.
	pub fn start(&self) -> Result<Position> {
		validate(&self.groups)?;
		Ok(Position::new(self.groups[0].sequence, self.frame_start))
	}

	/// Require this object to hold exactly the frames `start..end` of a record, with no group
	/// missing or empty.
	pub fn check_span(&self, start: Position, end: Position) -> Result<()> {
		if self.start()? != start {
			return Err(Error::Span);
		}
		for (expected, group) in (start.group..).zip(&self.groups) {
			if group.sequence != expected || group.frames.is_empty() {
				return Err(Error::Span);
			}
		}
		let last = self.groups.last().expect("validated as non-empty");
		let first = match self.groups.len() {
			1 => self.frame_start,
			_ => 0,
		};
		let stop = first.checked_add(last.frames.len() as u64).ok_or(Error::Overflow)?;
		let exact = match end.frame {
			0 => end.group == last.sequence + 1,
			frame => end.group == last.sequence && frame == stop,
		};
		if !exact {
			return Err(Error::Span);
		}
		Ok(())
	}

	/// Encode the binary envelope. The first sequence is absolute; later ones are `current - previous - 1`.
	pub fn encode(&self) -> Result<Bytes> {
		validate(&self.groups)?;

		let mut table = BytesMut::new();
		write_varint(&mut table, VERSION)?;
		write_varint(&mut table, self.frame_start)?;
		write_varint(&mut table, self.groups.len() as u64)?;

		let mut payload = BytesMut::new();
		let mut prev = None;
		for group in &self.groups {
			let delta = match prev {
				None => group.sequence,
				Some(previous) => group
					.sequence
					.checked_sub(previous)
					.ok_or(Error::Overflow)?
					.checked_sub(1)
					.ok_or(Error::Overflow)?,
			};
			prev = Some(group.sequence);
			write_varint(&mut table, delta)?;
			write_varint(&mut table, group.frames.len() as u64)?;
			for frame in &group.frames {
				let offset = u64::try_from(payload.len()).map_err(|_| Error::Overflow)?;
				let length = u64::try_from(frame.payload.len()).map_err(|_| Error::Overflow)?;
				write_varint(&mut table, frame.timestamp)?;
				write_varint(&mut table, offset)?;
				write_varint(&mut table, length)?;
				payload.extend_from_slice(&frame.payload);
			}
		}

		table.extend_from_slice(&payload);
		Ok(table.freeze())
	}

	/// Decode a complete table before slicing any payload, and refuse an unknown version.
	pub fn decode(mut buf: impl Buf) -> Result<Self> {
		let version = read_varint(&mut buf)?;
		if version != VERSION {
			return Err(Error::Version(version));
		}

		let frame_start = read_varint(&mut buf)?;
		let group_count = read_count(&mut buf, 2)?;
		if group_count == 0 {
			return Err(Error::Empty);
		}

		struct Entry {
			sequence: u64,
			frames: Vec<(u64, u64, u64)>,
		}

		let mut entries = Vec::new();
		let mut prev: Option<u64> = None;
		for i in 0..group_count {
			let delta = read_varint(&mut buf)?;
			let sequence = if i == 0 {
				check_id(delta)?
			} else {
				let previous = prev.unwrap();
				let sequence = previous
					.checked_add(1)
					.ok_or(Error::Overflow)?
					.checked_add(delta)
					.ok_or(Error::Overflow)?;
				check_id(sequence)?
			};
			if let Some(previous) = prev
				&& sequence <= previous
			{
				return Err(Error::Sequence);
			}
			prev = Some(sequence);

			let frame_count = read_count(&mut buf, 3)?;
			let mut frames = Vec::new();
			for _ in 0..frame_count {
				let timestamp = check_id(read_varint(&mut buf)?)?;
				let offset = read_varint(&mut buf)?;
				let length = read_varint(&mut buf)?;
				frames.push((timestamp, offset, length));
			}
			entries.push(Entry { sequence, frames });
		}

		let payload = buf.copy_to_bytes(buf.remaining());
		let payload_len = u64::try_from(payload.len()).map_err(|_| Error::Overflow)?;

		let mut expected = 0u64;
		let mut groups = Vec::with_capacity(entries.len());
		for entry in entries {
			let mut frames = Vec::with_capacity(entry.frames.len());
			for (timestamp, offset, length) in entry.frames {
				if offset != expected {
					return Err(Error::Table);
				}
				let end = offset.checked_add(length).ok_or(Error::Overflow)?;
				if end > payload_len {
					return Err(Error::Table);
				}
				let start = usize::try_from(offset).map_err(|_| Error::Overflow)?;
				let stop = usize::try_from(end).map_err(|_| Error::Overflow)?;
				frames.push(Frame {
					timestamp,
					payload: payload.slice(start..stop),
				});
				expected = end;
			}
			groups.push(Group {
				sequence: entry.sequence,
				frames,
			});
		}
		if expected != payload_len {
			return Err(Error::Table);
		}

		Ok(Self { frame_start, groups })
	}
}

fn validate(groups: &[Group]) -> Result<()> {
	if groups.is_empty() {
		return Err(Error::Empty);
	}
	let mut prev = None;
	for group in groups {
		check_id(group.sequence)?;
		if let Some(previous) = prev
			&& group.sequence <= previous
		{
			return Err(Error::Sequence);
		}
		prev = Some(group.sequence);
		for frame in &group.frames {
			check_id(frame.timestamp)?;
		}
	}
	Ok(())
}

fn write_varint(buf: &mut impl BufMut, value: u64) -> Result<()> {
	let value = VarInt::try_from(value).map_err(|_| Error::Overflow)?;
	value.encode_quic(buf).map_err(|_| Error::Overflow)
}

fn read_varint(buf: &mut impl Buf) -> Result<u64> {
	Ok(VarInt::decode_quic(buf).map_err(|_| Error::Table)?.into_inner())
}

fn read_count(buf: &mut impl Buf, min_entry: usize) -> Result<usize> {
	let n = read_varint(buf)?;
	let n = usize::try_from(n).map_err(|_| Error::Overflow)?;
	if min_entry == 0 || n > buf.remaining() / min_entry {
		return Err(Error::Table);
	}
	Ok(n)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::ID_MAX;

	fn frame(timestamp: u64, payload: &'static [u8]) -> Frame {
		Frame {
			timestamp,
			payload: Bytes::from_static(payload),
		}
	}

	fn object(groups: Vec<Group>) -> Object {
		Object::new(groups)
	}

	fn group(sequence: u64, frames: usize) -> Group {
		Group {
			sequence,
			frames: (0..frames as u64).map(|i| frame(i, b"x")).collect(),
		}
	}

	#[test]
	fn roundtrip_consecutive_and_sparse() {
		let original = object(vec![
			Group {
				sequence: 0,
				frames: vec![frame(0, b"a"), frame(1, b"bb")],
			},
			Group {
				sequence: 1,
				frames: vec![frame(2, b"ccc")],
			},
			Group {
				sequence: 4,
				frames: vec![frame(ID_MAX, b"d")],
			},
		]);
		let bytes = original.encode().unwrap();
		assert_eq!(Object::decode(&bytes[..]).unwrap(), original);
		assert_eq!(original.start().unwrap(), Position::group(0));

		let split = Object {
			frame_start: 300,
			..original
		};
		assert_eq!(Object::decode(split.encode().unwrap()).unwrap(), split);
		assert_eq!(split.start().unwrap(), Position::new(0, 300));
	}

	#[test]
	fn first_id_is_absolute_then_minus_one_deltas() {
		let bytes = object(vec![
			Group {
				sequence: 5,
				frames: vec![frame(0, b"x")],
			},
			Group {
				sequence: 6,
				frames: vec![frame(1, b"y")],
			},
			Group {
				sequence: 10,
				frames: vec![frame(2, b"z")],
			},
		])
		.encode()
		.unwrap();

		let mut buf = &bytes[..];
		assert_eq!(read_varint(&mut buf).unwrap(), 2); // version
		assert_eq!(read_varint(&mut buf).unwrap(), 0); // frame start
		assert_eq!(read_varint(&mut buf).unwrap(), 3); // group count
		assert_eq!(read_varint(&mut buf).unwrap(), 5); // absolute
		assert_eq!(read_varint(&mut buf).unwrap(), 1); // one frame
		read_varint(&mut buf).unwrap(); // timestamp
		read_varint(&mut buf).unwrap(); // offset
		read_varint(&mut buf).unwrap(); // length
		assert_eq!(read_varint(&mut buf).unwrap(), 0); // consecutive
		assert_eq!(read_varint(&mut buf).unwrap(), 1);
		read_varint(&mut buf).unwrap();
		read_varint(&mut buf).unwrap();
		read_varint(&mut buf).unwrap();
		assert_eq!(read_varint(&mut buf).unwrap(), 3); // 10 - 6 - 1
	}

	#[test]
	fn empty_object_is_rejected() {
		assert!(matches!(object(vec![]).encode(), Err(Error::Empty)));
		let mut bytes = BytesMut::new();
		write_varint(&mut bytes, VERSION).unwrap();
		write_varint(&mut bytes, 0).unwrap();
		write_varint(&mut bytes, 0).unwrap();
		assert!(matches!(Object::decode(bytes.freeze()), Err(Error::Empty)));
	}

	#[test]
	fn sequences_must_ascend() {
		assert!(matches!(
			object(vec![
				Group {
					sequence: 2,
					frames: vec![frame(0, b"a")],
				},
				Group {
					sequence: 2,
					frames: vec![frame(1, b"b")],
				},
			])
			.encode(),
			Err(Error::Sequence)
		));
		assert!(matches!(
			object(vec![
				Group {
					sequence: 2,
					frames: vec![frame(0, b"a")],
				},
				Group {
					sequence: 1,
					frames: vec![frame(1, b"b")],
				},
			])
			.encode(),
			Err(Error::Sequence)
		));
	}

	#[test]
	fn reconstructed_ids_must_stay_in_range() {
		let mut bytes = BytesMut::new();
		write_varint(&mut bytes, VERSION).unwrap();
		write_varint(&mut bytes, 0).unwrap();
		write_varint(&mut bytes, 2).unwrap();
		write_varint(&mut bytes, ID_MAX).unwrap();
		write_varint(&mut bytes, 0).unwrap(); // no frames
		write_varint(&mut bytes, 0).unwrap(); // consecutive => ID_MAX + 1
		write_varint(&mut bytes, 0).unwrap();
		assert!(matches!(Object::decode(bytes.freeze()), Err(Error::Id(_))));
	}

	/// A table with one frameless group per delta.
	fn deltas(deltas: &[u64]) -> Bytes {
		let mut bytes = BytesMut::new();
		write_varint(&mut bytes, VERSION).unwrap();
		write_varint(&mut bytes, 0).unwrap();
		write_varint(&mut bytes, deltas.len() as u64).unwrap();
		for &delta in deltas {
			write_varint(&mut bytes, delta).unwrap();
			write_varint(&mut bytes, 0).unwrap();
		}
		bytes.freeze()
	}

	fn sequences(bytes: Bytes) -> Result<Vec<u64>> {
		Ok(Object::decode(bytes)?
			.groups
			.iter()
			.map(|group| group.sequence)
			.collect())
	}

	#[test]
	fn delta_extremes() {
		let varint = (1u64 << 62) - 1;
		assert_eq!(sequences(deltas(&[0, 0, 0])).unwrap(), vec![0, 1, 2]);
		assert_eq!(sequences(deltas(&[0, 9, 0])).unwrap(), vec![0, 10, 11]);
		assert_eq!(sequences(deltas(&[0, ID_MAX - 1])).unwrap(), vec![0, ID_MAX]);
		assert_eq!(sequences(deltas(&[ID_MAX])).unwrap(), vec![ID_MAX]);
		assert!(matches!(sequences(deltas(&[0, ID_MAX])), Err(Error::Id(_))));
		assert!(matches!(sequences(deltas(&[ID_MAX + 1])), Err(Error::Id(_))));
		assert!(matches!(sequences(deltas(&[varint])), Err(Error::Id(_))));
		assert!(matches!(sequences(deltas(&[ID_MAX, varint])), Err(Error::Id(_))));
	}

	#[test]
	fn timestamp_bounds() {
		assert!(
			object(vec![Group {
				sequence: 0,
				frames: vec![frame(ID_MAX, b"a")],
			}])
			.encode()
			.is_ok()
		);
		assert!(matches!(
			object(vec![Group {
				sequence: 0,
				frames: vec![frame(ID_MAX + 1, b"a")],
			}])
			.encode(),
			Err(Error::Id(_))
		));
	}

	#[test]
	fn unknown_version_is_refused() {
		// Version 1 objects held whole groups under range-named keys.
		for version in [1, 3] {
			let mut bytes = BytesMut::new();
			write_varint(&mut bytes, version).unwrap();
			write_varint(&mut bytes, 1).unwrap();
			assert!(matches!(Object::decode(bytes.freeze()), Err(Error::Version(v)) if v == version));
		}
	}

	#[test]
	fn payload_must_be_contiguous() {
		let mut good = object(vec![Group {
			sequence: 0,
			frames: vec![frame(0, b"ab")],
		}])
		.encode()
		.unwrap()
		.to_vec();
		good.push(b'x');
		assert!(matches!(Object::decode(Bytes::from(good)), Err(Error::Table)));
	}

	#[test]
	fn advertised_counts_must_fit_minimum_entry_size() {
		// group_count equals remaining bytes, so a remaining-bytes check would
		// accept it, but each group needs at least two varints.
		let mut bytes = BytesMut::new();
		write_varint(&mut bytes, VERSION).unwrap();
		write_varint(&mut bytes, 0).unwrap();
		write_varint(&mut bytes, 4).unwrap();
		bytes.extend_from_slice(&[0, 0, 0, 0]);
		assert!(matches!(Object::decode(bytes.freeze()), Err(Error::Table)));
	}

	#[test]
	fn offset_overflow_and_truncated_table() {
		let mut bytes = BytesMut::new();
		for value in [VERSION, 0, 1, 0, 1, 0, 0, 4] {
			write_varint(&mut bytes, value).unwrap(); // length 4, no payload
		}
		assert!(matches!(Object::decode(bytes.freeze()), Err(Error::Table)));

		assert!(matches!(Object::decode(&b"\x02"[..]), Err(Error::Table)));
	}

	#[test]
	fn frame_offsets_must_tile_the_payload() {
		// One group of two frames at (offset, length), followed by `payload` bytes.
		let table = |frames: [(u64, u64); 2], payload: usize| {
			let mut bytes = BytesMut::new();
			for value in [VERSION, 0, 1, 0, 2] {
				write_varint(&mut bytes, value).unwrap();
			}
			for (offset, length) in frames {
				for value in [0, offset, length] {
					write_varint(&mut bytes, value).unwrap();
				}
			}
			bytes.extend_from_slice(&vec![b'x'; payload]);
			Object::decode(bytes.freeze())
		};
		assert!(table([(0, 1), (1, 2)], 3).is_ok());
		assert!(matches!(table([(0, 1), (0, 2)], 3), Err(Error::Table)), "overlap");
		assert!(matches!(table([(0, 1), (2, 1)], 3), Err(Error::Table)), "gap");
		assert!(matches!(table([(1, 1), (2, 1)], 3), Err(Error::Table)), "late start");
		assert!(matches!(table([(0, 1), (1, 3)], 3), Err(Error::Table)), "past the end");
		let varint = (1u64 << 62) - 1;
		assert!(
			matches!(table([(0, 1), (1, varint)], 3), Err(Error::Table)),
			"huge length"
		);
	}

	#[test]
	fn a_span_must_match_the_record() {
		let whole = object(vec![group(5, 2), group(6, 1)]);
		whole.check_span(Position::group(5), Position::group(7)).unwrap();
		assert_eq!(
			whole.check_span(Position::group(5), Position::group(8)),
			Err(Error::Span)
		);
		assert_eq!(
			whole.check_span(Position::group(4), Position::group(7)),
			Err(Error::Span)
		);
		assert_eq!(
			whole.check_span(Position::group(5), Position::new(6, 2)),
			Err(Error::Span),
			"a partial end names its frame count"
		);

		// A frame split: frames 3..5 of group 7.
		let split = Object {
			frame_start: 3,
			groups: vec![group(7, 2)],
		};
		split.check_span(Position::new(7, 3), Position::new(7, 5)).unwrap();
		assert_eq!(
			split.check_span(Position::new(7, 3), Position::new(7, 4)),
			Err(Error::Span)
		);

		// A split's tail continues into whole groups.
		let tail = Object {
			frame_start: 3,
			groups: vec![group(7, 2), group(8, 4)],
		};
		tail.check_span(Position::new(7, 3), Position::new(8, 4)).unwrap();
		tail.check_span(Position::new(7, 3), Position::group(9)).unwrap();

		// Gaps and empty groups never appear inside a record.
		let gap = object(vec![group(5, 1), group(7, 1)]);
		assert_eq!(gap.check_span(Position::group(5), Position::group(8)), Err(Error::Span));
		let empty = object(vec![group(5, 1), group(6, 0)]);
		assert_eq!(
			empty.check_span(Position::group(5), Position::group(7)),
			Err(Error::Span)
		);
	}

	#[test]
	fn endpoints_roundtrip() {
		let original = object(vec![Group {
			sequence: ID_MAX,
			frames: vec![frame(0, b"")],
		}]);
		assert_eq!(Object::decode(original.encode().unwrap()).unwrap(), original);
	}
}
