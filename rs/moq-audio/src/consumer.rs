//! Subscribe to an encoded audio track and emit raw PCM.

use bytes::Bytes;

use crate::codec::Decoder;
use crate::{AudioError, AudioFormat, AudioSamples};

#[cfg(feature = "opus")]
use crate::codec::OpusDecoder;

#[cfg(feature = "resample")]
use crate::resample::Resampler;

/// Subscribe to a moq-mux audio track and emit decoded PCM in the caller's chosen format.
pub struct AudioConsumer {
	decoder: Box<dyn Decoder>,
	track: moq_mux::container::Consumer<moq_mux::container::legacy::Wire>,

	output_format: AudioFormat,
	output_rate: u32,
	output_channels: u32,

	#[cfg(feature = "resample")]
	resampler: Option<Resampler>,

	/// Leftover decoded samples (in the *output* rate/channels, interleaved f32)
	/// from the resampler that haven't been requested yet.
	#[cfg(feature = "resample")]
	pending: Vec<f32>,

	/// Timestamp of the next packet we'll deliver, sourced from the moq-mux frame.
	#[cfg(feature = "resample")]
	next_timestamp_us: u64,
}

impl AudioConsumer {
	/// Subscribe to `name` in `broadcast` using the catalog entry to pick the codec.
	#[cfg(feature = "opus")]
	pub fn subscribe_opus(
		broadcast: &moq_net::BroadcastConsumer,
		config: &hang::catalog::AudioConfig,
		name: impl Into<String>,
		output_format: AudioFormat,
		output_rate: Option<u32>,
		output_channels: Option<u32>,
	) -> Result<Self, AudioError> {
		let decoder = OpusDecoder::from_config(config)?;
		Self::subscribe(
			broadcast,
			name,
			Box::new(decoder),
			output_format,
			output_rate,
			output_channels,
		)
	}

	/// Subscribe with a caller-supplied decoder.
	pub fn subscribe(
		broadcast: &moq_net::BroadcastConsumer,
		name: impl Into<String>,
		decoder: Box<dyn Decoder>,
		output_format: AudioFormat,
		output_rate: Option<u32>,
		output_channels: Option<u32>,
	) -> Result<Self, AudioError> {
		let name = name.into();
		let track = broadcast.subscribe_track(&moq_net::Track { name, priority: 0 })?;
		let track = moq_mux::container::Consumer::new(track, moq_mux::container::legacy::Wire);

		let output_rate = output_rate.unwrap_or_else(|| decoder.sample_rate());
		let output_channels = output_channels.unwrap_or_else(|| decoder.channel_count());

		#[cfg(feature = "resample")]
		let resampler = if output_rate == decoder.sample_rate() && output_channels == decoder.channel_count() {
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

		#[cfg(not(feature = "resample"))]
		if output_rate != decoder.sample_rate() || output_channels != decoder.channel_count() {
			return Err(AudioError::Unsupported(format!(
				"output {output_rate}Hz/{output_channels}ch does not match decoder {}Hz/{}ch and `resample` feature is disabled",
				decoder.sample_rate(),
				decoder.channel_count(),
			)));
		}

		Ok(Self {
			decoder,
			track,
			output_format,
			output_rate,
			output_channels,
			#[cfg(feature = "resample")]
			resampler,
			#[cfg(feature = "resample")]
			pending: Vec::new(),
			#[cfg(feature = "resample")]
			next_timestamp_us: 0,
		})
	}

	pub fn output_format(&self) -> AudioFormat {
		self.output_format
	}

	pub fn output_rate(&self) -> u32 {
		self.output_rate
	}

	pub fn output_channels(&self) -> u32 {
		self.output_channels
	}

	/// Read the next decoded PCM buffer, or `None` when the track ends.
	pub async fn read(&mut self) -> Result<Option<AudioSamples>, AudioError> {
		let Some(frame) = self.track.read().await? else {
			return self.flush();
		};

		let ts_us: u64 = frame
			.timestamp
			.as_micros()
			.try_into()
			.map_err(|_| AudioError::Unsupported("timestamp overflow".into()))?;
		#[cfg(feature = "resample")]
		{
			self.next_timestamp_us = ts_us;
		}

		let decoded = self.decoder.decode(&frame.payload)?;

		#[cfg(feature = "resample")]
		let pcm = match self.resampler.as_mut() {
			Some(r) => {
				let mut out = r.process(&decoded)?;
				if !self.pending.is_empty() {
					let mut merged = std::mem::take(&mut self.pending);
					merged.append(&mut out);
					merged
				} else {
					out
				}
			}
			None => decoded,
		};

		#[cfg(not(feature = "resample"))]
		let pcm = decoded;

		if pcm.is_empty() {
			// Resampler buffered everything internally; caller should poll again.
			return Ok(Some(AudioSamples {
				format: self.output_format,
				sample_rate: self.output_rate,
				channel_count: self.output_channels,
				timestamp_us: ts_us,
				data: Bytes::new(),
			}));
		}

		let bytes = self.output_format.from_interleaved_f32(&pcm, self.output_channels)?;
		Ok(Some(AudioSamples {
			format: self.output_format,
			sample_rate: self.output_rate,
			channel_count: self.output_channels,
			timestamp_us: ts_us,
			data: Bytes::from(bytes),
		}))
	}

	#[cfg(feature = "resample")]
	fn flush(&mut self) -> Result<Option<AudioSamples>, AudioError> {
		if self.pending.is_empty() {
			return Ok(None);
		}
		let pcm = std::mem::take(&mut self.pending);
		let bytes = self.output_format.from_interleaved_f32(&pcm, self.output_channels)?;
		Ok(Some(AudioSamples {
			format: self.output_format,
			sample_rate: self.output_rate,
			channel_count: self.output_channels,
			timestamp_us: self.next_timestamp_us,
			data: Bytes::from(bytes),
		}))
	}

	#[cfg(not(feature = "resample"))]
	fn flush(&mut self) -> Result<Option<AudioSamples>, AudioError> {
		Ok(None)
	}
}
