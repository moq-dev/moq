//! Publish raw PCM as encoded audio in a moq broadcast.

use bytes::Bytes;

use moq_mux::container::{Frame, Timestamp};

use crate::codec::Encoder;
use crate::{AudioError, AudioSamples};

#[cfg(feature = "opus")]
use crate::codec::OpusEncoder;

#[cfg(feature = "resample")]
use crate::resample::Resampler;

/// Encode raw PCM and publish it as a moq-mux audio track.
///
/// Flow per call to [`write`](Self::write):
///   1. Convert input bytes → interleaved `f32` (`AudioFormat::to_interleaved_f32`).
///   2. Resample to the codec's rate / channel count (no-op if they match).
///   3. Buffer into the codec's frame size (Opus: 20 ms windows).
///   4. Encode each full window into a packet.
///   5. Publish each packet as its own moq-lite group, with timestamps in
///      microseconds. Matches the pattern in `moq_mux::codec::opus::Import`.
pub struct AudioProducer {
	encoder: Box<dyn Encoder>,
	track: moq_mux::container::Producer<moq_mux::container::legacy::Wire>,
	track_name: String,
	catalog: moq_mux::catalog::hang::Producer,
	catalog_registered: bool,

	#[cfg(feature = "resample")]
	resampler: Option<Resampler>,

	/// Interleaved `f32` samples carried between calls because the
	/// caller's input didn't line up with the codec's frame size.
	pending: Vec<f32>,

	/// Frames per channel produced so far — used to derive monotonic
	/// timestamps if the caller doesn't supply one.
	frames_produced: u64,
}

impl AudioProducer {
	/// Build a new Opus producer for `broadcast`, registering `name` in `catalog`.
	#[cfg(feature = "opus")]
	pub fn new_opus(
		broadcast: &mut moq_net::BroadcastProducer,
		catalog: moq_mux::catalog::hang::Producer,
		name: impl Into<String>,
		input_rate: u32,
		input_channels: u32,
		bitrate: Option<u32>,
	) -> Result<Self, AudioError> {
		// Pick a libopus-supported rate close to the input. If the caller
		// is already at one of those, no resampling needed.
		let codec_rate = pick_opus_rate(input_rate);
		let encoder = OpusEncoder::new(codec_rate, input_channels, bitrate)?;
		Self::new(broadcast, catalog, name, Box::new(encoder), input_rate, input_channels)
	}

	/// Build a producer with a caller-supplied encoder.
	///
	/// `input_rate` / `input_channels` describe what the *caller* will
	/// feed to [`write`](Self::write). A resampler is inserted if they
	/// differ from the encoder's expected rate / channels.
	pub fn new(
		broadcast: &mut moq_net::BroadcastProducer,
		catalog: moq_mux::catalog::hang::Producer,
		name: impl Into<String>,
		encoder: Box<dyn Encoder>,
		input_rate: u32,
		input_channels: u32,
	) -> Result<Self, AudioError> {
		let name = name.into();
		let track = broadcast.create_track(moq_net::Track {
			name: name.clone(),
			priority: 0,
		})?;
		let track = moq_mux::container::Producer::new(track, moq_mux::container::legacy::Wire);

		#[cfg(feature = "resample")]
		let resampler = if input_rate == encoder.sample_rate() && input_channels == encoder.channel_count() {
			None
		} else {
			// Chunk size: 20ms of input frames — same window the codec uses.
			let chunk_frames = (input_rate as usize * 20) / 1000;
			Some(Resampler::new(
				input_rate,
				encoder.sample_rate(),
				input_channels,
				chunk_frames,
			)?)
		};

		#[cfg(not(feature = "resample"))]
		if input_rate != encoder.sample_rate() || input_channels != encoder.channel_count() {
			return Err(AudioError::Unsupported(format!(
				"input {input_rate}Hz/{input_channels}ch does not match codec {}Hz/{}ch and `resample` feature is disabled",
				encoder.sample_rate(),
				encoder.channel_count(),
			)));
		}

		Ok(Self {
			encoder,
			track,
			track_name: name,
			catalog,
			catalog_registered: false,
			#[cfg(feature = "resample")]
			resampler,
			pending: Vec::new(),
			frames_produced: 0,
		})
	}

	/// Track name as registered in the catalog.
	pub fn track_name(&self) -> &str {
		&self.track_name
	}

	/// Push one buffer of raw PCM. Encodes and publishes as many packets
	/// as the input contains; any leftover frames are carried to the
	/// next call.
	pub fn write(&mut self, samples: &AudioSamples) -> Result<(), AudioError> {
		self.ensure_catalog_registered()?;

		// Step 1: convert to interleaved f32 at the *input* rate/channels.
		let pcm = samples
			.format
			.to_interleaved_f32(samples.data.as_ref(), samples.channel_count)?;

		// Step 2: resample if needed.
		#[cfg(feature = "resample")]
		let pcm = match self.resampler.as_mut() {
			Some(r) => r.process(&pcm)?,
			None => pcm,
		};

		// Step 3-5: buffer, encode, publish.
		self.pending.extend(pcm);

		let frame_samples = self.encoder.frame_size() * self.encoder.channel_count() as usize;
		while self.pending.len() >= frame_samples {
			let chunk: Vec<f32> = self.pending.drain(..frame_samples).collect();
			let packet = self.encoder.encode(&chunk)?;

			let timestamp =
				Timestamp::from_micros((self.frames_produced * 1_000_000) / self.encoder.sample_rate() as u64)?;
			self.frames_produced += self.encoder.frame_size() as u64;

			self.publish(packet, timestamp)?;
		}

		Ok(())
	}

	fn publish(&mut self, payload: Bytes, timestamp: Timestamp) -> Result<(), AudioError> {
		// Each audio packet is its own moq-lite group, matching
		// moq_mux::codec::opus::Import. Opus PLC handles dropped groups.
		let frame = Frame {
			timestamp,
			payload,
			keyframe: true,
		};
		self.track.write(frame)?;
		self.track.finish_group()?;
		Ok(())
	}

	fn ensure_catalog_registered(&mut self) -> Result<(), AudioError> {
		if self.catalog_registered {
			return Ok(());
		}
		let config = self.encoder.config();
		self.catalog.lock().audio.insert(&self.track_name, config)?;
		self.catalog_registered = true;
		Ok(())
	}

	/// Flush any pending samples (padded with silence to the next frame
	/// boundary) and finalize the track.
	pub fn finish(mut self) -> Result<(), AudioError> {
		let frame_samples = self.encoder.frame_size() * self.encoder.channel_count() as usize;
		if !self.pending.is_empty() {
			self.pending.resize(frame_samples, 0.0);
			let chunk = std::mem::take(&mut self.pending);
			let packet = self.encoder.encode(&chunk)?;
			let timestamp =
				Timestamp::from_micros((self.frames_produced * 1_000_000) / self.encoder.sample_rate() as u64)?;
			self.publish(packet, timestamp)?;
		}
		self.track.finish()?;
		Ok(())
	}
}

impl Drop for AudioProducer {
	fn drop(&mut self) {
		if self.catalog_registered {
			self.catalog.lock().audio.remove(&self.track_name);
		}
	}
}

/// Snap an arbitrary input sample rate to the nearest libopus-supported rate.
fn pick_opus_rate(input_rate: u32) -> u32 {
	const SUPPORTED: [u32; 5] = [8_000, 12_000, 16_000, 24_000, 48_000];
	if SUPPORTED.contains(&input_rate) {
		return input_rate;
	}
	// Pick the smallest supported rate that's >= input, falling back to 48k.
	SUPPORTED.iter().copied().find(|&r| r >= input_rate).unwrap_or(48_000)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn opus_rate_picker_passes_supported_through() {
		for &r in &[8_000, 12_000, 16_000, 24_000, 48_000] {
			assert_eq!(pick_opus_rate(r), r);
		}
	}

	#[test]
	fn opus_rate_picker_rounds_up() {
		assert_eq!(pick_opus_rate(44_100), 48_000);
		assert_eq!(pick_opus_rate(22_050), 24_000);
		assert_eq!(pick_opus_rate(96_000), 48_000);
	}
}
