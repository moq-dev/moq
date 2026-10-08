//! The origin's failover as a state machine.
//!
//! A [`Front`] is one path served through one broadcast: it picks the source it
//! serves from and splices each logical track from that source's copy (its readers
//! read the copy; see `super::resume`), re-splicing when the source is replaced and
//! refusing what a source will not serve. Everything here is plain data: [`Front::step`] takes one
//! [`Event`] and returns the [`Action`]s to perform, without locks or waiters.
//! The origin's driver task feeds events from the world (the route table, the
//! sources, the tracks, the clock) and executes the actions; see
//! `origin::run_front`.
//!
//! The driver only offers a front routes that serve its broadcast: those with the
//! epoch it first resolved, whose tracks resume where they stopped, or its first
//! route alone when that had no epoch, since nothing says another serves the
//! same bytes. A newer epoch supersedes the front, ending even the tracks in
//! flight, so readers re-request the new broadcast rather than stall on the old.
//!
//! Sources and tracks are named by ids and names, never handles, so a
//! transition can be checked in a unit test by comparing the actions it emits.

use std::{
	collections::{BTreeMap, HashSet},
	sync::Arc,
	time::Duration,
};

use crate::{Error, time::Instant, track};

/// A route the table selected for the front: the entry id, and whether it is a
/// broadcast published on this origin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Candidate {
	pub route: u64,
	pub local: bool,
}

/// Why an upstream request through a route did not produce a source.
#[derive(Clone, Debug)]
pub(super) struct Refusal {
	pub err: Error,
	/// Whether the route still wins the path. A refusal from a standing route
	/// is the path's answer and ends the front; one from a route that retracted
	/// or was beaten while pending only means the table moved.
	pub standing: bool,
}

/// What happened in the world.
#[derive(Clone, Debug)]
pub(super) enum Event {
	/// The routes covering the path were re-read: the best route, if
	/// any, and whether the serving source has begun closing.
	Selected {
		best: Option<Candidate>,
		serving_closing: bool,
	},
	/// The request through `route` resolved with a source (registered under the
	/// given id by the driver) or was refused.
	Resolved { route: u64, result: Result<u64, Refusal> },
	/// A source closed: it will never serve again.
	SourceClosed { source: u64 },
	/// The front's broadcast handed out a new logical track to serve. It has no
	/// reader until [`Event::Used`] says so.
	TrackAssigned { track: Arc<str>, now: Instant },
	/// A source answered a track query: its copy's metadata, or a refusal.
	/// `closing` is whether the source has begun closing, in which case a
	/// refusal is not held against it: it is on its way out.
	TrackInfo {
		track: Arc<str>,
		source: u64,
		closing: bool,
		result: Result<track::Info, Error>,
	},
	/// The spliced copy of a track ended: completed, or died. `delivered` is
	/// whether it produced anything since it was spliced in; a copy that died
	/// without delivering refused the track.
	TrackEnded {
		track: Arc<str>,
		source: u64,
		closing: bool,
		result: Result<(), Error>,
		delivered: bool,
	},
	/// A reader arrived on the track.
	Used { track: Arc<str> },
	/// The last reader left the track.
	Unused { track: Arc<str>, now: Instant },
	/// The armed deadline passed.
	Deadline { now: Instant },
	/// The driver let go of a track the machine asked it to [`Action::Forget`].
	/// Not fed when a reader arrived first: [`Event::Used`] follows instead.
	Forgotten { track: Arc<str> },
	/// A newer publisher instance replaced the broadcast: every track ends now,
	/// even one in flight, and readers re-request the path.
	Superseded,
	/// The origin is tearing down.
	Closed,
}

/// What the driver does in the world.
#[derive(Clone, Debug)]
pub(super) enum Action {
	/// Ask the route table for the best route and feed it back as
	/// [`Event::Selected`].
	Reselect,
	/// Request the path through `route`; feed the outcome back as
	/// [`Event::Resolved`].
	Request { route: u64 },
	/// Drop the driver's handle on a source.
	Detach { source: u64 },
	/// Resolve the requesters parked on the front: it has a source.
	Resolve,
	/// Ask `source` for its copy of `track`; feed the answer back as
	/// [`Event::TrackInfo`].
	Query { track: Arc<str>, source: u64 },
	/// Splice `source`'s copy of `track` in: each reader resumes on it where it stopped.
	Splice { track: Arc<str>, source: u64 },
	/// Drop the source copy of `track`: nobody reads it, and a returning reader gets a
	/// fresh one.
	Park { track: Arc<str> },
	/// Remove `track` from the broadcast and drop everything behind it, unless a reader
	/// arrived meanwhile: it went unread for the linger, or the source is local.
	/// Feed back [`Event::Forgotten`] once it is gone.
	Forget { track: Arc<str> },
	/// The logical track completed.
	Finish { track: Arc<str> },
	/// The logical track failed for good.
	Abort { track: Arc<str>, err: Error },
	/// Arm (or clear) the deadline the front wants to be woken at.
	Arm { at: Option<Instant> },
	/// The front is over: reject the parked requesters with `err`, leave each
	/// read track to end with the copy it is spliced from or still waiting on,
	/// abort the rest with `err`, and drop every source.
	End { err: Error },
}

/// Where a track stands with respect to its source copy.
#[derive(Clone, Debug, PartialEq, Eq)]
enum TrackState {
	/// No copy: nothing spliced, or the source that served it left.
	Idle,
	/// A source was asked for its copy.
	Querying { source: u64 },
	/// A source's copy is spliced in and being served.
	Spliced { source: u64 },
	/// Nobody reads it: the copy was dropped, and the track stays until the linger
	/// expires and it is forgotten.
	Parked { since: Instant },
}

#[derive(Clone, Debug)]
struct Track {
	state: TrackState,
	/// Sources that refused this track, by id. A refusal is authoritative for
	/// that source; a source that reattaches has a new id and is asked afresh.
	refused: HashSet<u64>,
	/// The most recent refusal, reported when every source has refused.
	refusal: Option<Error>,
	/// Whether anyone reads the track: a copy is only spliced in for a reader.
	used: bool,
	/// A detached source whose copy still feeds the track until the serving source's
	/// replaces it: a route being beaten keeps serving until then, and keeps serving if
	/// the newcomer refuses the track.
	draining: Option<u64>,
	/// The track finished or aborted: nothing is spliced again, and it stays only
	/// until it goes unread for the linger.
	ended: bool,
}

/// One front's state; see the module docs.
#[derive(Clone, Debug)]
pub(super) struct Front {
	/// The attached source and the route that produced it.
	serving: Option<(u64, Candidate)>,
	/// Whether the serving source has begun closing, as of the last event that
	/// said. A closing source is never asked for anything again.
	serving_closing: bool,
	/// The route an upstream request is in flight through.
	upstream: Option<Candidate>,
	/// Routes excluded from selection: their source ended while still advertised.
	excluded: HashSet<u64>,
	/// Why the last candidate fell through, reported if the front ends unresolved.
	last_err: Option<Error>,
	/// Whether the parked requesters were resolved (the first source attached).
	resolved: bool,
	tracks: BTreeMap<Arc<str>, Track>,
	/// Immutable track metadata, fixed by the first copy of each track. Every
	/// source of the broadcast must serve the same content.
	info: BTreeMap<Arc<str>, track::Info>,
	/// How long an unread track stays before it is forgotten.
	linger: Duration,
	/// The deadline last armed, so a step only re-arms on change.
	armed: Option<Instant>,
	ended: bool,
}

impl Front {
	/// A front with no source and no tracks; `linger` is how long an unread
	/// track stays before it is forgotten.
	pub(super) fn new(linger: Duration) -> Self {
		Self {
			serving: None,
			serving_closing: false,
			upstream: None,
			excluded: HashSet::new(),
			last_err: None,
			resolved: false,
			tracks: BTreeMap::new(),
			info: BTreeMap::new(),
			linger,
			armed: None,
			ended: false,
		}
	}

	/// The routes excluded from selection; the driver skips them.
	pub(super) fn excluded_routes(&self) -> &HashSet<u64> {
		&self.excluded
	}

	/// Forget excluded routes that left the table (a reconnect is a fresh entry).
	pub(super) fn retain_routes(&mut self, standing: impl Fn(u64) -> bool) {
		self.excluded.retain(|route| standing(*route));
	}

	/// The route the attached source came through, if any.
	pub(super) fn serving_route(&self) -> Option<u64> {
		self.serving.map(|(_, candidate)| candidate.route)
	}

	/// The attached source, if any.
	pub(super) fn serving(&self) -> Option<u64> {
		self.serving.map(|(source, _)| source)
	}

	/// Whether the front is over.
	#[cfg(test)]
	fn ended(&self) -> bool {
		self.ended
	}

	/// Apply one event and return what to do about it. Nothing after [`Action::End`].
	pub(super) fn step(&mut self, event: Event) -> Vec<Action> {
		let mut actions = Vec::new();
		if self.ended {
			return actions;
		}
		match event {
			Event::Selected { best, serving_closing } => self.selected(best, serving_closing, &mut actions),
			Event::Resolved { route, result } => self.resolved(route, result, &mut actions),
			Event::SourceClosed { source } => self.source_closed(source, &mut actions),
			// A fresh logical track, even under a name served before: an earlier
			// verdict belonged to that request, and a later one asks afresh. The
			// broadcast's track metadata is what persists. Unread until a reader
			// shows up, so a lookup whose reader left early is forgotten too.
			Event::TrackAssigned { track, now } => {
				self.tracks.insert(
					track,
					Track {
						state: TrackState::Parked { since: now },
						refused: HashSet::new(),
						refusal: None,
						used: false,
						ended: false,
						draining: None,
					},
				);
			}
			Event::TrackInfo {
				track,
				source,
				closing,
				result,
			} => self.track_info(track, source, closing, result, &mut actions),
			Event::TrackEnded {
				track,
				source,
				closing,
				result,
				delivered,
			} => self.track_ended(track, source, closing, result, delivered, &mut actions),
			Event::Used { track } => self.used(track, &mut actions),
			Event::Unused { track, now } => self.unused(track, now, &mut actions),
			Event::Deadline { now } => self.deadline(now, &mut actions),
			Event::Forgotten { track } => {
				self.tracks.remove(&track);
			}
			Event::Superseded => self.supersede(&mut actions),
			Event::Closed => self.end(Error::Dropped, &mut actions),
		}
		if !self.ended {
			let at = self.next_deadline();
			if at != self.armed {
				self.armed = at;
				actions.push(Action::Arm { at });
			}
		}
		actions
	}

	fn selected(&mut self, best: Option<Candidate>, serving_closing: bool, actions: &mut Vec<Action>) {
		if self.serving.is_some() {
			self.serving_closing = serving_closing;
		}
		let serving_route = self.serving.map(|(_, serving)| serving.route);
		match best {
			// The serving route is still the best one.
			Some(candidate) if Some(candidate.route) == serving_route => {
				self.upstream = None;
			}
			// Already asking through it.
			Some(candidate) if self.upstream.is_some_and(|upstream| upstream.route == candidate.route) => {}
			// A better (or replacement) route: request through it.
			Some(candidate) => {
				self.upstream = Some(candidate);
				actions.push(Action::Request { route: candidate.route });
			}
			// Nothing serves the path: the front is over, even with a live source. Its
			// route left the table and nothing replaced it, so it serves nobody new;
			// a later announcement gets a fresh front.
			None => {
				self.upstream = None;
				let err = match self.resolved {
					true => Error::Dropped,
					false => self.last_err.take().unwrap_or(Error::Unroutable),
				};
				self.end(err, actions);
			}
		}
	}

	fn resolved(&mut self, route: u64, result: Result<u64, Refusal>, actions: &mut Vec<Action>) {
		let Some(upstream) = self.upstream.filter(|upstream| upstream.route == route) else {
			// A source for a request we no longer want: let it go.
			if let Ok(source) = result {
				actions.push(Action::Detach { source });
			}
			return;
		};
		match result {
			Ok(source) => {
				self.upstream = None;
				self.attach(source, upstream, actions);
			}
			// The route retracted or was beaten before serving: the table already
			// reflects it, so the next selection asks the new winner. Its own error
			// is moot, so a front left with nothing reports the path unroutable.
			Err(Refusal { standing: false, .. }) => {
				self.upstream = None;
				self.last_err = Some(Error::Unroutable);
				actions.push(Action::Reselect);
			}
			// The winning route's answer is final, even while another source serves:
			// asking a sibling or a shorter prefix instead would turn one refusal into
			// a request per candidate.
			Err(Refusal { err, standing: true }) => {
				self.upstream = None;
				self.end(err, actions);
			}
		}
	}

	fn attach(&mut self, source: u64, candidate: Candidate, actions: &mut Vec<Action>) {
		if let Some((old, _)) = self.serving.take() {
			actions.push(Action::Detach { source: old });
		}
		// Every copy is from an older source, including one a track still drains from a
		// source the previous one replaced.
		self.drain_copies();
		self.serving = Some((source, candidate));
		self.serving_closing = false;
		if !self.resolved {
			self.resolved = true;
			actions.push(Action::Resolve);
		}
		// A read track is never parked, so idle is the only state to re-query.
		for (name, track) in &mut self.tracks {
			if track.used && !track.ended && track.state == TrackState::Idle {
				track.state = TrackState::Querying { source };
				actions.push(Action::Query {
					track: name.clone(),
					source,
				});
			}
		}
	}

	fn source_closed(&mut self, source: u64, actions: &mut Vec<Action>) {
		let Some((serving, candidate)) = self.serving else {
			return;
		};
		if serving != source {
			return;
		}
		// A standing route can outlive the source it produced. Asking it again
		// would re-request the broadcast that just ended; another route may still
		// resume it.
		self.excluded.insert(candidate.route);
		self.serving = None;
		self.serving_closing = false;
		actions.push(Action::Detach { source });
		self.drain_copies();
		self.last_err = Some(Error::Dropped);
		actions.push(Action::Reselect);
	}

	/// The serving source is going: a spliced copy keeps feeding its track (readers read it
	/// until a replacement's is spliced, or it runs out), and one still being asked for is
	/// dropped.
	fn drain_copies(&mut self) {
		for track in self.tracks.values_mut() {
			match track.state {
				TrackState::Spliced { source } => {
					track.draining = Some(source);
					track.state = TrackState::Idle;
				}
				TrackState::Querying { .. } => track.state = TrackState::Idle,
				TrackState::Idle | TrackState::Parked { .. } => {}
			}
		}
	}

	fn track_info(
		&mut self,
		name: Arc<str>,
		source: u64,
		closing: bool,
		result: Result<track::Info, Error>,
		actions: &mut Vec<Action>,
	) {
		let Some(track) = self.tracks.get_mut(&name) else {
			return;
		};
		if track.state != (TrackState::Querying { source }) {
			return;
		}
		let verdict = match result {
			// A copy claiming the same content with different metadata is
			// refused rather than reinterpreted.
			Ok(info) => match self.info.get(&name) {
				Some(expected)
					if expected.timescale != info.timescale
						|| expected.max_age != info.max_age
						|| expected.priority != info.priority =>
				{
					Err(Error::Unsupported)
				}
				Some(_) => Ok(()),
				None => {
					self.info.insert(name.clone(), info);
					Ok(())
				}
			},
			Err(err) => Err(err),
		};
		match verdict {
			Ok(()) => {
				track.draining = None;
				track.state = TrackState::Spliced { source };
				actions.push(Action::Splice {
					track: name.clone(),
					source,
				});
			}
			Err(err) => self.refuse(name, source, closing, err, actions),
		}
	}

	fn track_ended(
		&mut self,
		name: Arc<str>,
		source: u64,
		closing: bool,
		result: Result<(), Error>,
		delivered: bool,
		actions: &mut Vec<Action>,
	) {
		let Some(track) = self.tracks.get_mut(&name) else {
			return;
		};
		let spliced = track.state == (TrackState::Spliced { source });
		if track.draining == Some(source) {
			track.draining = None;
			// Every source of a front serves one broadcast, so a copy that completed ends
			// the track whoever serves it now. One that failed leaves the track to the
			// serving source.
			if result.is_ok() && !track.ended {
				track.state = TrackState::Idle;
				track.ended = true;
				actions.push(Action::Finish { track: name });
				return;
			}
			if !spliced {
				return;
			}
		}
		if !spliced {
			return;
		}
		match result {
			// Over for good: readers drain what it delivered, and it is forgotten
			// once unread for the linger, like any other track.
			Ok(()) => {
				track.state = TrackState::Idle;
				track.ended = true;
				actions.push(Action::Finish { track: name });
			}
			// Died mid-serve after delivering: normal failover, re-splice from
			// the serving source at the first missing group.
			Err(_) if delivered => {
				track.state = TrackState::Idle;
				self.redispatch(name, actions);
			}
			// Died before producing anything: a refusal.
			Err(err) => self.refuse(name, source, closing, err, actions),
		}
	}

	/// Record that `source` will not serve `name`, then either ask another
	/// source or, when the serving source itself refused, abort the track.
	fn refuse(&mut self, name: Arc<str>, source: u64, closing: bool, err: Error, actions: &mut Vec<Action>) {
		let track = self.tracks.get_mut(&name).expect("refusing a known track");
		track.state = TrackState::Idle;
		// A closing source's answer is not a verdict: it is on its way out and
		// its replacement is asked afresh.
		if closing {
			return;
		}
		track.refused.insert(source);
		track.refusal = Some(err);
		// A source being replaced still serves it: keep reading that copy, and the
		// refusal stands once it runs out.
		if let Some(draining) = track.draining {
			track.state = TrackState::Spliced { source: draining };
			return;
		}
		self.redispatch(name, actions);
	}

	/// Splice `name` from the serving source if it can serve it, or abort the
	/// track once the serving source has refused it: refusals are never
	/// retried, and no other source will be asked while this one serves.
	fn redispatch(&mut self, name: Arc<str>, actions: &mut Vec<Action>) {
		let Some((source, _)) = self.serving else {
			return;
		};
		let track = self.tracks.get_mut(&name).expect("dispatching a known track");
		if !track.used || track.ended || track.state != TrackState::Idle {
			return;
		}
		if track.refused.contains(&source) {
			let err = track.refusal.clone().unwrap_or(Error::NotFound);
			track.ended = true;
			actions.push(Action::Abort { track: name, err });
			return;
		}
		if self.serving_closing {
			return;
		}
		track.state = TrackState::Querying { source };
		actions.push(Action::Query { track: name, source });
	}

	fn used(&mut self, name: Arc<str>, actions: &mut Vec<Action>) {
		let Some(track) = self.tracks.get_mut(&name) else {
			return;
		};
		track.used = true;
		if let TrackState::Parked { .. } = track.state {
			track.state = TrackState::Idle;
		}
		self.redispatch(name, actions);
	}

	fn unused(&mut self, name: Arc<str>, now: Instant, actions: &mut Vec<Action>) {
		let Some(track) = self.tracks.get_mut(&name) else {
			return;
		};
		track.used = false;
		// Nothing reads it, so a copy it still drains goes with the park.
		track.draining = None;
		match track.state {
			// A local source is reached again for free: forget the track outright, and a
			// returning reader re-splices the source.
			TrackState::Spliced { source }
				if self
					.serving
					.is_some_and(|(serving, candidate)| serving == source && candidate.local) =>
			{
				track.state = TrackState::Idle;
				actions.push(Action::Forget { track: name });
			}
			// Drop the copy so the source goes idle at once; the track lingers.
			TrackState::Spliced { .. } => {
				track.state = TrackState::Parked { since: now };
				actions.push(Action::Park { track: name });
			}
			TrackState::Parked { .. } => {}
			// A copy still being asked for is dropped with the park.
			TrackState::Idle | TrackState::Querying { .. } => {
				track.state = TrackState::Parked { since: now };
				// An ended track has nothing to drop.
				if !track.ended {
					actions.push(Action::Park { track: name });
				}
			}
		}
	}

	fn deadline(&mut self, now: Instant, actions: &mut Vec<Action>) {
		// The driver's deadline fired and cleared itself: re-arm whatever is still
		// parked, even if nothing was due yet, or it would never be forgotten.
		// Idle until the driver confirms: a reader that arrived first keeps it.
		self.armed = None;
		for (name, track) in &mut self.tracks {
			if let TrackState::Parked { since } = track.state
				&& since + self.linger <= now
			{
				track.state = TrackState::Idle;
				actions.push(Action::Forget { track: name.clone() });
			}
		}
	}

	fn next_deadline(&self) -> Option<Instant> {
		self.tracks
			.values()
			.filter_map(|track| match track.state {
				TrackState::Parked { since } => Some(since + self.linger),
				_ => None,
			})
			.min()
	}

	/// Abort every track still going, then end: unlike a retraction, a newer
	/// broadcast does not leave readers on the old one's copies.
	fn supersede(&mut self, actions: &mut Vec<Action>) {
		for (name, track) in &mut self.tracks {
			if !track.ended {
				track.ended = true;
				actions.push(Action::Abort {
					track: name.clone(),
					err: Error::Unroutable,
				});
			}
		}
		self.end(Error::Unroutable, actions);
	}

	fn end(&mut self, err: Error, actions: &mut Vec<Action>) {
		if self.ended {
			return;
		}
		self.ended = true;
		// No Detach: the driver keeps the serving source's copies until End, so a
		// track still waiting on its info can be spliced to the copy it asked.
		self.serving = None;
		actions.push(Action::End { err });
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	const LINGER: Duration = Duration::from_secs(30);

	fn remote(route: u64) -> Candidate {
		Candidate { route, local: false }
	}

	fn local(route: u64) -> Candidate {
		Candidate { route, local: true }
	}

	fn info() -> track::Info {
		track::Info::default()
	}

	fn name(s: &str) -> Arc<str> {
		Arc::from(s)
	}

	/// Actions compare by their debug form: `Error` has no equality.
	#[track_caller]
	fn assert_actions(actual: Vec<Action>, expected: &[Action]) {
		assert_eq!(format!("{actual:?}"), format!("{expected:?}"));
	}

	/// A front serving `source` through `candidate`, with `video` spliced and read.
	fn serving(candidate: Candidate, source: u64) -> Front {
		let mut front = Front::new(LINGER);
		assert_actions(
			front.step(Event::Selected {
				best: Some(candidate),
				serving_closing: false,
			}),
			&[Action::Request { route: candidate.route }],
		);
		assert_actions(
			front.step(Event::Resolved {
				route: candidate.route,
				result: Ok(source),
			}),
			&[Action::Resolve],
		);
		let t0 = Instant::now();
		assert_actions(
			front.step(Event::TrackAssigned {
				track: name("video"),
				now: t0,
			}),
			&[Action::Arm { at: Some(t0 + LINGER) }],
		);
		assert_actions(
			front.step(Event::Used { track: name("video") }),
			&[
				Action::Query {
					track: name("video"),
					source,
				},
				Action::Arm { at: None },
			],
		);
		assert_actions(
			front.step(Event::TrackInfo {
				track: name("video"),
				source,
				closing: false,
				result: Ok(info()),
			}),
			&[Action::Splice {
				track: name("video"),
				source,
			}],
		);
		front
	}

	#[test]
	fn first_source_resolves_and_serves_read_tracks() {
		let front = serving(remote(1), 100);
		assert_eq!(front.serving(), Some(100));
	}

	#[test]
	fn nothing_routable_ends_an_unresolved_front() {
		let mut front = Front::new(LINGER);
		assert_actions(
			front.step(Event::Selected {
				best: None,
				serving_closing: false,
			}),
			&[Action::End { err: Error::Unroutable }],
		);
		assert!(front.ended());
		assert_actions(front.step(Event::Closed), &[]);
	}

	#[test]
	fn unchanged_selection_is_a_no_op() {
		let mut front = serving(remote(1), 100);
		assert_actions(
			front.step(Event::Selected {
				best: Some(remote(1)),
				serving_closing: false,
			}),
			&[],
		);
	}

	#[test]
	fn better_route_takes_over_and_resplices() {
		let mut front = serving(remote(1), 100);
		assert_actions(
			front.step(Event::Selected {
				best: Some(remote(2)),
				serving_closing: false,
			}),
			&[Action::Request { route: 2 }],
		);
		assert_actions(
			front.step(Event::Resolved {
				route: 2,
				result: Ok(200),
			}),
			&[
				Action::Detach { source: 100 },
				Action::Query {
					track: name("video"),
					source: 200,
				},
			],
		);
		assert_eq!(front.serving, Some((200, remote(2))));
	}

	#[test]
	fn a_stale_resolution_is_let_go() {
		let mut front = serving(remote(1), 100);
		front.step(Event::Selected {
			best: Some(remote(2)),
			serving_closing: false,
		});
		// The table moved back before the request landed.
		assert_actions(
			front.step(Event::Selected {
				best: Some(remote(1)),
				serving_closing: false,
			}),
			&[],
		);
		assert_actions(
			front.step(Event::Resolved {
				route: 2,
				result: Ok(200),
			}),
			&[Action::Detach { source: 200 }],
		);
	}

	#[test]
	fn dead_source_reselects_another_route() {
		let mut front = serving(remote(1), 100);
		assert_actions(
			front.step(Event::SourceClosed { source: 100 }),
			&[Action::Detach { source: 100 }, Action::Reselect],
		);
		assert_eq!(front.excluded_routes(), &HashSet::from([1]));
		assert_actions(
			front.step(Event::Selected {
				best: Some(remote(3)),
				serving_closing: false,
			}),
			&[Action::Request { route: 3 }],
		);
		assert_actions(
			front.step(Event::Resolved {
				route: 3,
				result: Ok(300),
			}),
			&[Action::Query {
				track: name("video"),
				source: 300,
			}],
		);
	}

	/// A live source does not keep a front alive once its route is gone: nothing
	/// new may be served from content nobody announces.
	#[test]
	fn retracted_route_with_no_replacement_ends_a_live_front() {
		let mut front = serving(local(1), 100);
		assert_actions(
			front.step(Event::Selected {
				best: None,
				serving_closing: false,
			}),
			&[Action::End { err: Error::Dropped }],
		);
	}

	#[test]
	fn dead_source_with_no_replacement_ends() {
		let mut front = serving(remote(1), 100);
		front.step(Event::SourceClosed { source: 100 });
		assert_actions(
			front.step(Event::Selected {
				best: None,
				serving_closing: false,
			}),
			&[Action::End { err: Error::Dropped }],
		);
	}

	fn refuse(front: &mut Front, route: u64) -> Vec<Action> {
		front.step(Event::Resolved {
			route,
			result: Err(Refusal {
				err: Error::NotFound,
				standing: true,
			}),
		})
	}

	/// The winning route's refusal is the answer: no other route is asked.
	#[test]
	fn standing_refusal_ends_the_front() {
		let mut front = Front::new(LINGER);
		front.step(Event::Selected {
			best: Some(remote(1)),
			serving_closing: false,
		});
		assert_actions(refuse(&mut front, 1), &[Action::End { err: Error::NotFound }]);
		assert!(front.ended());
	}

	/// The serving source died and its replacement refuses the path: the front ends with
	/// that refusal rather than trying a third route.
	#[test]
	fn standing_refusal_after_the_source_died_ends_the_front() {
		let mut front = serving(remote(1), 100);
		front.step(Event::SourceClosed { source: 100 });
		front.step(Event::Selected {
			best: Some(remote(2)),
			serving_closing: false,
		});
		assert_actions(refuse(&mut front, 2), &[Action::End { err: Error::NotFound }]);
	}

	/// A better route refusing ends the front even while another source serves: its read
	/// tracks finish on the copies they are spliced from, and a new request gets the refusal.
	#[test]
	fn standing_refusal_while_serving_ends_the_front() {
		let mut front = serving(remote(1), 100);
		front.step(Event::Selected {
			best: Some(remote(2)),
			serving_closing: false,
		});
		assert_actions(refuse(&mut front, 2), &[Action::End { err: Error::NotFound }]);
		assert!(front.excluded_routes().is_empty());
	}

	#[test]
	fn retracted_route_is_not_a_refusal() {
		let mut front = Front::new(LINGER);
		front.step(Event::Selected {
			best: Some(remote(1)),
			serving_closing: false,
		});
		assert_actions(
			front.step(Event::Resolved {
				route: 1,
				result: Err(Refusal {
					err: Error::Unroutable,
					standing: false,
				}),
			}),
			&[Action::Reselect],
		);
		assert!(front.excluded_routes().is_empty());
		assert!(!front.ended());
	}

	#[test]
	fn a_source_refusing_a_track_aborts_it_and_nothing_else() {
		let mut front = serving(remote(1), 100);
		front.step(Event::TrackAssigned {
			track: name("audio"),
			now: Instant::now(),
		});
		front.step(Event::Used { track: name("audio") });
		assert_actions(
			front.step(Event::TrackInfo {
				track: name("audio"),
				source: 100,
				closing: false,
				result: Err(Error::NotFound),
			}),
			&[Action::Abort {
				track: name("audio"),
				err: Error::NotFound,
			}],
		);
		assert_eq!(front.tracks[&name("video")].state, TrackState::Spliced { source: 100 });
	}

	#[test]
	fn a_closing_source_refusal_is_not_a_verdict() {
		let mut front = serving(remote(1), 100);
		front.step(Event::TrackAssigned {
			track: name("audio"),
			now: Instant::now(),
		});
		front.step(Event::Used { track: name("audio") });
		assert_actions(
			front.step(Event::TrackInfo {
				track: name("audio"),
				source: 100,
				closing: true,
				result: Err(Error::NotFound),
			}),
			&[],
		);
		assert!(front.tracks[&name("audio")].refused.is_empty());
		// The replacement is asked afresh.
		front.step(Event::SourceClosed { source: 100 });
		front.step(Event::Selected {
			best: Some(remote(2)),
			serving_closing: false,
		});
		let actions = front.step(Event::Resolved {
			route: 2,
			result: Ok(200),
		});
		assert!(
			actions
				.iter()
				.any(|action| matches!(action, Action::Query { track, source: 200 } if track.as_ref() == "audio"))
		);
	}

	#[test]
	fn a_copy_dying_after_delivering_resplices() {
		let mut front = serving(remote(1), 100);
		assert_actions(
			front.step(Event::TrackEnded {
				track: name("video"),
				source: 100,
				closing: false,
				result: Err(Error::Dropped),
				delivered: true,
			}),
			&[Action::Query {
				track: name("video"),
				source: 100,
			}],
		);
	}

	#[test]
	fn a_copy_dying_before_delivering_is_a_refusal() {
		let mut front = serving(remote(1), 100);
		assert_actions(
			front.step(Event::TrackEnded {
				track: name("video"),
				source: 100,
				closing: false,
				result: Err(Error::Dropped),
				delivered: false,
			}),
			&[Action::Abort {
				track: name("video"),
				err: Error::Dropped,
			}],
		);
	}

	#[test]
	fn a_completed_copy_finishes_the_track() {
		let mut front = serving(remote(1), 100);
		assert_actions(
			front.step(Event::TrackEnded {
				track: name("video"),
				source: 100,
				closing: false,
				result: Ok(()),
				delivered: true,
			}),
			&[Action::Finish { track: name("video") }],
		);
	}

	#[test]
	fn incompatible_copy_is_refused() {
		let mut front = serving(remote(1), 100);
		front.step(Event::Selected {
			best: Some(remote(2)),
			serving_closing: false,
		});
		front.step(Event::Resolved {
			route: 2,
			result: Ok(200),
		});
		let other = track::Info {
			max_age: Some(Duration::from_secs(1)),
			..track::Info::default()
		};
		// The source it replaced keeps feeding the track (the audit's F3)...
		assert_actions(
			front.step(Event::TrackInfo {
				track: name("video"),
				source: 200,
				closing: false,
				result: Ok(other),
			}),
			&[],
		);
		// ...and the refusal stands once that runs out.
		assert_actions(
			front.step(Event::TrackEnded {
				track: name("video"),
				source: 100,
				closing: false,
				result: Err(Error::Dropped),
				delivered: true,
			}),
			&[Action::Abort {
				track: name("video"),
				err: Error::Unsupported,
			}],
		);
	}

	#[test]
	fn unread_track_parks_then_is_forgotten_after_the_linger() {
		let mut front = serving(remote(1), 100);
		let t0 = Instant::now();
		assert_actions(
			front.step(Event::Unused {
				track: name("video"),
				now: t0,
			}),
			&[
				Action::Park { track: name("video") },
				Action::Arm { at: Some(t0 + LINGER) },
			],
		);
		// Too early: nothing released, but the deadline is armed again since the
		// driver clears it on firing.
		assert_actions(
			front.step(Event::Deadline { now: t0 + LINGER / 2 }),
			&[Action::Arm { at: Some(t0 + LINGER) }],
		);
		// The driver cleared the fired deadline itself; nothing is parked, so
		// nothing re-arms.
		assert_actions(
			front.step(Event::Deadline { now: t0 + LINGER }),
			&[Action::Forget { track: name("video") }],
		);
		assert_actions(front.step(Event::Forgotten { track: name("video") }), &[]);
		assert!(!front.tracks.contains_key(&name("video")));
	}

	/// A reader that looked the track up before the driver could forget it keeps
	/// it: the driver feeds `Used` instead of `Forgotten`, and the track re-splices.
	#[test]
	fn a_reader_racing_the_forget_keeps_the_track() {
		let mut front = serving(remote(1), 100);
		let t0 = Instant::now();
		front.step(Event::Unused {
			track: name("video"),
			now: t0,
		});
		assert_actions(
			front.step(Event::Deadline { now: t0 + LINGER }),
			&[Action::Forget { track: name("video") }],
		);
		assert_actions(
			front.step(Event::Used { track: name("video") }),
			&[Action::Query {
				track: name("video"),
				source: 100,
			}],
		);
	}

	/// A finished track stays for readers still draining it and for the linger
	/// after the last one, is never spliced again, and is then forgotten.
	#[test]
	fn a_finished_track_lingers_then_is_forgotten() {
		let mut front = serving(remote(1), 100);
		assert_actions(
			front.step(Event::TrackEnded {
				track: name("video"),
				source: 100,
				closing: false,
				result: Ok(()),
				delivered: true,
			}),
			&[Action::Finish { track: name("video") }],
		);
		let t0 = Instant::now();
		assert_actions(
			front.step(Event::Unused {
				track: name("video"),
				now: t0,
			}),
			&[Action::Arm { at: Some(t0 + LINGER) }],
		);
		// A returning reader reads what it holds: no query, and the linger waits.
		assert_actions(
			front.step(Event::Used { track: name("video") }),
			&[Action::Arm { at: None }],
		);
		// A replacement source does not re-splice it either.
		front.step(Event::Selected {
			best: Some(remote(2)),
			serving_closing: false,
		});
		assert_actions(
			front.step(Event::Resolved {
				route: 2,
				result: Ok(200),
			}),
			&[Action::Detach { source: 100 }],
		);
		let t1 = t0 + LINGER;
		front.step(Event::Unused {
			track: name("video"),
			now: t1,
		});
		assert_actions(
			front.step(Event::Deadline { now: t1 + LINGER }),
			&[Action::Forget { track: name("video") }],
		);
		front.step(Event::Forgotten { track: name("video") });
		assert!(!front.tracks.contains_key(&name("video")));
	}

	/// An aborted track lingers the same way rather than staying for the life of
	/// the front.
	#[test]
	fn an_aborted_track_lingers_then_is_forgotten() {
		let mut front = serving(remote(1), 100);
		front.step(Event::TrackEnded {
			track: name("video"),
			source: 100,
			closing: false,
			result: Err(Error::Dropped),
			delivered: false,
		});
		let t0 = Instant::now();
		assert_actions(
			front.step(Event::Unused {
				track: name("video"),
				now: t0,
			}),
			&[Action::Arm { at: Some(t0 + LINGER) }],
		);
		assert_actions(
			front.step(Event::Deadline { now: t0 + LINGER }),
			&[Action::Forget { track: name("video") }],
		);
		front.step(Event::Forgotten { track: name("video") });
		assert!(!front.tracks.contains_key(&name("video")));
	}

	/// A local source keeps its own cache, so an unread track is forgotten at once.
	#[test]
	fn an_unread_local_track_is_forgotten_at_once() {
		let mut front = serving(local(1), 100);
		assert_actions(
			front.step(Event::Unused {
				track: name("video"),
				now: Instant::now(),
			}),
			&[Action::Forget { track: name("video") }],
		);
	}

	#[test]
	fn a_returning_reader_cancels_the_linger() {
		let mut front = serving(remote(1), 100);
		let t0 = Instant::now();
		front.step(Event::Unused {
			track: name("video"),
			now: t0,
		});
		assert_actions(
			front.step(Event::Used { track: name("video") }),
			&[
				Action::Query {
					track: name("video"),
					source: 100,
				},
				Action::Arm { at: None },
			],
		);
	}

	/// A track whose reader left before the front saw it is never spliced, and is
	/// forgotten after the linger rather than kept as a stub.
	#[test]
	fn unread_track_is_never_spliced() {
		let mut front = serving(remote(1), 100);
		let t0 = Instant::now();
		front.step(Event::TrackAssigned {
			track: name("audio"),
			now: t0,
		});
		assert_eq!(front.tracks[&name("audio")].state, TrackState::Parked { since: t0 });
		assert_actions(
			front.step(Event::Deadline { now: t0 + LINGER }),
			&[Action::Forget { track: name("audio") }],
		);
	}

	/// A newer publisher at the path takes over, local or not, and closing or not: the
	/// path is the same broadcast.
	#[test]
	fn a_newcomer_takes_over_the_incumbent() {
		for closing in [false, true] {
			let mut front = serving(local(1), 100);
			assert_actions(
				front.step(Event::Selected {
					best: Some(remote(2)),
					serving_closing: closing,
				}),
				&[Action::Request { route: 2 }],
			);
		}
	}

	/// The serving source ending resumes through whatever else serves the path.
	#[test]
	fn the_incumbent_ending_resumes_elsewhere() {
		let mut front = serving(local(1), 100);
		assert_actions(
			front.step(Event::SourceClosed { source: 100 }),
			&[Action::Detach { source: 100 }, Action::Reselect],
		);
		assert_actions(
			front.step(Event::Selected {
				best: Some(remote(2)),
				serving_closing: false,
			}),
			&[Action::Request { route: 2 }],
		);
	}

	#[test]
	fn teardown_ends_everything() {
		let mut front = serving(remote(1), 100);
		assert_actions(front.step(Event::Closed), &[Action::End { err: Error::Dropped }]);
	}

	/// Every sequence of events up to a small depth, over a small alphabet,
	/// checked against the invariants the driver relies on.
	#[test]
	fn bounded_sequences_hold_the_invariants() {
		let t0 = Instant::now();
		let alphabet: Vec<Event> = vec![
			Event::Selected {
				best: Some(remote(1)),
				serving_closing: false,
			},
			Event::Selected {
				best: Some(remote(2)),
				serving_closing: false,
			},
			Event::Selected {
				best: None,
				serving_closing: false,
			},
			Event::Resolved {
				route: 1,
				result: Ok(100),
			},
			Event::Resolved {
				route: 2,
				result: Ok(200),
			},
			Event::Resolved {
				route: 2,
				result: Err(Refusal {
					err: Error::NotFound,
					standing: true,
				}),
			},
			Event::SourceClosed { source: 100 },
			Event::TrackAssigned {
				track: name("v"),
				now: t0,
			},
			Event::Used { track: name("v") },
			Event::Unused {
				track: name("v"),
				now: t0,
			},
			Event::TrackInfo {
				track: name("v"),
				source: 100,
				closing: false,
				result: Ok(info()),
			},
			Event::TrackInfo {
				track: name("v"),
				source: 100,
				closing: false,
				result: Err(Error::NotFound),
			},
			Event::TrackEnded {
				track: name("v"),
				source: 100,
				closing: false,
				result: Err(Error::Dropped),
				delivered: false,
			},
			Event::Deadline { now: t0 + LINGER },
			Event::Forgotten { track: name("v") },
		];

		fn walk(front: &Front, alphabet: &[Event], depth: usize, sequences: &mut usize) {
			for event in alphabet {
				// Ids are minted once per resolution, so a source can never resolve
				// while it is already serving; the alphabet reuses ids for brevity.
				if let Event::Resolved { result: Ok(source), .. } = event
					&& front.serving.map(|(serving, _)| serving) == Some(*source)
				{
					continue;
				}
				let mut next = front.clone();
				let before = format!("{next:?}");
				let actions = next.step(event.clone());
				*sequences += 1;

				// An event that changes nothing produces nothing, except letting
				// go of a source the front never took.
				if format!("{next:?}") == before {
					assert!(
						actions.iter().all(|action| matches!(action, Action::Detach { .. })),
						"no-op event {event:?} produced {actions:?}"
					);
				}
				// Nothing after the end.
				if front.ended() {
					assert!(actions.is_empty());
				}
				// A source is only ever asked for a track it has not refused.
				for action in &actions {
					if let Action::Query { track, source } = action {
						assert!(!next.tracks[track].refused.contains(source));
					}
				}
				// At most one takeover per track per event.
				let splices = actions
					.iter()
					.filter(|action| matches!(action, Action::Splice { .. }))
					.count();
				assert!(splices <= 1);
				// A Detach names a source the front no longer serves from.
				for action in &actions {
					if let Action::Detach { source } = action {
						assert_ne!(next.serving.map(|(s, _)| s), Some(*source));
					}
				}
				if depth > 1 {
					walk(&next, alphabet, depth - 1, sequences);
				}
			}
		}

		let mut sequences = 0;
		walk(&Front::new(LINGER), &alphabet, 5, &mut sequences);
		assert!(sequences > 100_000);
	}

	/// The newcomer refused the track, so it stays on the source the newcomer replaced.
	/// A third source still asks for it: every copy the front holds is older than it.
	#[test]
	fn a_new_source_queries_a_track_draining_an_older_one() {
		let mut front = serving(remote(1), 100);
		front.step(Event::Selected {
			best: Some(remote(2)),
			serving_closing: false,
		});
		front.step(Event::Resolved {
			route: 2,
			result: Ok(200),
		});
		front.step(Event::TrackInfo {
			track: name("video"),
			source: 200,
			closing: false,
			result: Err(Error::NotFound),
		});
		front.step(Event::Selected {
			best: Some(remote(3)),
			serving_closing: false,
		});
		assert_actions(
			front.step(Event::Resolved {
				route: 3,
				result: Ok(300),
			}),
			&[
				Action::Detach { source: 200 },
				Action::Query {
					track: name("video"),
					source: 300,
				},
			],
		);
	}

	/// The newcomer refused the track, so it drained the incumbent's copy. Going unread
	/// parks the track and drops that copy, so a later refusal cannot put the track back
	/// on a copy nothing feeds any more.
	#[test]
	fn an_unread_track_lets_go_of_the_copy_it_drains() {
		let mut front = serving(remote(1), 100);
		front.step(Event::Selected {
			best: Some(remote(2)),
			serving_closing: false,
		});
		front.step(Event::Resolved {
			route: 2,
			result: Ok(200),
		});
		front.step(Event::TrackInfo {
			track: name("video"),
			source: 200,
			closing: false,
			result: Err(Error::NotFound),
		});
		let t0 = Instant::now();
		assert_actions(
			front.step(Event::Unused {
				track: name("video"),
				now: t0,
			}),
			&[
				Action::Park { track: name("video") },
				Action::Arm { at: Some(t0 + LINGER) },
			],
		);
		front.step(Event::Selected {
			best: Some(remote(3)),
			serving_closing: false,
		});
		front.step(Event::Resolved {
			route: 3,
			result: Ok(300),
		});
		front.step(Event::Used { track: name("video") });
		let actions = front.step(Event::TrackInfo {
			track: name("video"),
			source: 300,
			closing: false,
			result: Err(Error::NotFound),
		});
		assert_ne!(
			front.tracks[&name("video")].state,
			TrackState::Spliced { source: 100 },
			"spliced to a copy the park dropped; actions {actions:?}"
		);
	}

	/// A track draining the incumbent while the newcomer's answer is pending goes unread:
	/// the copy it drains is parked at once, like any unread track's.
	#[test]
	fn a_draining_track_parks_its_copy_when_unread() {
		let mut front = serving(remote(1), 100);
		front.step(Event::Selected {
			best: Some(remote(2)),
			serving_closing: false,
		});
		front.step(Event::Resolved {
			route: 2,
			result: Ok(200),
		});
		let t0 = Instant::now();
		assert_actions(
			front.step(Event::Unused {
				track: name("video"),
				now: t0,
			}),
			&[
				Action::Park { track: name("video") },
				Action::Arm { at: Some(t0 + LINGER) },
			],
		);
	}

	/// A front that served ends `Dropped` when its route leaves, not with the retraction
	/// of a challenger that never took over.
	#[test]
	fn a_front_that_served_ends_dropped() {
		let mut front = serving(remote(1), 100);
		front.step(Event::Selected {
			best: Some(remote(2)),
			serving_closing: false,
		});
		front.step(Event::Resolved {
			route: 2,
			result: Err(Refusal {
				err: Error::Unroutable,
				standing: false,
			}),
		});
		front.step(Event::Selected {
			best: Some(remote(1)),
			serving_closing: false,
		});
		assert_actions(
			front.step(Event::Selected {
				best: None,
				serving_closing: false,
			}),
			&[Action::End { err: Error::Dropped }],
		);
	}

	/// The serving source closed and the front reselected, but the pump still reads the
	/// closed source's copy. That copy ending cleanly ends the track: every source of
	/// the front serves one broadcast.
	#[test]
	fn a_closed_sources_clean_end_finishes_the_track() {
		let mut front = serving(remote(1), 100);
		front.step(Event::SourceClosed { source: 100 });
		front.step(Event::Selected {
			best: Some(remote(2)),
			serving_closing: false,
		});
		assert_actions(
			front.step(Event::TrackEnded {
				track: name("video"),
				source: 100,
				closing: false,
				result: Ok(()),
				delivered: true,
			}),
			&[Action::Finish { track: name("video") }],
		);
	}
}
