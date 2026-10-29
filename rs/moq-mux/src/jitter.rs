//! A jitter buffer that releases several tracks' frames in one decode order, each a fixed
//! delay after its decode time.

use std::borrow::Borrow;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::pin::Pin;
use std::task::Poll;
use std::time::Duration;

use moq_clock::{Instant, Sleep};
use moq_net::Timestamp;

/// One active publisher and at most one independently acquiring replacement.
/// Ordinary timeline jumps stay entirely inside each existing clock engine.
pub(crate) struct Buffer<K, T> {
	active: Engine<K, T>,
	handoff: Option<Box<Handoff<K, T>>>,
}

enum Handoff<K, T> {
	Pending(Pending<K, T>),
	Committed(Committed<K, T>),
}

struct Pending<K, T> {
	keys: BTreeSet<K>,
	engine: Engine<K, T>,
	held: BTreeMap<K, Ready<K, T>>,
}

struct Committed<K, T> {
	keys: BTreeSet<K>,
	old: Engine<K, T>,
}

impl<K, T> std::ops::Deref for Buffer<K, T> {
	type Target = Engine<K, T>;
	fn deref(&self) -> &Self::Target {
		&self.active
	}
}
impl<K, T> std::ops::DerefMut for Buffer<K, T> {
	fn deref_mut(&mut self) -> &mut Self::Target {
		&mut self.active
	}
}

impl<K: Ord + Clone, T> Buffer<K, T> {
	pub fn new(delay: Duration) -> Self {
		Self {
			active: Engine::new(delay),
			handoff: None,
		}
	}

	#[cfg(test)]
	pub fn replay(mut self) -> Self {
		self.active = self.active.replay();
		self
	}

	fn pending(&self) -> Option<&Pending<K, T>> {
		match self.handoff.as_deref() {
			Some(Handoff::Pending(pending)) => Some(pending),
			_ => None,
		}
	}
	fn committed(&self) -> Option<&Committed<K, T>> {
		match self.handoff.as_deref() {
			Some(Handoff::Committed(committed)) => Some(committed),
			_ => None,
		}
	}

	/// Acquire another publisher without replacing the active publisher's clock.
	pub fn start(&mut self, keys: impl IntoIterator<Item = K>, expect: impl IntoIterator<Item = K>) {
		debug_assert!(self.handoff.is_none());
		let mut engine = Engine::new(self.active.delay);
		engine.replay = self.active.replay;
		engine.expect(expect);
		self.handoff = Some(Box::new(Handoff::Pending(Pending {
			keys: keys.into_iter().collect(),
			engine,
			held: BTreeMap::new(),
		})));
	}

	pub fn expect_pending(&mut self, keys: impl IntoIterator<Item = K>, expect: impl IntoIterator<Item = K>) {
		if let Some(Handoff::Pending(pending)) = self.handoff.as_deref_mut() {
			pending.keys = keys.into_iter().collect();
			pending.engine.expect(expect);
		} else if let Some(Handoff::Committed(committed)) = self.handoff.as_deref_mut() {
			committed.keys = keys.into_iter().collect();
			self.active.expect(expect);
		}
	}

	fn reading<Q: Ord + ?Sized>(&self, key: &Q) -> &Engine<K, T>
	where
		K: Borrow<Q>,
	{
		match self.handoff.as_deref() {
			Some(Handoff::Pending(pending)) if pending.keys.contains(key) => &pending.engine,
			_ => &self.active,
		}
	}
	fn writing<Q: Ord + ?Sized>(&mut self, key: &Q) -> &mut Engine<K, T>
	where
		K: Borrow<Q>,
	{
		match self.handoff.as_deref_mut() {
			Some(Handoff::Pending(pending)) if pending.keys.contains(key) => &mut pending.engine,
			_ => &mut self.active,
		}
	}

	pub fn push(&mut self, key: K, arrival: Arrival<T>) -> anyhow::Result<Push> {
		if let Some(Handoff::Committed(committed)) = self.handoff.as_deref() {
			anyhow::ensure!(
				committed.keys.contains(&key),
				"a retired publisher queued after its handoff deadline"
			);
		}
		self.writing(&key).push(key, arrival)
	}
	pub fn full<Q: Ord + ?Sized>(&self, key: &Q) -> bool
	where
		K: Borrow<Q>,
	{
		self.pending().is_some_and(|pending| pending.held.contains_key(key)) || self.reading(key).full(key)
	}
	pub fn holds<Q: Ord + ?Sized>(&self, key: &Q) -> bool
	where
		K: Borrow<Q>,
	{
		self.pending().is_some_and(|pending| pending.held.contains_key(key)) || self.reading(key).holds(key)
	}
	pub fn peek<Q: Ord + ?Sized>(&self, key: &Q) -> Option<&T>
	where
		K: Borrow<Q>,
	{
		self.pending()
			.and_then(|pending| pending.held.get(key))
			.map(|ready| &ready.item)
			.or_else(|| self.reading(key).peek(key))
	}
	pub fn lose<Q: Ord + ?Sized>(&mut self, key: &Q, ahead: bool)
	where
		K: Borrow<Q>,
	{
		self.writing(key).lose(key, ahead);
	}
	pub fn discard(&mut self, ready: &Ready<K, T>) {
		if let Some(Handoff::Committed(committed)) = self.handoff.as_deref_mut()
			&& !committed.keys.contains(&ready.track)
		{
			committed.old.discard(ready);
			if ready.first
				&& let Some(track) = self.active.tracks.get_mut(&ready.track)
			{
				track.started = false;
			}
			return;
		}
		self.writing(&ready.track).discard(ready);
	}
	pub fn forget<Q: Ord + ?Sized>(&mut self, key: &Q)
	where
		K: Borrow<Q>,
	{
		if let Some(Handoff::Committed(committed)) = self.handoff.as_deref_mut()
			&& !committed.keys.contains(key)
		{
			committed.old.forget(key);
		}
		self.writing(key).forget(key);
	}

	/// Preview only the new publisher, leaving the old publisher free to feed and play.
	pub fn poll_pending(&mut self, waiter: &kio::Waiter) -> Poll<Ready<K, T>> {
		match self.handoff.as_deref_mut() {
			Some(Handoff::Pending(pending)) => pending.engine.poll_available(waiter, Some(&pending.held)),
			_ => Poll::Pending,
		}
	}

	/// One issued item per track, keeping that track backpressured until video is stable.
	pub fn hold_pending(&mut self, ready: Ready<K, T>) {
		let Some(Handoff::Pending(pending)) = self.handoff.as_deref_mut() else {
			unreachable!("an acquiring publisher");
		};
		pending.held.insert(ready.track.clone(), ready);
	}

	pub fn take_pending(&mut self) -> Vec<Ready<K, T>> {
		match self.handoff.as_deref_mut() {
			Some(Handoff::Pending(pending)) => std::mem::take(&mut pending.held).into_values().collect(),
			_ => Vec::new(),
		}
	}

	/// The new program is emittable. The old program's queued tail is dropped: the switch
	/// already breaks the program, and draining the tail first would add it to the latency
	/// of every later frame. Old clocks are kept until adoption for what already went out.
	pub fn commit(&mut self, ready: Ready<K, T>) -> Result<(), moq_net::TimeOverflow> {
		// Nothing more of the old publisher is read, so a timeline restart it is still
		// acquiring is settled now, then dropped with the rest of its tail.
		self.active.acquired();
		for track in self.active.tracks.values_mut() {
			let cut = std::mem::take(&mut track.queue).len() as u64;
			// A cancel resumes the old publisher, whose next frames may reference the cut ones.
			track.waiting |= cut > 0;
			match track.started {
				true => self.active.dropped += cut,
				false => self.active.trimmed += cut,
			}
		}
		let Handoff::Pending(Pending {
			keys,
			mut engine,
			mut held,
		}) = *self.handoff.take().expect("an acquiring publisher")
		else {
			unreachable!("an acquiring publisher");
		};
		held.insert(ready.track.clone(), ready);
		for (_, ready) in held {
			let track = engine.tracks.get_mut(&ready.track).expect("the previewed track");
			// The issued start is owned by the caller even while it waits on the old tail.
			track
				.queue
				.push_front((ready.generation, ready.decode, ready.deadline, ready.item));
		}
		let first = engine
			.tracks
			.values()
			.flat_map(|track| &track.queue)
			.map(|(_, _, at, _)| *at)
			.min();
		// Only what already went out bounds the new program, so output stays in order while
		// the new program keeps the delay its own clock acquired.
		let shift = self
			.active
			.released
			.zip(first)
			.map_or(Duration::ZERO, |(old, new)| old.saturating_duration_since(new));
		tracing::debug!(target: "moq_mux::container::ts::handoff", phase = "commit-clock", now = ?moq_clock::now(), old_released = ?self.active.released, new_first = ?first, ?shift, old_clock = ?self.active.clock.map(|clock| (clock.anchor, clock.base, clock.generation)), new_clock = ?engine.clock.map(|clock| (clock.anchor, clock.base, clock.generation)));
		let offset = self.active.clock.map_or(0, |clock| clock.generation + 1);
		for clock in [&mut engine.clock, &mut engine.previous].into_iter().flatten() {
			clock.anchor = clock.anchor.checked_add(shift).ok_or(moq_net::TimeOverflow)?;
			clock.generation += offset;
		}
		for track in engine.tracks.values_mut() {
			track.generation += offset;
			for (generation, _, at, _) in &mut track.queue {
				*generation += offset;
				*at = at.checked_add(shift).ok_or(moq_net::TimeOverflow)?;
			}
		}
		if let Some(acquire) = &mut engine.acquire {
			acquire.generation += offset;
		}
		engine.horizon = engine
			.horizon
			.map(|horizon| horizon.checked_add(shift).ok_or(moq_net::TimeOverflow))
			.transpose()?
			.max(self.active.released);
		engine.tracks.append(&mut self.active.tracks);
		let old = std::mem::replace(&mut self.active, engine);
		self.handoff = Some(Box::new(Handoff::Committed(Committed { keys, old })));
		Ok(())
	}

	pub fn is_committed(&self) -> bool {
		matches!(self.handoff.as_deref(), Some(Handoff::Committed(_)))
	}
	pub fn has_pending(&self) -> bool {
		self.pending().is_some()
	}
	pub fn pending_empty(&self) -> bool {
		self.pending()
			.is_none_or(|pending| pending.held.is_empty() && pending.engine.is_empty())
	}

	/// Return to the active publisher if a newer selection supersedes an unemitted one.
	pub fn cancel(&mut self) {
		if let Some(handoff) = self.handoff.take()
			&& let Handoff::Committed(Committed { keys, mut old }) = *handoff
		{
			old.tracks = self
				.active
				.tracks
				.extract_if(.., |key, _| !keys.contains(key))
				.collect();
			self.active = old;
		}
	}

	pub fn adopted(&mut self) {
		if let Some(handoff) = self.handoff.take()
			&& let Handoff::Committed(committed) = *handoff
		{
			self.active.tracks.retain(|key, _| committed.keys.contains(key));
		}
	}

	/// Statistics describe the emitted program, including its tail before adoption.
	pub fn reporting(&self) -> &Engine<K, T> {
		self.committed().map_or(&self.active, |committed| &committed.old)
	}

	pub fn at(&self, generation: u64, decode: impl Into<Decode>) -> Option<Instant> {
		let decode = decode.into();
		self.committed()
			.and_then(|committed| committed.old.at(generation, decode))
			.or_else(|| self.active.at(generation, decode))
	}

	#[cfg(test)]
	pub fn len(&self) -> usize {
		self.active.len()
			+ self
				.pending()
				.map_or(0, |pending| pending.engine.len() + pending.held.len())
	}

	#[cfg(test)]
	pub fn next_deadline(&self) -> Option<Instant> {
		self.active
			.next_deadline()
			.into_iter()
			.chain(
				self.pending()
					.and_then(|pending| pending.engine.next_available_deadline(Some(&pending.held))),
			)
			.min()
	}
}

/// An authored decode instant in signed nanoseconds, including times before PTS zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Decode(i128);

impl Decode {
	pub(crate) fn from_nanos(nanos: i128) -> Self {
		Self(nanos)
	}

	/// The instant `ticks` of a 90 kHz clock, rounded down.
	pub(crate) fn from_ticks(ticks: i128) -> Self {
		Self((ticks * 1_000_000_000).div_euclid(90_000))
	}

	fn as_nanos(self) -> i128 {
		self.0
	}
}

impl From<Timestamp> for Decode {
	fn from(timestamp: Timestamp) -> Self {
		Self::from_nanos(timestamp.as_nanos() as i128)
	}
}

/// A frame handed to [`Buffer::push`].
pub(crate) struct Arrival<T> {
	/// When the frame reached the reader: when it landed in the cache, or when the reader
	/// subscribed if that was later. Its deadline is judged on this, so a caller that reads late
	/// does not make it late, and a stall that holds the caller back too does not hide that it is.
	pub arrived: Instant,
	/// When it was read: the latest it could have arrived, which its freshness is judged on,
	/// so frames read together do not look fresher than they are.
	pub read: Instant,
	/// When the frame decodes, nondecreasing within a track.
	pub decode: Decode,
	/// How many times the source had restarted its timeline when the frame was read.
	pub restart: u64,
	/// How many times the source's playhead had jumped (a skipped group, or a restart)
	/// when the frame was read.
	pub skip: u64,
	/// Whether the frame decodes without the track's earlier frames.
	pub sync: bool,
	/// Whether it presents before the track's last sync frame, so it also references frames
	/// before that one: an open GOP's leading picture.
	pub leading: bool,
	pub item: T,
}

/// A frame [`Buffer::poll_next`] let go.
pub(crate) struct Ready<K, T> {
	pub track: K,
	/// Counts up each time a restart moved the timeline, and the clock with it. Every
	/// frame of one generation goes out before any frame of the next.
	pub generation: u64,
	pub item: T,
	decode: Decode,
	deadline: Instant,
	/// Whether it is the first frame of its track to go out.
	pub first: bool,
}

/// What [`Buffer::push`] did with a frame.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Push {
	Queued,
	/// It missed its deadline and was dropped; the track resumes at its next sync frame.
	Late,
	/// It was dropped because the track is waiting for a sync frame, or leads the one it resumed at.
	Waiting,
}

/// Holds each track's frames until a fixed delay past their decode time, like an SRT
/// receiver's TSBPD, in decode order across tracks.
///
/// Every track runs on one clock. Under it the deadline order is the decode order, so two
/// buffers that saw the same frames arrive with different skew, each within its deadline,
/// emit them in the same order. A frame that arrives past its deadline would break that
/// order, so it is dropped and counted.
///
/// The clock is acquired before anything goes out: frames are held until every expected
/// track has delivered (two delays at most) and a track starts a new group, or for a delay
/// at most once all have. Each track's freshest frame, the one that arrived least behind its
/// decode time, shows how far ahead of its decode time the source sends it; the clock is
/// anchored on the track sent latest, so its freshest frame is due a delay after it arrived
/// and every other track has at least the delay. A TS source sends video most of a second
/// ahead and audio just in time, and a clock anchored on the video would make every audio
/// frame late. A joiner is handed the group it joined at once, already partly old, and
/// anchoring on its first frame would carry that lag for the whole run. What the anchor
/// makes due before the acquisition ended is dropped instead.
///
/// A track starts on a sync frame. Until a frame of it has gone out, a frame dropped here
/// takes everything the track holds with it, and the track starts at its next sync frame.
/// The rest of a group its source cut short takes nothing: what the track holds of it
/// starts on its sync frame and decodes, and the hole after it is shed like any later one. A
/// path narrower than the media cuts every group, so waiting for a whole one could leave
/// the output dark for the whole run.
/// What goes before a track's first frame out is [`Buffer::trimmed`], not lost; what drops
/// after it is [`Buffer::dropped`].
///
/// A skipped group leaves the timeline where it was, so it keeps the clock: frames it made
/// late are dropped like any other. A source that restarts its timeline (a declared
/// marker) has moved it, so once frames have gone out the restart opens a new generation,
/// whose clock is acquired the same way, though never ahead of a deadline already given
/// out. So does a skip after which a frame would be held more than a delay past everything
/// queued, beyond how far ahead of the others its track is sent: its timeline jumped ahead,
/// as when the skipped groups held a marker the publisher shed. So does a step with no marker
/// at all, as a changeover between two encoders whose clocks disagree, once the delay cannot
/// absorb it: read as drift it would fail [`Buffer::push`], and steering on it would let the
/// step's span of decode time slew the drift in one go. A smaller one is a move in latency.
/// How far ahead of its decode time the source sends each track belongs to its mux and
/// survives a restart, so the new clock does not wait to hear from a track the previous one
/// measured: it is anchored late enough that the track sent latest keeps the delay when it
/// gets there. A track that has not reached the restart stays on the previous generation
/// while its frames land there and go out before the new generation's first frame; one that
/// would follow it drops as late.
/// One crossing the same restart joins the new generation when the frame lands on its
/// clock, neither late nor held more than a delay past everything queued, and opens another
/// otherwise. Before anything has gone out there is nothing to keep in step with, so a
/// restart there keeps the clock. Generations go out in turn, so no two tracks are ever on
/// different clocks at once.
///
/// The clock follows the source's: a source clock running slower than ours would make
/// every frame late in the end, and a faster one would hold more and more. So the clock
/// measures the source's rate and runs its decode timeline at it, holding the freshest
/// frames of the track sent latest a delay. Each [`STEER`] of our time gives each track it
/// hears its most slack, which queueing and retransmission only ever lower, and a floor,
/// the least of those (a track not heard placed by its lead). Over [`RATE_WINDOW`] the
/// rate is the slope of one track's upper envelope, the track whose slack keeps closest to
/// it: a TS mux sends its video ahead by however full the encoder's buffer is, so the
/// video's slack wanders by hundreds of ms where the audio's keeps within a few. The slack
/// now is the floors' upper envelope at that slope, kept within the floors seen, so the
/// track sent latest keeps the delay, and a spell of queueing shorter than half the window
/// moves neither.
/// The backlog a network stall releases is read at once, so it gives one floor, its
/// freshest frame's: stepped on decode time it would give one per step of the media it
/// held, and early on, before the window fills, those would read as a source far slower
/// than ours. The decode timeline is the output's system clock, so it keeps to what ISO/IEC
/// 13818-1 2.4.2.1 allows one: within [`MAX_DRIFT`] of ours, changing by at most
/// [`MAX_SLEW`]. A source past that is counted, and once it has cost half the delay
/// [`Buffer::push`] fails. What anchoring on frames that reached us late or early left is
/// not charged to it, while the clock pulls it shut. So a move in latency smaller than half
/// the delay never fails; early on, one past it that the floors so far trace as a line reads
/// as the rate, as a source that far off would.
///
/// A zero delay holds nothing and drops nothing: each frame goes out as soon as it is
/// read, ordered only among the frames read together.
///
/// A caller reads a track only while it is not [`Buffer::full`], so a backlog stays in the
/// source and goes out as it falls due, rather than being queued here whole first.
pub(crate) struct Engine<K, T> {
	delay: Duration,
	/// Anchor each clock on the first frame rather than acquire it: a test exporting a
	/// broadcast it wrote whole, every frame of which arrives at once.
	replay: bool,
	/// The clock every frame is pushed under.
	clock: Option<Clock>,
	/// The generation before it, for a track the restart has not reached yet, until the
	/// current generation's first frame goes out.
	previous: Option<Clock>,
	/// The next generation's frames, held while its clock is acquired.
	acquire: Option<Acquire<K, T>>,
	tracks: BTreeMap<K, Track<T>>,
	/// The latest deadline given out.
	horizon: Option<Instant>,
	/// The tracks an acquisition waits to hear from, bounded, so that it anchors on whichever
	/// of them is sent latest against its decode time.
	expect: BTreeSet<K>,
	/// The latest deadline of a frame gone out.
	released: Option<Instant>,
	/// What the clock is steered on, since it last started.
	steer: Steer<K>,
	/// How much more slack each track had than the track sent latest, in seconds, in the last
	/// [`LEADS`] steering steps that heard it; [`Buffer::lead`] is their median. How far ahead
	/// of its decode time a source sends each track belongs to its mux, not to one generation,
	/// so a new generation keeps the track sent latest at the delay even before that track
	/// reaches it, and a step that does not hear that track still measures against it.
	leads: BTreeMap<K, VecDeque<f64>>,
	timer: Option<Pin<Box<Sleep>>>,
	dropped: u64,
	trimmed: u64,
	/// Steps at which the source's clock ran further off ours than the clock may follow.
	out_of_tolerance: u64,
	/// The least time any frame arrived before its deadline in the last whole [`MARGIN`], in
	/// seconds: negative for a frame that missed it.
	margin: Option<f64>,
	/// The [`MARGIN`] under way: when its first frame was read, and the least time to spare so far.
	spare: Option<(Instant, f64)>,
}

/// How often the clock is steered, in our time: long enough that the floor covers a
/// group's worth of frames sent ahead by different amounts.
const STEER: Duration = Duration::from_secs(2);
/// How long [`Buffer::margin`] takes the least of, in our time: a report's interval.
const MARGIN: Duration = Duration::from_secs(1);
/// How far back the floors are kept, in decode time: a spell of queueing shorter than half
/// of it moves neither the rate nor the slack.
const RATE_WINDOW: f64 = 600.0;
/// How many steering steps a track's lead is the median of: a window's worth, so a spell of
/// queueing shorter than half of it does not move the lead either.
const LEADS: usize = (RATE_WINDOW / STEER.as_secs_f64()) as usize;
/// How quickly the clock pulls the slack back to the delay: it closes the gap over this
/// many seconds, or slower where the slew limit could not stop it in time.
const RESPONSE: f64 = 600.0;
/// The furthest the clock runs off ours: 810 Hz of the 27 MHz system clock (ISO/IEC 13818-1
/// 2.4.2.1).
const MAX_DRIFT: f64 = 810.0 / 27e6;
/// How fast that may change, per second: 0.075 Hz/s of 27 MHz (ISO/IEC 13818-1 2.4.2.1).
const MAX_SLEW: f64 = 0.075 / 27e6;
/// How close two tracks' depths under their envelopes are to fit them equally, in seconds: a
/// nanosecond, the resolution of the times they are measured from.
const FIT: f64 = 1e-9;

/// The decode timeline's mapping onto ours: where a frame decoding at `base` is due, less the
/// delay, and how much longer than the decode timeline ours runs.
#[derive(Clone, Copy)]
struct Clock {
	generation: u64,
	anchor: Instant,
	base: Decode,
	/// As a fraction of the decode timeline.
	drift: f64,
}

/// A generation whose clock is being acquired.
struct Acquire<K, T> {
	generation: u64,
	/// When its first frame was read; it ends a delay later at the latest.
	since: Instant,
	/// Its frames, in arrival order.
	frames: Vec<(K, Arrival<T>)>,
	/// Its tracks, and whether each is partway through a group, so its next sync frame
	/// starts the next one.
	tracks: BTreeMap<K, bool>,
	/// A track crossed a group boundary while these frames were being read.
	grouped: bool,
}

impl<K: Ord, T> Acquire<K, T> {
	/// The frame to anchor on: each track's freshest frame (the one read least behind its
	/// decode time) is that track's slack less its queueing, and of those, the one with the least
	/// slack, so the track sent latest against its decode time still has the delay.
	fn fresh(&self) -> (&K, &Arrival<T>) {
		let behind = |frame: &Arrival<T>| since(self.since, frame.read) - frame.decode.as_nanos();
		let mut freshest: BTreeMap<&K, &Arrival<T>> = BTreeMap::new();
		for (key, frame) in &self.frames {
			let best = freshest.entry(key).or_insert(frame);
			if behind(frame) < behind(best) {
				*best = frame;
			}
		}
		freshest
			.into_iter()
			.max_by_key(|(_, frame)| behind(frame))
			.expect("an acquisition holds a frame")
	}

	/// When it ends: once a frame that could still go out on time falls due on the freshest
	/// frame's clock, since waiting longer would drop it, and a delay after it began at the
	/// latest.
	fn end(&self, delay: Duration) -> Instant {
		let (_, fresh) = self.fresh();
		let due = |frame: &Arrival<T>| {
			let ahead = frame.decode.as_nanos() - fresh.decode.as_nanos();
			offset(fresh.read + delay, ahead)
		};
		self.frames
			.iter()
			.filter_map(|(_, frame)| due(frame).filter(|due| *due >= frame.read))
			.chain([self.since + delay])
			.min()
			.expect("a bound")
	}
}

/// What the clock is steered on.
struct Steer<K> {
	/// This step's floor.
	floor: Option<Floor<K>>,
	/// Each step's floor, as its decode time and phase in seconds, over the last
	/// [`RATE_WINDOW`].
	phase: VecDeque<(f64, f64)>,
	/// Each track's own phase at each step that heard it, over the same window. The slope of
	/// the steadiest's envelope is how much faster the source's clock runs than ours.
	tracks: BTreeMap<K, VecDeque<(f64, f64)>>,
	/// How far the drift has moved the deadlines, in seconds.
	steered: f64,
	/// The source's clock rate off ours, as last measured: positive runs fast.
	source: Option<f64>,
	/// How far the slack of the track sent latest sat off the delay, in seconds, as last
	/// measured: the gap the clock pulls shut.
	offset: Option<f64>,
	/// What is left of the gap between the slack and the delay that the rate's first
	/// measurement found, in seconds: what anchoring on frames that reached us late or early
	/// left, which the rate did not cost.
	startup: Option<f64>,
}

impl<K> Default for Steer<K> {
	fn default() -> Self {
		Self {
			floor: None,
			phase: VecDeque::new(),
			tracks: BTreeMap::new(),
			steered: 0.0,
			source: None,
			offset: None,
			startup: None,
		}
	}
}

impl<K: Ord> Steer<K> {
	/// The source's rate and the floor's phase now. The rate is the slope of the envelope of
	/// whichever track in `heard`, of those heard in at least half the floors, lies closest
	/// under its own at the median, so a spell of queueing on it shorter than half the window
	/// does not hand the rate to another; the phase is the floors' envelope at that slope.
	///
	/// Of tracks that fit their envelopes equally, as any with only two floors does, the one
	/// measuring the least drift is taken: a track whose send-ahead wanders fits as well as one
	/// that holds steady, and only the source's clock moves every track.
	fn measure<'a>(&self, heard: impl Iterator<Item = &'a K>, now: f64) -> Option<(f64, f64)>
	where
		K: 'a,
	{
		let fits: Vec<(f64, f64)> = heard
			.filter_map(|key| self.tracks.get(key))
			.filter(|series| 2 * series.len() >= self.phase.len())
			.filter_map(|series| {
				let (slope, at) = envelope(series, now)?;
				Some((depth(series, slope, at, now), slope))
			})
			.collect();
		let best = fits.iter().map(|&(depth, _)| depth).fold(f64::INFINITY, f64::min);
		let steadiest = fits
			.iter()
			.filter(|&&(depth, _)| depth - best <= FIT)
			.map(|&(_, slope)| slope)
			.min_by(|a, b| a.abs().total_cmp(&b.abs()));
		let rate = match steadiest {
			Some(rate) => rate,
			None => envelope(&self.phase, now)?.0,
		};
		let at = self.phase.iter().map(|&(x, y)| y + rate * (now - x)).reduce(f64::max)?;
		Some((rate, at))
	}
}

/// Each track's frame with the most slack in a step: the one queued least on its way. The
/// step's floor is the track with the least of those, the one sent latest against its decode time.
struct Floor<K> {
	/// Per track, its slack past the delay, less how far the drift has moved its deadline, and
	/// when it decodes, in seconds.
	tracks: BTreeMap<K, (f64, f64)>,
	/// When the step's first frame was read.
	began: Instant,
}

struct Track<T> {
	/// Frames in arrival order, each with its generation, decode time and deadline.
	queue: VecDeque<(u64, Decode, Instant, T)>,
	/// The source's restart and skip counters at the last frame, when it decodes, and when it
	/// arrived.
	restart: u64,
	skip: u64,
	decode: Decode,
	arrived: Instant,
	/// The generation of the last frame.
	generation: u64,
	/// Frames are dropped until the next sync frame.
	waiting: bool,
	/// Whether the track resumed at its last sync frame after waiting, whose leading frames are
	/// dropped too.
	resumed: bool,
	/// Whether a frame of it has gone out. Until one has, a loss takes everything it holds.
	started: bool,
}

impl<K: Ord + Clone, T> Engine<K, T> {
	pub fn new(delay: Duration) -> Self {
		Self {
			delay,
			replay: false,
			clock: None,
			previous: None,
			acquire: None,
			tracks: BTreeMap::new(),
			horizon: None,
			expect: BTreeSet::new(),
			released: None,
			steer: Steer::default(),
			leads: BTreeMap::new(),
			timer: None,
			dropped: 0,
			trimmed: 0,
			out_of_tolerance: 0,
			margin: None,
			spare: None,
		}
	}

	/// How long past its decode time each frame is held.
	pub fn delay(&self) -> Duration {
		self.delay
	}

	/// The tracks an acquisition waits to hear from, two delays at most, before it anchors: any
	/// of them may be the one sent latest against its decode time.
	pub fn expect(&mut self, keys: impl IntoIterator<Item = K>) {
		self.expect = keys.into_iter().collect();
	}

	/// Anchor each clock on its first frame instead of acquiring it, for a test that writes a
	/// broadcast whole before exporting it.
	#[cfg(test)]
	pub fn replay(mut self) -> Self {
		self.replay = true;
		self
	}

	/// Whether each clock anchors on its first frame ([`Self::replay`]).
	pub fn replaying(&self) -> bool {
		self.replay
	}

	/// Queue a frame, or drop it if it cannot make its deadline. Fails once the source's clock
	/// runs further off ours than the clock may follow.
	pub fn push(&mut self, key: K, arrival: Arrival<T>) -> anyhow::Result<Push> {
		let held = self
			.acquire
			.as_ref()
			.is_some_and(|acquire| acquire.tracks.contains_key(&key));
		let Some(clock) = self
			.clock
			.filter(|_| !held && !(self.released.is_some() && self.jumps(&key, &arrival)))
		else {
			self.hold(key, arrival);
			return Ok(Push::Queued);
		};
		if let Some(previous) = self.lagging(&key, &arrival) {
			let deadline = self.deadline(&previous, arrival.decode);
			return Ok(self.queue(key, arrival, previous.generation, deadline, false));
		}
		let deadline = self.deadline(&clock, arrival.decode);
		// A track still on an earlier generation, crossing no restart, that the previous clock
		// refused and the current one does not land is on the old timeline: it is late, and
		// says nothing about the current clock.
		let behind = self
			.tracks
			.get(&key)
			.filter(|track| track.generation != clock.generation && track.restart == arrival.restart);
		if let Some(track) = behind
			&& !self.delay.is_zero()
			&& !self.lands(&key, deadline, arrival.arrived)
		{
			let generation = track.generation;
			return Ok(self.queue(key, arrival, generation, None, false));
		}
		if self.released.is_some() && !self.delay.is_zero() {
			self.steer(&key, deadline, &arrival)?;
		}
		Ok(self.queue(key, arrival, clock.generation, deadline, false))
	}

	/// The previous generation's clock, for a frame of a track the restart that opened the
	/// current one has not reached yet: still on the previous generation, crossing no restart,
	/// on that clock, and due no later than the current generation's first frame, so the
	/// generations still go out in turn.
	fn lagging(&self, key: &K, arrival: &Arrival<T>) -> Option<Clock> {
		let previous = self.previous?;
		let track = self.tracks.get(key)?;
		let deadline = self.deadline(&previous, arrival.decode);
		let lags = track.generation == previous.generation
			&& track.restart == arrival.restart
			&& self.lands(key, deadline, arrival.arrived)
			&& deadline.is_some_and(|deadline| self.opened().is_none_or(|first| deadline <= first));
		lags.then_some(previous)
	}

	/// The earliest deadline queued on the current generation.
	fn opened(&self) -> Option<Instant> {
		let generation = self.clock?.generation;
		self.tracks
			.values()
			.flat_map(|track| &track.queue)
			.filter(|(queued, ..)| *queued == generation)
			.map(|(_, _, deadline, _)| *deadline)
			.min()
	}

	/// Whether a frame moves its track off the clock: a restart, unless the track is crossing
	/// one another track already opened a generation for and the frame lands on its clock, or
	/// a frame held past [`Self::bound`] after a skip, or while a generation is acquired, or a
	/// step in its timeline with no marker that the delay cannot absorb. The second is a track
	/// reaching the new timeline without a marker of its own, which belongs to that generation,
	/// not far ahead on the old clock.
	///
	/// An unmarked step is told apart from drift and queueing by size. Forward, a frame is held
	/// past [`Self::bound`] and reached us more than a delay earlier against its decode time
	/// than its track's last: no clock within [`MAX_DRIFT`] moves it that far in the time
	/// between them, and frames reach us in the order they are sent, so neither a backlog
	/// draining nor a track whose frames are sparse does. Back, a frame decodes before its
	/// track's last and is late: a stall only delays what comes next. It is a step on the
	/// clock's own generation, since a track still on an earlier one is on that timeline.
	/// Held for the next generation, the frame never steers the old clock. Without a delay
	/// nothing is held or late, so a step costs nothing there.
	fn jumps(&self, key: &K, arrival: &Arrival<T>) -> bool {
		let (Some(clock), Some(track)) = (self.clock, self.tracks.get(key)) else {
			return false;
		};
		let deadline = self.deadline(&clock, arrival.decode);
		let restarted = track.restart != arrival.restart
			&& (track.generation == clock.generation || !self.lands(key, deadline, arrival.arrived));
		let held = deadline.is_some_and(|deadline| deadline > self.bound(key, arrival.arrived));
		let leapt = (track.skip != arrival.skip || self.acquire.is_some()) && held;
		let unmarked = !self.delay.is_zero() && track.generation == clock.generation;
		let elapsed = arrival.decode.as_nanos() - track.decode.as_nanos();
		let ahead = elapsed - since(track.arrived, arrival.arrived);
		let drift = (elapsed.abs() as f64 * MAX_DRIFT) as i128;
		let forward = held && ahead > self.delay.as_nanos() as i128 + drift;
		let back = elapsed < 0 && deadline.is_none_or(|deadline| deadline < arrival.arrived);
		restarted || leapt || (unmarked && (forward || back))
	}

	/// Hold a frame for the generation being acquired, starting one if none is, and anchor it
	/// when output is polled after a track starts a new group. Without a delay, or replaying,
	/// it anchors at once.
	fn hold(&mut self, key: K, arrival: Arrival<T>) {
		let generation = self.clock.map_or(0, |clock| clock.generation + 1);
		let acquire = self.acquire.get_or_insert_with(|| Acquire {
			generation,
			since: arrival.read,
			frames: Vec::new(),
			tracks: BTreeMap::new(),
			grouped: false,
		});
		let mid = acquire.tracks.entry(key.clone()).or_default();
		acquire.grouped |= arrival.sync && *mid;
		*mid |= !arrival.sync;
		acquire.frames.push((key, arrival));
		// A live fill must read every track's available frames before choosing the clock.
		// Otherwise the first track crossing a group boundary anchors on a partial backlog.
		if self.replay || self.delay.is_zero() {
			self.acquired();
		}
	}

	/// Anchor the acquired generation's clock on its freshest frame (its first, replaying or
	/// without a delay), and queue what it held, dropping what that made due before it was
	/// read: what a joiner is handed of its group that is older than the delay.
	fn acquired(&mut self) {
		let Some(acquire) = self.acquire.take() else {
			return;
		};
		let acquiring = !self.replay && !self.delay.is_zero();
		let (key, fresh) = match acquiring {
			true => acquire.fresh(),
			false => {
				let (key, frame) = acquire.frames.first().expect("an acquisition holds a frame");
				(key, frame)
			}
		};
		let mut clock = Clock {
			generation: acquire.generation,
			anchor: match acquiring {
				true => fresh.read + self.unheard_lag(&acquire, key),
				false => fresh.arrived,
			},
			base: fresh.decode,
			drift: self.clock.map_or(0.0, |clock| clock.drift),
		};
		// Never ahead of a deadline already given out.
		let kept = acquire
			.frames
			.iter()
			.filter_map(|(_, frame)| match self.judge(&clock, frame, acquiring) {
				(deadline, false) => deadline,
				(_, true) => None,
			})
			.min();
		if let (Some(horizon), Some(kept)) = (self.horizon, kept)
			&& kept < horizon
		{
			clock.anchor += horizon - kept;
		}
		self.previous = self.clock.replace(clock);
		self.steer = Steer::default();
		for (key, frame) in acquire.frames {
			let (deadline, stale) = self.judge(&clock, &frame, acquiring);
			self.queue(key, frame, clock.generation, deadline, stale);
		}
	}

	/// A held frame's deadline on `clock`, and whether it was due before it was read, when
	/// `acquiring`.
	fn judge(&self, clock: &Clock, frame: &Arrival<T>, acquiring: bool) -> (Option<Instant>, bool) {
		let deadline = self.deadline(clock, frame.decode);
		(
			deadline,
			acquiring && deadline.is_none_or(|deadline| deadline < frame.read),
		)
	}

	/// Queue a frame on `generation` due at `deadline`, unless it is `stale` or cannot make it.
	///
	/// Until a frame of the track has gone out, a frame dropped here takes everything the track
	/// holds with it, so its output starts at its next sync frame rather than ahead of a hole.
	fn queue(&mut self, key: K, arrival: Arrival<T>, generation: u64, deadline: Option<Instant>, stale: bool) -> Push {
		if let Some(deadline) = deadline.filter(|_| !stale && !self.delay.is_zero()) {
			self.spare(arrival.read, since(arrival.arrived, deadline) as f64 / 1e9);
		}
		let track = self.tracks.entry(key).or_insert_with(|| Track {
			queue: VecDeque::new(),
			restart: arrival.restart,
			skip: arrival.skip,
			decode: arrival.decode,
			arrived: arrival.arrived,
			generation,
			waiting: false,
			resumed: false,
			started: false,
		});
		track.restart = arrival.restart;
		track.skip = arrival.skip;
		track.decode = arrival.decode;
		track.arrived = arrival.arrived;
		track.generation = generation;
		let push = match deadline {
			_ if stale => {
				track.waiting = true;
				Push::Waiting
			}
			_ if track.waiting && !arrival.sync => Push::Waiting,
			_ if track.resumed && arrival.leading => Push::Waiting,
			Some(deadline) if arrival.arrived <= deadline || self.delay.is_zero() => {
				if arrival.sync {
					track.resumed = track.waiting;
				}
				track.waiting = false;
				track
					.queue
					.push_back((generation, arrival.decode, deadline, arrival.item));
				self.horizon = self.horizon.max(Some(deadline));
				Push::Queued
			}
			// Past the deadline, or so far off the clock that no instant holds it.
			_ => {
				track.waiting = true;
				Push::Late
			}
		};
		match push {
			Push::Queued => {}
			_ if track.started => self.dropped += 1,
			// A leading frame of the sync frame the track starts at, which stays queued.
			_ if !track.waiting => self.trimmed += 1,
			_ => {
				self.trimmed += 1 + track.queue.len() as u64;
				track.queue.clear();
			}
		}
		push
	}

	/// Account for a frame read at `read` that arrived `spare` seconds before its deadline.
	fn spare(&mut self, read: Instant, spare: f64) {
		let window = self.spare.get_or_insert((read, spare));
		window.1 = window.1.min(spare);
		if read.saturating_duration_since(window.0) >= MARGIN {
			self.margin = Some(window.1);
			self.spare = None;
		}
	}

	/// Whether a frame of `key` arriving at `arrived` and due at `deadline` is on the clock: not
	/// late, and held no more than [`Self::bound`] allows.
	fn lands(&self, key: &K, deadline: Option<Instant>, arrived: Instant) -> bool {
		deadline.is_some_and(|deadline| arrived <= deadline && deadline <= self.bound(key, arrived))
	}

	/// The latest a frame of `key` arriving at `arrived` may be due and still be on the clock: a
	/// delay past both its arrival and everything already queued, and its track's lead further.
	/// A track sent ahead of the others is held that much longer, so once a stall of its own
	/// leaves only theirs queued, its next frame is not taken for a leap.
	fn bound(&self, key: &K, arrived: Instant) -> Instant {
		let lead = Duration::from_secs_f64(self.lead(key).unwrap_or(0.0).max(0.0));
		arrived.max(self.horizon.unwrap_or(arrived)) + self.delay + lead
	}

	/// Account for the slack a frame arrived with, and every [`STEER`] of our time step the
	/// clock's drift toward the source's rate plus a pull back to the delay, as far as
	/// [`MAX_SLEW`] allows.
	fn steer(&mut self, key: &K, deadline: Option<Instant>, arrival: &Arrival<T>) -> anyhow::Result<()> {
		let (Some(clock), Some(deadline)) = (self.clock, deadline) else {
			return Ok(());
		};
		let delay = self.delay.as_secs_f64();
		let now = arrival.decode.as_nanos() as f64 / 1e9;
		let elapsed = now - clock.base.as_nanos() as f64 / 1e9;
		// The source's phase at this frame: its slack past the delay, less how far the drift
		// has moved its deadline.
		let slack = since(arrival.read, deadline) as f64 / 1e9;
		let phase = slack - delay - (self.steer.steered + elapsed * clock.drift);
		let floor = self.steer.floor.get_or_insert_with(|| Floor {
			tracks: BTreeMap::new(),
			began: arrival.read,
		});
		let best = floor.tracks.entry(key.clone()).or_insert((phase, now));
		if phase > best.0 {
			*best = (phase, now);
		}
		if arrival.read.saturating_duration_since(floor.began) < STEER {
			return Ok(());
		}
		// Re-anchor where this frame decodes, so the new drift starts from there.
		let Some(anchor) = deadline.checked_sub(self.delay) else {
			return Ok(());
		};
		let floor = self.steer.floor.take().expect("a floor");
		let (mut phase, mut at) = floor
			.tracks
			.values()
			.copied()
			.min_by(|a, b| a.0.total_cmp(&b.0))
			.expect("a floor holds a track");
		// A track not heard this step, as audio that ended before its video, may be sent later
		// than every track heard: its lead places it below the heard track sent least ahead of
		// it. Without that the floor would leap when it stopped, which reads as a source far
		// faster than ours.
		let lead = |key: &K| self.lead(key).unwrap_or(0.0);
		let unheard = self
			.leads
			.keys()
			.filter(|key| !floor.tracks.contains_key(*key))
			.map(lead)
			.reduce(f64::min);
		let (heard, &(heard_phase, heard_at)) = floor
			.tracks
			.iter()
			.min_by(|a, b| lead(a.0).total_cmp(&lead(b.0)))
			.expect("a floor holds a track");
		// Where the floor sits among the leads.
		let mut base = 0.0;
		if let Some(unheard) = unheard
			&& unheard < lead(heard)
		{
			let placed = heard_phase - (lead(heard) - unheard);
			if placed < phase {
				(phase, at) = (placed, heard_at);
			}
			base = unheard + (phase - placed);
		}
		for (key, (track, _)) in &floor.tracks {
			let leads = self.leads.entry(key.clone()).or_default();
			leads.push_back(track - phase + base);
			if leads.len() > LEADS {
				leads.pop_front();
			}
		}
		self.steer.steered += elapsed * clock.drift;
		let window = |series: &mut VecDeque<(f64, f64)>| {
			while series.front().is_some_and(|&(at, _)| now - at > RATE_WINDOW) {
				series.pop_front();
			}
		};
		self.steer.phase.push_back((at, phase));
		window(&mut self.steer.phase);
		for (key, &(track, decode)) in &floor.tracks {
			self.steer
				.tracks
				.entry(key.clone())
				.or_default()
				.push_back((decode, track));
		}
		self.steer.tracks.values_mut().for_each(window);
		self.steer.tracks.retain(|_, series| !series.is_empty());
		let Some((rate, at)) = self.steer.measure(floor.tracks.keys(), now) else {
			self.clock = Some(Clock {
				anchor,
				base: arrival.decode,
				..clock
			});
			return Ok(());
		};
		// The phase rises as the source's clock runs fast, so frames arrive ever earlier.
		let slow = -rate;
		// Within the floors seen: past them the envelope is a guess at the rate, not slack any
		// frame had. Once a move in latency stops, the edge it moved along runs on past the
		// floors that hold after it, and would charge the rate for slack no frame lost or gained.
		let (low, high) = self
			.steer
			.phase
			.iter()
			.fold((f64::INFINITY, f64::NEG_INFINITY), |(low, high), &(_, y)| {
				(low.min(y), high.max(y))
			});
		let gap = at.clamp(low, high) + self.steer.steered;
		self.steer.source = Some(rate);
		self.steer.offset = Some(gap);
		// The clock pulls the startup gap shut, so it shrinks with the gap and never reopens.
		let startup = self.steer.startup.get_or_insert(gap);
		*startup = startup.clamp(gap.min(0.0), gap.max(0.0));
		let lost = gap - *startup;

		// Pull the gap shut over [`RESPONSE`], no faster than the slew limit can stop on it.
		let pull = (gap.abs() / RESPONSE).min((2.0 * MAX_SLEW * gap.abs()).sqrt());
		let target = slow - pull.copysign(gap);
		// Tracks interleave a little off decode order, so a step may decode before the last.
		let step = MAX_SLEW * elapsed.max(0.0);
		let drift = (clock.drift + (target - clock.drift).clamp(-step, step)).clamp(-MAX_DRIFT, MAX_DRIFT);
		self.clock = Some(Clock {
			anchor,
			base: arrival.decode,
			drift,
			..clock
		});

		// To a hundredth of a ppm, so a source right at the limit is not out by rounding.
		if (slow.abs() * 1e8).round() > (MAX_DRIFT * 1e8).round() {
			self.out_of_tolerance += 1;
			// The clock can never catch up, so fail once the rate has cost half the delay.
			if lost * slow.signum() < -delay / 2.0 {
				anyhow::bail!(
					"the source's clock runs {:+.1} ppm off ours, past the {:.0} ppm the output's may follow",
					rate * 1e6,
					MAX_DRIFT * 1e6
				);
			}
		}
		Ok(())
	}

	/// When a frame of `generation` decoding at `decode` goes out: on the current clock, or on the
	/// previous one while that generation's tail is still going out.
	pub fn at(&self, generation: u64, decode: impl Into<Decode>) -> Option<Instant> {
		let clock = [self.clock, self.previous]
			.into_iter()
			.flatten()
			.find(|clock| clock.generation == generation)?;
		self.deadline(&clock, decode.into())
	}

	/// When a frame decoding at `decode` goes out on `clock`, if any instant holds it.
	fn deadline(&self, clock: &Clock, decode: Decode) -> Option<Instant> {
		// Signed, since a frame can decode before the one that anchored the clock.
		let since = decode.as_nanos() - clock.base.as_nanos();
		offset(
			clock.anchor,
			since + (since as f64 * clock.drift) as i128 + self.delay.as_nanos() as i128,
		)
	}

	/// The deadline and track of the frame that goes out next: generation by generation, then
	/// in decode order, ties by track. Not in deadline order: two frames of one decode time
	/// queued either side of a steering step can be due nanoseconds apart, which would make
	/// the order depend on when each was read rather than on the media.
	fn front_available(&self, held: Option<&BTreeMap<K, Ready<K, T>>>) -> Option<(Instant, &K)> {
		self.tracks
			.iter()
			.filter(|(key, _)| held.is_none_or(|held| !held.contains_key(*key)))
			.filter_map(|(key, track)| {
				let (generation, decode, deadline, _) = track.queue.front()?;
				Some(((*generation, decode.as_nanos(), key), *deadline))
			})
			.min_by(|a, b| a.0.cmp(&b.0))
			.map(|((_, _, key), deadline)| (deadline, key))
	}

	/// When the acquisition under way ends, unless a track starts a new group first.
	fn acquired_by(&self) -> Option<Instant> {
		self.acquire.as_ref().map(|acquire| {
			if acquire.grouped && self.heard(acquire) {
				acquire.since
			} else {
				self.ends(acquire)
			}
		})
	}

	/// Whether every track it expects has delivered a frame to `acquire`, or, once a clock has
	/// run, has a lead from it that says how much later than the others it is sent.
	fn heard(&self, acquire: &Acquire<K, T>) -> bool {
		let known = |key: &K| self.clock.is_some() && self.leads.contains_key(key);
		self.expect
			.iter()
			.all(|key| acquire.tracks.contains_key(key) || known(key))
	}

	/// How much later than `fresh`'s track the latest-sent expected track that `acquire` has
	/// not heard from is sent, by the leads the previous generation measured: anchoring that
	/// much later keeps that track at the delay when it reaches the new generation.
	fn unheard_lag(&self, acquire: &Acquire<K, T>, fresh: &K) -> Duration {
		let Some(lead) = self.lead(fresh) else {
			return Duration::ZERO;
		};
		let lag = self
			.expect
			.iter()
			.filter(|key| !acquire.tracks.contains_key(key))
			.filter_map(|key| self.lead(key))
			.map(|other| lead - other)
			.fold(0.0, f64::max);
		Duration::from_secs_f64(lag)
	}

	/// How much more slack a track has than the track sent latest, in seconds: the median of
	/// its last [`LEADS`], so a spell of queueing does not move it.
	fn lead(&self, key: &K) -> Option<f64> {
		let mut leads: Vec<f64> = self.leads.get(key)?.iter().copied().collect();
		leads.sort_by(f64::total_cmp);
		Some(leads[leads.len() / 2])
	}

	/// When `acquire` ends: a track not yet heard from may be the one sent latest, so until
	/// every expected track has delivered it waits, two delays at most.
	fn ends(&self, acquire: &Acquire<K, T>) -> Instant {
		match self.heard(acquire) {
			true => acquire.end(self.delay),
			false => acquire.since + 2 * self.delay,
		}
	}

	/// The next frame whose deadline has come, in [`Self::front`] order.
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<Ready<K, T>> {
		self.poll_available(waiter, None)
	}

	fn poll_available(&mut self, waiter: &kio::Waiter, held: Option<&BTreeMap<K, Ready<K, T>>>) -> Poll<Ready<K, T>> {
		loop {
			let now = moq_clock::now();
			let acquired = self.acquired_by();
			if acquired.is_some_and(|end| now >= end) {
				self.acquired();
				continue;
			}
			let front = self
				.front_available(held)
				.map(|(deadline, key)| (deadline, key.clone()));
			if let Some((deadline, key)) = &front
				&& (self.delay.is_zero() || now >= *deadline)
			{
				let track = self.tracks.get_mut(key).expect("the front's track");
				let (generation, decode, deadline, item) = track.queue.pop_front().expect("the front frame");
				let first = !std::mem::replace(&mut track.started, true);
				self.released = self.released.max(Some(deadline));
				// A previous-generation frame queued now would go out after this one.
				if self.clock.is_some_and(|clock| clock.generation == generation) {
					self.previous = None;
				}
				return Poll::Ready(Ready {
					track: key.clone(),
					generation,
					item,
					decode,
					deadline,
					first,
				});
			}
			let Some(wake) = front.map(|(deadline, _)| deadline).into_iter().chain(acquired).min() else {
				self.timer = None;
				return Poll::Pending;
			};
			let timer = self.timer.get_or_insert_with(|| Box::pin(moq_clock::sleep_until(wake)));
			if timer.deadline() != wake {
				timer.as_mut().reset(wake);
			}
			if waiter.poll_future(timer.as_mut()).is_pending() {
				return Poll::Pending;
			}
		}
	}

	/// Count a frame the caller dropped after [`Self::poll_next`] released it. The first of its
	/// track is trimmed and leaves the track not started; a later one is a loss.
	pub fn discard(&mut self, ready: &Ready<K, T>) {
		if !ready.first {
			self.dropped += 1;
			return;
		}
		self.trimmed += 1;
		if let Some(track) = self.tracks.get_mut(&ready.track) {
			track.started = false;
		}
	}

	/// Count a frame of `key` the caller dropped before queuing it: a loss once the track
	/// started or when the caller queued frames `ahead` of it, else trimmed.
	pub fn lose<Q: Ord + ?Sized>(&mut self, key: &Q, ahead: bool)
	where
		K: Borrow<Q>,
	{
		match ahead || self.tracks.get(key).is_some_and(|track| track.started) {
			true => self.dropped += 1,
			false => self.trimmed += 1,
		}
	}

	/// The frame `key` releases next, if it has one queued.
	pub fn peek<Q: Ord + ?Sized>(&self, key: &Q) -> Option<&T>
	where
		K: Borrow<Q>,
	{
		let (_, _, _, item) = self.tracks.get(key)?.queue.front()?;
		Some(item)
	}

	/// Whether `key` has a frame waiting, queued or held for a clock still being acquired.
	pub fn holds<Q: Ord + ?Sized>(&self, key: &Q) -> bool
	where
		K: Borrow<Q>,
	{
		self.peek(key).is_some()
			|| self
				.acquire
				.as_ref()
				.is_some_and(|acquire| acquire.tracks.contains_key(key))
	}

	/// Forget `key`, which holds nothing and will push nothing more, so what it kept to stay in
	/// step goes with it.
	pub fn forget<Q: Ord + ?Sized>(&mut self, key: &Q)
	where
		K: Borrow<Q>,
	{
		debug_assert!(!self.holds(key), "forgot a track that holds frames");
		self.tracks.remove(key);
		self.expect.remove(key);
		self.leads.remove(key);
		self.steer.tracks.remove(key);
	}

	/// When the next queued frame is due, or the acquisition under way ends.
	#[cfg(test)]
	pub fn next_deadline(&self) -> Option<Instant> {
		self.next_available_deadline(None)
	}

	#[cfg(test)]
	fn next_available_deadline(&self, held: Option<&BTreeMap<K, Ready<K, T>>>) -> Option<Instant> {
		self.front_available(held)
			.map(|(deadline, _)| deadline)
			.into_iter()
			.chain(self.acquired_by())
			.min()
	}

	/// Whether `key` has a frame waiting that is already due (any frame, without a delay), so
	/// reading more of it now would only queue what the output cannot take yet.
	pub fn full<Q: Ord + ?Sized>(&self, key: &Q) -> bool
	where
		K: Borrow<Q>,
	{
		self.tracks
			.get(key)
			.and_then(|track| track.queue.front())
			.is_some_and(|(_, _, deadline, _)| self.delay.is_zero() || *deadline <= moq_clock::now())
	}

	/// How many frames are waiting.
	#[cfg(test)]
	pub fn len(&self) -> usize {
		let held = self.acquire.as_ref().map_or(0, |acquire| acquire.frames.len());
		held + self.tracks.values().map(|track| track.queue.len()).sum::<usize>()
	}

	/// Whether no frame is waiting.
	pub fn is_empty(&self) -> bool {
		self.acquire.is_none() && self.tracks.values().all(|track| track.queue.is_empty())
	}

	/// How many frames were dropped after their track's first went out: late, due before a new
	/// generation's acquisition ended, or lost with one of those, waiting for a sync frame after
	/// it or leading the sync frame the track resumed at.
	pub fn dropped(&self) -> u64 {
		self.dropped
	}

	/// How many frames were dropped before their track's first went out, so it starts on a sync
	/// frame: what a joiner is handed older than the delay, and what it held with them.
	pub fn trimmed(&self) -> u64 {
		self.trimmed
	}

	/// The source's clock rate off ours, as last measured, as a fraction: positive runs fast.
	pub fn drift(&self) -> Option<f64> {
		self.steer.source
	}

	/// How many times the source's clock was measured further off ours than the clock may
	/// follow.
	pub fn out_of_tolerance(&self) -> u64 {
		self.out_of_tolerance
	}

	/// The least time any frame arrived before its deadline in the last whole second of ours,
	/// in seconds: negative for a frame that missed it. `None` until a second has passed with
	/// a delay.
	pub fn margin(&self) -> Option<f64> {
		self.margin
	}

	/// How far the clock sits off the source's plus the delay, in seconds, as last measured:
	/// positive holds the track sent latest longer than the delay, negative shorter. The
	/// clock pulls it shut over [`RESPONSE`]. `None` until the clock has measured the source's
	/// rate, as [`Self::drift`].
	pub fn offset(&self) -> Option<f64> {
		self.steer.offset
	}
}

/// `nanos` after `at`, or before it if negative, if an instant holds it.
fn offset(at: Instant, nanos: i128) -> Option<Instant> {
	let span = Duration::from_nanos(u64::try_from(nanos.unsigned_abs()).ok()?);
	match nanos >= 0 {
		true => at.checked_add(span),
		false => at.checked_sub(span),
	}
}

/// How long after `from` the instant `to` is, in nanoseconds, negative if before.
fn since(from: Instant, to: Instant) -> i128 {
	match to >= from {
		true => (to - from).as_nanos() as i128,
		false => -((from - to).as_nanos() as i128),
	}
}

/// The upper envelope of `points`, sorted by time: the slope of its edge over their mean
/// time, and where that edge's line is at `now`. A point below the envelope, as queueing
/// leaves the floor, moves neither unless it lasts past the mean.
fn envelope(points: &VecDeque<(f64, f64)>, now: f64) -> Option<(f64, f64)> {
	let mut hull: Vec<(f64, f64)> = Vec::with_capacity(points.len());
	for &(x, y) in points {
		while let [.., (x1, y1), (x2, y2)] = hull[..]
			&& (y2 - y1) * (x - x1) <= (y - y1) * (x2 - x1)
		{
			hull.pop();
		}
		hull.push((x, y));
	}
	let mean = points.iter().map(|(x, _)| x).sum::<f64>() / points.len() as f64;
	let ((x1, y1), (x2, y2)) = hull
		.windows(2)
		.map(|edge| (edge[0], edge[1]))
		.find(|((x1, _), (x2, _))| *x1 <= mean && mean <= *x2 && x2 > x1)?;
	let slope = (y2 - y1) / (x2 - x1);
	Some((slope, y1 + slope * (now - x1)))
}

/// How far below the line of `slope` that is at `at` at `now` the median of `points` lies.
fn depth(points: &VecDeque<(f64, f64)>, slope: f64, at: f64, now: f64) -> f64 {
	let mut below: Vec<f64> = points.iter().map(|&(x, y)| at - slope * (now - x) - y).collect();
	below.sort_by(f64::total_cmp);
	below[below.len() / 2]
}

#[cfg(test)]
mod tests {
	use super::*;

	const DELAY: Duration = Duration::from_millis(100);

	/// A pending publisher cannot clobber active expected tracks or automatic clock jumps.
	#[moq_clock::test]
	async fn pending_keys_and_active_timeline_jumps_use_independent_clocks() {
		let start = moq_clock::now();
		let delay = Duration::from_millis(500);
		let mut buffer = Buffer::new(delay).replay();
		buffer.expect([0u16]);
		buffer.push(0, arrival(start, 0, "A0")).unwrap();
		buffer.start([1], [1]);
		buffer.push(1, arrival(start, 10_000, "B1")).unwrap();
		buffer.expect([0, 2]);
		buffer.expect_pending([1, 3], [1, 3]);
		buffer.push(2, arrival(start, 0, "A2")).unwrap();
		buffer.push(3, arrival(start, 10_000, "B3")).unwrap();
		moq_clock::sleep(delay).await;
		assert_eq!(due(&mut buffer), ["A0", "A2"]);
		let Poll::Ready(first) = buffer.poll_pending(&kio::Waiter::noop()) else {
			panic!("B ready");
		};
		assert_eq!((first.item, first.generation), ("B1", 0));
		buffer.hold_pending(first);
		assert!(buffer.full(&1));
		let Poll::Ready(next) = buffer.poll_pending(&kio::Waiter::noop()) else {
			panic!("B3 ready");
		};
		assert_eq!((next.item, next.generation), ("B3", 0));
		buffer
			.push(
				0,
				Arrival {
					restart: 1,
					..arrival(moq_clock::now(), 0, "A restart")
				},
			)
			.unwrap();
		buffer.push(3, arrival(moq_clock::now(), 10_020, "B next")).unwrap();
		moq_clock::sleep(Duration::from_millis(20)).await;
		let Poll::Ready(next) = buffer.poll_pending(&kio::Waiter::noop()) else {
			panic!("B clock kept its deadline");
		};
		assert_eq!((next.item, next.generation), ("B next", 0));
		moq_clock::sleep(delay - Duration::from_millis(20)).await;
		let Poll::Ready(next) = buffer.poll_next(&kio::Waiter::noop()) else {
			panic!("A restart ready");
		};
		assert_eq!((next.item, next.generation), ("A restart", 1));
	}

	/// A caller can hold B's first issued item, cancel before adoption, and resume A on its clock.
	#[moq_clock::test]
	async fn cancelling_an_issued_replacement_keeps_the_active_clock() {
		let start = moq_clock::now();
		let delay = Duration::from_millis(500);
		let mut buffer = Buffer::new(delay).replay();
		buffer.push(0u16, arrival(start, 0, "A0")).unwrap();
		buffer.push(0, arrival(start, 100, "A tail")).unwrap();
		buffer.lose(&0, true);
		buffer.start([1], [1]);
		buffer.push(1, arrival(start, 10_000, "B")).unwrap();
		buffer.lose(&1, true);
		buffer.lose(&1, true);
		moq_clock::sleep(delay).await;
		assert_eq!(due(&mut buffer), ["A0"]);
		let Poll::Ready(first) = buffer.poll_pending(&kio::Waiter::noop()) else {
			panic!("B ready");
		};
		buffer.commit(first).unwrap();
		// The commit drops A's tail.
		assert_eq!(buffer.reporting().dropped(), 2);
		moq_clock::sleep(Duration::from_millis(100)).await;
		let Poll::Ready(held) = buffer.poll_next(&kio::Waiter::noop()) else {
			panic!("B issued");
		};
		assert_eq!(held.item, "B");
		buffer.cancel();
		assert_eq!(buffer.reporting().dropped(), 2);
		assert_eq!(buffer.at(0, ms(200)), Some(start + delay + Duration::from_millis(200)));
		buffer.push(0, arrival(moq_clock::now(), 200, "A resumed")).unwrap();
		buffer.start([2], [2]);
		buffer.push(2, arrival(moq_clock::now(), 20_000, "C")).unwrap();
		moq_clock::sleep(Duration::from_millis(100)).await;
		assert_eq!(due(&mut buffer), ["A resumed"]);
		moq_clock::sleep(Duration::from_millis(400)).await;
		let Poll::Ready(first) = buffer.poll_pending(&kio::Waiter::noop()) else {
			panic!("C ready");
		};
		buffer.commit(first).unwrap();
		let Poll::Ready(next) = buffer.poll_next(&kio::Waiter::noop()) else {
			panic!("C retained");
		};
		assert_eq!((next.item, next.generation), ("C", 1));
		buffer.adopted();
		assert_eq!(buffer.reporting().dropped(), 0);
	}

	/// A cancel after commit resumes the old publisher at its next sync frame, since the frames
	/// after its cut tail may reference it.
	#[moq_clock::test]
	async fn a_cancelled_commit_resumes_the_old_publisher_at_a_sync_frame() {
		let start = moq_clock::now();
		let delay = Duration::from_millis(500);
		let mut buffer = Buffer::new(delay).replay();
		buffer.push(0u16, arrival(start, 0, "A0")).unwrap();
		buffer.push(0, arrival(start, 100, "A tail")).unwrap();
		buffer.start([1], [1]);
		buffer.push(1, arrival(start, 10_000, "B")).unwrap();
		moq_clock::sleep(delay).await;
		assert_eq!(due(&mut buffer), ["A0"]);
		let Poll::Ready(first) = buffer.poll_pending(&kio::Waiter::noop()) else {
			panic!("B ready");
		};
		buffer.commit(first).unwrap();
		buffer.cancel();
		assert_eq!(buffer.reporting().dropped(), 1);
		let delta = Arrival {
			sync: false,
			..arrival(moq_clock::now(), 200, "A delta")
		};
		assert_eq!(buffer.push(0, delta).unwrap(), Push::Waiting);
		assert_eq!(buffer.reporting().dropped(), 2);
		let key = arrival(moq_clock::now(), 300, "A key");
		assert_eq!(buffer.push(0, key).unwrap(), Push::Queued);
		moq_clock::sleep(delay).await;
		assert_eq!(due(&mut buffer), ["A key"]);
	}

	/// A restart the old publisher is still acquiring at commit is dropped with the old tail.
	#[moq_clock::test]
	async fn a_commit_drops_the_old_publishers_acquiring_restart() {
		let start = moq_clock::now();
		let delay = Duration::from_millis(500);
		let mut buffer = Buffer::new(delay);
		buffer.push(0u16, arrival(start, 0, "A0")).unwrap();
		moq_clock::sleep(delay).await;
		assert_eq!(released(&mut buffer), [("A0", 0)]);
		buffer.start([1], [1]);
		buffer.push(1, arrival(moq_clock::now(), 10_000, "B")).unwrap();
		moq_clock::sleep(Duration::from_millis(10)).await;
		buffer
			.push(0, after(1, arrival(moq_clock::now(), 5_000, "A restart")))
			.unwrap();
		moq_clock::sleep(delay - Duration::from_millis(10)).await;
		let Poll::Ready(first) = buffer.poll_pending(&kio::Waiter::noop()) else {
			panic!("B ready");
		};
		// A's acquisition has 10 ms left to run, so only the commit can settle it.
		buffer.commit(first).unwrap();
		assert_eq!(released(&mut buffer), [("B", 2)]);
		assert_eq!(buffer.reporting().dropped(), 1);
	}

	/// A replacement shifted behind what already went out fails past the clock's range.
	#[moq_clock::test]
	async fn a_handoff_past_the_clock_range_returns_overflow() {
		let start = moq_clock::now();
		let mut buffer = Buffer::new(DELAY).replay();
		// A's clock runs a millisecond behind B's, so B shifts behind A's last frame out.
		buffer
			.push(0u16, arrival(start + Duration::from_millis(1), 0, "A"))
			.unwrap();
		buffer.start([1], [1]);
		buffer.push(1, arrival(start, 0, "B first")).unwrap();
		let last = Instant::ZERO.checked_add(Duration::from_nanos(u64::MAX)).unwrap();
		let decode = Decode::from_nanos((last - start - DELAY).as_nanos() as i128);
		buffer
			.push(
				1,
				Arrival {
					decode,
					..arrival(start, 0, "B last")
				},
			)
			.unwrap();
		moq_clock::sleep(DELAY + Duration::from_millis(1)).await;
		assert_eq!(due(&mut buffer), ["A"]);
		let Poll::Ready(first) = buffer.poll_pending(&kio::Waiter::noop()) else {
			panic!("B ready");
		};
		assert_eq!(buffer.commit(first), Err(moq_net::TimeOverflow));
	}

	/// Each switch goes out at the delay its replacement's clock acquired, not after the old
	/// program's tail, so latency does not build up switch after switch, and every frame
	/// after a switch is the replacement's.
	#[moq_clock::test]
	async fn repeated_switches_keep_the_delay() {
		const TICK: Duration = Duration::from_millis(100);
		let start = moq_clock::now();
		let delay = Duration::from_millis(500);
		let mut buffer = Buffer::<u16, Instant>::new(delay).replay();
		// Publisher `key` stamps each frame `key * 100 s` past the time it sends it.
		let push = |buffer: &mut Buffer<u16, Instant>, key: u16| {
			let now = moq_clock::now();
			let frame = Arrival {
				arrived: now,
				read: now,
				decode: ms(u64::from(key) * 100_000 + (now - start).as_millis() as u64).into(),
				restart: 0,
				skip: 0,
				sync: true,
				leading: false,
				item: now,
			};
			assert_eq!(buffer.push(key, frame).unwrap(), Push::Queued);
		};
		// Every frame due now goes out the delay after it arrived, in generation order, and
		// all from `key`.
		let drain = |buffer: &mut Buffer<u16, Instant>, key: u16, generation: &mut u64| {
			let mut count = 0;
			while let Poll::Ready(ready) = buffer.poll_next(&kio::Waiter::noop()) {
				assert_eq!(ready.track, key, "an old frame after the switch");
				assert_eq!(moq_clock::now() - ready.item, delay);
				assert!(ready.generation >= *generation);
				*generation = ready.generation;
				count += 1;
			}
			count
		};
		let mut generation = 0;
		for _ in 0..20 {
			push(&mut buffer, 0);
			drain(&mut buffer, 0, &mut generation);
			moq_clock::sleep(TICK).await;
		}
		for key in 1..4u16 {
			buffer.start([key], [key]);
			let first = loop {
				push(&mut buffer, key - 1);
				push(&mut buffer, key);
				drain(&mut buffer, key - 1, &mut generation);
				if let Poll::Ready(first) = buffer.poll_pending(&kio::Waiter::noop()) {
					break first;
				}
				moq_clock::sleep(TICK).await;
			};
			let dropped = buffer.reporting().dropped();
			buffer.commit(first).unwrap();
			// The old tail still queued, the delay's worth read since the frame due now, is cut.
			assert_eq!(buffer.reporting().dropped() - dropped, 5);
			assert_eq!(drain(&mut buffer, key, &mut generation), 1);
			buffer.adopted();
			for _ in 0..20 {
				moq_clock::sleep(TICK).await;
				push(&mut buffer, key);
				drain(&mut buffer, key, &mut generation);
			}
		}
		assert_eq!(generation, 3);
	}

	#[moq_clock::test]
	async fn decode_before_zero_keeps_its_deadline_and_media_order() {
		let start = moq_clock::now();
		let mut buffer = Buffer::new(DELAY).replay();
		buffer.push(2, arrival(start, 0, "audio")).unwrap();
		let before_zero = Decode::from_nanos(-22_223);
		buffer
			.push(
				1,
				Arrival {
					decode: before_zero,
					..arrival(start, 0, "video")
				},
			)
			.unwrap();
		assert_eq!(
			buffer.at(0, before_zero),
			Some(start + DELAY - Duration::from_nanos(22_223))
		);
		assert_eq!(buffer.at(0, ms(0)), Some(start + DELAY));
		moq_clock::sleep(DELAY).await;
		assert_eq!(due(&mut buffer), ["video", "audio"]);
	}

	#[moq_clock::test]
	async fn moving_media_zero_preserves_acquisition_and_deadlines() {
		let start = moq_clock::now();
		let mut negative = Buffer::new(DELAY);
		let mut positive = Buffer::new(DELAY);
		for buffer in [&mut negative, &mut positive] {
			buffer.expect([1, 2]);
		}
		for (key, nanos, item) in [(1, -22_223, "video"), (2, 0, "audio"), (1, -11_112, "next")] {
			for (buffer, offset) in [(&mut negative, 0), (&mut positive, 1_000_000_000)] {
				buffer
					.push(
						key,
						Arrival {
							decode: Decode::from_nanos(nanos + offset),
							..arrival(start, 0, item)
						},
					)
					.unwrap();
			}
		}
		moq_clock::sleep(DELAY).await;
		assert_eq!(due(&mut negative), due(&mut positive));
		for nanos in [-22_223, -11_112, 0, 11_111] {
			assert_eq!(
				negative.at(0, Decode::from_nanos(nanos)),
				positive.at(0, Decode::from_nanos(nanos + 1_000_000_000)),
			);
		}
	}

	/// Two floors from the recovery arm's exact audio clock and wandering video lead.
	/// Both envelopes interpolate their two samples, so rounding in their equal depths
	/// must not make the changing send-ahead look like source clock drift.
	#[test]
	fn equal_depths_do_not_turn_a_track_lead_into_clock_drift() {
		let audio = VecDeque::from([(97.0, 9.44999008545876e-10), (98.32, 4.143163449267329e-10)]);
		let video = VecDeque::from([(98.726666, 0.6206660009629188), (99.004, 0.5390000015665989)]);
		let steer = Steer {
			phase: audio.clone(),
			tracks: BTreeMap::from([(1, audio), (2, video)]),
			..Steer::default()
		};
		let (rate, _) = steer.measure([1, 2].iter(), 100.284).unwrap();
		assert!(rate.abs() < MAX_DRIFT, "an exact clock measured {rate} off real time");
	}

	fn ms(ms: u64) -> Timestamp {
		Timestamp::from_millis(ms).unwrap()
	}

	fn arrival(arrived: Instant, decode: u64, item: &'static str) -> Arrival<&'static str> {
		Arrival {
			arrived,
			read: arrived,
			decode: ms(decode).into(),
			restart: 0,
			skip: 0,
			sync: true,
			leading: false,
			item,
		}
	}

	/// The same frame, read after its source restarted its timeline `restart` times.
	fn after(restart: u64, arrival: Arrival<&'static str>) -> Arrival<&'static str> {
		Arrival {
			restart,
			skip: restart,
			..arrival
		}
	}

	/// Every frame due by now, in order, with its generation.
	fn released(buffer: &mut Buffer<u16, &'static str>) -> Vec<(&'static str, u64)> {
		let waiter = kio::Waiter::noop();
		let mut out = Vec::new();
		while let Poll::Ready(ready) = buffer.poll_next(&waiter) {
			out.push((ready.item, ready.generation));
		}
		out
	}

	/// Every frame due by now, in order.
	fn due(buffer: &mut Buffer<u16, &'static str>) -> Vec<&'static str> {
		released(buffer).into_iter().map(|(item, _)| item).collect()
	}

	/// A fill reads each track's available backlog before acquisition selects a clock.
	#[tokio::test(start_paused = true)]
	async fn acquisition_observes_every_tracks_available_frames() {
		for order in [[1, 2], [2, 1]] {
			let start = moq_clock::now();
			let mut buffer = Buffer::new(Duration::from_millis(500));
			buffer.expect([1, 2]);
			// The held first frames reach the jitter buffer before the rest of the fill.
			for track in [1, 2] {
				buffer.push(track, arrival(start, 0, "first")).unwrap();
			}
			for track in order {
				// Video runs ahead of audio. Acquiring partway through either track
				// sees an incomplete backlog and can trim a different starting group.
				let end = if track == 1 { 1_200 } else { 800 };
				for decode in (40..=end).step_by(40) {
					assert!(
						!buffer.full(&track),
						"the fill must observe the whole backlog: {order:?}"
					);
					buffer
						.push(
							track,
							Arrival {
								sync: decode % 600 == 0,
								..arrival(start, decode, "backlog")
							},
						)
						.unwrap();
				}
			}
			assert!(due(&mut buffer).is_empty());
			assert_eq!(buffer.at(0, ms(800)), Some(start + Duration::from_millis(500)));
			tokio::time::advance(Duration::from_millis(500)).await;
			let first = buffer.poll_next(&kio::Waiter::noop());
			assert!(
				matches!(first, Poll::Ready(Ready { track: 1, .. })),
				"video begins on its 600 ms group"
			);
		}
	}

	#[tokio::test(start_paused = true)]
	async fn releases_at_the_delay_in_decode_order() {
		let start = moq_clock::now();
		let mut buffer = Buffer::new(DELAY).replay();
		assert_eq!(buffer.push(1, arrival(start, 0, "v0")).unwrap(), Push::Queued);
		assert_eq!(buffer.push(1, arrival(start, 40, "v40")).unwrap(), Push::Queued);
		// Audio arrives later than the video, with an earlier decode time.
		assert_eq!(
			buffer
				.push(2, arrival(start + Duration::from_millis(30), 20, "a20"))
				.unwrap(),
			Push::Queued
		);
		assert_eq!(
			buffer
				.push(2, arrival(start + Duration::from_millis(30), 40, "a40"))
				.unwrap(),
			Push::Queued
		);

		assert!(due(&mut buffer).is_empty(), "nothing is due before the delay");
		tokio::time::advance(DELAY).await;
		assert_eq!(due(&mut buffer), ["v0"]);
		tokio::time::advance(Duration::from_millis(20)).await;
		assert_eq!(due(&mut buffer), ["a20"]);
		tokio::time::advance(Duration::from_millis(20)).await;
		assert_eq!(due(&mut buffer), ["v40", "a40"], "a tie goes to the lower track");
		assert!(buffer.is_empty());
	}

	/// The margin is the least any frame had to spare over a whole second, a frame that missed
	/// its deadline included, and holds until the next second has passed.
	#[tokio::test(start_paused = true)]
	async fn the_margin_is_the_least_spare_over_a_second() {
		let start = Instant::now();
		let at = |ms: u64| start + Duration::from_millis(ms);
		let mut buffer = Buffer::new(DELAY).replay();
		assert_eq!(buffer.push(1, arrival(at(0), 0, "v0")).unwrap(), Push::Queued);
		assert_eq!(buffer.push(1, arrival(at(30), 40, "v40")).unwrap(), Push::Queued);
		// Due at 180 ms.
		assert_eq!(buffer.push(1, arrival(at(200), 80, "v80")).unwrap(), Push::Late);
		assert_eq!(buffer.margin(), None, "no second has passed");

		assert_eq!(
			buffer.push(1, arrival(at(1_000), 1_000, "v1000")).unwrap(),
			Push::Queued
		);
		let margin = buffer.margin().expect("a second has passed");
		assert!((margin + 0.020).abs() < 1e-6, "margin {margin} s");

		assert_eq!(
			buffer.push(1, arrival(at(1_500), 1_500, "v1500")).unwrap(),
			Push::Queued
		);
		assert!(buffer.margin().unwrap() < 0.0, "held until the next second has passed");
		assert_eq!(
			buffer.push(1, arrival(at(2_500), 2_500, "v2500")).unwrap(),
			Push::Queued
		);
		let margin = buffer.margin().unwrap();
		assert!((margin - 0.100).abs() < 1e-6, "the next second was clean: {margin} s");
	}

	/// Two buffers fed the same frames with different arrival skew, every frame inside
	/// its deadline, emit the same order.
	#[tokio::test(start_paused = true)]
	async fn arrival_skew_does_not_change_the_order() {
		let start = moq_clock::now();
		let (mut early, mut late) = (Buffer::new(DELAY).replay(), Buffer::new(DELAY).replay());
		let frames = [
			(1, 0, "v0"),
			(1, 40, "v40"),
			(1, 80, "v80"),
			(2, 0, "a0"),
			(2, 20, "a20"),
			(2, 60, "a60"),
		];
		for (key, decode, item) in frames {
			// One sees audio lag by 90ms, the other sees every frame on time.
			let skew = if key == 2 { 90 } else { 0 };
			assert_eq!(
				early
					.push(key, arrival(start + Duration::from_millis(decode), decode, item))
					.unwrap(),
				Push::Queued
			);
			let arrived = start + Duration::from_millis(decode + skew);
			assert_eq!(late.push(key, arrival(arrived, decode, item)).unwrap(), Push::Queued);
		}
		tokio::time::advance(Duration::from_secs(1)).await;
		let order = due(&mut early);
		assert_eq!(order, ["v0", "a0", "a20", "v40", "a60", "v80"]);
		assert_eq!(due(&mut late), order);
	}

	/// A late frame is dropped and counted, and the rest keep their order. A track that
	/// dropped a frame waits for its next sync frame.
	#[tokio::test(start_paused = true)]
	async fn late_frames_are_dropped_until_a_sync_frame() {
		let start = moq_clock::now();
		let mut buffer = Buffer::new(DELAY).replay();
		buffer.push(1, arrival(start, 0, "v0")).unwrap();
		buffer.push(2, arrival(start, 0, "a0")).unwrap();
		tokio::time::advance(DELAY).await;
		assert_eq!(due(&mut buffer), ["v0", "a0"]);
		let late = start + Duration::from_millis(200);
		let video = |decode, sync, item| Arrival {
			sync,
			..arrival(late, decode, item)
		};
		assert_eq!(buffer.push(1, video(40, false, "v40")).unwrap(), Push::Late);
		assert_eq!(buffer.push(1, video(200, false, "v200")).unwrap(), Push::Waiting);
		assert_eq!(buffer.push(1, video(240, true, "v240")).unwrap(), Push::Queued);
		assert_eq!(buffer.push(2, arrival(late, 180, "a180")).unwrap(), Push::Queued);
		assert_eq!((buffer.dropped(), buffer.trimmed()), (2, 0));

		tokio::time::advance(Duration::from_secs(1)).await;
		assert_eq!(due(&mut buffer), ["a180", "v240"]);
	}

	/// The sync frame a track resumes at after a drop loses its leading frames too, since
	/// they reference the dropped ones. The next sync frame's leading frames are kept.
	/// The track has started, so both are losses ([`Buffer::dropped`]).
	#[tokio::test(start_paused = true)]
	async fn a_resumed_sync_frame_drops_its_leading_frames() {
		let start = Instant::now();
		let mut buffer = Buffer::new(DELAY).replay();
		let video = |arrived, decode, sync, leading, item| Arrival {
			sync,
			leading,
			..arrival(arrived, decode, item)
		};
		buffer.push(1, video(start, 0, true, false, "v0")).unwrap();
		tokio::time::advance(DELAY).await;
		assert_eq!(due(&mut buffer), ["v0"]);
		let late = start + Duration::from_millis(200);
		assert_eq!(
			buffer.push(1, video(late, 40, false, false, "v40")).unwrap(),
			Push::Late
		);
		assert_eq!(
			buffer.push(1, video(late, 200, true, false, "v200")).unwrap(),
			Push::Queued
		);
		assert_eq!(
			buffer.push(1, video(late, 240, false, true, "v240")).unwrap(),
			Push::Waiting
		);
		assert_eq!(
			buffer.push(1, video(late, 280, false, false, "v280")).unwrap(),
			Push::Queued
		);
		assert_eq!(
			buffer.push(1, video(late, 320, true, false, "v320")).unwrap(),
			Push::Queued
		);
		assert_eq!(
			buffer.push(1, video(late, 360, false, true, "v360")).unwrap(),
			Push::Queued
		);
		assert_eq!(buffer.dropped(), 2, "the leading frame was lost with the late one");

		tokio::time::advance(Duration::from_secs(1)).await;
		assert_eq!(due(&mut buffer), ["v200", "v280", "v320", "v360"]);
	}

	/// A restart before anything went out keeps the clock, so the tracks stay on one.
	#[tokio::test(start_paused = true)]
	async fn a_skip_before_the_first_release_keeps_one_clock() {
		let start = moq_clock::now();
		let mut buffer = Buffer::new(DELAY).replay();
		// The video's first group is stale and anchors the clock; the audio is live.
		buffer.push(1, arrival(start, 0, "v0")).unwrap();
		buffer.push(2, arrival(start, 900, "a900")).unwrap();
		// The video consumer skips to the live group.
		let skip = start + Duration::from_millis(5);
		assert_eq!(
			buffer.push(1, after(1, arrival(skip, 900, "v900"))).unwrap(),
			Push::Queued
		);
		assert_eq!(
			buffer.push(1, after(1, arrival(skip, 940, "v940"))).unwrap(),
			Push::Queued
		);
		assert_eq!(buffer.push(2, arrival(skip, 940, "a940")).unwrap(), Push::Queued);

		tokio::time::advance(Duration::from_secs(2)).await;
		assert_eq!(
			released(&mut buffer),
			[("v0", 0), ("v900", 0), ("a900", 0), ("v940", 0), ("a940", 0)]
		);
	}

	/// A restart after frames went out opens a new generation, and every track's next frame
	/// runs on it, whether or not that track saw the restart.
	#[tokio::test(start_paused = true)]
	async fn every_track_joins_a_new_generation_at_its_next_frame() {
		let start = moq_clock::now();
		let mut buffer = Buffer::new(DELAY).replay();
		buffer.push(1, arrival(start, 0, "v0")).unwrap();
		buffer.push(2, arrival(start, 0, "a0")).unwrap();
		buffer.push(3, arrival(start, 0, "d0")).unwrap();
		tokio::time::advance(DELAY).await;
		assert_eq!(due(&mut buffer), ["v0", "a0", "d0"]);

		// The publisher resumes a second later with its timeline five seconds on.
		let resume = start + Duration::from_secs(1);
		tokio::time::advance(resume - moq_clock::now()).await;
		assert_eq!(
			buffer.push(1, after(1, arrival(resume, 5_000, "v5000"))).unwrap(),
			Push::Queued
		);
		// The audio crosses no restart of its own; the data crosses the same one.
		let late = resume + Duration::from_millis(10);
		assert_eq!(buffer.push(2, arrival(late, 5_000, "a5000")).unwrap(), Push::Queued);
		assert_eq!(
			buffer.push(3, after(1, arrival(late, 5_000, "d5000"))).unwrap(),
			Push::Queued
		);
		assert_eq!(buffer.push(2, arrival(late, 5_020, "a5020")).unwrap(), Push::Queued);

		tokio::time::advance(DELAY).await;
		assert_eq!(released(&mut buffer), [("v5000", 1), ("a5000", 1), ("d5000", 1)]);
		tokio::time::advance(Duration::from_millis(20)).await;
		assert_eq!(released(&mut buffer), [("a5020", 1)]);
	}

	/// Not replaying, the same: a track that crosses no restart of its own, sent behind the
	/// one that did, joins the generation being acquired once its frame is ahead of the old
	/// clock, rather than queuing seconds ahead on it and holding every track back. The new
	/// clock anchors on it, the track sent latest, so it keeps the delay.
	#[tokio::test(start_paused = true)]
	async fn a_track_without_the_restart_joins_the_acquisition() {
		let start = moq_clock::now();
		let mut buffer = Buffer::new(DELAY);
		buffer.expect([1, 2]);
		buffer.push(1, arrival(start, 0, "v0")).unwrap();
		buffer.push(2, arrival(start, 0, "a0")).unwrap();
		tokio::time::advance(DELAY).await;
		assert_eq!(due(&mut buffer), ["v0", "a0"]);

		// The publisher resumes a second later with its timeline five seconds on, and sends
		// the audio further behind the video than the delay.
		let resume = start + Duration::from_secs(1);
		tokio::time::advance(resume - moq_clock::now()).await;
		assert_eq!(
			buffer.push(1, after(1, arrival(resume, 5_000, "v5000"))).unwrap(),
			Push::Queued
		);
		let lag = resume + DELAY + Duration::from_millis(50);
		tokio::time::advance(lag - moq_clock::now()).await;
		assert_eq!(buffer.push(2, arrival(lag, 5_000, "a5000")).unwrap(), Push::Queued);
		tokio::time::advance(Duration::from_millis(20)).await;
		let next = moq_clock::now();
		assert_eq!(buffer.push(2, arrival(next, 5_020, "a5020")).unwrap(), Push::Queued);

		tokio::time::advance(lag + DELAY - moq_clock::now()).await;
		assert_eq!(released(&mut buffer), [("v5000", 1), ("a5000", 1)]);
		tokio::time::advance(Duration::from_millis(20)).await;
		assert_eq!(released(&mut buffer), [("a5020", 1)]);
		assert_eq!(buffer.dropped(), 0);
	}

	/// Two tracks every 100 ms of decode time for `steps` frames, the audio sent more than two
	/// delays behind the video and running `tail` frames past it, then a restart onto a
	/// timeline 15 seconds on: each released frame's generation, and how many were dropped.
	async fn cross(steps: u64, tail: u64) -> (Vec<u64>, u64) {
		const LAG: u64 = 300;
		let start = moq_clock::now();
		let mut buffer = Buffer::new(DELAY);
		buffer.expect([1, 2]);

		// (read, track, restarts, decode)
		let mut frames = Vec::new();
		for step in 0..steps {
			frames.push((step * 100, 1, 0, 1_000 + step * 100));
		}
		for step in 0..LAG / 100 + steps + tail {
			frames.push((step * 100, 2, 0, 1_000 - LAG + step * 100));
		}
		let restart = steps * 100;
		for step in 0..20 {
			let decode = 20_000 + step * 100;
			frames.push((restart + step * 100, 1, 1, decode));
			frames.push((restart + step * 100 + LAG, 2, 1, decode));
		}
		frames.sort();

		let mut out = Vec::new();
		for (read, key, restart, decode) in frames {
			let at = start + Duration::from_millis(read);
			tokio::time::advance(at - moq_clock::now()).await;
			out.extend(released(&mut buffer).into_iter().map(|(_, generation)| generation));
			buffer.push(key, after(restart, arrival(at, decode, "frame"))).unwrap();
		}
		tokio::time::advance(Duration::from_secs(2)).await;
		out.extend(released(&mut buffer).into_iter().map(|(_, generation)| generation));
		(out, buffer.dropped())
	}

	/// A track sent further behind than the new generation's acquisition lasts crosses the
	/// restart after the new clock is set. Four seconds in, the leads are measured, so the new
	/// clock keeps the lead the previous generation measured for it, its frames land there,
	/// and the frames it sends before crossing stay on the previous generation: nothing is
	/// dropped and no third generation opens.
	#[tokio::test(start_paused = true)]
	async fn a_track_crossing_after_the_new_clock_keeps_its_lead() {
		let (out, dropped) = cross(40, 0).await;
		assert_eq!(dropped, 0);
		assert!(out.is_sorted(), "a generation went out after the next: {out:?}");
		assert_eq!(out.iter().filter(|&&generation| generation == 0).count(), 83);
		assert_eq!(out.iter().filter(|&&generation| generation == 1).count(), 40);
		assert_eq!(out.len(), 123, "a third generation opened, or a frame never went out");
	}

	/// The same restart before any steering step, with the audio's old timeline running a frame
	/// past the video's: no lead covers it, so that frame, read after the new generation's
	/// first went out, is dropped rather than sent out of turn.
	#[tokio::test(start_paused = true)]
	async fn a_track_crossing_before_any_lead_keeps_the_generations_in_turn() {
		let (out, _) = cross(10, 1).await;
		assert!(out.is_sorted(), "a generation went out after the next: {out:?}");
	}

	/// A restart from 10 s onto a timeline at `to`, then, once the new generation's first frame
	/// went out, a frame of the old timeline from a track that crossed no restart: it lands on
	/// neither clock, so it drops as late without steering, and the track's next frame on the
	/// new timeline still joins it.
	async fn stray(to: u64) {
		let start = moq_clock::now();
		let mut buffer = Buffer::new(DELAY);
		buffer.expect([1]);
		buffer.push(1, arrival(start, 10_000, "v")).unwrap();
		buffer.push(2, arrival(start, 10_000, "d")).unwrap();
		tokio::time::advance(DELAY).await;
		assert_eq!(due(&mut buffer), ["v", "d"]);

		let resume = start + Duration::from_secs(1);
		tokio::time::advance(resume - moq_clock::now()).await;
		buffer.push(1, after(1, arrival(resume, to, "v'"))).unwrap();
		tokio::time::advance(DELAY).await;
		assert_eq!(released(&mut buffer), [("v'", 1)]);

		let now = moq_clock::now();
		assert_eq!(buffer.push(2, arrival(now, 10_040, "d")).unwrap(), Push::Late);
		assert!(buffer.is_empty(), "the old frame was queued on the new clock");
		assert!(buffer.steer.floor.is_none(), "the old frame was steered on");
		assert_eq!(buffer.push(2, arrival(now, to + 40, "d'")).unwrap(), Push::Queued);
		tokio::time::advance(DELAY).await;
		assert_eq!(released(&mut buffer), [("d'", 1)]);
	}

	#[tokio::test(start_paused = true)]
	async fn an_old_frame_after_a_forward_restart_is_not_steered_on() {
		stray(20_000).await;
	}

	#[tokio::test(start_paused = true)]
	async fn an_old_frame_after_a_backward_restart_is_dropped() {
		stray(0).await;
	}

	/// A skipped group whose timeline carried on keeps the clock: a frame it made late is
	/// dropped, and the next on-time frame goes out on the same generation.
	#[tokio::test(start_paused = true)]
	async fn a_skip_keeps_the_clock() {
		let start = moq_clock::now();
		let mut buffer = Buffer::new(DELAY);
		buffer.push(1, arrival(start, 0, "a0")).unwrap();
		tokio::time::advance(DELAY).await;
		assert_eq!(due(&mut buffer), ["a0"]);

		// Group 1 is skipped: frames from 100 ms on arrive after a 250 ms stall.
		let stalled = start + Duration::from_millis(350);
		tokio::time::advance(stalled - moq_clock::now()).await;
		let skipped = |decode, item| Arrival {
			skip: 1,
			..arrival(stalled, decode, item)
		};
		assert_eq!(buffer.push(1, skipped(200, "a200")).unwrap(), Push::Late);
		assert_eq!(buffer.push(1, skipped(300, "a300")).unwrap(), Push::Queued);
		tokio::time::advance(Duration::from_millis(100)).await;
		assert_eq!(released(&mut buffer), [("a300", 0)]);
	}

	/// A skip after which a frame would be held more than a delay past everything queued
	/// followed a timeline that leapt ahead, as when the skipped groups held a marker the
	/// publisher shed, so it opens a generation.
	#[tokio::test(start_paused = true)]
	async fn a_skip_that_leaps_ahead_opens_a_generation() {
		let start = moq_clock::now();
		let mut buffer = Buffer::new(DELAY);
		buffer.push(1, arrival(start, 0, "a0")).unwrap();
		tokio::time::advance(DELAY).await;
		assert_eq!(due(&mut buffer), ["a0"]);

		let now = moq_clock::now();
		let leapt = Arrival {
			skip: 1,
			..arrival(now, 5_000, "a5000")
		};
		assert_eq!(buffer.push(1, leapt).unwrap(), Push::Queued);
		tokio::time::advance(DELAY).await;
		assert_eq!(released(&mut buffer), [("a5000", 1)]);
	}

	/// A track sent ahead of the others is held its lead past the delay. Once a skip of its own
	/// leaves only theirs queued, its next frame is due that lead past them, and keeps the clock.
	#[tokio::test(start_paused = true)]
	async fn a_track_sent_ahead_resuming_after_a_skip_keeps_the_clock() {
		const LEAD: u64 = 300;
		let start = moq_clock::now();
		let mut buffer = Buffer::new(DELAY);
		buffer.expect([1, 2]);
		let mut out = Vec::new();
		for at in (0..12_000).step_by(40) {
			tokio::time::advance(start + Duration::from_millis(at) - moq_clock::now()).await;
			let now = moq_clock::now();
			assert_eq!(buffer.push(1, arrival(now, at, "v")).unwrap(), Push::Queued);
			// Track 2 is sent LEAD ahead, and skips a second of its own.
			let decode = at + LEAD;
			if !(8_000..9_000).contains(&decode) {
				let skip = u64::from(decode >= 9_000);
				let frame = Arrival {
					skip,
					..arrival(now, decode, "a")
				};
				assert_eq!(buffer.push(2, frame).unwrap(), Push::Queued, "at {at} ms");
			}
			out.extend(released(&mut buffer));
		}
		assert!(out.len() > 500, "too little went out: {}", out.len());
		assert!(
			out.iter().all(|&(_, generation)| generation == 0),
			"a generation opened"
		);
		assert_eq!(buffer.dropped(), 0);
	}

	/// Every frame of a generation goes out before the next generation's, even one the new
	/// clock puts earlier.
	#[tokio::test(start_paused = true)]
	async fn generations_go_out_in_turn() {
		let start = moq_clock::now();
		let mut buffer = Buffer::new(DELAY).replay();
		buffer.push(1, arrival(start, 0, "v0")).unwrap();
		buffer.push(1, arrival(start, 40, "v40")).unwrap();
		buffer.push(2, arrival(start, 20, "a20")).unwrap();
		tokio::time::advance(DELAY).await;
		assert_eq!(due(&mut buffer), ["v0"]);

		// A backlog across a jump in the timeline, read in one go.
		let now = moq_clock::now();
		buffer.push(1, after(1, arrival(now, 5_000, "v5000"))).unwrap();
		buffer.push(2, arrival(now, 4_900, "a4900")).unwrap();
		buffer.push(2, arrival(now, 5_020, "a5020")).unwrap();
		tokio::time::advance(Duration::from_secs(1)).await;
		assert_eq!(
			released(&mut buffer),
			[("a20", 0), ("v40", 0), ("a4900", 1), ("v5000", 1), ("a5020", 1)]
		);
	}

	/// What a run against a drifting source saw.
	struct Run {
		/// The error the run ended on, if it failed.
		err: Option<anyhow::Error>,
		/// The largest gap between a frame's slack and the delay, in seconds, from `settled`
		/// hours on.
		worst: f64,
		/// The source's rate the buffer measured at the end, and before `step` began.
		drift: Option<f64>,
		before: Option<f64>,
		/// The clock's drift at the end, and before `step` began.
		clock: f64,
		clock_before: f64,
		out_of_tolerance: u64,
		/// How far off the delay the frame the run failed on would have been held, in seconds.
		off: f64,
		/// The buffer's margin and offset at the end.
		margin: Option<f64>,
		offset: Option<f64>,
	}

	/// Feed `hours` of 2 fps frames from a source whose clock runs `ppm` slower than ours
	/// through a 500 ms buffer, each frame queued `queueing(hour)` extra on its way, checking
	/// that no frame goes late and the clock stays within what 13818-1 allows, until a push
	/// fails.
	async fn drift(ppm: f64, hours: f64, settled: f64, queueing: impl Fn(f64) -> Duration) -> Run {
		let delay = Duration::from_millis(500);
		let start = moq_clock::now();
		let clock = moq_clock::Deadlines::new(start);
		let _entered = clock.enter();
		let mut buffer = Buffer::new(delay);
		let mut run = Run {
			err: None,
			worst: 0.0,
			drift: None,
			before: None,
			clock: 0.0,
			clock_before: 0.0,
			out_of_tolerance: 0,
			off: 0.0,
			margin: None,
			offset: None,
		};
		let mut last: Option<(f64, Decode)> = None;
		let mut stepped = false;
		for k in 0..(hours * 3_600.0 * 2.0) as u64 {
			let decode = Duration::from_millis(k * 500);
			let hour = decode.as_secs_f64() / 3_600.0;
			let queued = queueing(hour);
			stepped |= !queued.is_zero();
			if !stepped {
				run.before = buffer.drift();
				run.clock_before = buffer.clock.map_or(0.0, |clock| clock.drift);
			}
			let source = decode.as_secs_f64() * (1.0 + ppm * 1e-6);
			let arrived = start + Duration::from_secs_f64(source) + queued;
			clock.advance(arrived.max(clock.now()));
			let frame = Arrival {
				arrived,
				read: arrived,
				decode: Timestamp::from_micros(decode.as_micros() as u64).unwrap().into(),
				restart: 0,
				skip: 0,
				sync: true,
				leading: false,
				item: "v",
			};
			let held = buffer
				.at(0, frame.decode)
				.map(|deadline| since(arrived, deadline) as f64 / 1e9);
			match buffer.push(1, frame) {
				Ok(push) => assert_eq!(push, Push::Queued, "{ppm} ppm: frame {k} was late"),
				Err(err) => {
					run.off = held.expect("a clock") - delay.as_secs_f64();
					run.err = Some(err);
					break;
				}
			}
			if hour >= settled
				&& queued.is_zero()
				&& let Some(deadline) = buffer.at(0, Timestamp::from_micros(decode.as_micros() as u64).unwrap())
			{
				let slack = since(arrived, deadline) as f64 / 1e9;
				run.worst = run.worst.max((slack - delay.as_secs_f64()).abs());
			}
			due(&mut buffer);

			let Some(clock) = buffer.clock else {
				continue;
			};
			assert!(clock.drift.abs() <= MAX_DRIFT, "{ppm} ppm: drift {}", clock.drift);
			if let Some((drift, base)) = last {
				let elapsed = (clock.base.as_nanos() as f64 - base.as_nanos() as f64) / 1e9;
				assert!(
					(clock.drift - drift).abs() <= MAX_SLEW * elapsed.max(0.0) * (1.0 + 1e-9),
					"{ppm} ppm: drift slewed from {drift} to {} in {elapsed} s",
					clock.drift
				);
			}
			last = Some((clock.drift, clock.base));
		}
		assert_eq!(buffer.dropped(), 0, "{ppm} ppm: frames went late");
		run.drift = buffer.drift();
		run.clock = buffer.clock.map_or(0.0, |clock| clock.drift);
		run.out_of_tolerance = buffer.out_of_tolerance();
		run.margin = buffer.margin();
		run.offset = buffer.offset();
		run
	}

	/// A source whose clock runs off ours by up to the 30 ppm 13818-1 allows loses no frame
	/// over a day, and settles at its rate. Within it the freshest frames are held the delay to
	/// 10 ms once converged; at the limit the clock cannot run past the source's to win back
	/// what reaching its rate under the slew limit cost, 30 ppm squared over twice the slew
	/// (162 ms), so it holds there.
	#[tokio::test(start_paused = true)]
	async fn the_clock_follows_a_drifting_source_for_a_day() {
		for (ppm, bound) in [(30.0, 0.170), (-30.0, 0.170), (25.0, 0.010), (-25.0, 0.010)] {
			let run = drift(ppm, 24.0, 16.0, |_| Duration::ZERO).await;
			assert!(run.err.is_none(), "{ppm} ppm: {:?}", run.err);
			assert!(run.worst <= bound, "{ppm} ppm: held {} s off the delay", run.worst);
			// One track queued nowhere: every frame's spare is its slack, the delay and the offset.
			let offset = run.offset.expect("an offset");
			assert!(offset.abs() <= bound, "{ppm} ppm: offset {offset} s");
			let margin = run.margin.expect("a margin");
			assert!(
				(margin - 0.5 - offset).abs() < 0.001,
				"{ppm} ppm: margin {margin} s, offset {offset} s"
			);
			let drift = run.drift.expect("a measurement") * 1e6;
			assert!((drift + ppm).abs() < 0.1, "{ppm} ppm: measured {drift} ppm");
			assert!(
				(run.clock * 1e6 - ppm).abs() < 0.5,
				"{ppm} ppm: the clock settled at {}",
				run.clock * 1e6
			);
			assert_eq!(run.out_of_tolerance, 0, "{ppm} ppm");
		}
	}

	/// A spell of queueing lowers the floor only while it lasts, so four minutes of 200 ms
	/// more moves neither the measured rate nor the clock.
	#[tokio::test(start_paused = true)]
	async fn a_step_in_queueing_does_not_move_the_estimate() {
		let spell = 6.0..6.0 + 4.0 / 60.0;
		let run = drift(10.0, 6.5, 6.5, |hour| match spell.contains(&hour) {
			true => Duration::from_millis(200),
			false => Duration::ZERO,
		})
		.await;
		assert!(run.err.is_none(), "{:?}", run.err);
		let (before, after) = (run.before.unwrap() * 1e6, run.drift.unwrap() * 1e6);
		assert!(
			(after - before).abs() < 0.01,
			"the estimate moved from {before} to {after} ppm"
		);
		let (before, after) = (run.clock_before * 1e6, run.clock * 1e6);
		assert!(
			(after - before).abs() < 0.01,
			"the clock moved from {before} to {after} ppm"
		);
	}

	/// A source further off than the clock may follow is counted, and fails once it has used
	/// half the delay, before any frame goes late, though the clock has not reached its limit.
	#[tokio::test(start_paused = true)]
	async fn a_source_past_the_drift_limit_fails() {
		for ppm in [40.0, -40.0, 400.0, -400.0] {
			let run = drift(ppm, 6.0, 6.0, |_| Duration::ZERO).await;
			assert!(run.err.is_some(), "{ppm} ppm: no error");
			assert!(run.out_of_tolerance > 0, "{ppm} ppm: not counted");
			assert!(
				(0.25..0.26).contains(&run.off.abs()),
				"{ppm} ppm: failed {} s off the delay",
				run.off
			);
		}
	}

	/// What [`feed`] saw.
	struct Fed {
		/// The decode time a push failed at, in seconds, and its error.
		err: Option<(f64, anyhow::Error)>,
		dropped: u64,
		/// What the last push did.
		last: Option<Push>,
		/// The source's rate the buffer measured at the end, and how often it was out of tolerance.
		drift: Option<f64>,
		out_of_tolerance: u64,
		/// The rate measured after each push, by decode time in seconds.
		rates: Vec<(f64, f64)>,
	}

	/// The buffer [`feed`] runs a source through: its delay and the tracks it carries.
	struct Shape {
		delay: Duration,
		tracks: &'static [u16],
	}

	/// Two tracks through a 500 ms buffer.
	const PAIR: Shape = Shape {
		delay: Duration::from_millis(500),
		tracks: &[1, 2],
	};

	/// `just test ts --full`'s export: a video and two audio tracks through a 900 ms buffer.
	const FULL: Shape = Shape {
		delay: Duration::from_millis(900),
		tracks: &[1, 2, 3],
	};

	/// Feed `secs` of 10 fps frames of `shape`'s tracks through its buffer from a source whose
	/// clock runs `ppm` slower than ours, the frame of `track` sent at `sent` seconds of our
	/// clock reaching us at `arrive(track, sent)`, or with the frame of its track before it if
	/// that is later, or never if `None`, in the order they reach us, until a push fails.
	async fn feed(shape: &Shape, ppm: f64, secs: f64, arrive: impl Fn(u16, f64) -> Option<f64>) -> Fed {
		let start = moq_clock::now();
		let clock = moq_clock::Deadlines::new(start);
		let _entered = clock.enter();
		let mut buffer = Buffer::new(shape.delay);
		let mut frames = Vec::new();
		for &track in shape.tracks {
			let mut last = 0.0f64;
			for k in 0..(secs * 10.0) as u64 {
				let decode = Duration::from_millis(k * 100);
				if let Some(sent) = arrive(track, decode.as_secs_f64() * (1.0 + ppm * 1e-6)) {
					last = sent.max(last);
					frames.push((last, track, decode));
				}
			}
		}
		frames.sort_by(|a, b| a.0.total_cmp(&b.0));
		buffer.expect(frames.iter().map(|(_, track, _)| *track));
		let mut fed = Fed {
			err: None,
			dropped: 0,
			last: None,
			drift: None,
			out_of_tolerance: 0,
			rates: Vec::new(),
		};
		for (sent, track, decode) in frames {
			let arrived = start + Duration::from_secs_f64(sent);
			clock.advance(arrived.max(clock.now()));
			let frame = Arrival {
				arrived,
				read: arrived,
				decode: Timestamp::from_micros(decode.as_micros() as u64).unwrap().into(),
				restart: 0,
				skip: 0,
				sync: true,
				leading: false,
				item: "v",
			};
			match buffer.push(track, frame) {
				Ok(push) => fed.last = Some(push),
				Err(err) => {
					fed.err = Some((decode.as_secs_f64(), err));
					break;
				}
			}
			if let Some(rate) = buffer.drift() {
				fed.rates.push((decode.as_secs_f64(), rate));
			}
			due(&mut buffer);
		}
		fed.dropped = buffer.dropped();
		fed.drift = buffer.drift();
		fed.out_of_tolerance = buffer.out_of_tolerance();
		fed
	}

	/// A source far slower than real time, as an importer starved of CPU, fails within a few
	/// seconds, naming its rate, rather than drop nearly every frame for hours.
	#[tokio::test(start_paused = true)]
	async fn a_source_far_off_real_time_fails_within_seconds() {
		let fed = feed(&PAIR, 220_000.0, 3_600.0, |track, sent| (track == 1).then_some(sent)).await;
		let (at, err) = fed.err.expect("no error");
		assert!(at < 10.0, "failed {at} s in");
		let err = err.to_string();
		assert!(err.contains("-220000.0 ppm"), "the error names the wrong rate: {err}");
	}

	/// A source on our clock whose latency moves by under half the delay while the export
	/// starts, queueing that builds or a backlog that drains over 5 to 20 s from the first
	/// frame on or once the first rates are measured, and then holds, never fails or drops a
	/// frame through `shape`'s buffer. Its first track (the video) is sent `lead(sent)` ahead
	/// and the rest as they decode.
	async fn start_up_latency_never_fails(shape: &Shape, lead: fn(f64) -> f64) {
		for draining in [false, true] {
			for depth in [0.3, 0.4] {
				for ramp in [5.0, 10.0, 20.0] {
					for from in [0.0, 3.0, 6.0] {
						let fed = feed(shape, 0.0, from + ramp + 120.0, |track, sent| {
							let built = depth * ((sent - from) / ramp).clamp(0.0, 1.0);
							let latency = if draining { depth - built } else { built };
							let lead = if track == 1 { lead(sent) } else { 0.0 };
							Some(sent + latency - lead)
						})
						.await;
						let case = match draining {
							true => format!("{depth} s draining over {ramp} s from {from} s"),
							false => format!("{depth} s building over {ramp} s from {from} s"),
						};
						assert!(fed.err.is_none(), "{case}: {:?}", fed.err);
						assert_eq!(fed.dropped, 0, "{case}");
					}
				}
			}
		}
	}

	/// Read at now, the envelope edge a move in latency ran along goes on past the floors that
	/// hold after it, and would charge the rate for slack no frame lost or gained.
	#[tokio::test(start_paused = true)]
	async fn start_up_latency_never_fails_one_track() {
		start_up_latency_never_fails(&Shape { tracks: &[1], ..FULL }, |_| 0.0).await;
	}

	/// The same on `just test ts --full`'s shape: video sent up to 0.65 s ahead, wandering
	/// with how full the encoder's buffer is, beside two audio tracks sent as they decode.
	#[tokio::test(start_paused = true)]
	async fn start_up_latency_never_fails_a_full_multiplex() {
		start_up_latency_never_fails(&FULL, |sent| 0.5 + 0.15 * (sent * std::f64::consts::TAU / 7.0).sin()).await;
	}

	/// A stall shorter than half [`RATE_WINDOW`] drops what it made late and recovers, even
	/// early, when only a few floors were measured before it.
	#[tokio::test(start_paused = true)]
	async fn a_stall_drops_late_frames_and_recovers() {
		for at in [5.0, 9.0, 1_200.0] {
			for length in [3.0, 10.0, 60.0, 290.0] {
				let stall = at..at + length;
				let fed = feed(&PAIR, 0.0, at + length + 30.0, |track, sent| {
					(track == 1).then_some(if stall.contains(&sent) { stall.end } else { sent })
				})
				.await;
				assert!(fed.err.is_none(), "{length} s stall at {at} s: {:?}", fed.err);
				assert!(fed.dropped > 0, "{length} s stall at {at} s: nothing dropped");
				assert_eq!(fed.last, Some(Push::Queued), "{length} s stall at {at} s");
			}
		}
	}

	/// What [`unmarked`] saw.
	struct Stepped {
		err: Option<anyhow::Error>,
		dropped: u64,
		/// Each frame's generation, in the order they went out.
		out: Vec<u64>,
		/// The clock's drift just before the step, and at the end.
		before: f64,
		after: f64,
	}

	/// Feed `secs` of 10 fps frames of `shape`'s tracks through its buffer from a source on our
	/// clock, each sent as it decodes with a few ms of jitter that changes every steering step,
	/// whose timeline steps by `step` seconds at `at` s with no restart or skip marker.
	async fn unmarked(shape: &Shape, at: f64, step: f64, secs: f64) -> Stepped {
		// Where the timeline starts, so a step back stays on it.
		const BASE: f64 = 20_000.0;
		let start = moq_clock::now();
		let clock = moq_clock::Deadlines::new(start);
		let _entered = clock.enter();
		let mut buffer = Buffer::new(shape.delay);
		buffer.expect(shape.tracks.iter().copied());
		let mut stepped = Stepped {
			err: None,
			dropped: 0,
			out: Vec::new(),
			before: 0.0,
			after: 0.0,
		};
		for k in 0..(secs * 10.0) as u64 {
			let sent = k as f64 / 10.0;
			let jitter = ((k / 20) * 3) % 5;
			let arrived = start + Duration::from_secs_f64(sent + jitter as f64 * 0.002);
			clock.advance(arrived.max(clock.now()));
			if sent < at {
				stepped.before = buffer.clock.map_or(0.0, |clock| clock.drift);
			}
			let decode = BASE + sent + if sent >= at { step } else { 0.0 };
			for &track in shape.tracks {
				let frame = Arrival {
					arrived,
					read: arrived,
					decode: Timestamp::from_micros((decode * 1e6).round() as u64).unwrap().into(),
					restart: 0,
					skip: 0,
					sync: true,
					leading: false,
					item: "v",
				};
				if let Err(err) = buffer.push(track, frame) {
					stepped.err = Some(err);
					return stepped;
				}
			}
			stepped
				.out
				.extend(released(&mut buffer).into_iter().map(|(_, generation)| generation));
		}
		stepped.dropped = buffer.dropped();
		stepped.after = buffer.clock.map_or(0.0, |clock| clock.drift);
		stepped
	}

	/// A timeline that steps with no marker, as a changeover between two encoders whose clocks
	/// started hours apart, by more than the delay absorbs, either way, before the rate window
	/// fills or once it has, re-acquires the clock instead of reading the step as drift: one
	/// new generation, nothing dropped, every frame out, and the step never swings the drift
	/// further than the slew limit allows over the time that passed.
	#[tokio::test(start_paused = true)]
	async fn an_unmarked_step_reacquires_the_clock() {
		for at in [30.0, 1_200.0] {
			for step in [16_844.5, -16_844.5, 2.0, -2.0] {
				let secs = at + 60.0;
				let run = unmarked(&PAIR, at, step, secs).await;
				let case = format!("{step:+} s step at {at} s");
				assert!(run.err.is_none(), "{case}: {:?}", run.err);
				assert_eq!(run.dropped, 0, "{case}");
				assert!(run.out.is_sorted(), "{case}: a generation went out after the next");
				assert_eq!(run.out.last(), Some(&1), "{case}: no single new generation");
				// What is still held at the end goes out within a delay.
				let held = (PAIR.delay.as_secs_f64() * 10.0).ceil() as usize + 1;
				let pushed = (secs * 10.0) as usize * PAIR.tracks.len();
				assert!(
					run.out.len() + held * PAIR.tracks.len() >= pushed,
					"{case}: {} of {pushed} went out",
					run.out.len()
				);
				let swing = (run.after - run.before).abs();
				assert!(
					swing <= MAX_SLEW * (secs - at) * (1.0 + 1e-9),
					"{case}: the drift swung {} ppm",
					swing * 1e6
				);
			}
		}
	}

	/// A source within what the clock may follow never fails, though its first frames reached
	/// us late, so the clock was anchored that much late, and each frame jitters a few ms, so
	/// the first floors, a few ms apart over two seconds, measure a rate far past the limit.
	#[tokio::test(start_paused = true)]
	async fn a_late_start_within_the_drift_limit_never_fails() {
		for ppm in [-20.0, 0.0, 20.0] {
			for late in [0.3, 0.45] {
				for phase in 0..5u64 {
					let fed = feed(&PAIR, ppm, 600.0, |track, sent| {
						let jitter = ((sent / 2.0) as u64 * 3 + phase) % 5;
						let late = if sent < 1.0 { late } else { 0.0 };
						(track == 1).then_some(sent + late + jitter as f64 * 0.002)
					})
					.await;
					assert!(
						fed.err.is_none(),
						"{ppm} ppm, {late} s late, phase {phase}: {:?}",
						fed.err
					);
				}
			}
		}
	}

	/// Closing a late start is the clock's own steering, not what the rate cost: hours after
	/// the clock pulled a 450 ms late start shut, a source within the limit does not fail on
	/// a rise in latency long enough to move the measured rate past the limit.
	#[tokio::test(start_paused = true)]
	async fn closing_a_late_start_is_not_charged_to_the_rate() {
		let rise = 14_000.0..14_600.0;
		let fed = feed(&PAIR, 0.0, 16_000.0, |track, sent| {
			let late = match sent {
				_ if sent < 1.0 => 0.45,
				_ if rise.contains(&sent) => 0.04,
				_ => 0.0,
			};
			(track == 1).then_some(sent + late)
		})
		.await;
		assert!(fed.err.is_none(), "{:?}", fed.err);
	}

	/// The track sent latest stopping, as audio sent 700 ms after its video ending first, keeps
	/// the clock: each floor is still measured against that track, by how much earlier than it
	/// the others are sent, rather than leap 700 ms, which would read as a source far faster
	/// than ours. That holds when its last frames were queued 400 ms more on their way, since
	/// a spell of queueing does not move the lead.
	#[tokio::test(start_paused = true)]
	async fn the_track_sent_latest_stopping_keeps_the_clock() {
		for queued in [0.0, 0.4] {
			let fed = feed(&PAIR, 0.0, 300.0, |track, sent| match track {
				1 => Some(sent),
				_ if sent < 56.0 => Some(sent + 0.7),
				_ => (sent < 60.0).then_some(sent + 0.7 + queued),
			})
			.await;
			assert!(fed.err.is_none(), "{queued} s queued: {:?}", fed.err);
			assert_eq!(fed.dropped, 0, "{queued} s queued");
		}
	}

	/// The track sent latest, sent ahead by a lead that wanders as a TS mux's video does with
	/// how full the encoder's buffer is, here by 200 ms over 20 minutes, does not move the rate:
	/// it is measured on the track sent as it decodes, whose slack keeps to its envelope, so a
	/// source within the limit is never out of tolerance and the estimate settles at its rate.
	#[tokio::test(start_paused = true)]
	async fn a_wandering_track_sent_latest_does_not_move_the_rate() {
		for ppm in [-20.0, 0.0, 20.0] {
			let fed = feed(&PAIR, ppm, 3_600.0, |track, sent| match track {
				1 => Some(sent + 0.2 - 0.1 * (sent * std::f64::consts::TAU / 1_200.0).cos()),
				_ => Some(sent),
			})
			.await;
			assert!(fed.err.is_none(), "{ppm} ppm: {:?}", fed.err);
			assert_eq!(fed.dropped, 0, "{ppm} ppm");
			assert_eq!(fed.out_of_tolerance, 0, "{ppm} ppm");
			let drift = fed.drift.expect("a measurement") * 1e6;
			assert!((drift + ppm).abs() < 0.5, "{ppm} ppm: measured {drift} ppm");
		}
	}

	/// A spell of queueing on the steady track, four minutes of 200 ms more, does not hand the
	/// rate to the wandering one: a track's swing is the median of how far its slack lies under
	/// its envelope, which a spell shorter than half the window does not move.
	#[tokio::test(start_paused = true)]
	async fn a_spell_on_the_steady_track_keeps_the_rate_on_it() {
		let spell = 1_500.0..1_740.0;
		for ppm in [-20.0, 0.0, 20.0] {
			let fed = feed(&PAIR, ppm, 2_400.0, |track, sent| match track {
				1 => Some(sent + 0.2 - 0.1 * (sent * std::f64::consts::TAU / 900.0).cos()),
				_ => Some(sent + if spell.contains(&sent) { 0.2 } else { 0.0 }),
			})
			.await;
			assert!(fed.err.is_none(), "{ppm} ppm: {:?}", fed.err);
			let worst = fed
				.rates
				.iter()
				.filter(|(at, _)| *at > RATE_WINDOW + 60.0)
				.map(|(_, rate)| (rate * 1e6 + ppm).abs())
				.fold(0.0, f64::max);
			assert!(worst < 0.5, "{ppm} ppm: measured up to {worst} ppm off");
		}
	}

	/// A joiner is handed its group at once. The clock is anchored on the freshest frame, so
	/// what is older than the delay is dropped without counting as late, and the next group
	/// goes out a delay after it arrived.
	#[tokio::test(start_paused = true)]
	async fn a_joiner_anchors_on_its_freshest_frame() {
		let start = moq_clock::now();
		let mut buffer = Buffer::new(DELAY);
		let video = |arrived, decode, sync, item| Arrival {
			sync,
			..arrival(arrived, decode, item)
		};
		// Joined 200 ms into a group, twice the delay.
		buffer.push(1, video(start, 0, true, "v0")).unwrap();
		for (decode, item) in [(40, "v40"), (80, "v80"), (120, "v120"), (160, "v160"), (200, "v200")] {
			buffer.push(1, video(start, decode, false, item)).unwrap();
		}
		tokio::time::advance(Duration::from_millis(40)).await;
		let next = start + Duration::from_millis(40);
		buffer.push(1, video(next, 240, true, "v240")).unwrap();
		buffer.push(1, video(next, 280, false, "v280")).unwrap();
		assert!(due(&mut buffer).is_empty());
		// Both frames were available together: v280, not its group's v240, is freshest.
		assert_eq!(buffer.next_deadline(), Some(start + DELAY));
		tokio::time::advance(Duration::from_millis(60)).await;
		assert_eq!(due(&mut buffer), ["v240"]);
		tokio::time::advance(Duration::from_millis(40)).await;
		assert_eq!(due(&mut buffer), ["v280"]);
		assert_eq!(buffer.dropped(), 0, "trimming the join is not a loss");
		assert_eq!(buffer.trimmed(), 6);
	}

	/// A start group its source cuts short before anything of it went out still goes out, from
	/// its sync frame, ahead of the hole: a path narrower than the media cuts every group, so a
	/// track waiting for a whole one might never start. Nothing is trimmed.
	#[tokio::test(start_paused = true)]
	async fn a_start_group_cut_short_goes_out() {
		for replay in [false, true] {
			let mut buffer = Buffer::new(DELAY);
			if replay {
				buffer = buffer.replay();
			}
			let video = |decode, sync, skip, item| Arrival {
				sync,
				skip,
				..arrival(moq_clock::now(), decode, item)
			};
			buffer.push(1, video(0, true, 0, "v0")).unwrap();
			tokio::time::advance(Duration::from_millis(40)).await;
			buffer.push(1, video(40, false, 0, "v40")).unwrap();
			// The rest of the group never arrives: the source cuts it short.
			tokio::time::advance(Duration::from_millis(80)).await;
			buffer.push(1, video(120, true, 1, "v120")).unwrap();
			tokio::time::advance(2 * DELAY).await;
			assert_eq!(due(&mut buffer), ["v0", "v40", "v120"], "replay {replay}");
			assert_eq!((buffer.dropped(), buffer.trimmed()), (0, 0), "replay {replay}");
		}
	}

	/// The keyframe a joiner holds while the export starts is read early and passes, while the
	/// rest of its group, read later, is older than the delay. The keyframe goes with them,
	/// rather than out alone ahead of a hole.
	#[tokio::test(start_paused = true)]
	async fn a_join_drops_its_group_whole() {
		let start = Instant::now();
		let mut buffer = Buffer::new(DELAY);
		let video = |arrived, decode, sync, item| Arrival {
			sync,
			..arrival(arrived, decode, item)
		};
		buffer.push(1, video(start, 0, true, "v0")).unwrap();
		tokio::time::advance(Duration::from_millis(250)).await;
		let later = start + Duration::from_millis(250);
		for (decode, sync, item) in [(40, false, "v40"), (80, false, "v80"), (400, true, "v400")] {
			buffer.push(1, video(later, decode, sync, item)).unwrap();
		}
		tokio::time::advance(Duration::from_secs(1)).await;
		assert_eq!(due(&mut buffer), ["v400"]);
		assert_eq!((buffer.dropped(), buffer.trimmed()), (0, 3));
	}

	/// A source that sends one track well ahead of its decode time and another just in time
	/// (a TS mux's video and audio) has the clock anchored on the one sent latest, so both
	/// keep the delay: anchored on the video, every audio frame would be late.
	#[tokio::test(start_paused = true)]
	async fn a_joiner_anchors_on_the_track_sent_latest() {
		let start = moq_clock::now();
		let mut buffer = Buffer::new(DELAY);
		buffer.expect([1, 2]);
		// Video is sent 300 ms ahead of its decode time, audio as it decodes.
		buffer.push(1, arrival(start, 300, "v300")).unwrap();
		tokio::time::advance(Duration::from_millis(20)).await;
		buffer
			.push(2, arrival(start + Duration::from_millis(20), 20, "a20"))
			.unwrap();
		tokio::time::advance(Duration::from_millis(100)).await;
		assert_eq!(due(&mut buffer), ["a20"], "the audio keeps the delay");
		assert_eq!(buffer.next_deadline(), Some(start + Duration::from_millis(400)));
		assert_eq!(buffer.dropped(), 0);
	}

	/// The acquisition waits to hear from every track it expects, since the silent one may be
	/// the one sent latest, but two delays at most.
	#[tokio::test(start_paused = true)]
	async fn the_acquisition_waits_two_delays_at_most_for_a_silent_track() {
		let start = moq_clock::now();
		let mut buffer = Buffer::new(DELAY);
		buffer.expect([1, 2]);
		buffer.push(1, arrival(start, 0, "v0")).unwrap();
		assert_eq!(buffer.next_deadline(), Some(start + 2 * DELAY));
		tokio::time::advance(2 * DELAY).await;
		assert_eq!(due(&mut buffer), ["v0"]);
	}

	/// Frames of one decode time go out by track, even when a steering step between their
	/// pushes put their deadlines a nanosecond apart: the order is the media's, not the
	/// moment each was read.
	#[tokio::test(start_paused = true)]
	async fn a_steering_step_does_not_reorder_a_tie() {
		let start = moq_clock::now();
		let order = |first: (u16, &'static str), second: (u16, &'static str)| {
			let mut buffer = Buffer::new(DELAY).replay();
			buffer.push(1, arrival(start, 0, "v0")).unwrap();
			buffer.push(first.0, arrival(start, 100, first.1)).unwrap();
			buffer.clock.as_mut().unwrap().anchor += Duration::from_nanos(1);
			buffer.push(second.0, arrival(start, 100, second.1)).unwrap();
			buffer
		};
		let mut video_first = order((1, "v100"), (2, "a100"));
		let mut audio_first = order((2, "a100"), (1, "v100"));
		tokio::time::advance(Duration::from_secs(1)).await;
		assert_eq!(due(&mut video_first), ["v0", "v100", "a100"]);
		assert_eq!(due(&mut audio_first), ["v0", "v100", "a100"]);
	}

	/// A frame the acquisition holds that is still in time goes out on time: the
	/// acquisition ends when the first of them falls due rather than drop it.
	#[tokio::test(start_paused = true)]
	async fn the_acquisition_ends_before_a_held_frame_is_due() {
		let start = moq_clock::now();
		let mut buffer = Buffer::new(DELAY);
		// Joined 60 ms into a group: its keyframe is due 40 ms from now.
		let key = Arrival {
			sync: true,
			..arrival(start, 0, "v0")
		};
		buffer.push(1, key).unwrap();
		buffer
			.push(
				1,
				Arrival {
					sync: false,
					..arrival(start, 60, "v60")
				},
			)
			.unwrap();
		assert_eq!(buffer.next_deadline(), Some(start + Duration::from_millis(40)));
		tokio::time::advance(Duration::from_millis(40)).await;
		assert_eq!(due(&mut buffer), ["v0"]);
	}

	/// Zero holds nothing: frames read together go out at once in decode order, and a
	/// frame behind the clock still goes out.
	#[tokio::test(start_paused = true)]
	async fn zero_delay_releases_on_arrival() {
		let start = moq_clock::now();
		let mut buffer = Buffer::new(Duration::ZERO);
		buffer.push(1, arrival(start, 0, "v0")).unwrap();
		buffer.push(1, arrival(start, 40, "v40")).unwrap();
		buffer.push(2, arrival(start, 20, "a20")).unwrap();
		assert_eq!(due(&mut buffer), ["v0", "a20", "v40"]);

		let late = start + Duration::from_secs(1);
		assert_eq!(buffer.push(2, arrival(late, 40, "a40")).unwrap(), Push::Queued);
		assert_eq!(due(&mut buffer), ["a40"]);
		assert_eq!(buffer.dropped(), 0);
	}

	/// A track is full once a frame of it is due, or holds any frame without a delay, so its
	/// reader leaves the rest in the source.
	#[tokio::test(start_paused = true)]
	async fn a_track_is_full_once_a_frame_is_due() {
		let start = moq_clock::now();
		let mut buffer = Buffer::new(DELAY).replay();
		assert!(!buffer.full(&1));
		buffer.push(1, arrival(start, 0, "v0")).unwrap();
		assert!(!buffer.full(&1), "a frame not yet due leaves room to read ahead");
		tokio::time::advance(DELAY).await;
		assert!(buffer.full(&1));
		assert!(!buffer.full(&2), "another track's due frame does not stop this one");
		assert_eq!(due(&mut buffer), ["v0"]);
		assert!(!buffer.full(&1));

		let mut buffer = Buffer::new(Duration::ZERO);
		buffer.push(1, arrival(start, 0, "v0")).unwrap();
		assert!(buffer.full(&1));
	}
}
