use crate::driver::Transport;
use crate::transport::poll::Session as _;
use crate::{frame, group, origin, track};
use futures::FutureExt as _;
use std::{
	collections::{HashMap, VecDeque},
	ops::Bound,
	sync::{
		Arc,
		atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
	},
	task::{Poll, ready},
	time::Duration,
};

use crate::transport::poll::SendGroup as _;
use crate::transport::poll::{GroupOf, SendOptions};

use crate::{
	DecodeError, Error, Timescale, Timestamp,
	coding::{Encode as _, Encoder, Stream, Writer},
	ietf::{self, Control, EndLocation, FetchHeader, FetchType, Filter, Location, RequestId},
	priority::{Priority, PriorityHandle, PriorityQueue},
	track::{Order, Subscription},
	util::BoxFuture,
};

use super::{Message, Version, cluster, error::request, peer, request_update};

/// Preserve retained groups when the subscriber supplies no age budget.
fn serving_subscription(subscriber_priority: u8) -> Subscription {
	Subscription {
		priority: super::priority::from_wire(subscriber_priority),
		// No Max Age is the publisher's retention alone, whatever it turns out to be.
		max_delay: None,
		..Default::default()
	}
}

/// The Track Properties an answer describing the track carries: SUBSCRIBE_OK,
/// TRACK_STATUS_OK, and FETCH_OK. Empty when the requester opted out with
/// INCLUDE_PROPERTIES=0, which keeps the block present but empty.
///
/// Declaring the timescale is what opts the track into timestamps; every object Timestamp
/// is in these units. A draft without room for a property drops it on encode.
///
/// Fails with [`Error::Unsupported`] for Immutable Properties the draft cannot carry:
/// rewriting them for its encoding would break them, and forwarding them unchanged would
/// change their meaning.
fn track_properties(
	info: &track::Info,
	wanted: bool,
	version: Version,
	negotiated: bool,
) -> Result<ietf::Properties, Error> {
	let compressed = info.compress && negotiated;
	super::flate::egress(info, compressed)?;
	let mut plain = info.clone();
	plain.compress = compressed;
	let info = &plain;
	if !wanted {
		return Ok(ietf::Properties::default());
	}
	let Some(track) = info.properties.as_deref() else {
		return Ok(ietf::Properties::restated(info));
	};
	let mut unknown = track.values.clone();
	// Between draft-18 and draft-22, forwarding 0x06 would give it a different meaning, as for object
	// properties. A mutable one is dropped; a sealed one is refused.
	if super::object::has_delivery_timeout(track.version) != super::object::has_delivery_timeout(version) {
		if track.sealed().any(|id| id == 0x06) {
			return Err(Error::Unsupported);
		}
		unknown.retain(|property| property.id != 0x06);
	}
	Ok(ietf::Properties {
		unknown,
		..ietf::Properties::restated(info)
	})
}

/// The rendezvous cap is zero, but asking to wait still makes a missing publisher a timeout.
/// Once the broadcast resolves, a missing track is the publisher's answer and stays DOES_NOT_EXIST.
fn rendezvous_error(error: Error, requested: bool, resolved: bool) -> Error {
	let missing = match &error {
		Error::Unroutable | Error::Stream(crate::StreamError::Unroutable) => true,
		Error::NotFound | Error::Stream(crate::StreamError::NotFound) => !resolved,
		_ => false,
	};
	if requested && missing { Error::Timeout } else { error }
}

#[cfg_attr(
	target_pointer_width = "64",
	expect(
		clippy::large_enum_variant,
		reason = "returned from one poll and matched at once, so its size never sits in a task"
	)
)]
enum FillStep {
	Batch,
	Partial(frame::Consumer),
	Done,
}

/// Where a fetch object sits relative to the one written before it on the same stream.
#[derive(Clone, Copy)]
enum FetchPrior {
	/// The first object on the stream.
	None,
	/// The next object of the same group.
	Same(Option<u64>),
	/// The first object of a group after another one's, carrying its Group ID field.
	Group(u64),
}

/// Where a fetch object sits, given the group written before its own, if any.
fn fetch_position(prior_group: Option<u64>, group: u64, prior: Option<u64>, object: u64) -> Result<FetchPrior, Error> {
	match (prior_group, prior) {
		// A later group is spelled as its distance from the prior one, less one.
		(Some(prior_group), None) => Ok(FetchPrior::Group(
			group
				.abs_diff(prior_group)
				.checked_sub(1)
				.ok_or(Error::MalformedTrack)?,
		)),
		_ => fetch_prior(prior, object),
	}
}

/// The span a FETCH answers, as a standalone FETCH spells it.
#[derive(Clone, Copy)]
struct FetchRange {
	/// The first Location.
	start: Location,
	/// The exclusive end: End Object 0 includes the whole End Group.
	end: Location,
}

impl FetchRange {
	/// The first Object ID read from group `sequence`.
	fn skip(&self, sequence: u64) -> u64 {
		match sequence == self.start.group {
			true => self.start.object,
			false => 0,
		}
	}

	/// The exclusive Object ID bound in group `sequence`, or `None` to read it whole.
	fn until(&self, sequence: u64) -> Option<u64> {
		(sequence == self.end.group && self.end.object > 0).then_some(self.end.object)
	}
}

/// A FETCH resolved against its track, with its group read ahead.
struct PreparedFetch {
	range: FetchRange,
	/// Read ahead so FETCH_OK can say where the response ends.
	last: FetchedGroup,
}

/// Resolve a one-group FETCH and read its objects before promising them in FETCH_OK.
/// A whole-group request waits for an open group to finish.
async fn prepare_fetch(
	track: &track::Consumer,
	mut range: FetchRange,
	largest_filter: Option<Filter>,
	joined: bool,
	priority: i8,
	compression: bool,
) -> Result<PreparedFetch, (Error, String)> {
	let failed = |err: Error| {
		let reason = err.to_string();
		(err, reason)
	};
	track.info().await.map_err(failed)?;
	// The cache's newest object is Largest Object while a live feed writes the track, or once
	// the track ended at its newest group. A fetch-only copy retains its publisher's report
	// independently. A joining FETCH ends where its subscription starts instead.
	let finished = track.final_sequence();
	let known =
		track.is_live() || finished.is_some_and(|end| Some(end) == track.latest_sequence().map(|latest| latest + 1));
	let largest = match !joined && (known || track.reported_object().is_some()) {
		true => live_edge(track).largest,
		false => None,
	};
	if let Some(filter) = largest_filter {
		let Some(largest) = largest else {
			return Err((
				Error::Unsupported,
				"FETCH relative to Largest Object needs a known publisher edge".into(),
			));
		};
		range.start = match filter {
			Filter::Unfiltered => Location { group: 0, object: 0 },
			Filter::NextObject => Location {
				group: largest.group,
				object: largest.object.saturating_add(1),
			},
			Filter::Relative(groups) => Location {
				group: largest.group.saturating_add(1).saturating_sub(groups),
				object: 0,
			},
			Filter::Absolute { start, .. } => start,
		};
		range.end = Location {
			group: largest.group,
			object: 0,
		};
	}
	if range.start.group != range.end.group {
		return Err((Error::Unsupported, "FETCH spans more than one group".into()));
	}

	if let Some(largest) = largest {
		// A start past Largest Object in a group the cache holds is refused INVALID_RANGE
		// (draft-22 section 9.11), rather than waiting on the live group. A group past every
		// one held does not exist yet.
		if (range.start.group, range.start.object) > (largest.group, largest.object) {
			let held = range
				.start
				.group
				.checked_add(1)
				.is_some_and(|end| !track.cached_groups(range.start.group..end).is_empty());
			return Err(match held {
				true => (Error::InvalidRange, "start past Largest Object".into()),
				false => failed(Error::NotFound),
			});
		}
	}

	let last = read_fetch(
		track,
		range.start.group,
		range.start.object,
		range.until(range.start.group),
		priority,
		compression,
	)
	.await
	.map_err(failed)?;
	Ok(PreparedFetch { range, last })
}

/// The group a FETCH answers with, read out of the cache so a later eviction cannot
/// truncate what FETCH_OK already promised.
struct FetchedGroup {
	sequence: u64,
	/// The Object ID of the first frame.
	first: u64,
	frames: Vec<frame::Object>,
	/// The group's END_OF_GROUP status object, when it falls in the range.
	end_object: Option<Arc<crate::model::object::Fields>>,
	/// The fetched range reached the finished group's final object.
	complete: bool,
	/// The track's info, which FETCH_OK describes and whose units stamp the objects.
	info: track::Info,
}

struct FetchObjectInfo<'a> {
	sequence: u64,
	object: u64,
	prior: FetchPrior,
	timestamp: Option<Timestamp>,
	inflated: Option<u64>,
	timescale: Option<Timescale>,
	version: Version,
	fields: Option<&'a crate::model::object::Fields>,
	/// The draft the track's publisher spoke, which serialized `fields`.
	source: Option<Version>,
}

fn fetch_prior(prior: Option<u64>, object: u64) -> Result<FetchPrior, Error> {
	let Some(prior) = prior else {
		return Ok(FetchPrior::None);
	};
	let step = object
		.checked_sub(prior)
		.filter(|step| *step > 0)
		.ok_or(Error::MalformedTrack)?;
	Ok(FetchPrior::Same((step != 1).then_some(step)))
}

impl FetchedGroup {
	/// Drop the END_OF_GROUP object: FETCH objects carry no status, so each group ends at its
	/// last frame, which is all a receiver can account for.
	fn without_status(mut self) -> Self {
		self.end_object = None;
		self
	}

	/// One past the last Object ID read, including the END_OF_GROUP object.
	fn end(&self) -> u64 {
		if let Some(end) = &self.end_object {
			return end.id.saturating_add(1);
		}
		self.frames.last().map_or(self.first, |frame| {
			frame
				.fields
				.as_ref()
				.map_or(self.first + self.frames.len() as u64, |fields| {
					fields.id.saturating_add(1)
				})
		})
	}
}

/// Read group `sequence` of `track` for a FETCH, from object `skip` up to the exclusive
/// `until`, or through the end of the group without one.
///
/// This is a [`track::Consumer::fetch_group`], so a relay fetches a miss upstream. The
/// track's info resolves first, as it would for a SUBSCRIBE.
async fn read_fetch(
	track: &track::Consumer,
	sequence: u64,
	skip: u64,
	until: Option<u64>,
	priority: i8,
	compression: bool,
) -> Result<FetchedGroup, Error> {
	let info = track.info().await?;
	let cached = track.peek_group(sequence);
	let wire = cached.as_ref().and_then(|group| group.wire());
	if wire.as_ref().is_some_and(|wire| !wire.fetchable) {
		return Err(Error::Unsupported);
	}
	// Relayed Object IDs need not match frame ordinals, so a relayed group is read from
	// its first frame and filtered by ID. Its marked gaps cover the IDs between where it
	// starts and its first object; a start below that misses to the upstream. A miss may
	// be relayed too, which only the upstream knows, so it fetches the whole group.
	let frame_start = match (&wire, &cached) {
		(Some(_), Some(group)) => skip.min(group.first_frame()),
		(None, Some(_)) => skip,
		(_, None) => 0,
	};
	let mut group = track
		.fetch_group(
			sequence,
			group::Fetch {
				priority,
				frame_start,
				..Default::default()
			},
		)
		.await?;
	if compression {
		group = group.compressed();
	}
	let mut first = skip;
	let mut frames = Vec::new();
	// Reading an object past the range proves the range missed the group's end.
	let mut overshot = false;
	// A long cached group is read a slice per poll, so the session's other tasks still run.
	let mut budget = kio::coop::Budget::new(32);
	loop {
		kio::wait(|waiter| budget.poll_yield(waiter)).await;
		if group.wire().is_some_and(|wire| !wire.fetchable) {
			return Err(Error::Unsupported);
		}
		let ordinal = group.index();
		let Some(frame) = kio::wait(|waiter| group.poll_read_object(waiter)).await? else {
			break;
		};
		let id = frame.fields.as_ref().map_or(ordinal, |fields| fields.id);
		if until.is_some_and(|until| id >= until) {
			overshot = true;
			break;
		}
		if id < skip {
			continue;
		}
		if frames.is_empty() {
			first = id;
		}
		frames.push(frame);
		if until.is_some_and(|until| id.saturating_add(1) >= until) {
			break;
		}
	}
	// A bounded range can reach the group's end without reading its terminal None, and a
	// start past the end is already there.
	let complete = !overshot
		&& matches!(group.finished().now_or_never(), Some(Ok(_)))
		&& group.index() >= group.frame_count() as u64;
	let end_object = group
		.end_object()
		.filter(|end| complete && end.id >= skip && until.is_none_or(|until| end.id < until));

	if group.wire().is_some_and(|wire| !wire.fetchable) {
		return Err(Error::Unsupported);
	}
	Ok(FetchedGroup {
		sequence,
		first,
		frames,
		end_object,
		complete,
		info,
	})
}

/// A broadcast whose route table is watched for changes in what we advertise: the
/// namespace becoming (un)advertisable, or its path or cost moving.
struct Watched {
	/// The route last announced for this namespace, as the origin delivered it.
	route: crate::origin::Route,
	/// What the peer currently holds for this namespace, or [`Advert::None`] while it
	/// is filtered. A selection that differs is worth a wire message; one that matches
	/// is not.
	sent: Advert,
	/// The peer should hold this namespace but does not: it refused the request, or we
	/// could not get a stream to make it on. Nothing about that clears on its own, so the
	/// loop comes back to it on a timer.
	deferred: bool,
	/// What the peer's refusal said about coming back, which outranks that timer.
	refused: Refused,
	/// How long to stay away the next time the peer ends this advertisement while idle.
	lapse: Duration,
}

/// What a refusal said about re-offering the namespace.
///
/// A peer answers a request it declines with a retry interval ({{moqt}} REQUEST_ERROR),
/// and ignoring it is how a permanent refusal (unauthorized, uninterested) turns into a
/// request every few seconds for the life of the session.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Refused {
	/// Never refused, or refused on a draft whose error carries no interval, so our own
	/// backoff is the only guidance there is.
	#[default]
	No,
	/// Refused with a minimum wait before re-offering.
	Until(crate::time::Instant),
	/// Refused with an interval of 0: the peer does not want this offered again.
	Never,
}

impl Refused {
	/// Whether a fresh offer may go out now.
	///
	/// The single gate, consulted on every reconciliation rather than only on the retry
	/// sweep: a route change re-prices an advertisement but does not excuse us from a
	/// wait the peer asked for, nor make a refused namespace a different one.
	fn offerable(&self, now: crate::time::Instant) -> bool {
		match self {
			Self::No => true,
			Self::Until(at) => now >= *at,
			Self::Never => false,
		}
	}

	/// Whether the loop should keep coming back at all. Only a refusal that forbids
	/// retrying ends it; a wait still has to arm the timer that counts it out.
	fn pending(&self) -> bool {
		*self != Self::Never
	}
}

impl Watched {
	fn new(route: crate::origin::Route) -> Self {
		Self {
			route,
			sent: Advert::None,
			deferred: false,
			refused: Refused::No,
			lapse: RETRY_BASE,
		}
	}

	/// The peer ended an advertisement it had accepted. The wait doubles each time, so a
	/// peer that ends every one it accepts sees them on a backoff rather than in a loop.
	fn lapsed(&mut self, now: crate::time::Instant) -> Refused {
		let wait = self.lapse;
		self.lapse = (wait * 2).min(RETRY_MAX);
		Refused::Until(now + wait)
	}
}

/// What to advertise to this peer for one broadcast.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum Advert {
	/// Nothing: every route loops through the peer or us, or none is announced.
	#[default]
	None,
	/// The namespace, with the parameters of the extensions the peer negotiated: the path
	/// it traversed and that path's accumulated cost (MoQ Cluster), and the instance it
	/// serves (MoQ Epoch). Empty when the peer negotiated neither.
	Some(ietf::AdvertParams),
}

impl Advert {
	/// Whether the peer should hold this namespace at all.
	fn wanted(&self) -> bool {
		!matches!(self, Self::None)
	}

	/// The parameters to put on the wire, as the message structs carry them.
	fn params(&self) -> ietf::AdvertParams {
		match self {
			Self::Some(params) => params.clone(),
			Self::None => Default::default(),
		}
	}
}

/// How long to wait for a stream to advertise one namespace on.
///
/// Only reached when the peer has granted no more, which on this path means it is holding
/// every advertisement we already sent. Long enough that a merely slow peer is not given
/// up on, short enough that the loop resumes and can retire something.
const ADVERTISE_TIMEOUT: Duration = Duration::from_secs(5);

/// First wait before re-offering a namespace we could not get up.
const RETRY_BASE: Duration = Duration::from_millis(100);

/// Ceiling on that wait. The loop retries for the life of the session, so it must settle
/// into a slow poll rather than a spin.
const RETRY_MAX: Duration = Duration::from_secs(5);

/// Spread a retry over half its window, so every namespace on a busy relay does not come
/// back at the same instant.
fn jitter(delay: Duration) -> Duration {
	use rand::RngExt;
	delay.mul_f64(0.5 + rand::rng().random::<f64>() / 2.0)
}

/// Where one announce loop's advertisements go.
enum Target {
	/// Inline NAMESPACE entries on the SUBSCRIBE_NAMESPACE stream that asked for them.
	Inline(Stream<Transport, Version>),
	/// Each advertisement on its own unsolicited PUBLISH_NAMESPACE request.
	Requests,
}

impl Target {
	/// The SUBSCRIBE_NAMESPACE stream this loop answers, if any.
	fn stream(&mut self) -> Option<&mut Stream<Transport, Version>> {
		match self {
			Self::Inline(stream) => Some(stream),
			Self::Requests => None,
		}
	}

	/// `goaways` checks each GOAWAY the requester sends on this stream.
	fn poll_event(
		&mut self,
		finished: &mut bool,
		goaways: &mut super::request_stream::Messages,
		version: Version,
		waiter: &kio::Waiter,
	) -> Poll<NamespaceEvent> {
		use super::request_stream::{NamespaceUpdate, Next};

		let Some(stream) = self.stream() else {
			return Poll::Pending;
		};
		while !*finished {
			match stream.reader.poll_decode_maybe::<Next<NamespaceUpdate>>(waiter) {
				Poll::Ready(Ok(Some(Next::Message(update)))) => return Poll::Ready(NamespaceEvent::Request(update)),
				Poll::Ready(Ok(Some(Next::GoAway { redirect }))) => {
					if let Err(err) = goaways.receive(redirect) {
						return Poll::Ready(NamespaceEvent::Closed(Err(err)));
					}
				}
				Poll::Ready(Ok(None)) if !super::request_stream::fin_cancels(version) => *finished = true,
				Poll::Ready(Ok(None)) => return Poll::Ready(NamespaceEvent::Closed(Ok(()))),
				Poll::Ready(Err(err)) => return Poll::Ready(NamespaceEvent::Closed(Err(err))),
				Poll::Pending => break,
			}
		}
		stream.writer.poll_closed(waiter).map(NamespaceEvent::Closed)
	}
}

/// One announce loop's state: where its advertisements go, and what the peer holds.
struct Namespaces {
	/// What the peer declared in its SETUP, which decides what an advertisement carries.
	peer: cluster::Peer,
	target: Target,
	/// Every announced broadcast under this loop's prefix.
	watched: HashMap<crate::Path, Watched>,
	/// The open PUBLISH_NAMESPACE request carrying each advertised namespace. Empty when
	/// the entries ride a SUBSCRIBE_NAMESPACE stream inline.
	requests: HashMap<crate::Path, NamespaceRequest>,
	/// Requests with an update to send, in the order they asked for a request ID. Each
	/// waits for credit beside the loop's other work and computes its update when it gets
	/// one, so a later change supersedes an earlier one that is still waiting.
	updates: VecDeque<crate::Path>,
	/// Requests with an update in flight, each answered on its own stream, by when each
	/// gives up. Sent in order with one timeout, so the front is the earliest; an entry
	/// whose request moved on is skipped when it reaches the front.
	inflight: VecDeque<(crate::time::Instant, crate::Path)>,
	/// Requests whose stream woke the loop, so a wake polls those rather than every
	/// update in flight.
	woken: Arc<Woken>,
	/// What we may advertise: our grant (MoQ Auth) and the ceiling on the peer.
	permit: crate::auth::Permit,
	/// The dispatcher-owned gate for a solicited namespace request.
	gate: Option<crate::auth::Gate>,
	/// The auth epoch last applied to `permit`.
	epoch: u64,
	/// Whether this discovery cursor includes hidden namespaces.
	hidden: bool,
	/// Whether the peer negotiated MoQ Epoch, so an advertisement names its route's
	/// instance.
	epochs: bool,
}

impl Namespaces {
	fn new(peer: cluster::Peer, target: Target) -> Self {
		Self {
			peer,
			target,
			watched: HashMap::new(),
			requests: HashMap::new(),
			updates: VecDeque::new(),
			inflight: VecDeque::new(),
			woken: Default::default(),
			permit: Default::default(),
			gate: None,
			epoch: 0,
			hidden: false,
			epochs: false,
		}
	}

	/// What the peer negotiated for an advertisement's parameters.
	fn negotiated(&self) -> ietf::Negotiated {
		ietf::Negotiated {
			cluster: self.peer.negotiated(),
			epoch: self.epochs,
		}
	}

	/// Whether we may advertise `path`. An unknown grant allows everything.
	fn permitted(&self, path: &crate::Path) -> bool {
		self.permit.matches(path.as_str())
	}
}

/// How a subscription's serve loop ended, short of the peer cancelling it.
#[derive(Debug)]
enum Served {
	/// The track and any fill finished, or serving ended with this error.
	Track(Result<(), Error>),
	/// An update could not be applied, its parameters unsupported or its renewal refused
	/// (draft-18 section 10.9.1). The update was already answered, and PUBLISH_DONE says
	/// UPDATE_FAILED.
	UpdateFailed(Error),
	/// A renewal was refused, ending the request's grant. Unlike [`Self::UpdateFailed`] the
	/// update is not answered yet: its REQUEST_ERROR is written once serving stops, outside
	/// the grant's guard, which the ended grant would otherwise trip first.
	RenewalRefused(crate::auth::Refusal),
	/// FORWARD=0 paused the subscription, which stays established. True when an update set
	/// it and is not answered yet, false when a SUBSCRIBE started paused.
	Paused(bool),
}

/// What reading a subscriber's next message on its request stream found.
enum UpdateRead {
	/// A REQUEST_UPDATE, already decoded.
	Update(super::request_stream::Update),
	/// The subscriber cancelled.
	Cancelled,
	/// A malformed message, which a protocol violation makes the session's to report.
	Malformed(Error),
	/// The update named a Request ID we never granted; the session is already closing.
	Failed(Error),
}

/// How an admitted subscription came out of serving.
enum Subscribed {
	/// It never started, refused with this reason and error before SUBSCRIBE_OK.
	Refused(String, Error),
	/// The subscriber withdrew it, or the session closed, before SUBSCRIBE_OK.
	Withdrawn,
	/// It was served, and opened this many group streams.
	Served(Served, u64),
}

impl Served {
	/// The PUBLISH_DONE status and reason this end reports.
	fn status(&self) -> (ietf::PublishDoneStatus, &'static str) {
		match self {
			Self::Track(Ok(())) => (ietf::PublishDoneStatus::TrackEnded, "track ended"),
			Self::Track(Err(
				Error::Unauthorized
				| Error::Session(crate::SessionError::Unauthorized)
				| Error::Stream(crate::StreamError::Unauthorized),
			)) => (ietf::PublishDoneStatus::Unauthorized, "not granted"),
			// PUBLISH_DONE registers no token expiry, and its EXPIRED means the subscription's
			// own lifetime ran out, so an expired token is reported as no longer granted.
			Self::Track(Err(Error::Expired | Error::Session(crate::SessionError::Expired))) => {
				(ietf::PublishDoneStatus::Unauthorized, "token expired")
			}
			Self::Track(Err(_)) => (ietf::PublishDoneStatus::InternalError, "internal error"),
			Self::UpdateFailed(_) | Self::RenewalRefused(_) => (ietf::PublishDoneStatus::UpdateFailed, "update failed"),
			// Pausing ends no subscription, so nothing reports it.
			Self::Paused(_) => (ietf::PublishDoneStatus::InternalError, "paused"),
		}
	}
}

/// What woke an announce-forwarding loop.
enum NamespaceEvent {
	/// The session or stream ended, with the result to surface.
	Closed(Result<(), Error>),
	/// A requester changes the namespace prefix.
	Request(super::request_stream::NamespaceUpdate),
	/// An origin-level route (un)announce: whether it is active and whether it
	/// restarts, `None` once the announce stream ends.
	Update(Option<(crate::announce::Announce, bool, bool)>),
	/// The retry sleep fired: re-offer whatever the peer should be holding and isn't.
	Retry,
	/// Our grant (MoQ Auth) or the ceiling changed: re-check every namespace against it.
	Regrant(crate::auth::Permit),
	/// The queued update at the front got this request ID.
	Renew(RequestId),
	/// What came back for an update in flight.
	Answered(crate::Path, Result<Answer, Error>),
}

/// What came back for a PUBLISH_NAMESPACE or an update in flight.
enum Answer {
	/// The peer's REQUEST_OK or REQUEST_ERROR.
	Message(super::request_stream::Framed),
	/// The peer ended the request instead, which cancels it.
	Ended,
	/// Nothing before its deadline.
	Late,
}

/// The requests whose stream woke an announce loop since it last looked.
#[derive(Default)]
struct Woken {
	suffixes: std::sync::Mutex<Vec<crate::Path>>,
	/// The loop, woken in turn.
	owner: std::sync::Mutex<Option<std::task::Waker>>,
}

impl Woken {
	/// A waker that marks `suffix` woken.
	fn waker(self: &Arc<Self>, suffix: crate::Path) -> std::task::Waker {
		std::task::Waker::from(Arc::new(RequestWaker {
			woken: self.clone(),
			suffix,
		}))
	}
}

struct RequestWaker {
	woken: Arc<Woken>,
	suffix: crate::Path,
}

impl std::task::Wake for RequestWaker {
	fn wake(self: Arc<Self>) {
		self.wake_by_ref()
	}

	fn wake_by_ref(self: &Arc<Self>) {
		self.woken.suffixes.lock().unwrap().push(self.suffix.clone());
		if let Some(owner) = &*self.woken.owner.lock().unwrap() {
			owner.wake_by_ref();
		}
	}
}

#[derive(Clone)]
pub(super) struct Publisher {
	pub(super) withdrawal: crate::session::Withdrawal,
	// Arms the advertise, retry, and linger timers.
	runtime: crate::time::Deadlines,
	session: Transport,
	// Traffic stats are attributed through this tagged origin handle.
	origin: origin::Consumer,
	pub(super) control: Control,
	// Our own Hop ID, stamped onto every advertisement we forward. Taken from the
	// origin we consume so it matches the local relay identity across every session,
	// which is what makes cross-session loop detection work.
	self_origin: crate::Hop,
	// The identity assigned to the peer (a fresh per-session id, or one the caller
	// pinned with `with_peer_hop`), used when the peer declares
	// none itself. A peer that negotiates the MoQ Cluster extension declares its own,
	// which wins unless it withheld it as the reserved 0.
	peer_hop: Option<crate::Hop>,
	// What the peer declared in its SETUP, filled when that stream is read.
	peer_setup: peer::PeerSetup,
	// Shared across request handlers; None marks a dispatched subscription still resolving.
	joins: kio::Shared<HashMap<RequestId, Option<Joined<GroupOf<Transport>>>>>,
	version: Version,
	// Our grant (MoQ Auth): only what it lets us publish is advertised and served, and a
	// shrink withdraws what it no longer covers.
	auth: crate::auth::Link,
	// Dispatched finite serves, including those not yet polled.
	pub(super) owed: Arc<AtomicUsize>,
	// Subscriptions the peer holds, PUBLISHes for its listings included, counted for the peak.
	pub(super) subscriptions: crate::session::Slots,
	// Ranks every subscription's group streams against each other for the transport.
	priority: PriorityQueue,
}

/// Authorization selected by dispatch: a standalone path or a live parent subscription.
enum FetchGate {
	Standalone(crate::auth::Gate),
	Joining { id: RequestId, auth: crate::auth::Link },
}

/// A dispatched SUBSCRIBE or FETCH serve, counted until dropped.
struct Serve {
	owed: Arc<AtomicUsize>,
	_working: crate::session::Working,
}

impl Drop for Serve {
	fn drop(&mut self) {
		self.owed.fetch_sub(1, Ordering::Relaxed);
	}
}

/// The snapshot a joining FETCH inherits from its subscription.
#[derive(Clone)]
#[allow(
	clippy::large_enum_variant,
	reason = "one per live subscription, and the small variants refuse at once; an allow, since the size gap varies by feature set"
)]
enum Joined<G> {
	/// This subscription's filter is not supported by the joining FETCH handler.
	Unsupported,
	/// No objects existed when the subscription started.
	Empty,
	/// The prefix ending immediately after the saved Largest Object.
	Group {
		end: Location,
		cache: track::Consumer,
		/// The subscription's namespace, which the fetch is held to the grant on.
		namespace: crate::Path,
		/// The grant the subscription's own request token earned, if one authorized it. The
		/// fetch is held to it too, past the subscription's end, and the limit bounds neither.
		grant: Option<crate::auth::RequestGrant>,
		/// The subscription's ranking, which the fetch joins.
		ranking: Ranking<G>,
	},
}

/// One subscription's entry in [`Publisher::joins`], removed when the subscription ends.
///
/// A fetch that arrives afterwards then finds nothing and is refused, rather than being
/// answered from a group that is no longer the edge of anything.
struct Join<G> {
	joins: kio::Shared<HashMap<RequestId, Option<Joined<G>>>>,
	request_id: RequestId,
}

impl<G> Drop for Join<G> {
	fn drop(&mut self) {
		self.joins.lock().remove(&self.request_id);
	}
}

impl Publisher {
	pub fn new(
		runtime: crate::time::Deadlines,
		session: Transport,
		origin: origin::Consumer,
		control: Control,
		peer_hop: Option<crate::Hop>,
		peer_setup: peer::PeerSetup,
		version: Version,
	) -> Self {
		Self {
			withdrawal: Default::default(),
			runtime,
			session,
			self_origin: origin.hop(),
			// Serving the peer is not a local reader asking to discover anything.
			origin: origin.serving(),
			control,
			peer_hop,
			peer_setup,
			joins: Default::default(),
			version,
			auth: crate::auth::Link::new(false),
			owed: Default::default(),
			subscriptions: Default::default(),
			priority: Default::default(),
		}
	}

	/// Mark a paused subscription for joining FETCHes: draft-18 section 5.1 refuses one
	/// unless the subscription forwards.
	fn pause_join(&self, join: Option<&Join<GroupOf<Transport>>>) {
		if let Some(join) = join {
			join.joins.lock().insert(join.request_id, Some(Joined::Empty));
		}
	}

	/// Bound what we publish by the grant this session's tokens earn (MoQ Auth).
	pub fn with_auth(mut self, auth: crate::auth::Link) -> Self {
		self.auth = auth;
		self
	}

	/// What the peer declared in its SETUP.
	///
	/// Blocks until the peer's SETUP arrives, because the extension changes the NAMESPACE
	/// encoding: nothing can be advertised until we know whether the peer speaks it.
	fn poll_peer(&self, waiter: &kio::Waiter) -> Poll<cluster::Peer> {
		self.peer_setup.poll_get(waiter).map(|peer| peer.cluster)
	}

	/// Await [`Self::poll_peer`].
	async fn peer(&self) -> cluster::Peer {
		kio::wait(|waiter| self.poll_peer(waiter)).await
	}

	/// Whether the peer requires advertisements to be solicited, from the same SETUP.
	///
	/// Blocks on it for the same reason [`Self::peer`] does: this decides whether the
	/// first advertisement is sent unasked, so it cannot be guessed and corrected later.
	async fn requires_solicitation(&self) -> bool {
		self.peer_setup.get().await.solicit.unwrap_or(false)
	}

	/// The origin to serve this peer's subscriptions from: sources whose hop chain flows
	/// through the peer are excluded, so a subscription is never handed data that already
	/// flowed through the subscriber.
	///
	/// The same exclusion the announce path applies (see [`Self::select`]), which is what
	/// keeps advertised paths truthful and prevents subscription cycles of any length.
	fn poll_serving_origin(&self, waiter: &kio::Waiter) -> Poll<origin::Consumer> {
		self.poll_peer(waiter).map(|peer| self.excluding(&peer))
	}

	/// Await [`Self::poll_serving_origin`].
	pub(super) async fn serving_origin(&self) -> origin::Consumer {
		kio::wait(|waiter| self.poll_serving_origin(waiter)).await
	}

	/// Resolve a request's broadcast while the peer still wants the answer.
	async fn resolve(
		&self,
		stream: &mut Stream<Transport, Version>,
		path: &crate::Path,
		epoch: Option<crate::Epoch>,
	) -> Result<Option<crate::broadcast::Consumer>, Error> {
		let Some(origin) = self
			.until_withdrawn(stream, |waiter| self.poll_serving_origin(waiter))
			.await
		else {
			return Ok(None);
		};
		// The immediate lookup: answering a peer never starts discovery of its own.
		let requesting = origin.lookup(path.clone(), epoch.as_ref());
		self.until_withdrawn(stream, |waiter| requesting.poll_ok(waiter))
			.await
			.transpose()
	}

	/// Our origin handle with [`Self::exclude`] applied, the view both the data plane
	/// and the announce loops read this peer's routes through.
	fn excluding(&self, peer: &cluster::Peer) -> origin::Consumer {
		self.origin.clone().excluding(self.exclude(peer))
	}

	/// `origin` as advertised to this peer: never its own routes back (split horizon),
	/// and under MoQ Cluster only the routes with room for our hop under the origin's
	/// cap, so selection falls back to a shorter route instead of withdrawing.
	fn advertising(&self, origin: origin::Consumer, peer: &cluster::Peer) -> origin::Consumer {
		let origin = origin.excluding(self.exclude(peer));
		match peer.negotiated() {
			true => origin.forwardable(),
			false => origin,
		}
	}

	/// The Hop ID whose paths must not be advertised (or served) back to this peer.
	///
	/// A peer that declared an identity supplies its own; otherwise fall back to the one
	/// we assigned it (a fresh per-session id, or one pinned with `Client::with_peer_hop`
	/// or `Request::with_peer_hop`), since moq-transport carries no identity
	/// of its own. A peer that declared the reserved 0 declared no identity, so it takes
	/// the fallback like any other anonymous peer.
	fn exclude(&self, peer: &cluster::Peer) -> crate::Hop {
		peer.identity().or(self.peer_hop).unwrap_or(crate::Hop::UNKNOWN)
	}

	/// Pick what to advertise to this peer for one route.
	///
	/// The origin's announce cursor already filters routes through the peer
	/// (control-plane split horizon); this only shapes what the wire can carry.
	/// `epochs` says the peer negotiated MoQ Epoch, so the route's instance travels too.
	fn select(&self, route: &crate::origin::Route, peer: &cluster::Peer, epochs: bool) -> Advert {
		// A route that already passed through us is a reflection. The origin
		// filters these on receive, so this is defensive.
		if self.self_origin != crate::Hop::UNKNOWN && route.hops.contains(&self.self_origin) {
			return Advert::None;
		}

		// Forwarded unchanged, and never invented for a route that arrived without one.
		let epoch = route.epoch.clone().filter(|_| epochs);
		if !peer.negotiated() {
			return Advert::Some(ietf::AdvertParams { cluster: None, epoch });
		}

		let cost = route.cost.value();
		// Our own Hop ID is always the last entry, so the peer reconstructs the full
		// path. A chain with no room left is a loop in all but name.
		match cluster::Advert::forward(&route.hops, cost, self.self_origin) {
			Ok(advert) => Advert::Some(ietf::AdvertParams {
				cluster: Some(advert),
				epoch,
			}),
			Err(_) => Advert::None,
		}
	}

	/// Handle an incoming bidi stream dispatched by the session.
	///
	/// `epochs` says both sides negotiated MoQ Epoch: a request naming an instance on a
	/// session that did not breaks the session.
	pub fn handle_stream(
		&self,
		id: u64,
		body: ietf::Body,
		stream: Stream<Transport, Version>,
		epochs: bool,
	) -> Result<BoxFuture<'static, ()>, Error> {
		let this = self.clone();
		let mut data = body.decoder(this.version);
		// Count at dispatch so close cannot pass a request waiting for its first poll.
		let serve = matches!(id, ietf::Subscribe::ID | ietf::Fetch::ID).then(|| {
			self.owed.fetch_add(1, Ordering::Relaxed);
			Serve {
				owed: self.owed.clone(),
				_working: self.session.work().start(),
			}
		});
		let task = match id {
			ietf::Subscribe::ID => {
				let msg = match ietf::Subscribe::decode_msg(&mut data, this.version) {
					Err(err) if super::token::refuses_request(&err) => {
						// Junk after the fields is fatal, which a token refusal must not hide.
						if !data.is_empty() {
							return Err(Error::WrongSize);
						}
						return Ok(this.reject_token(stream, request::Kind::Subscribe, err, serve));
					}
					msg => msg?,
				};
				if !data.is_empty() {
					return Err(Error::WrongSize);
				}
				if msg.epoch.is_some() && !epochs {
					return Err(Error::ProtocolViolation);
				}
				tracing::debug!(message = ?msg, "received subscribe");
				let gate = crate::auth::Gate::new(
					this.auth.clone(),
					msg.track_namespace.clone(),
					crate::auth::Direction::Publish,
				);
				let task = this.serve_subscribe_stream(stream, msg.into_owned(), gate);
				async move {
					let _serve = serve;
					if let Err(err) = task.await {
						tracing::debug!(%err, "subscribe stream error");
					}
				}
				.boxed()
			}
			ietf::Fetch::ID => {
				let msg = match ietf::Fetch::decode_msg(&mut data, this.version) {
					Err(err) if super::token::refuses_request(&err) => {
						// Junk after the fields is fatal, which a token refusal must not hide.
						if !data.is_empty() {
							return Err(Error::WrongSize);
						}
						return Ok(this.reject_token(stream, request::Kind::Fetch, err, serve));
					}
					msg => msg?,
				};
				if !data.is_empty() {
					return Err(Error::WrongSize);
				}
				if msg.epoch.is_some() && !epochs {
					return Err(Error::ProtocolViolation);
				}
				tracing::debug!(message = ?msg, "received fetch");
				let gate = this.fetch_gate(&msg);
				// Owned, since the task outlives the body it was read from.
				let msg = msg.into_owned();
				async move {
					let _serve = serve;
					if let Err(err) = this.serve_fetch_stream(stream, msg, gate).await {
						tracing::debug!(%err, "fetch stream error");
					}
				}
				.boxed()
			}
			ietf::SubscribeNamespace::ID => {
				let msg = match ietf::SubscribeNamespace::decode_msg(&mut data, this.version) {
					Err(err) if super::token::refuses_request(&err) => {
						// Junk after the fields is fatal, which a token refusal must not hide.
						if !data.is_empty() {
							return Err(Error::WrongSize);
						}
						let kind = request::Kind::SubscribeNamespace;
						return Ok(this.reject_token(stream, kind, err, serve));
					}
					decoded => decoded?,
				};
				if !data.is_empty() {
					return Err(Error::WrongSize);
				}
				tracing::debug!(message = ?msg, "received subscribe_namespace");
				let gate = crate::auth::Gate::new(
					this.auth.clone(),
					msg.namespace.clone(),
					crate::auth::Direction::Publish,
				);
				async move {
					let session = this.session.clone();
					match this.serve_subscribe_namespace_stream(stream, msg, gate).await {
						// Such as a malformed answer to an advertisement this request solicited.
						Err(err) if super::subscriber::is_protocol_violation(&err) => session.close(
							super::error::to_session_code(&crate::SessionError::from(&err)),
							&err.to_string(),
						),
						Err(err) => tracing::debug!(%err, "subscribe_namespace stream error"),
						Ok(()) => {}
					}
				}
				.boxed()
			}
			ietf::SubscribeTracks::ID => {
				let msg = match ietf::SubscribeTracks::decode_msg(&mut data, this.version) {
					Err(err) if super::token::refuses_request(&err) => {
						// Junk after the fields is fatal, which a token refusal must not hide.
						if !data.is_empty() {
							return Err(Error::WrongSize);
						}
						let kind = request::Kind::SubscribeNamespace;
						return Ok(this.reject_token(stream, kind, err, serve));
					}
					msg => msg?,
				};
				if !data.is_empty() {
					return Err(Error::WrongSize);
				}
				tracing::debug!(message = ?msg, "received subscribe_tracks");
				// We list no tracks by PUBLISH, so each listing is refused on its own and the
				// session stays up. Decoded first, so a refusal still admits its Request ID.
				let error =
					super::namespace::refusal(&msg.prefix, this.version).unwrap_or_else(|| ietf::RequestError {
						error_code: request::NOT_SUPPORTED,
						reason_phrase: "SUBSCRIBE_TRACKS is not supported".into(),
						retry_interval: 0,
					});
				async move {
					if let Err(err) = super::request_stream::reject(stream, error).await {
						tracing::debug!(%err, "subscribe_tracks refusal failed");
					}
				}
				.boxed()
			}
			ietf::TrackStatus::ID => {
				let msg = match ietf::TrackStatus::decode_msg(&mut data, this.version) {
					Err(err) if super::token::refuses_request(&err) => {
						// Junk after the fields is fatal, which a token refusal must not hide.
						if !data.is_empty() {
							return Err(Error::WrongSize);
						}
						return Ok(this.reject_token(stream, request::Kind::TrackStatus, err, serve));
					}
					msg => msg?,
				};
				if !data.is_empty() {
					return Err(Error::WrongSize);
				}
				if msg.epoch.is_some() && !epochs {
					return Err(Error::ProtocolViolation);
				}
				tracing::debug!(message = ?msg, "received track_status");
				let gate = crate::auth::Gate::new(
					this.auth.clone(),
					msg.track_namespace.clone(),
					crate::auth::Direction::Publish,
				);
				// Owned, since the task outlives the body it was read from.
				let msg = msg.into_owned();
				async move {
					if let Err(err) = this.serve_track_status_stream(stream, msg, gate).await {
						tracing::debug!(%err, "track_status stream error");
					}
				}
				.boxed()
			}
			_ => {
				tracing::warn!(id, "unexpected bidi stream type for publisher");
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
		serve: Option<Serve>,
	) -> BoxFuture<'static, ()> {
		let version = self.version;
		async move {
			let _serve = serve;
			if let Err(err) = super::token::reject(stream, kind, err, version).await {
				tracing::debug!(%err, "token refusal failed");
			}
		}
		.boxed()
	}

	/// Handle a SUBSCRIBE on its bidi stream.
	#[cfg(test)]
	fn run_subscribe_stream(
		self,
		stream: Stream<Transport, Version>,
		msg: ietf::Subscribe<'static>,
	) -> impl std::future::Future<Output = Result<(), Error>> {
		let gate = crate::auth::Gate::new(
			self.auth.clone(),
			msg.track_namespace.clone(),
			crate::auth::Direction::Publish,
		);
		self.serve_subscribe_stream(stream, msg, gate)
	}

	fn serve_subscribe_stream(
		self,
		mut stream: Stream<Transport, Version>,
		mut msg: ietf::Subscribe<'static>,
		gate: crate::auth::Gate,
	) -> impl std::future::Future<Output = Result<(), Error>> {
		// Register during dispatch, before either request task can be polled.
		let join = (!Filter::is_draft20(self.version)).then(|| self.register_join(msg.request_id));
		async move {
			let _join = join;
			if let Some(error) = super::namespace::refusal(&msg.track_namespace, self.version) {
				return super::request_stream::reject(stream, error).await;
			}
			let request_id = msg.request_id;
			let track_name = msg.track_name.clone();
			// Logged beside the root rather than joined, so a namespace too long under it is
			// refused by the lookup, on this request alone.
			let (root, namespace) = (self.origin.root().clone(), msg.track_namespace.clone());
			let broadcast = || crate::path::Rooted(&root, &namespace);
			tracing::debug!(id = %request_id, broadcast = %broadcast(), track = %track_name, "subscribe started");

			// Serve only what our grant lets us publish (MoQ Auth), and stop once it no
			// longer does. Checked before resolving, so a denied request never reaches the
			// origin. Our own authorization of the peer (the limit) admits a request without a
			// token; an AUTHORIZATION TOKEN on the request goes to the application to verify for
			// this request alone (MoQ request-token), even where the limit covers it. What the
			// peer granted us bounds what we send either way, and never stands in for our
			// authorization of it.
			let token = match &msg.authorization_token {
				Some(token)
					if self
						.auth
						.within_grant(crate::auth::Direction::Publish, msg.track_namespace.as_str())
						&& gate.token_decides() =>
				{
					Some(token.0.clone())
				}
				_ => None,
			};
			if token.is_none() && !gate.allows() {
				let err = Error::Unauthorized;
				return self.reject_subscribe(stream, &err, "not granted").await;
			}
			// Legal requests we can't honor are refused one at a time, never by closing the
			// session. Serving a Range Filter unfiltered would deliver objects the subscriber
			// excluded.
			if msg.range_filters {
				return super::request_stream::reject(
					stream,
					ietf::RequestError {
						error_code: request::INVALID_FILTER,
						reason_phrase: "Range Filters exceed MAX_FILTER_RANGES=0".into(),
						retry_interval: 0,
					},
				)
				.await;
			}
			// Held for the life of the subscription, and by every token verification the
			// application takes for it, initial or renewal, even past a cancel. The request keeps
			// its stream, and so its stream credit or request window slot, until all of them let
			// go, so a peer cancelling and resubscribing cannot leave more verifications in flight
			// than its credit admits.
			let credit = kio::Producer::<()>::default();
			let slot: crate::auth::Hold = std::sync::Arc::new((self.subscriptions.hold(), credit.consume()));

			let mut request_finished = false;
			let mut goaways = super::request_stream::Messages::new(self.session.clone(), self.control.client());
			// A token-authorized subscription is held to its own grant, which ends it when the
			// grant lapses, is revoked, or stops covering it. Every subscription is gated so it
			// ends once what the peer granted us stops covering it, and a limit-authorized one
			// once the limit does too.
			let (mut gate, request_grant) = match token {
				Some(token) => {
					let verdict = gate.verify(token, Some(slot.clone()));
					// The subscriber cancelling or the session closing withdraws the token.
					let verified = self
						.until_withdrawn(&mut stream, |waiter| verdict.poll_grant(waiter))
						.await;
					let Some(verified) = verified else {
						drop((verdict, slot));
						self.released(&credit).await;
						return Ok(());
					};
					let admitted = crate::auth::RequestGrant::new(
						&self.runtime,
						verdict,
						verified,
						msg.track_namespace.clone(),
						crate::auth::RequestKind::Subscribe,
						Some(slot.clone()),
					);
					match admitted {
						Ok(grant) => (gate.named(), Some(grant)),
						Err(refusal) => {
							return self.reject_subscribe(stream, &refusal.error, &refusal.reason).await;
						}
					}
				}
				None => (gate, None),
			};
			let _slot = slot;

			// Everything the subscription does once admitted, its setup included, runs under its
			// authority: a grant that ends or a gate that closes at any await drops it right there,
			// releasing what it holds upstream, and only then is the peer told. `acked` holds the
			// group stream count once SUBSCRIBE_OK is buffered, which decides how.
			let mut acked: Option<Arc<AtomicU64>> = None;
			// An instance never comes back, so a request pinned to a gone one is missing, not
			// worth waiting for.
			let rendezvous = msg.rendezvous && msg.epoch.is_none();
			let subscribed = {
				let serve = async {
					// Stats (subscriptions, viewer refcount, groups/frames/bytes) are counted in
					// the model, through the tagged `origin::Consumer` the broadcast resolves from.

					let refused = |err: Error, resolved| {
						Ok(Subscribed::Refused(
							err.to_string(),
							rendezvous_error(err, rendezvous, resolved),
						))
					};
					// We just received a subscribe for this exact namespace, so the peer must have
					// already seen the announcement. The immediate lookup resolves it at once, or
					// falls back to the route covering it (an `origin::Requests`), if any. Pinned to
					// the instance it names, if any: another is refused, never served.
					let broadcast = match self.resolve(&mut stream, &msg.track_namespace, msg.epoch.clone()).await {
						Ok(Some(broadcast)) => broadcast,
						// The reason is the origin's, not ours: a dynamic router refusing on
						// authorization or a drain says so, and only an unroutable path means the
						// broadcast is not here.
						Err(err) => return refused(err, false),
						Ok(None) => return Ok(Subscribed::Withdrawn),
					};
					let track = match broadcast.track(&msg.track_name) {
						Ok(track) => track,
						Err(err) => return refused(err, true),
					};
					let (mut track_serve, edge) = match self.subscribe_track(&mut stream, &track, &msg).await {
						Some(Ok(subscribed)) => subscribed,
						Some(Err(err)) => return refused(err, true),
						None => return Ok(Subscribed::Withdrawn),
					};
					let cache = track;
					// A subscriber that opted out of this SUBSCRIBE_OK's properties learns the
					// Timestamp's units from TRACK_STATUS, so the opt-out strips nothing from the
					// objects.
					let timescale = track_serve.timescale;
					let properties = match track_properties(
						track_serve.track.info(),
						msg.properties_wanted,
						self.version,
						self.peer_setup.get().await.flate,
					) {
						Ok(properties) => properties,
						Err(err) => return Ok(Subscribed::Refused(err.to_string(), err)),
					};
					// Draft-22 replaced joining FETCH with subscription fills. Older drafts save
					// the same boundary used by the subscription so the two streams never overlap,
					// again on every resume.
					let filter = msg.filter;
					let namespace = msg.track_namespace.clone();
					let grant = request_grant.clone();
					let save_join = |largest: Option<Location>,
					                 cache: &crate::track::Consumer,
					                 ranking: &Ranking<GroupOf<Transport>>| {
						if let Some(join) = &_join {
							let joined = match (filter, largest) {
								(Filter::NextObject, Some(largest)) => Joined::Group {
									end: Location {
										group: largest.group,
										object: largest.object + 1,
									},
									cache: cache.clone(),
									namespace: namespace.clone(),
									grant: grant.clone(),
									ranking: ranking.clone(),
								},
								(Filter::NextObject, None) => Joined::Empty,
								_ => Joined::Unsupported,
							};
							join.joins.lock().insert(request_id, Some(joined));
						}
					};
					save_join(edge.largest, &cache, &track_serve.ranking);

					// A fill reads the group cache through its own consumer, independent of the
					// subscription's cursor.
					let fill = msg
						.fill
						.filter(|_| msg.forward && Filter::is_draft20(self.version))
						.map(|fill| (fill_range(fill, msg.filter, edge.largest), cache, timescale));

					// Buffered whole before its first await, so however the subscription is
					// dropped from here, the request can still end on the wire.
					acked = Some(track_serve.opened.clone());
					stream
						.writer
						.encode_message(&ietf::SubscribeOk {
							track_alias: request_id.0,
							// The subscription floor and FETCH/FILL cap use this same snapshot.
							largest: edge.largest,
							properties,
						})
						.await?;

					// The fill (when one was requested) runs alongside on its own fetch stream;
					// its failures reset that stream and never touch the subscription. Every
					// subscription a resume makes counts toward the same PUBLISH_DONE.
					let opened = track_serve.opened.clone();
					let ranking = track_serve.ranking.clone();
					let fill = async {
						if let Some((fill, cache, timescale)) = fill {
							self.run_fill(fill, &ranking, cache, timescale, &opened, ranking.request_id)
								.await;
						}
					};
					let mut served = match msg.forward {
						true => {
							self.run_subscription(
								&mut stream,
								&mut track_serve,
								&mut request_finished,
								&mut goaways,
								fill,
								request_grant.clone(),
							)
							.await
						}
						false => {
							// Unpolled, it would still hold the track it serves from.
							drop(fill);
							Some(Served::Paused(false))
						}
					};
					// A pause drops the subscription, and its demand with it; a resume subscribes
					// again as a new SUBSCRIBE would, so nothing carries across but the request.
					while let Some(Served::Paused(answer)) = served {
						// What an update changed while forwarding outlives the pause.
						let applied = track_serve.track.subscription();
						msg.subscriber_priority = super::priority::to_wire(applied.priority);
						msg.filter = track_serve.filter;
						msg.max_age = applied.max_delay.and_then(|delay| delay.as_millis().try_into().ok());
						track_serve.close_groups();
						self.pause_join(_join.as_ref());
						drop(track_serve);
						if answer {
							self.write_request_ok(&mut stream.writer, None).await?;
						}
						match self
							.wait_resume(&mut stream, &mut request_finished, &mut goaways, &mut msg)
							.await
						{
							Ok(()) => {}
							Err(served) => {
								let served = served.unwrap_or(Served::Track(Ok(())));
								return Ok(Subscribed::Served(served, opened.load(Ordering::Relaxed)));
							}
						}
						let resumed = match broadcast.track(&msg.track_name) {
							Ok(track) => self
								.subscribe_track(&mut stream, &track, &msg)
								.await
								.map(|subscribed| subscribed.map(|(serve, edge)| (serve, edge, track))),
							Err(err) => Some(Err(err)),
						};
						// Withdrawn mid-setup, it ends as it would have while paused.
						let Some(resumed) = resumed else {
							return Ok(Subscribed::Served(
								Served::Track(Ok(())),
								opened.load(Ordering::Relaxed),
							));
						};
						let (edge, track);
						(track_serve, edge, track) = match resumed {
							Ok(resumed) => resumed,
							// The resuming update is owed an answer, so it fails.
							Err(err) => {
								self.write_subscribe_error(&mut stream.writer, &err, "resume failed")
									.await?;
								return Ok(Subscribed::Served(
									Served::UpdateFailed(err),
									opened.load(Ordering::Relaxed),
								));
							}
						};
						track_serve.opened = opened.clone();
						save_join(edge.largest, &track, &track_serve.ranking);
						self.write_request_ok(&mut stream.writer, edge.largest).await?;
						served = self
							.run_subscription(
								&mut stream,
								&mut track_serve,
								&mut request_finished,
								&mut goaways,
								async {},
								request_grant.clone(),
							)
							.await;
					}

					let completed = served.is_some();
					let served = served.unwrap_or(Served::Track(Ok(())));

					if completed && matches!(served, Served::Track(Ok(()))) {
						self.write_end_of_track(&mut stream, &mut track_serve, &mut request_finished, &mut goaways)
							.await;
					}

					// PUBLISH_DONE must follow the close of every data stream. A subscription that
					// completed has drained its groups; one that ended early resets the rest here.
					// The fill stream closed when `run_subscription` dropped it.
					track_serve.close_groups();
					Ok::<_, Error>(Subscribed::Served(served, track_serve.opened()))
				};
				crate::auth::until(serve, |waiter| {
					if let Some(grant) = &request_grant
						&& let Poll::Ready(refusal) = grant.poll_ended(waiter)
					{
						// PUBLISH_DONE carries a fixed reason per status, so only the code reaches
						// the peer once SUBSCRIBE_OK went out.
						tracing::info!(broadcast = %broadcast(), track = %track_name, err = %refusal.error, reason = %refusal.reason, "request token grant ended");
						return Poll::Ready(refusal);
					}
					if gate.poll_denied(waiter).is_ready() {
						tracing::info!(broadcast = %broadcast(), track = %track_name, "subscription no longer authorized");
						return Poll::Ready(Error::Unauthorized.into());
					}
					Poll::Pending
				})
				.await
			};
			// A FETCH joining this subscription is refused if it has not answered yet.
			drop(_join);
			// The subscription's demand is gone; only its credit may still wait on a verification.
			drop((request_grant, _slot));

			let (served, streams) = match subscribed {
				Ok(Ok(Subscribed::Served(Served::RenewalRefused(refusal), streams))) => {
					let res = self
						.write_subscribe_error(&mut stream.writer, &refusal.error, &refusal.reason)
						.await;
					match res {
						Ok(()) => (Served::UpdateFailed(refusal.error), streams),
						Err(err) => (Served::Track(Err(err)), streams),
					}
				}
				Ok(Ok(Subscribed::Served(served, streams))) => (served, streams),
				Ok(Ok(Subscribed::Refused(reason, err))) => {
					return self.reject_subscribe(stream, &err, &reason).await;
				}
				// Nobody is left to answer.
				Ok(Ok(Subscribed::Withdrawn)) => {
					self.released(&credit).await;
					return Ok(());
				}
				Ok(Err(err)) => return Err(err),
				Err(refusal) => match acked {
					Some(opened) => (Served::Track(Err(refusal.error)), opened.load(Ordering::Relaxed)),
					None => {
						return self.reject_subscribe(stream, &refusal.error, &refusal.reason).await;
					}
				},
			};
			self.write_publish_done(stream, served, streams, Some(&credit)).await
		}
	}

	/// Ready once the requester withdraws the request on `stream` or `session` closes, without
	/// reading anything it sent meanwhile.
	fn poll_withdrawn(
		&self,
		session: &mut Transport,
		stream: &mut Stream<Transport, Version>,
		waiter: &kio::Waiter,
	) -> Poll<()> {
		if session.poll_closed(waiter).is_ready() {
			return Poll::Ready(());
		}
		super::request_stream::poll_withdrawn(stream, self.version, waiter)
	}

	/// Poll `step` of a request's setup until ready, or `None` once [`Self::poll_withdrawn`] is.
	async fn until_withdrawn<T>(
		&self,
		stream: &mut Stream<Transport, Version>,
		mut step: impl FnMut(&kio::Waiter) -> Poll<T>,
	) -> Option<T> {
		let mut session = self.session.clone();
		kio::wait(|waiter| {
			if self.poll_withdrawn(&mut session, stream, waiter).is_ready() {
				return Poll::Ready(None);
			}
			step(waiter).map(Some)
		})
		.await
	}

	/// Wait until no token verification holds `credit`'s request any more, or the session closes.
	async fn released(&self, credit: &kio::Producer<()>) {
		let session = self.session.clone();
		kio::wait(|waiter| {
			if session.poll_closed(waiter).is_ready() || credit.poll_wanted_changed(waiter, true).is_ready() {
				return Poll::Ready(());
			}
			Poll::Pending
		})
		.await
	}

	/// Send the END_OF_TRACK a subscription that ran to the track's end still owes:
	/// PUBLISH_DONE carries no end location, so the marker is what tells the subscriber
	/// where the track ended. One cancelled while the marker waits for stream credit abandons
	/// it.
	async fn write_end_of_track(
		&self,
		stream: &mut Stream<Transport, Version>,
		track_serve: &mut TrackServe,
		request_finished: &mut bool,
		goaways: &mut super::request_stream::Messages,
	) {
		let Some(end) = track_serve.end() else {
			return;
		};
		let request_id = track_serve.request_id;
		let mut marker = std::pin::pin!(track_serve.write_end_of_track(end));
		let closed_session = self.session.clone();
		let written = kio::wait(|waiter| {
			if let Poll::Ready(res) = waiter.poll_future(marker.as_mut()) {
				return Poll::Ready(res);
			}
			if super::request_stream::poll_cancel(stream, request_finished, goaways, self.version, waiter).is_ready()
				|| closed_session.poll_closed(waiter).is_ready()
			{
				return Poll::Ready(Err(Error::Cancel));
			}
			Poll::Pending
		})
		.await;
		// A failure only costs the subscriber the early boundary.
		if let Err(err) = written {
			tracing::debug!(%err, id = %request_id, "end of track failed");
		}
	}

	/// End a subscription on the wire with the PUBLISH_DONE `served` reports, after `streams`
	/// group streams, returning how serving ended. The stream closes only once no verification
	/// holds `credit`'s request, if it has one.
	async fn write_publish_done(
		&self,
		mut stream: Stream<Transport, Version>,
		served: Served,
		streams: u64,
		credit: Option<&kio::Producer<()>>,
	) -> Result<(), Error> {
		let (status, reason) = served.status();
		let res = match served {
			Served::Track(res) => res,
			Served::UpdateFailed(err) => Err(err),
			Served::RenewalRefused(refusal) => Err(refusal.error),
			Served::Paused(_) => Ok(()),
		};

		let _ = stream
			.writer
			.encode_message(&ietf::PublishDone {
				status_code: status.code(),
				stream_count: streams,
				reason_phrase: reason.into(),
			})
			.await;

		// The peer learns the subscription ended at once, but the stream holds its credit until
		// the application lets go of every token it took for it.
		if let Some(credit) = credit {
			self.released(credit).await;
		}
		// PUBLISH_DONE is the last thing on this stream, so it needs the acknowledgement too.
		let _ = stream.writer.close().await;

		res
	}

	async fn run_subscription(
		&self,
		stream: &mut Stream<Transport, Version>,
		serve: &mut TrackServe,
		finished: &mut bool,
		goaways: &mut super::request_stream::Messages,
		fill: impl std::future::Future<Output = ()>,
		request_grant: Option<crate::auth::RequestGrant>,
	) -> Option<Served> {
		let session = self.session.clone();
		let mut fill = std::pin::pin!(fill);
		let mut fill_done = false;
		let mut served = None;
		let mut fills = crate::util::TaskSet::owned();
		let mut fills_done = true;
		let mut waiting: Option<super::request_stream::Update> = None;
		let mut renewal_largest = None;

		// Renewal state carried across turns of the loop. After SUBSCRIBE_OK the peer may send a
		// REQUEST_UPDATE refreshing the request's token. A token-bearing update's verify is raced
		// against serving (never a bare await), so media keeps flowing while the acceptor is
		// slow; the caller's guard still ends the request when the old grant does. While a verdict is
		// pending the loop keeps reading so a cancellation still ends the request, stashing every
		// further update, token or not, to handle once the verdict resolves: answers
		// carry no Request ID, so they must go out in the order the updates arrived. The stash is
		// a FIFO bounded by MAX_REQUEST_UPDATES (counting the one being verified).
		let mut renewal = request_update::Renewal::<super::request_stream::Update>::default();

		// What one turn of the loop resolved to.
		enum Turn {
			// The track and any fill finished, or the stream failed.
			Served(Result<(), Error>),
			// Reading the requester's next update failed.
			Malformed(Error),
			// The peer cancelled, or the session ended.
			Cancelled,
			// A REQUEST_UPDATE, already decoded.
			Update(super::request_stream::Update),
			// A pending renewal's verdict resolved, for the update with this request id.
			Renewal(request_update::Verdict),
		}

		// Each turn may apply a REQUEST_UPDATE, so a peer streaming them must not hold the
		// session's other tasks.
		let mut budget = kio::coop::Budget::new(32);
		loop {
			kio::wait(|waiter| budget.poll_yield(waiter)).await;
			let mut queued = waiting.is_none().then(|| renewal.next()).flatten();

			let turn = kio::wait(|waiter| {
				if session.poll_closed(waiter).is_ready() {
					return Poll::Ready(Turn::Cancelled);
				}
				if stream.writer.poll_closed(waiter).is_ready() {
					return Poll::Ready(Turn::Cancelled);
				}
				if let Poll::Ready(renewed) = renewal.poll(waiter) {
					return Poll::Ready(Turn::Renewal(renewed));
				}
				if !fill_done && waiter.poll_future(fill.as_mut()).is_ready() {
					fill_done = true;
				}
				if !fills_done && fills.poll(waiter).is_ready() {
					fills_done = true;
				}
				if fill_done
					&& fills_done && serve.tail.is_none()
					&& serve.opening.is_none()
					&& let Some(update) = waiting.take()
				{
					return Poll::Ready(Turn::Update(update));
				}
				if let Some(update) = queued.take() {
					return Poll::Ready(Turn::Update(update));
				}
				if waiting.is_none() {
					match self.poll_update(stream, finished, goaways, waiter) {
						Poll::Ready(UpdateRead::Update(update)) => return Poll::Ready(Turn::Update(update)),
						Poll::Ready(UpdateRead::Cancelled) => return Poll::Ready(Turn::Cancelled),
						Poll::Ready(UpdateRead::Malformed(err)) => return Poll::Ready(Turn::Malformed(err)),
						Poll::Ready(UpdateRead::Failed(err)) => return Poll::Ready(Turn::Served(Err(err))),
						Poll::Pending => {}
					}
				}

				if served.is_none()
					&& let Poll::Ready(done) = serve.poll(waiter)
				{
					served = Some(done);
				}
				if fill_done
					&& fills_done && serve.tail.is_none()
					&& serve.opening.is_none()
					&& let Some(update) = waiting.take()
				{
					return Poll::Ready(Turn::Update(update));
				}
				match (&served, fill_done && fills_done && waiting.is_none()) {
					(Some(Ok(())), true) => Poll::Ready(Turn::Served(Ok(()))),
					(Some(Err(err)), true) => Poll::Ready(Turn::Served(Err(err.clone()))),
					_ => Poll::Pending,
				}
			})
			.await;

			match turn {
				// A malformed update, such as one whose AUTHORIZATION TOKEN names an alias,
				// closes the session rather than only ending this subscription, as the same
				// token on the request itself would. A source track's decode failure is not
				// this requester's fault, so it reaches Served and ends only this subscription.
				Turn::Malformed(err) => {
					if super::subscriber::is_protocol_violation(&err) {
						self.session.clone().close(
							super::error::to_session_code(&crate::SessionError::from(&err)),
							&err.to_string(),
						);
					}
					return Some(Served::Track(Err(err)));
				}
				Turn::Served(res) => return Some(Served::Track(res)),
				Turn::Cancelled => return None,
				Turn::Renewal(request_update::Verdict { verdict, grant: res }) => {
					// Only a token-authorized request starts a renewal.
					let rg = request_grant.as_ref().expect("a renewal without a request grant");
					// On accept the old grant is dropped (ending the old token) and the deadline
					// re-armed at the new expiry.
					let Err(refusal) = rg.renew(verdict, res) else {
						if let Err(err) = self.write_request_ok(&mut stream.writer, renewal_largest.take()).await {
							return Some(Served::Track(Err(err)));
						}
						continue;
					};
					// Per draft-18 section 10.9.1, a refused or uncovered
					// renewal ends only this subscription: REQUEST_ERROR, then PUBLISH_DONE
					// UPDATE_FAILED. The session stays up and the old grant does not survive,
					// so a FETCH joined to it stops at once rather than finishing under it.
					rg.end(refusal.clone());
					return Some(Served::RenewalRefused(refusal));
				}
				// An update behind a pending verdict waits its turn, so its answer follows the
				// verdict's.
				Turn::Update(update) if renewal.pending() => match renewal.queue(update, self.version) {
					Ok(()) => {}
					Err(request_update::Overflow::Session) => {
						return Some(Served::Track(Err(request_update::too_many(&mut self.session.clone()))));
					}
					// Below draft 22 no limit is negotiated, so this is a local memory guard: end
					// this request, never the session.
					Err(request_update::Overflow::Request) => {
						return Some(Served::Track(Err(Error::ProtocolViolation)));
					}
				},
				Turn::Update(update)
					if update.fill.is_some()
						&& (!fill_done || !fills_done || serve.tail.is_some() || serve.opening.is_some()) =>
				{
					waiting = Some(update);
				}
				Turn::Update(update) => {
					match self
						.apply_subscription_update(stream, serve, &request_grant, &mut renewal, update)
						.await
					{
						Err(served) => return Some(served),
						Ok(Some(fill)) => {
							if renewal.pending() {
								renewal_largest = fill.largest;
							}
							let publisher = self.clone();
							fills.push(async move {
								publisher
									.run_fill(
										fill.fill,
										&fill.ranking,
										fill.track,
										fill.timescale,
										&fill.opened,
										fill.request_id,
									)
									.await;
							});
							fills_done = false;
						}
						Ok(None) => {}
					}
				}
			}
		}
	}

	/// Subscribe to `track` as `msg` asks, resolving its filter at the live edge, as a
	/// SUBSCRIBE does and each resume of one does again. `None` once the subscriber withdraws
	/// the request on `stream`, since the producer may never answer.
	async fn subscribe_track(
		&self,
		stream: &mut Stream<Transport, Version>,
		track: &crate::track::Consumer,
		msg: &ietf::Subscribe<'_>,
	) -> Option<Result<(TrackServe, LiveEdge), Error>> {
		let declared = self
			.until_withdrawn(stream, |waiter| self.peer_setup.poll_get(waiter))
			.await?;
		// A live SUBSCRIBE's NEW_GROUP_REQUEST asks nothing more: subscribing is itself a join,
		// which the track's refresh ratio judges.
		let mut subscription = serving_subscription(msg.subscriber_priority).with_order(msg.group_order.to_order());
		if let Some(max_age) = msg.max_age {
			subscription.max_delay = Some(Duration::from_millis(max_age));
		}
		// A range that resolves without the live edge has a fixed start, which is no join:
		// bound the subscription before subscribing so the ratio never judges it. From the
		// start of its group, so a relay's copy holds the group's head for its other readers,
		// as a live join would; the resolved range trims below.
		let fixed = subscribe_range(msg.filter, LiveEdge::default(), self.version);
		if let Some(start) = fixed.start {
			ServeRange {
				start: Some(Location { object: 0, ..start }),
				..fixed
			}
			.apply(&mut subscription);
		}

		// Subscribe before resolving the filter: on a routed broadcast the live edge only
		// becomes readable once the subscription's demand attaches a route, so the edge snapshot
		// has to come after. The resolved range is applied to the preference right below, before
		// anything is served.
		let subscribing = track.subscribe(subscription.clone());
		let mut subscribed = match self
			.until_withdrawn(stream, |waiter| subscribing.poll_ok(waiter))
			.await?
		{
			Ok(subscribed) => subscribed,
			Err(err) => return Some(Err(err)),
		};
		drop(subscribing);

		// A relay's copy that went idle cannot say where its live edge is until its route
		// answers again; answering from its cache would advertise a stale one.
		self.until_withdrawn(stream, |waiter| subscribed.poll_live(waiter))
			.await?;

		// The filter and any fill are relative to the live edge, so snapshot it once: the fill
		// ends exactly where a Next Object subscription begins, which is what lets the draft's
		// current-group join (Next Object plus a StartGroup=1 fill) cover the group with no gap
		// and no overlap.
		let edge = live_edge(track);
		let mut range = subscribe_range(msg.filter, edge, self.version);
		let fill = msg
			.fill
			.filter(|_| msg.forward && Filter::is_draft20(self.version))
			.map(|fill| fill_range(fill, msg.filter, edge.largest));
		if let Some(next) = range.fill_tail(fill, edge) {
			range.start = Some(next);
		}
		range.apply(&mut subscription);
		let _ = subscribed.update(subscription);
		// A Timestamp goes out wherever the track has units. An untimed track declares none,
		// so its objects stay unstamped.
		let timescale = subscribed.info().timescale;

		// Serve the track while reading updates; only abrupt closure cancels on draft-22. A
		// fill or joining FETCH ranks in the same subscription as the group it carries.
		let serve = TrackServe::new(
			self.session.clone(),
			subscribed,
			msg.request_id,
			self.version,
			range,
			timescale,
		)
		.compression(declared.flate)
		.ranked(self.priority.clone());
		let mut serve = serve;
		serve.filter = msg.filter;
		// A fixed start is no join, so its NEW_GROUP_REQUEST asks as an update's would.
		if fixed.start.is_some()
			&& let Some(value) = msg.new_group
		{
			serve.request_new_group(value);
		}
		Some(Ok((serve, edge)))
	}

	/// Hold a paused SUBSCRIBE, answering its updates, until one resumes it, or return how the
	/// request ended (`None` when cancelled).
	///
	/// A paused subscription holds nothing upstream, so only its priority and max age change
	/// while it waits; anything else fails the update, ending the request.
	async fn wait_resume(
		&self,
		stream: &mut Stream<Transport, Version>,
		finished: &mut bool,
		goaways: &mut super::request_stream::Messages,
		msg: &mut ietf::Subscribe<'_>,
	) -> Result<(), Option<Served>> {
		let session = self.session.clone();
		// Each update is answered, so a peer streaming them must not hold the session's
		// other tasks.
		let mut budget = kio::coop::Budget::new(32);
		loop {
			kio::wait(|waiter| budget.poll_yield(waiter)).await;
			let read = kio::wait(|waiter| {
				if session.poll_closed(waiter).is_ready() {
					return Poll::Ready(UpdateRead::Cancelled);
				}
				if stream.writer.poll_closed(waiter).is_ready() {
					return Poll::Ready(UpdateRead::Cancelled);
				}
				self.poll_update(stream, finished, goaways, waiter)
			})
			.await;
			let update = match read {
				UpdateRead::Update(update) => update,
				UpdateRead::Cancelled => return Err(None),
				UpdateRead::Malformed(err) => {
					if super::subscriber::is_protocol_violation(&err) {
						self.session.clone().close(
							super::error::to_session_code(&crate::SessionError::from(&err)),
							&err.to_string(),
						);
					}
					return Err(Some(Served::Track(Err(err))));
				}
				UpdateRead::Failed(err) => return Err(Some(Served::Track(Err(err)))),
			};
			let refused = match update.refused {
				Some(err) => Some(err),
				None if update.filter.is_some()
					|| update.fill.is_some()
					|| update.new_group.is_some()
					|| update.authorization_token.is_some() =>
				{
					Some(Error::Unsupported)
				}
				None => None,
			};
			if let Some(err) = refused {
				self.write_subscribe_error(
					&mut stream.writer,
					&err,
					"REQUEST_UPDATE parameters not supported while paused",
				)
				.await
				.map_err(|err| Some(Served::Track(Err(err))))?;
				return Err(Some(Served::UpdateFailed(Error::Unsupported)));
			}
			if let Some(priority) = update.priority {
				msg.subscriber_priority = priority;
			}
			if let Some(max_age) = update.max_age {
				msg.max_age = Some(max_age);
			}
			if update.forward == Some(true) {
				return Ok(());
			}
			self.write_request_ok(&mut stream.writer, None)
				.await
				.map_err(|err| Some(Served::Track(Err(err))))?;
		}
	}

	/// Read the requester's next REQUEST_UPDATE on its request stream, admitting its
	/// Request ID. Pending once the requester finished sending where a FIN does not cancel.
	fn poll_update(
		&self,
		stream: &mut Stream<Transport, Version>,
		finished: &mut bool,
		goaways: &mut super::request_stream::Messages,
		waiter: &kio::Waiter,
	) -> Poll<UpdateRead> {
		use super::request_stream::{Next, Update};
		while !*finished {
			match ready!(stream.reader.poll_decode_maybe::<Next<Update>>(waiter)) {
				Ok(Some(Next::Message(mut update))) => {
					if let Err(err) = self.control.accept(update.request_id) {
						if matches!(err, Error::GoingAway) {
							update.refused = Some(err);
							return Poll::Ready(UpdateRead::Update(update));
						}
						self.session.clone().close(
							super::error::to_session_code(&crate::SessionError::from(&err)),
							"invalid request ID",
						);
						return Poll::Ready(UpdateRead::Failed(err));
					}
					return Poll::Ready(UpdateRead::Update(update));
				}
				Ok(Some(Next::GoAway { redirect })) => {
					if let Err(err) = goaways.receive(redirect) {
						return Poll::Ready(UpdateRead::Malformed(err));
					}
				}
				Ok(None) if !super::request_stream::fin_cancels(self.version) => *finished = true,
				Ok(None) => return Poll::Ready(UpdateRead::Cancelled),
				Err(err) => return Poll::Ready(UpdateRead::Malformed(err)),
			}
		}
		Poll::Pending
	}

	/// Apply one REQUEST_UPDATE on an in-flight subscription, with no verdict pending: refuse
	/// unsupported parameters, apply a priority, then start verifying a renewal's token, or
	/// answer at once. A token-less update, or any update on a union-authorized request (which
	/// holds no grant to renew), owes one answer so the peer's MAX_REQUEST_UPDATES credit is
	/// restored.
	///
	/// An error is how the request ends.
	async fn apply_subscription_update(
		&self,
		stream: &mut Stream<Transport, Version>,
		serve: &mut TrackServe,
		request_grant: &Option<crate::auth::RequestGrant>,
		renewal: &mut request_update::Renewal<super::request_stream::Update>,
		update: super::request_stream::Update,
	) -> Result<Option<SubscriptionFill>, Served> {
		let failed = |err| Served::Track(Err(err));
		// A token renews a token-authorized request. Where tokens decide, one arriving on a
		// request admitted without one is refused rather than taken unread, so every token this
		// side admits is verified.
		let refused = match update.refused.clone() {
			Some(Error::Decode(err)) => Some((super::token::reason(&err), Error::Decode(err))),
			Some(Error::GoingAway) => Some(("going away", Error::GoingAway)),
			Some(err) => Some(("REQUEST_UPDATE parameters not supported", err)),
			// A SUBSCRIBE serves the range it asked for. A subscription we initiated with
			// PUBLISH decodes those parameters itself.
			None if update.fill.is_some() && !Filter::is_draft20(self.version) => {
				Some(("REQUEST_UPDATE parameters not supported", Error::Unsupported))
			}
			None if update.authorization_token.is_some()
				&& request_grant.is_none()
				&& self.auth.token_decides(crate::auth::Direction::Publish) =>
			{
				Some((
					"AUTHORIZATION TOKEN on a request admitted without one",
					Error::Unsupported,
				))
			}
			// The draft forbids it on a track that did not advertise DYNAMIC_GROUPS.
			None if update.new_group.is_some() && !serve.dynamic => Some((
				"NEW_GROUP_REQUEST on a track without DYNAMIC_GROUPS",
				Error::Unsupported,
			)),
			// A paused subscription opens no groups, so one is asked for only as it resumes.
			None if update.new_group.is_some() && update.forward == Some(false) => {
				Some(("NEW_GROUP_REQUEST while pausing", Error::Unsupported))
			}
			// The pause is answered before a renewal could be verified, which would acknowledge
			// a token nobody checked.
			None if update.authorization_token.is_some() && update.forward == Some(false) => {
				Some(("AUTHORIZATION TOKEN while pausing", Error::Unsupported))
			}
			None => None,
		};
		if let Some((reason, err)) = refused {
			self.write_subscribe_error(&mut stream.writer, &err, reason)
				.await
				.map_err(failed)?;
			// The update failed, which ends the subscription.
			return Err(Served::UpdateFailed(Error::Unsupported));
		}
		let fill = if update.filter.is_some() || update.fill.is_some() {
			serve.update_range(&update).map_err(failed)?
		} else {
			None
		};
		let largest = fill.as_ref().and_then(|fill| fill.largest);
		if let Some(priority) = update.priority {
			serve
				.set_priority(super::priority::from_wire(priority))
				.map_err(failed)?;
		}
		if let Some(max_age) = update.max_age {
			let mut subscription = serve.track.subscription();
			subscription.max_delay = Some(Duration::from_millis(max_age));
			serve.track.update(subscription).map_err(failed)?;
		}
		if let Some(value) = update.new_group {
			serve.request_new_group(value);
		}
		// The caller stops forwarding, then answers.
		if update.forward == Some(false) {
			return Err(Served::Paused(true));
		}
		if let (Some(token), Some(grant)) = (update.authorization_token, request_grant) {
			renewal.verify(grant.verify_renewal(&self.auth, token.0));
			return Ok(fill);
		}
		self.write_request_ok(&mut stream.writer, largest)
			.await
			.map_err(failed)?;
		Ok(fill)
	}

	/// Answer a REQUEST_UPDATE with REQUEST_OK, carrying the `largest` Location a resumed
	/// subscription now starts from.
	async fn write_request_ok(
		&self,
		writer: &mut Writer<crate::transport::boxed::SendStream, Version>,
		largest: Option<Location>,
	) -> Result<(), Error> {
		writer.encode_message(&ietf::RequestOk { largest, active: None }).await
	}

	/// Reject a SUBSCRIBE, ending the request stream.
	///
	/// Takes the whole stream because delivering the error is the other half of the job:
	/// [`Writer`] resets on drop, and a reset discards data the peer has not acknowledged, so
	/// returning here without [`Writer::close`] leaves the subscriber waiting on a request we
	/// already refused.
	async fn reject_subscribe(
		&self,
		mut stream: Stream<Transport, Version>,
		err: &Error,
		reason: &str,
	) -> Result<(), Error> {
		self.write_subscribe_error(&mut stream.writer, err, reason).await?;

		// The peer dropping the stream once it has the rejection is a normal end, not our failure.
		let _ = stream.writer.close().await;
		Ok(())
	}

	/// Write a subscribe error on the bidi stream writer.
	async fn write_subscribe_error(
		&self,
		writer: &mut Writer<crate::transport::boxed::SendStream, Version>,
		err: &Error,
		reason: &str,
	) -> Result<(), Error> {
		super::request_stream::write_error(writer, request::Kind::Subscribe, err, reason, self.version).await
	}

	/// Serve the largest group's head on its own draft-22 fetch stream.
	///
	/// A fill is a promise once requested. An empty range opens no stream, but a range we
	/// cannot serve still opens one and resets it right after the FETCH_HEADER, the
	/// draft's fill-failure signal. Nothing here touches the subscription either way.
	///
	/// Counts its stream into `opened` once it opens, like a group stream, so
	/// PUBLISH_DONE's Stream Count includes it however the subscription ends. It ranks in
	/// its subscription's `ranking` as the group it fills.
	async fn run_fill(
		&self,
		fill: FillServe,
		ranking: &Ranking<GroupOf<Transport>>,
		track: track::Consumer,
		timescale: Option<Timescale>,
		opened: &AtomicU64,
		request_id: RequestId,
	) {
		let priority = *ranking.priority.read();
		let rank = match fill {
			FillServe::Empty => return,
			FillServe::Group { sequence, skip, .. } => Some(ranking.rank(Location {
				group: sequence,
				object: skip,
			})),
			// Only the refusal goes out on its stream, so it competes with nothing.
			FillServe::Unsupported => None,
		};

		let mut rank = rank;
		let options = rank.as_mut().map(Rank::options).unwrap_or_default();
		let session = self.session.clone();
		let stream = match session.open_uni(options).await {
			Ok(stream) => stream,
			Err(err) => {
				tracing::debug!(err = %Error::from_transport(err), fill = %request_id, "fill stream failed to open");
				return;
			}
		};
		opened.fetch_add(1, Ordering::Relaxed);
		let mut stream = Writer::new(Ranked::new(stream, rank), self.version);

		let res = async {
			stream.varint(FetchHeader::TYPE).await?;
			stream.encode(&FetchHeader { request_id }).await?;

			let FillServe::Group { sequence, skip, until } = fill else {
				return Err(Error::Unsupported);
			};
			let group = track
				.fetch_group(
					sequence,
					group::Fetch {
						priority,
						..Default::default()
					},
				)
				.await?;
			let group = if self.peer_setup.get().await.flate {
				group.compressed()
			} else {
				group
			};
			Self::write_fetch_group(
				&mut stream,
				group,
				sequence,
				GroupSlice { skip, until },
				timescale,
				self.version,
				&mut None,
			)
			.await?;
			Ok(())
		}
		.await;

		match res {
			Ok(()) => {
				if let Err(err) = settle(&mut stream).await {
					tracing::debug!(%err, fill = %request_id, "fill stream close failed");
				} else {
					tracing::debug!(fill = %request_id, "fill complete");
				}
			}
			Err(err) => {
				tracing::debug!(%err, fill = %request_id, "fill failed, resetting its stream");
				stream.abort(&err);
			}
		}
	}

	/// Write one group's frames in the negotiated draft's FETCH object layout.
	///
	/// The first object carries its absolute Group and Object IDs plus the priority;
	/// every later one inherits them and increments the Object ID, so only the
	/// properties (the timestamp) and the payload go on the wire. A fetch object has no
	/// status field.
	async fn write_fetch_group(
		stream: &mut Writer<Ranked<crate::transport::boxed::SendStream>, Version>,
		mut group: group::Consumer,
		sequence: u64,
		slice: GroupSlice,
		timescale: Option<Timescale>,
		version: Version,
		prior_group: &mut Option<u64>,
	) -> Result<(), Error> {
		let GroupSlice { skip, until } = slice;
		let mut index: u64 = 0;
		let mut prior = None;

		let mut buf = arrayvec::ArrayVec::<frame::Object, 8>::new();
		if group.wire().is_some_and(|wire| !wire.fetchable) {
			return Err(Error::Unsupported);
		}
		// A long cached group is written a slice per poll, so the session's other tasks still run.
		let mut budget = kio::coop::Budget::new(32);
		'serve: loop {
			kio::wait(|waiter| budget.poll_yield(waiter)).await;
			// The cap is the Largest Object snapshot: the group may keep growing, but
			// everything past the snapshot belongs to the subscription, not the fill.
			if until.is_some_and(|until| index >= until) {
				break;
			}

			let step = {
				let mut closed = std::pin::pin!(stream.closed());
				kio::wait(|waiter| {
					if waiter.poll_future(closed.as_mut()).is_ready() {
						return Poll::Ready(Err(Error::Cancel));
					}
					match group.poll_read_objects(waiter, &mut buf) {
						Poll::Pending => match group.poll_next_frame(waiter) {
							Poll::Ready(Ok(Some(frame))) => Poll::Ready(Ok(FillStep::Partial(frame))),
							Poll::Ready(Ok(None)) => Poll::Ready(Ok(FillStep::Done)),
							Poll::Ready(Err(err)) => Poll::Ready(Err(err)),
							Poll::Pending => Poll::Pending,
						},
						Poll::Ready(Ok(0)) => Poll::Ready(Ok(FillStep::Done)),
						Poll::Ready(Ok(_)) => Poll::Ready(Ok(FillStep::Batch)),
						Poll::Ready(Err(err)) => Poll::Ready(Err(err)),
					}
				})
				.await
			};

			match step? {
				FillStep::Batch => {
					for frame in &buf {
						if until.is_some_and(|until| index >= until) {
							break 'serve;
						}
						let frame = frame.clone();
						index = frame.fields.as_ref().map_or(index, |fields| fields.id);
						if until.is_some_and(|until| index >= until) {
							break 'serve;
						}
						if index >= skip {
							Self::write_fetch_object(
								stream,
								FetchObjectInfo {
									sequence,
									object: index,
									prior: fetch_position(*prior_group, sequence, prior, index)?,
									timestamp: frame.timestamp,
									inflated: frame.inflated,
									timescale,
									version,
									fields: frame.fields.as_deref(),
									source: group.ietf(),
								},
							)
							.await?;
							Self::write_fetch_payload(stream, frame.frame.payload).await?;
							prior = Some(index);
						}
						index += 1;
						group.keep_alive();
					}
				}
				FillStep::Partial(mut frame) => {
					index = frame.fields.as_ref().map_or(index, |fields| fields.id);
					if until.is_some_and(|until| index >= until) {
						break;
					}
					if index < skip {
						// A skipped frame still has to be drained to advance the cursor.
						loop {
							let chunk = {
								let mut closed = std::pin::pin!(stream.closed());
								kio::wait(|waiter| {
									if waiter.poll_future(closed.as_mut()).is_ready() {
										return Poll::Ready(Err(Error::Cancel));
									}
									frame.poll_read_chunk(waiter)
								})
								.await
							};
							if chunk?.is_none() {
								break;
							}
						}
						index += 1;
						continue;
					}

					Self::write_fetch_object(
						stream,
						FetchObjectInfo {
							sequence,
							object: index,
							prior: fetch_position(*prior_group, sequence, prior, index)?,
							timestamp: frame.timestamp,
							inflated: frame.inflated,
							timescale,
							version,
							fields: frame.fields.as_deref(),
							source: group.ietf(),
						},
					)
					.await?;
					prior = Some(index);
					index += 1;

					stream.varint(frame.size).await?;
					loop {
						let chunk = {
							let mut closed = std::pin::pin!(stream.closed());
							kio::wait(|waiter| {
								if waiter.poll_future(closed.as_mut()).is_ready() {
									return Poll::Ready(Err(Error::Cancel));
								}
								frame.poll_read_chunk(waiter)
							})
							.await
						};

						match chunk? {
							Some(mut chunk) => stream.write_all(&mut chunk).await?,
							None => break,
						}
					}
				}
				FillStep::Done => break,
			}
		}

		if until.is_some_and(|until| index < until) {
			return Err(Error::NotFound);
		}

		if prior.is_some() {
			*prior_group = Some(sequence);
		}
		Ok(())
	}

	/// Write one fetch object's header and timestamp properties in the negotiated layout.
	async fn write_fetch_object(
		stream: &mut Writer<Ranked<crate::transport::boxed::SendStream>, Version>,
		header: FetchObjectInfo<'_>,
	) -> Result<(), Error> {
		let FetchObjectInfo {
			sequence,
			object,
			prior,
			timestamp,
			inflated,
			timescale,
			version,
			fields,
			source,
		} = header;
		// Relayed properties survive without a timescale. Plain frames generate a
		// timestamp only on a timed track.
		let properties = if let Some(fields) = fields {
			let properties = super::object::write(&fields.properties, source, timestamp, version, timescale)?;
			(!properties.is_empty()).then_some(properties)
		} else {
			match timescale {
				Some(timescale) => {
					// Every frame on a timed track is timed (see `track::Info::timescale`).
					let timestamp = timestamp.ok_or(Error::TimestampMismatch)?;
					let mut properties = Vec::new();
					ietf::encode_object_time(&mut Encoder::new(&mut properties, version.into()), timestamp, timescale)?;
					Some(properties)
				}
				None => None,
			}
		};

		let properties = super::object::compression(properties.unwrap_or_default(), version, inflated)?;
		let properties = (!properties.is_empty()).then_some(properties);

		let header = match prior {
			// The first object must carry its absolute Group and Object IDs. Include the
			// priority too: "same as the prior object" has no prior to refer to.
			FetchPrior::None => ietf::FetchObject::Object {
				subgroup: ietf::FetchSubgroup::Zero,
				group: Some(sequence),
				object: Some(object),
				priority: Some(0),
				properties,
			},
			// Same group and priority; the Object ID is the prior one plus one.
			FetchPrior::Same(delta) => ietf::FetchObject::Object {
				subgroup: ietf::FetchSubgroup::Zero,
				group: None,
				object: delta,
				priority: None,
				properties,
			},
			// A new group restates its Object ID absolutely, and keeps the prior priority.
			FetchPrior::Group(group) => ietf::FetchObject::Object {
				subgroup: ietf::FetchSubgroup::Zero,
				group: Some(group),
				object: Some(object),
				priority: None,
				properties,
			},
		};

		stream.encode(&header).await?;

		Ok(())
	}

	/// Write one group's objects to a fetch stream, after those of `prior_group`, which it
	/// becomes once the group wrote any.
	async fn write_fetched_group(
		writer: &mut Writer<Ranked<crate::transport::boxed::SendStream>, Version>,
		group: FetchedGroup,
		prior_group: &mut Option<u64>,
		version: Version,
		timescale: Option<Timescale>,
	) -> Result<(), Error> {
		let source = group.info.ietf();
		let mut prior = None;
		let mut budget = kio::coop::Budget::new(32);
		for (ordinal, frame) in (group.first..).zip(group.frames) {
			kio::wait(|waiter| budget.poll_yield(waiter)).await;
			let object = frame.fields.as_ref().map_or(ordinal, |fields| fields.id);
			let position = fetch_position(*prior_group, group.sequence, prior, object)?;
			prior = Some(object);
			Self::write_fetch_object(
				writer,
				FetchObjectInfo {
					sequence: group.sequence,
					object,
					prior: position,
					timestamp: frame.timestamp,
					inflated: frame.inflated,
					timescale,
					version,
					fields: frame.fields.as_deref(),
					source,
				},
			)
			.await?;
			Self::write_fetch_payload(writer, frame.frame.payload).await?;
		}
		if prior.is_some() {
			*prior_group = Some(group.sequence);
		}
		Ok(())
	}

	/// Write a whole fetch object's length and payload, after its header.
	async fn write_fetch_payload(
		stream: &mut Writer<Ranked<crate::transport::boxed::SendStream>, Version>,
		payload: crate::frame::Payload,
	) -> Result<(), Error> {
		stream.varint(payload.len() as u64).await?;
		if !payload.is_empty() {
			let mut payload = payload;
			stream.write_all(&mut payload).await?;
		}
		Ok(())
	}

	/// Register a pending subscription until the returned guard drops.
	fn register_join(&self, request_id: RequestId) -> Join<GroupOf<Transport>> {
		self.joins.lock().insert(request_id, None);
		Join {
			joins: self.joins.clone(),
			request_id,
		}
	}

	/// Answer a FETCH: a standalone or filtered range of the named track, or a joining
	/// FETCH's groups up to where its subscription starts.
	///
	/// Its highest group is read before replying: FETCH_OK names where the response ends,
	/// which a range running past the track only learns by reading it, and a refusal can
	/// still replace it until then.
	fn fetch_gate(&self, msg: &ietf::Fetch<'_>) -> FetchGate {
		match &msg.fetch_type {
			FetchType::Standalone { namespace, .. } | FetchType::Filtered { namespace, .. } => FetchGate::Standalone(
				crate::auth::Gate::new(self.auth.clone(), namespace.clone(), crate::auth::Direction::Publish),
			),
			FetchType::RelativeJoining {
				subscriber_request_id, ..
			}
			| FetchType::AbsoluteJoining {
				subscriber_request_id, ..
			} => FetchGate::Joining {
				id: *subscriber_request_id,
				auth: self.auth.clone(),
			},
		}
	}

	#[cfg(test)]
	async fn run_fetch_stream(
		self,
		stream: Stream<Transport, Version>,
		msg: ietf::Fetch<'static>,
	) -> Result<(), Error> {
		let gate = self.fetch_gate(&msg);
		self.serve_fetch_stream(stream, msg, gate).await
	}

	async fn serve_fetch_stream(
		mut self,
		mut stream: Stream<Transport, Version>,
		msg: ietf::Fetch<'static>,
		admission: FetchGate,
	) -> Result<(), Error> {
		let namespace = match &msg.fetch_type {
			FetchType::Standalone { namespace, .. } | FetchType::Filtered { namespace, .. } => Some(namespace),
			_ => None,
		};
		if let Some(error) = namespace.and_then(|namespace| super::namespace::refusal(namespace, self.version)) {
			return super::request_stream::reject(stream, error).await;
		}
		// What the requester sent on this stream so far, kept across every phase that watches
		// it for a cancel, so a second GOAWAY is caught wherever it arrives.
		let mut finished = false;
		let mut messages = super::request_stream::Messages::new(self.session.clone(), self.control.client());
		let priority = super::priority::from_wire(msg.subscriber_priority);

		// Serving a Range Filter unfiltered would deliver objects the subscriber excluded.
		if msg.range_filters {
			return super::request_stream::reject(
				stream,
				ietf::RequestError {
					error_code: request::INVALID_FILTER,
					reason_phrase: "Range Filters exceed MAX_FILTER_RANGES=0".into(),
					retry_interval: 0,
				},
			)
			.await;
		}

		// FILL_TIMEOUT is accepted and ignored: a one-group read ends with the group.

		// Pinned to the instance it names, if any: another is refused, never served.
		let epoch = msg.epoch.clone();

		// Draft-22's LOCATION_FILTER is answered as the standalone range it spells. Every other
		// filter ends at the Largest Object, which resolves once the track does.
		let mut largest_filter = None;
		let fetch_type = match msg.fetch_type {
			FetchType::Filtered {
				namespace,
				track,
				filter: Filter::Absolute { start, end: Some(end) },
			} => {
				// The filter's End Object is inclusive, where a standalone one is the last
				// object plus one. Without one, both include the whole End Group.
				let Some(object) = end.object.map_or(Some(0), |object| object.checked_add(1)) else {
					return self
						.reject_fetch(stream, &Error::InvalidRange, "End Object overflows")
						.await;
				};
				FetchType::Standalone {
					namespace,
					track,
					start,
					end: Location {
						group: end.group,
						object,
					},
				}
			}
			FetchType::Filtered {
				namespace,
				track,
				filter,
			} => {
				largest_filter = Some(filter);
				// Placeholders until the filter resolves against the Largest Object.
				let whole = Location { group: 0, object: 0 };
				FetchType::Standalone {
					namespace,
					track,
					start: whole,
					end: whole,
				}
			}
			other => other,
		};

		// Every FETCH is held to the grant while its groups load and its response is written.
		// A standalone one is also checked here, before it reaches the origin; a joining one
		// was checked when its subscription was.
		//
		// A joining one is refused if its subscription ends before FETCH_OK. After that the
		// head is already held and the requester can still cancel, so it finishes on its own
		// authority: the gate, and the token grant if one authorized the subscription, which
		// ends it on the token's expiry, revocation, or a refused renewal.
		let mut lapses_with = None;
		let mut token_grant = None;
		let (track, start, end, joined, ranking, mut gate) = match fetch_type {
			FetchType::Standalone {
				namespace,
				track,
				start,
				end,
			} => {
				let FetchGate::Standalone(mut gate) = admission else {
					return Err(Error::Unauthorized);
				};
				if !gate.allows() {
					return self.reject_fetch(stream, &Error::Unauthorized, "not granted").await;
				}

				// End Object 0 includes the entire End Group, even at the largest Group ID.
				if largest_filter.is_none()
					&& (start.group > end.group
						|| (start.group == end.group && end.object != 0 && start.object >= end.object))
				{
					return self.reject_fetch(stream, &Error::InvalidRange, "empty range").await;
				}

				if largest_filter.is_none() && start.group != end.group {
					return self
						.reject_fetch(stream, &Error::Unsupported, "FETCH spans more than one group")
						.await;
				}

				// The peer must have seen the announcement to name this namespace, so this
				// resolves like a SUBSCRIBE does.
				let broadcast = match gate.guard(self.resolve(&mut stream, &namespace, epoch)).await {
					Ok(Some(broadcast)) => broadcast,
					Ok(None) => return Ok(()),
					Err(err) => return self.reject_fetch(stream, &err, &err.to_string()).await,
				};
				let track = match broadcast.track(&track) {
					Ok(track) => track,
					Err(err) => return self.reject_fetch(stream, &err, &err.to_string()).await,
				};

				// Its own entry in the queue, or its own send group, so it competes with group
				// streams by its priority alone, its request id breaking ties in the queue.
				// Nothing updates a FETCH's priority, and it ranks before the track's info
				// resolves, so the publisher's priority is the unset default.
				let ranking = Ranking {
					queue: self.priority.clone(),
					group: self.session.clone().send_group(crate::priority::group(priority, 0)),
					request_id: msg.request_id,
					priority: kio::Producer::new(priority).consume(),
					publisher: 0,
					order: Order::Descending,
				};
				(track, start, end, false, ranking, gate)
			}
			FetchType::RelativeJoining {
				subscriber_request_id, ..
			}
			| FetchType::AbsoluteJoining {
				subscriber_request_id, ..
			} => {
				let FetchGate::Joining { id, auth } = admission else {
					return Err(Error::Unauthorized);
				};
				debug_assert_eq!(id, subscriber_request_id);
				let (end, cache, namespace, ranking, grant) = match self
					.joined(&mut stream, &mut finished, &mut messages, subscriber_request_id)
					.await?
				{
					Ok(joined) => joined,
					Err((err, reason)) => return self.reject_fetch(stream, &err, reason).await,
				};
				// The cache outlives the subscription, so the fetch holds its own gate rather than
				// trusting the cache. One the subscription's request token authorized is held to
				// the peer's grant alone, as the subscription is.
				let gate = crate::auth::Gate::new(auth, namespace, crate::auth::Direction::Publish);
				let gate = if grant.is_some() { gate.named() } else { gate };
				lapses_with = Some(subscriber_request_id);
				token_grant = grant;
				let start = match fetch_type {
					FetchType::RelativeJoining { group_offset: 0, .. } => end.group,
					FetchType::RelativeJoining { .. } => {
						return self
							.reject_fetch(
								stream,
								&Error::Unsupported,
								"joining FETCH reaches before the joined group",
							)
							.await;
					}
					FetchType::AbsoluteJoining { group_id, .. } if group_id <= end.group => group_id,
					_ => {
						return self
							.reject_fetch(stream, &Error::InvalidRange, "joining group past the subscription")
							.await;
					}
				};
				(
					cache,
					Location {
						group: start,
						object: 0,
					},
					end,
					true,
					ranking,
					gate,
				)
			}
			// Rewritten as standalone above.
			FetchType::Filtered { .. } => {
				return self.reject_fetch(stream, &Error::Unsupported, "not supported").await;
			}
		};

		let joins = self.joins.clone();
		let unsubscribed = |waiter: &kio::Waiter| {
			lapses_with.is_some_and(|id| {
				joins
					.poll(waiter, |joins| match joins.contains_key(&id) {
						true => Poll::Pending,
						false => Poll::Ready(()),
					})
					.is_ready()
			})
		};
		let expired = |waiter: &kio::Waiter| {
			token_grant
				.as_ref()
				.is_some_and(|grant: &crate::auth::RequestGrant| grant.poll_ended(waiter).is_ready())
		};

		// Read the group before FETCH_OK promises it, while a refusal is still possible.
		let compression = self.peer_setup.get().await.flate;
		let prepared = {
			let mut prepare = std::pin::pin!(prepare_fetch(
				&track,
				FetchRange { start, end },
				largest_filter,
				joined,
				priority,
				compression,
			));
			kio::wait(|waiter| {
				if super::request_stream::poll_cancel(&mut stream, &mut finished, &mut messages, self.version, waiter)
					.is_ready()
				{
					return Poll::Ready(None);
				}
				if gate.poll_denied(waiter).is_ready() || unsubscribed(waiter) || expired(waiter) {
					let err = Error::Unauthorized;
					let reason = err.to_string();
					return Poll::Ready(Some(Err((err, reason))));
				}
				waiter.poll_future(prepare.as_mut()).map(Some)
			})
			.await
		};
		let PreparedFetch { range, last: group } = match prepared {
			Some(Ok(prepared)) => prepared,
			Some(Err((err, reason))) => return self.reject_fetch(stream, &err, &reason).await,
			None => return Ok(()),
		};
		let end = range.end;
		let group = group.without_status();
		// Only the TIMESCALE in FETCH_OK opts the answer into timestamps, so a requester that
		// opted out of the properties gets an untimed track.
		let timescale = group.info.timescale.filter(|_| msg.properties_wanted);
		let frames = group
			.frames
			.iter()
			.filter_map(|frame| Some((frame.fields.as_deref()?, frame.timestamp)));
		for (fields, frame) in frames.chain(group.end_object.as_deref().map(|end| (end, None))) {
			if let Err(err) =
				super::object::write(&fields.properties, group.info.ietf(), frame, self.version, timescale)
			{
				return self.reject_fetch(stream, &err, &err.to_string()).await;
			}
		}
		let properties = match track_properties(&group.info, msg.properties_wanted, self.version, compression) {
			Ok(properties) => properties,
			Err(err) => return self.reject_fetch(stream, &err, &err.to_string()).await,
		};
		let draft20 = Filter::is_draft20(self.version);
		// Only a finished track's final object ends it, never a live track's Largest Object.
		let end_of_track = !joined && group.complete && track.final_sequence() == group.sequence.checked_add(1);

		let end_location = if joined {
			// The subscription starts at the saved Largest Object, so the prefix of that
			// group has to be here in full or the two leave a gap.
			if group.sequence != end.group || group.end() != end.object {
				return self
					.reject_fetch(stream, &Error::NotFound, "joining prefix unavailable")
					.await;
			}
			end
		} else if draft20 {
			// Draft-22's End Location is inclusive. An empty answer still covers its start.
			let object = match range.until(group.sequence) {
				// The track's last object as this draft writes it, without a status object.
				Some(until) if end_of_track => (until - 1).min(group.end().saturating_sub(1)),
				Some(until) => until - 1,
				None => group.end().saturating_sub(1).max(range.skip(group.sequence)),
			};
			Location {
				group: group.sequence,
				object,
			}
		} else if end_of_track && end.object > 0 {
			// A bounded response cannot promise objects past the track's end.
			Location {
				group: group.sequence,
				object: group.end(),
			}
		} else {
			end
		};

		// The response, raced against the grant: a standalone FETCH whose grant narrows while it
		// waits on stream credit stops there instead of sending what it no longer may.
		let ok = async {
			// FETCH_OK on every draft, never REQUEST_OK: section 5.2 allows exactly one FETCH_OK or
			// REQUEST_ERROR in answer to a FETCH, and REQUEST_OK's own definition lists the other
			// requests it answers without ever naming this one.
			stream
				.writer
				.encode_message(&ietf::FetchOk {
					end_of_track,
					end_location,
					properties,
				})
				.await
		};
		let res = {
			let mut ok = std::pin::pin!(ok);
			kio::wait(|waiter| {
				if gate.poll_denied(waiter).is_ready() || unsubscribed(waiter) || expired(waiter) {
					return Poll::Ready(Err(Error::Unauthorized));
				}
				waiter.poll_future(ok.as_mut())
			})
			.await
		};
		if let Err(err) = res {
			stream.writer.abort(&err);
			return Err(err);
		}

		// Cancellation releases the buffered answer even while its stream is flow controlled.
		// A joining one no longer ends with its subscription.
		let respond = async {
			let mut rank = ranking.rank(range.start);
			let uni = self
				.session
				.open_uni(rank.options())
				.await
				.map_err(Error::from_transport)?;
			let mut writer = Writer::new(Ranked::new(uni, Some(rank)), self.version);
			writer.varint(FetchHeader::TYPE).await?;
			writer
				.encode(&FetchHeader {
					request_id: msg.request_id,
				})
				.await?;
			let mut prior_group = None;
			Self::write_fetched_group(&mut writer, group, &mut prior_group, self.version, timescale).await?;
			settle(&mut writer).await?;
			Ok::<(), Error>(())
		};
		let res = {
			let mut respond = std::pin::pin!(respond);
			kio::wait(|waiter| {
				if super::request_stream::poll_cancel(&mut stream, &mut finished, &mut messages, self.version, waiter)
					.is_ready()
				{
					return Poll::Ready(None);
				}
				if gate.poll_denied(waiter).is_ready() || expired(waiter) {
					return Poll::Ready(Some(Err(Error::Unauthorized)));
				}
				waiter.poll_future(respond.as_mut()).map(Some)
			})
			.await
		};
		// Cancelled: the fetch stream, if open, reset as it dropped with the response.
		let Some(res) = res else {
			return Ok(());
		};
		if let Err(err) = res {
			// The fetch stream, if open, reset as it dropped with the response. Once FETCH_OK
			// is out, no REQUEST_ERROR can follow it, so the reset is the whole refusal.
			stream.writer.abort(&err);
			return Err(err);
		}

		// FETCH_OK is the last thing this stream has to say, and [`Writer`] resets on drop:
		// without the finish the answer is discarded before the peer ever reads it, exactly
		// as it would be for a refusal. The peer dropping the stream first is a normal end.
		let _ = stream.writer.close().await;

		Ok(())
	}

	/// Resolve the subscription a joining FETCH names to its saved start, or the refusal
	/// to answer the FETCH with.
	async fn joined(
		&mut self,
		stream: &mut Stream<Transport, Version>,
		finished: &mut bool,
		messages: &mut super::request_stream::Messages,
		subscribe_id: RequestId,
	) -> Result<
		Result<
			(
				Location,
				track::Consumer,
				crate::Path,
				Ranking<GroupOf<Transport>>,
				Option<crate::auth::RequestGrant>,
			),
			(Error, &'static str),
		>,
		Error,
	> {
		// Request streams can arrive out of order. Wait on registration, while bounding
		// the lifetime of a request whose subscription never arrives or resolves.
		let joined = {
			let mut pending = false;
			let mut deadline = crate::time::Deadline::after(&self.runtime, Duration::from_secs(10));
			kio::wait(|waiter| {
				// The request reader is what the subscriber FINs or resets.
				if super::request_stream::poll_cancel(stream, finished, messages, self.version, waiter).is_ready() {
					return Poll::Ready(Err(Error::Cancel));
				}
				let joins = self.joins.poll(waiter, |joins| match joins.get(&subscribe_id) {
					Some(Some(_)) => Poll::Ready(()),
					Some(None) => {
						pending = true;
						Poll::Pending
					}
					None if pending => Poll::Ready(()),
					None => Poll::Pending,
				});
				if let Poll::Ready(joins) = joins {
					return Poll::Ready(Ok(joins.get(&subscribe_id).cloned().flatten()));
				}
				if deadline.poll(waiter).is_ready() {
					return Poll::Ready(if pending { Err(Error::Timeout) } else { Ok(None) });
				}
				Poll::Pending
			})
			.await
		};
		let refusal = match joined {
			Err(Error::Timeout) => (Error::Timeout, "subscription not ready"),
			Err(err) => return Err(err),
			Ok(Some(Joined::Group {
				end,
				cache,
				namespace,
				ranking,
				grant,
			})) => return Ok(Ok((end, cache, namespace, ranking, grant))),
			Ok(None) => (
				match self.version {
					Version::Draft18 => Error::InvalidJoiningRequestId,
					_ => Error::NotFound,
				},
				"no such subscription",
			),
			Ok(Some(Joined::Unsupported)) => (Error::Unsupported, "joining filter not supported"),
			Ok(Some(Joined::Empty)) => (Error::InvalidRange, "no objects at subscription start"),
		};
		Ok(Err(refusal))
	}

	/// Answer a TRACK_STATUS with what a SUBSCRIBE_OK for the track would carry, without
	/// subscribing.
	///
	/// The track resolves as it would for a SUBSCRIBE, so a relay asks its upstream for a
	/// track it does not hold yet and the answer waits on that. The Largest Location is
	/// whatever the cache holds now: nothing waits for a fresher one.
	#[cfg(test)]
	fn run_track_status_stream(
		self,
		stream: Stream<Transport, Version>,
		msg: ietf::TrackStatus<'static>,
	) -> impl std::future::Future<Output = Result<(), Error>> {
		let gate = crate::auth::Gate::new(
			self.auth.clone(),
			msg.track_namespace.clone(),
			crate::auth::Direction::Publish,
		);
		self.serve_track_status_stream(stream, msg, gate)
	}

	async fn serve_track_status_stream(
		self,
		mut stream: Stream<Transport, Version>,
		msg: ietf::TrackStatus<'static>,
		mut gate: crate::auth::Gate,
	) -> Result<(), Error> {
		if let Some(error) = super::namespace::refusal(&msg.track_namespace, self.version) {
			return super::request_stream::reject(stream, error).await;
		}
		if !gate.allows() {
			return self.reject_track_status(stream, &Error::Unauthorized).await;
		}
		let broadcast = match gate
			.guard(self.resolve(&mut stream, &msg.track_namespace, msg.epoch.clone()))
			.await
		{
			Ok(Some(broadcast)) => broadcast,
			Ok(None) => return Ok(()),
			Err(err) => return self.reject_track_status(stream, &err).await,
		};

		let track = match broadcast.track(&msg.track_name) {
			Ok(track) => track,
			Err(err) => return self.reject_track_status(stream, &err).await,
		};

		// The requester abandoning the request is owed nothing.
		let info = {
			let query = track.info();
			let mut finished = false;
			let mut messages = super::request_stream::Messages::new(self.session.clone(), self.control.client());
			kio::wait(|waiter| {
				if super::request_stream::poll_cancel(&mut stream, &mut finished, &mut messages, self.version, waiter)
					.is_ready()
				{
					return Poll::Ready(None);
				}
				if gate.poll_denied(waiter).is_ready() {
					return Poll::Ready(Some(Err(Error::Unauthorized)));
				}
				query.poll_ok(waiter).map(Some)
			})
			.await
		};
		let negotiated = self.peer_setup.get().await.flate;
		let properties =
			match info.map(|info| track_properties(&info?, msg.properties_wanted, self.version, negotiated)) {
				Some(Ok(properties)) => properties,
				Some(Err(err)) => return self.reject_track_status(stream, &err).await,
				None => return Ok(()),
			};

		// The answer stays gated until the transport takes it: one parked on stream credit
		// when the grant narrows is reset rather than delivered.
		let respond = async {
			let ok = ietf::TrackStatusOk {
				largest: live_edge(&track).largest,
				properties,
			};
			stream.writer.buffer_message(ietf::TrackStatusOk::ID, &ok)?;
			kio::wait(|waiter| stream.writer.poll_flush(waiter)).await?;
			Ok::<(), Error>(())
		};
		let res = {
			let mut respond = std::pin::pin!(respond);
			kio::wait(|waiter| {
				if gate.poll_denied(waiter).is_ready() {
					return Poll::Ready(Err(Error::Unauthorized));
				}
				waiter.poll_future(respond.as_mut())
			})
			.await
		};
		if let Err(err) = res {
			stream.writer.abort(&err);
			return Err(err);
		}

		// The answer is all this stream carries. See [`Self::reject_subscribe`] for why the
		// close is not optional.
		let _ = stream.writer.close().await;
		Ok(())
	}

	async fn reject_track_status(&self, mut stream: Stream<Transport, Version>, err: &Error) -> Result<(), Error> {
		let (kind, reason) = (request::Kind::TrackStatus, err.to_string());
		super::request_stream::write_error(&mut stream.writer, kind, err, &reason, self.version).await?;
		let _ = stream.writer.close().await;
		Ok(())
	}

	/// Reject a FETCH, ending the request stream. See [`Self::reject_subscribe`] for why the
	/// close is not optional.
	async fn reject_fetch(
		&self,
		mut stream: Stream<Transport, Version>,
		err: &Error,
		reason: &str,
	) -> Result<(), Error> {
		let kind = request::Kind::Fetch;
		super::request_stream::write_error(&mut stream.writer, kind, err, reason, self.version).await?;
		let _ = stream.writer.close().await;
		Ok(())
	}

	/// Bring the peer's view of one namespace in line with the current selection.
	///
	/// A loop writing inline (answering a SUBSCRIBE_NAMESPACE) re-sends
	/// NAMESPACE on that stream, which the receiver treats as a replacement, and
	/// retracts with NAMESPACE_DONE. Otherwise each advertisement rides its own
	/// PUBLISH_NAMESPACE request: an update is a REQUEST_UPDATE **on the stream that
	/// already carries it**, since a second stream would leave two claiming one
	/// namespace, and a withdrawal cancels the request.
	async fn sync_namespace(&self, ns: &mut Namespaces, suffix: &crate::Path, path: &crate::Path) -> Result<(), Error> {
		let Some((advert, auth)) = self.offer(ns, suffix, path) else {
			return Ok(());
		};
		let negotiated = ns.negotiated();
		let Namespaces {
			target,
			watched,
			requests,
			woken,
			updates,
			..
		} = ns;
		let Some(watch) = watched.get_mut(suffix) else {
			return Ok(());
		};
		let refused = watch.refused;
		let wanted = advert.wanted();
		let held = watch.sent.wanted();
		let request = requests.get_mut(suffix);
		let held_auth = request.as_ref().and_then(|request| request.auth.clone());
		let unchanged = advert == watch.sent && auth == held_auth;
		// The epoch names the advertisement's instance for as long as it stands, so an
		// update never changes it (MoQ Epoch). One instance named by an epoch is replaced
		// by another in place, the new advertised before the old is withdrawn, so the peer
		// never sees the namespace absent. Gaining or losing an epoch withdraws and
		// advertises afresh.
		let (held_epoch, epoch) = (watch.sent.params().epoch, advert.params().epoch);
		let renamed = wanted && held && held_epoch != epoch;
		let replaced = renamed && held_epoch.is_some() && epoch.is_some();
		// moq-transport renames a request's auth with REQUEST_UPDATE, but cannot name one on
		// a request that had none, nor drop one: those withdraw the request and make it
		// again.
		let restart = renamed || held_auth != auth && (held_auth.is_none() || auth.is_none());

		// An update already waiting or in flight carries whatever is current once it goes
		// out, and its answer brings the loop back here for anything after that.
		if wanted
			&& !restart
			&& let Some(request) = request
			&& !unchanged
		{
			if !request.queued && request.pending.is_none() {
				request.queued = true;
				updates.push_back(suffix.clone());
			}
			return Ok(());
		}

		if unchanged {
			// Nothing to send. A namespace that is no longer advertisable is no longer
			// pending either, and leaving that set would keep the retry timer armed
			// forever for a wire message that can never happen.
			if !wanted {
				watch.deferred = false;
			}
			return Ok(());
		}

		// A fresh offer waits for what the refusal asked for, whatever brought us back.
		// Only withdrawing and re-announcing clears it, since that builds a fresh entry.
		if wanted && !held && !refused.offerable(self.runtime.now()) {
			return Ok(());
		}

		let absolute = self.origin.root().rejoin(path);
		// Only a PUBLISH_NAMESPACE request or an update to one can be refused; everything
		// else below either rides a stream the peer already accepted or says nothing at all.
		let mut refused = watch.refused;
		let sent = match target {
			Target::Requests => {
				match (advert.wanted(), requests.get_mut(suffix)) {
					(false, _) => {
						if held {
							tracing::debug!(broadcast = %absolute, "namespace_done");
						}
						self.withdraw_namespace(target, requests, suffix.clone()).await?;
					}
					(true, Some(_)) if replaced => {
						// Separate streams are unordered, so the old one goes only once the
						// peer answered the new one, refused or not: it serves another
						// instance either way.
						tracing::debug!(broadcast = %absolute, "publish_namespace for another epoch");
						let old = requests.remove(suffix);
						refused = self
							.advertise_namespace(requests, watch, path, suffix.clone(), advert.params(), auth)
							.await?;
						if let Some(old) = old {
							end_request(old.stream);
						}
					}
					(true, Some(_)) => {
						tracing::debug!(broadcast = %absolute, renamed, "publish_namespace restart");
						self.withdraw_namespace(target, requests, suffix.clone()).await?;
						refused = self
							.advertise_namespace(requests, watch, path, suffix.clone(), advert.params(), auth)
							.await?;
					}
					(true, None) => {
						tracing::debug!(broadcast = %absolute, "publish_namespace");
						refused = self
							.advertise_namespace(requests, watch, path, suffix.clone(), advert.params(), auth)
							.await?;
					}
				}
				// Register an idle reader too: GOAWAY can arrive without an update in flight.
				if requests.contains_key(suffix) {
					woken.suffixes.lock().unwrap().push(suffix.clone());
				}
				// The peer can reject a fresh PUBLISH_NAMESPACE or an update, which leaves
				// no request behind. Record what it actually holds, so a later route change
				// retries instead of believing the namespace is already advertised.
				match requests.contains_key(suffix) {
					true => advert,
					false => Advert::None,
				}
			}
			Target::Inline(stream) => {
				match (advert.wanted(), held) {
					(true, _) => {
						// A repeat under another epoch replaces the instance in place.
						if renamed && !replaced {
							tracing::debug!(broadcast = %absolute, "namespace_done for an epoch gained or lost");
							stream
								.writer
								.encode_message(&ietf::NamespaceDone { suffix: suffix.clone() })
								.await?;
						}
						tracing::debug!(broadcast = %absolute, "namespace");
						stream
							.writer
							.encode_message(&ietf::Namespace {
								suffix: suffix.clone(),
								params: negotiated.extended().then(|| advert.params()),
							})
							.await?;
					}
					(false, true) => {
						tracing::debug!(broadcast = %absolute, "namespace_done");
						stream
							.writer
							.encode_message(&ietf::NamespaceDone { suffix: suffix.clone() })
							.await?;
					}
					// Never advertised and still not advertisable: nothing to say.
					(false, false) => {}
				}
				advert
			}
		};

		// A peer that asked not to be offered this again outranks the retry timer;
		// anything else it should hold and does not comes back on one.
		watch.refused = refused;
		watch.deferred = wanted && !sent.wanted() && refused.pending();
		watch.sent = sent;
		Ok(())
	}

	/// Open a PUBLISH_NAMESPACE request for one namespace, recording it in `requests`
	/// so an update or withdrawal reuses the same stream. A declined request records
	/// nothing: a peer that wants none of this rejects each one and stays connected.
	///
	/// A request stream that fails on its own, reset or ended before its answer, ends only
	/// this advertisement, which comes back on `watch`'s growing backoff. Only the control
	/// stream failing or the peer breaking the protocol ends the loop. The latter closes the
	/// session here, since the solicited path runs this inside a request task whose errors
	/// end only that stream.
	///
	/// Returns what the refusal, if any, said about coming back.
	async fn advertise_namespace(
		&self,
		requests: &mut HashMap<crate::Path, NamespaceRequest>,
		watch: &mut Watched,
		path: &crate::Path,
		suffix: crate::Path,
		params: ietf::AdvertParams,
		auth: Option<crate::auth::Credential>,
	) -> Result<Refused, Error> {
		// Bounded, because an advertisement holds its stream for as long as the namespace
		// lives: a peer whose concurrent-stream limit we have filled refuses this open, and
		// the withdrawals queued behind it are the only thing that would free a slot.
		// Giving up records nothing and spends no request ID, so the namespace is simply
		// retried later.
		let Some(mut request) = self.open_request().await? else {
			tracing::debug!(broadcast = %crate::path::Rooted(self.origin.root(), path), "no stream for the advertisement");
			return Ok(Refused::No);
		};
		let request_id = self.control.next_request_id();

		let mut messages = super::request_stream::Messages::new(self.session.clone(), self.control.client());
		let sent = request
			.writer
			.encode_message(&ietf::PublishNamespace {
				request_id,
				track_namespace: path.clone(),
				params,
				authorization_token: auth.as_ref().and_then(|auth| self.request_token(auth)),
			})
			.await;
		let answer = match sent {
			Ok(()) => self.read_response(&mut request, &mut messages).await,
			Err(err) => Err(err),
		};

		let absolute = self.origin.root().rejoin(path);
		let super::request_stream::Framed(type_id, body) = match answer {
			Ok(Answer::Message(framed)) => framed,
			Ok(Answer::Late) => {
				tracing::debug!(broadcast = %absolute, "no answer to the advertisement");
				// The peer may hold the request it never answered, so it is withdrawn.
				end_request(request);
				return Ok(Refused::No);
			}
			Ok(Answer::Ended) => {
				tracing::debug!(broadcast = %absolute, "advertisement ended before its answer");
				return Ok(watch.lapsed(self.runtime.now()));
			}
			Err(err) if !super::subscriber::is_protocol_violation(&err) => {
				tracing::debug!(broadcast = %absolute, %err, "advertisement failed");
				return Ok(watch.lapsed(self.runtime.now()));
			}
			Err(err) => return Err(messages.failed(err)),
		};

		// `None` when the peer holds the namespace.
		let refused = (|| {
			match type_id {
				ietf::RequestOk::ID => {
					let msg: ietf::RequestOk = body.message(self.version)?;
					// ACTIVE_COUNT only answers SUBSCRIBE_NAMESPACE (MoQ Active Count).
					if msg.active.is_some() {
						return Err(Error::ProtocolViolation);
					}
					tracing::debug!(message = ?msg, "publish namespace ok");
				}
				ietf::RequestError::ID => {
					let msg: ietf::RequestError = body.message(self.version)?;
					tracing::warn!(message = ?msg, "publish namespace error");
					return Ok(Some(self.refusal(msg.retry_interval)));
				}
				_ => return Err(Error::UnexpectedMessage),
			}
			Ok(None)
		})()
		.map_err(|err| messages.failed(err))?;
		if let Some(refused) = refused {
			return Ok(refused);
		}

		requests.insert(
			suffix,
			NamespaceRequest {
				messages,
				path: path.clone(),
				stream: request,
				auth,
				queued: false,
				pending: None,
				waiter: None,
			},
		);
		Ok(Refused::No)
	}

	/// What the peer should hold for one namespace: the advertisement, and the auth its
	/// PUBLISH_NAMESPACE request carries. `None` when the namespace is not watched.
	///
	/// Nothing our grant does not cover reaches the wire, and a shrink withdraws it. Only a
	/// request carries an auth, and only one this session's handle issued: any other route
	/// is advertised under the session's scope.
	fn offer(
		&self,
		ns: &Namespaces,
		suffix: &crate::Path,
		path: &crate::Path,
	) -> Option<(Advert, Option<crate::auth::Credential>)> {
		let watch = ns.watched.get(suffix)?;
		let advert = match ns.permitted(path) {
			true => self.select(&watch.route, &ns.peer, ns.epochs),
			false => Advert::None,
		};
		let auth = match (&ns.target, advert.wanted()) {
			(Target::Requests, true) => watch
				.route
				.auth
				.as_ref()
				.filter(|auth| self.auth.token(auth).is_some())
				.cloned(),
			_ => None,
		};
		Some((advert, auth))
	}

	/// The `AUTHORIZATION TOKEN` a request names `auth` with, by value.
	fn request_token(&self, auth: &crate::auth::Credential) -> Option<super::token::RequestToken> {
		let token = self.auth.token(auth)?;
		Some(super::token::RequestToken(crate::setup::Token {
			kind: crate::setup::Token::OUT_OF_BAND,
			value: token.to_vec(),
		}))
	}

	/// Drop queued updates that no longer have anything to send, so the next one to take
	/// a request ID is one that will use it: drafts before 17 require the next ID.
	fn prune_updates(&self, ns: &mut Namespaces, prefix: &crate::Path) {
		while let Some(suffix) = ns.updates.front() {
			let wants = ns.requests.get(suffix).is_some_and(|request| {
				request.queued
					&& request.pending.is_none()
					&& ns.watched.get(suffix).is_some_and(|watch| {
						self.offer(ns, suffix, &prefix.rejoin(suffix))
							.is_some_and(|(advert, auth)| {
								advert.wanted() && (advert != watch.sent || auth != request.auth)
							})
					})
			});
			if wants {
				return;
			}
			let suffix = ns.updates.pop_front().expect("checked non-empty");
			if let Some(request) = ns.requests.get_mut(&suffix) {
				request.queued = false;
			}
		}
	}

	/// Send the update waiting at the front of the queue with `request_id`: REQUEST_UPDATE
	/// on the request that carries the namespace, with only what changed since the peer's
	/// copy. Its answer arrives through [`Self::poll_answer`] while the loop carries on.
	///
	/// One update is outstanding per request, which satisfies any MAX_REQUEST_UPDATES the
	/// peer set without reading it. A different original publisher is an update like any
	/// other: the receiver drains what it already serves from the old one and never splices
	/// the two.
	async fn send_update(&self, ns: &mut Namespaces, prefix: &crate::Path, request_id: RequestId) -> Result<(), Error> {
		let suffix = ns.updates.pop_front().expect("allocated for a queued update");
		let Some((advert, auth)) = self.offer(ns, &suffix, &prefix.rejoin(&suffix)) else {
			return Ok(());
		};
		let (Some(request), Some(watch)) = (ns.requests.get_mut(&suffix), ns.watched.get_mut(&suffix)) else {
			return Ok(());
		};
		request.queued = false;
		let mut update = match (advert.params().cluster, watch.sent.params().cluster) {
			(Some(next), Some(held)) => ietf::PublishNamespaceUpdate::between(request_id, &held, &next),
			_ => ietf::PublishNamespaceUpdate {
				request_id,
				hops: None,
				cost: None,
				authorization_token: None,
			},
		};
		if auth != request.auth {
			update.authorization_token = auth.as_ref().and_then(|auth| self.request_token(auth));
		}
		tracing::debug!(broadcast = %crate::path::Rooted(self.origin.root(), &request.path), "publish_namespace update");
		let written = request.stream.writer.encode_message(&update).await;
		if let Err(err) = written {
			// The peer ended the request before the update reached it, which ends only this
			// advertisement: closing our side completes the withdrawal, and the retry
			// re-offers it fresh.
			tracing::debug!(%err, "publish_namespace update not sent");
			self.withdraw_namespace(&mut ns.target, &mut ns.requests, suffix.clone())
				.await?;
			if let Some(watch) = ns.watched.get_mut(&suffix) {
				watch.refused = Refused::No;
				watch.deferred = Refused::No.pending();
				watch.sent = Advert::None;
			}
			return Ok(());
		}

		// The peer holds the update unless its answer says otherwise.
		let deadline = self.runtime.now() + ADVERTISE_TIMEOUT;
		request.pending = Some(deadline);
		request.auth = auth;
		watch.sent = advert;
		// Marked woken, so the loop's next pass reads the answer it may already have.
		ns.woken.suffixes.lock().unwrap().push(suffix.clone());
		ns.inflight.push_back((deadline, suffix));
		Ok(())
	}

	/// What came back for an update in flight, polling only the requests that woke.
	fn poll_answer(
		requests: &mut HashMap<crate::Path, NamespaceRequest>,
		inflight: &mut VecDeque<(crate::time::Instant, crate::Path)>,
		woken: &Arc<Woken>,
		now: crate::time::Instant,
		waiter: &kio::Waiter,
	) -> Poll<(crate::Path, Result<Answer, Error>)> {
		// Registered before the take, so a wake landing between them is not lost.
		*woken.owner.lock().unwrap() = Some(waiter.waker().clone());
		let mut suffixes = std::mem::take(&mut *woken.suffixes.lock().unwrap());
		while let Some(suffix) = suffixes.pop() {
			let Some(request) = requests.get_mut(&suffix) else {
				continue;
			};
			// Kept on the request, so its registrations outlive this pass.
			let request_waiter = request
				.waiter
				.get_or_insert_with(|| kio::Waiter::new(woken.waker(suffix.clone())));
			if let Poll::Ready(answer) = request.messages.poll_next(&mut request.stream.reader, request_waiter) {
				// The rest stay woken for the next pass.
				woken.suffixes.lock().unwrap().append(&mut suffixes);
				let answer = answer.and_then(|answer| {
					if answer.is_some() && request.pending.is_none() {
						return Err(request.messages.failed(Error::UnexpectedMessage));
					}
					Ok(answer.map_or(Answer::Ended, Answer::Message))
				});
				woken.suffixes.lock().unwrap().push(suffix.clone());
				return Poll::Ready((suffix, answer));
			}
		}
		while let Some((deadline, suffix)) = inflight.front() {
			let current = requests.get(suffix).and_then(|request| request.pending) == Some(*deadline);
			if current && now < *deadline {
				break;
			}
			let (_, suffix) = inflight.pop_front().expect("just peeked");
			if current {
				return Poll::Ready((suffix, Ok(Answer::Late)));
			}
		}
		Poll::Pending
	}

	/// Apply the answer to an update: the peer still holds the namespace on an OK, and a
	/// REQUEST_ERROR closed the stream and withdrew the advertisement (moq-transport Section
	/// 9.5.1), so the request is dropped and the retry re-offers it fresh. An unanswered
	/// update is dropped the same way, since a peer that ignored it cannot be assumed to hold
	/// either version, and so is one the peer cancelled instead of answering. An idle
	/// advertisement the peer ends waits out its own growing backoff before coming back.
	async fn answered(
		&self,
		ns: &mut Namespaces,
		prefix: &crate::Path,
		suffix: crate::Path,
		answer: Result<Answer, Error>,
	) -> Result<(), Error> {
		let Some(request) = ns.requests.get_mut(&suffix) else {
			return Ok(());
		};
		let idle = request.pending.take().is_none();
		let absolute = self.origin.root().rejoin(&request.path);

		// A reset stream ends only this request, like a FIN; a malformed answer is the peer's
		// violation and still ends the loop.
		let answer = match answer {
			Err(err) if !super::subscriber::is_protocol_violation(&err) => {
				tracing::debug!(broadcast = %absolute, %err, "update stream ended");
				Answer::Ended
			}
			answer => answer?,
		};
		let refused = match answer {
			Answer::Late => {
				tracing::debug!(broadcast = %absolute, "no answer to the update");
				// Abrupt: a peer that never answers is not owed the FIN handshake.
				ns.requests.remove(&suffix);
				Refused::No
			}
			Answer::Ended if idle => {
				tracing::debug!(broadcast = %absolute, "advertisement ended");
				self.withdraw_namespace(&mut ns.target, &mut ns.requests, suffix.clone())
					.await?;
				let now = self.runtime.now();
				ns.watched
					.get_mut(&suffix)
					.map_or(Refused::No, |watch| watch.lapsed(now))
			}
			Answer::Ended => {
				tracing::debug!(broadcast = %absolute, "update cancelled");
				// The peer closed its side; closing ours completes the withdrawal.
				self.withdraw_namespace(&mut ns.target, &mut ns.requests, suffix.clone())
					.await?;
				Refused::No
			}
			Answer::Message(super::request_stream::Framed(type_id, body)) => {
				match type_id {
					ietf::RequestOk::ID => {
						let msg: ietf::RequestOk = body.message(self.version)?;
						// ACTIVE_COUNT only answers SUBSCRIBE_NAMESPACE (MoQ Active Count);
						// closed here for the same reason as in `advertise_namespace`.
						if msg.active.is_some() {
							self.session.clone().close(
								super::error::to_session_code(&crate::SessionError::ProtocolViolation),
								"ACTIVE_COUNT on a REQUEST_UPDATE answer",
							);
							return Err(Error::ProtocolViolation);
						}
						tracing::debug!(message = ?msg, "publish_namespace update ok");
						// Whatever changed while it was in flight goes out next.
						return self.sync_namespace(ns, &suffix, &prefix.rejoin(&suffix)).await;
					}
					ietf::RequestError::ID => {
						let msg: ietf::RequestError = body.message(self.version)?;
						tracing::warn!(message = ?msg, "publish_namespace update error");
						// The peer closed its side; closing ours completes the withdrawal.
						self.withdraw_namespace(&mut ns.target, &mut ns.requests, suffix.clone())
							.await?;
						self.refusal(msg.retry_interval)
					}
					_ => return Err(Error::UnexpectedMessage),
				}
			}
		};

		if let Some(watch) = ns.watched.get_mut(&suffix) {
			watch.refused = refused;
			watch.deferred = refused.pending();
			watch.sent = Advert::None;
		}
		Ok(())
	}

	/// How to read a refusal's retry interval, in milliseconds.
	///
	/// 0 is the peer asking not to be offered this again, which is what keeps a permanent
	/// refusal (unauthorized, uninterested) from becoming a request every few seconds for
	/// the life of the session.
	fn refusal(&self, retry_interval: u64) -> Refused {
		match retry_interval {
			0 => Refused::Never,
			ms => Refused::Until(self.runtime.now() + Duration::from_millis(ms)),
		}
	}

	pub(super) fn stats(&self) -> &crate::stats::Session {
		self.origin.stats()
	}

	/// Open a stream for one advertisement, or `None` if the peer has no stream credit for
	/// it or did not take its header in time.
	///
	/// The announce loop is single-threaded over origin updates, so an open that parks
	/// forever parks everything, including the unannounces that release the streams the
	/// peer is waiting on us to retire. Failing instead keeps the loop moving.
	///
	/// Unlike a request, an advertisement refused for credit comes back on the retry timer:
	/// it is our own state the peer should hold, and credit returning raises no signal the
	/// loop could wait on. A replacement under a new epoch needs this too, since the
	/// instance it replaces still holds its stream when the new one opens.
	async fn open_request(&self) -> Result<Option<Stream<Transport, Version>>, Error> {
		let mut session = self.session.clone();
		let mut open = std::pin::pin!(Stream::open_request(&mut session, self.version));
		let mut timeout = crate::time::Deadline::after(&self.runtime, ADVERTISE_TIMEOUT);

		kio::wait(|waiter| {
			if let Poll::Ready(res) = waiter.poll_future(open.as_mut()) {
				return Poll::Ready(match res {
					Ok(stream) => Ok(Some(stream.with_stats(self.origin.stats()))),
					Err(Error::NoStreamCredit) => Ok(None),
					Err(err) => Err(err),
				});
			}
			if timeout.poll(waiter).is_ready() {
				return Poll::Ready(Ok(None));
			}
			Poll::Pending
		})
		.await
	}

	/// Read the peer's answer to one advertisement: [`Answer::Late`] if it did not answer in
	/// time, [`Answer::Ended`] if it ended the request instead.
	///
	/// Bounded for the same reason [`Self::open_request`] is, and it is the same peer
	/// behavior seen a step later: a stream the peer accepts and never answers on holds the
	/// loop just as effectively as one it never grants. Giving up records nothing, so the
	/// namespace stays outstanding and the retry re-offers it.
	async fn read_response(
		&self,
		request: &mut Stream<Transport, Version>,
		messages: &mut super::request_stream::Messages,
	) -> Result<Answer, Error> {
		let mut read = std::pin::pin!(messages.next(&mut request.reader));
		let mut timeout = crate::time::Deadline::after(&self.runtime, ADVERTISE_TIMEOUT);

		kio::wait(|waiter| {
			if let Poll::Ready(res) = waiter.poll_future(read.as_mut()) {
				return Poll::Ready(res.map(|answer| answer.map_or(Answer::Ended, Answer::Message)));
			}
			if timeout.poll(waiter).is_ready() {
				return Poll::Ready(Ok(Answer::Late));
			}
			Poll::Pending
		})
		.await
	}

	/// Withdraw an advertised namespace: NAMESPACE_DONE inline, or end the request that
	/// carried it.
	async fn withdraw_namespace(
		&self,
		target: &mut Target,
		requests: &mut HashMap<crate::Path, NamespaceRequest>,
		suffix: crate::Path,
	) -> Result<(), Error> {
		match target {
			Target::Requests => {
				if let Some(request) = requests.remove(&suffix) {
					end_request(request.stream);
				}
			}
			Target::Inline(stream) => {
				stream
					.writer
					.encode_message(&ietf::NamespaceDone { suffix: suffix.clone() })
					.await?;
			}
		}
		Ok(())
	}

	/// Close out every open PUBLISH_NAMESPACE request. A no-op for a loop whose entries
	/// ride the SUBSCRIBE_NAMESPACE stream itself, which retracts them by ending.
	async fn withdraw_requests(&self, target: &mut Target, requests: &mut HashMap<crate::Path, NamespaceRequest>) {
		let suffixes: Vec<crate::Path> = requests.keys().cloned().collect();
		for suffix in suffixes {
			let _ = self.withdraw_namespace(target, requests, suffix).await;
		}
	}

	/// Advertise every namespace we can, without waiting to be asked.
	///
	/// moq-transport itself says nothing about which of the two discovery messages a peer
	/// expects, and the peers that never send SUBSCRIBE_NAMESPACE are exactly the ones
	/// expecting a publisher to announce itself, so the default has to be to announce. A
	/// peer that would rather ask says so with the MoQ Solicit extension
	/// ([`solicit`](super::solicit)),
	/// and then this loop does nothing.
	/// [`Self::serve_subscribe_namespace_stream`] also carries every
	/// match, so a peer that did not ask hears each namespace both ways. A NAMESPACE is
	/// discovery only, not a second route.
	pub async fn run_publish_namespaces(self) -> Result<(), Error> {
		if self.requires_solicitation().await {
			return Ok(());
		}

		// The cluster extension changes what an advertisement carries, so nothing can be
		// sent until the peer's SETUP says whether it speaks it.
		let peer = self.peer().await;

		// Split horizon, as the solicited loop applies it: never advertise a route back
		// to the peer it came from.
		let origin = self
			.advertising(self.origin.clone(), &peer)
			.discovery(!self.peer_setup.get().await.hidden || self.origin.includes_hidden());

		let mut ns = Namespaces::new(peer, Target::Requests);
		ns.epochs = self.peer_setup.get().await.epoch;
		self.run_namespaces(origin.announced(), crate::Path::empty(), ns, Vec::new())
			.await
	}

	/// Handle a SUBSCRIBE_NAMESPACE on its bidi stream.
	///
	/// All the announce state is local to this task (mirroring `lite::Publisher`'s
	/// announce handling): whatever this subscription advertised is withdrawn
	/// when its stream ends. Every match is a NAMESPACE on this stream, whatever the
	/// peer's SETUP said.
	#[cfg(test)]
	async fn run_subscribe_namespace_stream(
		self,
		stream: Stream<Transport, Version>,
		msg: ietf::SubscribeNamespace,
	) -> Result<(), Error> {
		let gate = crate::auth::Gate::new(
			self.auth.clone(),
			msg.namespace.clone(),
			crate::auth::Direction::Publish,
		);
		self.serve_subscribe_namespace_stream(stream, msg, gate).await
	}

	async fn serve_subscribe_namespace_stream(
		self,
		mut stream: Stream<Transport, Version>,
		msg: ietf::SubscribeNamespace,
		gate: crate::auth::Gate,
	) -> Result<(), Error> {
		if let Some(error) = super::namespace::refusal(&msg.namespace, self.version) {
			return super::request_stream::reject(stream, error).await;
		}
		let prefix = msg.namespace.clone();

		tracing::debug!(prefix = %crate::path::Rooted(self.origin.root(), &prefix), "subscribe_namespace stream");

		// A prefix outside our scope (empty origin, or a token that doesn't grant it)
		// just means we have nothing to announce; respond with an empty set rather than
		// erroring, which would look fatal to the peer.
		// The wire prefix decodes as a literal path; convert it explicitly to its
		// subtree grant, refusing anything that cannot be a subtree.
		// The cursor is rooted at the prefix so every update arrives named as its
		// wire suffix, a route covering the prefix included: that one presents at
		// the root, as the empty suffix.
		let scope = crate::Pattern::subtree(prefix.as_str())
			.map(|subtree| subtree.rebase(prefix.as_str()))
			.unwrap_or_default();
		let origin = self
			.origin
			.scope(&prefix, &scope)
			.unwrap_or_else(|_| self.origin.empty());

		// The extension changes what an advertisement carries, so nothing can be
		// sent until the peer's SETUP says whether it speaks it. The same SETUP says
		// whether the OK counts what it sends first (MoQ Active Count).
		let peer = self.peer().await;
		let declared = self.peer_setup.get().await;
		// Register the split-horizon peer on the announce cursor too. The origin
		// model uses this exposure to park a reflected copy before it can replace
		// the source we are currently advertising to that peer.
		let origin = self.advertising(origin, &peer);

		// Peers that declared MoQ Hidden filter namespaces unless they opted in. A publish
		// origin that already opted in (the caller's choice for this peer) keeps them.
		let origin = origin.discovery(!declared.hidden || msg.hidden || self.origin.includes_hidden());

		// A NAMESPACE is discovery, not a route, so a peer that also hears the unsolicited
		// PUBLISH_NAMESPACE still gets the match here.
		let mut announced = origin.announced();

		// MoQ Active Count: take what is advertised now, so the OK can say how many
		// NAMESPACE messages carry it. They go out first, ahead of any change, so with
		// MoQ Auth the count waits for our grant and leaves out what it does not cover.
		let (initial, active) = match declared.active_count {
			true => {
				if declared.auth {
					kio::wait(|waiter| gate.poll_setup_answered(waiter)).await;
				}
				let (permit, _) = self.permit_now(Some(&gate));
				let initial = Self::snapshot(&mut announced);
				let count = initial
					.iter()
					.filter(|update| permit.matches(prefix.rejoin(&update.prefix).as_str()))
					.filter(|update| self.select(&update.route, &peer, declared.epoch).wanted())
					.count();
				(initial, Some(count as u64))
			}
			false => (Vec::new(), None),
		};

		// Send OK response
		stream
			.writer
			.encode_message(&ietf::RequestOk { largest: None, active })
			.await?;

		let target = Target::Inline(stream);

		let mut ns = Namespaces::new(peer, target);
		ns.gate = Some(gate);
		ns.hidden = !declared.hidden || msg.hidden || self.origin.includes_hidden();
		ns.epochs = declared.epoch;
		self.run_namespaces(announced, prefix, ns, initial).await
	}

	/// Our publish permit as it stands now, with the auth epoch it was read at.
	fn permit_now(&self, gate: Option<&crate::auth::Gate>) -> (crate::auth::Permit, u64) {
		let mut epoch = 0;
		let permit = match gate {
			Some(gate) => gate.poll_permit(&mut epoch, &kio::Waiter::noop()),
			None => self
				.auth
				.poll_permit(crate::auth::Direction::Publish, &mut epoch, &kio::Waiter::noop()),
		};
		match permit {
			Poll::Ready(permit) => (permit, epoch),
			Poll::Pending => (Default::default(), 0),
		}
	}

	/// The routes an announce cursor holds right now, without waiting for more.
	///
	/// A route announced and retracted within the snapshot is left out, and a repeat
	/// keeps its latest metadata, so each path appears once.
	fn snapshot(announced: &mut crate::announce::Consumer) -> Vec<crate::announce::Announce> {
		let mut initial = std::collections::BTreeMap::new();
		while let Some(event) = announced.try_next() {
			match event {
				crate::announce::Event::Start(update)
				| crate::announce::Event::Update(update)
				| crate::announce::Event::Restart(update)
				| crate::announce::Event::Takeover { announce: update, .. } => {
					initial.insert(update.prefix.clone(), update);
				}
				crate::announce::Event::End(update) => {
					initial.remove(&update.prefix);
				}
			}
		}
		initial.into_values().collect()
	}

	/// Forward origin (un)announces to the peer until the loop ends.
	///
	/// Shared by both announce paths: they differ in where the advertisements go
	/// ([`Target`]) and where the origin is rooted (`prefix`, empty when nothing asked
	/// for a subset).
	async fn run_namespaces(
		&self,
		mut announced: crate::announce::Consumer,
		mut prefix: crate::Path,
		mut ns: Namespaces,
		initial: Vec<crate::announce::Announce>,
	) -> Result<(), Error> {
		let mut finished = false;
		let mut goaways = super::request_stream::Messages::new(self.session.clone(), self.control.client());
		let _withdrawing = self.withdrawal.register();
		if self.withdrawal.poll(&kio::Waiter::noop()).is_ready() {
			return Ok(());
		}
		// With MoQ Auth, wait for the answer to the credential we presented at setup, so
		// the first advertisement, the initial set included, is already checked against
		// our grant.
		if self.peer_setup.get().await.auth {
			kio::wait(|waiter| self.auth.poll_setup_answered(waiter)).await;
		}
		(ns.permit, ns.epoch) = self.permit_now(ns.gate.as_ref());
		for update in initial {
			self.apply_update(&mut ns, &prefix, update, true).await?;
		}

		// When to re-offer whatever the peer should hold and doesn't, and how long to wait
		// the next time that fails. Jittered so a relay's namespaces don't all come back on
		// the same tick.
		let mut retry = crate::time::Deadline::new(&self.runtime);
		let mut retry_at: Option<crate::time::Instant> = None;
		// When the earliest update in flight gives up on its answer.
		let mut answer_by = crate::time::Deadline::new(&self.runtime);
		let mut retry_delay = RETRY_BASE;

		// Stream updates (origin route (un)announces), bailing if the peer closes
		// its side first. A peer streaming REQUEST_UPDATEs must not hold the session's other
		// tasks.
		let mut budget = kio::coop::Budget::new(32);
		let res = loop {
			kio::wait(|waiter| budget.poll_yield(waiter)).await;
			match ns.watched.values().any(|watch| watch.deferred) {
				// Arm on the edge, so a turn that changes nothing else doesn't push the
				// deadline out forever.
				true => retry_at = retry_at.or_else(|| Some(self.runtime.now() + jitter(retry_delay))),
				false => {
					retry_at = None;
					retry_delay = RETRY_BASE;
				}
			}
			retry.set(retry_at);
			self.prune_updates(&mut ns, &prefix);

			let event = {
				let Namespaces {
					target,
					epoch,
					requests,
					inflight,
					woken,
					updates,
					gate,
					..
				} = &mut ns;
				kio::wait(|waiter| {
					if self.withdrawal.poll(waiter).is_ready() {
						return Poll::Ready(NamespaceEvent::Update(None));
					}
					// A grant change applies before the next update, so a namespace it no
					// longer covers is withdrawn rather than re-sent.
					let permit = match gate {
						Some(gate) => gate.poll_permit(epoch, waiter),
						None => self.auth.poll_permit(crate::auth::Direction::Publish, epoch, waiter),
					};
					if let Poll::Ready(permit) = permit {
						return Poll::Ready(NamespaceEvent::Regrant(permit));
					}
					if let Poll::Ready(event) = target.poll_event(&mut finished, &mut goaways, self.version, waiter) {
						return Poll::Ready(event);
					}
					// Each update in flight is answered on its own stream, so many renew at
					// once and none holds up the announcements behind it.
					let now = self.runtime.now();
					if let Poll::Ready((suffix, answer)) = Self::poll_answer(requests, inflight, woken, now, waiter) {
						return Poll::Ready(NamespaceEvent::Answered(suffix, answer));
					}
					// The front is now the earliest update still waiting.
					answer_by.set(inflight.front().map(|(deadline, _)| *deadline));
					let _ = answer_by.poll(waiter);
					if !updates.is_empty() {
						return Poll::Ready(NamespaceEvent::Renew(self.control.next_request_id()));
					}
					if let Poll::Ready(next) = announced.poll_next(waiter) {
						return Poll::Ready(NamespaceEvent::Update(next.map(|event| match event {
							crate::announce::Event::Start(update) | crate::announce::Event::Update(update) => {
								(update, true, false)
							}
							crate::announce::Event::Restart(update) => (update, true, true),
							// moq-transport has no takeover: another instance, in place or not.
							crate::announce::Event::Takeover { announce, predecessor } => {
								let restart = predecessor == announce.prefix;
								(announce, true, restart)
							}
							crate::announce::Event::End(update) => (update, false, false),
						})));
					}
					if retry.poll(waiter).is_ready() {
						return Poll::Ready(NamespaceEvent::Retry);
					}
					Poll::Pending
				})
				.await
			};

			match event {
				// Only a solicited loop has a stream to close, and its request task closes the
				// session over a protocol violation.
				NamespaceEvent::Closed(res) => break res,
				NamespaceEvent::Request(mut update) => {
					if let Err(err) = self.control.accept(update.request_id) {
						if matches!(err, Error::GoingAway) {
							update.refused = Some(err);
						} else {
							self.session.clone().close(
								super::error::to_session_code(&crate::SessionError::from(&err)),
								"invalid request ID",
							);
							break Err(err);
						}
					}
					let refusal = update
						.refused
						.map(|error| ietf::RequestError {
							error_code: request::to_code(&error, request::Kind::SubscribeNamespace, self.version),
							reason_phrase: "namespace update refused".into(),
							retry_interval: 0,
						})
						.or_else(|| {
							update
								.prefix
								.as_ref()
								.and_then(|prefix| super::namespace::refusal(prefix, self.version))
						});
					if let Some(error) = refusal {
						let stream = ns.target.stream().expect("update has a stream");
						stream.writer.encode_message(&error).await?;
						// Dropping a finished writer resets it, which could discard the refusal.
						stream.writer.finish()?;
						break stream.writer.closed().await;
					}
					if let Some(next) = update.prefix.filter(|next| next != &prefix) {
						// DONE still names the old prefix. After OK every suffix is relative
						// to the new prefix, so replay the new cursor only after acknowledging.
						let held: Vec<_> = ns
							.watched
							.iter()
							.filter(|(_, watch)| watch.sent.wanted())
							.map(|(suffix, _)| suffix.clone())
							.collect();
						for suffix in held {
							self.withdraw_namespace(&mut ns.target, &mut ns.requests, suffix)
								.await?;
						}
						ns.watched.clear();
						prefix = next;
						let scope = crate::Pattern::subtree(prefix.as_str())
							.map(|p| p.rebase(prefix.as_str()))
							.unwrap_or_default();
						let origin = self
							.origin
							.scope(&prefix, &scope)
							.unwrap_or_else(|_| self.origin.empty());
						announced = self.advertising(origin, &ns.peer).discovery(ns.hidden).announced();
						retry_at = None;
						retry_delay = RETRY_BASE;
					}
					let stream = ns.target.stream().expect("update has a stream");
					stream
						.writer
						.encode_message(&ietf::RequestOk {
							largest: None,
							active: None,
						})
						.await?;
				}
				NamespaceEvent::Retry => {
					retry_at = None;
					retry_delay = (retry_delay * 2).min(RETRY_MAX);

					// A minimum wait the peer named is enforced by `sync_namespace`, which
					// every path goes through, so a namespace still inside one simply
					// makes no offer this turn. The next sweep is at most RETRY_MAX away.
					let deferred: Vec<crate::Path> = ns
						.watched
						.iter()
						.filter(|(_, watch)| watch.deferred)
						.map(|(suffix, _)| suffix.clone())
						.collect();

					for suffix in deferred {
						let path = prefix.rejoin(&suffix);
						self.sync_namespace(&mut ns, &suffix, &path).await?;
					}
				}
				NamespaceEvent::Regrant(permit) => {
					ns.permit = permit;
					let suffixes: Vec<crate::Path> = ns.watched.keys().cloned().collect();
					for suffix in suffixes {
						let path = prefix.rejoin(&suffix);
						self.sync_namespace(&mut ns, &suffix, &path).await?;
					}
				}
				NamespaceEvent::Renew(request_id) => self.send_update(&mut ns, &prefix, request_id).await?,
				NamespaceEvent::Answered(suffix, answer) => self.answered(&mut ns, &prefix, suffix, answer).await?,
				NamespaceEvent::Update(None) => {
					// The origin is gone: withdraw everything, then finish the
					// stream and wait for delivery.
					self.withdraw_requests(&mut ns.target, &mut ns.requests).await;
					let Some(stream) = ns.target.stream() else {
						return Ok(());
					};
					stream.writer.finish()?;
					return stream.writer.closed().await;
				}
				NamespaceEvent::Update(Some((update, active, restart))) => {
					// moq-transport has no restart: the peer sees the namespace end and start
					// again, and resubscribes. One that tells instances apart (MoQ Epoch) takes
					// another epoch in place instead, which `sync_namespace` makes before it
					// breaks the old.
					if restart && !Self::replaces(&ns, &update) {
						self.apply_update(&mut ns, &prefix, update.clone(), false).await?;
					}
					self.apply_update(&mut ns, &prefix, update, active).await?;
				}
			}
		};

		// This loop's advertisements die with it.
		self.withdraw_requests(&mut ns.target, &mut ns.requests).await;

		res
	}

	/// Whether a restart onto `update` replaces one instance the peer holds by epoch with
	/// another, rather than ending the advertisement.
	fn replaces(ns: &Namespaces, update: &crate::announce::Announce) -> bool {
		let held = ns
			.watched
			.get(&update.prefix)
			.and_then(|watch| watch.sent.params().epoch);
		ns.epochs && held.is_some() && update.route.epoch.is_some() && held != update.route.epoch
	}

	/// Reconcile what the peer holds with one route (un)announce.
	async fn apply_update(
		&self,
		ns: &mut Namespaces,
		prefix: &crate::Path,
		update: crate::announce::Announce,
		active: bool,
	) -> Result<(), Error> {
		let suffix = update.prefix;
		let path = prefix.rejoin(&suffix);

		if active {
			// A repeat for a live suffix is a metadata update: keep the
			// peer's refusal state and re-run the selection.
			match ns.watched.get_mut(&suffix) {
				Some(watch) => watch.route = update.route,
				None => {
					ns.watched.insert(suffix.clone(), Watched::new(update.route));
				}
			}
			self.sync_namespace(ns, &suffix, &path).await
		} else {
			// Only close out namespaces the peer actually saw.
			let held = ns.watched.remove(&suffix).is_some_and(|watch| watch.sent.wanted());
			if held {
				tracing::debug!(route = %crate::path::Rooted(self.origin.root(), &path), "namespace_done");
				self.withdraw_namespace(&mut ns.target, &mut ns.requests, suffix)
					.await?;
			}
			Ok(())
		}
	}
}

/// The track's end, shared by a subscription's group writers so the one serving the end's
/// group can carry the END_OF_TRACK inline.
#[derive(Default)]
struct Terminal {
	end: std::sync::OnceLock<track::Position>,
	/// A group stream carrying the marker was acknowledged, so no standalone one is needed.
	sent: AtomicBool,
}

struct SubscriptionFill {
	fill: FillServe,
	largest: Option<Location>,
	request_id: RequestId,
	track: track::Consumer,
	ranking: Ranking<GroupOf<Transport>>,
	timescale: Option<Timescale>,
	opened: Arc<AtomicU64>,
}

/// The current group's tail, even when its head was filled rather than arrived live.
struct FillTail {
	start: Location,
	fetch: track::Fetching,
}

/// The stream serving a subscription's highest group, as it was opened.
struct LiveGroup {
	sequence: u64,
	slice: GroupSlice,
	/// Set once a narrowing gave the stream up before its FIN.
	retired: Arc<AtomicBool>,
}

/// Serves a track's groups, one machine per group. One at a time waits for stream credit;
/// the rest wait in the cache, so what a peer withholding credit makes this hold is bounded
/// by the credit it grants, not by how long it withholds it.
struct TrackServe {
	session: Transport,
	track: track::Subscriber,
	request_id: RequestId,
	version: Version,
	range: ServeRange,
	requested: ServeRange,
	/// The live cursor's lowest group, which a fill-only update never lowers.
	floor: u64,
	/// Active group streams also observe narrowing, even while waiting for another frame.
	bounds: kio::Producer<ServeRange>,
	filter: Filter,
	timescale: Option<Timescale>,
	/// The one group waiting for stream credit. The cursor hands out no other meanwhile, so
	/// the backlog stays in the cache, which bills, ages, and evicts it, and the cursor's
	/// budget skips whatever went stale.
	opening: Option<GroupServe>,
	tail: Option<Box<FillTail>>,
	/// The stream serving the highest group, where a widening fill's tail resumes.
	live: Option<LiveGroup>,
	/// The groups with a stream, serving or awaiting the acknowledgement of their FIN.
	children: kio::Tasks<GroupServe>,
	/// The subscriber priority, which every group stream follows while it is open.
	priority: kio::Producer<i8>,
	/// Where the group streams are ranked: the subscription's send group, or the session's
	/// queue once [`Self::ranked`] on a transport without groups.
	ranking: Ranking<GroupOf<Transport>>,
	/// The track finished: the in-flight group machines drain, then FIN.
	draining: bool,
	/// Group streams opened, shared with the group machines.
	opened: Arc<AtomicU64>,
	/// The track's exclusive end, once its groups ran out because it finished.
	end: Option<track::Position>,
	terminal: Arc<Terminal>,
	/// Serve the track's datagrams too, as OBJECT_DATAGRAMs. Off when the transport has no
	/// datagrams; there is no stream fallback.
	datagrams: bool,
	compression: bool,
	/// A track that always has another group ready still lets the session's other tasks run.
	budget: kio::coop::Budget,
	/// Whether the track advertised DYNAMIC_GROUPS, which an update's NEW_GROUP_REQUEST
	/// needs.
	dynamic: bool,
}

impl TrackServe {
	fn new(
		session: Transport,
		track: track::Subscriber,
		request_id: RequestId,
		version: Version,
		range: ServeRange,
		timescale: Option<Timescale>,
	) -> Self {
		let floor = match range.start {
			Some(start) => start.group,
			None => track.latest_sequence().unwrap_or(0),
		};
		let end = range.end.map_or(Bound::Unbounded, |end| Bound::Included(end.group));
		let track = track.with_range((Bound::Included(floor), end));
		let datagrams = session.max_datagram_size() > 0;
		let dynamic = track.info().refresh.is_some();
		let priority = kio::Producer::new(track.subscription().priority);
		let publisher = track.info().priority;
		let session = session;
		let ranking = Ranking {
			queue: PriorityQueue::default(),
			group: session.send_group(crate::priority::group(track.subscription().priority, publisher)),
			request_id,
			priority: priority.consume(),
			publisher,
			order: track.subscription().order,
		};

		Self {
			datagrams,
			compression: false,
			session,
			track,
			request_id,
			version,
			range,
			requested: range,
			floor,
			bounds: kio::Producer::new(range),
			filter: Filter::Unfiltered,
			timescale,
			opening: None,
			tail: None,
			live: None,
			children: kio::Tasks::new(),
			priority,
			ranking,
			draining: false,
			opened: Default::default(),
			end: None,
			terminal: Default::default(),
			budget: kio::coop::Budget::new(32),
			dynamic,
		}
	}

	fn compression(mut self, negotiated: bool) -> Self {
		self.compression = negotiated && self.track.info().compress;
		self
	}

	/// Rank this subscription's group streams in `queue`, shared with the session's other
	/// subscriptions. Without it they are only ranked against each other.
	fn ranked(mut self, queue: PriorityQueue) -> Self {
		self.ranking.queue = queue;
		self
	}

	/// Replace the requested range and keep its filled head separate from the live tail.
	fn update_range(&mut self, update: &super::request_stream::Update) -> Result<Option<SubscriptionFill>, Error> {
		let cache = self.track.consume();
		let edge = live_edge(&cache);
		if let Some(filter) = update.filter {
			self.filter = filter;
			// Unfiltered on an update widens back to zero. A fresh live join here
			// would discard history before an empty relay can request its fill.
			self.requested = match filter {
				Filter::Unfiltered if Filter::is_draft20(self.version) => ServeRange {
					start: Some(Location { group: 0, object: 0 }),
					end: None,
				},
				_ => filter_range(filter, edge),
			};
		}
		let mut range = self.requested;
		if let Ok(mut bounds) = self.bounds.write() {
			*bounds = range;
		}
		let mut subscription = self.track.subscription();
		range.apply(&mut subscription);
		self.track.update(subscription)?;
		let fill = update.fill.map(|fill| fill_range(fill, self.filter, edge.largest));
		if let Some(next) = range.fill_tail(fill, edge) {
			let options = group::Fetch {
				priority: self.track.subscription().priority,
				max_delay: self.track.subscription().max_delay,
				..Default::default()
			};
			// A stream still serving this group from the fill's edge keeps what it covers, so
			// the tail resumes where that stream stops and nothing goes out twice.
			let live = self.live.as_ref().filter(|live| {
				live.sequence == next.group
					&& live.slice.skip <= next.object
					&& !live.retired.load(Ordering::Relaxed)
					&& !range.excludes(live.sequence, live.slice)
			});
			let start = match live {
				Some(live) => live.slice.until.map(|until| Location {
					group: next.group,
					object: next.object.max(until),
				}),
				None => Some(next),
			};
			self.tail = start
				.filter(|start| {
					range.end.is_none_or(|end| {
						end.group > start.group
							|| (end.group == start.group && end.object.is_none_or(|object| object >= start.object))
					})
				})
				.map(|start| {
					Box::new(FillTail {
						start,
						fetch: cache.fetch_group(start.group, options).into_inner(),
					})
				});
			// The tail has its own cursor: fetched groups are not live arrival entries.
			range.start = Some(Location {
				group: next.group.saturating_add(1),
				object: 0,
			});
		}
		let mut start = range.start.map_or(0, |start| start.group);
		// Below its floor the live cursor already passed every group or never owned it, so
		// lowering it for a fill-only update would hand those groups out a second time.
		if update.filter.is_none() {
			start = start.max(self.floor);
		}
		self.floor = start;
		let end = range.end.map_or(Bound::Unbounded, |end| Bound::Included(end.group));
		self.track.set_range((Bound::Included(start), end));
		self.range = range;
		self.draining = false;
		Ok(fill.map(|fill| SubscriptionFill {
			fill,
			largest: edge.largest,
			request_id: update.request_id,
			track: cache,
			ranking: self.ranking.clone(),
			timescale: self.timescale,
			opened: self.opened.clone(),
		}))
	}

	/// Move the subscriber priority, for the group streams already open as well as later ones.
	fn set_priority(&mut self, priority: i8) -> Result<(), Error> {
		let mut subscription = self.track.subscription();
		subscription.priority = priority;
		self.track.update(subscription)?;
		// Only a closed channel refuses the write, and this is the producer, which never closes it.
		if let Ok(mut current) = self.priority.write() {
			*current = priority;
		}
		// One move for the whole subscription; its streams keep their place within it.
		if let Some(group) = &mut self.ranking.group {
			group.set_priority(crate::priority::group(priority, self.ranking.publisher));
		}
		Ok(())
	}

	/// The subscriber's NEW_GROUP_REQUEST counts as a join, judged by the track's refresh
	/// ratio rather than forwarded as is: every hop's cache holds the same group. One
	/// naming a group already held asks nothing, since that group is on its way.
	fn request_new_group(&self, value: u64) {
		if value != 0 && self.track.latest_sequence().is_some_and(|latest| value <= latest) {
			return;
		}
		self.track.join();
	}

	/// Group streams opened so far.
	fn opened(&self) -> u64 {
		self.opened.load(Ordering::Relaxed)
	}

	/// Close every group stream still in flight. Dropping a group machine resets its
	/// stream, as a cancelled subscription's would; a drained track has none left.
	fn close_groups(&mut self) {
		self.tail = None;
		self.opening = None;
		self.children = kio::Tasks::new();
	}

	/// Group machines held: the one waiting for stream credit, and those with a stream.
	#[cfg(test)]
	fn machines(&self) -> usize {
		self.children.len() + usize::from(self.opening.is_some())
	}

	/// Where the track ends, when the subscription ran to that end rather than stopping at
	/// its own range first.
	fn end(&self) -> Option<track::Position> {
		let end = self.end?;
		if self.terminal.sent.load(Ordering::Relaxed) {
			return None;
		}
		let reached = self
			.range
			.end
			.is_none_or(|last| last.group.saturating_add(1) >= end.group.saturating_add(u64::from(end.frame != 0)));
		reached.then_some(end)
	}

	/// Send the original terminal on a separate stream when its group already sent FIN,
	/// ranked as the group it ends.
	async fn write_end_of_track(&mut self, end: track::Position) -> Result<(), Error> {
		let mut rank = self.ranking.rank(Location {
			group: end.group,
			object: end.frame,
		});
		let options = rank.options();
		let stream = kio::wait(|waiter| self.session.poll_open_uni(waiter, options))
			.await
			.map_err(Error::from_transport)?;
		self.opened.fetch_add(1, Ordering::Relaxed);
		let stream = Ranked::new(stream, Some(rank));
		let mut writer = Writer::new(stream, self.version);
		writer.buffer(&ietf::GroupHeader {
			track_alias: self.request_id.0,
			group_id: end.group,
			sub_group_id: 0,
			publisher_priority: super::priority::to_wire(self.track.info().priority),
			flags: ietf::GroupFlags {
				has_end: false,
				first_object: end.frame == 0,
				..Default::default()
			},
		})?;
		writer.buffer_varint(end.frame)?;
		writer.buffer_varint(0)?;
		writer.varint(END_OF_TRACK).await?;
		settle(&mut writer).await
	}

	fn serve_group(&mut self, group: group::Consumer, slice: GroupSlice) -> GroupServe {
		let group = if self.compression { group.compressed() } else { group };
		let sequence = group.sequence;
		let wire = group.wire();
		let msg = ietf::GroupHeader {
			track_alias: self.request_id.0,
			group_id: sequence,
			sub_group_id: 0,
			// The publisher's own ranking of its tracks, which is what a relay
			// prefers when it can't pick between its subscribers' priorities. The
			// model ranks higher-first and this wire field lower-first.
			publisher_priority: super::priority::to_wire(self.track.info().priority),
			// Carry per-object timestamps as extension headers (the Timestamp
			// Object Property) only when TIMESCALE was declared.
			flags: ietf::GroupFlags {
				has_extensions: self.compression
					|| self.timescale.is_some()
					|| wire.as_ref().is_some_and(|wire| wire.extensions),
				first_object: wire
					.as_ref()
					.map_or(slice.skip == 0, |wire| wire.first_object && slice.skip <= wire.first_id),
				// A stream capped by the range stops before the group may end, so its
				// FIN cannot claim END_OF_GROUP.
				has_end: slice.until.is_none() && wire.as_ref().is_none_or(|wire| wire.has_end),
				..Default::default()
			},
		};

		let rank = self.ranking.rank(Location {
			group: sequence,
			object: slice.skip,
		});
		let retired = Arc::new(AtomicBool::new(false));
		if self.live.as_ref().is_none_or(|live| sequence >= live.sequence) {
			self.live = Some(LiveGroup {
				sequence,
				slice,
				retired: retired.clone(),
			});
		}
		GroupServe::new(
			self.session.clone(),
			msg,
			rank,
			group,
			self.timescale,
			self.version,
			slice,
		)
		.counted(self.opened.clone())
		.terminal(self.terminal.clone())
		.bounded(self.bounds.consume(), retired)
	}

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		// Before the groups, so one finishing on this poll sees an end declared beside it.
		if self.terminal.end.get().is_none()
			&& let Poll::Ready(Ok(end)) = self.track.poll_finished(waiter)
		{
			let _ = self.terminal.end.set(end);
		}
		if self.draining {
			return self.children.poll(waiter).map(Ok);
		}

		let _ = self.children.poll(waiter);
		loop {
			// Still serves the datagrams and the groups already started below.
			if self.budget.poll_yield(waiter).is_pending() {
				break;
			}
			if self.opening.is_none()
				&& let Some(tail) = self.tail.as_mut()
			{
				match kio::Task::poll(&mut tail.fetch, waiter) {
					Poll::Ready(result) => {
						let tail = self.tail.take().expect("polled above");
						if let Ok(group) = result {
							let until = self
								.range
								.end
								.filter(|end| end.group == tail.start.group)
								.and_then(|end| end.object)
								.map(|end| end.saturating_add(1));
							self.opening = Some(self.serve_group(
								group,
								GroupSlice {
									skip: tail.start.object,
									until,
								},
							));
						}
					}
					Poll::Pending => {}
				}
			}
			if let Some(opening) = &mut self.opening {
				let Poll::Ready(res) = opening.poll_open(waiter) else {
					break;
				};
				let opening = self.opening.take().expect("polled above");
				// A group that failed without a stream has nothing to reset.
				if res.is_ok() {
					self.children.push(opening);
				}
			}
			match self.track.poll_recv_group(waiter) {
				Poll::Ready(Ok(Some(group))) => {
					let sequence = group.sequence;
					tracing::debug!(subscribe = %self.request_id, track = %self.track.name(), sequence, "serving group");

					let slice = self.range.slice(sequence);
					if slice.until.is_some_and(|until| until <= slice.skip) {
						continue;
					}

					self.opening = Some(self.serve_group(group, slice));
				}
				Poll::Ready(Ok(None)) => {
					if self.tail.is_some() {
						return Poll::Pending;
					}
					// Exhausting an updated filter does not end the track. Keep the
					// request updatable once its current range runs out.
					if self.range.end.is_some() && self.track.poll_finished(waiter).is_pending() {
						let _ = self.children.poll(waiter);
						return Poll::Pending;
					}
					// Datagrams written before the track finished still go out.
					self.poll_datagrams(waiter)?;
					self.draining = true;
					if let Poll::Ready(Ok(end)) = self.track.poll_finished(waiter) {
						self.end = Some(end);
					}
					return self.children.poll(waiter).map(Ok);
				}
				Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
				Poll::Pending => break,
			}
		}
		// Groups first, so a burst of datagrams cannot starve them.
		self.poll_datagrams(waiter)?;
		// Newly created group machines start now rather than on the next wake.
		let _ = self.children.poll(waiter);
		Poll::Pending
	}

	/// Send every buffered datagram as an OBJECT_DATAGRAM, best-effort, like moq-lite.
	///
	/// Public datagrams are Object 0 and end their group; relayed ones retain their fields.
	/// The track's end or failure surfaces through its groups, so this only stops.
	fn poll_datagrams(&mut self, waiter: &kio::Waiter) -> Result<(), Error> {
		if !self.datagrams {
			return Ok(());
		}
		while let Poll::Ready(Ok(Some(datagram))) = self.track.poll_recv_wire_datagram(waiter) {
			let sequence = datagram.sequence;
			let id = datagram.wire.as_ref().and_then(|wire| wire.object_id).unwrap_or(0);
			if self
				.range
				.start
				.is_some_and(|start| start.group == sequence && id < start.object)
				|| self
					.range
					.end
					.is_some_and(|end| end.group == sequence && end.object.is_some_and(|end| id > end))
			{
				continue;
			}
			let (object_id, priority, end_of_group, status, properties) = match &datagram.wire {
				Some(wire) => {
					// A datagram this draft cannot carry is dropped like an oversized one: it
					// is best-effort, and the track's groups do not depend on it.
					if wire.status.is_some_and(|status| !super::object::status_defined(status)) {
						tracing::debug!(sequence, "dropping a datagram status this draft cannot carry");
						continue;
					}
					// A status keeps its meaning without the properties this draft forbids
					// beside it, as a stream's status object does.
					let stripped = Default::default();
					let forwarded = match wire.status.is_some_and(|status| status != 0) {
						true => &stripped,
						false => &wire.properties,
					};
					let properties = match super::object::write(
						forwarded,
						self.track.info().ietf(),
						datagram.datagram.timestamp,
						self.version,
						self.timescale,
					) {
						Ok(properties) => properties,
						Err(err) => {
							tracing::debug!(%err, sequence, "dropping a datagram whose properties cannot be forwarded");
							continue;
						}
					};
					let object = wire.object_id;
					// An omitted priority inherited the source track's default, which this
					// response may not carry (or carry the same), so resolve it here.
					let priority = wire
						.priority
						.or_else(|| Some(super::priority::to_wire(self.track.info().priority)));
					(
						object,
						priority,
						wire.end_of_group,
						wire.status,
						(!properties.is_empty()).then_some(properties),
					)
				}
				None => {
					let mut properties = Vec::new();
					if let Some(timescale) = self.timescale {
						let timestamp = datagram.timestamp.ok_or(Error::TimestampMismatch)?;
						ietf::encode_object_time(
							&mut Encoder::new(&mut properties, self.version.into()),
							timestamp,
							timescale,
						)?;
					}
					(
						None,
						Some(super::priority::to_wire(self.track.info().priority)),
						true,
						None,
						(!properties.is_empty()).then_some(properties),
					)
				}
			};
			let (payload, inflated) = datagram.sent(self.compression);
			let properties = super::object::compression(properties.unwrap_or_default(), self.version, inflated)?;
			let properties = (!properties.is_empty()).then_some(properties);
			let body = ietf::ObjectDatagram {
				track_alias: self.request_id.0,
				group_id: sequence,
				object_id,
				publisher_priority: priority,
				end_of_group,
				properties,
				body: match status {
					Some(status) => ietf::DatagramBody::Status(status),
					None => ietf::DatagramBody::Payload(payload.clone()),
				},
			};
			let body = match body.encode_bytes(self.version) {
				Ok(body) => body,
				Err(err) => {
					tracing::debug!(%err, sequence, "dropping a datagram this draft cannot encode");
					continue;
				}
			};
			let max = self.session.max_datagram_size();
			if body.len() > max {
				tracing::debug!(
					sequence,
					size = body.len(),
					max,
					"dropping datagram larger than the transport limit"
				);
				continue;
			}
			// Ranked like a stream carrying the group from its start, so it merges into the
			// subscription's group order and wins a tie with that group's stream.
			let options = SendOptions {
				group: self.ranking.group.as_ref(),
				priority: crate::priority::in_group(sequence, 0, self.ranking.order),
			};
			let _ = self.session.send_datagram(body, options);
		}
		Ok(())
	}
}

/// Where one request's streams are ranked: the session's queue, keyed by the request's id
/// and following its subscriber priority.
#[derive(Clone)]
struct Ranking<G> {
	queue: PriorityQueue,
	/// The request's send group, which replaces the queue on a transport that has them.
	group: Option<G>,
	request_id: RequestId,
	priority: kio::Consumer<i8>,
	/// The publisher's priority for the track, breaking ties between requests at one
	/// subscriber priority.
	publisher: i8,
	/// The request's GROUP_ORDER, fixed for its life: no update carries it.
	order: Order,
}

impl<G: Clone> Ranking<G> {
	/// A stream carrying a group from `start` on, ranked among the request's others.
	fn rank(&self, start: Location) -> Rank<G> {
		if let Some(group) = &self.group {
			return Rank::Group {
				group: group.clone(),
				order: crate::priority::in_group(start.group, start.object, self.order),
			};
		}
		let applied = *self.priority.read();
		Rank::Queue(Queued {
			handle: self.queue.insert(
				Priority::new(applied, self.request_id.0, start.group)
					.with_publisher(self.publisher)
					.with_order(self.order)
					.with_start(start.object),
			),
			priority: self.priority.clone(),
			applied,
			order: self.order,
		})
	}
}

/// A send stream that follows its [`Rank`] until it settles, for one written by async code
/// rather than a [`GroupServe`] machine, which follows its own.
///
/// Every write and close poll applies a move first. A stream parked on its group still
/// polls its close to notice the subscriber leaving, so a move reaches the transport
/// whatever the stream waits on.
struct Ranked<T: crate::transport::poll::SendStream> {
	// Locked only by `poll_closed`, which observes through `&self` yet must still apply a
	// move; every other method reaches it through `get_mut`, which is free.
	inner: std::sync::Mutex<RankedInner<T>>,
}

struct RankedInner<T: crate::transport::poll::SendStream> {
	stream: T,
	/// Absent for a stream that only carries a refusal.
	rank: Option<Rank<T::Group>>,
}

impl<T: crate::transport::poll::SendStream> Ranked<T> {
	/// Wrap a stream opened with its rank's [options](Rank::options).
	fn new(stream: T, rank: Option<Rank<T::Group>>) -> Self {
		Self {
			inner: std::sync::Mutex::new(RankedInner { stream, rank }),
		}
	}

	fn get_mut(&mut self) -> &mut RankedInner<T> {
		self.inner.get_mut().unwrap()
	}
}

impl<T: crate::transport::poll::SendStream> RankedInner<T> {
	fn follow(&mut self, waiter: &kio::Waiter) {
		if let Some(rank) = &mut self.rank
			&& let Some(order) = rank.poll_moved(waiter)
		{
			self.stream.set_priority(order);
		}
	}
}

impl<T: crate::transport::poll::SendStream> crate::transport::poll::SendStream for Ranked<T> {
	type Error = T::Error;
	type Group = T::Group;

	fn poll_write(&mut self, waiter: &kio::Waiter, buf: &[u8]) -> Poll<Result<usize, Self::Error>> {
		let inner = self.get_mut();
		inner.follow(waiter);
		inner.stream.poll_write(waiter, buf)
	}

	fn poll_write_buf<B: bytes::Buf>(&mut self, waiter: &kio::Waiter, buf: &mut B) -> Poll<Result<usize, Self::Error>> {
		let inner = self.get_mut();
		inner.follow(waiter);
		inner.stream.poll_write_buf(waiter, buf)
	}

	fn set_priority(&mut self, order: i64) {
		self.get_mut().stream.set_priority(order);
	}

	fn set_group(&mut self, group: &Self::Group) {
		self.get_mut().stream.set_group(group);
	}

	fn finish(&mut self) -> Result<(), Self::Error> {
		self.get_mut().stream.finish()
	}

	fn reset(&mut self, code: u32) {
		self.get_mut().stream.reset(code);
	}

	fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<Result<(), Self::Error>> {
		let mut inner = self.inner.lock().unwrap();
		inner.follow(waiter);
		inner.stream.poll_closed(waiter)
	}
}

/// Finish a ranked stream and wait for the peer to acknowledge it, holding its rank until then.
///
/// Unlike [`Writer::close`], the writer keeps the stream while it waits, so cancelling resets
/// the stream as a cancelled group stream's is, rather than leaving it sending once its rank
/// is released. Once acknowledged, the stream is released and dropping the writer resets nothing.
async fn settle<T: crate::transport::poll::SendStream>(writer: &mut Writer<Ranked<T>, Version>) -> Result<(), Error> {
	writer.finish()?;
	kio::wait(|waiter| writer.poll_close(waiter)).await
}

/// A group stream's place in the session's send order: its subscription's priority, then
/// its sequence in the subscription's GROUP_ORDER.
enum Rank<G> {
	/// In the request's send group, at a place fixed for the stream's life: the request's
	/// priority moves the whole group.
	Group { group: G, order: i64 },
	/// In the session's queue, on a transport without send groups.
	Queue(Queued),
}

impl<G> Rank<G> {
	/// A group ranked only against itself, for driving one group machine alone.
	#[cfg(test)]
	fn solo(priority: kio::Consumer<i8>) -> Self {
		Self::Queue(Queued::solo(priority))
	}

	/// Where a newly opened stream goes.
	fn options(&mut self) -> SendOptions<'_, G> {
		match self {
			Self::Group { group, order } => SendOptions {
				group: Some(&*group),
				priority: *order,
			},
			Self::Queue(queued) => SendOptions {
				group: None,
				priority: queued.handle.send_order().into(),
			},
		}
	}

	/// A send order for the stream once the subscription priority or the queue moved it.
	fn poll_moved(&mut self, waiter: &kio::Waiter) -> Option<i64> {
		match self {
			Self::Group { .. } => None,
			Self::Queue(queued) => queued.poll_moved(waiter).map(Into::into),
		}
	}
}

/// A stream's entry in the session's queue, following its subscription's priority.
struct Queued {
	handle: PriorityHandle,
	/// The subscription's priority, followed while the stream is open.
	priority: kio::Consumer<i8>,
	/// The priority last applied to [`Self::handle`].
	applied: i8,
	/// The subscription's GROUP_ORDER, fixed for its life: no update carries it.
	order: Order,
}

impl Queued {
	/// A group ranked only against itself, for driving one group machine alone.
	#[cfg(test)]
	fn solo(priority: kio::Consumer<i8>) -> Self {
		let applied = *priority.read();
		Self {
			handle: PriorityQueue::default().insert(Priority::new(applied, 0, 0)),
			priority,
			applied,
			order: Order::Descending,
		}
	}

	/// A send order for the stream once the subscription priority or the queue moved it.
	fn poll_moved(&mut self, waiter: &kio::Waiter) -> Option<u8> {
		let applied = self.applied;
		let mut moved = None;
		if let Poll::Ready(Ok(priority)) = self.priority.poll(waiter, |priority| match **priority == applied {
			true => Poll::Pending,
			false => Poll::Ready(**priority),
		}) {
			self.applied = priority;
			moved = Some(PriorityHandle::send_order_of(self.handle.steer(priority, self.order)));
		}
		while let Poll::Ready(rank) = self.handle.poll_next(waiter) {
			moved = Some(PriorityHandle::send_order_of(rank));
		}
		moved
	}
}

/// Why to stop writing `group` while the transport parks it: the subscription's budget gave
/// it up, or the frames this cursor has yet to read are gone with no route to continue them.
/// A clean finish is no reason.
///
/// Waking on every frame written to a parked group costs what a reader parked at the live
/// edge already pays: each of its pending reads judges the budget too.
fn poll_given_up(group: &mut group::Consumer, waiter: &kio::Waiter) -> Option<Error> {
	if group.poll_expired(waiter) {
		return Some(Error::Old);
	}
	match group.poll_finished(waiter) {
		Poll::Ready(Err(err)) => Some(err),
		_ => None,
	}
}

/// Serves one group on its own unidirectional stream in the moq-transport
/// subgroup format.
struct GroupServe {
	session: Transport,
	/// The subscription's count of opened streams, bumped once this one opens.
	opened: Arc<AtomicU64>,
	msg: ietf::GroupHeader,
	rank: Rank<GroupOf<Transport>>,
	group: group::Consumer,
	timescale: Option<Timescale>,
	version: Version,
	prior_object: Option<u64>,
	slice: GroupSlice,
	bounds: Option<kio::Consumer<ServeRange>>,
	/// Set when a narrowing gives this stream up.
	retired: Arc<AtomicBool>,
	ended: bool,
	terminal: Option<Arc<Terminal>>,
	marked: bool,
	state: GroupState,
	/// A long cached group drains a slice per poll, so its siblings still run.
	budget: kio::coop::Budget,
}

#[cfg_attr(
	target_pointer_width = "64",
	expect(
		clippy::large_enum_variant,
		reason = "a state machine's enum is its storage: one transient instance per stream, so the big variant is the working state, not padding held in bulk"
	)
)]
enum GroupState {
	/// Waiting for stream credit on this machine's own session handle.
	Open,
	/// Streaming objects: the write buffer drains first, then the pending chunk,
	/// then the pending frame, then the next frame.
	Serve {
		writer: Writer<crate::transport::boxed::SendStream, Version>,
		frame: Option<frame::Consumer>,
		chunk: Option<crate::frame::Payload>,
		batch: Box<arrayvec::ArrayVec<frame::Object, 8>>,
		batch_pos: usize,
	},
	/// Every frame is written and the FIN sent: wait for the acknowledgement so a
	/// late cancel or expiry can still reset the stream.
	Closed {
		writer: Writer<crate::transport::boxed::SendStream, Version>,
	},
	Done,
}

impl kio::Task for GroupServe {
	type Output = ();

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		// Errors just drop the writer, whose Drop resets the stream, exactly like
		// the old future being discarded.
		ready!(self.poll_serve(waiter)).map(|()| ()).unwrap_or(());
		Poll::Ready(())
	}
}

impl GroupServe {
	fn new(
		session: Transport,
		msg: ietf::GroupHeader,
		rank: Rank<GroupOf<Transport>>,
		mut group: group::Consumer,
		timescale: Option<Timescale>,
		version: Version,
		slice: GroupSlice,
	) -> Self {
		if group.wire().is_none() {
			// Only the end: a start would clamp past a prefix the group never held,
			// which `skip_to` leaves to fail as `Lagged`.
			group.skip_to(slice.skip);
			group = group.with_end(slice.until);
		}
		Self {
			session,
			opened: Default::default(),
			msg,
			rank,
			group,
			timescale,
			version,
			prior_object: None,
			slice,
			bounds: None,
			retired: Default::default(),
			ended: false,
			terminal: None,
			marked: false,
			state: GroupState::Open,
			budget: kio::coop::Budget::new(32),
		}
	}

	fn bounded(mut self, bounds: kio::Consumer<ServeRange>, retired: Arc<AtomicBool>) -> Self {
		self.bounds = Some(bounds);
		self.retired = retired;
		self
	}

	/// A narrowed filter gives back a stream still writing objects it now excludes, whole
	/// group or within it, without ending its siblings. A stream whose FIN is already sent
	/// keeps the objects it delivered.
	fn poll_excluded(&self, waiter: &kio::Waiter) -> bool {
		if matches!(self.state, GroupState::Closed { .. }) {
			return false;
		}
		let excluded = self.bounds.as_ref().is_some_and(|bounds| {
			matches!(
				bounds.poll(waiter, |range| {
					if range.excludes(self.msg.group_id, self.slice) {
						Poll::Ready(())
					} else {
						Poll::Pending
					}
				}),
				Poll::Ready(Ok(()))
			)
		});
		if excluded {
			self.retired.store(true, Ordering::Relaxed);
		}
		excluded
	}

	fn terminal(mut self, terminal: Arc<Terminal>) -> Self {
		self.terminal = Some(terminal);
		self
	}

	/// Count this group's stream into `opened` once it opens.
	fn counted(mut self, opened: Arc<AtomicU64>) -> Self {
		self.opened = opened;
		self
	}

	/// Wait for stream credit, then write the group header, leaving the machine serving.
	/// Gives up once the group can no longer be sent, so a parked group never spends the
	/// credit it was waiting for on a reset.
	fn poll_open(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		if self.poll_excluded(waiter) {
			self.state = GroupState::Done;
			return Poll::Ready(Err(Error::Cancel));
		}
		if !matches!(self.state, GroupState::Open) {
			return Poll::Ready(Ok(()));
		}
		if let Some(err) = poll_given_up(&mut self.group, waiter) {
			self.state = GroupState::Done;
			return Poll::Ready(Err(err));
		}
		// The current rank, so a move while parked applies to the stream as it opens.
		let _ = self.rank.poll_moved(waiter);
		let stream = match ready!(self.session.poll_open_uni(waiter, self.rank.options())) {
			Ok(stream) => stream,
			Err(err) => {
				self.state = GroupState::Done;
				return Poll::Ready(Err(Error::from_transport(err)));
			}
		};
		self.opened.fetch_add(1, Ordering::Relaxed);

		let mut writer = Writer::new(stream, self.version);
		if let Err(err) = writer.buffer(&self.msg) {
			self.state = GroupState::Done;
			return Poll::Ready(Err(err));
		}
		self.state = GroupState::Serve {
			writer,
			frame: None,
			chunk: None,
			batch: Box::new(arrayvec::ArrayVec::new()),
			batch_pos: 0,
		};
		Poll::Ready(Ok(()))
	}

	fn poll_serve(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		if self.poll_excluded(waiter) {
			self.state = GroupState::Done;
			return Poll::Ready(Err(Error::Cancel));
		}
		// A REQUEST_UPDATE can move the subscriber priority while this stream is open, and
		// the session's other group streams can move its rank. Serve and Closed, the only
		// states holding a writer, apply the change; Open reads the current rank once it
		// gets its stream.
		let mut moved = self.rank.poll_moved(waiter);
		loop {
			match &mut self.state {
				GroupState::Open => {
					ready!(self.poll_open(waiter))?;
					// The current rank, which already includes anything `moved` saw.
					moved = None;
				}
				GroupState::Serve {
					writer,
					frame,
					chunk,
					batch,
					batch_pos,
				} => {
					if let Some(priority) = moved.take() {
						writer.set_priority(priority);
					}
					// The peer closing first cancels the group.
					if writer.poll_closed(waiter).is_ready() {
						self.state = GroupState::Done;
						return Poll::Ready(Err(Error::Cancel));
					}
					let res = 'serve: {
						loop {
							ready!(self.budget.poll_yield(waiter));
							match writer.poll_flush(waiter) {
								Poll::Ready(Ok(())) => {}
								Poll::Ready(Err(err)) => break 'serve Err(err),
								// Parking on the transport is the one stall the group cursor cannot
								// see, and the only place a served group applies the drift budget:
								// flow control must not pin a stream that has gone stale, or one
								// whose unread rest the cache evicted.
								Poll::Pending => {
									if let Some(err) = poll_given_up(&mut self.group, waiter) {
										break 'serve Err(err);
									}
									return Poll::Pending;
								}
							}
							if let Some(pending) = chunk {
								match writer.poll_write(waiter, pending) {
									Poll::Ready(Ok(_)) => {
										if !bytes::Buf::has_remaining(pending) {
											*chunk = None;
										}
									}
									Poll::Ready(Err(err)) => break 'serve Err(err),
									// Parking on the transport is the one stall the group cursor cannot
									// see, and the only place a served group applies the drift budget:
									// flow control must not pin a stream that has gone stale, or one
									// whose unread rest the cache evicted.
									Poll::Pending => {
										if let Some(err) = poll_given_up(&mut self.group, waiter) {
											break 'serve Err(err);
										}
										return Poll::Pending;
									}
								}
							} else if let Some(pending) = frame {
								match pending.poll_read_chunk(waiter) {
									Poll::Ready(Ok(Some(next))) => {
										let id = pending
											.fields
											.as_ref()
											.map_or(self.group.index() - 1, |fields| fields.id);
										if id >= self.slice.skip {
											*chunk = Some(next.into());
										}
									}
									Poll::Ready(Ok(None)) => *frame = None,
									Poll::Ready(Err(err)) => break 'serve Err(err),
									Poll::Pending => return Poll::Pending,
								}
							} else if *batch_pos < batch.len() {
								let ordinal = self.group.index() - batch.len() as u64 + *batch_pos as u64;
								let batched = &mut batch[*batch_pos];
								let id = batched.fields.as_ref().map_or(ordinal, |fields| fields.id);
								*batch_pos += 1;
								if self.slice.until.is_some_and(|until| id >= until) {
									break 'serve Ok(());
								}
								if id < self.slice.skip {
									continue;
								}
								let delta = object_delta(self.prior_object, id)?;
								buffer_object(
									writer,
									Object {
										delta,
										timestamp: batched.timestamp,
										size: batched.payload.len() as u64,
										inflated: batched.inflated,
										status: 0,
										fields: batched.fields.as_deref(),
									},
									ObjectEncoding {
										extensions: self.msg.flags.has_extensions,
										timescale: self.timescale,
										version: self.version,
										source: self.group.ietf(),
									},
								)?;
								self.prior_object = Some(id);
								let payload = std::mem::take(&mut batched.frame.payload);
								if !payload.is_empty() {
									*chunk = Some(payload);
								}
								self.group.keep_alive();
							} else {
								match self.group.poll_read_objects(waiter, batch) {
									Poll::Ready(Ok(count)) if count > 0 => {
										*batch_pos = 0;
										continue;
									}
									// Finish through the common EOF path so a terminal status
									// follows batched frames just as it follows streamed frames.
									Poll::Ready(Ok(_)) => {}
									Poll::Ready(Err(err)) => break 'serve Err(err),
									Poll::Pending => {}
								}

								match self.group.poll_next_frame(waiter) {
									Poll::Ready(Ok(Some(next))) => {
										let id =
											next.fields.as_ref().map_or(self.group.index() - 1, |fields| fields.id);
										if self.slice.until.is_some_and(|until| id >= until) {
											break 'serve Ok(());
										}
										if id < self.slice.skip {
											// The skipped partial frame still drains before the next object.
											*frame = Some(next);
											continue;
										}
										buffer_object(
											writer,
											Object {
												delta: object_delta(self.prior_object, id)?,
												timestamp: next.timestamp,
												size: next.size,
												inflated: next.inflated,
												status: 0,
												fields: next.fields.as_deref(),
											},
											ObjectEncoding {
												extensions: self.msg.flags.has_extensions,
												timescale: self.timescale,
												version: self.version,
												source: self.group.ietf(),
											},
										)?;
										self.prior_object = Some(id);
										// An empty object has no payload to stream.
										if next.size > 0 {
											*frame = Some(next);
										}
									}
									Poll::Ready(Ok(None)) => {
										if !self.ended
											&& let Some(end) = self.group.end_object().map(status_fields)
											&& end.id >= self.slice.skip && self
											.slice
											.until
											.is_none_or(|until| end.id < until)
										{
											buffer_object(
												writer,
												Object {
													delta: object_delta(self.prior_object, end.id)?,
													timestamp: None,
													size: 0,
													inflated: None,
													status: 3,
													fields: Some(&end),
												},
												ObjectEncoding {
													extensions: self.msg.flags.has_extensions,
													timescale: self.timescale,
													version: self.version,
													source: self.group.ietf(),
												},
											)?;
											self.ended = true;
											self.prior_object = Some(end.id);
											continue;
										}
										// The track ends right after this group's last object, so
										// the marker rides this stream rather than a separate one.
										if !self.marked
											&& let Some(end) =
												self.terminal.as_ref().and_then(|terminal| terminal.end.get())
											&& end.group == self.msg.group_id && end.frame == self.group.index()
										{
											writer.buffer_varint(object_delta(self.prior_object, end.frame)?)?;
											if self.msg.flags.has_extensions {
												writer.buffer_varint(0)?;
											}
											writer.buffer_varint(0)?;
											writer.buffer_varint(END_OF_TRACK)?;
											self.marked = true;
											continue;
										}
										break 'serve Ok(());
									}
									Poll::Ready(Err(err)) => break 'serve Err(err),
									Poll::Pending => return Poll::Pending,
								}
							}
						}
					};

					let GroupState::Serve { writer, .. } = std::mem::replace(&mut self.state, GroupState::Done) else {
						unreachable!()
					};
					match res {
						Ok(()) => {
							let mut writer = writer;
							match writer.finish() {
								Ok(()) => self.state = GroupState::Closed { writer },
								Err(err) => return Poll::Ready(Err(err)),
							}
						}
						Err(err) => return Poll::Ready(Err(err)),
					}
				}
				GroupState::Closed { writer } => {
					if let Some(priority) = moved.take() {
						writer.set_priority(priority);
					}
					// Wait until everything is acknowledged by the peer so we can still
					// cancel the stream. poll_close releases the stream on completion so
					// the Drop fallback cannot reset the acknowledged stream.
					let res = match writer.poll_close(waiter) {
						Poll::Ready(res) => res,
						// Those bytes still hold the connection until acknowledged, so a
						// group gone stale meanwhile releases them like one still serving:
						// dropping the writer resets the stream.
						Poll::Pending if self.group.poll_expired(waiter) => {
							self.state = GroupState::Done;
							return Poll::Ready(Err(Error::Old));
						}
						Poll::Pending => return Poll::Pending,
					};
					let sequence = self.msg.group_id;
					self.state = GroupState::Done;
					// Only an acknowledged marker spares the standalone one: a stream reset
					// after its FIN may never deliver it.
					if res.is_ok()
						&& self.marked && let Some(terminal) = &self.terminal
					{
						terminal.sent.store(true, Ordering::Relaxed);
					}
					return Poll::Ready(res.map(|()| {
						tracing::debug!(sequence, "finished group");
					}));
				}
				GroupState::Done => return Poll::Ready(Ok(())),
			}
		}
	}
}

use super::object::END_OF_TRACK;

/// An object's header, with private relay fields when it came from an IETF peer.
struct Object<'a> {
	delta: u64,
	timestamp: Option<Timestamp>,
	size: u64,
	inflated: Option<u64>,
	status: u64,
	fields: Option<&'a crate::model::object::Fields>,
}

struct ObjectEncoding {
	extensions: bool,
	timescale: Option<Timescale>,
	version: Version,
	/// The draft the track's publisher spoke, which serialized the objects' properties.
	source: Option<Version>,
}

fn object_delta(prior: Option<u64>, id: u64) -> Result<u64, Error> {
	match prior {
		None => Ok(id),
		Some(prior) => id
			.checked_sub(prior)
			.and_then(|step| step.checked_sub(1))
			.ok_or(Error::MalformedTrack),
	}
}

/// Withdraw one PUBLISH_NAMESPACE request by cancelling it in both directions, best effort:
/// the peer may already be gone. A FIN is only allowed to end SUBSCRIBE_NAMESPACE and
/// SUBSCRIBE_TRACKS.
fn end_request(mut stream: Stream<Transport, Version>) {
	stream.reader.abort(&Error::Cancel);
	stream.writer.abort(&Error::Cancel);
}

/// A status object's fields, without properties, which it may not carry.
fn status_fields(end: std::sync::Arc<crate::model::object::Fields>) -> std::sync::Arc<crate::model::object::Fields> {
	match end.properties.is_empty() {
		true => end,
		false => std::sync::Arc::new(crate::model::object::Fields {
			id: end.id,
			properties: Default::default(),
		}),
	}
}

fn buffer_object<W: crate::transport::poll::SendStream>(
	writer: &mut Writer<W, Version>,
	object: Object<'_>,
	encoding: ObjectEncoding,
) -> Result<(), Error> {
	writer.buffer_varint(object.delta)?;
	if encoding.extensions {
		let ext = match object.fields {
			Some(fields) => super::object::write(
				&fields.properties,
				encoding.source,
				object.timestamp,
				encoding.version,
				encoding.timescale,
			)?,
			None => {
				let mut ext = Vec::new();
				if let Some(timescale) = encoding.timescale {
					let timestamp = object.timestamp.ok_or(Error::TimestampMismatch)?;
					ietf::encode_object_time(
						&mut Encoder::new(&mut ext, encoding.version.into()),
						timestamp,
						timescale,
					)?;
				}
				ext
			}
		};
		let ext = super::object::compression(ext, encoding.version, object.inflated)?;
		writer.buffer_varint(ext.len() as u64)?;
		writer.buffer_raw(&ext);
	}
	writer.buffer_varint(object.size)?;
	if object.size == 0 {
		writer.buffer_varint(object.status)?;
	}
	Ok(())
}

/// One advertisement: the PUBLISH_NAMESPACE request stream it rides on.
struct NamespaceRequest {
	path: crate::Path,
	stream: Stream<Transport, Version>,
	messages: super::request_stream::Messages,
	/// The auth the peer holds the request under: the one it opened with, or the one
	/// its last update carried. Held, so the credential stays presented while it does.
	auth: Option<crate::auth::Credential>,
	/// Waiting in [`Namespaces::updates`] for a request ID.
	queued: bool,
	/// When to give up on the update in flight, which has no answer yet.
	pending: Option<crate::time::Instant>,
	/// Marks this request woken when its stream has an answer.
	waiter: Option<kio::Waiter>,
}

#[cfg(test)]
mod group_priority_test {
	use super::*;
	use crate::coding::Decode;
	use crate::ietf::priority;
	use crate::lite::test_transport::SinkSession;

	#[moq_clock::test]
	async fn standalone_end_of_track_does_not_claim_end_of_group() {
		let log = crate::lite::test_transport::Log::default();
		let session = SinkSession::new(log.clone());
		let track = track::Producer::new(
			std::sync::Arc::new(crate::broadcast::Info::default()),
			"test",
			crate::track::Info::timed(),
		);
		let mut serve = TrackServe::new(
			crate::driver::Transport::new(session.clone()),
			track.subscribe(None),
			RequestId(0),
			Version::Draft22,
			ServeRange::default(),
			None,
		);
		serve.write_end_of_track(track::Position::group(0)).await.unwrap();
		let bytes = log.writes.lock().unwrap().clone();
		let mut decoder = crate::coding::Decoder::new(&bytes, Version::Draft22.into());
		let header = ietf::GroupHeader::decode(&mut decoder, Version::Draft22).unwrap();
		assert!(
			!header.flags.has_end,
			"END_OF_TRACK does not end a group that never existed"
		);
	}

	/// The standalone END_OF_TRACK stream ranks as the group it ends, not at the raw
	/// subscriber priority.
	#[moq_clock::test]
	async fn standalone_end_of_track_ranks_as_its_group() {
		let log = crate::lite::test_transport::Log::default();
		let queue = PriorityQueue::default();
		let track = track::Producer::new(
			std::sync::Arc::new(crate::broadcast::Info::default()),
			"test",
			crate::track::Info::timed(),
		);
		let mut serve = TrackServe::new(
			crate::driver::Transport::new(SinkSession::new(log.clone())),
			track.subscribe(None),
			RequestId(1),
			Version::Draft22,
			ServeRange::default(),
			None,
		)
		.ranked(queue.clone());
		let _higher = queue.insert(Priority::new(i8::MAX, 0, 0));
		serve.write_end_of_track(track::Position::group(0)).await.unwrap();
		assert_eq!(log.priorities(), [254], "second, behind the higher priority");
	}

	/// Cancelled while its FIN waits for the acknowledgement, a ranked stream resets as it
	/// gives up its rank, rather than sending on with a send order the queue has reused.
	#[moq_clock::test]
	async fn a_ranked_stream_cancelled_after_its_fin_resets() {
		let log = crate::lite::test_transport::Log::default();
		let queue = PriorityQueue::default();
		let track = track::Producer::new(
			std::sync::Arc::new(crate::broadcast::Info::default()),
			"test",
			crate::track::Info::timed(),
		);
		let mut serve = TrackServe::new(
			crate::driver::Transport::new(SinkSession::new(log.clone()).with_unacked_fin()),
			track.subscribe(None),
			RequestId(0),
			Version::Draft22,
			ServeRange::default(),
			None,
		)
		.ranked(queue.clone());
		let mut rival = queue.insert(Priority::new(0, 1, 0));
		assert_eq!(rival.current(), 0);

		{
			let mut marker = std::pin::pin!(serve.write_end_of_track(track::Position::group(0)));
			assert!(
				futures::poll!(marker.as_mut()).is_pending(),
				"the FIN is unacknowledged"
			);
			assert_eq!(rival.current(), 1, "the marker holds its rank until acknowledged");
		}

		assert_eq!(log.resets().len(), 1, "cancelling reset the unacknowledged stream");
		assert_eq!(rival.current(), 0, "and released its rank");
	}

	#[moq_clock::test]
	async fn exact_end_of_track_survives_the_final_group_fin() {
		use crate::lite::test_transport::Sent;
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			for finished in [false, true] {
				let log = crate::lite::test_transport::Log::default();
				let mut track = track::Producer::new(
					Arc::new(crate::broadcast::Info::default()),
					"test",
					crate::track::Info::timed(),
				);
				let mut group = track.create_group(group::Info { sequence: 5 }).unwrap();
				for _ in 0..5 {
					group.write_frame(Timestamp::ZERO, b"x".as_slice()).unwrap();
				}
				let mut serve = TrackServe::new(
					crate::driver::Transport::new(SinkSession::new(log.clone())),
					track.subscribe(None),
					RequestId(0),
					version,
					ServeRange::default(),
					None,
				);
				assert!(serve.poll(&kio::Waiter::noop()).is_pending());
				if finished {
					group.finish().unwrap();
					assert!(serve.poll(&kio::Waiter::noop()).is_pending());
					assert!(log.trail().iter().any(|(_, event)| matches!(event, Sent::Finish)));
				}
				track.finish_at(track::Position { group: 5, frame: 5 }, None).unwrap();
				if !finished {
					group.finish().unwrap();
				}
				kio::wait(|waiter| serve.poll(waiter)).await.unwrap();
				if let Some(end) = serve.end() {
					serve.write_end_of_track(end).await.unwrap();
				}
				assert_eq!(serve.opened(), if finished { 2 } else { 1 });
				let marker_stream = usize::from(finished);
				let bytes: Vec<u8> = log
					.trail()
					.into_iter()
					.filter_map(|(id, event)| match event {
						Sent::Write(bytes) if id == marker_stream => Some(bytes),
						_ => None,
					})
					.flatten()
					.collect();
				let mut bytes = bytes::Bytes::from(bytes);
				let header =
					crate::coding::decode_buf(&mut bytes, version, |r, v| ietf::GroupHeader::decode(r, v)).unwrap();
				assert_eq!(header.group_id, 5);
				assert_eq!(header.flags.has_end, !finished);
				if !finished {
					for _ in 0..5 {
						assert_eq!(&bytes.split_to(3)[..], &[0, 1, b'x']);
					}
				}
				assert_eq!(&bytes[..], &[if finished { 5 } else { 0 }, 0, END_OF_TRACK as u8]);
			}
		}
	}

	/// An inline END_OF_TRACK spares the standalone marker only once its stream is
	/// acknowledged: a stream reset after its FIN may never deliver it.
	#[moq_clock::test]
	async fn an_unacknowledged_inline_end_of_track_is_not_sent() {
		for acked in [true, false] {
			let mut session = SinkSession::new(Default::default());
			if !acked {
				session = session.with_unacked_fin();
			}
			let log = session.log.clone();
			let track = track::Producer::new(
				std::sync::Arc::new(crate::broadcast::Info::default()),
				"test",
				crate::track::Info::timed(),
			);
			let mut subscriber = track.subscribe(None);
			let mut old = track.append_group().unwrap();
			old.write_frame(crate::Timestamp::ZERO, b"old".as_slice()).unwrap();
			old.finish().unwrap();
			let group = subscriber.recv_group().await.unwrap().expect("old group");

			// Set directly rather than through the track, which a later group expires.
			let terminal = Arc::new(Terminal::default());
			terminal.end.set(track::Position { group: 0, frame: 1 }).unwrap();
			let mut serve = GroupServe::new(
				crate::driver::Transport::new(session.clone()),
				ietf::GroupHeader {
					track_alias: 0,
					group_id: 0,
					sub_group_id: 0,
					publisher_priority: 0,
					flags: Default::default(),
				},
				Rank::solo(kio::Producer::new(0).consume()),
				group,
				Some(Timescale::MILLI),
				Version::Draft18,
				GroupSlice::default(),
			)
			.terminal(terminal.clone());
			let mut serving = std::pin::pin!(kio::wait(|waiter| serve.poll_serve(waiter)));
			if acked {
				assert!(matches!(futures::poll!(serving.as_mut()), Poll::Ready(Ok(()))));
				assert!(terminal.sent.load(Ordering::Relaxed), "an acknowledged marker was sent");
				continue;
			}
			assert!(
				futures::poll!(serving.as_mut()).is_pending(),
				"the FIN is unacknowledged"
			);
			let written = log.writes.lock().unwrap().clone();
			assert!(
				written.ends_with(&[0, 0, END_OF_TRACK as u8]),
				"the marker was written: {written:?}"
			);
			assert!(!terminal.sent.load(Ordering::Relaxed), "not until acknowledged");

			moq_clock::sim::advance(Duration::from_secs(1)).await;
			let mut edge = track.append_group().unwrap();
			edge.write_frame(crate::Timestamp::from_millis(1000).unwrap(), b"edge".as_slice())
				.unwrap();
			edge.finish().unwrap();
			assert!(matches!(futures::poll!(serving.as_mut()), Poll::Ready(Err(Error::Old))));
			assert!(
				!terminal.sent.load(Ordering::Relaxed),
				"a reset marker leaves the fallback"
			);
		}
	}

	/// A group of `subscribe` ranked in `queue` at `priority`'s current value.
	fn rank_in<G>(queue: &PriorityQueue, priority: &kio::Producer<i8>, subscribe: u64) -> Rank<G> {
		let applied = *priority.read();
		Rank::Queue(Queued {
			handle: queue.insert(Priority::new(applied, subscribe, 0)),
			priority: priority.consume(),
			applied,
			order: Order::Descending,
		})
	}

	/// The model's `Subscription::priority` is higher-first ("higher values preempt
	/// lower ones"), matching the transport trait's send order, so the higher-priority
	/// subscription's group stream must get the higher send order. An inversion here
	/// would transmit the LOWEST-priority track first under contention.
	#[moq_clock::test]
	async fn group_stream_preserves_model_priority() {
		let log = crate::lite::test_transport::Log::default();
		let queue = PriorityQueue::default();

		let track = track::Producer::new(
			std::sync::Arc::new(crate::broadcast::Info::default()),
			"test",
			crate::track::Info::timed(),
		);
		let mut group = track.create_group(group::Info { sequence: 0 }).unwrap();
		group
			.write_frame(crate::Timestamp::from_millis(0).unwrap(), b"hello".as_slice())
			.unwrap();
		group.finish().unwrap();

		let msg = ietf::GroupHeader {
			track_alias: 0,
			group_id: 0,
			sub_group_id: 0,
			publisher_priority: 0,
			flags: Default::default(),
		};

		// Queue both before either opens, so each stream is handed its final rank.
		let high = kio::Producer::new(72);
		let low = kio::Producer::new(-28);
		let mut serves = [(&low, 1), (&high, 0)].map(|(priority, subscribe)| {
			GroupServe::new(
				crate::driver::Transport::new(SinkSession::new(log.clone())),
				msg.clone(),
				rank_in(&queue, priority, subscribe),
				group.consume(),
				Some(Timescale::MILLI),
				Version::Draft18,
				GroupSlice::default(),
			)
		});
		for serve in &mut serves {
			kio::wait(|waiter| serve.poll_serve(waiter)).await.unwrap();
		}

		assert_eq!(
			log.priorities(),
			vec![254, 255],
			"the higher model priority must transmit first"
		);
	}

	/// The publisher's own ranking of its tracks (`track::Info::priority`) is what a relay
	/// prefers when it has no subscriber preference to go on, so it has to reach the wire.
	/// It went out as a flat 0 before, which put catalog, audio, and video in one tier for
	/// every moq-transport peer.
	#[moq_clock::test]
	async fn group_header_carries_the_publisher_priority() {
		let header = serve_group_header(track::Info::timed().with_priority(hang_audio_priority())).await;
		assert_eq!(
			header.publisher_priority,
			priority::to_wire(hang_audio_priority()),
			"the wire is lower-first, so audio must encode below video"
		);
		assert!(
			priority::to_wire(hang_audio_priority()) < priority::to_wire(hang_video_priority()),
			"audio outranks video on the wire"
		);
	}

	/// A track that never set a priority goes out at the midpoint, 127, one step off the
	/// draft's usual 128 so it mirrors moq-lite's 128, and not 255, the least urgent value a
	/// peer like moxygen would deprioritize.
	#[moq_clock::test]
	async fn group_header_defaults_to_the_midpoint() {
		let header = serve_group_header(track::Info::timed()).await;
		assert_eq!(header.publisher_priority, 127);
	}

	/// Drive `serve` beside a sibling that only counts its turns, until `done`, checking every
	/// poll stays under `most` by `progress` and leaves the sibling its turn.
	/// Each task-set pass returns to the executor so its children's yields reach the task.
	async fn serve_beside_a_sibling(
		mut serve: TrackServe,
		mut produce: impl FnMut() -> bool + 'static,
		progress: impl Fn() -> usize,
		most: usize,
		done: usize,
	) {
		type Task = Box<dyn FnMut(&kio::Waiter) -> Poll<()>>;
		let mut tasks: kio::Tasks<Task> = kio::Tasks::new();
		tasks.push(Box::new(move |waiter: &kio::Waiter| {
			if produce() {
				return Poll::Ready(());
			}
			waiter.waker().wake_by_ref();
			Poll::Pending
		}));
		let turns = std::sync::Arc::new(AtomicU64::new(0));
		let counted = turns.clone();
		tasks.push(Box::new(move |waiter: &kio::Waiter| {
			counted.fetch_add(1, Ordering::Relaxed);
			waiter.waker().wake_by_ref();
			Poll::Pending
		}));
		tasks.push(Box::new(move |waiter: &kio::Waiter| {
			serve.poll(waiter).map(|res| res.expect("serve"))
		}));

		let mut polls = 0;
		kio::wait(|owner| {
			if progress() >= done {
				return Poll::Ready(());
			}
			let before = progress();
			assert!(tasks.poll(owner).is_pending(), "the serve never ends");
			polls += 1;
			let made = progress() - before;
			assert!(made <= most, "one poll made {made} progress");
			assert_eq!(turns.load(Ordering::Relaxed), polls, "the sibling missed a turn");
			assert!(polls <= 2 * done as u64, "the serve stalled");
			Poll::Pending
		})
		.await;
	}

	/// A track that always has another group ready yields within its budget.
	#[moq_clock::test]
	async fn a_busy_track_yields_to_its_siblings() {
		/// `TrackServe`'s passes per poll; each starts at most one group.
		const BUDGET: usize = 32;
		const GROUPS: u64 = 10_000;
		const BATCH: u64 = 1_000;

		let session = SinkSession::new(crate::lite::test_transport::Log::default()).with_unacked_fin();
		let track = track::Producer::new(
			std::sync::Arc::new(crate::broadcast::Info::default()),
			"test",
			crate::track::Info::timed(),
		);
		let subscriber =
			track.subscribe(track::Subscription::default().with_max_delay(std::time::Duration::from_secs(3600)));
		let serve = TrackServe::new(
			crate::driver::Transport::new(session.clone()),
			subscriber,
			RequestId(0),
			Version::Draft18,
			ServeRange::default(),
			None,
		);
		let opened = serve.opened.clone();

		let producer = track.clone();
		let mut appended = 0;
		let produce = move || {
			for _ in 0..BATCH.min(GROUPS - appended) {
				let mut group = producer.create_group(group::Info { sequence: appended }).unwrap();
				group
					.write_frame(crate::Timestamp::from_millis(appended).unwrap(), b"x".as_slice())
					.unwrap();
				group.finish().unwrap();
				appended += 1;
			}
			appended == GROUPS
		};
		serve_beside_a_sibling(
			serve,
			produce,
			|| opened.load(Ordering::Relaxed) as usize,
			BUDGET,
			GROUPS as usize,
		)
		.await;
	}

	/// On a transport with send groups, a subscription opens one at its priority and its
	/// group streams join it, ranked newest first by sequence alone: the session-wide queue
	/// takes no part. A priority update moves the group once, not each stream.
	#[test]
	fn grouped_streams_rank_within_their_subscription() {
		let log = crate::lite::test_transport::Log::default();
		let session = SinkSession::new(log.clone()).with_groups().with_unacked_fin();
		let track = track::Producer::new(
			std::sync::Arc::new(crate::broadcast::Info::default()),
			"test",
			crate::track::Info::timed(),
		);
		let subscriber = track.subscribe(
			track::Subscription::default()
				.with_priority(3)
				.with_max_delay(std::time::Duration::from_secs(3600)),
		);
		let mut serve = TrackServe::new(
			crate::driver::Transport::new(session.clone()),
			subscriber,
			RequestId(0),
			Version::Draft18,
			ServeRange::default(),
			None,
		);
		for sequence in 0..2 {
			let mut group = track.create_group(group::Info { sequence }).unwrap();
			group
				.write_frame(crate::Timestamp::from_millis(sequence).unwrap(), b"x".as_slice())
				.unwrap();
			group.finish().unwrap();
		}

		let waiter = kio::Waiter::noop();
		assert!(serve.poll(&waiter).is_pending());
		assert_eq!(log.groups(), [(0, crate::priority::group(3, 0))]);
		assert_eq!(
			log.joins(),
			[(0, 0), (1, 0)],
			"each stream joins the subscription's group"
		);
		assert_eq!(log.orders(), [(0, 1), (1, 3)], "newer groups first");
		assert!(log.priorities().is_empty(), "the session queue ranks no stream");

		serve.set_priority(9).unwrap();
		assert!(serve.poll(&waiter).is_pending());
		assert_eq!(
			log.groups(),
			[(0, crate::priority::group(3, 0)), (0, crate::priority::group(9, 0))],
			"one move for the subscription"
		);
		assert_eq!(log.orders().len(), 2, "the streams keep their place in the group");
	}

	/// An OBJECT_DATAGRAM goes in its subscription's send group, ranked like the stream
	/// carrying its group from the start, so the transport merges it into the group order.
	#[test]
	fn datagrams_rank_within_their_subscription() {
		let log = crate::lite::test_transport::Log::default();
		let session = SinkSession::new(log.clone())
			.with_groups()
			.with_datagrams(1200)
			.with_unacked_fin();
		let mut track = track::Producer::new(
			std::sync::Arc::new(crate::broadcast::Info::default()),
			"test",
			crate::track::Info::timed(),
		);
		let subscriber = track.subscribe(
			track::Subscription::default()
				.with_priority(3)
				.with_max_delay(std::time::Duration::from_secs(3600)),
		);
		let mut serve = TrackServe::new(
			crate::driver::Transport::new(session.clone()),
			subscriber,
			RequestId(0),
			Version::Draft18,
			ServeRange::default(),
			None,
		);
		let mut group = track.create_group(group::Info { sequence: 0 }).unwrap();
		group
			.write_frame(crate::Timestamp::from_millis(0).unwrap(), b"x".as_slice())
			.unwrap();
		group.finish().unwrap();
		let sequence = track
			.append_datagram(crate::Timestamp::from_millis(1).unwrap(), b"d".as_slice())
			.unwrap();

		let waiter = kio::Waiter::noop();
		assert!(serve.poll(&waiter).is_pending());
		let order = crate::priority::in_group(sequence, 0, Order::Descending);
		assert_eq!(log.datagram_ranks(), [(Some(0), order)]);
		assert_eq!(log.orders(), [(0, 1)], "the stream for group 0 ranks below");
	}

	/// A group with more objects ready than one poll's budget drains over several polls.
	#[moq_clock::test]
	async fn a_long_group_drains_within_the_budget() {
		/// `GroupServe`'s passes per poll; each writes at most one object.
		const BUDGET: usize = 32;
		const FRAMES: usize = 4_000;
		const PAYLOAD: usize = 100;

		let log = crate::lite::test_transport::Log::default();
		let session = SinkSession::new(log.clone()).with_unacked_fin();
		let track = track::Producer::new(
			std::sync::Arc::new(crate::broadcast::Info::default()),
			"test",
			crate::track::Info::timed(),
		);
		let subscriber =
			track.subscribe(track::Subscription::default().with_max_delay(std::time::Duration::from_secs(3600)));
		let mut group = track.create_group(group::Info { sequence: 0 }).unwrap();
		for millis in 0..FRAMES as u64 {
			group
				.write_frame(crate::Timestamp::from_millis(millis).unwrap(), vec![0u8; PAYLOAD])
				.unwrap();
		}
		group.finish().unwrap();
		let serve = TrackServe::new(
			crate::driver::Transport::new(session.clone()),
			subscriber,
			RequestId(0),
			Version::Draft18,
			ServeRange::default(),
			None,
		);

		// An object's header takes well under a payload more, and a poll may also flush what
		// the last one buffered.
		serve_beside_a_sibling(
			serve,
			|| true,
			|| log.writes.lock().unwrap().len(),
			2 * (BUDGET + 1) * 2 * PAYLOAD,
			FRAMES * PAYLOAD,
		)
		.await;
	}

	/// Serve one group of a track with `info` and decode the subgroup header it opens with.
	async fn serve_group_header(info: track::Info) -> ietf::GroupHeader {
		let log = crate::lite::test_transport::Log::default();
		let session = SinkSession::new(log.clone());

		let track = track::Producer::new(std::sync::Arc::new(crate::broadcast::Info::default()), "test", info);
		let subscriber = track.subscribe(None);

		let mut group = track.append_group().unwrap();
		group.write_frame(crate::Timestamp::ZERO, b"hello".as_slice()).unwrap();
		group.finish().unwrap();
		track.finish().unwrap();

		let mut serve = TrackServe::new(
			crate::driver::Transport::new(session.clone()),
			subscriber,
			RequestId(0),
			Version::Draft18,
			ServeRange::default(),
			Some(Timescale::MILLI),
		);
		kio::wait(|waiter| serve.poll(waiter)).await.unwrap();

		let written = log.writes.lock().unwrap().clone();
		let mut buf = bytes::Bytes::from(written);
		crate::coding::decode_buf(&mut buf, Version::Draft18, |r, v| ietf::GroupHeader::decode(r, v))
			.expect("a group header")
	}

	/// `hang::catalog::PRIORITY` isn't reachable from `moq-net` (hang depends on it, not the
	/// other way round), so the two ranks it publishes audio and video at are spelled here.
	fn hang_audio_priority() -> i8 {
		80
	}

	fn hang_video_priority() -> i8 {
		60
	}

	/// Poll `serve` a few times, yielding between them, so its per-poll budget lets every
	/// group that is ready move on.
	async fn settle_serve(serve: &mut TrackServe) {
		for _ in 0..8 {
			let _ = serve.poll(&kio::Waiter::noop());
			moq_clock::sim::yield_now().await;
		}
	}

	/// A peer withholding stream credit makes each subscription hold one group machine,
	/// however many tracks it subscribes to and groups they publish: the backlog waits in the
	/// cache. Granting credit then serves every group still worth sending.
	#[moq_clock::test]
	async fn withheld_credit_holds_one_group_machine_per_subscription() {
		for (tracks, groups) in [(1, 1), (1, 64), (16, 1), (16, 64)] {
			let gate = kio::Producer::new(false);
			let session = SinkSession::gated_open_uni(gate.consume());
			let mut serves: Vec<_> = (0..tracks)
				.map(|id| {
					let track = track::Producer::new(
						std::sync::Arc::new(crate::broadcast::Info::default()),
						"test",
						crate::track::Info::timed(),
					);
					// Nothing goes stale, so only the credit bounds what is held.
					let subscriber =
						track.subscribe(track::Subscription::default().with_max_delay(Duration::from_secs(3600)));
					let serve = TrackServe::new(
						crate::driver::Transport::new(session.clone()),
						subscriber,
						RequestId(id),
						Version::Draft22,
						ServeRange::default(),
						Some(Timescale::MILLI),
					);
					(track, serve)
				})
				.collect();

			for sequence in 0..groups {
				for (track, serve) in &mut serves {
					let mut group = track.append_group().unwrap();
					group
						.write_frame(crate::Timestamp::from_millis(sequence).unwrap(), b"x".as_slice())
						.unwrap();
					group.finish().unwrap();
					settle_serve(serve).await;
				}
				let held = serves.iter().map(|(_, serve)| serve.machines() as u64).sum::<u64>();
				assert_eq!(held, tracks, "{tracks} tracks after {} groups each", sequence + 1);
			}

			*gate.write().ok().unwrap() = true;
			for (_, serve) in &mut serves {
				settle_serve(serve).await;
				assert_eq!(serve.opened(), groups);
				assert_eq!(serve.machines(), 0, "every group was served");
			}
		}
	}

	/// A group waiting for stream credit that the cache evicts leaves at once, rather than
	/// spending the credit on a reset once it arrives. The next group takes its place.
	#[moq_clock::test]
	async fn an_evicted_group_waiting_for_credit_leaves_at_once() {
		let gate = kio::Producer::new(false);
		let track = track::Producer::new(
			std::sync::Arc::new(crate::broadcast::Info::default()),
			"test",
			crate::track::Info::timed(),
		);
		let subscriber = track.subscribe(track::Subscription::default().with_max_delay(Duration::from_secs(3600)));
		let mut serve = TrackServe::new(
			crate::driver::Transport::new(SinkSession::gated_open_uni(gate.consume())),
			subscriber,
			RequestId(0),
			Version::Draft22,
			ServeRange::default(),
			Some(Timescale::MILLI),
		);

		let mut group = track.append_group().unwrap();
		group.write_frame(crate::Timestamp::ZERO, b"x".as_slice()).unwrap();
		settle_serve(&mut serve).await;
		assert_eq!(serve.machines(), 1, "group 0 waits for credit");

		group.abort(Error::Evicted).unwrap();
		settle_serve(&mut serve).await;
		assert_eq!(serve.machines(), 0, "the evicted group leaves without credit");

		let mut next = track.append_group().unwrap();
		next.write_frame(crate::Timestamp::from_millis(1).unwrap(), b"x".as_slice())
			.unwrap();
		next.finish().unwrap();
		settle_serve(&mut serve).await;
		assert_eq!(serve.machines(), 1, "group 1 takes its place");

		*gate.write().ok().unwrap() = true;
		settle_serve(&mut serve).await;
		assert_eq!(serve.opened(), 1, "the credit goes to group 1 alone");
	}

	/// A subgroup waiting for stream credit keeps its subscription expiry armed.
	#[moq_clock::test]
	async fn group_waiting_for_stream_credit_expires() {
		let gate = kio::Producer::new(false);
		let session = SinkSession::gated_open_uni(gate.consume());
		let track = track::Producer::new(
			std::sync::Arc::new(crate::broadcast::Info::default()),
			"test",
			crate::track::Info::timed(),
		);
		let mut subscriber = track.subscribe(None);
		let mut old = track.append_group().unwrap();
		old.write_frame(crate::Timestamp::ZERO, b"old".as_slice()).unwrap();
		old.finish().unwrap();
		let group = subscriber.recv_group().await.unwrap().expect("old group");

		let mut serve = GroupServe::new(
			crate::driver::Transport::new(session.clone()),
			ietf::GroupHeader {
				track_alias: 0,
				group_id: 0,
				sub_group_id: 0,
				publisher_priority: 0,
				flags: Default::default(),
			},
			Rank::solo(kio::Producer::new(0).consume()),
			group,
			Some(Timescale::MILLI),
			Version::Draft18,
			GroupSlice::default(),
		);
		let mut serving = std::pin::pin!(kio::wait(|waiter| serve.poll_serve(waiter)));
		assert!(
			futures::poll!(serving.as_mut()).is_pending(),
			"stream credit is exhausted"
		);

		moq_clock::sim::advance(Duration::from_secs(1)).await;
		let mut edge = track.append_group().unwrap();
		edge.write_frame(crate::Timestamp::from_millis(1000).unwrap(), b"edge".as_slice())
			.unwrap();
		edge.finish().unwrap();

		assert!(matches!(serving.await, Err(Error::Old)));
	}

	/// A local group that never held the requested prefix fails rather than serving its
	/// tail as the whole group, with or without an end inside it.
	#[moq_clock::test]
	async fn a_group_missing_its_prefix_fails() {
		for until in [None, Some(5)] {
			let track = track::Producer::new(
				std::sync::Arc::new(crate::broadcast::Info::default()),
				"test",
				crate::track::Info::timed(),
			);
			let mut subscriber = track.subscribe(None);
			let mut producer = track.create_group(group::Info { sequence: 0 }).unwrap();
			producer.start_at(1).unwrap();
			producer
				.write_frame(crate::Timestamp::ZERO, b"tail".as_slice())
				.unwrap();
			producer.finish().unwrap();
			let group = subscriber.recv_group().await.unwrap().expect("the group");

			let mut serve = GroupServe::new(
				crate::driver::Transport::new(SinkSession::new(Default::default())),
				ietf::GroupHeader {
					track_alias: 0,
					group_id: 0,
					sub_group_id: 0,
					publisher_priority: 0,
					flags: Default::default(),
				},
				Rank::solo(kio::Producer::new(0).consume()),
				group,
				Some(Timescale::MILLI),
				Version::Draft18,
				GroupSlice { skip: 0, until },
			);
			let served = kio::wait(|waiter| serve.poll_serve(waiter)).await;
			assert!(matches!(served, Err(Error::Lagged)), "until {until:?}: {served:?}");
		}
	}

	/// A FIN holds the subgroup's bytes until the peer acknowledges it, so a subgroup
	/// that goes stale while waiting still expires instead of pinning them.
	#[moq_clock::test]
	async fn unacknowledged_fin_expires_with_the_group() {
		let session = SinkSession::new(Default::default()).with_unacked_fin();
		let log = session.log.clone();
		let track = track::Producer::new(
			std::sync::Arc::new(crate::broadcast::Info::default()),
			"test",
			crate::track::Info::timed(),
		);
		let mut subscriber = track.subscribe(None);
		let mut old = track.append_group().unwrap();
		old.write_frame(crate::Timestamp::ZERO, b"old".as_slice()).unwrap();
		old.finish().unwrap();
		let group = subscriber.recv_group().await.unwrap().expect("old group");

		let mut serve = GroupServe::new(
			crate::driver::Transport::new(session.clone()),
			ietf::GroupHeader {
				track_alias: 0,
				group_id: 0,
				sub_group_id: 0,
				publisher_priority: 0,
				flags: Default::default(),
			},
			Rank::solo(kio::Producer::new(0).consume()),
			group,
			Some(Timescale::MILLI),
			Version::Draft18,
			GroupSlice::default(),
		);
		let mut serving = std::pin::pin!(kio::wait(|waiter| serve.poll_serve(waiter)));
		assert!(
			futures::poll!(serving.as_mut()).is_pending(),
			"the FIN is unacknowledged"
		);
		assert!(log.resets().is_empty());

		moq_clock::sim::advance(Duration::from_secs(1)).await;
		let mut edge = track.append_group().unwrap();
		edge.write_frame(crate::Timestamp::from_millis(1000).unwrap(), b"edge".as_slice())
			.unwrap();
		edge.finish().unwrap();

		assert!(matches!(futures::poll!(serving.as_mut()), Poll::Ready(Err(Error::Old))));
		assert_eq!(log.resets(), vec![crate::StreamError::Cancel.to_code()]);
	}

	/// A stream parked on its FIN acknowledgement still follows each priority update.
	#[moq_clock::test]
	async fn unacknowledged_fin_follows_priority_updates() {
		let session = SinkSession::new(Default::default()).with_unacked_fin();
		let log = session.log.clone();
		let track = track::Producer::new(
			std::sync::Arc::new(crate::broadcast::Info::default()),
			"test",
			crate::track::Info::timed(),
		);
		let mut group = track.create_group(group::Info { sequence: 0 }).unwrap();
		group.write_frame(crate::Timestamp::ZERO, b"done".as_slice()).unwrap();
		let consumer = group.consume();
		group.finish().unwrap();

		// Another subscription's group at 22 sits in the same queue, so each update
		// moves this stream across it.
		let queue = PriorityQueue::default();
		let _other = queue.insert(Priority::new(22, 1, 0));
		let priority = kio::Producer::new(72);
		let mut serve = GroupServe::new(
			crate::driver::Transport::new(session.clone()),
			ietf::GroupHeader {
				track_alias: 0,
				group_id: 0,
				sub_group_id: 0,
				publisher_priority: 0,
				flags: Default::default(),
			},
			rank_in(&queue, &priority, 0),
			consumer,
			Some(Timescale::MILLI),
			Version::Draft18,
			GroupSlice::default(),
		);
		let mut serving = std::pin::pin!(kio::wait(|waiter| serve.poll_serve(waiter)));
		assert!(
			futures::poll!(serving.as_mut()).is_pending(),
			"the FIN is unacknowledged"
		);
		assert_eq!(log.priorities(), vec![255]);

		*priority.write().ok().unwrap() = -28;
		assert!(futures::poll!(serving.as_mut()).is_pending());
		assert_eq!(log.priorities(), vec![255, 254]);

		*priority.write().ok().unwrap() = 47;
		assert!(futures::poll!(serving.as_mut()).is_pending());
		assert_eq!(log.priorities(), vec![255, 254, 255]);
	}

	/// The final payload remains guarded after its frame has advanced the group cursor.
	#[moq_clock::test]
	async fn blocked_final_transport_chunk_expires_with_the_group() {
		let gate = kio::Producer::new(true);
		let session = SinkSession::gated_uni(gate.consume());
		let track = track::Producer::new(
			std::sync::Arc::new(crate::broadcast::Info::default()),
			"test",
			crate::track::Info::timed(),
		);
		let mut subscriber = track.subscribe(None);
		let old = track.append_group().unwrap();
		let mut frame = old
			.create_frame(frame::Info {
				timestamp: Some(crate::Timestamp::ZERO),
				size: 2,
			})
			.unwrap();
		frame.write(b"a".as_slice()).unwrap();
		let group = subscriber.recv_group().await.unwrap().expect("old group");
		let mut serve = GroupServe::new(
			crate::driver::Transport::new(session.clone()),
			ietf::GroupHeader {
				track_alias: 0,
				group_id: 0,
				sub_group_id: 0,
				publisher_priority: 0,
				flags: Default::default(),
			},
			Rank::solo(kio::Producer::new(0).consume()),
			group,
			Some(Timescale::MILLI),
			Version::Draft18,
			GroupSlice::default(),
		);
		let mut serving = std::pin::pin!(kio::wait(|waiter| serve.poll_serve(waiter)));
		// Let it run until it blocks on the rest of the frame.
		assert!(futures::poll!(serving.as_mut()).is_pending());

		let Ok(mut open) = gate.write() else {
			panic!("transport gate closed");
		};
		*open = false;
		drop(open);
		frame.write(b"b".as_slice()).unwrap();
		frame.finish().unwrap().finish().unwrap();
		assert!(
			futures::poll!(serving.as_mut()).is_pending(),
			"the final byte is transport-blocked"
		);

		moq_clock::sim::advance(Duration::from_secs(1)).await;
		let mut edge = track.append_group().unwrap();
		edge.write_frame(crate::Timestamp::from_millis(1000).unwrap(), b"edge".as_slice())
			.unwrap();
		edge.finish().unwrap();

		assert!(matches!(serving.await, Err(Error::Old)));
	}
}

#[cfg(test)]
mod subscribe_cursor_test {
	use super::*;
	use crate::lite::test_transport::{Log, SinkSession};

	/// A group retention gives up on while a FETCH reads it is refused as too old, rather
	/// than answered with the prefix read so far.
	#[moq_clock::test]
	async fn a_fetch_past_retention_is_refused_not_truncated() {
		let track = track::Producer::new(
			std::sync::Arc::new(crate::broadcast::Info::default()),
			"video",
			crate::track::Info::timed().with_max_age(std::time::Duration::from_millis(500)),
		);
		let mut open = track.append_group().unwrap();
		open.write_frame(crate::Timestamp::ZERO, b"a".to_vec()).unwrap();

		let consumer = track.consume();
		let read = moq_clock::sim::spawn(async move { read_fetch(&consumer, 0, 0, None, 0, false).await.map(|_| ()) });
		moq_clock::sim::yield_now().await;
		// The track runs a full second past the group's successor.
		for ms in [1000, 2000] {
			let mut group = track.append_group().unwrap();
			group
				.write_frame(crate::Timestamp::from_millis(ms).unwrap(), b"x".to_vec())
				.unwrap();
			group.finish().unwrap();
		}
		assert!(matches!(read.await.unwrap(), Err(Error::Old)));
	}

	/// A subscription's cursor starts at the oldest cached group, so serving it verbatim
	/// replays every retained group at once, each on its own stream. Relays reject the burst
	/// and players skip straight back to the live edge, so the catch-up is pure waste.
	#[moq_clock::test]
	async fn a_subscribe_is_served_from_the_live_edge() {
		let log = Log::default();
		let session = SinkSession::new(log.clone());

		let track = track::Producer::new(
			std::sync::Arc::new(crate::broadcast::Info::default()),
			"video",
			crate::track::Info::timed(),
		);
		for sequence in 0..4 {
			let mut group = track.create_group(group::Info { sequence }).unwrap();
			group
				.write_frame(crate::Timestamp::from_millis(0).unwrap(), b"frame".as_slice())
				.unwrap();
			group.finish().unwrap();
		}

		let subscriber = track.subscribe(None);
		track.finish().unwrap();

		let mut serve = TrackServe::new(
			crate::driver::Transport::new(session.clone()),
			subscriber,
			RequestId(1),
			Version::Draft18,
			ServeRange::default(),
			Some(Timescale::MILLI),
		);
		kio::wait(|waiter| serve.poll(waiter)).await.unwrap();

		// `GroupServe` sets the priority once per stream it opens, so this counts groups served.
		assert_eq!(log.priorities().len(), 1, "only group 3 should have been served");
	}
}

#[cfg(test)]
mod serve_tests {
	use super::*;
	use crate::coding::{Decode, Decoder};
	use crate::ietf::GroupOrder;
	use crate::lite::test_transport::{Log, ScriptedSession, Sent, SinkSession};
	use crate::model::ProduceTest;

	#[moq_clock::test]
	async fn idle_advertisements_keep_reading_goaways() {
		let version = Version::Draft22;
		let session = ScriptedSession::new(vec![0x10, 0, 2, 0, 0]);
		let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), version)
			.await
			.unwrap();
		let suffix = crate::Path::from_static("room");
		let mut requests = HashMap::new();
		requests.insert(
			suffix.clone(),
			NamespaceRequest {
				path: suffix.clone(),
				stream,
				messages: super::super::request_stream::Messages::new(
					crate::driver::Transport::new(session.clone()),
					true,
				),
				auth: None,
				queued: false,
				pending: None,
				waiter: None,
			},
		);
		let woken = Arc::new(Woken::default());
		woken.suffixes.lock().unwrap().push(suffix.clone());
		let mut inflight = VecDeque::new();
		let runtime = crate::time::sim();
		let waiter = &kio::Waiter::noop();
		assert!(Publisher::poll_answer(&mut requests, &mut inflight, &woken, runtime.now(), waiter).is_pending());
		assert!(session.log.closes().is_empty());
		session.push(&[0x10, 0, 2, 0, 0]);
		woken.suffixes.lock().unwrap().push(suffix);
		assert!(matches!(
			Publisher::poll_answer(&mut requests, &mut inflight, &woken, runtime.now(), waiter),
			Poll::Ready((_, Err(Error::ProtocolViolation)))
		));
		assert_eq!(session.log.closes()[0].0, 0x03);
	}

	#[moq_clock::test]
	async fn an_update_after_draft18_goaway_is_refused_without_closing_the_session() {
		let version = Version::Draft18;
		let h = serve(version);
		h.publisher.control.accept(RequestId(0)).unwrap();
		assert_eq!(h.publisher.control.goaway().unwrap(), RequestId(2));
		// REQUEST_UPDATE, Request ID 2, no parameters.
		let session = ScriptedSession::new(vec![0x02, 0, 2, 2, 0]);
		let mut stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), version)
			.await
			.unwrap();
		let mut goaways = super::super::request_stream::Messages::new(
			crate::driver::Transport::new(h.session.clone()),
			h.publisher.control.client(),
		);
		let mut serving = TrackServe::new(
			crate::driver::Transport::new(h.session.clone()),
			h.track.subscribe(None),
			RequestId(0),
			version,
			ServeRange::default(),
			None,
		);
		let served = h
			.publisher
			.run_subscription(&mut stream, &mut serving, &mut false, &mut goaways, async {}, None)
			.await;
		assert!(matches!(served, Some(Served::UpdateFailed(_))), "{served:?}");
		assert!(h.log.closes().is_empty());
		let written = session.log.writes.lock().unwrap().clone();
		let mut bytes = bytes::Bytes::from(written);
		let super::super::request_stream::Framed(id, body) = crate::coding::decode_buf(&mut bytes, version, |r, v| {
			super::super::request_stream::Framed::decode(r, v)
		})
		.unwrap();
		assert_eq!(id, ietf::RequestError::ID);
		assert_eq!(body.message::<ietf::RequestError>(version).unwrap().error_code, 0x06);
	}

	#[moq_clock::test]
	async fn a_subscription_keeps_its_goaway_count_until_the_final_marker() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			let h = serve(version);
			let session = ScriptedSession::new(vec![0x10, 0, 2, 0, 0]);
			let mut stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), version)
				.await
				.unwrap();
			let mut serving = TrackServe::new(
				crate::driver::Transport::new(h.session.clone()),
				h.track.subscribe(None),
				RequestId(0),
				version,
				ServeRange::default(),
				None,
			);
			let mut finished = false;
			let mut goaways =
				super::super::request_stream::Messages::new(crate::driver::Transport::new(h.session.clone()), false);
			{
				let run = h.publisher.run_subscription(
					&mut stream,
					&mut serving,
					&mut finished,
					&mut goaways,
					async {},
					None,
				);
				let mut run = std::pin::pin!(run);
				assert!(futures::poll!(run.as_mut()).is_pending());
				h.track.finish().unwrap();
				assert!(matches!(run.await, Some(Served::Track(Ok(())))), "{version}");
			}
			// The same reader now watches cancellation while the final marker waits for credit.
			session.push(&[0x10, 0, 2, 0, 0]);
			let waiter = &kio::Waiter::noop();
			assert!(
				matches!(
					super::super::request_stream::poll_cancel(
						&mut stream,
						&mut finished,
						&mut goaways,
						version,
						waiter
					),
					Poll::Ready(Err(Error::ProtocolViolation))
				),
				"{version}"
			);
			assert_eq!(h.log.closes()[0].0, 0x03, "{version}");
		}
	}

	fn occurrences(log: &Log, needle: &[u8]) -> usize {
		let writes = log.writes.lock().unwrap();
		writes.windows(needle.len()).filter(|window| *window == needle).count()
	}

	fn timestamp() -> crate::Timestamp {
		crate::Timestamp::from_millis(0).unwrap()
	}

	/// A publisher whose origin serves one broadcast ("room") with one track ("video").
	struct Serve {
		publisher: Publisher,
		session: ScriptedSession,
		log: Log,
		track: track::Producer,
		_origin: origin::Producer,
		_broadcast: crate::origin::Publication,
	}

	fn serve(version: Version) -> Serve {
		serve_track(version, crate::track::Info::timed())
	}

	/// The same, with the track described by `info`.
	fn serve_track(version: Version, info: crate::track::Info) -> Serve {
		serve_route(version, info, crate::origin::Route::default())
	}

	/// The same, with the broadcast announced on `route`.
	fn serve_route(version: Version, info: crate::track::Info, route: crate::origin::Route) -> Serve {
		serve_session(version, info, route, ScriptedSession::per_stream(vec![Vec::new()]))
	}

	/// The same, over `session`.
	fn serve_session(
		version: Version,
		info: crate::track::Info,
		route: crate::origin::Route,
		session: ScriptedSession,
	) -> Serve {
		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let broadcast = origin.publish("room", route).unwrap();
		let track = broadcast.create_track("video", info).unwrap();

		let log = session.log.clone();

		let peer_setup = peer::PeerSetup::default();
		peer_setup.set(peer::Peer::default());

		let publisher = Publisher::new(
			crate::time::sim(),
			crate::driver::Transport::new(session.clone()),
			origin.consume(),
			Control::new(false),
			None,
			peer_setup,
			version,
		);

		Serve {
			publisher,
			session,
			log,
			track,
			_origin: origin,
			_broadcast: broadcast,
		}
	}

	#[moq_clock::test]
	async fn requester_fin_is_version_gated_for_subscriptions() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			let h = serve(version);
			let session = ScriptedSession::per_stream_eof(vec![vec![]]);
			let mut stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), version)
				.await
				.unwrap();
			let mut goaways = super::super::request_stream::Messages::new(
				crate::driver::Transport::new(h.session.clone()),
				h.publisher.control.client(),
			);
			let mut serving = TrackServe::new(
				crate::driver::Transport::new(h.session.clone()),
				h.track.subscribe(None),
				RequestId(0),
				version,
				ServeRange::default(),
				None,
			);
			let mut finished = false;
			let mut run = std::pin::pin!(h.publisher.run_subscription(
				&mut stream,
				&mut serving,
				&mut finished,
				&mut goaways,
				async {},
				None,
			));
			assert_eq!(
				futures::poll!(run.as_mut()).is_ready(),
				super::super::request_stream::fin_cancels(version),
				"{version}"
			);
			if !super::super::request_stream::fin_cancels(version) {
				let mut group = h.track.create_group(group::Info { sequence: 0 }).unwrap();
				group.write_frame(timestamp(), b"after FIN".as_slice()).unwrap();
				group.finish().unwrap();
				assert!(futures::poll!(run.as_mut()).is_pending());
				assert_eq!(occurrences(&h.log, b"after FIN"), 1, "{version}");
			}
		}
	}

	/// A reset request stream still cancels the subscription on every draft.
	#[moq_clock::test]
	async fn requester_reset_cancels_subscriptions() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			let h = serve(version);
			let session = ScriptedSession::per_stream_reset(vec![vec![]]);
			let mut stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), version)
				.await
				.unwrap();
			let mut goaways = super::super::request_stream::Messages::new(
				crate::driver::Transport::new(h.session.clone()),
				h.publisher.control.client(),
			);
			let mut serving = TrackServe::new(
				crate::driver::Transport::new(h.session.clone()),
				h.track.subscribe(None),
				RequestId(0),
				version,
				ServeRange::default(),
				None,
			);
			let mut finished = false;
			let run =
				h.publisher
					.run_subscription(&mut stream, &mut serving, &mut finished, &mut goaways, async {}, None);
			assert!(futures::poll!(std::pin::pin!(run)).is_ready(), "{version}");
		}
	}

	/// A close must count requests before their first poll and release them even
	/// when a dispatched task is cancelled or its message is refused.
	#[moq_clock::test]
	async fn drain_counts_dispatched_subscribe_and_fetch() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			let h = serve(version);
			let mut subscribe_body = Vec::new();
			subscribe(Filter::NextObject, None)
				.encode_msg(&mut Encoder::new(&mut subscribe_body, version.into()), version)
				.unwrap();
			let mut fetch_body = Vec::new();
			ietf::Fetch {
				request_id: FETCH_ID,
				subscriber_priority: 128,
				group_order: GroupOrder::Ascending,
				// Draft-20 dropped the Fetch Type tag, leaving only the filtered form.
				fetch_type: match version {
					Version::Draft20 | Version::Draft22 => FetchType::Filtered {
						namespace: crate::Path::from_static("missing"),
						track: "video".into(),
						filter: Filter::Unfiltered,
					},
					_ => FetchType::Standalone {
						namespace: crate::Path::from_static("missing"),
						track: "video".into(),
						start: Location { group: 0, object: 0 },
						end: Location { group: 0, object: 1 },
					},
				},
				range_filters: false,
				fill_timeout: false,
				properties_wanted: true,
				epoch: None,
			}
			.encode_msg(&mut Encoder::new(&mut fetch_body, version.into()), version)
			.unwrap();

			for (id, body) in [
				(ietf::Subscribe::ID, bytes::Bytes::from(subscribe_body)),
				(ietf::Fetch::ID, bytes::Bytes::from(fetch_body)),
			] {
				let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), version)
					.await
					.unwrap();
				let task = h
					.publisher
					.handle_stream(id, ietf::Body(body.clone()), stream, false)
					.unwrap();
				assert_eq!(h.publisher.owed.load(Ordering::Relaxed), 1, "count before polling");
				drop(task);
				assert_eq!(h.publisher.owed.load(Ordering::Relaxed), 0, "release a cancelled task");

				let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), version)
					.await
					.unwrap();
				assert!(
					h.publisher
						.handle_stream(id, ietf::Body(bytes::Bytes::new()), stream, false)
						.is_err()
				);
				assert_eq!(h.publisher.owed.load(Ordering::Relaxed), 0, "release malformed input");

				if id == ietf::Fetch::ID {
					let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), version)
						.await
						.unwrap();
					h.publisher
						.handle_stream(id, ietf::Body(body), stream, false)
						.unwrap()
						.await;
					assert_eq!(h.publisher.owed.load(Ordering::Relaxed), 0, "release a completed fetch");
				}
			}
		}
	}

	#[moq_clock::test]
	async fn subscription_update_applies_priority_without_ending_the_request() {
		let version = Version::Draft18;
		let h = serve(version);
		let session = ScriptedSession::per_stream(vec![vec![0x02, 0, 4, 2, 1, 0x20, 10, 0x02, 0, 2, 4, 0]]);
		let mut stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), version)
			.await
			.unwrap();
		let mut goaways = super::super::request_stream::Messages::new(
			crate::driver::Transport::new(h.session.clone()),
			h.publisher.control.client(),
		);
		let mut serving = TrackServe::new(
			crate::driver::Transport::new(h.session.clone()),
			h.track.subscribe(None),
			RequestId(0),
			version,
			ServeRange::default(),
			None,
		);
		let mut finished = false;
		let mut run = std::pin::pin!(h.publisher.run_subscription(
			&mut stream,
			&mut serving,
			&mut finished,
			&mut goaways,
			async {},
			None,
		));
		assert!(futures::poll!(run.as_mut()).is_pending());
		assert_eq!(h.track.subscription().unwrap().priority, 117);
		assert_eq!(occurrences(&session.log, &[0x07, 0, 1, 0]), 2);
	}

	/// A priority update moves the group streams already in flight too, not only the
	/// ones opened after it.
	#[moq_clock::test]
	async fn subscription_update_reprioritizes_open_group_streams() {
		let version = Version::Draft18;
		let h = serve(version);
		let session = ScriptedSession::new(Vec::new());
		let mut stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), version)
			.await
			.unwrap();
		// Another subscription's group at 100 shares the queue, so the update moves this
		// subscription's stream past it.
		let queue = PriorityQueue::default();
		let _other = queue.insert(Priority::new(100, 1, 0));
		let mut goaways = super::super::request_stream::Messages::new(
			crate::driver::Transport::new(h.session.clone()),
			h.publisher.control.client(),
		);
		let mut serving = TrackServe::new(
			crate::driver::Transport::new(h.session.clone()),
			h.track.subscribe(None),
			RequestId(0),
			version,
			ServeRange::default(),
			None,
		)
		.ranked(queue);
		let mut finished = false;
		let mut run = std::pin::pin!(h.publisher.run_subscription(
			&mut stream,
			&mut serving,
			&mut finished,
			&mut goaways,
			async {},
			None,
		));

		// An unfinished group keeps its stream open.
		let mut group = h.track.create_group(group::Info { sequence: 0 }).unwrap();
		group.write_frame(timestamp(), b"open".as_slice()).unwrap();
		assert!(futures::poll!(run.as_mut()).is_pending());
		let before = h.log.priorities();
		assert_eq!(before, [254], "below the other subscription");

		// REQUEST_UPDATE with SUBSCRIBER_PRIORITY 10.
		session.push(&[0x02, 0, 4, 2, 1, 0x20, 10]);
		for _ in 0..4 {
			assert!(futures::poll!(run.as_mut()).is_pending());
		}
		assert_eq!(h.track.subscription().unwrap().priority, 117);
		let after = h.log.priorities();
		assert_eq!(after.last(), Some(&255), "above the other subscription: {after:?}");
	}

	/// The request stream's control messages and every data stream's first FIN or reset,
	/// as positions in the cross-stream [`Log::trail`].
	struct Wire {
		/// Each control message's type and the positions of its first and last bytes.
		messages: Vec<(u64, std::ops::RangeInclusive<usize>)>,
		/// PUBLISH_DONE's Status Code and Stream Count.
		publish_done: Option<(u64, u64)>,
		/// Each data stream and the position where it was closed, if it was.
		data: std::collections::BTreeMap<usize, Option<usize>>,
	}

	impl Wire {
		fn read(log: &Log, version: Version) -> Self {
			let trail = log.trail();
			let request = request_stream(&trail);
			let mut bytes = Vec::new();
			let mut written_at = Vec::new();
			let mut data = std::collections::BTreeMap::new();
			for (position, (stream, sent)) in trail.iter().enumerate() {
				if *stream == request {
					if let Sent::Write(buf) = sent {
						bytes.extend_from_slice(buf);
						written_at.extend(std::iter::repeat_n(position, buf.len()));
					}
					continue;
				}
				let closed = data.entry(*stream).or_insert(None);
				if matches!(sent, Sent::Finish | Sent::Reset(_)) && closed.is_none() {
					*closed = Some(position);
				}
			}

			let mut messages = Vec::new();
			let mut publish_done = None;
			let mut offset = 0;
			while offset < bytes.len() {
				let mut rest = &bytes[offset..];
				let id = crate::coding::decode_varint(&mut rest, version).unwrap();
				let body = bytes.len() - rest.len();
				let end = body + 2 + usize::from(u16::from_be_bytes([rest[0], rest[1]]));
				if id == ietf::PublishDone::ID {
					let done = ietf::PublishDone::decode(&mut Decoder::new(&bytes[body..end], version.into()), version)
						.unwrap();
					publish_done = Some((done.status_code, done.stream_count));
				}
				messages.push((id, written_at[offset]..=written_at[end - 1]));
				offset = end;
			}
			Self {
				messages,
				publish_done,
				data,
			}
		}

		fn sent(&self, id: u64) -> Option<std::ops::RangeInclusive<usize>> {
			self.messages
				.iter()
				.find(|(sent, _)| *sent == id)
				.map(|(_, at)| at.clone())
		}

		/// Draft-22 §3.1.1: the publisher "MUST NOT send [PUBLISH_DONE] until it has
		/// closed all related streams", so each data stream's FIN or reset precedes
		/// PUBLISH_DONE's first byte.
		fn assert_streams_closed_before_publish_done(&self) {
			let done = *self.sent(ietf::PublishDone::ID).expect("PUBLISH_DONE was sent").start();
			for (stream, closed) in &self.data {
				assert!(
					closed.is_some_and(|closed| closed < done),
					"data stream {stream} closed at {closed:?}, but PUBLISH_DONE began at {done}: {:?}",
					self.data,
				);
			}
		}
	}

	/// SUBSCRIBE_OK is written before any data stream opens, so the request stream is
	/// the first to send anything.
	fn request_stream(trail: &[(usize, Sent)]) -> usize {
		trail.first().expect("nothing was sent").0
	}

	/// What each data stream has sent so far.
	fn data_streams(log: &Log) -> std::collections::BTreeMap<usize, Vec<Sent>> {
		let trail = log.trail();
		let mut data = std::collections::BTreeMap::<usize, Vec<Sent>>::new();
		if let Some((_, rest)) = trail.split_first() {
			let request = request_stream(&trail);
			for (stream, sent) in rest.iter().filter(|(stream, _)| *stream != request) {
				data.entry(*stream).or_default().push(sent.clone());
			}
		}
		data
	}

	/// Poll the subscription until `ready` holds, sleeping in between so the runtime can
	/// resolve the subscription's demand. The subscription must still be running when
	/// `ready` is reached.
	///
	/// Simulated time only advances once every task stalls. Sleeping stalls this one, so
	/// the timeout can expire; a loop that only yields would keep the executor busy.
	async fn serve_until<F: std::future::Future<Output = Result<(), Error>>>(
		mut serve: std::pin::Pin<&mut F>,
		what: &str,
		ready: impl Fn() -> bool,
	) {
		moq_clock::timeout(std::time::Duration::from_secs(10), async {
			while !ready() {
				assert!(
					futures::poll!(serve.as_mut()).is_pending(),
					"the subscription ended before {what}"
				);
				moq_clock::sleep(std::time::Duration::from_millis(1)).await;
			}
		})
		.await
		.unwrap_or_else(|_| panic!("{what} never happened"));
	}

	/// The canonical current-group join: Next Object plus a fill one group back. On a
	/// group whose object 0 exists, this opens a fill stream for object 0 and a group
	/// stream that waits for object 1.
	fn join_current_group() -> ietf::Subscribe<'static> {
		subscribe(
			Filter::NextObject,
			Some(ietf::Fill {
				group_order: None,
				filter: Some(Filter::Relative(1)),
				range_filters: false,
			}),
		)
	}

	/// A track error ends a draft-22 subscription while its group stream waits for the
	/// next object. The abort reaches the group, which resets its stream before
	/// PUBLISH_DONE(INTERNAL_ERROR) is sent.
	#[moq_clock::test]
	async fn publish_done_after_a_track_error_follows_every_stream_close() {
		let version = Version::Draft22;
		let h = serve(version);
		let mut group = h.track.create_group(group::Info { sequence: 0 }).unwrap();
		group.write_frame(timestamp(), b"head".as_slice()).unwrap();

		let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), version)
			.await
			.unwrap();
		let mut serve = std::pin::pin!(h.publisher.clone().run_subscribe_stream(stream, join_current_group()));
		serve_until(serve.as_mut(), "the fill finished and the group stream opened", || {
			let data = data_streams(&h.log);
			data.len() == 2 && data.values().any(|sent| sent.contains(&Sent::Finish))
		})
		.await;

		h.track.abort(Error::Transport("upstream failed".into())).unwrap();
		moq_clock::timeout(std::time::Duration::from_secs(10), serve)
			.await
			.expect("the track error ends the subscription")
			.ok();

		let wire = Wire::read(&h.log, version);
		let (status, _) = wire.publish_done.expect("PUBLISH_DONE was sent");
		assert_eq!(status, ietf::PublishDoneStatus::InternalError.code());
		assert_eq!(wire.data.len(), 2, "a group stream and a fill stream");
		wire.assert_streams_closed_before_publish_done();
		drop(group);
	}

	/// A subscription that runs to the track's end counts every data stream it opened,
	/// the fill and the END_OF_TRACK marker included, and closes each one before
	/// PUBLISH_DONE(TRACK_ENDED).
	#[moq_clock::test]
	async fn publish_done_at_the_track_end_counts_and_follows_every_stream() {
		let version = Version::Draft22;
		let h = serve(version);
		let mut group = h.track.create_group(group::Info { sequence: 0 }).unwrap();
		group.write_frame(timestamp(), b"head".as_slice()).unwrap();

		let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), version)
			.await
			.unwrap();
		let mut serve = std::pin::pin!(h.publisher.clone().run_subscribe_stream(stream, join_current_group()));
		serve_until(serve.as_mut(), "the fill finished and the group stream opened", || {
			let data = data_streams(&h.log);
			data.len() == 2 && data.values().any(|sent| sent.contains(&Sent::Finish))
		})
		.await;

		group.finish().unwrap();
		h.track.finish().unwrap();
		moq_clock::timeout(std::time::Duration::from_secs(10), serve)
			.await
			.expect("the track's end ends the subscription")
			.unwrap();

		let wire = Wire::read(&h.log, version);
		let (status, count) = wire.publish_done.expect("PUBLISH_DONE was sent");
		assert_eq!(status, ietf::PublishDoneStatus::TrackEnded.code());
		assert_eq!(wire.data.len(), 3, "a fill, a group and an END_OF_TRACK stream");
		assert_eq!(count, 3);
		wire.assert_streams_closed_before_publish_done();
	}

	/// A finished group's stream whose FIN is never acknowledged holds PUBLISH_DONE only until
	/// the group goes stale: its successor's arrival plus the budget, even under a budget past
	/// the cache window, which evicts both groups first.
	#[moq_clock::test]
	async fn publish_done_waits_on_an_unacknowledged_fin_until_the_group_is_stale() {
		let version = Version::Draft22;
		let session = ScriptedSession::per_stream(vec![Vec::new()]).with_unacked_uni_fins(1);
		let h = serve_session(
			version,
			crate::track::Info::timed(),
			crate::origin::Route::default(),
			session,
		);
		// Well past the cache window, which only dates idleness once a sweep passes.
		let budget = crate::cache::DEFAULT_EXPIRY * 4;
		let mut msg = subscribe(Filter::NextObject, None);
		msg.max_age = Some(budget.as_millis() as u64);

		// `create_broadcast` registers the broadcast from a spawned task.
		moq_clock::sleep(Duration::from_millis(1)).await;
		let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), version)
			.await
			.unwrap();
		let mut serve = std::pin::pin!(h.publisher.clone().run_subscribe_stream(stream, msg));
		// The subscription then parks at the live edge.
		for _ in 0..200 {
			assert!(futures::poll!(serve.as_mut()).is_pending());
		}

		// Frozen timestamps, so only the wall clock since group 1 arrived convicts group 0.
		let append = || {
			let mut group = h.track.append_group().unwrap();
			group.write_frame(timestamp(), b"x".as_slice()).unwrap();
			group.finish().unwrap();
		};
		let finished = || {
			data_streams(&h.log)
				.values()
				.filter(|sent| sent.contains(&Sent::Finish))
				.count()
		};
		let step = Duration::from_secs(1);
		async fn wait<F: std::future::Future>(mut serve: std::pin::Pin<&mut F>, seconds: u64) {
			for _ in 0..seconds {
				moq_clock::sim::advance(Duration::from_secs(1)).await;
				assert!(
					futures::poll!(serve.as_mut()).is_pending(),
					"PUBLISH_DONE went out early"
				);
			}
		}
		append();
		serve_until(serve.as_mut(), "group 0's FIN went out", || finished() == 1).await;
		append();
		serve_until(serve.as_mut(), "group 1's stream finished", || finished() == 2).await;

		// Group 2 arrives later, so a verdict that forgot group 1 would wait for 2's deadline.
		wait(serve.as_mut(), 10).await;
		append();
		h.track.finish().unwrap();
		serve_until(serve.as_mut(), "group 2's stream finished", || finished() == 3).await;

		// Groups 0 and 1 go idle past the cache window long before the deadline.
		wait(serve.as_mut(), budget.as_secs() - 12).await;
		assert!(h.log.resets().is_empty());

		moq_clock::sim::advance(step).await;
		moq_clock::timeout(step, serve)
			.await
			.expect("the stale group releases PUBLISH_DONE")
			.unwrap();
		assert_eq!(h.log.resets(), vec![crate::StreamError::Cancel.to_code()]);
		let wire = Wire::read(&h.log, version);
		let (status, _) = wire.publish_done.expect("PUBLISH_DONE was sent");
		assert_eq!(status, ietf::PublishDoneStatus::TrackEnded.code());
		wire.assert_streams_closed_before_publish_done();
	}

	/// A track that ends with an Unsupported error (a relay refusing a copy whose metadata
	/// changed does) ends its subscription with INTERNAL_ERROR: UPDATE_FAILED is only for an
	/// update the publisher could not apply.
	#[moq_clock::test]
	async fn an_unsupported_track_error_is_not_a_failed_update() {
		let version = Version::Draft18;
		let h = serve(version);
		let session = ScriptedSession::per_stream(vec![vec![]]);
		let mut stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), version)
			.await
			.unwrap();
		let mut goaways = super::super::request_stream::Messages::new(
			crate::driver::Transport::new(h.session.clone()),
			h.publisher.control.client(),
		);
		let mut serving = TrackServe::new(
			crate::driver::Transport::new(h.session.clone()),
			h.track.subscribe(None),
			RequestId(0),
			version,
			ServeRange::default(),
			None,
		);
		h.track.clone().abort(Error::Unsupported).unwrap();
		let mut finished = false;
		let served = h
			.publisher
			.run_subscription(&mut stream, &mut serving, &mut finished, &mut goaways, async {}, None)
			.await
			.expect("the track's error ends the subscription");
		assert!(matches!(served, Served::Track(Err(Error::Unsupported))), "{served:?}");
		assert_eq!(served.status().0, ietf::PublishDoneStatus::InternalError);
		assert_eq!(
			Served::UpdateFailed(Error::Unsupported).status().0,
			ietf::PublishDoneStatus::UpdateFailed
		);
		// A revocation bridged from upstream, however it was received, is still one.
		for err in [
			Error::Unauthorized,
			Error::Session(crate::SessionError::Unauthorized),
			Error::Stream(crate::StreamError::Unauthorized),
		] {
			assert_eq!(
				Served::Track(Err(err)).status().0,
				ietf::PublishDoneStatus::Unauthorized
			);
		}
	}

	/// Draft-22 §9.5: a failed REQUEST_UPDATE is answered with REQUEST_ERROR, then the
	/// subscription ends with PUBLISH_DONE(UPDATE_FAILED).
	///
	/// The update asks for a new group on a track without DYNAMIC_GROUPS. It arrives
	/// while object 0 of the current group is half written, so the fill stream is stalled
	/// inside it and the group stream waits for object 1.
	async fn fail_update_mid_flight(version: Version) -> Wire {
		let h = serve(version);
		let group = h.track.create_group(group::Info { sequence: 0 }).unwrap();
		let mut frame = group
			.create_frame(frame::Info {
				timestamp: Some(timestamp()),
				size: 4,
			})
			.unwrap();
		frame.write(b"he".as_slice()).unwrap();

		// The request stream shares the data streams' log, and its peer speaks later.
		let mut request = ScriptedSession::new(Vec::new());
		request.log = h.log.clone();
		let stream = Stream::open(&mut crate::driver::Transport::new(request.clone()), version)
			.await
			.unwrap();
		let mut serve = std::pin::pin!(h.publisher.clone().run_subscribe_stream(stream, join_current_group()));
		serve_until(
			serve.as_mut(),
			"the fill stalled mid-object beside the group stream",
			|| {
				let data = data_streams(&h.log);
				data.len() == 2 && data.values().any(|sent| sent.contains(&Sent::Write(b"he".to_vec())))
			},
		)
		.await;

		// REQUEST_UPDATE, Request ID 2, one NEW_GROUP_REQUEST (0x32) of 0.
		request.push(&[0x02, 0, 4, 2, 1, 0x32, 0x00]);
		moq_clock::timeout(std::time::Duration::from_secs(10), serve)
			.await
			.expect("the failed update ends the subscription")
			.ok();
		drop(frame);

		let wire = Wire::read(&h.log, version);
		let error = wire
			.sent(ietf::RequestError::ID)
			.expect("REQUEST_ERROR answers the update");
		let done = wire.sent(ietf::PublishDone::ID).expect("PUBLISH_DONE was sent");
		assert!(error.end() < done.start(), "REQUEST_ERROR precedes PUBLISH_DONE");
		let (status, _) = wire.publish_done.unwrap();
		assert_eq!(status, ietf::PublishDoneStatus::UpdateFailed.code());
		assert_eq!(wire.data.len(), 2, "a group stream and a fill stream");
		wire
	}

	#[moq_clock::test]
	async fn publish_done_after_a_failed_update_follows_every_stream_close() {
		fail_update_mid_flight(Version::Draft22)
			.await
			.assert_streams_closed_before_publish_done();
	}

	/// Draft-22 §9.9: Stream Count is every data stream the publisher opened, "including
	/// any fill fetch streams", or 2^64-1 when it can't be exact.
	#[moq_clock::test]
	async fn publish_done_after_a_failed_update_counts_the_open_fill() {
		let wire = fail_update_mid_flight(Version::Draft22).await;
		let (_, count) = wire.publish_done.unwrap();
		assert!(
			count == wire.data.len() as u64 || count == u64::MAX,
			"Stream Count {count}, but {} data streams were opened",
			wire.data.len(),
		);
	}

	/// A REQUEST_UPDATE with Request ID `id` carrying only FORWARD, framed for the stream of
	/// the [`subscribe`] it updates.
	fn forward_update(version: Version, id: u64, forward: bool) -> Vec<u8> {
		let mut body = Vec::new();
		ietf::SubscribeUpdate {
			max_age: None,
			new_group: None,
			request_id: RequestId(id),
			subscriber_priority: None,
			forward: Some(forward),
			filter: None,
			authorization_token: None,
			fill: None,
		}
		.encode_msg(&mut Encoder::new(&mut body, version.into()), version)
		.unwrap();
		let mut frame = vec![ietf::SubscribeUpdate::ID as u8, 0, body.len() as u8];
		frame.extend(body);
		frame
	}

	/// FORWARD=0 drops the subscription, and its upstream demand, while the request stays
	/// established; FORWARD=1 subscribes again as a new SUBSCRIBE would, at the live edge.
	#[moq_clock::test]
	async fn a_subscribe_pauses_and_resumes_as_a_new_subscriber() {
		for version in [Version::Draft22] {
			for start_paused in [false, true] {
				let h = serve(version);
				let mut request = ScriptedSession::new(Vec::new());
				request.log = h.log.clone();
				let stream = Stream::open(&mut crate::driver::Transport::new(request.clone()), version)
					.await
					.unwrap();
				let mut msg = subscribe(Filter::NextObject, None);
				msg.forward = !start_paused;
				let mut serve = std::pin::pin!(h.publisher.clone().run_subscribe_stream(stream, msg));
				let sent = |payload: &[u8]| {
					data_streams(&h.log)
						.values()
						.flatten()
						.filter(|sent| **sent == Sent::Write(payload.to_vec()))
						.count()
				};
				let answers = || {
					if h.log.trail().is_empty() {
						return (false, 0);
					}
					let wire = Wire::read(&h.log, version);
					let ok = wire.sent(ietf::SubscribeOk::ID).is_some();
					let answers = wire
						.messages
						.iter()
						.filter(|(id, _)| *id == ietf::RequestOk::ID)
						.count();
					(ok, answers)
				};
				let mut group = h.track.create_group(group::Info { sequence: 0 }).unwrap();
				let mut paused = 0;
				if !start_paused {
					serve_until(serve.as_mut(), "SUBSCRIBE_OK", || answers().0).await;
					group.write_frame(timestamp(), b"before".as_slice()).unwrap();
					serve_until(serve.as_mut(), "the object", || sent(b"before") == 1).await;
					request.push(&forward_update(version, 2, false));
					paused = 1;
				}
				serve_until(serve.as_mut(), "the pause", || {
					answers() == (true, paused) && h.track.subscription().is_none()
				})
				.await;

				group.write_frame(timestamp(), b"paused".as_slice()).unwrap();
				request.push(&forward_update(version, 4, true));
				serve_until(serve.as_mut(), "the resume", || {
					answers().1 == paused + 1 && h.track.subscription().is_some()
				})
				.await;
				group.write_frame(timestamp(), b"after".as_slice()).unwrap();
				serve_until(serve.as_mut(), "the object after", || sent(b"after") == 1).await;
				assert_eq!(sent(b"paused"), 0, "{version}: served what it missed");
			}
		}
	}

	/// A resume joins as a new SUBSCRIBE would, so a joining FETCH after it is served from where
	/// the resume joined rather than refused as the pause left it.
	#[moq_clock::test]
	async fn a_joining_fetch_after_a_resume_is_served() {
		let version = Version::Draft18;
		let h = serve(version);
		let mut group = h.track.create_group(group::Info { sequence: 0 }).unwrap();
		group.write_frame(timestamp(), b"head".as_slice()).unwrap();
		let mut request = ScriptedSession::new(Vec::new());
		request.log = h.log.clone();
		let stream = Stream::open(&mut crate::driver::Transport::new(request.clone()), version)
			.await
			.unwrap();
		let mut msg = subscribe(Filter::NextObject, None);
		msg.forward = false;
		let mut serve = std::pin::pin!(h.publisher.clone().run_subscribe_stream(stream, msg));
		serve_until(serve.as_mut(), "the pause", || {
			!h.log.trail().is_empty() && h.track.subscription().is_none()
		})
		.await;
		request.push(&forward_update(version, 4, true));
		serve_until(serve.as_mut(), "the resume", || {
			Wire::read(&h.log, version).sent(ietf::RequestOk::ID).is_some()
		})
		.await;

		let mark = h.log.writes.lock().unwrap().len();
		let mut buf = bytes::Bytes::from(joining_fetch(&h, mark).await.unwrap());
		let id = crate::coding::decode_varint(&mut buf, version).unwrap();
		assert_eq!(id, ietf::FetchOk::ID, "the joining FETCH was refused");
	}

	/// A resume that cannot subscribe again fails its update: REQUEST_ERROR answers it, then
	/// PUBLISH_DONE says UPDATE_FAILED.
	#[moq_clock::test]
	async fn a_failed_resume_answers_its_update() {
		for version in [Version::Draft22] {
			let h = serve(version);
			let mut request = ScriptedSession::new(Vec::new());
			request.log = h.log.clone();
			let stream = Stream::open(&mut crate::driver::Transport::new(request.clone()), version)
				.await
				.unwrap();
			let mut msg = subscribe(Filter::NextObject, None);
			msg.forward = false;
			let mut serve = std::pin::pin!(h.publisher.clone().run_subscribe_stream(stream, msg));
			serve_until(serve.as_mut(), "the pause", || {
				!h.log.trail().is_empty() && h.track.subscription().is_none()
			})
			.await;

			h.track.abort(Error::Transport("upstream failed".into())).unwrap();
			request.push(&forward_update(version, 4, true));
			moq_clock::timeout(std::time::Duration::from_secs(10), serve)
				.await
				.expect("the failed resume ends the subscription")
				.unwrap_err();
			let wire = Wire::read(&h.log, version);
			let error = wire.sent(ietf::RequestError::ID).expect("the resume is answered");
			let done = wire.sent(ietf::PublishDone::ID).expect("the subscription ends");
			assert!(done.start() > error.end(), "{version}: {:?}", wire.messages);
			assert_eq!(
				wire.publish_done.map(|(status, _)| status),
				Some(ietf::PublishDoneStatus::UpdateFailed.code()),
				"{version}"
			);
		}
	}

	/// A REQUEST_UPDATE with Request ID `id` carrying only a NEW_GROUP_REQUEST for `value`,
	/// framed for the subscription's stream.
	fn new_group_update(version: Version, id: u64, value: u64) -> Vec<u8> {
		let mut body = Vec::new();
		ietf::SubscribeUpdate {
			max_age: None,
			new_group: Some(value),
			request_id: RequestId(id),
			subscriber_priority: None,
			forward: None,
			filter: None,
			authorization_token: None,
			fill: None,
		}
		.encode_msg(&mut Encoder::new(&mut body, version.into()), version)
		.unwrap();
		let mut frame = vec![ietf::SubscribeUpdate::ID as u8, 0, body.len() as u8];
		frame.extend(body);
		frame
	}

	/// The SUBSCRIBE_OK written on the request stream.
	fn subscribe_ok(log: &Log, version: Version) -> ietf::SubscribeOk {
		let trail = log.trail();
		let request = request_stream(&trail);
		let mut bytes = Vec::new();
		for (stream, sent) in &trail {
			if let (true, Sent::Write(buf)) = (*stream == request, sent) {
				bytes.extend_from_slice(buf);
			}
		}
		let mut rest = bytes.as_slice();
		let id = crate::coding::decode_varint(&mut rest, version).unwrap();
		assert_eq!(id, ietf::SubscribeOk::ID, "not a SUBSCRIBE_OK");
		let size = usize::from(u16::from_be_bytes([rest[0], rest[1]]));
		ietf::SubscribeOk::decode_msg(&mut Decoder::new(&rest[2..2 + size], version.into()), version).unwrap()
	}

	/// Group 4 of `h`'s track: a 3 byte first frame and a 5 byte delta, over a refresh
	/// ratio of 1.
	fn refresh_group(h: &Serve) -> group::Producer {
		let mut group = h.track.create_group(group::Info { sequence: 4 }).unwrap();
		group.write_frame(Timestamp::ZERO, b"key".as_slice()).unwrap();
		group.write_frame(Timestamp::ZERO, b"delta".as_slice()).unwrap();
		group
	}

	/// A SUBSCRIBE is a join, with or without NEW_GROUP_REQUEST: a track opted in to refresh
	/// wants the group past one over its ratio, and SUBSCRIBE_OK advertises DYNAMIC_GROUPS.
	#[moq_clock::test]
	async fn a_subscribe_joins_under_the_refresh_ratio() {
		for version in [Version::Draft22] {
			for refresh in [Some(1), None] {
				let h = serve_track(version, crate::track::Info::timed().with_refresh(refresh));
				let _group = refresh_group(&h);
				let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), version)
					.await
					.unwrap();
				let msg = ietf::Subscribe {
					new_group: Some(0),
					..subscribe(Filter::NextObject, None)
				};
				let wanted = if refresh.is_some() { 5 } else { 0 };
				let mut serve = std::pin::pin!(h.publisher.clone().run_subscribe_stream(stream, msg));
				serve_until(serve.as_mut(), "the join reached the track", || {
					h.track.demand().wanted() == Some(wanted) && !h.log.trail().is_empty()
				})
				.await;
				assert_eq!(
					subscribe_ok(&h.log, version).properties.dynamic_groups,
					refresh.is_some(),
					"{version}"
				);
			}
		}
	}

	/// An absolute filter has a fixed start, so its SUBSCRIBE is no join even over the ratio.
	#[moq_clock::test]
	async fn an_absolute_subscribe_is_no_join() {
		let version = Version::Draft22;
		let h = serve_track(version, crate::track::Info::timed().with_refresh(1));
		let _group = refresh_group(&h);
		let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), version)
			.await
			.unwrap();
		let msg = subscribe(
			Filter::Absolute {
				start: Location { group: 4, object: 0 },
				end: None,
			},
			None,
		);
		let mut serve = std::pin::pin!(h.publisher.clone().run_subscribe_stream(stream, msg));
		serve_until(serve.as_mut(), "SUBSCRIBE_OK", || !h.log.trail().is_empty()).await;
		assert_eq!(h.track.demand().wanted(), Some(0));
	}

	/// An absolute SUBSCRIBE carrying NEW_GROUP_REQUEST asks as an update would: past a group
	/// the track holds, it is a join under the ratio.
	#[moq_clock::test]
	async fn an_absolute_subscribe_with_a_new_group_request_joins() {
		let version = Version::Draft22;
		for (value, wanted) in [(0, 5), (5, 5), (4, 0)] {
			let h = serve_track(version, crate::track::Info::timed().with_refresh(1));
			let _group = refresh_group(&h);
			let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), version)
				.await
				.unwrap();
			let msg = ietf::Subscribe {
				new_group: Some(value),
				..subscribe(
					Filter::Absolute {
						start: Location { group: 4, object: 0 },
						end: None,
					},
					None,
				)
			};
			let mut serve = std::pin::pin!(h.publisher.clone().run_subscribe_stream(stream, msg));
			serve_until(serve.as_mut(), "SUBSCRIBE_OK", || !h.log.trail().is_empty()).await;
			assert_eq!(h.track.demand().wanted(), Some(wanted), "{value}");
		}
	}

	/// An update's NEW_GROUP_REQUEST is a join on a track opted in to refresh, answered with
	/// REQUEST_OK. One naming a group the track holds asks nothing.
	#[moq_clock::test]
	async fn a_new_group_update_joins_a_refreshing_track() {
		for version in [Version::Draft22] {
			for (value, wanted) in [(0, 5), (5, 5), (4, 0)] {
				let h = serve_track(version, crate::track::Info::timed().with_refresh(1));
				let session = ScriptedSession::new(new_group_update(version, 10, value));
				let mut stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), version)
					.await
					.unwrap();
				// Subscribed before the group, so only the update can ask for a new one.
				let subscriber = h.track.subscribe(None);
				let _group = refresh_group(&h);
				let mut serving = TrackServe::new(
					crate::driver::Transport::new(h.session.clone()),
					subscriber,
					RequestId(0),
					version,
					ServeRange::default(),
					None,
				);
				let mut finished = false;
				let mut goaways = super::super::request_stream::Messages::new(
					crate::driver::Transport::new(h.session.clone()),
					h.publisher.control.client(),
				);
				let mut run = std::pin::pin!(h.publisher.run_subscription(
					&mut stream,
					&mut serving,
					&mut finished,
					&mut goaways,
					async {},
					None
				));
				assert!(futures::poll!(run.as_mut()).is_pending(), "{version}: update ended it");
				assert_eq!(h.track.demand().wanted(), Some(wanted), "{version} {value}");
				let written = session.log.writes.lock().unwrap().clone();
				assert_eq!(written[0], ietf::RequestOk::ID as u8, "{version}: {written:?}");
			}
		}
	}

	/// A track without refresh cannot honor a NEW_GROUP_REQUEST update, which fails the
	/// subscription like any update it cannot apply.
	#[moq_clock::test]
	async fn a_new_group_update_without_refresh_fails() {
		for version in [Version::Draft22] {
			let h = serve(version);
			let session = ScriptedSession::new(new_group_update(version, 10, 9));
			let mut stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), version)
				.await
				.unwrap();
			let mut goaways = super::super::request_stream::Messages::new(
				crate::driver::Transport::new(h.session.clone()),
				h.publisher.control.client(),
			);
			let mut serving = TrackServe::new(
				crate::driver::Transport::new(h.session.clone()),
				h.track.subscribe(None),
				RequestId(0),
				version,
				ServeRange::default(),
				None,
			);
			let mut finished = false;
			let served = h
				.publisher
				.run_subscription(&mut stream, &mut serving, &mut finished, &mut goaways, async {}, None)
				.await;
			assert!(
				matches!(served, Some(Served::UpdateFailed(Error::Unsupported))),
				"{version}: {served:?}"
			);
		}
	}

	/// An update whose only parameters are AUTHORIZATION TOKENs holding `tokens`, framed for
	/// the subscription's stream.
	fn update_with_tokens(tokens: &[&[u8]]) -> Vec<u8> {
		// Every parameter but the token is left as it is.
		let mut body = vec![2];
		body.extend(token_params(tokens));
		let mut frame = vec![ietf::SubscribeUpdate::ID as u8, 0, body.len() as u8];
		frame.extend(body);
		frame
	}

	/// An update's AUTHORIZATION TOKEN that this endpoint cannot use closes the session with
	/// the draft's code, as it would on the SUBSCRIBE, not only the subscription.
	#[moq_clock::test]
	async fn an_update_token_that_cannot_be_used_closes_the_session() {
		for version in [Version::Draft22] {
			for (token, expected, why) in [
				(
					&[0x01, 0x07, 0x00][..],
					crate::SessionError::AuthTokenCacheOverflow,
					"REGISTER",
				),
				(&[0x00, 0x07][..], crate::SessionError::UnknownAuthTokenAlias, "DELETE"),
				(
					&[0x02, 0x07][..],
					crate::SessionError::UnknownAuthTokenAlias,
					"USE_ALIAS",
				),
				(
					&[0x03][..],
					crate::SessionError::KeyValueFormatting,
					"USE_VALUE without a type",
				),
			] {
				let h = serve(version);
				let session = ScriptedSession::per_stream(vec![update_with_tokens(&[token])]);
				let mut stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), version)
					.await
					.unwrap();
				let mut goaways = super::super::request_stream::Messages::new(
					crate::driver::Transport::new(h.session.clone()),
					h.publisher.control.client(),
				);
				let mut serving = TrackServe::new(
					crate::driver::Transport::new(h.session.clone()),
					h.track.subscribe(None),
					RequestId(0),
					version,
					ServeRange::default(),
					None,
				);
				let mut finished = false;
				let served = h
					.publisher
					.run_subscription(&mut stream, &mut serving, &mut finished, &mut goaways, async {}, None)
					.await;
				assert!(
					matches!(served, Some(Served::Track(Err(_)) | Served::UpdateFailed(_))),
					"{version} {why}: {served:?}"
				);
				let codes: Vec<u32> = h.log.closes().into_iter().map(|(code, _)| code).collect();
				assert_eq!(codes, vec![expected.to_code()], "{version} {why}");
			}
		}
	}

	/// A parameter block of AUTHORIZATION TOKENs. Each key is coded as a delta from the one
	/// before, so a repeat is 0.
	fn token_params(tokens: &[&[u8]]) -> Vec<u8> {
		let mut params = vec![tokens.len() as u8];
		for (i, token) in tokens.iter().enumerate() {
			let delta = i > 0;
			params.extend([if delta { 0 } else { 0x03 }, token.len() as u8]);
			params.extend(*token);
		}
		params
	}

	/// USE_VALUE, type 0, value 0xAA.
	const TOKEN: &[u8] = &[0x03, 0x00, 0xAA];

	/// Section 8.9: a request whose tokens repeat is rejected MALFORMED_AUTH_TOKEN (0x4).
	/// Distinct tokens are legal but refused NOT_SUPPORTED,
	/// since authorizing on one would ignore the rest. Either way the session stays open, and
	/// a single token is answered as usual.
	#[moq_clock::test]
	async fn a_request_with_repeated_or_several_tokens_is_refused_alone() {
		for (version, malformed) in [(Version::Draft22, 0x4)] {
			let request = |tokens: &[&[u8]]| {
				let mut body = track_status("room", true, version);
				assert_eq!(body.pop(), Some(0), "{version}");
				body.extend(token_params(tokens));
				body
			};

			let code = refusal(version, ietf::TrackStatus::ID, request(&[TOKEN, TOKEN])).await;
			assert_eq!(code, malformed, "{version}");

			let other = [0x03, 0x00, 0xBB];
			let code = refusal(version, ietf::TrackStatus::ID, request(&[TOKEN, &other])).await;
			let unsupported = request::to_code(&Error::Unsupported, request::Kind::TrackStatus, version);
			assert_eq!(code, unsupported, "{version}: several tokens");

			// Junk after the tokens still closes the session: a token refusal must not hide it.
			for tokens in [[TOKEN, TOKEN], [TOKEN, &other]] {
				let mut body = request(&tokens);
				body.push(0);
				let h = serve(version);
				let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), version)
					.await
					.unwrap();
				let closed = h
					.publisher
					.clone()
					.handle_stream(
						ietf::TrackStatus::ID,
						ietf::Body(bytes::Bytes::from(body)),
						stream,
						false,
					)
					.err();
				assert!(matches!(closed, Some(Error::WrongSize)), "{version}: {closed:?}");
			}

			let h = serve(version);
			assert_eq!(answer_code(&h, request(&[TOKEN]), version).await, None, "{version}");
		}
	}

	/// An update whose tokens repeat is answered MALFORMED_AUTH_TOKEN rather than
	/// NOT_SUPPORTED, and one with several distinct tokens NOT_SUPPORTED. Either fails only its
	/// subscription.
	#[moq_clock::test]
	async fn a_repeated_or_several_update_tokens_are_refused() {
		let other: &[u8] = &[0x03, 0x00, 0xBB];
		for (version, tokens, code) in [
			(Version::Draft22, [TOKEN, TOKEN], 0x4),
			(Version::Draft22, [TOKEN, other], 0x3),
		] {
			let h = serve(version);
			let session = ScriptedSession::per_stream(vec![update_with_tokens(&tokens)]);
			let mut stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), version)
				.await
				.unwrap();
			let mut goaways = super::super::request_stream::Messages::new(
				crate::driver::Transport::new(h.session.clone()),
				h.publisher.control.client(),
			);
			let mut serving = TrackServe::new(
				crate::driver::Transport::new(h.session.clone()),
				h.track.subscribe(None),
				RequestId(0),
				version,
				ServeRange::default(),
				None,
			);
			let mut finished = false;
			let mut run = std::pin::pin!(h.publisher.run_subscription(
				&mut stream,
				&mut serving,
				&mut finished,
				&mut goaways,
				async {},
				None
			));
			let mut served = None;
			for _ in 0..50 {
				if let std::task::Poll::Ready(done) = futures::poll!(run.as_mut()) {
					served = Some(done);
					break;
				}
				settle().await;
			}
			assert!(
				matches!(served, Some(Some(Served::UpdateFailed(Error::Unsupported)))),
				"{version}: {served:?}"
			);
			assert!(h.log.closes().is_empty(), "{version}: the session closed");

			let wire = session.log.writes.lock().unwrap().clone();
			let mut buf = Decoder::new(&wire, version.into());
			assert_eq!(buf.varint().unwrap(), ietf::RequestError::ID, "{version}");
			let error = ietf::RequestError::decode(&mut buf, version).unwrap();
			assert_eq!(error.error_code, code, "{version}");
		}
	}

	#[moq_clock::test]
	async fn subscription_track_property_filter_closes_the_session() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			let h = serve(version);
			let session = ScriptedSession::new(vec![0x02, 0, 6, 2, 1, 0x29, 2, 0, 0]);
			let mut stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), version)
				.await
				.unwrap();
			let mut goaways = super::super::request_stream::Messages::new(
				crate::driver::Transport::new(h.session.clone()),
				h.publisher.control.client(),
			);
			let mut serving = TrackServe::new(
				crate::driver::Transport::new(h.session.clone()),
				h.track.subscribe(None),
				RequestId(0),
				version,
				ServeRange::default(),
				None,
			);
			let result = h
				.publisher
				.run_subscription(&mut stream, &mut serving, &mut false, &mut goaways, async {}, None)
				.await;
			assert!(
				matches!(result, Some(Served::Track(Err(Error::Decode(_))))),
				"{version}: {result:?}"
			);
			assert_eq!(
				h.log.closes().iter().map(|(code, _)| *code).collect::<Vec<_>>(),
				[0x03],
				"{version}"
			);
			assert!(session.log.writes.lock().unwrap().is_empty(), "no REQUEST_ERROR");
		}
	}

	/// A source track failing to decode ends only the subscriptions it feeds, not the requester's session.
	#[moq_clock::test]
	async fn source_decode_failure_keeps_the_session() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			let h = serve(version);
			let session = ScriptedSession::new(Vec::new());
			let mut stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), version)
				.await
				.unwrap();
			let mut goaways = super::super::request_stream::Messages::new(
				crate::driver::Transport::new(h.session.clone()),
				h.publisher.control.client(),
			);
			let mut serving = TrackServe::new(
				crate::driver::Transport::new(h.session.clone()),
				h.track.subscribe(None),
				RequestId(0),
				version,
				ServeRange::default(),
				None,
			);
			h.track.clone().abort(Error::Decode(DecodeError::InvalidValue)).unwrap();
			let result = h
				.publisher
				.run_subscription(&mut stream, &mut serving, &mut false, &mut goaways, async {}, None)
				.await;
			assert!(
				matches!(result, Some(Served::Track(Err(Error::Decode(_))))),
				"{version}: {result:?}"
			);
			assert!(h.log.closes().is_empty(), "{version}: the session closed");
		}
	}

	#[moq_clock::test]
	async fn namespace_updates_do_not_postpone_an_auth_withdrawal() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			let h = serve(version);
			settle().await;
			let session = ScriptedSession::new(Vec::new());
			let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), version)
				.await
				.unwrap();
			let mut running = std::pin::pin!(h.publisher.clone().run_subscribe_namespace_stream(
				stream,
				ietf::SubscribeNamespace {
					request_id: RequestId(0),
					namespace: crate::Path::empty(),
					hidden: false
				}
			));
			assert!(futures::poll!(running.as_mut()).is_pending());
			let mark = session.log.writes.lock().unwrap().len();
			h.publisher.auth.handle().authorize(&crate::auth::Grant::default());
			// An otherwise valid no-op update is ready at the same time as revocation.
			session.push(&[0x02, 0, 2, 2, 0]);
			assert!(futures::poll!(running.as_mut()).is_pending());
			let answer = session.log.writes.lock().unwrap().clone();
			let mut wire = Decoder::new(&answer[mark..], version.into());
			assert_eq!(
				wire.varint().unwrap(),
				ietf::NamespaceDone::ID,
				"{version}: withdrawal precedes update OK"
			);
			assert_eq!(
				ietf::NamespaceDone::decode(&mut wire, version).unwrap().suffix.as_str(),
				"room"
			);
			assert_eq!(wire.varint().unwrap(), ietf::RequestOk::ID);
			ietf::RequestOk::decode(&mut wire, version).unwrap();
			assert!(wire.is_empty());
		}
	}

	/// A refused namespace update waits for its REQUEST_ERROR to be acknowledged instead of
	/// letting the writer's drop reset discard it.
	#[moq_clock::test]
	async fn refused_namespace_update_is_delivered_before_the_stream_ends() {
		let version = Version::Draft22;
		let h = serve(version);
		settle().await;
		let session = ScriptedSession::new(Vec::new()).with_unacked_fin();
		let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), version)
			.await
			.unwrap();
		let msg = ietf::SubscribeNamespace {
			request_id: RequestId(0),
			namespace: crate::Path::from_static("room"),
			hidden: false,
		};
		let mut running = std::pin::pin!(h.publisher.clone().run_subscribe_namespace_stream(stream, msg));
		assert!(futures::poll!(running.as_mut()).is_pending());
		// Request ID 2; TRACK_NAMESPACE_PREFIX is the reserved one-field tuple ".session".
		let mut update = vec![0x02, 0, 13, 2, 1, 0x34, 1, 8];
		update.extend_from_slice(b".session");
		session.push(&update);
		assert!(
			futures::poll!(running.as_mut()).is_pending(),
			"{version}: ended before the ack"
		);
		assert!(
			session
				.log
				.trail()
				.iter()
				.any(|(_, sent)| matches!(sent, crate::lite::test_transport::Sent::Finish)),
			"{version}: no FIN"
		);
		assert!(session.log.resets().is_empty(), "{version}: the refusal was reset");
		assert!(h.log.closes().is_empty(), "{version}: the session closed");
	}

	#[moq_clock::test]
	async fn namespace_updates_move_the_prefix_after_acknowledgement() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			let h = serve(version);
			let _old = h
				._origin
				.announce("old/first", crate::origin::Route::default())
				.unwrap();
			let _new = h
				._origin
				.announce("new/second", crate::origin::Route::default())
				.unwrap();
			settle().await;
			let session = ScriptedSession::new(Vec::new());
			let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), version)
				.await
				.unwrap();
			let msg = ietf::SubscribeNamespace {
				request_id: RequestId(0),
				namespace: crate::Path::from_static("old"),
				hidden: false,
			};
			let mut running = std::pin::pin!(h.publisher.clone().run_subscribe_namespace_stream(stream, msg));
			assert!(futures::poll!(running.as_mut()).is_pending());
			let first = session.log.writes.lock().unwrap().clone();
			let mut wire = Decoder::new(&first, version.into());
			assert_eq!(wire.varint().unwrap(), ietf::RequestOk::ID);
			ietf::RequestOk::decode(&mut wire, version).unwrap();
			assert_eq!(wire.varint().unwrap(), ietf::Namespace::ID);
			assert_eq!(
				ietf::Namespace::decode(&mut wire, version).unwrap().suffix.as_str(),
				"first"
			);
			assert!(wire.is_empty());
			// Request ID 2; TRACK_NAMESPACE_PREFIX is the one-field tuple "new".
			let update = [0x02, 0, 8, 2, 1, 0x34, 1, 3, b'n', b'e', b'w'];
			// Partial messages do not consume the update or change the current prefix.
			session.push(&update[..6]);
			assert!(futures::poll!(running.as_mut()).is_pending());
			assert_eq!(*session.log.writes.lock().unwrap(), first);
			session.push(&update[6..]);
			assert!(futures::poll!(running.as_mut()).is_pending());
			let answer = session.log.writes.lock().unwrap().clone();
			let mut wire = Decoder::new(&answer[first.len()..], version.into());
			assert_eq!(wire.varint().unwrap(), ietf::NamespaceDone::ID, "{version}");
			assert_eq!(
				ietf::NamespaceDone::decode(&mut wire, version).unwrap().suffix.as_str(),
				"first"
			);
			assert_eq!(wire.varint().unwrap(), ietf::RequestOk::ID, "{version}");
			ietf::RequestOk::decode(&mut wire, version).unwrap();
			assert_eq!(wire.varint().unwrap(), ietf::Namespace::ID, "{version}");
			assert_eq!(
				ietf::Namespace::decode(&mut wire, version).unwrap().suffix.as_str(),
				"second"
			);
			assert!(wire.is_empty());
		}
	}

	#[moq_clock::test]
	async fn namespace_requester_fin_is_version_gated() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			let h = serve(version);
			moq_clock::sim::yield_now().await;
			let session = ScriptedSession::per_stream_eof(vec![vec![]]);
			let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), version)
				.await
				.unwrap();
			let msg = ietf::SubscribeNamespace {
				request_id: RequestId(0),
				namespace: crate::Path::from_static(""),
				hidden: false,
			};
			let mut run = std::pin::pin!(h.publisher.clone().run_subscribe_namespace_stream(stream, msg));
			assert_eq!(
				futures::poll!(run.as_mut()).is_ready(),
				super::super::request_stream::fin_cancels(version),
				"{version}"
			);
		}
	}

	/// A distinctive request id, so `[FetchHeader::TYPE, REQUEST_ID]` is a usable needle.
	const REQUEST_ID: u64 = 0x2B;

	fn subscribe(filter: Filter, fill: Option<ietf::Fill>) -> ietf::Subscribe<'static> {
		ietf::Subscribe {
			max_age: None,
			new_group: None,
			request_id: RequestId(REQUEST_ID),
			track_namespace: crate::Path::from_static("room"),
			track_name: "video".into(),
			subscriber_priority: 128,
			group_order: GroupOrder::Descending,
			filter,
			fill,
			properties_wanted: true,
			forward: true,
			rendezvous: false,
			range_filters: false,
			authorization_token: None,
			epoch: None,
		}
	}

	/// The bytes that begin every fill fetch stream.
	const FETCH_STREAM: &[u8] = &[FetchHeader::TYPE as u8, REQUEST_ID as u8];

	/// Serve `msg` against the live track, then finish the track so the subscription
	/// completes. Subscribing after the finish would be rejected instead of served.
	async fn run_live(h: &mut Serve, msg: ietf::Subscribe<'static>) {
		// `create_broadcast` registers the broadcast from a spawned task, so yield to the
		// runtime before subscribing or the lookup 404s.
		moq_clock::sleep(std::time::Duration::from_millis(1)).await;

		let session = h.session.clone();
		let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), h.publisher.version)
			.await
			.unwrap();
		let mut serve = std::pin::pin!(h.publisher.clone().run_subscribe_stream(stream, msg));

		// Everything cached serves immediately; the subscription then parks at the live
		// edge, which is where the track is allowed to finish.
		for _ in 0..200 {
			assert!(
				futures::poll!(serve.as_mut()).is_pending(),
				"subscription ended before the track finished"
			);
		}

		h.track.finish().unwrap();
		serve.await.unwrap();
	}

	/// A subscribe for a broadcast we do not serve is refused with the negotiated draft's own
	/// "does not exist" value, 0x10, not 0x4 (MALFORMED_AUTH_TOKEN): a peer told the wrong
	/// one re-authenticates instead of waiting for the announcement. The reply is encoded here rather than matched by code alone, so
	/// a value slipping outside the draft's table cannot pass, and the reason phrase is the
	/// origin's own, which is what carries a refusal the registry has no value for.
	#[moq_clock::test]
	async fn a_missing_broadcast_is_refused_with_the_draft_s_code() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			let error_code = 0x10;

			let h = serve(version);
			let session = h.session.clone();
			let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), version)
				.await
				.unwrap();

			let mut msg = subscribe(Filter::NextObject, None);
			msg.track_namespace = crate::Path::from_static("absent");

			h.publisher.clone().run_subscribe_stream(stream, msg).await.unwrap();

			let expected = {
				let log = crate::lite::test_transport::Log::default();
				let mut writer = crate::coding::Writer::new(
					crate::transport::boxed::SendStream::new(crate::lite::test_transport::SinkSend::new(log.clone())),
					version,
				);

				writer.varint(ietf::RequestError::ID).await.unwrap();
				writer
					.encode(&ietf::RequestError {
						error_code,
						reason_phrase: Error::Unroutable.to_string().into(),
						retry_interval: 0,
					})
					.await
					.unwrap();

				log.writes.lock().unwrap().clone()
			};

			assert_eq!(
				occurrences(&h.log, &expected),
				1,
				"{version} must refuse a missing broadcast with {error_code:#x}"
			);
		}
	}

	/// The refusal of request [`REQUEST_ID`] as gone.
	async fn unroutable(version: Version) -> Vec<u8> {
		let log = crate::lite::test_transport::Log::default();
		let mut writer = crate::coding::Writer::new(
			crate::transport::boxed::SendStream::new(crate::lite::test_transport::SinkSend::new(log.clone())),
			version,
		);
		writer.varint(ietf::RequestError::ID).await.unwrap();
		writer
			.encode(&ietf::RequestError {
				error_code: 0x10,
				reason_phrase: Error::Unroutable.to_string().into(),
				retry_interval: 0,
			})
			.await
			.unwrap();
		log.writes.lock().unwrap().clone()
	}

	/// Each request MoQ Epoch pins to an instance, naming `epoch`, encoded for `version`.
	fn pinned_requests(version: Version, epoch: &crate::Epoch) -> [(u64, Vec<u8>); 3] {
		let mut subscribe = subscribe(Filter::NextObject, None);
		subscribe.epoch = Some(epoch.clone());
		let fetch = ietf::Fetch {
			request_id: RequestId(REQUEST_ID),
			subscriber_priority: 128,
			group_order: GroupOrder::Ascending,
			fetch_type: fetch_range(
				version,
				"video",
				Location { group: 0, object: 0 },
				Location { group: 0, object: 1 },
			),
			range_filters: false,
			fill_timeout: false,
			properties_wanted: true,
			epoch: Some(epoch.clone()),
		};
		let status = ietf::TrackStatus {
			request_id: RequestId(REQUEST_ID),
			track_namespace: crate::Path::from_static("room"),
			track_name: "video".into(),
			properties_wanted: true,
			epoch: Some(epoch.clone()),
		};
		let mut bodies: [Vec<u8>; 3] = Default::default();
		subscribe
			.encode_msg(&mut Encoder::new(&mut bodies[0], version.into()), version)
			.unwrap();
		fetch
			.encode_msg(&mut Encoder::new(&mut bodies[1], version.into()), version)
			.unwrap();
		status
			.encode_msg(&mut Encoder::new(&mut bodies[2], version.into()), version)
			.unwrap();
		let [subscribe, fetch, status] = bodies;
		[
			(ietf::Subscribe::ID, subscribe),
			(ietf::Fetch::ID, fetch),
			(ietf::TrackStatus::ID, status),
		]
	}

	/// MoQ Epoch pins SUBSCRIBE, FETCH and TRACK_STATUS to the instance they name: the
	/// broadcast under the path now being another instance, or one with no epoch at all,
	/// refuses them as gone instead of serving another instance's content.
	#[moq_clock::test]
	async fn a_request_for_another_instance_is_refused() {
		for version in [Version::Draft22] {
			let served = crate::Epoch::mint();
			let stale = crate::Epoch::mint();
			let pinned = crate::origin::Route::default().with_epoch(served.clone());
			for (name, route, named, refused) in [
				("another epoch", pinned.clone(), &stale, true),
				("no epoch", crate::origin::Route::default(), &stale, true),
				("its epoch", pinned, &served, false),
			] {
				let mut h = serve_route(version, crate::track::Info::timed(), route.clone());
				publish_groups(&mut h, 0);
				settle().await;
				let expected = unroutable(version).await;
				for (id, body) in pinned_requests(version, named) {
					let mark = h.log.writes.lock().unwrap().len();
					let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), version)
						.await
						.unwrap();
					let mut task = h
						.publisher
						.clone()
						.handle_stream(id, ietf::Body(bytes::Bytes::from(body)), stream, true)
						.unwrap();
					// A served SUBSCRIBE parks at the live edge, so poll rather than await.
					for _ in 0..50 {
						if futures::poll!(task.as_mut()).is_ready() {
							break;
						}
						settle().await;
					}
					let wire = h.log.writes.lock().unwrap()[mark..].to_vec();
					let found = wire.windows(expected.len()).any(|window| window == expected.as_slice());
					assert_eq!(found, refused, "{version} {name}: request {id:#x}");
				}
			}
		}
	}

	/// A SUBSCRIBE pinned to a gone instance is refused as missing even when it asks to
	/// wait: an instance never comes back, so there is nothing to rendezvous with.
	#[moq_clock::test]
	async fn a_pinned_request_never_waits_for_its_instance() {
		for version in [Version::Draft22] {
			let stale = crate::Epoch::mint();
			let pinned = crate::origin::Route::default().with_epoch(crate::Epoch::mint());
			for (name, route) in [("another epoch", pinned), ("no epoch", crate::origin::Route::default())] {
				let mut h = serve_route(version, crate::track::Info::timed(), route);
				publish_groups(&mut h, 0);
				settle().await;
				let expected = unroutable(version).await;

				// Built by hand: we never send RENDEZVOUS_TIMEOUT ourselves.
				let encode = |body: &mut Vec<u8>| -> Result<(), crate::coding::EncodeError> {
					let w = &mut Encoder::new(body, version.into());
					RequestId(REQUEST_ID).encode(w, version)?;
					super::super::namespace::encode_track(w, &crate::Path::from_static("room"), "video")?;
					encode_params!(w, version,
						0x04 => 7u64,
						super::super::epoch::EPOCH_PARAM => Some(stale.clone()),
					);
					Ok(())
				};
				let mut body = Vec::new();
				encode(&mut body).unwrap();

				let mark = h.log.writes.lock().unwrap().len();
				let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), version)
					.await
					.unwrap();
				h.publisher
					.clone()
					.handle_stream(ietf::Subscribe::ID, ietf::Body(body.into()), stream, true)
					.unwrap()
					.await;
				let wire = h.log.writes.lock().unwrap()[mark..].to_vec();
				assert!(
					wire.windows(expected.len()).any(|window| window == expected.as_slice()),
					"{version} {name}"
				);
			}
		}
	}

	/// An EPOCH on a request breaks a session that did not negotiate MoQ Epoch: the peer
	/// cannot have learned an instance it was never told about.
	#[moq_clock::test]
	async fn an_epoch_without_the_extension_is_a_violation() {
		for version in [Version::Draft22] {
			let h = serve(version);
			for (id, body) in pinned_requests(version, &crate::Epoch::mint()) {
				let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), version)
					.await
					.unwrap();
				let result = h
					.publisher
					.clone()
					.handle_stream(id, ietf::Body(bytes::Bytes::from(body)), stream, false);
				assert!(
					matches!(result, Err(Error::ProtocolViolation)),
					"{version}: request {id:#x}"
				);
			}
		}
	}

	const ALL_VERSIONS: [Version; 3] = [Version::Draft18, Version::Draft20, Version::Draft22];

	fn track_status(namespace: &str, wanted: bool, version: Version) -> Vec<u8> {
		let msg = ietf::TrackStatus {
			request_id: RequestId(REQUEST_ID),
			track_namespace: crate::Path::new(namespace).unwrap(),
			track_name: "video".into(),
			properties_wanted: wanted,
			epoch: None,
		};
		let mut body = Vec::new();
		msg.encode_msg(&mut Encoder::new(&mut body, version.into()), version)
			.unwrap();
		body
	}

	/// TRACK_STATUS gets what a SUBSCRIBE_OK would carry, as far as each draft's answer has
	/// room, without subscribing to the track. INCLUDE_PROPERTIES=0 empties the properties.
	#[moq_clock::test]
	async fn track_status_is_answered_on_every_draft() {
		for version in ALL_VERSIONS {
			for wanted in [true, false] {
				let h = serve(version);
				let mut group = h.track.create_group(group::Info { sequence: 4 }).unwrap();
				group.write_frame(timestamp(), b"a".as_slice()).unwrap();
				group.write_frame(timestamp(), b"b".as_slice()).unwrap();
				moq_clock::sleep(std::time::Duration::from_millis(1)).await;

				let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), version)
					.await
					.unwrap();
				let mark = h.log.writes.lock().unwrap().len();
				let body = track_status("room", wanted, version);
				h.publisher
					.clone()
					.handle_stream(
						ietf::TrackStatus::ID,
						ietf::Body(bytes::Bytes::from(body)),
						stream,
						false,
					)
					.unwrap()
					.await;
				assert!(h.log.resets().is_empty(), "{version}: the answer was reset");
				assert!(h.track.subscription().is_none(), "{version}: TRACK_STATUS subscribed");

				let wire = h.log.writes.lock().unwrap()[mark..].to_vec();
				let mut buf = Decoder::new(&wire, version.into());
				assert_eq!(buf.varint().unwrap(), ietf::TrackStatusOk::ID, "{version}");
				let ok = ietf::TrackStatusOk::decode(&mut buf, version).unwrap();
				assert!(buf.is_empty(), "{version}: trailing bytes");

				let expected = match version {
					// Only draft-22 can carry the opt-out.
					_ if !wanted && Filter::is_draft20(version) => ietf::Properties::default(),
					_ => track_properties(&track::Info::timed(), true, version, true).unwrap(),
				};
				assert_eq!(
					ok,
					ietf::TrackStatusOk {
						largest: Some(Location { group: 4, object: 1 }),
						properties: expected,
					},
					"{version} wanted={wanted}"
				);
			}
		}
	}

	/// A TRACK_STATUS outside what the peer may subscribe to is refused before the track
	/// resolves, and one whose grant narrows while it resolves is refused too.
	#[moq_clock::test]
	async fn track_status_outside_the_grant_is_refused() {
		let nothing = || crate::auth::Grant {
			publish: Default::default(),
			subscribe: Default::default(),
			expires: None,
		};
		let unauthorized = |version| request::to_code(&Error::Unauthorized, request::Kind::TrackStatus, version);

		for version in ALL_VERSIONS {
			let mut h = serve(version);
			let claim = h
				._origin
				.requests_route("live", crate::origin::Route::default(), None)
				.unwrap();
			let auth = crate::auth::Link::new(false);
			auth.handle().authorize(&nothing());
			h.publisher = h.publisher.clone().with_auth(auth);
			let code = moq_clock::timeout(
				std::time::Duration::from_secs(1),
				answer_code(&h, track_status("live/cam", true, version), version),
			)
			.await
			.unwrap_or_else(|_| panic!("{version}: waited on a broadcast it may not answer for"));
			assert_eq!(code, Some(unauthorized(version)), "{version}: denied up front");
			use futures::FutureExt;
			assert!(
				claim.requested_broadcast().now_or_never().is_none(),
				"{version}: a denied request reached the origin"
			);
		}

		for version in ALL_VERSIONS {
			let mut h = serve(version);
			let claim = h
				._origin
				.requests_route("live", crate::origin::Route::default(), None)
				.unwrap();
			let auth = crate::auth::Link::new(false);
			h.publisher = h.publisher.clone().with_auth(auth.clone());

			let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), version)
				.await
				.unwrap();
			let mark = h.log.writes.lock().unwrap().len();
			let body = track_status("live/cam", true, version);
			let mut answer = std::pin::pin!(
				h.publisher
					.clone()
					.handle_stream(
						ietf::TrackStatus::ID,
						ietf::Body(bytes::Bytes::from(body)),
						stream,
						false
					)
					.unwrap()
			);
			assert!(futures::poll!(answer.as_mut()).is_pending(), "{version}");

			// Parked resolving the broadcast when the grant narrows.
			let request = claim.requested_broadcast().await.unwrap();
			auth.handle().authorize(&nothing());
			let output = crate::broadcast::Info::new().produce();
			let _track = output.create_track("video", None).unwrap();
			request.accept(&output);
			answer.await;

			let code = refusal_code(&h.log.writes.lock().unwrap()[mark..], version);
			assert_eq!(code, Some(unauthorized(version)), "{version}: narrowed while resolving");
		}
	}

	/// A request's USE_VALUE token decodes but grants nothing yet, so a TRACK_STATUS outside
	/// the session grant is still refused.
	#[moq_clock::test]
	async fn a_request_token_does_not_widen_the_grant() {
		let nothing = crate::auth::Grant {
			publish: Default::default(),
			subscribe: Default::default(),
			expires: None,
		};
		for version in [Version::Draft22] {
			let mut h = serve(version);
			let auth = crate::auth::Link::new(false);
			auth.handle().authorize(&nothing);
			h.publisher = h.publisher.clone().with_auth(auth);

			let mut body = track_status("room", true, version);
			// Replace the empty parameter block with an AUTHORIZATION TOKEN: USE_VALUE, type 0.
			assert_eq!(body.pop(), Some(0), "{version}");
			body.extend([0x01, 0x03, 0x03, 0x03, 0x00, 0xAA]);

			let unauthorized = request::to_code(&Error::Unauthorized, request::Kind::TrackStatus, version);
			assert_eq!(answer_code(&h, body, version).await, Some(unauthorized), "{version}");
		}
	}

	/// A TRACK_STATUS answer parked on its request stream when the grant narrows is reset,
	/// never delivered once the stream drains.
	#[moq_clock::test]
	async fn track_status_narrowed_while_answering_is_reset() {
		for version in ALL_VERSIONS {
			let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
			let broadcast = origin.publish("room", crate::origin::Route::default()).unwrap();
			let _track = broadcast.create_track("video", None).unwrap();
			settle().await;

			let bi = kio::Producer::new(true);
			let session = SinkSession::gated_bi(bi.consume());
			let peer_setup = peer::PeerSetup::default();
			peer_setup.set(peer::Peer::default());
			let auth = crate::auth::Link::new(false);
			let publisher = Publisher::new(
				crate::time::sim(),
				crate::driver::Transport::new(session.clone()),
				origin.consume(),
				Control::new(false),
				None,
				peer_setup,
				version,
			)
			.with_auth(auth.clone());

			let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), version)
				.await
				.unwrap();
			let mark = session.log.writes.lock().unwrap().len();
			// The request stream stops taking writes, so the answer parks on it.
			let Ok(mut writable) = bi.write() else {
				panic!("request stream gate closed");
			};
			*writable = false;
			drop(writable);
			let mut answer = std::pin::pin!(publisher.clone().run_track_status_stream(
				stream,
				ietf::TrackStatus {
					request_id: RequestId(REQUEST_ID),
					track_namespace: crate::Path::from_static("room"),
					track_name: "video".into(),
					properties_wanted: true,
					epoch: None,
				},
			));
			for _ in 0..100 {
				assert!(futures::poll!(answer.as_mut()).is_pending(), "{version}: never parked");
				moq_clock::sim::yield_now().await;
			}

			auth.handle().authorize(&crate::auth::Grant {
				publish: Default::default(),
				subscribe: Default::default(),
				expires: None,
			});
			let Ok(mut writable) = bi.write() else {
				panic!("request stream gate closed");
			};
			*writable = true;
			drop(writable);
			let res = moq_clock::timeout(std::time::Duration::from_secs(1), answer)
				.await
				.unwrap_or_else(|_| panic!("{version}: still answering after the grant narrowed"));
			assert!(matches!(res, Err(Error::Unauthorized)), "{version}: {res:?}");
			// The sink logs a write only once its gate lets it through, so even the parked
			// first write never landed.
			assert!(
				session.log.writes.lock().unwrap()[mark..].is_empty(),
				"{version}: the answer went out after the grant narrowed"
			);
			assert!(!session.log.resets().is_empty(), "{version}: the answer was not reset");
		}
	}

	/// Run one TRACK_STATUS to its end, returning the refusal code it got, if any.
	async fn answer_code(h: &Serve, body: Vec<u8>, version: Version) -> Option<u64> {
		let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), version)
			.await
			.unwrap();
		let mark = h.log.writes.lock().unwrap().len();
		h.publisher
			.clone()
			.handle_stream(
				ietf::TrackStatus::ID,
				ietf::Body(bytes::Bytes::from(body)),
				stream,
				false,
			)
			.unwrap()
			.await;
		refusal_code(&h.log.writes.lock().unwrap()[mark..], version)
	}

	/// The refusal code on the wire, or `None` for any other answer.
	fn refusal_code(wire: &[u8], version: Version) -> Option<u64> {
		let mut buf = Decoder::new(wire, version.into());
		let id = buf.varint().unwrap();
		match version {
			_ if id == ietf::RequestError::ID => {
				Some(ietf::RequestError::decode(&mut buf, version).unwrap().error_code)
			}
			_ => None,
		}
	}

	/// A TRACK_STATUS for a broadcast we do not serve is refused as a SUBSCRIBE would be.
	#[moq_clock::test]
	async fn track_status_for_a_missing_broadcast_is_refused() {
		for version in ALL_VERSIONS {
			let code = refusal(version, ietf::TrackStatus::ID, track_status("absent", true, version)).await;
			let expected = 0x10;
			assert_eq!(code, expected, "{version}");
		}
	}

	/// Opting out of SUBSCRIBE_OK's properties does not strip the objects' timestamps:
	/// their units come from TRACK_STATUS instead.
	#[moq_clock::test]
	async fn the_properties_opt_out_keeps_object_timestamps() {
		let version = Version::Draft22;
		let stamp = crate::Timestamp::from_millis(123_456).unwrap();
		let mut properties = Vec::new();
		ietf::encode_object_time(
			&mut Encoder::new(&mut properties, version.into()),
			stamp,
			track::Info::timed().timescale.expect("a timed track"),
		)
		.unwrap();

		for wanted in [true, false] {
			let mut h = serve(version);
			let mut group = h.track.create_group(group::Info { sequence: 0 }).unwrap();
			group.write_frame(stamp, b"stamped".as_slice()).unwrap();
			group.finish().unwrap();

			let mut msg = subscribe(Filter::Unfiltered, None);
			msg.properties_wanted = wanted;
			run_live(&mut h, msg).await;

			assert_eq!(occurrences(&h.log, b"stamped"), 1, "wanted={wanted}");
			assert_eq!(occurrences(&h.log, &properties), 1, "wanted={wanted}: object unstamped");
		}
	}

	/// Dispatch one request stream, returning the refusal code it wrote.
	///
	/// `handle_stream` returning `Ok` is what keeps the session open: the dispatch loop
	/// closes the session over an `Err`.
	async fn refusal(version: Version, id: u64, body: Vec<u8>) -> u64 {
		let h = serve(version);
		let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), version)
			.await
			.unwrap();
		let mark = h.log.writes.lock().unwrap().len();
		h.publisher
			.clone()
			.handle_stream(id, ietf::Body(bytes::Bytes::from(body)), stream, false)
			.unwrap_or_else(|e| panic!("{version}: the request closed the session: {e}"))
			.await;
		assert!(h.log.resets().is_empty(), "{version}: refusal was reset");

		let wire = h.log.writes.lock().unwrap()[mark..].to_vec();
		let mut buf = Decoder::new(&wire, version.into());
		assert_eq!(buf.varint().unwrap(), ietf::RequestError::ID);
		ietf::RequestError::decode(&mut buf, version).unwrap().error_code
	}

	#[moq_clock::test]
	async fn range_filters_over_the_default_limit_get_invalid_filter() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			let h = serve(version);
			for fetch in [false, true] {
				let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), version)
					.await
					.unwrap();
				let mark = h.log.writes.lock().unwrap().len();
				if fetch {
					h.publisher
						.clone()
						.run_fetch_stream(
							stream,
							ietf::Fetch {
								request_id: RequestId(REQUEST_ID),
								subscriber_priority: 128,
								group_order: GroupOrder::Descending,
								fetch_type: if Filter::is_draft20(version) {
									FetchType::Filtered {
										namespace: crate::Path::from_static("room"),
										track: "video".into(),
										filter: Filter::Unfiltered,
									}
								} else {
									FetchType::Standalone {
										namespace: crate::Path::from_static("room"),
										track: "video".into(),
										start: Location { group: 0, object: 0 },
										end: Location { group: 0, object: 0 },
									}
								},
								properties_wanted: true,
								range_filters: true,
								fill_timeout: false,
								epoch: None,
							},
						)
						.await
						.unwrap();
				} else {
					let mut msg = subscribe(Filter::NextObject, None);
					msg.range_filters = true;
					h.publisher.clone().run_subscribe_stream(stream, msg).await.unwrap();
				}
				assert_eq!(
					refusal_code(&h.log.writes.lock().unwrap()[mark..], version),
					Some(0x36),
					"{version}: fetch={fetch}"
				);
				assert!(h.log.closes().is_empty());
			}
		}
	}

	#[moq_clock::test]
	async fn rendezvous_wait_is_capped_at_zero_but_answers_timeout() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			// A wait applies only to a missing publisher; a missing track on a live one does not exist.
			for (namespace, track, publisher) in [("missing", "video", false), ("room", "audio", true)] {
				for wait in [None, Some(0), Some(7)] {
					let mut body = Vec::new();
					let mut w = Encoder::new(&mut body, version.into());
					RequestId(REQUEST_ID).encode(&mut w, version).unwrap();
					super::super::namespace::encode_namespace(&mut w, &crate::Path::new(namespace).unwrap()).unwrap();
					w.string(track).unwrap();
					w.varint(u64::from(wait.is_some())).unwrap();
					if let Some(wait) = wait {
						w.varint(0x04).unwrap();
						w.varint(wait).unwrap();
					}
					assert_eq!(
						refusal(version, ietf::Subscribe::ID, body).await,
						if !publisher && wait.is_some_and(|millis| millis > 0) {
							0x02
						} else {
							0x10
						},
						"{version}: {namespace}/{track} {wait:?}"
					);
				}
			}
		}
	}

	#[moq_clock::test]
	async fn reserved_namespace_requests_never_reach_the_application() {
		let version = Version::Draft22;
		for path in [".", "./child", ".session", ".session/extension"] {
			let h = serve(version);
			let claim = h
				._origin
				.requests_route("", crate::origin::Route::default(), None)
				.unwrap();
			settle().await;
			let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), version)
				.await
				.unwrap();
			let mark = h.log.writes.lock().unwrap().len();
			let body = track_status(path, true, version);
			let mut serving = std::pin::pin!(
				h.publisher
					.clone()
					.handle_stream(ietf::TrackStatus::ID, ietf::Body(body.into()), stream, false)
					.unwrap()
			);
			let done = futures::poll!(serving.as_mut());
			assert!(
				claim.requested_broadcast().now_or_never().is_none(),
				"{version}: {path} reached the application"
			);
			assert!(done.is_ready(), "{version}: {path} was left waiting");
			assert_eq!(refusal_code(&h.log.writes.lock().unwrap()[mark..], version), Some(0x10));
			assert!(h.log.closes().is_empty());
		}
	}

	/// A namespace that overflows `Path::MAX_BYTES` under a deep root is refused per
	/// request, and the session stays open.
	#[moq_clock::test]
	async fn a_namespace_too_long_under_the_root_is_refused_per_request() {
		for version in ALL_VERSIONS {
			let h = serve(version);
			let root = "r".repeat(crate::Path::MAX_BYTES - 2);
			let scoped = h
				._origin
				.consume()
				.scope(root.as_str(), &crate::Patterns::from(crate::Pattern::all()))
				.unwrap();
			let peer_setup = peer::PeerSetup::default();
			peer_setup.set(peer::Peer::default());
			let publisher = Publisher::new(
				crate::time::sim(),
				crate::driver::Transport::new(h.session.clone()),
				scoped,
				Control::new(false),
				None,
				peer_setup,
				version,
			);
			let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), version)
				.await
				.unwrap();
			let mark = h.log.writes.lock().unwrap().len();
			publisher
				.run_subscribe_stream(stream, subscribe(Filter::NextObject, None))
				.await
				.unwrap();

			let wire = h.log.writes.lock().unwrap()[mark..].to_vec();
			let mut buf = Decoder::new(&wire, version.into());
			assert_eq!(buf.varint().unwrap(), ietf::RequestError::ID, "{version}");
			let code = ietf::RequestError::decode(&mut buf, version).unwrap().error_code;
			let expected = request::to_code(
				&Error::from(crate::path::Error::TooLong),
				request::Kind::Subscribe,
				version,
			);
			assert_eq!(code, expected, "{version}");
			assert!(h.log.closes().is_empty(), "{version}");
		}
	}

	/// Legal requests we don't serve are refused NOT_SUPPORTED one at a time, and the
	/// session stays open for the next one.
	#[moq_clock::test]
	async fn legal_requests_we_do_not_serve_are_refused_per_request() {
		const NOT_SUPPORTED: u64 = 0x3;

		// Draft-22 bytes from the figures: Request ID 0x2B, Track Namespace ("room"),
		// Track Name ("video"), then the parameters.
		let head: &[u8] = &[
			0x2B, 0x01, 0x04, b'r', b'o', b'o', b'm', 0x05, b'v', b'i', b'd', b'e', b'o',
		];

		#[rustfmt::skip]
		let cases: [(&str, u64, &[u8]); 2] = [
			("SUBSCRIBE with a Range Filter", ietf::Subscribe::ID, &[
				0x02, // Number of Parameters
				0x03, 0x03, 0x03, 0x00, 0xAA, // AUTHORIZATION TOKEN
				0x23, 0x02, 0x00, 0x05, // OBJECTID_FILTER (0x26): SetID 0, from 5
			]),
			("FETCH", ietf::Fetch::ID, &[
				0x03, // Number of Parameters
				0x0A, 0x00, // FILL_TIMEOUT = 0
				0x17, 0x01, 0x00, // LOCATION_FILTER (0x21): whole track
				0x14, 0x01, // INCLUDE_PROPERTIES (0x35) = 1
			]),
		];

		let version = Version::Draft22;
		for (label, id, params) in cases {
			assert_eq!(
				refusal(version, id, [head, params].concat()).await,
				if id == ietf::Subscribe::ID { 0x36 } else { NOT_SUPPORTED },
				"{version}: {label}"
			);
		}
	}

	/// A SUBSCRIBE_NAMESPACE always asks for namespaces, and gets NAMESPACE for each.
	#[moq_clock::test]
	async fn subscribe_namespace_gets_namespaces() {
		let version = Version::Draft18;
		let h = serve(version);
		settle().await;

		let mut body = Vec::new();
		let mut w = Encoder::new(&mut body, version.into());
		ietf::SubscribeNamespace {
			request_id: RequestId(REQUEST_ID),
			namespace: crate::Path::from_static(""),
			hidden: false,
		}
		.encode_msg(&mut w, version)
		.unwrap();

		let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), version)
			.await
			.unwrap();
		let mark = h.log.writes.lock().unwrap().len();
		let task = h
			.publisher
			.clone()
			.handle_stream(
				ietf::SubscribeNamespace::ID,
				ietf::Body(bytes::Bytes::from(body)),
				stream,
				false,
			)
			.unwrap_or_else(|e| panic!("{version}: the request closed the session: {e}"));
		let mut task = std::pin::pin!(task);
		let mut done = false;
		for _ in 0..100 {
			done = futures::poll!(task.as_mut()).is_ready();
			if done || occurrences(&h.log, b"room") > 0 {
				break;
			}
			settle().await;
		}

		let wire = h.log.writes.lock().unwrap()[mark..].to_vec();
		let mut buf = Decoder::new(&wire, version.into());
		assert!(!done, "the subscription ended");
		assert_eq!(buf.varint().unwrap(), ietf::RequestOk::ID);
		ietf::RequestOk::decode(&mut buf, version).unwrap();
		assert_eq!(buf.varint().unwrap(), ietf::Namespace::ID);
		assert_eq!(
			ietf::Namespace::decode(&mut buf, version).unwrap().suffix.as_str(),
			"room"
		);
		assert!(buf.is_empty(), "trailing bytes");
	}

	/// A Next Object or unfiltered join gives its current-group fill the head exactly
	/// once, while the subscription starts after that snapshot.
	#[moq_clock::test]
	async fn canonical_join_serves_the_head_on_a_fetch_stream() {
		for filter in [Filter::NextObject, Filter::Unfiltered] {
			let mut h = serve(Version::Draft22);
			let mut earlier = h.track.create_group(group::Info { sequence: 0 }).unwrap();
			earlier.write_frame(timestamp(), b"earlier".as_slice()).unwrap();
			earlier.finish().unwrap();

			let mut group = h.track.create_group(group::Info { sequence: 1 }).unwrap();
			for payload in [b"head-0", b"head-1", b"head-2"] {
				group.write_frame(timestamp(), payload.as_slice()).unwrap();
			}
			group.finish().unwrap();

			run_live(
				&mut h,
				subscribe(
					filter,
					Some(ietf::Fill {
						group_order: None,
						filter: Some(Filter::Relative(1)),
						range_filters: false,
					}),
				),
			)
			.await;

			assert_eq!(occurrences(&h.log, FETCH_STREAM), 1, "expected one fill fetch stream");
			assert_eq!(occurrences(&h.log, b"earlier"), 0, "the join excludes earlier groups");
			for payload in [b"head-0", b"head-1", b"head-2"] {
				assert_eq!(
					occurrences(&h.log, payload),
					1,
					"each object exactly once, via the fill"
				);
			}
			assert!(h.log.resets().is_empty(), "a served fill must not reset");
		}
	}

	/// Narrowing an open subscription retires a group the new range excludes, whole or in
	/// part, before it forwards an object outside that range.
	#[moq_clock::test]
	async fn narrowing_retires_an_open_live_group() {
		let narrowed = [
			// The whole group falls below the new start.
			(Location { group: 4, object: 0 }, None, true),
			// The new end stops inside the open group.
			(
				Location { group: 0, object: 0 },
				Some(EndLocation {
					group: 1,
					object: Some(0),
				}),
				false,
			),
			// The new start moves past the open group's next object.
			(Location { group: 1, object: 2 }, None, true),
		];
		for (start, end, wanted) in narrowed {
			let version = Version::Draft22;
			let h = serve(version);
			let mut group = h.track.create_group(group::Info { sequence: 1 }).unwrap();
			group.write_frame(timestamp(), b"before-update".as_slice()).unwrap();
			moq_clock::sim::yield_now().await;
			let mut request = ScriptedSession::new(Vec::new());
			request.log = h.log.clone();
			let stream = Stream::open(&mut crate::driver::Transport::new(request.clone()), version)
				.await
				.unwrap();
			let mut serving = std::pin::pin!(
				h.publisher
					.clone()
					.run_subscribe_stream(stream, subscribe(Filter::Unfiltered, None))
			);
			serve_until(serving.as_mut(), "the current group is open", || {
				occurrences(&h.log, b"before-update") == 1
			})
			.await;
			let update = ietf::SubscribeUpdate {
				request_id: RequestId(2),
				subscriber_priority: None,
				forward: None,
				filter: Some(Filter::Absolute { start, end }),
				authorization_token: None,
				new_group: None,
				max_age: None,
				fill: None,
			};
			let mut bytes = Vec::new();
			let mut encoder = Encoder::new(&mut bytes, version.into());
			encoder.varint(ietf::SubscribeUpdate::ID).unwrap();
			update.encode(&mut encoder, version).unwrap();
			request.push(&bytes);
			serve_until(serving.as_mut(), "the narrowing is accepted", || {
				Wire::read(&h.log, version).sent(ietf::RequestOk::ID).is_some()
			})
			.await;
			group.write_frame(timestamp(), b"after-update".as_slice()).unwrap();
			group.finish().unwrap();
			let mut later = h.track.create_group(group::Info { sequence: 4 }).unwrap();
			later.write_frame(timestamp(), b"wanted".as_slice()).unwrap();
			later.finish().unwrap();
			h.track.finish().unwrap();
			serving.await.unwrap();
			assert_eq!(
				occurrences(&h.log, b"after-update"),
				0,
				"an object outside {start:?}..{end:?} is not forwarded"
			);
			assert_eq!(
				occurrences(&h.log, b"wanted"),
				usize::from(wanted),
				"the subscription remains live"
			);
		}
	}

	/// Widening with a fill hands each remaining object of the current group to exactly one
	/// stream: the live stream for what it still covers, and a fill tail for the rest,
	/// including all of it once a narrowing retired the live stream.
	#[moq_clock::test]
	async fn widening_with_a_fill_streams_the_current_group_once() {
		let open = Filter::Unfiltered;
		// The live stream stops after object 1, which the widening lifts.
		let capped = Filter::Absolute {
			start: Location { group: 1, object: 0 },
			end: Some(EndLocation {
				group: 1,
				object: Some(1),
			}),
		};
		let cases = [
			(open, None),
			(capped, None),
			// Retires the whole group.
			(open, Some(Location { group: 4, object: 0 })),
			// Retires it within the group, at the fill's edge.
			(open, Some(Location { group: 1, object: 1 })),
		];
		for (filter, narrowing) in cases {
			let version = Version::Draft22;
			let h = serve(version);
			let mut history = h.track.create_group(group::Info { sequence: 0 }).unwrap();
			history.write_frame(timestamp(), b"history".as_slice()).unwrap();
			history.finish().unwrap();
			let mut group = h.track.create_group(group::Info { sequence: 1 }).unwrap();
			group.write_frame(timestamp(), b"before-update".as_slice()).unwrap();
			moq_clock::sim::yield_now().await;
			let mut request = ScriptedSession::new(Vec::new());
			request.log = h.log.clone();
			let stream = Stream::open(&mut crate::driver::Transport::new(request.clone()), version)
				.await
				.unwrap();
			let mut serving = std::pin::pin!(
				h.publisher
					.clone()
					.run_subscribe_stream(stream, subscribe(filter, None))
			);
			serve_until(serving.as_mut(), "the current group is open", || {
				occurrences(&h.log, b"before-update") == 1
			})
			.await;
			let mut updates = vec![(Location { group: 1, object: 0 }, true)];
			if let Some(start) = narrowing {
				updates.insert(0, (start, false));
			}
			for (answered, (start, fill)) in updates.into_iter().enumerate() {
				let update = ietf::SubscribeUpdate {
					request_id: RequestId(2 + 2 * answered as u64),
					subscriber_priority: None,
					forward: None,
					filter: Some(Filter::Absolute { start, end: None }),
					authorization_token: None,
					new_group: None,
					max_age: None,
					fill: fill.then_some(ietf::Fill {
						group_order: None,
						filter: None,
						range_filters: false,
					}),
				};
				let mut bytes = Vec::new();
				let mut encoder = Encoder::new(&mut bytes, version.into());
				encoder.varint(ietf::SubscribeUpdate::ID).unwrap();
				update.encode(&mut encoder, version).unwrap();
				request.push(&bytes);
				serve_until(serving.as_mut(), "the update is accepted", || {
					let wire = Wire::read(&h.log, version);
					wire.messages
						.iter()
						.filter(|(id, _)| *id == ietf::RequestOk::ID)
						.count() > answered
				})
				.await;
			}
			for payload in [b"after-1", b"after-2"] {
				group.write_frame(timestamp(), payload.as_slice()).unwrap();
			}
			group.finish().unwrap();
			h.track.finish().unwrap();
			serving.await.unwrap();
			assert_eq!(
				occurrences(&h.log, b"history"),
				0,
				"the fill only carries the current head"
			);
			for payload in [b"after-1", b"after-2"] {
				assert_eq!(
					occurrences(&h.log, payload),
					1,
					"one stream carries each later object ({filter:?}, narrowed to {narrowing:?})"
				);
			}
		}
	}

	/// A fill-only update keeps a live join's floor, so a fill short of the live edge does not
	/// also hand the groups below that floor to the live cursor.
	#[moq_clock::test]
	async fn a_fill_only_update_keeps_the_live_join_floor() {
		let version = Version::Draft22;
		let h = serve(version);
		let mut history = h.track.create_group(group::Info { sequence: 0 }).unwrap();
		history.write_frame(timestamp(), b"history".as_slice()).unwrap();
		history.finish().unwrap();
		let mut group = h.track.create_group(group::Info { sequence: 1 }).unwrap();
		group.write_frame(timestamp(), b"live".as_slice()).unwrap();
		moq_clock::sim::yield_now().await;
		let mut request = ScriptedSession::new(Vec::new());
		request.log = h.log.clone();
		let stream = Stream::open(&mut crate::driver::Transport::new(request.clone()), version)
			.await
			.unwrap();
		let mut serving = std::pin::pin!(
			h.publisher
				.clone()
				.run_subscribe_stream(stream, subscribe(Filter::Unfiltered, None))
		);
		serve_until(serving.as_mut(), "the current group is open", || {
			occurrences(&h.log, b"live") == 1
		})
		.await;
		let update = ietf::SubscribeUpdate {
			request_id: RequestId(2),
			subscriber_priority: None,
			forward: None,
			filter: None,
			authorization_token: None,
			new_group: None,
			max_age: None,
			fill: Some(ietf::Fill {
				group_order: None,
				filter: Some(Filter::Absolute {
					start: Location { group: 0, object: 0 },
					end: Some(EndLocation { group: 0, object: None }),
				}),
				range_filters: false,
			}),
		};
		let mut bytes = Vec::new();
		let mut encoder = Encoder::new(&mut bytes, version.into());
		encoder.varint(ietf::SubscribeUpdate::ID).unwrap();
		update.encode(&mut encoder, version).unwrap();
		request.push(&bytes);
		serve_until(serving.as_mut(), "the update is accepted", || {
			Wire::read(&h.log, version).sent(ietf::RequestOk::ID).is_some()
		})
		.await;
		group.finish().unwrap();
		h.track.finish().unwrap();
		serving.await.unwrap();
		assert_eq!(occurrences(&h.log, b"history"), 0, "the historical fill is refused");
		assert_eq!(occurrences(&h.log, b"live"), 1, "the live group is not repeated");
	}

	/// An independent historical fill never expands the subscription's live bounds.
	#[moq_clock::test]
	async fn a_historical_update_fill_keeps_the_live_bounds() {
		for end in [None, Some(EndLocation { group: 0, object: None })] {
			let version = Version::Draft22;
			let h = serve(version);
			let mut group = h.track.create_group(group::Info { sequence: 1 }).unwrap();
			group.write_frame(timestamp(), b"history".as_slice()).unwrap();
			moq_clock::sim::yield_now().await;
			let mut request = ScriptedSession::new(Vec::new());
			request.log = h.log.clone();
			let stream = Stream::open(&mut crate::driver::Transport::new(request.clone()), version)
				.await
				.unwrap();
			let mut serving = std::pin::pin!(h.publisher.clone().run_subscribe_stream(
				stream,
				subscribe(
					Filter::Absolute {
						start: Location { group: 20, object: 0 },
						end: None
					},
					None
				)
			));
			serve_until(serving.as_mut(), "the initial subscription is accepted", || {
				!h.log.trail().is_empty() && Wire::read(&h.log, version).sent(ietf::SubscribeOk::ID).is_some()
			})
			.await;
			let update = ietf::SubscribeUpdate {
				request_id: RequestId(2),
				subscriber_priority: None,
				forward: None,
				filter: Some(Filter::Absolute {
					start: Location {
						group: if end.is_some() { 0 } else { 4 },
						object: 0,
					},
					end,
				}),
				authorization_token: None,
				new_group: None,
				max_age: None,
				fill: Some(ietf::Fill {
					group_order: None,
					filter: Some(Filter::Unfiltered),
					range_filters: false,
				}),
			};
			let mut bytes = Vec::new();
			let mut encoder = Encoder::new(&mut bytes, version.into());
			encoder.varint(ietf::SubscribeUpdate::ID).unwrap();
			update.encode(&mut encoder, version).unwrap();
			request.push(&bytes);
			serve_until(serving.as_mut(), "the historical fill is accepted", || {
				Wire::read(&h.log, version).sent(ietf::RequestOk::ID).is_some()
			})
			.await;
			group.write_frame(timestamp(), b"later-current".as_slice()).unwrap();
			group.finish().unwrap();
			for (sequence, payload) in [(2, b"excluded".as_slice()), (4, b"wanted".as_slice())] {
				let mut group = h.track.create_group(group::Info { sequence }).unwrap();
				group.write_frame(timestamp(), payload).unwrap();
				group.finish().unwrap();
			}
			h.track.finish().unwrap();
			serving.await.unwrap();
			assert_eq!(occurrences(&h.log, b"history"), 0, "the historical fill is refused");
			assert_eq!(occurrences(&h.log, b"excluded"), 0, "the live floor stays at group 4");
			assert_eq!(
				occurrences(&h.log, b"later-current"),
				0,
				"the fill owns only its snapshot"
			);
			assert_eq!(occurrences(&h.log, b"wanted"), usize::from(end.is_none()));
		}
	}

	/// moq-lite's own join over draft-22: Relative(1) names the start of the current
	/// group, so the cache replays the whole group on the subscription stream.
	#[moq_clock::test]
	async fn relative_one_replays_the_current_group() {
		let mut h = serve(Version::Draft22);

		let mut group = h.track.create_group(group::Info { sequence: 0 }).unwrap();
		for payload in [b"head-0", b"head-1", b"head-2"] {
			group.write_frame(timestamp(), payload.as_slice()).unwrap();
		}
		group.finish().unwrap();

		run_live(&mut h, subscribe(Filter::Relative(1), None)).await;

		assert_eq!(occurrences(&h.log, FETCH_STREAM), 0, "no fill was requested");
		for payload in [b"head-0", b"head-1", b"head-2"] {
			assert_eq!(occurrences(&h.log, payload), 1, "the whole group replays in range");
		}
	}

	/// A Next Object subscription never receives the already-published head of the
	/// current group: everything below the snapshot is outside the requested range.
	#[moq_clock::test]
	async fn next_object_does_not_replay_the_head() {
		let mut h = serve(Version::Draft22);

		let mut group = h.track.create_group(group::Info { sequence: 0 }).unwrap();
		for payload in [b"head-0", b"head-1", b"head-2"] {
			group.write_frame(timestamp(), payload.as_slice()).unwrap();
		}
		group.finish().unwrap();

		run_live(&mut h, subscribe(Filter::NextObject, None)).await;

		for payload in [b"head-0", b"head-1", b"head-2"] {
			assert_eq!(
				occurrences(&h.log, payload),
				0,
				"the head is outside the requested range"
			);
		}
	}

	/// A wider fill resets its promised stream and leaves the live subscription open.
	#[moq_clock::test]
	async fn a_multi_group_fill_is_refused_without_ending_the_subscription() {
		let mut h = serve(Version::Draft22);

		for sequence in 0..2 {
			let mut group = h.track.create_group(group::Info { sequence }).unwrap();
			group.write_frame(timestamp(), b"frame".as_slice()).unwrap();
			group.finish().unwrap();
		}

		run_live(
			&mut h,
			subscribe(
				Filter::NextObject,
				Some(ietf::Fill {
					group_order: None,
					filter: Some(Filter::Relative(2)),
					range_filters: false,
				}),
			),
		)
		.await;

		assert_eq!(occurrences(&h.log, FETCH_STREAM), 1, "the promised stream still opens");
		assert_eq!(h.log.resets().len(), 1, "only the fill stream resets");
		assert_eq!(occurrences(&h.log, b"frame"), 0, "no history is served");
		assert!(h.log.closes().is_empty(), "the session stays open");
	}

	/// Subscriber priority 128 on the wire, which every request in these tests carries.
	const PRIORITY: i8 = super::super::priority::from_wire(128);

	/// The streams a FETCH or fill competes with: one at a higher priority and one at a lower
	/// one, plus two at its own priority on either side of `request_id`.
	fn rival_streams(queue: &PriorityQueue, request_id: RequestId) -> [PriorityHandle; 4] {
		[
			queue.insert(Priority::new(72, 1, 0)),
			queue.insert(Priority::new(PRIORITY, request_id.0 - 1, 0)),
			queue.insert(Priority::new(PRIORITY, request_id.0 + 1, 0)),
			queue.insert(Priority::new(-78, 2, 0)),
		]
	}

	/// A fill ranks as the group it fills, in its subscription's GROUP_ORDER, and follows
	/// that rank while it is open: the queue moving it, and its subscriber priority.
	#[moq_clock::test]
	async fn a_fill_ranks_as_the_group_it_fills() {
		let h = serve(Version::Draft22);
		let mut group = h.track.create_group(group::Info { sequence: 4 }).unwrap();
		group.write_frame(timestamp(), b"fill".as_slice()).unwrap();

		let queue = h.publisher.priority.clone();
		let priority = kio::Producer::new(PRIORITY);
		let ranking = Ranking {
			queue: queue.clone(),
			group: None,
			request_id: RequestId(REQUEST_ID),
			priority: priority.consume(),
			publisher: 0,
			order: Order::Descending,
		};
		let [higher, _older, _later, _lower] = rival_streams(&queue, ranking.request_id);
		// Newest first: the subscription's group 5 goes ahead of the fill, and group 3 after.
		let _groups = [5, 3].map(|group| ranking.rank(Location { group, object: 0 }));

		let opened = AtomicU64::new(0);
		let fill = FillServe::Group {
			sequence: 4,
			skip: 0,
			until: None,
		};
		let mut fill =
			std::pin::pin!(
				h.publisher
					.run_fill(fill, &ranking, h.track.consume(), None, &opened, ranking.request_id)
			);
		assert!(futures::poll!(fill.as_mut()).is_pending(), "the group is still open");
		assert_eq!(
			h.log.priorities(),
			[252],
			"behind the higher priority, the older subscription, and group 5"
		);

		drop(higher);
		assert!(futures::poll!(fill.as_mut()).is_pending());
		assert_eq!(h.log.priorities(), [252, 253], "the queue moved it up");

		let Ok(mut current) = priority.write() else {
			unreachable!("this test holds the producer")
		};
		*current = i8::MAX;
		drop(current);
		assert!(futures::poll!(fill.as_mut()).is_pending());
		assert_eq!(
			h.log.priorities(),
			[252, 253, 255],
			"a REQUEST_UPDATE moved it to the top"
		);

		group.finish().unwrap();
		fill.await;
		assert_eq!(occurrences(&h.log, b"fill"), 1);
		assert!(h.log.resets().is_empty(), "a served fill must not reset");
	}

	/// A fill of the current group outranks the live stream carrying the rest of that group,
	/// which the subscriber cannot use without the fill's objects first.
	#[moq_clock::test]
	async fn a_fill_outranks_the_rest_of_its_group() {
		let h = serve(Version::Draft22);
		let mut group = h.track.create_group(group::Info { sequence: 4 }).unwrap();
		group.write_frame(timestamp(), b"head".as_slice()).unwrap();

		let ranking = Ranking {
			queue: h.publisher.priority.clone(),
			group: None,
			request_id: RequestId(REQUEST_ID),
			priority: kio::Producer::new(PRIORITY).consume(),
			publisher: 0,
			order: Order::Descending,
		};
		// The live stream, from object 1 on, queued first so the slab id alone would favor it.
		let Rank::Queue(mut live) = ranking.rank(Location { group: 4, object: 1 }) else {
			unreachable!("no send groups")
		};
		assert_eq!(live.handle.send_order(), 255);

		let opened = AtomicU64::new(0);
		let fill = FillServe::Group {
			sequence: 4,
			skip: 0,
			until: Some(1),
		};
		h.publisher
			.run_fill(fill, &ranking, h.track.consume(), None, &opened, ranking.request_id)
			.await;
		assert_eq!(h.log.priorities(), [255], "the start of the group goes first");
		assert_eq!(occurrences(&h.log, b"head"), 1);
	}

	/// A standalone FETCH competes with group streams by its subscriber priority alone, its
	/// request id breaking ties, whatever group and GROUP_ORDER it asks for.
	#[moq_clock::test]
	async fn a_standalone_fetch_ranks_by_its_priority() {
		for version in [Version::Draft22] {
			for (group, group_order) in [(1, GroupOrder::Ascending), (3, GroupOrder::Descending)] {
				let mut h = serve(version);
				publish_groups(&mut h, 3);
				settle().await;
				let _rivals = rival_streams(&h.publisher.priority, FETCH_ID);

				let whole = Location { group, object: 0 };
				let wire = standalone_fetch(&h, whole, whole, group_order).await;
				assert_eq!(fetch_answer(wire, version).1.len(), 1, "{version}: one object");
				// The fetch stream opens last, after the request stream.
				assert_eq!(
					h.log.priorities().last(),
					Some(&253),
					"{version} {group_order:?}: between its rivals at the same priority"
				);
			}
		}
	}

	/// A joining FETCH ranks as the group it fetches in the subscription it joins, not as a
	/// request of its own.
	#[moq_clock::test]
	async fn a_joining_fetch_ranks_in_its_subscription() {
		const LATEST: u64 = 5;

		for version in JOINING_DRAFTS {
			let mut h = serve(version);
			publish_groups(&mut h, LATEST);
			settle().await;

			let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), version)
				.await
				.unwrap();
			let mut serve = std::pin::pin!(
				h.publisher
					.clone()
					.run_subscribe_stream(stream, subscribe(Filter::NextObject, None))
			);
			registered(&h, serve.as_mut()).await;

			// Keyed by the FETCH's own id, it would land after both of the subscription's.
			let queue = h.publisher.priority.clone();
			let subscription = RequestId(REQUEST_ID);
			let _streams = [
				queue.insert(Priority::new(PRIORITY, subscription.0, LATEST + 1)),
				queue.insert(Priority::new(PRIORITY, subscription.0, LATEST - 1)),
			];
			let _rivals = rival_streams(&queue, subscription);

			let mark = h.log.writes.lock().unwrap().len();
			joining_fetch(&h, mark).await.unwrap();
			// The fetch stream opens last, after the request stream.
			assert_eq!(
				h.log.priorities().last(),
				Some(&252),
				"{version}: behind the higher priority, the older subscription, and the newer group"
			);
		}
	}

	/// The request id of the joining FETCH, distinct from the subscription's.
	const FETCH_ID: RequestId = RequestId(0x2C);

	/// Every draft that still carries a joining FETCH.
	const JOINING_DRAFTS: [Version; 1] = [Version::Draft18];

	const INVALID_JOINING_REQUEST_ID: u64 = 0x32;

	const INVALID_RANGE: u64 = 0x11;

	/// The registry's "does not exist" value.
	const DOES_NOT_EXIST: u64 = 0x10;

	/// Yield so the broadcast registered by `serve` is visible to the lookup.
	async fn settle() {
		moq_clock::sleep(std::time::Duration::from_millis(1)).await;
	}

	/// Fill the track with finished groups 0..=`latest`, so the live edge is a group past 0
	/// and a `{0, 0}` End Location would be below the subscriber's own Start.
	fn publish_groups(h: &mut Serve, latest: u64) {
		for sequence in 0..=latest {
			let mut group = h.track.create_group(group::Info { sequence }).unwrap();
			group.write_frame(timestamp(), b"frame".as_slice()).unwrap();
			group.finish().unwrap();
		}
	}

	/// Run a relative joining FETCH with an offset of 0, returning what the peer reads off
	/// the fetch's own request stream.
	///
	/// Every stream shares one write log, so the caller passes the length it had before the
	/// fetch ran; the subscription is parked at the live edge and writes nothing meanwhile.
	async fn joining_fetch(h: &Serve, mark: usize) -> Result<Vec<u8>, Error> {
		let version = h.publisher.version;
		let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), version)
			.await
			.unwrap();
		h.publisher
			.clone()
			.run_fetch_stream(
				stream,
				ietf::Fetch {
					request_id: FETCH_ID,
					subscriber_priority: 128,
					group_order: GroupOrder::Ascending,
					fetch_type: FetchType::RelativeJoining {
						subscriber_request_id: RequestId(REQUEST_ID),
						group_offset: 0,
					},
					range_filters: false,
					fill_timeout: false,
					properties_wanted: true,
					epoch: None,
				},
			)
			.await?;

		Ok(h.log.writes.lock().unwrap()[mark..].to_vec())
	}

	/// Drive a subscription until its snapshot is available, failing if it ends first.
	async fn registered(h: &Serve, serving: impl std::future::Future<Output = Result<(), Error>>) {
		let joined = h.publisher.joins.wait(|joins| {
			if matches!(joins.get(&RequestId(REQUEST_ID)), Some(Some(_))) {
				Poll::Ready(())
			} else {
				Poll::Pending
			}
		});
		match futures::future::select(std::pin::pin!(joined), std::pin::pin!(serving)).await {
			futures::future::Either::Left(_) => {}
			futures::future::Either::Right((result, _)) => panic!("subscription ended before registering: {result:?}"),
		}
	}

	/// FETCH returns the saved multi-object prefix, including empty objects, and excludes
	/// later objects that belong to the subscription. Decode the wire fields independently.
	#[moq_clock::test]
	async fn a_joining_fetch_is_answered_with_fetch_ok() {
		const LATEST: u64 = 5;

		for version in JOINING_DRAFTS {
			let mut h = serve(version);
			publish_groups(&mut h, LATEST - 1);
			let mut group = h.track.create_group(group::Info { sequence: LATEST }).unwrap();
			let payloads: &[&[u8]] = &[b"first-object", b"", b"third-object"];
			for payload in payloads {
				group.write_frame(timestamp(), *payload).unwrap();
			}

			settle().await;

			let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), version)
				.await
				.unwrap();
			let mut serve = std::pin::pin!(
				h.publisher
					.clone()
					.run_subscribe_stream(stream, subscribe(Filter::NextObject, None))
			);

			// Drive the subscription until its snapshot is registered.
			registered(&h, serve.as_mut()).await;
			assert_eq!(
				occurrences(&h.log, b"first-object"),
				0,
				"subscription replayed the prefix"
			);
			group.write_frame(timestamp(), b"new-object".as_slice()).unwrap();
			let mark = h.log.writes.lock().unwrap().len();

			let response = joining_fetch(&h, mark).await.unwrap();

			let mut buf = bytes::Bytes::from(response);
			let id = crate::coding::decode_varint(&mut buf, version).unwrap();
			assert_eq!(id, ietf::FetchOk::ID, "{version}: not a FETCH_OK");

			let ok = crate::coding::decode_buf(&mut buf, version, |r, v| ietf::FetchOk::decode(r, v)).unwrap();
			assert!(!ok.end_of_track, "{version}: the track has not ended");
			let properties = track_properties(&track::Info::timed(), true, version, true).unwrap();
			assert_eq!(ok.properties, properties, "{version}: wrong properties");
			assert_eq!(
				ok.end_location,
				Location {
					group: LATEST,
					object: payloads.len() as u64
				},
				"{version}: wrong saved end boundary"
			);

			assert_eq!(
				crate::coding::decode_varint(&mut buf, version).unwrap(),
				FetchHeader::TYPE
			);
			assert_eq!(
				crate::coding::decode_buf(&mut buf, version, |r, v| FetchHeader::decode(r, v))
					.unwrap()
					.request_id,
				FETCH_ID
			);
			for (index, payload) in payloads.iter().enumerate() {
				let flags = if index == 0 { 0x3c } else { 0x20 };
				assert_eq!(
					crate::coding::decode_varint(&mut buf, version).unwrap(),
					flags,
					"{version}"
				);
				if index == 0 {
					assert_eq!(crate::coding::decode_varint(&mut buf, version).unwrap(), LATEST);
					assert_eq!(crate::coding::decode_varint(&mut buf, version).unwrap(), 0);
					assert_eq!(bytes::Buf::try_get_u8(&mut buf).unwrap(), 0);
				}
				let properties = crate::coding::decode_buf(&mut buf, version, |r, _| Ok(r.bytes()?.to_vec())).unwrap();
				assert!(!properties.is_empty(), "{version}: missing Timestamp");
				let size = crate::coding::decode_varint(&mut buf, version).unwrap() as usize;
				assert_eq!(size, payload.len());
				assert_eq!(buf.split_to(size).as_ref(), *payload);
			}
			assert!(buf.is_empty(), "FETCH delivered objects beyond the saved prefix");
			let mark = h.log.writes.lock().unwrap().len();
			assert!(futures::poll!(serve.as_mut()).is_pending());
			let mut tail = bytes::Bytes::from(h.log.writes.lock().unwrap()[mark..].to_vec());
			assert_eq!(
				crate::coding::decode_varint(&mut tail, version).unwrap(),
				payloads.len() as u64
			);
			assert_eq!(occurrences(&h.log, b"new-object"), 1);
			assert!(h.log.resets().is_empty(), "{version}: an answered fetch must not reset");
		}
	}

	/// Dispatch order must not depend on which request task gets polled first.
	#[moq_clock::test]
	async fn a_joining_fetch_waits_for_its_dispatched_subscription() {
		let version = Version::Draft18;
		let mut h = serve(version);
		publish_groups(&mut h, 5);
		settle().await;
		let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), version)
			.await
			.unwrap();
		let mut data = Vec::new();
		subscribe(Filter::NextObject, None)
			.encode_msg(&mut Encoder::new(&mut data, version.into()), version)
			.unwrap();
		let mut serving = h
			.publisher
			.handle_stream(ietf::Subscribe::ID, ietf::Body(bytes::Bytes::from(data)), stream, false)
			.unwrap();
		let mut fetching = Box::pin(joining_fetch(&h, 0));
		assert!(
			futures::poll!(fetching.as_mut()).is_pending(),
			"FETCH must wait for the dispatched subscription"
		);
		match futures::future::select(&mut fetching, &mut serving).await {
			futures::future::Either::Left((response, _)) => {
				response.unwrap();
			}
			futures::future::Either::Right(_) => panic!("subscription ended before FETCH"),
		}
		assert_eq!(occurrences(&h.log, b"frame"), 1, "FETCH delivers the saved prefix");
		assert!(h.log.resets().is_empty());
	}

	#[moq_clock::test]
	async fn a_joining_fetch_waits_for_a_reordered_subscription() {
		let version = Version::Draft18;
		let mut h = serve(version);
		publish_groups(&mut h, 5);
		settle().await;
		let mut fetching = Box::pin(joining_fetch(&h, 0));
		assert!(
			futures::poll!(fetching.as_mut()).is_pending(),
			"allow SUBSCRIBE to arrive on its stream"
		);
		let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), version)
			.await
			.unwrap();
		let mut data = Vec::new();
		subscribe(Filter::NextObject, None)
			.encode_msg(&mut Encoder::new(&mut data, version.into()), version)
			.unwrap();
		let mut serving = h
			.publisher
			.handle_stream(ietf::Subscribe::ID, ietf::Body(bytes::Bytes::from(data)), stream, false)
			.unwrap();
		match futures::future::select(&mut fetching, &mut serving).await {
			futures::future::Either::Left((response, _)) => {
				response.unwrap();
			}
			futures::future::Either::Right(_) => panic!("subscription ended before FETCH"),
		}
		assert_eq!(occurrences(&h.log, b"frame"), 1);
		assert!(h.log.resets().is_empty());
	}

	#[moq_clock::test]
	async fn a_joining_fetch_wakes_when_its_pending_subscription_is_dropped() {
		for version in JOINING_DRAFTS {
			let h = serve(version);
			let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), version)
				.await
				.unwrap();
			let mut data = Vec::new();
			subscribe(Filter::NextObject, None)
				.encode_msg(&mut Encoder::new(&mut data, version.into()), version)
				.unwrap();
			let serving = h
				.publisher
				.handle_stream(ietf::Subscribe::ID, ietf::Body(bytes::Bytes::from(data)), stream, false)
				.unwrap();
			let mut fetching = Box::pin(joining_fetch(&h, 0));
			assert!(futures::poll!(fetching.as_mut()).is_pending());
			drop(serving);
			let mut response = bytes::Bytes::from(fetching.await.unwrap());
			let id = crate::coding::decode_varint(&mut response, version).unwrap();
			assert_eq!(id, ietf::RequestError::ID);
			assert_eq!(
				crate::coding::decode_buf(&mut response, version, |r, v| Ok(
					ietf::RequestError::decode(r, v)?.error_code
				))
				.unwrap(),
				INVALID_JOINING_REQUEST_ID
			);
			assert!(response.is_empty());
			assert!(h.publisher.joins.read().is_empty());
		}
	}

	#[moq_clock::test]
	async fn a_joining_fetch_times_out_an_unresolved_subscription() {
		let version = Version::Draft18;
		let h = serve(version);
		let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), version)
			.await
			.unwrap();
		let _serving = h
			.publisher
			.clone()
			.run_subscribe_stream(stream, subscribe(Filter::NextObject, None));
		let mut response = bytes::Bytes::from(joining_fetch(&h, 0).await.unwrap());
		assert_eq!(
			crate::coding::decode_varint(&mut response, version).unwrap(),
			ietf::RequestError::ID
		);
		assert_eq!(
			crate::coding::decode_buf(&mut response, version, |r, v| Ok(
				ietf::RequestError::decode(r, v)?.error_code
			))
			.unwrap(),
			0x2
		);
		assert!(response.is_empty());
		assert!(h.log.resets().is_empty());
	}

	/// The group never held the promised prefix, so the fetch itself fails and the
	/// refusal carries that error: refused before FETCH_OK, and never reset.
	#[moq_clock::test]
	async fn a_joining_fetch_refuses_a_missing_prefix_before_fetch_ok() {
		let version = Version::Draft18;
		let h = serve(version);
		let mut group = h.track.create_group(group::Info { sequence: 5 }).unwrap();
		group.start_at(1).unwrap();
		group.write_frame(timestamp(), b"frame".as_slice()).unwrap();
		group.finish().unwrap();
		settle().await;
		let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), version)
			.await
			.unwrap();
		let mut serving = Box::pin(
			h.publisher
				.clone()
				.run_subscribe_stream(stream, subscribe(Filter::NextObject, None)),
		);
		registered(&h, serving.as_mut()).await;
		let mark = h.log.writes.lock().unwrap().len();
		let mut response = bytes::Bytes::from(joining_fetch(&h, mark).await.expect("refuse before opening data"));
		assert_eq!(
			crate::coding::decode_varint(&mut response, version).unwrap(),
			ietf::RequestError::ID
		);
		assert_eq!(
			crate::coding::decode_buf(&mut response, version, |r, v| Ok(
				ietf::RequestError::decode(r, v)?.error_code
			))
			.unwrap(),
			DOES_NOT_EXIST
		);
		assert!(response.is_empty());
		assert!(h.log.resets().is_empty());
	}

	/// A joining FETCH whose prefix group was evicted is refused as missing before FETCH_OK.
	#[moq_clock::test]
	async fn a_joining_fetch_refuses_a_prefix_group_it_lost() {
		let version = Version::Draft18;
		let h = serve(version);
		let mut prefix = h.track.create_group(group::Info { sequence: 5 }).unwrap();
		prefix.write_frame(timestamp(), b"frame".as_slice()).unwrap();
		settle().await;
		let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), version)
			.await
			.unwrap();
		let mut serving = Box::pin(
			h.publisher
				.clone()
				.run_subscribe_stream(stream, subscribe(Filter::NextObject, None)),
		);
		registered(&h, serving.as_mut()).await;
		prefix.abort(Error::Evicted).unwrap();

		let mark = h.log.writes.lock().unwrap().len();
		let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), version)
			.await
			.unwrap();
		h.publisher
			.clone()
			.run_fetch_stream(
				stream,
				ietf::Fetch {
					request_id: FETCH_ID,
					subscriber_priority: 128,
					group_order: GroupOrder::Ascending,
					fetch_type: FetchType::RelativeJoining {
						subscriber_request_id: RequestId(REQUEST_ID),
						group_offset: 0,
					},
					range_filters: false,
					fill_timeout: false,
					properties_wanted: true,
					epoch: None,
				},
			)
			.await
			.unwrap();
		let mut response = bytes::Bytes::from(h.log.writes.lock().unwrap()[mark..].to_vec());
		assert_eq!(
			crate::coding::decode_varint(&mut response, version).unwrap(),
			ietf::RequestError::ID
		);
		assert_eq!(
			crate::coding::decode_buf(&mut response, version, |r, v| Ok(
				ietf::RequestError::decode(r, v)?.error_code
			))
			.unwrap(),
			DOES_NOT_EXIST
		);
		assert!(response.is_empty());
		assert!(h.log.resets().is_empty());
	}

	/// A joining FETCH that arrives after its subscription ended has no live edge to name, so
	/// it is refused rather than answered from a group that is no longer the edge of anything.
	#[moq_clock::test]
	async fn a_joining_fetch_without_its_subscription_is_refused() {
		for version in JOINING_DRAFTS {
			let mut h = serve(version);
			publish_groups(&mut h, 5);
			run_live(&mut h, subscribe(Filter::NextObject, None)).await;
			let mark = h.log.writes.lock().unwrap().len();
			let mut buf = bytes::Bytes::from(joining_fetch(&h, mark).await.unwrap());
			let id = crate::coding::decode_varint(&mut buf, version).unwrap();
			assert_eq!(id, ietf::RequestError::ID);
			assert_eq!(
				crate::coding::decode_buf(&mut buf, version, |r, v| Ok(
					ietf::RequestError::decode(r, v)?.error_code
				))
				.unwrap(),
				INVALID_JOINING_REQUEST_ID
			);
			assert!(buf.is_empty());
			assert!(h.log.resets().is_empty());
		}
	}

	#[moq_clock::test]
	async fn a_joining_fetch_rejects_unsupported_filters() {
		for version in JOINING_DRAFTS {
			for filter in [Filter::Unfiltered, Filter::Relative(0)] {
				let mut h = serve(version);
				publish_groups(&mut h, 5);
				settle().await;
				let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), version)
					.await
					.unwrap();
				let mut serving = std::pin::pin!(
					h.publisher
						.clone()
						.run_subscribe_stream(stream, subscribe(filter, None))
				);
				registered(&h, serving.as_mut()).await;
				let mark = h.log.writes.lock().unwrap().len();
				let result = joining_fetch(&h, mark).await;
				let mut buf = bytes::Bytes::from(result.unwrap());
				assert_eq!(
					crate::coding::decode_varint(&mut buf, version).unwrap(),
					ietf::RequestError::ID
				);
				assert_eq!(
					crate::coding::decode_buf(&mut buf, version, |r, v| Ok(
						ietf::RequestError::decode(r, v)?.error_code
					))
					.unwrap(),
					0x3
				);
			}
		}
	}

	/// A subscription that started on an empty track has no prefix to serve, so the
	/// fetch range is invalid on every draft that carries joining FETCH.
	#[moq_clock::test]
	async fn a_joining_fetch_rejects_an_empty_snapshot() {
		for version in JOINING_DRAFTS {
			let h = serve(version);
			settle().await;
			let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), version)
				.await
				.unwrap();
			let mut serving = std::pin::pin!(
				h.publisher
					.clone()
					.run_subscribe_stream(stream, subscribe(Filter::NextObject, None))
			);
			registered(&h, serving.as_mut()).await;
			let mark = h.log.writes.lock().unwrap().len();
			let mut buf = bytes::Bytes::from(joining_fetch(&h, mark).await.unwrap());
			let id = crate::coding::decode_varint(&mut buf, version).unwrap();
			assert_eq!(id, ietf::RequestError::ID);
			assert_eq!(
				crate::coding::decode_buf(&mut buf, version, |r, v| Ok(
					ietf::RequestError::decode(r, v)?.error_code
				))
				.unwrap(),
				INVALID_RANGE
			);
			assert!(buf.is_empty());
			assert!(h.log.resets().is_empty());
		}
	}

	/// Every draft, each carrying a standalone FETCH or draft-22's filtered one.
	const FETCH_DRAFTS: [Version; 3] = [Version::Draft18, Version::Draft20, Version::Draft22];

	/// A bounded FETCH_OK's End Location: one past `last` before draft 22,
	/// and the object itself from then on. Whole-group answers use Object 0 before draft 22.
	fn end_location(version: Version, group: u64, last: u64) -> Location {
		let object = match Filter::is_draft20(version) {
			true => last,
			false => last + 1,
		};
		Location { group, object }
	}

	/// Groups `0..count`, each holding `g-0` and `g-1`, skipping `hole`.
	fn publish_pairs(h: &mut Serve, count: u64, hole: Option<u64>) {
		for sequence in (0..count).filter(|sequence| Some(*sequence) != hole) {
			let mut group = h.track.create_group(group::Info { sequence }).unwrap();
			for object in 0..2 {
				group
					.write_frame(timestamp(), format!("{sequence}-{object}").into_bytes())
					.unwrap();
			}
			group.finish().unwrap();
		}
	}

	/// Run a standalone FETCH of `room/video`, returning what the peer reads back.
	///
	/// `end` is spelled as draft 18 does: the last object plus one, or 0 for the whole
	/// End Group. From draft 22 the same range goes out as an inclusive LOCATION_FILTER.
	async fn standalone_fetch(h: &Serve, start: Location, end: Location, group_order: GroupOrder) -> bytes::Bytes {
		let fetch_type = fetch_range(h.publisher.version, "video", start, end);
		run_fetch(h, fetch_type, group_order, true).await
	}

	#[moq_clock::test]
	async fn a_cached_standalone_fetch_yields_while_writing() {
		for version in FETCH_DRAFTS {
			let h = serve(version);
			let mut group = h.track.create_group(group::Info { sequence: 0 }).unwrap();
			for _ in 0..256 {
				group.write_frame(timestamp(), vec![7; 1024]).unwrap();
			}
			group.finish().unwrap();
			settle().await;

			let done = std::rc::Rc::new(std::cell::Cell::new(false));
			let saw_payload = std::rc::Rc::new(std::cell::Cell::new(false));
			let sibling = moq_clock::sim::spawn({
				let done = done.clone();
				let saw_payload = saw_payload.clone();
				let log = h.log.clone();
				async move {
					while !done.get() {
						if log.writes.lock().unwrap().len() > 1024 {
							saw_payload.set(true);
						}
						moq_clock::sim::yield_now().await;
					}
				}
			});
			let wire = standalone_fetch(
				&h,
				Location { group: 0, object: 0 },
				Location { group: 0, object: 256 },
				GroupOrder::Ascending,
			)
			.await;
			done.set(true);
			sibling.await.unwrap();
			assert!(wire.len() > 256 * 1024);
			assert!(
				saw_payload.get(),
				"{version}: writing the cached group starved its sibling"
			);
		}
	}

	/// A standalone FETCH of `room/<track>`, spelled as [`standalone_fetch`] describes.
	fn fetch_range(version: Version, track: &'static str, start: Location, end: Location) -> FetchType<'static> {
		let namespace = crate::Path::from_static("room");
		let track = track.into();
		match Filter::is_draft20(version) {
			true => FetchType::Filtered {
				namespace,
				track,
				filter: Filter::Absolute {
					start,
					end: Some(EndLocation {
						group: end.group,
						object: end.object.checked_sub(1),
					}),
				},
			},
			false => FetchType::Standalone {
				namespace,
				track,
				start,
				end,
			},
		}
	}

	/// Run a FETCH, returning what the peer reads back.
	async fn run_fetch(
		h: &Serve,
		fetch_type: FetchType<'static>,
		group_order: GroupOrder,
		properties_wanted: bool,
	) -> bytes::Bytes {
		let version = h.publisher.version;
		let mark = h.log.writes.lock().unwrap().len();
		let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), version)
			.await
			.unwrap();
		h.publisher
			.clone()
			.run_fetch_stream(
				stream,
				ietf::Fetch {
					request_id: FETCH_ID,
					subscriber_priority: 128,
					group_order,
					fetch_type,
					range_filters: false,
					fill_timeout: true,
					properties_wanted,
					epoch: None,
				},
			)
			.await
			.unwrap();
		bytes::Bytes::from(h.log.writes.lock().unwrap()[mark..].to_vec())
	}

	/// Decode a FETCH_OK and the ascending fetch stream after it, as `(group, object, payload)`.
	fn fetch_answer(wire: bytes::Bytes, version: Version) -> (ietf::FetchOk, Vec<(u64, u64, String)>) {
		fetch_answer_in(wire, version, GroupOrder::Ascending)
	}

	/// Decode a FETCH_OK and the fetch stream after it, its groups in `order`.
	fn fetch_answer_in(
		wire: bytes::Bytes,
		version: Version,
		order: GroupOrder,
	) -> (ietf::FetchOk, Vec<(u64, u64, String)>) {
		let mut buf = Decoder::new(&wire, version.into());
		assert_eq!(buf.varint().unwrap(), ietf::FetchOk::ID, "{version}: not a FETCH_OK");
		let ok = ietf::FetchOk::decode(&mut buf, version).unwrap();
		assert_eq!(buf.varint().unwrap(), FetchHeader::TYPE);
		assert_eq!(FetchHeader::decode(&mut buf, version).unwrap().request_id, FETCH_ID);

		let mut objects = Vec::new();
		let mut prior: Option<(u64, u64)> = None;
		while !buf.is_empty() {
			let (group, object) = {
				let ietf::FetchObject::Object { group, object, .. } =
					ietf::FetchObject::decode(&mut buf, version).unwrap()
				else {
					panic!("{version}: unexpected End of Range");
				};
				match (prior, group) {
					(None, group) => (group.unwrap(), object.unwrap()),
					// No Group ID: the same group, and an absent Object ID Delta is one.
					(Some((group, prior)), None) => (group, prior + object.unwrap_or(1)),
					(Some((prior, _)), Some(group)) => {
						let group = match (version, order) {
							(_, GroupOrder::Descending) => prior - group - 1,
							_ => prior + group + 1,
						};
						(group, object.unwrap())
					}
				}
			};
			let size = buf.varint().unwrap() as usize;
			let payload = String::from_utf8(buf.slice(size).unwrap().to_vec()).unwrap();
			objects.push((group, object, payload));
			prior = Some((group, object));
		}
		(ok, objects)
	}

	/// The `(group, object, payload)` a range of two-frame groups holds.
	fn pairs(groups: impl IntoIterator<Item = u64>) -> Vec<(u64, u64, String)> {
		groups
			.into_iter()
			.flat_map(|group| (0..2).map(move |object| (group, object, format!("{group}-{object}"))))
			.collect()
	}

	/// A whole group is answered on one fetch stream.
	#[moq_clock::test]
	async fn a_standalone_fetch_serves_one_whole_group() {
		for version in FETCH_DRAFTS {
			let mut h = serve(version);
			publish_pairs(&mut h, 5, None);
			settle().await;

			// An End Object of 0 asks for the whole End Group.
			let buf = standalone_fetch(
				&h,
				Location { group: 2, object: 0 },
				Location { group: 2, object: 0 },
				GroupOrder::Ascending,
			)
			.await;
			let (ok, objects) = fetch_answer(buf, version);
			let end = match Filter::is_draft20(version) {
				true => Location { group: 2, object: 1 },
				false => Location { group: 2, object: 0 },
			};
			assert_eq!(ok.end_location, end, "{version}");
			assert!(!ok.end_of_track, "{version}");
			assert_eq!(objects, pairs([2]), "{version}");
			assert!(h.log.resets().is_empty(), "{version}");
		}
	}

	/// FETCH_OK carries the properties SUBSCRIBE_OK would, as far as each draft has room for
	/// them, and INCLUDE_PROPERTIES=0 empties the block. The objects carry Timestamps only
	/// when that block declares their units.
	#[moq_clock::test]
	async fn fetch_ok_carries_the_track_properties() {
		let info = track::Info {
			max_age: Some(Duration::from_secs(5)),
			priority: 72,
			timescale: Some(crate::Timescale::MILLI),
			..Default::default()
		};
		let stamp = crate::Timestamp::from_millis(123_456).unwrap();

		for version in FETCH_DRAFTS {
			// Only draft-22's FETCH can carry the opt-out.
			let opt_out = Filter::is_draft20(version).then_some(false);
			for wanted in [Some(true), opt_out].into_iter().flatten() {
				let h = serve(version);
				let track = h._broadcast.create_track("timed", info.clone()).unwrap();
				let mut group = track.create_group(group::Info { sequence: 0 }).unwrap();
				group.write_frame(stamp, b"stamped".as_slice()).unwrap();
				group.finish().unwrap();
				settle().await;

				let range = fetch_range(
					version,
					"timed",
					Location { group: 0, object: 0 },
					Location { group: 0, object: 0 },
				);
				let (ok, objects) = fetch_answer(run_fetch(&h, range, GroupOrder::Ascending, wanted).await, version);
				assert_eq!(objects.len(), 1, "{version}");

				let expected = track_properties(&info, wanted, version, true).unwrap();
				assert_eq!(ok.properties, expected, "{version} wanted={wanted}");

				let mut properties = Vec::new();
				ietf::encode_object_time(
					&mut Encoder::new(&mut properties, version.into()),
					stamp,
					info.timescale.expect("a timed track"),
				)
				.unwrap();
				assert_eq!(
					occurrences(&h.log, &properties),
					usize::from(wanted),
					"{version} wanted={wanted}: stamped only with a TIMESCALE"
				);
			}
		}
	}

	/// The Track Properties block a publisher upstream of a relay sent on `version`: an even
	/// property the draft defines but moq-net does not implement (OBJECT_DELIVERY_TIMEOUT),
	/// and an odd one from the application range.
	fn unknown_properties(version: Version) -> ietf::Properties {
		let mut block = Vec::new();
		let mut w = Encoder::new(&mut block, version.into());
		w.varint(0x02).unwrap();
		w.varint(5000).unwrap();
		w.varint(0x3801 - 0x02).unwrap();
		w.bytes(b"opaque").unwrap();
		let properties = crate::coding::decode_buf(&mut bytes::Bytes::from(block), version, |r, v| {
			ietf::Properties::decode(r, v)
		})
		.unwrap();
		assert_eq!(properties.unknown.len(), 2, "{version}");
		properties
	}

	/// Every answer describing the track forwards the properties moq-net does not interpret
	/// unchanged, beside the ones it does, on every draft with a Track Properties block.
	#[moq_clock::test]
	async fn unknown_track_properties_are_forwarded() {
		for version in [Version::Draft22] {
			let upstream = unknown_properties(version);
			let info = track::Info {
				properties: Some(Arc::new(crate::model::object::TrackProperties {
					version,
					values: upstream.unknown.clone(),
				})),
				..track::Info::timed()
			};

			let mut h = serve_track(version, info.clone());
			run_live(&mut h, subscribe(Filter::NextObject, None)).await;
			assert_eq!(
				subscribe_ok(&h.log, version).properties.unknown,
				upstream.unknown,
				"{version}: SUBSCRIBE_OK"
			);

			let h = serve_track(version, info.clone());
			let mut group = h.track.create_group(group::Info { sequence: 0 }).unwrap();
			group.write_frame(timestamp(), b"a".as_slice()).unwrap();
			group.finish().unwrap();
			settle().await;
			let range = fetch_range(
				version,
				"video",
				Location { group: 0, object: 0 },
				Location { group: 0, object: 0 },
			);
			let (ok, _) = fetch_answer(run_fetch(&h, range, GroupOrder::Ascending, true).await, version);
			assert_eq!(ok.properties.unknown, upstream.unknown, "{version}: FETCH_OK");

			let h = serve_track(version, info);
			let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), version)
				.await
				.unwrap();
			h.publisher
				.clone()
				.handle_stream(
					ietf::TrackStatus::ID,
					ietf::Body(track_status("room", true, version).into()),
					stream,
					false,
				)
				.unwrap()
				.await;
			let wire = h.log.writes.lock().unwrap().clone();
			let mut buf = Decoder::new(&wire, version.into());
			assert_eq!(buf.varint().unwrap(), ietf::TrackStatusOk::ID);
			let ok = ietf::TrackStatusOk::decode(&mut buf, version).unwrap();
			assert_eq!(ok.properties.unknown, upstream.unknown, "{version}: TRACK_STATUS_OK");
		}
	}

	/// Immutable Properties keep the bytes their publisher sealed.
	#[moq_clock::test]
	async fn immutable_track_properties_keep_their_encoding() {
		// Sealed: OBJECT_DELIVERY_TIMEOUT = 42 and DYNAMIC_GROUPS = 1.
		let block = [0x0b, 4, 0x02, 42, 0x2e, 1];
		let upstream =
			crate::coding::decode_buf(&mut bytes::Bytes::copy_from_slice(&block), Version::Draft22, |r, v| {
				ietf::Properties::decode(r, v)
			})
			.unwrap();
		assert!(upstream.dynamic_groups);
		let info = track::Info {
			refresh: Some(0),
			properties: Some(Arc::new(crate::model::object::TrackProperties {
				version: Version::Draft22,
				values: upstream.unknown.clone(),
			})),
			..track::Info::timed()
		};

		let mut h = serve_track(Version::Draft22, info.clone());
		run_live(&mut h, subscribe(Filter::NextObject, None)).await;
		// DYNAMIC_GROUPS stays sealed: restating it as mutable too would fail this decode.
		let ok = subscribe_ok(&h.log, Version::Draft22);
		assert!(ok.properties.dynamic_groups);
		assert_eq!(ok.properties.unknown, upstream.unknown);
		// Its key is delta-encoded against TIMESCALE now, but the sealed bytes are verbatim.
		assert_eq!(occurrences(&h.log, &block[1..]), 1);
	}

	/// The info a relay behind a moq-lite hop with the `ietf` extension learns of a track
	/// published with `info`.
	async fn across_a_hop(info: track::Info) -> track::Info {
		let upstream = crate::origin::Config::new(crate::Hop::new(2).unwrap()).produce();
		let broadcast = upstream.publish("room", crate::origin::Route::default()).unwrap();
		let _track = broadcast.create_track("video", info).unwrap();
		let (downstream, _sessions) = crate::lite::ietf::test_hop(&upstream, "room", [true, true]).await;
		let broadcast = downstream.consume().request("room", None).await.unwrap();
		broadcast.track("video").unwrap().info().await.unwrap()
	}

	/// Draft 22 assigns 0x06 to SUBGROUP_DELIVERY_TIMEOUT, so a track property 0x06 crosses
	/// that boundary as an object property does: dropped when mutable, refused when sealed.
	/// Drafts 18 and 22 serialize alike, so across a relay hop only the track's carried
	/// ingress draft says which meaning it has.
	#[moq_clock::test]
	async fn track_property_0x06_does_not_cross_to_draft_22() {
		let upstream = |block: &[u8]| {
			let properties =
				crate::coding::decode_buf(&mut bytes::Bytes::copy_from_slice(block), Version::Draft18, |r, v| {
					ietf::Properties::decode(r, v)
				})
				.unwrap();
			track::Info {
				properties: Some(Arc::new(crate::model::object::TrackProperties {
					version: Version::Draft18,
					values: properties.unknown,
				})),
				..Default::default()
			}
		};
		let ids = |properties: ietf::Properties| properties.unknown.iter().map(|p| p.id).collect::<Vec<_>>();

		// 0x06 = 42, 0x3801 = "hi"
		let mut block = Vec::new();
		let mut w = Encoder::new(&mut block, Version::Draft18.into());
		w.varint(0x06).unwrap();
		w.varint(42).unwrap();
		w.varint(0x3801 - 0x06).unwrap();
		w.bytes(b"hi").unwrap();
		let sealed = upstream(&[0x0b, 2, 0x06, 42]);
		for hop in [false, true] {
			let (mutable, sealed) = match hop {
				false => (upstream(&block), sealed.clone()),
				true => (across_a_hop(upstream(&block)).await, across_a_hop(sealed.clone()).await),
			};
			assert_eq!(mutable.ietf(), Some(Version::Draft18), "hop={hop}");
			assert_eq!(
				ids(track_properties(&mutable, true, Version::Draft18, true).unwrap()),
				[0x06, 0x3801],
				"hop={hop}"
			);
			assert_eq!(
				ids(track_properties(&mutable, true, Version::Draft22, true).unwrap()),
				[0x3801],
				"hop={hop}"
			);

			assert!(
				track_properties(&sealed, true, Version::Draft18, true).is_ok(),
				"hop={hop}"
			);
			assert!(
				matches!(
					track_properties(&sealed, true, Version::Draft22, true),
					Err(Error::Unsupported)
				),
				"hop={hop}"
			);
		}
	}

	/// A range ending inside its group stops at its End Object, which draft 22 counts
	/// inclusively and older drafts count as one past.
	#[moq_clock::test]
	async fn a_standalone_fetch_stops_at_its_end_object() {
		for version in FETCH_DRAFTS {
			let mut h = serve(version);
			publish_pairs(&mut h, 5, None);
			settle().await;

			let buf = standalone_fetch(
				&h,
				Location { group: 2, object: 0 },
				Location { group: 2, object: 1 },
				GroupOrder::Ascending,
			)
			.await;
			let (ok, objects) = fetch_answer(buf, version);
			assert_eq!(ok.end_location, end_location(version, 2, 0), "{version}");
			assert!(!ok.end_of_track, "{version}");
			assert_eq!(objects, pairs([2])[..1].to_vec(), "{version}");
		}
	}

	/// A group holding no objects at or past the start is answered empty, on draft 22 with
	/// an End Location covering the range asked for (section 9.11). The whole group covers
	/// at least its start.
	#[moq_clock::test]
	async fn a_standalone_fetch_past_the_last_object_is_empty() {
		for version in FETCH_DRAFTS {
			for (end, covered) in [(0, 2), (6, 5)] {
				let mut h = serve(version);
				publish_pairs(&mut h, 5, None);
				settle().await;

				let buf = standalone_fetch(
					&h,
					Location { group: 2, object: 2 },
					Location { group: 2, object: end },
					GroupOrder::Ascending,
				)
				.await;
				let (ok, objects) = fetch_answer(buf, version);
				assert_eq!(objects, Vec::new(), "{version} end={end}");
				if Filter::is_draft20(version) {
					assert_eq!(
						ok.end_location,
						Location {
							group: 2,
							object: covered
						},
						"{version}"
					);
				}
			}
		}
	}

	/// How much a draft-22 publisher knows of Largest Object.
	#[derive(Clone, Copy, Debug)]
	enum Largest {
		/// The track is finished: its last object.
		Finished,
		/// A live feed: the newest object, here behind a newer group with no objects yet.
		Live,
		/// A relay's copy with no upstream subscription: unknown.
		Idle,
		/// An idle copy that learned a later end of track: still unknown.
		IdleEnded,
		/// An idle copy whose track ends after its newest group: that group's last object.
		IdleFinished,
		/// A live feed whose newest group is a datagram the cache cannot read: unknown.
		Datagram,
		/// A live feed whose newest group was aborted mid-write: unknown.
		Aborted,
	}

	/// Groups 0 to 4 of two objects each, with Largest Object known as `largest` says.
	fn publish_largest(h: &mut Serve, largest: Largest) -> Option<group::Producer> {
		publish_pairs(h, 5, None);
		match largest {
			Largest::Finished => {
				h.track.finish().unwrap();
				None
			}
			Largest::Live => Some(h.track.create_group(group::Info { sequence: 5 }).unwrap()),
			Largest::Idle => {
				h.track.set_idle();
				None
			}
			Largest::IdleEnded => {
				h.track.set_idle();
				h.track.finish_at(track::Position::group(10), None).unwrap();
				None
			}
			Largest::IdleFinished => {
				h.track.set_idle();
				h.track.finish().unwrap();
				None
			}
			Largest::Datagram => {
				h.track.append_datagram(timestamp(), b"d".as_slice()).unwrap();
				None
			}
			Largest::Aborted => {
				let mut newer = h.track.create_group(group::Info { sequence: 5 }).unwrap();
				newer.write_frame(timestamp(), b"5-0".to_vec()).unwrap();
				newer.abort(Error::Cancel).unwrap();
				None
			}
		}
	}

	/// On draft 22, a start past Largest Object is refused INVALID_RANGE (section 9.11),
	/// wherever the publisher knows it. Where it does not, it answers empty. A start beyond
	/// the final group's last object still finds the track's end there.
	#[moq_clock::test]
	async fn a_draft22_fetch_past_the_largest_object_is_refused() {
		let version = Version::Draft22;
		for largest in [
			Largest::Finished,
			Largest::Live,
			Largest::Idle,
			Largest::IdleEnded,
			Largest::IdleFinished,
			Largest::Datagram,
			Largest::Aborted,
		] {
			for start in [2, 3] {
				let mut h = serve(version);
				let _newer = publish_largest(&mut h, largest);
				settle().await;

				let buf = standalone_fetch(
					&h,
					Location {
						group: 4,
						object: start,
					},
					Location { group: 4, object: 6 },
					GroupOrder::Ascending,
				)
				.await;
				match largest {
					Largest::Finished | Largest::Live | Largest::IdleFinished => assert_eq!(
						fetch_refusal(buf, version),
						INVALID_RANGE,
						"{version} {largest:?} start={start}"
					),
					_ => {
						let (ok, objects) = fetch_answer(buf, version);
						assert_eq!(
							ok.end_location,
							Location { group: 4, object: 5 },
							"{version} {largest:?} start={start}"
						);
						assert_eq!(objects, Vec::new(), "{version} {largest:?} start={start}");
					}
				}
			}
		}
	}

	/// On a live feed whose newest group finished with no objects, Largest Object sits in the
	/// group before it, so a draft-22 FETCH of the empty group starts past it.
	#[moq_clock::test]
	async fn a_draft22_fetch_of_an_empty_newest_group_is_refused() {
		let version = Version::Draft22;
		let mut h = serve(version);
		publish_pairs(&mut h, 5, None);
		let mut empty = h.track.create_group(group::Info { sequence: 5 }).unwrap();
		empty.finish().unwrap();
		settle().await;

		let buf = standalone_fetch(
			&h,
			Location { group: 5, object: 0 },
			Location { group: 5, object: 4 },
			GroupOrder::Ascending,
		)
		.await;
		assert_eq!(fetch_refusal(buf, version), INVALID_RANGE, "{version}");
	}

	/// On draft 22, an End Object past a finished group's last object is still the End
	/// Location reported (section 10.14), even in the group holding Largest Object, unless
	/// the track ends there.
	#[moq_clock::test]
	async fn a_draft22_fetch_past_a_group_end_reports_the_requested_end() {
		let version = Version::Draft22;
		for (group, largest, reported) in [
			(2, Largest::Live, 7),
			(4, Largest::Live, 7),
			(4, Largest::Finished, 1),
			(4, Largest::Idle, 7),
			(4, Largest::IdleEnded, 7),
			(4, Largest::Datagram, 7),
			(4, Largest::Aborted, 7),
		] {
			let mut h = serve(version);
			let _newer = publish_largest(&mut h, largest);
			settle().await;

			let buf = standalone_fetch(
				&h,
				Location { group, object: 0 },
				Location { group, object: 8 },
				GroupOrder::Ascending,
			)
			.await;
			let (ok, objects) = fetch_answer(buf, version);
			assert_eq!(
				ok.end_location,
				Location {
					group,
					object: reported
				},
				"{version} group {group} {largest:?}"
			);
			assert_eq!(ok.end_of_track, matches!(largest, Largest::Finished), "{version}");
			assert_eq!(objects, pairs([group]), "{version}");
		}
	}

	/// A track whose cache only ever answered a fetch has no live feed, so its newest
	/// object is not Largest Object: a draft-22 FETCH past it echoes the requested end.
	#[moq_clock::test]
	async fn a_draft22_fetch_of_a_fetch_only_cache_is_not_capped() {
		let version = Version::Draft22;
		let h = serve(version);
		let request = h._broadcast.reserve_track("cam").unwrap();
		let dynamic = request.requests();
		let fetching = request.consume().fetch_group(4, None);
		let mut fetched = dynamic.requested_fill_group().await.unwrap().accept(None).unwrap();
		for object in 0..2 {
			fetched.write_frame(None, format!("4-{object}").into_bytes()).unwrap();
		}
		fetched.finish().unwrap();
		fetching.await.unwrap();

		let fetch_type = fetch_range(
			version,
			"cam",
			Location { group: 4, object: 2 },
			Location { group: 4, object: 6 },
		);
		let buf = run_fetch(&h, fetch_type, GroupOrder::Ascending, true).await;
		let (ok, objects) = fetch_answer(buf, version);
		assert_eq!(ok.end_location, Location { group: 4, object: 5 }, "{version}");
		assert!(!ok.end_of_track, "{version}");
		assert_eq!(objects, Vec::new(), "{version}");
	}

	/// The last group of a finished track ends the response at its last object, with End
	/// of Track set.
	#[moq_clock::test]
	async fn a_standalone_fetch_of_the_last_group_reports_the_end_of_track() {
		for version in FETCH_DRAFTS {
			let mut h = serve(version);
			publish_pairs(&mut h, 5, None);
			h.track.finish().unwrap();
			settle().await;

			let buf = standalone_fetch(
				&h,
				Location { group: 4, object: 1 },
				Location { group: 4, object: 0 },
				GroupOrder::Any,
			)
			.await;
			let (ok, objects) = fetch_answer(buf, version);
			let end = match Filter::is_draft20(version) {
				true => end_location(version, 4, 1),
				false => Location { group: 4, object: 0 },
			};
			assert_eq!(ok.end_location, end, "{version}");
			assert!(ok.end_of_track, "{version}");
			assert_eq!(objects, pairs([4])[1..].to_vec(), "{version}");
		}
	}

	/// A bounded range ending at the last object still reports the track's end; a
	/// shorter prefix must not claim that the track ends there.
	#[moq_clock::test]
	async fn a_fetch_ending_at_the_final_object_reports_the_end_of_track() {
		for version in FETCH_DRAFTS {
			for end in [1, 2] {
				let mut h = serve(version);
				publish_pairs(&mut h, 5, None);
				h.track.finish().unwrap();
				settle().await;

				let buf = standalone_fetch(
					&h,
					Location { group: 4, object: 0 },
					Location { group: 4, object: end },
					GroupOrder::Ascending,
				)
				.await;
				let (ok, objects) = fetch_answer(buf, version);
				assert_eq!(ok.end_location, end_location(version, 4, end - 1), "{version}");
				assert_eq!(ok.end_of_track, end == 2, "{version}: exclusive end {end}");
				assert_eq!(objects, pairs([4])[..end as usize].to_vec(), "{version}");
			}
		}
	}

	/// A draft-22 filter bounded by Largest Object resolves against it, starting past it only
	/// for Next Object. An idle copy resolves from its upstream report, and refuses
	/// a relative filter when it has not learned that report.
	#[moq_clock::test]
	async fn a_draft22_fetch_relative_to_largest_object_resolves_against_it() {
		for version in [Version::Draft22] {
			for (filter, groups) in [
				(Filter::Unfiltered, Some(vec![0, 1, 2])),
				(Filter::Relative(1), Some(vec![2])),
				(Filter::NextObject, None),
				(Filter::Relative(2), Some(vec![1, 2])),
				(
					Filter::Absolute {
						start: Location { group: 2, object: 0 },
						end: None,
					},
					Some(vec![2]),
				),
			] {
				for (idle, reported) in [(false, false), (true, false), (true, true)] {
					let mut h = serve(version);
					publish_pairs(&mut h, 3, None);
					if idle {
						h.track.set_idle();
						if reported {
							h.track.report_object(2, 1);
						}
					}
					settle().await;

					let fetch_type = FetchType::Filtered {
						namespace: crate::Path::from_static("room"),
						track: "video".into(),
						filter,
					};
					let buf = run_fetch(&h, fetch_type, GroupOrder::Ascending, true).await;
					match (&groups, idle && !reported) {
						(_, true) => assert_eq!(fetch_refusal(buf, version), 0x3, "{version}: {filter:?}"),
						(None, false) => {
							assert_eq!(fetch_refusal(buf, version), INVALID_RANGE, "{version}: {filter:?}")
						}
						(Some(groups), false) if groups.len() > 1 => {
							assert_eq!(fetch_refusal(buf, version), 0x3, "{version}: {filter:?}");
						}
						(Some(groups), false) => {
							let (ok, objects) = fetch_answer(buf, version);
							assert_eq!(
								ok.end_location,
								Location { group: 2, object: 1 },
								"{version}: {filter:?}"
							);
							assert!(!ok.end_of_track, "{version}: {filter:?}");
							assert_eq!(objects, pairs(groups.clone()), "{version}: {filter:?}");
						}
					}
				}
			}
		}
	}

	/// Wider requests are refused before FETCH_OK, even if their end is past the track.
	#[moq_clock::test]
	async fn a_standalone_fetch_spanning_several_groups_is_refused() {
		for version in FETCH_DRAFTS {
			for order in [GroupOrder::Ascending, GroupOrder::Descending] {
				let mut h = serve(version);
				publish_pairs(&mut h, 5, Some(2));
				settle().await;
				for end in [3, 9] {
					let buf = standalone_fetch(
						&h,
						Location { group: 0, object: 1 },
						Location { group: end, object: 0 },
						order,
					)
					.await;
					assert_eq!(fetch_refusal(buf, version), 0x3, "{version} {order:?}");
					assert!(h.log.resets().is_empty());
				}
			}
		}
	}

	/// A whole live group waits for its end and never claims the track ends.
	#[moq_clock::test]
	async fn a_standalone_fetch_waits_for_the_largest_object_group() {
		for version in FETCH_DRAFTS {
			let mut h = serve(version);
			publish_pairs(&mut h, 4, None);
			let mut live = h.track.create_group(group::Info { sequence: 4 }).unwrap();
			live.write_frame(timestamp(), b"4-0".to_vec()).unwrap();
			settle().await;

			let fetch = standalone_fetch(
				&h,
				Location { group: 4, object: 0 },
				Location { group: 4, object: 0 },
				GroupOrder::Ascending,
			);
			let finish = async {
				settle().await;
				live.write_frame(timestamp(), b"4-1".to_vec()).unwrap();
				live.finish().unwrap();
			};
			let (buf, ()) = futures::join!(fetch, finish);
			let (ok, objects) = fetch_answer(buf, version);
			assert_eq!(
				ok.end_location,
				if Filter::is_draft20(version) {
					Location { group: 4, object: 1 }
				} else {
					Location { group: 4, object: 0 }
				},
				"{version}"
			);
			assert!(!ok.end_of_track, "{version}");
			assert_eq!(objects, pairs([4]), "{version}");
		}
	}

	/// Read a REQUEST_ERROR off a refused FETCH, as its code.
	fn fetch_refusal(wire: bytes::Bytes, version: Version) -> u64 {
		let mut buf = Decoder::new(&wire, version.into());
		assert_eq!(buf.varint().unwrap(), ietf::RequestError::ID);
		let code = ietf::RequestError::decode(&mut buf, version).unwrap().error_code;
		assert!(buf.is_empty(), "{version}: a refusal opens no fetch stream");
		code
	}

	/// A group that does not exist, below the newest one or past it, is refused.
	#[moq_clock::test]
	async fn a_standalone_fetch_of_a_missing_group_is_refused() {
		for version in FETCH_DRAFTS {
			for missing in [2, 5] {
				let mut h = serve(version);
				publish_pairs(&mut h, 4, Some(2));
				settle().await;

				let buf = standalone_fetch(
					&h,
					Location {
						group: missing,
						object: 0,
					},
					Location {
						group: missing,
						object: 0,
					},
					GroupOrder::Ascending,
				)
				.await;
				assert_eq!(
					fetch_refusal(buf, version),
					DOES_NOT_EXIST,
					"{version}: group {missing}"
				);
			}
		}
	}

	/// A standalone FETCH outside what the peer may subscribe to is refused before it
	/// reaches the origin, and one whose grant narrows while its group loads is refused too.
	#[moq_clock::test]
	async fn a_standalone_fetch_outside_the_grant_is_refused() {
		let unauthorized = |version| {
			crate::ietf::error::request::to_code(
				&Error::Unauthorized,
				crate::ietf::error::request::Kind::Fetch,
				version,
			)
		};
		let nothing = crate::auth::Grant {
			publish: Default::default(),
			subscribe: Default::default(),
			expires: None,
		};
		let location = Location { group: 0, object: 0 };

		for version in FETCH_DRAFTS {
			let mut h = serve(version);
			publish_pairs(&mut h, 1, None);
			settle().await;
			let auth = crate::auth::Link::new(false);
			auth.handle().authorize(&nothing);
			h.publisher = h.publisher.clone().with_auth(auth);

			let buf = standalone_fetch(&h, location, location, GroupOrder::Ascending).await;
			assert_eq!(
				fetch_refusal(buf, version),
				unauthorized(version),
				"{version}: denied up front"
			);
		}

		for version in FETCH_DRAFTS {
			// The group is still being written, so the fetch waits on it when the grant narrows.
			let h = serve(version);
			let _group = h.track.clone().create_group(group::Info { sequence: 0 }).unwrap();
			settle().await;
			let auth = crate::auth::Link::new(false);
			let mut h = h;
			h.publisher = h.publisher.clone().with_auth(auth.clone());

			let mut fetch = std::pin::pin!(standalone_fetch(
				&h,
				location,
				Location { group: 0, object: 1 },
				GroupOrder::Ascending
			));
			assert!(
				futures::poll!(fetch.as_mut()).is_pending(),
				"{version}: served an unwritten group"
			);
			auth.handle().authorize(&nothing);
			let buf = moq_clock::timeout(std::time::Duration::from_secs(1), fetch)
				.await
				.unwrap_or_else(|_| panic!("{version}: still loading after the grant narrowed"));
			assert_eq!(
				fetch_refusal(buf, version),
				unauthorized(version),
				"{version}: narrowed mid-load"
			);
		}
	}

	/// A grant that narrows while a standalone FETCH waits on stream credit for its response
	/// stops it there: nothing it no longer may send reaches the wire.
	#[moq_clock::test]
	async fn a_standalone_fetch_narrowed_while_responding_stops() {
		for version in FETCH_DRAFTS {
			let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
			let broadcast = origin.publish("room", crate::origin::Route::default()).unwrap();
			let track = broadcast.create_track("video", crate::track::Info::timed()).unwrap();
			let mut group = track.create_group(group::Info { sequence: 0 }).unwrap();
			group.write_frame(timestamp(), b"0-0".to_vec()).unwrap();
			group.finish().unwrap();
			settle().await;

			// Request streams flow, but no uni stream credit is ever granted.
			let bi = kio::Producer::new(true);
			let credit = kio::Producer::new(false);
			let session = SinkSession::gated_bi(bi.consume()).with_open_uni_gate(credit.consume());
			let peer_setup = peer::PeerSetup::default();
			peer_setup.set(peer::Peer::default());
			let auth = crate::auth::Link::new(false);
			let publisher = Publisher::new(
				crate::time::sim(),
				crate::driver::Transport::new(session.clone()),
				origin.consume(),
				Control::new(false),
				None,
				peer_setup,
				version,
			)
			.with_auth(auth.clone());

			let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), version)
				.await
				.unwrap();
			let mark = session.log.writes.lock().unwrap().len();
			let location = Location { group: 0, object: 0 };
			let mut fetch = std::pin::pin!(publisher.run_fetch_stream(
				stream,
				ietf::Fetch {
					request_id: FETCH_ID,
					subscriber_priority: 128,
					group_order: GroupOrder::Ascending,
					fetch_type: FetchType::Standalone {
						namespace: crate::Path::from_static("room"),
						track: "video".into(),
						start: location,
						end: Location { group: 0, object: 1 },
					},
					range_filters: false,
					fill_timeout: false,
					properties_wanted: true,
					epoch: None,
				},
			));
			// Driven until FETCH_OK is out, so the fetch is parked on uni stream credit.
			for _ in 0..100 {
				assert!(
					futures::poll!(fetch.as_mut()).is_pending(),
					"{version}: never waited on credit"
				);
				if session.log.writes.lock().unwrap().len() > mark {
					break;
				}
				moq_clock::sim::yield_now().await;
			}
			assert!(
				session.log.writes.lock().unwrap().len() > mark,
				"{version}: FETCH_OK never went out"
			);

			auth.handle().authorize(&crate::auth::Grant {
				publish: Default::default(),
				subscribe: Default::default(),
				expires: None,
			});
			let res = moq_clock::timeout(std::time::Duration::from_secs(1), fetch)
				.await
				.unwrap_or_else(|_| panic!("{version}: still responding after the grant narrowed"));
			assert!(matches!(res, Err(Error::Unauthorized)), "{version}: {res:?}");
			drop(credit);
		}
	}

	/// A joining FETCH answers from its subscription's cache, which outlives the
	/// subscription, so it holds its own gate: one parked on stream credit when the grant
	/// narrows stops there, and nothing reaches the wire once credit returns.
	#[moq_clock::test]
	async fn a_joining_fetch_narrowed_while_responding_stops() {
		const PAYLOAD: &[u8] = b"joined-prefix";

		for version in JOINING_DRAFTS {
			for fetch_type in [
				FetchType::RelativeJoining {
					subscriber_request_id: RequestId(REQUEST_ID),
					group_offset: 0,
				},
				FetchType::AbsoluteJoining {
					subscriber_request_id: RequestId(REQUEST_ID),
					group_id: 0,
				},
			] {
				let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
				let broadcast = origin.publish("room", crate::origin::Route::default()).unwrap();
				let track = broadcast.create_track("video", crate::track::Info::timed()).unwrap();
				let mut group = track.create_group(group::Info { sequence: 0 }).unwrap();
				group.write_frame(timestamp(), PAYLOAD.to_vec()).unwrap();
				settle().await;

				// Request streams flow, but no uni stream credit is granted until the end.
				let bi = kio::Producer::new(true);
				let credit = kio::Producer::new(false);
				let session = SinkSession::gated_bi(bi.consume()).with_open_uni_gate(credit.consume());
				let peer_setup = peer::PeerSetup::default();
				peer_setup.set(peer::Peer::default());
				let auth = crate::auth::Link::new(false);
				let publisher = Publisher::new(
					crate::time::sim(),
					crate::driver::Transport::new(session.clone()),
					origin.consume(),
					Control::new(false),
					None,
					peer_setup,
					version,
				)
				.with_auth(auth.clone());

				let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), version)
					.await
					.unwrap();
				let mut subscription = std::pin::pin!(
					publisher
						.clone()
						.run_subscribe_stream(stream, subscribe(Filter::NextObject, None))
				);
				let joined = publisher.joins.wait(|joins| match joins.get(&RequestId(REQUEST_ID)) {
					Some(Some(_)) => Poll::Ready(()),
					_ => Poll::Pending,
				});
				match futures::future::select(std::pin::pin!(joined), subscription.as_mut()).await {
					futures::future::Either::Left(_) => {}
					futures::future::Either::Right((res, _)) => panic!("{version}: subscription ended: {res:?}"),
				}

				let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), version)
					.await
					.unwrap();
				let mark = session.log.writes.lock().unwrap().len();
				let mut fetch = std::pin::pin!(publisher.clone().run_fetch_stream(
					stream,
					ietf::Fetch {
						request_id: FETCH_ID,
						subscriber_priority: 128,
						group_order: GroupOrder::Ascending,
						fetch_type,
						range_filters: false,
						fill_timeout: false,
						properties_wanted: true,
						epoch: None,
					},
				));
				// Driven until FETCH_OK is out, so the fetch is parked on uni stream credit.
				for _ in 0..100 {
					assert!(
						futures::poll!(fetch.as_mut()).is_pending(),
						"{version}: never waited on credit"
					);
					if session.log.writes.lock().unwrap().len() > mark {
						break;
					}
					moq_clock::sim::yield_now().await;
				}
				assert!(
					session.log.writes.lock().unwrap().len() > mark,
					"{version}: FETCH_OK never went out"
				);

				auth.handle().authorize(&crate::auth::Grant {
					publish: Default::default(),
					subscribe: Default::default(),
					expires: None,
				});
				let res = moq_clock::timeout(std::time::Duration::from_secs(1), fetch)
					.await
					.unwrap_or_else(|_| panic!("{version}: still responding after the grant narrowed"));
				assert!(matches!(res, Err(Error::Unauthorized)), "{version}: {res:?}");

				let Ok(mut open) = credit.write() else {
					panic!("credit gate closed");
				};
				*open = true;
				drop(open);
				let _ = futures::poll!(subscription.as_mut());
				settle().await;
				assert_eq!(
					occurrences(&session.log, PAYLOAD),
					0,
					"{version}: revoked media reached the wire"
				);
			}
		}
	}

	/// How a joining FETCH's subscription ends while the answer, past FETCH_OK, waits on
	/// stream credit.
	#[derive(Clone, Copy, Debug, PartialEq)]
	enum JoinEnd {
		/// The subscription ends, and the session grant still covers the fetch.
		Ended,
		/// A token-authorized subscription ends, and its token is still granted.
		TokenEnded,
		/// The token's grant stops covering the track while its subscription still serves.
		Narrowed,
		/// The token is revoked after its subscription ended.
		RevokedAfter,
	}

	/// Past FETCH_OK, a joining FETCH finishes its head on its own authority: the subscription
	/// ending leaves it to finish, but the token that authorized the subscription still ends
	/// it, before or after the subscription has.
	#[moq_clock::test]
	async fn a_joining_fetch_outlives_its_subscription_but_not_its_token() {
		const PAYLOAD: &[u8] = b"joined-prefix";
		for (version, end) in JOINING_DRAFTS.into_iter().flat_map(|version| {
			[
				JoinEnd::Ended,
				JoinEnd::TokenEnded,
				JoinEnd::Narrowed,
				JoinEnd::RevokedAfter,
			]
			.map(|end| (version, end))
		}) {
			let token = end != JoinEnd::Ended;
			let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
			let broadcast = origin.publish("room", crate::origin::Route::default()).unwrap();
			let track = broadcast.create_track("video", crate::track::Info::timed()).unwrap();
			let mut group = track.create_group(group::Info { sequence: 0 }).unwrap();
			group.write_frame(timestamp(), PAYLOAD.to_vec()).unwrap();
			settle().await;

			let bi = kio::Producer::new(true);
			let credit = kio::Producer::new(false);
			let session = SinkSession::gated_bi(bi.consume()).with_open_uni_gate(credit.consume());
			let peer_setup = peer::PeerSetup::default();
			peer_setup.set(peer::Peer::default());
			let auth = crate::auth::Link::new(false);
			let mut requests = auth.handle().requests();
			// A limit of nothing, so only the token admits the subscription.
			auth.handle().authorize(&match token {
				true => crate::auth::Grant::default(),
				false => crate::auth::Grant::all(),
			});
			let publisher = Publisher::new(
				crate::time::sim(),
				crate::driver::Transport::new(session.clone()),
				origin.consume(),
				Control::new(false),
				None,
				peer_setup,
				version,
			)
			.with_auth(auth.clone());

			let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), version)
				.await
				.unwrap();
			let mut msg = token_subscribe();
			if !token {
				msg.authorization_token = None;
			}
			let mut subscription = Box::pin(publisher.clone().run_subscribe_stream(stream, msg));
			let mut enforced = if token {
				let accepted = async { requests.next().await.expect("a request") };
				let request = match futures::future::select(std::pin::pin!(accepted), subscription.as_mut()).await {
					futures::future::Either::Left((request, _)) => request,
					futures::future::Either::Right((res, _)) => panic!("{version}: subscription ended: {res:?}"),
				};
				Some(request.accept(crate::auth::Grant::all()))
			} else {
				None
			};

			let joined = publisher.joins.wait(|joins| match joins.get(&RequestId(REQUEST_ID)) {
				Some(Some(_)) => Poll::Ready(()),
				_ => Poll::Pending,
			});
			match futures::future::select(std::pin::pin!(joined), subscription.as_mut()).await {
				futures::future::Either::Left(_) => {}
				futures::future::Either::Right((res, _)) => panic!("{version}: subscription ended: {res:?}"),
			}

			let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), version)
				.await
				.unwrap();
			let mark = session.log.writes.lock().unwrap().len();
			let mut fetch = std::pin::pin!(publisher.clone().run_fetch_stream(
				stream,
				ietf::Fetch {
					request_id: FETCH_ID,
					subscriber_priority: 128,
					group_order: GroupOrder::Ascending,
					fetch_type: FetchType::RelativeJoining {
						subscriber_request_id: RequestId(REQUEST_ID),
						group_offset: 0,
					},
					range_filters: false,
					fill_timeout: false,
					properties_wanted: true,
					epoch: None,
				},
			));
			for _ in 0..100 {
				assert!(
					futures::poll!(fetch.as_mut()).is_pending(),
					"{version}: never waited on credit"
				);
				if session.log.writes.lock().unwrap().len() > mark {
					break;
				}
				moq_clock::sim::yield_now().await;
			}
			assert!(
				session.log.writes.lock().unwrap().len() > mark,
				"{version}: FETCH_OK never went out"
			);

			let mut subscription = Some(subscription);
			if end == JoinEnd::Narrowed {
				enforced
					.as_ref()
					.expect("a token")
					.update(crate::auth::Grant::default());
				// The subscription reads the update first, and the fetch still sees it. The
				// subscription's request then waits on the fetch, which holds its token's slot.
				let serving = subscription.as_mut().expect("still serving");
				assert!(futures::poll!(serving.as_mut()).is_pending(), "{version}");
			} else {
				subscription = None;
				for _ in 0..10 {
					assert!(
						futures::poll!(fetch.as_mut()).is_pending(),
						"{version} {end:?}: the fetch ended with its subscription"
					);
					moq_clock::sim::yield_now().await;
				}
				if end == JoinEnd::RevokedAfter {
					let enforced = enforced.take().expect("a token");
					enforced.revoke(crate::SessionError::Unauthorized, "revoked");
				}
			}

			let Ok(mut open) = credit.write() else {
				panic!("credit gate closed");
			};
			*open = true;
			drop(open);
			let res = moq_clock::timeout(std::time::Duration::from_secs(1), fetch)
				.await
				.unwrap_or_else(|_| panic!("{version} {end:?}: the fetch never ended"));
			let finished = matches!(end, JoinEnd::Ended | JoinEnd::TokenEnded);
			match finished {
				true => assert!(res.is_ok(), "{version} {end:?}: {res:?}"),
				false => assert!(matches!(res, Err(Error::Unauthorized)), "{version} {end:?}: {res:?}"),
			}
			if let Some(subscription) = subscription {
				let _ = moq_clock::timeout(std::time::Duration::from_secs(1), subscription)
					.await
					.unwrap_or_else(|_| panic!("{version}: the revoked subscription kept serving"));
			}
			assert_eq!(
				occurrences(&session.log, PAYLOAD),
				usize::from(finished),
				"{version} {end:?}: the head on the wire"
			);
			drop(enforced);
		}
	}

	/// A token subscription on draft 18 whose renewal the acceptor is still verifying, and a
	/// FETCH joined to it that is past FETCH_OK and parked on stream credit, on its own task
	/// so only a wake of its own resumes it. Its first grant lapses in 60s.
	struct AnsweredJoin {
		h: Serve,
		serving: std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), Error>>>>,
		renewal: crate::auth::Request,
		fetch: moq_clock::sim::JoinHandle<Result<(), Error>>,
		/// The gate holding the fetch's answer back, shut.
		_credit: kio::Producer<bool>,
		_first: crate::auth::Issued,
		_cred: crate::auth::Credential,
	}

	const ANSWERED_HEAD: &[u8] = b"joined-head";

	async fn answered_join() -> AnsweredJoin {
		const VERSION: Version = Version::Draft18;

		let (auth, mut requests, _cred) = auth_covering_other();
		let credit = kio::Producer::new(false);
		let session = ScriptedSession::per_stream(vec![subscribe_update_with_token(VERSION, true).await])
			.with_open_uni_gate(credit.consume());
		let h = serve_on(VERSION, auth, session);
		let rt = h.publisher.runtime.clone();

		let mut group = h.track.create_group(group::Info { sequence: 0 }).unwrap();
		group.write_frame(timestamp(), ANSWERED_HEAD.to_vec()).unwrap();
		group.finish().unwrap();
		settle().await;

		let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), VERSION)
			.await
			.unwrap();
		let mut serving: std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), Error>>>> =
			Box::pin(h.publisher.clone().run_subscribe_stream(stream, token_subscribe()));
		let first = async { requests.next().await.expect("a request") };
		let first = match futures::future::select(std::pin::pin!(first), serving.as_mut()).await {
			futures::future::Either::Left((request, _)) => request,
			futures::future::Either::Right((res, _)) => panic!("subscription ended: {res:?}"),
		};
		let first = first.accept(grant_all_expiring(&rt, Some(60)));
		let renewal = async { requests.next().await.expect("a renewal") };
		let renewal = match futures::future::select(std::pin::pin!(renewal), serving.as_mut()).await {
			futures::future::Either::Left((request, _)) => request,
			futures::future::Either::Right((res, _)) => panic!("subscription ended: {res:?}"),
		};
		let joined = h.publisher.joins.wait(|joins| match joins.get(&RequestId(REQUEST_ID)) {
			Some(Some(_)) => Poll::Ready(()),
			_ => Poll::Pending,
		});
		match futures::future::select(std::pin::pin!(joined), serving.as_mut()).await {
			futures::future::Either::Left(_) => {}
			futures::future::Either::Right((res, _)) => panic!("subscription ended: {res:?}"),
		}

		let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), VERSION)
			.await
			.unwrap();
		let mark = h.log.writes.lock().unwrap().len();
		let fetch = moq_clock::sim::spawn(h.publisher.clone().run_fetch_stream(
			stream,
			ietf::Fetch {
				request_id: FETCH_ID,
				subscriber_priority: 128,
				group_order: GroupOrder::Ascending,
				fetch_type: FetchType::RelativeJoining {
					subscriber_request_id: RequestId(REQUEST_ID),
					group_offset: 0,
				},
				range_filters: false,
				fill_timeout: false,
				properties_wanted: true,
				epoch: None,
			},
		));
		for _ in 0..100 {
			if h.log.writes.lock().unwrap().len() > mark {
				break;
			}
			moq_clock::sim::yield_now().await;
		}
		assert!(h.log.writes.lock().unwrap().len() > mark, "FETCH_OK never went out");
		settle().await;
		assert!(!fetch.is_finished(), "never waited on credit");

		AnsweredJoin {
			h,
			serving,
			renewal,
			fetch,
			_credit: credit,
			_first: first,
			_cred,
		}
	}

	/// A refused renewal ends the token's authority, not only the subscription: a joining
	/// FETCH already past FETCH_OK stops serving under the refused token rather than finishing
	/// on the old grant, while the subscription still ends with UPDATE_FAILED.
	#[moq_clock::test]
	async fn a_refused_renewal_ends_an_answered_joining_fetch() {
		let join = answered_join().await;

		// Credit stays shut: the refusal alone must wake the fetch parked on it. The
		// subscription's request then waits on the fetch, which holds its token's slot.
		join.renewal.reject(crate::SessionError::Unauthorized, "no");
		let served = moq_clock::timeout(Duration::from_secs(1), join.serving)
			.await
			.expect("the refused subscription kept serving");
		assert!(
			matches!(served, Err(Error::Session(crate::SessionError::Unauthorized))),
			"{served:?}"
		);
		let (status, _) = Wire::read(&join.h.log, Version::Draft18)
			.publish_done
			.expect("PUBLISH_DONE was sent");
		assert_eq!(status, ietf::PublishDoneStatus::UpdateFailed.code(), "UPDATE_FAILED");
		let res = moq_clock::timeout(Duration::from_secs(1), join.fetch)
			.await
			.expect("the fetch never ended")
			.expect("the fetch panicked");
		assert!(
			matches!(res, Err(Error::Unauthorized)),
			"kept serving under the refused token: {res:?}"
		);
		assert_eq!(occurrences(&join.h.log, ANSWERED_HEAD), 0, "the head on the wire");
	}

	/// A renewal the subscription abandons, granted but never applied because the
	/// subscription ended first, does not keep the old grant alive for a joined FETCH that
	/// outlives it: the FETCH still ends when the old grant lapses.
	#[moq_clock::test]
	async fn an_abandoned_renewal_leaves_an_answered_joining_fetch_the_old_expiry() {
		let join = answered_join().await;
		let rt = join.h.publisher.runtime.clone();

		// Granted, and the grant kept, but the subscription ends before it reads the verdict.
		let _renewed = join.renewal.accept(grant_all_expiring(&rt, None));
		drop(join.serving);

		let res = moq_clock::timeout(Duration::from_secs(61), join.fetch)
			.await
			.expect("the fetch outlived its token")
			.expect("the fetch panicked");
		assert!(matches!(res, Err(Error::Unauthorized)), "{res:?}");
		assert_eq!(occurrences(&join.h.log, ANSWERED_HEAD), 0, "the head on the wire");
	}

	/// Where a token subscription is parked when its grant ends.
	#[derive(Clone, Copy, Debug)]
	enum Parked {
		/// Resolving a broadcast whose route never answers.
		Resolving,
		/// Writing SUBSCRIBE_OK to a peer that grants no flow control.
		Acking,
		/// Writing the END_OF_TRACK marker without stream credit.
		Ending,
	}

	/// A token subscription's grant ending ends it at every await, setup and teardown
	/// included, not only while it serves: what it holds upstream is released first, and the
	/// peer is then told as far as it got, refused before SUBSCRIBE_OK and done after it.
	#[moq_clock::test]
	async fn a_revoked_token_ends_a_subscription_wherever_it_waits() {
		for version in ALL_VERSIONS {
			for parked in [Parked::Resolving, Parked::Acking, Parked::Ending] {
				revoked_while(version, parked).await;
			}
		}
	}

	async fn revoked_while(version: Version, parked: Parked) {
		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let (claim, published) = match parked {
			Parked::Resolving => (
				Some(
					origin
						.requests_route("room", crate::origin::Route::default(), None)
						.unwrap(),
				),
				None,
			),
			Parked::Acking | Parked::Ending => {
				let broadcast = origin.publish("room", crate::origin::Route::default()).unwrap();
				let track = broadcast.create_track("video", crate::track::Info::timed()).unwrap();
				match parked {
					// One published group, so the subscription parks at the live edge.
					Parked::Acking => {
						let mut group = track.create_group(group::Info { sequence: 0 }).unwrap();
						group.write_frame(timestamp(), b"frame".to_vec()).unwrap();
						group.finish().unwrap();
					}
					// An empty finished track: nothing to serve but the marker.
					_ => track.finish().unwrap(),
				}
				(None, Some((broadcast, track)))
			}
		};
		settle().await;

		let bi = kio::Producer::new(!matches!(parked, Parked::Acking));
		let credit = kio::Producer::new(false);
		let session = SinkSession::gated_bi(bi.consume()).with_open_uni_gate(credit.consume());
		let peer_setup = peer::PeerSetup::default();
		peer_setup.set(peer::Peer::default());
		let auth = crate::auth::Link::new(false);
		let mut requests = auth.handle().requests();
		// A limit of nothing, so only the token admits the subscription.
		auth.handle().authorize(&crate::auth::Grant::default());
		let publisher = Publisher::new(
			crate::time::sim(),
			crate::driver::Transport::new(session.clone()),
			origin.consume(),
			Control::new(false),
			None,
			peer_setup,
			version,
		)
		.with_auth(auth.clone());

		let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), version)
			.await
			.unwrap();
		let mut subscription = std::pin::pin!(publisher.clone().run_subscribe_stream(stream, token_subscribe()));
		let accepted = async { requests.next().await.expect("a request") };
		let request = match futures::future::select(std::pin::pin!(accepted), subscription.as_mut()).await {
			futures::future::Either::Left((request, _)) => request,
			futures::future::Either::Right((res, _)) => panic!("{version} {parked:?}: ended unverified: {res:?}"),
		};
		let enforced = request.accept(crate::auth::Grant::all());

		// Drive the subscription to where it parks.
		let _route = match &claim {
			Some(claim) => {
				let requested = std::pin::pin!(claim.requested_broadcast());
				match futures::future::select(requested, subscription.as_mut()).await {
					futures::future::Either::Left((route, _)) => Some(route.unwrap()),
					futures::future::Either::Right((res, _)) => panic!("{version} {parked:?}: ended: {res:?}"),
				}
			}
			None => None,
		};
		for _ in 0..50 {
			assert!(
				futures::poll!(subscription.as_mut()).is_pending(),
				"{version} {parked:?}: ended before its grant did"
			);
			settle().await;
		}
		let subscribed = || {
			published
				.as_ref()
				.is_some_and(|(_, track)| track.subscription().is_some())
		};
		match parked {
			Parked::Resolving => {}
			Parked::Acking => {
				assert!(subscribed(), "{version}: never subscribed upstream");
				assert!(
					session.log.writes.lock().unwrap().is_empty(),
					"{version}: SUBSCRIBE_OK was not held"
				);
			}
			Parked::Ending => assert!(
				session.log.trail().iter().all(|(stream, _)| *stream == 0),
				"{version}: the marker went out without credit"
			),
		}

		enforced.revoke(crate::SessionError::Unauthorized, "revoked");
		if matches!(parked, Parked::Acking) {
			// The peer cannot read a word yet, but the upstream subscription is already gone.
			for _ in 0..10 {
				assert!(futures::poll!(subscription.as_mut()).is_pending(), "{version}");
				settle().await;
			}
			assert!(!subscribed(), "{version}: held upstream until the peer could be told");
			let Ok(mut writable) = bi.write() else {
				panic!("request stream gate closed");
			};
			*writable = true;
			drop(writable);
		}
		let _ = moq_clock::timeout(std::time::Duration::from_secs(1), subscription.as_mut())
			.await
			.unwrap_or_else(|_| panic!("{version} {parked:?}: outlived its grant"));

		let wire = Wire::read(&session.log, version);
		let sent: Vec<u64> = wire.messages.iter().map(|(id, _)| *id).collect();
		let expected = match (parked, version) {
			(Parked::Resolving, _) => vec![ietf::RequestError::ID],
			_ => vec![ietf::SubscribeOk::ID, ietf::PublishDone::ID],
		};
		assert_eq!(sent, expected, "{version} {parked:?}");
		drop(credit);
	}

	/// Where a SUBSCRIBE's setup waits on a producer that never answers.
	#[derive(Clone, Copy, Debug)]
	enum Setup {
		/// Resolving a broadcast whose route never answers.
		Resolving,
		/// Subscribing to a track its broadcast never answers.
		Subscribing,
		/// Waiting for an idle copy's live edge, which its route never reports.
		Live,
	}

	/// Withdraw the SUBSCRIBE on `request` as `version` cancels one: a FIN on draft 18, a
	/// reset after.
	fn withdraw(request: &ScriptedSession, version: Version) {
		use crate::lite::test_transport::Close;
		match version {
			Version::Draft18 => request.close(Close::Fin),
			_ => request.close(Close::Reset),
		}
	}

	/// A subscriber withdrawing a SUBSCRIBE while its setup waits on a producer that never
	/// answers ends it at once: its demand and its slot are given back, and nothing answers it.
	#[moq_clock::test]
	async fn a_subscribe_withdrawn_during_setup_releases_its_demand() {
		for version in ALL_VERSIONS {
			for setup in [Setup::Resolving, Setup::Subscribing, Setup::Live] {
				withdrawn_during(version, setup, request::Kind::Subscribe).await;
			}
		}
	}

	/// Broadcast resolution cannot hold a withdrawn FETCH or TRACK_STATUS alive.
	#[moq_clock::test]
	async fn requests_withdrawn_while_resolving_release_their_demand() {
		for version in ALL_VERSIONS {
			for kind in [request::Kind::Fetch, request::Kind::TrackStatus] {
				withdrawn_during(version, Setup::Resolving, kind).await;
			}
		}
	}

	async fn withdrawn_during(version: Version, setup: Setup, kind: request::Kind) {
		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let session = ScriptedSession::new(Vec::new());
		let peer_setup = peer::PeerSetup::default();
		peer_setup.set(peer::Peer::default());
		let publisher = Publisher::new(
			crate::time::sim(),
			crate::driver::Transport::new(session.clone()),
			origin.consume(),
			Control::new(false),
			None,
			peer_setup,
			version,
		);
		let request = ScriptedSession::new(Vec::new());
		let stream = Stream::open(&mut crate::driver::Transport::new(request.clone()), version)
			.await
			.unwrap();
		let mut serving = std::pin::pin!(async {
			match kind {
				request::Kind::Subscribe => {
					publisher
						.clone()
						.run_subscribe_stream(stream, subscribe(Filter::NextObject, None))
						.await
				}
				request::Kind::Fetch => {
					publisher
						.clone()
						.run_fetch_stream(
							stream,
							ietf::Fetch {
								request_id: RequestId(REQUEST_ID),
								subscriber_priority: 128,
								group_order: GroupOrder::Ascending,
								fetch_type: fetch_range(
									version,
									"video",
									Location { group: 0, object: 0 },
									Location { group: 0, object: 1 },
								),
								range_filters: false,
								fill_timeout: false,
								properties_wanted: true,
								epoch: None,
							},
						)
						.await
				}
				request::Kind::TrackStatus => {
					publisher
						.clone()
						.run_track_status_stream(
							stream,
							ietf::TrackStatus {
								request_id: RequestId(REQUEST_ID),
								track_namespace: crate::Path::from_static("room"),
								track_name: "video".into(),
								properties_wanted: true,
								epoch: None,
							},
						)
						.await
				}
				_ => unreachable!(),
			}
		});

		// Drive the subscription to where it waits, holding what it asked for.
		let mut claim = None;
		let mut asked = None;
		let mut idle = None;
		match setup {
			Setup::Resolving => {
				let route = origin
					.requests_route("room", crate::origin::Route::default(), None)
					.unwrap();
				let request = {
					let requested = std::pin::pin!(route.requested_broadcast());
					match futures::future::select(requested, serving.as_mut()).await {
						futures::future::Either::Left((request, _)) => request.unwrap(),
						futures::future::Either::Right((res, _)) => panic!("{version} {setup:?}: ended: {res:?}"),
					}
				};
				claim = Some((route, request));
			}
			Setup::Subscribing => {
				let broadcast = origin.publish("room", crate::origin::Route::default()).unwrap();
				let mut dynamic = broadcast.requests();
				let request = {
					let requested = std::pin::pin!(dynamic.requested_track());
					match futures::future::select(requested, serving.as_mut()).await {
						futures::future::Either::Left((request, _)) => request.unwrap(),
						futures::future::Either::Right((res, _)) => panic!("{version} {setup:?}: ended: {res:?}"),
					}
				};
				asked = Some((broadcast, dynamic, request));
			}
			Setup::Live => {
				let broadcast = origin.publish("room", crate::origin::Route::default()).unwrap();
				let mut track = broadcast.create_track("video", crate::track::Info::timed()).unwrap();
				track.set_idle();
				serve_until(serving.as_mut(), "the upstream subscription", || {
					track.subscription().is_some()
				})
				.await;
				idle = Some((broadcast, track));
			}
		}
		let demanded = || {
			asked
				.as_ref()
				.is_some_and(|(_, _, request)| request.subscription().is_some())
				|| idle.as_ref().is_some_and(|(_, track)| track.subscription().is_some())
		};
		for _ in 0..10 {
			assert!(
				futures::poll!(serving.as_mut()).is_pending(),
				"{version} {setup:?}: setup ended unanswered"
			);
			settle().await;
		}
		assert_eq!(
			publisher.subscriptions.available(),
			usize::MAX - usize::from(matches!(kind, request::Kind::Subscribe)),
			"{version} {setup:?}"
		);
		assert_eq!(demanded(), !matches!(setup, Setup::Resolving), "{version} {setup:?}");

		withdraw(&request, version);
		let res = moq_clock::timeout(std::time::Duration::from_secs(1), serving)
			.await
			.unwrap_or_else(|_| panic!("{version} {setup:?}: outlived its withdrawal"));
		assert!(res.is_ok(), "{version} {setup:?}: {res:?}");
		settle().await;
		assert!(!demanded(), "{version} {setup:?}: held its demand");
		assert_eq!(publisher.subscriptions.available(), usize::MAX, "{version} {setup:?}");
		assert!(
			request.log.writes.lock().unwrap().is_empty(),
			"{version} {setup:?}: answered a withdrawn request"
		);
		drop(claim);
	}

	/// A resume subscribes again as a new SUBSCRIBE does, so a subscriber withdrawing the
	/// request while the resume waits on an idle copy's live edge ends it at once too.
	#[moq_clock::test]
	async fn a_subscribe_withdrawn_while_resuming_releases_its_demand() {
		for version in ALL_VERSIONS {
			let mut h = serve(version);
			let mut request = ScriptedSession::new(Vec::new());
			request.log = h.log.clone();
			let stream = Stream::open(&mut crate::driver::Transport::new(request.clone()), version)
				.await
				.unwrap();
			let mut msg = subscribe(Filter::NextObject, None);
			msg.forward = false;
			let mut serve = std::pin::pin!(h.publisher.clone().run_subscribe_stream(stream, msg));
			serve_until(serve.as_mut(), "the pause", || {
				!h.log.trail().is_empty() && h.track.subscription().is_none()
			})
			.await;

			h.track.set_idle();
			request.push(&forward_update(version, 4, true));
			serve_until(serve.as_mut(), "the resume", || h.track.subscription().is_some()).await;
			for _ in 0..10 {
				assert!(
					futures::poll!(serve.as_mut()).is_pending(),
					"{version}: the resume ended unanswered"
				);
				settle().await;
			}

			withdraw(&request, version);
			moq_clock::timeout(std::time::Duration::from_secs(1), serve)
				.await
				.unwrap_or_else(|_| panic!("{version}: outlived its withdrawal"))
				.unwrap();
			settle().await;
			assert!(h.track.subscription().is_none(), "{version}: held its demand");
		}
	}

	/// A datagram group is never fetchable: a FETCH for one is refused like a group that
	/// does not exist, newest or not, and opens no fetch stream for its payload.
	#[moq_clock::test]
	async fn a_standalone_fetch_of_a_datagram_group_is_refused() {
		for version in FETCH_DRAFTS {
			for newest in [true, false] {
				let mut h = serve(version);
				publish_pairs(&mut h, 2, None);
				let sequence = h.track.append_datagram(timestamp(), b"d".as_slice()).unwrap();
				if !newest {
					h.track.append_group().unwrap().finish().unwrap();
				}
				settle().await;

				let location = Location {
					group: sequence,
					object: 0,
				};
				let buf = standalone_fetch(&h, location, location, GroupOrder::Ascending).await;
				assert_eq!(
					fetch_refusal(buf, version),
					DOES_NOT_EXIST,
					"{version}: newest={newest}"
				);
			}
		}
	}

	/// A joining FETCH reaching back before the subscription's group is refused.
	#[moq_clock::test]
	async fn a_joining_fetch_spanning_several_groups_is_refused() {
		for version in JOINING_DRAFTS {
			let mut h = serve(version);
			publish_pairs(&mut h, 4, None);
			let mut group = h.track.create_group(group::Info { sequence: 4 }).unwrap();
			group.write_frame(timestamp(), b"4-0".as_slice()).unwrap();
			settle().await;

			let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), version)
				.await
				.unwrap();
			let mut serving = std::pin::pin!(
				h.publisher
					.clone()
					.run_subscribe_stream(stream, subscribe(Filter::NextObject, None))
			);
			registered(&h, serving.as_mut()).await;

			for (fetch_type, _first) in [
				(
					FetchType::RelativeJoining {
						subscriber_request_id: RequestId(REQUEST_ID),
						group_offset: 2,
					},
					2,
				),
				(
					FetchType::AbsoluteJoining {
						subscriber_request_id: RequestId(REQUEST_ID),
						group_id: 1,
					},
					1,
				),
			] {
				let mark = h.log.writes.lock().unwrap().len();
				let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), version)
					.await
					.unwrap();
				h.publisher
					.clone()
					.run_fetch_stream(
						stream,
						ietf::Fetch {
							request_id: FETCH_ID,
							subscriber_priority: 128,
							group_order: GroupOrder::Ascending,
							fetch_type,
							range_filters: false,
							fill_timeout: false,
							properties_wanted: true,
							epoch: None,
						},
					)
					.await
					.unwrap();
				let buf = bytes::Bytes::from(h.log.writes.lock().unwrap()[mark..].to_vec());

				assert_eq!(fetch_refusal(buf, version), 0x3, "{version}");
			}
		}
	}

	/// An absolute joining FETCH starting past the subscription's group names an empty range.
	#[moq_clock::test]
	async fn an_absolute_joining_fetch_past_the_subscription_is_refused() {
		let version = Version::Draft18;
		let mut h = serve(version);
		publish_pairs(&mut h, 3, None);
		settle().await;

		let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), version)
			.await
			.unwrap();
		let mut serving = std::pin::pin!(
			h.publisher
				.clone()
				.run_subscribe_stream(stream, subscribe(Filter::NextObject, None))
		);
		registered(&h, serving.as_mut()).await;

		let mark = h.log.writes.lock().unwrap().len();
		let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), version)
			.await
			.unwrap();
		h.publisher
			.clone()
			.run_fetch_stream(
				stream,
				ietf::Fetch {
					request_id: FETCH_ID,
					subscriber_priority: 128,
					group_order: GroupOrder::Ascending,
					fetch_type: FetchType::AbsoluteJoining {
						subscriber_request_id: RequestId(REQUEST_ID),
						group_id: 5,
					},
					range_filters: false,
					fill_timeout: false,
					properties_wanted: true,
					epoch: None,
				},
			)
			.await
			.unwrap();
		let buf = bytes::Bytes::from(h.log.writes.lock().unwrap()[mark..].to_vec());
		assert_eq!(fetch_refusal(buf, version), INVALID_RANGE);
	}

	/// A fill against an empty track has an empty range: no fetch stream is owed.
	#[moq_clock::test]
	async fn an_empty_track_opens_no_fill_stream() {
		let mut h = serve(Version::Draft22);

		run_live(
			&mut h,
			subscribe(
				Filter::NextObject,
				Some(ietf::Fill {
					group_order: None,
					filter: Some(Filter::Relative(1)),
					range_filters: false,
				}),
			),
		)
		.await;

		assert_eq!(occurrences(&h.log, FETCH_STREAM), 0);
		assert!(h.log.resets().is_empty());
	}

	/// Advertise the true snapshot used by a joining FETCH or subscription fill.
	async fn subscribe_ok_largest(version: Version) -> Option<Location> {
		let mut h = serve(version);

		// A live edge in the middle of group 5: objects 0 through 3.
		let mut group = h.track.create_group(group::Info { sequence: 5 }).unwrap();
		for payload in [b"5-0", b"5-1", b"5-2", b"5-3"] {
			group.write_frame(timestamp(), payload.as_slice()).unwrap();
		}
		group.finish().unwrap();

		run_live(&mut h, subscribe(Filter::Unfiltered, None)).await;

		// SUBSCRIBE_OK is the first thing written, before any group stream opens.
		let writes = h.log.writes.lock().unwrap().clone();
		let mut buf = writes.as_slice();
		assert_eq!(
			crate::coding::decode_varint(&mut buf, version).unwrap(),
			ietf::SubscribeOk::ID
		);
		crate::coding::decode_buf(&mut buf, version, |r, v| ietf::SubscribeOk::decode(r, v))
			.unwrap()
			.largest
	}

	/// SUBSCRIBE_OK announces the publisher's default order, newest first, whatever the
	/// subscription asked for.
	#[moq_clock::test]
	async fn subscribe_ok_announces_the_default_order() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			for requested in [GroupOrder::Ascending, GroupOrder::Descending, GroupOrder::Any] {
				let mut h = serve(version);
				let mut msg = subscribe(Filter::NextObject, None);
				msg.group_order = requested;
				run_live(&mut h, msg).await;

				let writes = h.log.writes.lock().unwrap().clone();
				let mut buf = writes.as_slice();
				assert_eq!(
					crate::coding::decode_varint(&mut buf, version).unwrap(),
					ietf::SubscribeOk::ID
				);
				let ok = crate::coding::decode_buf(&mut buf, version, |r, v| ietf::SubscribeOk::decode(r, v)).unwrap();
				assert_eq!(
					ok.properties.group_order,
					Some(GroupOrder::Descending),
					"{version:?} {requested:?}"
				);
			}
		}
	}

	#[moq_clock::test]
	async fn largest_object_is_the_live_edge_before_draft22() {
		for version in [Version::Draft18] {
			assert_eq!(
				subscribe_ok_largest(version).await,
				Some(Location { group: 5, object: 3 }),
				"{version:?} advertises the snapshot used by joining FETCH"
			);
		}
	}

	#[moq_clock::test]
	async fn largest_object_is_the_live_edge_on_draft22() {
		assert_eq!(
			subscribe_ok_largest(Version::Draft22).await,
			Some(Location { group: 5, object: 3 }),
			"a fill sizes its backfill against the true edge"
		);
	}

	/// The filter's object bounds trim what `run_group` writes: the skipped head is not
	/// sent, the first written object's delta is its absolute id, and a capped tail stops
	/// early. Extensions are off so the wire is just deltas, sizes, and payloads.
	#[moq_clock::test]
	async fn run_group_honors_the_slice() {
		fn header() -> ietf::GroupHeader {
			ietf::GroupHeader {
				track_alias: 0,
				group_id: 0,
				sub_group_id: 0,
				publisher_priority: 0,
				flags: ietf::GroupFlags {
					first_object: false,
					..Default::default()
				},
			}
		}

		async fn serve_slice(slice: GroupSlice) -> Vec<u8> {
			let log = Log::default();
			let session = SinkSession::new(log.clone());
			let track = track::Producer::new(
				std::sync::Arc::new(crate::broadcast::Info::default()),
				"test",
				crate::track::Info::timed(),
			);
			let mut group = track.create_group(group::Info { sequence: 0 }).unwrap();
			for payload in [b"aa", b"bb", b"cc", b"dd"] {
				group.write_frame(timestamp(), payload.as_slice()).unwrap();
			}
			let consumer = group.consume();
			group.finish().unwrap();

			let mut serve = GroupServe::new(
				crate::driver::Transport::new(session.clone()),
				header(),
				Rank::solo(kio::Producer::new(0).consume()),
				consumer,
				Some(Timescale::MILLI),
				Version::Draft22,
				slice,
			);
			kio::wait(|waiter| serve.poll_serve(waiter)).await.unwrap();

			log.writes.lock().unwrap().clone()
		}

		// Skip 2: the head is dropped and the first delta is the absolute id 2.
		let trimmed = serve_slice(GroupSlice { skip: 2, until: None }).await;
		assert!(
			trimmed.ends_with(&[0x02, 0x02, b'c', b'c', 0x00, 0x02, b'd', b'd']),
			"expected delta 2 then cc, delta 0 then dd, got {trimmed:x?}"
		);

		// Until 2: only the head is written, stopping before the cap.
		let capped = serve_slice(GroupSlice {
			skip: 0,
			until: Some(2),
		})
		.await;
		assert!(
			capped.ends_with(&[0x00, 0x02, b'a', b'a', 0x00, 0x02, b'b', b'b']),
			"expected aa then bb only, got {capped:x?}"
		);
		assert_eq!(
			capped.windows(2).filter(|w| *w == b"cc").count(),
			0,
			"the cap excludes cc"
		);
	}

	/// A stream cut short by the range's end Location does not claim END_OF_GROUP, since
	/// the group goes on past it. A range ending on a whole group still does. A cap at or past
	/// the group's real end clears the bit too, since the header is written before we know.
	#[moq_clock::test]
	async fn capped_group_does_not_claim_its_end() {
		async fn serve(object: Option<u64>) -> ietf::GroupHeader {
			let log = crate::lite::test_transport::Log::default();
			let session = SinkSession::new(log.clone());

			let track = track::Producer::new(
				std::sync::Arc::new(crate::broadcast::Info::default()),
				"test",
				crate::track::Info::timed(),
			);
			let subscriber = track.subscribe(None);

			let mut group = track.append_group().unwrap();
			for payload in [b"aa", b"bb", b"cc"] {
				group.write_frame(crate::Timestamp::ZERO, payload.as_slice()).unwrap();
			}
			group.finish().unwrap();
			track.finish().unwrap();

			let range = ServeRange {
				start: Some(Location { group: 0, object: 0 }),
				end: Some(EndLocation { group: 0, object }),
			};
			let mut serve = TrackServe::new(
				crate::driver::Transport::new(session.clone()),
				subscriber,
				RequestId(0),
				Version::Draft22,
				range,
				None,
			);
			kio::wait(|waiter| serve.poll(waiter)).await.unwrap();

			let mut buf = bytes::Bytes::from(log.writes.lock().unwrap().clone());
			crate::coding::decode_buf(&mut buf, Version::Draft22, |r, v| ietf::GroupHeader::decode(r, v))
				.expect("a group header")
		}

		assert!(!serve(Some(0)).await.flags.has_end, "capped at object 0 of 3");
		assert!(!serve(Some(2)).await.flags.has_end, "capped at the last object");
		assert!(!serve(Some(3)).await.flags.has_end, "capped past the last object");
		assert!(serve(None).await.flags.has_end, "the whole group");
	}

	/// Like [`serve`], but with an app auth acceptor wired in and the subscribe stream's
	/// reader scripted with `first_script` (a REQUEST_UPDATE, for the renewal tests).
	fn serve_with_auth(version: Version, auth: crate::auth::Link, first_script: Vec<u8>) -> Serve {
		serve_on(version, auth, ScriptedSession::per_stream(vec![first_script]))
	}

	/// [`serve_with_auth`] over a given session, such as one that finishes the stream after
	/// its script.
	fn serve_on(version: Version, auth: crate::auth::Link, session: ScriptedSession) -> Serve {
		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let broadcast = origin.publish("room", crate::origin::Route::default()).unwrap();
		let track = broadcast.create_track("video", crate::track::Info::timed()).unwrap();

		let log = session.log.clone();

		let peer_setup = peer::PeerSetup::default();
		peer_setup.set(peer::Peer::default());

		let publisher = Publisher::new(
			crate::time::sim(),
			crate::driver::Transport::new(session.clone()),
			origin.consume(),
			Control::new(false),
			None,
			peer_setup,
			version,
		)
		.with_auth(auth);

		Serve {
			publisher,
			session,
			log,
			track,
			_origin: origin,
			_broadcast: broadcast,
		}
	}

	/// A grant to subscribe to everything, lapsing in `secs` (or never), on the publisher's clock.
	fn grant_all_expiring(runtime: &crate::time::Deadlines, secs: Option<u64>) -> crate::auth::Grant {
		crate::auth::Grant {
			publish: crate::Patterns::new(),
			subscribe: crate::Pattern::all().into(),
			expires: secs.map(|s| runtime.now().checked_add(Duration::from_secs(s)).unwrap()),
		}
	}

	/// An auth handle whose session grant covers only `other`, so a SUBSCRIBE for `room`
	/// falls to its request token. The presented credential is kept alive by the returned
	/// [`crate::auth::Credential`].
	fn auth_covering_other() -> (crate::auth::Link, crate::auth::Requests, crate::auth::Credential) {
		let auth = crate::auth::Link::new(true);
		let requests = auth.handle().requests();
		let cred = auth.present(bytes::Bytes::from_static(b"cred"), true).unwrap();
		// The peer lets us publish everything, and we let it read only "other".
		auth.granted(0, crate::auth::Grant::all());
		auth.handle().authorize(&crate::auth::Grant {
			publish: crate::Patterns::new(),
			subscribe: crate::Pattern::subtree("other").unwrap().into(),
			expires: None,
		});
		assert!(
			!auth.allows(crate::auth::Direction::Publish, "room"),
			"the limit must not cover the request path"
		);
		(auth, requests, cred)
	}
	/// An out-of-band request token, the only kind a session verifies.
	/// A request token whose Token Type takes two varint bytes.
	fn request_token() -> crate::ietf::token::RequestToken {
		crate::ietf::token::RequestToken(crate::setup::Token {
			kind: crate::setup::Token::OUT_OF_BAND,
			value: vec![0x00, 0xff],
		})
	}

	/// One REQUEST_UPDATE carrying a fresh AUTHORIZATION TOKEN, framed as the peer sends it.
	async fn subscribe_update_with_token(version: Version, forward: bool) -> Vec<u8> {
		let log = crate::lite::test_transport::Log::default();
		let mut writer = crate::coding::Writer::new(
			crate::transport::boxed::SendStream::new(crate::lite::test_transport::SinkSend::new(log.clone())),
			version,
		);
		let msg = ietf::SubscribeUpdate {
			max_age: None,
			new_group: None,
			request_id: RequestId(0x40),
			subscriber_priority: Some(128),
			forward: Some(forward),
			filter: None,
			authorization_token: Some(request_token()),
			fill: None,
		};
		writer.varint(ietf::SubscribeUpdate::ID).await.unwrap();
		writer.encode(&msg).await.unwrap();
		log.writes.lock().unwrap().clone()
	}

	/// The wire bytes of one REQUEST_OK.
	async fn request_ok_bytes(version: Version) -> Vec<u8> {
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
		log.writes.lock().unwrap().clone()
	}

	/// The REQUEST_ERROR a refused renewal writes before the subscription ends, matching
	/// `write_subscribe_error(.., &Error::Unauthorized, reason)`.
	async fn request_error_bytes(version: Version, reason: &str) -> Vec<u8> {
		let log = crate::lite::test_transport::Log::default();
		let mut writer = crate::coding::Writer::new(
			crate::transport::boxed::SendStream::new(crate::lite::test_transport::SinkSend::new(log.clone())),
			version,
		);
		writer.varint(ietf::RequestError::ID).await.unwrap();
		writer
			.encode(&ietf::RequestError {
				error_code: request::to_code(&Error::Unauthorized, request::Kind::Subscribe, version),
				reason_phrase: reason.into(),
				retry_interval: 0,
			})
			.await
			.unwrap();
		log.writes.lock().unwrap().clone()
	}

	/// A live SUBSCRIBE for `room/video` presenting a request token, with one published
	/// group so the subscription parks at the live edge rather than ending.
	fn token_subscribe() -> ietf::Subscribe<'static> {
		let mut msg = subscribe(Filter::NextObject, None);
		msg.request_id = RequestId(REQUEST_ID);
		msg.authorization_token = Some(request_token());
		msg
	}

	/// A REQUEST_UPDATE whose token the acceptor renews keeps a token-authorized
	/// subscription alive past the old grant's expiry: the reader re-verifies the token off
	/// the subscribe stream and re-arms the deadline. Proven against the deadline the
	/// original grant would otherwise have lapsed at.
	#[moq_clock::test]
	async fn a_request_update_renews_a_token_subscription_past_the_old_expiry() {
		const VERSION: Version = Version::Draft18;

		let (auth, mut requests, _cred) = auth_covering_other();
		let h = serve_with_auth(VERSION, auth, subscribe_update_with_token(VERSION, true).await);
		let rt = h.publisher.runtime.clone();

		let mut group = h.track.create_group(group::Info { sequence: 0 }).unwrap();
		group.write_frame(timestamp(), b"frame".as_slice()).unwrap();
		group.finish().unwrap();
		settle().await;

		let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), VERSION)
			.await
			.unwrap();

		let answered = std::sync::Arc::new(AtomicU64::new(0));
		let acceptor = {
			let answered = answered.clone();
			async move {
				let mut held = Vec::new();
				// The first grant lapses in 60s; the renewal never expires.
				let first = requests.next().await.unwrap();
				held.push(first.accept(grant_all_expiring(&rt, Some(60))));
				answered.fetch_add(1, Ordering::Relaxed);
				let renewal = requests.next().await.unwrap();
				held.push(renewal.accept(grant_all_expiring(&rt, None)));
				answered.fetch_add(1, Ordering::Relaxed);
				std::future::pending::<()>().await
			}
		};
		let mut acceptor = std::pin::pin!(acceptor);
		let mut serving = std::pin::pin!(h.publisher.clone().run_subscribe_stream(stream, token_subscribe()));

		// Drive until the acceptor has answered the initial token and the renewal.
		for _ in 0..500 {
			let _ = futures::poll!(acceptor.as_mut());
			assert!(
				futures::poll!(serving.as_mut()).is_pending(),
				"subscription ended during setup"
			);
			if answered.load(Ordering::Relaxed) >= 2 {
				break;
			}
			settle().await;
		}
		assert_eq!(
			answered.load(Ordering::Relaxed),
			2,
			"acceptor never answered both tokens"
		);

		// Let the serve loop apply the renewal it read off the stream.
		for _ in 0..20 {
			let _ = futures::poll!(acceptor.as_mut());
			assert!(futures::poll!(serving.as_mut()).is_pending());
			settle().await;
		}

		// Past the original 60s expiry: the renewal re-armed the deadline, so the
		// subscription lives on.
		moq_clock::sim::advance(Duration::from_secs(120)).await;
		for _ in 0..50 {
			let _ = futures::poll!(acceptor.as_mut());
			assert!(
				futures::poll!(serving.as_mut()).is_pending(),
				"the renewal did not extend the subscription past the old expiry"
			);
			settle().await;
		}
	}

	/// A pause is answered before a renewal riding it could be verified, so an update carrying
	/// both fails instead of acknowledging a token nobody checked.
	#[moq_clock::test]
	async fn a_pause_carrying_a_token_fails_the_update() {
		for version in [Version::Draft18] {
			let (auth, mut requests, _cred) = auth_covering_other();
			let h = serve_with_auth(version, auth, subscribe_update_with_token(version, false).await);
			let rt = h.publisher.runtime.clone();
			let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), version)
				.await
				.unwrap();
			let acceptor = async move {
				let first = requests.next().await.unwrap();
				let _held = first.accept(grant_all_expiring(&rt, Some(60)));
				std::future::pending::<()>().await
			};
			let mut acceptor = std::pin::pin!(acceptor);
			let mut serving = std::pin::pin!(h.publisher.clone().run_subscribe_stream(stream, token_subscribe()));
			let mut ended = false;
			for _ in 0..200 {
				let _ = futures::poll!(acceptor.as_mut());
				if futures::poll!(serving.as_mut()).is_ready() {
					ended = true;
					break;
				}
				settle().await;
			}
			assert!(ended, "{version:?}: the failed update ends the subscription");
			let wire = Wire::read(&h.log, version);
			assert!(
				wire.sent(ietf::RequestError::ID).is_some(),
				"{version:?}: the update is refused"
			);
			assert!(
				wire.sent(ietf::RequestOk::ID).is_none(),
				"{version:?}: nothing acknowledged it"
			);
			let (status, _) = wire.publish_done.expect("PUBLISH_DONE was sent");
			assert_eq!(status, ietf::PublishDoneStatus::UpdateFailed.code(), "{version:?}");
		}
	}

	/// One REQUEST_UPDATE carrying a fresh token, keyed to its own Request ID so several can be
	/// told apart on the wire.
	async fn subscribe_update_token_rid(version: Version, rid: u64) -> Vec<u8> {
		let log = crate::lite::test_transport::Log::default();
		let mut writer = crate::coding::Writer::new(
			crate::transport::boxed::SendStream::new(crate::lite::test_transport::SinkSend::new(log.clone())),
			version,
		);
		let msg = ietf::SubscribeUpdate {
			max_age: None,
			new_group: None,
			request_id: RequestId(rid),
			subscriber_priority: Some(128),
			forward: Some(true),
			filter: None,
			authorization_token: Some(request_token()),
			fill: None,
		};
		writer.varint(ietf::SubscribeUpdate::ID).await.unwrap();
		writer.encode(&msg).await.unwrap();
		log.writes.lock().unwrap().clone()
	}

	/// One token-less REQUEST_UPDATE (a priority or forward change) keyed to `rid`, framed as the
	/// peer sends it. It carries no AUTHORIZATION TOKEN, so it starts no verify and is answered at
	/// once rather than held.
	async fn subscribe_update_bare_rid(version: Version, rid: u64) -> Vec<u8> {
		let log = crate::lite::test_transport::Log::default();
		let mut writer = crate::coding::Writer::new(
			crate::transport::boxed::SendStream::new(crate::lite::test_transport::SinkSend::new(log.clone())),
			version,
		);
		let msg = ietf::SubscribeUpdate {
			max_age: None,
			new_group: None,
			request_id: RequestId(rid),
			subscriber_priority: Some(128),
			forward: Some(true),
			filter: None,
			authorization_token: None,
			fill: None,
		};
		writer.varint(ietf::SubscribeUpdate::ID).await.unwrap();
		writer.encode(&msg).await.unwrap();
		log.writes.lock().unwrap().clone()
	}

	/// Two token renewals that arrive while a first renewal's verdict is still pending are each
	/// verified and answered, not collapsed. The loop keeps per-update state while a verify is
	/// pending (draft-18 section 10.9.1 answers each update); the single slot it replaced
	/// dropped the middle renewal (0x42) entirely, so its token was never verified.
	#[moq_clock::test]
	async fn two_renewals_buffered_behind_a_pending_verdict_each_get_a_verdict() {
		const VERSION: Version = Version::Draft18;

		let (auth, mut requests, _cred) = auth_covering_other();
		let mut script = subscribe_update_token_rid(VERSION, 0x40).await;
		script.extend(subscribe_update_token_rid(VERSION, 0x42).await);
		script.extend(subscribe_update_token_rid(VERSION, 0x44).await);
		let h = serve_with_auth(VERSION, auth, script);
		let rt = h.publisher.runtime.clone();

		let mut group = h.track.create_group(group::Info { sequence: 0 }).unwrap();
		group.write_frame(timestamp(), b"frame".as_slice()).unwrap();
		group.finish().unwrap();
		settle().await;

		let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), VERSION)
			.await
			.unwrap();

		// Hold the first renewal (0x40) so the next two (0x42, 0x44) are read and buffered while
		// its verdict is pending: the exact window the single slot used to collapse.
		let release = std::sync::Arc::new(Release::default());
		let answered = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
		let acceptor = {
			let answered = answered.clone();
			let release = release.clone();
			async move {
				let first = requests.next().await.unwrap();
				let mut held = vec![first.accept(grant_all_expiring(&rt, None))];
				answered.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
				let a = requests.next().await.unwrap();
				release.notified().await;
				held.push(a.accept(grant_all_expiring(&rt, None)));
				answered.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
				// 0x42 then 0x44: reached only if neither was dropped from the buffer.
				loop {
					let renewal = requests.next().await.unwrap();
					held.push(renewal.accept(grant_all_expiring(&rt, None)));
					answered.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
				}
			}
		};
		let mut acceptor = std::pin::pin!(acceptor);
		let mut serving = std::pin::pin!(h.publisher.clone().run_subscribe_stream(stream, token_subscribe()));

		// Let 0x42 and 0x44 buffer behind the held 0x40.
		for _ in 0..200 {
			let _ = futures::poll!(acceptor.as_mut());
			assert!(futures::poll!(serving.as_mut()).is_pending(), "ended during setup");
			settle().await;
		}
		release.notify_one();
		for _ in 0..500 {
			let _ = futures::poll!(acceptor.as_mut());
			assert!(futures::poll!(serving.as_mut()).is_pending());
			if answered.load(std::sync::atomic::Ordering::Relaxed) >= 4 {
				break;
			}
			settle().await;
		}
		assert_eq!(
			answered.load(std::sync::atomic::Ordering::Relaxed),
			4,
			"the initial token and all three renewals must each be verified; the single slot dropped 0x42"
		);
	}

	/// Draft-22 advertises MAX_REQUEST_UPDATES, so a peer that leaves more outstanding than that
	/// broke the negotiated limit: draft-22 section 9.1.7 closes the session with
	/// TOO_MANY_REQUEST_UPDATES.
	#[moq_clock::test]
	async fn renewals_past_the_advertised_limit_close_the_session() {
		const VERSION: Version = Version::Draft22;

		let (auth, mut requests, _cred) = auth_covering_other();
		// Exactly one past the limit: the held renewal plus MAX_REQUEST_UPDATES queued behind it
		// is the (limit + 1)th outstanding, the one that must close the session.
		let last = 0x40 + 2 * request_update::MAX_REQUEST_UPDATES;
		let mut script = Vec::new();
		for rid in (0x40..=last).step_by(2) {
			script.extend(subscribe_update_token_rid(VERSION, rid).await);
		}
		let h = serve_with_auth(VERSION, auth, script);
		let rt = h.publisher.runtime.clone();

		let mut group = h.track.create_group(group::Info { sequence: 0 }).unwrap();
		group.write_frame(timestamp(), b"frame".as_slice()).unwrap();
		group.finish().unwrap();
		settle().await;

		let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), VERSION)
			.await
			.unwrap();

		// Accept the initial token, then never answer a renewal, so the buffer only grows.
		let acceptor = async move {
			let first = requests.next().await.unwrap();
			let _held = first.accept(grant_all_expiring(&rt, None));
			std::future::pending::<()>().await
		};
		let mut acceptor = std::pin::pin!(acceptor);
		let mut serving = std::pin::pin!(h.publisher.clone().run_subscribe_stream(stream, token_subscribe()));

		let mut ended = false;
		for _ in 0..2000 {
			let _ = futures::poll!(acceptor.as_mut());
			if futures::poll!(serving.as_mut()).is_ready() {
				ended = true;
				break;
			}
			settle().await;
		}
		assert!(ended, "flooding past the advertised limit must end the request");
		assert!(
			h.log
				.closes()
				.iter()
				.any(|close| close.0 == crate::SessionError::TooManyRequestUpdates.to_code()),
			"the flood must close the session with TOO_MANY_REQUEST_UPDATES: {:?}",
			h.log.closes()
		);
	}

	/// Exactly MAX_REQUEST_UPDATES outstanding (the one being verified plus the queue) is within
	/// the advertised limit, so a draft-22 peer holding that many is not faulted: the request
	/// keeps running and the session stays up.
	#[moq_clock::test]
	async fn renewals_at_the_advertised_limit_keep_the_request() {
		const VERSION: Version = Version::Draft18;

		let (auth, mut requests, _cred) = auth_covering_other();
		// The held renewal plus MAX_REQUEST_UPDATES - 1 queued behind it is exactly the limit.
		let last = 0x40 + 2 * (request_update::MAX_REQUEST_UPDATES - 1);
		let mut script = Vec::new();
		for rid in (0x40..=last).step_by(2) {
			script.extend(subscribe_update_token_rid(VERSION, rid).await);
		}
		let h = serve_with_auth(VERSION, auth, script);
		let rt = h.publisher.runtime.clone();

		let mut group = h.track.create_group(group::Info { sequence: 0 }).unwrap();
		group.write_frame(timestamp(), b"frame".as_slice()).unwrap();
		group.finish().unwrap();
		settle().await;

		let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), VERSION)
			.await
			.unwrap();

		let acceptor = async move {
			let first = requests.next().await.unwrap();
			let _held = first.accept(grant_all_expiring(&rt, None));
			std::future::pending::<()>().await
		};
		let mut acceptor = std::pin::pin!(acceptor);
		let mut serving = std::pin::pin!(h.publisher.clone().run_subscribe_stream(stream, token_subscribe()));

		for _ in 0..2000 {
			let _ = futures::poll!(acceptor.as_mut());
			assert!(
				futures::poll!(serving.as_mut()).is_pending(),
				"the request must stay up at exactly the advertised limit"
			);
			settle().await;
		}
		assert!(
			h.log.closes().is_empty(),
			"no session close at the limit: {:?}",
			h.log.closes()
		);
	}

	/// Drafts below 19 carry no MAX_REQUEST_UPDATES option, so a peer there agreed to no ceiling:
	/// the same flood that closes a draft-22 session must neither end the request nor close the
	/// session on draft-18, because a conforming peer must not be stranded for a limit it never saw.
	#[moq_clock::test]
	async fn older_drafts_do_not_strand_a_renewal_flood() {
		const VERSION: Version = Version::Draft18;

		let (auth, mut requests, _cred) = auth_covering_other();
		let mut script = Vec::new();
		for rid in (0x40..=0x80u64).step_by(2) {
			script.extend(subscribe_update_token_rid(VERSION, rid).await);
		}
		let h = serve_with_auth(VERSION, auth, script);
		let rt = h.publisher.runtime.clone();

		let mut group = h.track.create_group(group::Info { sequence: 0 }).unwrap();
		group.write_frame(timestamp(), b"frame".as_slice()).unwrap();
		group.finish().unwrap();
		settle().await;

		let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), VERSION)
			.await
			.unwrap();

		let acceptor = async move {
			let first = requests.next().await.unwrap();
			let _held = first.accept(grant_all_expiring(&rt, None));
			std::future::pending::<()>().await
		};
		let mut acceptor = std::pin::pin!(acceptor);
		let mut serving = std::pin::pin!(h.publisher.clone().run_subscribe_stream(stream, token_subscribe()));

		for _ in 0..2000 {
			let _ = futures::poll!(acceptor.as_mut());
			assert!(
				futures::poll!(serving.as_mut()).is_pending(),
				"draft-18 negotiates no limit, so the flood must not strand the request"
			);
			settle().await;
		}
		assert!(h.log.closes().is_empty(), "draft-18 flood must not close the session");
	}

	/// Every REQUEST_UPDATE owes one answer, token or not (moq-transport section 10.9), and that
	/// answer restores one MAX_REQUEST_UPDATES credit. A peer that only sends token-less priority
	/// updates must keep its credit so it can still renew, so each gets a REQUEST_OK and the
	/// session stays up well past the advertised limit.
	#[moq_clock::test]
	async fn token_less_updates_are_each_acknowledged() {
		const VERSION: Version = Version::Draft18;

		let (auth, mut requests, _cred) = auth_covering_other();
		// More than the advertised limit, none carrying a token: none starts a verify, so none is
		// ever outstanding.
		let count = request_update::MAX_REQUEST_UPDATES + 4;
		let mut script = Vec::new();
		for rid in (0x40..0x40 + 2 * count).step_by(2) {
			script.extend(subscribe_update_bare_rid(VERSION, rid).await);
		}
		let h = serve_with_auth(VERSION, auth, script);
		let rt = h.publisher.runtime.clone();

		let mut group = h.track.create_group(group::Info { sequence: 0 }).unwrap();
		group.write_frame(timestamp(), b"frame".as_slice()).unwrap();
		group.finish().unwrap();
		settle().await;

		let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), VERSION)
			.await
			.unwrap();

		let acceptor = async move {
			let first = requests.next().await.unwrap();
			let _held = first.accept(grant_all_expiring(&rt, None));
			std::future::pending::<()>().await
		};
		let mut acceptor = std::pin::pin!(acceptor);
		let mut serving = std::pin::pin!(h.publisher.clone().run_subscribe_stream(stream, token_subscribe()));

		for _ in 0..2000 {
			let _ = futures::poll!(acceptor.as_mut());
			assert!(
				futures::poll!(serving.as_mut()).is_pending(),
				"token-less updates must not strand the request"
			);
			settle().await;
		}

		// Draft-22 answers each with a REQUEST_OK, request id omitted.
		let one_ok = {
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
			log.writes.lock().unwrap().clone()
		};
		let written = h.log.writes.lock().unwrap().clone();
		let acks = written
			.windows(one_ok.len())
			.filter(|w| *w == one_ok.as_slice())
			.count();
		assert_eq!(
			acks, count as usize,
			"each token-less update must get exactly one REQUEST_OK"
		);
		assert!(
			h.log.closes().is_empty(),
			"token-less updates must not close the session"
		);
	}

	/// An update that arrives while a renewal's verdict is pending is answered after the
	/// renewal, never before it: an answer names no Request ID, so its order is
	/// all that ties it to its update. The renewal is refused, so its REQUEST_ERROR is the first
	/// answer either way, and the token-less update behind it is never answered ahead of it.
	#[moq_clock::test]
	async fn an_update_behind_a_pending_renewal_is_answered_in_order() {
		for version in [Version::Draft18] {
			let (auth, mut requests, _cred) = auth_covering_other();
			let mut script = subscribe_update_token_rid(version, 0x40).await;
			script.extend(subscribe_update_bare_rid(version, 0x42).await);
			let h = serve_with_auth(version, auth, script);
			let rt = h.publisher.runtime.clone();

			let mut group = h.track.create_group(group::Info { sequence: 0 }).unwrap();
			group.write_frame(timestamp(), b"frame".as_slice()).unwrap();
			group.finish().unwrap();
			settle().await;

			let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), version)
				.await
				.unwrap();
			let release = Release::default();
			let acceptor = {
				let release = release.clone();
				async move {
					let first = requests.next().await.unwrap();
					let _held = first.accept(grant_all_expiring(&rt, None));
					let renewal = requests.next().await.unwrap();
					release.notified().await;
					renewal.reject(crate::SessionError::Unauthorized, "no");
					std::future::pending::<()>().await
				}
			};
			let mut acceptor = std::pin::pin!(acceptor);
			let mut serving = std::pin::pin!(h.publisher.clone().run_subscribe_stream(stream, token_subscribe()));

			// Both updates are read while the renewal is held.
			for _ in 0..200 {
				let _ = futures::poll!(acceptor.as_mut());
				assert!(futures::poll!(serving.as_mut()).is_pending(), "{version}: ended early");
				settle().await;
			}
			let ok = request_ok_bytes(version).await;
			let held = h.log.writes.lock().unwrap().clone();
			assert!(
				!held.windows(ok.len()).any(|window| window == ok.as_slice()),
				"{version}: the update behind the pending renewal was answered first"
			);
			let before = held.len();

			release.notify_one();
			for _ in 0..200 {
				let _ = futures::poll!(acceptor.as_mut());
				if futures::poll!(serving.as_mut()).is_ready() {
					break;
				}
				settle().await;
			}
			let written = h.log.writes.lock().unwrap()[before..].to_vec();
			let error = request_error_bytes(version, "no").await;
			assert!(
				written.starts_with(&error),
				"{version}: the renewal's REQUEST_ERROR is the first answer"
			);
		}
	}

	/// On drafts without the option the guard is a memory backstop, not a protocol limit: a flood
	/// past it ends the one request through PUBLISH_DONE, never the session.
	#[moq_clock::test]
	async fn an_older_draft_flood_past_the_guard_ends_the_request() {
		const VERSION: Version = Version::Draft18;

		let (auth, mut requests, _cred) = auth_covering_other();
		let last = 0x40 + 2 * (request_update::UNNEGOTIATED_GUARD as u64 + 1);
		let mut script = Vec::new();
		for rid in (0x40..=last).step_by(2) {
			script.extend(subscribe_update_token_rid(VERSION, rid).await);
		}
		let h = serve_with_auth(VERSION, auth, script);
		let rt = h.publisher.runtime.clone();

		let mut group = h.track.create_group(group::Info { sequence: 0 }).unwrap();
		group.write_frame(timestamp(), b"frame".as_slice()).unwrap();
		group.finish().unwrap();
		settle().await;

		let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), VERSION)
			.await
			.unwrap();

		let acceptor = async move {
			let first = requests.next().await.unwrap();
			let _held = first.accept(grant_all_expiring(&rt, None));
			std::future::pending::<()>().await
		};
		let mut acceptor = std::pin::pin!(acceptor);
		let mut serving = std::pin::pin!(h.publisher.clone().run_subscribe_stream(stream, token_subscribe()));

		let mut ended = false;
		for _ in 0..4000 {
			let _ = futures::poll!(acceptor.as_mut());
			if futures::poll!(serving.as_mut()).is_ready() {
				ended = true;
				break;
			}
			settle().await;
		}
		assert!(ended, "a flood past the guard must end the request");
		assert!(h.log.closes().is_empty(), "the guard ends the request, not the session");
	}

	/// A peer that ends the subscription while a renewal is still being verified ends it here
	/// too: with a grant that never expires and an acceptor sitting on the renewal, the stream
	/// closing drops the subscription's demand at once. The request keeps its stream, and so its
	/// credit, until the acceptor lets go of the renewal.
	#[moq_clock::test]
	async fn a_closed_subscription_ends_while_a_renewal_is_pending() {
		const VERSION: Version = Version::Draft18;

		let (auth, requests, _cred) = auth_covering_other();
		let session = ScriptedSession::new(subscribe_update_with_token(VERSION, true).await);
		let h = serve_on(VERSION, auth, session.clone());
		let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), VERSION)
			.await
			.unwrap();
		let cancel = || session.close(crate::lite::test_transport::Close::Fin);
		ends_while_a_renewal_is_pending(&h, requests, stream, cancel, "draft-18").await;
	}

	/// Serve a token-authorized SUBSCRIBE, and `cancel` it once the acceptor holds a renewal. Its
	/// demand ends at once; the request ends only once the renewal is dropped.
	async fn ends_while_a_renewal_is_pending(
		h: &Serve,
		mut requests: crate::auth::Requests,
		stream: Stream<Transport, Version>,
		cancel: impl FnOnce(),
		label: &str,
	) {
		let mut group = h.track.create_group(group::Info { sequence: 0 }).unwrap();
		group.write_frame(timestamp(), b"frame".as_slice()).unwrap();
		group.finish().unwrap();
		settle().await;

		let renewal = std::sync::Arc::new(std::sync::Mutex::new(None));
		let acceptor = {
			let renewal = renewal.clone();
			async move {
				let first = requests.next().await.expect("a request");
				let _issued = first.accept(crate::auth::Grant::all());
				*renewal.lock().unwrap() = Some(requests.next().await.expect("a renewal"));
				std::future::pending::<()>().await
			}
		};
		let mut acceptor = std::pin::pin!(acceptor);
		let mut serving = std::pin::pin!(h.publisher.clone().run_subscribe_stream(stream, token_subscribe()));
		for _ in 0..500 {
			let _ = futures::poll!(acceptor.as_mut());
			assert!(
				futures::poll!(serving.as_mut()).is_pending(),
				"{label}: ended before the cancel"
			);
			if renewal.lock().unwrap().is_some() {
				break;
			}
			settle().await;
		}
		assert!(
			renewal.lock().unwrap().is_some(),
			"{label}: the acceptor never saw the renewal"
		);
		assert!(
			h.track.subscription().is_some(),
			"{label}: the subscription is not serving"
		);

		cancel();
		for _ in 0..10 {
			assert!(
				futures::poll!(serving.as_mut()).is_pending(),
				"{label}: the request let go of its stream while the acceptor held the renewal"
			);
			settle().await;
		}
		assert!(
			h.track.subscription().is_none(),
			"{label}: the cancel did not end the subscription's demand"
		);

		drop(renewal.lock().unwrap().take());
		let mut ended = false;
		for _ in 0..10 {
			if futures::poll!(serving.as_mut()).is_ready() {
				ended = true;
				break;
			}
			settle().await;
		}
		assert!(ended, "{label}: dropping the renewal ends the request");
	}

	/// A REQUEST_UPDATE the acceptor refuses ends only that subscription, at once, through the
	/// PUBLISH_DONE UPDATE_FAILED path (draft-18 section 10.9.1). The old
	/// grant does not keep it alive to its 60s expiry, and the session stays up.
	#[moq_clock::test]
	async fn a_refused_request_update_ends_the_subscription() {
		const VERSION: Version = Version::Draft18;

		let (auth, mut requests, _cred) = auth_covering_other();
		let h = serve_with_auth(VERSION, auth, subscribe_update_with_token(VERSION, true).await);
		let rt = h.publisher.runtime.clone();

		let mut group = h.track.create_group(group::Info { sequence: 0 }).unwrap();
		group.write_frame(timestamp(), b"frame".as_slice()).unwrap();
		group.finish().unwrap();
		settle().await;

		let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), VERSION)
			.await
			.unwrap();

		let answered = std::sync::Arc::new(AtomicU64::new(0));
		let acceptor = {
			let answered = answered.clone();
			async move {
				// The first grant lasts 60s; the renewal is refused.
				let first = requests.next().await.unwrap();
				let _issued = first.accept(grant_all_expiring(&rt, Some(60)));
				answered.fetch_add(1, Ordering::Relaxed);
				let renewal = requests.next().await.unwrap();
				renewal.reject(crate::SessionError::Unauthorized, "no");
				answered.fetch_add(1, Ordering::Relaxed);
				std::future::pending::<()>().await
			}
		};
		let mut acceptor = std::pin::pin!(acceptor);
		let mut serving = std::pin::pin!(h.publisher.clone().run_subscribe_stream(stream, token_subscribe()));

		// No time is advanced: the refusal must end the subscription on its own, not by letting
		// the 60s grant lapse.
		let mut result = None;
		for _ in 0..500 {
			let _ = futures::poll!(acceptor.as_mut());
			if let Poll::Ready(res) = futures::poll!(serving.as_mut()) {
				result = Some(res);
				break;
			}
			settle().await;
		}
		assert_eq!(
			answered.load(Ordering::Relaxed),
			2,
			"acceptor never answered both tokens"
		);
		assert!(
			matches!(result, Some(Err(Error::Session(crate::SessionError::Unauthorized)))),
			"a refused renewal ends the subscription at once, got {result:?}"
		);
		let (status, _) = Wire::read(&h.log, VERSION).publish_done.expect("PUBLISH_DONE was sent");
		assert_eq!(status, ietf::PublishDoneStatus::UpdateFailed.code(), "UPDATE_FAILED");
		// The refused update is answered with exactly one REQUEST_ERROR before the subscription
		// ends.
		let written = h.log.writes.lock().unwrap().clone();
		let error = request_error_bytes(VERSION, "no").await;
		assert_eq!(
			written.windows(error.len()).filter(|w| *w == error.as_slice()).count(),
			1,
			"exactly one unkeyed REQUEST_ERROR before the subscription ends"
		);
		assert!(
			h.log.closes().is_empty(),
			"the session must stay up: {:?}",
			h.log.closes()
		);
	}

	/// A slow acceptor answering a renewal must not stall serving nor let the request outlive
	/// its grant: the renewal verify is raced against the old deadline, which still fires and
	/// sends PUBLISH_DONE. The stream keeps its credit until the acceptor lets go of the renewal.
	#[moq_clock::test]
	async fn a_slow_renewal_does_not_stall_serving() {
		const VERSION: Version = Version::Draft18;
		let (auth, mut requests, _cred) = auth_covering_other();
		let h = serve_with_auth(VERSION, auth, subscribe_update_with_token(VERSION, true).await);
		let rt = h.publisher.runtime.clone();

		let mut group = h.track.create_group(group::Info { sequence: 0 }).unwrap();
		group.write_frame(timestamp(), b"frame".as_slice()).unwrap();
		group.finish().unwrap();
		settle().await;

		let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), VERSION)
			.await
			.unwrap();

		let popped = std::sync::Arc::new(AtomicU64::new(0));
		let acceptor = {
			let popped = popped.clone();
			async move {
				let mut held = Vec::new();
				// The first grant lapses in 60s.
				let first = requests.next().await.expect("a request");
				held.push(first.accept(grant_all_expiring(&rt, Some(60))));
				popped.fetch_add(1, Ordering::Relaxed);
				// The renewal is popped but never answered: a slow or hung acceptor.
				let _slow = requests.next().await.expect("a renewal");
				popped.fetch_add(1, Ordering::Relaxed);
				std::future::pending::<()>().await
			}
		};
		let mut acceptor = Box::pin(acceptor);
		let mut serving = std::pin::pin!(h.publisher.clone().run_subscribe_stream(stream, token_subscribe()));

		// Let the loop read the REQUEST_UPDATE and the acceptor pop both requests.
		for _ in 0..500 {
			let _ = futures::poll!(acceptor.as_mut());
			assert!(
				futures::poll!(serving.as_mut()).is_pending(),
				"the subscription ended during setup"
			);
			if popped.load(Ordering::Relaxed) >= 2 {
				break;
			}
			settle().await;
		}
		assert_eq!(popped.load(Ordering::Relaxed), 2, "the acceptor never saw the renewal");

		// The acceptor is stuck. Past the old 60s expiry the deadline must still fire and end
		// the request EXPIRED, rather than the pending renewal stalling serving.
		moq_clock::sim::advance(Duration::from_secs(120)).await;
		let done = {
			let log = Log::default();
			let mut writer = crate::coding::Writer::new(
				crate::transport::boxed::SendStream::new(crate::lite::test_transport::SinkSend::new(log.clone())),
				VERSION,
			);
			writer.varint(ietf::PublishDone::ID).await.unwrap();
			log.writes.lock().unwrap().clone()
		};
		let request = |sent: &crate::lite::test_transport::Sent| h.log.trail().contains(&(0, sent.clone()));
		// PUBLISH_DONE goes out whole, so its write leads with the type.
		let sent_done = || {
			h.log.trail().iter().any(|(stream, sent)| {
				*stream == 0
					&& matches!(sent, crate::lite::test_transport::Sent::Write(bytes) if bytes.starts_with(&done))
			})
		};
		for _ in 0..50 {
			let _ = futures::poll!(acceptor.as_mut());
			assert!(
				futures::poll!(serving.as_mut()).is_pending(),
				"the request let go of its stream while the acceptor held the renewal"
			);
			if sent_done() {
				break;
			}
			settle().await;
		}
		assert!(
			sent_done(),
			"serving must end at the old deadline, not stall on the slow renewal"
		);
		assert!(
			!request(&crate::lite::test_transport::Sent::Finish),
			"the stream closed while the acceptor held the renewal"
		);

		drop(acceptor);
		let mut ended = None;
		for _ in 0..10 {
			if let Poll::Ready(res) = futures::poll!(serving.as_mut()) {
				ended = Some(res);
				break;
			}
			settle().await;
		}
		let res = ended.expect("dropping the renewal ends the request");
		assert!(matches!(res, Err(Error::Expired)), "{res:?}");
	}

	/// Publisher / SUBSCRIBE side: on a session without the AUTH extension the union is
	/// `None` forever. A token-bearing SUBSCRIBE the limit does not cover is verified by the
	/// application, not admitted by the permissive union.
	#[moq_clock::test]
	async fn a_request_token_on_a_no_auth_session_is_verified() {
		const VERSION: Version = Version::Draft18;
		let auth = crate::auth::Link::new(false);
		let mut requests = auth.handle().requests();
		auth.handle().authorize(&crate::auth::Grant::default());

		let h = serve_with_auth(VERSION, auth, Vec::new());
		let mut group = h.track.create_group(group::Info { sequence: 0 }).unwrap();
		group.write_frame(timestamp(), b"frame".as_slice()).unwrap();
		group.finish().unwrap();
		settle().await;

		let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), VERSION)
			.await
			.unwrap();
		let consulted = std::sync::Arc::new(AtomicU64::new(0));
		let acceptor = {
			let consulted = consulted.clone();
			async move {
				let mut held = Vec::new();
				loop {
					let request = requests.next().await.expect("a request");
					consulted.fetch_add(1, Ordering::Relaxed);
					held.push(request.accept(crate::auth::Grant::all()));
				}
			}
		};
		let mut acceptor = std::pin::pin!(acceptor);
		let mut serving = std::pin::pin!(h.publisher.clone().run_subscribe_stream(stream, token_subscribe()));
		for _ in 0..500 {
			let _ = futures::poll!(acceptor.as_mut());
			assert!(
				futures::poll!(serving.as_mut()).is_pending(),
				"the subscription ended early"
			);
			if consulted.load(Ordering::Relaxed) >= 1 {
				break;
			}
			settle().await;
		}
		assert!(
			consulted.load(Ordering::Relaxed) >= 1,
			"the token must be verified by the acceptor, not admitted by the permissive default"
		);
	}

	/// A token-bearing SUBSCRIBE being served on a no-AUTH session whose acceptor answers every
	/// token with `grant`, after `limit` (if any) narrowed what the peer may do.
	type Serving = std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), Error>>>>;

	async fn serve_token_subscribe(
		grant: crate::auth::Grant,
		limit: Option<crate::auth::Grant>,
	) -> (crate::auth::Link, Serve, Serving) {
		const VERSION: Version = Version::Draft18;
		let auth = crate::auth::Link::new(false);
		let mut requests = auth.handle().requests();
		if let Some(limit) = &limit {
			auth.handle().authorize(limit);
		}
		let h = serve_with_auth(VERSION, auth.clone(), Vec::new());
		let mut group = h.track.create_group(group::Info { sequence: 0 }).unwrap();
		group.write_frame(timestamp(), b"frame".as_slice()).unwrap();
		group.finish().unwrap();
		settle().await;

		let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), VERSION)
			.await
			.unwrap();
		let serving = h.publisher.clone().run_subscribe_stream(stream, token_subscribe());
		let serving: Serving = Box::pin(async move {
			let acceptor = async move {
				let mut held = Vec::new();
				while let Some(request) = requests.next().await {
					held.push(request.accept(grant.clone()));
				}
				std::future::pending::<Result<(), Error>>().await
			};
			let acceptor = std::pin::pin!(acceptor);
			let serving = std::pin::pin!(serving);
			match futures::future::select(acceptor, serving).await {
				futures::future::Either::Left((res, _)) | futures::future::Either::Right((res, _)) => res,
			}
		});
		(auth, h, serving)
	}

	/// Whether `serving` is still running after the acceptor and the serve loop have had their
	/// turns.
	async fn still_serving(serving: &mut Serving) -> bool {
		for _ in 0..300 {
			if futures::poll!(serving.as_mut()).is_ready() {
				return false;
			}
			settle().await;
		}
		true
	}

	/// A subscriber's token grant is checked on its `subscribe` patterns: a write-only grant
	/// does not admit a SUBSCRIBE, a read-only one does.
	#[moq_clock::test]
	async fn a_subscribe_token_needs_a_subscribe_grant() {
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
		let nothing = Some(crate::auth::Grant::default());
		let (_auth, _h, mut serving) = serve_token_subscribe(read_only, nothing.clone()).await;
		assert!(still_serving(&mut serving).await, "a read grant admits it");
		let (_auth, _h, mut serving) = serve_token_subscribe(write_only, nothing).await;
		assert!(!still_serving(&mut serving).await, "a write grant refuses it");
	}

	/// A request token stands in for the limit this side set on the peer, so a SUBSCRIBE the
	/// limit refuses is admitted on the token and a later narrowing leaves it standing.
	#[moq_clock::test]
	async fn a_request_token_stands_in_for_the_local_limit() {
		let nothing = Some(crate::auth::Grant::default());
		let (auth, _h, mut serving) = serve_token_subscribe(crate::auth::Grant::all(), nothing).await;
		assert!(
			still_serving(&mut serving).await,
			"a limit of nothing defers to the token"
		);
		auth.handle().authorize(&crate::auth::Grant {
			publish: crate::Patterns::new(),
			subscribe: crate::Pattern::subtree("other").unwrap().into(),
			expires: None,
		});
		assert!(
			still_serving(&mut serving).await,
			"narrowing the limit leaves a token-authorized subscription"
		);
	}

	/// What the peer's AUTH_OK grants us never authorizes what the peer asks of us: with a
	/// covering peer grant and no limit, the application still verifies the SUBSCRIBE's token,
	/// and its refusal refuses the subscription.
	#[moq_clock::test]
	async fn a_covering_peer_grant_never_skips_the_verifier() {
		const VERSION: Version = Version::Draft18;
		let auth = crate::auth::Link::new(true);
		let mut requests = auth.handle().requests();
		let _cred = auth.present(bytes::Bytes::from_static(b"cred"), true).unwrap();
		auth.granted(0, crate::auth::Grant::all());
		let h = serve_with_auth(VERSION, auth, Vec::new());
		let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), VERSION)
			.await
			.unwrap();

		let consulted = std::sync::Arc::new(AtomicU64::new(0));
		let acceptor = {
			let consulted = consulted.clone();
			async move {
				let request = requests.next().await.expect("a request");
				consulted.fetch_add(1, Ordering::Relaxed);
				request.reject(crate::SessionError::Unauthorized, "no");
				std::future::pending::<()>().await
			}
		};
		let mut acceptor = std::pin::pin!(acceptor);
		let mut serving = std::pin::pin!(h.publisher.clone().run_subscribe_stream(stream, token_subscribe()));
		let mut ended = false;
		for _ in 0..500 {
			let _ = futures::poll!(acceptor.as_mut());
			if futures::poll!(serving.as_mut()).is_ready() {
				ended = true;
				break;
			}
			settle().await;
		}
		assert_eq!(consulted.load(Ordering::Relaxed), 1, "the verifier ran");
		assert!(ended, "the refused subscription ends");
	}

	/// Every token is verified, even on a SUBSCRIBE the limit already covers: the verifier
	/// refusing it refuses the subscription, and one it grants stands outside the limit, so
	/// narrowing the limit away from it leaves the subscription its token admitted.
	#[moq_clock::test]
	async fn a_covering_limit_still_verifies_the_token() {
		let limit = |subtree: &str| crate::auth::Grant {
			publish: crate::Patterns::new(),
			subscribe: crate::Pattern::subtree(subtree).unwrap().into(),
			expires: None,
		};
		let (_auth, _h, mut serving) = serve_token_subscribe(crate::auth::Grant::default(), Some(limit("room"))).await;
		assert!(
			!still_serving(&mut serving).await,
			"the verifier refused the token the limit would have let through"
		);

		let (auth, _h, mut serving) = serve_token_subscribe(crate::auth::Grant::all(), Some(limit("room"))).await;
		assert!(still_serving(&mut serving).await, "the verified token admits it");
		auth.handle().authorize(&limit("other"));
		assert!(
			still_serving(&mut serving).await,
			"narrowing the limit leaves the token-verified subscription"
		);
	}

	/// Where tokens decide, a token arriving on an update for a subscription admitted without
	/// one is refused NOT_SUPPORTED rather than taken unread, failing only that subscription.
	#[moq_clock::test]
	async fn a_token_on_a_subscription_admitted_without_one_is_refused() {
		for version in [Version::Draft22] {
			let auth = crate::auth::Link::new(false);
			auth.handle().authorize(&crate::auth::Grant::all());
			let h = serve_with_auth(version, auth, Vec::new());
			let session = ScriptedSession::per_stream(vec![update_with_tokens(&[TOKEN])]);
			let mut stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), version)
				.await
				.unwrap();
			let mut goaways = super::super::request_stream::Messages::new(
				crate::driver::Transport::new(h.session.clone()),
				h.publisher.control.client(),
			);
			let mut serving = TrackServe::new(
				crate::driver::Transport::new(h.session.clone()),
				h.track.subscribe(None),
				RequestId(0),
				version,
				ServeRange::default(),
				None,
			);
			let mut finished = false;
			let mut run = std::pin::pin!(h.publisher.run_subscription(
				&mut stream,
				&mut serving,
				&mut finished,
				&mut goaways,
				async {},
				None
			));
			let mut served = None;
			for _ in 0..50 {
				if let std::task::Poll::Ready(done) = futures::poll!(run.as_mut()) {
					served = Some(done);
					break;
				}
				settle().await;
			}
			assert!(
				matches!(served, Some(Some(Served::UpdateFailed(Error::Unsupported)))),
				"{version}: {served:?}"
			);
			assert!(h.log.closes().is_empty(), "{version}: the session closed");

			let wire = session.log.writes.lock().unwrap().clone();
			let mut buf = Decoder::new(&wire, version.into());
			assert_eq!(buf.varint().unwrap(), ietf::RequestError::ID, "{version}");
			let error = ietf::RequestError::decode(&mut buf, version).unwrap();
			assert_eq!(error.error_code, 0x3, "{version}");
		}
	}

	/// With no verifier, a limited session refuses a token as unsupported rather than
	/// unauthorized, even one its limit covers, and a session never limited ignores the token.
	#[moq_clock::test]
	async fn a_token_without_a_verifier_is_unsupported() {
		const VERSION: Version = Version::Draft18;
		for limit in [crate::auth::Grant::default(), crate::auth::Grant::all()] {
			a_token_without_a_verifier_is_refused(VERSION, limit).await;
		}

		let unlimited = crate::auth::Link::new(false);
		let h = serve_with_auth(VERSION, unlimited, Vec::new());
		let mut group = h.track.create_group(group::Info { sequence: 0 }).unwrap();
		group.write_frame(timestamp(), b"frame".as_slice()).unwrap();
		settle().await;
		let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), VERSION)
			.await
			.unwrap();
		let mut serving = std::pin::pin!(h.publisher.clone().run_subscribe_stream(stream, token_subscribe()));
		for _ in 0..50 {
			assert!(
				futures::poll!(serving.as_mut()).is_pending(),
				"an unlimited session serves it"
			);
			settle().await;
		}
	}

	/// Serve a token SUBSCRIBE on a session limited to `limit` with no verifier, and check it
	/// is refused NOT_SUPPORTED.
	async fn a_token_without_a_verifier_is_refused(version: Version, limit: crate::auth::Grant) {
		let auth = crate::auth::Link::new(false);
		auth.handle().authorize(&limit);
		let h = serve_with_auth(version, auth, Vec::new());
		let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), version)
			.await
			.unwrap();
		let mut serving = std::pin::pin!(h.publisher.clone().run_subscribe_stream(stream, token_subscribe()));
		let mut res = None;
		for _ in 0..50 {
			if let std::task::Poll::Ready(done) = futures::poll!(serving.as_mut()) {
				res = Some(done);
				break;
			}
			settle().await;
		}
		assert!(matches!(res, Some(Ok(()))), "refused, not failed or served: {res:?}");
		let refusal = {
			let log = Log::default();
			let mut writer = crate::coding::Writer::new(
				crate::transport::boxed::SendStream::new(crate::lite::test_transport::SinkSend::new(log.clone())),
				version,
			);
			writer.varint(ietf::RequestError::ID).await.unwrap();
			writer
				.encode(&ietf::RequestError {
					error_code: request::to_code(&Error::Unsupported, request::Kind::Subscribe, version),
					reason_phrase: Error::Unsupported.to_string().into(),
					retry_interval: 0,
				})
				.await
				.unwrap();
			log.writes.lock().unwrap().clone()
		};
		assert_eq!(occurrences(&h.log, &refusal), 1, "refused NOT_SUPPORTED");
	}

	/// A token waiting on the application holds the subscription's slot. Withdrawn while still
	/// queued it gives the slot back at once; once the application took it, the slot stays
	/// counted until the application answers, and that late answer authorizes nothing.
	#[moq_clock::test]
	async fn a_stalled_verification_holds_the_subscription_slot() {
		const VERSION: Version = Version::Draft18;

		// Withdrawn before the application looked.
		let auth = crate::auth::Link::new(false);
		let _requests = auth.handle().requests();
		let mut h = serve_with_auth(VERSION, auth, Vec::new());
		h.publisher.subscriptions = crate::session::Slots::new(1);
		let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), VERSION)
			.await
			.unwrap();
		{
			let mut serving = std::pin::pin!(h.publisher.clone().run_subscribe_stream(stream, token_subscribe()));
			for _ in 0..10 {
				assert!(futures::poll!(serving.as_mut()).is_pending(), "verification is pending");
				settle().await;
			}
			assert!(
				h.publisher.subscriptions.acquire().is_err(),
				"the pending token holds the slot"
			);
		}
		assert!(
			h.publisher.subscriptions.acquire().is_ok(),
			"a withdrawn queued token gives the slot back"
		);

		// Withdrawn after the application took it.
		let auth = crate::auth::Link::new(false);
		let mut requests = auth.handle().requests();
		let mut h = serve_with_auth(VERSION, auth, Vec::new());
		h.publisher.subscriptions = crate::session::Slots::new(1);
		let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), VERSION)
			.await
			.unwrap();
		let taken = {
			let mut serving = std::pin::pin!(h.publisher.clone().run_subscribe_stream(stream, token_subscribe()));
			let mut taken = None;
			for _ in 0..10 {
				assert!(futures::poll!(serving.as_mut()).is_pending(), "verification is pending");
				if taken.is_none()
					&& let Poll::Ready(Some(event)) = requests.poll_next(&kio::Waiter::noop())
				{
					taken = Some(event);
				}
				settle().await;
			}
			taken.expect("the application took the token")
		};
		assert!(
			h.publisher.subscriptions.acquire().is_err(),
			"a token the application holds keeps its slot after the request ends"
		);
		let late = taken.accept(crate::auth::Grant::all());
		assert!(
			h.publisher.subscriptions.acquire().is_ok(),
			"answering releases the slot"
		);
		let ended = late.closed().await;
		assert!(
			matches!(ended, Error::Cancel),
			"the late answer lands nowhere: {ended:?}"
		);
	}

	/// A peer that cancels a SUBSCRIBE whose token the application took keeps the request's
	/// stream, and so its credit, until the application answers: cancelling and resubscribing
	/// cannot leave more verifications in flight than the peer's credit admits.
	#[moq_clock::test]
	async fn a_cancelled_subscribe_keeps_its_stream_until_the_application_answers() {
		const VERSION: Version = Version::Draft18;
		let auth = crate::auth::Link::new(false);
		let mut requests = auth.handle().requests();
		let h = serve_with_auth(VERSION, auth, Vec::new());
		let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), VERSION)
			.await
			.unwrap();
		let mut serving = std::pin::pin!(h.publisher.clone().run_subscribe_stream(stream, token_subscribe()));
		let mut taken = None;
		for _ in 0..10 {
			assert!(futures::poll!(serving.as_mut()).is_pending(), "verification is pending");
			if taken.is_none()
				&& let Poll::Ready(Some(request)) = requests.poll_next(&kio::Waiter::noop())
			{
				taken = Some(request);
			}
			settle().await;
		}
		let taken = taken.expect("the application took the token");

		h.session.close(crate::lite::test_transport::Close::Reset);
		for _ in 0..10 {
			assert!(
				futures::poll!(serving.as_mut()).is_pending(),
				"the cancelled request let go of its stream before the application answered"
			);
			settle().await;
		}
		let ended = taken.closed().await;
		assert!(
			matches!(ended, Error::Cancel),
			"the application sees the cancel: {ended:?}"
		);

		drop(taken.accept(crate::auth::Grant::all()));
		let res = futures::poll!(serving.as_mut());
		assert!(
			matches!(res, Poll::Ready(Ok(()))),
			"answered, the request ends: {res:?}"
		);
	}

	/// What the peer granted us bounds what we send, so a request token never admits a
	/// SUBSCRIBE outside it, and the application is not even asked.
	#[moq_clock::test]
	async fn a_request_token_cannot_exceed_the_peers_grant() {
		const VERSION: Version = Version::Draft18;
		let auth = crate::auth::Link::new(true);
		let mut requests = auth.handle().requests();
		let _cred = auth.present(bytes::Bytes::from_static(b"cred"), true).unwrap();
		auth.granted(
			0,
			crate::auth::Grant {
				publish: crate::Pattern::subtree("other").unwrap().into(),
				subscribe: crate::Patterns::new(),
				expires: None,
			},
		);
		auth.handle().authorize(&crate::auth::Grant::default());
		let h = serve_with_auth(VERSION, auth, Vec::new());
		let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), VERSION)
			.await
			.unwrap();
		let res = h
			.publisher
			.clone()
			.run_subscribe_stream(stream, token_subscribe())
			.await;
		assert!(res.is_ok(), "refused, not failed: {res:?}");
		assert!(
			requests.poll_next(&kio::Waiter::noop()).is_pending(),
			"the token must not reach the application"
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

#[cfg(test)]
mod tests {
	use super::*;
	use crate::coding::Decode;
	use crate::ietf::GroupOrder;
	use crate::lite::test_transport::SinkSession;
	use crate::model::ProduceTest;
	use futures::FutureExt;

	async fn settle() {
		moq_clock::sleep(Duration::from_millis(1)).await;
	}

	fn occurrences(log: &crate::lite::test_transport::Log, needle: &[u8]) -> usize {
		let writes = log.writes.lock().unwrap();
		writes.windows(needle.len()).filter(|window| *window == needle).count()
	}

	/// NAMESPACE and NAMESPACE_DONE suffixes on a SUBSCRIBE_NAMESPACE stream, in order.
	fn namespace_events(bytes: &[u8], version: Version) -> Vec<(u64, String)> {
		let mut dec = crate::coding::Decoder::new(bytes, version.into());
		let mut out = Vec::new();
		while !dec.is_empty() {
			let id = dec.varint().expect("message type");
			match id {
				ietf::RequestOk::ID => {
					ietf::RequestOk::decode(&mut dec, version).expect("request ok");
				}
				ietf::Namespace::ID => {
					let msg = ietf::Namespace::decode(&mut dec, version).expect("namespace");
					out.push((id, msg.suffix.as_str().to_owned()));
				}
				ietf::NamespaceDone::ID => {
					let msg = ietf::NamespaceDone::decode(&mut dec, version).expect("namespace done");
					out.push((id, msg.suffix.as_str().to_owned()));
				}
				other => panic!("unexpected message {other:#x}"),
			}
		}
		out
	}

	/// A SETUP slot already filled with what the peer declared. The announce loops block
	/// on it, so a test that leaves it empty is a test that never advertises.
	fn declared(solicit: Option<bool>) -> peer::PeerSetup {
		let slot = peer::PeerSetup::default();
		slot.set(peer::Peer {
			solicit,
			..Default::default()
		});
		slot
	}

	/// An absent max age preserves everything the producer still retains.
	#[test]
	fn serving_subscription_keeps_retained_backlog() {
		let producer = track::Producer::new(
			std::sync::Arc::new(crate::broadcast::Info::default()),
			"video",
			crate::track::Info::timed(),
		);
		for millis in [0, 1000] {
			let mut group = producer.append_group().unwrap();
			group
				.write_frame(crate::Timestamp::from_millis(millis).unwrap(), b"frame".as_slice())
				.unwrap();
			group.finish().unwrap();
		}

		let subscription = serving_subscription(128);
		assert_eq!(
			subscription.max_delay, None,
			"no Max Age is the publisher's retention alone"
		);
		let mut subscriber = producer.subscribe(subscription);
		for sequence in [0, 1] {
			let group = subscriber
				.recv_group()
				.now_or_never()
				.expect("retained group should be ready")
				.unwrap()
				.expect("track should remain open");
			assert_eq!(group.sequence, sequence);
		}
	}

	/// A peer that requires solicitation, which is what hands the advertisements to the
	/// SUBSCRIBE_NAMESPACE stream.
	fn requires_solicitation() -> peer::PeerSetup {
		declared(Some(true))
	}

	/// A publisher for a peer assigned `assigned`, over an origin holding two
	/// broadcasts: `from/peer`, whose only route flows through `assigned`, and
	/// `from/us`, which does not. The producers are returned so the routes outlive
	/// the assertions.
	async fn echo_harness(assigned: crate::Hop) -> (Publisher, origin::Consumer, Vec<crate::model::AnnounceProducer>) {
		let other = crate::Hop::new(778).unwrap();
		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let consumer = origin.consume();

		let session = crate::lite::test_transport::SinkSession::new(Default::default());
		let publisher = Publisher::new(
			crate::time::sim(),
			crate::driver::Transport::new(session.clone()),
			origin.consume(),
			Control::new(false),
			Some(assigned),
			peer::PeerSetup::default(),
			Version::Draft18,
		);

		let mut echoed_hops = crate::Hops::new();
		echoed_hops.push(crate::Hop::UNKNOWN).unwrap();
		let echoed = origin
			.announce(
				"from/peer",
				crate::origin::Route::default()
					.with_hops(echoed_hops)
					.with_via(assigned),
			)
			.unwrap();

		let mut local_hops = crate::Hops::new();
		local_hops.push(other).unwrap();
		let local = origin
			.announce("from/us", crate::origin::Route::default().with_hops(local_hops))
			.unwrap();

		(publisher, consumer, vec![echoed, local])
	}

	/// A broadcast whose every route flows through the peer's assigned identity
	/// (`Client::with_peer_hop`) is never advertised to that peer; it would only
	/// echo the peer's own content back at it. A broadcast with an independent
	/// route still is.
	#[moq_clock::test]
	async fn assigned_peer_hop_filters_echoed_announces() {
		let assigned = crate::Hop::new(777).unwrap();
		let (publisher, consumer, _routes) = echo_harness(assigned).await;

		let peer = cluster::Peer::default();

		// The cursor is what filters: an excluded consumer never sees the echoed route.
		let mut announced = consumer.excluding(assigned).announced();
		let local = announced.assert_next_active("from/us");
		announced.assert_next_wait();

		assert_eq!(publisher.select(&local, &peer, false), Advert::Some(Default::default()));
	}

	/// An anonymous chain received from an identified peer keeps the 0 on the wire
	/// and is never advertised back to that session: split-horizon matches `via`
	/// as well as the chain.
	#[moq_clock::test]
	async fn anonymous_chain_is_forwarded_with_zero_and_not_echoed() {
		let assigned = crate::Hop::new(777).unwrap();
		let r1 = crate::Hop::new(9).unwrap();
		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let consumer = origin.consume();
		let publisher = Publisher::new(
			crate::time::sim(),
			crate::driver::Transport::new(crate::lite::test_transport::SinkSession::new(Default::default())),
			origin.consume(),
			Control::new(false),
			Some(assigned),
			peer::PeerSetup::default(),
			Version::Draft18,
		);

		let mut hops = crate::Hops::new();
		hops.push(crate::Hop::UNKNOWN).unwrap();
		hops.push(r1).unwrap();
		let _echoed = origin
			.announce(
				"from/peer",
				crate::origin::Route::default().with_hops(hops.clone()).with_via(r1),
			)
			.unwrap();

		let peer = cluster::Peer {
			hop: Some(r1),
			cost: None,
		};
		let mut announced = consumer.excluding(publisher.exclude(&peer)).announced();
		announced.assert_next_wait();

		let forwarded = cluster::Advert::forward(&hops, 0, crate::Hop::new(1).unwrap()).unwrap();
		let ids: Vec<_> = forwarded.hops.hops().iter().map(|h| h.id()).collect();
		assert_eq!(ids, vec![0, 9, 1]);
	}

	/// Declaring the reserved 0 turns the extension on while naming nobody, so the
	/// identity we assigned stands in, exactly as for a peer that never negotiated.
	/// Asserted on the resolution itself rather than through an advertisement: a
	/// negotiated peer always sends its own HOP_PATH, so a route attributed to the
	/// assigned identity is a state this peer class cannot reach; see
	/// [`a_declared_zero_chain_is_not_advertised_back`] for what it gets instead.
	#[moq_clock::test]
	async fn withheld_peer_hop_falls_back_to_assigned() {
		let assigned = crate::Hop::new(777).unwrap();
		let declared = crate::Hop::new(9).unwrap();
		let (publisher, _consumer, _routes) = echo_harness(assigned).await;

		let withheld = cluster::Peer {
			hop: Some(crate::Hop::UNKNOWN),
			cost: None,
		};
		assert!(withheld.negotiated(), "the extension is on");
		assert_eq!(publisher.exclude(&withheld), assigned, "0 names nobody, so we do");

		let absent = cluster::Peer::default();
		assert_eq!(publisher.exclude(&absent), assigned, "so does declaring nothing");

		let named = cluster::Peer {
			hop: Some(declared),
			cost: None,
		};
		assert_eq!(publisher.exclude(&named), declared, "a declared identity wins");
	}

	/// A peer that negotiated the extension MUST send a HOP_PATH on every advertisement,
	/// and one that declared 0 names itself 0 there. An arriving chain is not rewritten,
	/// so the route carries 0; the assigned identity stays on `via` and split-horizon
	/// matches it, so the peer is not advertised its own route back.
	#[moq_clock::test]
	async fn a_declared_zero_chain_is_not_advertised_back() {
		let assigned = crate::Hop::new(777).unwrap();
		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let consumer = origin.consume();

		let publisher = Publisher::new(
			crate::time::sim(),
			crate::driver::Transport::new(crate::lite::test_transport::SinkSession::new(Default::default())),
			origin.consume(),
			Control::new(false),
			Some(assigned),
			peer::PeerSetup::default(),
			Version::Draft18,
		);

		// The chain as ingress stores it: the peer named itself 0, and `via` is the
		// identity we assigned that session.
		let mut hops = crate::Hops::new();
		hops.push(crate::Hop::UNKNOWN).unwrap();
		let _echoed = origin
			.announce(
				"from/peer",
				crate::origin::Route::default().with_hops(hops).with_via(assigned),
			)
			.unwrap();

		let peer = cluster::Peer {
			hop: Some(crate::Hop::UNKNOWN),
			cost: None,
		};
		let mut announced = consumer.excluding(publisher.exclude(&peer)).announced();
		announced.assert_next_wait();
	}

	/// MoQ Active Count: the OK counts exactly the NAMESPACE messages that follow it,
	/// so a route this peer is never told about is not counted either. Counting one would
	/// leave the peer waiting on a message that never comes.
	#[moq_clock::test]
	async fn the_ok_counts_only_what_is_advertised() {
		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();

		let session = SinkSession::gated_bi(kio::Producer::new(true).consume());
		let log = session.log.clone();
		let setup = peer::PeerSetup::default();
		setup.set(peer::Peer {
			cluster: cluster::Peer {
				hop: Some(crate::Hop::new(9).unwrap()),
				cost: None,
			},
			solicit: Some(true),
			active_count: true,
			..Default::default()
		});
		let publisher = Publisher::new(
			crate::time::sim(),
			crate::driver::Transport::new(session.clone()),
			origin.consume(),
			Control::new(false),
			None,
			setup,
			Version::Draft18,
		);

		let mut clean = crate::Hops::new();
		clean.push(crate::Hop::new(778).unwrap()).unwrap();
		let _clean = origin
			.announce("cam-clean", crate::origin::Route::default().with_hops(clean))
			.unwrap();
		// A chain with no room left for our own hop cannot be forwarded.
		let mut full = crate::Hops::new();
		while full.push(crate::Hop::new(100 + full.len() as u64).unwrap()).is_ok() {}
		let _full = origin
			.announce("cam-full", crate::origin::Route::default().with_hops(full))
			.unwrap();
		settle().await;

		let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), Version::Draft18)
			.await
			.unwrap();
		let msg = ietf::SubscribeNamespace {
			request_id: RequestId(1),
			namespace: crate::Path::from_static(""),
			hidden: false,
		};
		let mut run = std::pin::pin!(publisher.run_subscribe_namespace_stream(stream, msg));
		assert!(futures::poll!(run.as_mut()).is_pending());

		assert_eq!(occurrences(&log, b"cam-clean"), 1);
		assert_eq!(occurrences(&log, b"cam-full"), 0);

		let expected = crate::lite::test_transport::Log::default();
		let mut writer = crate::coding::Writer::new(
			crate::transport::boxed::SendStream::new(crate::lite::test_transport::SinkSend::new(expected.clone())),
			Version::Draft18,
		);
		writer.varint(ietf::RequestOk::ID).await.unwrap();
		writer
			.encode(&ietf::RequestOk {
				largest: None,
				active: Some(1),
			})
			.await
			.unwrap();
		let expected = expected.writes.lock().unwrap().clone();
		assert!(
			log.writes.lock().unwrap().starts_with(&expected),
			"the OK counts one NAMESPACE"
		);
	}

	/// A same-path source can splice into (or detach from) an existing broadcast
	/// without an origin-level (un)announce, silently flipping `advertisable`.
	/// Namespace forwarding must follow: advertise when a clean route appears,
	/// withdraw when the last one detaches.
	#[moq_clock::test]
	async fn namespace_follows_route_eligibility_changes() {
		let assigned = crate::Hop::new(777).unwrap();
		let clean_publisher = crate::Hop::new(778).unwrap();
		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();

		let gate = kio::Producer::new(true);
		let session = SinkSession::gated_bi(gate.consume());
		let log = session.log.clone();
		let publisher = Publisher::new(
			crate::time::sim(),
			crate::driver::Transport::new(session.clone()),
			origin.consume(),
			Control::new(false),
			Some(assigned),
			requires_solicitation(),
			Version::Draft18,
		);

		// The prefix starts with only a route from the assigned peer: hop 0 on the
		// chain, identity on `via`.
		let mut tainted_hops = crate::Hops::new();
		tainted_hops.push(crate::Hop::UNKNOWN).unwrap();
		let _tainted = origin
			.announce(
				"route-flip-cam",
				crate::origin::Route::default()
					.with_hops(tainted_hops)
					.with_via(assigned),
			)
			.unwrap();
		settle().await;

		let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), Version::Draft18)
			.await
			.unwrap();
		let msg = ietf::SubscribeNamespace {
			request_id: RequestId(1),
			namespace: crate::Path::from_static(""),
			hidden: false,
		};
		let mut run = std::pin::pin!(publisher.run_subscribe_namespace_stream(stream, msg));

		// Initial set: the tainted-only broadcast is filtered, nothing but the OK
		// response on the wire.
		assert!(futures::poll!(run.as_mut()).is_pending());
		assert_eq!(occurrences(&log, b"route-flip-cam"), 0);

		// A clean route joins the same prefix: the excluded cursor now has a best
		// visible route, so the namespace must be advertised.
		let mut clean_hops = crate::Hops::new();
		clean_hops.push(clean_publisher).unwrap();
		let clean = origin
			.announce("route-flip-cam", crate::origin::Route::default().with_hops(clean_hops))
			.unwrap();
		settle().await;
		assert!(futures::poll!(run.as_mut()).is_pending());
		assert_eq!(
			occurrences(&log, b"route-flip-cam"),
			1,
			"NAMESPACE after a clean route joins"
		);

		// The clean route retracts, leaving only the tainted one: withdrawn.
		drop(clean);
		settle().await;
		assert!(futures::poll!(run.as_mut()).is_pending());
		assert_eq!(
			occurrences(&log, b"route-flip-cam"),
			2,
			"NAMESPACE_DONE after the last clean route detaches"
		);
	}

	/// The peer's OK to a PUBLISH_NAMESPACE, framed exactly as the announce path
	/// reads it -- built with the crate's own writer so the framing can't drift
	/// from the encoder under test.
	/// A REQUEST_ERROR declining an advertisement, with the retry interval the peer asked
	/// for in milliseconds. Zero means it does not want the namespace offered again.
	async fn publish_namespace_error(version: Version, retry_interval: u64) -> Vec<u8> {
		let log = crate::lite::test_transport::Log::default();
		let mut writer = crate::coding::Writer::new(
			crate::transport::boxed::SendStream::new(crate::lite::test_transport::SinkSend::new(log.clone())),
			version,
		);

		writer.varint(ietf::RequestError::ID).await.unwrap();
		writer
			.encode(&ietf::RequestError {
				// UNINTERESTED.
				error_code: 0x20,
				reason_phrase: "no".into(),
				retry_interval,
			})
			.await
			.unwrap();

		log.writes.lock().unwrap().clone()
	}

	/// A peer that refuses an advertisement with a retry interval of 0 is asking not to be
	/// offered it again. Coming back anyway turns a permanent refusal (unauthorized,
	/// uninterested) into a request every few seconds for the life of the session.
	#[moq_clock::test]
	async fn a_refusal_that_forbids_retrying_is_not_retried() {
		const VERSION: Version = Version::Draft18;

		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let _cam = origin.announce("lonely-cam", crate::origin::Route::default()).unwrap();
		settle().await;

		// Every stream is answered with the same refusal, so a retry would show up as a
		// second occurrence on the wire.
		let refusal = publish_namespace_error(VERSION, 0).await;
		let session =
			crate::lite::test_transport::ScriptedSession::per_stream(vec![refusal.clone(), refusal.clone(), refusal]);
		let log = session.log.clone();

		let publisher = Publisher::new(
			crate::time::sim(),
			crate::driver::Transport::new(session.clone()),
			origin.consume(),
			Control::new(false),
			None,
			declared(Some(false)),
			VERSION,
		);

		let mut run = std::pin::pin!(publisher.run_publish_namespaces());
		for _ in 0..100 {
			assert!(futures::poll!(run.as_mut()).is_pending());
			if occurrences(&log, b"lonely-cam") > 0 {
				break;
			}
			settle().await;
		}
		assert_eq!(occurrences(&log, b"lonely-cam"), 1, "the advertisement never went out");

		// Well past every retry the loop would otherwise take.
		for _ in 0..100 {
			assert!(futures::poll!(run.as_mut()).is_pending());
			tick().await;
		}

		assert_eq!(
			occurrences(&log, b"lonely-cam"),
			1,
			"re-offered a namespace the peer asked not to be offered again"
		);
	}

	/// MoQ Epoch: a restart onto another epoch advertises the new instance before it
	/// withdraws the old, and only once the peer accepted it, since the two streams are
	/// unordered. The peer never sees the namespace absent. A peer without the extension
	/// cannot tell the two apart, so it still sees the old one withdrawn first.
	#[moq_clock::test]
	async fn another_epoch_is_advertised_before_the_old_is_withdrawn() {
		use crate::lite::test_transport::Sent;
		const VERSION: Version = Version::Draft18;

		for epochs in [true, false] {
			let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
			let _old = origin
				.announce("cam", crate::origin::Route::default().with_epoch(crate::Epoch::mint()))
				.unwrap();
			settle().await;

			let ok = publish_namespace_ok(VERSION).await;
			let session = crate::lite::test_transport::ScriptedSession::per_stream(vec![ok.clone(), ok]);
			let log = session.log.clone();
			let declared = peer::PeerSetup::default();
			declared.set(peer::Peer {
				solicit: Some(false),
				epoch: epochs,
				..Default::default()
			});
			let publisher = Publisher::new(
				crate::time::sim(),
				crate::driver::Transport::new(session.clone()),
				origin.consume(),
				Control::new(false),
				None,
				declared,
				VERSION,
			);

			let mut run = std::pin::pin!(publisher.run_publish_namespaces());
			let mut _new = None;
			for count in [1, 2] {
				for _ in 0..100 {
					assert!(futures::poll!(run.as_mut()).is_pending());
					if occurrences(&log, b"cam") >= count {
						break;
					}
					settle().await;
				}
				assert_eq!(occurrences(&log, b"cam"), count, "epochs: {epochs}");
				if count == 1 {
					// A newer epoch outranks the old at the same path: a restart.
					let route = crate::origin::Route::default().with_epoch(crate::Epoch::mint());
					_new = Some(origin.announce("cam", route).unwrap());
				}
			}
			for _ in 0..10 {
				assert!(futures::poll!(run.as_mut()).is_pending());
				settle().await;
			}

			// Where each advertisement went out, and where the first one was withdrawn.
			let trail = log.trail();
			let advertised: Vec<(usize, usize)> = trail
				.iter()
				.enumerate()
				.filter_map(|(at, (stream, sent))| match sent {
					Sent::Write(bytes) if bytes.windows(3).any(|window| window == b"cam") => Some((at, *stream)),
					_ => None,
				})
				.collect();
			let [(_, old), (made, new)] = advertised[..] else {
				panic!("epochs: {epochs}: expected two advertisements, got {advertised:?}");
			};
			assert_ne!(old, new, "epochs: {epochs}: one request carried both");
			let broken = trail
				.iter()
				.position(|(stream, sent)| *stream == old && matches!(sent, Sent::Reset(_)))
				.unwrap_or_else(|| panic!("epochs: {epochs}: the old advertisement was never withdrawn"));
			assert_eq!(
				made < broken,
				epochs,
				"epochs: {epochs}: advertised at {made}, withdrew at {broken}"
			);
		}
	}

	async fn publish_namespace_ok(version: Version) -> Vec<u8> {
		let log = crate::lite::test_transport::Log::default();
		let mut writer = crate::coding::Writer::new(
			crate::transport::boxed::SendStream::new(crate::lite::test_transport::SinkSend::new(log.clone())),
			version,
		);

		// The response names no request id: it rides the request's stream.
		writer.varint(ietf::RequestOk::ID).await.unwrap();
		writer
			.encode(&ietf::RequestOk {
				largest: None,
				active: None,
			})
			.await
			.unwrap();

		let writes = log.writes.lock().unwrap();
		writes.clone()
	}

	/// A peer that declared nothing is told without being asked. Relays that never send
	/// SUBSCRIBE_NAMESPACE hear nothing otherwise, and every third-party one behaves
	/// that way: a publisher is expected to announce itself.
	#[moq_clock::test]
	async fn a_peer_that_declared_nothing_is_told_unsolicited() {
		const VERSION: Version = Version::Draft18;

		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let _local = origin.announce("local-cam", crate::origin::Route::default()).unwrap();
		settle().await;

		// The only stream is the PUBLISH_NAMESPACE request we open ourselves.
		let session =
			crate::lite::test_transport::ScriptedSession::per_stream(vec![publish_namespace_ok(VERSION).await]);
		let log = session.log.clone();

		let peer_setup = peer::PeerSetup::default();
		peer_setup.set(peer::Peer::default());

		let publisher = Publisher::new(
			crate::time::sim(),
			crate::driver::Transport::new(session.clone()),
			origin.consume(),
			Control::new(false),
			None,
			peer_setup,
			VERSION,
		);

		let mut run = std::pin::pin!(publisher.run_publish_namespaces());
		for _ in 0..100 {
			assert!(futures::poll!(run.as_mut()).is_pending());
			if occurrences(&log, b"local-cam") >= 1 {
				break;
			}
			settle().await;
		}

		assert_eq!(
			occurrences(&log, b"local-cam"),
			1,
			"PUBLISH_NAMESPACE without a SUBSCRIBE_NAMESPACE"
		);
		assert_eq!(log.bi_opens(), 1, "one request stream");
	}

	/// A PUBLISH_NAMESPACE is withdrawn by cancelling its request: reset
	/// plus STOP_SENDING. A FIN there is a protocol violation some peers close on.
	#[moq_clock::test]
	async fn publish_namespace_withdraws_by_reset() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
			let local = origin.announce("local-cam", crate::origin::Route::default()).unwrap();
			settle().await;

			let session =
				crate::lite::test_transport::ScriptedSession::per_stream(vec![publish_namespace_ok(version).await]);
			let log = session.log.clone();
			let peer_setup = peer::PeerSetup::default();
			peer_setup.set(peer::Peer::default());
			let publisher = Publisher::new(
				crate::time::sim(),
				crate::driver::Transport::new(session.clone()),
				origin.consume(),
				Control::new(false),
				None,
				peer_setup,
				version,
			);

			let mut run = std::pin::pin!(publisher.run_publish_namespaces());
			for _ in 0..100 {
				assert!(futures::poll!(run.as_mut()).is_pending());
				if occurrences(&log, b"local-cam") >= 1 {
					break;
				}
				settle().await;
			}
			assert_eq!(occurrences(&log, b"local-cam"), 1, "{version}: PUBLISH_NAMESPACE");
			let advertised = log.writes.lock().unwrap().len();

			drop(local);
			for _ in 0..100 {
				assert!(futures::poll!(run.as_mut()).is_pending());
				if !log.resets().is_empty() {
					break;
				}
				settle().await;
			}

			let cancelled = crate::ietf::error::CANCELLED;
			assert_eq!(log.resets(), vec![cancelled], "{version}: reset");
			assert_eq!(log.stops(), vec![cancelled], "{version}: STOP_SENDING");
			// No PUBLISH_NAMESPACE_DONE: a receiver reads its type as a protocol violation.
			assert_eq!(
				log.writes.lock().unwrap().len(),
				advertised,
				"{version}: wrote a message"
			);
			assert!(
				!log.trail()
					.iter()
					.any(|(_, sent)| *sent == crate::lite::test_transport::Sent::Finish),
				"{version}: FIN"
			);
			assert!(log.closes().is_empty(), "{version}: the session closed");
		}
	}

	/// ACTIVE_COUNT only answers SUBSCRIBE_NAMESPACE (MoQ Active Count), so one on the OK
	/// to a PUBLISH_NAMESPACE is the peer breaking the extension, negotiated or not.
	#[moq_clock::test]
	async fn a_counted_publish_namespace_ok_is_a_violation() {
		const VERSION: Version = Version::Draft18;

		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let _local = origin.announce("local-cam", crate::origin::Route::default()).unwrap();
		settle().await;

		let ok = crate::lite::test_transport::Log::default();
		let mut writer = crate::coding::Writer::new(
			crate::transport::boxed::SendStream::new(crate::lite::test_transport::SinkSend::new(ok.clone())),
			VERSION,
		);
		writer.varint(ietf::RequestOk::ID).await.unwrap();
		writer
			.encode(&ietf::RequestOk {
				largest: None,
				active: Some(0),
			})
			.await
			.unwrap();
		let ok = ok.writes.lock().unwrap().clone();
		let session = crate::lite::test_transport::ScriptedSession::per_stream(vec![ok]);
		let log = session.log.clone();

		let peer_setup = peer::PeerSetup::default();
		peer_setup.set(peer::Peer {
			active_count: true,
			..Default::default()
		});
		let publisher = Publisher::new(
			crate::time::sim(),
			crate::driver::Transport::new(session.clone()),
			origin.consume(),
			Control::new(false),
			None,
			peer_setup,
			VERSION,
		);

		let res = moq_clock::timeout(Duration::from_secs(5), publisher.run_publish_namespaces())
			.await
			.expect("the violation ends the loop");
		assert!(matches!(res, Err(Error::ProtocolViolation)), "{res:?}");
		// Closed at the source, since the solicited path's request task only logs errors.
		let code = crate::SessionError::ProtocolViolation.to_code();
		assert!(log.closes().iter().any(|(c, _)| *c == code), "{:?}", log.closes());
	}

	/// Drive both announce loops at once against a peer that declared `solicit`,
	/// returning how many times the namespace hit the wire and how many bidi streams
	/// were opened. One stream means the entry rode the subscription inline; two means
	/// it went out as its own PUBLISH_NAMESPACE request.
	async fn advertise_both_ways(solicit: Option<bool>) -> (usize, usize) {
		let log = advertise_with_hidden(Discovery {
			solicit,
			..Discovery::default()
		})
		.await;
		(occurrences(&log, b"cam"), log.bi_opens())
	}

	/// [`advertise_both_ways`] with a hidden `.stats/node` beside `cam`, and the peer's
	/// SUBSCRIBE_NAMESPACE for `prefix` opting in to hidden namespaces or not.
	struct Discovery<'a> {
		version: Version,
		declared: bool,
		solicit: Option<bool>,
		prefix: &'a str,
		hidden: bool,
		scoped: bool,
	}

	impl Default for Discovery<'_> {
		fn default() -> Self {
			Self {
				version: Version::Draft18,
				declared: true,
				solicit: Some(true),
				prefix: "",
				hidden: false,
				scoped: false,
			}
		}
	}

	async fn advertise_with_hidden(case: Discovery<'_>) -> crate::lite::test_transport::Log {
		let Discovery {
			version,
			declared: hidden_declared,
			solicit,
			prefix,
			hidden,
			scoped,
		} = case;
		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let _cam = origin.announce("cam", crate::origin::Route::default()).unwrap();
		let _stats = origin.announce(".stats/node", crate::origin::Route::default()).unwrap();
		settle().await;

		// Stream 1 is the peer's SUBSCRIBE_NAMESPACE; stream 2, if opened at all, is our
		// PUBLISH_NAMESPACE request.
		let session = crate::lite::test_transport::ScriptedSession::per_stream(vec![
			Vec::new(),
			publish_namespace_ok(version).await,
		]);
		let log = session.log.clone();

		let setup = peer::PeerSetup::default();
		setup.set(peer::Peer {
			solicit,
			hidden: hidden_declared,
			..Default::default()
		});
		let consume = match scoped {
			true => origin
				.consume()
				.scope("", &crate::Pattern::subtree(".stats").unwrap().into())
				.unwrap(),
			false => origin.consume(),
		};
		let publisher = Publisher::new(
			crate::time::sim(),
			crate::driver::Transport::new(session.clone()),
			consume,
			Control::new(false),
			None,
			setup,
			version,
		);

		let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), version)
			.await
			.unwrap();
		let msg = ietf::SubscribeNamespace {
			request_id: RequestId(1),
			namespace: crate::Path::new(prefix).unwrap(),
			hidden,
		};
		let mut solicited = std::pin::pin!(publisher.clone().run_subscribe_namespace_stream(stream, msg));
		let mut unsolicited = std::pin::pin!(publisher.run_publish_namespaces());

		// Poll well past the first advertisement, so a second one from the other loop
		// would show up rather than being missed by an early break. The unsolicited loop
		// finishes immediately when the peer requires solicitation, and a completed
		// future must not be polled again.
		let mut quiet = false;
		for _ in 0..100 {
			assert!(futures::poll!(solicited.as_mut()).is_pending());
			if !quiet {
				quiet = futures::poll!(unsolicited.as_mut()).is_ready();
			}
			settle().await;
		}

		log
	}

	/// A hidden namespace reaches only a subscription that opted in or named its dot
	/// segment. The subscription also repeats every visible
	/// namespace the unsolicited loop already sent. A NAMESPACE is discovery, not
	/// a second route.
	#[moq_clock::test]
	async fn hidden_namespaces_need_an_opt_in() {
		for (solicit, prefix, hidden, cam, stats) in [
			(Some(false), "", false, 2, 0),
			(Some(false), "", true, 2, 1),
			(Some(false), ".stats", false, 1, 1),
			(Some(true), "", false, 1, 0),
			(Some(true), "", true, 1, 1),
			(Some(true), ".stats", false, 0, 1),
		] {
			let log = advertise_with_hidden(Discovery {
				solicit,
				prefix,
				hidden,
				..Discovery::default()
			})
			.await;
			let case = format!("solicit {solicit:?}, prefix {prefix:?}, hidden {hidden}");
			assert_eq!(occurrences(&log, b"cam"), cam, "{case}");
			// An inline entry names its suffix, so count the leaf.
			assert_eq!(occurrences(&log, b"node"), stats, "{case}");
		}
	}

	#[moq_clock::test]
	async fn hidden_discovery_obeys_peer_setup_and_requested_prefix() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			for declared in [false, true] {
				for solicit in [Some(false), Some(true)] {
					for scoped in [false, true] {
						for (prefix, hidden) in [("", false), ("", true), (".stats", false)] {
							let log = advertise_with_hidden(Discovery {
								version,
								declared,
								solicit,
								prefix,
								hidden,
								scoped,
							})
							.await;
							// Visible on the subscription: not filtered, opted in, or the prefix
							// names the dot segment. The unsolicited loop sends a dot namespace
							// only when the peer did not declare MoQ Hidden.
							let on_stream = !declared || hidden || prefix == ".stats";
							let unsolicited = solicit != Some(true) && !declared;
							let expected = usize::from(on_stream) + usize::from(unsolicited);
							assert_eq!(
								occurrences(&log, b"node"),
								expected,
								"{version:?}: declared={declared}, solicit={solicit:?}, scoped={scoped}, prefix={prefix}, hidden={hidden}"
							);
						}
					}
				}
			}
		}
	}

	/// A peer that did not ask to be solicited hears each namespace twice: an
	/// unsolicited PUBLISH_NAMESPACE and a NAMESPACE on the SUBSCRIBE_NAMESPACE stream.
	/// Those are two discoveries of one namespace, not two routes. A peer that asked to
	/// be solicited hears it only on the stream.
	#[moq_clock::test]
	async fn a_non_solicit_peer_hears_a_namespace_both_ways() {
		let (times, streams) = advertise_both_ways(Some(false)).await;
		assert_eq!(times, 2, "PUBLISH_NAMESPACE and NAMESPACE");
		assert_eq!(streams, 2, "the subscription plus one PUBLISH_NAMESPACE request");

		let (once, streams) = advertise_both_ways(Some(true)).await;
		assert_eq!(once, 1, "a peer that asked to be told on request is told once");
		assert_eq!(streams, 1, "inline on the SUBSCRIBE_NAMESPACE stream it asked on");
	}

	/// A peer that did not send SOLICIT still gets NAMESPACE on its SUBSCRIBE_NAMESPACE
	/// stream: one for a match that already exists, one announced
	/// after, then NAMESPACE_DONE when that announcement ends.
	#[moq_clock::test]
	async fn a_non_solicit_subscribe_namespace_carries_namespace() {
		for version in [Version::Draft18] {
			let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
			let early = origin.announce("early-cam", crate::origin::Route::default()).unwrap();
			settle().await;

			let session = crate::lite::test_transport::ScriptedSession::per_stream(vec![Vec::new()]);
			let log = session.log.clone();

			let peer_setup = peer::PeerSetup::default();
			peer_setup.set(peer::Peer::default());

			let publisher = Publisher::new(
				crate::time::sim(),
				crate::driver::Transport::new(session.clone()),
				origin.consume(),
				Control::new(false),
				None,
				peer_setup,
				version,
			);

			let stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), version)
				.await
				.unwrap();
			let msg = ietf::SubscribeNamespace {
				request_id: RequestId(1),
				namespace: crate::Path::from_static(""),
				hidden: false,
			};
			let mut run = std::pin::pin!(publisher.run_subscribe_namespace_stream(stream, msg));

			for _ in 0..100 {
				assert!(futures::poll!(run.as_mut()).is_pending());
				if occurrences(&log, b"early-cam") >= 1 {
					break;
				}
				settle().await;
			}
			let bytes = log.writes.lock().unwrap().clone();
			assert_eq!(
				namespace_events(&bytes, version),
				vec![(ietf::Namespace::ID, "early-cam".to_owned())],
				"{version:?}: existing match",
			);

			let _late = origin.announce("late-cam", crate::origin::Route::default()).unwrap();
			for _ in 0..100 {
				assert!(futures::poll!(run.as_mut()).is_pending());
				if occurrences(&log, b"late-cam") >= 1 {
					break;
				}
				settle().await;
			}
			let bytes = log.writes.lock().unwrap().clone();
			assert_eq!(
				namespace_events(&bytes, version),
				vec![
					(ietf::Namespace::ID, "early-cam".to_owned()),
					(ietf::Namespace::ID, "late-cam".to_owned()),
				],
				"{version:?}: announced later",
			);

			drop(early);
			for _ in 0..100 {
				assert!(futures::poll!(run.as_mut()).is_pending());
				if occurrences(&log, b"early-cam") >= 2 {
					break;
				}
				settle().await;
			}
			let bytes = log.writes.lock().unwrap().clone();
			assert_eq!(
				namespace_events(&bytes, version),
				vec![
					(ietf::Namespace::ID, "early-cam".to_owned()),
					(ietf::Namespace::ID, "late-cam".to_owned()),
					(ietf::NamespaceDone::ID, "early-cam".to_owned()),
				],
				"{version:?}: NAMESPACE_DONE when it ends",
			);
		}
	}

	/// Send capacity can park an admitted stream's header. That must not wedge the loop:
	/// withdrawals queued behind it still need to progress. A withdrawal ends its request's
	/// own stream, which needs no new open, so it is visible while every open is blocked.
	#[moq_clock::test]
	async fn a_parked_open_still_lets_a_namespace_be_withdrawn() {
		const VERSION: Version = Version::Draft18;

		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let first = origin.announce("first-cam", crate::origin::Route::default()).unwrap();
		settle().await;

		// The first advertisement has send capacity, and the peer answers it.
		let gate = kio::Producer::new(true);
		let ok = publish_namespace_ok(VERSION).await;
		let session = crate::lite::test_transport::ScriptedSession::gated_open(vec![ok.clone(), ok], gate.consume());
		let log = session.log.clone();

		let publisher = Publisher::new(
			crate::time::sim(),
			crate::driver::Transport::new(session.clone()),
			origin.consume(),
			Control::new(false),
			None,
			declared(Some(false)),
			VERSION,
		);

		let mut run = std::pin::pin!(publisher.run_publish_namespaces());
		for _ in 0..100 {
			assert!(futures::poll!(run.as_mut()).is_pending());
			if occurrences(&log, b"first-cam") > 0 {
				break;
			}
			settle().await;
		}
		assert_eq!(
			occurrences(&log, b"first-cam"),
			1,
			"the first advertisement never went out"
		);

		// Send capacity runs out before the second advertisement can send its header.
		set_gate(&gate, false);
		let _second = origin.announce("second-cam", crate::origin::Route::default()).unwrap();
		settle().await;

		// Retiring the first needs no new stream, so the loop has to reach
		// it despite the open above.
		drop(first);

		let ended = || {
			log.trail()
				.into_iter()
				.any(|(_, sent)| !matches!(sent, crate::lite::test_transport::Sent::Write(_)))
		};
		for _ in 0..100 {
			assert!(futures::poll!(run.as_mut()).is_pending());
			if ended() {
				break;
			}
			tick().await;
		}
		assert!(ended(), "the withdrawal never went out: the open wedged the loop");

		// Send capacity returns, and nothing else about the origin changes.
		set_gate(&gate, true);

		for _ in 0..100 {
			assert!(futures::poll!(run.as_mut()).is_pending());
			if occurrences(&log, b"second-cam") > 0 {
				break;
			}
			tick().await;
		}
		assert_eq!(
			occurrences(&log, b"second-cam"),
			1,
			"never retried once credit returned"
		);
	}

	/// A namespace nobody can advertise any more is not pending, whatever happened before.
	/// `deferred` outliving the want would arm the retry timer forever for a wire message
	/// that can never happen: not a spin, but a session that never sleeps.
	#[moq_clock::test]
	async fn a_namespace_that_stops_being_advertisable_stops_being_deferred() {
		let assigned = crate::Hop::new(777).unwrap();

		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();

		// A route that already passed through us must never be forwarded: `select`
		// wants nothing, which is what the peer already holds.
		let mut hops = crate::Hops::new();
		hops.push(crate::Hop::new(1).unwrap()).unwrap();

		let session = crate::lite::test_transport::SinkSession::new(Default::default());
		let publisher = Publisher::new(
			crate::time::sim(),
			crate::driver::Transport::new(session.clone()),
			origin.consume(),
			Control::new(false),
			Some(assigned),
			declared(Some(false)),
			Version::Draft18,
		);

		// The state a refused or failed offer leaves behind: the peer holds nothing, and
		// the loop is coming back to it on a timer.
		let suffix: crate::Path = crate::Path::from_static("from/peer");
		let mut watch = Watched::new(crate::origin::Route::default().with_hops(hops));
		watch.deferred = true;

		let mut ns = Namespaces::new(cluster::Peer::default(), Target::Requests);
		ns.watched.insert(suffix.clone(), watch);

		publisher.sync_namespace(&mut ns, &suffix, &suffix).await.unwrap();

		assert!(
			!ns.watched[&suffix].deferred,
			"the retry timer stays armed for a namespace that can never be advertised"
		);
	}

	/// A minimum wait binds every path back to the namespace, not just the retry sweep.
	/// A route change re-prices the advertisement; it does not excuse us from the wait the
	/// peer asked for.
	#[moq_clock::test]
	async fn a_route_change_still_waits_out_a_refusal() {
		const VERSION: Version = Version::Draft18;

		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		// One epoch, so the second route re-prices the instance rather than restart it.
		let epoch = crate::Epoch::mint();
		let cam = origin
			.announce("solo-cam", crate::origin::Route::default().with_epoch(epoch.clone()))
			.unwrap();
		settle().await;

		// Refused with a wait far longer than any backoff the loop would take on its own.
		let refusal = publish_namespace_error(VERSION, 600_000).await;
		let session =
			crate::lite::test_transport::ScriptedSession::per_stream(vec![refusal.clone(), refusal.clone(), refusal]);
		let log = session.log.clone();

		let publisher = Publisher::new(
			crate::time::sim(),
			crate::driver::Transport::new(session.clone()),
			origin.consume(),
			Control::new(false),
			None,
			declared(Some(false)),
			VERSION,
		);

		let mut run = std::pin::pin!(publisher.run_publish_namespaces());
		for _ in 0..100 {
			assert!(futures::poll!(run.as_mut()).is_pending());
			if occurrences(&log, b"solo-cam") > 0 {
				break;
			}
			settle().await;
		}
		assert_eq!(occurrences(&log, b"solo-cam"), 1, "the advertisement never went out");

		// A cheaper second route makes the advertisement worth re-pricing, which is a
		// path back into the reconciliation that does not go through the retry timer.
		let _standby = origin
			.announce(
				"solo-cam",
				crate::origin::Route::default().with_epoch(epoch).with_cost(0),
			)
			.unwrap();

		for _ in 0..100 {
			assert!(futures::poll!(run.as_mut()).is_pending());
			tick().await;
		}

		assert_eq!(
			occurrences(&log, b"solo-cam"),
			1,
			"re-offered inside the wait the peer asked for"
		);

		drop(cam);
	}

	/// A SETUP slot for a peer that negotiated the cluster extension, so an
	/// advertisement carries a path and a cost worth repricing.
	fn clustered(solicit: Option<bool>) -> peer::PeerSetup {
		let slot = peer::PeerSetup::default();
		slot.set(peer::Peer {
			cluster: cluster::Peer {
				hop: Some(crate::Hop::new(9).unwrap()),
				cost: None,
			},
			solicit,
			hidden: false,
			max_age: false,
			auth: false,
			active_count: false,
			epoch: false,
			flate: false,
		});
		slot
	}

	/// The bytes of one REQUEST_UPDATE, framed as the publisher writes it.
	async fn request_update(version: Version, msg: &ietf::PublishNamespaceUpdate) -> Vec<u8> {
		let log = crate::lite::test_transport::Log::default();
		let mut writer = crate::coding::Writer::new(
			crate::transport::boxed::SendStream::new(crate::lite::test_transport::SinkSend::new(log.clone())),
			version,
		);
		writer.varint(ietf::PublishNamespaceUpdate::ID).await.unwrap();
		writer.encode(msg).await.unwrap();
		log.writes.lock().unwrap().clone()
	}

	/// A relay that starts carrying a namespace reprices it with REQUEST_UPDATE on the
	/// request that already carries it, sending the changed parameter only. The new cost
	/// is 0, which has to be explicit: REQUEST_UPDATE keeps an omitted parameter, so
	/// leaving it out would keep the old price.
	#[moq_clock::test]
	async fn a_repricing_is_a_request_update() {
		const VERSION: Version = Version::Draft18;

		// Forward the update at once: the hold is not what this checks.
		let origin = crate::origin::Config {
			update_hold: Duration::ZERO,
			..crate::origin::Config::new(crate::Hop::new(1).unwrap())
		}
		.produce();
		// One epoch, so the warm route re-prices the instance rather than restart it.
		let epoch = crate::Epoch::mint();
		let _cold = origin
			.announce(
				"cam",
				crate::origin::Route::default().with_epoch(epoch.clone()).with_cost(4),
			)
			.unwrap();
		settle().await;

		// One stream, answering the PUBLISH_NAMESPACE. The update goes unanswered: an answer
		// scripted ahead of it would arrive while the request is idle, which is a violation.
		let ok = publish_namespace_ok(VERSION).await;
		let session = crate::lite::test_transport::ScriptedSession::per_stream(vec![ok]);
		let log = session.log.clone();

		let publisher = Publisher::new(
			crate::time::sim(),
			crate::driver::Transport::new(session.clone()),
			origin.consume(),
			Control::new(false),
			None,
			clustered(Some(false)),
			VERSION,
		);

		let mut run = std::pin::pin!(publisher.run_publish_namespaces());
		for _ in 0..100 {
			assert!(futures::poll!(run.as_mut()).is_pending());
			if occurrences(&log, b"cam") >= 1 {
				break;
			}
			settle().await;
		}
		assert_eq!(occurrences(&log, b"cam"), 1, "the advertisement never went out");

		// We start carrying it: a free route outranks the cold one.
		let _warm = origin
			.announce("cam", crate::origin::Route::default().with_epoch(epoch).with_cost(0))
			.unwrap();

		// The request consumed id 1, so the update takes the next of our parity. The
		// path is unchanged and omitted; the cost is an explicit 0.
		let expected = request_update(
			VERSION,
			&ietf::PublishNamespaceUpdate {
				request_id: RequestId(3),
				hops: None,
				cost: Some(0),
				authorization_token: None,
			},
		)
		.await;
		for _ in 0..100 {
			assert!(futures::poll!(run.as_mut()).is_pending());
			if occurrences(&log, &expected) >= 1 {
				break;
			}
			settle().await;
		}

		assert_eq!(occurrences(&log, &expected), 1, "REQUEST_UPDATE with an explicit 0");
		assert_eq!(occurrences(&log, b"cam"), 1, "PUBLISH_NAMESPACE was not repeated");
		assert_eq!(log.bi_opens(), 1, "the update rode the request's own stream");
	}

	/// ACTIVE_COUNT only answers SUBSCRIBE_NAMESPACE (MoQ Active Count), so one on the OK
	/// to a REQUEST_UPDATE is the peer breaking the extension too.
	#[moq_clock::test]
	async fn a_counted_update_ok_is_a_violation() {
		const VERSION: Version = Version::Draft18;

		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let epoch = crate::Epoch::mint();
		let _cold = origin
			.announce(
				"cam",
				crate::origin::Route::default().with_epoch(epoch.clone()).with_cost(4),
			)
			.unwrap();
		settle().await;

		// The PUBLISH_NAMESPACE is answered cleanly; its update's OK carries a count.
		let counted = crate::lite::test_transport::Log::default();
		let mut writer = crate::coding::Writer::new(
			crate::transport::boxed::SendStream::new(crate::lite::test_transport::SinkSend::new(counted.clone())),
			VERSION,
		);
		writer.varint(ietf::RequestOk::ID).await.unwrap();
		writer
			.encode(&ietf::RequestOk {
				largest: None,
				active: Some(0),
			})
			.await
			.unwrap();
		let counted = counted.writes.lock().unwrap().clone();
		let ok = publish_namespace_ok(VERSION).await;
		let session = crate::lite::test_transport::ScriptedSession::new(ok);
		let log = session.log.clone();
		let peer = session.clone();

		let publisher = Publisher::new(
			crate::time::sim(),
			crate::driver::Transport::new(session.clone()),
			origin.consume(),
			Control::new(false),
			None,
			clustered(Some(false)),
			VERSION,
		);

		let mut run = std::pin::pin!(publisher.run_publish_namespaces());
		for _ in 0..100 {
			assert!(futures::poll!(run.as_mut()).is_pending());
			if occurrences(&log, b"cam") >= 1 {
				break;
			}
			settle().await;
		}
		assert_eq!(occurrences(&log, b"cam"), 1, "the advertisement never went out");

		// Queued behind the OK the idle request already read, so it answers the update:
		// nothing wakes the reader until the update goes out.
		peer.push(&counted);
		// Repricing sends the REQUEST_UPDATE whose OK is counted.
		let _warm = origin
			.announce("cam", crate::origin::Route::default().with_epoch(epoch).with_cost(0))
			.unwrap();

		let res = moq_clock::timeout(Duration::from_secs(5), run)
			.await
			.expect("the violation ends the loop");
		assert!(matches!(res, Err(Error::ProtocolViolation)), "{res:?}");
		// Closed at the source, since the solicited path's request task only logs errors.
		let code = crate::SessionError::ProtocolViolation.to_code();
		assert!(log.closes().iter().any(|(c, _)| *c == code), "{:?}", log.closes());
	}

	/// A route from a different original publisher under the same epoch (a replica)
	/// updates the advertisement in place, like any other change within an instance:
	/// withdrawing it would make the namespace briefly vanish downstream just because
	/// its replica moved.
	#[moq_clock::test]
	async fn a_replica_change_is_a_request_update() {
		const VERSION: Version = Version::Draft18;

		// Forward the update at once: the hold is not what this checks.
		let origin = crate::origin::Config {
			update_hold: Duration::ZERO,
			..crate::origin::Config::new(crate::Hop::new(1).unwrap())
		}
		.produce();
		let publisher_a = crate::Hops::try_from(vec![crate::Hop::new(7).unwrap()]).unwrap();
		let publisher_b = crate::Hops::try_from(vec![crate::Hop::new(8).unwrap()]).unwrap();
		let epoch = crate::Epoch::mint();
		let _from_a = origin
			.announce(
				"cam",
				crate::origin::Route::default()
					.with_epoch(epoch.clone())
					.with_hops(publisher_a)
					.with_cost(4),
			)
			.unwrap();
		settle().await;

		// One stream, answering the advertisement from A. Its update to B goes unanswered: an
		// answer scripted ahead of it would arrive while the request is idle.
		let ok = publish_namespace_ok(VERSION).await;
		let session = crate::lite::test_transport::ScriptedSession::per_stream(vec![ok]);
		let log = session.log.clone();

		let publisher = Publisher::new(
			crate::time::sim(),
			crate::driver::Transport::new(session.clone()),
			origin.consume(),
			Control::new(false),
			None,
			clustered(Some(false)),
			VERSION,
		);

		let mut run = std::pin::pin!(publisher.run_publish_namespaces());
		for _ in 0..100 {
			assert!(futures::poll!(run.as_mut()).is_pending());
			if occurrences(&log, b"cam") >= 1 {
				break;
			}
			settle().await;
		}
		assert_eq!(occurrences(&log, b"cam"), 1, "the advertisement never went out");

		// A cheaper route from B wins the selection.
		let _from_b = origin
			.announce(
				"cam",
				crate::origin::Route::default()
					.with_epoch(epoch)
					.with_hops(publisher_b)
					.with_cost(0),
			)
			.unwrap();
		let update = request_update(
			VERSION,
			&ietf::PublishNamespaceUpdate {
				request_id: RequestId(3),
				hops: Some(cluster::HopPath::new(
					crate::Hops::try_from(vec![crate::Hop::new(8).unwrap(), crate::Hop::new(1).unwrap()]).unwrap(),
				)),
				cost: Some(0),
				authorization_token: None,
			},
		)
		.await;
		for _ in 0..100 {
			assert!(futures::poll!(run.as_mut()).is_pending());
			if occurrences(&log, &update) >= 1 {
				break;
			}
			settle().await;
		}

		assert_eq!(occurrences(&log, &update), 1, "REQUEST_UPDATE carrying B's path");
		assert_eq!(occurrences(&log, b"cam"), 1, "PUBLISH_NAMESPACE was not repeated");
		assert_eq!(log.bi_opens(), 1, "no withdrawal and no second stream");
	}

	/// The token a request carries for `value`, as the publisher writes it.
	fn auth_token(value: &[u8]) -> super::super::token::RequestToken {
		super::super::token::RequestToken(crate::setup::Token {
			kind: crate::setup::Token::OUT_OF_BAND,
			value: value.to_vec(),
		})
	}

	/// A publisher on `VERSION` whose auth handle carries tokens on requests, advertising
	/// `origin` unsolicited, with one script per stream it opens.
	fn auth_publisher(
		origin: &crate::origin::Producer,
		auth: &crate::auth::Link,
		scripts: Vec<Vec<u8>>,
		version: Version,
	) -> (Publisher, crate::lite::test_transport::Log) {
		let session = crate::lite::test_transport::ScriptedSession::per_stream(scripts);
		let log = session.log.clone();
		let publisher = Publisher::new(
			crate::time::sim(),
			crate::driver::Transport::new(session.clone()),
			origin.consume(),
			Control::new(false),
			None,
			declared(Some(false)),
			version,
		)
		.with_auth(auth.clone());
		(publisher, log)
	}

	/// An origin that forwards route updates at once: the hold is not what these check.
	fn unheld_origin() -> crate::origin::Producer {
		crate::origin::Config {
			update_hold: Duration::ZERO,
			..crate::origin::Config::new(crate::Hop::new(1).unwrap())
		}
		.produce()
	}

	/// Poll the loop until `needle` shows up `count` times, or give up after 100 settles.
	async fn poll_until<F: std::future::Future>(
		run: &mut std::pin::Pin<&mut F>,
		log: &crate::lite::test_transport::Log,
		needle: &[u8],
		count: usize,
	) {
		for _ in 0..100 {
			assert!(futures::poll!(run.as_mut()).is_pending());
			if occurrences(log, needle) >= count {
				return;
			}
			settle().await;
		}
	}

	/// A route's auth rides its PUBLISH_NAMESPACE as the request's token, and swapping it
	/// renews the request in place: REQUEST_UPDATE on the same stream, carrying only the
	/// new token.
	#[moq_clock::test]
	async fn swapping_a_route_auth_is_a_request_update() {
		swaps_a_route_auth(Version::Draft18).await;
	}

	async fn swaps_a_route_auth(version: Version) {
		let origin = unheld_origin();
		let auth = crate::auth::Link::ietf(false);
		let first = auth.present(bytes::Bytes::from_static(b"first-secret"), false).unwrap();
		let second = auth
			.present(bytes::Bytes::from_static(b"second-secret"), false)
			.unwrap();
		let cam = origin
			.announce("cam", crate::origin::Route::default().with_auth(first))
			.unwrap();
		settle().await;

		// From draft 18 the update goes unanswered: an answer scripted ahead of it would
		// arrive while the request is idle.
		let ok = publish_namespace_ok(version).await;
		let script = ok;
		let (publisher, log) = auth_publisher(&origin, &auth, vec![script], version);
		let mut run = std::pin::pin!(publisher.run_publish_namespaces());
		poll_until(&mut run, &log, b"cam", 1).await;
		assert_eq!(
			occurrences(&log, b"cam"),
			1,
			"{version}: the advertisement never went out"
		);
		assert_eq!(
			occurrences(&log, b"first-secret"),
			1,
			"{version}: PUBLISH_NAMESPACE carries the auth"
		);

		cam.update(crate::origin::Route::default().with_auth(second)).unwrap();
		let expected = request_update(
			version,
			&ietf::PublishNamespaceUpdate {
				request_id: RequestId(3),
				hops: None,
				cost: None,
				authorization_token: Some(auth_token(b"second-secret")),
			},
		)
		.await;
		poll_until(&mut run, &log, &expected, 1).await;

		assert_eq!(
			occurrences(&log, &expected),
			1,
			"{version}: REQUEST_UPDATE with the new token"
		);
		// Let its answer land, which leaves the request standing.
		for _ in 0..10 {
			assert!(futures::poll!(run.as_mut()).is_pending());
			settle().await;
		}
		assert_eq!(
			occurrences(&log, b"cam"),
			1,
			"{version}: PUBLISH_NAMESPACE was not repeated"
		);
		assert_eq!(log.bi_opens(), 1, "{version}: the update rode the request's own stream");
	}

	/// An auth another handle issued is not this session's to present: the route is
	/// advertised under the session's scope, and the credential never reaches this peer.
	#[moq_clock::test]
	async fn a_foreign_route_auth_is_not_carried() {
		const VERSION: Version = Version::Draft18;

		let origin = unheld_origin();
		let other = crate::auth::Link::ietf(false);
		let foreign = other
			.present(bytes::Bytes::from_static(b"foreign-secret"), false)
			.unwrap();
		let _cam = origin
			.announce("cam", crate::origin::Route::default().with_auth(foreign))
			.unwrap();
		settle().await;

		let ok = publish_namespace_ok(VERSION).await;
		let (publisher, log) = auth_publisher(&origin, &crate::auth::Link::ietf(false), vec![ok], VERSION);
		let mut run = std::pin::pin!(publisher.run_publish_namespaces());
		poll_until(&mut run, &log, b"cam", 1).await;

		assert_eq!(occurrences(&log, b"cam"), 1, "the advertisement never went out");
		assert_eq!(
			occurrences(&log, b"foreign-secret"),
			0,
			"sent another handle's credential"
		);
	}

	/// moq-transport cannot name an auth on a request that had none, so naming one
	/// withdraws the request and makes it again with the token.
	#[moq_clock::test]
	async fn naming_an_auth_restarts_an_unnamed_request() {
		const VERSION: Version = Version::Draft18;

		let origin = unheld_origin();
		let auth = crate::auth::Link::ietf(false);
		let token = auth.present(bytes::Bytes::from_static(b"late-secret"), false).unwrap();
		let cam = origin.announce("cam", crate::origin::Route::default()).unwrap();
		settle().await;

		let ok = publish_namespace_ok(VERSION).await;
		let (publisher, log) = auth_publisher(&origin, &auth, vec![ok.clone(), ok], VERSION);
		let mut run = std::pin::pin!(publisher.run_publish_namespaces());
		poll_until(&mut run, &log, b"cam", 1).await;
		assert_eq!(occurrences(&log, b"late-secret"), 0, "named before it had an auth");

		cam.update(crate::origin::Route::default().with_auth(token)).unwrap();
		poll_until(&mut run, &log, b"cam", 2).await;

		assert_eq!(occurrences(&log, b"cam"), 2, "the request was not made again");
		assert_eq!(occurrences(&log, b"late-secret"), 1, "the new request carries the auth");
		assert_eq!(log.bi_opens(), 2, "a fresh stream for the fresh request");
	}

	/// Renewals wait on their answers side by side: one the peer has not answered holds up
	/// neither another namespace's renewal nor a new announcement.
	#[moq_clock::test]
	async fn renewals_are_in_flight_together() {
		const VERSION: Version = Version::Draft18;

		let origin = unheld_origin();
		let auth = crate::auth::Link::ietf(false);
		let present = |token: &'static [u8]| auth.present(bytes::Bytes::from_static(token), false).unwrap();
		let a = origin
			.announce("cam-a", crate::origin::Route::default().with_auth(present(b"a-first")))
			.unwrap();
		let b = origin
			.announce("cam-b", crate::origin::Route::default().with_auth(present(b"b-first")))
			.unwrap();
		settle().await;

		// Each advertisement is answered and its update never is.
		let ok = publish_namespace_ok(VERSION).await;
		let (publisher, log) = auth_publisher(&origin, &auth, vec![ok.clone(), ok.clone(), ok], VERSION);
		let mut run = std::pin::pin!(publisher.run_publish_namespaces());
		poll_until(&mut run, &log, b"cam-b", 1).await;
		assert_eq!(occurrences(&log, b"cam-a"), 1, "cam-a was never advertised");
		assert_eq!(occurrences(&log, b"cam-b"), 1, "cam-b was never advertised");

		a.update(crate::origin::Route::default().with_auth(present(b"a-second")))
			.unwrap();
		b.update(crate::origin::Route::default().with_auth(present(b"b-second")))
			.unwrap();
		poll_until(&mut run, &log, b"b-second", 1).await;
		assert_eq!(occurrences(&log, b"a-second"), 1, "cam-a's renewal never went out");
		assert_eq!(
			occurrences(&log, b"b-second"),
			1,
			"cam-b's renewal waited on cam-a's answer"
		);

		let _c = origin.announce("cam-c", crate::origin::Route::default()).unwrap();
		poll_until(&mut run, &log, b"cam-c", 1).await;
		assert_eq!(
			occurrences(&log, b"cam-c"),
			1,
			"a new announcement waited on the renewals"
		);
	}

	/// From draft 18 an idle advertisement keeps reading, so a peer that FINs or resets its
	/// answer while nothing is pending has the namespace offered again, after a wait that
	/// doubles each time so a peer ending every advertisement does not get a request loop.
	#[moq_clock::test]
	async fn an_idle_advertisement_that_ends_is_offered_again() {
		use crate::transport::memory;
		const VERSION: Version = Version::Draft18;

		for fin in [true, false] {
			let origin = unheld_origin();
			let _cam = origin.announce("cam", crate::origin::Route::default()).unwrap();
			settle().await;

			let (session, peer) = memory::pair(memory::Config::default());
			let publisher = Publisher::new(
				crate::time::sim(),
				crate::driver::Transport::new(session.clone()),
				origin.consume(),
				Control::new(false),
				None,
				declared(Some(false)),
				VERSION,
			);
			let run = moq_clock::sim::spawn(publisher.run_publish_namespaces());

			let offered = async || {
				let mut stream = moq_clock::timeout(
					Duration::from_secs(10),
					Stream::accept(&mut crate::driver::Transport::new(peer.clone()), VERSION),
				)
				.await
				.expect("no advertisement")
				.unwrap();
				assert_eq!(stream.reader.varint().await.unwrap(), ietf::PublishNamespace::ID);
				let _: ietf::Body = stream.reader.decode().await.unwrap();
				stream
			};
			let mut stream = offered().await;
			for wait in [100, 200, 400, 800].map(Duration::from_millis) {
				let mut ok = bytes::Bytes::from(publish_namespace_ok(VERSION).await);
				stream.writer.write_all(&mut ok).await.unwrap();
				// Paused time advances once every task is idle: the answer was read, and the
				// advertisement is parked on its idle reader.
				moq_clock::sleep(Duration::from_millis(1)).await;
				match fin {
					true => stream.writer.finish().unwrap(),
					false => stream.writer.abort(&Error::Cancel),
				}
				let ended = moq_clock::Instant::now();
				stream = offered().await;
				assert!(
					ended.elapsed() >= wait,
					"fin={fin}: re-offered after {:?}",
					ended.elapsed()
				);
			}
			assert!(!run.is_finished(), "fin={fin}: the loop ended");
		}
	}

	/// A peer that resets or ends one namespace's PUBLISH_NAMESPACE before answering it ends
	/// only that advertisement: the next namespace is still answered and withdrawn as usual,
	/// and the failed one is offered again after a backoff rather than in a loop.
	#[moq_clock::test]
	async fn a_failed_advertisement_ends_only_its_namespace() {
		for version in [Version::Draft18] {
			for fin in [true, false] {
				fails_one_advertisement(version, fin).await;
			}
		}
	}

	async fn fails_one_advertisement(version: Version, fin: bool) {
		use crate::transport::memory;

		let origin = unheld_origin();
		let mut cams: HashMap<&str, _> = ["a-cam", "b-cam"]
			.into_iter()
			.map(|name| (name, origin.announce(name, crate::origin::Route::default()).unwrap()))
			.collect();
		settle().await;

		let (session, peer) = memory::pair(memory::Config::default());
		let publisher = Publisher::new(
			crate::time::sim(),
			crate::driver::Transport::new(session.clone()),
			origin.consume(),
			Control::new(false),
			None,
			declared(Some(false)),
			version,
		);
		let run = moq_clock::sim::spawn(publisher.run_publish_namespaces());

		// The next advertisement, with the namespace it names.
		let offered = async || {
			let mut stream = moq_clock::timeout(
				Duration::from_secs(10),
				Stream::accept(&mut crate::driver::Transport::new(peer.clone()), version),
			)
			.await
			.expect("no advertisement")
			.unwrap();
			assert_eq!(stream.reader.varint().await.unwrap(), ietf::PublishNamespace::ID);
			let body: ietf::Body = stream.reader.decode().await.unwrap();
			let name = match body.0.windows(5).any(|window| window == b"a-cam") {
				true => "a-cam",
				false => "b-cam",
			};
			(stream, name)
		};

		let (mut failed, first) = offered().await;
		match fin {
			true => failed.writer.finish().unwrap(),
			false => failed.writer.abort(&Error::Cancel),
		}
		let ended = moq_clock::Instant::now();

		let (mut held, second) = offered().await;
		assert_ne!(
			first, second,
			"{version} fin={fin}: the other namespace was never offered"
		);
		let mut ok = bytes::Bytes::from(publish_namespace_ok(version).await);
		held.writer.write_all(&mut ok).await.unwrap();

		let (mut again, name) = offered().await;
		assert_eq!(
			name, first,
			"{version} fin={fin}: the failed namespace was not offered again"
		);
		assert!(
			ended.elapsed() >= RETRY_BASE,
			"{version} fin={fin}: re-offered after {:?}",
			ended.elapsed()
		);
		// Answered, so the loop moves on now rather than withdrawing it at its deadline.
		let mut ok = bytes::Bytes::from(publish_namespace_ok(version).await);
		again.writer.write_all(&mut ok).await.unwrap();

		cams.remove(second);
		assert!(
			held.reader.varint().await.is_err(),
			"{version} fin={fin}: not withdrawn"
		);
		assert!(!run.is_finished(), "{version} fin={fin}: the loop ended");
	}

	/// A PUBLISH_NAMESPACE whose header fails to send backs off that namespace rather than
	/// closing the session.
	#[moq_clock::test]
	async fn a_failed_header_backs_off_the_namespace() {
		use crate::transport::memory;

		for version in [Version::Draft18] {
			let origin = unheld_origin();
			// A token too big for the header's u16 length, so the header fails after its stream
			// opened and its request ID was spent. A message goes out whole, so this is the one
			// failure a fresh in-memory stream can be made to show.
			let auth = crate::auth::Link::ietf(false);
			let token = auth.present(bytes::Bytes::from(vec![0; 70_000]), false).unwrap();
			let _cam = origin
				.announce("cam", crate::origin::Route::default().with_auth(token))
				.unwrap();
			settle().await;

			let (session, peer) = memory::pair(memory::Config::default());
			let publisher = Publisher::new(
				crate::time::sim(),
				crate::driver::Transport::new(session.clone()),
				origin.consume(),
				Control::new(false),
				None,
				declared(Some(false)),
				version,
			)
			.with_auth(auth.clone());
			let run = moq_clock::sim::spawn(publisher.run_publish_namespaces());

			moq_clock::timeout(
				Duration::from_secs(10),
				Stream::accept(&mut crate::driver::Transport::new(peer.clone()), version),
			)
			.await
			.expect("no advertisement")
			.unwrap();

			// Offered again after the backoff, on a session that stays open.
			moq_clock::timeout(
				Duration::from_secs(10),
				Stream::accept(&mut crate::driver::Transport::new(peer.clone()), version),
			)
			.await
			.unwrap_or_else(|_| panic!("{version}: not offered again"))
			.unwrap();
			assert!(!run.is_finished(), "{version}: the loop ended");
		}
	}

	/// A peer that resets a request's stream while its renewal is in flight ends only that
	/// request: the loop keeps running and advertises the namespace afresh.
	#[moq_clock::test]
	async fn a_reset_renewal_ends_only_its_request() {
		const VERSION: Version = Version::Draft18;

		let origin = unheld_origin();
		let auth = crate::auth::Link::ietf(false);
		let present = |token: &'static [u8]| auth.present(bytes::Bytes::from_static(token), false).unwrap();
		let cam = origin
			.announce("cam", crate::origin::Route::default().with_auth(present(b"first")))
			.unwrap();
		settle().await;

		let ok = publish_namespace_ok(VERSION).await;
		let session = crate::lite::test_transport::ScriptedSession::per_stream(vec![ok.clone(), ok]);
		let log = session.log.clone();
		let peer = session.clone();
		let publisher = Publisher::new(
			crate::time::sim(),
			crate::driver::Transport::new(session.clone()),
			origin.consume(),
			Control::new(false),
			None,
			declared(Some(false)),
			VERSION,
		)
		.with_auth(auth.clone());
		let mut run = std::pin::pin!(publisher.run_publish_namespaces());
		poll_until(&mut run, &log, b"first", 1).await;
		// The idle request has read its OK and parked, so the reset waits for the renewal
		// to wake it.
		peer.close(crate::lite::test_transport::Close::Reset);
		cam.update(crate::origin::Route::default().with_auth(present(b"second")))
			.unwrap();
		poll_until(&mut run, &log, b"second", 1).await;
		assert!(futures::poll!(run.as_mut()).is_pending());
		assert_eq!(log.resets().len(), 1, "the reset request was not withdrawn");

		// The advertisement comes back on the retry, jittered up to RETRY_BASE, which
		// outlasts poll_until.
		moq_clock::sleep(RETRY_BASE).await;
		poll_until(&mut run, &log, b"second", 2).await;
		assert_eq!(
			occurrences(&log, b"second"),
			2,
			"the renewal and the advertisement made again after the reset"
		);
		assert_eq!(log.bi_opens(), 2, "the reset request was replaced");
	}

	/// An update the peer never answers drops its request once the timeout passes, and
	/// the retry advertises the namespace afresh under the current auth.
	#[moq_clock::test]
	async fn an_unanswered_renewal_advertises_again() {
		advertises_again_unanswered(Version::Draft18).await;
	}

	async fn advertises_again_unanswered(version: Version) {
		let origin = unheld_origin();
		let auth = crate::auth::Link::ietf(false);
		let present = |token: &'static [u8]| auth.present(bytes::Bytes::from_static(token), false).unwrap();
		let cam = origin
			.announce("cam", crate::origin::Route::default().with_auth(present(b"first")))
			.unwrap();
		settle().await;

		let ok = publish_namespace_ok(version).await;
		let (publisher, log) = auth_publisher(&origin, &auth, vec![ok.clone(), ok], version);
		let mut run = std::pin::pin!(publisher.run_publish_namespaces());
		poll_until(&mut run, &log, b"first", 1).await;
		cam.update(crate::origin::Route::default().with_auth(present(b"second")))
			.unwrap();
		poll_until(&mut run, &log, b"second", 1).await;
		assert_eq!(occurrences(&log, b"second"), 1, "the renewal never went out");

		for _ in 0..20 {
			if occurrences(&log, b"second") >= 2 {
				break;
			}
			assert!(futures::poll!(run.as_mut()).is_pending());
			moq_clock::sleep(Duration::from_secs(1)).await;
		}
		assert_eq!(
			occurrences(&log, b"second"),
			2,
			"{version}: the unanswered renewal was never retried"
		);
	}

	/// moq-transport has no restart: another source winning a namespace without an epoch
	/// withdraws it and advertises it afresh, so the peer resubscribes.
	#[moq_clock::test]
	async fn a_restart_re_advertises_the_namespace() {
		const VERSION: Version = Version::Draft18;

		// Forward the restart at once: the hold is not what this checks.
		let origin = crate::origin::Config {
			update_hold: Duration::ZERO,
			..crate::origin::Config::new(crate::Hop::new(1).unwrap())
		}
		.produce();
		let _old = origin
			.announce("cam", crate::origin::Route::default().with_cost(4))
			.unwrap();
		settle().await;

		let ok = publish_namespace_ok(VERSION).await;
		let session = crate::lite::test_transport::ScriptedSession::per_stream(vec![ok.clone(), ok]);
		let log = session.log.clone();

		let publisher = Publisher::new(
			crate::time::sim(),
			crate::driver::Transport::new(session.clone()),
			origin.consume(),
			Control::new(false),
			None,
			clustered(Some(false)),
			VERSION,
		);

		let mut run = std::pin::pin!(publisher.run_publish_namespaces());
		for _ in 0..100 {
			assert!(futures::poll!(run.as_mut()).is_pending());
			if occurrences(&log, b"cam") >= 1 {
				break;
			}
			settle().await;
		}
		assert_eq!(occurrences(&log, b"cam"), 1, "the advertisement never went out");

		let _new = origin
			.announce("cam", crate::origin::Route::default().with_cost(0))
			.unwrap();
		for _ in 0..100 {
			assert!(futures::poll!(run.as_mut()).is_pending());
			if occurrences(&log, b"cam") >= 2 {
				break;
			}
			settle().await;
		}

		assert_eq!(occurrences(&log, b"cam"), 2, "advertised afresh");
		assert_eq!(log.bi_opens(), 2, "on a fresh request stream");
	}

	/// A peer that refuses an update closes the stream, which withdraws the
	/// advertisement. The namespace is then not held at all, so it comes back as a fresh
	/// PUBLISH_NAMESPACE once the refusal's wait is out, not as another update on a
	/// stream the peer already ended.
	#[moq_clock::test]
	async fn a_refused_update_is_re_advertised_fresh() {
		const VERSION: Version = Version::Draft18;

		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let epoch = crate::Epoch::mint();
		let _cold = origin
			.announce(
				"cam",
				crate::origin::Route::default().with_epoch(epoch.clone()).with_cost(4),
			)
			.unwrap();
		settle().await;

		// Stream 1 accepts the advertisement and refuses its update, with a wait shorter
		// than the retry sweep. The streams share one script, so the refusal is queued once
		// the idle request has read its OK.
		let ok = publish_namespace_ok(VERSION).await;
		let refusal = publish_namespace_error(VERSION, 50).await;
		let session = crate::lite::test_transport::ScriptedSession::new(ok);
		let log = session.log.clone();
		let peer = session.clone();

		let publisher = Publisher::new(
			crate::time::sim(),
			crate::driver::Transport::new(session.clone()),
			origin.consume(),
			Control::new(false),
			None,
			clustered(Some(false)),
			VERSION,
		);

		let mut run = std::pin::pin!(publisher.run_publish_namespaces());
		for _ in 0..100 {
			assert!(futures::poll!(run.as_mut()).is_pending());
			if occurrences(&log, b"cam") >= 1 {
				break;
			}
			settle().await;
		}
		assert_eq!(occurrences(&log, b"cam"), 1, "the advertisement never went out");

		peer.push(&refusal);
		let _warm = origin
			.announce("cam", crate::origin::Route::default().with_epoch(epoch).with_cost(0))
			.unwrap();

		for _ in 0..100 {
			assert!(futures::poll!(run.as_mut()).is_pending());
			if occurrences(&log, b"cam") >= 2 {
				break;
			}
			tick().await;
		}

		assert_eq!(occurrences(&log, b"cam"), 2, "re-advertised after the refusal");
		assert_eq!(log.bi_opens(), 2, "on a fresh request stream");
		let update = request_update(
			VERSION,
			&ietf::PublishNamespaceUpdate {
				request_id: RequestId(3),
				hops: None,
				cost: Some(0),
				authorization_token: None,
			},
		)
		.await;
		assert_eq!(occurrences(&log, &update), 1, "only the one update was attempted");
	}

	/// The peer granting a stream is only half the exchange. One it accepts and then never
	/// answers on wedges the loop exactly as a parked open does, so the response is bounded
	/// too: everything queued behind it is otherwise stranded for the session.
	#[moq_clock::test]
	async fn a_silent_answer_still_lets_the_next_namespace_be_advertised() {
		const VERSION: Version = Version::Draft18;

		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let _first = origin.announce("first-cam", crate::origin::Route::default()).unwrap();
		let _second = origin.announce("second-cam", crate::origin::Route::default()).unwrap();
		settle().await;

		// Every stream opens and then goes silent: an exhausted script parks rather than
		// reporting EOF, which is the peer that takes the request and answers nothing.
		let session = crate::lite::test_transport::ScriptedSession::per_stream(vec![Vec::new(), Vec::new()]);
		let log = session.log.clone();

		let publisher = Publisher::new(
			crate::time::sim(),
			crate::driver::Transport::new(session.clone()),
			origin.consume(),
			Control::new(false),
			None,
			declared(Some(false)),
			VERSION,
		);

		let mut run = std::pin::pin!(publisher.run_publish_namespaces());
		for _ in 0..200 {
			assert!(futures::poll!(run.as_mut()).is_pending());
			if occurrences(&log, b"first-cam") > 0 && occurrences(&log, b"second-cam") > 0 {
				break;
			}
			tick().await;
		}

		// Whichever went first is the one that stalled, so both having reached the wire is
		// the proof: the loop gave up on the answer and carried on.
		assert!(
			occurrences(&log, b"first-cam") > 0,
			"the first advertisement never went out"
		);
		assert!(
			occurrences(&log, b"second-cam") > 0,
			"the silent answer wedged the loop: the second namespace never went out"
		);
	}

	/// An advertisement the peer never answers is given up at its deadline: its own stream's
	/// reset ends it.
	#[moq_clock::test]
	async fn an_unanswered_advertisement_is_withdrawn() {
		for version in [Version::Draft18] {
			let origin = unheld_origin();
			let _cam = origin.announce("cam", crate::origin::Route::default()).unwrap();
			settle().await;

			// The peer takes the request and answers nothing.
			let session = crate::lite::test_transport::ScriptedSession::per_stream(vec![Vec::new()]);
			let log = session.log.clone();
			let publisher = Publisher::new(
				crate::time::sim(),
				crate::driver::Transport::new(session.clone()),
				origin.consume(),
				Control::new(false),
				None,
				declared(Some(false)),
				version,
			);
			let mut run = std::pin::pin!(publisher.run_publish_namespaces());
			poll_until(&mut run, &log, b"cam", 1).await;

			let withdrawn = || !log.resets().is_empty();
			moq_clock::sleep(ADVERTISE_TIMEOUT - Duration::from_millis(10)).await;
			assert!(futures::poll!(run.as_mut()).is_pending());
			assert!(!withdrawn(), "{version}: withdrawn before its deadline");

			moq_clock::sleep(Duration::from_millis(20)).await;
			assert!(futures::poll!(run.as_mut()).is_pending());
			assert!(
				withdrawn(),
				"{version}: the unanswered advertisement was never withdrawn"
			);
		}
	}

	/// Credit returning raises no signal of its own, so only the retry timer brings back an
	/// advertisement refused for credit; without it the namespace stays undiscoverable for
	/// the life of the session. The refusal itself must not spend a request ID.
	#[moq_clock::test]
	async fn an_advertisement_without_credit_is_retried_and_spends_no_id() {
		use std::sync::atomic::Ordering;
		for version in [Version::Draft18] {
			let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
			let _cam = origin.announce("lonely-cam", crate::origin::Route::default()).unwrap();
			settle().await;
			let session =
				crate::lite::test_transport::ScriptedSession::new(publish_namespace_ok(version).await).without_credit();
			let log = session.log.clone();
			let control = Control::new(false);
			let publisher = Publisher::new(
				crate::time::sim(),
				crate::driver::Transport::new(session.clone()),
				origin.consume(),
				control.clone(),
				None,
				declared(Some(false)),
				version,
			);
			let mut run = std::pin::pin!(publisher.run_publish_namespaces());
			assert!(futures::poll!(run.as_mut()).is_pending());
			assert_eq!(log.request_attempts.load(Ordering::Relaxed), 1, "{version}");
			assert_eq!(log.bi_opens(), 0, "{version}: refused without opening a stream");

			// Credit returns. Nothing else changes: no publish, no unannounce, no route move.
			session.grant_credit();
			for _ in 0..100 {
				assert!(futures::poll!(run.as_mut()).is_pending());
				if occurrences(&log, b"lonely-cam") > 0 {
					break;
				}
				tick().await;
			}
			assert_eq!(
				occurrences(&log, b"lonely-cam"),
				1,
				"{version}: never came back on its own"
			);
			assert!(log.closes().is_empty());
			assert_eq!(
				control.next_request_id(),
				RequestId(3),
				"{version}: only the admitted attempt spends an ID"
			);
		}
	}

	/// Send capacity returning raises no signal of its own: no announce, no route change, nothing
	/// the loop is watching. Only a retry brings the namespace back, and without one it
	/// stays undiscoverable for the life of the session.
	#[moq_clock::test]
	async fn an_advertisement_waiting_for_send_capacity_is_retried_on_its_own() {
		const VERSION: Version = Version::Draft18;

		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let _cam = origin.announce("lonely-cam", crate::origin::Route::default()).unwrap();
		settle().await;

		// No send capacity from the start: an admitted header cannot finish.
		let gate = kio::Producer::new(false);
		let ok = publish_namespace_ok(VERSION).await;
		let session = crate::lite::test_transport::ScriptedSession::gated_open(vec![ok], gate.consume());
		let log = session.log.clone();

		let publisher = Publisher::new(
			crate::time::sim(),
			crate::driver::Transport::new(session.clone()),
			origin.consume(),
			Control::new(false),
			None,
			declared(Some(false)),
			VERSION,
		);

		let mut run = std::pin::pin!(publisher.run_publish_namespaces());

		// Well past the point where the open gives up.
		for _ in 0..100 {
			assert!(futures::poll!(run.as_mut()).is_pending());
			tick().await;
		}
		assert_eq!(occurrences(&log, b"lonely-cam"), 0, "advertised without a stream");

		// Send capacity returns. Nothing else changes: no publish, no unannounce, no route move.
		set_gate(&gate, true);

		for _ in 0..100 {
			assert!(futures::poll!(run.as_mut()).is_pending());
			if occurrences(&log, b"lonely-cam") > 0 {
				break;
			}
			tick().await;
		}
		assert_eq!(occurrences(&log, b"lonely-cam"), 1, "never came back on its own");
	}

	/// Advance far enough that a parked open gives up and its retry comes due, without
	/// making the test wait: time is paused, so this only moves the clock the loop reads.
	async fn tick() {
		moq_clock::sim::advance(Duration::from_millis(200)).await;
	}

	fn set_gate(gate: &kio::Producer<bool>, open: bool) {
		let Ok(mut gate) = gate.write() else {
			panic!("gate closed")
		};
		*gate = open;
	}

	/// A publisher talking to a scripted peer that never answers, over one bidi stream.
	struct Harness {
		publisher: Publisher,
		session: crate::lite::test_transport::ScriptedSession,
		log: crate::lite::test_transport::Log,
		/// Keeps the origin alive; the publisher only holds a consumer.
		_origin: origin::Producer,
	}

	fn harness(version: Version) -> Harness {
		let origin = crate::origin::Config::new(crate::Hop::new(1).unwrap()).produce();
		let session = crate::lite::test_transport::ScriptedSession::per_stream(vec![Vec::new()]);
		let log = session.log.clone();

		// Serving a request blocks on the peer's SETUP, which no scripted peer sends here.
		let peer_setup = peer::PeerSetup::default();
		peer_setup.set(peer::Peer::default());

		let publisher = Publisher::new(
			crate::time::sim(),
			crate::driver::Transport::new(session.clone()),
			origin.consume(),
			Control::new(false),
			None,
			peer_setup,
			version,
		);

		Harness {
			publisher,
			session,
			log,
			_origin: origin,
		}
	}

	/// Subscribe to a path nothing publishes, returning what the peer would read off the
	/// request stream plus the reset codes the stream recorded.
	async fn subscribe_missing(version: Version) -> (Vec<u8>, Vec<u32>) {
		let h = harness(version);

		let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), version)
			.await
			.unwrap();
		h.publisher
			.clone()
			.run_subscribe_stream(
				stream,
				ietf::Subscribe {
					max_age: None,
					new_group: None,
					request_id: RequestId(1),
					track_namespace: crate::Path::from_static("nothing/here"),
					track_name: "video".into(),
					subscriber_priority: 128,
					group_order: GroupOrder::Descending,
					filter: Filter::NextObject,
					fill: None,
					properties_wanted: true,
					forward: true,
					rendezvous: false,
					range_filters: false,
					authorization_token: None,
					epoch: None,
				},
			)
			.await
			.unwrap();

		let writes = h.log.writes.lock().unwrap().clone();
		(writes, h.log.resets())
	}

	/// Send a FETCH we refuse, returning the same pair.
	async fn fetch_refused(version: Version, fetch_type: FetchType<'static>) -> (Vec<u8>, Vec<u32>) {
		let h = harness(version);

		let stream = Stream::open(&mut crate::driver::Transport::new(h.session.clone()), version)
			.await
			.unwrap();
		h.publisher
			.clone()
			.run_fetch_stream(
				stream,
				ietf::Fetch {
					request_id: RequestId(1),
					subscriber_priority: 128,
					group_order: GroupOrder::Descending,
					fetch_type,
					range_filters: false,
					fill_timeout: false,
					properties_wanted: true,
					epoch: None,
				},
			)
			.await
			.unwrap();

		let writes = h.log.writes.lock().unwrap().clone();
		(writes, h.log.resets())
	}

	/// A SUBSCRIBE for a path with no publisher is refused with REQUEST_ERROR, and the refusal
	/// has to survive the trip. `Writer` resets the stream on drop, and a reset that races the
	/// write discards the bytes the peer has not read yet, which leaves the subscriber waiting
	/// on a request we already refused. Finishing first makes the drop-time reset a no-op.
	#[moq_clock::test]
	async fn missing_broadcast_is_refused_without_resetting_the_stream() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			let (writes, resets) = subscribe_missing(version).await;

			assert!(!writes.is_empty(), "{version}: nothing was sent");
			assert_eq!(
				writes[0],
				ietf::RequestError::ID as u8,
				"{version}: not a REQUEST_ERROR"
			);
			assert!(resets.is_empty(), "{version}: stream reset, discarding the error");
		}
	}

	/// Every FETCH we refuse goes out through its own error encoder, so it needs the same
	/// finish: a reset there loses the rejection the same way.
	#[moq_clock::test]
	async fn a_refused_fetch_does_not_reset_the_stream() {
		let refused = || {
			[
				(
					"standalone",
					FetchType::Standalone {
						namespace: crate::Path::from_static("nothing/here"),
						track: "video".into(),
						start: Location { group: 0, object: 0 },
						end: Location { group: 1, object: 0 },
					},
				),
				(
					"empty range",
					FetchType::Standalone {
						namespace: crate::Path::from_static("nothing/here"),
						track: "video".into(),
						start: Location { group: 2, object: 0 },
						end: Location { group: 1, object: 0 },
					},
				),
			]
		};

		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			for (label, fetch_type) in refused() {
				let (writes, resets) = fetch_refused(version, fetch_type).await;

				assert!(!writes.is_empty(), "{version} {label}: nothing was sent");
				assert_eq!(
					writes[0],
					ietf::RequestError::ID as u8,
					"{version} {label}: not a REQUEST_ERROR"
				);
				assert!(
					resets.is_empty(),
					"{version} {label}: stream reset, discarding the error"
				);
			}
		}
	}
}

/// The live edge a SUBSCRIBE resolves against, snapshotted once so the subscription
/// floor, the fill cap, and the advertised LARGEST_OBJECT all agree on where it is.
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
struct LiveEdge {
	/// The newest group sequence, `None` before any group exists.
	latest: Option<u64>,
	/// The precise Largest Object. `None` when the track is empty, or when the newest
	/// group's frames cannot be read right now (none written yet), in which case nothing
	/// is advertised and no fill is servable.
	largest: Option<Location>,
	/// One past the Largest Object, which is where a Next Object subscription begins.
	/// When the edge is imprecise this falls back to the next group boundary: never below
	/// the true Next Object, at worst under-delivering the current group's tail.
	next: Option<Location>,
}

/// Snapshot the live edge of a track.
fn live_edge(track: &track::Consumer) -> LiveEdge {
	let reported = track
		.reported_object()
		.map(|(group, object)| Location { group, object });
	let cached = cached_edge(track);
	match reported.filter(|reported| {
		cached
			.largest
			.is_none_or(|cached| (reported.group, reported.object) > (cached.group, cached.object))
	}) {
		Some(largest) => LiveEdge {
			latest: cached.latest.max(Some(largest.group)),
			largest: Some(largest),
			next: Some(Location {
				group: largest.group,
				object: largest.object.saturating_add(1),
			}),
		},
		None => cached,
	}
}

/// The live objects immediately readable in this cache.
fn cached_edge(track: &track::Consumer) -> LiveEdge {
	let Some(latest) = track.latest_sequence() else {
		return LiveEdge::default();
	};

	match track.peek_latest() {
		Some(group) if group.sequence == latest => {
			let largest = match group.last_object() {
				Some(object) => Some(Location { group: latest, object }),
				// A group with no frames yet has no objects, so the largest sits in an
				// earlier group. Walk back through the cache to find it, or a peer that
				// subscribes in the instant between a group's creation and its first
				// frame is told the track is empty and gets no fill.
				None => largest_before(track, latest),
			};
			// One past the edge, even when the edge sits below the newest group: a group
			// may keep writing after a newer one exists, and a floor above the true Next
			// Object would strand those objects between the fill cap and the
			// subscription. With no readable object anywhere, the newest group's start
			// excludes nothing the cache can still name.
			let next = match largest {
				Some(largest) => Location {
					group: largest.group,
					object: largest.object.saturating_add(1),
				},
				None => Location {
					group: latest,
					object: 0,
				},
			};
			LiveEdge {
				latest: Some(latest),
				largest,
				next: Some(next),
			}
		}
		_ => LiveEdge {
			latest: Some(latest),
			largest: None,
			next: Some(Location {
				group: latest.saturating_add(1),
				object: 0,
			}),
		},
	}
}

/// The last object below `sequence`: the nearest earlier cached group that has started a
/// frame, walked in cache order so legal gaps in the group numbering are crossed. Empty
/// groups exist for at most the instant between creation and first frame, so the walk is
/// one step in practice. A group evicted from the cache is not visible, which is fine:
/// Largest Object is the track from this publisher's perspective, and that is the cache.
fn largest_before(track: &track::Consumer, sequence: u64) -> Option<Location> {
	let mut sequence = sequence;
	loop {
		let group = track.peek_before(sequence)?;
		if let Some(object) = group.last_object() {
			return Some(Location {
				group: group.sequence,
				object,
			});
		}
		sequence = group.sequence;
	}
}

/// The Locations a SUBSCRIBE's Location Filter selects, resolved against the live edge.
///
/// `start: None` joins at the beginning of the latest group, which is what moq-lite means
/// by joining a live track. An explicit start is honored down to the object: the start
/// group is served from `start.object` and the end group up to `end.object`, so a filter
/// is never widened into objects the subscriber excluded.
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
struct ServeRange {
	/// The first Location to serve, or `None` for the start of the latest group.
	start: Option<Location>,
	/// Where the range ends, inclusive. `None` is open ended. The subscription stays
	/// open once the range is exhausted; draft-22 removed the notion of a filter ending
	/// a subscription.
	end: Option<EndLocation>,
}

impl ServeRange {
	/// Where a fill covering this range's prefix leaves the live tail.
	fn fill_tail(self, fill: Option<FillServe>, edge: LiveEdge) -> Option<Location> {
		let next = edge.next?;
		let largest = edge.largest?;
		let requested = self.start.unwrap_or(Location {
			group: edge.latest.unwrap_or(0),
			object: 0,
		});
		if (requested.group, requested.object) > (next.group, next.object) {
			return None;
		}
		let (start, end) = match fill? {
			FillServe::Group { sequence, skip, until } => (
				Location {
					group: sequence,
					object: skip,
				},
				EndLocation {
					group: sequence,
					object: until.and_then(|until| until.checked_sub(1)),
				},
			),
			_ => return None,
		};
		((requested.group, requested.object) >= (start.group, start.object)
			&& end.group == largest.group
			&& end.object.is_none_or(|object| object >= largest.object))
		.then_some(next)
	}

	/// Whether this range drops objects a stream serving `slice` of group `sequence` would send.
	fn excludes(&self, sequence: u64, slice: GroupSlice) -> bool {
		let ours = self.slice(sequence);
		self.start.is_some_and(|start| sequence < start.group)
			|| self.end.is_some_and(|end| sequence > end.group)
			|| ours.skip > slice.skip
			|| ours
				.until
				.is_some_and(|until| slice.until.is_none_or(|theirs| until < theirs))
	}

	/// The objects of group `sequence` this range selects; interior groups are served whole.
	fn slice(&self, sequence: u64) -> GroupSlice {
		GroupSlice {
			skip: match self.start {
				Some(start) if start.group == sequence => start.object,
				_ => 0,
			},
			until: match self.end {
				Some(end) if end.group == sequence => end.object.map(|object| object.saturating_add(1)),
				_ => None,
			},
		}
	}

	/// Bound `subscription` to the whole groups this range touches: a model subscription
	/// starts and ends on a group boundary, and [`GroupSlice`] cuts the objects when serving.
	fn apply(&self, subscription: &mut Subscription) {
		*subscription = std::mem::take(subscription)
			.with_start(self.start.map(|start| start.group))
			.with_end(self.end.and_then(|end| end.group.checked_add(1)));
	}
}

/// The slice of one group a subscription's [`ServeRange`] selects.
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
struct GroupSlice {
	/// Frames dropped from the front; also the first written object's absolute id.
	skip: u64,
	/// One past the last object to write, when the filter ends inside this group.
	until: Option<u64>,
}

/// Resolve a SUBSCRIBE's Location Filter into the range to serve.
///
/// Next Object is honored on every draft so a joining FETCH owns the cached prefix.
/// Other draft-18 filters retain their existing serving behavior.
fn subscribe_range(filter: Filter, edge: LiveEdge, version: Version) -> ServeRange {
	if !Filter::is_draft20(version) && filter != Filter::NextObject {
		if !matches!(filter, Filter::NextObject | Filter::Unfiltered) {
			tracing::warn!(?filter, "filter not supported before draft-22, ignoring");
		}
		return ServeRange::default();
	}

	filter_range(filter, edge)
}

/// The Locations a single Location Filter selects, resolved against the live edge.
fn filter_range(filter: Filter, edge: LiveEdge) -> ServeRange {
	match filter {
		// No restriction. moq-lite starts at the beginning of the latest group, which is
		// the join point it is built around; a subscription passes objects as they are
		// published, so an absent filter is not a request to replay history.
		Filter::Unfiltered => ServeRange::default(),
		// `{Largest.Group, Largest.Object + 1}`. Everything below it, including the
		// already-published head of the current group, is outside the requested range,
		// so the join is mid-group by construction. The draft pairs this with a fill
		// when the subscriber wants the head; see `run_fill`.
		Filter::NextObject => ServeRange {
			start: edge.next,
			end: None,
		},
		// `{Largest.Group + 1 - groups, 0}`: 0 is the next group and 1 is the current one.
		// Counted from `Largest.Group`, which sits below the newest group while that
		// group has no objects yet; only with no largest at all does the newest group
		// stand in for it.
		Filter::Relative(groups) => ServeRange {
			start: edge
				.largest
				.map(|largest| largest.group)
				.or(edge.latest)
				.map(|group| Location {
					group: group.saturating_add(1).saturating_sub(groups),
					object: 0,
				}),
			end: None,
		},
		Filter::Absolute { start, end } => ServeRange {
			start: Some(start),
			end,
		},
	}
}

/// What a draft-22 fill request resolves to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FillServe {
	/// The range is empty, so no fetch stream is opened at all.
	Empty,
	/// A single group served from the cache: `skip` frames dropped from the front, and
	/// delivery stopping before `until` when set (the current group is capped at the
	/// Largest Object snapshot; a whole past group reads to its end).
	Group {
		sequence: u64,
		skip: u64,
		until: Option<u64>,
	},
	/// Object filters that this publisher cannot serve.
	Unsupported,
}

/// Resolve a fill request using the Fetch rules: relative to Largest Object and never
/// extending beyond it. An omitted Location Filter inherits the subscription's.
fn fill_range(fill: ietf::Fill, subscription: Filter, largest: Option<Location>) -> FillServe {
	// A Range Filter narrows which objects pass, which we do not implement; serving the
	// unfiltered range instead would deliver objects the peer excluded, so refuse it.
	if fill.range_filters {
		return FillServe::Unsupported;
	}
	let filter = fill.filter.unwrap_or(subscription);

	// Nothing published (or no precise edge to cap at) means no fill is servable; an
	// empty range opens no stream.
	let Some(largest) = largest else {
		return FillServe::Empty;
	};

	let start = match filter {
		// A Fetch without a filter is the whole track up to Largest Object.
		Filter::Unfiltered => Location { group: 0, object: 0 },
		// One past the edge, which for a Fetch is always empty.
		Filter::NextObject => return FillServe::Empty,
		Filter::Relative(groups) => Location {
			group: largest.group.saturating_add(1).saturating_sub(groups),
			object: 0,
		},
		Filter::Absolute { start, .. } => start,
	};

	// Cap the requested end at Largest Object.
	let end = match filter {
		Filter::Absolute { end: Some(end), .. }
			if end.group < largest.group
				|| (end.group == largest.group && end.object.is_some_and(|object| object < largest.object)) =>
		{
			end
		}
		_ => EndLocation {
			group: largest.group,
			object: Some(largest.object),
		},
	};

	if start.group > end.group || (start.group == end.group && end.object.is_some_and(|object| object < start.object)) {
		return FillServe::Empty;
	}
	if start.group != largest.group || end.group != largest.group {
		return FillServe::Unsupported;
	}

	FillServe::Group {
		sequence: start.group,
		skip: start.object,
		until: end.object.map(|object| object.saturating_add(1)),
	}
}

#[cfg(test)]
mod range_tests {
	use super::*;
	use crate::ietf::EndLocation;
	use crate::ietf::GroupOrder;

	fn subscribe(filter: Filter) -> ietf::Subscribe<'static> {
		ietf::Subscribe {
			max_age: None,
			new_group: None,
			request_id: RequestId(1),
			track_namespace: crate::Path::from_static("broadcast"),
			track_name: "video".into(),
			subscriber_priority: 128,
			group_order: GroupOrder::Descending,
			filter,
			fill: None,
			properties_wanted: true,
			forward: true,
			rendezvous: false,
			range_filters: false,
			authorization_token: None,
			epoch: None,
		}
	}

	/// A live edge of group 100 whose current group has objects 0 through 4.
	const EDGE: LiveEdge = LiveEdge {
		latest: Some(100),
		largest: Some(Location { group: 100, object: 4 }),
		next: Some(Location { group: 100, object: 5 }),
	};

	/// A start past the live edge is what the subscriber asked for, so it is used as given.
	/// Clamping it to the live edge would serve a group outside the requested range.
	#[moq_clock::test]
	async fn a_future_start_is_not_clamped_to_the_live_edge() {
		let track = track::Producer::new(
			std::sync::Arc::new(crate::broadcast::Info::default()),
			"video",
			crate::track::Info::timed(),
		);
		track
			.create_group(group::Info { sequence: 7 })
			.unwrap()
			.finish()
			.unwrap();
		track
			.create_group(group::Info { sequence: 8 })
			.unwrap()
			.finish()
			.unwrap();

		// Next Group against a live edge of 8 asks for 9, which does not exist yet.
		let mut subscriber = track.subscribe(None);
		subscriber.set_range(9..);
		assert!(
			futures::poll!(std::pin::pin!(subscriber.recv_group())).is_pending(),
			"a future start must wait for its group rather than serving the live edge"
		);

		// The group it asked for is what it gets once published.
		track
			.create_group(group::Info { sequence: 9 })
			.unwrap()
			.finish()
			.unwrap();
		let group = subscriber.recv_group().await.unwrap().expect("group 9");
		assert_eq!(group.sequence, 9);
	}

	/// Earlier drafts never had their absolute filters served, so honoring one now would
	/// change what an existing peer receives.
	#[test]
	fn older_drafts_are_ignored() {
		let msg = subscribe(Filter::Absolute {
			start: Location { group: 4, object: 0 },
			end: Some(EndLocation { group: 9, object: None }),
		});
		assert_eq!(
			subscribe_range(msg.filter, EDGE, Version::Draft18),
			ServeRange::default()
		);
	}

	/// An absent filter is "no restriction on what is forwarded", not a request for
	/// history, so it joins at the live edge.
	#[test]
	fn an_unfiltered_subscription_stays_live() {
		let msg = subscribe(Filter::Unfiltered);
		assert_eq!(
			subscribe_range(msg.filter, EDGE, Version::Draft22),
			ServeRange::default()
		);
	}

	/// Next Object starts one past the Largest Object, mid-group. Everything below it,
	/// including the current group's head, is outside the requested range.
	#[test]
	fn next_object_starts_past_the_largest_object() {
		let msg = subscribe(Filter::NextObject);
		assert_eq!(
			subscribe_range(msg.filter, EDGE, Version::Draft22),
			ServeRange {
				start: Some(Location { group: 100, object: 5 }),
				end: None,
			}
		);
	}

	/// When the edge cannot be read precisely, Next Object falls back to the next group
	/// boundary: never below the true Next Object, so nothing already published is sent.
	#[test]
	fn next_object_without_a_precise_edge_waits_for_the_next_group() {
		let edge = LiveEdge {
			latest: Some(100),
			largest: None,
			next: Some(Location { group: 101, object: 0 }),
		};
		let msg = subscribe(Filter::NextObject);
		assert_eq!(
			subscribe_range(msg.filter, edge, Version::Draft22),
			ServeRange {
				start: Some(Location { group: 101, object: 0 }),
				end: None,
			}
		);
	}

	/// `{Largest.Group + 1 - groups, 0}`: one is the current group, zero is the next one,
	/// and larger values reach further back.
	#[test]
	fn relative_counts_back_from_the_next_group() {
		for (groups, expected) in [(0, 101), (1, 100), (2, 99), (5, 96)] {
			let msg = subscribe(Filter::Relative(groups));
			assert_eq!(
				subscribe_range(msg.filter, EDGE, Version::Draft22),
				ServeRange {
					start: Some(Location {
						group: expected,
						object: 0,
					}),
					end: None,
				},
				"{groups} groups back"
			);
		}
	}

	/// Relative counts from `Largest.Group`, which is below the newest group while that
	/// group has no objects yet, so a current-group join still reaches the content.
	#[test]
	fn relative_counts_from_the_largest_group_over_an_empty_newest_group() {
		let edge = LiveEdge {
			latest: Some(1),
			largest: Some(Location { group: 0, object: 2 }),
			next: Some(Location { group: 0, object: 3 }),
		};
		let msg = subscribe(Filter::Relative(1));
		assert_eq!(
			subscribe_range(msg.filter, edge, Version::Draft22),
			ServeRange {
				start: Some(Location { group: 0, object: 0 }),
				end: None,
			}
		);
	}

	/// Counting back further than the track goes lands at its start rather than wrapping.
	#[test]
	fn relative_saturates_at_the_start() {
		let msg = subscribe(Filter::Relative(500));
		assert_eq!(
			subscribe_range(msg.filter, EDGE, Version::Draft22),
			ServeRange {
				start: Some(Location { group: 0, object: 0 }),
				end: None,
			}
		);
	}

	/// Nothing published yet means there is no edge to count back from.
	#[test]
	fn relative_without_an_edge_stays_live() {
		let msg = subscribe(Filter::Relative(3));
		assert_eq!(
			subscribe_range(msg.filter, LiveEdge::default(), Version::Draft22),
			ServeRange::default()
		);
	}

	/// A group created but not yet written has no objects, so the largest sits in an
	/// earlier group. Losing it would tell a fill-requesting peer the track is empty, and
	/// a floor above the true Next Object would strand a late object of the earlier group
	/// between the fill cap and the subscription: a group may keep writing after a newer
	/// one exists, so the earlier group is deliberately left unfinished here.
	#[test]
	fn an_empty_newest_group_walks_back_for_the_largest() {
		let track = track::Producer::new(
			std::sync::Arc::new(crate::broadcast::Info::default()),
			"video",
			crate::track::Info::timed(),
		);
		let mut first = track.create_group(group::Info { sequence: 0 }).unwrap();
		for _ in 0..3 {
			first
				.write_frame(crate::Timestamp::from_millis(0).unwrap(), b"frame".as_slice())
				.unwrap();
		}
		let _open = track.create_group(group::Info { sequence: 1 }).unwrap();

		let edge = live_edge(&track.consume());
		assert_eq!(edge.latest, Some(1));
		assert_eq!(
			edge.largest,
			Some(Location { group: 0, object: 2 }),
			"the largest object is the previous group's last frame"
		);
		assert_eq!(
			edge.next,
			Some(Location { group: 0, object: 3 }),
			"the floor is one past the largest, so a late object of group 0 is not stranded"
		);
	}

	/// Anything received past the upstream's last reported Largest Object outdates it, a
	/// later object of its group or a newer group, even once the cache evicted what arrived,
	/// so the edge never moves back to the report. A report that lands after a frame past it
	/// arrived is stale on arrival.
	#[test]
	fn received_objects_outdate_the_report_after_eviction() {
		for (sequence, frames, reported_first) in [(3, 6, true), (3, 6, false), (9, 1, true), (9, 1, false)] {
			let track = track::Producer::new(
				std::sync::Arc::new(crate::broadcast::Info::default()),
				"video",
				crate::track::Info::timed(),
			);
			if reported_first {
				track.report_object(3, 4);
			}
			let mut newer = track.create_group(group::Info { sequence }).unwrap();
			for _ in 0..frames {
				newer
					.write_frame(crate::Timestamp::from_millis(0).unwrap(), b"frame".as_slice())
					.unwrap();
			}
			if !reported_first {
				track.report_object(3, 4);
			}
			newer.abort(Error::Evicted).unwrap();

			let edge = live_edge(&track.consume());
			assert_eq!(edge.latest, Some(sequence));
			assert_eq!(
				edge.largest, None,
				"group {sequence}: the stale report is not the largest"
			);
		}
	}

	/// A relay holding groups of a requested track stages them before committing, so the
	/// Largest Location that SUBSCRIBE_OK snapshots as soon as the subscribe resolves always
	/// names the replayed group (#5862). Raced on real threads, many times.
	#[test]
	fn subscribe_ok_reports_a_staged_replay() {
		for _ in 0..500 {
			let broadcast = crate::broadcast::Info::new().produce();
			let request = broadcast.reserve_track("video").unwrap();
			let cache = broadcast.consume().track("video").unwrap();
			let snapshot = std::thread::spawn(move || {
				futures::executor::block_on(async {
					let mut track = cache.subscribe(None).await.unwrap();
					kio::wait(|waiter| track.poll_live(waiter)).await;
					live_edge(&cache).largest
				})
			});
			let staged = request.stage(crate::track::Info::timed());
			for sequence in 0..3 {
				let mut group = staged.create_group(group::Info { sequence }).unwrap();
				for _ in 0..2 {
					group
						.write_frame(crate::Timestamp::from_millis(sequence).unwrap(), b"frame".as_slice())
						.unwrap();
				}
				group.finish().unwrap();
			}
			let _track = staged.commit();
			assert_eq!(snapshot.join().unwrap(), Some(Location { group: 2, object: 1 }));
		}
	}

	/// Group numbering may legally skip sequences, so the walk follows the cache's own
	/// order rather than decrementing by one.
	#[test]
	fn the_walkback_crosses_a_gap_in_the_numbering() {
		let track = track::Producer::new(
			std::sync::Arc::new(crate::broadcast::Info::default()),
			"video",
			crate::track::Info::timed(),
		);
		let mut first = track.create_group(group::Info { sequence: 0 }).unwrap();
		first
			.write_frame(crate::Timestamp::from_millis(0).unwrap(), b"frame".as_slice())
			.unwrap();
		first.finish().unwrap();
		// Sequence 1 never exists; the newest group is empty.
		let _open = track.create_group(group::Info { sequence: 2 }).unwrap();

		let edge = live_edge(&track.consume());
		assert_eq!(edge.latest, Some(2));
		assert_eq!(edge.largest, Some(Location { group: 0, object: 0 }));
		assert_eq!(edge.next, Some(Location { group: 0, object: 1 }));
	}

	/// Both ends carry through, object bounds included, so the boundary groups can be
	/// trimmed rather than widened.
	#[test]
	fn absolute_carries_both_ends() {
		let msg = subscribe(Filter::Absolute {
			start: Location { group: 4, object: 3 },
			end: Some(EndLocation {
				group: 9,
				object: Some(6),
			}),
		});
		assert_eq!(
			subscribe_range(msg.filter, EDGE, Version::Draft22),
			ServeRange {
				start: Some(Location { group: 4, object: 3 }),
				end: Some(EndLocation {
					group: 9,
					object: Some(6)
				}),
			}
		);
	}
}

#[cfg(test)]
mod fill_range_tests {
	use super::*;
	use crate::ietf::GroupOrder;

	/// Objects 0 through 4 of group 100 are published.
	const LARGEST: Option<Location> = Some(Location { group: 100, object: 4 });

	/// A fill with an explicit Location Filter and no range filters.
	fn fill(filter: Filter) -> ietf::Fill {
		ietf::Fill {
			group_order: None,
			filter: Some(filter),
			range_filters: false,
		}
	}

	/// The canonical current-group join: a fill one group back covers the published head
	/// of the current group, capped at the Largest Object snapshot.
	#[test]
	fn current_group_fill() {
		assert_eq!(
			fill_range(fill(Filter::Relative(1)), Filter::NextObject, LARGEST),
			FillServe::Group {
				sequence: 100,
				skip: 0,
				until: Some(5),
			}
		);
	}

	/// A fill of the next group starts past the Largest Object, which for a Fetch is
	/// always empty, as is an explicit Next Object.
	#[test]
	fn a_future_fill_is_empty() {
		assert_eq!(
			fill_range(fill(Filter::Relative(0)), Filter::NextObject, LARGEST),
			FillServe::Empty
		);
		assert_eq!(
			fill_range(fill(Filter::NextObject), Filter::NextObject, LARGEST),
			FillServe::Empty
		);
	}

	/// Nothing published means every fill range is empty; no stream is owed.
	#[test]
	fn no_content_means_no_fill() {
		assert_eq!(
			fill_range(fill(Filter::Relative(1)), Filter::NextObject, None),
			FillServe::Empty
		);
		assert_eq!(
			fill_range(fill(Filter::Unfiltered), Filter::NextObject, None),
			FillServe::Empty
		);
	}

	/// A fill of a past group is refused even when it names only one group.
	#[test]
	fn a_past_group_fill_is_unsupported() {
		assert_eq!(
			fill_range(
				fill(Filter::Absolute {
					start: Location { group: 7, object: 0 },
					end: Some(EndLocation { group: 7, object: None }),
				}),
				Filter::NextObject,
				LARGEST
			),
			FillServe::Unsupported
		);
	}

	/// Object bounds inside the group carry through to the served slice.
	#[test]
	fn object_bounds_trim_the_group() {
		assert_eq!(
			fill_range(
				fill(Filter::Absolute {
					start: Location { group: 100, object: 2 },
					end: Some(EndLocation {
						group: 100,
						object: Some(3)
					}),
				}),
				Filter::NextObject,
				LARGEST
			),
			FillServe::Group {
				sequence: 100,
				skip: 2,
				until: Some(4),
			}
		);
	}

	/// An end past the edge is capped at the Largest Object, per the Fetch rules.
	#[test]
	fn the_end_is_capped_at_the_largest_object() {
		assert_eq!(
			fill_range(
				fill(Filter::Absolute {
					start: Location { group: 100, object: 0 },
					end: Some(EndLocation {
						group: 100,
						object: Some(1000),
					}),
				}),
				Filter::NextObject,
				LARGEST
			),
			FillServe::Group {
				sequence: 100,
				skip: 0,
				until: Some(5),
			}
		);
	}

	/// A fill never promises history spanning several groups, regardless of group order.
	#[test]
	fn a_multi_group_fill_is_unsupported() {
		for filter in [
			Filter::Unfiltered,
			Filter::Relative(3),
			Filter::Absolute {
				start: Location { group: 7, object: 2 },
				end: Some(EndLocation { group: 9, object: None }),
			},
		] {
			for order in [GroupOrder::Ascending, GroupOrder::Descending] {
				let mut request = fill(filter);
				request.group_order = Some(order);
				assert_eq!(fill_range(request, Filter::NextObject, LARGEST), FillServe::Unsupported);
			}
		}
	}

	/// A Range Filter narrows which objects pass; refusing beats serving objects the
	/// peer excluded.
	#[test]
	fn a_range_filtered_fill_is_unsupported() {
		let fill = ietf::Fill {
			group_order: None,
			filter: Some(Filter::Relative(1)),
			range_filters: true,
		};
		assert_eq!(fill_range(fill, Filter::NextObject, LARGEST), FillServe::Unsupported);
	}

	/// An omitted Location Filter inherits the subscription's, per the draft: a fill
	/// scope carries only the settings that differ.
	#[test]
	fn an_omitted_filter_inherits_the_subscription() {
		let empty = ietf::Fill::default();
		// A Next Object subscription inherited into a Fetch is always empty.
		assert_eq!(fill_range(empty, Filter::NextObject, LARGEST), FillServe::Empty);
		// A current-group subscription inherited into the fill covers its head.
		assert_eq!(
			fill_range(empty, Filter::Relative(1), LARGEST),
			FillServe::Group {
				sequence: 100,
				skip: 0,
				until: Some(5),
			}
		);
	}

	/// A backwards range is empty, not an error.
	#[test]
	fn a_backwards_range_is_empty() {
		assert_eq!(
			fill_range(
				fill(Filter::Absolute {
					start: Location { group: 7, object: 5 },
					end: Some(EndLocation {
						group: 7,
						object: Some(2)
					}),
				}),
				Filter::NextObject,
				LARGEST
			),
			FillServe::Empty
		);
	}

	/// The whole track fits in one group only when the track has exactly one group.
	#[test]
	fn unfiltered_with_one_group_is_the_canonical_fill() {
		assert_eq!(
			fill_range(
				fill(Filter::Unfiltered),
				Filter::NextObject,
				Some(Location { group: 0, object: 9 })
			),
			FillServe::Group {
				sequence: 0,
				skip: 0,
				until: Some(10),
			}
		);
	}
}

#[cfg(test)]
mod compression_tests {
	use super::*;
	#[test]
	fn sealed_compression_refuses_incompatible_egress() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			let mut sealed = Vec::new();
			let mut encoder = Encoder::new(&mut sealed, version.into());
			encoder.varint(super::super::flate::COMPRESSION).unwrap();
			encoder.varint(1).unwrap();
			let info = track::Info {
				compress: true,
				properties: Some(Arc::new(crate::model::object::TrackProperties {
					version,
					values: vec![crate::model::object::Property {
						id: 0x0b,
						value: crate::model::object::Value::Bytes(sealed.into()),
					}],
				})),
				..Default::default()
			};
			let outgoing = track_properties(&info, true, version, true).unwrap();
			assert!(outgoing.compress);
			let mut bytes = Vec::new();
			outgoing
				.encode(&mut Encoder::new(&mut bytes, version.into()), version)
				.unwrap();
			let decoded =
				ietf::Properties::decode(&mut crate::coding::Decoder::new(&bytes, version.into()), version).unwrap();
			assert!(decoded.compress);
			assert_eq!(decoded.unknown, outgoing.unknown);
			let mut contradiction = info.clone();
			contradiction.compress = false;
			assert!(
				contradiction
					.properties
					.as_ref()
					.unwrap()
					.refused(&contradiction)
					.is_some()
			);
			for wanted in [false, true] {
				assert!(matches!(
					track_properties(&info, wanted, version, false),
					Err(Error::Unsupported)
				));
			}
			let plain = track::Info::default().with_compress(true);
			assert!(!track_properties(&plain, true, version, false).unwrap().compress);
		}
	}

	/// The actual JS SETUP offer selects the plain egress of a compressed Rust track.
	#[moq_clock::test]
	#[ignore = "requires Bun; run by just test interop"]
	async fn ietf_compression_fallback_interop() {
		use crate::coding::Decode;
		use crate::lite::test_transport::{Log, SinkSession};
		for (version, draft) in [(Version::Draft18, 18), (Version::Draft22, 22)] {
			let input = serde_json::json!({"draft": draft});
			let bytes: Vec<u8> =
				serde_json::from_slice(&crate::test_interop::bun("ietf-compression.ts", input)).unwrap();
			let (parameters, consumed) = ietf::Parameters::decode_slice(&bytes, version).unwrap();
			assert_eq!(consumed, bytes.len());
			let negotiated = super::super::flate::from_setup(&parameters).unwrap();
			assert!(!negotiated, "JS offered an unsupported codec");
			let track = track::Producer::new(
				Arc::new(crate::broadcast::Info::default()),
				"data",
				track::Info::timed().with_compress(true),
			);
			let subscriber = track.subscribe(None);
			let frames = [vec![b'a'; 1024], vec![b'b'; 1024], Vec::new()];
			let mut group = track.append_group().unwrap();
			for payload in &frames {
				group.write_frame(crate::Timestamp::ZERO, payload.clone()).unwrap();
			}
			group.finish().unwrap();
			track.finish().unwrap();
			let response = ietf::SubscribeOk {
				track_alias: 0,
				largest: None,
				properties: track_properties(track.info(), true, version, negotiated).unwrap(),
			}
			.encode_bytes(version)
			.unwrap();
			let log = Log::default();
			let mut serve = TrackServe::new(
				crate::driver::Transport::new(SinkSession::new(log.clone())),
				subscriber,
				RequestId(0),
				version,
				ServeRange::default(),
				Some(Timescale::MILLI),
			)
			.compression(negotiated);
			kio::wait(|waiter| serve.poll(waiter)).await.unwrap();
			let written = log.writes.lock().unwrap().clone();
			let input = serde_json::json!({"draft": draft, "response": response.to_vec(), "group": written});
			let received: Vec<Vec<u8>> =
				serde_json::from_slice(&crate::test_interop::bun("ietf-compression.ts", input)).unwrap();
			assert_eq!(received, frames, "draft{draft}: plain JS fallback");
		}
	}
}
