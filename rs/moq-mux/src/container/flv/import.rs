//! FLV demuxer.
//!
//! [`Import`] reads an FLV byte stream, splits it into tags, and routes the
//! video and audio onto MoQ tracks. Two payload generations are handled:
//!
//! - **Legacy FLV/RTMP**: H.264 (AVC) video carried as length-prefixed NALU with
//!   an out-of-band `AVCDecoderConfigurationRecord` (avcC), and AAC audio with an
//!   out-of-band `AudioSpecificConfig`.
//! - **Enhanced RTMP (E-RTMP)**: the FourCC-signaled payloads OBS and ffmpeg emit
//!   for HEVC (`hvc1`), AV1 (`av01`), VP9 (`vp09`), and the legacy AVC FourCC
//!   (`avc1`); plus enhanced audio for Opus (`Opus`), AC-3 (`ac-3`), E-AC-3
//!   (`ec-3`), AAC (`mp4a`), and MP3 (`.mp3`).
//!
//! MP3 is also accepted as the legacy SoundFormat 2 audio tag.
//!
//! Each codec's out-of-band config record (avcC / hvcC / av1C / `AudioSpecificConfig`
//! / `OpusHead`) becomes the catalog `description`; VP9 and the verbatim audio
//! codecs (MP3 / AC-3 / E-AC-3) carry their config in band, so they configure from
//! the first frame instead. Sample bytes already match the [`Legacy`](crate::catalog::hang::Container)
//! container, so no codec transform is needed. FLAC (`fLaC`) enhanced audio, and
//! any other codec, are logged and dropped.

use std::collections::BTreeMap;

use anyhow::Context;
use bytes::{Buf, Bytes, BytesMut};
use hang::catalog::{AAC, AudioCodec, AudioConfig, Container, H264, VideoConfig};

use super::{
	AAC_RAW, AAC_SEQUENCE_HEADER, AUDIO_FORMAT_AAC, AUDIO_FORMAT_EX, AUDIO_FORMAT_MP3, AUDIO_PACKET_CODED_FRAMES,
	AUDIO_PACKET_MULTICHANNEL_CONFIG, AUDIO_PACKET_MULTITRACK, AUDIO_PACKET_SEQUENCE_END, AUDIO_PACKET_SEQUENCE_START,
	AVC_NALU, AVC_SEQUENCE_HEADER, FILE_HEADER_LEN, FRAME_TYPE_KEY, MULTITRACK_MANY_TRACKS,
	MULTITRACK_MANY_TRACKS_MANY_CODECS, MULTITRACK_ONE_TRACK, PREV_TAG_SIZE_LEN, TAG_AUDIO, TAG_HEADER_LEN, TAG_SCRIPT,
	TAG_VIDEO, VIDEO_CODEC_AVC, VIDEO_EX_HEADER, VIDEO_PACKET_CODED_FRAMES, VIDEO_PACKET_CODED_FRAMES_X,
	VIDEO_PACKET_METADATA, VIDEO_PACKET_MULTITRACK, VIDEO_PACKET_SEQUENCE_END, VIDEO_PACKET_SEQUENCE_START, read_i24,
	read_u24,
};
use crate::container::{Frame, Timestamp};

/// Implicit RTMP track id for a legacy or single-track enhanced tag (which carry
/// no explicit id). Multitrack tags address tracks by an explicit id instead.
const DEFAULT_TRACK_ID: u8 = 0;

/// Upper bound on the FLV header's `data_offset`. The header is 9 bytes in
/// practice; this cap stops a crafted offset from forcing unbounded buffering.
const MAX_DATA_OFFSET: usize = 64 * 1024;

/// Demuxes an FLV byte stream into a MoQ broadcast.
///
/// Supports legacy H.264 + AAC and the enhanced-RTMP FourCC codecs (HEVC, AV1,
/// VP9, Opus, AC-3, E-AC-3), the payloads produced by RTMP encoders and
/// `ffmpeg -f flv`. Unsupported codecs, plus `onMetaData` script tags, are logged
/// and dropped.
///
/// Legacy and single-track enhanced tags carry one video and one audio track;
/// enhanced-RTMP multitrack tags carry several of each, addressed by track id,
/// and each id becomes its own catalog rendition. A new sequence header for a
/// track replaces that track's previous configuration.
pub struct Import<E: crate::catalog::hang::CatalogExt = ()> {
	broadcast: moq_net::BroadcastProducer,
	catalog: crate::catalog::Producer<E>,

	/// Accumulated unparsed input. Whole tags are drained out; a trailing partial
	/// tag is retained for the next [`decode`](Self::decode) call.
	buffer: BytesMut,
	/// True once the 9-byte FLV file header and its `PreviousTagSize0` have been consumed.
	header_seen: bool,

	/// Demuxed video tracks keyed by RTMP track id (legacy / single-track tags use
	/// [`DEFAULT_TRACK_ID`]).
	video: BTreeMap<u8, VideoStream>,
	/// Demuxed audio tracks keyed by RTMP track id.
	audio: BTreeMap<u8, AudioStream>,
}

/// The demuxed video track plus its current catalog config, so a repeated
/// (identical) sequence header is a no-op rather than a track rebuild.
struct VideoStream {
	track: crate::container::Producer<crate::catalog::hang::Container>,
	config: VideoConfig,
}

/// The demuxed audio track plus its current catalog config.
struct AudioStream {
	track: crate::container::Producer<crate::catalog::hang::Container>,
	config: AudioConfig,
}

impl<E: crate::catalog::hang::CatalogExt> Import<E> {
	/// Create a demuxer publishing into `broadcast` with renditions announced on `catalog`.
	pub fn new(broadcast: moq_net::BroadcastProducer, catalog: crate::catalog::Producer<E>) -> Self {
		Self {
			broadcast,
			catalog,
			buffer: BytesMut::new(),
			header_seen: false,
			video: BTreeMap::new(),
			audio: BTreeMap::new(),
		}
	}

	/// Append `buf` to the internal scratch and demux every whole tag it now
	/// completes. The buffer is fully consumed; a trailing partial tag is retained
	/// for the next call.
	pub fn decode(&mut self, data: &[u8]) -> anyhow::Result<()> {
		self.buffer.extend_from_slice(data);

		self.drain()
	}

	fn drain(&mut self) -> anyhow::Result<()> {
		if !self.header_seen {
			if self.buffer.len() < FILE_HEADER_LEN {
				return Ok(());
			}
			anyhow::ensure!(&self.buffer[0..3] == b"FLV", "not an FLV stream");
			// data_offset is where the body starts (>= 9). It's followed by the
			// 4-byte PreviousTagSize0 before the first tag.
			let data_offset =
				u32::from_be_bytes([self.buffer[5], self.buffer[6], self.buffer[7], self.buffer[8]]) as usize;
			anyhow::ensure!(data_offset >= FILE_HEADER_LEN, "invalid FLV data offset");
			// The header is tiny in practice (9 bytes). Cap it so a crafted offset
			// can't force unbounded buffering before the first tag is reached.
			anyhow::ensure!(data_offset <= MAX_DATA_OFFSET, "FLV data offset too large");
			if self.buffer.len() < data_offset + PREV_TAG_SIZE_LEN {
				return Ok(());
			}
			self.buffer.advance(data_offset + PREV_TAG_SIZE_LEN);
			self.header_seen = true;
		}

		while self.buffer.len() >= TAG_HEADER_LEN {
			let tag_type = self.buffer[0];
			let data_size = read_u24(&self.buffer[1..4]) as usize;
			// Header + body + the trailing PreviousTagSize that follows every tag.
			let total = TAG_HEADER_LEN + data_size + PREV_TAG_SIZE_LEN;
			if self.buffer.len() < total {
				break;
			}

			// FLV timestamps are milliseconds: a 24-bit field plus an 8-bit
			// most-significant extension byte.
			let timestamp = (read_u24(&self.buffer[4..7]) as u64) | ((self.buffer[7] as u64) << 24);
			let body = Bytes::copy_from_slice(&self.buffer[TAG_HEADER_LEN..TAG_HEADER_LEN + data_size]);
			self.buffer.advance(total);

			match tag_type {
				TAG_VIDEO => self.handle_video(&body, timestamp)?,
				TAG_AUDIO => self.handle_audio(&body, timestamp)?,
				TAG_SCRIPT => {} // onMetaData and friends: nothing we need.
				other => tracing::debug!(tag_type = other, "ignoring unknown FLV tag"),
			}
		}

		Ok(())
	}

	fn handle_video(&mut self, body: &[u8], timestamp: u64) -> anyhow::Result<()> {
		let Some(&first) = body.first() else {
			return Ok(());
		};
		// The enhanced E-RTMP signaling sets the high bit and switches to FourCC
		// codec identification.
		if first & VIDEO_EX_HEADER != 0 {
			return self.handle_video_enhanced(first, body, timestamp);
		}

		let frame_type = first >> 4;
		let codec_id = first & 0x0f;
		if codec_id != VIDEO_CODEC_AVC {
			tracing::warn!(codec_id, "unsupported FLV video codec, dropping");
			return Ok(());
		}

		anyhow::ensure!(body.len() >= 5, "AVC video tag too short");
		let avc_packet_type = body[1];
		let composition_time = read_i24(&body[2..5]);
		let data = &body[5..];

		match avc_packet_type {
			AVC_SEQUENCE_HEADER => self.init_video(DEFAULT_TRACK_ID, config_from_avcc(data)?),
			AVC_NALU => self.write_video(
				DEFAULT_TRACK_ID,
				data,
				timestamp,
				composition_time,
				frame_type == FRAME_TYPE_KEY,
			),
			// AVCPacketType 2 is "end of sequence"; nothing to emit.
			_ => Ok(()),
		}
	}

	/// Handle an enhanced-RTMP (FourCC) video tag, single-track or multitrack.
	fn handle_video_enhanced(&mut self, first: u8, body: &[u8], timestamp: u64) -> anyhow::Result<()> {
		let keyframe = (first >> 4) & 0x07 == FRAME_TYPE_KEY;
		let packet_type = first & 0x0f;

		if packet_type == VIDEO_PACKET_MULTITRACK {
			return self.handle_video_multitrack(&body[1..], keyframe, timestamp);
		}

		anyhow::ensure!(body.len() >= 5, "enhanced video tag too short for FourCC");
		let fourcc: [u8; 4] = body[1..5].try_into().expect("slice is 4 bytes");
		self.handle_video_track(DEFAULT_TRACK_ID, &fourcc, packet_type, keyframe, &body[5..], timestamp)
	}

	/// Split a multitrack video tag into its per-track payloads and route each to
	/// [`handle_video_track`](Self::handle_video_track).
	///
	/// `data` starts at the multitrack framing byte (after the ex-header byte).
	fn handle_video_multitrack(&mut self, data: &[u8], keyframe: bool, timestamp: u64) -> anyhow::Result<()> {
		let mut cursor = data;
		let header = split_multitrack_header(&mut cursor, "video")?;
		anyhow::ensure!(
			header.packet_type != VIDEO_PACKET_MULTITRACK,
			"nested multitrack video tag"
		);

		loop {
			let track = split_multitrack_track(&mut cursor, &header, "video")?;
			self.handle_video_track(
				track.track_id,
				&track.fourcc,
				header.packet_type,
				keyframe,
				track.payload,
				timestamp,
			)?;
			if header.multitrack_type == MULTITRACK_ONE_TRACK || cursor.is_empty() {
				break;
			}
		}
		Ok(())
	}

	/// Route one track's video payload (a FourCC codec + `VideoPacketType`) onto
	/// the track's MoQ rendition, keyed by `track_id`.
	fn handle_video_track(
		&mut self,
		track_id: u8,
		fourcc: &[u8; 4],
		packet_type: u8,
		keyframe: bool,
		payload: &[u8],
		timestamp: u64,
	) -> anyhow::Result<()> {
		match packet_type {
			VIDEO_PACKET_SEQUENCE_START => {
				let config = match fourcc {
					b"avc1" => config_from_avcc(payload)?,
					b"hvc1" => crate::codec::h265::config_from_hvcc(payload)?,
					b"av01" => crate::codec::av1::config_from_av1c(payload)?,
					// VP9 carries its config in band; the SequenceStart vpcC is
					// redundant with the key-frame header we configure from.
					b"vp09" => return Ok(()),
					other => {
						tracing::warn!(fourcc = ?other, "unsupported enhanced FLV video codec, dropping");
						return Ok(());
					}
				};
				self.init_video(track_id, config)
			}
			VIDEO_PACKET_CODED_FRAMES | VIDEO_PACKET_CODED_FRAMES_X => {
				// hvc1/avc1 CodedFrames prefix a 3-byte composition time; CodedFramesX
				// and the always-zero-offset av01/vp09 do not.
				let has_cts = packet_type == VIDEO_PACKET_CODED_FRAMES && matches!(fourcc, b"hvc1" | b"avc1");
				let (data, cts) = if has_cts {
					anyhow::ensure!(payload.len() >= 3, "enhanced CodedFrames missing composition time");
					(&payload[3..], read_i24(&payload[0..3]))
				} else {
					(payload, 0)
				};

				// VP9 has no out-of-band config record, so (re)configure from each key
				// frame's uncompressed header. `init_video` dedups when unchanged, so
				// this is a no-op except on the first key frame or a resolution change.
				// A malformed header drops just this frame rather than aborting the stream.
				if fourcc == b"vp09" && keyframe {
					match crate::codec::vp9::config_from_keyframe(data) {
						Ok(Some(config)) => self.init_video(track_id, config)?,
						Ok(None) => {}
						Err(err) => tracing::warn!(%err, "dropping malformed VP9 key frame"),
					}
				}

				self.write_video(track_id, data, timestamp, cts, keyframe)
			}
			VIDEO_PACKET_SEQUENCE_END | VIDEO_PACKET_METADATA => Ok(()),
			other => {
				tracing::debug!(packet_type = other, "ignoring enhanced FLV video packet type");
				Ok(())
			}
		}
	}

	fn handle_audio(&mut self, body: &[u8], timestamp: u64) -> anyhow::Result<()> {
		let Some(&first) = body.first() else {
			return Ok(());
		};
		let sound_format = first >> 4;
		if sound_format == AUDIO_FORMAT_EX {
			return self.handle_audio_enhanced(first, body, timestamp);
		}
		if sound_format == AUDIO_FORMAT_MP3 {
			// Legacy MP3: the raw frame follows the one-byte tag header, with the
			// config in band. Configure from the first frame, then write it.
			let frame = &body[1..];
			if !self.audio.contains_key(&DEFAULT_TRACK_ID) {
				self.init_audio(DEFAULT_TRACK_ID, config_from_mp3(frame)?)?;
			}
			return self.write_audio(DEFAULT_TRACK_ID, frame, timestamp);
		}
		if sound_format != AUDIO_FORMAT_AAC {
			tracing::warn!(sound_format, "unsupported FLV audio format, dropping");
			return Ok(());
		}

		anyhow::ensure!(body.len() >= 2, "AAC audio tag too short");
		let aac_packet_type = body[1];
		let data = &body[2..];

		match aac_packet_type {
			AAC_SEQUENCE_HEADER => self.init_audio(DEFAULT_TRACK_ID, config_from_asc(data)?),
			AAC_RAW => self.write_audio(DEFAULT_TRACK_ID, data, timestamp),
			_ => Ok(()),
		}
	}

	/// Handle an enhanced-RTMP (FourCC) audio tag, single-track or multitrack.
	fn handle_audio_enhanced(&mut self, first: u8, body: &[u8], timestamp: u64) -> anyhow::Result<()> {
		let packet_type = first & 0x0f;

		if packet_type == AUDIO_PACKET_MULTITRACK {
			return self.handle_audio_multitrack(&body[1..], timestamp);
		}

		anyhow::ensure!(body.len() >= 5, "enhanced audio tag too short for FourCC");
		let fourcc: [u8; 4] = body[1..5].try_into().expect("slice is 4 bytes");
		self.handle_audio_track(DEFAULT_TRACK_ID, &fourcc, packet_type, &body[5..], timestamp)
	}

	/// Split a multitrack audio tag into its per-track payloads and route each to
	/// [`handle_audio_track`](Self::handle_audio_track).
	fn handle_audio_multitrack(&mut self, data: &[u8], timestamp: u64) -> anyhow::Result<()> {
		let mut cursor = data;
		let header = split_multitrack_header(&mut cursor, "audio")?;
		anyhow::ensure!(
			header.packet_type != AUDIO_PACKET_MULTITRACK,
			"nested multitrack audio tag"
		);

		loop {
			let track = split_multitrack_track(&mut cursor, &header, "audio")?;
			self.handle_audio_track(
				track.track_id,
				&track.fourcc,
				header.packet_type,
				track.payload,
				timestamp,
			)?;
			if header.multitrack_type == MULTITRACK_ONE_TRACK || cursor.is_empty() {
				break;
			}
		}
		Ok(())
	}

	/// Route one track's audio payload (a FourCC codec + `AudioPacketType`) onto
	/// the track's MoQ rendition, keyed by `track_id`.
	fn handle_audio_track(
		&mut self,
		track_id: u8,
		fourcc: &[u8; 4],
		packet_type: u8,
		payload: &[u8],
		timestamp: u64,
	) -> anyhow::Result<()> {
		match packet_type {
			AUDIO_PACKET_SEQUENCE_START => {
				let config = match fourcc {
					b"Opus" => config_from_opus_head(payload)?,
					b"mp4a" => config_from_asc(payload)?,
					// MP3 / AC-3 / E-AC-3 are verbatim with no sequence header; they
					// configure from the first frame. Anything else is unsupported.
					other => {
						tracing::warn!(fourcc = ?other, "unsupported enhanced FLV audio codec, dropping");
						return Ok(());
					}
				};
				self.init_audio(track_id, config)
			}
			AUDIO_PACKET_CODED_FRAMES => {
				// MP3 / AC-3 / E-AC-3 carry their config in the frame header, so
				// configure from the first frame when no sequence header preceded it.
				if !self.audio.contains_key(&track_id) {
					let config = match fourcc {
						b".mp3" => Some(config_from_mp3(payload)?),
						b"ac-3" => Some(config_from_ac3(payload)?),
						b"ec-3" => Some(config_from_eac3(payload)?),
						_ => None,
					};
					if let Some(config) = config {
						self.init_audio(track_id, config)?;
					}
				}
				self.write_audio(track_id, payload, timestamp)
			}
			AUDIO_PACKET_SEQUENCE_END | AUDIO_PACKET_MULTICHANNEL_CONFIG => Ok(()),
			other => {
				tracing::debug!(packet_type = other, "ignoring enhanced FLV audio packet type");
				Ok(())
			}
		}
	}

	/// Write one decoded video sample, dropping a leading delta before the first
	/// keyframe (a mid-GOP join) rather than aborting.
	fn write_video(
		&mut self,
		track_id: u8,
		data: &[u8],
		dts: u64,
		composition_time: i32,
		keyframe: bool,
	) -> anyhow::Result<()> {
		let Some(stream) = self.video.get_mut(&track_id) else {
			tracing::debug!("video frame before sequence header, dropping");
			return Ok(());
		};
		// FLV stores DTS in the tag; PTS is DTS plus the composition offset.
		let pts_ms = (dts as i64) + (composition_time as i64);
		anyhow::ensure!(pts_ms >= 0, "negative video presentation timestamp");
		match stream.track.write(Frame {
			timestamp: Timestamp::from_millis(pts_ms as u64)?,
			duration: None,
			payload: Bytes::copy_from_slice(data),
			keyframe,
		}) {
			Ok(()) | Err(crate::Error::MissingKeyframe(_)) => Ok(()),
			Err(e) => Err(e.into()),
		}
	}

	/// Write one audio frame as its own group, so the relay can forward it immediately.
	fn write_audio(&mut self, track_id: u8, data: &[u8], timestamp: u64) -> anyhow::Result<()> {
		let Some(stream) = self.audio.get_mut(&track_id) else {
			tracing::debug!("audio frame before config, dropping");
			return Ok(());
		};
		stream.track.write(Frame {
			timestamp: Timestamp::from_millis(timestamp)?,
			duration: None,
			payload: Bytes::copy_from_slice(data),
			keyframe: true,
		})?;
		stream.track.finish_group()?;
		Ok(())
	}

	/// (Re)build the video track `track_id` for `config`, unless it matches the current one.
	fn init_video(&mut self, track_id: u8, config: VideoConfig) -> anyhow::Result<()> {
		if self.video.get(&track_id).is_some_and(|s| s.config == config) {
			return Ok(());
		}

		let net_track = self.replace_video(track_id)?;
		self.catalog
			.lock()
			.video
			.renditions
			.insert(net_track.name().to_string(), config.clone());
		self.video.insert(
			track_id,
			VideoStream {
				// Leading deltas before the first keyframe are skipped at the write
				// site (the producer reports MissingKeyframe), so a mid-GOP join works.
				track: crate::container::Producer::new(net_track, crate::catalog::hang::Container::Legacy),
				config,
			},
		);
		Ok(())
	}

	/// (Re)build the audio track `track_id` for `config`, unless it matches the current one.
	fn init_audio(&mut self, track_id: u8, config: AudioConfig) -> anyhow::Result<()> {
		if self.audio.get(&track_id).is_some_and(|s| s.config == config) {
			return Ok(());
		}

		let net_track = self.replace_audio(track_id)?;
		self.catalog
			.lock()
			.audio
			.renditions
			.insert(net_track.name().to_string(), config.clone());
		self.audio.insert(
			track_id,
			AudioStream {
				track: crate::container::Producer::new(net_track, crate::catalog::hang::Container::Legacy),
				config,
			},
		);
		Ok(())
	}

	/// Drop any existing video track `track_id` (finishing it and clearing its
	/// catalog rendition) and allocate a fresh one.
	fn replace_video(&mut self, track_id: u8) -> anyhow::Result<moq_net::TrackProducer> {
		if let Some(mut old) = self.video.remove(&track_id) {
			old.track.finish()?;
			self.catalog.lock().video.renditions.remove(old.track.name());
		}
		Ok(self.broadcast.unique_track(".flv-v")?)
	}

	/// Drop any existing audio track `track_id` (finishing it and clearing its
	/// catalog rendition) and allocate a fresh one.
	fn replace_audio(&mut self, track_id: u8) -> anyhow::Result<moq_net::TrackProducer> {
		if let Some(mut old) = self.audio.remove(&track_id) {
			old.track.finish()?;
			self.catalog.lock().audio.renditions.remove(old.track.name());
		}
		Ok(self.broadcast.unique_track(".flv-a")?)
	}

	/// Close the current group on every track and reopen at `sequence`.
	pub fn seek(&mut self, sequence: u64) -> anyhow::Result<()> {
		for stream in self.video.values_mut() {
			stream.track.seek(sequence)?;
		}
		for stream in self.audio.values_mut() {
			stream.track.seek(sequence)?;
		}
		Ok(())
	}

	/// Finish every track, flushing the current group.
	pub fn finish(&mut self) -> anyhow::Result<()> {
		for stream in self.video.values_mut() {
			stream.track.finish()?;
		}
		for stream in self.audio.values_mut() {
			stream.track.finish()?;
		}
		Ok(())
	}
}

impl<E: crate::catalog::hang::CatalogExt> Drop for Import<E> {
	fn drop(&mut self) {
		let mut catalog = self.catalog.lock();
		for stream in self.video.values() {
			catalog.video.renditions.remove(stream.track.name());
		}
		for stream in self.audio.values() {
			catalog.audio.renditions.remove(stream.track.name());
		}
	}
}

/// The multitrack framing common to every track in one tag: the layout and the
/// real `VideoPacketType`/`AudioPacketType`, plus the shared FourCC (present for
/// every layout except `ManyTracksManyCodecs`, where each track carries its own).
struct MultitrackHeader {
	multitrack_type: u8,
	packet_type: u8,
	shared_fourcc: Option<[u8; 4]>,
}

/// One track record inside a multitrack tag.
struct MultitrackTrack<'a> {
	fourcc: [u8; 4],
	track_id: u8,
	payload: &'a [u8],
}

/// Parse the multitrack framing byte and shared FourCC, advancing `cursor` past
/// them. `cursor` starts at the framing byte (after the ex-header byte).
fn split_multitrack_header(cursor: &mut &[u8], kind: &str) -> anyhow::Result<MultitrackHeader> {
	let (&framing, rest) = cursor
		.split_first()
		.with_context(|| format!("multitrack {kind} tag missing framing byte"))?;
	*cursor = rest;
	let multitrack_type = framing >> 4;
	let packet_type = framing & 0x0f;
	anyhow::ensure!(
		matches!(
			multitrack_type,
			MULTITRACK_ONE_TRACK | MULTITRACK_MANY_TRACKS | MULTITRACK_MANY_TRACKS_MANY_CODECS
		),
		"unsupported multitrack {kind} type {multitrack_type}"
	);

	let shared_fourcc = if multitrack_type == MULTITRACK_MANY_TRACKS_MANY_CODECS {
		None
	} else {
		Some(take_fourcc(cursor).with_context(|| format!("multitrack {kind} missing shared FourCC"))?)
	};

	Ok(MultitrackHeader {
		multitrack_type,
		packet_type,
		shared_fourcc,
	})
}

/// Parse one track record from a multitrack tag body, advancing `cursor` past it.
///
/// Reads the per-track FourCC (only for `ManyTracksManyCodecs`), the one-byte
/// track id, and the payload: length-prefixed for `ManyTracks*`, or the rest of
/// the tag for `OneTrack`.
fn split_multitrack_track<'a>(
	cursor: &mut &'a [u8],
	header: &MultitrackHeader,
	kind: &str,
) -> anyhow::Result<MultitrackTrack<'a>> {
	let fourcc = match header.shared_fourcc {
		Some(fourcc) => fourcc,
		None => take_fourcc(cursor).with_context(|| format!("multitrack {kind} missing per-track FourCC"))?,
	};

	let (&track_id, rest) = cursor
		.split_first()
		.with_context(|| format!("multitrack {kind} missing track id"))?;
	*cursor = rest;

	let payload = if header.multitrack_type == MULTITRACK_ONE_TRACK {
		std::mem::take(cursor)
	} else {
		anyhow::ensure!(cursor.len() >= 3, "multitrack {kind} missing track size");
		let size = read_u24(&cursor[0..3]) as usize;
		*cursor = &cursor[3..];
		anyhow::ensure!(
			cursor.len() >= size,
			"multitrack {kind} track size {size} exceeds remaining {}",
			cursor.len()
		);
		let (payload, rest) = cursor.split_at(size);
		*cursor = rest;
		payload
	};

	Ok(MultitrackTrack {
		fourcc,
		track_id,
		payload,
	})
}

/// Read a 4-byte FourCC from the front of `cursor`, advancing past it.
fn take_fourcc(cursor: &mut &[u8]) -> Option<[u8; 4]> {
	let fourcc = cursor.get(0..4)?.try_into().expect("slice is 4 bytes");
	*cursor = &cursor[4..];
	Some(fourcc)
}

/// Build a video config for the `avc1` shape from an `AVCDecoderConfigurationRecord`.
fn config_from_avcc(avcc_bytes: &[u8]) -> anyhow::Result<VideoConfig> {
	let avcc = crate::codec::h264::Avcc::parse(avcc_bytes)?;
	let mut config = VideoConfig::new(H264 {
		profile: avcc.profile,
		constraints: avcc.constraints,
		level: avcc.level,
		inline: false,
	});
	config.description = Some(Bytes::copy_from_slice(avcc_bytes));
	config.coded_width = avcc.coded_width;
	config.coded_height = avcc.coded_height;
	config.container = Container::Legacy;
	Ok(config)
}

/// Build an audio config for AAC from an `AudioSpecificConfig`.
fn config_from_asc(asc_bytes: &[u8]) -> anyhow::Result<AudioConfig> {
	let mut cursor = asc_bytes;
	let cfg = crate::codec::aac::Config::parse(&mut cursor)?;
	let mut config = AudioConfig::new(AAC { profile: cfg.profile }, cfg.sample_rate, cfg.channel_count);
	config.description = Some(Bytes::copy_from_slice(asc_bytes));
	config.container = Container::Legacy;
	Ok(config)
}

/// Build an audio config for Opus from an `OpusHead` (RFC 7845) record.
fn config_from_opus_head(head: &[u8]) -> anyhow::Result<AudioConfig> {
	let mut cursor = head;
	let cfg = crate::codec::opus::Config::parse(&mut cursor)?;
	let mut config = AudioConfig::new(AudioCodec::Opus, cfg.sample_rate, cfg.channel_count);
	config.description = Some(Bytes::copy_from_slice(head));
	config.container = Container::Legacy;
	Ok(config)
}

/// Build an audio config for MP3 from a frame header (config is in band).
fn config_from_mp3(frame: &[u8]) -> anyhow::Result<AudioConfig> {
	let cfg = crate::codec::mp3::Config::parse(frame)?;
	let mut config = AudioConfig::new(AudioCodec::Mp3, cfg.sample_rate, cfg.channel_count);
	config.container = Container::Legacy;
	Ok(config)
}

/// Build an audio config for AC-3 from a sync frame header.
fn config_from_ac3(frame: &[u8]) -> anyhow::Result<AudioConfig> {
	let header = crate::codec::ac3::parse_header(frame)?;
	let mut config = AudioConfig::new(AudioCodec::Ac3, header.sample_rate, header.channel_count);
	config.container = Container::Legacy;
	Ok(config)
}

/// Build an audio config for E-AC-3 from a sync frame header.
fn config_from_eac3(frame: &[u8]) -> anyhow::Result<AudioConfig> {
	let header = crate::codec::eac3::parse_header(frame)?;
	let mut config = AudioConfig::new(AudioCodec::Ec3, header.sample_rate, header.channel_count);
	config.container = Container::Legacy;
	Ok(config)
}
