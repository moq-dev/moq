use std::{fmt, str::FromStr};

use bytes::Buf;
use hang::Error;

/// The supported framed formats (known frame boundaries).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum FramedFormat {
	/// H264 with AVCC framing (length-prefixed NALUs, out-of-band SPS/PPS).
	#[cfg(feature = "h264")]
	Avc1,
	/// H264 with Annex B framing (start code prefixed, inline SPS/PPS).
	#[cfg(feature = "h264")]
	Avc3,
	/// fMP4/CMAF container.
	#[cfg(feature = "mp4")]
	Fmp4,
	/// aka H265 with inline SPS/PPS
	#[cfg(feature = "h265")]
	Hev1,
	/// AV1 with inline sequence headers
	#[cfg(feature = "av1")]
	Av01,
	/// Raw AAC frames (not ADTS).
	#[cfg(feature = "aac")]
	Aac,
	/// Raw Opus frames (not Ogg).
	#[cfg(feature = "opus")]
	Opus,
}

#[deprecated(note = "use FramedFormat instead")]
pub type DecoderFormat = FramedFormat;

impl FromStr for FramedFormat {
	type Err = Error;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		match s {
			#[cfg(feature = "h264")]
			"avc1" | "avcc" => Ok(FramedFormat::Avc1),
			#[cfg(feature = "h264")]
			"avc3" => Ok(FramedFormat::Avc3),
			#[cfg(feature = "h264")]
			"h264" | "annex-b" => {
				tracing::warn!("format '{s}' is deprecated, use 'avc3' instead");
				Ok(FramedFormat::Avc3)
			}
			#[cfg(feature = "h265")]
			"hev1" => Ok(FramedFormat::Hev1),
			#[cfg(feature = "mp4")]
			"fmp4" | "cmaf" => Ok(FramedFormat::Fmp4),
			#[cfg(feature = "av1")]
			"av01" | "av1" | "av1C" => Ok(FramedFormat::Av01),
			#[cfg(feature = "aac")]
			"aac" => Ok(FramedFormat::Aac),
			#[cfg(feature = "opus")]
			"opus" => Ok(FramedFormat::Opus),
			_ => Err(Error::UnknownFormat(s.to_string())),
		}
	}
}

impl fmt::Display for FramedFormat {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match *self {
			#[cfg(feature = "h264")]
			FramedFormat::Avc1 => write!(f, "avc1"),
			#[cfg(feature = "h264")]
			FramedFormat::Avc3 => write!(f, "avc3"),
			#[cfg(feature = "mp4")]
			FramedFormat::Fmp4 => write!(f, "fmp4"),
			#[cfg(feature = "h265")]
			FramedFormat::Hev1 => write!(f, "hev1"),
			#[cfg(feature = "av1")]
			FramedFormat::Av01 => write!(f, "av01"),
			#[cfg(feature = "aac")]
			FramedFormat::Aac => write!(f, "aac"),
			#[cfg(feature = "opus")]
			FramedFormat::Opus => write!(f, "opus"),
		}
	}
}

/// Formats that support stream decoding (unknown frame boundaries).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum StreamFormat {
	/// aka H264 with inline SPS/PPS
	#[cfg(feature = "h264")]
	Avc3,
	/// fMP4/CMAF container.
	#[cfg(feature = "mp4")]
	Fmp4,
	/// aka H265 with inline SPS/PPS
	#[cfg(feature = "h265")]
	Hev1,
	/// AV1 with inline sequence headers
	#[cfg(feature = "av1")]
	Av01,
}

impl FromStr for StreamFormat {
	type Err = Error;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		match s {
			#[cfg(feature = "h264")]
			"avc3" => Ok(StreamFormat::Avc3),
			#[cfg(feature = "h264")]
			"h264" | "annex-b" => {
				tracing::warn!("format '{s}' is deprecated, use 'avc3' instead");
				Ok(StreamFormat::Avc3)
			}
			#[cfg(feature = "h265")]
			"hev1" => Ok(StreamFormat::Hev1),
			#[cfg(feature = "mp4")]
			"fmp4" | "cmaf" => Ok(StreamFormat::Fmp4),
			#[cfg(feature = "av1")]
			"av01" | "av1" | "av1C" => Ok(StreamFormat::Av01),
			_ => Err(Error::UnknownFormat(s.to_string())),
		}
	}
}

impl fmt::Display for StreamFormat {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match *self {
			#[cfg(feature = "h264")]
			StreamFormat::Avc3 => write!(f, "avc3"),
			#[cfg(feature = "mp4")]
			StreamFormat::Fmp4 => write!(f, "fmp4"),
			#[cfg(feature = "h265")]
			StreamFormat::Hev1 => write!(f, "hev1"),
			#[cfg(feature = "av1")]
			StreamFormat::Av01 => write!(f, "av01"),
		}
	}
}

impl From<StreamFormat> for FramedFormat {
	fn from(format: StreamFormat) -> Self {
		match format {
			#[cfg(feature = "h264")]
			StreamFormat::Avc3 => FramedFormat::Avc3,
			#[cfg(feature = "mp4")]
			StreamFormat::Fmp4 => FramedFormat::Fmp4,
			#[cfg(feature = "h265")]
			StreamFormat::Hev1 => FramedFormat::Hev1,
			#[cfg(feature = "av1")]
			StreamFormat::Av01 => FramedFormat::Av01,
		}
	}
}

#[derive(derive_more::From)]
enum StreamKind {
	/// aka H264 with inline SPS/PPS
	#[cfg(feature = "h264")]
	Avc3(super::Avc3),
	// Boxed because it's a large struct and clippy complains about the size.
	#[cfg(feature = "mp4")]
	Fmp4(Box<super::Fmp4>),
	/// aka H265 with inline SPS/PPS
	#[cfg(feature = "h265")]
	Hev1(super::Hev1),
	#[cfg(feature = "av1")]
	Av01(super::Av01),
}

#[derive(derive_more::From)]
enum FramedKind {
	/// H264 with AVCC framing
	#[cfg(feature = "h264")]
	Avc1(super::Avc1),
	/// H264 with Annex B framing
	#[cfg(feature = "h264")]
	Avc3(super::Avc3),
	// Boxed because it's a large struct and clippy complains about the size.
	#[cfg(feature = "mp4")]
	Fmp4(Box<super::Fmp4>),
	/// aka H265 with inline SPS/PPS
	#[cfg(feature = "h265")]
	Hev1(super::Hev1),
	#[cfg(feature = "av1")]
	Av01(super::Av01),
	#[cfg(feature = "aac")]
	Aac(super::Aac),
	#[cfg(feature = "opus")]
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
			#[cfg(feature = "h264")]
			StreamFormat::Avc3 => super::Avc3::new(broadcast, catalog).into(),
			#[cfg(feature = "mp4")]
			StreamFormat::Fmp4 => Box::new(super::Fmp4::new(broadcast, catalog)).into(),
			#[cfg(feature = "h265")]
			StreamFormat::Hev1 => super::Hev1::new(broadcast, catalog).into(),
			#[cfg(feature = "av1")]
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
			#[cfg(feature = "h264")]
			StreamKind::Avc3(ref mut decoder) => decoder.initialize(buf)?,
			#[cfg(feature = "mp4")]
			StreamKind::Fmp4(ref mut decoder) => decoder.decode(buf)?,
			#[cfg(feature = "h265")]
			StreamKind::Hev1(ref mut decoder) => decoder.initialize(buf)?,
			#[cfg(feature = "av1")]
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
			#[cfg(feature = "h264")]
			StreamKind::Avc3(ref mut decoder) => decoder.decode_stream(buf, None),
			#[cfg(feature = "mp4")]
			StreamKind::Fmp4(ref mut decoder) => decoder.decode(buf),
			#[cfg(feature = "h265")]
			StreamKind::Hev1(ref mut decoder) => decoder.decode_stream(buf, None),
			#[cfg(feature = "av1")]
			StreamKind::Av01(ref mut decoder) => decoder.decode_stream(buf, None),
		}
	}

	/// Finish the decoder, flushing any buffered data.
	///
	/// This should be called when the input stream ends to ensure the last
	/// group is properly finalized.
	pub fn finish(&mut self) -> anyhow::Result<()> {
		match self.decoder {
			#[cfg(feature = "h264")]
			StreamKind::Avc3(ref mut decoder) => decoder.finish(),
			#[cfg(feature = "mp4")]
			StreamKind::Fmp4(ref mut decoder) => decoder.finish(),
			#[cfg(feature = "h265")]
			StreamKind::Hev1(ref mut decoder) => decoder.finish(),
			#[cfg(feature = "av1")]
			StreamKind::Av01(ref mut decoder) => decoder.finish(),
		}
	}

	/// Check if the decoder has read enough data to be initialized.
	pub fn is_initialized(&self) -> bool {
		match self.decoder {
			#[cfg(feature = "h264")]
			StreamKind::Avc3(ref decoder) => decoder.is_initialized(),
			#[cfg(feature = "mp4")]
			StreamKind::Fmp4(ref decoder) => decoder.is_initialized(),
			#[cfg(feature = "h265")]
			StreamKind::Hev1(ref decoder) => decoder.is_initialized(),
			#[cfg(feature = "av1")]
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
			#[cfg(feature = "h264")]
			FramedFormat::Avc1 => {
				let mut decoder = super::Avc1::new(broadcast, catalog);
				decoder.initialize(buf)?;
				decoder.into()
			}
			#[cfg(feature = "h264")]
			FramedFormat::Avc3 => {
				let mut decoder = super::Avc3::new(broadcast, catalog);
				decoder.initialize(buf)?;
				decoder.into()
			}
			#[cfg(feature = "mp4")]
			FramedFormat::Fmp4 => {
				let mut decoder = Box::new(super::Fmp4::new(broadcast, catalog));
				decoder.decode(buf)?;
				decoder.into()
			}
			#[cfg(feature = "h265")]
			FramedFormat::Hev1 => {
				let mut decoder = super::Hev1::new(broadcast, catalog);
				decoder.initialize(buf)?;
				decoder.into()
			}
			#[cfg(feature = "av1")]
			FramedFormat::Av01 => {
				let mut decoder = super::Av01::new(broadcast, catalog);
				decoder.initialize(buf)?;
				decoder.into()
			}
			#[cfg(feature = "aac")]
			FramedFormat::Aac => {
				let config = super::AacConfig::parse(buf)?;
				super::Aac::new(broadcast, catalog, config)?.into()
			}
			#[cfg(feature = "opus")]
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
			#[cfg(feature = "h264")]
			FramedKind::Avc1(ref mut decoder) => decoder.finish(),
			#[cfg(feature = "h264")]
			FramedKind::Avc3(ref mut decoder) => decoder.finish(),
			#[cfg(feature = "mp4")]
			FramedKind::Fmp4(ref mut decoder) => decoder.finish(),
			#[cfg(feature = "h265")]
			FramedKind::Hev1(ref mut decoder) => decoder.finish(),
			#[cfg(feature = "av1")]
			FramedKind::Av01(ref mut decoder) => decoder.finish(),
			#[cfg(feature = "aac")]
			FramedKind::Aac(ref mut decoder) => decoder.finish(),
			#[cfg(feature = "opus")]
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
			#[cfg(feature = "h264")]
			FramedKind::Avc1(ref mut decoder) => decoder.decode(buf, pts)?,
			#[cfg(feature = "h264")]
			FramedKind::Avc3(ref mut decoder) => decoder.decode_frame(buf, pts)?,
			#[cfg(feature = "mp4")]
			FramedKind::Fmp4(ref mut decoder) => decoder.decode(buf)?,
			#[cfg(feature = "h265")]
			FramedKind::Hev1(ref mut decoder) => decoder.decode_frame(buf, pts)?,
			#[cfg(feature = "av1")]
			FramedKind::Av01(ref mut decoder) => decoder.decode_frame(buf, pts)?,
			#[cfg(feature = "aac")]
			FramedKind::Aac(ref mut decoder) => decoder.decode(buf, pts)?,
			#[cfg(feature = "opus")]
			FramedKind::Opus(ref mut decoder) => decoder.decode(buf, pts)?,
		}

		anyhow::ensure!(!buf.has_remaining(), "buffer was not fully consumed");

		Ok(())
	}
}

#[cfg(feature = "opus")]
impl From<super::Opus> for Framed {
	fn from(opus: super::Opus) -> Self {
		Self { decoder: opus.into() }
	}
}

#[cfg(feature = "aac")]
impl From<super::Aac> for Framed {
	fn from(aac: super::Aac) -> Self {
		Self { decoder: aac.into() }
	}
}
