//! On-disk byte format for the cache's disk and remote tiers.
//!
//! A *segment* is one band of groups ([`super::Batch`]) serialized as a single self-describing
//! object: the group blobs back to back, then a footer holding a per-group offset table, then an
//! 8-byte trailer (footer length + magic). Because the trailer is last and fixed-size, a reader
//! can fetch it with one tail-ranged GET, parse the footer, then fetch just the byte range of the
//! group it wants. Each group blob is itself self-delimiting (frame count, then length-prefixed
//! frames carrying their optional media timestamp), so frames round-trip losslessly.
//!
//! `rollup` concatenates several small segments into one larger object, rewriting the offset
//! table. It copies group blobs verbatim (no frame re-encoding), so it is cheap and lossless; it
//! is how the disk tier compacts into one remote object.

use bytes::{Buf, BufMut, Bytes, BytesMut};

use super::{Frame, Group};
use crate::{DecodeError, EncodeError, Timescale, Timestamp, VarInt};

/// Magic trailer identifying a cache segment ("MOQS").
const MAGIC: u32 = 0x4D4F_5153;

/// Fixed trailer size: a little-endian u32 footer length followed by the u32 magic.
const TRAILER: usize = 8;

/// An error decoding or encoding a [`Segment`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	/// The data is shorter than a declared length or the trailer.
	#[error("segment truncated")]
	Truncated,
	/// The trailing magic did not match, so this is not a cache segment.
	#[error("bad segment magic")]
	BadMagic,
	/// A varint or field failed to decode.
	#[error(transparent)]
	Decode(#[from] DecodeError),
	/// A varint failed to encode.
	#[error(transparent)]
	Encode(#[from] EncodeError),
	/// A value (varint or timestamp) was out of the representable range.
	#[error("value out of range")]
	Value,
}

/// One row of a segment's footer: where a group lives and its summary, without decoding the blob.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct GroupEntry {
	/// The group's sequence number within its track.
	pub sequence: u64,
	/// Byte offset of the group blob within the segment.
	pub offset: u64,
	/// Byte length of the group blob.
	pub length: u64,
	/// Number of frames in the group.
	pub frames: u64,
	/// Media timestamp of the group's first frame, if any.
	pub ts_first: Option<Timestamp>,
	/// Media timestamp of the group's last frame, if any.
	pub ts_last: Option<Timestamp>,
}

/// Serialize a band of groups into one segment.
pub fn encode(batch: &[Group]) -> Result<Bytes, Error> {
	let mut buf = BytesMut::new();
	let mut entries = Vec::with_capacity(batch.len());

	for group in batch {
		let offset = buf.len() as u64;
		put_group(&mut buf, group)?;
		let length = buf.len() as u64 - offset;
		entries.push(GroupEntry {
			sequence: group.sequence,
			offset,
			length,
			frames: group.frames.len() as u64,
			ts_first: group.ts_first(),
			ts_last: group.ts_last(),
		});
	}

	write_footer(&mut buf, &entries)?;
	Ok(buf.freeze())
}

/// Concatenate several segments into one, rewriting offsets. Group blobs are copied verbatim, so
/// this is lossless and does not re-encode frames. Entries keep their original order across the
/// inputs (segments are expected to cover disjoint, ascending sequence ranges).
pub fn rollup(segments: &[Bytes]) -> Result<Bytes, Error> {
	let mut buf = BytesMut::new();
	let mut entries = Vec::new();

	for bytes in segments {
		let segment = Segment::open(bytes.clone())?;
		for entry in segment.entries() {
			let blob = segment.blob(entry)?;
			let offset = buf.len() as u64;
			buf.extend_from_slice(&blob);
			entries.push(GroupEntry {
				offset,
				..entry.clone()
			});
		}
	}

	write_footer(&mut buf, &entries)?;
	Ok(buf.freeze())
}

/// A parsed segment: the raw bytes plus its decoded footer. Cheap to clone (the bytes are shared).
#[derive(Clone)]
pub struct Segment {
	data: Bytes,
	entries: Vec<GroupEntry>,
}

impl Segment {
	/// Parse a segment from its full bytes. Reads the trailer, validates the magic, and decodes
	/// the footer; group blobs are decoded lazily by [`group`](Self::group).
	pub fn open(data: Bytes) -> Result<Self, Error> {
		let n = data.len();
		if n < TRAILER {
			return Err(Error::Truncated);
		}

		let trailer = &data[n - TRAILER..];
		let footer_len = u32::from_le_bytes(trailer[0..4].try_into().expect("4 bytes")) as usize;
		let magic = u32::from_le_bytes(trailer[4..8].try_into().expect("4 bytes"));
		if magic != MAGIC {
			return Err(Error::BadMagic);
		}

		let footer_end = n - TRAILER;
		let footer_start = footer_end.checked_sub(footer_len).ok_or(Error::Truncated)?;
		let entries = read_footer(data.slice(footer_start..footer_end))?;

		Ok(Self { data, entries })
	}

	/// The footer's offset table.
	pub fn entries(&self) -> &[GroupEntry] {
		&self.entries
	}

	/// Number of groups in the segment.
	pub fn len(&self) -> usize {
		self.entries.len()
	}

	/// Whether the segment holds no groups.
	pub fn is_empty(&self) -> bool {
		self.entries.is_empty()
	}

	/// Total size of the segment object in bytes (blobs, footer, and trailer).
	pub fn byte_len(&self) -> usize {
		self.data.len()
	}

	/// Decode the group with this sequence, or `None` if the segment does not contain it.
	pub fn group(&self, sequence: u64) -> Option<Result<Group, Error>> {
		let entry = self.entries.iter().find(|e| e.sequence == sequence)?;
		Some(self.blob(entry).and_then(|b| group_from_blob(entry.sequence, b)))
	}

	/// Decode the group at the given footer index.
	pub fn group_at(&self, index: usize) -> Option<Result<Group, Error>> {
		let entry = self.entries.get(index)?;
		Some(self.blob(entry).and_then(|b| group_from_blob(entry.sequence, b)))
	}

	/// The raw blob bytes for an entry, bounds-checked against the data.
	fn blob(&self, entry: &GroupEntry) -> Result<Bytes, Error> {
		let start = entry.offset as usize;
		let end = start.checked_add(entry.length as usize).ok_or(Error::Truncated)?;
		if end > self.data.len() {
			return Err(Error::Truncated);
		}
		Ok(self.data.slice(start..end))
	}
}

/// Decode one group from just its blob bytes and known sequence.
///
/// This is the ranged-read decode path: the disk/remote tier reads `[offset, offset+length)` for
/// a group (from the index) and decodes those bytes without the surrounding segment or footer.
pub fn group_from_blob(sequence: u64, mut blob: Bytes) -> Result<Group, Error> {
	let count = get_varint(&mut blob)? as usize;
	let mut frames = Vec::with_capacity(count.min(8192));
	for _ in 0..count {
		frames.push(get_frame(&mut blob)?);
	}
	Ok(Group { sequence, frames })
}

fn put_group(buf: &mut BytesMut, group: &Group) -> Result<(), Error> {
	put_varint(buf, group.frames.len() as u64)?;
	for frame in &group.frames {
		put_frame(buf, frame)?;
	}
	Ok(())
}

fn put_frame(buf: &mut BytesMut, frame: &Frame) -> Result<(), Error> {
	put_varint(buf, frame.payload.len() as u64)?;
	let flags = u8::from(frame.timestamp.is_some());
	buf.put_u8(flags);
	if let Some(ts) = frame.timestamp {
		put_timestamp(buf, ts)?;
	}
	buf.extend_from_slice(&frame.payload);
	Ok(())
}

fn get_frame(buf: &mut Bytes) -> Result<Frame, Error> {
	let len = get_varint(buf)? as usize;
	let flags = get_u8(buf)?;
	let timestamp = if flags & 1 != 0 {
		Some(get_timestamp(buf)?)
	} else {
		None
	};
	if buf.remaining() < len {
		return Err(Error::Truncated);
	}
	let payload = buf.copy_to_bytes(len);
	Ok(Frame { timestamp, payload })
}

fn write_footer(buf: &mut BytesMut, entries: &[GroupEntry]) -> Result<(), Error> {
	let start = buf.len();
	put_varint(buf, entries.len() as u64)?;
	for entry in entries {
		put_varint(buf, entry.sequence)?;
		put_varint(buf, entry.offset)?;
		put_varint(buf, entry.length)?;
		put_varint(buf, entry.frames)?;
		let flags = u8::from(entry.ts_first.is_some()) | (u8::from(entry.ts_last.is_some()) << 1);
		buf.put_u8(flags);
		if let Some(ts) = entry.ts_first {
			put_timestamp(buf, ts)?;
		}
		if let Some(ts) = entry.ts_last {
			put_timestamp(buf, ts)?;
		}
	}
	let footer_len = (buf.len() - start) as u32;
	buf.put_u32_le(footer_len);
	buf.put_u32_le(MAGIC);
	Ok(())
}

fn read_footer(mut body: Bytes) -> Result<Vec<GroupEntry>, Error> {
	let count = get_varint(&mut body)? as usize;
	let mut entries = Vec::with_capacity(count.min(65536));
	for _ in 0..count {
		let sequence = get_varint(&mut body)?;
		let offset = get_varint(&mut body)?;
		let length = get_varint(&mut body)?;
		let frames = get_varint(&mut body)?;
		let flags = get_u8(&mut body)?;
		let ts_first = if flags & 1 != 0 {
			Some(get_timestamp(&mut body)?)
		} else {
			None
		};
		let ts_last = if flags & 2 != 0 {
			Some(get_timestamp(&mut body)?)
		} else {
			None
		};
		entries.push(GroupEntry {
			sequence,
			offset,
			length,
			frames,
			ts_first,
			ts_last,
		});
	}
	Ok(entries)
}

fn put_timestamp(buf: &mut BytesMut, ts: Timestamp) -> Result<(), Error> {
	// Store the raw (value, scale) so any timescale (e.g. 90kHz video) round-trips exactly.
	put_varint(buf, ts.value())?;
	put_varint(buf, ts.scale().as_u64())?;
	Ok(())
}

fn get_timestamp(buf: &mut impl Buf) -> Result<Timestamp, Error> {
	let value = get_varint(buf)?;
	let scale = get_varint(buf)?;
	let scale = Timescale::try_from(scale).map_err(|_| Error::Value)?;
	Timestamp::new(value, scale).map_err(|_| Error::Value)
}

fn put_varint(buf: &mut BytesMut, value: u64) -> Result<(), Error> {
	VarInt::try_from(value).map_err(|_| Error::Value)?.encode_quic(buf)?;
	Ok(())
}

fn get_varint(buf: &mut impl Buf) -> Result<u64, Error> {
	Ok(VarInt::decode_quic(buf)?.into())
}

fn get_u8(buf: &mut impl Buf) -> Result<u8, Error> {
	if buf.remaining() < 1 {
		return Err(Error::Truncated);
	}
	Ok(buf.get_u8())
}

#[cfg(test)]
mod tests {
	use super::*;

	fn ts(value: u64, scale: u64) -> Timestamp {
		Timestamp::from_scale(value, scale).unwrap()
	}

	fn frame(payload: &[u8], timestamp: Option<Timestamp>) -> Frame {
		Frame {
			timestamp,
			payload: Bytes::copy_from_slice(payload),
		}
	}

	fn group(sequence: u64, frames: Vec<Frame>) -> Group {
		Group { sequence, frames }
	}

	/// A small group whose frames carry 90kHz timestamps (a non-micro scale).
	fn video_group(sequence: u64, base: u64) -> Group {
		group(
			sequence,
			vec![
				frame(b"keyframe", Some(ts(base, 90_000))),
				frame(b"delta", Some(ts(base + 3000, 90_000))),
			],
		)
	}

	#[test]
	fn round_trip_single_group() {
		let g = video_group(7, 0);
		let bytes = encode(std::slice::from_ref(&g)).unwrap();
		let segment = Segment::open(bytes).unwrap();

		assert_eq!(segment.len(), 1);
		let decoded = segment.group(7).unwrap().unwrap();
		assert_eq!(decoded, g);
	}

	#[test]
	fn round_trip_batch_and_entries() {
		let batch = vec![video_group(0, 0), video_group(1, 6000), video_group(2, 12000)];
		let bytes = encode(&batch).unwrap();
		let segment = Segment::open(bytes).unwrap();

		assert_eq!(segment.len(), 3);
		// Footer summarizes each group.
		for (entry, g) in segment.entries().iter().zip(&batch) {
			assert_eq!(entry.sequence, g.sequence);
			assert_eq!(entry.frames, g.frames.len() as u64);
			assert_eq!(entry.ts_first, g.ts_first());
			assert_eq!(entry.ts_last, g.ts_last());
		}
		// Every group decodes back to the original, by sequence and by index.
		for (i, g) in batch.iter().enumerate() {
			assert_eq!(&segment.group(g.sequence).unwrap().unwrap(), g);
			assert_eq!(&segment.group_at(i).unwrap().unwrap(), g);
		}
	}

	#[test]
	fn timestamps_lossless_at_any_scale() {
		// A 90kHz tick is not an integer number of micros; raw (value, scale) must survive.
		let bytes = encode(&[video_group(0, 1)]).unwrap();
		let segment = Segment::open(bytes).unwrap();
		let decoded = segment.group(0).unwrap().unwrap();

		let t = decoded.frames[0].timestamp.unwrap();
		assert_eq!(t.value(), 1);
		assert_eq!(t.scale().as_u64(), 90_000);
	}

	#[test]
	fn mixed_and_absent_timestamps() {
		let g = group(
			3,
			vec![
				frame(b"a", None),
				frame(b"b", Some(ts(500, 1_000_000))),
				frame(b"c", None),
			],
		);
		let bytes = encode(std::slice::from_ref(&g)).unwrap();
		let segment = Segment::open(bytes).unwrap();
		assert_eq!(segment.group(3).unwrap().unwrap(), g);
		// ts_first is absent (first frame), ts_last is absent (last frame).
		assert_eq!(segment.entries()[0].ts_first, None);
		assert_eq!(segment.entries()[0].ts_last, None);
	}

	#[test]
	fn empty_group_and_empty_batch() {
		// A group with no frames, and a segment with no groups, both round-trip.
		let g = group(9, vec![]);
		let segment = Segment::open(encode(std::slice::from_ref(&g)).unwrap()).unwrap();
		assert_eq!(segment.group(9).unwrap().unwrap(), g);

		let empty = Segment::open(encode(&[]).unwrap()).unwrap();
		assert!(empty.is_empty());
		assert!(empty.group(0).is_none());
	}

	#[test]
	fn missing_sequence_is_none() {
		let segment = Segment::open(encode(&[video_group(5, 0)]).unwrap()).unwrap();
		assert!(segment.group(6).is_none());
		assert!(segment.group_at(1).is_none());
	}

	#[test]
	fn bad_magic_is_rejected() {
		let mut bytes = encode(&[video_group(0, 0)]).unwrap().to_vec();
		let n = bytes.len();
		bytes[n - 1] ^= 0xFF; // corrupt the magic
		assert!(matches!(Segment::open(Bytes::from(bytes)), Err(Error::BadMagic)));
	}

	#[test]
	fn truncated_is_rejected() {
		let bytes = encode(&[video_group(0, 0)]).unwrap();
		// Drop the trailer entirely.
		assert!(Segment::open(bytes.slice(0..4)).is_err());
		// Keep the trailer but lie about the footer length by chopping the middle.
		let short = bytes.slice(0..bytes.len() - TRAILER - 1);
		assert!(matches!(
			Segment::open(short),
			Err(Error::Truncated) | Err(Error::BadMagic)
		));
	}

	#[test]
	fn rollup_concatenates_and_preserves_groups() {
		let first = encode(&[video_group(0, 0), video_group(1, 6000)]).unwrap();
		let second = encode(&[video_group(2, 12000), video_group(3, 18000)]).unwrap();

		let rolled = rollup(&[first, second]).unwrap();
		let segment = Segment::open(rolled).unwrap();

		// All four groups present, in order, decoding identically to the originals.
		assert_eq!(segment.len(), 4);
		let expected = [
			video_group(0, 0),
			video_group(1, 6000),
			video_group(2, 12000),
			video_group(3, 18000),
		];
		for (i, g) in expected.iter().enumerate() {
			assert_eq!(&segment.group_at(i).unwrap().unwrap(), g);
			assert_eq!(&segment.group(g.sequence).unwrap().unwrap(), g);
		}

		// Offsets are rewritten to be ascending and non-overlapping in the merged object.
		let entries = segment.entries();
		for pair in entries.windows(2) {
			assert!(pair[1].offset >= pair[0].offset + pair[0].length);
		}
	}

	#[test]
	fn rollup_of_one_segment_round_trips() {
		let batch = vec![video_group(0, 0), video_group(1, 6000)];
		let single = encode(&batch).unwrap();
		let rolled = Segment::open(rollup(std::slice::from_ref(&single)).unwrap()).unwrap();
		for g in &batch {
			assert_eq!(&rolled.group(g.sequence).unwrap().unwrap(), g);
		}
	}

	#[test]
	fn rollup_rejects_corrupt_input() {
		let good = encode(&[video_group(0, 0)]).unwrap();
		let bad = Bytes::from_static(b"not a segment!!!");
		assert!(rollup(&[good, bad]).is_err());
	}
}
