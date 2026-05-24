//! Subscribe to an encoded audio track and emit raw PCM.

use bytes::Bytes;

use crate::codec::{Decoder, DecoderConfig};
use crate::resample::Resampler;
use crate::{AudioError, Frame};

/// Subscribe to a moq-mux audio track and emit decoded PCM in the
/// format declared by [`DecoderConfig`].
///
/// Output format / sample rate / channel count are fixed at
/// construction; [`read`](Self::read) returns plain [`Frame`]s.
pub struct AudioConsumer {
	decoder: Decoder,
	track: moq_mux::container::Consumer<moq_mux::container::legacy::Wire>,
	resampler: Option<Resampler>,
	config: DecoderConfig,
	output_rate: u32,
	output_channels: u32,
}

impl AudioConsumer {
	/// Subscribe to `name` in `broadcast` using the catalog entry to
	/// pick the codec.
	pub fn new(
		broadcast: &moq_net::BroadcastConsumer,
		catalog: &hang::catalog::AudioConfig,
		name: impl Into<String>,
		config: DecoderConfig,
	) -> Result<Self, AudioError> {
		let decoder = Decoder::new(catalog)?;
		let output_rate = config.output_sample_rate.unwrap_or_else(|| decoder.sample_rate());
		let output_channels = config.output_channels.unwrap_or_else(|| decoder.channel_count());

		if output_channels != decoder.channel_count() {
			return Err(AudioError::Unsupported(format!(
				"channel remapping not implemented (decoder {}ch, requested {output_channels}ch)",
				decoder.channel_count()
			)));
		}

		let resampler = if output_rate == decoder.sample_rate() {
			None
		} else {
			let chunk_frames = (decoder.sample_rate() as usize * 20) / 1000;
			Some(Resampler::new(
				decoder.sample_rate(),
				output_rate,
				decoder.channel_count(),
				chunk_frames,
			)?)
		};

		let name = name.into();
		let track = broadcast.subscribe_track(&moq_net::Track { name, priority: 0 })?;
		let track = moq_mux::container::Consumer::new(track, moq_mux::container::legacy::Wire);

		Ok(Self {
			decoder,
			track,
			resampler,
			config,
			output_rate,
			output_channels,
		})
	}

	pub fn config(&self) -> &DecoderConfig {
		&self.config
	}

	pub fn output_rate(&self) -> u32 {
		self.output_rate
	}

	pub fn output_channels(&self) -> u32 {
		self.output_channels
	}

	/// Read the next decoded PCM frame, or `None` when the track ends.
	pub async fn read(&mut self) -> Result<Option<Frame>, AudioError> {
		let Some(mux_frame) = self.track.read().await? else {
			return Ok(None);
		};

		let ts_us: u64 = mux_frame
			.timestamp
			.as_micros()
			.try_into()
			.map_err(|_| AudioError::Unsupported("timestamp overflow".into()))?;

		let decoded = self.decoder.decode_f32(&mux_frame.payload)?;
		let pcm = match self.resampler.as_mut() {
			Some(r) => r.process(&decoded)?,
			None => decoded,
		};

		let bytes = self
			.config
			.output_format
			.from_interleaved_f32(&pcm, self.output_channels)?;
		Ok(Some(Frame {
			timestamp_us: ts_us,
			data: Bytes::from(bytes),
		}))
	}
}
