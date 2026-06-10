//! Audio: stereo unsigned-8-bit PCM (Game Boy APU) -> Opus -> MoQ.
//!
//! A thin wrapper over [`moq_audio::AudioProducer`], which resamples to 48 kHz,
//! encodes Opus, and anchors timestamps to a wall clock so audio stays in sync
//! with video. `push_samples` stamps each buffer with the shared emulator
//! clock; `reset_epoch` re-anchors on pause/resume so the gap lands in the PTS.

use std::time::Duration;

use anyhow::Result;
use bytes::Bytes;

/// The Game Boy APU outputs stereo audio.
const CHANNELS: u32 = 2;
/// 64 kbps is reasonable for stereo Game Boy audio (simple waveforms).
const OPUS_BITRATE: u32 = 64_000;

pub struct AudioEncoder {
	producer: moq_audio::AudioProducer,
}

impl AudioEncoder {
	pub fn new(
		mut broadcast: moq_net::BroadcastProducer,
		catalog: moq_mux::catalog::Producer,
		input_sample_rate: u32,
	) -> Result<Self> {
		let input = moq_audio::EncoderInput {
			format: moq_audio::AudioFormat::U8,
			sample_rate: input_sample_rate,
			channels: CHANNELS,
		};
		let output = moq_audio::EncoderOutput {
			bitrate: Some(OPUS_BITRATE),
			..Default::default()
		};

		let producer = moq_audio::AudioProducer::new(&mut broadcast, catalog, "audio", input, output)?;
		Ok(Self { producer })
	}

	pub fn track(&self) -> &moq_net::TrackProducer {
		self.producer.track()
	}

	/// Re-anchor the timeline so a pause gap shows up in the audio PTS.
	pub fn reset_epoch(&mut self) {
		self.producer.reset_epoch();
	}

	/// Push interleaved unsigned-8-bit stereo PCM captured at `elapsed` (since
	/// the emulator started, shared with the video clock).
	pub fn push_samples(&mut self, samples: &[u8], elapsed: Duration) -> Result<()> {
		let frame = moq_audio::Frame {
			timestamp_us: elapsed.as_micros() as u64,
			data: Bytes::copy_from_slice(samples),
		};
		self.producer.write(&frame)?;
		Ok(())
	}
}
