//! Audio capture: a microphone via [`cpal`] (pure-Rust: CoreAudio / WASAPI /
//! ALSA), or macOS system audio via ScreenCaptureKit.
//!
//! [`Source`] picks between them and [`publish_capture`] is the turnkey entry
//! point: it yields interleaved-`f32` PCM and publishes it as an Opus track.
//! Encoding stays on `unsafe-libopus`, so audio never touches ffmpeg.
//!
//! Both backends deliver buffers from a realtime callback through an async
//! channel that the on-demand capture loop awaits, so dropping the publish
//! future (e.g. on Ctrl+C) cancels the read and releases the device.

use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use tokio::sync::mpsc;

use crate::{AudioError, AudioFormat, AudioProducer, EncoderInput, EncoderOutput, Frame};

mod permission;

#[cfg(target_os = "macos")]
mod screencapture;

/// Where the audio comes from.
///
/// The identifiers come from [`devices`]; each listed device's `source()` builds
/// the matching variant.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Source {
	/// An audio input device, by the name [`devices`] reports. `None` opens the
	/// system default input.
	Microphone(Option<String>),

	/// System (desktop) audio: everything the machine is playing, minus this
	/// process. macOS only, and it needs the Screen Recording permission, since
	/// that's the API Apple exposes it through.
	System,
}

/// The default microphone, matching the historical `Config::default()`.
impl Default for Source {
	fn default() -> Self {
		Self::Microphone(None)
	}
}

/// How long `open` waits for the first buffer before assuming the mic never
/// started (e.g. permission denied), mirroring the camera path's first-frame
/// timeout. Without this the capture loop hangs silently forever when macOS TCC
/// denies microphone access.
const FIRST_BUFFER_TIMEOUT: Duration = Duration::from_secs(5);

/// Audio capture configuration. All fields are hints; the backend picks the
/// closest supported mode and the [`AudioProducer`] resamples to the codec rate
/// anyway.
///
/// `#[non_exhaustive]`: construct via [`Config::default`] and set fields, so
/// new options can be added without breaking callers.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Config {
	/// What to capture.
	pub source: Source,
	pub sample_rate: Option<u32>,
	pub channels: Option<u32>,
}

/// An open capture source, read buffer-by-buffer via [`read`](Self::read).
///
/// Private: [`publish_capture`] is the entry point, so the per-source backends
/// stay an implementation detail.
enum Input {
	Microphone(Microphone),
	#[cfg(target_os = "macos")]
	System(screencapture::SystemAudio),
}

impl Input {
	/// The format `config` will capture at, without opening the device, so the
	/// catalog can be populated before anything turns on.
	fn format(config: &Config) -> Result<(u32, u32), AudioError> {
		match &config.source {
			Source::Microphone(device) => Microphone::format(device.as_deref(), config),
			#[cfg(target_os = "macos")]
			Source::System => Ok(screencapture::SystemAudio::format(config.sample_rate, config.channels)),
			#[cfg(not(target_os = "macos"))]
			Source::System => Err(AudioError::Unsupported(
				"system audio capture is only supported on macOS".into(),
			)),
		}
	}

	async fn open(config: &Config) -> Result<Self, AudioError> {
		match &config.source {
			Source::Microphone(device) => Ok(Self::Microphone(Microphone::open(device.as_deref(), config).await?)),
			#[cfg(target_os = "macos")]
			Source::System => Ok(Self::System(
				screencapture::SystemAudio::open(config.sample_rate, config.channels).await?,
			)),
			#[cfg(not(target_os = "macos"))]
			Source::System => Err(AudioError::Unsupported(
				"system audio capture is only supported on macOS".into(),
			)),
		}
	}

	/// Await the next buffer of interleaved `f32` PCM, or `None` once the source
	/// stops. Cancel-safe: drop the future to release the device.
	async fn read(&mut self) -> Option<Vec<f32>> {
		match self {
			Self::Microphone(mic) => mic.read().await,
			#[cfg(target_os = "macos")]
			Self::System(system) => system.read().await,
		}
	}
}

/// An open microphone.
///
/// Holds the live `cpal` stream, which is `!Send`, so build and use it on a
/// single task. Buffers arrive from the realtime callback over an async channel.
struct Microphone {
	// Kept alive to keep capturing; dropping it stops the stream.
	_stream: cpal::Stream,
	rx: mpsc::UnboundedReceiver<Vec<f32>>,
	/// The first buffer, captured during `open` to surface a permission failure
	/// as an error rather than a silent hang.
	pending: Option<Vec<f32>>,
}

impl Microphone {
	/// The device's negotiated format `(sample_rate, channels)` without opening
	/// a stream, so the catalog can be populated before the mic is turned on.
	fn format(device: Option<&str>, config: &Config) -> Result<(u32, u32), AudioError> {
		let (_, _, stream_config) = resolve(device, config)?;
		Ok((stream_config.sample_rate, stream_config.channels as u32))
	}

	/// Open (and start) the requested microphone.
	async fn open(selector: Option<&str>, config: &Config) -> Result<Self, AudioError> {
		// Fail fast on a denied/restricted mic (macOS TCC) instead of opening a
		// stream that silently delivers nothing. A no-op on other platforms.
		permission::ensure_microphone_access().await?;

		let (device, sample_format, stream_config) = resolve(selector, config)?;
		let sample_rate = stream_config.sample_rate;
		let channels = stream_config.channels as u32;

		let (tx, mut rx) = mpsc::unbounded_channel::<Vec<f32>>();

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

		// Await the first buffer to surface a permission failure (or dead device)
		// as an error rather than a silent hang in the capture loop.
		let pending = match tokio::time::timeout(FIRST_BUFFER_TIMEOUT, rx.recv()).await {
			Ok(Some(samples)) => samples,
			Ok(None) => {
				return Err(AudioError::Unsupported(format!(
					"microphone {device} stopped before any samples"
				)));
			}
			Err(_) => {
				return Err(AudioError::Unsupported(format!(
					"no samples from microphone {device} within {FIRST_BUFFER_TIMEOUT:?} (permission denied?)"
				)));
			}
		};

		tracing::info!(device = %device, sample_rate, channels, "opened microphone");

		Ok(Self {
			_stream: stream,
			rx,
			pending: Some(pending),
		})
	}

	/// Await the next buffer of interleaved `f32` PCM, or `None` once the stream
	/// stops. Cancel-safe: drop the future to stop reading.
	async fn read(&mut self) -> Option<Vec<f32>> {
		match self.pending.take() {
			Some(samples) => Some(samples),
			None => self.rx.recv().await, // stream dropped / device gone
		}
	}
}

/// Capture audio on demand and publish it as an Opus moq track named
/// `track_name`.
///
/// The catalog rendition is registered up front from the source's reported
/// format (no capture needed), but the device only opens while a subscriber is
/// listening and is released when the last one leaves. On resume the timeline
/// re-anchors (via [`AudioProducer::reset_epoch`]) so the idle gap lands in the
/// PTS, keeping audio aligned with a wall-clock video track.
///
/// The capture-side settings come from [`Config`]; the encode-side settings
/// (codec, bitrate, frame duration) from [`EncoderOutput`]. Returns when the
/// broadcast is dropped or the capture loop fails.
pub async fn publish_capture(
	mut broadcast: moq_net::broadcast::Producer,
	catalog: moq_mux::catalog::Producer,
	config: Config,
	track_name: impl Into<String>,
	output: EncoderOutput,
	clock: moq_mux::Clock,
) -> Result<(), AudioError> {
	let (sample_rate, channels) = Input::format(&config)?;
	let input = EncoderInput {
		format: AudioFormat::F32,
		sample_rate,
		channels,
	};

	let mut producer = AudioProducer::new(&mut broadcast, catalog, track_name, input, output)?;
	let track = producer.track().clone();

	let result = capture_loop(&mut producer, &track, &config, &clock).await;

	// Best-effort clean close: flush the trailing sub-frame and finalize the
	// track. Runs only when the loop ends on its own; a Ctrl+C cancels the future
	// before this point, since async `Drop` can't finalize the track.
	if let Err(err) = producer.finish() {
		tracing::debug!(error = %err, "audio track finish after capture ended");
	}
	result
}

/// Async capture/encode loop: open the source while a listener is subscribed,
/// release it when the last one leaves, and re-anchor the timeline on resume so
/// the idle gap lands in the PTS.
///
/// Cancel safety: every wait is a real `.await` (a buffer read or a demand
/// transition), so dropping this future (e.g. on Ctrl+C) drops the [`Input`] and
/// stops the underlying stream. No blocking thread is left behind.
async fn capture_loop(
	producer: &mut AudioProducer,
	track: &moq_net::track::Producer,
	config: &Config,
	clock: &moq_mux::Clock,
) -> Result<(), AudioError> {
	loop {
		// Idle until a listener subscribes; the track ending is a clean exit.
		if let Err(err) = track.used().await {
			log_track_ended(err);
			return Ok(());
		}

		let mut input = Input::open(config).await?;

		loop {
			// Race the next buffer against the last listener leaving so we release
			// the device promptly. `biased` checks demand first so an unwatched track
			// stops before reading another buffer.
			let samples = tokio::select! {
				biased;
				res = track.unused() => {
					if let Err(err) = res {
						log_track_ended(err);
						return Ok(());
					}
					break; // no listeners: release the device, then wait for one
				}
				samples = input.read() => samples,
			};

			let Some(samples) = samples else { break }; // device stopped producing samples

			// Stamp from the shared clock (including any idle gap) so the producer's
			// epoch re-anchors and audio stays aligned with the video track.
			producer.write(&frame(samples, clock.micros()))?;
		}

		// Release the device and re-anchor so the next frame after resume reflects the gap.
		drop(input);
		producer.reset_epoch();
		tracing::info!("no listeners: released audio capture");
	}
}

/// A dropped or closed track is the normal end of a publish; any other cause is
/// a real abort (e.g. a transport reset) worth surfacing rather than treating as
/// a clean exit.
fn log_track_ended(err: moq_net::Error) {
	if matches!(err, moq_net::Error::Dropped | moq_net::Error::Closed) {
		tracing::debug!("audio track no longer announced; stopping capture");
	} else {
		tracing::warn!(error = %err, "audio track aborted; stopping capture");
	}
}

/// Forward a buffer to the reader, ignoring send errors (receiver dropped means
/// capture is shutting down).
fn forward(tx: &mpsc::UnboundedSender<Vec<f32>>, samples: Vec<f32>) {
	let _ = tx.send(samples);
}

/// An audio input reported by [`devices`].
#[derive(Clone, Debug)]
pub struct Device {
	/// The device name, which is also its identifier: pass it to
	/// [`Source::Microphone`].
	pub name: String,
	/// Whether this is the system default input.
	pub default: bool,
}

impl Device {
	/// The [`Source`] that captures this device.
	pub fn source(&self) -> Source {
		Source::Microphone(Some(self.name.clone()))
	}
}

/// List the audio inputs.
pub fn devices() -> Result<Vec<Device>, AudioError> {
	let host = cpal::default_host();
	let default = host.default_input_device().map(|d| d.to_string());
	Ok(host
		.input_devices()
		.map_err(cpal_err)?
		.map(|device| {
			let name = device.to_string();
			Device {
				default: Some(&name) == default.as_ref(),
				name,
			}
		})
		.collect())
}

/// Resolve the input device and its negotiated stream config from `config`.
fn resolve(
	selector: Option<&str>,
	config: &Config,
) -> Result<(cpal::Device, cpal::SampleFormat, cpal::StreamConfig), AudioError> {
	let host = cpal::default_host();
	let device = match selector {
		Some(name) => host
			.input_devices()
			.map_err(cpal_err)?
			.find(|d| d.to_string() == name)
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
	Ok((device, sample_format, stream_config))
}

/// Pack interleaved `f32` samples into a timestamped [`Frame`] of
/// little-endian bytes (i.e. `AudioFormat::F32`).
fn frame(samples: Vec<f32>, timestamp_us: u64) -> Frame {
	let mut bytes = Vec::with_capacity(samples.len() * size_of::<f32>());
	for sample in &samples {
		bytes.extend_from_slice(&sample.to_le_bytes());
	}
	Frame {
		timestamp_us,
		data: bytes.into(),
	}
}

fn stream_err(err: cpal::Error) {
	tracing::error!(error = %err, "microphone stream error");
}

fn cpal_err(err: cpal::Error) -> AudioError {
	AudioError::Unsupported(format!("audio capture: {err}"))
}
