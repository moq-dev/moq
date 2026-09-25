//! Opus through libopus, the software decoder for every host.

use unsafe_libopus::{
	OPUS_OK, OPUS_RESET_STATE, OpusDecoder, opus_decode_float, opus_decoder_create, opus_decoder_ctl_impl,
	opus_decoder_destroy, varargs,
};

use super::Backend;
use crate::decode::Decoded;
use crate::{Error, Layout, opus};

pub(super) const NAME: &str = "libopus";

/// Opus packets cap at 120 ms (RFC 6716 §2.1.4).
const MAX_FRAME_MS: usize = 120;

pub(super) struct Libopus {
	inner: *mut OpusDecoder,
	sample_rate: u32,
	layout: Layout,
	pre_skip: usize,
	max_frame_size: usize,
	in_dtx: bool,
}

// SAFETY: the decoder is owned exclusively and libopus keeps no thread-local state.
unsafe impl Send for Libopus {}

impl Libopus {
	/// Parses the OpusHead `description` if present; falls back to the catalog's
	/// declared sample rate / channel count.
	pub(super) fn open(catalog: &hang::catalog::AudioConfig) -> Result<Box<dyn Backend>, Error> {
		let (sample_rate, channel_count, pre_skip) = if let Some(desc) = &catalog.description {
			let mut buf = desc.as_ref();
			match moq_mux::codec::opus::Config::parse(&mut buf) {
				Ok(head) => (head.sample_rate, head.channel_count, head.pre_skip),
				Err(_) => (catalog.sample_rate, catalog.channel_count, 0),
			}
		} else {
			(catalog.sample_rate, catalog.channel_count, 0)
		};

		opus::validate_rate(sample_rate)?;
		let channels = opus::validate_channels(channel_count)?;
		let layout = Layout::from_channels(channel_count)?;

		let mut err = 0i32;
		// SAFETY: out-pointer is valid; inner is checked for null below.
		let inner = unsafe { opus_decoder_create(sample_rate as i32, channels, &mut err) };
		if err != OPUS_OK || inner.is_null() {
			return Err(opus::error(err, "opus_decoder_create"));
		}

		Ok(Box::new(Self {
			inner,
			sample_rate,
			layout,
			// OpusHead counts pre-skip at 48 kHz whatever rate the decoder runs at.
			pre_skip: (pre_skip as usize * sample_rate as usize) / 48_000,
			max_frame_size: (sample_rate as usize * MAX_FRAME_MS) / 1000,
			in_dtx: false,
		}))
	}
}

impl Backend for Libopus {
	/// Empty packets invoke packet-loss concealment. Loss during DTX remains
	/// classified as DTX, while loss during active audio remains active.
	fn decode(&mut self, packet: &[u8]) -> Result<Decoded, Error> {
		let channels = self.layout.channels() as usize;
		let mut out = vec![0.0f32; self.max_frame_size * channels];
		// SAFETY: `inner` owns a live OpusDecoder; packet/out slices are bounded by
		// the lengths we pass.
		let samples = unsafe {
			opus_decode_float(
				&mut *self.inner,
				packet.as_ptr(),
				packet.len() as i32,
				out.as_mut_ptr(),
				self.max_frame_size as i32,
				0,
			)
		};
		if samples < 0 {
			return Err(opus::decode_error(samples));
		}
		out.truncate(samples as usize * channels);

		let activity = opus::activity(packet, self.in_dtx);
		self.in_dtx = activity.is_dtx();
		Ok(Decoded { samples: out, activity })
	}

	fn reset(&mut self) -> Result<(), Error> {
		// SAFETY: `inner` owns a live decoder and OPUS_RESET_STATE takes no arguments.
		let rc = unsafe { opus_decoder_ctl_impl(self.inner, OPUS_RESET_STATE, varargs![]) };
		if rc != OPUS_OK {
			return Err(opus::error(rc, "OPUS_RESET_STATE"));
		}
		self.in_dtx = false;
		Ok(())
	}

	fn sample_rate(&self) -> u32 {
		self.sample_rate
	}

	fn layout(&self) -> Layout {
		self.layout
	}

	fn delay(&self) -> usize {
		self.pre_skip
	}

	fn name(&self) -> &str {
		NAME
	}
}

impl Drop for Libopus {
	fn drop(&mut self) {
		// SAFETY: `inner` is a live OpusDecoder that nothing else aliases.
		unsafe { opus_decoder_destroy(self.inner) };
	}
}
