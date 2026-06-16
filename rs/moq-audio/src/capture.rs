//! Microphone capture via [`cpal`] (pure-Rust: CoreAudio / WASAPI / ALSA).
//!
//! [`Microphone`] opens an input device and yields interleaved-`f32` PCM
//! [`Frame`]s, ready to feed an [`AudioProducer`] with
//! an [`EncoderInput`] of `format = AudioFormat::F32`.
//! Encoding stays on `unsafe-libopus`, so audio never touches ffmpeg.

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::{AudioError, AudioFormat, AudioProducer, EncoderInput, EncoderOutput, Frame};

mod permission;

/// How long `open` waits for the first buffer before assuming the mic never
/// started (e.g. permission denied), mirroring the camera path's first-frame
/// timeout. Without this the capture loop hangs silently forever when macOS TCC
/// denies microphone access.
const FIRST_BUFFER_TIMEOUT: Duration = Duration::from_secs(5);

/// Microphone capture configuration. All fields are hints; the backend picks
/// the closest supported mode and the [`AudioProducer`]
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
	/// The first buffer, captured during `open` to surface a permission failure
	/// as an error rather than a silent hang.
	pending: Option<Vec<f32>>,
}

impl Microphone {
	/// The device's negotiated format `(sample_rate, channels)` without opening
	/// a stream, so the catalog can be populated before the mic is turned on.
	pub fn format(config: &Config) -> Result<(u32, u32), AudioError> {
		let (_, _, stream_config) = resolve(config)?;
		Ok((stream_config.sample_rate, stream_config.channels as u32))
	}

	/// Open (and start) the microphone described by `config`.
	pub fn open(config: &Config) -> Result<Self, AudioError> {
		// Fail fast on a denied/restricted mic (macOS TCC) instead of opening a
		// stream that silently delivers nothing. A no-op on other platforms.
		permission::ensure_microphone_access()?;

		let (device, sample_format, stream_config) = resolve(config)?;
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

		// Block for the first buffer to surface a permission failure (or dead
		// device) as an error rather than a silent hang in the capture loop.
		let pending = match rx.recv_timeout(FIRST_BUFFER_TIMEOUT) {
			Ok(samples) => samples,
			Err(RecvTimeoutError::Timeout) => {
				return Err(AudioError::Unsupported(format!(
					"no samples from microphone {device} within {FIRST_BUFFER_TIMEOUT:?} (permission denied?)"
				)));
			}
			Err(RecvTimeoutError::Disconnected) => {
				return Err(AudioError::Unsupported(format!(
					"microphone {device} stopped before any samples"
				)));
			}
		};

		tracing::info!(device = %device, sample_rate, channels, "opened microphone");

		Ok(Self {
			_stream: stream,
			rx,
			sample_rate,
			channels,
			frames_read: 0,
			pending: Some(pending),
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
		let samples = match self.pending.take() {
			Some(samples) => samples,
			None => {
				let Ok(samples) = self.rx.recv() else {
					return Ok(None); // stream dropped / device gone
				};
				samples
			}
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

/// Capture the microphone on demand and publish it as an Opus moq track named
/// `track_name`.
///
/// The catalog rendition is registered up front from the device's reported
/// format (no capture needed), but the mic only opens while a subscriber is
/// listening and is released when the last one leaves. On resume the timeline
/// re-anchors (via [`AudioProducer::reset_epoch`]) so the idle gap lands in the
/// PTS, keeping audio aligned with a wall-clock video track.
///
/// The capture-side settings come from [`Config`]; the encode-side settings
/// (codec, bitrate, frame duration) from [`EncoderOutput`]. Returns when the
/// broadcast is dropped or the capture loop fails.
pub async fn publish_microphone(
	mut broadcast: moq_net::BroadcastProducer,
	catalog: moq_mux::catalog::Producer,
	config: Config,
	track_name: impl Into<String>,
	output: EncoderOutput,
	clock: moq_mux::Clock,
) -> Result<(), AudioError> {
	let (sample_rate, channels) = Microphone::format(&config)?;
	let input = EncoderInput {
		format: AudioFormat::F32,
		sample_rate,
		channels,
	};

	let producer = AudioProducer::new(&mut broadcast, catalog, track_name, input, output)?;
	let track = producer.track().clone();

	let gate = Gate::new();
	let worker_gate = gate.clone();
	let mut worker = tokio::task::spawn_blocking(move || capture_loop(producer, config, worker_gate, clock));

	tokio::select! {
		res = &mut worker => res.map_err(task_err)?,
		() = monitor_demand(&track, &gate) => {
			gate.close();
			worker.await.map_err(task_err)?
		}
	}
}

/// Toggle the gate as listeners subscribe and unsubscribe. Returns once the
/// track stops being announced (broadcast dropped / aborted).
async fn monitor_demand(track: &moq_net::TrackProducer, gate: &Gate) {
	loop {
		match track.used().await {
			Ok(()) => gate.set_active(true),
			Err(err) => return log_track_ended(err),
		}
		match track.unused().await {
			Ok(()) => gate.set_active(false),
			Err(err) => return log_track_ended(err),
		}
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

/// Blocking capture/encode loop. Opens the mic only while watched, releases it
/// when idle, and stamps frames with wall-clock time so a release/reopen gap
/// shows up in the PTS.
fn capture_loop(
	mut producer: AudioProducer,
	config: Config,
	gate: Arc<Gate>,
	clock: moq_mux::Clock,
) -> Result<(), AudioError> {
	let mut mic: Option<Microphone> = None;

	loop {
		if !gate.is_active() {
			if mic.take().is_some() {
				// Re-anchor so the next frame after resume reflects the gap.
				producer.reset_epoch();
				tracing::info!("no listeners: released microphone");
			}
			if !gate.wait_active() {
				break; // closed
			}
			continue;
		}

		if mic.is_none() {
			mic = Some(Microphone::open(&config)?);
		}

		let Some(mut frame) = mic.as_mut().expect("mic open above").read()? else {
			break; // device stopped producing samples
		};

		// Stamp from the shared clock (including any idle gap) so the producer's
		// epoch re-anchors and audio stays aligned with the video track.
		frame.timestamp_us = clock.micros();
		producer.write(&frame)?;
	}

	producer.finish()?;
	Ok(())
}

fn task_err(err: tokio::task::JoinError) -> AudioError {
	AudioError::Unsupported(format!("capture task: {err}"))
}

/// Bridges the async demand monitor to the blocking capture thread: the monitor
/// flips `active`, the capture loop waits on it.
struct Gate {
	state: Mutex<GateState>,
	cond: Condvar,
}

#[derive(Default)]
struct GateState {
	active: bool,
	closed: bool,
}

impl Gate {
	fn new() -> Arc<Self> {
		Arc::new(Self {
			state: Mutex::new(GateState::default()),
			cond: Condvar::new(),
		})
	}

	fn set_active(&self, active: bool) {
		let mut state = self.state.lock().unwrap();
		state.active = active;
		self.cond.notify_all();
	}

	fn close(&self) {
		let mut state = self.state.lock().unwrap();
		state.active = false;
		state.closed = true;
		self.cond.notify_all();
	}

	fn is_active(&self) -> bool {
		self.state.lock().unwrap().active
	}

	/// Block until active or closed. Returns `false` if closed.
	fn wait_active(&self) -> bool {
		let mut state = self.state.lock().unwrap();
		while !state.active && !state.closed {
			state = self.cond.wait(state).unwrap();
		}
		!state.closed
	}
}

/// Forward a buffer to the reader, ignoring send errors (receiver dropped means
/// capture is shutting down).
fn forward(tx: &Sender<Vec<f32>>, samples: Vec<f32>) {
	let _ = tx.send(samples);
}

/// Resolve the input device and its negotiated stream config from `config`.
fn resolve(config: &Config) -> Result<(cpal::Device, cpal::SampleFormat, cpal::StreamConfig), AudioError> {
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
	Ok((device, sample_format, stream_config))
}

fn stream_err(err: cpal::Error) {
	tracing::error!(error = %err, "microphone stream error");
}

fn cpal_err(err: cpal::Error) -> AudioError {
	AudioError::Unsupported(format!("audio capture: {err}"))
}
