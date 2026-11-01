//! Media time: the playout clock, and the audio track's own timeline.

use anyhow::Context;
use moq_clock::Instant;
use std::time::Duration;

use super::playback::Kind;
use hang::moq_net::Timestamp;

/// The playout clock: one anchor the window and the speaker both present
/// against.
///
/// Every arriving frame is folded into a single [`Pacer`](moq_mux::Pacer), which
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
pub(super) struct Presentation {
	pacer: moq_mux::Pacer,
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
	/// Wakes every pending resume when a boundary, a transition or the anchor
	/// changes. Kept apart from the window's and the decoders' wakeups, so a
	/// resume can never take a wakeup meant for them.
	timeline: std::sync::Arc<tokio::sync::Notify>,
}

/// One encoder restart, shared by the roles whose ends declared it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Transition(u64);

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

impl Presentation {
	pub(super) fn new(delay: Duration) -> Self {
		Self {
			pacer: moq_mux::Pacer::default().with_delay(delay),
			delay,
			speaker: false,
			restart: false,
			ends: Default::default(),
			tails: Vec::new(),
			transitions: Default::default(),
			declared: Default::default(),
			next: Transition(1),
			reference: None,
			written: None,
			target: None,
			timeline: Default::default(),
		}
	}

	/// How many transitions are still pending.
	#[cfg(test)]
	pub(super) fn pending(&self) -> usize {
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
		let reference = timestamp(reference);
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
	pub(super) fn video(&mut self, timestamp: Timestamp, now: Instant) -> bool {
		if self.speaker {
			return false;
		}

		self.reference = Some(timestamp);
		let before = self.pacer.due(timestamp);
		let moved = before != Some(self.pacer.pace(timestamp, now));
		if moved {
			self.timeline.notify_waiters();
		}
		moved
	}

	/// Start a new video timeline without taking ownership from a speaker.
	pub(super) fn video_restarted(&mut self) {
		if !self.speaker {
			self.pacer = moq_mux::Pacer::default().with_delay(self.delay);
			self.timeline.notify_waiters();
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
	pub(super) fn audio(&mut self, end: Duration, buffered: Duration, now: Instant) -> bool {
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

		let moved = before != Some(after);
		if moved {
			self.timeline.notify_waiters();
		}
		moved
	}

	/// The speaker restarted on a new timeline, so the media either side of the
	/// break is unrelated: re-pin the anchor on the next audio frame rather than
	/// pacing across a jump the speaker never plays.
	pub(super) fn restarted(&mut self) {
		self.restart = true;
	}

	/// The speaker went quiet, so nothing holds playback to its cadence any
	/// more and video takes the anchor back.
	///
	/// A replacement rendition takes ownership again on its first frame, and
	/// re-pins there: it is a track boundary, so its timestamps are not
	/// necessarily continuous with what just ended. That covers a pending
	/// restart, which would otherwise re-pin the frame after it too.
	pub(super) fn stopped(&mut self) {
		self.speaker = false;
		self.restart = false;
		self.written = None;
		self.target = None;
	}

	/// Record the playout target the speaker is being held on.
	pub(super) fn hold(&mut self, target: Duration) {
		self.target = Some(target);
	}

	/// The media time sounding at `now`, or `None` with no speaker.
	///
	/// Projected from the last write at the wall rate, held at its end (a speaker
	/// with nothing left plays nothing further), and never behind where it stood
	/// at that write (refill padding plays no media).
	pub(super) fn playhead(&self, now: Instant) -> Option<Duration> {
		let (end, sounds, floor) = self.written?;
		let projected = end.saturating_sub(sounds.saturating_duration_since(now));
		Some(floor.map_or(projected, |floor| projected.max(floor)).min(end))
	}

	/// The playout target the speaker is held on, or `None` with no speaker.
	pub(super) fn target(&self) -> Option<Duration> {
		self.target
	}

	/// When `timestamp` should be presented, or `None` before any frame has
	/// anchored the clock. `None` means as soon as possible: there is nothing
	/// left to wait for.
	pub(super) fn due(&self, timestamp: Timestamp) -> Option<Instant> {
		self.pacer.due(timestamp)
	}
}

/// Maps each encoder segment onto the shared playback clock.
pub(super) struct Role {
	presentation: std::sync::Arc<std::sync::Mutex<Presentation>>,
	timeline: std::sync::Arc<tokio::sync::Notify>,
	role: Kind,
	mapping: std::sync::Mutex<Mapping>,
}

#[derive(Clone, Copy, Default)]
pub(super) struct Mapping {
	source: Duration,
	target: Duration,
	starts: Option<Instant>,
}

impl Mapping {
	fn map(self, source: Timestamp) -> anyhow::Result<Timestamp> {
		let source = Duration::from(source);
		let target = if source >= self.source {
			self.target
				.checked_add(source - self.source)
				.context("playback timeline overflow")?
		} else {
			self.target
				.checked_sub(self.source - source)
				.context("picture precedes the playback timeline")?
		};
		Ok(Timestamp::from_micros(u64::try_from(target.as_micros())?)?)
	}

	fn started(&self, now: Instant) -> bool {
		self.starts.is_none_or(|starts| starts <= now)
	}
}

impl Role {
	pub(super) fn new(presentation: std::sync::Arc<std::sync::Mutex<Presentation>>, role: Kind) -> Self {
		let timeline = presentation.lock().unwrap().timeline.clone();
		Self {
			presentation,
			timeline,
			role,
			mapping: Default::default(),
		}
	}

	/// Stop while a tail still plays until `drained`, holding a declared end until
	/// then. Dropping instead discards the tail with the end.
	pub(super) fn retire(self, drained: Option<Instant>) {
		let now = moq_clock::now();
		let mut presentation = self.presentation.lock().unwrap();
		presentation.tails.retain(|tail| *tail > now);
		if presentation.ends[self.role as usize].take().is_some()
			&& let Some(drained) = drained
		{
			presentation.tails.push(drained);
		}
	}

	pub(super) fn map(&self, source: Timestamp) -> anyhow::Result<Timestamp> {
		self.mapping.lock().unwrap().map(source)
	}

	/// Declare that this role's encoder stopped, returning the transition its
	/// replacement resumes.
	///
	/// A peer's transition this role has not joined yet is the same restart, as
	/// long as its replacement has not started playing; otherwise this one opens
	/// a transition of its own.
	pub(super) fn declare(&self) -> Transition {
		let now = moq_clock::now();
		let own = self.role as usize;
		let mut presentation = self.presentation.lock().unwrap();
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
		self.timeline.notify_waiters();
		id
	}

	/// Mark where this role's current segment ends on the shared clock.
	pub(super) fn end(&self, source: Timestamp) -> anyhow::Result<()> {
		let mapping = *self.mapping.lock().unwrap();
		self.presentation.lock().unwrap().ends[self.role as usize] = Some(Boundary {
			starts: mapping.starts,
			end: timestamp(mapping.map(source)?),
		});
		self.timeline.notify_waiters();
		Ok(())
	}

	/// Record the first replacement media for `transition`.
	pub(super) fn plan(&self, transition: Transition, source: Timestamp, delay: Option<Duration>) {
		let mut presentation = self.presentation.lock().unwrap();
		let delay = delay.unwrap_or(presentation.delay);
		if let Some(pending) = presentation.transition(transition) {
			pending.plans[self.role as usize] = Some(Plan {
				source: Duration::from(source),
				delay,
			});
		}
		drop(presentation);
		self.timeline.notify_waiters();
	}

	/// Media was lost: nothing waits for the discarded tail.
	pub(super) fn gap(&self) {
		self.presentation.lock().unwrap().ends[self.role as usize] = None;
		self.timeline.notify_waiters();
	}

	/// This role will never resume `transition`, so a peer must not pair with its plan.
	pub(super) fn abandon(&self, transition: Transition) {
		let own = self.role as usize;
		let mut presentation = self.presentation.lock().unwrap();
		if let Some(pending) = presentation.transition(transition) {
			pending.members[own] = false;
			pending.plans[own] = None;
		}
		presentation.prune(moq_clock::now());
		drop(presentation);
		self.timeline.notify_waiters();
	}

	pub(super) fn apply(&self, transition: Transition, mapping: Mapping) {
		*self.mapping.lock().unwrap() = mapping;
		let mut presentation = self.presentation.lock().unwrap();
		if let Some(pending) = presentation.transition(transition) {
			pending.resumed[self.role as usize] = true;
		}
		presentation.prune(moq_clock::now());
		drop(presentation);
		self.timeline.notify_waiters();
	}

	/// Drain declared old media, then map `transition`'s replacement onto the
	/// running clock.
	pub(super) async fn resume(
		&self,
		transition: Transition,
		source: Timestamp,
		delay: Option<Duration>,
	) -> anyhow::Result<Mapping> {
		loop {
			// Armed before reading the boundaries, so a change made between the read
			// and the wait still wakes it.
			let changed = self.timeline.notified();
			let mut changed = std::pin::pin!(changed);
			changed.as_mut().enable();
			let now = moq_clock::now();
			let end = {
				let mut presentation = self.presentation.lock().unwrap();
				// The peer already resumed this transition after draining every end
				// before it; one it declared since must not hold this role back.
				if let Some(mapping) = presentation.transition(transition).and_then(|pending| pending.mapping) {
					return Ok(mapping);
				}
				presentation
					.ends
					.iter()
					.flatten()
					.filter(|boundary| boundary.starts.is_none_or(|start| start <= now))
					.filter_map(|boundary| presentation.due(media(boundary.end)))
					.chain(presentation.tails.iter().copied())
					.max()
			};
			if let Some(end) = end.filter(|end| *end > now) {
				tokio::select! {
					_ = moq_clock::sleep_until(end) => {},
					_ = changed => {},
				}
				continue;
			}
			let mut presentation = self.presentation.lock().unwrap();
			let peer = 1 - self.role as usize;
			// A transition abandoned under this resume, by a gap the caller rechecks,
			// maps alone like one the peer never joined.
			let (mapping, paired) = presentation.transition(transition).map_or((None, None), |pending| {
				let both = pending.members[peer] && pending.plans.iter().all(Option::is_some);
				(pending.mapping, pending.plans[Kind::Audio as usize].filter(|_| both))
			});
			// The peer already resumed this transition.
			if let Some(mapping) = mapping {
				return Ok(mapping);
			}
			// Both roles have replacement media. The speaker's first sample defines
			// their common origin even when its adaptive delay differs from video's.
			// Otherwise this role maps alone and a peer joining later follows it.
			let (source, delay) = match paired {
				Some(audio) => (audio.source, audio.delay),
				None => (Duration::from(source), delay.unwrap_or(presentation.delay)),
			};
			let starts = now
				.checked_add(delay)
				.context("playback delay exceeds the system clock")?;
			let mapping = Mapping {
				source,
				target: presentation.position(starts).unwrap_or(source),
				starts: Some(starts),
			};
			if let Some(pending) = presentation.transition(transition) {
				pending.mapping = Some(mapping);
			}
			return Ok(mapping);
		}
	}
}

impl Drop for Role {
	fn drop(&mut self) {
		let own = self.role as usize;
		if let Ok(mut presentation) = self.presentation.lock() {
			presentation.ends[own] = None;
			for pending in presentation
				.transitions
				.iter_mut()
				.filter(|pending| !pending.resumed[own])
			{
				pending.members[own] = false;
				pending.plans[own] = None;
			}
			presentation.declared[own] = Transition::default();
			presentation.prune(moq_clock::now());
		}
		self.timeline.notify_waiters();
	}
}

/// A wire timestamp as a duration from the start of the track.
pub(super) fn timestamp(timestamp: Timestamp) -> Duration {
	Duration::from_micros(timestamp.as_micros().min(u64::MAX as u128) as u64)
}

/// A duration from the start of the track as a wire timestamp, the inverse of
/// [`timestamp`].
fn media(duration: Duration) -> Timestamp {
	// A timestamp caps at 2^61 - 1, which is ~73,000 years of microseconds.
	const MAX: u128 = (1 << 61) - 1;
	Timestamp::from_micros(duration.as_micros().min(MAX) as u64).expect("clamped to the timestamp maximum")
}

/// Where the audio track has reached, measured from its own origin so
/// timestamp rounding can't accumulate into drift.
#[derive(Default)]
pub(super) struct AudioTimeline {
	origin: Option<Duration>,
	end: Option<Duration>,
	written: u64,
}

/// What the speaker owes before the frame just pushed: silence to play a hole
/// through, or a fresh sink when the timeline jumped too far to fill.
pub(super) struct AudioTiming {
	/// Media time the pushed frame ends at.
	pub(super) end: Duration,
	/// Samples of silence to write first.
	pub(super) silence: u64,
	/// Whether the buffered sink has to be replaced.
	pub(super) reset_sink: bool,
}

impl AudioTimeline {
	pub(super) fn push(&mut self, start: Duration, samples: usize, sample_rate: u32, fill_max: u64) -> AudioTiming {
		let duration = Duration::from_secs_f64(samples as f64 / sample_rate as f64);
		let end = start.saturating_add(duration);
		// Millisecond-stamped input can put adjacent frames on either side of their
		// exact boundary. Two output samples cover the conversions on top of that.
		let tolerance = Duration::from_millis(1).saturating_add(Duration::from_secs_f64(2.0 / sample_rate as f64));
		let rewound = self
			.end
			.is_some_and(|previous| start.saturating_add(tolerance) < previous);
		if rewound {
			self.origin = None;
			self.written = 0;
		}

		// Measure every hole from the track origin so timestamp rounding cannot
		// accumulate into drift. Advancing to `expected` even when the hole is skipped
		// keeps the next frame contiguous with the new timeline position.
		let origin = *self.origin.get_or_insert(start);
		let expected = (start.saturating_sub(origin).as_secs_f64() * sample_rate as f64).round() as u64;
		let hole = expected.saturating_sub(self.written);
		let skipped = hole > fill_max;
		let silence = if skipped { 0 } else { hole };
		let reset_sink = rewound || skipped;
		self.written = self
			.written
			.max(expected)
			.saturating_add(u64::try_from(samples).unwrap_or(u64::MAX));
		self.end = Some(end);

		AudioTiming {
			end,
			silence,
			reset_sink,
		}
	}
}

/// How to hand a write to a speaker already holding `buffered`, so it sits on
/// `target`: the jitter buffer the browser's audio rings hold, in sample frames.
/// The write is `fill` samples of silence standing in for a hole in the media,
/// then the `frame` itself.
///
/// A `dry` ring, one that ran out or has only just opened, re-stalls: it pads up
/// to the target before playing on, rather than playing the next arrival on an
/// empty cushion. A ring more than `slack` over the target lands back on it by
/// skipping the oldest of what is arriving.
///
/// A playing ring is judged on what it holds ahead of the frame. The speaker
/// drains a frame before the next one is due, so counting it would skip a frame
/// that lands a little early and leave the ring a frame short for the next late
/// one: a click, then a dropout. The slack is what a device period moves the
/// level by, so ordinary cadence never skips.
pub(super) fn fit(dry: bool, buffered: u64, target: u64, slack: u64, fill: u64, frame: u64) -> Fit {
	let incoming = fill + frame;
	let pad = if dry {
		target.saturating_sub(buffered + incoming)
	} else {
		0
	};
	let level = buffered + pad + incoming;
	let ahead = if dry { level } else { level - frame };
	let skip = if ahead > target + slack {
		(ahead - target).min(incoming)
	} else {
		0
	};
	Fit { pad, skip }
}

/// What [`fit`] decided for one write.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Fit {
	/// Silence to write first.
	pub(super) pad: u64,
	/// Samples to drop from the front of the write.
	pub(super) skip: u64,
}

#[cfg(test)]
mod tests {
	use super::*;

	const DELAY: Duration = Duration::from_millis(100);

	fn ms(millis: u64) -> Timestamp {
		Timestamp::from_millis(millis).unwrap()
	}

	/// The playhead runs at the wall rate from the last write and stops at its
	/// end, which is what a speaker that ran dry is still sounding.
	#[test]
	fn the_playhead_projects_the_last_write_and_holds_at_its_end() {
		let now = Instant::now();
		let mut presentation = Presentation::new(DELAY);
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
		let mut presentation = Presentation::new(DELAY);
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
		let start = moq_clock::now();
		let presentation = std::sync::Arc::new(std::sync::Mutex::new(Presentation::new(DELAY)));
		let audio = Role::new(presentation.clone(), Kind::Audio);
		let video = Role::new(presentation.clone(), Kind::Video);
		presentation.lock().unwrap().video(ms(10_000), start);
		let transition = audio.declare();
		audio.end(ms(10_020)).unwrap();
		tokio::time::advance(DELAY + Duration::from_millis(20)).await;
		let unchanged = presentation.lock().unwrap().due(ms(10_500));
		audio.apply(transition, audio.resume(transition, ms(0), Some(DELAY)).await.unwrap());
		assert_eq!(audio.map(ms(0)).unwrap().as_millis(), 10_120);
		assert_eq!(video.map(ms(10_500)).unwrap().as_millis(), 10_500);
		assert_eq!(presentation.lock().unwrap().due(ms(10_500)), unchanged);
	}

	#[tokio::test]
	async fn a_late_paired_replacement_keeps_its_media_offset() {
		tokio::time::pause();
		let now = moq_clock::now();
		let presentation = std::sync::Arc::new(std::sync::Mutex::new(Presentation::new(DELAY)));
		presentation.lock().unwrap().audio(Duration::from_secs(10), DELAY, now);
		let audio = Role::new(presentation.clone(), Kind::Audio);
		let video = Role::new(presentation.clone(), Kind::Video);
		let transition = audio.declare();
		assert_eq!(video.declare(), transition, "one restart");
		audio.end(ms(10_020)).unwrap();
		video.end(ms(10_020)).unwrap();
		audio.plan(transition, ms(0), Some(DELAY));
		tokio::time::advance(DELAY + Duration::from_millis(20)).await;
		audio.apply(transition, audio.resume(transition, ms(0), Some(DELAY)).await.unwrap());
		// The video replacement lands after the resumed audio has started playing.
		tokio::time::advance(DELAY * 2).await;
		video.plan(transition, ms(40), None);
		video.apply(transition, video.resume(transition, ms(40), None).await.unwrap());
		assert_eq!(
			video.map(ms(40)).unwrap().as_millis() - audio.map(ms(0)).unwrap().as_millis(),
			40
		);
	}

	#[tokio::test]
	async fn a_late_replacement_pairs_with_its_own_transition() {
		tokio::time::pause();
		let now = moq_clock::now();
		let presentation = std::sync::Arc::new(std::sync::Mutex::new(Presentation::new(DELAY)));
		presentation.lock().unwrap().audio(Duration::from_secs(10), DELAY, now);
		let audio = Role::new(presentation.clone(), Kind::Audio);
		let video = Role::new(presentation.clone(), Kind::Video);
		let transition = audio.declare();
		video.declare();
		audio.end(ms(10_020)).unwrap();
		video.end(ms(10_020)).unwrap();
		video.plan(transition, ms(40), None);
		tokio::time::advance(DELAY + Duration::from_millis(20)).await;
		video.apply(transition, video.resume(transition, ms(40), None).await.unwrap());
		// Video already queued its next replacement when audio's first one lands.
		let next = video.declare();
		assert_ne!(next, transition);
		video.end(ms(60)).unwrap();
		video.plan(next, ms(5_000), None);
		audio.plan(transition, ms(0), Some(DELAY));
		audio.apply(transition, audio.resume(transition, ms(0), Some(DELAY)).await.unwrap());
		assert_eq!(
			video.map(ms(40)).unwrap().as_millis() - audio.map(ms(0)).unwrap().as_millis(),
			40
		);
	}

	#[tokio::test]
	async fn a_peer_two_restarts_ahead_pairs_with_the_first() {
		tokio::time::pause();
		let now = moq_clock::now();
		let presentation = std::sync::Arc::new(std::sync::Mutex::new(Presentation::new(DELAY)));
		presentation.lock().unwrap().audio(Duration::from_secs(10), DELAY, now);
		let audio = Role::new(presentation.clone(), Kind::Audio);
		let video = Role::new(presentation.clone(), Kind::Video);
		let transition = audio.declare();
		video.declare();
		audio.end(ms(10_020)).unwrap();
		video.end(ms(10_020)).unwrap();
		tokio::time::advance(DELAY + Duration::from_millis(20)).await;
		video.plan(transition, ms(40), None);
		let first = video.resume(transition, ms(40), None).await.unwrap();
		video.apply(transition, first);
		// Video restarts again and resumes before audio's first replacement lands.
		let next = video.declare();
		video.plan(next, ms(5_000), None);
		tokio::time::advance(Duration::from_millis(10)).await;
		video.apply(next, video.resume(next, ms(5_000), None).await.unwrap());
		audio.plan(transition, ms(0), Some(DELAY));
		audio.apply(transition, audio.resume(transition, ms(0), Some(DELAY)).await.unwrap());
		assert_eq!(
			first.map(ms(40)).unwrap().as_millis() - audio.map(ms(0)).unwrap().as_millis(),
			40
		);
	}

	#[tokio::test]
	async fn an_abandoned_replacement_leaves_the_next_transition_alone() {
		tokio::time::pause();
		let now = moq_clock::now();
		let presentation = std::sync::Arc::new(std::sync::Mutex::new(Presentation::new(DELAY)));
		presentation.lock().unwrap().audio(Duration::from_secs(10), DELAY, now);
		let audio = Role::new(presentation.clone(), Kind::Audio);
		let video = Role::new(presentation.clone(), Kind::Video);
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
		audio.apply(transition, audio.resume(transition, ms(0), Some(DELAY)).await.unwrap());
		video.apply(next, video.resume(next, ms(5_000), None).await.unwrap());
		// The later replacement maps onto the running clock, not the audio restart's.
		let resumed = moq_clock::now() + DELAY;
		assert_eq!(
			presentation.lock().unwrap().due(video.map(ms(5_000)).unwrap()),
			Some(resumed)
		);
	}

	#[tokio::test]
	async fn an_abandoned_peer_plan_is_not_paired() {
		tokio::time::pause();
		let now = moq_clock::now();
		let presentation = std::sync::Arc::new(std::sync::Mutex::new(Presentation::new(DELAY)));
		presentation.lock().unwrap().audio(Duration::from_secs(10), DELAY, now);
		let audio = Role::new(presentation.clone(), Kind::Audio);
		let video = Role::new(presentation.clone(), Kind::Video);
		let transition = audio.declare();
		video.declare();
		audio.end(ms(10_020)).unwrap();
		video.end(ms(10_020)).unwrap();
		audio.plan(transition, ms(0), Some(Duration::from_millis(80)));
		audio.abandon(transition);
		video.plan(transition, ms(40), None);
		tokio::time::advance(DELAY + Duration::from_millis(20)).await;
		video.apply(transition, video.resume(transition, ms(40), None).await.unwrap());
		let resumed = moq_clock::now() + DELAY;
		assert_eq!(
			presentation.lock().unwrap().due(video.map(ms(40)).unwrap()),
			Some(resumed)
		);
	}

	#[tokio::test]
	async fn a_resumed_transition_ignores_the_peers_later_end() {
		tokio::time::pause();
		let now = moq_clock::now();
		let presentation = std::sync::Arc::new(std::sync::Mutex::new(Presentation::new(DELAY)));
		presentation.lock().unwrap().audio(Duration::from_secs(10), DELAY, now);
		let audio = Role::new(presentation.clone(), Kind::Audio);
		let video = Role::new(presentation.clone(), Kind::Video);
		let transition = audio.declare();
		video.declare();
		audio.end(ms(10_020)).unwrap();
		video.end(ms(10_020)).unwrap();
		video.plan(transition, ms(40), None);
		tokio::time::advance(DELAY + Duration::from_millis(20)).await;
		let mapping = video.resume(transition, ms(40), None).await.unwrap();
		video.apply(transition, mapping);
		// The resumed video plays, then its encoder stops again far ahead.
		tokio::time::advance(DELAY * 2).await;
		video.declare();
		video.end(ms(60_000)).unwrap();
		audio.plan(transition, ms(0), Some(DELAY));
		let resumed = moq_clock::timeout(Duration::from_millis(1), audio.resume(transition, ms(0), Some(DELAY))).await;
		assert!(resumed.is_ok(), "audio waited for video's next end");
	}

	#[tokio::test]
	async fn a_cancelled_role_releases_its_declared_end() {
		tokio::time::pause();
		let now = moq_clock::now();
		let presentation = std::sync::Arc::new(std::sync::Mutex::new(Presentation::new(DELAY)));
		presentation.lock().unwrap().audio(Duration::from_secs(10), DELAY, now);
		let audio = Role::new(presentation.clone(), Kind::Audio);
		let video = Role::new(presentation.clone(), Kind::Video);
		audio.declare();
		audio.end(ms(10_200)).unwrap();
		// Cancelled with its sink, so nothing plays the tail.
		drop(audio);
		let transition = video.declare();
		video.end(ms(10_000)).unwrap();
		video.plan(transition, ms(0), None);
		let resume = moq_clock::timeout(
			DELAY + Duration::from_millis(150),
			video.resume(transition, ms(0), None),
		)
		.await;
		assert!(resume.is_ok(), "video waited for discarded audio");
	}

	#[tokio::test]
	async fn a_closed_role_still_holds_its_declared_end() {
		tokio::time::pause();
		let now = moq_clock::now();
		let presentation = std::sync::Arc::new(std::sync::Mutex::new(Presentation::new(DELAY)));
		presentation.lock().unwrap().audio(Duration::from_secs(10), DELAY, now);
		let audio = Role::new(presentation.clone(), Kind::Audio);
		let video = Role::new(presentation.clone(), Kind::Video);
		audio.declare();
		audio.end(ms(10_200)).unwrap();
		// The track closed, but its returned sink still plays the tail up to the end.
		audio.retire(Some(now + Duration::from_millis(300)));
		let transition = video.declare();
		video.end(ms(10_000)).unwrap();
		video.plan(transition, ms(0), None);
		let resume = moq_clock::timeout(
			DELAY + Duration::from_millis(150),
			video.resume(transition, ms(0), None),
		)
		.await;
		assert!(resume.is_err(), "video resumed before the audio tail played");
	}

	#[tokio::test]
	async fn a_retired_end_ignores_a_later_re_pin() {
		tokio::time::pause();
		let now = moq_clock::now();
		let presentation = std::sync::Arc::new(std::sync::Mutex::new(Presentation::new(DELAY)));
		presentation.lock().unwrap().audio(Duration::from_secs(10), DELAY, now);
		let audio = Role::new(presentation.clone(), Kind::Audio);
		let video = Role::new(presentation.clone(), Kind::Video);
		audio.declare();
		audio.end(ms(10_200)).unwrap();
		audio.retire(Some(now + Duration::from_millis(300)));
		// A replacement rendition starts over at zero and re-pins the anchor.
		{
			let mut presentation = presentation.lock().unwrap();
			presentation.restarted();
			presentation.audio(Duration::from_millis(50), DELAY, now);
		}
		let transition = video.declare();
		video.plan(transition, ms(0), None);
		let resume = moq_clock::timeout(Duration::from_millis(400), video.resume(transition, ms(0), None)).await;
		assert!(resume.is_ok(), "video waited past the drained tail");
	}

	#[tokio::test]
	async fn a_cancelled_successor_keeps_a_retired_tail() {
		tokio::time::pause();
		let now = moq_clock::now();
		let presentation = std::sync::Arc::new(std::sync::Mutex::new(Presentation::new(DELAY)));
		presentation.lock().unwrap().audio(Duration::from_secs(10), DELAY, now);
		let audio = Role::new(presentation.clone(), Kind::Audio);
		let video = Role::new(presentation.clone(), Kind::Video);
		audio.declare();
		audio.end(ms(10_200)).unwrap();
		audio.retire(Some(now + Duration::from_millis(300)));
		// The next rendition fails while the retired sink still plays.
		let successor = Role::new(presentation.clone(), Kind::Audio);
		successor.declare();
		drop(successor);
		let transition = video.declare();
		video.plan(transition, ms(0), None);
		let resume = moq_clock::timeout(Duration::from_millis(250), video.resume(transition, ms(0), None)).await;
		assert!(resume.is_err(), "video resumed over the retired audio tail");
	}

	#[tokio::test]
	async fn paired_plans_keep_media_offsets_and_follow_the_speakers_delay() {
		tokio::time::pause();
		let now = moq_clock::now();
		let presentation = std::sync::Arc::new(std::sync::Mutex::new(Presentation::new(DELAY)));
		presentation.lock().unwrap().audio(Duration::from_secs(10), DELAY, now);
		let audio = Role::new(presentation.clone(), Kind::Audio);
		let video = Role::new(presentation.clone(), Kind::Video);
		let transition = audio.declare();
		video.declare();
		audio.end(ms(10_020)).unwrap();
		video.end(ms(10_020)).unwrap();
		audio.plan(transition, ms(0), Some(Duration::from_millis(80)));
		video.plan(transition, ms(40), None);
		tokio::time::advance(DELAY + Duration::from_millis(20)).await;
		video.apply(transition, video.resume(transition, ms(40), None).await.unwrap());
		audio.apply(
			transition,
			audio
				.resume(transition, ms(0), Some(Duration::from_millis(80)))
				.await
				.unwrap(),
		);
		assert_eq!(
			video.map(ms(40)).unwrap().as_millis() - audio.map(ms(0)).unwrap().as_millis(),
			40
		);
		assert_eq!(
			presentation.lock().unwrap().due(audio.map(ms(0)).unwrap()),
			Some(now + Duration::from_millis(200))
		);
	}

	/// Earlier arrivals pull the anchor forward so playback converges on the live
	/// edge without giving up the configured delay.
	#[test]
	fn a_late_first_frame_catches_up_to_live() {
		let start = Instant::now();
		let mut presentation = Presentation::new(DELAY);

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
		let mut presentation = Presentation::new(DELAY);

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
		let mut presentation = Presentation::new(DELAY);

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
		let mut presentation = Presentation::new(DELAY);
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
		let mut presentation = Presentation::new(DELAY);
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
		let mut presentation = Presentation::new(DELAY);
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
		let mut presentation = Presentation::new(DELAY);
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

	#[test]
	fn audio_timeline_restarts_when_media_time_rewinds() {
		let mut timeline = AudioTimeline::default();
		let first = timeline.push(Duration::from_secs(10), 960, 48_000, 24_000);
		assert!(!first.reset_sink);

		let rewound = timeline.push(Duration::from_secs(5), 960, 48_000, 24_000);
		assert!(rewound.reset_sink);
		assert_eq!(rewound.silence, 0);

		let next = timeline.push(Duration::from_millis(5_020), 960, 48_000, 24_000);
		assert!(!next.reset_sink);
		assert_eq!(next.silence, 0);
	}

	#[test]
	fn audio_timeline_tolerates_millisecond_stamp_rounding() {
		let mut timeline = AudioTimeline::default();
		let first = timeline.push(Duration::ZERO, 1024, 44_100, 22_050);
		assert!(!first.reset_sink);

		// 1024 frames end at 23.22 ms, but an FLV timestamp carries 23 ms.
		let rounded = timeline.push(Duration::from_millis(23), 1024, 44_100, 22_050);
		assert!(!rounded.reset_sink);
	}

	#[test]
	fn audio_timeline_resets_sink_when_forward_hole_exceeds_fill_cap() {
		let mut timeline = AudioTimeline::default();
		timeline.push(Duration::ZERO, 960, 48_000, 4_800);

		let filled = timeline.push(Duration::from_millis(100), 960, 48_000, 4_800);
		assert!(!filled.reset_sink);
		assert_eq!(filled.silence, 3_840);

		let skipped = timeline.push(Duration::from_secs(1), 960, 48_000, 4_800);
		assert!(skipped.reset_sink);
		assert_eq!(skipped.silence, 0);

		let next = timeline.push(Duration::from_millis(1_020), 960, 48_000, 4_800);
		assert!(!next.reset_sink);
		assert_eq!(next.silence, 0);
	}

	/// A dry ring refills to the target before the next arrival plays.
	#[test]
	fn a_dry_ring_restalls_to_the_target() {
		assert_eq!(fit(true, 0, 4_800, 960, 0, 960), Fit { pad: 3_840, skip: 0 });
		// A fresh sink starts on a little silence of its own, which counts.
		assert_eq!(fit(true, 2_400, 4_800, 960, 0, 960), Fit { pad: 1_440, skip: 0 });
		// A write longer than the target needs no padding, only the landing.
		assert_eq!(fit(true, 0, 960, 960, 0, 4_800), Fit { pad: 0, skip: 3_840 });
	}

	/// Ordinary cadence, a write on a ring at most the slack over the target, loses
	/// nothing, however long the write.
	#[test]
	fn cadence_within_the_slack_plays_everything() {
		assert_eq!(fit(false, 4_000, 4_800, 960, 0, 960), Fit { pad: 0, skip: 0 });
		assert_eq!(fit(false, 4_800, 4_800, 960, 0, 960), Fit { pad: 0, skip: 0 });
		// A frame that lands early, on a ring a device period over the target.
		assert_eq!(fit(false, 5_760, 4_800, 960, 0, 960), Fit { pad: 0, skip: 0 });
		assert_eq!(fit(false, 4_800, 4_800, 960, 0, 1_920), Fit { pad: 0, skip: 0 });
		// Running low is not running dry: nothing is padded until the ring is out.
		assert_eq!(fit(false, 480, 4_800, 960, 0, 960), Fit { pad: 0, skip: 0 });
	}

	/// A ring holding more than the slack over the target lands back on it,
	/// skipping as much of the write as that takes.
	#[test]
	fn excess_lands_on_the_target() {
		assert_eq!(fit(false, 5_761, 4_800, 960, 0, 960), Fit { pad: 0, skip: 960 });
		assert_eq!(fit(false, 6_000, 4_800, 960, 0, 2_400), Fit { pad: 0, skip: 1_200 });
		// A target that fell is reached by skipping, not by waiting to drain.
		assert_eq!(fit(false, 9_600, 4_800, 960, 0, 960), Fit { pad: 0, skip: 960 });
	}

	/// The silence standing in for a hole sits ahead of the frame, so it is what
	/// an over-full ring skips first; a ring the hole drained plays it through.
	#[test]
	fn a_hole_is_skipped_before_its_frame() {
		assert_eq!(fit(false, 4_800, 4_800, 960, 1_920, 960), Fit { pad: 0, skip: 1_920 });
		assert_eq!(fit(false, 4_000, 4_800, 960, 1_920, 960), Fit { pad: 0, skip: 1_120 });
		assert_eq!(fit(false, 2_880, 4_800, 960, 1_920, 960), Fit { pad: 0, skip: 0 });
	}
}
