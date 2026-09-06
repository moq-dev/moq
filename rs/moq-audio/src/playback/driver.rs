//! The thread that owns the cpal output stream.
//!
//! That thread owns everything slow or fallible about the device: opening it,
//! switching it, and rebuilding it after an error. Sinks never talk to it on
//! the hot path; they register themselves in [`Shared`] and hand their consumer
//! straight to the mixer.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};
#[cfg(feature = "aec")]
use std::sync::mpsc::TrySendError;
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::Thread;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, StreamTrait};
use rand::RngExt;

use super::mixer::{self, Mixer};
use super::sink::{Registration, Sink};
use crate::Error;

/// Backoff bounds for reopening a device that failed. The first retry is quick because the common
/// case is a device that came right back (a USB re-enumerate, a sample-rate change); the ceiling
/// keeps a permanently gone device from spinning.
///
/// No give-up budget: the engine outlives any one device, and the user plugging a headset back in
/// is exactly the external change a retry is waiting for.
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

/// Device switches waiting while the driver is inside a host operation.
///
/// Switches are the only work with a payload, so this is a hard bound on both
/// the mailbox's storage and the number of callers awaiting a reply. Everything
/// else the driver waits on is a flag that coalesces.
const DRIVER_QUEUE: usize = 16;

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
	/// Posts a driver sync so a retry actually happens. A plain sender, not
	/// a [`Handle`](super::Handle): a canceller must not keep the output device
	/// open, and shutdown is explicit state rather than a disconnect.
	///
	/// `None` in tests that drive [`Shared`] without a driver behind it.
	#[cfg(feature = "aec")]
	waker: Option<Commands>,
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

	/// Remember the channel that posts a driver sync, so a send the mixer's
	/// command queue was too full to take gets retried.
	#[cfg(feature = "aec")]
	pub(super) fn wake_with(&self, waker: Commands) {
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
			waker.sync();
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
///
/// Not a queue of messages. Only a switch carries a payload; everything else
/// coalesces into a flag, so the mailbox derives one of these from whatever is
/// pending.
enum Work {
	/// Move to another output device, or back to the system default with `None`.
	Switch {
		device: Option<String>,
		reply: tokio::sync::oneshot::Sender<Result<(), Error>>,
	},
	/// The live stream reported problems worth inspecting.
	Failed,
	/// A sink was added or dropped. Drops whatever the mixer retired and retries
	/// anything its command queue was too full to take.
	Sync,
	/// A failed start's backoff has run out.
	Retry,
	/// The last [`Engine`](super::Engine) and [`Sink`](super::Sink) are gone.
	Shutdown,
}

/// A caller waiting to be moved to another output device.
struct Switch {
	/// The device to open, or the system default with `None`.
	device: Option<String>,
	/// Dropped rather than answered if the driver stops first, which is how the
	/// caller learns the thread is gone.
	reply: tokio::sync::oneshot::Sender<Result<(), Error>>,
}

/// The driver's mailbox, split by who is allowed to write which half.
///
/// cpal's error callback is not a normal thread. On CoreAudio it *is* the
/// render callback, and JACK, PipeWire, WASAPI and AAudio all raise it from
/// their process threads; cpal hands those paths `try_emit_error` precisely so
/// they never block. So everything reachable from a failure report is an atomic
/// plus [`Thread::unpark`], which allocates nothing and cannot wait on another
/// thread. Only a switch takes a lock, and no callback ever raises one.
///
/// `unpark` is also what makes a wake impossible to lose. It leaves a permit
/// behind, so a signal raised between the driver's last check and its `park`
/// makes that `park` return at once rather than sleeping through it.
#[derive(Default)]
struct Mailbox {
	/// Callers waiting on a device switch.
	switches: Mutex<Switches>,
	signals: Signals,
	/// The driver thread, so a sender can wake it. Registered before the driver
	/// can park, so a signal raised before it lands is still caught by the
	/// first check rather than slept through.
	driver: OnceLock<Thread>,
}

#[derive(Default)]
struct Switches {
	/// Capped at [`DRIVER_QUEUE`].
	waiting: VecDeque<Switch>,
	/// Latched under this lock together with the drain in
	/// [`Commands::shutdown`], so a switch cannot slip into a queue the driver
	/// has already stopped serving.
	closed: bool,
}

#[derive(Default)]
struct Signals {
	/// Written from cpal's error callback, which is why this is an atomic.
	failed: AtomicBool,
	sync: AtomicBool,
	shutdown: AtomicBool,
}

/// The sending half of the driver's mailbox.
#[derive(Clone, Default)]
pub(super) struct Commands {
	mailbox: Arc<Mailbox>,
}

impl Commands {
	/// Queue a device switch, or refuse it if the driver is already carrying its
	/// fixed maximum.
	///
	/// Refusing before the caller awaits is the point: a queue deep enough to
	/// never say no is a queue with no memory bound.
	pub(super) fn switch(
		&self,
		device: Option<String>,
		reply: tokio::sync::oneshot::Sender<Result<(), Error>>,
	) -> Result<(), Error> {
		let mut switches = self.mailbox.switches.lock().unwrap();

		if switches.closed {
			return Err(Error::Playback("the playback thread stopped".into()));
		} else if switches.waiting.len() >= DRIVER_QUEUE {
			return Err(Error::Playback("the playback thread is busy".into()));
		}

		switches.waiting.push_back(Switch { device, reply });
		drop(switches);

		self.mailbox.wake();
		Ok(())
	}

	/// Ask the driver to collect retired sinks and retry anything the mixer's
	/// command queue was too full to take.
	pub(super) fn sync(&self) {
		self.mailbox.signals.sync.store(true, Ordering::Release);
		self.mailbox.wake();
	}

	/// Stop the driver. A flag rather than a queued message, so saturation
	/// cannot delay or drop it.
	pub(super) fn shutdown(&self) {
		self.mailbox.signals.shutdown.store(true, Ordering::Release);

		// Answer everyone still waiting. The driver stops serving these, so
		// dropping the replies here is what tells those callers the thread is
		// gone instead of leaving them parked forever.
		let mut switches = self.mailbox.switches.lock().unwrap();
		switches.closed = true;
		switches.waiting.clear();
		drop(switches);

		self.mailbox.wake();
	}

	/// Tell the driver its live stream has failures waiting.
	///
	/// Reached from cpal's error callback, so this must stay lock-free.
	fn failed(&self) {
		self.mailbox.signals.failed.store(true, Ordering::Release);
		self.mailbox.wake();
	}
}

impl Mailbox {
	/// Wake the driver, or leave a permit if it has not parked yet.
	///
	/// Lock-free and allocation-free: safe to call from an audio callback.
	fn wake(&self) {
		if let Some(driver) = self.driver.get() {
			driver.unpark();
		}
	}
}

/// The receiving half of the driver's mailbox. Only the driver thread holds one.
pub(super) struct Requests {
	mailbox: Arc<Mailbox>,
}

impl Requests {
	/// Let senders wake this thread.
	///
	/// Called before the driver can park. A signal raised before this lands
	/// wakes nobody, which is harmless only because the wait below always
	/// re-checks every signal before parking.
	fn attach(&self) {
		let _ = self.mailbox.driver.set(std::thread::current());
	}

	/// Whatever is pending right now, or `None` if the driver would have to wait.
	///
	/// The order here is the driver's whole priority policy, in one place so a
	/// probe and a blocking wait cannot disagree about it.
	fn poll(&self, deadline: Option<Instant>) -> Option<Work> {
		let signals = &self.mailbox.signals;

		// Shutdown wins over everything: once the last handle is gone there is
		// no point opening a device. Nothing is stranded by that, since a
		// shutdown answers the switches it overtakes and refuses any that follow.
		if signals.shutdown.load(Ordering::Acquire) {
			return Some(Work::Shutdown);
		}

		// Ahead of the retry: a switch reopens the device itself, which is what
		// the retry was waiting to do anyway. Switches are capped and
		// caller-driven, so they cannot crowd it out.
		if let Some(Switch { device, reply }) = self.mailbox.switches.lock().unwrap().waiting.pop_front() {
			return Some(Work::Switch { device, reply });
		}

		// A due retry outranks the signals below, which re-arm themselves.
		// Serving those first would let steady sink churn keep a dead device
		// from ever being reopened.
		if deadline.is_some_and(|at| Instant::now() >= at) {
			return Some(Work::Retry);
		}

		if signals.failed.swap(false, Ordering::AcqRel) {
			return Some(Work::Failed);
		}
		if signals.sync.swap(false, Ordering::AcqRel) {
			return Some(Work::Sync);
		}

		None
	}

	/// Block until there is something to do, or until `deadline` passes.
	fn wait(&self, deadline: Option<Instant>) -> Work {
		loop {
			if let Some(work) = self.poll(deadline) {
				return work;
			}

			// Nothing pending. A sender that raced the poll above either
			// unparked us already, leaving a permit that returns from the park
			// below at once, or has yet to signal and will unpark us after it
			// does. Either way the loop polls again before sleeping, so an
			// early wake costs a lap rather than a spurious retry.
			match deadline {
				Some(at) => std::thread::park_timeout(at.saturating_duration_since(Instant::now())),
				None => std::thread::park(),
			}
		}
	}
}

/// Closing the mailbox behind a driver that is gone, so a caller gets the
/// stopped error instead of awaiting a reply nobody is left to send.
///
/// Covers an unexpected exit as much as an orderly one: only [`Commands::shutdown`]
/// closes the queue on the way out, and a driver that unwound never called it.
impl Drop for Requests {
	fn drop(&mut self) {
		// The driver may have unwound while holding this, and a panic inside a
		// drop would abort.
		let mut switches = self.mailbox.switches.lock().unwrap_or_else(|err| err.into_inner());
		switches.closed = true;
		switches.waiting.clear();
	}
}

/// Build the driver's mailbox.
pub(super) fn channel() -> (Commands, Requests) {
	let mailbox = Arc::new(Mailbox::default());
	(
		Commands {
			mailbox: mailbox.clone(),
		},
		Requests { mailbox },
	)
}

/// Unclassified error kinds worth naming in a log, indexed by the code
/// [`Failures::last`] holds. Zero means nothing recorded.
///
/// A code rather than the `cpal::Error` itself: the error owns a message, so
/// keeping one would mean freeing the previous one on the audio thread.
const UNCLASSIFIED: [cpal::ErrorKind; 9] = [
	cpal::ErrorKind::DeviceBusy,
	cpal::ErrorKind::HostUnavailable,
	cpal::ErrorKind::InvalidInput,
	cpal::ErrorKind::PermissionDenied,
	cpal::ErrorKind::ResourceExhausted,
	cpal::ErrorKind::UnsupportedConfig,
	cpal::ErrorKind::UnsupportedOperation,
	cpal::ErrorKind::BackendError,
	cpal::ErrorKind::Other,
];

fn code(kind: cpal::ErrorKind) -> u8 {
	// `ErrorKind` is `#[non_exhaustive]`, so a kind added upstream simply has no
	// code and logs as the count alone.
	UNCLASSIFIED.iter().position(|k| *k == kind).map_or(0, |i| i as u8 + 1)
}

fn named(code: u8) -> Option<cpal::ErrorKind> {
	UNCLASSIFIED.get(usize::from(code.checked_sub(1)?)).copied()
}

/// Failures the live stream has reported since the driver last looked.
///
/// Every field is an atomic because cpal's error callback writes them from the
/// audio thread. Fixed size too: the counters saturate at the limits that act
/// on them and only the latest unclassified kind is kept, so a storm costs no
/// more storage than one failure. Replacing a stream replaces this state, so a
/// retired callback can only write somewhere the driver never reads again.
#[derive(Default)]
struct Failures {
	unavailable: AtomicBool,
	invalidated: AtomicBool,
	changed: AtomicBool,
	realtime_denied: AtomicBool,
	xruns: AtomicU32,
	unclassified: AtomicU32,
	last: AtomicU8,
}

impl Failures {
	fn record(&self, kind: cpal::ErrorKind) {
		match kind {
			cpal::ErrorKind::DeviceNotAvailable => self.unavailable.store(true, Ordering::Release),
			cpal::ErrorKind::StreamInvalidated => self.invalidated.store(true, Ordering::Release),
			cpal::ErrorKind::DeviceChanged => self.changed.store(true, Ordering::Release),
			cpal::ErrorKind::RealtimeDenied => self.realtime_denied.store(true, Ordering::Release),
			cpal::ErrorKind::Xrun => {
				self.xruns.fetch_add(1, Ordering::AcqRel);
			}
			_ => {
				self.unclassified.fetch_add(1, Ordering::AcqRel);
				self.last.store(code(kind), Ordering::Release);
			}
		}
	}

	fn take(&self) -> FailureBatch {
		FailureBatch {
			unavailable: self.unavailable.swap(false, Ordering::AcqRel),
			invalidated: self.invalidated.swap(false, Ordering::AcqRel),
			changed: self.changed.swap(false, Ordering::AcqRel),
			realtime_denied: self.realtime_denied.swap(false, Ordering::AcqRel),
			// Clamped here rather than on the way in: `fetch_add` is one
			// instruction, where a saturating compare-exchange loop could spin
			// on the audio thread.
			xruns: self.xruns.swap(0, Ordering::AcqRel).min(UNDERRUN_LIMIT + 1),
			unclassified: self.unclassified.swap(0, Ordering::AcqRel).min(ERROR_LIMIT),
			last: named(self.last.swap(0, Ordering::AcqRel)),
		}
	}
}

struct FailureBatch {
	unavailable: bool,
	invalidated: bool,
	changed: bool,
	realtime_denied: bool,
	xruns: u32,
	unclassified: u32,
	last: Option<cpal::ErrorKind>,
}

/// Handed to one stream's error callback.
struct FailureReporter {
	failures: Arc<Failures>,
	commands: Commands,
}

impl FailureReporter {
	/// Record a failure and wake the driver.
	///
	/// cpal raises this from the audio thread on most backends, so nothing here
	/// may allocate, lock, or block.
	fn report(&self, error: &cpal::Error) {
		self.failures.record(error.kind());
		self.commands.failed();
	}
}

/// Run the output device until every [`Engine`](super::Engine) and
/// [`Sink`](super::Sink) has been dropped.
///
/// `opened` reports whether the first device came up, so
/// [`Engine::open`](super::Engine::open) can fail fast on a machine with no
/// output rather than handing back a handle that plays into nothing.
pub(super) fn run(
	requests: Requests,
	commands: Commands,
	shared: Arc<Shared>,
	device: Option<String>,
	opened: tokio::sync::oneshot::Sender<Result<(), Error>>,
) {
	let mut driver = Driver {
		shared,
		commands,
		device,
		stream: None,
		retired: None,
		failures: None,
		retry: RETRY_MIN,
		retry_at: None,
		underruns: 0,
		unclassified: 0,
		window: Instant::now(),
	};

	requests.attach();

	let first = driver.start();
	let started = first.is_ok();
	if opened.send(first).is_err() || !started {
		// Either the caller gave up on `open`, or there is no device to play
		// out of. Nothing to drive either way.
		return;
	}

	loop {
		// A failed start leaves a deadline to wake on; otherwise just block.
		match requests.wait(driver.retry_at) {
			Work::Switch { device, reply } => {
				driver.device = device;
				let _ = reply.send(driver.restart());
			}
			Work::Failed => {
				if driver.should_restart() {
					let _ = driver.restart();
				}
			}
			Work::Sync => driver.sync(),
			Work::Retry => {
				if driver.restart().is_ok() {
					tracing::info!("audio output recovered");
				}
			}
			Work::Shutdown => break,
		}
	}
}

struct Driver {
	shared: Arc<Shared>,
	/// Posts coalesced wakes for failures from the live stream.
	commands: Commands,
	device: Option<String>,
	/// The live stream. Dropping it stops the audio thread.
	stream: Option<cpal::Stream>,
	/// Sinks the mixer has finished with, dropped here so the audio thread never
	/// has to free one.
	retired: Option<Receiver<mixer::Retired>>,
	/// Failure state for the live stream only. Replaced with the stream, so a
	/// retired callback's writes are never read.
	failures: Option<Arc<Failures>>,
	/// Delay before reopening a device that would not start, doubling per failure.
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

		let failures = Arc::new(Failures::default());
		let reporter = FailureReporter {
			failures: failures.clone(),
			commands: self.commands.clone(),
		};
		let stream = self.build(&device, config, format, mixer, reporter)?;
		stream
			.play()
			.map_err(|err| Error::Playback(format!("cannot start output stream: {err}")))?;

		self.shared.rebind(rate, tx);
		self.stream = Some(stream);
		self.failures = Some(failures);
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
		failures: FailureReporter,
	) -> Result<cpal::Stream, Error> {
		match format {
			cpal::SampleFormat::F32 => self.build_as::<f32>(device, config, mixer, failures),
			cpal::SampleFormat::I16 => self.build_as::<i16>(device, config, mixer, failures),
			cpal::SampleFormat::U16 => self.build_as::<u16>(device, config, mixer, failures),
			cpal::SampleFormat::I32 => self.build_as::<i32>(device, config, mixer, failures),
			other => Err(Error::Unsupported(format!("output sample format {other:?}"))),
		}
	}

	fn build_as<T>(
		&self,
		device: &cpal::Device,
		config: cpal::StreamConfig,
		mut mixer: Mixer,
		failures: FailureReporter,
	) -> Result<cpal::Stream, Error>
	where
		T: cpal::SizedSample + cpal::FromSample<f32>,
	{
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
					failures.report(&error);
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
		self.failures = None;
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
	///
	/// Jittered so a host running many streams doesn't reopen the device in lockstep after a
	/// suspend or a driver reload.
	fn schedule(&mut self) -> Instant {
		let wait = self.retry.mul_f64(0.5 + rand::rng().random::<f64>() / 2.0);
		self.retry = (self.retry * 2).min(RETRY_MAX);
		Instant::now() + wait
	}

	/// Whether the failures the live stream has reported require a rebuild.
	fn should_restart(&mut self) -> bool {
		let Some(failures) = &self.failures else { return false };
		let failures = failures.take();
		self.roll_window();

		// Count everything before deciding anything. One batch can hold errors
		// that arrived before a fatal one, and those still belong to the
		// escalation window: dropping them lets a stream that keeps failing
		// after the rebuild sit dead longer than it otherwise would.
		self.underruns = self.underruns.saturating_add(failures.xruns);
		self.unclassified = self.unclassified.saturating_add(failures.unclassified);

		// cpal documents both as survivable: the stream keeps running and needs
		// no rebuild.
		if failures.changed || failures.realtime_denied {
			tracing::debug!("audio output changed underneath us");
		}
		if let Some(kind) = failures.last {
			tracing::warn!(%kind, count = failures.unclassified, "audio output error");
		}

		if failures.unavailable || failures.invalidated {
			tracing::warn!("audio output lost");
			return true;
		}

		// One underrun is a glitch, not a broken device. Only a sustained run
		// of them is worth interrupting playback to fix.
		if self.underruns > UNDERRUN_LIMIT {
			self.underruns = 0;
			tracing::warn!("restarting audio output after repeated underruns");
			return true;
		}

		if self.unclassified >= ERROR_LIMIT {
			self.unclassified = 0;
			tracing::warn!("restarting audio output after repeated unclassified errors");
			return true;
		}

		false
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
	use super::*;
	use crate::playback::sink::{self, Input};

	/// Everything a `Shared` needs without a device: a mixer command queue of
	/// `depth` (so a test decides when the mixer drains) and the driver's own
	/// queue (so a test can see what would have woken it).
	struct Wired {
		shared: Arc<Shared>,
		handle: Arc<super::super::Handle>,
		mixer: Receiver<mixer::Command>,
		driver: Requests,
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

	/// Long enough that only a lost wake, rather than a loaded machine, trips a
	/// test that waits this out.
	const PATIENCE: Duration = Duration::from_secs(5);

	fn queue_switch(commands: &Commands) -> tokio::sync::oneshot::Receiver<Result<(), Error>> {
		let (reply, response) = tokio::sync::oneshot::channel();
		commands.switch(None, reply).unwrap();
		response
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
		assert!(
			matches!(w.driver.poll(None), Some(Work::Sync)),
			"adding a sink did not wake the driver"
		);

		drop(sink);
		assert!(
			matches!(w.driver.poll(None), Some(Work::Sync)),
			"dropping a sink did not wake the driver"
		);
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

	/// Every notification class has fixed storage however hard it is hit, and a
	/// flood collapses into one wake per class.
	#[test]
	fn notification_floods_are_coalesced() {
		let (commands, requests) = channel();
		let failures = Arc::new(Failures::default());
		let reporter = FailureReporter {
			failures: failures.clone(),
			commands: commands.clone(),
		};

		for _ in 0..1_000 {
			commands.sync();
			for kind in [
				cpal::ErrorKind::DeviceNotAvailable,
				cpal::ErrorKind::StreamInvalidated,
				cpal::ErrorKind::DeviceChanged,
				cpal::ErrorKind::RealtimeDenied,
				cpal::ErrorKind::Xrun,
				cpal::ErrorKind::BackendError,
			] {
				reporter.report(&cpal::Error::new(kind));
			}
		}

		assert!(requests.mailbox.switches.lock().unwrap().waiting.is_empty());
		assert!(matches!(requests.poll(None), Some(Work::Failed)));
		assert!(matches!(requests.poll(None), Some(Work::Sync)));
		assert!(requests.poll(None).is_none(), "a flood outlived its coalesced wakes");

		let failures = failures.take();
		assert!(failures.unavailable);
		assert!(failures.invalidated);
		assert!(failures.changed);
		assert!(failures.realtime_denied);
		assert_eq!(failures.xruns, UNDERRUN_LIMIT + 1);
		assert_eq!(failures.unclassified, ERROR_LIMIT);
		assert_eq!(failures.last, Some(cpal::ErrorKind::BackendError));
	}

	/// cpal raises the error callback from the audio thread on most backends, so
	/// a report must never wait on a lock another thread is holding. Reporting
	/// while the switch queue is locked would deadlock the audio thread if the
	/// failure path touched it.
	#[test]
	fn reporting_a_failure_never_waits_on_the_mailbox() {
		let (commands, requests) = channel();
		let failures = Arc::new(Failures::default());
		let reporter = FailureReporter {
			failures: failures.clone(),
			commands: commands.clone(),
		};

		// Held for the whole report. If the failure path took this lock, the
		// audio thread would be stuck here until another thread let go.
		let held = requests.mailbox.switches.lock().unwrap();
		reporter.report(&cpal::Error::new(cpal::ErrorKind::DeviceNotAvailable));
		commands.sync();
		drop(held);

		assert!(failures.take().unavailable, "the failure never landed");
		assert!(requests.mailbox.signals.sync.load(Ordering::Acquire));
	}

	/// A switch either takes one of the fixed slots or reports overload before
	/// its caller starts awaiting a response.
	#[test]
	fn switches_complete_or_reject_overload() {
		let (commands, requests) = channel();
		let responses: Vec<_> = (0..DRIVER_QUEUE).map(|_| queue_switch(&commands)).collect();

		let (reply, _response) = tokio::sync::oneshot::channel();
		let error = commands.switch(None, reply).unwrap_err();
		assert!(matches!(error, Error::Playback(message) if message.contains("busy")));

		for response in responses {
			let Some(Work::Switch { reply, .. }) = requests.poll(None) else {
				panic!("a switch went missing");
			};
			reply.send(Ok(())).unwrap();
			assert!(matches!(response.blocking_recv(), Ok(Ok(()))));
		}
	}

	/// A sync raised while every switch slot is taken still reaches the driver.
	/// It has no slot to compete for, so saturation cannot drop it.
	#[test]
	fn sync_survives_a_saturated_driver() {
		let (commands, requests) = channel();
		let _responses: Vec<_> = (0..DRIVER_QUEUE).map(|_| queue_switch(&commands)).collect();

		commands.sync();

		for _ in 0..DRIVER_QUEUE {
			assert!(matches!(requests.poll(None), Some(Work::Switch { .. })));
		}
		assert!(
			matches!(requests.poll(None), Some(Work::Sync)),
			"saturation lost a sync"
		);
	}

	/// The last handle must stop a saturated driver rather than leaking the
	/// thread and the device with it.
	#[test]
	fn final_handle_shuts_down_a_saturated_driver() {
		let (commands, requests) = channel();
		let handle = Arc::new(super::super::Handle {
			commands: commands.clone(),
		});
		let responses: Vec<_> = (0..DRIVER_QUEUE).map(|_| queue_switch(&commands)).collect();

		drop(handle);
		assert!(
			matches!(requests.poll(None), Some(Work::Shutdown)),
			"saturation lost shutdown"
		);

		// The driver returns rather than serving the backlog, so every waiting
		// caller learns the thread stopped instead of hanging on its reply.
		for response in responses {
			assert!(response.blocking_recv().is_err());
		}
		let (reply, _response) = tokio::sync::oneshot::channel();
		let error = commands.switch(None, reply).unwrap_err();
		assert!(matches!(error, Error::Playback(message) if message.contains("stopped")));
	}

	/// A driver parked with nothing pending has to be woken by the next
	/// notification, whatever else raced with it. Losing this wake stranded a
	/// sink registration, or the shutdown that closes the device.
	#[test]
	fn a_parked_driver_is_woken() {
		let (commands, requests) = channel();
		let (sent, arrived) = std::sync::mpsc::channel();

		let waker = commands.clone();
		let driver = std::thread::spawn(move || {
			requests.attach();

			// Deadlines rather than a plain park, so a lost wake fails the test
			// instead of hanging it.
			let woken = matches!(requests.wait(Some(Instant::now() + PATIENCE)), Work::Sync);
			sent.send(()).unwrap();

			// Syncs racing in behind the first are fine; only silence is not.
			let deadline = Instant::now() + PATIENCE;
			let stopped = loop {
				match requests.wait(Some(deadline)) {
					Work::Shutdown => break true,
					Work::Retry => break false,
					_ => continue,
				}
			};
			(woken, stopped)
		});

		// Let the driver reach `park` with an empty mailbox, then race a flood
		// of notifications against it.
		std::thread::sleep(Duration::from_millis(10));
		std::thread::scope(|s| {
			for _ in 0..8 {
				s.spawn(|| waker.sync());
			}
		});

		arrived.recv_timeout(PATIENCE).expect("a sync never woke the driver");
		commands.shutdown();

		let (woken, stopped) = driver.join().unwrap();
		assert!(woken, "a sync never woke the driver");
		assert!(stopped, "a shutdown never woke the driver");
	}

	/// A signal raised before the driver registers itself wakes nobody, so the
	/// first check has to find it rather than parking through it.
	#[test]
	fn a_signal_racing_startup_is_not_slept_through() {
		let (commands, requests) = channel();
		commands.sync();

		let driver = std::thread::spawn(move || {
			requests.attach();
			requests.wait(Some(Instant::now() + PATIENCE))
		});

		assert!(
			matches!(driver.join().unwrap(), Work::Sync),
			"a signal raised before attach was slept through"
		);
	}

	/// A device that failed to open has to get its retry even while sinks churn.
	/// Sync and failure re-arm themselves, so serving them first would leave
	/// playback down for as long as the churn lasted.
	#[test]
	fn a_due_retry_outranks_reasserted_signals() {
		let (commands, requests) = channel();
		let due = Instant::now();

		for _ in 0..8 {
			commands.sync();
			assert!(
				matches!(requests.wait(Some(due)), Work::Retry),
				"steady sink churn starved the device retry"
			);
		}
	}

	/// An early wake costs a lap, not a retry: the deadline is what decides,
	/// not the fact that the park returned.
	#[test]
	fn an_early_wake_does_not_fake_a_retry() {
		let (commands, requests) = channel();

		let waker = commands.clone();
		let driver = std::thread::spawn(move || {
			requests.attach();
			// A deadline far enough out that only the unpark below can end the
			// park, so a Retry here would mean the deadline was misread.
			requests.wait(Some(Instant::now() + PATIENCE))
		});

		std::thread::sleep(Duration::from_millis(10));
		waker.sync();

		assert!(
			matches!(driver.join().unwrap(), Work::Sync),
			"an early wake was reported as a retry"
		);
	}

	/// A driver that unwinds takes the mailbox down with it. Without that, a
	/// switch is queued for a receiver that no longer exists and its caller
	/// awaits a reply forever.
	#[test]
	fn losing_the_driver_releases_switch_callers() {
		let (commands, requests) = channel();
		let queued = queue_switch(&commands);

		drop(requests);

		assert!(queued.blocking_recv().is_err(), "a queued switch outlived its driver");

		let (reply, _response) = tokio::sync::oneshot::channel();
		let error = commands.switch(None, reply).unwrap_err();
		assert!(matches!(error, Error::Playback(message) if message.contains("stopped")));
	}

	/// Errors that arrive before a fatal one in the same batch still count.
	/// Coalescing must not reset the escalation window that a stream failing
	/// again after the rebuild depends on.
	#[test]
	fn a_fatal_batch_keeps_the_counts_that_preceded_it() {
		let (commands, _requests) = channel();
		let failures = Arc::new(Failures::default());

		let mut driver = Driver {
			shared: Arc::new(Shared::default()),
			commands,
			device: None,
			stream: None,
			retired: None,
			failures: Some(failures.clone()),
			retry: RETRY_MIN,
			retry_at: None,
			underruns: 0,
			unclassified: 0,
			window: Instant::now(),
		};

		// Two survivable errors, then the fatal one, all in one batch.
		failures.record(cpal::ErrorKind::BackendError);
		failures.record(cpal::ErrorKind::BackendError);
		failures.record(cpal::ErrorKind::DeviceNotAvailable);
		assert!(driver.should_restart(), "a lost device did not restart the stream");

		// The replacement fails once more. That is the third unclassified error
		// in the window, so it has to escalate rather than start over.
		let replacement = Arc::new(Failures::default());
		driver.failures = Some(replacement.clone());
		replacement.record(cpal::ErrorKind::BackendError);
		assert!(
			driver.should_restart(),
			"the fatal restart threw away the errors before it"
		);
	}

	/// A stream that has already been replaced can still report an error. Acting
	/// on it tears down the healthy stream that replaced it.
	#[test]
	fn ignores_errors_from_a_replaced_stream() {
		let w = wired(8);
		let (commands, _requests) = channel();
		let stale = Arc::new(Failures::default());
		let current = Arc::new(Failures::default());
		let stale_reporter = FailureReporter {
			failures: stale,
			commands: commands.clone(),
		};

		let mut driver = Driver {
			shared: w.shared,
			commands: commands.clone(),
			device: None,
			stream: None,
			retired: None,
			failures: Some(current.clone()),
			retry: RETRY_MIN,
			retry_at: None,
			underruns: 0,
			unclassified: 0,
			window: Instant::now(),
		};

		let lost = cpal::Error::new(cpal::ErrorKind::DeviceNotAvailable);
		stale_reporter.report(&lost);
		assert!(!driver.should_restart(), "acted on a retired stream's error");

		current.record(lost.kind());
		assert!(driver.should_restart(), "ignored the live stream's error");
	}
}
