//! Encode decoded video frames and publish them as an H.264 moq track.

use ffmpeg_next as ffmpeg;

use moq_mux::container::Timestamp;

use crate::Error;
use crate::capture::{Camera, CameraConfig};
use crate::encoder::{Encoder, EncoderConfig, EncoderKind};

/// Default capture/encode framerate when the camera config doesn't pin one.
const DEFAULT_FRAMERATE: u32 = 30;

/// Encode decoded [`ffmpeg::frame::Video`] frames to H.264 and publish them
/// through `moq_mux::codec::h264::Import` (avc3: inline SPS/PPS).
///
/// The catalog rendition is registered eagerly by the importer, so a
/// subscriber that opens the catalog before any frame arrives still sees
/// the video track.
pub struct VideoProducer {
	import: moq_mux::codec::h264::Import,
	encoder: Encoder,
}

impl VideoProducer {
	pub fn new(
		broadcast: moq_net::BroadcastProducer,
		catalog: moq_mux::catalog::Producer,
		config: &EncoderConfig,
	) -> Result<Self, Error> {
		let import =
			moq_mux::codec::h264::Import::new(broadcast, catalog).with_mode(moq_mux::codec::h264::Mode::Avc3)?;
		let encoder = Encoder::new(config)?;
		Ok(Self { import, encoder })
	}

	/// The ffmpeg encoder in use, e.g. `"h264_videotoolbox"`.
	pub fn encoder_name(&self) -> &str {
		self.encoder.name()
	}

	/// Encode and publish one frame at the given presentation timestamp.
	pub fn write(&mut self, frame: &ffmpeg::frame::Video, timestamp: Timestamp) -> Result<(), Error> {
		for mut packet in self.encoder.encode(frame)? {
			self.import.decode_frame(&mut packet, Some(timestamp))?;
		}
		Ok(())
	}

	/// Flush the encoder and finalize the track.
	pub fn finish(&mut self, timestamp: Timestamp) -> Result<(), Error> {
		for mut packet in self.encoder.finish()? {
			self.import.decode_frame(&mut packet, Some(timestamp))?;
		}
		self.import.finish()?;
		Ok(())
	}
}

/// High-level webcam publish configuration.
#[derive(Clone, Debug, Default)]
pub struct CameraPublishConfig {
	pub camera: CameraConfig,
	/// Target bitrate in bits per second; `None` derives from resolution.
	pub bitrate: Option<u64>,
	/// Encoder implementation preference.
	pub kind: EncoderKind,
}

/// Open the camera, encode, and publish until the device stops or the
/// broadcast is dropped. Blocking: run it on a dedicated thread (e.g.
/// `tokio::task::spawn_blocking`).
///
/// Presentation timestamps are derived from a monotonic frame counter at
/// the configured framerate, so the on-wire timeline stays smooth and
/// gap-free even when camera delivery jitters.
pub fn publish_camera(
	broadcast: moq_net::BroadcastProducer,
	catalog: moq_mux::catalog::Producer,
	config: CameraPublishConfig,
) -> Result<(), Error> {
	let framerate = config.camera.framerate.unwrap_or(DEFAULT_FRAMERATE);

	let mut camera = Camera::open(&config.camera)?;

	// Resolution comes from the camera's negotiated mode, not the request:
	// the backend may snap to the nearest supported size.
	let mut encoder_config = EncoderConfig::new(camera.width(), camera.height(), framerate);
	encoder_config.bitrate = config.bitrate;
	encoder_config.kind = config.kind;

	let mut producer = VideoProducer::new(broadcast, catalog, &encoder_config)?;
	tracing::info!(
		encoder = producer.encoder_name(),
		device = camera.device(),
		framerate,
		"publishing webcam"
	);

	let mut index: u64 = 0;
	let timestamp_for = |index: u64| -> Result<Timestamp, Error> {
		Ok(Timestamp::from_micros((index * 1_000_000) / framerate as u64)?)
	};

	while let Some(frame) = camera.read()? {
		producer.write(&frame, timestamp_for(index)?)?;
		index += 1;
	}

	producer.finish(timestamp_for(index)?)?;
	Ok(())
}
