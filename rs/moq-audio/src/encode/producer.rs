//! Encode raw PCM and publish it as a moq audio track.

use std::time::Duration;

use bytes::Bytes;

use moq_mux::catalog::hang::CatalogExt;
use moq_mux::container::Frame as MuxFrame;
use moq_net::Timestamp;

use super::encoder::{Codec, Config, Encoder, Input};
use crate::resample::Resampler;
use crate::{Error, Frame};

/// Source-agnostic encode knobs for [`Producer`] and `publish_capture`, where
/// the input PCM layout comes from the caller's frames or the capture source
/// rather than from these options. For the bring-your-own-PCM
/// [`Encoder`](super::Encoder), which needs that layout up front, use
/// [`Config`](super::Config) instead.
///
/// `#[non_exhaustive]`: construct via [`Options::default`] and set fields, so
/// new knobs can be added without breaking callers.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Options {
	/// Track name to publish under. `None` derives a unique one from the codec
	/// (`0.opus`, then `1.opus`, ...), matching how the video side names its
	/// track. Subscribers find it through the catalog either way.
	pub track: Option<String>,
	/// Output codec. Defaults to [`Codec::Opus`].
	pub codec: Codec,
	/// Sample rate the codec runs at. `None` snaps the input rate up to the
	/// nearest rate the codec supports, resampling if that moved it.
	pub sample_rate: Option<u32>,
	/// Channel count the codec runs at. `None` matches the input; anything else
	/// is rejected, since remapping isn't implemented.
	pub channels: Option<u32>,
	/// Bitrate in bits per second. `None` lets the codec pick.
	pub bitrate: Option<u32>,
	/// Encoded frame duration. Opus accepts 2.5 / 5 / 10 / 20 / 40 / 60 ms.
	pub frame_duration: Duration,
}

impl Default for Options {
	fn default() -> Self {
		Self {
			track: None,
			codec: Codec::default(),
			sample_rate: None,
			channels: None,
			bitrate: None,
			frame_duration: Duration::from_millis(20),
		}
	}
}

impl Options {
	/// The [`Config`] these options describe once `input`'s layout is known.
	fn config(&self, input: Input) -> Config {
		Config {
			input,
			codec: self.codec,
			sample_rate: self.sample_rate,
			channels: self.channels,
			bitrate: self.bitrate,
			frame_duration: self.frame_duration,
		}
	}
}

/// Encode raw PCM and publish it as a moq-mux audio track.
///
/// The input PCM layout is fixed at construction via [`Input`]; the codec
/// settings via [`Options`]. Subsequent [`write`](Self::write) calls just pass a
/// [`Frame`]: payload bytes and a timestamp.
///
/// The catalog rendition is registered at construction (not on first write), so
/// a subscriber that opens the catalog before any frames arrive still sees the
/// track.
pub struct Producer<E: CatalogExt = ()> {
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

impl<E: CatalogExt> Producer<E> {
	/// Publish a track encoding `input` into `broadcast`, registering its
	/// rendition in `catalog` immediately.
	pub fn new(
		broadcast: &mut moq_net::broadcast::Producer,
		catalog: moq_mux::catalog::Producer<E>,
		input: Input,
		options: &Options,
	) -> Result<Self, Error> {
		let encoder = Encoder::new(&options.config(input))?;
		let input = &encoder.config().input;

		let resampler = if input.sample_rate == encoder.codec_rate() {
			None
		} else {
			// Use microsecond precision so 2.5 ms frame_duration (supported by
			// libopus) doesn't truncate to 2 ms.
			let chunk_frames =
				((input.sample_rate as u128 * encoder.config().frame_duration.as_micros()) / 1_000_000) as usize;
			Some(Resampler::new(
				input.sample_rate,
				encoder.codec_rate(),
				input.channels,
				chunk_frames,
			)?)
		};

		let track = match &options.track {
			// Audio hang frames carry microsecond timestamps; advertise that on the
			// track so Lite05 subscribers know what scale to expect and the model
			// layer accepts Frame::timestamp on append. `unique_track` does the same.
			Some(name) => broadcast.create_track(name.clone(), hang::container::track_info())?,
			// Mirrors the video side, which derives a unique name from the codec
			// rather than making every caller invent one.
			None => moq_mux::import::unique_track(broadcast, &format!(".{}", options.codec))?,
		};
		let name = track.name().to_string();
		let track = catalog.media_producer(track, moq_mux::container::legacy::Wire);

		let mut catalog_mut = catalog.clone();
		let mut config = encoder.catalog();
		config.timeline = Some(catalog.timeline(&name).section());
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

	/// The name of the published track, which is [`Options::track`] resolved.
	pub fn track_name(&self) -> &str {
		&self.track_name
	}

	/// The underlying track producer, e.g. to watch subscriber state via
	/// [`used`](moq_net::track::Producer::used) / [`unused`](moq_net::track::Producer::unused).
	pub fn track(&self) -> &moq_net::track::Producer {
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

	/// Push one [`Frame`] of PCM in the layout declared by [`Input`]. Encodes and
	/// publishes as many packets as the input contains; any partial trailing
	/// frame is carried to the next call.
	///
	/// The first frame after construction (or [`reset_epoch`](Self::reset_epoch))
	/// anchors the timeline: its timestamp becomes the epoch, and emitted PTS
	/// then advances purely by the running sample count, so subsequent frames'
	/// timestamps are ignored. An idle gap is only reflected in the PTS if you
	/// call [`reset_epoch`](Self::reset_epoch) on resume (which re-anchors from
	/// the next frame's wall-clock stamp); writing straight across a gap without
	/// resetting compresses it out.
	pub fn write(&mut self, frame: &Frame) -> Result<(), Error> {
		let timestamp_us = u64::try_from(frame.timestamp.as_micros())
			.map_err(|_| Error::Unsupported(format!("frame timestamp {:?} out of range", frame.timestamp)))?;
		let epoch_us = *self.epoch_us.get_or_insert(timestamp_us);

		let input = &self.encoder.config().input;
		let (format, channels) = (input.format, input.channels);
		let pcm = format.as_interleaved_f32(frame.data.as_ref(), channels)?;
		let pcm: Vec<f32> = match self.resampler.as_mut() {
			Some(r) => r.process(&pcm)?,
			None => pcm.into_owned(),
		};

		self.pending.extend(pcm);

		let frame_samples = self.encoder.frame_size() * self.encoder.codec_channels() as usize;
		while self.pending.len() >= frame_samples {
			let chunk: Vec<f32> = self.pending.drain(..frame_samples).collect();
			let packet = self.encoder.encode(&chunk)?;

			let timestamp = self.timestamp(epoch_us)?;
			self.frames_produced += self.encoder.frame_size() as u64;
			self.publish(packet, timestamp)?;
		}

		Ok(())
	}

	/// PTS of the next frame: the epoch plus the samples emitted since it.
	fn timestamp(&self, epoch_us: u64) -> Result<Timestamp, Error> {
		let offset_us = (self.frames_produced * 1_000_000) / self.encoder.codec_rate() as u64;
		Ok(Timestamp::from_micros(epoch_us + offset_us)?)
	}

	fn publish(&mut self, payload: Bytes, timestamp: Timestamp) -> Result<(), Error> {
		// Each audio packet is its own moq-lite group, matching
		// moq_mux::codec::opus::Import. Opus PLC handles dropped groups.
		let mux_frame = MuxFrame {
			timestamp,
			payload,
			keyframe: true,
			duration: None,
		};
		self.track.write(mux_frame)?;
		// No boundary to give: the next packet bounds this one, and Opus frames have a
		// deterministic duration anyway.
		self.track.cut(None)?;
		Ok(())
	}

	/// Flush any pending samples (zero-padded to a full frame) and finalize the
	/// track.
	pub fn finish(mut self) -> Result<(), Error> {
		let frame_samples = self.encoder.frame_size() * self.encoder.codec_channels() as usize;
		if !self.pending.is_empty() {
			self.pending.resize(frame_samples, 0.0);
			let chunk = std::mem::take(&mut self.pending);
			let packet = self.encoder.encode(&chunk)?;
			let timestamp = self.timestamp(self.epoch_us.unwrap_or(0))?;
			self.publish(packet, timestamp)?;
		}
		self.track.finish()?;
		Ok(())
	}

	/// Abort the track with `err` instead of finishing it, so subscribers see the
	/// real cause rather than [`moq_net::Error::Dropped`]. Pending samples are dropped.
	pub fn abort(mut self, err: moq_net::Error) {
		self.track.abort(err);
	}
}

impl<E: CatalogExt> Drop for Producer<E> {
	fn drop(&mut self) {
		self.catalog.lock().audio.remove(&self.track_name);
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::Format;

	// One 20 ms Opus frame at 48 kHz mono is exactly 960 f32 samples, so each
	// `write` of this drains precisely one packet (no resampler, no leftover).
	fn full_frame(timestamp_us: u64) -> Frame {
		let mut data = Vec::with_capacity(960 * 4);
		for _ in 0..960 {
			data.extend_from_slice(&0.1f32.to_le_bytes());
		}
		Frame {
			timestamp: Timestamp::from_micros(timestamp_us).unwrap(),
			data: data.into(),
		}
	}

	/// Publish each frame and read back the resulting packet PTS (microseconds).
	/// If `reset_before` contains an index, `reset_epoch()` is called before that
	/// frame's `write`.
	async fn published_pts(frames: &[Frame], reset_before: Option<usize>) -> Vec<u128> {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast).unwrap();
		let consumer = broadcast.consume();

		// Input rate == Opus codec rate, so there's no resampler and sample
		// counts stay exact, making the PTS assertions deterministic.
		let input = Input {
			format: Format::F32,
			sample_rate: 48_000,
			channels: 1,
		};
		let options = Options {
			track: Some("audio".to_string()),
			..Options::default()
		};
		let mut producer = Producer::new(&mut broadcast, catalog, input, &options).unwrap();

		let track = consumer.track("audio").unwrap().subscribe(None).await.unwrap();
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

	/// `Options::track = None` derives a codec-suffixed name rather than making
	/// the caller invent one, mirroring the video side. Pins the exact name the
	/// docs promise, and that a second producer doesn't collide with the first.
	#[tokio::test]
	async fn default_options_derive_the_track_name() {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast).unwrap();

		let first = Producer::new(&mut broadcast, catalog.clone(), Input::default(), &Options::default()).unwrap();
		assert_eq!(first.track_name(), "0.opus");

		let second = Producer::new(&mut broadcast, catalog, Input::default(), &Options::default()).unwrap();
		assert_eq!(second.track_name(), "1.opus");
	}
}
