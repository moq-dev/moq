use super::Config;
use crate::catalog::hang::CatalogExt;
use crate::container::Frame;

/// AAC importer.
///
/// The catalog comes from an AudioSpecificConfig (variable-length, typically extracted from an MP4
/// ESDS atom); build the config with [`config`]. Every AAC packet is independently decodable, so
/// [`decode`](Self::decode) marks only the first frame of each group a keyframe (the rest extend it):
/// frames accumulate into the current group until the caller [`cut`](Self::cut)s or [`seek`](Self::seek)s.
/// The [`import::Track`](crate::import::Track) facade passes that through, so boundaries are the
/// caller's either way: cut per packet for one group (one QUIC stream) the relay forwards without
/// waiting, or at a segment cadence to align with video. The codec's packet loss concealment
/// handles drops.
pub struct Import<E: CatalogExt = ()> {
	track: crate::container::Producer<crate::catalog::hang::Container>,
	rendition: crate::catalog::AudioTrack<E>,
}

impl<E: CatalogExt> Import<E> {
	/// Publish on an existing track producer with a resolved catalog config.
	///
	/// Build one from an AudioSpecificConfig with [`config`] (which keeps its bytes as the catalog
	/// `description`), or from an out-of-band [`Config`] via `into()` (which synthesizes the
	/// description). The rendition publishes immediately.
	pub fn new(
		track: moq_net::track::Producer,
		reserved: crate::catalog::Reserved<E>,
		config: hang::catalog::AudioConfig,
	) -> crate::Result<Self> {
		tracing::debug!(name = ?track.name(), ?config, "starting track");
		let mut rendition = reserved.audio(track.name());
		rendition.set(config);
		Ok(Self {
			track: reserved
				.producer()
				.media_producer(track, crate::catalog::hang::Container::Legacy)?,
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

	/// Publish one AAC packet, stamping `pts` or a wall clock when absent.
	///
	/// AAC is independently decodable, so the packet is marked a keyframe only when it starts a group
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

/// Build a catalog config from an AudioSpecificConfig, keeping `init` verbatim as the catalog
/// `description` (re-encoding the parsed fields would drop any SBR/PS extension the parse ignores).
/// Errors on a malformed or empty buffer.
pub fn config(init: &[u8]) -> crate::Result<hang::catalog::AudioConfig> {
	let mut buf = init;
	let mut audio: hang::catalog::AudioConfig = Config::parse(&mut buf)?.into();
	audio.description = Some(bytes::Bytes::copy_from_slice(init));
	Ok(audio)
}

impl From<Config> for hang::catalog::AudioConfig {
	/// Build a catalog config from a config resolved out of band (an ADTS header, gstreamer caps),
	/// synthesizing the AudioSpecificConfig `description` since no verbatim bytes are available.
	fn from(config: Config) -> Self {
		let mut audio = hang::catalog::AudioConfig::new(
			hang::catalog::AAC {
				profile: config.profile,
			},
			config.sample_rate,
			config.channel_count,
		);
		audio.container = hang::catalog::Container::Legacy;
		audio.description = Some(config.encode());
		audio
	}
}
