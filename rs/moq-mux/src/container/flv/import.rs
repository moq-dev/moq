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

use bytes::{Buf, Bytes, BytesMut};
use hang::catalog::{AAC, AudioCodec, AudioConfig, Container, H264, VideoConfig};

use super::{
	AAC_RAW, AAC_SEQUENCE_HEADER, AUDIO_FORMAT_AAC, AUDIO_FORMAT_EX, AUDIO_FORMAT_MP3, AUDIO_PACKET_CODED_FRAMES,
	AUDIO_PACKET_MULTICHANNEL_CONFIG, AUDIO_PACKET_SEQUENCE_END, AUDIO_PACKET_SEQUENCE_START, AVC_NALU,
	AVC_SEQUENCE_HEADER, FILE_HEADER_LEN, FRAME_TYPE_KEY, PREV_TAG_SIZE_LEN, TAG_AUDIO, TAG_HEADER_LEN, TAG_SCRIPT,
	TAG_VIDEO, VIDEO_CODEC_AVC, VIDEO_EX_HEADER, VIDEO_PACKET_CODED_FRAMES, VIDEO_PACKET_CODED_FRAMES_X,
	VIDEO_PACKET_METADATA, VIDEO_PACKET_SEQUENCE_END, VIDEO_PACKET_SEQUENCE_START, read_i24, read_u24,
};
use moq_net::Timestamp;

use crate::container::Frame;

/// Upper bound on the FLV header's `data_offset`. The header is 9 bytes in
/// practice; this cap stops a crafted offset from forcing unbounded buffering.
const MAX_DATA_OFFSET: usize = 64 * 1024;

/// Demuxes an FLV byte stream into a MoQ broadcast.
///
/// Supports legacy H.264 + AAC and the enhanced-RTMP FourCC codecs (HEVC, AV1,
/// VP9, Opus, AC-3, E-AC-3), the payloads produced by RTMP encoders and
/// `ffmpeg -f flv`. Unsupported codecs, plus `onMetaData` script tags, are logged
/// and dropped. A single FLV stream carries at most one video and one audio
/// track; a new sequence header replaces the previous configuration.
pub struct Import<E: crate::catalog::hang::CatalogExt = ()> {
	broadcast: moq_net::BroadcastProducer,
	catalog: crate::catalog::Producer<E>,

	/// Accumulated unparsed input. Whole tags are drained out; a trailing partial
	/// tag is retained for the next [`decode`](Self::decode) call.
	buffer: BytesMut,
	/// True once the 9-byte FLV file header and its `PreviousTagSize0` have been consumed.
	header_seen: bool,

	video: Option<VideoStream>,
	audio: Option<AudioStream>,
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
			video: None,
			audio: None,
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
			AVC_SEQUENCE_HEADER => self.init_video(config_from_avcc(data)?),
			AVC_NALU => self.write_video(data, timestamp, composition_time, frame_type == FRAME_TYPE_KEY),
			// AVCPacketType 2 is "end of sequence"; nothing to emit.
			_ => Ok(()),
		}
	}

	/// Handle an enhanced-RTMP (FourCC) video tag.
	fn handle_video_enhanced(&mut self, first: u8, body: &[u8], timestamp: u64) -> anyhow::Result<()> {
		let frame_type = (first >> 4) & 0x07;
		let packet_type = first & 0x0f;
		anyhow::ensure!(body.len() >= 5, "enhanced video tag too short for FourCC");
		let fourcc: [u8; 4] = body[1..5].try_into().expect("slice is 4 bytes");
		let payload = &body[5..];
		let keyframe = frame_type == FRAME_TYPE_KEY;

		match packet_type {
			VIDEO_PACKET_SEQUENCE_START => {
				let config = match &fourcc {
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
				self.init_video(config)
			}
			VIDEO_PACKET_CODED_FRAMES | VIDEO_PACKET_CODED_FRAMES_X => {
				// hvc1/avc1 CodedFrames prefix a 3-byte composition time; CodedFramesX
				// and the always-zero-offset av01/vp09 do not.
				let has_cts = packet_type == VIDEO_PACKET_CODED_FRAMES && matches!(&fourcc, b"hvc1" | b"avc1");
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
				if &fourcc == b"vp09" && keyframe {
					match crate::codec::vp9::config_from_keyframe(data) {
						Ok(Some(config)) => self.init_video(config)?,
						Ok(None) => {}
						Err(err) => tracing::warn!(%err, "dropping malformed VP9 key frame"),
					}
				}

				self.write_video(data, timestamp, cts, keyframe)
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
			if self.audio.is_none() {
				self.init_audio(config_from_mp3(frame)?)?;
			}
			return self.write_audio(frame, timestamp);
		}
		if sound_format != AUDIO_FORMAT_AAC {
			tracing::warn!(sound_format, "unsupported FLV audio format, dropping");
			return Ok(());
		}

		anyhow::ensure!(body.len() >= 2, "AAC audio tag too short");
		let aac_packet_type = body[1];
		let data = &body[2..];

		match aac_packet_type {
			AAC_SEQUENCE_HEADER => self.init_audio(config_from_asc(data)?),
			AAC_RAW => self.write_audio(data, timestamp),
			_ => Ok(()),
		}
	}

	/// Handle an enhanced-RTMP (FourCC) audio tag.
	fn handle_audio_enhanced(&mut self, first: u8, body: &[u8], timestamp: u64) -> anyhow::Result<()> {
		let packet_type = first & 0x0f;
		anyhow::ensure!(body.len() >= 5, "enhanced audio tag too short for FourCC");
		let fourcc: [u8; 4] = body[1..5].try_into().expect("slice is 4 bytes");
		let payload = &body[5..];

		match packet_type {
			AUDIO_PACKET_SEQUENCE_START => {
				let config = match &fourcc {
					b"Opus" => config_from_opus_head(payload)?,
					b"mp4a" => config_from_asc(payload)?,
					// MP3 / AC-3 / E-AC-3 are verbatim with no sequence header; they
					// configure from the first frame. Anything else is unsupported.
					other => {
						tracing::warn!(fourcc = ?other, "unsupported enhanced FLV audio codec, dropping");
						return Ok(());
					}
				};
				self.init_audio(config)
			}
			AUDIO_PACKET_CODED_FRAMES => {
				// MP3 / AC-3 / E-AC-3 carry their config in the frame header, so
				// configure from the first frame when no sequence header preceded it.
				if self.audio.is_none() {
					let config = match &fourcc {
						b".mp3" => Some(config_from_mp3(payload)?),
						b"ac-3" => Some(config_from_ac3(payload)?),
						b"ec-3" => Some(config_from_eac3(payload)?),
						_ => None,
					};
					if let Some(config) = config {
						self.init_audio(config)?;
					}
				}
				self.write_audio(payload, timestamp)
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
	fn write_video(&mut self, data: &[u8], dts: u64, composition_time: i32, keyframe: bool) -> anyhow::Result<()> {
		let Some(stream) = self.video.as_mut() else {
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
	fn write_audio(&mut self, data: &[u8], timestamp: u64) -> anyhow::Result<()> {
		let Some(stream) = self.audio.as_mut() else {
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

	/// (Re)build the video track for `config`, unless it matches the current one.
	fn init_video(&mut self, config: VideoConfig) -> anyhow::Result<()> {
		if self.video.as_ref().is_some_and(|s| s.config == config) {
			return Ok(());
		}

		let net_track = self.replace_video()?;
		self.catalog
			.lock()
			.video
			.renditions
			.insert(net_track.name().to_string(), config.clone());
		self.video = Some(VideoStream {
			// Leading deltas before the first keyframe are skipped at the write
			// site (the producer reports MissingKeyframe), so a mid-GOP join works.
			track: crate::container::Producer::new(net_track, crate::catalog::hang::Container::Legacy),
			config,
		});
		Ok(())
	}

	/// (Re)build the audio track for `config`, unless it matches the current one.
	fn init_audio(&mut self, config: AudioConfig) -> anyhow::Result<()> {
		if self.audio.as_ref().is_some_and(|s| s.config == config) {
			return Ok(());
		}

		let net_track = self.replace_audio()?;
		self.catalog
			.lock()
			.audio
			.renditions
			.insert(net_track.name().to_string(), config.clone());
		self.audio = Some(AudioStream {
			track: crate::container::Producer::new(net_track, crate::catalog::hang::Container::Legacy),
			config,
		});
		Ok(())
	}

	/// Drop any existing video track (finishing it and clearing its catalog
	/// rendition) and allocate a fresh one.
	fn replace_video(&mut self) -> anyhow::Result<moq_net::TrackProducer> {
		if let Some(mut old) = self.video.take() {
			old.track.finish()?;
			self.catalog.lock().video.renditions.remove(old.track.name());
		}
		Ok(self.broadcast.unique_track(
			".flv-v",
			moq_net::TrackInfo::default().with_timescale(hang::container::TIMESCALE),
		)?)
	}

	/// Drop any existing audio track (finishing it and clearing its catalog
	/// rendition) and allocate a fresh one.
	fn replace_audio(&mut self) -> anyhow::Result<moq_net::TrackProducer> {
		if let Some(mut old) = self.audio.take() {
			old.track.finish()?;
			self.catalog.lock().audio.renditions.remove(old.track.name());
		}
		Ok(self.broadcast.unique_track(
			".flv-a",
			moq_net::TrackInfo::default().with_timescale(hang::container::TIMESCALE),
		)?)
	}

	/// Close the current group on every track and reopen at `sequence`.
	pub fn seek(&mut self, sequence: u64) -> anyhow::Result<()> {
		if let Some(stream) = self.video.as_mut() {
			stream.track.seek(sequence)?;
		}
		if let Some(stream) = self.audio.as_mut() {
			stream.track.seek(sequence)?;
		}
		Ok(())
	}

	/// Finish every track, flushing the current group.
	pub fn finish(&mut self) -> anyhow::Result<()> {
		if let Some(stream) = self.video.as_mut() {
			stream.track.finish()?;
		}
		if let Some(stream) = self.audio.as_mut() {
			stream.track.finish()?;
		}
		Ok(())
	}
}

impl<E: crate::catalog::hang::CatalogExt> Drop for Import<E> {
	fn drop(&mut self) {
		let mut catalog = self.catalog.lock();
		if let Some(stream) = &self.video {
			catalog.video.renditions.remove(stream.track.name());
		}
		if let Some(stream) = &self.audio {
			catalog.audio.renditions.remove(stream.track.name());
		}
	}
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
