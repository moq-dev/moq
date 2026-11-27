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
//! the frames instead, a changed config starting a new track. Sample bytes already match the codec payload, so no codec
//! transform is needed before the selected media container wraps them. FLAC (`fLaC`)
//! enhanced audio, and any other unsupported codec, fail the import.

use std::collections::BTreeMap;

use bytes::{Buf, Bytes, BytesMut};

/// Return [`Malformed`](super::Error::Malformed), formatted, unless `cond` holds.
macro_rules! ensure {
	($cond:expr, $($arg:tt)*) => {
		if !$cond {
			return Err(super::Error::Malformed(format!($($arg)*)).into());
		}
	};
}

use hang::catalog::{AAC, AudioCodec, AudioConfig, VideoConfig};

use super::{
	AAC_RAW, AAC_SEQUENCE_HEADER, AUDIO_FORMAT_AAC, AUDIO_FORMAT_EX, AUDIO_FORMAT_MP3, AUDIO_PACKET_CODED_FRAMES,
	AUDIO_PACKET_MULTICHANNEL_CONFIG, AUDIO_PACKET_MULTITRACK, AUDIO_PACKET_SEQUENCE_END, AUDIO_PACKET_SEQUENCE_START,
	AVC_NALU, AVC_SEQUENCE_HEADER, FILE_HEADER_LEN, FRAME_TYPE_KEY, MULTITRACK_MANY_TRACKS,
	MULTITRACK_MANY_TRACKS_MANY_CODECS, MULTITRACK_ONE_TRACK, PREV_TAG_SIZE_LEN, TAG_AUDIO, TAG_HEADER_LEN, TAG_SCRIPT,
	TAG_VIDEO, VIDEO_CODEC_AVC, VIDEO_EX_HEADER, VIDEO_PACKET_CODED_FRAMES, VIDEO_PACKET_CODED_FRAMES_X,
	VIDEO_PACKET_METADATA, VIDEO_PACKET_MULTITRACK, VIDEO_PACKET_SEQUENCE_END, VIDEO_PACKET_SEQUENCE_START, read_i24,
	read_u24,
};
use moq_net::Timestamp;

use crate::container::Frame;

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
/// `ffmpeg -f flv`. Unsupported codecs fail the import; `onMetaData` script tags are logged
/// and dropped.
///
/// Legacy and single-track enhanced tags carry one video and one audio track;
/// enhanced-RTMP multitrack tags carry several of each, addressed by track id,
/// and each id becomes its own catalog rendition. A new sequence header for a
/// track replaces that track's previous configuration.
pub struct Import<E: crate::catalog::hang::CatalogExt = ()> {
	broadcast: moq_net::broadcast::Producer,
	catalog: crate::catalog::Producer<E>,
	container: hang::catalog::Container,

	/// Held until the first media frame, by which point all sequence headers (hence renditions) have
	/// been declared, so the catalog is withheld until the track set is known (and, when composed with
	/// other importers, until they finish too).
	initial_reservation: Option<crate::catalog::Reserved<E>>,

	/// The stream's timestamp base: the first media frame anchors it, and every frame shifts by its
	/// offset onto the catalog clock.
	timebase: crate::catalog::Timebase<E>,

	/// Accumulated unparsed input. Whole tags are drained out; a trailing partial
	/// tag is retained for the next [`decode`](Self::decode) call.
	buffer: BytesMut,
	/// True once the 9-byte FLV file header and its `PreviousTagSize0` have been consumed.
	header_seen: bool,
	dropped_metadata: u64,

	/// Demuxed video tracks keyed by RTMP track id (legacy / single-track tags use
	/// [`DEFAULT_TRACK_ID`]).
	video: BTreeMap<u8, VideoStream>,
	/// Demuxed audio tracks keyed by RTMP track id.
	audio: BTreeMap<u8, AudioStream>,

	/// Whether input is still accepted: [`finish`](Self::finish) ends it.
	input: crate::container::Input,
}

/// A demuxed video track, holding what it was configured from so a repeated (identical) sequence
/// header is a no-op rather than a track rebuild.
enum VideoStream {
	/// H.264 through its importer, which holds the avcC until the next keyframe says whether the
	/// parameter sets also ride in band.
	H264 {
		import: Box<crate::codec::h264::Import>,
		record: Bytes,
	},
	/// H.265 through its importer, which holds the hvcC the same way.
	H265 {
		import: Box<crate::codec::h265::Import>,
		record: Bytes,
	},
	/// AV1 or VP9, configured from its av1C or keyframe header.
	Plain(Box<PlainVideo>),
}

/// An AV1 or VP9 track plus the config it was built from.
struct PlainVideo {
	track: crate::container::Producer<crate::catalog::hang::Container, VideoConfig>,
	config: VideoConfig,
}

impl VideoStream {
	fn write(&mut self, frame: Frame) -> crate::Result<()> {
		match self {
			Self::H264 { import, .. } => import.decode([frame]),
			Self::H265 { import, .. } => import.decode([frame]),
			Self::Plain(plain) => plain.track.write(frame),
		}
	}

	fn seek(&mut self, start: u64) -> crate::Result<()> {
		match self {
			Self::H264 { import, .. } => import.seek(start),
			Self::H265 { import, .. } => import.seek(start),
			Self::Plain(plain) => plain.track.seek(start),
		}
	}

	/// End the track for a successor or a drop, leaving a record no keyframe followed unpublished.
	fn end(&mut self) -> crate::Result<()> {
		match self {
			Self::H264 { import, .. } => import.end(),
			Self::H265 { import, .. } => import.end(),
			Self::Plain(plain) => plain.track.end(),
		}
	}

	/// End the track as the input ends, publishing a record no keyframe followed as it stands.
	fn finish(&mut self) -> crate::Result<()> {
		match self {
			Self::H264 { import, .. } => import.finish_in_place(),
			Self::H265 { import, .. } => import.finish_in_place(),
			Self::Plain(plain) => plain.track.end(),
		}
	}

	fn abort(self, err: moq_net::Error) {
		match self {
			Self::H264 { import, .. } => import.abort(err),
			Self::H265 { import, .. } => import.abort(err),
			Self::Plain(plain) => plain.track.abort(err),
		}
	}
}

/// The demuxed audio track plus its current catalog config.
struct AudioStream {
	track: crate::container::Producer<crate::catalog::hang::Container, AudioConfig>,
	config: AudioConfig,
	/// Where the open group closes.
	join: crate::container::Join,
	/// The latest frame's timestamp, which a tag repeating it adds nothing to.
	previous: Option<Timestamp>,
	/// Where the open group's audio ends, as its frames' codec states it, or `None` once a frame
	/// states nothing, which leaves the group with no end to declare.
	end: Option<Timestamp>,
}

impl AudioStream {
	/// Close the open group where its audio ends, capped at `cap`, or with no declared end when a
	/// frame's duration is unknown.
	fn close(&mut self, cap: Option<Timestamp>) -> crate::Result<()> {
		if self.track.needs_keyframe() {
			return Ok(());
		}
		match self.end {
			Some(end) => self
				.track
				.cut(Some(cap.map_or(end, |cap| crate::container::earlier(end, cap)))),
			None => self.track.cut_unended(),
		}
	}
}

impl<E: crate::catalog::hang::CatalogExt> Import<E> {
	/// Create a demuxer publishing into `broadcast` with renditions announced on `catalog`.
	pub fn new(broadcast: moq_net::broadcast::Producer, reserved: crate::catalog::Reserved<E>) -> Self {
		let container = hang::catalog::Container::default();
		Self {
			broadcast,
			catalog: reserved.producer(),
			container,
			timebase: reserved.producer().timebase(),
			initial_reservation: Some(reserved),
			buffer: BytesMut::new(),
			header_seen: false,
			dropped_metadata: 0,
			video: BTreeMap::new(),
			audio: BTreeMap::new(),
			input: crate::container::Input::Open,
		}
	}

	/// Return the number of metadata units discarded by this importer.
	#[cfg(test)]
	pub(crate) fn dropped_metadata(&self) -> u64 {
		self.dropped_metadata
	}

	/// Shift this input's timestamps through `timebase`, a timestamp base of the catalog `reserved`
	/// belongs to, instead of a fresh one: importers of one source share its offset.
	///
	/// Set it before the first [`decode`](Self::decode), which may anchor the timebase.
	pub fn with_timebase(mut self, timebase: crate::catalog::Timebase<E>) -> Self {
		self.timebase = timebase;
		self
	}

	/// Select the container this importer wraps decoded media renditions in.
	///
	/// [`Legacy`](hang::catalog::Container::Legacy) unless selected. It applies to every rendition
	/// this input demuxes, since the tracks are discovered rather than named by the caller.
	pub fn with_container(mut self, container: hang::catalog::Container) -> Self {
		self.container = container;
		self
	}

	/// Append `buf` to the internal scratch and demux every whole tag it now
	/// completes. The buffer is fully consumed; a trailing partial tag is retained
	/// for the next call.
	///
	/// A malformed or unsupported tag leaves the importer usable, since tags are framed
	/// independently. A failure to publish, such as a
	/// [`TimestampRewind`](crate::container::TimestampRewind) from a restarted source,
	/// refuses all later input, so a caller skipping bad tags can tell the two apart by
	/// whether the next call is refused.
	///
	/// Input after [`finish`](Self::finish) is refused with [`Finished`](crate::Error::Finished),
	/// or with the cause of a finish that failed.
	pub fn decode(&mut self, data: &[u8]) -> crate::Result<()> {
		self.input.check()?;
		self.buffer.extend_from_slice(data);

		self.drain()
	}

	fn drain(&mut self) -> crate::Result<()> {
		if !self.header_seen {
			if self.buffer.len() < FILE_HEADER_LEN {
				return Ok(());
			}
			ensure!(&self.buffer[0..3] == b"FLV", "not an FLV stream");
			// data_offset is where the body starts (>= 9). It's followed by the
			// 4-byte PreviousTagSize0 before the first tag.
			let data_offset =
				u32::from_be_bytes([self.buffer[5], self.buffer[6], self.buffer[7], self.buffer[8]]) as usize;
			ensure!(data_offset >= FILE_HEADER_LEN, "invalid FLV data offset");
			// The header is tiny in practice (9 bytes). Cap it so a crafted offset
			// can't force unbounded buffering before the first tag is reached.
			ensure!(data_offset <= MAX_DATA_OFFSET, "FLV data offset too large");
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
				TAG_SCRIPT => {
					self.dropped_metadata += 1;
					if self.dropped_metadata == 1 {
						tracing::warn!(count = self.dropped_metadata, "discarding FLV script metadata");
					}
				}
				other => tracing::debug!(tag_type = other, "ignoring unknown FLV tag"),
			}
		}

		Ok(())
	}

	fn handle_video(&mut self, body: &[u8], timestamp: u64) -> crate::Result<()> {
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
			return Err(crate::Error::UnknownFormat(format!("FLV video CodecID {codec_id}")));
		}

		ensure!(body.len() >= 5, "AVC video tag too short");
		let avc_packet_type = body[1];
		let composition_time = read_i24(&body[2..5]);
		let data = &body[5..];

		match avc_packet_type {
			AVC_SEQUENCE_HEADER => self.video_record(DEFAULT_TRACK_ID, b"avc1", data),
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
	fn handle_video_enhanced(&mut self, first: u8, body: &[u8], timestamp: u64) -> crate::Result<()> {
		let keyframe = (first >> 4) & 0x07 == FRAME_TYPE_KEY;
		let packet_type = first & 0x0f;

		if packet_type == VIDEO_PACKET_MULTITRACK {
			return self.handle_video_multitrack(&body[1..], keyframe, timestamp);
		}

		ensure!(body.len() >= 5, "enhanced video tag too short for FourCC");
		let fourcc: [u8; 4] = body[1..5].try_into().expect("slice is 4 bytes");
		self.handle_video_track(DEFAULT_TRACK_ID, &fourcc, packet_type, keyframe, &body[5..], timestamp)
	}

	/// Split a multitrack video tag into its per-track payloads and route each to
	/// [`handle_video_track`](Self::handle_video_track).
	///
	/// `data` starts at the multitrack framing byte (after the ex-header byte).
	fn handle_video_multitrack(&mut self, data: &[u8], keyframe: bool, timestamp: u64) -> crate::Result<()> {
		let mut cursor = data;
		let header = split_multitrack_header(&mut cursor, "video")?;
		ensure!(
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
	) -> crate::Result<()> {
		if !matches!(fourcc, b"avc1" | b"hvc1" | b"av01" | b"vp09") {
			return Err(crate::Error::UnknownFormat(format!("FLV video FourCC {fourcc:?}")));
		}
		match packet_type {
			VIDEO_PACKET_SEQUENCE_START => {
				let config = match fourcc {
					b"avc1" | b"hvc1" => return self.video_record(track_id, fourcc, payload),
					b"av01" => crate::codec::av1::config_from_av1c(payload)?,
					// VP9 carries its config in band; the SequenceStart vpcC is
					// redundant with the key-frame header we configure from.
					b"vp09" => return Ok(()),
					other => {
						return Err(crate::Error::UnknownFormat(format!("FLV video FourCC {other:?}")));
					}
				};
				self.init_video(track_id, config)
			}
			VIDEO_PACKET_CODED_FRAMES | VIDEO_PACKET_CODED_FRAMES_X => {
				// hvc1/avc1 CodedFrames prefix a 3-byte composition time; CodedFramesX
				// and the always-zero-offset av01/vp09 do not.
				let has_cts = packet_type == VIDEO_PACKET_CODED_FRAMES && matches!(fourcc, b"hvc1" | b"avc1");
				let (data, cts) = if has_cts {
					ensure!(payload.len() >= 3, "enhanced CodedFrames missing composition time");
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

	fn handle_audio(&mut self, body: &[u8], timestamp: u64) -> crate::Result<()> {
		let Some(&first) = body.first() else {
			return Ok(());
		};
		let sound_format = first >> 4;
		if sound_format == AUDIO_FORMAT_EX {
			return self.handle_audio_enhanced(first, body, timestamp);
		}
		if sound_format == AUDIO_FORMAT_MP3 {
			// Legacy MP3: the raw frame follows the one-byte tag header, with the
			// config in band. Configure from each frame, which starts a new track only
			// when the header changes, then write it.
			let frame = &body[1..];
			self.init_audio(DEFAULT_TRACK_ID, config_from_mp3(frame)?)?;
			return self.write_audio(DEFAULT_TRACK_ID, frame, timestamp);
		}
		if sound_format != AUDIO_FORMAT_AAC {
			return Err(crate::Error::UnknownFormat(format!(
				"FLV audio SoundFormat {sound_format}"
			)));
		}

		ensure!(body.len() >= 2, "AAC audio tag too short");
		let aac_packet_type = body[1];
		let data = &body[2..];

		match aac_packet_type {
			AAC_SEQUENCE_HEADER => self.init_audio(DEFAULT_TRACK_ID, config_from_asc(data)?),
			AAC_RAW => self.write_audio(DEFAULT_TRACK_ID, data, timestamp),
			_ => Ok(()),
		}
	}

	/// Handle an enhanced-RTMP (FourCC) audio tag, single-track or multitrack.
	fn handle_audio_enhanced(&mut self, first: u8, body: &[u8], timestamp: u64) -> crate::Result<()> {
		let packet_type = first & 0x0f;

		if packet_type == AUDIO_PACKET_MULTITRACK {
			return self.handle_audio_multitrack(&body[1..], timestamp);
		}

		ensure!(body.len() >= 5, "enhanced audio tag too short for FourCC");
		let fourcc: [u8; 4] = body[1..5].try_into().expect("slice is 4 bytes");
		self.handle_audio_track(DEFAULT_TRACK_ID, &fourcc, packet_type, &body[5..], timestamp)
	}

	/// Split a multitrack audio tag into its per-track payloads and route each to
	/// [`handle_audio_track`](Self::handle_audio_track).
	fn handle_audio_multitrack(&mut self, data: &[u8], timestamp: u64) -> crate::Result<()> {
		let mut cursor = data;
		let header = split_multitrack_header(&mut cursor, "audio")?;
		ensure!(
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
	) -> crate::Result<()> {
		if !matches!(fourcc, b"Opus" | b"mp4a" | b".mp3" | b"ac-3" | b"ec-3") {
			return Err(crate::Error::UnknownFormat(format!("FLV audio FourCC {fourcc:?}")));
		}
		match packet_type {
			AUDIO_PACKET_SEQUENCE_START => {
				let config = match fourcc {
					b"Opus" => crate::codec::opus::config(payload)?,
					b"mp4a" => config_from_asc(payload)?,
					// MP3 / AC-3 / E-AC-3 are verbatim with no sequence header; they
					// configure from the first frame. Anything else is unsupported.
					other => {
						return Err(crate::Error::UnknownFormat(format!("FLV audio FourCC {other:?}")));
					}
				};
				self.init_audio(track_id, config)
			}
			AUDIO_PACKET_CODED_FRAMES => {
				// MP3 / AC-3 / E-AC-3 carry their config in the frame header, so configure from
				// each frame, which starts a new track only when the header changes.
				let config = match fourcc {
					b".mp3" => Some(config_from_mp3(payload)?),
					b"ac-3" => Some(config_from_ac3(payload)?),
					b"ec-3" => Some(config_from_eac3(payload)?),
					_ => None,
				};
				if let Some(config) = config {
					self.init_audio(track_id, config)?;
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
	) -> crate::Result<()> {
		// FLV stores DTS in the tag; PTS is DTS plus the composition offset.
		let pts_ms = (dts as i64) + (composition_time as i64);
		if pts_ms < 0 {
			return Err(crate::Error::NegativeFlvPts {
				dts_ms: dts,
				composition_time_ms: composition_time,
			});
		}
		if !self.video.contains_key(&track_id) {
			tracing::debug!("video frame before sequence header, dropping");
			return Ok(());
		}
		// The first frame anchors the stream before the reservation below publishes.
		let shifted = self.timebase.shift(Timestamp::from_millis(pts_ms as u64)?);
		let timestamp = self.refuse(shifted)?;
		// A media frame means every sequence header has arrived (FLV sends config before data), so
		// the track set is declared; release the reservation to publish. An H.264/H.265 rendition
		// still waiting for its first keyframe holds the catalog back on its own.
		self.initial_reservation = None;
		let stream = self.video.get_mut(&track_id).expect("checked above");
		let result = match stream.write(Frame {
			timestamp,
			duration: None,
			payload: Bytes::copy_from_slice(data),
			keyframe,
		}) {
			Err(crate::Error::MissingKeyframe(_)) => Ok(()),
			result => result,
		};
		self.refuse(result)
	}

	/// Write one audio frame, which forwards at once, closing its group once that carries a join.
	///
	/// A tag states no duration, so each frame plays as long as its codec says. A group its stated
	/// durations fill closes with the frame that fills it. A frame a gap or rewind puts outside the
	/// open group closes it first and opens a new one, so the producer checks it as a group start. A
	/// group holding a frame whose codec says nothing closes there with no declared end.
	fn write_audio(&mut self, track_id: u8, data: &[u8], timestamp: u64) -> crate::Result<()> {
		if !self.audio.contains_key(&track_id) {
			tracing::debug!("audio frame before config, dropping");
			return Ok(());
		}
		// The first frame anchors the stream before the reservation below publishes.
		let shifted = self.timebase.shift(Timestamp::from_millis(timestamp)?);
		let timestamp = self.refuse(shifted)?;
		// A media frame means every sequence header has arrived (FLV sends config before data), so
		// the track set is declared; release the reservation to publish.
		self.initial_reservation = None;
		let stream = self.audio.get_mut(&track_id).expect("checked above");
		// A tag at the previous one's timestamp adds no time, so a stalled clock would hold one
		// group open for good. Drop it, as the producer drops a group start that doesn't advance.
		if stream.previous == Some(timestamp) {
			stream.track.report_skipped(1);
			return Ok(());
		}
		let previous = stream.previous.replace(timestamp);
		let stated = stated_duration(&stream.config, data);
		let end = stated.and_then(|duration| after(timestamp, duration));
		// The frame carries its duration only where the track's timescale counts it in whole units,
		// as a CMAF track must; elsewhere the producer measures each frame to the next.
		let duration = stated.filter(|duration| whole(*duration, stream.track.track().info().timescale));
		let result = (|| {
			if !stream.track.needs_keyframe() && stream.join.opens(timestamp) {
				// After a gap the group never ends past this frame, which would move it. A rewind
				// leaves the producer to check this frame against the end.
				stream.close(previous.filter(|previous| timestamp > *previous).map(|_| timestamp))?;
			}
			let keyframe = stream.track.needs_keyframe();
			if keyframe {
				stream.join.reset();
			}
			stream.track.write(Frame {
				timestamp,
				duration,
				payload: Bytes::copy_from_slice(data),
				keyframe,
			})?;
			stream.end = match (keyframe, stream.end, end) {
				(true, _, end) => end,
				(false, Some(reach), Some(end)) => Some(crate::container::later(reach, end)),
				(false, _, _) => None,
			};
			// Only a stated end fills a group: an unstated one counts this frame's start alone.
			if stream.join.fill(timestamp, end.unwrap_or(timestamp)).is_some() {
				stream.close(None)?;
			}
			Ok(())
		})();
		self.refuse(result)
	}

	/// Refuse all later input when publishing failed, so the caller can tell it from a bad tag.
	fn refuse<T>(&mut self, result: crate::Result<T>) -> crate::Result<T> {
		result.inspect_err(|err| self.input = crate::container::Input::Refused(err.clone()))
	}

	/// Hand an H.264 (`avc1`) or H.265 (`hvc1`) sequence header to the track's codec importer,
	/// which holds it until the next keyframe says whether the parameter sets also ride in band.
	/// A header for another codec replaces the track.
	fn video_record(&mut self, track_id: u8, fourcc: &[u8; 4], record: &[u8]) -> crate::Result<()> {
		// Validate before touching the track, so a malformed header leaves it as it was.
		match fourcc {
			b"hvc1" => drop(crate::codec::h265::config_from_hvcc(record)?),
			_ => drop(crate::codec::h264::config_from_avcc(record)?),
		}
		let last = match (self.video.get_mut(&track_id), fourcc) {
			(Some(VideoStream::H264 { import, record: last }), b"avc1") if last.as_ref() != record => {
				import.initialize(record)?;
				last
			}
			(Some(VideoStream::H265 { import, record: last }), b"hvc1") if last.as_ref() != record => {
				import.initialize(record)?;
				last
			}
			// Unchanged, the importer would only re-detect what its last keyframe already settled.
			(Some(VideoStream::H264 { .. }), b"avc1") | (Some(VideoStream::H265 { .. }), b"hvc1") => return Ok(()),
			_ => {
				let result = self.build_record(track_id, fourcc, record);
				return self.refuse(result);
			}
		};
		*last = Bytes::copy_from_slice(record);
		Ok(())
	}

	fn build_record(&mut self, track_id: u8, fourcc: &[u8; 4], record: &[u8]) -> crate::Result<()> {
		let track = self.replace_video(track_id)?;
		// Past the initial reservation, a fresh one still holds an unpublished catalog for this
		// track's first keyframe, and holds nothing once the catalog went out.
		let reserved = match &self.initial_reservation {
			Some(reserved) => reserved.clone(),
			None => self.catalog.reserve(),
		};
		let hint = crate::catalog::VideoHint {
			container: self.container.clone(),
			..Default::default()
		};
		let record = Bytes::copy_from_slice(record);
		let stream = match fourcc {
			b"hvc1" => {
				let mut import = crate::codec::h265::Import::new(track, reserved, hint)?;
				import.initialize(&record)?;
				VideoStream::H265 {
					import: Box::new(import),
					record,
				}
			}
			_ => {
				let mut import = crate::codec::h264::Import::new(track, reserved, hint)?;
				import.initialize(&record)?;
				VideoStream::H264 {
					import: Box::new(import),
					record,
				}
			}
		};
		self.video.insert(track_id, stream);
		Ok(())
	}

	/// (Re)build the AV1/VP9 track `track_id` for `config`, unless it matches the current one.
	fn init_video(&mut self, track_id: u8, config: VideoConfig) -> crate::Result<()> {
		let result = self.build_video(track_id, config);
		self.refuse(result)
	}

	fn build_video(&mut self, track_id: u8, mut config: VideoConfig) -> crate::Result<()> {
		config.container = self.container.clone();
		if let Some(VideoStream::Plain(current)) = self.video.get(&track_id)
			&& current.config == config
		{
			return Ok(());
		}

		let net_track = self.replace_video(track_id)?;
		// Build the wire producer before advertising the rendition. Both steps are fallible (an
		// unsupported container, a colliding timeline track), and a rendition published for a track
		// we then fail to produce would be advertised to consumers but never served.
		let wire = crate::catalog::hang::Container::try_from(&config)?;
		let media = match &self.initial_reservation {
			Some(reserved) => reserved.video(net_track, wire, config.clone())?,
			None => self.catalog.video(net_track, wire, config.clone())?,
		};
		self.video.insert(
			track_id,
			VideoStream::Plain(Box::new(PlainVideo {
				// Leading deltas before the first keyframe are skipped at the write
				// site (the producer reports MissingKeyframe), so a mid-GOP join works.
				track: media,
				config,
			})),
		);
		Ok(())
	}

	/// (Re)build the audio track `track_id` for `config`, unless it matches the current one.
	fn init_audio(&mut self, track_id: u8, config: AudioConfig) -> crate::Result<()> {
		let result = self.build_audio(track_id, config);
		self.refuse(result)
	}

	fn build_audio(&mut self, track_id: u8, mut config: AudioConfig) -> crate::Result<()> {
		config.container = self.container.clone();
		if self.audio.get(&track_id).is_some_and(|s| s.config == config) {
			return Ok(());
		}

		let net_track = self.replace_audio(track_id)?;
		// Build the wire producer before advertising the rendition. Both steps are fallible (an
		// unsupported container, a colliding timeline track), and a rendition published for a track
		// we then fail to produce would be advertised to consumers but never served.
		let wire = crate::catalog::hang::Container::try_from(&config)?;
		let media = match &self.initial_reservation {
			Some(reserved) => reserved.audio(net_track, wire, config.clone())?,
			None => self.catalog.audio(net_track, wire, config.clone())?,
		};
		self.audio.insert(
			track_id,
			AudioStream {
				track: media,
				config,
				join: crate::container::Join::default(),
				previous: None,
				end: None,
			},
		);
		Ok(())
	}

	/// Retire any existing video track `track_id` and allocate a fresh one.
	fn replace_video(&mut self, track_id: u8) -> crate::Result<moq_net::track::Producer> {
		if let Some(mut old) = self.video.remove(&track_id) {
			old.end()?;
		}
		Ok(self
			.broadcast
			.unique_track(".flv-v", self.catalog.track_info(hang::catalog::PRIORITY.video)))
	}

	/// Retire any existing audio track `track_id` and allocate a fresh one.
	fn replace_audio(&mut self, track_id: u8) -> crate::Result<moq_net::track::Producer> {
		if let Some(mut old) = self.audio.remove(&track_id) {
			old.close(None)?;
			old.track.end()?;
		}
		Ok(self
			.broadcast
			.unique_track(".flv-a", self.catalog.track_info(hang::catalog::PRIORITY.audio)))
	}

	/// Close the current group on every track and reopen at `start`.
	pub fn seek(&mut self, start: u64) -> crate::Result<()> {
		self.input.check()?;
		for stream in self.video.values_mut() {
			stream.seek(start)?;
		}
		for stream in self.audio.values_mut() {
			stream.track.seek(start)?;
		}
		Ok(())
	}

	/// Finish every track, flushing the current group, and return the [`Finished`] import.
	///
	/// Every track ends even when one fails to, aborting with the cause, so a failed finish can't
	/// be retried.
	pub fn finish(mut self) -> crate::Result<Finished<E>> {
		self.finish_in_place()?;
		Ok(Finished { import: self })
	}

	fn finish_in_place(&mut self) -> crate::Result<()> {
		match &self.input {
			crate::container::Input::Finished => return Ok(()),
			crate::container::Input::Failed(err) => return Err(err.clone()),
			crate::container::Input::Open | crate::container::Input::Refused(_) => {}
		}
		// No input follows, so the buffer that waited for the rest of a tag is done.
		self.buffer = BytesMut::new();
		// No frame follows to anchor the clock, so publish the declared track set now, with a track
		// whose record no keyframe followed as the record stands.
		self.initial_reservation = None;
		let mut result = Ok(());
		for stream in self.video.values_mut() {
			result = result.and(stream.finish());
		}
		for stream in self.audio.values_mut() {
			// No tag follows, so the open group ends where its audio does, which the producer would
			// otherwise estimate from the spacing of its last frames.
			result = result.and(stream.close(None)).and(stream.track.end());
		}
		match &result {
			// A refusal keeps refusing input with its own cause.
			Ok(()) if matches!(self.input, crate::container::Input::Refused(_)) => {}
			Ok(()) => self.input = crate::container::Input::Finished,
			Err(err) => self.input = crate::container::Input::Failed(err.clone()),
		}
		result
	}

	/// Abort every track with `err`, so consumers see the real cause instead of the
	/// generic [`moq_net::Error::Dropped`] a bare drop surfaces. The counterpart to
	/// [`Self::finish`] for a failed teardown (e.g. the RTMP client disconnected).
	/// Consumes the importer.
	///
	/// Unlike a drop, this leaves the tracks' HLS records to the catalog, whose abort fails them
	/// rather than ending them as a complete recording.
	pub fn abort(mut self, err: moq_net::Error) {
		for stream in std::mem::take(&mut self.video).into_values() {
			stream.abort(err.clone());
		}
		for stream in std::mem::take(&mut self.audio).into_values() {
			stream.track.abort(err.clone());
		}
	}
}

/// An FLV import that [finished](Import::finish): every track has ended, and they can still be
/// read or aborted, but it takes no more input.
///
/// Dropping it retires the tracks and their catalog renditions, as dropping the [`Import`] does.
///
/// ```compile_fail,E0599
/// # fn cannot_decode(mut import: moq_mux::container::flv::Finished) {
/// import.decode(b"").unwrap();
/// # }
/// ```
#[must_use = "dropping it retires the tracks and their catalog renditions"]
pub struct Finished<E: crate::catalog::hang::CatalogExt = ()> {
	import: Import<E>,
}

impl<E: crate::catalog::hang::CatalogExt> Finished<E> {
	/// Abort every track with `err`; see [`Import::abort`]. Readers still draining a track get its
	/// clean end instead once every group below its end finished.
	pub fn abort(self, err: moq_net::Error) {
		self.import.abort(err);
	}
}

impl<E: crate::catalog::hang::CatalogExt> Drop for Import<E> {
	fn drop(&mut self) {
		let (video, audio) = (std::mem::take(&mut self.video), std::mem::take(&mut self.audio));
		let video = video.into_values().map(|mut stream| stream.end());
		let audio = audio.into_values().map(|mut stream| stream.track.end());
		for result in video.chain(audio) {
			if let Err(err) = result {
				tracing::warn!(%err, "failed to end a dropped FLV track");
			}
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
fn split_multitrack_header(cursor: &mut &[u8], kind: &str) -> crate::Result<MultitrackHeader> {
	let (&framing, rest) = cursor
		.split_first()
		.ok_or_else(|| super::Error::Malformed(format!("multitrack {kind} tag missing framing byte")))?;
	*cursor = rest;
	let multitrack_type = framing >> 4;
	let packet_type = framing & 0x0f;
	ensure!(
		matches!(
			multitrack_type,
			MULTITRACK_ONE_TRACK | MULTITRACK_MANY_TRACKS | MULTITRACK_MANY_TRACKS_MANY_CODECS
		),
		"unsupported multitrack {kind} type {multitrack_type}"
	);

	let shared_fourcc = if multitrack_type == MULTITRACK_MANY_TRACKS_MANY_CODECS {
		None
	} else {
		Some(
			take_fourcc(cursor)
				.ok_or_else(|| super::Error::Malformed(format!("multitrack {kind} missing shared FourCC")))?,
		)
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
) -> crate::Result<MultitrackTrack<'a>> {
	let fourcc = match header.shared_fourcc {
		Some(fourcc) => fourcc,
		None => take_fourcc(cursor)
			.ok_or_else(|| super::Error::Malformed(format!("multitrack {kind} missing per-track FourCC")))?,
	};

	let (&track_id, rest) = cursor
		.split_first()
		.ok_or_else(|| super::Error::Malformed(format!("multitrack {kind} missing track id")))?;
	*cursor = rest;

	let payload = if header.multitrack_type == MULTITRACK_ONE_TRACK {
		std::mem::take(cursor)
	} else {
		ensure!(cursor.len() >= 3, "multitrack {kind} missing track size");
		let size = read_u24(&cursor[0..3]) as usize;
		*cursor = &cursor[3..];
		ensure!(
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

/// How long the audio `data` plays, when its codec's header or packet clock says without decoding.
fn stated_duration(config: &AudioConfig, data: &[u8]) -> Option<Timestamp> {
	let header = match &config.codec {
		AudioCodec::Mp3 => return crate::codec::mp3::duration(data),
		AudioCodec::Ac3 => crate::codec::ac3::parse_header(data).ok()?,
		AudioCodec::Ec3 => crate::codec::eac3::parse_header(data).ok()?,
		AudioCodec::AAC(_) => return crate::codec::aac::frame_duration(config.description.as_deref()?),
		_ => return crate::container::legacy::Audio::new(config)?.duration(data),
	};
	Timestamp::from_scale(header.samples, header.sample_rate.into()).ok()
}

/// Where audio from `timestamp` lasting `duration` ends, in microseconds, counting only whole
/// milliseconds of it: FLV stamps tags in whole milliseconds, truncated or rounded, so the next tag
/// never lands below that end.
fn after(timestamp: Timestamp, duration: Timestamp) -> Option<Timestamp> {
	let micros = timestamp
		.as_micros()
		.ok()?
		.checked_add(duration.as_millis().ok()? * 1000)?;
	Timestamp::from_micros(u64::try_from(micros).ok()?).ok()
}

/// Whether `timescale` counts `duration` in whole units.
fn whole(duration: Timestamp, timescale: Option<moq_net::Timescale>) -> bool {
	match (duration.scale(), timescale) {
		(Some(scale), Some(timescale)) => {
			(u128::from(duration.value()) * u128::from(timescale.as_u64())).is_multiple_of(u128::from(scale.as_u64()))
		}
		_ => false,
	}
}

/// Build an audio config for AAC from an `AudioSpecificConfig`.
fn config_from_asc(asc_bytes: &[u8]) -> crate::Result<AudioConfig> {
	let mut cursor = asc_bytes;
	let cfg = crate::codec::aac::Config::parse(&mut cursor)?;
	let mut config = AudioConfig::new(AAC { profile: cfg.profile }, cfg.sample_rate, cfg.channel_count);
	config.description = Some(Bytes::copy_from_slice(asc_bytes));
	Ok(config)
}

/// Build an audio config for MP3 from a frame header (config is in band).
fn config_from_mp3(frame: &[u8]) -> crate::Result<AudioConfig> {
	let cfg = crate::codec::mp3::Config::parse(frame)?;
	Ok(AudioConfig::new(AudioCodec::Mp3, cfg.sample_rate, cfg.channel_count))
}

/// Build an audio config for AC-3 from a sync frame header.
fn config_from_ac3(frame: &[u8]) -> crate::Result<AudioConfig> {
	let header = crate::codec::ac3::parse_header(frame)?;
	Ok(AudioConfig::new(
		AudioCodec::Ac3,
		header.sample_rate,
		header.channel_count,
	))
}

/// Build an audio config for E-AC-3 from a sync frame header.
fn config_from_eac3(frame: &[u8]) -> crate::Result<AudioConfig> {
	let header = crate::codec::eac3::parse_header(frame)?;
	Ok(AudioConfig::new(
		AudioCodec::Ec3,
		header.sample_rate,
		header.channel_count,
	))
}
