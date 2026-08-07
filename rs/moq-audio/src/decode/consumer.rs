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
	track: moq_mux::container::Consumer<moq_mux::catalog::hang::Container>,
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
		// The catalog says how the track is framed, and it is not always the legacy
		// wire: `moq import fmp4` publishes CMAF. Reading a moof+mdat fragment as a
		// varint timestamp plus a payload decodes to garbage rather than failing.
		let container = moq_mux::catalog::hang::Container::try_from(&catalog.container)?;
		let mut track = moq_mux::container::Consumer::new(track, container);
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

	#[tokio::test]
	async fn reads_the_container_the_catalog_declares() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast.create_track("audio", hang::container::track_info()).unwrap();
		let subscriber = broadcast.consume();

		let mut catalog = hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Pcm, 48_000, 1);
		catalog.container = hang::catalog::Container::Loc;

		let mut producer = moq_mux::container::Producer::new(track, moq_mux::catalog::hang::Container::Loc);
		let mut consumer = Consumer::new(
			&subscriber,
			&catalog,
			"audio",
			Config {
				format: Format::F32,
				..Config::new()
			},
		)
		.await
		.unwrap();

		let samples = [0.25f32, -0.5, 0.75, -1.0];
		let payload: Vec<u8> = samples.iter().flat_map(|sample| sample.to_le_bytes()).collect();
		producer
			.write(moq_mux::container::Frame {
				timestamp: Timestamp::ZERO,
				duration: None,
				payload: payload.into(),
				keyframe: true,
			})
			.unwrap();

		let frame = consumer.read().await.unwrap().expect("decoded frame");
		assert_eq!(
			Format::F32.as_interleaved_f32(&frame.data, 1).unwrap().as_ref(),
			samples
		);
	}

	#[tokio::test]
	async fn reads_cmaf_container_declared_by_catalog() {
		let mut source_broadcast = moq_net::broadcast::Info::new().produce();
		let source_subscriber = source_broadcast.consume();
		let source_catalog = moq_mux::catalog::Producer::new(&mut source_broadcast).unwrap();
		let input = Input {
			format: Format::F32,
			sample_rate: 48_000,
			channels: 1,
		};
		let options = Options {
			track: Some("audio".to_string()),
			..Options::default()
		};
		let mut producer = Producer::new(&mut source_broadcast, source_catalog, input, &options).unwrap();

		let samples = vec![0.25f32; 960];
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

		let origin = moq_net::Origin::random().produce();
		let mut requests = origin.dynamic();
		let served = source_subscriber.clone();
		tokio::spawn(async move {
			while let Ok(request) = requests.requested_broadcast().await {
				request.accept(served.clone());
			}
		});
		let catalog = moq_mux::catalog::Consumer::<()>::new(&source_subscriber, moq_mux::catalog::CatalogFormat::Hang)
			.await
			.unwrap();
		let source = moq_mux::Source::new(origin.consume(), "test");
		let mut export = moq_mux::container::fmp4::Export::new(source, catalog);
		let init = export.next().await.unwrap().expect("CMAF init");
		let fragment = export.next().await.unwrap().expect("CMAF fragment");

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let subscriber = broadcast.consume();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast).unwrap();
		let mut import = moq_mux::container::fmp4::Import::new(broadcast, catalog.reserve());
		import.decode(&init).unwrap();
		import.decode(&fragment).unwrap();

		let snapshot = catalog.snapshot();
		let (name, config) = snapshot.audio.renditions.iter().next().expect("audio rendition");
		assert!(matches!(config.container, hang::catalog::Container::Cmaf { .. }));
		let mut consumer = Consumer::new(
			&subscriber,
			config,
			name,
			Config {
				format: Format::F32,
				..Config::new()
			},
		)
		.await
		.unwrap();

		let frame = consumer.read().await.unwrap().expect("decoded frame");
		assert_eq!(frame.timestamp.as_micros(), 0);
		assert!(!frame.data.is_empty());
	}
}
