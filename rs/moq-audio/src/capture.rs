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

use std::task::Poll;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ringbuf::traits::Producer;

use crate::Error;

mod buffer;
#[cfg(target_os = "macos")]
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

	/// Returns the allocation to the microphone callback after every downstream
	/// borrower is done with it. `None` for non-cpal capture sources.
	recycle: Option<ringbuf::HeapProd<Vec<f32>>>,
}

impl Samples {
	/// Samples whose allocation belongs to the ordinary async path.
	#[cfg(any(test, target_os = "macos"))]
	pub(crate) fn plain(data: Vec<f32>, gap: bool) -> Self {
		Self {
			data,
			gap,
			recycle: None,
		}
	}

	/// Samples borrowed from the microphone callback's fixed buffer pool.
	fn pooled(data: Vec<f32>, gap: bool, recycle: ringbuf::HeapProd<Vec<f32>>) -> Self {
		Self {
			data,
			gap,
			recycle: Some(recycle),
		}
	}

	/// Replace pooled samples with an async allocation, returning the old buffer
	/// before it gets dropped.
	pub(crate) fn replace(&mut self, data: Vec<f32>) {
		self.recycle();
		self.data = data;
	}

	fn recycle(&mut self) {
		let Some(mut recycle) = self.recycle.take() else {
			return;
		};

		let mut data = std::mem::take(&mut self.data);
		data.clear();
		if recycle.try_push(data).is_err() {
			// This is the async consumer, so freeing a buffer here is safe.
		}
	}
}

impl Drop for Samples {
	fn drop(&mut self) {
		self.recycle();
	}
}

/// The PCM layout delivered by one capture stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Layout {
	pub sample_rate: u32,
	pub channels: u32,
}

/// A capture failure plus whether reopening can succeed without caller action.
#[derive(Debug)]
pub(crate) enum Failure {
	Retry(Error),
	Fatal(Error),
}

impl Failure {
	pub(crate) fn retry(error: Error) -> Self {
		Self::Retry(error)
	}

	pub(crate) fn fatal(error: Error) -> Self {
		Self::Fatal(error)
	}

	fn cpal(error: cpal::Error) -> Self {
		let retryable = retryable(error.kind());
		let error = capture_err(error);
		if retryable {
			Self::Retry(error)
		} else {
			Self::Fatal(error)
		}
	}

	pub(crate) fn is_retryable(&self) -> bool {
		matches!(self, Self::Retry(_))
	}

	pub(crate) fn into_error(self) -> Error {
		match self {
			Self::Retry(error) | Self::Fatal(error) => error,
		}
	}
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
	/// The concrete microphone currently in use, if this is a microphone stream.
	pub(crate) fn device(&self) -> Option<&Device> {
		match self {
			Self::Microphone(mic) => Some(&mic.device),
			#[cfg(target_os = "macos")]
			Self::System(_) => None,
		}
	}

	/// The PCM layout this opened stream actually delivers.
	pub(crate) fn layout(&self) -> Layout {
		match self {
			Self::Microphone(mic) => mic.layout,
			#[cfg(target_os = "macos")]
			Self::System(system) => system.layout(),
		}
	}

	/// Await the next buffer, or `None` once the source stops. A microphone stream
	/// error is returned immediately even if the device delivers no more samples.
	/// Cancel-safe: drop the future to release the device.
	pub(crate) async fn read(&mut self) -> Result<Option<Samples>, Failure> {
		match self {
			Self::Microphone(mic) => mic.read().await,
			#[cfg(target_os = "macos")]
			Self::System(system) => Ok(system.read().await),
		}
	}
}

/// The format `config` will capture at, without opening the device, so the
/// catalog can be populated before anything turns on.
pub(crate) async fn format(config: &Config) -> Result<Layout, Failure> {
	match &config.source {
		Source::Microphone(device) => {
			let (device, config) = (device.clone(), config.clone());
			// cpal enumerates devices with blocking host I/O, so keep it off the
			// runtime's worker threads.
			tokio::task::spawn_blocking(move || {
				let (_, _, _, stream_config) = resolve(device.as_deref(), &config)?;
				Ok(Layout {
					sample_rate: stream_config.sample_rate,
					channels: stream_config.channels as u32,
				})
			})
			.await
			.map_err(|err| Failure::fatal(Error::Capture(format!("audio host thread failed: {err}"))))?
		}
		#[cfg(target_os = "macos")]
		Source::System => Ok(screencapture::SystemAudio::format(config.sample_rate, config.channels)),
		#[cfg(not(target_os = "macos"))]
		Source::System => Err(Failure::fatal(Error::Unsupported(
			"system audio capture is only supported on macOS".into(),
		))),
	}
}

/// Open the capture source described by `config`.
pub(crate) async fn open(config: &Config) -> Result<Stream, Failure> {
	match &config.source {
		Source::Microphone(device) => Ok(Stream::Microphone(Microphone::open(device.as_deref(), config).await?)),
		#[cfg(target_os = "macos")]
		Source::System => Ok(Stream::System(
			screencapture::SystemAudio::open(config.sample_rate, config.channels)
				.await
				.map_err(Failure::fatal)?,
		)),
		#[cfg(not(target_os = "macos"))]
		Source::System => Err(Failure::fatal(Error::Unsupported(
			"system audio capture is only supported on macOS".into(),
		))),
	}
}

/// An open microphone.
///
/// Holds the live `cpal` stream, which keeps capturing until it is dropped.
/// Buffers arrive from the realtime callback over an async channel.
pub(crate) struct Microphone {
	// Kept alive to keep capturing; dropping it stops the stream.
	_stream: cpal::Stream,
	reader: MicrophoneReader,
	/// The first buffer, captured during `open` to surface a permission failure
	/// as an error rather than a silent hang.
	pending: Option<Samples>,
	/// The format cpal negotiated for this stream generation.
	layout: Layout,
	/// The concrete device selected for this stream generation.
	device: Device,
}

/// The async half of a microphone stream, separate from the cpal handle so its
/// failure wakeup and stream-generation isolation can be tested without audio
/// hardware.
struct MicrophoneReader {
	rx: buffer::Reader,
	errors: kio::Consumer<Option<cpal::Error>>,
}

impl MicrophoneReader {
	/// Return the buffer consumed during open unless a stream error arrived in
	/// the meantime.
	async fn pending(&mut self, samples: Samples) -> Result<Option<Samples>, Failure> {
		tokio::select! {
			biased;
			Some(err) = failure(&self.errors) => Err(err),
			_ = std::future::ready(()) => Ok(Some(samples)),
		}
	}

	/// Race samples against cpal's error callback. Errors win if both are ready so
	/// a dead device is never kept alive just to drain already-buffered audio.
	async fn read(&mut self) -> Result<Option<Samples>, Failure> {
		let data = tokio::select! {
			biased;
			Some(err) = failure(&self.errors) => return Err(err),
			data = self.rx.recv() => data,
		};

		Ok(data)
	}
}

/// Await the first terminal cpal error for this stream generation.
async fn failure(errors: &kio::Consumer<Option<cpal::Error>>) -> Option<Failure> {
	errors
		.wait(|error| match error.as_ref() {
			Some(error) => Poll::Ready(error.clone()),
			None => Poll::Pending,
		})
		.await
		.ok()
		.map(Failure::cpal)
}

impl Microphone {
	/// Open (and start) the requested microphone.
	///
	/// The cpal calls run inline rather than going through [`blocking`]: they
	/// return as soon as the device starts, so the only real wait is the
	/// first-buffer await below.
	async fn open(selector: Option<&str>, config: &Config) -> Result<Self, Failure> {
		// Fail fast on a denied/restricted mic (macOS TCC) instead of opening a
		// stream that silently delivers nothing. A no-op on other platforms.
		permission::ensure_microphone_access().await.map_err(Failure::fatal)?;

		let (device, current, sample_format, stream_config) = resolve(selector, config)?;
		let sample_rate = stream_config.sample_rate;
		let channels = stream_config.channels as u32;

		// Tell the canceller what it's listening to before the first callback
		// arrives, so the buffers it needs are allocated off the audio thread.
		#[cfg(feature = "aec")]
		if let Some(aec) = &config.aec {
			aec.open(sample_rate, channels).map_err(Failure::fatal)?;
		}

		let (mut writer, rx) = buffer::channel(
			channels as usize,
			#[cfg(feature = "aec")]
			config.aec.clone(),
		);
		let error_tx = kio::Producer::new(None);
		let errors = error_tx.consume();
		let mut reader = MicrophoneReader { rx, errors };

		// The callback runs on cpal's realtime audio thread. Every format writes
		// into the same preallocated bounded pool.
		let stream = match sample_format {
			cpal::SampleFormat::F32 => {
				let errors = error_tx.clone();
				device.build_input_stream(
					stream_config,
					move |data: &[f32], _: &_| writer.write_f32(data),
					move |err| stream_err(&errors, err),
					None,
				)
			}
			cpal::SampleFormat::I16 => {
				let errors = error_tx.clone();
				device.build_input_stream(
					stream_config,
					move |data: &[i16], _: &_| writer.write_i16(data),
					move |err| stream_err(&errors, err),
					None,
				)
			}
			cpal::SampleFormat::U16 => {
				let errors = error_tx.clone();
				device.build_input_stream(
					stream_config,
					move |data: &[u16], _: &_| writer.write_u16(data),
					move |err| stream_err(&errors, err),
					None,
				)
			}
			other => {
				return Err(Failure::fatal(Error::Unsupported(format!(
					"unsupported input sample format {other:?}"
				))));
			}
		}
		.map_err(Failure::cpal)?;

		stream.play().map_err(Failure::cpal)?;

		// Await the first buffer to surface a permission failure (or dead device)
		// as an error rather than a silent hang in the capture loop.
		let pending = match tokio::time::timeout(FIRST_BUFFER_TIMEOUT, reader.read()).await {
			Ok(Ok(Some(samples))) => samples,
			Ok(Ok(None)) => {
				return Err(Failure::retry(Error::Capture(format!(
					"microphone {device} stopped before any samples"
				))));
			}
			Ok(Err(err)) => return Err(err),
			Err(_) => {
				return Err(Failure::fatal(Error::Capture(format!(
					"no samples from microphone {device} within {FIRST_BUFFER_TIMEOUT:?} (permission denied?)"
				))));
			}
		};

		tracing::info!(device = %device, sample_rate, channels, "opened microphone");

		Ok(Self {
			_stream: stream,
			reader,
			pending: Some(pending),
			layout: Layout { sample_rate, channels },
			device: current,
		})
	}

	/// Await the next buffer or stream error. Cancel-safe: drop the future to stop
	/// reading.
	async fn read(&mut self) -> Result<Option<Samples>, Failure> {
		if let Some(samples) = self.pending.take() {
			return self.reader.pending(samples).await;
		}

		self.reader.read().await
	}
}

/// An audio input reported by [`devices`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
	/// Opaque identifier: pass to [`Source::Microphone`].
	///
	/// This is cpal's `host:device` id, so it is stable across restarts and
	/// unique even when two inputs share a [`name`](Self::name).
	pub id: String,
	/// Human-readable name, e.g. "MacBook Pro Microphone".
	pub name: String,
	/// Whether this is the system default input.
	///
	/// True for at most one device: the preferred host's default. Another host's
	/// default is that host's, not the system's.
	pub default: bool,
	/// The host API this device is reached through, e.g. "PipeWire" or "ALSA".
	///
	/// The same hardware is usually reachable through several, so a caller that
	/// offers a choice groups by this.
	pub host: String,
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
	// Every host, not just the preferred one, matching the output side. The same
	// hardware appears under each, and which one a caller wants is its decision:
	// PipeWire and PulseAudio carry the server's own names and routing, ALSA
	// reaches a device directly. `Device::host` is what lets a caller group them.
	let preferred = cpal::default_host().id();
	let mut devices = Vec::new();
	// A sound server reports one id per stream, not per device, so a client with
	// several open would otherwise appear once per stream.
	let mut seen = std::collections::HashSet::new();

	for id in cpal::available_hosts() {
		// A host that will not open takes every device on it with it, so say so:
		// the symptom is a device missing from the listing with no other trace.
		let host = match cpal::host_from_id(id) {
			Ok(host) => host,
			Err(err) => {
				tracing::debug!(host = id.name(), error = %err, "skipping an audio host that would not open");
				continue;
			}
		};
		let default = host.default_input_device().and_then(|device| device.id().ok());

		let inputs = match host.input_devices() {
			Ok(inputs) => inputs,
			Err(err) => {
				tracing::debug!(host = id.name(), error = %err, "skipping a host that would not list its inputs");
				continue;
			}
		};
		for device in inputs {
			let device_id = match device.id() {
				Ok(device_id) => device_id,
				Err(err) => {
					tracing::debug!(host = id.name(), error = %err, "skipping an input device with no id");
					continue;
				}
			};
			if !seen.insert(device_id.to_string()) {
				continue;
			}
			// Only the preferred host's default is the system default; the others
			// are that host's idea of one.
			let is_default = id == preferred && Some(&device_id) == default.as_ref();
			match describe(&device, &device_id, is_default) {
				Ok(device) => devices.push(device),
				Err(err) => {
					tracing::debug!(error = %err, "skipping an input device that could not be described");
				}
			}
		}
	}

	Ok(devices)
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
) -> Result<(cpal::Device, Device, cpal::SampleFormat, cpal::StreamConfig), Failure> {
	let host = cpal::default_host();
	let default = host.default_input_device().and_then(|device| device.id().ok());
	let (device, id) = match selector {
		// `Host::device_by_id` searches outputs too, so match against the inputs
		// ourselves: an output id must not resolve as a microphone.
		Some(selector) => {
			let wanted: cpal::DeviceId = selector.parse().map_err(|err| {
				Failure::fatal(Error::Device(format!(
					"{selector:?} is not an input device id; run `devices` to list them: {err}"
				)))
			})?;
			// Ids are host-qualified, so route to the host that issued this one
			// rather than searching the preferred host alone: `devices` lists
			// every host, and a device it named has to be openable.
			let host = cpal::host_from_id(wanted.host())
				.map_err(|err| Failure::fatal(Error::Device(format!("{selector:?}: {err}"))))?;
			let device = host
				.input_devices()
				.map_err(Failure::cpal)?
				.find(|device| device.id().ok().as_ref() == Some(&wanted))
				.ok_or_else(|| Failure::retry(Error::Device(format!("input device {selector:?} not found"))))?;
			(device, wanted)
		}
		None => {
			let device = host
				.default_input_device()
				.ok_or_else(|| Failure::retry(Error::Device("no default input device".into())))?;
			let id = device.id().map_err(Failure::cpal)?;
			(device, id)
		}
	};
	let current = describe(&device, &id, Some(&id) == default.as_ref()).map_err(Failure::cpal)?;

	let supported = device.default_input_config().map_err(Failure::cpal)?;
	let sample_format = supported.sample_format();
	let mut stream_config = supported.config();
	if let Some(rate) = config.sample_rate {
		stream_config.sample_rate = rate;
	}
	if let Some(channels) = config.channels {
		stream_config.channels = channels as u16;
	}
	Ok((device, current, sample_format, stream_config))
}

/// Build the listing entry for `device`, whose id the caller has already read.
fn describe(device: &cpal::Device, id: &cpal::DeviceId, default: bool) -> Result<Device, cpal::Error> {
	Ok(Device {
		default,
		name: device.description()?.name().into(),
		host: id.host().name().to_string(),
		id: id.to_string(),
	})
}

fn stream_err(errors: &kio::Producer<Option<cpal::Error>>, err: cpal::Error) {
	if survivable(err.kind()) {
		tracing::warn!(error = %err, "microphone stream error does not require a restart");
		return;
	}

	tracing::error!(error = %err, "microphone stream error");
	let Ok(mut failure) = errors.write() else { return };
	if failure.is_some() {
		return;
	}
	*failure = Some(err);
	failure.close();
}

/// Errors for which cpal documents that the live stream remains usable.
fn survivable(kind: cpal::ErrorKind) -> bool {
	matches!(
		kind,
		cpal::ErrorKind::DeviceChanged | cpal::ErrorKind::RealtimeDenied | cpal::ErrorKind::Xrun
	)
}

/// Errors that can clear after the device or host state changes by itself.
fn retryable(kind: cpal::ErrorKind) -> bool {
	matches!(
		kind,
		cpal::ErrorKind::DeviceBusy
			| cpal::ErrorKind::DeviceNotAvailable
			| cpal::ErrorKind::HostUnavailable
			| cpal::ErrorKind::ResourceExhausted
			| cpal::ErrorKind::StreamInvalidated
	)
}

fn capture_err(err: impl std::fmt::Display) -> Error {
	Error::Capture(err.to_string())
}

#[cfg(test)]
mod tests {
	use super::*;

	fn reader() -> (buffer::Writer, kio::Producer<Option<cpal::Error>>, MicrophoneReader) {
		let (tx, rx) = buffer::channel(
			1,
			#[cfg(feature = "aec")]
			None,
		);
		let failures = kio::Producer::new(None);
		let errors = failures.consume();
		(tx, failures, MicrophoneReader { rx, errors })
	}

	fn fail(errors: &kio::Producer<Option<cpal::Error>>, message: &'static str) {
		stream_err(
			errors,
			cpal::Error::with_message(cpal::ErrorKind::DeviceNotAvailable, message),
		);
	}

	#[tokio::test]
	async fn stream_error_wakes_a_reader_without_samples() {
		let (_samples, errors, mut reader) = reader();
		fail(&errors, "device lost");

		let err = match reader.read().await {
			Err(err) => err.into_error(),
			Ok(_) => panic!("the reader ignored its stream error"),
		};
		assert!(matches!(err, Error::Capture(message) if message == "device lost"));
	}

	#[tokio::test]
	async fn replaced_stream_cannot_fail_its_replacement() {
		let (_old_samples, old_errors, old_reader) = reader();
		let (mut new_samples, _new_errors, mut new_reader) = reader();
		drop(old_reader);

		fail(&old_errors, "stale");
		new_samples.write_f32(&[1.0]);
		let samples = new_reader.read().await.unwrap().unwrap();
		assert_eq!(samples.data, vec![1.0]);
	}

	#[tokio::test]
	async fn stream_error_wins_over_the_buffer_saved_during_open() {
		let (_samples, errors, mut reader) = reader();
		fail(&errors, "device lost");

		let result = reader.pending(Samples::plain(vec![1.0], false)).await;
		let err = match result {
			Err(err) => err.into_error(),
			Ok(_) => panic!("the pending sample hid a stream error"),
		};
		assert!(matches!(err, Error::Capture(message) if message == "device lost"));
	}

	#[test]
	fn survivable_errors_do_not_end_the_stream() {
		let (_samples, errors, _reader) = reader();
		stream_err(&errors, cpal::Error::new(cpal::ErrorKind::DeviceChanged));

		assert!(errors.read().is_none());
	}

	#[test]
	fn permission_errors_are_not_retryable() {
		let failure = Failure::cpal(cpal::Error::new(cpal::ErrorKind::PermissionDenied));
		assert!(!failure.is_retryable());
	}

	#[test]
	fn opaque_backend_errors_are_not_retryable() {
		let failure = Failure::cpal(cpal::Error::new(cpal::ErrorKind::BackendError));
		assert!(!failure.is_retryable());
	}
}
