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
//! The driver only offers a front routes that serve its broadcast: those of the
//! instance it first resolved, at its newest epoch, whose tracks resume where they
//! stopped since the instance promises one continuous key space. An instance a
//! session minted, for a wire that carries none, promises nothing past its route:
//! that route can answer a re-request with another stream, so a track whose copy
//! fails there ends with the error instead of re-splicing. Another
//! instance winning the path leaves the front alone: its readers stay until they
//! leave or its routes go, and new requests get a fresh front for the winner.
//!
//! A front nothing needs retires: once every track is forgotten (after the linger,
//! so a returning reader still finds the cache) and no consumer holds its broadcast,
//! it ends, and the next request mints a fresh one. A front waiting for a route is
//! not idle.
//!
//! Sources and tracks are named by ids and names, never handles, so a
//! transition can be checked in a unit test by comparing the actions it emits.

use std::{
	collections::{BTreeMap, BTreeSet, HashSet},
	sync::Arc,
	time::Duration,
};

use crate::{Error, time::Instant, track};

/// A route the table selected for the front: the entry id, whether it is a
/// broadcast published on this origin, and whether its instance was carried rather
/// than minted by a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Candidate {
	pub route: u64,
	pub local: bool,
	/// Whether a track whose copy failed may be asked for again through the route:
	/// a carried instance says the answer is the same stream. A minted one names the
	/// announcement, and the route could answer with whatever stream now wins upstream.
	pub carried: bool,
	/// Whether the route is a lite-07 claim, which the route layer keeps in the table
	/// through its debounce once its session closes; see [`Front::waiting`].
	pub claim: bool,
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
	/// reader until [`Event::Wanted`] says so.
	TrackAssigned { track: Arc<str>, now: Instant },
	/// A source answered a track query: its copy's metadata, or a refusal.
	/// `closing` is whether the source has begun closing, in which case a
	/// refusal is not held against it: it is on its way out.
	TrackInfo {
		track: Arc<str>,
		source: u64,
		closing: bool,
		result: Result<track::Info, Error>,
		/// The Retry Interval an upstream refusal asked for, kept with the refusal.
		retry: Option<Duration>,
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
	Wanted { track: Arc<str> },
	/// The last reader left the track. A finished track is read until nothing holds it,
	/// such as a fetch still reading a group, which keeps moving off a dying route.
	Unwanted { track: Arc<str>, now: Instant },
	/// The armed deadline passed.
	Deadline { now: Instant },
	/// The driver let go of a track the machine asked it to [`Action::Forget`].
	/// Not fed when a reader arrived first: [`Event::Wanted`] follows instead.
	Forgotten { track: Arc<str> },
	/// A consumer took hold of the front's broadcast: a request joined it, or the
	/// driver declined an [`Action::Retire`] because one did.
	Held,
	/// The last consumer let go of the front's broadcast.
	Unheld,
	/// The front's broadcast closed, whether the driver closed it for an
	/// [`Action::Retire`] or something else did: nobody can join it now.
	Retired,
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
	/// Drop the driver's source handle, keeping its pending track queries when `drain` is true.
	Detach { source: u64, drain: bool },
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
	/// Ask `source` for its copy of the finished `track`, for the reads still holding it:
	/// a group read on it continues from that copy if its own route dies. Nothing to do
	/// when nothing holds the track; else feed the answer back as [`Event::TrackInfo`].
	Reroute { track: Arc<str>, source: u64 },
	/// No source will replace the copy of the finished `track` any more: a group read on it
	/// fails once its own route does, rather than wait for one.
	Conclude { track: Arc<str> },
	/// The logical track completed.
	Finish { track: Arc<str> },
	/// The logical track failed for good, with the Retry Interval of the upstream refusal it
	/// carries, if any.
	Abort {
		track: Arc<str>,
		err: Error,
		retry: Option<Duration>,
	},
	/// Arm (or clear) the deadline the front wants to be woken at.
	Arm { at: Option<Instant> },
	/// Nothing needs the front: close its broadcast unless a consumer took hold of it
	/// since, then feed back [`Event::Retired`], or [`Event::Held`] when one did.
	Retire,
	/// The front is over: leave the origin's front table first, so nothing joins it
	/// as it ends, then reject the parked requesters with `err`, leave each read
	/// track to end with the copy it is spliced from or still waiting on, abort the
	/// rest with `err`, and drop every source.
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
	/// The most recent refusal, reported when every source has refused, with the Retry
	/// Interval an upstream refusal asked for.
	refusal: Option<(Error, Option<Duration>)>,
	/// Why the serving copy died mid-serve, or what a closing source answered, while a
	/// re-splice is awaited. A refusal of that re-splice, or the front ending first, reports
	/// this instead: the source closing is what ended the track.
	lost: Option<Error>,
	/// What the copy died of when its source closed under it with nothing serving the
	/// track. A front ending before another source splices it reports this, but unlike
	/// [`Self::lost`] a refusal does not: the next source's answer is its own verdict.
	closed: Option<Error>,
	/// Whether anyone reads the track: a copy is only spliced in for a reader.
	wanted: bool,
	/// A detached source whose copy still feeds the track until the serving source's
	/// replaces it: a route being beaten keeps serving until then, and keeps serving if
	/// the newcomer refuses the track.
	draining: Option<u64>,
	/// The source whose route lost the track, failing or refusing it as unroutable, which
	/// judges the route, not the track. The track waits for another source, or for the
	/// table to move, rather than ask that source again at once or end; a draining copy
	/// keeps feeding it meanwhile.
	stranded: Option<u64>,
	/// How the track ended, once it did. It stays only until it goes unread for the linger.
	ended: Option<Ended>,
}

/// How a logical track ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ended {
	/// Completed: its readers have all it holds, but a group still read on it moves with
	/// the serving source, from a copy only that group's reads use.
	Finished,
	/// Finished, and the serving source refused it since: nothing is spliced again.
	Concluded,
	/// Failed for good: nothing is spliced again.
	Aborted,
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
	/// The excluded routes that are claims, while the table still shows them.
	orphans: HashSet<u64>,
	/// Whether the parked requesters were resolved (the first source attached).
	resolved: bool,
	tracks: BTreeMap<Arc<str>, Track>,
	parked: BTreeSet<(Instant, Arc<str>)>,
	/// Immutable track metadata, fixed by the first copy of each track until it is
	/// forgotten. Every source of the broadcast must serve the same content.
	info: BTreeMap<Arc<str>, track::Info>,
	/// How long an unread track stays before it is forgotten.
	linger: Duration,
	/// The deadline last armed, so a step only re-arms on change.
	armed: Option<Instant>,
	/// Whether a consumer holds the front's broadcast. A front starts held by the
	/// request that minted it.
	held: bool,
	/// An [`Action::Retire`] is out, awaiting its answer.
	retiring: bool,
	ended: bool,
}

impl Front {
	/// A front with no source and no tracks, held by the request that minted it;
	/// `linger` is how long an unread track stays before it is forgotten.
	pub(super) fn new(linger: Duration) -> Self {
		Self {
			serving: None,
			serving_closing: false,
			upstream: None,
			excluded: HashSet::new(),
			orphans: HashSet::new(),
			resolved: false,
			tracks: BTreeMap::new(),
			parked: BTreeSet::new(),
			info: BTreeMap::new(),
			linger,
			armed: None,
			held: true,
			retiring: false,
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
		self.orphans.retain(|route| self.excluded.contains(route));
	}

	/// Whether a selection that finds no route leaves the front waiting rather than ending.
	/// A claim whose source closed with its session stays in the table, unavailable, until
	/// another session serves it, a detour that may be learned only a round trip later and
	/// always arrives as another route, or until its debounce runs out. Its own route is
	/// never asked again: it would re-request the broadcast that just ended.
	pub(super) fn waiting(&self) -> bool {
		!self.orphans.is_empty()
	}

	/// The route the attached source came through, if any.
	pub(super) fn serving_route(&self) -> Option<u64> {
		self.serving.map(|(_, candidate)| candidate.route)
	}

	/// The attached source, if any.
	pub(super) fn serving(&self) -> Option<u64> {
		self.serving.map(|(source, _)| source)
	}

	/// Why `track`'s serving copy died, while its re-splice is still awaited. A track the
	/// front ends meanwhile ends with this rather than the front's end.
	pub(super) fn lost(&self, track: &str) -> Option<Error> {
		let track = self.tracks.get(track)?;
		track.lost.clone().or_else(|| track.closed.clone())
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
		// Only a track event can change that track's linger. Route-wide transitions
		// leave parked tracks alone, so they need no deadline-index scan.
		let touched = match &event {
			Event::TrackAssigned { track, .. }
			| Event::TrackInfo { track, .. }
			| Event::TrackEnded { track, .. }
			| Event::Wanted { track }
			| Event::Unwanted { track, .. }
			| Event::Forgotten { track } => Some(track.clone()),
			_ => None,
		};
		let before = touched.as_ref().and_then(|name| self.parked_at(name));
		match event {
			Event::Selected { best, serving_closing } => self.selected(best, serving_closing, &mut actions),
			Event::Resolved { route, result } => self.resolved(route, result, &mut actions),
			Event::SourceClosed { source } => self.source_closed(source, &mut actions),
			// A fresh logical track, even under a name served before: an earlier
			// verdict and metadata belonged to that request, and a later one asks
			// afresh. Unread until a reader shows up, so a lookup whose reader left
			// early is forgotten too.
			Event::TrackAssigned { track, now } => {
				self.tracks.insert(
					track,
					Track {
						state: TrackState::Parked { since: now },
						refused: HashSet::new(),
						refusal: None,
						lost: None,
						closed: None,
						wanted: false,
						ended: None,
						draining: None,
						stranded: None,
					},
				);
			}
			Event::TrackInfo {
				track,
				source,
				closing,
				result,
				retry,
			} => self.track_info(track, source, closing, result.map_err(|err| (err, retry)), &mut actions),
			Event::TrackEnded {
				track,
				source,
				closing,
				result,
				delivered,
			} => self.track_ended(track, source, closing, result, delivered, &mut actions),
			Event::Wanted { track } => self.wanted(track, &mut actions),
			Event::Unwanted { track, now } => self.unwanted(track, now, &mut actions),
			Event::Deadline { now } => self.deadline(now, &mut actions),
			// Its metadata goes too, so a peer naming endless tracks grows nothing.
			Event::Forgotten { track } => {
				self.tracks.remove(&track);
				self.info.remove(&track);
			}
			Event::Held => {
				self.held = true;
				self.retiring = false;
			}
			Event::Unheld => self.held = false,
			// Ended here, not by a source: nothing failed.
			Event::Retired => self.end(Error::Cancel, &mut actions),
			Event::Closed => self.end(Error::Closed, &mut actions),
		}
		if let Some(name) = touched {
			let after = self.parked_at(&name);
			if before != after {
				if let Some(at) = before {
					self.parked.remove(&(at, name.clone()));
				}
				if let Some(at) = after {
					self.parked.insert((at, name));
				}
			}
		}
		if !self.ended {
			let at = self.next_deadline();
			if at != self.armed {
				self.armed = at;
				actions.push(Action::Arm { at });
			}
			if !self.retiring && self.idle() {
				self.retiring = true;
				actions.push(Action::Retire);
			}
		}
		actions
	}

	/// Whether nothing needs the front: no track is left, even a lingering one, no
	/// consumer holds its broadcast, and it serves from a source with no route pending.
	fn idle(&self) -> bool {
		!self.held && self.tracks.is_empty() && self.serving.is_some() && self.upstream.is_none()
	}

	fn selected(&mut self, best: Option<Candidate>, serving_closing: bool, actions: &mut Vec<Action>) {
		if self.serving.is_some() {
			self.serving_closing = serving_closing;
		}
		let serving_route = self.serving.map(|(_, serving)| serving.route);
		match best {
			// The serving route is still the best one. The table moved, which is the
			// evidence a track it lost waits for: ask it again, as for a track that waited
			// on a switch given up on, however that switch ended (a refused request clears
			// `upstream` before it reselects).
			Some(candidate) if Some(candidate.route) == serving_route => {
				self.upstream = None;
				self.unstrand(actions);
				self.redispatch_idle(actions);
			}
			// Already asking through it.
			Some(candidate) if self.upstream.is_some_and(|upstream| upstream.route == candidate.route) => {}
			// A better (or replacement) route: request through it.
			Some(candidate) => {
				self.upstream = Some(candidate);
				actions.push(Action::Request { route: candidate.route });
			}
			// Nothing serves the path yet, but a claim it lost may still come back on a detour.
			None if self.waiting() => self.upstream = None,
			// Nothing serves the path: the front is over, even with a live source. Its
			// route left the table and nothing replaced it, so it serves nobody new;
			// a later announcement gets a fresh front.
			None => {
				self.upstream = None;
				self.end(Error::Unroutable, actions);
			}
		}
	}

	fn resolved(&mut self, route: u64, result: Result<u64, Refusal>, actions: &mut Vec<Action>) {
		let Some(upstream) = self.upstream.filter(|upstream| upstream.route == route) else {
			// A source for a request we no longer want: let it go.
			if let Ok(source) = result {
				actions.push(Action::Detach { source, drain: false });
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
			actions.push(Action::Detach {
				source: old,
				drain: false,
			});
		}
		// Every copy is from an older source, including one a track still drains from a
		// source the previous one replaced.
		self.drain_copies(false);
		self.serving = Some((source, candidate));
		self.serving_closing = false;
		if !self.resolved {
			self.resolved = true;
			actions.push(Action::Resolve);
		}
		// A read track is never parked, so idle is the only state to re-query. A finished
		// one keeps its state, lingering as before, while a read on it follows the source.
		for (name, track) in &mut self.tracks {
			match track.ended {
				None if track.wanted && track.state == TrackState::Idle => {
					track.state = TrackState::Querying { source };
					actions.push(Action::Query {
						track: name.clone(),
						source,
					});
				}
				Some(Ended::Finished) => actions.push(Action::Reroute {
					track: name.clone(),
					source,
				}),
				_ => {}
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
		if candidate.claim {
			self.orphans.insert(candidate.route);
		}
		self.serving = None;
		self.serving_closing = false;
		actions.push(Action::Detach { source, drain: true });
		self.drain_copies(true);
		actions.push(Action::Reselect);
	}

	/// The serving source is going: a spliced copy keeps feeding its track (readers read it
	/// until a replacement's is spliced, or it runs out), and one still being asked for is
	/// kept while that source can still answer, until a replacement attaches.
	fn drain_copies(&mut self, keep_queries: bool) {
		for track in self.tracks.values_mut() {
			match track.state {
				TrackState::Spliced { source } => {
					track.draining = Some(source);
					track.state = TrackState::Idle;
				}
				TrackState::Querying { .. } if !keep_queries => track.state = TrackState::Idle,
				TrackState::Querying { .. } => {}
				TrackState::Idle | TrackState::Parked { .. } => {}
			}
		}
	}

	fn track_info(
		&mut self,
		name: Arc<str>,
		source: u64,
		closing: bool,
		result: Result<track::Info, (Error, Option<Duration>)>,
		actions: &mut Vec<Action>,
	) {
		let serving = self.serving.is_some_and(|(serving, _)| serving == source);
		let strands = self.strands();
		let Some(track) = self.tracks.get_mut(&name) else {
			return;
		};
		// A rerouted copy of a finished track joins its reads only from the serving source,
		// and only with the same metadata. The serving source refusing it is its verdict, as
		// for an open track, so a read whose route dies fails rather than wait for another.
		// A closing source's answer or a lost route's is none: its replacement is asked.
		if track.ended == Some(Ended::Finished) {
			if !serving {
				return;
			}
			let verdict = match result {
				Ok(info) if self.info.get(&name) == Some(&info) => Ok(()),
				Ok(_) => Err(Error::Unsupported),
				Err((err, _)) => Err(err),
			};
			match verdict {
				Ok(()) => {
					track.stranded = None;
					actions.push(Action::Splice { track: name, source });
				}
				Err(_) if closing => {}
				// The route lost it: ask again once the table moves, as for an open track. A
				// local or minted route's word is the verdict, as it is there.
				Err(err) if err.unroutable() && strands => track.stranded = Some(source),
				Err(_) => {
					track.ended = Some(Ended::Concluded);
					actions.push(Action::Conclude { track: name });
				}
			}
			return;
		}
		if track.state != (TrackState::Querying { source }) {
			return;
		}
		let verdict = match result {
			// A copy of the same instance claims identical content, so any difference
			// in its metadata is refused rather than reinterpreted.
			Ok(info) => match self.info.get(&name) {
				Some(expected) if *expected != info => Err((Error::Unsupported, None)),
				Some(_) => Ok(()),
				None => {
					self.info.insert(name.clone(), info);
					Ok(())
				}
			},
			Err(refusal) => Err(refusal),
		};
		match verdict {
			Ok(()) => {
				track.draining = None;
				track.lost = None;
				track.closed = None;
				track.stranded = None;
				track.state = TrackState::Spliced { source };
				actions.push(Action::Splice {
					track: name.clone(),
					source,
				});
			}
			Err(refusal) => self.refuse(name, source, closing, refusal, actions),
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
		let strands = self.strands();
		let serving = self.serving.map(|(serving, _)| serving);
		let Some(track) = self.tracks.get_mut(&name) else {
			return;
		};
		let spliced = track.state == (TrackState::Spliced { source });
		// A track finishing on a copy other than the serving source's asks that source for
		// one anew, since finishing lets go of a query in flight: the group reads still
		// holding the track continue from it, as after a later source change.
		let reroute = serving.filter(|serving| track.state != (TrackState::Spliced { source: *serving }));
		let finish = |name: Arc<str>, actions: &mut Vec<Action>| {
			actions.push(Action::Finish { track: name.clone() });
			if let Some(source) = reroute {
				actions.push(Action::Reroute { track: name, source });
			}
		};
		if track.draining == Some(source) {
			track.draining = None;
			// Every source of a front serves one broadcast, so a copy that completed ends
			// the track whoever serves it now. One that failed leaves the track to the
			// serving source.
			if result.is_ok() && track.ended.is_none() {
				track.state = TrackState::Idle;
				track.ended = Some(Ended::Finished);
				finish(name, actions);
				return;
			}
			if !spliced {
				// The source closed under the copy and nothing serves yet: what the copy died
				// of is what ends the track, unless a route the front waits on resumes it.
				if let Err(err) = result
					&& serving.is_none()
					&& track.ended.is_none()
				{
					track.closed.get_or_insert(err);
				}
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
				track.ended = Some(Ended::Finished);
				finish(name, actions);
			}
			// Died mid-serve after delivering from the same instance: normal failover,
			// re-splice at the first missing group from the serving source, or from the
			// route a switch already under way moves to, whatever the error, since the
			// route it leaves may be what failed. A copy still read from a source that
			// closed during the switch has no serving source to judge the instance by, so
			// the route the switch asks speaks for it.
			Err(err)
				if delivered
					&& (err.transient() || self.upstream.is_some())
					&& (self.serving.map(|(_, candidate)| candidate))
						.or(self.upstream)
						.is_some_and(|candidate| candidate.carried) =>
			{
				track.state = TrackState::Idle;
				track.lost = Some(err);
				self.fail_over(name, actions);
			}
			// The route lost the content, which another route of the instance continues:
			// wait for one rather than ask this route again, which can only refuse. A copy
			// drained from a replaced source leaves the track to the serving one.
			Err(err) if delivered && err.unroutable() && strands => {
				track.state = TrackState::Idle;
				track.lost = Some(err);
				if self.serving.is_some_and(|(serving, _)| serving == source) {
					track.stranded = Some(source);
				} else {
					self.redispatch(name, actions);
				}
			}
			// A content verdict survives every copy. Under a minted instance, even a route
			// failure cannot be retried against the same content.
			Err(err) if delivered => {
				track.state = TrackState::Idle;
				track.ended = Some(Ended::Aborted);
				actions.push(Action::Abort {
					track: name,
					err,
					retry: None,
				});
			}
			// Died before producing anything: a refusal.
			Err(err) => self.refuse(name, source, closing, (err, None), actions),
		}
	}

	/// Record that `source` will not serve `name`, then either ask another
	/// source or, when the serving source itself refused, abort the track.
	fn refuse(
		&mut self,
		name: Arc<str>,
		source: u64,
		closing: bool,
		refusal: (Error, Option<Duration>),
		actions: &mut Vec<Action>,
	) {
		let strands = self.strands();
		let serving = self.serving.is_some_and(|(serving, _)| serving == source);
		let track = self.tracks.get_mut(&name).expect("refusing a known track");
		track.state = TrackState::Idle;
		// A closing source's answer is not a verdict: it is on its way out and its
		// replacement is asked afresh. It is still what ended the track if nothing replaces
		// the source, unless an earlier loss did.
		if closing {
			track.lost.get_or_insert(refusal.0);
			return;
		}
		// A route that lost the content says nothing of the track: wait for another of its
		// instance, keeping a draining copy meanwhile. A replaced source's loss leaves the
		// track to the serving one, whose own answer stands.
		if refusal.0.unroutable() && strands {
			if let Some(draining) = track.draining {
				track.state = TrackState::Spliced { source: draining };
			} else if !serving {
				self.redispatch(name, actions);
				return;
			}
			if serving {
				track.stranded = Some(source);
			}
			return;
		}
		track.refused.insert(source);
		track.refusal = Some(match track.lost.take() {
			Some(lost) => (lost, None),
			None => refusal,
		});
		// A source being replaced still serves it: keep reading that copy, and the
		// refusal stands once it runs out.
		if let Some(draining) = track.draining {
			track.state = TrackState::Spliced { source: draining };
			return;
		}
		self.fail_over(name, actions);
	}

	/// A copy of `name` failed: while a switch to another route is under way, the
	/// track waits for it, and [`Self::attach`] asks the new source, since the failure
	/// may be the route the switch leaves; otherwise the serving source is asked again.
	fn fail_over(&mut self, name: Arc<str>, actions: &mut Vec<Action>) {
		if self.upstream.is_none() {
			self.redispatch(name, actions);
		}
	}

	/// [`Self::redispatch`] every idle track.
	fn redispatch_idle(&mut self, actions: &mut Vec<Action>) {
		let idle: Vec<Arc<str>> = (self.tracks.iter())
			.filter(|(_, track)| track.state == TrackState::Idle)
			.map(|(name, _)| name.clone())
			.collect();
		for name in idle {
			self.redispatch(name, actions);
		}
	}

	/// Splice `name` from the serving source if it can serve it, or abort the
	/// track once the serving source has refused it: refusals are never
	/// retried, and no other source will be asked while this one serves.
	fn redispatch(&mut self, name: Arc<str>, actions: &mut Vec<Action>) {
		let Some((source, _)) = self.serving else {
			return;
		};
		let track = self.tracks.get_mut(&name).expect("dispatching a known track");
		if !track.wanted || track.ended.is_some() || track.state != TrackState::Idle || track.stranded == Some(source) {
			return;
		}
		if track.refused.contains(&source) {
			let (err, retry) = track.refusal.clone().unwrap_or((Error::NotFound, None));
			track.ended = Some(Ended::Aborted);
			actions.push(Action::Abort {
				track: name,
				err,
				retry,
			});
			return;
		}
		if self.serving_closing {
			return;
		}
		track.state = TrackState::Querying { source };
		actions.push(Action::Query { track: name, source });
	}

	/// Whether an unroutable end from the serving source judges its route rather than the
	/// track, so the track waits for the table to move. Only a received route can be stale:
	/// a local publisher's word is the track's verdict, and no news about it is in flight. A
	/// minted instance has no other route to wait for.
	fn strands(&self) -> bool {
		self.serving
			.is_some_and(|(_, candidate)| candidate.carried && !candidate.local)
	}

	/// Ask the serving source again for every track it lost as unroutable. One still read from
	/// a draining copy keeps reading it until the answer, as during any handover.
	fn unstrand(&mut self, actions: &mut Vec<Action>) {
		let Some((source, _)) = self.serving else {
			return;
		};
		let mut idle = Vec::new();
		for (name, track) in &mut self.tracks {
			if track.stranded != Some(source) {
				continue;
			}
			track.stranded = None;
			match track.state {
				TrackState::Spliced { source: draining }
					if track.draining == Some(draining)
						&& track.wanted && track.ended.is_none()
						&& !self.serving_closing =>
				{
					track.state = TrackState::Querying { source };
					actions.push(Action::Query {
						track: name.clone(),
						source,
					});
				}
				// A finished track asks for a rerouted copy anew; its state stays as it lingers.
				_ if track.ended == Some(Ended::Finished) => {
					if !self.serving_closing {
						actions.push(Action::Reroute {
							track: name.clone(),
							source,
						});
					}
				}
				_ => idle.push(name.clone()),
			}
		}
		for name in idle {
			self.redispatch(name, actions);
		}
	}

	fn wanted(&mut self, name: Arc<str>, actions: &mut Vec<Action>) {
		let Some(track) = self.tracks.get_mut(&name) else {
			return;
		};
		track.wanted = true;
		if let TrackState::Parked { .. } = track.state {
			track.state = TrackState::Idle;
		}
		self.redispatch(name, actions);
	}

	fn unwanted(&mut self, name: Arc<str>, now: Instant, actions: &mut Vec<Action>) {
		let Some(track) = self.tracks.get_mut(&name) else {
			return;
		};
		track.wanted = false;
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
			// A copy still being asked for is dropped with the park, and with it the loss
			// it would have recovered: a returning reader's query is a fresh request.
			TrackState::Idle | TrackState::Querying { .. } => {
				track.state = TrackState::Parked { since: now };
				track.lost = None;
				track.closed = None;
				// An ended track has nothing to drop.
				if track.ended.is_none() {
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
		while self.parked.first().is_some_and(|(at, _)| *at <= now) {
			let (_, name) = self.parked.pop_first().unwrap();
			let track = self.tracks.get_mut(&name).expect("a parked track is registered");
			track.state = TrackState::Idle;
			actions.push(Action::Forget { track: name });
		}
	}

	fn parked_at(&self, name: &str) -> Option<Instant> {
		match self.tracks.get(name)?.state {
			TrackState::Parked { since } => Some(since.saturating_add(self.linger)),
			_ => None,
		}
	}

	fn next_deadline(&self) -> Option<Instant> {
		self.parked.first().map(|(at, _)| *at)
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
		Candidate {
			route,
			local: false,
			carried: true,
			claim: false,
		}
	}

	fn local(route: u64) -> Candidate {
		Candidate {
			route,
			local: true,
			carried: true,
			claim: false,
		}
	}

	fn info() -> track::Info {
		track::Info::timed()
	}

	fn name(s: &str) -> Arc<str> {
		Arc::from(s)
	}

	/// Actions compare by their debug form: `Error` has no equality.
	#[track_caller]
	fn assert_actions(actual: Vec<Action>, expected: &[Action]) {
		assert_eq!(format!("{actual:?}"), format!("{expected:?}"));
	}

	/// The expiry index and next deadline agree with a full scan of the tracks.
	#[track_caller]
	fn assert_index_matches_scan(front: &Front) {
		let scanned: BTreeSet<_> = front
			.tracks
			.iter()
			.filter_map(|(name, track)| match track.state {
				TrackState::Parked { since } => Some((since.saturating_add(front.linger), name.clone())),
				_ => None,
			})
			.collect();
		assert_eq!(front.parked, scanned);
		assert_eq!(front.next_deadline(), scanned.first().map(|(at, _)| *at));
	}

	/// A front querying `source` through `candidate` for the read track `video`.
	fn querying(candidate: Candidate, source: u64) -> Front {
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
			&[Action::Arm {
				at: Some(t0.saturating_add(LINGER)),
			}],
		);
		assert_actions(
			front.step(Event::Wanted { track: name("video") }),
			&[
				Action::Query {
					track: name("video"),
					source,
				},
				Action::Arm { at: None },
			],
		);
		front
	}

	/// A front serving `source` through `candidate`, with `video` spliced and read.
	fn serving(candidate: Candidate, source: u64) -> Front {
		let mut front = querying(candidate, source);
		assert_actions(
			front.step(Event::TrackInfo {
				track: name("video"),
				source,
				closing: false,
				result: Ok(info()),
				retry: None,
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
				Action::Detach {
					source: 100,
					drain: false,
				},
				Action::Query {
					track: name("video"),
					source: 200,
				},
			],
		);
		assert_eq!(front.serving, Some((200, remote(2))));
	}

	#[test]
	fn a_closing_source_answers_while_another_route_is_resolving() {
		let mut front = querying(remote(1), 100);
		front.step(Event::SourceClosed { source: 100 });
		front.step(Event::Selected {
			best: Some(remote(2)),
			serving_closing: false,
		});
		assert_actions(
			front.step(Event::TrackInfo {
				track: name("video"),
				source: 100,
				closing: true,
				result: Ok(info()),
				retry: None,
			}),
			&[Action::Splice {
				track: name("video"),
				source: 100,
			}],
		);
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
			&[Action::Detach {
				source: 200,
				drain: false,
			}],
		);
	}

	#[test]
	fn dead_source_reselects_another_route() {
		let mut front = serving(remote(1), 100);
		assert_actions(
			front.step(Event::SourceClosed { source: 100 }),
			&[
				Action::Detach {
					source: 100,
					drain: true,
				},
				Action::Reselect,
			],
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
			&[Action::End { err: Error::Unroutable }],
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
			&[Action::End { err: Error::Unroutable }],
		);
	}

	fn claim(route: u64) -> Candidate {
		Candidate {
			claim: true,
			..remote(route)
		}
	}

	/// A front whose claim's session closed under `video` before its copy failed, with no
	/// other route yet: it waits on the claim the table still shows, never asking it again.
	fn orphaned() -> Front {
		let mut front = serving(claim(1), 100);
		assert_actions(
			front.step(Event::SourceClosed { source: 100 }),
			&[
				Action::Detach {
					source: 100,
					drain: true,
				},
				Action::Reselect,
			],
		);
		assert_eq!(front.excluded_routes(), &HashSet::from([1]));
		assert!(front.waiting());
		assert_actions(
			front.step(Event::Selected {
				best: None,
				serving_closing: false,
			}),
			&[],
		);
		assert_actions(
			front.step(Event::TrackEnded {
				track: name("video"),
				source: 100,
				closing: false,
				result: Err(Error::Session(crate::SessionError::Cancel)),
				delivered: true,
			}),
			&[],
		);
		front
	}

	/// The claim comes back on another session, a detour learned after the close: the
	/// front resumes on it rather than end.
	#[test]
	fn a_closed_claim_resumes_on_the_session_that_serves_it_next() {
		let mut front = orphaned();
		assert_actions(
			front.step(Event::Selected {
				best: Some(claim(2)),
				serving_closing: false,
			}),
			&[Action::Request { route: 2 }],
		);
		assert_actions(
			front.step(Event::Resolved {
				route: 2,
				result: Ok(200),
			}),
			&[Action::Query {
				track: name("video"),
				source: 200,
			}],
		);
		assert_actions(
			front.step(Event::TrackInfo {
				track: name("video"),
				source: 200,
				closing: false,
				result: Ok(info()),
				retry: None,
			}),
			&[Action::Splice {
				track: name("video"),
				source: 200,
			}],
		);
		assert!(front.lost("video").is_none());
	}

	/// The session that serves the claim next frames the track differently: its verdict
	/// ends the track, not the closed session's error.
	#[test]
	fn a_closed_claims_next_session_refusing_ends_with_its_verdict() {
		let mut front = orphaned();
		front.step(Event::Selected {
			best: Some(claim(2)),
			serving_closing: false,
		});
		front.step(Event::Resolved {
			route: 2,
			result: Ok(200),
		});
		assert_actions(
			front.step(Event::TrackInfo {
				track: name("video"),
				source: 200,
				closing: false,
				result: Ok(info().with_compression(track::Compression::Deflate)),
				retry: None,
			}),
			&[Action::Abort {
				track: name("video"),
				err: Error::Unsupported,
				retry: None,
			}],
		);
	}

	/// Nothing serves the claim again: the front ends, and its track with what ended the
	/// copy, the session's error, not the later `Unroutable`.
	#[test]
	fn a_closed_claim_nothing_resumes_ends_with_the_sessions_error() {
		let mut front = orphaned();
		// The claim's debounce ran out: it left the table.
		front.retain_routes(|_| false);
		assert!(!front.waiting());
		assert_actions(
			front.step(Event::Selected {
				best: None,
				serving_closing: false,
			}),
			&[Action::End { err: Error::Unroutable }],
		);
		assert_eq!(
			front.lost("video").map(|err| err.to_string()),
			Some(Error::Session(crate::SessionError::Cancel).to_string())
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
		front.step(Event::Wanted { track: name("audio") });
		assert_actions(
			front.step(Event::TrackInfo {
				track: name("audio"),
				source: 100,
				closing: false,
				result: Err(Error::NotFound),
				retry: None,
			}),
			&[Action::Abort {
				track: name("audio"),
				err: Error::NotFound,
				retry: None,
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
		front.step(Event::Wanted { track: name("audio") });
		assert_actions(
			front.step(Event::TrackInfo {
				track: name("audio"),
				source: 100,
				closing: true,
				result: Err(Error::NotFound),
				retry: None,
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

	/// A copy reset before delivering, seen only once its source began closing, is no
	/// refusal, yet ends the track with the reset rather than `Unroutable` when nothing
	/// replaces the source, or when its replacement refuses. The driver reports it as the copy's end once spliced, or as the answer to
	/// its query when the reset came before the metadata was read.
	#[test]
	fn a_closing_source_reset_reports_the_loss() {
		let reset = || Error::from(crate::StreamError::Session(crate::SessionError::Cancel));
		let ended = || {
			let mut front = serving(local(1), 100);
			let actions = front.step(Event::TrackEnded {
				track: name("video"),
				source: 100,
				closing: true,
				result: Err(reset()),
				delivered: false,
			});
			(front, actions)
		};
		let answered = || {
			let mut front = querying(local(1), 100);
			let actions = front.step(Event::TrackInfo {
				track: name("video"),
				source: 100,
				closing: true,
				result: Err(reset()),
				retry: None,
			});
			(front, actions)
		};
		for (mut front, actions) in [ended(), answered()] {
			assert_actions(actions, &[]);
			assert!(front.tracks[&name("video")].refused.is_empty());
			front.step(Event::SourceClosed { source: 100 });

			let mut replaced = front.clone();
			replaced.step(Event::Selected {
				best: Some(local(2)),
				serving_closing: false,
			});
			replaced.step(Event::Resolved {
				route: 2,
				result: Ok(200),
			});
			assert_actions(
				replaced.step(Event::TrackInfo {
					track: name("video"),
					source: 200,
					closing: false,
					result: Err(Error::NotFound),
					retry: None,
				}),
				&[Action::Abort {
					track: name("video"),
					err: reset(),
					retry: None,
				}],
			);

			assert_actions(
				front.step(Event::Selected {
					best: None,
					serving_closing: false,
				}),
				&[Action::End { err: Error::Unroutable }],
			);
			assert_eq!(
				front.lost("video").map(|err| err.to_string()),
				Some(reset().to_string())
			);
		}
	}

	/// A source that loses a copy mid-serve and then refuses its re-splice, such as a
	/// closing publisher, ends the track with the loss, not the refusal.
	#[test]
	fn a_refused_resplice_reports_the_loss() {
		let mut front = serving(remote(1), 100);
		front.step(Event::TrackEnded {
			track: name("video"),
			source: 100,
			closing: false,
			result: Err(Error::Dropped),
			delivered: true,
		});
		assert!(matches!(front.lost("video"), Some(Error::Dropped)));
		// A reader leaving abandons the recovery, so its loss is not reported later.
		let mut parked = front.clone();
		parked.step(Event::Unwanted {
			track: name("video"),
			now: Instant::now(),
		});
		assert!(parked.lost("video").is_none());
		assert_actions(
			front.step(Event::TrackInfo {
				track: name("video"),
				source: 100,
				closing: false,
				result: Err(Error::NotFound),
				retry: None,
			}),
			&[Action::Abort {
				track: name("video"),
				err: Error::Dropped,
				retry: None,
			}],
		);
	}

	#[test]
	fn a_copy_verdict_aborts_without_querying_again() {
		for candidate in [remote(1), local(1)] {
			for err in [
				Error::from(crate::StreamError::FrameTooLarge),
				Error::from(crate::StreamError::MalformedTrack),
				Error::App(7),
				Error::NotFound,
				Error::Unsupported,
			] {
				let mut front = serving(candidate, 100);
				assert_actions(
					front.step(Event::TrackEnded {
						track: name("video"),
						source: 100,
						closing: false,
						result: Err(err.clone()),
						delivered: true,
					}),
					&[Action::Abort {
						track: name("video"),
						err,
						retry: None,
					}],
				);
				assert_actions(front.step(Event::Wanted { track: name("video") }), &[]);
			}
		}
	}

	/// A route losing the content judges the route, not the track: whether its copy fails
	/// unroutable after delivering or it refuses the track so, the track waits for another
	/// source rather than end, and asks the same one again only once the table moves.
	#[test]
	fn an_unroutable_route_strands_the_track_until_the_table_moves() {
		for err in [Error::Unroutable, Error::from(crate::StreamError::Unroutable)] {
			let mut front = serving(remote(1), 100);
			let lost = Event::TrackEnded {
				track: name("video"),
				source: 100,
				closing: false,
				result: Err(err.clone()),
				delivered: true,
			};
			assert_actions(front.step(lost), &[]);
			// The table moved while the same route still wins: ask it again.
			let query = [Action::Query {
				track: name("video"),
				source: 100,
			}];
			let same = Event::Selected {
				best: Some(remote(1)),
				serving_closing: false,
			};
			assert_actions(front.step(same.clone()), &query);
			// Refused the same way: still waiting, and a reader returning asks nothing.
			let refused = Event::TrackInfo {
				track: name("video"),
				source: 100,
				closing: false,
				result: Err(err.clone()),
				retry: None,
			};
			assert_actions(front.step(refused), &[]);
			assert_actions(front.step(Event::Wanted { track: name("video") }), &[]);
			// Another route serves it.
			front.step(Event::Selected {
				best: Some(remote(2)),
				serving_closing: false,
			});
			assert_actions(
				front.step(Event::Resolved {
					route: 2,
					result: Ok(200),
				}),
				&[
					Action::Detach {
						source: 100,
						drain: false,
					},
					Action::Query {
						track: name("video"),
						source: 200,
					},
				],
			);
			assert!(!front.ended());
			// With no route left the front ends, and the track with the route's loss.
			front.step(Event::TrackInfo {
				track: name("video"),
				source: 200,
				closing: false,
				result: Err(err.clone()),
				retry: None,
			});
			assert_actions(
				front.step(Event::Selected {
					best: None,
					serving_closing: false,
				}),
				&[Action::End { err: Error::Unroutable }],
			);
			assert_eq!(front.lost("video").map(|err| err.to_string()), Some(err.to_string()));

			// A minted instance has no other route, and a local publisher's word is the track's
			// verdict with no news in flight to overtake it: either refusal ends the track, as
			// does a local copy failing so.
			for (local, carried) in [(false, false), (true, true)] {
				let candidate = Candidate {
					route: 1,
					local,
					carried,
					claim: false,
				};
				let abort = [Action::Abort {
					track: name("video"),
					err: err.clone(),
					retry: None,
				}];
				let mut front = querying(candidate, 100);
				let refused = Event::TrackInfo {
					track: name("video"),
					source: 100,
					closing: false,
					result: Err(err.clone()),
					retry: None,
				};
				assert_actions(front.step(refused), &abort);
				if local {
					let mut front = serving(candidate, 100);
					let lost = Event::TrackEnded {
						track: name("video"),
						source: 100,
						closing: false,
						result: Err(err.clone()),
						delivered: true,
					};
					assert_actions(front.step(lost), &abort);
				}
			}
		}
	}

	/// The table moving toward a route that has yet to answer is no news for a stranded track:
	/// it waits for that answer, or asks its own route again once that route wins back.
	#[test]
	fn a_pending_takeover_leaves_a_stranded_track_waiting() {
		let stranded = || {
			let mut front = serving(remote(1), 100);
			front.step(Event::TrackEnded {
				track: name("video"),
				source: 100,
				closing: false,
				result: Err(Error::Unroutable),
				delivered: true,
			});
			// A cheaper route wins: ask it, and nothing else.
			let cheaper = Event::Selected {
				best: Some(remote(2)),
				serving_closing: false,
			};
			assert_actions(front.step(cheaper.clone()), &[Action::Request { route: 2 }]);
			assert_actions(front.step(cheaper), &[]);
			front
		};

		// It answers: the track asks it.
		let mut front = stranded();
		assert_actions(
			front.step(Event::Resolved {
				route: 2,
				result: Ok(200),
			}),
			&[
				Action::Detach {
					source: 100,
					drain: false,
				},
				Action::Query {
					track: name("video"),
					source: 200,
				},
			],
		);

		// It leaves first: the serving route wins again, which is the news the track waited for.
		let mut front = stranded();
		assert_actions(
			front.step(Event::Selected {
				best: Some(remote(1)),
				serving_closing: false,
			}),
			&[Action::Query {
				track: name("video"),
				source: 100,
			}],
		);
	}

	/// The serving source refuses a track unroutable while the copy it replaces still drains.
	/// Once the table moves, the source is asked again with the draining copy still read, and
	/// its answer replaces it. A request for the track after it aborted starts unstranded.
	#[test]
	fn a_stranded_track_still_draining_asks_again_when_the_table_moves() {
		let replaced = || {
			let mut front = serving(remote(1), 100);
			front.step(Event::Selected {
				best: Some(remote(2)),
				serving_closing: false,
			});
			front.step(Event::Resolved {
				route: 2,
				result: Ok(200),
			});
			let refused = Event::TrackInfo {
				track: name("video"),
				source: 200,
				closing: false,
				result: Err(Error::from(crate::StreamError::Unroutable)),
				retry: None,
			};
			assert_actions(front.step(refused), &[]);
			front
		};
		let query = [Action::Query {
			track: name("video"),
			source: 200,
		}];

		let mut front = replaced();
		let moved = Event::Selected {
			best: Some(remote(2)),
			serving_closing: false,
		};
		assert_actions(front.step(moved), &query);
		assert_actions(
			front.step(Event::TrackInfo {
				track: name("video"),
				source: 200,
				closing: false,
				result: Ok(info()),
				retry: None,
			}),
			&[Action::Splice {
				track: name("video"),
				source: 200,
			}],
		);

		// The draining copy fails for good; a fresh request asks the serving source.
		let mut front = replaced();
		assert_actions(
			front.step(Event::TrackEnded {
				track: name("video"),
				source: 100,
				closing: false,
				result: Err(Error::App(7)),
				delivered: true,
			}),
			&[Action::Abort {
				track: name("video"),
				err: Error::App(7),
				retry: None,
			}],
		);
		front.step(Event::TrackAssigned {
			track: name("video"),
			now: Instant::now(),
		});
		assert_actions(
			front.step(Event::Wanted { track: name("video") }),
			&[query[0].clone(), Action::Arm { at: None }],
		);
	}

	/// A replaced copy that delivered nothing loses its route after its replacement refused
	/// the track: the replacement's answer stands, so the track ends with it rather than wait
	/// on a route nothing serves from.
	#[test]
	fn a_drained_copy_lost_as_unroutable_leaves_the_serving_refusal_standing() {
		let mut front = serving(remote(1), 100);
		front.step(Event::Selected {
			best: Some(remote(2)),
			serving_closing: false,
		});
		front.step(Event::Resolved {
			route: 2,
			result: Ok(200),
		});
		let refused = Event::TrackInfo {
			track: name("video"),
			source: 200,
			closing: false,
			result: Err(Error::NotFound),
			retry: None,
		};
		assert_actions(front.step(refused), &[]);
		assert_actions(
			front.step(Event::TrackEnded {
				track: name("video"),
				source: 100,
				closing: false,
				result: Err(Error::from(crate::StreamError::Unroutable)),
				delivered: false,
			}),
			&[Action::Abort {
				track: name("video"),
				err: Error::NotFound,
				retry: None,
			}],
		);
	}

	/// A replaced copy still draining fails unroutable after its replacement refused the
	/// track so: the track waits on the serving source, the one it asks once the table moves.
	#[test]
	fn a_drained_copy_lost_as_unroutable_waits_on_the_serving_source() {
		let mut front = serving(remote(1), 100);
		front.step(Event::Selected {
			best: Some(remote(2)),
			serving_closing: false,
		});
		front.step(Event::Resolved {
			route: 2,
			result: Ok(200),
		});
		let unroutable = || Error::from(crate::StreamError::Unroutable);
		let refused = Event::TrackInfo {
			track: name("video"),
			source: 200,
			closing: false,
			result: Err(unroutable()),
			retry: None,
		};
		assert_actions(front.step(refused), &[]);
		let drained = Event::TrackEnded {
			track: name("video"),
			source: 100,
			closing: false,
			result: Err(unroutable()),
			delivered: true,
		};
		assert_actions(front.step(drained), &[]);
		assert_actions(
			front.step(Event::Selected {
				best: Some(remote(2)),
				serving_closing: false,
			}),
			&[Action::Query {
				track: name("video"),
				source: 200,
			}],
		);
	}

	/// Under a minted instance a re-request could reach another stream, so the track ends
	/// with the error instead, whether the route is a peer's or a local broadcast.
	#[test]
	fn a_copy_dying_under_a_minted_instance_ends_the_track() {
		for (local, closing) in [(false, false), (false, true), (true, false)] {
			let mut front = serving(
				Candidate {
					route: 1,
					local,
					carried: false,
					claim: false,
				},
				100,
			);
			assert_actions(
				front.step(Event::TrackEnded {
					track: name("video"),
					source: 100,
					closing,
					result: Err(Error::App(7)),
					delivered: true,
				}),
				&[Action::Abort {
					track: name("video"),
					err: Error::App(7),
					retry: None,
				}],
			);
			// A returning reader is not re-spliced either.
			assert_actions(front.step(Event::Wanted { track: name("video") }), &[]);
		}
	}

	/// A copy that fails while a switch to another route is under way waits for it,
	/// whatever its error: the route the switch leaves may be what failed, so the new
	/// source continues the track rather than the old one refusing it.
	#[test]
	fn a_copy_failing_during_a_switch_waits_for_it() {
		for err in [
			Error::Unroutable,
			Error::from(crate::StreamError::Unroutable),
			Error::Dropped,
		] {
			let mut front = serving(remote(1), 100);
			assert_actions(
				front.step(Event::Selected {
					best: Some(remote(2)),
					serving_closing: false,
				}),
				&[Action::Request { route: 2 }],
			);
			assert_actions(
				front.step(Event::TrackEnded {
					track: name("video"),
					source: 100,
					closing: false,
					result: Err(err.clone()),
					delivered: true,
				}),
				&[],
			);
			let actions = front.step(Event::Resolved {
				route: 2,
				result: Ok(200),
			});
			assert!(
				actions
					.iter()
					.any(|action| matches!(action, Action::Query { track, source: 200 } if track.as_ref() == "video")),
				"{err}: {actions:?}"
			);
			assert!(
				!actions.iter().any(|action| matches!(action, Action::Abort { .. })),
				"{err}: {actions:?}"
			);
		}
	}

	/// A track still asking a source when it closes is spliced to that source's copy once
	/// its info arrives, while the switch away is under way. When that copy fails, no
	/// source serves to judge the instance by, so the route the switch asks does: the
	/// track waits for it rather than ending.
	#[test]
	fn a_copy_of_a_closed_source_failing_during_a_switch_waits_for_it() {
		let mut front = querying(remote(1), 100);
		assert_actions(
			front.step(Event::SourceClosed { source: 100 }),
			&[
				Action::Detach {
					source: 100,
					drain: true,
				},
				Action::Reselect,
			],
		);
		assert_actions(
			front.step(Event::Selected {
				best: Some(remote(3)),
				serving_closing: false,
			}),
			&[Action::Request { route: 3 }],
		);
		assert_actions(
			front.step(Event::TrackInfo {
				track: name("video"),
				source: 100,
				closing: true,
				result: Ok(info()),
				retry: None,
			}),
			&[Action::Splice {
				track: name("video"),
				source: 100,
			}],
		);
		assert_actions(
			front.step(Event::TrackEnded {
				track: name("video"),
				source: 100,
				closing: true,
				result: Err(Error::Unroutable),
				delivered: true,
			}),
			&[],
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

	/// A switch given up on, with the serving route the best again, leaves a track that
	/// waited on it to the serving source, which a lost copy re-asks.
	#[test]
	fn a_switch_given_up_on_asks_the_serving_source_again() {
		let mut front = serving(remote(1), 100);
		front.step(Event::Selected {
			best: Some(remote(2)),
			serving_closing: false,
		});
		front.step(Event::TrackEnded {
			track: name("video"),
			source: 100,
			closing: false,
			result: Err(Error::Dropped),
			delivered: true,
		});
		assert_actions(
			front.step(Event::Selected {
				best: Some(remote(1)),
				serving_closing: false,
			}),
			&[Action::Query {
				track: name("video"),
				source: 100,
			}],
		);
	}

	/// A switch the target retracted ends with a refusal that is not standing: the serving
	/// route winning the reselect still asks the track that waited on it.
	#[test]
	fn a_switch_refused_without_standing_asks_the_serving_source_again() {
		let mut front = serving(remote(1), 100);
		front.step(Event::Selected {
			best: Some(remote(2)),
			serving_closing: false,
		});
		assert_actions(
			front.step(Event::TrackEnded {
				track: name("video"),
				source: 100,
				closing: false,
				result: Err(Error::Dropped),
				delivered: true,
			}),
			&[],
		);
		assert_actions(
			front.step(Event::Resolved {
				route: 2,
				result: Err(Refusal {
					err: Error::NotFound,
					standing: false,
				}),
			}),
			&[Action::Reselect],
		);
		assert_actions(
			front.step(Event::Selected {
				best: Some(remote(1)),
				serving_closing: false,
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
				retry: None,
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
		// A copy of the same instance claims identical content, so every field counts, `refresh` too.
		let differing = [
			track::Info {
				max_age: Some(Duration::from_secs(1)),
				..info()
			},
			track::Info {
				compression: crate::track::Compression::Deflate,
				..info()
			},
			track::Info {
				refresh: crate::track::Refresh::default().with_ratio(4),
				..info()
			},
		];
		for other in differing {
			let mut front = serving(remote(1), 100);
			front.step(Event::Selected {
				best: Some(remote(2)),
				serving_closing: false,
			});
			front.step(Event::Resolved {
				route: 2,
				result: Ok(200),
			});
			// The source it replaced keeps feeding the track (the audit's F3)...
			assert_actions(
				front.step(Event::TrackInfo {
					track: name("video"),
					source: 200,
					closing: false,
					result: Ok(other),
					retry: None,
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
					retry: None,
				}],
			);
		}
	}

	#[test]
	fn unread_track_parks_then_is_forgotten_after_the_linger() {
		let mut front = serving(remote(1), 100);
		let t0 = Instant::now();
		assert_actions(
			front.step(Event::Unwanted {
				track: name("video"),
				now: t0,
			}),
			&[
				Action::Park { track: name("video") },
				Action::Arm {
					at: Some(t0.saturating_add(LINGER)),
				},
			],
		);
		// Too early: nothing released, but the deadline is armed again since the
		// driver clears it on firing.
		assert_actions(
			front.step(Event::Deadline {
				now: t0.saturating_add(LINGER / 2),
			}),
			&[Action::Arm {
				at: Some(t0.saturating_add(LINGER)),
			}],
		);
		// The driver cleared the fired deadline itself; nothing is parked, so
		// nothing re-arms.
		assert_actions(
			front.step(Event::Deadline {
				now: t0.saturating_add(LINGER),
			}),
			&[Action::Forget { track: name("video") }],
		);
		assert!(front.info.contains_key(&name("video")));
		assert_actions(front.step(Event::Forgotten { track: name("video") }), &[]);
		assert!(!front.tracks.contains_key(&name("video")));
		// Its metadata goes with it, so endless track names grow nothing.
		assert!(!front.info.contains_key(&name("video")));
	}

	/// A reader that looked the track up before the driver could forget it keeps
	/// it: the driver feeds `Wanted` instead of `Forgotten`, and the track re-splices.
	#[test]
	fn a_reader_racing_the_forget_keeps_the_track() {
		let mut front = serving(remote(1), 100);
		let t0 = Instant::now();
		front.step(Event::Unwanted {
			track: name("video"),
			now: t0,
		});
		assert_actions(
			front.step(Event::Deadline {
				now: t0.saturating_add(LINGER),
			}),
			&[Action::Forget { track: name("video") }],
		);
		assert_actions(
			front.step(Event::Wanted { track: name("video") }),
			&[Action::Query {
				track: name("video"),
				source: 100,
			}],
		);
	}

	/// A front nobody holds stays while its track lingers, so a returning reader still
	/// finds the cache, and retires once the track is forgotten.
	#[test]
	fn an_unread_front_retires_after_the_linger() {
		let mut front = serving(remote(1), 100);
		let t0 = Instant::now();
		front.step(Event::Unwanted {
			track: name("video"),
			now: t0,
		});
		assert_actions(front.step(Event::Unheld), &[]);
		assert_actions(
			front.step(Event::Deadline {
				now: t0.saturating_add(LINGER),
			}),
			&[Action::Forget { track: name("video") }],
		);
		assert_actions(front.step(Event::Forgotten { track: name("video") }), &[Action::Retire]);
		assert_actions(front.step(Event::Retired), &[Action::End { err: Error::Cancel }]);
		assert!(front.ended());
	}

	/// A consumer holding the broadcast keeps a front with no track, since it may still
	/// ask for one. A local source's unread track goes at once, so its front retires as
	/// soon as the holder leaves.
	#[test]
	fn a_held_front_stays_without_tracks() {
		let mut front = serving(local(1), 100);
		front.step(Event::Unwanted {
			track: name("video"),
			now: Instant::now(),
		});
		assert_actions(front.step(Event::Forgotten { track: name("video") }), &[]);
		assert_actions(front.step(Event::Unheld), &[Action::Retire]);
	}

	/// A front waiting for a route is not idle, though nobody holds it: neither one
	/// still resolving nor one whose source died while it asks another route.
	#[test]
	fn a_front_waiting_for_a_route_stays() {
		let mut front = Front::new(LINGER);
		front.step(Event::Selected {
			best: Some(remote(1)),
			serving_closing: false,
		});
		assert_actions(front.step(Event::Unheld), &[]);
		assert_actions(
			front.step(Event::Resolved {
				route: 1,
				result: Ok(100),
			}),
			&[Action::Resolve, Action::Retire],
		);

		let mut front = serving(local(1), 100);
		front.step(Event::Unwanted {
			track: name("video"),
			now: Instant::now(),
		});
		front.step(Event::Forgotten { track: name("video") });
		assert_actions(
			front.step(Event::SourceClosed { source: 100 }),
			&[
				Action::Detach {
					source: 100,
					drain: true,
				},
				Action::Reselect,
			],
		);
		assert_actions(front.step(Event::Unheld), &[]);
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
			&[Action::Retire],
		);
	}

	/// A request that joined before the driver could retire the front keeps it: the
	/// driver feeds `Held` instead of `Retired`, and the front retires only once that
	/// holder leaves too.
	#[test]
	fn a_request_racing_the_retire_keeps_the_front() {
		let mut front = serving(local(1), 100);
		front.step(Event::Unwanted {
			track: name("video"),
			now: Instant::now(),
		});
		front.step(Event::Forgotten { track: name("video") });
		assert_actions(front.step(Event::Unheld), &[Action::Retire]);
		assert_actions(front.step(Event::Held), &[]);
		assert!(!front.ended());
		assert_actions(front.step(Event::Unheld), &[Action::Retire]);
		assert_actions(front.step(Event::Retired), &[Action::End { err: Error::Cancel }]);
	}

	/// A finished track stays for readers still draining it and for the linger
	/// after the last one, and is then forgotten. A replacement source reroutes it for the
	/// reads still holding it, without touching the linger.
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
			front.step(Event::Unwanted {
				track: name("video"),
				now: t0,
			}),
			&[Action::Arm {
				at: Some(t0.saturating_add(LINGER)),
			}],
		);
		// A returning reader reads what it holds: no query, and the linger waits.
		assert_actions(
			front.step(Event::Wanted { track: name("video") }),
			&[Action::Arm { at: None }],
		);
		// A replacement source asks for its copy, and splices the answer, for a group still
		// read on the track. Nothing ends or lingers again.
		front.step(Event::Selected {
			best: Some(remote(2)),
			serving_closing: false,
		});
		assert_actions(
			front.step(Event::Resolved {
				route: 2,
				result: Ok(200),
			}),
			&[
				Action::Detach {
					source: 100,
					drain: false,
				},
				Action::Reroute {
					track: name("video"),
					source: 200,
				},
			],
		);
		assert_actions(
			front.step(Event::TrackInfo {
				track: name("video"),
				source: 200,
				closing: false,
				result: Ok(info()),
				retry: None,
			}),
			&[Action::Splice {
				track: name("video"),
				source: 200,
			}],
		);
		assert_actions(
			front.step(Event::TrackEnded {
				track: name("video"),
				source: 200,
				closing: false,
				result: Ok(()),
				delivered: false,
			}),
			&[],
		);
		let t1 = t0.saturating_add(LINGER);
		front.step(Event::Unwanted {
			track: name("video"),
			now: t1,
		});
		assert_actions(
			front.step(Event::Deadline {
				now: t1.saturating_add(LINGER),
			}),
			&[Action::Forget { track: name("video") }],
		);
		front.step(Event::Forgotten { track: name("video") });
		assert!(!front.tracks.contains_key(&name("video")));
	}

	/// A rerouted copy of a finished track joins its reads only from the serving source and
	/// with the track's metadata. A lost route is asked again once the table moves. That
	/// source refusing it, other than closing or losing the route, concludes the track:
	/// nothing is rerouted again. A parked one keeps its linger either way.
	#[test]
	fn a_finished_track_splices_only_a_matching_copy_from_the_serving_source() {
		for refusal in [None, Some(Ok(track::Info::default())), Some(Err(Error::NotFound))] {
			let mut front = serving(remote(1), 100);
			front.step(Event::TrackEnded {
				track: name("video"),
				source: 100,
				closing: false,
				result: Ok(()),
				delivered: true,
			});
			let t0 = Instant::now();
			front.step(Event::Unwanted {
				track: name("video"),
				now: t0,
			});
			front.step(Event::Selected {
				best: Some(remote(2)),
				serving_closing: false,
			});
			front.step(Event::Resolved {
				route: 2,
				result: Ok(200),
			});
			let answer = |source, closing, result| Event::TrackInfo {
				track: name("video"),
				source,
				closing,
				result,
				retry: None,
			};

			// From a source the front no longer serves from, a closing one's refusal, or a
			// lost route's.
			assert_actions(front.step(answer(100, false, Ok(info()))), &[]);
			assert_actions(front.step(answer(200, true, Err(Error::NotFound))), &[]);
			assert_actions(front.step(answer(200, false, Err(Error::Unroutable))), &[]);
			// The lost route is asked again once the table moves, and only then.
			let moved = || Event::Selected {
				best: Some(remote(2)),
				serving_closing: false,
			};
			assert_actions(
				front.step(moved()),
				&[Action::Reroute {
					track: name("video"),
					source: 200,
				}],
			);
			assert_actions(front.step(moved()), &[]);
			match refusal {
				// Other metadata, or a refusal.
				Some(result) => {
					assert_actions(
						front.step(answer(200, false, result)),
						&[Action::Conclude { track: name("video") }],
					);
					assert_actions(front.step(answer(200, false, Ok(info()))), &[]);
					front.step(Event::Selected {
						best: Some(remote(1)),
						serving_closing: false,
					});
					assert_actions(
						front.step(Event::Resolved {
							route: 1,
							result: Ok(300),
						}),
						&[Action::Detach {
							source: 200,
							drain: false,
						}],
					);
				}
				None => assert_actions(
					front.step(answer(200, false, Ok(info()))),
					&[Action::Splice {
						track: name("video"),
						source: 200,
					}],
				),
			}
			assert_index_matches_scan(&front);
			assert_eq!(front.next_deadline(), Some(t0.saturating_add(LINGER)));
		}
	}

	/// A local route has no stale view to wait out: its unroutable answer for a finished
	/// track's rerouted copy concludes the track, as any other refusal does.
	#[test]
	fn a_local_routes_unroutable_answer_concludes_a_finished_track() {
		let mut front = serving(remote(1), 100);
		front.step(Event::TrackEnded {
			track: name("video"),
			source: 100,
			closing: false,
			result: Ok(()),
			delivered: true,
		});
		front.step(Event::Selected {
			best: Some(local(2)),
			serving_closing: false,
		});
		front.step(Event::Resolved {
			route: 2,
			result: Ok(200),
		});
		assert_actions(
			front.step(Event::TrackInfo {
				track: name("video"),
				source: 200,
				closing: false,
				result: Err(Error::Unroutable),
				retry: None,
			}),
			&[Action::Conclude { track: name("video") }],
		);
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
			front.step(Event::Unwanted {
				track: name("video"),
				now: t0,
			}),
			&[Action::Arm {
				at: Some(t0.saturating_add(LINGER)),
			}],
		);
		assert_actions(
			front.step(Event::Deadline {
				now: t0.saturating_add(LINGER),
			}),
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
			front.step(Event::Unwanted {
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
		front.step(Event::Unwanted {
			track: name("video"),
			now: t0,
		});
		assert_actions(
			front.step(Event::Wanted { track: name("video") }),
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
	fn returning_reader_preserves_other_tracks_deadlines() {
		let mut front = serving(remote(1), 100);
		let now = Instant::now();
		front.step(Event::Unwanted {
			track: name("video"),
			now,
		});
		front.step(Event::TrackAssigned {
			track: name("audio"),
			now: now.saturating_add(LINGER / 2),
		});
		assert_actions(
			front.step(Event::Wanted { track: name("video") }),
			&[
				Action::Query {
					track: name("video"),
					source: 100,
				},
				Action::Arm {
					at: Some(now.saturating_add(LINGER).saturating_add(LINGER / 2)),
				},
			],
		);
		assert_actions(
			front.step(Event::Deadline {
				now: now.saturating_add(LINGER).saturating_add(LINGER / 2),
			}),
			&[Action::Forget { track: name("audio") }],
		);
		assert_actions(front.step(Event::Forgotten { track: name("audio") }), &[]);
		assert!(front.tracks.contains_key("video"));
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
			front.step(Event::Deadline {
				now: t0.saturating_add(LINGER),
			}),
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
			&[
				Action::Detach {
					source: 100,
					drain: true,
				},
				Action::Reselect,
			],
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
		assert_actions(front.step(Event::Closed), &[Action::End { err: Error::Closed }]);
	}

	#[test]
	fn deadline_index_matches_scan_across_track_transitions() {
		use rand::{RngExt, SeedableRng};

		let mut rng = rand::rngs::StdRng::seed_from_u64(0x66726f6e74);
		let mut front = serving(remote(1), 100);
		let mut now = Instant::now();
		for _ in 0..4096 {
			let track = name(&format!("track-{}", rng.random_range(0..32)));
			let event = if !front.tracks.contains_key(&track) {
				Event::TrackAssigned { track, now }
			} else {
				match rng.random_range(0..4) {
					0 => Event::Wanted { track },
					1 => Event::Unwanted { track, now },
					2 => {
						now = now.saturating_add(LINGER / 4);
						Event::Deadline { now }
					}
					_ => Event::TrackInfo {
						track,
						source: 100,
						closing: false,
						result: Ok(info()),
						retry: None,
					},
				}
			};
			for action in front.step(event) {
				if let Action::Forget { track } = action {
					front.step(Event::Forgotten { track });
				}
			}
			assert_index_matches_scan(&front);
		}
	}

	/// Every key of events up to a small depth, over a small alphabet,
	/// checked against the invariants the driver relies on.
	#[test]
	fn bounded_keys_hold_the_invariants() {
		let t0 = Instant::now();
		let alphabet: Vec<Event> = vec![
			Event::Selected {
				best: Some(remote(1)),
				serving_closing: false,
			},
			// A claim, which its source closing leaves to be asked again.
			Event::Selected {
				best: Some(claim(2)),
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
			Event::SourceClosed { source: 200 },
			Event::TrackAssigned {
				track: name("v"),
				now: t0,
			},
			Event::Wanted { track: name("v") },
			Event::Unwanted {
				track: name("v"),
				now: t0,
			},
			Event::TrackInfo {
				track: name("v"),
				source: 100,
				closing: false,
				result: Ok(info()),
				retry: None,
			},
			Event::TrackInfo {
				track: name("v"),
				source: 100,
				closing: false,
				result: Err(Error::NotFound),
				retry: None,
			},
			Event::TrackEnded {
				track: name("v"),
				source: 100,
				closing: false,
				result: Err(Error::Dropped),
				delivered: false,
			},
			Event::Deadline {
				now: t0.saturating_add(LINGER),
			},
			Event::Forgotten { track: name("v") },
			Event::Unheld,
			Event::Held,
		];

		fn walk(front: &Front, alphabet: &[Event], depth: usize, keys: &mut usize) {
			// Rendered only when an event needs it: formatting dominates the walk.
			let mut before = None;
			for event in alphabet {
				// Ids are minted once per resolution, so a source can never resolve
				// while it is already serving; the alphabet reuses ids for brevity.
				if let Event::Resolved { result: Ok(source), .. } = event
					&& front.serving.map(|(serving, _)| serving) == Some(*source)
				{
					continue;
				}
				let mut next = front.clone();
				let actions = next.step(event.clone());
				*keys += 1;

				// Route-wide events skip the index update, so they must leave parked tracks alone.
				assert_index_matches_scan(&next);

				// An event that changes nothing produces nothing, except letting
				// go of a source the front never took.
				if !actions.iter().all(|action| matches!(action, Action::Detach { .. })) {
					// The cheap fields settle most events; the rest compare as rendered.
					let flags = |f: &Front| {
						(
							f.serving,
							f.serving_closing,
							f.upstream,
							f.resolved,
							f.armed,
							f.held,
							f.retiring,
							f.ended,
						)
					};
					let moved = flags(&next) != flags(front)
						|| next.parked != front.parked
						|| next.excluded != front.excluded
						|| next.tracks.len() != front.tracks.len();
					if !moved {
						let before = before.get_or_insert_with(|| format!("{front:?}"));
						assert_ne!(
							&format!("{next:?}"),
							before,
							"no-op event {event:?} produced {actions:?}"
						);
					}
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
					if let Action::Detach { source, .. } = action {
						assert_ne!(next.serving.map(|(s, _)| s), Some(*source));
					}
				}
				// A retire is asked only of an idle front, and once until answered.
				if actions.iter().any(|action| matches!(action, Action::Retire)) {
					assert!(next.idle() && !front.retiring, "retired a front in use: {next:?}");
				}
				if depth > 1 {
					walk(&next, alphabet, depth - 1, keys);
				}
			}
		}

		let mut keys = 0;
		walk(&Front::new(LINGER), &alphabet, 5, &mut keys);
		assert!(keys > 100_000);
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
			retry: None,
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
				Action::Detach {
					source: 200,
					drain: false,
				},
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
			retry: None,
		});
		let t0 = Instant::now();
		assert_actions(
			front.step(Event::Unwanted {
				track: name("video"),
				now: t0,
			}),
			&[
				Action::Park { track: name("video") },
				Action::Arm {
					at: Some(t0.saturating_add(LINGER)),
				},
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
		front.step(Event::Wanted { track: name("video") });
		let actions = front.step(Event::TrackInfo {
			track: name("video"),
			source: 300,
			closing: false,
			result: Err(Error::NotFound),
			retry: None,
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
			front.step(Event::Unwanted {
				track: name("video"),
				now: t0,
			}),
			&[
				Action::Park { track: name("video") },
				Action::Arm {
					at: Some(t0.saturating_add(LINGER)),
				},
			],
		);
	}

	/// A front that served ends `Unroutable` when its route leaves, after a challenger
	/// that never took over was refused.
	#[test]
	fn a_front_that_served_ends_unroutable() {
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
			&[Action::End { err: Error::Unroutable }],
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

	/// A draining copy ending cleanly while the serving source's query is in flight finishes
	/// the track, and asks that source again: finishing lets go of the query, and a group
	/// read still holding the track continues from the serving source's copy.
	#[test]
	fn a_drained_clean_end_reroutes_to_the_serving_source() {
		let mut front = serving(remote(1), 100);
		front.step(Event::SourceClosed { source: 100 });
		front.step(Event::Selected {
			best: Some(remote(2)),
			serving_closing: false,
		});
		assert_actions(
			front.step(Event::Resolved {
				route: 2,
				result: Ok(200),
			}),
			&[Action::Query {
				track: name("video"),
				source: 200,
			}],
		);
		assert_actions(
			front.step(Event::TrackEnded {
				track: name("video"),
				source: 100,
				closing: false,
				result: Ok(()),
				delivered: true,
			}),
			&[
				Action::Finish { track: name("video") },
				Action::Reroute {
					track: name("video"),
					source: 200,
				},
			],
		);
	}
}
