//! Audio: stereo signed-16-bit PCM (Game Boy APU) -> Opus -> MoQ.
//!
//! A thin wrapper over [`moq_audio::encode::Producer`], which resamples to
//! 48 kHz, encodes Opus, and anchors timestamps to a wall clock so audio stays
//! in sync with video. `push_samples` stamps each buffer with the shared
//! emulator clock; `reset_epoch` re-anchors on pause/resume so the gap lands in
//! the PTS.

use std::time::Duration;

use anyhow::Result;
use bytes::Bytes;

/// The Game Boy APU outputs stereo audio.
const CHANNELS: u32 = 2;
/// 64 kbps is reasonable for stereo Game Boy audio (simple waveforms).
const OPUS_BITRATE: u32 = 64_000;

pub struct AudioEncoder {
	producer: moq_audio::encode::Producer,
}

impl AudioEncoder {
	pub fn new(
		mut broadcast: moq_net::broadcast::Producer,
		catalog: moq_mux::catalog::Producer,
		input_sample_rate: u32,
	) -> Result<Self> {
		let input = moq_audio::encode::Input {
			format: moq_audio::Format::S16,
			sample_rate: input_sample_rate,
			channels: CHANNELS,
		};
		let mut options = moq_audio::encode::Options::default();
		options.track = Some("audio".to_string());
		options.bitrate = Some(OPUS_BITRATE);

		let producer = moq_audio::encode::Producer::new(&mut broadcast, catalog, input, &options)?;
		Ok(Self { producer })
	}

	pub fn track(&self) -> &moq_net::track::Producer {
		self.producer.track()
	}

	/// Re-anchor the timeline so a pause gap shows up in the audio PTS.
	pub fn reset_epoch(&mut self) {
		self.producer.reset_epoch();
	}

	/// Push interleaved signed-16-bit stereo PCM captured at `elapsed` (since
	/// the emulator started, shared with the video clock).
	pub fn push_samples(&mut self, samples: &[i16], elapsed: Duration) -> Result<()> {
		let mut data = Vec::with_capacity(samples.len() * 2);
		for sample in samples {
			data.extend_from_slice(&sample.to_le_bytes());
		}
		let frame = moq_audio::Frame {
			timestamp: moq_net::Timestamp::from_micros(elapsed.as_micros() as u64)?,
			data: Bytes::from(data),
		};
		self.producer.write(&frame)?;
		Ok(())
	}
}
