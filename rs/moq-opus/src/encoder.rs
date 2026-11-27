use crate::src::opus_defines::{
	OPUS_APPLICATION_AUDIO, OPUS_APPLICATION_VOIP, OPUS_AUTO, OPUS_GET_APPLICATION_REQUEST, OPUS_GET_BITRATE_REQUEST,
	OPUS_GET_DTX_REQUEST, OPUS_GET_IN_DTX_REQUEST, OPUS_GET_LOOKAHEAD_REQUEST, OPUS_GET_SIGNAL_REQUEST,
	OPUS_RESET_STATE, OPUS_SET_BITRATE_REQUEST, OPUS_SET_DTX_REQUEST, OPUS_SET_SIGNAL_REQUEST, OPUS_SIGNAL_MUSIC,
	OPUS_SIGNAL_VOICE,
};
use crate::src::opus_encoder::{
	opus_encode_float, opus_encoder_create, opus_encoder_ctl_impl, opus_encoder_destroy, OpusEncoder,
};
use crate::varargs::varargs;
use crate::Error;

/// What an [`Encoder`] tunes for, fixed when it is created.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Application {
	/// Speech intelligibility.
	Voip,
	/// Faithfulness to the input, for music and mixed content.
	Audio,
}

impl Application {
	pub(crate) fn code(self) -> i32 {
		match self {
			Self::Voip => OPUS_APPLICATION_VOIP,
			Self::Audio => OPUS_APPLICATION_AUDIO,
		}
	}
}

/// A hint about the input, which biases the encoder's mode decisions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Signal {
	/// No hint: the encoder classifies the input itself.
	#[default]
	Auto,
	/// The input is mostly speech.
	Voice,
	/// The input is mostly music.
	Music,
}

impl Signal {
	fn code(self) -> i32 {
		match self {
			Self::Auto => OPUS_AUTO,
			Self::Voice => OPUS_SIGNAL_VOICE,
			Self::Music => OPUS_SIGNAL_MUSIC,
		}
	}
}

/// A mono or stereo Opus encoder.
pub struct Encoder {
	inner: *mut OpusEncoder,
	channels: usize,
}

// SAFETY: the encoder state is heap-allocated and owned by this handle alone,
// and libopus keeps no thread-local state, so it may move across threads.
unsafe impl Send for Encoder {}

impl Encoder {
	/// Encode `channels` (1 or 2) interleaved at `sample_rate`, which is 8, 12,
	/// 16, 24, or 48 kHz.
	pub fn new(sample_rate: u32, channels: u8, application: Application) -> Result<Self, Error> {
		let sample_rate = i32::try_from(sample_rate).map_err(|_| Error::BadArg)?;
		let mut err = 0;
		// SAFETY: `err` is a valid out-pointer; libopus validates every other argument.
		let inner = unsafe { opus_encoder_create(sample_rate, channels.into(), application.code(), &mut err) };
		Error::check(err)?;
		if inner.is_null() {
			return Err(Error::AllocFail);
		}
		Ok(Self {
			inner,
			channels: channels.into(),
		})
	}

	/// Encode one frame of interleaved `pcm` into `packet`, returning the packet's length.
	///
	/// The frame is `pcm.len()` divided by the channel count, and must be 2.5, 5,
	/// 10, 20, 40, or 60 ms long. A packet never exceeds 1275 bytes per frame.
	pub fn encode_float(&mut self, pcm: &[f32], packet: &mut [u8]) -> Result<usize, Error> {
		if !pcm.len().is_multiple_of(self.channels) {
			return Err(Error::BadArg);
		}
		let frame_size = i32::try_from(pcm.len() / self.channels).map_err(|_| Error::BadArg)?;
		let max = i32::try_from(packet.len()).unwrap_or(i32::MAX);
		// SAFETY: `inner` is live, and libopus reads `frame_size * channels` samples
		// (all of `pcm`) and writes at most `max` bytes of `packet`.
		let len = unsafe { opus_encode_float(self.inner, pcm.as_ptr(), frame_size, packet.as_mut_ptr(), max) };
		Ok(Error::check(len)? as usize)
	}

	/// The target bitrate in bits per second.
	pub fn bitrate(&self) -> Result<u32, Error> {
		Ok(self.get(OPUS_GET_BITRATE_REQUEST)? as u32)
	}

	/// Target `bitrate` bits per second. libopus clamps it to what the
	/// configuration can code, 500 to 300,000 per channel.
	pub fn set_bitrate(&mut self, bitrate: u32) -> Result<(), Error> {
		self.set(
			OPUS_SET_BITRATE_REQUEST,
			i32::try_from(bitrate).map_err(|_| Error::BadArg)?,
		)
	}

	/// The application the encoder was created with.
	pub fn application(&self) -> Result<Application, Error> {
		match self.get(OPUS_GET_APPLICATION_REQUEST)? {
			OPUS_APPLICATION_VOIP => Ok(Application::Voip),
			OPUS_APPLICATION_AUDIO => Ok(Application::Audio),
			// `new` takes only the applications above.
			_ => Err(Error::InvalidState),
		}
	}

	/// The signal hint.
	pub fn signal(&self) -> Result<Signal, Error> {
		match self.get(OPUS_GET_SIGNAL_REQUEST)? {
			OPUS_SIGNAL_VOICE => Ok(Signal::Voice),
			OPUS_SIGNAL_MUSIC => Ok(Signal::Music),
			OPUS_AUTO => Ok(Signal::Auto),
			_ => Err(Error::InvalidState),
		}
	}

	/// Hint what the input is.
	pub fn set_signal(&mut self, signal: Signal) -> Result<(), Error> {
		self.set(OPUS_SET_SIGNAL_REQUEST, signal.code())
	}

	/// Whether discontinuous transmission is on.
	pub fn dtx(&self) -> Result<bool, Error> {
		Ok(self.get(OPUS_GET_DTX_REQUEST)? != 0)
	}

	/// Turn discontinuous transmission on or off: when on, silence codes as
	/// occasional keep-alive packets instead of full frames.
	pub fn set_dtx(&mut self, dtx: bool) -> Result<(), Error> {
		self.set(OPUS_SET_DTX_REQUEST, dtx.into())
	}

	/// Whether the last packet encoded was discontinuous transmission.
	pub fn in_dtx(&self) -> Result<bool, Error> {
		Ok(self.get(OPUS_GET_IN_DTX_REQUEST)? != 0)
	}

	/// Samples per channel the encoder delays its input by, at its sample rate.
	pub fn lookahead(&self) -> Result<usize, Error> {
		Ok(self.get(OPUS_GET_LOOKAHEAD_REQUEST)? as usize)
	}

	/// Forget all history, as if newly created, keeping every setting.
	pub fn reset(&mut self) -> Result<(), Error> {
		// SAFETY: `inner` is live and OPUS_RESET_STATE takes no arguments.
		Error::check(unsafe { opus_encoder_ctl_impl(self.inner, OPUS_RESET_STATE, varargs![]) })?;
		Ok(())
	}

	fn set(&mut self, request: i32, value: i32) -> Result<(), Error> {
		// SAFETY: `inner` is live and each setter request takes one i32.
		Error::check(unsafe { opus_encoder_ctl_impl(self.inner, request, varargs![value]) })?;
		Ok(())
	}

	/// A getter request, whose value is never negative except as `OPUS_AUTO`.
	fn get(&self, request: i32) -> Result<i32, Error> {
		let mut value = 0;
		// SAFETY: `inner` is live and each getter request takes one i32 out-pointer.
		// A getter only reads the state, and `Encoder` is not `Sync`, so nothing
		// else touches it during the call.
		Error::check(unsafe { opus_encoder_ctl_impl(self.inner, request, varargs![&mut value]) })?;
		Ok(value)
	}
}

impl Drop for Encoder {
	fn drop(&mut self) {
		// SAFETY: `inner` is live and nothing else aliases it.
		unsafe { opus_encoder_destroy(self.inner) };
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn settings_round_trip() {
		let mut encoder = Encoder::new(48_000, 2, Application::Voip).unwrap();
		assert_eq!(encoder.application().unwrap(), Application::Voip);
		assert_eq!(encoder.signal().unwrap(), Signal::Auto);

		encoder.set_signal(Signal::Music).unwrap();
		assert_eq!(encoder.signal().unwrap(), Signal::Music);
		encoder.set_dtx(true).unwrap();
		assert!(encoder.dtx().unwrap());
		encoder.set_bitrate(32_000).unwrap();
		assert_eq!(encoder.bitrate().unwrap(), 32_000);
	}

	#[test]
	fn encodes_a_frame() {
		let mut encoder = Encoder::new(48_000, 2, Application::Audio).unwrap();
		let mut packet = [0u8; 4000];
		let len = encoder.encode_float(&[0.0; 960 * 2], &mut packet).unwrap();
		assert!(len > 0);
		assert_eq!(crate::packet::samples_per_frame(&packet[..len], 48_000), Some(960));
	}

	#[test]
	fn rejects_bad_arguments() {
		assert_eq!(Encoder::new(44_100, 2, Application::Audio).err(), Some(Error::BadArg));
		assert_eq!(Encoder::new(48_000, 3, Application::Audio).err(), Some(Error::BadArg));

		let mut encoder = Encoder::new(48_000, 2, Application::Audio).unwrap();
		let mut packet = [0u8; 4000];
		// Not a whole number of stereo frames.
		assert_eq!(encoder.encode_float(&[0.0; 3], &mut packet), Err(Error::BadArg));
		// Not an Opus frame duration.
		assert_eq!(encoder.encode_float(&[0.0; 100 * 2], &mut packet), Err(Error::BadArg));
	}
}
