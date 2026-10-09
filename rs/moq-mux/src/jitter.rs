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
	/// Whether the frame decodes without the track's earlier frames.
	pub sync: bool,
	pub item: T,
}

/// A frame [`Buffer::poll_next`] let go.
pub(crate) struct Ready<K, T> {
	pub track: K,
	/// Counts up each time a restart moved the timeline, and the clock with it. Every
	/// frame of one generation goes out before any frame of the next.
	pub generation: u64,
	pub item: T,
}

/// What [`Buffer::push`] did with a frame.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Push {
	Queued,
	/// It missed its deadline and was dropped; the track resumes at its next sync frame.
	Late,
	/// It was dropped because the track is waiting for a sync frame.
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
/// makes due before the acquisition ended is dropped instead, without counting as late.
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
/// frames of the track sent latest a delay. Each [`STEER`] of decode time gives a floor, the
/// most slack a frame of that track arrived with (the least across tracks of each one's
/// most), which queueing and retransmission only ever lower; the upper envelope of
/// the floors over [`RATE_WINDOW`] gives the rate (its slope) and the slack now, so a
/// spell of queueing shorter than half the window moves neither. The decode timeline is
/// the output's system clock, so it keeps to what ISO/IEC 13818-1 2.4.2.1 allows one:
/// within [`MAX_DRIFT`] of ours, changing by at most [`MAX_SLEW`]. A source past that is
/// counted, and once it has used half the delay [`Buffer::push`] fails.
///
/// A zero delay holds nothing and drops nothing: each frame goes out as soon as it is
/// read, ordered only among the frames read together.
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
	/// Each track's slack in the last steering step, in seconds, against the others'. How far
	/// ahead of its decode time a source sends each track belongs to its mux, not to one
	/// generation, so a new generation keeps the track sent latest at the delay even before
	/// that track reaches it.
	leads: BTreeMap<K, f64>,
	timer: Option<Pin<Box<Sleep>>>,
	dropped: u64,
	/// Steps at which the source's clock ran further off ours than the clock may follow.
	out_of_tolerance: u64,
}

/// How often the clock is steered, in decode time: long enough that the floor covers a
/// group's worth of frames sent ahead by different amounts.
const STEER: Duration = Duration::from_secs(2);
/// How far back the floors are kept, in decode time: a spell of queueing shorter than half
/// of it moves neither the rate nor the slack.
const RATE_WINDOW: f64 = 600.0;
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
	/// [`RATE_WINDOW`]. Its slope is how much faster the source's clock runs than ours.
	phase: VecDeque<(f64, f64)>,
	/// How far the drift has moved the deadlines, in seconds.
	steered: f64,
	/// The source's clock rate off ours, as last measured: positive runs fast.
	source: Option<f64>,
}

impl<K> Default for Steer<K> {
	fn default() -> Self {
		Self {
			floor: None,
			phase: VecDeque::new(),
			steered: 0.0,
			source: None,
		}
	}
}

/// Each track's frame with the most slack in a step: the one queued least on its way. The
/// step's floor is the track with the least of those, the one sent latest against its decode time.
struct Floor<K> {
	/// Per track, its slack past the delay, less how far the drift has moved its deadline, and
	/// when it decodes, in seconds.
	tracks: BTreeMap<K, (f64, f64)>,
	/// When the step's first frame decodes.
	began: Timestamp,
}

struct Track<T> {
	/// Frames in arrival order, each with its generation, decode time and deadline.
	queue: VecDeque<(u64, Timestamp, Instant, T)>,
	/// The source's restart and skip counters at the last frame.
	restart: u64,
	skip: u64,
	/// The generation of the last frame.
	generation: u64,
	/// Frames are dropped until the next sync frame, and why.
	waiting: Option<Wait>,
}

/// Why a track waits for a sync frame.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Wait {
	/// A frame missed its deadline: what it takes with it counts as dropped.
	Late,
	/// The acquisition dropped what was due before it ended, which is not a loss.
	Acquired,
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
			out_of_tolerance: 0,
		}
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
	fn queue(&mut self, key: K, arrival: Arrival<T>, generation: u64, deadline: Option<Instant>, stale: bool) -> Push {
		let track = self.tracks.entry(key).or_insert_with(|| Track {
			queue: VecDeque::new(),
			restart: arrival.restart,
			skip: arrival.skip,
			generation,
			waiting: None,
		});
		track.restart = arrival.restart;
		track.skip = arrival.skip;
		track.generation = generation;
		let push = match deadline {
			_ if stale => {
				track.waiting = Some(Wait::Acquired);
				Push::Waiting
			}
			_ if track.waiting.is_some() && !arrival.sync => Push::Waiting,
			Some(deadline) if arrival.arrived <= deadline || self.delay.is_zero() => {
				track.waiting = None;
				track
					.queue
					.push_back((generation, arrival.decode, deadline, arrival.item));
				self.horizon = self.horizon.max(Some(deadline));
				Push::Queued
			}
			// Past the deadline, or so far off the clock that no instant holds it.
			_ => {
				track.waiting = Some(Wait::Late);
				Push::Late
			}
		};
		if push != Push::Queued && track.waiting == Some(Wait::Late) {
			self.dropped += 1;
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

	/// Account for the slack a frame arrived with, and every [`STEER`] of decode time step the
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
			began: arrival.decode,
		});
		let best = floor.tracks.entry(key.clone()).or_insert((phase, now));
		if phase > best.0 {
			*best = (phase, now);
		}
		if arrival.decode.as_nanos().saturating_sub(floor.began.as_nanos()) < STEER.as_nanos() {
			return Ok(());
		}
		// Re-anchor where this frame decodes, so the new drift starts from there.
		let Some(anchor) = deadline.checked_sub(self.delay) else {
			return Ok(());
		};
		let floor = self.steer.floor.take().expect("a floor");
		for (key, (phase, _)) in &floor.tracks {
			self.leads.insert(key.clone(), *phase);
		}
		let (phase, at) = floor
			.tracks
			.into_values()
			.min_by(|a, b| a.0.total_cmp(&b.0))
			.expect("a floor holds a track");
		self.steer.steered += elapsed * clock.drift;
		let phases = &mut self.steer.phase;
		phases.push_back((at, phase));
		while phases.front().is_some_and(|&(at, _)| now - at > RATE_WINDOW) {
			phases.pop_front();
		}
		let Some((rate, at)) = envelope(phases, now) else {
			self.clock = Some(Clock {
				anchor,
				base: arrival.decode,
				..clock
			});
			return Ok(());
		};
		// The phase rises as the source's clock runs fast, so frames arrive ever earlier.
		let slow = -rate;
		let gap = at + self.steer.steered;
		self.steer.source = Some(rate);

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
			// At the limit and still losing ground, with half the delay gone.
			if drift == MAX_DRIFT.copysign(slow) && gap * slow.signum() < -delay / 2.0 {
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
		let Some(lead) = self.leads.get(fresh) else {
			return Duration::ZERO;
		};
		let lag = self
			.expect
			.iter()
			.filter(|key| !acquire.tracks.contains_key(key))
			.filter_map(|key| self.leads.get(key))
			.map(|other| lead - other)
			.fold(0.0, f64::max);
		Duration::from_secs_f64(lag)
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
				let (generation, _, _, item) = self.tracks.get_mut(key).and_then(|t| t.queue.pop_front()).unwrap();
				self.released = true;
				// A previous-generation frame queued now would go out after this one.
				if self.clock.is_some_and(|clock| clock.generation == generation) {
					self.previous = None;
				}
				return Poll::Ready(Ready {
					track: key.clone(),
					generation,
					item,
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

	/// When the next queued frame is due, or the acquisition under way ends.
	#[cfg(test)]
	pub fn next_deadline(&self) -> Option<Instant> {
		self.front()
			.map(|(deadline, _)| deadline)
			.into_iter()
			.chain(self.acquired_by())
			.min()
	}

	/// Whether no frame is waiting.
	pub fn is_empty(&self) -> bool {
		self.acquire.is_none() && self.tracks.values().all(|track| track.queue.is_empty())
	}

	/// How many frames were dropped, late or waiting for a sync frame after one was.
	pub fn dropped(&self) -> u64 {
		self.dropped
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

	/// Drop every queued frame and the clock, so the next frame starts it afresh.
	pub fn clear(&mut self) {
		self.clock = None;
		self.previous = None;
		self.leads.clear();
		self.acquire = None;
		self.tracks.clear();
		self.horizon = None;
		self.released = false;
		self.steer = Steer::default();
		self.timer = None;
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
			sync: true,
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
		let late = start + Duration::from_millis(200);
		let video = |decode, sync, item| Arrival {
			sync,
			..arrival(late, decode, item)
		};
		assert_eq!(buffer.push(1, video(40, false, "v40")).unwrap(), Push::Late);
		assert_eq!(buffer.push(1, video(200, false, "v200")).unwrap(), Push::Waiting);
		assert_eq!(buffer.push(1, video(240, true, "v240")).unwrap(), Push::Queued);
		assert_eq!(buffer.push(2, arrival(late, 180, "a180")).unwrap(), Push::Queued);
		assert_eq!(buffer.dropped(), 2);

		tokio::time::advance(Duration::from_secs(1)).await;
		assert_eq!(due(&mut buffer), ["v0", "a0", "a180", "v240"]);
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
				sync: true,
				item: "v",
			};
			match buffer.push(1, frame) {
				Ok(push) => assert_eq!(push, Push::Queued, "{ppm} ppm: frame {k} was late"),
				Err(err) => {
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
	/// half the delay, before any frame goes late.
	#[tokio::test(start_paused = true)]
	async fn a_source_past_the_drift_limit_fails() {
		for ppm in [40.0, -40.0] {
			let run = drift(ppm, 6.0, 6.0, |_| Duration::ZERO).await;
			assert!(run.err.is_some(), "{ppm} ppm: no error");
			assert!(run.out_of_tolerance > 0, "{ppm} ppm: not counted");
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

	#[tokio::test(start_paused = true)]
	async fn clear_starts_a_fresh_clock() {
		let start = Instant::now();
		let mut buffer = Buffer::new(DELAY);
		buffer.push(1, arrival(start, 5_000, "old")).unwrap();
		buffer.clear();
		assert!(buffer.is_empty());

		tokio::time::advance(Duration::from_secs(1)).await;
		let now = Instant::now();
		assert_eq!(buffer.push(1, arrival(now, 0, "new")).unwrap(), Push::Queued);
		tokio::time::advance(DELAY).await;
		assert_eq!(due(&mut buffer), ["new"]);
	}
}
