//! [`Observer`]: what the device played, counted on the audio thread.
//!
//! The output callback folds every buffer it plays into 128-frame quanta, the
//! Web Audio render quantum, so a native count reads the same as one taken off
//! a browser's output. Counters are plain atomics and closed gaps go out over a
//! bounded queue whose slots are allocated up front, so nothing here blocks,
//! allocates, or calls back into the caller on the audio thread. A reader that
//! falls so far behind that the queue fills loses events, and the observer
//! reports that as an error rather than an undercount.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, sync_channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::task::Poll;
use std::time::Duration;

use super::driver::Commands;
use crate::Error;

/// Frames per quantum, the Web Audio render quantum.
const QUANTUM: usize = 128;

/// A quantum whose RMS is under this is quiet. About -60 dBFS.
const QUIET_RMS: f64 = 0.001;

/// Events the queue holds between reads. At most one gap closes every two
/// quanta, so this is over five seconds of the worst case at 48 kHz.
const EVENTS: usize = 1024;

/// `latency` before any callback has reported one.
const UNKNOWN: u64 = u64::MAX;

/// A run of consecutive quanta the mix could not fill.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Gap {
	/// Where the first missing frame would have played, on [`Observation::clock`].
	pub at: Duration,
	/// The audio that was missing, at the device rate.
	pub missing: Duration,
	/// Quanta in the gap, partly and entirely empty alike.
	pub quanta: u64,
	/// Of those, the quanta that were partly filled.
	pub short: u64,
}

/// A change in whether anything was playing, on [`Observation::clock`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Stall {
	/// When the change was played.
	pub at: Duration,
	/// Whether nothing is playing from here on.
	pub stalled: bool,
}

/// What an [`Observer`] has seen: running totals, and the events since the last read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Observation {
	/// Device time rendered since the engine opened: frames pulled over the
	/// device rate. The clock every other timestamp here is on.
	pub clock: Duration,
	/// How far the last buffer was handed over ahead of the speaker, as the
	/// host reports it. `None` until a host reports one.
	pub latency: Option<Duration>,
	/// Quanta rendered since the first audible one.
	pub quanta: u64,
	/// Of those, the quanta under -60 dBFS, whatever the cause.
	pub quiet: u64,
	/// Whether nothing is playing: no sink is being written to, or every one
	/// still waits for its first samples.
	pub stalled: bool,
	/// Gaps that closed since the last read. One still open is reported once a
	/// full quantum ends it, or once [`Observer::finish`] resolves.
	pub gaps: Vec<Gap>,
	/// Changes in [`stalled`](Self::stalled) since the last read.
	pub stalls: Vec<Stall>,
}

enum Event {
	Gap(Gap),
	Stall(Stall),
}

/// State shared between the audio thread and the [`Observer`].
pub(super) struct Shared {
	/// [`Observation::clock`] in nanoseconds, carried across stream rebuilds.
	clock: AtomicU64,
	/// [`Observation::latency`] in nanoseconds, or [`UNKNOWN`].
	latency: AtomicU64,
	quanta: AtomicU64,
	quiet: AtomicU64,
	stalled: AtomicBool,
	/// The reading [`Observer`]'s generation, or 0 with none: events only queue
	/// while one reads. Each event carries it, so a send racing one observer's
	/// teardown never reaches the next.
	observer: AtomicU64,
	/// The last generation handed out.
	generation: AtomicU64,
	/// The generation whose event found the queue full, or 0.
	overflowed: AtomicU64,
	/// The generation that ended its measurement, or 0.
	finished: AtomicU64,
	/// The generation whose end a callback has closed out, or 0.
	acknowledged: AtomicU64,
	/// Ledgers still counting, one per live stream. With none, nothing is left
	/// to close out a measurement, so it is already final.
	ledgers: AtomicUsize,
	/// The [`Observer::finish`] waiting on [`acknowledged`](Self::acknowledged) or
	/// [`ledgers`](Self::ledgers). Woken off the audio thread, which only posts a
	/// driver sync. Its entry dies with the future, so a cancelled finish holds
	/// nothing here.
	waiters: Mutex<kio::WaiterList>,
	/// Posts that sync. Unset in tests that run a ledger without a driver.
	driver: OnceLock<Commands>,
	events: SyncSender<(u64, Event)>,
	received: Mutex<Receiver<(u64, Event)>>,
}

impl Default for Shared {
	fn default() -> Self {
		let (events, received) = sync_channel(EVENTS);
		Self {
			clock: AtomicU64::new(0),
			latency: AtomicU64::new(UNKNOWN),
			quanta: AtomicU64::new(0),
			quiet: AtomicU64::new(0),
			stalled: AtomicBool::new(true),
			observer: AtomicU64::new(0),
			generation: AtomicU64::new(0),
			overflowed: AtomicU64::new(0),
			finished: AtomicU64::new(0),
			acknowledged: AtomicU64::new(0),
			ledgers: AtomicUsize::new(0),
			waiters: Mutex::new(kio::WaiterList::new()),
			driver: OnceLock::new(),
			events,
			received: Mutex::new(received),
		}
	}
}

impl Shared {
	/// Route acknowledgements from the audio thread through `driver`.
	pub(super) fn wake_with(&self, driver: Commands) {
		let _ = self.driver.set(driver);
	}

	/// Wake a [`Observer::finish`] whose measurement a callback or the last stream
	/// going away closed out. Locks, so the driver thread calls it, never the
	/// audio thread.
	pub(super) fn wake(&self) {
		self.waiters.lock().unwrap().wake();
	}
}

/// Reads what an [`Engine`](super::Engine) played, quantum by quantum.
///
/// Built by [`Engine::observe`](super::Engine::observe). An engine has at most
/// one, so no two readers split its events.
pub struct Observer {
	shared: Arc<Shared>,
	generation: u64,
}

impl Observer {
	pub(super) fn new(shared: Arc<Shared>) -> Result<Self, Error> {
		let generation = shared.generation.fetch_add(1, Ordering::Relaxed) + 1;
		if shared
			.observer
			.compare_exchange(0, generation, Ordering::AcqRel, Ordering::Acquire)
			.is_err()
		{
			return Err(Error::Busy("playback engine already has an observer".into()));
		}
		Ok(Self { shared, generation })
	}

	/// The totals so far, and the gaps and stalls since the previous read.
	///
	/// Fails once the audio thread found the event queue full, since every
	/// count after that would be missing events. Read at least every few
	/// seconds to avoid it.
	pub fn read(&mut self) -> Result<Observation, Error> {
		let shared = &self.shared;
		let mut observation = Observation {
			clock: Duration::from_nanos(shared.clock.load(Ordering::Relaxed)),
			latency: match shared.latency.load(Ordering::Relaxed) {
				UNKNOWN => None,
				nanos => Some(Duration::from_nanos(nanos)),
			},
			quanta: shared.quanta.load(Ordering::Relaxed),
			quiet: shared.quiet.load(Ordering::Relaxed),
			stalled: shared.stalled.load(Ordering::Relaxed),
			..Default::default()
		};

		let received = shared.received.lock().unwrap();
		loop {
			match received.try_recv() {
				Ok((generation, _)) if generation != self.generation => {}
				Ok((_, Event::Gap(gap))) => observation.gaps.push(gap),
				Ok((_, Event::Stall(stall))) => observation.stalls.push(stall),
				Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
			}
		}

		// Checked after draining, so an overflow that raced the drain is not missed.
		if shared.overflowed.load(Ordering::Acquire) == self.generation {
			return Err(Error::Playback("observer fell behind and lost events".into()));
		}
		Ok(observation)
	}

	/// End the measurement now, resolving once the next callback, or the stream
	/// going away, has closed a gap still open and stopped counting. With no
	/// stream it resolves at once. The read after it resolves is final; only the
	/// clock moves on.
	///
	/// Dropping the future still ends the measurement; it only stops waiting.
	pub fn finish(&mut self) -> impl Future<Output = ()> + '_ {
		// SeqCst pairs with a ledger starting: either it sees this store and counts
		// nothing, or `poll_finished` sees it counting and waits for it.
		self.shared.finished.store(self.generation, Ordering::SeqCst);
		kio::wait(|waiter| self.poll_finished(waiter))
	}

	fn poll_finished(&self, waiter: &kio::Waiter) -> Poll<()> {
		if self.settled() {
			return Poll::Ready(());
		}
		waiter.register(&mut self.shared.waiters.lock().unwrap());
		// Checked again after registering, so an acknowledgement or a last stream
		// going away that raced it is not missed.
		match self.settled() {
			true => Poll::Ready(()),
			false => Poll::Pending,
		}
	}

	/// Whether a callback closed out this measurement, or no stream is left to.
	fn settled(&self) -> bool {
		let shared = &self.shared;
		shared.acknowledged.load(Ordering::Acquire) == self.generation || shared.ledgers.load(Ordering::SeqCst) == 0
	}
}

impl Drop for Observer {
	fn drop(&mut self) {
		// Drain before releasing the slot, so the next observer's events can only
		// arrive after this teardown has stopped draining. A send that races it
		// still carries this generation, which the next observer skips.
		let received = self.shared.received.lock().unwrap();
		while received.try_recv().is_ok() {}
		self.shared.observer.store(0, Ordering::Release);
	}
}

/// The gap being built, still open until a full quantum ends it.
struct Open {
	/// The observer generation it opened under: a gap belongs to the observer
	/// that saw it start, or to the first one if none did.
	observer: u64,
	/// The first missing frame, on the ledger's frame count.
	at: u64,
	missing: u64,
	quanta: u64,
	short: u64,
}

/// The audio thread's half: folds every buffer the mixer plays into quanta.
///
/// Owned by one mixer, so one stream: a rebuilt stream starts a new ledger on
/// the same [`Shared`], continuing its clock.
pub(super) struct Ledger {
	shared: Arc<Shared>,
	rate: f64,
	/// [`Shared::clock`] when this ledger started, in nanoseconds.
	base: u64,
	/// Frames played through this ledger.
	frames: u64,
	playing: bool,
	/// Whether an audible quantum has played yet. Nothing before one counts:
	/// that is the first fill, not a gap.
	started: bool,
	/// Frames into the current quantum.
	filled: usize,
	/// Of those, frames no sink supplied.
	missing: usize,
	/// The first of those, on the frame count.
	first: Option<u64>,
	/// Sum of squares over the current quantum, every channel.
	power: f64,
	open: Option<Open>,
}

impl Ledger {
	pub(super) fn new(shared: Arc<Shared>, rate: u32) -> Self {
		let base = shared.clock.load(Ordering::Relaxed);
		// A rebuilt stream continues the engine's counts: once anything has played,
		// a quiet quantum while the new stream recovers is silence, not the first fill.
		let started = shared.quanta.load(Ordering::Relaxed) > 0;
		shared.ledgers.fetch_add(1, Ordering::SeqCst);
		Self {
			shared,
			rate: rate as f64,
			base,
			frames: 0,
			playing: false,
			started,
			filled: 0,
			missing: 0,
			first: None,
			power: 0.0,
			open: None,
		}
	}

	/// The host's latency for the buffer being filled, if it reports one.
	pub(super) fn latency(&self, latency: Option<Duration>) {
		let nanos = latency.map_or(UNKNOWN, |l| l.as_nanos().min(UNKNOWN as u128 - 1) as u64);
		self.shared.latency.store(nanos, Ordering::Relaxed);
	}

	/// Fold one mixed buffer in. `supplied` is how many of its leading frames
	/// the sinks filled, the rest being zeros they could not; `playing` is
	/// whether any sink was readable at all.
	pub(super) fn add(&mut self, mix: &[f32], channels: usize, supplied: usize, playing: bool) {
		let frames = mix.len() / channels;
		if let Some(observer) = self.finished() {
			// The measurement is over: report what was open, count nothing after it.
			self.acknowledge(observer);
			// Lock-free: the driver thread wakes the waiting observer.
			if let Some(driver) = self.shared.driver.get() {
				driver.sync();
			}
			(self.filled, self.missing, self.first, self.power) = (0, 0, None, 0.0);
			self.frames += frames as u64;
			self.shared.clock.store(self.nanos(self.frames), Ordering::Relaxed);
			return;
		}

		if playing != self.playing {
			self.playing = playing;
			self.shared.stalled.store(!playing, Ordering::Relaxed);
			let at = self.clock(self.frames);
			self.send(Event::Stall(Stall { at, stalled: !playing }));
		}

		let supplied = if playing { supplied } else { 0 };
		let mut done = 0;
		while done < frames {
			let take = (QUANTUM - self.filled).min(frames - done);
			let full = supplied.saturating_sub(done).min(take);
			// The first audio landing mid-quantum is the first fill, not a gap:
			// forget the missing frames ahead of it when nothing else preceded them.
			if !self.started && full > 0 && self.missing == self.filled {
				(self.missing, self.first) = (0, None);
			}
			if full < take {
				self.first.get_or_insert(self.frames + (done + full) as u64);
				self.missing += take - full;
			}
			self.power += mix[done * channels..(done + take) * channels]
				.iter()
				.map(|&s| s as f64 * s as f64)
				.sum::<f64>();
			self.filled += take;
			done += take;

			if self.filled == QUANTUM {
				self.quantum(channels);
			}
		}

		self.frames += frames as u64;
		self.shared.clock.store(self.nanos(self.frames), Ordering::Relaxed);
	}

	/// Classify the quantum just completed.
	fn quantum(&mut self, channels: usize) {
		let missing = std::mem::take(&mut self.missing);
		let first = self.first.take();
		let power = std::mem::take(&mut self.power);
		self.filled = 0;

		let silent = missing == QUANTUM;
		if !self.started {
			if silent {
				return;
			}
			self.started = true;
		}

		self.shared.quanta.fetch_add(1, Ordering::Relaxed);
		if (power / (QUANTUM * channels) as f64).sqrt() < QUIET_RMS {
			self.shared.quiet.fetch_add(1, Ordering::Relaxed);
		}

		let Some(first) = first else {
			self.close();
			return;
		};
		// A partly filled quantum inside an open gap is a sink that got a little
		// and ran out again: still one gap, since only a full quantum ends it.
		let observer = self.shared.observer.load(Ordering::Acquire);
		let open = self.open.get_or_insert(Open {
			observer,
			at: first,
			missing: 0,
			quanta: 0,
			short: 0,
		});
		open.missing += missing as u64;
		open.quanta += 1;
		if !silent {
			open.short += 1;
		}
	}

	fn close(&mut self) {
		let Some(open) = self.open.take() else { return };
		if open.observer != 0 && open.observer != self.shared.observer.load(Ordering::Acquire) {
			return;
		}
		let gap = Gap {
			at: self.clock(open.at),
			missing: Duration::from_secs_f64(open.missing as f64 / self.rate),
			quanta: open.quanta,
			short: open.short,
		};
		self.send(Event::Gap(gap));
	}

	/// Close a gap still open and tell `observer`, the generation that asked,
	/// its measurement is final. The gap is queued before the acknowledgement,
	/// so a read after it sees it, and a replacement observer is never the one
	/// acknowledged.
	fn acknowledge(&mut self, observer: u64) {
		self.close();
		self.shared.acknowledged.store(observer, Ordering::Release);
	}

	/// The reading observer's generation, if it has ended its measurement.
	fn finished(&self) -> Option<u64> {
		let observer = self.shared.observer.load(Ordering::Acquire);
		(observer != 0 && self.shared.finished.load(Ordering::SeqCst) == observer).then_some(observer)
	}

	fn nanos(&self, frames: u64) -> u64 {
		self.base + (frames as f64 * 1e9 / self.rate).round() as u64
	}

	fn clock(&self, frames: u64) -> Duration {
		Duration::from_nanos(self.nanos(frames))
	}

	fn send(&self, event: Event) {
		let observer = self.shared.observer.load(Ordering::Acquire);
		if observer == 0 {
			return;
		}
		if self.shared.events.try_send((observer, event)).is_err() {
			self.shared.overflowed.store(observer, Ordering::Release);
		}
	}
}

impl Drop for Ledger {
	/// A stream going away mid-gap still reports it, and nothing plays until the
	/// next stream does. Runs on the driver thread, which drops the stream.
	fn drop(&mut self) {
		if let Some(observer) = self.finished() {
			self.acknowledge(observer);
		} else {
			self.close();
			if self.playing {
				self.shared.stalled.store(true, Ordering::Relaxed);
				let at = self.clock(self.frames);
				self.send(Event::Stall(Stall { at, stalled: true }));
			}
		}
		// Woken even without a finish seen above: one that raced this drop waits
		// on the last ledger going away.
		self.shared.ledgers.fetch_sub(1, Ordering::SeqCst);
		self.shared.wake();
	}
}

#[cfg(test)]
mod tests {
	use std::task::Waker;

	use super::*;

	const RATE: u32 = 48_000;

	fn observed() -> (Ledger, Observer) {
		let shared = Arc::new(Shared::default());
		let observer = Observer::new(shared.clone()).unwrap();
		(Ledger::new(shared, RATE), observer)
	}

	/// `frames` stereo frames, the leading `supplied` of them a loud tone.
	fn buffer(frames: usize, supplied: usize) -> Vec<f32> {
		let mut mix = vec![0.0; frames * 2];
		mix[..supplied * 2].fill(0.5);
		mix
	}

	fn add(ledger: &mut Ledger, frames: usize, supplied: usize) {
		ledger.add(&buffer(frames, supplied), 2, supplied, true);
	}

	fn ms(frames: u64) -> Duration {
		Duration::from_secs_f64(frames as f64 / RATE as f64)
	}

	#[test]
	fn nothing_counts_before_the_first_audible_quantum() {
		let (mut ledger, mut observer) = observed();
		add(&mut ledger, 1024, 0);
		let observation = observer.read().unwrap();
		assert_eq!(observation.quanta, 0);
		assert!(observation.gaps.is_empty());
		assert_eq!(observation.clock, ms(1024));
	}

	#[test]
	fn a_full_quantum_closes_a_gap_of_short_and_silent_quanta() {
		let (mut ledger, mut observer) = observed();
		add(&mut ledger, 256, 256);
		// Runs dry 32 frames into the third quantum, and stays dry through the fourth.
		add(&mut ledger, 256, 32);
		assert!(observer.read().unwrap().gaps.is_empty(), "the gap is still open");
		add(&mut ledger, 128, 128);

		let observation = observer.read().unwrap();
		assert_eq!(
			observation.gaps,
			vec![Gap {
				at: ms(256 + 32),
				missing: ms(96 + 128),
				quanta: 2,
				short: 1,
			}]
		);
		assert_eq!(observation.quanta, 5);
		assert_eq!(observation.quiet, 1, "only the empty quantum is quiet");
	}

	#[test]
	fn the_first_fill_landing_mid_quantum_is_not_a_gap() {
		let (mut ledger, mut observer) = observed();
		// Nothing readable for 200 frames, then audio from frame 200 on, mid-quantum.
		ledger.add(&buffer(200, 0), 2, 0, false);
		add(&mut ledger, 312, 312);
		let observation = observer.read().unwrap();
		assert!(observation.gaps.is_empty(), "{observation:?}");
		assert_eq!(observation.quanta, 3, "the quantum it landed in counts from there");
	}

	#[test]
	fn quanta_span_buffers_of_any_size() {
		let (mut ledger, mut observer) = observed();
		// 480-frame buffers, a common device period, never align to the quantum.
		for _ in 0..4 {
			add(&mut ledger, 480, 480);
		}
		add(&mut ledger, 480, 100);
		add(&mut ledger, 480, 480);

		let observation = observer.read().unwrap();
		assert_eq!(observation.quanta, 2880 / 128);
		let [gap] = observation.gaps[..] else {
			panic!("one gap, got {:?}", observation.gaps)
		};
		assert_eq!(gap.at, ms(1920 + 100));
		assert_eq!(gap.missing, ms(380));
	}

	#[test]
	fn stalls_are_stamped_and_play_as_empty_quanta() {
		let (mut ledger, mut observer) = observed();
		add(&mut ledger, 128, 128);
		ledger.add(&buffer(256, 256), 2, 256, false);
		add(&mut ledger, 128, 128);

		let observation = observer.read().unwrap();
		assert_eq!(
			observation.stalls,
			vec![
				Stall {
					at: Duration::ZERO,
					stalled: false
				},
				Stall {
					at: ms(128),
					stalled: true
				},
				Stall {
					at: ms(384),
					stalled: false
				},
			]
		);
		assert_eq!(observation.gaps.len(), 1);
		assert_eq!(observation.gaps[0].quanta, 2);
		assert!(!observation.stalled);
	}

	#[test]
	fn finish_closes_an_open_gap_and_stops_counting() {
		let (mut ledger, mut observer) = observed();
		add(&mut ledger, 128, 128);
		add(&mut ledger, 128, 0);
		{
			let mut cx = std::task::Context::from_waker(Waker::noop());
			let mut finish = std::pin::pin!(observer.finish());
			assert!(
				finish.as_mut().poll(&mut cx).is_pending(),
				"no callback has seen it yet"
			);
			add(&mut ledger, 128, 0);
			assert!(finish.as_mut().poll(&mut cx).is_ready());
		}
		add(&mut ledger, 128, 128);
		add(&mut ledger, 128, 0);
		ledger.add(&buffer(128, 0), 2, 0, false);
		drop(ledger);

		let observation = observer.read().unwrap();
		assert_eq!(observation.gaps.len(), 1, "{observation:?}");
		assert_eq!(
			observation.gaps[0].quanta, 1,
			"closed before the next quantum joined it"
		);
		assert_eq!(observation.quanta, 2, "nothing after the finish counts");
		assert_eq!(
			observation.stalls.len(),
			1,
			"the stall after the finish is not reported"
		);
		assert_eq!(observation.clock, ms(128 * 6), "the clock still runs");
	}

	/// A stream going away closes out a waiting finish and wakes it, since no
	/// callback is left to.
	#[test]
	fn finish_wakes_when_the_stream_goes_away() {
		struct Flag(AtomicBool);
		impl std::task::Wake for Flag {
			fn wake(self: Arc<Self>) {
				self.0.store(true, Ordering::Release);
			}
		}

		let (ledger, mut observer) = observed();
		let flag = Arc::new(Flag(AtomicBool::new(false)));
		let waker = Waker::from(flag.clone());
		let mut cx = std::task::Context::from_waker(&waker);
		let mut finish = std::pin::pin!(observer.finish());
		assert!(finish.as_mut().poll(&mut cx).is_pending());

		drop(ledger);
		assert!(flag.0.load(Ordering::Acquire), "the waiting finish was not woken");
		assert!(finish.as_mut().poll(&mut cx).is_ready());
	}

	/// With no stream left to close it out, such as after the engine shut down,
	/// a finish resolves at once, and a stream opened after it counts nothing.
	#[test]
	fn finish_without_a_stream_resolves_at_once() {
		let shared = Arc::new(Shared::default());
		let mut observer = Observer::new(shared.clone()).unwrap();
		let mut ledger = Ledger::new(shared.clone(), RATE);
		add(&mut ledger, 128, 128);
		add(&mut ledger, 128, 0);
		drop(ledger);
		{
			let mut cx = std::task::Context::from_waker(Waker::noop());
			let mut finish = std::pin::pin!(observer.finish());
			assert!(finish.as_mut().poll(&mut cx).is_ready());
		}
		let mut ledger = Ledger::new(shared, RATE);
		add(&mut ledger, 128, 128);
		let observation = observer.read().unwrap();
		assert_eq!(
			observation.gaps.len(),
			1,
			"the dropped stream closed its gap: {observation:?}"
		);
		assert_eq!(observation.quanta, 2, "nothing after the finish counts");
	}

	/// A finish dropped while it waits, such as on a timeout, leaves no waker behind.
	#[test]
	fn a_cancelled_finish_releases_its_waker() {
		let (_ledger, mut observer) = observed();
		let shared = observer.shared.clone();
		{
			let mut cx = std::task::Context::from_waker(Waker::noop());
			let mut finish = std::pin::pin!(observer.finish());
			assert!(finish.as_mut().poll(&mut cx).is_pending());
			assert!(!shared.waiters.lock().unwrap().is_empty());
		}
		assert!(shared.waiters.lock().unwrap().is_empty());
	}

	#[test]
	fn a_gap_open_across_observers_stays_with_the_first() {
		let shared = Arc::new(Shared::default());
		let mut ledger = Ledger::new(shared.clone(), RATE);
		let first = Observer::new(shared.clone()).unwrap();
		add(&mut ledger, 128, 128);
		add(&mut ledger, 128, 0);
		drop(first);
		let mut second = Observer::new(shared).unwrap();
		add(&mut ledger, 128, 128);
		assert!(second.read().unwrap().gaps.is_empty());
	}

	#[test]
	fn a_new_observer_inherits_nothing_from_the_last() {
		let shared = Arc::new(Shared::default());
		let first = Observer::new(shared.clone()).unwrap();
		let stale = first.generation;
		drop(first);
		// A send that raced the first observer's teardown, and the overflow it hit.
		let gap = Gap {
			at: Duration::ZERO,
			missing: Duration::ZERO,
			quanta: 1,
			short: 0,
		};
		shared.events.try_send((stale, Event::Gap(gap))).unwrap();
		shared.overflowed.store(stale, Ordering::Release);

		let mut second = Observer::new(shared).unwrap();
		let observation = second.read().unwrap();
		assert!(observation.gaps.is_empty());
	}

	#[test]
	fn a_rebuilt_stream_continues_the_clock() {
		let shared = Arc::new(Shared::default());
		let mut observer = Observer::new(shared.clone()).unwrap();
		let mut ledger = Ledger::new(shared.clone(), RATE);
		add(&mut ledger, 480, 480);
		drop(ledger);
		let mut ledger = Ledger::new(shared, 44_100);
		ledger.add(&buffer(441, 441), 2, 441, true);
		assert_eq!(observer.read().unwrap().clock, ms(480) + Duration::from_millis(10));
	}

	#[test]
	fn a_stream_dropped_while_playing_stalls_until_the_next_one_plays() {
		let shared = Arc::new(Shared::default());
		let mut observer = Observer::new(shared.clone()).unwrap();
		let mut ledger = Ledger::new(shared.clone(), RATE);
		add(&mut ledger, 128, 128);
		drop(ledger);
		let observation = observer.read().unwrap();
		assert!(observation.stalled, "no stream is playing");
		assert_eq!(
			observation.stalls.last(),
			Some(&Stall {
				at: ms(128),
				stalled: true
			})
		);

		// The new stream recovers through empty quanta, which count: the engine already started.
		let mut ledger = Ledger::new(shared, RATE);
		ledger.add(&[0.0; 256 * 2], 2, 0, true);
		let observation = observer.read().unwrap();
		assert!(!observation.stalled);
		assert_eq!((observation.quanta, observation.quiet), (3, 2));
	}

	#[test]
	fn an_overflowed_queue_fails_the_read() {
		let (mut ledger, mut observer) = observed();
		for _ in 0..=EVENTS {
			add(&mut ledger, 128, 128);
			add(&mut ledger, 128, 0);
		}
		add(&mut ledger, 128, 128);
		assert!(matches!(observer.read(), Err(Error::Playback(_))));
	}

	#[test]
	fn one_observer_at_a_time() {
		let shared = Arc::new(Shared::default());
		let first = Observer::new(shared.clone()).unwrap();
		assert!(matches!(Observer::new(shared.clone()), Err(Error::Busy(_))));
		drop(first);
		Observer::new(shared).unwrap();
	}

	#[test]
	fn events_only_queue_while_observed() {
		let shared = Arc::new(Shared::default());
		let mut ledger = Ledger::new(shared.clone(), RATE);
		for _ in 0..=EVENTS {
			add(&mut ledger, 128, 128);
			add(&mut ledger, 128, 0);
		}
		let mut observer = Observer::new(shared).unwrap();
		add(&mut ledger, 128, 128);
		let observation = observer.read().unwrap();
		assert_eq!(observation.gaps.len(), 1);
		assert_eq!(observation.quanta as usize, 2 * (EVENTS + 1) + 1);
	}
}
