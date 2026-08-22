use super::Config;
use crate::catalog::hang::CatalogExt;
use crate::container::Frame;

/// Opus importer.
///
/// Publishes raw Opus frames (no Ogg framing) to a single moq track. Build it with
/// [`new`](Self::new), passing the track producer and the
/// [`catalog::Reserved`](crate::catalog::Reserved) it reserves its rendition from.
///
/// Every Opus packet is independently decodable, so [`decode`](Self::decode) marks only the first
/// frame of each group a keyframe (the rest extend it): frames accumulate into the current group
/// until the caller [`cut`](Self::cut)s or [`seek`](Self::seek)s. The
/// [`import::Track`](crate::import::Track) facade cuts after every packet by default (one group per
/// frame, forwarded immediately); a caller driving its own boundaries cuts less often. Opus' packet
/// loss concealment handles drops.
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
	) -> crate::Result<Self> {
		tracing::debug!(name = ?track.name(), ?config, "starting track");
		// Advertise this rendition's timeline before publishing (the generic set() no longer does).
		config.timeline = Some(reserved.producer().timeline(track.name())?.section());
		config.container = reserved.container().into();
		let mut rendition = reserved.audio(track.name());
		rendition.set(config);
		Ok(Self {
			track: reserved.producer().media_producer(track, reserved.container().into())?,
			rendition,
		})
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

	/// Mark a break in the timeline by publishing an empty group. To bound the closing
	/// group's final frame first, [`cut(end)`](Self::cut) before this. See
	/// [`Producer::discontinuity`](crate::container::Producer::discontinuity).
	pub fn discontinuity(&mut self) -> crate::Result<()> {
		self.track.discontinuity()?;
		self.estimate();
		Ok(())
	}

	/// Close the current group and open the next one at `sequence`.
	pub fn seek(&mut self, sequence: u64) -> crate::Result<()> {
		self.track.seek(sequence)?;
		self.estimate();
		Ok(())
	}

	/// Publish one Opus packet, stamping `pts` or a wall clock when absent.
	///
	/// Opus is independently decodable, so the packet is marked a keyframe only when it starts a group
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
		audio.description = config.encode().ok();
		audio.container = hang::catalog::Container::Legacy;
		audio
	}
}

#[cfg(test)]
mod tests {
	use std::time::Duration;

	use moq_net::Timestamp;

	#[tokio::test(start_paused = true)]
	async fn a_loc_reservation_reaches_the_wire_and_the_catalog() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
		let track = broadcast.create_track("audio", hang::container::track_info()).unwrap();
		let subscriber = track.subscribe(None);
		let reserved = catalog.reserve().with_container(crate::catalog::MediaContainer::Loc);
		let config = crate::codec::opus::Config::new(48_000, 2);
		let mut import = super::Import::new(track, reserved, config.into()).unwrap();

		let audio = catalog.snapshot().audio.renditions.get("audio").cloned().unwrap();
		assert_eq!(audio.container, hang::catalog::Container::Loc);

		let payload = b"opus payload";
		import
			.decode(payload, Some(Timestamp::from_micros(1_000).unwrap()))
			.unwrap();
		let mut media = crate::container::Consumer::new(subscriber, crate::catalog::hang::Container::Loc);
		let frame = tokio::time::timeout(Duration::from_secs(1), media.read())
			.await
			.unwrap()
			.unwrap()
			.unwrap();
		assert_eq!(frame.payload.as_ref(), payload);
		assert_eq!(frame.timestamp, Timestamp::from_micros(1_000).unwrap());

		import.finish().unwrap();
	}
}
