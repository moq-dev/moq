use std::collections::{HashMap, HashSet};
use std::io::Cursor;

use crate::Result;
use bytes::{Bytes, BytesMut};
use ebml_iterable::specs::{EbmlSpecification, EbmlTag, TagDataType};
use hang::catalog::{AAC, AudioCodec, AudioConfig, H265, VP9, VideoCodec, VideoConfig};
use moq_net::Timestamp;
use mp4_atom::Atom;
use webm_iterable::matroska_spec::{Master, MatroskaSpec};

use super::Error;
use super::ebml::{self, Event, Header, id};
use crate::container::producer::Streamed;

/// Default Matroska TimestampScale: 1 ms (in nanoseconds).
const DEFAULT_TIMESTAMP_SCALE_NS: u64 = 1_000_000;

/// Converts MKV/WebM (Matroska) files into MoQ broadcast streams.
///
/// Supports both batch and streaming/live input. WebM "live mode" (Segment and
/// Cluster elements with unknown size) is handled the same as bounded files.
///
/// ## Supported Codecs
///
/// **Video:**
/// - H.264 (`V_MPEG4/ISO/AVC`)
/// - H.265 (`V_MPEGH/ISO/HEVC`)
/// - VP8 (`V_VP8`)
/// - VP9 (`V_VP9`)
/// - AV1 (`V_AV1`)
///
/// **Audio:**
/// - AAC (`A_AAC`)
/// - Opus (`A_OPUS`)
/// - FLAC (`A_FLAC`)
/// - MP3 (`A_MPEG/L3`)
///
/// Laced blocks require a positive TrackEntry DefaultDuration to timestamp each frame.
///
/// Unsupported track roles are skipped; malformed tracks and unsupported codecs fail the import.
///
/// ## One pass
///
/// Each block's payload streams into its frame as its bytes arrive, readable before the block
/// completes, and elements the importer does not use are discarded as they arrive. The importer
/// retains no more than an element header and the track fields it reads, whatever sizes the input
/// declares; the published frames are held by their tracks, which refuse one too large for a group.
///
/// Input whose output depends on bytes after a payload is refused before that payload is published:
/// - a video `BlockGroup` whose `Block` neither follows a `ReferenceBlock` nor ends the group,
///   since a later `ReferenceBlock` would decide whether it starts a group;
/// - subtitle tracks, whose WebVTT cues change length with their content;
/// - [`Cmaf`](hang::catalog::Container::Cmaf) output, whose `moof` describes a payload before it.
///
/// Remux refused video so its blocks are `SimpleBlock`s, and extract subtitles separately.
pub struct Import<E: crate::catalog::hang::CatalogExt = ()> {
	broadcast: moq_net::broadcast::Producer,
	catalog: crate::catalog::Producer<E>,
	container: hang::catalog::Container,

	/// Held until the first block anchors the clock, so the catalog is withheld from the broadcast
	/// until every rendition is in and its root `clock` is final (and, when composed with other
	/// importers, until they release theirs too).
	initial_reservation: Option<crate::catalog::Reserved<E>>,

	/// The stream's timestamp base: the first block anchors it, and every block shifts by its
	/// offset onto the catalog clock.
	timebase: crate::catalog::Timebase<E>,

	/// Incremental element framing across input chunks.
	reader: ebml::Reader,
	/// The fields read from each metadata master still open, innermost last. A repeated field
	/// replaces its earlier value, so each holds at most one value per field it reads.
	fields: Vec<Vec<MatroskaSpec>>,
	/// TrackNumbers the Tracks element declared so far.
	numbers: HashSet<u64>,
	/// Whether a ReferenceBlock preceded the current BlockGroup's Block.
	referenced: bool,
	/// The SimpleBlock or Block streaming in.
	block: Option<Block>,
	/// A seek waiting for the streaming block to end, since its later frames continue its group.
	seek: Option<u64>,
	/// Whether `finish()` ended the input, even if it failed before the input drained.
	finished: bool,
	/// Why a chunk was refused. The rest of that chunk was never read, so later input cannot
	/// continue it.
	failed: Option<crate::Error>,
	/// Whether the Tracks element has been processed.
	tracks_seen: bool,

	/// Active TimestampScale (nanoseconds per Matroska tick).
	timestamp_scale_ns: u64,
	/// Current Cluster.Timestamp (in Matroska ticks).
	cluster_timestamp: u64,

	/// Per-TrackNumber state.
	tracks: HashMap<u64, MkvTrack>,
}

#[derive(PartialEq, Debug, Clone, Copy)]
enum TrackKind {
	Video,
	Audio,
}

impl TrackKind {
	/// The publisher priority for this kind of media, so audio isn't stuck behind a
	/// video backlog on a busy connection.
	fn priority(&self) -> i8 {
		match self {
			Self::Video => hang::catalog::PRIORITY.video,
			Self::Audio => hang::catalog::PRIORITY.audio,
		}
	}
}

struct MkvTrack {
	default_duration: Option<std::time::Duration>,
	kind: TrackKind,
	track: Media,
}

enum Media {
	Video(crate::container::Producer<crate::catalog::hang::Container, VideoConfig>),
	Audio(crate::container::Producer<crate::catalog::hang::Container, AudioConfig>),
}

impl Media {
	fn open(
		&mut self,
		timestamp: Timestamp,
		keyframe: bool,
		duration: Option<Timestamp>,
		size: usize,
	) -> crate::Result<Streamed> {
		match self {
			Self::Video(track) => track.open(timestamp, keyframe, duration, size),
			Self::Audio(track) => track.open(timestamp, keyframe, duration, size),
		}
	}

	fn commit(&mut self, frame: Streamed) -> crate::Result<()> {
		match self {
			Self::Video(track) => track.commit(frame),
			Self::Audio(track) => track.commit(frame),
		}
	}

	fn cut(&mut self, timestamp: Option<Timestamp>) -> crate::Result<()> {
		match self {
			Self::Video(track) => track.cut(timestamp),
			Self::Audio(track) => track.cut(timestamp),
		}
	}

	fn seek(&mut self, sequence: u64) -> crate::Result<()> {
		match self {
			Self::Video(track) => track.seek(sequence),
			Self::Audio(track) => track.seek(sequence),
		}
	}

	fn finish(&mut self) -> crate::Result<()> {
		match self {
			Self::Video(track) => track.finish(),
			Self::Audio(track) => track.finish(),
		}
	}

	fn end(&mut self) -> crate::Result<()> {
		match self {
			Self::Video(track) => track.end(),
			Self::Audio(track) => track.end(),
		}
	}

	fn abort(self, err: moq_net::Error) {
		match self {
			Self::Video(track) => track.abort(err),
			Self::Audio(track) => track.abort(err),
		}
	}

	fn name(&self) -> &str {
		match self {
			Self::Video(track) => track.name(),
			Self::Audio(track) => track.name(),
		}
	}
}

/// How the importer consumes an element.
enum Action {
	/// Walk its children.
	Descend,
	/// Walk its children, keeping the fields it reads for when it ends.
	Collect,
	/// Read its value whole: a field the importer uses.
	Read,
	/// Pass its body through: a block.
	Stream,
	/// Discard its body as it arrives.
	Skip,
}

/// A SimpleBlock or Block whose body is streaming in.
struct Block {
	/// Body bytes still to arrive.
	remaining: u64,
	/// For a Block, how its BlockGroup decides its keyframe flag; a SimpleBlock's flags carry it.
	grouped: Option<Grouped>,
	state: BlockState,
}

#[derive(Clone, Copy)]
struct Grouped {
	/// A ReferenceBlock preceded the Block, so it continues a group.
	referenced: bool,
	/// The Block ends its BlockGroup, so no ReferenceBlock can follow it.
	last: bool,
}

enum BlockState {
	/// The track number, relative timestamp, and flags, which may split across chunks.
	Header(Vec<u8>),
	/// Lace sizes, parsed a byte at a time.
	Lacing(Lacing),
	/// Frames streaming into their track.
	Frames(Box<Frames>),
	/// A skipped track's payload, or the rest of a failed block, discarded.
	Skip,
}

/// What a block header says.
#[derive(Clone, Copy)]
struct Fixed {
	track: u64,
	timestamp: i16,
	keyframe: bool,
}

/// Matroska lacing, from the block flags.
#[derive(Clone, Copy, PartialEq)]
enum Lace {
	Xiph,
	FixedSize,
	Ebml,
}

struct Lacing {
	block: Fixed,
	lace: Lace,
	/// The frame count, once its byte arrives.
	count: Option<usize>,
	/// Sizes of all but the last frame, as each completes.
	sizes: Vec<u64>,
	/// The Xiph size being summed.
	sum: u64,
	/// The EBML size being read.
	vint: Vec<u8>,
}

struct Frames {
	track: u64,
	kind: TrackKind,
	/// The first frame's presentation time, in nanoseconds.
	pts_ns: u64,
	/// Nanoseconds between laced frames.
	step: u64,
	keyframe: bool,
	duration: Option<Timestamp>,
	/// Every frame's size, in order.
	sizes: Vec<u64>,
	/// The next frame to open.
	index: usize,
	/// The frame being written.
	open: Option<Streamed>,
}

impl<E: crate::catalog::hang::CatalogExt> Import<E> {
	/// Import a byte stream into the broadcast using its reserved catalog producer.
	pub fn new(broadcast: moq_net::broadcast::Producer, reserved: crate::catalog::Reserved<E>) -> Self {
		let container = hang::catalog::Container::default();
		Self {
			broadcast,
			catalog: reserved.producer(),
			container,
			timebase: reserved.timebase(),
			initial_reservation: Some(reserved),
			reader: ebml::Reader::new(),
			fields: Vec::new(),
			numbers: HashSet::new(),
			referenced: false,
			block: None,
			seek: None,
			finished: false,
			failed: None,
			tracks_seen: false,
			timestamp_scale_ns: DEFAULT_TIMESTAMP_SCALE_NS,
			cluster_timestamp: 0,
			tracks: HashMap::default(),
		}
	}

	/// Select the container this importer wraps decoded media renditions in.
	///
	/// [`Legacy`](hang::catalog::Container::Legacy) unless selected. It applies to every rendition
	/// this input demuxes, since the tracks are discovered rather than named by the caller.
	/// [`Cmaf`](hang::catalog::Container::Cmaf) is refused when the first track is declared, since a
	/// block's payload streams into its frame.
	pub fn with_container(mut self, container: hang::catalog::Container) -> Self {
		self.container = container;
		self
	}

	/// Parse `data`, a chunk of the input split anywhere.
	///
	/// Every byte is consumed: a block's payload is written to its frame, an ignored element's is
	/// discarded, and only an element header split across chunks or a field being read is retained.
	/// A refused chunk fails the import: later input is refused with the same error, and
	/// [`finish`](Self::finish) ends the tracks with what was accepted.
	pub fn decode(&mut self, data: &[u8]) -> Result<()> {
		if self.finished && !data.is_empty() {
			return Err(Error::TracksAfterFinish.into());
		}
		if let Some(err) = &self.failed {
			return Err(err.clone());
		}
		let result = self.drain(data);
		if let Err(err) = &result {
			self.failed = Some(err.clone());
		}
		result
	}

	fn drain_eof(&mut self) -> Result<()> {
		while let Some(event) = self.reader.finish()? {
			self.handle(event)?;
		}
		Ok(())
	}

	fn drain(&mut self, mut input: &[u8]) -> Result<()> {
		while let Some(event) = self.reader.next(&mut input)? {
			self.handle(event)?;
		}
		Ok(())
	}

	fn handle(&mut self, event: Event<'_>) -> Result<()> {
		match event {
			Event::Start(header) => self.start(header),
			Event::Value(id, data) => self.value(id, data),
			Event::Chunk(chunk) => self.chunk(chunk),
			Event::End(id) => self.end(id),
		}
	}

	fn action(&self, parent: Option<u32>, header: &Header) -> Result<Action> {
		use id::*;
		let action = match (parent, header.id) {
			(None, EBML) => Action::Collect,
			(None, SEGMENT) => Action::Descend,
			(Some(EBML), DOC_TYPE) => Action::Read,
			(Some(SEGMENT), INFO | CLUSTER) => Action::Descend,
			// A second Tracks element would redeclare a published track set.
			(Some(SEGMENT), TRACKS) if !self.tracks_seen => Action::Descend,
			(Some(INFO), TIMESTAMP_SCALE) => Action::Read,
			(Some(TRACKS), TRACK_ENTRY) => Action::Collect,
			(Some(TRACK_ENTRY), VIDEO | AUDIO) => Action::Collect,
			(Some(TRACK_ENTRY), TRACK_NUMBER | TRACK_TYPE | CODEC_ID | CODEC_PRIVATE | DEFAULT_DURATION) => {
				Action::Read
			}
			(Some(VIDEO), COLOUR | PROJECTION) => Action::Collect,
			(
				Some(VIDEO),
				PIXEL_WIDTH | PIXEL_HEIGHT | DISPLAY_WIDTH | DISPLAY_HEIGHT | DISPLAY_UNIT | PIXEL_CROP_BOTTOM
				| PIXEL_CROP_TOP | PIXEL_CROP_LEFT | PIXEL_CROP_RIGHT | STEREO_MODE,
			) => Action::Read,
			(
				Some(PROJECTION),
				PROJECTION_TYPE | PROJECTION_POSE_YAW | PROJECTION_POSE_PITCH | PROJECTION_POSE_ROLL,
			) => Action::Read,
			// Only the equirectangular `equi` body is understood, so other sizes are skipped and
			// recorded as unsupported in `start`.
			(Some(PROJECTION), PROJECTION_PRIVATE) if header.size == Some(EQUI_PRIVATE_LEN) => Action::Read,
			(Some(COLOUR), MASTERING_METADATA) => Action::Collect,
			(Some(COLOUR), MATRIX_COEFFICIENTS | RANGE | TRANSFER_CHARACTERISTICS | PRIMARIES | MAX_CLL | MAX_FALL) => {
				Action::Read
			}
			(Some(MASTERING_METADATA), PRIMARY_R_CHROMATICITY_X..=LUMINANCE_MIN) => Action::Read,
			(Some(AUDIO), SAMPLING_FREQUENCY | CHANNELS) => Action::Read,
			(Some(CLUSTER), TIMESTAMP) => Action::Read,
			(Some(CLUSTER), SIMPLE_BLOCK) => Action::Stream,
			(Some(CLUSTER), BLOCK_GROUP) => Action::Descend,
			(Some(BLOCK_GROUP), BLOCK) => Action::Stream,
			_ => Action::Skip,
		};
		// A number is at most 8 bytes, so a larger one is refused before its bytes are held.
		if let Action::Read = action
			&& matches!(
				MatroskaSpec::get_tag_data_type(header.id.into()),
				Some(TagDataType::UnsignedInt | TagDataType::Integer | TagDataType::Float)
			) && header.size.is_none_or(|size| size > 8)
		{
			return Err(Error::MatroskaParse.into());
		}
		Ok(action)
	}

	fn start(&mut self, header: Header) -> Result<()> {
		let parent = self.reader.parent();
		match self.action(parent, &header)? {
			Action::Descend => self.reader.descend()?,
			Action::Collect => {
				self.reader.descend()?;
				self.fields.push(Vec::new());
			}
			Action::Read => self.reader.read()?,
			Action::Stream => {
				self.reader.stream()?;
				let grouped = (header.id == id::BLOCK).then(|| Grouped {
					referenced: self.referenced,
					last: header.end == self.reader.parent_end(),
				});
				self.block = Some(Block {
					remaining: header.size.unwrap_or_default(),
					grouped,
					state: BlockState::Header(Vec::new()),
				});
			}
			Action::Skip => {
				self.reader.skip()?;
				if parent == Some(id::BLOCK_GROUP) && header.id == id::REFERENCE_BLOCK {
					self.referenced = true;
				}
				// An empty body is never a supported one, so a projection that needs it is refused
				// when its master ends, without holding the bytes.
				if parent == Some(id::PROJECTION) && header.id == id::PROJECTION_PRIVATE {
					set(
						self.fields.last_mut().expect("Projection is collected"),
						MatroskaSpec::ProjectionPrivate(Vec::new()),
					)?;
				}
			}
		}
		match header.id {
			id::CLUSTER => self.cluster_timestamp = 0,
			id::BLOCK_GROUP => self.referenced = false,
			_ => {}
		}
		Ok(())
	}

	fn value(&mut self, id: u32, data: Vec<u8>) -> Result<()> {
		let value = field(id, data)?;
		match value {
			MatroskaSpec::TimestampScale(0) => return Err(Error::ValueOutOfRange("TimestampScale").into()),
			MatroskaSpec::TimestampScale(scale) => self.timestamp_scale_ns = scale,
			// Within a Cluster, this is the cluster timestamp (in Matroska ticks).
			MatroskaSpec::Timestamp(timestamp) => self.cluster_timestamp = timestamp,
			value => set(
				self.fields.last_mut().expect("a read field has a collecting parent"),
				value,
			)?,
		}
		Ok(())
	}

	fn end(&mut self, id: u32) -> Result<()> {
		match id {
			id::EBML => {
				let children = self.fields.pop().unwrap_or_default();
				self.handle_ebml(&children)?;
			}
			id::TRACK_ENTRY => {
				let children = self.fields.pop().unwrap_or_default();
				// Only `finish()` releases the reservation before Tracks, since a block needs a track.
				let reserved = self.initial_reservation.clone().ok_or(Error::TracksAfterFinish)?;
				self.add_track(&reserved, children)?;
			}
			id::VIDEO | id::AUDIO | id::COLOUR | id::MASTERING_METADATA | id::PROJECTION => {
				let children = self.fields.pop().unwrap_or_default();
				let master =
					MatroskaSpec::get_master_tag(id.into(), Master::Full(children)).ok_or(Error::MatroskaParse)?;
				set(
					self.fields
						.last_mut()
						.expect("a collected master has a collecting parent"),
					master,
				)?;
			}
			id::TRACKS => {
				self.tracks_seen = true;
				self.numbers.clear();
				// The reservation stays held until the first block anchors the clock, unless no track
				// was declared: then no block ever will.
				if self.tracks.is_empty() {
					self.initial_reservation = None;
				}
			}
			id::SIMPLE_BLOCK | id::BLOCK => self.block_end()?,
			_ => {}
		}
		Ok(())
	}

	fn handle_ebml(&self, children: &[MatroskaSpec]) -> Result<()> {
		for c in children {
			if let MatroskaSpec::DocType(doc) = c {
				match doc.as_str() {
					"matroska" | "webm" => return Ok(()),
					other => return Err(Error::UnsupportedDocType(other.to_string()).into()),
				}
			}
		}
		Err(Error::MissingDocType.into())
	}

	fn add_track(&mut self, reserved: &crate::catalog::Reserved<E>, children: Vec<MatroskaSpec>) -> Result<()> {
		let mut track_number: Option<u64> = None;
		let mut track_type: Option<u64> = None;
		let mut codec_id: Option<String> = None;
		let mut codec_private: Option<Bytes> = None;
		let mut audio_children: Option<Vec<MatroskaSpec>> = None;
		let mut video_children: Option<Vec<MatroskaSpec>> = None;
		let mut default_duration = None;

		for c in children {
			match c {
				MatroskaSpec::DefaultDuration(v) if v > 0 => {
					default_duration = Some(std::time::Duration::from_nanos(v))
				}
				MatroskaSpec::TrackNumber(v) => track_number = Some(v),
				MatroskaSpec::TrackType(v) => track_type = Some(v),
				MatroskaSpec::CodecID(v) => codec_id = Some(v),
				MatroskaSpec::CodecPrivate(v) => codec_private = Some(Bytes::from(v)),
				MatroskaSpec::Audio(Master::Full(v)) => audio_children = Some(v),
				MatroskaSpec::Video(Master::Full(v)) => video_children = Some(v),
				_ => {}
			}
		}

		let track_number = track_number.ok_or(Error::MissingTrackNumber)?;
		if track_number == 0 {
			return Err(Error::ValueOutOfRange("TrackNumber").into());
		}
		if !self.numbers.insert(track_number) {
			return Err(moq_net::Error::Duplicate.into());
		}
		let track_type = track_type.ok_or(Error::MissingTrackType)?;
		let codec_id = codec_id.ok_or(Error::MissingCodecId)?;

		// Matroska TrackType: 1 = video, 2 = audio, 17 = subtitles.
		let (kind, suffix) = match track_type {
			1 => (TrackKind::Video, ".mkv-v"),
			2 => (TrackKind::Audio, ".mkv-a"),
			17 => return Err(Error::SubtitleTrack(codec_id).into()),
			other => {
				tracing::warn!(track_type = other, codec_id, "unsupported MKV track type, skipping");
				return Ok(());
			}
		};
		if matches!(self.container, hang::catalog::Container::Cmaf { .. }) {
			return Err(Error::StreamedCmaf.into());
		}

		let track = self.broadcast.create_track(
			self.broadcast.unique_name(suffix),
			self.catalog.track_info(kind.priority()),
		)?;
		// Build the media producer before publishing the rendition. It is fallible (its
		// timeline track can collide), and a rendition published for a track we then fail
		// to produce would be advertised to consumers but never served.
		let wire = crate::catalog::hang::Container::new(
			&self.container,
			match kind {
				TrackKind::Video => crate::container::Kind::Video,
				TrackKind::Audio => crate::container::Kind::Audio,
			},
		)?;
		let media = match kind {
			TrackKind::Video => {
				let mut config = build_video_config(&codec_id, codec_private.as_ref(), video_children.as_deref())?;
				config.container = self.container.clone();
				Media::Video(reserved.video(track, wire, config)?)
			}
			TrackKind::Audio => {
				let mut config = build_audio_config(&codec_id, codec_private.as_ref(), audio_children.as_deref())?;
				config.container = self.container.clone();
				Media::Audio(reserved.audio(track, wire, config)?)
			}
		};

		self.tracks.insert(
			track_number,
			MkvTrack {
				default_duration,
				kind,
				track: media,
			},
		);

		Ok(())
	}

	/// Feed a chunk of the streaming block's body.
	fn chunk(&mut self, chunk: &[u8]) -> Result<()> {
		let Some(mut block) = self.block.take() else {
			return Ok(());
		};
		let result = self.feed(&mut block, chunk);
		if let Err(err) = &result {
			// The rest of a failed block is discarded, and the frame it cut short fails its group.
			if let BlockState::Frames(frames) = std::mem::replace(&mut block.state, BlockState::Skip)
				&& let Some(frame) = frames.open
			{
				frame.abort(err.reason());
			}
		}
		self.block = Some(block);
		result
	}

	fn feed(&mut self, block: &mut Block, mut chunk: &[u8]) -> Result<()> {
		loop {
			let next = match &mut block.state {
				BlockState::Header(header) => {
					let Some((&byte, rest)) = chunk.split_first() else {
						return Ok(());
					};
					chunk = rest;
					block.remaining -= 1;
					header.push(byte);
					let Some((fixed, lace)) = block_header(header, block.grouped.is_some())? else {
						continue;
					};
					if !self.tracks.contains_key(&fixed.track) {
						// Unknown or skipped track.
						BlockState::Skip
					} else if let Some(lace) = lace {
						BlockState::Lacing(Lacing {
							block: fixed,
							lace,
							count: None,
							sizes: Vec::new(),
							sum: 0,
							vint: Vec::new(),
						})
					} else {
						BlockState::Frames(Box::new(self.frames(
							fixed,
							block.grouped,
							vec![block.remaining],
							false,
						)?))
					}
				}
				BlockState::Lacing(lacing) => {
					let Some((&byte, rest)) = chunk.split_first() else {
						return Ok(());
					};
					chunk = rest;
					block.remaining -= 1;
					let Some(sizes) = lacing.push(byte, block.remaining).map_err(anyhow::Error::new)? else {
						continue;
					};
					let fixed = lacing.block;
					BlockState::Frames(Box::new(self.frames(fixed, block.grouped, sizes, true)?))
				}
				BlockState::Frames(frames) => {
					let written = self.write(frames, chunk)?;
					block.remaining -= written as u64;
					return Ok(());
				}
				BlockState::Skip => {
					block.remaining -= chunk.len() as u64;
					return Ok(());
				}
			};
			block.state = next;
		}
	}

	/// Check a block's frames before any is published, and plan them.
	fn frames(&mut self, block: Fixed, grouped: Option<Grouped>, sizes: Vec<u64>, laced: bool) -> Result<Frames> {
		let track = self.tracks.get(&block.track).expect("a planned block's track exists");
		if track.kind == TrackKind::Video
			&& let Some(grouped) = grouped
			&& !grouped.referenced
			&& !grouped.last
		{
			return Err(Error::TrailingReference.into());
		}

		// Compute PTS in MKV's native nanosecond units and stamp it on the
		// timestamp at NANO scale so a passthrough re-emit preserves precision.
		let block_ticks = self
			.cluster_timestamp
			.checked_add_signed(i64::from(block.timestamp))
			.ok_or(if block.timestamp < 0 {
				Error::NegativeBlockTimestamp
			} else {
				Error::TimestampOverflow
			})?;
		let pts_ns = block_ticks
			.checked_mul(self.timestamp_scale_ns)
			.ok_or(Error::TimestampOverflow)?;

		let duration = match laced {
			true => Some(
				track
					.default_duration
					.ok_or_else(|| anyhow::Error::new(LacingError::MissingDuration))?,
			),
			false => None,
		};
		// Validate the last timestamp before publishing any part of this block.
		let step = duration.map(|duration| duration.as_nanos()).unwrap_or_default();
		let duration = duration
			.map(|duration| Timestamp::from_nanos(duration.as_nanos() as u64))
			.transpose()?;
		let last = u128::from(pts_ns) + step * (sizes.len() - 1) as u128;
		Timestamp::from_nanos(u64::try_from(last).map_err(|_| Error::TimestampOverflow)?)?;

		let mut frames = Frames {
			track: block.track,
			kind: track.kind,
			pts_ns,
			step: step as u64,
			keyframe: grouped.map_or(block.keyframe, |grouped| !grouped.referenced),
			duration,
			sizes,
			index: 0,
			open: None,
		};
		// An empty frame is due before any byte arrives.
		self.write(&mut frames, &[])?;
		Ok(frames)
	}

	/// Write `chunk` into the block's frames, opening each as the last completes. Returns the bytes
	/// written, which is all of `chunk` unless it overruns the block.
	fn write(&mut self, frames: &mut Frames, mut chunk: &[u8]) -> Result<usize> {
		let total = chunk.len();
		let track = self
			.tracks
			.get_mut(&frames.track)
			.expect("a planned block's track exists");
		loop {
			let frame = match frames.open.as_mut() {
				Some(frame) => frame,
				None => {
					let Some(&size) = frames.sizes.get(frames.index) else {
						break;
					};
					let pts_ns = frames.pts_ns + frames.step * frames.index as u64;
					// Anchor before releasing the reservation, so the first catalog carries its clock.
					let timestamp = self.timebase.shift(Timestamp::from_nanos(pts_ns)?)?;
					self.initial_reservation = None;
					let keyframe = match frames.kind {
						TrackKind::Audio => frames.index == 0,
						TrackKind::Video => frames.keyframe,
					};
					let size = usize::try_from(size).map_err(|_| moq_net::Error::FrameTooLarge)?;
					frames.index += 1;
					frames
						.open
						.insert(track.track.open(timestamp, keyframe, frames.duration, size)?)
				}
			};
			let (bytes, rest) = chunk.split_at(frame.remaining().min(chunk.len()));
			if !bytes.is_empty() {
				frame.write(bytes)?;
			}
			chunk = rest;
			if frame.remaining() > 0 {
				break;
			}
			let frame = frames.open.take().expect("a frame is open");
			track.track.commit(frame)?;
		}
		Ok(total - chunk.len())
	}

	/// The streaming block ended: its frames are complete, or it was malformed.
	fn block_end(&mut self) -> Result<()> {
		let Some(block) = self.block.take() else {
			return Ok(());
		};
		match block.state {
			BlockState::Header(_) => {
				return Err(match block.grouped {
					Some(_) => Error::InvalidBlock,
					None => Error::InvalidSimpleBlock,
				}
				.into());
			}
			BlockState::Lacing(_) => return Err(anyhow::Error::new(LacingError::Invalid).into()),
			BlockState::Frames(frames) => {
				// The frame sizes add up to the body, so every frame was written as its bytes arrived.
				debug_assert!(frames.open.is_none() && frames.index == frames.sizes.len());
				if frames.kind == TrackKind::Audio
					&& let Some(track) = self.tracks.get_mut(&frames.track)
				{
					track.track.cut(None)?;
				}
			}
			BlockState::Skip => {}
		}
		if let Some(sequence) = self.seek.take() {
			self.seek(sequence)?;
		}
		Ok(())
	}

	/// Close the current group on every track and open the next one at `sequence`.
	///
	/// Broadcast-wide: every track inside this MKV import advances together; per-track
	/// control is intentionally not exposed. While a block streams in, the seek waits for it to end,
	/// since its later frames continue its group.
	pub fn seek(&mut self, sequence: u64) -> Result<()> {
		if self.block.is_some() {
			self.seek = Some(sequence);
			return Ok(());
		}
		for track in self.tracks.values_mut() {
			track.track.seek(sequence)?;
		}
		Ok(())
	}

	/// Finish all tracks, flushing current groups.
	///
	/// Input that ends inside an element other than an unknown-size Segment or Cluster is refused as
	/// truncated.
	pub fn finish(&mut self) -> Result<()> {
		self.finished = true;
		if self.failed.is_none()
			&& let Err(err) = self.drain_eof()
		{
			// A frame cut short fails its group with the reason, rather than waiting for the rest.
			self.abort_block(err.reason());
			return Err(err);
		}
		// No frame follows to anchor the clock, so publish the declared track set now.
		self.initial_reservation = None;
		for track in self.tracks.values_mut() {
			track.track.finish()?;
		}
		Ok(())
	}

	/// Abort all tracks with `err` instead of finishing, so subscribers see the real
	/// cause rather than [`moq_net::Error::Dropped`]. Consumes the importer.
	///
	/// Unlike a drop, this leaves the tracks' HLS records to the catalog, whose abort fails them
	/// rather than ending them as a complete recording.
	pub fn abort(mut self, err: moq_net::Error) {
		self.abort_block(err.clone());
		for track in std::mem::take(&mut self.tracks).into_values() {
			track.track.abort(err.clone());
		}
	}

	/// Fail the frame a block was streaming into, which holds its track's group.
	fn abort_block(&mut self, err: moq_net::Error) {
		if let Some(Block {
			state: BlockState::Frames(frames),
			..
		}) = self.block.take()
			&& let Some(frame) = frames.open
		{
			frame.abort(err);
		}
	}
}

impl<E: crate::catalog::hang::CatalogExt> Drop for Import<E> {
	/// End each track, then retire its HLS records: every track takes a fresh name, so nothing
	/// publishes it again.
	fn drop(&mut self) {
		// A frame cut short fails its group before the track ends.
		self.abort_block(moq_net::Error::Dropped);
		for mut track in std::mem::take(&mut self.tracks).into_values() {
			let name = track.track.name().to_string();
			let ended = track.track.end();
			drop(track);
			if let Err(err) = ended {
				tracing::warn!(%err, "failed to end a dropped MKV track");
			}
			self.catalog.retire_dropped(&name);
		}
	}
}

/// Decode a field the importer reads into its Matroska element.
fn field(id: u32, data: Vec<u8>) -> Result<MatroskaSpec> {
	let id = u64::from(id);
	let invalid = |_| Error::MatroskaParse;
	let value = match MatroskaSpec::get_tag_data_type(id) {
		Some(TagDataType::UnsignedInt) => {
			MatroskaSpec::get_unsigned_int_tag(id, ebml_iterable::tools::arr_to_u64(&data).map_err(invalid)?)
		}
		Some(TagDataType::Float) => MatroskaSpec::get_float_tag(
			id,
			match data.len() {
				0 => 0.0,
				_ => ebml_iterable::tools::arr_to_f64(&data).map_err(invalid)?,
			},
		),
		Some(TagDataType::Utf8) => {
			MatroskaSpec::get_utf8_tag(id, String::from_utf8(data).map_err(|_| Error::MatroskaParse)?)
		}
		Some(TagDataType::Binary) => MatroskaSpec::get_binary_tag(id, &data),
		_ => None,
	};
	Ok(value.ok_or(Error::MatroskaParse)?)
}

/// Record `value` in its master's fields, replacing an earlier value of the same field.
fn set(fields: &mut Vec<MatroskaSpec>, value: MatroskaSpec) -> Result<()> {
	let id = value.get_id();
	match fields.iter_mut().find(|field| field.get_id() == id) {
		// A track declares one number; a second would make its blocks ambiguous.
		Some(_) if matches!(value, MatroskaSpec::TrackNumber(_)) => return Err(moq_net::Error::Duplicate.into()),
		Some(field) => *field = value,
		None => fields.push(value),
	}
	Ok(())
}

/// Parse a block header: the track number, the relative timestamp, and the flags. `None` until
/// `buf` holds all of it.
fn block_header(buf: &[u8], grouped: bool) -> Result<Option<(Fixed, Option<Lace>)>> {
	let invalid = || match grouped {
		true => Error::InvalidBlock,
		false => Error::InvalidSimpleBlock,
	};
	let first = buf[0];
	if first == 0 {
		return Err(invalid().into());
	}
	let len = first.leading_zeros() as usize + 1;
	let Some(&[high, low, flags]) = buf.get(len..len + 3) else {
		return Ok(None);
	};
	let mut track = u64::from(first) & (0xff >> len);
	for byte in &buf[1..len] {
		track = (track << 8) | u64::from(*byte);
	}
	let lace = match (flags >> 1) & 0b11 {
		0 => None,
		1 => Some(Lace::Xiph),
		2 => Some(Lace::FixedSize),
		_ => Some(Lace::Ebml),
	};
	let fixed = Fixed {
		track,
		timestamp: i16::from_be_bytes([high, low]),
		// A Block's keyframe bit is reserved; its BlockGroup decides instead.
		keyframe: flags & 0x80 != 0,
	};
	Ok(Some((fixed, lace)))
}

fn build_video_config(
	codec_id: &str,
	codec_private: Option<&Bytes>,
	video_children: Option<&[MatroskaSpec]>,
) -> Result<VideoConfig> {
	let mut config = match codec_id {
		"V_VP8" => VideoConfig::new(VideoCodec::VP8),
		"V_VP9" => VideoConfig::new(VP9 {
			profile: 0,
			level: 0,
			bit_depth: 8,
			color_primaries: 1,
			chroma_subsampling: 1,
			transfer_characteristics: 1,
			matrix_coefficients: 1,
			full_range: false,
		}),
		"V_MPEG4/ISO/AVC" => build_h264_config(codec_private)?,
		"V_MPEGH/ISO/HEVC" => build_h265_config(codec_private)?,
		"V_AV1" => build_av1_config(codec_private)?,
		other => return Err(Error::UnsupportedVideoCodec(other.to_string()).into()),
	};

	let mut width = None;
	let mut height = None;
	for child in video_children.unwrap_or_default() {
		match child {
			MatroskaSpec::PixelWidth(v) => width = Some(*v),
			MatroskaSpec::PixelHeight(v) => height = Some(*v),
			_ => {}
		}
	}

	let coded_width = width
		.map(|v| u32::try_from(v).map_err(|_| Error::ValueOutOfRange("PixelWidth")))
		.transpose()?;
	let coded_height = height
		.map(|v| u32::try_from(v).map_err(|_| Error::ValueOutOfRange("PixelHeight")))
		.transpose()?;
	if config.coded_width.is_none() {
		config.coded_width = coded_width;
	}
	if config.coded_height.is_none() {
		config.coded_height = coded_height;
	}
	// The codec config outranks the Colour element a muxer added, which only fills its gaps.
	let colour = video_children
		.and_then(|cs| {
			cs.iter().find_map(|c| match c {
				MatroskaSpec::Colour(Master::Full(colour)) => Some(colour),
				_ => None,
			})
		})
		.map(|colour| matroska_color(colour))
		.transpose()?
		.flatten();
	crate::codec::color::fill(&mut config.color, colour.as_ref(), "Colour");
	(config.projection, config.stereo) = matroska_spatial(video_children.unwrap_or_default())?;
	if let Some(children) = video_children
		&& let Some(aspect) = matroska_aspect(children, (width, height))?
	{
		let coded = width
			.zip(height)
			.and_then(|(w, h)| crate::codec::video::reduce_aspect(w.into(), h.into()));
		if Some(aspect) != coded || config.display_aspect_width.is_some() {
			crate::codec::video::fill_display_aspect(&mut config, aspect, "Matroska");
		}
	}

	Ok(config)
}

/// The whole picture's display aspect, or `None` when the samples are square by default.
///
/// `DisplayWidth`/`DisplayHeight` size the picture after `PixelCrop*`, but the catalog carries no
/// crop and its aspect covers the whole decoded picture. So the cropped sample shape is applied to
/// the full frame, as FFmpeg does, instead of squeezing the uncropped picture into the cropped ratio.
fn matroska_aspect(children: &[MatroskaSpec], pixels: (Option<u64>, Option<u64>)) -> Result<Option<(u32, u32)>> {
	let (mut width, mut height, mut unit) = (None, None, 0);
	let (mut left, mut right, mut top, mut bottom) = (0, 0, 0, 0);
	for child in children {
		match child {
			MatroskaSpec::DisplayWidth(v) => width = Some(*v),
			MatroskaSpec::DisplayHeight(v) => height = Some(*v),
			MatroskaSpec::DisplayUnit(v) => unit = *v,
			MatroskaSpec::PixelCropLeft(v) => left = *v,
			MatroskaSpec::PixelCropRight(v) => right = *v,
			MatroskaSpec::PixelCropTop(v) => top = *v,
			MatroskaSpec::PixelCropBottom(v) => bottom = *v,
			_ => {}
		}
	}
	// 0 pixels, 1 centimeters, 2 inches, 3 aspect ratio, 4 unknown; both fields share the unit.
	if unit > 4 {
		return Err(Error::InvalidAspect("unknown DisplayUnit").into());
	}
	if unit != 0 && (width.is_none() || height.is_none()) {
		return Err(Error::InvalidAspect("non-pixel DisplayUnit without both display dimensions").into());
	}
	// A missing pixel-unit dimension defaults to the cropped size, which keeps samples square.
	if width.is_none() && height.is_none() {
		return Ok(None);
	}

	let (shown_width, shown_height) = if (left, right, top, bottom) == (0, 0, 0, 0) {
		let width = width.or(pixels.0).ok_or(Error::InvalidAspect("missing PixelWidth"))?;
		let height = height.or(pixels.1).ok_or(Error::InvalidAspect("missing PixelHeight"))?;
		(u128::from(width), u128::from(height))
	} else {
		let frame_width = pixels.0.ok_or(Error::InvalidAspect("missing PixelWidth"))?;
		let frame_height = pixels.1.ok_or(Error::InvalidAspect("missing PixelHeight"))?;
		let cropped = |size: u64, first: u64, second: u64| {
			size.checked_sub(first)
				.and_then(|size| size.checked_sub(second))
				.filter(|size| *size > 0)
				.ok_or(Error::InvalidAspect("crop leaves no picture"))
		};
		let cropped_width = cropped(frame_width, left, right)?;
		let cropped_height = cropped(frame_height, top, bottom)?;
		// display / cropped is the sample shape; scale the whole frame by it.
		let scale = |display: u64, frame: u64, cropped_other: u64| {
			u128::from(display)
				.checked_mul(u128::from(frame))
				.and_then(|size| size.checked_mul(u128::from(cropped_other)))
				.ok_or(Error::InvalidAspect("unrepresentable display dimensions"))
		};
		(
			scale(width.unwrap_or(cropped_width), frame_width, cropped_height)?,
			scale(height.unwrap_or(cropped_height), frame_height, cropped_width)?,
		)
	};
	let aspect = crate::codec::video::reduce_aspect(shown_width, shown_height)
		.ok_or(Error::InvalidAspect("invalid or unrepresentable display dimensions"))?;
	Ok(Some(aspect))
}

/// The projection and stereo packing a Matroska `Projection` and `StereoMode` signal, each `None`
/// for flat or mono video.
///
/// The catalog has no cubemap, mesh, pose, or eye order beyond left-first side by side and top-bottom,
/// and an absent field claims flat, mono video, so those are refused rather than dropped.
fn matroska_spatial(
	children: &[MatroskaSpec],
) -> Result<(Option<hang::catalog::Projection>, Option<hang::catalog::Stereo>)> {
	let mut stereo = None;
	let mut projection = None;
	for child in children {
		match child {
			// 0 is mono, 1 side by side left eye first, 3 top-bottom left eye first. The rest put the
			// right eye first, interleave, checkerboard, or anaglyph, or lace the eyes in one block.
			MatroskaSpec::StereoMode(mode) => {
				stereo = match mode {
					0 => None,
					1 => Some(hang::catalog::Stereo::LeftRight),
					3 => Some(hang::catalog::Stereo::TopBottom),
					_ => return Err(Error::UnsupportedSpatial("StereoMode").into()),
				}
			}
			MatroskaSpec::Projection(Master::Full(fields)) => projection = matroska_projection(fields)?,
			_ => {}
		}
	}
	Ok((projection, stereo))
}

/// The size of an `equi` box's body: version and flags, then four 32-bit bounds.
const EQUI_PRIVATE_LEN: u64 = 20;

/// The projection a Matroska `Projection` element signals, `None` for a rectangular one.
fn matroska_projection(fields: &[MatroskaSpec]) -> Result<Option<hang::catalog::Projection>> {
	let unsupported = Error::UnsupportedSpatial;
	let (mut kind, mut private) = (0, None);
	for field in fields {
		match field {
			MatroskaSpec::ProjectionType(v) => kind = *v,
			MatroskaSpec::ProjectionPrivate(v) => private = Some(v.as_slice()),
			MatroskaSpec::ProjectionPoseYaw(v)
			| MatroskaSpec::ProjectionPosePitch(v)
			| MatroskaSpec::ProjectionPoseRoll(v)
				if *v != 0.0 =>
			{
				return Err(unsupported("nonzero ProjectionPose").into());
			}
			_ => {}
		}
	}
	match kind {
		0 => Ok(None),
		// The private data is an `equi` box's body: version and flags, then the top, bottom, left,
		// and right bounds. Without it the picture covers the whole sphere, as FFmpeg reads it.
		1 => {
			let [top, bottom, left, right] = match private {
				None => [0; 4],
				Some(&[0, _, _, _, ref bounds @ ..]) if bounds.len() == 16 => {
					std::array::from_fn(|i| u32::from_be_bytes(bounds[i * 4..][..4].try_into().unwrap()))
				}
				Some(_) => return Err(unsupported("equirectangular ProjectionPrivate").into()),
			};
			let projection = crate::codec::spatial::equirectangular(top, bottom, left, right).map_err(unsupported)?;
			Ok(Some(projection))
		}
		2 => Err(unsupported("cubemap projection").into()),
		3 => Err(unsupported("mesh projection").into()),
		_ => Err(unsupported("ProjectionType").into()),
	}
}

/// The colour a Matroska `Colour` element signals. Its code points are H.273's, `Range` counts 0
/// as unspecified, 1 as limited, and 2 as full, and a partial mastering display is dropped. A code
/// point past 255 or a light level past 65535 has no representation, so it is refused.
pub(super) fn matroska_color(children: &[MatroskaSpec]) -> Result<Option<hang::catalog::Color>> {
	use hang::catalog::{Chromaticity, Color, ContentLight, Mastering};

	let (mut primaries, mut transfer, mut matrix, mut full_range) = (2, 2, 2, None);
	let (mut max_cll, mut max_fall) = (None, None);
	let mut mastering = None;
	let code = |value: u64| u8::try_from(value).map_err(|_| Error::InvalidColour("code point"));
	let light = |value: u64| u16::try_from(value).map_err(|_| Error::InvalidColour("light level"));
	for child in children {
		match child {
			MatroskaSpec::Primaries(v) => primaries = code(*v)?,
			MatroskaSpec::TransferCharacteristics(v) => transfer = code(*v)?,
			MatroskaSpec::MatrixCoefficients(v) => matrix = code(*v)?,
			MatroskaSpec::Range(1) => full_range = Some(false),
			MatroskaSpec::Range(2) => full_range = Some(true),
			MatroskaSpec::MaxCLL(v) => max_cll = Some(light(*v)?),
			MatroskaSpec::MaxFALL(v) => max_fall = Some(light(*v)?),
			MatroskaSpec::MasteringMetadata(Master::Full(fields)) => {
				// Red, green, blue, and white x and y, then the max and min luminance.
				let mut v = [None; 10];
				for field in fields {
					let (i, value) = match field {
						MatroskaSpec::PrimaryRChromaticityX(x) => (0, x),
						MatroskaSpec::PrimaryRChromaticityY(x) => (1, x),
						MatroskaSpec::PrimaryGChromaticityX(x) => (2, x),
						MatroskaSpec::PrimaryGChromaticityY(x) => (3, x),
						MatroskaSpec::PrimaryBChromaticityX(x) => (4, x),
						MatroskaSpec::PrimaryBChromaticityY(x) => (5, x),
						MatroskaSpec::WhitePointChromaticityX(x) => (6, x),
						MatroskaSpec::WhitePointChromaticityY(x) => (7, x),
						MatroskaSpec::LuminanceMax(x) => (8, x),
						MatroskaSpec::LuminanceMin(x) => (9, x),
						_ => continue,
					};
					// Chromaticities lie in 0..=1 and luminances are nonnegative (Matroska's ranges).
					let valid = match i {
						0..=7 => (0.0..=1.0).contains(value),
						_ => value.is_finite() && *value >= 0.0,
					};
					if !valid {
						return Err(Error::InvalidColour("mastering value").into());
					}
					v[i] = Some(*value);
				}
				let xy = |i: usize| Some(Chromaticity { x: v[i]?, y: v[i + 1]? });
				mastering = (|| {
					Some(Mastering {
						red: xy(0)?,
						green: xy(2)?,
						blue: xy(4)?,
						white: xy(6)?,
						max_luminance: v[8]?,
						min_luminance: v[9]?,
					})
				})();
			}
			_ => {}
		}
	}

	let mut color = crate::codec::color::cicp(primaries, transfer, matrix, full_range).unwrap_or_default();
	color.mastering = mastering;
	if max_cll.is_some() || max_fall.is_some() {
		color.content_light = Some(ContentLight {
			max_cll: max_cll.unwrap_or(0),
			max_fall: max_fall.unwrap_or(0),
		});
	}
	Ok((color != Color::default()).then_some(color))
}

fn build_audio_config(
	codec_id: &str,
	codec_private: Option<&Bytes>,
	audio_children: Option<&[MatroskaSpec]>,
) -> Result<AudioConfig> {
	// Reject an unsupported codec before interpreting its codec-specific fields.
	let mut config = match codec_id {
		"A_OPUS" => {
			// The catalog describes the decoder's output: Opus always decodes at 48 kHz, whatever the
			// informational input rate in the OpusHead or SamplingFrequency claims.
			let mut config = match codec_private {
				Some(head) => crate::codec::opus::config(head)?,
				None => crate::codec::opus::Config::new(48_000, 0).into(),
			};
			config.description = codec_private.cloned();
			config
		}
		"A_AAC" => {
			let priv_data = codec_private.ok_or(Error::MissingCodecPrivate {
				codec_id: "A_AAC",
				purpose: "AudioSpecificConfig",
			})?;
			let mut cursor = priv_data.clone();
			let cfg = crate::codec::aac::Config::parse(&mut cursor)?;
			let mut config = AudioConfig::new(AAC { profile: cfg.profile }, cfg.sample_rate, cfg.channel_count);
			config.description = Some(priv_data.clone());
			config
		}
		"A_FLAC" => {
			// Matroska A_FLAC CodecPrivate is the FLAC header: the `fLaC` marker
			// followed by the metadata blocks (STREAMINFO first). That is exactly the
			// WebCodecs FLAC description, so it passes straight through, and STREAMINFO
			// is authoritative for rate/channels.
			let priv_data = codec_private.ok_or(Error::MissingCodecPrivate {
				codec_id: "A_FLAC",
				purpose: "FLAC STREAMINFO",
			})?;
			let mut cursor = priv_data.clone();
			let cfg = crate::codec::flac::Config::parse(&mut cursor)?;
			let mut config = AudioConfig::new(AudioCodec::Flac, cfg.sample_rate, cfg.channel_count);
			config.description = Some(priv_data.clone());
			config
		}
		// MP3 carries its config in band; the track header supplies rate and channels.
		"A_MPEG/L3" => AudioConfig::new(AudioCodec::Mp3, 0, 0),
		other => return Err(Error::UnsupportedAudioCodec(other.to_string()).into()),
	};

	let mut sample_rate = 0;
	let mut channels = 0;
	for child in audio_children.unwrap_or_default() {
		match child {
			MatroskaSpec::SamplingFrequency(v) => {
				if !(0.0..=f64::from(u32::MAX)).contains(v) {
					return Err(Error::ValueOutOfRange("SamplingFrequency").into());
				}
				sample_rate = *v as u32;
			}
			MatroskaSpec::Channels(v) => {
				channels = u32::try_from(*v).map_err(|_| Error::ValueOutOfRange("Channels"))?;
			}
			_ => {}
		}
	}
	if config.sample_rate == 0 {
		config.sample_rate = sample_rate;
	}
	if config.channel_count == 0 {
		config.channel_count = channels;
	}
	Ok(config)
}

fn build_h264_config(codec_private: Option<&Bytes>) -> Result<VideoConfig> {
	let avcc_bytes = codec_private.ok_or(Error::MissingCodecPrivate {
		codec_id: "V_MPEG4/ISO/AVC",
		purpose: "AVCDecoderConfigurationRecord",
	})?;
	crate::codec::h264::config(avcc_bytes)
}

fn build_h265_config(codec_private: Option<&Bytes>) -> Result<VideoConfig> {
	let hvcc_data = codec_private.ok_or(Error::MissingCodecPrivate {
		codec_id: "V_MPEGH/ISO/HEVC",
		purpose: "HEVCDecoderConfigurationRecord",
	})?;
	let mut cursor = Cursor::new(hvcc_data.as_ref());
	let hvcc = mp4_atom::Hvcc::decode_body(&mut cursor).map_err(|_| Error::InvalidHvcc)?;

	let mut description = BytesMut::new();
	hvcc.encode_body(&mut description)?;

	let mut config = VideoConfig::new(H265 {
		in_band: false,
		profile_space: hvcc.general_profile_space,
		profile_idc: hvcc.general_profile_idc,
		profile_compatibility_flags: hvcc.general_profile_compatibility_flags,
		tier_flag: hvcc.general_tier_flag,
		level_idc: hvcc.general_level_idc,
		constraint_flags: hvcc.general_constraint_indicator_flags,
	});
	config.description = Some(description.freeze());
	crate::codec::h265::apply_hvcc_sps(&hvcc, &mut config)?;
	Ok(config)
}

fn build_av1_config(codec_private: Option<&Bytes>) -> Result<VideoConfig> {
	let av1c_data = codec_private.ok_or(Error::MissingCodecPrivate {
		codec_id: "V_AV1",
		purpose: "AV1CodecConfigurationRecord",
	})?;
	let mut cursor = Cursor::new(av1c_data.as_ref());
	let av1c = mp4_atom::Av1c::decode_body(&mut cursor).map_err(|_| Error::InvalidAv1c)?;

	let mut description = BytesMut::new();
	av1c.encode_body(&mut description)?;

	let mut config = VideoConfig::new(crate::codec::av1::av1_from_av1c(&av1c));
	config.color = crate::codec::av1::color(&av1c.config_obus);
	config.description = Some(description.freeze());
	Ok(config)
}

#[derive(Debug, thiserror::Error)]
enum LacingError {
	#[error("invalid MKV lacing")]
	Invalid,
	#[error("MKV lacing requires a positive TrackEntry DefaultDuration")]
	MissingDuration,
}

impl Lacing {
	/// Take the next lace header byte, returning every frame's size once the header ends.
	/// `remaining` counts the body bytes after `byte`; the last frame takes what the others leave.
	// webm-iterable's splitter uses unchecked indexing and size arithmetic on malformed laces, so
	// sizes and signed EBML deltas stay checked here.
	fn push(&mut self, byte: u8, remaining: u64) -> std::result::Result<Option<Vec<u64>>, LacingError> {
		let invalid = LacingError::Invalid;
		let Some(count) = self.count else {
			let count = usize::from(byte) + 1;
			if count < 2 {
				return Err(invalid);
			}
			self.count = Some(count);
			if self.lace != Lace::FixedSize {
				return Ok(None);
			}
			let frames = count as u64;
			if !remaining.is_multiple_of(frames) {
				return Err(invalid);
			}
			return Ok(Some(vec![remaining / frames; count]));
		};
		match self.lace {
			Lace::Xiph => {
				self.sum = self.sum.checked_add(byte.into()).ok_or(invalid)?;
				if byte == 255 {
					return Ok(None);
				}
				self.sizes.push(std::mem::take(&mut self.sum));
			}
			Lace::Ebml => {
				self.vint.push(byte);
				let first = self.vint[0];
				let len = first.leading_zeros() as usize + 1;
				if len > 8 {
					return Err(invalid);
				}
				if self.vint.len() < len {
					return Ok(None);
				}
				let mut value = u64::from(first) & (0xff >> len);
				for byte in &self.vint[1..] {
					value = (value << 8) | u64::from(*byte);
				}
				self.vint.clear();
				let size = match self.sizes.last() {
					None => value,
					Some(&previous) => {
						let delta = value as i64 - ((1i64 << (7 * len - 1)) - 1);
						previous.checked_add_signed(delta).ok_or(invalid)?
					}
				};
				self.sizes.push(size);
			}
			Lace::FixedSize => unreachable!("fixed-size lacing has no sizes"),
		}
		if self.sizes.len() < count - 1 {
			return Ok(None);
		}
		let sum = self
			.sizes
			.iter()
			.try_fold(0u64, |sum, size| sum.checked_add(*size))
			.ok_or(LacingError::Invalid)?;
		let last = remaining.checked_sub(sum).ok_or(LacingError::Invalid)?;
		let mut sizes = std::mem::take(&mut self.sizes);
		sizes.push(last);
		Ok(Some(sizes))
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use webm_iterable::{WebmWriter, WriteOptions};

	#[test]
	fn ignored_payload_does_not_accumulate_under_a_large_metadata_master() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
		let mut import = Import::new(broadcast, catalog.reserve());
		// A 1 GiB EBML header containing a nearly 1 GiB Void. Neither declaration is
		// a reason to retain ignored bytes while waiting for the master to finish.
		import.decode(&[0x1a, 0x45, 0xdf, 0xa3, 0x08, 0x40, 0, 0, 0]).unwrap();
		import.decode(&[0xec, 0x08, 0x3f, 0xff, 0xff, 0xfa]).unwrap();
		for _ in 0..64 {
			import.decode(&[0; 1024]).unwrap();
			assert!(
				import.reader.retained() <= 12,
				"ignored bytes retained: {}",
				import.reader.retained()
			);
		}
		assert!(
			matches!(import.finish(), Err(crate::Error::Mkv(Error::Truncated))),
			"truncated ignored payload must fail"
		);
	}

	#[test]
	fn completed_live_elements_do_not_accumulate_a_metadata_prefix() {
		let mut header = Vec::new();
		{
			let mut writer = WebmWriter::new(&mut header);
			writer
				.write(&MatroskaSpec::Ebml(Master::Full(vec![MatroskaSpec::DocType(
					"webm".into(),
				)])))
				.unwrap();
			writer
				.write_advanced(
					&MatroskaSpec::Segment(Master::Start),
					WriteOptions::is_unknown_sized_element(),
				)
				.unwrap();
			writer.flush().unwrap();
		}
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
		let mut import = Import::new(broadcast, catalog.reserve());
		import.decode(&header).unwrap();
		let mut consumed = header.len() as u64;
		for timestamp in [0, u8::MAX as u64, u16::MAX as u64, u32::MAX as u64] {
			let mut cluster = Vec::new();
			{
				let mut writer = WebmWriter::new(&mut cluster);
				writer
					.write_advanced(
						&MatroskaSpec::Cluster(Master::Start),
						WriteOptions::is_unknown_sized_element(),
					)
					.unwrap();
				// This fragment is received under the importer's already-open Segment.
				writer.write_raw(0xe7, &timestamp.to_be_bytes()).unwrap();
				writer.flush().unwrap();
			}
			for byte in &cluster {
				import.decode(&[*byte]).unwrap();
				consumed += 1;
				assert_eq!(import.reader.position(), consumed);
				// At most a split element header and the 8-byte timestamp being read.
				assert!(import.reader.retained() <= 12 + 8, "{}", import.reader.retained());
			}
			assert_eq!(import.cluster_timestamp, timestamp);
		}
		import.finish().unwrap();
		import.finish().unwrap();
		assert!(matches!(
			import.decode(&[0]),
			Err(crate::Error::Mkv(Error::TracksAfterFinish))
		));
	}

	/// A WebM prefix declaring `entries`, leaving an unknown-size Segment and Cluster open.
	fn live(entries: Vec<MatroskaSpec>) -> Vec<u8> {
		let mut data = Vec::new();
		{
			let mut writer = WebmWriter::new(&mut data);
			writer
				.write(&MatroskaSpec::Ebml(Master::Full(vec![MatroskaSpec::DocType(
					"webm".into(),
				)])))
				.unwrap();
			writer
				.write_advanced(
					&MatroskaSpec::Segment(Master::Start),
					WriteOptions::is_unknown_sized_element(),
				)
				.unwrap();
			writer.write(&MatroskaSpec::Tracks(Master::Full(entries))).unwrap();
			writer.flush().unwrap();
		}
		// An unknown-size Cluster at timestamp 0.
		data.extend_from_slice(&[0x1f, 0x43, 0xb6, 0x75, 0xff, 0xe7, 0x81, 0x00]);
		data
	}

	fn entry(number: u64, track_type: u64, codec: &str) -> MatroskaSpec {
		MatroskaSpec::TrackEntry(Master::Full(vec![
			MatroskaSpec::TrackNumber(number),
			MatroskaSpec::TrackType(track_type),
			MatroskaSpec::CodecID(codec.into()),
		]))
	}

	/// An element header declaring `size` body bytes.
	fn header(id: &[u8], size: u64) -> Vec<u8> {
		let mut header = id.to_vec();
		header.push(0x01);
		header.extend_from_slice(&size.to_be_bytes()[1..]);
		header
	}

	/// A SimpleBlock header for a keyframe on `track` declaring `size` payload bytes.
	fn simple_block(track: u8, size: u64) -> Vec<u8> {
		let mut block = header(&[0xa3], size + 4);
		block.extend_from_slice(&[0x80 | track, 0, 0, 0x80]);
		block
	}

	fn import() -> (Import, crate::catalog::Producer, moq_net::broadcast::Consumer) {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
		let import = Import::new(broadcast, catalog.reserve());
		(import, catalog, consumer)
	}

	async fn first_frame(
		catalog: &crate::catalog::Producer,
		consumer: &moq_net::broadcast::Consumer,
	) -> moq_net::frame::Consumer {
		let name = catalog.snapshot().video.renditions.keys().next().unwrap().clone();
		let mut track = consumer.track(&name).unwrap().subscribe(None).await.unwrap();
		let mut group = track.recv_group().await.unwrap().unwrap();
		group.next_frame().await.unwrap().unwrap()
	}

	const WAIT: std::time::Duration = std::time::Duration::from_secs(5);

	#[tokio::test]
	async fn a_block_streams_into_its_frame_before_it_completes() {
		const SIZE: usize = 1024 * 1024;
		let (mut import, catalog, consumer) = import();
		import.decode(&live(vec![entry(1, 1, "V_VP9")])).unwrap();
		import.decode(&simple_block(1, SIZE as u64)).unwrap();
		import.decode(&[1; 1024]).unwrap();

		let mut frame = first_frame(&catalog, &consumer).await;
		// The Legacy timestamp prefix and the first chunk are readable while the rest is in flight.
		let mut read = Vec::new();
		while read.len() < 1 + 1024 {
			let chunk = kio::clock::timeout(WAIT, frame.read_chunk()).await.unwrap().unwrap();
			read.extend_from_slice(&chunk.expect("the frame is still open"));
		}
		assert_eq!(read, [&[0][..], &[1; 1024]].concat());

		for _ in 1..SIZE / 1024 {
			import.decode(&[1; 1024]).unwrap();
			assert!(import.reader.retained() <= 12, "{}", import.reader.retained());
		}
		while let Some(chunk) = kio::clock::timeout(WAIT, frame.read_chunk()).await.unwrap().unwrap() {
			read.extend_from_slice(&chunk);
		}
		assert_eq!(read.len(), 1 + SIZE);
		import.finish().unwrap();
	}

	#[test]
	fn a_block_past_the_frame_limit_is_refused_before_its_payload() {
		let (mut import, _catalog, _consumer) = import();
		import.decode(&live(vec![entry(1, 1, "V_VP9")])).unwrap();
		let err = import.decode(&simple_block(1, 1 << 40)).unwrap_err();
		assert!(
			matches!(err, crate::Error::Moq(moq_net::Error::FrameTooLarge)),
			"{err:?}"
		);
		assert!(import.reader.retained() <= 12);
		// The rest of the refused chunk was never read, so later input cannot continue it.
		assert!(matches!(
			import.decode(&[0]),
			Err(crate::Error::Moq(moq_net::Error::FrameTooLarge))
		));
		import.finish().unwrap();
	}

	#[test]
	fn a_track_entry_publishes_before_its_tracks_master_ends() {
		let (mut import, _catalog, _consumer) = import();
		let mut data = Vec::new();
		{
			let mut writer = WebmWriter::new(&mut data);
			writer
				.write(&MatroskaSpec::Ebml(Master::Full(vec![MatroskaSpec::DocType(
					"webm".into(),
				)])))
				.unwrap();
			writer
				.write_advanced(
					&MatroskaSpec::Segment(Master::Start),
					WriteOptions::is_unknown_sized_element(),
				)
				.unwrap();
			writer.flush().unwrap();
		}
		// Tracks declares 1 GiB, of which the entry is a few bytes and a Void claims the rest.
		data.extend(header(&[0x16, 0x54, 0xae, 0x6b], 1 << 30));
		// TrackEntry: TrackNumber 1, TrackType 1 (video), CodecID V_VP9.
		data.extend_from_slice(&[0xae, 0x8d, 0xd7, 0x81, 0x01, 0x83, 0x81, 0x01, 0x86, 0x85]);
		data.extend_from_slice(b"V_VP9");
		import.decode(&data).unwrap();
		assert_eq!(import.tracks.len(), 1);
		assert!(import.fields.is_empty(), "the entry's fields were released");

		import.decode(&header(&[0xec], (1 << 30) - 64)).unwrap();
		for _ in 0..64 {
			import.decode(&[0; 1024]).unwrap();
			assert!(import.reader.retained() <= 12, "{}", import.reader.retained());
		}
		assert!(matches!(import.finish(), Err(crate::Error::Mkv(Error::Truncated))));
	}

	/// A WebM prefix with an unknown-size Segment, then a Tracks holding one VP9 TrackEntry whose
	/// last child is `field`'s header, so `size` body bytes of it remain.
	fn entry_ending_in(field: &[u8], size: u64) -> Vec<u8> {
		let mut data = Vec::new();
		{
			let mut writer = WebmWriter::new(&mut data);
			writer
				.write(&MatroskaSpec::Ebml(Master::Full(vec![MatroskaSpec::DocType(
					"webm".into(),
				)])))
				.unwrap();
			writer
				.write_advanced(
					&MatroskaSpec::Segment(Master::Start),
					WriteOptions::is_unknown_sized_element(),
				)
				.unwrap();
			writer.flush().unwrap();
		}
		// TrackNumber 1, TrackType 1 (video), CodecID V_VP9.
		let mut entry = vec![0xd7, 0x81, 0x01, 0x83, 0x81, 0x01, 0x86, 0x85];
		entry.extend_from_slice(b"V_VP9");
		entry.extend_from_slice(field);
		let entry_size = entry.len() as u64 + size;
		let mut tracks = header(&[0xae], entry_size);
		tracks.extend(entry);
		data.extend(header(&[0x16, 0x54, 0xae, 0x6b], tracks.len() as u64 + size));
		data.extend(tracks);
		data
	}

	/// Feed `size` body bytes in 1 KiB chunks, checking none are held, and return the last
	/// chunk's result.
	fn feed_unretained(import: &mut Import, size: u64) -> crate::Result<()> {
		for _ in 1..size / 1024 {
			import.decode(&[b'a'; 1024]).unwrap();
			assert!(import.reader.retained() <= 12, "{}", import.reader.retained());
		}
		import.decode(&[b'a'; 1024])
	}

	#[test]
	fn unused_track_fields_are_discarded_as_they_arrive() {
		const SIZE: u64 = 1024 * 1024;
		// Name, Language, and LanguageIETF have no effect on output.
		for id in [&[0x53, 0x6e][..], &[0x22, 0xb5, 0x9c], &[0x22, 0xb5, 0x9d]] {
			let (mut import, _catalog, _consumer) = import();
			import.decode(&entry_ending_in(&header(id, SIZE), SIZE)).unwrap();
			feed_unretained(&mut import, SIZE).unwrap();
			assert_eq!(import.tracks.len(), 1, "{id:x?}");
			import.finish().unwrap();
		}
	}

	#[test]
	fn unsupported_projection_private_data_is_not_held() {
		const SIZE: u64 = 1024 * 1024;
		for projection_type in [0, 1] {
			// Video: PixelWidth 320, PixelHeight 240, then Projection: ProjectionType, then the
			// ProjectionPrivate header.
			let mut projection = vec![0x76, 0x71, 0x81, projection_type];
			projection.extend(header(&[0x76, 0x72], SIZE));
			let mut video = vec![0xb0, 0x82, 0x01, 0x40, 0xba, 0x81, 0xf0];
			video.extend(header(&[0x76, 0x70], projection.len() as u64 + SIZE));
			video.extend(projection);
			let mut field = header(&[0xe0], video.len() as u64 + SIZE);
			field.extend(video);

			let (mut import, _catalog, _consumer) = import();
			import.decode(&entry_ending_in(&field, SIZE)).unwrap();
			let result = feed_unretained(&mut import, SIZE);
			match projection_type {
				// A rectangular projection ignores its private data.
				0 => {
					result.unwrap();
					assert_eq!(import.tracks.len(), 1);
				}
				// An equirectangular one is refused, as any size but an `equi` body is.
				_ => assert!(
					matches!(result, Err(crate::Error::Mkv(Error::UnsupportedSpatial(_)))),
					"{result:?}"
				),
			}
		}
	}

	#[tokio::test]
	async fn skipped_and_unknown_track_payloads_are_discarded_as_they_arrive() {
		let (mut import, catalog, consumer) = import();
		// Track 2 is a role the importer skips; track 9 is never declared.
		import
			.decode(&live(vec![entry(1, 1, "V_VP9"), entry(2, 3, "B_VOBBTN")]))
			.unwrap();
		assert_eq!(import.tracks.len(), 1);
		for track in [2, 9] {
			import.decode(&simple_block(track, 1024 * 1024)).unwrap();
			for _ in 0..1024 {
				import.decode(&[2; 1024]).unwrap();
				assert!(import.reader.retained() <= 12);
			}
		}
		import.decode(&simple_block(1, 3)).unwrap();
		import.decode(b"key").unwrap();
		let mut frame = first_frame(&catalog, &consumer).await;
		assert_eq!(frame.read_all().await.unwrap(), b"\0key"[..]);
		import.finish().unwrap();
	}

	/// Output nobody reads is evicted by its track, so the importer keeps accepting input.
	#[test]
	fn unread_output_does_not_stall_the_importer() {
		let (mut import, _catalog, _consumer) = import();
		import.decode(&live(vec![entry(1, 1, "V_VP9")])).unwrap();
		let payload = vec![3; 1024 * 1024];
		// Twice the most a track holds.
		for _ in 0..64 {
			import.decode(&simple_block(1, payload.len() as u64)).unwrap();
			import.decode(&payload).unwrap();
		}
		import.finish().unwrap();
	}

	#[tokio::test]
	async fn truncation_fails_the_frame_cut_short() {
		let (mut import, catalog, consumer) = import();
		import.decode(&live(vec![entry(1, 1, "V_VP9")])).unwrap();
		import.decode(&simple_block(1, 1024)).unwrap();
		import.decode(&[4; 512]).unwrap();
		let mut frame = first_frame(&catalog, &consumer).await;
		assert!(matches!(import.finish(), Err(crate::Error::Mkv(Error::Truncated))));
		// The importer is still owned, and the frame fails rather than waiting on it.
		let read = kio::clock::timeout(WAIT, frame.read_all()).await.unwrap();
		assert!(read.is_err(), "a truncated frame must not read as complete: {read:?}");
		drop(import);
	}

	#[tokio::test]
	async fn an_abort_fails_the_frame_cut_short() {
		for drop in [false, true] {
			let (mut import, catalog, consumer) = import();
			import.decode(&live(vec![entry(1, 1, "V_VP9")])).unwrap();
			import.decode(&simple_block(1, 1024)).unwrap();
			import.decode(&[4; 512]).unwrap();
			let mut frame = first_frame(&catalog, &consumer).await;
			match drop {
				true => std::mem::drop(import),
				false => import.abort(moq_net::Error::Cancel),
			}
			let read = kio::clock::timeout(WAIT, frame.read_all()).await.unwrap();
			assert!(read.is_err(), "a frame cut short must not read as complete: {read:?}");
		}
	}

	#[test]
	fn input_after_a_failed_finish_is_refused() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
		let mut import = Import::new(broadcast, catalog.reserve());
		// An EBML header declaring a byte it never sends.
		import.decode(&[0x1a, 0x45, 0xdf, 0xa3, 0x81]).unwrap();
		assert!(matches!(import.finish(), Err(crate::Error::Mkv(Error::Truncated))));
		assert!(matches!(
			import.decode(&[0]),
			Err(crate::Error::Mkv(Error::TracksAfterFinish))
		));
	}
}
