use bytes::Buf;

use crate::{self as hang, import::Aac};

use super::{Avc3, Fmp4};

#[derive(Clone, Copy, Debug, Eq, PartialEq, strum::EnumString)]
#[strum(serialize_all = "lowercase")]
pub enum DecoderFormat {
	/// aka H264 with inline SPS/PPS
	Avc3,
	/// fMP4/CMAF container.
	Fmp4,
	/// Raw AAC frames (not ADTS).
	Aac,
}

#[derive(derive_more::From)]
enum DecoderKind {
	/// aka H264 with inline SPS/PPS
	Avc3(Avc3),
	// Boxed because it's a large struct and clippy complains about the size.
	Fmp4(Box<Fmp4>),
	Aac(Aac),
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
	pub fn new(broadcast: hang::BroadcastProducer, format: DecoderFormat) -> Self {
		let decoder = match format {
			DecoderFormat::Avc3 => Avc3::new(broadcast).into(),
			DecoderFormat::Fmp4 => Box::new(Fmp4::new(broadcast)).into(),
			DecoderFormat::Aac => Aac::new(broadcast).into(),
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
			DecoderKind::Avc3(decoder) => decoder.initialize(buf)?,
			DecoderKind::Fmp4(decoder) => decoder.decode(buf)?,
			DecoderKind::Aac(decoder) => decoder.initialize(buf)?,
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
			DecoderKind::Avc3(decoder) => decoder.decode_stream(buf, None)?,
			DecoderKind::Fmp4(decoder) => decoder.decode(buf)?,
			// TODO Fix or make this more type safe.
			DecoderKind::Aac(_) => anyhow::bail!("AAC does not support stream decoding"),
		}

		Ok(())
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
		pts: Option<hang::Timestamp>,
	) -> anyhow::Result<()> {
		match &mut self.decoder {
			DecoderKind::Avc3(decoder) => decoder.decode_frame(buf, pts)?,
			DecoderKind::Fmp4(decoder) => decoder.decode(buf)?,
			DecoderKind::Aac(decoder) => decoder.decode(buf, pts)?,
		}

		Ok(())
	}

	/// Check if the decoder has read enough data to be initialized.
	pub fn is_initialized(&self) -> bool {
		match &self.decoder {
			DecoderKind::Avc3(decoder) => decoder.is_initialized(),
			DecoderKind::Fmp4(decoder) => decoder.is_initialized(),
			DecoderKind::Aac(decoder) => decoder.is_initialized(),
		}
	}
}
