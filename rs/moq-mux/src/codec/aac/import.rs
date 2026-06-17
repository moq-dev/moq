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
/// and the [`catalog::Producer`](crate::catalog::Producer) it publishes its rendition into.
pub struct Import<E: CatalogExt = ()> {
	track: crate::container::Producer<crate::catalog::hang::Container>,
	rendition: crate::catalog::AudioTrack<E>,
}

impl<E: CatalogExt> Import<E> {
	/// Publish on an existing track producer, registering the rendition in `catalog`.
	pub fn new(
		track: moq_net::TrackProducer,
		catalog: crate::catalog::Producer<E>,
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

		tracing::debug!(name = ?track.name(), config = ?audio_config, "starting track");

		let mut rendition = catalog.audio_track(track.name());
		rendition.set(audio_config);

		Ok(Self {
			track: crate::container::Producer::new(track, crate::catalog::hang::Container::Legacy),
			rendition,
		})
	}

	/// A watch-only handle to this track's subscriber demand.
	pub fn demand(&self) -> moq_net::TrackDemand {
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
		self.track.finish()?;
		Ok(())
	}

	/// Close the current group and open the next one at `sequence`.
	pub fn seek(&mut self, sequence: u64) -> crate::Result<()> {
		self.track.seek(sequence)?;
		Ok(())
	}

	/// Publish one AAC packet as its own group, stamping `pts` or a wall clock when absent.
	pub fn decode(&mut self, frame: &[u8], pts: Option<moq_net::Timestamp>) -> crate::Result<()> {
		let timestamp = self.rendition.timestamp(pts)?;
		self.track.write(Frame {
			timestamp,
			payload: bytes::Bytes::copy_from_slice(frame),
			keyframe: true,
			duration: None,
		})?;
		self.track.finish_group()?;
		Ok(())
	}
}
