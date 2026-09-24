//! The origin's failover as a state machine.
//!
//! A [`Front`] is one path served through a spliced broadcast: it picks the
//! source it serves from and splices each logical track from that source's
//! copy, re-splicing when the source is replaced and refusing what a source
//! will not serve. Everything here is plain data: [`Front::step`] takes one
//! [`Event`] and returns the [`Action`]s to perform, without locks or waiters.
//! The origin's driver task feeds events from the world (the route table, the
//! sources, the tracks, the clock) and executes the actions; see
//! `origin::run_front`.
//!
//! Sources and tracks are named by ids and names, never handles, so a
//! transition can be checked in a unit test by comparing the actions it emits.

use std::{
	collections::{BTreeMap, HashSet},
	sync::Arc,
	time::Duration,
};

use crate::{Error, Hop, runtime::Instant, track};

/// A route the table selected for the front: the entry id, the endpoint that
/// originated it, and whether it is a broadcast published on this origin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Candidate {
	pub route: u64,
	pub first: Option<Hop>,
	pub local: bool,
}

/// Why an upstream request through a route did not produce a source.
#[derive(Clone, Debug)]
pub(super) struct Refusal {
	pub err: Error,
	/// Whether the route is still in the table. A refusal from a standing route
	/// is the handler's answer and is never re-asked; one from a retracted route
	/// only means the table moved.
	pub standing: bool,
}

/// What happened in the world.
#[derive(Clone, Debug)]
pub(super) enum Event {
	/// The routes covering the path were re-read: the best qualifying route, if
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
	/// The spliced broadcast handed out a new logical track to serve.
	TrackAssigned { track: Arc<str> },
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
	/// The origin is tearing down.
	Closed,
}

/// What the driver does in the world.
#[derive(Clone, Debug)]
pub(super) enum Action {
	/// Ask the route table for the best qualifying route and feed it back as
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
	/// Splice `source`'s copy of `track` in, resuming where the segments stop.
	Splice { track: Arc<str>, source: u64 },
	/// Drop the source copy of `track` but keep the delivered groups spliced,
	/// so resume stays seamless while nobody reads.
	Park { track: Arc<str> },
	/// Drop the delivered groups of `track` too: the linger expired unread.
	Release { track: Arc<str> },
	/// The logical track completed.
	Finish { track: Arc<str> },
	/// The logical track failed for good.
	Abort { track: Arc<str>, err: Error },
	/// Arm (or clear) the deadline the front wants to be woken at.
	Arm { at: Option<Instant> },
	/// The front is over: reject the parked requesters with `err`, leave each
	/// read track to end with the copy it is spliced from, and abort the rest
	/// with `err`.
	End { err: Error },
}

/// The content identity of a front: who its first source came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Identity {
	/// No source has attached yet: the first request may resolve through any
	/// covering route, and whoever serves it fixes the identity.
	Undetermined,
	/// The first source was a broadcast published on this origin. A newer local
	/// publisher takes over while the incumbent is live; once the incumbent is
	/// closing the front ends instead, so a newcomer gets a fresh broadcast
	/// rather than being spliced into one that is over.
	Local,
	/// The serving route's first hop was absent or [`Hop::UNKNOWN`], which
	/// identifies nobody: the front cannot resume, so its source ending ends it.
	Anonymous,
	/// The first hop of the serving route. Routes sharing it are the same
	/// origin reached another way and safe to resume through.
	Publisher(Hop),
}

/// Which routes qualify for a front's (re)selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Pin {
	/// Any served route.
	Any,
	/// Only broadcasts published on this origin.
	Local,
	/// Only routes originated by this first hop.
	Publisher(Hop),
	/// Nothing: the front never re-selects.
	None,
}

impl Identity {
	fn pin(self) -> Pin {
		match self {
			Self::Undetermined => Pin::Any,
			Self::Local => Pin::Local,
			Self::Anonymous => Pin::None,
			Self::Publisher(hop) => Pin::Publisher(hop),
		}
	}
}

/// Where a track stands with respect to its source copy.
#[derive(Clone, Debug, PartialEq, Eq)]
enum TrackState {
	/// No copy: nothing spliced, or the segment stays spliced from a source
	/// that left.
	Idle,
	/// A source was asked for its copy.
	Querying { source: u64 },
	/// A source's copy is spliced in and being served.
	Spliced { source: u64 },
	/// Nobody reads it: the copy was dropped, the delivered groups stay
	/// spliced until the linger expires.
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
}

/// One front's state; see the module docs.
#[derive(Clone, Debug)]
pub(super) struct Front {
	identity: Identity,
	/// The attached source and the route that produced it.
	serving: Option<(u64, u64)>,
	/// Whether the serving source has begun closing, as of the last event that
	/// said. A closing source is never asked for anything again.
	serving_closing: bool,
	/// The route an upstream request is in flight through.
	upstream: Option<u64>,
	/// Routes that refused the path while another source was serving.
	refused: HashSet<u64>,
	/// Why the last candidate fell through, reported if the front ends unresolved.
	last_err: Option<Error>,
	/// Whether the parked requesters were resolved (the first source attached).
	resolved: bool,
	tracks: BTreeMap<Arc<str>, Track>,
	/// Immutable track metadata, fixed by the first copy of each track. Every
	/// source of the broadcast must serve the same content.
	info: BTreeMap<Arc<str>, track::Info>,
	/// How long an unread track keeps its delivered groups spliced.
	linger: Duration,
	/// The deadline last armed, so a step only re-arms on change.
	armed: Option<Instant>,
	ended: bool,
}

impl Front {
	/// A front with no source and no tracks; `linger` is how long an unread
	/// track keeps its delivered groups spliced.
	pub(super) fn new(linger: Duration) -> Self {
		Self {
			identity: Identity::Undetermined,
			serving: None,
			serving_closing: false,
			upstream: None,
			refused: HashSet::new(),
			last_err: None,
			resolved: false,
			tracks: BTreeMap::new(),
			info: BTreeMap::new(),
			linger,
			armed: None,
			ended: false,
		}
	}

	/// Which routes qualify for the next selection.
	pub(super) fn pin(&self) -> Pin {
		self.identity.pin()
	}

	/// The routes that refused the path; the driver skips them when selecting.
	pub(super) fn refused_routes(&self) -> &HashSet<u64> {
		&self.refused
	}

	/// Forget refused routes that left the table (a reconnect is a fresh entry).
	pub(super) fn retain_routes(&mut self, standing: impl Fn(u64) -> bool) {
		self.refused.retain(|route| standing(*route));
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
			// broadcast's track metadata is what persists.
			Event::TrackAssigned { track } => {
				self.tracks.insert(
					track,
					Track {
						state: TrackState::Idle,
						refused: HashSet::new(),
						refusal: None,
						used: false,
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
		let serving_route = self.serving.map(|(_, route)| route);
		match best {
			// The serving route is still the best qualifying one.
			Some(candidate) if Some(candidate.route) == serving_route => {
				self.upstream = None;
			}
			// Already asking through it.
			Some(candidate) if self.upstream == Some(candidate.route) => {}
			// A local newcomer must not be spliced into a broadcast whose
			// publisher is on the way out: end, and a fresh front serves it.
			Some(candidate) if candidate.local && self.identity == Identity::Local && serving_closing => {
				self.end(Error::Dropped, actions);
			}
			// A better (or replacement) qualifying route: request through it.
			Some(candidate) => {
				self.upstream = Some(candidate.route);
				actions.push(Action::Request { route: candidate.route });
			}
			// Nothing qualifies. A live source keeps serving (its route may
			// return); without one the front is over.
			None => {
				self.upstream = None;
				if self.serving.is_none() {
					let err = match self.identity {
						Identity::Undetermined => self.last_err.take().unwrap_or(Error::Unroutable),
						_ => self.last_err.take().unwrap_or(Error::Dropped),
					};
					self.end(err, actions);
				}
			}
		}
	}

	fn resolved(&mut self, route: u64, result: Result<u64, Refusal>, actions: &mut Vec<Action>) {
		match result {
			// A source for a request we no longer want: let it go.
			Ok(source) if self.upstream != Some(route) => actions.push(Action::Detach { source }),
			Ok(source) => {
				self.upstream = None;
				self.attach(source, route, actions);
			}
			Err(_) if self.upstream != Some(route) => {}
			// The route retracted before serving: the table already reflects
			// it, so the next selection retries the survivor.
			Err(Refusal { standing: false, .. }) => {
				self.upstream = None;
				self.last_err = Some(Error::Unroutable);
				actions.push(Action::Reselect);
			}
			// An authoritative refusal of the path. It ends a front with no
			// other source; a serving front merely skips the refuser.
			Err(Refusal { err, standing: true }) => {
				self.upstream = None;
				match self.serving {
					Some(_) => {
						self.refused.insert(route);
						self.last_err = Some(err);
						actions.push(Action::Reselect);
					}
					None => self.end(err, actions),
				}
			}
		}
	}

	fn attach(&mut self, source: u64, route: u64, actions: &mut Vec<Action>) {
		if let Some((old, _)) = self.serving.take() {
			actions.push(Action::Detach { source: old });
			// Its copies are gone with it; the segments they delivered stay
			// spliced and the replacement resumes past them.
			for track in self.tracks.values_mut() {
				if matches!(track.state, TrackState::Querying { source: s } | TrackState::Spliced { source: s } if s == old)
				{
					track.state = TrackState::Idle;
				}
			}
		}
		self.serving = Some((source, route));
		self.serving_closing = false;
		if !self.resolved {
			self.resolved = true;
			actions.push(Action::Resolve);
		}
		// A read track is never parked, so idle is the only state to re-query.
		for (name, track) in &mut self.tracks {
			if track.used && track.state == TrackState::Idle {
				track.state = TrackState::Querying { source };
				actions.push(Action::Query {
					track: name.clone(),
					source,
				});
			}
		}
	}

	/// Fix the identity from the first source's candidate. Called by the driver
	/// with the candidate it requested through, before the source resolves.
	pub(super) fn identify(&mut self, candidate: Candidate) {
		if self.identity != Identity::Undetermined {
			return;
		}
		self.identity = match (candidate.local, candidate.first) {
			(true, _) => Identity::Local,
			(false, Some(hop)) if hop != Hop::UNKNOWN => Identity::Publisher(hop),
			(false, _) => Identity::Anonymous,
		};
	}

	fn source_closed(&mut self, source: u64, actions: &mut Vec<Action>) {
		let Some((serving, _)) = self.serving else {
			return;
		};
		if serving != source {
			return;
		}
		self.serving = None;
		self.serving_closing = false;
		actions.push(Action::Detach { source });
		for track in self.tracks.values_mut() {
			if matches!(track.state, TrackState::Querying { source: s } | TrackState::Spliced { source: s } if s == source)
			{
				track.state = TrackState::Idle;
			}
		}
		self.last_err = Some(Error::Dropped);
		match self.identity {
			// A local publisher ending ends its broadcast; a newcomer at the
			// path gets a fresh one. An anonymous source can never be resumed.
			Identity::Local | Identity::Anonymous | Identity::Undetermined => self.end(Error::Dropped, actions),
			Identity::Publisher(_) => actions.push(Action::Reselect),
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
		if track.state != (TrackState::Spliced { source }) {
			return;
		}
		match result {
			// Over for good: the track leaves the machine, so a name served again
			// later starts afresh and a long-lived front does not keep a stub per
			// name it ever served.
			Ok(()) => {
				self.tracks.remove(&name);
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
		if !track.used || track.state != TrackState::Idle {
			return;
		}
		if track.refused.contains(&source) {
			let err = track.refusal.clone().unwrap_or(Error::NotFound);
			self.tracks.remove(&name);
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
		match track.state {
			// Drop the copy so the source goes idle at once; the delivered
			// groups stay spliced for the linger.
			TrackState::Spliced { .. } => {
				track.state = TrackState::Parked { since: now };
				actions.push(Action::Park { track: name });
			}
			TrackState::Querying { .. } => track.state = TrackState::Idle,
			_ => {}
		}
	}

	fn deadline(&mut self, now: Instant, actions: &mut Vec<Action>) {
		// The driver's deadline fired and cleared itself: re-arm whatever is still
		// parked, even if nothing was due yet, or it would never be released.
		self.armed = None;
		for (name, track) in &mut self.tracks {
			if let TrackState::Parked { since } = track.state
				&& since + self.linger <= now
			{
				track.state = TrackState::Idle;
				actions.push(Action::Release { track: name.clone() });
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

	fn end(&mut self, err: Error, actions: &mut Vec<Action>) {
		if self.ended {
			return;
		}
		self.ended = true;
		if let Some((source, _)) = self.serving.take() {
			actions.push(Action::Detach { source });
		}
		actions.push(Action::End { err });
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	const LINGER: Duration = Duration::from_secs(30);

	fn hop(id: u64) -> Hop {
		Hop::new(id).unwrap()
	}

	fn remote(route: u64, first: u64) -> Candidate {
		Candidate {
			route,
			first: Some(hop(first)),
			local: false,
		}
	}

	fn local(route: u64) -> Candidate {
		Candidate {
			route,
			first: None,
			local: true,
		}
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
		front.identify(candidate);
		assert_actions(
			front.step(Event::Resolved {
				route: candidate.route,
				result: Ok(source),
			}),
			&[Action::Resolve],
		);
		assert_actions(front.step(Event::TrackAssigned { track: name("video") }), &[]);
		assert_actions(
			front.step(Event::Used { track: name("video") }),
			&[Action::Query {
				track: name("video"),
				source,
			}],
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
		let front = serving(remote(1, 10), 100);
		assert_eq!(front.identity, Identity::Publisher(hop(10)));
		assert_eq!(front.pin(), Pin::Publisher(hop(10)));
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
		let mut front = serving(remote(1, 10), 100);
		assert_actions(
			front.step(Event::Selected {
				best: Some(remote(1, 10)),
				serving_closing: false,
			}),
			&[],
		);
	}

	#[test]
	fn better_route_takes_over_and_resplices() {
		let mut front = serving(remote(1, 10), 100);
		assert_actions(
			front.step(Event::Selected {
				best: Some(remote(2, 10)),
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
		assert_eq!(front.serving, Some((200, 2)));
	}

	#[test]
	fn a_stale_resolution_is_let_go() {
		let mut front = serving(remote(1, 10), 100);
		front.step(Event::Selected {
			best: Some(remote(2, 10)),
			serving_closing: false,
		});
		// The table moved back before the request landed.
		assert_actions(
			front.step(Event::Selected {
				best: Some(remote(1, 10)),
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
	fn dead_source_reselects_through_the_same_publisher() {
		let mut front = serving(remote(1, 10), 100);
		assert_actions(
			front.step(Event::SourceClosed { source: 100 }),
			&[Action::Detach { source: 100 }, Action::Reselect],
		);
		assert_actions(
			front.step(Event::Selected {
				best: Some(remote(3, 10)),
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

	#[test]
	fn dead_source_with_no_replacement_ends() {
		let mut front = serving(remote(1, 10), 100);
		front.step(Event::SourceClosed { source: 100 });
		assert_actions(
			front.step(Event::Selected {
				best: None,
				serving_closing: false,
			}),
			&[Action::End { err: Error::Dropped }],
		);
	}

	#[test]
	fn anonymous_source_never_resumes() {
		let candidate = Candidate {
			route: 1,
			first: Some(Hop::UNKNOWN),
			local: false,
		};
		let mut front = serving(candidate, 100);
		assert_eq!(front.pin(), Pin::None);
		assert_actions(
			front.step(Event::SourceClosed { source: 100 }),
			&[Action::Detach { source: 100 }, Action::End { err: Error::Dropped }],
		);
	}

	#[test]
	fn standing_refusal_ends_an_unresolved_front() {
		let mut front = Front::new(LINGER);
		front.step(Event::Selected {
			best: Some(remote(1, 10)),
			serving_closing: false,
		});
		assert_actions(
			front.step(Event::Resolved {
				route: 1,
				result: Err(Refusal {
					err: Error::NotFound,
					standing: true,
				}),
			}),
			&[Action::End { err: Error::NotFound }],
		);
	}

	#[test]
	fn standing_refusal_while_serving_skips_the_refuser() {
		let mut front = serving(remote(1, 10), 100);
		front.step(Event::Selected {
			best: Some(remote(2, 10)),
			serving_closing: false,
		});
		assert_actions(
			front.step(Event::Resolved {
				route: 2,
				result: Err(Refusal {
					err: Error::NotFound,
					standing: true,
				}),
			}),
			&[Action::Reselect],
		);
		assert!(front.refused_routes().contains(&2));
		assert_eq!(front.serving, Some((100, 1)));
	}

	#[test]
	fn retracted_route_is_not_a_refusal() {
		let mut front = Front::new(LINGER);
		front.step(Event::Selected {
			best: Some(remote(1, 10)),
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
		assert!(front.refused_routes().is_empty());
		assert!(!front.ended());
	}

	#[test]
	fn a_source_refusing_a_track_aborts_it_and_nothing_else() {
		let mut front = serving(remote(1, 10), 100);
		front.step(Event::TrackAssigned { track: name("audio") });
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
		let mut front = serving(remote(1, 10), 100);
		front.step(Event::TrackAssigned { track: name("audio") });
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
			best: Some(remote(2, 10)),
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
		let mut front = serving(remote(1, 10), 100);
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
		let mut front = serving(remote(1, 10), 100);
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
		let mut front = serving(remote(1, 10), 100);
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
		let mut front = serving(remote(1, 10), 100);
		front.step(Event::Selected {
			best: Some(remote(2, 10)),
			serving_closing: false,
		});
		front.step(Event::Resolved {
			route: 2,
			result: Ok(200),
		});
		let other = track::Info {
			max_age: Duration::from_secs(1),
			..track::Info::default()
		};
		assert_actions(
			front.step(Event::TrackInfo {
				track: name("video"),
				source: 200,
				closing: false,
				result: Ok(other),
			}),
			&[Action::Abort {
				track: name("video"),
				err: Error::Unsupported,
			}],
		);
	}

	#[test]
	fn unread_track_parks_then_releases_after_the_linger() {
		let mut front = serving(remote(1, 10), 100);
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
			&[Action::Release { track: name("video") }],
		);
		// A returning reader re-splices from the serving source.
		assert_actions(
			front.step(Event::Used { track: name("video") }),
			&[Action::Query {
				track: name("video"),
				source: 100,
			}],
		);
	}

	#[test]
	fn a_returning_reader_cancels_the_linger() {
		let mut front = serving(remote(1, 10), 100);
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

	#[test]
	fn unread_track_is_never_spliced() {
		let mut front = serving(remote(1, 10), 100);
		assert_actions(front.step(Event::TrackAssigned { track: name("audio") }), &[]);
		assert_eq!(front.tracks[&name("audio")].state, TrackState::Idle);
	}

	#[test]
	fn local_newcomer_takes_over_a_live_incumbent() {
		let mut front = serving(local(1), 100);
		assert_eq!(front.identity, Identity::Local);
		assert_eq!(front.pin(), Pin::Local);
		assert_actions(
			front.step(Event::Selected {
				best: Some(local(2)),
				serving_closing: false,
			}),
			&[Action::Request { route: 2 }],
		);
	}

	#[test]
	fn local_newcomer_never_splices_into_a_closing_incumbent() {
		let mut front = serving(local(1), 100);
		assert_actions(
			front.step(Event::Selected {
				best: Some(local(2)),
				serving_closing: true,
			}),
			&[Action::Detach { source: 100 }, Action::End { err: Error::Dropped }],
		);
	}

	#[test]
	fn local_incumbent_ending_ends_the_front() {
		let mut front = serving(local(1), 100);
		assert_actions(
			front.step(Event::SourceClosed { source: 100 }),
			&[Action::Detach { source: 100 }, Action::End { err: Error::Dropped }],
		);
	}

	#[test]
	fn teardown_ends_everything() {
		let mut front = serving(remote(1, 10), 100);
		assert_actions(
			front.step(Event::Closed),
			&[Action::Detach { source: 100 }, Action::End { err: Error::Dropped }],
		);
	}

	/// Every sequence of events up to a small depth, over a small alphabet,
	/// checked against the invariants the driver relies on.
	#[test]
	fn bounded_sequences_hold_the_invariants() {
		let t0 = Instant::now();
		let alphabet: Vec<Event> = vec![
			Event::Selected {
				best: Some(remote(1, 10)),
				serving_closing: false,
			},
			Event::Selected {
				best: Some(remote(2, 10)),
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
			Event::TrackAssigned { track: name("v") },
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
}
