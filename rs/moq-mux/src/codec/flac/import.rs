use super::Config;
use crate::catalog::hang::CatalogExt;
use crate::container::Frame;

/// FLAC importer.
///
/// Publishes raw FLAC frames to a single moq track. Build it with
/// [`new`](Self::new), passing the track producer and the
/// [`catalog::Reserved`](crate::catalog::Reserved) it reserves its rendition from.
///
/// The STREAMINFO ([`Config`]) is required up front: it becomes the catalog
/// `description` (the `fLaC` marker plus STREAMINFO) so a decoder can initialize
/// from the catalog alone. Each FLAC frame is independently decodable, so every
/// frame handed to [`decode`](Self::decode) is published in its own group and
/// flagged as a keyframe.
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
		let mut audio = hang::catalog::AudioConfig::new(
			hang::catalog::AudioCodec::Flac,
			config.sample_rate,
			config.channel_count,
		);
		audio.container = hang::catalog::Container::Legacy;
		audio.description = Some(config.description());

		tracing::debug!(name = ?track.name(), config = ?audio, "starting track");

		let mut rendition = reserved.audio(track.name());
		rendition.set(audio);

		Ok(Self {
			track: crate::container::Producer::new(track, crate::catalog::hang::Container::Legacy),
			rendition,
		})
	}

	/// A watch-only handle to this track's subscriber demand.
	pub fn demand(&self) -> moq_net::track::Demand {
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

	/// Publish one FLAC frame as its own group, stamping `pts` or a wall clock when absent.
	pub fn decode<B: moq_net::IntoBytes>(&mut self, frame: B, pts: Option<moq_net::Timestamp>) -> crate::Result<()> {
		let timestamp = self.rendition.timestamp(pts)?;
		self.track.write(Frame {
			timestamp,
			payload: frame.into_bytes(),
			keyframe: true,
			duration: None,
		})?;
		self.track.finish_group()?;
		Ok(())
	}
}
