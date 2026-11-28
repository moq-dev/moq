use crate::src::opus_defines::{OPUS_RESET_STATE, OPUS_SET_GAIN_REQUEST};
use crate::src::opus_multistream_decoder::{
	opus_multistream_decode_float, opus_multistream_decoder_create, opus_multistream_decoder_ctl_impl,
	opus_multistream_decoder_destroy, OpusMSDecoder,
};
use crate::varargs::varargs;
use crate::Error;

/// How a multistream packet's Opus streams map to output channels (RFC 7845 §5.1.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mapping {
	/// Opus streams in each packet.
	pub streams: u8,
	/// How many of those streams are stereo; they come first.
	pub coupled: u8,
	/// For each output channel, the decoded channel it takes: the left and right
	/// of each coupled stream, then each mono stream. 255 is silence.
	pub table: Vec<u8>,
}

impl Mapping {
	/// A mono or stereo stream (channel mapping family 0): one stream, coupled when stereo.
	pub fn family0(channels: u8) -> Result<Self, Error> {
		match channels {
			1 => Ok(Self {
				streams: 1,
				coupled: 0,
				table: vec![0],
			}),
			2 => Ok(Self {
				streams: 1,
				coupled: 1,
				table: vec![0, 1],
			}),
			_ => Err(Error::BadArg),
		}
	}
}

/// An Opus decoder for any channel mapping, mono and stereo included.
pub struct Decoder {
	inner: *mut OpusMSDecoder,
	channels: usize,
}

// SAFETY: the decoder state is heap-allocated and owned by this handle alone,
// and libopus keeps no thread-local state, so it may move across threads.
unsafe impl Send for Decoder {}

impl Decoder {
	/// Decode `mapping` to its channels at `sample_rate`, which is 8, 12, 16, 24, or 48 kHz.
	pub fn new(sample_rate: u32, mapping: &Mapping) -> Result<Self, Error> {
		let sample_rate = i32::try_from(sample_rate).map_err(|_| Error::BadArg)?;
		let channels = i32::try_from(mapping.table.len()).map_err(|_| Error::BadArg)?;
		let mut err = 0;
		// SAFETY: libopus reads `channels` entries of the table, all of it, and
		// `err` is a valid out-pointer.
		let inner = unsafe {
			opus_multistream_decoder_create(
				sample_rate,
				channels,
				mapping.streams.into(),
				mapping.coupled.into(),
				mapping.table.as_ptr(),
				&mut err,
			)
		};
		Error::check(err)?;
		if inner.is_null() {
			return Err(Error::AllocFail);
		}
		Ok(Self {
			inner,
			channels: mapping.table.len(),
		})
	}

	/// Scale the output by `gain` in Q7.8 dB (1/256 dB), as an OpusHead's output gain is.
	/// Resets keep it.
	pub fn set_gain(&mut self, gain: i16) -> Result<(), Error> {
		// SAFETY: `inner` is live and OPUS_SET_GAIN takes one i32.
		Error::check(unsafe {
			opus_multistream_decoder_ctl_impl(self.inner, OPUS_SET_GAIN_REQUEST, varargs![i32::from(gain)])
		})?;
		Ok(())
	}

	/// Decode `packet` into interleaved `pcm`, returning the samples per channel written.
	///
	/// An empty `packet` was lost: the decoder conceals as many samples per channel
	/// as `pcm` holds, a multiple of 2.5 ms, but at most 120 ms per call, so a longer
	/// gap takes several calls, each advancing by the count returned. Otherwise
	/// `pcm` must hold the whole packet, up to 120 ms.
	pub fn decode_float(&mut self, packet: &[u8], pcm: &mut [f32]) -> Result<usize, Error> {
		let len = i32::try_from(packet.len()).map_err(|_| Error::BadArg)?;
		let frame_size = i32::try_from(pcm.len() / self.channels).map_err(|_| Error::BadArg)?;
		// SAFETY: `inner` is live, libopus reads at most `len` bytes of `packet`
		// (none when it is empty) and writes at most `frame_size * channels`
		// samples, within `pcm`.
		let samples =
			unsafe { opus_multistream_decode_float(self.inner, packet.as_ptr(), len, pcm.as_mut_ptr(), frame_size, 0) };
		Ok(Error::check(samples)? as usize)
	}

	/// Forget all history, as if newly created, keeping the gain.
	pub fn reset(&mut self) -> Result<(), Error> {
		// SAFETY: `inner` is live and OPUS_RESET_STATE takes no arguments.
		Error::check(unsafe { opus_multistream_decoder_ctl_impl(self.inner, OPUS_RESET_STATE, varargs![]) })?;
		Ok(())
	}
}

impl Drop for Decoder {
	fn drop(&mut self) {
		// SAFETY: `inner` is live and nothing else aliases it.
		unsafe { opus_multistream_decoder_destroy(self.inner) };
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{Application, Encoder};

	#[test]
	fn stereo_round_trips() {
		let mut encoder = Encoder::new(48_000, 2, Application::Audio).unwrap();
		let mut decoder = Decoder::new(48_000, &Mapping::family0(2).unwrap()).unwrap();
		decoder.set_gain(-256).unwrap();

		let mut packet = [0u8; 4000];
		let len = encoder.encode_float(&[0.0; 960 * 2], &mut packet).unwrap();
		let mut pcm = [0.0f32; 5760 * 2];
		assert_eq!(decoder.decode_float(&packet[..len], &mut pcm).unwrap(), 960);

		// A loss conceals as much as `pcm` holds, up to 120 ms.
		assert_eq!(decoder.decode_float(&[], &mut pcm[..480 * 2]).unwrap(), 480);
		let mut long = [0.0f32; 9600 * 2];
		assert_eq!(decoder.decode_float(&[], &mut long).unwrap(), 5760);
		decoder.reset().unwrap();
	}

	#[test]
	fn rejects_bad_arguments() {
		assert_eq!(Mapping::family0(3).err(), Some(Error::BadArg));
		// More coupled streams than streams.
		let mapping = Mapping {
			streams: 1,
			coupled: 2,
			table: vec![0, 1],
		};
		assert_eq!(Decoder::new(48_000, &mapping).err(), Some(Error::BadArg));

		let mut decoder = Decoder::new(48_000, &Mapping::family0(2).unwrap()).unwrap();
		assert_eq!(
			decoder.decode_float(&[0xff; 3], &mut [0.0; 5760 * 2]),
			Err(Error::InvalidPacket)
		);
	}
}
