use crate as hang;
use anyhow::Context;
use buf_list::BufList;
use bytes::Buf;

/// Opus decoder, initialized via OpusHead.
pub struct Opus {
	broadcast: moq_lite::BroadcastProducer,
	catalog: hang::CatalogProducer,
	track: Option<moq_lite::TrackProducer>,
}

impl Opus {
	pub fn new(broadcast: moq_lite::BroadcastProducer, catalog: hang::CatalogProducer) -> Self {
		Self {
			broadcast,
			catalog,
			track: None,
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

		let config = hang::AudioConfig {
			codec: hang::AudioCodec::Opus,
			sample_rate,
			channel_count,
			bitrate: None,
			description: None,
		};

		let track = catalog.audio.create("opus", config.clone());
		tracing::info!(%track, ?config, "started track");

		let delivery = moq_lite::Delivery {
			priority: 2,
			max_latency: super::DEFAULT_MAX_LATENCY,
			ordered: false,
		};

		let producer = self.broadcast.create_track(track, delivery);
		self.track = Some(producer);

		Ok(())
	}

	pub fn decode<T: Buf>(&mut self, buf: &mut T, pts: Option<hang::Timestamp>) -> anyhow::Result<()> {
		let pts = self.pts(pts)?;
		let track = self.track.as_mut().context("not initialized")?;

		// Create a BufList at chunk boundaries, potentially avoiding allocations.
		let mut payload = BufList::new();
		while !buf.chunk().is_empty() {
			payload.push_chunk(buf.copy_to_bytes(buf.chunk().len()));
		}

		let container = hang::Container {
			timestamp: pts,
			payload,
		};

		// Each audio frame is a single group, because they are independent.
		let mut group = track.append_group()?;
		container.encode(&mut group)?;
		group.close()?;

		Ok(())
	}

	pub fn is_initialized(&self) -> bool {
		self.track.is_some()
	}

	fn pts(&mut self, hint: Option<hang::Timestamp>) -> anyhow::Result<hang::Timestamp> {
		if let Some(pts) = hint {
			return Ok(pts);
		}

		// Default to the unix timestamp
		Ok(hang::Timestamp::now())
	}
}

impl Drop for Opus {
	fn drop(&mut self) {
		let Some(mut track) = self.track.take() else { return };
		track.close().ok();

		let config = self.catalog.lock().audio.remove(&track).unwrap();
		tracing::debug!(track = %track.info(), ?config, "ended track");
	}
}
