//! The thread that owns the cpal output stream.
//!
//! A `cpal::Stream` is `!Send`, so it has to live on the thread that built it.
//! That thread also owns everything slow or fallible about the device: opening
//! it, switching it, and rebuilding it after an error. Sinks never talk to it on
//! the hot path; they register themselves in [`Shared`] and hand their consumer
//! straight to the mixer.

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, StreamTrait};

use super::mixer::{self, Mixer};
use super::sink::{Registration, Sink};
use crate::Error;

/// Backoff bounds for reopening a device that failed. The first retry is quick
/// because the common case is a device that came right back (a USB re-enumerate,
/// a sample-rate change); the ceiling keeps a permanently gone device from
/// spinning.
const RETRY_MIN: Duration = Duration::from_millis(500);
const RETRY_MAX: Duration = Duration::from_secs(4);

/// Underruns tolerated in [`UNDERRUN_WINDOW`] before the stream is rebuilt. A
/// few are normal under load; a stream of them means the device is wedged.
const UNDERRUN_LIMIT: u32 = 20;
const UNDERRUN_WINDOW: Duration = Duration::from_secs(5);

/// Frames the sample-format conversion buffer holds. Comfortably more than any
/// host's period, so the loop over it almost always runs once.
const SCRATCH_FRAMES: usize = 2048;

/// State shared between the caller's [`Engine`](super::Engine) handles, their
/// sinks, and the driver thread.
///
/// One mutex, held only for pointer swaps and list edits, never across a cpal
/// call. That keeps [`Engine::sink`](super::Engine::sink) synchronous and quick
/// even while the driver is opening a device.
#[derive(Default)]
pub(super) struct Shared {
	state: Mutex<State>,
}

#[derive(Default)]
struct State {
	/// Rate the device is running at, which is what sinks resample to. Zero
	/// until the first stream opens.
	rate: u32,
	/// Registration channel to the live mixer, replaced every time the stream is
	/// rebuilt. `None` while no stream is running.
	mixer: Option<SyncSender<mixer::Command>>,
	/// Every live sink, so a rebuild can re-create their channels at the new
	/// device rate.
	sinks: Vec<Registration>,
	next_id: u64,
}

impl Shared {
	/// Build a sink, register it, and start mixing it.
	///
	/// `build` is handed the sink's id and the rate its channel should target.
	/// It runs with no device open too: the registration waits for the next
	/// restart, so a device that is briefly missing doesn't become an error the
	/// caller has to retry.
	pub(super) fn add<F>(&self, build: F) -> Result<Sink, Error>
	where
		F: FnOnce(u64, u32) -> Result<(Sink, Registration), Error>,
	{
		let mut state = self.state.lock().unwrap();

		// 48 kHz stands in until a device opens and the channel is rebuilt at
		// the real rate.
		let rate = if state.rate == 0 { 48_000 } else { state.rate };
		let (sink, mut registration) = build(state.next_id, rate)?;
		state.next_id += 1;

		if let Some(mixer) = &state.mixer {
			registration.attach(mixer);
		}

		state.sinks.push(registration);
		Ok(sink)
	}

	/// Stop mixing the sink with this id, called when the caller drops it.
	pub(super) fn remove(&self, id: u64) {
		let mut state = self.state.lock().unwrap();
		state.sinks.retain(|s| s.id != id);
		if let Some(mixer) = &state.mixer {
			let _ = mixer.try_send(mixer::Command::Remove { id });
		}
	}

	/// Point every sink at a freshly opened stream: rebuild each channel at
	/// `rate` and hand the new consumers to `mixer`.
	fn rebind(&self, rate: u32, mixer: SyncSender<mixer::Command>) {
		let mut state = self.state.lock().unwrap();
		for sink in &mut state.sinks {
			sink.rebuild(rate);
			sink.attach(&mixer);
		}
		state.rate = rate;
		state.mixer = Some(mixer);
	}

	/// Forget the running stream, so sinks registered while the device is down
	/// wait for the next one instead of writing into a dead mixer.
	fn unbind(&self) {
		self.state.lock().unwrap().mixer = None;
	}
}

/// What the driver thread waits on.
pub(super) enum Command {
	/// Move to another output device, or back to the system default with `None`.
	Switch {
		device: Option<String>,
		reply: tokio::sync::oneshot::Sender<Result<(), Error>>,
	},
	/// The audio thread reported a problem. Sent from cpal's error callback,
	/// which is why the driver wakes on it instead of polling.
	Failed(cpal::Error),
	/// The last [`Engine`](super::Engine) and [`Sink`] are gone.
	///
	/// An explicit message rather than watching the channel disconnect: every
	/// live stream's error callback holds a sender of its own, so the channel
	/// only closes once the driver has already dropped the device.
	Shutdown,
}

/// Run the output device until every [`Engine`](super::Engine) and
/// [`Sink`](super::Sink) has been dropped.
///
/// `opened` reports whether the first device came up, so
/// [`Engine::open`](super::Engine::open) can fail fast on a machine with no
/// output rather than handing back a handle that plays into nothing.
pub(super) fn run(
	commands: Receiver<Command>,
	failures: Sender<Command>,
	shared: Arc<Shared>,
	device: Option<String>,
	opened: tokio::sync::oneshot::Sender<Result<(), Error>>,
) {
	let mut driver = Driver {
		shared,
		failures,
		device,
		stream: None,
		retry: RETRY_MIN,
		underruns: 0,
		window: Instant::now(),
	};

	let first = driver.start();
	let started = first.is_ok();
	if opened.send(first).is_err() || !started {
		// Either the caller gave up on `open`, or there is no device to play
		// out of. Nothing to drive either way.
		return;
	}

	// A failed start schedules a retry; until then the thread just blocks.
	let mut retry_at: Option<Instant> = None;

	loop {
		let command = match retry_at {
			Some(at) => driver.commands_until(&commands, at),
			None => commands.recv().map_err(|_| Timeout::Disconnected),
		};

		match command {
			Ok(Command::Switch { device, reply }) => {
				driver.device = device;
				// Drop the old stream first: some hosts refuse to open a second
				// stream while one is live, and the caller asked to leave anyway.
				driver.stop();
				let result = driver.start();
				retry_at = result.is_err().then(|| driver.schedule());
				let _ = reply.send(result);
			}
			Ok(Command::Failed(err)) => {
				if driver.fatal(&err) {
					driver.stop();
					retry_at = driver.start().err().map(|_| driver.schedule());
				}
			}
			Ok(Command::Shutdown) => break,
			Err(Timeout::Elapsed) => {
				retry_at = match driver.start() {
					Ok(()) => {
						tracing::info!("audio output recovered");
						None
					}
					Err(err) => {
						tracing::debug!(%err, "audio output still unavailable");
						Some(driver.schedule())
					}
				};
			}
			Err(Timeout::Disconnected) => break,
		}
	}
}

enum Timeout {
	Elapsed,
	Disconnected,
}

struct Driver {
	shared: Arc<Shared>,
	/// Handed to each stream's error callback so failures arrive as commands.
	failures: Sender<Command>,
	device: Option<String>,
	/// The live stream. Dropping it stops the audio thread.
	stream: Option<cpal::Stream>,
	retry: Duration,
	underruns: u32,
	window: Instant,
}

impl Driver {
	fn commands_until(&self, commands: &Receiver<Command>, at: Instant) -> Result<Command, Timeout> {
		match commands.recv_timeout(at.saturating_duration_since(Instant::now())) {
			Ok(command) => Ok(command),
			Err(RecvTimeoutError::Timeout) => Err(Timeout::Elapsed),
			Err(RecvTimeoutError::Disconnected) => Err(Timeout::Disconnected),
		}
	}

	/// Open the device, start mixing into it, and move every sink onto it.
	fn start(&mut self) -> Result<(), Error> {
		let device = super::device::open(self.device.as_deref())?;
		let supported = super::device::negotiate(&device)?;

		let format = supported.sample_format();
		let config: cpal::StreamConfig = supported.into();
		let rate = config.sample_rate;
		let channels = config.channels as usize;

		if rate == 0 || channels == 0 {
			return Err(Error::Playback(format!(
				"output device negotiated an empty format ({rate} Hz, {channels} channels)"
			)));
		}

		// Build with an empty mixer, then hand it the sinks: the callback drains
		// its command channel on every pass, so registration does not race the
		// build.
		let (tx, rx) = sync_channel(64);
		let mixer = Mixer::new(rx, rate, channels);

		let stream = self.build(&device, config, format, mixer)?;
		stream
			.play()
			.map_err(|err| Error::Playback(format!("cannot start output stream: {err}")))?;

		self.shared.rebind(rate, tx);
		self.stream = Some(stream);
		self.retry = RETRY_MIN;

		tracing::info!(rate, channels, ?format, "opened audio output");
		Ok(())
	}

	/// Build the stream in whatever sample format the device wants, converting
	/// from the mixer's `f32` on the way out.
	fn build(
		&self,
		device: &cpal::Device,
		config: cpal::StreamConfig,
		format: cpal::SampleFormat,
		mixer: Mixer,
	) -> Result<cpal::Stream, Error> {
		match format {
			cpal::SampleFormat::F32 => self.build_as::<f32>(device, config, mixer),
			cpal::SampleFormat::I16 => self.build_as::<i16>(device, config, mixer),
			cpal::SampleFormat::U16 => self.build_as::<u16>(device, config, mixer),
			cpal::SampleFormat::I32 => self.build_as::<i32>(device, config, mixer),
			other => Err(Error::Unsupported(format!("output sample format {other:?}"))),
		}
	}

	fn build_as<T>(
		&self,
		device: &cpal::Device,
		config: cpal::StreamConfig,
		mut mixer: Mixer,
	) -> Result<cpal::Stream, Error>
	where
		T: cpal::SizedSample + cpal::FromSample<f32>,
	{
		let failures = self.failures.clone();

		// The mixer works in `f32`, so anything else needs a staging buffer.
		// Allocated once and a whole number of frames long, so however big a
		// buffer the device asks for, the callback loops over this rather than
		// resizing (allocating on the audio thread is the one thing it must
		// never do).
		let mut scratch = vec![0.0f32; SCRATCH_FRAMES * config.channels as usize];

		device
			.build_output_stream::<T, _, _>(
				config,
				move |data, _| {
					for chunk in data.chunks_mut(scratch.len()) {
						let scratch = &mut scratch[..chunk.len()];
						mixer.fill(scratch);
						for (out, sample) in chunk.iter_mut().zip(scratch.iter()) {
							*out = T::from_sample(*sample);
						}
					}
				},
				move |err| {
					// This is cpal's error callback, not the audio callback, so
					// an allocating send is fine here.
					let _ = failures.send(Command::Failed(err));
				},
				None,
			)
			.map_err(|err| Error::Playback(format!("cannot open output stream: {err}")))
	}

	/// Tear the stream down and detach every sink from it.
	fn stop(&mut self) {
		self.shared.unbind();
		self.stream = None;
	}

	/// When the next restart may be attempted, doubling the backoff.
	fn schedule(&mut self) -> Instant {
		let at = Instant::now() + self.retry;
		self.retry = (self.retry * 2).min(RETRY_MAX);
		at
	}

	/// Whether this error means the stream has to be rebuilt.
	fn fatal(&mut self, err: &cpal::Error) -> bool {
		match err.kind() {
			// One underrun is a glitch, not a broken device. Only a sustained
			// run of them is worth interrupting playback to fix.
			cpal::ErrorKind::Xrun => {
				if self.window.elapsed() > UNDERRUN_WINDOW {
					self.underruns = 0;
					self.window = Instant::now();
				}
				self.underruns += 1;
				let restart = self.underruns > UNDERRUN_LIMIT;
				if restart {
					self.underruns = 0;
					tracing::warn!("restarting audio output after repeated underruns");
				}
				restart
			}
			cpal::ErrorKind::DeviceNotAvailable | cpal::ErrorKind::StreamInvalidated => {
				tracing::warn!(%err, "audio output lost");
				true
			}
			_ => {
				tracing::warn!(%err, "audio output error");
				false
			}
		}
	}
}
