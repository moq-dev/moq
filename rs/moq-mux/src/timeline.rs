//! One playout clock shared by a broadcast's audio and video.
//!
//! Feed video arrivals and the audio device's buffered position to a [`Producer`].
//! Give its [`Consumer`] to a presenter: [`Consumer::present`] selects the newest
//! due picture without waiting, so a canvas can repeat its previous picture.
//! Audio owns the anchor while it plays; without audio, video uses the delay
//! passed to [`Producer::new`]. The audio jitter target comes from the caller,
//! keeping this module independent of decoders and output devices.
//!
//! A [`Track`] maps encoder segments onto the clock across declared ends. Keep
//! one active track per [`Kind`]; retire it before starting its successor. Bound
//! replacement media with the caller's retention budget and [`Track::abandon`]
//! transitions whose replacement is discarded.

use kio::clock::Instant;
use std::time::Duration;

use moq_net::Timestamp;
use std::{
	pin::Pin,
	sync::{Arc, Mutex},
	task::Poll,
};

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

/// The playout clock: one anchor the window and the speaker both present
/// against.
///
/// Every arriving frame is folded into a single [`Pacer`](crate::Pacer), which
/// maps media time onto the wall clock and holds each result back by the
/// configured delay. That is what pulls playback toward live: a frame arriving
/// earlier than the anchor predicted re-pins it, and the delay is measured from
/// the edge it just set rather than from wherever the first frame happened to
/// land.
///
/// While audio is playing it is the only half allowed to move the anchor. The
/// speaker drains on its own clock and cannot skip forward with a re-anchor, so
/// a video tune-in burst re-anchoring would leave the picture ahead of the
/// sound. Video reads the anchor and follows it.
struct State {
	pacer: crate::Pacer,
	/// How far behind the live edge playback runs.
	delay: Duration,
	/// Whether the speaker owns the anchor.
	speaker: bool,
	/// Set when the speaker restarts on a new timeline mid-track, so the next
	/// audio frame pins the anchor outright instead of pacing against media it
	/// will never play.
	restart: bool,
	/// Endpoints declared by each live role, on the shared playback timeline.
	ends: [Option<Boundary>; 2],
	/// When each retired role's tail finishes playing, frozen on the wall clock:
	/// a later rendition re-pins the anchor, which would carry a media endpoint
	/// onto an unrelated timeline. Kept apart from `ends` so a successor role
	/// never clears its predecessor's tail.
	tails: Vec<Instant>,
	/// Encoder restarts whose replacements have not all started playing.
	transitions: std::collections::VecDeque<Pending>,
	/// The latest transition each role declared.
	declared: [Transition; 2],
	next: Transition,
	reference: Option<Timestamp>,
	/// The last speaker write: the media time it ends at, when that sounds, and
	/// the playhead it may not fall back behind on the same timeline.
	written: Option<(Duration, Instant, Option<Duration>)>,
	/// The playout target the speaker holds, once audio is playing.
	target: Option<Duration>,
}

/// One encoder restart, shared by the roles whose ends declared it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Transition(u64);

struct Pending {
	id: Transition,
	/// Roles whose declared end opened or joined this transition.
	members: [bool; 2],
	/// Members that applied its mapping.
	resumed: [bool; 2],
	/// Each member's first replacement media.
	plans: [Option<Plan>; 2],
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
}

impl State {
	fn new(delay: Duration) -> Self {
		Self {
			pacer: crate::Pacer::default().with_delay(delay),
			delay,
			speaker: false,
			restart: false,
			ends: Default::default(),
			tails: Vec::new(),
			transitions: Default::default(),
			declared: [Transition(0); 2],
			next: Transition(1),
			reference: None,
			written: None,
			target: None,
		}
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
			let resumed = (0..2).all(|role| !pending.members[role] || pending.resumed[role]);
			let started = pending.mapping.is_some_and(|mapping| mapping.started(now));
			members && !(resumed && started)
		});
	}

	/// The media position the shared clock will reach at `at`.
	fn position(&self, at: Instant) -> Option<Duration> {
		let reference = self.reference?;
		let due = self.due(reference)?;
		let reference = Duration::from(reference);
		Some(if at >= due {
			reference.saturating_add(at.duration_since(due))
		} else {
			reference.saturating_sub(due.duration_since(at))
		})
	}

	/// Fold an arriving video frame into the anchor, unless the speaker owns it,
	/// reporting whether that moved the schedule.
	///
	/// A move makes every queued frame due earlier, so the window has to recompute
	/// the deadline it is already asleep on.
	fn video(&mut self, timestamp: Timestamp, now: Instant) -> bool {
		if self.speaker {
			return false;
		}

		self.reference = Some(timestamp);
		let before = self.pacer.due(timestamp);
		before != Some(self.pacer.pace(timestamp, now))
	}

	/// Start a new video timeline without taking ownership from a speaker.
	fn video_restarted(&mut self) {
		if !self.speaker {
			self.pacer = crate::Pacer::default().with_delay(self.delay);
		}
	}

	/// Fold the speaker's position into the anchor, reporting whether that moved
	/// the schedule.
	///
	/// `end` is the media time of the last sample written and `buffered` is how
	/// much of the write is still queued, so `end` does not sound until
	/// `buffered` from now. The anchor is pinned at the instant that leaves
	/// exactly the delay before it does, which is where the live edge sits on a
	/// clock trailing by that delay. Pinning the sample sounding *now* instead
	/// would schedule video a whole delay behind the speaker.
	///
	/// A move makes every queued video frame due earlier, so the window has to
	/// recompute the deadline it is already asleep on.
	fn audio(&mut self, end: Duration, buffered: Duration, now: Instant) -> bool {
		// Silence padded ahead of the frame to refill the speaker is not media: the
		// playhead holds where it was until the padding has played, rather than
		// projecting back over media already heard. A new timeline starts afresh.
		let floor = (self.speaker && !self.restart).then(|| self.playhead(now)).flatten();
		self.written = now.checked_add(buffered).map(|sounds| (end, sounds, floor));
		let timestamp = media(end);
		self.reference = Some(timestamp);
		let edge = now
			.checked_add(buffered)
			.and_then(|at| at.checked_sub(self.delay))
			.unwrap_or(now);

		// Taking ownership re-pins outright rather than pacing. Whatever set the
		// anchor before is either video, which the speaker cannot follow, or a
		// track that has since ended, and a timestamp that maps into the past under
		// that anchor would never re-anchor on its own: the sink would play now
		// while the picture stayed on a timeline nothing is feeding.
		let before = self.pacer.due(timestamp);
		let after = if !self.speaker || std::mem::take(&mut self.restart) {
			self.pacer.hurry(timestamp, edge)
		} else {
			self.pacer.pace(timestamp, edge)
		};
		self.speaker = true;

		before != Some(after)
	}

	/// The speaker restarted on a new timeline, so the media either side of the
	/// break is unrelated: re-pin the anchor on the next audio frame rather than
	/// pacing across a jump the speaker never plays.
	fn restarted(&mut self) {
		self.restart = true;
	}

	/// The speaker went quiet, so nothing holds playback to its cadence any
	/// more and video takes the anchor back.
	///
	/// A replacement rendition takes ownership again on its first frame, and
	/// re-pins there: it is a track boundary, so its timestamps are not
	/// necessarily continuous with what just ended. That covers a pending
	/// restart, which would otherwise re-pin the frame after it too.
	fn stopped(&mut self) {
		self.speaker = false;
		self.restart = false;
		self.written = None;
		self.target = None;
	}

	/// Record the playout target the speaker is being held on.
	fn hold(&mut self, target: Duration) {
		self.target = Some(target);
	}

	/// The media time sounding at `now`, or `None` with no speaker.
	///
	/// Projected from the last write at the wall rate, held at its end (a speaker
	/// with nothing left plays nothing further), and never behind where it stood
	/// at that write (refill padding plays no media).
	fn playhead(&self, now: Instant) -> Option<Duration> {
		let (end, sounds, floor) = self.written?;
		let projected = end.saturating_sub(sounds.saturating_duration_since(now));
		Some(floor.map_or(projected, |floor| projected.max(floor)).min(end))
	}

	/// The playout target the speaker is held on, or `None` with no speaker.
	fn target(&self) -> Option<Duration> {
		self.target
	}

	/// When `timestamp` should be presented, or `None` before any frame has
	/// anchored the clock. `None` means as soon as possible: there is nothing
	/// left to wait for.
	fn due(&self, timestamp: Timestamp) -> Option<Instant> {
		self.pacer.due(timestamp)
	}
}

/// Maps each encoder segment onto the shared playback clock.
pub struct Track {
	presentation: Arc<Clock>,
	role: Kind,
	mapping: std::sync::Mutex<Mapping>,
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
	fn new(presentation: Arc<Clock>, role: Kind) -> Self {
		Self {
			presentation,
			role,
			mapping: std::sync::Mutex::new(Mapping {
				source: Duration::ZERO,
				target: Duration::ZERO,
				starts: None,
			}),
		}
	}

	/// Stop while a tail still plays until `drained`, holding a declared end until
	/// then. Dropping instead discards the tail with the end.
	pub fn retire(self, drained: Option<Instant>) {
		let now = kio::clock::now();
		let mut presentation = self.presentation.state.lock().unwrap();
		presentation.tails.retain(|tail| *tail > now);
		if presentation.ends[self.role as usize].take().is_some()
			&& let Some(drained) = drained
		{
			presentation.tails.push(drained);
		}
	}

	/// Map a source timestamp onto the shared broadcast timeline.
	pub fn map(&self, source: Timestamp) -> Result<Timestamp> {
		self.mapping.lock().unwrap().map(source)
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
		let mut presentation = self.presentation.state.lock().unwrap();
		presentation.prune(now);
		let after = presentation.declared[own];
		let joined = presentation
			.transitions
			.iter_mut()
			.filter(|transition| transition.id > after)
			.find(|transition| {
				transition.members[1 - own]
					&& !transition.members[own]
					&& transition.mapping.is_none_or(|mapping| !mapping.started(now))
			})
			.map(|transition| {
				transition.members[own] = true;
				transition.id
			});
		let id = joined.unwrap_or_else(|| {
			let id = presentation.next;
			presentation.next = Transition(id.0 + 1);
			let mut members = [false; 2];
			members[own] = true;
			presentation.transitions.push_back(Pending {
				id,
				members,
				resumed: [false; 2],
				plans: [None, None],
				mapping: None,
			});
			id
		});
		presentation.declared[own] = id;
		drop(presentation);
		self.presentation.wake();
		id
	}

	/// Mark where this role's current segment ends on the shared clock.
	pub fn end(&self, source: Timestamp) -> Result<()> {
		let mapping = *self.mapping.lock().unwrap();
		self.presentation.state.lock().unwrap().ends[self.role as usize] = Some(Boundary {
			starts: mapping.starts,
			end: Duration::from(mapping.map(source)?),
		});
		self.presentation.wake();
		Ok(())
	}

	/// Record the first replacement media for `transition`.
	pub fn plan(&self, transition: Transition, source: Timestamp, delay: Option<Duration>) {
		let mut presentation = self.presentation.state.lock().unwrap();
		let delay = delay.unwrap_or(presentation.delay);
		if let Some(pending) = presentation.transition(transition) {
			pending.plans[self.role as usize] = Some(Plan {
				source: Duration::from(source),
				delay,
			});
		}
		drop(presentation);
		self.presentation.wake();
	}

	/// Media was lost: nothing waits for the discarded tail.
	pub fn gap(&self) {
		self.presentation.state.lock().unwrap().ends[self.role as usize] = None;
		self.presentation.wake();
	}

	/// This role will never resume `transition`, so a peer must not pair with its plan.
	pub fn abandon(&self, transition: Transition) {
		let own = self.role as usize;
		let mut presentation = self.presentation.state.lock().unwrap();
		if let Some(pending) = presentation.transition(transition) {
			pending.members[own] = false;
			pending.plans[own] = None;
		}
		presentation.prune(kio::clock::now());
		drop(presentation);
		self.presentation.wake();
	}

	/// Commit the mapping once its replacement media starts playing.
	pub fn apply(&self, transition: Transition, mapping: Mapping) {
		*self.mapping.lock().unwrap() = mapping;
		let mut presentation = self.presentation.state.lock().unwrap();
		if let Some(pending) = presentation.transition(transition) {
			pending.resumed[self.role as usize] = true;
		}
		presentation.prune(kio::clock::now());
		drop(presentation);
		self.presentation.wake();
	}

	/// Prepare a mapping after the old media drains; dropping the wait leaves the track unchanged.
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
			let mut presentation = self.presentation.state.lock().unwrap();
			presentation.ends[own] = None;
			for pending in presentation
				.transitions
				.iter_mut()
				.filter(|pending| !pending.resumed[own])
			{
				pending.members[own] = false;
				pending.plans[own] = None;
			}
			presentation.declared[own] = Transition(0);
			presentation.prune(kio::clock::now());
		}
		self.presentation.wake();
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
	/// Resolve the replacement's mapping once all declared old media has drained.
	pub fn poll_ready(&mut self, waiter: &kio::Waiter) -> Poll<Result<Mapping>> {
		let _ = self.track.presentation.changed.poll(waiter, |_| Poll::Pending);
		let now = kio::clock::now();
		let mut presentation = self.track.presentation.state.lock().unwrap();
		if let Some(mapping) = presentation
			.transition(self.transition)
			.and_then(|pending| pending.mapping)
		{
			return Poll::Ready(Ok(mapping));
		}
		let end = presentation
			.ends
			.iter()
			.flatten()
			.filter(|boundary| boundary.starts.is_none_or(|start| start <= now))
			.filter_map(|boundary| presentation.due(media(boundary.end)))
			.chain(presentation.tails.iter().copied())
			.max();
		if let Some(end) = end.filter(|end| *end > now) {
			let timer = self.timer.get_or_insert_with(|| kio::clock::sleep_until(end));
			if timer.deadline() != end {
				timer.reset(end);
			}
			std::task::ready!(waiter.poll_future(Pin::new(timer)));
		}

		let peer = 1 - self.track.role as usize;
		// A transition abandoned under this resume, by a gap the caller rechecks,
		// maps alone like one the peer never joined.
		let (mapping, paired) = presentation
			.transition(self.transition)
			.map_or((None, None), |pending| {
				let both = pending.members[peer] && pending.plans.iter().all(Option::is_some);
				(pending.mapping, pending.plans[Kind::Audio as usize].filter(|_| both))
			});
		// The peer already resumed this transition.
		if let Some(mapping) = mapping {
			return Poll::Ready(Ok(mapping));
		}
		// Both roles have replacement media. The speaker's first sample defines
		// their common origin even when its adaptive delay differs from video's.
		// Otherwise this role maps alone and a peer joining later follows it.
		let (source, delay) = match paired {
			Some(audio) => (audio.source, audio.delay),
			None => (Duration::from(self.source), self.delay.unwrap_or(presentation.delay)),
		};
		let starts = now.checked_add(delay).ok_or(Error::DelayOverflow)?;
		let mapping = Mapping {
			source,
			target: presentation.position(starts).unwrap_or(source),
			starts: Some(starts),
		};
		if let Some(pending) = presentation.transition(self.transition) {
			pending.mapping = Some(mapping);
		}
		Poll::Ready(Ok(mapping))
	}

	/// Wait for the old media to drain and return the replacement's mapping.
	pub async fn ready(mut self) -> Result<Mapping> {
		kio::wait(|waiter| self.poll_ready(waiter)).await
	}
}

/// Whether a track feeds the picture or the speaker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
	/// A picture track.
	Video,
	/// A sound track.
	Audio,
}

/// A timestamp or delay cannot be represented on the playout clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
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

/// The speaker's last write and caller-selected jitter target.
#[derive(Clone, Copy, Debug)]
pub struct Audio {
	/// Media time at the end of the last write.
	pub end: Duration,
	/// How long until the end of that write sounds.
	pub buffered: Duration,
	/// The jitter target used to fill the speaker's buffer.
	pub target: Duration,
}

/// Updates one broadcast's playout clock, shared by audio and video tasks.
///
/// Clones capture this broadcast's clock; resetting a handle never changes the
/// clock its previous clones or track handles update.
pub struct Producer {
	clock: Mutex<Arc<Clock>>,
	current: Arc<Mutex<Arc<Clock>>>,
}

impl Clone for Producer {
	fn clone(&self) -> Self {
		Self {
			clock: Mutex::new(self.clock()),
			current: self.current.clone(),
		}
	}
}

/// Reads the current playout schedule without moving its anchor.
///
/// Keeps the final schedule readable after update handles drop, and follows
/// the replacement clock when a producer resets for another broadcast.
#[derive(Clone)]
pub struct Consumer {
	current: Arc<Mutex<Arc<Clock>>>,
}

impl Producer {
	/// Create a broadcast clock with this video-only delay.
	pub fn new(delay: Duration) -> Self {
		let clock = Arc::new(Clock::new(delay));
		Self {
			clock: Mutex::new(clock.clone()),
			current: Arc::new(Mutex::new(clock)),
		}
	}

	fn clock(&self) -> Arc<Clock> {
		self.clock.lock().unwrap().clone()
	}

	/// Start a new broadcast clock and switch readers to it, isolating all previous update handles.
	pub fn reset(&self, delay: Duration) {
		let clock = Arc::new(Clock::new(delay));
		*self.clock.lock().unwrap() = clock.clone();
		*self.current.lock().unwrap() = clock;
	}

	/// Return a reader that follows this handle's broadcast replacements.
	pub fn consume(&self) -> Consumer {
		Consumer {
			current: self.current.clone(),
		}
	}

	/// Return a track handle bound to this broadcast's clock.
	pub fn track(&self, kind: Kind) -> Track {
		Track::new(self.clock(), kind)
	}

	/// Fold an arriving video frame into the anchor, unless the speaker owns it,
	/// reporting whether that moved the schedule.
	///
	/// A move makes every queued frame due earlier, so the window has to recompute
	/// the deadline it is already asleep on.
	pub fn video(&self, timestamp: Timestamp, now: Instant) -> bool {
		let clock = self.clock();
		let moved = clock.state.lock().unwrap().video(timestamp, now);
		if moved {
			clock.wake();
		}
		moved
	}

	/// Start a new video timeline without taking ownership from a speaker.
	pub fn video_restarted(&self) {
		let clock = self.clock();
		let moved = {
			let mut state = clock.state.lock().unwrap();
			if state.speaker {
				false
			} else {
				state.video_restarted();
				true
			}
		};
		if moved {
			clock.wake();
		}
	}

	/// Fold the speaker's position into the anchor, reporting whether that moved
	/// the schedule.
	///
	/// `audio.end` is the media time of the last sample written and `audio.buffered` is how
	/// much of the write is still queued, so `end` does not sound until
	/// `buffered` from now. The anchor is pinned at the instant that leaves
	/// exactly the delay before it does, which is where the live edge sits on a
	/// clock trailing by that delay. Pinning the sample sounding *now* instead
	/// would schedule video a whole delay behind the speaker.
	///
	/// A move makes every queued video frame due earlier, so the window has to
	/// recompute the deadline it is already asleep on.
	pub fn audio(&self, audio: Audio, now: Instant) -> bool {
		let clock = self.clock();
		let moved = {
			let mut state = clock.state.lock().unwrap();
			state.hold(audio.target);
			state.audio(audio.end, audio.buffered, now)
		};
		if moved {
			clock.wake();
		}
		moved
	}

	/// The speaker restarted on a new timeline, so the media either side of the
	/// break is unrelated: re-pin the anchor on the next audio frame rather than
	/// pacing across a jump the speaker never plays.
	pub fn audio_restarted(&self) {
		self.clock().state.lock().unwrap().restarted()
	}

	/// The speaker went quiet, so nothing holds playback to its cadence any
	/// more and video takes the anchor back.
	///
	/// A replacement rendition takes ownership again on its first frame, and
	/// re-pins there: it is a track boundary, so its timestamps are not
	/// necessarily continuous with what just ended. That covers a pending
	/// restart, which would otherwise re-pin the frame after it too.
	pub fn audio_stopped(&self) {
		self.clock().state.lock().unwrap().stopped()
	}

	/// When `timestamp` should be presented, or `None` before any frame has
	/// anchored the clock. `None` means as soon as possible: there is nothing
	/// left to wait for.
	pub fn due(&self, timestamp: Timestamp) -> Option<Instant> {
		self.clock().state.lock().unwrap().due(timestamp)
	}
}

impl Consumer {
	/// The media time sounding now, held at the last audio write's end.
	pub fn playhead(&self, now: Instant) -> Option<Duration> {
		self.current.lock().unwrap().state.lock().unwrap().playhead(now)
	}

	/// The caller-supplied audio playout target, or `None` without audio.
	pub fn target(&self) -> Option<Duration> {
		self.current.lock().unwrap().state.lock().unwrap().target()
	}

	/// When this timestamp is due, or `None` before the first anchor.
	pub fn due(&self, timestamp: Timestamp) -> Option<Instant> {
		self.current.lock().unwrap().state.lock().unwrap().due(timestamp)
	}

	/// The newest due picture's index in presentation order, or `None` to repeat the previous one.
	///
	/// Pass queued timestamps in ascending order and discard entries through the
	/// returned index after showing that picture. This stores no frames and leaves
	/// their retention and backpressure to the caller. Query again after every
	/// anchor update; a cached deadline can become stale.
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
	#[tokio::test]
	async fn replacing_a_broadcast_isolates_its_old_update_handles() {
		tokio::time::pause();
		let clock = Producer::new(DELAY);
		let reader = clock.consume();
		let previous = clock.clone();
		let old = clock.track(Kind::Audio);
		old.declare();
		clock.reset(DELAY);
		let now = kio::clock::now();
		clock.audio(
			Audio {
				end: Duration::ZERO,
				buffered: DELAY,
				target: DELAY,
			},
			now,
		);
		let audio = clock.track(Kind::Audio);
		audio.declare();
		audio.end(ms(1_000)).unwrap();
		// A JoinSet abort can leave these updates and drops until after the new track started.
		drop(old);
		previous.audio(
			Audio {
				end: Duration::from_secs(10),
				buffered: Duration::ZERO,
				target: Duration::ZERO,
			},
			now,
		);
		assert_eq!(reader.due(ms(0)), Some(now + DELAY));
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
		use std::sync::{
			Arc,
			atomic::{AtomicUsize, Ordering},
		};
		use std::task::{Wake, Waker};
		struct Wakes(AtomicUsize);
		impl Wake for Wakes {
			fn wake(self: Arc<Self>) {
				self.0.fetch_add(1, Ordering::Relaxed);
			}
		}
		tokio::time::pause();
		let clock = Producer::new(DELAY);
		let now = kio::clock::now();
		clock.audio(
			Audio {
				end: Duration::ZERO,
				buffered: DELAY,
				target: DELAY,
			},
			now,
		);
		let audio = clock.track(Kind::Audio);
		audio.end(ms(1_000)).unwrap();
		let video = clock.track(Kind::Video);
		let transition = video.declare();
		video.plan(transition, ms(0), None);
		let wakes = Arc::new(Wakes(AtomicUsize::new(0)));
		let waiter = kio::Waiter::new(Waker::from(wakes.clone()));
		let mut resume = video.resume(transition, ms(0), None);
		assert!(resume.poll_ready(&waiter).is_pending());
		clock.video(ms(20), now);
		clock.audio(
			Audio {
				end: Duration::from_millis(20),
				buffered: DELAY,
				target: DELAY,
			},
			now + Duration::from_millis(20),
		);
		assert_eq!(wakes.0.load(Ordering::Relaxed), 0);
		clock.audio(
			Audio {
				end: Duration::from_millis(40),
				buffered: DELAY,
				target: DELAY,
			},
			now + Duration::from_millis(20),
		);
		assert!(
			wakes.0.load(Ordering::Relaxed) > 0,
			"a moved anchor must wake the resume"
		);
	}

	#[test]
	fn discarded_replacements_release_their_transition_state() {
		let presentation = Arc::new(Clock::new(Duration::ZERO));
		let video = Track::new(presentation.clone(), Kind::Video);
		let audio = Track::new(presentation.clone(), Kind::Audio);
		let first = audio.declare();
		assert_eq!(first, video.declare());
		// Audio can hold its declared end while video discards any number of replacements.
		for _ in 0..100 {
			let transition = video.declare();
			video.plan(transition, Timestamp::ZERO, None);
			video.abandon(transition);
			assert_eq!(presentation.state.lock().unwrap().pending(), 1);
		}
		video.abandon(first);
		drop(audio);
		assert_eq!(presentation.state.lock().unwrap().pending(), 0);
	}

	#[test]
	fn presentation_selects_due_frames_on_the_current_audio_anchor() {
		let clock = Producer::new(Duration::from_millis(40));
		let video = clock.consume();
		let now = Instant::now();
		clock.video(ms(0), now);
		let frames = [ms(0), ms(20), ms(40)];
		assert_eq!(video.present(frames, now), None);
		assert_eq!(video.present(frames, now + Duration::from_millis(60)), Some(1));
		// The speaker takes over with 100ms queued; stale video deadlines no longer apply.
		clock.audio(
			Audio {
				end: Duration::from_millis(40),
				buffered: Duration::from_millis(100),
				target: Duration::from_millis(100),
			},
			now,
		);
		assert_eq!(video.present(frames, now + Duration::from_millis(60)), Some(0));
		assert_eq!(video.present(frames, now + Duration::from_millis(99)), Some(1));
		assert_eq!(video.present(frames, now + Duration::from_millis(100)), Some(2));
		// Neither a stall nor a future-only queue invents a new picture.
		assert_eq!(video.present([], now + Duration::from_secs(1)), None);
		assert_eq!(video.present([ms(80)], now + Duration::from_millis(100)), None);
		clock.video(ms(1_000), now);
		assert_eq!(video.due(ms(40)), Some(now + Duration::from_millis(100)));
	}

	const DELAY: Duration = Duration::from_millis(100);

	fn ms(millis: u64) -> Timestamp {
		Timestamp::from_millis(millis).unwrap()
	}

	/// The playhead runs at the wall rate from the last write and stops at its
	/// end, which is what a speaker that ran dry is still sounding.
	#[test]
	fn the_playhead_projects_the_last_write_and_holds_at_its_end() {
		let now = Instant::now();
		let mut presentation = State::new(DELAY);
		assert_eq!(presentation.playhead(now), None);

		presentation.audio(Duration::from_secs(10), Duration::from_millis(200), now);
		presentation.hold(DELAY);
		assert_eq!(presentation.playhead(now), Some(Duration::from_millis(9_800)));
		let later = now + Duration::from_millis(150);
		assert_eq!(presentation.playhead(later), Some(Duration::from_millis(9_950)));
		let dry = now + Duration::from_secs(1);
		assert_eq!(presentation.playhead(dry), Some(Duration::from_secs(10)));
		assert_eq!(presentation.target(), Some(DELAY));

		presentation.stopped();
		assert_eq!((presentation.playhead(dry), presentation.target()), (None, None));
	}

	/// Silence padded ahead of a frame after the speaker ran dry plays no media,
	/// so the playhead holds instead of replaying media already heard.
	#[test]
	fn the_playhead_holds_through_refill_padding() {
		let now = Instant::now();
		let mut presentation = State::new(DELAY);
		presentation.audio(Duration::from_secs(10), Duration::from_millis(200), now);
		let dry = now + Duration::from_secs(1);
		assert_eq!(presentation.playhead(dry), Some(Duration::from_secs(10)));

		// 100 ms of padding, then a 20 ms frame continuing from 10 s.
		presentation.audio(Duration::from_millis(10_020), Duration::from_millis(120), dry);
		assert_eq!(presentation.playhead(dry), Some(Duration::from_secs(10)));
		let padded = dry + Duration::from_millis(100);
		assert_eq!(presentation.playhead(padded), Some(Duration::from_secs(10)));
		let played = dry + Duration::from_millis(110);
		assert_eq!(presentation.playhead(played), Some(Duration::from_millis(10_010)));

		// A new timeline may start anywhere.
		presentation.restarted();
		presentation.audio(Duration::from_secs(2), Duration::from_millis(100), played);
		assert_eq!(presentation.playhead(played), Some(Duration::from_millis(1_900)));
	}

	#[tokio::test]
	async fn one_ended_role_maps_to_the_continuous_peers_clock() {
		tokio::time::pause();
		let start = kio::clock::now();
		let presentation = Arc::new(Clock::new(DELAY));
		let audio = Track::new(presentation.clone(), Kind::Audio);
		let video = Track::new(presentation.clone(), Kind::Video);
		presentation.state.lock().unwrap().video(ms(10_000), start);
		let transition = audio.declare();
		audio.end(ms(10_020)).unwrap();
		tokio::time::advance(DELAY + Duration::from_millis(20)).await;
		let unchanged = presentation.state.lock().unwrap().due(ms(10_500));
		audio.apply(
			transition,
			audio.resume(transition, ms(0), Some(DELAY)).ready().await.unwrap(),
		);
		assert_eq!(audio.map(ms(0)).unwrap().as_millis(), 10_120);
		assert_eq!(video.map(ms(10_500)).unwrap().as_millis(), 10_500);
		assert_eq!(presentation.state.lock().unwrap().due(ms(10_500)), unchanged);
	}

	#[tokio::test]
	async fn a_late_paired_replacement_keeps_its_media_offset() {
		tokio::time::pause();
		let now = kio::clock::now();
		let presentation = Arc::new(Clock::new(DELAY));
		presentation
			.state
			.lock()
			.unwrap()
			.audio(Duration::from_secs(10), DELAY, now);
		let audio = Track::new(presentation.clone(), Kind::Audio);
		let video = Track::new(presentation.clone(), Kind::Video);
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
		let presentation = Arc::new(Clock::new(DELAY));
		presentation
			.state
			.lock()
			.unwrap()
			.audio(Duration::from_secs(10), DELAY, now);
		let audio = Track::new(presentation.clone(), Kind::Audio);
		let video = Track::new(presentation.clone(), Kind::Video);
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
		let presentation = Arc::new(Clock::new(DELAY));
		presentation
			.state
			.lock()
			.unwrap()
			.audio(Duration::from_secs(10), DELAY, now);
		let audio = Track::new(presentation.clone(), Kind::Audio);
		let video = Track::new(presentation.clone(), Kind::Video);
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
		let presentation = Arc::new(Clock::new(DELAY));
		presentation
			.state
			.lock()
			.unwrap()
			.audio(Duration::from_secs(10), DELAY, now);
		let audio = Track::new(presentation.clone(), Kind::Audio);
		let video = Track::new(presentation.clone(), Kind::Video);
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
		// The later replacement maps onto the running clock, not the audio restart's.
		let resumed = kio::clock::now() + DELAY;
		assert_eq!(
			presentation.state.lock().unwrap().due(video.map(ms(5_000)).unwrap()),
			Some(resumed)
		);
	}

	#[tokio::test]
	async fn an_abandoned_peer_plan_is_not_paired() {
		tokio::time::pause();
		let now = kio::clock::now();
		let presentation = Arc::new(Clock::new(DELAY));
		presentation
			.state
			.lock()
			.unwrap()
			.audio(Duration::from_secs(10), DELAY, now);
		let audio = Track::new(presentation.clone(), Kind::Audio);
		let video = Track::new(presentation.clone(), Kind::Video);
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
		let resumed = kio::clock::now() + DELAY;
		assert_eq!(
			presentation.state.lock().unwrap().due(video.map(ms(40)).unwrap()),
			Some(resumed)
		);
	}

	#[tokio::test]
	async fn a_resumed_transition_ignores_the_peers_later_end() {
		tokio::time::pause();
		let now = kio::clock::now();
		let presentation = Arc::new(Clock::new(DELAY));
		presentation
			.state
			.lock()
			.unwrap()
			.audio(Duration::from_secs(10), DELAY, now);
		let audio = Track::new(presentation.clone(), Kind::Audio);
		let video = Track::new(presentation.clone(), Kind::Video);
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
		let presentation = Arc::new(Clock::new(DELAY));
		presentation
			.state
			.lock()
			.unwrap()
			.audio(Duration::from_secs(10), DELAY, now);
		let audio = Track::new(presentation.clone(), Kind::Audio);
		let video = Track::new(presentation.clone(), Kind::Video);
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
		let presentation = Arc::new(Clock::new(DELAY));
		presentation
			.state
			.lock()
			.unwrap()
			.audio(Duration::from_secs(10), DELAY, now);
		let audio = Track::new(presentation.clone(), Kind::Audio);
		let video = Track::new(presentation.clone(), Kind::Video);
		audio.declare();
		audio.end(ms(10_200)).unwrap();
		// The track closed, but its returned sink still plays the tail up to the end.
		audio.retire(Some(now + Duration::from_millis(300)));
		let transition = video.declare();
		video.end(ms(10_000)).unwrap();
		video.plan(transition, ms(0), None);
		let resume = kio::clock::timeout(
			DELAY + Duration::from_millis(150),
			video.resume(transition, ms(0), None).ready(),
		)
		.await;
		assert!(resume.is_err(), "video resumed before the audio tail played");
	}

	#[tokio::test]
	async fn a_retired_end_ignores_a_later_re_pin() {
		tokio::time::pause();
		let now = kio::clock::now();
		let presentation = Arc::new(Clock::new(DELAY));
		presentation
			.state
			.lock()
			.unwrap()
			.audio(Duration::from_secs(10), DELAY, now);
		let audio = Track::new(presentation.clone(), Kind::Audio);
		let video = Track::new(presentation.clone(), Kind::Video);
		audio.declare();
		audio.end(ms(10_200)).unwrap();
		audio.retire(Some(now + Duration::from_millis(300)));
		// A replacement rendition starts over at zero and re-pins the anchor.
		{
			let mut presentation = presentation.state.lock().unwrap();
			presentation.restarted();
			presentation.audio(Duration::from_millis(50), DELAY, now);
		}
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
		let presentation = Arc::new(Clock::new(DELAY));
		presentation
			.state
			.lock()
			.unwrap()
			.audio(Duration::from_secs(10), DELAY, now);
		let audio = Track::new(presentation.clone(), Kind::Audio);
		let video = Track::new(presentation.clone(), Kind::Video);
		audio.declare();
		audio.end(ms(10_200)).unwrap();
		audio.retire(Some(now + Duration::from_millis(300)));
		// The next rendition fails while the retired sink still plays.
		let successor = Track::new(presentation.clone(), Kind::Audio);
		successor.declare();
		drop(successor);
		let transition = video.declare();
		video.plan(transition, ms(0), None);
		let resume = kio::clock::timeout(
			Duration::from_millis(250),
			video.resume(transition, ms(0), None).ready(),
		)
		.await;
		assert!(resume.is_err(), "video resumed over the retired audio tail");
	}

	#[tokio::test]
	async fn paired_plans_keep_media_offsets_and_follow_the_speakers_delay() {
		tokio::time::pause();
		let now = kio::clock::now();
		let presentation = Arc::new(Clock::new(DELAY));
		presentation
			.state
			.lock()
			.unwrap()
			.audio(Duration::from_secs(10), DELAY, now);
		let audio = Track::new(presentation.clone(), Kind::Audio);
		let video = Track::new(presentation.clone(), Kind::Video);
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
			presentation.state.lock().unwrap().due(audio.map(ms(0)).unwrap()),
			Some(now + Duration::from_millis(200))
		);
	}

	/// Earlier arrivals pull the anchor forward so playback converges on the live
	/// edge without giving up the configured delay.
	#[test]
	fn a_late_first_frame_catches_up_to_live() {
		let start = Instant::now();
		let mut presentation = State::new(DELAY);

		// Produced 500ms ago, though nothing here knows that yet. The window is
		// asleep on the deadline the anchor last gave it, so every move is reported.
		assert!(presentation.video(ms(0), start), "the first anchor");
		assert_eq!(presentation.due(ms(0)), Some(start + DELAY));

		// 40ms of media later, but only 20ms of wall clock: the anchor was 480ms
		// behind live, so it moves onto this frame.
		let now = start + Duration::from_millis(20);
		assert!(presentation.video(ms(40), now), "an early frame left the anchor alone");
		assert_eq!(presentation.due(ms(40)), Some(now + DELAY));

		// A frame that merely arrives late keeps its media instant. The delay is
		// the room it has to be late in.
		let late = now + Duration::from_millis(70);
		assert!(!presentation.video(ms(80), late), "a late arrival moved the anchor");
		assert_eq!(presentation.due(ms(80)), Some(now + Duration::from_millis(140)));
	}

	/// The speaker cannot skip forward with a re-anchor, so while it is playing a
	/// video burst must not move the anchor out from under it.
	#[test]
	fn the_speaker_owns_the_anchor_while_it_plays() {
		let start = Instant::now();
		let mut presentation = State::new(DELAY);

		presentation.audio(Duration::from_secs(1), DELAY, start);
		let anchored = presentation.due(ms(1_000));

		// A tune-in burst: seconds of video arriving at once.
		presentation.video(ms(1_040), start);
		presentation.video(ms(4_000), start);
		assert_eq!(
			presentation.due(ms(1_000)),
			anchored,
			"video moved the speaker's anchor"
		);

		// Once audio stops, video anchors again.
		presentation.stopped();
		presentation.video(ms(4_040), start);
		assert_eq!(presentation.due(ms(4_040)), Some(start + DELAY));
	}

	/// The speaker reports the sample sounding now, which is a delay behind the
	/// edge. Pinning that instant directly would schedule video a second delay
	/// behind the sound.
	#[test]
	fn the_speaker_anchors_at_the_live_edge() {
		let start = Instant::now();
		let mut presentation = State::new(DELAY);

		// The last sample written is a full delay from sounding, which is exactly
		// where a settled sink sits: media time and the picture agree.
		presentation.audio(Duration::from_secs(1), DELAY, start);
		assert_eq!(presentation.due(ms(1_000)), Some(start + DELAY));

		// Filling up, with only 40ms queued: that sample sounds in 40ms, so the
		// picture that goes with it is due then too.
		let now = start + Duration::from_millis(500);
		presentation.audio(Duration::from_millis(1_500), Duration::from_millis(40), now);
		assert_eq!(presentation.due(ms(1_500)), Some(now + Duration::from_millis(40)));
	}

	/// Video usually anchors first, and audio's first frame can sit behind that
	/// anchor. Pacing it would map it into the past, which never re-anchors, so
	/// the sink would play now while the picture stayed on video's timeline.
	#[test]
	fn the_speaker_re_anchors_when_it_takes_over() {
		let start = Instant::now();
		let mut presentation = State::new(DELAY);
		presentation.video(ms(4_000), start);

		presentation.audio(Duration::from_secs(1), DELAY, start);
		assert_eq!(presentation.due(ms(1_000)), Some(start + DELAY));

		// A replacement rendition is a track boundary too: its timestamps need not
		// continue the one that just ended.
		presentation.stopped();
		let now = start + Duration::from_millis(20);
		presentation.audio(Duration::from_millis(200), DELAY, now);
		assert_eq!(presentation.due(ms(200)), Some(now + DELAY));
	}

	/// A hole too large to play through drops the buffered audio and starts a new
	/// sink, and a rewind restarts the timeline outright. Pacing across either
	/// would schedule video against samples the speaker never plays.
	#[test]
	fn a_restarted_speaker_re_anchors() {
		let start = Instant::now();
		let mut presentation = State::new(DELAY);
		presentation.audio(Duration::from_secs(10), DELAY, start);

		// The publisher rewound: without the restart this pins media 9 seconds
		// behind the anchor, leaving the picture 9 seconds in the past.
		let now = start + Duration::from_millis(20);
		presentation.restarted();
		presentation.audio(Duration::from_secs(1), DELAY, now);
		assert_eq!(presentation.due(ms(1_000)), Some(now + DELAY));
	}

	/// A retired rendition marks a restart for its replacement, which may never
	/// come. The next track re-pins on its first frame for taking ownership, so a
	/// restart left over would re-pin the frame after it onto that frame's jitter.
	#[test]
	fn a_stopped_speaker_drops_a_pending_restart() {
		let start = Instant::now();
		let mut presentation = State::new(DELAY);
		presentation.audio(Duration::from_secs(10), DELAY, start);
		presentation.restarted();
		presentation.stopped();

		presentation.audio(Duration::from_secs(1), DELAY, start);
		// 20ms of media, 40ms of wall clock: a late frame paces and leaves the
		// anchor where it was.
		let now = start + Duration::from_millis(40);
		presentation.audio(Duration::from_millis(1_020), DELAY, now);
		assert_eq!(
			presentation.due(ms(1_020)),
			Some(start + Duration::from_millis(20) + DELAY)
		);
	}

	/// The window sleeps on the deadline it last computed, so an anchor the
	/// speaker moved has to say so or the queue presents late by that much.
	#[test]
	fn a_moved_anchor_is_reported() {
		let start = Instant::now();
		let mut presentation = State::new(DELAY);
		assert!(
			presentation.audio(Duration::from_secs(1), DELAY, start),
			"the first anchor"
		);

		// On schedule: 40ms of media, 40ms of wall clock, same sink depth.
		let now = start + Duration::from_millis(40);
		assert!(
			!presentation.audio(Duration::from_millis(1_040), DELAY, now),
			"a frame on the anchor moved it"
		);

		// Arriving early re-anchors, and the window has to hear about it.
		assert!(
			presentation.audio(Duration::from_millis(1_100), DELAY, now),
			"an early frame left the anchor alone"
		);
	}
}
