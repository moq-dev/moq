use bytes::{Buf, BytesMut};

use super::Config;

/// Opus importer.
///
/// Initialized from an OpusHead packet. Each input buffer passed to [`decode`](Self::decode)
/// is published as one hang frame in its own group, so the relay can forward each frame
/// without waiting for a group boundary. Opus' packet loss concealment handles drops.
/// Ogg framing is not supported, feed raw Opus packets.
pub struct Import {
	catalog: crate::catalog::hang::Producer,
	track: crate::container::Producer<crate::catalog::hang::Container>,
	zero: Option<tokio::time::Instant>,
}

impl Import {
	pub fn new(
		mut broadcast: moq_net::BroadcastProducer,
		mut catalog: crate::catalog::hang::Producer,
		config: Config,
	) -> crate::Result<Self> {
		let track = broadcast.unique_track(".opus")?;

		let mut audio_config = hang::catalog::AudioConfig::new(
			hang::catalog::AudioCodec::Opus,
			config.sample_rate,
			config.channel_count,
		);
		audio_config.container = hang::catalog::Container::Legacy;

		tracing::debug!(name = ?track.name, config = ?audio_config, "starting track");
		catalog.lock().audio.renditions.insert(track.name.clone(), audio_config);

		Ok(Self {
			catalog,
			track: crate::container::Producer::new(track, crate::catalog::hang::Container::Legacy),
			zero: None,
		})
	}

	/// Returns a reference to the underlying track producer, e.g. for
	/// monitoring subscriber state via `used()`/`unused()`.
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
		// Opus' packet loss concealment handles drops.
		let frame = crate::container::Frame {
			timestamp: pts,
			payload: payload.freeze(),
			keyframe: true,
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

impl Drop for Import {
	fn drop(&mut self) {
		tracing::debug!(name = ?self.track.name, "ending track");
		self.catalog.lock().audio.renditions.remove(&self.track.name);
	}
}
