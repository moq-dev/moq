//! Incremental EBML framing for the MKV importer.
//!
//! [`Reader`] walks the element tree as bytes arrive, retaining only a split element header and
//! the body of an element its caller chose to read whole. The caller answers every
//! [`Event::Start`] with how to consume that element, so a block streams through and an ignored
//! element is discarded as its bytes arrive, whatever size either declares.

use super::Error;

/// Matroska element IDs, with their length markers, as they appear on the wire.
pub(super) mod id {
	pub const EBML: u32 = 0x1A45_DFA3;
	pub const DOC_TYPE: u32 = 0x4282;

	pub const SEGMENT: u32 = 0x1853_8067;
	pub const SEEK_HEAD: u32 = 0x114D_9B74;
	pub const INFO: u32 = 0x1549_A966;
	pub const TRACKS: u32 = 0x1654_AE6B;
	pub const CLUSTER: u32 = 0x1F43_B675;
	pub const CUES: u32 = 0x1C53_BB6B;
	pub const CHAPTERS: u32 = 0x1043_A770;
	pub const TAGS: u32 = 0x1254_C367;
	pub const ATTACHMENTS: u32 = 0x1941_A469;

	pub const TIMESTAMP_SCALE: u32 = 0x2A_D7B1;

	pub const TRACK_ENTRY: u32 = 0xAE;
	pub const TRACK_NUMBER: u32 = 0xD7;
	pub const TRACK_TYPE: u32 = 0x83;
	pub const CODEC_ID: u32 = 0x86;
	pub const CODEC_PRIVATE: u32 = 0x63A2;
	pub const DEFAULT_DURATION: u32 = 0x23_E383;

	pub const VIDEO: u32 = 0xE0;
	pub const PIXEL_WIDTH: u32 = 0xB0;
	pub const PIXEL_HEIGHT: u32 = 0xBA;
	pub const DISPLAY_WIDTH: u32 = 0x54B0;
	pub const DISPLAY_HEIGHT: u32 = 0x54BA;
	pub const DISPLAY_UNIT: u32 = 0x54B2;
	pub const PIXEL_CROP_BOTTOM: u32 = 0x54AA;
	pub const PIXEL_CROP_TOP: u32 = 0x54BB;
	pub const PIXEL_CROP_LEFT: u32 = 0x54CC;
	pub const PIXEL_CROP_RIGHT: u32 = 0x54DD;

	pub const STEREO_MODE: u32 = 0x53B8;
	pub const PROJECTION: u32 = 0x7670;
	pub const PROJECTION_TYPE: u32 = 0x7671;
	pub const PROJECTION_PRIVATE: u32 = 0x7672;
	pub const PROJECTION_POSE_YAW: u32 = 0x7673;
	pub const PROJECTION_POSE_PITCH: u32 = 0x7674;
	pub const PROJECTION_POSE_ROLL: u32 = 0x7675;

	pub const COLOUR: u32 = 0x55B0;
	pub const MATRIX_COEFFICIENTS: u32 = 0x55B1;
	pub const RANGE: u32 = 0x55B9;
	pub const TRANSFER_CHARACTERISTICS: u32 = 0x55BA;
	pub const PRIMARIES: u32 = 0x55BB;
	pub const MAX_CLL: u32 = 0x55BC;
	pub const MAX_FALL: u32 = 0x55BD;
	pub const MASTERING_METADATA: u32 = 0x55D0;
	/// The first of MasteringMetadata's chromaticity and luminance fields, which are contiguous.
	pub const PRIMARY_R_CHROMATICITY_X: u32 = 0x55D1;
	/// The last of MasteringMetadata's fields.
	pub const LUMINANCE_MIN: u32 = 0x55DA;

	pub const AUDIO: u32 = 0xE1;
	pub const SAMPLING_FREQUENCY: u32 = 0xB5;
	pub const CHANNELS: u32 = 0x9F;

	pub const TIMESTAMP: u32 = 0xE7;
	pub const SIMPLE_BLOCK: u32 = 0xA3;
	pub const BLOCK_GROUP: u32 = 0xA0;
	pub const BLOCK: u32 = 0xA1;
	pub const REFERENCE_BLOCK: u32 = 0xFB;
}

/// The longest element header: a 4-byte ID (EBMLMaxIDLength) and an 8-byte size.
const MAX_HEADER: usize = 12;

/// An element's ID and where its body ends.
#[derive(Clone, Copy, Debug)]
pub(super) struct Header {
	pub id: u32,
	/// The input position one past the body, or `None` for an unknown size.
	pub end: Option<u64>,
	/// Body bytes the header declares, or `None` for an unknown size.
	pub size: Option<u64>,
}

/// One step through the element tree.
#[derive(Debug)]
pub(super) enum Event<'a> {
	/// An element starts. Answer with [`Reader::descend`], [`Reader::read`], [`Reader::stream`],
	/// or [`Reader::skip`] before the next call.
	Start(Header),
	/// A [`read`](Reader::read) element's whole body.
	Value(u32, Vec<u8>),
	/// The next bytes of a [`stream`](Reader::stream)ed element's body, borrowed from the input.
	Chunk(&'a [u8]),
	/// A descended or streamed element ended.
	End(u32),
}

struct Open {
	id: u32,
	end: Option<u64>,
}

enum Body {
	/// Between elements: the next bytes are a header.
	Between,
	/// A parsed header, until no unknown-size master it closes remains open.
	Parsed(Header),
	/// A started element awaiting the caller's choice.
	Pending(Header),
	Read {
		id: u32,
		end: u64,
		data: Vec<u8>,
	},
	Stream {
		id: u32,
		end: u64,
	},
	Skip {
		end: u64,
	},
}

/// Walks EBML elements across input chunks split anywhere.
pub(super) struct Reader {
	/// Descended masters, outermost first.
	open: Vec<Open>,
	/// Input position of the next unconsumed byte.
	position: u64,
	/// A header split across chunks, at most [`MAX_HEADER`] bytes.
	header: Vec<u8>,
	body: Body,
}

impl Reader {
	pub fn new() -> Self {
		Self {
			open: Vec::new(),
			position: 0,
			header: Vec::with_capacity(MAX_HEADER),
			body: Body::Between,
		}
	}

	/// Input bytes consumed so far.
	#[cfg(test)]
	pub fn position(&self) -> u64 {
		self.position
	}

	/// Bytes held for a header split across chunks or a read element's body.
	#[cfg(test)]
	pub fn retained(&self) -> usize {
		let body = match &self.body {
			Body::Read { data, .. } => data.capacity(),
			_ => 0,
		};
		self.header.capacity() + body
	}

	/// The innermost descended master.
	pub fn parent(&self) -> Option<u32> {
		self.open.last().map(|open| open.id)
	}

	/// Where the innermost descended master ends, if its size is known.
	pub fn parent_end(&self) -> Option<u64> {
		self.open.last().and_then(|open| open.end)
	}

	/// Where the innermost open master ends: its declared end, or an unknown-size master's nearest
	/// sized ancestor's.
	fn bound(&self) -> Option<u64> {
		self.open.iter().rev().find_map(|open| open.end)
	}

	/// Enter the started master; its children follow.
	pub fn descend(&mut self) -> Result<(), Error> {
		let header = self.take_pending();
		// Only Segment and Cluster may have an unknown size, and only they can find their end
		// without a schema of every child.
		if header.end.is_none() && !matches!(header.id, id::SEGMENT | id::CLUSTER) {
			return Err(Error::MatroskaParse);
		}
		self.open.push(Open {
			id: header.id,
			end: header.end,
		});
		Ok(())
	}

	/// Collect the started element's body into one [`Event::Value`].
	pub fn read(&mut self) -> Result<(), Error> {
		let header = self.take_pending();
		let end = header.end.ok_or(Error::MatroskaParse)?;
		self.body = Body::Read {
			id: header.id,
			end,
			data: Vec::new(),
		};
		Ok(())
	}

	/// Pass the started element's body through as [`Event::Chunk`]s, then [`Event::End`].
	pub fn stream(&mut self) -> Result<(), Error> {
		let header = self.take_pending();
		let end = header.end.ok_or(Error::MatroskaParse)?;
		self.body = Body::Stream { id: header.id, end };
		Ok(())
	}

	/// Discard the started element's body as it arrives.
	pub fn skip(&mut self) -> Result<(), Error> {
		let header = self.take_pending();
		let end = header.end.ok_or(Error::MatroskaParse)?;
		self.body = Body::Skip { end };
		Ok(())
	}

	fn take_pending(&mut self) -> Header {
		match std::mem::replace(&mut self.body, Body::Between) {
			Body::Pending(header) => header,
			_ => panic!("no started element awaits a choice"),
		}
	}

	/// The next event `input` completes, consuming the bytes it spans. `None` once `input` is
	/// exhausted mid-element or between elements.
	pub fn next<'a>(&mut self, input: &mut &'a [u8]) -> Result<Option<Event<'a>>, Error> {
		loop {
			match &mut self.body {
				Body::Between => {
					// A master ends at its declared end, and an unknown-size one at its nearest
					// sized ancestor's.
					if let Some(open) = self.open.last()
						&& self.bound() == Some(self.position)
					{
						let id = open.id;
						self.open.pop();
						return Ok(Some(Event::End(id)));
					}
					let Some(header) = self.header(input)? else {
						return Ok(None);
					};
					self.body = Body::Parsed(header);
				}
				Body::Parsed(header) => {
					let header = *header;
					// An unknown-size master ends where an element it cannot contain starts.
					if let Some(open) = self.open.last()
						&& open.end.is_none()
						&& closes(open.id, header.id)
					{
						let id = open.id;
						self.open.pop();
						return Ok(Some(Event::End(id)));
					}
					if let Some(bound) = self.bound()
						&& header.end.unwrap_or(self.position) > bound
					{
						return Err(Error::MatroskaParse);
					}
					self.body = Body::Pending(header);
					return Ok(Some(Event::Start(header)));
				}
				// A refused choice leaves the element unread, so nothing after it can be framed.
				Body::Pending(_) => return Err(Error::MatroskaParse),
				Body::Read { id, end, data } => {
					let taken = take(input, *end - self.position);
					data.extend_from_slice(taken);
					self.position += taken.len() as u64;
					if self.position < *end {
						return Ok(None);
					}
					let event = Event::Value(*id, std::mem::take(data));
					self.body = Body::Between;
					return Ok(Some(event));
				}
				Body::Stream { id, end } => {
					if self.position == *end {
						let id = *id;
						self.body = Body::Between;
						return Ok(Some(Event::End(id)));
					}
					let taken = take(input, *end - self.position);
					if taken.is_empty() {
						return Ok(None);
					}
					self.position += taken.len() as u64;
					return Ok(Some(Event::Chunk(taken)));
				}
				Body::Skip { end } => {
					let taken = take(input, *end - self.position);
					self.position += taken.len() as u64;
					if self.position < *end {
						return Ok(None);
					}
					self.body = Body::Between;
				}
			}
		}
	}

	/// The next event at the end of input: unknown-size masters close, and anything else still
	/// open was truncated.
	pub fn finish(&mut self) -> Result<Option<Event<'static>>, Error> {
		if let Some(event) = self.next(&mut &[][..])? {
			return Ok(Some(event));
		}
		if !self.header.is_empty() || !matches!(self.body, Body::Between) {
			return Err(Error::Truncated);
		}
		match self.open.pop() {
			Some(Open { id, end: None }) => Ok(Some(Event::End(id))),
			Some(Open { end: Some(_), .. }) => Err(Error::Truncated),
			None => Ok(None),
		}
	}

	/// Parse the next element header, joining one split across chunks.
	fn header(&mut self, input: &mut &[u8]) -> Result<Option<Header>, Error> {
		let start = self.position - self.header.len() as u64;
		let parsed = if self.header.is_empty() {
			let parsed = parse(input)?;
			if parsed.is_none() {
				self.header.extend_from_slice(input);
			}
			parsed
		} else {
			// Join byte by byte, so nothing past the header is consumed.
			let mut parsed = None;
			while parsed.is_none()
				&& let Some((&byte, rest)) = input.split_first()
			{
				self.header.push(byte);
				*input = rest;
				parsed = parse(&self.header)?;
			}
			parsed
		};
		let Some((id, size, len)) = parsed else {
			// The bytes held are a header prefix that needs more bytes, so it cannot reach the
			// parent's end.
			self.position = start + self.header.len() as u64;
			if let Some(bound) = self.bound()
				&& self.position >= bound
			{
				return Err(Error::MatroskaParse);
			}
			*input = &[];
			return Ok(None);
		};
		if self.header.is_empty() {
			*input = &input[len..];
		}
		self.header.clear();
		self.position = start + len as u64;
		let end = size
			.map(|size| self.position.checked_add(size).ok_or(Error::MatroskaParse))
			.transpose()?;
		Ok(Some(Header { id, end, size }))
	}
}

/// Split up to `max` bytes off the front of `input`.
fn take<'a>(input: &mut &'a [u8], max: u64) -> &'a [u8] {
	let len = usize::try_from(max).unwrap_or(usize::MAX).min(input.len());
	let (taken, rest) = input.split_at(len);
	*input = rest;
	taken
}

/// Parse an element header from the front of `buf`: its ID, its size (`None` when unknown), and
/// the header's length. `None` when `buf` holds only part of it.
fn parse(buf: &[u8]) -> Result<Option<(u32, Option<u64>, usize)>, Error> {
	let Some(&first) = buf.first() else {
		return Ok(None);
	};
	// IDs keep their length marker, and are at most 4 bytes (EBMLMaxIDLength).
	let id_len = first.leading_zeros() as usize + 1;
	if id_len > 4 {
		return Err(Error::MatroskaParse);
	}
	let Some(id) = buf.get(..id_len) else {
		return Ok(None);
	};
	let id = id.iter().fold(0u32, |id, byte| (id << 8) | u32::from(*byte));

	let Some(&first) = buf.get(id_len) else {
		return Ok(None);
	};
	let size_len = first.leading_zeros() as usize + 1;
	if size_len > 8 {
		return Err(Error::MatroskaParse);
	}
	let Some(size) = buf.get(id_len..id_len + size_len) else {
		return Ok(None);
	};
	let mut value = u64::from(first) & (0xff >> size_len);
	for byte in &size[1..] {
		value = (value << 8) | u64::from(*byte);
	}
	// Every value bit set marks an unknown size, at any length.
	let unknown = value == (1u64 << (7 * size_len)) - 1;
	Ok(Some((id, (!unknown).then_some(value), id_len + size_len)))
}

/// Whether an element with `child`'s ID ends the unknown-size master `parent`.
fn closes(parent: u32, child: u32) -> bool {
	match parent {
		id::CLUSTER => matches!(
			child,
			id::EBML
				| id::SEGMENT
				| id::SEEK_HEAD
				| id::INFO | id::TRACKS
				| id::CLUSTER
				| id::CUES | id::CHAPTERS
				| id::TAGS | id::ATTACHMENTS
		),
		id::SEGMENT => matches!(child, id::EBML | id::SEGMENT),
		_ => false,
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use ebml_iterable::specs::EbmlTag;
	use webm_iterable::matroska_spec::{Master, MatroskaSpec};

	#[test]
	fn ids_match_the_matroska_spec() {
		let expected = [
			(id::EBML, MatroskaSpec::Ebml(Master::Start)),
			(id::DOC_TYPE, MatroskaSpec::DocType(String::new())),
			(id::SEGMENT, MatroskaSpec::Segment(Master::Start)),
			(id::SEEK_HEAD, MatroskaSpec::SeekHead(Master::Start)),
			(id::INFO, MatroskaSpec::Info(Master::Start)),
			(id::TRACKS, MatroskaSpec::Tracks(Master::Start)),
			(id::CLUSTER, MatroskaSpec::Cluster(Master::Start)),
			(id::CUES, MatroskaSpec::Cues(Master::Start)),
			(id::CHAPTERS, MatroskaSpec::Chapters(Master::Start)),
			(id::TAGS, MatroskaSpec::Tags(Master::Start)),
			(id::ATTACHMENTS, MatroskaSpec::Attachments(Master::Start)),
			(id::TIMESTAMP_SCALE, MatroskaSpec::TimestampScale(0)),
			(id::TRACK_ENTRY, MatroskaSpec::TrackEntry(Master::Start)),
			(id::TRACK_NUMBER, MatroskaSpec::TrackNumber(0)),
			(id::TRACK_TYPE, MatroskaSpec::TrackType(0)),
			(id::CODEC_ID, MatroskaSpec::CodecID(String::new())),
			(id::CODEC_PRIVATE, MatroskaSpec::CodecPrivate(Vec::new())),
			(id::DEFAULT_DURATION, MatroskaSpec::DefaultDuration(0)),
			(id::VIDEO, MatroskaSpec::Video(Master::Start)),
			(id::PIXEL_WIDTH, MatroskaSpec::PixelWidth(0)),
			(id::PIXEL_HEIGHT, MatroskaSpec::PixelHeight(0)),
			(id::DISPLAY_WIDTH, MatroskaSpec::DisplayWidth(0)),
			(id::DISPLAY_HEIGHT, MatroskaSpec::DisplayHeight(0)),
			(id::DISPLAY_UNIT, MatroskaSpec::DisplayUnit(0)),
			(id::PIXEL_CROP_BOTTOM, MatroskaSpec::PixelCropBottom(0)),
			(id::PIXEL_CROP_TOP, MatroskaSpec::PixelCropTop(0)),
			(id::PIXEL_CROP_LEFT, MatroskaSpec::PixelCropLeft(0)),
			(id::PIXEL_CROP_RIGHT, MatroskaSpec::PixelCropRight(0)),
			(id::STEREO_MODE, MatroskaSpec::StereoMode(0)),
			(id::PROJECTION, MatroskaSpec::Projection(Master::Start)),
			(id::PROJECTION_TYPE, MatroskaSpec::ProjectionType(0)),
			(id::PROJECTION_PRIVATE, MatroskaSpec::ProjectionPrivate(Vec::new())),
			(id::PROJECTION_POSE_YAW, MatroskaSpec::ProjectionPoseYaw(0.0)),
			(id::PROJECTION_POSE_PITCH, MatroskaSpec::ProjectionPosePitch(0.0)),
			(id::PROJECTION_POSE_ROLL, MatroskaSpec::ProjectionPoseRoll(0.0)),
			(id::COLOUR, MatroskaSpec::Colour(Master::Start)),
			(id::MATRIX_COEFFICIENTS, MatroskaSpec::MatrixCoefficients(0)),
			(id::RANGE, MatroskaSpec::Range(0)),
			(id::TRANSFER_CHARACTERISTICS, MatroskaSpec::TransferCharacteristics(0)),
			(id::PRIMARIES, MatroskaSpec::Primaries(0)),
			(id::MAX_CLL, MatroskaSpec::MaxCLL(0)),
			(id::MAX_FALL, MatroskaSpec::MaxFALL(0)),
			(id::MASTERING_METADATA, MatroskaSpec::MasteringMetadata(Master::Start)),
			(id::PRIMARY_R_CHROMATICITY_X, MatroskaSpec::PrimaryRChromaticityX(0.0)),
			(id::LUMINANCE_MIN, MatroskaSpec::LuminanceMin(0.0)),
			(id::AUDIO, MatroskaSpec::Audio(Master::Start)),
			(id::SAMPLING_FREQUENCY, MatroskaSpec::SamplingFrequency(0.0)),
			(id::CHANNELS, MatroskaSpec::Channels(0)),
			(id::TIMESTAMP, MatroskaSpec::Timestamp(0)),
			(id::SIMPLE_BLOCK, MatroskaSpec::SimpleBlock(Vec::new())),
			(id::BLOCK_GROUP, MatroskaSpec::BlockGroup(Master::Start)),
			(id::BLOCK, MatroskaSpec::Block(Vec::new())),
			(id::REFERENCE_BLOCK, MatroskaSpec::ReferenceBlock(0)),
		];
		for (id, tag) in expected {
			assert_eq!(u64::from(id), tag.get_id(), "{tag:?}");
		}
	}

	/// Feed `data` in `chunk`-byte pieces, choosing `descend` for masters and `read` otherwise.
	fn walk(data: &[u8], chunk: usize) -> Vec<String> {
		let mut reader = Reader::new();
		let mut events = Vec::new();
		// Chunk boundaries differ by split, so a streamed body is reported once, whole.
		let mut streamed = Vec::new();
		let mut handle = |reader: &mut Reader, event: Event| match event {
			Event::Start(header) => {
				events.push(format!("start {:x}", header.id));
				match header.id {
					id::SEGMENT | id::CLUSTER | id::TRACKS => reader.descend().unwrap(),
					id::SIMPLE_BLOCK => reader.stream().unwrap(),
					_ => reader.read().unwrap(),
				}
			}
			Event::Value(id, data) => events.push(format!("value {id:x} {data:?}")),
			Event::Chunk(data) => streamed.extend_from_slice(data),
			Event::End(id) => {
				if !streamed.is_empty() {
					events.push(format!("chunk {:?}", std::mem::take(&mut streamed)));
				}
				events.push(format!("end {id:x}"));
			}
		};
		for mut input in data.chunks(chunk) {
			while let Some(event) = reader.next(&mut input).unwrap() {
				handle(&mut reader, event);
			}
			assert!(input.is_empty());
		}
		while let Some(event) = reader.finish().unwrap() {
			handle(&mut reader, event);
		}
		events
	}

	#[test]
	fn every_split_yields_the_same_events() {
		// Unknown-size Segment > unknown-size Cluster > Timestamp, SimpleBlock; then a sized
		// Cluster that closes the first.
		let data = [
			0x18, 0x53, 0x80, 0x67, 0x01, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, // Segment
			0x1f, 0x43, 0xb6, 0x75, 0xff, // Cluster, unknown size in one byte
			0xe7, 0x81, 0x05, // Timestamp 5
			0xa3, 0x83, 1, 2, 3, // SimpleBlock
			0x1f, 0x43, 0xb6, 0x75, 0x83, // Cluster of 3 bytes
			0xe7, 0x81, 0x06, // Timestamp 6
		];
		let expected = walk(&data, data.len());
		assert_eq!(
			expected,
			[
				"start 18538067",
				"start 1f43b675",
				"start e7",
				"value e7 [5]",
				"start a3",
				"chunk [1, 2, 3]",
				"end a3",
				"end 1f43b675",
				"start 1f43b675",
				"start e7",
				"value e7 [6]",
				"end 1f43b675",
				"end 18538067",
			]
		);
		for chunk in 1..data.len() {
			assert_eq!(walk(&data, chunk), expected, "chunk {chunk}");
		}
	}

	#[test]
	fn an_unknown_size_cluster_ends_with_its_sized_segment() {
		let data = [
			0x18, 0x53, 0x80, 0x67, 0x88, // Segment of 8 bytes
			0x1f, 0x43, 0xb6, 0x75, 0xff, // Cluster, unknown size
			0xe7, 0x81, 0x05, // Timestamp 5
			0xe7, 0x81, 0x06, // a top-level Timestamp after the Segment
		];
		let expected = [
			"start 18538067",
			"start 1f43b675",
			"start e7",
			"value e7 [5]",
			"end 1f43b675",
			"end 18538067",
			"start e7",
			"value e7 [6]",
		];
		for chunk in 1..=data.len() {
			assert_eq!(walk(&data, chunk), expected, "chunk {chunk}");
		}
	}

	#[test]
	fn a_child_cannot_overrun_its_parent() {
		// Tracks of 3 bytes holding a 2-byte element whose body needs 2 more.
		let data = [0x16, 0x54, 0xae, 0x6b, 0x83, 0xd7, 0x82, 0x01, 0x02];
		let mut reader = Reader::new();
		let mut input = &data[..];
		let Some(Event::Start(_)) = reader.next(&mut input).unwrap() else {
			panic!("expected Tracks");
		};
		reader.descend().unwrap();
		assert!(matches!(reader.next(&mut input), Err(Error::MatroskaParse)));
	}

	#[test]
	fn a_header_cannot_straddle_its_parents_end() {
		// Tracks of 1 byte, followed by a 2-byte ID and an empty size: an empty CodecPrivate if the
		// header escaped its parent.
		let data = [0x16, 0x54, 0xae, 0x6b, 0x81, 0x63, 0xa2, 0x80];
		// Every split is refused, including one exactly at the parent's end.
		for split in 0..=data.len() {
			let mut reader = Reader::new();
			let mut refused = false;
			'chunks: for mut input in [&data[..split], &data[split..]] {
				loop {
					match reader.next(&mut input) {
						Ok(Some(Event::Start(header))) if header.id == id::TRACKS => reader.descend().unwrap(),
						Ok(Some(event)) => panic!("split {split}: unexpected {event:?}"),
						Ok(None) => break,
						Err(err) => {
							assert!(matches!(err, Error::MatroskaParse), "split {split}: {err:?}");
							refused = true;
							break 'chunks;
						}
					}
				}
			}
			assert!(refused, "split {split}");
		}
	}

	#[test]
	fn only_segment_and_cluster_may_have_an_unknown_size() {
		let data = [0x16, 0x54, 0xae, 0x6b, 0xff];
		let mut reader = Reader::new();
		let mut input = &data[..];
		let Some(Event::Start(header)) = reader.next(&mut input).unwrap() else {
			panic!("expected Tracks");
		};
		assert!(header.end.is_none());
		assert!(matches!(reader.descend(), Err(Error::MatroskaParse)));
	}

	#[test]
	fn truncation_is_refused_at_eof() {
		for data in [
			&[0x1a, 0x45][..],                         // a split ID
			&[0x1a, 0x45, 0xdf, 0xa3, 0x84, 0x42][..], // a sized master's partial child header
		] {
			let mut reader = Reader::new();
			let mut input = data;
			while let Some(event) = reader.next(&mut input).unwrap() {
				if let Event::Start(_) = event {
					reader.descend().unwrap();
				}
			}
			assert!(matches!(reader.finish(), Err(Error::Truncated)), "{data:?}");
		}
	}

	#[test]
	fn skipped_bytes_are_never_retained() {
		// An EBML header declaring 1 GiB, holding a Void declaring nearly as much.
		let mut reader = Reader::new();
		let mut input = &[
			0x1a, 0x45, 0xdf, 0xa3, 0x08, 0x40, 0, 0, 0, 0xec, 0x08, 0x3f, 0xff, 0xff, 0xfa,
		][..];
		let Some(Event::Start(_)) = reader.next(&mut input).unwrap() else {
			panic!("expected EBML");
		};
		reader.descend().unwrap();
		let Some(Event::Start(_)) = reader.next(&mut input).unwrap() else {
			panic!("expected Void");
		};
		reader.skip().unwrap();
		for _ in 0..64 {
			let mut input = &[0u8; 1024][..];
			assert!(reader.next(&mut input).unwrap().is_none());
			assert!(input.is_empty());
			assert_eq!(reader.retained(), MAX_HEADER);
		}
		assert!(matches!(reader.finish(), Err(Error::Truncated)));
	}
}
