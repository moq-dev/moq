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
/// can initialize from the catalog alone; build the config with [`config`]. Each FLAC frame is
/// independently decodable, so every frame handed to [`decode`](Self::decode) is published in its
/// own group and flagged as a keyframe.
pub struct Import<E: CatalogExt = ()> {
	track: crate::container::Producer<crate::catalog::hang::Container>,
	rendition: crate::catalog::AudioTrack<E>,
}

impl<E: CatalogExt> Import<E> {
	/// Publish on an existing track producer with a resolved catalog config.
	///
	/// Build one from a FLAC header with [`config`]; a FLAC decoder needs the STREAMINFO
	/// `description` it carries. The rendition publishes immediately.
	pub fn new(
		track: moq_net::track::Producer,
		reserved: crate::catalog::Reserved<E>,
		mut config: hang::catalog::AudioConfig,
	) -> Self {
		tracing::debug!(name = ?track.name(), ?config, "starting track");
		// Advertise this rendition's timeline before publishing (the generic set() no longer does).
		config.timeline = Some(reserved.producer().timeline(track.name()).section());
		let mut rendition = reserved.audio(track.name());
		rendition.set(config);
		Self {
			track: reserved
				.producer()
				.media_producer(track, crate::catalog::hang::Container::Legacy),
			rendition,
		}
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

	/// Cut the current group at `end` without finishing the track.
	pub fn cut(&mut self, end: Option<moq_net::Timestamp>) -> crate::Result<()> {
		self.rendition.record_group_end(end);
		self.track.cut(end)?;
		Ok(())
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
		self.track.cut(None)?;
		self.rendition.record_frame(timestamp, bytes);
		Ok(())
	}
}

/// Build a catalog config from a FLAC header (the `fLaC` marker plus STREAMINFO), keeping `init`
/// verbatim as the catalog `description` (a decoder needs it). Errors on a malformed or empty buffer.
pub fn config(init: &[u8]) -> crate::Result<hang::catalog::AudioConfig> {
	let mut buf = init;
	let parsed = Config::parse(&mut buf)?;
	let mut audio = hang::catalog::AudioConfig::new(
		hang::catalog::AudioCodec::Flac,
		parsed.sample_rate,
		parsed.channel_count,
	);
	audio.container = hang::catalog::Container::Legacy;
	audio.description = Some(bytes::Bytes::copy_from_slice(init));
	Ok(audio)
}
