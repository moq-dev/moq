use anyhow::Context;
use buf_list::BufList;
use bytes::Buf;

/// AAC decoder, initialized via AudioSpecificConfig (variable length from ESDS box).
pub struct Aac {
	broadcast: moq_lite::BroadcastProducer,
	catalog: hang::catalog::CatalogProducer,
	track: Option<hang::container::OrderedProducer>,
	zero: Option<tokio::time::Instant>,
}

impl Aac {
	pub fn new(broadcast: moq_lite::BroadcastProducer, catalog: hang::catalog::CatalogProducer) -> Self {
		Self {
			broadcast,
			catalog,
			track: None,
			zero: None,
		}
	}

	pub fn initialize<T: Buf>(&mut self, buf: &mut T) -> anyhow::Result<()> {
		anyhow::ensure!(buf.remaining() >= 2, "AudioSpecificConfig must be at least 2 bytes");

		// Parse AudioSpecificConfig (ISO 14496-3)
		// This parser handles variable-length configurations including:
		// - Basic formats (object_type < 31)
		// - Extended formats (object_type == 31)
		// - SBR/PS extensions
		// - Explicit sample rates (freq_index == 15)

		// Read first byte
		let b0 = buf.get_u8();
		let mut object_type = b0 >> 3;
		let mut freq_index;

		// Handle extended audioObjectType (object_type == 31)
		let (profile, sample_rate, channel_count) = if object_type == 31 {
			anyhow::ensure!(
				buf.remaining() >= 2,
				"extended audioObjectType requires 2 additional bytes"
			);
			// Extended format: next 6 bits are the extended object_type (32-63)
			// Bits 5-7 of b0 are the first 3 bits of extended object_type
			let b_ext = buf.get_u8();
			// Bits 0-2 of b_ext are the last 3 bits of extended object_type
			let audio_object_type_ext = ((b0 & 0x07) << 3) | ((b_ext >> 5) & 0x07);
			object_type = 32 + audio_object_type_ext;
			// Bits 3-6 of b_ext are samplingFrequencyIndex (4 bits)
			freq_index = (b_ext >> 1) & 0x0F;
			// Bit 0 of b_ext is the first bit of channelConfiguration
			let channel_config_high = b_ext & 0x01;

			// Read next byte for rest of channelConfiguration
			anyhow::ensure!(buf.remaining() >= 1, "AudioSpecificConfig incomplete");
			let b1 = buf.get_u8();
			// Bits 5-7 of b1 are the remaining 3 bits of channelConfiguration
			let channel_config = (channel_config_high << 3) | ((b1 >> 5) & 0x07);

			let sample_rate = sample_rate_from_index(freq_index, buf)?;
			let channel_count = channel_count_from_config(channel_config);

			// Consume any remaining extension data
			if buf.remaining() > 0 {
				buf.advance(buf.remaining());
			}

			(object_type, sample_rate, channel_count)
		} else {
			// Standard format: bits 5-7 of b0 are first 3 bits of freq_index
			freq_index = (b0 & 0x07) << 1;

			// Read second byte
			anyhow::ensure!(buf.remaining() >= 1, "AudioSpecificConfig incomplete");
			let b1 = buf.get_u8();

			// Complete frequency index (bit 7 of b1 is bit 0 of freq_index)
			freq_index |= (b1 >> 7) & 0x01;

			// Channel configuration
			let channel_config = (b1 >> 3) & 0x0F;

			let sample_rate = sample_rate_from_index(freq_index, buf)?;
			let channel_count = channel_count_from_config(channel_config);

			// Consume any remaining extension data (SBR, PS, etc.)
			// AudioSpecificConfig can have variable-length extensions that we don't need to parse.
			// Since we've already extracted the essential info (object_type, sample_rate, channels),
			// we'll consume any remaining bytes to ensure the buffer is properly advanced.
			// This makes the parser robust to different AAC variants from OBS and other sources.
			if buf.remaining() > 0 {
				buf.advance(buf.remaining());
			}

			(object_type, sample_rate, channel_count)
		};

		let mut catalog = self.catalog.lock();

		let config = hang::catalog::AudioConfig {
			codec: hang::catalog::AAC { profile }.into(),
			sample_rate,
			channel_count,
			bitrate: None,
			description: None,
			container: hang::catalog::Container::Legacy,
			jitter: None,
		};
		let track = catalog.audio.create_track("aac", config.clone());
		tracing::debug!(name = ?track.name, ?config, "starting track");

		self.track = Some(self.broadcast.create_track(track).into());

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
		track.flush()?; // We know the next frame will be a keyframe, so flush the current group.

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

impl Drop for Aac {
	fn drop(&mut self) {
		if let Some(track) = self.track.take() {
			tracing::debug!(name = ?track.info.name, "ending track");
			self.catalog.lock().audio.remove_track(&track.info);
		}
	}
}

fn sample_rate_from_index<T: Buf>(freq_index: u8, buf: &mut T) -> anyhow::Result<u32> {
	const SAMPLE_RATES: [u32; 13] = [
		96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350,
	];

	if freq_index == 15 {
		anyhow::ensure!(buf.remaining() >= 3, "explicit sample rate requires 3 additional bytes");
		let rate_bytes = [buf.get_u8(), buf.get_u8(), buf.get_u8()];
		return Ok(((rate_bytes[0] as u32) << 16) | ((rate_bytes[1] as u32) << 8) | (rate_bytes[2] as u32));
	}

	SAMPLE_RATES
		.get(freq_index as usize)
		.copied()
		.context("unsupported sample rate index")
}

fn channel_count_from_config(channel_config: u8) -> u32 {
	if channel_config == 0 {
		2
	} else if channel_config <= 7 {
		channel_config as u32
	} else {
		tracing::warn!(channel_config, "unsupported channel config, defaulting to stereo");
		2
	}
}
