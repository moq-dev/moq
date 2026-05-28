//! Format dispatchers for callers who only have a format string.
//!
//! [`Framed`] is the entry point when the caller already has whole
//! frames (the typical case for files and reassembled network input).
//! [`Stream`] is for raw byte streams where frame boundaries have to
//! be inferred (piped Annex-B H.264, an fMP4 reader, …). Both pick a
//! concrete importer from a [`FramedFormat`] / [`StreamFormat`] string.
//! The concrete importers themselves live with their format under
//! [`crate::container`] or [`crate::codec`].

use std::{fmt, str::FromStr};

use bytes::Buf;

use crate::Result;

/// The supported framed formats (known frame boundaries).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum FramedFormat {
	/// H264 with AVCC framing (length-prefixed NALUs, out-of-band SPS/PPS).
	Avc1,
	/// H264 with Annex B framing (start code prefixed, inline SPS/PPS).
	Avc3,
	/// fMP4/CMAF container.
	Fmp4,
	/// aka H265 with inline SPS/PPS
	Hev1,
	/// AV1 with inline sequence headers
	Av01,
	/// Raw AAC frames (not ADTS).
	Aac,
	/// Raw Opus frames (not Ogg).
	Opus,
	/// Matroska / WebM container.
	Mkv,
}

impl FromStr for FramedFormat {
	type Err = crate::Error;

	fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
		match s {
			"avc1" | "avcc" => Ok(FramedFormat::Avc1),
			"avc3" | "h264" => Ok(FramedFormat::Avc3),
			"hev1" => Ok(FramedFormat::Hev1),
			"fmp4" | "cmaf" => Ok(FramedFormat::Fmp4),
			"av01" | "av1" | "av1c" | "av1C" => Ok(FramedFormat::Av01),
			"aac" => Ok(FramedFormat::Aac),
			"opus" => Ok(FramedFormat::Opus),
			"mkv" | "webm" | "matroska" => Ok(FramedFormat::Mkv),
			_ => Err(crate::Error::UnknownFormat(s.to_string())),
		}
	}
}

impl fmt::Display for FramedFormat {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match *self {
			FramedFormat::Avc1 => write!(f, "avc1"),
			FramedFormat::Avc3 => write!(f, "avc3"),
			FramedFormat::Fmp4 => write!(f, "fmp4"),
			FramedFormat::Hev1 => write!(f, "hev1"),
			FramedFormat::Av01 => write!(f, "av01"),
			FramedFormat::Aac => write!(f, "aac"),
			FramedFormat::Opus => write!(f, "opus"),
			FramedFormat::Mkv => write!(f, "mkv"),
		}
	}
}

impl From<StreamFormat> for FramedFormat {
	fn from(format: StreamFormat) -> Self {
		match format {
			StreamFormat::Avc3 => FramedFormat::Avc3,
			StreamFormat::Fmp4 => FramedFormat::Fmp4,
			StreamFormat::Hev1 => FramedFormat::Hev1,
			StreamFormat::Av01 => FramedFormat::Av01,
			StreamFormat::Mkv => FramedFormat::Mkv,
		}
	}
}

enum FramedKind {
	/// H.264 (both avc1 and avc3 wire shapes go through this importer; mode
	/// is pinned by the caller's FramedFormat choice).
	H264(crate::codec::h264::Import),
	// Boxed because it's a large struct and clippy complains about the size.
	Fmp4(Box<crate::container::fmp4::Import>),
	Hev1(crate::codec::h265::Import),
	Av01(crate::codec::av1::Import),
	Aac(crate::codec::aac::Import),
	Opus(crate::codec::opus::Import),
	// Boxed for the same reason as Fmp4.
	Mkv(Box<crate::container::mkv::Import>),
}

/// An importer for formats with known frame boundaries.
///
/// This supports all formats and should be used when the caller knows the frame boundaries.
pub struct Framed {
	decoder: FramedKind,
}

impl Framed {
	/// Create a new framed importer with the given format and initialization data.
	///
	/// The buffer will be fully consumed, or an error will be returned.
	pub fn new<T: Buf + AsRef<[u8]>>(
		broadcast: moq_net::BroadcastProducer,
		catalog: crate::catalog::hang::Producer,
		format: FramedFormat,
		buf: &mut T,
	) -> Result<Self> {
		use crate::codec::h264::Mode as H264Mode;
		let decoder = match format {
			FramedFormat::Avc1 => {
				let mut decoder = crate::codec::h264::Import::new(broadcast, catalog).with_mode(H264Mode::Avc1)?;
				decoder.initialize(buf)?;
				FramedKind::H264(decoder)
			}
			FramedFormat::Avc3 => {
				let mut decoder = crate::codec::h264::Import::new(broadcast, catalog).with_mode(H264Mode::Avc3)?;
				decoder.initialize(buf)?;
				FramedKind::H264(decoder)
			}
			FramedFormat::Fmp4 => {
				let mut decoder = Box::new(crate::container::fmp4::Import::new(broadcast, catalog));
				decoder.decode(buf)?;
				FramedKind::Fmp4(decoder)
			}
			FramedFormat::Hev1 => {
				let mut decoder = crate::codec::h265::Import::new(broadcast, catalog);
				decoder.initialize(buf)?;
				FramedKind::Hev1(decoder)
			}
			FramedFormat::Av01 => {
				let mut decoder = crate::codec::av1::Import::new(broadcast, catalog);
				decoder.initialize(buf)?;
				FramedKind::Av01(decoder)
			}
			FramedFormat::Aac => {
				let config = crate::codec::aac::Config::parse(buf)?;
				FramedKind::Aac(crate::codec::aac::Import::new(broadcast, catalog, config)?)
			}
			FramedFormat::Opus => {
				let config = crate::codec::opus::Config::parse(buf)?;
				FramedKind::Opus(crate::codec::opus::Import::new(broadcast, catalog, config)?)
			}
			FramedFormat::Mkv => {
				let mut decoder = Box::new(crate::container::mkv::Import::new(broadcast, catalog));
				decoder.decode(buf)?;
				FramedKind::Mkv(decoder)
			}
		};

		if buf.has_remaining() {
			return Err(crate::Error::BufferNotConsumed);
		}

		Ok(Self { decoder })
	}

	/// Finish the decoder, flushing any buffered data.
	pub fn finish(&mut self) -> Result<()> {
		match self.decoder {
			FramedKind::H264(ref mut decoder) => decoder.finish(),
			FramedKind::Fmp4(ref mut decoder) => decoder.finish(),
			FramedKind::Hev1(ref mut decoder) => decoder.finish(),
			FramedKind::Av01(ref mut decoder) => decoder.finish(),
			FramedKind::Aac(ref mut decoder) => decoder.finish(),
			FramedKind::Opus(ref mut decoder) => decoder.finish(),
			FramedKind::Mkv(ref mut decoder) => decoder.finish(),
		}
	}

	/// Close the current group and open the next one at `sequence`.
	pub fn seek(&mut self, sequence: u64) -> Result<()> {
		match self.decoder {
			FramedKind::H264(ref mut decoder) => decoder.seek(sequence),
			FramedKind::Fmp4(ref mut decoder) => decoder.seek(sequence),
			FramedKind::Hev1(ref mut decoder) => decoder.seek(sequence),
			FramedKind::Av01(ref mut decoder) => decoder.seek(sequence),
			FramedKind::Aac(ref mut decoder) => decoder.seek(sequence),
			FramedKind::Opus(ref mut decoder) => decoder.seek(sequence),
			FramedKind::Mkv(ref mut decoder) => decoder.seek(sequence),
		}
	}

	/// Return the single track produced by this importer.
	pub fn track(&self) -> Result<&moq_net::TrackProducer> {
		match self.decoder {
			FramedKind::H264(ref decoder) => decoder
				.track()
				.ok_or_else(|| crate::codec::h264::Error::Avc3TrackNotCreated.into()),
			FramedKind::Fmp4(_) => Err(crate::Error::MultipleTracks("fmp4")),
			FramedKind::Hev1(ref decoder) => decoder.track(),
			FramedKind::Av01(ref decoder) => decoder.track(),
			FramedKind::Aac(ref decoder) => Ok(decoder.track()),
			FramedKind::Opus(ref decoder) => Ok(decoder.track()),
			FramedKind::Mkv(_) => Err(crate::Error::MultipleTracks("mkv")),
		}
	}

	/// Decode a frame from the given buffer.
	pub fn decode_frame<T: Buf + AsRef<[u8]>>(&mut self, buf: &mut T, pts: Option<moq_net::Timestamp>) -> Result<()> {
		match self.decoder {
			FramedKind::H264(ref mut decoder) => decoder.decode_frame(buf, pts)?,
			FramedKind::Fmp4(ref mut decoder) => decoder.decode(buf)?,
			FramedKind::Hev1(ref mut decoder) => decoder.decode_frame(buf, pts)?,
			FramedKind::Av01(ref mut decoder) => decoder.decode_frame(buf, pts)?,
			FramedKind::Aac(ref mut decoder) => decoder.decode(buf, pts)?,
			FramedKind::Opus(ref mut decoder) => decoder.decode(buf, pts)?,
			FramedKind::Mkv(ref mut decoder) => {
				let _ = pts;
				decoder.decode(buf)?;
			}
		}

		if buf.has_remaining() {
			return Err(crate::Error::BufferNotConsumed);
		}

		Ok(())
	}
}

// Lift an already-built codec importer into a `Framed` so callers that build
// their config out-of-band (e.g. moq-gst, which constructs `opus::Config` from
// gstreamer caps instead of an OpusHead buffer) can keep using `.into()`.
impl From<crate::codec::opus::Import> for Framed {
	fn from(opus: crate::codec::opus::Import) -> Self {
		Self {
			decoder: FramedKind::Opus(opus),
		}
	}
}

impl From<crate::codec::aac::Import> for Framed {
	fn from(aac: crate::codec::aac::Import) -> Self {
		Self {
			decoder: FramedKind::Aac(aac),
		}
	}
}

// -- stream dispatcher --

/// Formats that support stream decoding (unknown frame boundaries).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum StreamFormat {
	/// aka H264 with inline SPS/PPS
	Avc3,
	/// fMP4/CMAF container.
	Fmp4,
	/// aka H265 with inline SPS/PPS
	Hev1,
	/// AV1 with inline sequence headers
	Av01,
	/// Matroska / WebM container.
	Mkv,
}

impl FromStr for StreamFormat {
	type Err = crate::Error;

	fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
		match s {
			"avc3" | "h264" => Ok(StreamFormat::Avc3),
			"hev1" => Ok(StreamFormat::Hev1),
			"fmp4" | "cmaf" => Ok(StreamFormat::Fmp4),
			"av01" | "av1" | "av1c" | "av1C" => Ok(StreamFormat::Av01),
			"mkv" | "webm" | "matroska" => Ok(StreamFormat::Mkv),
			_ => Err(crate::Error::UnknownFormat(s.to_string())),
		}
	}
}

impl fmt::Display for StreamFormat {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match *self {
			StreamFormat::Avc3 => write!(f, "avc3"),
			StreamFormat::Fmp4 => write!(f, "fmp4"),
			StreamFormat::Hev1 => write!(f, "hev1"),
			StreamFormat::Av01 => write!(f, "av01"),
			StreamFormat::Mkv => write!(f, "mkv"),
		}
	}
}

enum StreamKind {
	/// H.264 in avc3 wire shape (Annex-B with inline SPS/PPS).
	Avc3(crate::codec::h264::Import),
	// Boxed because it's a large struct and clippy complains about the size.
	Fmp4(Box<crate::container::fmp4::Import>),
	Hev1(crate::codec::h265::Import),
	Av01(crate::codec::av1::Import),
	// Boxed for the same reason as Fmp4.
	Mkv(Box<crate::container::mkv::Import>),
}

/// An importer for formats that support stream decoding (unknown frame boundaries).
///
/// This includes formats like H.264 (AVC3), H.265 (HEV1), and fMP4/CMAF.
/// Use this when the caller does not know the frame boundaries.
pub struct Stream {
	decoder: StreamKind,
}

impl Stream {
	/// Create a new stream importer with the given format.
	pub fn new(
		broadcast: moq_net::BroadcastProducer,
		catalog: crate::catalog::hang::Producer,
		format: StreamFormat,
	) -> Result<Self> {
		use crate::codec::h264::Mode as H264Mode;
		let decoder = match format {
			StreamFormat::Avc3 => {
				StreamKind::Avc3(crate::codec::h264::Import::new(broadcast, catalog).with_mode(H264Mode::Avc3)?)
			}
			StreamFormat::Fmp4 => StreamKind::Fmp4(Box::new(crate::container::fmp4::Import::new(broadcast, catalog))),
			StreamFormat::Hev1 => StreamKind::Hev1(crate::codec::h265::Import::new(broadcast, catalog)),
			StreamFormat::Av01 => StreamKind::Av01(crate::codec::av1::Import::new(broadcast, catalog)),
			StreamFormat::Mkv => StreamKind::Mkv(Box::new(crate::container::mkv::Import::new(broadcast, catalog))),
		};

		Ok(Self { decoder })
	}

	/// Initialize the decoder with the given buffer and populate the broadcast.
	///
	/// This is not required for self-describing formats like fMP4 or AVC3.
	///
	/// The buffer will be fully consumed, or an error will be returned.
	pub fn initialize<T: Buf + AsRef<[u8]>>(&mut self, buf: &mut T) -> Result<()> {
		match self.decoder {
			StreamKind::Avc3(ref mut decoder) => decoder.initialize(buf)?,
			StreamKind::Fmp4(ref mut decoder) => decoder.decode(buf)?,
			StreamKind::Hev1(ref mut decoder) => decoder.initialize(buf)?,
			StreamKind::Av01(ref mut decoder) => decoder.initialize(buf)?,
			StreamKind::Mkv(ref mut decoder) => decoder.decode(buf)?,
		}

		if buf.has_remaining() {
			return Err(crate::Error::BufferNotConsumed);
		}

		Ok(())
	}

	/// Decode a stream of data from the given buffer.
	pub fn decode_stream<T: Buf + AsRef<[u8]>>(&mut self, buf: &mut T) -> Result<()> {
		match self.decoder {
			StreamKind::Avc3(ref mut decoder) => decoder.decode_stream(buf, None),
			StreamKind::Fmp4(ref mut decoder) => decoder.decode(buf),
			StreamKind::Hev1(ref mut decoder) => decoder.decode_stream(buf, None),
			StreamKind::Av01(ref mut decoder) => decoder.decode_stream(buf, None),
			StreamKind::Mkv(ref mut decoder) => decoder.decode(buf),
		}
	}

	/// Finish the decoder, flushing any buffered data.
	pub fn finish(&mut self) -> Result<()> {
		match self.decoder {
			StreamKind::Avc3(ref mut decoder) => decoder.finish(),
			StreamKind::Fmp4(ref mut decoder) => decoder.finish(),
			StreamKind::Hev1(ref mut decoder) => decoder.finish(),
			StreamKind::Av01(ref mut decoder) => decoder.finish(),
			StreamKind::Mkv(ref mut decoder) => decoder.finish(),
		}
	}

	/// Close the current group and open the next one at `sequence`.
	pub fn seek(&mut self, sequence: u64) -> Result<()> {
		match self.decoder {
			StreamKind::Avc3(ref mut decoder) => decoder.seek(sequence),
			StreamKind::Fmp4(ref mut decoder) => decoder.seek(sequence),
			StreamKind::Hev1(ref mut decoder) => decoder.seek(sequence),
			StreamKind::Av01(ref mut decoder) => decoder.seek(sequence),
			StreamKind::Mkv(ref mut decoder) => decoder.seek(sequence),
		}
	}

	/// Check if the decoder has read enough data to be initialized.
	pub fn is_initialized(&self) -> bool {
		match self.decoder {
			StreamKind::Avc3(ref decoder) => decoder.is_initialized(),
			StreamKind::Fmp4(ref decoder) => decoder.is_initialized(),
			StreamKind::Hev1(ref decoder) => decoder.is_initialized(),
			StreamKind::Av01(ref decoder) => decoder.is_initialized(),
			StreamKind::Mkv(ref decoder) => decoder.is_initialized(),
		}
	}
}
