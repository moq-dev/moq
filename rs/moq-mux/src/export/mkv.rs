use std::collections::HashMap;
use std::io::Cursor;
use std::task::Poll;
use std::time::Duration;

use anyhow::Context;
use bytes::{BufMut, Bytes, BytesMut};
use hang::catalog::{AudioCodec, AudioConfig, Catalog, Container, VideoCodec, VideoConfig};
use webm_iterable::matroska_spec::{Master, MatroskaSpec};
use webm_iterable::{WebmWriter, WriteOptions};

use crate::catalog::CatalogFormat;
use crate::container::{Consumer, Frame, Hang};
use crate::transform::{Avc1, Hvc1};

use super::CatalogSource;

/// Matroska TimestampScale: 1 ms (in nanoseconds).
const TIMESTAMP_SCALE_NS: u64 = 1_000_000;

/// Subscribe to a moq broadcast and produce a single Matroska / WebM byte stream.
///
/// Built from a [`moq_net::BroadcastConsumer`], `Mkv` subscribes to the hang catalog,
/// (un)subscribes per-rendition tracks, decodes them via [`Consumer<Hang>`], and
/// re-encodes everything as EBML + Segment + Tracks + Cluster/SimpleBlock tags ready
/// for any Matroska-aware consumer (ffplay, libwebm, browser MSE for WebM).
///
/// Use [`next`](Self::next) to pull byte chunks. The first chunk is the file
/// header (EBML + unknown-size Segment + Info + Tracks); subsequent chunks are
/// complete Cluster elements. By default a Cluster contains one GOP (rolled
/// over on each video keyframe, or on i16 timestamp overflow);
/// [`with_fragment_duration`](Self::with_fragment_duration) caps Cluster
/// duration for downstream consumers that throttle by fragment rate.
/// Returns `None` when the broadcast ends.
///
/// ## Avc3 / Hev1 sources
///
/// `Mkv` accepts Annex-B sources (`H264 { inline: true }`, `H265 { in_band: true }`,
/// catalog `description` empty) by attaching a [`crate::transform::Avc1`] /
/// [`crate::transform::Hvc1`] to each affected track. The transform caches
/// parameter sets, builds the out-of-band `AVCDecoderConfigurationRecord` /
/// `HEVCDecoderConfigurationRecord`, and length-prefixes sample NALs. Header
/// emission is deferred until every such track has produced its codec config
/// (typically the first keyframe).
///
/// Only Legacy-container tracks (raw codec payloads) are supported. CMAF tracks
/// (moof+mdat passthrough) are rejected with a clear error.
pub struct Mkv {
	broadcast: moq_net::BroadcastConsumer,
	catalog: Option<CatalogSource>,
	latency: Duration,
	fragment_duration: Option<Duration>,

	tracks: HashMap<String, MkvTrack>,
	/// Catalog snapshot used to build the header. Retained until header emission;
	/// subsequent catalog updates only (un)subscribe tracks.
	catalog_snapshot: Option<Catalog>,

	/// Whether the file header has been emitted.
	header_emitted: bool,

	/// Currently-open cluster, accumulating frames until it's time to flush.
	cluster: Option<ClusterBuilder>,
}

struct MkvTrack {
	consumer: Consumer<Hang>,
	pending: Option<Frame>,
	finished: bool,
	track_number: u64,
	kind: TrackKind,
	/// Optional per-codec transform. Wraps the sample bytes into the shape
	/// MKV expects (length-prefixed for H.264/H.265) and builds avcC/hvcC
	/// from inline parameter sets for Avc3/Hev1 sources.
	video_transform: Option<VideoTransform>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TrackKind {
	Video,
	Audio,
}

enum VideoTransform {
	Avc1(Avc1),
	Hvc1(Hvc1),
}

impl VideoTransform {
	/// Returns the codec config record (avcC / hvcC) if available.
	fn codec_private(&self) -> Option<&Bytes> {
		match self {
			VideoTransform::Avc1(t) => t.avcc(),
			VideoTransform::Hvc1(t) => t.hvcc(),
		}
	}

	fn transform(&mut self, payload: Bytes) -> anyhow::Result<Option<Bytes>> {
		match self {
			VideoTransform::Avc1(t) => t.transform(payload),
			VideoTransform::Hvc1(t) => t.transform(payload),
		}
	}
}

struct ClusterBuilder {
	/// Cluster.Timestamp in ticks (ms).
	start_ticks: u64,
	body: BytesMut,
	/// Highest frame timestamp seen in this cluster.
	max_ticks: u64,
	/// Whether the cluster has at least one video frame appended. Audio-only
	/// content shouldn't trigger a GOP-boundary rollover on the first video
	/// keyframe (audio that arrived ahead of the keyframe gets folded into
	/// this cluster instead of a stale one).
	has_video: bool,
}

impl ClusterBuilder {
	fn new(start_ticks: u64) -> Self {
		let mut body = BytesMut::with_capacity(64 * 1024);
		// Cluster.Timestamp (id = 0xE7).
		write_tag_id(&mut body, ID_TIMESTAMP as u32);
		let ts_bytes = encode_uint(start_ticks);
		write_vint(&mut body, ts_bytes.len() as u64);
		body.extend_from_slice(&ts_bytes);
		Self {
			start_ticks,
			body,
			max_ticks: start_ticks,
			has_video: false,
		}
	}

	fn append(
		&mut self,
		track_number: u64,
		frame_ticks: u64,
		keyframe: bool,
		payload: &[u8],
		is_video: bool,
	) -> anyhow::Result<()> {
		let rel = (frame_ticks as i64)
			.checked_sub(self.start_ticks as i64)
			.context("cluster underflow")?;
		let rel: i16 = rel.try_into().context("block timestamp doesn't fit in i16")?;

		let sb_body = encode_simple_block_body(track_number, rel, keyframe, payload);
		write_tag_id(&mut self.body, ID_SIMPLEBLOCK as u32);
		write_vint(&mut self.body, sb_body.len() as u64);
		self.body.extend_from_slice(&sb_body);

		if frame_ticks > self.max_ticks {
			self.max_ticks = frame_ticks;
		}
		if is_video {
			self.has_video = true;
		}
		Ok(())
	}

	/// Returns true if a frame at the given timestamp can still fit in this cluster
	/// without overflowing the i16 block-relative-timestamp field.
	fn fits(&self, frame_ticks: u64) -> bool {
		match (frame_ticks as i64).checked_sub(self.start_ticks as i64) {
			Some(rel) => i16::try_from(rel).is_ok(),
			None => false,
		}
	}

	/// Build the full Cluster element bytes.
	fn finish(self) -> Bytes {
		let mut out = BytesMut::with_capacity(self.body.len() + 16);
		write_tag_id(&mut out, ID_CLUSTER);
		write_vint(&mut out, self.body.len() as u64);
		out.extend_from_slice(&self.body);
		out.freeze()
	}
}

impl Mkv {
	/// Subscribe to `broadcast` and produce MKV byte chunks.
	///
	/// `catalog_format` selects which catalog track the exporter subscribes to
	/// for track discovery. Both formats end up driving the same internal
	/// `hang::Catalog`-based pipeline (MSF snapshots are converted on receipt),
	/// so the only observable difference is which wire catalog is consumed.
	pub fn new(broadcast: moq_net::BroadcastConsumer, catalog_format: CatalogFormat) -> Result<Self, crate::Error> {
		let catalog = CatalogSource::new(&broadcast, catalog_format)?;

		Ok(Self {
			broadcast,
			catalog: Some(catalog),
			latency: Duration::ZERO,
			fragment_duration: None,
			tracks: HashMap::new(),
			catalog_snapshot: None,
			header_emitted: false,
			cluster: None,
		})
	}

	/// Set the maximum buffering latency for each per-track [`Consumer`].
	pub fn with_latency(mut self, latency: Duration) -> Self {
		self.latency = latency;
		self
	}

	/// Cap the fragment (Cluster) duration.
	///
	/// By default Clusters roll over only on video keyframes (one cluster per GOP)
	/// or i16 timestamp overflow. Setting this caps each Cluster to roughly
	/// `duration` of frames, useful for downstream consumers that throttle by
	/// fragment rate. [`Duration::ZERO`] emits one Cluster per frame; otherwise
	/// the cap applies in addition to GOP / overflow rollover.
	///
	/// Accepts either `Duration` or `Option<Duration>` (where `None` restores
	/// the per-GOP default).
	pub fn with_fragment_duration(mut self, duration: impl Into<Option<Duration>>) -> Self {
		self.fragment_duration = duration.into();
		self
	}

	/// Get the next byte chunk.
	pub async fn next(&mut self) -> anyhow::Result<Option<Bytes>> {
		conducer::wait(|waiter| self.poll_next(waiter)).await
	}

	pub fn poll_next(&mut self, waiter: &conducer::Waiter) -> Poll<anyhow::Result<Option<Bytes>>> {
		// 1. Drain catalog updates.
		while let Some(catalog) = self.catalog.as_mut() {
			match catalog.poll_next(waiter)? {
				Poll::Ready(Some(snapshot)) => self.update_catalog(snapshot)?,
				Poll::Ready(None) => {
					self.catalog = None;
					break;
				}
				Poll::Pending => break,
			}
		}

		// 2. Pull frames from each track into `pending`, transforming codec
		// shape (Annex-B → length-prefixed) at pull time so downstream code
		// never sees a raw Avc3/Hev1 payload.
		//
		// Pre-header: for video tracks whose transmuxer hasn't yet built its
		// codec config (Avc3/Hev1 source, no SPS/PPS seen), drop transformed
		// slices instead of parking them. A receiver who subscribed mid-GOP
		// can't render those bytes without the header anyway, and parking
		// them would stop us from polling for the next SPS/PPS-bearing frame.
		let waiting_for_header = !self.header_emitted;
		for track in self.tracks.values_mut() {
			if track.pending.is_some() || track.finished {
				continue;
			}
			loop {
				match track.consumer.poll_read(waiter) {
					Poll::Ready(Ok(Some(frame))) => {
						let transformed = match &mut track.video_transform {
							// Parameter-only frame: transform absorbed it; pull the next one.
							Some(t) => t
								.transform(frame.payload.clone())?
								.map(|payload| Frame { payload, ..frame }),
							None => Some(frame),
						};
						if let Some(f) = transformed {
							let still_no_config = waiting_for_header
								&& track.kind == TrackKind::Video
								&& track
									.video_transform
									.as_ref()
									.is_some_and(|t| t.codec_private().is_none());
							if still_no_config {
								// Drop this slice and keep polling for SPS/PPS.
								continue;
							}
							track.pending = Some(f);
							break;
						}
					}
					Poll::Ready(Ok(None)) => {
						track.finished = true;
						break;
					}
					Poll::Ready(Err(e)) => return Poll::Ready(Err(e.into())),
					Poll::Pending => break,
				}
			}
		}

		// 3. Before the header is emitted: keep pulling until every video
		// transform has produced its codec config. Frames arriving in this
		// phase land in `pending` already transformed; once the header lands
		// we drain them like any other pending frame.
		if !self.header_emitted {
			if self.header_ready() {
				let header = self.build_header()?;
				self.header_emitted = true;
				return Poll::Ready(Ok(Some(header)));
			}

			// Still waiting on codec configs. If every track is finished and
			// the header still isn't ready, the source never produced enough
			// info to build it.
			if self.catalog.is_none() && self.tracks.values().all(|t| t.finished) {
				return Poll::Ready(Ok(None));
			}

			return Poll::Pending;
		}

		// 5. Pick the smallest-timestamp pending frame across tracks and route it
		// through the cluster builder.
		if let Some(name) = self.pick_next_track() {
			let frame = self.tracks.get_mut(&name).unwrap().pending.take().unwrap();
			if let Some(chunk) = self.feed_frame(&name, frame)? {
				return Poll::Ready(Ok(Some(chunk)));
			}
			// Frame consumed into the open cluster; loop and see if there's more to do.
			return self.poll_next(waiter);
		}

		// 6. End-of-stream: flush any open cluster when every subscribed track
		// has finished. We don't require catalog EOS here — a long-lived
		// producer may keep the catalog open even after every active track has
		// ended, and we'd rather flush the cluster than hold the last frames.
		if !self.tracks.is_empty() && self.tracks.values().all(|t| t.finished) {
			if let Some(cluster) = self.cluster.take() {
				return Poll::Ready(Ok(Some(cluster.finish())));
			}
			if self.catalog.is_none() {
				return Poll::Ready(Ok(None));
			}
		} else if self.catalog.is_none() && self.tracks.is_empty() {
			return Poll::Ready(Ok(None));
		}

		// 7. Drop finished, drained tracks so the next catalog update can re-add them.
		self.tracks.retain(|_, t| !(t.finished && t.pending.is_none()));

		Poll::Pending
	}

	fn update_catalog(&mut self, catalog: Catalog) -> anyhow::Result<()> {
		let mut active: HashMap<String, ()> = HashMap::new();
		for name in catalog.video.renditions.keys() {
			active.insert(name.clone(), ());
		}
		for name in catalog.audio.renditions.keys() {
			active.insert(name.clone(), ());
		}

		// The MKV `Tracks` element is written once and can't be amended, so
		// reject any rendition add/remove once the header has been emitted.
		if self.header_emitted {
			for name in active.keys() {
				if !self.tracks.contains_key(name) {
					anyhow::bail!("MKV track layout changed after header was emitted: track '{name}' added");
				}
			}
			for name in self.tracks.keys() {
				if !active.contains_key(name) {
					anyhow::bail!("MKV track layout changed after header was emitted: track '{name}' removed");
				}
			}
			self.catalog_snapshot = Some(catalog);
			return Ok(());
		}

		let mut next_track_number: u64 = self.tracks.values().map(|t| t.track_number).max().unwrap_or(0) + 1;

		for (name, config) in catalog.video.renditions.iter() {
			if self.tracks.contains_key(name) {
				continue;
			}
			ensure_legacy(&config.container, "video", name)?;
			let consumer = subscribe(&self.broadcast, name, &config.container, self.latency)?;
			let transform = build_video_transform(config)?;
			self.tracks.insert(
				name.clone(),
				MkvTrack {
					consumer,
					pending: None,
					finished: false,
					track_number: next_track_number,
					kind: TrackKind::Video,
					video_transform: transform,
				},
			);
			next_track_number += 1;
		}

		for (name, config) in catalog.audio.renditions.iter() {
			if self.tracks.contains_key(name) {
				continue;
			}
			ensure_legacy(&config.container, "audio", name)?;
			let consumer = subscribe(&self.broadcast, name, &config.container, self.latency)?;
			self.tracks.insert(
				name.clone(),
				MkvTrack {
					consumer,
					pending: None,
					finished: false,
					track_number: next_track_number,
					kind: TrackKind::Audio,
					video_transform: None,
				},
			);
			next_track_number += 1;
		}

		self.tracks.retain(|name, _| active.contains_key(name));
		self.catalog_snapshot = Some(catalog);
		Ok(())
	}

	/// Header is ready when every video track has its codec config — either
	/// supplied in the catalog or built by the transform.
	fn header_ready(&self) -> bool {
		for track in self.tracks.values() {
			if track.kind != TrackKind::Video {
				continue;
			}
			match &track.video_transform {
				Some(t) if t.codec_private().is_none() => return false,
				_ => {}
			}
		}
		true
	}

	fn build_header(&self) -> anyhow::Result<Bytes> {
		let catalog = self.catalog_snapshot.as_ref().context("no catalog snapshot")?;

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
		for (name, config) in catalog.video.renditions.iter() {
			let track = self.tracks.get(name).context("video track not subscribed")?;
			entries.push(build_video_track_entry(
				track.track_number,
				config,
				track.video_transform.as_ref(),
			)?);
		}
		for (name, config) in catalog.audio.renditions.iter() {
			let track = self.tracks.get(name).context("audio track not subscribed")?;
			entries.push(build_audio_track_entry(track.track_number, config)?);
		}

		let mut dest = Cursor::new(Vec::new());
		{
			let mut writer = WebmWriter::new(&mut dest);
			writer.write(&MatroskaSpec::Ebml(Master::Full(vec![
				MatroskaSpec::DocType(doc_type.to_string()),
				MatroskaSpec::DocTypeVersion(4),
				MatroskaSpec::DocTypeReadVersion(2),
			])))?;
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

		Ok(Bytes::from(dest.into_inner()))
	}

	fn pick_next_track(&self) -> Option<String> {
		self.tracks
			.iter()
			.filter_map(|(n, t)| t.pending.as_ref().map(|f| (n.clone(), f.timestamp)))
			.min_by_key(|(_, ts)| *ts)
			.map(|(n, _)| n)
	}

	/// Route an already-transformed frame through the cluster builder. Returns
	/// a chunk if the cluster rolled over (the returned chunk is the
	/// *previous* cluster; the new frame becomes the first block of a new
	/// open cluster).
	fn feed_frame(&mut self, name: &str, frame: Frame) -> anyhow::Result<Option<Bytes>> {
		let track = self.tracks.get(name).context("missing track")?;
		let track_number = track.track_number;
		let kind = track.kind;
		let payload = &frame.payload;

		let frame_ticks: u64 = (frame.timestamp.as_micros() / 1_000)
			.try_into()
			.context("timestamp doesn't fit in u64 ms")?;

		let is_video = kind == TrackKind::Video;
		let keyframe = frame.keyframe;

		let roll_over = match &self.cluster {
			None => true,
			Some(cluster) => {
				let overflow = !cluster.fits(frame_ticks);
				// Roll on a video keyframe only once the cluster already has video
				// frames in it — otherwise audio that arrived before the first
				// keyframe would split into its own (un-renderable) cluster.
				let gop_boundary = is_video && keyframe && cluster.has_video;
				// Optional time-based cap. Some(ZERO) means per-frame.
				let too_long = match self.fragment_duration {
					Some(d) if d.is_zero() => !cluster.body.is_empty(),
					Some(d) => frame_ticks.saturating_sub(cluster.start_ticks) >= d.as_millis() as u64,
					None => false,
				};
				overflow || gop_boundary || too_long
			}
		};

		let emit = if roll_over {
			let finished = self.cluster.take().map(|c| c.finish());
			self.cluster = Some(ClusterBuilder::new(frame_ticks));
			finished
		} else {
			None
		};

		self.cluster
			.as_mut()
			.unwrap()
			.append(track_number, frame_ticks, keyframe, payload, is_video)?;

		Ok(emit)
	}
}

fn ensure_legacy(container: &Container, kind: &str, name: &str) -> anyhow::Result<()> {
	match container {
		Container::Legacy => Ok(()),
		Container::Cmaf { .. } => {
			anyhow::bail!("MKV export does not support CMAF {} track '{}'", kind, name);
		}
		Container::Loc => {
			anyhow::bail!("MKV export does not support LOC {} track '{}'", kind, name);
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

/// Build a video transform for the given catalog config, or None if no
/// transform is needed (e.g. VP8/VP9/AV1, or H.264/H.265 with an existing avcC/hvcC).
fn build_video_transform(config: &VideoConfig) -> anyhow::Result<Option<VideoTransform>> {
	// A non-empty description means the source is already avc1/hvc1 (out-of-band
	// codec config + length-prefixed NALs); no transform needed.
	let needs_transform = config.description.as_ref().map(|d| d.is_empty()).unwrap_or(true);
	if !needs_transform {
		return Ok(None);
	}
	match &config.codec {
		VideoCodec::H264(_) => Ok(Some(VideoTransform::Avc1(Avc1::new()))),
		VideoCodec::H265(_) => Ok(Some(VideoTransform::Hvc1(Hvc1::new()))),
		_ => Ok(None),
	}
}

fn build_video_track_entry(
	track_number: u64,
	config: &VideoConfig,
	transform: Option<&VideoTransform>,
) -> anyhow::Result<MatroskaSpec> {
	// For Avc3/Hev1 sources the avcC/hvcC is synthesized by the transform;
	// for avc1/hvc1 sources it lives in the catalog description.
	let codec_private_from_transform = transform.and_then(|t| t.codec_private().map(|b| b.to_vec()));
	let codec_private_from_description = config
		.description
		.as_ref()
		.filter(|b| !b.is_empty())
		.map(|b| b.to_vec());

	let (codec_id, codec_private) = match &config.codec {
		VideoCodec::VP8 => ("V_VP8", None),
		VideoCodec::VP9(_) => ("V_VP9", None),
		VideoCodec::AV1(_) => ("V_AV1", codec_private_from_description),
		VideoCodec::H264(_) => {
			let avcc = codec_private_from_transform
				.or(codec_private_from_description)
				.context("H.264 track missing AVCDecoderConfigurationRecord")?;
			("V_MPEG4/ISO/AVC", Some(avcc))
		}
		VideoCodec::H265(_) => {
			let hvcc = codec_private_from_transform
				.or(codec_private_from_description)
				.context("H.265 track missing HEVCDecoderConfigurationRecord")?;
			("V_MPEGH/ISO/HEVC", Some(hvcc))
		}
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
