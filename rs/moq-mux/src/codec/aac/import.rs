use super::Config;
use crate::catalog::hang::CatalogExt;
use crate::container::Frame;

/// AAC importer.
///
/// The catalog comes from an AudioSpecificConfig (variable-length, typically extracted from an MP4
/// ESDS atom); build the config with [`config`]. Each packet passed to [`decode`](Self::decode) is
/// published as one hang frame in its own group, so the relay can forward each frame without waiting
/// for a group boundary. The codec's packet loss concealment handles drops.
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
		self.track.cut(None)?;
		self.rendition.record_frame(timestamp, bytes);
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
