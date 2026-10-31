use std::collections::{HashMap, HashSet};
use std::convert::TryFrom;
use std::io::Cursor;

use crate::Result;
use bytes::{Bytes, BytesMut};
use hang::catalog::{AAC, AudioCodec, AudioConfig, H265, VP9, VideoCodec, VideoConfig};
use moq_net::Timestamp;
use mp4_atom::Atom;
use webm_iterable::matroska_spec::{BlockLacing, Master, MatroskaSpec, SimpleBlock};

use super::Error;

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
/// **Text:** timed SRT (`S_TEXT/UTF8`) and WebVTT (`S_TEXT/WEBVTT`), emitted as WebVTT.
///
/// Laced blocks require a positive TrackEntry DefaultDuration to timestamp each frame.
///
/// Unsupported track roles are skipped; malformed tracks and unsupported codecs fail the import.
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

	/// Accumulated unparsed input.
	buffer: BytesMut,
	/// Incremental element state; an incomplete master retains its own children across chunks.
	decoder: ebml_iterable::TagDecoder<MatroskaSpec>,
	/// Whether `finish()` ended the input, even if it failed before the decoder drained.
	finished: bool,
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
	Text,
}

impl TrackKind {
	/// The publisher priority for this kind of media, so audio isn't stuck behind a
	/// video backlog on a busy connection.
	fn priority(&self) -> i8 {
		match self {
			Self::Video => hang::catalog::PRIORITY.video,
			Self::Audio => hang::catalog::PRIORITY.audio,
			Self::Text => hang::catalog::PRIORITY.text,
		}
	}
}

struct MkvTrack {
	default_duration: Option<std::time::Duration>,
	kind: TrackKind,
	text_header: Option<String>,
	track: Media,
	group: Option<moq_net::group::Producer>,
}

/// BlockGroup fields that only subtitle tracks consume.
#[derive(Default)]
struct Cue<'a> {
	/// BlockDuration, in Matroska ticks.
	duration: Option<u64>,
	/// WebVTT settings, identifier and comments (BlockAddID 1).
	addition: Option<&'a [u8]>,
	/// Whether the BlockGroup replaces the codec initialization (CodecState).
	codec_state: bool,
}

enum Media {
	Video(crate::container::Producer<crate::catalog::hang::Container, VideoConfig>),
	Audio(crate::container::Producer<crate::catalog::hang::Container, AudioConfig>),
	Text(crate::container::Producer<crate::catalog::hang::Container, hang::catalog::TextConfig>),
}

impl Media {
	fn write(&mut self, frame: crate::container::Frame) -> crate::Result<()> {
		match self {
			Self::Video(track) => track.write(frame),
			Self::Audio(track) => track.write(frame),
			Self::Text(track) => track.write(frame),
		}
	}

	fn cut(&mut self, timestamp: Option<Timestamp>) -> crate::Result<()> {
		match self {
			Self::Video(track) => track.cut(timestamp),
			Self::Audio(track) => track.cut(timestamp),
			Self::Text(track) => track.cut(timestamp),
		}
	}

	fn seek(&mut self, sequence: u64) -> crate::Result<()> {
		match self {
			Self::Video(track) => track.seek(sequence),
			Self::Audio(track) => track.seek(sequence),
			Self::Text(track) => track.seek(sequence),
		}
	}

	fn finish(&mut self) -> crate::Result<()> {
		match self {
			Self::Video(track) => track.finish(),
			Self::Audio(track) => track.finish(),
			Self::Text(track) => track.finish(),
		}
	}

	fn end(&mut self) -> crate::Result<()> {
		match self {
			Self::Video(track) => track.end(),
			Self::Audio(track) => track.end(),
			Self::Text(track) => track.end(),
		}
	}

	fn abort(self, err: moq_net::Error) {
		match self {
			Self::Video(track) => track.abort(err),
			Self::Audio(track) => track.abort(err),
			Self::Text(track) => track.abort(err),
		}
	}

	fn name(&self) -> &str {
		match self {
			Self::Video(track) => track.name(),
			Self::Audio(track) => track.name(),
			Self::Text(track) => track.name(),
		}
	}
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
			buffer: BytesMut::new(),
			decoder: ebml_iterable::TagDecoder::new(&[
				MatroskaSpec::Ebml(Master::Start),
				MatroskaSpec::Info(Master::Start),
				MatroskaSpec::Tracks(Master::Start),
				MatroskaSpec::TrackEntry(Master::Start),
				MatroskaSpec::Audio(Master::Start),
				MatroskaSpec::Video(Master::Start),
				MatroskaSpec::BlockGroup(Master::Start),
			]),
			finished: false,
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
	pub fn with_container(mut self, container: hang::catalog::Container) -> Self {
		self.container = container;
		self
	}

	/// Append the buffer to the internal scratch and parse as many tags as possible.
	///
	/// The buffer is fully consumed on every call (data is moved into the internal
	/// scratch). Bytes that cannot yet form a complete top-level tag are retained
	/// for the next call.
	pub fn decode(&mut self, data: &[u8]) -> Result<()> {
		if self.finished && !data.is_empty() {
			return Err(Error::TracksAfterFinish.into());
		}
		self.buffer.extend_from_slice(data);

		self.drain()
	}

	/// Decode complete elements, leaving incomplete bytes and master state for the next chunk.
	fn drain(&mut self) -> Result<()> {
		while let Some(tag) = self
			.decoder
			.decode(&mut self.buffer)
			.map_err(|_| Error::MatroskaParse)?
		{
			self.handle_tag(tag.tag)?;
		}
		Ok(())
	}

	fn handle_tag(&mut self, tag: MatroskaSpec) -> Result<()> {
		match tag {
			MatroskaSpec::Ebml(Master::Full(children)) => {
				self.handle_ebml(&children)?;
			}
			MatroskaSpec::Segment(Master::Start) => {
				// Just descend.
			}
			MatroskaSpec::Segment(Master::End) => {}
			MatroskaSpec::Info(Master::Full(children)) => {
				for c in &children {
					if let MatroskaSpec::TimestampScale(v) = c {
						if *v == 0 {
							return Err(Error::ValueOutOfRange("TimestampScale").into());
						}
						self.timestamp_scale_ns = *v;
					}
				}
			}
			// A second Tracks element would redeclare a published track set.
			MatroskaSpec::Tracks(Master::Full(children)) if !self.tracks_seen => {
				// Only `finish()` releases the reservation before Tracks, since a block needs a track.
				let reserved = self.initial_reservation.clone().ok_or(Error::TracksAfterFinish)?;
				self.handle_tracks(&reserved, children)?;
				self.tracks_seen = true;
				// The reservation stays held until the first block anchors the clock, unless no track
				// was declared: then no block ever will.
				if self.tracks.is_empty() {
					self.initial_reservation = None;
				}
			}
			MatroskaSpec::Cluster(Master::Start) => {
				self.cluster_timestamp = 0;
			}
			MatroskaSpec::Cluster(Master::End) => {}
			MatroskaSpec::Timestamp(v) => {
				// Within a Cluster, this is the cluster timestamp (in Matroska ticks).
				self.cluster_timestamp = v;
			}
			MatroskaSpec::SimpleBlock(ref data) => {
				let sb = SimpleBlock::try_from(data.as_slice()).map_err(|_| Error::InvalidSimpleBlock)?;
				self.handle_block(&sb, sb.keyframe, Cue::default())?;
			}
			MatroskaSpec::BlockGroup(Master::Full(children)) => {
				self.handle_block_group(&children)?;
			}
			// Tags we deliberately ignore.
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

	fn handle_tracks(&mut self, reserved: &crate::catalog::Reserved<E>, entries: Vec<MatroskaSpec>) -> Result<()> {
		let mut numbers = HashSet::new();
		for entry in entries {
			if let MatroskaSpec::TrackEntry(Master::Full(children)) = entry {
				self.add_track(reserved, children, &mut numbers)?;
			}
		}
		Ok(())
	}

	fn add_track(
		&mut self,
		reserved: &crate::catalog::Reserved<E>,
		children: Vec<MatroskaSpec>,
		numbers: &mut HashSet<u64>,
	) -> Result<()> {
		let mut track_number: Option<u64> = None;
		let mut track_type: Option<u64> = None;
		let mut codec_id: Option<String> = None;
		let mut codec_private: Option<Bytes> = None;
		let mut audio_children: Option<Vec<MatroskaSpec>> = None;
		let mut video_children: Option<Vec<MatroskaSpec>> = None;
		let mut language = None;
		let mut language_ietf = None;
		let mut name = None;
		let mut default_duration = None;

		for c in children {
			match c {
				MatroskaSpec::DefaultDuration(v) if v > 0 => {
					default_duration = Some(std::time::Duration::from_nanos(v))
				}
				MatroskaSpec::TrackNumber(v) => {
					if track_number.replace(v).is_some() {
						return Err(moq_net::Error::Duplicate.into());
					}
				}
				MatroskaSpec::TrackType(v) => track_type = Some(v),
				MatroskaSpec::CodecID(v) => codec_id = Some(v),
				MatroskaSpec::Language(v) => language = Some(v),
				MatroskaSpec::LanguageIETF(v) => language_ietf = Some(v),
				MatroskaSpec::Name(v) => name = Some(v),
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
		if !numbers.insert(track_number) {
			return Err(moq_net::Error::Duplicate.into());
		}
		let track_type = track_type.ok_or(Error::MissingTrackType)?;
		let codec_id = codec_id.ok_or(Error::MissingCodecId)?;

		// Matroska TrackType: 1 = video, 2 = audio, 17 = subtitles.
		let (kind, suffix) = match track_type {
			1 => (TrackKind::Video, ".mkv-v"),
			2 => (TrackKind::Audio, ".mkv-a"),
			17 if matches!(codec_id.as_str(), "S_TEXT/UTF8" | "S_TEXT/WEBVTT") => (TrackKind::Text, ".mkv-t"),
			17 => return Err(crate::Error::UnknownFormat(format!("MKV subtitle codec {codec_id}"))),
			other => {
				tracing::warn!(track_type = other, codec_id, "unsupported MKV track type, skipping");
				return Ok(());
			}
		};

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
				TrackKind::Text => crate::container::Kind::Data,
			},
		)?;
		let text_header = if codec_id == "S_TEXT/WEBVTT" {
			let private = codec_private
				.as_deref()
				.ok_or(Error::InvalidSubtitle("missing WebVTT codec private"))?;
			let header =
				std::str::from_utf8(private).map_err(|_| Error::InvalidSubtitle("invalid WebVTT header UTF-8"))?;
			Some(crate::container::text::header(header).map_err(Error::InvalidSubtitle)?)
		} else {
			None
		};
		let media = match kind {
			TrackKind::Text => {
				let mut config = hang::catalog::TextConfig::new(hang::catalog::TextFormat::Vtt);
				if let Some(header) = &text_header {
					config.role = crate::container::text::role(header);
				}
				config.lang = language_ietf.or(language);
				config.label = name;
				config.container = self.container.clone();
				Media::Text(reserved.text(track, wire, config)?)
			}
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
				text_header,
				track: media,
				group: None,
			},
		);

		Ok(())
	}

	fn handle_block_group(&mut self, children: &[MatroskaSpec]) -> Result<()> {
		let mut block_data: Option<&[u8]> = None;
		let mut has_reference = false;
		let mut duration = None;
		let mut addition = None;
		let mut codec_state = false;

		for c in children {
			match c {
				MatroskaSpec::Block(data) => block_data = Some(data.as_slice()),
				MatroskaSpec::ReferenceBlock(_) => has_reference = true,
				MatroskaSpec::BlockAdditions(Master::Full(entries)) => {
					for entry in entries {
						if let MatroskaSpec::BlockMore(Master::Full(fields)) = entry {
							let mut id = 1;
							let mut data = None;
							for field in fields {
								match field {
									MatroskaSpec::BlockAddID(value) => id = *value,
									MatroskaSpec::BlockAdditional(value) => data = Some(value.as_slice()),
									_ => {}
								}
							}
							if id == 1 {
								if addition.is_some() {
									return Err(Error::InvalidSubtitle("duplicate cue additions").into());
								}
								addition = data;
							}
						}
					}
				}
				MatroskaSpec::BlockDuration(value) => duration = Some(*value),
				MatroskaSpec::CodecState(_) => codec_state = true,
				_ => {}
			}
		}

		let Some(data) = block_data else {
			return Ok(());
		};

		// `Block` has the same on-wire header as `SimpleBlock` minus the keyframe flag.
		// We parse it via `SimpleBlock::try_from` (which works on the raw slice) but
		// derive keyframe from the absence of `ReferenceBlock`.
		let parsed = SimpleBlock::try_from(data).map_err(|_| Error::InvalidBlock)?;
		let keyframe = !has_reference;

		self.handle_block(
			&parsed,
			keyframe,
			Cue {
				duration,
				addition,
				codec_state,
			},
		)
	}

	fn handle_block(&mut self, block: &SimpleBlock<'_>, keyframe: bool, cue: Cue<'_>) -> Result<()> {
		let Some(track) = self.tracks.get_mut(&block.track) else {
			// Unknown or skipped track.
			return Ok(());
		};

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
		if track.kind == TrackKind::Text {
			if block.lacing.is_some() {
				return Err(Error::InvalidSubtitle("laced subtitles are unsupported").into());
			}
			// A replaced WebVTT header would otherwise leave stale global blocks on every later cue.
			if cue.codec_state {
				return Err(Error::InvalidSubtitle("subtitle CodecState updates are unsupported").into());
			}
			let duration = match cue.duration {
				Some(ticks) => ticks
					.checked_mul(self.timestamp_scale_ns)
					.ok_or(Error::TimestampOverflow)?,
				None => {
					let duration = track
						.default_duration
						.ok_or(Error::InvalidSubtitle("missing cue duration"))?;
					u64::try_from(duration.as_nanos()).map_err(|_| Error::TimestampOverflow)?
				}
			};
			let duration = Timestamp::from_nanos(duration)?;
			let timestamp = self.timebase.shift(Timestamp::from_nanos(pts_ns)?)?;
			self.initial_reservation = None;
			let payload = cue_payload(
				track.text_header.as_deref(),
				cue.addition,
				block.raw_frame_data(),
				timestamp,
				duration,
			)?;
			track.track.write(crate::container::Frame {
				timestamp,
				payload,
				keyframe: true,
				duration: Some(duration),
			})?;
			// Each cue is self-contained, so close its group immediately.
			return track.track.cut(None);
		}

		let frames = split_lacing(block).map_err(anyhow::Error::new)?;
		let duration = if block.lacing.is_some() {
			Some(
				track
					.default_duration
					.ok_or_else(|| anyhow::Error::new(LacingError::MissingDuration))?,
			)
		} else {
			None
		};
		// Validate the last timestamp before publishing any part of this block.
		let step = duration.map(|duration| duration.as_nanos()).unwrap_or_default();
		let duration = duration
			.map(|duration| Timestamp::from_nanos(duration.as_nanos() as u64))
			.transpose()?;
		let last = u128::from(pts_ns) + step * (frames.len() - 1) as u128;
		Timestamp::from_nanos(u64::try_from(last).map_err(|_| Error::TimestampOverflow)?)?;
		for (index, payload) in frames.into_iter().enumerate() {
			let pts_ns = (u128::from(pts_ns) + step * index as u128) as u64;
			// Anchor before releasing the reservation, so the first catalog carries its clock.
			let timestamp = self.timebase.shift(Timestamp::from_nanos(pts_ns)?)?;
			self.initial_reservation = None;
			let frame = crate::container::Frame {
				timestamp,
				payload: Bytes::copy_from_slice(payload),
				keyframe: match track.kind {
					TrackKind::Audio => index == 0,
					TrackKind::Video | TrackKind::Text => keyframe,
				},
				duration,
			};
			track.track.write(frame)?;
		}
		if matches!(track.kind, TrackKind::Audio) {
			track.track.cut(None)?;
		}

		Ok(())
	}

	/// Close the current group on every track and open the next one at `sequence`.
	///
	/// Broadcast-wide: every track inside this MKV import advances together; per-track
	/// control is intentionally not exposed. Text spends a group per cue, so a text track that is
	/// already past `sequence` continues from its own next group instead.
	pub fn seek(&mut self, sequence: u64) -> Result<()> {
		for track in self.tracks.values_mut() {
			let sequence = match &track.track {
				Media::Text(text) => sequence.max(text.next_sequence()),
				_ => sequence,
			};
			track.track.seek(sequence)?;
		}
		Ok(())
	}

	/// Finish all tracks, flushing current groups.
	pub fn finish(&mut self) -> Result<()> {
		self.finished = true;
		while let Some(tag) = self
			.decoder
			.decode_eof(&mut self.buffer)
			.map_err(|_| Error::MatroskaParse)?
		{
			self.handle_tag(tag.tag)?;
		}
		// Finished sources can remain owned after EOF; release the last element's allocation.
		self.buffer = BytesMut::new();
		// No frame follows to anchor the clock, so publish the declared track set now.
		self.initial_reservation = None;
		for track in self.tracks.values_mut() {
			if let Some(mut g) = track.group.take() {
				g.finish()?;
			}
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
		for mut track in std::mem::take(&mut self.tracks).into_values() {
			if let Some(g) = track.group.take() {
				let _ = g.abort(err.clone());
			}
			track.track.abort(err.clone());
		}
	}
}

impl<E: crate::catalog::hang::CatalogExt> Drop for Import<E> {
	/// End each track, then retire its HLS records: every track takes a fresh name, so nothing
	/// publishes it again.
	fn drop(&mut self) {
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

/// Render one Matroska subtitle block as a self-contained WebVTT document.
///
/// `header` is the WebVTT CodecPrivate; SRT (`S_TEXT/UTF8`) tracks have none and get a bare header.
fn cue_payload(
	header: Option<&str>,
	addition: Option<&[u8]>,
	body: &[u8],
	start: Timestamp,
	duration: Timestamp,
) -> Result<Bytes> {
	let end = start.checked_add(duration)?;
	let body = std::str::from_utf8(body).map_err(|_| Error::InvalidSubtitle("invalid UTF-8"))?;
	let mut output = header.unwrap_or("WEBVTT\n\n").to_string();
	let mut id = "";
	let mut settings = "";
	if let Some(addition) = addition.filter(|_| header.is_some()) {
		let addition =
			std::str::from_utf8(addition).map_err(|_| Error::InvalidSubtitle("invalid cue additions UTF-8"))?;
		let mut lines = addition.splitn(3, '\n');
		settings = lines.next().unwrap_or_default();
		id = lines.next().unwrap_or_default();
		if let Some(comments) = lines.next().filter(|value| !value.is_empty()) {
			crate::container::text::comments(&mut output, comments).map_err(Error::InvalidSubtitle)?;
		}
	}
	let body = match header {
		Some(_) => crate::container::text::rebase(body, start..end, Timestamp::ZERO).map_err(Error::InvalidSubtitle)?,
		// SRT is plain text, where a literal `-->` is valid but would end a WebVTT cue.
		None => body.replace("-->", "--&gt;"),
	};
	crate::container::text::append(
		&mut output,
		crate::container::text::Cue {
			start,
			end,
			body: &body,
			id,
			settings,
		},
	)
	.map_err(Error::InvalidSubtitle)?;
	Ok(Bytes::from(output))
}

#[derive(Debug, thiserror::Error)]
enum LacingError {
	#[error("invalid MKV lacing")]
	Invalid,
	#[error("MKV lacing requires a positive TrackEntry DefaultDuration")]
	MissingDuration,
}

// webm-iterable's splitter uses unchecked indexing and size arithmetic on malformed laces.
// Keep bounds and signed EBML size deltas checked before slicing untrusted input.
fn split_lacing<'a>(block: &'a SimpleBlock<'_>) -> std::result::Result<Vec<&'a [u8]>, LacingError> {
	let Some(lacing) = block.lacing else {
		return Ok(vec![block.raw_frame_data()]);
	};
	let invalid = || LacingError::Invalid;
	let mut data = block.raw_frame_data();
	let count = usize::from(*data.first().ok_or_else(invalid)?) + 1;
	if count < 2 {
		return Err(invalid());
	}
	data = &data[1..];
	let mut sizes = Vec::with_capacity(count);
	match lacing {
		BlockLacing::FixedSize => {
			if !data.len().is_multiple_of(count) {
				return Err(invalid());
			}
			sizes.resize(count - 1, data.len() / count);
		}
		BlockLacing::Xiph => {
			for _ in 0..count - 1 {
				let mut size = 0usize;
				loop {
					let byte = *data.first().ok_or_else(invalid)?;
					data = &data[1..];
					size = size.checked_add(usize::from(byte)).ok_or_else(invalid)?;
					if byte != 255 {
						break;
					}
				}
				sizes.push(size);
			}
		}
		BlockLacing::Ebml => {
			for index in 0..count - 1 {
				let first = *data.first().ok_or_else(invalid)?;
				let len = first.leading_zeros() as usize + 1;
				if len > 8 || data.len() < len {
					return Err(invalid());
				}
				let mut value = u64::from(first) & (0xffu64 >> len);
				for byte in &data[1..len] {
					value = (value << 8) | u64::from(*byte);
				}
				data = &data[len..];
				let size = if index == 0 {
					value as i64
				} else {
					let delta = value as i64 - ((1i64 << (7 * len - 1)) - 1);
					i64::try_from(sizes[index - 1])
						.map_err(|_| invalid())?
						.checked_add(delta)
						.ok_or_else(invalid)?
				};
				sizes.push(usize::try_from(size).map_err(|_| invalid())?);
			}
		}
	}
	let mut frames = Vec::with_capacity(count);
	for size in sizes {
		if size > data.len() {
			return Err(invalid());
		}
		let (frame, remaining) = data.split_at(size);
		frames.push(frame);
		data = remaining;
	}
	frames.push(data);
	Ok(frames)
}

#[cfg(test)]
mod tests {
	use super::*;
	use webm_iterable::{WebmWriter, WriteOptions};

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
		let mut consumed = header.len();
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
				assert_eq!(import.decoder.position() + import.buffer.len(), consumed);
			}
			assert!(import.buffer.is_empty(), "a complete cluster left its prefix buffered");
			assert_eq!(import.decoder.position(), consumed);
			assert_eq!(import.cluster_timestamp, timestamp);
		}
		import.finish().unwrap();
		import.finish().unwrap();
		assert!(matches!(
			import.decode(&[0]),
			Err(crate::Error::Mkv(Error::TracksAfterFinish))
		));
	}

	#[test]
	fn input_after_a_failed_finish_is_refused() {
		// Unknown-size Tracks closes only at EOF, queueing Segment's end behind the refused track.
		let mut data = Vec::new();
		{
			let mut writer = WebmWriter::new(&mut data);
			writer
				.write(&MatroskaSpec::Ebml(Master::Full(vec![MatroskaSpec::DocType(
					"webm".into(),
				)])))
				.unwrap();
			for master in [
				MatroskaSpec::Segment(Master::Start),
				MatroskaSpec::Tracks(Master::Start),
			] {
				writer
					.write_advanced(&master, WriteOptions::is_unknown_sized_element())
					.unwrap();
			}
			writer
				.write(&MatroskaSpec::TrackEntry(Master::Full(vec![
					MatroskaSpec::TrackNumber(0),
					MatroskaSpec::TrackType(2),
					MatroskaSpec::CodecID("A_OPUS".into()),
				])))
				.unwrap();
			writer.flush().unwrap();
		}
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
		let mut import = Import::new(broadcast, catalog.reserve());
		import.decode(&data).unwrap();
		assert!(matches!(
			import.finish(),
			Err(crate::Error::Mkv(Error::ValueOutOfRange("TrackNumber")))
		));
		assert!(matches!(
			import.decode(&[0]),
			Err(crate::Error::Mkv(Error::TracksAfterFinish))
		));
	}
}
