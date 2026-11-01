use crate::driver::Transport;
use crate::transport::poll::Session as _;
use futures::FutureExt as _;
use std::{
	collections::{BTreeMap, HashMap, hash_map::Entry},
	sync::atomic::{AtomicU64, Ordering},
	task::{Poll, ready},
	time::Duration,
};

use crate::{
	Error, Path, SessionError, Timescale, broadcast,
	coding::{Decode, DecodeError, Decoder, Reader, Stream, Writer},
	frame, group,
	ietf::{self, Control, FetchType, Filter, GroupOrder, RequestId},
	origin, track,
	util::{BoxFuture, TaskSet, Tasks},
};

use super::{Message, Version, cluster, error::request, group::ObjectExtensionsLength, peer, request_update};
use crate::tail::{Reading, Settle, Tail};

use kio::Lock;

mod namespaces;
mod tracks;

const TRACK_ALIAS_TIMEOUT: Duration = Duration::from_secs(1);

/// How many cancelled aliases to remember. Objects keep arriving for about a round trip
/// after we cancel, so a handful covers the window, while the cap keeps a long session with
/// heavy subscription churn from accumulating tombstones for its whole lifetime.
///
/// The bound is a count rather than a deadline, which is what keeps eviction synchronous
/// with retirement instead of needing a timer to sweep expired entries. The trade is that a
/// session cancelling more than this many distinct aliases inside one round trip evicts a
/// tombstone whose objects are still arriving; those groups fall back to the unknown-alias
/// wait, which is the old behavior rather than a new failure.
const RETIRED_ALIAS_CAPACITY: usize = 64;

/// Distinguish a peer's answer from a local terminal that still owes cancellation.
enum SetupResponse<T> {
	Response(Result<T, Error>),
	Cancelled(Error),
}

/// What a track alias currently refers to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Alias {
	/// An established subscription. Groups carrying this alias belong to it.
	Active(RequestId),

	/// A subscription we cancelled, whose publisher may still be feeding the alias.
	///
	/// The publisher only stops once our cancellation reaches it, so objects keep arriving
	/// for at least a round trip afterwards. Remembering the alias is what lets us discard
	/// them immediately instead of stalling each one on [`TRACK_ALIAS_TIMEOUT`] and calling
	/// it unknown.
	///
	/// It does not make that window safe, only quiet. A publisher that has processed the
	/// cancellation may reassign the alias, and nothing on a group stream distinguishes the
	/// old subscription's objects from the new one's, so a group still in flight when the
	/// new SUBSCRIBE_OK binds the alias is delivered to the new track. The protocol offers
	/// no way to tell them apart: the alias is the only identifier a group carries, and the
	/// draft permits the reuse as long as the two tracks are not live at once. Cancelling
	/// promptly is what bounds the exposure, since it caps the arrival window at a round
	/// trip rather than leaving it open for the life of the session.
	Retired,

	/// A paused subscription a listing holds: the alias stays its own, but a group carrying
	/// it is refused rather than buffered, since nothing reads the track.
	Paused(RequestId),

	/// A paused subscription resuming, whose REQUEST_UPDATE is not answered yet. Its groups
	/// can overtake the answer, so they wait on it as they would on a SUBSCRIBE_OK.
	Resuming(RequestId),
}

impl Alias {
	/// The subscription holding the alias, if any.
	fn holder(self) -> Option<RequestId> {
		match self {
			Self::Active(id) | Self::Paused(id) | Self::Resuming(id) => Some(id),
			Self::Retired => None,
		}
	}
}

/// The aliases a remote publisher has bound on this session, plus the cancelled ones we
/// still remember.
#[derive(Default)]
struct AliasTable {
	/// No control response can bind another alias once receive draining starts.
	ended: bool,
	map: HashMap<u64, Alias>,

	/// Retired aliases in retirement order, so the oldest is forgotten first.
	retired: std::collections::VecDeque<u64>,
}

type TrackAliases = kio::Producer<AliasTable>;

fn insert_track_alias(aliases: &TrackAliases, alias: u64, request_id: RequestId) -> Result<(), Error> {
	let mut aliases = aliases.write().map_err(|_| {
		// The table is never explicitly closed, and this call holds a producer.
		debug_assert!(false, "alias table closed while binding a live subscription");
		Error::Cancel
	})?;
	let table = &mut *aliases;

	match table.map.entry(alias) {
		// Our subscription is gone, so the publisher is free to point the alias somewhere
		// new. Reclaiming it also drops the tombstone early, which reopens the window
		// described on `Alias::Retired`: a group from the old subscription arriving after
		// this lands is indistinguishable from one for the new track.
		Entry::Occupied(mut entry) if *entry.get() == Alias::Retired => {
			entry.insert(Alias::Active(request_id));
			table.retired.retain(|&retired| retired != alias);
			Ok(())
		}
		Entry::Occupied(mut entry) if entry.get().holder() == Some(request_id) => {
			entry.insert(Alias::Active(request_id));
			Ok(())
		}
		Entry::Occupied(_) => Err(Error::Duplicate),
		Entry::Vacant(entry) => {
			entry.insert(Alias::Active(request_id));
			Ok(())
		}
	}
}

/// Whether an error means the peer broke the protocol, as opposed to a stream or
/// transport failing on its own.
///
/// Only the former justifies taking the whole session down. An encode error is ours,
/// not the peer's: we cannot ask it to answer for a message we failed to write.
pub(super) fn is_protocol_violation(err: &Error) -> bool {
	matches!(
		err,
		Error::Decode(_)
			| Error::BoundsExceeded(_)
			| Error::WrongSize
			| Error::ProtocolViolation
			| Error::UnexpectedMessage
			| Error::UnexpectedStream
	)
}

/// Move `alias` to `to` (paused, resuming, or active again), unless another subscription
/// than `request_id` holds it.
fn rebind_track_alias(aliases: &TrackAliases, alias: u64, request_id: RequestId, to: Alias) {
	let Ok(mut aliases) = aliases.write() else {
		return;
	};
	if let Some(held) = aliases.map.get_mut(&alias)
		&& held.holder() == Some(request_id)
	{
		*held = to;
	}
}

/// Retire an alias, so groups still in flight for it are dropped promptly rather than
/// reported as unknown.
///
/// Only retires an alias that still belongs to this request: a later subscription may
/// already have reclaimed it, and that binding outranks a departing owner.
fn retire_track_alias(aliases: &TrackAliases, alias: u64, request_id: RequestId) {
	let Ok(mut aliases) = aliases.write() else {
		return;
	};
	let table = &mut *aliases;

	if table.map.get(&alias).and_then(|held| held.holder()) != Some(request_id) {
		return;
	}

	table.map.insert(alias, Alias::Retired);
	table.retired.push_back(alias);

	while table.retired.len() > RETIRED_ALIAS_CAPACITY {
		let oldest = table.retired.pop_front().expect("non-empty above the capacity");
		// Only forget an entry that is still a tombstone. A reclaimed alias is live again
		// and its own retirement is queued separately.
		if table.map.get(&oldest) == Some(&Alias::Retired) {
			table.map.remove(&oldest);
		}
	}
}

#[derive(Default)]
struct State {
	// Each active subscription
	subscribes: HashMap<RequestId, TrackState>,

	// Joining FETCH request ids, mapped to the SUBSCRIBE they name.
	fetches: HashMap<RequestId, FillRequest>,

	// Group FETCH request ids, filling a cache miss.
	group_fetches: HashMap<RequestId, kio::Producer<GroupFetch>>,

	// Track aliases chosen by the remote publisher.
	aliases: TrackAliases,

	// Each broadcast created by a PUBLISH_NAMESPACE message.
	broadcasts: HashMap<Path, BroadcastState>,

	// Names each advertisement the peer makes, so one that a newer advertisement of the
	// same namespace replaced cannot overwrite it.
	next_advert: u64,

	// Copies nobody subscribes to, kept for the linger with no subscription upstream:
	// a session ending ends them too.
	lingering: HashMap<u64, track::Producer>,
	next_lingering: u64,
}

impl State {
	/// End every active subscription with the error that ended the session.
	///
	/// Active receive tasks abort their own groups. Abort any head waiting for its
	/// tail here, along with the track. Ordinary unsubscribe removes its entry.
	fn abort(&mut self, err: &Error) {
		for (_, mut track) in self.subscribes.drain() {
			if let Some(request) = track.pending.take() {
				request.reject(err.clone());
			}
			if let Some(producer) = track.producer {
				let _ = producer.abort(err.clone());
			}
			if let Ok(mut fill) = track.fill.write() {
				fill.abort(err.clone());
			}
		}
		for (_, producer) in self.lingering.drain() {
			let _ = producer.abort(err.clone());
		}
	}
}

impl Drop for State {
	fn drop(&mut self) {
		// The session dispatcher owns this state and can be dropped at any await. A
		// session that ended with an error already aborted these with it; what
		// remains was cancelled with the dispatcher.
		self.abort(&Error::Cancel);
	}
}

/// The head of a joined group, delivered on the subscription's fill fetch stream.
///
/// Draft-22's current-group join (section 3.5) splits one group across two streams: the
/// fill carries the objects already published when we subscribed, and the subscription
/// carries everything after them. The model has one producer per group, so the fill owns it
/// while it writes the head and hands it over here for the live tail to append to.
///
/// A publisher that promises a fill and never delivers one costs only the fill's own stream
/// and the group it heads: the tail waiting on it ends with the subscription, and a resumed
/// copy goes live once a group past the answer's Largest arrives, head or not.
enum Fill {
	/// Requested, waiting on SUBSCRIBE_OK: it declares the timescale the fill's own object
	/// timestamps are in, and the fetch stream can arrive before it does.
	Requested,

	/// Ready to be served, in these timestamp units. `None` means the track declared none,
	/// so its frames are untimed.
	Serving(Option<Timescale>),

	/// A fetch stream is writing the head. A second one answers no request of ours.
	Active,

	/// The head is written: `sequence` holds objects up to but excluding `next`, and its
	/// producer is waiting for the live tail to claim it.
	///
	/// The tail is what ends the group, and a publisher serving the subscription's range
	/// opens a stream for it even when the group ended at the join point, since that empty
	/// stream is how the group ends. One that opens none instead leaves this head unfinished
	/// until the publisher ends the subscription, which is what publishes it.
	///
	/// Nothing shorter is safe to infer. A later group arriving looks like proof that no
	/// tail is coming, but streams are independent: the tail's own can still be behind it.
	/// Finishing the head on that guess drops the tail when it lands.
	Ready {
		sequence: u64,
		next: u64,
		producer: group::Producer,
	},

	/// No head is coming, and none is owed: none was requested, the tail already claimed
	/// it, or we stopped reading. A subgroup stream starting mid-group then stands alone,
	/// since only a requested head says where its group begins.
	Done,

	/// The head of `sequence` was requested and is never coming: the fill was refused,
	/// failed, or ended without it. That group's mid-group stream has a hole at the front,
	/// so it gets dropped whether it arrives before this settles or after, which degrades
	/// the join to the next group boundary.
	Failed { sequence: u64 },

	/// The publisher ended the subscription cleanly, so no tail is coming. Unlike
	/// [`Fill::Done`], a head still being written is the whole group once it lands: a
	/// draft-18 joining FETCH is not in PUBLISH_DONE's Stream Count, so the settle can
	/// end before it does.
	Ended,
}

impl Fill {
	/// Whether a head might still arrive or is waiting to be claimed, which is what makes a
	/// subgroup stream worth peeking before its group is created.
	fn outstanding(&self) -> bool {
		!matches!(self, Fill::Done | Fill::Failed { .. } | Fill::Ended)
	}

	/// No head is coming for the fill whose Largest Group is `largest`, which is the group
	/// it would have headed. Without one, the fill was owed nothing.
	fn failed(largest: Option<u64>) -> Self {
		match largest {
			Some(sequence) => Fill::Failed { sequence },
			None => Fill::Done,
		}
	}

	/// Whether a stream for `sequence` starting mid-group is missing the head it was owed.
	fn headless(&self, sequence: u64) -> bool {
		matches!(*self, Fill::Failed { sequence: s } if s == sequence)
	}

	/// Take the head for `sequence`, if this is one and it ends where the tail begins.
	///
	/// `start` is the Object ID the tail stream starts at, or `None` for a tail with no
	/// objects of its own, which takes the head whatever it ends at.
	fn claim(&mut self, sequence: u64, start: Option<u64>) -> Result<Option<(group::Producer, u64)>, Error> {
		match *self {
			Fill::Ready { sequence: s, next, .. } if s == sequence => {
				if start.is_some_and(|start| start != next) {
					// A head that stops somewhere other than where the tail starts leaves a
					// hole the model cannot express, so neither half of the group is usable.
					tracing::warn!(sequence, next, start, "the fill does not meet the live tail");
					self.cancel();
					return Err(Error::Unsupported);
				}
			}
			// Nothing of ours: no head at all, or one for another group whose own tail may
			// still claim it.
			_ => return Ok(None),
		}

		match std::mem::replace(self, Fill::Done) {
			Fill::Ready { producer, next, .. } => Ok(Some((producer, next))),
			// Unreachable: the match above proved it is Ready.
			_ => Ok(None),
		}
	}

	/// Install the head a finished fetch stream produced.
	///
	/// [`Fill::Done`] and [`Fill::Ended`] are terminal: the subscription ended while the
	/// head was being written, and its teardown could not reach a producer the fetch stream
	/// still owned. Settle the head as that teardown would have rather than installing it
	/// for a tail that is never coming, or it outlives the subscription unfinished.
	/// [`Fill::Failed`] is too: its request failed while the stream was still writing, and
	/// a tail that arrived meanwhile was already dropped as headless.
	fn install(&mut self, head: Fill) {
		let mut head = head;
		match self {
			Fill::Done | Fill::Failed { .. } => head.cancel(),
			Fill::Ended => head.release(),
			_ => *self = head,
		}
	}

	/// Release a head nothing claimed, publishing the objects it did carry.
	///
	/// The tail is what normally ends the group, so this is the fallback for when none is
	/// coming: the publisher ended the subscription, or the tail that arrived could not be
	/// stitched. Finishing rather than aborting, because the head is a valid prefix of the
	/// group: it starts at the group's first object and has no holes.
	fn release(&mut self) {
		if let Fill::Ready { mut producer, .. } = std::mem::replace(self, Fill::Done) {
			let _ = producer.finish();
		}
	}

	/// The publisher ended the subscription cleanly: release the head, and any still being
	/// written when it lands.
	///
	/// [`Fill::Failed`] stays: a joining FETCH is not in PUBLISH_DONE's Stream Count, so its
	/// head can land after the end, and the tail it owed may already have been dropped.
	/// Releasing that head would publish a cut group as complete.
	fn end(&mut self) {
		if matches!(self, Fill::Failed { .. }) {
			return;
		}
		self.release();
		*self = Fill::Ended;
	}

	/// Abort a head nothing claimed because we stopped reading, not because its group ended.
	///
	/// The group may still be open upstream, so finishing it would cut it short for good: a
	/// rejoin could not replace the cached group, and its readers would see it end early.
	/// A clean end already settled the head, so [`Fill::Ended`] stays. So does
	/// [`Fill::Failed`], so a tail still opening on this fill is dropped as headless.
	fn cancel(&mut self) {
		self.abort(Error::Cancel);
	}

	/// [`Self::cancel`], with `err` as the reason the head was cut short.
	fn abort(&mut self, err: Error) {
		match std::mem::replace(self, Fill::Done) {
			Fill::Ready { producer, .. } => {
				let _ = producer.abort(err);
			}
			Fill::Ended => *self = Fill::Ended,
			Fill::Failed { sequence } => *self = Fill::Failed { sequence },
			_ => {}
		}
	}
}

#[derive(Clone, Copy)]
enum FillRequest {
	Joining(RequestId),
	Update(RequestId),
}

impl FillRequest {
	fn subscription(self) -> RequestId {
		match self {
			Self::Joining(id) | Self::Update(id) => id,
		}
	}
}

/// Cancelling a fill handler releases tails parked on its unfinished head.
struct Filling(kio::Producer<Fill>);

impl Drop for Filling {
	fn drop(&mut self) {
		if let Ok(mut fill) = self.0.write()
			&& matches!(*fill, Fill::Active)
		{
			fill.cancel();
		}
	}
}

/// A standalone FETCH of a run of groups, from FETCH_OK to its fetch stream.
enum GroupFetch {
	/// Waiting on FETCH_OK, which the fetch stream can overtake.
	Pending,
	/// FETCH_OK settled what the answer covers, waiting for the fetch stream to write it.
	Ready(RangeFetch),
	/// A fetch stream is writing the run, and the group it is writing, if any.
	Receiving(Option<group::Handle>),
	/// The request gave up on the run, so its fetch stream stops.
	Cancelled,
	/// The run is written or failed.
	Done,
}

/// What FETCH_OK settled about a range FETCH, for the fetch stream writing its groups.
struct RangeFetch {
	fill: std::sync::Arc<track::Fill>,
	timescale: Option<Timescale>,
	/// The first group requested.
	first: u64,
	/// The last group the answer covers.
	last: u64,
	/// The first Object ID requested in `first`, which its producer starts at.
	start: u64,
	/// The exclusive last Object ID of `last`, when FETCH_OK's End Location falls inside it.
	end: Option<u64>,
}

/// The groups one fetch stream writes into its fill, a group at a time.
struct RangeWriter<'a> {
	fetch: RangeFetch,
	slot: &'a kio::Producer<GroupFetch>,
	/// The group being written, with its producer unless its objects are read past.
	current: Option<(u64, Option<crate::recv::Group>)>,
	/// The first group not opened yet.
	next: u64,
}

impl<'a> RangeWriter<'a> {
	fn new(fetch: RangeFetch, slot: &'a kio::Producer<GroupFetch>) -> Self {
		Self {
			next: fetch.first,
			fetch,
			slot,
			current: None,
		}
	}

	/// The group being written.
	fn sequence(&self) -> Option<u64> {
		self.current.as_ref().map(|(sequence, _)| *sequence)
	}

	/// The current group's producer, `None` while its objects are read past.
	fn producer(&mut self) -> Option<&mut crate::recv::Group> {
		self.current.as_mut()?.1.as_mut()
	}

	/// The exclusive Object ID bounding group `sequence`, if FETCH_OK's End Location falls
	/// inside it.
	fn bound(&self, sequence: u64) -> Option<u64> {
		self.fetch.end.filter(|_| sequence == self.fetch.last)
	}

	/// Finish the current group and open group `sequence`, returning the first Object ID it
	/// starts at. The sequences between them do not exist. A group cached since it was asked
	/// is read past, as is a datagram, which no request ever gets.
	fn open(&mut self, sequence: u64, datagram: bool) -> Result<u64, Error> {
		if sequence < self.next || sequence > self.fetch.last {
			tracing::warn!(
				sequence,
				"a group fetch's group is out of order or past FETCH_OK's End Location"
			);
			return Err(Error::ProtocolViolation);
		}
		self.finish()?;
		let fill = &self.fetch.fill;
		fill.drop_groups(self.next..sequence);
		self.next = sequence + 1;

		let start = match sequence == self.fetch.first {
			true => self.fetch.start,
			false => 0,
		};
		let producer = match datagram {
			true => {
				tracing::debug!(sequence, "a datagram group is not fetchable");
				fill.reject_groups(sequence..sequence + 1, Error::NotFetchable);
				None
			}
			false => match fill.accept(sequence, None) {
				Ok(producer) => Some(crate::recv::Group::new(producer)),
				// Delivered since it was asked: this fetch's copy is a duplicate.
				Err(Error::Duplicate) => None,
				Err(err) => return Err(err),
			},
		};
		if let Some(producer) = &producer {
			let handle = producer.handle().clone();
			if let Ok(mut state) = self.slot.write()
				&& matches!(*state, GroupFetch::Receiving(_))
			{
				*state = GroupFetch::Receiving(Some(handle));
			}
		}
		self.current = Some((sequence, producer));
		if let Some(producer) = self.producer() {
			// The objects keep the IDs they have in the group rather than restarting at 0.
			producer.start_at(start)?;
			producer.set_wire(group::Wire {
				sparse: false,
				first_object: start == 0,
				first_id: start,
				extensions: true,
				has_end: true,
				fetchable: true,
				last: None,
			})?;
		}
		Ok(start)
	}

	/// Finish the current group: its objects end where the next group's begin. The slot
	/// stays locked meanwhile, so an abandoning request never aborts a group as it finishes.
	fn finish(&mut self) -> Result<(), Error> {
		let mut state = self.slot.write().ok();
		if let Some(state) = state.as_mut()
			&& matches!(**state, GroupFetch::Receiving(_))
		{
			**state = GroupFetch::Receiving(None);
		}
		self.close_group()
	}

	/// Finish the current group, with the slot already settled.
	fn close_group(&mut self) -> Result<(), Error> {
		match self.current.take() {
			Some((_, Some(producer))) => producer.finish(),
			_ => Ok(()),
		}
	}

	/// Settle the run once its stream ends: the groups after the last one written do not
	/// exist, except that the last group bounded by an object may still hold later objects.
	/// A failed stream aborts the group it was writing and refuses the rest for now.
	fn settle(mut self, res: Result<(), Error>) -> Result<(), Error> {
		let fill = self.fetch.fill.clone();
		let end = self.fetch.last + 1;
		let res = match res {
			Ok(()) => self.close_group(),
			Err(err) => {
				if let Some((_, Some(producer))) = self.current.take() {
					let _ = producer.abort(err.clone());
				}
				Err(err)
			}
		};
		match res {
			Ok(()) => {
				match self.fetch.end {
					Some(_) if self.next < end => {
						fill.drop_groups(self.next..self.fetch.last);
						fill.reject_groups(self.fetch.last..end, Error::NotFound);
					}
					_ => fill.drop_groups(self.next..end),
				}
				Ok(())
			}
			Err(err) => {
				// As for a subgroup object: the track is malformed, not just this fetch.
				if matches!(err, Error::MalformedTrack) {
					let _ = track::TrackState::abort(&fill.track(), Error::MalformedTrack);
				}
				fill.refuse_groups(self.next..end, err.clone());
				Err(err)
			}
		}
	}
}

/// A group FETCH's entry in [`State::group_fetches`], removed when its request ends.
struct GroupFetchEntry {
	state: Lock<State>,
	fetch_id: RequestId,
	slot: kio::Producer<GroupFetch>,
}

impl GroupFetchEntry {
	fn new(state: &Lock<State>, fetch_id: RequestId, slot: kio::Producer<GroupFetch>) -> Self {
		state.lock().group_fetches.insert(fetch_id, slot.clone());
		Self {
			state: state.clone(),
			fetch_id,
			slot,
		}
	}
}

impl Drop for GroupFetchEntry {
	fn drop(&mut self) {
		self.state.lock().group_fetches.remove(&self.fetch_id);
		// A fetch stream that overtook a refused or unserved FETCH_OK holds its own clone
		// of the slot, so only closing it wakes that stream.
		let _ = self.slot.close();
	}
}

/// What a SUBSCRIBE_OK told us about the track it accepted.
struct Accepted {
	/// The Track Alias the publisher bound to this subscription.
	alias: u64,

	/// The Track Properties it declared.
	properties: ietf::Properties,

	/// The largest Location in the track, absent when it has no content yet. That absence
	/// is what says a fill we asked for is owed nothing.
	largest: Option<ietf::Location>,
}

/// What a subscription is for: the origin's request, until the first SUBSCRIBE_OK
/// accepts it, then the copy it became, subscribed again after lingering.
enum Target {
	Request(track::Request),
	Resume(Idle),
}

impl Target {
	/// The subscribers' aggregate demand.
	fn subscription(&self) -> Option<track::Subscription> {
		match self {
			Self::Request(request) => request.subscription(),
			Self::Resume(idle) => idle.track.subscription(),
		}
	}

	/// The group a join asked for past the newest cached, which a new SUBSCRIBE carries as
	/// NEW_GROUP_REQUEST. A request has nothing cached for a join to be judged against.
	fn new_group(&self) -> Option<u64> {
		let Self::Resume(idle) = self else {
			return None;
		};
		let wanted = idle.demand.wanted()?;
		let latest = idle.track.latest_sequence()?;
		(wanted > latest).then_some(wanted)
	}

	fn producer(&self) -> Option<&track::Producer> {
		match self {
			Self::Request(_) => None,
			Self::Resume(idle) => Some(&idle.track),
		}
	}

	/// The subscription could not be made: the track fails with `err`.
	fn fail(self, err: Error) {
		match self {
			Self::Request(request) => request.reject(err),
			Self::Resume(idle) => {
				let _ = idle.track.abort(err);
			}
		}
	}
}

/// The model's info for a track a `version` publisher described (SUBSCRIBE_OK or
/// TRACK_STATUS_OK).
///
/// The copy keeps the units the publisher declared, so it matches a copy of the same
/// instance that arrived over another route: resuming across routes refuses one whose
/// metadata differs. A sealed TIMESCALE travels downstream unchanged, so it must too.
fn accepted_info(properties: &ietf::Properties, version: Version) -> track::Info {
	let mut info = track::Info::default()
		.with_compress(properties.compress)
		.with_timescale(properties.timescale)
		.with_max_age(properties.max_cache_duration)
		// A track without the property gets the default.
		.with_priority(super::priority::from_wire(
			properties.priority.unwrap_or(super::priority::DEFAULT_WIRE),
		))
		// A publisher answering NEW_GROUP_REQUEST refreshes on any join with a delta to skip;
		// this session carries the request back to it.
		.with_refresh(properties.dynamic_groups.then_some(0));
	info.properties = Some(std::sync::Arc::new(crate::model::object::TrackProperties {
		version,
		values: properties.unknown.clone(),
	}));
	info
}

/// Why a track a `version` publisher answered with `properties` is neither processed nor
/// forwarded, if it is not.
fn refused(properties: &ietf::Properties, version: Version, negotiated: bool) -> Option<Error> {
	if properties.compress && !negotiated {
		return Some(Error::UnsupportedExtension);
	}
	let info = accepted_info(properties, version);
	info.properties.as_deref()?.refused(&info)
}

/// The Largest Group a subscription knows: its SUBSCRIBE_OK's, or a newer one since.
fn largest_group(track: &track::Producer, answer: Option<ietf::Location>) -> Option<u64> {
	answer.map(|largest| largest.group).max(track.latest_sequence())
}

/// A copy with no subscription upstream, kept for fetches and a returning subscriber.
struct Idle {
	track: track::Producer,
	/// Serves its cache misses with a group FETCH, subscribed or not.
	dynamic: track::Requests,
	demand: track::Demand,
	timescale: Option<Timescale>,
	/// The publisher ended the track, so a returning subscriber reads the copy as it is.
	finished: bool,
}

struct TrackState {
	producer: Option<track::Producer>,
	/// The origin request, until SUBSCRIBE_OK accepts it. Abort rejects this: the
	/// producer does not exist yet, and dropping the setup task would be `Dropped`.
	pending: Option<track::Request>,
	name: String,
	alias: Option<u64>,

	// The backfill this subscription asked for, and the rendezvous between its fetch
	// stream and the subgroup stream carrying the rest of the group.
	fill: kio::Producer<Fill>,

	// The broadcast this track was subscribed from. With the track name and the instance
	// it names, it forms the full track name, which is what decides whether a repeated
	// alias is the fatal collision (one alias, two tracks) or the legal sharing of an alias
	// across subscriptions.
	broadcast: Path,
	epoch: Option<crate::Epoch>,

	// Units for this track's object Timestamps, from the TIMESCALE Track Property in
	// SUBSCRIBE_OK. `None` until it arrives, and for a track that declares none: its
	// frames are untimed.
	timescale: Option<Timescale>,
	fill_largest: Option<u64>,
	// The exact sent update's fill floor; a snapshot below it opens no fill stream.
	fill_start: Option<ietf::Location>,

	// The joining FETCH or latest fill update's own request id, when one was sent.
	fetch_id: Option<RequestId>,
	// The last group a replaced fill failed to head. Its mid-group tail can still arrive
	// after a later fill settles, with the same hole at the front. A watermark rather than
	// every failed group, so repeated updates cannot grow it: a mid-group stream at or
	// below it can only be one already in flight before the update, so dropping it costs
	// at most a late group.
	headless_through: Option<u64>,

	// The data streams read so far, which PUBLISH_DONE's Stream Count is checked against.
	tail: kio::Producer<Tail>,
}

impl TrackState {
	fn fill_ok(&mut self, largest: Option<ietf::Location>) {
		if let Ok(mut fill) = self.fill.write()
			&& matches!(*fill, Fill::Requested)
		{
			self.fill_largest = largest.map(|largest| largest.group);
			*fill = if largest.is_some_and(|largest| {
				self.fill_start
					.is_none_or(|start| (start.group, start.object) <= (largest.group, largest.object))
			}) {
				Fill::Serving(self.timescale)
			} else {
				Fill::Done
			};
		}
	}

	#[cfg(test)]
	fn new(producer: track::Producer, broadcast: Path, fill: kio::Producer<Fill>) -> Self {
		let mut state = Self::pending(producer.name().to_owned(), broadcast, fill);
		state.producer = Some(producer);
		state
	}

	fn pending(name: String, broadcast: Path, fill: kio::Producer<Fill>) -> Self {
		Self {
			producer: None,
			pending: None,
			name,
			alias: None,
			broadcast,
			epoch: None,
			timescale: None,
			fill_largest: None,
			fill_start: None,
			fill,
			fetch_id: None,
			headless_through: None,
			tail: Default::default(),
		}
	}
}

struct BroadcastState {
	// The route announced into our origin for this namespace, post-charge: the newest
	// live advertisement's.
	route: crate::origin::Route,

	// Each live advertisement of the namespace with the route it describes, oldest first.
	// A publisher replacing its instance withdraws one advertisement and makes another, on
	// separate streams that can arrive in either order: the newest stands until it goes,
	// and an update to an older one changes nothing.
	adverts: Vec<(u64, crate::origin::Route)>,

	// The served route: dropping it (and the serve task's clone) retracts the
	// route and rejects its queued requests. `None` while the session's limit holds
	// it back, though the peer still advertises it.
	dynamic: Option<crate::origin::Requests>,

	// Bumped each time the route attaches, so a serve task outlived by a limit that
	// took the route away and gave it back ends rather than serve beside the new one.
	generation: u64,

	// How many of them their own request token authorized. While any is live the
	// namespace is exempt from the session's limit.
	tokens: usize,

	// Closes as the namespace is retracted, which closes every source minted under it.
	live: kio::Producer<()>,

	// Counts this namespace against the session's announce cap until it is retracted.
	_slot: crate::auth::Hold,
}

/// A namespace request and the authorization selected at dispatch.
struct IncomingNamespace {
	msg: ietf::PublishNamespace,
	peer: cluster::Peer,
	declared: Option<bool>,
	gate: crate::auth::Gate,
}

/// What authorized one advertisement.
enum Admit {
	/// The session, so the limit holds it back while it does not cover the namespace.
	/// Carries the announce slot its stream already took, when it took one.
	Session(Option<crate::auth::Hold>),
	/// Its own request token, which stands in for the limit. Carries the announce slot
	/// taken before verification, when the namespace was not attached yet.
	Token(Option<crate::auth::Hold>),
}

/// How a live advertisement ends, written once its route is released.
#[derive(Debug)]
enum Withdraw {
	/// Nothing more to write: the peer withdrew it, or the stream ended.
	Done,
	/// Close our side and wait for the peer to read everything.
	Close,
	/// Refuse the request, then close.
	Reject(crate::auth::Refusal),
	/// Refuse an update, then cancel the advertisement.
	Cancel(crate::auth::Refusal),
}

/// What one advertisement said, once its parameters are resolved against the session.
struct Advertised {
	/// The route it describes, with this link's price already charged. The prefix
	/// is stamped where the advertisement attaches (the namespace).
	route: crate::origin::Route,
}

impl Advertised {
	/// The publisher instance the advertisement named (MoQ Epoch), taken as given.
	fn with_epoch(mut self, epoch: Option<crate::Epoch>) -> Self {
		self.route.epoch = epoch;
		self
	}
}

/// One NAMESPACE a SUBSCRIBE_NAMESPACE stream holds, live on the wire until its
/// NAMESPACE_DONE whether or not its route is attached.
struct Advertisement {
	/// Names its attached route to [`Subscriber::stop_announce`] and
	/// [`Subscriber::update_announce`]; `None` while it loops back through us or falls
	/// outside the scope.
	id: Option<u64>,
	/// The publisher instance it names: a repeat may name another, but not gain or lose one.
	epoch: Option<crate::Epoch>,
	/// Counts it against the session's announce cap until its NAMESPACE_DONE, attached or
	/// not; the namespace's route shares it rather than taking a second.
	slot: crate::auth::Hold,
}

/// What a PUBLISH_NAMESPACE stream advertises, kept current across its updates.
struct Held {
	/// The MoQ Cluster parameters, which a REQUEST_UPDATE carries only when they change.
	cluster: Option<cluster::Advert>,
	/// The MoQ Epoch instance, fixed for the advertisement's life: REQUEST_UPDATE cannot
	/// carry one.
	epoch: Option<crate::Epoch>,
}

/// A joining request may be abandoned before it takes an ID. Once committed, the session
/// owns ending it on the wire.
struct JoiningTask {
	work: Option<BoxFuture<'static, Result<(), Error>>>,
	state: kio::Shared<JoiningState>,
	tasks: Tasks,
}

#[derive(Default)]
struct JoiningState {
	committed: bool,
	cancelled: bool,
}

impl JoiningTask {
	/// Ready once the join is done with: `Err` when the head it owes is never coming.
	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		let Some(work) = self.work.as_mut() else {
			return Poll::Ready(Ok(()));
		};
		let res = ready!(waiter.poll_future(work.as_mut()));
		self.work = None;
		Poll::Ready(res)
	}
}

impl Drop for JoiningTask {
	fn drop(&mut self) {
		let committed = {
			let mut state = self.state.lock();
			state.cancelled = true;
			state.committed
		};
		if committed && let Some(work) = self.work.take() {
			// The session owns cancelling the request, including after the subscription
			// ends: closing it once its header is out, or resetting it while the peer's
			// send window still holds the header back.
			self.tasks.push(async move {
				let _ = work.await;
			});
		}
	}
}

#[derive(Clone)]
pub(super) struct Subscriber {
	// Scoped to a minted source on the clone that serves its requests.
	validity: crate::model::broadcast::RequestValidity,
	// The publisher instance the minted source was resolved under (MoQ Epoch), named on
	// every request for it, so the peer refuses rather than answer from another. Scoped
	// like `validity`, and only ever set on a session that negotiated the extension.
	epoch: Option<crate::Epoch>,
	// Arms the track-alias and request-id timeouts.
	runtime: crate::time::Deadlines,
	session: Transport,
	// Traffic stats are attributed through this tagged origin handle.
	origin: origin::Producer,
	control: Control,
	// The origin naming this link for split-horizon (`Route.via`) when the peer
	// declares none of its own (see `session_route`). Base moq-transport carries no
	// hop ids, so a peer only has an identity if it negotiated the MoQ Cluster
	// extension; otherwise this is the one the session assigned it (a fresh id per
	// dialed or accepted session, unless the caller pinned one with `with_peer_hop`),
	// or `Hop::UNKNOWN` (0) when none was assigned.
	//
	// The assigned id stays local: it is never written into a hop chain, so a peer
	// that withheld an identity is not named on the wire.
	session_origin: crate::Hop,
	// Our own Hop ID, which an advertisement must not already contain: one that does
	// looped back through us.
	self_origin: crate::Hop,
	// What the peer declared in its SETUP.
	peer_setup: peer::PeerSetup,
	// Whether our SETUP declared MoQ Solicit, which is what makes an unsolicited
	// PUBLISH_NAMESPACE the peer's fault.
	solicit: bool,
	// Local policy for what pulling from this peer costs, overriding whatever it
	// declared. See `cluster::link_cost`.
	cost: Option<u64>,
	state: Lock<State>,
	tasks: Tasks,
	version: Version,
	// Set once the peer sends a GOAWAY; this session's routes then cost
	// Cost::DRAIN, so a replacement session outranks it. Requests keep opening,
	// deliberately past draft-22 section 9.2's SHOULD NOT: refusing them would
	// fail requests that land before the replacement is up.
	going_away: crate::goaway::GoingAway,
	// Our grant (MoQ Auth): a subscription it stops covering is cancelled.
	auth: crate::auth::Link,
	// Namespaces the peer may have announced at once (`session::Limits::announces`), and
	// tracks it lists to us.
	pub(super) announces: crate::session::Slots,
	// What SUBSCRIBE_TRACKS lists of the peer's tracks.
	listings: Lock<tracks::Listings>,
}

/// One SUBSCRIBE_NAMESPACE on its stream.
struct NamespaceRequest {
	stream: Stream<Transport, Version>,
	messages: super::request_stream::Messages,
	prefix: Path,
	/// Set once the request goes out.
	request_id: Option<RequestId>,
	/// The suffixes this stream advertised, with the advertisement each one holds, so a
	/// repeat is recognized as an update to it rather than a second one (which would
	/// leak the refcount).
	live: HashMap<Path, Advertisement>,
}

/// The initial set of a SUBSCRIBE_NAMESPACE still to read, under MoQ Active Count.
struct Initial {
	/// NAMESPACE messages still owed before the first change.
	initial: u64,
	/// Released once `initial` reaches zero.
	snapshot: Option<crate::model::Snapshot>,
}

/// Resolve the subscription a data stream belongs to.
///
/// SUBSCRIBE_OK can be reordered behind the stream it describes, so an alias we have not
/// seen is worth waiting on briefly (draft-22 section 9.9). Three outcomes:
/// the subscription, [`Error::Cancel`] for an alias we retired, and [`Error::NotFound`]
/// once the wait expires without any binding at all.
async fn resolve_track_alias(
	runtime: &crate::time::Deadlines,
	aliases: kio::Consumer<AliasTable>,
	alias: u64,
) -> Result<RequestId, Error> {
	let mut timeout = crate::time::Deadline::after(runtime, TRACK_ALIAS_TIMEOUT);
	kio::wait(|waiter| {
		let resolved = aliases.poll(waiter, |aliases| match aliases.map.get(&alias) {
			Some(Alias::Active(request_id)) => Poll::Ready(Ok(*request_id)),
			// A subscription we already cancelled, whose publisher has not caught up with
			// our STOP_SENDING. Discard the group now rather than waiting out the timeout
			// for a binding that is never coming.
			Some(Alias::Retired) => Poll::Ready(Err(Error::Cancel)),
			// Nothing reads a paused subscription, so its groups are refused.
			Some(Alias::Paused(_)) => Poll::Ready(Err(Error::Cancel)),
			Some(Alias::Resuming(_)) | None if aliases.ended => Poll::Ready(Err(Error::Cancel)),
			Some(Alias::Resuming(_)) | None => Poll::Pending,
		});
		if let Poll::Ready(result) = resolved {
			return Poll::Ready(result.unwrap_or(Err(Error::Cancel)));
		}
		if timeout.poll(waiter).is_ready() {
			return Poll::Ready(Err(Error::NotFound));
		}
		Poll::Pending
	})
	.await
}

impl Subscriber {
	#[expect(
		clippy::too_many_arguments,
		reason = "private plumbing; a single-use argument struct would only rename the parameters"
	)]
	pub fn new(
		runtime: crate::time::Deadlines,
		session: Transport,
		origin: origin::Producer,
		control: Control,
		peer_hop: Option<crate::Hop>,
		peer_setup: peer::PeerSetup,
		self_origin: crate::Hop,
		cost: Option<u64>,
		version: Version,
		tasks: Tasks,
		going_away: crate::goaway::GoingAway,
	) -> Self {
		Self {
			runtime,
			session,
			// Its own frame budget, so a peer's declared sizes crowd out no other session.
			origin: origin.with_own_frames(),
			control,
			session_origin: peer_hop.unwrap_or(crate::Hop::UNKNOWN),
			self_origin,
			peer_setup,
			solicit: true,
			cost,
			state: Default::default(),
			tasks,
			version,
			going_away,
			auth: crate::auth::Link::new(false),
			announces: Default::default(),
			listings: Default::default(),
			validity: Default::default(),
			epoch: None,
		}
	}

	/// Bound what we subscribe to by the grant this session's tokens earn (MoQ Auth),
	/// and what the peer may publish to us by the session's limit, as either changes.
	pub fn with_auth(mut self, auth: crate::auth::Link) -> Self {
		self.auth = auth;
		let this = self.clone();
		self.tasks.push(async move { this.run_limit().await });
		self
	}

	/// Whether our SETUP declared MoQ Solicit. Without it an unsolicited PUBLISH_NAMESPACE
	/// is what we invited, even from a peer that implements the extension.
	pub fn with_solicit(mut self, solicit: bool) -> Self {
		self.solicit = solicit;
		self
	}

	/// Follow the session's limit, attaching every withheld namespace a new limit
	/// covers. Each route's own serve task holds itself back when a limit no longer
	/// covers it.
	async fn run_limit(&self) {
		let mut epoch = 0;
		loop {
			let permit = kio::wait(|waiter| {
				self.auth
					.poll_permit(crate::auth::Direction::Subscribe, &mut epoch, waiter)
			})
			.await;
			let mut state = self.state.lock();
			let mut attached = Vec::new();
			for (path, entry) in state.broadcasts.iter_mut() {
				if permit.within_limit(path.as_str())
					&& let Ok(Some(generation)) = self.reattach(path, entry)
				{
					attached.push((path.clone(), generation));
				}
			}
			drop(state);
			for (path, generation) in attached {
				self.serve_route(path, generation);
			}
		}
	}

	/// Serve the requests beneath one attached namespace on its own task.
	fn serve_route(&self, path: Path, generation: u64) {
		let this = self.clone();
		self.tasks.push(async move {
			// stop_announce is the authoritative remover: it drops the entry
			// (retracting the route) once the announce refcount hits zero,
			// which is what makes run_route exit, as does a limit holding it back.
			this.run_route(path, generation).await;
		});
	}

	/// End every active subscription with the error that ended the session.
	pub fn abort(&self, err: &Error) {
		self.state.lock().abort(err);
	}

	/// Settle waits for control state that cannot arrive on the closed transport,
	/// retaining known aliases and tracks until their buffered data is consumed.
	pub fn settle_control(&self) {
		let state = self.state.lock();
		if let Ok(mut aliases) = state.aliases.write() {
			aliases.ended = true;
		}
		for track in state.subscribes.values() {
			if let Ok(mut fill) = track.fill.write()
				&& matches!(*fill, Fill::Requested | Fill::Serving(_))
			{
				*fill = Fill::Done;
			}
		}
		for fetch in state.group_fetches.values() {
			if let Ok(mut fetch) = fetch.write()
				&& matches!(*fetch, GroupFetch::Pending)
			{
				*fetch = GroupFetch::Done;
			}
		}
	}

	/// Leave `alias` in the state a cancelled subscription leaves behind: bound to a
	/// subscription, then retired.
	///
	/// The alias table is private to this module and the loop that answers a group for a
	/// retired alias lives in `session.rs`, so this is what lets that loop be driven end to
	/// end from there.
	#[cfg(test)]
	pub(super) fn retire_alias(&self, alias: u64) {
		// Which request owned the alias does not matter, only that retirement follows the
		// same binding it does in production.
		const REQUEST_ID: RequestId = RequestId(0);

		let aliases = self.state.lock().aliases.clone();
		insert_track_alias(&aliases, alias, REQUEST_ID).expect("bind the alias");
		retire_track_alias(&aliases, alias, REQUEST_ID);
	}

	/// What the peer declared in its SETUP. See [`super::Publisher::peer`].
	pub(super) async fn peer(&self) -> cluster::Peer {
		self.peer_setup.get().await.cluster
	}

	/// The announcing session's declared or assigned identity, for split-horizon.
	///
	/// Local selection state: it is stored as `Route.via` and never written into the
	/// hop chain, so an assigned id is not forwarded as a name for a peer that
	/// declined to give one.
	fn via(&self, peer: &cluster::Peer) -> crate::Hop {
		peer.identity().unwrap_or(self.session_origin)
	}

	/// The route for an advertisement that carries no path of its own.
	///
	/// Base moq-transport has no hops on the wire, so the chain is a single 0: the
	/// anonymous mark, forwarded unchanged. The session's assigned identity stays
	/// on `via` for split-horizon; putting it in the chain would publish a name for
	/// a peer that declined to give one.
	///
	/// The link is charged all the same. Such an advertisement carries no ROUTE_COST,
	/// which reads as 0, but the draft charges every advertisement for the direction it
	/// arrived over regardless. Skipping it would forward a paid upstream to
	/// cluster-aware peers as free and pull subscriptions onto the wrong relay.
	///
	/// It is charged only one hop, though the chain it stands for may be arbitrarily
	/// long: a peer that carries no hop ids hides its depth, so this route understates
	/// its true length. An anonymous route already ranks below every identified one,
	/// so that understatement cannot beat a real path. Price such a link with
	/// [`crate::Client::with_cost`] among other anonymous routes.
	fn session_route(&self, peer: &cluster::Peer) -> crate::origin::Route {
		let mut hops = crate::Hops::new();
		hops.push(crate::Hop::UNKNOWN)
			.expect("an empty hop chain has room for one entry");
		crate::origin::Route::default()
			.with_hops(hops)
			.with_via(self.via(peer))
			// A peer without Cluster contributes only the arriving link price.
			.with_cost(crate::origin::Cost::UNKNOWN.charged(cluster::link_cost(self.cost, peer)))
	}

	/// The route an advertisement describes, or `None` when it must be discarded.
	///
	/// A negotiated peer supplies the path and cost, so the route is what the mesh
	/// actually knows: the full chain, and the accumulated cost plus this link's price.
	/// A received 0 stays 0. An advertisement whose path already contains our own Hop
	/// ID looped back, and neither forwarding it nor subscribing through it is safe.
	fn route(&self, advert: Option<&cluster::Advert>, peer: &cluster::Peer) -> Option<Advertised> {
		let Some(advert) = advert else {
			return Some(Advertised {
				route: self.session_route(peer),
			});
		};

		if advert.loops(self.self_origin) || advert.hops.hops().len() > self.origin.max_hops().get() {
			return None;
		}

		Some(Advertised {
			route: advert
				.route(cluster::link_cost(self.cost, peer))
				.with_via(self.via(peer)),
		})
	}

	/// Bind the alias the publisher chose for this subscription.
	///
	/// Two failures, and only one of them is the session's. A publisher may hand the same
	/// alias to several subscriptions of one track, which draft-22 section 3.1.3 allows and
	/// expects the subscriber to demux by re-applying each subscription's filter. Ours are
	/// all LargestObject, so they are indistinguishable and we cannot: that costs the one
	/// subscription ([`Error::Unsupported`]). The same alias naming a *different* track is
	/// the collision section 11.1 makes fatal ([`Error::Duplicate`]).
	fn register_alias(&self, request_id: RequestId, alias: u64) -> Result<(), Error> {
		let mut state = self.state.lock();
		if !state.subscribes.contains_key(&request_id) {
			return Err(Error::NotFound);
		}

		if let Err(err) = insert_track_alias(&state.aliases, alias, request_id) {
			return Err(match self.alias_names_same_track(&state, alias, request_id) {
				true => Error::Unsupported,
				false => err,
			});
		}

		state.subscribes.get_mut(&request_id).unwrap().alias = Some(alias);
		Ok(())
	}

	/// Move the alias the subscription with `request_id` is bound to into `to`.
	fn rebind_alias(&self, request_id: RequestId, to: Alias) {
		let state = self.state.lock();
		if let Some(alias) = state.subscribes.get(&request_id).and_then(|track| track.alias) {
			rebind_track_alias(&state.aliases, alias, request_id, to);
		}
	}

	/// Whether the subscription already holding `alias` is for the same full track name as
	/// `request_id`, making the repeat legal sharing rather than a collision.
	fn alias_names_same_track(&self, state: &State, alias: u64, request_id: RequestId) -> bool {
		let aliases = state.aliases.read();
		let Some(holder) = aliases.map.get(&alias).and_then(|held| held.holder()) else {
			return false;
		};

		let (Some(held), Some(new)) = (state.subscribes.get(&holder), state.subscribes.get(&request_id)) else {
			return false;
		};

		held.broadcast == new.broadcast && held.epoch == new.epoch && held.name == new.name
	}

	/// Take the origin request back out of a subscription that is still setting up.
	fn take_pending(&self, request_id: RequestId) -> Option<track::Request> {
		self.state.lock().subscribes.get_mut(&request_id)?.pending.take()
	}

	fn remove_subscribe(&self, request_id: RequestId) -> Option<TrackState> {
		let mut state = self.state.lock();
		let track = state.subscribes.remove(&request_id)?;
		if let Some(fetch_id) = track.fetch_id {
			state.fetches.remove(&fetch_id);
		}
		if let Some(alias) = track.alias {
			retire_track_alias(&state.aliases, alias, request_id);
		}
		// The subscription is over, so the tail a fill's head was waiting for is never
		// coming. A publisher's clean end already published the head, so what is left was
		// cut short by us or a failure: cancel it rather than drop the producer unfinished.
		if let Ok(mut fill) = track.fill.write() {
			fill.cancel();
		}
		Some(track)
	}

	pub(super) fn stats(&self) -> &crate::stats::Session {
		self.origin.stats()
	}

	/// Send SUBSCRIBE_NAMESPACE for one prefix on a bidi stream, and read what it reports
	/// until the peer ends it or `live` closes.
	/// The caller opens the stream, one per prefix.
	///
	/// A failure here is per-prefix, so the caller decides what it means for the
	/// session: [`is_protocol_violation`] separates the peer's fault (fatal) from a
	/// stream of ours that simply died (survivable).
	///
	/// `snapshot` holds the origin's announce consumers under the prefix short of
	/// synced. It drops once the NAMESPACE messages MoQ Active Count promised are
	/// in, on REQUEST_OK without the extension, or when no request goes out at all.
	///
	/// [`Ended::Local`](namespaces::Ended::Local) means `live` closed, and the request is
	/// off the wire by the time this returns.
	async fn run_subscribe_namespace(
		&mut self,
		stream: Stream<Transport, Version>,
		prefix: Path,
		snapshot: crate::model::Snapshot,
		live: kio::Consumer<()>,
	) -> Result<namespaces::Ended, Error> {
		let mut request = NamespaceRequest {
			stream: stream.with_stats(self.origin.stats()),
			messages: super::request_stream::Messages::new(self.session.clone(), self.control.client()),
			prefix,
			request_id: None,
			live: HashMap::new(),
		};
		let res = self.subscribe_namespace(&mut request, snapshot, &live).await;

		// The stream owns every advertisement it carried, so release them however it
		// ends: a clean close, a decode error, the peer resetting it, or the session
		// ending it. Without this each namespace keeps its refcount and the source never
		// detaches.
		for (path, advert) in std::mem::take(&mut request.live) {
			if let Some(id) = advert.id {
				let _ = self.stop_announce(path, id, false);
			}
		}

		if let Ok(namespaces::Ended::Local) = res {
			self.unsubscribe_namespace(request).await;
		}
		res
	}

	/// Send the request and read its answers until either side ends it.
	async fn subscribe_namespace(
		&mut self,
		request: &mut NamespaceRequest,
		snapshot: crate::model::Snapshot,
		live: &kio::Consumer<()>,
	) -> Result<namespaces::Ended, Error> {
		// Hidden namespaces are requested too, as on moq-lite: the session mirrors the
		// peer into the origin and each local reader opts in on its own. The parameter
		// fails decoding at a peer that doesn't know it, so it waits on the peer's SETUP
		// to say whether it does (MoQ Hidden).
		let declared = self.peer_setup.get().await;
		let hidden = declared.hidden;
		let prefix = request.prefix.clone();

		let request_id = self.control.next_request_id();
		request.request_id = Some(request_id);
		let stream = &mut request.stream;

		let msg = ietf::SubscribeNamespace {
			request_id,
			namespace: prefix.clone(),
			hidden,
		};
		// Flushed while waiting for the answer, so `live` closing still ends a request the
		// peer's send window holds back.
		stream.writer.buffer_message(ietf::SubscribeNamespace::ID, &msg)?;

		tracing::debug!(%prefix, "subscribe_namespace started");

		// Read response
		let Some(framed) = self.next_namespace_message(request, live).await? else {
			return Ok(namespaces::Ended::Local);
		};
		let super::request_stream::Framed(type_id, body) =
			framed.ok_or_else(|| request.messages.failed(Error::ProtocolViolation))?;
		let mut data = body.decoder(self.version);

		let count = match type_id {
			ietf::RequestOk::ID => ietf::RequestOk::decode_msg(&mut data, self.version)?.active,
			ietf::RequestError::ID => {
				let msg = ietf::RequestError::decode_msg(&mut data, self.version)?;
				let err = request::from_code(msg.error_code, request::Kind::SubscribeNamespace, self.version);
				tracing::warn!(%err, reason = %msg.reason_phrase, "subscribe_namespace error");
				return Err(err);
			}
			_ => return Err(Error::UnexpectedMessage),
		};

		tracing::debug!(%prefix, ?count, "subscribe_namespace ok");

		// MoQ Active Count says how many NAMESPACE messages make up the initial set. A
		// count the negotiation did not promise, or a missing one it did, is the peer
		// breaking the extension. Without it, the answer is the best boundary there is.
		if declared.active_count != count.is_some() {
			return Err(Error::ProtocolViolation);
		}
		let initial = count.unwrap_or(0);
		let snapshot = (initial > 0).then_some(snapshot);

		// The extensions change the NAMESPACE encoding, so we can't parse one until
		// the peer's SETUP says whether it negotiated them.
		let peer = self.peer().await;
		let negotiated = ietf::Negotiated {
			cluster: peer.negotiated(),
			epoch: declared.epoch,
		};

		self.run_namespace_entries(request, &peer, negotiated, Initial { initial, snapshot }, live)
			.await
	}

	/// The next message on a SUBSCRIBE_NAMESPACE stream: `None` once `live` closes,
	/// `Some(None)` once the peer finishes the stream.
	async fn next_namespace_message(
		&self,
		request: &mut NamespaceRequest,
		live: &kio::Consumer<()>,
	) -> Result<Option<Option<super::request_stream::Framed>>, Error> {
		let NamespaceRequest { stream, messages, .. } = request;
		kio::wait(|waiter| {
			if live.poll_closed(waiter).is_ready() {
				return Poll::Ready(Ok(None));
			}
			ready!(stream.writer.poll_flush(waiter))?;
			messages
				.poll_next(&mut stream.reader, waiter)
				.map(|next| next.map(Some))
		})
		.await
	}

	/// End a request the session ended, and wait for it to leave the wire, so an
	/// overlapping one can go out: a peer may refuse a prefix that overlaps one it still
	/// holds.
	///
	/// The stream closes (a FIN on draft-18, a reset on draft-22) and is read until the peer
	/// closes its half.
	async fn unsubscribe_namespace(&self, request: NamespaceRequest) {
		let NamespaceRequest {
			stream: Stream { mut writer, mut reader },
			mut messages,
			prefix,
			request_id,
			..
		} = request;
		// Nothing went out, so there is nothing to end.
		if request_id.is_none() {
			return;
		}
		// The request never reached the peer whole, so it holds nothing to wait on.
		if !writer.is_flushed() {
			tracing::debug!(%prefix, "subscribe_namespace abandoned before it was sent");
			reader.abort(&Error::Cancel);
			writer.abort(&Error::Cancel);
			return;
		}
		tracing::debug!(%prefix, "unsubscribe_namespace");

		match super::request_stream::fin_cancels(self.version) {
			true => {
				let _ = writer.finish();
			}
			false => writer.abort(&Error::Cancel),
		}
		let session = self.session.clone();
		// A peer still streaming entries must not hold the session's other tasks.
		let mut budget = kio::coop::Budget::new(32);
		kio::wait(|waiter| {
			if session.poll_closed(waiter).is_ready() {
				return Poll::Ready(());
			}
			loop {
				ready!(budget.poll_yield(waiter));
				match ready!(messages.poll_next(&mut reader, waiter)) {
					Ok(Some(_)) => continue,
					Ok(None) | Err(_) => return Poll::Ready(()),
				}
			}
		})
		.await;
	}

	/// Read NAMESPACE / NAMESPACE_DONE entries until either side ends the request.
	///
	/// `request.live` tracks the suffixes this stream has advertised, so a repeat is
	/// recognized as an update rather than a second advertisement, and the caller can
	/// release whatever is still held when the stream ends.
	async fn run_namespace_entries(
		&mut self,
		request: &mut NamespaceRequest,
		peer: &cluster::Peer,
		negotiated: ietf::Negotiated,
		mut initial: Initial,
		live: &kio::Consumer<()>,
	) -> Result<namespaces::Ended, Error> {
		// Each entry routes into the origin, so a peer streaming them must not hold the
		// session's other tasks.
		let mut budget = kio::coop::Budget::new(32);
		loop {
			kio::wait(|waiter| budget.poll_yield(waiter)).await;
			// Every counted NAMESPACE is in the origin by now, dropped ones included.
			if initial.initial == 0 {
				initial.snapshot = None;
			}
			let super::request_stream::Framed(type_id, body) = match self.next_namespace_message(request, live).await? {
				Some(Some(message)) => message,
				None => return Ok(namespaces::Ended::Local),
				// A stream ending inside the counted set cut it short.
				Some(None) if initial.initial > 0 => return Err(Error::ProtocolViolation),
				Some(None) => return Ok(namespaces::Ended::Peer),
			};
			let prefix = &request.prefix;
			let live = &mut request.live;
			let mut data = body.decoder(self.version);

			match type_id {
				// The suffix is relative to the prefix we subscribed, which is itself
				// relative to our root -- so the join is too, which is what everything
				// below wants (`create_broadcast` joins the root itself).
				ietf::Namespace::ID => {
					let msg = ietf::Namespace::decode_body(&mut data, self.version, negotiated)?;
					if !data.is_empty() {
						return Err(Error::WrongSize);
					}
					initial.initial = initial.initial.saturating_sub(1);
					let path = prefix.join(&msg.suffix)?;
					let params = msg.params.unwrap_or_default();
					let held = match live.entry(path.clone()) {
						// A repeat under another epoch replaces the instance in place, but one that
						// gains or loses an epoch skipped the NAMESPACE_DONE it needs.
						Entry::Occupied(entry) if entry.get().epoch.is_some() != params.epoch.is_some() => {
							tracing::warn!(%path, "namespace repeated gaining or losing an epoch");
							return Err(Error::ProtocolViolation);
						}
						Entry::Occupied(entry) => entry.into_mut(),
						// The peer holds the namespace until its NAMESPACE_DONE whatever we make
						// of it, so it takes its announce slot before the decision.
						Entry::Vacant(entry) => entry.insert(Advertisement {
							id: None,
							epoch: None,
							slot: std::sync::Arc::new(self.acquire_announce()?),
						}),
					};
					held.epoch = params.epoch.clone();
					let Some(advert) = self.route(params.cluster.as_ref(), peer) else {
						// Looped back through us: forwarding it would extend the loop and
						// subscribing through it would route us back to ourselves.
						//
						// An update replaces the advertisement it repeats, so a reflected
						// replacement retracts the route we were holding. Keeping it would
						// leave subscriptions on a path the peer no longer offers.
						//
						// The advertisement itself stays live on the wire, epoch and all.
						tracing::debug!(%path, "dropping reflected namespace");
						if let Some(id) = held.id.take() {
							let _ = self.stop_announce(path, id, false);
						}
						continue;
					};
					let advert = advert.with_epoch(params.epoch.clone());

					tracing::debug!(%path, hops = advert.route.hops.len(), cost = ?advert.route.cost, epoch = ?params.epoch, "namespace");
					match held.id {
						// A repeat replaces the advertisement atomically; nothing is torn
						// down merely because an update arrived, and another epoch restarts
						// the route without it ever going absent.
						Some(id) => self.update_announce(path, id, advert)?,
						None => {
							match self.start_announce(path.clone(), advert, Admit::Session(Some(held.slot.clone()))) {
								Ok(id) => held.id = Some(id),
								// The interest names a pattern's literal head, so the peer
								// legitimately advertises namespaces beneath it that the
								// scope excludes; those are filtered here, not fatal.
								Err(Error::Unauthorized) => {
									tracing::debug!(%path, "namespace outside the subscribe scope; ignoring");
								}
								Err(err) => return Err(err),
							}
						}
					}
				}
				// Only NAMESPACE may come before the counted set is in.
				ietf::NamespaceDone::ID if initial.initial > 0 => return Err(Error::ProtocolViolation),
				ietf::NamespaceDone::ID => {
					let msg = ietf::NamespaceDone::decode_msg(&mut data, self.version)?;
					let path = prefix.join(&msg.suffix)?;
					tracing::debug!(%path, "namespace_done");
					if let Some(id) = live.remove(&path).and_then(|held| held.id) {
						let _ = self.stop_announce(path, id, false);
					}
				}
				_ => {
					tracing::warn!(type_id, "unexpected message on subscribe_namespace stream");
					return Err(Error::UnexpectedMessage);
				}
			}
		}
	}

	/// Handle an incoming bidi stream dispatched by the session.
	///
	/// `peer` and `declared` are what the peer declared in its SETUP, which the dispatcher
	/// awaited once before accepting streams: PUBLISH_NAMESPACE cannot be parsed without
	/// knowing whether the MoQ Cluster extension is on, and `declared` says whether an
	/// unsolicited one is a bug (MoQ Solicit).
	pub fn handle_stream(
		&mut self,
		id: u64,
		body: ietf::Body,
		stream: Stream<Transport, Version>,
		peer: cluster::Peer,
		epoch: bool,
		declared: Option<bool>,
	) -> Result<BoxFuture<'static, ()>, Error> {
		let mut this = self.clone();
		let mut data = body.decoder(this.version);
		let task = match id {
			ietf::Publish::ID => {
				let msg = match ietf::Publish::decode_msg(&mut data, this.version) {
					Err(err) if super::token::refuses_request(&err) => {
						// Junk after the fields is fatal, which a token refusal must not hide.
						if !data.is_empty() {
							return Err(Error::WrongSize);
						}
						return Ok(this.reject_token(stream, request::Kind::Publish, err));
					}
					msg => msg?,
				};
				if !data.is_empty() {
					return Err(Error::WrongSize);
				}
				tracing::debug!(message = ?msg, "received publish");
				// Owned, since the task outlives the body it was read from.
				let msg = msg.into_owned();
				async move {
					if let Err(err) = this.serve_publish(stream, msg).await {
						tracing::debug!(%err, "publish stream error");
					}
				}
				.boxed()
			}
			ietf::PublishNamespace::ID => {
				// A negotiated session that omits HOP_PATH fails the decode here, which
				// the dispatcher turns into the protocol violation the draft requires.
				let negotiated = ietf::Negotiated {
					cluster: peer.negotiated(),
					epoch,
				};
				let msg = match ietf::PublishNamespace::decode_body(&mut data, this.version, negotiated) {
					Err(err) if super::token::refuses_request(&err) => {
						// Junk after the fields is fatal, which a token refusal must not hide.
						if !data.is_empty() {
							return Err(Error::WrongSize);
						}
						return Ok(this.reject_token(stream, request::Kind::PublishNamespace, err));
					}
					msg => msg?,
				};
				if !data.is_empty() {
					return Err(Error::WrongSize);
				}
				tracing::debug!(message = ?msg, "received publish_namespace");
				let gate = crate::auth::Gate::new(
					this.auth.clone(),
					msg.track_namespace.clone(),
					crate::auth::Direction::Subscribe,
				);
				let incoming = IncomingNamespace {
					msg,
					peer,
					declared,
					gate,
				};
				async move {
					if let Err(err) = this.serve_publish_namespace_stream(stream, incoming).await {
						// An advertisement update is decoded here rather than in the
						// dispatcher, so nothing else would surface a malformed one. The
						// cluster draft requires closing the session on those; a stream
						// the peer simply reset is not the peer's fault.
						if is_protocol_violation(&err) {
							this.session.close(
								super::error::to_session_code(&SessionError::from(&err)),
								err.to_string().as_ref(),
							);
						}
						tracing::debug!(%err, "publish_namespace stream error");
					}
				}
				.boxed()
			}
			_ => {
				tracing::warn!(id, "unexpected bidi stream type for subscriber");
				return Err(Error::UnexpectedStream);
			}
		};
		Ok(task)
	}

	/// Refuse a request whose AUTHORIZATION TOKEN repeats (section 8.9), which unlike any
	/// other decode error leaves the session open.
	fn reject_token(
		&self,
		stream: Stream<Transport, Version>,
		kind: request::Kind,
		err: DecodeError,
	) -> BoxFuture<'static, ()> {
		let version = self.version;
		async move {
			if let Err(err) = super::token::reject(stream, kind, err, version).await {
				tracing::debug!(%err, "token refusal failed");
			}
		}
		.boxed()
	}

	/// What the peer declared about being solicited (MoQ Solicit).
	///
	/// Read once by the dispatch loop and handed to each stream rather than awaited per
	/// stream: the slot is settled by the time streams are accepted, and a stream task
	/// that waited on it would park forever if it never were.
	pub(super) async fn solicit(&self) -> Option<bool> {
		self.peer_setup.get().await.solicit
	}

	/// Whether an incoming PUBLISH_NAMESPACE means the peer ignored our SETUP.
	///
	/// Unless [`with_solicit`](Self::with_solicit) turned it off, we declare that
	/// advertisements to us must be solicited (MoQ Solicit), and a peer that wrote the
	/// option at all proves it implements the extension, whichever value it chose. It also
	/// cannot have advertised before reading our SETUP, since our SETUP is what says whether
	/// advertising unasked is allowed. So this is a bug in the peer, and a silent one on
	/// both sides if we tolerate it.
	fn unsolicited_is_a_violation(&self, declared: Option<bool>) -> bool {
		if !self.solicit {
			return false;
		}
		declared.is_some()
	}

	/// Handle an incoming PUBLISH_NAMESPACE on its bidi stream.
	#[cfg(test)]
	async fn run_publish_namespace_stream(
		&mut self,
		stream: Stream<Transport, Version>,
		msg: ietf::PublishNamespace,
		peer: cluster::Peer,
		declared: Option<bool>,
	) -> Result<(), Error> {
		let gate = crate::auth::Gate::new(
			self.auth.clone(),
			msg.track_namespace.clone(),
			crate::auth::Direction::Subscribe,
		);
		self.serve_publish_namespace_stream(
			stream,
			IncomingNamespace {
				msg,
				peer,
				declared,
				gate,
			},
		)
		.await
	}

	async fn serve_publish_namespace_stream(
		&mut self,
		mut stream: Stream<Transport, Version>,
		incoming: IncomingNamespace,
	) -> Result<(), Error> {
		let IncomingNamespace {
			msg,
			peer,
			declared,
			gate,
		} = incoming;
		if let Some(error) = super::namespace::refusal(&msg.track_namespace, self.version) {
			return super::request_stream::reject(stream, error).await;
		}
		let path = msg.track_namespace.to_owned();

		if self.unsolicited_is_a_violation(declared) {
			tracing::warn!(%path, "unsolicited publish_namespace from a peer that implements MoQ Solicit");
			return Err(Error::ProtocolViolation);
		}

		// A path that already contains our own Hop ID looped back. Reject it rather
		// than attaching a source we would then have to route around.
		let epoch = msg.params.epoch.clone();
		let Some(advert) = self.route(msg.params.cluster.as_ref(), &peer) else {
			tracing::debug!(%path, "dropping reflected publish_namespace");
			self.write_error(&mut stream, &Error::Unroutable, "route loops through this relay")
				.await?;
			let _ = stream.writer.close().await;
			return Ok(());
		};
		let advert = advert.with_epoch(epoch.clone());

		// Our authorization of the peer (the limit) admits an announce without a token, and
		// holds back one it does not cover until it does. An AUTHORIZATION TOKEN on the
		// announce goes to the application to verify for this announce alone (MoQ
		// request-token), even where the limit covers it, and a verified one is exempt from the
		// limit while it lasts.
		let token = match &msg.authorization_token {
			Some(token) if gate.token_decides() => Some(token.0.clone()),
			_ => None,
		};
		let (admit, token_grant) = match token {
			None => (Admit::Session(None), None),
			Some(token) => {
				// Counted against the cap while the application verifies, unless the
				// namespace already holds a slot.
				let slot: Option<crate::auth::Hold> = match self.state.lock().broadcasts.contains_key(&path) {
					true => None,
					false => Some(std::sync::Arc::new(self.acquire_announce()?)),
				};
				let verdict = gate.verify(token, slot.clone());
				// The session closing or the peer withdrawing the announce withdraws the token.
				let session = self.session.clone();
				let version = self.version;
				let verified = kio::wait(|waiter| {
					if session.poll_closed(waiter).is_ready()
						|| super::request_stream::poll_withdrawn(&mut stream, version, waiter).is_ready()
					{
						return Poll::Ready(None);
					}
					verdict.poll_grant(waiter).map(Some)
				})
				.await;
				let Some(verified) = verified else {
					return Ok(());
				};
				let admitted = crate::auth::RequestGrant::new(
					&self.runtime,
					verdict,
					verified,
					path.clone(),
					crate::auth::RequestKind::PublishNamespace,
					slot.clone(),
				);
				match admitted {
					Ok(grant) => (Admit::Token(slot), Some(grant)),
					Err(refusal) => {
						self.write_error(&mut stream, &refusal.error, &refusal.reason).await?;
						let _ = stream.writer.close().await;
						return Ok(());
					}
				}
			}
		};
		let token = token_grant.is_some();

		// Everything the advertisement does once admitted runs under its grant: one that ends
		// at any await, the first answer included, drops it right there. `attached` says the
		// route is up, and `acked` that the OK went out, which decides how the peer is told.
		let mut attached = None;
		let mut acked = false;
		let lived = crate::auth::RequestGrant::guard(token_grant.as_ref(), async {
			match self.start_announce(path.clone(), advert, admit) {
				Ok(id) => attached = Some(id),
				Err(err) => return Ok(Withdraw::Reject(err.into())),
			}
			// Buffered whole before its first await, so however this is dropped from here, the
			// advertisement can still end on the wire.
			acked = true;
			self.write_ok(&mut stream).await?;

			// An endpoint updates an advertisement with REQUEST_UPDATE on the stream that
			// already carries it, so keep reading until the stream closes.
			self.run_publish_namespace_updates(
				&mut stream,
				&path,
				Held {
					cluster: msg.params.cluster,
					epoch,
				},
				peer,
				&mut attached,
				token_grant.clone(),
			)
			.await
		})
		.await;

		// The route goes before anything waits on the peer, so a peer that stops reading
		// cannot keep it routed. A stream that detached mid-flight (a reflected update) has
		// nothing left to release.
		self.release_announce(&path, &mut attached, token)?;

		let withdraw = match lived {
			Ok(withdraw) => withdraw?,
			Err(refusal) => {
				tracing::info!(%path, err = %refusal.error, reason = %refusal.reason, "request token grant ended");
				if !acked {
					self.write_error(&mut stream, &refusal.error, &refusal.reason).await?;
					let _ = stream.writer.close().await;
					return Ok(());
				}
				self.cancel_announce(&mut stream).await?;
				return Err(refusal.error);
			}
		};
		match withdraw {
			Withdraw::Done => {}
			Withdraw::Close => {
				let _ = stream.writer.close().await;
			}
			Withdraw::Reject(refusal) => {
				self.write_error(&mut stream, &refusal.error, &refusal.reason).await?;
				let _ = stream.writer.close().await;
			}
			Withdraw::Cancel(refusal) => {
				self.write_error(&mut stream, &refusal.error, &refusal.reason).await?;
				self.cancel_announce(&mut stream).await?;
			}
		}
		Ok(())
	}

	/// Read advertisement updates off a live PUBLISH_NAMESPACE stream until it closes, and
	/// return how it ends, which the caller writes once the route is released.
	///
	/// `held` is what the peer advertised, kept current because a REQUEST_UPDATE carries
	/// only what changed. Each one is answered with REQUEST_OK, or REQUEST_ERROR and a
	/// closed stream when it cannot be applied, which withdraws the advertisement
	/// (moq-transport Section 9.5.1).
	async fn run_publish_namespace_updates(
		&mut self,
		stream: &mut Stream<Transport, Version>,
		path: &Path,
		held: Held,
		peer: cluster::Peer,
		attached: &mut Option<u64>,
		token_grant: Option<crate::auth::RequestGrant>,
	) -> Result<Withdraw, Error> {
		let Held {
			cluster: mut held,
			epoch,
		} = held;
		// A REQUEST_UPDATE carrying a fresh token refreshes the announce's request grant (MoQ
		// request-token); its verify is raced against the stream in the loop's single wait
		// (never a bare await), so a peer ending the announce ends it whatever the acceptor
		// does, and the caller's guard ends it when the old grant does. Messages behind a
		// pending verdict wait in `renewal`, handled in order once it resolves.
		let mut renewal = request_update::Renewal::<(u64, ietf::Body)>::default();
		let mut messages = super::request_stream::Messages::new(self.session.clone(), self.control.client());
		// Draft-22 only: the peer finished its send direction, which does not withdraw the
		// advertisement there, so stop reading and keep watching the writer.
		let mut finished = false;

		// What one turn of the loop resolved to.
		enum Turn {
			// A whole message off the stream, or one drained from the queue.
			Message(u64, ietf::Body),
			// The peer finished its send direction.
			Fin,
			// Our send direction closed (the peer stopped reading or reset it).
			WriterClosed(Result<(), Error>),
			// The read failed.
			Failed(Error),
			// A pending renewal's verdict resolved, for the update with this request id.
			Renewal(request_update::Verdict),
		}

		// Request IDs of updates queued behind a pending verdict, already spent.
		let mut spent = std::collections::HashSet::new();
		// Each update re-routes in the origin and is answered, so a peer streaming them must
		// not hold the session's other tasks.
		let mut budget = kio::coop::Budget::new(32);
		loop {
			kio::wait(|waiter| budget.poll_yield(waiter)).await;
			// Handle one buffered update before reading the next, so answers stay in order.
			let turn = if let Some((id, body)) = renewal.next() {
				Turn::Message(id, body)
			} else {
				kio::wait(|waiter| {
					if let Poll::Ready(result) = stream.writer.poll_closed(waiter) {
						return Poll::Ready(Turn::WriterClosed(result));
					}
					if let Poll::Ready(renewed) = renewal.poll(waiter) {
						return Poll::Ready(Turn::Renewal(renewed));
					}
					// Keep reading while a verdict is pending, so a withdrawal still ends the
					// announce: a buffered update must not mask a later terminal.
					if !finished {
						return match messages.poll_next(&mut stream.reader, waiter) {
							Poll::Ready(Ok(Some(super::request_stream::Framed(id, body)))) => {
								Poll::Ready(Turn::Message(id, body))
							}
							Poll::Ready(Ok(None)) => Poll::Ready(Turn::Fin),
							Poll::Ready(Err(err)) => Poll::Ready(Turn::Failed(err)),
							Poll::Pending => Poll::Pending,
						};
					}
					Poll::Pending
				})
				.await
			};

			let (type_id, body) = match turn {
				// A non-terminal message read while a verdict is pending waits for it.
				Turn::Message(id, body) if renewal.pending() => {
					if id == ietf::PublishNamespaceUpdate::ID {
						// Its request ID is spent on arrival, so one dropped with the queue still moves
						// GOAWAY's cutoff past it.
						let update_id = ietf::RequestId::decode(&mut body.decoder(self.version), self.version)?;
						if let Err(err) = self.spend(update_id) {
							if matches!(err, Error::GoingAway) {
								return Ok(Withdraw::Cancel(crate::auth::Refusal {
									error: err,
									reason: "going away".to_string(),
								}));
							}
							return Err(err);
						}
						spent.insert(update_id);
						// Decoded on arrival too, so a session-fatal update still closes the session
						// when a withdrawal drops the queue. A token refusal only refuses this
						// announce, which waits for the update's turn.
						let mut data = body.decoder(self.version);
						match ietf::PublishNamespaceUpdate::decode_msg(&mut data, self.version) {
							Err(err) if super::token::refuses_request(&err) => {}
							msg => {
								msg?;
							}
						}
						if !data.is_empty() {
							return Err(Error::WrongSize);
						}
					}
					match renewal.queue((id, body), self.version) {
						Ok(()) => continue,
						// The dispatcher closes only on is_protocol_violation, which excludes
						// Error::Session, so the session is closed here.
						Err(request_update::Overflow::Session) => {
							return Err(request_update::too_many(&mut self.session));
						}
						// Drafts below 19 negotiate no limit, so this is a local memory guard, not a
						// protocol fault: end this announce, never the session.
						Err(request_update::Overflow::Request) => return Ok(Withdraw::Close),
					}
				}
				Turn::Message(id, body) => (id, body),
				Turn::Fin => {
					if super::request_stream::fin_cancels(self.version) {
						return Ok(Withdraw::Done);
					}
					// The advertisement outlives the peer's send direction.
					finished = true;
					continue;
				}
				// Already finished: the advertisement outlived the peer's send direction until now.
				Turn::WriterClosed(result) if finished => return result.map(|()| Withdraw::Done),
				Turn::WriterClosed(result) => {
					stream.reader.abort(&Error::Cancel);
					result?;
					if !super::request_stream::fin_cancels(self.version) {
						stream.writer.closed().await?;
					}
					return Ok(Withdraw::Done);
				}
				Turn::Failed(err) => return Err(err),
				Turn::Renewal(request_update::Verdict { verdict, grant: res }) => {
					// Only a token-authorized announce starts a renewal.
					let rg = token_grant.as_ref().expect("a renewal without a request grant");
					// On accept the old grant is dropped and the deadline re-armed (REQUEST_OK).
					let Err(refusal) = rg.renew(verdict, res) else {
						self.write_ok(stream).await?;
						continue;
					};
					// Per draft-18 section 10.9.1, a refused or uncovered
					// renewal ends only this announce. Answer REQUEST_ERROR, then close the stream;
					// the session stays up and the old grant does not survive.
					return Ok(Withdraw::Cancel(refusal));
				}
			};

			if type_id != ietf::PublishNamespaceUpdate::ID {
				// A repeated PUBLISH_NAMESPACE lands here too: a second request on the
				// stream is the base draft's duplicate request ID.
				tracing::warn!(type_id, "unexpected message on publish_namespace stream");
				return Err(Error::UnexpectedMessage);
			}

			let mut data = body.decoder(self.version);

			// An update spends its own request ID, even one refused below. A queued one was
			// spent when it arrived.
			let update_id = ietf::RequestId::decode(&mut body.decoder(self.version), self.version)?;
			if !spent.remove(&update_id)
				&& let Err(err) = self.spend(update_id)
			{
				if matches!(err, Error::GoingAway) {
					return Ok(Withdraw::Cancel(crate::auth::Refusal {
						error: err,
						reason: "going away".to_string(),
					}));
				}
				return Err(err);
			}
			let msg = match ietf::PublishNamespaceUpdate::decode_msg(&mut data, self.version) {
				// A token refusal rejects the update, which withdraws the advertisement as
				// any refused update does.
				Err(err) if super::token::refuses_request(&err) => {
					// Junk after the fields is fatal, which a token refusal must not hide.
					if !data.is_empty() {
						return Err(Error::WrongSize);
					}
					let refusal = crate::auth::Refusal {
						reason: super::token::reason(&err).to_string(),
						error: Error::Decode(err),
					};
					return Ok(Withdraw::Cancel(refusal));
				}
				msg => msg?,
			};
			// Junk inside the declared size would otherwise be applied silently, which
			// is the one decode path that skipped the check the others make.
			if !data.is_empty() {
				return Err(Error::WrongSize);
			}

			// Cluster parameters and a token renewal can ride the same update, so apply the
			// routing first, for every update that carries it, before dealing with the token.
			// A different original publisher applies in place too: the origin drains what the
			// old one already serves and never splices the two. The parameters exist only on a
			// session that negotiated the extension; anywhere else they are the peer's violation.
			let carries_cluster = msg.hops.is_some() || msg.cost.is_some();
			held = match &held {
				Some(current) => Some(msg.apply(current)),
				None if carries_cluster => {
					tracing::warn!(%path, "cluster parameters on a session that negotiated none");
					return Err(Error::ProtocolViolation);
				}
				None => None,
			};

			// Re-route only when the update actually changes the route (an omitted parameter
			// keeps its value, so a token-only update leaves it untouched). A path that now runs
			// through us is unusable, so detach rather than keep serving it; reading continues,
			// since this stream is the advertisement's only channel and a later clean path
			// arrives here or nowhere. Ending the stream is not ours to do: a peer MAY
			// legitimately send a path carrying our Hop ID when a redundant sibling shares it.
			let applied: Result<(), Error> = match carries_cluster {
				true => match self.route(held.as_ref(), &peer) {
					None => {
						if let Some(id) = attached.take() {
							tracing::debug!(%path, "publish_namespace now loops back; detaching");
							let _ = self.stop_announce(path.clone(), id, token_grant.is_some());
						}
						Ok(())
					}
					Some(advert) => {
						// The epoch is the advertisement's for as long as it stands.
						let advert = advert.with_epoch(epoch.clone());
						tracing::debug!(%path, hops = advert.route.hops.len(), cost = ?advert.route.cost, "publish_namespace update");
						match *attached {
							Some(id) => self.update_announce(path.clone(), id, advert),
							// Re-attach: a clean path replaced the reflected one we detached from.
							None => {
								// The grant still holds the slot it was admitted with, so reuse it.
								let admit = match &token_grant {
									Some(grant) => Admit::Token(grant.hold()),
									None => Admit::Session(None),
								};
								self.start_announce(path.clone(), advert, admit)
									.map(|id| *attached = Some(id))
							}
						}
					}
				},
				false => Ok(()),
			};

			// An unroutable apply withdraws the announce whether or not a token also rides it.
			// The close is the withdrawal.
			if let Err(err) = applied {
				tracing::warn!(%path, %err, "publish_namespace update refused");
				return Ok(Withdraw::Reject(err.into()));
			}

			// A REQUEST_UPDATE carrying a fresh token refreshes the announce's request grant
			// (MoQ request-token), when the announce is token-authorized. The verify is not
			// awaited here: it becomes the pending renewal raced against the deadline above.
			// The routing above is already applied, so the renewal's answer (written when its
			// verdict resolves) is this update's single response, cluster parameters included.
			if let Some(token) = msg.authorization_token {
				match &token_grant {
					Some(grant) => {
						renewal.verify(grant.verify_renewal(&self.auth, token.0));
						continue;
					}
					// Where tokens decide, a token on an announce admitted without one is refused
					// rather than taken unread, so every token this side admits is verified.
					None if self.auth.token_decides(crate::auth::Direction::Subscribe) => {
						let refusal = crate::auth::Refusal {
							error: Error::Unsupported,
							reason: "AUTHORIZATION TOKEN on an announce admitted without one".to_string(),
						};
						return Ok(Withdraw::Cancel(refusal));
					}
					None => {}
				}
			}

			// No token rides this update: acknowledge it now.
			self.write_ok(stream).await?;
		}
	}

	/// Retract an advertisement's route ahead of a withdrawal that waits on the peer, so a peer
	/// that stops reading cannot keep it routed. `attached` makes it once only.
	fn release_announce(&mut self, path: &Path, attached: &mut Option<u64>, token: bool) -> Result<(), Error> {
		if let Some(id) = attached.take() {
			self.stop_announce(path.clone(), id, token)?;
		}
		Ok(())
	}

	/// End an advertisement we accepted, leaving the session up: a closed stream.
	async fn cancel_announce(&self, stream: &mut Stream<Transport, Version>) -> Result<(), Error> {
		// An OK cut short by the grant ending is still buffered, and goes out ahead of the FIN.
		kio::wait(|waiter| stream.writer.poll_flush(waiter)).await?;
		if stream.writer.finish().is_ok() {
			let _ = stream.writer.closed().await;
		}
		Ok(())
	}

	/// Reject an incoming PUBLISH.
	///
	/// PUBLISH offers a single track, so honoring it means routing per
	/// (namespace, track). Our model routes per namespace: a source attaches at a
	/// path and serves every track under it, resolved on demand via SUBSCRIBE.
	/// Accepting a PUBLISH would mean inventing a namespace-level source out of a
	/// track-level offer, and that fiction then contradicts any real
	/// PUBLISH_NAMESPACE for the same path.
	///
	/// Declining the request rather than failing the session, since a peer using
	/// a feature we don't implement is not a protocol violation.
	async fn run_publish_stream(
		&mut self,
		mut stream: Stream<Transport, Version>,
		msg: ietf::Publish<'static>,
	) -> Result<(), Error> {
		if let Some(error) = super::namespace::refusal(&msg.track_namespace, self.version) {
			return super::request_stream::reject(stream, error).await;
		}
		tracing::debug!(broadcast = %msg.track_namespace, track = %msg.track_name, "rejecting publish");

		// We decline the method itself rather than this particular track, which would be
		// UNINTERESTED.
		//
		// The alias the message carries is deliberately not recorded. Nothing will ever bind
		// it, and a rejected request has no lifetime of ours to hang the cleanup on, so the
		// entry would have to be swept asynchronously. Any data streams the publisher opened
		// before reading this are dropped by the unknown-alias path instead.
		self.write_publish_error(&mut stream, &Error::Unsupported, "PUBLISH is not supported")
			.await?;
		// The rejection is the whole exchange, but it still has to arrive: a finish alone
		// leaves the drop-time reset free to discard it before the peer acknowledges it.
		let _ = stream.writer.close().await;

		Ok(())
	}

	/// Send OK on the bidi stream.
	/// Spend a peer's request ID, closing the session on one it already used.
	fn spend(&self, id: RequestId) -> Result<(), Error> {
		if let Err(err) = self.control.accept(id) {
			if matches!(err, Error::GoingAway) {
				return Err(err);
			}
			self.session.clone().close(
				super::error::to_session_code(&crate::SessionError::from(&err)),
				"invalid request ID",
			);
			return Err(err);
		}
		Ok(())
	}

	async fn write_ok(&self, stream: &mut Stream<Transport, Version>) -> Result<(), Error> {
		stream
			.writer
			.encode_message(&ietf::RequestOk {
				largest: None,
				active: None,
			})
			.await?;
		Ok(())
	}

	/// Refuse a PUBLISH_NAMESPACE on the bidi stream that carries it.
	async fn write_error(
		&self,
		stream: &mut Stream<Transport, Version>,
		err: &Error,
		reason: &str,
	) -> Result<(), Error> {
		super::request_stream::write_error(
			&mut stream.writer,
			request::Kind::PublishNamespace,
			err,
			reason,
			self.version,
		)
		.await
	}

	/// Refuse a PUBLISH on the bidi stream that carries it.
	async fn write_publish_error(
		&self,
		stream: &mut Stream<Transport, Version>,
		err: &Error,
		reason: &str,
	) -> Result<(), Error> {
		super::request_stream::write_error(&mut stream.writer, request::Kind::Publish, err, reason, self.version).await
	}

	/// Attach the route for one newly advertised namespace, returning the id that names
	/// this advertisement.
	///
	/// Pair with [`Self::stop_announce`].
	fn start_announce(&mut self, path: Path, advert: Advertised, admit: Admit) -> Result<u64, Error> {
		let mut state = self.state.lock();
		let (token, slot) = match admit {
			Admit::Session(slot) => (false, slot),
			Admit::Token(slot) => (true, slot),
		};
		let id = state.next_advert;
		state.next_advert += 1;
		let existing = state.broadcasts.contains_key(&path);
		self.attach(&mut state, path.clone(), id, advert, slot, token)?;
		if !existing {
			return Ok(id);
		}
		let Some(entry) = state.broadcasts.get_mut(&path) else {
			return Ok(id);
		};
		// A token stands in for the limit, so a namespace the limit withheld attaches now.
		let attached = match token {
			true => self.reattach(&path, entry)?,
			false => None,
		};
		entry.tokens += usize::from(token);
		drop(state);
		if let Some(generation) = attached {
			self.serve_route(path, generation);
		}
		Ok(id)
	}

	/// Attach the route of a namespace the limit withheld, returning the generation
	/// to serve. `None` when it is attached already.
	fn reattach(&self, path: &Path, entry: &mut BroadcastState) -> Result<Option<u64>, Error> {
		if entry.dynamic.is_some() {
			return Ok(None);
		}
		let mut route = entry.route.clone();
		if self.going_away.is_set() {
			route.cost = crate::origin::Cost::DRAIN;
		}
		entry.dynamic = Some(self.origin.requests_route(path, route, None)?);
		entry.generation += 1;
		tracing::info!(route = %crate::path::Rooted(self.origin.root(), path), "namespace authorized again");
		Ok(Some(entry.generation))
	}

	/// Apply a changed advertisement `id` to a namespace that is already attached.
	///
	/// An update replaces the advertisement atomically: the refcount does not move,
	/// and no subscription is torn down merely because one arrived.
	fn update_announce(&mut self, path: Path, id: u64, advert: Advertised) -> Result<(), Error> {
		let mut state = self.state.lock();
		if !state.broadcasts.contains_key(&path) {
			return Err(Error::NotFound);
		}
		self.attach(&mut state, path, id, advert, None, false)?;
		Ok(())
	}

	/// Create the announced route for one namespace, or record advertisement `id` against
	/// one that exists: added when new, replaced when it repeats.
	///
	/// This is the semantic heart of the mapping: a moq-transport namespace IS a
	/// prefix route, so a PUBLISH_NAMESPACE advertises the whole prefix and paths
	/// beneath it materialize on demand.
	///
	/// `slot` is one taken ahead for a new entry, and `token` exempts a new entry from
	/// the limit.
	fn attach(
		&self,
		state: &mut State,
		path: Path,
		id: u64,
		advert: Advertised,
		slot: Option<crate::auth::Hold>,
		token: bool,
	) -> Result<(), Error> {
		let Advertised { mut route } = advert;

		// A namespace published after the peer's GOAWAY starts out draining, so
		// a late arrival on a dying connection can't take over as primary.
		if self.going_away.is_set() {
			route.cost = crate::origin::Cost::DRAIN;
		}

		match state.broadcasts.entry(path.clone()) {
			Entry::Occupied(entry) => {
				let entry = entry.into_mut();
				match entry.adverts.iter().position(|(advert, _)| *advert == id) {
					// A repeat under another epoch is a newer advertisement, so it becomes the
					// newest; one of the same instance stays where it is.
					Some(index) if entry.adverts[index].1.epoch != route.epoch => {
						entry.adverts.remove(index);
						entry.adverts.push((id, route));
					}
					Some(index) => entry.adverts[index].1 = route,
					None => entry.adverts.push((id, route)),
				}
				// The newest advertisement stands, and an update to an older one changes
				// nothing. A repeat of the same instance re-prices in place and in-flight
				// tracks keep flowing; another epoch restarts it.
				let (newest, route) = entry.adverts.last().expect("just recorded");
				if *newest == id {
					entry.route = route.clone();
					if let Some(dynamic) = &entry.dynamic {
						dynamic.update_route(entry.route.clone())?;
					}
				}
				Ok(())
			}
			Entry::Vacant(entry) => {
				// A peer past its limits loses the session, not just this namespace.
				let slot = match slot {
					Some(slot) => slot,
					None => std::sync::Arc::new(self.acquire_announce()?),
				};
				// Outside the session's limit: held, not refused, so a wider limit can
				// attach it while the peer still advertises it.
				if !token && !self.auth.within_limit(crate::auth::Direction::Subscribe, path.as_str()) {
					tracing::debug!(route = %crate::path::Rooted(self.origin.root(), &path), "withholding announce outside the limit");
					entry.insert(BroadcastState {
						route: route.clone(),
						adverts: vec![(id, route)],
						dynamic: None,
						generation: 0,
						tokens: usize::from(token),
						live: Default::default(),
						_slot: slot,
					});
					return Ok(());
				}
				// Propagates Error::Unauthorized if the namespace is out of scope.
				let dynamic = self.origin.requests_route(&path, route.clone(), None)?;

				entry.insert(BroadcastState {
					route: route.clone(),
					adverts: vec![(id, route)],
					dynamic: Some(dynamic),
					generation: 0,
					tokens: usize::from(token),
					live: Default::default(),
					_slot: slot,
				});

				tracing::debug!(route = %crate::path::Rooted(self.origin.root(), &path), "announce");
				self.serve_route(path, 0);

				Ok(())
			}
		}
	}

	/// Take one slot of the session's announce cap. A peer past it loses the session.
	fn acquire_announce(&self) -> Result<crate::session::Slot, Error> {
		self.announces.acquire().inspect_err(|err| {
			self.session.clone().close(
				super::error::to_session_code(&crate::SessionError::from(err)),
				"too many announcements",
			);
		})
	}

	/// Release advertisement `id` of `path`, closing its sources when it was the last.
	/// `token` says its own request token authorized it.
	fn stop_announce(&mut self, path: Path, id: u64, token: bool) -> Result<(), Error> {
		let mut state = self.state.lock();

		match state.broadcasts.entry(path.clone()) {
			Entry::Occupied(mut entry) => {
				let state = entry.get_mut();
				let Some(index) = state.adverts.iter().position(|(advert, _)| *advert == id) else {
					return Err(Error::NotFound);
				};
				state.adverts.remove(index);
				state.tokens -= usize::from(token);
				if state.adverts.is_empty() {
					tracing::debug!(route = %crate::path::Rooted(self.origin.root(), &path), "unannounced");
					// Dropping the entry retracts the route (its announcement drops) and
					// closes its sources (its token closes).
					entry.remove();
					return Ok(());
				}
				// The newest went while an older one stands: that one is what remains.
				let (_, newest) = state.adverts.last().expect("not empty");
				if *newest != state.route {
					state.route = newest.clone();
					if let Some(dynamic) = &state.dynamic {
						dynamic.update_route(state.route.clone())?;
					}
				}
				if state.tokens == 0
					&& state.dynamic.is_some()
					&& !self.auth.within_limit(crate::auth::Direction::Subscribe, path.as_str())
				{
					// The last token that stood in for the limit is gone: hold the route back.
					tracing::info!(route = %crate::path::Rooted(self.origin.root(), &path), "namespace no longer authorized");
					state.dynamic = None;
					state.live = Default::default();
				}
			}
			Entry::Vacant(_) => return Err(Error::NotFound),
		};

		Ok(())
	}

	/// Run `tasks` to their own end, or until the session dies.
	///
	/// A retraction does not disturb subscriptions already in flight: what ended
	/// takes no new work, but the work it started finishes.
	async fn drain(&self, tasks: &mut TaskSet) {
		let session = self.session.clone();
		kio::wait(|waiter| {
			if session.poll_closed(waiter).is_ready() {
				return Poll::Ready(());
			}
			tasks.poll(waiter)
		})
		.await
	}

	/// Serve materialization requests for one announced namespace: mint a source
	/// per requested path and serve its track requests until the route is
	/// retracted or the session dies. Tracks in flight at a retraction run to
	/// their own end, unless the announce was what authorized them.
	async fn run_route(&self, path: Path, generation: u64) {
		let mut broadcasts = TaskSet::owned();
		let closed_session = self.session.clone();
		let mut epoch = 0;
		loop {
			let next = broadcasts
				.drive(|waiter| {
					if closed_session.poll_closed(waiter).is_ready() {
						return Poll::Ready(None);
					}
					// A draining peer usually stops publishing namespaces, so react
					// to the GOAWAY itself; waiting for another message would leave
					// the route primary until the session finally closed.
					// Idempotent, since the signal stays set.
					if self.going_away.poll(waiter).is_ready() {
						self.drain_route(&path);
					}
					// A limit that no longer covers the namespace holds the route back as a
					// retraction would, closing its sources. Tracks in flight end on their
					// own gates, with `Unauthorized`.
					let mut excluded = false;
					while let Poll::Ready(permit) =
						self.auth
							.poll_permit(crate::auth::Direction::Subscribe, &mut epoch, waiter)
					{
						excluded = !permit.within_limit(path.as_str());
					}
					// The route lives in the entry: stop_announce removing it retracts
					// the route, and this loop ends with it.
					let mut state = self.state.lock();
					let Some(entry) = state.broadcasts.get_mut(&path) else {
						return Poll::Ready(None);
					};
					if entry.generation != generation {
						return Poll::Ready(None);
					}
					// A namespace its own request token authorized is exempt from the limit.
					if excluded && entry.tokens == 0 && entry.dynamic.is_some() {
						tracing::info!(route = %crate::path::Rooted(self.origin.root(), &path), "namespace no longer authorized");
						entry.dynamic = None;
						// Dropping the namespace's token closes every source minted under it.
						entry.live = Default::default();
					}
					match &mut entry.dynamic {
						Some(dynamic) => dynamic.poll_requested_broadcast(waiter).map(Some),
						None => Poll::Ready(None),
					}
				})
				.await;

			let request = match next {
				Some(Ok(request)) => request,
				// Retracted or torn down: no request will ever arrive again, but
				// the broadcasts already served keep their tracks in flight.
				Some(Err(_)) | None => {
					self.drain(&mut broadcasts).await;
					break;
				}
			};

			// The request path is absolute; the wire (and our origin handle) speak
			// paths relative to the session's root.
			let requested = match request.path().strip_prefix(self.origin.root()) {
				Some(requested) => requested.clone(),
				None => continue,
			};
			// The source serves the instance the route names now: every request for it names
			// that epoch when the peer can read one, and the source takes new tracks only
			// while the peer still selects that instance.
			let (validity, epoch) = {
				let state = self.state.lock();
				let entry = state
					.broadcasts
					.get(&path)
					.filter(|entry| entry.generation == generation);
				let validity = entry
					.and_then(|entry| entry.dynamic.as_ref())
					.map(|route| crate::model::broadcast::RequestValidity::for_source(route, request.path()))
					.unwrap_or_else(|| {
						crate::model::broadcast::RequestValidity::token(Some(kio::Producer::new(()).consume()))
					});
				(validity, entry.and_then(|entry| entry.route.epoch.clone()))
			};
			let source =
				crate::model::broadcast::SourceGuard::new(self.origin.create_source(requested.clone(), validity));
			// The handler exists before the requester sees the source, so a track it asks
			// for before `run_broadcast` first polls queues instead of failing `NotFound`.
			let dynamic = source.requests();
			request.accept(&*source);

			// The namespace's token closes the source as it is retracted. If it was
			// retracted since the accept, close it here as that retraction would have,
			// and still serve what it took on: tracks subscribed since carry on.
			let route = self
				.state
				.lock()
				.broadcasts
				.get(&path)
				.filter(|entry| entry.generation == generation)
				.map(|entry| entry.live.consume());
			if route.is_none() {
				source.close();
			}

			let mut this = self.clone();
			this.validity = dynamic.validity().clone();
			this.epoch = epoch;
			broadcasts.push(async move {
				if let Err(err) = this.run_broadcast(requested, source, dynamic, route).await {
					tracing::debug!(%err, "error running broadcast");
				}
			});
		}
	}

	/// Re-price one attached route to a draining cost (the peer sent a GOAWAY):
	/// every other candidate outranks it while it stays selectable as the last
	/// path. Idempotent, since the signal stays set.
	fn drain_route(&self, path: &Path) {
		let mut state = self.state.lock();
		let Some(entry) = state.broadcasts.get_mut(path) else {
			return;
		};
		if entry.route.cost == crate::origin::Cost::DRAIN {
			return;
		}
		entry.route.cost = crate::origin::Cost::DRAIN;
		for (_, route) in &mut entry.adverts {
			route.cost = crate::origin::Cost::DRAIN;
		}
		if let Some(dynamic) = &entry.dynamic {
			let _ = dynamic.update_route(entry.route.clone());
		}
	}

	/// Serve one minted source's track requests, taken through its `broadcast` handler,
	/// until it closes: its namespace is retracted (`route` closes), the peer stops
	/// selecting its instance, nothing holds it any more, or the session dies.
	async fn run_broadcast(
		&self,
		path: Path,
		source: crate::model::broadcast::SourceGuard,
		mut broadcast: broadcast::Requests,
		route: Option<kio::Consumer<()>>,
	) -> Result<(), Error> {
		let mut subscribes = TaskSet::owned();
		let closed_session = self.session.clone();
		// Answers the source's listings with the peer's tracks, while anyone lists it.
		let lister = std::sync::Arc::new(
			broadcast
				.lister()
				.expect("only the source answers its broadcast's listings"),
		);
		let mut listening = None;
		loop {
			let next = subscribes
				.drive(|waiter| {
					if closed_session.poll_closed(waiter).is_ready() {
						return Poll::Ready(None);
					}
					self.poll_listing(&path, &lister, &mut listening, waiter);
					loop {
						if let Poll::Ready(next) = broadcast.poll_requested_track(waiter) {
							return Poll::Ready(Some(next));
						}
						if route.as_ref().is_some_and(|route| route.poll_closed(waiter).is_ready()) {
							source.close();
							continue;
						}
						// Nothing holds the source and no track is on its way: retire it, so
						// what the session keeps for the path goes with the fronts that used
						// it. Declined when a holder or a track got there first, so look again.
						if source.poll_unheld(waiter).is_pending() {
							return Poll::Pending;
						}
						source.close_unheld();
					}
				})
				.await;

			let request = match next {
				Some(Ok(request)) => request,
				Some(Err(err)) => {
					tracing::debug!(%err, "broadcast closed");
					drop(listening.take());
					// No new tracks, but those in flight run to their own end.
					self.drain(&mut subscribes).await;
					break;
				}
				// Session gone.
				None => break,
			};

			let mut this = self.clone();

			let path = path.clone();
			let broadcast = broadcast.clone();
			let route = route.clone();
			subscribes.push(async move {
				this.run_subscribe(path, broadcast, route, request).await;
			});
		}

		Ok(())
	}

	/// Answer the source's listings while anyone lists it, from a SUBSCRIBE_TRACKS covering
	/// it; a failed listing ends its readers, which ask again.
	fn poll_listing(
		&self,
		path: &Path,
		lister: &std::sync::Arc<broadcast::Lister>,
		listening: &mut Option<tracks::Listening>,
		waiter: &kio::Waiter,
	) {
		loop {
			match listening {
				None => {
					if !matches!(lister.poll_wanted(waiter), Poll::Ready(Ok(()))) {
						return;
					}
					*listening = Some(self.listen(path.to_owned(), lister.clone()));
				}
				Some(_) if lister.poll_unwanted(waiter).is_ready() => {
					*listening = None;
					lister.clear();
				}
				Some(current) => match current.poll_failed(waiter) {
					Poll::Ready(err) => {
						*listening = None;
						lister.abort(err);
					}
					Poll::Pending => return,
				},
			}
		}
	}

	async fn run_subscribe(
		&mut self,
		broadcast_path: Path,
		// Held for the subscription's lifetime but never watched: the broadcast ending
		// is a retraction, which does not disturb a subscription the scope allows.
		_broadcast: broadcast::Requests,
		// Open while the namespace stays announced and authorized, which its own token may
		// have done in place of the scope. `None` once retracted.
		route: Option<kio::Consumer<()>>,
		request: track::Request,
	) {
		// Data streams wait on the alias bound by SUBSCRIBE_OK, so leave the model request
		// pending until its immutable track metadata is known.
		// Subscribe only to what our grant covers (MoQ Auth), and cancel once it no longer
		// does, leaving the rest of the session alone.
		let (auth, path, direction) = (
			self.auth.clone(),
			broadcast_path.clone(),
			crate::auth::Direction::Subscribe,
		);
		let mut gate = match route {
			Some(live) => crate::auth::Gate::announced(auth, path, direction, live),
			None => crate::auth::Gate::new(auth, path, direction),
		};
		// Nobody wanting the track any more ends a wait on a token that may never be answered.
		loop {
			let admitted = kio::wait(|waiter| {
				if gate.poll_ready(waiter).is_ready() {
					return Poll::Ready(Some(gate.poll_denied(waiter).is_pending()));
				}
				crate::util::poll_unwanted(&mut request.demand(), waiter).map(|_| None)
			})
			.await;
			match admitted {
				Some(true) => break,
				Some(false) => {
					request.reject(Error::Unauthorized);
					return;
				}
				None if request.reject_unwanted(Error::Cancel) => return,
				// Demand returned in the gap.
				None => continue,
			}
		}

		let track_name = request.name().to_owned();
		// Group FETCHes for cache misses: standalone, so they outlive each subscription.
		let mut group_fetches = TaskSet::owned();

		// Demand with nobody subscribing (fetches, or a TRACK_STATUS downstream) learns the
		// track from TRACK_STATUS rather than a SUBSCRIBE: a finished track refuses one, and
		// one made only to learn the track races the fetches it was for.
		let mut target = match request.subscription() {
			subscription
				if subscription
					.as_ref()
					.is_none_or(|subscription| subscription.ranges.is_empty()) =>
			{
				let Some(idle) = self
					.track_status(&broadcast_path, &track_name, request, &mut gate)
					.await
				else {
					return;
				};
				let Some(next) = self
					.linger(&broadcast_path, &track_name, idle, &mut group_fetches, &mut gate)
					.await
				else {
					return;
				};
				Target::Resume(next)
			}
			_ => Target::Request(request),
		};
		loop {
			let Some(idle) = self
				.subscribe_once(&broadcast_path, &track_name, target, &mut group_fetches, &mut gate)
				.await
			else {
				return;
			};
			let Some(next) = self
				.linger(&broadcast_path, &track_name, idle, &mut group_fetches, &mut gate)
				.await
			else {
				return;
			};
			target = Target::Resume(next);
		}
	}

	/// Subscribe upstream for `target`, until the subscription ends: `Some` once nobody
	/// subscribes any more or the track ended, with the copy kept for [`Self::linger`],
	/// `None` once it failed.
	async fn subscribe_once(
		&mut self,
		broadcast_path: &Path,
		track_name: &str,
		mut target: Target,
		group_fetches: &mut TaskSet,
		gate: &mut crate::auth::Gate,
	) -> Option<Idle> {
		let validity = self.validity.start();
		let mut resolved = ResolvedRanges::default();
		let subscription = target
			.subscription()
			.map(|subscription| match target.producer() {
				Some(track) => resolved.resolve(track, &subscription),
				None if needs_resolution(&subscription) => Err(Error::Unsupported),
				None => Ok(subscription),
			})
			.transpose();
		// A relative anchor the copy cannot settle yet counts back from the Largest Object, so
		// the live join that names it goes first and the answer resolves the anchor. No Group ID
		// arithmetic spells it upstream. A time anchor names no group the answer settles.
		let deferred = matches!(subscription, Err(Error::Unsupported))
			&& target
				.subscription()
				.is_some_and(|subscription| !has_time(&subscription));
		let subscription = match subscription {
			Ok(subscription) => subscription,
			Err(_) if deferred => None,
			Err(err) => {
				target.fail(err);
				return None;
			}
		};
		let start = subscription.as_ref().and_then(|s| s.start());
		let join = match subscribe_join(start, subscription.as_ref().and_then(|s| s.end()), self.version) {
			Ok(join) => join,
			Err(err) => {
				target.fail(err);
				return None;
			}
		};
		// A live join delivers nothing below the group SUBSCRIBE_OK names as Largest. Every
		// draft-18 join is one, since its joining FETCH carries that group alone.
		let live = start.is_none() || join.fetch;

		// A listed track's paused subscription is already this track's upstream copy, so
		// resume it rather than open a second one.
		let key: tracks::TrackKey = (broadcast_path.clone(), track_name.into());
		let adopted = match &self.want(&key) {
			None => None,
			Some(handoff) => {
				// The handoff waits for the held stream to flush, which a peer withholding
				// credit can stall, so nobody wanting the track any more ends the wait too.
				enum Abandon {
					Failed(Error),
					Unused,
				}
				loop {
					let adopt = kio::wait(|waiter| {
						if gate.poll_denied(waiter).is_ready() {
							return Poll::Ready(Err(Abandon::Failed(Error::Unauthorized)));
						}
						if let Err(err) = validity.check(waiter) {
							return Poll::Ready(Err(Abandon::Failed(err)));
						}
						if let Poll::Ready(adopted) = Self::poll_handoff(handoff, waiter) {
							return Poll::Ready(Ok(adopted));
						}
						let unwanted = match &mut target {
							Target::Request(request) => crate::util::poll_unwanted(&mut request.demand(), waiter),
							Target::Resume(idle) => crate::util::poll_unwanted(&mut idle.demand, waiter),
						};
						unwanted.map(|_| Err(Abandon::Unused))
					})
					.await;
					match adopt {
						Ok(adopted) => break adopted,
						Err(Abandon::Failed(err)) => {
							self.unwant(key, handoff);
							target.fail(err);
							return None;
						}
						// Nobody wants the track: a resumed copy lingers again, a request is refused.
						Err(Abandon::Unused) => match target {
							Target::Resume(idle) => {
								self.unwant(key, handoff);
								return Some(idle);
							}
							Target::Request(ref request) if request.reject_unwanted(Error::Cancel) => {
								self.unwant(key, handoff);
								return None;
							}
							Target::Request(_) => continue,
						},
					}
				}
			}
		};

		// The resumed subscription's alias, properties, and owed answers, when resuming one.
		let mut held: Option<tracks::Resumed> = None;
		let (request_id, mut stream, mut messages) = match adopted {
			Some(upstream) => {
				self.adopted(&key, upstream.request_id);
				let tracks::Upstream {
					stream,
					messages,
					request_id,
					alias,
					properties,
					owed,
					// This subscription forwards into the copy itself from here.
					copy: _,
				} = upstream;
				held = Some(tracks::Resumed {
					alias,
					properties,
					owed,
					ended: None,
				});
				(request_id, stream, messages)
			}
			None => {
				let opening = self.session.clone();
				let stream = match kio::wait(|waiter| {
					if gate.poll_denied(waiter).is_ready() {
						return Poll::Ready(Err(Error::Unauthorized));
					}
					validity.check(waiter)?;
					Stream::poll_open_request(&opening, self.version, waiter)
				})
				.await
				{
					Ok(stream) => stream.with_stats(self.origin.stats()),
					Err(err) => {
						target.fail(err);
						return None;
					}
				};

				let request_id = self.control.next_request_id();
				let messages = super::request_stream::Messages::new(self.session.clone(), self.control.client());
				(request_id, stream, messages)
			}
		};

		// Register the request before writing SUBSCRIBE so SUBSCRIBE_OK can bind its alias,
		// and so a fill fetch stream that overtakes it finds the subscription it answers.
		let fill = kio::Producer::new(match join.fill.is_some() || join.fetch {
			true => Fill::Requested,
			false => Fill::Done,
		});
		{
			let mut state = self.state.lock();
			// A resumed subscription keeps the alias it is bound to and the data streams it
			// counted while it forwarded before.
			let (alias, tail) = match held.is_some() {
				true => state
					.subscribes
					.remove(&request_id)
					.map(|held| (held.alias, held.tail))
					.unwrap_or_default(),
				false => Default::default(),
			};
			state.subscribes.insert(
				request_id,
				TrackState {
					// A resumed copy is the session's to abort from here on.
					producer: target.producer().cloned(),
					epoch: self.epoch.clone(),
					alias,
					tail,
					..TrackState::pending(track_name.to_owned(), broadcast_path.clone(), fill.clone())
				},
			);
		}

		// Write Subscribe message. The aggregate is read now: a subscriber can join while
		// the request ID and stream were awaited. Updates follow its priority from here, but
		// no draft lets one change the order.
		let max_age_supported = self.peer_setup.get().await.max_age;
		let aggregate = target.subscription();
		let priority = aggregate.as_ref().map(|s| s.priority).unwrap_or(0);
		let max_age =
			max_age_supported.then(|| super::max_age::millis(aggregate.as_ref().map_or(Duration::MAX, |s| s.budget())));
		let order = aggregate.map(|s| s.order).unwrap_or_default();
		// Without an Established subscription, a relay includes the request it holds.
		let new_group = target.new_group();
		// Buffered only: the wait for the answer flushes it, so the local terminals still end a
		// request the peer's send window holds back.
		let written = match &held {
			// Resume the held subscription: its group order is the one it was made with,
			// since no update can change it. Its groups wait on the answer from here.
			Some(held) => {
				self.rebind_alias(request_id, Alias::Resuming(request_id));
				let forward = tracks::Forward {
					forward: true,
					filter: Some(join.filter),
					fill: join.fill,
					priority: Some(priority),
					max_age,
					// Only a track declaring DYNAMIC_GROUPS takes one on an update.
					new_group: new_group.filter(|_| held.properties.dynamic_groups),
				};
				self.buffer_forward(&mut stream.writer, &forward)
			}
			None => stream.writer.buffer_message(
				ietf::Subscribe::ID,
				&ietf::Subscribe {
					max_age,
					new_group,
					request_id,
					track_namespace: broadcast_path.clone(),
					track_name: track_name.into(),
					subscriber_priority: super::priority::to_wire(priority),
					group_order: order.into(),
					filter: join.filter,
					fill: join.fill,
					// A resumed copy already knows the track, from the answer that accepted it.
					properties_wanted: matches!(target, Target::Request(_)),
					forward: true,
					rendezvous: false,
					range_filters: false,
					authorization_token: None,
					epoch: self.epoch.clone(),
				},
			),
		};
		if let Err(err) = written {
			tracing::debug!(%err, "failed to encode subscribe");
			self.remove_subscribe(request_id);
			self.entry_ended(&key, request_id, Err(err.clone()));
			target.fail(err);
			return None;
		}

		tracing::debug!(broadcast = %crate::path::Rooted(self.origin.root(), broadcast_path), track = %track_name, "subscribe started");

		// Park the origin request where a session abort can reject it. The producer
		// does not exist until SUBSCRIBE_OK, and dropping this task would otherwise
		// end the track as `Dropped`.
		let mut resumed = match target {
			Target::Request(request) => {
				let mut state = self.state.lock();
				let Some(held) = state.subscribes.get_mut(&request_id) else {
					request.reject(Error::Cancel);
					return None;
				};
				held.pending = Some(request);
				None
			}
			Target::Resume(idle) => Some(idle),
		};

		// A publisher can be serving before its SUBSCRIBE_OK reaches us, since the data
		// streams are independent of the request stream. Waiting for the response alone would
		// miss the local side going away in that window and leave the publisher serving a
		// track nobody reads, which is the leak this whole path exists to close. The broadcast
		// ending is not a local side going away: a retraction does not disturb subscriptions
		// already in flight, and this one is.
		enum Setup {
			Response(Result<Option<Accepted>, Error>),
			Unused,
			/// `abort` already rejected the parked request.
			Gone,
		}

		// A local terminal ended the wait, so the peer still holds the request to cancel.
		let mut unanswered = false;
		let setup = {
			let mut response = std::pin::pin!(async {
				kio::wait(|waiter| stream.writer.poll_flush(waiter)).await?;
				self.read_answer(&mut stream, &mut messages, held.as_mut()).await
			});
			loop {
				let setup = kio::wait(|waiter| {
					let response = waiter.poll_future(response.as_mut());
					if let Poll::Ready(Err(err)) = response {
						return Poll::Ready(Setup::Response(Err(err)));
					}
					if let Err(err) = validity.check(waiter) {
						unanswered = true;
						return Poll::Ready(Setup::Response(Err(err)));
					}
					if gate.poll_denied(waiter).is_ready() {
						unanswered = true;
						return Poll::Ready(Setup::Response(Err(Error::Unauthorized)));
					}
					// An answer that has already arrived wins over the local terminals. Both can
					// be ready in one poll, and taking abandonment there would discard a response
					// the publisher has already sent: if it was a rejection, the request is gone
					// and cancelling it names a dead id back at a peer entitled to object.
					if let Poll::Ready(res) = response {
						return Poll::Ready(Setup::Response(res));
					}
					let mut state = self.state.lock();
					if let Some(idle) = &mut resumed {
						// A session abort drained the subscription, and the copy with it.
						if !state.subscribes.contains_key(&request_id) {
							return Poll::Ready(Setup::Gone);
						}
						return match crate::util::poll_unwanted(&mut idle.demand, waiter) {
							Poll::Ready(_) => Poll::Ready(Setup::Unused),
							Poll::Pending => Poll::Pending,
						};
					}
					let Some(pending) = state
						.subscribes
						.get_mut(&request_id)
						.and_then(|held| held.pending.as_mut())
					else {
						return Poll::Ready(Setup::Gone);
					};
					if crate::util::poll_unwanted(&mut pending.demand(), waiter).is_ready() {
						return Poll::Ready(Setup::Unused);
					}
					Poll::Pending
				})
				.await;

				match setup {
					Setup::Response(res) => break Some(res),
					Setup::Gone => break None,
					// Nobody holds the resumed copy any more: back to lingering.
					Setup::Unused if resumed.is_some() => break None,
					Setup::Unused => {
						let mut state = self.state.lock();
						let Some(pending) = state
							.subscribes
							.get_mut(&request_id)
							.and_then(|held| held.pending.take())
						else {
							break None;
						};
						if pending.reject_unwanted(Error::Cancel) {
							break None;
						}
						if let Some(held) = state.subscribes.get_mut(&request_id) {
							held.pending = Some(pending);
						}
					}
				}
			}
		};

		let Some(response) = setup else {
			tracing::debug!(
				broadcast = %crate::path::Rooted(self.origin.root(), broadcast_path),
				track = %track_name,
				"subscribe abandoned before it was accepted"
			);
			// A held subscription is still the track's entry: pause it again, owing the
			// answer to the resume as well.
			if let Some(held) = held
				&& self.state.lock().subscribes.contains_key(&request_id)
			{
				let upstream = tracks::Upstream {
					stream,
					messages,
					request_id,
					alias: held.alias,
					properties: held.properties,
					owed: held.owed + 1,
					copy: resumed.as_ref().map(|idle| idle.track.clone()),
				};
				self.pause(key, upstream);
				return resumed;
			}
			// The publisher may already be serving before it answers. A session abort
			// already rejected the parked request; dropping what remains is not a second one.
			let aborted = self.remove_subscribe(request_id).is_none();
			self.cancel_subscribe(stream).await;
			// A resumed copy goes back to lingering, unless the session took it.
			return resumed.filter(|_| !aborted);
		};

		// SUBSCRIBE_OK commits the model's immutable metadata before the alias releases
		// any data stream that arrived ahead of this control response.
		let accepted = match response {
			// A track that needs a property we do not understand, or carries more than we
			// could forward, is neither processed nor forwarded: the subscription is
			// cancelled and the subscribers refused.
			Ok(Some(accepted))
				if refused(&accepted.properties, self.version, self.peer_setup.get().await.flate).is_some() =>
			{
				let err = refused(&accepted.properties, self.version, self.peer_setup.get().await.flate)
					.expect("checked by the guard");
				tracing::debug!(broadcast = %crate::path::Rooted(self.origin.root(), broadcast_path), track = %track_name, %err, "subscribe refused");
				// Refuse before waiting on the peer to acknowledge the cancellation.
				if let Some(pending) = self.take_pending(request_id) {
					pending.reject(err.clone());
				}
				self.remove_subscribe(request_id);
				self.entry_ended(&key, request_id, Err(err.clone()));
				if let Some(idle) = resumed {
					let _ = idle.track.abort(err);
				}
				self.cancel_subscribe(stream).await;
				return None;
			}
			Ok(Some(accepted)) => accepted,
			Ok(None) => {
				if let Some(pending) = self.take_pending(request_id) {
					pending.reject(Error::UnexpectedMessage);
				}
				self.remove_subscribe(request_id);
				self.entry_ended(&key, request_id, Err(Error::UnexpectedMessage));
				if let Some(idle) = resumed {
					let _ = idle.track.abort(Error::UnexpectedMessage);
				}
				return None;
			}
			Err(err) => {
				tracing::debug!(%err, "subscribe response error");
				// Refuse before waiting on the peer to acknowledge the cancellation.
				if let Some(pending) = self.take_pending(request_id) {
					pending.reject(err.clone());
				}
				self.remove_subscribe(request_id);
				// A held subscription that the publisher ended before answering ends its
				// entry as the PUBLISH_DONE says; one whose resume failed fails it, and is
				// cancelled since its PUBLISH_DONE has yet to come.
				let ended = held.as_mut().and_then(|held| held.ended.take());
				let cancel = unanswered || (held.is_some() && ended.is_none());
				let finished = matches!(ended, Some(Ok(())));
				self.entry_ended(&key, request_id, ended.unwrap_or_else(|| Err(err.clone())));
				// A resumed copy whose track finished before the resume was answered cannot
				// place the end, so it lingers again and its reader's SUBSCRIBE learns it.
				if finished && resumed.is_some() {
					return resumed;
				}
				if let Some(idle) = resumed {
					let _ = idle.track.abort(err);
				}
				if cancel {
					self.cancel_subscribe(stream).await;
				}
				return None;
			}
		};
		let Accepted {
			alias,
			properties,
			largest,
		} = accepted;
		// What a pause holds again: the properties the track was listed with.
		let held_properties = properties.clone();
		// A resumed copy opts out of the properties from draft-22, so it keeps the units it
		// learned. Before that the answer declares them again.
		let timescale = properties
			.timescale
			.or(resumed.as_ref().and_then(|idle| idle.timescale));
		let info = accepted_info(
			&ietf::Properties {
				timescale,
				..properties
			},
			self.version,
		);
		// The copy already holds the track: it is current again up to the answer's Largest
		// Location once the join's head lands or a newer group does, since leaving cut the
		// group it was in.
		let mut resuming = resumed.is_some().then(|| {
			largest.map(|largest| track::Position {
				group: largest.group,
				frame: largest.object,
			})
		});
		// A fresh copy is staged until the answer is written in below, so no reader sees it
		// before its Largest and start.
		let mut staged = None;
		let (mut track, dynamic) = match resumed {
			Some(idle) => {
				if !self.state.lock().subscribes.contains_key(&request_id) {
					// Aborted with the session while the answer was in hand.
					return None;
				}
				(idle.track, idle.dynamic)
			}
			None => {
				let Some(request) = self.take_pending(request_id) else {
					// Aborted while the answer was in hand. The parked request is already rejected.
					self.remove_subscribe(request_id);
					return None;
				};
				// Pending until the answer is written in below, so a relay serving the copy
				// never reads the requested floor as where the feed starts.
				let request = request.request_start(start);
				// Serves cache misses with a group FETCH. Registered before accepting, so a
				// miss queued meanwhile waits for it rather than failing for want of a handler.
				let dynamic = request.requests();
				let copy = request.stage(info);
				let track = track::Producer::clone(&copy);
				staged = Some(copy);
				(track, dynamic)
			}
		};
		track.received();
		track.anchored_cached();
		// Object IDs can start past zero or skip, while readers see frame ordinals, so only
		// the group carries over: its first frame is the edge a reader can count on.
		if let Some(largest) = largest {
			track.report_object(largest.group, largest.object);
			// A live join's Largest is where its feed is, which a relative anchor counts back
			// from even when a newer group lands before the joined one.
			if live {
				track.report_edge(largest.group);
			}
		}
		// A live join starts at the publisher's edge; an absolute one where it asked. With
		// no Largest the join skips nothing, so every group is owed: a lower one's stream
		// may still arrive after a higher one's. This resolves the pending start.
		let _ = match live {
			true => track.start_at(largest.map_or(0, |largest| largest.group)),
			false => track.start_at(start),
		};
		if let Some(staged) = staged {
			staged.commit();
		}
		// Resolved while the answer's Largest is still the live edge: no data stream reaches
		// the copy before its alias is bound below.
		let settled = match deferred {
			true => track
				.subscription()
				.map(|subscription| (settle_bounds(&mut resolved, &track, &subscription), subscription)),
			false => None,
		};
		let mut fetching: Option<JoiningTask> = None;
		{
			let mut state = self.state.lock();
			if let Some(held) = state.subscribes.get_mut(&request_id) {
				held.producer = Some(track.clone());
				held.timescale = timescale;
				held.fill_ok(largest);
			}
		}
		self.entry_info(&key, request_id, track.info());
		// A resumed subscription stays bound to the alias it was held with.
		if held.is_some() {
			self.rebind_alias(request_id, Alias::Active(request_id));
		} else if let Err(err) = self.register_alias(request_id, alias) {
			if matches!(err, Error::Duplicate) {
				tracing::warn!(track_alias = %alias, "publisher reused a live track alias for another track");
				self.session.close(
					super::error::to_session_code(&SessionError::from(&err)),
					err.to_string().as_ref(),
				);
			} else {
				tracing::warn!(track_alias = %alias, %err, "could not bind track alias");
				self.cancel_subscribe(stream).await;
			}
			self.remove_subscribe(request_id);
			let _ = track.abort(err);
			return None;
		}
		if join.fetch && largest.is_some() {
			fetching = self.start_joining_fetch(request_id, &track);
		}

		// One event ends the subscription: the last subscriber leaving, or the
		// publisher's PUBLISH_DONE. The broadcast ending does not: a retraction
		// does not disturb subscriptions already in flight.
		enum End {
			Idle,
			Revoked,
			Done(Result<ietf::PublishDone<'static>, Error>),
			Fill(track::Fill),
			Update(RequestId, Update),
			Invalid(Error),
			/// An update failed to go out.
			Failed(Error),
		}

		// The NEW_GROUP_REQUEST last sent, the SUBSCRIBE's included. It stays outstanding
		// until the Largest Group reaches it, and joins meanwhile ask for the same group.
		let mut requested = new_group;
		let dynamic_groups = track.info().refresh.is_some();
		// The group the track's consumers want, `None` once nobody does.
		let mut wanted = None;
		// The subscriber priority upstream holds, and the one the aggregate now asks for.
		let mut sent_max_age = max_age;
		let mut wanted_max_age = max_age;
		let mut sent_priority = priority;
		let mut priority = priority;
		let mut sent_bounds = (start, subscription.as_ref().and_then(|subscription| subscription.end()));
		let mut wanted_bounds = Ok(sent_bounds);
		// A deferred relative anchor, until the copy learns the live edge it counts back from:
		// an answer with no Largest leaves it to the first group. The bounds it then resolves
		// to go upstream as an update, absolute like any other.
		let mut unsettled = None;
		match settled {
			Some((Ok(Some(bounds)), _)) => wanted_bounds = Ok(bounds),
			Some((Ok(None), subscription)) => unsettled = Some(subscription),
			Some((Err(err), _)) => wanted_bounds = Err(err),
			None => {}
		}
		let mut edge = track.consume();
		// REQUEST_UPDATEs sent and not yet answered.
		let unanswered = AtomicU64::new(0);

		let mut fetch_done = fetching.is_none();
		// Our grant stopped covering the track: it is aborted rather than kept lingering.
		let mut failed = false;
		// How the upstream subscription ended, which ends a listing entry it held.
		let mut ended: Option<Result<(), Error>> = None;
		// The publisher ended the track cleanly: the copy lingers for its history's fills.
		let mut finished = false;
		let mut demand = track.demand();
		// Nobody subscribing at all (only fetches asked) needs no subscription.
		let mut subscribed = track
			.subscription()
			.is_some_and(|subscription| !subscription.ranges.is_empty());
		let idle = {
			let updating_track = track.clone();
			let mut done = std::pin::pin!(Self::read_publish_done(
				&mut stream.reader,
				self.version,
				&unanswered,
				&mut messages,
				|ok| {
					if let Some(largest) = ok.largest {
						updating_track.report_object(largest.group, largest.object);
					}
					let mut state = self.state.lock();
					if let Some(held) = state.subscribes.get_mut(&request_id) {
						held.fill_ok(ok.largest);
					}
				}
			));
			loop {
				let end = kio::wait(|waiter| {
					if gate.poll_denied(waiter).is_ready() {
						return Poll::Ready(End::Revoked);
					}
					// An update the peer's send window holds back goes out from here, so it never
					// holds back the ends below.
					if let Poll::Ready(Err(err)) = stream.writer.poll_flush(waiter) {
						return Poll::Ready(End::Failed(err));
					}
					if !fetch_done
						&& let Some(fut) = fetching.as_mut()
						&& let Poll::Ready(res) = fut.poll(waiter)
					{
						fetch_done = true;
						if let Err(err) = res {
							tracing::debug!(%err, "joining FETCH failed, joining at the next whole group");
						}
					}
					// An error is the track closing, which the arms below report.
					if let Poll::Ready(Ok(requested)) = dynamic.poll_requested_fill(waiter) {
						return Poll::Ready(End::Fill(requested));
					}
					let _ = group_fetches.poll(waiter);
					// A group past the answer's Largest also makes the copy current, so a join
					// whose head never arrives cannot hold every later group back.
					if let Some(largest) = resuming
						&& (poll_headed(&fill, waiter).is_ready()
							|| largest.is_some_and(|largest| track.poll_past(largest.group, waiter).is_ready()))
					{
						track.set_live(largest, None);
						resuming = None;
					}
					// The last subscriber left: the upstream subscription goes with it, as on
					// lite, and the copy lingers for fetches and a returning subscriber.
					while let Poll::Ready(Ok(subscription)) = track.poll_subscription_changed(waiter) {
						subscribed = subscription
							.as_ref()
							.is_some_and(|subscription| !subscription.ranges.is_empty());
						unsettled = None;
						if let Some(subscription) = subscription {
							priority = subscription.priority;
							match settle_bounds(&mut resolved, &track, &subscription) {
								Ok(Some(bounds)) => wanted_bounds = Ok(bounds),
								Ok(None) => unsettled = Some(subscription.clone()),
								Err(err) => wanted_bounds = Err(err),
							}
							if max_age_supported {
								wanted_max_age = Some(super::max_age::millis(subscription.budget()));
							}
						}
					}
					if let Some(subscription) = &unsettled {
						while let Poll::Ready(Some(_)) = edge.poll_largest(waiter) {}
						match settle_bounds(&mut resolved, &track, subscription) {
							Ok(Some(bounds)) => {
								wanted_bounds = Ok(bounds);
								unsettled = None;
							}
							Ok(None) => {}
							Err(err) => return Poll::Ready(End::Invalid(err)),
						}
					}
					loop {
						match demand.poll_wanted_changed(waiter) {
							Poll::Ready(Ok(next)) => wanted = next,
							Poll::Ready(Err(_)) => wanted = None,
							Poll::Pending => break,
						}
						if wanted.is_none() {
							break;
						}
					}
					if !subscribed || wanted.is_none() {
						return Poll::Ready(End::Idle);
					}
					if let Poll::Ready(res) = waiter.poll_future(done.as_mut()) {
						return Poll::Ready(End::Done(res));
					}
					// One REQUEST_UPDATE in flight at a time, which any MAX_REQUEST_UPDATES
					// allows. Changes arriving meanwhile coalesce into the aggregate, read
					// once its answer lands. Polled after the answers are read for that reason.
					if unanswered.load(Ordering::Relaxed) > 0 {
						return Poll::Pending;
					}
					let new_group = wanted.filter(|&value| {
						dynamic_groups
							&& largest_group(&track, largest).is_some_and(|largest| value > largest)
							&& requested.is_none_or(|requested| value > requested)
					});
					let bounds = match &wanted_bounds {
						Ok(bounds) => *bounds,
						Err(err) => return Poll::Ready(End::Invalid(err.clone())),
					};
					let widened = match (bounds.0, sent_bounds.0) {
						(None, Some(_)) => true,
						(Some(start), Some(prior)) => start < prior,
						_ => false,
					} || match (bounds.1, sent_bounds.1) {
						(None, Some(_)) => true,
						(Some(end), Some(prior)) => end > prior,
						_ => false,
					};
					let ranges = if bounds != sent_bounds {
						let mut join = if Filter::is_draft20(self.version) {
							match subscribe_join(bounds.0, bounds.1, self.version) {
								Ok(join) => join,
								Err(err) => return Poll::Ready(End::Invalid(err)),
							}
						} else {
							Join {
								filter: match bounds {
									(None | Some(0), None) => Filter::Unfiltered,
									(start, end) => Filter::Absolute {
										start: ietf::Location {
											group: start.unwrap_or(0),
											object: 0,
										},
										end: end
											.and_then(|end| end.checked_sub(1))
											.map(|group| ietf::EndLocation { group, object: None }),
									},
								},
								fill: None,
								fetch: false,
							}
						};
						if widened && Filter::is_draft20(self.version) {
							join.fill = Some(ietf::Fill {
								filter: Some(match bounds.0 {
									None => Filter::Relative(1),
									Some(_) => join.filter,
								}),
								group_order: Some(GroupOrder::Ascending),
								range_filters: false,
							});
						} else {
							join.fill = None;
						}
						Some(join)
					} else {
						None
					};
					let mut update = Update {
						filter: ranges.as_ref().map(|join| join.filter),
						fill: ranges.and_then(|join| join.fill),
						priority: (priority != sent_priority).then_some(priority),
						max_age: wanted_max_age.filter(|_| wanted_max_age != sent_max_age),
						new_group,
					};
					let mut state = self.state.lock();
					let Some(held) = state.subscribes.get_mut(&request_id) else {
						return Poll::Ready(End::Idle);
					};
					// Reserve the new fill while holding the old slot. A stream that already
					// owns a head must hand it off, but a reset may have discarded a pending
					// fill's header, leaving nothing that could settle that request.
					let prior_fill = held.fill.clone();
					let filling = if update.fill.is_some() {
						match prior_fill.poll(waiter, |fill| match **fill {
							Fill::Active | Fill::Ready { .. } => Poll::Pending,
							_ => Poll::Ready(()),
						}) {
							Poll::Ready(Ok(fill)) => Some(fill),
							Poll::Ready(Err(_)) => return Poll::Ready(End::Invalid(Error::Cancel)),
							Poll::Pending => {
								update.filter = None;
								update.fill = None;
								None
							}
						}
					} else {
						None
					};
					if update.priority.is_none()
						&& update.max_age.is_none()
						&& update.new_group.is_none()
						&& update.filter.is_none()
					{
						return Poll::Pending;
					}
					let update_id = self.control.next_request_id();
					if let Some(mut filling) = filling {
						// A handler may already have the old header and be waiting on its
						// answer. It must keep the old state, never claim the new request.
						if let Fill::Failed { sequence } = *filling {
							held.headless_through = held.headless_through.max(Some(sequence));
						}
						filling.cancel();
						drop(filling);
						held.fill = kio::Producer::new(Fill::Requested);
						held.fill_start = match update.fill.and_then(|fill| fill.filter.or(update.filter)) {
							Some(Filter::Absolute { start, .. }) => Some(start),
							_ => None,
						};
						let previous = held.fetch_id.replace(update_id);
						if let Some(previous) = previous {
							state.fetches.remove(&previous);
						}
						state.fetches.insert(update_id, FillRequest::Update(request_id));
					}
					Poll::Ready(End::Update(update_id, update))
				})
				.await;

				match end {
					End::Invalid(err) => {
						ended = Some(Err(err.clone()));
						let _ = track.clone().abort(err);
						break true;
					}
					End::Update(update_id, update) => {
						let changed_range = update.filter.is_some();
						if changed_range {
							sent_bounds = wanted_bounds.clone().expect("validated update bounds");
						}
						let (asked, new_group, asked_age) = (update.priority, update.new_group, update.max_age);
						if let Err(err) = stream.writer.buffer_message(
							ietf::SubscribeUpdate::ID,
							&ietf::SubscribeUpdate {
								max_age: update.max_age,
								request_id: update_id,
								subscriber_priority: update.priority.map(super::priority::to_wire),
								forward: None,
								filter: update.filter,
								authorization_token: None,
								new_group: update.new_group,
								fill: update.fill,
							},
						) {
							tracing::debug!(%err, "failed to update the subscription");
							ended = Some(Err(err.clone()));
							let _ = track.clone().abort(err);
							break false;
						}
						if changed_range {
							let _ = track.start_at(sent_bounds.0);
						}
						if asked_age.is_some() {
							sent_max_age = asked_age;
						}
						if let Some(asked) = asked {
							sent_priority = asked;
						}
						if let Some(value) = new_group {
							requested = Some(value);
						}
						unanswered.fetch_add(1, Ordering::Relaxed);
					}
					End::Failed(err) => {
						tracing::debug!(%err, "failed to update the subscription");
						ended = Some(Err(err.clone()));
						let _ = track.clone().abort(err);
						break false;
					}
					End::Revoked => {
						tracing::info!(broadcast = %crate::path::Rooted(self.origin.root(), broadcast_path), track = %track_name, "subscription no longer authorized");
						let _ = track.clone().abort(Error::Unauthorized);
						failed = true;
						ended = Some(Err(Error::Unauthorized));
						break true;
					}
					End::Fill(requested) => {
						let fetch = self.clone().serve_fill(
							broadcast_path.clone(),
							track_name.to_owned(),
							requested,
							timescale,
						);
						group_fetches.push(fetch);
					}
					End::Idle => {
						tracing::debug!(broadcast = %crate::path::Rooted(self.origin.root(), broadcast_path), track = %track_name, "subscribe cancelled (idle)");
						break true;
					}
					End::Done(res) => {
						// Only TRACK_ENDED ends a listing entry quietly; a clean end of another
						// kind, such as SUBSCRIPTION_ENDED, leaves the track's fate unknown.
						let end = match &res {
							Ok(done) => tracks::listing_end(done, self.version),
							Err(err) => Err(err.clone()),
						};
						ended = Some(end.clone());
						match res.map(|done| done.stream_count) {
							Ok(count) => {
								tracing::debug!(broadcast = %crate::path::Rooted(self.origin.root(), broadcast_path), track = %track_name, "subscribe complete");
								// The publisher sends PUBLISH_DONE once every data stream it opened
								// is closed, but QUIC does not order them, so some can still be on
								// their way. Wait until Stream Count of their headers arrived and
								// each is read to its end, or a bounded grace for any reset before
								// its header (the draft says to use a timeout). The count is a hint:
								// a published peer sends 0, which waits out the grace.
								let held = self
									.state
									.lock()
									.subscribes
									.get(&request_id)
									.map(|held| (held.tail.consume(), held.fill.clone()));
								if let Some((tail, fill)) = held {
									let mut settle = Settle::new(&self.runtime, tail);
									kio::wait(|waiter| {
										if !fetch_done
											&& let Some(fut) = fetching.as_mut()
											&& let Poll::Ready(res) = fut.poll(waiter)
										{
											fetch_done = true;
											if let Err(err) = res {
												tracing::debug!(%err, "joining FETCH failed, joining at the next whole group");
											}
										}
										poll_settled(&mut settle, waiter, &fill, count)
									})
									.await;
									// The publisher ended without a tail for the head, so the head
									// is the whole group, even one that lands after this.
									if let Ok(mut fill) = fill.write() {
										fill.end();
									}
								}
								// Only TRACK_ENDED ends the track. Any other status ends just this
								// subscription, so the copy fails rather than declare an end the
								// track may run past: a later reader subscribes afresh to the same
								// name, and its groups continue.
								match end {
									// A no-op once an END_OF_TRACK declared the end, whose readers
									// wait for a missing group only as long as the grace.
									Ok(()) => {
										let _ = track.finish();
										finished = true;
									}
									Err(err) => {
										let _ = track.clone().abort(err);
									}
								}
							}
							Err(err) => {
								tracing::debug!(%err, "subscribe ended with error");
								let _ = track.clone().abort(err);
							}
						}
						// The publisher already ended the request, so there is nothing to cancel.
						break false;
					}
				}
			}
		};

		// Release an unallocated join now, or let the session cancel its committed request.
		drop(fetching);

		if let Some(end) = ended {
			self.entry_ended(&key, request_id, end);
		} else if idle && self.is_entry(&key, request_id) && self.state.lock().subscribes.contains_key(&request_id) {
			// A listed track's subscription pauses rather than ends, held again as its entry.
			track.set_idle();
			let upstream = tracks::Upstream {
				stream,
				messages,
				request_id,
				alias,
				properties: held_properties,
				owed: unanswered.load(Ordering::Relaxed),
				copy: Some(track.clone()),
			};
			self.pause(key, upstream);
			return Some(Idle {
				demand: track.demand(),
				track,
				dynamic,
				timescale,
				finished: false,
			});
		}

		// Clean up
		let aborted = self.remove_subscribe(request_id).is_none();

		if !idle {
			// The publisher already ended the request, so a FIN is all we owe it, unless an
			// update its send window held back would go out truncated behind it.
			match stream.writer.is_flushed() {
				true => {
					stream.writer.finish().ok();
				}
				false => stream.writer.abort(&Error::Cancel),
			}
			// A reader may still miss history the publisher holds, such as a group whose
			// fetch was in flight as the track ended, so a finished copy keeps serving fills.
			return (finished && !aborted).then(|| Idle {
				demand: track.demand(),
				track,
				dynamic,
				timescale,
				finished: true,
			});
		}
		// What the copy cached goes stale from here. Marked before the cancel, which waits
		// on the publisher: it stops serving as soon as the cancel lands, and a reader
		// returning in between must not take the cache as the live edge.
		if !aborted && !failed {
			track.set_idle();
		}
		{
			// Refusing the copy wakes its FETCH jobs too. Keep driving their cancels
			// while this stream waits for its FIN, even if the peer never ACKs it.
			let mut cancelling = std::pin::pin!(self.cancel_subscribe(stream));
			kio::wait(|waiter| {
				let _ = group_fetches.poll(waiter);
				waiter.poll_future(cancelling.as_mut())
			})
			.await;
		}
		// A session abort took the copy too, and a revoked grant ended it.
		if aborted || failed {
			return None;
		}
		Some(Idle {
			demand: track.demand(),
			track,
			dynamic,
			timescale,
			finished: false,
		})
	}

	/// Keep a copy nobody subscribes to, cache and all, for fetches and a subscriber
	/// returning soon: `Some` once one does, to subscribe again, `None` once nobody held
	/// it through the linger, or the session ended. A finished copy never subscribes again.
	async fn linger(
		&mut self,
		broadcast_path: &Path,
		track_name: &str,
		mut idle: Idle,
		group_fetches: &mut TaskSet,
		gate: &mut crate::auth::Gate,
	) -> Option<Idle> {
		// Registered so a session abort ends the copy with its error.
		let id = {
			let mut state = self.state.lock();
			state.next_lingering += 1;
			let id = state.next_lingering;
			state.lingering.insert(id, idle.track.clone());
			id
		};
		let mut linger = crate::time::Deadline::new(&self.runtime);
		let mut subscribed = idle
			.track
			.subscription()
			.is_some_and(|subscription| !subscription.ranges.is_empty());
		enum Step {
			Fill(track::Fill),
			Subscribe,
			Expired,
			Revoked,
			Closed,
		}
		let resume = loop {
			let step = kio::wait(|waiter| {
				if idle.track.poll_aborted(waiter).is_ready() {
					return Poll::Ready(Step::Closed);
				}
				if gate.poll_denied(waiter).is_ready() {
					return Poll::Ready(Step::Revoked);
				}
				if let Poll::Ready(Ok(fill)) = idle.dynamic.poll_requested_fill(waiter) {
					return Poll::Ready(Step::Fill(fill));
				}
				let _ = group_fetches.poll(waiter);
				while let Poll::Ready(Ok(subscription)) = idle.track.poll_subscription_changed(waiter) {
					subscribed = subscription
						.as_ref()
						.is_some_and(|subscription| !subscription.ranges.is_empty());
				}
				if subscribed && !idle.finished {
					return Poll::Ready(Step::Subscribe);
				}
				// Nobody holds it: let it go after the linger. A reader waiting on a fetch holds
				// it too. A holder returning restarts the countdown when it next leaves.
				if crate::util::poll_unwanted(&mut idle.demand, waiter).is_ready() {
					if linger.deadline().is_none() {
						linger.set(self.runtime.now().checked_add(track::IDLE_LINGER));
					}
					if linger.poll(waiter).is_ready() {
						return Poll::Ready(Step::Expired);
					}
				} else {
					linger.set(None);
				}
				Poll::Pending
			})
			.await;

			match step {
				Step::Fill(fill) => {
					let fetch =
						self.clone()
							.serve_fill(broadcast_path.clone(), track_name.to_owned(), fill, idle.timescale);
					group_fetches.push(fetch);
				}
				Step::Subscribe => break true,
				Step::Expired => match idle.track.abort_unwanted(Error::Cancel) {
					Ok(()) => {
						tracing::debug!(broadcast = %crate::path::Rooted(self.origin.root(), broadcast_path), track = %track_name, "track released (idle)");
						self.state.lock().lingering.remove(&id);
						return None;
					}
					Err(used) => {
						idle.track = used;
						linger.set(None);
					}
				},
				Step::Revoked => {
					tracing::info!(broadcast = %crate::path::Rooted(self.origin.root(), broadcast_path), track = %track_name, "lingering track no longer authorized");
					let _ = idle.track.clone().abort(Error::Unauthorized);
					self.drain(group_fetches).await;
					break false;
				}
				Step::Closed => break false,
			}
		};
		// The session's abort took it out of the registry already, or this does.
		let registered = self.state.lock().lingering.remove(&id).is_some();
		(resume && registered).then_some(idle)
	}

	/// Read the PUBLISH_DONE that ends an Established subscription, as the end it reports
	/// and, for a clean end, its Stream Count.
	///
	/// The publisher must send it before its FIN (draft-22 section 6.4.2.2), so a FIN
	/// without one is a failed request, not a clean end.
	/// Read the subscription's PUBLISH_DONE, and on the way the answers to the
	/// REQUEST_UPDATEs counted in `unanswered`. Any other message is unexpected.
	async fn read_publish_done(
		reader: &mut Reader<crate::transport::boxed::RecvStream, Version>,
		version: Version,
		unanswered: &AtomicU64,
		messages: &mut super::request_stream::Messages,
		mut acknowledged: impl FnMut(ietf::RequestOk),
	) -> Result<ietf::PublishDone<'static>, Error> {
		// A refused update is followed by PUBLISH_DONE, which ends the subscription. Its
		// count stays, so no further update goes out meanwhile, and no answer is owed.
		let mut refused = false;
		loop {
			let super::request_stream::Framed(type_id, body) = messages.message(reader).await?;
			if type_id == ietf::PublishDone::ID {
				let msg = body.message::<ietf::PublishDone>(version)?.into_owned();
				tracing::debug!(message = ?msg, "received publish done");
				msg.end(version)?;
				return Ok(msg);
			}
			let answer = matches!(type_id, ietf::RequestOk::ID | ietf::RequestError::ID)
				&& !refused && unanswered.load(Ordering::Relaxed) > 0;
			if !answer {
				return Err(Error::UnexpectedMessage);
			}
			let mut data = body.decoder(version);
			match type_id {
				ietf::RequestOk::ID => {
					let msg = ietf::RequestOk::decode_msg(&mut data, version)?;
					tracing::debug!(message = ?msg, "subscription update ok");
					acknowledged(msg);
					unanswered.fetch_sub(1, Ordering::Relaxed);
				}
				_ => {
					let msg = ietf::RequestError::decode_msg(&mut data, version)?;
					tracing::debug!(message = ?msg, "subscription update refused");
					refused = true;
				}
			}
		}
	}

	/// Tell the publisher to stop serving a subscription we are walking away from.
	///
	/// Every path that abandons an Established subscription goes through here, because
	/// staying silent is what leaves the publisher serving a track nobody is reading and
	/// feeding an alias we already retired.
	///
	/// There is no UNSUBSCRIBE message, only the stream itself: a FIN is explicitly not a
	/// cancellation (draft-22 section 6.4.2.2), so section 6.4.2.3's pair applies, an endpoint
	/// that has already FINed its sending direction cancels with STOP_SENDING on the
	/// receiving one.
	async fn cancel_subscribe(&self, stream: Stream<Transport, Version>) {
		let Stream { writer, mut reader } = stream;

		// STOP_SENDING needs no acknowledgement, so it goes first and the wait below covers
		// only what we still have to deliver.
		reader.abort(&Error::Cancel);

		// Finishing alone would leave the writer's Drop free to RESET_STREAM, and a stream
		// that has sent its FIN is still retransmitting: the reset would discard what we
		// already wrote. Closing consumes the writer, removing that fallback, and waits for
		// the acknowledgement. A request the peer's send window held back resets instead.
		if let Err(err) = writer.close().await {
			tracing::debug!(%err, "failed to close the subscribe stream");
		}
	}

	/// Send the joining FETCH that follows a draft-18 SUBSCRIBE, and keep reading its
	/// answer until the subscription ends.
	///
	/// It asks for the head of the group SUBSCRIBE_OK names as Largest, and nothing below
	/// it: an earlier start's groups are the copy's fills, one FETCH per run of them. The
	/// subscribe's request id names the FETCH. The data
	/// arrives on its own fetch stream and [`Self::recv_fill`] stitches it. A refusal or a
	/// failed send settles the fill as [`Fill::Failed`], as a failed draft-22 fill does: only
	/// the head's group is lost, and a whole group still stands alone. A malformed answer
	/// closes the session.
	fn start_joining_fetch(&self, subscribe_id: RequestId, track: &track::Producer) -> Option<JoiningTask> {
		let fill = self.state.lock().subscribes.get(&subscribe_id)?.fill.clone();
		let track = track.clone();
		let state = kio::Shared::<JoiningState>::default();
		let progress = state.clone();
		let mut this = self.clone();
		let validity = self.validity.start();
		let work = async move {
			let res = async {
				// This subscription owns its joining fetch, so wait for stream credit.
				// No ID is spent while opening a stream, so dropping a blocked open
				// cannot leave a gap in the peer's sequential request IDs.
				let opening = this.session.clone();
				let mut stream = kio::wait(|waiter| {
					validity.check(waiter)?;
					if let Poll::Ready(err) = track.poll_aborted(waiter) {
						return Poll::Ready(Err(err));
					}
					Stream::poll_open(&opening, this.version, waiter)
				})
				.await?;
				stream = stream.with_stats(this.origin.stats());
				let fetch_id = this.control.next_request_id();
				progress.lock().committed = true;
				{
					let mut state = this.state.lock();
					if let Some(track) = state.subscribes.get_mut(&subscribe_id) {
						track.fetch_id = Some(fetch_id);
						state.fetches.insert(fetch_id, FillRequest::Joining(subscribe_id));
					}
				}
				// Flushed while waiting for the answer, so a cancellation still ends a request the
				// peer's send window holds back.
				stream.writer.buffer_message(
					ietf::Fetch::ID,
					&ietf::Fetch {
						request_id: fetch_id,
						subscriber_priority: super::priority::to_wire(
							track.subscription().map(|s| s.priority).unwrap_or(0),
						),
						group_order: GroupOrder::Ascending,
						fetch_type: FetchType::RelativeJoining {
							subscriber_request_id: subscribe_id,
							group_offset: 0,
						},
						range_filters: false,
						fill_timeout: false,
						properties_wanted: true,
						// Served by the subscription it joins, which named the instance.
						epoch: None,
					},
				)?;
				this.finish_joining_fetch(stream, progress).await
			}
			.await;
			// A refusal costs only the head, but a malformed answer is the peer breaking the
			// protocol, which closes the session as it would on any request.
			if let Err(err) = &res
				&& is_protocol_violation(err)
			{
				this.session.close(
					super::error::to_session_code(&SessionError::from(err)),
					err.to_string().as_ref(),
				);
			}
			// Release a mid-group tail parked on the head, including one still being written:
			// a request that failed after FETCH_OK cannot be trusted to finish its stream.
			if res.is_err() {
				let largest = this
					.state
					.lock()
					.subscribes
					.get(&subscribe_id)
					.and_then(|track| track.fill_largest);
				if let Ok(mut state) = fill.write()
					&& matches!(*state, Fill::Requested | Fill::Serving(_) | Fill::Active)
				{
					*state = Fill::failed(largest);
				}
			}
			res
		}
		.boxed();
		Some(JoiningTask {
			work: Some(work),
			state,
			tasks: self.tasks.clone(),
		})
	}

	async fn finish_joining_fetch(
		&mut self,
		mut stream: Stream<Transport, Version>,
		progress: kio::Shared<JoiningState>,
	) -> Result<(), Error> {
		let cancelled = |waiter: &kio::Waiter| {
			progress
				.poll(waiter, |state| {
					state.cancelled.then_some(()).map_or(Poll::Pending, Poll::Ready)
				})
				.map(drop)
		};
		let mut messages = super::request_stream::Messages::new(self.session.clone(), self.control.client());
		let res = async {
			let answer = {
				let mut response = std::pin::pin!(async {
					kio::wait(|waiter| stream.writer.poll_flush(waiter)).await?;
					self.read_fetch_response(&mut stream, &mut messages).await
				});
				kio::wait(|waiter| {
					if cancelled(waiter).is_ready() {
						return Poll::Ready(None);
					}
					waiter.poll_future(response.as_mut()).map(Some)
				})
				.await
			};
			match answer {
				Some(answer) => answer?,
				None => return Ok(()),
			};
			// The request stays open while the subscription still needs its fill. From draft 18
			// it may carry a GOAWAY meanwhile; before that, nothing follows the answer here.
			let mut finished = false;
			kio::wait(|waiter| {
				if !finished {
					match messages.poll_end(&mut stream.reader, waiter) {
						Poll::Ready(Ok(())) => finished = true,
						Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
						Poll::Pending => {}
					}
				}
				cancelled(waiter).map(Ok)
			})
			.await
		}
		.await;
		stream.reader.abort(&Error::Cancel);
		let _ = stream.writer.close().await;
		res
	}

	async fn read_subscribe_response(
		&self,
		stream: &mut Stream<Transport, Version>,
		messages: &mut super::request_stream::Messages,
	) -> Result<Option<Accepted>, Error> {
		// Read type_id + size + body from the stream
		let super::request_stream::Framed(type_id, body) = messages.message(&mut stream.reader).await?;
		let mut data = body.decoder(self.version);

		match type_id {
			ietf::SubscribeOk::ID => {
				let msg = ietf::SubscribeOk::decode_msg(&mut data, self.version)?;
				tracing::debug!(message = ?msg, "received subscribe ok");
				Ok(Some(Accepted {
					alias: msg.track_alias,
					properties: msg.properties,
					largest: msg.largest,
				}))
			}
			ietf::RequestError::ID => {
				let msg = ietf::RequestError::decode_msg(&mut data, self.version)?;
				tracing::warn!(message = ?msg, "request error");
				Err(request::from_code(
					msg.error_code,
					request::Kind::Subscribe,
					self.version,
				))
			}
			_ => Err(Error::UnexpectedMessage),
		}
	}

	/// Learn a track nobody subscribes to from TRACK_STATUS, accepting an idle copy that
	/// [`Self::linger`] serves fetches from and subscribes once someone does. `None` once
	/// refused or abandoned: a refused status fails the query-only or fetch-only request.
	async fn track_status(
		&mut self,
		broadcast_path: &Path,
		track_name: &str,
		request: track::Request,
		gate: &mut crate::auth::Gate,
	) -> Option<Idle> {
		let validity = self.validity.start();
		let opening = self.session.clone();
		let mut stream = match kio::wait(|waiter| {
			if gate.poll_denied(waiter).is_ready() {
				return Poll::Ready(Err(Error::Unauthorized));
			}
			validity.check(waiter)?;
			Stream::poll_open_request(&opening, self.version, waiter)
		})
		.await
		{
			Ok(stream) => stream,
			Err(err) => {
				request.reject(err);
				return None;
			}
		};
		stream = stream.with_stats(self.origin.stats());
		let request_id = self.control.next_request_id();

		// Flushed while waiting for the answer, so the terminals below still end a request
		// the peer's send window holds back.
		let written = stream.writer.buffer_message(
			ietf::TrackStatus::ID,
			&ietf::TrackStatus {
				request_id,
				track_namespace: broadcast_path.clone(),
				track_name: track_name.into(),
				properties_wanted: true,
				epoch: self.epoch.clone(),
			},
		);
		if let Err(err) = written {
			request.reject(err);
			return None;
		}

		// Nobody wanting the track any more ends the wait on a peer that may never answer,
		// and the session ending fails it with the session's reason.
		let mut demand = request.demand();
		let closed = self.session.clone();
		let response = {
			let mut response = std::pin::pin!(async {
				kio::wait(|waiter| stream.writer.poll_flush(waiter)).await?;
				self.read_track_status_response(&mut stream).await
			});
			loop {
				let res = kio::wait(|waiter| {
					let response = waiter.poll_future(response.as_mut());
					if let Poll::Ready(Err(err)) = response {
						return Poll::Ready(Some(SetupResponse::Response(Err(err))));
					}
					if let Err(err) = validity.check(waiter) {
						return Poll::Ready(Some(SetupResponse::Cancelled(err)));
					}
					if gate.poll_denied(waiter).is_ready() {
						return Poll::Ready(Some(SetupResponse::Cancelled(Error::Unauthorized)));
					}
					if let Poll::Ready(res) = response {
						return Poll::Ready(Some(SetupResponse::Response(res)));
					}
					if let Poll::Ready(err) = closed.poll_closed(waiter) {
						return Poll::Ready(Some(SetupResponse::Cancelled(Error::from_transport(err))));
					}
					crate::util::poll_unwanted(&mut demand, waiter).map(|_| None)
				})
				.await;
				match res {
					Some(res) => break Some(res),
					None if request.reject_unwanted(Error::Cancel) => break None,
					None => continue,
				}
			}
		};
		let response = match response {
			Some(SetupResponse::Response(response)) => response,
			Some(SetupResponse::Cancelled(err)) => {
				request.reject(err);
				stream.reader.abort(&Error::Cancel);
				let _ = stream.writer.close().await;
				return None;
			}
			None => {
				stream.reader.abort(&Error::Cancel);
				stream.writer.abort(&Error::Cancel);
				return None;
			}
		};
		let _ = stream.writer.finish();
		let ok = match response {
			// A track that needs a property we do not understand is not forwarded.
			Ok(ok) => match refused(&ok.properties, self.version, self.peer_setup.get().await.flate) {
				Some(err) => Err(err),
				None => Ok(ok),
			},
			res => res,
		};
		let ok = match ok {
			Ok(ok) => ok,
			Err(err) => {
				tracing::debug!(%err, broadcast = %crate::path::Rooted(self.origin.root(), broadcast_path), track = %track_name, "track status refused");
				request.reject(err);
				return None;
			}
		};

		tracing::debug!(broadcast = %crate::path::Rooted(self.origin.root(), broadcast_path), track = %track_name, "track status known");
		let dynamic = request.requests();
		// Nothing feeds this copy live: what it caches is fetched, never the live edge. Idle
		// from the accept, so no reader takes it as live in between.
		let track = request.idle().stage(accepted_info(&ok.properties, self.version));
		// Marked received before any reader sees it, so none resolves an anchor this wire dropped.
		track.received();
		track.anchored_cached();
		if let Some(largest) = ok.largest {
			track.report_object(largest.group, largest.object);
		}
		let track = track.commit();
		Some(Idle {
			demand: track.demand(),
			track,
			dynamic,
			timescale: ok.properties.timescale,
			finished: false,
		})
	}

	/// Read the answer to a TRACK_STATUS: TRACK_STATUS_OK, or the publisher's refusal as an
	/// error.
	async fn read_track_status_response(
		&self,
		stream: &mut Stream<Transport, Version>,
	) -> Result<ietf::TrackStatusOk, Error> {
		let mut messages = super::request_stream::Messages::new(self.session.clone(), self.control.client());
		let super::request_stream::Framed(type_id, body) = messages.message(&mut stream.reader).await?;
		let mut data = body.decoder(self.version);

		let code = match type_id {
			id if id == ietf::TrackStatusOk::ID => {
				return Ok(ietf::TrackStatusOk::decode_msg(&mut data, self.version)?);
			}
			ietf::RequestError::ID if true => ietf::RequestError::decode_msg(&mut data, self.version)?.error_code,
			_ => return Err(Error::UnexpectedMessage),
		};
		Err(request::from_code(code, request::Kind::TrackStatus, self.version))
	}

	pub async fn recv_group(
		&mut self,
		stream: &mut Reader<crate::transport::boxed::RecvStream, Version>,
	) -> Result<(), Error> {
		let group: ietf::GroupHeader = stream.decode().await?;
		// Ranked as the least urgent subscription until its alias resolves.
		stream.set_priority(crate::priority::recv_group(i8::MIN, group.group_id, Default::default()));

		if group.sub_group_id != 0 {
			tracing::warn!(sub_group_id = %group.sub_group_id, "subgroup ID is not supported, dropping stream");
			return Err(Error::Unsupported);
		}

		// SUBSCRIBE_OK or PUBLISH can be reordered behind this stream. Hold only the
		// subgroup header while waiting so the data stream cannot consume flow control.
		let aliases = self.state.lock().aliases.consume();
		let request_id = match resolve_track_alias(&self.runtime, aliases, group.track_alias).await {
			Ok(request_id) => request_id,
			// Ours: we cancelled the subscription and the publisher has not stopped yet.
			Err(err @ Error::Cancel) => {
				tracing::debug!(track_alias = %group.track_alias, "dropping group for a cancelled subscription");
				return Err(err);
			}
			// Theirs: nothing ever bound this alias. Either the publisher sent data for a
			// track it never acknowledged, or SUBSCRIBE_OK is more than a timeout behind.
			Err(err) => {
				tracing::warn!(
					track_alias = %group.track_alias,
					timeout = ?TRACK_ALIAS_TIMEOUT,
					"unknown track alias: no SUBSCRIBE_OK bound it"
				);
				return Err(err);
			}
		};

		let (mut track, timescale, fill, headless, mut reading) = {
			let state = self.state.lock();
			let track = state.subscribes.get(&request_id).ok_or(Error::NotFound)?;
			(
				track.producer.clone().ok_or(Error::NotFound)?,
				track.timescale,
				track.fill.clone(),
				track.headless_through.is_some_and(|through| group.group_id <= through),
				// Every data stream counts toward PUBLISH_DONE's Stream Count, even one
				// dropped below, and the subscription's end waits until it is read.
				Reading::open(&track.tail, Some(group.group_id), self.runtime.now()),
			)
		};
		let (priority, order) = track
			.subscription()
			.map_or((0, Default::default()), |sub| (sub.priority, sub.order));
		stream.set_priority(crate::priority::recv_group(priority, group.group_id, order));

		// Peek before creating a group: a standalone END_OF_TRACK has no payload
		// group, and a fill may already own the prefix.
		//
		// The peek inside blocks until the publisher produces the group's first object, so
		// race it against the subscription going away the same way the group read below is.
		// Otherwise dropping the local subscriber cannot end this handler.
		let passthrough = !fill.read().outstanding();
		let opened = {
			let mut opening = track.clone();
			let mut open = std::pin::pin!(crate::util::sendable(self.open_group(
				stream,
				&mut opening,
				&fill,
				headless,
				&group,
				&mut reading
			)));
			kio::wait(|waiter| {
				if let Poll::Ready(err) = track.poll_aborted(waiter) {
					return Poll::Ready(Err(err));
				}
				waiter.poll_future(open.as_mut())
			})
			.await
		};
		let opened = match opened {
			// The group is at or past the end the publisher declared, which no later stream
			// can repair.
			Err(Error::Closed) => {
				tracing::warn!(group = group.group_id, "group past the declared end of track");
				let _ = track.abort(Error::ProtocolViolation);
				return Err(Error::ProtocolViolation);
			}
			Err(err) => return Err(err),
			Ok(opened) => opened,
		};
		let (producer, start) = match opened {
			Opened::Group(producer, start) => (producer, start),
			// A standalone terminal carries a Location, not a payload group.
			Opened::EndOfTrack { object, properties } => {
				return end_track(
					&mut track,
					track::Position {
						group: group.group_id,
						frame: object,
					},
					properties,
				);
			}
		};

		if passthrough || producer.wire().is_none() {
			let first_id = start.max(producer.frame_count() as u64);
			producer.set_wire(group::Wire {
				sparse: false,
				first_object: group.flags.first_object && first_id == start,
				first_id,
				extensions: group.flags.has_extensions,
				has_end: group.flags.has_end,
				fetchable: true,
				last: None,
			})?;
		}

		// Guarded: this handler can be dropped at any await below, and a group producer
		// that dies without a terminal leaves its consumer waiting on nothing.
		let mut producer = crate::recv::Group::new(producer);
		let observing = producer.handle().clone();

		let mut ingest = GroupIngest::new(self.version, &group, timescale);
		ingest.expected_first = Some(start);
		let res = {
			kio::wait(|waiter| {
				if let Poll::Ready(err) = track.poll_aborted(waiter) {
					return Poll::Ready(Err(err));
				}
				if let Poll::Ready(err) = observing.poll_aborted(waiter) {
					return Poll::Ready(Err(err));
				}
				ingest.poll(stream, &mut producer, waiter)
			})
			.await
		};

		match res {
			Err(err @ (Error::Cancel | Error::Stream(crate::StreamError::Cancel))) => {
				let _ = producer.abort(err);
			}
			// The peer's fault: a group we cannot carry, or one it sent malformed.
			Err(
				err @ (Error::Unsupported
				| Error::Session(SessionError::ProtocolViolation)
				| Error::Decode(_)
				| Error::WrongSize
				| Error::FrameTooLarge
				| Error::GroupTooLarge),
			) => {
				tracing::warn!(%err, group = %producer.sequence, "refusing group");
				let _ = producer.abort(err.clone());
				// Return the refusal to the dispatcher so it sends STOP_SENDING.
				return Err(err);
			}
			// The track is malformed, not just this group, so no later group is trusted.
			Err(Error::MalformedTrack) => {
				tracing::warn!(group = %producer.sequence, "malformed track");
				let _ = producer.abort(Error::MalformedTrack);
				let _ = track.abort(Error::MalformedTrack);
				return Err(Error::MalformedTrack);
			}
			// The peer reset the stream, the transport failed, or the track went away.
			Err(err) => {
				tracing::debug!(%err, group = %producer.sequence, "group error");
				let _ = producer.abort(err);
			}
			Ok(Ended::Group) => {
				let _ = producer.finish();
			}
			// No object past this group's last one exists, so the track ends after it.
			Ok(Ended::Track { object, properties }) => {
				let _ = producer.finish();
				return end_track(
					&mut track,
					track::Position {
						group: group.group_id,
						frame: object,
					},
					properties,
				);
			}
		}

		Ok(())
	}

	/// Deliver an OBJECT_DATAGRAM with private relay metadata on its subscription.
	///
	/// A malformed datagram is a protocol error. An unbound alias is dropped, as the
	/// draft permits instead of buffering; public readers receive only Normal payloads.
	pub fn recv_datagram(&self, payload: bytes::Bytes) -> Result<(), Error> {
		let (datagram, _) = ietf::ObjectDatagram::decode_slice(&payload, self.version)?;
		let (alias, sequence) = (datagram.track_alias, datagram.group_id);

		let (payload, status) = match datagram.body {
			ietf::DatagramBody::Payload(payload) => (payload, None),
			ietf::DatagramBody::Status(status) if super::object::status_defined(status) => {
				(bytes::Bytes::new(), Some(status))
			}
			ietf::DatagramBody::Status(_) => return Err(Error::Session(SessionError::ProtocolViolation)),
		};

		let mut state = self.state.lock();
		let request_id = match state.aliases.read().map.get(&alias) {
			Some(Alias::Active(request_id)) => *request_id,
			_ => {
				tracing::debug!(alias, sequence, "dropping a datagram for an unbound alias");
				return Ok(());
			}
		};
		let Some(track) = state.subscribes.get_mut(&request_id) else {
			return Ok(());
		};

		// Like a subgroup object: a track that declared no timescale is untimed. One on a
		// timed track without a Timestamp is refused by the model.
		let properties = bytes::Bytes::from(datagram.properties.unwrap_or_default());
		// A status object has no frame to hold its time, so its block keeps it.
		let (timestamp, properties, inflated) = match status.is_some_and(|status| status != 0) {
			true => (
				None,
				super::object::unframed(&properties, self.version, track.timescale)?,
				None,
			),
			false => {
				let decoded = super::object::read(&properties, self.version, track.timescale)?;
				super::object::check_frame(&decoded.properties, track.timescale)?;
				(decoded.timestamp, decoded.properties, decoded.inflated)
			}
		};
		let Some(producer) = track.producer.as_mut() else {
			return Ok(());
		};
		let inflated = match status.is_some_and(|s| s != 0) {
			true => None,
			false => super::object::inflated(inflated, producer.info().compress)?,
		};
		let stored = crate::model::datagram::Stored {
			datagram: crate::Datagram {
				sequence,
				timestamp,
				payload,
			},
			wire: Some(std::sync::Arc::new(crate::model::datagram::Wire {
				object_id: datagram.object_id,
				priority: datagram.publisher_priority,
				end_of_group: datagram.end_of_group,
				status,
				properties,
			})),
			deflated: None,
		};
		match inflated {
			Some(size) => producer.insert_deflated_datagram(stored, size)?,
			None => producer.insert_wire_datagram(stored)?,
		}
		Ok(())
	}
}

/// Mark where the track ends, as an END_OF_TRACK object said.
///
/// PUBLISH_DONE carries no end location, so this is what lets a subscriber
/// learn the end before the live edge reaches it. A boundary at or below a group already
/// received is the publisher breaking its own end, which no later group can repair. A
/// marker that lands after the subscription already ended (its grace expired) changes
/// nothing. A marker carrying properties has nowhere to keep them, since the end is
/// track-level, so the track is refused rather than lose them.
fn end_track(track: &mut track::Producer, end: track::Position, properties: bool) -> Result<(), Error> {
	if properties {
		tracing::warn!(?end, "END_OF_TRACK properties are unsupported");
		let _ = track.clone().abort(Error::Unsupported);
		return Err(Error::Unsupported);
	}
	// Lower groups may still be on the wire, behind the one that carried the end: the
	// received copy holds readers at a hole for the grace.
	if let Err(err) = track.finish_at(end, None) {
		tracing::warn!(%err, ?end, "invalid END_OF_TRACK");
		let _ = track.clone().abort(Error::ProtocolViolation);
		return Err(Error::ProtocolViolation);
	}
	Ok(())
}

/// How [`Subscriber::open_group`] resolved a subgroup stream.
enum Opened {
	/// The group producer the stream writes into, and the Object ID it starts at.
	Group(group::Producer, u64),
	/// A standalone terminal marker, which creates no payload group, and whether it
	/// carried properties.
	EndOfTrack { object: u64, properties: bool },
}

/// How a subgroup stream ended cleanly.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Ended {
	/// The stream finished, or carried an explicit end of group.
	Group,
	/// It carried an END_OF_TRACK after the group's last object, and whether that
	/// marker carried properties.
	Track { object: u64, properties: bool },
}

use super::object::{END_OF_GROUP, END_OF_TRACK};

/// The start of a subgroup stream's first object, peeked before its group is created.
#[derive(Debug, Clone, Copy)]
struct FirstObject {
	/// The Object ID, which the first object's delta is.
	id: u64,
	end_of_track: bool,
	has_properties: bool,
}

/// [`FirstObject`] for a stream whose objects carry extensions, or don't.
#[derive(Debug)]
struct PeekFirst<const EXTENSIONS: bool>(FirstObject);

/// The first Object ID alone, which names a compact stream's subgroup.
#[derive(Debug)]
struct PeekId(u64);

impl<'a> Decode<'a, Version> for PeekId {
	fn decode(buf: &mut Decoder<'a>, _: Version) -> Result<Self, DecodeError> {
		Ok(Self(buf.varint()?))
	}
}

impl<'a, const EXTENSIONS: bool> Decode<'a, Version> for PeekFirst<EXTENSIONS> {
	fn decode(buf: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError> {
		let id = buf.varint()?;
		let has_properties = if EXTENSIONS {
			let ObjectExtensionsLength(size) = ObjectExtensionsLength::decode(buf, version)?;
			buf.slice(size)?;
			size != 0
		} else {
			false
		};
		let size = buf.varint()?;
		let end_of_track = size == 0 && buf.varint()? == END_OF_TRACK;
		Ok(Self(FirstObject {
			id,
			end_of_track,
			has_properties,
		}))
	}
}

impl Subscriber {
	/// The group producer this subgroup stream writes into, and the Object ID it starts at.
	///
	/// The first object is peeked before any producer exists, since a standalone
	/// END_OF_TRACK carries no payload group. Normally the stream then starts the
	/// group. While a fill is outstanding it may instead be the tail of the group the fill
	/// fetch stream began, which the first Object ID decides.
	async fn open_group(
		&self,
		stream: &mut Reader<crate::transport::boxed::RecvStream, Version>,
		track: &mut track::Producer,
		fill: &kio::Producer<Fill>,
		// Whether a fill an update replaced failed to head this group, or a later one.
		headless: bool,
		header: &ietf::GroupHeader,
		reading: &mut Reading,
	) -> Result<Opened, Error> {
		let sequence = header.group_id;
		// Stats (groups/frames/bytes) are counted in the model as the group is written,
		// through the tagged `track::Producer`.
		let create = |track: &mut track::Producer| track.create_group(group::Info { sequence });

		// The compact form names the subgroup by its first Object ID, so check it before the
		// rest of the object can fail and open the group as subgroup 0. A stream with no
		// objects names none, and a failed read fails again in the full peek below.
		if header.flags.has_subgroup_object
			&& let Ok(Some(PeekId(id))) = stream.decode_peek_maybe::<PeekId>().await
			&& id != 0
		{
			tracing::warn!(sub_group_id = id, "subgroup ID is not supported, dropping stream");
			return Err(Error::Unsupported);
		}

		// The subgroup header is enough to hand a promised head to its live tail.
		// Waiting for the next object would hold every later range update on a quiet group.
		let outstanding = fill.read().outstanding();
		if !header.flags.first_object
			&& !header.flags.has_subgroup_object
			&& outstanding
			&& let Some((producer, next)) = self.claim_fill(fill, track, sequence, None, reading).await?
		{
			return Ok(Opened::Group(producer, next));
		}

		let peeked = match header.flags.has_extensions {
			true => stream
				.decode_peek_maybe::<PeekFirst<true>>()
				.await
				.map(|peek| peek.map(|peek| peek.0)),
			false => stream
				.decode_peek_maybe::<PeekFirst<false>>()
				.await
				.map(|peek| peek.map(|peek| peek.0)),
		};
		let first = match peeked {
			Ok(first) => first,
			// The header arrived, so the stream counts toward the track's end. Abort the
			// group it named rather than let the track end clean without it. A fill that
			// already holds the group reports it through its own producer.
			Err(err) => {
				if let Ok(group) = create(track) {
					let _ = group.abort(err.clone());
				}
				return Err(err);
			}
		};
		if let Some(first) = first.filter(|first| first.end_of_track) {
			return Ok(Opened::EndOfTrack {
				object: first.id,
				properties: first.has_properties,
			});
		}

		// Without a fill, a relay preserves the subgroup's first Object ID and FIRST_OBJECT
		// claim independently. Plain readers see only frame ordinals. A head that was owed
		// and never came is the exception: the stream is the same unstitchable tail it is
		// when it arrives before the fill settles, below.
		if !fill.read().outstanding() {
			let start = first.map_or(0, |first| first.id);
			if start != 0 && (headless || fill.read().headless(sequence)) {
				tracing::warn!(sequence, start, "no fill to stitch a mid-group stream onto");
				return Err(Error::Unsupported);
			}
			return Ok(Opened::Group(create(track)?, start));
		}

		// The first object's ID delta is its absolute Object ID (see `next_object_id`).
		match first.map(|first| first.id) {
			// A group delivered from its start stands alone, unless the fill already
			// headed this very sequence: the publisher then served those objects twice,
			// and the model has one producer per group. Refuse both unfinished pieces
			// rather than cache a cut group as complete.
			Some(0) => {
				let headed = matches!(*fill.read(), Fill::Ready { sequence: s, .. } if s == sequence);
				if headed {
					tracing::warn!(sequence, "a whole group arrived for one the fill already headed");
					if let Ok(mut state) = fill.write() {
						state.cancel();
					}
					return Err(Error::Unsupported);
				}

				Ok(Opened::Group(create(track)?, 0))
			}

			// A group starting partway through is the tail of one the fill began, and
			// without that head it has a hole at the front.
			Some(start) => match self.claim_fill(fill, track, sequence, Some(start), reading).await? {
				Some((producer, _)) => Ok(Opened::Group(producer, start)),
				None => {
					tracing::warn!(sequence, start, "no fill to stitch a mid-group stream onto");
					Err(Error::Unsupported)
				}
			},

			// A stream that ends without an object: the group is over and had nothing
			// outside the fill's range, so the head it delivered is the whole group.
			None => match self.claim_fill(fill, track, sequence, None, reading).await? {
				Some((producer, next)) => Ok(Opened::Group(producer, next)),
				None => Ok(Opened::Group(create(track)?, 0)),
			},
		}
	}

	/// Take the head the fill fetch stream delivered for `sequence`, once it has finished
	/// writing it.
	///
	/// The model has one producer per group, so this is the handoff: the fill owns the
	/// producer while it writes objects `0..next`, and the subgroup stream carrying the rest
	/// picks it up here. `start` is the Object ID that stream begins at, or `None` when it
	/// carries no objects at all and simply ends the group.
	///
	/// Waiting is what keeps the two streams from interleaving into one producer. It ends
	/// with the subscription, so a publisher that promises a fill and never delivers one
	/// costs this stream and nothing else. Meanwhile this stream stops holding the
	/// subscription's end open: the fill's own stream holds it if its header arrived, and
	/// the grace gives up on it if not.
	async fn claim_fill(
		&self,
		fill: &kio::Producer<Fill>,
		track: &track::Producer,
		sequence: u64,
		start: Option<u64>,
		reading: &mut Reading,
	) -> Result<Option<(group::Producer, u64)>, Error> {
		reading.park();
		let settled = kio::wait(|waiter| {
			if let Poll::Ready(err) = track.poll_aborted(waiter) {
				return Poll::Ready(Err(err));
			}

			let settled = fill.poll(waiter, |fill| match **fill {
				Fill::Requested | Fill::Serving(_) | Fill::Active => Poll::Pending,
				Fill::Ready { .. } | Fill::Done | Fill::Failed { .. } | Fill::Ended => Poll::Ready(()),
			});

			match settled {
				Poll::Ready(Ok(_)) => Poll::Ready(Ok(())),
				// Fill is never explicitly closed, and this task holds a producer.
				Poll::Ready(Err(_)) => {
					debug_assert!(false, "fill closed while a receive task owns it");
					Poll::Ready(Err(Error::Cancel))
				}
				Poll::Pending => Poll::Pending,
			}
		})
		.await;
		// Hold the end open again before taking the head: once the fill is claimed, this
		// stream is the only thing saying the group is still being read.
		reading.resume();
		settled?;
		fill.write()
			.map_err(|_| {
				debug_assert!(false, "fill closed while claiming its head");
				Error::Cancel
			})?
			.claim(sequence, start)
	}
}

/// Pumps moq-transport subgroup objects from a reader into a group producer:
/// the id delta, the extension headers (carrying the timestamp), the size, the
/// status for empty objects, and the streamed payload.
pub(crate) struct GroupIngest {
	has_extensions: bool,
	timescale: Option<Timescale>,
	version: Version,
	prior_object: Option<u64>,
	/// The live tail must begin exactly after the filled head, even if it is quiet.
	pub(crate) expected_first: Option<u64>,
	properties: crate::model::object::Properties,
	/// The property list `properties` was read from, for a status object, which has no frame.
	raw: bytes::Bytes,
	gap: Option<u64>,
	inflated: Option<u64>,
	phase: IngestPhase,
}

enum IngestPhase {
	/// Reading the object id delta. Stream end here ends the group.
	Delta,
	/// Reading the extension block's size.
	ExtSize,
	/// Reading (and decoding or discarding) the extension block.
	ExtBytes { size: usize },
	/// Reading the object size.
	Size { timestamp: Option<crate::Timestamp> },
	/// Reading the status of an empty object.
	Status { timestamp: Option<crate::Timestamp> },
	/// Streaming the object payload.
	Payload { frame: frame::Producer },
	/// An explicit end-of-group or end-of-track status arrived.
	Finished(Ended),
}

impl GroupIngest {
	pub(crate) fn new(version: Version, group: &ietf::GroupHeader, timescale: Option<Timescale>) -> Self {
		Self {
			has_extensions: group.flags.has_extensions,
			timescale,
			version,
			prior_object: None,
			expected_first: None,
			properties: Default::default(),
			raw: bytes::Bytes::new(),
			gap: None,
			inflated: None,
			phase: IngestPhase::Delta,
		}
	}
}

impl Subscriber {
	/// Read a fill fetch stream: the head of the group a subscription joins part way through.
	///
	/// A draft-22 fill answers the FILL_PARAMETERS we sent and is named by the SUBSCRIBE's
	/// Request ID. A draft-18 joining FETCH has its own request id, mapped back to that
	/// subscription. Unlike a subgroup stream it needs no track alias. It writes the objects
	/// into a group producer of its own and hands that to the subgroup stream carrying the
	/// rest of the group; see [`Fill`]. A reset stream is the publisher's fill-failure
	/// signal, and arrives here as a read error, which drops the head and the join with it.
	pub async fn recv_fill(
		&mut self,
		stream: &mut Reader<crate::transport::boxed::RecvStream, Version>,
	) -> Result<(), Error> {
		// The dispatcher peeked the stream type to get here.
		let _ = stream.varint().await?;
		let header: ietf::FetchHeader = stream.decode().await?;

		let group_fetch = self.state.lock().group_fetches.get(&header.request_id).cloned();
		if let Some(slot) = group_fetch {
			return self.recv_group_fetch(stream, slot).await;
		}

		let (subscribe_id, fill, _counted, multiple) = {
			let state = self.state.lock();
			// A draft-22 fill is named by the SUBSCRIBE's request id. A draft-18 joining
			// FETCH has its own id, which `fetches` maps back to that subscription.
			let joined = state.fetches.get(&header.request_id).copied();
			let subscribe_id = joined.map(FillRequest::subscription).unwrap_or(header.request_id);
			let track = state.subscribes.get(&subscribe_id).ok_or(Error::NotFound)?;
			// Once an update owns the fill, a delayed initial fill cannot claim its slot.
			if joined.is_none() && track.fetch_id.is_some() {
				return Err(Error::Unsupported);
			}
			// A fill is one of the subscription's own data streams, so PUBLISH_DONE counts
			// it. A joining FETCH is a request of its own.
			let counted = (!matches!(joined, Some(FillRequest::Joining(_))))
				.then(|| Reading::open(&track.tail, None, self.runtime.now()));
			(
				subscribe_id,
				track.fill.clone(),
				counted,
				!matches!(joined, Some(FillRequest::Joining(_))),
			)
		};

		// SUBSCRIBE_OK declares the units these object timestamps are in, and this stream can
		// be reordered ahead of it. Taking the fill in the same step is what refuses a second
		// stream for a request that asked for one fill.
		let timescale = kio::wait(|waiter| {
			let accepted = fill.poll(waiter, |fill| match **fill {
				Fill::Requested => Poll::Pending,
				_ => Poll::Ready(()),
			});

			match accepted {
				Poll::Ready(Ok(mut fill)) => Poll::Ready(match *fill {
					Fill::Serving(timescale) => {
						*fill = Fill::Active;
						Ok(timescale)
					}
					// We requested no fill, or this is a second stream answering the one we
					// did. Either way its objects would duplicate a group already in flight.
					_ => Err(Error::Unsupported),
				}),
				// Fill is never explicitly closed, and this task holds a producer.
				Poll::Ready(Err(_)) => {
					debug_assert!(false, "fill closed while a receive task owns it");
					Poll::Ready(Err(Error::Cancel))
				}
				Poll::Pending => Poll::Pending,
			}
		})
		.await?;
		let _filling = Filling(fill.clone());
		// A fill stream can overtake SUBSCRIBE_OK. The fill gate above is released only
		// after that response commits track Info and installs the producer.
		let (track, largest) = {
			let state = self.state.lock();
			let held = state.subscribes.get(&subscribe_id).ok_or(Error::NotFound)?;
			(held.producer.clone().ok_or(Error::NotFound)?, held.fill_largest)
		};

		// Race the peer's stream against the subscription going away, the same way a
		// subgroup stream is served. Otherwise a peer that stalls partway through a payload
		// keeps this handler and its stream alive for as long as it cares to: aborting the
		// track does not close a group producer, since those lifecycles are independent.
		let res = {
			let mut serving = track.clone();
			let mut serve = std::pin::pin!(crate::util::sendable(self.run_fill(
				stream,
				&mut serving,
				timescale,
				multiple
			)));
			kio::wait(|waiter| {
				if let Poll::Ready(err) = track.poll_aborted(waiter) {
					return Poll::Ready(Err(err));
				}
				waiter.poll_future(serve.as_mut())
			})
			.await
		};

		let mut head = match res {
			Ok(head) => head,
			Err(err) => {
				if let Ok(mut state) = fill.write() {
					*state = Fill::failed(largest);
				}
				// As for a subgroup object: the track is malformed, not just this fill.
				if matches!(err, Error::MalformedTrack) {
					let _ = track.abort(Error::MalformedTrack);
				}
				return Err(err);
			}
		};

		if matches!(head, Fill::Ready { sequence, .. } if largest.is_some_and(|largest| sequence < largest)) {
			head.release();
		}
		// A fill that ended without reaching the Largest Group still owed that group's head.
		if matches!(head, Fill::Done) {
			head = Fill::failed(largest);
		}
		// The subscription can end while the head is being written, and its teardown cannot
		// reach a producer this task still owns. So the handoff is where that is settled.
		match fill.write() {
			Ok(mut state) => state.install(head),
			// Fill is never explicitly closed, and this task holds a producer.
			Err(_) => {
				debug_assert!(false, "fill closed while installing its head");
				let mut head = head;
				head.cancel();
				return Err(Error::Cancel);
			}
		}

		Ok(())
	}

	/// Read the fill's objects into a group producer of its own.
	///
	/// Returns the head for the live tail to claim, or [`Fill::Done`] when the stream
	/// carried no objects at all. Aborts the producer on the way out of an error, since a
	/// half-written head is a group with no end.
	async fn run_fill(
		&mut self,
		stream: &mut Reader<crate::transport::boxed::RecvStream, Version>,
		track: &mut track::Producer,
		timescale: Option<Timescale>,
		multiple: bool,
	) -> Result<Fill, Error> {
		let mut head: Option<(u64, u64, crate::recv::Group)> = None;

		match self
			.run_fill_objects(stream, track, timescale, &mut head, multiple)
			.await
		{
			Ok(()) => Ok(match head {
				Some((sequence, next, producer)) => Fill::Ready {
					sequence,
					next,
					producer: producer.into_inner(),
				},
				None => Fill::Done,
			}),
			Err(err) => {
				if let Some((_, _, producer)) = head {
					let _ = producer.abort(err.clone());
				}
				Err(err)
			}
		}
	}

	/// Decode fetch objects into `head`, creating its group from the first object's
	/// absolute IDs.
	///
	/// Earlier groups finish at the next group's first object; the final group's head
	/// waits for its live tail. A Prior Object ID Gap accounts for every missing ID.
	async fn run_fill_objects(
		&mut self,
		stream: &mut Reader<crate::transport::boxed::RecvStream, Version>,
		track: &mut track::Producer,
		timescale: Option<Timescale>,
		head: &mut Option<(u64, u64, crate::recv::Group)>,
		multiple: bool,
	) -> Result<(), Error> {
		let mut prior_group = None;
		let mut skipped: Option<(u64, u64)> = None;
		let mut budget = kio::coop::Budget::new(32);
		let mut first = true;
		while let Some(object) = decode_fetch_object(stream, std::mem::take(&mut first)).await? {
			kio::wait(|waiter| budget.poll_yield(waiter)).await;
			if !object.datagram && !object.subgroup_ok {
				tracing::warn!("subgroup ID is not supported, dropping fill");
				return Err(Error::Unsupported);
			}

			// Object ID still keys off whether the wire Group ID field was present.
			let group = resolve_fetch_group(prior_group, object.group)?;
			if let (Some(prior), Some(group)) = (prior_group, group)
				&& group < prior
			{
				return Err(Error::ProtocolViolation);
			}
			if let Some(sequence) = group {
				prior_group = Some(sequence);
			}

			// A datagram cannot be reconstructed from a FETCH stream.
			if object.datagram {
				tracing::debug!(?group, "a datagram group is not fetchable");
				return Err(Error::NotFetchable);
			}

			let raw = bytes::Bytes::from(object.properties.unwrap_or_default());
			let decoded = super::object::read(&raw, self.version, timescale)?;
			let expected = head
				.as_ref()
				.filter(|(sequence, _, _)| group.is_none_or(|group| group == *sequence))
				.map(|(_, next, _)| *next)
				.or_else(|| {
					skipped
						.filter(|(sequence, _)| group.is_none_or(|group| group == *sequence))
						.map(|(_, next)| next)
				})
				.unwrap_or(0);
			let id = match (object.group.is_some(), object.object) {
				(true, Some(id)) => id,
				(false, Some(delta)) => expected
					.checked_sub(1)
					.and_then(|prior| prior.checked_add(delta))
					.ok_or(Error::ProtocolViolation)?,
				(false, None) => expected,
				_ => return Err(Error::ProtocolViolation),
			};
			if id < expected {
				return Err(Error::ProtocolViolation);
			}
			if id > expected && decoded.gap.is_none_or(|gap| gap < id - expected) {
				return Err(Error::Unsupported);
			}
			let current = head
				.as_ref()
				.map(|(sequence, _, _)| *sequence)
				.or_else(|| skipped.map(|(sequence, _)| sequence));
			if current.is_some() && group.is_some_and(|group| Some(group) != current) && !multiple {
				return Err(Error::Unsupported);
			}
			if current.is_none() || group.is_some_and(|group| Some(group) != current) {
				if let Some((_, _, producer)) = head.take() {
					producer.finish()?;
				}
				skipped = None;
				let sequence = group.ok_or(Error::ProtocolViolation)?;
				match open_fill_group(track, head, sequence) {
					Ok(()) => {}
					// Widening a range can fill groups the copy already owns. Drain their
					// objects without replacing their producer or cutting the rest of the fill.
					Err(Error::Duplicate) if multiple => skipped = Some((sequence, 0)),
					Err(err) => return Err(err),
				}
			}
			if let Some((_, next)) = skipped.as_mut() {
				skip_fetch_payload(stream).await?;
				*next = id.checked_add(1).ok_or(Error::ProtocolViolation)?;
				continue;
			}

			let (_, next, producer) = head.as_mut().expect("the head was created above");
			if *next == 0
				&& let Some(mut wire) = producer.wire()
			{
				wire.first_id = id;
				producer.set_wire(wire)?;
			}
			self.recv_fetch_payload(stream, producer, FetchPayload { decoded, timescale, id })
				.await?;
			*next = id.checked_add(1).ok_or(Error::ProtocolViolation)?;
		}

		Ok(())
	}

	/// Read one fetch object's length and payload into `producer`, after its header.
	async fn recv_fetch_payload(
		&self,
		stream: &mut Reader<crate::transport::boxed::RecvStream, Version>,
		producer: &mut crate::recv::Group,
		payload: FetchPayload,
	) -> Result<(), Error> {
		let FetchPayload { decoded, timescale, id } = payload;
		producer.mark_gap(id, decoded.gap)?;
		let size = stream.varint().await?;
		super::object::check_frame(&decoded.properties, timescale)?;
		let timestamp = object_time(timescale, decoded.timestamp)?;
		let plain = id == producer.frame_count() as u64 && decoded.properties.is_empty();
		let fields = (!plain).then(|| {
			std::sync::Arc::new(crate::model::object::Fields {
				id,
				properties: decoded.properties,
			})
		});
		let header = frame::Header {
			info: frame::Info { size, timestamp },
			fields,
			inflated: super::object::inflated(decoded.inflated, producer.compressed())?,
		};
		let mut frame = producer.create_object(header)?;
		if let Err(err) = kio::wait(|waiter| stream.poll_read_frame(waiter, &mut frame)).await {
			let _ = frame.abort(err.clone());
			return Err(err);
		}
		producer.restore(frame.finish()?);
		Ok(())
	}

	/// Serve a fill a run at a time, newest run first for a descending reader, until it is
	/// answered or nobody waits on it. Each run is one FETCH, except that the source's live
	/// group always goes alone: a run including the group its source is still writing would
	/// hold the earlier groups until it finishes, or be cut short at the Largest Object by a
	/// publisher following draft-22 section 9.11.
	async fn serve_fill(self, broadcast: Path, name: String, fill: track::Fill, timescale: Option<Timescale>) {
		let fill = std::sync::Arc::new(fill);
		let mut last = None;
		while let Some(run) = fill.next_run() {
			let run = match fill.live_group() {
				Some(live) if run.contains(&live) => {
					let mut pieces = [run.start..live, live..live + 1, live + 1..run.end]
						.into_iter()
						.filter(|piece| !piece.is_empty());
					let piece = match fill.order() {
						track::Order::Ascending => pieces.next(),
						track::Order::Descending => pieces.next_back(),
					};
					piece.expect("the run holds the live group")
				}
				_ => run,
			};
			// Every fetch answers its run, so the same one coming back is a bug; ending the
			// fill names it dropped rather than fetching it forever.
			if last.as_ref() == Some(&run) {
				debug_assert!(false, "fill run {run:?} left unanswered");
				return;
			}
			last = Some(run.clone());
			self.clone()
				.run_range_fetch(broadcast.clone(), name.clone(), fill.clone(), run, timescale)
				.await;
		}
	}

	/// Serve a whole fill with one FETCH, as [`Self::serve_fill`] serves each run.
	#[cfg(test)]
	async fn run_group_fetch(self, broadcast: Path, name: String, fill: track::Fill, timescale: Option<Timescale>) {
		let run = fill.start().group..fill.end();
		self.run_range_fetch(broadcast, name, std::sync::Arc::new(fill), run, timescale)
			.await;
	}

	/// Fetch a run of groups from the publisher to fill cache misses, with one standalone
	/// (or draft-22 filtered) FETCH for this subscription's track. It asks from the frame the
	/// reader wants in the first group through the end of the last, so a publisher that
	/// evicted a prefix can still answer.
	///
	/// FETCH_OK settles what the answer covers before any group is accepted, so a refusal
	/// reaches every waiting reader as the publisher's own error. The objects arrive on a
	/// fetch stream, which [`Self::recv_fill`] routes to [`Self::recv_group_fetch`].
	async fn run_range_fetch(
		self,
		broadcast: Path,
		name: String,
		fill: std::sync::Arc<track::Fill>,
		run: std::ops::Range<u64>,
		timescale: Option<Timescale>,
	) {
		let validity = self.validity.start();
		let last = run.end - 1;
		let start = fill.frame_start(run.start);
		let draft20 = Filter::is_draft20(self.version);

		// The owning subscribe/linger loop enforces its admitted gate, including a
		// token-authorized announce outside the session scope. Its abort ends this job.
		// The fill is already owned by waiting readers; credit backpressures it until
		// their demand leaves instead of refusing accepted work.
		let opening = self.session.clone();
		let mut stream = match kio::wait(|waiter| {
			validity.check(waiter)?;
			if let Poll::Ready(err) = fill.poll_aborted(waiter) {
				return Poll::Ready(Err(err));
			}
			if fill.poll_unwanted_groups(run.clone(), waiter).is_ready() {
				return Poll::Ready(Err(Error::Cancel));
			}
			Stream::poll_open(&opening, self.version, waiter)
		})
		.await
		{
			Ok(stream) => stream,
			Err(err) => return fill.reject_groups(run, err),
		};
		stream = stream.with_stats(self.origin.stats());
		let fetch_id = self.control.next_request_id();

		// Registered before the FETCH goes out, since its stream can overtake FETCH_OK.
		let slot = kio::Producer::new(GroupFetch::Pending);
		let registered = GroupFetchEntry::new(&self.state, fetch_id, slot.clone());

		let namespace = broadcast.clone();
		let track = name.as_str().into();
		let from = ietf::Location {
			group: run.start,
			object: start,
		};
		let fetch_type = match draft20 {
			// No End Object includes the whole End Group.
			true => FetchType::Filtered {
				namespace,
				track,
				filter: Filter::Absolute {
					start: from,
					end: Some(ietf::EndLocation {
						group: last,
						object: None,
					}),
				},
			},
			// An End Object of 0 is the whole End Group.
			false => FetchType::Standalone {
				namespace,
				track,
				start: from,
				end: ietf::Location { group: last, object: 0 },
			},
		};
		// Flushed while waiting for the answer, so the terminals below still end a request the
		// peer's send window holds back.
		let res = stream.writer.buffer_message(
			ietf::Fetch::ID,
			&ietf::Fetch {
				request_id: fetch_id,
				subscriber_priority: super::priority::to_wire(fill.priority()),
				group_order: GroupOrder::Ascending,
				fetch_type,
				range_filters: false,
				fill_timeout: false,
				// The copy learned the track before any fetch, from SUBSCRIBE_OK or
				// TRACK_STATUS_OK, so a FETCH_OK repeating it is waste, except that only its
				// TIMESCALE opts the answer into the timestamps a timed copy needs.
				properties_wanted: timescale.is_some(),
				epoch: self.epoch.clone(),
			},
		);
		let mut messages = super::request_stream::Messages::new(self.session.clone(), self.control.client());
		let res = match res {
			Err(err) => SetupResponse::Response(Err(err)),
			Ok(()) => {
				let mut response = std::pin::pin!(async {
					kio::wait(|waiter| stream.writer.poll_flush(waiter)).await?;
					self.read_fetch_response(&mut stream, &mut messages).await
				});
				kio::wait(|waiter| {
					let response = waiter.poll_future(response.as_mut());
					if let Poll::Ready(Err(err)) = response {
						return Poll::Ready(SetupResponse::Response(Err(err)));
					}
					if let Err(err) = validity.check(waiter) {
						return Poll::Ready(SetupResponse::Cancelled(err));
					}
					if let Poll::Ready(err) = fill.poll_aborted(waiter) {
						return Poll::Ready(SetupResponse::Cancelled(err));
					}
					if let Poll::Ready(res) = response {
						return Poll::Ready(SetupResponse::Response(res));
					}
					fill.poll_unwanted_groups(run.clone(), waiter)
						.map(|_| SetupResponse::Cancelled(Error::Cancel))
				})
				.await
			}
		};

		let ok = match res {
			SetupResponse::Response(Ok(ok)) => ok,
			SetupResponse::Cancelled(err) => {
				fill.reject_groups(run, err);
				drop(registered);
				self.cancel_group_fetch(stream).await;
				return;
			}
			SetupResponse::Response(Err(err)) => {
				tracing::debug!(%err, groups = ?run, "group fetch refused");
				fill.reject_groups(run, err);
				let _ = stream.writer.close().await;
				return;
			}
		};

		// Draft-22's End Location is inclusive; older drafts spell a bounded object one past it.
		let mut end = ok.end_location;
		if draft20 {
			let Some(object) = end.object.checked_add(1) else {
				fill.reject_groups(run, Error::ProtocolViolation);
				let _ = stream.writer.close().await;
				return;
			};
			end.object = object;
		}

		// Before draft-22, Object 0 covers the whole named group. Legacy answers one
		// group past it still pass the exclusive comparison below.
		let whole_last = !draft20 && end.group == last && end.object == 0;

		// Draft-22's answer ends at or after its start and never past the requested end, which
		// a publisher following section 9.11 cuts at the Largest Object. Validate before accepting a group or letting End of
		// Track mark where the track ends.
		if draft20 && (end.group < run.start || end.group > last || (end.group == run.start && end.object <= start)) {
			let err = Error::ProtocolViolation;
			tracing::warn!(groups = ?run, ?end, "FETCH_OK's End Location is outside the requested range");
			self.session.clone().close(
				super::error::to_session_code(&SessionError::from(&err)),
				err.to_string().as_ref(),
			);
			fill.reject_groups(run, err);
			return;
		}

		// The publisher knows where the track ends, which a range FETCH downstream needs.
		if ok.end_of_track {
			let Some(final_sequence) = end.group.checked_add(u64::from(whole_last || end.object > 0)) else {
				fill.reject_groups(run, Error::ProtocolViolation);
				let _ = stream.writer.close().await;
				return;
			};
			fill.finish_track_at(final_sequence);
		}

		// Before draft-22, an empty answer opens no fetch stream at all.
		if !whole_last && (end.group, end.object) <= (run.start, start) {
			fill.reject_groups(run, Error::NotFound);
			let _ = stream.writer.close().await;
			return;
		}

		// The groups the answer covers: whole up to the End Location's group, and that one up
		// to its exclusive object bound. Before draft-22, an End Object of 0 covers nothing of
		// a group short of the requested end, and one past the requested end covers it whole.
		let (covered, bound) = match end.object {
			_ if whole_last => (last, None),
			0 => ((end.group - 1).min(last), None),
			_ if end.group > last => (last, None),
			object => (end.group, Some(object)),
		};
		// A publisher may stop at the Largest Object, so the groups past it are not answered.
		// They are refused for now rather than named dropped: they may still be written.
		if covered < last {
			fill.reject_groups(covered + 1..run.end, Error::NotFound);
		}

		if let Ok(mut state) = slot.write() {
			*state = GroupFetch::Ready(RangeFetch {
				fill: fill.clone(),
				timescale,
				first: run.start,
				last: covered,
				start,
				end: bound,
			});
		}
		let covered = run.start..covered + 1;

		// Keep the request open until its data stream finishes or every reader leaves.
		// A publisher that fails after FETCH_OK resets the request instead and owes no fetch
		// stream, so the groups it left waiting are refused. A FIN is not that: the fetch
		// stream can trail it.
		let mut open = true;
		let mut abandoned = false;
		let reset = kio::wait(|waiter| {
			if open {
				let closed = messages.poll_end(&mut stream.reader, waiter);
				match closed {
					Poll::Ready(Err(err)) => return Poll::Ready(Some(err)),
					Poll::Ready(Ok(())) => open = false,
					Poll::Pending => {}
				}
			}
			if let Poll::Ready(err) = fill.poll_aborted(waiter) {
				// Completed history stays cached. Only this FETCH's unfinished group is
				// ours to abort; closing a track never cascades into cached siblings.
				let state = slot.read();
				if let GroupFetch::Receiving(Some(group)) = &*state {
					group.abort_open(err.clone());
				}
				abandoned = !matches!(*state, GroupFetch::Done);
				return Poll::Ready(Some(err));
			}

			let unwanted = fill.poll_unwanted_groups(covered.clone(), waiter).is_ready();
			slot.poll(waiter, |state| match &**state {
				GroupFetch::Done => Poll::Ready(()),
				GroupFetch::Receiving(Some(group))
					if unwanted && group.poll_unwanted(waiter).is_ready() && group.abort_unwanted(Error::Cancel) =>
				{
					abandoned = true;
					Poll::Ready(())
				}
				GroupFetch::Ready(_) | GroupFetch::Receiving(None) if unwanted => {
					abandoned = true;
					Poll::Ready(())
				}
				_ => Poll::Pending,
			})
			.map(|_| None)
		})
		.await;
		if abandoned {
			if let Ok(mut state) = slot.write()
				&& !matches!(*state, GroupFetch::Done)
			{
				*state = GroupFetch::Cancelled;
			}
			// A closed track answers what the run still owes with its abort, so the fill is
			// not asked for the same run again; groups nobody reads are owed nothing.
			if let Some(err) = reset {
				fill.reject_groups(covered, err);
			}
			drop(registered);
			self.cancel_group_fetch(stream).await;
			return;
		}
		// A run already being written is the fetch stream's to settle; one still waiting for
		// its stream is lost.
		if let Some(err) = reset
			&& let Ok(mut state) = slot.write()
			&& matches!(*state, GroupFetch::Ready(_))
		{
			*state = GroupFetch::Done;
			fill.reject_groups(covered, err);
		}
		let _ = stream.writer.close().await;
	}

	/// Cancel a FETCH using the negotiated draft's existing cancellation signal.
	async fn cancel_group_fetch(&self, mut stream: Stream<Transport, Version>) {
		stream.reader.abort(&Error::Cancel);
		stream.writer.abort(&Error::Cancel)
	}

	/// Read the answer to a FETCH: FETCH_OK, or the publisher's refusal as an error.
	async fn read_fetch_response(
		&self,
		stream: &mut Stream<Transport, Version>,
		messages: &mut super::request_stream::Messages,
	) -> Result<ietf::FetchOk, Error> {
		let super::request_stream::Framed(type_id, body) = messages.message(&mut stream.reader).await?;
		let mut data = body.decoder(self.version);

		match type_id {
			ietf::FetchOk::ID => Ok(ietf::FetchOk::decode_msg(&mut data, self.version)?),
			ietf::RequestError::ID => {
				let msg = ietf::RequestError::decode_msg(&mut data, self.version)?;
				Err(request::from_code(msg.error_code, request::Kind::Fetch, self.version))
			}
			_ => Err(Error::UnexpectedMessage),
		}
	}

	/// Write a FETCH's objects into the groups of the run it answers.
	async fn recv_group_fetch(
		&mut self,
		stream: &mut Reader<crate::transport::boxed::RecvStream, Version>,
		slot: kio::Producer<GroupFetch>,
	) -> Result<(), Error> {
		// FETCH_OK can trail its own fetch stream. Taking the run in the same step is what
		// refuses a second stream for one request.
		let taken = kio::wait(|waiter| {
			match slot.poll(waiter, |state| match &**state {
				GroupFetch::Pending => Poll::Pending,
				_ => Poll::Ready(()),
			}) {
				Poll::Ready(Ok(mut state)) => {
					Poll::Ready(match std::mem::replace(&mut *state, GroupFetch::Receiving(None)) {
						GroupFetch::Ready(fetch) => Ok(fetch),
						GroupFetch::Cancelled => {
							*state = GroupFetch::Cancelled;
							Err(Error::Cancel)
						}
						other => {
							*state = other;
							Err(Error::Unsupported)
						}
					})
				}
				Poll::Ready(Err(_)) => Poll::Ready(Err(Error::Cancel)),
				Poll::Pending => Poll::Pending,
			}
		})
		.await;
		let mut writer = RangeWriter::new(taken?, &slot);

		let res = {
			let mut receive = std::pin::pin!(crate::util::sendable(
				self.recv_group_fetch_objects(stream, &mut writer)
			));
			kio::wait(|waiter| {
				if let Poll::Ready(res) = waiter.poll_future(receive.as_mut()) {
					return Poll::Ready(res);
				}
				// The request gave up on the run, or the group being written was aborted under
				// it, as by eviction while the stream stalls: the rest of the run is refused
				// rather than left waiting on a stream that may never resume.
				let mut aborted = Error::Cancel;
				slot.poll(waiter, |state| match &**state {
					GroupFetch::Cancelled => Poll::Ready(()),
					GroupFetch::Receiving(Some(group)) => group.poll_aborted(waiter).map(|err| aborted = err),
					_ => Poll::Pending,
				})
				.map(|_| Err(aborted))
			})
			.await
		};

		// Completion and abandonment share the slot lock, so a complete group is never
		// aborted between finishing it and publishing Done to the request handler.
		let mut state = slot.write().ok();
		let res = writer.settle(res);
		if let Some(state) = state.as_mut() {
			**state = GroupFetch::Done;
		}
		res
	}

	/// Decode a FETCH's objects into the groups of its run, preserving marked gaps and their
	/// properties, never past FETCH_OK's End Location. A group's objects end where the next
	/// group's begin, since a FETCH carries every object of its range in order. Before
	/// draft-22 the stream must also reach an object bound inside the last group.
	async fn recv_group_fetch_objects(
		&self,
		stream: &mut Reader<crate::transport::boxed::RecvStream, Version>,
		writer: &mut RangeWriter<'_>,
	) -> Result<(), Error> {
		let timescale = writer.fetch.timescale;
		// The next Object ID expected in the current group.
		let mut next = 0;
		let mut prior_group = None;
		let mut first = true;
		while let Some(object) = decode_fetch_object(stream, std::mem::take(&mut first)).await? {
			if !object.subgroup_ok && !object.datagram {
				tracing::warn!("subgroup ID is not supported, dropping group fetch");
				return Err(Error::Unsupported);
			}

			let group = resolve_fetch_group(prior_group, object.group)?;
			if let Some(group) = group {
				prior_group = Some(group);
			}
			// The first object always names its group, so a later one leaving it out stays in it.
			let sequence = group.or(writer.sequence()).ok_or(Error::ProtocolViolation)?;
			if writer.sequence() != Some(sequence) {
				next = writer.open(sequence, object.datagram)?;
			} else if object.datagram {
				// A datagram group has one object, so it never shares one with a stream.
				return Err(Error::ProtocolViolation);
			}

			let id = match (object.group.is_some(), object.object) {
				(true, Some(id)) => id,
				(false, Some(delta)) => next
					.checked_sub(1)
					.and_then(|prior| prior.checked_add(delta))
					.ok_or(Error::ProtocolViolation)?,
				(false, None) => next,
				_ => return Err(Error::ProtocolViolation),
			};
			if id < next {
				return Err(Error::ProtocolViolation);
			}
			let raw = bytes::Bytes::from(object.properties.unwrap_or_default());
			let decoded = super::object::read(&raw, self.version, timescale)?;
			if id > next && decoded.gap.is_none_or(|gap| gap < id - next) {
				return Err(Error::Unsupported);
			}
			match writer.producer() {
				Some(producer) => {
					self.recv_fetch_payload(stream, producer, FetchPayload { decoded, timescale, id })
						.await?
				}
				// Cached already, or a datagram: read past it.
				None => skip_fetch_payload(stream).await?,
			}
			next = id.checked_add(1).ok_or(Error::ProtocolViolation)?;
			if writer.bound(sequence).is_some_and(|end| next > end) {
				tracing::warn!(sequence, "a group fetch continued past FETCH_OK's End Location");
				return Err(Error::ProtocolViolation);
			}
		}

		// A clean FIN short of the promised end would otherwise cache a truncated group as
		// a complete one. Draft-22's End Location is the range covered, not the last object
		// sent: objects missing before it do not exist (section 9.11). Before draft-22 the
		// stream must reach the bounded group at all, not just its objects.
		if !Filter::is_draft20(self.version)
			&& let Some(end) = writer.fetch.end
			&& (writer.sequence() != Some(writer.fetch.last) || next < end)
		{
			let sequence = writer.sequence();
			tracing::warn!(?sequence, next, "a group fetch ended before FETCH_OK's End Location");
			return Err(Error::ProtocolViolation);
		}

		Ok(())
	}
}

/// Read past one fetch object's payload, for a group this fetch does not write.
async fn skip_fetch_payload(stream: &mut Reader<crate::transport::boxed::RecvStream, Version>) -> Result<(), Error> {
	let mut left = stream.varint().await?;
	let mut budget = kio::coop::Budget::new(32);
	while left > 0 {
		kio::wait(|waiter| budget.poll_yield(waiter)).await;
		let max = usize::try_from(left).unwrap_or(usize::MAX);
		let chunk = kio::wait(|waiter| stream.poll_read_chunk(waiter, max))
			.await?
			.ok_or(Error::ProtocolViolation)?;
		left -= chunk.len() as u64;
	}
	Ok(())
}

struct FetchPayload {
	decoded: super::object::Decoded,
	timescale: Option<Timescale>,
	id: u64,
}

/// One Object header on a fetch stream, after version-specific decoding.
struct FetchedObject {
	group: Option<u64>,
	object: Option<u64>,
	/// A fetch may carry an Object published as a datagram. A datagram group is
	/// never cached, so it is never filled from a fetch.
	datagram: bool,
	subgroup_ok: bool,
	properties: Option<Vec<u8>>,
}

/// Decode the next fetch object header, or `None` at stream end.
///
/// The `first` object on a stream has no prior to inherit from, so a field it leaves to
/// the prior object is a protocol violation.
async fn decode_fetch_object<R: crate::transport::poll::RecvStream>(
	stream: &mut Reader<R, Version>,
	first: bool,
) -> Result<Option<FetchedObject>, Error> {
	Ok(match stream.decode_maybe::<ietf::FetchObject>().await? {
		None => None,
		Some(ietf::FetchObject::EndOfRange { .. }) => {
			tracing::warn!("a fill with an End of Range cannot be stitched");
			return Err(Error::Unsupported);
		}
		Some(ietf::FetchObject::Object {
			subgroup,
			group,
			object,
			priority,
			properties,
		}) => {
			let inherits = group.is_none()
				|| object.is_none()
				|| priority.is_none()
				|| matches!(subgroup, ietf::FetchSubgroup::Prior | ietf::FetchSubgroup::PriorPlusOne);
			if first && inherits {
				tracing::warn!("the first fetch object refers to a prior object");
				return Err(Error::ProtocolViolation);
			}
			Some(FetchedObject {
				group,
				object,
				datagram: subgroup == ietf::FetchSubgroup::Datagram,
				subgroup_ok: matches!(
					subgroup,
					ietf::FetchSubgroup::Zero | ietf::FetchSubgroup::Prior | ietf::FetchSubgroup::Explicit(0)
				),
				properties,
			})
		}
	})
}

/// Absolute Group ID from a fetch object's Group ID field.
///
/// The first object is always absolute; later ones send a Group ID Delta, resolved here
/// in the ascending order this FETCH requested.
fn resolve_fetch_group(prior: Option<u64>, wire: Option<u64>) -> Result<Option<u64>, Error> {
	let Some(wire) = wire else {
		return Ok(None);
	};
	let Some(prior) = prior else {
		return Ok(Some(wire));
	};
	let step = wire.checked_add(1).ok_or(Error::Unsupported)?;
	let group = prior.checked_add(step).ok_or(Error::Unsupported)?;
	Ok(Some(group))
}

fn open_fill_group(
	track: &mut track::Producer,
	head: &mut Option<(u64, u64, crate::recv::Group)>,
	sequence: u64,
) -> Result<(), Error> {
	let producer = track.create_group(group::Info { sequence })?;
	producer.set_wire(group::Wire {
		sparse: false,
		first_object: true,
		first_id: 0,
		extensions: true,
		has_end: true,
		fetchable: true,
		last: None,
	})?;
	*head = Some((sequence, 0, crate::recv::Group::new(producer)));
	Ok(())
}

/// Ready once a finished subscription's data streams are accounted for: Stream Count of
/// their headers, and no fill outstanding. A tail parked on its head holds nothing open
/// itself, so the fill does until the head is claimed.
fn poll_settled(settle: &mut Settle, waiter: &kio::Waiter, fill: &kio::Producer<Fill>, count: u64) -> Poll<()> {
	// Read before the tail: Done is terminal, so the answer cannot go stale.
	let filled = !fill.read().outstanding();
	settle.poll(waiter, |tail| filled && count > 0 && tail.streams() >= count)
}

/// Ready once a join's head is in the copy, or none is coming.
fn poll_headed(fill: &kio::Producer<Fill>, waiter: &kio::Waiter) -> Poll<()> {
	let headed = fill.poll(waiter, |fill| match **fill {
		Fill::Requested | Fill::Serving(_) | Fill::Active => Poll::Pending,
		Fill::Ready { .. } | Fill::Done | Fill::Failed { .. } | Fill::Ended => Poll::Ready(()),
	});
	match headed {
		Poll::Pending => Poll::Pending,
		Poll::Ready(_) => Poll::Ready(()),
	}
}

/// A Normal object's timestamp: none on an untimed track, and required on a timed one,
/// where an object without a Timestamp is malformed.
fn object_time(
	timescale: Option<Timescale>,
	timestamp: Option<crate::Timestamp>,
) -> Result<Option<crate::Timestamp>, Error> {
	match (timescale, timestamp) {
		(Some(_), None) => Err(Error::MalformedTrack),
		(_, timestamp) => Ok(timestamp),
	}
}

impl GroupIngest {
	fn header(&mut self, group: &group::Producer, info: frame::Info) -> Result<frame::Header, Error> {
		super::object::check_frame(&self.properties, self.timescale)?;
		let id = self.prior_object.expect("payload follows object ID");
		group.mark_gap(self.prior_object.expect("properties follow Object ID"), self.gap)?;
		let plain = id == group.frame_count() as u64 && self.properties.is_empty();
		let fields = (!plain).then(|| {
			std::sync::Arc::new(crate::model::object::Fields {
				id,
				properties: std::mem::take(&mut self.properties),
			})
		});
		Ok(frame::Header {
			info,
			fields,
			inflated: super::object::inflated(self.inflated, group.compressed())?,
		})
	}

	/// `Ready(Ok(_))` once the stream FINs on an object boundary, or an explicit
	/// end-of-group or end-of-track status arrives. The caller finishes or aborts the
	/// group; an object cut short mid-payload was already aborted here with the reason.
	pub(crate) fn poll<R: crate::transport::poll::RecvStream>(
		&mut self,
		reader: &mut Reader<R, Version>,
		group: &mut crate::recv::Group,
		waiter: &kio::Waiter,
	) -> Poll<Result<Ended, Error>> {
		// An empty object is a few bytes, so a peer keeping them ready must not hold the
		// session's other tasks.
		let mut budget = kio::coop::Budget::new(32);
		loop {
			match &mut self.phase {
				IngestPhase::Delta => {
					ready!(budget.poll_yield(waiter));
					let Some(id_delta) = ready!(reader.poll_varint_maybe(waiter))? else {
						return Poll::Ready(Ok(Ended::Group));
					};
					let id = next_object_id(self.prior_object, id_delta)?;
					if self.prior_object.is_none() && self.expected_first.is_some_and(|expected| expected != id) {
						return Poll::Ready(Err(Error::Unsupported));
					}
					self.prior_object = Some(id);
					self.properties = Default::default();
					self.raw = bytes::Bytes::new();
					self.gap = None;
					self.inflated = None;
					self.phase = match self.has_extensions {
						true => IngestPhase::ExtSize,
						false => IngestPhase::Size { timestamp: None },
					};
				}
				IngestPhase::ExtSize => {
					let ObjectExtensionsLength(size) = ready!(reader.poll_decode(waiter))?;
					self.phase = IngestPhase::ExtBytes { size };
				}
				IngestPhase::ExtBytes { size } => {
					let ext = ready!(reader.poll_read_exact(waiter, *size))?;
					let decoded = super::object::read(&ext, self.version, self.timescale)?;
					let timestamp = decoded.timestamp;
					self.properties = decoded.properties;
					self.raw = ext;
					self.gap = decoded.gap;
					self.inflated = decoded.inflated;
					self.phase = IngestPhase::Size { timestamp };
				}
				IngestPhase::Size { timestamp } => {
					let size = ready!(reader.poll_varint(waiter))?;
					if size == 0 {
						self.phase = IngestPhase::Status { timestamp: *timestamp };
						continue;
					}
					// `create_object` is the allocation chokepoint: it rejects an oversized
					// `size` and allocates up front only within the origin's budget, so no
					// pre-check is needed.
					let timestamp = object_time(self.timescale, *timestamp)?;
					let header = self.header(group, frame::Info { size, timestamp })?;
					let frame = group.create_object(header)?;
					self.phase = IngestPhase::Payload { frame };
				}
				IngestPhase::Status { timestamp } => {
					let status = ready!(reader.poll_varint(waiter))?;
					if status == 0 {
						let timestamp = object_time(self.timescale, *timestamp)?;
						let header = self.header(group, frame::Info { size: 0, timestamp })?;
						let frame = group.create_object(header)?;
						group.restore(frame.finish()?);
						self.phase = IngestPhase::Delta;
						continue;
					}
					// A status object has no frame to hold its time, so its block keeps it.
					self.properties = super::object::unframed(&self.raw, self.version, self.timescale)?;
					super::object::check_status(&self.properties)?;
					if status == END_OF_GROUP {
						group.mark_gap(self.prior_object.expect("properties follow Object ID"), self.gap)?;
						group.set_end_object(crate::model::object::Fields {
							id: self.prior_object.expect("status follows object ID"),
							properties: std::mem::take(&mut self.properties),
						})?;
						// Allowed even when the header marks the group's end: that bit only
						// lets a FIN imply it, and imquic sends both.
						self.phase = IngestPhase::Finished(Ended::Group);
					} else if status == END_OF_TRACK {
						// Defined on every implemented draft, whether or not the header marks
						// the group's end.
						self.phase = IngestPhase::Finished(Ended::Track {
							object: self.prior_object.expect("status follows object ID"),
							properties: !self.properties.is_empty(),
						});
					} else {
						return Poll::Ready(Err(Error::Unsupported));
					}
				}
				IngestPhase::Payload { frame } => {
					let failed = ready!(reader.poll_read_frame(waiter, frame)).err();

					let IngestPhase::Payload { frame } = std::mem::replace(&mut self.phase, IngestPhase::Delta) else {
						unreachable!()
					};
					match failed {
						None => group.restore(frame.finish()?),
						Some(err) => {
							// Fail the group with the reason, not the Drop fallback's
							// generic `Dropped`.
							let _ = frame.abort(err.clone());
							return Poll::Ready(Err(err));
						}
					}
				}
				IngestPhase::Finished(ended) => {
					let ended = std::mem::replace(ended, Ended::Group);
					return Poll::Ready(Ok(ended));
				}
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use crate::model::ProduceTest;
	use futures::poll;

	use super::*;

	/// What keeps a SUBSCRIBE_NAMESPACE open until the peer ends it, while the first lives.
	fn live() -> (kio::Producer<()>, kio::Consumer<()>) {
		let live = kio::Producer::new(());
		let consumer = live.consume();
		(live, consumer)
	}

	#[moq_clock::test]
	async fn internal_alias_resolution_ends_with_its_session() {
		let aliases = TrackAliases::default();
		let consumer = aliases.consume();
		drop(aliases);
		assert!(matches!(
			resolve_track_alias(&crate::time::sim(), consumer, 7).await,
			Err(Error::Cancel)
		));
	}

	async fn check_publish_fin(responses: Vec<u8>, clean: bool) {
		use crate::lite::test_transport::ScriptedSession;
		use crate::transport::poll::Session as _;
		let session = ScriptedSession::eof(responses);
		let (_, recv) = session.open_bi(Default::default()).await.unwrap();
		let mut reader = Reader::new(crate::transport::boxed::RecvStream::new(recv), Version::Draft18);
		let result = Subscriber::read_publish_done(
			&mut reader,
			Version::Draft18,
			&AtomicU64::new(0),
			&mut crate::ietf::request_stream::Messages::new(crate::driver::Transport::new(session.clone()), true),
			|_| {},
		)
		.await;
		if clean {
			assert_eq!(result.unwrap().stream_count, 0);
		} else {
			assert!(matches!(result, Err(Error::ProtocolViolation)));
		}
	}

	fn fin_responses(clean: bool) -> Vec<u8> {
		use crate::coding::Encode;
		let mut responses = Vec::new();
		if clean {
			crate::coding::Encoder::new(&mut responses, Version::Draft18.into())
				.varint(ietf::PublishDone::ID)
				.unwrap();
			ietf::PublishDone {
				status_code: ietf::PublishDoneStatus::TrackEnded.code(),
				stream_count: 0,
				reason_phrase: "done".into(),
			}
			.encode(
				&mut crate::coding::Encoder::new(&mut responses, Version::Draft18.into()),
				Version::Draft18,
			)
			.unwrap();
		}
		responses
	}

	#[moq_clock::test]
	async fn bare_fin_requires_publish_done() {
		for clean in [false, true] {
			check_publish_fin(fin_responses(clean), clean).await;
		}
	}

	#[moq_clock::test]
	#[ignore = "requires Bun; run by just test bare-fin in interop CI"]
	async fn bare_fin_interop() {
		for clean in [false, true] {
			let responses = crate::test_interop::fin("moqt-18", false, clean, fin_responses(clean));
			check_publish_fin(responses, clean).await;
		}
	}

	#[moq_clock::test]
	async fn track_alias_waits_for_control_message() {
		let runtime = crate::time::sim();
		let aliases = TrackAliases::default();
		let pending = resolve_track_alias(&runtime, aliases.consume(), 7);
		let mut pending = std::pin::pin!(pending);

		assert!(poll!(&mut pending).is_pending());

		insert_track_alias(&aliases, 7, RequestId(11)).unwrap();

		assert_eq!(pending.await.unwrap(), RequestId(11));
	}

	/// SUBSCRIBE_OK has not accepted the track, so the map holds no producer.
	/// Abort still has to reject the parked origin request with the session error.
	#[moq_clock::test]
	async fn session_death_rejects_a_subscribe_still_setting_up() {
		let broadcast = crate::broadcast::Info::new().produce();
		let mut dynamic = broadcast.requests();
		let consumer = broadcast.consume();
		let mut waiting = std::pin::pin!(consumer.track("video").unwrap().subscribe(None));
		assert!(poll!(&mut waiting).is_pending());

		let mut requested = std::pin::pin!(dynamic.requested_track());
		let std::task::Poll::Ready(Ok(request)) = poll!(&mut requested) else {
			panic!("the subscribe did not request a track");
		};

		let mut state = State::default();
		state.subscribes.insert(
			RequestId(1),
			TrackState {
				pending: Some(request),
				..TrackState::pending(
					"video".to_string(),
					crate::Path::from_static("bcast"),
					kio::Producer::new(Fill::Done),
				)
			},
		);
		state.abort(&Error::Session(crate::SessionError::App(7)));

		assert!(
			matches!(
				poll!(&mut waiting),
				std::task::Poll::Ready(Err(Error::Session(crate::SessionError::App(7))))
			),
			"setup was not rejected with the session error"
		);
	}

	#[moq_clock::test]
	async fn unknown_track_alias_times_out() {
		let aliases = TrackAliases::default();
		assert!(matches!(
			resolve_track_alias(&crate::time::sim(), aliases.consume(), 7).await,
			Err(Error::NotFound)
		));
	}

	async fn settle() {
		moq_clock::sleep(Duration::from_millis(1)).await;
	}

	fn occurrences(log: &crate::lite::test_transport::Log, needle: &[u8]) -> usize {
		let writes = log.writes.lock().unwrap();
		writes.windows(needle.len()).filter(|window| *window == needle).count()
	}

	/// What an unsolicited advertisement means to a subscriber on `version` whose peer
	/// declared `solicit`, when we declared MoQ Solicit ourselves.
	fn unsolicited_is_a_violation(solicit: Option<bool>, version: Version) -> bool {
		unsolicited_is_a_violation_to(true, solicit, version)
	}

	/// The same, for a subscriber whose own SETUP declared MoQ Solicit only if `ours`.
	fn unsolicited_is_a_violation_to(ours: bool, solicit: Option<bool>, version: Version) -> bool {
		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let session = crate::lite::test_transport::SinkSession::new(Default::default());
		let peer_setup = peer::PeerSetup::default();
		peer_setup.set(peer::Peer {
			solicit,
			..Default::default()
		});
		let (tasks, _task_set) = crate::util::TaskSet::new();

		Subscriber::new(
			crate::time::sim(),
			crate::driver::Transport::new(session.clone()),
			origin,
			Control::new(false),
			None,
			peer_setup,
			crate::Hop::new(1).unwrap(),
			None,
			version,
			tasks,
			Default::default(),
		)
		.with_solicit(ours)
		.unsolicited_is_a_violation(solicit)
	}

	/// A side that declined MoQ Solicit invited unsolicited advertisements, so one is
	/// expected even from a peer that implements the extension.
	#[moq_clock::test]
	async fn an_announce_to_a_side_that_declined_solicit_is_fine() {
		for solicit in [Some(true), Some(false), None] {
			assert!(
				!unsolicited_is_a_violation_to(false, solicit, Version::Draft18),
				"{solicit:?}"
			);
		}
	}

	/// By default we declare that advertisements to us must be solicited, so a peer that
	/// implements the extension and announces anyway has a bug. Tolerating it is what
	/// keeps that bug invisible on both sides, so the session goes.
	///
	/// Writing the option is the proof of support, whichever value it carries: an explicit
	/// 0 says "no requirement of my own" and still says "I read yours".
	#[moq_clock::test]
	async fn an_announce_from_a_peer_that_implements_solicit_is_fatal() {
		assert!(
			unsolicited_is_a_violation(Some(true), Version::Draft18),
			"a peer that requires solicitation itself"
		);
		assert!(
			unsolicited_is_a_violation(Some(false), Version::Draft18),
			"an explicit 0 declares support, so ours binds it too"
		);
	}

	/// A peer that declared nothing has never heard of the extension, so it cannot have
	/// honored ours. Announcing at us is what it is supposed to do, and #2730 is what
	/// happens when nobody does.
	#[moq_clock::test]
	async fn an_announce_from_a_peer_that_declared_nothing_is_fine() {
		assert!(!unsolicited_is_a_violation(None, Version::Draft18));
	}

	/// A rooted subscriber asks the peer for its permitted SCOPE. The root names where
	/// replies mount on our side, which is meaningless to a peer outside our namespace,
	/// so sending it asks for a prefix that matches nothing there.
	#[moq_clock::test]
	async fn a_rooted_subscriber_asks_for_its_scope_not_its_root() {
		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let scope = crate::Patterns::from(crate::Pattern::subtree("cam").unwrap());
		let scoped = origin.scope("rootns", &scope).expect("scope the origin");

		let gate = kio::Producer::new(true);
		let session = crate::lite::test_transport::SinkSession::gated_bi(gate.consume());
		let log = session.log.clone();
		let (tasks, _task_set) = crate::util::TaskSet::new();
		// The request waits on the peer's SETUP to learn whether it may opt in to
		// hidden namespaces.
		let peer_setup = peer::PeerSetup::default();
		peer_setup.set(peer::Peer::default());
		let mut subscriber = Subscriber::new(
			crate::time::sim(),
			crate::driver::Transport::new(session.clone()),
			scoped,
			Control::new(false),
			None,
			peer_setup,
			crate::Hop::new(1).unwrap(),
			None,
			Version::Draft18,
			tasks,
			Default::default(),
		);

		let mut namespaces = crate::model::interest_prefixes(&subscriber.origin.allowed());
		assert_eq!(
			namespaces,
			vec![crate::Path::from_static("cam")],
			"one SUBSCRIBE_NAMESPACE per permitted prefix, relative to the root",
		);
		let prefix = namespaces.pop().unwrap();

		let (_live, live) = live();
		let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), Version::Draft18)
			.await
			.unwrap();
		let mut run =
			std::pin::pin!(subscriber.run_subscribe_namespace(stream, prefix, crate::model::Snapshot::landed(), live));
		// Parks awaiting the peer's response; the request is already on the wire.
		assert!(futures::poll!(run.as_mut()).is_pending());

		assert_eq!(occurrences(&log, b"cam"), 1, "asked the peer for our scope");
		assert_eq!(occurrences(&log, b"rootns"), 0, "asked the peer for our local root");
	}

	/// A SUBSCRIBE_NAMESPACE the peer's send window holds back part-way still ends as soon as
	/// `live` closes, resetting the stream: a FIN would hand the peer a truncated request,
	/// and there is no answer to wait for.
	#[moq_clock::test]
	async fn a_namespace_request_stalled_on_credit_ends_when_cancelled() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			let session = crate::lite::test_transport::ScriptedSession::new(Vec::new()).with_send_window(1);
			let log = session.log.clone();
			let (tasks, _task_set) = crate::util::TaskSet::new();
			let mut subscriber = Subscriber::new(
				crate::time::sim(),
				crate::driver::Transport::new(session.clone()),
				crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce(),
				Control::new(false),
				None,
				peer::PeerSetup::declared(peer::Peer::default()),
				crate::Hop::new(1).unwrap(),
				None,
				version,
				tasks,
				Default::default(),
			);

			let (cancel, live) = live();
			let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), version)
				.await
				.unwrap();
			let mut run = std::pin::pin!(subscriber.run_subscribe_namespace(
				stream,
				Path::from_static("room"),
				crate::model::Snapshot::landed(),
				live
			));
			assert!(futures::poll!(run.as_mut()).is_pending());
			assert_eq!(log.writes.lock().unwrap().len(), 1, "{version}: stalled mid-message");

			drop(cancel);
			let Poll::Ready(res) = futures::poll!(run.as_mut()) else {
				panic!("{version}: the cancelled request stayed alive");
			};
			assert!(matches!(res, Ok(namespaces::Ended::Local)), "{version}: {res:?}");
			assert_eq!(log.resets(), vec![crate::ietf::error::CANCELLED], "{version}");
			assert_eq!(log.stops(), vec![crate::ietf::error::CANCELLED], "{version}");
			assert!(
				!log.trail()
					.iter()
					.any(|(_, sent)| matches!(sent, crate::lite::test_transport::Sent::Finish)),
				"{version}: finished a truncated request"
			);
		}
	}

	/// The peer's REQUEST_OK followed by one NAMESPACE, framed exactly as
	/// `run_subscribe_namespace` reads it -- built with the crate's own writer so the
	/// framing can't drift from the encoder under test.
	async fn namespace_response(version: Version, suffix: &str) -> Vec<u8> {
		let log = crate::lite::test_transport::Log::default();
		let mut writer = crate::coding::Writer::new(
			crate::transport::boxed::SendStream::new(crate::lite::test_transport::SinkSend::new(log.clone())),
			version,
		);

		writer.varint(ietf::RequestOk::ID).await.unwrap();
		writer
			.encode(&ietf::RequestOk {
				largest: None,
				active: None,
			})
			.await
			.unwrap();
		writer.varint(ietf::Namespace::ID).await.unwrap();
		writer
			.encode(&ietf::Namespace {
				suffix: crate::Path::new(suffix).unwrap(),
				params: None,
			})
			.await
			.unwrap();

		let writes = log.writes.lock().unwrap();
		writes.clone()
	}

	/// A NAMESPACE suffix is relative to the prefix we subscribed, and mounts under
	/// our root exactly once.
	///
	/// Driven through the real response stream rather than by recomputing the join
	/// here: a test that did its own `prefix.join(suffix)` would still pass if the
	/// NAMESPACE arm went back to joining the root.
	#[moq_clock::test]
	async fn a_rooted_subscriber_mounts_a_reply_under_its_root_once() {
		const VERSION: Version = Version::Draft18;

		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let consumer = origin.consume();
		let scope = crate::Patterns::from(crate::Pattern::subtree("cam").unwrap());
		let scoped = origin.scope("rootns", &scope).expect("scope the origin");

		let session = crate::lite::test_transport::ScriptedSession::new(namespace_response(VERSION, "x.hang").await);
		let (tasks, _task_set) = crate::util::TaskSet::new();
		// Draft-18 can negotiate the cluster extension, so the subscriber waits for
		// the peer's SETUP before resolving advertisements; settle it as extension-off.
		let peer_setup = peer::PeerSetup::default();
		peer_setup.set(peer::Peer::default());
		let mut subscriber = Subscriber::new(
			crate::time::sim(),
			crate::driver::Transport::new(session.clone()),
			scoped,
			Control::new(false),
			None,
			peer_setup,
			crate::Hop::new(1).unwrap(),
			None,
			VERSION,
			tasks,
			Default::default(),
		);

		let prefix = crate::model::interest_prefixes(&subscriber.origin.allowed())
			.pop()
			.expect("one prefix");
		let (_live, live) = live();
		let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), VERSION)
			.await
			.unwrap();
		// Parks on the read after the scripted NAMESPACE is consumed.
		let mut run =
			std::pin::pin!(subscriber.run_subscribe_namespace(stream, prefix, crate::model::Snapshot::landed(), live));
		for _ in 0..100 {
			// The result is deliberately ignored: a regressed mount lands out of scope
			// and errors here, which the assertions below name far better than a poll
			// would.
			let _ = futures::poll!(run.as_mut());
			if routed_now(&consumer, "rootns/cam/x.hang").is_some() {
				break;
			}
			settle().await;
		}

		assert!(
			routed_now(&consumer, "rootns/cam/x.hang").is_some(),
			"the reply mounts under the root once",
		);
		assert!(
			routed_now(&consumer, "rootns/rootns/cam/x.hang").is_none(),
			"the root was applied twice",
		);
	}

	/// A NAMESPACE that loops back through us detaches its route, but the advertisement
	/// stays live on the wire until its NAMESPACE_DONE: a repeat that restores a clean path
	/// while gaining or losing an epoch is still the violation it would be without the loop.
	#[moq_clock::test]
	async fn a_reflected_namespace_keeps_its_epoch_live() {
		const VERSION: Version = Version::Draft18;
		let us = crate::Hop::new(5).unwrap();
		let epoch = crate::Epoch::mint();

		for (reflected, drops) in [(false, true), (true, true), (true, false)] {
			let log = crate::lite::test_transport::Log::default();
			let mut writer = crate::coding::Writer::new(
				crate::transport::boxed::SendStream::new(crate::lite::test_transport::SinkSend::new(log.clone())),
				VERSION,
			);
			writer.varint(ietf::RequestOk::ID).await.unwrap();
			writer
				.encode(&ietf::RequestOk {
					largest: None,
					active: None,
				})
				.await
				.unwrap();
			let mut adverts = vec![(hop_path(&[7, 9]), Some(epoch.clone()))];
			if reflected {
				adverts.push((hop_path(&[7, 5, 9]), Some(epoch.clone())));
			}
			adverts.push((hop_path(&[7, 9]), (!drops).then(|| epoch.clone())));
			for (hops, epoch) in adverts {
				writer.varint(ietf::Namespace::ID).await.unwrap();
				writer
					.encode(&ietf::Namespace {
						suffix: crate::Path::from_static("room"),
						params: Some(ietf::AdvertParams {
							cluster: Some(cluster::Advert { hops, cost: 0 }),
							epoch,
						}),
					})
					.await
					.unwrap();
			}
			let script = log.writes.lock().unwrap().clone();

			let origin = crate::origin::Config::new(us).produce();
			let session = crate::lite::test_transport::ScriptedSession::new(script);
			let (tasks, _task_set) = crate::util::TaskSet::new();
			let peer_setup = peer::PeerSetup::default();
			peer_setup.set(peer::Peer {
				cluster: cluster::Peer {
					hop: Some(crate::Hop::new(9).unwrap()),
					cost: None,
				},
				epoch: true,
				..Default::default()
			});
			let mut subscriber = Subscriber::new(
				crate::time::sim(),
				crate::driver::Transport::new(session.clone()),
				origin,
				Control::new(false),
				None,
				peer_setup,
				us,
				None,
				VERSION,
				tasks,
				Default::default(),
			);

			let prefix = crate::model::interest_prefixes(&subscriber.origin.allowed())
				.pop()
				.expect("one prefix");
			let (_live, live) = live();
			let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), VERSION)
				.await
				.unwrap();
			let mut run = std::pin::pin!(subscriber.run_subscribe_namespace(
				stream,
				prefix,
				crate::model::Snapshot::landed(),
				live
			));
			let mut result = None;
			for _ in 0..100 {
				if let std::task::Poll::Ready(done) = futures::poll!(run.as_mut()) {
					result = Some(done);
					break;
				}
				settle().await;
			}
			let violated = matches!(result, Some(Err(ref err)) if is_protocol_violation(err));
			assert_eq!(violated, drops, "reflected: {reflected}, drops: {drops}: {result:?}");
		}
	}

	/// A namespace the peer holds on a SUBSCRIBE_NAMESPACE stream takes an announce slot
	/// whatever we make of it, so reflected ones the origin never sees still exhaust the
	/// session's budget, and a NAMESPACE_DONE gives the slot back.
	#[moq_clock::test]
	async fn a_reflected_namespace_holds_an_announce_slot() {
		const VERSION: Version = Version::Draft18;
		let us = crate::Hop::new(5).unwrap();

		let log = crate::lite::test_transport::Log::default();
		let mut writer = crate::coding::Writer::new(crate::lite::test_transport::SinkSend::new(log.clone()), VERSION);
		writer.varint(ietf::RequestOk::ID).await.unwrap();
		writer
			.encode(&ietf::RequestOk {
				largest: None,
				active: None,
			})
			.await
			.unwrap();
		let namespace = |suffix| ietf::Namespace {
			suffix: crate::Path::new(suffix).unwrap(),
			params: Some(ietf::AdvertParams {
				cluster: Some(cluster::Advert {
					hops: hop_path(&[7, 5, 9]),
					cost: 0,
				}),
				epoch: None,
			}),
		};
		writer.varint(ietf::Namespace::ID).await.unwrap();
		writer.encode(&namespace("a")).await.unwrap();
		writer.varint(ietf::NamespaceDone::ID).await.unwrap();
		writer
			.encode(&ietf::NamespaceDone {
				suffix: crate::Path::from_static("a"),
			})
			.await
			.unwrap();
		for suffix in ["b", "c"] {
			writer.varint(ietf::Namespace::ID).await.unwrap();
			writer.encode(&namespace(suffix)).await.unwrap();
		}
		let script = log.writes.lock().unwrap().clone();

		let origin = crate::origin::Config::new(us).produce();
		let session = crate::lite::test_transport::ScriptedSession::new(script);
		let (tasks, _task_set) = crate::util::TaskSet::new();
		let peer_setup = peer::PeerSetup::default();
		peer_setup.set(peer::Peer {
			cluster: cluster::Peer {
				hop: Some(crate::Hop::new(9).unwrap()),
				cost: None,
			},
			..Default::default()
		});
		let mut subscriber = Subscriber::new(
			crate::time::sim(),
			crate::driver::Transport::new(session.clone()),
			origin,
			Control::new(false),
			None,
			peer_setup,
			us,
			None,
			VERSION,
			tasks,
			Default::default(),
		);
		subscriber.announces = crate::session::Slots::new(1);

		let prefix = crate::model::interest_prefixes(&subscriber.origin.allowed())
			.pop()
			.expect("one prefix");
		let (_live, live) = live();
		let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), VERSION)
			.await
			.unwrap();
		let mut run =
			std::pin::pin!(subscriber.run_subscribe_namespace(stream, prefix, crate::model::Snapshot::landed(), live));
		let mut result = None;
		for _ in 0..100 {
			if let std::task::Poll::Ready(done) = futures::poll!(run.as_mut()) {
				result = Some(done);
				break;
			}
			settle().await;
		}
		assert!(
			matches!(result, Some(Err(Error::TooManyRequests))),
			"the second namespace held at once fit a budget of one: {result:?}",
		);
	}

	/// MoQ Active Count is negotiated, so a REQUEST_OK that breaks the negotiation
	/// either way is the peer's fault: a count we cannot rely on, or one missing where we
	/// would otherwise wait on it forever.
	#[moq_clock::test]
	async fn a_count_the_negotiation_did_not_promise_is_a_violation() {
		const VERSION: Version = Version::Draft18;

		for (negotiated, count) in [(true, None), (false, Some(0))] {
			let log = crate::lite::test_transport::Log::default();
			let mut writer = crate::coding::Writer::new(
				crate::transport::boxed::SendStream::new(crate::lite::test_transport::SinkSend::new(log.clone())),
				VERSION,
			);
			writer.varint(ietf::RequestOk::ID).await.unwrap();
			writer
				.encode(&ietf::RequestOk {
					largest: None,
					active: count,
				})
				.await
				.unwrap();
			let response = log.writes.lock().unwrap().clone();

			let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
			let session = crate::lite::test_transport::ScriptedSession::new(response);
			let (tasks, _task_set) = crate::util::TaskSet::new();
			let peer_setup = peer::PeerSetup::default();
			peer_setup.set(peer::Peer {
				active_count: negotiated,
				..Default::default()
			});
			let mut subscriber = Subscriber::new(
				crate::time::sim(),
				crate::driver::Transport::new(session.clone()),
				origin,
				Control::new(false),
				None,
				peer_setup,
				crate::Hop::new(1).unwrap(),
				None,
				VERSION,
				tasks,
				Default::default(),
			);

			let (_live, live) = live();
			let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), VERSION)
				.await
				.unwrap();
			let res = subscriber
				.run_subscribe_namespace(
					stream,
					crate::Path::from_static(""),
					crate::model::Snapshot::landed(),
					live,
				)
				.await;
			assert!(
				matches!(res, Err(Error::ProtocolViolation)),
				"negotiated {negotiated}, count {count:?}: {res:?}"
			);
		}
	}

	/// The peer's REQUEST_OK carrying `active`, then a NAMESPACE per suffix, or a
	/// NAMESPACE_DONE for one prefixed with `-`.
	async fn counted_response(version: Version, active: Option<u64>, entries: &[&str]) -> Vec<u8> {
		let log = crate::lite::test_transport::Log::default();
		let mut writer = crate::coding::Writer::new(
			crate::transport::boxed::SendStream::new(crate::lite::test_transport::SinkSend::new(log.clone())),
			version,
		);
		writer.varint(ietf::RequestOk::ID).await.unwrap();
		writer.encode(&ietf::RequestOk { largest: None, active }).await.unwrap();
		for entry in entries {
			match entry.strip_prefix('-') {
				Some(suffix) => {
					writer.varint(ietf::NamespaceDone::ID).await.unwrap();
					writer
						.encode(&ietf::NamespaceDone {
							suffix: crate::Path::new(suffix).unwrap(),
						})
						.await
						.unwrap();
				}
				None => {
					writer.varint(ietf::Namespace::ID).await.unwrap();
					writer
						.encode(&ietf::Namespace {
							suffix: crate::Path::new(entry).unwrap(),
							params: None,
						})
						.await
						.unwrap();
				}
			}
		}
		let writes = log.writes.lock().unwrap();
		writes.clone()
	}

	/// A root SUBSCRIBE_NAMESPACE on a fresh origin whose peer replays `session`, holding
	/// the origin's snapshot as the session does.
	fn counted_subscriber(
		version: Version,
		active_count: bool,
		session: crate::lite::test_transport::ScriptedSession,
	) -> (Subscriber, crate::origin::Consumer, crate::model::Snapshot) {
		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let consumer = origin.consume();
		let snapshot = origin.snapshot("");
		let (tasks, _task_set) = crate::util::TaskSet::new();
		let peer_setup = peer::PeerSetup::default();
		peer_setup.set(peer::Peer {
			active_count,
			..Default::default()
		});
		let subscriber = Subscriber::new(
			crate::time::sim(),
			crate::driver::Transport::new(session.clone()),
			origin,
			Control::new(false),
			None,
			peer_setup,
			crate::Hop::new(1).unwrap(),
			None,
			version,
			tasks,
			Default::default(),
		);
		(subscriber, consumer, snapshot)
	}

	/// With MoQ Active Count, the origin syncs once the counted NAMESPACEs are in.
	#[moq_clock::test]
	async fn counted_namespaces_catch_up() {
		const VERSION: Version = Version::Draft18;
		for (entries, synced) in [(&["a"][..], false), (&["a", "b"][..], true)] {
			let session =
				crate::lite::test_transport::ScriptedSession::new(counted_response(VERSION, Some(2), entries).await);
			let (mut subscriber, consumer, snapshot) = counted_subscriber(VERSION, true, session.clone());
			let mut cursor = consumer.announced();
			let (_live, live) = live();
			let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), VERSION)
				.await
				.unwrap();
			let mut run = std::pin::pin!(subscriber.run_subscribe_namespace(
				stream,
				crate::Path::from_static(""),
				snapshot,
				live
			));
			for _ in 0..10 {
				assert!(futures::poll!(run.as_mut()).is_pending());
				settle().await;
			}
			for entry in entries {
				cursor.assert_next_active(*entry);
			}
			assert_eq!(cursor.is_synced(), synced, "{entries:?}");
		}
	}

	/// Without MoQ Active Count, REQUEST_OK is the best boundary there is.
	#[moq_clock::test]
	async fn an_uncounted_answer_catches_up_on_the_ok() {
		const VERSION: Version = Version::Draft18;
		let session = crate::lite::test_transport::ScriptedSession::new(counted_response(VERSION, None, &[]).await);
		let (mut subscriber, consumer, snapshot) = counted_subscriber(VERSION, false, session.clone());
		let cursor = consumer.announced();
		assert!(!cursor.is_synced());
		let (_live, live) = live();
		let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), VERSION)
			.await
			.unwrap();
		let mut run =
			std::pin::pin!(subscriber.run_subscribe_namespace(stream, crate::Path::from_static(""), snapshot, live));
		for _ in 0..10 {
			assert!(futures::poll!(run.as_mut()).is_pending());
			settle().await;
		}
		assert!(cursor.is_synced());
	}

	/// A counted set cut short, by a NAMESPACE_DONE or the stream ending, is the peer's fault.
	#[moq_clock::test]
	async fn a_counted_set_cut_short_is_a_violation() {
		const VERSION: Version = Version::Draft18;
		for (entries, fin) in [(&["a", "-a"][..], false), (&["a"][..], true)] {
			let session =
				crate::lite::test_transport::ScriptedSession::new(counted_response(VERSION, Some(2), entries).await);
			if fin {
				session.close(crate::lite::test_transport::Close::Fin);
			}
			let (mut subscriber, _consumer, snapshot) = counted_subscriber(VERSION, true, session.clone());
			let (_live, live) = live();
			let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), VERSION)
				.await
				.unwrap();
			let res = subscriber
				.run_subscribe_namespace(stream, crate::Path::from_static(""), snapshot, live)
				.await;
			assert!(matches!(res, Err(Error::ProtocolViolation)), "{entries:?}: {res:?}");
		}
	}

	#[test]
	fn retiring_old_track_does_not_retire_reused_alias() {
		let aliases = TrackAliases::default();
		insert_track_alias(&aliases, 7, RequestId(11)).unwrap();
		retire_track_alias(&aliases, 7, RequestId(13));

		assert_eq!(aliases.read().map.get(&7), Some(&Alias::Active(RequestId(11))));
	}

	/// A cancelled subscription leaves its alias behind, so the groups the publisher is
	/// still sending are discarded at once instead of stalling out the timeout and being
	/// reported as unknown.
	#[moq_clock::test]
	async fn retired_alias_drops_late_groups_immediately() {
		let aliases = TrackAliases::default();
		insert_track_alias(&aliases, 7, RequestId(11)).unwrap();
		retire_track_alias(&aliases, 7, RequestId(11));

		let runtime = crate::time::sim();
		let resolve = resolve_track_alias(&runtime, aliases.consume(), 7);
		let mut resolve = std::pin::pin!(resolve);

		assert!(
			matches!(poll!(&mut resolve), std::task::Poll::Ready(Err(Error::Cancel))),
			"a retired alias must resolve without waiting on the timeout",
		);
	}

	/// A group arriving for a retired alias is the expected tail of our own cancellation, so
	/// the code it maps to has to say so. moq-lite's cancel encodes to 0, which on this wire
	/// is an internal failure, and reporting one to a publisher for a routine unsubscribe is
	/// what distorts its error handling.
	///
	/// Covers the error this path produces and the code it maps to, not the dispatch loop
	/// that sends it. `session::a_group_for_a_retired_alias_is_stopped_with_cancelled`
	/// drives that loop over a real receive stream.
	#[moq_clock::test]
	async fn a_retired_alias_maps_to_the_cancelled_code() {
		let aliases = TrackAliases::default();
		insert_track_alias(&aliases, 7, RequestId(11)).unwrap();
		retire_track_alias(&aliases, 7, RequestId(11));

		let err = resolve_track_alias(&crate::time::sim(), aliases.consume(), 7)
			.await
			.expect_err("a retired alias resolves to a cancellation");

		assert_eq!(
			crate::ietf::error::to_stream_code(&crate::StreamError::from(&err)),
			crate::ietf::error::CANCELLED,
			"the code the dispatch loop maps this error onto",
		);
	}

	/// The publisher may point a retired alias at a new track, so a later SUBSCRIBE_OK
	/// reclaims it rather than colliding with the tombstone.
	#[test]
	fn subscribe_ok_reclaims_a_retired_alias() {
		let aliases = TrackAliases::default();
		insert_track_alias(&aliases, 7, RequestId(11)).unwrap();
		retire_track_alias(&aliases, 7, RequestId(11));

		insert_track_alias(&aliases, 7, RequestId(13)).unwrap();

		assert_eq!(aliases.read().map.get(&7), Some(&Alias::Active(RequestId(13))));
		assert!(
			aliases.read().retired.is_empty(),
			"reclaiming an alias must drop its tombstone",
		);
	}

	/// An alias still serving a live subscription is not a tombstone, so a publisher
	/// pointing it at a second track is the duplicate the draft makes fatal.
	#[test]
	fn active_alias_rejects_a_second_track() {
		let aliases = TrackAliases::default();
		insert_track_alias(&aliases, 7, RequestId(11)).unwrap();

		assert!(matches!(
			insert_track_alias(&aliases, 7, RequestId(13)),
			Err(Error::Duplicate)
		));
	}

	/// Build a subscriber with `subscribes` pre-populated, so alias binding can be
	/// exercised without driving a whole SUBSCRIBE exchange.
	fn subscriber_with_tracks(tracks: &[(RequestId, &str, &str)]) -> Subscriber {
		let (tasks, task_set) = crate::util::TaskSet::new();
		// The tests drive binding directly, so nothing spawns; leaking keeps the handle alive
		// without a spawner.
		std::mem::forget(task_set);

		let subscriber = Subscriber::new(
			crate::time::sim(),
			crate::driver::Transport::new(crate::lite::test_transport::SinkSession::new(Default::default())),
			crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce(),
			Control::new(false),
			None,
			peer::PeerSetup::default(),
			crate::Hop::new(1).unwrap(),
			None,
			Version::Draft18,
			tasks,
			Default::default(),
		);

		{
			let mut state = subscriber.state.lock();
			for (request_id, broadcast, name) in tracks {
				state.subscribes.insert(
					*request_id,
					TrackState::new(
						track::Producer::new(
							std::sync::Arc::new(crate::broadcast::Info::default()),
							*name,
							crate::track::Info::timed(),
						),
						Path::new(broadcast).unwrap(),
						kio::Producer::new(Fill::Done),
					),
				);
			}
		}

		subscriber
	}

	/// Draft-22 section 3.1.3 lets a publisher give several subscriptions to one track the
	/// same alias. Our filters are all LargestObject, so we cannot re-apply them to tell the
	/// groups apart, but that is one subscription's problem. Killing the session over a
	/// legal choice would take every other broadcast down with it.
	#[moq_clock::test]
	async fn a_shared_alias_for_one_track_costs_only_that_subscription() {
		let subscriber = subscriber_with_tracks(&[(RequestId(11), "cam", "video"), (RequestId(13), "cam", "video")]);

		subscriber.register_alias(RequestId(11), 7).unwrap();

		assert!(
			matches!(subscriber.register_alias(RequestId(13), 7), Err(Error::Unsupported)),
			"a shared alias must not be reported as the fatal collision",
		);
	}

	/// Relays keep datagram Object IDs, while plain readers see only normal object 0.
	/// An unbound alias is dropped, and a malformed datagram is the peer's violation.
	#[moq_clock::test]
	async fn an_object_datagram_is_a_datagram_group() {
		use crate::coding::Encode as _;
		use futures::FutureExt as _;

		let subscriber = subscriber_with_tracks(&[(RequestId(11), "cam", "audio")]);
		subscriber.register_alias(RequestId(11), 7).unwrap();
		let (mut consumer, mut relay) = {
			let mut state = subscriber.state.lock();
			let track = state.subscribes.get_mut(&RequestId(11)).unwrap();
			track.timescale = Some(Timescale::MILLI);
			let producer = track.producer.as_ref().unwrap();
			(producer.subscribe(None), producer.subscribe(None))
		};

		let timestamp = crate::Timestamp::new(96_000, Timescale::MILLI).unwrap();
		let datagram = |alias: u64, group_id: u64, object_id: Option<u64>, body: ietf::DatagramBody| {
			let mut properties = Vec::new();
			ietf::encode_object_time(
				&mut crate::coding::Encoder::new(&mut properties, Version::Draft18.into()),
				timestamp,
				Timescale::MILLI,
			)
			.unwrap();
			ietf::ObjectDatagram {
				track_alias: alias,
				group_id,
				object_id,
				publisher_priority: None,
				// Only a Normal Object may carry Properties, and a status cannot end the group.
				end_of_group: matches!(body, ietf::DatagramBody::Payload(_)),
				properties: matches!(body, ietf::DatagramBody::Payload(_)).then_some(properties),
				body,
			}
			.encode_bytes(Version::Draft18)
			.unwrap()
		};
		let payload = |bytes: &'static [u8]| ietf::DatagramBody::Payload(bytes::Bytes::from_static(bytes));

		// Nonzero objects survive for relays; plain readers skip them, unbound aliases, and statuses.
		subscriber
			.recv_datagram(datagram(7, 4, Some(1), payload(b"no")))
			.unwrap();
		subscriber.recv_datagram(datagram(8, 4, None, payload(b"no"))).unwrap();
		subscriber
			.recv_datagram(datagram(7, 9, Some(0), payload(b"yes")))
			.unwrap();
		subscriber
			.recv_datagram(datagram(7, 10, None, ietf::DatagramBody::Status(END_OF_TRACK)))
			.unwrap();
		let Poll::Ready(Ok(Some(first))) = relay.poll_recv_stored_datagram(&kio::Waiter::noop()) else {
			panic!("the relay did not receive the object");
		};
		assert_eq!(first.sequence, 4);
		assert_eq!(first.wire.as_ref().unwrap().object_id, Some(1));
		assert_eq!(&first.payload[..], b"no");
		let received = consumer.recv_datagram().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(received.sequence, 9, "the Group ID is the sequence");
		assert_eq!(received.timestamp, Some(timestamp));
		assert_eq!(&received.payload[..], b"yes");
		assert!(
			consumer.recv_datagram().now_or_never().is_none(),
			"only the normal object 0 payload got through"
		);

		// A status datagram cannot end the group.
		let malformed = bytes::Bytes::from_static(&[0x22, 0x07, 0x04, 0x00]);
		assert!(is_protocol_violation(&subscriber.recv_datagram(malformed).unwrap_err()));
	}

	/// A datagram that lands before SUBSCRIBE_OK binds its alias is dropped, not held for the
	/// subscription to replay once it is bound. The reader was already subscribed, so only the
	/// missing alias can have dropped it.
	#[moq_clock::test]
	async fn a_datagram_before_its_alias_is_dropped() {
		use crate::coding::Encode as _;
		use futures::FutureExt as _;

		let subscriber = subscriber_with_tracks(&[(RequestId(11), "cam", "audio")]);
		let mut consumer = {
			let mut state = subscriber.state.lock();
			let track = state.subscribes.get_mut(&RequestId(11)).unwrap();
			track.timescale = Some(Timescale::MILLI);
			track.producer.as_ref().unwrap().subscribe(None)
		};
		let datagram = |group_id: u64| {
			let mut properties = Vec::new();
			ietf::encode_object_time(
				&mut crate::coding::Encoder::new(&mut properties, Version::Draft18.into()),
				crate::Timestamp::new(96_000, Timescale::MILLI).unwrap(),
				Timescale::MILLI,
			)
			.unwrap();
			ietf::ObjectDatagram {
				track_alias: 7,
				group_id,
				object_id: None,
				publisher_priority: None,
				end_of_group: true,
				properties: Some(properties),
				body: ietf::DatagramBody::Payload(bytes::Bytes::from_static(b"d")),
			}
			.encode_bytes(Version::Draft18)
			.unwrap()
		};

		subscriber.recv_datagram(datagram(4)).unwrap();
		subscriber.register_alias(RequestId(11), 7).unwrap();
		subscriber.recv_datagram(datagram(5)).unwrap();

		let received = consumer.recv_datagram().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(received.sequence, 5);
		assert!(
			consumer.recv_datagram().now_or_never().is_none(),
			"group 4 never arrives"
		);
	}

	/// One alias naming two different tracks is the collision section 11.1 makes fatal.
	#[moq_clock::test]
	async fn an_alias_reused_for_another_track_is_fatal() {
		let subscriber = subscriber_with_tracks(&[(RequestId(11), "cam", "video"), (RequestId(13), "cam", "audio")]);

		subscriber.register_alias(RequestId(11), 7).unwrap();

		assert!(matches!(
			subscriber.register_alias(RequestId(13), 7),
			Err(Error::Duplicate)
		));
	}

	/// Same track name under a different broadcast is a different full track name, so it is
	/// a collision too.
	#[moq_clock::test]
	async fn an_alias_reused_across_broadcasts_is_fatal() {
		let subscriber = subscriber_with_tracks(&[(RequestId(11), "cam", "video"), (RequestId(13), "screen", "video")]);

		subscriber.register_alias(RequestId(11), 7).unwrap();

		assert!(matches!(
			subscriber.register_alias(RequestId(13), 7),
			Err(Error::Duplicate)
		));
	}

	/// A FIN only says we will send nothing further; it is not a cancellation (draft-22
	/// section 6.4.2.2). A publisher holding an Established subscription keeps serving it
	/// until STOP_SENDING arrives on the direction it writes (section 6.4.2.3),
	/// so a subscriber that only finishes leaves it feeding an alias forever. That is what
	/// turns a routine unsubscribe into an endless "unknown track alias" stream.
	#[moq_clock::test]
	async fn cancelling_a_subscription_stops_the_publisher() {
		for version in [Version::Draft22] {
			let log = cancel_a_subscription(version).await;
			// CANCELLED, not the moq-lite cancel code: 0 on this wire is INTERNAL_ERROR, so a
			// routine unsubscribe would read to the publisher as a fault on our side.
			assert_eq!(
				log.stops(),
				vec![crate::ietf::error::CANCELLED],
				"{version:?}: cancelling must STOP_SENDING the publisher's direction, not just FIN ours",
			);
			assert_ne!(
				crate::ietf::error::CANCELLED,
				crate::SessionError::Cancel.to_code(),
				"the two error spaces disagree; that is why this code is mapped separately",
			);
		}
	}

	/// A rejection and the last consumer leaving can both be ready when the task is next
	/// polled. The publisher destroyed the request when it sent the error, so treating that
	/// as abandonment would cancel a request that no longer exists and name a dead id back at
	/// a peer entitled to object. The answer wins.
	#[moq_clock::test]
	async fn a_ready_rejection_beats_local_abandonment() {
		const VERSION: Version = Version::Draft18;

		// A peer that rejects the subscribe outright.
		let rejection = {
			let log = crate::lite::test_transport::Log::default();
			let mut writer = crate::coding::Writer::new(
				crate::transport::boxed::SendStream::new(crate::lite::test_transport::SinkSend::new(log.clone())),
				VERSION,
			);
			writer.varint(ietf::RequestError::ID).await.unwrap();
			writer
				.encode(&ietf::RequestError {
					// DOES_NOT_EXIST.
					error_code: 0x10,
					reason_phrase: "not found".into(),
					retry_interval: 0,
				})
				.await
				.unwrap();

			log.writes.lock().unwrap().clone()
		};

		let session = crate::lite::test_transport::ScriptedSession::new(rejection);
		let log = session.log.clone();

		let (tasks, _task_set) = crate::util::TaskSet::new();
		let mut subscriber = Subscriber::new(
			crate::time::sim(),
			crate::driver::Transport::new(session.clone()),
			crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce(),
			Control::new(false),
			None,
			peer::PeerSetup::declared(peer::Peer::default()),
			crate::Hop::new(1).unwrap(),
			None,
			VERSION,
			tasks,
			Default::default(),
		);

		let producer = crate::broadcast::Info::default().produce();
		let mut dynamic = producer.requests();
		let consumer = producer.consume();
		let track = consumer.track("video").unwrap();
		let subscription = track.subscribe(None);

		let request = dynamic.requested_track().await.expect("no track requested");

		// Drop the demand before the task runs, so the rejection and the unused wake are both
		// ready the first time the setup race is polled.
		drop(subscription);
		drop(track);
		drop(consumer);

		let serving = moq_clock::sim::spawn(async move {
			subscriber
				.run_subscribe(Path::from_static("broadcast"), dynamic, None, request)
				.await;
		});

		moq_clock::timeout(std::time::Duration::from_secs(1), serving)
			.await
			.expect("run_subscribe did not finish")
			.unwrap();

		assert!(
			log.stops().is_empty(),
			"a rejected request is already gone; there is nothing to cancel",
		);
	}

	/// A subscriber whose session presented a token still unanswered, over a peer that
	/// accepts streams and says nothing.
	fn waiting_on_auth(
		version: Version,
	) -> (
		Subscriber,
		crate::lite::test_transport::Log,
		crate::auth::Link,
		Vec<crate::auth::Credential>,
		crate::util::TaskSet,
	) {
		let session = crate::lite::test_transport::ScriptedSession::new(Vec::new());
		let log = session.log.clone();
		let auth = crate::auth::Link::new(true);
		let setup = auth.present(bytes::Bytes::new(), true).unwrap();
		auth.granted(setup.id(), crate::auth::Grant::default());
		let unanswered = auth.present(bytes::Bytes::from_static(b"new"), false).unwrap();
		let (tasks, task_set) = crate::util::TaskSet::new();
		let subscriber = Subscriber::new(
			crate::time::sim(),
			crate::driver::Transport::new(session.clone()),
			crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce(),
			Control::new(false),
			None,
			peer::PeerSetup::default(),
			crate::Hop::new(1).unwrap(),
			None,
			version,
			tasks,
			Default::default(),
		)
		.with_auth(auth.clone());
		(subscriber, log, auth, vec![setup, unanswered], task_set)
	}

	/// A subscribe whose readers leave while it waits on a token ends, rather than holding
	/// its task and track until a token that may never be answered.
	#[moq_clock::test]
	async fn an_abandoned_subscribe_stops_waiting_for_auth() {
		const VERSION: Version = Version::Draft18;
		let (mut subscriber, log, _auth, _tokens, _task_set) = waiting_on_auth(VERSION);

		let producer = crate::broadcast::Info::default().produce();
		let mut dynamic = producer.requests();
		let consumer = producer.consume();
		let track = consumer.track("video").unwrap();
		let subscription = track.subscribe(None);
		let request = dynamic.requested_track().await.expect("no track requested");

		let mut serving = moq_clock::sim::spawn(async move {
			subscriber
				.run_subscribe(Path::from_static("broadcast"), dynamic, None, request)
				.await;
		});
		assert!(
			moq_clock::timeout(std::time::Duration::from_secs(1), &mut serving)
				.await
				.is_err(),
			"finished while the track was wanted"
		);

		drop(subscription);
		drop(track);
		drop(consumer);
		moq_clock::timeout(std::time::Duration::from_secs(1), serving)
			.await
			.expect("an abandoned subscribe kept waiting on its token")
			.unwrap();
		assert!(
			!control_message_types(&log, VERSION).contains(&ietf::Subscribe::ID),
			"an abandoned subscribe reached the wire",
		);
	}

	/// A subscribe under an announced route that is withdrawn while it waits on a token
	/// ends, judged against the union as it stands, so routing can recover.
	#[moq_clock::test]
	async fn a_withdrawn_route_ends_a_subscribe_waiting_for_auth() {
		const VERSION: Version = Version::Draft18;
		let (mut subscriber, log, _auth, _tokens, _task_set) = waiting_on_auth(VERSION);

		let producer = crate::broadcast::Info::default().produce();
		let mut dynamic = producer.requests();
		let consumer = producer.consume();
		let track = consumer.track("video").unwrap();
		let subscription = track.subscribe(None);
		let request = dynamic.requested_track().await.expect("no track requested");

		let route = kio::Producer::<()>::default();
		let live = route.consume();
		let mut serving = moq_clock::sim::spawn(async move {
			subscriber
				.run_subscribe(Path::from_static("broadcast"), dynamic, Some(live), request)
				.await;
		});
		assert!(
			moq_clock::timeout(std::time::Duration::from_secs(1), &mut serving)
				.await
				.is_err(),
			"finished while the route stood"
		);

		drop(route);
		moq_clock::timeout(std::time::Duration::from_secs(1), serving)
			.await
			.expect("a withdrawn route kept the subscribe waiting on its token")
			.unwrap();
		assert!(
			!control_message_types(&log, VERSION).contains(&ietf::Subscribe::ID),
			"a subscribe outside the union reached the wire",
		);
		drop(subscription);
	}

	/// A publisher can be serving before its SUBSCRIBE_OK arrives, since data streams are
	/// independent of the request stream. If the last consumer leaves in that window, the
	/// subscriber still owes it a cancellation: walking away silently is what leaves it
	/// serving a track nobody reads.
	#[moq_clock::test]
	async fn abandoning_before_subscribe_ok_still_cancels() {
		const VERSION: Version = Version::Draft18;

		// A peer that accepts the stream and then says nothing at all.
		let session = crate::lite::test_transport::ScriptedSession::new(Vec::new());
		let log = session.log.clone();

		let (tasks, _task_set) = crate::util::TaskSet::new();
		let mut subscriber = Subscriber::new(
			crate::time::sim(),
			crate::driver::Transport::new(session.clone()),
			crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce(),
			Control::new(false),
			None,
			peer::PeerSetup::declared(peer::Peer::default()),
			crate::Hop::new(1).unwrap(),
			None,
			VERSION,
			tasks,
			Default::default(),
		);

		let producer = crate::broadcast::Info::default().produce();
		let mut dynamic = producer.requests();
		let consumer = producer.consume();
		let track = consumer.track("video").unwrap();
		let subscription = track.subscribe(None);

		let request = dynamic.requested_track().await.expect("no track requested");

		let serving = moq_clock::sim::spawn(async move {
			subscriber
				.run_subscribe(Path::from_static("broadcast"), dynamic, None, request)
				.await;
		});

		// Let the SUBSCRIBE go out. No SUBSCRIBE_OK is coming, so the subscription never
		// reaches Established on our side.
		settle().await;

		drop(subscription);
		drop(track);
		drop(consumer);

		moq_clock::timeout(std::time::Duration::from_secs(1), serving)
			.await
			.expect("run_subscribe parked waiting for a response that never came")
			.unwrap();

		assert_eq!(
			log.stops(),
			vec![crate::ietf::error::CANCELLED],
			"a subscribe abandoned before SUBSCRIBE_OK must still be cancelled",
		);
	}

	/// A subscriber whose grant from the peer covers everything until `auth.ended(0, ..)`
	/// revokes it, with the token handle to hold meanwhile.
	fn revocable_auth() -> (crate::auth::Link, crate::auth::Credential) {
		let auth = crate::auth::Link::new(true);
		let cred = auth.present(bytes::Bytes::from_static(b"cred"), true).unwrap();
		auth.granted(0, crate::auth::Grant::all());
		(auth, cred)
	}

	/// A grant revoked while the SUBSCRIBE waits for its answer refuses the request and
	/// cancels the subscribe at once, not once the peer answers or acknowledges the cancel.
	#[moq_clock::test]
	async fn a_revocation_never_waits_behind_subscribe_ok() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			// A peer that accepts the stream and then says nothing at all, not even
			// acknowledging the cancellation's FIN.
			let session = crate::lite::test_transport::ScriptedSession::new(Vec::new()).with_unacked_fin();
			let log = session.log.clone();
			let (tasks, _task_set) = crate::util::TaskSet::new();
			let (auth, _cred) = revocable_auth();
			let mut subscriber = Subscriber::new(
				crate::time::sim(),
				crate::driver::Transport::new(session.clone()),
				crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce(),
				Control::new(false),
				None,
				peer::PeerSetup::declared(peer::Peer::default()),
				crate::Hop::new(1).unwrap(),
				None,
				version,
				tasks,
				Default::default(),
			)
			.with_auth(auth.clone());

			let producer = crate::broadcast::Info::default().produce();
			let mut dynamic = producer.requests();
			let consumer = producer.consume();
			let track = consumer.track("video").unwrap();
			let subscription = track.subscribe(None);
			let request = dynamic.requested_track().await.expect("no track requested");

			let serving = moq_clock::sim::spawn(async move {
				subscriber
					.run_subscribe(Path::from_static("broadcast"), dynamic, None, request)
					.await;
			});
			// Let the SUBSCRIBE go out. No SUBSCRIBE_OK is coming.
			settle().await;
			assert!(occurrences(&log, &[ietf::Subscribe::ID as u8]) > 0, "{version}");

			auth.ended(0, Error::Session(SessionError::Unauthorized));
			let result = moq_clock::timeout(Duration::from_secs(1), subscription)
				.await
				.unwrap_or_else(|_| panic!("{version}: the revocation waited behind SUBSCRIBE_OK"));
			assert!(
				matches!(result, Err(Error::Unauthorized)),
				"{version}: the request outlived its grant: {:?}",
				result.err()
			);
			// Only the wire cleanup still waits on the peer's acknowledgement.
			settle().await;
			drop(serving);
			// A subscribe revoked before SUBSCRIBE_OK must still be cancelled, by stopping the
			// publisher's direction.
			assert_eq!(log.stops(), vec![crate::ietf::error::CANCELLED], "{version}");
			assert!(log.closes().is_empty(), "{version}: the session stays up");
		}
	}

	/// An unanswered epochless request cannot prove which instance the peer resolved.
	/// Retiring its source cancels it even while the caller keeps its demand.
	#[moq_clock::test]
	async fn a_retirement_before_subscribe_ok_refuses_the_subscription() {
		const VERSION: Version = Version::Draft18;

		// A peer that accepts the stream and then says nothing at all.
		let session = crate::lite::test_transport::ScriptedSession::new(Vec::new());
		let log = session.log.clone();

		let (tasks, _task_set) = crate::util::TaskSet::new();
		let mut subscriber = Subscriber::new(
			crate::time::sim(),
			crate::driver::Transport::new(session.clone()),
			crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce(),
			Control::new(false),
			None,
			peer::PeerSetup::declared(peer::Peer::default()),
			crate::Hop::new(1).unwrap(),
			None,
			VERSION,
			tasks,
			Default::default(),
		);

		let route = kio::Producer::new(());
		subscriber.validity = crate::model::broadcast::RequestValidity::token(Some(route.consume()));

		let producer = crate::broadcast::Info::default().produce();
		let mut dynamic = producer.requests();
		let consumer = producer.consume();
		let track = consumer.track("video").unwrap();
		let subscription = track.subscribe(None);

		let request = dynamic.requested_track().await.expect("no track requested");

		let serving = moq_clock::sim::spawn(async move {
			subscriber
				.run_subscribe(Path::from_static("broadcast"), dynamic, None, request)
				.await;
		});

		// Let the SUBSCRIBE go out, then retract the broadcast before any response.
		settle().await;
		drop(route);
		serving.await.unwrap();
		assert!(matches!(subscription.await, Err(Error::Unroutable)));
		assert_eq!(log.stops(), vec![crate::ietf::error::CANCELLED]);
	}

	#[derive(Clone, Copy, Debug)]
	enum SetupWait {
		SubscribeOpen,
		/// The request's first message, stalled part-way on send credit.
		SubscribeWrite,
		StatusOpen,
		StatusWrite,
		StatusResponse,
		FetchOpen,
		FetchWrite,
		FetchResponse,
	}

	#[derive(Clone, Copy, Debug)]
	enum Invalidated {
		Retired,
		Revoked,
		Narrowed,
		Expired,
	}

	/// Drive each setup wait without an open stream or an answer. Refusal must be local even
	/// when the peer never acknowledges the cancellation's FIN.
	async fn invalid_setup(stage: SetupWait, version: Version, cause: Invalidated) {
		let opening = matches!(
			stage,
			SetupWait::SubscribeOpen | SetupWait::StatusOpen | SetupWait::FetchOpen
		);
		let writing = matches!(
			stage,
			SetupWait::SubscribeWrite | SetupWait::StatusWrite | SetupWait::FetchWrite
		);
		let open = kio::Producer::new(false);
		let session = if opening {
			crate::lite::test_transport::ScriptedSession::gated_open(Vec::new(), open.consume())
		} else if writing {
			crate::lite::test_transport::ScriptedSession::new(Vec::new()).with_send_window(1)
		} else {
			crate::lite::test_transport::ScriptedSession::new(Vec::new()).with_unacked_fin()
		};
		let log = session.log.clone();
		let (tasks, _task_set) = TaskSet::new();
		let (auth, _cred) = revocable_auth();
		let control = Control::new(false);
		let mut subscriber = Subscriber::new(
			crate::time::sim(),
			crate::driver::Transport::new(session.clone()),
			crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce(),
			control.clone(),
			None,
			peer::PeerSetup::declared(peer::Peer::default()),
			crate::Hop::new(1).unwrap(),
			None,
			version,
			tasks,
			Default::default(),
		)
		.with_auth(auth.clone());
		let route = kio::Producer::new(());
		subscriber.validity = crate::model::broadcast::RequestValidity::token(Some(route.consume()));
		let mut gate = crate::auth::Gate::new(
			auth.clone(),
			Path::from_static("broadcast"),
			crate::auth::Direction::Subscribe,
		);
		let waiter = kio::Waiter::noop();
		assert!(gate.poll_ready(&waiter).is_ready());
		assert!(gate.poll_denied(&waiter).is_pending());

		let producer = crate::broadcast::Info::default().produce();
		let mut dynamic = producer.requests();
		let consumer = producer.consume();
		let track = consumer.track("video").unwrap();
		let mut subscription = Box::pin(track.subscribe(None));
		let request = dynamic.requested_track().await.expect("track request");
		let mut group = None;
		let mut accepted = None;
		let work: std::pin::Pin<Box<dyn std::future::Future<Output = ()>>> = match stage {
			SetupWait::SubscribeOpen | SetupWait::SubscribeWrite => Box::pin(async {
				assert!(
					subscriber
						.subscribe_once(
							&Path::from_static("broadcast"),
							"video",
							Target::Request(request),
							&mut TaskSet::owned(),
							&mut gate
						)
						.await
						.is_none()
				);
			}),
			SetupWait::StatusOpen | SetupWait::StatusWrite | SetupWait::StatusResponse => Box::pin(async {
				assert!(
					subscriber
						.track_status(&Path::from_static("broadcast"), "video", request, &mut gate)
						.await
						.is_none()
				);
			}),
			_ => {
				let track = request.accept(track::Info::default().with_timescale(None));
				let dynamic = track.requests();
				let mut pending = Box::pin(track.consume().fetch_group(0, None));
				assert!(futures::poll!(pending.as_mut()).is_pending());
				let request = dynamic.requested_fill().await.expect("group request");
				group = Some(pending);
				accepted = Some(track);
				Box::pin(subscriber.clone().run_group_fetch(
					Path::from_static("broadcast"),
					"video".into(),
					request,
					None,
				))
			}
		};
		let mut work = work;
		assert!(futures::poll!(work.as_mut()).is_pending(), "{stage:?}/{version}");
		if opening {
			assert!(log.writes.lock().unwrap().is_empty());
		}
		if writing {
			assert_eq!(
				log.writes.lock().unwrap().len(),
				1,
				"{stage:?}/{version}: stalled mid-message"
			);
		}
		let expected = match cause {
			Invalidated::Retired => {
				drop(route);
				Error::Unroutable
			}
			Invalidated::Revoked => {
				auth.ended(0, Error::Unauthorized);
				Error::Unauthorized
			}
			Invalidated::Narrowed => {
				auth.handle().authorize(&crate::auth::Grant::default());
				Error::Unauthorized
			}
			// The session's AUTH handler reports expiry by ending the peer's grant.
			Invalidated::Expired => {
				auth.ended(0, Error::Timeout);
				Error::Unauthorized
			}
		};
		if let Some(track) = accepted.as_ref()
			&& !matches!(cause, Invalidated::Retired)
		{
			// The owning subscribe/linger loop observes this same gate and aborts
			// the copy before polling its standalone fetch jobs.
			let _ = track.clone().abort(expected.clone());
		}
		let done = futures::poll!(work.as_mut()).is_ready();
		let result = match group.as_mut() {
			Some(group) => futures::poll!(group.as_mut()).map(|res| res.map(|_| ())),
			None => futures::poll!(subscription.as_mut()).map(|res| res.map(|_| ())),
		};
		assert!(
			matches!(
				(&result, &expected),
				(Poll::Ready(Err(Error::Unroutable)), Error::Unroutable)
					| (Poll::Ready(Err(Error::Unauthorized)), Error::Unauthorized)
			),
			"{stage:?}/{version}/{cause:?}: {result:?}"
		);
		if opening {
			assert!(done, "{stage:?}/{version}/{cause:?}: cancelled wait stayed alive");
			assert!(log.writes.lock().unwrap().is_empty());
		} else if writing {
			assert!(done, "{stage:?}/{version}/{cause:?}: cancelled write stayed alive");
			// A FIN would hand the peer a truncated message.
			assert_eq!(
				log.resets(),
				vec![crate::ietf::error::CANCELLED],
				"{stage:?}/{version}/{cause:?}"
			);
			assert!(
				!log.trail()
					.iter()
					.any(|(_, sent)| matches!(sent, crate::lite::test_transport::Sent::Finish)),
				"{stage:?}/{version}/{cause:?}: finished a truncated request"
			);
			assert_eq!(
				log.stops(),
				vec![crate::ietf::error::CANCELLED],
				"{stage:?}/{version}/{cause:?}"
			);
		} else {
			assert_eq!(
				log.stops(),
				vec![crate::ietf::error::CANCELLED],
				"{stage:?}/{version}/{cause:?}"
			);
		}
		drop(work);
		if opening {
			assert_eq!(control.next_request_id(), RequestId(1), "cancelled open spends no ID");
		}
		assert!(log.closes().is_empty(), "the session stays up");
	}

	macro_rules! setup_wait_test {
		($name:ident, $stage:ident, [$($version:ident),+]) => {
			#[moq_clock::test]
			async fn $name() {
				for version in [$(Version::$version),+] {
					for cause in [Invalidated::Retired, Invalidated::Revoked, Invalidated::Narrowed, Invalidated::Expired] {
						invalid_setup(SetupWait::$stage, version, cause).await;
					}
				}
			}
		};
	}

	setup_wait_test!(invalid_subscribe_open_is_refused, SubscribeOpen, [Draft22]);
	setup_wait_test!(invalid_subscribe_write_is_refused, SubscribeWrite, [Draft18, Draft22]);
	setup_wait_test!(invalid_status_open_is_refused, StatusOpen, [Draft22]);
	setup_wait_test!(invalid_status_write_is_refused, StatusWrite, [Draft18, Draft22]);
	setup_wait_test!(invalid_status_response_is_refused, StatusResponse, [Draft22]);
	setup_wait_test!(invalid_fetch_open_is_refused, FetchOpen, [Draft22]);
	setup_wait_test!(invalid_fetch_write_is_refused, FetchWrite, [Draft18, Draft22]);
	setup_wait_test!(invalid_fetch_response_is_refused, FetchResponse, [Draft22]);

	/// The control messages that actually reached the wire, by type id.
	///
	/// Decoding the framing rather than scanning for a byte: a type id is one varint among
	/// many, and a substring match would happily find one inside a length or a payload.
	fn control_message_types(log: &crate::lite::test_transport::Log, version: Version) -> Vec<u64> {
		let writes = log.writes.lock().unwrap().clone();
		let mut buf = Decoder::new(&writes, version.into());
		let mut types = Vec::new();

		while !buf.is_empty() {
			let Ok(type_id) = buf.varint() else {
				break;
			};
			let Ok(size) = buf.u16() else {
				break;
			};
			if buf.slice(size as usize).is_err() {
				break;
			}
			types.push(type_id);
		}

		types
	}

	/// Establish a subscription on `version`, then drop its last consumer, and hand back
	/// what the session recorded on the way out.
	async fn cancel_a_subscription(version: Version) -> crate::lite::test_transport::Log {
		cancel_a_subscription_inner(version, false).await
	}

	/// Writing the UNSUBSCRIBE is not the same as delivering it. A stream that has only been
	/// finished is still retransmitting, so the writer's Drop reset would discard the message
	/// before the peer read it. Closing consumes the writer, which is what removes that
	/// fallback, so a reset here means the cancellation never landed.
	#[moq_clock::test]
	async fn cancelling_does_not_reset_away_the_unsubscribe() {
		for version in [Version::Draft22] {
			let log = cancel_a_subscription(version).await;
			assert!(
				log.resets().is_empty(),
				"{version:?}: the send side must be closed, not reset out from under the cancellation",
			);
		}
	}

	/// Run a draft-22 subscription to Established over a peer whose request stream takes only
	/// `window` bytes. Returns the session, the running task, the track, and its first reader.
	async fn update_under_window(
		window: Option<usize>,
	) -> (
		crate::lite::test_transport::ScriptedSession,
		moq_clock::sim::JoinHandle<()>,
		crate::track::Consumer,
		kio::Pending<track::Subscribing>,
	) {
		let version = Version::Draft22;
		let subscribe_ok = message(
			ietf::SubscribeOk::ID,
			&ietf::SubscribeOk {
				track_alias: 7,
				largest: None,
				properties: Default::default(),
			},
			version,
		);
		let mut session = crate::lite::test_transport::ScriptedSession::new(subscribe_ok);
		if let Some(window) = window {
			session = session.with_send_window(window);
		}
		let (tasks, task_set) = crate::util::TaskSet::new();
		std::mem::forget(task_set);
		let mut subscriber = Subscriber::new(
			crate::time::sim(),
			crate::driver::Transport::new(session.clone()),
			crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce(),
			Control::new(false),
			None,
			peer::PeerSetup::declared(peer::Peer::default()),
			crate::Hop::new(1).unwrap(),
			None,
			version,
			tasks,
			Default::default(),
		);
		let producer = crate::broadcast::Info::default().produce();
		let mut dynamic = producer.requests();
		let track = producer.consume().track("video").unwrap();
		let first = track.subscribe(None);
		let request = dynamic.requested_track().await.expect("no track requested");
		let serving = moq_clock::sim::spawn(async move {
			let _producer = producer;
			subscriber
				.run_subscribe(Path::from_static("broadcast"), dynamic, None, request)
				.await;
		});
		settle().await;
		(session, serving, track, first)
	}

	/// A REQUEST_UPDATE the peer's send window holds back part-way is still buffered when
	/// PUBLISH_DONE ends the subscription, so the stream resets rather than FIN a truncated
	/// update.
	#[moq_clock::test]
	async fn a_stalled_update_resets_when_the_publisher_ends() {
		// The SUBSCRIBE alone, measured over an open window.
		let subscribe = {
			let (session, serving, _track, _first) = update_under_window(None).await;
			serving.abort();
			session.log.writes.lock().unwrap().len()
		};

		let (session, serving, track, _first) = update_under_window(Some(subscribe + 1)).await;
		let _raised = track
			.subscribe(track::Subscription {
				priority: 5,
				..Default::default()
			})
			.await
			.unwrap();
		settle().await;
		assert_eq!(
			session.log.writes.lock().unwrap().len(),
			subscribe + 1,
			"the update stalled mid-message"
		);

		session.push(&message(
			ietf::PublishDone::ID,
			&ietf::PublishDone {
				status_code: 0x2,
				stream_count: 0,
				reason_phrase: "done".into(),
			},
			Version::Draft22,
		));
		// The script push wakes nothing, so a subscription change re-polls the loop.
		let _again = track
			.subscribe(track::Subscription {
				priority: 7,
				..Default::default()
			})
			.await
			.unwrap();
		// Past the grace PUBLISH_DONE waits out for its data streams; the copy then lingers.
		moq_clock::sleep(crate::ledger::GRACE * 2).await;
		assert_eq!(session.log.resets(), vec![crate::ietf::error::CANCELLED]);
		assert!(
			!session
				.log
				.trail()
				.iter()
				.any(|(_, sent)| matches!(sent, crate::lite::test_transport::Sent::Finish)),
			"finished a truncated update"
		);
		serving.abort();
	}

	/// When `conflict` is set, an alias-7 binding for the same full track name is seeded
	/// first, so the subscription under test loses the race to bind it.
	async fn cancel_a_subscription_inner(version: Version, conflict: bool) -> crate::lite::test_transport::Log {
		// A peer that accepts the subscription, binding alias 7, then says nothing more.
		let subscribe_ok = {
			let log = crate::lite::test_transport::Log::default();
			let mut writer = crate::coding::Writer::new(
				crate::transport::boxed::SendStream::new(crate::lite::test_transport::SinkSend::new(log.clone())),
				version,
			);
			writer.varint(ietf::SubscribeOk::ID).await.unwrap();
			writer
				.encode(&ietf::SubscribeOk {
					track_alias: 7,
					largest: None,
					properties: Default::default(),
				})
				.await
				.unwrap();

			log.writes.lock().unwrap().clone()
		};

		let session = crate::lite::test_transport::ScriptedSession::new(subscribe_ok);
		let log = session.log.clone();

		let (tasks, _task_set) = crate::util::TaskSet::new();
		let mut subscriber = Subscriber::new(
			crate::time::sim(),
			crate::driver::Transport::new(session.clone()),
			crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce(),
			Control::new(false),
			None,
			peer::PeerSetup::declared(peer::Peer::default()),
			crate::Hop::new(1).unwrap(),
			None,
			version,
			tasks,
			Default::default(),
		);

		if conflict {
			let holder = RequestId(999);
			let mut state = subscriber.state.lock();
			state.subscribes.insert(
				holder,
				TrackState {
					alias: Some(7),
					..TrackState::new(
						track::Producer::new(
							std::sync::Arc::new(crate::broadcast::Info::default()),
							"video",
							crate::track::Info::timed(),
						),
						Path::from_static("broadcast"),
						kio::Producer::new(Fill::Done),
					)
				},
			);
			insert_track_alias(&state.aliases, 7, holder).unwrap();
		}

		// A consumer asking for a track is what dispatches a request to the session.
		let producer = crate::broadcast::Info::default().produce();
		let mut dynamic = producer.requests();
		let consumer = producer.consume();
		let track = consumer.track("video").unwrap();
		let subscription = track.subscribe(None);

		let request = dynamic.requested_track().await.expect("no track requested");

		// A handle on the same state the spawned task mutates, so the test can prove the
		// subscription reached Established rather than assume it.
		let probe = subscriber.clone();

		let serving = moq_clock::sim::spawn(async move {
			subscriber
				.run_subscribe(Path::from_static("broadcast"), dynamic, None, request)
				.await;
		});

		// Let the SUBSCRIBE go out and the SUBSCRIBE_OK come back, so the subscription is
		// Established when we walk away from it.
		settle().await;

		// Without this the test would still pass if SUBSCRIBE_OK never landed, and it would
		// then be asserting against a subscription that was never established.
		assert!(
			matches!(probe.state.lock().aliases.read().map.get(&7), Some(Alias::Active(_))),
			"{version:?}: alias 7 must be bound before we cancel",
		);

		// The last consumer leaves: nothing wants this track any more. The upstream
		// subscription is cancelled at once, and the copy lingers for a returning reader.
		drop(subscription);
		drop(track);
		drop(consumer);

		moq_clock::timeout(crate::track::IDLE_LINGER * 2, serving)
			.await
			.expect("run_subscribe did not finish")
			.unwrap();

		log
	}

	/// Establish a draft-22 subscription against a SUBSCRIBE_OK carrying `largest`, and
	/// report whether a fill is still outstanding once it is accepted.
	async fn fill_after_subscribe_ok(largest: Option<ietf::Location>, priority: Option<u8>) -> bool {
		let version = Version::Draft22;

		let subscribe_ok = {
			let log = crate::lite::test_transport::Log::default();
			let mut writer = crate::coding::Writer::new(
				crate::transport::boxed::SendStream::new(crate::lite::test_transport::SinkSend::new(log.clone())),
				version,
			);
			writer.varint(ietf::SubscribeOk::ID).await.unwrap();
			writer
				.encode(&ietf::SubscribeOk {
					track_alias: 7,
					largest,
					properties: ietf::Properties {
						priority,
						..Default::default()
					},
				})
				.await
				.unwrap();

			log.writes.lock().unwrap().clone()
		};

		let session = crate::lite::test_transport::ScriptedSession::new(subscribe_ok);
		let (tasks, _task_set) = crate::util::TaskSet::new();
		let mut subscriber = Subscriber::new(
			crate::time::sim(),
			crate::driver::Transport::new(session.clone()),
			crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce(),
			Control::new(false),
			None,
			peer::PeerSetup::declared(peer::Peer::default()),
			crate::Hop::new(1).unwrap(),
			None,
			version,
			tasks,
			Default::default(),
		);

		let producer = crate::broadcast::Info::default().produce();
		let mut dynamic = producer.requests();
		let consumer = producer.consume();
		let track = consumer.track("video").unwrap();
		let _subscription = track.subscribe(None);
		let request = dynamic.requested_track().await.expect("no track requested");

		let probe = subscriber.clone();
		let serving = moq_clock::sim::spawn(async move {
			subscriber
				.run_subscribe(Path::from_static("broadcast"), dynamic, None, request)
				.await;
		});

		settle().await;

		let outstanding = {
			let state = probe.state.lock();
			let track = state
				.subscribes
				.values()
				.next()
				.expect("the subscription is registered");
			assert_eq!(
				track.producer.as_ref().unwrap().publisher_priority(),
				// An omitted priority is the drafts' 128, one step below the unset 127.
				priority.map_or(-1, super::super::priority::from_wire),
				"SUBSCRIBE_OK priority must be committed before alias registration"
			);
			// The answer's largest is known before any group arrives.
			assert_eq!(
				track.producer.as_ref().unwrap().largest(),
				largest.map(|largest| track::Position {
					group: largest.group,
					frame: 0,
				}),
				"only the group carries over, since Object IDs are not frame ordinals"
			);
			let fill = track.fill.read();
			fill.outstanding()
		};

		serving.abort();
		outstanding
	}

	/// A control message as the peer writes it: type id, then the length-prefixed body.
	fn message<'m, M: Message<'m>>(id: u64, msg: &M, version: Version) -> Vec<u8> {
		use crate::coding::Encode as _;
		let mut buf = Vec::new();
		crate::coding::Encoder::new(&mut buf, version.into())
			.varint(id)
			.unwrap();
		msg.encode(&mut crate::coding::Encoder::new(&mut buf, version.into()), version)
			.unwrap();
		buf
	}

	/// Serve the front's request on its wake, before the requesting call can return.
	/// A peer that refuses TRACK_STATUS must still receive SUBSCRIBE for a catalog reader.
	#[moq_clock::test]
	async fn a_catalog_subscription_never_requests_track_status() {
		use std::sync::{Arc, Mutex};
		use std::task::{Context, Wake, Waker};

		struct Peer {
			version: Version,
			dynamic: Mutex<Option<broadcast::Requests>>,
			park: Mutex<kio::Park>,
			log: Mutex<Option<crate::lite::test_transport::Log>>,
		}
		impl Wake for Peer {
			fn wake(self: Arc<Self>) {
				self.wake_by_ref();
			}
			fn wake_by_ref(self: &Arc<Self>) {
				let (dynamic, request) = {
					let mut dynamic = self.dynamic.lock().unwrap();
					let Some(serving) = dynamic.as_mut() else { return };
					let mut park = self.park.lock().unwrap();
					let waker = Waker::from(self.clone());
					let waiter = park.hold(&Context::from_waker(&waker));
					let Poll::Ready(Ok(request)) = serving.poll_requested_track(waiter) else {
						return;
					};
					*park = kio::Park::default();
					(dynamic.take().unwrap(), request)
				};
				let version = self.version;
				// The subscriber chooses its wire request from this snapshot. Answer a
				// TRACK_STATUS with the undefined code used by the refusing peer.
				let answer = if request.subscription().is_none() {
					message(
						ietf::RequestError::ID,
						&ietf::RequestError {
							error_code: u32::MAX.into(),
							reason_phrase: "TRACK_STATUS is refused".into(),
							retry_interval: 0,
						},
						version,
					)
				} else {
					message(
						ietf::SubscribeOk::ID,
						&ietf::SubscribeOk {
							track_alias: 7,
							largest: None,
							properties: Default::default(),
						},
						version,
					)
				};
				let session = crate::lite::test_transport::ScriptedSession::new(answer);
				*self.log.lock().unwrap() = Some(session.log.clone());
				let mut serving = Box::pin(async move {
					let (tasks, _task_set) = crate::util::TaskSet::new();
					let mut subscriber = Subscriber::new(
						crate::time::sim(),
						crate::driver::Transport::new(session.clone()),
						crate::origin::Config::new(crate::Hop::new(2).unwrap()).produce(),
						Control::new(false),
						None,
						peer::PeerSetup::declared(peer::Peer::default()),
						crate::Hop::new(2).unwrap(),
						None,
						version,
						tasks,
						Default::default(),
					);
					subscriber
						.run_subscribe(Path::from_static("broadcast"), dynamic, None, request)
						.await;
				});
				// Run the request classification before the front can add demand after
				// its query. The simulator takes over only once the wire work parks.
				if std::future::Future::poll(serving.as_mut(), &mut Context::from_waker(Waker::noop())).is_pending() {
					drop(moq_clock::sim::spawn(serving));
				}
			}
		}

		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
			let source = origin.publish("broadcast", Default::default()).unwrap();
			let resolved = origin.consume().request("broadcast", None).await.unwrap();
			let peer = Arc::new(Peer {
				version,
				dynamic: Mutex::new(Some(source.requests())),
				park: Default::default(),
				log: Default::default(),
			});
			peer.wake_by_ref();
			let result = resolved.track("catalog").unwrap().subscribe(None).await;
			let log = peer.log.lock().unwrap();
			assert_eq!(
				control_message_types(log.as_ref().expect("the front asked its source"), version),
				[ietf::Subscribe::ID],
				"{version}: a subscribing reader sent TRACK_STATUS"
			);
			assert!(result.is_ok(), "{version}: the catalog reader was refused");
		}
	}

	/// Query-only and fetch-only demand fail with the peer's TRACK_STATUS refusal,
	/// including NOT_SUPPORTED and undefined codes, without opening a subscription.
	#[moq_clock::test]
	async fn a_refused_track_status_never_subscribes() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			for code in [3, u64::from(u32::MAX)] {
				for fetching in [false, true] {
					let refused = message(
						ietf::RequestError::ID,
						&ietf::RequestError {
							error_code: code,
							reason_phrase: "TRACK_STATUS is refused".into(),
							retry_interval: 0,
						},
						version,
					);
					let session = crate::lite::test_transport::ScriptedSession::new(refused);
					let log = session.log.clone();
					let (tasks, _task_set) = crate::util::TaskSet::new();
					let mut subscriber = Subscriber::new(
						crate::time::sim(),
						crate::driver::Transport::new(session.clone()),
						crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce(),
						Control::new(false),
						None,
						peer::PeerSetup::declared(peer::Peer::default()),
						crate::Hop::new(1).unwrap(),
						None,
						version,
						tasks,
						Default::default(),
					);

					let producer = crate::broadcast::Info::default().produce();
					let mut dynamic = producer.requests();
					let consumer = producer.consume();
					let track = consumer.track("video").unwrap();
					let mut pending = Box::pin(async {
						if fetching {
							track.fetch_group(0, None).await.map(|_| ())
						} else {
							track.info().await.map(|_| ())
						}
					});
					assert!(futures::poll!(pending.as_mut()).is_pending());
					let request = dynamic.requested_track().await.expect("no track requested");
					let mut serving =
						Box::pin(subscriber.run_subscribe(Path::from_static("broadcast"), dynamic, None, request));
					assert!(
						futures::poll!(serving.as_mut()).is_ready(),
						"{version}: refusal was not terminal"
					);
					match pending.await.expect_err("TRACK_STATUS was refused") {
						Error::Unsupported => assert_eq!(code, 3),
						Error::Remote(value) => assert_eq!(u64::from(value), code),
						err => panic!("{version}, fetching: {fetching}: wrong refusal: {err:?}"),
					}
					assert_eq!(
						control_message_types(&log, version),
						[ietf::TrackStatus::ID],
						"{version}"
					);
				}
			}
		}
	}

	/// A copy that already knows its track, here from TRACK_STATUS_OK, opts out of the
	/// properties when a subscriber returns, and keeps the units it learned rather than
	/// taking the empty block the opted-out SUBSCRIBE_OK carries.
	#[moq_clock::test]
	async fn a_resumed_subscribe_opts_out_of_known_properties() {
		let declared = Timescale::new(90_000).unwrap();
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			let opts_out = ietf::Filter::is_draft20(version);
			let declares = |yes: bool| ietf::Properties {
				timescale: yes.then_some(declared),
				..Default::default()
			};
			let status_ok = message(
				ietf::TrackStatusOk::ID,
				&ietf::TrackStatusOk {
					largest: Some(ietf::Location { group: 3, object: 42 }),
					properties: declares(true),
				},
				version,
			);
			let session = crate::lite::test_transport::ScriptedSession::per_stream(vec![
				status_ok,
				// Empty when opted out. Before draft-22 SUBSCRIBE cannot opt out, so it declares
				// the units again.
				message(
					ietf::SubscribeOk::ID,
					&ietf::SubscribeOk {
						track_alias: 7,
						largest: None,
						properties: declares(!opts_out),
					},
					version,
				),
			]);
			let log = session.log.clone();
			let (tasks, _task_set) = crate::util::TaskSet::new();
			let mut subscriber = Subscriber::new(
				crate::time::sim(),
				crate::driver::Transport::new(session.clone()),
				crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce(),
				Control::new(false),
				None,
				peer::PeerSetup::declared(peer::Peer::default()),
				crate::Hop::new(1).unwrap(),
				None,
				version,
				tasks,
				Default::default(),
			);

			let producer = crate::broadcast::Info::default().produce();
			let mut dynamic = producer.requests();
			let consumer = producer.consume();
			let track = consumer.track("video").unwrap();
			// Demand with nobody subscribing learns the track from TRACK_STATUS.
			let mut query = std::pin::pin!(track.info());
			assert!(futures::poll!(query.as_mut()).is_pending());
			let request = dynamic.requested_track().await.expect("no track requested");

			let probe = subscriber.clone();
			let serving = moq_clock::sim::spawn(async move {
				subscriber
					.run_subscribe(Path::from_static("broadcast"), dynamic, None, request)
					.await;
			});
			settle().await;
			assert_eq!(
				track.reported_object(),
				Some((3, 42)),
				"{version}: status Largest survives on a fetch-only copy"
			);
			let _subscription = track.subscribe(None);
			settle().await;

			let writes = log.writes.lock().unwrap().clone();
			let mut buf = Decoder::new(&writes, version.into());
			let mut subscribes = Vec::new();
			while !buf.is_empty() {
				let type_id = buf.varint().unwrap();
				let size = buf.u16().unwrap();
				let mut body = Decoder::new(buf.slice(size as usize).unwrap(), version.into());
				if type_id == ietf::Subscribe::ID {
					subscribes.push(ietf::Subscribe::decode_msg(&mut body, version).unwrap());
				}
			}
			assert_eq!(subscribes.len(), 1, "{version}: the returning subscriber subscribes");
			assert_eq!(
				subscribes[0].properties_wanted, !opts_out,
				"{version}: TRACK_STATUS_OK already gave the properties"
			);

			let timescale = probe
				.state
				.lock()
				.subscribes
				.values()
				.next()
				.expect("the subscription is registered")
				.timescale;
			assert_eq!(
				timescale,
				Some(declared),
				"{version}: the copy keeps the units it learned"
			);
			serving.abort();
		}
	}

	/// The publisher opens no fetch stream for an empty range, so a fill against a track
	/// with no content is owed nothing. Leaving it outstanding would withhold every later
	/// group behind a head that is never coming.
	#[moq_clock::test]
	async fn an_empty_track_settles_the_fill() {
		assert!(
			!fill_after_subscribe_ok(None, Some(37)).await,
			"no LARGEST_OBJECT means no content, so no fill is owed"
		);
	}

	/// A track with content does owe one, so the fill stays outstanding until its fetch
	/// stream arrives.
	#[moq_clock::test]
	async fn a_track_with_content_still_awaits_its_fill() {
		assert!(
			fill_after_subscribe_ok(Some(ietf::Location { group: 3, object: 4 }), Some(37)).await,
			"a fetch stream is still owed"
		);
	}

	#[moq_clock::test]
	async fn an_older_peer_without_priority_property_uses_wire_default() {
		assert!(!fill_after_subscribe_ok(None, None).await);
	}

	/// Tombstones are bounded: a session churning through subscriptions must not
	/// accumulate one entry per alias it ever used.
	#[test]
	fn retired_aliases_are_capped() {
		let aliases = TrackAliases::default();

		for i in 0..(RETIRED_ALIAS_CAPACITY as u64 + 10) {
			insert_track_alias(&aliases, i, RequestId(i)).unwrap();
			retire_track_alias(&aliases, i, RequestId(i));
		}

		let table = aliases.read();
		assert_eq!(table.retired.len(), RETIRED_ALIAS_CAPACITY);
		assert_eq!(table.map.len(), RETIRED_ALIAS_CAPACITY);
		assert!(!table.map.contains_key(&0), "the oldest tombstone is forgotten first");
	}

	/// A namespace past the session's cap closes the session with TOO_MANY_REQUESTS, a
	/// repeat of a held namespace costs nothing, and a retraction frees its slot.
	#[moq_clock::test]
	async fn namespaces_past_the_cap_close_the_session() {
		let session = crate::lite::test_transport::SinkSession::new(Default::default());
		let log = session.log.clone();
		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let (tasks, _task_set) = crate::util::TaskSet::new();
		let mut subscriber = Subscriber::new(
			crate::time::sim(),
			crate::driver::Transport::new(session.clone()),
			origin,
			Control::new(false),
			None,
			peer::PeerSetup::default(),
			crate::Hop::new(1).unwrap(),
			None,
			Version::Draft18,
			tasks,
			Default::default(),
		);
		subscriber.announces = crate::session::Slots::new(1);

		let advert = |subscriber: &Subscriber| subscriber.route(None, &cluster::Peer::default()).expect("route");
		let a = crate::Path::from_static("a");
		let b = crate::Path::from_static("b");
		let (first, repeat, over, last) = (
			advert(&subscriber),
			advert(&subscriber),
			advert(&subscriber),
			advert(&subscriber),
		);
		let first = subscriber
			.start_announce(a.clone(), first, Admit::Session(None))
			.unwrap();
		let repeat = subscriber
			.start_announce(a.clone(), repeat, Admit::Session(None))
			.unwrap();
		assert!(matches!(
			subscriber.start_announce(b.clone(), over, Admit::Session(None)),
			Err(Error::TooManyRequests)
		));
		assert_eq!(
			log.closes(),
			vec![(
				crate::SessionError::TooManyRequests.to_code(),
				"too many announcements".to_string()
			)]
		);

		subscriber.stop_announce(a.clone(), first, false).unwrap();
		subscriber.stop_announce(a, repeat, false).unwrap();
		subscriber.start_announce(b, last, Admit::Session(None)).unwrap();
	}

	/// moq-transport carries no hop ids, so a peer's broadcasts are marked
	/// anonymous (hop 0). An identity assigned via `Client::with_peer_hop` is
	/// stored as `via` for split-horizon and never written into the chain.
	#[moq_clock::test]
	async fn assigned_peer_hop_attributes_announces() {
		let session = crate::lite::test_transport::SinkSession::new(Default::default());
		let assigned = crate::Hop::new(777).unwrap();

		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let consumer = origin.consume();
		let (tasks, _task_set) = crate::util::TaskSet::new();
		let mut subscriber = Subscriber::new(
			crate::time::sim(),
			crate::driver::Transport::new(session.clone()),
			origin,
			Control::new(false),
			Some(assigned),
			peer::PeerSetup::default(),
			crate::Hop::new(1).unwrap(),
			None,
			Version::Draft18,
			tasks,
			Default::default(),
		);

		let advert = subscriber.route(None, &cluster::Peer::default()).expect("route");
		subscriber
			.start_announce(crate::Path::from_static("room/host"), advert, Admit::Session(None))
			.unwrap();

		let mut announced = consumer.announced();
		let route = announced.expect_next_active("room/host").await;
		let hops: Vec<_> = route.hops.iter().copied().collect();
		assert_eq!(hops, vec![crate::Hop::UNKNOWN]);
		assert!(route.is_anonymous());

		let mut hidden = consumer.excluding(assigned).announced();
		hidden.assert_next_wait();
	}

	/// Both directions of a sync target point at one relay, which has no way to tell
	/// our two connections apart on a wire with no hop ids and so offers our own
	/// broadcast back to us. That reflection must not look like a rival publisher
	/// claiming the path: taking it over would leave only a route we refuse to
	/// advertise back to the peer, and the publish direction would withdraw the
	/// announce it just made.
	#[moq_clock::test]
	async fn reflected_announce_does_not_evict_the_source_we_publish() {
		let session = crate::lite::test_transport::SinkSession::new(Default::default());
		let peer = crate::Hop::new(777).unwrap();
		let self_origin = crate::Hop::new(1).unwrap();

		let origin = crate::origin::Config::new(self_origin).produce();
		let consumer = origin.consume();
		let mut announced = consumer.announced();

		// The publish direction: an origin handle scoped to the peer, which is what
		// `Client::with_peer_hop` hands the publisher. Holding its announce stream
		// is what records that the peer has been offered these paths.
		let mut publishing = consumer.clone().excluding(peer).announced();

		// What we are publishing to the peer: a real upstream route.
		let upstream = crate::Hops::try_from(vec![crate::Hop::new(7).unwrap()]).unwrap();
		let _source = origin
			.announce("room/host", crate::origin::Route::default().with_hops(upstream.clone()))
			.unwrap();
		announced.expect_next_active("room/host").await;
		let _advertised = publishing.expect_next_active("room/host").await;

		// The peer reflects it back over the subscribe direction, which carries no
		// hop chain of its own.
		let (tasks, _task_set) = crate::util::TaskSet::new();
		let mut subscriber = Subscriber::new(
			crate::time::sim(),
			crate::driver::Transport::new(session.clone()),
			origin,
			Control::new(false),
			Some(peer),
			peer::PeerSetup::default(),
			self_origin,
			None,
			Version::Draft18,
			tasks,
			Default::default(),
		);
		let advert = subscriber.route(None, &cluster::Peer::default()).expect("route");
		subscriber
			.start_announce(crate::Path::from_static("room/host"), advert, Admit::Session(None))
			.unwrap();

		// No announce churn: the upstream route stays the best one, on both cursors.
		announced.assert_next_wait();
		publishing.assert_next_wait();
		let route = routed_now(&consumer, "room/host").expect("still routed");
		assert_eq!(route.hops, upstream);
	}

	/// Two sessions assigned the same identity announce the same anonymous chain.
	/// Without an epoch the fresh session is another source, so consumers restart
	/// once, and retracting the stale session leaves the fresh route standing quietly.
	#[moq_clock::test]
	async fn reconnecting_peer_restarts_the_route() {
		let peer = crate::Hop::new(777).unwrap();
		let self_origin = crate::Hop::new(1).unwrap();

		let origin = crate::origin::Config::new(self_origin).produce();
		let consumer = origin.consume();
		let mut announced = consumer.announced();

		let connect = || {
			let (tasks, task_set) = crate::util::TaskSet::new();
			std::mem::forget(task_set);
			let mut subscriber = Subscriber::new(
				crate::time::sim(),
				crate::driver::Transport::new(crate::lite::test_transport::SinkSession::new(Default::default())),
				origin.clone(),
				Control::new(false),
				Some(peer),
				peer::PeerSetup::default(),
				self_origin,
				None,
				Version::Draft18,
				tasks,
				Default::default(),
			);
			let advert = subscriber.route(None, &cluster::Peer::default()).expect("route");
			subscriber
				.start_announce(crate::Path::from_static("room/host"), advert, Admit::Session(None))
				.unwrap();
			subscriber
		};

		let first = connect();
		moq_clock::sim::yield_now().await;
		announced.expect_next_active("room/host").await;

		// The peer reconnects before the old session is retired: an identical route
		// from the fresh session wins at once, as another source.
		let _second = connect();
		moq_clock::sim::yield_now().await;
		announced.expect_next_restarted("room/host").await;
		announced.assert_next_wait();

		// Neither route is offered back to the peer they both came from.
		consumer.clone().excluding(peer).announced().assert_next_wait();

		// The stale session finally retracting leaves the fresh route standing.
		drop(first);
		moq_clock::sim::yield_now().await;
		announced.assert_next_wait();
		assert!(routed_now(&consumer, "room/host").is_some());
	}

	fn cluster_subscriber(self_origin: crate::Hop) -> (Subscriber, crate::origin::Producer) {
		let session = crate::lite::test_transport::SinkSession::new(Default::default());
		let origin = crate::origin::Config::new(self_origin).produce();
		let (tasks, task_set) = crate::util::TaskSet::new();
		// The set only drains announce-serving tasks; the tests here drive the model
		// directly, so leaking it keeps the handles alive without a spawner.
		std::mem::forget(task_set);

		let subscriber = Subscriber::new(
			crate::time::sim(),
			crate::driver::Transport::new(session.clone()),
			origin.clone(),
			Control::new(false),
			None,
			peer::PeerSetup::default(),
			self_origin,
			None,
			Version::Draft18,
			tasks,
			Default::default(),
		);
		(subscriber, origin)
	}

	/// The current best route covering `path`, if any (synchronous peek).
	fn routed_now(consumer: &crate::origin::Consumer, path: &str) -> Option<crate::origin::Route> {
		use futures::FutureExt;
		match consumer.routes(path).ok()?.next().now_or_never()?? {
			crate::announce::Event::End(_) => None,
			crate::announce::Event::Start(announce)
			| crate::announce::Event::Update(announce)
			| crate::announce::Event::Restart(announce)
			| crate::announce::Event::Takeover { announce, .. } => Some(announce.route),
		}
	}

	fn hop_path(ids: &[u64]) -> cluster::HopPath {
		let hops = ids.iter().map(|&id| crate::Hop::new(id).unwrap()).collect::<Vec<_>>();
		cluster::HopPath::new(crate::Hops::try_from(hops).unwrap())
	}

	/// A negotiated advertisement carries the whole path and its accumulated cost, and
	/// the receiving relay charges its own link on top (saturating, so an absurd
	/// upstream value ranks last rather than wrapping to best).
	#[moq_clock::test]
	async fn cluster_advert_becomes_a_route_with_the_link_charged() {
		let (subscriber, origin) = cluster_subscriber(crate::Hop::new(1).unwrap());
		let consumer = origin.consume();

		let peer = cluster::Peer {
			hop: Some(crate::Hop::new(9).unwrap()),
			cost: Some(3),
		};
		let advert = cluster::Advert {
			hops: hop_path(&[7, 9]),
			cost: 4,
		};

		let advertised = subscriber.route(Some(&advert), &peer).expect("route");
		assert_eq!(
			advertised.route.cost.value(),
			7,
			"the link's price is added to the advertised cost"
		);
		assert_eq!(advertised.route.hops, hop_path(&[7, 9]).hops().clone());

		let mut subscriber = subscriber;
		subscriber
			.start_announce(crate::Path::from_static("room/host"), advertised, Admit::Session(None))
			.unwrap();

		let route = routed_now(&consumer, "room/host").expect("routed");
		let hops: Vec<_> = route.hops.iter().map(|h| h.id()).collect();
		assert_eq!(hops, vec![7, 9]);
		assert_eq!(route.cost.value(), 7);
	}

	/// An advertisement whose path already contains our own Hop ID looped back:
	/// forwarding it would extend the loop and subscribing through it would route us
	/// back to ourselves. Hop ID 0 identifies nothing, so it is never a loop.
	#[moq_clock::test]
	async fn cluster_advert_loop_is_discarded() {
		let (subscriber, _origin) = cluster_subscriber(crate::Hop::new(5).unwrap());
		let peer = cluster::Peer {
			hop: Some(crate::Hop::new(9).unwrap()),
			cost: None,
		};

		let looped = cluster::Advert {
			hops: hop_path(&[7, 5, 9]),
			cost: 0,
		};
		assert!(subscriber.route(Some(&looped), &peer).is_none());

		let clean = cluster::Advert {
			hops: hop_path(&[7, 9]),
			cost: 0,
		};
		assert!(subscriber.route(Some(&clean), &peer).is_some());
	}

	/// An unpriced link costs 1, so an unpriced mesh accumulates a cost equal to the
	/// hop count and degenerates to shortest-path routing.
	#[moq_clock::test]
	async fn unpriced_link_costs_one() {
		let (subscriber, _origin) = cluster_subscriber(crate::Hop::new(1).unwrap());
		let peer = cluster::Peer {
			hop: Some(crate::Hop::new(9).unwrap()),
			cost: None,
		};
		let advert = cluster::Advert {
			hops: hop_path(&[7, 9]),
			cost: 2,
		};
		assert_eq!(subscriber.route(Some(&advert), &peer).unwrap().route.cost.value(), 3);

		// Zero is meaningful and distinct from absent: a free link adds nothing.
		let free = cluster::Peer {
			hop: Some(crate::Hop::new(9).unwrap()),
			cost: Some(0),
		};
		assert_eq!(subscriber.route(Some(&advert), &free).unwrap().route.cost.value(), 2);
	}

	/// A namespace stream that ends with advertisements still live detaches them, so
	/// the broadcast closes rather than staying announced over a dead stream. True
	/// even of a clean FIN, since closing the stream retracts nothing: the protocol
	/// has NAMESPACE_DONE for that. moq-lite already behaves this way (its route map
	/// is a local whose guards drop), which `lite::subscriber` pins separately.
	///
	/// Driven through the real exit path rather than by calling `stop_announce`: a test
	/// that picked the detach itself would still pass if the stream stopped using it.
	#[moq_clock::test]
	async fn a_lost_namespace_stream_closes_the_broadcast() {
		const VERSION: Version = Version::Draft18;

		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let consumer = origin.consume();

		// The peer answers, advertises one namespace, then the stream ends without ever
		// retracting it.
		let session = crate::lite::test_transport::ScriptedSession::eof(namespace_response(VERSION, "x.hang").await);
		let (tasks, task_set) = crate::util::TaskSet::new();
		std::mem::forget(task_set);
		// Draft-18 can negotiate the extension, so the read loop waits for the peer's
		// SETUP before parsing a NAMESPACE; settle it as extension-off.
		let peer_setup = peer::PeerSetup::default();
		peer_setup.set(peer::Peer::default());
		let mut subscriber = Subscriber::new(
			crate::time::sim(),
			crate::driver::Transport::new(session.clone()),
			origin,
			Control::new(false),
			None,
			peer_setup,
			crate::Hop::new(1).unwrap(),
			None,
			VERSION,
			tasks,
			Default::default(),
		);

		let (_live, live) = live();
		let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), VERSION)
			.await
			.unwrap();
		subscriber
			.run_subscribe_namespace(
				stream,
				crate::Path::from_static(""),
				crate::model::Snapshot::landed(),
				live,
			)
			.await
			.expect("a clean FIN is not an error");
		settle().await;

		assert!(
			routed_now(&consumer, "x.hang").is_none(),
			"an ended stream must retract the route, not leave a stale one",
		);
	}

	/// NAMESPACE has no REQUEST_UPDATE, so a peer reprices one by re-sending it on the
	/// SUBSCRIBE_NAMESPACE stream. The repeat is neither a duplicate nor a violation: it
	/// replaces the advertisement in place, and the route is never retracted for it.
	#[moq_clock::test]
	async fn a_re_sent_namespace_reprices_in_place() {
		const VERSION: Version = Version::Draft18;

		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let consumer = origin.consume();

		let script = {
			let log = crate::lite::test_transport::Log::default();
			let mut writer = crate::coding::Writer::new(
				crate::transport::boxed::SendStream::new(crate::lite::test_transport::SinkSend::new(log.clone())),
				VERSION,
			);
			writer.varint(ietf::RequestOk::ID).await.unwrap();
			writer
				.encode(&ietf::RequestOk {
					largest: None,
					active: None,
				})
				.await
				.unwrap();
			for cost in [4, 0] {
				writer.varint(ietf::Namespace::ID).await.unwrap();
				writer
					.encode(&ietf::Namespace {
						suffix: crate::Path::from_static("x.hang"),
						params: Some(ietf::AdvertParams {
							cluster: Some(cluster::Advert {
								hops: hop_path(&[7, 9]),
								cost,
							}),
							epoch: None,
						}),
					})
					.await
					.unwrap();
			}
			log.writes.lock().unwrap().clone()
		};

		let session = crate::lite::test_transport::ScriptedSession::new(script);
		let (tasks, task_set) = crate::util::TaskSet::new();
		std::mem::forget(task_set);
		// A negotiated peer over a free link, so the route cost is what it advertised.
		let peer_setup = peer::PeerSetup::default();
		peer_setup.set(peer::Peer {
			cluster: cluster::Peer {
				hop: Some(crate::Hop::new(9).unwrap()),
				cost: Some(0),
			},
			..Default::default()
		});
		let mut subscriber = Subscriber::new(
			crate::time::sim(),
			crate::driver::Transport::new(session.clone()),
			origin,
			Control::new(false),
			None,
			peer_setup,
			crate::Hop::new(1).unwrap(),
			None,
			VERSION,
			tasks,
			Default::default(),
		);

		let (_live, live) = live();
		let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), VERSION)
			.await
			.unwrap();
		let mut run = std::pin::pin!(subscriber.run_subscribe_namespace(
			stream,
			crate::Path::from_static(""),
			crate::model::Snapshot::landed(),
			live
		));
		for _ in 0..100 {
			assert!(
				futures::poll!(run.as_mut()).is_pending(),
				"the stream stays open through a repeat"
			);
			if routed_now(&consumer, "x.hang").is_some_and(|route| route.cost.value() == 0) {
				break;
			}
			settle().await;
		}

		let route = routed_now(&consumer, "x.hang").expect("still routed");
		assert_eq!(route.cost.value(), 0, "the repeat repriced the route");
	}

	/// The peer explicitly retracting a namespace ends the broadcast immediately: it
	/// said the namespace is gone, so a later create at the path is new content.
	#[moq_clock::test]
	async fn an_explicit_namespace_done_closes_the_broadcast() {
		let (mut subscriber, origin) = cluster_subscriber(crate::Hop::new(1).unwrap());
		let consumer = origin.consume();

		let path = crate::Path::from_static("room/host");
		let advert = subscriber.route(None, &cluster::Peer::default()).expect("route");
		let id = subscriber
			.start_announce(path.clone(), advert, Admit::Session(None))
			.unwrap();
		settle().await;

		subscriber.stop_announce(path, id, false).unwrap();
		assert!(
			routed_now(&consumer, "room/host").is_none(),
			"an explicit NAMESPACE_DONE must retract the route",
		);
	}

	#[moq_clock::test]
	async fn publish_namespace_requester_fin_keeps_the_route() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			for reset in [false, true] {
				let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
				let consumer = origin.consume();
				let session = if reset {
					crate::lite::test_transport::ScriptedSession::per_stream_reset(vec![vec![]])
				} else {
					crate::lite::test_transport::ScriptedSession::per_stream_eof(vec![vec![]])
				};
				let (tasks, _task_set) = crate::util::TaskSet::new();
				let mut subscriber = Subscriber::new(
					crate::time::sim(),
					crate::driver::Transport::new(session.clone()),
					origin,
					Control::new(false),
					None,
					peer::PeerSetup::default(),
					crate::Hop::new(1).unwrap(),
					None,
					version,
					tasks,
					Default::default(),
				);
				let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), version)
					.await
					.unwrap();
				let msg = ietf::PublishNamespace {
					request_id: RequestId(0),
					track_namespace: crate::Path::from_static("room/host"),
					params: Default::default(),
					authorization_token: None,
				};
				let mut run = std::pin::pin!(subscriber.run_publish_namespace_stream(
					stream,
					msg,
					cluster::Peer::default(),
					None
				));
				assert_eq!(
					futures::poll!(run.as_mut()).is_ready(),
					reset || super::super::request_stream::fin_cancels(version),
					"{version}"
				);
				if !reset && !super::super::request_stream::fin_cancels(version) {
					assert!(routed_now(&consumer, "room/host").is_some(), "{version}");
				}
			}
		}
	}

	/// A PUBLISH_NAMESPACE stream that dies mid-advertisement detaches it, closing the
	/// broadcast: the advertisement was never withdrawn, but the stream carrying it is
	/// gone, and a route into a dead stream must not stay announced.
	///
	/// Driven through the real exit path rather than by calling `stop_announce`: a test
	/// that picked the detach itself would still pass if the stream stopped using it.
	#[moq_clock::test]
	async fn a_broken_publish_namespace_stream_closes_the_broadcast() {
		const VERSION: Version = Version::Draft18;

		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let consumer = origin.consume();

		// The peer sends something that does not belong on this stream, ending it with an
		// error while the advertisement is still live.
		let log = crate::lite::test_transport::Log::default();
		let mut writer = crate::coding::Writer::new(
			crate::transport::boxed::SendStream::new(crate::lite::test_transport::SinkSend::new(log.clone())),
			VERSION,
		);
		writer.varint(ietf::NamespaceDone::ID).await.unwrap();
		let script = log.writes.lock().unwrap().clone();

		let session = crate::lite::test_transport::ScriptedSession::eof(script);
		let (tasks, task_set) = crate::util::TaskSet::new();
		std::mem::forget(task_set);
		let mut subscriber = Subscriber::new(
			crate::time::sim(),
			crate::driver::Transport::new(session.clone()),
			origin,
			Control::new(false),
			None,
			peer::PeerSetup::default(),
			crate::Hop::new(1).unwrap(),
			None,
			VERSION,
			tasks,
			Default::default(),
		);

		let path = crate::Path::from_static("room/host");
		let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), VERSION)
			.await
			.unwrap();
		let msg = ietf::PublishNamespace {
			request_id: RequestId(0),
			track_namespace: path.clone(),
			params: Default::default(),
			authorization_token: None,
		};
		subscriber
			.run_publish_namespace_stream(stream, msg, cluster::Peer::default(), None)
			.await
			.expect_err("an unexpected message ends the stream");
		settle().await;

		assert!(
			routed_now(&consumer, "room/host").is_none(),
			"a broken stream must close the broadcast, not leave a stale route",
		);
	}

	/// Several advertisements share one refcounted source, so the detach that empties it
	/// is the one that counts: the broadcast survives the first stop and closes on the
	/// last.
	///
	/// That is the model's own rule for several sources at one path (the front's source
	/// selection),
	/// which is what these advertisements would be had they arrived on two sessions. The
	/// refcount is a detail of sharing one `SourceGuard` per session; it must not change
	/// what the origin sees.
	#[moq_clock::test]
	async fn the_last_owner_out_decides_the_detach() {
		let (mut subscriber, origin) = cluster_subscriber(crate::Hop::new(1).unwrap());
		let consumer = origin.consume();
		let path = crate::Path::from_static("room/host");
		let peer = cluster::Peer::default();

		let mut ids = Vec::new();
		for _ in 0..2 {
			let advert = subscriber.route(None, &peer).expect("route");
			ids.push(
				subscriber
					.start_announce(path.clone(), advert, Admit::Session(None))
					.unwrap(),
			);
		}
		settle().await;

		// One advertisement's stream dies: the other still holds the source.
		subscriber.stop_announce(path.clone(), ids[0], false).unwrap();
		settle().await;
		assert!(
			routed_now(&consumer, "room/host").is_some(),
			"the broadcast must survive while an owner remains",
		);

		// The last owner retracts: the broadcast closes with it.
		subscriber.stop_announce(path, ids[1], false).unwrap();
		settle().await;
		assert!(
			routed_now(&consumer, "room/host").is_none(),
			"the last owner out must close the broadcast",
		);
	}

	/// A source minted under a namespace goes once nothing holds it, so the session keeps
	/// nothing for a path its fronts let go of, and goes at once as the namespace is
	/// retracted, held or not.
	#[moq_clock::test]
	async fn a_minted_source_closes_unheld_or_retracted() {
		let (subscriber, origin) = cluster_subscriber(crate::Hop::new(1).unwrap());
		let run = |route: &kio::Producer<()>| {
			let source = crate::model::broadcast::SourceGuard::new(
				origin.create_source(crate::Path::from_static("pool/p"), Default::default()),
			);
			let (holder, dynamic) = (source.consume(), source.requests());
			let (this, route) = (subscriber.clone(), route.consume());
			let task = moq_clock::sim::spawn(async move {
				this.run_broadcast(crate::Path::from_static("pool/p"), source, dynamic, Some(route))
					.await
			});
			(holder, task)
		};

		let route = kio::Producer::<()>::default();
		let (holder, task) = run(&route);
		settle().await;
		assert!(!task.is_finished(), "retired a source in use");
		drop(holder);
		settle().await;
		assert!(task.is_finished(), "kept a source nothing holds");

		let (holder, task) = run(&route);
		settle().await;
		drop(route);
		settle().await;
		assert!(task.is_finished(), "kept a source past its namespace");
		assert!(holder.is_closed());
	}

	/// An advertisement with no path of its own (a peer that did not negotiate the
	/// extension) still pays for the link it arrived over. Forwarding it as free would
	/// advertise a paid upstream as the cheapest route in the mesh.
	#[moq_clock::test]
	async fn a_pathless_advert_still_pays_for_its_link() {
		let (unpriced, _origin) = cluster_subscriber(crate::Hop::new(1).unwrap());
		let peer = cluster::Peer::default();

		// Nothing priced this direction, so it ranks by hop count.
		assert_eq!(
			unpriced.route(None, &peer).unwrap().route.cost.value(),
			cluster::DEFAULT_COST
		);

		// A peer that declared its egress price is charged it, extension or not.
		let priced_peer = cluster::Peer {
			hop: None,
			cost: Some(4),
		};
		assert_eq!(unpriced.route(None, &priced_peer).unwrap().route.cost.value(), 4);

		// Local policy still wins over what the peer declared.
		let (mut priced, _origin) = cluster_subscriber(crate::Hop::new(1).unwrap());
		priced.cost = Some(6);
		assert_eq!(priced.route(None, &priced_peer).unwrap().route.cost.value(), 6);
	}

	/// An update replaces the advertisement in place: the route moves, the refcount does
	/// not, and the source is not torn down.
	#[moq_clock::test]
	async fn cluster_update_replaces_in_place() {
		let (mut subscriber, origin) = cluster_subscriber(crate::Hop::new(1).unwrap());
		let consumer = origin.consume();
		let path = crate::Path::from_static("room/host");

		let first = Advertised {
			route: crate::origin::Route::default()
				.with_hops(hop_path(&[7, 9]).hops().clone())
				.with_cost(4),
		};
		let id = subscriber
			.start_announce(path.clone(), first, Admit::Session(None))
			.unwrap();
		assert!(routed_now(&consumer, "room/host").is_some());

		// A new chain and cost: the route updates in place.
		let rerouted = Advertised {
			route: crate::origin::Route::default()
				.with_hops(hop_path(&[7, 11]).hops().clone())
				.with_cost(2),
		};
		subscriber.update_announce(path.clone(), id, rerouted).unwrap();

		let route = routed_now(&consumer, "room/host").expect("routed");
		let hops: Vec<_> = route.hops.iter().map(|h| h.id()).collect();
		assert_eq!(hops, vec![7, 11]);
		assert_eq!(route.cost.value(), 2);

		// One advertisement, so one unannounce detaches it. If the update had bumped the
		// refcount, this would leave the route stranded.
		subscriber.stop_announce(path, id, false).unwrap();
		assert!(routed_now(&consumer, "room/host").is_none());
	}

	/// Regression: a publisher that declares no identity of its own contributes
	/// `Hop::UNKNOWN` as the first hop, which identifies nothing. A repeat NAMESPACE
	/// is still the same advertisement being repriced (the expected update, and how a
	/// relay signals that it started carrying the namespace), so the source and every
	/// live subscription on it must survive. Reading the repeat as a new publisher
	/// detached the source milliseconds after SUBSCRIBE went out.
	#[moq_clock::test]
	async fn anonymous_publisher_survives_a_repricing_update() {
		let (mut subscriber, origin) = cluster_subscriber(crate::Hop::new(1).unwrap());
		let consumer = origin.consume();
		let path = crate::Path::from_static("room/host");
		// A free link, so the route cost is exactly what the peer advertised.
		let peer = cluster::Peer {
			hop: Some(crate::Hop::new(9).unwrap()),
			cost: Some(0),
		};
		let hops = cluster::HopPath::new(
			crate::Hops::try_from(vec![crate::Hop::UNKNOWN, crate::Hop::new(9).unwrap()]).unwrap(),
		);

		let advertised = subscriber
			.route(
				Some(&cluster::Advert {
					hops: hops.clone(),
					cost: 2,
				}),
				&peer,
			)
			.expect("route");
		let id = subscriber
			.start_announce(path.clone(), advertised, Admit::Session(None))
			.unwrap();
		assert!(routed_now(&consumer, "room/host").is_some());

		// The peer re-advertises the same path cheaper: it started carrying it.
		let repriced = subscriber
			.route(Some(&cluster::Advert { hops, cost: 1 }), &peer)
			.expect("route");
		subscriber.update_announce(path.clone(), id, repriced).unwrap();

		let route = routed_now(&consumer, "room/host").expect("still routed");
		assert_eq!(
			route.cost,
			crate::origin::Cost::new(1),
			"the repriced static cost arrives"
		);

		// One advertisement, so one unannounce detaches it.
		subscriber.stop_announce(path, id, false).unwrap();
		assert!(routed_now(&consumer, "room/host").is_none());
	}

	/// MoQ Epoch: a publisher replacing its instance withdraws one advertisement and makes
	/// another, on separate streams that arrive in either order. The newest advertisement
	/// stands, so neither the old one's late update nor its withdrawal reverts the route
	/// to the instance it named.
	#[moq_clock::test]
	async fn a_reordered_replacement_keeps_the_new_instance() {
		let (mut subscriber, origin) = cluster_subscriber(crate::Hop::new(1).unwrap());
		let consumer = origin.consume();
		let path = crate::Path::from_static("room/host");
		let peer = cluster::Peer::default();
		let (old, new) = (crate::Epoch::mint(), crate::Epoch::mint());
		let advert = |subscriber: &Subscriber, epoch: &crate::Epoch| {
			subscriber
				.route(None, &peer)
				.expect("route")
				.with_epoch(Some(epoch.clone()))
		};
		let epoch_now = || routed_now(&consumer, "room/host").and_then(|route| route.epoch);

		let first = advert(&subscriber, &old);
		let first = subscriber
			.start_announce(path.clone(), first, Admit::Session(None))
			.unwrap();
		assert_eq!(epoch_now(), Some(old.clone()));

		// The replacement's PUBLISH_NAMESPACE beats the old one's PUBLISH_NAMESPACE_DONE.
		let second = advert(&subscriber, &new);
		let second = subscriber
			.start_announce(path.clone(), second, Admit::Session(None))
			.unwrap();
		assert_eq!(epoch_now(), Some(new.clone()));

		// A late update to the old advertisement changes nothing.
		let stale = advert(&subscriber, &old);
		subscriber.update_announce(path.clone(), first, stale).unwrap();
		assert_eq!(epoch_now(), Some(new.clone()), "a stale update reverted the instance");

		// Its withdrawal, arriving last, leaves the new instance standing.
		subscriber.stop_announce(path.clone(), first, false).unwrap();
		assert_eq!(
			epoch_now(),
			Some(new.clone()),
			"the old withdrawal retracted the new instance"
		);

		subscriber.stop_announce(path, second, false).unwrap();
		assert_eq!(routed_now(&consumer, "room/host"), None);
	}

	/// MoQ Epoch: a namespace advertised both inline and by a later PUBLISH_NAMESPACE takes
	/// another epoch from either. An inline repeat under the new epoch is the newest
	/// advertisement, not an update to the older of the two.
	#[moq_clock::test]
	async fn a_repeat_under_another_epoch_becomes_the_newest() {
		let (mut subscriber, origin) = cluster_subscriber(crate::Hop::new(1).unwrap());
		let consumer = origin.consume();
		let path = crate::Path::from_static("room/host");
		let peer = cluster::Peer::default();
		let (old, new) = (crate::Epoch::mint(), crate::Epoch::mint());
		let advert = |subscriber: &Subscriber, epoch: &crate::Epoch| {
			subscriber
				.route(None, &peer)
				.expect("route")
				.with_epoch(Some(epoch.clone()))
		};
		let epoch_now = || routed_now(&consumer, "room/host").and_then(|route| route.epoch);

		let inline = advert(&subscriber, &old);
		let inline = subscriber
			.start_announce(path.clone(), inline, Admit::Session(None))
			.unwrap();
		let request = advert(&subscriber, &old);
		let request = subscriber
			.start_announce(path.clone(), request, Admit::Session(None))
			.unwrap();
		assert_eq!(epoch_now(), Some(old.clone()));

		let repeat = advert(&subscriber, &new);
		subscriber.update_announce(path.clone(), inline, repeat).unwrap();
		assert_eq!(
			epoch_now(),
			Some(new.clone()),
			"the older request kept the old instance"
		);

		// The old instance's withdrawal leaves the new one standing.
		subscriber.stop_announce(path.clone(), request, false).unwrap();
		assert_eq!(epoch_now(), Some(new));
		subscriber.stop_announce(path, inline, false).unwrap();
		assert_eq!(routed_now(&consumer, "room/host"), None);
	}

	/// Two *separate* advertisements for one namespace refcount a single route:
	/// it takes both retractions to retract it.
	#[moq_clock::test]
	async fn separate_adverts_refcount_the_route() {
		let (mut subscriber, origin) = cluster_subscriber(crate::Hop::new(1).unwrap());
		let consumer = origin.consume();
		let path = crate::Path::from_static("room/host");
		let peer = cluster::Peer {
			hop: Some(crate::Hop::new(9).unwrap()),
			cost: None,
		};
		let hops = cluster::HopPath::new(
			crate::Hops::try_from(vec![crate::Hop::UNKNOWN, crate::Hop::new(9).unwrap()]).unwrap(),
		);
		let advert = cluster::Advert { hops, cost: 0 };

		let first = subscriber.route(Some(&advert), &peer).expect("route");
		let first = subscriber
			.start_announce(path.clone(), first, Admit::Session(None))
			.unwrap();
		assert!(routed_now(&consumer, "room/host").is_some());

		let second = subscriber.route(Some(&advert), &peer).expect("route");
		let second = subscriber
			.start_announce(path.clone(), second, Admit::Session(None))
			.unwrap();
		assert!(routed_now(&consumer, "room/host").is_some());

		// Two advertisements, so it takes two unannounces to retract.
		subscriber.stop_announce(path.clone(), first, false).unwrap();
		assert!(routed_now(&consumer, "room/host").is_some());
		subscriber.stop_announce(path, second, false).unwrap();
		assert!(routed_now(&consumer, "room/host").is_none());
	}

	/// Regression: without the MoQ Cluster extension an advertisement carries no path,
	/// so there is no publisher identity to compare. PUBLISH_NAMESPACE and NAMESPACE for
	/// one namespace are then two messages about a single source, and treating the
	/// second as a different publisher would tear down what the first attached, right
	/// as a subscriber is resolving a track through it.
	#[moq_clock::test]
	async fn pathless_adverts_never_replace_the_source() {
		let (mut subscriber, origin) = cluster_subscriber(crate::Hop::new(1).unwrap());
		let consumer = origin.consume();
		let path = crate::Path::from_static("room/host");
		let peer = cluster::Peer::default();

		// What a PUBLISH_NAMESPACE with no cluster parameters resolves to.
		let first = subscriber.route(None, &peer).expect("route");
		let first = subscriber
			.start_announce(path.clone(), first, Admit::Session(None))
			.unwrap();
		assert!(routed_now(&consumer, "room/host").is_some());

		// The NAMESPACE for the same namespace arrives second.
		let second = subscriber.route(None, &peer).expect("route");
		let second = subscriber
			.start_announce(path.clone(), second, Admit::Session(None))
			.unwrap();
		assert!(routed_now(&consumer, "room/host").is_some());

		// Two advertisements, so it takes two unannounces to retract.
		subscriber.stop_announce(path.clone(), first, false).unwrap();
		assert!(routed_now(&consumer, "room/host").is_some());
		subscriber.stop_announce(path, second, false).unwrap();
		assert!(routed_now(&consumer, "room/host").is_none());
	}

	/// An update replaces the advertisement it repeats. When the replacement loops back
	/// through us it is a retraction, so the route we were holding must go: keeping it
	/// would leave subscriptions on a path the peer no longer offers.
	#[moq_clock::test]
	async fn reflected_replacement_retracts_the_route() {
		let self_origin = crate::Hop::new(5).unwrap();
		let (mut subscriber, origin) = cluster_subscriber(self_origin);
		let consumer = origin.consume();
		let path = crate::Path::from_static("room/host");
		let peer = cluster::Peer {
			hop: Some(crate::Hop::new(9).unwrap()),
			cost: None,
		};

		let clean = cluster::Advert {
			hops: hop_path(&[7, 9]),
			cost: 0,
		};
		let advert = subscriber.route(Some(&clean), &peer).expect("route");
		let id = subscriber
			.start_announce(path.clone(), advert, Admit::Session(None))
			.unwrap();
		assert!(routed_now(&consumer, "room/host").is_some());

		// The peer re-advertises the namespace over a path that now flows through us.
		let looped = cluster::Advert {
			hops: hop_path(&[7, 5, 9]),
			cost: 0,
		};
		assert!(
			subscriber.route(Some(&looped), &peer).is_none(),
			"a path containing our own Hop ID is a loop"
		);

		// That supersedes the advertisement it repeats, so the old route is retired.
		subscriber.stop_announce(path, id, false).unwrap();
		assert!(
			routed_now(&consumer, "room/host").is_none(),
			"the superseded route must not stay attached"
		);
	}

	async fn publish_namespace_updates(updates: &[cluster::Advert]) -> Vec<u8> {
		const VERSION: Version = Version::Draft18;
		let log = crate::lite::test_transport::Log::default();
		let mut writer = crate::coding::Writer::new(
			crate::transport::boxed::SendStream::new(crate::lite::test_transport::SinkSend::new(log.clone())),
			VERSION,
		);

		for (i, advert) in updates.iter().enumerate() {
			writer.varint(ietf::PublishNamespaceUpdate::ID).await.unwrap();
			writer
				.encode(&ietf::PublishNamespaceUpdate {
					// Each update consumes a request id of the peer's parity.
					request_id: RequestId(3 + 2 * i as u64),
					hops: Some(advert.hops.clone()),
					cost: Some(advert.cost),
					authorization_token: None,
				})
				.await
				.unwrap();
		}

		let writes = log.writes.lock().unwrap();
		writes.clone()
	}

	/// Build a subscriber whose peer replays `script` on one PUBLISH_NAMESPACE stream,
	/// with the advertisement already attached. The origin's driver is returned so a
	/// test can tear the origin down underneath a live advertisement.
	async fn update_harness(
		self_origin: crate::Hop,
		peer: &cluster::Peer,
		attached: &cluster::Advert,
		script: Vec<u8>,
	) -> (
		Subscriber,
		crate::origin::Consumer,
		Stream<Transport, Version>,
		crate::origin::Driver,
		crate::lite::test_transport::Log,
	) {
		const VERSION: Version = Version::Draft18;
		let session = crate::lite::test_transport::ScriptedSession::new(script);
		let log = session.log.clone();
		let (origin, driver) = crate::origin::Producer::new(crate::origin::Config::new(self_origin));
		let consumer = origin.consume();
		let (tasks, task_set) = crate::util::TaskSet::new();
		// The tests drive the loop directly, so nothing spawns; leaking keeps the
		// handles alive without a spawner.
		std::mem::forget(task_set);

		let mut subscriber = Subscriber::new(
			crate::time::sim(),
			crate::driver::Transport::new(session.clone()),
			origin,
			Control::new(true),
			None,
			peer::PeerSetup::default(),
			self_origin,
			None,
			VERSION,
			tasks,
			Default::default(),
		);

		let path = crate::Path::from_static("room/host");
		let advert = subscriber.route(Some(attached), peer).expect("route");
		let id = subscriber.start_announce(path, advert, Admit::Session(None)).unwrap();
		assert_eq!(id, 0, "the harness advertisement is the first");
		assert!(routed_now(&consumer, "room/host").is_some(), "attached to start with");

		let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), VERSION)
			.await
			.unwrap();
		(subscriber, consumer, stream, driver, log)
	}

	/// Build a subscriber whose peer replays `updates` on one PUBLISH_NAMESPACE stream,
	/// with the advertisement already attached.
	async fn reflected_harness(
		self_origin: crate::Hop,
		peer: &cluster::Peer,
		attached: &cluster::Advert,
		updates: &[cluster::Advert],
	) -> (
		Subscriber,
		crate::origin::Consumer,
		Stream<Transport, Version>,
		crate::lite::test_transport::Log,
	) {
		let script = publish_namespace_updates(updates).await;
		let (subscriber, consumer, stream, driver, log) = update_harness(self_origin, peer, attached, script).await;
		// Dropping the driver tears the origin down, so leak it: these tests only
		// need the synchronous half.
		std::mem::forget(driver);
		(subscriber, consumer, stream, log)
	}

	/// How many times `type_id` was written to the peer, as a one-byte message type.
	fn replies(log: &crate::lite::test_transport::Log, type_id: u64) -> usize {
		let writes = log.writes.lock().unwrap();
		// A REQUEST_OK is the type, a two-byte length of 1, and an empty parameter
		// block; a REQUEST_ERROR's body is longer. Counting the type at the start of
		// each framed message keeps a body byte from being mistaken for a type.
		let mut count = 0;
		let mut at = 0;
		while at + 3 <= writes.len() {
			if writes[at] as u64 == type_id {
				count += 1;
			}
			let len = u16::from_be_bytes([writes[at + 1], writes[at + 2]]) as usize;
			at += 3 + len;
		}
		count
	}

	fn peer_9() -> cluster::Peer {
		cluster::Peer {
			hop: Some(crate::Hop::new(9).unwrap()),
			cost: None,
		}
	}

	/// A clean path, and one that runs back through us (Hop ID 5).
	fn clean_and_looped() -> (cluster::Advert, cluster::Advert) {
		(
			cluster::Advert {
				hops: hop_path(&[7, 9]),
				cost: 0,
			},
			cluster::Advert {
				hops: hop_path(&[7, 5, 9]),
				cost: 0,
			},
		)
	}

	/// A reflected update detaches the route but MUST NOT end the stream. Updates ride
	/// the stream that already carries the advertisement, so closing it strands the
	/// namespace even when the peer's path goes clean again. It is also not ours to
	/// close: a peer MAY legitimately send a path carrying our Hop ID when a redundant
	/// sibling shares it, which the draft answers with "discard", not PROTOCOL_VIOLATION.
	#[moq_clock::test]
	async fn a_reflected_update_detaches_but_keeps_the_stream() {
		let self_origin = crate::Hop::new(5).unwrap();
		let peer = peer_9();
		let (clean, looped) = clean_and_looped();

		let (mut subscriber, consumer, mut stream, log) =
			reflected_harness(self_origin, &peer, &clean, &[looped]).await;

		let path = crate::Path::from_static("room/host");
		let mut attached = Some(0);
		{
			let mut run = std::pin::pin!(subscriber.run_publish_namespace_updates(
				&mut stream,
				&path,
				Held {
					cluster: Some(clean.clone()),
					epoch: None
				},
				peer,
				&mut attached,
				None,
			));

			for _ in 0..100 {
				assert!(
					futures::poll!(run.as_mut()).is_pending(),
					"the stream must stay open after a reflected update"
				);
				if routed_now(&consumer, "room/host").is_none() {
					break;
				}
				settle().await;
			}
		}

		assert!(
			routed_now(&consumer, "room/host").is_none(),
			"an unusable path must not stay attached"
		);
		assert!(attached.is_none(), "the caller must not release it a second time");
		assert_eq!(
			replies(&log, ietf::RequestOk::ID),
			1,
			"the update was applied, so it is acknowledged"
		);
	}

	/// Having kept the stream, a later usable path re-attaches on it. This is the whole
	/// reason the stream stays open.
	#[moq_clock::test]
	async fn a_clean_update_after_a_reflection_reattaches() {
		let self_origin = crate::Hop::new(5).unwrap();
		let peer = peer_9();
		let (clean, looped) = clean_and_looped();

		let (mut subscriber, consumer, mut stream, _log) =
			reflected_harness(self_origin, &peer, &clean, &[looped, clean.clone()]).await;

		let path = crate::Path::from_static("room/host");
		let mut attached = Some(0);
		{
			let mut run = std::pin::pin!(subscriber.run_publish_namespace_updates(
				&mut stream,
				&path,
				Held {
					cluster: Some(clean.clone()),
					epoch: None
				},
				peer,
				&mut attached,
				None,
			));

			// Both updates apply, then the loop parks on the exhausted script. The
			// intermediate detach is not observable (one poll can drain both messages),
			// so the end state is what this asserts; the detach itself is covered by
			// `a_reflected_update_detaches_but_keeps_the_stream`.
			for _ in 0..20 {
				assert!(futures::poll!(run.as_mut()).is_pending());
				settle().await;
			}
		}

		assert!(attached.is_some(), "the clean path must re-attach");
		assert!(
			routed_now(&consumer, "room/host").is_some(),
			"the namespace is routable again",
		);
	}

	/// A token-authorized announce re-attaching after a reflection reuses the slot its grant
	/// still holds: with a cap of one, taking a second would close the session.
	#[moq_clock::test]
	async fn a_token_announce_reattaches_on_its_own_slot() {
		let self_origin = crate::Hop::new(5).unwrap();
		let peer = peer_9();
		let (clean, looped) = clean_and_looped();
		let script = publish_namespace_updates(&[looped, clean.clone()]).await;
		let session = crate::lite::test_transport::ScriptedSession::new(script);
		let log = session.log.clone();
		let (origin, driver) = crate::origin::Producer::new(crate::origin::Config::new(self_origin));
		std::mem::forget(driver);
		let consumer = origin.consume();
		let (tasks, task_set) = crate::util::TaskSet::new();
		std::mem::forget(task_set);
		let mut subscriber = Subscriber::new(
			crate::time::sim(),
			crate::driver::Transport::new(session.clone()),
			origin,
			Control::new(true),
			None,
			peer::PeerSetup::default(),
			self_origin,
			None,
			Version::Draft18,
			tasks,
			Default::default(),
		);
		subscriber.announces = crate::session::Slots::new(1);

		// Admitted by its own token, which holds the one slot for the request's life.
		let path = crate::Path::from_static("room/host");
		let hold: crate::auth::Hold = std::sync::Arc::new(subscriber.announces.acquire().unwrap());
		let auth = crate::auth::Link::new(true);
		let mut requests = auth.handle().requests();
		let token = crate::setup::Token {
			kind: crate::setup::Token::OUT_OF_BAND,
			value: vec![0x00, 0xff],
		};
		let verdict = auth.verify_request(token, Some(hold.clone()));
		let _issued = requests.next().await.unwrap().accept(crate::auth::Grant::all());
		let answered = verdict.grant().await;
		let grant = crate::auth::RequestGrant::new(
			&subscriber.runtime,
			verdict,
			answered,
			path.clone(),
			crate::auth::RequestKind::PublishNamespace,
			Some(hold.clone()),
		)
		.unwrap();
		let advert = subscriber.route(Some(&clean), &peer).expect("route");
		subscriber
			.start_announce(path.clone(), advert, Admit::Token(Some(hold)))
			.unwrap();

		let mut stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), Version::Draft18)
			.await
			.unwrap();
		let mut attached = Some(0);
		{
			let mut run = std::pin::pin!(subscriber.run_publish_namespace_updates(
				&mut stream,
				&path,
				Held {
					cluster: Some(clean.clone()),
					epoch: None
				},
				peer,
				&mut attached,
				Some(grant),
			));
			for _ in 0..20 {
				assert!(
					futures::poll!(run.as_mut()).is_pending(),
					"the re-attach must not end the announce"
				);
				settle().await;
			}
		}

		assert!(attached.is_some(), "the clean path must re-attach");
		assert!(
			routed_now(&consumer, "room/host").is_some(),
			"the namespace is routable again"
		);
		assert!(log.closes().is_empty(), "the session stays up: {:?}", log.closes());
	}

	/// The expected update: a relay that started carrying the namespace reprices it to
	/// 0. REQUEST_UPDATE keeps an omitted parameter, so the 0 arrives explicit and alone,
	/// lands on the path already held, and is answered REQUEST_OK.
	#[moq_clock::test]
	async fn an_explicit_zero_reprices_the_held_path() {
		const VERSION: Version = Version::Draft18;
		let self_origin = crate::Hop::new(5).unwrap();
		// A free link, so the route cost is exactly what the peer advertised.
		let peer = cluster::Peer {
			hop: Some(crate::Hop::new(9).unwrap()),
			cost: Some(0),
		};
		let held = cluster::Advert {
			hops: hop_path(&[7, 9]),
			cost: 4,
		};

		let script = {
			let log = crate::lite::test_transport::Log::default();
			let mut writer = crate::coding::Writer::new(
				crate::transport::boxed::SendStream::new(crate::lite::test_transport::SinkSend::new(log.clone())),
				VERSION,
			);
			writer.varint(ietf::PublishNamespaceUpdate::ID).await.unwrap();
			writer
				.encode(&ietf::PublishNamespaceUpdate {
					request_id: RequestId(3),
					hops: None,
					cost: Some(0),
					authorization_token: None,
				})
				.await
				.unwrap();
			let writes = log.writes.lock().unwrap();
			writes.clone()
		};

		let (mut subscriber, consumer, mut stream, driver, log) =
			update_harness(self_origin, &peer, &held, script).await;
		std::mem::forget(driver);
		assert_eq!(routed_now(&consumer, "room/host").expect("routed").cost.value(), 4);

		let path = crate::Path::from_static("room/host");
		let mut attached = Some(0);
		{
			let mut run = std::pin::pin!(subscriber.run_publish_namespace_updates(
				&mut stream,
				&path,
				Held {
					cluster: Some(held.clone()),
					epoch: None
				},
				peer,
				&mut attached,
				None,
			));
			for _ in 0..20 {
				assert!(futures::poll!(run.as_mut()).is_pending(), "the stream stays open");
				settle().await;
			}
		}

		let route = routed_now(&consumer, "room/host").expect("still routed");
		assert_eq!(route.cost.value(), 0, "the explicit 0 replaced the held cost");
		let hops: Vec<_> = route.hops.iter().map(|h| h.id()).collect();
		assert_eq!(hops, vec![7, 9], "the omitted path kept its value");
		assert!(attached.is_some(), "a repricing is not a retraction");
		assert_eq!(replies(&log, ietf::RequestOk::ID), 1);
		assert_eq!(replies(&log, ietf::RequestError::ID), 0);
	}

	/// An update that cannot be applied is refused with REQUEST_ERROR and the stream
	/// closed, which withdraws the advertisement (moq-transport Section 9.5.1). The
	/// caller releases the route, so the loop must return cleanly rather than fault the
	/// session.
	#[moq_clock::test]
	async fn a_failed_update_withdraws_the_advertisement() {
		let self_origin = crate::Hop::new(5).unwrap();
		let peer = peer_9();
		let (clean, _) = clean_and_looped();
		let cheaper = cluster::Advert {
			cost: 0,
			..clean.clone()
		};

		let script = publish_namespace_updates(&[cheaper]).await;
		let (mut subscriber, _consumer, mut stream, driver, log) =
			update_harness(self_origin, &peer, &clean, script).await;

		// Tear the origin down underneath the advertisement: the route can no longer be
		// repriced, which is the one way an in-place update fails.
		drop(driver);

		let path = crate::Path::from_static("room/host");
		let mut attached = Some(0);
		let mut result = None;
		{
			let mut run = std::pin::pin!(subscriber.run_publish_namespace_updates(
				&mut stream,
				&path,
				Held {
					cluster: Some(clean.clone()),
					epoch: None
				},
				peer,
				&mut attached,
				None,
			));
			for _ in 0..20 {
				if let std::task::Poll::Ready(res) = futures::poll!(run.as_mut()) {
					result = Some(res);
					break;
				}
				settle().await;
			}
		}

		assert!(
			matches!(result, Some(Ok(Withdraw::Reject(..)))),
			"a refused update ends the stream cleanly, got {result:?}"
		);
		assert!(attached.is_some(), "the caller releases the route it attached");
		assert_eq!(
			replies(&log, ietf::RequestError::ID),
			0,
			"the caller answers once the route is released"
		);
		assert_eq!(replies(&log, ietf::RequestOk::ID), 0);
	}

	/// An update whose first Hop ID differs names a different publisher. It still
	/// replaces the advertisement in place and the stream stays open: the origin, not the
	/// session, keeps the two publishers' content apart.
	#[moq_clock::test]
	async fn an_update_that_changes_the_publisher_applies_in_place() {
		let self_origin = crate::Hop::new(5).unwrap();
		let peer = peer_9();
		let (clean, _) = clean_and_looped();
		let other_publisher = cluster::Advert {
			hops: hop_path(&[8, 9]),
			cost: 0,
		};

		let script = publish_namespace_updates(&[other_publisher]).await;
		let (mut subscriber, consumer, mut stream, driver, log) =
			update_harness(self_origin, &peer, &clean, script).await;
		std::mem::forget(driver);

		let path = crate::Path::from_static("room/host");
		let mut attached = Some(0);
		{
			let mut run = std::pin::pin!(subscriber.run_publish_namespace_updates(
				&mut stream,
				&path,
				Held {
					cluster: Some(clean.clone()),
					epoch: None
				},
				peer,
				&mut attached,
				None,
			));
			for _ in 0..20 {
				assert!(
					futures::poll!(run.as_mut()).is_pending(),
					"a publisher change must not close the stream"
				);
				settle().await;
			}
		}

		assert!(attached.is_some(), "the advertisement stays attached");
		assert_eq!(replies(&log, ietf::RequestOk::ID), 1, "REQUEST_OK went out");
		assert_eq!(replies(&log, ietf::RequestError::ID), 0);
		let route = routed_now(&consumer, "room/host").expect("still routed");
		let hops: Vec<_> = route.hops.iter().map(|h| h.id()).collect();
		assert_eq!(hops, vec![8, 9], "the held path was replaced");
	}

	/// A second PUBLISH_NAMESPACE on the stream that already carries one is not an
	/// update any more: it is the base draft's duplicate request, a protocol violation.
	#[moq_clock::test]
	async fn a_repeated_publish_namespace_is_a_duplicate() {
		const VERSION: Version = Version::Draft18;
		let self_origin = crate::Hop::new(5).unwrap();
		let peer = peer_9();
		let (clean, _) = clean_and_looped();

		let script = {
			let log = crate::lite::test_transport::Log::default();
			let mut writer = crate::coding::Writer::new(
				crate::transport::boxed::SendStream::new(crate::lite::test_transport::SinkSend::new(log.clone())),
				VERSION,
			);
			writer.varint(ietf::PublishNamespace::ID).await.unwrap();
			writer
				.encode(&ietf::PublishNamespace {
					request_id: RequestId(1),
					track_namespace: crate::Path::from_static("room/host"),
					params: ietf::AdvertParams {
						cluster: Some(cluster::Advert {
							cost: 0,
							..clean.clone()
						}),
						epoch: None,
					},
					authorization_token: None,
				})
				.await
				.unwrap();
			let writes = log.writes.lock().unwrap();
			writes.clone()
		};

		let (mut subscriber, _consumer, mut stream, driver, _log) =
			update_harness(self_origin, &peer, &clean, script).await;
		std::mem::forget(driver);

		let path = crate::Path::from_static("room/host");
		let mut attached = Some(0);
		let mut run = std::pin::pin!(subscriber.run_publish_namespace_updates(
			&mut stream,
			&path,
			Held {
				cluster: Some(clean.clone()),
				epoch: None
			},
			peer,
			&mut attached,
			None,
		));
		let mut result = None;
		for _ in 0..20 {
			if let std::task::Poll::Ready(res) = futures::poll!(run.as_mut()) {
				result = Some(res);
				break;
			}
			settle().await;
		}

		let err = result.expect("the loop ends").expect_err("a repeat is refused");
		assert!(is_protocol_violation(&err), "a duplicate request is fatal, got {err}");
	}

	/// The SUBSCRIBE_NAMESPACE stream owns every advertisement it carried. When it ends
	/// without a NAMESPACE_DONE for each, those refcounts must still be released, or the
	/// source stays attached for the rest of the session (the stream can die while the
	/// session keeps running).
	#[moq_clock::test]
	async fn namespace_stream_close_releases_live_paths() {
		let (mut subscriber, origin) = cluster_subscriber(crate::Hop::new(1).unwrap());
		let consumer = origin.consume();
		let peer = cluster::Peer::default();

		let mut live = std::collections::HashMap::new();
		for path in ["room/a", "room/b"] {
			let path = crate::Path::new(path).unwrap();
			let advert = subscriber.route(None, &peer).expect("route");
			let id = subscriber
				.start_announce(path.clone(), advert, Admit::Session(None))
				.unwrap();
			live.insert(path, id);
		}
		assert!(routed_now(&consumer, "room/a").is_some());
		assert!(routed_now(&consumer, "room/b").is_some());

		// What the stream's exit path does with whatever it still holds.
		for (path, id) in live {
			subscriber.stop_announce(path, id, false).unwrap();
		}

		assert!(routed_now(&consumer, "room/a").is_none(), "room/a leaked a refcount");
		assert!(routed_now(&consumer, "room/b").is_none(), "room/b leaked a refcount");
	}

	/// PUBLISH offers one track, but a source attaches per namespace and serves every
	/// track under it. Rather than invent a namespace-level source from a track-level
	/// offer, decline the request with NOT_SUPPORTED and leave the session running.
	#[moq_clock::test]
	async fn publish_is_rejected_without_announcing() {
		for version in [Version::Draft18] {
			// An open gate, so the rejection actually reaches the wire.
			let gate = kio::Producer::new(true);
			let session = crate::lite::test_transport::SinkSession::gated_bi(gate.consume());
			let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
			let consumer = origin.consume();
			let (tasks, task_set) = crate::util::TaskSet::new();
			std::mem::forget(task_set);

			let mut subscriber = Subscriber::new(
				crate::time::sim(),
				crate::driver::Transport::new(session.clone()),
				origin,
				Control::new(false),
				None,
				peer::PeerSetup::default(),
				crate::Hop::new(1).unwrap(),
				None,
				version,
				tasks,
				Default::default(),
			);

			let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), version)
				.await
				.unwrap();
			let msg = ietf::Publish {
				request_id: RequestId(1),
				track_namespace: crate::Path::from_static("room/host"),
				track_name: "video".into(),
				track_alias: 7,
				largest_location: None,
				forward: true,
				properties: ietf::Properties::default(),
				subscriber_priority: None,
				group_order: None,
				filter: None,
			};

			// Errors are surfaced to the peer on the stream, not raised as a session error.
			subscriber.run_publish_stream(stream, msg).await.unwrap();
			moq_clock::sleep(Duration::from_millis(1)).await;

			assert!(
				routed_now(&consumer, "room/host").is_none(),
				"a rejected PUBLISH must not announce a broadcast"
			);
			// Encode the reply we expect rather than matching the reason alone, so an error
			// code regressing to something outside the draft's table cannot slip through.
			// NOT_SUPPORTED is 0x3 in every registry, which is what makes it comparable here.
			let expected = {
				const NOT_SUPPORTED: u64 = 0x3;

				let log = crate::lite::test_transport::Log::default();
				let mut writer = crate::coding::Writer::new(
					crate::transport::boxed::SendStream::new(crate::lite::test_transport::SinkSend::new(log.clone())),
					version,
				);

				writer.varint(ietf::RequestError::ID).await.unwrap();
				writer
					.encode(&ietf::RequestError {
						error_code: NOT_SUPPORTED,
						reason_phrase: "PUBLISH is not supported".into(),
						retry_interval: 0,
					})
					.await
					.unwrap();

				log.writes.lock().unwrap().clone()
			};

			assert_eq!(
				occurrences(&session.log, &expected),
				1,
				"{version} must decline the PUBLISH as NOT_SUPPORTED"
			);
		}
	}

	/// A subscriber whose session grant covers only `other`, with an app auth acceptor
	/// wired in, so a PUBLISH_NAMESPACE for `room/alice` falls to its request token. The
	/// subscribe stream's reader is scripted with `first_script` (a REQUEST_UPDATE, for the
	/// renewal test). Returns the presented credential to keep it alive.
	fn auth_announce_harness(
		version: Version,
		first_script: Vec<u8>,
	) -> (
		Subscriber,
		crate::auth::Requests,
		crate::auth::Credential,
		origin::Consumer,
		crate::lite::test_transport::ScriptedSession,
	) {
		auth_announce_harness_on(
			version,
			crate::lite::test_transport::ScriptedSession::per_stream(vec![first_script]),
		)
	}

	/// [`auth_announce_harness`] over a given session, such as one that finishes the stream
	/// after its script.
	fn auth_announce_harness_on<S: crate::transport::poll::Boxable + Clone>(
		version: Version,
		session: S,
	) -> (
		Subscriber,
		crate::auth::Requests,
		crate::auth::Credential,
		origin::Consumer,
		S,
	) {
		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let consumer = origin.consume();
		let (tasks, task_set) = crate::util::TaskSet::new();
		std::mem::forget(task_set);
		let peer_setup = peer::PeerSetup::default();
		peer_setup.set(peer::Peer::default());

		let auth = crate::auth::Link::new(true);
		let requests = auth.handle().requests();
		let cred = auth.present(bytes::Bytes::from_static(b"cred"), true).unwrap();
		auth.granted(
			0,
			crate::auth::Grant {
				publish: crate::Patterns::new(),
				subscribe: crate::Pattern::subtree("other").unwrap().into(),
				expires: None,
			},
		);
		// We let the peer announce only "other".
		auth.handle().authorize(&crate::auth::Grant {
			publish: crate::Pattern::subtree("other").unwrap().into(),
			subscribe: crate::Patterns::new(),
			expires: None,
		});
		assert!(
			!auth.within_limit(crate::auth::Direction::Subscribe, "room/alice"),
			"the limit must not cover the announced path"
		);

		let subscriber = Subscriber::new(
			crate::time::sim(),
			crate::driver::Transport::new(session.clone()),
			origin,
			// The announcing peer is the server here, so its update IDs are odd.
			Control::new(true),
			None,
			peer_setup,
			crate::Hop::new(1).unwrap(),
			None,
			version,
			tasks,
			Default::default(),
		)
		.with_auth(auth);

		(subscriber, requests, cred, consumer, session)
	}

	/// A grant to publish everything, lapsing in `secs` (or never), on the subscriber's clock:
	/// what an announcing peer's token must carry.
	fn publish_grant_expiring(runtime: &crate::time::Deadlines, secs: Option<u64>) -> crate::auth::Grant {
		crate::auth::Grant {
			publish: crate::Pattern::all().into(),
			subscribe: crate::Patterns::new(),
			expires: secs.map(|s| runtime.now().checked_add(std::time::Duration::from_secs(s)).unwrap()),
		}
	}
	/// An out-of-band request token, the only kind a session verifies.
	/// A request token whose Token Type takes two varint bytes.
	fn announce_token() -> crate::ietf::token::RequestToken {
		crate::ietf::token::RequestToken(crate::setup::Token {
			kind: crate::setup::Token::OUT_OF_BAND,
			value: vec![0x00, 0xff],
		})
	}

	fn token_publish_namespace() -> ietf::PublishNamespace {
		ietf::PublishNamespace {
			request_id: RequestId(1),
			track_namespace: crate::Path::from_static("room/alice"),
			params: Default::default(),
			authorization_token: Some(announce_token()),
		}
	}

	/// One REQUEST_UPDATE on the announce stream carrying a fresh token, framed as the peer
	/// sends it.
	async fn publish_namespace_update_with_token(version: Version) -> Vec<u8> {
		let log = crate::lite::test_transport::Log::default();
		let mut writer = crate::coding::Writer::new(
			crate::transport::boxed::SendStream::new(crate::lite::test_transport::SinkSend::new(log.clone())),
			version,
		);
		let msg = ietf::PublishNamespaceUpdate {
			request_id: RequestId(3),
			hops: None,
			cost: None,
			authorization_token: Some(announce_token()),
		};
		writer.varint(ietf::PublishNamespaceUpdate::ID).await.unwrap();
		writer.encode(&msg).await.unwrap();
		log.writes.lock().unwrap().clone()
	}

	/// One REQUEST_UPDATE on the announce stream carrying both a fresh token and new cluster
	/// parameters (HOP_PATH/ROUTE_COST), framed as the peer sends it.
	async fn publish_namespace_update_with_token_and_cluster(version: Version) -> Vec<u8> {
		let log = crate::lite::test_transport::Log::default();
		let mut writer = crate::coding::Writer::new(
			crate::transport::boxed::SendStream::new(crate::lite::test_transport::SinkSend::new(log.clone())),
			version,
		);
		let msg = ietf::PublishNamespaceUpdate {
			request_id: RequestId(3),
			hops: Some(hop_path(&[7, 9])),
			cost: Some(0),
			authorization_token: Some(announce_token()),
		};
		writer.varint(ietf::PublishNamespaceUpdate::ID).await.unwrap();
		writer.encode(&msg).await.unwrap();
		log.writes.lock().unwrap().clone()
	}

	/// A request token on a PUBLISH_NAMESPACE the session grant does not cover authorizes
	/// the announce: the subscriber verifies it through the acceptor and attaches the route.
	#[moq_clock::test]
	async fn a_publish_namespace_token_authorizes_an_uncovered_announce() {
		const VERSION: Version = Version::Draft18;
		let (mut subscriber, mut requests, _cred, consumer, session) = auth_announce_harness(VERSION, Vec::new());
		let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), VERSION)
			.await
			.unwrap();

		let acceptor = async {
			let mut held = Vec::new();
			loop {
				let request = requests.next().await.expect("a request");
				held.push(request.accept(crate::auth::Grant::all()));
			}
		};
		let mut acceptor = std::pin::pin!(acceptor);
		let mut run = std::pin::pin!(subscriber.run_publish_namespace_stream(
			stream,
			token_publish_namespace(),
			cluster::Peer::default(),
			None,
		));

		let mut announced = false;
		for _ in 0..500 {
			let _ = futures::poll!(acceptor.as_mut());
			assert!(futures::poll!(run.as_mut()).is_pending(), "the announce ended early");
			if routed_now(&consumer, "room/alice").is_some() {
				announced = true;
				break;
			}
			settle().await;
		}
		assert!(announced, "a valid request token must authorize the announce");
	}

	/// An announce's grant ending while its first answer waits on a peer that stopped
	/// reading retracts the route at once, not once the answer drains: the peer is told
	/// only afterwards.
	#[moq_clock::test]
	async fn a_revoked_token_retracts_an_announce_whose_ok_is_blocked() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			let gate = kio::Producer::new(false);
			let session = crate::lite::test_transport::SinkSession::gated_bi(gate.consume());
			let (mut subscriber, mut requests, _cred, consumer, session) =
				auth_announce_harness_on(version, session.clone());
			let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), version)
				.await
				.unwrap();
			let mut run = std::pin::pin!(subscriber.run_publish_namespace_stream(
				stream,
				token_publish_namespace(),
				cluster::Peer::default(),
				None,
			));

			let accepted = async { requests.next().await.expect("a request") };
			let request = match futures::future::select(std::pin::pin!(accepted), run.as_mut()).await {
				futures::future::Either::Left((request, _)) => request,
				futures::future::Either::Right((res, _)) => panic!("{version}: ended unverified: {res:?}"),
			};
			let enforced = request.accept(crate::auth::Grant::all());
			for _ in 0..50 {
				assert!(futures::poll!(run.as_mut()).is_pending(), "{version}: ended early");
				settle().await;
			}
			assert!(routed_now(&consumer, "room/alice").is_some(), "{version}: never routed");
			assert!(
				session.log.writes.lock().unwrap().is_empty(),
				"{version}: the OK was not held"
			);

			enforced.revoke(crate::SessionError::Unauthorized, "revoked");
			for _ in 0..10 {
				assert!(
					futures::poll!(run.as_mut()).is_pending(),
					"{version}: told a peer that cannot read"
				);
				settle().await;
			}
			assert!(
				routed_now(&consumer, "room/alice").is_none(),
				"{version}: routed until the peer could be told"
			);

			let Ok(mut open) = gate.write() else {
				panic!("request stream gate closed");
			};
			*open = true;
			drop(open);
			let _ = moq_clock::timeout(std::time::Duration::from_secs(1), run.as_mut())
				.await
				.unwrap_or_else(|_| panic!("{version}: outlived its grant"));

			let bytes: Vec<u8> = session
				.log
				.trail()
				.into_iter()
				.filter_map(|(_, sent)| match sent {
					crate::lite::test_transport::Sent::Write(buf) => Some(buf),
					_ => None,
				})
				.flatten()
				.collect();
			let mut sent = Vec::new();
			let mut rest = bytes.as_slice();
			while !rest.is_empty() {
				sent.push(crate::coding::decode_varint(&mut rest, version).unwrap());
				let len = usize::from(u16::from_be_bytes([rest[0], rest[1]]));
				rest = &rest[2 + len..];
			}
			let expected = vec![ietf::RequestOk::ID];
			assert_eq!(sent, expected, "{version}");
			assert!(session.log.closes().is_empty(), "{version}: the session stays up");
		}
	}

	/// A refused request token is answered UNAUTHORIZED and the announce is not attached; the
	/// session is untouched.
	#[moq_clock::test]
	async fn a_refused_publish_namespace_token_is_not_announced() {
		const VERSION: Version = Version::Draft18;
		let (mut subscriber, mut requests, _cred, consumer, session) = auth_announce_harness(VERSION, Vec::new());
		let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), VERSION)
			.await
			.unwrap();

		let acceptor = async {
			let request = requests.next().await.expect("a request");
			request.reject(crate::SessionError::Unauthorized, "no");
			std::future::pending::<()>().await
		};
		let mut acceptor = std::pin::pin!(acceptor);
		let mut run = std::pin::pin!(subscriber.run_publish_namespace_stream(
			stream,
			token_publish_namespace(),
			cluster::Peer::default(),
			None,
		));

		let mut ended = false;
		for _ in 0..500 {
			let _ = futures::poll!(acceptor.as_mut());
			if futures::poll!(run.as_mut()).is_ready() {
				ended = true;
				break;
			}
			settle().await;
		}
		assert!(ended, "a refused announce ends its own stream");
		assert!(
			routed_now(&consumer, "room/alice").is_none(),
			"a refused token must not attach the route"
		);
	}

	/// What the peer's AUTH_OK grants us never authorizes what the peer asks of us: with a
	/// covering peer grant and no limit, the application still verifies the announce's token,
	/// and its refusal refuses the announce.
	#[moq_clock::test]
	async fn a_covering_peer_grant_never_skips_the_verifier() {
		const VERSION: Version = Version::Draft18;
		let auth = crate::auth::Link::new(true);
		let _cred = auth.present(bytes::Bytes::from_static(b"cred"), true).unwrap();
		auth.granted(0, crate::auth::Grant::all());
		let (mut subscriber, mut requests, consumer, session) = announce_harness_with(VERSION, auth);
		let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), VERSION)
			.await
			.unwrap();

		let consulted = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
		let acceptor = {
			let consulted = consulted.clone();
			async move {
				let request = requests.next().await.expect("a request");
				consulted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
				request.reject(crate::SessionError::Unauthorized, "no");
				std::future::pending::<()>().await
			}
		};
		let mut acceptor = std::pin::pin!(acceptor);
		let mut run = std::pin::pin!(subscriber.run_publish_namespace_stream(
			stream,
			token_publish_namespace(),
			cluster::Peer::default(),
			None,
		));
		let mut ended = false;
		for _ in 0..500 {
			let _ = futures::poll!(acceptor.as_mut());
			if futures::poll!(run.as_mut()).is_ready() {
				ended = true;
				break;
			}
			settle().await;
		}
		assert_eq!(
			consulted.load(std::sync::atomic::Ordering::Relaxed),
			1,
			"the verifier ran"
		);
		assert!(ended, "the refused announce ends");
		assert!(routed_now(&consumer, "room/alice").is_none(), "and is never attached");
	}

	/// Every token is verified, even on an announce the limit already covers, so a verified
	/// one stands outside the limit: narrowing the limit away from it leaves it attached, as the
	/// publisher, which counts it token-backed, expects. Admitted on the limit instead, the
	/// narrowing would withhold a route the publisher still believes its token backs.
	#[moq_clock::test]
	async fn a_covering_limit_still_verifies_the_token() {
		const VERSION: Version = Version::Draft18;
		let limit = |subtree: &str| crate::auth::Grant {
			publish: crate::Pattern::subtree(subtree).unwrap().into(),
			subscribe: crate::Patterns::new(),
			expires: None,
		};
		let auth = crate::auth::Link::new(false);
		auth.handle().authorize(&limit("room"));
		let (mut subscriber, mut requests, consumer, session) = announce_harness_with(VERSION, auth.clone());
		let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), VERSION)
			.await
			.unwrap();
		let consulted = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
		let acceptor = {
			let consulted = consulted.clone();
			async move {
				let request = requests.next().await.expect("a request");
				consulted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
				let _held = request.accept(crate::auth::Grant::all());
				std::future::pending::<()>().await
			}
		};
		let mut acceptor = std::pin::pin!(acceptor);
		let mut run = std::pin::pin!(subscriber.run_publish_namespace_stream(
			stream,
			token_publish_namespace(),
			cluster::Peer::default(),
			None,
		));
		let mut routed = false;
		for _ in 0..500 {
			let _ = futures::poll!(acceptor.as_mut());
			assert!(futures::poll!(run.as_mut()).is_pending(), "the announce ended early");
			if routed_now(&consumer, "room/alice").is_some() {
				routed = true;
				break;
			}
			settle().await;
		}
		assert!(routed, "the verified token admits the announce");
		assert_eq!(
			consulted.load(std::sync::atomic::Ordering::Relaxed),
			1,
			"the verifier ran though the limit covers the announce"
		);

		auth.handle().authorize(&limit("other"));
		for _ in 0..50 {
			let _ = futures::poll!(acceptor.as_mut());
			assert!(futures::poll!(run.as_mut()).is_pending(), "the announce ended");
			settle().await;
		}
		assert!(
			routed_now(&consumer, "room/alice").is_some(),
			"narrowing the limit withheld the token-verified announce"
		);
	}

	/// A token-authorized announce stands outside the limit while it lasts: a narrower limit
	/// leaves it attached, and only its own end retracts it.
	#[moq_clock::test]
	async fn a_token_authorized_announce_is_exempt_from_the_limit() {
		const VERSION: Version = Version::Draft18;
		let auth = crate::auth::Link::new(false);
		auth.handle().authorize(&crate::auth::Grant::default());
		let (mut subscriber, mut requests, consumer, session) = announce_harness_with(VERSION, auth.clone());
		let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), VERSION)
			.await
			.unwrap();
		let acceptor = async move {
			let request = requests.next().await.expect("a request");
			let _held = request.accept(crate::auth::Grant::all());
			std::future::pending::<()>().await
		};
		let mut acceptor = std::pin::pin!(acceptor);
		let mut run = std::pin::pin!(subscriber.run_publish_namespace_stream(
			stream,
			token_publish_namespace(),
			cluster::Peer::default(),
			None,
		));
		let mut routed = false;
		for _ in 0..500 {
			let _ = futures::poll!(acceptor.as_mut());
			assert!(futures::poll!(run.as_mut()).is_pending(), "the announce ended early");
			if routed_now(&consumer, "room/alice").is_some() {
				routed = true;
				break;
			}
			settle().await;
		}
		assert!(routed, "the token admits the announce outside the limit");

		auth.handle().authorize(&crate::auth::Grant {
			publish: crate::Pattern::subtree("other").unwrap().into(),
			subscribe: crate::Patterns::new(),
			expires: None,
		});
		for _ in 0..50 {
			let _ = futures::poll!(acceptor.as_mut());
			assert!(futures::poll!(run.as_mut()).is_pending(), "the announce ended");
			settle().await;
		}
		assert!(
			routed_now(&consumer, "room/alice").is_some(),
			"a narrower limit leaves the token-authorized announce attached"
		);
	}

	/// A token-authorized announce authorizes the tracks subscribed under it, outside the
	/// limit, and only while its token lasts.
	#[moq_clock::test]
	async fn a_token_authorized_announce_admits_its_subscriptions() {
		const VERSION: Version = Version::Draft18;
		let auth = crate::auth::Link::new(false);
		auth.handle().authorize(&crate::auth::Grant::default());
		// The announce's peer says nothing; the subscription's accepts it.
		let subscribe_ok = {
			let log = crate::lite::test_transport::Log::default();
			let mut writer = crate::coding::Writer::new(
				crate::transport::boxed::SendStream::new(crate::lite::test_transport::SinkSend::new(log.clone())),
				VERSION,
			);
			writer.varint(ietf::SubscribeOk::ID).await.unwrap();
			let ok = ietf::SubscribeOk {
				track_alias: 7,
				largest: None,
				properties: Default::default(),
			};
			writer.encode(&ok).await.unwrap();
			log.writes.lock().unwrap().clone()
		};
		let session = crate::lite::test_transport::ScriptedSession::per_stream(vec![Vec::new(), subscribe_ok]);
		let (mut subscriber, mut requests, consumer, session) = announce_harness_on(VERSION, auth, session);
		// Serve the route, which the harness leaves undriven.
		let (tasks, mut task_set) = crate::util::TaskSet::new();
		subscriber.tasks = tasks;
		let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), VERSION)
			.await
			.unwrap();
		let mut run = std::pin::pin!(subscriber.run_publish_namespace_stream(
			stream,
			token_publish_namespace(),
			cluster::Peer::default(),
			None,
		));

		let mut next = std::pin::pin!(requests.next());
		let mut issued = None;
		for _ in 0..500 {
			assert!(futures::poll!(run.as_mut()).is_pending(), "the announce ended early");
			if let Poll::Ready(request) = futures::poll!(next.as_mut()) {
				issued = Some(request.expect("a request").accept(crate::auth::Grant::all()));
				break;
			}
			settle().await;
		}
		let issued = issued.expect("the announce never presented its token");

		let mut following = consumer.follow("room/alice", None).unwrap();
		let mut routing = std::pin::pin!(following.available());
		let mut broadcast = None;
		for _ in 0..500 {
			assert!(futures::poll!(run.as_mut()).is_pending(), "the announce ended early");
			let _ = task_set.poll(&kio::Waiter::noop());
			if let Poll::Ready(routed) = futures::poll!(routing.as_mut()) {
				broadcast = Some(routed.expect("the announce routes the broadcast"));
				break;
			}
			settle().await;
		}
		let broadcast = broadcast.expect("the broadcast never routed");

		let track = broadcast.track("video").unwrap();
		let mut subscribing = std::pin::pin!(track.subscribe(None));
		let mut subscribed = None;
		for _ in 0..500 {
			assert!(futures::poll!(run.as_mut()).is_pending(), "the announce ended early");
			let _ = task_set.poll(&kio::Waiter::noop());
			if let Poll::Ready(res) = futures::poll!(subscribing.as_mut()) {
				subscribed = Some(res.expect("the token authorizes the subscription"));
				break;
			}
			settle().await;
		}
		let mut subscribed = subscribed.expect("the subscription never started");

		issued.revoke(crate::SessionError::Unauthorized, "revoked");
		let mut ended = None;
		let mut retracted = false;
		for _ in 0..500 {
			if !retracted {
				retracted = futures::poll!(run.as_mut()).is_ready();
			}
			let _ = task_set.poll(&kio::Waiter::noop());
			if let Poll::Ready(res) = subscribed.poll_recv_group(&kio::Waiter::noop()) {
				ended = Some(res.err());
				break;
			}
			settle().await;
		}
		assert!(
			matches!(ended, Some(Some(Error::Unauthorized))),
			"the subscription must end with its token: {ended:?}"
		);
	}

	/// A REQUEST_UPDATE the acceptor renews keeps a token-authorized announce alive past the
	/// old grant's expiry: the subscriber re-verifies the token off the announce stream and
	/// re-arms the deadline.
	#[moq_clock::test]
	async fn a_publish_namespace_renewal_extends_past_the_old_expiry() {
		for version in [Version::Draft18] {
			let (mut subscriber, mut requests, _cred, consumer, session) =
				auth_announce_harness(version, publish_namespace_update_with_token(version).await);
			let rt = subscriber.runtime.clone();
			let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), version)
				.await
				.unwrap();

			let answered = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
			let acceptor = {
				let answered = answered.clone();
				async move {
					let mut held = Vec::new();
					// The first grant lapses in 60s; the renewal never expires.
					let first = requests.next().await.expect("a request");
					held.push(first.accept(publish_grant_expiring(&rt, Some(60))));
					answered.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
					let renewal = requests.next().await.expect("a renewal");
					held.push(renewal.accept(publish_grant_expiring(&rt, None)));
					answered.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
					std::future::pending::<()>().await
				}
			};
			let mut acceptor = std::pin::pin!(acceptor);
			let mut run = std::pin::pin!(subscriber.run_publish_namespace_stream(
				stream,
				token_publish_namespace(),
				cluster::Peer::default(),
				None,
			));

			for _ in 0..500 {
				let _ = futures::poll!(acceptor.as_mut());
				assert!(
					futures::poll!(run.as_mut()).is_pending(),
					"the announce ended during setup"
				);
				if answered.load(std::sync::atomic::Ordering::Relaxed) >= 2 {
					break;
				}
				settle().await;
			}
			assert_eq!(
				answered.load(std::sync::atomic::Ordering::Relaxed),
				2,
				"acceptor never answered both tokens"
			);
			// Let the loop apply the renewal it read.
			for _ in 0..20 {
				let _ = futures::poll!(acceptor.as_mut());
				assert!(futures::poll!(run.as_mut()).is_pending());
				settle().await;
			}

			// Past the original 60s expiry: the renewal re-armed the deadline, so the announce
			// stays attached.
			moq_clock::sim::advance(std::time::Duration::from_secs(120)).await;
			for _ in 0..50 {
				let _ = futures::poll!(acceptor.as_mut());
				assert!(
					futures::poll!(run.as_mut()).is_pending(),
					"renewal did not extend the announce"
				);
				settle().await;
			}
			assert!(
				routed_now(&consumer, "room/alice").is_some(),
				"the renewed announce must still be attached"
			);
		}
	}

	/// A lapsed grant retracts its announce at once, even while the withdrawal waits on a peer
	/// that never acknowledges the closed stream.
	#[moq_clock::test]
	async fn a_lapsed_announce_grant_retracts_before_the_peer_reads() {
		const VERSION: Version = Version::Draft18;
		let session = crate::lite::test_transport::ScriptedSession::per_stream(vec![Vec::new()]).with_unacked_fin();
		let (mut subscriber, mut requests, _cred, consumer, session) =
			auth_announce_harness_on(VERSION, session.clone());
		let rt = subscriber.runtime.clone();
		let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), VERSION)
			.await
			.unwrap();

		let acceptor = async move {
			let request = requests.next().await.expect("a request");
			let _held = request.accept(publish_grant_expiring(&rt, Some(60)));
			std::future::pending::<()>().await
		};
		let mut acceptor = std::pin::pin!(acceptor);
		let mut run = std::pin::pin!(subscriber.run_publish_namespace_stream(
			stream,
			token_publish_namespace(),
			cluster::Peer::default(),
			None,
		));

		let mut routed = false;
		for _ in 0..500 {
			let _ = futures::poll!(acceptor.as_mut());
			assert!(futures::poll!(run.as_mut()).is_pending(), "the announce ended early");
			if routed_now(&consumer, "room/alice").is_some() {
				routed = true;
				break;
			}
			settle().await;
		}
		assert!(routed, "the token must authorize the announce");

		moq_clock::sim::advance(std::time::Duration::from_secs(120)).await;
		let mut retracted = false;
		for _ in 0..50 {
			let _ = futures::poll!(acceptor.as_mut());
			assert!(
				futures::poll!(run.as_mut()).is_pending(),
				"the withdrawal waits on the peer's acknowledgement"
			);
			if routed_now(&consumer, "room/alice").is_none() {
				retracted = true;
				break;
			}
			settle().await;
		}
		assert!(
			retracted,
			"a lapsed grant must retract the route without waiting on the peer"
		);
	}

	/// An update repeating its token withdraws a token-authorized announce, retracting the route
	/// at once even while the withdrawal waits on a peer that never acknowledges it.
	#[moq_clock::test]
	async fn a_repeated_token_update_retracts_before_the_peer_reads() {
		const VERSION: Version = Version::Draft18;
		const TOKEN: &[u8] = &[0x03, 0x00, 0xAA];
		// REQUEST_UPDATE 3 carrying the same AUTHORIZATION TOKEN twice: a count, then two
		// delta-encoded token parameters.
		let mut body = vec![3, 2, 0x03, TOKEN.len() as u8];
		body.extend(TOKEN);
		body.extend([0, TOKEN.len() as u8]);
		body.extend(TOKEN);
		let mut script = vec![ietf::PublishNamespaceUpdate::ID as u8, 0, body.len() as u8];
		script.extend(body);

		let session = crate::lite::test_transport::ScriptedSession::per_stream(vec![script]).with_unacked_fin();
		let (mut subscriber, mut requests, _cred, consumer, session) =
			auth_announce_harness_on(VERSION, session.clone());
		let rt = subscriber.runtime.clone();
		let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), VERSION)
			.await
			.unwrap();

		let acceptor = async move {
			let request = requests.next().await.expect("a request");
			let _held = request.accept(publish_grant_expiring(&rt, None));
			std::future::pending::<()>().await
		};
		let mut acceptor = std::pin::pin!(acceptor);
		let mut run = std::pin::pin!(subscriber.run_publish_namespace_stream(
			stream,
			token_publish_namespace(),
			cluster::Peer::default(),
			None,
		));

		for _ in 0..200 {
			let _ = futures::poll!(acceptor.as_mut());
			assert!(
				futures::poll!(run.as_mut()).is_pending(),
				"the withdrawal waits on the peer's acknowledgement"
			);
			settle().await;
		}
		assert!(
			routed_now(&consumer, "room/alice").is_none(),
			"a repeated token must retract the route without waiting on the peer"
		);
		assert!(session.log.closes().is_empty(), "the session stays up");
	}

	/// Per draft-18 section 10.9.1, a refused PUBLISH_NAMESPACE
	/// renewal ends only that announce at once, by closing its stream, not by waiting for
	/// the old 60s grant to lapse. The session stays up.
	#[moq_clock::test]
	async fn a_refused_publish_namespace_renewal_ends_the_announce() {
		for version in [Version::Draft18] {
			let (mut subscriber, mut requests, _cred, _consumer, session) =
				auth_announce_harness(version, publish_namespace_update_with_token(version).await);
			let rt = subscriber.runtime.clone();
			let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), version)
				.await
				.unwrap();

			let answered = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
			let acceptor = {
				let answered = answered.clone();
				async move {
					// The first grant lasts 60s; the renewal is refused.
					let first = requests.next().await.expect("a request");
					let _issued = first.accept(publish_grant_expiring(&rt, Some(60)));
					answered.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
					let renewal = requests.next().await.expect("a renewal");
					renewal.reject(crate::SessionError::Unauthorized, "no");
					answered.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
					std::future::pending::<()>().await
				}
			};
			let mut acceptor = std::pin::pin!(acceptor);
			let mut run = std::pin::pin!(subscriber.run_publish_namespace_stream(
				stream,
				token_publish_namespace(),
				cluster::Peer::default(),
				None,
			));

			// No time is advanced: the refusal must end the announce on its own, not by letting the
			// 60s grant lapse.
			let mut result = None;
			for _ in 0..500 {
				let _ = futures::poll!(acceptor.as_mut());
				if let std::task::Poll::Ready(res) = futures::poll!(run.as_mut()) {
					result = Some(res);
					break;
				}
				settle().await;
			}
			assert_eq!(
				answered.load(std::sync::atomic::Ordering::Relaxed),
				2,
				"acceptor never answered both tokens"
			);
			assert!(
				matches!(result, Some(Ok(()))),
				"a refused renewal ends the announce cleanly by closing the stream, got {result:?}"
			);
			assert!(
				session.log.closes().is_empty(),
				"the session must stay up: {:?}",
				session.log.closes()
			);
		}
	}

	/// Where tokens decide, a token arriving on an update for an announce the limit admitted
	/// without one is refused rather than taken unread: it ends only that announce, unverified.
	#[moq_clock::test]
	async fn a_token_on_an_announce_admitted_without_one_is_refused() {
		for version in [Version::Draft18] {
			let (mut subscriber, mut requests, _cred, consumer, session) =
				auth_announce_harness(version, publish_namespace_update_with_token(version).await);
			let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), version)
				.await
				.unwrap();
			let tokenless = ietf::PublishNamespace {
				request_id: RequestId(1),
				track_namespace: crate::Path::from_static("other/alice"),
				params: Default::default(),
				authorization_token: None,
			};
			let mut run = std::pin::pin!(subscriber.run_publish_namespace_stream(
				stream,
				tokenless,
				cluster::Peer::default(),
				None,
			));

			let mut result = None;
			for _ in 0..500 {
				if let std::task::Poll::Ready(res) = futures::poll!(run.as_mut()) {
					result = Some(res);
					break;
				}
				settle().await;
			}
			assert!(
				matches!(result, Some(Ok(()))),
				"{version}: the update's token ends the announce, got {result:?}"
			);
			assert!(routed_now(&consumer, "other/alice").is_none(), "{version}");
			assert!(
				requests.poll_next(&kio::Waiter::noop()).is_pending(),
				"{version}: the token reached the verifier"
			);
			assert!(session.log.closes().is_empty(), "{version}: the session closed");
		}
	}

	/// A REQUEST_UPDATE carrying both a fresh token and cluster parameters applies both: the
	/// renewal re-arms the grant and the HOP_PATH/ROUTE_COST re-route the advertisement,
	/// answered with the renewal's single response. Before the fix the token branch
	/// short-circuited the loop and the cluster parameters on the same update were dropped.
	#[moq_clock::test]
	async fn an_update_applies_both_a_renewal_and_cluster_params() {
		const VERSION: Version = Version::Draft18;
		let (mut subscriber, mut requests, _cred, consumer, session) =
			auth_announce_harness(VERSION, publish_namespace_update_with_token_and_cluster(VERSION).await);
		let rt = subscriber.runtime.clone();
		let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), VERSION)
			.await
			.unwrap();

		// A negotiated peer over a free link, so the advertised cost is the route's warm cost.
		let peer = cluster::Peer {
			hop: Some(crate::Hop::new(9).unwrap()),
			cost: Some(0),
		};
		// The announce arrives already routed at cost 4; the update re-routes it to cost 0.
		let mut initial = token_publish_namespace();
		initial.params.cluster = Some(cluster::Advert {
			hops: hop_path(&[7, 9]),
			cost: 4,
		});

		let acceptor = {
			let rt = rt.clone();
			async move {
				let mut held = Vec::new();
				let first = requests.next().await.expect("a request");
				held.push(first.accept(publish_grant_expiring(&rt, Some(60))));
				let renewal = requests.next().await.expect("a renewal");
				held.push(renewal.accept(publish_grant_expiring(&rt, None)));
				std::future::pending::<()>().await
			}
		};
		let mut acceptor = std::pin::pin!(acceptor);
		let mut run = std::pin::pin!(subscriber.run_publish_namespace_stream(stream, initial, peer, None));

		let mut rerouted = false;
		for _ in 0..500 {
			let _ = futures::poll!(acceptor.as_mut());
			assert!(futures::poll!(run.as_mut()).is_pending(), "the announce ended early");
			if routed_now(&consumer, "room/alice").is_some_and(|route| route.cost.value() == 0) {
				rerouted = true;
				break;
			}
			settle().await;
		}
		assert!(
			rerouted,
			"the update's cluster parameters must re-route the announce (cost 4 to 0), not be dropped by the renewal"
		);
	}

	/// One PUBLISH_NAMESPACE REQUEST_UPDATE carrying a fresh token, keyed to its own Request ID,
	/// optionally changing the HOP_PATH and/or ROUTE_COST, framed as the peer sends it.
	async fn publish_namespace_update_token_rid(
		version: Version,
		rid: u64,
		hops: Option<cluster::HopPath>,
		cost: Option<u64>,
	) -> Vec<u8> {
		let log = crate::lite::test_transport::Log::default();
		let mut writer = crate::coding::Writer::new(
			crate::transport::boxed::SendStream::new(crate::lite::test_transport::SinkSend::new(log.clone())),
			version,
		);
		let msg = ietf::PublishNamespaceUpdate {
			request_id: RequestId(rid),
			hops,
			cost,
			authorization_token: Some(announce_token()),
		};
		writer.varint(ietf::PublishNamespaceUpdate::ID).await.unwrap();
		writer.encode(&msg).await.unwrap();
		log.writes.lock().unwrap().clone()
	}

	/// Two announce renewals that arrive while a first renewal's verdict is still pending are
	/// each verified, and each one's cluster delta is applied, not collapsed. The middle update
	/// (0x05) changes only ROUTE_COST (4 to 1) and the last (0x07) only HOP_PATH, so the final
	/// cost shows the middle delta survived; the single slot this replaced dropped 0x05,
	/// leaving the cost at the initial 4 and never verifying its token.
	#[moq_clock::test]
	async fn two_announce_renewals_buffered_behind_a_pending_verdict_each_apply_and_answer() {
		const VERSION: Version = Version::Draft18;

		let mut script = publish_namespace_update_token_rid(VERSION, 0x03, None, None).await;
		script.extend(publish_namespace_update_token_rid(VERSION, 0x05, None, Some(1)).await);
		script.extend(publish_namespace_update_token_rid(VERSION, 0x07, Some(hop_path(&[7, 11])), None).await);
		let (mut subscriber, mut requests, _cred, consumer, session) = auth_announce_harness(VERSION, script);
		let rt = subscriber.runtime.clone();
		let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), VERSION)
			.await
			.unwrap();

		// A negotiated peer over a free link, so the advertised cost is the route's warm cost.
		let peer = cluster::Peer {
			hop: Some(crate::Hop::new(9).unwrap()),
			cost: Some(0),
		};
		// The announce arrives routed at cost 4; the buffered updates re-route it.
		let mut initial = token_publish_namespace();
		initial.params.cluster = Some(cluster::Advert {
			hops: hop_path(&[7, 9]),
			cost: 4,
		});

		// Hold the first renewal (0x03) so the next two (0x05, 0x07) are read and buffered while
		// its verdict is pending: the window the single slot used to collapse.
		let release = std::sync::Arc::new(Release::default());
		let answered = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
		let acceptor = {
			let rt = rt.clone();
			let answered = answered.clone();
			let release = release.clone();
			async move {
				let first = requests.next().await.expect("a request");
				let mut held = vec![first.accept(publish_grant_expiring(&rt, None))];
				answered.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
				let a = requests.next().await.expect("a renewal");
				release.notified().await;
				held.push(a.accept(publish_grant_expiring(&rt, None)));
				answered.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
				loop {
					let renewal = requests.next().await.expect("a renewal");
					held.push(renewal.accept(publish_grant_expiring(&rt, None)));
					answered.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
				}
			}
		};
		let mut acceptor = std::pin::pin!(acceptor);
		let mut run = std::pin::pin!(subscriber.run_publish_namespace_stream(stream, initial, peer, None));

		for _ in 0..200 {
			let _ = futures::poll!(acceptor.as_mut());
			assert!(futures::poll!(run.as_mut()).is_pending(), "the announce ended early");
			settle().await;
		}
		release.notify_one();
		let mut both = false;
		for _ in 0..500 {
			let _ = futures::poll!(acceptor.as_mut());
			assert!(futures::poll!(run.as_mut()).is_pending(), "the announce ended early");
			if answered.load(std::sync::atomic::Ordering::Relaxed) >= 4
				&& routed_now(&consumer, "room/alice").is_some_and(|route| route.cost.value() == 1)
			{
				both = true;
				break;
			}
			settle().await;
		}
		assert!(
			both,
			"both buffered updates must each be verified (4 total) and each delta applied: the \
			 middle update's cost (1), not the single-slot survivor's initial 4"
		);
	}

	/// Draft-22 advertises MAX_REQUEST_UPDATES, so a peer that leaves more outstanding than that on
	/// an announce stream broke the negotiated limit: draft-22 section 9.1.7 closes the session
	/// with TOO_MANY_REQUEST_UPDATES. run_publish_namespace_updates must close explicitly (the
	/// dispatcher does not close on an Error::Session), and surface that error too.
	#[moq_clock::test]
	async fn announce_renewals_past_the_advertised_limit_close_the_session() {
		const VERSION: Version = Version::Draft22;

		// Exactly one past the limit: the held renewal plus MAX_REQUEST_UPDATES queued behind it.
		let last = 0x41 + 2 * request_update::MAX_REQUEST_UPDATES;
		let mut script = Vec::new();
		for rid in (0x41..=last).step_by(2) {
			script.extend(publish_namespace_update_token_rid(VERSION, rid, None, None).await);
		}
		let (mut subscriber, mut requests, _cred, _consumer, session) = auth_announce_harness(VERSION, script);
		let rt = subscriber.runtime.clone();
		let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), VERSION)
			.await
			.unwrap();

		// Accept the initial token, then never answer a renewal, so the buffer only grows.
		let acceptor = async move {
			let first = requests.next().await.expect("a request");
			let _held = first.accept(publish_grant_expiring(&rt, None));
			std::future::pending::<()>().await
		};
		let mut acceptor = std::pin::pin!(acceptor);
		let mut run = std::pin::pin!(subscriber.run_publish_namespace_stream(
			stream,
			token_publish_namespace(),
			cluster::Peer::default(),
			None,
		));

		let mut result = None;
		for _ in 0..2000 {
			let _ = futures::poll!(acceptor.as_mut());
			if let std::task::Poll::Ready(r) = futures::poll!(run.as_mut()) {
				result = Some(r);
				break;
			}
			settle().await;
		}
		let Some(Err(err)) = result else {
			panic!("flooding past the advertised limit must end the announce with an error: {result:?}");
		};
		assert_eq!(
			SessionError::from(&err),
			SessionError::TooManyRequestUpdates,
			"the flood must surface TOO_MANY_REQUEST_UPDATES"
		);
		assert!(
			session
				.log
				.closes()
				.iter()
				.any(|close| close.0 == SessionError::TooManyRequestUpdates.to_code()),
			"the flood must close the session with TOO_MANY_REQUEST_UPDATES: {:?}",
			session.log.closes()
		);
	}

	/// Exactly MAX_REQUEST_UPDATES outstanding (the one being verified plus the queue) is within
	/// the advertised limit, so a draft-22 peer holding that many is not faulted: the announce
	/// keeps running and the session stays up.
	#[moq_clock::test]
	async fn announce_renewals_at_the_advertised_limit_keep_the_announce() {
		const VERSION: Version = Version::Draft18;

		let last = 0x41 + 2 * (request_update::MAX_REQUEST_UPDATES - 1);
		let mut script = Vec::new();
		for rid in (0x41..=last).step_by(2) {
			script.extend(publish_namespace_update_token_rid(VERSION, rid, None, None).await);
		}
		let (mut subscriber, mut requests, _cred, _consumer, session) = auth_announce_harness(VERSION, script);
		let rt = subscriber.runtime.clone();
		let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), VERSION)
			.await
			.unwrap();

		let acceptor = async move {
			let first = requests.next().await.expect("a request");
			let _held = first.accept(publish_grant_expiring(&rt, None));
			std::future::pending::<()>().await
		};
		let mut acceptor = std::pin::pin!(acceptor);
		let mut run = std::pin::pin!(subscriber.run_publish_namespace_stream(
			stream,
			token_publish_namespace(),
			cluster::Peer::default(),
			None,
		));

		for _ in 0..2000 {
			let _ = futures::poll!(acceptor.as_mut());
			assert!(
				futures::poll!(run.as_mut()).is_pending(),
				"the announce must stay up at exactly the advertised limit"
			);
			settle().await;
		}
		assert!(
			session.log.closes().is_empty(),
			"no session close at the limit: {:?}",
			session.log.closes()
		);
	}

	/// Drafts below 19 carry no MAX_REQUEST_UPDATES option, so a peer there agreed to no ceiling:
	/// the same flood that closes a draft-22 session must neither end the announce nor close the
	/// session on draft-18, because a conforming peer must not be stranded for a limit it never saw.
	#[moq_clock::test]
	async fn older_drafts_do_not_strand_an_announce_flood() {
		const VERSION: Version = Version::Draft18;

		let mut script = Vec::new();
		for rid in (0x41..=0x81u64).step_by(2) {
			script.extend(publish_namespace_update_token_rid(VERSION, rid, None, None).await);
		}
		let (mut subscriber, mut requests, _cred, _consumer, session) = auth_announce_harness(VERSION, script);
		let rt = subscriber.runtime.clone();
		let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), VERSION)
			.await
			.unwrap();

		let acceptor = async move {
			let first = requests.next().await.expect("a request");
			let _held = first.accept(publish_grant_expiring(&rt, None));
			std::future::pending::<()>().await
		};
		let mut acceptor = std::pin::pin!(acceptor);
		let mut run = std::pin::pin!(subscriber.run_publish_namespace_stream(
			stream,
			token_publish_namespace(),
			cluster::Peer::default(),
			None,
		));

		for _ in 0..2000 {
			let _ = futures::poll!(acceptor.as_mut());
			assert!(
				futures::poll!(run.as_mut()).is_pending(),
				"draft-18 negotiates no limit, so the flood must not strand the announce"
			);
			settle().await;
		}
		assert!(
			session.log.closes().is_empty(),
			"draft-18 flood must not close the session"
		);
	}

	/// On drafts without the option the guard is a memory backstop, not a protocol limit: a flood
	/// past it ends the one announce by finishing the stream, never the session.
	#[moq_clock::test]
	async fn an_older_draft_announce_flood_past_the_guard_ends_the_announce() {
		const VERSION: Version = Version::Draft18;

		let last = 0x41 + 2 * (request_update::UNNEGOTIATED_GUARD as u64 + 1);
		let mut script = Vec::new();
		for rid in (0x41..=last).step_by(2) {
			script.extend(publish_namespace_update_token_rid(VERSION, rid, None, None).await);
		}
		let (mut subscriber, mut requests, _cred, _consumer, session) = auth_announce_harness(VERSION, script);
		let rt = subscriber.runtime.clone();
		let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), VERSION)
			.await
			.unwrap();

		let acceptor = async move {
			let first = requests.next().await.expect("a request");
			let _held = first.accept(publish_grant_expiring(&rt, None));
			std::future::pending::<()>().await
		};
		let mut acceptor = std::pin::pin!(acceptor);
		let mut run = std::pin::pin!(subscriber.run_publish_namespace_stream(
			stream,
			token_publish_namespace(),
			cluster::Peer::default(),
			None,
		));

		let mut result = None;
		for _ in 0..4000 {
			let _ = futures::poll!(acceptor.as_mut());
			if let std::task::Poll::Ready(r) = futures::poll!(run.as_mut()) {
				result = Some(r);
				break;
			}
			settle().await;
		}
		assert!(
			matches!(result, Some(Ok(()))),
			"a flood past the guard ends the announce with Ok: {result:?}"
		);
		assert!(
			session.log.closes().is_empty(),
			"the guard ends the announce, not the session"
		);
	}

	/// A flood past the guard retracts the announce at once, even while the withdrawal waits on
	/// a peer that never acknowledges the finished stream.
	#[moq_clock::test]
	async fn a_flood_past_the_guard_retracts_before_the_peer_reads() {
		const VERSION: Version = Version::Draft18;

		let last = 0x41 + 2 * (request_update::UNNEGOTIATED_GUARD as u64 + 1);
		let mut script = Vec::new();
		for rid in (0x41..=last).step_by(2) {
			script.extend(publish_namespace_update_token_rid(VERSION, rid, None, None).await);
		}
		let session = crate::lite::test_transport::ScriptedSession::per_stream(vec![script]).with_unacked_fin();
		let (mut subscriber, mut requests, _cred, consumer, session) =
			auth_announce_harness_on(VERSION, session.clone());
		let rt = subscriber.runtime.clone();
		let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), VERSION)
			.await
			.unwrap();

		let acceptor = async move {
			let first = requests.next().await.expect("a request");
			let _held = first.accept(publish_grant_expiring(&rt, None));
			std::future::pending::<()>().await
		};
		let mut acceptor = std::pin::pin!(acceptor);
		let mut run = std::pin::pin!(subscriber.run_publish_namespace_stream(
			stream,
			token_publish_namespace(),
			cluster::Peer::default(),
			None,
		));

		// The announce attaches, then the flood overflows the guard, possibly within one poll.
		for _ in 0..200 {
			let _ = futures::poll!(acceptor.as_mut());
			assert!(
				futures::poll!(run.as_mut()).is_pending(),
				"the withdrawal waits on the peer's acknowledgement"
			);
			settle().await;
		}
		assert!(
			routed_now(&consumer, "room/alice").is_none(),
			"a flood past the guard must retract the route without waiting on the peer"
		);
		assert!(
			session.log.closes().is_empty(),
			"the guard ends the announce, not the session"
		);
	}

	/// Like [`auth_announce_harness`] but the session never negotiated the MoQ Auth extension,
	/// so its union is `None` forever, and the application set `limit`, if any.
	fn auth_announce_harness_no_ext(
		version: Version,
		limit: Option<crate::auth::Grant>,
	) -> (
		Subscriber,
		crate::auth::Requests,
		origin::Consumer,
		crate::lite::test_transport::ScriptedSession,
	) {
		let auth = crate::auth::Link::new(false);
		if let Some(limit) = &limit {
			auth.handle().authorize(limit);
		}
		announce_harness_with(version, auth)
	}

	/// An announce harness over `auth`, set up by the caller, taking its requests.
	fn announce_harness_with(
		version: Version,
		auth: crate::auth::Link,
	) -> (
		Subscriber,
		crate::auth::Requests,
		origin::Consumer,
		crate::lite::test_transport::ScriptedSession,
	) {
		let session = crate::lite::test_transport::ScriptedSession::per_stream(vec![Vec::new()]);
		announce_harness_on(version, auth, session)
	}

	/// An announce harness over `auth` and `session`, both set up by the caller.
	fn announce_harness_on(
		version: Version,
		auth: crate::auth::Link,
		session: crate::lite::test_transport::ScriptedSession,
	) -> (
		Subscriber,
		crate::auth::Requests,
		origin::Consumer,
		crate::lite::test_transport::ScriptedSession,
	) {
		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let consumer = origin.consume();
		let (tasks, task_set) = crate::util::TaskSet::new();
		std::mem::forget(task_set);
		let peer_setup = peer::PeerSetup::default();
		peer_setup.set(peer::Peer::default());

		let requests = auth.handle().requests();

		let subscriber = Subscriber::new(
			crate::time::sim(),
			crate::driver::Transport::new(session.clone()),
			origin,
			Control::new(false),
			None,
			peer_setup,
			crate::Hop::new(1).unwrap(),
			None,
			version,
			tasks,
			Default::default(),
		)
		.with_auth(auth);

		(subscriber, requests, consumer, session)
	}

	/// On a session without the AUTH extension, a token-bearing PUBLISH_NAMESPACE the limit
	/// does not cover is verified by the application. Admitted on a covering grant; refused
	/// UNAUTHORIZED (not admitted) on refusal. This is the standard moq-transport peer shape.
	#[moq_clock::test]
	async fn a_request_token_on_a_no_auth_session_is_verified() {
		const VERSION: Version = Version::Draft18;

		// Accepted: the acceptor is consulted (proving the token path, not the permissive
		// default, which the origin model would also route) and its grant admits the announce.
		{
			let (mut subscriber, mut requests, consumer, session) =
				auth_announce_harness_no_ext(VERSION, Some(crate::auth::Grant::default()));
			let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), VERSION)
				.await
				.unwrap();
			let consulted = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
			let acceptor = {
				let consulted = consulted.clone();
				async move {
					let mut held = Vec::new();
					loop {
						let request = requests.next().await.expect("a request reaches the acceptor");
						consulted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
						held.push(request.accept(crate::auth::Grant::all()));
					}
				}
			};
			let mut acceptor = std::pin::pin!(acceptor);
			let mut run = std::pin::pin!(subscriber.run_publish_namespace_stream(
				stream,
				token_publish_namespace(),
				cluster::Peer::default(),
				None,
			));
			let mut routed = false;
			for _ in 0..500 {
				let _ = futures::poll!(acceptor.as_mut());
				assert!(
					futures::poll!(run.as_mut()).is_pending(),
					"the announce ended during setup"
				);
				if routed_now(&consumer, "room/alice").is_some() {
					routed = true;
					break;
				}
				settle().await;
			}
			assert!(
				routed,
				"a token on a no-auth session must be verified and admitted, not ignored"
			);
			assert!(
				consulted.load(std::sync::atomic::Ordering::Relaxed) >= 1,
				"the token must reach the acceptor, not be admitted by the permissive default"
			);
		}

		// Refused: not admitted by the permissive default.
		{
			let (mut subscriber, mut requests, consumer, session) =
				auth_announce_harness_no_ext(VERSION, Some(crate::auth::Grant::default()));
			let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), VERSION)
				.await
				.unwrap();
			let acceptor = async move {
				let request = requests.next().await.expect("a request reaches the acceptor");
				request.reject(crate::SessionError::Unauthorized, "no");
				std::future::pending::<()>().await
			};
			let mut acceptor = std::pin::pin!(acceptor);
			let mut run = std::pin::pin!(subscriber.run_publish_namespace_stream(
				stream,
				token_publish_namespace(),
				cluster::Peer::default(),
				None,
			));
			let mut ended = false;
			for _ in 0..500 {
				let _ = futures::poll!(acceptor.as_mut());
				if futures::poll!(run.as_mut()).is_ready() {
					ended = true;
					break;
				}
				settle().await;
			}
			assert!(ended, "a refused token ends the announce");
			assert!(
				routed_now(&consumer, "room/alice").is_none(),
				"a refused token must not be admitted by the permissive default"
			);
		}
	}

	/// A peer that ends the announce while a renewal is still being verified ends it here too:
	/// with a grant that never expires and an acceptor that never answers the renewal, only
	/// the stream closing can end the request, and it must.
	#[moq_clock::test]
	async fn a_closed_announce_ends_while_a_renewal_is_pending() {
		const VERSION: Version = Version::Draft18;
		let session = crate::lite::test_transport::ScriptedSession::per_stream_eof(vec![
			publish_namespace_update_with_token(VERSION).await,
		]);
		let (mut subscriber, mut requests, _cred, consumer, session) =
			auth_announce_harness_on(VERSION, session.clone());
		let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), VERSION)
			.await
			.unwrap();
		let popped = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
		let acceptor = {
			let popped = popped.clone();
			async move {
				let first = requests.next().await.expect("a request");
				let _issued = first.accept(crate::auth::Grant::all());
				popped.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
				let _never_answered = requests.next().await.expect("a renewal");
				popped.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
				std::future::pending::<()>().await
			}
		};
		let mut acceptor = std::pin::pin!(acceptor);
		let mut run = std::pin::pin!(subscriber.run_publish_namespace_stream(
			stream,
			token_publish_namespace(),
			cluster::Peer::default(),
			None,
		));
		let mut ended = false;
		for _ in 0..500 {
			let _ = futures::poll!(acceptor.as_mut());
			if futures::poll!(run.as_mut()).is_ready() {
				ended = true;
				break;
			}
			settle().await;
		}
		assert!(
			popped.load(std::sync::atomic::Ordering::Relaxed) >= 1,
			"the announce was token-authorized"
		);
		assert!(
			ended,
			"closing the stream ends the announce despite the pending renewal"
		);
		assert!(
			routed_now(&consumer, "room/alice").is_none(),
			"the announce is withdrawn"
		);
	}

	/// An update queued behind a pending renewal spends its request ID on arrival, so the
	/// announce ending before the queue drains leaves no ID the peer could reuse.
	#[moq_clock::test]
	async fn a_queued_update_spends_its_request_id_when_the_announce_ends() {
		const VERSION: Version = Version::Draft18;
		let queued = {
			let log = crate::lite::test_transport::Log::default();
			let mut writer = crate::coding::Writer::new(
				crate::transport::boxed::SendStream::new(crate::lite::test_transport::SinkSend::new(log.clone())),
				VERSION,
			);
			writer.varint(ietf::PublishNamespaceUpdate::ID).await.unwrap();
			writer
				.encode(&ietf::PublishNamespaceUpdate {
					request_id: RequestId(5),
					hops: None,
					cost: None,
					authorization_token: None,
				})
				.await
				.unwrap();
			log.writes.lock().unwrap().clone()
		};
		let session = crate::lite::test_transport::ScriptedSession::per_stream_eof(vec![
			[publish_namespace_update_with_token(VERSION).await, queued].concat(),
		]);
		let (mut subscriber, mut requests, _cred, _consumer, session) =
			auth_announce_harness_on(VERSION, session.clone());
		let control = subscriber.control.clone();
		let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), VERSION)
			.await
			.unwrap();
		let acceptor = async move {
			let first = requests.next().await.expect("a request");
			let _issued = first.accept(crate::auth::Grant::all());
			let _never_answered = requests.next().await.expect("a renewal");
			std::future::pending::<()>().await
		};
		let mut acceptor = std::pin::pin!(acceptor);
		let mut run = std::pin::pin!(subscriber.run_publish_namespace_stream(
			stream,
			token_publish_namespace(),
			cluster::Peer::default(),
			None,
		));
		let mut ended = false;
		for _ in 0..500 {
			let _ = futures::poll!(acceptor.as_mut());
			if futures::poll!(run.as_mut()).is_ready() {
				ended = true;
				break;
			}
			settle().await;
		}
		assert!(ended, "closing the stream ends the announce");
		assert_eq!(
			control.goaway().unwrap(),
			RequestId(7),
			"the queued update's request ID was left unspent"
		);
	}

	/// An update queued behind a pending renewal is decoded on arrival, so a session-fatal one
	/// still closes the session when the peer withdraws before the queue drains.
	#[moq_clock::test]
	async fn a_queued_malformed_update_is_fatal_despite_a_withdrawal() {
		const VERSION: Version = Version::Draft18;
		let queued = {
			let log = crate::lite::test_transport::Log::default();
			let mut writer = crate::coding::Writer::new(
				crate::transport::boxed::SendStream::new(crate::lite::test_transport::SinkSend::new(log.clone())),
				VERSION,
			);
			writer.varint(ietf::PublishNamespaceUpdate::ID).await.unwrap();
			writer
				.encode(&ietf::PublishNamespaceUpdate {
					request_id: RequestId(5),
					hops: None,
					cost: None,
					authorization_token: Some(crate::ietf::token::RequestToken(crate::setup::Token {
						kind: 0,
						value: Vec::new(),
					})),
				})
				.await
				.unwrap();
			let bytes = log.writes.lock().unwrap().clone();
			// Rewrite the by-value token (length 2: USE_VALUE, type 0) as a reference to alias 7,
			// which was never registered.
			let at: Vec<_> = bytes
				.windows(3)
				.enumerate()
				.filter(|(_, w)| *w == [0x02, 0x03, 0x00])
				.collect();
			assert_eq!(at.len(), 1, "the token parameter is found once");
			let at = at[0].0;
			[&bytes[..at], &[0x02, 0x02, 0x07], &bytes[at + 3..]].concat()
		};
		let session = crate::lite::test_transport::ScriptedSession::per_stream_eof(vec![
			[publish_namespace_update_with_token(VERSION).await, queued].concat(),
		]);
		let (mut subscriber, mut requests, _cred, _consumer, session) =
			auth_announce_harness_on(VERSION, session.clone());
		let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), VERSION)
			.await
			.unwrap();
		let acceptor = async move {
			let first = requests.next().await.expect("a request");
			let _issued = first.accept(crate::auth::Grant::all());
			let _never_answered = requests.next().await.expect("a renewal");
			std::future::pending::<()>().await
		};
		let mut acceptor = std::pin::pin!(acceptor);
		let mut run = std::pin::pin!(subscriber.run_publish_namespace_stream(
			stream,
			token_publish_namespace(),
			cluster::Peer::default(),
			None,
		));
		let mut ended = None;
		for _ in 0..500 {
			let _ = futures::poll!(acceptor.as_mut());
			if let Poll::Ready(result) = futures::poll!(run.as_mut()) {
				ended = Some(result);
				break;
			}
			settle().await;
		}
		let Some(result) = ended else {
			panic!("closing the stream ends the announce");
		};
		assert!(
			matches!(
				result,
				Err(Error::Decode(DecodeError::Session(
					crate::SessionError::UnknownAuthTokenAlias
				)))
			),
			"the queued update's unknown alias closes the session: {result:?}"
		);
	}

	/// An announcing peer's token grant is checked on its `publish` patterns: a read-only grant
	/// does not admit a PUBLISH_NAMESPACE, a write-only one does.
	#[moq_clock::test]
	async fn an_announce_token_needs_a_publish_grant() {
		const VERSION: Version = Version::Draft18;
		let read_only = crate::auth::Grant {
			publish: crate::Patterns::new(),
			subscribe: crate::Pattern::all().into(),
			expires: None,
		};
		let write_only = crate::auth::Grant {
			publish: crate::Pattern::all().into(),
			subscribe: crate::Patterns::new(),
			expires: None,
		};
		for (grant, admitted) in [(read_only, false), (write_only, true)] {
			let (mut subscriber, mut requests, consumer, session) =
				auth_announce_harness_no_ext(VERSION, Some(crate::auth::Grant::default()));
			let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), VERSION)
				.await
				.unwrap();
			let acceptor = async move {
				let request = requests.next().await.expect("a request reaches the acceptor");
				let _issued = request.accept(grant);
				std::future::pending::<()>().await
			};
			let mut acceptor = std::pin::pin!(acceptor);
			let mut run = std::pin::pin!(subscriber.run_publish_namespace_stream(
				stream,
				token_publish_namespace(),
				cluster::Peer::default(),
				None,
			));
			let mut ended = false;
			for _ in 0..300 {
				let _ = futures::poll!(acceptor.as_mut());
				if futures::poll!(run.as_mut()).is_ready() {
					ended = true;
					break;
				}
				if admitted && routed_now(&consumer, "room/alice").is_some() {
					break;
				}
				settle().await;
			}
			assert_eq!(
				routed_now(&consumer, "room/alice").is_some(),
				admitted,
				"admitted={admitted}"
			);
			assert_eq!(ended, !admitted, "a grant that does not cover the announce refuses it");
		}
	}

	/// The additive constraint: a token-LESS PUBLISH_NAMESPACE on a no-auth session (None union)
	/// is admitted by the origin model exactly as before; the token path is never entered.
	#[moq_clock::test]
	async fn a_token_less_request_on_a_no_auth_session_is_unchanged() {
		const VERSION: Version = Version::Draft18;
		let (mut subscriber, _requests, consumer, session) = auth_announce_harness_no_ext(VERSION, None);
		let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), VERSION)
			.await
			.unwrap();

		let mut msg = token_publish_namespace();
		msg.authorization_token = None;

		let mut run =
			std::pin::pin!(subscriber.run_publish_namespace_stream(stream, msg, cluster::Peer::default(), None,));
		let mut routed = false;
		for _ in 0..500 {
			assert!(futures::poll!(run.as_mut()).is_pending(), "the announce ended early");
			if routed_now(&consumer, "room/alice").is_some() {
				routed = true;
				break;
			}
			settle().await;
		}
		assert!(
			routed,
			"a token-less announce is admitted by the origin model as before"
		);
	}

	/// Holds an acceptor until the test releases it.
	#[derive(Clone, Default)]
	struct Release(kio::Shared<bool>);

	impl Release {
		fn notify_one(&self) {
			*self.0.lock() = true;
		}

		async fn notified(&self) {
			kio::wait(|waiter| {
				self.0
					.poll(waiter, |released| match **released {
						true => Poll::Ready(()),
						false => Poll::Pending,
					})
					.map(|_| ())
			})
			.await
		}
	}
}

/// What an update to an Established subscription changes.
struct Update {
	/// The aggregate group span.
	filter: Option<Filter>,
	/// History newly covered by a draft-22 widening.
	fill: Option<ietf::Fill>,
	/// The aggregate subscriber priority, in the model's higher-first terms.
	priority: Option<i8>,
	/// Subscriber age budget, in milliseconds.
	max_age: Option<u64>,
	/// NEW_GROUP_REQUEST.
	new_group: Option<u64>,
}

/// Anchor resolutions live only as long as the ranges in the current aggregate. Cache
/// eviction must not resolve an unchanged time end again at a different group.
#[derive(Default)]
struct ResolvedRanges(BTreeMap<track::Range, track::Range>);

impl ResolvedRanges {
	fn resolve(
		&mut self,
		track: &track::Producer,
		subscription: &track::Subscription,
	) -> Result<track::Subscription, Error> {
		let mut next = BTreeMap::new();
		let mut pending = Vec::new();
		for range in &subscription.ranges {
			match self.0.get(range) {
				Some(resolved) => {
					next.insert(*range, *resolved);
				}
				None => pending.push(*range),
			}
		}
		let resolved = track.resolve_subscription(&track::Subscription {
			ranges: pending.clone(),
			..subscription.clone()
		})?;
		next.extend(pending.into_iter().zip(resolved.ranges));
		let mut subscription = subscription.clone();
		for range in &mut subscription.ranges {
			*range = next[range];
		}
		self.0 = next;
		Ok(subscription)
	}
}

/// The wire bounds `subscription` resolves to, `None` while a relative anchor still waits
/// for the live edge it counts back from.
fn settle_bounds(
	resolved: &mut ResolvedRanges,
	track: &track::Producer,
	subscription: &track::Subscription,
) -> Result<Option<WireBounds>, Error> {
	match resolved.resolve(track, subscription) {
		Ok(subscription) => Ok(Some(wire_bounds(&subscription))),
		Err(Error::Unsupported) if !has_time(subscription) => Ok(None),
		Err(err) => Err(err),
	}
}

fn has_time(subscription: &track::Subscription) -> bool {
	subscription
		.ranges
		.iter()
		.any(|range| matches!(range.start, track::Anchor::Time(_)) || matches!(range.end, Some(track::Anchor::Time(_))))
}

fn needs_resolution(subscription: &track::Subscription) -> bool {
	subscription.ranges.iter().any(|range| {
		let start = match range.start {
			track::Anchor::Absolute(_) => false,
			track::Anchor::Relative(back) => back != 0 && back != u64::MAX,
			track::Anchor::Time(_) => true,
		};
		start || matches!(range.end, Some(track::Anchor::Time(_) | track::Anchor::Relative(_)))
	})
}

/// A subscription's single wire span: its start and exclusive end group, each open when `None`.
type WireBounds = (Option<u64>, Option<u64>);

/// The single wire span covers mixed absolute/live starts from group zero.
fn wire_bounds(subscription: &track::Subscription) -> WireBounds {
	let start = subscription.start().or_else(|| {
		subscription
			.ranges
			.iter()
			.any(|range| matches!(range.start, track::Anchor::Absolute(_)))
			.then_some(0)
	});
	(start, subscription.end())
}

/// What a SUBSCRIBE asks for: the range it delivers, and the backfill covering a head that
/// range excludes.
#[derive(Debug, Default, PartialEq, Eq)]
struct Join {
	/// The Location Filter, bounding what the subscription itself delivers.
	filter: Filter,

	/// The FILL_PARAMETERS backfill, delivered on its own fetch stream.
	fill: Option<ietf::Fill>,

	/// A draft-18 joining FETCH, sent as its own request after SUBSCRIBE.
	fetch: bool,
}

/// What a moq-lite subscription's group range asks for on the wire.
///
/// moq-lite joins a track at the *start* of the current group, which is a decodable point.
/// Draft-22 spells that as the draft's own current-group join (section 3.5): a Next
/// Object subscription plus a `StartGroup=1` fill, which is the only form a publisher has
/// to honor. It splits the group across two streams, the fill carrying the head and the
/// subscription the tail, which `claim_fill` stitches back into one group producer.
///
/// Earlier drafts have no fill parameter. Every subscription is a live join: Largest
/// Object, followed by a joining FETCH relative at offset 0 for that group's head. An
/// earlier start's groups are left to the copy's fills, one FETCH per run of them, and a
/// later start skips the head locally. A bounded end has no
/// joining-FETCH spelling, so it is refused rather than left open.
///
/// A start group we already know is absolute on draft-22 and needs no fill: the
/// subscription's own range covers it, which is what our publisher serves from its cache.
fn subscribe_join(start: Option<u64>, end: Option<u64>, version: Version) -> Result<Join, Error> {
	if !Filter::is_draft20(version) {
		if end.is_some() {
			return Err(Error::Unsupported);
		}
		return Ok(Join {
			filter: Filter::NextObject,
			fill: None,
			fetch: true,
		});
	}

	Ok(match start {
		// The live join: everything after the live edge, plus the current group's head.
		None => Join {
			filter: Filter::NextObject,
			fill: Some(ietf::Fill {
				group_order: None,
				// One group back from the next group is the current one.
				filter: Some(Filter::Relative(1)),
				range_filters: false,
			}),
			fetch: false,
		},
		// An absolute {0, 0} with no end is defined as unfiltered, so it spells itself.
		Some(0) if end.is_none() => Join {
			filter: Filter::Unfiltered,
			fill: None,
			fetch: false,
		},
		Some(start) => Join {
			filter: Filter::Absolute {
				start: ietf::Location {
					group: start,
					object: 0,
				},
				end: end.and_then(|end| {
					Some(ietf::EndLocation {
						group: end.checked_sub(1)?,
						object: None,
					})
				}),
			},
			fill: None,
			fetch: false,
		},
	})
}

/// Resolve the first absolute ID or a later ID delta without overflowing. Subgroup relay
/// ingest preserves gaps.
fn next_object_id(prior: Option<u64>, delta: u64) -> Result<u64, Error> {
	match prior {
		None => Ok(delta),
		Some(prior) => prior
			.checked_add(delta)
			.and_then(|id| id.checked_add(1))
			.ok_or(Error::Session(SessionError::ProtocolViolation)),
	}
}

#[cfg(test)]
mod object_id_tests {
	use super::*;

	/// The first delta is the absolute Object ID and each later one counts past the prior.
	#[test]
	fn resolves_ids_and_gaps() {
		assert_eq!(next_object_id(None, 6).unwrap(), 6);
		assert_eq!(next_object_id(Some(6), 0).unwrap(), 7);
		assert_eq!(next_object_id(Some(6), 2).unwrap(), 9);
	}

	/// The running ID is bounded, and the draft makes an overflow a protocol violation
	/// rather than something to wrap.
	#[test]
	fn rejects_an_overflow() {
		assert!(matches!(
			next_object_id(Some(u64::MAX), 0),
			Err(Error::Session(SessionError::ProtocolViolation))
		));
	}
}

#[cfg(test)]
mod filter_tests {
	use super::*;

	/// The live join is the draft's own: the subscription starts after the live edge and a
	/// `StartGroup=1` fill covers the current group's head, so every object arrives exactly
	/// once and the group still starts at a decodable point.
	#[test]
	fn live_joins_the_current_group_with_a_fill() {
		assert_eq!(
			subscribe_join(None, None, Version::Draft22).unwrap(),
			Join {
				filter: Filter::NextObject,
				fill: Some(ietf::Fill {
					group_order: None,
					filter: Some(Filter::Relative(1)),
					range_filters: false,
				}),
				fetch: false,
			}
		);
	}

	/// A start we can name absolutely is inside the subscription's own range, so there is no
	/// head outside it to fill.
	#[test]
	fn a_past_start_is_absolute() {
		assert_eq!(
			subscribe_join(Some(7), Some(10), Version::Draft22).unwrap(),
			Join {
				filter: Filter::Absolute {
					start: ietf::Location { group: 7, object: 0 },
					end: Some(ietf::EndLocation { group: 9, object: None }),
				},
				fill: None,
				fetch: false,
			}
		);
	}

	/// The whole track has a spelling of its own: an absent filter is unrestricted.
	#[test]
	fn the_whole_track_is_unfiltered() {
		assert_eq!(
			subscribe_join(Some(0), None, Version::Draft22).unwrap(),
			Join {
				filter: Filter::Unfiltered,
				fill: None,
				fetch: false,
			}
		);
	}

	const JOINING_DRAFTS: [Version; 1] = [Version::Draft18];

	/// Every draft-18 join is Largest Object plus a joining FETCH, whatever its start,
	/// so the current group's head arrives on the fetch stream and the live tail on the
	/// subscription.
	#[test]
	fn older_drafts_always_join_live() {
		for version in JOINING_DRAFTS {
			for start in [None, Some(7)] {
				assert_eq!(
					subscribe_join(start, None, version).unwrap(),
					Join {
						filter: Filter::NextObject,
						fill: None,
						fetch: true,
					},
					"{version} from {start:?}"
				);
			}
		}
	}

	/// A bounded end has no joining-FETCH spelling, so it is refused rather than left open.
	#[test]
	fn older_drafts_refuse_a_bounded_end() {
		for version in JOINING_DRAFTS {
			assert!(
				matches!(subscribe_join(Some(7), Some(10), version), Err(Error::Unsupported)),
				"{version}"
			);
		}
	}
}

/// Draft-22's current-group join, where one group arrives on two streams: the fill fetch
/// stream carries the head and the subscription's own subgroup stream the tail.
#[cfg(test)]
mod stitch_tests {

	use super::*;
	use crate::{
		Timestamp,
		coding::{Encode as _, Encoder},
		lite::test_transport::ScriptedSession,
		model::ProduceTest,
		util::{TaskSet, Tasks},
	};

	const VERSION: Version = Version::Draft22;
	const ALIAS: u64 = 7;
	const REQUEST: RequestId = RequestId(1);
	const SEQUENCE: u64 = 4;

	/// A distinct timestamp per object, so a stitched group's frames can be told apart.
	fn timestamp(index: usize) -> Timestamp {
		Timestamp::from_micros(1000 + index as u64).expect("in range")
	}

	/// A publisher's fill fetch stream: a FETCH_HEADER, then one object per payload
	/// numbered from the group's first.
	fn fill_stream(sequence: u64, payloads: &[&[u8]]) -> Vec<u8> {
		fill_stream_for(REQUEST, &[(sequence, payloads)])
	}

	/// A joining FETCH stream named by its own request id, possibly spanning groups. The
	/// track is timed, so every object carries a Timestamp.
	fn fill_stream_for<B: AsRef<[u8]>>(request_id: RequestId, groups: &[(u64, &[B])]) -> Vec<u8> {
		let mut buf = Vec::new();
		crate::coding::Encoder::new(&mut buf, VERSION.into())
			.varint(ietf::FetchHeader::TYPE)
			.unwrap();
		ietf::FetchHeader { request_id }
			.encode(&mut crate::coding::Encoder::new(&mut buf, VERSION.into()), VERSION)
			.unwrap();

		let mut object_index = 0usize;
		let mut prev_group = None;
		for &(sequence, payloads) in groups {
			for (index, payload) in payloads.iter().enumerate() {
				let payload = payload.as_ref();
				let mut properties = Vec::new();
				let w = &mut Encoder::new(&mut properties, VERSION.into());
				ietf::encode_object_time(w, timestamp(object_index), Timescale::MICRO).unwrap();
				let properties = Some(properties);

				// The first object of the stream carries the absolute Group ID. From
				// draft-18 on, the first object of a later group carries the ascending
				// Group ID Delta (new = prior + delta + 1), so 7 then 8 is delta 0.
				let first = index == 0;
				let group = match (first, prev_group) {
					(false, _) => None,
					(true, None) => Some(sequence),
					(true, Some(prev)) => Some(sequence.checked_sub(prev + 1).expect("ascending groups")),
				};
				ietf::FetchObject::Object {
					subgroup: ietf::FetchSubgroup::Zero,
					group,
					object: first.then_some(0),
					priority: first.then_some(0),
					properties,
				}
				.encode(&mut crate::coding::Encoder::new(&mut buf, VERSION.into()), VERSION)
				.unwrap();

				crate::coding::Encoder::new(&mut buf, VERSION.into())
					.varint(payload.len() as u64)
					.unwrap();
				buf.extend_from_slice(payload);
				object_index += 1;
			}
			prev_group = Some(sequence);
		}

		buf.to_vec()
	}

	/// The subscription's own subgroup stream, starting at `start` because a strict
	/// publisher delivers nothing before it: that head is the fill's job. Each object
	/// carries the Timestamp of its Object ID.
	fn tail_stream(sequence: u64, start: u64, payloads: &[&[u8]]) -> Vec<u8> {
		let mut buf = Vec::new();
		ietf::GroupHeader {
			track_alias: ALIAS,
			group_id: sequence,
			sub_group_id: 0,
			publisher_priority: 0,
			flags: ietf::GroupFlags {
				first_object: start == 0,
				has_extensions: true,
				..Default::default()
			},
		}
		.encode(&mut crate::coding::Encoder::new(&mut buf, VERSION.into()), VERSION)
		.unwrap();

		for (index, payload) in payloads.iter().enumerate() {
			// The first object's delta is its absolute Object ID; every later one counts
			// the objects skipped, so zero is the next one.
			let delta = match index {
				0 => start,
				_ => 0,
			};
			crate::coding::Encoder::new(&mut buf, VERSION.into())
				.varint(delta)
				.unwrap();
			let mut ext = Vec::new();
			let object = usize::try_from(start).unwrap() + index;
			ietf::encode_object_time(
				&mut Encoder::new(&mut ext, VERSION.into()),
				timestamp(object),
				Timescale::MICRO,
			)
			.unwrap();
			let w = &mut crate::coding::Encoder::new(&mut buf, VERSION.into());
			w.varint(ext.len() as u64).unwrap();
			buf.extend_from_slice(&ext);
			crate::coding::Encoder::new(&mut buf, VERSION.into())
				.varint(payload.len() as u64)
				.unwrap();
			buf.extend_from_slice(payload);
		}

		buf.to_vec()
	}

	/// A draft-18 subgroup stream whose FIRST_OBJECT bit is independent of the first
	/// object's absolute ID. A strict publisher sets the bit exactly when `start` is 0;
	/// one that is out of spec can leave it clear and still start there. Each object
	/// carries the Timestamp of its Object ID, which the harness's timed track requires.
	fn draft18_subgroup(sequence: u64, first_object: bool, start: u64, payloads: &[&[u8]]) -> Vec<u8> {
		let version = Version::Draft18;
		let mut buf = Vec::new();
		ietf::GroupHeader {
			track_alias: ALIAS,
			group_id: sequence,
			sub_group_id: 0,
			publisher_priority: 0,
			flags: ietf::GroupFlags {
				first_object,
				has_extensions: true,
				..Default::default()
			},
		}
		.encode(&mut crate::coding::Encoder::new(&mut buf, version.into()), version)
		.unwrap();

		for (index, payload) in payloads.iter().enumerate() {
			// The first object's delta is its absolute Object ID; every later one counts
			// the objects skipped, so zero is the next one.
			let delta = match index {
				0 => start,
				_ => 0,
			};
			crate::coding::Encoder::new(&mut buf, version.into())
				.varint(delta)
				.unwrap();
			let mut ext = Vec::new();
			let object = usize::try_from(start).unwrap() + index;
			ietf::encode_object_time(
				&mut Encoder::new(&mut ext, version.into()),
				timestamp(object),
				Timescale::MICRO,
			)
			.unwrap();
			crate::coding::Encoder::new(&mut buf, version.into())
				.varint(ext.len() as u64)
				.unwrap();
			buf.extend_from_slice(&ext);
			crate::coding::Encoder::new(&mut buf, version.into())
				.varint(payload.len() as u64)
				.unwrap();
			buf.extend_from_slice(payload);
		}

		buf
	}

	/// A reader over the next scripted stream, decoded as draft-18.
	async fn read_draft18(session: &ScriptedSession) -> Reader<crate::transport::boxed::RecvStream, Version> {
		let session = session.clone();
		let (_, recv) = session.open_bi(Default::default()).await.unwrap();
		Reader::new(crate::transport::boxed::RecvStream::new(recv), Version::Draft18)
	}

	/// A subscriber holding one draft-22 subscription, as its SUBSCRIBE_OK left it: the
	/// alias bound, the timescale declared, and `fill` waiting on its fetch stream.
	struct Harness {
		subscriber: Subscriber,
		session: ScriptedSession,
		track: track::Producer,
		fill: kio::Producer<Fill>,
		_tasks: (Tasks, TaskSet),
	}

	impl Harness {
		fn new(fill: Fill, scripts: Vec<Vec<u8>>) -> Self {
			Self::with_timescale(fill, scripts, Some(Timescale::MICRO))
		}

		/// A subscription whose SUBSCRIBE_OK declared `timescale`, or none for an untimed
		/// track.
		fn with_timescale(fill: Fill, scripts: Vec<Vec<u8>>, timescale: Option<Timescale>) -> Self {
			let session = ScriptedSession::per_stream_eof(scripts);
			let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
			let tasks = TaskSet::new();

			let subscriber = Subscriber::new(
				crate::time::sim(),
				crate::driver::Transport::new(session.clone()),
				origin,
				Control::new(false),
				None,
				peer::PeerSetup::default(),
				crate::Hop::new(1).unwrap(),
				None,
				VERSION,
				tasks.0.clone(),
				Default::default(),
			);

			// The subscriber accepts a timed track at microseconds, matching `run_subscribe`.
			let info = accepted_info(
				&ietf::Properties {
					timescale: timescale.map(|_| Timescale::MICRO),
					..Default::default()
				},
				VERSION,
			);
			let track = track::Producer::new(std::sync::Arc::new(crate::broadcast::Info::default()), "video", info);
			let fill = kio::Producer::new(fill);

			{
				let mut state = subscriber.state.lock();
				state.subscribes.insert(
					REQUEST,
					TrackState {
						alias: Some(ALIAS),
						timescale,
						..TrackState::new(track.clone(), Path::from_static("broadcast"), fill.clone())
					},
				);
				insert_track_alias(&state.aliases, ALIAS, REQUEST).unwrap();
			}

			Self {
				subscriber,
				session,
				track,
				fill,
				_tasks: tasks,
			}
		}

		/// Bind a draft-18 joining FETCH onto this subscription, so `recv_fill` looks
		/// the stream up by the FETCH's own request id.
		fn with_joining(self, fetch_id: RequestId) -> Self {
			{
				let mut state = self.subscriber.state.lock();
				state.fetches.insert(fetch_id, FillRequest::Joining(REQUEST));
				if let Some(track) = state.subscribes.get_mut(&REQUEST) {
					track.fetch_id = Some(fetch_id);
				}
			}
			self
		}

		/// Name `sequence` as the Largest Group the fill answers, so it owes that group's head.
		fn with_largest(self, sequence: u64) -> Self {
			self.subscriber
				.state
				.lock()
				.subscribes
				.get_mut(&REQUEST)
				.unwrap()
				.fill_largest = Some(sequence);
			self
		}

		fn with_update(self, request_id: RequestId) -> Self {
			let mut state = self.subscriber.state.lock();
			state.fetches.insert(request_id, FillRequest::Update(REQUEST));
			state.subscribes.get_mut(&REQUEST).unwrap().fetch_id = Some(request_id);
			drop(state);
			self
		}

		/// Run the next scripted stream through `recv_group` and the dispatcher's handling of
		/// its result, asserting that stream alone was refused with STOP_SENDING.
		async fn dispatch_refused(&self) {
			let mut stream = self.stream().await;
			let res = self.subscriber.clone().recv_group(&mut stream).await;
			let detail = format!("{res:?}");
			super::super::session::stop_on_error(&mut stream, res).expect("the session survives");
			assert_eq!(
				self.session.log.stops().len(),
				1,
				"the stream was not refused: {detail}"
			);
			assert!(self.session.log.closes().is_empty());
		}

		/// A reader over the next scripted stream, standing in for one the peer opened.
		async fn stream(&self) -> Reader<crate::transport::boxed::RecvStream, Version> {
			let session = self.session.clone();
			let (_, recv) = session.open_bi(Default::default()).await.unwrap();
			Reader::new(crate::transport::boxed::RecvStream::new(recv), VERSION)
		}
	}

	/// Every frame of the next group, once it finishes.
	async fn read_group(subscriber: &mut track::Subscriber) -> (u64, Vec<(Option<Timestamp>, Vec<u8>)>) {
		let mut group = subscriber
			.recv_group()
			.await
			.expect("track aborted")
			.expect("track finished");

		let sequence = group.sequence;
		let mut frames = Vec::new();
		while let Some(frame) = group.read_frame().await.expect("group aborted") {
			frames.push((frame.timestamp, frame.payload.to_vec()));
		}

		(sequence, frames)
	}

	/// The canonical join: the fill carries the objects published before we subscribed and
	/// the subscription the ones after, and they land in one group in order.
	///
	/// The tail is read first, so it has to wait for the head rather than start a group of
	/// its own: with newest-first group order the publisher can prioritize the tail's stream
	/// ahead of the fill's.
	#[moq_clock::test]
	async fn a_fill_and_its_tail_stitch_into_one_group() {
		let h = Harness::new(
			Fill::Serving(Some(Timescale::MICRO)),
			vec![
				fill_stream(SEQUENCE, &[b"head-0", b"head-1"]),
				tail_stream(SEQUENCE, 2, &[b"tail-2"]),
			],
		);
		let mut consumer = h.track.subscribe(None);

		let mut fill = h.stream().await;
		let mut tail = h.stream().await;

		let mut serve_tail = h.subscriber.clone();
		let mut serve_fill = h.subscriber.clone();
		let (tail, head) = futures::join!(serve_tail.recv_group(&mut tail), serve_fill.recv_fill(&mut fill));
		head.expect("fill");
		tail.expect("tail");

		let (sequence, frames) = read_group(&mut consumer).await;
		assert_eq!(sequence, SEQUENCE);
		assert_eq!(
			frames,
			vec![
				(Some(timestamp(0)), b"head-0".to_vec()),
				(Some(timestamp(1)), b"head-1".to_vec()),
				(Some(timestamp(2)), b"tail-2".to_vec()),
			]
		);
		assert!(matches!(*h.fill.read(), Fill::Done), "the head was claimed");
	}

	/// History groups finish at the next group's header; only the newest head waits
	/// for the live tail. A gap in group numbering is not a missing object.
	#[moq_clock::test]
	async fn a_multi_group_fill_finishes_history_before_stitching_the_tail() {
		let history: &[&[u8]] = &[b"history"];
		let head: &[&[u8]] = &[b"head"];
		let h = Harness::new(
			Fill::Serving(Some(Timescale::MICRO)),
			vec![
				fill_stream_for(REQUEST, &[(SEQUENCE - 2, history), (SEQUENCE, head)]),
				tail_stream(SEQUENCE, 1, &[b"tail"]),
			],
		);
		let mut consumer = h
			.track
			.subscribe(Some(
				track::Subscription::default()
					.with_order(crate::track::Order::Ascending)
					.with_max_delay(None),
			))
			.with_range(..);
		let mut fill = h.stream().await;
		let mut tail = h.stream().await;
		let mut receiving = h.subscriber.clone();
		let mut tailing = h.subscriber.clone();
		let (head, tail) = futures::join!(receiving.recv_fill(&mut fill), tailing.recv_group(&mut tail));
		head.expect("fill");
		tail.expect("tail");
		let (sequence, frames) = read_group(&mut consumer).await;
		assert_eq!(sequence, SEQUENCE - 2);
		assert_eq!(frames, vec![(Some(timestamp(0)), b"history".to_vec())]);
		let (sequence, frames) = read_group(&mut consumer).await;
		assert_eq!(sequence, SEQUENCE);
		assert_eq!(
			frames,
			vec![
				(Some(timestamp(1)), b"head".to_vec()),
				(Some(timestamp(1)), b"tail".to_vec())
			]
		);
		assert!(matches!(*h.fill.read(), Fill::Done));
	}

	/// Cancelling an update fill in a partial payload releases its tail without
	/// cancelling the live subscription: its next whole group still arrives.
	#[moq_clock::test]
	async fn cancelling_an_update_fill_does_not_park_the_live_tail() {
		const UPDATE: RequestId = RequestId(99);
		let h = Harness::new(
			Fill::Serving(Some(Timescale::MICRO)),
			vec![
				tail_stream(SEQUENCE, 0, &[b"initial"]),
				tail_stream(SEQUENCE + 1, 1, &[b"tail"]),
				tail_stream(SEQUENCE + 2, 0, &[b"next"]),
			],
		)
		.with_update(UPDATE);
		let mut initial = h.stream().await;
		h.subscriber.clone().recv_group(&mut initial).await.unwrap();
		let mut bytes = fill_stream_for(UPDATE, &[(SEQUENCE + 1, &[b"unfinished".as_slice()])]);
		bytes.pop();
		let pending = ScriptedSession::new(bytes);
		let (_, recv) = pending.open_bi(Default::default()).await.unwrap();
		let mut stream = Reader::new(crate::transport::boxed::RecvStream::new(recv), VERSION);
		let mut receiving = h.subscriber.clone();
		{
			let mut filling = std::pin::pin!(receiving.recv_fill(&mut stream));
			assert!(futures::poll!(filling.as_mut()).is_pending());
		}
		let mut tail = h.stream().await;
		let result = moq_clock::timeout(Duration::from_secs(1), h.subscriber.clone().recv_group(&mut tail))
			.await
			.expect("cancelled fill left its tail parked");
		result.expect("the live tail still supports passthrough delivery");
		let mut next = h.stream().await;
		h.subscriber.clone().recv_group(&mut next).await.unwrap();
		let mut reader = h.track.subscribe(None);
		let (sequence, frames) = read_group(&mut reader).await;
		assert_eq!(sequence, SEQUENCE + 2);
		assert_eq!(frames[0].1, b"next");
		assert!(h.session.log.closes().is_empty());
	}

	/// A tail parked on its head stops holding the end open, so the finished fill does until
	/// the head is claimed: otherwise the subscription looks settled between the fill
	/// finishing and the tail waking, and ends before the tail is read.
	#[moq_clock::test]
	async fn an_unclaimed_fill_holds_the_end_open() {
		let h = Harness::new(
			Fill::Serving(Some(Timescale::MICRO)),
			vec![
				fill_stream(SEQUENCE, &[b"head-0", b"head-1"]),
				tail_stream(SEQUENCE, 2, &[b"tail-2"]),
			],
		);
		let mut fill = h.stream().await;
		let mut tail = h.stream().await;

		let mut serve_tail = h.subscriber.clone();
		let mut tailing = std::pin::pin!(serve_tail.recv_group(&mut tail));
		assert!(
			futures::poll!(tailing.as_mut()).is_pending(),
			"the tail waits for its head"
		);
		h.subscriber.clone().recv_fill(&mut fill).await.expect("fill");

		let state = h.subscriber.state.lock().subscribes[&REQUEST].tail.consume();
		let count = state.read().streams();
		let mut settle = Settle::new(&crate::time::sim(), state);
		let mut settled = std::pin::pin!(kio::wait(|waiter| poll_settled(&mut settle, waiter, &h.fill, count)));
		assert!(
			futures::poll!(settled.as_mut()).is_pending(),
			"the head is still owed to the tail"
		);

		tailing.await.expect("tail");
		settled.await;
	}

	/// Relay `script` through an untimed track.
	///
	/// Also relays it through a second relay behind a moq-lite hop with the `ietf`
	/// extension, which must not change a byte. Object numbering is relay metadata; plain
	/// readers still see consecutive frames.
	async fn relay_passthrough(
		script: Vec<u8>,
		ingress: Version,
		egress: Version,
		fetch: Option<(u64, u64)>,
	) -> (Vec<u8>, crate::lite::test_transport::Log) {
		let direct = relay_passthrough_via(script.clone(), ingress, egress, fetch, None, false).await;
		let hopped = relay_passthrough_via(script, ingress, egress, fetch, None, true).await;
		let fetch_ok = |bytes: &[u8]| {
			let mut decoder = Decoder::new(bytes, egress.into());
			(decoder.varint().ok()? == ietf::FetchOk::ID).then_some(())?;
			let ok = ietf::FetchOk::decode(&mut decoder, egress).ok()?;
			Some((ok, bytes[bytes.len() - decoder.remaining()..].to_vec()))
		};
		match (fetch_ok(&direct.0), fetch_ok(&hopped.0)) {
			// A moq-lite FETCH says nothing of the track, so only a subscription tells the
			// second relay that the track ended. Everything else must match.
			(Some((direct_ok, direct_rest)), Some((hopped_ok, hopped_rest)))
				if direct_ok.end_of_track && !hopped_ok.end_of_track =>
			{
				// Without End of Track, the End Location is the requested end rather than the
				// track's, which can only reach further.
				assert!(
					(hopped_ok.end_location.group, hopped_ok.end_location.object)
						>= (direct_ok.end_location.group, direct_ok.end_location.object),
					"{ingress} -> {egress}"
				);
				assert_eq!(
					hopped_ok,
					ietf::FetchOk {
						end_of_track: false,
						end_location: hopped_ok.end_location,
						..direct_ok
					},
					"{ingress} -> {egress}: only End of Track and its End Location may differ"
				);
				assert_eq!(
					hopped_rest, direct_rest,
					"{ingress} -> {egress}: the hop changed the objects"
				);
			}
			_ => assert_eq!(
				hopped.0, direct.0,
				"{ingress} -> {egress}: the moq-lite hop changed the relayed objects"
			),
		}
		direct
	}

	/// Relay `script` through a track the ingress times in `timescale`, which the cache
	/// retains on the publisher's grid like [`accepted_info`], via one relay or two joined by a
	/// moq-lite `hop`.
	async fn relay_passthrough_via(
		script: Vec<u8>,
		ingress: Version,
		egress: Version,
		fetch: Option<(u64, u64)>,
		timescale: Option<Timescale>,
		hop: bool,
	) -> (Vec<u8>, crate::lite::test_transport::Log) {
		use crate::lite::test_transport::Sent;
		let origin = crate::origin::Config::new(crate::Hop::new(2).unwrap()).produce();
		let broadcast = origin.publish("room", crate::origin::Route::default()).unwrap();
		let track = broadcast
			.create_track(
				"video",
				accepted_info(
					&ietf::Properties {
						timescale,
						..Default::default()
					},
					ingress,
				),
			)
			.unwrap();
		let mut h = Harness::with_timescale(Fill::Done, vec![script], timescale);
		h.subscriber.version = ingress;
		h.subscriber.state.lock().subscribes.get_mut(&REQUEST).unwrap().producer = Some(track.clone());
		let mut stream = h.stream().await.with_version(ingress);
		h.subscriber.clone().recv_group(&mut stream).await.unwrap();
		track.finish().unwrap();
		let (served, _sessions) = match hop {
			true => {
				let (served, sessions) = crate::lite::ietf::test_hop(&origin, "room", [true, true]).await;
				(served, Some(sessions))
			}
			false => (origin, None),
		};

		let downstream = ScriptedSession::per_stream(vec![vec![]]);
		let peer = peer::PeerSetup::default();
		peer.set(peer::Peer::default());
		let publisher = crate::ietf::Publisher::new(
			crate::time::sim(),
			crate::driver::Transport::new(downstream.clone()),
			served.consume(),
			Control::new(false),
			None,
			peer,
			egress,
		);
		let mut body = Vec::new();
		let id = if let Some((start, end)) = fetch {
			let fetch_type = if Filter::is_draft20(egress) {
				ietf::FetchType::Filtered {
					namespace: Path::from_static("room"),
					track: "video".into(),
					filter: Filter::Absolute {
						start: ietf::Location {
							group: SEQUENCE,
							object: start,
						},
						end: Some(ietf::EndLocation {
							group: SEQUENCE,
							object: Some(end - 1),
						}),
					},
				}
			} else {
				ietf::FetchType::Standalone {
					namespace: Path::from_static("room"),
					track: "video".into(),
					start: ietf::Location {
						group: SEQUENCE,
						object: start,
					},
					end: ietf::Location {
						group: SEQUENCE,
						object: end,
					},
				}
			};
			ietf::Fetch {
				request_id: RequestId(3),
				subscriber_priority: 128,
				group_order: ietf::GroupOrder::Ascending,
				fetch_type,
				range_filters: false,
				fill_timeout: false,
				properties_wanted: true,
				epoch: None,
			}
			.encode_msg(&mut Encoder::new(&mut body, egress.into()), egress)
			.unwrap();
			ietf::Fetch::ID
		} else {
			ietf::Subscribe {
				max_age: None,
				new_group: None,
				request_id: RequestId(3),
				track_namespace: Path::from_static("room"),
				track_name: "video".into(),
				subscriber_priority: 128,
				group_order: ietf::GroupOrder::Ascending,
				filter: Filter::Unfiltered,
				fill: None,
				properties_wanted: true,
				forward: true,
				rendezvous: false,
				range_filters: false,
				authorization_token: None,
				epoch: None,
			}
			.encode_msg(&mut Encoder::new(&mut body, egress.into()), egress)
			.unwrap();
			ietf::Subscribe::ID
		};
		let stream = Stream::open(&mut crate::driver::Transport::new(downstream.clone()), egress)
			.await
			.unwrap();
		publisher
			.handle_stream(id, ietf::Body(body.into()), stream, false)
			.unwrap()
			.await;
		let mut streams = std::collections::BTreeMap::<usize, Vec<u8>>::new();
		for (id, event) in downstream.log.trail() {
			if let Sent::Write(bytes) = event {
				streams.entry(id).or_default().extend(bytes);
			}
		}
		let bytes = if fetch.is_some() {
			streams.into_values().flatten().collect()
		} else {
			streams
				.into_values()
				.find(|bytes| {
					ietf::GroupHeader::decode(&mut Decoder::new(bytes, egress.into()), egress)
						.is_ok_and(|header| header.group_id == SEQUENCE)
				})
				.expect("the relay emitted the group")
		};
		(bytes, downstream.log)
	}

	async fn relay_passthrough_stream(
		script: Vec<u8>,
		ingress: Version,
		egress: Version,
	) -> (Vec<u8>, crate::lite::test_transport::Log) {
		relay_passthrough(script, ingress, egress, None).await
	}

	#[moq_clock::test]
	async fn passthrough_preserves_object_properties_through_dispatch() {
		for version in [Version::Draft22] {
			let mut script = Vec::new();
			ietf::GroupHeader {
				track_alias: ALIAS,
				group_id: SEQUENCE,
				sub_group_id: 0,
				publisher_priority: 0,
				flags: ietf::GroupFlags {
					has_extensions: true,
					..Default::default()
				},
			}
			.encode(&mut Encoder::new(&mut script, version.into()), version)
			.unwrap();
			let properties = vec![2, 7, 3, 1, 42];
			let mut encoder = Encoder::new(&mut script, version.into());
			encoder.varint(0u64).unwrap();
			encoder.bytes(&properties).unwrap();
			encoder.varint(1u64).unwrap();
			script.push(99);
			let (bytes, _) = relay_passthrough_stream(script, version, version).await;
			let mut decoder = Decoder::new(&bytes, version.into());
			let header = ietf::GroupHeader::decode(&mut decoder, version).unwrap();
			assert!(header.flags.has_extensions, "{version}: properties were dropped");
			assert_eq!(decoder.varint().unwrap(), 0);
			assert_eq!(decoder.bytes().unwrap(), properties);
			assert_eq!(decoder.varint().unwrap(), 1);
			assert_eq!(decoder.slice(1).unwrap(), &[99]);
		}
	}

	/// A relayed Timestamp leaves in the units the publisher declared: the cache holds the
	/// track on the publisher's grid, so a copy of the same instance over another route
	/// matches it, including across a moq-lite hop that negotiated the `ietf` extension.
	#[moq_clock::test]
	async fn passthrough_keeps_the_publishers_timestamp_units() {
		let version = Version::Draft22;
		// Timestamp 42 (ms), then an unknown property that must survive beside it.
		let properties = [0x10, 42, 0x70, 7];
		let script = object_stream(version, true, &[(0, &properties, None)]);
		for hop in [false, true] {
			let timestamp = Timestamp::new(42, Timescale::MILLI).ok();
			let (bytes, log) =
				relay_passthrough_via(script.clone(), version, version, None, Some(Timescale::MILLI), hop).await;
			assert!(log.resets().is_empty(), "hop: {hop}");
			let mut decoder = Decoder::new(&bytes, version.into());
			ietf::GroupHeader::decode(&mut decoder, version).unwrap();
			assert_eq!(decoder.varint().unwrap(), 0);
			let relayed = bytes::Bytes::copy_from_slice(decoder.bytes().unwrap());
			let decoded = super::super::object::read(&relayed, version, timestamp.map(|_| Timescale::MILLI)).unwrap();
			assert_eq!(decoded.timestamp, timestamp, "hop: {hop}");
			assert_eq!(
				super::super::object::listed(&decoded.properties, version).len(),
				1,
				"hop: {hop}"
			);
		}
	}

	fn object_stream(version: Version, first: bool, objects: &[(u64, &[u8], Option<u64>)]) -> Vec<u8> {
		let mut bytes = Vec::new();
		ietf::GroupHeader {
			track_alias: ALIAS,
			group_id: SEQUENCE,
			sub_group_id: 0,
			publisher_priority: 128,
			flags: ietf::GroupFlags {
				first_object: first,
				has_extensions: true,
				has_end: true,
				..Default::default()
			},
		}
		.encode(&mut Encoder::new(&mut bytes, version.into()), version)
		.unwrap();
		let mut previous = None;
		for &(id, properties, status) in objects {
			let delta = previous.map_or(id, |previous| id - previous - 1);
			previous = Some(id);
			let mut encoder = Encoder::new(&mut bytes, version.into());
			encoder.varint(delta).unwrap();
			encoder.bytes(properties).unwrap();
			encoder.varint(u64::from(status.is_none())).unwrap();
			if let Some(status) = status {
				encoder.varint(status).unwrap();
			} else {
				bytes.push(99);
			}
		}
		bytes
	}

	#[moq_clock::test]
	async fn passthrough_fetch_requires_marked_gaps_and_preserves_fields() {
		for version in [Version::Draft22] {
			for marked in [false, true] {
				let properties = if marked { vec![0x3e, 6] } else { vec![] };
				let second = if marked { vec![0x3e, 1] } else { vec![] };
				for start in [0, 2, 7] {
					let script = object_stream(version, true, &[(6, &properties, None), (8, &second, None)]);
					let (bytes, log) = relay_passthrough(script, version, version, Some((start, 9))).await;
					let mut decoder = Decoder::new(&bytes, version.into());
					if !marked {
						assert_eq!(decoder.varint().unwrap(), ietf::RequestError::ID);
						assert_eq!(ietf::RequestError::decode(&mut decoder, version).unwrap().error_code, 3);
						assert!(decoder.is_empty());
						continue;
					}
					assert_eq!(decoder.varint().unwrap(), ietf::FetchOk::ID);
					let ok = ietf::FetchOk::decode(&mut decoder, version).unwrap();
					assert_eq!(
						ok.end_location.object,
						if Filter::is_draft20(version) { 8 } else { 9 },
						"{version} start={start}"
					);
					assert_eq!(decoder.varint().unwrap(), ietf::FetchHeader::TYPE);
					ietf::FetchHeader::decode(&mut decoder, version).unwrap();
					let mut prior = None;
					for (id, properties) in [(6, &properties), (8, &second)]
						.into_iter()
						.filter(|(id, _)| *id >= start)
					{
						let ietf::FetchObject::Object {
							object,
							properties: actual,
							..
						} = ietf::FetchObject::decode(&mut decoder, version).unwrap()
						else {
							panic!("object");
						};
						let wire = if version == Version::Draft22 {
							prior.map_or(id, |prior| id - prior)
						} else {
							id
						};
						assert_eq!(object, Some(wire), "{version}");
						assert_eq!(actual.as_ref(), Some(properties));
						assert_eq!(decoder.varint().unwrap(), 1);
						assert_eq!(decoder.slice(1).unwrap(), &[99]);
						prior = Some(id);
					}
					assert!(decoder.is_empty());
					assert!(log.resets().is_empty());
				}
			}
		}
	}

	/// A relayed FETCH ends its answer at the last frame rather than carrying the
	/// END_OF_GROUP object, and the next relay fills its cache from that answer.
	#[moq_clock::test]
	async fn passthrough_fetch_ends_at_the_last_frame() {
		for version in [Version::Draft22] {
			let objects = [
				(6, &[0x3e, 6][..], None),
				(8, &[0x3e, 1][..], None),
				(9, &[][..], Some(END_OF_GROUP)),
			];
			let script = object_stream(version, true, &objects);
			let (bytes, log) = relay_passthrough(script, version, version, Some((0, 12))).await;
			assert!(log.resets().is_empty());
			let mut decoder = Decoder::new(&bytes, version.into());
			assert_eq!(decoder.varint().unwrap(), ietf::FetchOk::ID, "{version}");
			let ok = ietf::FetchOk::decode(&mut decoder, version).unwrap();
			let last = if Filter::is_draft20(version) { 8 } else { 9 };
			assert_eq!(ok.end_location.object, last, "{version}");
			assert_eq!(decoder.varint().unwrap(), ietf::FetchHeader::TYPE);
			ietf::FetchHeader::decode(&mut decoder, version).unwrap();

			// The next relay reads the same objects into its cache, up to the same end.
			let end = last + u64::from(Filter::is_draft20(version));
			let mut run = GroupFetchRun::new(version, decoder.rest().to_vec()).await;
			let mut reader = run
				.recv_cached(Some(end), None)
				.await
				.unwrap_or_else(|err| panic!("{version}: {err}"));
			for id in [6, 8] {
				let frame = kio::wait(|waiter| reader.poll_read_object(waiter))
					.await
					.unwrap()
					.unwrap();
				assert_eq!(frame.fields.as_ref().unwrap().id, id, "{version}");
			}
			assert!(reader.read_frame().await.unwrap().is_none());
			assert!(reader.end_object().is_none(), "{version}");
		}
	}

	/// Only a Normal object may carry properties: a status object with them is refused on
	/// ingest.
	#[moq_clock::test]
	async fn passthrough_keeps_properties_off_status_objects() {
		for version in [Version::Draft22] {
			for status in [END_OF_GROUP, END_OF_TRACK] {
				let script = object_stream(version, true, &[(0, &[], None), (1, &[0x20, 7], Some(status))]);
				let mut h = Harness::with_timescale(Fill::Done, vec![script], None);
				h.subscriber.version = version;
				let mut stream = h.stream().await.with_version(version);
				let result = h.subscriber.clone().recv_group(&mut stream).await;
				assert!(
					matches!(result, Err(Error::Session(SessionError::ProtocolViolation))),
					"{version} {status}: {result:?}"
				);
			}
		}
	}

	#[moq_clock::test]
	async fn passthrough_refuses_contradictory_gap_properties() {
		for properties in [vec![0x3e, 2], vec![0x3e, 3], vec![0x3e, 1, 0, 1]] {
			let script = object_stream(VERSION, true, &[(0, &[], None), (2, &properties, None)]);
			let h = Harness::with_timescale(Fill::Done, vec![script], None);
			let mut stream = h.stream().await;
			assert!(matches!(
				h.subscriber.clone().recv_group(&mut stream).await,
				Err(Error::MalformedTrack)
			));
		}
	}

	#[moq_clock::test]
	async fn passthrough_fetch_ingest_keeps_marked_gaps_and_stitches_its_tail() {
		for version in [Version::Draft22] {
			let mut objects = Vec::new();
			for (index, id) in [6u64, 8].into_iter().enumerate() {
				let properties = vec![0x3e, if index == 0 { 6 } else { 1 }];
				ietf::FetchObject::Object {
					subgroup: ietf::FetchSubgroup::Zero,
					group: (index == 0).then_some(SEQUENCE),
					object: Some(if version == Version::Draft22 && index > 0 {
						2
					} else {
						id
					}),
					priority: (index == 0).then_some(128),
					properties: Some(properties),
				}
				.encode(&mut Encoder::new(&mut objects, version.into()), version)
				.unwrap();
				Encoder::new(&mut objects, version.into()).varint(1u64).unwrap();
				objects.push(99);
			}
			let mut run = GroupFetchRun::new(version, objects.clone()).await;
			let mut reader = run.recv_cached(Some(9), None).await.unwrap();
			for id in [6, 8] {
				let frame = kio::wait(|waiter| reader.poll_read_object(waiter))
					.await
					.unwrap()
					.unwrap();
				assert_eq!(frame.fields.as_ref().unwrap().id, id);
				assert_eq!(
					super::super::object::listed(&frame.fields.as_ref().unwrap().properties, version)[0].id,
					0x3e
				);
			}

			let tail = object_stream(version, false, &[(9, &[], None)]);
			let mut h = Harness::with_timescale(Fill::Serving(None), vec![tail], None);
			h.subscriber.version = version;
			let mut run = GroupFetchRun::new(version, objects).await;
			let fill = h
				.subscriber
				.run_fill(&mut run.stream, &mut h.track, None, true)
				.await
				.unwrap();
			*h.fill.write().ok().expect("fill open") = fill;
			let mut consumer = h.track.subscribe(None);
			let mut tail = h.stream().await.with_version(version);
			h.subscriber.clone().recv_group(&mut tail).await.unwrap();
			let mut group = consumer.recv_group().await.unwrap().unwrap();
			assert_eq!(group.wire().unwrap().first_id, 6);
			assert!(
				group.wire().unwrap().fetchable,
				"a contiguous tail adds no gap to the FETCH head"
			);
			for id in [6, 8, 9] {
				let frame = kio::wait(|waiter| group.poll_read_object(waiter))
					.await
					.unwrap()
					.unwrap();
				assert_eq!(frame.fields.as_ref().unwrap().id, id);
			}
			assert!(group.read_frame().await.unwrap().is_none());
		}
	}

	#[moq_clock::test]
	async fn passthrough_preserves_ids_first_object_and_end_status() {
		for version in [Version::Draft22] {
			for first in [false, true] {
				let objects = [
					(6, &[][..], None),
					(8, &[][..], None),
					(11, &[][..], Some(END_OF_GROUP)),
				];
				let script = object_stream(version, first, &objects);
				let (bytes, log) = relay_passthrough_stream(script, version, version).await;
				assert!(log.resets().is_empty());
				let mut decoder = Decoder::new(&bytes, version.into());
				let header = ietf::GroupHeader::decode(&mut decoder, version).unwrap();
				assert_eq!(header.flags.first_object, first);
				for (delta, status) in [(6, None), (1, None), (2, Some(END_OF_GROUP))] {
					assert_eq!(decoder.varint().unwrap(), delta);
					assert!(decoder.bytes().unwrap().is_empty());
					assert_eq!(decoder.varint().unwrap(), u64::from(status.is_none()));
					if let Some(status) = status {
						assert_eq!(decoder.varint().unwrap(), status);
					} else {
						assert_eq!(decoder.slice(1).unwrap(), &[99]);
					}
				}
				assert!(decoder.is_empty());
			}
		}
	}

	#[moq_clock::test]
	async fn passthrough_preserves_immutable_bytes() {
		for ingress in [Version::Draft18, Version::Draft20, Version::Draft22] {
			let mut inner = Vec::new();
			let mut encoder = Encoder::new(&mut inner, ingress.into());
			for value in [2u64, 96_000, 2, 128, 1] {
				encoder.varint(value).unwrap();
			}
			encoder.bytes(&[42; 130]).unwrap();
			let mut properties = Vec::new();
			let mut encoder = Encoder::new(&mut properties, ingress.into());
			encoder.varint(0x0bu64).unwrap();
			encoder.bytes(&inner).unwrap();
			for egress in [Version::Draft18, Version::Draft20, Version::Draft22] {
				let script = object_stream(ingress, true, &[(0, &properties, None)]);
				let (bytes, log) = relay_passthrough_stream(script, ingress, egress).await;
				assert!(log.resets().is_empty());
				let mut decoder = Decoder::new(&bytes, egress.into());
				ietf::GroupHeader::decode(&mut decoder, egress).unwrap();
				assert_eq!(decoder.varint().unwrap(), 0);
				assert_eq!(decoder.bytes().unwrap(), properties);
			}
		}
	}

	/// Draft 22 assigns 0x06 to SUBGROUP_DELIVERY_TIMEOUT. A draft-18 publisher's 0x06 is
	/// dropped on its way to draft 22 when mutable, and refused when sealed, through a
	/// moq-lite hop too: drafts 18 and 22 serialize a block alike, so only the track's
	/// carried ingress draft tells them apart.
	#[moq_clock::test]
	async fn passthrough_keeps_0x06_from_crossing_to_draft_22() {
		let (ingress, egress) = (Version::Draft18, Version::Draft22);
		// 0x06 = 42 and 0x20 = 7, mutable.
		let mutable = [0x06, 42, 0x20 - 0x06, 7];
		let script = object_stream(ingress, true, &[(0, &mutable, None)]);
		let (bytes, log) = relay_passthrough_stream(script, ingress, egress).await;
		assert!(log.resets().is_empty());
		let mut decoder = Decoder::new(&bytes, egress.into());
		ietf::GroupHeader::decode(&mut decoder, egress).unwrap();
		assert_eq!(decoder.varint().unwrap(), 0);
		assert_eq!(decoder.bytes().unwrap(), [0x20, 7], "the mutable 0x06 crossed");

		// 0x0b = { 0x06 = 42 }, sealed.
		let sealed = [0x0b, 2, 0x06, 42];
		let script = object_stream(ingress, true, &[(0, &sealed, None)]);
		let (_, log) = relay_passthrough_stream(script, ingress, egress).await;
		assert!(!log.resets().is_empty(), "the sealed 0x06 crossed");

		// Between two draft-18 or two draft-22 sides both cross unchanged.
		for properties in [&mutable[..], &sealed] {
			let script = object_stream(ingress, true, &[(0, properties, None)]);
			let (bytes, log) = relay_passthrough_stream(script, ingress, ingress).await;
			assert!(log.resets().is_empty());
			let mut decoder = Decoder::new(&bytes, ingress.into());
			ietf::GroupHeader::decode(&mut decoder, ingress).unwrap();
			assert_eq!(decoder.varint().unwrap(), 0);
			assert_eq!(decoder.bytes().unwrap(), properties);
		}
	}

	#[moq_clock::test]
	async fn passthrough_preserves_datagram_objects_through_dispatch() {
		// Also through a second relay behind a moq-lite hop with the `ietf` extension.
		for (version, ranged, hop) in [Version::Draft22]
			.into_iter()
			.flat_map(|version| [(version, false), (version, true)])
			.filter(|(version, ranged)| !*ranged || *version == Version::Draft22)
			.flat_map(|(version, ranged)| [(version, ranged, false), (version, ranged, true)])
		{
			let origin = crate::origin::Config::new(crate::Hop::new(2).unwrap()).produce();
			let broadcast = origin.publish("room", crate::origin::Route::default()).unwrap();
			let track = broadcast
				.create_track("video", accepted_info(&Default::default(), version))
				.unwrap();
			let mut h = Harness::with_timescale(Fill::Done, vec![], None);
			h.subscriber.version = version;
			h.subscriber.state.lock().subscribes.get_mut(&REQUEST).unwrap().producer = Some(track.clone());
			let properties = vec![2, 7, 3, 1, 42];
			let mut original = Vec::new();
			for id in [6, 7, 8] {
				let datagram = ietf::ObjectDatagram {
					track_alias: ALIAS,
					group_id: SEQUENCE,
					object_id: Some(id),
					// An omitted priority inherits the track default, which the
					// downstream response need not carry.
					publisher_priority: (id != 6).then_some(200),
					end_of_group: false,
					properties: (id != 8).then(|| properties.clone()),
					body: if id == 8 {
						ietf::DatagramBody::Status(END_OF_GROUP)
					} else {
						ietf::DatagramBody::Payload(bytes::Bytes::from_static(b"object"))
					},
				};
				h.subscriber
					.recv_datagram(datagram.encode_bytes(version).unwrap())
					.unwrap();
				original.push(datagram);
			}
			track.finish().unwrap();
			let (served, _sessions) = match hop {
				true => {
					let (served, sessions) = crate::lite::ietf::test_hop(&origin, "room", [true, true]).await;
					(served, Some(sessions))
				}
				false => (origin, None),
			};
			let downstream = ScriptedSession::per_stream(vec![vec![]]).with_datagrams(65535);
			let peer = peer::PeerSetup::default();
			peer.set(peer::Peer::default());
			let publisher = crate::ietf::Publisher::new(
				crate::time::sim(),
				crate::driver::Transport::new(downstream.clone()),
				served.consume(),
				Control::new(false),
				None,
				peer,
				version,
			);
			let subscribe = ietf::Subscribe {
				max_age: None,
				new_group: None,
				request_id: RequestId(3),
				track_namespace: Path::from_static("room"),
				track_name: "video".into(),
				subscriber_priority: 128,
				group_order: ietf::GroupOrder::Ascending,
				filter: if ranged {
					Filter::Absolute {
						start: ietf::Location {
							group: SEQUENCE,
							object: 7,
						},
						end: Some(ietf::EndLocation {
							group: SEQUENCE,
							object: Some(7),
						}),
					}
				} else {
					Filter::Unfiltered
				},
				fill: None,
				properties_wanted: true,
				forward: true,
				rendezvous: false,
				range_filters: false,
				authorization_token: None,
				epoch: None,
			};
			let mut body = Vec::new();
			subscribe
				.encode_msg(&mut Encoder::new(&mut body, version.into()), version)
				.unwrap();
			let stream = Stream::open(&mut crate::driver::Transport::new(downstream.clone()), version)
				.await
				.unwrap();
			publisher
				.handle_stream(ietf::Subscribe::ID, ietf::Body(body.into()), stream, false)
				.unwrap()
				.await;
			let sent = downstream.log.datagrams.lock().unwrap();
			original.retain(|datagram| !ranged || datagram.object_id == Some(7));
			assert_eq!(sent.len(), original.len(), "{version} ranged={ranged} hop={hop}");
			let inherited = super::super::priority::to_wire(track.publisher_priority());
			for (bytes, mut expected) in sent.iter().zip(original) {
				expected.track_alias = 3;
				expected.publisher_priority = expected.publisher_priority.or(Some(inherited));
				let (actual, _) = ietf::ObjectDatagram::decode_slice(bytes, version).unwrap();
				assert_eq!(actual, expected);
			}
		}
	}

	#[moq_clock::test]
	async fn passthrough_translates_mutable_property_encodings() {
		for ingress in [Version::Draft22] {
			let mut properties = Vec::new();
			let mut encoder = Encoder::new(&mut properties, ingress.into());
			encoder.varint(0x80u64).unwrap();
			encoder.varint(96_000u64).unwrap();
			encoder.varint(1u64).unwrap();
			encoder.bytes(&[42; 130]).unwrap();
			for egress in [Version::Draft22] {
				let script = object_stream(ingress, true, &[(0, &properties, None)]);
				let (bytes, log) = relay_passthrough_stream(script, ingress, egress).await;
				assert!(log.resets().is_empty());
				let mut decoder = Decoder::new(&bytes, egress.into());
				ietf::GroupHeader::decode(&mut decoder, egress).unwrap();
				assert_eq!(decoder.varint().unwrap(), 0);
				let mut props = Decoder::new(decoder.bytes().unwrap(), egress.into());
				assert_eq!(props.varint().unwrap(), 0x80);
				assert_eq!(props.varint().unwrap(), 96_000);
				assert_eq!(props.varint().unwrap(), 1);
				assert_eq!(props.bytes().unwrap(), &[42; 130]);
				assert!(props.is_empty());
			}
		}
	}

	#[moq_clock::test]
	async fn passthrough_accepts_nonzero_first_objects_and_gaps() {
		for version in [Version::Draft22] {
			let mut script = Vec::new();
			ietf::GroupHeader {
				track_alias: ALIAS,
				group_id: SEQUENCE,
				sub_group_id: 0,
				publisher_priority: 0,
				flags: ietf::GroupFlags::default(),
			}
			.encode(&mut Encoder::new(&mut script, version.into()), version)
			.unwrap();
			for delta in [6u64, 1, 2] {
				let mut encoder = Encoder::new(&mut script, version.into());
				encoder.varint(delta).unwrap();
				encoder.varint(1u64).unwrap();
				script.push(42);
			}
			let mut h = Harness::with_timescale(Fill::Done, vec![script], None);
			h.subscriber.version = version;
			let mut consumer = h.track.subscribe(None);
			let mut stream = h.stream().await.with_version(version);
			h.subscriber.clone().recv_group(&mut stream).await.unwrap();
			assert_eq!(read_group(&mut consumer).await, (SEQUENCE, vec![(None, vec![42]); 3]));
		}
	}

	/// Relays retain a datagram's Object ID; a plain reader skips it until object 0 arrives.
	#[moq_clock::test]
	async fn passthrough_accepts_datagrams_past_object_zero() {
		let version = Version::Draft22;
		let mut h = Harness::with_timescale(Fill::Done, vec![], None);
		h.subscriber.version = version;
		let mut consumer = h.track.subscribe(None);
		let mut plain = h.track.subscribe(None);
		let mut datagram = ietf::ObjectDatagram {
			track_alias: ALIAS,
			group_id: SEQUENCE,
			object_id: Some(6),
			publisher_priority: Some(128),
			end_of_group: false,
			properties: None,
			body: ietf::DatagramBody::Payload(bytes::Bytes::from_static(b"object-six")),
		};
		h.subscriber
			.recv_datagram(datagram.encode_bytes(version).unwrap())
			.unwrap();
		let received = consumer.poll_recv_stored_datagram(&kio::Waiter::noop());
		assert!(
			matches!(&received, Poll::Ready(Ok(Some(data)))
				if data.payload == b"object-six"[..] && data.wire.as_ref().unwrap().object_id == Some(6)),
			"{received:?}"
		);
		assert!(plain.poll_recv_datagram(&kio::Waiter::noop()).is_pending());
		datagram.object_id = Some(0);
		datagram.body = ietf::DatagramBody::Payload(bytes::Bytes::from_static(b"object-zero"));
		h.subscriber
			.recv_datagram(datagram.encode_bytes(version).unwrap())
			.unwrap();
		let received = plain.poll_recv_datagram(&kio::Waiter::noop());
		assert!(
			matches!(&received, Poll::Ready(Ok(Some(data))) if data.payload == b"object-zero"[..]),
			"{received:?}"
		);
	}

	#[moq_clock::test]
	async fn an_overflowing_object_id_reaches_session_teardown() {
		let mut bytes = tail_stream(SEQUENCE, 0, &[b"head"]);
		crate::coding::Encoder::new(&mut bytes, VERSION.into())
			.varint(u64::MAX)
			.unwrap();
		let h = Harness::new(Fill::Done, vec![bytes]);
		let mut stream = h.stream().await;
		let result = h.subscriber.clone().recv_group(&mut stream).await;
		assert!(
			matches!(result, Err(Error::Session(crate::SessionError::ProtocolViolation))),
			"{result:?}"
		);
	}

	/// A subgroup 1 stream costs only itself: the track's subgroup 0 stream still arrives.
	#[moq_clock::test]
	async fn a_non_zero_subgroup_leaves_the_track_flowing() {
		let mut refused = Vec::new();
		ietf::GroupHeader {
			track_alias: ALIAS,
			group_id: SEQUENCE,
			sub_group_id: 1,
			publisher_priority: 0,
			flags: ietf::GroupFlags {
				has_subgroup: true,
				first_object: true,
				..Default::default()
			},
		}
		.encode(&mut crate::coding::Encoder::new(&mut refused, VERSION.into()), VERSION)
		.unwrap();

		let h = Harness::new(Fill::Done, vec![refused, tail_stream(SEQUENCE, 0, &[b"ok"])]);
		let mut consumer = h.track.subscribe(None);

		let mut stream = h.stream().await;
		let result = h.subscriber.clone().recv_group(&mut stream).await;
		assert!(matches!(result, Err(Error::Unsupported)), "{result:?}");

		let mut stream = h.stream().await;
		h.subscriber.clone().recv_group(&mut stream).await.unwrap();
		let (sequence, frames) = read_group(&mut consumer).await;
		assert_eq!(sequence, SEQUENCE);
		assert_eq!(frames.len(), 1);
		assert_eq!(frames[0].1, b"ok");
		assert!(h.session.log.closes().is_empty());
	}

	/// A compact header names its subgroup by the first Object ID, so one starting at object
	/// 2 is subgroup 2: refused like an explicit one, never taken as subgroup 0, even when the
	/// rest of that object is cut off. A compact stream with no objects names no subgroup and
	/// is subgroup 0.
	#[moq_clock::test]
	async fn a_compact_non_zero_subgroup_is_refused() {
		for version in [Version::Draft22] {
			let compact = |sequence: u64, first: Option<u64>| {
				let mut bytes = Vec::new();
				ietf::GroupHeader {
					track_alias: ALIAS,
					group_id: sequence,
					sub_group_id: 0,
					publisher_priority: 0,
					flags: ietf::GroupFlags {
						has_subgroup_object: true,
						..Default::default()
					},
				}
				.encode(&mut Encoder::new(&mut bytes, version.into()), version)
				.unwrap();
				if let Some(first) = first {
					let mut encoder = Encoder::new(&mut bytes, version.into());
					encoder.varint(first).unwrap();
					encoder.varint(1u64).unwrap();
					bytes.push(42);
				}
				bytes
			};
			let mut truncated = compact(SEQUENCE + 3, None);
			Encoder::new(&mut truncated, version.into()).varint(2u64).unwrap();
			let scripts = vec![
				compact(SEQUENCE, Some(2)),
				truncated,
				compact(SEQUENCE + 1, Some(0)),
				compact(SEQUENCE + 2, None),
			];
			let mut h = Harness::with_timescale(Fill::Done, scripts, None);
			h.subscriber.version = version;
			let mut consumer = h.track.subscribe(None);

			let mut stream = h.stream().await.with_version(version);
			let result = h.subscriber.clone().recv_group(&mut stream).await;
			assert!(matches!(result, Err(Error::Unsupported)), "{version}: {result:?}");
			assert_eq!(
				h.track.latest_sequence(),
				None,
				"{version}: the refused subgroup opened a group"
			);

			let mut stream = h.stream().await.with_version(version);
			let result = h.subscriber.clone().recv_group(&mut stream).await;
			assert!(matches!(result, Err(Error::Unsupported)), "{version}: {result:?}");
			assert_eq!(
				h.track.latest_sequence(),
				None,
				"{version}: a truncated subgroup opened a group"
			);

			let mut stream = h.stream().await.with_version(version);
			h.subscriber.clone().recv_group(&mut stream).await.unwrap();
			assert_eq!(read_group(&mut consumer).await, (SEQUENCE + 1, vec![(None, vec![42])]));

			let mut stream = h.stream().await.with_version(version);
			h.subscriber.clone().recv_group(&mut stream).await.unwrap();
			assert_eq!(read_group(&mut consumer).await, (SEQUENCE + 2, vec![]));
			assert!(h.session.log.closes().is_empty(), "{version}");
		}
	}

	/// A subgroup stream for `sequence` without extension headers, starting at object 0: the
	/// header, then `varints`, then `payload` verbatim.
	fn raw_stream(sequence: u64, varints: &[u64], payload: &[u8]) -> Vec<u8> {
		let mut buf = Vec::new();
		ietf::GroupHeader {
			track_alias: ALIAS,
			group_id: sequence,
			sub_group_id: 0,
			publisher_priority: 0,
			flags: ietf::GroupFlags {
				first_object: true,
				..Default::default()
			},
		}
		.encode(&mut crate::coding::Encoder::new(&mut buf, VERSION.into()), VERSION)
		.unwrap();
		let mut encoder = crate::coding::Encoder::new(&mut buf, VERSION.into());
		for value in varints {
			encoder.varint(*value).unwrap();
		}
		buf.extend_from_slice(payload);
		buf
	}

	/// A truncated property inside a later object's extension block is a malformed group.
	#[moq_clock::test]
	async fn a_malformed_extension_block_is_refused() {
		// Delta 0, then a 1-byte extension block holding half of a 2-byte varint.
		let mut stream = tail_stream(SEQUENCE, 0, &[b"ok"]);
		stream.extend_from_slice(&[0, 1, 0x40]);
		let h = Harness::new(Fill::Done, vec![stream]);
		h.dispatch_refused().await;
	}

	/// A FIN before the declared payload ends.
	#[moq_clock::test]
	async fn a_payload_cut_short_is_refused() {
		let h = Harness::with_timescale(Fill::Done, vec![raw_stream(SEQUENCE, &[0, 4], &[1, 2])], None);
		h.dispatch_refused().await;
	}

	/// A declared object size past what a group may hold.
	#[moq_clock::test]
	async fn an_oversized_frame_is_refused() {
		let size = crate::group::MAX_CACHE_BYTES + 1;
		let h = Harness::with_timescale(Fill::Done, vec![raw_stream(SEQUENCE, &[0, size], &[])], None);
		h.dispatch_refused().await;
	}

	/// A second stream for a group the track already holds.
	#[moq_clock::test]
	async fn a_second_stream_for_a_group_is_refused() {
		let whole = raw_stream(SEQUENCE, &[0, 1], &[42]);
		let h = Harness::with_timescale(Fill::Done, vec![whole.clone(), whole], None);
		let mut stream = h.stream().await;
		h.subscriber.clone().recv_group(&mut stream).await.unwrap();
		h.dispatch_refused().await;
	}

	#[moq_clock::test]
	async fn object_extension_limit() {
		for size in [65536usize, 65537] {
			for first in [true, false] {
				let mut script = Vec::new();
				ietf::GroupHeader {
					track_alias: ALIAS,
					group_id: SEQUENCE,
					sub_group_id: 0,
					publisher_priority: 0,
					flags: ietf::GroupFlags {
						has_extensions: true,
						..Default::default()
					},
				}
				.encode(&mut crate::coding::Encoder::new(&mut script, VERSION.into()), VERSION)
				.unwrap();
				if !first {
					// A complete first object exercises the ingestion path on the next one.
					script.extend_from_slice(&[0, 0, 1, 42]);
				}
				crate::coding::Encoder::new(&mut script, VERSION.into())
					.varint(0u64)
					.unwrap();
				crate::coding::Encoder::new(&mut script, VERSION.into())
					.varint(size as u64)
					.unwrap();
				if size == 65536 {
					// Unknown even properties with value zero, valid with delta type ids.
					script.resize(script.len() + size, 0);
					script.extend_from_slice(&[1, 42]);
				}
				// Over-limit lengths deliberately carry no extension bytes. The objects carry
				// no Timestamp, so the track is untimed.
				let h = Harness::with_timescale(Fill::Done, vec![script], None);
				let mut consumer = h.track.subscribe(None);
				let mut stream = h.stream().await;
				let result = h.subscriber.clone().recv_group(&mut stream).await;
				if size == 65536 {
					result.unwrap();
					let (_, frames) = read_group(&mut consumer).await;
					assert_eq!(frames.len(), if first { 1 } else { 2 });
				} else {
					assert!(matches!(
						result,
						Err(Error::Decode(DecodeError::MessageTooLarge {
							size: 65537,
							max: 65536
						}))
					));
					// The stream dispatcher uses this mapping to stop only this stream.
					assert_eq!(
						crate::StreamError::from(&result.unwrap_err()),
						crate::StreamError::MalformedTrack
					);
				}
				assert!(h.session.log.closes().is_empty());
			}
		}
	}

	/// Append a status object such as END_OF_TRACK: delta 0, an empty payload, then its status.
	fn end_marker(mut stream: Vec<u8>, status: u64) -> Vec<u8> {
		// The id delta, an empty extension block, a zero size, and the status.
		for value in [0u64, 0, 0, status] {
			crate::coding::Encoder::new(&mut stream, VERSION.into())
				.varint(value)
				.unwrap();
		}
		stream
	}

	/// A fill object without a Timestamp on a timed track ends the track, as a subgroup
	/// object does: the fill is the head of a group the subscription carries.
	#[moq_clock::test]
	async fn an_unstamped_fill_object_ends_the_track() {
		use futures::FutureExt;

		let mut script = Vec::new();
		crate::coding::Encoder::new(&mut script, VERSION.into())
			.varint(ietf::FetchHeader::TYPE)
			.unwrap();
		ietf::FetchHeader { request_id: REQUEST }
			.encode(&mut crate::coding::Encoder::new(&mut script, VERSION.into()), VERSION)
			.unwrap();
		ietf::FetchObject::Object {
			subgroup: ietf::FetchSubgroup::Zero,
			group: Some(SEQUENCE),
			object: Some(0),
			priority: Some(0),
			properties: None,
		}
		.encode(&mut crate::coding::Encoder::new(&mut script, VERSION.into()), VERSION)
		.unwrap();
		crate::coding::Encoder::new(&mut script, VERSION.into())
			.varint(1u64)
			.unwrap();
		script.push(42);

		let h = Harness::new(Fill::Serving(Some(Timescale::MICRO)), vec![script]);
		let mut stream = h.stream().await;
		let res = h.subscriber.clone().recv_fill(&mut stream).await;
		assert!(matches!(res, Err(Error::MalformedTrack)), "{res:?}");
		assert!(matches!(
			h.track.closed().now_or_never(),
			Some(Err(Error::MalformedTrack))
		));
	}

	/// A subgroup object without a Timestamp on a timed track makes the whole track
	/// malformed, so the track ends rather than only its group.
	#[moq_clock::test]
	async fn an_unstamped_subgroup_object_ends_the_track() {
		use futures::FutureExt;

		let mut script = Vec::new();
		ietf::GroupHeader {
			track_alias: ALIAS,
			group_id: SEQUENCE,
			sub_group_id: 0,
			publisher_priority: 0,
			flags: ietf::GroupFlags {
				first_object: true,
				..Default::default()
			},
		}
		.encode(&mut crate::coding::Encoder::new(&mut script, VERSION.into()), VERSION)
		.unwrap();
		// The id delta, the size, and the payload, with no extension block.
		script.extend_from_slice(&[0, 1, 42]);

		let h = Harness::new(Fill::Done, vec![script]);
		let mut stream = h.stream().await;
		let res = h.subscriber.clone().recv_group(&mut stream).await;
		assert!(matches!(res, Err(Error::MalformedTrack)), "{res:?}");
		assert!(matches!(
			h.track.closed().now_or_never(),
			Some(Err(Error::MalformedTrack))
		));
	}

	/// END_OF_TRACK after a group's last object ends the track right after that group.
	#[moq_clock::test]
	async fn an_end_of_track_after_a_group_ends_the_track_after_it() {
		let h = Harness::new(
			Fill::Done,
			vec![end_marker(tail_stream(SEQUENCE, 0, &[b"last"]), END_OF_TRACK)],
		);
		let mut consumer = h.track.subscribe(None);
		let mut stream = h.stream().await;

		h.subscriber.clone().recv_group(&mut stream).await.unwrap();
		assert_eq!(
			h.track.finished(),
			Some(track::Position {
				group: SEQUENCE,
				frame: 1
			})
		);

		let mut group = consumer.recv_group().await.unwrap().expect("the group arrives");
		assert_eq!(group.read_frame().await.unwrap().unwrap().payload.as_ref(), b"last");
		assert!(group.read_frame().await.unwrap().is_none(), "the group is finished");
		assert!(consumer.recv_group().await.unwrap().is_none(), "then the track ends");
	}

	#[moq_clock::test]
	async fn a_standalone_end_of_track_preserves_a_nonzero_object() {
		let mut marker = tail_stream(5, 5, &[]);
		for value in [5u64, 0, 0, END_OF_TRACK] {
			Encoder::new(&mut marker, VERSION.into()).varint(value).unwrap();
		}
		let h = Harness::new(Fill::Done, vec![marker]);
		let mut stream = h.stream().await;
		h.subscriber.clone().recv_group(&mut stream).await.unwrap();
		assert_eq!(h.track.finished(), Some(track::Position { group: 5, frame: 5 }));
		assert_eq!(
			h.track.latest_sequence(),
			None,
			"a terminal marker creates no payload group"
		);
	}

	/// A group at or past the end an END_OF_TRACK declared contradicts that end, which no
	/// later stream can repair, so the whole track fails rather than ending clean without it.
	#[moq_clock::test]
	async fn a_group_past_the_declared_end_aborts_the_track() {
		use futures::FutureExt;

		let mut h = Harness::new(Fill::Done, vec![tail_stream(SEQUENCE, 0, &[b"late"])]);
		h.track.finish_at(track::Position::group(SEQUENCE), None).unwrap();
		let mut stream = h.stream().await;

		let res = h.subscriber.clone().recv_group(&mut stream).await;
		assert!(matches!(res, Err(Error::ProtocolViolation)), "{res:?}");
		assert!(matches!(
			h.track.closed().now_or_never(),
			Some(Err(Error::ProtocolViolation))
		));
	}

	/// END_OF_TRACK at object 0 says the group does not exist, so the track ends before it
	/// and no group is created for it.
	#[moq_clock::test]
	async fn an_end_of_track_at_object_zero_creates_no_group() {
		let h = Harness::new(
			Fill::Done,
			vec![end_marker(tail_stream(SEQUENCE, 0, &[]), END_OF_TRACK)],
		);
		let mut stream = h.stream().await;

		h.subscriber.clone().recv_group(&mut stream).await.unwrap();
		assert_eq!(h.track.finished(), Some(track::Position::group(SEQUENCE)));
		assert_eq!(h.track.latest_sequence(), None, "no group was created");
	}

	/// The group ended exactly where we joined it, so the subscription's stream carries no
	/// objects at all. That still ends the group, which is what publishes the head.
	#[moq_clock::test]
	async fn an_empty_tail_finishes_the_filled_group() {
		let h = Harness::new(
			Fill::Serving(Some(Timescale::MICRO)),
			vec![
				fill_stream(SEQUENCE, &[b"head-0", b"head-1"]),
				tail_stream(SEQUENCE, 2, &[]),
			],
		);
		let mut consumer = h.track.subscribe(None);

		let mut fill = h.stream().await;
		let mut tail = h.stream().await;

		h.subscriber.clone().recv_fill(&mut fill).await.expect("fill");
		h.subscriber.clone().recv_group(&mut tail).await.expect("tail");

		let (sequence, frames) = read_group(&mut consumer).await;
		assert_eq!(sequence, SEQUENCE);
		assert_eq!(frames.len(), 2, "the head is the whole group");
	}

	/// A fill that ends without the head it owed settles before the mid-group tail arrives.
	/// The tail then has a hole at the front, so it is dropped as it would be had it waited
	/// on the fill, rather than cached as a whole group starting at its first object.
	#[moq_clock::test]
	async fn a_headless_tail_after_the_fill_settles_is_dropped() {
		let h = Harness::new(
			Fill::Serving(Some(Timescale::MICRO)),
			vec![
				fill_stream_for::<&[u8]>(REQUEST, &[]),
				tail_stream(SEQUENCE, 2, &[b"tail-2"]),
				tail_stream(SEQUENCE + 1, 0, &[b"next-0"]),
			],
		)
		.with_largest(SEQUENCE);
		let mut consumer = h.track.subscribe(None);

		let mut fill = h.stream().await;
		h.subscriber.clone().recv_fill(&mut fill).await.expect("fill");
		assert!(matches!(*h.fill.read(), Fill::Failed { sequence: SEQUENCE }));

		h.dispatch_refused().await;

		let mut next = h.stream().await;
		h.subscriber.clone().recv_group(&mut next).await.expect("next group");
		let (sequence, frames) = read_group(&mut consumer).await;
		assert_eq!(sequence, SEQUENCE + 1, "the headless group was dropped");
		assert_eq!(frames.len(), 1);
	}

	/// The same headless tail arriving first waits on the fill, and is dropped once the fill
	/// ends without the head.
	#[moq_clock::test]
	async fn a_headless_tail_before_the_fill_settles_is_dropped() {
		let h = Harness::new(
			Fill::Serving(Some(Timescale::MICRO)),
			vec![
				fill_stream_for::<&[u8]>(REQUEST, &[]),
				tail_stream(SEQUENCE, 2, &[b"tail-2"]),
				tail_stream(SEQUENCE + 1, 0, &[b"next-0"]),
			],
		)
		.with_largest(SEQUENCE);
		let mut consumer = h.track.subscribe(None);

		let mut fill = h.stream().await;
		let mut tail = h.stream().await;
		let mut serve_tail = h.subscriber.clone();
		let mut serve_fill = h.subscriber.clone();
		let (tail, head) = futures::join!(serve_tail.recv_group(&mut tail), serve_fill.recv_fill(&mut fill));
		head.expect("fill");
		assert!(matches!(tail, Err(Error::Unsupported)), "{tail:?}");

		let mut next = h.stream().await;
		h.subscriber.clone().recv_group(&mut next).await.expect("next group");
		let (sequence, frames) = read_group(&mut consumer).await;
		assert_eq!(sequence, SEQUENCE + 1, "the headless group was dropped");
		assert_eq!(frames.len(), 1);
	}

	/// A subscription that requested no head was owed none, so a stream starting mid-group
	/// still stands alone.
	#[moq_clock::test]
	async fn a_mid_group_stream_without_a_fill_stands_alone() {
		let h = Harness::new(Fill::Done, vec![tail_stream(SEQUENCE, 2, &[b"tail-2"])]);
		let mut consumer = h.track.subscribe(None);

		let mut tail = h.stream().await;
		h.subscriber.clone().recv_group(&mut tail).await.expect("tail");
		let (sequence, frames) = read_group(&mut consumer).await;
		assert_eq!(sequence, SEQUENCE);
		assert_eq!(frames.len(), 1);
	}

	/// The subscription can end while the fetch stream is still writing, and that teardown
	/// cannot reach a producer the fetch stream still owns. The handoff has to settle it, or
	/// the head outlives the subscription unfinished and a consumer blocks on it. It is
	/// cancelled rather than finished, since nothing says the group ended there.
	#[moq_clock::test]
	async fn a_head_finishing_after_teardown_is_cancelled_not_installed() {
		let track = track::Producer::new(
			std::sync::Arc::new(crate::broadcast::Info::default()),
			"video",
			track::Info::default().with_timescale(Timescale::MICRO),
		);
		let mut producer = track.create_group(group::Info { sequence: SEQUENCE }).unwrap();
		producer.write_frame(timestamp(0), b"head-0".as_slice()).unwrap();
		let mut group = producer.consume();

		// `remove_subscribe` got there first.
		let mut fill = Fill::Done;
		fill.install(Fill::Ready {
			sequence: SEQUENCE,
			next: 1,
			producer,
		});
		assert!(matches!(fill, Fill::Done), "Done is terminal");

		assert!(matches!(group.read_frame().await, Err(Error::Cancel)));
	}

	/// A joining FETCH whose request fails while its stream is still writing has already
	/// dropped the headless tail, so a head landing afterwards is cancelled, not installed
	/// for a tail that is gone.
	#[moq_clock::test]
	async fn a_head_finishing_after_its_fill_failed_is_cancelled() {
		let track = track::Producer::new(
			std::sync::Arc::new(crate::broadcast::Info::default()),
			"video",
			track::Info::default().with_timescale(Timescale::MICRO),
		);
		let mut producer = track.create_group(group::Info { sequence: SEQUENCE }).unwrap();
		producer.write_frame(timestamp(0), b"head-0".as_slice()).unwrap();
		let mut group = producer.consume();

		let mut fill = Fill::Failed { sequence: SEQUENCE };
		fill.install(Fill::Ready {
			sequence: SEQUENCE,
			next: 1,
			producer,
		});
		assert!(
			matches!(fill, Fill::Failed { sequence: SEQUENCE }),
			"Failed is terminal"
		);

		assert!(matches!(group.read_frame().await, Err(Error::Cancel)));
	}

	/// A joining FETCH is not in PUBLISH_DONE's Stream Count, so the subscription can end
	/// cleanly after its request failed and its headless tail was dropped, with the head
	/// still writing. That head is cancelled when it lands, not published as the whole group.
	#[moq_clock::test]
	async fn a_clean_end_keeps_a_failed_fill_terminal() {
		let track = track::Producer::new(
			std::sync::Arc::new(crate::broadcast::Info::default()),
			"video",
			track::Info::default().with_timescale(Timescale::MICRO),
		);
		let mut producer = track.create_group(group::Info { sequence: SEQUENCE }).unwrap();
		producer.write_frame(timestamp(0), b"head-0".as_slice()).unwrap();
		let mut group = producer.consume();

		let mut fill = Fill::Failed { sequence: SEQUENCE };
		fill.end();
		assert!(
			matches!(fill, Fill::Failed { sequence: SEQUENCE }),
			"the end keeps it failed"
		);
		fill.install(Fill::Ready {
			sequence: SEQUENCE,
			next: 1,
			producer,
		});

		assert!(matches!(group.read_frame().await, Err(Error::Cancel)));
	}

	/// Widening updates replaced fills that failed to head this group and the next, and the
	/// current fill has settled. A mid-group tail at or below the last failed group is still
	/// headless, so it is dropped, while a whole group there stands alone.
	#[moq_clock::test]
	async fn a_headless_tail_after_its_fill_was_replaced_is_dropped() {
		let h = Harness::new(
			Fill::Done,
			vec![
				tail_stream(SEQUENCE, 2, &[b"tail-2"]),
				tail_stream(SEQUENCE + 1, 0, &[b"next-0"]),
			],
		);
		h.subscriber
			.state
			.lock()
			.subscribes
			.get_mut(&REQUEST)
			.unwrap()
			.headless_through = Some(SEQUENCE + 1);
		let mut consumer = h.track.subscribe(None);

		h.dispatch_refused().await;

		let mut next = h.stream().await;
		h.subscriber.clone().recv_group(&mut next).await.expect("next group");
		let (sequence, frames) = read_group(&mut consumer).await;
		assert_eq!(sequence, SEQUENCE + 1, "the headless group was dropped");
		assert_eq!(frames.len(), 1);
	}

	/// Leaving a track while its head waits for the tail cancels that group rather than
	/// finishing it: the group may still be open upstream, and a finished copy would cut it
	/// short for a rejoin.
	#[moq_clock::test]
	async fn an_unclaimed_head_is_cancelled_when_the_subscription_goes() {
		let h = Harness::new(
			Fill::Serving(Some(Timescale::MICRO)),
			vec![fill_stream(SEQUENCE, &[b"head-0"])],
		);
		let mut consumer = h.track.subscribe(None);

		let mut fill = h.stream().await;
		h.subscriber.clone().recv_fill(&mut fill).await.expect("fill");
		let mut group = consumer.recv_group().await.unwrap().unwrap();
		assert_eq!(group.sequence, SEQUENCE);

		h.subscriber.remove_subscribe(REQUEST).expect("subscribed");
		assert!(matches!(group.read_frame().await, Err(Error::Cancel)));
	}

	/// Closing locally cancels a filled head whose live tail never arrived.
	#[moq_clock::test]
	async fn an_unclaimed_head_is_cancelled_when_the_session_closes() {
		let h = Harness::new(
			Fill::Serving(Some(Timescale::MICRO)),
			vec![fill_stream(SEQUENCE, &[b"head-0"])],
		);
		let mut consumer = h.track.subscribe(None);
		let mut fill = h.stream().await;
		h.subscriber.clone().recv_fill(&mut fill).await.expect("fill");
		let mut group = consumer.recv_group().await.unwrap().unwrap();
		h.subscriber.abort(&Error::Session(SessionError::Cancel));
		let result = group.read_frame().await;
		assert!(
			matches!(result, Err(Error::Session(SessionError::Cancel))),
			"{result:?}"
		);
		assert!(
			matches!(*h.fill.read(), Fill::Done),
			"the cancelled head stays retained"
		);
	}

	/// A publisher that serves a head and then opens a whole group for the same sequence
	/// has contradicted its own fill. The model holds one producer per group, so the
	/// duplicate stream and unfinished head are refused without caching a cut group.
	#[moq_clock::test]
	async fn a_whole_group_for_a_headed_sequence_is_refused() {
		let h = Harness::new(
			Fill::Serving(Some(Timescale::MICRO)),
			vec![
				fill_stream(SEQUENCE, &[b"head-0", b"head-1"]),
				tail_stream(SEQUENCE, 0, &[b"again-0"]),
			],
		);
		let mut consumer = h.track.subscribe(None);

		let mut fill = h.stream().await;
		let mut again = h.stream().await;

		h.subscriber.clone().recv_fill(&mut fill).await.expect("fill");
		let mut head = consumer.recv_group().await.unwrap().unwrap();
		assert!(matches!(
			h.subscriber.clone().recv_group(&mut again).await,
			Err(Error::Unsupported)
		));

		assert!(matches!(head.read_frame().await, Err(Error::Cancel)));
	}

	/// A fill head parked waiting for its tail is still a live group producer. Dropping the
	/// session has to end it, or the consumer waits on a group nobody will ever write again.
	/// The guard is `State`'s own `Drop`, so it runs however the driver was torn down.
	#[moq_clock::test]
	async fn a_cancelled_session_aborts_a_waiting_fill_head() {
		let h = Harness::new(
			Fill::Serving(Some(Timescale::MICRO)),
			vec![fill_stream(SEQUENCE, &[b"head-0"])],
		);
		let mut consumer = h.track.subscribe(None);

		let mut fill = h.stream().await;
		h.subscriber.clone().recv_fill(&mut fill).await.expect("fill");

		// The head exists and is waiting for the tail that never comes.
		let mut group = consumer
			.recv_group()
			.await
			.expect("track aborted")
			.expect("track finished");

		drop(h);

		assert!(
			matches!(group.read_frame().await, Err(Error::Cancel)),
			"a waiting fill head must be cancelled, not left parked"
		);
	}

	#[moq_clock::test]
	async fn session_death_keeps_a_waiting_fill_heads_resume_position() {
		let h = Harness::new(
			Fill::Serving(Some(Timescale::MICRO)),
			vec![fill_stream(SEQUENCE, &[b"head-0"])],
		);
		let track = h.track.consume();
		let mut consumer = h.track.subscribe(None);
		let mut fill = h.stream().await;
		h.subscriber.clone().recv_fill(&mut fill).await.expect("fill");
		let mut group = consumer.recv_group().await.unwrap().expect("the fill head arrived");
		let resume = track.resume_position();
		assert_eq!(
			resume,
			Some(track::Position {
				group: SEQUENCE,
				frame: 1
			})
		);
		let err = Error::Session(crate::SessionError::App(7));
		h.subscriber.abort(&err);
		assert_eq!(track.resume_position(), resume);
		assert!(matches!(
			group.read_frame().await,
			Err(Error::Session(crate::SessionError::App(7)))
		));
	}

	/// The same contradiction as above, with the streams the other way round: the whole
	/// group lands before the fill has written its head. The model holds one producer per
	/// live sequence, so the fill loses the race to create it and gives up, rather than a
	/// second producer appearing and the objects being delivered twice.
	#[moq_clock::test]
	async fn a_whole_group_that_precedes_the_head_wins_the_sequence() {
		let h = Harness::new(
			Fill::Serving(Some(Timescale::MICRO)),
			vec![
				tail_stream(SEQUENCE, 0, &[b"whole-0"]),
				fill_stream(SEQUENCE, &[b"head-0", b"head-1"]),
			],
		);
		let mut consumer = h.track.subscribe(None);

		let mut whole = h.stream().await;
		let mut fill = h.stream().await;

		h.subscriber
			.clone()
			.recv_group(&mut whole)
			.await
			.expect("the whole group");
		h.subscriber
			.clone()
			.recv_fill(&mut fill)
			.await
			.expect("overlapping fill is drained without another producer");

		let (sequence, frames) = read_group(&mut consumer).await;
		assert_eq!(sequence, SEQUENCE);
		assert_eq!(frames.len(), 1, "the group is whatever one producer wrote, not both");
	}

	/// A standalone subgroup tail keeps its identity while plain readers see its payload.
	#[moq_clock::test]
	async fn a_tail_without_a_fill_is_preserved() {
		let h = Harness::new(Fill::Done, vec![tail_stream(SEQUENCE, 2, &[b"tail-2"])]);
		let mut consumer = h.track.subscribe(None);
		let mut tail = h.stream().await;
		h.subscriber.clone().recv_group(&mut tail).await.unwrap();
		assert_eq!(read_group(&mut consumer).await.1[0].1, b"tail-2");
	}

	/// Object IDs are absolute whatever FIRST_OBJECT says, and they start at 0, so a
	/// draft-18 stream that leaves the bit clear and starts at object 0 is the whole
	/// group. The publisher is out of spec on the bit, not missing a head.
	#[moq_clock::test]
	async fn a_clear_first_object_at_zero_delivers_the_group() {
		let bytes = draft18_subgroup(SEQUENCE, false, 0, &[b"whole", b"next"]);
		assert_eq!(
			u64::from(bytes[0]) & ietf::GroupFlags::FIRST_OBJECT_BIT,
			0,
			"the bit is clear"
		);

		let mut h = Harness::new(Fill::Done, vec![bytes]);
		h.subscriber.version = Version::Draft18;
		let mut consumer = h.track.subscribe(None);
		let mut stream = read_draft18(&h.session).await;

		h.subscriber.clone().recv_group(&mut stream).await.expect("the group");

		let (sequence, frames) = read_group(&mut consumer).await;
		assert_eq!(sequence, SEQUENCE);
		assert_eq!(frames.len(), 2);
		assert_eq!(frames[0].1, b"whole");
		assert_eq!(frames[1].1, b"next");
	}

	/// The END_OF_GROUP header bit only lets a FIN imply the group's end, so an explicit
	/// END_OF_GROUP status on the same stream ends the group there rather than failing it.
	#[moq_clock::test]
	async fn an_end_of_group_status_on_a_marked_stream_finishes_the_group() {
		use futures::FutureExt;

		let payloads: [&[u8]; 5] = [b"o0", b"o1", b"o2", b"o3", b"o4"];
		let bytes = end_marker(draft18_subgroup(SEQUENCE, true, 0, &payloads), END_OF_GROUP);
		let flags = ietf::GroupFlags::decode(u64::from(bytes[0])).unwrap();
		assert!(flags.has_end, "the header marks the group's end");

		let mut h = Harness::new(Fill::Done, vec![bytes]);
		h.subscriber.version = Version::Draft18;
		let mut consumer = h.track.subscribe(None);
		let mut stream = read_draft18(&h.session).await;

		h.subscriber.clone().recv_group(&mut stream).await.expect("the group");

		// The stream was read to its end, so everything is already delivered.
		let mut group = consumer.recv_group().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(group.sequence, SEQUENCE);
		for payload in payloads {
			let frame = group.read_frame().now_or_never().unwrap().unwrap().unwrap();
			assert_eq!(frame.payload.as_ref(), payload);
		}
		let end = group.read_frame().now_or_never().expect("the group is closed");
		assert!(matches!(end, Ok(None)), "the group is finished, not aborted: {end:?}");
	}

	/// FIRST_OBJECT and the starting ID are independent relay metadata.
	#[moq_clock::test]
	async fn a_clear_first_object_past_zero_is_preserved() {
		let partial = draft18_subgroup(SEQUENCE, false, 3, &[b"tail-3"]);
		let whole = draft18_subgroup(SEQUENCE + 1, true, 0, &[b"next"]);
		assert_eq!(u64::from(partial[0]) & ietf::GroupFlags::FIRST_OBJECT_BIT, 0);

		let mut h = Harness::new(Fill::Done, vec![partial, whole]);
		h.subscriber.version = Version::Draft18;
		let mut consumer = h.track.subscribe(None);

		let mut partial = read_draft18(&h.session).await;
		h.subscriber.clone().recv_group(&mut partial).await.unwrap();
		let (sequence, frames) = read_group(&mut consumer).await;
		assert_eq!(sequence, SEQUENCE);
		assert_eq!(frames[0].1, b"tail-3");

		let mut whole = read_draft18(&h.session).await;
		h.subscriber
			.clone()
			.recv_group(&mut whole)
			.await
			.expect("the next group");

		let (sequence, frames) = read_group(&mut consumer).await;
		assert_eq!(sequence, SEQUENCE + 1);
		assert_eq!(frames[0].1, b"next");
		assert!(h.session.log.closes().is_empty());
	}

	/// A received group whose first object rewinds past the group before it is accepted: a
	/// moq-transport peer may stamp arrival times, so its timed tracks are unchecked.
	#[moq_clock::test]
	async fn a_received_rewind_is_accepted() {
		use futures::FutureExt;

		let mut h = Harness::new(Fill::Done, vec![draft18_subgroup(SEQUENCE, true, 0, &[b"rewound"])]);
		h.subscriber.version = Version::Draft18;
		// Starts past the incoming group's first object, at `timestamp(0)`.
		let mut before = h.track.create_group((SEQUENCE - 1).into()).unwrap();
		before.write_frame(timestamp(4000), b"before".as_slice()).unwrap();
		before.finish().unwrap();

		let mut consumer = h.track.subscribe(None);
		assert_eq!(read_group(&mut consumer).await.0, SEQUENCE - 1);

		let mut stream = read_draft18(&h.session).await;
		h.subscriber.clone().recv_group(&mut stream).await.unwrap();
		// The stream was read to its end, so the group is already delivered.
		let mut group = consumer
			.recv_group()
			.now_or_never()
			.expect("delivered")
			.unwrap()
			.unwrap();
		assert_eq!(group.sequence, SEQUENCE);
		let frame = group.read_frame().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(frame.payload.as_ref(), b"rewound");
		assert!(h.track.closed().now_or_never().is_none());
	}

	/// A head that stops short of where the tail starts would leave a hole in the middle of
	/// the group. Refusing it aborts the incomplete group instead of caching a cut prefix.
	#[moq_clock::test]
	async fn a_head_that_misses_the_tail_is_refused() {
		let h = Harness::new(
			Fill::Serving(Some(Timescale::MICRO)),
			vec![
				fill_stream(SEQUENCE, &[b"head-0", b"head-1"]),
				tail_stream(SEQUENCE, 5, &[b"tail-5"]),
			],
		);
		let mut consumer = h.track.subscribe(None);

		let mut fill = h.stream().await;
		let mut tail = h.stream().await;

		h.subscriber.clone().recv_fill(&mut fill).await.expect("fill");
		let mut head = consumer.recv_group().await.unwrap().unwrap();
		assert!(matches!(
			h.subscriber.clone().recv_group(&mut tail).await,
			Err(Error::Unsupported)
		));

		assert!(matches!(head.read_frame().await, Err(Error::Unsupported)));
	}

	#[moq_clock::test]
	async fn receive_drain_keeps_bound_aliases_and_ends_unknown_alias_waits() {
		let h = Harness::new(Fill::Done, vec![]);
		let aliases = h.subscriber.state.lock().aliases.consume();
		let mut unknown = Box::pin(resolve_track_alias(&h.subscriber.runtime, aliases.clone(), ALIAS + 1));
		assert!(futures::poll!(unknown.as_mut()).is_pending());
		h.subscriber.settle_control();
		assert!(matches!(
			futures::FutureExt::now_or_never(unknown),
			Some(Err(Error::Cancel))
		));
		assert!(matches!(resolve_track_alias(&h.subscriber.runtime, aliases, ALIAS).await, Ok(id) if id == REQUEST));
	}

	#[moq_clock::test]
	async fn receive_drain_ends_a_fill_waiting_for_subscribe_ok() {
		let h = Harness::new(Fill::Requested, vec![fill_stream(SEQUENCE, &[b"head"])]);
		let mut stream = h.stream().await;
		let mut subscriber = h.subscriber.clone();
		let mut receiving = Box::pin(subscriber.recv_fill(&mut stream));
		assert!(futures::poll!(receiving.as_mut()).is_pending());
		h.subscriber.settle_control();
		assert!(matches!(
			futures::FutureExt::now_or_never(receiving),
			Some(Err(Error::Unsupported))
		));
	}

	#[moq_clock::test]
	async fn receive_drain_ends_a_tail_waiting_for_a_missing_fill() {
		let h = Harness::new(
			Fill::Serving(Some(Timescale::MICRO)),
			vec![tail_stream(SEQUENCE, 1, &[b"tail"])],
		);
		let mut stream = h.stream().await;
		let mut subscriber = h.subscriber.clone();
		let mut receiving = Box::pin(subscriber.recv_group(&mut stream));
		assert!(futures::poll!(receiving.as_mut()).is_pending());
		h.subscriber.settle_control();
		assert!(futures::FutureExt::now_or_never(receiving).is_some());
	}

	#[moq_clock::test]
	async fn receive_drain_preserves_a_buffered_fill_and_tail() {
		let h = Harness::new(
			Fill::Serving(Some(Timescale::MICRO)),
			vec![tail_stream(SEQUENCE, 1, &[b"tail"]), fill_stream(SEQUENCE, &[b"head"])],
		);
		let mut consumer = h.track.subscribe(None);
		let mut tail = h.stream().await;
		let mut head = h.stream().await;
		let mut tail_subscriber = h.subscriber.clone();
		let mut head_subscriber = h.subscriber.clone();
		let mut receiving = Box::pin(futures::future::join(
			tail_subscriber.recv_group(&mut tail),
			head_subscriber.recv_fill(&mut head),
		));
		assert!(futures::poll!(receiving.as_mut()).is_pending());
		h.subscriber.settle_control();
		let (tail, head) = receiving.await;
		tail.expect("buffered tail");
		head.expect("buffered head");
		let (_, frames) = read_group(&mut consumer).await;
		assert_eq!(
			frames.iter().map(|(_, bytes)| bytes.as_slice()).collect::<Vec<_>>(),
			vec![b"head".as_slice(), b"tail".as_slice()]
		);
	}

	#[moq_clock::test]
	async fn receive_drain_ends_a_group_fetch_waiting_for_fetch_ok() {
		let h = Harness::new(Fill::Done, vec![fill_stream(SEQUENCE, &[b"head"])]);
		h.subscriber
			.state
			.lock()
			.group_fetches
			.insert(REQUEST, kio::Producer::new(GroupFetch::Pending));
		let mut stream = h.stream().await;
		let mut subscriber = h.subscriber.clone();
		let mut receiving = Box::pin(subscriber.recv_fill(&mut stream));
		assert!(futures::poll!(receiving.as_mut()).is_pending());
		h.subscriber.settle_control();
		assert!(matches!(
			futures::FutureExt::now_or_never(receiving),
			Some(Err(Error::Unsupported))
		));
	}

	/// A fetch stream can arrive before SUBSCRIBE_OK commits the pending track. It waits
	/// for that response instead of looking up a producer that does not exist yet.
	#[moq_clock::test]
	async fn an_early_fill_waits_for_subscribe_ok() {
		let h = Harness::new(Fill::Requested, vec![fill_stream(SEQUENCE, &[b"head-0"])]);
		{
			let mut state = h.subscriber.state.lock();
			state.subscribes.get_mut(&REQUEST).unwrap().producer = None;
		}
		let mut fill = h.stream().await;
		let mut subscriber = h.subscriber.clone();
		let mut receiving = Box::pin(subscriber.recv_fill(&mut fill));
		assert!(futures::poll!(receiving.as_mut()).is_pending());
		{
			let mut state = h.subscriber.state.lock();
			state.subscribes.get_mut(&REQUEST).unwrap().producer = Some(h.track.clone());
		}
		*h.fill.write().ok().unwrap() = Fill::Serving(Some(Timescale::MICRO));
		receiving.await.expect("early fill");
		assert!(matches!(*h.fill.read(), Fill::Ready { .. }));
	}

	/// A fetch stream answering a subscription that asked for no fill duplicates a group the
	/// subscription itself is delivering, so it is refused rather than written.
	#[moq_clock::test]
	async fn an_unsolicited_fill_is_refused() {
		let h = Harness::new(Fill::Done, vec![fill_stream(SEQUENCE, &[b"head-0"])]);
		let mut fill = h.stream().await;

		assert!(matches!(
			h.subscriber.clone().recv_fill(&mut fill).await,
			Err(Error::Unsupported)
		));
	}

	const FETCH: RequestId = RequestId(3);

	/// A mid-group subscribe stream waits for the joining FETCH's head and stitches onto it,
	/// the same rendezvous a draft-22 fill uses. The FETCH is named by its own request id.
	#[moq_clock::test]
	async fn a_joining_fetch_stitches_a_mid_group_tail() {
		let h = Harness::new(
			Fill::Serving(Some(Timescale::MICRO)),
			vec![
				fill_stream_for(FETCH, &[(SEQUENCE, &[b"head-0", b"head-1"])]),
				tail_stream(SEQUENCE, 2, &[b"tail-2"]),
			],
		)
		.with_joining(FETCH);
		let mut consumer = h.track.subscribe(None);

		let mut fill = h.stream().await;
		let mut tail = h.stream().await;

		let mut serve_tail = h.subscriber.clone();
		let mut serve_fill = h.subscriber.clone();
		let (tail, head) = futures::join!(serve_tail.recv_group(&mut tail), serve_fill.recv_fill(&mut fill));
		head.expect("joining fetch");
		tail.expect("tail");

		let (sequence, frames) = read_group(&mut consumer).await;
		assert_eq!(sequence, SEQUENCE);
		assert_eq!(frames.len(), 3);
		assert_eq!(frames[0].1, b"head-0");
		assert_eq!(frames[1].1, b"head-1");
		assert_eq!(frames[2].1, b"tail-2");
	}

	/// A subscribe stream that starts at object 0 stands alone; the joining FETCH's answer
	/// is discarded rather than delivered twice.
	#[moq_clock::test]
	async fn a_whole_group_stream_discards_the_joining_fetch() {
		let h = Harness::new(
			Fill::Serving(Some(Timescale::MICRO)),
			vec![
				tail_stream(SEQUENCE, 0, &[b"whole-0"]),
				fill_stream_for(FETCH, &[(SEQUENCE, &[b"head-0", b"head-1"])]),
			],
		)
		.with_joining(FETCH);
		let mut consumer = h.track.subscribe(None);

		let mut whole = h.stream().await;
		let mut fill = h.stream().await;

		h.subscriber
			.clone()
			.recv_group(&mut whole)
			.await
			.expect("the whole group");
		assert!(
			h.subscriber.clone().recv_fill(&mut fill).await.is_err(),
			"the fetch cannot create a second producer for a live sequence"
		);

		let (sequence, frames) = read_group(&mut consumer).await;
		assert_eq!(sequence, SEQUENCE);
		assert_eq!(frames.len(), 1);
		assert_eq!(frames[0].1, b"whole-0");
	}

	/// A later object's Group ID field is a delta: 7 then 8 is 0, not 8.
	#[test]
	fn a_later_fetch_group_field_is_a_delta() {
		assert_eq!(resolve_fetch_group(Some(7), Some(0)).unwrap(), Some(8));
		assert_eq!(resolve_fetch_group(Some(7), Some(1)).unwrap(), Some(9));
	}

	/// A joining FETCH carries the one group its subscription joins, so a second group on
	/// it is refused rather than published as a head no tail continues.
	#[moq_clock::test]
	async fn a_joining_fetch_spanning_several_groups_is_refused() {
		let groups: &[(u64, &[&[u8]])] = &[(9, &[b"g9-0"]), (10, &[b"g10-0"])];
		let h = Harness::new(
			Fill::Serving(Some(Timescale::MICRO)),
			vec![fill_stream_for(FETCH, groups)],
		)
		.with_joining(FETCH);
		let mut fill = h.stream().await;
		assert!(matches!(
			h.subscriber.clone().recv_fill(&mut fill).await,
			Err(Error::Unsupported)
		));
	}

	/// The first object on a fetch stream has no prior object, so leaving any field to
	/// the prior one is a violation, not "the requested group, from its start".
	#[moq_clock::test]
	async fn a_group_fetch_refuses_a_first_object_that_inherits() {
		use ietf::FetchSubgroup::{Prior, Zero};
		let header = |group, object, subgroup, priority| ietf::FetchObject::Object {
			subgroup,
			group,
			object,
			priority,
			properties: None,
		};
		let headers = [
			// Both IDs omitted: "the requested group, from its start" is exactly the bug.
			header(None, None, Zero, Some(0)),
			header(Some(SEQUENCE), None, Zero, Some(0)),
			header(Some(SEQUENCE), Some(0), Prior, Some(0)),
			header(Some(SEQUENCE), Some(0), Zero, None),
		];

		for version in [VERSION] {
			for header in &headers {
				let mut buf = Vec::new();
				header
					.encode(&mut crate::coding::Encoder::new(&mut buf, version.into()), version)
					.unwrap();
				crate::coding::Encoder::new(&mut buf, version.into())
					.varint(1u64)
					.unwrap();
				buf.extend_from_slice(b"a");

				let mut run = GroupFetchRun::new(version, buf.to_vec()).await;
				let res = run.recv_objects().await;
				assert!(
					matches!(res, Err(Error::ProtocolViolation)),
					"{version} {header:?}: {res:?}"
				);
			}
		}
	}

	/// On a timed track every Normal object carries a Timestamp, so one without is
	/// malformed rather than stamped with its arrival time.
	#[moq_clock::test]
	async fn an_unstamped_object_on_a_timed_track_is_malformed() {
		let mut run = GroupFetchRun::new(VERSION, group_fetch_objects(SEQUENCE, 0, &[b"a"])).await;
		let track = track::Producer::new(
			std::sync::Arc::new(crate::broadcast::Info::default()),
			"timed",
			track::Info::default().with_timescale(Timescale::MICRO),
		);
		run.track = track.clone();
		let (slot, _) = run.slot(Some(Timescale::MICRO), None).await;
		let res = run.subscriber.recv_group_fetch(&mut run.stream, slot).await;
		assert!(matches!(res, Err(Error::MalformedTrack)), "{res:?}");
		assert!(matches!(
			futures::FutureExt::now_or_never(track.closed()),
			Some(Err(Error::MalformedTrack))
		));
	}

	/// FETCH_OK's End Location inside the group bounds the stream: one that runs past it
	/// fails the group instead of caching it. Before draft-22 the End Location also promises
	/// every object before it, so a stream that FINs short fails too. From draft-22 it is the
	/// range covered, and the objects missing before it do not exist (section 9.11).
	#[moq_clock::test]
	async fn a_group_fetch_stays_within_its_end_location() {
		const END: u64 = 3;
		for version in [Version::Draft18, VERSION] {
			let short = Filter::is_draft20(version);
			for (count, complete) in [(2, short), (3, true), (4, false)] {
				let payloads: Vec<&[u8]> = [b"a", b"b", b"c", b"d"][..count].iter().map(|p| &p[..]).collect();
				let mut run = GroupFetchRun::new(version, group_fetch_objects(SEQUENCE, 0, &payloads)).await;

				let (slot, fetch) = run.slot(None, Some(END)).await;
				let res = run.subscriber.recv_group_fetch(&mut run.stream, slot).await;
				assert_eq!(res.is_ok(), complete, "{version} {count} objects: {res:?}");
				assert!(futures::FutureExt::now_or_never(run.track.closed()).is_none());

				let mut read = 0;
				let end = match fetch.await {
					Ok(mut consumer) => loop {
						match consumer.read_frame().await {
							Ok(Some(_)) => read += 1,
							Ok(None) => break Ok(read),
							Err(err) => break Err(err),
						}
					},
					Err(err) => Err(err),
				};
				match complete {
					true => assert_eq!(end.expect("a complete group"), count as u64, "{version}"),
					false => assert!(end.is_err(), "{version} {count} objects: the group must fail, not end"),
				}
			}
		}
	}

	/// A stream that FINs before the group its End Location bounds, with no objects or after
	/// an earlier group, is short before draft-22. From draft-22 the bounded group is refused
	/// for now, since objects past the bound may still be written.
	#[moq_clock::test]
	async fn a_range_fetch_must_reach_its_bounded_group() {
		let last = SEQUENCE + 2;
		for version in [Version::Draft18, VERSION] {
			for objects in [Vec::new(), group_fetch_objects(SEQUENCE, 0, &[b"a"])] {
				let empty = objects.is_empty();
				let mut run = GroupFetchRun::new(version, objects).await;
				let dynamic = run.track.requests();
				let consumer = run.track.consume();
				run.track.start_at(last + 1).unwrap();
				run.track.create_group((last + 1).into()).unwrap().finish().unwrap();
				let mut reading = consumer
					.subscribe(Some(
						track::Subscription::default()
							.with_range(SEQUENCE..last + 1)
							.with_order(crate::track::Order::Ascending)
							.with_max_delay(None),
					))
					.await
					.unwrap();
				assert!(futures::FutureExt::now_or_never(reading.recv_group()).is_none());
				let fill = dynamic.requested_fill().await.unwrap();
				let bounded = consumer.fetch_group(last, None);
				let slot = kio::Producer::new(GroupFetch::Ready(RangeFetch {
					fill: std::sync::Arc::new(fill),
					timescale: None,
					first: SEQUENCE,
					last,
					start: 0,
					end: Some(2),
				}));
				let res = run.subscriber.recv_group_fetch(&mut run.stream, slot).await;
				match Filter::is_draft20(version) {
					false => assert!(
						matches!(res, Err(Error::ProtocolViolation)),
						"{version} empty={empty}: {res:?}"
					),
					true => {
						assert!(res.is_ok(), "{version} empty={empty}: {res:?}");
						let bounded = moq_clock::timeout(Duration::from_secs(1), bounded)
							.await
							.expect("refused");
						assert!(
							matches!(bounded, Err(Error::NotFound)),
							"{version} empty={empty}: {:?}",
							bounded.err()
						);
					}
				}
			}
		}
	}

	/// One Object published as a datagram, as a fetch stream may carry it.
	fn datagram_object(sequence: u64) -> Vec<u8> {
		let mut properties = Vec::new();
		let w = &mut Encoder::new(&mut properties, VERSION.into());
		ietf::encode_object_time(w, timestamp(0), Timescale::MICRO).unwrap();
		let mut buf = Vec::new();
		ietf::FetchObject::Object {
			subgroup: ietf::FetchSubgroup::Datagram,
			group: Some(sequence),
			object: Some(0),
			priority: Some(0),
			properties: Some(properties),
		}
		.encode(&mut crate::coding::Encoder::new(&mut buf, VERSION.into()), VERSION)
		.unwrap();
		crate::coding::Encoder::new(&mut buf, VERSION.into()).varint(1).unwrap();
		buf.extend_from_slice(b"d");
		buf
	}

	/// A datagram group is never fetchable, so a group fetch answered with one names it
	/// dropped as not fetchable, caching nothing, and reads on past its payload.
	#[moq_clock::test]
	async fn a_group_fetch_drops_a_datagram_group() {
		let mut run = GroupFetchRun::new(VERSION, datagram_object(SEQUENCE)).await;
		let (slot, fetch) = run.slot(None, None).await;

		let res = run.subscriber.recv_group_fetch(&mut run.stream, slot).await;
		assert!(res.is_ok(), "{res:?}");
		assert!(matches!(fetch.await, Err(Error::NotFetchable)));
		assert!(run.track.consume().peek_group(SEQUENCE).is_none(), "nothing is cached");
	}

	/// A third-party publisher may answer a range even though ours refuses it. The subscriber
	/// caches the supplied groups and reports the gaps between them as misses.
	#[moq_clock::test]
	async fn a_range_fetch_caches_the_groups_with_the_groups_between_dropped() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			let mut objects = group_fetch_objects(4, 0, &[b"4-0", b"4-1"]);
			// Later Group IDs are deltas, so 1 advances from group 4 to group 6.
			objects.extend(group_fetch_objects(1, 0, &[b"6-0", b"6-1"]));
			let mut run = GroupFetchRun::new(version, objects).await;
			let dynamic = run.track.requests();
			let consumer = run.track.consume();
			run.track.start_at(7).unwrap();
			run.track.create_group(7u64.into()).unwrap().finish().unwrap();
			let mut reading = consumer
				.subscribe(Some(
					track::Subscription::default()
						.with_range(3..7)
						.with_order(crate::track::Order::Ascending)
						.with_max_delay(None),
				))
				.await
				.unwrap();
			assert!(futures::FutureExt::now_or_never(reading.recv_group()).is_none());
			let fill = dynamic.requested_fill().await.unwrap();
			assert_eq!(
				(fill.start().group, fill.end()),
				(3, 7),
				"{version}: one fill for the run"
			);
			// Readers of the missing groups join that fill.
			let missing = [3, 5].map(|sequence| consumer.fetch_group(sequence, None));
			let slot = kio::Producer::new(GroupFetch::Ready(RangeFetch {
				fill: std::sync::Arc::new(fill),
				timescale: None,
				first: 3,
				last: 6,
				start: 0,
				end: None,
			}));
			let res = moq_clock::timeout(
				Duration::from_secs(1),
				run.subscriber.recv_group_fetch(&mut run.stream, slot),
			)
			.await
			.expect("recv");
			assert!(res.is_ok(), "{version}: {res:?}");
			assert_eq!(consumer.cached_groups(0..7), [4, 6], "{version}");
			for sequence in [4, 6] {
				let mut group = consumer.fetch_group(sequence, None).await.unwrap();
				for object in 0..2 {
					let frame = group.read_frame().await.unwrap().unwrap();
					assert_eq!(
						frame.payload.as_ref(),
						format!("{sequence}-{object}").as_bytes(),
						"{version}"
					);
				}
				assert!(group.read_frame().await.unwrap().is_none(), "{version}");
			}
			for fetched in missing {
				let fetched = moq_clock::timeout(Duration::from_secs(1), fetched)
					.await
					.expect("dropped");
				assert!(
					matches!(fetched, Err(Error::NotFound)),
					"{version}: {:?}",
					fetched.err()
				);
			}
		}
	}

	/// A fill answered with a datagram Object is refused the same way. Only the fill goes:
	/// the subscription keeps delivering the groups after it.
	#[moq_clock::test]
	async fn a_datagram_fill_fails_only_the_fill() {
		let mut fill = Vec::new();
		crate::coding::Encoder::new(&mut fill, VERSION.into())
			.varint(ietf::FetchHeader::TYPE)
			.unwrap();
		ietf::FetchHeader { request_id: REQUEST }
			.encode(&mut crate::coding::Encoder::new(&mut fill, VERSION.into()), VERSION)
			.unwrap();
		fill.extend(datagram_object(SEQUENCE));

		let h = Harness::new(
			Fill::Serving(Some(Timescale::MICRO)),
			vec![fill, tail_stream(SEQUENCE + 1, 0, &[b"next"])],
		);
		let mut consumer = h.track.subscribe(None);
		let mut fill = h.stream().await;
		let mut tail = h.stream().await;

		assert!(matches!(
			h.subscriber.clone().recv_fill(&mut fill).await,
			Err(Error::NotFetchable)
		));
		h.subscriber
			.clone()
			.recv_group(&mut tail)
			.await
			.expect("the subscription carries on");

		let (sequence, frames) = read_group(&mut consumer).await;
		assert_eq!(sequence, SEQUENCE + 1);
		assert_eq!(frames.len(), 1);
		assert_eq!(frames[0].1, b"next");
	}

	/// A group fetch's objects after the FETCH_HEADER: the first one names the group and
	/// `start`, and every later one is the next object.
	fn group_fetch_objects(sequence: u64, start: u64, payloads: &[&[u8]]) -> Vec<u8> {
		let mut buf = Vec::new();
		for (index, payload) in payloads.iter().enumerate() {
			let first = index == 0;
			ietf::FetchObject::Object {
				subgroup: ietf::FetchSubgroup::Zero,
				group: first.then_some(sequence),
				object: first.then_some(start),
				priority: first.then_some(0),
				properties: None,
			}
			.encode(&mut crate::coding::Encoder::new(&mut buf, VERSION.into()), VERSION)
			.unwrap();
			crate::coding::Encoder::new(&mut buf, VERSION.into())
				.varint(payload.len() as u64)
				.unwrap();
			buf.extend_from_slice(payload);
		}
		buf.to_vec()
	}

	/// A group aborted under a stalled fetch stream, as by eviction, ends the run with its
	/// error instead of leaving the run's readers waiting on the stream.
	#[moq_clock::test]
	async fn a_group_aborted_under_a_stalled_fetch_ends_the_run() {
		let mut run = GroupFetchRun::stalled(VERSION, group_fetch_objects(SEQUENCE, 0, &[b"0"])).await;
		let (slot, _fetch) = run.slot(None, None).await;
		let mut receiving = run.subscriber.clone();
		let mut receive = std::pin::pin!(receiving.recv_group_fetch(&mut run.stream, slot.clone()));
		assert!(futures::poll!(&mut receive).is_pending());
		let group = match &*slot.read() {
			GroupFetch::Receiving(Some(group)) => group.clone(),
			_ => panic!("the group is being written"),
		};
		group.abort_open(Error::Evicted);
		assert!(matches!(receive.await, Err(Error::Evicted)));
	}

	#[moq_clock::test]
	async fn internal_group_fetch_setup_cancellation_is_cancel() {
		let mut run = GroupFetchRun::new(VERSION, Vec::new()).await;
		let slot = kio::Producer::new(GroupFetch::Pending);
		let entry = GroupFetchEntry::new(&run.subscriber.state, RequestId(23), slot.clone());
		let mut receiving = run.subscriber.clone();
		let mut receive = std::pin::pin!(receiving.recv_group_fetch(&mut run.stream, slot));
		assert!(futures::poll!(&mut receive).is_pending());
		drop(entry);
		assert!(matches!(receive.await, Err(Error::Cancel)));
	}

	/// A subscriber reading one scripted group fetch stream, already past its header.
	struct GroupFetchRun {
		subscriber: Subscriber,
		stream: Reader<crate::transport::boxed::RecvStream, Version>,
		track: track::Producer,
		_tasks: (Tasks, TaskSet),
	}

	impl GroupFetchRun {
		async fn new(version: Version, objects: Vec<u8>) -> Self {
			Self::with_session(version, ScriptedSession::per_stream_eof(vec![objects])).await
		}

		/// A stream that goes quiet after `objects` rather than ending.
		async fn stalled(version: Version, objects: Vec<u8>) -> Self {
			Self::with_session(version, ScriptedSession::per_stream(vec![objects])).await
		}

		async fn with_session(version: Version, session: ScriptedSession) -> Self {
			let tasks = TaskSet::new();
			let subscriber = Subscriber::new(
				crate::time::sim(),
				crate::driver::Transport::new(session.clone()),
				crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce(),
				Control::new(false),
				None,
				peer::PeerSetup::default(),
				crate::Hop::new(1).unwrap(),
				None,
				version,
				tasks.0.clone(),
				Default::default(),
			);
			let (_, recv) = session.open_bi(Default::default()).await.unwrap();
			let track = track::Producer::new(
				std::sync::Arc::new(crate::broadcast::Info::default()),
				"video",
				track::Info::default().with_timescale(None),
			);

			Self {
				subscriber,
				stream: Reader::new(crate::transport::boxed::RecvStream::new(recv), version),
				track,
				_tasks: tasks,
			}
		}

		/// Queue the cache miss through the same request state as a wire FETCH, answered
		/// through group `SEQUENCE`'s object `end` when set. The fetch resolves once the stream
		/// writes the group.
		async fn slot(
			&self,
			timescale: Option<Timescale>,
			end: Option<u64>,
		) -> (
			kio::Producer<GroupFetch>,
			std::pin::Pin<Box<kio::Pending<track::Fetching>>>,
		) {
			let dynamic = self.track.requests();
			let mut fetch = Box::pin(self.track.consume().fetch_group(SEQUENCE, None));
			assert!(futures::poll!(fetch.as_mut()).is_pending());
			let fill = dynamic.requested_fill().await.expect("group request");
			let slot = kio::Producer::new(GroupFetch::Ready(RangeFetch {
				fill: std::sync::Arc::new(fill),
				timescale,
				first: SEQUENCE,
				last: SEQUENCE,
				start: 0,
				end,
			}));
			(slot, fetch)
		}

		/// Decode the stream as the answer for group `SEQUENCE`.
		async fn recv_objects(&mut self) -> Result<(), Error> {
			let (slot, _fetch) = self.slot(None, None).await;
			self.subscriber.recv_group_fetch(&mut self.stream, slot).await
		}

		/// Decode the stream as the answer for group `SEQUENCE` through the exclusive object
		/// `end`, in `timescale`, and read back the group it cached.
		async fn recv_cached(
			&mut self,
			end: Option<u64>,
			timescale: Option<Timescale>,
		) -> Result<group::Consumer, Error> {
			let (slot, fetch) = self.slot(timescale, end).await;
			self.subscriber.recv_group_fetch(&mut self.stream, slot).await?;
			fetch.await
		}
	}
}

/// A scripted peer answering SUBSCRIBE then FETCH, so the join is spelled on the wire.
#[cfg(test)]
mod joining_fetch_tests {
	use super::*;
	use crate::{
		coding::{Encode as _, Encoder},
		lite::test_transport::ScriptedSession,
		model::ProduceTest,
		util::{TaskSet, Tasks},
	};
	use futures::FutureExt;

	const JOINING_DRAFTS: [Version; 1] = [Version::Draft18];

	async fn settle() {
		moq_clock::sleep(Duration::from_millis(1)).await;
	}

	fn message_bytes<'m, M: Message<'m>>(id: u64, msg: &M, version: Version) -> Vec<u8> {
		let mut buf = Vec::new();
		crate::coding::Encoder::new(&mut buf, version.into())
			.varint(id)
			.unwrap();
		msg.encode(&mut crate::coding::Encoder::new(&mut buf, version.into()), version)
			.unwrap();
		buf
	}

	fn subscribe_ok(version: Version, largest: Option<ietf::Location>) -> Vec<u8> {
		message_bytes(
			ietf::SubscribeOk::ID,
			&ietf::SubscribeOk {
				track_alias: 7,
				largest,
				properties: Default::default(),
			},
			version,
		)
	}

	fn fetch_ok(version: Version) -> Vec<u8> {
		message_bytes(
			ietf::FetchOk::ID,
			&ietf::FetchOk {
				end_of_track: false,
				end_location: ietf::Location { group: 4, object: 2 },
				properties: Default::default(),
			},
			version,
		)
	}

	fn fetch_error(version: Version) -> Vec<u8> {
		message_bytes(
			ietf::RequestError::ID,
			&ietf::RequestError {
				error_code: request::NOT_SUPPORTED,
				reason_phrase: "refused".into(),
				retry_interval: 0,
			},
			version,
		)
	}

	fn decode_messages(log: &crate::lite::test_transport::Log, version: Version) -> Vec<(u64, bytes::Bytes)> {
		use crate::coding::Decode;

		let writes = log.writes.lock().unwrap().clone();
		let mut buf = Decoder::new(&writes, version.into());
		let mut messages = Vec::new();
		while !buf.is_empty() {
			let Ok(type_id) = buf.varint() else {
				break;
			};
			let Ok(body) = ietf::Body::decode(&mut buf, version) else {
				break;
			};
			messages.push((type_id, body.0));
		}
		messages
	}

	struct JoinRun {
		subscriber: Subscriber,
		session: ScriptedSession,
		_hold: (
			crate::broadcast::Producer,
			track::Consumer,
			kio::Pending<track::Subscribing>,
		),
		_tasks: (Tasks, TaskSet),
		serving: moq_clock::sim::JoinHandle<()>,
	}

	impl JoinRun {
		async fn start(version: Version, start: Option<u64>, ok: Vec<u8>, fetch: Vec<u8>) -> Self {
			Self::start_with_control(version, start, [ok, fetch], Control::new(false)).await
		}

		async fn start_with_control(
			version: Version,
			start: Option<u64>,
			scripts: [Vec<u8>; 2],
			control: Control,
		) -> Self {
			let session = ScriptedSession::per_stream(Vec::from(scripts));
			let (tasks, _task_set) = crate::util::TaskSet::new();
			let subscriber = Subscriber::new(
				crate::time::sim(),
				crate::driver::Transport::new(session.clone()),
				crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce(),
				control,
				None,
				peer::PeerSetup::declared(peer::Peer::default()),
				crate::Hop::new(1).unwrap(),
				None,
				version,
				tasks.clone(),
				Default::default(),
			);

			let producer = crate::broadcast::Info::default().produce();
			let mut dynamic = producer.requests();
			let consumer = producer.consume();
			let track = consumer.track("video").unwrap();
			let subscription = match start {
				None => track.subscribe(None),
				Some(start) => track.subscribe(track::Subscription::default().with_start(start)),
			};
			let request = dynamic.requested_track().await.expect("no track requested");

			let mut serving_subscriber = subscriber.clone();
			let serving = moq_clock::sim::spawn(async move {
				serving_subscriber
					.run_subscribe(Path::from_static("broadcast"), dynamic, None, request)
					.await;
			});

			settle().await;

			Self {
				subscriber,
				session,
				_hold: (producer, track, subscription),
				_tasks: (tasks, _task_set),
				serving,
			}
		}

		fn fill_outstanding(&self) -> bool {
			let state = self.subscriber.state.lock();
			let track = state
				.subscribes
				.values()
				.next()
				.expect("the subscription is registered");
			track.fill.read().outstanding()
		}

		fn fill_headless(&self, sequence: u64) -> bool {
			let state = self.subscriber.state.lock();
			let track = state
				.subscribes
				.values()
				.next()
				.expect("the subscription is registered");
			track.fill.read().headless(sequence)
		}
	}

	impl Drop for JoinRun {
		fn drop(&mut self) {
			self.serving.abort();
		}
	}

	fn pending_join(session: Transport, version: Version) -> (Subscriber, track::Producer, TaskSet) {
		let (tasks, task_set) = TaskSet::new();
		let subscriber = Subscriber::new(
			crate::time::sim(),
			session,
			crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce(),
			Control::new(false),
			None,
			peer::PeerSetup::default(),
			crate::Hop::new(1).unwrap(),
			None,
			version,
			tasks,
			Default::default(),
		);
		let track = track::Producer::new(
			std::sync::Arc::new(crate::broadcast::Info::default()),
			"video",
			track::Info::timed(),
		);
		subscriber.state.lock().subscribes.insert(
			RequestId(99),
			TrackState::new(
				track.clone(),
				Path::from_static("broadcast"),
				kio::Producer::new(Fill::Requested),
			),
		);
		(subscriber, track, task_set)
	}

	/// The parent keeps its child FETCH cancellation moving while its own FIN awaits ACK.
	async fn revoked_child_fetch(accepted: bool) {
		for version in [Version::Draft22] {
			let session = ScriptedSession::per_stream(vec![
				subscribe_ok(version, None),
				if accepted { fetch_ok(version) } else { Vec::new() },
			])
			.with_unacked_fin();
			let log = session.log.clone();
			let (tasks, _task_set) = TaskSet::new();
			let mut subscriber = Subscriber::new(
				crate::time::sim(),
				crate::driver::Transport::new(session.clone()),
				crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce(),
				Control::new(false),
				None,
				peer::PeerSetup::declared(peer::Peer::default()),
				crate::Hop::new(1).unwrap(),
				None,
				version,
				tasks,
				Default::default(),
			);
			let auth = subscriber.auth.clone();
			let broadcast = crate::broadcast::Info::default().produce();
			let mut dynamic = broadcast.requests();
			let consumer = broadcast.consume();
			let track = consumer.track("video").unwrap();
			let mut subscribing = Box::pin(track.subscribe(None));
			let request = dynamic.requested_track().await.unwrap();
			let mut serving =
				Box::pin(subscriber.run_subscribe(Path::from_static("broadcast"), dynamic, None, request));
			assert!(futures::poll!(serving.as_mut()).is_pending());
			let received = (&mut subscribing).now_or_never().expect("SUBSCRIBE_OK").unwrap();
			let mut fetch = Box::pin(track.fetch_group(4, None));
			assert!(futures::poll!(fetch.as_mut()).is_pending());
			assert!(futures::poll!(serving.as_mut()).is_pending());
			let requests = decode_messages(&log, version)
				.iter()
				.filter(|(id, _)| *id == ietf::Fetch::ID)
				.count();
			assert!(requests > 0, "cache-miss FETCH was sent");
			// A group is accepted only once its first object arrives, so FETCH_OK alone
			// leaves the fetch waiting on the stream.
			assert!(futures::poll!(fetch.as_mut()).is_pending());
			auth.handle().authorize(&crate::auth::Grant::default());
			assert!(
				futures::poll!(serving.as_mut()).is_pending(),
				"FIN stays unacknowledged"
			);
			assert!(matches!(
				futures::poll!(fetch.as_mut()),
				Poll::Ready(Err(Error::Unauthorized))
			));
			let stops = log.stops().len();
			// One stop belongs to SUBSCRIBE, another to the cache-miss FETCH.
			assert!(stops >= 2, "{version}: only {stops} streams cancelled before ACK");
			drop(received);
		}
	}

	#[moq_clock::test]
	async fn revocation_cancels_a_group_fetch_before_the_subscribe_fin_is_acked() {
		revoked_child_fetch(false).await;
	}

	#[moq_clock::test]
	async fn revocation_cancels_an_answered_fetch_before_the_subscribe_fin_is_acked() {
		revoked_child_fetch(true).await;
	}

	/// From draft 18 the joining FETCH's request stays open after FETCH_OK. Its failure while
	/// the head is still being written settles the fill, so a mid-group tail is not parked
	/// on a head that may never finish.
	#[moq_clock::test]
	async fn a_request_failing_mid_head_settles_the_fill() {
		let version = Version::Draft18;
		let session = ScriptedSession::per_stream_reset(vec![fetch_ok(version)]);
		let (subscriber, track, _tasks) = pending_join(crate::driver::Transport::new(session.clone()), version);
		let fill = subscriber.state.lock().subscribes[&RequestId(99)].fill.clone();
		// A fetch stream is writing the head.
		if let Ok(mut state) = fill.write() {
			*state = Fill::Active;
		}
		let mut joining = subscriber.start_joining_fetch(RequestId(99), &track).unwrap();
		assert!(
			kio::wait(|waiter| joining.poll(waiter)).await.is_err(),
			"{version}: the reset request fails"
		);
		assert!(matches!(*fill.read(), Fill::Done), "{version}: the head is released");
	}

	#[moq_clock::test]
	async fn invalid_joining_open_is_refused() {
		for version in JOINING_DRAFTS {
			for retired in [false, true] {
				let session = ScriptedSession::new(Vec::new()).without_credit();
				let log = session.log.clone();
				let (mut subscriber, track, _tasks) =
					pending_join(crate::driver::Transport::new(session.clone()), version);
				let route = kio::Producer::new(());
				subscriber.validity = crate::model::broadcast::RequestValidity::token(Some(route.consume()));
				let mut joining = subscriber.start_joining_fetch(RequestId(99), &track).unwrap();
				assert!(joining.poll(&kio::Waiter::noop()).is_pending());
				if retired {
					drop(route);
				} else {
					// Grant loss is enforced by the owning subscription's existing gate.
					let _ = track.clone().abort(Error::Unauthorized);
				}
				assert!(
					joining.poll(&kio::Waiter::noop()).is_ready(),
					"{version}: invalid joining open stayed alive"
				);
				assert_eq!(subscriber.control.next_request_id(), RequestId(1));
				assert_eq!(log.bi_opens(), 0);
			}
		}
	}

	#[moq_clock::test]
	async fn a_joining_fetch_waits_for_stream_credit() {
		for version in JOINING_DRAFTS {
			let session = ScriptedSession::new(Vec::new()).without_credit();
			let (subscriber, track, _tasks) = pending_join(crate::driver::Transport::new(session.clone()), version);
			let mut joining = subscriber.start_joining_fetch(RequestId(99), &track).unwrap();
			assert!(
				joining.poll(&kio::Waiter::noop()).is_pending(),
				"{version}: a subscription owns its joining FETCH"
			);
			assert_eq!(session.log.bi_opens(), 0);
			session.grant_credit();
			assert!(joining.poll(&kio::Waiter::noop()).is_pending());
			assert_eq!(session.log.bi_opens(), 1, "{version}: credit releases the FETCH");
		}
	}

	/// A committed joining FETCH the peer's send window holds back part-way is reset once
	/// its subscription lets go, rather than waiting for credit to finish the header.
	#[moq_clock::test]
	async fn a_joining_fetch_stalled_on_credit_resets_when_cancelled() {
		for version in JOINING_DRAFTS {
			let session = ScriptedSession::new(Vec::new()).with_send_window(1);
			let log = session.log.clone();
			let (subscriber, track, mut tasks) = pending_join(crate::driver::Transport::new(session.clone()), version);
			let mut joining = subscriber.start_joining_fetch(RequestId(99), &track).unwrap();
			assert!(joining.poll(&kio::Waiter::noop()).is_pending());
			assert_eq!(log.writes.lock().unwrap().len(), 1, "{version}: stalled mid-message");

			// The session's tasks now own the request.
			drop(joining);
			let _ = tasks.poll(&kio::Waiter::noop());
			assert_eq!(log.resets(), vec![crate::ietf::error::CANCELLED], "{version}");
			assert!(
				!log.trail()
					.iter()
					.any(|(_, sent)| matches!(sent, crate::lite::test_transport::Sent::Finish)),
				"{version}: finished a truncated request"
			);
		}
	}

	/// Abandoning a blocked open leaves the next sequential ID available to unrelated work.
	#[moq_clock::test]
	async fn cancelling_a_joining_fetch_open_spends_no_id() {
		for version in JOINING_DRAFTS {
			let gate = kio::Producer::new(false);
			let session = ScriptedSession::gated_open(Vec::new(), gate.consume());
			let log = session.log.clone();
			let (subscriber, track, mut tasks) = pending_join(crate::driver::Transport::new(session.clone()), version);
			let mut joining = subscriber.start_joining_fetch(RequestId(99), &track).unwrap();
			assert!(joining.poll(&kio::Waiter::noop()).is_pending());
			drop(joining);
			*gate.write().ok().expect("gate is open") = true;
			let _ = tasks.poll(&kio::Waiter::noop());
			assert_eq!(subscriber.control.next_request_id(), RequestId(1));
			assert_eq!(log.bi_opens(), 0, "{version}: the abandoned open is gone");
		}
	}

	/// Every draft-18 live join is Largest Object on the SUBSCRIBE and a relative
	/// joining FETCH at offset 0 that names the subscribe's request id.
	#[moq_clock::test]
	async fn a_live_join_is_spelled_as_largest_object_plus_relative_fetch() {
		let largest = Some(ietf::Location { group: 4, object: 1 });
		for version in JOINING_DRAFTS {
			let run = JoinRun::start(version, None, subscribe_ok(version, largest), fetch_ok(version)).await;

			let messages = decode_messages(&run.session.log, version);
			let subscribe = messages
				.iter()
				.find(|(id, _)| *id == ietf::Subscribe::ID)
				.expect("SUBSCRIBE");
			let body = &subscribe.1;
			let msg = crate::coding::decode_from(&mut &body[..], version, ietf::Subscribe::decode_msg).unwrap();
			assert_eq!(msg.filter, Filter::NextObject, "{version}");
			assert!(msg.fill.is_none(), "{version}");

			let fetch = messages.iter().find(|(id, _)| *id == ietf::Fetch::ID).expect("FETCH");
			let body = &fetch.1;
			let msg = crate::coding::decode_from(&mut &body[..], version, ietf::Fetch::decode_msg).unwrap();
			assert_eq!(
				msg.fetch_type,
				FetchType::RelativeJoining {
					subscriber_request_id: RequestId(1),
					group_offset: 0,
				},
				"{version}"
			);
		}
	}

	/// A start below, at, or past the Largest group joins that group alone: a group below it
	/// is left to the copy's fills.
	#[moq_clock::test]
	async fn a_join_from_any_start_fetches_only_the_largest_group() {
		let largest = Some(ietf::Location { group: 9, object: 0 });
		for version in JOINING_DRAFTS {
			for start in [7, 9, 11] {
				let run = JoinRun::start(version, Some(start), subscribe_ok(version, largest), fetch_ok(version)).await;

				let messages = decode_messages(&run.session.log, version);
				let fetch = messages.iter().find(|(id, _)| *id == ietf::Fetch::ID).expect("FETCH");
				let body = &fetch.1;
				let msg = crate::coding::decode_from(&mut &body[..], version, ietf::Fetch::decode_msg).unwrap();
				assert_eq!(
					msg.fetch_type,
					FetchType::RelativeJoining {
						subscriber_request_id: RequestId(1),
						group_offset: 0,
					},
					"{version} from {start}"
				);
			}
		}
	}

	/// A publisher that serves no FETCH refuses the joining FETCH. That costs the head of
	/// the Largest group and nothing more, as a failed draft-22 fill does: the fill settles,
	/// so a stream starting mid-group no longer waits on it, and the subscription keeps
	/// delivering whole groups.
	#[moq_clock::test]
	async fn a_refused_fetch_joins_at_the_next_whole_group() {
		let largest = Some(ietf::Location { group: 4, object: 1 });
		for version in JOINING_DRAFTS {
			let mut run = JoinRun::start(version, None, subscribe_ok(version, largest), fetch_error(version)).await;
			let mut received = (&mut run._hold.2)
				.now_or_never()
				.expect("SUBSCRIBE_OK accepted the track")
				.unwrap();
			assert!(
				received.finished().now_or_never().is_none(),
				"{version}: the track is still live"
			);
			assert!(!run.fill_outstanding(), "{version}: no head is coming");
			assert!(run.fill_headless(4), "{version}: the Largest Group's tail is dropped");
			assert!(run.session.log.closes().is_empty(), "{version}: the session stays up");
		}
	}

	/// A malformed answer to the joining FETCH is not a refusal: the peer broke the
	/// protocol, so the session closes.
	#[moq_clock::test]
	async fn a_malformed_fetch_answer_closes_the_session() {
		let largest = Some(ietf::Location { group: 4, object: 1 });
		for version in JOINING_DRAFTS {
			// A SUBSCRIBE_OK where FETCH_OK or a refusal belongs.
			let run = JoinRun::start(
				version,
				None,
				subscribe_ok(version, largest),
				subscribe_ok(version, largest),
			)
			.await;
			assert!(
				!run.session.log.closes().is_empty(),
				"{version}: an unexpected answer closes the session"
			);
		}
	}

	/// A joining FETCH is not one of the subscription's counted streams, so a clean
	/// PUBLISH_DONE can settle while its head is still being written. The head is then the
	/// whole group, as it is when it lands first, rather than cut short as on a leave.
	#[moq_clock::test]
	async fn a_head_landing_after_a_clean_end_finishes_its_group() {
		let largest = Some(ietf::Location { group: 4, object: 0 });
		for version in JOINING_DRAFTS {
			let mut ok = subscribe_ok(version, largest);
			ok.extend(message_bytes(
				ietf::PublishDone::ID,
				&ietf::PublishDone {
					status_code: ietf::PublishDoneStatus::TrackEnded.code(),
					stream_count: 0,
					reason_phrase: "done".into(),
				},
				version,
			));
			let mut run = JoinRun::start(version, None, ok, fetch_ok(version)).await;
			let fetch_id = *run
				.subscriber
				.state
				.lock()
				.fetches
				.keys()
				.next()
				.expect("joining FETCH");

			// The head's first object arrives, but not its end.
			let mut objects = Vec::new();
			let w = &mut Encoder::new(&mut objects, version.into());
			w.varint(ietf::FetchHeader::TYPE).unwrap();
			ietf::FetchHeader { request_id: fetch_id }.encode(w, version).unwrap();
			ietf::FetchObject::Object {
				subgroup: ietf::FetchSubgroup::Zero,
				group: Some(4),
				object: Some(0),
				priority: Some(0),
				properties: None,
			}
			.encode(w, version)
			.unwrap();
			w.bytes(b"x").unwrap();
			let data = ScriptedSession::new(objects);
			let (_, recv) = data.clone().open_bi(Default::default()).await.unwrap();
			let mut reader = Reader::new(crate::transport::boxed::RecvStream::new(recv), version);
			let mut receiving = run.subscriber.clone();
			let mut fill = Box::pin(receiving.recv_fill(&mut reader));
			assert!(futures::poll!(fill.as_mut()).is_pending());

			let mut subscriber = (&mut run._hold.2).await.expect("subscribed");
			let mut group = subscriber.recv_group().await.unwrap().unwrap();
			assert_eq!(group.sequence, 4, "{version}");
			assert_eq!(group.read_frame().await.unwrap().unwrap().payload.as_ref(), b"x");

			// The grace runs out with the head still being written, ending the subscription.
			// The finished copy lingers for fills while the reader holds it.
			while !run.subscriber.state.lock().subscribes.is_empty() {
				moq_clock::sleep(Duration::from_millis(1)).await;
			}
			assert!(!run.serving.is_finished(), "{version}: the finished copy lingers");

			data.close(crate::lite::test_transport::Close::Fin);
			assert!(
				matches!(futures::poll!(fill.as_mut()), Poll::Ready(Ok(()))),
				"{version}"
			);
			let end = group.read_frame().await;
			assert!(matches!(end, Ok(None)), "{version}: the head was cut short: {end:?}");
		}
	}

	/// SUBSCRIPTION_ENDED ends only our subscription, not the track, so the copy is released
	/// for a later reader to subscribe afresh rather than lingering as if the track ended. Its
	/// reader sees an error, not a clean end: the fresh copy's groups continue past this one's,
	/// so an end here would be one the track runs past.
	#[moq_clock::test]
	async fn a_subscription_ended_copy_does_not_linger() {
		for version in JOINING_DRAFTS {
			let mut ok = subscribe_ok(version, None);
			ok.extend(message_bytes(
				ietf::PublishDone::ID,
				&ietf::PublishDone {
					status_code: 0x3,
					stream_count: 0,
					reason_phrase: "done".into(),
				},
				version,
			));
			let mut run = JoinRun::start(version, None, ok, Vec::new()).await;
			let mut subscriber = (&mut run._hold.2).await.expect("subscribed");
			while !run.subscriber.state.lock().subscribes.is_empty() {
				settle().await;
			}
			settle().await;
			assert!(run.serving.is_finished(), "{version}: the copy is released");
			let end = subscriber.recv_group().await.map(|group| group.is_some());
			assert!(end.is_err(), "{version}: the copy ended cleanly: {end:?}");
		}
	}

	#[derive(Clone, Copy, PartialEq, Eq)]
	enum FetchStage {
		Credit,
		CreditRetired,
		CreditGranted,
		Unanswered,
		Retired,
		Accepted,
		Receiving,
		Complete,
		/// Complete, then the publisher resets the request stream.
		CompleteReset,
	}

	/// Last-reader cancellation covers a pending answer, an accepted group waiting for
	/// its stream, and a stream stalled after its first complete frame.
	async fn abandon_group_fetch(version: Version, stage: FetchStage) {
		const GROUP: u64 = 4;
		let complete = matches!(stage, FetchStage::Complete | FetchStage::CompleteReset);
		let waiting = matches!(
			stage,
			FetchStage::Credit | FetchStage::CreditRetired | FetchStage::CreditGranted
		);
		let session = ScriptedSession::new(Vec::new());
		let session = if waiting { session.without_credit() } else { session };
		let (tasks, _task_set) = TaskSet::new();
		let mut subscriber = Subscriber::new(
			crate::time::sim(),
			crate::driver::Transport::new(session.clone()),
			crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce(),
			Control::new(false),
			None,
			peer::PeerSetup::default(),
			crate::Hop::new(1).unwrap(),
			None,
			version,
			tasks,
			Default::default(),
		);
		let route = kio::Producer::new(());
		subscriber.validity = crate::model::broadcast::RequestValidity::token(Some(route.consume()));
		let track = track::Producer::new(
			std::sync::Arc::new(crate::broadcast::Info::default()),
			"video",
			track::Info::default().with_timescale(None),
		);
		let dynamic = track.requests();
		let consumer = track.consume();
		let mut fetch = Box::pin(consumer.fetch_group(GROUP, None));
		assert!(futures::poll!(fetch.as_mut()).is_pending());
		let request = dynamic.requested_fill().await.expect("group request");
		let outcome = request.joined();
		let mut run = Box::pin(subscriber.clone().run_group_fetch(
			Path::from_static("broadcast"),
			"video".into(),
			request,
			None,
		));
		assert!(futures::poll!(run.as_mut()).is_pending());
		if waiting {
			assert_eq!(session.log.bi_opens(), 0, "{version}: waits before opening");
			assert!(subscriber.state.lock().group_fetches.is_empty());
			if stage == FetchStage::CreditGranted {
				session.grant_credit();
				assert!(futures::poll!(run.as_mut()).is_pending());
				assert_eq!(session.log.bi_opens(), 1, "{version}: credit releases the FETCH");
			}
			if stage == FetchStage::CreditRetired {
				drop(route);
			} else {
				drop(fetch);
			}
			assert!(
				futures::poll!(run.as_mut()).is_ready(),
				"{version}: releases owned work"
			);
			assert!(subscriber.state.lock().group_fetches.is_empty());
			assert!(matches!(
				outcome.read().refused(GROUP).map(|(err, _)| err.clone()),
				Some(Error::Cancel | Error::Unroutable)
			));
			return;
		}
		let slot = subscriber.state.lock().group_fetches[&RequestId(1)].clone();

		if matches!(stage, FetchStage::Unanswered | FetchStage::Retired) {
			if stage == FetchStage::Retired {
				drop(route);
			} else {
				drop(fetch);
			}
			assert!(
				futures::poll!(run.as_mut()).is_ready(),
				"{version:?}: unanswered FETCH stays alive"
			);
			assert!(matches!(
				outcome.read().refused(GROUP).map(|(err, _)| err.clone()),
				Some(Error::Cancel | Error::Unroutable)
			));
		} else {
			session.push(&message_bytes(
				ietf::FetchOk::ID,
				&ietf::FetchOk {
					end_of_track: false,
					end_location: ietf::Location {
						group: GROUP,
						object: 0,
					},
					properties: Default::default(),
				},
				version,
			));
			assert!(futures::poll!(run.as_mut()).is_pending());
			// No group is accepted until its first object arrives.
			let observed = if stage == FetchStage::Accepted {
				drop(fetch);
				assert!(
					futures::poll!(run.as_mut()).is_ready(),
					"{version:?}: answered FETCH stays alive"
				);
				None
			} else {
				let mut objects = Vec::new();
				ietf::FetchObject::Object {
					subgroup: ietf::FetchSubgroup::Zero,
					group: Some(GROUP),
					object: Some(0),
					priority: Some(0),
					properties: None,
				}
				.encode(&mut crate::coding::Encoder::new(&mut objects, version.into()), version)
				.unwrap();
				crate::coding::Encoder::new(&mut objects, version.into())
					.varint(1u64)
					.unwrap();
				objects.push(b'x');
				let data = ScriptedSession::new(objects);
				let (_, recv) = data.clone().open_bi(Default::default()).await.unwrap();
				let mut reader = Reader::new(crate::transport::boxed::RecvStream::new(recv), version);
				let mut receiving = subscriber.clone();
				let mut fill = Box::pin(receiving.recv_group_fetch(&mut reader, slot.clone()));
				assert!(futures::poll!(fill.as_mut()).is_pending());
				let mut group = fetch.await.expect("accepted group");
				let observed = {
					let state = slot.read();
					let GroupFetch::Receiving(Some(handle)) = &*state else {
						panic!("receiving slot")
					};
					handle.clone()
				};
				assert_eq!(group.read_frame().await.unwrap().unwrap().payload.as_ref(), b"x");
				assert!(
					futures::poll!(run.as_mut()).is_pending(),
					"the reader still wants the group"
				);
				drop(group);
				if complete {
					data.close(crate::lite::test_transport::Close::Fin);
					assert!(matches!(futures::poll!(fill.as_mut()), Poll::Ready(Ok(()))));
					if stage == FetchStage::CompleteReset {
						session.close(crate::lite::test_transport::Close::Reset);
					}
					assert!(futures::poll!(run.as_mut()).is_ready());
					assert!(observed.is_finished());
					assert!(!observed.is_aborted());
					let mut cached = consumer.fetch_group(GROUP, None).await.expect("complete group cached");
					assert_eq!(cached.read_frame().await.unwrap().unwrap().payload.as_ref(), b"x");
					assert!(cached.read_frame().await.unwrap().is_none());
				} else {
					assert!(
						futures::poll!(run.as_mut()).is_ready(),
						"{version:?}: partial FETCH stays alive"
					);
					assert!(matches!(futures::poll!(fill.as_mut()), Poll::Ready(Err(Error::Cancel))));
				}
				Some(observed)
			};
			if !complete && let Some(observed) = observed {
				assert!(
					matches!(
						observed.poll_closed(&kio::Waiter::noop()),
						Poll::Ready(Err(Error::Cancel))
					),
					"partial group must abort"
				);
			}
		}

		if complete {
			assert!(session.log.stops().is_empty());
			assert!(session.log.resets().is_empty());
			assert!(subscriber.state.lock().group_fetches.is_empty());
			return;
		}
		assert_eq!(session.log.stops(), [crate::ietf::error::CANCELLED]);
		assert_eq!(session.log.resets(), [crate::ietf::error::CANCELLED]);
		assert!(subscriber.state.lock().group_fetches.is_empty(), "request retired");
	}

	#[moq_clock::test]
	async fn a_retired_group_fetch_before_fetch_ok_is_cancelled() {
		for version in JOINING_DRAFTS {
			abandon_group_fetch(version, FetchStage::Retired).await;
		}
	}

	#[moq_clock::test]
	async fn an_abandoned_group_fetch_before_fetch_ok_is_cancelled() {
		for version in JOINING_DRAFTS {
			abandon_group_fetch(version, FetchStage::Unanswered).await;
		}
	}

	#[moq_clock::test]
	async fn an_abandoned_group_fetch_after_fetch_ok_is_cancelled() {
		for version in JOINING_DRAFTS {
			abandon_group_fetch(version, FetchStage::Accepted).await;
		}
	}

	#[moq_clock::test]
	async fn an_abandoned_partial_group_fetch_is_cancelled() {
		for version in JOINING_DRAFTS {
			abandon_group_fetch(version, FetchStage::Receiving).await;
		}
	}

	#[moq_clock::test]
	async fn a_complete_group_fetch_is_cached_when_the_reader_leaves() {
		for version in JOINING_DRAFTS {
			abandon_group_fetch(version, FetchStage::Complete).await;
		}
	}

	/// A request reset after the group is written must not abort the cached group.
	#[moq_clock::test]
	async fn a_complete_group_fetch_survives_a_request_reset() {
		for version in JOINING_DRAFTS {
			abandon_group_fetch(version, FetchStage::CompleteReset).await;
		}
	}

	#[moq_clock::test]
	async fn a_claimed_fetch_waits_only_while_wanted() {
		for version in JOINING_DRAFTS {
			for stage in [FetchStage::Credit, FetchStage::CreditRetired, FetchStage::CreditGranted] {
				abandon_group_fetch(version, stage).await;
			}
		}
	}

	/// A publisher that resets the request after FETCH_OK owes no fetch stream, so the
	/// group it accepted is aborted instead of left open for every reader to wait on.
	/// Without that, `run_group_fetch` never returns.
	#[moq_clock::test]
	async fn a_group_fetch_reset_after_fetch_ok_aborts_the_group() {
		const VERSION: Version = Version::Draft18;
		const GROUP: u64 = 4;

		let ok = message_bytes(
			ietf::FetchOk::ID,
			&ietf::FetchOk {
				end_of_track: false,
				end_location: ietf::Location {
					group: GROUP,
					object: 0,
				},
				properties: Default::default(),
			},
			VERSION,
		);

		let session = ScriptedSession::per_stream_reset(vec![ok]);
		let (tasks, _task_set) = crate::util::TaskSet::new();
		let subscriber = Subscriber::new(
			crate::time::sim(),
			crate::driver::Transport::new(session.clone()),
			crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce(),
			Control::new(false),
			None,
			peer::PeerSetup::default(),
			crate::Hop::new(1).unwrap(),
			None,
			VERSION,
			tasks,
			Default::default(),
		);

		let track = track::Producer::new(
			std::sync::Arc::new(crate::broadcast::Info::default()),
			"video",
			track::Info::default().with_timescale(None),
		);
		let dynamic = track.requests();
		let consumer = track.consume();
		let mut fetch = std::pin::pin!(consumer.fetch_group(GROUP, None));
		assert!(futures::poll!(fetch.as_mut()).is_pending());
		let request = dynamic.requested_fill().await.expect("no group requested");

		subscriber
			.clone()
			.run_group_fetch(Path::from_static("broadcast"), "video".into(), request, None)
			.await;

		assert!(fetch.await.is_err(), "the accepted group was aborted");
	}

	/// From draft-22 an End Location outside the FETCH's range is malformed:
	/// the session closes with PROTOCOL_VIOLATION rather than reading it as an empty answer.
	#[moq_clock::test]
	async fn a_draft22_end_location_outside_the_range_closes_the_session() {
		const GROUP: u64 = 4;
		const START: u64 = 3;

		let version = Version::Draft22;
		for end_location in [
			ietf::Location {
				group: GROUP,
				object: START - 1,
			},
			ietf::Location {
				group: GROUP,
				object: 0,
			},
		] {
			for end_of_track in [false, true] {
				let ok = message_bytes(
					ietf::FetchOk::ID,
					&ietf::FetchOk {
						end_of_track,
						end_location,
						properties: Default::default(),
					},
					version,
				);

				let session = ScriptedSession::per_stream_reset(vec![ok]);
				let (tasks, _task_set) = crate::util::TaskSet::new();
				let subscriber = Subscriber::new(
					crate::time::sim(),
					crate::driver::Transport::new(session.clone()),
					crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce(),
					Control::new(false),
					None,
					peer::PeerSetup::default(),
					crate::Hop::new(1).unwrap(),
					None,
					version,
					tasks,
					Default::default(),
				);

				let track = track::Producer::new(
					std::sync::Arc::new(crate::broadcast::Info::default()),
					"video",
					track::Info::default().with_timescale(None),
				);
				let dynamic = track.requests();
				let consumer = track.consume();
				let mut fetch = std::pin::pin!(consumer.fetch_group(
					GROUP,
					group::Fetch {
						frame_start: START,
						..Default::default()
					}
				));
				assert!(futures::poll!(fetch.as_mut()).is_pending());
				let request = dynamic.requested_fill().await.expect("no group requested");

				subscriber
					.clone()
					.run_group_fetch(Path::from_static("broadcast"), "video".into(), request, None)
					.await;

				assert_eq!(
					consumer.final_sequence(),
					None,
					"{version}: rejected end changed the track"
				);
				assert!(matches!(fetch.await, Err(Error::ProtocolViolation)));
				let violation = SessionError::from(&Error::ProtocolViolation).to_code();
				assert!(
					session.log.closes().iter().any(|(code, _)| *code == violation),
					"{:?}",
					session.log.closes()
				);
			}
		}
	}

	/// A cache miss for a group's tail asks upstream from the frame the reader wants and
	/// numbers what arrives from there, so a publisher that evicted the prefix can answer.
	#[moq_clock::test]
	async fn a_group_fetch_asks_from_the_wanted_frame() {
		for version in JOINING_DRAFTS.into_iter().chain([Version::Draft22]) {
			for ended in [false, true] {
				group_fetch_from_frame(version, false, ended).await;
			}
		}
	}

	/// Legacy moxygen/cache answers one group past the requested group remain readable.
	#[moq_clock::test]
	async fn a_legacy_group_fetch_answer_still_covers_the_requested_group() {
		for version in JOINING_DRAFTS {
			for ended in [false, true] {
				group_fetch_from_frame(version, true, ended).await;
			}
		}
	}

	async fn group_fetch_from_frame(version: Version, legacy: bool, ended: bool) {
		const GROUP: u64 = 4;
		const START: u64 = 2;
		const LAST: u64 = START + 1;
		let draft20 = Filter::is_draft20(version);

		// The whole group, as each draft answers it: draft 22 names the last object, and
		// older drafts name the whole group with Object 0.
		let end_location = match draft20 {
			true => ietf::Location {
				group: GROUP,
				object: LAST,
			},
			false => ietf::Location {
				group: GROUP + u64::from(legacy),
				object: 0,
			},
		};
		let ok = message_bytes(
			ietf::FetchOk::ID,
			&ietf::FetchOk {
				end_of_track: ended,
				end_location,
				properties: Default::default(),
			},
			version,
		);

		// The fetch stream answering our first request id, from object START on.
		let mut objects = Vec::new();
		crate::coding::Encoder::new(&mut objects, version.into())
			.varint(ietf::FetchHeader::TYPE)
			.unwrap();
		ietf::FetchHeader {
			request_id: RequestId(1),
		}
		.encode(&mut crate::coding::Encoder::new(&mut objects, version.into()), version)
		.unwrap();
		for (index, payload) in [b"c", b"d"].iter().enumerate() {
			let first = index == 0;
			ietf::FetchObject::Object {
				subgroup: ietf::FetchSubgroup::Zero,
				group: first.then_some(GROUP),
				object: first.then_some(START),
				priority: first.then_some(0),
				properties: None,
			}
			.encode(&mut crate::coding::Encoder::new(&mut objects, version.into()), version)
			.unwrap();
			crate::coding::Encoder::new(&mut objects, version.into())
				.varint(1u64)
				.unwrap();
			objects.extend_from_slice(&payload[..]);
		}

		let session = ScriptedSession::per_stream_eof(vec![ok, objects.to_vec()]);
		let (tasks, _task_set) = crate::util::TaskSet::new();
		let subscriber = Subscriber::new(
			crate::time::sim(),
			crate::driver::Transport::new(session.clone()),
			crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce(),
			Control::new(false),
			None,
			peer::PeerSetup::default(),
			crate::Hop::new(1).unwrap(),
			None,
			version,
			tasks,
			Default::default(),
		);

		let track = track::Producer::new(
			std::sync::Arc::new(crate::broadcast::Info::default()),
			"video",
			track::Info::default().with_timescale(None),
		);
		let dynamic = track.requests();
		let consumer = track.consume();
		let mut fetch = std::pin::pin!(consumer.fetch_group(
			GROUP,
			group::Fetch {
				frame_start: START,
				..Default::default()
			}
		));
		assert!(futures::poll!(fetch.as_mut()).is_pending());
		let request = dynamic.requested_fill().await.expect("no group requested");

		let serving = moq_clock::sim::spawn(subscriber.clone().run_group_fetch(
			Path::from_static("broadcast"),
			"video".into(),
			request,
			None,
		));
		settle().await;

		// The fetch stream, as the peer would open it.
		let (_, recv) = session.clone().open_bi(Default::default()).await.unwrap();
		let mut stream = Reader::new(crate::transport::boxed::RecvStream::new(recv), version);
		subscriber
			.clone()
			.recv_fill(&mut stream)
			.await
			.unwrap_or_else(|err| panic!("{version}: {err:?}"));
		serving.await.expect("run_group_fetch");

		assert_eq!(consumer.final_sequence(), ended.then_some(GROUP + 1));
		let mut group = fetch.await.expect("fetched");
		assert_eq!(group.index(), START, "the group starts where the reader wanted");
		let mut payloads = Vec::new();
		while let Some(frame) = group.read_frame().await.expect("complete") {
			payloads.push(frame.payload.to_vec());
		}
		assert_eq!(payloads, [b"c".to_vec(), b"d".to_vec()]);

		let messages = decode_messages(&session.log, version);
		let fetch = messages.iter().find(|(id, _)| *id == ietf::Fetch::ID).expect("FETCH");
		let mut body = Decoder::new(&fetch.1, version.into());
		let msg = ietf::Fetch::decode_msg(&mut body, version).unwrap();
		let from = ietf::Location {
			group: GROUP,
			object: START,
		};
		match msg.fetch_type {
			// No End Object is the whole End Group.
			FetchType::Filtered { filter, .. } if draft20 => assert_eq!(
				filter,
				Filter::Absolute {
					start: from,
					end: Some(ietf::EndLocation {
						group: GROUP,
						object: None
					}),
				},
				"{version}: through the end of the group"
			),
			// An End Object of 0 is the whole End Group.
			FetchType::Standalone { start, end, .. } if !draft20 => {
				assert_eq!(start, from, "{version}");
				assert_eq!(
					end,
					ietf::Location {
						group: GROUP,
						object: 0
					},
					"{version}: through the end of the group"
				);
			}
			other => panic!("{version}: unexpected group fetch: {other:?}"),
		}
	}
}

/// NEW_GROUP_REQUEST on the upstream subscription, with the original publisher's end of
/// the session played by hand and downstream subscribers joining the relay's copy.
#[cfg(test)]
mod new_group_tests {
	use super::*;
	use crate::{model::ProduceTest, transport::memory};

	async fn settle() {
		moq_clock::sleep(Duration::from_millis(1)).await;
	}

	const VERSION: Version = Version::Draft22;

	type PeerStream = Stream<Transport, Version>;

	struct Relay {
		track: track::Consumer,
		probe: Subscriber,
		peer: memory::Session,
		serving: moq_clock::sim::JoinHandle<()>,
		/// Landed groups stay open, as a live publisher's would.
		landed: Vec<group::Producer>,
		_broadcast: crate::broadcast::Producer,
		_tasks: TaskSet,
	}

	/// Serve one track upstream, for a first subscriber still waiting on its answer.
	async fn relay() -> (Relay, kio::Pending<track::Subscribing>) {
		let (session, peer) = memory::pair(memory::Config::default());
		let (tasks, task_set) = TaskSet::new();
		let mut subscriber = Subscriber::new(
			crate::time::sim(),
			crate::driver::Transport::new(session.clone()),
			crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce(),
			Control::new(false),
			None,
			peer::PeerSetup::declared(peer::Peer::default()),
			crate::Hop::new(1).unwrap(),
			None,
			VERSION,
			tasks,
			Default::default(),
		);
		let broadcast = crate::broadcast::Info::default().produce();
		let mut dynamic = broadcast.requests();
		let track = broadcast.consume().track("video").unwrap();
		let first = track.subscribe(None);
		let request = dynamic.requested_track().await.expect("no track requested");
		let probe = subscriber.clone();
		let serving = moq_clock::sim::spawn(async move {
			subscriber
				.run_subscribe(Path::from_static("broadcast"), dynamic, None, request)
				.await;
		});
		let relay = Relay {
			track,
			probe,
			peer,
			serving,
			landed: Vec::new(),
			_broadcast: broadcast,
			_tasks: task_set,
		};
		(relay, first)
	}

	async fn read(stream: &mut PeerStream) -> (u64, ietf::Body) {
		let read = async {
			let id = stream.reader.varint().await.unwrap();
			(id, stream.reader.decode::<ietf::Body>().await.unwrap())
		};
		moq_clock::timeout(Duration::from_secs(1), read)
			.await
			.expect("the relay sent nothing")
	}

	impl Relay {
		/// Accept the SUBSCRIBE the relay sends upstream, returning its NEW_GROUP_REQUEST.
		async fn accept(&mut self) -> (PeerStream, Option<u64>) {
			let mut stream = Stream::accept(&mut crate::driver::Transport::new(self.peer.clone()), VERSION)
				.await
				.unwrap();
			let (id, body) = read(&mut stream).await;
			assert_eq!(id, ietf::Subscribe::ID);
			let msg = ietf::Subscribe::decode_msg(&mut body.decoder(VERSION), VERSION).unwrap();
			(stream, msg.new_group)
		}

		/// Group `sequence` lands in the copy: a first frame, and a delta when `delta`.
		fn land(&mut self, sequence: u64, delta: bool) {
			let state = self.probe.state.lock();
			let copy = state.subscribes.values().next().unwrap().producer.clone().unwrap();
			drop(state);
			let mut group = copy.create_group(group::Info { sequence }).unwrap();
			group.write_frame(None, b"key".as_slice()).unwrap();
			if delta {
				group.write_frame(None, b"delta".as_slice()).unwrap();
			}
			self.landed.push(group);
		}

		/// A downstream subscriber joins the copy.
		async fn join(&self) -> track::Subscriber {
			let subscriber = self.track.subscribe(None).await.unwrap();
			settle().await;
			subscriber
		}
	}

	/// Accept the subscription at Largest Group `largest`, with or without DYNAMIC_GROUPS.
	async fn accept(stream: &mut PeerStream, dynamic_groups: bool, largest: u64) {
		stream.writer.varint(ietf::SubscribeOk::ID).await.unwrap();
		stream
			.writer
			.encode(&ietf::SubscribeOk {
				track_alias: 7,
				largest: Some(ietf::Location {
					group: largest,
					object: 0,
				}),
				properties: ietf::Properties {
					dynamic_groups,
					..Default::default()
				},
			})
			.await
			.unwrap();
		settle().await;
	}

	/// The NEW_GROUP_REQUEST of the next REQUEST_UPDATE, which carries nothing else.
	async fn update(stream: &mut PeerStream) -> Option<u64> {
		let (id, body) = read(stream).await;
		assert_eq!(id, ietf::SubscribeUpdate::ID);
		let msg = ietf::SubscribeUpdate::decode_msg(&mut body.decoder(VERSION), VERSION).unwrap();
		assert_eq!((msg.subscriber_priority, msg.forward), (None, None));
		msg.new_group
	}

	async fn answer(stream: &mut PeerStream) {
		stream.writer.varint(ietf::RequestOk::ID).await.unwrap();
		stream
			.writer
			.encode(&ietf::RequestOk {
				largest: None,
				active: None,
			})
			.await
			.unwrap();
		settle().await;
	}

	/// Nothing more reached the publisher.
	async fn quiet(stream: &mut PeerStream) {
		settle().await;
		assert!(
			futures::poll!(std::pin::pin!(stream.reader.varint_maybe())).is_pending(),
			"the relay sent another message"
		);
	}

	/// A copy with nothing cached has no group to judge a join by, so its SUBSCRIBE asks for
	/// none.
	#[moq_clock::test]
	async fn a_first_subscribe_asks_for_no_group() {
		let (mut relay, _first) = relay().await;
		let (_stream, sent) = relay.accept().await;
		assert_eq!(sent, None);
		relay.serving.abort();
	}

	/// A join over the ratio goes upstream in a REQUEST_UPDATE for the group past the
	/// newest. Joins until that group lands ask for the same one and go no further.
	#[moq_clock::test]
	async fn a_join_over_the_ratio_requests_a_group() {
		let (mut relay, _first) = relay().await;
		let (mut stream, _) = relay.accept().await;
		accept(&mut stream, true, 5).await;
		relay.land(5, true);

		let _a = relay.join().await;
		assert_eq!(update(&mut stream).await, Some(6));
		answer(&mut stream).await;
		let _b = relay.join().await;
		quiet(&mut stream).await;

		relay.land(6, true);
		let _c = relay.join().await;
		assert_eq!(update(&mut stream).await, Some(7));
		answer(&mut stream).await;
		quiet(&mut stream).await;
		relay.serving.abort();
	}

	/// A join under the ratio reads the current group and asks for nothing.
	#[moq_clock::test]
	async fn a_join_under_the_ratio_asks_nothing() {
		let (mut relay, _first) = relay().await;
		let (mut stream, _) = relay.accept().await;
		accept(&mut stream, true, 5).await;
		relay.land(5, false);

		let _a = relay.join().await;
		quiet(&mut stream).await;
		relay.serving.abort();
	}

	/// One REQUEST_UPDATE is in flight at a time; a join meanwhile goes up once it is answered.
	#[moq_clock::test]
	async fn a_join_behind_an_unanswered_update_waits() {
		let (mut relay, _first) = relay().await;
		let (mut stream, _) = relay.accept().await;
		accept(&mut stream, true, 5).await;
		relay.land(5, true);

		let _a = relay.join().await;
		assert_eq!(update(&mut stream).await, Some(6));
		relay.land(6, true);
		let _b = relay.join().await;
		quiet(&mut stream).await;

		answer(&mut stream).await;
		assert_eq!(update(&mut stream).await, Some(7));
		answer(&mut stream).await;
		quiet(&mut stream).await;
		relay.serving.abort();
	}

	/// A refused update ends the subscription, so nothing more goes up while PUBLISH_DONE
	/// is on its way, even for a join that waited behind it.
	#[moq_clock::test]
	async fn a_refused_update_sends_no_more() {
		let (mut relay, _first) = relay().await;
		let (mut stream, _) = relay.accept().await;
		accept(&mut stream, true, 5).await;
		relay.land(5, true);

		let _a = relay.join().await;
		assert_eq!(update(&mut stream).await, Some(6));
		relay.land(6, true);
		let _b = relay.join().await;
		stream.writer.varint(ietf::RequestError::ID).await.unwrap();
		stream
			.writer
			.encode(&ietf::RequestError {
				error_code: 0,
				reason_phrase: "refused".into(),
				retry_interval: 0,
			})
			.await
			.unwrap();
		quiet(&mut stream).await;
		relay.serving.abort();
	}

	/// A group the publisher already has is on its way, so a join asking for it goes no
	/// further.
	#[moq_clock::test]
	async fn a_group_upstream_has_is_not_requested() {
		let (mut relay, _first) = relay().await;
		let (mut stream, _) = relay.accept().await;
		accept(&mut stream, true, 7).await;
		relay.land(5, true);

		let _a = relay.join().await;
		quiet(&mut stream).await;
		relay.serving.abort();
	}

	/// A publisher that did not advertise DYNAMIC_GROUPS refreshes on no join, so it never
	/// hears a REQUEST_UPDATE carrying one.
	#[moq_clock::test]
	async fn a_publisher_without_dynamic_groups_hears_no_updates() {
		let (mut relay, _first) = relay().await;
		let (mut stream, _) = relay.accept().await;
		accept(&mut stream, false, 5).await;
		relay.land(5, true);

		let _a = relay.join().await;
		quiet(&mut stream).await;
		relay.serving.abort();
	}

	/// A group still wanted when the copy goes idle rides the SUBSCRIBE that resumes it, and
	/// is outstanding from there.
	#[moq_clock::test]
	async fn a_resumed_copy_carries_its_request() {
		let (mut relay, first) = relay().await;
		let (mut stream, _) = relay.accept().await;
		accept(&mut stream, true, 5).await;
		relay.land(5, true);
		let a = relay.join().await;
		assert_eq!(update(&mut stream).await, Some(6));
		answer(&mut stream).await;

		// The last subscriber leaves, which cancels the upstream subscription.
		drop((first, a));
		settle().await;

		let _b = relay.track.subscribe(None);
		let (mut stream, sent) = relay.accept().await;
		assert_eq!(sent, Some(6));
		accept(&mut stream, true, 5).await;
		quiet(&mut stream).await;
		relay.serving.abort();
	}

	/// An identified fill keeps ownership until its head is handed off. A fill with no
	/// header can instead be superseded, since a reset may have discarded that header.
	#[moq_clock::test]
	async fn only_an_identified_fill_holds_back_a_widening() {
		let (mut relay, mut first) = relay().await;
		let (mut stream, _) = relay.accept().await;
		accept(&mut stream, false, 5).await;
		let fill = relay
			.probe
			.state
			.lock()
			.subscribes
			.values()
			.next()
			.unwrap()
			.fill
			.clone();
		*fill.write().ok().unwrap() = Fill::Active;

		first.update(track::Subscription::default().with_range(6..)).unwrap();
		let (id, body) = read(&mut stream).await;
		assert_eq!(id, ietf::SubscribeUpdate::ID);
		let msg = ietf::SubscribeUpdate::decode_msg(&mut body.decoder(VERSION), VERSION).unwrap();
		assert!(msg.filter.is_some(), "the narrowing went up");
		assert!(msg.fill.is_none());
		answer(&mut stream).await;

		first.update(track::Subscription::default()).unwrap();
		quiet(&mut stream).await;

		// No stream claimed the head after all. This request can be retired without
		// waiting for a failure signal that could never identify the request.
		*fill.write().ok().unwrap() = Fill::Serving(None);
		let (id, body) = read(&mut stream).await;
		assert_eq!(id, ietf::SubscribeUpdate::ID);
		let msg = ietf::SubscribeUpdate::decode_msg(&mut body.decoder(VERSION), VERSION).unwrap();
		assert!(msg.fill.is_some(), "the widening superseded the unidentified fill");
		assert!(matches!(*fill.read(), Fill::Done), "the old request was retired");
		relay.serving.abort();
	}

	/// Delayed headers, including one already waiting for REQUEST_OK, belong to the
	/// request they answer and cannot consume a later widening's fill.
	#[moq_clock::test]
	async fn superseded_fill_headers_cannot_claim_the_new_request() {
		use crate::{coding::Encode, lite::test_transport::ScriptedSession};

		let header = |request_id| async move {
			let mut bytes = Vec::new();
			let mut encoder = crate::coding::Encoder::new(&mut bytes, VERSION.into());
			encoder.varint(ietf::FetchHeader::TYPE).unwrap();
			ietf::FetchHeader { request_id }.encode(&mut encoder, VERSION).unwrap();
			let session = ScriptedSession::per_stream_eof(vec![bytes]);
			let (_, reader) = session.open_bi(Default::default()).await.unwrap();
			Reader::new(crate::transport::boxed::RecvStream::new(reader), VERSION)
		};

		let (mut relay, mut first) = relay().await;
		let (mut stream, _) = relay.accept().await;
		accept(&mut stream, false, 5).await;
		let subscription = *relay.probe.state.lock().subscribes.keys().next().unwrap();
		first.update(track::Subscription::default().with_range(6..)).unwrap();
		let _ = read(&mut stream).await;
		answer(&mut stream).await;
		first.update(track::Subscription::default().with_range(4..)).unwrap();
		let (id, body) = read(&mut stream).await;
		assert_eq!(id, ietf::SubscribeUpdate::ID);
		let old = ietf::SubscribeUpdate::decode_msg(&mut body.decoder(VERSION), VERSION).unwrap();

		// The initial SUBSCRIBE's delayed header cannot consume this update's slot.
		let mut initial = header(subscription).await;
		assert!(matches!(
			relay.probe.clone().recv_fill(&mut initial).await,
			Err(Error::Unsupported)
		));

		// This update's header overtakes its REQUEST_OK and parks on the old slot.
		let mut early = header(old.request_id).await;
		let mut receiving = relay.probe.clone();
		let mut early = std::pin::pin!(receiving.recv_fill(&mut early));
		assert!(futures::poll!(early.as_mut()).is_pending());
		stream
			.writer
			.encode_message(&ietf::RequestOk {
				largest: Some(ietf::Location { group: 5, object: 0 }),
				active: None,
			})
			.await
			.unwrap();
		settle().await;

		first.update(track::Subscription::default()).unwrap();
		let (id, body) = read(&mut stream).await;
		assert_eq!(id, ietf::SubscribeUpdate::ID);
		let newest = ietf::SubscribeUpdate::decode_msg(&mut body.decoder(VERSION), VERSION).unwrap();
		assert!(newest.fill.is_some());
		assert!(matches!(early.await, Err(Error::Unsupported)));

		// The same old header arriving after retirement has no mapping left, either.
		let mut late = header(old.request_id).await;
		assert!(matches!(
			relay.probe.clone().recv_fill(&mut late).await,
			Err(Error::NotFound)
		));
		let fill = relay.probe.state.lock().subscribes[&subscription].fill.clone();
		assert!(matches!(*fill.read(), Fill::Requested));
		stream
			.writer
			.encode_message(&ietf::RequestOk {
				largest: Some(ietf::Location { group: 5, object: 0 }),
				active: None,
			})
			.await
			.unwrap();
		settle().await;
		let mut current = header(newest.request_id).await;
		relay.probe.clone().recv_fill(&mut current).await.unwrap();
		assert!(
			matches!(*fill.read(), Fill::Failed { sequence: 5 }),
			"only the current header claimed the slot, and it carried no head"
		);
		relay.serving.abort();
	}

	/// A widening update replaces a fill that failed to head its group, while that group's
	/// mid-group tail can still be in flight. The tail stays headless both to a stream
	/// already opening on the old fill and to one opening after either replacement.
	#[moq_clock::test]
	async fn a_replaced_failed_fill_keeps_its_group_headless() {
		use crate::{coding::Encode, lite::test_transport::ScriptedSession};

		let header = |request_id| async move {
			let mut bytes = Vec::new();
			let mut encoder = crate::coding::Encoder::new(&mut bytes, VERSION.into());
			encoder.varint(ietf::FetchHeader::TYPE).unwrap();
			ietf::FetchHeader { request_id }.encode(&mut encoder, VERSION).unwrap();
			let session = ScriptedSession::per_stream_eof(vec![bytes]);
			let (_, reader) = session.open_bi(Default::default()).await.unwrap();
			Reader::new(crate::transport::boxed::RecvStream::new(reader), VERSION)
		};

		let (mut relay, mut first) = relay().await;
		let (mut stream, _) = relay.accept().await;
		accept(&mut stream, false, 5).await;
		let subscription = *relay.probe.state.lock().subscribes.keys().next().unwrap();
		first.update(track::Subscription::default().with_range(6..)).unwrap();
		let _ = read(&mut stream).await;
		answer(&mut stream).await;

		// Each widening's fill answers with no objects, so it owes its Largest Group's head
		// for good, and the next widening replaces it.
		let mut failed = Vec::new();
		for (range, largest) in [(Some(4), 5), (Some(2), 6), (None, 6)] {
			let wanted = track::Subscription::default();
			first
				.update(match range {
					Some(start) => wanted.with_range(start..),
					None => wanted,
				})
				.unwrap();
			let (id, body) = read(&mut stream).await;
			assert_eq!(id, ietf::SubscribeUpdate::ID);
			let widening = ietf::SubscribeUpdate::decode_msg(&mut body.decoder(VERSION), VERSION).unwrap();
			assert!(widening.fill.is_some());
			if range.is_none() {
				break;
			}
			stream
				.writer
				.encode_message(&ietf::RequestOk {
					largest: Some(ietf::Location {
						group: largest,
						object: 0,
					}),
					active: None,
				})
				.await
				.unwrap();
			settle().await;
			let mut empty = header(widening.request_id).await;
			relay.probe.clone().recv_fill(&mut empty).await.unwrap();
			let fill = relay.probe.state.lock().subscribes[&subscription].fill.clone();
			assert!(matches!(*fill.read(), Fill::Failed { sequence } if sequence == largest));
			failed.push((largest, fill));
		}

		for (sequence, fill) in &failed {
			assert!(
				fill.read().headless(*sequence),
				"a stream already opening on the old fill still finds group {sequence} headless"
			);
		}
		assert_eq!(
			relay.probe.state.lock().subscribes[&subscription].headless_through,
			Some(6),
			"a stream opening after either replacement does too: group 5 is below the watermark"
		);
		relay.serving.abort();
	}
}

/// Track Properties on SUBSCRIBE_OK, with the original publisher's end of the session played
/// by hand.
#[cfg(test)]
mod track_property_tests {
	use super::*;
	use crate::{
		coding::Encoder,
		model::{
			ProduceTest,
			object::{Property, Value},
		},
		transport::memory,
	};

	/// The newest draft and an older one, both with a request stream per subscription.
	const VERSIONS: [Version; 3] = [Version::Draft18, Version::Draft20, Version::Draft22];

	/// Subscribe to one track through a relay on `version`, whose publisher answers with
	/// `block` as SUBSCRIBE_OK's Track Properties, returning what the subscriber got.
	async fn subscribe(version: Version, block: &[u8]) -> (Result<track::Subscriber, Error>, Subscriber) {
		let (session, peer) = memory::pair(memory::Config::default());
		let (tasks, _task_set) = TaskSet::new();
		let mut subscriber = Subscriber::new(
			crate::time::sim(),
			crate::driver::Transport::new(session.clone()),
			crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce(),
			Control::new(false),
			None,
			peer::PeerSetup::declared(peer::Peer::default()),
			crate::Hop::new(1).unwrap(),
			None,
			version,
			tasks,
			Default::default(),
		);
		let broadcast = crate::broadcast::Info::default().produce();
		let mut dynamic = broadcast.requests();
		let track = broadcast.consume().track("video").unwrap();
		let subscribing = track.subscribe(None);
		let request = dynamic.requested_track().await.expect("no track requested");
		let probe = subscriber.clone();
		let serving = moq_clock::sim::spawn(async move {
			subscriber
				.run_subscribe(Path::from_static("broadcast"), dynamic, None, request)
				.await;
		});

		let mut stream = Stream::accept(&mut crate::driver::Transport::new(peer.clone()), version)
			.await
			.unwrap();
		assert_eq!(stream.reader.varint().await.unwrap(), ietf::Subscribe::ID);
		let _subscribe: ietf::Body = stream.reader.decode().await.unwrap();

		let mut body = Vec::new();
		ietf::SubscribeOk {
			track_alias: 7,
			largest: None,
			properties: Default::default(),
		}
		.encode_msg(&mut Encoder::new(&mut body, version.into()), version)
		.unwrap();
		// Track Properties are the final field.
		body.extend_from_slice(block);
		let mut frame = Vec::new();
		Encoder::new(&mut frame, version.into())
			.varint(ietf::SubscribeOk::ID)
			.unwrap();
		frame.extend(u16::try_from(body.len()).unwrap().to_be_bytes());
		frame.extend(body);
		stream.writer.write_all(&mut bytes::Bytes::from(frame)).await.unwrap();

		let subscribed = moq_clock::timeout(Duration::from_secs(1), subscribing)
			.await
			.expect("the subscription was never answered");
		moq_clock::sleep(Duration::from_millis(1)).await;
		serving.abort();
		(subscribed, probe)
	}

	/// A Track Properties block holding `pairs`, in ascending order.
	fn block(version: Version, pairs: &[Property]) -> Vec<u8> {
		let mut block = Vec::new();
		let mut w = Encoder::new(&mut block, version.into());
		let mut prev = 0;
		for Property { id, value } in pairs {
			w.varint(id - prev).unwrap();
			prev = *id;
			match value {
				Value::Int(value) => w.varint(*value).unwrap(),
				Value::Bytes(value) => w.bytes(value).unwrap(),
			}
		}
		block
	}

	/// An even property the draft defines but moq-net does not implement
	/// (OBJECT_DELIVERY_TIMEOUT) and an odd one from the application range reach the track
	/// unchanged, for the relay's own answers downstream to forward.
	#[moq_clock::test]
	async fn unknown_properties_reach_the_track() {
		for version in VERSIONS {
			let unknown = vec![
				Property {
					id: 0x02,
					value: Value::Int(5000),
				},
				Property {
					id: 0x3801,
					value: Value::Bytes(bytes::Bytes::from_static(b"opaque")),
				},
			];
			let (subscribed, _probe) = subscribe(version, &block(version, &unknown)).await;
			let subscriber = subscribed.unwrap_or_else(|err| panic!("{version}: {err}"));
			let properties = subscriber
				.info()
				.properties
				.clone()
				.expect("the properties were dropped");
			assert_eq!(properties.values, unknown, "{version}");
			assert!(properties.immutable().is_none(), "{version}");
		}
	}

	/// A Mandatory Track Property we do not understand, mutable or sealed, refuses the
	/// track with UNSUPPORTED_EXTENSION instead of forwarding it.
	#[moq_clock::test]
	async fn a_mandatory_property_refuses_the_track() {
		for version in VERSIONS {
			let mandatory = Property {
				id: 0x4000,
				value: Value::Int(1),
			};
			let sealed = Property {
				id: 0x0b,
				value: Value::Bytes(block(version, std::slice::from_ref(&mandatory)).into()),
			};
			for property in [mandatory, sealed] {
				let (subscribed, probe) = subscribe(version, &block(version, &[property])).await;
				assert!(
					matches!(subscribed, Err(Error::UnsupportedExtension)),
					"{version}: {:?}",
					subscribed.map(|subscriber| subscriber.info().clone())
				);
				assert!(
					probe.state.lock().subscribes.is_empty(),
					"{version}: the subscription was kept"
				);
			}
		}
	}

	/// A sealed DEFAULT_PUBLISHER_GROUP_ORDER is advisory: the relay accepts the track, serves
	/// its own order, and keeps the sealed block to forward unchanged.
	#[moq_clock::test]
	async fn a_sealed_group_order_the_relay_does_not_serve_is_accepted() {
		for version in VERSIONS {
			for order in [2, 1] {
				let sealed = Property {
					id: 0x0b,
					value: Value::Bytes(
						block(
							version,
							&[Property {
								id: 0x22,
								value: Value::Int(order),
							}],
						)
						.into(),
					),
				};
				let (subscribed, _probe) = subscribe(version, &block(version, std::slice::from_ref(&sealed))).await;
				let subscriber = subscribed.unwrap_or_else(|err| panic!("{version}: {order}: {err}"));
				let properties = subscriber
					.info()
					.properties
					.clone()
					.expect("the sealed block was dropped");
				assert_eq!(properties.values, [sealed], "{version}: {order}");
			}
		}
	}

	/// A TIMESCALE keeps the publisher's units, mutable or sealed: a sealed one travels
	/// downstream unchanged, and a mutable one matches a copy that arrived over another route.
	#[moq_clock::test]
	async fn a_timescale_keeps_its_units() {
		for version in VERSIONS {
			let timescale = Property {
				id: super::super::properties::TIMESCALE,
				value: Value::Int(1000),
			};
			let sealed = Property {
				id: 0x0b,
				value: Value::Bytes(block(version, std::slice::from_ref(&timescale)).into()),
			};
			for property in [timescale, sealed] {
				let (subscribed, _probe) = subscribe(version, &block(version, &[property])).await;
				let subscriber = subscribed.unwrap_or_else(|err| panic!("{version}: {err}"));
				assert_eq!(subscriber.info().timescale, Some(Timescale::MILLI), "{version}");
			}
		}
	}
}

#[cfg(test)]
mod full_track_name_tests {
	use super::*;
	use crate::{model::ProduceTest, transport::memory};

	/// A Full Track Name too long for the wire fails only its own subscription, and the
	/// session carries the next one.
	#[moq_clock::test]
	async fn an_unencodable_track_name_fails_only_its_subscription() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			let (session, peer) = memory::pair(memory::Config::default());
			let (tasks, _task_set) = TaskSet::new();
			let subscriber = Subscriber::new(
				crate::time::sim(),
				crate::driver::Transport::new(session.clone()),
				crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce(),
				Control::new(false),
				None,
				peer::PeerSetup::declared(peer::Peer::default()),
				crate::Hop::new(1).unwrap(),
				None,
				version,
				tasks,
				Default::default(),
			);
			// One byte past 4096 with the 9-byte namespace.
			let long = "x".repeat(4088);
			for (name, fits) in [(long.as_str(), false), ("video", true)] {
				let broadcast = crate::broadcast::Info::default().produce();
				let mut dynamic = broadcast.requests();
				let track = broadcast.consume().track(name).unwrap();
				let mut subscribing = std::pin::pin!(track.subscribe(None));
				let request = dynamic.requested_track().await.expect("no track requested");
				let mut subscriber = subscriber.clone();
				let serving = moq_clock::sim::spawn(async move {
					subscriber
						.run_subscribe(Path::from_static("broadcast"), dynamic, None, request)
						.await;
				});
				if fits {
					// The refused request's stream was reset before its header, which a peer ignores.
					let id = loop {
						let mut stream = Stream::accept(&mut crate::driver::Transport::new(peer.clone()), version)
							.await
							.unwrap();
						if let Ok(id) = stream.reader.varint().await {
							break id;
						}
					};
					assert_eq!(id, ietf::Subscribe::ID, "{version}");
				} else {
					let refused = moq_clock::timeout(Duration::from_secs(1), &mut subscribing)
						.await
						.expect("the subscription was never refused");
					assert!(
						matches!(refused, Err(Error::Encode(crate::coding::EncodeError::BoundsExceeded))),
						"{version}: {:?}",
						refused.map(|_| ())
					);
				}
				serving.abort();
			}
		}
	}
}

#[cfg(test)]
mod compression_negotiation_tests {
	use super::*;
	#[test]
	fn a_compression_declaration_requires_mutual_negotiation() {
		let properties = ietf::Properties {
			compress: true,
			..Default::default()
		};
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			assert!(matches!(
				refused(&properties, version, false),
				Some(Error::UnsupportedExtension)
			));
			assert!(refused(&properties, version, true).is_none(), "{version}");
		}
		let info = accepted_info(&properties, Version::Draft22);
		assert!(info.compress);
		assert!(info.properties.unwrap().values.is_empty());
	}
}
