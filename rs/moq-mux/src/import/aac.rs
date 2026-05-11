use anyhow::Context;
use bytes::{Buf, BytesMut};

// Pack ~100ms of audio per group. AAC frames are typically ~21ms (1024 samples at 48kHz),
// so 5 frames is a good fit. Any value works — this is just a knob for relay efficiency.
const GROUP_FRAMES: usize = 5;

/// Typed AAC configuration for initialization without binary blobs.
pub struct AacConfig {
	pub profile: u8,
	pub sample_rate: u32,
	pub channel_count: u32,
}

impl AacConfig {
	/// Parse an AudioSpecificConfig buffer into an AacConfig.
	pub fn parse<T: Buf>(buf: &mut T) -> anyhow::Result<Self> {
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

		Ok(Self {
			profile,
			sample_rate,
			channel_count,
		})
	}
}

/// AAC importer.
///
/// Initialized from an AudioSpecificConfig blob (variable-length, typically extracted from
/// an MP4 ESDS atom). Each input buffer passed to [`decode`](Self::decode) is published as
/// one hang frame; group boundaries are managed automatically every ~100 ms.
pub struct Aac {
	catalog: crate::catalog::Producer,
	track: crate::container::Producer<crate::container::Hang>,
	zero: Option<tokio::time::Instant>,
	frames: usize,
}

impl Aac {
	pub fn new(
		mut broadcast: moq_lite::BroadcastProducer,
		mut catalog: crate::catalog::Producer,
		config: AacConfig,
	) -> anyhow::Result<Self> {
		let track = broadcast.unique_track(".aac")?;

		let audio_config = hang::catalog::AudioConfig {
			codec: hang::catalog::AAC {
				profile: config.profile,
			}
			.into(),
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
			frames: 0,
		})
	}

	/// Returns a reference to the underlying track producer.
	pub fn track(&self) -> &moq_lite::TrackProducer {
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

		// Start a new group every GROUP_FRAMES frames.
		let frame = crate::container::Frame {
			timestamp: pts,
			payload: payload.freeze(),
			keyframe: self.frames % GROUP_FRAMES == 0,
		};
		self.frames += 1;

		self.track.write(frame)?;

		// Close the group immediately after the Nth frame so the relay can forward it
		// without waiting for the next keyframe to arrive.
		if self.frames % GROUP_FRAMES == 0 {
			self.track.finish_group()?;
		}

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

impl Drop for Aac {
	fn drop(&mut self) {
		tracing::debug!(name = ?self.track.name, "ending track");
		self.catalog.lock().audio.renditions.remove(&self.track.name);
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
