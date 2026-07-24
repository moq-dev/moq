//! PCM wire-format constraints shared by the encoder and decoder.

use std::time::Duration;

use crate::Error;

pub(crate) const BYTES_PER_SAMPLE: usize = std::mem::size_of::<f32>();

pub(crate) fn bitrate(sample_rate: u32, channels: u32) -> Result<u64, Error> {
	u64::from(sample_rate)
		.checked_mul(u64::from(channels))
		.and_then(|samples| samples.checked_mul(32))
		.ok_or_else(|| Error::Unsupported("pcm bitrate exceeds the catalog range".into()))
}

pub(crate) fn frame_size(sample_rate: u32, duration: Duration) -> Result<usize, Error> {
	let samples = u128::from(sample_rate) * duration.as_nanos();
	if samples == 0 || !samples.is_multiple_of(1_000_000_000) {
		return Err(Error::Unsupported(format!(
			"pcm frame duration must contain a whole number of samples at {sample_rate} Hz"
		)));
	}
	usize::try_from(samples / 1_000_000_000).map_err(|_| Error::Unsupported("pcm frame duration is too large".into()))
}

pub(crate) fn frame_bytes(frame_size: usize, channels: u32) -> Result<usize, Error> {
	frame_size
		.checked_mul(channels as usize)
		.and_then(|samples| samples.checked_mul(BYTES_PER_SAMPLE))
		.ok_or_else(|| Error::Unsupported("pcm frame is too large".into()))
}
