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
//! publish future (e.g. on Ctrl+C) cancels the read and releases the device. A
//! reader that falls behind loses buffers rather than growing the queue, and
//! each read reports whether that happened so the encoder can re-anchor.
//!
//! A microphone that can hear the speaker takes an
//! [`aec::Canceller`](crate::aec::Canceller) through [`Config::aec`], which runs
//! in that same callback so the buffers leaving here are already clean.

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
	/// Cancel the echo of what a speaker is playing, from
	/// [`Engine::canceller`](crate::playback::Engine::canceller).
	///
	/// Applies to [`Source::Microphone`] only: system audio is already the
	/// output, so there is nothing to subtract from it. Costs up to 10 ms of
	/// capture latency while enabled.
	///
	/// Requires the `aec` feature.
	#[cfg(feature = "aec")]
	pub aec: Option<crate::aec::Canceller>,
}

/// One buffer read from a capture source.
pub(crate) struct Samples {
	/// Interleaved `f32` PCM.
	pub data: Vec<f32>,

	/// Set when buffers were dropped before this one, because the reader fell
	/// behind. The samples are not contiguous with the previous read, so the
	/// caller must re-anchor its timeline rather than encode straight across:
	/// PTS advances by sample count, so a swallowed gap becomes permanent drift
	/// behind wall clock.
	pub gap: bool,
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
	/// Await the next buffer, or `None` once the source stops. A microphone stream
	/// error is returned immediately even if the device delivers no more samples.
	/// Cancel-safe: drop the future to release the device.
	pub(crate) async fn read(&mut self) -> Result<Option<Samples>, Error> {
		match self {
			Self::Microphone(mic) => mic.read().await,
			#[cfg(target_os = "macos")]
			Self::System(system) => Ok(system.read().await),
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
	reader: MicrophoneReader,
	/// The first buffer, captured during `open` to surface a permission failure
	/// as an error rather than a silent hang.
	pending: Option<Samples>,
}

/// The async half of a microphone stream, separate from the cpal handle so its
/// failure wakeup and stream-generation isolation can be tested without audio
/// hardware.
struct MicrophoneReader {
	rx: channel::Receiver<Vec<f32>>,
	errors: tokio::sync::mpsc::UnboundedReceiver<Error>,
}

impl MicrophoneReader {
	/// Return the buffer consumed during open unless a stream error arrived in
	/// the meantime.
	async fn pending(&mut self, samples: Samples) -> Result<Option<Samples>, Error> {
		tokio::select! {
			biased;
			Some(err) = self.errors.recv() => Err(err),
			_ = std::future::ready(()) => Ok(Some(samples)),
		}
	}

	/// Race samples against cpal's error callback. Errors win if both are ready so
	/// a dead device is never kept alive just to drain already-buffered audio.
	async fn read(&mut self) -> Result<Option<Samples>, Error> {
		tokio::select! {
			biased;
			Some(err) = self.errors.recv() => Err(err),
			data = self.rx.recv() => Ok(data.map(|data| Samples {
				data,
				gap: self.rx.gap(),
			})),
		}
	}
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

		// Tell the canceller what it's listening to before the first callback
		// arrives, so the buffers it needs are allocated off the audio thread.
		#[cfg(feature = "aec")]
		if let Some(aec) = &config.aec {
			aec.open(sample_rate, channels)?;
		}

		let (tx, rx) = channel::bounded::<Vec<f32>>();
		let (error_tx, errors) = tokio::sync::mpsc::unbounded_channel();
		let mut reader = MicrophoneReader { rx, errors };

		// What every sample format funnels into once it is interleaved `f32`.
		// Echo cancellation edits the buffer in place, so it costs no allocation
		// beyond the one the conversion already made.
		let deliver = {
			#[cfg(feature = "aec")]
			let aec = config.aec.clone();

			move |#[allow(unused_mut)] mut pcm: Vec<f32>| {
				#[cfg(feature = "aec")]
				if let Some(aec) = &aec {
					aec.process(&mut pcm);
				}
				tx.push(pcm);
			}
		};

		// The callback runs on cpal's realtime audio thread. Sample conversion
		// allocates one Vec per callback; the bounded handoff never blocks.
		let stream = match sample_format {
			cpal::SampleFormat::F32 => {
				let errors = error_tx.clone();
				device.build_input_stream(
					stream_config,
					move |data: &[f32], _: &_| deliver(data.to_vec()),
					move |err| stream_err(&errors, err),
					None,
				)
			}
			cpal::SampleFormat::I16 => {
				let errors = error_tx.clone();
				device.build_input_stream(
					stream_config,
					move |data: &[i16], _: &_| deliver(data.iter().map(|&s| s as f32 / 32768.0).collect()),
					move |err| stream_err(&errors, err),
					None,
				)
			}
			cpal::SampleFormat::U16 => {
				let errors = error_tx.clone();
				device.build_input_stream(
					stream_config,
					move |data: &[u16], _: &_| deliver(data.iter().map(|&s| (s as f32 - 32768.0) / 32768.0).collect()),
					move |err| stream_err(&errors, err),
					None,
				)
			}
			other => {
				return Err(Error::Unsupported(format!("unsupported input sample format {other:?}")));
			}
		}
		.map_err(capture_err)?;

		stream.play().map_err(capture_err)?;

		// Await the first buffer to surface a permission failure (or dead device)
		// as an error rather than a silent hang in the capture loop.
		let pending = match tokio::time::timeout(FIRST_BUFFER_TIMEOUT, reader.read()).await {
			Ok(Ok(Some(samples))) => samples,
			Ok(Ok(None)) => {
				return Err(Error::Capture(format!(
					"microphone {device} stopped before any samples"
				)));
			}
			Ok(Err(err)) => return Err(err),
			Err(_) => {
				return Err(Error::Capture(format!(
					"no samples from microphone {device} within {FIRST_BUFFER_TIMEOUT:?} (permission denied?)"
				)));
			}
		};

		tracing::info!(device = %device, sample_rate, channels, "opened microphone");

		Ok(Self {
			_stream: stream,
			reader,
			pending: Some(pending),
		})
	}

	/// Await the next buffer or stream error. Cancel-safe: drop the future to stop
	/// reading.
	async fn read(&mut self) -> Result<Option<Samples>, Error> {
		if let Some(samples) = self.pending.take() {
			return self.reader.pending(samples).await;
		}

		self.reader.read().await
	}
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

fn stream_err(errors: &tokio::sync::mpsc::UnboundedSender<Error>, err: cpal::Error) {
	tracing::error!(error = %err, "microphone stream error");
	let _ = errors.send(capture_err(err));
}

fn capture_err(err: impl std::fmt::Display) -> Error {
	Error::Capture(err.to_string())
}

#[cfg(test)]
mod tests {
	use super::*;

	fn reader() -> (
		channel::Sender<Vec<f32>>,
		tokio::sync::mpsc::UnboundedSender<Error>,
		MicrophoneReader,
	) {
		let (tx, rx) = channel::bounded();
		let (error_tx, errors) = tokio::sync::mpsc::unbounded_channel();
		(tx, error_tx, MicrophoneReader { rx, errors })
	}

	#[tokio::test]
	async fn stream_error_wakes_a_reader_without_samples() {
		let (_samples, errors, mut reader) = reader();
		errors.send(Error::Capture("device lost".into())).unwrap();

		let err = match reader.read().await {
			Err(err) => err,
			Ok(_) => panic!("the reader ignored its stream error"),
		};
		assert!(matches!(err, Error::Capture(message) if message == "device lost"));
	}

	#[tokio::test]
	async fn replaced_stream_cannot_fail_its_replacement() {
		let (_old_samples, old_errors, old_reader) = reader();
		let (new_samples, _new_errors, mut new_reader) = reader();
		drop(old_reader);

		assert!(old_errors.send(Error::Capture("stale".into())).is_err());
		new_samples.push(vec![1.0]);
		let samples = new_reader.read().await.unwrap().unwrap();
		assert_eq!(samples.data, vec![1.0]);
	}

	#[tokio::test]
	async fn stream_error_wins_over_the_buffer_saved_during_open() {
		let (_samples, errors, mut reader) = reader();
		errors.send(Error::Capture("device lost".into())).unwrap();

		let result = reader
			.pending(Samples {
				data: vec![1.0],
				gap: false,
			})
			.await;
		let err = match result {
			Err(err) => err,
			Ok(_) => panic!("the pending sample hid a stream error"),
		};
		assert!(matches!(err, Error::Capture(message) if message == "device lost"));
	}
}
