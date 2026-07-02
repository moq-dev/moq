//! FLV muxer.
//!
//! [`Export`] subscribes to a MoQ broadcast and produces a single FLV byte
//! stream: the file header, the video/audio sequence headers, then one tag per
//! media frame interleaved by timestamp. Legacy H.264 + AAC + MP3 are muxed as
//! the classic CodecID tags; HEVC, AV1, VP9, Opus, AC-3, and E-AC-3 are muxed as
//! the enhanced-RTMP (E-RTMP) FourCC payloads. Frames flow through [`ExportSource`],
//! which normalizes H.264/H.265 to length-prefixed NALU plus a resolved
//! avcC/hvcC (parsing inline avc3/hev1 parameter sets when needed) and hands the
//! other codecs through unchanged. FLV carries a single video and a single audio
//! stream, so only the first rendition of each kind is muxed; extra renditions
//! and any unsupported codec are rejected.

use std::task::Poll;
use std::time::Duration;

use anyhow::Context;
use bytes::{BufMut, Bytes, BytesMut};
use hang::catalog::{AV1, AudioCodec, Catalog, Container, VideoCodec};

use super::{
	AAC_AUDIO_TAG_HEADER, AAC_RAW, AAC_SEQUENCE_HEADER, AUDIO_FORMAT_EX, AUDIO_PACKET_CODED_FRAMES,
	AUDIO_PACKET_SEQUENCE_START, AVC_NALU, AVC_SEQUENCE_HEADER, FRAME_TYPE_INTER, FRAME_TYPE_KEY, MP3_AUDIO_TAG_HEADER,
	TAG_AUDIO, TAG_HEADER_LEN, TAG_VIDEO, VIDEO_CODEC_AVC, VIDEO_EX_HEADER, VIDEO_PACKET_CODED_FRAMES,
	VIDEO_PACKET_SEQUENCE_START,
};
use crate::catalog::{CatalogFormat, Stream};
use crate::container::{ExportSource, Frame};

/// Which FLV payload shape a bound track is muxed as: a legacy CodecID
/// (`Avc`/`Aac`) or an enhanced-RTMP FourCC codec.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Flavor {
	Avc,
	Hevc,
	Av1,
	Vp9,
	Aac,
	Mp3,
	Opus,
	Ac3,
	Eac3,
}

impl Flavor {
	/// The enhanced-RTMP FourCC for this codec, or `None` for the legacy
	/// (CodecID-signaled) AVC, AAC, and MP3 shapes.
	fn fourcc(self) -> Option<[u8; 4]> {
		match self {
			Flavor::Hevc => Some(*b"hvc1"),
			Flavor::Av1 => Some(*b"av01"),
			Flavor::Vp9 => Some(*b"vp09"),
			Flavor::Opus => Some(*b"Opus"),
			Flavor::Ac3 => Some(*b"ac-3"),
			Flavor::Eac3 => Some(*b"ec-3"),
			Flavor::Avc | Flavor::Aac | Flavor::Mp3 => None,
		}
	}
}

/// Subscribe to a broadcast and produce an FLV byte stream.
///
/// Use [`next`](Self::next) to pull byte chunks. The first chunk is the FLV file
/// header followed by the AVC/AAC sequence headers; each subsequent chunk is the
/// tag for one media frame. Returns `None` when the broadcast ends.
///
/// ## Avc3 sources
///
/// Annex-B H.264 (`H264 { inline: true }`, empty `description`) is accepted: an
/// [`Avc1`](crate::codec::h264::Avc1) transform caches the inline SPS/PPS, builds
/// the avcC for the sequence-header tag, and length-prefixes each sample. The
/// header is deferred until that codec config is available (typically the first
/// keyframe). Only Legacy and LOC container tracks (raw codec payloads) are
/// supported; CMAF tracks are rejected.
pub struct Export {
	broadcast: moq_net::BroadcastConsumer,
	catalog: Option<crate::catalog::Consumer>,
	latency: Duration,

	video: Option<FlvTrack>,
	audio: Option<FlvTrack>,

	/// True once the file header and sequence headers have been emitted.
	header_emitted: bool,
}

/// A subscribed rendition feeding the muxer.
struct FlvTrack {
	name: String,
	source: ExportSource,
	pending: Option<Frame>,
	finished: bool,
	/// The FLV payload shape (legacy CodecID vs enhanced FourCC) to mux this
	/// track as, fixed from its catalog codec when it's bound.
	flavor: Flavor,
	/// A codec config record synthesized at bind time for the sequence-header
	/// tag, used when the catalog (and thus [`ExportSource::description`]) carries
	/// none. Only AV1 needs this today (its av1C is optional in the catalog).
	fallback_description: Option<Bytes>,
}

impl Export {
	/// Subscribe to `broadcast` and produce FLV byte chunks, using the default
	/// catalog format ([`CatalogFormat::Hang`]).
	pub async fn new(broadcast: moq_net::BroadcastConsumer) -> Result<Self, crate::Error> {
		Self::with_catalog_format(broadcast, CatalogFormat::default()).await
	}

	/// Subscribe to `broadcast` and produce FLV byte chunks, selecting an explicit
	/// `catalog_format` for track discovery.
	pub async fn with_catalog_format(
		broadcast: moq_net::BroadcastConsumer,
		catalog_format: CatalogFormat,
	) -> Result<Self, crate::Error> {
		let catalog = crate::catalog::Consumer::new(&broadcast, catalog_format).await?;
		Ok(Self {
			broadcast,
			catalog: Some(catalog),
			latency: Duration::ZERO,
			video: None,
			audio: None,
			header_emitted: false,
		})
	}

	/// Set the maximum buffering latency for each per-track source.
	pub fn with_latency(mut self, latency: Duration) -> Self {
		self.latency = latency;
		self
	}

	/// Get the next byte chunk.
	pub async fn next(&mut self) -> anyhow::Result<Option<Bytes>> {
		kio::wait(|waiter| self.poll_next(waiter)).await
	}

	/// Poll for the next byte chunk.
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<anyhow::Result<Option<Bytes>>> {
		// 1. Drain catalog updates to discover the track layout.
		while let Some(catalog) = self.catalog.as_mut() {
			match catalog.poll_next(waiter)? {
				Poll::Ready(Some(snapshot)) => self.update_catalog(snapshot.media())?,
				Poll::Ready(None) => {
					self.catalog = None;
					break;
				}
				Poll::Pending => break,
			}
		}

		// 2. Pull frames from each track into `pending`. Pre-header, drop slices
		// that arrived before the track's codec config is ready: a mid-GOP joiner
		// can't render them, and parking would block polling for the next SPS/PPS.
		let waiting_for_header = !self.header_emitted;
		for track in [self.video.as_mut(), self.audio.as_mut()].into_iter().flatten() {
			if track.pending.is_some() || track.finished {
				continue;
			}
			loop {
				match track.source.poll_read(waiter) {
					Poll::Ready(Ok(Some(frame))) => {
						if waiting_for_header && !track.source.header_ready() {
							continue;
						}
						track.pending = Some(frame);
						break;
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

		// 3. Emit the header once every track's codec config has resolved.
		if !self.header_emitted {
			if self.header_ready() {
				let header = self.build_header()?;
				self.header_emitted = true;
				return Poll::Ready(Ok(Some(header)));
			}
			// The catalog closed and nothing more can resolve a codec config (no
			// tracks arrived, or every bound track ended first): there's no header.
			if self.catalog.is_none() && (!self.has_tracks() || self.tracks().all(|t| t.finished)) {
				return Poll::Ready(Ok(None));
			}
			return Poll::Pending;
		}

		// 4. Emit the smallest-timestamp pending frame as one tag.
		if let Some(is_video) = self.pick_next_track() {
			let track = if is_video {
				self.video.as_mut()
			} else {
				self.audio.as_mut()
			};
			let track = track.unwrap();
			let frame = track.pending.take().unwrap();
			let chunk = self.encode_frame(is_video, frame)?;
			return Poll::Ready(Ok(Some(chunk)));
		}

		// 5. End-of-stream once every subscribed track is drained.
		if self.has_tracks() && self.tracks().all(|t| t.finished && t.pending.is_none()) {
			if self.catalog.is_none() {
				return Poll::Ready(Ok(None));
			}
		} else if self.catalog.is_none() && !self.has_tracks() {
			return Poll::Ready(Ok(None));
		}

		Poll::Pending
	}

	/// Iterate the subscribed tracks (video first, then audio).
	fn tracks(&self) -> impl Iterator<Item = &FlvTrack> {
		[self.video.as_ref(), self.audio.as_ref()].into_iter().flatten()
	}

	fn has_tracks(&self) -> bool {
		self.video.is_some() || self.audio.is_some()
	}

	fn update_catalog(&mut self, catalog: Catalog) -> anyhow::Result<()> {
		// FLV carries one video and one audio stream. Bind to the first rendition of
		// each kind and ignore the rest; a layout change once bound is rejected.
		if self.video.is_none()
			&& let Some((name, config)) = catalog.video.renditions.iter().next()
		{
			let flavor = video_flavor(config)?;
			ensure_legacy(&config.container, "video", name)?;
			// AV1's av1C is optional in the catalog; synthesize one from the codec
			// struct so the enhanced SequenceStart tag always has a config record.
			let fallback_description = match (&config.codec, config.description.as_ref()) {
				(VideoCodec::AV1(av1), None) => Some(Bytes::copy_from_slice(&av1c_bytes(av1))),
				_ => None,
			};
			let source = ExportSource::for_video(&self.broadcast, name, config, self.latency)?;
			self.video = Some(FlvTrack {
				name: name.clone(),
				source,
				pending: None,
				finished: false,
				flavor,
				fallback_description,
			});
		}
		if catalog.video.renditions.len() > 1 {
			tracing::warn!("FLV export only supports one video track; ignoring the rest");
		}

		if self.audio.is_none()
			&& let Some((name, config)) = catalog.audio.renditions.iter().next()
		{
			let flavor = audio_flavor(config)?;
			ensure_legacy(&config.container, "audio", name)?;
			let source = ExportSource::for_audio(&self.broadcast, name, config, self.latency)?;
			self.audio = Some(FlvTrack {
				name: name.clone(),
				source,
				pending: None,
				finished: false,
				flavor,
				fallback_description: None,
			});
		}
		if catalog.audio.renditions.len() > 1 {
			tracing::warn!("FLV export only supports one audio track; ignoring the rest");
		}

		// A bound track vanishing from the catalog is a layout change FLV can't express.
		if let Some(track) = &self.video {
			anyhow::ensure!(
				catalog.video.renditions.contains_key(&track.name),
				"FLV video track '{}' removed mid-stream",
				track.name
			);
		}
		if let Some(track) = &self.audio {
			anyhow::ensure!(
				catalog.audio.renditions.contains_key(&track.name),
				"FLV audio track '{}' removed mid-stream",
				track.name
			);
		}

		Ok(())
	}

	/// Header is ready once at least one track is bound and every bound track's
	/// [`ExportSource`] has resolved its codec config (from the catalog
	/// `description` or synthesized by the Avc3 transform).
	fn header_ready(&self) -> bool {
		self.has_tracks() && self.tracks().all(|t| t.source.header_ready())
	}

	/// Build the FLV file header plus the AVC/AAC sequence-header tags.
	fn build_header(&self) -> anyhow::Result<Bytes> {
		let mut out = BytesMut::new();

		// FLV file header: signature, version 1, type flags, data offset = 9.
		out.put_slice(b"FLV");
		out.put_u8(1);
		let mut flags = 0u8;
		if self.video.is_some() {
			flags |= 0x01;
		}
		if self.audio.is_some() {
			flags |= 0x04;
		}
		out.put_u8(flags);
		out.put_u32(9);
		// PreviousTagSize0 is always zero.
		out.put_u32(0);

		if let Some(track) = &self.video
			&& let Some(body) = video_sequence_header(track)?
		{
			write_tag(&mut out, TAG_VIDEO, 0, &body)?;
		}
		if let Some(track) = &self.audio
			&& let Some(body) = audio_sequence_header(track)?
		{
			write_tag(&mut out, TAG_AUDIO, 0, &body)?;
		}

		Ok(out.freeze())
	}

	/// Pick the track with the smallest pending timestamp. Returns whether it's the
	/// video track, or `None` if no frame is pending.
	fn pick_next_track(&self) -> Option<bool> {
		let video = self
			.video
			.as_ref()
			.and_then(|t| t.pending.as_ref())
			.map(|f| f.timestamp);
		let audio = self
			.audio
			.as_ref()
			.and_then(|t| t.pending.as_ref())
			.map(|f| f.timestamp);
		match (video, audio) {
			(Some(v), Some(a)) => Some(v <= a),
			(Some(_), None) => Some(true),
			(None, Some(_)) => Some(false),
			(None, None) => None,
		}
	}

	/// Encode one frame as a single FLV tag.
	fn encode_frame(&self, is_video: bool, frame: Frame) -> anyhow::Result<Bytes> {
		let timestamp_ms: u32 = (frame.timestamp.as_millis())
			.try_into()
			.context("FLV timestamp exceeds 32 bits")?;

		let mut out = BytesMut::with_capacity(TAG_HEADER_LEN + frame.payload.len() + 8);
		if is_video {
			let flavor = self.video.as_ref().expect("video frame without a video track").flavor;
			let frame_type = if frame.keyframe {
				FRAME_TYPE_KEY
			} else {
				FRAME_TYPE_INTER
			};
			let mut body = BytesMut::with_capacity(8 + frame.payload.len());
			match flavor.fourcc() {
				// Legacy AVC: CodecID + AVCPacketType + composition time (PTS in the tag).
				None => {
					body.put_u8((frame_type << 4) | VIDEO_CODEC_AVC);
					body.put_u8(AVC_NALU);
					body.put_slice(&[0, 0, 0]);
				}
				// Enhanced FourCC CodedFrames. hvc1 keeps the 3-byte composition
				// time (zero, since we carry PTS in the tag); av01/vp09 omit it.
				Some(fourcc) => {
					body.put_u8(VIDEO_EX_HEADER | (frame_type << 4) | VIDEO_PACKET_CODED_FRAMES);
					body.put_slice(&fourcc);
					if flavor == Flavor::Hevc {
						body.put_slice(&[0, 0, 0]);
					}
				}
			}
			body.put_slice(&frame.payload);
			write_tag(&mut out, TAG_VIDEO, timestamp_ms, &body)?;
		} else {
			let flavor = self.audio.as_ref().expect("audio frame without an audio track").flavor;
			let mut body = BytesMut::with_capacity(5 + frame.payload.len());
			match flavor {
				// Legacy AAC: tag header + AACPacketType (raw frame follows).
				Flavor::Aac => {
					body.put_u8(AAC_AUDIO_TAG_HEADER);
					body.put_u8(AAC_RAW);
				}
				// Legacy MP3: tag header only; the raw frame carries its own config.
				Flavor::Mp3 => body.put_u8(MP3_AUDIO_TAG_HEADER),
				// Enhanced FourCC CodedFrames.
				_ => {
					let fourcc = flavor.fourcc().expect("enhanced audio flavor missing FourCC");
					body.put_u8((AUDIO_FORMAT_EX << 4) | AUDIO_PACKET_CODED_FRAMES);
					body.put_slice(&fourcc);
				}
			}
			body.put_slice(&frame.payload);
			write_tag(&mut out, TAG_AUDIO, timestamp_ms, &body)?;
		}
		Ok(out.freeze())
	}
}

/// Append one FLV tag (header + body + trailing `PreviousTagSize`) to `out`.
///
/// Errors if `body` exceeds FLV's 24-bit `DataSize` field (16 MiB), which would
/// otherwise be silently truncated into a corrupt header.
fn write_tag(out: &mut BytesMut, tag_type: u8, timestamp_ms: u32, body: &[u8]) -> anyhow::Result<()> {
	let size: u32 = body
		.len()
		.try_into()
		.ok()
		.filter(|n| *n <= 0x00FF_FFFF)
		.context("FLV tag body exceeds the 24-bit DataSize limit")?;
	out.put_u8(tag_type);
	out.put_slice(&size.to_be_bytes()[1..]); // 24-bit data size
	out.put_slice(&timestamp_ms.to_be_bytes()[1..]); // 24-bit timestamp (low)
	out.put_u8((timestamp_ms >> 24) as u8); // timestamp extension (high)
	out.put_slice(&[0, 0, 0]); // stream id
	out.put_slice(body);
	out.put_u32(TAG_HEADER_LEN as u32 + size);
	Ok(())
}

fn ensure_legacy(container: &Container, kind: &str, name: &str) -> anyhow::Result<()> {
	match container {
		Container::Legacy | Container::Loc => Ok(()),
		Container::Cmaf { .. } => anyhow::bail!("FLV export does not support CMAF {kind} track '{name}'"),
	}
}

fn video_flavor(config: &hang::catalog::VideoConfig) -> anyhow::Result<Flavor> {
	match &config.codec {
		VideoCodec::H264(_) => Ok(Flavor::Avc),
		VideoCodec::H265(_) => Ok(Flavor::Hevc),
		VideoCodec::AV1(_) => Ok(Flavor::Av1),
		VideoCodec::VP9(_) => Ok(Flavor::Vp9),
		other => anyhow::bail!("FLV export does not support video codec {other:?}"),
	}
}

fn audio_flavor(config: &hang::catalog::AudioConfig) -> anyhow::Result<Flavor> {
	match &config.codec {
		AudioCodec::AAC(_) => Ok(Flavor::Aac),
		AudioCodec::Mp3 => Ok(Flavor::Mp3),
		AudioCodec::Opus => Ok(Flavor::Opus),
		AudioCodec::Ac3 => Ok(Flavor::Ac3),
		AudioCodec::Ec3 => Ok(Flavor::Eac3),
		other => anyhow::bail!("FLV export does not support audio codec {other:?}"),
	}
}

/// Build the FLV video sequence-header tag body, or `None` for codecs that carry
/// their config in band (VP9), so no out-of-band record is emitted.
fn video_sequence_header(track: &FlvTrack) -> anyhow::Result<Option<BytesMut>> {
	let mut body = BytesMut::new();
	match track.flavor {
		Flavor::Avc => {
			let avcc = track.source.description().context("H.264 track missing avcC")?;
			body.put_u8((FRAME_TYPE_KEY << 4) | VIDEO_CODEC_AVC);
			body.put_u8(AVC_SEQUENCE_HEADER);
			body.put_slice(&[0, 0, 0]); // composition time
			body.put_slice(avcc);
		}
		Flavor::Hevc => {
			let hvcc = track.source.description().context("H.265 track missing hvcC")?;
			ex_video_sequence_start(&mut body, b"hvc1", hvcc);
		}
		Flavor::Av1 => {
			// av1C from the catalog `description`, or the record synthesized at bind
			// time (the sequence header is carried in band, so an empty configOBUs
			// record is enough for the decoder).
			let av1c = track
				.source
				.description()
				.or(track.fallback_description.as_ref())
				.context("AV1 track missing av1C")?;
			ex_video_sequence_start(&mut body, b"av01", av1c);
		}
		// VP9 configures the decoder from the key frame; no sequence header tag.
		Flavor::Vp9 => return Ok(None),
		Flavor::Aac | Flavor::Mp3 | Flavor::Opus | Flavor::Ac3 | Flavor::Eac3 => {
			unreachable!("audio flavor on a video track")
		}
	}
	Ok(Some(body))
}

/// Build the FLV audio sequence-header tag body, or `None` for codecs that carry
/// their config in band (MP3 / AC-3 / E-AC-3).
fn audio_sequence_header(track: &FlvTrack) -> anyhow::Result<Option<BytesMut>> {
	let mut body = BytesMut::new();
	match track.flavor {
		Flavor::Aac => {
			let asc = track
				.source
				.description()
				.context("AAC track missing AudioSpecificConfig")?;
			body.put_u8(AAC_AUDIO_TAG_HEADER);
			body.put_u8(AAC_SEQUENCE_HEADER);
			body.put_slice(asc);
		}
		Flavor::Opus => {
			let head = track.source.description().context("Opus track missing OpusHead")?;
			body.put_u8((AUDIO_FORMAT_EX << 4) | AUDIO_PACKET_SEQUENCE_START);
			body.put_slice(b"Opus");
			body.put_slice(head);
		}
		// MP3 / AC-3 / E-AC-3 carry their config in each frame; no sequence header tag.
		Flavor::Mp3 | Flavor::Ac3 | Flavor::Eac3 => return Ok(None),
		Flavor::Avc | Flavor::Hevc | Flavor::Av1 | Flavor::Vp9 => unreachable!("video flavor on an audio track"),
	}
	Ok(Some(body))
}

/// Append an enhanced-RTMP video `SequenceStart` tag body: ex-header + FourCC +
/// the codec config record.
fn ex_video_sequence_start(body: &mut BytesMut, fourcc: &[u8; 4], config: &[u8]) {
	body.put_u8(VIDEO_EX_HEADER | (FRAME_TYPE_KEY << 4) | VIDEO_PACKET_SEQUENCE_START);
	body.put_slice(fourcc);
	body.put_slice(config);
}

/// Build a minimal `AV1CodecConfigurationRecord` (av1C) from the catalog AV1
/// struct, with an empty `configOBUs` (the sequence header is carried in band).
fn av1c_bytes(av1: &AV1) -> [u8; 4] {
	let high_bitdepth = av1.bitdepth >= 10;
	let twelve_bit = av1.bitdepth >= 12;
	[
		0x81, // marker (1) + version (1)
		((av1.profile & 0x07) << 5) | (av1.level & 0x1f),
		((av1.tier == 'H') as u8) << 7
			| (high_bitdepth as u8) << 6
			| (twelve_bit as u8) << 5
			| (av1.mono_chrome as u8) << 4
			| (av1.chroma_subsampling_x as u8) << 3
			| (av1.chroma_subsampling_y as u8) << 2
			| (av1.chroma_sample_position & 0x03),
		0x00, // no initial presentation delay
	]
}
