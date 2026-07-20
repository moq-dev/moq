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
}

impl<E: CatalogExt> Import<E> {
	/// Publish on an existing track producer with a resolved catalog config.
	///
	/// Audio can't derive its config from frames, so the caller passes a complete
	/// [`AudioConfig`](hang::catalog::AudioConfig) (build one from an OpusHead with [`config`], or
	/// from an out-of-band [`Config`] via `into()`). The rendition publishes immediately.
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
		self.track.cut(None)?;
		self.rendition.record_frame(timestamp, bytes);
		Ok(())
	}
}

/// Build a catalog config from an OpusHead. Errors on a malformed or empty buffer.
pub fn config(init: &[u8]) -> crate::Result<hang::catalog::AudioConfig> {
	let mut buf = init;
	Ok(Config::parse(&mut buf)?.into())
}

impl From<Config> for hang::catalog::AudioConfig {
	/// Build a catalog config from a config resolved out of band (e.g. gstreamer caps).
	fn from(config: Config) -> Self {
		let mut audio = hang::catalog::AudioConfig::new(
			hang::catalog::AudioCodec::Opus,
			config.sample_rate,
			config.channel_count,
		);
		audio.container = hang::catalog::Container::Legacy;
		audio
	}
}
