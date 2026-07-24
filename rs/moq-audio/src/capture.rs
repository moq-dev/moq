//! Audio capture: a microphone via [`cpal`] (pure-Rust: CoreAudio / WASAPI /
//! ALSA), or macOS system audio via ScreenCaptureKit.
//!
//! [`Source`] picks between them and [`devices`] lists what's available, handing
//! back the ids it takes. The turnkey entry point is
//! [`encode::publish_capture`](crate::encode::publish_capture), which yields
//! interleaved-`f32` PCM and publishes it as an encoded track; encoding stays on
//! `unsafe-libopus`, so audio never touches ffmpeg.
//!
//! Both backends deliver buffers from a realtime callback through a bounded
//! async channel that the on-demand capture loop awaits, so dropping the
//! publish future (e.g. on Ctrl+C) cancels the read and releases the device.

use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::Error;

mod channel;
mod permission;

#[cfg(target_os = "macos")]
mod screencapture;

/// Where the audio comes from.
///
/// The identifiers come from [`devices`]; each listed device's
/// [`source`](Device::source) builds the matching variant.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Source {
	/// An audio input device, by the id [`devices`] reports. `None` opens the
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
/// closest supported mode and the [`encode::Producer`](crate::encode::Producer)
/// resamples to the codec rate anyway.
///
/// `#[non_exhaustive]`: construct via [`Config::default`] and set fields, so
/// new options can be added without breaking callers.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Config {
	/// What to capture.
	pub source: Source,
	/// Samples per second to ask the device for. `None` takes its default.
	pub sample_rate: Option<u32>,
	/// Channels to ask the device for. `None` takes its default.
	pub channels: Option<u32>,
}

/// An open capture source, read buffer-by-buffer via [`read`](Self::read).
///
/// `pub(crate)`: [`encode::publish_capture`](crate::encode::publish_capture) is
/// the entry point, so the per-source backends stay an implementation detail.
pub(crate) enum Stream {
	Microphone(Microphone),
	#[cfg(target_os = "macos")]
	System(screencapture::SystemAudio),
}

impl Stream {
	/// Await the next buffer of interleaved `f32` PCM, or `None` once the source
	/// stops. Cancel-safe: drop the future to release the device.
	pub(crate) async fn read(&mut self) -> Option<Vec<f32>> {
		match self {
			Self::Microphone(mic) => mic.read().await,
			#[cfg(target_os = "macos")]
			Self::System(system) => system.read().await,
		}
	}
}

/// The format `config` will capture at, without opening the device, so the
/// catalog can be populated before anything turns on.
pub(crate) async fn format(config: &Config) -> Result<(u32, u32), Error> {
	match &config.source {
		Source::Microphone(device) => {
			let (device, config) = (device.clone(), config.clone());
			// cpal enumerates devices with blocking host I/O, so keep it off the
			// runtime's worker threads.
			blocking(move || {
				let (_, _, stream_config) = resolve(device.as_deref(), &config)?;
				Ok((stream_config.sample_rate, stream_config.channels as u32))
			})
			.await
		}
		#[cfg(target_os = "macos")]
		Source::System => Ok(screencapture::SystemAudio::format(config.sample_rate, config.channels)),
		#[cfg(not(target_os = "macos"))]
		Source::System => Err(Error::Unsupported(
			"system audio capture is only supported on macOS".into(),
		)),
	}
}

/// Open the capture source described by `config`.
pub(crate) async fn open(config: &Config) -> Result<Stream, Error> {
	match &config.source {
		Source::Microphone(device) => Ok(Stream::Microphone(Microphone::open(device.as_deref(), config).await?)),
		#[cfg(target_os = "macos")]
		Source::System => Ok(Stream::System(
			screencapture::SystemAudio::open(config.sample_rate, config.channels).await?,
		)),
		#[cfg(not(target_os = "macos"))]
		Source::System => Err(Error::Unsupported(
			"system audio capture is only supported on macOS".into(),
		)),
	}
}

/// An open microphone.
///
/// Holds the live `cpal` stream, which is `!Send`, so build and use it on a
/// single task. Buffers arrive from the realtime callback over an async channel.
pub(crate) struct Microphone {
	// Kept alive to keep capturing; dropping it stops the stream.
	_stream: cpal::Stream,
	rx: channel::Receiver<Vec<f32>>,
	/// The first buffer, captured during `open` to surface a permission failure
	/// as an error rather than a silent hang.
	pending: Option<Vec<f32>>,
}

impl Microphone {
	/// Open (and start) the requested microphone.
	///
	/// The cpal calls block inline rather than going through [`blocking`]: a
	/// `cpal::Stream` is `!Send` and so can't be built on another thread and
	/// moved here. They return as soon as the device starts; the await is the
	/// first-buffer wait below.
	async fn open(selector: Option<&str>, config: &Config) -> Result<Self, Error> {
		// Fail fast on a denied/restricted mic (macOS TCC) instead of opening a
		// stream that silently delivers nothing. A no-op on other platforms.
		permission::ensure_microphone_access().await?;

		let (device, sample_format, stream_config) = resolve(selector, config)?;
		let sample_rate = stream_config.sample_rate;
		let channels = stream_config.channels as u32;

		let (tx, mut rx) = channel::bounded::<Vec<f32>>();

		// The callback runs on cpal's realtime audio thread. Sample conversion
		// allocates one Vec per callback; the bounded handoff never blocks.
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
				return Err(Error::Unsupported(format!("unsupported input sample format {other:?}")));
			}
		}
		.map_err(capture_err)?;

		stream.play().map_err(capture_err)?;

		// Await the first buffer to surface a permission failure (or dead device)
		// as an error rather than a silent hang in the capture loop.
		let pending = match tokio::time::timeout(FIRST_BUFFER_TIMEOUT, rx.recv()).await {
			Ok(Some(samples)) => samples,
			Ok(None) => {
				return Err(Error::Capture(format!(
					"microphone {device} stopped before any samples"
				)));
			}
			Err(_) => {
				return Err(Error::Capture(format!(
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

/// Forward a buffer without blocking, dropping it if the bounded queue is full.
fn forward(tx: &channel::Sender<Vec<f32>>, samples: Vec<f32>) {
	tx.push(samples);
}

/// An audio input reported by [`devices`].
#[derive(Clone, Debug)]
pub struct Device {
	/// Opaque identifier: pass to [`Source::Microphone`].
	///
	/// cpal exposes no identifier other than the device name, so this currently
	/// equals [`name`](Self::name). Match on `id` anyway: it is what
	/// [`source`](Self::source) uses, so a host that grows a stable id later
	/// won't change this API.
	pub id: String,
	/// Human-readable name, e.g. "MacBook Pro Microphone".
	pub name: String,
	/// Whether this is the system default input.
	pub default: bool,
}

impl Device {
	/// The [`Source`] that captures this device.
	pub fn source(&self) -> Source {
		Source::Microphone(Some(self.id.clone()))
	}
}

/// List the audio inputs.
pub async fn devices() -> Result<Vec<Device>, Error> {
	blocking(list).await
}

/// The blocking half of [`devices`].
fn list() -> Result<Vec<Device>, Error> {
	let host = cpal::default_host();
	let default = host.default_input_device().map(|d| d.to_string());
	Ok(host
		.input_devices()
		.map_err(capture_err)?
		.map(|device| {
			let name = device.to_string();
			Device {
				default: Some(&name) == default.as_ref(),
				id: name.clone(),
				name,
			}
		})
		.collect())
}

/// Run blocking cpal host I/O off the runtime's worker threads.
async fn blocking<T, F>(f: F) -> Result<T, Error>
where
	F: FnOnce() -> Result<T, Error> + Send + 'static,
	T: Send + 'static,
{
	tokio::task::spawn_blocking(f)
		.await
		.map_err(|err| Error::Capture(format!("audio host thread failed: {err}")))?
}

/// Resolve the input device and its negotiated stream config from `config`.
fn resolve(
	selector: Option<&str>,
	config: &Config,
) -> Result<(cpal::Device, cpal::SampleFormat, cpal::StreamConfig), Error> {
	let host = cpal::default_host();
	let device = match selector {
		Some(name) => host
			.input_devices()
			.map_err(capture_err)?
			.find(|d| d.to_string() == name)
			.ok_or_else(|| Error::Device(format!("input device {name:?} not found")))?,
		None => host
			.default_input_device()
			.ok_or_else(|| Error::Device("no default input device".into()))?,
	};

	let supported = device.default_input_config().map_err(capture_err)?;
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

fn stream_err(err: cpal::Error) {
	tracing::error!(error = %err, "microphone stream error");
}

fn capture_err(err: impl std::fmt::Display) -> Error {
	Error::Capture(err.to_string())
}
