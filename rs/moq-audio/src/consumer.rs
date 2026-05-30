//! Subscribe to an encoded audio track and emit raw PCM.

use bytes::Bytes;

use crate::codec::{Decoder, DecoderOutput};
use crate::resample::Resampler;
use crate::{AudioError, Frame};

/// Subscribe to a moq-mux audio track and emit decoded PCM in the
/// format declared by [`DecoderOutput`].
///
/// Output format / sample rate / channel count are fixed at
/// construction; [`read`](Self::read) returns plain [`Frame`]s.
pub struct AudioConsumer {
	decoder: Decoder,
	track: moq_mux::container::Consumer<moq_mux::container::legacy::Wire>,
	resampler: Option<Resampler>,
	output: DecoderOutput,
	resolved_sample_rate: u32,
	resolved_channels: u32,
}

impl AudioConsumer {
	/// Subscribe to `name` in `broadcast` using the catalog entry to
	/// pick the codec.
	pub fn new(
		broadcast: &moq_net::BroadcastConsumer,
		catalog: &hang::catalog::AudioConfig,
		name: impl Into<String>,
		output: DecoderOutput,
	) -> Result<Self, AudioError> {
		let decoder = Decoder::new(catalog)?;
		let sample_rate = output.sample_rate.unwrap_or_else(|| decoder.sample_rate());
		let channels = output.channels.unwrap_or_else(|| decoder.channel_count());

		if channels != decoder.channel_count() {
			return Err(AudioError::Unsupported(format!(
				"channel remapping not implemented (decoder {}ch, requested {channels}ch)",
				decoder.channel_count()
			)));
		}

		let resampler = if sample_rate == decoder.sample_rate() {
			None
		} else {
			let chunk_frames = (decoder.sample_rate() as usize * 20) / 1000;
			Some(Resampler::new(
				decoder.sample_rate(),
				sample_rate,
				decoder.channel_count(),
				chunk_frames,
			)?)
		};

		let name = name.into();
		let track = broadcast.subscribe_track(&moq_net::Track {
			name,
			..Default::default()
		})?;
		let mut track = moq_mux::container::Consumer::new(track, moq_mux::container::legacy::Wire);
		if let Some(latency) = output.latency_max {
			track = track.with_latency(latency);
		}

		Ok(Self {
			decoder,
			track,
			resampler,
			output,
			resolved_sample_rate: sample_rate,
			resolved_channels: channels,
		})
	}

	pub fn output(&self) -> &DecoderOutput {
		&self.output
	}

	/// Sample rate samples are actually delivered at (= `output.sample_rate`
	/// if set, otherwise the decoder's native rate).
	pub fn sample_rate(&self) -> u32 {
		self.resolved_sample_rate
	}

	/// Channel count samples are actually delivered at.
	pub fn channels(&self) -> u32 {
		self.resolved_channels
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

		let bytes = self.output.format.from_interleaved_f32(&pcm, self.resolved_channels)?;
		Ok(Some(Frame {
			timestamp_us: ts_us,
			data: Bytes::from(bytes),
		}))
	}
}
