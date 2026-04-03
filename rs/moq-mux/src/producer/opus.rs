use buf_list::BufList;
use bytes::Buf;

// Make a new audio group every 100ms.
// NOTE: We could do this per-frame, but there's not much benefit to it.
const MAX_GROUP_DURATION: hang::container::Timestamp = hang::container::Timestamp::from_millis_unchecked(100);

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

/// Opus decoder, initialized via a OpusHead. Does not support Ogg.
pub struct Opus {
	catalog: crate::CatalogProducer,
	audio: hang::catalog::Audio,
	track: hang::container::OrderedProducer,
	zero: Option<tokio::time::Instant>,
}

impl Opus {
	pub fn new(
		broadcast: moq_lite::BroadcastProducer,
		mut catalog: crate::CatalogProducer,
		config: OpusConfig,
	) -> anyhow::Result<Self> {
		// Read the current audio section from the catalog, if any
		let mut audio: hang::catalog::Audio = {
			let state = catalog.writer().read();
			state
				.sections
				.get("audio")
				.and_then(|v| serde_json::from_value(v.clone()).ok())
				.unwrap_or_default()
		};

		let audio_config = hang::catalog::AudioConfig {
			codec: hang::catalog::AudioCodec::Opus,
			sample_rate: config.sample_rate,
			channel_count: config.channel_count,
			bitrate: None,
			description: None,
			container: hang::catalog::Container::Legacy,
			jitter: None,
		};

		let track_info = audio.create_track("opus", audio_config.clone());
		tracing::debug!(name = ?track_info.name, config = ?audio_config, "starting track");

		let _ = catalog.set(&hang::catalog::AUDIO, &audio);
		catalog.flush();

		let track = broadcast.create_track(track_info)?;

		Ok(Self {
			catalog,
			audio,
			track: hang::container::OrderedProducer::new(track).with_max_group_duration(MAX_GROUP_DURATION),
			zero: None,
		})
	}

	/// Finish the track, flushing the current group.
	pub fn finish(&mut self) -> anyhow::Result<()> {
		self.track.finish()?;
		Ok(())
	}

	pub fn decode<T: Buf>(&mut self, buf: &mut T, pts: Option<hang::container::Timestamp>) -> anyhow::Result<()> {
		let pts = self.pts(pts)?;

		// Create a BufList at chunk boundaries, potentially avoiding allocations.
		let mut payload = BufList::new();
		while !buf.chunk().is_empty() {
			payload.push_chunk(buf.copy_to_bytes(buf.chunk().len()));
		}

		let frame = hang::container::Frame {
			timestamp: pts,
			payload,
		};

		self.track.write(frame)?;

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
		tracing::debug!(name = ?self.track.info.name, "ending track");
		self.audio.remove_track(&self.track.info);
		let _ = self.catalog.set(&hang::catalog::AUDIO, &self.audio);
		self.catalog.flush();
	}
}
