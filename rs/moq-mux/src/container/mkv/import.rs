use std::collections::HashMap;
use std::convert::TryFrom;
use std::io::Cursor;

use crate::Result;
use bytes::{Buf, Bytes, BytesMut};
use hang::catalog::{AAC, AudioCodec, AudioConfig, Container, H264, H265, VP9, VideoCodec, VideoConfig};
use moq_net::Timestamp;
use mp4_atom::Atom;
use tokio::io::{AsyncRead, AsyncReadExt};
use webm_iterable::WebmIterator;
use webm_iterable::errors::TagIteratorError;
use webm_iterable::iterator::AllowableErrors;
use webm_iterable::matroska_spec::{Master, MatroskaSpec, SimpleBlock};

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
///
/// Unsupported codecs (e.g. Vorbis, AC3, MP3, subtitles) are logged and dropped.
pub struct Import {
	broadcast: moq_net::BroadcastProducer,
	catalog: crate::catalog::hang::Producer,

	/// Accumulated unparsed input.
	buffer: BytesMut,
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

struct MkvTrack {
	kind: TrackKind,
	track: crate::container::Producer<crate::catalog::hang::Container>,
	group: Option<moq_net::GroupProducer>,
	/// Highest block timestamp (Matroska ticks: cluster_ts + block_relative) already emitted.
	/// Used to dedup re-parsed blocks across decode() calls.
	last_emitted_ticks: Option<i64>,
}

impl Import {
	pub fn new(broadcast: moq_net::BroadcastProducer, catalog: crate::catalog::hang::Producer) -> Self {
		Self {
			broadcast,
			catalog,
			buffer: BytesMut::new(),
			tracks_seen: false,
			timestamp_scale_ns: DEFAULT_TIMESTAMP_SCALE_NS,
			cluster_timestamp: 0,
			tracks: HashMap::default(),
		}
	}

	pub fn is_initialized(&self) -> bool {
		self.tracks_seen
	}

	/// Decode from an asynchronous reader. Drives [`Self::decode`] in a loop.
	pub async fn decode_from<T: AsyncRead + Unpin>(&mut self, reader: &mut T) -> Result<()> {
		let mut chunk = BytesMut::with_capacity(64 * 1024);
		loop {
			chunk.clear();
			let n = reader.read_buf(&mut chunk).await?;
			if n == 0 {
				break;
			}
			self.decode(&mut chunk)?;
		}
		Ok(())
	}

	/// Append the buffer to the internal scratch and parse as many tags as possible.
	///
	/// The buffer is fully consumed on every call (data is moved into the internal
	/// scratch). Bytes that cannot yet form a complete top-level tag are retained
	/// for the next call.
	pub fn decode<T: Buf + AsRef<[u8]>>(&mut self, buf: &mut T) -> Result<()> {
		// Move the input into our scratch buffer.
		while buf.has_remaining() {
			let chunk = buf.chunk();
			self.buffer.extend_from_slice(chunk);
			let len = chunk.len();
			buf.advance(len);
		}

		self.drain()
	}

	/// Run the iterator over the buffered bytes, processing every fully-parsed top-level tag.
	///
	/// On each call, the iterator restarts from the beginning of the retained buffer. Tag
	/// handling is idempotent (state flags for header/tracks, per-track timestamp dedup for
	/// blocks). After parsing stops (UnexpectedEOF or end of buffer), bytes up to the start
	/// of the most-recently emitted top-level tag are discarded so memory does not grow
	/// unboundedly.
	fn drain(&mut self) -> Result<()> {
		// Buffer master tags that are bounded and convenient to handle atomically.
		let buffered = [
			MatroskaSpec::Ebml(Master::Start),
			MatroskaSpec::Info(Master::Start),
			MatroskaSpec::Tracks(Master::Start),
			MatroskaSpec::TrackEntry(Master::Start),
			MatroskaSpec::Audio(Master::Start),
			MatroskaSpec::Video(Master::Start),
			MatroskaSpec::BlockGroup(Master::Start),
		];

		if self.buffer.is_empty() {
			return Ok(());
		}

		let snapshot = self.buffer.clone().freeze();
		let mut cursor = Cursor::new(snapshot.as_ref());
		let mut iter = WebmIterator::new(&mut cursor, &buffered);
		// We restart the iterator from the beginning of the retained buffer on every
		// drain pass. Once data is replayed mid-Segment, ebml-iterable would otherwise
		// reject Segment children (Cluster, Tracks, etc.) as appearing without their
		// parent. Allowing hierarchy problems plus our own dedup logic on emitted
		// blocks gives us idempotent streaming behavior.
		iter.allow_errors(&[AllowableErrors::HierarchyProblems]);
		// Don't synthesize Master::End tags when the buffer ends mid-element.
		iter.emit_master_end_when_eof(false);

		let mut last_offset: usize = 0;

		loop {
			match iter.next() {
				Some(Ok(tag)) => {
					last_offset = iter.last_emitted_tag_offset();
					self.handle_tag(tag)?;
				}
				Some(Err(TagIteratorError::UnexpectedEOF { .. })) => break,
				Some(Err(_e)) => {
					return Err(Error::MatroskaParse.into());
				}
				None => {
					last_offset = snapshot.len();
					break;
				}
			}
		}

		drop(iter);

		// Retain bytes from the start of the last emitted tag (safe replay point) onward.
		// At the very least, this lets us reuse partially-read tags as more data arrives.
		// If we never emitted anything (very first call with too few bytes), keep everything.
		if last_offset > 0 {
			self.buffer.advance(last_offset);
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
						self.timestamp_scale_ns = *v;
					}
				}
			}
			// Idempotency: if the parser restarts mid-stream and `last_offset`
			// happens to point at Tracks (i.e. Tracks was the last fully-emitted
			// tag), we'll see it again. Process once.
			MatroskaSpec::Tracks(Master::Full(children)) if !self.tracks_seen => {
				self.handle_tracks(children)?;
				self.tracks_seen = true;
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
				self.handle_block(sb.track, sb.timestamp, sb.keyframe, sb.raw_frame_data())?;
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

	fn handle_tracks(&mut self, entries: Vec<MatroskaSpec>) -> Result<()> {
		for entry in entries {
			if let MatroskaSpec::TrackEntry(Master::Full(children)) = entry {
				if let Err(e) = self.add_track(children) {
					tracing::warn!(error = ?e, "skipping MKV track");
				}
			}
		}
		Ok(())
	}

	fn add_track(&mut self, children: Vec<MatroskaSpec>) -> Result<()> {
		let mut track_number: Option<u64> = None;
		let mut track_type: Option<u64> = None;
		let mut codec_id: Option<String> = None;
		let mut codec_private: Option<Bytes> = None;
		let mut audio_children: Option<Vec<MatroskaSpec>> = None;
		let mut video_children: Option<Vec<MatroskaSpec>> = None;

		for c in children {
			match c {
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
		let track_type = track_type.ok_or(Error::MissingTrackType)?;
		let codec_id = codec_id.ok_or(Error::MissingCodecId)?;

		// Matroska TrackType: 1 = video, 2 = audio.
		let (kind, suffix) = match track_type {
			1 => (TrackKind::Video, ".mkv-v"),
			2 => (TrackKind::Audio, ".mkv-a"),
			other => {
				tracing::warn!(track_type = other, codec_id, "unsupported MKV track type, skipping");
				return Ok(());
			}
		};

		let net_track = self.broadcast.unique_track(suffix)?;
		let mut catalog = self.catalog.clone();
		let mut catalog = catalog.lock();

		match kind {
			TrackKind::Video => {
				let config = build_video_config(&codec_id, codec_private.as_ref(), video_children.as_deref())?;
				catalog.video.renditions.insert(net_track.name.clone(), config);
			}
			TrackKind::Audio => {
				let config = build_audio_config(&codec_id, codec_private.as_ref(), audio_children.as_deref())?;
				catalog.audio.renditions.insert(net_track.name.clone(), config);
			}
		}

		drop(catalog);

		self.tracks.insert(
			track_number,
			MkvTrack {
				kind,
				track: crate::container::Producer::new(net_track, crate::catalog::hang::Container::Legacy),
				group: None,
				last_emitted_ticks: None,
			},
		);

		Ok(())
	}

	fn handle_block_group(&mut self, children: &[MatroskaSpec]) -> Result<()> {
		let mut block_data: Option<&[u8]> = None;
		let mut has_reference = false;

		for c in children {
			match c {
				MatroskaSpec::Block(data) => block_data = Some(data.as_slice()),
				MatroskaSpec::ReferenceBlock(_) => has_reference = true,
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

		self.handle_block(parsed.track, parsed.timestamp, keyframe, parsed.raw_frame_data())
	}

	fn handle_block(&mut self, track_number: u64, rel_ts: i16, keyframe: bool, payload: &[u8]) -> Result<()> {
		let Some(track) = self.tracks.get_mut(&track_number) else {
			// Unknown or skipped track.
			return Ok(());
		};

		// Compute PTS in MKV's native nanosecond units and stamp it on the
		// timestamp at NANO scale so a passthrough re-emit preserves precision.
		let block_ticks = (self.cluster_timestamp as i64) + (rel_ts as i64);
		if block_ticks < 0 {
			return Err(Error::NegativeBlockTimestamp.into());
		}

		// Skip blocks we've already emitted on a previous decode() pass (buffer replay).
		if let Some(last) = track.last_emitted_ticks
			&& block_ticks <= last
		{
			return Ok(());
		}
		track.last_emitted_ticks = Some(block_ticks);

		let pts_ns = (block_ticks as u64)
			.checked_mul(self.timestamp_scale_ns)
			.ok_or(Error::TimestampOverflow)?;
		let timestamp = Timestamp::from_nanos(pts_ns)?;

		// Audio tracks: always treat as keyframes (matches fmp4 behavior).
		let keyframe = matches!(track.kind, TrackKind::Audio) || keyframe;

		let frame = crate::container::Frame {
			timestamp,
			payload: Bytes::copy_from_slice(payload),
			keyframe,
		};

		// Manage groups: new group on video keyframe; audio always finishes its group immediately.
		match track.kind {
			TrackKind::Video => {
				if keyframe {
					if let Some(mut prev) = track.group.take() {
						prev.finish()?;
					}
				}
				track.track.write(frame)?;
			}
			TrackKind::Audio => {
				track.track.write(frame)?;
				track.track.finish_group()?;
			}
		}

		Ok(())
	}

	/// Close the current group on every track and open the next one at `sequence`.
	///
	/// Broadcast-wide: every track inside this MKV import advances together; per-track
	/// control is intentionally not exposed.
	pub fn seek(&mut self, sequence: u64) -> Result<()> {
		for track in self.tracks.values_mut() {
			track.track.seek(sequence)?;
		}
		Ok(())
	}

	/// Finish all tracks, flushing current groups.
	pub fn finish(&mut self) -> Result<()> {
		for track in self.tracks.values_mut() {
			if let Some(mut g) = track.group.take() {
				g.finish()?;
			}
			track.track.finish()?;
		}
		Ok(())
	}
}

impl Drop for Import {
	fn drop(&mut self) {
		let mut catalog = self.catalog.lock();
		for track in self.tracks.values() {
			match track.kind {
				TrackKind::Video => {
					catalog.video.renditions.remove(&track.track.name);
				}
				TrackKind::Audio => {
					catalog.audio.renditions.remove(&track.track.name);
				}
			}
		}
	}
}

fn build_video_config(
	codec_id: &str,
	codec_private: Option<&Bytes>,
	video_children: Option<&[MatroskaSpec]>,
) -> Result<VideoConfig> {
	let (width, height) = video_children
		.map(|cs| {
			let mut w = None;
			let mut h = None;
			for c in cs {
				match c {
					MatroskaSpec::PixelWidth(v) => w = Some(*v as u32),
					MatroskaSpec::PixelHeight(v) => h = Some(*v as u32),
					_ => {}
				}
			}
			(w, h)
		})
		.unwrap_or((None, None));

	let mut config = match codec_id {
		"V_VP8" => {
			let mut config = VideoConfig::new(VideoCodec::VP8);
			config.coded_width = width;
			config.coded_height = height;
			config.container = Container::Legacy;
			config
		}
		"V_VP9" => {
			let mut config = VideoConfig::new(VP9 {
				profile: 0,
				level: 0,
				bit_depth: 8,
				color_primaries: 1,
				chroma_subsampling: 1,
				transfer_characteristics: 1,
				matrix_coefficients: 1,
				full_range: false,
			});
			config.coded_width = width;
			config.coded_height = height;
			config.container = Container::Legacy;
			config
		}
		"V_MPEG4/ISO/AVC" => build_h264_config(codec_private)?,
		"V_MPEGH/ISO/HEVC" => build_h265_config(codec_private)?,
		"V_AV1" => build_av1_config(codec_private)?,
		other => return Err(Error::UnsupportedVideoCodec(other.to_string()).into()),
	};

	if config.coded_width.is_none() {
		config.coded_width = width;
	}
	if config.coded_height.is_none() {
		config.coded_height = height;
	}

	Ok(config)
}

fn build_audio_config(
	codec_id: &str,
	codec_private: Option<&Bytes>,
	audio_children: Option<&[MatroskaSpec]>,
) -> Result<AudioConfig> {
	let mut sample_rate: u32 = 0;
	let mut channels: u32 = 0;

	if let Some(cs) = audio_children {
		for c in cs {
			match c {
				MatroskaSpec::SamplingFrequency(v) => sample_rate = *v as u32,
				MatroskaSpec::Channels(v) => channels = *v as u32,
				_ => {}
			}
		}
	}

	match codec_id {
		"A_OPUS" => {
			// Codec private is OpusHead. If present, it's authoritative for rate/channels.
			let (cfg_rate, cfg_channels) = if let Some(priv_data) = codec_private {
				let mut cursor = priv_data.clone();
				let cfg = crate::codec::opus::Config::parse(&mut cursor)?;
				(cfg.sample_rate, cfg.channel_count)
			} else {
				(sample_rate, channels)
			};

			let mut config = AudioConfig::new(
				AudioCodec::Opus,
				if cfg_rate > 0 { cfg_rate } else { sample_rate },
				if cfg_channels > 0 { cfg_channels } else { channels },
			);
			config.container = Container::Legacy;
			Ok(config)
		}
		"A_AAC" => {
			let priv_data = codec_private.ok_or(Error::MissingCodecPrivate {
				codec_id: "A_AAC",
				purpose: "AudioSpecificConfig",
			})?;
			let mut cursor = priv_data.clone();
			let cfg = crate::codec::aac::Config::parse(&mut cursor)?;

			let mut config = AudioConfig::new(
				AAC { profile: cfg.profile },
				if cfg.sample_rate > 0 {
					cfg.sample_rate
				} else {
					sample_rate
				},
				if cfg.channel_count > 0 {
					cfg.channel_count
				} else {
					channels
				},
			);
			config.description = Some(priv_data.clone());
			config.container = Container::Legacy;
			Ok(config)
		}
		other => Err(Error::UnsupportedAudioCodec(other.to_string()).into()),
	}
}

fn build_h264_config(codec_private: Option<&Bytes>) -> Result<VideoConfig> {
	let avcc_bytes = codec_private.ok_or(Error::MissingCodecPrivate {
		codec_id: "V_MPEG4/ISO/AVC",
		purpose: "AVCDecoderConfigurationRecord",
	})?;
	let avcc = crate::codec::h264::Avcc::parse(avcc_bytes)?;

	let mut config = VideoConfig::new(H264 {
		profile: avcc.profile,
		constraints: avcc.constraints,
		level: avcc.level,
		inline: false,
	});
	config.description = Some(avcc_bytes.clone());
	config.coded_width = avcc.coded_width;
	config.coded_height = avcc.coded_height;
	config.container = Container::Legacy;
	Ok(config)
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
	config.container = Container::Legacy;
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
	config.description = Some(description.freeze());
	config.container = Container::Legacy;
	Ok(config)
}
