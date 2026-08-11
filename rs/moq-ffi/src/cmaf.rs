//! Generic single-rendition CMAF muxing for application-selected media frames.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;

use crate::error::MoqError;
use crate::media::{MoqAudio, MoqMediaFrame, MoqVideo};

/// The media configuration used by a single-rendition CMAF muxer.
#[derive(uniffi::Enum)]
pub enum MoqCmafTrack {
	/// An encoded video rendition and its catalog metadata.
	Video { config: MoqVideo },
	/// An encoded audio rendition and its catalog metadata.
	Audio { config: MoqAudio },
}

/// Options for constructing a single-rendition CMAF muxer.
#[derive(uniffi::Record)]
pub struct MoqCmafConfig {
	/// The encoded rendition to package.
	pub track: MoqCmafTrack,
	/// Timestamp subtracted from every sample so independently fetched fragments share a timeline.
	#[uniffi(default = 0)]
	pub origin_us: u64,
}

/// CMAF data produced from one batch of encoded media frames.
#[derive(uniffi::Record)]
pub struct MoqCmafOutput {
	/// The current `ftyp+moov`, once inline codec configuration has been resolved.
	pub initialization: Option<Vec<u8>>,
	/// One `moof+mdat`, or absent when the batch produced no usable samples.
	pub fragment: Option<Vec<u8>>,
}

/// Muxes encoded media frames into CMAF initialization and media segments.
#[derive(uniffi::Object)]
pub struct MoqCmafMuxer {
	inner: Mutex<moq_mux::container::fmp4::Muxer>,
	origin: Duration,
}

#[uniffi::export]
impl MoqCmafMuxer {
	/// Create a single-rendition muxer.
	#[uniffi::constructor]
	pub fn new(config: MoqCmafConfig) -> Result<Arc<Self>, MoqError> {
		let inner = match config.track {
			MoqCmafTrack::Video { config } => moq_mux::container::fmp4::Muxer::video(&video_config(config)?)?,
			MoqCmafTrack::Audio { config } => moq_mux::container::fmp4::Muxer::audio(&audio_config(config)?)?,
		};
		Ok(Arc::new(Self {
			inner: Mutex::new(inner),
			origin: Duration::from_micros(config.origin_us),
		}))
	}

	/// Return the current initialization segment, or absent until inline codec metadata arrives.
	pub fn init_segment(&self) -> Result<Option<Vec<u8>>, MoqError> {
		let inner = self.inner.lock().map_err(|_| MoqError::Closed)?;
		Ok(inner.init()?.map(|init| init.to_vec()))
	}

	/// Normalize and encode one batch of frames on the configured zero-based timeline.
	///
	/// A batch must not cross an inline codec configuration boundary. Split and retry at the
	/// input frame index reported by the error when a rendition is reconfigured.
	pub fn mux(&self, sequence: u32, frames: Vec<MoqMediaFrame>) -> Result<MoqCmafOutput, MoqError> {
		let frames = media_frames(frames)?;
		let mut inner = self.inner.lock().map_err(|_| MoqError::Closed)?;
		let output = inner.mux(sequence, self.origin, frames)?;
		Ok(MoqCmafOutput {
			initialization: output.initialization.map(|init| init.to_vec()),
			fragment: output.fragment.map(|fragment| fragment.to_vec()),
		})
	}
}

fn media_frames(frames: Vec<MoqMediaFrame>) -> Result<Vec<moq_mux::container::Frame>, MoqError> {
	frames
		.into_iter()
		.map(|frame| {
			Ok(moq_mux::container::Frame {
				payload: Bytes::from(frame.payload),
				timestamp: moq_net::Timestamp::from_micros(frame.timestamp_us)?,
				keyframe: frame.keyframe,
				duration: frame.duration_us.map(moq_net::Timestamp::from_micros).transpose()?,
			})
		})
		.collect()
}

fn video_config(video: MoqVideo) -> Result<hang::catalog::VideoConfig, MoqError> {
	let codec: hang::catalog::VideoCodec = video.codec.parse().map_err(|_| MoqError::Unsupported)?;
	let mut config = hang::catalog::VideoConfig::new(codec);
	config.description = video.description.map(Bytes::from);
	if let Some(coded) = video.coded {
		config.coded_width = Some(coded.width);
		config.coded_height = Some(coded.height);
	}
	if let Some(display) = video.display_aspect {
		config.display_aspect_width = Some(display.width);
		config.display_aspect_height = Some(display.height);
	}
	config.bitrate = video.bitrate;
	config.framerate = video.framerate;
	config.container = video.container.into();
	Ok(config)
}

fn audio_config(audio: MoqAudio) -> Result<hang::catalog::AudioConfig, MoqError> {
	let codec: hang::catalog::AudioCodec = audio.codec.parse().map_err(|_| MoqError::Unsupported)?;
	let mut config = hang::catalog::AudioConfig::new(codec, audio.sample_rate, audio.channel_count);
	config.description = audio.description.map(Bytes::from);
	config.bitrate = audio.bitrate;
	config.container = audio.container.into();
	Ok(config)
}
