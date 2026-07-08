use super::Config;
use crate::catalog::hang::CatalogExt;
use crate::container::Frame;

/// Opus importer.
///
/// Publishes raw Opus frames (no Ogg framing) to a single moq track. Build it with
/// [`new`](Self::new), passing the track producer and the
/// [`catalog::Reserved`](crate::catalog::Reserved) it reserves its rendition from.
///
/// Each packet handed to [`decode`](Self::decode) is published in its own group so
/// the relay can forward it immediately without waiting for a group boundary; Opus'
/// packet loss concealment handles drops.
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
	/// the codec, sample rate, and channel count, otherwise once [`initialize`](Self::initialize)
	/// parses an OpusHead. A codec [`Config`] converts into a hint via `into()`.
	pub fn new(
		track: moq_net::track::Producer,
		reserved: crate::catalog::Reserved<E>,
		hint: crate::catalog::AudioHint,
	) -> Self {
		let initial = hint.to_config();
		let rendition = reserved.audio_with_hint(track.name(), hint);
		let mut import = Self {
			track: crate::container::Producer::new(track, crate::catalog::hang::Container::Legacy),
			rendition,
			config: None,
		};
		if let Some(config) = initial {
			import.publish(config);
		}
		import
	}

	/// Resolve the config from an OpusHead, publishing the rendition. A no-op on an empty buffer, and
	/// unnecessary when the hint already carried the sample rate and channel count.
	pub fn initialize(&mut self, data: &[u8]) -> crate::Result<()> {
		if data.is_empty() {
			return Ok(());
		}
		let mut cursor = data;
		let config = Config::parse(&mut cursor)?;
		let mut audio = hang::catalog::AudioConfig::new(
			hang::catalog::AudioCodec::Opus,
			config.sample_rate,
			config.channel_count,
		);
		audio.container = hang::catalog::Container::Legacy;
		self.publish(audio);
		Ok(())
	}

	/// Publish (or re-publish) the resolved config, validating it against the hint via
	/// [`Rendition::set`](crate::catalog::Rendition::set). A no-op if unchanged.
	fn publish(&mut self, config: hang::catalog::AudioConfig) {
		if self.config.as_ref() == Some(&config) {
			return;
		}
		tracing::debug!(name = ?self.track.name(), ?config, "starting track");
		self.rendition.set(config.clone());
		self.config = Some(config);
	}

	/// The MoQ track name this importer publishes on.
	pub fn name(&self) -> &str {
		self.track.track().name()
	}

	/// A watch-only handle to this track's subscriber demand.
	pub fn demand(&self) -> moq_net::track::Demand {
		self.track.track().demand()
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

	/// Publish one Opus packet as its own group, stamping `pts` or a wall clock when absent.
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
	/// Seed a hint from a config resolved out of band (e.g. gstreamer caps rather than an OpusHead).
	fn from(config: Config) -> Self {
		crate::catalog::AudioHint {
			codec: Some(hang::catalog::AudioCodec::Opus),
			sample_rate: Some(config.sample_rate),
			channel_count: Some(config.channel_count),
			..Default::default()
		}
	}
}
