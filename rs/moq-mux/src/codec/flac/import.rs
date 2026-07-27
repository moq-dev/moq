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
/// can initialize from the catalog alone; build the config with [`config`]. Every FLAC frame is
/// independently decodable, so [`decode`](Self::decode) marks only the first frame of each group a
/// keyframe (the rest extend it): frames accumulate into the current group until the caller
/// [`cut`](Self::cut)s or [`seek`](Self::seek)s. The [`import::Track`](crate::import::Track) facade
/// cuts after every frame by default (one group per frame); a caller driving its own boundaries cuts
/// less often.
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
	) -> crate::Result<Self> {
		tracing::debug!(name = ?track.name(), ?config, "starting track");
		// Advertise this rendition's timeline before publishing (the generic set() no longer does).
		config.timeline = Some(reserved.producer().timeline(track.name())?.section());
		let mut rendition = reserved.audio(track.name());
		rendition.set(config);
		Ok(Self {
			track: reserved
				.producer()
				.media_producer(track, crate::catalog::hang::Container::Legacy)?,
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
		self.estimate();
		Ok(())
	}

	/// Abort the track with `err` instead of finishing it cleanly, so subscribers
	/// see the real cause rather than [`moq_net::Error::Dropped`]. Consumes this importer.
	pub fn abort(self, err: moq_net::Error) {
		self.track.abort(err);
	}

	/// Publish what the track measured (bitrate, jitter) into the catalog rendition, filling only
	/// the fields its config didn't supply.
	fn estimate(&mut self) {
		self.rendition.estimate(self.track.estimate());
	}

	/// Cut the current group at `end` without finishing the track.
	pub fn cut(&mut self, end: Option<moq_net::Timestamp>) -> crate::Result<()> {
		self.track.cut(end)?;
		self.estimate();
		Ok(())
	}

	/// Close the current group and open the next one at `sequence`.
	pub fn seek(&mut self, sequence: u64) -> crate::Result<()> {
		self.track.seek(sequence)?;
		self.estimate();
		Ok(())
	}

	/// Publish one FLAC frame, stamping `pts` or a wall clock when absent.
	///
	/// FLAC is independently decodable, so the frame is marked a keyframe only when it starts a group
	/// (see [`Producer::needs_keyframe`](crate::container::Producer::needs_keyframe)); otherwise it
	/// extends the current group. The caller bounds groups via [`cut`](Self::cut) / [`seek`](Self::seek).
	pub fn decode<B: moq_net::IntoBytes>(&mut self, frame: B, pts: Option<moq_net::Timestamp>) -> crate::Result<()> {
		let timestamp = self.rendition.timestamp(pts)?;
		// Only the first frame of each group is a keyframe, so the group spans until the caller cuts
		// instead of opening one group (one QUIC stream) per packet.
		let keyframe = self.track.needs_keyframe();
		self.track.write(Frame {
			timestamp,
			payload: frame.into_bytes(),
			keyframe,
			duration: None,
		})?;
		self.estimate();
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
