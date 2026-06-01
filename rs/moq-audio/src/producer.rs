//! Publish raw PCM as encoded audio in a moq broadcast.

use bytes::Bytes;

use moq_mux::container::Frame as MuxFrame;
use moq_net::Timestamp;

use crate::codec::{Encoder, EncoderInput, EncoderOutput};
use crate::resample::Resampler;
use crate::{AudioError, Frame};

/// Encode raw PCM and publish it as a moq-mux audio track.
///
/// PCM layout (format / sample rate / channel count) is fixed at
/// construction via [`EncoderInput`]; codec configuration (codec id,
/// optional output sample rate / channels, bitrate, frame duration)
/// via [`EncoderOutput`]. Subsequent [`write`](Self::write) calls
/// just pass payload bytes and a timestamp.
///
/// The catalog rendition is registered at construction (not on first
/// write), so a subscriber that opens the catalog before any frames
/// arrive still sees the track.
pub struct AudioProducer {
	encoder: Encoder,
	resampler: Option<Resampler>,
	track: moq_mux::container::Producer<moq_mux::container::legacy::Wire>,
	track_name: String,
	catalog: moq_mux::catalog::hang::Producer,
	pending: Vec<f32>,
	frames_produced: u64,
}

impl AudioProducer {
	/// Build a producer for `name` on `broadcast`, registering the
	/// rendition in `catalog` immediately.
	pub fn new(
		broadcast: &mut moq_net::BroadcastProducer,
		catalog: moq_mux::catalog::hang::Producer,
		name: impl Into<String>,
		input: EncoderInput,
		output: EncoderOutput,
	) -> Result<Self, AudioError> {
		let encoder = Encoder::new(input, output)?;

		let resampler = if encoder.input().sample_rate == encoder.codec_rate() {
			None
		} else {
			// Use microsecond precision so 2.5 ms frame_duration (supported by
			// libopus) doesn't truncate to 2 ms.
			let chunk_frames = ((encoder.input().sample_rate as u128 * encoder.output().frame_duration.as_micros())
				/ 1_000_000) as usize;
			Some(Resampler::new(
				encoder.input().sample_rate,
				encoder.codec_rate(),
				encoder.input().channels,
				chunk_frames,
			)?)
		};

		let name = name.into();
		// Audio hang frames carry microsecond timestamps; advertise that on the
		// track so Lite05 subscribers know what scale to expect and the model
		// layer accepts Frame::timestamp on append.
		let track =
			broadcast.create_track(moq_net::Track::new(name.clone()).with_timescale(hang::container::TIMESCALE))?;
		let track = moq_mux::container::Producer::new(track, moq_mux::container::legacy::Wire);

		let mut catalog_mut = catalog.clone();
		catalog_mut.lock().audio.insert(&name, encoder.catalog())?;

		Ok(Self {
			encoder,
			resampler,
			track,
			track_name: name,
			catalog,
			pending: Vec::new(),
			frames_produced: 0,
		})
	}

	pub fn track_name(&self) -> &str {
		&self.track_name
	}

	/// Push one [`Frame`] of PCM in the format declared in
	/// [`EncoderInput`]. Encodes and publishes as many packets as the
	/// input contains; any partial trailing frame is carried to the
	/// next call.
	///
	/// `frame.timestamp_us` is currently informational; the producer
	/// derives the emitted timestamps from the running sample count so
	/// the on-wire timestamps stay monotonic and gap-free even if the
	/// caller's clock drifts.
	pub fn write(&mut self, frame: &Frame) -> Result<(), AudioError> {
		let _ = frame.timestamp_us;
		let input = self.encoder.input();
		let pcm = input.format.as_interleaved_f32(frame.data.as_ref(), input.channels)?;
		let pcm: Vec<f32> = match self.resampler.as_mut() {
			Some(r) => r.process(&pcm)?,
			None => pcm.into_owned(),
		};

		self.pending.extend(pcm);

		let frame_samples = self.encoder.frame_size() * self.encoder.codec_channels() as usize;
		while self.pending.len() >= frame_samples {
			let chunk: Vec<f32> = self.pending.drain(..frame_samples).collect();
			let packet = self.encoder.encode_f32(&chunk)?;

			let timestamp =
				Timestamp::from_micros((self.frames_produced * 1_000_000) / self.encoder.codec_rate() as u64)?;
			self.frames_produced += self.encoder.frame_size() as u64;
			self.publish(packet, timestamp)?;
		}

		Ok(())
	}

	fn publish(&mut self, payload: Bytes, timestamp: Timestamp) -> Result<(), AudioError> {
		// Each audio packet is its own moq-lite group, matching
		// moq_mux::codec::opus::Import. Opus PLC handles dropped groups.
		let mux_frame = MuxFrame {
			timestamp,
			payload,
			keyframe: true,
			duration: None,
		};
		self.track.write(mux_frame)?;
		self.track.finish_group()?;
		Ok(())
	}

	/// Flush any pending samples (zero-padded to a full frame) and
	/// finalize the track.
	pub fn finish(mut self) -> Result<(), AudioError> {
		let frame_samples = self.encoder.frame_size() * self.encoder.codec_channels() as usize;
		if !self.pending.is_empty() {
			self.pending.resize(frame_samples, 0.0);
			let chunk = std::mem::take(&mut self.pending);
			let packet = self.encoder.encode_f32(&chunk)?;
			let timestamp =
				Timestamp::from_micros((self.frames_produced * 1_000_000) / self.encoder.codec_rate() as u64)?;
			self.publish(packet, timestamp)?;
		}
		self.track.finish()?;
		Ok(())
	}
}

impl Drop for AudioProducer {
	fn drop(&mut self) {
		self.catalog.lock().audio.remove(&self.track_name);
	}
}
