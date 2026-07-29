//! The thread that owns the cpal output stream.
//!
//! A `cpal::Stream` is `!Send`, so it has to live on the thread that built it.
//! That thread also owns everything slow or fallible about the device: opening
//! it, switching it, and rebuilding it after an error. Sinks never talk to it on
//! the hot path; they register themselves in [`Shared`] and hand their consumer
//! straight to the mixer.

#[cfg(feature = "aec")]
use std::sync::mpsc::TrySendError;
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

/// Problems tolerated in [`ERROR_WINDOW`] before the stream is rebuilt.
///
/// Underruns get a long rope because a few are normal under load. Errors we
/// can't classify get a short one: they may well be terminal, and without an
/// escalation the stream would sit dead with nothing but a warning to show for
/// it, since nothing else wakes the driver.
const UNDERRUN_LIMIT: u32 = 20;
const ERROR_LIMIT: u32 = 3;
const ERROR_WINDOW: Duration = Duration::from_secs(5);

/// Sink updates the mixer's command queue holds. Preallocated, since draining it
/// happens on the audio thread. Deep enough that only a burst of registrations
/// between two device periods can fill it, and [`Driver::sync`] covers that.
const COMMAND_QUEUE: usize = 2 * mixer::MAX_SINKS;

/// How hard the driver tries to push a sink update the mixer's command queue was
/// too full to take. It drains every callback, so a couple of device periods is
/// already generous.
const SYNC_ATTEMPTS: u32 = 8;
const SYNC_DELAY: Duration = Duration::from_millis(4);

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
pub(crate) struct Shared {
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
	/// Sinks the mixer has not been told to drop yet, because its command queue
	/// was full. Retried by [`Shared::sync`].
	detaching: Vec<u64>,
	next_id: u64,
	/// The echo-cancellation tap, rebuilt alongside the sinks. At most one:
	/// there is one mix, and one microphone hearing it.
	#[cfg(feature = "aec")]
	reference: Option<crate::aec::Reference>,
	/// Set when the mixer has not been told to drop the tap yet, because its
	/// command queue was full. Retried by [`Shared::sync`].
	#[cfg(feature = "aec")]
	detaching_reference: bool,
	/// Posts [`Command::Sync`] so a retry actually happens. A plain sender, not
	/// a [`Handle`](super::Handle): a canceller must not keep the output device
	/// open, and shutdown is an explicit message rather than a disconnect.
	///
	/// `None` in tests that drive [`Shared`] without a driver behind it.
	#[cfg(feature = "aec")]
	waker: Option<Sender<Command>>,
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

		// The mixer sizes its entry list once so it never allocates on the audio
		// thread, so the limit has to be refused here, loudly, rather than
		// discovered there as a sink that plays nothing.
		if state.sinks.len() >= mixer::MAX_SINKS {
			return Err(Error::Unsupported(format!(
				"at most {} playback sinks per device",
				mixer::MAX_SINKS
			)));
		}

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

		let Some(mixer) = &state.mixer else { return };
		if mixer.try_send(mixer::Command::Remove { id }).is_err() {
			// Queue full. Remember it: dropping it here would leave the mixer
			// reading a sink nobody owns for the life of the stream.
			state.detaching.push(id);
		}
	}

	/// Remember the channel that posts [`Command::Sync`], so a send the mixer's
	/// command queue was too full to take gets retried.
	#[cfg(feature = "aec")]
	pub(super) fn wake_with(&self, waker: Sender<Command>) {
		self.state.lock().unwrap().waker = Some(waker);
	}

	/// Ask the driver to retry whatever didn't get through.
	///
	/// The sink paths do this from [`Engine::sink`](super::Engine::sink) and
	/// `Sink::drop`, which hold a [`Handle`](super::Handle). A canceller holds
	/// no handle by design, so its retries are posted from here instead.
	#[cfg(feature = "aec")]
	fn wake(state: &State) {
		if let Some(waker) = &state.waker {
			let _ = waker.send(Command::Sync);
		}
	}

	/// Start feeding an echo canceller the mix, replacing any previous one.
	///
	/// Registers with no device open too: the tap waits for the next restart,
	/// exactly as a sink does.
	#[cfg(feature = "aec")]
	pub(crate) fn set_reference(&self, mut reference: crate::aec::Reference) {
		let mut state = self.state.lock().unwrap();
		if let Some(mixer) = &state.mixer
			&& state.rate != 0
			&& reference.rebuild(state.rate)
		{
			attach_reference(&mut reference, mixer);
		}

		// A replaced canceller is already detached: the mixer takes whichever
		// producer arrives last.
		state.detaching_reference = false;
		state.reference = Some(reference);

		// Covers the case where the mixer's command queue was momentarily full,
		// so a canceller is never left silently unattached.
		Self::wake(&state);
	}

	/// Stop feeding the canceller with this id, called when its last clone drops.
	///
	/// A no-op once a newer canceller has taken the slot: the one going away is
	/// already detached, and taking the tap with it would silently break the one
	/// that replaced it.
	#[cfg(feature = "aec")]
	pub(crate) fn clear_reference(&self, id: u64) {
		let mut state = self.state.lock().unwrap();
		if !state.reference.as_ref().is_some_and(|r| r.owned_by(id)) {
			return;
		}

		state.reference = None;
		let Some(mixer) = &state.mixer else { return };
		if mixer.try_send(mixer::Command::Reference(None)).is_err() {
			// Queue full. Remember it: dropping it here would leave the mixer
			// filling a ring nobody reads for the life of the stream.
			state.detaching_reference = true;
		}

		// The mixer hands the retired tap back rather than dropping it on the
		// audio thread, so somebody has to come collect it, and any failed send
		// above still needs retrying.
		Self::wake(&state);
	}

	/// Re-send whatever the mixer's command queue was too full to take.
	///
	/// Returns whether everything is now through. The driver calls this after a
	/// sink is added or dropped, so a full queue costs a retry rather than a
	/// sink that is silent (or one that never stops) until the next device
	/// restart.
	pub(super) fn sync(&self) -> bool {
		let mut state = self.state.lock().unwrap();
		let Some(mixer) = state.mixer.clone() else {
			// No stream to talk to. Registrations stay pending and `rebind`
			// picks them up when one opens.
			state.detaching.clear();
			#[cfg(feature = "aec")]
			{
				state.detaching_reference = false;
			}
			return true;
		};

		state
			.detaching
			.retain(|id| mixer.try_send(mixer::Command::Remove { id: *id }).is_err());
		for sink in &mut state.sinks {
			sink.attach(&mixer);
		}

		#[cfg(feature = "aec")]
		{
			if state.detaching_reference {
				state.detaching_reference = mixer.try_send(mixer::Command::Reference(None)).is_err();
			}
			if let Some(reference) = &mut state.reference {
				attach_reference(reference, &mixer);
			}
		}

		let done = state.detaching.is_empty() && state.sinks.iter().all(|s| s.attached());
		#[cfg(feature = "aec")]
		let done = done && !state.detaching_reference && state.reference.as_ref().is_none_or(|r| r.attached());
		done
	}

	/// Point every sink at a freshly opened stream: rebuild each channel at
	/// `rate` and hand the new consumers to `mixer`.
	fn rebind(&self, rate: u32, mixer: SyncSender<mixer::Command>) {
		let mut state = self.state.lock().unwrap();
		for sink in &mut state.sinks {
			sink.rebuild(rate);
			sink.attach(&mixer);
		}

		#[cfg(feature = "aec")]
		if let Some(reference) = &mut state.reference {
			if reference.rebuild(rate) {
				attach_reference(reference, &mixer);
			} else {
				// The canceller went away without us noticing.
				state.reference = None;
			}
		}

		state.rate = rate;
		state.mixer = Some(mixer);
		// The old mixer is gone, and with it every sink it was told about.
		state.detaching.clear();
		#[cfg(feature = "aec")]
		{
			state.detaching_reference = false;
		}
	}

	/// Forget the running stream, so sinks registered while the device is down
	/// wait for the next one instead of writing into a dead mixer.
	fn unbind(&self) {
		self.state.lock().unwrap().mixer = None;
	}

	/// Whether an echo canceller is registered, for the tests in [`crate::aec`].
	#[cfg(all(test, feature = "aec"))]
	pub(crate) fn has_reference(&self) -> bool {
		self.state.lock().unwrap().reference.is_some()
	}
}

/// Hand the tap's producer to a running mixer, keeping it if the mixer is
/// backed up so the next attach retries. The sink-side equivalent lives on
/// [`Registration::attach`].
#[cfg(feature = "aec")]
fn attach_reference(reference: &mut crate::aec::Reference, mixer: &SyncSender<mixer::Command>) {
	let Some(prod) = reference.take() else { return };
	if let Err(err) = mixer.try_send(mixer::Command::Reference(Some(prod))) {
		let (TrySendError::Full(rejected) | TrySendError::Disconnected(rejected)) = err;
		if let mixer::Command::Reference(Some(prod)) = rejected {
			reference.restore(prod);
		}
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
	///
	/// `generation` is the stream that raised it. A stream that has already been
	/// replaced can still have an error sitting in this queue, and acting on it
	/// would tear down the healthy stream that replaced it.
	Failed { generation: u64, error: cpal::Error },
	/// A sink was added or dropped. Drops whatever the mixer retired and retries
	/// anything its command queue was too full to take.
	Sync,
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
		retired: None,
		generation: 0,
		retry: RETRY_MIN,
		retry_at: None,
		underruns: 0,
		unclassified: 0,
		window: Instant::now(),
	};

	let first = driver.start();
	let started = first.is_ok();
	if opened.send(first).is_err() || !started {
		// Either the caller gave up on `open`, or there is no device to play
		// out of. Nothing to drive either way.
		return;
	}

	loop {
		// A failed start leaves a deadline to wake on; otherwise just block.
		let command = match driver.retry_at {
			Some(at) => driver.commands_until(&commands, at),
			None => commands.recv().map_err(|_| Timeout::Disconnected),
		};

		match command {
			Ok(Command::Switch { device, reply }) => {
				driver.device = device;
				let _ = reply.send(driver.restart());
			}
			Ok(Command::Failed { generation, error }) => {
				if driver.should_restart(generation, &error) {
					let _ = driver.restart();
				}
			}
			Ok(Command::Sync) => driver.sync(),
			Ok(Command::Shutdown) => break,
			Err(Timeout::Elapsed) => {
				if driver.restart().is_ok() {
					tracing::info!("audio output recovered");
				}
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
	/// Sinks the mixer has finished with, dropped here so the audio thread never
	/// has to free one.
	retired: Option<Receiver<mixer::Retired>>,
	/// Bumped on every stream, so an error from a retired one can be told apart
	/// from one the live stream raised.
	generation: u64,
	retry: Duration,
	/// When a failed start may be retried, and what the command wait times out
	/// against. `None` while the stream is healthy.
	retry_at: Option<Instant>,
	underruns: u32,
	/// Errors whose kind we have no rule for, counted over the same window.
	unclassified: u32,
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
		let (tx, rx) = sync_channel(COMMAND_QUEUE);
		// One slot per command the mixer can drain in a single pass, since each
		// retires at most one thing. Sized off the command queue rather than the
		// sink count: a pass can retire every sink *and* the echo reference, and
		// a full retirement channel is the one case where the mixer has to free
		// on the audio thread after all.
		let (retired_tx, retired_rx) = sync_channel(COMMAND_QUEUE);
		let mixer = Mixer::new(rx, retired_tx, rate, channels);

		self.generation += 1;
		let stream = self.build(&device, config, format, mixer)?;
		stream
			.play()
			.map_err(|err| Error::Playback(format!("cannot start output stream: {err}")))?;

		self.shared.rebind(rate, tx);
		self.stream = Some(stream);
		// Replaces the previous receiver, dropping anything the old stream
		// retired and never got drained.
		self.retired = Some(retired_rx);
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
		let generation = self.generation;

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
				move |error| {
					// This is cpal's error callback, not the audio callback, so
					// an allocating send is fine here.
					let _ = failures.send(Command::Failed { generation, error });
				},
				None,
			)
			.map_err(|err| Error::Playback(format!("cannot open output stream: {err}")))
	}

	/// Rebuild the stream on the current device, scheduling a retry if it will
	/// not open.
	///
	/// The single path for every reason a stream gets replaced (a switch, a
	/// fault, a scheduled retry), so the backoff and the sink hand-off can't
	/// drift between them.
	fn restart(&mut self) -> Result<(), Error> {
		// Drop the old stream first: some hosts refuse to open a second while
		// one is live, and every caller here is leaving it behind anyway.
		self.stop();

		let result = self.start();
		self.retry_at = match &result {
			Ok(()) => None,
			Err(err) => {
				tracing::debug!(%err, "audio output unavailable");
				Some(self.schedule())
			}
		};

		result
	}

	/// Tear the stream down and detach every sink from it.
	fn stop(&mut self) {
		self.shared.unbind();
		self.stream = None;
		// Dropping the receiver drops whatever the mixer retired, here rather
		// than on the audio thread.
		self.retired = None;
	}

	/// Catch the mixer up after a sink was added or dropped.
	fn sync(&mut self) {
		if let Some(retired) = &self.retired {
			// Each of these frees a ring buffer, which is exactly why the mixer
			// handed it over instead of dropping it itself.
			while retired.try_recv().is_ok() {}
		}

		for _ in 0..SYNC_ATTEMPTS {
			if self.shared.sync() {
				return;
			}
			// The mixer's queue is full, which takes a burst far larger than a
			// device period. Give it a period to drain and try again.
			std::thread::sleep(SYNC_DELAY);
		}

		tracing::warn!("audio output is not keeping up with sink changes");
	}

	/// When the next restart may be attempted, doubling the backoff.
	fn schedule(&mut self) -> Instant {
		let at = Instant::now() + self.retry;
		self.retry = (self.retry * 2).min(RETRY_MAX);
		at
	}

	/// Whether a failure reported by stream `generation` should rebuild the
	/// stream.
	///
	/// Errors outlive the stream that raised them: cpal's error callback posts
	/// into the same queue the driver reads, so a switch or a restart can leave
	/// one behind. Acting on it would tear down the healthy stream that replaced
	/// it, which is a dropout caused entirely by our own bookkeeping.
	fn should_restart(&mut self, generation: u64, error: &cpal::Error) -> bool {
		if generation != self.generation {
			tracing::debug!(%error, generation, "ignoring an error from a replaced audio output");
			return false;
		}

		self.fatal(error)
	}

	/// Whether this error means the stream has to be rebuilt.
	fn fatal(&mut self, err: &cpal::Error) -> bool {
		self.roll_window();

		match err.kind() {
			cpal::ErrorKind::DeviceNotAvailable | cpal::ErrorKind::StreamInvalidated => {
				tracing::warn!(%err, "audio output lost");
				true
			}
			// cpal documents both as survivable: the stream keeps running and
			// needs no rebuild.
			cpal::ErrorKind::DeviceChanged | cpal::ErrorKind::RealtimeDenied => {
				tracing::debug!(%err, "audio output changed underneath us");
				false
			}
			// One underrun is a glitch, not a broken device. Only a sustained
			// run of them is worth interrupting playback to fix.
			cpal::ErrorKind::Xrun => {
				self.underruns += 1;
				let restart = self.underruns > UNDERRUN_LIMIT;
				if restart {
					self.underruns = 0;
					tracing::warn!("restarting audio output after repeated underruns");
				}
				restart
			}
			_ => {
				tracing::warn!(%err, "audio output error");
				self.unclassified += 1;
				let restart = self.unclassified >= ERROR_LIMIT;
				if restart {
					self.unclassified = 0;
					tracing::warn!("restarting audio output after repeated unclassified errors");
				}
				restart
			}
		}
	}

	/// Start a fresh counting window once the old one has run out.
	fn roll_window(&mut self) {
		if self.window.elapsed() > ERROR_WINDOW {
			self.underruns = 0;
			self.unclassified = 0;
			self.window = Instant::now();
		}
	}
}

#[cfg(test)]
mod tests {
	use std::sync::mpsc::channel;

	use super::*;
	use crate::playback::sink::{self, Input};

	/// Everything a `Shared` needs without a device: a mixer command queue of
	/// `depth` (so a test decides when the mixer drains) and the driver's own
	/// queue (so a test can see what would have woken it).
	struct Wired {
		shared: Arc<Shared>,
		handle: Arc<super::super::Handle>,
		mixer: Receiver<mixer::Command>,
		driver: Receiver<Command>,
	}

	fn wired(depth: usize) -> Wired {
		let shared = Arc::new(Shared::default());
		let (commands, driver) = channel();
		let handle = Arc::new(super::super::Handle { commands });

		let (tx, mixer) = sync_channel(depth);
		shared.rebind(48_000, tx);

		Wired {
			shared,
			handle,
			mixer,
			driver,
		}
	}

	fn add(shared: &Arc<Shared>, handle: &Arc<super::super::Handle>) -> Result<Sink, Error> {
		shared.add(|id, rate| sink::new(id, rate, Input::default(), shared.clone(), handle.clone()))
	}

	fn syncs(driver: &Receiver<Command>) -> usize {
		std::iter::from_fn(|| driver.try_recv().ok())
			.filter(|c| matches!(c, Command::Sync))
			.count()
	}

	/// A registration the mixer's queue was too full to take must not be
	/// forgotten: without the retry it stayed silent until the next device
	/// restart.
	#[test]
	fn registrations_survive_a_full_mixer_queue() {
		let depth = 4;
		let w = wired(depth);

		let sinks: Vec<_> = (0..depth + 1).map(|_| add(&w.shared, &w.handle).unwrap()).collect();
		assert_eq!(sinks.len(), depth + 1);

		// One more sink than the queue holds, so the last one could not attach.
		assert!(!w.shared.sync(), "expected a sink to be waiting on the queue");

		// The mixer drains, which is what the driver's retry waits for.
		while w.mixer.try_recv().is_ok() {}
		assert!(w.shared.sync(), "the waiting sink was never re-sent");

		let state = w.shared.state.lock().unwrap();
		assert!(state.sinks.iter().all(|s| s.attached()), "a sink is still unattached");
	}

	/// `sync` only helps if something calls it, so adding or dropping a sink has
	/// to wake the driver.
	#[test]
	fn adding_and_dropping_a_sink_wakes_the_driver() {
		let w = wired(8);

		let engine = super::super::Engine {
			shared: w.shared.clone(),
			handle: w.handle.clone(),
		};

		let sink = engine.sink(Input::default()).unwrap();
		assert_eq!(syncs(&w.driver), 1, "adding a sink did not wake the driver");

		drop(sink);
		assert_eq!(syncs(&w.driver), 1, "dropping a sink did not wake the driver");
	}

	/// Same for removals. Dropping one would leave the mixer reading a sink
	/// nobody owns for the life of the stream.
	#[test]
	fn removals_survive_a_full_mixer_queue() {
		let w = wired(1);

		let sink = add(&w.shared, &w.handle).unwrap();
		let id = w.shared.state.lock().unwrap().sinks[0].id;

		// The add filled the single queue slot, so the remove cannot get through.
		drop(sink);
		assert_eq!(w.shared.state.lock().unwrap().detaching, vec![id]);

		while w.mixer.try_recv().is_ok() {}
		assert!(w.shared.sync());
		assert!(
			w.shared.state.lock().unwrap().detaching.is_empty(),
			"the removal was lost"
		);
	}

	/// The mixer sizes its entry list once, so the cap has to be refused here
	/// rather than discovered on the audio thread.
	#[test]
	fn refuses_more_sinks_than_the_mixer_can_hold() {
		let w = wired(4 * mixer::MAX_SINKS);

		let sinks: Vec<_> = (0..mixer::MAX_SINKS)
			.map(|_| add(&w.shared, &w.handle).unwrap())
			.collect();
		assert!(matches!(add(&w.shared, &w.handle), Err(Error::Unsupported(_))));

		// Dropping one makes room again.
		drop(sinks.into_iter().next_back());
		add(&w.shared, &w.handle).expect("a slot freed by the dropped sink");
	}

	/// A stream that has already been replaced can still have an error queued.
	/// Acting on it tears down the healthy stream that replaced it.
	#[test]
	fn ignores_errors_from_a_replaced_stream() {
		let w = wired(8);
		let (failures, _requests) = channel();

		let mut driver = Driver {
			shared: w.shared,
			failures,
			device: None,
			stream: None,
			retired: None,
			generation: 7,
			retry: RETRY_MIN,
			retry_at: None,
			underruns: 0,
			unclassified: 0,
			window: Instant::now(),
		};

		let lost = cpal::Error::new(cpal::ErrorKind::DeviceNotAvailable);
		assert!(!driver.should_restart(6, &lost), "acted on a retired stream's error");
		assert!(driver.should_restart(7, &lost), "ignored the live stream's error");
	}
}
