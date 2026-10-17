//! A jitter buffer that releases several tracks' frames in one decode order, each a fixed
//! delay after its decode time.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::pin::Pin;
use std::task::Poll;
use std::time::Duration;

use moq_net::Timestamp;
use web_async::time::{Instant, Sleep};

/// A frame handed to [`Buffer::push`].
pub(crate) struct Arrival<T> {
	/// The earliest the frame could have arrived, which its deadline is judged on, so a
	/// caller that reads late does not make it late.
	pub arrived: Instant,
	/// When it was read: the latest it could have arrived, which its freshness is judged on,
	/// so frames read together do not look fresher than they are.
	pub read: Instant,
	/// When the frame decodes, nondecreasing within a track.
	pub decode: Timestamp,
	/// How many times the source had restarted its timeline when the frame was read.
	pub restart: u64,
	/// How many times the source's playhead had jumped (a skipped group, or a restart)
	/// when the frame was read.
	pub skip: u64,
	/// How many times the source had lost the rest of a group it had begun delivering when the
	/// frame was read: the skips that cut a group short.
	pub cut: u64,
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
/// A track starts on a whole group. Until a frame of it has gone out, a loss (a frame
/// dropped here, or the rest of a group its source cut short) takes everything the track
/// holds with it, and the track starts at its next sync frame. Whole groups missing after one
/// that ended take nothing: a joiner whose zero-budget live-edge pick has the
/// publisher cut its group short starts at the next group rather than on the cut-off start.
/// What goes before a track's first frame out is [`Buffer::trimmed`], not lost; what drops
/// after it is [`Buffer::dropped`].
///
/// A skipped group leaves the timeline where it was, so it keeps the clock: frames it made
/// late are dropped like any other. A source that restarts its timeline (a declared
/// marker) has moved it, so once frames have gone out the restart opens a new generation,
/// whose clock is acquired the same way, though never ahead of a deadline already given
/// out. So does a skip after which a frame would be held more than a delay past everything
/// queued: its timeline jumped ahead, as when the skipped groups held a marker the
/// publisher shed. How far ahead of its decode time the source sends each track belongs to
/// its mux and survives a restart, so the new clock does not wait to hear from a track the
/// previous one measured: it is anchored late enough that the track sent latest keeps the
/// delay when it gets there. A track that has
/// not reached the restart stays on the previous generation while its frames land there and
/// go out before the new generation's first frame; one that would follow it drops as late.
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
pub(crate) struct Buffer<K, T> {
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
	/// Whether a frame has gone out since the clock started.
	released: bool,
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
}

/// How often the clock is steered, in our time: long enough that the floor covers a
/// group's worth of frames sent ahead by different amounts.
const STEER: Duration = Duration::from_secs(2);
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

/// The decode timeline's mapping onto ours: where a frame decoding at `base` is due, less the
/// delay, and how much longer than the decode timeline ours runs.
#[derive(Clone, Copy)]
struct Clock {
	generation: u64,
	anchor: Instant,
	base: Timestamp,
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
}

impl<K: Ord, T> Acquire<K, T> {
	/// The frame to anchor on: each track's freshest frame (the one read least behind its
	/// decode time) is that track's slack less its queueing, and of those, the one with the least
	/// slack, so the track sent latest against its decode time still has the delay.
	fn fresh(&self) -> (&K, &Arrival<T>) {
		let behind = |frame: &Arrival<T>| since(self.since, frame.read) - frame.decode.as_nanos() as i128;
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
			let ahead = frame.decode.as_nanos() as i128 - fresh.decode.as_nanos() as i128;
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
			startup: None,
		}
	}
}

impl<K: Ord> Steer<K> {
	/// The source's rate and the floor's phase now. The rate is the slope of the envelope of
	/// whichever track in `heard`, of those heard in at least half the floors, lies closest
	/// under its own at the median, so a spell of queueing on it shorter than half the window
	/// does not hand the rate to another; the phase is the floors' envelope at that slope.
	fn measure<'a>(&self, heard: impl Iterator<Item = &'a K>, now: f64) -> Option<(f64, f64)>
	where
		K: 'a,
	{
		let steadiest = heard
			.filter_map(|key| self.tracks.get(key))
			.filter(|series| 2 * series.len() >= self.phase.len())
			.filter_map(|series| {
				let (slope, at) = envelope(series, now)?;
				Some((depth(series, slope, at, now), slope))
			})
			.min_by(|a, b| a.0.total_cmp(&b.0))
			.map(|(_, slope)| slope);
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
	queue: VecDeque<(u64, Timestamp, Instant, T)>,
	/// The source's restart, skip and cut counters at the last frame.
	restart: u64,
	skip: u64,
	cut: u64,
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

impl<K: Ord + Clone, T> Buffer<K, T> {
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
			released: false,
			steer: Steer::default(),
			leads: BTreeMap::new(),
			timer: None,
			dropped: 0,
			trimmed: 0,
			out_of_tolerance: 0,
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
			.filter(|_| !held && !(self.released && self.jumps(&key, &arrival)))
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
			&& !self.lands(deadline, arrival.arrived)
		{
			let generation = track.generation;
			return Ok(self.queue(key, arrival, generation, None, false));
		}
		if self.released && !self.delay.is_zero() {
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
			&& self.lands(deadline, arrival.arrived)
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
	/// a frame held more than a delay past everything queued after a skip, or while a
	/// generation is acquired. The latter is a track reaching the new timeline without a
	/// marker of its own, which belongs to that generation, not far ahead on the old clock.
	fn jumps(&self, key: &K, arrival: &Arrival<T>) -> bool {
		let (Some(clock), Some(track)) = (self.clock, self.tracks.get(key)) else {
			return false;
		};
		let deadline = self.deadline(&clock, arrival.decode);
		let restarted = track.restart != arrival.restart
			&& (track.generation == clock.generation || !self.lands(deadline, arrival.arrived));
		let leapt = (track.skip != arrival.skip || self.acquire.is_some())
			&& deadline.is_some_and(|deadline| deadline > self.bound(arrival.arrived));
		restarted || leapt
	}

	/// Hold a frame for the generation being acquired, starting one if none is, and anchor it
	/// once a track starts a new group. Without a delay, or replaying, it anchors at once.
	fn hold(&mut self, key: K, arrival: Arrival<T>) {
		let generation = self.clock.map_or(0, |clock| clock.generation + 1);
		let acquire = self.acquire.get_or_insert_with(|| Acquire {
			generation,
			since: arrival.read,
			frames: Vec::new(),
			tracks: BTreeMap::new(),
		});
		let mid = acquire.tracks.entry(key.clone()).or_default();
		let grouped = arrival.sync && *mid;
		*mid |= !arrival.sync;
		let now = arrival.read;
		acquire.frames.push((key, arrival));
		let acquire = self.acquire.as_ref().expect("just held");
		let heard = self.heard(acquire);
		if (grouped && heard) || self.replay || self.delay.is_zero() || self.ends(acquire) <= now {
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
	/// Until a frame of the track has gone out, a loss (a frame dropped here, or the rest of a
	/// group its source cut short) takes everything the track holds with it, so its output starts on a
	/// whole group at its next sync frame rather than on a group's cut-off start.
	fn queue(&mut self, key: K, arrival: Arrival<T>, generation: u64, deadline: Option<Instant>, stale: bool) -> Push {
		let track = self.tracks.entry(key).or_insert_with(|| Track {
			queue: VecDeque::new(),
			restart: arrival.restart,
			skip: arrival.skip,
			cut: arrival.cut,
			generation,
			waiting: false,
			resumed: false,
			started: false,
		});
		// The rest of the group the track holds will never arrive.
		if arrival.cut != track.cut && !track.started {
			self.trimmed += track.queue.len() as u64;
			track.queue.clear();
		}
		track.restart = arrival.restart;
		track.skip = arrival.skip;
		track.cut = arrival.cut;
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

	/// Whether a frame arriving at `arrived` and due at `deadline` is on the clock: not late,
	/// and held no more than a delay past both its arrival and everything already queued.
	fn lands(&self, deadline: Option<Instant>, arrived: Instant) -> bool {
		deadline.is_some_and(|deadline| arrived <= deadline && deadline <= self.bound(arrived))
	}

	/// The latest a frame arriving at `arrived` may be due and still be on the clock: a delay
	/// past both its arrival and everything already queued.
	fn bound(&self, arrived: Instant) -> Instant {
		arrived.max(self.horizon.unwrap_or(arrived)) + self.delay
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

	/// When a frame decoding at `decode` goes out on the current clock, once there is one.
	pub fn at(&self, decode: Timestamp) -> Option<Instant> {
		self.deadline(self.clock.as_ref()?, decode)
	}

	/// When a frame decoding at `decode` goes out on `clock`, if any instant holds it.
	fn deadline(&self, clock: &Clock, decode: Timestamp) -> Option<Instant> {
		// Signed, since a frame can decode before the one that anchored the clock.
		let since = decode.as_nanos() as i128 - clock.base.as_nanos() as i128;
		offset(
			clock.anchor,
			since + (since as f64 * clock.drift) as i128 + self.delay.as_nanos() as i128,
		)
	}

	/// The deadline and track of the frame that goes out next: generation by generation, then
	/// in decode order, ties by track. Not in deadline order: two frames of one decode time
	/// queued either side of a steering step can be due nanoseconds apart, which would make
	/// the order depend on when each was read rather than on the media.
	fn front(&self) -> Option<(Instant, &K)> {
		self.tracks
			.iter()
			.filter_map(|(key, track)| {
				let (generation, decode, deadline, _) = track.queue.front()?;
				Some(((*generation, decode.as_nanos(), key), *deadline))
			})
			.min_by(|a, b| a.0.cmp(&b.0))
			.map(|((_, _, key), deadline)| (deadline, key))
	}

	/// When the acquisition under way ends, unless a track starts a new group first.
	fn acquired_by(&self) -> Option<Instant> {
		self.acquire.as_ref().map(|acquire| self.ends(acquire))
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
		loop {
			let now = Instant::now();
			let acquired = self.acquired_by();
			if acquired.is_some_and(|end| now >= end) {
				self.acquired();
				continue;
			}
			let front = self.front().map(|(deadline, key)| (deadline, key.clone()));
			if let Some((deadline, key)) = &front
				&& (self.delay.is_zero() || now >= *deadline)
			{
				let track = self.tracks.get_mut(key).expect("the front's track");
				let (generation, _, _, item) = track.queue.pop_front().expect("the front frame");
				let first = !std::mem::replace(&mut track.started, true);
				self.released = true;
				// A previous-generation frame queued now would go out after this one.
				if self.clock.is_some_and(|clock| clock.generation == generation) {
					self.previous = None;
				}
				return Poll::Ready(Ready {
					track: key.clone(),
					generation,
					item,
					first,
				});
			}
			let Some(wake) = front.map(|(deadline, _)| deadline).into_iter().chain(acquired).min() else {
				self.timer = None;
				return Poll::Pending;
			};
			let timer = self
				.timer
				.get_or_insert_with(|| Box::pin(web_async::time::sleep_until(wake)));
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
	pub fn lose(&mut self, key: &K, ahead: bool) {
		match ahead || self.tracks.get(key).is_some_and(|track| track.started) {
			true => self.dropped += 1,
			false => self.trimmed += 1,
		}
	}

	/// The frame `key` releases next, if it has one queued.
	pub fn peek(&self, key: &K) -> Option<&T> {
		let (_, _, _, item) = self.tracks.get(key)?.queue.front()?;
		Some(item)
	}

	/// When the next queued frame is due, or the acquisition under way ends.
	#[cfg(test)]
	pub fn next_deadline(&self) -> Option<Instant> {
		self.front()
			.map(|(deadline, _)| deadline)
			.into_iter()
			.chain(self.acquired_by())
			.min()
	}

	/// Whether `key` has a frame waiting that is already due (any frame, without a delay), so
	/// reading more of it now would only queue what the output cannot take yet.
	pub fn full(&self, key: &K) -> bool {
		self.tracks
			.get(key)
			.and_then(|track| track.queue.front())
			.is_some_and(|(_, _, deadline, _)| self.delay.is_zero() || *deadline <= Instant::now())
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

	/// How many frames were dropped before their track's first went out, so it starts on a whole
	/// group: what a joiner is handed older than the delay, and a start group a loss cut short.
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

	fn ms(ms: u64) -> Timestamp {
		Timestamp::from_millis(ms).unwrap()
	}

	fn arrival(arrived: Instant, decode: u64, item: &'static str) -> Arrival<&'static str> {
		Arrival {
			arrived,
			read: arrived,
			decode: ms(decode),
			restart: 0,
			skip: 0,
			cut: 0,
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

	#[tokio::test(start_paused = true)]
	async fn releases_at_the_delay_in_decode_order() {
		let start = Instant::now();
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

	/// Two buffers fed the same frames with different arrival skew, every frame inside
	/// its deadline, emit the same order.
	#[tokio::test(start_paused = true)]
	async fn arrival_skew_does_not_change_the_order() {
		let start = Instant::now();
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
		let start = Instant::now();
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
		let start = Instant::now();
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
		let start = Instant::now();
		let mut buffer = Buffer::new(DELAY).replay();
		buffer.push(1, arrival(start, 0, "v0")).unwrap();
		buffer.push(2, arrival(start, 0, "a0")).unwrap();
		buffer.push(3, arrival(start, 0, "d0")).unwrap();
		tokio::time::advance(DELAY).await;
		assert_eq!(due(&mut buffer), ["v0", "a0", "d0"]);

		// The publisher resumes a second later with its timeline five seconds on.
		let resume = start + Duration::from_secs(1);
		tokio::time::advance(resume - Instant::now()).await;
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
		let start = Instant::now();
		let mut buffer = Buffer::new(DELAY);
		buffer.expect([1, 2]);
		buffer.push(1, arrival(start, 0, "v0")).unwrap();
		buffer.push(2, arrival(start, 0, "a0")).unwrap();
		tokio::time::advance(DELAY).await;
		assert_eq!(due(&mut buffer), ["v0", "a0"]);

		// The publisher resumes a second later with its timeline five seconds on, and sends
		// the audio further behind the video than the delay.
		let resume = start + Duration::from_secs(1);
		tokio::time::advance(resume - Instant::now()).await;
		assert_eq!(
			buffer.push(1, after(1, arrival(resume, 5_000, "v5000"))).unwrap(),
			Push::Queued
		);
		let lag = resume + DELAY + Duration::from_millis(50);
		tokio::time::advance(lag - Instant::now()).await;
		assert_eq!(buffer.push(2, arrival(lag, 5_000, "a5000")).unwrap(), Push::Queued);
		tokio::time::advance(Duration::from_millis(20)).await;
		let next = Instant::now();
		assert_eq!(buffer.push(2, arrival(next, 5_020, "a5020")).unwrap(), Push::Queued);

		tokio::time::advance(lag + DELAY - Instant::now()).await;
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
		let start = Instant::now();
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
			tokio::time::advance(at - Instant::now()).await;
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
		let start = Instant::now();
		let mut buffer = Buffer::new(DELAY);
		buffer.expect([1]);
		buffer.push(1, arrival(start, 10_000, "v")).unwrap();
		buffer.push(2, arrival(start, 10_000, "d")).unwrap();
		tokio::time::advance(DELAY).await;
		assert_eq!(due(&mut buffer), ["v", "d"]);

		let resume = start + Duration::from_secs(1);
		tokio::time::advance(resume - Instant::now()).await;
		buffer.push(1, after(1, arrival(resume, to, "v'"))).unwrap();
		tokio::time::advance(DELAY).await;
		assert_eq!(released(&mut buffer), [("v'", 1)]);

		let now = Instant::now();
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
		let start = Instant::now();
		let mut buffer = Buffer::new(DELAY);
		buffer.push(1, arrival(start, 0, "a0")).unwrap();
		tokio::time::advance(DELAY).await;
		assert_eq!(due(&mut buffer), ["a0"]);

		// Group 1 is skipped: frames from 100 ms on arrive after a 250 ms stall.
		let stalled = start + Duration::from_millis(350);
		tokio::time::advance(stalled - Instant::now()).await;
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
		let start = Instant::now();
		let mut buffer = Buffer::new(DELAY);
		buffer.push(1, arrival(start, 0, "a0")).unwrap();
		tokio::time::advance(DELAY).await;
		assert_eq!(due(&mut buffer), ["a0"]);

		let now = Instant::now();
		let leapt = Arrival {
			skip: 1,
			..arrival(now, 5_000, "a5000")
		};
		assert_eq!(buffer.push(1, leapt).unwrap(), Push::Queued);
		tokio::time::advance(DELAY).await;
		assert_eq!(released(&mut buffer), [("a5000", 1)]);
	}

	/// Every frame of a generation goes out before the next generation's, even one the new
	/// clock puts earlier.
	#[tokio::test(start_paused = true)]
	async fn generations_go_out_in_turn() {
		let start = Instant::now();
		let mut buffer = Buffer::new(DELAY).replay();
		buffer.push(1, arrival(start, 0, "v0")).unwrap();
		buffer.push(1, arrival(start, 40, "v40")).unwrap();
		buffer.push(2, arrival(start, 20, "a20")).unwrap();
		tokio::time::advance(DELAY).await;
		assert_eq!(due(&mut buffer), ["v0"]);

		// A backlog across a jump in the timeline, read in one go.
		let now = Instant::now();
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
	}

	/// Feed `hours` of 2 fps frames from a source whose clock runs `ppm` slower than ours
	/// through a 500 ms buffer, each frame queued `queueing(hour)` extra on its way, checking
	/// that no frame goes late and the clock stays within what 13818-1 allows, until a push
	/// fails.
	async fn drift(ppm: f64, hours: f64, settled: f64, queueing: impl Fn(f64) -> Duration) -> Run {
		let delay = Duration::from_millis(500);
		let start = Instant::now();
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
		};
		let mut last: Option<(f64, Timestamp)> = None;
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
			tokio::time::advance(arrived.saturating_duration_since(Instant::now())).await;
			let frame = Arrival {
				arrived,
				read: arrived,
				decode: Timestamp::from_micros(decode.as_micros() as u64).unwrap(),
				restart: 0,
				skip: 0,
				cut: 0,
				sync: true,
				leading: false,
				item: "v",
			};
			let held = buffer
				.at(frame.decode)
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
				&& let Some(deadline) = buffer.at(Timestamp::from_micros(decode.as_micros() as u64).unwrap())
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
		let start = Instant::now();
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
			tokio::time::advance(arrived.saturating_duration_since(Instant::now())).await;
			let frame = Arrival {
				arrived,
				read: arrived,
				decode: Timestamp::from_micros(decode.as_micros() as u64).unwrap(),
				restart: 0,
				skip: 0,
				cut: 0,
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
		let start = Instant::now();
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
		assert_eq!(buffer.next_deadline(), Some(next + DELAY));
		tokio::time::advance(DELAY).await;
		assert_eq!(due(&mut buffer), ["v240"]);
		assert_eq!(buffer.dropped(), 0, "trimming the join is not a loss");
		assert_eq!(buffer.trimmed(), 6);
	}

	/// A start group its source cuts short before anything of it went out is dropped whole,
	/// so the track starts on the next group rather than on the cut-off start. Whole groups
	/// missing after one that ended take nothing with them.
	#[tokio::test(start_paused = true)]
	async fn a_start_group_cut_by_a_gap_is_dropped_whole() {
		let start = Instant::now();
		let mut buffer = Buffer::new(DELAY).replay();
		let video = |decode, sync, skip, cut, item| Arrival {
			sync,
			skip,
			cut,
			..arrival(start, decode, item)
		};
		buffer.push(1, video(0, true, 0, 0, "v0")).unwrap();
		buffer.push(1, video(40, false, 0, 0, "v40")).unwrap();
		// The rest of the group never arrives: the source cuts it short.
		assert_eq!(buffer.push(1, video(120, true, 1, 1, "v120")).unwrap(), Push::Queued);
		buffer.push(1, video(160, false, 1, 1, "v160")).unwrap();
		// A whole group is missing after that one ended.
		assert_eq!(buffer.push(1, video(280, true, 2, 1, "v280")).unwrap(), Push::Queued);
		tokio::time::advance(Duration::from_secs(1)).await;
		assert_eq!(due(&mut buffer), ["v120", "v160", "v280"]);
		assert_eq!((buffer.dropped(), buffer.trimmed()), (0, 2));

		// Once a frame went out, a cut is a loss in the output, and the rest still goes out.
		buffer.push(1, video(320, false, 2, 1, "v320")).unwrap();
		buffer.push(1, video(400, true, 3, 2, "v400")).unwrap();
		tokio::time::advance(Duration::from_secs(1)).await;
		assert_eq!(due(&mut buffer), ["v320", "v400"]);
		assert_eq!((buffer.dropped(), buffer.trimmed()), (0, 2));
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
		let start = Instant::now();
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
		let start = Instant::now();
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
		let start = Instant::now();
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
		let start = Instant::now();
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
		let start = Instant::now();
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
		let start = Instant::now();
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
