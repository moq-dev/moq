use std::collections::HashMap;
use std::io::Cursor;
use std::task::Poll;
use std::time::Duration;

use anyhow::Context;
use bytes::{BufMut, Bytes, BytesMut};
use hang::catalog::{AudioCodec, AudioConfig, Catalog, Container, VideoCodec, VideoConfig};
use webm_iterable::matroska_spec::{Master, MatroskaSpec};
use webm_iterable::{WebmWriter, WriteOptions};

use crate::container::{Consumer, Frame, Hang};

/// Default Matroska TimestampScale: 1 ms (in nanoseconds).
const TIMESTAMP_SCALE_NS: u64 = 1_000_000;

/// Subscribe to a moq broadcast and produce a single Matroska / WebM byte stream.
///
/// Built from a [`moq_net::BroadcastConsumer`], `Mkv` subscribes to the hang catalog,
/// (un)subscribes per-rendition tracks, decodes them via [`Consumer<Hang>`], and
/// re-encodes everything as EBML + Segment + Tracks + Cluster/SimpleBlock tags ready
/// for any Matroska-aware consumer (ffplay, libwebm, browser MSE for WebM).
///
/// Use [`next`](Self::next) to pull byte chunks: the first call returns the header
/// (EBML + Segment-start + Info + Tracks), subsequent calls return per-frame Cluster
/// fragments. Returns `None` when the broadcast ends.
///
/// Only Legacy-container tracks (raw codec payloads) are supported. CMAF tracks
/// (moof+mdat passthrough) would require unwrapping the fragment and are rejected.
pub struct Mkv {
	broadcast: moq_net::BroadcastConsumer,
	catalog: Option<crate::catalog::Consumer>,
	latency: Duration,

	tracks: HashMap<String, MkvTrack>,

	/// Doc type to advertise in the EBML header. "webm" if every active codec is a
	/// WebM-allowed codec (VP8/VP9/AV1/Opus); "matroska" otherwise.
	doc_type: &'static str,

	/// Queued header tags emitted on the first [`next`](Self::next) call.
	header_pending: Option<Bytes>,
	header_emitted: bool,
}

struct MkvTrack {
	consumer: Consumer<Hang>,

	/// Next decoded frame; held for cross-track ordering.
	pending: Option<Frame>,

	/// Whether the consumer has signalled end-of-track.
	finished: bool,

	/// Matroska TrackNumber assigned at subscription time.
	track_number: u64,
}

impl Mkv {
	/// Subscribe to `broadcast` and produce MKV byte chunks.
	pub fn new(broadcast: moq_net::BroadcastConsumer) -> Result<Self, crate::Error> {
		let catalog_track = broadcast.subscribe_track(&hang::Catalog::default_track())?;
		let catalog = crate::catalog::Consumer::new(catalog_track);

		Ok(Self {
			broadcast,
			catalog: Some(catalog),
			latency: Duration::ZERO,
			tracks: HashMap::new(),
			doc_type: "matroska",
			header_pending: None,
			header_emitted: false,
		})
	}

	/// Set the maximum buffering latency for each per-track [`Consumer`].
	pub fn with_latency(mut self, latency: Duration) -> Self {
		self.latency = latency;
		self
	}

	/// Get the next byte chunk.
	///
	/// First call returns the EBML + Segment-start + Info + Tracks header; subsequent
	/// calls return one or more SimpleBlock tags (wrapped in a Cluster header when a
	/// new cluster begins). Returns `None` when the catalog and every track have ended.
	pub async fn next(&mut self) -> anyhow::Result<Option<Bytes>> {
		conducer::wait(|waiter| self.poll_next(waiter)).await
	}

	pub fn poll_next(&mut self, waiter: &conducer::Waiter) -> Poll<anyhow::Result<Option<Bytes>>> {
		// 1. Drain catalog updates.
		while let Some(catalog) = self.catalog.as_mut() {
			match catalog.poll_next(waiter).map_err(crate::Error::from)? {
				Poll::Ready(Some(snapshot)) => self.update_catalog(&snapshot)?,
				Poll::Ready(None) => {
					self.catalog = None;
					break;
				}
				Poll::Pending => break,
			}
		}

		// 2. Emit the header once.
		if !self.header_emitted
			&& let Some(header) = self.header_pending.take()
		{
			self.header_emitted = true;
			return Poll::Ready(Ok(Some(header)));
		}

		// 3. Fill empty pending slots from each consumer.
		for track in self.tracks.values_mut() {
			if track.pending.is_some() || track.finished {
				continue;
			}
			match track.consumer.poll_read(waiter) {
				Poll::Ready(Ok(Some(frame))) => track.pending = Some(frame),
				Poll::Ready(Ok(None)) => track.finished = true,
				Poll::Ready(Err(e)) => return Poll::Ready(Err(e.into())),
				Poll::Pending => {}
			}
		}

		// 4. Pick the track whose pending frame has the smallest timestamp.
		let chosen = self
			.tracks
			.iter()
			.filter_map(|(name, t)| t.pending.as_ref().map(|f| (name.clone(), f.timestamp)))
			.min_by_key(|(_, ts)| *ts)
			.map(|(name, _)| name);

		if let Some(name) = chosen {
			let track = self.tracks.get_mut(&name).unwrap();
			let frame = track.pending.take().unwrap();
			let bytes = self.encode_block(&name, &frame)?;
			return Poll::Ready(Ok(Some(bytes)));
		}

		// 5. End-of-stream. The Segment is unknown-sized (live mode), so we don't
		// need to close it.
		if self.catalog.is_none() && self.tracks.values().all(|t| t.finished) {
			return Poll::Ready(Ok(None));
		}

		// 6. Drop finished, drained tracks.
		self.tracks.retain(|_, t| !(t.finished && t.pending.is_none()));

		Poll::Pending
	}

	fn update_catalog(&mut self, catalog: &Catalog) -> anyhow::Result<()> {
		// Build the header on the first catalog snapshot.
		if !self.header_emitted && self.header_pending.is_none() {
			let (header, doc_type) = build_header(catalog)?;
			self.header_pending = Some(header);
			self.doc_type = doc_type;
		}

		let mut active: HashMap<String, ()> = HashMap::new();
		for name in catalog.video.renditions.keys() {
			active.insert(name.clone(), ());
		}
		for name in catalog.audio.renditions.keys() {
			active.insert(name.clone(), ());
		}

		// Assign track numbers in catalog iteration order. This must match the order
		// used when writing the Tracks element in build_header.
		let mut next_track_number: u64 = self.tracks.values().map(|t| t.track_number).max().unwrap_or(0) + 1;

		for name in catalog.video.renditions.keys() {
			if self.tracks.contains_key(name) {
				continue;
			}
			let config = &catalog.video.renditions[name];
			ensure_legacy(&config.container, "video", name)?;
			let consumer = subscribe(&self.broadcast, name, &config.container, self.latency)?;
			self.tracks.insert(
				name.clone(),
				MkvTrack {
					consumer,
					pending: None,
					finished: false,
					track_number: next_track_number,
				},
			);
			next_track_number += 1;
		}

		for name in catalog.audio.renditions.keys() {
			if self.tracks.contains_key(name) {
				continue;
			}
			let config = &catalog.audio.renditions[name];
			ensure_legacy(&config.container, "audio", name)?;
			let consumer = subscribe(&self.broadcast, name, &config.container, self.latency)?;
			self.tracks.insert(
				name.clone(),
				MkvTrack {
					consumer,
					pending: None,
					finished: false,
					track_number: next_track_number,
				},
			);
			next_track_number += 1;
		}

		self.tracks.retain(|name, _| active.contains_key(name));
		Ok(())
	}

	fn encode_block(&mut self, name: &str, frame: &Frame) -> anyhow::Result<Bytes> {
		let track = self.tracks.get(name).context("missing track")?;
		let track_number = track.track_number;

		// Compute timestamp in Matroska ticks (ms, given our fixed scale of 1ms).
		let frame_ticks: u64 = (frame.timestamp.as_micros() / 1_000)
			.try_into()
			.context("timestamp doesn't fit in u64 ms")?;

		// Emit one Cluster per frame with the frame's timestamp as the Cluster's
		// Timestamp and a zero block-relative offset. Each Cluster is self-contained
		// so a fresh chunk can be decoded standalone. Hand-encode the EBML to avoid
		// the WebmWriter's schema validation, which rejects Clusters that aren't
		// nested inside a Segment in the same writer's open-tag stack.
		Ok(encode_cluster(
			frame_ticks,
			track_number,
			frame.keyframe,
			&frame.payload,
		))
	}
}

fn ensure_legacy(container: &Container, kind: &str, name: &str) -> anyhow::Result<()> {
	match container {
		Container::Legacy => Ok(()),
		Container::Cmaf { .. } => {
			anyhow::bail!("MKV export does not support CMAF {} track '{}'", kind, name);
		}
	}
}

fn subscribe(
	broadcast: &moq_net::BroadcastConsumer,
	name: &str,
	container: &Container,
	latency: Duration,
) -> Result<Consumer<Hang>, crate::Error> {
	let media: Hang = container.try_into()?;
	let track = broadcast.subscribe_track(&moq_net::Track::new(name.to_string()))?;
	Ok(Consumer::new(track, media).with_latency(latency))
}

/// Build the file header (EBML + unknown-size Segment + Info + Tracks) from a catalog
/// snapshot. Tracks are numbered starting at 1 in catalog iteration order:
/// video renditions first, then audio.
fn build_header(catalog: &Catalog) -> anyhow::Result<(Bytes, &'static str)> {
	// Decide DocType: webm only if every codec is WebM-allowed.
	let webm_only = catalog
		.video
		.renditions
		.values()
		.all(|c| matches!(c.codec, VideoCodec::VP8 | VideoCodec::VP9(_) | VideoCodec::AV1(_)))
		&& catalog
			.audio
			.renditions
			.values()
			.all(|c| matches!(c.codec, AudioCodec::Opus));
	let doc_type = if webm_only { "webm" } else { "matroska" };

	let mut entries: Vec<MatroskaSpec> = Vec::new();
	let mut track_number: u64 = 1;

	for (_name, config) in catalog.video.renditions.iter() {
		entries.push(build_video_track_entry(track_number, config)?);
		track_number += 1;
	}
	for (_name, config) in catalog.audio.renditions.iter() {
		entries.push(build_audio_track_entry(track_number, config)?);
		track_number += 1;
	}

	let mut dest = Cursor::new(Vec::new());
	{
		let mut writer = WebmWriter::new(&mut dest);
		writer.write(&MatroskaSpec::Ebml(Master::Full(vec![
			MatroskaSpec::DocType(doc_type.to_string()),
			MatroskaSpec::DocTypeVersion(4),
			MatroskaSpec::DocTypeReadVersion(2),
		])))?;
		// Segment is written with an unknown size so we can stream Clusters into it
		// indefinitely without ever closing it.
		writer.write_advanced(
			&MatroskaSpec::Segment(Master::Start),
			WriteOptions::is_unknown_sized_element(),
		)?;
		writer.write(&MatroskaSpec::Info(Master::Full(vec![
			MatroskaSpec::TimestampScale(TIMESTAMP_SCALE_NS),
			MatroskaSpec::MuxingApp("moq-mux".to_string()),
			MatroskaSpec::WritingApp("moq-mux".to_string()),
		])))?;
		writer.write(&MatroskaSpec::Tracks(Master::Full(entries)))?;
		writer.flush()?;
	}

	Ok((Bytes::from(dest.into_inner()), doc_type))
}

fn build_video_track_entry(track_number: u64, config: &VideoConfig) -> anyhow::Result<MatroskaSpec> {
	let (codec_id, codec_private) = match &config.codec {
		VideoCodec::VP8 => ("V_VP8", None),
		VideoCodec::VP9(_) => ("V_VP9", None),
		VideoCodec::AV1(_) => ("V_AV1", config.description.as_ref().map(|b| b.to_vec())),
		VideoCodec::H264(_) => (
			"V_MPEG4/ISO/AVC",
			Some(
				config
					.description
					.as_ref()
					.context("H.264 track missing AVCDecoderConfigurationRecord (description)")?
					.to_vec(),
			),
		),
		VideoCodec::H265(_) => (
			"V_MPEGH/ISO/HEVC",
			Some(
				config
					.description
					.as_ref()
					.context("H.265 track missing HEVCDecoderConfigurationRecord (description)")?
					.to_vec(),
			),
		),
		other => anyhow::bail!("MKV export does not support video codec {:?}", other),
	};

	let mut video_children: Vec<MatroskaSpec> = Vec::new();
	if let Some(w) = config.coded_width {
		video_children.push(MatroskaSpec::PixelWidth(w as u64));
	}
	if let Some(h) = config.coded_height {
		video_children.push(MatroskaSpec::PixelHeight(h as u64));
	}

	let mut entry: Vec<MatroskaSpec> = vec![
		MatroskaSpec::TrackNumber(track_number),
		MatroskaSpec::TrackUID(track_number),
		MatroskaSpec::TrackType(1),
		MatroskaSpec::CodecID(codec_id.to_string()),
	];
	if let Some(cp) = codec_private {
		entry.push(MatroskaSpec::CodecPrivate(cp));
	}
	if !video_children.is_empty() {
		entry.push(MatroskaSpec::Video(Master::Full(video_children)));
	}

	Ok(MatroskaSpec::TrackEntry(Master::Full(entry)))
}

fn build_audio_track_entry(track_number: u64, config: &AudioConfig) -> anyhow::Result<MatroskaSpec> {
	let (codec_id, codec_private) = match &config.codec {
		AudioCodec::Opus => (
			"A_OPUS",
			Some(build_opus_head(config.sample_rate, config.channel_count)),
		),
		AudioCodec::AAC(_) => (
			"A_AAC",
			Some(
				config
					.description
					.as_ref()
					.context("AAC track missing AudioSpecificConfig (description)")?
					.to_vec(),
			),
		),
		other => anyhow::bail!("MKV export does not support audio codec {:?}", other),
	};

	let entry = vec![
		MatroskaSpec::TrackNumber(track_number),
		MatroskaSpec::TrackUID(track_number),
		MatroskaSpec::TrackType(2),
		MatroskaSpec::CodecID(codec_id.to_string()),
		MatroskaSpec::CodecPrivate(codec_private.unwrap()),
		MatroskaSpec::Audio(Master::Full(vec![
			MatroskaSpec::SamplingFrequency(config.sample_rate as f64),
			MatroskaSpec::Channels(config.channel_count as u64),
		])),
	];

	Ok(MatroskaSpec::TrackEntry(Master::Full(entry)))
}

/// Construct a minimal OpusHead packet (RFC 7845 §5.1).
fn build_opus_head(sample_rate: u32, channels: u32) -> Vec<u8> {
	let mut head = Vec::with_capacity(19);
	head.extend_from_slice(b"OpusHead");
	head.push(1); // version
	head.push(channels as u8);
	head.extend_from_slice(&0u16.to_le_bytes()); // pre-skip
	head.extend_from_slice(&sample_rate.to_le_bytes());
	head.extend_from_slice(&0i16.to_le_bytes()); // output gain
	head.push(0); // channel mapping family (0 = mono/stereo)
	head
}

/// EBML tag IDs we hand-encode.
const ID_CLUSTER: u32 = 0x1F43B675;
const ID_TIMESTAMP: u16 = 0xE7;
const ID_SIMPLEBLOCK: u16 = 0xA3;

/// Encode a self-contained Cluster element containing one Timestamp and one
/// SimpleBlock. Returns a complete byte fragment that a Matroska parser will
/// recognise as a top-level Cluster (the receiver pre-context being inside a
/// Segment).
fn encode_cluster(cluster_ts: u64, track_number: u64, keyframe: bool, payload: &[u8]) -> Bytes {
	// Build the inner Cluster body: Timestamp + SimpleBlock.
	let mut body = BytesMut::with_capacity(payload.len() + 32);
	write_tag_id(&mut body, ID_TIMESTAMP as u32);
	let ts_bytes = encode_uint(cluster_ts);
	write_vint(&mut body, ts_bytes.len() as u64);
	body.extend_from_slice(&ts_bytes);

	let sb_body = encode_simple_block_body(track_number, 0, keyframe, payload);
	write_tag_id(&mut body, ID_SIMPLEBLOCK as u32);
	write_vint(&mut body, sb_body.len() as u64);
	body.extend_from_slice(&sb_body);

	// Wrap as Cluster.
	let mut out = BytesMut::with_capacity(body.len() + 16);
	write_tag_id(&mut out, ID_CLUSTER);
	write_vint(&mut out, body.len() as u64);
	out.extend_from_slice(&body);
	out.freeze()
}

/// Encode the body of a SimpleBlock element. The on-wire format is:
///   <track-number VINT> <timestamp i16 BE> <flags u8> <frame data>
fn encode_simple_block_body(track_number: u64, rel_ts: i16, keyframe: bool, payload: &[u8]) -> Bytes {
	let mut data = BytesMut::with_capacity(payload.len() + 11);
	write_vint(&mut data, track_number);
	data.put_i16(rel_ts);
	let mut flags: u8 = 0;
	if keyframe {
		flags |= 0x80;
	}
	data.put_u8(flags);
	data.extend_from_slice(payload);
	data.freeze()
}

/// Write an EBML tag ID (the canonical encoding has the high bit of the leading byte set).
fn write_tag_id(buf: &mut BytesMut, id: u32) {
	let bytes = id.to_be_bytes();
	let start = bytes.iter().position(|&b| b != 0).unwrap_or(3);
	buf.extend_from_slice(&bytes[start..]);
}

/// Encode a u64 as a big-endian byte sequence using the minimum number of bytes.
fn encode_uint(value: u64) -> Vec<u8> {
	if value == 0 {
		return vec![0];
	}
	let leading_zero_bytes = (value.leading_zeros() / 8) as usize;
	let bytes = value.to_be_bytes();
	bytes[leading_zero_bytes..].to_vec()
}

/// Encode an unsigned integer as an EBML variable-length integer (VINT).
fn write_vint(buf: &mut BytesMut, value: u64) {
	// Determine the byte width: 1 byte holds 7 bits, 2 holds 14, etc.
	let mut width = 1;
	while width < 8 && value >= (1u64 << (7 * width)) - 1 {
		width += 1;
	}
	let marker = 1u8 << (8 - width);
	let mut bytes = [0u8; 8];
	for i in 0..width {
		bytes[width - 1 - i] = (value >> (8 * i)) as u8;
	}
	bytes[0] |= marker;
	buf.extend_from_slice(&bytes[..width]);
}
