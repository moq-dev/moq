use super::Config;
use crate::catalog::hang::CatalogExt;
use crate::container::Frame;

/// Opus importer.
///
/// Publishes raw Opus frames (no Ogg framing) to a single moq track. Build it with
/// [`new`](Self::new), passing the track producer and the
/// [`catalog::Producer`](crate::catalog::Producer) it publishes its rendition into.
///
/// Each packet handed to [`decode`](Self::decode) is published in its own group so
/// the relay can forward it immediately without waiting for a group boundary; Opus'
/// packet loss concealment handles drops.
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
		let mut audio = hang::catalog::AudioConfig::new(
			hang::catalog::AudioCodec::Opus,
			config.sample_rate,
			config.channel_count,
		);
		audio.container = hang::catalog::Container::Legacy;

		tracing::debug!(name = ?track.name(), config = ?audio, "starting track");

		let mut rendition = catalog.audio_track(track.name());
		rendition.set(audio);

		Ok(Self {
			track: crate::container::Producer::new(track, crate::catalog::hang::Container::Legacy),
			rendition,
		})
	}

	/// A watch-only handle to this track's subscriber demand.
	pub fn demand(&self) -> moq_net::TrackDemand {
		self.track.track().demand()
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

	/// Publish one Opus packet as its own group, stamping `pts` or a wall clock when absent.
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
