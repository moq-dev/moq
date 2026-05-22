use std::{fmt, str::FromStr};

use bytes::Buf;
use hang::Error;

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

/// An importer for formats that support stream decoding (unknown frame boundaries).
///
/// This includes formats like H.264 (AVC3), H.265 (HEV1), and fMP4/CMAF.
/// Use this when the caller does not know the frame boundaries.
pub struct Stream {
	decoder: StreamKind,
}

impl Stream {
	/// Create a new stream importer with the given format.
	pub fn new(broadcast: moq_net::BroadcastProducer, catalog: crate::catalog::Producer, format: StreamFormat) -> Self {
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
