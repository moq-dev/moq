//! Single-codec importers.
//!
//! [`Track`] publishes one MoQ track from whole frames; [`TrackStream`] does the
//! same from a raw byte stream where frame boundaries have to be inferred. Both
//! own exactly one track, so they expose [`Track::demand`] / [`Track::name`]
//! directly rather than fallibly.

use crate::Result;
use crate::catalog::hang::CatalogExt;
use crate::catalog::{AudioHint, VideoHint};

pub use super::Init;

/// The caller-provided video fields for `init`, defaulting the codec from the format when the codec
/// carries no extra parameters (VP8), so a hint with a codec can publish before the first frame.
fn video_hint(init: &Init, default_codec: Option<hang::catalog::VideoCodec>) -> VideoHint {
	let mut hint = init.video.clone().unwrap_or_default();
	if hint.codec.is_none() {
		hint.codec = default_codec;
	}
	hint
}

/// The caller-provided audio fields for `init`, defaulting the codec from the format for the
/// parameter-less codecs (Opus, FLAC, MP3).
fn audio_hint(init: &Init, default_codec: Option<hang::catalog::AudioCodec>) -> AudioHint {
	let mut hint = init.audio.clone().unwrap_or_default();
	if hint.codec.is_none() {
		hint.codec = default_codec;
	}
	hint
}

/// Build an H.264 avc3 split + import pair.
///
/// The import reads `init` for the codec config (or publishes from `hint` up front); the split then
/// reads it as the leading bytes of the stream (caching any inline SPS/PPS). Any frames in the init
/// buffer are published.
fn build_h264_avc3<E: CatalogExt>(
	track: moq_net::track::Producer,
	reserved: crate::catalog::Reserved<E>,
	init: &[u8],
	hint: VideoHint,
) -> Result<(crate::codec::h264::Split, crate::codec::h264::Import<E>)> {
	let mut import = crate::codec::h264::Import::new(track, reserved, hint)?;
	import.initialize(init)?;
	let mut split = crate::codec::h264::Split::new();
	let frames = split.decode(init, None)?;
	import.decode(frames)?;
	Ok((split, import))
}

/// Build an H.264 avc1 import, resolving the config and the NALU length size from
/// the avcC. avc1 has no splitter: each access unit is wrapped directly via
/// [`crate::codec::h264::avc1_frame`].
fn build_h264_avc1<E: CatalogExt>(
	track: moq_net::track::Producer,
	reserved: crate::catalog::Reserved<E>,
	init: &[u8],
	hint: VideoHint,
) -> Result<(usize, crate::codec::h264::Import<E>)> {
	let mut import = crate::codec::h264::Import::new(track, reserved, hint)?;
	import.initialize(init)?;
	let length_size = crate::codec::h264::Avcc::parse(init)?.length_size;
	Ok((length_size, import))
}

/// Build an H.265 split + import pair.
fn build_h265<E: CatalogExt>(
	track: moq_net::track::Producer,
	reserved: crate::catalog::Reserved<E>,
	init: &[u8],
	hint: VideoHint,
) -> Result<(crate::codec::h265::Split, crate::codec::h265::Import<E>)> {
	let mut import = crate::codec::h265::Import::new(track, reserved, hint)?;
	import.initialize(init)?;
	let mut split = crate::codec::h265::Split::new();
	let frames = split.decode(init, None)?;
	import.decode(frames)?;
	Ok((split, import))
}

/// Build an AV1 split + import pair.
fn build_av1<E: CatalogExt>(
	track: moq_net::track::Producer,
	reserved: crate::catalog::Reserved<E>,
	init: &[u8],
	hint: VideoHint,
) -> Result<(crate::codec::av1::Split, crate::codec::av1::Import<E>)> {
	let mut import = crate::codec::av1::Import::new(track, reserved, hint)?;
	import.initialize(init)?;
	let mut split = crate::codec::av1::Split::new();
	// av1C (leading 0x81, ISO/IEC 14496-15) is an out-of-band config record, not an
	// OBU stream, so it's read for config (above) and dropped here. Raw OBUs are the
	// leading bytes of the stream and feed the splitter.
	let frames = if init.len() >= 16 && init[0] == 0x81 {
		Vec::new()
	} else {
		split.decode(init, None)?
	};
	import.decode(frames)?;
	Ok((split, import))
}

enum TrackKind<E: CatalogExt = ()> {
	/// H.264 avc3 (Annex-B, inline SPS/PPS). The split owns byte parsing; the
	/// import publishes.
	Avc3 {
		split: crate::codec::h264::Split,
		import: crate::codec::h264::Import<E>,
	},
	/// H.264 avc1 (length-prefixed NALU, out-of-band avcC). No splitter: each
	/// access unit is wrapped directly. `length_size` is the NALU length prefix
	/// width read from the avcC.
	Avc1 {
		length_size: usize,
		import: crate::codec::h264::Import<E>,
	},
	Hev1 {
		split: crate::codec::h265::Split,
		import: crate::codec::h265::Import<E>,
	},
	Av01 {
		split: crate::codec::av1::Split,
		import: crate::codec::av1::Import<E>,
	},
	Vp8(crate::codec::vp8::Import<E>),
	Vp9(crate::codec::vp9::Import<E>),
	Aac(crate::codec::aac::Import<E>),
	Opus(crate::codec::opus::Import<E>),
	Mp3(crate::codec::mp3::Import<E>),
	Flac(crate::codec::flac::Import<E>),
}

/// A single-codec importer for whole frames.
///
/// Use this when the caller already has whole frames (the typical case for files
/// and reassembled network input). Each [`decode`](Self::decode) call takes one
/// complete frame.
pub struct Track<E: CatalogExt = ()> {
	kind: TrackKind<E>,
}

impl<E: CatalogExt> Track<E> {
	/// Create an importer that publishes a single codec onto a reserved track.
	///
	/// The caller reserves the track (by name) with
	/// [`BroadcastProducer::reserve_track`](moq_net::broadcast::Producer::reserve_track);
	/// the importer accepts it here, which is where the track's timescale is set. The catalog
	/// rendition is registered once the codec config is resolved, or up front from the [`Init`] hints
	/// when they carry enough to publish (see [`AudioHint`] / [`VideoHint`]).
	pub fn new(request: moq_net::track::Request, reserved: crate::catalog::Reserved<E>, init: Init) -> Result<Self> {
		use hang::catalog::{AudioCodec, VideoCodec};

		// Accept at the legacy microsecond timescale, matching the frame timestamps
		// the container stamps. A codec-specific timescale (e.g. the opus sample
		// rate) would be chosen here instead.
		let track = request.accept(moq_net::track::Info::default().with_timescale(hang::container::TIMESCALE));
		let data = init.data.as_ref();
		let kind = match init.format.as_str() {
			"avc1" | "avcc" => {
				let (length_size, import) = build_h264_avc1(track, reserved, data, video_hint(&init, None))?;
				TrackKind::Avc1 { length_size, import }
			}
			"avc3" | "h264" => {
				let (split, import) = build_h264_avc3(track, reserved, data, video_hint(&init, None))?;
				TrackKind::Avc3 { split, import }
			}
			"hev1" => {
				let (split, import) = build_h265(track, reserved, data, video_hint(&init, None))?;
				TrackKind::Hev1 { split, import }
			}
			"av01" | "av1" | "av1c" | "av1C" => {
				let (split, import) = build_av1(track, reserved, data, video_hint(&init, None))?;
				TrackKind::Av01 { split, import }
			}
			"vp8" | "vp08" => {
				let mut import =
					crate::codec::vp8::Import::new(track, reserved, video_hint(&init, Some(VideoCodec::VP8)))?;
				import.initialize(data)?;
				TrackKind::Vp8(import)
			}
			"vp9" | "vp09" => {
				let mut import = crate::codec::vp9::Import::new(track, reserved, video_hint(&init, None))?;
				import.initialize(data)?;
				TrackKind::Vp9(import)
			}
			"aac" => {
				let config = match data.is_empty() {
					true => None,
					false => Some(crate::codec::aac::Config::parse(&mut { data })?),
				};
				let import = crate::codec::aac::Import::new(track, reserved, config, audio_hint(&init, None))?;
				TrackKind::Aac(import)
			}
			"opus" => {
				let config = match data.is_empty() {
					true => None,
					false => Some(crate::codec::opus::Config::parse(&mut { data })?),
				};
				let hint = audio_hint(&init, Some(AudioCodec::Opus));
				let import = crate::codec::opus::Import::new(track, reserved, config, hint)?;
				TrackKind::Opus(import)
			}
			"flac" => {
				// `data` is a FLAC header: the `fLaC` marker plus the STREAMINFO block.
				let config = match data.is_empty() {
					true => None,
					false => Some(crate::codec::flac::Config::parse(&mut { data })?),
				};
				let hint = audio_hint(&init, Some(AudioCodec::Flac));
				let import = crate::codec::flac::Import::new(track, reserved, config, hint)?;
				TrackKind::Flac(import)
			}
			"mp3" => {
				let config = match data.is_empty() {
					true => None,
					false => Some(crate::codec::mp3::Config::parse(data)?),
				};
				let hint = audio_hint(&init, Some(AudioCodec::Mp3));
				let import = crate::codec::mp3::Import::new(track, reserved, config, hint)?;
				TrackKind::Mp3(import)
			}
			_ => return Err(crate::Error::UnknownFormat(init.format)),
		};

		Ok(Self { kind })
	}

	/// Decode one whole frame.
	pub fn decode<B: moq_net::IntoBytes>(&mut self, frame: B, pts: Option<moq_net::Timestamp>) -> Result<()> {
		match self.kind {
			TrackKind::Avc3 {
				ref mut split,
				ref mut import,
			} => {
				// One whole access unit per call, so flush to emit it rather than
				// waiting for the next start code.
				let mut frames = split.decode(frame.as_ref(), pts)?;
				frames.extend(split.flush(pts)?);
				import.decode(frames)?;
			}
			TrackKind::Avc1 {
				length_size,
				ref mut import,
			} => {
				let pts = pts.ok_or(crate::codec::h264::Error::MissingTimestamp)?;
				let frame = crate::codec::h264::avc1_frame(frame, length_size, pts)?;
				import.decode([frame])?;
			}
			TrackKind::Hev1 {
				ref mut split,
				ref mut import,
			} => {
				let mut frames = split.decode(frame.as_ref(), pts)?;
				frames.extend(split.flush(pts)?);
				import.decode(frames)?;
			}
			TrackKind::Av01 {
				ref mut split,
				ref mut import,
			} => {
				let mut frames = split.decode(frame.as_ref(), pts)?;
				frames.extend(split.flush(pts)?);
				import.decode(frames)?;
			}
			TrackKind::Vp8(ref mut import) => import.decode(frame, pts)?,
			TrackKind::Vp9(ref mut import) => import.decode(frame, pts)?,
			TrackKind::Aac(ref mut import) => import.decode(frame, pts)?,
			TrackKind::Opus(ref mut import) => import.decode(frame, pts)?,
			TrackKind::Mp3(ref mut import) => import.decode(frame, pts)?,
			TrackKind::Flac(ref mut import) => import.decode(frame, pts)?,
		}

		Ok(())
	}

	/// Finish the importer, flushing any buffered data.
	pub fn finish(&mut self) -> Result<()> {
		match self.kind {
			TrackKind::Avc3 { ref mut import, .. } => import.finish(),
			TrackKind::Avc1 { ref mut import, .. } => import.finish(),
			TrackKind::Hev1 { ref mut import, .. } => import.finish(),
			TrackKind::Av01 { ref mut import, .. } => import.finish(),
			TrackKind::Vp8(ref mut import) => import.finish(),
			TrackKind::Vp9(ref mut import) => import.finish(),
			TrackKind::Aac(ref mut import) => import.finish(),
			TrackKind::Opus(ref mut import) => import.finish(),
			TrackKind::Mp3(ref mut import) => import.finish(),
			TrackKind::Flac(ref mut import) => import.finish(),
		}
	}

	/// Abort the track with `err` instead of finishing it cleanly, so subscribers
	/// see the real cause rather than [`moq_net::Error::Dropped`].
	pub fn abort(&mut self, err: moq_net::Error) {
		match self.kind {
			TrackKind::Avc3 { ref mut import, .. } => import.abort(err),
			TrackKind::Avc1 { ref mut import, .. } => import.abort(err),
			TrackKind::Hev1 { ref mut import, .. } => import.abort(err),
			TrackKind::Av01 { ref mut import, .. } => import.abort(err),
			TrackKind::Vp8(ref mut import) => import.abort(err),
			TrackKind::Vp9(ref mut import) => import.abort(err),
			TrackKind::Aac(ref mut import) => import.abort(err),
			TrackKind::Opus(ref mut import) => import.abort(err),
			TrackKind::Mp3(ref mut import) => import.abort(err),
			TrackKind::Flac(ref mut import) => import.abort(err),
		}
	}

	/// Close the current group and open the next one at `sequence`.
	pub fn seek(&mut self, sequence: u64) -> Result<()> {
		match self.kind {
			TrackKind::Avc3 {
				ref mut split,
				ref mut import,
			} => {
				split.reset();
				import.seek(sequence)
			}
			TrackKind::Avc1 { ref mut import, .. } => import.seek(sequence),
			TrackKind::Hev1 {
				ref mut split,
				ref mut import,
			} => {
				split.reset();
				import.seek(sequence)
			}
			TrackKind::Av01 {
				ref mut split,
				ref mut import,
			} => {
				split.reset();
				import.seek(sequence)
			}
			TrackKind::Vp8(ref mut import) => import.seek(sequence),
			TrackKind::Vp9(ref mut import) => import.seek(sequence),
			TrackKind::Aac(ref mut import) => import.seek(sequence),
			TrackKind::Opus(ref mut import) => import.seek(sequence),
			TrackKind::Mp3(ref mut import) => import.seek(sequence),
			TrackKind::Flac(ref mut import) => import.seek(sequence),
		}
	}

	/// A watch-only handle to the track's subscriber demand.
	pub fn demand(&self) -> moq_net::track::Demand {
		match self.kind {
			TrackKind::Avc3 { ref import, .. } => import.demand(),
			TrackKind::Avc1 { ref import, .. } => import.demand(),
			TrackKind::Hev1 { ref import, .. } => import.demand(),
			TrackKind::Av01 { ref import, .. } => import.demand(),
			TrackKind::Vp8(ref import) => import.demand(),
			TrackKind::Vp9(ref import) => import.demand(),
			TrackKind::Aac(ref import) => import.demand(),
			TrackKind::Opus(ref import) => import.demand(),
			TrackKind::Mp3(ref import) => import.demand(),
			TrackKind::Flac(ref import) => import.demand(),
		}
	}

	/// The name of the track this importer publishes.
	pub fn name(&self) -> String {
		self.demand().name().to_string()
	}
}

// Lift an already-built opus importer into a `Track` so callers that build their
// config out-of-band (e.g. moq-gst, which constructs `opus::Config` from gstreamer
// caps instead of an OpusHead buffer) can keep using `.into()`.
impl<E: CatalogExt> From<crate::codec::opus::Import<E>> for Track<E> {
	fn from(opus: crate::codec::opus::Import<E>) -> Self {
		Self {
			kind: TrackKind::Opus(opus),
		}
	}
}

impl<E: CatalogExt> From<crate::codec::aac::Import<E>> for Track<E> {
	fn from(aac: crate::codec::aac::Import<E>) -> Self {
		Self {
			kind: TrackKind::Aac(aac),
		}
	}
}

// Lift an already-built mp3 importer into a `Track` so callers that build their
// config out-of-band (e.g. moq-gst, which reads rate/channels from gstreamer caps
// rather than parsing a frame header) can keep using `.into()`.
impl<E: CatalogExt> From<crate::codec::mp3::Import<E>> for Track<E> {
	fn from(mp3: crate::codec::mp3::Import<E>) -> Self {
		Self {
			kind: TrackKind::Mp3(mp3),
		}
	}
}

enum TrackStreamKind<E: CatalogExt = ()> {
	/// H.264 in avc3 wire shape (Annex-B with inline SPS/PPS). The split owns
	/// byte parsing; the import publishes.
	Avc3 {
		split: crate::codec::h264::Split,
		import: crate::codec::h264::Import<E>,
	},
	Hev1 {
		split: crate::codec::h265::Split,
		import: crate::codec::h265::Import<E>,
	},
	Av01 {
		split: crate::codec::av1::Split,
		import: crate::codec::av1::Import<E>,
	},
}

/// A single-codec importer for a raw byte stream with unknown frame boundaries.
///
/// Use this when the caller does not know the frame boundaries (piped Annex-B
/// H.264, an fMP4 reader, …); the importer infers them.
pub struct TrackStream<E: CatalogExt = ()> {
	kind: TrackStreamKind<E>,
}

impl<E: CatalogExt> TrackStream<E> {
	/// Create an importer that publishes a single codec onto a reserved track.
	///
	/// The caller reserves the track with
	/// [`BroadcastProducer::reserve_track`](moq_net::broadcast::Producer::reserve_track);
	/// the importer accepts it here at the legacy microsecond timescale (where a codec-specific
	/// timescale would be chosen). A [`VideoHint`] carrying a codec publishes the catalog before the
	/// first frame; any [`Init::data`] seeds the stream (as a call to [`initialize`](Self::initialize)).
	pub fn new(request: moq_net::track::Request, reserved: crate::catalog::Reserved<E>, init: Init) -> Result<Self> {
		let track = request.accept(moq_net::track::Info::default().with_timescale(hang::container::TIMESCALE));
		let hint = video_hint(&init, None);
		// Only the self-delimiting codecs can be recovered from a raw byte stream.
		let kind = match init.format.as_str() {
			"avc3" | "h264" => TrackStreamKind::Avc3 {
				split: crate::codec::h264::Split::new(),
				import: crate::codec::h264::Import::new(track, reserved, hint)?,
			},
			"hev1" => TrackStreamKind::Hev1 {
				split: crate::codec::h265::Split::new(),
				import: crate::codec::h265::Import::new(track, reserved, hint)?,
			},
			"av01" | "av1" | "av1c" | "av1C" => TrackStreamKind::Av01 {
				split: crate::codec::av1::Split::new(),
				import: crate::codec::av1::Import::new(track, reserved, hint)?,
			},
			_ => return Err(crate::Error::UnknownFormat(init.format)),
		};

		let mut stream = Self { kind };
		if !init.data.is_empty() {
			stream.initialize(&init.data)?;
		}
		Ok(stream)
	}

	/// Initialize the importer with the given buffer and populate the broadcast.
	///
	/// This is not required for self-describing formats like AVC3.
	pub fn initialize(&mut self, data: &[u8]) -> Result<()> {
		match self.kind {
			TrackStreamKind::Avc3 {
				ref mut split,
				ref mut import,
			} => {
				import.initialize(data)?;
				let frames = split.decode(data, None)?;
				import.decode(frames)?;
			}
			TrackStreamKind::Hev1 {
				ref mut split,
				ref mut import,
			} => {
				import.initialize(data)?;
				let frames = split.decode(data, None)?;
				import.decode(frames)?;
			}
			TrackStreamKind::Av01 {
				ref mut split,
				ref mut import,
			} => {
				import.initialize(data)?;
				// av1C (leading 0x81) is an out-of-band config record, not an OBU
				// stream; read for config above and dropped here.
				let frames = if data.len() >= 16 && data[0] == 0x81 {
					Vec::new()
				} else {
					split.decode(data, None)?
				};
				import.decode(frames)?;
			}
		}

		Ok(())
	}

	/// Decode a chunk of the byte stream.
	pub fn decode(&mut self, data: &[u8]) -> Result<()> {
		match self.kind {
			TrackStreamKind::Avc3 {
				ref mut split,
				ref mut import,
			} => {
				let frames = split.decode(data, None)?;
				import.decode(frames)
			}
			TrackStreamKind::Hev1 {
				ref mut split,
				ref mut import,
			} => {
				let frames = split.decode(data, None)?;
				import.decode(frames)
			}
			TrackStreamKind::Av01 {
				ref mut split,
				ref mut import,
			} => {
				let frames = split.decode(data, None)?;
				import.decode(frames)
			}
		}
	}

	/// Finish the importer, flushing any buffered data.
	pub fn finish(&mut self) -> Result<()> {
		match self.kind {
			TrackStreamKind::Avc3 {
				ref mut split,
				ref mut import,
			} => {
				let tail = split.flush(None)?;
				import.decode(tail)?;
				import.finish()
			}
			TrackStreamKind::Hev1 {
				ref mut split,
				ref mut import,
			} => {
				let tail = split.flush(None)?;
				import.decode(tail)?;
				import.finish()
			}
			TrackStreamKind::Av01 {
				ref mut split,
				ref mut import,
			} => {
				let tail = split.flush(None)?;
				import.decode(tail)?;
				import.finish()
			}
		}
	}

	/// Abort the track with `err` instead of finishing it cleanly, so subscribers
	/// see the real cause rather than [`moq_net::Error::Dropped`].
	pub fn abort(&mut self, err: moq_net::Error) {
		match self.kind {
			TrackStreamKind::Avc3 { ref mut import, .. } => import.abort(err),
			TrackStreamKind::Hev1 { ref mut import, .. } => import.abort(err),
			TrackStreamKind::Av01 { ref mut import, .. } => import.abort(err),
		}
	}

	/// Close the current group and open the next one at `sequence`.
	pub fn seek(&mut self, sequence: u64) -> Result<()> {
		match self.kind {
			TrackStreamKind::Avc3 {
				ref mut split,
				ref mut import,
			} => {
				split.reset();
				import.seek(sequence)
			}
			TrackStreamKind::Hev1 {
				ref mut split,
				ref mut import,
			} => {
				split.reset();
				import.seek(sequence)
			}
			TrackStreamKind::Av01 {
				ref mut split,
				ref mut import,
			} => {
				split.reset();
				import.seek(sequence)
			}
		}
	}

	/// A watch-only handle to the track's subscriber demand.
	pub fn demand(&self) -> moq_net::track::Demand {
		match self.kind {
			TrackStreamKind::Avc3 { ref import, .. } => import.demand(),
			TrackStreamKind::Hev1 { ref import, .. } => import.demand(),
			TrackStreamKind::Av01 { ref import, .. } => import.demand(),
		}
	}

	/// The name of the track this importer publishes.
	pub fn name(&self) -> String {
		self.demand().name().to_string()
	}
}

#[cfg(test)]
mod tests {
	use std::time::Duration;

	use super::*;
	use moq_net::Timestamp;

	fn opus_head() -> Vec<u8> {
		let mut head = Vec::with_capacity(19);
		head.extend_from_slice(b"OpusHead");
		head.push(1);
		head.push(2);
		head.extend_from_slice(&0u16.to_le_bytes());
		head.extend_from_slice(&48000u32.to_le_bytes());
		head.extend_from_slice(&0u16.to_le_bytes());
		head.push(0);
		head
	}

	fn h264_init() -> Vec<u8> {
		let mut init = Vec::new();
		init.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
		init.extend_from_slice(&[
			0x67, 0x64, 0x00, 0x1f, 0xac, 0x24, 0x84, 0x01, 0x40, 0x16, 0xec, 0x04, 0x40, 0x00, 0x00, 0x03, 0x00, 0x40,
			0x00, 0x00, 0x0c, 0x23, 0xc6, 0x0c, 0x92,
		]);
		init.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
		init.extend_from_slice(&[0x68, 0xee, 0x32, 0xc8, 0xb0]);
		init
	}

	fn new_broadcast() -> (moq_net::broadcast::Producer, crate::catalog::Producer) {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
		(broadcast, catalog)
	}

	#[tokio::test(start_paused = true)]
	async fn existing_track_opus_uses_existing_name() {
		let (mut broadcast, catalog) = new_broadcast();
		// The importer accepts the reserved track, setting its (microsecond) timescale.
		let request = broadcast.reserve_track("requested-audio").unwrap();
		let mut import = Track::new(request, catalog.reserve(), Init::new("opus", opus_head())).unwrap();

		assert_eq!(import.name(), "requested-audio");
		let snapshot = catalog.snapshot();
		assert!(snapshot.audio.renditions.contains_key("requested-audio"));
		assert!(!snapshot.audio.renditions.contains_key("0.opus"));

		// Frame delivery and the accepted timescale are covered by `opus_import_delivers_frames`.
		import
			.decode(b"opus payload", Some(Timestamp::from_micros(1_000).unwrap()))
			.unwrap();
		import.finish().unwrap();
	}

	#[tokio::test(start_paused = true)]
	async fn aac_import_attaches_audio_specific_config() {
		let (mut broadcast, catalog) = new_broadcast();
		let config = crate::codec::aac::Config {
			profile: 2,
			sample_rate: 44_100,
			channel_count: 2,
		};
		let init = config.encode();
		let request = broadcast.reserve_track("audio").unwrap();

		let import = Track::new(request, catalog.reserve(), Init::new("aac", init.clone())).unwrap();

		assert_eq!(import.name(), "audio");
		let snapshot = catalog.snapshot();
		let audio = snapshot.audio.renditions.get("audio").unwrap();
		assert_eq!(audio.codec.to_string(), "mp4a.40.2");
		assert_eq!(audio.sample_rate, config.sample_rate);
		assert_eq!(audio.channel_count, config.channel_count);
		assert_eq!(audio.description.as_deref(), Some(init.as_ref()));
	}

	#[tokio::test(start_paused = true)]
	async fn unique_track_opus_attaches_catalog_and_retires_on_drop() {
		let (mut broadcast, catalog) = new_broadcast();

		// A freshly reserved track attaches its catalog rendition on init.
		let name = broadcast.unique_name(".opus");
		let request = broadcast.reserve_track(name).unwrap();
		let mut import = Track::new(request, catalog.reserve(), Init::new("opus", opus_head())).unwrap();

		assert_eq!(import.name(), "0.opus");
		assert!(catalog.snapshot().audio.renditions.contains_key("0.opus"));

		import
			.decode(b"opus payload", Some(Timestamp::from_micros(2_000).unwrap()))
			.unwrap();
		import.finish().unwrap();

		// Dropping the importer retires its rendition from the shared catalog.
		drop(import);
		assert!(!catalog.snapshot().audio.renditions.contains_key("0.opus"));
	}

	#[tokio::test(start_paused = true)]
	async fn opus_import_delivers_frames() {
		let (mut broadcast, catalog) = new_broadcast();
		let track = broadcast
			.create_track(
				"audio",
				moq_net::track::Info::default().with_timescale(hang::container::TIMESCALE),
			)
			.unwrap();
		let subscriber = track.subscribe(None);

		let config = crate::codec::opus::Config {
			sample_rate: 48_000,
			channel_count: 2,
		};
		let mut import =
			crate::codec::opus::Import::new(track, catalog.reserve(), Some(config), Default::default()).unwrap();
		assert!(catalog.snapshot().audio.renditions.contains_key("audio"));

		let mut media = crate::container::Consumer::new(subscriber, crate::catalog::hang::Container::Legacy);

		let payload = b"opus payload".to_vec();
		import
			.decode(&payload, Some(Timestamp::from_micros(1_000).unwrap()))
			.unwrap();

		let frame = tokio::time::timeout(Duration::from_secs(1), media.read())
			.await
			.unwrap()
			.unwrap()
			.unwrap();
		assert_eq!(frame.payload, payload);
		assert_eq!(frame.timestamp, Timestamp::from_micros(1_000).unwrap());

		import.finish().unwrap();
	}

	#[tokio::test(start_paused = true)]
	async fn existing_track_h264_uses_existing_name_in_catalog() {
		let (mut broadcast, catalog) = new_broadcast();
		let request = broadcast.reserve_track("camera").unwrap();

		let import = Track::new(request, catalog.reserve(), Init::new("avc3", h264_init())).unwrap();

		assert_eq!(import.name(), "camera");
		let snapshot = catalog.snapshot();
		let video = snapshot.video.renditions.get("camera").unwrap();
		assert_eq!(video.coded_width, Some(1280));
		assert_eq!(video.coded_height, Some(720));
		assert!(!snapshot.video.renditions.contains_key("0.avc3"));
	}

	/// A changed key frame just updates the rendition in place; there are no fixed
	/// tracks to reject a reconfiguration, so the second key frame succeeds.
	#[tokio::test(start_paused = true)]
	async fn reconfiguration_updates_in_place() {
		let (mut broadcast, catalog) = new_broadcast();
		let request = broadcast.reserve_track("video").unwrap();
		let mut import = Track::new(request, catalog.reserve(), Init::new("vp8", Vec::new())).unwrap();

		import
			.decode(
				&[0x10, 0x00, 0x00, 0x9d, 0x01, 0x2a, 0x40, 0x01, 0xf0, 0x00],
				Some(Timestamp::from_micros(0).unwrap()),
			)
			.unwrap();

		import
			.decode(
				&[0x10, 0x00, 0x00, 0x9d, 0x01, 0x2a, 0x80, 0x02, 0xe0, 0x01],
				Some(Timestamp::from_micros(33_000).unwrap()),
			)
			.unwrap();
	}

	/// A hint with the sample rate and channel count publishes the audio catalog before any frame,
	/// even with no init bytes (the codec comes from the format).
	#[tokio::test(start_paused = true)]
	async fn audio_hint_publishes_before_first_frame() {
		let (mut broadcast, catalog) = new_broadcast();
		let request = broadcast.reserve_track("audio").unwrap();
		let hint = crate::catalog::AudioHint {
			sample_rate: Some(48_000),
			channel_count: Some(2),
			bitrate: Some(96_000),
			..Default::default()
		};
		let _import = Track::new(
			request,
			catalog.reserve(),
			Init::new("opus", Vec::new()).with_audio(hint),
		)
		.unwrap();

		let audio = catalog.snapshot().audio.renditions.get("audio").cloned().unwrap();
		assert_eq!(audio.codec.to_string(), "opus");
		assert_eq!(audio.sample_rate, 48_000);
		assert_eq!(audio.channel_count, 2);
		assert_eq!(audio.bitrate, Some(96_000));
	}

	/// A hint carries a field the stream can't reveal (bitrate) through onto the detected config.
	#[tokio::test(start_paused = true)]
	async fn audio_hint_bitrate_survives_detection() {
		let (mut broadcast, catalog) = new_broadcast();
		let request = broadcast.reserve_track("audio").unwrap();
		let hint = crate::catalog::AudioHint {
			bitrate: Some(64_000),
			..Default::default()
		};
		let _import = Track::new(
			request,
			catalog.reserve(),
			Init::new("opus", opus_head()).with_audio(hint),
		)
		.unwrap();

		let audio = catalog.snapshot().audio.renditions.get("audio").cloned().unwrap();
		assert_eq!(audio.sample_rate, 48_000, "detected from the OpusHead");
		assert_eq!(audio.bitrate, Some(64_000), "carried from the hint");
	}

	/// A hint that contradicts what the stream says is an error, not silent drift.
	#[tokio::test(start_paused = true)]
	async fn audio_hint_mismatch_is_rejected() {
		let (mut broadcast, catalog) = new_broadcast();
		let request = broadcast.reserve_track("audio").unwrap();
		let hint = crate::catalog::AudioHint {
			sample_rate: Some(44_100), // the OpusHead says 48000
			..Default::default()
		};
		let result = Track::new(
			request,
			catalog.reserve(),
			Init::new("opus", opus_head()).with_audio(hint),
		);
		match result {
			Err(crate::Error::InitMismatch { .. }) => {}
			Err(err) => panic!("expected InitMismatch, got {err:?}"),
			Ok(_) => panic!("expected InitMismatch, got Ok"),
		}
	}

	/// A video codec with no extra parameters (VP8) publishes the catalog before the first key frame.
	#[tokio::test(start_paused = true)]
	async fn video_publishes_before_first_frame() {
		let (mut broadcast, catalog) = new_broadcast();
		let request = broadcast.reserve_track("video").unwrap();
		let _import = Track::new(request, catalog.reserve(), Init::new("vp8", Vec::new())).unwrap();

		let video = catalog.snapshot().video.renditions.get("video").cloned().unwrap();
		assert_eq!(video.codec.to_string(), "vp8");
	}
}
