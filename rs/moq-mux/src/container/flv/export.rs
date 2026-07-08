//! FLV muxer.
//!
//! [`Export`] subscribes to a MoQ broadcast and produces a single FLV byte
//! stream: the file header, the video/audio sequence headers, then one tag per
//! media frame interleaved by timestamp. Legacy H.264 + AAC + MP3 are muxed as
//! the classic CodecID tags; HEVC, AV1, VP9, Opus, AC-3, and E-AC-3 are muxed as
//! the enhanced-RTMP (E-RTMP) FourCC payloads. Frames flow through [`ExportSource`],
//! which normalizes H.264/H.265 to length-prefixed NALU plus a resolved
//! avcC/hvcC (parsing inline avc3/hev1 parameter sets when needed) and hands the
//! other codecs through unchanged.
//!
//! By default FLV carries a single video and a single audio stream, so only the
//! first rendition of each kind is muxed and the rest are ignored. With
//! [`with_multitrack`](Export::with_multitrack) every rendition is muxed instead,
//! each as an enhanced-RTMP multitrack track addressed by its own track id (use
//! this only for a player that advertised the `Multitrack` capability).

use std::task::Poll;
use std::time::Duration;

use anyhow::Context;
use bytes::{BufMut, Bytes, BytesMut};
use hang::catalog::{AV1, AudioCodec, Catalog, Container, VideoCodec};

use super::{
	AAC_AUDIO_TAG_HEADER, AAC_RAW, AAC_SEQUENCE_HEADER, AUDIO_FORMAT_EX, AUDIO_PACKET_CODED_FRAMES,
	AUDIO_PACKET_MULTITRACK, AUDIO_PACKET_SEQUENCE_START, AVC_NALU, AVC_SEQUENCE_HEADER, FRAME_TYPE_INTER,
	FRAME_TYPE_KEY, MP3_AUDIO_TAG_HEADER, MULTITRACK_ONE_TRACK, TAG_AUDIO, TAG_HEADER_LEN, TAG_VIDEO, VIDEO_CODEC_AVC,
	VIDEO_EX_HEADER, VIDEO_PACKET_CODED_FRAMES, VIDEO_PACKET_MULTITRACK, VIDEO_PACKET_SEQUENCE_START,
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

	/// The enhanced-RTMP FourCC to use in multitrack framing, where every codec
	/// (including the ones with a legacy CodecID) is identified by FourCC.
	fn multitrack_fourcc(self) -> [u8; 4] {
		match self {
			Flavor::Avc => *b"avc1",
			Flavor::Aac => *b"mp4a",
			Flavor::Mp3 => *b".mp3",
			// The rest share their single-track FourCC.
			_ => self.fourcc().expect("enhanced flavor has a FourCC"),
		}
	}

	fn has_composition_time(self) -> bool {
		matches!(self, Flavor::Avc | Flavor::Hevc)
	}

	/// True if the codec carries an out-of-band config record (emitted as a
	/// sequence-header tag). VP9 / MP3 / AC-3 / E-AC-3 carry their config in band.
	fn has_sequence_header(self) -> bool {
		matches!(
			self,
			Flavor::Avc | Flavor::Hevc | Flavor::Av1 | Flavor::Aac | Flavor::Opus
		)
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
	/// Emit every rendition as an enhanced-RTMP multitrack track, rather than only
	/// the first video + first audio rendition.
	multitrack: bool,

	video: Vec<FlvTrack>,
	audio: Vec<FlvTrack>,

	/// True once the file header and sequence headers have been emitted.
	header_emitted: bool,
}

/// A subscribed rendition feeding the muxer.
struct FlvTrack {
	name: String,
	/// The enhanced-RTMP track id, assigned by bind order within its kind. Only
	/// used when muxing multitrack; a single-track FLV stream carries no id.
	track_id: u8,
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
	/// How far to run the authored decode clock behind the PTS, in FLV milliseconds.
	dts_reserve: u32,
	/// Last authored video DTS, in FLV milliseconds.
	last_dts: Option<u32>,
}

impl FlvTrack {
	fn tag_timestamp(&self, frame: &Frame) -> anyhow::Result<u32> {
		let pts = frame_timestamp_ms(frame)?;
		if self.flavor.has_composition_time() {
			author_dts(pts, self.dts_reserve, self.last_dts)
		} else {
			Ok(pts)
		}
	}

	/// The out-of-band config record for this track's sequence-header tag, or
	/// `None` for codecs that carry their config in band.
	fn config_record(&self) -> anyhow::Result<Option<&[u8]>> {
		if !self.flavor.has_sequence_header() {
			return Ok(None);
		}
		let record = self
			.source
			.description()
			.or(self.fallback_description.as_ref())
			.with_context(|| format!("FLV track '{}' missing its codec config record", self.name))?;
		Ok(Some(record.as_ref()))
	}
}

impl Export {
	/// Subscribe to `broadcast` and produce FLV byte chunks, using the default
	/// catalog format ([`CatalogFormat::Hang`]).
	pub fn new(broadcast: moq_net::BroadcastConsumer) -> Result<Self, crate::Error> {
		Self::with_catalog_format(broadcast, CatalogFormat::default())
	}

	/// Subscribe to `broadcast` and produce FLV byte chunks, selecting an explicit
	/// `catalog_format` for track discovery.
	pub fn with_catalog_format(
		broadcast: moq_net::BroadcastConsumer,
		catalog_format: CatalogFormat,
	) -> Result<Self, crate::Error> {
		let catalog = crate::catalog::Consumer::new(&broadcast, catalog_format)?;
		Ok(Self {
			broadcast,
			catalog: Some(catalog),
			latency: Duration::ZERO,
			multitrack: false,
			video: Vec::new(),
			audio: Vec::new(),
			header_emitted: false,
		})
	}

	/// Set the maximum buffering latency for each per-track source.
	pub fn with_latency(mut self, latency: Duration) -> Self {
		self.latency = latency;
		self
	}

	/// Mux every rendition as an enhanced-RTMP multitrack track (one FLV stream
	/// carrying several video and/or audio tracks), rather than only the first
	/// video + first audio rendition.
	///
	/// Only enable this for a player that advertised the enhanced-RTMP
	/// `Multitrack` capability in its `connect` `capsEx`; a legacy player can't
	/// parse the multitrack framing. Defaults to off.
	pub fn with_multitrack(mut self, multitrack: bool) -> Self {
		self.multitrack = multitrack;
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
		for track in self.video.iter_mut().chain(self.audio.iter_mut()) {
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

		// 4. Emit the smallest FLV tag timestamp as one tag.
		if let Some((is_video, index)) = self.pick_next_track()? {
			let track = if is_video {
				&mut self.video[index]
			} else {
				&mut self.audio[index]
			};
			let frame = track.pending.take().unwrap();
			let chunk = self.encode_frame(is_video, index, frame)?;
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
		self.video.iter().chain(self.audio.iter())
	}

	fn has_tracks(&self) -> bool {
		!self.video.is_empty() || !self.audio.is_empty()
	}

	fn update_catalog(&mut self, catalog: Catalog) -> anyhow::Result<()> {
		// A single-track FLV stream binds only the first rendition of each kind;
		// multitrack binds them all. Bind newly-seen renditions in name order (the
		// catalog is a BTreeMap) so each keeps a stable track id.
		//
		// Only bind before the header is emitted: the sequence-header (config) tags
		// go out with the header, and there's no in-band way to introduce a new
		// track's config mid-stream, so a rendition first seen afterward is left
		// unmuxed rather than emitted as undecodable config-less frames. (This
		// mirrors the single-track path ignoring extra renditions.)
		if !self.header_emitted {
			self.bind_video(&catalog)?;
			self.bind_audio(&catalog)?;
		} else if catalog.video.renditions.len() > self.video.len() || catalog.audio.renditions.len() > self.audio.len()
		{
			tracing::warn!("ignoring FLV rendition that appeared after the stream header");
		}

		// A bound track vanishing from the catalog is a layout change FLV can't express.
		for track in self.tracks() {
			let present = if is_video_flavor(track.flavor) {
				catalog.video.renditions.contains_key(&track.name)
			} else {
				catalog.audio.renditions.contains_key(&track.name)
			};
			anyhow::ensure!(present, "FLV track '{}' removed mid-stream", track.name);
		}

		Ok(())
	}

	fn bind_video(&mut self, catalog: &Catalog) -> anyhow::Result<()> {
		for (name, config) in &catalog.video.renditions {
			if !self.multitrack && !self.video.is_empty() {
				tracing::warn!("FLV export only supports one video track; ignoring the rest (enable multitrack)");
				break;
			}
			if self.video.iter().any(|t| &t.name == name) {
				continue;
			}
			let flavor = video_flavor(config)?;
			ensure_legacy(&config.container, "video", name)?;
			// AV1's av1C is optional in the catalog; synthesize one from the codec
			// struct so the enhanced SequenceStart tag always has a config record.
			let fallback_description = match (&config.codec, config.description.as_ref()) {
				(VideoCodec::AV1(av1), None) => Some(Bytes::copy_from_slice(&av1c_bytes(av1))),
				_ => None,
			};
			let source = ExportSource::for_video(&self.broadcast, name, config, self.latency)?;
			let track_id = u8::try_from(self.video.len()).context("too many FLV video tracks")?;
			self.video.push(FlvTrack {
				name: name.clone(),
				track_id,
				source,
				pending: None,
				finished: false,
				flavor,
				fallback_description,
				dts_reserve: dts_reserve(config),
				last_dts: None,
			});
		}
		Ok(())
	}

	fn bind_audio(&mut self, catalog: &Catalog) -> anyhow::Result<()> {
		for (name, config) in &catalog.audio.renditions {
			if !self.multitrack && !self.audio.is_empty() {
				tracing::warn!("FLV export only supports one audio track; ignoring the rest (enable multitrack)");
				break;
			}
			if self.audio.iter().any(|t| &t.name == name) {
				continue;
			}
			let flavor = audio_flavor(config)?;
			ensure_legacy(&config.container, "audio", name)?;
			let source = ExportSource::for_audio(&self.broadcast, name, config, self.latency)?;
			let track_id = u8::try_from(self.audio.len()).context("too many FLV audio tracks")?;
			self.audio.push(FlvTrack {
				name: name.clone(),
				track_id,
				source,
				pending: None,
				finished: false,
				flavor,
				fallback_description: None,
				dts_reserve: 0,
				last_dts: None,
			});
		}
		Ok(())
	}

	/// Header is ready once at least one track is bound and every bound track's
	/// [`ExportSource`] has resolved its codec config (from the catalog
	/// `description` or synthesized by the Avc3 transform).
	fn header_ready(&self) -> bool {
		self.has_tracks() && self.tracks().all(|t| t.source.header_ready())
	}

	/// Build the FLV file header plus every bound track's sequence-header tag.
	fn build_header(&self) -> anyhow::Result<Bytes> {
		let mut out = BytesMut::new();

		// FLV file header: signature, version 1, type flags, data offset = 9.
		out.put_slice(b"FLV");
		out.put_u8(1);
		let mut flags = 0u8;
		if !self.video.is_empty() {
			flags |= 0x01;
		}
		if !self.audio.is_empty() {
			flags |= 0x04;
		}
		out.put_u8(flags);
		out.put_u32(9);
		// PreviousTagSize0 is always zero.
		out.put_u32(0);

		for track in &self.video {
			if let Some(body) = self.video_sequence_header(track)? {
				write_tag(&mut out, TAG_VIDEO, 0, &body)?;
			}
		}
		for track in &self.audio {
			if let Some(body) = self.audio_sequence_header(track)? {
				write_tag(&mut out, TAG_AUDIO, 0, &body)?;
			}
		}

		Ok(out.freeze())
	}

	/// Pick the track with the smallest pending FLV tag timestamp. Returns whether
	/// it's a video track and its index within that kind, or `None` if no frame is
	/// pending. Video wins ties, matching the single-track interleave.
	fn pick_next_track(&self) -> anyhow::Result<Option<(bool, usize)>> {
		let mut best: Option<(u32, bool, usize)> = None;
		for (index, track) in self.video.iter().enumerate() {
			if let Some(frame) = &track.pending {
				let ts = track.tag_timestamp(frame)?;
				if best.is_none_or(|(b, _, _)| ts < b) {
					best = Some((ts, true, index));
				}
			}
		}
		for (index, track) in self.audio.iter().enumerate() {
			if let Some(frame) = &track.pending {
				let ts = frame_timestamp_ms(frame)?;
				if best.is_none_or(|(b, _, _)| ts < b) {
					best = Some((ts, false, index));
				}
			}
		}
		Ok(best.map(|(_, is_video, index)| (is_video, index)))
	}

	/// Encode one frame as a single FLV tag.
	fn encode_frame(&mut self, is_video: bool, index: usize, frame: Frame) -> anyhow::Result<Bytes> {
		let mut out = BytesMut::with_capacity(TAG_HEADER_LEN + frame.payload.len() + 8);
		let multitrack = self.multitrack;
		if is_video {
			let track = &mut self.video[index];
			let flavor = track.flavor;
			let track_id = track.track_id;
			let pts_ms = frame_timestamp_ms(&frame)?;
			let timestamp_ms = track.tag_timestamp(&frame)?;
			let frame_type = if frame.keyframe {
				FRAME_TYPE_KEY
			} else {
				FRAME_TYPE_INTER
			};
			let cts = if flavor.has_composition_time() {
				composition_time(pts_ms, timestamp_ms)?
			} else {
				0
			};
			let mut body = BytesMut::with_capacity(12 + frame.payload.len());
			if multitrack {
				// Enhanced multitrack CodedFrames: ex-header + framing byte + FourCC +
				// track id, then the per-codec composition time (avc1/hvc1) and payload.
				body.put_u8(VIDEO_EX_HEADER | (frame_type << 4) | VIDEO_PACKET_MULTITRACK);
				body.put_u8((MULTITRACK_ONE_TRACK << 4) | VIDEO_PACKET_CODED_FRAMES);
				body.put_slice(&flavor.multitrack_fourcc());
				body.put_u8(track_id);
				if flavor.has_composition_time() {
					write_i24(&mut body, cts)?;
				}
			} else {
				match flavor.fourcc() {
					// Legacy AVC: CodecID + AVCPacketType + signed composition time.
					None => {
						body.put_u8((frame_type << 4) | VIDEO_CODEC_AVC);
						body.put_u8(AVC_NALU);
						write_i24(&mut body, cts)?;
					}
					// Enhanced FourCC CodedFrames. hvc1 keeps the 3-byte composition
					// time; av01/vp09 omit it.
					Some(fourcc) => {
						body.put_u8(VIDEO_EX_HEADER | (frame_type << 4) | VIDEO_PACKET_CODED_FRAMES);
						body.put_slice(&fourcc);
						if flavor == Flavor::Hevc {
							write_i24(&mut body, cts)?;
						}
					}
				}
			}
			if flavor.has_composition_time() {
				track.last_dts = Some(timestamp_ms);
			}
			body.put_slice(&frame.payload);
			write_tag(&mut out, TAG_VIDEO, timestamp_ms, &body)?;
		} else {
			let track = &self.audio[index];
			let flavor = track.flavor;
			let track_id = track.track_id;
			let timestamp_ms = frame_timestamp_ms(&frame)?;
			let mut body = BytesMut::with_capacity(7 + frame.payload.len());
			if multitrack {
				body.put_u8((AUDIO_FORMAT_EX << 4) | AUDIO_PACKET_MULTITRACK);
				body.put_u8((MULTITRACK_ONE_TRACK << 4) | AUDIO_PACKET_CODED_FRAMES);
				body.put_slice(&flavor.multitrack_fourcc());
				body.put_u8(track_id);
			} else {
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
			}
			body.put_slice(&frame.payload);
			write_tag(&mut out, TAG_AUDIO, timestamp_ms, &body)?;
		}
		Ok(out.freeze())
	}

	/// Build the FLV video sequence-header tag body for `track`, or `None` for
	/// codecs that carry their config in band (VP9).
	fn video_sequence_header(&self, track: &FlvTrack) -> anyhow::Result<Option<BytesMut>> {
		let Some(config) = track.config_record()? else {
			return Ok(None);
		};
		let mut body = BytesMut::new();
		if self.multitrack {
			ex_multitrack_sequence_start(
				&mut body,
				TAG_VIDEO,
				&track.flavor.multitrack_fourcc(),
				track.track_id,
				config,
			);
			return Ok(Some(body));
		}
		match track.flavor {
			Flavor::Avc => {
				body.put_u8((FRAME_TYPE_KEY << 4) | VIDEO_CODEC_AVC);
				body.put_u8(AVC_SEQUENCE_HEADER);
				body.put_slice(&[0, 0, 0]); // composition time
				body.put_slice(config);
			}
			Flavor::Hevc => ex_video_sequence_start(&mut body, b"hvc1", config),
			Flavor::Av1 => ex_video_sequence_start(&mut body, b"av01", config),
			// Codecs with no out-of-band record are filtered out by `config_record`.
			Flavor::Vp9 | Flavor::Aac | Flavor::Mp3 | Flavor::Opus | Flavor::Ac3 | Flavor::Eac3 => {
				unreachable!("no video sequence header for {:?}", track.name)
			}
		}
		Ok(Some(body))
	}

	/// Build the FLV audio sequence-header tag body for `track`, or `None` for
	/// codecs that carry their config in band (MP3 / AC-3 / E-AC-3).
	fn audio_sequence_header(&self, track: &FlvTrack) -> anyhow::Result<Option<BytesMut>> {
		let Some(config) = track.config_record()? else {
			return Ok(None);
		};
		let mut body = BytesMut::new();
		if self.multitrack {
			ex_multitrack_sequence_start(
				&mut body,
				TAG_AUDIO,
				&track.flavor.multitrack_fourcc(),
				track.track_id,
				config,
			);
			return Ok(Some(body));
		}
		match track.flavor {
			Flavor::Aac => {
				body.put_u8(AAC_AUDIO_TAG_HEADER);
				body.put_u8(AAC_SEQUENCE_HEADER);
				body.put_slice(config);
			}
			Flavor::Opus => {
				body.put_u8((AUDIO_FORMAT_EX << 4) | AUDIO_PACKET_SEQUENCE_START);
				body.put_slice(b"Opus");
				body.put_slice(config);
			}
			// Codecs with no out-of-band record are filtered out by `config_record`.
			Flavor::Mp3 | Flavor::Ac3 | Flavor::Eac3 | Flavor::Avc | Flavor::Hevc | Flavor::Av1 | Flavor::Vp9 => {
				unreachable!("no audio sequence header for {:?}", track.name)
			}
		}
		Ok(Some(body))
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

fn is_video_flavor(flavor: Flavor) -> bool {
	matches!(flavor, Flavor::Avc | Flavor::Hevc | Flavor::Av1 | Flavor::Vp9)
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

fn frame_timestamp_ms(frame: &Frame) -> anyhow::Result<u32> {
	frame
		.timestamp
		.as_millis()
		.try_into()
		.context("FLV timestamp exceeds 32 bits")
}

fn composition_time(pts: u32, dts: u32) -> anyhow::Result<i32> {
	let cts = i64::from(pts) - i64::from(dts);
	i32::try_from(cts)
		.ok()
		.filter(|cts| (-0x80_0000..=0x7f_ffff).contains(cts))
		.context("FLV composition time exceeds signed 24 bits")
}

fn write_i24(out: &mut BytesMut, value: i32) -> anyhow::Result<()> {
	anyhow::ensure!(
		(-0x80_0000..=0x7f_ffff).contains(&value),
		"FLV composition time exceeds signed 24 bits"
	);
	out.put_slice(&(value as u32).to_be_bytes()[1..]);
	Ok(())
}

fn author_dts(pts: u32, reserve: u32, last: Option<u32>) -> anyhow::Result<u32> {
	let mut dts = pts.saturating_sub(reserve);
	if let Some(prev) = last
		&& dts <= prev
	{
		dts = prev.checked_add(1).context("FLV DTS exceeds 32 bits")?;
	}
	Ok(dts)
}

fn dts_reserve(config: &hang::catalog::VideoConfig) -> u32 {
	config
		.jitter
		.and_then(|t| u32::try_from(t.as_millis()).ok())
		.filter(|reserve| *reserve > 0)
		.unwrap_or(1)
}

/// Append an enhanced-RTMP video `SequenceStart` tag body: ex-header + FourCC +
/// the codec config record.
fn ex_video_sequence_start(body: &mut BytesMut, fourcc: &[u8; 4], config: &[u8]) {
	body.put_u8(VIDEO_EX_HEADER | (FRAME_TYPE_KEY << 4) | VIDEO_PACKET_SEQUENCE_START);
	body.put_slice(fourcc);
	body.put_slice(config);
}

/// Append an enhanced-RTMP multitrack `SequenceStart` tag body (a single
/// `OneTrack` record): ex-header + multitrack framing + FourCC + track id + the
/// codec config record. `tag_type` selects the video vs audio ex-header byte.
fn ex_multitrack_sequence_start(body: &mut BytesMut, tag_type: u8, fourcc: &[u8; 4], track_id: u8, config: &[u8]) {
	if tag_type == TAG_VIDEO {
		body.put_u8(VIDEO_EX_HEADER | (FRAME_TYPE_KEY << 4) | VIDEO_PACKET_MULTITRACK);
		body.put_u8((MULTITRACK_ONE_TRACK << 4) | VIDEO_PACKET_SEQUENCE_START);
	} else {
		body.put_u8((AUDIO_FORMAT_EX << 4) | AUDIO_PACKET_MULTITRACK);
		body.put_u8((MULTITRACK_ONE_TRACK << 4) | AUDIO_PACKET_SEQUENCE_START);
	}
	body.put_slice(fourcc);
	body.put_u8(track_id);
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
