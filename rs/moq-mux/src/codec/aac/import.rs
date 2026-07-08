use super::Config;
use crate::catalog::hang::CatalogExt;
use crate::container::Frame;

/// AAC importer.
///
/// The catalog comes from an AudioSpecificConfig (variable-length, typically extracted from an MP4
/// ESDS atom) passed to [`initialize`](Self::initialize), or from the [`new`](Self::new) hint. Each
/// packet passed to [`decode`](Self::decode) is published as one hang frame in its own group, so the
/// relay can forward each frame without waiting for a group boundary. The codec's packet loss
/// concealment handles drops.
pub struct Import<E: CatalogExt = ()> {
	track: crate::container::Producer<crate::catalog::hang::Container>,
	rendition: crate::catalog::AudioTrack<E>,
	/// The published config, so `initialize` (or a re-init) doesn't re-publish an unchanged catalog.
	config: Option<hang::catalog::AudioConfig>,
}

impl<E: CatalogExt> Import<E> {
	/// Publish on an existing track producer, seeding the rendition from `hint` (pass
	/// [`AudioHint::default`](crate::catalog::AudioHint) for none).
	///
	/// The catalog rendition publishes as soon as the config is known: up front when the hint carries
	/// the codec (with its profile), sample rate, and channel count, otherwise once
	/// [`initialize`](Self::initialize) parses an AudioSpecificConfig. A codec [`Config`] converts
	/// into a hint via `into()`.
	pub fn new(
		track: moq_net::track::Producer,
		reserved: crate::catalog::Reserved<E>,
		hint: crate::catalog::AudioHint,
	) -> crate::Result<Self> {
		let initial = hint.to_config()?;
		let rendition = reserved.audio_with_hint(track.name(), hint);
		let mut import = Self {
			track: crate::container::Producer::new(track, crate::catalog::hang::Container::Legacy),
			rendition,
			config: None,
		};
		if let Some(config) = initial {
			import.publish(config)?;
		}
		Ok(import)
	}

	/// Resolve the config from an AudioSpecificConfig, publishing the rendition. A no-op on an empty
	/// buffer, and unnecessary when the hint already carried the codec, sample rate, and channels.
	pub fn initialize(&mut self, data: &[u8]) -> crate::Result<()> {
		if data.is_empty() {
			return Ok(());
		}
		let mut cursor = data;
		let config = Config::parse(&mut cursor)?;
		let audio_config = hang::catalog::AudioConfig::new(
			hang::catalog::AAC {
				profile: config.profile,
			},
			config.sample_rate,
			config.channel_count,
		);
		self.publish(audio_config)
	}

	/// Publish (or re-publish) the resolved config, synthesizing the AudioSpecificConfig `description`
	/// for out-of-band consumers (fMP4/MKV export, WebCodecs) and validating against the hint via
	/// [`Rendition::set`](crate::catalog::Rendition::set). A no-op if unchanged.
	fn publish(&mut self, mut config: hang::catalog::AudioConfig) -> crate::Result<()> {
		config.container = hang::catalog::Container::Legacy;
		if config.description.is_none()
			&& let hang::catalog::AudioCodec::AAC(aac) = &config.codec
		{
			config.description = Some(
				Config {
					profile: aac.profile,
					sample_rate: config.sample_rate,
					channel_count: config.channel_count,
				}
				.encode(),
			);
		}

		if self.config.as_ref() == Some(&config) {
			return Ok(());
		}
		tracing::debug!(name = ?self.track.name(), ?config, "starting track");
		self.rendition.set(config.clone())?;
		self.config = Some(config);
		Ok(())
	}

	/// The MoQ track name this importer publishes on.
	pub fn name(&self) -> &str {
		self.track.name()
	}

	/// A watch-only handle to this track's subscriber demand.
	pub fn demand(&self) -> moq_net::track::Demand {
		self.track.track().demand()
	}

	/// Refine the single audio rendition in place, republishing the catalog.
	///
	/// The TS importer uses this to set the synthesized `description` and an
	/// audio-burst `jitter` once it knows them.
	pub(crate) fn update_rendition(&mut self, f: impl FnOnce(&mut hang::catalog::AudioConfig)) {
		self.rendition.update(f);
	}

	/// Finish the track, flushing the current group.
	pub fn finish(&mut self) -> crate::Result<()> {
		self.rendition.record_group_end(None);
		self.track.finish()?;
		Ok(())
	}

	/// Abort the track with `err` instead of finishing it cleanly, so subscribers
	/// see the real cause rather than [`moq_net::Error::Dropped`].
	pub fn abort(&mut self, err: moq_net::Error) {
		self.track.abort(err);
	}

	/// Close the current group and open the next one at `sequence`.
	pub fn seek(&mut self, sequence: u64) -> crate::Result<()> {
		self.rendition.record_group_end(None);
		self.track.seek(sequence)?;
		Ok(())
	}

	/// Publish one AAC packet as its own group, stamping `pts` or a wall clock when absent.
	pub fn decode<B: moq_net::IntoBytes>(&mut self, frame: B, pts: Option<moq_net::Timestamp>) -> crate::Result<()> {
		let timestamp = self.rendition.timestamp(pts)?;
		self.rendition.record_group_end(Some(timestamp));
		let bytes = frame.as_ref().len();
		self.track.write(Frame {
			timestamp,
			payload: frame.into_bytes(),
			keyframe: true,
			duration: None,
		})?;
		self.track.finish_group()?;
		self.rendition.record_frame(timestamp, bytes);
		Ok(())
	}
}

impl From<Config> for crate::catalog::AudioHint {
	/// Seed a hint from a config resolved out of band (e.g. an ADTS header or gstreamer caps rather
	/// than an AudioSpecificConfig); the importer synthesizes the description from it.
	fn from(config: Config) -> Self {
		crate::catalog::AudioHint {
			codec: Some(
				hang::catalog::AAC {
					profile: config.profile,
				}
				.into(),
			),
			sample_rate: Some(config.sample_rate),
			channel_count: Some(config.channel_count),
			..Default::default()
		}
	}
}
