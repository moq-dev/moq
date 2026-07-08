use super::Config;
use crate::catalog::hang::CatalogExt;
use crate::container::Frame;

/// AAC importer.
///
/// Initialized from an AudioSpecificConfig blob (variable-length, typically extracted from
/// an MP4 ESDS atom), so its catalog is known up front. Each packet passed to
/// [`decode`](Self::decode) is published as one hang frame in its own group, so the relay can
/// forward each frame without waiting for a group boundary. The codec's packet loss
/// concealment handles drops. Build it with [`new`](Self::new), passing the track producer
/// and the [`catalog::Reserved`](crate::catalog::Reserved) it reserves its rendition from.
pub struct Import<E: CatalogExt = ()> {
	track: crate::container::Producer<crate::catalog::hang::Container>,
	rendition: crate::catalog::AudioTrack<E>,
}

impl<E: CatalogExt> Import<E> {
	/// Publish on an existing track producer, reserving the rendition from `reserved`.
	pub fn new(
		track: moq_net::track::Producer,
		reserved: crate::catalog::Reserved<E>,
		config: Config,
	) -> crate::Result<Self> {
		let mut audio_config = hang::catalog::AudioConfig::new(
			hang::catalog::AAC {
				profile: config.profile,
			},
			config.sample_rate,
			config.channel_count,
		);
		audio_config.container = hang::catalog::Container::Legacy;
		audio_config.description = Some(config.encode());

		tracing::debug!(name = ?track.name(), config = ?audio_config, "starting track");

		let mut rendition = reserved.audio(track.name());
		rendition.set(audio_config);

		Ok(Self {
			track: crate::container::Producer::new(track, crate::catalog::hang::Container::Legacy),
			rendition,
		})
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
