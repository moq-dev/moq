//! Subscribe to an encoded audio track and emit raw PCM.

use bytes::Bytes;

use super::decoder::{Config, Decoder};
use crate::resample::{Resampler, remix};
use crate::{Error, Frame};

/// Subscribe to a moq-mux audio track and emit decoded PCM in the layout
/// declared by [`Config`].
///
/// The mirror of [`encode::Producer`](crate::encode::Producer): output format /
/// sample rate / channel count are fixed at construction, and
/// [`read`](Self::read) returns plain [`Frame`]s.
pub struct Consumer {
	decoder: Decoder,
	track: moq_mux::container::Consumer<moq_mux::container::legacy::Wire>,
	resampler: Option<Resampler>,
	config: Config,
	resolved_sample_rate: u32,
	resolved_channels: u32,
}

impl Consumer {
	/// Subscribe to `name` in `broadcast`, using the catalog entry to pick the
	/// codec.
	pub async fn new(
		broadcast: &moq_net::broadcast::Consumer,
		catalog: &hang::catalog::AudioConfig,
		name: impl Into<String>,
		config: Config,
	) -> Result<Self, Error> {
		let decoder = Decoder::new(catalog)?;
		let sample_rate = config.sample_rate.unwrap_or_else(|| decoder.sample_rate());
		let channels = config.channels.unwrap_or_else(|| decoder.channel_count());
		crate::opus::validate_channels(channels)?;

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
		let track = broadcast
			.track(&name)?
			.subscribe(moq_net::track::Subscription::default().with_priority(hang::catalog::PRIORITY.audio))
			.await?;
		let mut track = moq_mux::container::Consumer::new(track, moq_mux::container::legacy::Wire);
		if let Some(latency) = config.latency_max {
			track = track.with_latency(latency);
		}

		Ok(Self {
			decoder,
			track,
			resampler,
			config,
			resolved_sample_rate: sample_rate,
			resolved_channels: channels,
		})
	}

	/// The config this consumer was built with.
	pub fn config(&self) -> &Config {
		&self.config
	}

	/// Sample rate samples are actually delivered at, which is
	/// [`Config::sample_rate`] resolved against the catalog.
	pub fn sample_rate(&self) -> u32 {
		self.resolved_sample_rate
	}

	/// Channel count samples are actually delivered at, which is
	/// [`Config::channels`] resolved against the catalog.
	pub fn channels(&self) -> u32 {
		self.resolved_channels
	}

	/// Read the next decoded PCM frame, or `None` when the track ends.
	pub async fn read(&mut self) -> Result<Option<Frame>, Error> {
		let Some(mux_frame) = self.track.read().await? else {
			return Ok(None);
		};

		let decoded = self.decoder.decode(&mux_frame.payload)?;
		let pcm = match self.resampler.as_mut() {
			Some(r) => r.process(&decoded)?,
			None => decoded,
		};
		let pcm = if self.decoder.channel_count() == self.resolved_channels {
			pcm
		} else {
			remix(&pcm, self.decoder.channel_count(), self.resolved_channels)?
		};

		let bytes = self.config.format.from_interleaved_f32(&pcm, self.resolved_channels)?;
		Ok(Some(Frame {
			timestamp: mux_frame.timestamp,
			data: Bytes::from(bytes),
		}))
	}
}

#[cfg(test)]
mod tests {
	use moq_net::Timestamp;

	use super::*;
	use crate::Format;
	use crate::encode::{Encoder, Input, Options, Producer};

	#[tokio::test]
	async fn remixes_mono_stream_to_stereo_output() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast).unwrap();
		let subscriber = broadcast.consume();
		let input = Input {
			format: Format::F32,
			sample_rate: 48_000,
			channels: 1,
		};
		let options = Options {
			track: Some("audio".to_string()),
			..Options::default()
		};
		let mut producer = Producer::new(&mut broadcast, catalog, input.clone(), &options).unwrap();
		let catalog = Encoder::new(&crate::encode::Config::new(input)).unwrap().catalog();
		let mut consumer = Consumer::new(
			&subscriber,
			&catalog,
			"audio",
			Config {
				channels: Some(2),
				..Config::new()
			},
		)
		.await
		.unwrap();

		let samples = vec![0.1f32; 960];
		let mut data = Vec::with_capacity(samples.len() * size_of::<f32>());
		for sample in samples {
			data.extend_from_slice(&sample.to_le_bytes());
		}
		producer
			.write(&Frame {
				timestamp: Timestamp::ZERO,
				data: data.into(),
			})
			.unwrap();

		let frame = consumer.read().await.unwrap().expect("decoded frame");
		let samples = Format::F32.as_interleaved_f32(&frame.data, 2).unwrap();
		assert_eq!(samples.len(), (960 - 312) * 2);
		for pair in samples.chunks_exact(2) {
			assert_eq!(pair[0], pair[1]);
		}
	}
}
