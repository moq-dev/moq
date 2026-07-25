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
		self.track.track().name()
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

	/// Abort the track with `err` instead of finishing it cleanly, so subscribers
	/// see the real cause rather than [`moq_net::Error::Dropped`]. Consumes this importer.
	pub fn abort(self, err: moq_net::Error) {
		self.track.abort(err);
	}

	/// Cut the current group at `end` without finishing the track.
	pub fn cut(&mut self, end: Option<moq_net::Timestamp>) -> crate::Result<()> {
		// An explicit boundary finalizes the pending bitrate window at `end`. A bare `cut(None)`
		// (the facade's per-frame close) stays metric-neutral, leaving the estimator to decode's
		// per-packet window so it isn't clobbered by a zero-length group.
		if end.is_some() {
			self.rendition.record_group_end(end);
		}
		self.track.cut(end)?;
		Ok(())
	}

	/// Mark a break in the timeline by publishing an empty group. To bound the closing
	/// group's final frame first, [`cut(end)`](Self::cut) before this. See
	/// [`Producer::discontinuity`](crate::container::Producer::discontinuity).
	pub fn discontinuity(&mut self) -> crate::Result<()> {
		self.track.discontinuity()?;
		Ok(())
	}

	/// Close the current group and open the next one at `sequence`.
	pub fn seek(&mut self, sequence: u64) -> crate::Result<()> {
		self.track.seek(sequence)?;
		Ok(())
	}

	/// Publish one Opus packet, stamping `pts` or a wall clock when absent.
	///
	/// Opus is independently decodable, so the packet is marked a keyframe only when it starts a group
	/// (see [`Producer::needs_keyframe`](crate::container::Producer::needs_keyframe)); otherwise it
	/// extends the current group. The caller bounds groups via [`cut`](Self::cut) / [`seek`](Self::seek).
	pub fn decode<B: moq_net::IntoBytes>(&mut self, frame: B, pts: Option<moq_net::Timestamp>) -> crate::Result<()> {
		let timestamp = self.rendition.timestamp(pts)?;
		// Feed the bitrate estimator one window per packet: close the previous packet's window at
		// this timestamp, then open this one. Owned entirely by decode so it's independent of where
		// the caller draws group boundaries (cut/seek).
		self.rendition.record_group_end(Some(timestamp));
		let bytes = frame.as_ref().len();
		// Only the first frame of each group is a keyframe, so the group spans until the caller cuts
		// instead of opening one group (one QUIC stream) per packet.
		let keyframe = self.track.needs_keyframe();
		self.track.write(Frame {
			timestamp,
			payload: frame.into_bytes(),
			keyframe,
			duration: None,
		})?;
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
		audio.description = config.encode().ok();
		audio.container = hang::catalog::Container::Legacy;
		audio
	}
}
