use std::{fmt, str::FromStr};

use bytes::Buf;
use hang::Error;

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
}

#[deprecated(note = "use FramedFormat instead")]
pub type DecoderFormat = FramedFormat;

impl FromStr for FramedFormat {
	type Err = Error;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		match s {
			"avc1" | "avcc" => Ok(FramedFormat::Avc1),
			"avc3" => Ok(FramedFormat::Avc3),
			"h264" | "annex-b" => {
				tracing::warn!("format '{s}' is deprecated, use 'avc3' instead");
				Ok(FramedFormat::Avc3)
			}
			"hev1" => Ok(FramedFormat::Hev1),
			"fmp4" | "cmaf" => Ok(FramedFormat::Fmp4),
			"av01" | "av1" | "av1C" => Ok(FramedFormat::Av01),
			"aac" => Ok(FramedFormat::Aac),
			"opus" => Ok(FramedFormat::Opus),
			_ => Err(Error::UnknownFormat(s.to_string())),
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
		}
	}
}

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
}

impl FromStr for StreamFormat {
	type Err = Error;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		match s {
			"avc3" => Ok(StreamFormat::Avc3),
			"h264" | "annex-b" => {
				tracing::warn!("format '{s}' is deprecated, use 'avc3' instead");
				Ok(StreamFormat::Avc3)
			}
			"hev1" => Ok(StreamFormat::Hev1),
			"fmp4" | "cmaf" => Ok(StreamFormat::Fmp4),
			"av01" | "av1" | "av1C" => Ok(StreamFormat::Av01),
			_ => Err(Error::UnknownFormat(s.to_string())),
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
		}
	}
}

#[derive(derive_more::From)]
enum StreamKind {
	/// aka H264 with inline SPS/PPS
	Avc3(super::Avc3),
	// Boxed because it's a large struct and clippy complains about the size.
	Fmp4(Box<super::Fmp4>),
	/// aka H265 with inline SPS/PPS
	Hev1(super::Hev1),
	Av01(super::Av01),
}

#[derive(derive_more::From)]
enum FramedKind {
	/// H264 with AVCC framing
	Avc1(super::Avc1),
	/// H264 with Annex B framing
	Avc3(super::Avc3),
	// Boxed because it's a large struct and clippy complains about the size.
	Fmp4(Box<super::Fmp4>),
	/// aka H265 with inline SPS/PPS
	Hev1(super::Hev1),
	Av01(super::Av01),
	Aac(super::Aac),
	Opus(super::Opus),
}

/// An importer for formats that support stream decoding (unknown frame boundaries).
///
/// This includes formats like H.264 (AVC3), H.265 (HEV1), and fMP4/CMAF.
/// Use this when the caller does not know the frame boundaries.
pub struct Stream {
	decoder: StreamKind,
}

#[deprecated(note = "use Stream instead")]
pub type StreamDecoder = Stream;

impl Stream {
	/// Create a new stream importer with the given format.
	pub fn new(broadcast: moq_lite::BroadcastProducer, catalog: crate::CatalogProducer, format: StreamFormat) -> Self {
		let decoder = match format {
			StreamFormat::Avc3 => super::Avc3::new(broadcast, catalog).into(),
			StreamFormat::Fmp4 => Box::new(super::Fmp4::new(broadcast, catalog)).into(),
			StreamFormat::Hev1 => super::Hev1::new(broadcast, catalog).into(),
			StreamFormat::Av01 => super::Av01::new(broadcast, catalog).into(),
		};

		Self { decoder }
	}

	/// Initialize the decoder with the given buffer and populate the broadcast.
	///
	/// This is not required for self-describing formats like fMP4 or AVC3.
	///
	/// The buffer will be fully consumed, or an error will be returned.
	pub fn initialize<T: Buf + AsRef<[u8]>>(&mut self, buf: &mut T) -> anyhow::Result<()> {
		match self.decoder {
			StreamKind::Avc3(ref mut decoder) => decoder.initialize(buf)?,
			StreamKind::Fmp4(ref mut decoder) => decoder.decode(buf)?,
			StreamKind::Hev1(ref mut decoder) => decoder.initialize(buf)?,
			StreamKind::Av01(ref mut decoder) => decoder.initialize(buf)?,
		}

		anyhow::ensure!(!buf.has_remaining(), "buffer was not fully consumed");

		Ok(())
	}

	/// Decode a stream of data from the given buffer.
	///
	/// This method should be used when the caller does not know the frame boundaries.
	/// For example, reading a fMP4 file from disk or receiving annex.b over the network.
	///
	/// A timestamp cannot be provided because you don't even know if the buffer contains a frame.
	/// The wall clock time will be used if the format does not contain its own timestamps.
	///
	/// If the buffer is not fully consumed, more data is needed.
	pub fn decode_stream<T: Buf + AsRef<[u8]>>(&mut self, buf: &mut T) -> anyhow::Result<()> {
		match self.decoder {
			StreamKind::Avc3(ref mut decoder) => decoder.decode_stream(buf, None),
			StreamKind::Fmp4(ref mut decoder) => decoder.decode(buf),
			StreamKind::Hev1(ref mut decoder) => decoder.decode_stream(buf, None),
			StreamKind::Av01(ref mut decoder) => decoder.decode_stream(buf, None),
		}
	}

	/// Finish the decoder, flushing any buffered data.
	///
	/// This should be called when the input stream ends to ensure the last
	/// group is properly finalized.
	pub fn finish(&mut self) -> anyhow::Result<()> {
		match self.decoder {
			StreamKind::Avc3(ref mut decoder) => decoder.finish(),
			StreamKind::Fmp4(ref mut decoder) => decoder.finish(),
			StreamKind::Hev1(ref mut decoder) => decoder.finish(),
			StreamKind::Av01(ref mut decoder) => decoder.finish(),
		}
	}

	/// Check if the decoder has read enough data to be initialized.
	pub fn is_initialized(&self) -> bool {
		match self.decoder {
			StreamKind::Avc3(ref decoder) => decoder.is_initialized(),
			StreamKind::Fmp4(ref decoder) => decoder.is_initialized(),
			StreamKind::Hev1(ref decoder) => decoder.is_initialized(),
			StreamKind::Av01(ref decoder) => decoder.is_initialized(),
		}
	}
}

/// An importer for formats with known frame boundaries.
///
/// This supports all formats and should be used when the caller knows the frame boundaries.
pub struct Framed {
	decoder: FramedKind,
}

#[deprecated(note = "use Framed instead")]
pub type Decoder = Framed;

impl Framed {
	/// Create a new framed importer with the given format and initialization data.
	///
	/// The buffer will be fully consumed, or an error will be returned.
	pub fn new<T: Buf + AsRef<[u8]>>(
		broadcast: moq_lite::BroadcastProducer,
		catalog: crate::CatalogProducer,
		format: FramedFormat,
		buf: &mut T,
	) -> anyhow::Result<Self> {
		let decoder = match format {
			FramedFormat::Avc1 => {
				let mut decoder = super::Avc1::new(broadcast, catalog);
				decoder.initialize(buf)?;
				decoder.into()
			}
			FramedFormat::Avc3 => {
				let mut decoder = super::Avc3::new(broadcast, catalog);
				decoder.initialize(buf)?;
				decoder.into()
			}
			FramedFormat::Fmp4 => {
				let mut decoder = Box::new(super::Fmp4::new(broadcast, catalog));
				decoder.decode(buf)?;
				decoder.into()
			}
			FramedFormat::Hev1 => {
				let mut decoder = super::Hev1::new(broadcast, catalog);
				decoder.initialize(buf)?;
				decoder.into()
			}
			FramedFormat::Av01 => {
				let mut decoder = super::Av01::new(broadcast, catalog);
				decoder.initialize(buf)?;
				decoder.into()
			}
			FramedFormat::Aac => {
				let config = super::AacConfig::parse(buf)?;
				super::Aac::new(broadcast, catalog, config)?.into()
			}
			FramedFormat::Opus => {
				let config = super::OpusConfig::parse(buf)?;
				super::Opus::new(broadcast, catalog, config)?.into()
			}
		};

		anyhow::ensure!(!buf.has_remaining(), "buffer was not fully consumed");

		Ok(Self { decoder })
	}

	/// Finish the decoder, flushing any buffered data.
	///
	/// This should be called when the input stream ends to ensure the last
	/// group is properly finalized.
	pub fn finish(&mut self) -> anyhow::Result<()> {
		match self.decoder {
			FramedKind::Avc1(ref mut decoder) => decoder.finish(),
			FramedKind::Avc3(ref mut decoder) => decoder.finish(),
			FramedKind::Fmp4(ref mut decoder) => decoder.finish(),
			FramedKind::Hev1(ref mut decoder) => decoder.finish(),
			FramedKind::Av01(ref mut decoder) => decoder.finish(),
			FramedKind::Aac(ref mut decoder) => decoder.finish(),
			FramedKind::Opus(ref mut decoder) => decoder.finish(),
		}
	}

	/// Decode a frame from the given buffer.
	///
	/// This method should be used when the caller knows the buffer consists of an entire frame.
	///
	/// A timestamp may be provided if the format does not contain its own timestamps.
	/// Otherwise, a value of [None] will use the wall clock time.
	///
	/// The buffer will be fully consumed, or an error will be returned.
	/// If the buffer did not contain a frame, future decode calls may fail.
	pub fn decode_frame<T: Buf + AsRef<[u8]>>(
		&mut self,
		buf: &mut T,
		pts: Option<hang::container::Timestamp>,
	) -> anyhow::Result<()> {
		match self.decoder {
			FramedKind::Avc1(ref mut decoder) => decoder.decode(buf, pts)?,
			FramedKind::Avc3(ref mut decoder) => decoder.decode_frame(buf, pts)?,
			FramedKind::Fmp4(ref mut decoder) => decoder.decode(buf)?,
			FramedKind::Hev1(ref mut decoder) => decoder.decode_frame(buf, pts)?,
			FramedKind::Av01(ref mut decoder) => decoder.decode_frame(buf, pts)?,
			FramedKind::Aac(ref mut decoder) => decoder.decode(buf, pts)?,
			FramedKind::Opus(ref mut decoder) => decoder.decode(buf, pts)?,
		}

		anyhow::ensure!(!buf.has_remaining(), "buffer was not fully consumed");

		Ok(())
	}
}

impl From<super::Opus> for Framed {
	fn from(opus: super::Opus) -> Self {
		Self { decoder: opus.into() }
	}
}

impl From<super::Aac> for Framed {
	fn from(aac: super::Aac) -> Self {
		Self { decoder: aac.into() }
	}
}
