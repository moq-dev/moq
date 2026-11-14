//! One playout clock shared by a broadcast's tracks.
//!
//! Each [`Track`] folds its arrivals into the clock with [`Track::received`],
//! and [`Producer::due`] says when media is due. The clock's reference is the earliest
//! arrival any live track has seen, measured against its timestamp, so it sits
//! on the wall clock and follows the live edge, moving later only after a lasting
//! step in path delay (see `doc/concept/audio-jitter.md`). Every track presents against
//! it, which holds them together in the steady state. One that falls behind,
//! such as audio stalled by congestion, only arrives late: it never holds or
//! moves the others. Audio converges on the clock by stretching toward its
//! [`due`](Producer::due) time rather than owning it.
//!
//! Playback trails the reference by the largest target a live track holds
//! ([`Track::set_target`]), or by the delay passed to [`Producer::new`] while
//! none holds one. Give a [`Consumer`] to a presenter: [`Consumer::present`]
//! selects the newest due picture without waiting, so a canvas can repeat its
//! previous picture. A presenter that sleeps until its next picture is due
//! reads the [`Schedule`] it sleeps on and wakes when
//! [`Consumer::poll_changed`] reports that it moved.
//!
//! A track also maps encoder segments onto the clock across declared ends.
//! Keep one active track per [`Kind`]; retire it before
//! starting its successor. Bound replacement media with the caller's retention
//! budget and [`Track::abandon`] transitions whose replacement is discarded.

use kio::clock::Instant;
use std::time::Duration;

use moq_net::Timestamp;
use std::{
	pin::Pin,
	sync::{Arc, Mutex},
	task::Poll,
};

use crate::container::Kind;

/// One slot per [`Kind`].
const KINDS: usize = 3;

/// How long a track's arrivals must all trail its anchor before the anchor
/// follows them later: the estimator's window, so the clock and the target
/// measure lateness against the same fastest frame.
const FOLLOW: Duration = Duration::from_secs(2);

struct Clock {
	state: Mutex<State>,
	changed: kio::Shared<()>,
}

impl Clock {
	fn new(delay: Duration) -> Self {
		Self {
			state: Mutex::new(State::new(delay)),
			changed: Default::default(),
		}
	}

	fn wake(&self) {
		*self.changed.lock() = ();
	}
}

impl Part {
	/// Fold `arrival` into the open window, and once it closes move the anchor
	/// later to the window's earliest arrival if every arrival in it trailed the
	/// anchor by more than `half`, as after a lasting increase in path delay.
	fn follow(&mut self, arrival: Anchor, half: Duration) {
		let Some(mut window) = self.window else {
			self.window = Some(Window {
				start: arrival.at,
				least: arrival,
			});
			return;
		};
		if arrival.earlier(&window.least) {
			window.least = arrival;
		}
		// The arrival that closes a window counts in it and opens the next.
		if arrival.at.saturating_duration_since(window.start) < FOLLOW {
			self.window = Some(window);
			return;
		}
		self.window = Some(Window {
			start: arrival.at,
			least: arrival,
		});
		let Some(anchor) = self.anchor else { return };
		let Some(at) = anchor.at.checked_add(half) else { return };
		let bound = Anchor { at, ..anchor };
		if bound.earlier(&window.least) {
			self.anchor = Some(window.least);
		}
	}
}

/// A schedule pinning media time to the wall clock: `media` arrived at `at`.
#[derive(Clone, Copy, Debug)]
struct Anchor {
	at: Instant,
	media: Duration,
}

impl Anchor {
	/// When `media` lands on this schedule, before any delay. A time before the
	/// platform clock can express lands on the anchor, which is already past.
	fn at(&self, media: Duration) -> Option<Instant> {
		match media.checked_sub(self.media) {
			Some(ahead) => self.at.checked_add(ahead),
			None => Some(self.at.checked_sub(self.media - media).unwrap_or(self.at)),
		}
	}

	/// Whether this schedule runs ahead of `other`, so its media lands earlier.
	fn earlier(&self, other: &Self) -> bool {
		match other.media.checked_sub(self.media) {
			Some(ahead) => self.at.checked_add(ahead).is_some_and(|at| at < other.at),
			None => other
				.at
				.checked_add(self.media - other.media)
				.is_none_or(|at| self.at < at),
		}
	}
}

/// Whether two optional schedules differ.
fn moved(before: Option<Anchor>, after: Option<Anchor>) -> bool {
	match (before, after) {
		(Some(before), Some(after)) => before.earlier(&after) || after.earlier(&before),
		(before, after) => before.is_some() != after.is_some(),
	}
}

/// One track's hold on the clock.
#[derive(Clone, Copy, Default)]
struct Part {
	/// The earliest arrival on this track's current timeline, or the earliest
	/// of the last window once a whole window trailed it.
	anchor: Option<Anchor>,
	/// The delay this track asks playback to hold.
	target: Option<Duration>,
	/// The window of arrivals that decides whether the anchor follows a lasting step later.
	window: Option<Window>,
}

/// A window of one track's arrivals: when it opened and the earliest schedule among them.
#[derive(Clone, Copy)]
struct Window {
	start: Instant,
	least: Anchor,
}

/// The playout clock every track of one broadcast presents against.
///
/// Each track keeps the earliest arrival on its own timeline, and the reference
/// is the earliest of those. That is what pulls playback toward live: a frame
/// arriving earlier than the reference predicted re-pins it, while a late
/// frame only reports that it is late. A lasting step later in the path's
/// delay moves a track's anchor later once a whole window of its arrivals
/// trailed it by more than half the delay. A track that resets or goes away
/// gives up its arrivals, so the reference falls back to the earliest track left.
struct State {
	/// How far behind the reference playback runs while no track holds a target.
	delay: Duration,
	parts: [Part; KINDS],
	/// The schedule the last track to go left behind, which what it already
	/// queued still presents against until any track anchors again.
	left: Option<Anchor>,
	/// Endpoints declared by each live role, on the shared playback timeline.
	ends: [Option<Boundary>; KINDS],
	/// When each retired role's tail finishes playing, frozen on the wall clock:
	/// a later rendition anchors afresh, which would carry a media endpoint
	/// onto an unrelated timeline. Kept apart from `ends` so a successor role
	/// never clears its predecessor's tail.
	tails: Vec<Instant>,
	/// Encoder restarts whose replacements have not all started playing.
	transitions: std::collections::VecDeque<Pending>,
	/// The latest transition each role declared.
	declared: [Transition; KINDS],
	next: Transition,
}

/// One encoder restart, shared by the roles whose ends declared it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Transition(u64);

struct Pending {
	id: Transition,
	/// Roles whose declared end opened or joined this transition.
	members: [bool; KINDS],
	/// Members that applied its mapping.
	resumed: [bool; KINDS],
	/// Each member's first replacement media.
	plans: [Option<Plan>; KINDS],
	mapping: Option<Mapping>,
}

struct Boundary {
	starts: Option<Instant>,
	end: Duration,
}

#[derive(Clone, Copy)]
struct Plan {
	source: Duration,
	delay: Duration,
	/// When the replacement media arrived.
	arrived: Instant,
}

impl State {
	fn new(delay: Duration) -> Self {
		Self {
			delay,
			parts: Default::default(),
			left: None,
			ends: Default::default(),
			tails: Vec::new(),
			transitions: Default::default(),
			declared: [Transition(0); KINDS],
			next: Transition(1),
		}
	}

	/// The earliest schedule any track holds.
	fn reference(&self) -> Option<Anchor> {
		self.parts
			.iter()
			.filter_map(|part| part.anchor)
			.reduce(|earliest, anchor| if anchor.earlier(&earliest) { anchor } else { earliest })
			.or(self.left)
	}

	/// How far behind the reference playback runs.
	fn delay(&self) -> Duration {
		self.parts
			.iter()
			.filter_map(|part| part.target)
			.max()
			.unwrap_or(self.delay)
	}

	/// Fold an arrival into `kind`'s timeline, reporting whether that moved the schedule.
	fn received(&mut self, kind: Kind, timestamp: Timestamp, now: Instant) -> bool {
		let arrival = Anchor {
			at: now,
			media: Duration::from(timestamp),
		};
		let before = self.reference();
		let half = self.delay() / 2;
		let part = &mut self.parts[kind as usize];
		if part.anchor.is_none_or(|anchor| arrival.earlier(&anchor)) {
			part.anchor = Some(arrival);
		}
		part.follow(arrival, half);
		self.left = None;
		moved(before, self.reference())
	}

	/// End `kind`'s timeline, reporting whether that moved the schedule.
	fn reset(&mut self, kind: Kind) -> bool {
		let before = self.reference();
		let part = &mut self.parts[kind as usize];
		part.anchor = None;
		part.window = None;
		moved(before, self.reference())
	}

	/// Release `kind` as its track goes.
	fn release(&mut self, kind: Kind) {
		let part = std::mem::take(&mut self.parts[kind as usize]);
		if self.parts.iter().all(|part| part.anchor.is_none()) {
			self.left = part.anchor.or(self.left);
		}
	}

	/// Hold `target` for `kind`, reporting whether that moved the schedule.
	fn set_target(&mut self, kind: Kind, target: Duration) -> bool {
		let before = self.delay();
		self.parts[kind as usize].target = Some(target);
		before != self.delay()
	}

	/// How many transitions are still pending.
	#[cfg(test)]
	fn pending(&self) -> usize {
		self.transitions.len()
	}

	fn transition(&mut self, id: Transition) -> Option<&mut Pending> {
		self.transitions.iter_mut().find(|pending| pending.id == id)
	}

	/// Forget transitions nobody resumes, or that every member resumed once
	/// their replacement started and nothing can join them.
	fn prune(&mut self, now: Instant) {
		self.transitions.retain(|pending| {
			let members = pending.members.iter().any(|member| *member);
			let resumed = (0..KINDS).all(|role| !pending.members[role] || pending.resumed[role]);
			let started = pending.mapping.is_some_and(|mapping| mapping.started(now));
			members && !(resumed && started)
		});
	}

	/// The media position the clock will reach at `at`.
	fn position(&self, at: Instant) -> Option<Duration> {
		let reference = self.reference()?;
		let at = at.checked_sub(self.delay()).unwrap_or(Instant::ZERO);
		Some(if at >= reference.at {
			reference.media.saturating_add(at.duration_since(reference.at))
		} else {
			reference.media.saturating_sub(reference.at.duration_since(at))
		})
	}

	/// When `timestamp` should be presented, or `None` before any frame has
	/// anchored the clock. `None` means as soon as possible: there is nothing
	/// left to wait for.
	fn due(&self, timestamp: Timestamp) -> Option<Instant> {
		self.schedule().due(timestamp)
	}

	/// The reference and delay every due time derives from.
	fn schedule(&self) -> Schedule {
		Schedule {
			reference: self.reference(),
			delay: self.delay(),
		}
	}
}

/// The playout schedule at one moment: where the reference sits and how far
/// playback trails it.
///
/// Two schedules are equal when they place every timestamp at the same
/// instant, so a reference re-pinned onto the same line compares equal.
#[derive(Clone, Copy, Debug)]
pub struct Schedule {
	reference: Option<Anchor>,
	delay: Duration,
}

impl Schedule {
	/// How far behind the reference playback runs.
	pub fn delay(&self) -> Duration {
		self.delay
	}

	/// When `timestamp` is due on this schedule, or `None` before the first anchor.
	pub fn due(&self, timestamp: Timestamp) -> Option<Instant> {
		let at = self.reference?.at(Duration::from(timestamp))?;
		Some(at.checked_add(self.delay).unwrap_or(at))
	}
}

impl PartialEq for Schedule {
	fn eq(&self, other: &Self) -> bool {
		self.delay == other.delay && !moved(self.reference, other.reference)
	}
}

/// One track's hold on the shared clock: its arrivals and target, and the
/// mapping of its encoder segments onto the shared timeline.
///
/// Dropping it releases its arrivals and target.
pub struct Track {
	clock: Arc<Clock>,
	role: Kind,
	mapping: std::sync::Mutex<Segment>,
}

/// The current segment's mapping, and how long it held the segment's media
/// back behind the old segment's tail.
#[derive(Clone, Copy)]
struct Segment {
	mapping: Mapping,
	/// Added to each arrival: media held behind a tail arrived that much earlier
	/// than its schedule, which says nothing about the live edge.
	held: Duration,
}

/// A segment's timestamp mapping, applied only after its first media can play.
#[derive(Clone, Copy)]
#[must_use]
pub struct Mapping {
	source: Duration,
	target: Duration,
	starts: Option<Instant>,
}

impl Mapping {
	fn map(self, source: Timestamp) -> Result<Timestamp> {
		let source = Duration::from(source);
		let target = if source >= self.source {
			self.target.checked_add(source - self.source).ok_or(Error::Overflow)?
		} else {
			self.target
				.checked_sub(self.source - source)
				.ok_or(Error::BeforeOrigin)?
		};
		Timestamp::from_micros(target.as_micros().try_into().map_err(|_| Error::Overflow)?).map_err(|_| Error::Overflow)
	}

	fn started(&self, now: Instant) -> bool {
		self.starts.is_none_or(|starts| starts <= now)
	}
}

impl Track {
	fn new(clock: Arc<Clock>, role: Kind) -> Self {
		Self {
			clock,
			role,
			mapping: std::sync::Mutex::new(Segment {
				mapping: Mapping {
					source: Duration::ZERO,
					target: Duration::ZERO,
					starts: None,
				},
				held: Duration::ZERO,
			}),
		}
	}

	/// Fold a frame that arrived at `now` into the clock, reporting whether that
	/// moved the schedule.
	///
	/// `timestamp` is on the shared timeline, as [`map`](Self::map) returns it.
	/// A frame arriving earlier than the schedule predicted pulls playback toward
	/// live; a late one leaves it alone. A move makes every queued frame due
	/// earlier, so a presenter has to recompute the deadline it is asleep on.
	pub fn received(&self, timestamp: Timestamp, now: Instant) -> bool {
		let held = self.mapping.lock().unwrap().held;
		let now = now.checked_add(held).unwrap_or(now);
		let moved = self.clock.state.lock().unwrap().received(self.role, timestamp, now);
		if moved {
			self.clock.wake();
		}
		moved
	}

	/// End this track's timeline at a discontinuity, such as lost media or a
	/// rewind: its arrivals stop holding the clock until the next one.
	///
	/// Media after the break was not held behind any tail, so its arrivals are
	/// the live edge again. Keeping the hold would anchor the clock that much
	/// late and make the media wait for it.
	pub fn reset(&self) {
		self.mapping.lock().unwrap().held = Duration::ZERO;
		if self.clock.state.lock().unwrap().reset(self.role) {
			self.clock.wake();
		}
	}

	/// Ask playback to trail the reference by at least `target`, reporting
	/// whether that moved the schedule. Playback holds the largest target any
	/// live track asks for.
	pub fn set_target(&self, target: Duration) -> bool {
		let moved = self.clock.state.lock().unwrap().set_target(self.role, target);
		if moved {
			self.clock.wake();
		}
		moved
	}

	/// Wait until playback trails the reference by something other than `seen`, returning the new delay.
	#[cfg_attr(feature = "async", kio::wrapper)]
	pub fn poll_delay_changed(&self, waiter: &kio::Waiter, seen: Duration) -> Poll<Duration> {
		let mut delay = seen;
		let _ = std::task::ready!(self.clock.changed.poll(waiter, |_| {
			delay = self.clock.state.lock().unwrap().delay();
			if delay == seen { Poll::Pending } else { Poll::Ready(()) }
		}));
		Poll::Ready(delay)
	}

	/// Stop while a tail still plays until `drained`, holding a declared end until
	/// then. Dropping instead discards the tail with the end.
	pub fn retire(self, drained: Option<Instant>) {
		let now = kio::clock::now();
		let mut state = self.clock.state.lock().unwrap();
		state.tails.retain(|tail| *tail > now);
		if state.ends[self.role as usize].take().is_some()
			&& let Some(drained) = drained
		{
			state.tails.push(drained);
		}
	}

	/// Map a source timestamp onto the shared broadcast timeline.
	pub fn map(&self, source: Timestamp) -> Result<Timestamp> {
		self.mapping.lock().unwrap().mapping.map(source)
	}

	/// Declare that this role's encoder stopped, returning the transition its
	/// replacement resumes.
	///
	/// A peer's transition this role has not joined yet is the same restart, as
	/// long as its replacement has not started playing; otherwise this one opens
	/// a transition of its own.
	pub fn declare(&self) -> Transition {
		let now = kio::clock::now();
		let own = self.role as usize;
		let mut state = self.clock.state.lock().unwrap();
		state.prune(now);
		let after = state.declared[own];
		let joined = state
			.transitions
			.iter_mut()
			.filter(|transition| transition.id > after)
			.find(|transition| {
				!transition.members[own]
					&& transition.members.iter().any(|member| *member)
					&& transition.mapping.is_none_or(|mapping| !mapping.started(now))
			})
			.map(|transition| {
				transition.members[own] = true;
				transition.id
			});
		let id = joined.unwrap_or_else(|| {
			let id = state.next;
			state.next = Transition(id.0 + 1);
			let mut members = [false; KINDS];
			members[own] = true;
			state.transitions.push_back(Pending {
				id,
				members,
				resumed: [false; KINDS],
				plans: [None; KINDS],
				mapping: None,
			});
			id
		});
		state.declared[own] = id;
		drop(state);
		self.clock.wake();
		id
	}

	/// Mark where this role's current segment ends on the shared clock.
	pub fn end(&self, source: Timestamp) -> Result<()> {
		let mapping = self.mapping.lock().unwrap().mapping;
		self.clock.state.lock().unwrap().ends[self.role as usize] = Some(Boundary {
			starts: mapping.starts,
			end: Duration::from(mapping.map(source)?),
		});
		self.clock.wake();
		Ok(())
	}

	/// Record the first replacement media for `transition`, as it arrives.
	pub fn plan(&self, transition: Transition, source: Timestamp, delay: Option<Duration>) {
		let arrived = kio::clock::now();
		let mut state = self.clock.state.lock().unwrap();
		let delay = delay.unwrap_or(state.delay());
		if let Some(pending) = state.transition(transition) {
			pending.plans[self.role as usize] = Some(Plan {
				source: Duration::from(source),
				delay,
				arrived,
			});
		}
		drop(state);
		self.clock.wake();
	}

	/// Media was lost: nothing waits for the discarded tail.
	pub fn gap(&self) {
		self.clock.state.lock().unwrap().ends[self.role as usize] = None;
		self.clock.wake();
	}

	/// This role will never resume `transition`, so a peer must not pair with its plan.
	pub fn abandon(&self, transition: Transition) {
		let own = self.role as usize;
		let mut state = self.clock.state.lock().unwrap();
		if let Some(pending) = state.transition(transition) {
			pending.members[own] = false;
			pending.plans[own] = None;
		}
		state.prune(kio::clock::now());
		drop(state);
		self.clock.wake();
	}

	/// Commit the mapping once its replacement media starts playing.
	pub fn apply(&self, transition: Transition, mapping: Mapping) {
		let mut segment = self.mapping.lock().unwrap();
		let mut state = self.clock.state.lock().unwrap();
		let reference = state.reference();
		let mut held = Duration::ZERO;
		if let Some(pending) = state.transition(transition) {
			pending.resumed[self.role as usize] = true;
			// How much earlier the replacement arrived than the schedule it now
			// plays on, having waited for the old tail to drain.
			if let Some(plan) = pending.plans[self.role as usize]
				&& let Some(scheduled) = mapping
					.map(media(plan.source))
					.ok()
					.zip(reference)
					.and_then(|(mapped, reference)| reference.at(Duration::from(mapped)))
			{
				held = scheduled.saturating_duration_since(plan.arrived);
			}
		}
		*segment = Segment { mapping, held };
		drop(segment);
		state.prune(kio::clock::now());
		drop(state);
		self.clock.wake();
	}

	/// Prepare a mapping one delay before the old media drains; dropping the wait leaves the track unchanged.
	pub fn resume(&self, transition: Transition, source: Timestamp, delay: Option<Duration>) -> Resume<'_> {
		Resume {
			track: self,
			transition,
			source,
			delay,
			timer: None,
		}
	}
}

impl Drop for Track {
	fn drop(&mut self) {
		let own = self.role as usize;
		{
			let mut state = self.clock.state.lock().unwrap();
			state.release(self.role);
			state.ends[own] = None;
			for pending in state.transitions.iter_mut().filter(|pending| !pending.resumed[own]) {
				pending.members[own] = false;
				pending.plans[own] = None;
			}
			state.declared[own] = Transition(0);
			state.prune(kio::clock::now());
		}
		self.clock.wake();
	}
}

/// A duration from the start of the track as a wire timestamp.
fn media(duration: Duration) -> Timestamp {
	// A timestamp caps at 2^61 - 1, which is ~73,000 years of microseconds.
	const MAX: u128 = (1 << 61) - 1;
	Timestamp::from_micros(duration.as_micros().min(MAX) as u64).expect("clamped to the timestamp maximum")
}

/// A pending segment mapping, driven by the caller's executor or waiter.
#[must_use = "drive the wait before applying its mapping"]
pub struct Resume<'a> {
	track: &'a Track,
	transition: Transition,
	source: Timestamp,
	delay: Option<Duration>,
	timer: Option<kio::clock::Sleep>,
}

impl Resume<'_> {
	/// The replacement's mapping, once all declared old media is due to drain within one delay.
	#[cfg_attr(feature = "async", kio::wrapper)]
	pub fn poll_ready(&mut self, waiter: &kio::Waiter) -> Poll<Result<Mapping>> {
		let _ = self.track.clock.changed.poll(waiter, |_| Poll::Pending);
		let now = kio::clock::now();
		let mut state = self.track.clock.state.lock().unwrap();
		if let Some(mapping) = state.transition(self.transition).and_then(|pending| pending.mapping) {
			return Poll::Ready(Ok(mapping));
		}
		let tail = state
			.ends
			.iter()
			.flatten()
			.filter(|boundary| boundary.starts.is_none_or(|start| start <= now))
			.filter_map(|boundary| state.due(media(boundary.end)))
			.chain(state.tails.iter().copied())
			.max();
		// Wake one delay before the tail ends: media written then plays as it ends.
		let lead = self.delay.unwrap_or(state.delay());
		if let Some(wake) = tail
			.map(|end| end.checked_sub(lead).unwrap_or(now))
			.filter(|wake| *wake > now)
		{
			let timer = self.timer.get_or_insert_with(|| kio::clock::sleep_until(wake));
			if timer.deadline() != wake {
				timer.reset(wake);
			}
			std::task::ready!(waiter.poll_future(Pin::new(timer)));
		}

		let own = self.track.role as usize;
		// A transition abandoned under this resume, by a gap the caller rechecks,
		// maps alone like one no peer joined.
		let (mapping, paired, own_plan) = state.transition(self.transition).map_or((None, None, None), |pending| {
			let mut plans = pending
				.members
				.iter()
				.zip(&pending.plans)
				.filter(|(member, _)| **member);
			let joined = pending.members[own] && plans.clone().count() > 1;
			let origin = plans
				.try_fold(None::<Plan>, |earliest, (_, plan)| {
					let plan = (*plan)?;
					Some(Some(match earliest {
						Some(earliest) if earliest.source <= plan.source => earliest,
						_ => plan,
					}))
				})
				.flatten();
			(pending.mapping, origin.filter(|_| joined), pending.plans[own])
		});
		// The peer already resumed this transition.
		if let Some(mapping) = mapping {
			return Poll::Ready(Ok(mapping));
		}
		// Every member has replacement media. The earliest defines their common
		// origin, keeping the media offsets between them, even when its delay
		// differs from the others'. Otherwise this role maps alone and a peer
		// joining later follows it.
		let (source, delay, arrived) = match paired {
			Some(origin) => (origin.source, origin.delay, Some(origin.arrived)),
			None => (
				Duration::from(self.source),
				self.delay.unwrap_or(state.delay()),
				own_plan.map(|plan| plan.arrived),
			),
		};
		// Media that arrived before the old tail ended continues its schedule:
		// it starts one delay after arriving, or as the tail ends if later, so
		// back-to-back segments add no hold. Only after a real gap, where the
		// sink underruns anyway, does it re-buffer one delay from now.
		let starts = match (tail, arrived) {
			(Some(end), Some(arrived)) if arrived <= end => {
				// One tick past the end: a tail whose last frame declared no
				// duration ends at that frame's timestamp, which the first new
				// frame must not share.
				let after = end.checked_add(Duration::from_micros(1)).ok_or(Error::DelayOverflow)?;
				arrived.checked_add(delay).ok_or(Error::DelayOverflow)?.max(after)
			}
			_ => now.checked_add(delay).ok_or(Error::DelayOverflow)?,
		};
		let mapping = Mapping {
			source,
			target: state.position(starts).unwrap_or(source),
			starts: Some(starts),
		};
		if let Some(pending) = state.transition(self.transition) {
			pending.mapping = Some(mapping);
		}
		Poll::Ready(Ok(mapping))
	}
}

/// A timestamp or delay cannot be represented on the playout clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	/// Mapping the timestamp would overflow.
	#[error("playback timeline overflow")]
	Overflow,
	/// A picture precedes the mapped segment's origin.
	#[error("picture precedes the playback timeline")]
	BeforeOrigin,
	/// The requested delay exceeds the system clock.
	#[error("playback delay exceeds the system clock")]
	DelayOverflow,
}

/// A result from mapping media onto the playout clock.
pub type Result<T> = std::result::Result<T, Error>;

/// Owns one broadcast's playout clock and hands out its tracks.
///
/// Clones capture this broadcast's clock; resetting a handle never changes the
/// clock its previous clones or tracks update.
#[derive(Clone)]
pub struct Producer {
	clock: Arc<Clock>,
	current: Arc<Mutex<Arc<Clock>>>,
}

/// Reads the current playout schedule without moving it.
///
/// Keeps the final schedule readable after its tracks drop, and follows the
/// replacement clock when a producer resets for another broadcast.
#[derive(Clone)]
pub struct Consumer {
	current: Arc<Mutex<Arc<Clock>>>,
}

impl Producer {
	/// Create a broadcast clock that trails the reference by `delay` while no track holds a target.
	pub fn new(delay: Duration) -> Self {
		let clock = Arc::new(Clock::new(delay));
		Self {
			clock: clock.clone(),
			current: Arc::new(Mutex::new(clock)),
		}
	}

	/// Replace this producer with a new broadcast clock and switch readers to it.
	///
	/// Previous clones and tracks retain their old broadcast's clock.
	#[must_use = "use the returned producer for the replacement broadcast"]
	pub fn reset(self, delay: Duration) -> Self {
		let clock = Arc::new(Clock::new(delay));
		// Wake the clock readers followed, which is not this handle's own when a
		// stale clone resets, and only after the swap so they find the new one.
		let replaced = std::mem::replace(&mut *self.current.lock().unwrap(), clock.clone());
		replaced.wake();
		Self {
			clock,
			current: self.current,
		}
	}

	/// Return a reader that follows this handle's broadcast replacements.
	pub fn consume(&self) -> Consumer {
		Consumer {
			current: self.current.clone(),
		}
	}

	/// Return a track bound to this broadcast's clock.
	pub fn track(&self, kind: Kind) -> Track {
		Track::new(self.clock.clone(), kind)
	}

	/// When `timestamp` should be presented, or `None` before any frame has
	/// anchored the clock. `None` means as soon as possible: there is nothing
	/// left to wait for.
	pub fn due(&self, timestamp: Timestamp) -> Option<Instant> {
		self.clock.state.lock().unwrap().due(timestamp)
	}
}

impl Consumer {
	/// How far behind the reference playback runs.
	pub fn delay(&self) -> Duration {
		self.current.lock().unwrap().state.lock().unwrap().delay()
	}

	/// When this timestamp is due, or `None` before the first anchor.
	pub fn due(&self, timestamp: Timestamp) -> Option<Instant> {
		self.current.lock().unwrap().state.lock().unwrap().due(timestamp)
	}

	/// The media time the clock presents at `at`, or `None` before the first anchor.
	pub fn position(&self, at: Instant) -> Option<Duration> {
		self.current.lock().unwrap().state.lock().unwrap().position(at)
	}

	/// The current schedule, to read due times from or to pass to
	/// [`poll_changed`](Self::poll_changed).
	pub fn schedule(&self) -> Schedule {
		self.current.lock().unwrap().state.lock().unwrap().schedule()
	}

	/// Wait until the schedule differs from `seen`, returning the new one.
	///
	/// Resolves once any due time moved: a track re-pinned the reference, the
	/// delay changed, the last anchor went away, or the producer reset onto a
	/// clock that schedules differently. A change made before the call, even
	/// while no wait was pending, resolves it at once; changes that cancel out
	/// before the call do not.
	#[cfg_attr(feature = "async", kio::wrapper)]
	pub fn poll_changed(&self, waiter: &kio::Waiter, seen: Schedule) -> Poll<Schedule> {
		loop {
			let clock = self.current.lock().unwrap().clone();
			let mut changed = None;
			// A reset swaps readers before waking the old clock, so checking the
			// swap under the old clock's signal never misses one.
			let _ = std::task::ready!(clock.changed.poll(waiter, |_| {
				if !Arc::ptr_eq(&self.current.lock().unwrap(), &clock) {
					return Poll::Ready(());
				}
				let schedule = clock.state.lock().unwrap().schedule();
				if schedule == seen {
					return Poll::Pending;
				}
				changed = Some(schedule);
				Poll::Ready(())
			}));
			if let Some(schedule) = changed {
				return Poll::Ready(schedule);
			}
		}
	}

	/// The newest due picture's index in presentation order, or `None` to repeat the previous one.
	///
	/// Pass queued timestamps in ascending order and discard entries through the
	/// returned index after showing that picture. This stores no frames and leaves
	/// their retention and backpressure to the caller. Query again after every
	/// schedule move, which [`poll_changed`](Self::poll_changed) reports; a cached
	/// deadline can become stale.
	pub fn present(&self, timestamps: impl IntoIterator<Item = Timestamp>, now: Instant) -> Option<usize> {
		let clock = self.current.lock().unwrap();
		let state = clock.state.lock().unwrap();
		timestamps
			.into_iter()
			.take_while(|timestamp| state.due(*timestamp).is_none_or(|at| at <= now))
			.count()
			.checked_sub(1)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	const DELAY: Duration = Duration::from_millis(100);
	/// How far past the tail's end a continuing segment starts.
	const TICK: Duration = Duration::from_micros(1);

	fn ms(millis: u64) -> Timestamp {
		Timestamp::from_millis(millis).unwrap()
	}

	/// A clock with an audio and a video track, audio anchored at 10 s arriving `now`.
	fn anchored(now: Instant) -> (Arc<Clock>, Track, Track) {
		let clock = Arc::new(Clock::new(DELAY));
		let audio = Track::new(clock.clone(), Kind::Audio);
		let video = Track::new(clock.clone(), Kind::Video);
		audio.received(ms(10_000), now);
		(clock, audio, video)
	}

	#[derive(serde::Deserialize)]
	struct Case {
		name: String,
		delay: u64,
		runs: Vec<Run>,
	}

	#[derive(serde::Deserialize)]
	struct Run {
		tracks: Vec<String>,
		start: u64,
		every: u64,
		frames: u64,
		offsets: Vec<u64>,
		reference: u64,
	}

	/// Each case plays runs of arrivals and checks where the reference sits after each.
	#[test]
	fn the_reference_conforms_to_the_corpus() {
		// Media starts this far into the track, so it is never negative.
		const MEDIA: u64 = 100_000;
		let cases: Vec<Case> =
			serde_json::from_str(include_str!("../../../doc/concept/audio-jitter/clock/corpus.json")).unwrap();
		let base = Instant::ZERO.saturating_add(Duration::from_secs(5));
		for case in cases {
			let delay = Duration::from_millis(case.delay);
			let clock = Producer::new(delay);
			let audio = clock.track(Kind::Audio);
			let video = clock.track(Kind::Video);
			for (index, run) in case.runs.iter().enumerate() {
				for i in 0..run.frames {
					let arrival = run.start + i * run.every;
					let offset = run.offsets[(i as usize) % run.offsets.len()];
					let track = match run.tracks[(i as usize) % run.tracks.len()].as_str() {
						"audio" => &audio,
						"video" => &video,
						other => panic!("unknown track {other}"),
					};
					track.received(
						ms(MEDIA + arrival - offset),
						base.saturating_add(Duration::from_millis(arrival)),
					);
				}
				// The reference's offset is when media `MEDIA` is due, less the delay, after `base`.
				let due = clock.due(ms(MEDIA)).unwrap().saturating_sub(delay);
				assert_eq!(
					due.duration_since(base),
					Duration::from_millis(run.reference),
					"{} run {index}",
					case.name
				);
			}
		}
	}

	/// Earlier arrivals pull the reference forward so playback converges on the
	/// live edge without giving up the delay; late ones leave it alone.
	#[test]
	fn the_earliest_arrival_sets_the_schedule() {
		let start = Instant::ZERO.saturating_add(Duration::from_secs(1));
		let clock = Producer::new(DELAY);
		let video = clock.track(Kind::Video);
		assert_eq!(clock.due(ms(0)), None, "nothing anchors before the first frame");

		assert!(video.received(ms(0), start), "the first anchor");
		assert_eq!(clock.due(ms(0)), Some(start.saturating_add(DELAY)));

		// 40ms of media later, but only 20ms of wall clock: the first frame was
		// 20ms late, so the schedule moves onto this one.
		let now = start.saturating_add(Duration::from_millis(20));
		assert!(video.received(ms(40), now), "an early frame left the schedule alone");
		assert_eq!(clock.due(ms(40)), Some(now.saturating_add(DELAY)));

		// A frame that merely arrives late keeps its media instant. The delay is
		// the room it has to be late in.
		let late = now.saturating_add(Duration::from_millis(70));
		assert!(!video.received(ms(80), late), "a late arrival moved the schedule");
		assert_eq!(clock.due(ms(80)), Some(now.saturating_add(Duration::from_millis(140))));
	}

	/// The required decoupling: audio stalled by congestion arrives late, which
	/// neither holds video back nor moves it, while both share one schedule in
	/// the steady state.
	#[test]
	fn stalled_audio_no_longer_holds_video() {
		let start = Instant::ZERO.saturating_add(Duration::from_secs(1));
		let clock = Producer::new(DELAY);
		let audio = clock.track(Kind::Audio);
		let video = clock.track(Kind::Video);
		audio.received(ms(0), start);
		video.received(ms(0), start);
		let steady = clock.due(ms(1_000));
		assert_eq!(
			steady,
			Some(start.saturating_add(DELAY).saturating_add(Duration::from_secs(1)))
		);

		// Video keeps arriving on time while audio stalls for 500ms, then audio
		// lands in a late burst and resets its timeline on a fresh sink.
		for frame in 1..=25 {
			let at = start.saturating_add(Duration::from_millis(frame * 40));
			assert!(!video.received(ms(frame * 40), at));
		}
		let resumed = start.saturating_add(Duration::from_millis(1_000));
		for frame in 0..25 {
			assert!(!audio.received(ms(500 + frame * 20), resumed), "late audio moved video");
		}
		audio.reset();
		assert!(!audio.received(ms(1_000), resumed.saturating_add(Duration::from_millis(100))));
		assert_eq!(clock.due(ms(1_000)), steady, "stalled audio held video");
	}

	/// A video tune-in burst is the live edge: nothing owns the schedule, so it
	/// moves every track onto it, audio included.
	#[test]
	fn a_video_burst_moves_audio_onto_the_live_edge() {
		let start = Instant::ZERO.saturating_add(Duration::from_secs(1));
		let clock = Producer::new(DELAY);
		let audio = clock.track(Kind::Audio);
		let video = clock.track(Kind::Video);
		audio.received(ms(1_000), start);
		assert!(video.received(ms(4_000), start));
		assert_eq!(clock.due(ms(4_000)), Some(start.saturating_add(DELAY)));
	}

	/// A track that resets gives up its arrivals: the schedule falls back to the
	/// earliest track left, and with none left the next frame anchors afresh.
	#[test]
	fn a_reset_hands_the_schedule_to_the_tracks_left() {
		let start = Instant::ZERO.saturating_add(Duration::from_secs(1));
		let clock = Producer::new(DELAY);
		let audio = clock.track(Kind::Audio);
		let video = clock.track(Kind::Video);
		video.received(ms(0), start);
		// Audio runs 30ms ahead of video, so it holds the schedule.
		audio.received(ms(30), start);
		assert_eq!(clock.due(ms(30)), Some(start.saturating_add(DELAY)));

		audio.reset();
		assert_eq!(
			clock.due(ms(30)),
			Some(start.saturating_add(DELAY).saturating_add(Duration::from_millis(30)))
		);

		video.reset();
		assert_eq!(clock.due(ms(30)), None);
		let now = start.saturating_add(Duration::from_secs(5));
		audio.received(ms(500), now);
		assert_eq!(clock.due(ms(500)), Some(now.saturating_add(DELAY)));

		// The last track to go leaves its schedule for what it queued, until
		// another anchors afresh.
		drop(audio);
		assert_eq!(clock.due(ms(500)), Some(now.saturating_add(DELAY)));
		let later = now.saturating_add(Duration::from_secs(5));
		video.received(ms(0), later);
		assert_eq!(clock.due(ms(0)), Some(later.saturating_add(DELAY)));
	}

	/// Playback holds the deepest target a live track asks for, and the default
	/// while none asks.
	#[test]
	fn the_delay_is_the_largest_target_held() {
		let start = Instant::ZERO.saturating_add(Duration::from_secs(1));
		let clock = Producer::new(DELAY);
		let reader = clock.consume();
		let audio = clock.track(Kind::Audio);
		let video = clock.track(Kind::Video);
		video.received(ms(0), start);
		assert_eq!(reader.delay(), DELAY);

		assert!(audio.set_target(Duration::from_millis(60)));
		assert_eq!(reader.delay(), Duration::from_millis(60));
		assert_eq!(clock.due(ms(0)), Some(start.saturating_add(Duration::from_millis(60))));
		assert!(video.set_target(Duration::from_millis(80)));
		assert!(
			!audio.set_target(Duration::from_millis(70)),
			"a shallower target moved the delay"
		);
		assert_eq!(reader.delay(), Duration::from_millis(80));

		drop(video);
		assert_eq!(reader.delay(), Duration::from_millis(70));
		drop(audio);
		assert_eq!(reader.delay(), DELAY);
	}

	#[test]
	fn presentation_selects_due_frames_on_the_current_schedule() {
		let clock = Producer::new(Duration::from_millis(40));
		let reader = clock.consume();
		let audio = clock.track(Kind::Audio);
		let video = clock.track(Kind::Video);
		let now = Instant::ZERO.saturating_add(Duration::from_secs(1));
		video.received(ms(0), now);
		let frames = [ms(0), ms(20), ms(40)];
		assert_eq!(reader.present(frames, now), None);
		assert_eq!(
			reader.present(frames, now.saturating_add(Duration::from_millis(60))),
			Some(1)
		);
		// Audio arrives 40ms ahead; stale video deadlines no longer apply.
		audio.received(ms(40), now);
		assert_eq!(reader.present(frames, now), Some(0));
		assert_eq!(
			reader.present(frames, now.saturating_add(Duration::from_millis(39))),
			Some(1)
		);
		assert_eq!(
			reader.present(frames, now.saturating_add(Duration::from_millis(40))),
			Some(2)
		);
		// Neither a stall nor a future-only queue invents a new picture.
		assert_eq!(reader.present([], now.saturating_add(Duration::from_secs(1))), None);
		assert_eq!(
			reader.present([ms(200)], now.saturating_add(Duration::from_millis(100))),
			None
		);
	}

	#[tokio::test]
	async fn replacing_a_broadcast_isolates_its_old_update_handles() {
		tokio::time::pause();
		let clock = Producer::new(DELAY);
		let reader = clock.consume();
		let previous = clock.clone();
		let old = clock.track(Kind::Audio);
		old.declare();
		let clock = clock.reset(DELAY);
		let now = kio::clock::now();
		let audio = clock.track(Kind::Audio);
		audio.received(ms(0), now);
		audio.declare();
		audio.end(ms(1_000)).unwrap();
		// A JoinSet abort can leave these updates and drops until after the new track started.
		drop(old);
		previous.track(Kind::Audio).received(ms(10_000), now);
		assert_eq!(reader.due(ms(0)), Some(now.saturating_add(DELAY)));
		let video = clock.track(Kind::Video);
		let transition = video.declare();
		video.plan(transition, ms(0), None);
		let resumed = kio::clock::timeout(
			DELAY + Duration::from_millis(500),
			video.resume(transition, ms(0), None).ready(),
		)
		.await;
		assert!(resumed.is_err(), "old handle cleared the new audio tail");
	}

	#[tokio::test]
	async fn ordinary_arrivals_leave_a_parked_resume_asleep() {
		tokio::time::pause();
		let clock = Producer::new(DELAY);
		let now = kio::clock::now();
		let audio = clock.track(Kind::Audio);
		audio.received(ms(0), now);
		audio.end(ms(1_000)).unwrap();
		let video = clock.track(Kind::Video);
		let transition = video.declare();
		video.plan(transition, ms(0), None);
		let (wakes, waiter) = Wakes::waiter();
		let mut resume = video.resume(transition, ms(0), None);
		assert!(resume.poll_ready(&waiter).is_pending());
		let later = now.saturating_add(Duration::from_millis(20));
		video.received(ms(20), later);
		audio.received(ms(20), later);
		assert_eq!(wakes.count(), 0);
		audio.received(ms(40), later);
		assert!(wakes.count() > 0, "a moved schedule must wake the resume");
	}

	#[test]
	fn discarded_replacements_release_their_transition_state() {
		let clock = Arc::new(Clock::new(Duration::ZERO));
		let video = Track::new(clock.clone(), Kind::Video);
		let audio = Track::new(clock.clone(), Kind::Audio);
		let first = audio.declare();
		assert_eq!(first, video.declare());
		// Audio can hold its declared end while video discards any number of replacements.
		for _ in 0..100 {
			let transition = video.declare();
			video.plan(transition, Timestamp::ZERO, None);
			video.abandon(transition);
			assert_eq!(clock.state.lock().unwrap().pending(), 1);
		}
		video.abandon(first);
		drop(audio);
		assert_eq!(clock.state.lock().unwrap().pending(), 0);
	}

	#[tokio::test]
	async fn a_data_track_joins_a_paired_restart() {
		tokio::time::pause();
		let now = kio::clock::now();
		let (clock, audio, video) = anchored(now);
		let data = Track::new(clock.clone(), Kind::Data);
		let transition = audio.declare();
		assert_eq!(video.declare(), transition);
		assert_eq!(data.declare(), transition, "one restart");
		audio.plan(transition, ms(0), Some(DELAY));
		video.plan(transition, ms(40), None);
		tokio::time::advance(DELAY).await;
		video.apply(
			transition,
			video.resume(transition, ms(40), None).ready().await.unwrap(),
		);
		// The data track has no replacement yet, so the others map alone.
		assert_eq!(video.map(ms(40)).unwrap().as_millis(), 10_100);
		data.plan(transition, ms(20), None);
		data.apply(transition, data.resume(transition, ms(20), None).ready().await.unwrap());
		assert_eq!(data.map(ms(40)).unwrap(), video.map(ms(40)).unwrap());
	}

	#[tokio::test]
	async fn one_ended_role_maps_to_the_continuous_peers_clock() {
		tokio::time::pause();
		let start = kio::clock::now();
		let clock = Arc::new(Clock::new(DELAY));
		let audio = Track::new(clock.clone(), Kind::Audio);
		let video = Track::new(clock.clone(), Kind::Video);
		video.received(ms(10_000), start);
		let transition = audio.declare();
		audio.end(ms(10_020)).unwrap();
		tokio::time::advance(DELAY + Duration::from_millis(20)).await;
		let unchanged = clock.state.lock().unwrap().due(ms(10_500));
		audio.apply(
			transition,
			audio.resume(transition, ms(0), Some(DELAY)).ready().await.unwrap(),
		);
		assert_eq!(audio.map(ms(0)).unwrap().as_millis(), 10_120);
		assert_eq!(video.map(ms(10_500)).unwrap().as_millis(), 10_500);
		assert_eq!(clock.state.lock().unwrap().due(ms(10_500)), unchanged);
	}

	/// A replacement that arrived while the old tail played waits for it, so it
	/// plays later than it arrived. Its arrivals are on time for that schedule,
	/// not early: they must not drag the other tracks ahead.
	#[tokio::test]
	async fn media_held_behind_a_tail_leaves_the_schedule_alone() {
		tokio::time::pause();
		let start = kio::clock::now();
		let (clock, audio, video) = anchored(start);
		video.received(ms(10_000), start);
		let transition = audio.declare();
		audio.end(ms(10_100)).unwrap();
		// The replacement starts over at zero and lands at once.
		audio.plan(transition, ms(0), Some(DELAY));
		let mapping = audio.resume(transition, ms(0), Some(DELAY)).ready().await.unwrap();
		audio.apply(transition, mapping);
		assert!(
			kio::clock::now() >= start.saturating_add(DELAY),
			"the replacement waited until one delay before the tail ends"
		);
		let unchanged = clock.state.lock().unwrap().due(ms(10_500));
		for frame in 0..10 {
			let at = start.saturating_add(Duration::from_millis(frame * 20));
			assert!(
				!audio.received(audio.map(ms(frame * 20)).unwrap(), at),
				"held media moved the schedule"
			);
		}
		assert_eq!(clock.state.lock().unwrap().due(ms(10_500)), unchanged);
	}

	/// Media after a reset was not held behind the tail, so it plays one delay
	/// after it arrives rather than waiting out the old hold too.
	#[tokio::test]
	async fn a_reset_drops_the_hold_behind_a_tail() {
		tokio::time::pause();
		let start = kio::clock::now();
		let clock = Arc::new(Clock::new(DELAY));
		let audio = Track::new(clock.clone(), Kind::Audio);
		audio.received(ms(10_000), start);
		let transition = audio.declare();
		audio.end(ms(10_100)).unwrap();
		audio.plan(transition, ms(0), Some(DELAY));
		let mapping = audio.resume(transition, ms(0), Some(DELAY)).ready().await.unwrap();
		audio.apply(transition, mapping);

		audio.reset();
		let now = kio::clock::now();
		let next = audio.map(ms(1_000)).unwrap();
		audio.received(next, now);
		assert_eq!(clock.state.lock().unwrap().due(next), now.checked_add(DELAY));
	}

	/// A press arriving while the previous one still plays continues its
	/// schedule, so back-to-back presses add no delay; one after a gap
	/// re-buffers one delay.
	#[tokio::test]
	async fn back_to_back_segments_keep_their_delay() {
		tokio::time::pause();
		let start = kio::clock::now();
		let clock = Arc::new(Clock::new(DELAY));
		let audio = Track::new(clock.clone(), Kind::Audio);
		audio.received(ms(0), start);
		for press in 1..=5u64 {
			// Each press lasts 1 s, and the next arrives as it ends.
			let transition = audio.declare();
			audio.end(ms(1_000)).unwrap();
			tokio::time::advance(start.saturating_add(Duration::from_secs(press)) - kio::clock::now()).await;
			let arrived = kio::clock::now();
			audio.plan(transition, ms(0), Some(DELAY));
			let mapping = audio.resume(transition, ms(0), Some(DELAY)).ready().await.unwrap();
			audio.apply(transition, mapping);
			// Each press starts one tick past the tail it touches.
			assert_eq!(
				clock.state.lock().unwrap().due(audio.map(ms(0)).unwrap()),
				arrived.checked_add(DELAY + TICK * press as u32),
				"press {press} drifted"
			);
		}

		// After a gap, the tail has drained: it re-buffers one delay from now.
		let transition = audio.declare();
		audio.end(ms(1_000)).unwrap();
		tokio::time::advance(Duration::from_secs(5)).await;
		let arrived = kio::clock::now();
		audio.plan(transition, ms(0), Some(DELAY));
		let mapping = audio.resume(transition, ms(0), Some(DELAY)).ready().await.unwrap();
		audio.apply(transition, mapping);
		assert_eq!(
			clock.state.lock().unwrap().due(audio.map(ms(0)).unwrap()),
			arrived.checked_add(DELAY)
		);
	}

	#[tokio::test]
	async fn a_late_paired_replacement_keeps_its_media_offset() {
		tokio::time::pause();
		let now = kio::clock::now();
		let (_clock, audio, video) = anchored(now);
		let transition = audio.declare();
		assert_eq!(video.declare(), transition, "one restart");
		audio.end(ms(10_020)).unwrap();
		video.end(ms(10_020)).unwrap();
		audio.plan(transition, ms(0), Some(DELAY));
		tokio::time::advance(DELAY + Duration::from_millis(20)).await;
		audio.apply(
			transition,
			audio.resume(transition, ms(0), Some(DELAY)).ready().await.unwrap(),
		);
		// The video replacement lands after the resumed audio has started playing.
		tokio::time::advance(DELAY * 2).await;
		video.plan(transition, ms(40), None);
		video.apply(
			transition,
			video.resume(transition, ms(40), None).ready().await.unwrap(),
		);
		assert_eq!(
			video.map(ms(40)).unwrap().as_millis() - audio.map(ms(0)).unwrap().as_millis(),
			40
		);
	}

	#[tokio::test]
	async fn a_late_replacement_pairs_with_its_own_transition() {
		tokio::time::pause();
		let now = kio::clock::now();
		let (_clock, audio, video) = anchored(now);
		let transition = audio.declare();
		video.declare();
		audio.end(ms(10_020)).unwrap();
		video.end(ms(10_020)).unwrap();
		video.plan(transition, ms(40), None);
		tokio::time::advance(DELAY + Duration::from_millis(20)).await;
		video.apply(
			transition,
			video.resume(transition, ms(40), None).ready().await.unwrap(),
		);
		// Video already queued its next replacement when audio's first one lands.
		let next = video.declare();
		assert_ne!(next, transition);
		video.end(ms(60)).unwrap();
		video.plan(next, ms(5_000), None);
		audio.plan(transition, ms(0), Some(DELAY));
		audio.apply(
			transition,
			audio.resume(transition, ms(0), Some(DELAY)).ready().await.unwrap(),
		);
		assert_eq!(
			video.map(ms(40)).unwrap().as_millis() - audio.map(ms(0)).unwrap().as_millis(),
			40
		);
	}

	#[tokio::test]
	async fn a_peer_two_restarts_ahead_pairs_with_the_first() {
		tokio::time::pause();
		let now = kio::clock::now();
		let (_clock, audio, video) = anchored(now);
		let transition = audio.declare();
		video.declare();
		audio.end(ms(10_020)).unwrap();
		video.end(ms(10_020)).unwrap();
		tokio::time::advance(DELAY + Duration::from_millis(20)).await;
		video.plan(transition, ms(40), None);
		let first = video.resume(transition, ms(40), None).ready().await.unwrap();
		video.apply(transition, first);
		// Video restarts again and resumes before audio's first replacement lands.
		let next = video.declare();
		video.plan(next, ms(5_000), None);
		tokio::time::advance(Duration::from_millis(10)).await;
		video.apply(next, video.resume(next, ms(5_000), None).ready().await.unwrap());
		audio.plan(transition, ms(0), Some(DELAY));
		audio.apply(
			transition,
			audio.resume(transition, ms(0), Some(DELAY)).ready().await.unwrap(),
		);
		assert_eq!(
			first.map(ms(40)).unwrap().as_millis() - audio.map(ms(0)).unwrap().as_millis(),
			40
		);
	}

	#[tokio::test]
	async fn an_abandoned_replacement_leaves_the_next_transition_alone() {
		tokio::time::pause();
		let now = kio::clock::now();
		let (clock, audio, video) = anchored(now);
		let transition = audio.declare();
		video.declare();
		audio.end(ms(10_020)).unwrap();
		video.end(ms(10_020)).unwrap();
		// Video's first replacement is evicted before it plays, then it restarts again.
		video.plan(transition, ms(40), None);
		video.abandon(transition);
		let next = video.declare();
		video.plan(next, ms(5_000), None);
		audio.plan(transition, ms(0), Some(DELAY));
		tokio::time::advance(DELAY + Duration::from_millis(20)).await;
		audio.apply(
			transition,
			audio.resume(transition, ms(0), Some(DELAY)).ready().await.unwrap(),
		);
		video.apply(next, video.resume(next, ms(5_000), None).ready().await.unwrap());
		// The later replacement maps onto the running clock, not the audio
		// restart's, continuing as the tail ends since it arrived before then.
		let resumed = kio::clock::now().saturating_add(TICK);
		assert_eq!(
			clock.state.lock().unwrap().due(video.map(ms(5_000)).unwrap()),
			Some(resumed)
		);
	}

	#[tokio::test]
	async fn an_abandoned_peer_plan_is_not_paired() {
		tokio::time::pause();
		let now = kio::clock::now();
		let (clock, audio, video) = anchored(now);
		let transition = audio.declare();
		video.declare();
		audio.end(ms(10_020)).unwrap();
		video.end(ms(10_020)).unwrap();
		audio.plan(transition, ms(0), Some(Duration::from_millis(80)));
		audio.abandon(transition);
		video.plan(transition, ms(40), None);
		tokio::time::advance(DELAY + Duration::from_millis(20)).await;
		video.apply(
			transition,
			video.resume(transition, ms(40), None).ready().await.unwrap(),
		);
		// Video arrived before the tail ended, so it continues as the tail ends.
		let resumed = kio::clock::now().saturating_add(TICK);
		assert_eq!(
			clock.state.lock().unwrap().due(video.map(ms(40)).unwrap()),
			Some(resumed)
		);
	}

	#[tokio::test]
	async fn a_resumed_transition_ignores_the_peers_later_end() {
		tokio::time::pause();
		let now = kio::clock::now();
		let (_clock, audio, video) = anchored(now);
		let transition = audio.declare();
		video.declare();
		audio.end(ms(10_020)).unwrap();
		video.end(ms(10_020)).unwrap();
		video.plan(transition, ms(40), None);
		tokio::time::advance(DELAY + Duration::from_millis(20)).await;
		let mapping = video.resume(transition, ms(40), None).ready().await.unwrap();
		video.apply(transition, mapping);
		// The resumed video plays, then its encoder stops again far ahead.
		tokio::time::advance(DELAY * 2).await;
		video.declare();
		video.end(ms(60_000)).unwrap();
		audio.plan(transition, ms(0), Some(DELAY));
		let resumed = kio::clock::timeout(
			Duration::from_millis(1),
			audio.resume(transition, ms(0), Some(DELAY)).ready(),
		)
		.await;
		assert!(resumed.is_ok(), "audio waited for video's next end");
	}

	#[tokio::test]
	async fn a_cancelled_role_releases_its_declared_end() {
		tokio::time::pause();
		let now = kio::clock::now();
		let (_clock, audio, video) = anchored(now);
		// Video holds the schedule once audio goes.
		video.received(ms(10_000), now);
		audio.declare();
		audio.end(ms(10_200)).unwrap();
		// Cancelled with its sink, so nothing plays the tail.
		drop(audio);
		let transition = video.declare();
		video.end(ms(10_000)).unwrap();
		video.plan(transition, ms(0), None);
		let resume = kio::clock::timeout(
			DELAY + Duration::from_millis(150),
			video.resume(transition, ms(0), None).ready(),
		)
		.await;
		assert!(resume.is_ok(), "video waited for discarded audio");
	}

	#[tokio::test]
	async fn a_closed_role_still_holds_its_declared_end() {
		tokio::time::pause();
		let now = kio::clock::now();
		let (_clock, audio, video) = anchored(now);
		audio.declare();
		audio.end(ms(10_200)).unwrap();
		// The track closed, but its returned sink still plays the tail up to the end.
		audio.retire(Some(now.saturating_add(Duration::from_millis(300))));
		let transition = video.declare();
		video.end(ms(10_000)).unwrap();
		video.plan(transition, ms(0), None);
		// It may map one delay before the tail ends, so its media plays as the tail ends.
		let resume = kio::clock::timeout(
			Duration::from_millis(150),
			video.resume(transition, ms(0), None).ready(),
		)
		.await;
		assert!(resume.is_err(), "video resumed before the audio tail played");
	}

	#[tokio::test]
	async fn a_retired_end_ignores_a_later_re_pin() {
		tokio::time::pause();
		let now = kio::clock::now();
		let (clock, audio, video) = anchored(now);
		audio.declare();
		audio.end(ms(10_200)).unwrap();
		audio.retire(Some(now.saturating_add(Duration::from_millis(300))));
		// A replacement rendition starts over at zero and anchors afresh.
		let successor = Track::new(clock.clone(), Kind::Audio);
		successor.received(ms(50), now);
		let transition = video.declare();
		video.plan(transition, ms(0), None);
		let resume = kio::clock::timeout(
			Duration::from_millis(400),
			video.resume(transition, ms(0), None).ready(),
		)
		.await;
		assert!(resume.is_ok(), "video waited past the drained tail");
	}

	#[tokio::test]
	async fn a_cancelled_successor_keeps_a_retired_tail() {
		tokio::time::pause();
		let now = kio::clock::now();
		let (clock, audio, video) = anchored(now);
		audio.declare();
		audio.end(ms(10_200)).unwrap();
		audio.retire(Some(now.saturating_add(Duration::from_millis(300))));
		// The next rendition fails while the retired sink still plays.
		let successor = Track::new(clock.clone(), Kind::Audio);
		successor.declare();
		drop(successor);
		let transition = video.declare();
		video.plan(transition, ms(0), None);
		let resume = kio::clock::timeout(
			Duration::from_millis(150),
			video.resume(transition, ms(0), None).ready(),
		)
		.await;
		assert!(resume.is_err(), "video resumed over the retired audio tail");
	}

	#[tokio::test]
	async fn paired_plans_keep_media_offsets_and_follow_the_earliest_plan() {
		tokio::time::pause();
		let now = kio::clock::now();
		let (clock, audio, video) = anchored(now);
		let transition = audio.declare();
		video.declare();
		audio.end(ms(10_020)).unwrap();
		video.end(ms(10_020)).unwrap();
		audio.plan(transition, ms(0), Some(Duration::from_millis(80)));
		video.plan(transition, ms(40), None);
		tokio::time::advance(DELAY + Duration::from_millis(20)).await;
		video.apply(
			transition,
			video.resume(transition, ms(40), None).ready().await.unwrap(),
		);
		audio.apply(
			transition,
			audio
				.resume(transition, ms(0), Some(Duration::from_millis(80)))
				.ready()
				.await
				.unwrap(),
		);
		assert_eq!(
			video.map(ms(40)).unwrap().as_millis() - audio.map(ms(0)).unwrap().as_millis(),
			40
		);
		assert_eq!(
			clock.state.lock().unwrap().due(audio.map(ms(0)).unwrap()),
			// The pair arrived before the tail ended, so it continues as the tail ends.
			Some(now.saturating_add(Duration::from_millis(120) + TICK))
		);
	}

	/// Counts the wakes a parked poll receives.
	struct Wakes(std::sync::atomic::AtomicUsize);

	impl std::task::Wake for Wakes {
		fn wake(self: Arc<Self>) {
			self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
		}
	}

	impl Wakes {
		fn waiter() -> (Arc<Self>, kio::Waiter) {
			let wakes = Arc::new(Self(Default::default()));
			let waiter = kio::Waiter::new(std::task::Waker::from(wakes.clone()));
			(wakes, waiter)
		}

		fn count(&self) -> usize {
			self.0.load(std::sync::atomic::Ordering::Relaxed)
		}
	}

	/// Parks a waiter on the current schedule, applies `change`, and checks that
	/// the waiter woke and resolved to the schedule the change left behind.
	fn wakes_on(reader: &Consumer, change: impl FnOnce()) -> Schedule {
		let seen = reader.schedule();
		let (wakes, waiter) = Wakes::waiter();
		assert!(reader.poll_changed(&waiter, seen).is_pending());
		change();
		assert!(wakes.count() > 0, "the change left the waiter asleep");
		let Poll::Ready(moved) = reader.poll_changed(&waiter, seen) else {
			panic!("the change did not resolve the wait");
		};
		assert_ne!(moved, seen);
		assert_eq!(moved, reader.schedule());
		moved
	}

	/// Every way a due time can move wakes a parked reader.
	#[test]
	fn the_change_waiter_wakes_on_every_schedule_move() {
		let start = Instant::ZERO.saturating_add(Duration::from_secs(1));
		let clock = Producer::new(DELAY);
		let reader = clock.consume();
		let audio = clock.track(Kind::Audio);
		let video = clock.track(Kind::Video);

		let anchored = wakes_on(&reader, || assert!(video.received(ms(0), start)));
		assert_eq!(anchored.due(ms(0)), Some(start.saturating_add(DELAY)));

		// Audio arrives 20ms ahead of video's schedule and re-pins it.
		let pinned = wakes_on(&reader, || {
			assert!(audio.received(ms(40), start.saturating_add(Duration::from_millis(20))))
		});
		assert_eq!(
			pinned.due(ms(40)),
			Some(start.saturating_add(Duration::from_millis(20) + DELAY))
		);

		let deeper = wakes_on(&reader, || assert!(video.set_target(Duration::from_millis(300))));
		assert_eq!(deeper.delay(), Duration::from_millis(300));

		// Releasing the deepest target falls back to the default delay.
		let shallower = wakes_on(&reader, || drop(video));
		assert_eq!(shallower.delay(), DELAY);

		// The only anchored track breaks its timeline: nothing is scheduled.
		let reset = wakes_on(&reader, || audio.reset());
		assert_eq!(reset.due(ms(0)), None);

		// A producer reset onto a clock with another delay moves readers onto it.
		let replaced = wakes_on(&reader, || drop(clock.reset(Duration::from_millis(250))));
		assert_eq!(replaced.delay(), Duration::from_millis(250));
	}

	/// A wait resolves only when a due time moves, not on every signal the
	/// clock raises for its transitions.
	#[test]
	fn the_change_waiter_ignores_what_leaves_the_schedule_alone() {
		let start = Instant::ZERO.saturating_add(Duration::from_secs(1));
		let clock = Producer::new(DELAY);
		let reader = clock.consume();
		let audio = clock.track(Kind::Audio);
		let video = clock.track(Kind::Video);
		video.received(ms(0), start);
		assert!(audio.set_target(Duration::from_millis(200)));

		let seen = reader.schedule();
		let (wakes, waiter) = Wakes::waiter();
		assert!(reader.poll_changed(&waiter, seen).is_pending());

		// A late arrival and a shallower target raise no signal at all.
		assert!(!video.received(ms(40), start.saturating_add(Duration::from_millis(60))));
		assert!(!video.set_target(Duration::from_millis(150)));
		assert_eq!(wakes.count(), 0, "an unmoved schedule woke the waiter");

		// Declaring a restart and its end signals the transition's resume, but
		// the schedule stays put, so the reader parks again.
		let transition = video.declare();
		video.end(ms(1_000)).unwrap();
		video.plan(transition, ms(0), None);
		assert!(reader.poll_changed(&waiter, seen).is_pending());

		// A target that moves and moves back before the reader looks is no change.
		assert!(audio.set_target(Duration::from_millis(400)));
		assert!(audio.set_target(Duration::from_millis(200)));
		assert!(reader.poll_changed(&waiter, seen).is_pending());

		// The reference passing to another anchor on the same line, as the track
		// holding it goes and then as the last one leaves its schedule behind,
		// moves no due time.
		let data = clock.track(Kind::Data);
		assert!(!data.received(ms(40), start.saturating_add(Duration::from_millis(40))));
		drop(video);
		assert!(reader.poll_changed(&waiter, seen).is_pending());
		drop(data);
		assert!(reader.poll_changed(&waiter, seen).is_pending());
	}

	/// A producer reset onto a clock that schedules the same way leaves the
	/// reader waiting, now on the new clock.
	#[test]
	fn the_change_waiter_follows_a_producer_reset() {
		let start = Instant::ZERO.saturating_add(Duration::from_secs(1));
		let clock = Producer::new(DELAY);
		let reader = clock.consume();
		let seen = reader.schedule();
		let (wakes, waiter) = Wakes::waiter();
		assert!(reader.poll_changed(&waiter, seen).is_pending());

		let clock = clock.reset(DELAY);
		assert!(wakes.count() > 0, "the reset left the reader on the old clock");
		assert!(reader.poll_changed(&waiter, seen).is_pending());

		let woken = wakes.count();
		let video = clock.track(Kind::Video);
		assert!(video.received(ms(0), start));
		assert!(wakes.count() > woken, "the reader still waits on the old clock");
		assert_eq!(reader.poll_changed(&waiter, seen), Poll::Ready(reader.schedule()));
	}

	/// A stale clone's reset replaces the clock readers follow, not its own, and
	/// wakes them on that one.
	#[test]
	fn the_change_waiter_follows_a_stale_clone_reset() {
		let clock = Producer::new(DELAY);
		let stale = clock.clone();
		let reader = clock.consume();
		let _current = clock.reset(DELAY);
		let seen = reader.schedule();
		let (wakes, waiter) = Wakes::waiter();
		assert!(reader.poll_changed(&waiter, seen).is_pending());

		let replaced = stale.reset(Duration::from_millis(300));
		assert!(wakes.count() > 0, "the reset left the reader on the clock it replaced");
		let Poll::Ready(moved) = reader.poll_changed(&waiter, seen) else {
			panic!("the reset did not resolve the wait");
		};
		assert_eq!(moved.delay(), Duration::from_millis(300));
		drop(replaced);
	}

	/// One move wakes every reader of the clock, clones and separate consumers alike.
	#[test]
	fn the_change_waiter_wakes_every_reader() {
		let start = Instant::ZERO.saturating_add(Duration::from_secs(1));
		let clock = Producer::new(DELAY);
		let first = clock.consume();
		let readers = [first.clone(), first, clock.consume()];
		let video = clock.track(Kind::Video);
		let seen = readers[0].schedule();
		let waiters: Vec<_> = readers
			.iter()
			.map(|reader| {
				let (wakes, waiter) = Wakes::waiter();
				assert!(reader.poll_changed(&waiter, seen).is_pending());
				(wakes, waiter)
			})
			.collect();

		video.received(ms(0), start);
		for (reader, (wakes, waiter)) in readers.iter().zip(&waiters) {
			assert!(wakes.count() > 0, "a reader slept through the move");
			assert_eq!(reader.poll_changed(waiter, seen), Poll::Ready(reader.schedule()));
		}
	}

	/// Dropping a wait loses nothing: a move made while no wait is pending
	/// resolves the next one at once.
	#[kio::test]
	async fn a_dropped_change_wait_keeps_the_move() {
		let clock = Producer::new(DELAY);
		let reader = clock.consume();
		let video = clock.track(Kind::Video);
		video.received(ms(0), kio::clock::now());
		let seen = reader.schedule();
		{
			let mut wait = std::pin::pin!(reader.changed(seen));
			assert!(futures::poll!(wait.as_mut()).is_pending());
		}

		assert!(video.set_target(Duration::from_millis(300)));
		let moved = reader.changed(seen).await;
		assert_eq!(moved.delay(), Duration::from_millis(300));
		assert_eq!(moved, reader.schedule());
	}

	/// A presenter asleep on a picture's due time wakes when the delay drops,
	/// and presents at the new due time instead of the one it slept on.
	#[kio::test]
	async fn a_presenter_reschedules_when_the_delay_moves() {
		use futures::future::{Either, select};

		let clock = Producer::new(Duration::from_millis(500));
		let reader = clock.consume();
		let video = clock.track(Kind::Video);
		let start = kio::clock::now();
		video.received(ms(0), start);

		let present = async {
			let mut schedule = reader.schedule();
			loop {
				let due = schedule.due(ms(0)).expect("anchored");
				let sleep = std::pin::pin!(kio::clock::sleep_until(due));
				let moved = std::pin::pin!(reader.changed(schedule));
				match select(sleep, moved).await {
					Either::Left(_) => return kio::clock::now(),
					Either::Right((moved, _)) => schedule = moved,
				}
			}
		};
		let lower = async {
			kio::clock::sleep(Duration::from_millis(100)).await;
			assert!(video.set_target(Duration::from_millis(200)));
		};
		let (presented, ()) = futures::join!(present, lower);
		assert_eq!(presented, start.saturating_add(Duration::from_millis(200)));
	}
}
