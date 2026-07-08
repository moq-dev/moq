use super::Config;
use crate::catalog::hang::CatalogExt;
use crate::container::Frame;

/// FLAC importer.
///
/// Publishes raw FLAC frames to a single moq track. Build it with
/// [`new`](Self::new), passing the track producer and the
/// [`catalog::Reserved`](crate::catalog::Reserved) it reserves its rendition from.
///
/// The STREAMINFO becomes the catalog `description` (the `fLaC` marker plus STREAMINFO) so a decoder
/// can initialize from the catalog alone; pass it to [`initialize`](Self::initialize). Each FLAC
/// frame is independently decodable, so every frame handed to [`decode`](Self::decode) is published
/// in its own group and flagged as a keyframe.
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
	/// A FLAC decoder needs the STREAMINFO `description`, which only [`initialize`](Self::initialize)
	/// can supply, so a hint-only catalog is incomplete until that runs.
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

	/// Resolve the config from a FLAC header (the `fLaC` marker plus STREAMINFO), publishing the
	/// rendition with the `description` a decoder needs. A no-op on an empty buffer.
	pub fn initialize(&mut self, data: &[u8]) -> crate::Result<()> {
		if data.is_empty() {
			return Ok(());
		}
		let mut cursor = data;
		let config = Config::parse(&mut cursor)?;
		let mut audio = hang::catalog::AudioConfig::new(
			hang::catalog::AudioCodec::Flac,
			config.sample_rate,
			config.channel_count,
		);
		// Keep the caller's `fLaC` + STREAMINFO verbatim rather than re-encoding the parsed fields,
		// which would drop any trailing metadata blocks the parse ignores.
		audio.description = Some(bytes::Bytes::copy_from_slice(data));
		self.publish(audio)
	}

	/// Publish (or re-publish) the resolved config, validating it against the hint via
	/// [`Rendition::set`](crate::catalog::Rendition::set). A no-op if unchanged.
	fn publish(&mut self, mut config: hang::catalog::AudioConfig) -> crate::Result<()> {
		config.container = hang::catalog::Container::Legacy;
		if self.config.as_ref() == Some(&config) {
			return Ok(());
		}
		tracing::debug!(name = ?self.track.name(), ?config, "starting track");
		self.rendition.set(config.clone())?;
		self.config = Some(config);
		Ok(())
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

	/// Publish one FLAC frame as its own group, stamping `pts` or a wall clock when absent.
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
