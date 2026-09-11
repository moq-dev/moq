use bytes::{Buf, BufMut, Bytes, BytesMut};
use moq_net::VarInt;

use crate::path::check_id;
use crate::{Error, Result, VERSION};

/// One complete group in a segment object, in sequence order.
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
	/// Groups in strictly ascending sequence order.
	pub groups: Vec<Group>,
}

impl Object {
	/// Inclusive first and last group sequences.
	pub fn bounds(&self) -> Result<(u64, u64)> {
		validate(&self.groups)?;
		Ok((self.groups[0].sequence, self.groups.last().unwrap().sequence))
	}

	/// Encode the binary envelope. The first sequence is absolute; later ones are `current - previous - 1`.
	pub fn encode(&self) -> Result<Bytes> {
		validate(&self.groups)?;

		let mut table = BytesMut::new();
		write_varint(&mut table, VERSION)?;
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

		Ok(Self { groups })
	}

	/// Decode and require the table's first and last sequences to match `smallest` and `largest`.
	pub fn decode_groups(buf: impl Buf, largest: u64, smallest: u64) -> Result<Self> {
		let object = Self::decode(buf)?;
		object.check_bounds(largest, smallest)?;
		Ok(object)
	}

	/// Require this object's sequences to match a range-named key.
	pub fn check_bounds(&self, largest: u64, smallest: u64) -> Result<()> {
		let (got_smallest, got_largest) = self.bounds()?;
		if got_smallest != smallest || got_largest != largest {
			return Err(Error::Bounds {
				smallest: got_smallest,
				largest: got_largest,
			});
		}
		Ok(())
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
		Object { groups }
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
		original.check_bounds(4, 0).unwrap();
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
		assert_eq!(read_varint(&mut buf).unwrap(), 1); // version
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
		write_varint(&mut bytes, 1).unwrap();
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
		write_varint(&mut bytes, 1).unwrap();
		write_varint(&mut bytes, 2).unwrap();
		write_varint(&mut bytes, ID_MAX).unwrap();
		write_varint(&mut bytes, 0).unwrap(); // no frames
		write_varint(&mut bytes, 0).unwrap(); // consecutive => ID_MAX + 1
		write_varint(&mut bytes, 0).unwrap();
		assert!(matches!(Object::decode(bytes.freeze()), Err(Error::Id(_))));
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
		let mut bytes = BytesMut::new();
		write_varint(&mut bytes, 2).unwrap();
		write_varint(&mut bytes, 1).unwrap();
		assert!(matches!(Object::decode(bytes.freeze()), Err(Error::Version(2))));
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
		write_varint(&mut bytes, 1).unwrap();
		write_varint(&mut bytes, 4).unwrap();
		bytes.extend_from_slice(&[0, 0, 0, 0]);
		assert!(matches!(Object::decode(bytes.freeze()), Err(Error::Table)));
	}

	#[test]
	fn offset_overflow_and_truncated_table() {
		let mut bytes = BytesMut::new();
		write_varint(&mut bytes, 1).unwrap();
		write_varint(&mut bytes, 1).unwrap();
		write_varint(&mut bytes, 0).unwrap();
		write_varint(&mut bytes, 1).unwrap();
		write_varint(&mut bytes, 0).unwrap();
		write_varint(&mut bytes, 0).unwrap();
		write_varint(&mut bytes, 4).unwrap(); // length 4, no payload
		assert!(matches!(Object::decode(bytes.freeze()), Err(Error::Table)));

		assert!(matches!(Object::decode(&b"\x01"[..]), Err(Error::Table)));
	}

	#[test]
	fn filename_bounds_must_match_the_table() {
		let object = object(vec![
			Group {
				sequence: 5,
				frames: vec![frame(0, b"a")],
			},
			Group {
				sequence: 7,
				frames: vec![frame(1, b"b")],
			},
		]);
		let bytes = object.encode().unwrap();
		assert!(Object::decode_groups(&bytes[..], 7, 5).is_ok());
		assert!(matches!(
			Object::decode_groups(&bytes[..], 7, 6),
			Err(Error::Bounds {
				smallest: 5,
				largest: 7
			})
		));
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
