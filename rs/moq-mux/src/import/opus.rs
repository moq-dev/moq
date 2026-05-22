use bytes::{Buf, BytesMut};

/// Typed Opus configuration for initialization without binary blobs.
pub struct OpusConfig {
	pub sample_rate: u32,
	pub channel_count: u32,
}

impl OpusConfig {
	/// Parse an OpusHead buffer into an OpusConfig.
	pub fn parse<T: Buf>(buf: &mut T) -> anyhow::Result<Self> {
		// Parse OpusHead (https://datatracker.ietf.org/doc/html/rfc7845#section-5.1)
		//  - Verifies "OpusHead" magic signature
		//  - Reads channel count
		//  - Reads sample rate
		//  - Ignores pre-skip, gain, channel mapping for now

		anyhow::ensure!(buf.remaining() >= 19, "OpusHead must be at least 19 bytes");
		const OPUS_HEAD: u64 = u64::from_be_bytes(*b"OpusHead");
		let signature = buf.get_u64();
		anyhow::ensure!(signature == OPUS_HEAD, "invalid OpusHead signature");

		buf.advance(1); // Skip version
		let channel_count = buf.get_u8() as u32;
		buf.advance(2); // Skip pre-skip (lol)
		let sample_rate = buf.get_u32_le();

		// Skip gain, channel mapping until if/when we support them
		if buf.remaining() > 0 {
			buf.advance(buf.remaining());
		}

		Ok(Self {
			sample_rate,
			channel_count,
		})
	}
}

/// Opus importer.
///
/// Initialized from an OpusHead packet. Each input buffer passed to [`decode`](Self::decode)
/// is published as one hang frame in its own group, so the relay can forward each frame
/// without waiting for a group boundary. Opus' packet loss concealment handles drops.
/// Ogg framing is not supported, feed raw Opus packets.
pub struct Opus {
	catalog: crate::catalog::Producer,
	track: crate::container::Producer<crate::container::Hang>,
	zero: Option<tokio::time::Instant>,
}

impl Opus {
	pub fn new(
		mut broadcast: moq_net::BroadcastProducer,
		mut catalog: crate::catalog::Producer,
		config: OpusConfig,
	) -> anyhow::Result<Self> {
		let track = broadcast.unique_track(".opus")?;

		let audio_config = hang::catalog::AudioConfig {
			codec: hang::catalog::AudioCodec::Opus,
			sample_rate: config.sample_rate,
			channel_count: config.channel_count,
			bitrate: None,
			description: None,
			container: hang::catalog::Container::Legacy,
			jitter: None,
		};

		tracing::debug!(name = ?track.name, config = ?audio_config, "starting track");
		catalog.lock().audio.renditions.insert(track.name.clone(), audio_config);

		Ok(Self {
			catalog,
			track: crate::container::Producer::new(track, crate::container::Hang::Legacy),
			zero: None,
		})
	}

	/// Returns a reference to the underlying track producer, e.g. for
	/// monitoring subscriber state via `used()`/`unused()`.
	pub fn track(&self) -> &moq_net::TrackProducer {
		&self.track.track
	}

	/// Finish the track, flushing the current group.
	pub fn finish(&mut self) -> anyhow::Result<()> {
		self.track.finish()?;
		Ok(())
	}

	pub fn decode<T: Buf>(&mut self, buf: &mut T, pts: Option<hang::container::Timestamp>) -> anyhow::Result<()> {
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

	fn pts(&mut self, hint: Option<hang::container::Timestamp>) -> anyhow::Result<hang::container::Timestamp> {
		if let Some(pts) = hint {
			return Ok(pts);
		}

		let zero = self.zero.get_or_insert_with(tokio::time::Instant::now);
		Ok(hang::container::Timestamp::from_micros(
			zero.elapsed().as_micros() as u64
		)?)
	}
}

impl Drop for Opus {
	fn drop(&mut self) {
		tracing::debug!(name = ?self.track.name, "ending track");
		self.catalog.lock().audio.renditions.remove(&self.track.name);
	}
}
