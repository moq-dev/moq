//! Fragmented MP4 (fMP4 / CMAF).
//!
//! A widely supported file format that's also a viable wire format.
//! Each moq frame carries one moof+mdat fragment, optionally with
//! several samples packed inside. [`Wire`] is the wire-level
//! container; [`Import`] parses external fMP4 streams and [`Export`]
//! produces them.

mod export;
mod import;

pub use export::*;
pub use import::*;

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

	fn write(&self, group: &mut moq_net::GroupProducer, frames: &[Frame]) -> std::result::Result<(), Self::Error> {
		let timescale = moq_net::Timescale::new(self.trak.mdia.mdhd.timescale as u64)?;
		let track_id = self.trak.tkhd.track_id;
		encode(group, frames, timescale, track_id)
	}

	fn poll_read(
		&self,
		group: &mut moq_net::GroupConsumer,
		waiter: &kio::Waiter,
	) -> Poll<std::result::Result<Option<Vec<Frame>>, Self::Error>> {
		use std::task::ready;

		let Some(data) = ready!(group.poll_read_frame(waiter)?) else {
			return Poll::Ready(Ok(None));
		};

		let timescale = moq_net::Timescale::new(self.trak.mdia.mdhd.timescale as u64)?;
		Poll::Ready(Ok(Some(decode(data, timescale)?)))
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

	let mut frames = Vec::new();
	let mut offset = 0usize;
	let mut dts = base_dts;

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
			let sample_duration = entry.duration.or(default_duration);
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
		}
	}

	Ok(frames)
}

pub(crate) fn encode(
	group: &mut moq_net::GroupProducer,
	frames: &[Frame],
	timescale: moq_net::Timescale,
	track_id: u32,
) -> Result<()> {
	if frames.is_empty() {
		return Ok(());
	}

	let sequence_number = group.frame_count() as u32;
	let bytes = encode_fragment(track_id, timescale, sequence_number, frames)?;
	let mut writer = group.create_frame(bytes.len())?;
	writer.write(bytes)?;
	writer.finish()?;

	Ok(())
}

/// Encode a single-traf moof+mdat fragment from a sequence of frames.
///
/// Performs the two-pass encoding required by ISO/IEC 14496-12: encode once
/// to learn the moof size, then again with `trun.data_offset` pointing past
/// the moof and mdat header. The DTS of the first frame is computed at the
/// caller-supplied `timescale`.
///
/// Returns an empty `Bytes` when `frames` is empty.
pub(crate) fn encode_fragment(
	track_id: u32,
	timescale: moq_net::Timescale,
	sequence_number: u32,
	frames: &[Frame],
) -> Result<Bytes> {
	use mp4_atom::Encode;

	if frames.is_empty() {
		return Ok(Bytes::new());
	}

	// Re-express the first frame's timestamp at the target track's scale. When the
	// importer preserved the source scale (the common passthrough case), this is a
	// no-op; otherwise it's a single rescale rather than the legacy `micros * scale
	// / 1_000_000` round-trip.
	let dts = frames[0].timestamp.as_scale(timescale) as u64;

	let entries: Vec<_> = frames
		.iter()
		.map(|f| {
			let flags = if f.keyframe { 0x0200_0000 } else { 0x0001_0000 };
			// Write the sample-duration back at the track's scale when we know it, so
			// fMP4 -> fMP4 round-trips it. Frames without one stay byte-identical.
			let duration = f.duration.map(|d| d.as_scale(timescale) as u32);
			mp4_atom::TrunEntry {
				size: Some(f.payload.len() as u32),
				flags: Some(flags),
				duration,
				..Default::default()
			}
		})
		.collect();

	let mdat_data: Vec<u8> = frames.iter().flat_map(|f| f.payload.iter().copied()).collect();

	let build_moof = |data_offset| mp4_atom::Moof {
		mfhd: mp4_atom::Mfhd { sequence_number },
		traf: vec![mp4_atom::Traf {
			tfhd: mp4_atom::Tfhd {
				track_id,
				..Default::default()
			},
			tfdt: Some(mp4_atom::Tfdt {
				base_media_decode_time: dts,
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

	Ok(build_video_trak(track_id, timescale, sample_entry, width, height))
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
		AudioCodec::Opus => mp4_atom::Codec::from(mp4_atom::Opus {
			audio,
			dops: mp4_atom::Dops {
				output_channel_count: config.channel_count as u8,
				pre_skip: 0,
				input_sample_rate: config.sample_rate,
				output_gain: 0,
			},
			btrt: None,
		}),
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

			let bitrate = config.bitrate.unwrap_or(0) as u32;
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
							buffer_size_db: Default::default(),
							max_bitrate: bitrate,
							avg_bitrate: bitrate,
							dec_specific,
						},
						sl_config: Default::default(),
					},
				},
				btrt: None,
				taic: None,
			})
		}
		other => return Err(Error::UnsupportedSynthesis(format!("audio codec {:?}", other))),
	};

	Ok(build_audio_trak(track_id, timescale, sample_entry))
}

fn build_video_trak(
	track_id: u32,
	timescale: u64,
	sample_entry: mp4_atom::Codec,
	width: u16,
	height: u16,
) -> mp4_atom::Trak {
	mp4_atom::Trak {
		tkhd: mp4_atom::Tkhd {
			track_id,
			enabled: true,
			width: mp4_atom::FixedPoint::from(width),
			height: mp4_atom::FixedPoint::from(height),
			..Default::default()
		},
		mdia: build_mdia(timescale, b"vide", true, sample_entry),
		..Default::default()
	}
}

fn build_audio_trak(track_id: u32, timescale: u64, sample_entry: mp4_atom::Codec) -> mp4_atom::Trak {
	mp4_atom::Trak {
		tkhd: mp4_atom::Tkhd {
			track_id,
			enabled: true,
			..Default::default()
		},
		mdia: build_mdia(timescale, b"soun", false, sample_entry),
		..Default::default()
	}
}

fn build_mdia(timescale: u64, handler: &[u8; 4], is_video: bool, sample_entry: mp4_atom::Codec) -> mp4_atom::Mdia {
	mp4_atom::Mdia {
		mdhd: mp4_atom::Mdhd {
			timescale: timescale as u32,
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
pub(crate) fn default_video_timescale(config: &VideoConfig) -> u64 {
	if let Some(fps) = config.framerate {
		(fps * 1000.0) as u64
	} else {
		90000
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn ts(micros: u64) -> Timestamp {
		Timestamp::from_micros(micros).unwrap()
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

		let fragment = encode_fragment(1, timescale, 0, &input).unwrap();
		let frames = decode(fragment, timescale).unwrap();

		assert_eq!(frames.len(), 1);
		assert_eq!(frames[0].duration, Some(ts(33_333)));
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

		let fragment = encode_fragment(1, timescale, 0, &frames).unwrap();
		let frames = decode(fragment, timescale).unwrap();

		assert_eq!(frames.len(), 1);
		assert_eq!(frames[0].duration, None);
	}
}
