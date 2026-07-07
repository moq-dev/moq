//! Publish raw PCM as encoded audio in a moq broadcast.

use bytes::Bytes;

use moq_mux::container::{Frame as MuxFrame, Timestamp};

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
pub struct AudioProducer<E: moq_mux::catalog::hang::CatalogExt = ()> {
	encoder: Encoder,
	resampler: Option<Resampler>,
	track: moq_mux::container::Producer<moq_mux::container::legacy::Wire>,
	track_name: String,
	catalog: moq_mux::catalog::Producer<E>,
	pending: Vec<f32>,
	/// Samples emitted since the current epoch (reset by [`reset_epoch`](Self::reset_epoch)).
	frames_produced: u64,
	/// Wall-clock anchor in microseconds, taken from the first frame after each
	/// (re)start. Emitted PTS = `epoch + frames_produced / codec_rate`. `None`
	/// until the first write so the next frame re-anchors to its timestamp.
	epoch_us: Option<u64>,
}

impl<E: moq_mux::catalog::hang::CatalogExt> AudioProducer<E> {
	/// Build a producer for `name` on `broadcast`, registering the
	/// rendition in `catalog` immediately.
	pub fn new(
		broadcast: &mut moq_net::BroadcastProducer,
		catalog: moq_mux::catalog::Producer<E>,
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
		let track = broadcast.create_track(moq_net::Track::new(name.clone()))?;
		let track = catalog.media_producer(track, moq_mux::container::legacy::Wire);

		let mut catalog_mut = catalog.clone();
		let mut config = encoder.catalog();
		config.timeline = Some(catalog.timeline_section(&name));
		catalog_mut.lock().audio.insert(&name, config)?;

		Ok(Self {
			encoder,
			resampler,
			track,
			track_name: name,
			catalog,
			pending: Vec::new(),
			frames_produced: 0,
			epoch_us: None,
		})
	}

	pub fn track_name(&self) -> &str {
		&self.track_name
	}

	/// The underlying track producer, e.g. to watch subscriber state via
	/// [`used`](moq_net::TrackProducer::used) / [`unused`](moq_net::TrackProducer::unused).
	pub fn track(&self) -> &moq_net::TrackProducer {
		self.track.track()
	}

	/// Re-anchor the timeline to the next frame's timestamp, dropping any
	/// buffered samples. Call this when resuming after an idle gap (e.g. a
	/// released-then-reopened microphone) so the gap appears in the PTS and
	/// audio stays aligned with a wall-clock video track, rather than the gap
	/// being compressed out by the running sample count. Mirrors moq-boy's
	/// `reset_epoch`.
	pub fn reset_epoch(&mut self) {
		self.epoch_us = None;
		self.frames_produced = 0;
		self.pending.clear();
	}

	/// Push one [`Frame`] of PCM in the format declared in
	/// [`EncoderInput`]. Encodes and publishes as many packets as the
	/// input contains; any partial trailing frame is carried to the
	/// next call.
	///
	/// The first frame after construction (or [`reset_epoch`](Self::reset_epoch))
	/// anchors the timeline: its `timestamp_us` becomes the epoch, and emitted
	/// PTS then advances purely by the running sample count -- subsequent
	/// frames' timestamps are ignored. So an idle gap is only reflected in the
	/// PTS if you call [`reset_epoch`](Self::reset_epoch) on resume (which
	/// re-anchors from the next frame's wall-clock stamp); writing straight
	/// across a gap without resetting compresses it out.
	pub fn write(&mut self, frame: &Frame) -> Result<(), AudioError> {
		let epoch_us = *self.epoch_us.get_or_insert(frame.timestamp_us);

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

			let timestamp = self.timestamp(epoch_us)?;
			self.frames_produced += self.encoder.frame_size() as u64;
			self.publish(packet, timestamp)?;
		}

		Ok(())
	}

	/// PTS of the next frame: the epoch plus the samples emitted since it.
	fn timestamp(&self, epoch_us: u64) -> Result<Timestamp, AudioError> {
		let offset_us = (self.frames_produced * 1_000_000) / self.encoder.codec_rate() as u64;
		Ok(Timestamp::from_micros(epoch_us + offset_us)?)
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
			let timestamp = self.timestamp(self.epoch_us.unwrap_or(0))?;
			self.publish(packet, timestamp)?;
		}
		self.track.finish()?;
		Ok(())
	}
}

impl<E: moq_mux::catalog::hang::CatalogExt> Drop for AudioProducer<E> {
	fn drop(&mut self) {
		self.catalog.lock().audio.remove(&self.track_name);
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{AudioFormat, EncoderInput, EncoderOutput};

	// One 20 ms Opus frame at 48 kHz mono is exactly 960 f32 samples, so each
	// `write` of this drains precisely one packet (no resampler, no leftover).
	fn full_frame(timestamp_us: u64) -> Frame {
		let mut data = Vec::with_capacity(960 * 4);
		for _ in 0..960 {
			data.extend_from_slice(&0.1f32.to_le_bytes());
		}
		Frame {
			timestamp_us,
			data: data.into(),
		}
	}

	/// Publish each frame and read back the resulting packet PTS (microseconds).
	/// If `reset_before` contains an index, `reset_epoch()` is called before that
	/// frame's `write`.
	async fn published_pts(frames: &[Frame], reset_before: Option<usize>) -> Vec<u128> {
		let mut broadcast = moq_net::Broadcast::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast).unwrap();
		let consumer = broadcast.consume();

		// Input rate == Opus codec rate, so there's no resampler and sample
		// counts stay exact, making the PTS assertions deterministic.
		let input = EncoderInput {
			format: AudioFormat::F32,
			sample_rate: 48_000,
			channels: 1,
		};
		let mut producer =
			AudioProducer::new(&mut broadcast, catalog, "audio", input, EncoderOutput::default()).unwrap();

		let track = consumer.subscribe_track(&moq_net::Track::new("audio")).unwrap();
		let mut reader = moq_mux::container::Consumer::new(track, moq_mux::container::legacy::Wire);

		let mut pts = Vec::new();
		for (i, frame) in frames.iter().enumerate() {
			if reset_before == Some(i) {
				producer.reset_epoch();
			}
			producer.write(frame).unwrap();
			let read = reader.read().await.unwrap().expect("a packet per full frame");
			pts.push(read.timestamp.as_micros());
		}
		pts
	}

	#[tokio::test]
	async fn epoch_anchors_to_first_frame_timestamp() {
		// The first frame's timestamp becomes the epoch (regression guard: the
		// old code derived PTS purely from the sample count, always near 0).
		let pts = published_pts(&[full_frame(1_000_000)], None).await;
		assert_eq!(pts, vec![1_000_000]);
	}

	#[tokio::test]
	async fn pts_advances_by_frame_duration_ignoring_later_timestamps() {
		// Second frame's own timestamp (way ahead) is ignored; PTS advances by
		// exactly one 20 ms frame from the epoch.
		let pts = published_pts(&[full_frame(1_000), full_frame(999_999)], None).await;
		assert_eq!(pts, vec![1_000, 1_000 + 20_000]);
	}

	#[tokio::test]
	async fn reset_epoch_reanchors_so_the_gap_lands_in_pts() {
		// Frame at t=0, then reset_epoch + a frame at t=5s: the 5 s idle gap must
		// appear in the PTS (otherwise audio drifts behind a wall-clock video track).
		let pts = published_pts(&[full_frame(0), full_frame(5_000_000)], Some(1)).await;
		assert_eq!(pts, vec![0, 5_000_000]);
	}
}
