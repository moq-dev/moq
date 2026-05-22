use std::{fmt, str::FromStr};

use bytes::Buf;
use hang::Error;

use super::stream::StreamFormat;

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
		catalog: crate::catalog::Producer,
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

	/// Return the single track produced by this importer.
	///
	/// Container formats like fMP4 can produce multiple tracks, so callers that
	/// need one concrete track must use a single-track format.
	pub fn track(&self) -> anyhow::Result<&moq_net::TrackProducer> {
		match self.decoder {
			FramedKind::Avc1(ref decoder) => decoder.track(),
			FramedKind::Avc3(ref decoder) => Ok(decoder.track()),
			FramedKind::Fmp4(_) => anyhow::bail!("fmp4 can contain multiple tracks"),
			FramedKind::Hev1(ref decoder) => decoder.track(),
			FramedKind::Av01(ref decoder) => decoder.track(),
			FramedKind::Aac(ref decoder) => Ok(decoder.track()),
			FramedKind::Opus(ref decoder) => Ok(decoder.track()),
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
