use bytes::{Buf, BytesMut};

use super::Config;
use crate::container::Frame;
use crate::import::Renditions;

/// Opus importer.
///
/// Publishes raw Opus frames (no Ogg framing) to a single moq track. Build it
/// from a [`moq_net::TrackRequest`] (the on-demand path, [`new`](Self::new)) or
/// an existing [`moq_net::TrackProducer`] ([`from_track`](Self::from_track)).
///
/// Each input frame is published in its own group so the relay can forward it
/// immediately without waiting for a group boundary; Opus' packet loss
/// concealment handles drops. The catalog rendition this importer publishes is
/// available via [`catalog`](Self::catalog); attach it to a broadcast catalog
/// with [`crate::import::Track`].
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
		let mut audio = hang::catalog::AudioConfig::new(
			hang::catalog::AudioCodec::Opus,
			config.sample_rate,
			config.channel_count,
		);
		audio.container = hang::catalog::Container::Legacy;

		tracing::debug!(name = ?track.name(), config = ?audio, "starting track");

		let mut catalog = hang::Catalog::default();
		catalog.audio.renditions.insert(track.name().to_string(), audio);

		Ok(Self {
			track: crate::container::Producer::new(track, crate::catalog::hang::Container::Legacy),
			catalog,
			zero: None,
		})
	}

	/// The standalone catalog this importer publishes (one Opus audio rendition).
	pub fn catalog(&self) -> &hang::Catalog {
		&self.catalog
	}

	/// The underlying track producer, e.g. for monitoring subscriber state via
	/// `used()` / `unused()`.
	pub fn track(&self) -> &moq_net::TrackProducer {
		self.track.track()
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

	/// Publish frames, each in its own group.
	pub fn decode(&mut self, frames: impl IntoIterator<Item = Frame>) -> crate::Result<()> {
		for frame in frames {
			self.track.write(frame)?;
			self.track.finish_group()?;
		}
		Ok(())
	}

	/// Publish one Opus packet from `buf`, stamping `pts` or a wall clock when absent.
	///
	/// Convenience for callers that hand over raw packet bytes plus an optional
	/// timestamp; it wraps the packet in a [`Frame`] and forwards to [`decode`](Self::decode).
	pub fn decode_buf<T: Buf>(&mut self, buf: &mut T, pts: Option<moq_net::Timestamp>) -> crate::Result<()> {
		let timestamp = self.pts(pts)?;

		let mut payload = BytesMut::with_capacity(buf.remaining());
		while buf.has_remaining() {
			let chunk = buf.chunk();
			payload.extend_from_slice(chunk);
			let len = chunk.len();
			buf.advance(len);
		}

		self.decode(std::iter::once(Frame {
			timestamp,
			payload: payload.freeze(),
			keyframe: true,
			duration: None,
		}))
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
