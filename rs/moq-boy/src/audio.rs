use anyhow::{Context, Result};

/// Audio encoding pipeline: PCM samples -> Opus -> MoQ.
///
/// Uses ffmpeg-next for Opus encoding (same dependency as video H.264).
pub struct AudioEncoder {
	opus: moq_mux::import::Opus,
	ffmpeg_encoder: ffmpeg_next::encoder::audio::Encoder,
	resampler: Option<ffmpeg_next::software::resampling::Context>,
	sample_buffer: Vec<i16>,
	frame_size: usize,
	frame_count: u64,
	input_sample_rate: u32,
}

/// Target Opus sample rate.
const OPUS_SAMPLE_RATE: u32 = 48000;
/// Opus frame duration: 20ms at 48kHz = 960 samples per channel.
const OPUS_FRAME_SAMPLES: usize = 960;
/// GB APU outputs stereo.
const CHANNELS: u32 = 2;

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
		enc.set_bit_rate(64000);

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
			sample_buffer: Vec::new(),
			frame_size: if frame_size > 0 { frame_size } else { OPUS_FRAME_SAMPLES },
			frame_count: 0,
			input_sample_rate,
		})
	}

	/// Returns a reference to the underlying track producer.
	pub fn track(&self) -> &moq_lite::TrackProducer {
		self.opus.track()
	}

	/// Feed interleaved stereo u8 samples from the emulator.
	/// Boytacean outputs unsigned 8-bit PCM (0-255, center at 128).
	pub fn push_samples(&mut self, samples: &[u8]) -> Result<()> {
		// Convert u8 (unsigned, center=128) to i16 (signed, center=0).
		let i16_samples: Vec<i16> = samples.iter().map(|&s| ((s as i16) - 128) * 256).collect();
		self.sample_buffer.extend_from_slice(&i16_samples);

		// Process full frames worth of samples.
		let samples_per_frame = self.frame_size * CHANNELS as usize;

		while self.sample_buffer.len() >= samples_per_frame {
			let frame_samples: Vec<i16> = self.sample_buffer.drain(..samples_per_frame).collect();
			self.encode_frame(&frame_samples)?;
		}

		Ok(())
	}

	fn encode_frame(&mut self, samples: &[i16]) -> Result<()> {
		// Create an audio frame with interleaved i16 samples.
		let mut frame = ffmpeg_next::frame::Audio::new(
			ffmpeg_next::format::Sample::I16(ffmpeg_next::format::sample::Type::Packed),
			self.frame_size,
			ffmpeg_next::ChannelLayout::STEREO,
		);
		frame.set_rate(self.input_sample_rate);
		frame.set_pts(Some(self.frame_count as i64 * self.frame_size as i64));

		// Copy sample data into the frame.
		let data = frame.data_mut(0);
		let bytes: &[u8] = unsafe { std::slice::from_raw_parts(samples.as_ptr() as *const u8, samples.len() * 2) };
		data[..bytes.len()].copy_from_slice(bytes);

		// Resample if needed (different sample rate), otherwise encode directly.
		let frame_to_encode = if let Some(resampler) = &mut self.resampler {
			let mut resampled = ffmpeg_next::frame::Audio::empty();
			resampler.run(&frame, &mut resampled)?;
			resampled
		} else {
			frame
		};

		self.ffmpeg_encoder.send_frame(&frame_to_encode)?;

		let mut pkt = ffmpeg_next::Packet::empty();
		while self.ffmpeg_encoder.receive_packet(&mut pkt).is_ok() {
			if let Some(data) = pkt.data() {
				let ts = hang::container::Timestamp::from_micros(
					self.frame_count * (self.frame_size as u64 * 1_000_000 / OPUS_SAMPLE_RATE as u64),
				)?;
				self.opus.decode(&mut &*data, Some(ts))?;
			}
		}

		self.frame_count += 1;
		Ok(())
	}
}
