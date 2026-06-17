use bytes::{Buf, BytesMut};

use super::Config;
use crate::import::Renditions;

/// AAC importer.
///
/// Initialized from an AudioSpecificConfig blob (variable-length, typically extracted from
/// an MP4 ESDS atom), so its catalog is known up front. Each input buffer passed to
/// [`decode`](Self::decode) is published as one hang frame in its own group, so the relay can
/// forward each frame without waiting for a group boundary. The codec's packet loss
/// concealment handles drops. Build it from a [`moq_net::TrackRequest`] ([`new`](Self::new))
/// or an existing track ([`from_track`](Self::from_track)).
pub struct Import {
	track: crate::container::Producer<crate::catalog::hang::Container>,
	catalog: hang::Catalog,
	zero: Option<tokio::time::Instant>,
}

impl Import {
	/// Serve a track request, accepting it at the microsecond timescale.
	pub fn new(request: moq_net::TrackRequest, config: Config) -> crate::Result<Self> {
		let info = moq_net::TrackInfo::default().with_timescale(hang::container::TIMESCALE);
		Self::from_track(request.accept(info), config)
	}

	/// Publish on an existing track producer.
	pub fn from_track(track: moq_net::TrackProducer, config: Config) -> crate::Result<Self> {
		let mut audio_config = hang::catalog::AudioConfig::new(
			hang::catalog::AAC {
				profile: config.profile,
			},
			config.sample_rate,
			config.channel_count,
		);
		audio_config.container = hang::catalog::Container::Legacy;

		tracing::debug!(name = ?track.name(), config = ?audio_config, "starting track");

		let mut catalog = hang::Catalog::default();
		catalog.audio.renditions.insert(track.name().to_string(), audio_config);

		Ok(Self {
			track: crate::container::Producer::new(track, crate::catalog::hang::Container::Legacy),
			catalog,
			zero: None,
		})
	}

	/// The standalone catalog this importer publishes (one AAC audio rendition).
	pub fn catalog(&self) -> &hang::Catalog {
		&self.catalog
	}

	/// Returns a reference to the underlying track producer.
	pub fn track(&self) -> &moq_net::TrackProducer {
		self.track.track()
	}

	/// Mutable access to the single audio rendition, for callers that refine it
	/// after construction (the TS importer sets the synthesized `description` and
	/// an audio-burst `jitter`). Follow with [`crate::import::Track::sync`].
	pub(crate) fn rendition_mut(&mut self) -> Option<&mut hang::catalog::AudioConfig> {
		let name = self.track.name();
		self.catalog.audio.renditions.get_mut(name)
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

	pub fn decode<T: Buf>(&mut self, buf: &mut T, pts: Option<moq_net::Timestamp>) -> crate::Result<()> {
		let pts = self.pts(pts)?;

		// Collect the input into a contiguous Bytes payload.
		let mut payload = BytesMut::with_capacity(buf.remaining());
		while buf.has_remaining() {
			let chunk = buf.chunk();
			payload.extend_from_slice(chunk);
			let len = chunk.len();
			buf.advance(len);
		}

		// Each frame is its own group so the relay can forward it immediately.
		// The codec's packet loss concealment handles drops.
		let frame = crate::container::Frame {
			timestamp: pts,
			payload: payload.freeze(),
			keyframe: true,
			duration: None,
		};

		self.track.write(frame)?;
		self.track.finish_group()?;

		Ok(())
	}

	fn pts(&mut self, hint: Option<moq_net::Timestamp>) -> crate::Result<moq_net::Timestamp> {
		if let Some(pts) = hint {
			return Ok(pts);
		}

		let zero = self.zero.get_or_insert_with(tokio::time::Instant::now);
		Ok(moq_net::Timestamp::from_micros(zero.elapsed().as_micros() as u64)?)
	}
}

impl Renditions for Import {
	fn renditions(&self) -> &hang::Catalog {
		&self.catalog
	}
}
