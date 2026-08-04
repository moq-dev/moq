//! Fragmented MP4 (fMP4 / CMAF).
//!
//! A widely supported file format that's also a viable wire format.
//! Each moq frame carries one moof+mdat fragment, optionally with
//! several samples packed inside. [`Wire`] is the wire-level
//! container; [`Import`] parses external fMP4 streams and [`Export`]
//! produces them.

mod export;
mod import;
mod muxer;

pub use export::*;
pub use import::*;
pub use muxer::*;

#[cfg(test)]
mod export_test;
#[cfg(test)]
mod import_test;

use std::task::Poll;

use bytes::Bytes;
use hang::catalog::{AudioCodec, AudioConfig, VideoCodec, VideoConfig};
use mp4_atom::Atom;

use moq_net::Timestamp;

use crate::container::{Container, Frame};

#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	#[error("mp4: {0}")]
	Mp4(std::sync::Arc<mp4_atom::Error>),

	#[error("moq: {0}")]
	Moq(#[from] moq_net::Error),

	#[error("flac: {0}")]
	Flac(#[from] crate::codec::flac::Error),

	#[error("opus: {0}")]
	Opus(#[from] crate::codec::opus::Error),

	#[error("missing keyframe: a group must open on a keyframe")]
	MissingKeyframe(#[from] crate::container::MissingKeyframe),

	#[error("timestamp overflow")]
	TimestampOverflow(#[from] moq_net::TimeOverflow),

	#[error("no traf in moof")]
	NoTraf,

	#[error("no tfdt in traf")]
	NoTfdt,

	#[error("PTS overflow")]
	PtsOverflow,

	#[error("missing moof")]
	NoMoof,

	#[error("missing mdat")]
	NoMdat,

	#[error("missing moov")]
	NoMoov,

	#[error("no tracks in moov")]
	NoTracks,

	#[error("multiple tracks in moov, use Trak instead")]
	MultipleTracks,

	#[error("can't synthesize CMAF init for {0}")]
	UnsupportedSynthesis(String),

	#[error("subtitle tracks are not supported")]
	UnsupportedSubtitle,

	#[error("unknown track handler: {0:?}")]
	UnknownTrackHandler([u8; 4]),

	#[error("missing codec")]
	MissingCodec,

	#[error("multiple codecs")]
	MultipleCodecs,

	#[error("unknown codec: {0:?}")]
	UnknownCodec(mp4_atom::FourCC),

	#[error("unsupported codec: {0:?}")]
	UnsupportedCodec(Box<mp4_atom::Codec>),

	#[error("unsupported codec: MPEG2")]
	UnsupportedMpeg2,

	#[error("duplicate moof")]
	DuplicateMoof,

	#[error("missing trun")]
	MissingTrun,

	#[error("missing tfdt")]
	MissingTfdt,

	#[error("video codec {0} needs a description (codec config record) to synthesize a CMAF init")]
	MissingVideoDescription(String),

	#[error("video track {0} missing in catalog")]
	MissingVideoTrack(String),

	#[error("audio track {0} missing in catalog")]
	MissingAudioTrack(String),

	#[error("invalid data offset")]
	InvalidDataOffset,

	#[error("unknown track {0}")]
	UnknownTrack(u32),

	#[error("no keyframe at start of group")]
	NoKeyframe,

	#[error("track sample range {start}..{end} is out of bounds of mdat (len {len})")]
	SampleRangeOutOfBounds { start: usize, end: usize, len: usize },

	#[error("no catalog snapshot")]
	NoCatalogSnapshot,

	#[error("encode_fragment called with no frames")]
	NoFrames,

	#[error("audio codec {0} needs a description (AudioSpecificConfig) to synthesize a CMAF init")]
	MissingAudioDescription(String),

	#[error("multi-sample fragment has a non-final sample with no duration; DTS is unrecoverable")]
	MissingSampleDuration,

	/// A `Cmaf` rendition's init passes through from the catalog at its own scale, so
	/// [`Muxer::with_timescale`] can't move it without desynchronising it from the fragments.
	#[error("a CMAF rendition's timescale comes from its init segment and can't be overridden")]
	TimescaleOverride,

	/// `mdhd.timescale` is a 32-bit field, so a larger scale would reach the init segment
	/// truncated while the fragments kept the full value, putting them on different timelines.
	#[error("timescale {0} does not fit the 32-bit mdhd field")]
	TimescaleTooLarge(u64),

	/// [`Muxer::fragment_at`] gives the decode timeline to the caller, so it refuses to invent a
	/// duration: an inferred one would land in the `trun` without the caller being able to
	/// reproduce it, and the fragments would stop tiling the timeline the caller is authoring.
	#[error("frame {0} has no duration, which Muxer::fragment_at requires on every frame")]
	MissingFrameDuration(usize),
}

impl From<mp4_atom::Error> for Error {
	fn from(err: mp4_atom::Error) -> Self {
		Error::Mp4(std::sync::Arc::new(err))
	}
}

pub type Result<T> = std::result::Result<T, Error>;

/// CMAF container: encodes/decodes a single track's moof+mdat fragments.
///
/// Build from a CMAF init segment with [`Wire::from_init`], or wrap a
/// pre-extracted [`mp4_atom::Trak`] directly with [`Wire::new`].
///
/// The [`mp4_atom::Trak`] is heap-allocated so that embedding `Wire`
/// in other enums (e.g. [`catalog::hang::Container`](crate::catalog::hang::Container))
/// doesn't bloat unrelated variants.
pub struct Wire {
	trak: Box<mp4_atom::Trak>,
}

impl Wire {
	/// Wrap an already-parsed track.
	pub fn new(trak: mp4_atom::Trak) -> Self {
		Self { trak: Box::new(trak) }
	}

	/// Parse a CMAF init segment (ftyp+moov), extracting the single track.
	pub fn from_init(init_data: &[u8]) -> Result<Self> {
		use mp4_atom::DecodeMaybe;

		let mut cursor = std::io::Cursor::new(init_data);
		while let Some(atom) = mp4_atom::Any::decode_maybe(&mut cursor)? {
			if let mp4_atom::Any::Moov(mut moov) = atom {
				return match moov.trak.len() {
					1 => Ok(Self::new(moov.trak.remove(0))),
					0 => Err(Error::NoTracks),
					_ => Err(Error::MultipleTracks),
				};
			}
		}
		Err(Error::NoMoov)
	}

	pub fn trak(&self) -> &mp4_atom::Trak {
		&self.trak
	}
}

impl Container for Wire {
	type Error = Error;

	fn write(&self, group: &mut moq_net::group::Producer, frames: &[Frame]) -> std::result::Result<(), Self::Error> {
		let timescale = moq_net::Timescale::new(self.trak.mdia.mdhd.timescale as u64)?;
		let track_id = self.trak.tkhd.track_id;
		encode(group, frames, timescale, track_id)
	}

	fn poll_read(
		&self,
		group: &mut moq_net::group::Consumer,
		waiter: &kio::Waiter,
	) -> Poll<std::result::Result<Option<Vec<Frame>>, Self::Error>> {
		use std::task::ready;

		let Some(frame) = ready!(group.poll_read_frame(waiter)?) else {
			return Poll::Ready(Ok(None));
		};

		let timescale = moq_net::Timescale::new(self.trak.mdia.mdhd.timescale as u64)?;
		Poll::Ready(Ok(Some(decode(frame.payload, timescale)?)))
	}
}

pub(crate) fn decode(data: Bytes, timescale: moq_net::Timescale) -> Result<Vec<Frame>> {
	use mp4_atom::DecodeMaybe;

	let mut cursor = std::io::Cursor::new(&data);
	let mut moof = None;
	let mut mdat_data = None;

	while let Some(atom) = mp4_atom::Any::decode_maybe(&mut cursor)? {
		match atom {
			mp4_atom::Any::Moof(m) => moof = Some(m),
			mp4_atom::Any::Mdat(m) => mdat_data = Some(m.data),
			_ => {}
		}
	}

	let moof = moof.ok_or(Error::NoMoof)?;
	let mdat_data = mdat_data.ok_or(Error::NoMdat)?;
	let traf = moof.traf.first().ok_or(Error::NoTraf)?;
	let tfdt = traf.tfdt.as_ref().ok_or(Error::NoTfdt)?;
	let base_dts = tfdt.base_media_decode_time;

	let default_size = traf.tfhd.default_sample_size;
	let default_duration = traf.tfhd.default_sample_duration;

	// DTS is reconstructed by accumulating each sample's duration. A non-final sample
	// with no resolvable duration would leave every following sample stuck at the same
	// DTS, silently collapsing their timestamps, so reject that fragment instead.
	let total_samples: usize = traf.trun.iter().map(|t| t.entries.len()).sum();

	let mut frames = Vec::new();
	let mut offset = 0usize;
	let mut dts = base_dts;
	let mut sample_index = 0usize;

	for trun in &traf.trun {
		for entry in &trun.entries {
			let size = entry.size.or(default_size).unwrap_or(0) as usize;
			let end = offset + size;

			if end > mdat_data.len() {
				return Err(Error::SampleRangeOutOfBounds {
					start: offset,
					end,
					len: mdat_data.len(),
				});
			}

			let cts = entry.cts.unwrap_or_default() as i64;
			let pts = dts.checked_add_signed(cts).ok_or(Error::PtsOverflow)?;
			// Preserve the fmp4 track's native scale through the pipeline.
			let timestamp = Timestamp::new(pts, timescale)?;
			let payload = Bytes::copy_from_slice(&mdat_data[offset..end]);
			let flags = entry.flags.unwrap_or(0);
			// depends_on_no_other (bits 24-25 == 0x2) means keyframe
			let keyframe = (flags >> 24) & 0x3 == 0x2;

			// Carry the sample-duration through at the track's scale when present, so
			// the jitter buffer can use it and an exporter can write it back.
			let sample_duration = entry.duration.or(default_duration).filter(|d| *d != 0);

			// The last sample needs no duration (nothing follows it to time), but any
			// earlier sample without one makes the rest of the fragment's DTS ambiguous.
			let is_last = sample_index + 1 == total_samples;
			if sample_duration.is_none() && !is_last {
				return Err(Error::MissingSampleDuration);
			}

			let duration = sample_duration
				.map(|d| Timestamp::new(d as u64, timescale))
				.transpose()?;

			frames.push(Frame {
				timestamp,
				payload,
				keyframe,
				duration,
			});

			offset = end;
			dts += sample_duration.unwrap_or(0) as u64;
			sample_index += 1;
		}
	}

	Ok(frames)
}

pub(crate) fn encode(
	group: &mut moq_net::group::Producer,
	frames: &[Frame],
	timescale: moq_net::Timescale,
	track_id: u32,
) -> Result<()> {
	if frames.is_empty() {
		return Ok(());
	}

	let sequence_number = group.frame_count() as u32;
	let info = FragmentInfo {
		track_id,
		timescale,
		sequence_number,
	};
	let bytes = encode_fragment(info, frames)?;
	// The fragment may carry several samples; the net frame's timestamp is the
	// fragment's earliest presentation time so a relay can order it.
	let mut writer = group.create_frame(moq_net::frame::Info {
		size: bytes.len() as u64,
		timestamp: frames[0].timestamp,
	})?;
	writer.write(bytes)?;
	writer.finish()?;

	Ok(())
}

/// Which track a fragment belongs to, and where it sits in that track.
///
/// Bundled rather than passed positionally because `track_id` and `sequence_number` are both
/// `u32`, so a swapped pair would still compile.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FragmentInfo {
	/// The `tfhd` track id, which must match the one the init segment declares.
	pub track_id: u32,
	/// The track's media timescale, which every timestamp is re-expressed at.
	pub timescale: moq_net::Timescale,
	/// The `mfhd` sequence number, informative only.
	pub sequence_number: u32,
}

/// Encode a single-traf moof+mdat fragment anchored at its own first frame.
///
/// The fragment stands alone: its `tfdt` is `frames[0]`'s presentation time, so it decodes
/// without reference to whatever came before it. A caller that owns a continuous decode
/// timeline across calls wants [`encode_fragment_at`] instead.
///
/// Returns an empty `Bytes` when `frames` is empty.
pub(crate) fn encode_fragment(info: FragmentInfo, frames: &[Frame]) -> Result<Bytes> {
	let Some(base_dts) = frames.first().map(|f| f.timestamp) else {
		return Ok(Bytes::new());
	};
	encode_fragment_at(info, base_dts, frames)
}

/// Encode a single-traf moof+mdat fragment whose decode timeline starts at `base_dts`.
///
/// Performs the two-pass encoding required by ISO/IEC 14496-12: encode once
/// to learn the moof size, then again with `trun.data_offset` pointing past
/// the moof and mdat header. `base_dts` is re-expressed at the track's timescale.
///
/// Frames arrive in decode order carrying presentation timestamps. DTS is authored by
/// accumulating sample durations from `base_dts`, and each sample stores `PTS - DTS` as its
/// signed composition offset, so a reordered frame keeps its presentation time even when the
/// fragment holds a single sample.
///
/// Returns an empty `Bytes` when `frames` is empty.
pub(crate) fn encode_fragment_at(info: FragmentInfo, base_dts: Timestamp, frames: &[Frame]) -> Result<Bytes> {
	let FragmentInfo {
		track_id,
		timescale,
		sequence_number,
	} = info;

	use mp4_atom::Encode;

	if frames.is_empty() {
		return Ok(Bytes::new());
	}

	// Re-express the anchor at the target track's scale. When the importer preserved the
	// source scale (the common passthrough case), this is a no-op; otherwise it's a single
	// rescale rather than the legacy `micros * scale / 1_000_000` round-trip.
	let base_dts = base_dts.as_scale(timescale) as u64;
	let mut dts = base_dts;

	let entries: Vec<_> = frames
		.iter()
		.map(|f| {
			let flags = if f.keyframe { 0x0200_0000 } else { 0x0001_0000 };
			// Write the sample-duration back at the track's scale when we know it, so
			// fMP4 -> fMP4 round-trips it. Frames without one stay byte-identical.
			let duration = f.duration.map(|d| d.as_scale(timescale) as u32);
			let pts = f.timestamp.as_scale(timescale) as i128;
			let cts = pts - i128::from(dts);
			let cts = i32::try_from(cts).map_err(|_| Error::PtsOverflow)?;

			// Frame timestamps are PTS while sample order is decode order. Author DTS
			// by accumulating durations and store PTS-DTS as the signed CTS.
			if let Some(duration) = duration {
				dts = dts.checked_add(u64::from(duration)).ok_or(Error::PtsOverflow)?;
			}

			Ok(mp4_atom::TrunEntry {
				duration,
				size: Some(f.payload.len() as u32),
				flags: Some(flags),
				cts: (cts != 0).then_some(cts),
			})
		})
		.collect::<Result<_>>()?;

	let mdat_data: Vec<u8> = frames.iter().flat_map(|f| f.payload.iter().copied()).collect();

	let build_moof = |data_offset| mp4_atom::Moof {
		mfhd: mp4_atom::Mfhd { sequence_number },
		traf: vec![mp4_atom::Traf {
			tfhd: mp4_atom::Tfhd {
				track_id,
				..Default::default()
			},
			tfdt: Some(mp4_atom::Tfdt {
				base_media_decode_time: base_dts,
			}),
			trun: vec![mp4_atom::Trun {
				data_offset: Some(data_offset),
				entries: entries.clone(),
			}],
			..Default::default()
		}],
	};

	// First pass to learn the moof size.
	let mut buf = Vec::new();
	build_moof(0).encode(&mut buf)?;
	let moof_size = buf.len();

	// Second pass with data_offset = moof_size + 8 (mdat header).
	buf.clear();
	build_moof((moof_size + 8) as i32).encode(&mut buf)?;

	let mdat = mp4_atom::Mdat { data: mdat_data };
	mdat.encode(&mut buf)?;

	Ok(Bytes::from(buf))
}

/// Synthesize a CMAF `Trak` for a video rendition that has no init segment.
///
/// Used by the fMP4 exporter when its source is a `Container::Legacy` track
/// (Avc3/Hev1/etc. importers that publish raw codec bitstreams). H.264/H.265
/// need their out-of-band configuration record (`description`), e.g. because the
/// Avc1 / Hvc1 transform has finished building it from inline parameter sets.
/// VP8 carries no out-of-band config, so `description` is `None` for it.
pub(crate) fn synthesize_video_trak(
	track_id: u32,
	timescale: u64,
	config: &VideoConfig,
	description: Option<&[u8]>,
) -> Result<mp4_atom::Trak> {
	let width = config.coded_width.unwrap_or(0) as u16;
	let height = config.coded_height.unwrap_or(0) as u16;
	let visual = mp4_atom::Visual {
		data_reference_index: 1,
		width,
		height,
		..Default::default()
	};

	// Codecs that carry an out-of-band config record require `description`.
	let require_description = || description.ok_or_else(|| Error::MissingVideoDescription(config.codec.to_string()));

	let sample_entry = match &config.codec {
		VideoCodec::H264(_) => {
			let mut cursor = std::io::Cursor::new(require_description()?);
			let avcc = mp4_atom::Avcc::decode_body(&mut cursor).map_err(Error::from)?;
			mp4_atom::Codec::from(mp4_atom::Avc1 {
				visual,
				avcc,
				..Default::default()
			})
		}
		VideoCodec::H265(h265) => {
			let mut cursor = std::io::Cursor::new(require_description()?);
			let hvcc = mp4_atom::Hvcc::decode_body(&mut cursor).map_err(Error::from)?;
			// `in_band` (catalog) ↔ hev1 sample entry; otherwise hvc1.
			if h265.in_band {
				mp4_atom::Codec::from(mp4_atom::Hev1 {
					visual,
					hvcc,
					..Default::default()
				})
			} else {
				mp4_atom::Codec::from(mp4_atom::Hvc1 {
					visual,
					hvcc,
					..Default::default()
				})
			}
		}
		VideoCodec::AV1(av1) => mp4_atom::Codec::from(mp4_atom::Av01 {
			visual,
			av1c: crate::codec::av1::av1c_from_av1(av1),
			..Default::default()
		}),
		VideoCodec::VP8 => mp4_atom::Codec::from(mp4_atom::Vp08 {
			visual,
			vpcc: crate::codec::vp8::vpcc(),
			..Default::default()
		}),
		VideoCodec::VP9(vp9) => mp4_atom::Codec::from(mp4_atom::Vp09 {
			visual,
			vpcc: crate::codec::vp9::vpcc(vp9),
			..Default::default()
		}),
		other => return Err(Error::UnsupportedSynthesis(format!("video codec {:?}", other))),
	};

	Ok(build_video_trak(
		track_id,
		mdhd_timescale(timescale)?,
		sample_entry,
		width,
		height,
	))
}

/// Synthesize a CMAF `Trak` for an audio rendition that has no init segment.
pub(crate) fn synthesize_audio_trak(track_id: u32, timescale: u64, config: &AudioConfig) -> Result<mp4_atom::Trak> {
	use mp4_atom::Decode;

	let audio = mp4_atom::Audio {
		data_reference_index: 1,
		channel_count: config.channel_count as u16,
		sample_size: 16,
		sample_rate: mp4_atom::FixedPoint::from(config.sample_rate as u16),
	};

	let sample_entry = match &config.codec {
		AudioCodec::Opus => {
			let pre_skip = match &config.description {
				Some(description) => {
					let mut description = description.as_ref();
					crate::codec::opus::Config::parse(&mut description)?.pre_skip
				}
				None => 0,
			};
			mp4_atom::Codec::from(mp4_atom::Opus {
				audio,
				dops: mp4_atom::Dops {
					output_channel_count: config.channel_count as u8,
					pre_skip,
					input_sample_rate: config.sample_rate,
					output_gain: 0,
				},
				btrt: None,
			})
		}
		AudioCodec::AAC(_) => {
			// The catalog `description` is the AudioSpecificConfig (set by the TS
			// importer via aac::Config::encode, or carried over from a CMAF source).
			// mp4_atom models the esds DecoderSpecific as the parsed
			// AudioSpecificConfig, so decode the blob back into that shape.
			let description = config
				.description
				.as_ref()
				.ok_or_else(|| Error::MissingAudioDescription(config.codec.to_string()))?;
			let mut cursor = std::io::Cursor::new(description.as_ref());
			let dec_specific = mp4_atom::esds::DecoderSpecific::decode(&mut cursor)?;

			// Safari and AVFoundation reject an AAC track whose DecoderConfigDescriptor is all
			// zeros: they endOfStream("decode") on the *init* append, which leaves the
			// ManagedMediaSource ended and every later audio and video append failing. The
			// catalog bitrate is optional and real publishers omit it, so fall back to the
			// AAC-LC values ffmpeg and the iTunes encoders write. A zero or wider-than-u32
			// bitrate takes the fallback as well, since writing it back would rebuild the
			// all-zero descriptor this exists to avoid.
			let bitrate = config.bitrate.and_then(|b| u32::try_from(b).ok()).filter(|b| *b > 0);
			let (max_bitrate, avg_bitrate) = match bitrate {
				Some(bitrate) => (bitrate, bitrate),
				None => (256_000, 128_000),
			};
			mp4_atom::Codec::from(mp4_atom::Mp4a {
				audio,
				esds: mp4_atom::Esds {
					es_desc: mp4_atom::esds::EsDescriptor {
						// ISO/IEC 14496-14 §5.6: ES_ID is 0 in an MP4 file (the track id carries identity).
						es_id: 0,
						dec_config: mp4_atom::esds::DecoderConfig {
							object_type_indication: 0x40, // MPEG-4 AAC
							stream_type: 0x05,            // audio
							up_stream: 0,
							// 24 KiB, the decoder buffer those same encoders declare.
							buffer_size_db: mp4_atom::u24::from([0x00, 0x60, 0x00]),
							max_bitrate,
							avg_bitrate,
							dec_specific,
						},
						sl_config: Default::default(),
					},
				},
				btrt: None,
				taic: None,
			})
		}
		AudioCodec::Flac => {
			// The catalog `description` is the FLAC header (`fLaC` marker + STREAMINFO).
			// Parse it back into the STREAMINFO fields the `dfLa` box stores.
			let description = config
				.description
				.as_ref()
				.ok_or_else(|| Error::MissingAudioDescription(config.codec.to_string()))?;
			let info = crate::codec::flac::Config::parse(&mut description.as_ref())?;

			let stream_info = mp4_atom::FlacMetadataBlock::StreamInfo {
				minimum_block_size: info.min_block_size,
				maximum_block_size: info.max_block_size,
				// Frame sizes are 24-bit; clamp defensively so the conversion can't fail.
				minimum_frame_size: info.min_frame_size.min(0xFF_FFFF).try_into().expect("fits in u24"),
				maximum_frame_size: info.max_frame_size.min(0xFF_FFFF).try_into().expect("fits in u24"),
				sample_rate: info.sample_rate,
				num_channels_minus_one: info.channel_count.saturating_sub(1) as u8,
				bits_per_sample_minus_one: info.bits_per_sample.saturating_sub(1) as u8,
				number_of_interchannel_samples: info.total_samples,
				md5_checksum: info.md5.to_vec(),
			};

			mp4_atom::Codec::from(mp4_atom::Flac {
				audio,
				dfla: mp4_atom::Dfla {
					blocks: vec![stream_info],
				},
			})
		}
		other => return Err(Error::UnsupportedSynthesis(format!("audio codec {:?}", other))),
	};

	Ok(build_audio_trak(track_id, mdhd_timescale(timescale)?, sample_entry))
}

/// All-ones: the ISO/IEC 14496-12 spelling of "duration not known up front", which is always
/// the case for the live fragmented streams synthesized here. Zero reads as an empty file to a
/// strict parser, and VLC refuses to play one.
const UNKNOWN_DURATION: u64 = u64::MAX;

fn build_video_trak(
	track_id: u32,
	timescale: u32,
	sample_entry: mp4_atom::Codec,
	width: u16,
	height: u16,
) -> mp4_atom::Trak {
	mp4_atom::Trak {
		tkhd: mp4_atom::Tkhd {
			track_id,
			enabled: true,
			// track_in_movie. A player is entitled to skip a track the presentation doesn't
			// claim, and the flags field is 0x000001 rather than 0x000003 without it.
			in_movie: true,
			duration: UNKNOWN_DURATION,
			width: mp4_atom::FixedPoint::from(width),
			height: mp4_atom::FixedPoint::from(height),
			..Default::default()
		},
		mdia: build_mdia(timescale, b"vide", true, sample_entry),
		..Default::default()
	}
}

fn build_audio_trak(track_id: u32, timescale: u32, sample_entry: mp4_atom::Codec) -> mp4_atom::Trak {
	mp4_atom::Trak {
		tkhd: mp4_atom::Tkhd {
			track_id,
			enabled: true,
			in_movie: true,
			duration: UNKNOWN_DURATION,
			// Full volume (8.8 fixed point). The default is 0, which is a muted track, and
			// only an audio track carries a meaningful value.
			volume: mp4_atom::FixedPoint::from(1),
			..Default::default()
		},
		mdia: build_mdia(timescale, b"soun", false, sample_entry),
		..Default::default()
	}
}

/// Assemble a fragmented init segment (ftyp + moov) around already-built traks.
///
/// `ftyp` is the one a passed-through CMAF init carried, if any; otherwise a plain `isom` one
/// is synthesized. The `mvhd` is ours either way, so it declares the unknown duration and the
/// movie timescale of the assembled init rather than of whatever source a trak came from.
/// [`extract_init`] normalizes a passed-through trak to match.
pub(crate) fn encode_init(
	ftyp: Option<mp4_atom::Ftyp>,
	traks: Vec<mp4_atom::Trak>,
	trexs: Vec<mp4_atom::Trex>,
) -> Result<Bytes> {
	use mp4_atom::Encode;

	let ftyp = ftyp.unwrap_or(mp4_atom::Ftyp {
		major_brand: b"isom".into(),
		minor_version: 0x200,
		compatible_brands: vec![b"isom".into(), b"iso6".into(), b"mp41".into()],
	});
	let timescale = traks.first().map(|t| t.mdia.mdhd.timescale).unwrap_or(1000);
	let next_track_id = traks.iter().map(|t| t.tkhd.track_id).max().unwrap_or(0) + 1;

	let moov = mp4_atom::Moov {
		mvhd: mp4_atom::Mvhd {
			timescale,
			duration: UNKNOWN_DURATION,
			// Normal playback rate and full volume (16.16 and 8.8 fixed point). Both default
			// to 0, which declares a stopped, muted presentation.
			rate: mp4_atom::FixedPoint::from(1),
			volume: mp4_atom::FixedPoint::from(1),
			// The next id a track could take, so it must be past every one already present.
			next_track_id,
			..Default::default()
		},
		trak: traks,
		mvex: (!trexs.is_empty()).then(|| mp4_atom::Mvex {
			trex: trexs,
			..Default::default()
		}),
		..Default::default()
	};

	let mut buf = Vec::new();
	ftyp.encode(&mut buf)?;
	moov.encode(&mut buf)?;
	Ok(Bytes::from(buf))
}

/// Narrow a media timescale to the 32-bit `mdhd` field, rejecting what would truncate.
///
/// `moq_net::Timescale` permits the whole QUIC varint range, so a caller-supplied scale can be
/// wider than the field. Truncating would put the init segment and the fragments on different
/// timelines, silently, so refuse instead.
fn mdhd_timescale(timescale: u64) -> Result<u32> {
	u32::try_from(timescale).map_err(|_| Error::TimescaleTooLarge(timescale))
}

fn build_mdia(timescale: u32, handler: &[u8; 4], is_video: bool, sample_entry: mp4_atom::Codec) -> mp4_atom::Mdia {
	mp4_atom::Mdia {
		mdhd: mp4_atom::Mdhd {
			timescale,
			..Default::default()
		},
		hdlr: mp4_atom::Hdlr {
			handler: mp4_atom::FourCC::new(handler),
			name: String::new(),
		},
		minf: mp4_atom::Minf {
			vmhd: is_video.then(mp4_atom::Vmhd::default),
			smhd: (!is_video).then(mp4_atom::Smhd::default),
			dinf: mp4_atom::Dinf {
				dref: mp4_atom::Dref {
					urls: vec![mp4_atom::Url::default()],
				},
			},
			stbl: mp4_atom::Stbl {
				stsd: mp4_atom::Stsd {
					codecs: vec![sample_entry],
				},
				..Default::default()
			},
			..Default::default()
		},
	}
}

/// Default video timescale when the catalog doesn't supply one.
///
/// Used by the fMP4 exporter when synthesizing an init segment for a
/// Legacy or LOC source: prefer `framerate * 1000` (so each frame has an
/// integer duration), falling back to 90 kHz (the MPEG-TS convention).
///
/// A framerate that doesn't scale to a whole tick takes the fallback too. Zero and negative land
/// on 0 through `as u64`, which is not a timescale anything accepts, and infinity saturates to
/// `u64::MAX`, which is past the varint range a `Timescale` holds.
pub(crate) fn default_video_timescale(config: &VideoConfig) -> u64 {
	let ticks = config
		.framerate
		.filter(|fps| fps.is_finite())
		.map(|fps| (fps * 1000.0) as u64);
	match ticks {
		Some(ticks) if ticks > 0 => ticks,
		_ => 90000,
	}
}

/// The decode timeline a fragment actually carries: its `tfdt` and each sample's composition
/// offset. This is what a player reads, as opposed to the PTS [`decode`] reconstructs from it.
#[cfg(test)]
pub(crate) fn timeline(fragment: &Bytes) -> (u64, Vec<i32>) {
	use mp4_atom::DecodeMaybe;

	let mut cursor = std::io::Cursor::new(fragment.as_ref());
	while let Some(atom) = mp4_atom::Any::decode_maybe(&mut cursor).unwrap() {
		if let mp4_atom::Any::Moof(moof) = atom {
			let traf = &moof.traf[0];
			let cts = traf.trun[0].entries.iter().map(|e| e.cts.unwrap_or_default()).collect();
			return (traf.tfdt.as_ref().unwrap().base_media_decode_time, cts);
		}
	}
	panic!("no moof");
}

/// The sample durations a fragment's `trun` claims, which is what a player advances its own
/// clock by. Distinct from the durations that went in: a missing one is inferred or defaulted.
#[cfg(test)]
pub(crate) fn trun_durations(fragment: &Bytes) -> Vec<Option<u32>> {
	use mp4_atom::DecodeMaybe;

	let mut cursor = std::io::Cursor::new(fragment.as_ref());
	while let Some(atom) = mp4_atom::Any::decode_maybe(&mut cursor).unwrap() {
		if let mp4_atom::Any::Moof(moof) = atom {
			return moof.traf[0].trun[0].entries.iter().map(|e| e.duration).collect();
		}
	}
	panic!("no moof");
}

#[cfg(test)]
mod tests {
	use super::*;

	fn ts(micros: u64) -> Timestamp {
		Timestamp::from_micros(micros).unwrap()
	}

	fn info(track_id: u32, timescale: moq_net::Timescale, sequence_number: u32) -> FragmentInfo {
		FragmentInfo {
			track_id,
			timescale,
			sequence_number,
		}
	}

	// An AAC-LC / 44.1 kHz / stereo AudioSpecificConfig, the catalog `description` shape.
	fn aac_config(bitrate: Option<u64>) -> AudioConfig {
		let mut config = AudioConfig::new(AudioCodec::AAC(hang::catalog::AAC { profile: 2 }), 44_100, 2);
		config.description = Some(Bytes::from_static(&[0x12, 0x10]));
		config.bitrate = bitrate;
		config
	}

	/// The moov an encoded init segment carries, so a test asserts on what a player parses
	/// rather than on the struct that went in.
	fn moov(init: &Bytes) -> mp4_atom::Moov {
		use mp4_atom::DecodeMaybe;

		let mut cursor = std::io::Cursor::new(init.as_ref());
		while let Some(atom) = mp4_atom::Any::decode_maybe(&mut cursor).unwrap() {
			if let mp4_atom::Any::Moov(moov) = atom {
				return moov;
			}
		}
		panic!("no moov");
	}

	fn dec_config(trak: &mp4_atom::Trak) -> mp4_atom::esds::DecoderConfig {
		match &trak.mdia.minf.stbl.stsd.codecs[0] {
			mp4_atom::Codec::Mp4a(mp4a) => mp4a.esds.es_desc.dec_config,
			other => panic!("expected mp4a, got {other:?}"),
		}
	}

	// Safari and AVFoundation reject an all-zero DecoderConfigDescriptor on the *init* append,
	// which ends the ManagedMediaSource and fails every append after it. The catalog bitrate is
	// optional and real publishers omit it, so a synthesized esds must not lean on it.
	#[test]
	fn synthesized_aac_init_has_non_zero_bitrates() {
		let inferred = dec_config(&synthesize_audio_trak(1, 44_100, &aac_config(None)).unwrap());
		assert_ne!(u32::from(inferred.buffer_size_db), 0);
		assert_ne!(inferred.max_bitrate, 0);
		assert_ne!(inferred.avg_bitrate, 0);

		let stated = dec_config(&synthesize_audio_trak(1, 44_100, &aac_config(Some(96_000))).unwrap());
		assert_eq!(stated.max_bitrate, 96_000, "the catalog's bitrate wins");
		assert_eq!(stated.avg_bitrate, 96_000);

		// A stated bitrate the field can't carry rebuilds the same all-zero descriptor, so it
		// takes the fallback rather than the catalog.
		for unusable in [Some(0), Some(u64::from(u32::MAX) + 1)] {
			let config = dec_config(&synthesize_audio_trak(1, 44_100, &aac_config(unusable)).unwrap());
			assert_eq!(config.max_bitrate, inferred.max_bitrate, "{unusable:?}");
			assert_eq!(config.avg_bitrate, inferred.avg_bitrate, "{unusable:?}");
		}
	}

	// `framerate * 1000` is 0 for a zero, negative or non-finite catalog framerate, and 0 is not
	// a timescale: Timescale::new rejects it, so Muxer::video would fail to build at all.
	#[test]
	fn default_video_timescale_ignores_an_unusable_framerate() {
		let mut config = VideoConfig::new(hang::catalog::VideoCodec::VP8);
		for unusable in [0.0, -30.0, f64::NAN, f64::INFINITY, 0.0005] {
			config.framerate = Some(unusable);
			assert_eq!(default_video_timescale(&config), 90_000, "{unusable}");
		}

		config.framerate = Some(30.0);
		assert_eq!(default_video_timescale(&config), 30_000);
	}

	// A live fragmented stream's duration isn't known up front. Zero reads as an empty file to a
	// strict parser: VLC refuses to play one.
	#[test]
	fn synthesized_init_declares_unknown_duration() {
		let trak = synthesize_audio_trak(1, 44_100, &aac_config(None)).unwrap();
		assert_eq!(trak.tkhd.duration, u64::MAX);

		let init = encode_init(None, vec![trak], Vec::new()).unwrap();
		let moov = moov(&init);
		assert_eq!(moov.mvhd.duration, u64::MAX);
		assert_eq!(moov.trak[0].tkhd.duration, u64::MAX);
	}

	// The header defaults are all the "off" value: a track the presentation doesn't claim, a
	// stopped playback rate, a muted volume, and a next id that collides with the track already
	// there. A strict player is entitled to act on any of them.
	#[test]
	fn synthesized_init_headers_describe_a_playable_presentation() {
		let audio = synthesize_audio_trak(1, 44_100, &aac_config(None)).unwrap();
		let init = encode_init(None, vec![audio], Vec::new()).unwrap();
		let moov = moov(&init);

		assert_eq!(moov.mvhd.rate.integer(), 1, "normal playback rate");
		assert_eq!(moov.mvhd.volume.integer(), 1, "full volume");
		assert_eq!(moov.mvhd.next_track_id, 2, "past the only track id");

		let tkhd = &moov.trak[0].tkhd;
		assert!(tkhd.enabled && tkhd.in_movie, "flags 0x000003");
		assert_eq!(tkhd.volume.integer(), 1, "an audio track carries the volume");
	}

	// A video track's volume stays 0: the field only means something for audio.
	#[test]
	fn synthesized_video_init_sets_the_track_flags() {
		let mut config = VideoConfig::new(hang::catalog::VideoCodec::VP8);
		config.framerate = Some(30.0);
		let video = synthesize_video_trak(1, 30_000, &config, None).unwrap();
		let init = encode_init(None, vec![video], Vec::new()).unwrap();
		let moov = moov(&init);

		let tkhd = &moov.trak[0].tkhd;
		assert!(tkhd.enabled && tkhd.in_movie, "flags 0x000003");
		assert_eq!(tkhd.volume.integer(), 0);
	}

	#[test]
	fn decode_reads_trun_sample_duration() {
		use mp4_atom::Encode;

		// Microsecond timescale so each tick maps 1:1 to the Timestamp's µs.
		// decode() walks the mdat by sample size and ignores data_offset, so a
		// hand-built moof+mdat with explicit per-sample durations is enough.
		let timescale = moq_net::Timescale::MICRO;
		let moof = mp4_atom::Moof {
			mfhd: mp4_atom::Mfhd { sequence_number: 0 },
			traf: vec![mp4_atom::Traf {
				tfhd: mp4_atom::Tfhd {
					track_id: 1,
					..Default::default()
				},
				tfdt: Some(mp4_atom::Tfdt {
					base_media_decode_time: 0,
				}),
				trun: vec![mp4_atom::Trun {
					data_offset: Some(0),
					entries: vec![
						mp4_atom::TrunEntry {
							size: Some(2),
							duration: Some(33_333),
							..Default::default()
						},
						mp4_atom::TrunEntry {
							size: Some(2),
							duration: Some(33_333),
							..Default::default()
						},
					],
				}],
				..Default::default()
			}],
		};

		let mut buf = Vec::new();
		moof.encode(&mut buf).unwrap();
		mp4_atom::Mdat {
			data: vec![0xDE, 0xAD, 0xBE, 0xEF],
		}
		.encode(&mut buf)
		.unwrap();

		let frames = decode(Bytes::from(buf), timescale).unwrap();
		assert_eq!(frames.len(), 2);
		assert_eq!(frames[0].timestamp, ts(0));
		assert_eq!(frames[0].duration, Some(ts(33_333)));
		assert_eq!(frames[1].timestamp, ts(33_333));
		assert_eq!(frames[1].duration, Some(ts(33_333)));
	}

	#[test]
	fn duration_round_trips_through_encode() {
		// A frame with a known duration must survive encode -> decode.
		let timescale = moq_net::Timescale::MICRO;
		let input = vec![Frame {
			timestamp: ts(0),
			payload: Bytes::from_static(&[0xDE, 0xAD]),
			keyframe: true,
			duration: Some(ts(33_333)),
		}];

		let fragment = encode_fragment(info(1, timescale, 0), &input).unwrap();
		let frames = decode(fragment, timescale).unwrap();

		assert_eq!(frames.len(), 1);
		assert_eq!(frames[0].duration, Some(ts(33_333)));
	}

	#[test]
	fn reordered_pts_round_trips_with_cts() {
		let timescale = moq_net::Timescale::new(1_000_000).unwrap();
		let input = vec![
			Frame {
				timestamp: ts(0),
				payload: Bytes::from_static(&[0x00]),
				keyframe: true,
				duration: Some(ts(33_000)),
			},
			Frame {
				timestamp: ts(99_000),
				payload: Bytes::from_static(&[0x01]),
				keyframe: false,
				duration: Some(ts(33_000)),
			},
			Frame {
				timestamp: ts(33_000),
				payload: Bytes::from_static(&[0x02]),
				keyframe: false,
				duration: Some(ts(33_000)),
			},
		];

		let fragment = encode_fragment(info(1, timescale, 0), &input).unwrap();
		let frames = decode(fragment, timescale).unwrap();

		assert_eq!(frames.len(), input.len());
		for (actual, expected) in frames.iter().zip(&input) {
			assert_eq!(actual.timestamp, expected.timestamp);
			assert_eq!(actual.duration, expected.duration);
			assert_eq!(actual.payload, expected.payload);
		}
	}

	#[test]
	fn encode_fragment_at_matches_encode_fragment_for_a_self_anchored_call() {
		let timescale = moq_net::Timescale::new(1_000_000).unwrap();
		let input = vec![
			Frame {
				timestamp: ts(5_000),
				payload: Bytes::from_static(&[0x00]),
				keyframe: true,
				duration: Some(ts(33_000)),
			},
			Frame {
				timestamp: ts(71_000),
				payload: Bytes::from_static(&[0x01]),
				keyframe: false,
				duration: Some(ts(33_000)),
			},
		];

		let self_anchored = encode_fragment(info(1, timescale, 3), &input).unwrap();
		let explicit = encode_fragment_at(info(1, timescale, 3), input[0].timestamp, &input).unwrap();
		assert_eq!(self_anchored, explicit);
	}

	#[test]
	fn encode_fragment_at_empty_frames_is_empty_bytes() {
		let timescale = moq_net::Timescale::new(1_000_000).unwrap();
		assert!(
			encode_fragment_at(info(1, timescale, 0), ts(1_000), &[])
				.unwrap()
				.is_empty()
		);
	}

	// One fragment per frame, the shape a fetch-on-demand origin storing one object per encoded
	// frame produces. Anchoring each fragment at its own frame would collapse every cts to zero
	// and let tfdt follow the reordered PTS; a caller-authored base keeps both right.
	#[test]
	fn per_frame_fragments_round_trip_reordered_pts() {
		let timescale = moq_net::Timescale::new(1_000_000).unwrap();
		let input = [
			Frame {
				timestamp: ts(0),
				payload: Bytes::from_static(&[0x00]),
				keyframe: true,
				duration: Some(ts(33_000)),
			},
			Frame {
				timestamp: ts(99_000),
				payload: Bytes::from_static(&[0x01]),
				keyframe: false,
				duration: Some(ts(33_000)),
			},
			Frame {
				timestamp: ts(33_000),
				payload: Bytes::from_static(&[0x02]),
				keyframe: false,
				duration: Some(ts(33_000)),
			},
		];

		// The caller owns the decode timeline: one frame duration per fragment.
		let mut base_dts = ts(0);
		let mut fragments = Vec::new();
		for (sequence, frame) in input.iter().enumerate() {
			let fragment = encode_fragment_at(
				info(1, timescale, sequence as u32),
				base_dts,
				std::slice::from_ref(frame),
			)
			.unwrap();
			base_dts = base_dts.checked_add(frame.duration.unwrap()).unwrap();
			fragments.push(fragment);
		}

		let timelines: Vec<_> = fragments.iter().map(timeline).collect();
		assert_eq!(
			timelines,
			vec![(0, vec![0]), (33_000, vec![66_000]), (66_000, vec![-33_000])],
			"tfdt is the authored DTS and cts carries the reorder"
		);

		for (fragment, expected) in fragments.into_iter().zip(&input) {
			let decoded = decode(fragment, timescale).unwrap();
			assert_eq!(decoded.len(), 1);
			assert_eq!(decoded[0].timestamp, expected.timestamp, "pts = dts + cts");
			assert_eq!(decoded[0].payload, expected.payload);
		}
	}

	#[test]
	fn decode_without_duration_reports_none() {
		// encode_fragment writes no sample-duration for a duration-less frame,
		// so decode must report None (and output stays byte-identical to before).
		let timescale = moq_net::Timescale::new(90_000).unwrap();
		let frames = vec![Frame {
			timestamp: ts(0),
			payload: Bytes::from_static(&[0xDE, 0xAD]),
			keyframe: true,
			duration: None,
		}];

		let fragment = encode_fragment(info(1, timescale, 0), &frames).unwrap();
		let frames = decode(fragment, timescale).unwrap();

		assert_eq!(frames.len(), 1);
		assert_eq!(frames[0].duration, None);
	}

	#[test]
	fn decode_zero_duration_reports_none() {
		use mp4_atom::Encode;

		let timescale = moq_net::Timescale::new(24_000).unwrap();
		let moof = mp4_atom::Moof {
			mfhd: mp4_atom::Mfhd { sequence_number: 0 },
			traf: vec![mp4_atom::Traf {
				tfhd: mp4_atom::Tfhd {
					track_id: 1,
					default_sample_duration: Some(0),
					default_sample_size: Some(2),
					..Default::default()
				},
				tfdt: Some(mp4_atom::Tfdt {
					base_media_decode_time: 2_000,
				}),
				trun: vec![mp4_atom::Trun {
					data_offset: Some(0),
					entries: vec![mp4_atom::TrunEntry {
						size: None,
						duration: None,
						..Default::default()
					}],
				}],
				..Default::default()
			}],
		};

		let mut buf = Vec::new();
		moof.encode(&mut buf).unwrap();
		mp4_atom::Mdat { data: vec![0xDE, 0xAD] }.encode(&mut buf).unwrap();

		let frames = decode(Bytes::from(buf), timescale).unwrap();
		assert_eq!(frames.len(), 1);
		assert_eq!(frames[0].timestamp.as_micros(), 83_333);
		assert_eq!(frames[0].duration, None);
	}
}
