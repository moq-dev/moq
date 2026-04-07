//! Audio encoding pipeline: PCM samples -> resample -> Opus -> MoQ.
//!
//! The Game Boy APU outputs unsigned 8-bit stereo PCM at ~44.1kHz.
//! This module resamples to 48kHz and encodes to Opus (20ms frames)
//! using ffmpeg-next, then publishes via `moq_mux::import::Opus`.
//!
//! Audio timestamps are anchored to the same wall clock as video,
//! ensuring A/V sync. The epoch is set on the first `push_samples()`
//! call and reset on pause/resume.

use anyhow::{Context, Result};

/// Audio encoding pipeline: PCM samples -> Opus -> MoQ.
///
/// Uses ffmpeg-next for Opus encoding (same dependency as video H.264).
pub struct AudioEncoder {
	opus: moq_mux::import::Opus,
	ffmpeg_encoder: ffmpeg_next::encoder::audio::Encoder,

	resampler: Option<ffmpeg_next::software::resampling::Context>,

	/// Input samples at input_sample_rate, waiting to be resampled.
	input_buffer: Vec<i16>,
	/// Resampled samples at OPUS_SAMPLE_RATE, waiting to be encoded.
	encode_buffer: Vec<i16>,

	frame_size: usize,
	frame_count: u64,
	input_sample_rate: u32,

	/// Set once on the first `push_samples()` call.
	///
	/// Audio timestamps are computed as: `epoch + frame_count * frame_duration`.
	/// This produces exactly contiguous frames with no gaps, regardless of when
	/// `push_samples()` is called. The epoch accounts for samples already
	/// buffered (not yet encoded) at the time of initialization:
	///
	/// ```text
	/// epoch = wall_clock_elapsed - (buffered_samples / sample_rate)
	/// ```
	///
	/// This ensures the first encoded frame's PTS matches where it would have
	/// been if encoding had started from the very beginning.
	epoch: Option<u64>,
}

/// Target Opus sample rate (standard for Opus).
const OPUS_SAMPLE_RATE: u32 = 48000;
/// Opus frame duration: 20ms at 48kHz = 960 samples per channel.
const OPUS_FRAME_SAMPLES: usize = 960;
/// Game Boy APU outputs stereo audio.
const CHANNELS: u32 = 2;
/// Opus encoding bitrate. 64kbps is reasonable for stereo Game Boy
/// audio (simple waveforms, limited frequency range).
const OPUS_BITRATE: usize = 64000;

impl AudioEncoder {
	pub fn new(
		broadcast: moq_lite::BroadcastProducer,
		catalog: moq_mux::CatalogProducer,
		input_sample_rate: u32,
	) -> Result<Self> {
		let opus = moq_mux::import::Opus::new(
			broadcast,
			catalog,
			moq_mux::import::OpusConfig {
				sample_rate: OPUS_SAMPLE_RATE,
				channel_count: CHANNELS,
			},
		)?;

		// Set up ffmpeg Opus encoder with s16 (signed 16-bit interleaved) format.
		// libopus only supports s16 and flt (both packed/interleaved).
		let codec = ffmpeg_next::encoder::find(ffmpeg_next::codec::Id::OPUS).context("Opus encoder not found")?;
		let ctx = ffmpeg_next::codec::Context::new_with_codec(codec);
		let mut enc = ctx.encoder().audio()?;
		enc.set_rate(OPUS_SAMPLE_RATE as i32);
		enc.set_format(ffmpeg_next::format::Sample::I16(
			ffmpeg_next::format::sample::Type::Packed,
		));
		enc.set_channel_layout(ffmpeg_next::ChannelLayout::STEREO);
		enc.set_time_base(ffmpeg_next::Rational::new(1, OPUS_SAMPLE_RATE as i32));
		enc.set_bit_rate(OPUS_BITRATE);

		let ffmpeg_encoder = enc.open()?;
		let frame_size = ffmpeg_encoder.frame_size() as usize;

		// Set up resampler if input rate differs from Opus rate.
		let resampler = if input_sample_rate != OPUS_SAMPLE_RATE {
			Some(ffmpeg_next::software::resampling::Context::get(
				ffmpeg_next::format::Sample::I16(ffmpeg_next::format::sample::Type::Packed),
				ffmpeg_next::ChannelLayout::STEREO,
				input_sample_rate,
				ffmpeg_next::format::Sample::I16(ffmpeg_next::format::sample::Type::Packed),
				ffmpeg_next::ChannelLayout::STEREO,
				OPUS_SAMPLE_RATE,
			)?)
		} else {
			None
		};

		Ok(Self {
			opus,
			ffmpeg_encoder,
			resampler,
			input_buffer: Vec::new(),
			encode_buffer: Vec::new(),
			frame_size: if frame_size > 0 { frame_size } else { OPUS_FRAME_SAMPLES },
			frame_count: 0,
			input_sample_rate,
			epoch: None,
		})
	}

	/// Returns a reference to the underlying track producer.
	pub fn track(&self) -> &moq_lite::TrackProducer {
		self.opus.track()
	}

	/// Reset the epoch so audio timestamps re-anchor to wall clock on the next push.
	/// Call this on pause/resume so the gap appears in audio PTS too.
	/// Also drains buffered samples so stale pre-pause audio isn't encoded
	/// with post-pause timestamps.
	pub fn reset_epoch(&mut self) {
		self.epoch = None;
		self.frame_count = 0;
		self.input_buffer.clear();
		self.encode_buffer.clear();

		// Flush the resampler's internal delay buffer so pre-pause samples
		// don't leak into post-pause audio.
		if let Some(resampler) = &mut self.resampler {
			let mut flushed = ffmpeg_next::frame::Audio::empty();
			let _ = resampler.flush(&mut flushed);
		}
	}

	/// Feed interleaved stereo u8 samples from the emulator.
	/// Boytacean outputs unsigned 8-bit PCM (0-255, center at 128).
	///
	/// `elapsed` is the wall-clock time since the emulator started, shared with
	/// the video encoder so audio and video PTS stay aligned.
	pub fn push_samples(&mut self, samples: &[u8], elapsed: std::time::Duration) -> Result<()> {
		// Convert u8 (unsigned, center=128) to i16 (signed, center=0).
		let i16_samples: Vec<i16> = samples.iter().map(|&s| ((s as i16) - 128) * 256).collect();

		self.input_buffer.extend_from_slice(&i16_samples);

		// Resample input to OPUS_SAMPLE_RATE first, then encode in frame_size chunks.
		self.resample()?;

		let samples_per_frame = self.frame_size * CHANNELS as usize;
		let frame_duration_us = self.frame_size as u64 * 1_000_000 / OPUS_SAMPLE_RATE as u64;

		// Initialize epoch on first call so audio timestamps align with video.
		// Subtract buffered time so the first frame's PTS accounts for samples
		// that were accumulated before encoding begins.
		if self.epoch.is_none() && self.encode_buffer.len() >= samples_per_frame {
			let buffered_us = self.encode_buffer.len() as u64 * 1_000_000 / (OPUS_SAMPLE_RATE as u64 * CHANNELS as u64);
			self.epoch = Some((elapsed.as_micros() as u64).saturating_sub(buffered_us));
		}

		while self.encode_buffer.len() >= samples_per_frame {
			let frame_samples: Vec<i16> = self.encode_buffer.drain(..samples_per_frame).collect();
			let ts_micros = self.epoch.unwrap() + self.frame_count * frame_duration_us;
			self.encode_frame(&frame_samples, ts_micros)?;
		}

		Ok(())
	}

	/// Resample all pending input samples to OPUS_SAMPLE_RATE and append to encode_buffer.
	fn resample(&mut self) -> Result<()> {
		let Some(resampler) = &mut self.resampler else {
			// No resampling needed; move input directly to encode buffer.
			self.encode_buffer.append(&mut self.input_buffer);
			return Ok(());
		};

		if self.input_buffer.is_empty() {
			return Ok(());
		}

		let nb_samples = self.input_buffer.len() / CHANNELS as usize;
		let mut frame = ffmpeg_next::frame::Audio::new(
			ffmpeg_next::format::Sample::I16(ffmpeg_next::format::sample::Type::Packed),
			nb_samples,
			ffmpeg_next::ChannelLayout::STEREO,
		);
		frame.set_rate(self.input_sample_rate);

		// Copy i16 samples into the frame's byte buffer.
		// SAFETY: i16 is always 2 bytes, little-endian on all supported platforms.
		// The source and destination buffers are properly aligned (Vec<i16> guarantees
		// alignment, and we're reading as bytes which has no alignment requirement).
		let data = frame.data_mut(0);
		let bytes: &[u8] =
			unsafe { std::slice::from_raw_parts(self.input_buffer.as_ptr() as *const u8, self.input_buffer.len() * 2) };
		data[..bytes.len()].copy_from_slice(bytes);
		self.input_buffer.clear();

		// Pre-allocate the output frame with the correct size.
		// ffmpeg-next's run() has a bug: it allocates output with input.samples()
		// instead of computing the correct count for rate conversion.
		let delay = resampler.delay().map(|d| d.input as u64).unwrap_or(0);
		let out_samples =
			((nb_samples as u64 + delay) * OPUS_SAMPLE_RATE as u64).div_ceil(self.input_sample_rate as u64);

		let mut resampled = ffmpeg_next::frame::Audio::new(
			ffmpeg_next::format::Sample::I16(ffmpeg_next::format::sample::Type::Packed),
			out_samples as usize,
			ffmpeg_next::ChannelLayout::STEREO,
		);
		resampler.run(&frame, &mut resampled)?;

		// Extract resampled i16 samples from the frame's byte buffer.
		// SAFETY: Same as above — reinterpreting the frame's u8 data as i16.
		// ffmpeg guarantees the audio data is in s16 packed format (set above),
		// so the byte layout is valid i16 values.
		let out_samples = resampled.samples() * CHANNELS as usize;
		let out_data = resampled.data(0);
		let out_i16: &[i16] = unsafe { std::slice::from_raw_parts(out_data.as_ptr() as *const i16, out_samples) };
		self.encode_buffer.extend_from_slice(out_i16);

		Ok(())
	}

	fn encode_frame(&mut self, samples: &[i16], ts_micros: u64) -> Result<()> {
		// Create an audio frame at OPUS_SAMPLE_RATE (already resampled).
		let mut frame = ffmpeg_next::frame::Audio::new(
			ffmpeg_next::format::Sample::I16(ffmpeg_next::format::sample::Type::Packed),
			self.frame_size,
			ffmpeg_next::ChannelLayout::STEREO,
		);
		frame.set_rate(OPUS_SAMPLE_RATE);
		frame.set_pts(Some(self.frame_count as i64 * self.frame_size as i64));

		// SAFETY: Same as resample() — copying i16 data as bytes into the frame.
		let data = frame.data_mut(0);
		let bytes: &[u8] = unsafe { std::slice::from_raw_parts(samples.as_ptr() as *const u8, samples.len() * 2) };
		data[..bytes.len()].copy_from_slice(bytes);

		self.ffmpeg_encoder.send_frame(&frame)?;

		let mut pkt = ffmpeg_next::Packet::empty();
		while self.ffmpeg_encoder.receive_packet(&mut pkt).is_ok() {
			if let Some(data) = pkt.data() {
				let ts = hang::container::Timestamp::from_micros(ts_micros)?;
				self.opus.decode(&mut &*data, Some(ts))?;
			}
		}

		self.frame_count += 1;

		Ok(())
	}
}
