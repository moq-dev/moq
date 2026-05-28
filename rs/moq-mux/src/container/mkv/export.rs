use std::collections::HashMap;
use std::io::Cursor;
use std::task::Poll;
use std::time::Duration;

use bytes::{BufMut, Bytes, BytesMut};
use hang::catalog::{AudioCodec, AudioConfig, Catalog, Container, VideoCodec, VideoConfig};
use webm_iterable::matroska_spec::{Master, MatroskaSpec};
use webm_iterable::{WebmWriter, WriteOptions};

use crate::Result;
use crate::catalog::Stream;
use crate::container::ExportSource;
use crate::container::Frame;
use crate::container::mkv::Error;

/// Matroska TimestampScale: 1 ms (in nanoseconds).
const TIMESTAMP_SCALE_NS: u64 = 1_000_000;

/// Subscribe to a moq broadcast and produce a single Matroska / WebM byte stream.
///
/// Built from a [`moq_net::BroadcastConsumer`], `Export` subscribes to the hang catalog,
/// (un)subscribes per-rendition tracks, decodes them via a per-track source, and
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
/// `Export` accepts Annex-B sources (`H264 { inline: true }`, `H265 { in_band: true }`,
/// catalog `description` empty) by attaching a [`crate::codec::h264::Avc1`] /
/// [`crate::codec::h265::Hvc1`] to each affected track. The transform caches
/// parameter sets, builds the out-of-band `AVCDecoderConfigurationRecord` /
/// `HEVCDecoderConfigurationRecord`, and length-prefixes sample NALs. Header
/// emission is deferred until every such track has produced its codec config
/// (typically the first keyframe).
///
/// Only Legacy-container tracks (raw codec payloads) are supported. CMAF tracks
/// (moof+mdat passthrough) are rejected with a clear error.
pub struct Export<S: Stream> {
	broadcast: moq_net::BroadcastConsumer,
	catalog: Option<S>,
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
	source: ExportSource,
	pending: Option<Frame>,
	finished: bool,
	track_number: u64,
	kind: TrackKind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TrackKind {
	Video,
	Audio,
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
	) -> Result<()> {
		let rel = (frame_ticks as i64)
			.checked_sub(self.start_ticks as i64)
			.ok_or(Error::ClusterUnderflow)?;
		let rel: i16 = rel.try_into().map_err(|_| Error::BlockTimestampOverflow)?;

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

impl<S: Stream> Export<S> {
	/// Subscribe to `broadcast` and produce MKV byte chunks, driving track
	/// (un)subscription from `catalog`.
	///
	/// `catalog` is any [`Stream`] of catalog snapshots, typically a
	/// [`catalog::Consumer`](crate::catalog::Consumer) directly, or wrapped in
	/// [`catalog::Filter`](crate::catalog::Filter) /
	/// [`catalog::Target`](crate::catalog::Target) to narrow the rendition set.
	pub fn new(broadcast: moq_net::BroadcastConsumer, catalog: S) -> Self {
		Self {
			broadcast,
			catalog: Some(catalog),
			latency: Duration::ZERO,
			fragment_duration: None,
			tracks: HashMap::new(),
			catalog_snapshot: None,
			header_emitted: false,
			cluster: None,
		}
	}

	/// Set the maximum buffering latency for each per-track source.
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
	pub async fn next(&mut self) -> Result<Option<Bytes>> {
		conducer::wait(|waiter| self.poll_next(waiter)).await
	}

	pub fn poll_next(&mut self, waiter: &conducer::Waiter) -> Poll<Result<Option<Bytes>>> {
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

		// 2. Pull frames from each track into `pending`. ExportSource has
		// already transformed Annex-B payloads (Avc3/Hev1) into length-prefixed
		// form and absorbed any parameter-only frames before returning.
		//
		// Pre-header: drop slices that arrived before this track's codec config
		// is ready. A receiver who subscribed mid-GOP can't render those bytes
		// without the header anyway, and parking them would stop us from
		// polling for the next SPS/PPS-bearing frame.
		let waiting_for_header = !self.header_emitted;
		for track in self.tracks.values_mut() {
			if track.pending.is_some() || track.finished {
				continue;
			}
			loop {
				match track.source.poll_read(waiter) {
					Poll::Ready(Ok(Some(frame))) => {
						if waiting_for_header && !track.source.header_ready() {
							// Drop this slice and keep polling for SPS/PPS.
							continue;
						}
						track.pending = Some(frame);
						break;
					}
					Poll::Ready(Ok(None)) => {
						track.finished = true;
						break;
					}
					Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
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

	fn update_catalog(&mut self, catalog: Catalog) -> Result<()> {
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
					return Err(Error::HeaderAddedTrack(name.clone()).into());
				}
			}
			for name in self.tracks.keys() {
				if !active.contains_key(name) {
					return Err(Error::HeaderRemovedTrack(name.clone()).into());
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
			let source = ExportSource::for_video(&self.broadcast, name, config, self.latency)?;
			self.tracks.insert(
				name.clone(),
				MkvTrack {
					source,
					pending: None,
					finished: false,
					track_number: next_track_number,
					kind: TrackKind::Video,
				},
			);
			next_track_number += 1;
		}

		for (name, config) in catalog.audio.renditions.iter() {
			if self.tracks.contains_key(name) {
				continue;
			}
			ensure_legacy(&config.container, "audio", name)?;
			let source = ExportSource::for_audio(&self.broadcast, name, config, self.latency)?;
			self.tracks.insert(
				name.clone(),
				MkvTrack {
					source,
					pending: None,
					finished: false,
					track_number: next_track_number,
					kind: TrackKind::Audio,
				},
			);
			next_track_number += 1;
		}

		self.tracks.retain(|name, _| active.contains_key(name));
		self.catalog_snapshot = Some(catalog);
		Ok(())
	}

	/// Header is ready when every track's [`ExportSource`] has resolved its
	/// codec config — from the catalog `description` or built by the transform.
	fn header_ready(&self) -> bool {
		self.tracks.values().all(|t| t.source.header_ready())
	}

	fn build_header(&self) -> Result<Bytes> {
		let catalog = self.catalog_snapshot.as_ref().ok_or(Error::NoCatalogSnapshot)?;

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
			let track = self
				.tracks
				.get(name)
				.ok_or_else(|| Error::MissingVideoTrack(name.clone()))?;
			entries.push(build_video_track_entry(
				track.track_number,
				config,
				track.source.description(),
			)?);
		}
		for (name, config) in catalog.audio.renditions.iter() {
			let track = self
				.tracks
				.get(name)
				.ok_or_else(|| Error::MissingAudioTrack(name.clone()))?;
			entries.push(build_audio_track_entry(track.track_number, config)?);
		}

		let mut dest = Cursor::new(Vec::new());
		{
			let mut writer = WebmWriter::new(&mut dest);
			writer
				.write(&MatroskaSpec::Ebml(Master::Full(vec![
					MatroskaSpec::DocType(doc_type.to_string()),
					MatroskaSpec::DocTypeVersion(4),
					MatroskaSpec::DocTypeReadVersion(2),
				])))
				.map_err(Error::from)?;
			writer
				.write_advanced(
					&MatroskaSpec::Segment(Master::Start),
					WriteOptions::is_unknown_sized_element(),
				)
				.map_err(Error::from)?;
			writer
				.write(&MatroskaSpec::Info(Master::Full(vec![
					MatroskaSpec::TimestampScale(TIMESTAMP_SCALE_NS),
					MatroskaSpec::MuxingApp("moq-mux".to_string()),
					MatroskaSpec::WritingApp("moq-mux".to_string()),
				])))
				.map_err(Error::from)?;
			writer
				.write(&MatroskaSpec::Tracks(Master::Full(entries)))
				.map_err(Error::from)?;
			writer.flush().map_err(Error::from)?;
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
	fn feed_frame(&mut self, name: &str, frame: Frame) -> Result<Option<Bytes>> {
		let track = self.tracks.get(name).ok_or(Error::MissingTrack)?;
		let track_number = track.track_number;
		let kind = track.kind;
		let payload = &frame.payload;

		// MKV's wire scale is ms (TIMESTAMP_SCALE_NS = 1_000_000). Re-express the
		// frame's timestamp directly at MILLI rather than going through micros.
		let frame_ticks: u64 = frame
			.timestamp
			.as_millis()
			.try_into()
			.map_err(|_| Error::TimestampU64)?;

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

fn ensure_legacy(container: &Container, kind: &str, name: &str) -> Result<()> {
	match container {
		// MKV emits raw codec payloads, so it accepts both wire formats whose
		// frames are raw codec bitstreams (Legacy varint, LOC properties).
		Container::Legacy | Container::Loc => Ok(()),
		Container::Cmaf { .. } => Err(Error::UnsupportedCmafTrack {
			kind: kind.to_string(),
			name: name.to_string(),
		}
		.into()),
	}
}

fn build_video_track_entry(
	track_number: u64,
	config: &VideoConfig,
	description: Option<&Bytes>,
) -> Result<MatroskaSpec> {
	// The description came from either the catalog (avc1/hvc1 sources) or
	// the codec transform (Avc3/Hev1 sources synthesizing it from inline params).
	let codec_private = description.map(|b| b.to_vec());

	let (codec_id, codec_private) = match &config.codec {
		VideoCodec::VP8 => ("V_VP8", None),
		VideoCodec::VP9(_) => ("V_VP9", None),
		VideoCodec::AV1(_) => ("V_AV1", codec_private),
		VideoCodec::H264(_) => {
			let avcc = codec_private.ok_or(Error::MissingH264Avcc)?;
			("V_MPEG4/ISO/AVC", Some(avcc))
		}
		VideoCodec::H265(_) => {
			let hvcc = codec_private.ok_or(Error::MissingH265Hvcc)?;
			("V_MPEGH/ISO/HEVC", Some(hvcc))
		}
		other => return Err(Error::UnsupportedVideoExport(format!("{:?}", other)).into()),
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

fn build_audio_track_entry(track_number: u64, config: &AudioConfig) -> Result<MatroskaSpec> {
	let (codec_id, codec_private) = match &config.codec {
		AudioCodec::Opus => (
			"A_OPUS",
			Some(
				crate::codec::opus::Config {
					sample_rate: config.sample_rate,
					channel_count: config.channel_count,
				}
				.encode()
				.to_vec(),
			),
		),
		AudioCodec::AAC(_) => (
			"A_AAC",
			Some(
				config
					.description
					.as_ref()
					.ok_or(Error::MissingAacDescription)?
					.to_vec(),
			),
		),
		other => return Err(Error::UnsupportedAudioExport(format!("{:?}", other)).into()),
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
