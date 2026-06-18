//! FLV demuxer.
//!
//! [`Import`] reads an FLV byte stream, splits it into tags, and routes the
//! H.264 (AVC) video and AAC audio onto MoQ tracks. FLV carries codec config
//! out of band: the AVC sequence header is an `AVCDecoderConfigurationRecord`
//! (avcC) and the AAC sequence header is an `AudioSpecificConfig`, both reused
//! verbatim as the catalog `description`. Video access units are already
//! length-prefixed NALU (the avc1 shape) and audio frames are raw AAC, so the
//! sample bytes pass through to the [`Legacy`](crate::catalog::hang::Container)
//! container unchanged.

use bytes::{Buf, Bytes, BytesMut};
use hang::catalog::{AAC, AudioConfig, Container, H264, VideoConfig};

use super::{
	AAC_RAW, AAC_SEQUENCE_HEADER, AUDIO_FORMAT_AAC, AVC_NALU, AVC_SEQUENCE_HEADER, FILE_HEADER_LEN, FRAME_TYPE_KEY,
	PREV_TAG_SIZE_LEN, TAG_AUDIO, TAG_HEADER_LEN, TAG_SCRIPT, TAG_VIDEO, VIDEO_CODEC_AVC, read_i24, read_u24,
};
use moq_net::Timestamp;

use crate::container::Frame;

/// Upper bound on the FLV header's `data_offset`. The header is 9 bytes in
/// practice; this cap stops a crafted offset from forcing unbounded buffering.
const MAX_DATA_OFFSET: usize = 64 * 1024;

/// Demuxes an FLV byte stream into a MoQ broadcast.
///
/// Supports H.264 (CodecID 7) video and AAC (SoundFormat 10) audio, the modern
/// FLV payload produced by RTMP encoders and `ffmpeg -f flv`. Every other codec,
/// plus the enhanced E-RTMP FourCC tags and `onMetaData` script tags, is logged
/// and dropped. A single FLV stream carries at most one video and one audio
/// track; a new sequence header replaces the previous configuration.
pub struct Import {
	broadcast: moq_net::BroadcastProducer,
	catalog: crate::catalog::Producer,

	/// Accumulated unparsed input. Whole tags are drained out; a trailing partial
	/// tag is retained for the next [`decode`](Self::decode) call.
	buffer: BytesMut,
	/// True once the 9-byte FLV file header and its `PreviousTagSize0` have been consumed.
	header_seen: bool,

	video: Option<Stream>,
	audio: Option<Stream>,
}

/// One demuxed track: its producer plus the sequence-header bytes last seen, so a
/// repeated (identical) sequence header is a no-op rather than a track rebuild.
struct Stream {
	track: crate::container::Producer<crate::catalog::hang::Container>,
	description: Bytes,
}

impl Import {
	/// Create a demuxer publishing into `broadcast` with renditions announced on `catalog`.
	pub fn new(broadcast: moq_net::BroadcastProducer, catalog: crate::catalog::Producer) -> Self {
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
		// codec identification; we only speak classic AVC.
		if first & 0x80 != 0 {
			tracing::warn!("enhanced FLV (FourCC) video not supported, dropping");
			return Ok(());
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
			AVC_SEQUENCE_HEADER => self.init_video(data),
			AVC_NALU => {
				let Some(stream) = self.video.as_mut() else {
					tracing::debug!("AVC NALU before sequence header, dropping");
					return Ok(());
				};
				// FLV stores DTS in the tag; PTS is DTS plus the composition offset.
				let pts_ms = (timestamp as i64) + (composition_time as i64);
				anyhow::ensure!(pts_ms >= 0, "negative AVC presentation timestamp");
				// Live FLV can join mid-GOP: a leading delta before the first
				// keyframe has no group to anchor, so the producer reports
				// MissingKeyframe and we drop it rather than abort.
				match stream.track.write(Frame {
					timestamp: Timestamp::from_millis(pts_ms as u64)?,
					duration: None,
					payload: Bytes::copy_from_slice(data),
					keyframe: frame_type == FRAME_TYPE_KEY,
				}) {
					Ok(()) | Err(crate::Error::MissingKeyframe(_)) => Ok(()),
					Err(e) => Err(e.into()),
				}
			}
			// AVCPacketType 2 is "end of sequence"; nothing to emit.
			_ => Ok(()),
		}
	}

	fn handle_audio(&mut self, body: &[u8], timestamp: u64) -> anyhow::Result<()> {
		let Some(&first) = body.first() else {
			return Ok(());
		};
		let sound_format = first >> 4;
		if sound_format != AUDIO_FORMAT_AAC {
			tracing::warn!(sound_format, "unsupported FLV audio format, dropping");
			return Ok(());
		}

		anyhow::ensure!(body.len() >= 2, "AAC audio tag too short");
		let aac_packet_type = body[1];
		let data = &body[2..];

		match aac_packet_type {
			AAC_SEQUENCE_HEADER => self.init_audio(data),
			AAC_RAW => {
				let Some(stream) = self.audio.as_mut() else {
					tracing::debug!("AAC frame before sequence header, dropping");
					return Ok(());
				};
				// Each frame is its own group so the relay can forward it immediately.
				stream.track.write(Frame {
					timestamp: Timestamp::from_millis(timestamp)?,
					duration: None,
					payload: Bytes::copy_from_slice(data),
					keyframe: true,
				})?;
				stream.track.finish_group()?;
				Ok(())
			}
			_ => Ok(()),
		}
	}

	/// Handle an AVC sequence header (an `AVCDecoderConfigurationRecord`). On the
	/// first one, or whenever the bytes change, (re)build the video track.
	fn init_video(&mut self, avcc_bytes: &[u8]) -> anyhow::Result<()> {
		if self.video.as_ref().is_some_and(|s| s.description == avcc_bytes) {
			return Ok(());
		}

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

		let net_track = self.replace_video()?;
		self.catalog
			.lock()
			.video
			.renditions
			.insert(net_track.name().to_string(), config);
		self.video = Some(Stream {
			// Leading deltas before the first keyframe are skipped at the write
			// site (the producer reports MissingKeyframe), so a mid-GOP join works.
			track: crate::container::Producer::new(net_track, crate::catalog::hang::Container::Legacy),
			description: Bytes::copy_from_slice(avcc_bytes),
		});
		Ok(())
	}

	/// Handle an AAC sequence header (an `AudioSpecificConfig`).
	fn init_audio(&mut self, asc_bytes: &[u8]) -> anyhow::Result<()> {
		if self.audio.as_ref().is_some_and(|s| s.description == asc_bytes) {
			return Ok(());
		}

		let mut cursor = asc_bytes;
		let cfg = crate::codec::aac::Config::parse(&mut cursor)?;
		let mut config = AudioConfig::new(AAC { profile: cfg.profile }, cfg.sample_rate, cfg.channel_count);
		config.description = Some(Bytes::copy_from_slice(asc_bytes));
		config.container = Container::Legacy;

		let net_track = self.replace_audio()?;
		self.catalog
			.lock()
			.audio
			.renditions
			.insert(net_track.name().to_string(), config);
		self.audio = Some(Stream {
			track: crate::container::Producer::new(net_track, crate::catalog::hang::Container::Legacy),
			description: Bytes::copy_from_slice(asc_bytes),
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

impl Drop for Import {
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
