//! Microphone capture via [`cpal`] (pure-Rust: CoreAudio / WASAPI / ALSA).
//!
//! [`Microphone`] opens an input device and yields interleaved-`f32` PCM
//! [`Frame`]s, ready to feed an [`AudioProducer`](crate::AudioProducer) with
//! an [`EncoderInput`](crate::EncoderInput) of `format = AudioFormat::F32`.
//! Encoding stays on `unsafe-libopus`, so audio never touches ffmpeg.

use std::sync::mpsc::{Receiver, Sender};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::{AudioError, Frame};

/// Microphone capture configuration. All fields are hints; the backend picks
/// the closest supported mode and the [`AudioProducer`](crate::AudioProducer)
/// resamples to the codec rate anyway.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Config {
	/// Input device name. `None` opens the system default input.
	pub device: Option<String>,
	pub sample_rate: Option<u32>,
	pub channels: Option<u32>,
}

/// An open microphone, read frame-by-frame via [`read`](Self::read).
///
/// Holds the live `cpal` stream, which is `!Send`, so build and use it on a
/// single thread (e.g. inside a `spawn_blocking` closure).
pub struct Microphone {
	// Kept alive to keep capturing; dropping it stops the stream.
	_stream: cpal::Stream,
	rx: Receiver<Vec<f32>>,
	sample_rate: u32,
	channels: u32,
	frames_read: u64,
}

impl Microphone {
	/// Open the microphone described by `config`.
	pub fn open(config: &Config) -> Result<Self, AudioError> {
		let host = cpal::default_host();
		let device = match &config.device {
			Some(name) => host
				.input_devices()
				.map_err(cpal_err)?
				.find(|d| d.to_string() == *name)
				.ok_or_else(|| AudioError::Unsupported(format!("input device {name:?} not found")))?,
			None => host
				.default_input_device()
				.ok_or_else(|| AudioError::Unsupported("no default input device".into()))?,
		};

		let supported = device.default_input_config().map_err(cpal_err)?;
		let sample_format = supported.sample_format();
		let mut stream_config = supported.config();
		if let Some(rate) = config.sample_rate {
			stream_config.sample_rate = rate;
		}
		if let Some(channels) = config.channels {
			stream_config.channels = channels as u16;
		}
		let sample_rate = stream_config.sample_rate;
		let channels = stream_config.channels as u32;

		let (tx, rx) = std::sync::mpsc::channel::<Vec<f32>>();

		// The callback runs on cpal's realtime audio thread; convert to f32 and
		// forward. Keep it allocation-light and never block.
		let stream = match sample_format {
			cpal::SampleFormat::F32 => device.build_input_stream(
				stream_config,
				move |data: &[f32], _: &_| forward(&tx, data.to_vec()),
				stream_err,
				None,
			),
			cpal::SampleFormat::I16 => device.build_input_stream(
				stream_config,
				move |data: &[i16], _: &_| forward(&tx, data.iter().map(|&s| s as f32 / 32768.0).collect()),
				stream_err,
				None,
			),
			cpal::SampleFormat::U16 => device.build_input_stream(
				stream_config,
				move |data: &[u16], _: &_| forward(&tx, data.iter().map(|&s| (s as f32 - 32768.0) / 32768.0).collect()),
				stream_err,
				None,
			),
			other => {
				return Err(AudioError::Unsupported(format!(
					"unsupported input sample format {other:?}"
				)));
			}
		}
		.map_err(cpal_err)?;

		stream.play().map_err(cpal_err)?;

		tracing::info!(device = %device, sample_rate, channels, "opened microphone");

		Ok(Self {
			_stream: stream,
			rx,
			sample_rate,
			channels,
			frames_read: 0,
		})
	}

	pub fn sample_rate(&self) -> u32 {
		self.sample_rate
	}

	pub fn channels(&self) -> u32 {
		self.channels
	}

	/// Block until the next buffer of PCM is captured, or `None` once the
	/// stream stops. The returned [`Frame`] holds interleaved little-endian
	/// `f32` samples (i.e. `AudioFormat::F32`).
	pub fn read(&mut self) -> Result<Option<Frame>, AudioError> {
		let Ok(samples) = self.rx.recv() else {
			return Ok(None); // stream dropped / device gone
		};

		let timestamp_us = self.frames_read * 1_000_000 / self.sample_rate as u64;
		self.frames_read += (samples.len() / self.channels.max(1) as usize) as u64;

		let mut bytes = Vec::with_capacity(samples.len() * 4);
		for sample in &samples {
			bytes.extend_from_slice(&sample.to_le_bytes());
		}

		Ok(Some(Frame {
			timestamp_us,
			data: bytes.into(),
		}))
	}
}

/// Forward a buffer to the reader, ignoring send errors (receiver dropped means
/// capture is shutting down).
fn forward(tx: &Sender<Vec<f32>>, samples: Vec<f32>) {
	let _ = tx.send(samples);
}

fn stream_err(err: cpal::Error) {
	tracing::error!(error = %err, "microphone stream error");
}

fn cpal_err(err: cpal::Error) -> AudioError {
	AudioError::Unsupported(format!("audio capture: {err}"))
}
