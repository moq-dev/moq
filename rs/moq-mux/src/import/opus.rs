use anyhow::Context;
use buf_list::BufList;
use bytes::Buf;

/// Opus decoder, initialized via a OpusHead. Does not support Ogg.
pub struct Opus {
	broadcast: moq_lite::BroadcastProducer,
	catalog: hang::CatalogProducer,
	track: Option<hang::container::OrderedProducer>,
	zero: Option<tokio::time::Instant>,
}

impl Opus {
	pub fn new(broadcast: moq_lite::BroadcastProducer, catalog: hang::CatalogProducer) -> Self {
		Self {
			broadcast,
			catalog,
			track: None,
			zero: None,
		}
	}

	pub fn initialize<T: Buf>(&mut self, buf: &mut T) -> anyhow::Result<()> {
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

		let mut catalog = self.catalog.lock();

		let config = hang::catalog::AudioConfig {
			codec: hang::catalog::AudioCodec::Opus,
			sample_rate,
			channel_count,
			bitrate: None,
			description: None,
			container: hang::catalog::Container::Legacy,
			jitter: None,
		};

		let track = catalog.audio.create_track("opus", config.clone());
		tracing::debug!(name = ?track.name, ?config, "starting track");

		let track = self.broadcast.create_track(track);
		self.track = Some(track.into());

		Ok(())
	}

	pub fn decode<T: Buf>(&mut self, buf: &mut T, pts: Option<hang::container::Timestamp>) -> anyhow::Result<()> {
		let pts = self.pts(pts)?;
		let track = self.track.as_mut().context("not initialized")?;

		// Create a BufList at chunk boundaries, potentially avoiding allocations.
		let mut payload = BufList::new();
		while !buf.chunk().is_empty() {
			payload.push_chunk(buf.copy_to_bytes(buf.chunk().len()));
		}

		let frame = hang::container::Frame {
			timestamp: pts,
			keyframe: true, // Audio frames are always keyframes
			payload,
		};

		track.write(frame)?;
		track.flush()?; // Flush the current group because we know the next frame will be a keyframe.

		Ok(())
	}

	pub fn is_initialized(&self) -> bool {
		self.track.is_some()
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
		if let Some(track) = self.track.take() {
			tracing::debug!(name = ?track.info.name, "ending track");
			self.catalog.lock().audio.remove_track(&track.info);
		}
	}
}
