use std::{fmt, str::FromStr};

use bytes::Buf;
use hang::Error;

/// The supported decoder formats.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum DecoderFormat {
	/// aka H264 with inline SPS/PPS
	#[cfg(feature = "h264")]
	Avc3,
	/// fMP4/CMAF container.
	#[cfg(feature = "mp4")]
	Fmp4,
	/// aka H265 with inline SPS/PPS
	#[cfg(feature = "h265")]
	Hev1,
	/// Raw AAC frames (not ADTS).
	#[cfg(feature = "aac")]
	Aac,
	/// Raw Opus frames (not Ogg).
	#[cfg(feature = "opus")]
	Opus,
}

impl FromStr for DecoderFormat {
	type Err = Error;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		match s {
			#[cfg(feature = "h264")]
			"avc3" => Ok(DecoderFormat::Avc3),
			#[cfg(feature = "h264")]
			"h264" | "annex-b" => {
				tracing::warn!("format '{s}' is deprecated, use 'avc3' instead");
				Ok(DecoderFormat::Avc3)
			}
			#[cfg(feature = "h265")]
			"hev1" => Ok(DecoderFormat::Hev1),
			#[cfg(feature = "mp4")]
			"fmp4" | "cmaf" => Ok(DecoderFormat::Fmp4),
			#[cfg(feature = "aac")]
			"aac" => Ok(DecoderFormat::Aac),
			#[cfg(feature = "opus")]
			"opus" => Ok(DecoderFormat::Opus),
			_ => Err(Error::UnknownFormat(s.to_string())),
		}
	}
}

impl fmt::Display for DecoderFormat {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			#[cfg(feature = "h264")]
			DecoderFormat::Avc3 => write!(f, "avc3"),
			#[cfg(feature = "mp4")]
			DecoderFormat::Fmp4 => write!(f, "fmp4"),
			#[cfg(feature = "h265")]
			DecoderFormat::Hev1 => write!(f, "hev1"),
			#[cfg(feature = "aac")]
			DecoderFormat::Aac => write!(f, "aac"),
			#[cfg(feature = "opus")]
			DecoderFormat::Opus => write!(f, "opus"),
		}
	}
}

#[derive(derive_more::From)]
enum DecoderKind {
	/// aka H264 with inline SPS/PPS
	#[cfg(feature = "h264")]
	Avc3(super::Avc3),
	// Boxed because it's a large struct and clippy complains about the size.
	#[cfg(feature = "mp4")]
	Fmp4(Box<super::Fmp4>),
	/// aka H265 with inline SPS/PPS
	#[cfg(feature = "h265")]
	Hev1(super::Hev1),
	#[cfg(feature = "aac")]
	Aac(super::Aac),
	#[cfg(feature = "opus")]
	Opus(super::Opus),
}

/// A generic interface for importing a stream of media into a hang broadcast.
///
/// If you know the format in advance, use the specific decoder instead.
pub struct Decoder {
	// The decoder for the given format.
	decoder: DecoderKind,
}

impl Decoder {
	/// Create a new decoder with the given format.
	pub fn new(broadcast: moq_lite::BroadcastProducer, catalog: hang::CatalogProducer, format: DecoderFormat) -> Self {
		let decoder = match format {
			#[cfg(feature = "h264")]
			DecoderFormat::Avc3 => super::Avc3::new(broadcast, catalog).into(),
			#[cfg(feature = "mp4")]
			DecoderFormat::Fmp4 => Box::new(super::Fmp4::new(broadcast, catalog, super::Fmp4Config::default())).into(),
			#[cfg(feature = "h265")]
			DecoderFormat::Hev1 => super::Hev1::new(broadcast, catalog).into(),
			#[cfg(feature = "aac")]
			DecoderFormat::Aac => super::Aac::new(broadcast, catalog).into(),
			#[cfg(feature = "opus")]
			DecoderFormat::Opus => super::Opus::new(broadcast, catalog).into(),
		};

		Self { decoder }
	}

	/// Initialize the decoder with the given buffer and populate the broadcast.
	///
	/// This is not required for self-describing formats like fMP4 or AVC3.
	/// However, some formats, like AAC, use a separate encoding for its initialization data.
	///
	/// The buffer will be fully consumed, or an error will be returned.
	pub fn initialize<T: Buf + AsRef<[u8]>>(&mut self, buf: &mut T) -> anyhow::Result<()> {
		match &mut self.decoder {
			#[cfg(feature = "h264")]
			DecoderKind::Avc3(decoder) => decoder.initialize(buf)?,
			#[cfg(feature = "mp4")]
			DecoderKind::Fmp4(decoder) => decoder.decode(buf)?,
			#[cfg(feature = "h265")]
			DecoderKind::Hev1(decoder) => decoder.initialize(buf)?,
			#[cfg(feature = "aac")]
			DecoderKind::Aac(decoder) => decoder.initialize(buf)?,
			#[cfg(feature = "opus")]
			DecoderKind::Opus(decoder) => decoder.initialize(buf)?,
		}

		anyhow::ensure!(!buf.has_remaining(), "buffer was not fully consumed");

		Ok(())
	}

	/// Decode a stream of frames from the given buffer.
	///
	/// This method should be used when the caller does not know the frame boundaries.
	/// For example, reading a fMP4 file from disk or receiving annex.b over the network.
	///
	/// A timestamp cannot be provided because you don't even know if the buffer contains a frame.
	/// The wall clock time will be used if the format does not contain its own timestamps.
	///
	/// If you know the buffer ends with a frame, use [Self::decode_frame] instead.
	/// ex. the end of the file or if there's higher level framing (like a container).
	/// This may avoid a frame of latency depending on the format.
	///
	/// If the buffer is not fully consumed, more data is needed.
	pub fn decode_stream<T: Buf + AsRef<[u8]>>(&mut self, buf: &mut T) -> anyhow::Result<()> {
		match &mut self.decoder {
			#[cfg(feature = "h264")]
			DecoderKind::Avc3(decoder) => decoder.decode_stream(buf, None),
			#[cfg(feature = "mp4")]
			DecoderKind::Fmp4(decoder) => decoder.decode(buf),
			#[cfg(feature = "h265")]
			DecoderKind::Hev1(decoder) => decoder.decode_stream(buf, None),
			// TODO Fix or make these more type safe.
			#[cfg(feature = "aac")]
			DecoderKind::Aac(_) => anyhow::bail!("AAC does not support stream decoding"),
			#[cfg(feature = "opus")]
			DecoderKind::Opus(_) => anyhow::bail!("Opus does not support stream decoding"),
		}
	}

	/// Flush the decoder at a frame boundary.
	///
	/// This method should be used when the caller knows the buffer consists of an entire frame.
	/// If you don't know the buffer contains a frame, use [Self::decode_stream] instead.
	///
	/// A timestamp may be provided if the format does not contain its own timestamps.
	/// Otherwise, a value of [None] will use the wall clock time like [Self::decode_stream].
	///
	/// The buffer will be fully consumed, or an error will be returned.
	/// If the buffer did not contain a frame, future decode calls may fail.
	pub fn decode_frame<T: Buf + AsRef<[u8]>>(
		&mut self,
		buf: &mut T,
		pts: Option<hang::container::Timestamp>,
	) -> anyhow::Result<()> {
		match &mut self.decoder {
			#[cfg(feature = "h264")]
			DecoderKind::Avc3(decoder) => decoder.decode_frame(buf, pts),
			#[cfg(feature = "mp4")]
			DecoderKind::Fmp4(decoder) => decoder.decode(buf),
			#[cfg(feature = "h265")]
			DecoderKind::Hev1(decoder) => decoder.decode_frame(buf, pts),
			#[cfg(feature = "aac")]
			DecoderKind::Aac(decoder) => decoder.decode(buf, pts),
			#[cfg(feature = "opus")]
			DecoderKind::Opus(decoder) => decoder.decode(buf, pts),
		}
	}

	/// Check if the decoder has read enough data to be initialized.
	pub fn is_initialized(&self) -> bool {
		match &self.decoder {
			#[cfg(feature = "h264")]
			DecoderKind::Avc3(decoder) => decoder.is_initialized(),
			#[cfg(feature = "mp4")]
			DecoderKind::Fmp4(decoder) => decoder.is_initialized(),
			#[cfg(feature = "h265")]
			DecoderKind::Hev1(decoder) => decoder.is_initialized(),
			#[cfg(feature = "aac")]
			DecoderKind::Aac(decoder) => decoder.is_initialized(),
			#[cfg(feature = "opus")]
			DecoderKind::Opus(decoder) => decoder.is_initialized(),
		}
	}
}
