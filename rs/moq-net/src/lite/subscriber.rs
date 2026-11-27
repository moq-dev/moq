use crate::driver::Transport;
use crate::transport::poll::Session as _;
use crate::{datagram, frame, group, origin, track};
use std::{
	collections::{BTreeMap, BTreeSet, HashMap},
	sync::{Arc, atomic},
	task::{Poll, ready},
	time::Duration,
};

use crate::{
	Error, Path, Timescale, Timestamp, bandwidth,
	coding::{Decode as _, Encode as _, Reader, Recv, Stream},
	lite,
	track::{Position, Subscription},
	util::SendCell,
};

use super::Version;
use crate::tail::{self, Reading, Settle, Tail};

use kio::Lock;

mod fills;
use fills::{FillAsk, RangeFills, Recovery, Refusal};
mod fit;
pub(super) mod route_stream;
mod routed;

pub(super) struct SubscriberConfig {
	pub runtime: crate::time::Deadlines,
	pub session: Transport,
	/// The origin into which remote broadcasts are inserted. Traffic stats are
	/// attributed through this handle: tag it with [`origin::Producer::with_stats`]
	/// first.
	pub origin: origin::Producer,
	/// Receiver-side bandwidth producer for PROBE feedback. None disables the
	/// feature (used by versions that don't carry probe streams).
	pub recv_bandwidth: Option<bandwidth::Producer>,
	pub version: Version,
	/// Shared slot for the peer's SETUP (lite-05+). Written when the peer's Setup
	/// stream is read; the probe stream waits on it before opening.
	pub peer_setup: super::PeerSetup,
	/// The origin (hop) id assigned to the peer, used whenever the peer doesn't
	/// declare one itself. See `Client::with_peer_node`.
	pub peer_node: Option<crate::Hop>,
	/// Local policy for what pulling from this peer costs, overriding whatever it
	/// declared in its SETUP. `None` charges the peer's declared price.
	pub cost: Option<u64>,
	/// Set once the peer sends a GOAWAY; this session's routes then rank below every
	/// available route (draining), so a replacement session outranks it.
	pub going_away: crate::goaway::GoingAway,
	/// Our tokens' grants: a subscription the union stops covering is cancelled.
	pub auth: crate::auth::Link,
}

#[derive(Clone)]
pub(super) struct Subscriber {
	runtime: crate::time::Deadlines,
	session: Transport,

	origin: origin::Producer,
	recv_bandwidth: Option<bandwidth::Producer>,
	// Session-level origin id shared with the Publisher. Used to drop reflected
	// announces: any incoming announce whose hop chain already passed through us
	// has looped, so it is neither used as a route nor forwarded. On lite-04/05
	// we also ask the peer to filter them out (AnnounceRequest.exclude_hop) so
	// they never hit the wire, but this check is what makes it correct.
	self_origin: crate::Hop,
	// The origin stored as `Route.via` for broadcasts from versions that don't
	// carry real hop ids on the wire (Lite01/02/03), and for a peer that reports
	// 0 in AnnounceOk. Lite03 placeholders stay 0 and count as anonymous.
	//
	// This is the peer's assigned identity (`peer_node`): a fresh id per dialed or
	// accepted session, so its routes are distinguishable from another session's,
	// unless the caller pinned a stable one with `with_peer_node`. Without one it is
	// `Hop::UNKNOWN` (0), the reserved "no identity" value. The assigned id stays
	// local and is never written into a hop chain.
	session_origin: crate::Hop,
	subscribes: Lock<HashMap<u64, TrackEntry>>,
	/// The datagram tracks subscribed upstream, by Subscribe ID, once their datagrams can be
	/// read.
	datagrams: Lock<HashMap<u64, DatagramEntry>>,
	/// Lite-05/06: the datagram tracks taking the datagrams of their name's stream track
	/// subscription, by broadcast and name. One SUBSCRIBE carries both kinds there.
	legacy_datagrams: Lock<HashMap<(Path, String), DatagramEntry>>,
	/// The datagram tracks ended, and on lite-05/06 the subscriptions carrying them, while the
	/// driver routes the datagrams received with their end; see [`Self::end_datagrams`].
	datagram_ends: Lock<DatagramEnds>,
	/// Datagrams received for a subscription with no datagram track, which
	/// [`crate::session::Stats::datagrams_unrouted`] reports.
	pub(super) datagrams_unrouted: Arc<atomic::AtomicU64>,
	/// Why this session ended, once it has. A track still waiting on TRACK_INFO has no
	/// copy in [`Self::subscribes`], so dropping its request reads this instead of
	/// becoming [`Error::Dropped`].
	ended: Lock<Option<Error>>,
	next_id: Arc<atomic::AtomicU64>,
	version: Version,
	/// The peer's advertised SETUP (lite-05+), set when its Setup stream is read.
	peer_setup: super::PeerSetup,
	/// GROUP streams parked until the peer's SETUP says how to read their header.
	setup_waits: Arc<atomic::AtomicUsize>,
	/// GROUP streams parked until their subscription's TRACK_INFO says how to read them.
	park_waits: Arc<atomic::AtomicUsize>,
	/// Local policy overriding the peer's declared egress price. See `poll_link_cost`.
	cost: Option<u64>,
	/// Sources minted by the announce half, drained by the driver into
	/// [`SourceServe`] machines.
	sources: kio::Queue<MintedSource>,
	/// Lite-07: the Announce Streams the peer opened, for the route layer to read. It
	/// allows the peer one.
	announce_streams: kio::Queue<route_stream::Reader>,
	going_away: crate::goaway::GoingAway,
	auth: crate::auth::Link,
	/// Broadcasts the peer may have announced at once (`session::Limits::announces`).
	pub(super) announces: crate::session::Slots,
	/// The viewers whose sum the PROBE stream sends as its target.
	pub(super) viewers: bandwidth::Viewers,
	/// The instances minted for the paths the peer advertises without one, shared by the
	/// announce streams.
	minted: Minted,
}

/// The instance a session minted for each path a peer advertises without one (every
/// version before lite-07), shared by its announce streams: a peer serves one broadcast at
/// a path at a time, so the path advertised on two streams at once is one stream. The peer
/// ending it on any stream retires its instance, so the next advertisement is a new stream
/// with a fresh one, even while a lagging stream still holds the old: holders alone cannot
/// tell a replacement from continuity. A stream closing only lets go of it.
#[derive(Clone, Default)]
struct Minted(Lock<HashMap<Path, (crate::Instance, usize)>>);

impl Minted {
	fn acquire(&self, path: &Path) -> crate::Instance {
		let mut minted = self.0.lock();
		let (instance, holders) = minted
			.entry(path.clone())
			.or_insert_with(|| (crate::Instance::minted(), 0));
		*holders += 1;
		instance.clone()
	}

	/// Let go of `instance`, retiring it if the peer `ended` its advertisement. A no-op once
	/// another stream's end retired it.
	fn release(&self, path: &Path, instance: &crate::Instance, ended: bool) {
		let mut minted = self.0.lock();
		let Some((current, holders)) = minted.get_mut(path) else {
			return;
		};
		if current != instance {
			return;
		}
		*holders -= 1;
		if ended || *holders == 0 {
			minted.remove(path);
		}
	}
}

/// A datagram track subscribed upstream, whose datagrams the session routes to it.
struct DatagramEntry {
	producer: datagram::Producer,
	/// The units of every datagram Timestamp on the wire, or `None` when they carry none.
	timescale: Option<Timescale>,
}

/// What the sources ended, applied once the driver read the datagrams queued with it.
#[derive(Default)]
struct DatagramEnds {
	/// Lite-07: the datagram tracks the publisher finished, by Subscribe ID.
	tracks: Vec<(u64, datagram::Producer)>,
	/// Lite-05/06: the datagram tracks whose stream track finished, by broadcast and name.
	shared: Vec<((Path, String), datagram::Producer)>,
	/// Lite-05/06: the subscriptions finished, by Subscribe ID, with the broadcast and name
	/// whose datagram track takes their datagrams.
	subscribes: Vec<(u64, (Path, String))>,
	/// Closed once these ends apply. Each serve that queued one keeps its stream until then, so
	/// the peer's stream credit bounds what a datagram flood can leave queued.
	applied: kio::Producer<()>,
}

/// Where an active subscription routes incoming GROUP streams.
enum TrackTarget {
	/// Registered before `TRACK_INFO` lands; incoming groups park until ready.
	Pending,
	/// A track copy fed by a normal or recovery `SUBSCRIBE`.
	Copy(Accepted),
	/// Lite-07: a single-group bounded fill served via one-shot `SUBSCRIBE`.
	Fill(FillTarget),
}

struct TrackEntry {
	/// Where the subscription's groups go, once ready.
	target: TrackTarget,
	/// Lite-07: group keys that were ever capped mid-group by a `Position` End on this
	/// subscription, so a bare FIN without `GROUP_END` marks them back-cut.
	capped: std::collections::HashSet<u64>,
	/// The group streams that arrived before the copy, while any wait. Parked groups hold
	/// group writers, which reach the whole track model: the cell proves `Send` once here, so
	/// the session driver's proof stays within the default recursion limit.
	parked: Option<SendCell<kio::Producer<Parked>>>,
	/// The groups received so far, so the subscription's end can wait for the ones owed.
	tail: kio::Producer<Tail>,
	/// Lite-05/06: where the feed starts, from SUBSCRIBE_START.
	start: Option<u64>,
	/// Lite-05/06: the first group header, which says where the live feed is.
	first: Option<Position>,
	/// Lite-05/06: the broadcast and name whose datagram track takes the datagrams this
	/// subscription carries.
	legacy: Option<(Path, String)>,
}

/// Target for a single-group one-shot SUBSCRIBE fill (`SubFillRun`).
struct FillTarget {
	request: Option<track::GroupFill>,
	info: track::Info,
	framing: lite::Framing,
	frame_end: Option<u64>,
}

impl TrackEntry {
	/// Lite-05/06: an idle copy goes live once its answer shows both where the feed starts
	/// and a group of it. START names the start but not the live edge, and a group can
	/// overtake it: on a descending track the first to land is the newest, which would read
	/// as a gap after the cache.
	fn answered(&mut self, start: Option<u64>, first: Option<Position>) {
		self.start = self.start.or(start);
		self.first = self.first.or(first);
		if let (Some(start), Some(first), TrackTarget::Copy(copy)) = (self.start, self.first, &mut self.target) {
			copy.producer.set_live_from(first, Some(start));
		}
	}

	fn mark_capped(&mut self, group: u64) {
		self.capped.insert(group);
	}

	fn is_capped(&self, group: u64) -> bool {
		self.capped.contains(&group)
	}
}

/// A subscription's accepted copy of its track.
struct Accepted {
	producer: track::Producer,
	/// How to read the track, from its TRACK_INFO.
	framing: lite::Framing,
}

/// Group streams that beat their subscription's TRACK_INFO, each parked unread after its
/// header, which flow control bounds. The accept opens their groups at once, in the order
/// they arrived, so the copy receives them as it would have without the wait, and each
/// stream then takes its own.
#[derive(Default)]
struct Parked {
	/// The headers still waiting on the accept, by ticket, in arrival order, each already
	/// holding its [`Reading`] so its group start is accounted for even if reset or evicted
	/// while parked.
	waiting: Vec<(u64, lite::Group, Reading)>,
	/// What the accept opened for each, until its stream takes it.
	opened: HashMap<u64, Result<Open, Error>>,
	next: u64,
}

/// What a group stream's header opened in its subscription's copy.
enum Open {
	Group(Opened),
	/// A one-shot SUBSCRIBE fill whose group is accepted in `GroupRecv` right before ingest,
	/// once SETUP has settled, so callers never observe a half-created group before its frames
	/// and Group State are read.
	Fill(Reading),
	Duplicate(Duplicate),
}

#[allow(clippy::large_enum_variant, reason = "transient return value matching Open")]
enum ReadyOpen {
	Group(Opened),
	Fill(Reading),
}

/// The track copy or one-shot fill owning an opened group stream.
enum OpenedTarget {
	Copy(Box<track::Producer>),
	Fill(track::FillReaders, i8),
}

/// A group stream's group, opened in its subscription's copy (or fill) for the stream to fill.
struct Opened {
	group: crate::recv::Group,
	target: OpenedTarget,
	framing: lite::Framing,
	reading: Reading,
}

/// A group stream duplicating a copy still arriving, such as another subscription's stream
/// feeding the same copy. It waits unread, which its stream credit bounds, and is discarded
/// if that copy finishes or takes the group if it fails.
struct Duplicate {
	/// The copy still arriving, whose readers wait while this holds it.
	arriving: track::Hold,
	track: track::Producer,
	/// Counted already, but parked while held: the copy it waits on arrives on another stream,
	/// which holds the end of whatever carries it, so a stall there never holds this one's.
	reading: Reading,
}

impl Drop for TrackEntry {
	fn drop(&mut self) {
		// Groups the accept opened for streams that never took them end as cancelled with
		// the subscription, rather than dropped unfinished.
		let Some(parked) = &self.parked else {
			return;
		};
		let opened = match parked.write() {
			Ok(mut state) => std::mem::take(&mut state.opened),
			Err(_) => return,
		};
		for opened in opened.into_values().flatten() {
			if let Open::Group(opened) = opened {
				let _ = opened.group.abort(Error::Cancel);
			}
		}
	}
}

/// Counts one GROUP stream parked on its subscription for as long as it waits.
struct ParkWait(Arc<atomic::AtomicUsize>);

impl ParkWait {
	fn new(waits: &Arc<atomic::AtomicUsize>) -> Self {
		waits.fetch_add(1, atomic::Ordering::Relaxed);
		Self(waits.clone())
	}
}

impl Drop for ParkWait {
	fn drop(&mut self) {
		self.0.fetch_sub(1, atomic::Ordering::Relaxed);
	}
}

impl TrackEntry {
	fn new(copy: Option<Accepted>, tail: kio::Producer<Tail>) -> Self {
		Self {
			target: match copy {
				Some(copy) => TrackTarget::Copy(copy),
				None => TrackTarget::Pending,
			},
			capped: Default::default(),
			parked: None,
			tail,
			start: None,
			first: None,
			legacy: None,
		}
	}

	/// Whether the entry already has a target (either a track copy or a single-group fill).
	fn is_ready(&self) -> bool {
		!matches!(self.target, TrackTarget::Pending)
	}

	/// Give a single-group fill target to the subscription, unparking any waiting group streams.
	fn accept_fill(&mut self, fill: FillTarget) {
		self.target = TrackTarget::Fill(fill);
		let Some(parked) = self.parked.clone() else {
			return;
		};
		let waiting = match parked.write() {
			Ok(mut state) => std::mem::take(&mut state.waiting),
			Err(_) => return,
		};
		let opened: Vec<_> = waiting
			.into_iter()
			.map(|(ticket, _, reading)| (ticket, Ok(Open::Fill(reading))))
			.collect();
		if let Ok(mut state) = parked.write() {
			state.opened.extend(opened);
		}
	}

	/// Take the fill's request if no group stream has accepted it yet.
	fn take_fill_request(&mut self) -> Option<track::GroupFill> {
		match &mut self.target {
			TrackTarget::Fill(fill) => fill.request.take(),
			_ => None,
		}
	}

	/// Whether a fill entry still holds its unaccepted request.
	fn fill_pending(&self) -> bool {
		matches!(&self.target, TrackTarget::Fill(fill) if fill.request.is_some())
	}

	#[cfg(test)]
	fn accepted(producer: track::Producer, framing: lite::Framing, tail: kio::Producer<Tail>) -> Self {
		Self::new(Some(Accepted { producer, framing }), tail)
	}

	/// Park a group stream whose `hdr` arrived before the copy, opening its [`Reading`] now so
	/// `hdr.key` is accounted for in the tail even if reset or evicted before `TRACK_INFO` lands.
	fn park(&mut self, hdr: &lite::Group, now: crate::time::Instant) -> (kio::Consumer<Parked>, u64) {
		let reading = Reading::open(&self.tail, Some(hdr.key), now);
		let parked = self.parked.get_or_insert_with(Default::default);
		let ticket = match parked.write() {
			Ok(mut state) => {
				let ticket = state.next;
				state.next += 1;
				state.waiting.push((ticket, hdr.clone(), reading));
				ticket
			}
			Err(_) => unreachable!("open while the entry holds it"),
		};
		(parked.consume(), ticket)
	}

	/// Give the copy to the subscription, opening the groups of the streams parked on it.
	fn accept(&mut self, copy: Accepted, version: Version, now: crate::time::Instant) {
		self.target = TrackTarget::Copy(copy);
		let Some(parked) = self.parked.clone() else {
			return;
		};
		let waiting = match parked.write() {
			Ok(mut state) => std::mem::take(&mut state.waiting),
			Err(_) => return,
		};
		let opened: Vec<_> = waiting
			.into_iter()
			.map(|(ticket, hdr, reading)| (ticket, self.open_with_reading(&hdr, reading, version, now)))
			.collect();
		if let Ok(mut state) = parked.write() {
			state.opened.extend(opened);
		}
	}

	/// Take what the accept opened for the parked stream `ticket`.
	fn unpark(&mut self, ticket: u64) -> Result<Open, Error> {
		let Some(parked) = &self.parked else {
			return Err(Error::Cancel);
		};
		let (opened, done) = match parked.write() {
			Ok(mut state) => {
				let opened = state.opened.remove(&ticket).unwrap_or(Err(Error::Cancel));
				(opened, state.opened.is_empty() && state.waiting.is_empty())
			}
			Err(_) => (Err(Error::Cancel), false),
		};
		if done {
			self.parked = None;
		}
		opened
	}

	/// Withdraw the parked stream `ticket`, which failed before taking its group: a group the
	/// accept already opened for it ends with `err` rather than waiting for a writer.
	fn withdraw(&mut self, ticket: u64, err: &Error) {
		let Some(parked) = &self.parked else {
			return;
		};
		let (opened, done) = match parked.write() {
			Ok(mut state) => {
				state.waiting.retain(|(waiting, _, _)| *waiting != ticket);
				let opened = state.opened.remove(&ticket);
				(opened, state.opened.is_empty() && state.waiting.is_empty())
			}
			Err(_) => return,
		};
		if let Some(Ok(Open::Group(opened))) = opened {
			let _ = opened.group.abort(err.clone());
		}
		if done {
			self.parked = None;
		}
	}

	/// Open the group of a stream whose header is `hdr` in the copy or fill.
	fn open(&mut self, hdr: &lite::Group, version: Version, now: crate::time::Instant) -> Result<Open, Error> {
		let producer = match &mut self.target {
			TrackTarget::Fill(_) => {
				let reading = Reading::open(&self.tail, Some(hdr.key), now);
				return Ok(Open::Fill(reading));
			}
			TrackTarget::Copy(Accepted { producer, .. }) => producer,
			TrackTarget::Pending => return Err(Error::Cancel),
		};
		let group_info = group::Info { start: hdr.key };
		// Stats (groups/frames/bytes) are counted in the model as the group
		// is written, through the tagged `track::Producer`. The track counts
		// it as content once its first frame lands.
		let received = producer.create_group_at(group_info, hdr.frame_start, 0..0);
		// The subscription's end waits until this stream is read. Counted
		// whatever the cache said, since the publisher counts every stream it
		// opens (a resubscribe resends a cached group), and only after the
		// cache holds the group, so a fetch held for the feed that sees it
		// accounted also finds it.
		let reading = Reading::open(&self.tail, Some(hdr.key), now);
		self.finish_open(hdr, version, received, reading, now)
	}

	/// Open the group for a parked stream whose [`Reading`] was already opened on arrival.
	fn open_with_reading(
		&mut self,
		hdr: &lite::Group,
		reading: Reading,
		version: Version,
		now: crate::time::Instant,
	) -> Result<Open, Error> {
		let producer = match &mut self.target {
			TrackTarget::Fill(_) => return Ok(Open::Fill(reading)),
			TrackTarget::Copy(Accepted { producer, .. }) => producer,
			TrackTarget::Pending => return Err(Error::Cancel),
		};
		let group_info = group::Info { start: hdr.key };
		let received = producer.create_group_at(group_info, hdr.frame_start, 0..0);
		self.finish_open(hdr, version, received, reading, now)
	}

	fn open_fill(&mut self, hdr: &lite::Group, reading: Reading) -> Result<Opened, Error> {
		let TrackTarget::Fill(fill) = &mut self.target else {
			return Err(Error::Cancel);
		};
		let Some(request) = fill.request.take() else {
			return Err(Error::Duplicate);
		};
		if hdr.key != request.key() {
			fill.request = Some(request);
			return Err(Error::Cancel);
		}
		let priority = request.priority();
		let readers = request.readers();
		let bounds = group::Bounds {
			start: hdr.frame_start,
			end: fill.frame_end,
		};
		match request.accept_from(fill.info.clone(), bounds) {
			Ok(Some(producer)) => Ok(Opened {
				group: crate::recv::Group::new(producer),
				target: OpenedTarget::Fill(readers, priority),
				framing: fill.framing,
				reading,
			}),
			Ok(None) => Err(Error::Duplicate),
			Err(err) => Err(err),
		}
	}

	fn finish_open(
		&mut self,
		hdr: &lite::Group,
		version: Version,
		received: Result<track::Received, Error>,
		reading: Reading,
		now: crate::time::Instant,
	) -> Result<Open, Error> {
		let TrackTarget::Copy(Accepted { producer, .. }) = &mut self.target else {
			return Err(Error::Cancel);
		};
		// A route's first group says where its live feed is, when its answer does
		// not: even one the cache already holds, as an idle copy asked from its
		// head gets back. The copy goes live once the cache shows that group.
		// Lite-07's answer does, and a group can overtake it: on a descending
		// track the first to land is the newest, not where the feed starts.
		if !producer.is_live() {
			let first = Position {
				group: hdr.key,
				frame: hdr.frame_start,
			};
			if !version.has_track_stream() {
				producer.set_live_from(first, None);
			} else if !version.has_largest() {
				self.first = self.first.or(Some(first));
				if let (Some(start), Some(first)) = (self.start, self.first) {
					producer.set_live_from(first, Some(start));
				}
			}
		}
		self.place(hdr, version, received, reading, now)
	}

	/// Open the group of a held stream again, once the copy it waited on failed.
	fn reopen(
		&self,
		hdr: &lite::Group,
		version: Version,
		reading: Reading,
		now: crate::time::Instant,
	) -> Result<Open, Error> {
		let TrackTarget::Copy(Accepted { producer, .. }) = &self.target else {
			return Err(Error::Cancel);
		};
		let received = producer.create_group_at(group::Info { start: hdr.key }, hdr.frame_start, 0..0);
		self.place(hdr, version, received, reading, now)
	}

	/// Give a stream what the cache answered for its header.
	fn place(
		&self,
		hdr: &lite::Group,
		version: Version,
		received: Result<track::Received, Error>,
		reading: Reading,
		now: crate::time::Instant,
	) -> Result<Open, Error> {
		let TrackTarget::Copy(Accepted { producer, framing }) = &self.target else {
			return Err(Error::Cancel);
		};
		let group = match received {
			Ok(track::Received::Group(group)) => group,
			Ok(track::Received::Held(arriving)) => {
				return Ok(Open::Duplicate(Duplicate {
					arriving,
					track: producer.clone(),
					reading,
				}));
			}
			// The group is at or past the end the publisher declared, which no
			// later stream can repair. Before lite-05 only a local finish sets
			// an end, and lite-05 specified an inclusive one, so its last
			// group lands on it: drop only that stream there.
			Err(Error::Closed)
				if !matches!(
					version,
					Version::Lite01 | Version::Lite02 | Version::Lite03 | Version::Lite04 | Version::Lite05
				) =>
			{
				tracing::warn!(group = hdr.key, "group past the declared end of track");
				let _ = producer.clone().abort(Error::ProtocolViolation);
				return Err(Error::ProtocolViolation);
			}
			// A whole copy the cache already holds refuses this one. Its span, as any copy that
			// finished it says, holds no other start, which this subscription accounts for as the
			// copy's own stream did for its subscription, as with a duplicate of a copy still
			// arriving once it settles.
			Err(err) => {
				let end = producer.consume().span_end(hdr.key.saturating_add(1));
				reading.account(hdr.key..end, now);
				return Err(err);
			}
		};
		Ok(Open::Group(Opened {
			group: crate::recv::Group::new(group),
			target: OpenedTarget::Copy(Box::new(producer.clone())),
			framing: *framing,
			reading,
		}))
	}
}

impl Subscriber {
	pub fn new(config: SubscriberConfig) -> Self {
		// Identity for incoming-hop loop detection. Derived from the local
		// origin we publish into so it matches the relay identity across
		// every session sharing that origin, required for cross-session
		// loop detection.
		let self_origin = crate::Hop::from(config.origin.node());
		Self {
			session: config.session,
			runtime: config.runtime,
			origin: config.origin,
			recv_bandwidth: config.recv_bandwidth,
			self_origin,
			session_origin: config.peer_node.unwrap_or(crate::Hop::UNKNOWN),
			subscribes: Default::default(),
			datagrams: Default::default(),
			legacy_datagrams: Default::default(),
			datagram_ends: Default::default(),
			datagrams_unrouted: Default::default(),
			ended: Default::default(),
			next_id: Default::default(),
			version: config.version,
			peer_setup: config.peer_setup,
			setup_waits: Default::default(),
			park_waits: Default::default(),
			cost: config.cost,
			sources: kio::Queue::new(),
			announce_streams: kio::Queue::new(),
			going_away: config.going_away,
			auth: config.auth,
			announces: Default::default(),
			viewers: Default::default(),
			minted: Default::default(),
		}
	}

	/// Record the error that ended the session. The first one wins: a later cancel
	/// from dropping the driver must not replace it.
	fn note_end(&self, err: &Error) {
		let mut slot = self.ended.lock();
		if slot.is_none() {
			*slot = Some(err.clone());
		}
	}

	/// The recorded session end, or cancellation when the dispatcher was dropped.
	fn end_reason(&self) -> Error {
		self.ended.lock().clone().unwrap_or(Error::Cancel)
	}

	/// What pulling content across this session's link costs, added to the route cost
	/// of every announcement received over it.
	///
	/// A locally configured price wins, since what we charge our own routing is local
	/// policy. Otherwise we charge what the peer declared, which is how a server that
	/// cannot tell a sibling from a stranger prices a link: the dialer that chose the
	/// peer declares the price for both of them. Falls back to
	/// [`super::DEFAULT_COST`] when neither priced it, and to `0` on a version that
	/// carries no cost at all, whose routes rank on hop count alone.
	///
	/// Our own price short-circuits the peer's, so a session that configured one never
	/// blocks on a SETUP to start routing.
	pub(super) fn poll_link_cost(&self, waiter: &kio::Waiter) -> Poll<u64> {
		// Older versions carry no cost on the wire, so nothing is charged and their
		// routes rank on hop count alone. Returning early also avoids blocking on a
		// SETUP that versions without a Setup Stream never send.
		if !self.version.has_route_cost() {
			return Poll::Ready(0);
		}
		match self.cost {
			Some(cost) => Poll::Ready(cost),
			None => self
				.peer_setup
				.poll_cost(waiter)
				.map(|cost| cost.unwrap_or(super::DEFAULT_COST)),
		}
	}

	/// Apply one received announce message to the origin and the per-stream
	/// bookkeeping in `run`.
	fn handle_announce(
		&mut self,
		prefix: &Path,
		announce: lite::AnnounceBroadcast,
		run: &mut PrefixRun,
	) -> Result<(), Error> {
		match announce {
			lite::AnnounceBroadcast::Active { suffix, hops, cost } => {
				let suffix = match self.version.has_announce_id() {
					// Every `active` assigns the next ordinal, even ones we drop locally.
					// Only lite-07 refuses a forwarded path, and it never reaches here.
					true => run.decoder.start(Some(suffix))?.ok_or(Error::ProtocolViolation)?,
					// Nothing references an announcement here, so there are no bases.
					false => suffix.rest,
				};
				let path = prefix.join(&suffix)?;
				if lite::update_supported(self.version)
					&& !self.version.has_announce_id()
					&& run.announced.contains(&path)
				{
					// lite-05 only: a duplicate ANNOUNCE for an already-announced path is an update;
					// atomically replace its metadata. Lite06+ updates by announce id, and older
					// versions never defined updates, so both fall through to start_announce, which
					// rejects the duplicate (Error::ProtocolViolation).
					self.update_announce(
						path,
						hops,
						cost,
						run.link_cost,
						run.responder_origin,
						&mut run.announced,
					)?;
				} else {
					// An announcement here names no instance, so the session mints one.
					self.start_announce(
						path,
						None,
						hops,
						cost,
						run.link_cost,
						run.responder_origin,
						&mut run.announced,
					)?;
				}
			}
			lite::AnnounceBroadcast::Ended { suffix, .. } => {
				let path = prefix.join(&suffix)?;
				tracing::debug!(broadcast = %self.log_path(&path), "unannounced");
				run.announced.withdraw(&path);
			}
			lite::AnnounceBroadcast::EndedId { id } => {
				// Resolve and retire the id; an unknown or already-retired id is a
				// protocol violation.
				let path = prefix.join(&run.decoder.end(id)?.ok_or(Error::ProtocolViolation)?)?;
				tracing::debug!(broadcast = %self.log_path(&path), "unannounced");
				run.announced.withdraw(&path);
			}
			lite::AnnounceBroadcast::Update { id, hops, cost, .. } => {
				// Resolve the id; it stays live (the replacement reuses it). An unknown
				// or retired id is a protocol violation.
				let suffix = run.decoder.get(id)?;
				let path = prefix.join(&suffix)?;
				self.update_announce(
					path,
					hops,
					cost,
					run.link_cost,
					run.responder_origin,
					&mut run.announced,
				)?;
			}
			lite::AnnounceBroadcast::Skipped => {}
			// Only lite-07 carries the route layer, on a stream of its own.
			lite::AnnounceBroadcast::Start { .. }
			| lite::AnnounceBroadcast::Change { .. }
			| lite::AnnounceBroadcast::End { .. }
			| lite::AnnounceBroadcast::Synced { .. }
			| lite::AnnounceBroadcast::Request { .. }
			| lite::AnnounceBroadcast::Ok
			| lite::AnnounceBroadcast::Route { .. }
			| lite::AnnounceBroadcast::RouteEnd { .. }
			| lite::AnnounceBroadcast::Dead { .. }
			| lite::AnnounceBroadcast::Drain { .. }
			| lite::AnnounceBroadcast::Floor { .. } => return Err(Error::ProtocolViolation),
		}
		Ok(())
	}

	/// Records the advertisement either way. Returns `Ok(true)` if it was accepted
	/// and attached a route, or `Ok(false)` if it was declined locally.
	#[expect(
		clippy::too_many_arguments,
		reason = "private plumbing; a single-use argument struct would only rename the parameters"
	)]
	fn start_announce(
		&mut self,
		path: Path,
		// The instance and epoch the peer announced, fixed until it restarts or retracts.
		instance: Option<(crate::Instance, u64)>,
		hops: crate::Hops,
		// The route cost off the wire, i.e. as the peer advertised it.
		// [`Cost::UNKNOWN`] before lite-06, leaving the hop chain as the only
		// routing input as before.
		cost: crate::origin::Cost,
		// This link's price, added to the wire cost.
		link_cost: u64,
		// Lite05+: the announce sender's origin id (from AnnounceOk). The sender no
		// longer stamps itself onto the chain, so we append it here to reconstruct
		// the full `[src...sender]` chain Lite04 stored. None for older versions,
		// where the sender already appended itself.
		responder_origin: Option<crate::Hop>,
		announced: &mut Announced,
	) -> Result<bool, Error> {
		// One current advertisement per prefix per stream. Test what the peer
		// advertised, not only what we accepted locally.
		if announced.contains(&path) {
			return Err(Error::ProtocolViolation);
		}

		// The peer holds this prefix now. Everything below either accepts the announcement,
		// replacing this, or declines it and leaves it exactly as reserved. Past the
		// session's cap this refuses the announce stream instead.
		announced.reserve(path.clone(), instance)?;

		let Some(hops) = self.chain(&path, hops, responder_origin) else {
			return Ok(false);
		};

		tracing::debug!(route = %self.log_path(&path), hops = hops.len(), "announce");

		// Announce this session's route into the origin: paths under the prefix
		// resolve through this session on demand. An error means the prefix is
		// outside our scope, so don't serve it. Reflections are already
		// filtered above.
		let route = self.announced_route(&path, hops, cost, link_cost, responder_origin, announced);
		Ok(announced.offer(self, path, route))
	}

	/// The route to announce for a prefix this peer advertised, charging our
	/// link's price on top of the cost it advertised.
	///
	/// Once the peer has sent a GOAWAY every route it announces starts out draining,
	/// including a restart of one already attached: a connection on its way out must
	/// not win selection, however good the path it advertises looks.
	///
	/// The instance and epoch are the ones the peer announced at `path`: an update
	/// carries neither, since it never changes the stream.
	fn announced_route(
		&self,
		path: &Path,
		hops: crate::Hops,
		cost: crate::origin::Cost,
		link_cost: u64,
		responder: Option<crate::Hop>,
		announced: &Announced,
	) -> crate::origin::RouteInfo {
		let mut route = match announced.instance(path) {
			Some((instance, epoch)) => crate::origin::RouteInfo::new().with_instance(instance, epoch),
			None => crate::origin::RouteInfo::new(),
		}
		.with_hops(hops)
		.with_cost(cost.charged(link_cost))
		.with_via(self.via(responder));

		route.draining = self.going_away.is_set();
		route
	}

	/// The announcing session's declared or assigned identity, for split-horizon.
	///
	/// A non-zero AnnounceOk origin is the declared id. Otherwise the caller-assigned
	/// identity stands in, locally: it is never written into the hop chain.
	fn via(&self, responder: Option<crate::Hop>) -> crate::Hop {
		responder
			.filter(|hop| *hop != crate::Hop::UNKNOWN)
			.unwrap_or(self.session_origin)
	}

	/// Handle an ANNOUNCE_UPDATE (an explicit update status, or a duplicate ANNOUNCE on
	/// lite-05).
	///
	/// An update carries no content claim, so this session's route re-prices in
	/// place whatever the new chain says: in-flight tracks keep flowing and the
	/// origin only hands over if the winner changed.
	/// The advertisement is already live, so this can attach a route even when the
	/// original advertisement was declined locally.
	///
	/// Returns `Ok(false)` if the new hop chain is a reflected loop (this session's
	/// route is now gone), `Ok(true)` otherwise.
	fn update_announce(
		&mut self,
		path: Path,
		hops: crate::Hops,
		// The route cost off the wire and this link's price. See `start_announce`.
		cost: crate::origin::Cost,
		link_cost: u64,
		// Lite05+: the announce sender's origin id (from AnnounceOk), appended here to
		// rebuild the full chain since the sender no longer stamps itself. None for older
		// versions. See `start_announce`.
		responder_origin: Option<crate::Hop>,
		announced: &mut Announced,
	) -> Result<bool, Error> {
		// Reflected loop (or a full chain): detach its route but keep the advertisement live.
		let Some(hops) = self.chain(&path, hops, responder_origin) else {
			announced.declined(path);
			return Ok(false);
		};

		tracing::debug!(route = %self.log_path(&path), hops = hops.len(), "update");
		let metadata = self.announced_route(&path, hops, cost, link_cost, responder_origin, announced);

		// A re-price keeps the route's prefix (and its
		// served paths) and re-prices in place. In-flight tracks keep flowing.
		if let Some(entry) = announced.attached(&path) {
			entry.update(metadata);
			return Ok(true);
		}

		Ok(announced.offer(self, path, metadata))
	}

	/// Whether `hops` passed through this node: the claim came back, and filed again it
	/// would keep itself alive once its source left.
	pub(super) fn names_self(&self, hops: &crate::Hops) -> bool {
		hops.contains(&self.self_origin)
	}

	/// The full chain of a new or replacing advertisement, or `None` when it is
	/// reflected or exceeds the origin's hop limit.
	fn chain(&self, path: &Path, mut hops: crate::Hops, responder_origin: Option<crate::Hop>) -> Option<crate::Hops> {
		let reflected = match responder_origin {
			// A chain already naming the sender came back through it. Zero names
			// nobody and may repeat; every identified sender must be appended once.
			Some(responder) => {
				(responder != crate::Hop::UNKNOWN && hops.contains(&responder))
					|| hops.push(responder).is_err()
					|| self.names_self(&hops)
			}
			None => self.names_self(&hops),
		};
		if reflected {
			tracing::debug!(route = %self.log_path(path), "dropping reflected announce");
			return None;
		}

		// Lite03 carries anonymous placeholders, and Lite01/02 carry no path.
		// Keep an anonymous mark so downstream ranks unidentified paths last.
		if hops.is_empty() {
			hops.push(crate::Hop::UNKNOWN)
				.expect("an empty hop chain always has room for one entry, and repeats nothing");
		}
		if !matches!(self.version, Version::Lite01 | Version::Lite02) && hops.len() > self.origin.max_hops().get() {
			tracing::debug!(route = %self.log_path(path), hops = hops.len(), "dropping announce beyond hop limit");
			return None;
		}
		Some(hops)
	}

	/// Remove a subscription, releasing the session's handle on its producer.
	fn remove_subscribe(&self, id: u64) {
		self.subscribes.lock().remove(&id);
	}

	/// Remove a subscription its publisher finished. On lite-05/06 it keeps routing its
	/// datagrams to its name's datagram track, which may register meanwhile, until the ends
	/// apply, which the returned consumer's close reports.
	fn finish_subscribe(&self, id: u64) -> Option<kio::Consumer<()>> {
		let removed = self.subscribes.lock().remove(&id);
		let legacy = removed.and_then(|mut entry| entry.legacy.take())?;
		let mut ends = self.datagram_ends.lock();
		ends.subscribes.push((id, legacy));
		Some(ends.applied.consume())
	}

	/// Apply the datagram ends the sources read, once the driver routed the datagrams received
	/// with them: it read the transport's queue empty. A publisher sends a track's last datagram
	/// right before its end, and a fresh track's answer, first datagram and end can all share a
	/// receive pass. A peer keeping the queue full delays its tracks' ends, holding nothing more.
	fn end_datagrams(&self) {
		let ends = std::mem::take(&mut *self.datagram_ends.lock());
		for (id, producer) in ends.tracks {
			self.datagrams.lock().remove(&id);
			producer.finish();
		}
		for (key, producer) in ends.shared {
			let mut shared = self.legacy_datagrams.lock();
			if shared.get(&key).is_some_and(|entry| entry.producer.is_clone(&producer)) {
				shared.remove(&key);
			}
			drop(shared);
			producer.finish();
		}
	}

	/// Decode one datagram body and hand it to the datagram track it was sent for, or return
	/// the Subscribe ID it names when that subscription has no datagram track: one unknown,
	/// ended, or not yet answered.
	///
	/// A datagram track's subscription takes its datagrams. On lite-05/06 a stream track's
	/// subscription carries its name's datagrams too, which go to that name's datagram track.
	/// A body past the wire cap names no subscription.
	fn route_datagram(&self, payload: bytes::Bytes) -> Result<Option<u64>, Error> {
		if payload.len() > lite::Datagram::MAX_SIZE {
			return Ok(None);
		}
		let dg = lite::Datagram::decode(payload, self.version)?;
		let Some((mut producer, timescale)) = self.datagram_target(dg.subscribe) else {
			return Ok(Some(dg.subscribe));
		};
		// A scale-less datagram's timestamp is bare, which the track checks is its sequence.
		let timestamp = match timescale {
			Some(scale) => {
				Timestamp::new(dg.timestamp, scale).map_err(|_| Error::BoundsExceeded(crate::coding::BoundsExceeded))?
			}
			None => Timestamp::unscaled(dg.timestamp),
		};
		producer.insert(crate::datagram::Datagram {
			sequence: dg.sequence,
			timestamp,
			payload: dg.payload,
		})?;
		Ok(None)
	}

	/// The datagram track Subscribe ID `id` delivers to, and its datagrams' timescale.
	fn datagram_target(&self, id: u64) -> Option<(datagram::Producer, Option<Timescale>)> {
		if let Some(entry) = self.datagrams.lock().get(&id) {
			return Some((entry.producer.clone(), entry.timescale));
		}
		let key = match self.subscribes.lock().get(&id) {
			Some(entry) => entry.legacy.clone()?,
			None => {
				let ends = self.datagram_ends.lock();
				ends.subscribes.iter().find(|(ended, _)| *ended == id)?.1.clone()
			}
		};
		let legacy = self.legacy_datagrams.lock();
		let entry = legacy.get(&key)?;
		Some((entry.producer.clone(), entry.timescale))
	}

	fn log_path<'a>(&'a self, path: &'a Path) -> crate::path::Rooted<'a> {
		crate::path::Rooted(self.origin.root(), path)
	}
}

/// The subscriber half's driver: the announce prefixes, the uni-stream accept
/// loop, PROBE feedback, datagrams, and the per-source serve machines. Only an
/// error ends it.
// Owns the active subscriptions for exactly as long as the driver. A dropped
// driver is how a cancelled session unwinds, so cleanup cannot depend on any
// poll returning Ready.
struct SubscriptionCleanup(Lock<HashMap<u64, TrackEntry>>, Lock<HashMap<u64, DatagramEntry>>);

impl SubscriptionCleanup {
	/// End every active subscription with the session's error. Group machines own
	/// their cleanup independently; this records the track's terminal state.
	///
	/// Even a subscription the publisher already ended aborts: finishing it before its tail
	/// settled would report what arrived as the whole track. Once the declared end settled,
	/// [`track::Producer::abort`] ends its readers cleanly instead. Before lite-05 the FIN
	/// is that end, declared here at the live edge since nothing more can arrive.
	fn abort(&self, err: &Error) {
		for (_, entry) in self.1.lock().drain() {
			entry.producer.abort(err.clone());
		}
		for (_, mut entry) in self.0.lock().drain() {
			// A track still waiting on TRACK_INFO has no copy yet: its request rejects with
			// the session's end instead.
			let TrackTarget::Copy(Accepted { producer, .. }) =
				std::mem::replace(&mut entry.target, TrackTarget::Pending)
			else {
				continue;
			};
			if entry.tail.read().is_ended() {
				let _ = producer.finish_shared();
			}
			let _ = producer.abort(err.clone());
		}
	}
}

impl Drop for SubscriptionCleanup {
	fn drop(&mut self) {
		// A session that ended with an error already aborted these with it; what
		// remains was cancelled with the driver.
		self.abort(&Error::Cancel);
	}
}

pub(super) struct SubscriberDriver {
	subscriber: Subscriber,
	/// Aborts whatever is still subscribed when the session ends.
	cleanup: SubscriptionCleanup,
	/// Which prefixes to ask the peer about.
	discovery: crate::discovery::Discovery,
	/// One machine per prefix asked about, each woken on its own so a wake polls
	/// only the prefixes it concerns. Only an error ends the session; a prefix
	/// finishing cleanly (publisher FIN) just retires.
	announces: kio::Tasks<AnnouncePrefix>,
	/// Each open prefix's machine in `announces`.
	prefixes: HashMap<Path, kio::TaskKey>,
	/// Lite-07: the route layer, which asks the peer for what discovery wants on the
	/// session's one Announce Stream in place of `announces`.
	routes: Option<route_stream::RouteStream>,
	uni: UniAccept,
	/// PROBE feedback; finishes quietly when unsupported or given up on.
	bandwidth: Option<RecvBandwidth>,
	/// Datagram receive; inert on a version or transport without datagrams.
	datagrams: Option<DatagramRecv>,
	/// One machine per minted source, serving the origin's track requests.
	sources: kio::Tasks<SourceServe>,
}

impl SubscriberDriver {
	pub fn new(
		subscriber: Subscriber,
		early: Vec<Reader<crate::transport::boxed::RecvStream, Version>>,
		client: bool,
		publish: route_stream::Publish,
	) -> Self {
		// Lite03/04 mark no initial set, so a watch would read as synced before the peer
		// answers it. Asking for everything at connect keeps their discovery eager instead.
		let lazy = client && !matches!(subscriber.version, Version::Lite03 | Version::Lite04);
		Self {
			discovery: crate::discovery::Discovery::new(&subscriber.origin, lazy, subscriber.runtime.clone()),
			announces: kio::Tasks::new(),
			prefixes: HashMap::new(),
			routes: (subscriber.version.has_routes())
				.then(|| route_stream::RouteStream::new(subscriber.clone(), publish)),
			cleanup: SubscriptionCleanup(subscriber.subscribes.clone(), subscriber.datagrams.clone()),
			uni: UniAccept::new(subscriber.clone(), early),
			bandwidth: Some(RecvBandwidth::new(subscriber.clone())),
			datagrams: Some(DatagramRecv::new(subscriber.clone())),
			sources: kio::Tasks::new(),
			subscriber,
		}
	}

	/// End every active subscription with the error that ended the session.
	pub fn abort(&self, err: &Error) {
		// Before the subscribe map, so a TRACK_INFO request dropped with this driver
		// rejects with `err` rather than `Dropped`.
		self.subscriber.note_end(err);
		// Before the copies they serve, so each front reads what its copy fails with as its
		// source going, not as the track's verdict.
		for source in self.sources.iter() {
			source.source.close();
		}
		self.cleanup.abort(err);
	}

	/// Read what the closed transport already delivered: the buffered uni streams, then
	/// the subscribe responses. Serve machines wait for the group streams, since ending a
	/// subscription removes the state their tails are delivered into.
	pub fn poll_drain(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		let closed = self.uni.poll(waiter).is_ready();
		let _ = self.uni.children.poll(waiter);
		// Streams parked on a TRACK_INFO wait for their source to read it, which accepts them
		// or ends their subscription. Either wakes this.
		let parked = self.subscriber.park_waits.load(atomic::Ordering::Relaxed);
		if closed && parked > 0 && self.uni.children.len() == parked {
			let _ = self.sources.poll(waiter);
			self.subscriber.end_datagrams();
			return Poll::Pending;
		}
		// Once every remaining stream waits on a SETUP that never arrived, none of
		// them can deliver it: the peer can open no more.
		let stuck = self.subscriber.peer_setup.ietf().is_none()
			&& self.uni.children.len() == self.subscriber.setup_waits.load(atomic::Ordering::Relaxed);
		if !closed || !(self.uni.children.is_empty() || stuck) {
			return Poll::Pending;
		}
		// The peer's close can win the race against a SUBSCRIBE FIN it sent first, and on
		// Lite03/04 that FIN is the only end: read it, so the track finishes rather than
		// aborts. Each source reads its streams once, then retires on the closed session.
		let _ = self.sources.poll(waiter);
		self.subscriber.end_datagrams();
		Poll::Ready(())
	}

	pub fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		loop {
			while let Poll::Ready(change) = self.discovery.poll(waiter) {
				match (change, &mut self.routes) {
					(crate::discovery::Change::Open(prefix, snapshot), Some(routes)) => routes.want(prefix, snapshot),
					(crate::discovery::Change::Close(prefix), Some(routes)) => routes.unwant(&prefix),
					(crate::discovery::Change::Open(prefix, snapshot), None) => {
						let machine = AnnouncePrefix::new(self.subscriber.clone(), prefix.clone(), snapshot);
						self.prefixes.insert(prefix, self.announces.insert(machine));
					}
					// Dropping the machine resets its stream, which the peer reads as the
					// request ending, and retracts every route it reported.
					(crate::discovery::Change::Close(prefix), None) => {
						if let Some(key) = self.prefixes.remove(&prefix) {
							self.announces.remove(key);
						}
					}
				}
			}
			if let Some(routes) = &mut self.routes
				&& let Poll::Ready(err) = routes.poll(waiter)
			{
				return Poll::Ready(Err(err));
			}
			let mut ended = Vec::new();
			let mut failed = None;
			let _ = self.announces.poll_each(waiter, |machine, res| match res {
				Ok(()) => ended.push(machine.prefix),
				Err(err) => {
					failed.get_or_insert(err);
				}
			});
			if let Some(err) = failed {
				return Poll::Ready(Err(err));
			}
			if ended.is_empty() {
				break;
			}
			for prefix in ended {
				self.prefixes.remove(&prefix);
				self.discovery.ended(&prefix);
			}
		}
		if let Poll::Ready(res) = self.uni.poll(waiter) {
			return Poll::Ready(res);
		}
		if let Some(bandwidth) = &mut self.bandwidth {
			match bandwidth.poll(waiter) {
				Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
				Poll::Ready(Ok(())) => self.bandwidth = None,
				Poll::Pending => {}
			}
		}

		// Datagrams go around the sources, which decode the answers that register their
		// routes and the ends that remove them. A publisher sends a track's first datagram
		// right behind its answer and its last right before its end, often in the same
		// packet. So the datagrams of known tracks go first, those with no known track wait
		// for the sources' answers, and the ends the sources read apply after both, once the
		// datagrams queued with them are read.
		if let Poll::Ready(Err(err)) = self.poll_datagrams(waiter, true) {
			return Poll::Ready(Err(err));
		}

		// Sources minted by the announce half; their completion never ends the
		// session (the origin delivers the unannounce itself).
		while let Poll::Ready(Ok(minted)) = self.subscriber.sources.poll_pop(waiter) {
			self.sources.push(SourceServe::new(self.subscriber.clone(), minted));
		}
		let _ = self.sources.poll(waiter);

		let routed = self.poll_datagrams(waiter, false);
		// A pass the budget cut short leaves datagrams queued, which may have come with an end:
		// its ends wait for a later poll's pass to read the queue empty.
		if self.datagrams.as_ref().is_none_or(|recv| recv.caught_up) {
			self.subscriber.end_datagrams();
		}
		if let Poll::Ready(Err(err)) = routed {
			return Poll::Ready(Err(err));
		}

		Poll::Pending
	}

	/// Route the ready datagrams, see [`DatagramRecv::poll`]; ready only with the error that
	/// ends the session.
	fn poll_datagrams(&mut self, waiter: &kio::Waiter, hold: bool) -> Poll<Result<(), Error>> {
		if let Some(datagrams) = &mut self.datagrams {
			match datagrams.poll(waiter, hold) {
				Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
				Poll::Ready(Ok(())) => self.datagrams = None,
				Poll::Pending => {}
			}
		}
		Poll::Pending
	}
}

impl Drop for SubscriberDriver {
	fn drop(&mut self) {
		// `abort` already recorded a session error when the driver failed. A driver
		// dropped without that still has to name an end before its setup machines drop.
		self.subscriber.note_end(&Error::Cancel);
		// The route stream disconnects before `cleanup` aborts what is still subscribed, so
		// the routes through this session have left the route layer by the time a reader
		// sees its copy fail, and a front fails over rather than ending.
		self.routes = None;
	}
}

/// Accepts incoming uni streams (GROUP data plus the peer's SETUP) and drives
/// each as a child machine. Resolves only on a transport error.
struct UniAccept {
	subscriber: Subscriber,
	// A dedicated accept handle: the poll interface takes `&mut self`.
	accept: Transport,
	children: kio::Tasks<UniServe>,
}

impl UniAccept {
	fn new(subscriber: Subscriber, early: Vec<Reader<crate::transport::boxed::RecvStream, Version>>) -> Self {
		let accept = subscriber.session.clone();
		let mut children = kio::Tasks::new();
		for reader in early {
			children.push(UniServe {
				subscriber: subscriber.clone(),
				state: UniState::Start {
					reader: reader.with_stats(subscriber.origin.stats()),
				},
			});
		}
		Self {
			subscriber,
			accept,
			children,
		}
	}

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		let _ = self.children.poll(waiter);

		// MAX_STREAMS credit alone lets one poll accept thousands of streams, so a peer
		// opening them at once must not hold the session's other tasks. A yield has woken
		// the task, and this always ends Pending, so the children below still run.
		let mut budget = kio::coop::Budget::new(32);
		while budget.poll_yield(waiter).is_ready() {
			match self.accept.poll_accept_uni(waiter) {
				Poll::Ready(Ok(stream)) => {
					self.children.push(UniServe {
						subscriber: self.subscriber.clone(),
						state: UniState::Start {
							reader: Reader::new(stream, self.subscriber.version)
								.with_stats(self.subscriber.origin.stats()),
						},
					});
					// Start each child before accepting the next. The set polls by slot, from a
					// rotating start, and reuses freed slots, so a burst started together would
					// read its headers in no particular order: a group stream whose bytes are
					// here opens its group in the order the streams arrived.
					let _ = self.children.poll(waiter);
				}
				Poll::Ready(Err(err)) => return Poll::Ready(Err(Error::from_transport(err))),
				Poll::Pending => break,
			}
		}
		Poll::Pending
	}
}

/// One accepted uni stream, dispatched on its first varint.
struct UniServe {
	subscriber: Subscriber,
	state: UniState,
}

#[expect(
	clippy::large_enum_variant,
	reason = "a state machine's enum is its storage: one transient instance per stream, so the big variant is the working state, not padding held in bulk"
)]
enum UniState {
	/// Reading the stream's type.
	Start {
		reader: Reader<crate::transport::boxed::RecvStream, Version>,
	},
	/// Reading the peer's single SETUP message, recorded so capability-gated
	/// streams (PROBE) can consult it. lite-05+ only.
	Setup {
		reader: Reader<crate::transport::boxed::RecvStream, Version>,
	},
	Group(GroupRecv),
	Done,
}

impl kio::Task for UniServe {
	type Output = ();

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		if let Err(err) = ready!(self.poll_serve(waiter)) {
			tracing::debug!(%err, "error running uni stream");
			if matches!(err, Error::ProtocolViolation) {
				self.subscriber
					.session
					.clone()
					.close(crate::SessionError::ProtocolViolation.to_code(), &err.to_string());
			}
		}
		Poll::Ready(())
	}
}

impl UniServe {
	/// Abort the stream with the given error, wherever the reader currently lives.
	fn abort(&mut self, err: &Error) {
		match &mut self.state {
			UniState::Start { reader } | UniState::Setup { reader } => reader.abort(err),
			UniState::Group(recv) => recv.reader.abort(err),
			UniState::Done => {}
		}
	}

	fn poll_serve(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		loop {
			match &mut self.state {
				UniState::Start { reader } => {
					// An unknown type is stopped, never the session's failure, so a later
					// version can add one.
					let kind = match ready!(reader.poll_decode::<lite::DataType>(waiter)) {
						Ok(kind) => kind,
						Err(err) => {
							tracing::debug!(%err, "unknown uni stream");
							reader.abort(&Error::UnexpectedStream);
							return Poll::Ready(Ok(()));
						}
					};
					// Claim before decoding the body, so two incomplete SETUPs are duplicates too.
					if matches!(kind, lite::DataType::Setup) && self.subscriber.version.has_setup_stream() {
						self.subscriber.peer_setup.claim()?;
					}
					let UniState::Start { reader } = std::mem::replace(&mut self.state, UniState::Done) else {
						unreachable!()
					};
					self.state = match kind {
						lite::DataType::Group => UniState::Group(GroupRecv::new(self.subscriber.clone(), reader)),
						lite::DataType::Setup => {
							let mut reader = reader;
							reader.set_priority(crate::priority::RECV_CONTROL);
							UniState::Setup { reader }
						}
						lite::DataType::Announce => {
							let mut reader = reader;
							if !self.subscriber.version.has_routes() {
								reader.abort(&Error::UnexpectedStream);
								return Poll::Ready(Ok(()));
							}
							// The route layer reads it, and closes the session on a second.
							reader.set_priority(crate::priority::RECV_CONTROL);
							let _ = self.subscriber.announce_streams.try_push(reader);
							return Poll::Ready(Ok(()));
						}
					};
				}
				UniState::Setup { reader } => {
					if !self.subscriber.version.has_setup_stream() {
						let err = Error::UnexpectedStream;
						self.abort(&err);
						return Poll::Ready(Ok(()));
					}
					let res = ready!(reader.poll_decode::<lite::setup::ReceivedSetup>(waiter));
					match res {
						Ok(setup) => {
							tracing::debug!(?setup, "received peer setup");
							self.subscriber.peer_setup.receive(setup)?;
							return Poll::Ready(Ok(()));
						}
						// The slot is claimed, so no other SETUP can arrive and the streams
						// waiting on it would hang. The session cannot continue.
						Err(err) => {
							tracing::debug!(%err, "failed to read peer setup");
							self.abort(&err);
							return Poll::Ready(Err(Error::ProtocolViolation));
						}
					}
				}

				UniState::Group(recv) => {
					let res = ready!(recv.poll_serve(waiter));
					if let Err(err) = res {
						self.abort(&err);
					}
					return Poll::Ready(Ok(()));
				}
				UniState::Done => return Poll::Ready(Ok(())),
			}
		}
	}
}

/// Receives one GROUP stream into its subscription's track producer.
struct GroupRecv {
	subscriber: Subscriber,
	reader: Reader<crate::transport::boxed::RecvStream, Version>,
	state: GroupRecvState,
	/// Held while parked on the peer's SETUP, so a drain can tell it never arrives.
	setup_wait: Option<SetupWait>,
	/// Held while parked on the subscription's TRACK_INFO, so a drain reads it first.
	park_wait: Option<ParkWait>,
}

/// Counts one GROUP stream parked on the peer's SETUP for as long as it lives.
struct SetupWait(Arc<atomic::AtomicUsize>);

impl SetupWait {
	fn new(waits: &Arc<atomic::AtomicUsize>) -> Self {
		waits.fetch_add(1, atomic::Ordering::Relaxed);
		Self(waits.clone())
	}
}

impl Drop for SetupWait {
	fn drop(&mut self) {
		self.0.fetch_sub(1, atomic::Ordering::Relaxed);
	}
}

#[kio::machine(self.state: GroupRecvState)]
impl GroupRecv {
	/// The stream keeps its [unidentified](crate::priority::RECV_UNIDENTIFIED) rank until its
	/// header is read, and ranks [parked](crate::priority::RECV_PARKED) only while it waits
	/// unread, so a full window stops what holds the most first.
	fn new(subscriber: Subscriber, reader: Reader<crate::transport::boxed::RecvStream, Version>) -> Self {
		Self {
			subscriber,
			reader,
			state: GroupRecvState::Start,
			setup_wait: None,
			park_wait: None,
		}
	}

	fn poll_serve(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		// Decode GROUP before waiting on SETUP or TRACK_INFO: `lite::Group` does not depend on
		// SETUP, and reading it first lets us account for `hdr.key` in the subscription's tail
		// before parking, so a reset or eviction while parked never drops the group unaccounted.
		let hdr: lite::Group = wait!(Header => self.reader.poll_decode::<lite::Group>(waiter))?;
		// A SUBSCRIBE sent alongside its TRACK may be served before the TRACK_INFO that says
		// how to read it: the stream parks, unread, until the accept opens its group.
		let parked: Option<(kio::Consumer<Parked>, u64)> = {
			let mut subs = self.subscriber.subscribes.lock();
			match subs.get_mut(&hdr.subscribe) {
				Some(entry) if !entry.is_ready() => {
					self.park_wait = Some(ParkWait::new(&self.subscriber.park_waits));
					self.reader.set_priority(crate::priority::RECV_PARKED);
					Some(entry.park(&hdr, self.subscriber.runtime.now()))
				}
				_ => None,
			}
		};
		// Closed once the subscription went, which the lookup below reports.
		let accepted: Result<(), Error> = wait!(Accept => match parked {
			Some((parked, ticket)) => match parked.poll(waiter, |parked| match parked.opened.contains_key(ticket) {
				true => Poll::Ready(()),
				false => Poll::Pending,
			}) {
				Poll::Ready(_) => Poll::Ready(Ok(())),
				// Unread meanwhile, so a reset or an eviction would otherwise go unseen, and an
				// evicted stream holds further eviction off until it is stopped.
				Poll::Pending => self.reader.poll_reset(waiter).map(Err),
			},
			None => Poll::Ready(Ok(())),
		});
		self.park_wait = None;
		if let (Err(err), Some((_, ticket))) = (&accepted, &parked)
			&& let Some(entry) = self.subscriber.subscribes.lock().get_mut(&hdr.subscribe)
		{
			entry.withdraw(*ticket, err);
		}
		accepted?;

		let open = {
			let mut subs = self.subscriber.subscribes.lock();
			let entry = subs.get_mut(&hdr.subscribe).ok_or(Error::Cancel)?;
			match &parked {
				Some((_, ticket)) => entry.unpark(*ticket),
				None => entry.open(&hdr, self.subscriber.version, self.subscriber.runtime.now()),
			}
		};
		let mut open: Option<Open> = Some(open?);
		if matches!(open, Some(Open::Duplicate(_))) {
			self.reader.set_priority(crate::priority::RECV_PARKED);
		}
		// A duplicate of a copy still arriving waits, unread, for that copy to settle.
		let ready_open: ReadyOpen = wait!(Hold => loop {
			let mut duplicate = match open.take() {
				Some(Open::Group(opened)) => break Poll::Ready(Ok(ReadyOpen::Group(opened))),
				Some(Open::Fill(reading)) => break Poll::Ready(Ok(ReadyOpen::Fill(reading))),
				Some(Open::Duplicate(duplicate)) => duplicate,
				None => break Poll::Ready(Err(Error::Cancel)),
			};
			duplicate.reading.park();
			if let Poll::Ready(err) = self.reader.poll_reset(waiter) {
				break Poll::Ready(Err(err));
			}
			if let Poll::Ready(err) = duplicate.track.poll_aborted(waiter) {
				break Poll::Ready(Err(err));
			}
			match duplicate.arriving.arriving().poll_closed(waiter) {
				Poll::Pending => {
					*open = Some(Open::Duplicate(duplicate));
					break Poll::Pending;
				}
				// The copy holds every frame, so this one only duplicates it. A whole copy's span
				// holds no other start, which this subscription accounts for as the copy's own
				// stream did for its subscription.
				Poll::Ready(Ok(())) => {
					if let Some(end) = duplicate.arriving.arriving().end() {
						duplicate
							.reading
							.account(hdr.key..end, self.subscriber.runtime.now());
					}
					break Poll::Ready(Err(Error::Duplicate));
				}
				Poll::Ready(Err(_)) => {
					let subs = self.subscriber.subscribes.lock();
					let Some(entry) = subs.get(&hdr.subscribe) else {
						break Poll::Ready(Err(Error::Cancel));
					};
					let Duplicate {
						arriving, mut reading, ..
					} = duplicate;
					// Taking the group, this stream alone holds the end open for it.
					reading.resume();
					let reopened = entry.reopen(hdr, self.subscriber.version, reading, self.subscriber.runtime.now());
					// Released once the group is opened again, so the failed copy's readers
					// waiting on it find this copy.
					drop(arriving);
					match reopened {
						Ok(reopened) => *open = Some(reopened),
						Err(err) => break Poll::Ready(Err(err)),
					}
				}
			}
		})?;
		// Wait for the peer's SETUP only once `hdr.key` is accounted for, to know whether the
		// `ietf` extension's object records precede each frame.
		let ietf: bool = wait!(Setup => {
			match self.subscriber.peer_setup.poll_ietf(waiter) {
				Poll::Ready(ietf) => {
					self.setup_wait = None;
					Poll::Ready(Ok(ietf))
				}
				Poll::Pending => {
					if self.setup_wait.is_none() {
						self.setup_wait = Some(SetupWait::new(&self.subscriber.setup_waits));
						self.reader.set_priority(crate::priority::RECV_PARKED);
					}
					// Unread meanwhile, so a reset or an eviction would otherwise go unseen.
					self.reader.poll_reset(waiter).map(Err)
				}
			}
		})?;
		let opened = match ready_open {
			ReadyOpen::Group(opened) => opened,
			ReadyOpen::Fill(reading) => {
				let mut subs = self.subscriber.subscribes.lock();
				let entry = subs.get_mut(&hdr.subscribe).ok_or(Error::Cancel)?;
				entry.open_fill(&hdr, reading)?
			}
		};
		let Opened {
			group,
			target,
			framing,
			reading,
		} = opened;
		let records: bool = ietf && framing.ietf.is_some();
		let (priority, order) = match &target {
			OpenedTarget::Copy(track) => track
				.subscription()
				.map_or((0, Default::default()), |sub| (sub.priority, sub.order)),
			OpenedTarget::Fill(_, priority) => (*priority, Default::default()),
		};
		self.reader
			.set_priority(crate::priority::recv_group(priority, hdr.key, order));

		// The timescale came from TRACK_INFO, so frames decode immediately. No
		// SUBSCRIBE_OK to wait on.
		// Guarded: dropping this machine mid-group is a cancellation, not a clean end.
		let group: crate::recv::Group = group;
		let target: OpenedTarget = target;
		let clock = lite::Clock::new(self.subscriber.version, hdr.key);
		let mut ingest: FrameIngest = FrameIngest::new(framing, records, hdr.frame_start, clock);
		let reading: Reading = reading;

		// Filling the group, bailing if the track or group dies first. The track or group
		// dying cancels the stream; the peer's own close arrives through the ingest's reads.
		let res = wait!(Serve => {
			if let OpenedTarget::Copy(track) = &target
				&& let Poll::Ready(err) = track.poll_aborted(waiter)
			{
				Poll::Ready(Err(err))
			} else if let Poll::Ready(err) = group.handle().poll_aborted(waiter) {
				// If the entire stream up to FIN already arrived in the transport buffer, the
				// publisher may already have received the FIN ACK and retired the group stream,
				// so a STOP_SENDING would not trigger a re-sent skip. Drain locally to extract
				// GROUP_END without blocking or allocating; if more bytes are still on the wire,
				// STOP_SENDING reaches the live publisher stream and it re-sends the cut span.
				if self.subscriber.version.has_group_span()
					&& let Some(end) = ingest.try_drain_end(&mut self.reader, group.compressed())
					&& group.check_end(end).is_ok()
				{
					reading.account(hdr.key..end, self.subscriber.runtime.now());
					if let OpenedTarget::Copy(track) = &target {
						track.skip_groups(hdr.key.saturating_add(1)..end);
					}
				}
				Poll::Ready(Err(err))
			} else {
				match ingest.poll(&mut self.reader, group, waiter) {
					Poll::Ready(res) => Poll::Ready(res),
					Poll::Pending => {
						if let OpenedTarget::Fill(readers, _) = &target
							&& readers.poll_unwanted(waiter).is_ready()
							&& group.handle().poll_unwanted(waiter).is_ready()
							&& group.handle().abort_unwanted(Error::Cancel)
						{
							Poll::Ready(Err(Error::Cancel))
						} else {
							Poll::Pending
						}
					}
				}
			}
		});

		let res = match res {
			// Whole at a GROUP_END's end, else closed: every frame sent arrived. A whole group's
			// span holds no other start, so its stream accounts for those starts too, and the
			// track records them skipped, as its own publisher's append did.
			Ok(end) => {
				// Only a GROUP_END the group accepts accounts for its span: one below a frame it
				// holds is malformed, and names nothing. A bare FIN on a group that was capped
				// mid-group by a Position End marks the copy back-cut so switch-back fetches the
				// remaining frames rather than reusing the truncated slice.
				let capped = end.is_none()
					&& !ingest.saw_end_object()
					&& self.subscriber.version.has_group_end()
					&& self
						.subscriber
						.subscribes
						.lock()
						.get(&hdr.subscribe)
						.is_some_and(|entry| entry.is_capped(hdr.key));
				let ended = if capped { group.end_slice() } else { group.end(end) };
				if ended.is_ok()
					&& let Some(end) = end
				{
					reading.account(hdr.key..end, self.subscriber.runtime.now());
					if let OpenedTarget::Copy(track) = &target {
						track.skip_groups(hdr.key.saturating_add(1)..end);
					}
				}
				ended
			}
			Err(err @ (Error::Cancel | Error::Stream(crate::StreamError::Cancel))) => {
				let _ = group.abort(err);
				Ok(())
			}
			Err(err) => {
				tracing::debug!(%err, group = %group.key, "group error");
				let _ = group.abort(err.clone());
				Err(err)
			}
		};
		// Held until the group settles above, so a frame the track or group cut short
		// drops into an already-aborted group instead of reporting a loss.
		drop((ingest, reading));
		Poll::Ready(res)
	}
}

/// Pumps bare FRAME messages from a reader into a group producer: the wire
/// format shared by GROUP streams and FETCH responses.
pub(crate) struct FrameIngest {
	/// Whether frames carry the zigzag-delta timestamp prefix, in the track's timescale:
	/// lite-05+, except a scale-less lite-07 track, whose frames each take their group's start.
	framing: lite::Framing,
	/// The times the stream carried, which each FRAME's delta and GROUP_END count from.
	clock: lite::Clock,
	phase: IngestPhase,
	/// Where the stream's frames start, the first Object ID until a record names it.
	frame_start: u64,
	/// `Some` once the Group State of the `ietf` extension arrived: every frame follows an
	/// object record.
	records: Option<RecordIngest>,
}

/// A group's object records on ingest, under the `ietf` extension.
struct RecordIngest {
	/// The Group State carried relay state. Otherwise every record is plain: its Object ID
	/// is the frame's index, with no properties and no END_OF_GROUP.
	relayed: bool,
	/// The Group State set EXTENSIONS, without which no record has properties.
	extensions: bool,
	/// The previous record's Object ID, `None` before the first.
	prior: Option<u64>,
	/// The record of the frame being read: its Object ID and properties block.
	///
	/// It is checked when the frame is created, which charges it to the cache, so a stalled
	/// stream holds only the bytes it read.
	pending: Option<(u64, bytes::Bytes)>,
}

enum IngestPhase {
	/// Reading the `ietf` extension's Group State.
	State,
	/// Reading the next object record. Stream end here means the group has no more objects.
	Object,
	/// Reading the record's properties block, whose size comes first.
	Properties { id: u64, end: bool, size: Option<usize> },
	/// The END_OF_GROUP record arrived, so only the stream end may follow.
	Ended,
	/// Lite-07: reading the `Payload Length`, the payload size plus one, which leads the
	/// frame, or 0 for GROUP_END. Stream end here means the group closed incomplete.
	Length,
	/// Lite-07: reading GROUP_END's `End`, the group's exclusive end as a delta from the
	/// stream's highest timestamp.
	GroupEnd,
	/// Lite-07: GROUP_END said where the group ends, so only the stream end may follow.
	Finished { end: u64 },
	/// Reading the timestamp delta (skipped when frames carry none), after the size on lite-07.
	/// Stream end here before lite-07 means the group has no more frames.
	Timing { size: Option<u64> },
	/// Reading a compressed Track's inflated size (lite-07), or skipped. Stream end here
	/// also ends the group on an untimed Track before lite-07, where nothing precedes it.
	Inflated {
		size: Option<u64>,
		timestamp: Option<Timestamp>,
	},
	/// Before lite-07: reading the frame size, which ends the header. Stream end here also
	/// ends the group (pre-lite-05, where there is no timing prefix to act as the sentinel).
	Size {
		timestamp: Option<Timestamp>,
		inflated: Option<u64>,
	},
	/// Waiting to admit the frame until the group's fastest reader leaves room for it.
	Admit { header: frame::Header },
	/// Streaming the frame payload.
	Payload { frame: frame::Producer },
}

impl FrameIngest {
	/// `records` when the session negotiated the `ietf` extension and a moq-transport
	/// publisher made the track, so the stream starts with a Group State. `clock` counts the
	/// stream's times from its group's start.
	pub(crate) fn new(framing: lite::Framing, records: bool, frame_start: u64, clock: lite::Clock) -> Self {
		Self {
			framing,
			clock,
			phase: match records {
				true => IngestPhase::State,
				false => Self::first_field(framing),
			},
			frame_start,
			records: None,
		}
	}

	/// The phase that starts the next frame.
	fn next_frame(&self) -> IngestPhase {
		match self.records {
			Some(_) => IngestPhase::Object,
			None => Self::first_field(self.framing),
		}
	}

	/// The phase reading a FRAME's first field, after any object record.
	fn first_field(framing: lite::Framing) -> IngestPhase {
		match framing.length_first {
			true => IngestPhase::Length,
			false => IngestPhase::Timing { size: None },
		}
	}

	/// Whether an explicit IETF `END_OF_GROUP` record completed this stream.
	fn saw_end_object(&self) -> bool {
		matches!(self.phase, IngestPhase::Ended)
	}

	/// The header of the frame whose fields were just read, checking the object record that
	/// preceded it under the `ietf` extension.
	fn header(
		&mut self,
		group: &mut crate::recv::Group,
		timestamp: Option<Timestamp>,
		inflated: Option<u64>,
		size: u64,
	) -> Result<frame::Header, Error> {
		// A frame that says nothing a plain frame would not keeps no fields.
		let fields = match &mut self.records {
			Some(records) => {
				let (id, block) = records.pending.take().expect("a record precedes each frame");
				let lite::ietf::Decoded { properties, gap } = lite::ietf::decode_properties(&block, self.framing)?;
				let plain = id == group.frame_count() as u64 && properties.is_empty();
				if !plain && !records.relayed {
					return Err(Error::ProtocolViolation);
				}
				// Only a frame we accept names the keys below its group.
				group.mark_gap(id, gap)?;
				(!plain).then(|| Arc::new(crate::model::object::Fields { id, properties }))
			}
			None => None,
		};
		let timestamp = Timestamp::received(timestamp, group.key);
		Ok(frame::Header {
			info: frame::Info { size, timestamp },
			fields,
			inflated,
		})
	}

	/// Read a frame header varint. EOF may end a plain group only at a frame's
	/// first field (`boundary`); a truncated header is an error.
	fn poll_header_varint<R: crate::transport::poll::RecvStream>(
		&self,
		reader: &mut Reader<R, Version>,
		boundary: bool,
		waiter: &kio::Waiter,
	) -> Poll<Result<Option<u64>, Error>> {
		if boundary && self.records.is_none() {
			reader.poll_varint_maybe(waiter)
		} else {
			reader.poll_varint(waiter).map_ok(Some)
		}
	}

	/// `Ready(Ok(end))` once the stream FINs on a frame boundary, with the group's end when a
	/// GROUP_END preceded the FIN. The caller finishes, closes, or aborts the group; a frame
	/// cut short mid-payload was already aborted here with the reason.
	pub(crate) fn poll<R: crate::transport::poll::RecvStream + crate::coding::Pause>(
		&mut self,
		reader: &mut Reader<R, Version>,
		group: &mut crate::recv::Group,
		waiter: &kio::Waiter,
	) -> Poll<Result<Option<u64>, Error>> {
		let mut budget = kio::coop::Budget::new(32);
		loop {
			match &mut self.phase {
				IngestPhase::State => {
					let state = ready!(reader.poll_varint(waiter))?;
					let wire = lite::ietf::decode_state(state, self.frame_start)?;
					let relayed = wire.is_some();
					let extensions = wire.as_ref().is_some_and(|wire| wire.extensions);
					if let Some(wire) = wire {
						group.set_wire(wire)?;
					}
					self.records = Some(RecordIngest {
						relayed,
						extensions,
						prior: None,
						pending: None,
					});
					self.phase = IngestPhase::Object;
				}
				IngestPhase::Object => {
					let Some(code) = ready!(reader.poll_varint_maybe(waiter))? else {
						return Poll::Ready(Ok(None));
					};
					let records = self.records.as_mut().expect("the Group State came first");
					let (id, end) = lite::ietf::decode_object(records.prior, code)?;
					if records.prior.is_none()
						&& let Some(mut wire) = group.wire()
					{
						wire.first_id = id;
						group.set_wire(wire)?;
					}
					records.prior = Some(id);
					self.phase = IngestPhase::Properties { id, end, size: None };
				}
				IngestPhase::Properties { id, end, size } => {
					let len = match size {
						Some(len) => *len,
						None => {
							let len = ready!(reader.poll_varint(waiter))?;
							if len > lite::ietf::MAX_PROPERTIES as u64 {
								return Poll::Ready(Err(Error::BoundsExceeded(crate::coding::BoundsExceeded)));
							}
							*size.insert(len as usize)
						}
					};
					let block = ready!(reader.poll_read_exact(waiter, len))?;
					let (id, end) = (*id, *end);
					let records = self.records.as_mut().expect("the Group State came first");
					if (end && !records.relayed) || (!block.is_empty() && !records.extensions) {
						return Poll::Ready(Err(Error::ProtocolViolation));
					}
					if end {
						let decoded = lite::ietf::decode_end(&block, self.framing)?;
						group.mark_gap(id, decoded.gap)?;
						group.set_end_object(crate::model::object::Fields {
							id,
							properties: decoded.properties,
						})?;
						self.phase = IngestPhase::Ended;
					} else {
						records.pending = Some((id, block));
						self.phase = Self::first_field(self.framing);
					}
				}
				IngestPhase::Ended => {
					if ready!(reader.poll_has_more(waiter))? {
						return Poll::Ready(Err(Error::ProtocolViolation));
					}
					return Poll::Ready(Ok(None));
				}
				IngestPhase::Length => {
					let Some(length) = ready!(self.poll_header_varint(reader, true, waiter))? else {
						return Poll::Ready(Ok(None));
					};
					// 0 is GROUP_END, where a frame would start. Under object records a record
					// precedes every FRAME, and the group's end is the stream's.
					let Some(size) = length.checked_sub(1) else {
						if self.records.is_some() {
							return Poll::Ready(Err(Error::ProtocolViolation));
						}
						self.phase = IngestPhase::GroupEnd;
						continue;
					};
					self.phase = IngestPhase::Timing { size: Some(size) };
				}
				IngestPhase::GroupEnd => {
					let delta = ready!(reader.poll_varint(waiter))?;
					let end = self
						.clock
						.end(delta)
						.ok_or(Error::BoundsExceeded(crate::coding::BoundsExceeded))?;
					self.phase = IngestPhase::Finished { end };
				}
				IngestPhase::Finished { end } => {
					let end = *end;
					if ready!(reader.poll_has_more(waiter))? {
						return Poll::Ready(Err(Error::MalformedTrack));
					}
					return Poll::Ready(Ok(Some(end)));
				}
				IngestPhase::Timing { size } => {
					let size = *size;
					if !self.framing.stamped() {
						self.phase = IngestPhase::Inflated { size, timestamp: None };
						continue;
					}
					// Before lite-07 the timestamp delta doubles as the per-frame sentinel.
					let Some(zz) = ready!(self.poll_header_varint(reader, size.is_none(), waiter))? else {
						return Poll::Ready(Ok(None));
					};
					let next = self
						.clock
						.timestamp(crate::coding::varint::unzigzag(zz))
						.ok_or(Error::BoundsExceeded(crate::coding::BoundsExceeded))?;
					let scale = self.framing.timescale.expect("only a timed track is stamped");
					let timestamp = Timestamp::new(next, scale)
						.map_err(|_| Error::BoundsExceeded(crate::coding::BoundsExceeded))?;
					self.phase = IngestPhase::Inflated {
						size,
						timestamp: Some(timestamp),
					};
				}
				IngestPhase::Inflated { size, timestamp } => {
					let (size, timestamp) = (*size, *timestamp);
					// Only a compressed Track carries it, and only lite-07 can mark one.
					let inflated = match group.compressed() {
						true => {
							let boundary = size.is_none() && !self.framing.stamped();
							let Some(inflated) = ready!(self.poll_header_varint(reader, boundary, waiter))? else {
								return Poll::Ready(Ok(None));
							};
							Some(inflated)
						}
						false => None,
					};
					self.phase = match size {
						Some(size) => {
							let header = self.header(group, timestamp, inflated, size)?;
							IngestPhase::Admit { header }
						}
						None => IngestPhase::Size { timestamp, inflated },
					};
				}
				IngestPhase::Size { timestamp, inflated } => {
					let (timestamp, inflated) = (*timestamp, *inflated);
					let boundary = !self.framing.stamped() && !group.compressed();
					let Some(size) = ready!(self.poll_header_varint(reader, boundary, waiter))? else {
						return Poll::Ready(Ok(None));
					};
					let header = self.header(group, timestamp, inflated, size)?;
					self.phase = IngestPhase::Admit { header };
				}
				IngestPhase::Admit { header } => {
					ready!(reader.poll_admit(waiter, group, header))?;
					let IngestPhase::Admit { header } = std::mem::replace(&mut self.phase, IngestPhase::Length) else {
						unreachable!()
					};
					// `create_object` is the allocation chokepoint: it rejects an oversized
					// `size` and allocates up front only within the origin's budget, so no
					// pre-check is needed. No wire timestamp (pre-lite-05) means an untimed
					// frame. A compressed frame stores the slice as it arrives, its inflated
					// size checked against the same limits before anything inflates.
					let frame = group.create_object(header)?;
					self.phase = IngestPhase::Payload { frame };
				}
				IngestPhase::Payload { frame } => {
					let failed = ready!(reader.poll_read_frame(waiter, frame)).err();

					let next = self.next_frame();
					let IngestPhase::Payload { frame } = std::mem::replace(&mut self.phase, next) else {
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
					// An empty frame is a few bytes, so a peer keeping them ready must not
					// hold the session's other tasks.
					ready!(budget.poll_yield(waiter));
				}
			}
		}
	}

	/// Synchronously drain any already-buffered lite-07 frames to extract a trailing `GROUP_END`
	/// without allocating or blocking. Returns `Some(end)` only when the entire stream up to
	/// `GROUP_END` + `FIN` is already buffered locally; returns `None` if more bytes are still on
	/// the wire (in which case stopping the stream sends `STOP_SENDING` to the still-live
	/// publisher stream).
	fn try_drain_end<R: crate::transport::poll::RecvStream>(
		&mut self,
		reader: &mut Reader<R, Version>,
		compressed: bool,
	) -> Option<u64> {
		if self.records.is_some() || !self.framing.length_first {
			return None;
		}
		let noop = kio::Waiter::noop();
		let mut max_ts = 0u64;
		let mut skip = match std::mem::replace(&mut self.phase, IngestPhase::Length) {
			IngestPhase::Payload { frame } => frame.remaining() as u64,
			IngestPhase::Admit { header } => {
				if header.info.timestamp.scale().is_some() {
					max_ts = max_ts.max(header.info.timestamp.value());
				}
				header.info.size
			}
			other @ (IngestPhase::Inflated { timestamp, .. } | IngestPhase::Size { timestamp, .. }) => {
				if let Some(ts) = timestamp {
					max_ts = max_ts.max(ts.value());
				}
				self.phase = other;
				0
			}
			other => {
				self.phase = other;
				0
			}
		};
		loop {
			if skip > 0 {
				match reader.poll_skip(&noop, &mut skip) {
					Poll::Ready(Ok(())) => continue,
					Poll::Ready(Err(_)) | Poll::Pending => return None,
				}
			}
			match &self.phase {
				IngestPhase::Length => match reader.poll_varint_maybe(&noop) {
					Poll::Ready(Ok(Some(0))) => self.phase = IngestPhase::GroupEnd,
					Poll::Ready(Ok(Some(length))) => {
						self.phase = IngestPhase::Timing { size: Some(length - 1) };
					}
					Poll::Ready(Ok(None)) | Poll::Ready(Err(_)) | Poll::Pending => return None,
				},
				IngestPhase::Timing { size } => {
					let size = *size;
					if self.framing.stamped() {
						let Poll::Ready(Ok(zz)) = reader.poll_varint(&noop) else {
							return None;
						};
						let next = self.clock.timestamp(crate::coding::varint::unzigzag(zz))?;
						max_ts = max_ts.max(next);
					}
					self.phase = IngestPhase::Inflated { size, timestamp: None };
				}
				IngestPhase::Inflated { size, .. } => {
					let size = (*size)?;
					if compressed {
						let Poll::Ready(Ok(_)) = reader.poll_varint(&noop) else {
							return None;
						};
					}
					skip = size;
					self.phase = IngestPhase::Length;
				}
				IngestPhase::GroupEnd => {
					let Poll::Ready(Ok(delta)) = reader.poll_varint(&noop) else {
						return None;
					};
					let end = self.clock.end(delta)?;
					if end < max_ts {
						return None;
					}
					self.phase = IngestPhase::Finished { end };
				}
				IngestPhase::Finished { end } => {
					let end = *end;
					if end < max_ts {
						return None;
					}
					return match reader.poll_has_more(&noop) {
						Poll::Ready(Ok(false)) => Some(end),
						_ => None,
					};
				}
				_ => return None,
			}
		}
	}
}

/// Receives QUIC datagrams and routes each to its subscription's track producer
/// (lite-05 §6.4).
///
/// A decode error or an unknown subscribe id drops that datagram without tearing
/// down the session (best-effort); only a transport-level failure ends the loop.
struct DatagramRecv {
	subscriber: Subscriber,
	// A dedicated receive handle: the poll interface takes `&mut self`.
	recv: Transport,
	enabled: bool,
	/// The datagrams with no known track before the sources ran, routed once they have. At
	/// most one pass's budget, and empty again by the end of each driver poll.
	held: Vec<bytes::Bytes>,
	/// Whether the last pass read the transport's queue empty, or the receive ended, rather
	/// than stopping at its budget.
	caught_up: bool,
}

impl DatagramRecv {
	fn new(subscriber: Subscriber) -> Self {
		let recv = subscriber.session.clone();
		let enabled = subscriber.version.has_datagrams() && recv.max_datagram_size() > 0;
		Self {
			subscriber,
			recv,
			enabled,
			held: Vec::new(),
			caught_up: true,
		}
	}

	/// Route the ready datagrams. With `hold`, before the sources run, keep each with no known
	/// track: its answer may be among what they decode. The pass after them, in the same
	/// driver poll, routes those first, then the rest, dropping and counting each that still
	/// finds no track.
	fn poll(&mut self, waiter: &kio::Waiter, hold: bool) -> Poll<Result<(), Error>> {
		if !self.enabled {
			return Poll::Ready(Ok(()));
		}
		if !hold {
			for payload in std::mem::take(&mut self.held) {
				self.route(payload, false);
			}
		}
		// A peer keeping datagrams ready must not hold the session's other tasks, and bounds
		// what the first pass holds.
		let mut budget = kio::coop::Budget::new(32);
		loop {
			self.caught_up = false;
			ready!(budget.poll_yield(waiter));
			self.caught_up = true;
			let payload = match ready!(self.recv.poll_recv_datagram(waiter)) {
				Ok(payload) => payload,
				Err(err) => {
					// The session ends without the sources' answers: what is held finds no track.
					for payload in std::mem::take(&mut self.held) {
						self.route(payload, false);
					}
					return Poll::Ready(Err(Error::from_transport(err)));
				}
			};
			self.route(payload, hold);
		}
	}

	/// Route one datagram, holding it with `hold` when its track isn't known yet.
	fn route(&mut self, payload: bytes::Bytes, hold: bool) {
		match self.subscriber.route_datagram(payload.clone()) {
			Ok(None) => {}
			Ok(Some(_)) if hold => self.held.push(payload),
			Ok(Some(subscribe)) => {
				self.subscriber
					.datagrams_unrouted
					.fetch_add(1, atomic::Ordering::Relaxed);
				tracing::debug!(subscribe, "dropping datagram for a subscription with no datagram track");
			}
			Err(err) => tracing::debug!(%err, "dropping datagram"),
		}
	}
}

/// Opens a PROBE stream on demand while a consumer is interested, and sends it the
/// viewers' target when the peer probes up to one.
///
/// Loops forever: wait for a consumer, race the probe stream against the
/// consumer leaving, then loop back. Transport errors stay per-stream; malformed
/// control bytes end the session.
struct RecvBandwidth {
	subscriber: Subscriber,
	state: BandwidthState,
	/// The viewers' target, followed only once the peer advertised `Increase`.
	target: Option<bandwidth::Target>,
	/// The target last read from it.
	wanted: bandwidth::Rate,
}

#[expect(
	clippy::large_enum_variant,
	reason = "a state machine's enum is its storage: one transient instance per stream, so the big variant is the working state, not padding held in bulk"
)]
enum BandwidthState {
	/// lite-05+ negotiates probing: only open a PROBE stream if the peer
	/// advertised it (Report or higher) in its SETUP. Older versions have no
	/// SETUP, so probe is always available there.
	Gate,
	/// Wait until at least one consumer is interested in the estimate.
	WaitUsed,
	/// Race the last consumer leaving against the probe stream ending.
	Probing(ProbeStream),
}

impl RecvBandwidth {
	fn new(subscriber: Subscriber) -> Self {
		Self {
			subscriber,
			state: BandwidthState::Gate,
			target: None,
			wanted: bandwidth::Rate::ZERO,
		}
	}

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		loop {
			match &mut self.state {
				BandwidthState::Gate => {
					if self.subscriber.recv_bandwidth.is_none() {
						return Poll::Ready(Ok(()));
					}
					if self.subscriber.version.has_setup_stream() {
						let level = ready!(self.subscriber.peer_setup.poll_probe_level(waiter));
						if level < lite::ProbeLevel::Report {
							tracing::debug!("peer does not support probing; skipping probe stream");
							return Poll::Ready(Ok(()));
						}
						// A Report-level peer ignores a target, so only a peer that probes is sent one.
						if level >= lite::ProbeLevel::Increase {
							self.target = Some(self.subscriber.viewers.target());
						}
					}
					self.state = BandwidthState::WaitUsed;
				}
				BandwidthState::WaitUsed => {
					let bandwidth = self.subscriber.recv_bandwidth.as_ref().expect("gated above");
					match ready!(bandwidth.poll_wanted_changed(waiter, false)) {
						Ok(_) => self.state = BandwidthState::Probing(ProbeStream::new(&self.subscriber)),
						Err(_) => return Poll::Ready(Ok(())),
					}
				}
				BandwidthState::Probing(probe) => {
					let bandwidth = self.subscriber.recv_bandwidth.as_ref().expect("gated above");
					match bandwidth.poll_wanted_changed(waiter, true) {
						// Loop back: a new consumer may arrive later. Dropping the probe
						// machine resets its stream, and clearing the estimate keeps that
						// consumer from reading a stale rate before the new stream reports.
						Poll::Ready(Ok(_)) => {
							let _ = bandwidth.set(None);
							self.state = BandwidthState::WaitUsed;
							continue;
						}
						// The channel closed: give up for the rest of the session.
						Poll::Ready(Err(_)) => return Poll::Ready(Ok(())),
						Poll::Pending => {}
					}
					if let Some(target) = &mut self.target {
						while let Poll::Ready(rate) = target.poll_changed(waiter) {
							self.wanted = rate;
						}
					}
					match ready!(probe.poll(waiter, self.wanted)) {
						Ok(()) => tracing::debug!("probe stream closed"),
						Err(err) => {
							let err = lite::error::control(&mut self.subscriber.session.clone(), err);
							match &err {
								// The session or the peer ended it cleanly.
								Error::Cancel
								| Error::Stream(crate::StreamError::Cancel)
								| Error::Session(crate::SessionError::Cancel) => {
									tracing::debug!(%err, "probe stream cancelled")
								}
								err => tracing::warn!(%err, "probe stream error"),
							}
						}
					}
					// The stream ended (peer FIN'd or errored). Don't hammer an
					// uncooperative peer; give up for the rest of the session, and
					// clear the estimate so consumers don't keep reading its last rate.
					let bandwidth = self.subscriber.recv_bandwidth.as_ref().expect("gated above");
					let _ = bandwidth.set(None);
					return Poll::Ready(Ok(()));
				}
			}
		}
	}
}

/// One PROBE stream: send the type, then feed the peer's estimates into the
/// bandwidth producer until it FINs, sending each new target along the way.
struct ProbeStream {
	/// Its place in line for the peer's stream credit while its stream opens.
	open: crate::transport::open::Open,
	subscriber: Subscriber,
	session: Transport,
	state: ProbeState,
	/// The target last sent; zero sends none.
	sent: bandwidth::Rate,
}

#[kio::machine(self.state: ProbeState)]
impl ProbeStream {
	fn new(subscriber: &Subscriber) -> Self {
		Self {
			open: Default::default(),
			subscriber: subscriber.clone(),
			session: subscriber.session.clone(),
			state: ProbeState::Start,
			sent: bandwidth::Rate::ZERO,
		}
	}

	/// Run the stream, sending `target` whenever it differs from the last one sent.
	fn poll(&mut self, waiter: &kio::Waiter, target: bandwidth::Rate) -> Poll<Result<(), Error>> {
		let mut stream: Stream<Transport, Version> = wait!(Open => {
			// Probe is best-effort telemetry; a session that is going away
			// has no use for a new estimate, so skip it rather than erroring.
			if self.subscriber.going_away.is_set() {
				return Poll::Ready(Ok(()));
			}
			Stream::poll_open(&self.session, &mut self.open, self.subscriber.version, waiter)
		})?
		.with_stats(self.subscriber.origin.stats());
		stream.writer.buffer(&lite::ControlType::Probe)?;
		wait!(Send => stream.writer.poll_flush(waiter))?;
		Poll::Ready(wait!(Read => {
			// Deliver the target without holding up the estimates behind it. Only a flushed
			// writer takes the next one, so a peer that stops reading holds one target, and
			// the latest is sent once it drains.
			if let Poll::Ready(res) = stream.writer.poll_flush(waiter) {
				res?;
				if target != self.sent {
					// Zero reads as unknown on the wire, which retracts the last target.
					let bitrate = (target > bandwidth::Rate::ZERO).then_some(target.as_bps());
					stream.writer.buffer(&lite::Probe { bitrate, rtt: None })?;
					self.sent = target;
					if let Poll::Ready(res) = stream.writer.poll_flush(waiter) {
						res?;
					}
				}
			}
			let bandwidth = self.subscriber.recv_bandwidth.as_ref().expect("gated by RecvBandwidth");
			// Each estimate wakes every reader, so a peer streaming them must not hold
			// the session's other tasks.
			let mut budget = kio::coop::Budget::new(32);
			loop {
				ready!(budget.poll_yield(waiter));
				let Some(probe) = ready!(stream.reader.poll_decode_maybe::<lite::Probe>(waiter))? else {
					break Poll::Ready(Ok(()));
				};
				bandwidth.set(probe.bitrate.map(bandwidth::Rate::from_bps))?;
			}
		}))
	}
}

/// One announce-interest stream: sends the ANNOUNCE_REQUEST for a prefix, then
/// feeds every received announce into the origin. Only its *error* ends the
/// session; a publisher FIN is a clean end for the prefix alone.
struct AnnouncePrefix {
	/// Its place in line for the peer's stream credit while its stream opens.
	open: crate::transport::open::Open,
	subscriber: Subscriber,
	/// The prefix discovery wants, which keys the machine.
	prefix: Path,
	state: PrefixState,
	/// Holds the origin's announce consumers under the prefix unsynced
	/// until the peer's initial set is in. Dropped at the boundary the version
	/// marks, or with the stream.
	snapshot: Option<crate::model::Snapshot>,
}

impl kio::Task for AnnouncePrefix {
	type Output = Result<(), Error>;

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Self::Output> {
		AnnouncePrefix::poll(self, waiter)
	}
}

/// The announce-decode loop's state.
struct PrefixRun {
	responder_origin: Option<crate::Hop>,
	/// What we charge every announcement arriving on this stream. Resolved once:
	/// it comes from the connect config or the peer's SETUP, neither of which
	/// changes for the life of the session.
	link_cost: u64,
	announced: Announced,
	// Lite06+: announce ids. Each received `active` implicitly assigns the next
	// per-stream ordinal; `ended`/`restart` reference it instead of repeating the
	// path, and lite-07 bases name it too. Tracked even for announces we drop
	// locally (reflected loops), since the sender doesn't know we dropped them.
	decoder: lite::AnnounceDecoder,
	/// The auth epoch last applied, so a new limit holds back or brings back routes.
	auth_epoch: u64,
	/// Lite05+: the ANNOUNCE_STARTs of the initial set still to read. Anything else
	/// before them, or the stream ending, cuts the counted set short.
	initial: u64,
}

#[kio::machine(self.state: PrefixState)]
impl AnnouncePrefix {
	/// `snapshot` was taken when the prefix was first wanted, so a consumer of the
	/// origin never reads as synced before the request even goes out.
	fn new(subscriber: Subscriber, prefix: Path, snapshot: crate::model::Snapshot) -> Self {
		Self {
			open: Default::default(),
			subscriber,
			prefix,
			state: PrefixState::Start,
			snapshot: Some(snapshot),
		}
	}

	/// Fails the session on any error, [`Error::TooManyRequests`] included: a peer
	/// announcing more than the session allows loses the session.
	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		self.poll_run(waiter)
			.map_err(|err| lite::error::control(&mut self.subscriber.session.clone(), err))
	}

	fn poll_run(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		// Opening the control stream (after the GOAWAY gate).
		let mut stream: Stream<Transport, Version> = wait!(Open => Stream::poll_open(
			&self.subscriber.session,
			&mut self.open,
			self.subscriber.version,
			waiter
		))?
		.with_stats(self.subscriber.origin.stats());
		stream.writer.buffer(&lite::ControlType::Announce)?;
		// Lite04/05: ask the peer to filter out announces that already passed
		// through us, so the reflected ones never hit the wire. Encoding drops
		// this on every other version, where start_announce below is the only
		// filter.
		stream.writer.buffer(&lite::AnnounceRequest {
			prefix: self.prefix.clone(),
			exclude_hop: self.subscriber.self_origin,
		})?;
		wait!(Send => stream.writer.poll_flush(waiter))?;

		let version: Version = self.subscriber.version;
		// Lite03/04 mark no initial set, so the request going out is the best
		// boundary. Lite01/02 use ANNOUNCE_INIT, lite05/06 a count.
		if !version.has_announce_ok() && !matches!(version, Version::Lite01 | Version::Lite02) {
			self.snapshot = None;
		}

		// Lite05+: reading the publisher's ANNOUNCE_OK.
		let ok = wait!(ReadOk => match self.subscriber.version.has_announce_ok() {
			true => stream.reader.poll_decode::<lite::AnnounceOk>(waiter).map_ok(Some),
			false => Poll::Ready(Ok(None)),
		})?;
		// The publisher reports its own origin id, which we stamp onto every received
		// Announce's hop chain since it no longer does so itself. A peer may legally
		// report id 0 (no identity). Keep it: the assigned identity stays on `via` and
		// is never forwarded as a hop. Lite05/06 mark the initial set by count, the
		// ANNOUNCE_STARTs the publisher counted into it.
		let (responder_origin, initial): (Option<crate::Hop>, u64) = match ok {
			Some(ok) => {
				if ok.active == 0 {
					self.snapshot = None;
				}
				(Some(ok.origin), ok.active)
			}
			None => (None, 0),
		};

		// Waiting for the link cost (may block on the peer's SETUP).
		let link_cost: u64 = wait!(Cost => self.subscriber.poll_link_cost(waiter));
		let mut run: PrefixRun = PrefixRun {
			responder_origin,
			link_cost,
			announced: Announced::new(self.subscriber.announces.clone(), self.subscriber.minted.clone()),
			decoder: lite::AnnounceDecoder::default(),
			auth_epoch: 0,
			initial,
		};

		// Lite01/02 send the initial set as one ANNOUNCE_INIT message, so they
		// read that before the update stream. Every other version streams it as
		// ordinary announces. Each suffix takes an announce slot, so a set past
		// what is left of the budget is refused before it is decoded.
		let init = wait!(ReadInit => match self.subscriber.version {
			Version::Lite01 | Version::Lite02 => {
				let budget = run.announced.slots.available();
				stream
					.reader
					.poll_decode_with(waiter, |r| lite::AnnounceInit::decode_within(r, *version, budget))
					.map_ok(Some)
			}
			_ => Poll::Ready(Ok(None)),
		})?;
		if let Some(init) = init {
			for suffix in init.suffixes {
				let path = self.prefix.join(&suffix)?;
				// Lite01/02 don't carry hop information; the broadcast starts with
				// an empty chain and an unpriced link. Stats are attributed in the
				// model when this enters the origin via `route`.
				self.subscriber.start_announce(
					path,
					None,
					crate::Hops::new(),
					crate::origin::Cost::UNKNOWN,
					0,
					run.responder_origin,
					&mut run.announced,
				)?;
			}
			self.snapshot = None;
		}

		// Streaming announce updates.
		Poll::Ready(wait!(Run => {
			// A draining peer usually stops announcing, so react to the
			// GOAWAY itself; waiting for another message would leave the
			// route primary until the session finally closed. Idempotent,
			// since the signal stays set.
			if self.subscriber.going_away.poll(waiter).is_ready() {
				run.announced.drain();
			}
			// A new limit holds back what the peer may no longer publish to us,
			// and brings back what it may again. The peer still holds each
			// advertisement either way.
			while let Poll::Ready(permit) =
				self.subscriber
					.auth
					.poll_permit(crate::auth::Direction::Subscribe, &mut run.auth_epoch, waiter)
			{
				run.announced.limit(&permit, &self.subscriber);
			}
			// Each announce routes into the origin, so a peer streaming them must not hold
			// the session's other tasks. A yield has woken the task, and this arm always
			// ends Pending, so the routes below are still served this turn.
			let mut budget = kio::coop::Budget::new(32);
			while budget.poll_yield(waiter).is_ready() {
				match stream.reader.poll_decode_maybe::<lite::AnnounceBroadcast>(waiter) {
					Poll::Ready(Ok(Some(announce))) => {
						if run.initial > 0 {
							match announce {
								lite::AnnounceBroadcast::Active { .. } => run.initial -= 1,
								// An unknown type is no change, so it neither counts nor cuts.
								lite::AnnounceBroadcast::Skipped => {}
								_ => return Poll::Ready(Err(Error::ProtocolViolation)),
							}
						}
						self.subscriber.handle_announce(&self.prefix, announce, run)?;
						// Released only once the last counted route is in the origin.
						if run.initial == 0 {
							self.snapshot = None;
						}
					}
					Poll::Ready(Ok(None)) if run.initial > 0 => {
						return Poll::Ready(Err(Error::ProtocolViolation));
					}
					Poll::Ready(Ok(None)) => {
						// The publisher FINed: it has nothing (more) to announce for this
						// prefix (e.g. a publish-only peer). That's a clean completion of
						// this announce stream, not a session error, so finish our side
						// and return Ok. Tearing down only the announce stream is correct
						// since no further progress can be made, but we must not
						// propagate an error that would kill the whole connection.
						stream.writer.finish().ok();
						return Poll::Ready(Ok(()));
					}
					Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
					Poll::Pending => break,
				}
			}
			// Serve the routes whose request queue woke, including any attached
			// above. Each route parks on its own waker, so the rest cost nothing.
			run.announced.poll_serve(&self.subscriber, waiter);
			Poll::Pending
		}))
	}
}

/// A source the announce half minted for one requested path, for the driver to serve.
struct MintedSource {
	path: Path,
	/// The instance the route announced, asked for on every request.
	instance: Option<crate::Instance>,
	/// The node every request names (lite-07), or none to let the peer select.
	node: Option<crate::route::NodeId>,
	source: crate::model::broadcast::SourceGuard,
	/// The source's track handler, registered before its requester could ask for a track.
	dynamic: crate::broadcast::Requests,
	/// Closes when the route that minted the source goes.
	route: kio::Consumer<()>,
}

/// Serves the origin's track requests for one minted source until it closes: its
/// route goes, the peer stops selecting its instance, nothing holds it any more, or
/// the session dies. A closed source
/// takes no new tracks but lets those in flight run to their own end (moq-lite:
/// retraction does not disturb subscriptions already in flight); the session
/// dying drops them.
struct SourceServe {
	subscriber: Subscriber,
	path: Path,
	instance: Option<crate::Instance>,
	node: Option<crate::route::NodeId>,
	source: crate::model::broadcast::SourceGuard,
	dynamic: crate::broadcast::Requests,
	route: kio::Consumer<()>,
	// A dedicated close-watch handle, since each pending operation needs its own.
	closed: Transport,
	tracks: kio::Tasks<TrackServeRun>,
	/// Answers the datagram tracks consumers ask the source for.
	datagram_requests: crate::broadcast::DatagramRequests,
	datagrams: kio::Tasks<DatagramServe>,
	// The source ended: no more track requests will arrive.
	ended: bool,
	/// Answers listings of the source with the peer's TRACKS.
	lister: crate::broadcast::Lister,
	/// The peer's listing, while anyone lists the source.
	listing: Option<TracksRecv>,
}

/// Forwards the peer's TRACKS listing of a source into the source's lister.
struct TracksRecv {
	/// Its place in line for the peer's stream credit while its stream opens.
	open: crate::transport::open::Open,
	session: Transport,
	stream: Option<Stream<Transport, Version>>,
	/// What the peer lists, by kind then name, each holding an `announces` slot: discovery
	/// state the peer makes us hold.
	listed: TracksListed,
	synced: bool,
	/// Each event fans out to every reader, so a peer streaming add/remove churn must not
	/// hold the thread for as long as its bytes keep arriving.
	budget: kio::coop::Budget,
	/// A listing is a subscribe: it waits for a token that may cover the broadcast, and
	/// ends once the session stops allowing it.
	gate: crate::auth::Gate,
	/// Until the peer answers, what the source must still hold: the selected instance
	/// until the request is out, then no replacement, since a TRACKS request without an
	/// instance lists whichever one won the path when it arrived.
	unanswered: Option<crate::model::broadcast::RequestValidity>,
}

/// The tracks a peer lists, one map per kind since each kind is a namespace apart.
#[derive(Default)]
struct TracksListed {
	tracks: HashMap<Arc<str>, crate::session::Slot>,
	datagrams: HashMap<Arc<str>, crate::session::Slot>,
}

impl TracksListed {
	fn of(&mut self, kind: crate::broadcast::Kind) -> &mut HashMap<Arc<str>, crate::session::Slot> {
		match kind {
			crate::broadcast::Kind::Track => &mut self.tracks,
			crate::broadcast::Kind::Datagrams => &mut self.datagrams,
		}
	}

	/// The key listed for `name` of `kind`, shared rather than copied.
	fn known(&self, kind: crate::broadcast::Kind, name: &str) -> Option<Arc<str>> {
		let listed = match kind {
			crate::broadcast::Kind::Track => &self.tracks,
			crate::broadcast::Kind::Datagrams => &self.datagrams,
		};
		listed.get_key_value(name).map(|(name, _)| name.clone())
	}
}

/// A TRACKS event, with its name resolved against what the listing holds.
#[derive(Debug)]
enum Listing {
	Added(Arc<str>, lite::Listed),
	/// The listed name of that kind, or `None` if nothing of that kind and name is listed.
	Removed(crate::broadcast::Kind, Option<Arc<str>>),
	/// Like [`Self::Removed`].
	Finished(crate::broadcast::Kind, Option<Arc<str>>),
	Synced,
}

impl TracksRecv {
	/// Apply the peer's events until it ends the listing, or the error that ends it.
	fn poll(&mut self, source: &SourceServe, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		match (&self.unanswered, &self.stream) {
			(Some(validity), None) => validity.check(waiter)?,
			(Some(validity), Some(_)) => validity.check_unanswered(waiter)?,
			(None, _) => {}
		}
		ready!(self.gate.poll_ready(waiter));
		if self.gate.poll_denied(waiter).is_ready() {
			return Poll::Ready(Err(Error::Unauthorized));
		}
		let version = source.subscriber.version;
		let stream = match &mut self.stream {
			Some(stream) => stream,
			None => {
				let mut stream = ready!(Stream::poll_open(&self.session, &mut self.open, version, waiter))?;
				stream = stream.with_stats(source.subscriber.origin.stats());
				stream.writer.buffer(&lite::ControlType::Tracks)?;
				stream.writer.buffer(&lite::Tracks {
					broadcast: source.path.clone(),
					instance: source.instance.clone(),
					node: source.node,
				})?;
				self.stream.insert(stream)
			}
		};
		ready!(stream.writer.poll_flush(waiter))?;
		loop {
			ready!(self.budget.poll_yield(waiter));
			// Resolved while the name is still the wire's: only a new listing copies it, and
			// a removal is a lookup.
			let listed = &self.listed;
			let event = ready!(stream.reader.poll_decode_maybe_with(waiter, |r| {
				Ok(match lite::TracksEvent::decode(r, version)? {
					lite::TracksEvent::Added { name, info } => Listing::Added(name.as_ref().into(), info),
					lite::TracksEvent::Removed { kind, name } => Listing::Removed(kind, listed.known(kind, &name)),
					lite::TracksEvent::Finished { kind, name } => Listing::Finished(kind, listed.known(kind, &name)),
					lite::TracksEvent::Synced => Listing::Synced,
				})
			}))?;
			self.unanswered = None;
			let Some(event) = event else {
				return Poll::Ready(Ok(()));
			};
			match event {
				Listing::Added(name, info) => {
					let listed = self.listed.of(info.kind());
					if listed.contains_key(&name) {
						return Poll::Ready(Err(Error::ProtocolViolation));
					}
					let slot = source.subscriber.announces.acquire()?;
					match info {
						lite::Listed::Track(info) => source.lister.add(name.clone(), crate::track::Info::from(info))?,
						lite::Listed::Datagrams(info) => source.lister.add(name.clone(), datagram::Info::from(info))?,
					}
					listed.insert(name, slot);
				}
				Listing::Removed(kind, name) => {
					let name = name.ok_or(Error::ProtocolViolation)?;
					self.listed.of(kind).remove(&name);
					source.lister.remove(kind, name)?;
				}
				Listing::Finished(kind, name) => {
					let name = name.ok_or(Error::ProtocolViolation)?;
					self.listed.of(kind).remove(&name);
					source.lister.finish(kind, name)?;
				}
				Listing::Synced => {
					if std::mem::replace(&mut self.synced, true) {
						return Poll::Ready(Err(Error::ProtocolViolation));
					}
					// Synced when the peer is, so a track listed after it is new upstream too.
					source.lister.synced();
				}
			}
		}
	}
}

impl SourceServe {
	fn new(subscriber: Subscriber, minted: MintedSource) -> Self {
		let closed = subscriber.session.clone();
		Self {
			lister: minted
				.dynamic
				.lister()
				.expect("only the source answers its broadcast's listings"),
			listing: None,
			subscriber,
			path: minted.path,
			instance: minted.instance,
			node: minted.node,
			source: minted.source,
			datagram_requests: minted.dynamic.datagrams(),
			dynamic: minted.dynamic,
			route: minted.route,
			closed,
			tracks: kio::Tasks::new(),
			datagrams: kio::Tasks::new(),
			ended: false,
		}
	}

	/// Answer the source's listings while anyone lists it, from the peer's TRACKS.
	fn poll_listing(&mut self, waiter: &kio::Waiter) {
		// Taken out while it runs, since it reads the rest of the source.
		let mut listing = self.listing.take();
		loop {
			match &mut listing {
				None => {
					if !matches!(self.lister.poll_wanted(waiter), Poll::Ready(Ok(()))) {
						break;
					}
					if !self.subscriber.version.has_tracks() {
						// An empty answer would claim the peer lists nothing.
						self.lister.abort(Error::Unsupported);
						continue;
					}
					listing = Some(TracksRecv {
						open: Default::default(),
						session: self.subscriber.session.clone(),
						stream: None,
						listed: TracksListed::default(),
						synced: false,
						budget: kio::coop::Budget::new(32),
						gate: crate::auth::Gate::new(
							self.subscriber.auth.clone(),
							self.path.clone(),
							crate::auth::Direction::Subscribe,
						),
						unanswered: Some(self.dynamic.validity().start()),
					});
				}
				Some(_) if self.lister.poll_unwanted(waiter).is_ready() => {
					// Dropping the stream resets it.
					listing = None;
					self.lister.clear();
				}
				Some(recv) => match recv.poll(self, waiter) {
					Poll::Pending => break,
					// The peer's broadcast ended, whether or not its retraction has arrived:
					// end the listing and let go of what it listed.
					Poll::Ready(Ok(())) => {
						listing = None;
						self.lister.end();
					}
					Poll::Ready(Err(err)) => {
						let err = lite::error::control(&mut self.subscriber.session.clone(), err);
						tracing::debug!(broadcast = %self.path, %err, "tracks failed");
						if let Some(stream) = recv.stream.take() {
							stream.writer.abort(&err);
						}
						listing = None;
						self.lister.abort(err);
					}
				},
			}
		}
		self.listing = listing;
	}
}

impl kio::Task for SourceServe {
	type Output = ();

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		let _ = self.tracks.poll(waiter);
		let _ = self.datagrams.poll(waiter);
		// A batch of queued requests must not hold the thread from the session's other tasks.
		let mut budget = kio::coop::Budget::new(32);

		loop {
			if self.closed.poll_closed(waiter).is_ready() {
				// Session gone.
				return Poll::Ready(());
			}
			self.poll_listing(waiter);
			if self.ended {
				// Done once the tracks in flight are.
				ready!(self.datagrams.poll(waiter));
				return self.tracks.poll(waiter);
			}
			if let Poll::Ready(Ok(request)) = self.datagram_requests.poll_requested(waiter) {
				let serve = DatagramServe::new(
					self.subscriber.clone(),
					self.path.clone(),
					(self.instance.clone(), self.node),
					self.dynamic.validity().clone(),
					self.source.consume(),
					request,
				);
				self.datagrams.push(serve);
				ready!(budget.poll_yield(waiter));
				continue;
			}
			match self.dynamic.poll_requested_track(waiter) {
				Poll::Ready(Ok(request)) => {
					let serve = TrackServe {
						validity: self.dynamic.validity().clone(),
						route: Some(self.route.clone()),
						source: Some((*self.source).clone()),
						subscriber: self.subscriber.clone(),
						path: self.path.clone(),
						instance: self.instance.clone(),
						node: self.node,
						name: request.name().to_string(),
					};
					// One machine per track serves its lone subscription and any number
					// of fetches concurrently.
					self.tracks.push(TrackServeRun::new(serve, request));
					ready!(budget.poll_yield(waiter));
				}
				// The source was finished (unannounced) or aborted.
				Poll::Ready(Err(err)) => {
					tracing::debug!(%err, "source closed");
					self.ended = true;
				}
				Poll::Pending => {
					// The route that minted the source went: close it now.
					if self.route.poll_closed(waiter).is_ready() {
						self.source.close();
						continue;
					}
					// Nothing holds the source and no track is on its way: retire it, so
					// what the session keeps for the path goes with the fronts that used
					// it, and a later request asks the route afresh. Declined when a holder
					// or a track got there first, so look again either way.
					if self.source.poll_unheld(waiter).is_pending() {
						break;
					}
					self.source.close_unheld();
				}
			}
		}

		// Newly requested tracks start now rather than on the next wake.
		let _ = self.tracks.poll(waiter);
		let _ = self.datagrams.poll(waiter);
		Poll::Pending
	}
}

/// Serves one datagram track a consumer asked a source for: subscribes to it upstream, and
/// keeps the subscriber priority current until nobody reads it or it ends. The session
/// routes its datagrams by Subscribe ID (see [`Subscriber::route_datagram`]).
///
/// Lite-07 asks on a Datagrams Stream. Lite-05/06 carry a name's stream track and datagram
/// track on one SUBSCRIBE, so this subscribes to the name's stream track through the source,
/// sharing its one upstream subscription with any stream reader, and takes the datagrams it
/// carries. Older versions carry no datagrams.
struct DatagramServe {
	/// Its place in line for the peer's stream credit while its stream opens.
	open: crate::transport::open::Open,
	subscriber: Subscriber,
	path: Path,
	instance: Option<crate::Instance>,
	node: Option<crate::route::NodeId>,
	validity: crate::model::broadcast::RequestValidity,
	// A dedicated close-watch handle.
	closed: Transport,
	id: u64,
	/// Lite-05/06: the source to subscribe to the stream track through, until it is asked.
	source: Option<crate::broadcast::Consumer>,
	state: DatagramState,
	/// Cancels the track once the session stops allowing its broadcast.
	gate: crate::auth::Gate,
	/// Holds the session busy while the track is subscribed.
	_working: crate::session::Working,
}

enum DatagramState {
	/// Asking the publisher: DATAGRAMS on lite-07, the stream track on lite-05/06.
	Open {
		request: datagram::Request,
	},
	/// Lite-07: waiting for DATAGRAMS_INFO.
	Info {
		request: datagram::Request,
		stream: Stream<Transport, Version>,
		/// The priority DATAGRAMS carried.
		priority: i8,
	},
	/// Lite-05/06: waiting for the stream track's subscription.
	Subscribe {
		request: datagram::Request,
		subscribing: track::Subscribing,
	},
	/// Lite-07: subscribed, forwarding priority changes until the track ends or nobody
	/// reads it.
	Live {
		producer: datagram::Producer,
		stream: Stream<Transport, Version>,
		priority: i8,
	},
	/// Lite-05/06: taking the stream track subscription's datagrams, with its priority,
	/// until the track ends or nobody reads them.
	Shared {
		producer: datagram::Producer,
		/// Boxed: far larger than the other states.
		subscriber: Box<track::Subscriber>,
		priority: i8,
	},
	/// The publisher ended the track: waiting for the end to apply, see
	/// [`Subscriber::end_datagrams`]. Lite-07 keeps its stream until then.
	Ending {
		applied: kio::Consumer<()>,
		writer: Option<crate::coding::Writer<crate::transport::boxed::SendStream, Version>>,
	},
	/// Our side finished: waiting for the publisher to read it.
	Finish {
		writer: crate::coding::Writer<crate::transport::boxed::SendStream, Version>,
	},
	Done,
}

impl DatagramServe {
	fn new(
		subscriber: Subscriber,
		path: Path,
		(instance, node): (Option<crate::Instance>, Option<crate::route::NodeId>),
		validity: crate::model::broadcast::RequestValidity,
		source: crate::broadcast::Consumer,
		request: datagram::Request,
	) -> Self {
		let id = subscriber.next_id.fetch_add(1, atomic::Ordering::Relaxed);
		Self {
			open: Default::default(),
			gate: crate::auth::Gate::new(subscriber.auth.clone(), path.clone(), crate::auth::Direction::Subscribe),
			_working: subscriber.session.work().start(),
			closed: subscriber.session.clone(),
			subscriber,
			path,
			instance,
			node,
			validity,
			id,
			source: Some(source),
			state: DatagramState::Open { request },
		}
	}

	/// Route the track's datagrams to `producer` from now on.
	fn register(&self, producer: &datagram::Producer, timescale: Option<Timescale>) {
		self.subscriber.datagrams.lock().insert(
			self.id,
			DatagramEntry {
				producer: producer.clone(),
				timescale,
			},
		);
	}

	/// Lite-05/06: route the datagrams of the name's stream track subscription to `producer`.
	fn register_shared(&self, producer: &datagram::Producer, timescale: Option<Timescale>) {
		self.subscriber.legacy_datagrams.lock().insert(
			(self.path.clone(), producer.name().to_string()),
			DatagramEntry {
				producer: producer.clone(),
				timescale,
			},
		);
	}

	/// Stop routing to `producer`, unless another track took the name since.
	fn unregister_shared(&self, producer: &datagram::Producer) {
		let mut shared = self.subscriber.legacy_datagrams.lock();
		let key = (self.path.clone(), producer.name().to_string());
		if shared.get(&key).is_some_and(|entry| entry.producer.is_clone(producer)) {
			shared.remove(&key);
		}
	}

	/// Yield to the driver once the route is registered, so it routes the datagrams it held
	/// for this answer before the next pass may read the track's end, which removes the route.
	/// The answer, the first datagram and the end can share a receive pass.
	fn poll_routed(waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		waiter.waker().wake_by_ref();
		Poll::Pending
	}

	fn name(&self) -> &str {
		match &self.state {
			DatagramState::Open { request }
			| DatagramState::Info { request, .. }
			| DatagramState::Subscribe { request, .. } => request.name(),
			DatagramState::Live { producer, .. } | DatagramState::Shared { producer, .. } => producer.name(),
			DatagramState::Ending { .. } | DatagramState::Finish { .. } | DatagramState::Done => "",
		}
	}

	fn poll_serve(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		let version = self.subscriber.version;
		loop {
			if let Poll::Ready(err) = self.closed.poll_closed(waiter) {
				return Poll::Ready(Err(Error::from_transport(err)));
			}
			if self.poll_unused(waiter).is_ready() {
				return Poll::Ready(Err(Error::Cancel));
			}
			match &mut self.state {
				DatagramState::Open { request } => {
					self.validity.check(waiter)?;
					if !version.has_datagrams() || !version.has_track_stream() || self.closed.max_datagram_size() == 0 {
						return Poll::Ready(Err(Error::Unsupported));
					}
					let priority = request.priority().unwrap_or_default();
					if !version.has_datagram_tracks() {
						let source = self.source.take().expect("asked once");
						let subscription = track::Subscription::default().with_priority(priority);
						let subscribing = source.track(request.name())?.subscribe(subscription).into_inner();
						let DatagramState::Open { request } = std::mem::replace(&mut self.state, DatagramState::Done)
						else {
							unreachable!()
						};
						self.state = DatagramState::Subscribe { request, subscribing };
						continue;
					}
					let mut stream = ready!(Stream::poll_open(
						&self.subscriber.session,
						&mut self.open,
						version,
						waiter
					))?
					.with_stats(self.subscriber.origin.stats());
					let DatagramState::Open { request } = std::mem::replace(&mut self.state, DatagramState::Done)
					else {
						unreachable!()
					};
					stream.writer.buffer(&lite::ControlType::Datagrams)?;
					stream.writer.buffer(&lite::Datagrams {
						id: self.id,
						broadcast: self.path.clone(),
						instance: self.instance.clone(),
						node: self.node,
						track: request.name().to_string().into(),
						priority,
					})?;
					self.state = DatagramState::Info {
						request,
						stream,
						priority,
					};
				}
				DatagramState::Info { stream, .. } => {
					self.validity.check_unanswered(waiter)?;
					ready!(stream.writer.poll_flush(waiter))?;
					let info = ready!(stream.reader.poll_decode::<lite::DatagramsInfo>(waiter))?;
					let DatagramState::Info {
						request,
						stream,
						priority,
					} = std::mem::replace(&mut self.state, DatagramState::Done)
					else {
						unreachable!()
					};
					// A change since DATAGRAMS went out reaches the publisher as an update.
					let producer = request.accept(info.into());
					self.register(&producer, info.timescale);
					self.state = DatagramState::Live {
						producer,
						stream,
						priority,
					};
					return Self::poll_routed(waiter);
				}
				DatagramState::Subscribe { subscribing, .. } => {
					let subscriber = ready!(subscribing.poll_ok(waiter))?;
					let DatagramState::Subscribe { request, .. } =
						std::mem::replace(&mut self.state, DatagramState::Done)
					else {
						unreachable!()
					};
					let info = subscriber.info();
					let timescale = info.timescale;
					let priority = subscriber.subscription().priority;
					let producer = request.accept(
						datagram::Info::default()
							.with_priority(info.priority)
							.with_timescale(timescale),
					);
					self.register_shared(&producer, timescale);
					// A change since the subscription went out reaches it on the next pass.
					self.state = DatagramState::Shared {
						producer,
						subscriber: Box::new(subscriber),
						priority,
					};
					return Self::poll_routed(waiter);
				}
				DatagramState::Live {
					producer,
					stream,
					priority,
				} => {
					// Nothing follows DATAGRAMS_INFO but the publisher's end.
					let ended = match ready_or(stream.reader.poll_decode_maybe::<lite::DatagramsInfo>(waiter)) {
						Some(Ok(None)) => Some(Ok(())),
						Some(Ok(Some(_))) => return Poll::Ready(Err(Error::ProtocolViolation)),
						Some(Err(err)) => Some(Err(err)),
						None => None,
					};
					if let Some(end) = ended {
						let DatagramState::Live { producer, stream, .. } =
							std::mem::replace(&mut self.state, DatagramState::Done)
						else {
							unreachable!()
						};
						if let Err(err) = end {
							self.subscriber.datagrams.lock().remove(&self.id);
							producer.abort(err.clone());
							return Poll::Ready(Err(err));
						}
						let mut ends = self.subscriber.datagram_ends.lock();
						ends.tracks.push((self.id, producer));
						self.state = DatagramState::Ending {
							applied: ends.applied.consume(),
							writer: Some(stream.writer),
						};
						continue;
					}
					if let Poll::Ready(next) = Self::poll_demand(producer, waiter, *priority) {
						*priority = next?;
						stream.writer.buffer(&lite::DatagramsUpdate { priority: *priority })?;
						continue;
					}
					ready!(stream.writer.poll_flush(waiter))?;
					return Poll::Pending;
				}
				DatagramState::Shared {
					producer,
					subscriber,
					priority,
				} => {
					// The stream track's end is the datagrams' end.
					if let Poll::Ready(end) = subscriber.poll_finished(waiter) {
						end?;
						let DatagramState::Shared { producer, .. } =
							std::mem::replace(&mut self.state, DatagramState::Done)
						else {
							unreachable!()
						};
						let key = (self.path.clone(), producer.name().to_string());
						let mut ends = self.subscriber.datagram_ends.lock();
						ends.shared.push((key, producer));
						self.state = DatagramState::Ending {
							applied: ends.applied.consume(),
							writer: None,
						};
						continue;
					}
					*priority = ready!(Self::poll_demand(producer, waiter, *priority))?;
					let subscription = subscriber.subscription().with_priority(*priority);
					subscriber.update(subscription)?;
				}
				DatagramState::Ending { applied, writer } => {
					ready!(applied.poll_closed(waiter));
					let Some(mut writer) = writer.take() else {
						return Poll::Ready(Ok(()));
					};
					if writer.finish().is_err() || !version.waits_for_subscriber_fin() {
						return Poll::Ready(Ok(()));
					}
					self.state = DatagramState::Finish { writer };
				}
				DatagramState::Finish { writer } => {
					let _ = ready!(writer.poll_close(waiter));
					return Poll::Ready(Ok(()));
				}
				DatagramState::Done => return Poll::Ready(Ok(())),
			}
		}
	}

	/// The subscriber priority once it differs from `sent`, or [`Error::Cancel`] once nobody
	/// reads the track. While only passive readers remain, the priority reads `None` but the
	/// track stays: park until demand returns or they leave.
	fn poll_demand(producer: &datagram::Producer, waiter: &kio::Waiter, sent: i8) -> Poll<Result<i8, Error>> {
		loop {
			match ready!(producer.poll_priority(waiter, Some(sent))) {
				Some(next) => return Poll::Ready(Ok(next)),
				// Atomic with a reader joining, whose priority the next pass reads.
				None if producer.close_unused() => return Poll::Ready(Err(Error::Cancel)),
				None => {
					if producer.poll_priority(waiter, None).is_pending() && producer.poll_unused(waiter).is_pending() {
						return Poll::Pending;
					}
				}
			}
		}
	}

	/// Ready once nobody waits for the track during setup, which then ended it. Atomic with a
	/// joining reader: one that arrived meanwhile parks the poll on it leaving. Once
	/// subscribed, the serve watches its priority instead.
	fn poll_unused(&self, waiter: &kio::Waiter) -> Poll<()> {
		loop {
			let closed = match &self.state {
				DatagramState::Open { request }
				| DatagramState::Info { request, .. }
				| DatagramState::Subscribe { request, .. } => {
					ready!(request.poll_unused(waiter));
					request.close_unused()
				}
				_ => return Poll::Pending,
			};
			if closed {
				return Poll::Ready(());
			}
		}
	}

	/// End the track and the request with `err`, unless it already ended. An ended track's
	/// route goes with its end instead, once the datagrams received with it routed: see
	/// [`Subscriber::end_datagrams`].
	fn abort(&mut self, err: Error) {
		match std::mem::replace(&mut self.state, DatagramState::Done) {
			DatagramState::Open { request } | DatagramState::Subscribe { request, .. } => request.reject(err),
			DatagramState::Info { request, stream, .. } => {
				stream.writer.abort(&err);
				request.reject(err);
			}
			DatagramState::Live { producer, stream, .. } => {
				self.subscriber.datagrams.lock().remove(&self.id);
				stream.writer.abort(&err);
				producer.abort(err);
			}
			DatagramState::Shared { producer, .. } => {
				self.unregister_shared(&producer);
				producer.abort(err);
			}
			DatagramState::Ending { .. } | DatagramState::Finish { .. } | DatagramState::Done => {}
		}
	}
}

/// A ready poll's value, or `None` while pending.
fn ready_or<T>(poll: Poll<T>) -> Option<T> {
	match poll {
		Poll::Ready(value) => Some(value),
		Poll::Pending => None,
	}
}

impl kio::Task for DatagramServe {
	type Output = ();

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		// Checked before the auth wait too, so an abandoned request never parks on a token.
		if self.poll_unused(waiter).is_ready() {
			self.abort(Error::Cancel);
			return Poll::Ready(());
		}
		ready!(self.gate.poll_ready(waiter));
		// Nothing is left to revoke once the track ended, nor on a closed session, whose end
		// decides how the track ends.
		if !matches!(
			self.state,
			DatagramState::Ending { .. } | DatagramState::Finish { .. } | DatagramState::Done
		) && self.gate.poll_denied(waiter).is_ready()
			&& self.closed.poll_ending(waiter).is_pending()
		{
			tracing::info!(broadcast = %self.subscriber.log_path(&self.path), track = %self.name(), "datagrams no longer authorized");
			self.abort(Error::Unauthorized);
			return Poll::Ready(());
		}
		let err = match ready!(self.poll_serve(waiter)) {
			Ok(()) => return Poll::Ready(()),
			Err(err) => err,
		};
		tracing::debug!(id = self.id, broadcast = %self.subscriber.log_path(&self.path), track = %self.name(), %err, "datagrams ended");
		self.abort(err);
		Poll::Ready(())
	}
}
impl Drop for DatagramServe {
	fn drop(&mut self) {
		// Dropped with its source when the session dies.
		let err = self.subscriber.end_reason();
		self.abort(err);
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::Timescale;
	use crate::coding::{Decode, Encode};
	use crate::lite::test_transport::{Sent, SinkSession};
	use crate::model::ProduceTest;
	use futures::FutureExt;

	/// A burst's answers naming the same largest position keep it ended if any says so, in
	/// either order: a later one from a copy yet to see the end can't reopen the group.
	#[test]
	fn newer_keeps_an_ended_largest() {
		let at = |ended| {
			lite::Edge::Largest(lite::Largest {
				position: Position { group: 5, frame: 3 },
				ended,
			})
		};
		assert_eq!(newer(at(true), at(false)), at(true));
		assert_eq!(newer(at(false), at(true)), at(true));
		assert_eq!(newer(at(false), at(false)), at(false));
	}

	/// The answer to a SUBSCRIBE's one range starting at group 0: on lite-07, `largest` says
	/// where the track is, or that it has no group yet.
	fn start_answer(version: Version, largest: Option<lite::Largest>) -> lite::SubscribeStart {
		let largest = match largest {
			_ if !version.has_ranges() => lite::Edge::Unknown,
			Some(largest) => lite::Edge::Largest(largest),
			None => lite::Edge::Empty,
		};
		lite::SubscribeStart {
			id: 0,
			start: Some(0),
			largest,
		}
	}

	const VERSION: Version = Version::Lite05;

	/// A pre-lite-05 relay's track, untimed, with a fetch of `key` queued on it.
	fn feed_fetch(key: u64) -> (track::Producer, kio::Pending<track::Fetching>, track::FillGroups) {
		let info = track::Info::default().with_timescale(None).with_max_age(None);
		let track = track::Producer::new(std::sync::Arc::new(crate::broadcast::Info::default()), "video", info);
		// The relay's copy of the upstream feed, whose groups may land out of order.
		track.received();
		let dynamic = track.requests();
		let fetching = track.consume().fetch_group(key, None);
		let req = dynamic.requested_fill().now_or_never().expect("queued").unwrap();
		(track, fetching, req)
	}

	/// A live feed whose ledger is `tail`.
	async fn feed_sub(tail: &kio::Producer<Tail>) -> Sub {
		let session = crate::lite::test_transport::ScriptedSession::new(Vec::new());
		Sub::Active(SubStream {
			order: crate::track::Order::Descending,
			stream: Stream::open(&crate::driver::Transport::new(session.clone()), Version::Lite04)
				.await
				.unwrap(),
			id: 0,
			max_delay: Some(Duration::ZERO),
			stale_budget: Duration::ZERO,
			stale_cap: None,
			start: None,
			priority: 0,
			requested: None,
			live: lite::LiveRanges::new(Version::Lite04, &[lite::Range::OPEN]),
			demand: Some(Subscription::default()),
			fill_ids: Default::default(),
			probe_ids: Default::default(),
			fill_groups: Default::default(),
			tail: tail.clone(),
			started: None,
			went_live: false,
			finishing: None,
			parked: false,
			starts: Default::default(),
			join_ticks: Default::default(),
			join_budgets: Default::default(),
			declared: None,
			end: None,
			wanted: None,
			unanswered: None,
			_working: crate::session::Work::default().start(),
		})
	}

	/// A group arriving at the relay as its header lands: cached, then accounted for.
	fn arrive(track: &track::Producer, tail: &kio::Producer<Tail>, key: u64, now: crate::time::Instant) {
		let mut group = track.create_group(group::Info { start: key }).unwrap();
		group
			.write_frame(crate::Timestamp::unscaled(group.start), &b"x"[..])
			.unwrap();
		group.finish().unwrap();
		let Ok(mut ledger) = tail.write() else {
			panic!("ledger closed")
		};
		ledger.account(key..key + 1, now);
	}

	/// A fetch held for a pre-lite-05 feed is released once its caller gives up, rather
	/// than waiting, track handle and all, for a group the feed may never reach.
	#[kio::test]
	async fn an_abandoned_feed_fetch_is_released() {
		let start = crate::time::Instant::now();
		let clock = crate::time::Deadlines::new(start);
		let tail = kio::Producer::new(Tail::default());
		let sub = feed_sub(&tail).await;
		let (track, fetching, req) = feed_fetch(100);

		let mut fetches = FeedFetches::new(&clock);
		fetches.push(req, &track.consume());
		fetches.poll(&sub, start, &kio::Waiter::noop());
		assert_eq!(fetches.held.len(), 1, "held while its caller waits");

		drop(fetching);
		fetches.poll(&sub, start, &kio::Waiter::noop());
		assert!(fetches.held.is_empty(), "released once abandoned");
	}

	/// A bounded feed goes live at its answer alone only when its resolved ranges stop short
	/// of Largest. One that serves Largest waits for it, so a finite read judges an old
	/// cached group against that edge rather than against itself.
	#[kio::test]
	async fn a_bounded_feed_goes_live_ahead_only_without_largest() {
		let tail = kio::Producer::new(Tail::default());
		let Sub::Active(mut active) = feed_sub(&tail).await else {
			unreachable!("feed_sub is active")
		};
		let largest = Some(Position::group(10));
		let live = |ranges: &[lite::Range]| lite::LiveRanges::new(Version::Lite07, ranges);
		active.live = live(&[lite::Range::span(Some(0), 0, Some(10), None)]);
		active.started = Some(Some(0));
		active.starts = [(0, Some(0))].into();
		assert_eq!(active.never_serves(largest), None, "group 10 is in 0..11");

		active.live = live(&[lite::Range::span(Some(0), 0, Some(3), None)]);
		assert_eq!(active.never_serves(largest), largest, "0..4 never reaches group 10");

		let later = lite::Range::span(Some(10), 0, Some(12), None);
		active.live = live(&[lite::Range::span(Some(0), 0, Some(3), None), later]);
		active.starts = [(0, Some(0)), (1, Some(10))].into();
		assert_eq!(active.never_serves(largest), None, "a later range serves it");
		active.starts.insert(1, None);
		assert_eq!(active.never_serves(largest), largest, "that range served nothing");
		active.starts.remove(&1);
		assert_eq!(
			active.never_serves(largest),
			largest,
			"an unanswered range is not waited on"
		);

		let open = lite::Range { end: None, ..later };
		active.live = live(&[lite::Range::span(Some(0), 0, Some(3), None), open]);
		assert_eq!(active.never_serves(largest), None, "an open feed reaches it");
		assert_eq!(active.never_serves(None), None, "nothing to go live at");
	}

	/// A demand range added once the live feed started leaves its declared start standing while
	/// unanswered when it can't start below it: from a time at or past it, or at the latest
	/// group. Only one that may start lower makes the start pending again.
	#[kio::test]
	async fn a_range_added_past_the_feed_start_keeps_it_declared() {
		let tail = kio::Producer::new(Tail::default());
		let Sub::Active(mut active) = feed_sub(&tail).await else {
			unreachable!("feed_sub is active")
		};
		let first = lite::Range::span(Some(10), 0, None, None);
		active.live = lite::LiveRanges::new(Version::Lite07, &[first]);
		active.starts = [(0, Some(10))].into();
		active.started = Some(Some(10));
		active.start = Some(10);
		assert_eq!(active.feed_start(), Some((Some(10), false)));

		let latest = lite::Range {
			start: Some(lite::Anchor::Live {
				min: Some(Duration::ZERO),
				max: Some(Duration::ZERO),
			}),
			end: None,
		};
		for added in [lite::Range::span(Some(20), 0, None, None), latest] {
			active.live = lite::LiveRanges::new(Version::Lite07, &[first, added]);
			assert_eq!(active.feed_start(), Some((Some(10), false)), "{added:?}");
		}

		active.live = lite::LiveRanges::new(Version::Lite07, &[first, lite::Range::span(Some(5), 0, None, None)]);
		active.start = Some(5);
		assert_eq!(
			active.feed_start(),
			Some((Some(5), true)),
			"a range from 5 may start below 10"
		);
	}

	/// A lite-07 range answered with no group to deliver lands none of its groups, so the copy's
	/// ordered reads hold no later group back for its time, within the budget it was asked
	/// under. Only one whose end takes in its time says so: one ending before it may still have
	/// skipped the group containing it. A time inside it is no time it was asked from.
	#[kio::test]
	async fn an_empty_answer_releases_only_a_range_reaching_its_time() {
		let tail = kio::Producer::new(Tail::default());
		let Sub::Active(mut active) = feed_sub(&tail).await else {
			unreachable!("feed_sub is active")
		};
		// Times 10 through 20, time 30 ending at group 25, below it, and time 66 onward.
		active.live = lite::LiveRanges::new(
			Version::Lite07,
			&[
				lite::Range {
					start: Some(lite::Anchor::Time(10)),
					end: Some(lite::Anchor::Time(20)),
				},
				lite::Range {
					start: Some(lite::Anchor::Time(30)),
					end: Some(lite::Anchor::Time(24)),
				},
				lite::Range {
					start: Some(lite::Anchor::Time(66)),
					end: None,
				},
			],
		);
		active.starts = [(0, None), (1, None), (2, Some(66))].into();
		let info = track::Info::default().with_timescale(None).with_max_age(None);
		let serving = track::Producer::new(Arc::new(crate::broadcast::Info::default()), "video", info);
		serving.received();
		serving.anchored();
		serving
			.create_group(group::Info { start: 66 })
			.unwrap()
			.finish()
			.unwrap();
		active.report_starts(&serving);

		let read = |time: u64| {
			let mut ordered = serving
				.subscribe(
					track::Subscription::default()
						.with_ranges([track::Range {
							start: crate::track::Start::Time(time),
							end: None,
						}])
						.with_max(None),
				)
				.ordered();
			match ordered.next_item().now_or_never() {
				Some(Ok(Some(track::Item::Group(group)))) => Some(group.start),
				None => None,
				Some(res) => panic!("not a group: {:?}", res.map(|item| item.is_some())),
			}
		};
		assert_eq!(read(10), Some(66), "its range reached 20, past it");
		assert_eq!(read(15), None, "inside that range");
		assert_eq!(read(30), None, "its range ended before it");
	}

	/// An open range answered with no group to deliver takes in every group past its time, so
	/// its answer places a reader there as a bounded range's does. Unrecorded, the reader would
	/// wait for good on a group held past it, and no probe asks for its time while a demand
	/// range starts there.
	#[kio::test]
	async fn an_open_range_answered_empty_places_its_time() {
		let tail = kio::Producer::new(Tail::default());
		let Sub::Active(mut active) = feed_sub(&tail).await else {
			unreachable!("feed_sub is active")
		};
		active.live = lite::LiveRanges::new(
			Version::Lite07,
			&[lite::Range {
				start: Some(lite::Anchor::Time(40)),
				end: None,
			}],
		);
		active.starts = [(0, None)].into();
		let info = track::Info::default().with_timescale(None).with_max_age(None);
		let serving = track::Producer::new(Arc::new(crate::broadcast::Info::default()), "video", info);
		serving.received();
		serving.anchored();
		serving
			.create_group(group::Info { start: 66 })
			.unwrap()
			.finish()
			.unwrap();
		active.report_starts(&serving);

		let mut ordered = serving
			.subscribe(
				track::Subscription::default()
					.with_ranges([track::Range {
						start: crate::track::Start::Time(40),
						end: None,
					}])
					.with_max(None),
			)
			.ordered();
		let read = match ordered.next_item().now_or_never() {
			Some(Ok(Some(track::Item::Group(group)))) => Some(group.start),
			None => None,
			Some(res) => panic!("not a group: {:?}", res.map(|item| item.is_some())),
		};
		assert_eq!(read, Some(66), "no group at or before time 40 is left");
	}

	/// A placed join under a max age may be answered past groups the publisher still holds,
	/// stale for it alone, so its answer places no history reader at its group; one with no
	/// max age skips nothing, so it does, unless it was asked under one before a retune.
	#[kio::test]
	async fn only_an_unbounded_placed_join_answers_for_history() {
		for (max, retuned_from, placed) in [
			(Some(Duration::ZERO), None, None),
			(None, None, Some(20)),
			(None, Some(Duration::ZERO), None),
		] {
			let tail = kio::Producer::new(Tail::default());
			let Sub::Active(mut active) = feed_sub(&tail).await else {
				unreachable!("feed_sub is active")
			};
			active.live = lite::LiveRanges::new(
				Version::Lite07,
				&[lite::Range {
					start: Some(lite::Anchor::Joined {
						at: track::Position { group: 10, frame: 0 },
						max,
					}),
					end: None,
				}],
			);
			active.starts = [(0, Some(20))].into();
			active.join_budgets = retuned_from.map(|asked| (0, asked)).into_iter().collect();
			let info = track::Info::default().with_timescale(None).with_max_age(None);
			let serving = track::Producer::new(Arc::new(crate::broadcast::Info::default()), "video", info);
			serving.received();
			serving.anchored();
			serving
				.create_group(group::Info { start: 20 })
				.unwrap()
				.finish()
				.unwrap();
			active.report_starts(&serving);

			let mut ordered = serving
				.subscribe(
					track::Subscription::default()
						.with_ranges([track::Range {
							start: crate::track::Start::Time(10),
							end: None,
						}])
						.with_max(None),
				)
				.ordered();
			let read = match ordered.next_item().now_or_never() {
				Some(Ok(Some(track::Item::Group(group)))) => Some(group.start),
				None => None,
				Some(res) => panic!("not a group: {:?}", res.map(|item| item.is_some())),
			};
			assert_eq!(read, placed, "max {max:?}, retuned from {retuned_from:?}");
		}
	}

	/// QUIC does not order streams, so group 2's header can reach the relay before group 1's.
	/// A fetch held for group 1 keeps waiting through the grace and is served when it lands.
	#[kio::test]
	async fn a_feed_fetch_waits_out_a_reordered_group() {
		let start = crate::time::Instant::now();
		let clock = crate::time::Deadlines::new(start);
		let tail = kio::Producer::new(Tail::default());
		let sub = feed_sub(&tail).await;
		let (track, fetching, req) = feed_fetch(1);
		arrive(&track, &tail, 0, start);

		let mut fetches = FeedFetches::new(&clock);
		fetches.push(req, &track.consume());
		arrive(&track, &tail, 2, start);
		let late = start.saturating_add(crate::tail::GRACE / 2);
		fetches.poll(&sub, late, &kio::Waiter::noop());
		assert_eq!(fetches.held.len(), 1, "group 2 alone does not give up on group 1");
		assert_eq!(fetches.fold.deadline(), Some(start.saturating_add(crate::tail::GRACE)));

		arrive(&track, &tail, 1, late);
		fetches.poll(&sub, late, &kio::Waiter::noop());
		assert!(fetches.held.is_empty());
		let group = fetching
			.now_or_never()
			.expect("answered")
			.expect("served from the feed");
		assert_eq!(group.start, 1);
	}

	/// A group still missing a grace after a later one arrived is a miss, not a hold.
	#[kio::test]
	async fn a_feed_fetch_misses_once_the_ledger_gives_up() {
		let start = crate::time::Instant::now();
		let clock = crate::time::Deadlines::new(start);
		let tail = kio::Producer::new(Tail::default());
		let sub = feed_sub(&tail).await;
		let (track, fetching, req) = feed_fetch(1);
		arrive(&track, &tail, 0, start);
		arrive(&track, &tail, 2, start);

		let mut fetches = FeedFetches::new(&clock);
		fetches.push(req, &track.consume());
		fetches.poll(&sub, start.saturating_add(crate::tail::GRACE), &kio::Waiter::noop());
		assert!(fetches.held.is_empty());
		assert!(matches!(fetching.now_or_never(), Some(Err(Error::NotFound))));
	}

	/// A floor lowered by an update, or a fresh SUBSCRIBE starting below earlier arrivals,
	/// asks for groups only now, so a fetch held for one waits out a fresh grace rather than
	/// the age of the gap below those arrivals.
	#[kio::test]
	async fn a_lowered_floor_restarts_a_held_fetch_gap() {
		for update in [true, false] {
			let h = Harness::new(Version::Lite04);
			let (mut track, fetching, req) = feed_fetch(3);
			for key in [5, 6] {
				let mut group = track.create_group(group::Info { start: key }).unwrap();
				group
					.write_frame(crate::Timestamp::unscaled(group.start), &b"x"[..])
					.unwrap();
				group.finish().unwrap();
			}
			kio::clock::sleep(crate::tail::GRACE * 2).await;

			let lowered = Some(Subscription::default().with_range(2..));
			// The SUBSCRIBE in force, already answered.
			let floor = if update { 5 } else { 2 };
			let mut sub = Sub::Active(SubStream {
				order: crate::track::Order::Descending,
				stream: Stream::open(&crate::driver::Transport::new(h.session.clone()), Version::Lite04)
					.await
					.unwrap(),
				id: 0,
				max_delay: Some(Duration::ZERO),
				stale_budget: Duration::ZERO,
				stale_cap: None,
				start: Some(floor),
				priority: 0,
				requested: Some(floor),
				live: lite::LiveRanges::new(Version::Lite04, &[lite::Range::span(Some(floor), 0, None, None)]),
				demand: Some(Subscription::default()),
				fill_ids: Default::default(),
				probe_ids: Default::default(),
				fill_groups: Default::default(),
				tail: Default::default(),
				started: None,
				went_live: false,
				finishing: None,
				parked: false,
				starts: Default::default(),
				join_ticks: Default::default(),
				join_budgets: Default::default(),
				declared: None,
				end: None,
				wanted: None,
				unanswered: None,
				_working: crate::session::Work::default().start(),
			});
			if update {
				h.serve
					.handle_subscription(&mut track, &mut sub, lowered, true)
					.await
					.unwrap();
			} else {
				let begin =
					h.serve
						.begin_subscription(&mut track, &mut Sub::None, lowered, &mut RangeFills::default(), true);
				assert!(matches!(begin, Ok(Begin::Establish(_))));
			}

			let now = crate::time::Instant::now();
			let clock = crate::time::Deadlines::new(now);
			let mut fetches = FeedFetches::new(&clock);
			fetches.push(req, &track.consume());
			fetches.poll(&sub, now, &kio::Waiter::noop());
			assert_eq!(
				fetches.held.len(),
				1,
				"group 3 was asked for just now (update: {update})"
			);

			let mut group = track.create_group(group::Info { start: 3 }).unwrap();
			group
				.write_frame(crate::Timestamp::unscaled(group.start), &b"x"[..])
				.unwrap();
			group.finish().unwrap();
			fetches.poll(&sub, now, &kio::Waiter::noop());
			let group = fetching
				.now_or_never()
				.expect("answered")
				.expect("served from the feed");
			assert_eq!(group.start, 3);
		}
	}

	/// Many group fetches on one track share model state, but never share readiness.
	fn feed_batch(
		keys: impl IntoIterator<Item = u64>,
	) -> (
		track::Producer,
		Vec<kio::Pending<track::Fetching>>,
		Vec<track::FillGroups>,
	) {
		let info = track::Info::default().with_timescale(None).with_max_age(None);
		let track = track::Producer::new(Arc::new(crate::broadcast::Info::default()), "video", info);
		// The relay's copy of the upstream feed, whose groups may land out of order.
		track.received();
		let dynamic = track.requests();
		let mut callers = Vec::new();
		let mut requests = Vec::new();
		for key in keys {
			callers.push(track.consume().fetch_group(key, None));
			requests.push(dynamic.requested_fill().now_or_never().expect("queued").unwrap());
		}
		(track, callers, requests)
	}

	/// A held group readied while the poll drains the ready queue, as its fill's waker fires on
	/// another thread, is taken by that poll or wakes its owner: never left queued unseen.
	#[kio::test]
	async fn a_feed_fetch_readied_during_a_drain_is_not_lost() {
		#[derive(Default)]
		struct Wakes(std::sync::atomic::AtomicUsize);
		impl std::task::Wake for Wakes {
			fn wake(self: Arc<Self>) {
				self.0.fetch_add(1, atomic::Ordering::Relaxed);
			}
		}
		let start = crate::time::Instant::now();
		let clock = crate::time::Deadlines::new(start);
		let tail = kio::Producer::new(Tail::default());
		let sub = feed_sub(&tail).await;
		let (track, _callers, requests) = feed_batch([100, 101]);
		let mut fetches = FeedFetches::new(&clock);
		for req in requests {
			fetches.push(req, &track.consume());
		}
		let wakes = Arc::new(Wakes::default());
		let waiter = kio::Waiter::new(std::task::Waker::from(wakes.clone()));
		fetches.poll(&sub, start, &waiter);
		assert_eq!(fetches.held.len(), 2, "both held while their callers wait");

		// Group 100 is readied, and group 101's waker fires just as the drain takes it: parked on
		// the queue, it runs once the drain releases the lock.
		fetches.ready.lock().insert(100);
		let racing = kio::Waiter::new(std::task::Waker::from(Arc::new(FetchWake {
			key: 101,
			ready: fetches.ready.clone(),
		})));
		let _ = fetches.ready.poll(&racing, |_| Poll::<()>::Pending);
		wakes.0.store(0, atomic::Ordering::Relaxed);
		fetches.poll(&sub, start, &waiter);
		let queued = !fetches.ready.read().is_empty();
		assert!(
			!queued || wakes.0.load(atomic::Ordering::Relaxed) > 0,
			"group 101 sits readied with nothing to poll it"
		);
	}

	#[kio::test]
	async fn an_abandoned_feed_fetch_wakes_its_owner_without_an_arrival() {
		#[derive(Default)]
		struct Wakes(std::sync::atomic::AtomicUsize);
		impl std::task::Wake for Wakes {
			fn wake(self: Arc<Self>) {
				self.0.fetch_add(1, atomic::Ordering::Relaxed);
			}
		}
		let start = crate::time::Instant::now();
		let clock = crate::time::Deadlines::new(start);
		let tail = kio::Producer::new(Tail::default());
		let sub = feed_sub(&tail).await;
		let (track, caller, req) = feed_fetch(100);
		let wakes = Arc::new(Wakes::default());
		let waiter = kio::Waiter::new(std::task::Waker::from(wakes.clone()));
		let mut fetches = FeedFetches::new(&clock);
		fetches.push(req, &track.consume());
		fetches.poll(&sub, start, &waiter);
		wakes.0.store(0, atomic::Ordering::Relaxed);
		drop(caller);
		assert!(
			wakes.0.load(atomic::Ordering::Relaxed) > 0,
			"cancellation wakes the ready queue's owner"
		);
		fetches.poll(&sub, start, &waiter);
		assert!(fetches.held.is_empty());
		arrive(&track, &tail, 200, start);
		fetches.poll(&sub, start.saturating_add(crate::tail::GRACE), &waiter);
		wakes.0.store(0, atomic::Ordering::Relaxed);
		fetches.poll(&sub, start.saturating_add(crate::tail::GRACE), &waiter);
		assert_eq!(
			wakes.0.load(atomic::Ordering::Relaxed),
			0,
			"an empty expired gap does not self-wake"
		);
		fetches.poll(&Sub::None, start, &waiter);
		wakes.0.store(0, atomic::Ordering::Relaxed);
		fetches.poll(&Sub::None, start, &waiter);
		assert_eq!(
			wakes.0.load(atomic::Ordering::Relaxed),
			0,
			"an absent feed does not self-wake"
		);
	}

	#[kio::test]
	async fn a_feed_arrival_polls_only_its_group() {
		let start = crate::time::Instant::now();
		let clock = crate::time::Deadlines::new(start);
		let tail = kio::Producer::new(Tail::default());
		let sub = feed_sub(&tail).await;
		let mut fetches = FeedFetches::new(&clock);
		let (track, _callers, requests) = feed_batch(0..32);
		for req in requests {
			fetches.push(req, &track.consume());
		}
		let waiter = kio::Waiter::noop();
		fetches.poll(&sub, start, &waiter);
		let polled = fetches.polled;
		arrive(&track, &tail, 16, start);
		fetches.poll(&sub, start, &waiter);
		assert_eq!(fetches.polled - polled, 1, "an arrival touches its group alone");
		assert_eq!(fetches.held.len(), 31);
		assert_eq!(fetches.fold.deadline(), Some(start.saturating_add(crate::tail::GRACE)));
	}

	#[kio::test]
	async fn a_feed_fold_polls_only_its_expired_gap() {
		let start = crate::time::Instant::now();
		let clock = crate::time::Deadlines::new(start);
		let tail = kio::Producer::new(Tail::default());
		let sub = feed_sub(&tail).await;
		let (track, _callers, requests) = feed_batch([1, 3, 5, 1000]);
		let mut fetches = FeedFetches::new(&clock);
		for req in requests {
			fetches.push(req, &track.consume());
		}
		arrive(&track, &tail, 0, start);
		arrive(&track, &tail, 2, start);
		kio::clock::sleep(crate::tail::GRACE / 2).await;
		arrive(&track, &tail, 4, start.saturating_add(crate::tail::GRACE / 2));
		let waiter = kio::Waiter::noop();
		fetches.poll(&sub, start, &waiter);
		let polled = fetches.polled;
		fetches.poll(&sub, start.saturating_add(crate::tail::GRACE), &waiter);
		assert_eq!(fetches.polled - polled, 1);
		assert!(!fetches.held.contains_key(&1));
		assert_eq!(fetches.held.len(), 3);
		assert_eq!(
			fetches.fold.deadline(),
			Some(
				start
					.saturating_add(crate::tail::GRACE)
					.saturating_add(crate::tail::GRACE / 2)
			)
		);
	}

	#[kio::test]
	async fn feed_boundaries_poll_only_the_groups_they_exclude() {
		let start = crate::time::Instant::now();
		let clock = crate::time::Deadlines::new(start);
		let tail = kio::Producer::new(Tail::default());
		let sub = feed_sub(&tail).await;
		let (mut track, _callers, requests) = feed_batch([1, 3, 5, 1000]);
		let mut fetches = FeedFetches::new(&clock);
		for req in requests {
			fetches.push(req, &track.consume());
		}
		let waiter = kio::Waiter::noop();
		fetches.poll(&sub, start, &waiter);
		let polled = fetches.polled;
		track.start_at(2).unwrap();
		fetches.poll(&sub, start, &waiter);
		assert_eq!(fetches.polled - polled, 1);
		assert!(!fetches.held.contains_key(&1));
		let polled = fetches.polled;
		track.finish_at(Position::group(5)).unwrap();
		fetches.poll(&sub, start, &waiter);
		assert_eq!(fetches.polled - polled, 2);
		assert_eq!(fetches.held.len(), 1);
		assert!(fetches.held.contains_key(&3));
		track.abort(Error::Cancel).unwrap();
		fetches.poll(&sub, start, &waiter);
		assert!(fetches.held.is_empty(), "closure releases all remaining held groups");
	}

	#[kio::test]
	async fn a_feed_drop_polls_only_the_groups_it_excludes() {
		let start = crate::time::Instant::now();
		let clock = crate::time::Deadlines::new(start);
		let tail = kio::Producer::new(Tail::default());
		let sub = feed_sub(&tail).await;
		let (track, mut callers, requests) = feed_batch([1, 3, 5]);
		let mut fetches = FeedFetches::new(&clock);
		for req in requests {
			fetches.push(req, &track.consume());
		}
		let waiter = kio::Waiter::noop();
		fetches.poll(&sub, start, &waiter);
		let polled = fetches.polled;
		track.drop_groups(1..2, crate::StreamError::Old);
		fetches.poll(&sub, start, &waiter);
		assert_eq!(fetches.polled - polled, 1);
		assert!(!fetches.held.contains_key(&1));
		assert_eq!(fetches.held.len(), 2);
		// The held fetch ends with the drop's own error.
		assert!(matches!(
			callers.remove(0).now_or_never(),
			Some(Err(Error::Stream(crate::StreamError::Old)))
		));
	}

	/// A peer that starts its feed far past a reader's floor makes the reader's miss wide.
	/// Holding it settles what the feed gave up by run, in one poll, rather than holding or
	/// refusing its groups one at a time.
	#[kio::test]
	async fn a_wide_feed_fill_settles_by_run() {
		const WIDE: u64 = 1 << 40;
		let start = crate::time::Instant::now();
		let clock = crate::time::Deadlines::new(start);
		let tail = kio::Producer::new(Tail::default());
		let sub = feed_sub(&tail).await;
		let info = track::Info::default().with_timescale(None).with_max_age(None);
		let mut track = track::Producer::new(Arc::new(crate::broadcast::Info::default()), "video", info);
		let dynamic = track.requests();
		let mut reader = track.subscribe(
			Subscription::default()
				.with_range(0..)
				.with_max(Duration::from_secs(30)),
		);
		track.start_at(WIDE).unwrap();
		arrive(&track, &tail, WIDE, start);
		let waiter = kio::Waiter::noop();
		// Reading judges the reader's misses.
		let _ = reader.poll_recv_group(&waiter);
		let fill = dynamic.requested_fill().now_or_never().expect("queued").unwrap();
		assert_eq!((fill.start().group, fill.end()), (0, WIDE));

		let mut fetches = FeedFetches::new(&clock);
		fetches.push(fill, &track.consume());
		fetches.poll(&sub, start, &waiter);
		assert!(fetches.held.is_empty(), "the feed starts past every group of it");
		assert_eq!(fetches.polled, 1);
		assert!(reader.poll_recv_group(&waiter).is_pending());
		assert!(
			dynamic.requested_fill().now_or_never().is_none(),
			"a refusal is not asked again"
		);
	}

	/// A held fill costs one entry, shared by every reader that joins it, until the last leaves.
	#[kio::test]
	async fn held_feed_fills_follow_shared_demand() {
		let start = crate::time::Instant::now();
		let clock = crate::time::Deadlines::new(start);
		let tail = kio::Producer::new(Tail::default());
		let sub = feed_sub(&tail).await;
		let (track, mut callers, requests) = feed_batch([1, 2, 3]);
		let dynamic = track.requests();
		let mut fetches = FeedFetches::new(&clock);
		for req in requests {
			fetches.push(req, &track.consume());
		}
		let waiter = kio::Waiter::noop();
		fetches.poll(&sub, start, &waiter);
		assert_eq!(fetches.held.len(), 3, "one entry per fill");
		assert!(callers.iter_mut().all(|caller| caller.now_or_never().is_none()));

		let mut joined: Vec<_> = (1..=3).map(|key| track.consume().fetch_group(key, None)).collect();
		assert!(
			dynamic.requested_fill().now_or_never().is_none(),
			"joins existing fills"
		);
		drop(callers);
		fetches.poll(&sub, start, &waiter);
		assert_eq!(fetches.held.len(), 3, "the joined readers still own the demand");
		assert!(joined.iter_mut().all(|caller| caller.now_or_never().is_none()));

		drop(joined);
		fetches.poll(&sub, start, &waiter);
		assert!(fetches.held.is_empty(), "the last reader leaving releases every fill");
		assert!(fetches.ready.read().is_empty());
		assert_eq!(fetches.fold.deadline(), None);
		assert!(dynamic.requested_fill().now_or_never().is_none());
	}

	#[kio::test]
	async fn a_feed_fetch_can_replace_an_abandoned_attempt_before_polling() {
		let start = crate::time::Instant::now();
		let clock = crate::time::Deadlines::new(start);
		let tail = kio::Producer::new(Tail::default());
		let sub = feed_sub(&tail).await;
		let (track, caller, req) = feed_fetch(1);
		let dynamic = track.requests();
		let mut fetches = FeedFetches::new(&clock);
		fetches.push(req, &track.consume());
		fetches.poll(&sub, start, &kio::Waiter::noop());
		drop(caller);
		let next = track.consume().fetch_group(1, None);
		let req = dynamic
			.requested_fill()
			.now_or_never()
			.expect("replacement queued")
			.unwrap();
		fetches.push(req, &track.consume());
		fetches.poll(&sub, start, &kio::Waiter::noop());
		assert_eq!(fetches.held.len(), 1);
		arrive(&track, &tail, 1, start);
		fetches.poll(&sub, start, &kio::Waiter::noop());
		assert!(next.now_or_never().unwrap().is_ok());
	}

	/// Sweep held fetches, unrelated tracks, and live arrival runs independently.
	#[kio::test]
	#[ignore = "release benchmark, run by nightly"]
	async fn benchmark_feed_fetch_wakes() {
		struct ScanFetches {
			held: Vec<(track::GroupFill, track::Consumer)>,
			/// Armed at the earliest instant the feed's ledger gives up on a held group, so one
			/// that never arrives still fails.
			fold: crate::time::Deadline,
		}

		impl ScanFetches {
			fn new(clock: &crate::time::Deadlines) -> Self {
				Self {
					held: Vec::new(),
					fold: crate::time::Deadline::new(clock),
				}
			}

			/// Settle the held fetches against `sub`, the feed. A group it will not deliver is a
			/// miss, the same answer as an evicted one: with no feed open, once the feed starts past
			/// it or ends before it, or once its ledger settles it without the group (dropped, or
			/// still missing a grace after a later group arrived). A newer group alone settles
			/// nothing, since QUIC does not order streams. A fetch nobody waits on any more is
			/// dropped, as [`FetchServeRun`] does, so a far-future one isn't held forever.
			fn poll(&mut self, sub: &Sub, now: crate::time::Instant, waiter: &kio::Waiter) {
				let Sub::Active(active) = sub else {
					for (req, _) in self.held.drain(..) {
						req.reject(Error::NotFound);
					}
					self.fold.set(None);
					return;
				};
				let tail = active.tail.read();
				let mut fold: Option<crate::time::Instant> = None;
				for (req, feed) in std::mem::take(&mut self.held) {
					if req.poll_unwanted(waiter).is_ready() {
						continue;
					}
					let key = req.key();
					// The ledger before the cache: a group it accounts for by arrival is cached first.
					let missing = !tail.covers(key..key.saturating_add(1));
					let folds = tail.folds_at(key);
					// Every held key is ahead of the measured arrivals, so the
					// newer-group check is inert. Keep the scanner on the model's global
					// wake channel rather than giving it the new per-group index.
					match feed.poll_group(key, waiter) {
						Poll::Ready(Ok(Some(_))) => drop(req),
						Poll::Ready(Ok(None)) => req.reject(Error::NotFound),
						Poll::Ready(Err(err)) => req.reject(err),
						Poll::Pending if !missing || folds.is_some_and(|at| at <= now) => req.reject(Error::NotFound),
						Poll::Pending => {
							fold = fold.into_iter().chain(folds).min();
							self.held.push((req, feed));
						}
					}
				}
				drop(tail);
				self.fold.set(fold);
				let _ = self.fold.poll(waiter);
			}
		}

		const EVENTS: u64 = 128;
		println!("held,tracks,runs,scan_ns,indexed_ns");
		for held in [1, 64, 1024] {
			for (tracks, runs) in [(1, 1), (8, 1), (32, 1), (1, 64), (1, 1024)] {
				let mut medians = Vec::new();
				for indexed in [false, true] {
					let mut samples = Vec::new();
					for _ in 0..5 {
						let start = crate::time::Instant::now();
						let clock = crate::time::Deadlines::new(start);
						let waiter = kio::Waiter::noop();
						let mut fixtures = Vec::new();
						for _ in 0..tracks {
							// The sim counts ready polls per task poll to catch spinning loops, so
							// give each track's setup, and the timed loop below, a poll of its own.
							kio::clock::sim::yield_now().await;
							let (track, callers, requests) = feed_batch(1_000_000..1_000_000 + held);
							let tail = kio::Producer::new(Tail::default());
							let sub = feed_sub(&tail).await;
							for key in 0..runs {
								arrive(&track, &tail, key * 2, start);
							}
							let mut scan = ScanFetches::new(&clock);
							let mut fetches = FeedFetches::new(&clock);
							for req in requests {
								if indexed {
									fetches.push(req, &track.consume());
								} else {
									scan.held.push((req.group(req.start().group), track.consume()));
								}
							}
							if indexed {
								fetches.poll(&sub, start, &waiter);
							} else {
								scan.poll(&sub, start, &waiter);
							}
							fixtures.push((track, callers, tail, sub, scan, fetches));
						}
						let (track, _callers, tail, sub, scan, fetches) = &mut fixtures[0];
						kio::clock::sim::yield_now().await;
						#[expect(
							clippy::disallowed_methods,
							clippy::disallowed_types,
							reason = "the scan cost is real elapsed time"
						)]
						let began = std::time::Instant::now();
						for key in runs * 2..runs * 2 + EVENTS {
							arrive(track, tail, key, start);
							if indexed {
								fetches.poll(sub, start, &waiter);
							} else {
								scan.poll(sub, start, &waiter);
							}
						}
						#[expect(clippy::disallowed_methods, reason = "the scan cost is real elapsed time")]
						samples.push(began.elapsed().as_nanos() / u128::from(EVENTS));
					}
					samples.sort_unstable();
					medians.push(samples[2]);
				}
				println!("{held},{tracks},{runs},{},{}", medians[0], medians[1]);
			}
		}
	}

	/// A live demand range still unanswered answers the join it asks only while the edit going
	/// out keeps it. One a wider range hides, or a collapse merges away, is retired with its
	/// answer, so the join is asked by a probe of its own rather than left with no ask in flight.
	#[kio::test]
	async fn a_retired_unanswered_join_range_is_asked_again() {
		let session = crate::lite::test_transport::ScriptedSession::new(Vec::new());
		let stream = Stream::open(&crate::driver::Transport::new(session.clone()), VERSION)
			.await
			.unwrap();
		let join = track::JoinAsk {
			min: Some(Duration::from_secs(1)),
			end: None,
			budget: Duration::from_secs(30),
			since: 0,
		};
		let probe = join_probe(&join);
		let sub = SubStream {
			order: crate::track::Order::Ascending,
			stream,
			id: 0,
			max_delay: None,
			stale_budget: Duration::ZERO,
			stale_cap: None,
			start: None,
			priority: 0,
			requested: None,
			live: lite::LiveRanges::new(VERSION, &[probe]),
			demand: Some(Subscription::default()),
			fill_ids: Default::default(),
			probe_ids: Default::default(),
			fill_groups: Default::default(),
			tail: Default::default(),
			started: None,
			went_live: false,
			finishing: None,
			parked: false,
			starts: Default::default(),
			join_ticks: Default::default(),
			join_budgets: Default::default(),
			declared: None,
			end: None,
			wanted: None,
			unanswered: None,
			_working: crate::session::Work::default().start(),
		};

		// Kept, the unanswered demand range still answers the join.
		let (probes, _) = join_probes(Some(&sub), &[join], &[probe]);
		assert_eq!(probes, [], "the kept demand range answers the join");

		// Hidden by an oldest range, it is retired, so the join is asked by a probe.
		let (probes, _) = join_probes(Some(&sub), &[join], &[lite::Range { start: None, end: None }]);
		assert_eq!(probes, [probe], "the retired demand range answers nothing");
	}

	/// A probe answered before its reader began waiting again, under a narrower max than the
	/// reader now waits with, is retired and asked afresh, not retuned in place: a retune keeps
	/// the old answer, which places no reader that began after it, so the reader would wait on
	/// it for good. A lite-06 viewer restating its max as its playout delay settles does this.
	#[kio::test]
	async fn a_probe_answered_before_a_restate_is_asked_afresh() {
		let session = crate::lite::test_transport::ScriptedSession::new(Vec::new());
		let stream = Stream::open(&crate::driver::Transport::new(session.clone()), Version::Lite07)
			.await
			.unwrap();
		let join = |budget, since| track::JoinAsk {
			min: Some(Duration::from_secs(1)),
			end: None,
			budget,
			since,
		};
		let narrow = join(Duration::from_millis(120), 12);
		// The demand range, then the probe asked for the reader at 120 ms, each answered.
		let demand = join_probe(&narrow);
		let mut sub = SubStream {
			order: crate::track::Order::Ascending,
			stream,
			id: 0,
			max_delay: None,
			stale_budget: Duration::ZERO,
			stale_cap: None,
			start: None,
			priority: 0,
			requested: None,
			live: lite::LiveRanges::new(Version::Lite07, &[demand, join_probe(&narrow)]),
			demand: Some(Subscription::default()),
			fill_ids: Default::default(),
			probe_ids: [1].into(),
			fill_groups: Default::default(),
			tail: Default::default(),
			started: None,
			went_live: false,
			finishing: None,
			parked: false,
			starts: [(0, Some(4000)), (1, Some(4000))].into(),
			join_ticks: [(0, 13), (1, 14)].into(),
			join_budgets: [(0, Duration::ZERO), (1, Duration::from_millis(120))].into(),
			declared: None,
			end: None,
			wanted: None,
			unanswered: None,
			_working: crate::session::Work::default().start(),
		};

		// The reader restated at 300 ms after both answers landed.
		let wide = join(Duration::from_millis(300), 15);
		let ranges = [join_probe(&wide)];
		let (probes, reasked) = join_probes(Some(&sub), &[wide], &ranges);
		assert_eq!(probes, [join_probe(&wide)], "neither answer places the restated reader");
		let reasked = fitted_reasks(Some(&sub), reasked, &probes);
		assert_eq!(reasked, [1].into(), "the fresh probe asks the same join");

		let wanted: Vec<_> = ranges.iter().chain(&probes).copied().collect();
		let (edit, _) = sub.edit(&wanted, (probes.len(), 0), &reasked).unwrap();
		let lite::RangeEdit::Edit { remove, add, retune } = edit else {
			panic!("lite-07 edits its ranges");
		};
		assert_eq!(remove, [1], "the stale probe is retired");
		assert_eq!(add, [join_probe(&wide)], "and asked afresh");
		assert_eq!(
			retune,
			[(0, Some(Duration::from_millis(300)))],
			"the demand range is retuned"
		);
	}

	/// A live demand range retuned wider before its answer may still be answered under the max
	/// it was asked with until then, which places no reader waiting with the wider one. So it
	/// answers that reader's join only once nothing narrower was asked of it, and the join goes
	/// out on a probe of its own meanwhile.
	#[kio::test]
	async fn a_join_range_retuned_wider_before_its_answer_is_probed() {
		let session = crate::lite::test_transport::ScriptedSession::new(Vec::new());
		let stream = Stream::open(&crate::driver::Transport::new(session.clone()), Version::Lite07)
			.await
			.unwrap();
		let join = track::JoinAsk {
			min: Some(Duration::from_secs(1)),
			end: None,
			budget: Duration::from_millis(300),
			since: 0,
		};
		// Asked at 120 ms, then retuned to the reader's 300 ms, still unanswered.
		let demand = join_probe(&join);
		let sub = SubStream {
			order: crate::track::Order::Ascending,
			stream,
			id: 0,
			max_delay: None,
			stale_budget: Duration::ZERO,
			stale_cap: None,
			start: None,
			priority: 0,
			requested: None,
			live: lite::LiveRanges::new(Version::Lite07, &[demand]),
			demand: Some(Subscription::default()),
			fill_ids: Default::default(),
			probe_ids: Default::default(),
			fill_groups: Default::default(),
			tail: Default::default(),
			started: None,
			went_live: false,
			finishing: None,
			parked: false,
			starts: Default::default(),
			join_ticks: Default::default(),
			join_budgets: [(0, Duration::from_millis(120))].into(),
			declared: None,
			end: None,
			wanted: None,
			unanswered: None,
			_working: crate::session::Work::default().start(),
		};

		let (probes, reasked) = join_probes(Some(&sub), &[join], &[demand]);
		assert_eq!(
			probes,
			[join_probe(&join)],
			"an answer under 120 ms places no reader at 300 ms"
		);
		assert!(reasked.is_empty());
	}

	/// A flood of time starts the copy's readers wait on leaves room for a live join's probe, so
	/// a reader joining live is not held behind history readers, nor they behind joins.
	#[test]
	fn time_probes_leave_room_for_a_join() {
		let asks: Vec<u64> = (0..10_000).collect();
		let join = track::JoinAsk {
			min: Some(Duration::from_secs(1)),
			end: None,
			budget: Duration::from_millis(300),
			since: 0,
		};
		let (wanted, _) = wanted_probes(None, &asks, &[join], &[]);
		let budget = fit::Budget::new(Version::Lite07, 0, 0, wanted.len());
		let fitted = budget.fit_after(std::iter::empty(), wanted);
		assert!(fitted.len() < asks.len(), "not every time start fits");
		assert!(fitted.contains(&join_probe(&join)), "the join is starved");
	}

	/// The owed groups start at the floor the demand last asked for, which an update can raise
	/// after SUBSCRIBE_START answered the SUBSCRIBE, but not lower below that answer: nothing
	/// says the publisher read the update before its FIN.
	#[kio::test]
	#[expect(
		clippy::single_range_in_vec_init,
		reason = "the owed groups are a list of runs, often one"
	)]
	async fn the_owed_floor_follows_the_demand() {
		let session = crate::lite::test_transport::ScriptedSession::new(Vec::new());
		let stream = Stream::open(&crate::driver::Transport::new(session.clone()), VERSION)
			.await
			.unwrap();
		let mut sub = SubStream {
			order: crate::track::Order::Descending,
			stream,
			id: 0,
			max_delay: Some(Duration::ZERO),
			stale_budget: Duration::ZERO,
			stale_cap: None,
			start: Some(2),
			priority: 0,
			requested: Some(2),
			live: lite::LiveRanges::new(VERSION, &[lite::Range::span(Some(2), 0, None, None)]),
			demand: Some(Subscription::default()),
			fill_ids: Default::default(),
			probe_ids: Default::default(),
			fill_groups: Default::default(),
			tail: Default::default(),
			started: None,
			went_live: false,
			finishing: None,
			parked: false,
			starts: Default::default(),
			join_ticks: Default::default(),
			join_budgets: Default::default(),
			declared: None,
			end: Some(lite::SubscribeEnd { group: 10, error: None }),
			wanted: None,
			unanswered: None,
			_working: crate::session::Work::default().start(),
		};
		assert_eq!(
			sub.owed(None),
			Some(vec![]),
			"without SUBSCRIBE_START nothing was served"
		);

		// An update moves the floor and the ranges together, as `begin_subscription` does.
		let update = |sub: &mut SubStream, start: Option<u64>, ranges: Vec<lite::Range>| {
			sub.start = start;
			sub.edit(&ranges, (0, 0), &Default::default()).unwrap();
		};
		sub.started = Some(None);
		update(&mut sub, Some(1), vec![lite::Range::span(Some(1), 0, None, None)]);
		assert_eq!(
			sub.owed(None),
			Some(vec![1..10]),
			"an update asks from a floor even when SUBSCRIBE_START started no range"
		);

		sub.start = Some(2);
		sub.edit(
			&[
				lite::Range::span(Some(2), 0, Some(3), None),
				lite::Range::span(Some(6), 0, None, None),
			],
			(0, 0),
			&Default::default(),
		)
		.unwrap();
		assert_eq!(
			sub.owed(None),
			Some(vec![2..4, 6..10]),
			"an update that keeps the floor but adds a range still owes it"
		);

		update(&mut sub, Some(2), vec![lite::Range::span(Some(2), 0, None, None)]);
		// A START for the range now live.
		sub.started = Some(Some(4));
		let (id, _) = sub.live.iter().next().unwrap();
		sub.starts = [(id, Some(4))].into();
		assert_eq!(
			sub.owed(None),
			Some(vec![4..10]),
			"the groups below START are accounted for on arrival"
		);
		update(&mut sub, Some(6), vec![lite::Range::span(Some(6), 0, None, None)]);
		assert_eq!(
			sub.owed(None),
			Some(vec![6..10]),
			"a raised floor owes nothing below it"
		);
		update(&mut sub, Some(1), vec![lite::Range::span(Some(1), 0, None, None)]);
		assert_eq!(
			sub.owed(None),
			Some(vec![4..10]),
			"a floor lowered below START owes nothing below it"
		);
		update(&mut sub, None, vec![lite::Range::span(None, 0, None, None)]);
		assert_eq!(
			sub.owed(Some(8)),
			Some(vec![4..8]),
			"a live-edge floor starts where START resolved it"
		);
		update(
			&mut sub,
			Some(4),
			vec![
				lite::Range::span(Some(4), 0, Some(5), None),
				lite::Range::span(Some(7), 0, None, None),
			],
		);
		assert_eq!(
			sub.owed(None),
			Some(vec![4..6, 7..10]),
			"nothing is owed between disjoint ranges"
		);
	}

	/// A fill asking for exactly what a demand range does is still a fill, judged by its own
	/// Range ID, and keeps that ID while it stays held.
	#[kio::test]
	async fn a_lite07_fill_is_known_by_its_range_id() {
		let session = crate::lite::test_transport::ScriptedSession::new(Vec::new());
		let stream = Stream::open(&crate::driver::Transport::new(session.clone()), Version::Lite07)
			.await
			.unwrap();
		// As a fill asks on the wire.
		let groups = |range: std::ops::Range<u64>| lite::Range {
			start: Some(lite::Anchor::Absolute(Position::group(range.start))),
			end: Some(lite::Anchor::Absolute(Position::group(range.end))),
		};
		let demand = groups(4..8);
		let mut sub = SubStream {
			order: crate::track::Order::Descending,
			stream,
			id: 0,
			max_delay: Some(Duration::ZERO),
			stale_budget: Duration::ZERO,
			stale_cap: None,
			start: Some(4),
			priority: 0,
			requested: Some(4),
			live: lite::LiveRanges::new(Version::Lite07, &[demand]),
			demand: Some(Subscription::default()),
			fill_ids: Default::default(),
			probe_ids: Default::default(),
			fill_groups: Default::default(),
			tail: Default::default(),
			started: Some(Some(4)),
			went_live: false,
			finishing: None,
			parked: false,
			starts: [(0, Some(4))].into(),
			join_ticks: Default::default(),
			join_budgets: Default::default(),
			declared: None,
			end: None,
			wanted: None,
			unanswered: None,
			_working: crate::session::Work::default().start(),
		};
		assert_eq!(sub.fill_range(0), None, "the demand's range is no fill");

		// The same bounds again, as a fill.
		sub.edit(&[demand, demand], (0, 1), &Default::default()).unwrap();
		assert_eq!(sub.fill_range(0), None, "the demand keeps its range");
		assert_eq!(sub.fill_range(1), Some(4..8), "the copy the edit added is the fill");

		// Another fill joins, and the first keeps its Range ID.
		let other = groups(10..12);
		sub.edit(&[demand, demand, other], (0, 2), &Default::default()).unwrap();
		assert_eq!(sub.fill_ids.iter().copied().collect::<Vec<_>>(), [1, 2]);

		// The demand moves away while the fill stays: the fill keeps Range ID 1.
		let moved = lite::Range::span(Some(20), 0, None, None);
		sub.edit(&[moved, demand, other], (0, 2), &Default::default()).unwrap();
		assert_eq!(sub.fill_ids.iter().copied().collect::<Vec<_>>(), [1, 2]);
		assert_eq!(sub.fill_range(1), Some(4..8));
		assert_eq!(sub.fill_range(3), None, "the demand's new range is no fill");

		// Before SUBSCRIBE_START, a fill an update retires keeps its Range ID, since the start
		// still names it, and it must not count as the demand's.
		sub.started = None;
		sub.edit(&[moved, demand], (0, 1), &Default::default()).unwrap();
		assert_eq!(sub.fill_range(2), None, "retired");
		assert!(sub.fill_ids.contains(&2), "still a fill's");
		sub.started = Some(Some(4));
		sub.edit(&[moved, demand], (0, 1), &Default::default()).unwrap();
		assert!(!sub.fill_ids.contains(&2), "forgotten once started");
	}

	/// A lite-07 range an update adds owes nothing known until SUBSCRIBE_START says where it
	/// starts, and then owes from there; one retired since owes nothing.
	#[kio::test]
	#[expect(
		clippy::single_range_in_vec_init,
		reason = "the owed groups are a list of runs, often one"
	)]
	async fn a_lite07_added_range_is_owed_from_its_reported_start() {
		let session = crate::lite::test_transport::ScriptedSession::new(Vec::new());
		let stream = Stream::open(&crate::driver::Transport::new(session.clone()), Version::Lite07)
			.await
			.unwrap();
		let first = lite::Range::span(Some(4), 0, None, None);
		let mut sub = SubStream {
			order: crate::track::Order::Descending,
			stream,
			id: 0,
			max_delay: Some(Duration::ZERO),
			stale_budget: Duration::ZERO,
			stale_cap: None,
			start: Some(4),
			priority: 0,
			requested: Some(4),
			live: lite::LiveRanges::new(Version::Lite07, &[first]),
			demand: Some(Subscription::default()),
			fill_ids: Default::default(),
			probe_ids: Default::default(),
			fill_groups: Default::default(),
			tail: Default::default(),
			started: Some(Some(4)),
			went_live: false,
			finishing: None,
			parked: false,
			starts: [(0, Some(4))].into(),
			join_ticks: Default::default(),
			join_budgets: Default::default(),
			declared: None,
			end: Some(lite::SubscribeEnd { group: 10, error: None }),
			wanted: None,
			unanswered: None,
			_working: crate::session::Work::default().start(),
		};
		assert_eq!(sub.owed(None), Some(vec![4..10]));

		// A live join, which only the publisher can place.
		let back = lite::Range {
			start: Some(lite::Anchor::Live {
				min: Some(Duration::ZERO),
				max: None,
			}),
			end: None,
		};
		let (edit, changed) = sub.edit(&[first, back], (0, 0), &Default::default()).unwrap();
		assert!(changed);
		assert_eq!(
			edit,
			lite::RangeEdit::Edit {
				remove: vec![],
				add: vec![back],
				retune: Vec::new(),
			}
		);
		assert_eq!(sub.owed(None), None, "nothing says yet where it starts");
		sub.starts.insert(1, Some(1));
		assert_eq!(sub.owed(None), Some(vec![4..10, 1..10]));

		// Retiring the first range forgets its start.
		sub.edit(&[back], (0, 0), &Default::default()).unwrap();
		assert_eq!(sub.starts.keys().copied().collect::<Vec<_>>(), [1]);
		assert_eq!(sub.owed(Some(8)), Some(vec![1..8]));

		// A join asked afresh is placed again, as a range of its own.
		let (edit, changed) = sub.edit(&[back], (0, 0), &[1].into()).unwrap();
		assert!(changed);
		assert_eq!(
			edit,
			lite::RangeEdit::Edit {
				remove: vec![1],
				add: vec![back],
				retune: Vec::new(),
			}
		);
		assert!(sub.starts.is_empty(), "the new range waits on its own start");
	}

	/// A peer opening streams at once gets a budget of them accepted per poll, leaving the
	/// rest for later so the session's other tasks run in between.
	#[kio::test]
	async fn a_flood_of_uni_streams_yields() {
		use crate::transport::poll::Session as _;

		let (local, peer) = crate::transport::memory::pair(Default::default());
		let subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(local.clone()),
			origin: origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce(),
			recv_bandwidth: None,
			version: Version::Lite07,
			peer_setup: lite::PeerSetup::new(false, crate::transport::Address::Setup),
			peer_node: None,
			cost: None,
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});
		let mut accept = UniAccept::new(subscriber, Vec::new());
		// Held open and empty, so each accepted stream waits on its type.
		let mut opened = Vec::new();
		for _ in 0..1024 {
			opened.push(peer.open_uni(Default::default()).await.unwrap());
		}

		assert!(accept.poll(&kio::Waiter::noop()).is_pending());
		assert!(
			local.poll_accept_uni(&kio::Waiter::noop()).is_ready(),
			"one poll accepted every stream"
		);
	}

	/// A peer keeping datagrams ready gets a budget of them routed per poll, leaving the
	/// rest for later so the session's other tasks run in between.
	#[kio::test]
	async fn a_flood_of_datagrams_yields() {
		use crate::transport::poll::Session as _;

		let (local, peer) = crate::transport::memory::pair(Default::default());
		let subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(local.clone()),
			origin: origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce(),
			recv_bandwidth: None,
			version: Version::Lite07,
			peer_setup: lite::PeerSetup::new(false, crate::transport::Address::Setup),
			peer_node: None,
			cost: None,
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});
		let mut recv = DatagramRecv::new(subscriber);
		assert!(recv.enabled);
		// Each names no subscription, so it is dropped once routed.
		for _ in 0..1024 {
			peer.send_datagram(bytes::Bytes::from_static(b"x"), Default::default())
				.unwrap();
		}

		assert!(recv.poll(&kio::Waiter::noop(), false).is_pending());
		let other = local.clone();
		assert!(
			other.poll_recv_datagram(&kio::Waiter::noop()).is_ready(),
			"one poll routed every datagram"
		);
	}

	/// Before the sources run, a datagram whose track isn't known yet is held rather than
	/// dropped, while known tracks' datagrams go on, and the pass after them routes it to a
	/// track registered in between. One that still finds no track is dropped and counted.
	#[kio::test]
	async fn a_datagram_waits_for_the_sources_before_it_is_dropped() {
		use crate::transport::poll::Session as _;

		let (local, peer) = crate::transport::memory::pair(Default::default());
		let subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(local),
			origin: origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce(),
			recv_bandwidth: None,
			version: Version::Lite07,
			peer_setup: lite::PeerSetup::new(false, crate::transport::Address::Setup),
			peer_node: None,
			cost: None,
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});
		let mut recv = DatagramRecv::new(subscriber.clone());
		let send = |subscribe| {
			let body = lite::Datagram {
				subscribe,
				sequence: 0,
				timestamp: 0,
				payload: bytes::Bytes::from_static(b"x"),
			};
			peer.send_datagram(body.encode_bytes(Version::Lite07).unwrap(), Default::default())
				.unwrap();
		};
		let unrouted = || subscriber.datagrams_unrouted.load(atomic::Ordering::Relaxed);
		let waiter = kio::Waiter::noop();

		send(1);
		assert!(recv.poll(&waiter, true).is_pending());
		assert_eq!(recv.held.len(), 1, "dropped before the sources ran");
		let producer = datagram::Info::default().produce("datagrams");
		let mut received = producer.consume();
		subscriber.datagrams.lock().insert(
			1,
			DatagramEntry {
				producer,
				timescale: None,
			},
		);
		assert!(recv.poll(&waiter, false).is_pending());
		assert!(matches!(received.poll_recv(&waiter), Poll::Ready(Ok(Some(_)))));
		assert_eq!(unrouted(), 0);

		// A held datagram doesn't hold up a known track's behind it, which an end the sources
		// read could otherwise drop.
		send(2);
		send(1);
		assert!(recv.poll(&waiter, true).is_pending());
		assert!(matches!(received.poll_recv(&waiter), Poll::Ready(Ok(Some(_)))));
		assert_eq!(recv.held.len(), 1);
		assert!(recv.poll(&waiter, false).is_pending());
		assert!(recv.held.is_empty());
		assert_eq!(unrouted(), 1);
	}

	/// A track's end waits for the datagrams queued with it, though they outlast a driver
	/// poll's budget, and applies once a pass reads the queue empty.
	#[kio::test]
	async fn an_end_waits_for_the_datagrams_queued_with_it() {
		use crate::transport::poll::Session as _;

		const QUEUED: u64 = 100;
		let (local, peer) = crate::transport::memory::pair(Default::default());
		let subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(local),
			origin: origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce(),
			recv_bandwidth: None,
			version: Version::Lite07,
			peer_setup: lite::PeerSetup::new(false, crate::transport::Address::Setup),
			peer_node: None,
			cost: None,
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});
		let mut driver = SubscriberDriver::new(subscriber.clone(), Vec::new(), false, route_stream::Publish::idle());
		let producer = datagram::Info::default().produce("datagrams");
		let mut received = producer.consume();
		subscriber.datagrams.lock().insert(
			1,
			DatagramEntry {
				producer: producer.clone(),
				timescale: None,
			},
		);
		for key in 0..QUEUED {
			let body = lite::Datagram {
				subscribe: 1,
				sequence: key,
				timestamp: key,
				payload: bytes::Bytes::from_static(b"x"),
			};
			peer.send_datagram(body.encode_bytes(Version::Lite07).unwrap(), Default::default())
				.unwrap();
		}
		// As the sources queue an end they read.
		subscriber.datagram_ends.lock().tracks.push((1, producer));

		let waiter = kio::Waiter::noop();
		let mut polls = 0;
		let mut count = 0;
		while !subscriber.datagram_ends.lock().tracks.is_empty() {
			assert!(driver.poll(&waiter).is_pending());
			polls += 1;
			assert!(polls < 16, "the end never applied");
			// The track keeps only so many unread.
			while let Poll::Ready(Ok(Some(_))) = received.poll_recv(&waiter) {
				count += 1;
			}
		}
		assert!(polls > 1, "one poll read the whole queue");
		assert!(matches!(received.poll_recv(&waiter), Poll::Ready(Ok(None))));
		assert_eq!(count, QUEUED);
		assert_eq!(subscriber.datagrams_unrouted.load(atomic::Ordering::Relaxed), 0);
	}

	/// Poll `running` as the driver does, which applies the datagram ends queued in its pass.
	fn poll_run(running: &mut TrackServeRun, waiter: &kio::Waiter) -> Poll<()> {
		let res = kio::Task::poll(running, waiter);
		running.serve.subscriber.end_datagrams();
		match running.state {
			TrackRunState::Ending { .. } => kio::Task::poll(running, waiter),
			_ => res,
		}
	}

	/// A stream type the version doesn't define, such as lite-07's removed padding (0x2), is
	/// stopped without ending the session.
	#[kio::test]
	async fn an_unknown_stream_type_is_stopped() {
		for version in [
			Version::Lite01,
			Version::Lite02,
			Version::Lite03,
			Version::Lite04,
			Version::Lite05,
			Version::Lite06,
			Version::Lite07,
		] {
			let mut bytes = Vec::new();
			crate::coding::Encoder::new(&mut bytes, version.into())
				.varint(2)
				.unwrap();
			let session = crate::lite::test_transport::ScriptedSession::new(bytes);
			let stream = Stream::open(&crate::driver::Transport::new(session.clone()), version)
				.await
				.unwrap();
			let log = session.log.clone();
			let subscriber = Subscriber::new(SubscriberConfig {
				runtime: crate::time::sim(),
				session: crate::driver::Transport::new(session.clone()),
				origin: origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce(),
				recv_bandwidth: None,
				version,
				peer_setup: lite::PeerSetup::new(false, crate::transport::Address::Setup),
				peer_node: None,
				cost: None,
				going_away: Default::default(),
				auth: crate::auth::Link::new(false),
			});
			let mut uni = UniServe {
				subscriber,
				state: UniState::Start { reader: stream.reader },
			};
			assert!(uni.poll_serve(&kio::Waiter::noop()).is_ready());
			drop(uni);
			assert!(!log.stops().is_empty(), "{version}: the stream was not stopped");
			assert!(log.closes().is_empty(), "{version}: the session closed");
		}
	}

	/// A GROUP needs no negotiated extension, so it proceeds before SETUP, or after a
	/// server's gated accept held it.
	#[kio::test]
	async fn early_group_is_delivered() {
		for version in [Version::Lite05, Version::Lite06, Version::Lite07] {
			for pre_read in [false, true] {
				let mut bytes = Vec::new();
				lite::DataType::Group
					.encode(&mut crate::coding::Encoder::new(&mut bytes, version.into()), version)
					.unwrap();
				lite::Group {
					subscribe: 7,
					key: 3,
					frame_start: 0,
				}
				.encode(&mut crate::coding::Encoder::new(&mut bytes, version.into()), version)
				.unwrap();
				let mut w = crate::coding::Encoder::new(&mut bytes, version.into());
				w.varint(0).unwrap();
				w.varint(1).unwrap();
				w.u8(42);
				let mut setup = Vec::new();
				lite::DataType::Setup
					.encode(&mut crate::coding::Encoder::new(&mut setup, version.into()), version)
					.unwrap();
				lite::Setup::default()
					.encode(&mut crate::coding::Encoder::new(&mut setup, version.into()), version)
					.unwrap();
				let session =
					crate::lite::test_transport::ScriptedSession::eof(Vec::new()).with_incoming_unis(if pre_read {
						vec![Vec::new(), vec![42], bytes, setup]
					} else {
						vec![bytes]
					});
				let log = session.log.clone();
				let peer_setup = lite::PeerSetup::new(false, crate::transport::Address::Setup);
				let early = if pre_read {
					let accepted = lite::accept_setup(&crate::driver::Transport::new(session.clone()), version)
						.await
						.unwrap();
					assert_eq!(accepted.early.len(), 2);
					assert!(log.stops().is_empty());
					peer_setup.set(accepted.setup).unwrap();
					accepted.early
				} else {
					Vec::new()
				};
				let subscriber = Subscriber::new(SubscriberConfig {
					runtime: crate::time::sim(),
					session: crate::driver::Transport::new(session.clone()),
					origin: origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce(),
					recv_bandwidth: None,
					version,
					peer_setup: peer_setup.clone(),
					peer_node: None,
					cost: None,
					going_away: Default::default(),
					auth: crate::auth::Link::new(false),
				});
				let track = track::Producer::new(
					std::sync::Arc::new(crate::broadcast::Info::default()),
					"video",
					crate::track::Info::timed(),
				);
				let mut consumer = track.consume().subscribe(None).await.unwrap();
				subscriber.subscribes.lock().insert(
					7,
					TrackEntry::accepted(
						track,
						lite::Framing {
							timescale: Some(Timescale::MILLI),
							ietf: None,
							compressed: false,
							length_first: false,
						},
						Default::default(),
					),
				);
				let mut accept = UniAccept::new(subscriber, early);
				assert!(accept.poll(&kio::Waiter::noop()).is_pending());
				// Only the early stream of an unknown type is stopped.
				assert_eq!(log.stops().len(), usize::from(pre_read));
				let mut group = consumer.recv_group().await.unwrap().unwrap();
				assert_eq!(group.start, 3);
				let frame = group.read_frame().await.unwrap().unwrap();
				assert_eq!(frame.payload.as_ref(), &[42]);
				assert!(group.read_frame().await.unwrap().is_none());
			}
		}
	}

	/// Serve one Group Stream for `key` on subscription 7, accepted into `track`.
	async fn serve_group(version: Version, key: u64, track: &track::Producer) -> Result<(), Error> {
		use crate::transport::poll::Session as _;

		let mut script = Vec::new();
		lite::Group {
			subscribe: 7,
			key,
			frame_start: 0,
		}
		.encode(&mut crate::coding::Encoder::new(&mut script, version.into()), version)
		.unwrap();
		let session = crate::lite::test_transport::ScriptedSession::eof(script);
		let subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(session.clone()),
			origin: origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce(),
			recv_bandwidth: None,
			version,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			peer_node: None,
			cost: None,
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});
		subscriber.subscribes.lock().insert(
			7,
			TrackEntry::accepted(
				track.clone(),
				lite::Framing {
					timescale: Some(Timescale::MILLI),
					ietf: None,
					compressed: false,
					length_first: false,
				},
				Default::default(),
			),
		);

		let (_, recv) = session.open_bi(Default::default()).await.unwrap();
		let mut group = GroupRecv::new(
			subscriber,
			Reader::new(crate::transport::boxed::RecvStream::new(recv), version),
		);
		kio::wait(|waiter| group.poll_serve(waiter)).await
	}

	/// A group at or past the end the publisher declared contradicts that end, which no later
	/// stream can repair, so the whole track fails rather than ending clean without it. lite-05
	/// specified an inclusive end, so there it costs only that group's stream.
	#[kio::test]
	async fn a_group_past_the_declared_end_aborts_the_track() {
		for version in [Version::Lite05, Version::Lite06, Version::Lite07] {
			let mut track = track::Producer::new(
				std::sync::Arc::new(crate::broadcast::Info::default()),
				"video",
				crate::track::Info::timed(),
			);
			track.finish_at(Position::group(3)).unwrap();
			let res = serve_group(version, 3, &track).await;
			match version {
				Version::Lite05 => {
					assert!(matches!(res, Err(Error::Closed)), "{version:?}: {res:?}");
					assert!(
						track.closed().now_or_never().is_none(),
						"{version:?}: the track lives on"
					);
				}
				_ => {
					assert!(matches!(res, Err(Error::ProtocolViolation)), "{version:?}: {res:?}");
					assert!(matches!(
						track.closed().now_or_never(),
						Some(Err(Error::ProtocolViolation))
					));
				}
			}
		}
	}

	/// Group `u64::MAX` is reserved, so a stream naming it is refused, and the track lives on.
	#[kio::test]
	async fn a_group_at_the_reserved_key_is_refused() {
		let track = track::Producer::new(
			std::sync::Arc::new(crate::broadcast::Info::default()),
			"video",
			crate::track::Info::timed(),
		);
		let res = serve_group(Version::Lite07, u64::MAX, &track).await;
		assert!(matches!(res, Err(Error::BoundsExceeded(_))), "{res:?}");
		assert!(track.closed().now_or_never().is_none());
		assert_eq!(track.latest_key(), None);
	}

	/// A track in `timescale` that a draft-18 moq-transport publisher published.
	fn draft18(timescale: Option<Timescale>) -> lite::Framing {
		lite::Framing {
			timescale,
			ietf: Some(crate::ietf::Version::Draft18),
			compressed: false,
			length_first: false,
		}
	}

	/// Receive group 0 of a track `framing` describes over a lite-07 session that negotiated
	/// the `ietf` extension, its stream holding `body` after the GROUP header. Also returns
	/// the cache pool the track charges.
	async fn receive_ietf(
		framing: lite::Framing,
		body: &[u64],
	) -> (Result<(), Error>, track::Producer, crate::cache::Pool) {
		let header = lite::Group {
			subscribe: 7,
			key: 0,
			frame_start: 0,
		};
		receive_lite07(framing, true, header, body).await
	}

	/// Receive the group `header` opens on a track `framing` describes over a lite-07
	/// session, which negotiated the `ietf` extension when `ietf`, its stream holding `body`
	/// after the header. Also returns the cache pool the track charges.
	async fn receive_lite07(
		framing: lite::Framing,
		ietf: bool,
		header: lite::Group,
		body: &[u64],
	) -> (Result<(), Error>, track::Producer, crate::cache::Pool) {
		use crate::transport::poll::Session as _;

		let version = Version::Lite07;
		let mut script = Vec::new();
		let mut encoder = crate::coding::Encoder::new(&mut script, version.into());
		header.encode(&mut encoder, version).unwrap();
		for value in body {
			encoder.varint(*value).unwrap();
		}
		let session = crate::lite::test_transport::ScriptedSession::eof(script);
		let peer_setup = crate::lite::PeerSetup::new(false, crate::transport::Address::Setup).offer_ietf(ietf);
		peer_setup
			.set(lite::Setup {
				ietf,
				..Default::default()
			})
			.unwrap();
		let subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(session.clone()),
			origin: origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce(),
			recv_bandwidth: None,
			version,
			peer_setup,
			peer_node: None,
			cost: None,
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});
		let pool = crate::cache::Pool::unbounded();
		let mut info = crate::track::Info::default().with_timescale(framing.timescale);
		info.properties = framing.ietf.map(|version| {
			std::sync::Arc::new(crate::model::object::TrackProperties {
				version,
				values: Vec::new(),
			})
		});
		let track = track::Producer::new(
			std::sync::Arc::new(crate::broadcast::Info {
				pool: pool.clone(),
				..Default::default()
			}),
			"video",
			info,
		);
		subscriber
			.subscribes
			.lock()
			.insert(7, TrackEntry::accepted(track.clone(), framing, Default::default()));

		let (_, recv) = session.open_bi(Default::default()).await.unwrap();
		let mut group = GroupRecv::new(
			subscriber,
			Reader::new(crate::transport::boxed::RecvStream::new(recv), version),
		);
		let res = kio::wait(|waiter| group.poll_serve(waiter)).await;
		(res, track, pool)
	}

	/// A lite-07 track with no extension, in `timescale`.
	fn lite07(timescale: Option<Timescale>) -> lite::Framing {
		lite::Framing {
			timescale,
			ietf: None,
			compressed: false,
			length_first: true,
		}
	}

	/// Group `key`'s header on subscription 7.
	fn lite07_header(key: u64) -> lite::Group {
		lite::Group {
			subscribe: 7,
			key,
			frame_start: 0,
		}
	}

	/// GROUP_END finishes the group whole at its End, and a bare FIN closes it with every frame
	/// sent but no end: each frame is its `Payload Length`, a timestamp delta from the group's
	/// start, and the payload.
	#[kio::test]
	async fn group_end_finishes_and_a_bare_fin_closes() {
		let x = u64::from(b'x');
		for (body, end) in [(&[2, 0, x, 0, 5][..], Some(5)), (&[2, 0, x][..], None)] {
			let (res, track, _) = receive_lite07(lite07(Some(Timescale::MILLI)), false, lite07_header(0), body).await;
			res.unwrap();
			let mut group = track.consume().peek_group(0).unwrap();
			assert_eq!(
				group.read_frame().now_or_never().unwrap().unwrap().unwrap().payload,
				b"x"[..]
			);
			assert!(group.read_frame().now_or_never().unwrap().unwrap().is_none());
			assert_eq!(group.end(), end, "{body:?}");
		}
	}

	/// Anything after GROUP_END, an End past 64 bits, or a zero length under object records,
	/// where a record would come first, is refused.
	#[kio::test]
	async fn group_end_refuses_what_cannot_end_a_group() {
		let x = u64::from(b'x');
		for (key, body, reason) in [
			(0, &[2, 0, x, 0, 5, 0][..], "a byte after GROUP_END"),
			(0, &[2, 0, x, 0, 5, 2, 0, x], "a frame after GROUP_END"),
			(5, &[2, 4, x, 0, u64::MAX], "an End past 64 bits"),
			(0, &[0], "a stream ending inside GROUP_END"),
		] {
			let (res, track, _) = receive_lite07(lite07(Some(Timescale::MILLI)), false, lite07_header(key), body).await;
			assert!(res.is_err(), "accepted {reason}");
			if let Some(mut group) = track.consume().peek_group(key) {
				while let Some(Ok(Some(_))) = group.read_frame().now_or_never() {}
				assert!(group.end().is_none(), "{reason}: the group finished");
			}
		}
		let (res, ..) = receive_ietf(draft18(None), &[0, 0]).await;
		assert!(res.is_err(), "accepted GROUP_END under object records");
	}

	/// A lite-07 FRAME on a scale-less track carries no timestamp: its time is its group's
	/// start, and GROUP_END counts from that start.
	#[kio::test]
	async fn a_scale_less_frame_takes_its_groups_start() {
		let x = u64::from(b'x');
		let (res, track, _) = receive_lite07(lite07(None), false, lite07_header(9), &[2, x, 2, x, 0, 3]).await;
		res.unwrap();
		let mut group = track.consume().peek_group(9).unwrap();
		for _ in 0..2 {
			let frame = group.read_frame().now_or_never().unwrap().unwrap().unwrap();
			assert_eq!(frame.timestamp, Timestamp::unscaled(9));
		}
		assert_eq!(group.end(), Some(12));
	}

	/// The first FRAME counts from the group's start, later ones from the frame before, and
	/// GROUP_END from the highest frame, not the last, since B-frames go back in time. The
	/// whole group's span holds no other start, so a reader waits for none there.
	#[kio::test]
	async fn group_end_counts_from_the_highest_frame() {
		let x = u64::from(b'x');
		let zigzag = crate::coding::varint::zigzag;
		// Frames at 10, 12 and 10, then an End 3 past 12.
		let body = [2, zigzag(0), x, 2, zigzag(2), x, 2, zigzag(-2), x, 0, 3];
		let (res, track, _) = receive_lite07(lite07(Some(Timescale::MILLI)), false, lite07_header(10), &body).await;
		res.unwrap();
		let mut group = track.consume().peek_group(10).unwrap();
		for millis in [10, 12, 10] {
			let frame = group.read_frame().now_or_never().unwrap().unwrap().unwrap();
			assert_eq!(frame.timestamp, Timestamp::from_millis(millis).unwrap());
		}
		assert_eq!(group.end(), Some(15));
		let consumer = track.subscribe(None);
		assert!(consumer.is_skipped(11..15), "no group starts inside the span");
		assert!(!consumer.is_skipped(15..16), "the next group may start at the end");
	}

	/// Object records: `(Skip << 1) | END`, the properties block's length, then the frame.
	#[kio::test]
	async fn ietf_records_frame_a_group() {
		// A plain group: Group State 0, then frame 0 with an empty record.
		let (res, track, _) = receive_ietf(draft18(None), &[0, 0, 0, 1, u64::from(b'x')]).await;
		res.unwrap();
		let mut group = track.consume().peek_group(0).unwrap();
		assert_eq!(
			group.read_frame().now_or_never().unwrap().unwrap().unwrap().payload,
			b"x"[..]
		);

		// A relayed group starting at Object 4, then END_OF_GROUP at Object 6.
		let (res, track, _) = receive_ietf(draft18(None), &[0x1, 4 << 1, 0, 1, u64::from(b'x'), (1 << 1) | 1, 0]).await;
		res.unwrap();
		let group = track.consume().peek_group(0).unwrap();
		assert_eq!(group.end_object().map(|end| end.id), Some(6));
		assert_eq!(group.wire().map(|wire| wire.first_id), Some(4));
	}

	/// A record the Group State does not allow, or bytes past END_OF_GROUP, are refused.
	#[kio::test]
	async fn ietf_records_refuse_what_their_group_cannot_hold() {
		for (body, reason) in [
			(&[0x20][..], "an unknown Group State bit"),
			(&[0x2][..], "relay state without RELAYED"),
			(&[0, 1 << 1, 0, 1, 0], "a plain group skipping an Object ID"),
			(&[0, 1, 0], "a plain group with END_OF_GROUP"),
			(&[0, 0, 2, 0x10, 0], "a plain group with properties"),
			(&[0x1, 1, 0, 0], "a byte after END_OF_GROUP"),
			(&[0x1, 0, 0], "a stream ending inside a frame"),
		] {
			let (res, ..) = receive_ietf(draft18(None), body).await;
			assert!(res.is_err(), "accepted {reason}");
		}
	}

	/// A track published over moq-lite has no moq-transport state, so its Group Streams
	/// carry no Group State and no records, as on a session without the extension.
	#[kio::test]
	async fn a_moq_lite_track_has_no_records() {
		let (res, track, _) = receive_ietf(lite::Framing::default(), &[1, u64::from(b'x')]).await;
		res.unwrap();
		let mut group = track.consume().peek_group(0).unwrap();
		let frame = group.read_frame().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(frame.payload, b"x"[..]);
		assert!(group.read_frame().now_or_never().unwrap().unwrap().is_none());
		assert!(group.wire().is_none());
	}

	/// A relayed group's records carry properties only when its Group State sets EXTENSIONS,
	/// as the moq-transport subgroup it mirrors could hold none otherwise.
	#[kio::test]
	async fn ietf_records_need_extensions_for_properties() {
		// Object 0 with a draft-18 block holding 0x20 = 1, or a Timestamp.
		for record in [&[2, 0x20, 1][..], &[2, 0x10, 7]] {
			for (state, ok) in [(0x1 | 0x4, true), (0x1, false)] {
				let mut body = vec![state, 0];
				body.extend(record);
				body.push(0);
				let (res, ..) = receive_ietf(draft18(None), &body).await;
				assert_eq!(res.is_ok(), ok, "Group State {state:#x}, record {record:?}");
			}
		}
	}

	/// A record's Timestamp, mutable or sealed, is a property like any other: nothing reads
	/// time from it, so the frame keeps the time its own header carries.
	#[kio::test]
	async fn ietf_records_leave_time_to_their_frame() {
		let frame = crate::coding::varint::zigzag(500);
		// Each record after its Object code: Object 0 unless it says otherwise.
		for (record, ok, reason) in [
			(&[0, 0][..], true, "nothing at all"),
			(&[0, 3, 0x10, 500], true, "a mutable Timestamp"),
			(&[0, 2, 0x20, 1], true, "properties without a Timestamp"),
			(&[0, 5, 0x0b, 3, 0x10, 499], true, "a sealed Timestamp"),
			(&[1 << 1, 0], true, "Object 1 without properties"),
		] {
			let mut body = vec![0x1 | 0x4];
			body.extend(record);
			body.extend([frame, 0]);
			let (res, track, _) = receive_ietf(draft18(Some(Timescale::MILLI)), &body).await;
			assert_eq!(res.is_ok(), ok, "{reason}: {res:?}");
			if ok {
				let mut group = track.consume().peek_group(0).unwrap();
				let frame = group.read_frame().now_or_never().unwrap().unwrap().unwrap();
				assert_eq!(frame.timestamp, Timestamp::from_millis(500).unwrap(), "{reason}");
			}
		}
	}

	/// A record's properties stay the bytes they arrived as, which the cache charges, rather
	/// than a decoded list costing many times as much.
	#[kio::test]
	async fn ietf_records_hold_properties_at_their_wire_size() {
		const COUNT: u64 = 1000;
		// A relayed group's Object 0, whose draft-18 block holds COUNT two-byte properties
		// (0x20 = 1, then zero deltas), then an empty frame.
		let mut body = vec![0x1 | 0x4, 0, 2 * COUNT, 0x20, 1];
		for _ in 1..COUNT {
			body.extend([0, 1]);
		}
		body.push(0);
		let (res, track, pool) = receive_ietf(draft18(None), &body).await;
		res.unwrap();
		let group = track.consume().peek_group(0).unwrap();
		assert_eq!(group.frame_count(), 1);
		let decoded = COUNT * size_of::<crate::model::object::Property>() as u64;
		assert!(
			(2 * COUNT..decoded / 2).contains(&pool.used()),
			"{} cached for a {}-byte block",
			pool.used(),
			2 * COUNT
		);
	}

	/// Receive group 5 over `version`, one frame per millisecond timestamp delta, into the
	/// copy a subscriber makes there, whose group 4 started at 1000ms, returning the stream's
	/// result and the track.
	async fn receive_group(version: Version, deltas: &[i64]) -> (Result<(), Error>, track::Producer) {
		use crate::transport::poll::Session as _;

		let mut script = Vec::new();
		let mut encoder = crate::coding::Encoder::new(&mut script, version.into());
		lite::Group {
			subscribe: 7,
			key: 5,
			frame_start: 0,
		}
		.encode(&mut encoder, version)
		.unwrap();
		// The first delta counts from the group's start on lite-07, and from 0 before.
		let base = match version.has_group_end() {
			true => 5,
			false => 0,
		};
		for (index, delta) in deltas.iter().enumerate() {
			let delta = if index == 0 { delta - base } else { *delta };
			encoder.varint(crate::coding::varint::zigzag(delta)).unwrap();
			encoder.bytes(b"frame").unwrap();
		}
		let session = crate::lite::test_transport::ScriptedSession::eof(script);
		let subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(session.clone()),
			origin: origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce(),
			recv_bandwidth: None,
			version,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			peer_node: None,
			cost: None,
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});

		// Unchecked where the wire's timestamps may be send times, as the copy is accepted.
		let info = crate::track::Info {
			unchecked: !version.has_media_time(),
			..crate::track::Info::timed()
		};
		let track = track::Producer::new(std::sync::Arc::new(crate::broadcast::Info::default()), "video", info);
		let mut before = track.create_group(4u64.into()).unwrap();
		before
			.write_frame(Timestamp::from_millis(1000).unwrap(), &b"before"[..])
			.unwrap();
		before.finish().unwrap();
		subscriber.subscribes.lock().insert(
			7,
			TrackEntry::accepted(
				track.clone(),
				lite::Framing {
					timescale: Some(Timescale::MILLI),
					ietf: None,
					compressed: false,
					length_first: false,
				},
				Default::default(),
			),
		);

		let (_, recv) = session.open_bi(Default::default()).await.unwrap();
		let mut group = GroupRecv::new(
			subscriber,
			Reader::new(crate::transport::boxed::RecvStream::new(recv), version),
		);
		let res = kio::wait(|waiter| group.poll_serve(waiter)).await;
		(res, track)
	}

	/// Lite-05/06 timestamps may be send times, so a received group that rewinds keeps its
	/// timestamp unchecked, and the track stays open.
	#[kio::test]
	async fn a_lite06_rewind_passes_through() {
		for version in [Version::Lite05, Version::Lite06] {
			// Frame 0 at 500ms, before group 4's 1000ms.
			let (res, track) = receive_group(version, &[500]).await;
			res.unwrap_or_else(|err| panic!("{version}: {err:?}"));
			assert!(track.closed().now_or_never().is_none(), "{version}");
			let mut consumer = track.subscribe(None);
			let mut group = consumer.recv_group().now_or_never().unwrap().unwrap().unwrap();
			assert_eq!(group.start, 5, "{version}");
			let frame = group.read_frame().now_or_never().unwrap().unwrap().unwrap();
			assert_eq!(frame.payload.as_ref(), b"frame", "{version}");
			assert_eq!(frame.timestamp, Timestamp::from_millis(500).unwrap(), "{version}");
		}
	}

	/// Lite-07 timestamps are media time, so a received group is checked like a local
	/// write and one that rewinds is refused.
	#[kio::test]
	async fn a_lite07_rewind_is_refused() {
		let (res, _track) = receive_group(Version::Lite07, &[500]).await;
		assert!(matches!(res, Err(Error::TimestampMismatch)), "{res:?}");
	}

	/// A received timestamp past 2^61 - 1 is refused at every version, so a relay never
	/// holds one that lite-05/06 could not carry as a group's first frame. lite-06 reaches
	/// it with a second delta, lite-07 with a first frame its 64-bit varint can encode.
	#[kio::test]
	async fn a_received_timestamp_past_2_61_is_refused() {
		let max: i64 = (1 << 61) - 1;
		for version in [Version::Lite05, Version::Lite06, Version::Lite07] {
			let (res, track) = receive_group(version, &[max]).await;
			res.unwrap_or_else(|err| panic!("{version}: {err:?}"));
			let mut group = track.consume().peek_group(5).unwrap();
			let frame = group.read_frame().now_or_never().unwrap().unwrap().unwrap();
			assert_eq!(frame.timestamp.value(), max as u64, "{version}");

			let (res, _) = receive_group(version, &[max, 1]).await;
			assert!(matches!(res, Err(Error::BoundsExceeded(_))), "{version}: {res:?}");
		}
		let (res, _) = receive_group(Version::Lite07, &[max + 1]).await;
		assert!(matches!(res, Err(Error::BoundsExceeded(_))), "{res:?}");
	}

	/// Lite-05/06: an idle copy judges the gap after its cache by SUBSCRIBE_START, whichever
	/// lands first. A descending track's newest group overtaking the START waits for it,
	/// rather than reading as a feed that skipped past the cache.
	#[test]
	fn an_idle_copy_judges_the_gap_by_the_start() {
		for header_first in [true, false] {
			let mut track = track::Producer::new(
				std::sync::Arc::new(crate::broadcast::Info::default()),
				"video",
				crate::track::Info::timed(),
			);
			let mut cached = track.create_group(group::Info { start: 0 }).unwrap();
			cached.write_frame(cached.key_time(), &b"0"[..]).unwrap();
			cached.finish().unwrap();
			track.set_idle();
			let mut entry = TrackEntry::accepted(track.clone(), Default::default(), Default::default());

			// The route resends from the cached head, newest first.
			if header_first {
				entry.answered(None, Some(Position::group(3)));
				assert!(!track.is_live(), "the header waits for the start");
				entry.answered(Some(0), None);
			} else {
				entry.answered(Some(0), None);
				assert!(!track.is_live(), "the start waits for a group");
				entry.answered(None, Some(Position::group(3)));
			}
			let mut newest = track.create_group(group::Info { start: 3 }).unwrap();
			newest
				.write_frame(Timestamp::from_millis(300).unwrap(), &b"3"[..])
				.unwrap();
			newest.finish().unwrap();
			assert!(track.is_live(), "header first: {header_first}");

			let subscription = crate::track::Subscription::default()
				.with_max(std::time::Duration::from_secs(10))
				.with_range(0..);
			let mut reader = track.subscribe(subscription);
			let mut keys = Vec::new();
			while let Poll::Ready(Ok(Some(group))) = reader.poll_recv_group(&kio::Waiter::noop()) {
				keys.push(group.start);
			}
			keys.sort();
			assert_eq!(keys, [0, 3], "header first: {header_first}");
		}
	}

	/// An idle copy that resubscribes gets its newest cached group again. That stream only
	/// duplicates the cache, but lite-07's SUBSCRIBE_END still counts it, so the tail must
	/// too, or the subscription waits out the grace before it ends.
	#[kio::test]
	async fn a_duplicate_group_stream_still_counts_toward_the_end() {
		use crate::transport::poll::Session as _;

		let version = Version::Lite07;
		let mut script = Vec::new();
		lite::Group {
			subscribe: 7,
			key: 3,
			frame_start: 0,
		}
		.encode(&mut crate::coding::Encoder::new(&mut script, version.into()), version)
		.unwrap();
		let session = crate::lite::test_transport::ScriptedSession::eof(script);
		let clock = crate::time::Deadlines::new(crate::time::Instant::now());
		let subscriber = Subscriber::new(SubscriberConfig {
			runtime: clock.clone(),
			session: crate::driver::Transport::new(session.clone()),
			origin: origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce(),
			recv_bandwidth: None,
			version,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			peer_node: None,
			cost: None,
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});

		let track = track::Producer::new(
			std::sync::Arc::new(crate::broadcast::Info::default()),
			"video",
			crate::track::Info::timed(),
		);
		let mut cached = track.create_group(group::Info { start: 3 }).unwrap();
		cached.write_frame(cached.key_time(), &b"x"[..]).unwrap();
		cached.finish().unwrap();
		let tail = kio::Producer::new(Tail::default());
		subscriber.subscribes.lock().insert(
			7,
			TrackEntry::accepted(
				track.clone(),
				lite::Framing {
					timescale: Some(Timescale::MILLI),
					ietf: None,
					compressed: false,
					length_first: false,
				},
				tail.clone(),
			),
		);

		let (_, recv) = session.open_bi(Default::default()).await.unwrap();
		let mut group = GroupRecv::new(
			subscriber,
			Reader::new(crate::transport::boxed::RecvStream::new(recv), version),
		);
		let res = kio::wait(|waiter| group.poll_serve(waiter)).await;
		assert!(matches!(res, Err(Error::Duplicate)), "{res:?}");
		drop(group);

		// SUBSCRIBE_END counted one stream: the tail settles without the clock moving.
		let mut settle = Settle::new(&clock, tail.consume());
		assert!(settle.poll(&kio::Waiter::noop(), |tail| tail.streams() >= 1).is_ready());
	}

	/// A duplicate of a whole copy the cache already holds accounts for the copy's span, which its
	/// own GROUP_END would have: the publisher names none of those starts again. The span is read
	/// from whichever copy finished the group, even beside a fetched head still open.
	#[kio::test]
	async fn a_duplicate_of_a_whole_cached_copy_accounts_its_span() {
		use crate::transport::poll::Session as _;

		for beside_a_head in [false, true] {
			let version = Version::Lite07;
			let frame_start = u64::from(beside_a_head);
			let mut script = Vec::new();
			lite::Group {
				subscribe: 7,
				key: 3,
				frame_start,
			}
			.encode(&mut crate::coding::Encoder::new(&mut script, version.into()), version)
			.unwrap();
			let session = crate::lite::test_transport::ScriptedSession::eof(script);
			let clock = crate::time::Deadlines::new(crate::time::Instant::now());
			let subscriber = Subscriber::new(SubscriberConfig {
				runtime: clock.clone(),
				session: crate::driver::Transport::new(session.clone()),
				origin: origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce(),
				recv_bandwidth: None,
				version,
				peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
				peer_node: None,
				cost: None,
				going_away: Default::default(),
				auth: crate::auth::Link::new(false),
			});

			let track = track::Producer::new(
				std::sync::Arc::new(crate::broadcast::Info::default()),
				"video",
				crate::track::Info::timed(),
			);
			let dynamic = track.requests();
			let mut cached = track.create_group(group::Info { start: 3 }).unwrap();
			cached.start_at(frame_start).unwrap();
			cached.write_frame(cached.key_time(), &b"x"[..]).unwrap();
			// A fetched head from frame 0, still open beside the live copy from frame 1.
			let mut _head = None;
			if beside_a_head {
				let _fetch = track.consume().fetch_group(3, None);
				let request = dynamic.requested_fill_group().await.unwrap();
				let mut head = request
					.accept(crate::track::Info::timed())
					.unwrap()
					.expect("a fresh group");
				head.write_frame(head.key_time(), &b"snapshot"[..]).unwrap();
				_head = Some(head);
			}
			cached.finish_at(6).unwrap();
			let tail = kio::Producer::new(Tail::default());
			subscriber.subscribes.lock().insert(
				7,
				TrackEntry::accepted(
					track.clone(),
					lite::Framing {
						timescale: Some(Timescale::MILLI),
						ietf: None,
						compressed: false,
						length_first: false,
					},
					tail.clone(),
				),
			);

			let (_, recv) = session.open_bi(Default::default()).await.unwrap();
			let mut group = GroupRecv::new(
				subscriber,
				Reader::new(crate::transport::boxed::RecvStream::new(recv), version),
			);
			let res = kio::wait(|waiter| group.poll_serve(waiter)).await;
			assert!(matches!(res, Err(Error::Duplicate)), "{beside_a_head}: {res:?}");
			drop(group);
			assert!(
				tail.read().covers(3..6),
				"{beside_a_head}: the copy's span is accounted"
			);
		}
	}

	/// A lite-07 re-offer can overtake the reset of the stream it replaces: its stream waits,
	/// unread, until that copy settles. It is discarded once the copy finishes, accounting for
	/// the span a whole copy holds, and takes the group once the copy fails, continuing the
	/// reads of that copy.
	#[kio::test]
	async fn a_duplicate_of_an_arriving_copy_waits_for_it_to_settle() {
		use crate::transport::poll::Session as _;

		for first_fails in [false, true] {
			let version = Version::Lite07;
			let mut script = Vec::new();
			let mut encoder = crate::coding::Encoder::new(&mut script, version.into());
			lite::Group {
				subscribe: 7,
				key: 3,
				frame_start: 0,
			}
			.encode(&mut encoder, version)
			.unwrap();
			encoder.varint(crate::coding::varint::zigzag(300)).unwrap();
			encoder.bytes(b"first").unwrap();
			encoder.varint(crate::coding::varint::zigzag(1)).unwrap();
			encoder.bytes(b"again").unwrap();
			let session = crate::lite::test_transport::ScriptedSession::eof(script);
			let clock = crate::time::Deadlines::new(crate::time::Instant::now());
			let subscriber = Subscriber::new(SubscriberConfig {
				runtime: clock.clone(),
				session: crate::driver::Transport::new(session.clone()),
				origin: origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce(),
				recv_bandwidth: None,
				version,
				peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
				peer_node: None,
				cost: None,
				going_away: Default::default(),
				auth: crate::auth::Link::new(false),
			});

			let track = track::Producer::new(
				std::sync::Arc::new(crate::broadcast::Info::default()),
				"video",
				crate::track::Info::timed(),
			);
			let subscription = crate::track::Subscription::default()
				.with_max(std::time::Duration::from_secs(10))
				.with_range(0..);
			let mut reader = track.subscribe(subscription.clone());
			// The original copy, still arriving: one frame in, no end yet.
			let Ok(track::Received::Group(mut first)) = track.create_group_at(group::Info { start: 3 }, 0, 0..0) else {
				panic!("the first copy opens");
			};
			first
				.write_frame(Timestamp::from_millis(300).unwrap(), &b"first"[..])
				.unwrap();
			let tail = kio::Producer::new(Tail::default());
			subscriber.subscribes.lock().insert(
				7,
				TrackEntry::accepted(
					track.clone(),
					lite::Framing {
						timescale: Some(Timescale::MILLI),
						ietf: None,
						compressed: false,
						length_first: false,
					},
					tail.clone(),
				),
			);

			let (_, recv) = session.open_bi(Default::default()).await.unwrap();
			let mut duplicate = GroupRecv::new(
				subscriber,
				Reader::new(crate::transport::boxed::RecvStream::new(recv), version),
			);
			let waiter = kio::Waiter::noop();
			assert!(duplicate.poll_serve(&waiter).is_pending(), "first fails: {first_fails}");
			assert_eq!(duplicate.state.name(), "Hold", "first fails: {first_fails}");
			// Counted, but not holding the end open while it waits, so the end settles however
			// long the stream of the copy it waits on stalls.
			assert_eq!(tail.read().streams(), 1);
			assert!(!tail.read().is_reading(), "first fails: {first_fails}");
			let mut settle = Settle::new(&clock, tail.consume());
			assert!(settle.poll(&waiter, |tail| tail.covers(3..4)).is_ready());

			let mut original = reader.assert_group();
			let frame = original.read_frame().await.unwrap().unwrap();
			assert_eq!(&frame.payload[..], b"first");
			if !first_fails {
				first.finish_at(400).unwrap();
				let res = kio::wait(|waiter| duplicate.poll_serve(waiter)).await;
				assert!(matches!(res, Err(Error::Duplicate)), "{res:?}");
				assert!(original.read_frame().await.unwrap().is_none());
				// The copy finished whole, so the discarded stream accounts for its span too.
				assert!(tail.read().accounted(0..500).eq(std::iter::once(3..400)));
				continue;
			}

			// The reset lands: the reader already handed the original waits on the held stream
			// rather than failing, and continues on it from its frame once it takes the group.
			first.abort(Error::Cancel).unwrap();
			assert!(original.poll_read_frame(&waiter).is_pending());
			let res = kio::wait(|waiter| duplicate.poll_serve(waiter)).await;
			assert!(res.is_ok(), "{res:?}");
			assert_eq!(&original.read_frame().await.unwrap().unwrap().payload[..], b"again");
			assert!(original.read_frame().await.unwrap().is_none());
			// One that had not reached it reads the re-offer whole.
			let mut later = track.subscribe(subscription);
			let mut group = later.assert_group();
			assert_eq!(group.start, 3);
			assert_eq!(&group.read_frame().await.unwrap().unwrap().payload[..], b"first");
			assert_eq!(&group.read_frame().await.unwrap().unwrap().payload[..], b"again");
			assert!(group.read_frame().await.unwrap().is_none());
		}
	}

	/// A stream reset to make room for a higher-priority one ends its group with the reset's
	/// code on every version: the frames that arrived stay, and no later stream continues it.
	#[kio::test]
	async fn a_group_reset_to_make_room_ends() {
		use crate::lite::test_transport::{Close, ScriptedSession};
		use crate::transport::poll::Session as _;

		for version in [Version::Lite06, Version::Lite07] {
			let length_first = version == Version::Lite07;
			let framing = lite::Framing {
				timescale: Some(Timescale::MILLI),
				ietf: None,
				compressed: false,
				length_first,
			};
			let mut script = Vec::new();
			let mut encoder = crate::coding::Encoder::new(&mut script, version.into());
			lite::Group {
				subscribe: 7,
				key: 3,
				frame_start: 0,
			}
			.encode(&mut encoder, version)
			.unwrap();
			// Frame 0 whole, then frame 1 cut after three bytes.
			for (delta, payload, sent) in [(300, &b"zero"[..], 4), (10, &b"onetwo!"[..], 3)] {
				let mut encoder = crate::coding::Encoder::new(&mut script, version.into());
				if length_first {
					encoder.varint(payload.len() as u64 + 1).unwrap();
					encoder.varint(crate::coding::varint::zigzag(delta)).unwrap();
				} else {
					encoder.varint(crate::coding::varint::zigzag(delta)).unwrap();
					encoder.varint(payload.len() as u64).unwrap();
				}
				script.extend_from_slice(&payload[..sent]);
			}
			let session = ScriptedSession::new(script);
			let clock = crate::time::Deadlines::new(crate::time::Instant::now());
			let subscriber = Subscriber::new(SubscriberConfig {
				runtime: clock.clone(),
				session: crate::driver::Transport::new(session.clone()),
				origin: origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce(),
				recv_bandwidth: None,
				version,
				peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
				peer_node: None,
				cost: None,
				going_away: Default::default(),
				auth: crate::auth::Link::new(false),
			});
			let track = track::Producer::new(
				std::sync::Arc::new(crate::broadcast::Info::default()),
				"video",
				crate::track::Info::timed(),
			);
			let subscription = crate::track::Subscription::default()
				.with_max(std::time::Duration::from_secs(10))
				.with_range(0..);
			let mut reader = track.subscribe(subscription);
			let tail = kio::Producer::new(Tail::default());
			subscriber
				.subscribes
				.lock()
				.insert(7, TrackEntry::accepted(track.clone(), framing, tail.clone()));

			let (_, recv) = session.open_bi(Default::default()).await.unwrap();
			let mut stream = GroupRecv::new(
				subscriber.clone(),
				Reader::new(crate::transport::boxed::RecvStream::new(recv), version),
			);
			assert!(stream.poll_serve(&kio::Waiter::noop()).is_pending(), "{version:?}");
			let mut group = reader.assert_group();
			assert_eq!(group.start, 3);
			let read = group.read_frame().await.unwrap().unwrap();
			assert_eq!(&read.payload[..], b"zero", "{version:?}");

			session.close(Close::ResetStream(crate::StreamError::TooFarBehind.to_code()));
			let res = kio::wait(|waiter| stream.poll_serve(waiter)).await;
			assert!(res.is_err(), "{version:?}: {res:?}");
			let read = group.read_frame().await;
			assert!(
				matches!(read, Err(Error::Stream(crate::StreamError::TooFarBehind))),
				"{version:?}: {read:?}"
			);
			assert!(
				!tail.read().is_reading(),
				"{version:?}: the reset stream lets go of the end"
			);
		}
	}

	/// A SUBSCRIBE_END below a group already received contradicts that group. lite-05
	/// specified an inclusive end, and `@moq/net` 0.1.3 to 0.1.9 sent one, so there it only
	/// costs the early boundary; later drafts abort the track.
	#[kio::test]
	async fn a_subscribe_end_below_a_received_group() {
		for version in [Version::Lite05, Version::Lite06, Version::Lite07] {
			let mut responses = Vec::new();
			lite::SubscribeResponse::Start(start_answer(version, None))
				.encode(
					&mut crate::coding::Encoder::new(&mut responses, version.into()),
					version,
				)
				.unwrap();
			lite::SubscribeResponse::End(lite::SubscribeEnd { group: 2, error: None })
				.encode(
					&mut crate::coding::Encoder::new(&mut responses, version.into()),
					version,
				)
				.unwrap();
			let session = crate::lite::test_transport::ScriptedSession::eof(responses);
			let subscriber = Subscriber::new(SubscriberConfig {
				runtime: crate::time::sim(),
				session: crate::driver::Transport::new(session.clone()),
				origin: origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce(),
				recv_bandwidth: None,
				version,
				peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
				peer_node: None,
				cost: None,
				going_away: Default::default(),
				auth: crate::auth::Link::new(false),
			});
			let serve = TrackServe {
				validity: Default::default(),
				route: None,
				source: None,
				subscriber,
				path: Path::from_static("room"),
				instance: None,
				node: None,
				name: "video".to_string(),
			};
			let broadcast = crate::broadcast::Info::new().produce();
			let request = broadcast.reserve_track("video").unwrap();
			let serving = ServeLoop::new(&serve, request, crate::track::Info::timed());
			let mut group = serving.serving.create_group(group::Info { start: 2 }).unwrap();
			group.write_frame(group.key_time(), b"2".as_slice()).unwrap();
			group.finish().unwrap();
			let mut reader = broadcast
				.consume()
				.track("video")
				.unwrap()
				.subscribe(None)
				.await
				.unwrap();
			let gate = serve.gate();
			let mut running = TrackServeRun {
				serve,
				state: TrackRunState::Serve(serving),
				gate,
				datagrams: None,
				finished: None,
			};
			// The finished copy stays for its reader, serving fetches: the subscription is what
			// ends.
			kio::wait(|waiter| {
				let _ = poll_run(&mut running, waiter);
				match running.state {
					TrackRunState::Done => Poll::Ready(()),
					_ => Poll::Pending,
				}
			})
			.await;

			let end = loop {
				match reader.recv_group().await {
					Ok(Some(_)) => continue,
					end => break end.map(|group| group.map(|group| group.start)),
				}
			};
			match version {
				Version::Lite05 => assert!(matches!(end, Ok(None)), "{version:?}: {end:?}"),
				_ => assert!(matches!(end, Err(Error::ProtocolViolation)), "{version:?}: {end:?}"),
			}
		}
	}

	/// Answer a SUBSCRIBE under another instance with a finished, empty track, but only read the
	/// answer after `retire` changed its source's selection, then return what the reader got.
	async fn answer_after(
		version: Version,
		retire: impl FnOnce(&mut crate::model::TestSelection),
	) -> Option<Result<Option<u64>, Error>> {
		let session = crate::lite::test_transport::ScriptedSession::new(Vec::new());
		let subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(session.clone()),
			origin: origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce(),
			recv_bandwidth: None,
			version,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			peer_node: None,
			cost: None,
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});
		let mut selection = crate::model::TestSelection::new();
		let serve = TrackServe {
			validity: selection.validity(),
			route: None,
			source: None,
			subscriber,
			path: Path::from_static("room"),
			instance: None,
			node: None,
			name: "video".to_string(),
		};
		let broadcast = crate::broadcast::Info::new().produce();
		let request = broadcast.reserve_track("video").unwrap();
		let serving = ServeLoop::new(&serve, request, crate::track::Info::timed());
		let mut reader = broadcast
			.consume()
			.track("video")
			.unwrap()
			.subscribe(None)
			.await
			.unwrap();
		let gate = serve.gate();
		let mut running = TrackServeRun {
			serve,
			state: TrackRunState::Serve(serving),
			gate,
			datagrams: None,
			finished: None,
		};
		assert!(kio::Task::poll(&mut running, &kio::Waiter::noop()).is_pending());

		let mut answer = Vec::new();
		let mut encoder = crate::coding::Encoder::new(&mut answer, version.into());
		lite::SubscribeResponse::Start(start_answer(version, None))
			.encode(&mut encoder, version)
			.unwrap();
		lite::SubscribeResponse::End(lite::SubscribeEnd { group: 0, error: None })
			.encode(&mut encoder, version)
			.unwrap();
		session.push(&answer);
		retire(&mut selection);
		let _ = kio::Task::poll(&mut running, &kio::Waiter::noop());

		reader
			.recv_group()
			.now_or_never()
			.map(|res| res.map(|group| group.map(|group| group.start)))
	}

	/// An answer read only after another instance took the path may be that instance's,
	/// so the replacement refuses the subscription instead, as does a withdrawal followed
	/// by a new announcement.
	#[kio::test]
	async fn an_answer_read_after_a_replacement_is_refused() {
		for version in [Version::Lite05, Version::Lite06, Version::Lite07] {
			let res = answer_after(version, |selection| selection.replace()).await;
			assert!(matches!(res, Some(Err(Error::Unroutable))), "{version:?}: {res:?}");
			let res = answer_after(version, |selection| {
				selection.withdraw();
				selection.replace();
			})
			.await;
			assert!(matches!(res, Some(Err(Error::Unroutable))), "{version:?}: {res:?}");
		}
	}

	/// A withdrawal alone leaves the SUBSCRIBE to its answer: the publisher concludes a
	/// subscription in flight, so a track that finished as its broadcast ended arrives whole.
	#[kio::test]
	async fn an_answer_read_after_a_withdrawal_is_kept() {
		for version in [Version::Lite05, Version::Lite06, Version::Lite07] {
			let res = answer_after(version, |selection| selection.withdraw()).await;
			assert!(matches!(res, Some(Ok(None))), "{version:?}: {res:?}");
		}
	}

	/// Lite-07's SUBSCRIBE_START says where the track is, so a reader knows its largest
	/// frame before any group arrives. The FIN with no SUBSCRIBE_END then aborts the track,
	/// and the stream ends with it.
	#[kio::test]
	async fn subscribe_start_reports_the_largest() {
		let version = Version::Lite07;
		let largest = track::Position { group: 4, frame: 2 };
		let mut responses = Vec::new();
		lite::SubscribeResponse::Start(lite::SubscribeStart {
			id: 0,
			start: Some(4),
			largest: lite::Edge::Largest(lite::Largest {
				position: largest,
				ended: true,
			}),
		})
		.encode(
			&mut crate::coding::Encoder::new(&mut responses, version.into()),
			version,
		)
		.unwrap();
		let session = crate::lite::test_transport::ScriptedSession::eof(responses);
		let subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(session.clone()),
			origin: origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce(),
			recv_bandwidth: None,
			version,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			peer_node: None,
			cost: None,
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});
		let serve = TrackServe {
			validity: Default::default(),
			route: None,
			source: None,
			subscriber,
			path: Path::from_static("room"),
			instance: None,
			node: None,
			name: "video".to_string(),
		};
		let broadcast = crate::broadcast::Info::new().produce();
		let request = broadcast.reserve_track("video").unwrap();
		let serving = ServeLoop::new(&serve, request, crate::track::Info::timed());
		let mut reader = broadcast
			.consume()
			.track("video")
			.unwrap()
			.subscribe(None)
			.await
			.unwrap();
		let gate = serve.gate();
		let mut running = TrackServeRun {
			serve,
			state: TrackRunState::Serve(serving),
			gate,
			datagrams: None,
			finished: None,
		};
		kio::wait(|waiter| kio::Task::poll(&mut running, waiter)).await;

		// Recorded as ended, so a relay serving this copy says so in its own SUBSCRIBE_START.
		assert!(reader.largest_finished(largest));
		assert_eq!(reader.largest().await, Some(largest));
		assert_eq!(reader.largest().await, None, "the stream ends with the aborted track");
		assert!(matches!(reader.recv_group().await, Err(Error::ProtocolViolation)));
	}

	/// Every lite-07 SUBSCRIBE_START says where the track is, so one answering a range after
	/// the copy went live still moves its largest, and whether its group ended there, which a
	/// relay serving the copy forwards in its own SUBSCRIBE_STARTs.
	#[kio::test]
	async fn a_later_subscribe_start_moves_the_largest() {
		let (serve, mut serving, _tail, _broadcast) = lite07_serving(Duration::from_millis(500)).await;
		let bounded = |start, end| lite::Range {
			start: Some(lite::Anchor::Time(start)),
			end: Some(lite::Anchor::Time(end)),
		};
		let Sub::Active(active) = &mut serving.sub else {
			unreachable!("the helper opens a subscription");
		};
		active.live = lite::LiveRanges::new(Version::Lite07, &[bounded(0, 1000), bounded(1000, 2000)]);
		let reader = serving.serving.subscribe(track::Subscription::default());
		for (id, position, ended) in [
			(0, track::Position { group: 4, frame: 2 }, false),
			(1, track::Position { group: 6, frame: 1 }, true),
		] {
			let start = lite::SubscribeStart {
				id,
				start: Some(position.group),
				largest: lite::Edge::Largest(lite::Largest { position, ended }),
			};
			let moved = serving.record_start(&serve, &start).unwrap();
			serving.apply_starts(&serve, start.largest, moved).unwrap();
			assert_eq!(serving.serving.largest(), Some(position), "range {id}");
			assert_eq!(reader.largest_finished(position), ended, "range {id}");
		}
	}

	/// A lite-07 subscription's serve loop with an active SUBSCRIBE under the max age `budget`,
	/// its broadcast, and the tail it accounts on.
	async fn lite07_serving(
		budget: Duration,
	) -> (TrackServe, ServeLoop, kio::Producer<Tail>, crate::broadcast::Producer) {
		let version = Version::Lite07;
		let session = crate::lite::test_transport::ScriptedSession::new(Vec::new());
		let subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(session.clone()),
			origin: origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce(),
			recv_bandwidth: None,
			version,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			peer_node: None,
			cost: None,
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});
		let serve = TrackServe {
			validity: Default::default(),
			route: None,
			source: None,
			subscriber,
			path: Path::from_static("room"),
			instance: None,
			node: None,
			name: "video".to_string(),
		};
		let broadcast = crate::broadcast::Info::new().produce();
		let request = broadcast.reserve_track("video").unwrap();
		let mut serving = ServeLoop::new(&serve, request, crate::track::Info::timed());
		let tail = kio::Producer::new(Tail::default());
		serving.sub = Sub::Active(SubStream {
			order: crate::track::Order::Descending,
			stream: Stream::open(&crate::driver::Transport::new(session.clone()), version)
				.await
				.unwrap(),
			id: 0,
			max_delay: Some(budget),
			stale_budget: budget,
			stale_cap: None,
			start: None,
			priority: 0,
			requested: None,
			live: lite::LiveRanges::new(version, &[lite::Range::OPEN]),
			demand: Some(Subscription::default()),
			fill_ids: Default::default(),
			probe_ids: Default::default(),
			fill_groups: Default::default(),
			tail: tail.clone(),
			started: None,
			went_live: false,
			finishing: None,
			parked: false,
			starts: Default::default(),
			join_ticks: Default::default(),
			join_budgets: Default::default(),
			declared: None,
			end: None,
			wanted: None,
			unanswered: None,
			_working: crate::session::Work::default().start(),
		});
		(serve, serving, tail, broadcast)
	}

	/// A run no fill owes stays asked while a stream of it still arrives, judged by the copy
	/// alone: one the pool evicts mid-stream lets the run go without asking a flight of the
	/// group, which nobody here would read.
	#[kio::test]
	#[expect(clippy::single_range_in_vec_init, reason = "the asked runs are a list, here one")]
	async fn a_drained_run_asks_no_flight_for_an_evicted_stream() {
		let (serve, _serving, _tail, _broadcast) = lite07_serving(Duration::from_millis(500)).await;
		let waiter = kio::Waiter::noop();
		let copy = crate::track::Producer::new(
			std::sync::Arc::new(crate::broadcast::Info::default()),
			"video",
			crate::track::Info::default()
				.with_timescale(crate::Timescale::MILLI)
				.with_max_age(None),
		);
		let requests = copy.requests();
		let mut arriving = copy.create_group(group::Info { start: 0 }).unwrap();
		arriving
			.write_frame(crate::Timestamp::from_millis(0).unwrap(), b"x".as_slice())
			.unwrap();
		let mut fills = fills::RangeFills::default();
		fills.ask.groups.insert(0..1);
		fills.poll(&serve, &copy, None, true, &waiter).unwrap();
		assert_eq!(
			fills.ask.groups.iter().cloned().collect::<Vec<_>>(),
			[0..1],
			"its stream still arrives"
		);

		copy.evict(0);
		assert!(fills.poll(&serve, &copy, None, true, &waiter).unwrap());
		assert!(fills.ask.groups.is_empty(), "the stream ended with its copy");
		assert!(requests.poll_requested_fill(&waiter).is_pending(), "no flight is asked");
		drop(arriving);
	}

	/// A run no fill owes stays asked for a stream of it still arriving only while someone reads
	/// the track: once nobody does, the fills ask for nothing, so the subscription they hold
	/// and the copy's linger can end.
	#[kio::test]
	async fn a_drained_run_goes_once_nobody_reads() {
		let (serve, _serving, _tail, _broadcast) = lite07_serving(Duration::from_millis(500)).await;
		let waiter = kio::Waiter::noop();
		let copy = crate::track::Producer::new(
			std::sync::Arc::new(crate::broadcast::Info::default()),
			"video",
			crate::track::Info::default()
				.with_timescale(crate::Timescale::MILLI)
				.with_max_age(None),
		);
		let mut arriving = copy.create_group(group::Info { start: 0 }).unwrap();
		arriving
			.write_frame(crate::Timestamp::from_millis(0).unwrap(), b"x".as_slice())
			.unwrap();
		let mut fills = fills::RangeFills::default();
		fills.ask.groups.insert(0..1);
		assert!(!fills.poll(&serve, &copy, None, true, &waiter).unwrap());
		assert!(!fills.is_empty(), "its stream still arrives for a reader");

		assert!(fills.poll(&serve, &copy, None, false, &waiter).unwrap());
		assert!(fills.is_empty(), "nobody reads the stream still arriving");
		drop(arriving);
	}

	/// A stale drop of part of a fill's run recovers that part on its own subscription, which
	/// takes it out of the copy's ranges at once, though the rest of the run stays asked: the
	/// subscription that dropped it never delivers it.
	#[kio::test]
	#[expect(clippy::single_range_in_vec_init, reason = "the asked runs are a list, here one")]
	async fn a_recovered_run_leaves_the_subscription() {
		let (serve, _serving, _tail, _broadcast) = lite07_serving(Duration::from_millis(500)).await;
		let waiter = kio::Waiter::noop();
		let copy = crate::track::Producer::new(
			std::sync::Arc::new(crate::broadcast::Info::default()),
			"video",
			crate::track::Info::default()
				.with_timescale(crate::Timescale::MILLI)
				.with_max_age(None),
		);
		let requests = copy.requests();
		for key in 0..6 {
			let mut group = copy.create_group(group::Info { start: key }).unwrap();
			group
				.write_frame(crate::Timestamp::from_millis(key * 1000).unwrap(), b"x".as_slice())
				.unwrap();
			group.finish().unwrap();
		}
		// From group 0, which a time start names whatever is held, so the reader asks the
		// missing groups straight away rather than looking up where its start falls.
		for key in 0..4 {
			copy.evict(key);
		}
		let mut reader = copy
			.subscribe(crate::track::Subscription::default().with_max(None).with_range(0..4))
			.ordered();
		assert!(reader.poll_next_group(&waiter).is_pending(), "groups 0..4 are owed");
		let Poll::Ready(Ok(fill)) = requests.poll_requested_fill(&waiter) else {
			panic!("groups 0..4 are a miss");
		};
		let mut fills = fills::RangeFills::default();
		fills.push(fill);
		assert!(fills.poll(&serve, &copy, None, true, &waiter).unwrap());
		assert_eq!(fills.ask.groups.iter().cloned().collect::<Vec<_>>(), [0..4]);

		fills.refuse(0..2, crate::StreamError::Old);
		assert!(fills.poll(&serve, &copy, None, true, &waiter).unwrap());
		assert!(fills.recovering(), "0..2 recovered on its own subscription");
		assert_eq!(
			fills.ask.groups.iter().cloned().collect::<Vec<_>>(),
			[2..4],
			"the recovered run is no longer asked"
		);
	}

	/// A group the subscription delivered, recovered once and evicted again before its reader
	/// took it, is recovered again rather than refused, and the copy landing is held for the
	/// reader: a key a reader is owed is never lost to the cache.
	#[kio::test]
	async fn an_evicted_recovered_group_is_recovered_again() {
		let (serve, _serving, tail, _broadcast) = lite07_serving(Duration::from_millis(500)).await;
		let waiter = kio::Waiter::noop();
		let copy = crate::track::Producer::new(
			std::sync::Arc::new(crate::broadcast::Info::default()),
			"video",
			crate::track::Info::default()
				.with_timescale(crate::Timescale::MILLI)
				.with_max_age(None),
		);
		let requests = copy.requests();
		let write = |key: u64| {
			let mut group = copy.create_group(group::Info { start: key }).unwrap();
			group
				.write_frame(crate::Timestamp::from_millis(key * 1000).unwrap(), b"x".as_slice())
				.unwrap();
			group.finish().unwrap();
		};
		for key in 0..3 {
			write(key);
		}
		copy.evict(0);
		let mut reader = copy
			.subscribe(crate::track::Subscription::default().with_range(0..1))
			.ordered();
		assert!(reader.poll_next_group(&waiter).is_pending(), "group 0 is owed");
		let Poll::Ready(Ok(fill)) = requests.poll_requested_fill(&waiter) else {
			panic!("group 0 is a miss");
		};
		let mut fills = fills::RangeFills::default();
		fills.push(fill);
		// Delivered on the subscription before, and recovered once since.
		let Ok(mut accounted) = tail.write() else {
			unreachable!("the tail is open");
		};
		accounted.account(0..1, serve.subscriber.runtime.now());
		drop(accounted);
		fills.recovered_once(0..1);
		fills.poll(&serve, &copy, Some(&tail), true, &waiter).unwrap();
		assert!(fills.recovering(), "recovered again");
		assert!(reader.poll_next_group(&waiter).is_pending(), "still owed, not given up");

		// The recovered copy lands and is held for the reader, which takes it.
		write(0);
		fills.poll(&serve, &copy, Some(&tail), true, &waiter).unwrap();
		let Poll::Ready(Ok(Some(group))) = reader.poll_next_group(&waiter) else {
			panic!("group 0 reaches the reader");
		};
		assert_eq!(group.start, 0);
	}

	/// Whether a fill owing group 0, recovered once already as `size` bytes and evicted for
	/// debt, recovers it again once `room` resizes the pool. If `lands`, the recovery landed in
	/// the run the fill awaits, held, and was evicted for debt before the fill answered it,
	/// and the track forgot every eviction since, as a debt or the miss grace does.
	async fn recovers_again(size: usize, room: impl Fn(&crate::cache::Pool), lands: bool) -> bool {
		let (serve, _serving, tail, _broadcast) = lite07_serving(Duration::from_millis(500)).await;
		let waiter = kio::Waiter::noop();
		let pool = crate::cache::Pool::unbounded();
		let copy = crate::track::Producer::new(
			std::sync::Arc::new(crate::broadcast::Info {
				pool: pool.clone(),
				..Default::default()
			}),
			"video",
			crate::track::Info::default()
				.with_timescale(crate::Timescale::MILLI)
				.with_max_age(None),
		);
		let requests = copy.requests();
		for key in 0..3 {
			let mut group = copy.create_group(group::Info { start: key }).unwrap();
			let frame = vec![0u8; if key == 0 { size } else { 1 }];
			group
				.write_frame(crate::Timestamp::from_millis(key * 1000).unwrap(), frame)
				.unwrap();
			group.finish().unwrap();
		}
		copy.evict_for_debt(0);
		let mut reader = copy
			.subscribe(crate::track::Subscription::default().with_range(0..1))
			.ordered();
		assert!(reader.poll_next_group(&waiter).is_pending(), "group 0 is owed");
		let Poll::Ready(Ok(fill)) = requests.poll_requested_fill(&waiter) else {
			panic!("group 0 is a miss");
		};
		if lands {
			assert_eq!(fill.next_run(), Some(0..1), "the fill awaits group 0");
			let mut group = copy.create_group(group::Info { start: 0 }).unwrap();
			group.write_frame(crate::Timestamp::ZERO, vec![0u8; size]).unwrap();
			group.finish().unwrap();
			room(&pool);
			copy.evict_for_debt(0);
			copy.forget_lost();
		}
		let mut fills = fills::RangeFills::default();
		fills.push(fill);
		let Ok(mut accounted) = tail.write() else {
			unreachable!("the tail is open");
		};
		accounted.account(0..1, serve.subscriber.runtime.now());
		drop(accounted);
		fills.recovered_once(0..1);
		room(&pool);
		fills.poll(&serve, &copy, Some(&tail), true, &waiter).unwrap();
		let again = fills.recovering();
		assert_eq!(fills.is_empty(), !again, "refused unless recovered");
		again
	}

	/// A group recovered once already is refused rather than recovered again while no hold
	/// fits, as in a pool its live edges fill: the pool would evict the copy landing unheld,
	/// and each recovery after it would go the same way.
	#[kio::test]
	async fn a_recovered_group_is_not_recovered_again_without_hold_room() {
		assert!(!recovers_again(1, |pool| pool.resize(0), false).await);
	}

	/// A group recovered once already is refused rather than recovered again when it is bigger
	/// than the room the pool has for a hold, however much room that is: it would land held
	/// past the capacity, be evicted again, and be recovered again, forever.
	#[kio::test]
	async fn a_recovered_group_too_big_to_hold_is_not_recovered_again() {
		// Room for a hold beside what the copy caches, though not for group 0.
		let room = |pool: &crate::cache::Pool| pool.resize(pool.used() + 1_000);
		assert!(!recovers_again(100_000, room, false).await);
		assert!(
			recovers_again(100, room, false).await,
			"one that fits is recovered again"
		);
	}

	/// A recovery too big to hold, evicted before its fill answered it, is not recovered again
	/// once the track forgets the eviction: the fill that held it keeps the verdict.
	#[kio::test]
	async fn a_recovery_too_big_to_hold_is_not_recovered_again_once_its_eviction_is_forgotten() {
		// Room for a hold beside the copy's live edge, though not for group 0.
		let room = |pool: &crate::cache::Pool| pool.resize(pool.pinned() + 10_000);
		assert!(!recovers_again(100_000, room, true).await);
		assert!(
			recovers_again(100, room, true).await,
			"one that fits is recovered again"
		);
	}

	/// A group evicted after the subscription delivered it is recovered again even when the
	/// run owing it starts with keys a recovery answered skipped: a refusal answers only its
	/// own keys.
	#[kio::test]
	async fn an_evicted_group_past_a_skipped_run_is_recovered() {
		let (serve, _serving, tail, _broadcast) = lite07_serving(Duration::from_millis(500)).await;
		let waiter = kio::Waiter::noop();
		let copy = crate::track::Producer::new(
			std::sync::Arc::new(crate::broadcast::Info::default()),
			"video",
			crate::track::Info::default()
				.with_timescale(crate::Timescale::MILLI)
				.with_max_age(None),
		);
		let requests = copy.requests();
		let write = |key: u64| {
			let mut group = copy.create_group(group::Info { start: key }).unwrap();
			group
				.write_frame(crate::Timestamp::from_millis(key * 1000).unwrap(), b"x".as_slice())
				.unwrap();
			group.finish().unwrap();
		};
		for key in 0..3 {
			write(key);
		}
		copy.evict(0);
		copy.evict(1);
		let mut reader = copy
			.subscribe(crate::track::Subscription::default().with_range(0..2))
			.ordered();
		assert!(reader.poll_next_group(&waiter).is_pending(), "groups 0 and 1 are owed");
		let Poll::Ready(Ok(fill)) = requests.poll_requested_fill(&waiter) else {
			panic!("a miss");
		};
		let mut fills = fills::RangeFills::default();
		fills.push(fill);
		let Ok(mut accounted) = tail.write() else {
			unreachable!("the tail is open");
		};
		accounted.account(0..2, serve.subscriber.runtime.now());
		drop(accounted);
		// The run the fill owes starts at the key a recovery answered skipped.
		fills.refused_skipped(0..1);
		fills.poll(&serve, &copy, Some(&tail), true, &waiter).unwrap();
		assert!(fills.recovering(), "group 1 is recovered");

		write(1);
		fills.poll(&serve, &copy, Some(&tail), true, &waiter).unwrap();
		let Poll::Ready(Ok(Some(group))) = reader.poll_next_group(&waiter) else {
			panic!("group 1 reaches the reader");
		};
		assert_eq!(group.start, 1);
	}

	/// What a lite-06 copy holds and what its reader owes.
	struct Missing {
		/// The groups written to the copy.
		written: Vec<u64>,
		/// Those evicted again.
		evicted: std::ops::Range<u64>,
		/// The keys the reader reads in order.
		read: std::ops::Range<u64>,
	}

	impl Missing {
		/// Groups 0..6 written, 2..4 evicted and read.
		fn evicted() -> Self {
			Self {
				written: (0..6).collect(),
				evicted: 2..4,
				read: 2..4,
			}
		}
	}

	/// A lite-06 copy over `session` whose reader owes `missing`, its run already recovered on a
	/// SUBSCRIBE of its own.
	struct Recovering {
		serve: TrackServe,
		copy: crate::track::Producer,
		fills: fills::RangeFills,
		/// The reader, the session, and the copy's requests, held for the test.
		held: (
			crate::track::Ordered,
			crate::lite::test_transport::ScriptedSession,
			crate::track::Requests,
		),
	}

	fn recovering(session: crate::lite::test_transport::ScriptedSession, missing: Missing) -> Recovering {
		let version = Version::Lite06;
		let subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(session.clone()),
			origin: origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce(),
			recv_bandwidth: None,
			version,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			peer_node: None,
			cost: None,
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});
		let serve = TrackServe {
			validity: Default::default(),
			route: None,
			source: None,
			subscriber,
			path: Path::from_static("room"),
			instance: None,
			node: None,
			name: "video".to_string(),
		};
		let waiter = kio::Waiter::noop();
		let copy = crate::track::Producer::new(
			std::sync::Arc::new(crate::broadcast::Info::default()),
			"video",
			crate::track::Info::default()
				.with_timescale(crate::Timescale::MILLI)
				.with_max_age(None),
		);
		let requests = copy.requests();
		for &key in &missing.written {
			let mut group = copy.create_group(group::Info { start: key }).unwrap();
			group
				.write_frame(crate::Timestamp::from_millis(key * 1000).unwrap(), b"x".as_slice())
				.unwrap();
			group.finish().unwrap();
		}
		for key in missing.evicted {
			copy.evict(key);
		}
		let mut reader = copy
			.subscribe(crate::track::Subscription::default().with_range(missing.read))
			.ordered();
		assert!(reader.poll_next_group(&waiter).is_pending(), "the read is owed");
		let Poll::Ready(Ok(fill)) = requests.poll_requested_fill(&waiter) else {
			panic!("the read is a miss");
		};
		let mut fills = fills::RangeFills::new(version);
		fills.push(fill);
		fills.poll(&serve, &copy, None, true, &waiter).unwrap();
		assert!(fills.recovering(), "the miss is recovered on its own subscription");
		Recovering {
			serve,
			copy,
			fills,
			held: (reader, session, requests),
		}
	}

	/// Encode lite-06 subscribe `responses`.
	fn lite06_responses(responses: impl IntoIterator<Item = lite::SubscribeResponse>) -> Vec<u8> {
		let version = Version::Lite06;
		let mut bytes = Vec::new();
		for response in responses {
			response
				.encode(&mut crate::coding::Encoder::new(&mut bytes, version.into()), version)
				.unwrap();
		}
		bytes
	}

	/// TEMPORARY MIGRATION, delete with `fills::Recovery::walk`: a relay of the previous release
	/// starts a lite-06 recovery at its floor, sends the next group it holds, and never accounts
	/// for the keys between. Once the grace passes, the fill owing them goes to FETCH instead.
	#[kio::test]
	async fn a_stalled_recovery_walks_its_keys() {
		let responses = lite06_responses([lite::SubscribeResponse::Start(lite::SubscribeStart {
			id: 0,
			start: Some(2),
			largest: lite::Edge::Unknown,
		})]);
		let Recovering {
			serve,
			copy,
			mut fills,
			held: _held,
		} = recovering(
			crate::lite::test_transport::ScriptedSession::new(responses),
			Missing::evicted(),
		);
		let waiter = kio::Waiter::noop();

		// The relay sends group 4, the next it holds, and nothing for 2 or 3.
		{
			let subscribes = serve.subscriber.subscribes.lock();
			let (_, entry) = subscribes.iter().next().expect("the recovery subscribed");
			Reading::open(&entry.tail, Some(4), serve.subscriber.runtime.now());
		}
		fills.poll(&serve, &copy, None, true, &waiter).unwrap();
		assert!(fills.take_walks().is_empty(), "the grace has not passed");
		assert!(fills.recovering());

		kio::clock::sleep(crate::tail::GRACE * 2).await;
		fills.poll(&serve, &copy, None, true, &waiter).unwrap();
		let walks = fills.take_walks();
		assert_eq!(walks.len(), 1, "the fill owing 2..4 goes to FETCH");
		assert_eq!(walks[0].wanted(), Some(2));
		assert!(!fills.recovering(), "the stalled recovery ended");
	}

	/// A lite-06 publisher that finished the track FINs a recovery before the group streams
	/// it opened land. Nothing names a key after that FIN, so one that never lands is given up
	/// after the grace, even when no max delay bounds the copy's own.
	#[kio::test]
	async fn a_drained_recovery_gives_up_a_missing_key() {
		let responses = lite06_responses([
			lite::SubscribeResponse::Start(lite::SubscribeStart {
				id: 0,
				start: Some(2),
				largest: lite::Edge::Unknown,
			}),
			lite::SubscribeResponse::End(lite::SubscribeEnd { group: 6, error: None }),
		]);
		let Recovering {
			serve,
			copy,
			mut fills,
			held: _held,
		} = recovering(
			crate::lite::test_transport::ScriptedSession::eof(responses),
			Missing::evicted(),
		);
		let waiter = kio::Waiter::noop();

		// The FIN is read, and group 2 never lands.
		fills.poll(&serve, &copy, None, true, &waiter).unwrap();
		assert!(fills.recovering(), "group 2 may still land");

		kio::clock::sleep(crate::tail::GRACE * 2).await;
		fills.poll(&serve, &copy, None, true, &waiter).unwrap();
		assert!(!fills.recovering(), "the drained recovery gave up group 2");
	}

	/// On lite-07 an OLD drop is final, recorded as a stale verdict under the narrowest budget
	/// the subscription asked: a SUBSCRIBE_UPDATE widening the budget reopens nothing, and an
	/// OLD drop arriving after it may have been judged before the publisher read it.
	#[kio::test]
	async fn an_update_reopens_no_stale_drop() {
		let version = Version::Lite07;
		let budget = Duration::from_millis(500);
		let (serve, mut serving, tail, _broadcast) = lite07_serving(budget).await;
		// A live join under the budget, which the upstream placed at group 0: a stale drop
		// under that budget ends what it reads.
		let lag = Some(Duration::from_secs(10));
		let mut reader =
			serving.serving.subscribe(
				crate::track::Subscription::default().with_ranges([crate::track::Range::from(
					crate::track::Start::Live {
						min: lag,
						max: Some(budget),
					},
				)]),
			);
		serving.serving.answer_join(lag, None, Some(0), Duration::MAX);
		let waiter = kio::Waiter::noop();
		let old = |groups: std::ops::Range<u64>| {
			lite::SubscribeResponse::Drop(lite::SubscribeDrop::new(groups, &crate::StreamError::Old, version))
		};
		let covers = |groups: std::ops::Range<u64>| tail.read().covers(groups);

		serving.apply_response(&serve, old(1..3)).unwrap();
		assert!(covers(1..3));
		assert_eq!(
			reader.poll_drop(&waiter),
			Poll::Ready((1..3, track::Hole::stale(budget)))
		);

		{
			let Sub::Active(active) = &mut serving.sub else {
				unreachable!("an active subscription");
			};
			// A live join under a wider max age replaces the range.
			let wider = lite::Range {
				start: Some(lite::Anchor::Live {
					min: Some(Duration::ZERO),
					max: Some(Duration::from_secs(5)),
				}),
				end: None,
			};
			let ranges = active.live.edit(&[wider]).unwrap();
			buffer_update(active, ranges).unwrap();
		}
		assert!(covers(1..3), "still accounted for");
		assert!(reader.poll_drop(&waiter).is_pending(), "reported once");

		serving.apply_response(&serve, old(4..5)).unwrap();
		assert!(covers(4..5));
		assert_eq!(
			reader.poll_drop(&waiter),
			Poll::Ready((4..5, track::Hole::stale(budget)))
		);
	}

	/// A stale drop is recorded within the cap its subscription asked under, so a reader capped
	/// alike settles on it, though the groups above the run have not arrived, instead of
	/// fetching a group they would convict. A cap that varied, or none, settles no capped reader.
	#[kio::test]
	async fn a_stale_drop_carries_the_cap_it_was_asked_under() {
		let version = Version::Lite07;
		let budget = Duration::from_millis(500);
		let (serve, mut serving, _tail, _broadcast) = lite07_serving(budget).await;
		let lag = Some(Duration::from_secs(10));
		let live = lite::Range {
			start: Some(lite::Anchor::Live {
				min: lag,
				max: Some(budget),
			}),
			end: Some(lite::Anchor::Time(3)),
		};
		let Sub::Active(active) = &mut serving.sub else {
			unreachable!("an active subscription");
		};
		active.live = lite::LiveRanges::new(version, &[live]);
		active.stale_cap = cap_of(&active.live);
		assert_eq!(active.stale_cap, Some(4), "an inclusive time end caps one past it");

		let mut capped = serving
			.serving
			.subscribe(crate::track::Subscription::default().with_ranges([crate::track::Range {
				start: crate::track::Start::Live {
					min: lag,
					max: Some(budget),
				},
				end: Some(3),
			}]));
		serving.serving.answer_join(lag, Some(3), Some(0), Duration::MAX);
		let old = lite::SubscribeResponse::Drop(lite::SubscribeDrop::new(1..3, &crate::StreamError::Old, version));
		serving.apply_response(&serve, old).unwrap();
		assert_eq!(
			capped.poll_drop(&kio::Waiter::noop()),
			Poll::Ready((1..3, track::Hole::stale(budget).capped(Some(4))))
		);
	}

	/// The cap of a subscription is where its last range ends: none while one follows the live
	/// edge, and an empty range reaches nothing.
	#[test]
	fn the_cap_is_where_the_last_range_ends() {
		let version = Version::Lite07;
		let ending = |end: Option<lite::Anchor>| lite::Range { start: None, end };
		let cap = |ranges: &[lite::Range]| cap_of(&lite::LiveRanges::new(version, ranges));
		let position = |group: u64, frame: u64| lite::Anchor::Absolute(crate::track::Position::new(group, frame));
		assert_eq!(cap(&[ending(Some(lite::Anchor::Time(3)))]), Some(4));
		assert_eq!(
			cap(&[ending(Some(position(5, 0)))]),
			Some(5),
			"a whole group ends before it"
		);
		assert_eq!(
			cap(&[ending(Some(position(5, 2)))]),
			Some(6),
			"a partial one includes it"
		);
		assert_eq!(cap(&[ending(Some(position(0, 0)))]), Some(0), "nothing to reach");
		assert_eq!(
			cap(&[ending(Some(lite::Anchor::Time(3))), ending(Some(position(9, 0)))]),
			Some(9)
		);
		assert_eq!(cap(&[ending(Some(lite::Anchor::Time(3))), ending(None)]), None);
	}

	/// A group that arrives over a lite-07 stale (OLD) drop wins over it in the copy, whichever
	/// lands first: the group's stream may overtake the drop, or a recovery may deliver it. A
	/// group still arriving is never aborted by the drop, and an update owes none of them again.
	#[kio::test]
	async fn a_group_wins_over_its_stale_drop() {
		let version = Version::Lite07;
		let (serve, mut serving, tail, _broadcast) = lite07_serving(Duration::from_millis(500)).await;
		let now = || serve.subscriber.runtime.now();
		let code = u64::from(crate::coding::StreamCodes::encode_stream_code(
			&version,
			&crate::StreamError::Old,
		));
		let old = |key: u64| {
			lite::SubscribeResponse::Drop(lite::SubscribeDrop {
				groups: key..key + 1,
				error: code,
			})
		};
		let frame = |mut group: crate::group::Producer| {
			group.write_frame(group.key_time(), &b"x"[..])?;
			group.finish().map(drop)
		};
		let arrive = |serving: &mut ServeLoop, key| {
			let group = serving.serving.create_group(crate::group::Info { start: key }).unwrap();
			(group, Reading::open(&tail, Some(key), now()))
		};

		// OLD, then the group, which finishes.
		serving.apply_response(&serve, old(1)).unwrap();
		let (one, reading) = arrive(&mut serving, 1);
		frame(one).unwrap();
		drop(reading);
		// The group first, still arriving when its OLD lands.
		let (two, _two) = arrive(&mut serving, 2);
		serving.apply_response(&serve, old(2)).unwrap();
		// OLD, then the group, still arriving at the update.
		serving.apply_response(&serve, old(3)).unwrap();
		let (three, _three) = arrive(&mut serving, 3);
		// OLD, and no group.
		serving.apply_response(&serve, old(4)).unwrap();

		{
			let Sub::Active(active) = &mut serving.sub else {
				unreachable!("an active subscription");
			};
			let ranges = active.live.unchanged();
			buffer_update(active, ranges).unwrap();
		}
		assert!(tail.read().covers(1..5), "every group stays accounted for");
		frame(two).expect("the drop leaves a group still arriving");
		frame(three).expect("the update leaves a group still arriving");
	}

	/// A SKIP, or any final drop, that follows an OLD drop of the same groups replaces it in
	/// the copy, and an update reopens none of them. An empty range names nothing.
	#[kio::test]
	async fn a_skip_or_final_drop_replaces_an_earlier_stale_one() {
		let version = Version::Lite07;
		let budget = Duration::from_millis(500);
		let (serve, mut serving, tail, _broadcast) = lite07_serving(budget).await;
		let code = u64::from(crate::coding::StreamCodes::encode_stream_code(
			&version,
			&crate::StreamError::Old,
		));
		let old = |first, last: u64| {
			lite::SubscribeResponse::Drop(lite::SubscribeDrop {
				groups: first..last + 1,
				error: code,
			})
		};
		let covers = |groups: std::ops::Range<u64>| tail.read().covers(groups);

		serving.apply_response(&serve, old(1, 2)).unwrap();
		serving.apply_response(&serve, old(4, 4)).unwrap();
		serving
			.apply_response(
				&serve,
				lite::SubscribeResponse::Drop(lite::SubscribeDrop::new(1..3, &crate::StreamError::Skip, version)),
			)
			.unwrap();
		serving
			.apply_response(
				&serve,
				lite::SubscribeResponse::Drop(lite::SubscribeDrop::new(4..5, &crate::StreamError::App(5), version)),
			)
			.unwrap();
		// Empty ranges.
		serving.apply_response(&serve, old(6, 5)).unwrap();
		serving
			.apply_response(
				&serve,
				lite::SubscribeResponse::Drop(lite::SubscribeDrop::new(6..6, &crate::StreamError::Skip, version)),
			)
			.unwrap();

		{
			let Sub::Active(active) = &mut serving.sub else {
				unreachable!("an active subscription");
			};
			let ranges = active.live.unchanged();
			buffer_update(active, ranges).unwrap();
		}
		assert!(covers(1..3), "the skip is final");
		assert!(covers(4..5), "the drop is final");
		assert!(
			matches!(
				serving.serving.create_group(crate::group::Info { start: 1 }),
				Err(Error::Skipped)
			),
			"still skipped after the update"
		);
	}

	/// A SKIP replaces an earlier final DROP of the same groups too: "never produced" is the
	/// most authoritative verdict. A later DROP leaves the skip in place, and a SKIP naming a
	/// group that arrived keeps the group, without ending the session.
	#[kio::test]
	async fn a_skip_replaces_a_final_drop_but_not_a_cached_group() {
		let version = Version::Lite07;
		let (serve, mut serving, tail, _broadcast) = lite07_serving(Duration::from_millis(500)).await;
		let drop = |first, last: u64, error: &crate::StreamError| {
			lite::SubscribeResponse::Drop(lite::SubscribeDrop::new(first..last + 1, error, version))
		};
		let skip = |first, last: u64| {
			lite::SubscribeResponse::Drop(lite::SubscribeDrop::new(
				first..last + 1,
				&crate::StreamError::Skip,
				version,
			))
		};
		let mut group = serving.serving.create_group(crate::group::Info { start: 5 }).unwrap();
		group
			.write_frame(crate::Timestamp::from_millis(5).unwrap(), &b"x"[..])
			.unwrap();
		group.finish().unwrap();

		serving
			.apply_response(&serve, drop(1, 2, &crate::StreamError::App(5)))
			.unwrap();
		serving.apply_response(&serve, skip(1, 2)).unwrap();
		assert!(
			matches!(
				serving.serving.create_group(crate::group::Info { start: 2 }),
				Err(Error::Skipped)
			),
			"the skip replaced the final drop"
		);
		serving
			.apply_response(&serve, drop(2, 2, &crate::StreamError::NotFound))
			.unwrap();
		assert!(
			matches!(
				serving.serving.create_group(crate::group::Info { start: 2 }),
				Err(Error::Skipped)
			),
			"a later drop leaves the skip"
		);

		// Group 5 arrived, so the skip names only 4 and 6.
		serving.apply_response(&serve, skip(4, 6)).unwrap();
		assert!(tail.read().covers(4..7));
		let mut reader = serving
			.serving
			.subscribe(crate::track::Subscription::default().with_ranges([crate::track::Range::groups(0..)]));
		let waiter = kio::Waiter::noop();
		assert_eq!(reader.poll_drop(&waiter), Poll::Ready((1..3, track::Hole::Skip)));
		assert_eq!(reader.poll_drop(&waiter), Poll::Ready((4..5, track::Hole::Skip)));
		assert_eq!(reader.poll_drop(&waiter), Poll::Ready((6..7, track::Hole::Skip)));
		assert!(reader.poll_drop(&waiter).is_pending());
		let group = reader.recv_group().await.unwrap().unwrap();
		assert_eq!(group.start, 5, "the group that arrived is kept");
	}

	/// A SUBSCRIBE_DROP ends a group still arriving now, with its Error Code on lite-07.
	/// Earlier versions carry an application code there, so the same bytes read as
	/// OLD.
	#[kio::test]
	async fn a_subscribe_drop_ends_an_arriving_group_with_its_code() {
		for version in [Version::Lite05, Version::Lite06, Version::Lite07] {
			let mut responses = Vec::new();
			let code = crate::coding::StreamCodes::encode_stream_code(&Version::Lite07, &crate::StreamError::App(5));
			for resp in [
				lite::SubscribeResponse::Start(start_answer(version, None)),
				lite::SubscribeResponse::Drop(lite::SubscribeDrop {
					groups: 2..3,
					error: code.into(),
				}),
			] {
				resp.encode(
					&mut crate::coding::Encoder::new(&mut responses, version.into()),
					version,
				)
				.unwrap();
			}
			// No FIN: the subscription stays open, so only the DROP ends the group.
			let session = crate::lite::test_transport::ScriptedSession::new(responses);
			let subscriber = Subscriber::new(SubscriberConfig {
				runtime: crate::time::sim(),
				session: crate::driver::Transport::new(session.clone()),
				origin: origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce(),
				recv_bandwidth: None,
				version,
				peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
				peer_node: None,
				cost: None,
				going_away: Default::default(),
				auth: crate::auth::Link::new(false),
			});
			let serve = TrackServe {
				validity: Default::default(),
				route: None,
				source: None,
				subscriber,
				path: Path::from_static("room"),
				instance: None,
				node: None,
				name: "video".to_string(),
			};
			let broadcast = crate::broadcast::Info::new().produce();
			let request = broadcast.reserve_track("video").unwrap();
			let serving = ServeLoop::new(&serve, request, crate::track::Info::timed());
			let producer = serving.serving.clone();
			let mut arriving = serving.serving.create_group(group::Info { start: 2 }).unwrap();
			arriving.write_frame(arriving.key_time(), b"2".as_slice()).unwrap();
			// From the oldest group, which reads what the copy holds: a live join would wait on
			// the upstream's answer for the live edge.
			let mut reader = broadcast
				.consume()
				.track("video")
				.unwrap()
				.subscribe(crate::track::Subscription::default().with_ranges([crate::track::Range::ALL]))
				.await
				.unwrap();
			let mut group = reader.recv_group().await.unwrap().unwrap();
			assert_eq!(group.read_frame().await.unwrap().unwrap().payload.as_ref(), b"2");

			let gate = serve.gate();
			let mut running = TrackServeRun {
				serve,
				state: TrackRunState::Serve(serving),
				gate,
				datagrams: None,
				finished: None,
			};
			assert!(kio::Task::poll(&mut running, &kio::Waiter::noop()).is_pending());

			let expected = match version {
				Version::Lite07 => crate::StreamError::App(5),
				_ => crate::StreamError::Old,
			};
			let res = group.read_frame().now_or_never();
			assert!(
				matches!(&res, Some(Err(Error::Stream(err))) if *err == expected),
				"{version:?}: {res:?}"
			);

			// The track goes on: the next group still reaches the reader.
			let mut following = producer.create_group(group::Info { start: 3 }).unwrap();
			following.write_frame(following.key_time(), b"3".as_slice()).unwrap();
			let mut next = reader
				.recv_group()
				.now_or_never()
				.expect("the next group")
				.unwrap()
				.unwrap();
			assert_eq!(next.start, 3, "{version:?}");
			assert_eq!(next.read_frame().await.unwrap().unwrap().payload.as_ref(), b"3");
		}
	}

	/// A finished copy still serving fetches while our FIN waits for the publisher ends once
	/// the session no longer allows the broadcast, rather than outliving the grant.
	#[kio::test]
	async fn a_revoked_grant_ends_a_finished_copy_awaiting_its_fin() {
		let (session, _peer) = crate::transport::memory::pair(Default::default());
		let auth = crate::auth::Link::new(false);
		auth.handle().authorize(&crate::auth::Grant::all());
		let subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(session.clone()),
			origin: origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce(),
			recv_bandwidth: None,
			version: Version::Lite07,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			peer_node: None,
			cost: None,
			going_away: Default::default(),
			auth: auth.clone(),
		});
		let serve = TrackServe {
			validity: Default::default(),
			route: None,
			source: None,
			subscriber,
			path: Path::from_static("room"),
			instance: None,
			node: None,
			name: "video".to_string(),
		};
		let broadcast = crate::broadcast::Info::new().produce();
		let serve_loop = ServeLoop::new(
			&serve,
			broadcast.reserve_track("video").unwrap(),
			crate::track::Info::timed(),
		);
		let track = serve_loop.serving.clone();
		let finished = FinishedServe {
			serving: Some(serve_loop.serving),
			demand: serve_loop.demand,
			dynamic: serve_loop.dynamic,
			fetches: serve_loop.fetches,
			range_fills: RangeFills::per_run(),
			asks: Vec::new(),
			priority: 0,
			probes: Default::default(),
			placed: Default::default(),
			closed: serve_loop.closed,
		};
		// A reader holds the finished copy.
		let _reader = broadcast
			.consume()
			.track("video")
			.unwrap()
			.subscribe(None)
			.await
			.unwrap();
		let gate = serve.gate();
		let mut running = TrackServeRun {
			serve,
			state: TrackRunState::Finish {
				writer: None,
				reader: None,
				_working: crate::session::Work::default().start(),
			},
			gate,
			datagrams: None,
			finished: Some(finished),
		};

		auth.handle().authorize(&crate::auth::Grant::default());
		let waiter = kio::Waiter::noop();
		let _ = kio::Task::poll(&mut running, &waiter);
		assert!(running.finished.is_none(), "the finished copy outlived its grant");
		let closed = track.poll_closed(&waiter);
		assert!(matches!(closed, Poll::Ready(Err(Error::Unauthorized))), "{closed:?}");
	}

	/// A finished copy's reader waiting on a time its probes can't ask, the last key, is failed
	/// rather than left waiting for good.
	#[kio::test]
	async fn a_finished_copy_refuses_a_time_start_at_the_last_key() {
		let (session, _peer) = crate::transport::memory::pair(Default::default());
		let subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(session.clone()),
			origin: origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce(),
			recv_bandwidth: None,
			version: Version::Lite07,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			peer_node: None,
			cost: None,
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});
		let serve = TrackServe {
			validity: Default::default(),
			route: None,
			source: None,
			subscriber,
			path: Path::from_static("room"),
			instance: None,
			node: None,
			name: "video".to_string(),
		};
		let broadcast = crate::broadcast::Info::new().produce();
		let serve_loop = ServeLoop::new(
			&serve,
			broadcast.reserve_track("video").unwrap(),
			crate::track::Info::timed(),
		);
		let mut track = serve_loop.serving.clone();
		track.finish_at(Position::group(10)).unwrap();
		let mut finished = FinishedServe {
			serving: Some(serve_loop.serving),
			demand: serve_loop.demand,
			dynamic: serve_loop.dynamic,
			fetches: serve_loop.fetches,
			range_fills: RangeFills::per_run(),
			asks: Vec::new(),
			priority: 0,
			probes: Default::default(),
			placed: Default::default(),
			closed: serve_loop.closed,
		};
		let reader = broadcast
			.consume()
			.track("video")
			.unwrap()
			.subscribe(
				track::Subscription::default()
					.with_ranges([track::Range {
						start: crate::track::Start::Time(u64::MAX),
						end: None,
					}])
					.with_max(None),
			)
			.await
			.unwrap();
		let mut ordered = reader.ordered();
		assert!(ordered.next_item().now_or_never().is_none(), "waits on its upstream");

		finished.probe(&serve, true, &kio::Waiter::noop()).unwrap();
		assert_eq!(finished.asks, [u64::MAX], "the reader asks the last key");
		let res = ordered.next_item().now_or_never();
		assert!(
			matches!(res, Some(Err(_))),
			"{:?}",
			res.map(|res| res.map(|item| item.is_some()))
		);
	}

	/// Drive the subscriber with a peer's response bytes followed by FIN.
	async fn check_subscription_fin(version: Version, responses: Vec<u8>, clean: bool) {
		let session = crate::lite::test_transport::ScriptedSession::eof(responses);
		let origin = origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce();
		let subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(session.clone()),
			origin,
			recv_bandwidth: None,
			version,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			peer_node: None,
			cost: None,
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});
		let serve = TrackServe {
			validity: Default::default(),
			route: None,
			source: None,
			subscriber,
			path: Path::from_static("room"),
			instance: None,
			node: None,
			name: "video".to_string(),
		};
		let broadcast = crate::broadcast::Info::new().produce();
		let request = broadcast.reserve_track("video").unwrap();
		let serving = ServeLoop::new(&serve, request, crate::track::Info::timed());
		let mut reader = broadcast
			.consume()
			.track("video")
			.unwrap()
			.subscribe(None)
			.await
			.unwrap();
		let gate = serve.gate();
		let mut running = TrackServeRun {
			serve,
			state: TrackRunState::Serve(serving),
			gate,
			datagrams: None,
			finished: None,
		};
		// A finished copy stays for its reader, serving fetches; the subscription is what
		// settles.
		let run = poll_run(&mut running, &kio::Waiter::noop());
		assert!(
			matches!(running.state, TrackRunState::Done),
			"{version:?}: FIN must settle immediately"
		);
		assert_eq!(run.is_ready(), !clean, "{version:?}: only a finished copy stays");
		if clean {
			assert!(reader.recv_group().await.unwrap().is_none());
		} else {
			assert!(matches!(reader.recv_group().await, Err(Error::ProtocolViolation)));
		}
	}

	fn fin_responses(version: Version, started: bool, clean: bool) -> Vec<u8> {
		let mut responses = Vec::new();
		if started {
			lite::SubscribeResponse::Start(start_answer(version, None))
				.encode(
					&mut crate::coding::Encoder::new(&mut responses, version.into()),
					version,
				)
				.unwrap();
		}
		if clean {
			lite::SubscribeResponse::End(lite::SubscribeEnd { group: 0, error: None })
				.encode(
					&mut crate::coding::Encoder::new(&mut responses, version.into()),
					version,
				)
				.unwrap();
		}
		responses
	}

	#[kio::test]
	async fn bare_fin_requires_subscribe_end() {
		for version in [Version::Lite05, Version::Lite06, Version::Lite07] {
			for (started, clean) in [(false, false), (true, false), (false, true)] {
				check_subscription_fin(version, fin_responses(version, started, clean), clean).await;
			}
		}
	}

	#[kio::test]
	#[ignore = "requires Bun; run by just test bare-fin in interop CI"]
	async fn bare_fin_interop() {
		for version in [Version::Lite05, Version::Lite06, Version::Lite07] {
			for (started, clean) in [(false, false), (true, false), (false, true)] {
				let responses = fin_responses(version, started, clean);
				let responses =
					crate::test_interop::fin(crate::Version::from(version).alpn(), started, clean, responses);
				check_subscription_fin(version, responses, clean).await;
			}
		}
	}

	/// SUBSCRIBE_START at group 0 and SUBSCRIBE_END at 1: the publisher ended the track
	/// owing group 0.
	fn owed_responses(version: Version) -> Vec<u8> {
		let mut responses = Vec::new();
		let mut encoder = crate::coding::Encoder::new(&mut responses, version.into());
		lite::SubscribeResponse::Start(start_answer(version, None))
			.encode(&mut encoder, version)
			.unwrap();
		lite::SubscribeResponse::End(lite::SubscribeEnd { group: 1, error: None })
			.encode(&mut encoder, version)
			.unwrap();
		responses
	}

	/// Drive the subscriber with a peer's response bytes and FIN, owing a group that never
	/// arrives, then close the session: the track ends with the session's error.
	async fn check_owed_tail_death(version: Version, responses: Vec<u8>) {
		let session = crate::lite::test_transport::ScriptedSession::eof(responses);
		let subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(session.clone()),
			origin: origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce(),
			recv_bandwidth: None,
			version,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			peer_node: None,
			cost: None,
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});
		let serve = TrackServe {
			validity: Default::default(),
			route: None,
			source: None,
			subscriber,
			path: Path::from_static("room"),
			instance: None,
			node: None,
			name: "video".to_string(),
		};
		let broadcast = crate::broadcast::Info::new().produce();
		let serving = ServeLoop::new(&serve, broadcast.reserve_track("video").unwrap(), Default::default());
		let mut reader = broadcast
			.consume()
			.track("video")
			.unwrap()
			.subscribe(None)
			.await
			.unwrap();
		let gate = serve.gate();
		let mut running = TrackServeRun {
			serve,
			state: TrackRunState::Serve(serving),
			gate,
			datagrams: None,
			finished: None,
		};
		assert!(
			poll_run(&mut running, &kio::Waiter::noop()).is_pending(),
			"{version:?}: the tail still owes group 0"
		);
		crate::transport::poll::Session::close(&session, crate::SessionError::App(7).to_code(), "died");
		assert!(poll_run(&mut running, &kio::Waiter::noop()).is_ready());
		let res = reader.recv_group().await.err();
		assert!(matches!(res, Some(Error::Session(_))), "{version:?}: {res:?}");
	}

	#[kio::test]
	async fn an_owed_tail_aborts_at_session_death() {
		for version in [Version::Lite05, Version::Lite06, Version::Lite07] {
			check_owed_tail_death(version, owed_responses(version)).await;
		}
	}

	/// A failed subscription reads a stream whose header arrived to its end before it gives
	/// the track back, but only while someone holds the copy: once nobody does, a stream the
	/// publisher never finishes cannot keep the subscription.
	#[kio::test]
	async fn a_failed_subscription_drains_only_while_wanted() {
		for wanted in [true, false] {
			let version = Version::Lite06;
			let mut responses = Vec::new();
			lite::SubscribeResponse::Start(start_answer(version, None))
				.encode(
					&mut crate::coding::Encoder::new(&mut responses, version.into()),
					version,
				)
				.unwrap();
			let session = crate::lite::test_transport::ScriptedSession::new(responses);
			let subscriber = Subscriber::new(SubscriberConfig {
				runtime: crate::time::sim(),
				session: crate::driver::Transport::new(session.clone()),
				origin: origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce(),
				recv_bandwidth: None,
				version,
				peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
				peer_node: None,
				cost: None,
				going_away: Default::default(),
				auth: crate::auth::Link::new(false),
			});
			let serve = TrackServe {
				validity: Default::default(),
				route: None,
				source: None,
				subscriber,
				path: Path::from_static("room"),
				instance: None,
				node: None,
				name: "video".to_string(),
			};
			let broadcast = crate::broadcast::Info::new().produce();
			let serving = ServeLoop::new(
				&serve,
				broadcast.reserve_track("video").unwrap(),
				crate::track::Info::timed(),
			);
			let mut reader = broadcast
				.consume()
				.track("video")
				.unwrap()
				.subscribe(None)
				.await
				.unwrap();
			let gate = serve.gate();
			let mut running = TrackServeRun {
				serve,
				state: TrackRunState::Serve(serving),
				gate,
				datagrams: None,
				finished: None,
			};
			assert!(kio::Task::poll(&mut running, &kio::Waiter::noop()).is_pending());

			// A group stream whose header arrived, still being read.
			let reading = {
				let subscribes = running.serve.subscriber.subscribes.lock();
				let (_, entry) = subscribes.iter().next().expect("subscribed");
				Reading::open(&entry.tail, Some(0), running.serve.subscriber.runtime.now())
			};
			session.close(crate::lite::test_transport::Close::ResetStream(64 + 7));
			assert!(
				kio::Task::poll(&mut running, &kio::Waiter::noop()).is_pending(),
				"wanted={wanted}: the stream being read holds the track"
			);

			match wanted {
				true => {
					drop(reading);
					assert!(kio::Task::poll(&mut running, &kio::Waiter::noop()).is_ready());
					assert!(reader.recv_group().await.is_err(), "the track is given back failed");
				}
				false => {
					drop(reader);
					assert!(
						kio::Task::poll(&mut running, &kio::Waiter::noop()).is_ready(),
						"nobody holds the copy"
					);
					assert!(running.serve.subscriber.subscribes.lock().is_empty());
					drop(reading);
				}
			}
		}
	}

	/// Both runtimes abort a track whose tail the other's publisher still owed when the
	/// session died.
	#[kio::test]
	#[ignore = "requires Bun; run by just test bare-fin in interop CI"]
	async fn owed_tail_death_interop() {
		for version in [Version::Lite05, Version::Lite06, Version::Lite07] {
			let responses =
				crate::test_interop::owed_tail(crate::Version::from(version).alpn(), owed_responses(version));
			check_owed_tail_death(version, responses).await;
		}
	}

	/// The session routes a datagram to its datagram track's subscription by Subscribe ID,
	/// dropping a body past the wire cap.
	#[kio::test]
	async fn route_datagram_delivers_to_its_subscription() {
		for version in [Version::Lite05, Version::Lite06, Version::Lite07] {
			let origin = origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce();
			let subscriber = Subscriber::new(SubscriberConfig {
				runtime: crate::time::sim(),
				session: crate::driver::Transport::new(SinkSession::default()),
				origin,
				recv_bandwidth: None,
				version,
				peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
				peer_node: None,
				cost: None,
				going_away: Default::default(),
				auth: crate::auth::Link::new(false),
			});

			let producer = datagram::Info::default()
				.with_timescale(Timescale::MILLI)
				.produce("datagrams");
			let mut received = producer.consume();
			subscriber.datagrams.lock().insert(
				1,
				DatagramEntry {
					producer,
					timescale: Some(Timescale::MILLI),
				},
			);
			let mut recv = || received.poll_recv(&kio::Waiter::noop());

			let payload = |key, size| {
				lite::Datagram {
					subscribe: 1,
					sequence: key,
					timestamp: key,
					payload: bytes::Bytes::from(vec![0; size]),
				}
				.encode_bytes(version)
				.unwrap()
			};
			assert_eq!(subscriber.route_datagram(payload(1, 1)).unwrap(), None);
			let Poll::Ready(Ok(Some(datagram))) = recv() else {
				panic!("{version}: the datagram was not delivered");
			};
			assert_eq!(datagram.sequence, 1);
			assert_eq!(datagram.timestamp, Timestamp::new(1, Timescale::MILLI).unwrap());

			let max_payload = 1200 - payload(2, 0).len();
			let oversized = payload(2, max_payload + 1);
			assert_eq!(oversized.len(), 1201);
			assert_eq!(subscriber.route_datagram(oversized).unwrap(), None);
			assert!(recv().is_pending(), "oversized body was delivered");
			let boundary = payload(3, max_payload);
			assert_eq!(boundary.len(), 1200);
			assert_eq!(subscriber.route_datagram(boundary).unwrap(), None);
			assert!(matches!(recv(), Poll::Ready(Ok(Some(datagram))) if datagram.sequence == 3));

			// An unknown subscription's datagram goes nowhere, and names it.
			let mut other = payload(4, 1).to_vec();
			other[usize::from(version.has_datagram_tracks())] = 2;
			assert_eq!(subscriber.route_datagram(other.into()).unwrap(), Some(2));
			assert!(recv().is_pending());
		}
	}

	/// A lite-05 subscribe still waiting on TRACK_INFO is not in the subscribe map.
	/// Dropping that machine must reject the origin request with the session's error.
	#[kio::test]
	async fn session_death_rejects_a_track_waiting_for_info() {
		let subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(SinkSession::default()),
			origin: origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce(),
			recv_bandwidth: None,
			version: VERSION,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			peer_node: None,
			cost: None,
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});

		let broadcast = crate::broadcast::Info::new().produce();
		let mut dynamic = broadcast.requests();
		let consumer = broadcast.consume();
		let mut waiting = std::pin::pin!(consumer.track("video").unwrap().subscribe(None));
		assert!(
			futures::poll!(waiting.as_mut()).is_pending(),
			"waiting on the publisher"
		);
		let request = dynamic.requested_track().now_or_never().unwrap().expect("request");

		let run = TrackServeRun::new(
			TrackServe {
				validity: Default::default(),
				route: None,
				source: None,
				subscriber: subscriber.clone(),
				path: Path::from_static("room"),
				instance: None,
				node: None,
				name: "video".to_string(),
			},
			request,
		);
		let death = Error::Session(crate::SessionError::App(7));
		subscriber.note_end(&death);
		drop(run);

		let ended = waiting.now_or_never().expect("subscribe must end");
		assert!(
			matches!(ended, Err(Error::Session(crate::SessionError::App(7)))),
			"waiting track did not end with the session error"
		);
	}

	#[kio::test]
	async fn announce_interest_waits_for_credit() {
		for version in [Version::Lite04, Version::Lite06] {
			let origin = origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce();
			let _interest = origin.consume().routes();
			let session = crate::lite::test_transport::ScriptedSession::new(Vec::new()).without_credit();
			let log = session.log.clone();
			let subscriber = Subscriber::new(SubscriberConfig {
				runtime: crate::time::sim(),
				session: crate::driver::Transport::new(session.clone()),
				origin,
				recv_bandwidth: None,
				version,
				peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
				cost: Some(1),
				peer_node: None,
				going_away: Default::default(),
				auth: crate::auth::Link::new(false),
			});
			let mut driver = SubscriberDriver::new(subscriber, Vec::new(), false, route_stream::Publish::idle());
			assert!(driver.poll(&kio::Waiter::noop()).is_pending());
			kio::clock::sleep(std::time::Duration::from_secs(20)).await;
			assert!(driver.poll(&kio::Waiter::noop()).is_pending());
			assert_eq!(log.bi_opens(), 0, "{version}: interest waits without a stream");
			assert!(log.closes().is_empty(), "{version}: waiting keeps the session");

			session.grant_credit();
			let _ = driver.poll(&kio::Waiter::noop());
			assert_eq!(log.bi_opens(), 1, "{version}: interest opens once credit returns");
		}
	}

	#[kio::test]
	async fn a_claimed_fetch_waits_only_while_wanted() {
		for version in [Version::Lite05, Version::Lite06, Version::Lite07] {
			for end in ["credit", "unused", "retired"] {
				let session = crate::lite::test_transport::ScriptedSession::new(Vec::new()).without_credit();
				let subscriber = Subscriber::new(SubscriberConfig {
					runtime: crate::time::sim(),
					session: crate::driver::Transport::new(session.clone()),
					origin: origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce(),
					recv_bandwidth: None,
					version,
					peer_setup: lite::PeerSetup::new(false, crate::transport::Address::Setup),
					peer_node: None,
					cost: None,
					going_away: Default::default(),
					auth: crate::auth::Link::new(false),
				});
				let route = kio::Producer::new(());
				let serve = TrackServe {
					validity: crate::model::broadcast::RequestValidity::token(Some(route.consume())),
					route: None,
					source: None,
					subscriber,
					path: Path::from_static("room"),
					instance: None,
					node: None,
					name: "video".into(),
				};
				let info = track::Info::default().with_timescale(None);
				let track = track::Producer::new(Arc::new(crate::broadcast::Info::default()), "video", info.clone());
				let dynamic = track.requests();
				let fetch = track.consume().fetch_group(4, None);
				let request = dynamic.requested_fill_group().await.unwrap();
				let mut run = FetchServeRun::new(serve, request, FetchInfo::Known(info));
				let waiter = kio::Waiter::noop();
				assert!(kio::Task::poll(&mut run, &waiter).is_pending(), "{version}: {end}");
				assert_eq!(session.log.bi_opens(), 0);
				if end == "credit" {
					session.grant_credit();
					assert!(kio::Task::poll(&mut run, &waiter).is_pending());
					assert_eq!(session.log.bi_opens(), 1, "credit releases the FETCH");
				}
				if end == "retired" {
					drop(route);
					assert!(kio::Task::poll(&mut run, &waiter).is_ready());
					assert!(matches!(fetch.await, Err(Error::Unroutable)));
				} else {
					drop(fetch);
					assert!(kio::Task::poll(&mut run, &waiter).is_ready(), "{version}: {end}");
				}
				drop(run);
				// An abandoned FETCH says so on both halves, so the publisher serving it reads a
				// cancellation rather than a fault.
				let cancelled = match end {
					"credit" => vec![crate::StreamError::Cancel.to_code()],
					_ => Vec::new(),
				};
				assert_eq!(session.log.resets(), cancelled, "{version}: {end}");
				assert_eq!(session.log.stops(), cancelled, "{version}: {end}");
			}
		}
	}

	/// A track still waiting on TRACK_INFO ends once nobody wants it, whether its TRACK
	/// transport header never finished, its request is stuck in
	/// send, or its peer never answers.
	/// Otherwise every request a relay's front abandons (a failover, a reader leaving)
	/// holds a task, a stream, and the track until the peer answers, which may be never.
	#[kio::test]
	async fn an_unused_track_stops_waiting_for_info() {
		// A closed gate holds the TRACK request in its send; an open one sends it to a
		// peer that never answers.
		let (closed, open) = (kio::Producer::new(false), kio::Producer::new(true));
		for (stage, session) in [
			("open", SinkSession::default()),
			("send", SinkSession::gated_bi(closed.consume())),
			("read", SinkSession::gated_bi(open.consume())),
		] {
			let subscriber = Subscriber::new(SubscriberConfig {
				runtime: crate::time::sim(),
				session: crate::driver::Transport::new(session.clone()),
				origin: origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce(),
				recv_bandwidth: None,
				version: VERSION,
				peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
				peer_node: None,
				cost: None,
				going_away: Default::default(),
				auth: crate::auth::Link::new(false),
			});

			let broadcast = crate::broadcast::Info::new().produce();
			let mut dynamic = broadcast.requests();
			let consumer = broadcast.consume();
			let mut waiting = Box::pin(consumer.track("video").unwrap().subscribe(None));
			assert!(
				futures::poll!(waiting.as_mut()).is_pending(),
				"{stage}: waiting on the publisher"
			);
			let request = dynamic.requested_track().now_or_never().unwrap().expect("request");

			let mut run = TrackServeRun::new(
				TrackServe {
					validity: Default::default(),
					route: None,
					source: None,
					subscriber,
					path: Path::from_static("room"),
					instance: None,
					node: None,
					name: "video".to_string(),
				},
				request,
			);
			let waiter = kio::Waiter::noop();
			assert!(
				kio::Task::poll(&mut run, &waiter).is_pending(),
				"{stage}: waits while the track is wanted"
			);

			drop(waiting);
			assert!(
				kio::Task::poll(&mut run, &waiter).is_ready(),
				"{stage}: an unused track kept waiting on TRACK_INFO"
			);
		}
	}

	/// A subscriber on `version` over `session`, whose peer's SETUP was already read.
	fn pipelining(session: crate::driver::Transport, version: Version) -> Subscriber {
		let peer_setup = crate::lite::PeerSetup::new(false, crate::transport::Address::Setup);
		peer_setup.set(lite::Setup::default()).unwrap();
		Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session,
			origin: origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce(),
			recv_bandwidth: None,
			version,
			peer_setup,
			peer_node: None,
			cost: None,
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		})
	}

	/// A subscription to `video` over `subscriber`, waiting on the run that serves it.
	fn subscribing(
		subscriber: &Subscriber,
		subscription: Subscription,
	) -> (kio::Pending<crate::track::Subscribing>, TrackServeRun) {
		let broadcast = crate::broadcast::Info::new().produce();
		let mut dynamic = broadcast.requests();
		let mut waiting = broadcast.consume().track("video").unwrap().subscribe(subscription);
		assert!((&mut waiting).now_or_never().is_none(), "waiting on the publisher");
		let request = dynamic.requested_track().now_or_never().unwrap().expect("request");
		let serve = TrackServe {
			validity: Default::default(),
			route: None,
			source: None,
			subscriber: subscriber.clone(),
			path: Path::from_static("room"),
			instance: None,
			node: None,
			name: "video".to_string(),
		};
		(waiting, TrackServeRun::new(serve, request))
	}

	/// What `session` wrote on its `stream`th send stream.
	fn sent(log: &crate::lite::test_transport::Log, stream: usize) -> Vec<u8> {
		log.trail()
			.into_iter()
			.filter(|(id, _)| *id == stream)
			.filter_map(|(_, sent)| match sent {
				crate::lite::test_transport::Sent::Write(bytes) => Some(bytes),
				_ => None,
			})
			.flatten()
			.collect()
	}

	/// The SUBSCRIBE a request stream carried, and whether anything followed it.
	fn sent_subscribe(wire: &[u8], version: Version) -> (lite::Subscribe<'static>, bool) {
		let mut wire = wire;
		assert_eq!(
			crate::coding::decode_buf(&mut wire, version, |r, v| lite::ControlType::decode(r, v)).unwrap(),
			lite::ControlType::Subscribe
		);
		let msg = crate::coding::decode_from(&mut wire, version, lite::Subscribe::decode).unwrap();
		(msg.into_owned(), !wire.is_empty())
	}

	/// On lite-05 and later the SUBSCRIBE goes out beside its TRACK, registered before the
	/// TRACK_INFO its copy is accepted with.
	#[kio::test]
	async fn a_subscribe_goes_out_with_its_track() {
		for version in [Version::Lite05, Version::Lite06, Version::Lite07] {
			let session = crate::lite::test_transport::ScriptedSession::per_stream(vec![Vec::new(), Vec::new()]);
			let subscriber = pipelining(crate::driver::Transport::new(session.clone()), version);
			let (mut waiting, mut run) = subscribing(&subscriber, Subscription::default().with_priority(3));
			assert!(kio::Task::poll(&mut run, &kio::Waiter::noop()).is_pending());

			let track = sent(&session.log, 0);
			let mut track = track.as_slice();
			assert_eq!(
				crate::coding::decode_buf(&mut track, version, |r, v| lite::ControlType::decode(r, v)).unwrap(),
				lite::ControlType::Track,
				"{version}: the TRACK opens first"
			);
			let (msg, trailing) = sent_subscribe(&sent(&session.log, 1), version);
			assert_eq!((msg.id, msg.track.as_ref(), msg.priority), (0, "video", 3), "{version}");
			assert!(!trailing, "{version}");
			assert!(
				subscriber
					.subscribes
					.lock()
					.get(&0)
					.is_some_and(|entry| !entry.is_ready()),
				"{version}: registered with its copy pending"
			);
			assert!((&mut waiting).now_or_never().is_none(), "{version}: no TRACK_INFO yet");
		}
	}

	/// An in-flight TRACK survives withdrawal, like its pipelined SUBSCRIBE, but
	/// never accepts a replacement instance's info.
	#[kio::test]
	async fn an_in_flight_track_info_keeps_its_instance() {
		for version in [Version::Lite05, Version::Lite06, Version::Lite07] {
			for replace in [false, true] {
				let session = crate::lite::test_transport::ScriptedSession::new(Vec::new());
				let subscriber = pipelining(crate::driver::Transport::new(session.clone()), version);
				let (_waiting, mut run) = subscribing(&subscriber, Subscription::default());
				let route = kio::Producer::new(());
				run.serve.route = Some(route.consume());
				let mut selection = crate::model::TestSelection::new();
				run.serve.validity = selection.validity();
				let mut fetch = TrackInfoFetch::new(&run.serve);
				let waiter = kio::Waiter::noop();
				assert!(fetch.poll_fetch(&run.serve, true, &waiter).is_pending());
				drop(route);
				selection.withdraw();
				if replace {
					selection.replace();
				} else {
					assert!(
						fetch.poll_fetch(&run.serve, true, &waiter).is_pending(),
						"{version}: the request already went out"
					);
				}

				let mut info = Vec::new();
				lite::TrackInfo {
					refresh: crate::track::Refresh::default(),
					priority: 0,
					max_age: None,
					timescale: Some(Timescale::MILLI),
					compression: crate::track::Compression::None,
				}
				.encode(&mut crate::coding::Encoder::new(&mut info, version.into()), version)
				.unwrap();
				session.push(&info);
				let result = fetch.poll_fetch(&run.serve, true, &waiter);
				if replace {
					assert!(matches!(result, Poll::Ready(Err(Error::Unroutable))), "{version}");
				} else {
					assert!(matches!(result, Poll::Ready(Ok(_))), "{version}");
				}
			}
		}
	}

	/// Accepting the TRACK_INFO sends no SUBSCRIBE_UPDATE, even when the publisher keeps
	/// groups for less than the subscriber waits: the publisher applies its own max age.
	#[kio::test]
	async fn accepting_track_info_sends_no_subscribe_update() {
		for version in [Version::Lite05, Version::Lite06, Version::Lite07] {
			let mut info = Vec::new();
			lite::TrackInfo {
				refresh: crate::track::Refresh::default(),
				priority: 0,
				max_age: Some(Duration::from_secs(1)),
				timescale: Some(Timescale::MILLI),
				compression: crate::track::Compression::None,
			}
			.encode(&mut crate::coding::Encoder::new(&mut info, version.into()), version)
			.unwrap();
			let session = crate::lite::test_transport::ScriptedSession::per_stream(vec![info, Vec::new()]);
			let subscriber = pipelining(crate::driver::Transport::new(session.clone()), version);
			let wait = Duration::from_secs(10);
			let (waiting, mut run) = subscribing(&subscriber, Subscription::default().with_max(wait));
			for _ in 0..4 {
				let _ = kio::Task::poll(&mut run, &kio::Waiter::noop());
			}
			waiting.now_or_never().expect("accepted").unwrap();

			let (msg, trailing) = sent_subscribe(&sent(&session.log, 1), version);
			// Lite-07 carries it on the live join, an older wire as the subscription's.
			let max = match version.has_ranges() {
				true => match msg.ranges[..] {
					[
						lite::Range {
							start: Some(lite::Anchor::Live { max, .. }),
							..
						},
					] => max,
					_ => panic!("{version}: one live join, got {:?}", msg.ranges),
				},
				false => msg.max_delay,
			};
			assert_eq!(max, Some(wait), "{version}: the subscriber's own max age");
			assert!(!trailing, "{version}: a SUBSCRIBE_UPDATE followed the accept");
		}
	}

	/// A wire before lite-07 carries the demand's widest live max as its one Subscriber Max
	/// Age, even once its span narrows to a start, here the newest group an idle copy cached.
	#[kio::test]
	async fn a_span_narrowed_to_a_start_keeps_the_demands_max_age() {
		for version in [Version::Lite05, Version::Lite06] {
			let mut h = Harness::new(version);
			let mut cached = h.producer.create_group(group::Info { start: 3 }).unwrap();
			cached.write_frame(cached.key_time(), &b"x"[..]).unwrap();
			cached.finish().unwrap();
			h.producer.set_idle();

			let wait = Duration::from_secs(10);
			let mut sub = Sub::None;
			let demand = Subscription::default().with_max(wait);
			h.serve
				.handle_subscription(&mut h.producer, &mut sub, Some(demand), true)
				.await
				.unwrap();

			let (msg, _) = sent_subscribe(&h.wire(), version);
			assert!(
				matches!(
					msg.ranges[..],
					[lite::Range {
						start: Some(lite::Anchor::Absolute(position)),
						..
					}] if position.group == 3
				),
				"{version}: asked from the newest cached group, got {:?}",
				msg.ranges
			);
			assert_eq!(msg.max_delay, Some(wait), "{version}: the demand's max age");
		}
	}

	/// A TRACK reset while its SUBSCRIBE is live rejects the request, so the origin can try
	/// another route, and resets the SUBSCRIBE.
	#[kio::test]
	async fn a_reset_track_rejects_the_request_and_resets_its_subscribe() {
		for version in [Version::Lite05, Version::Lite06, Version::Lite07] {
			let session = crate::lite::test_transport::ScriptedSession::per_stream_reset(vec![Vec::new(), Vec::new()]);
			let subscriber = pipelining(crate::driver::Transport::new(session.clone()), version);
			let (waiting, mut run) = subscribing(&subscriber, Subscription::default());
			assert!(kio::Task::poll(&mut run, &kio::Waiter::noop()).is_ready());

			assert!(waiting.now_or_never().expect("rejected").is_err(), "{version}");
			assert!(
				session
					.log
					.trail()
					.iter()
					.any(|(stream, sent)| *stream == 1 && matches!(sent, crate::lite::test_transport::Sent::Reset(_))),
				"{version}: the SUBSCRIBE was not reset"
			);
			assert!(subscriber.subscribes.lock().is_empty(), "{version}");
		}
	}

	/// A group stream that beats its subscription's TRACK_INFO waits after its header,
	/// unread, and the accept opens the groups that waited in the order they arrived.
	#[kio::test]
	async fn a_group_that_beats_its_track_info_waits_for_it() {
		let version = Version::Lite07;
		let group = |key: u64, payload: u8| {
			let mut bytes = Vec::new();
			let mut w = crate::coding::Encoder::new(&mut bytes, version.into());
			lite::DataType::Group.encode(&mut w, version).unwrap();
			lite::Group {
				subscribe: 0,
				key,
				frame_start: 0,
			}
			.encode(&mut w, version)
			.unwrap();
			// A one-byte payload's length plus one, then the frame at the group's key, since
			// starts strictly increase.
			w.varint(2).unwrap();
			w.varint(crate::coding::varint::zigzag(key as i64)).unwrap();
			w.u8(payload);
			bytes
		};
		let session = crate::lite::test_transport::ScriptedSession::eof(Vec::new())
			.with_incoming_unis(vec![group(3, 30), group(2, 20)]);
		let subscriber = pipelining(crate::driver::Transport::new(session.clone()), version);
		subscriber
			.subscribes
			.lock()
			.insert(0, TrackEntry::new(None, Default::default()));
		let mut accept = UniAccept::new(subscriber.clone(), Vec::new());
		assert!(accept.poll(&kio::Waiter::noop()).is_pending());
		assert_eq!(subscriber.park_waits.load(atomic::Ordering::Relaxed), 2);
		assert!(session.log.stops().is_empty(), "a parked stream is kept");

		let track = crate::track::Producer::new(
			Arc::new(crate::broadcast::Info::default()),
			"video",
			crate::track::Info::default()
				.with_timescale(Timescale::MILLI)
				.with_max_age(None),
		);
		// Every group, oldest first, however late.
		let every = Subscription::default()
			.with_order(crate::track::Order::Ascending)
			.with_start(0)
			.with_max(Duration::from_secs(60));
		let mut consumer = track.consume().subscribe(every).await.unwrap();
		let framing = lite::Framing::new(track.info(), version);
		subscriber.subscribes.lock().get_mut(&0).unwrap().accept(
			Accepted {
				producer: track.clone(),
				framing,
			},
			version,
			subscriber.runtime.now(),
		);
		// Both opened at the accept, the first to arrive first.
		assert_eq!(track.latest_key(), Some(3));

		assert!(accept.poll(&kio::Waiter::noop()).is_pending());
		assert_eq!(subscriber.park_waits.load(atomic::Ordering::Relaxed), 0);
		let mut read = Vec::new();
		for _ in 0..2 {
			let mut group = consumer.recv_group().await.unwrap().unwrap();
			read.push((group.start, group.read_frame().await.unwrap().unwrap().payload[0]));
		}
		read.sort_unstable();
		assert_eq!(read, [(2, 20), (3, 30)]);
		assert!(
			subscriber.subscribes.lock().get(&0).unwrap().parked.is_none(),
			"released once every stream took its group"
		);
	}

	/// Group streams accepted together open their groups in the order they arrived, even
	/// once earlier streams freed places in the accept's task set, so a reader following the
	/// live edge gets none of them late.
	#[kio::test]
	async fn a_burst_of_group_streams_opens_its_groups_in_order() {
		let version = Version::Lite07;
		let group = |key: u64| {
			let mut bytes = Vec::new();
			let mut w = crate::coding::Encoder::new(&mut bytes, version.into());
			lite::DataType::Group.encode(&mut w, version).unwrap();
			lite::Group {
				subscribe: 0,
				key,
				frame_start: 0,
			}
			.encode(&mut w, version)
			.unwrap();
			w.varint(2).unwrap();
			w.varint(crate::coding::varint::zigzag(key as i64)).unwrap();
			w.u8(key as u8);
			bytes
		};
		// Streams ending before their type finish at once and free their places.
		let session =
			crate::lite::test_transport::ScriptedSession::eof(Vec::new()).with_incoming_unis(vec![Vec::new(); 3]);
		let subscriber = pipelining(crate::driver::Transport::new(session.clone()), version);
		let track = crate::track::Producer::new(
			Arc::new(crate::broadcast::Info::default()),
			"video",
			crate::track::Info::default()
				.with_timescale(Timescale::MILLI)
				.with_max_age(None),
		);
		let live = Subscription::default().with_max(Duration::from_secs(60));
		let mut consumer = track.consume().subscribe(live).await.unwrap();
		let mut entry = TrackEntry::new(None, Default::default());
		let framing = lite::Framing::new(track.info(), version);
		entry.accept(
			Accepted {
				producer: track.clone(),
				framing,
			},
			version,
			subscriber.runtime.now(),
		);
		subscriber.subscribes.lock().insert(0, entry);
		let mut accept = UniAccept::new(subscriber.clone(), Vec::new());
		assert!(accept.poll(&kio::Waiter::noop()).is_pending());

		for key in 1..=3 {
			session.push_incoming_uni(group(key));
		}
		assert!(accept.poll(&kio::Waiter::noop()).is_pending());
		let mut read = Vec::new();
		for _ in 0..3 {
			read.push(consumer.recv_group().await.unwrap().unwrap().start);
		}
		assert_eq!(track.arrival_order(), [1, 2, 3], "opened in arrival order");
		// Read newest first, the default Group Order.
		assert_eq!(read, [3, 2, 1]);
	}

	/// A parked stream that fails before taking its group withdraws it: the accept opens none
	/// for it, or the group it opened ends with the stream's error, and the rest still open.
	#[kio::test]
	async fn a_failed_parked_stream_withdraws_its_group() {
		let version = Version::Lite07;
		let header = |key| lite::Group {
			subscribe: 0,
			key,
			frame_start: 0,
		};
		let now = crate::time::sim().now();
		let mut entry = TrackEntry::new(None, Default::default());
		let (_early_wait, early) = entry.park(&header(1), now);
		let (_late_wait, late) = entry.park(&header(2), now);
		let (_kept_wait, kept) = entry.park(&header(3), now);
		entry.withdraw(early, &Error::Cancel);

		let track = crate::track::Producer::new(
			Arc::new(crate::broadcast::Info::default()),
			"video",
			crate::track::Info::default().with_timescale(Timescale::MILLI),
		);
		let every = Subscription::default()
			.with_order(crate::track::Order::Ascending)
			.with_start(0)
			.with_max(Duration::from_secs(60));
		let mut consumer = track.consume().subscribe(every).await.unwrap();
		let framing = lite::Framing::new(track.info(), version);
		entry.accept(
			Accepted {
				producer: track.clone(),
				framing,
			},
			version,
			crate::time::sim().now(),
		);
		let mut opened: Vec<u64> = match entry.parked.as_ref().unwrap().write() {
			Ok(state) => state.opened.keys().copied().collect(),
			Err(_) => panic!("the entry holds the parked streams"),
		};
		opened.sort_unstable();
		assert_eq!(opened, [late, kept], "nothing opened for the withdrawn header");

		entry.withdraw(late, &Error::Evicted);
		let opened = match entry.parked.as_ref().unwrap().write() {
			Ok(state) => state.opened.keys().copied().collect::<Vec<_>>(),
			Err(_) => panic!("the entry holds the parked streams"),
		};
		assert_eq!(opened, [kept], "the withdrawn group is gone");
		let Ok(Open::Group(opened)) = entry.unpark(kept) else {
			panic!("the kept group");
		};
		opened.group.finish().unwrap();
		let _track = track.finish().unwrap();
		// Only the kept group reaches the subscriber: the withdrawn one ended, so nothing
		// waits on it.
		let mut keys = Vec::new();
		while let Some(group) = consumer.recv_group().await.unwrap() {
			keys.push(group.start);
		}
		assert_eq!(keys, [3]);
		assert!(entry.parked.is_none(), "released once the last stream took its group");
		// The withdrawn parked header (key 1) was still accounted in the tail at park time.
		assert!(
			entry.tail.read().covers(1..2),
			"withdrawn parked stream kept its key accounted"
		);
	}

	/// TRACK opens its stream before the fetches sent alongside it: TRACK fails fast without
	/// stream credit, where a fetch waits for it, so fetches opened first could leave TRACK
	/// none and fail the track with every fetch.
	#[kio::test]
	async fn track_opens_before_the_fetches_sent_alongside() {
		let session = SinkSession::gated_bi(kio::Producer::new(true).consume());
		let log = session.log.clone();
		let subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(session),
			origin: origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce(),
			recv_bandwidth: None,
			version: VERSION,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			peer_node: None,
			cost: None,
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});

		let broadcast = crate::broadcast::Info::new().produce();
		let mut dynamic = broadcast.requests();
		let consumer = broadcast.consume();
		let _waiting = Box::pin(consumer.track("video").unwrap().subscribe(None));
		let request = dynamic.requested_track().await.expect("request");
		let copy = request.consume();

		let mut run = TrackServeRun::new(
			TrackServe {
				validity: Default::default(),
				route: None,
				source: None,
				subscriber,
				path: Path::from_static("room"),
				instance: None,
				node: None,
				name: "video".to_string(),
			},
			request,
		);
		// Queued before the run first polls, as several fetches can be.
		let mut fetching = Box::pin(copy.fetch_group(0, None));
		assert!(futures::poll!(fetching.as_mut()).is_pending());
		assert!(kio::Task::poll(&mut run, &kio::Waiter::noop()).is_pending());

		let first = |control: lite::ControlType| {
			log.trail().into_iter().find_map(|(stream, sent)| match sent {
				Sent::Write(bytes) if bytes.first() == Some(&(u64::from(control) as u8)) => Some(stream),
				_ => None,
			})
		};
		let track = first(lite::ControlType::Track).expect("TRACK went out");
		let fetch = first(lite::ControlType::Fetch).expect("FETCH went out alongside");
		assert!(track < fetch, "a FETCH opened its stream before TRACK");
	}

	/// The front reads whether a source is closing from its broadcast, and takes a failure from a
	/// source still open as the track's verdict. So a run that fails its copy, or the query
	/// still waiting on TRACK_INFO, with the session's end closes the source first: the front
	/// then waits for the next route rather than end a track its relay still shows.
	#[kio::test]
	async fn a_track_failed_with_its_session_closes_its_source_first() {
		use crate::transport::poll::Session as _;
		// Lite04 serves at once; Lite05 waits on TRACK_INFO.
		for version in [Version::Lite04, Version::Lite05] {
			let (session, peer) = crate::transport::memory::pair(Default::default());
			let subscriber = pipelining(crate::driver::Transport::new(session.clone()), version);
			let source = crate::broadcast::Info::new().produce();
			let consumer = source.consume();
			let mut dynamic = source.requests();
			let mut waiting = consumer.track("video").unwrap().subscribe(None);
			assert!((&mut waiting).now_or_never().is_none(), "waiting on the publisher");
			let request = dynamic.requested_track().now_or_never().unwrap().expect("request");
			let mut run = TrackServeRun::new(
				TrackServe {
					validity: Default::default(),
					route: None,
					source: Some(source.clone()),
					subscriber,
					path: Path::from_static("room"),
					instance: None,
					node: None,
					name: "video".to_string(),
				},
				request,
			);
			assert!(kio::Task::poll(&mut run, &kio::Waiter::noop()).is_pending());
			assert!(!consumer.is_closed(), "{version:?}: closed before the session ended");

			peer.close(crate::SessionError::Cancel.to_code(), "gone");
			for _ in 0..4 {
				let _ = kio::Task::poll(&mut run, &kio::Waiter::noop());
			}
			let ended = match (&mut waiting).now_or_never().expect("the track ended") {
				Ok(mut copy) => copy.recv_group().now_or_never().expect("the copy ended").err(),
				Err(err) => Some(err),
			};
			assert!(ended.is_some(), "{version:?}: the track ended cleanly");
			assert!(
				consumer.is_closed(),
				"{version:?}: failed the track before closing its source"
			);
		}
	}

	/// The session's end aborts every subscription still open, so it closes every source first,
	/// for the same reason a run does.
	#[kio::test]
	async fn the_session_end_closes_its_sources_before_its_subscriptions() {
		let (session, _peer) = crate::transport::memory::pair(Default::default());
		let subscriber = pipelining(crate::driver::Transport::new(session), Version::Lite07);
		let source = crate::broadcast::Info::new().produce();
		let consumer = source.consume();
		let route = kio::Producer::new(());
		let _ = subscriber.sources.try_push(MintedSource {
			path: Path::from_static("room"),
			instance: None,
			node: None,
			dynamic: source.requests(),
			source: crate::model::broadcast::SourceGuard::new(source),
			route: route.consume(),
		});
		let mut driver = SubscriberDriver::new(subscriber, Vec::new(), false, route_stream::Publish::idle());
		let _ = driver.poll(&kio::Waiter::noop());
		assert_eq!(driver.sources.len(), 1, "the driver took the source");
		assert!(!consumer.is_closed());
		driver.abort(&Error::IdleTimeout);
		assert!(
			consumer.is_closed(),
			"aborted its subscriptions before closing its sources"
		);
		drop(route);
	}

	/// A lite-07 source of `room` over `session`, a consumer to list it through, and the
	/// route that keeps it served.
	fn listed_source(
		session: SinkSession,
		auth: crate::auth::Link,
		validity: crate::model::broadcast::RequestValidity,
	) -> (SourceServe, crate::broadcast::Consumer, kio::Producer<()>) {
		let subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(session.clone()),
			origin: origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce(),
			recv_bandwidth: None,
			version: Version::Lite07,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			peer_node: None,
			cost: None,
			going_away: Default::default(),
			auth,
		});
		let broadcast = crate::broadcast::Info::new().produce().with_validity(validity);
		let consumer = broadcast.consume();
		let dynamic = broadcast.requests();
		let route = kio::Producer::new(());
		let serve = SourceServe::new(
			subscriber,
			MintedSource {
				path: Path::from_static("room"),
				instance: None,
				node: None,
				source: crate::model::broadcast::SourceGuard::new(broadcast),
				dynamic,
				route: route.consume(),
			},
		);
		(serve, consumer, route)
	}

	/// A listing is a subscribe: it waits for a token that may cover the broadcast before
	/// asking the peer, which would refuse it until then. Its readers sync only when the
	/// peer does.
	#[kio::test]
	async fn a_listing_waits_for_its_token() {
		let auth = crate::auth::Link::new(true);
		let token = auth.present(bytes::Bytes::from_static(b"new"), false).unwrap();
		let open = kio::Producer::new(true);
		let session = SinkSession::gated_bi(open.consume());
		let (mut serve, consumer, _route) = listed_source(session.clone(), auth, Default::default());
		let mut tracks = consumer.tracks().unwrap();

		assert!(kio::Task::poll(&mut serve, &kio::Waiter::noop()).is_pending());
		assert_eq!(session.log.bi_opens(), 0, "asked the peer before the token settled");
		assert!(
			tracks.next().now_or_never().is_none(),
			"synced before the token settled"
		);

		drop(token);
		assert!(kio::Task::poll(&mut serve, &kio::Waiter::noop()).is_pending());
		assert_eq!(session.log.bi_opens(), 1);
		assert!(tracks.next().now_or_never().is_none(), "synced before the peer");
	}

	/// An instance retired while its listing waits for a token lists nothing: a TRACKS
	/// request under another instance would list its replacement. The source ends with it.
	#[kio::test]
	async fn a_listing_of_a_retired_instance_never_reaches_the_wire() {
		let auth = crate::auth::Link::new(true);
		let token = auth.present(bytes::Bytes::from_static(b"new"), false).unwrap();
		let open = kio::Producer::new(true);
		let session = SinkSession::gated_bi(open.consume());
		let selected = kio::Producer::new(());
		let validity = crate::model::broadcast::RequestValidity::Route(selected.consume());
		let (mut serve, consumer, _route) = listed_source(session.clone(), auth, validity);
		let mut tracks = consumer.tracks().unwrap();

		assert!(kio::Task::poll(&mut serve, &kio::Waiter::noop()).is_pending());
		drop(selected);
		drop(token);
		assert!(kio::Task::poll(&mut serve, &kio::Waiter::noop()).is_ready());
		assert!(matches!(tracks.next().now_or_never(), Some(Err(Error::Unroutable))));
		assert_eq!(session.log.bi_opens(), 0);
	}

	/// A listing on the wire but unanswered when another instance takes the path lists
	/// nothing: the peer may list that instance.
	#[kio::test]
	async fn an_unanswered_listing_of_a_replaced_instance_is_refused() {
		let open = kio::Producer::new(true);
		let session = SinkSession::gated_bi(open.consume());
		let mut selection = crate::model::TestSelection::new();
		let (mut serve, consumer, _route) =
			listed_source(session.clone(), crate::auth::Link::new(false), selection.validity());
		let mut tracks = consumer.tracks().unwrap();

		assert!(kio::Task::poll(&mut serve, &kio::Waiter::noop()).is_pending());
		assert_eq!(session.log.bi_opens(), 1);
		assert!(tracks.next().now_or_never().is_none());

		selection.replace();
		let _ = kio::Task::poll(&mut serve, &kio::Waiter::noop());
		assert!(matches!(tracks.next().now_or_never(), Some(Err(Error::Unroutable))));
		assert_eq!(session.log.resets(), vec![crate::StreamError::Unroutable.to_code()]);
	}

	/// A listing outside the session's scope is refused before it reaches the wire.
	#[kio::test]
	async fn a_listing_outside_the_scope_is_refused() {
		let auth = crate::auth::Link::new(false);
		auth.handle().authorize(&crate::auth::Grant::default());
		let open = kio::Producer::new(true);
		let session = SinkSession::gated_bi(open.consume());
		let (mut serve, consumer, _route) = listed_source(session.clone(), auth, Default::default());
		let mut tracks = consumer.tracks().unwrap();

		assert!(kio::Task::poll(&mut serve, &kio::Waiter::noop()).is_pending());
		assert!(matches!(tracks.next().now_or_never(), Some(Err(Error::Unauthorized))));
		assert_eq!(session.log.bi_opens(), 0);
	}

	/// A narrower scope resets a live listing with UNAUTHORIZED rather than keep listing
	/// what the session no longer allows, and leaves the session up.
	#[kio::test]
	async fn a_narrower_limit_resets_a_listing() {
		let auth = crate::auth::Link::new(false);
		let open = kio::Producer::new(true);
		let session = SinkSession::gated_bi(open.consume());
		let (mut serve, consumer, _route) = listed_source(session.clone(), auth.clone(), Default::default());
		let mut tracks = consumer.tracks().unwrap();

		assert!(kio::Task::poll(&mut serve, &kio::Waiter::noop()).is_pending());
		assert_eq!(session.log.bi_opens(), 1);
		assert!(tracks.next().now_or_never().is_none());

		auth.handle().authorize(&crate::auth::Grant::default());
		assert!(kio::Task::poll(&mut serve, &kio::Waiter::noop()).is_pending());
		assert!(matches!(tracks.next().now_or_never(), Some(Err(Error::Unauthorized))));
		assert_eq!(session.log.resets(), vec![crate::StreamError::Unauthorized.to_code()]);
		assert!(session.log.closes().is_empty());
	}

	/// A track waiting on an unanswered token ends once nobody wants it, rather than
	/// holding its task and track until a token that may never be answered.
	#[kio::test]
	async fn an_unused_track_stops_waiting_for_auth() {
		let auth = crate::auth::Link::new(true);
		let _token = auth.present(bytes::Bytes::from_static(b"new"), false).unwrap();
		let subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(SinkSession::default()),
			origin: origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce(),
			recv_bandwidth: None,
			version: VERSION,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			peer_node: None,
			cost: None,
			going_away: Default::default(),
			auth,
		});

		let broadcast = crate::broadcast::Info::new().produce();
		let mut dynamic = broadcast.requests();
		let consumer = broadcast.consume();
		let mut waiting = Box::pin(consumer.track("video").unwrap().subscribe(None));
		assert!(futures::poll!(waiting.as_mut()).is_pending());
		let request = dynamic.requested_track().now_or_never().unwrap().expect("request");

		let mut run = TrackServeRun::new(
			TrackServe {
				validity: Default::default(),
				route: None,
				source: None,
				subscriber,
				path: Path::from_static("room"),
				instance: None,
				node: None,
				name: "video".to_string(),
			},
			request,
		);
		let waiter = kio::Waiter::noop();
		assert!(
			kio::Task::poll(&mut run, &waiter).is_pending(),
			"waits on the token while the track is wanted"
		);

		drop(waiting);
		assert!(
			kio::Task::poll(&mut run, &waiter).is_ready(),
			"an unused track kept waiting on its token"
		);
	}

	/// `establish` puts exactly one SUBSCRIBE on the wire, and the id is registered
	/// before any of it reaches the transport.
	///
	/// Both halves matter: a second stream re-requests the same id, which the peer is
	/// free to serve twice, and a late insert loses the race with a publisher that
	/// serves its first group the instant it reads the request (`recv_group` drops a
	/// group whose id isn't in the map yet).
	#[kio::test]
	async fn establish_sends_one_registered_subscribe() {
		// Writes park until this opens, so the assertions below run at the exact moment
		// the request would hit the wire.
		let gate = kio::Producer::new(false);
		let session = SinkSession::gated_bi(gate.consume());

		let origin = origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce();
		let subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(session.clone()),
			origin,
			recv_bandwidth: None,
			version: VERSION,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			peer_node: None,
			cost: None,
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});
		let subscribes = subscriber.subscribes.clone();
		let serve = TrackServe {
			validity: Default::default(),
			route: None,
			source: None,
			subscriber,
			path: Path::from_static("room/host"),
			instance: None,
			node: None,
			name: "catalog.json".to_string(),
		};

		let broadcast = crate::broadcast::Info::new().produce();
		let mut producer = broadcast
			.create_track("catalog.json", crate::track::Info::timed())
			.unwrap();
		let mut sub = Sub::None;
		let mut establish = std::pin::pin!(serve.establish(&mut producer, &mut sub, Subscription::default()));

		// Parked on the first write: the stream is open and nothing has been sent yet.
		assert!(futures::poll!(establish.as_mut()).is_pending());
		assert_eq!(session.log.bi_opens(), 1);
		assert!(subscribes.lock().contains_key(&0), "registered before the wire");

		let Ok(mut open) = gate.write() else {
			panic!("gate closed")
		};
		*open = true;
		drop(open);

		establish.await.unwrap();

		// One request, on one stream, and nothing else behind it.
		assert_eq!(session.log.bi_opens(), 1);

		let writes = session.log.writes.lock().unwrap().clone();
		let mut wire = writes.as_slice();
		assert_eq!(
			crate::coding::decode_buf(&mut wire, VERSION, |r, v| lite::ControlType::decode(r, v)).unwrap(),
			lite::ControlType::Subscribe
		);
		let msg = crate::coding::decode_from(&mut wire, VERSION, lite::Subscribe::decode).unwrap();
		assert_eq!(msg.id, 0);
		assert_eq!(msg.track, "catalog.json");
		assert!(wire.is_empty(), "a second SUBSCRIBE trailed the first");
	}

	/// A narrower limit landing while SUBSCRIBE is still opening resets it with
	/// UNAUTHORIZED, like a live one, and releases its id.
	#[kio::test]
	async fn a_narrower_limit_resets_a_subscribe_still_opening() {
		// Writes park, so the SUBSCRIBE is on an open stream but not yet flushed.
		let gate = kio::Producer::new(false);
		let session = SinkSession::gated_bi(gate.consume());
		let auth = crate::auth::Link::new(false);

		let origin = origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce();
		let subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(session.clone()),
			origin,
			recv_bandwidth: None,
			version: VERSION,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			peer_node: None,
			cost: None,
			going_away: Default::default(),
			auth: auth.clone(),
		});
		let subscribes = subscriber.subscribes.clone();
		let serve = TrackServe {
			validity: Default::default(),
			route: None,
			source: None,
			subscriber,
			path: Path::from_static("room/host"),
			instance: None,
			node: None,
			name: "audio".to_string(),
		};

		let broadcast = crate::broadcast::Info::new().produce();
		let request = broadcast.reserve_track("audio").unwrap();
		let mut serving = ServeLoop::new(&serve, request, Default::default());
		let _reader = serving.serving.subscribe(None);
		let wire = Wire {
			subscription: Subscription::default(),
			demand: Some(Subscription::default()),
			demand_ranges: vec![lite::Range::OPEN],
			probes: 0,
			reasked: Default::default(),
			ranges: vec![lite::Range::OPEN],
			max_age: None,
		};
		let establish = serve.prepare_establish(&mut serving.serving, wire, &Default::default());
		serving.mode = ServeMode::Establish(establish);
		let gate_run = serve.gate();
		let mut running = TrackServeRun {
			serve,
			state: TrackRunState::Serve(serving),
			gate: gate_run,
			datagrams: None,
			finished: None,
		};

		assert!(kio::Task::poll(&mut running, &kio::Waiter::noop()).is_pending());
		assert_eq!(session.log.bi_opens(), 1, "the SUBSCRIBE stream is open");

		auth.handle().authorize(&crate::auth::Grant::default());
		assert!(kio::Task::poll(&mut running, &kio::Waiter::noop()).is_ready());

		let unauthorized = crate::StreamError::Unauthorized.to_code();
		assert_eq!(session.log.resets(), vec![unauthorized]);
		assert!(subscribes.lock().is_empty(), "the id outlived its subscription");
		drop(gate);
	}

	/// Losing demand releases setup even when credit, writes, or the answer never arrive.
	#[kio::test]
	async fn an_abandoned_setup_releases_its_track_and_registration() {
		for stage in ["credit", "write", "answer"] {
			let gate = kio::Producer::new(stage != "write");
			let opening = kio::Producer::new(true);
			let session = SinkSession::gated_bi(gate.consume()).with_open_bi_gate(opening.consume());

			let origin = origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce();
			let subscriber = Subscriber::new(SubscriberConfig {
				runtime: crate::time::sim(),
				session: crate::driver::Transport::new(session.clone()),
				origin,
				recv_bandwidth: None,
				version: Version::Lite07,
				peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
				peer_node: None,
				cost: None,
				going_away: Default::default(),
				auth: crate::auth::Link::new(false),
			});
			let subscribes = subscriber.subscribes.clone();
			let serve = TrackServe {
				validity: Default::default(),
				route: None,
				source: None,
				subscriber,
				path: Path::from_static("room/host"),
				instance: None,
				node: None,
				name: "audio".to_string(),
			};

			let broadcast = crate::broadcast::Info::new().produce();
			let request = broadcast.reserve_track("audio").unwrap();
			let mut serving = ServeLoop::new(&serve, request, Default::default());
			let reader = serving.serving.subscribe(None);
			let held = Stream::open(&serve.subscriber.session, Version::Lite07).await.unwrap();
			serving.held = Some(HeldTrack(held));
			if stage == "credit" {
				*opening.write().ok().expect("gate live") = false;
			}
			serving.linger.set(Some(serve.subscriber.runtime.now()));
			let pref = serving.serving.subscription();
			serving.begin(&serve, pref).unwrap();
			assert!(serving.linger.deadline().is_none(), "new demand ends the old linger");
			let gate_run = serve.gate();
			let mut running = TrackServeRun {
				serve,
				state: TrackRunState::Serve(serving),
				gate: gate_run,
				datagrams: None,
				finished: None,
			};

			assert!(kio::Task::poll(&mut running, &kio::Waiter::noop()).is_pending());
			assert_eq!(session.log.bi_opens(), if stage == "credit" { 1 } else { 2 });
			assert!(!subscribes.lock().is_empty());
			assert!(
				!session
					.log
					.trail()
					.iter()
					.any(|(_, sent)| matches!(sent, crate::lite::test_transport::Sent::Finish))
			);

			// A canceled fetch must retire even while establishment is blocked.
			let (_track, fetching, fill) = feed_fetch(50);
			let TrackRunState::Serve(serving) = &mut running.state else {
				unreachable!()
			};
			serving.fetches.push(FillServeRun::new(
				running.serve.clone(),
				fill,
				FetchInfo::Known(Default::default()),
			));
			drop(fetching);
			drop(reader);
			let _ = kio::Task::poll(&mut running, &kio::Waiter::noop());

			assert!(
				session
					.log
					.trail()
					.iter()
					.any(|(id, sent)| *id == 0 && matches!(sent, crate::lite::test_transport::Sent::Finish)),
				"TRACK hold released"
			);
			assert!(session.log.closes().is_empty(), "session stays live");
			assert!(subscribes.lock().is_empty(), "the id outlived its subscription");
			let TrackRunState::Serve(serving) = &running.state else {
				panic!("copy should linger")
			};
			assert!(matches!(serving.mode, ServeMode::Select));
			assert!(serving.linger.deadline().is_some());
			assert!(serving.fetches.is_empty());
			drop(gate);
		}
	}

	/// Equal demand returning between cancellation and Select must open a new setup.
	#[kio::test]
	async fn identical_demand_after_setup_cancellation_is_not_already_served() {
		let gate = kio::Producer::new(false);
		let opening = kio::Producer::new(true);
		let session = SinkSession::gated_bi(gate.consume()).with_open_bi_gate(opening.consume());

		let origin = origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce();
		let subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(session.clone()),
			origin,
			recv_bandwidth: None,
			version: Version::Lite07,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			peer_node: None,
			cost: None,
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});
		let subscribes = subscriber.subscribes.clone();
		let serve = TrackServe {
			validity: Default::default(),
			route: None,
			source: None,
			subscriber,
			path: Path::from_static("room/host"),
			instance: None,
			node: None,
			name: "audio".to_string(),
		};

		let broadcast = crate::broadcast::Info::new().produce();
		let request = broadcast.reserve_track("audio").unwrap();
		let mut serving = ServeLoop::new(&serve, request, Default::default());
		let held = Stream::open(&serve.subscriber.session, Version::Lite07).await.unwrap();
		serving.held = Some(HeldTrack(held));
		let wire = Wire {
			subscription: Subscription::default(),
			demand: Some(Subscription::default()),
			demand_ranges: vec![lite::Range::OPEN],
			probes: 0,
			reasked: Default::default(),
			ranges: vec![lite::Range::OPEN],
			max_age: None,
		};
		let reader = serving.serving.subscribe(None);
		serving.serving.seen_subscription(Some(Subscription::default()));
		let establish = serve.prepare_establish(&mut serving.serving, wire, &Default::default());
		serving.mode = ServeMode::Establish(establish);

		drop(reader);
		serving.abandon_setup();
		let _returning = serving.serving.subscribe(None);
		assert!(matches!(
			serving.serving.poll_subscription_changed(&kio::Waiter::noop()),
			Poll::Ready(Ok(Some(_)))
		));
		let pref = serving.serving.subscription();
		serving.begin(&serve, pref).unwrap();
		assert!(matches!(serving.mode, ServeMode::Establish(_)));
		assert_eq!(subscribes.lock().len(), 1);
	}

	/// A requester owns setup through metadata acceptance until a reader attaches or
	/// the requester disappears, including when no poll ever observed its demand.
	#[kio::test]
	async fn a_requester_owns_setup_until_attachment_or_cancellation() {
		for attach in [false, true] {
			let gate = kio::Producer::new(false);
			let opening = kio::Producer::new(true);
			let session = SinkSession::gated_bi(gate.consume()).with_open_bi_gate(opening.consume());

			let origin = origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce();
			let subscriber = Subscriber::new(SubscriberConfig {
				runtime: crate::time::sim(),
				session: crate::driver::Transport::new(session.clone()),
				origin,
				recv_bandwidth: None,
				version: Version::Lite07,
				peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
				peer_node: None,
				cost: None,
				going_away: Default::default(),
				auth: crate::auth::Link::new(false),
			});
			let subscribes = subscriber.subscribes.clone();
			let serve = TrackServe {
				validity: Default::default(),
				route: None,
				source: None,
				subscriber,
				path: Path::from_static("room/host"),
				instance: None,
				node: None,
				name: "audio".to_string(),
			};

			let broadcast = crate::broadcast::Info::new().produce();
			let request = broadcast.reserve_track("audio").unwrap();
			let requester = request.consume().info();
			let mut serving = ServeLoop::new(&serve, request, Default::default());
			assert!(requester.poll_ok(&kio::Waiter::noop()).is_ready());
			let held = Stream::open(&serve.subscriber.session, Version::Lite07).await.unwrap();
			serving.held = Some(HeldTrack(held));
			let wire = Wire {
				subscription: Subscription::default(),
				demand: Some(Subscription::default()),
				demand_ranges: vec![lite::Range::OPEN],
				probes: 0,
				reasked: Default::default(),
				ranges: vec![lite::Range::OPEN],
				max_age: None,
			};
			let establish = serve.prepare_establish(&mut serving.serving, wire, &Default::default());
			serving.mode = ServeMode::Establish(establish);
			let gate_run = serve.gate();
			let mut running = TrackServeRun {
				serve,
				state: TrackRunState::Serve(serving),
				gate: gate_run,
				datagrams: None,
				finished: None,
			};

			// Cancellation can happen before the first serving poll after metadata acceptance.
			if !attach {
				drop(requester);
				let _ = kio::Task::poll(&mut running, &kio::Waiter::noop());
				assert!(subscribes.lock().is_empty(), "a vanished requester releases setup");
				assert!(
					session
						.log
						.trail()
						.iter()
						.any(|(id, sent)| *id == 0 && matches!(sent, crate::lite::test_transport::Sent::Finish)),
					"a vanished requester releases TRACK"
				);
				continue;
			}
			// The requester still owns setup while no group reader has attached.
			assert!(kio::Task::poll(&mut running, &kio::Waiter::noop()).is_pending());
			let TrackRunState::Serve(serving) = &mut running.state else {
				unreachable!()
			};
			assert!(matches!(serving.mode, ServeMode::Establish(_)));
			assert!(!subscribes.lock().is_empty(), "the fresh setup keeps its registration");
			assert!(
				!session
					.log
					.trail()
					.iter()
					.any(|(_, sent)| matches!(sent, crate::lite::test_transport::Sent::Finish)),
				"a fresh setup keeps its TRACK hold"
			);

			// A reader arriving keeps it, and its leaving is what releases the setup.
			let reader = serving.serving.subscribe(None);
			drop(requester);
			assert!(kio::Task::poll(&mut running, &kio::Waiter::noop()).is_pending());
			drop(reader);
			let _ = kio::Task::poll(&mut running, &kio::Waiter::noop());
			assert!(
				session
					.log
					.trail()
					.iter()
					.any(|(id, sent)| *id == 0 && matches!(sent, crate::lite::test_transport::Sent::Finish)),
				"TRACK hold released once demand left"
			);
			assert!(subscribes.lock().is_empty(), "the id outlived its subscription");
			drop(gate);
		}
	}

	/// Everything a `handle_subscription` test needs to stay alive for the call.
	struct Harness {
		serve: TrackServe,
		session: SinkSession,
		producer: track::Producer,
		_broadcast: crate::broadcast::Producer,
		_gate: kio::Producer<bool>,
	}

	impl Harness {
		/// A `TrackServe` writing straight to a sink session at `version`.
		fn new(version: Version) -> Self {
			// Open the gate up front: these tests assert what reached the wire, not
			// what was true at the instant it would.
			let gate = kio::Producer::new(true);
			let session = SinkSession::gated_bi(gate.consume());
			let origin = origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce();
			let subscriber = Subscriber::new(SubscriberConfig {
				runtime: crate::time::sim(),
				session: crate::driver::Transport::new(session.clone()),
				origin,
				recv_bandwidth: None,
				version,
				peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
				peer_node: None,
				cost: None,
				going_away: Default::default(),
				auth: crate::auth::Link::new(false),
			});
			let broadcast = crate::broadcast::Info::new().produce();
			let producer = broadcast
				.create_track("catalog.json", crate::track::Info::timed())
				.unwrap();

			Self {
				serve: TrackServe {
					validity: Default::default(),
					route: None,
					source: None,
					subscriber,
					path: Path::from_static("room/host"),
					instance: None,
					node: None,
					name: "catalog.json".to_string(),
				},
				session,
				producer,
				_broadcast: broadcast,
				_gate: gate,
			}
		}

		/// Everything written to the session so far.
		fn wire(&self) -> Vec<u8> {
			self.session.log.writes.lock().unwrap().clone()
		}
	}

	#[kio::test]
	async fn internal_cancel_without_a_recorded_session_end_is_cancel() {
		let h = Harness::new(VERSION);
		assert!(matches!(h.serve.subscriber.end_reason(), Error::Cancel));
	}

	#[kio::test]
	async fn internal_serve_preserves_an_abort_from_a_live_clone() {
		for dynamic_first in [true, false] {
			let h = Harness::new(VERSION);
			let request = h._broadcast.reserve_track("pending").unwrap();
			let mut serving = ServeLoop::new(&h.serve, request, Default::default());
			let other = h._broadcast.create_track("other", None).unwrap();
			if !dynamic_first {
				// Isolate the subscription-changed arm, which can observe a concurrent
				// abort after the dynamic-request poll returned Pending.
				serving.dynamic = other.requests();
			}
			serving.serving.clone().abort(Error::ProtocolViolation).unwrap();
			assert!(matches!(
				serving.poll(&h.serve, &kio::Waiter::noop()),
				Poll::Ready(ServeEnd::GiveBack(Error::ProtocolViolation))
			));
		}
	}

	/// A session that dies before a tail settles hands the track back with the session's error,
	/// even one the publisher ended, and the abort `TrackServeRun` commits then ends readers
	/// cleanly only if the declared end settled: SUBSCRIBE_END on lite-05+, the FIN before it.
	#[kio::test]
	async fn internal_session_end_aborts_an_unsettled_tail() {
		use crate::transport::poll::Session as _;
		// The publisher ended the track; whether its last group finished.
		for (version, ended) in [
			(Version::Lite04, None),
			(Version::Lite04, Some(false)),
			(Version::Lite04, Some(true)),
			(Version::Lite05, Some(false)),
			(Version::Lite05, Some(true)),
		] {
			let (session, peer) = crate::transport::memory::pair(Default::default());
			let subscriber = Subscriber::new(SubscriberConfig {
				runtime: crate::time::sim(),
				session: crate::driver::Transport::new(session.clone()),
				origin: origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce(),
				recv_bandwidth: None,
				version,
				peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
				peer_node: None,
				cost: None,
				going_away: Default::default(),
				auth: crate::auth::Link::new(false),
			});
			let serve = TrackServe {
				validity: Default::default(),
				route: None,
				source: None,
				subscriber,
				path: Path::from_static("room"),
				instance: None,
				node: None,
				name: "video".into(),
			};
			let broadcast = crate::broadcast::Info::new().produce();
			let mut serving = ServeLoop::new(&serve, broadcast.reserve_track("video").unwrap(), Default::default());
			let _reader = serving.serving.subscribe(None);
			assert!(serving.poll(&serve, &kio::Waiter::noop()).is_pending());
			// Held for the whole round: a dropped tail settles at once.
			let tail = kio::Producer::new(Tail::default());
			let mut group = Some(serving.serving.append_next().unwrap());
			if let Some(finished) = ended {
				if finished {
					group.take().unwrap().finish().unwrap();
				}
				if version.has_track_stream() {
					serving.serving.finish_at(Position::group(1)).unwrap();
				}
				// Nothing says which groups are owed, so only the grace settles it, which has not passed.
				serving.mode = ServeMode::Tail {
					settle: Settle::new(&serve.subscriber.runtime, tail.consume()),
					requested_end: None,
				};
				assert!(serving.poll(&serve, &kio::Waiter::noop()).is_pending());
			}
			peer.close(crate::SessionError::Unauthorized.to_code(), "revoked");
			let end = serving.poll(&serve, &kio::Waiter::noop());
			let Poll::Ready(ServeEnd::GiveBack(err @ Error::Session(crate::SessionError::Unauthorized))) = end else {
				panic!("{version:?} {ended:?}: the tail must hand the track back");
			};
			let track = serving.serving.clone();
			serving.serving.abort(err).unwrap();
			let outcome = track.poll_closed(&kio::Waiter::noop());
			match ended {
				Some(true) => assert!(matches!(outcome, Poll::Ready(Ok(()))), "{version:?}: {outcome:?}"),
				_ => assert!(
					matches!(
						outcome,
						Poll::Ready(Err(Error::Session(crate::SessionError::Unauthorized)))
					),
					"{version:?} {ended:?}: {outcome:?}"
				),
			}
			drop(group);
		}
	}

	/// A publisher that finishes the track stops reading its subscribe stream, so a
	/// SUBSCRIBE_UPDATE written then fails. The track still ends with the publisher's
	/// finish; a reset still hands it back with the reset's error.
	#[kio::test]
	async fn an_update_the_publisher_stopped_reading_defers_to_its_end() {
		use crate::transport::poll::{RecvStream as _, SendStream as _, Session as _};
		for reset in [false, true] {
			let version = Version::Lite06;
			let (session, peer) = crate::transport::memory::pair(Default::default());
			let subscriber = Subscriber::new(SubscriberConfig {
				runtime: crate::time::sim(),
				session: crate::driver::Transport::new(session.clone()),
				origin: origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce(),
				recv_bandwidth: None,
				version,
				peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
				peer_node: None,
				cost: None,
				going_away: Default::default(),
				auth: crate::auth::Link::new(false),
			});
			let serve = TrackServe {
				validity: Default::default(),
				route: None,
				source: None,
				subscriber,
				path: Path::from_static("room"),
				instance: None,
				node: None,
				name: "video".into(),
			};
			let broadcast = crate::broadcast::Info::new().produce();
			let mut serving = ServeLoop::new(&serve, broadcast.reserve_track("video").unwrap(), Default::default());
			let mut reader = serving.serving.subscribe(None);
			assert!(serving.poll(&serve, &kio::Waiter::noop()).is_pending());
			assert!(matches!(serving.sub, Sub::Active(_)), "the SUBSCRIBE is on the wire");

			// The publisher ends the subscription and stops reading the stream.
			let (mut send, mut recv) = kio::wait(|waiter| peer.poll_accept_bi(waiter)).await.unwrap();
			recv.stop(crate::StreamError::Cancel.to_code());
			if reset {
				send.reset(crate::StreamError::NotFound.to_code());
			} else {
				let mut end = Vec::new();
				lite::SubscribeResponse::End(lite::SubscribeEnd { group: 0, error: None })
					.encode(&mut crate::coding::Encoder::new(&mut end, version.into()), version)
					.unwrap();
				kio::wait(|waiter| send.poll_write(waiter, &end)).await.unwrap();
				send.finish().unwrap();
			}

			// The update races it, and its write fails.
			reader.update(Subscription::default().with_priority(1)).unwrap();
			let gate = serve.gate();
			let mut running = TrackServeRun {
				serve,
				state: TrackRunState::Serve(serving),
				gate,
				datagrams: None,
				finished: None,
			};
			// A finished copy stays for its reader, serving fetches; a reset one is gone.
			let run = poll_run(&mut running, &kio::Waiter::noop());
			assert!(matches!(running.state, TrackRunState::Done));
			assert_eq!(run.is_ready(), reset);
			let end = reader.recv_group().now_or_never().expect("the track ended").map(|_| ());
			match reset {
				false => assert!(matches!(end, Ok(())), "{end:?}"),
				true => assert!(
					matches!(end, Err(Error::Stream(crate::StreamError::NotFound))),
					"{end:?}"
				),
			}
		}
	}

	/// Before lite-07 nothing answers a SUBSCRIBE_UPDATE, so a floor a reader lowers below
	/// SUBSCRIBE_START as the publisher ends the track may never reach it: the publisher FINs
	/// first and ignores the update. The copy then FINs its side at once, rather than after a
	/// grace for groups nobody will send, which would hold the publisher's draining close.
	#[kio::test]
	async fn a_floor_lowered_as_the_publisher_ends_owes_nothing_below_start() {
		use crate::transport::poll::{RecvStream as _, SendStream as _, Session as _};
		for version in [Version::Lite05, Version::Lite06] {
			let (session, peer) = crate::transport::memory::pair(Default::default());
			let subscriber = Subscriber::new(SubscriberConfig {
				runtime: crate::time::sim(),
				session: crate::driver::Transport::new(session.clone()),
				origin: origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce(),
				recv_bandwidth: None,
				version,
				peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
				peer_node: None,
				cost: None,
				going_away: Default::default(),
				auth: crate::auth::Link::new(false),
			});
			let serve = TrackServe {
				validity: Default::default(),
				route: None,
				source: None,
				subscriber,
				path: Path::from_static("room"),
				instance: None,
				node: None,
				name: "video".into(),
			};
			let broadcast = crate::broadcast::Info::new().produce();
			let mut serving = ServeLoop::new(&serve, broadcast.reserve_track("video").unwrap(), Default::default());
			let _live = serving.serving.subscribe(None);
			assert!(serving.poll(&serve, &kio::Waiter::noop()).is_pending());

			let (mut send, mut recv) = kio::wait(|waiter| peer.poll_accept_bi(waiter)).await.unwrap();
			let respond = |msg: lite::SubscribeResponse| {
				let mut buf = Vec::new();
				msg.encode(&mut crate::coding::Encoder::new(&mut buf, version.into()), version)
					.unwrap();
				buf
			};
			let start = respond(lite::SubscribeResponse::Start(lite::SubscribeStart {
				id: 0,
				start: Some(2),
				largest: lite::Edge::Unknown,
			}));
			kio::wait(|waiter| send.poll_write(waiter, &start)).await.unwrap();
			assert!(serving.poll(&serve, &kio::Waiter::noop()).is_pending());

			// A reader asking from group 0 lowers the floor, which goes out as an update.
			let _history = serving.serving.subscribe(Some(Subscription::default().with_start(0)));
			assert!(serving.poll(&serve, &kio::Waiter::noop()).is_pending());

			// The publisher had already ended the track, so it never applies the update.
			let end = respond(lite::SubscribeResponse::End(lite::SubscribeEnd {
				group: 2,
				error: None,
			}));
			kio::wait(|waiter| send.poll_write(waiter, &end)).await.unwrap();
			send.finish().unwrap();
			let gate = serve.gate();
			let mut running = TrackServeRun {
				serve,
				state: TrackRunState::Serve(serving),
				gate,
				datagrams: None,
				finished: None,
			};
			let _ = poll_run(&mut running, &kio::Waiter::noop());

			// No time passed, so no grace did: the SUBSCRIBE and its update, then the FIN.
			let mut chunks = 0;
			loop {
				match recv.poll_read_chunk(&kio::Waiter::noop(), usize::MAX) {
					Poll::Ready(Ok(Some(_))) => chunks += 1,
					Poll::Ready(Ok(None)) => break,
					other => panic!("{version:?}: no FIN after {chunks} chunks: {other:?}"),
				}
			}
		}
	}

	/// A SUBSCRIBE_UPDATE the publisher has no credit for holds back only the next demand
	/// change, which goes out once it is on the wire: the serve loop still reads responses, so a
	/// publisher blocked writing them while it reads none of ours is no deadlock, and still
	/// cancels once the last reader leaves.
	#[kio::test]
	async fn an_update_waiting_on_credit_still_reads_and_cancels() {
		use crate::transport::poll::{RecvStream as _, SendStream as _, Session as _};
		for version in [Version::Lite06, Version::Lite07] {
			// Draining: lite-07 fills still ask for a run whose stream arrives, which only a reader keeps.
			for (cancel, draining) in [(false, false), (true, false), (true, true)] {
				if draining && !version.has_ranges() {
					continue;
				}
				// One queued message per stream, so a peer that stops reading stalls the writer.
				let config = crate::transport::memory::Config {
					stream_buffer: 0,
					..Default::default()
				};
				let (session, peer) = crate::transport::memory::pair(config);
				let subscriber = Subscriber::new(SubscriberConfig {
					runtime: crate::time::sim(),
					session: crate::driver::Transport::new(session.clone()),
					origin: origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce(),
					recv_bandwidth: None,
					version,
					peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
					peer_node: None,
					cost: None,
					going_away: Default::default(),
					auth: crate::auth::Link::new(false),
				});
				let serve = TrackServe {
					validity: Default::default(),
					route: None,
					source: None,
					subscriber,
					path: Path::from_static("room"),
					instance: None,
					node: None,
					name: "video".into(),
				};
				let waiter = kio::Waiter::noop();
				let broadcast = crate::broadcast::Info::new().produce();
				// Timed, for the group a draining run waits on.
				let info = track::Info::timed();
				let mut serving = ServeLoop::new(&serve, broadcast.reserve_track("video").unwrap(), info);
				let live = serving.serving.subscribe(None);
				assert!(serving.poll(&serve, &waiter).is_pending());
				let (mut send, mut recv) = kio::wait(|waiter| peer.poll_accept_bi(waiter)).await.unwrap();

				// The SUBSCRIBE fills the stream, so the update a lowered floor sends waits on credit,
				// and the change after it waits for that one.
				let history = serving.serving.subscribe(Some(Subscription::default().with_start(0)));
				assert!(serving.poll(&serve, &waiter).is_pending());
				let urgent = serving
					.serving
					.subscribe(Some(Subscription::default().with_start(0).with_priority(5)));
				#[derive(Default)]
				struct Wakes(std::sync::atomic::AtomicUsize);
				impl std::task::Wake for Wakes {
					fn wake(self: Arc<Self>) {
						self.0.fetch_add(1, atomic::Ordering::Relaxed);
					}
				}
				let wakes = Arc::new(Wakes::default());
				let counted = kio::Waiter::new(std::task::Waker::from(wakes.clone()));
				assert!(serving.poll(&serve, &counted).is_pending());
				assert!(serving.deferred, "{version:?}: the change waits on the update");
				// The change after the deferred one still wakes the loop, as a cancel must.
				let later = serving
					.serving
					.subscribe(Some(Subscription::default().with_start(0).with_priority(9)));
				assert!(
					wakes.0.load(atomic::Ordering::Relaxed) > 0,
					"{version:?}: a change after a deferred one wakes the loop"
				);
				drop(later);
				assert!(serving.poll(&serve, &waiter).is_pending());

				if cancel {
					let mut _arriving = None;
					if draining {
						let mut group = serving.serving.create_group(group::Info { start: 0 }).unwrap();
						group.write_frame(group.key_time(), b"x".as_slice()).unwrap();
						_arriving = Some(group);
						serving.range_fills.ask.groups.insert(0..1);
					}
					drop((live, history, urgent));
					assert!(serving.poll(&serve, &waiter).is_pending());
					assert!(
						matches!(serving.sub, Sub::None),
						"{version:?} draining {draining}: the subscription is cancelled with its update unsent"
					);
					assert!(
						!serving.deferred,
						"{version:?} draining {draining}: nothing is left to apply"
					);
					continue;
				}

				// Each change goes out once its turn comes: the SUBSCRIBE, then one update each.
				let mut chunks = 0;
				while let Poll::Ready(Ok(Some(_))) = recv.poll_read_chunk(&waiter, usize::MAX) {
					chunks += 1;
					assert!(serving.poll(&serve, &waiter).is_pending());
				}
				assert_eq!(chunks, 3, "{version:?}");

				// Stalled again: one update unread, the next waiting on it.
				drop(urgent);
				assert!(serving.poll(&serve, &waiter).is_pending());
				drop(history);
				assert!(serving.poll(&serve, &waiter).is_pending());

				// The publisher gives up the subscription without reading the update.
				send.reset(crate::StreamError::Cancel.to_code());
				let end = serving.poll(&serve, &waiter);
				assert!(
					matches!(end, Poll::Ready(ServeEnd::GiveBack(_))),
					"{version:?}: the reset is read behind the unsent update"
				);
				drop(live);
			}
		}
	}

	/// Malformed bytes on a Subscribe Stream close the session at once, on every version, even
	/// with a group stream still being read: the close never waits on a drain that a reader
	/// leaving meanwhile would cut short.
	#[kio::test]
	async fn a_malformed_subscribe_response_closes_the_session_at_once() {
		for version in [Version::Lite06, Version::Lite07] {
			malformed_subscribe_response_closes_at_once(version).await;
		}
	}

	async fn malformed_subscribe_response_closes_at_once(version: Version) {
		use crate::transport::poll::{SendStream as _, Session as _};
		let (session, peer) = crate::transport::memory::pair(Default::default());
		let subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(session.clone()),
			origin: origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce(),
			recv_bandwidth: None,
			version,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			peer_node: None,
			cost: None,
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});
		let serve = TrackServe {
			validity: Default::default(),
			route: None,
			source: None,
			subscriber,
			path: Path::from_static("room"),
			instance: None,
			node: None,
			name: "video".into(),
		};
		let broadcast = crate::broadcast::Info::new().produce();
		let mut serving = ServeLoop::new(&serve, broadcast.reserve_track("video").unwrap(), Default::default());
		let reader = serving.serving.subscribe(None);
		assert!(serving.poll(&serve, &kio::Waiter::noop()).is_pending());
		let Sub::Active(active) = &serving.sub else {
			panic!("the SUBSCRIBE is on the wire");
		};
		// A group stream the publisher has yet to finish keeps the tail busy.
		let _reading = Reading::open(&active.tail, Some(0), serve.subscriber.runtime.now());

		// The publisher answers with an unknown response type.
		let (mut send, _recv) = kio::wait(|waiter| peer.poll_accept_bi(waiter)).await.unwrap();
		kio::wait(|waiter| send.poll_write(waiter, &[0x3f, 0x00]))
			.await
			.unwrap();
		let _reader = reader;

		let gate = serve.gate();
		let mut running = TrackServeRun {
			serve,
			state: TrackRunState::Serve(serving),
			gate,
			datagrams: None,
			finished: None,
		};
		let _ = poll_run(&mut running, &kio::Waiter::noop());
		let closed = peer.poll_closed(&kio::Waiter::noop());
		let Poll::Ready(err) = closed else {
			panic!("{version:?}: the session is still open");
		};
		assert_eq!(
			crate::transport::Error::session_error(&err).map(|(code, _)| code),
			Some(crate::SessionError::ProtocolViolation.to_code()),
			"{version:?}"
		);
	}

	/// A PROBE stream the peer ends, by FIN or reset, leaves no estimate behind: the
	/// session gives up probing, so a held consumer must not keep its last rate.
	#[kio::test]
	async fn an_ended_probe_clears_the_estimate() {
		use crate::transport::poll::{SendStream as _, Session as _};
		for reset in [false, true] {
			let version = Version::Lite04;
			let (session, peer) = crate::transport::memory::pair(Default::default());
			let producer = bandwidth::Producer::new();
			let mut estimate = producer.consume();
			let subscriber = Subscriber::new(SubscriberConfig {
				runtime: crate::time::sim(),
				session: crate::driver::Transport::new(session.clone()),
				origin: origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce(),
				recv_bandwidth: Some(producer),
				version,
				peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
				peer_node: None,
				cost: None,
				going_away: Default::default(),
				auth: crate::auth::Link::new(false),
			});
			let mut probe = RecvBandwidth::new(subscriber);
			assert!(probe.poll(&kio::Waiter::noop()).is_pending());

			// The peer reports once.
			let (mut send, _recv) = kio::wait(|waiter| peer.poll_accept_bi(waiter)).await.unwrap();
			let mut report = Vec::new();
			lite::Probe {
				bitrate: Some(1_000_000),
				rtt: None,
			}
			.encode(&mut crate::coding::Encoder::new(&mut report, version.into()), version)
			.unwrap();
			kio::wait(|waiter| send.poll_write(waiter, &report)).await.unwrap();
			let rate = kio::wait(|waiter| {
				let _ = probe.poll(waiter);
				estimate.poll_changed(waiter)
			})
			.await
			.unwrap();
			assert_eq!(rate, Some(bandwidth::Rate::from_bps(1_000_000)));

			// Then ends the stream.
			if reset {
				send.reset(crate::StreamError::Cancel.to_code());
			} else {
				send.finish().unwrap();
			}
			kio::wait(|waiter| probe.poll(waiter)).await.unwrap();
			assert_eq!(estimate.peek(), None, "reset: {reset}");
		}
	}

	/// A peer that stops reading holds one PROBE target, and once it drains, only the
	/// latest is sent: the targets set while the writer was stalled never pile up behind it.
	#[kio::test]
	async fn a_stalled_probe_writer_sends_only_the_latest_target() {
		use crate::transport::poll::{RecvStream as _, Session as _};
		let version = Version::Lite06;
		// One queued message per stream, so a peer that stops reading stalls the writer.
		let config = crate::transport::memory::Config {
			stream_buffer: 0,
			..Default::default()
		};
		let (session, peer) = crate::transport::memory::pair(config);
		let producer = bandwidth::Producer::new();
		let _estimate = producer.consume();
		let peer_setup = lite::PeerSetup::new(false, crate::transport::Address::Setup);
		peer_setup
			.set(lite::Setup {
				probe: lite::ProbeLevel::Increase,
				..Default::default()
			})
			.unwrap();
		let subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(session.clone()),
			origin: origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce(),
			recv_bandwidth: Some(producer),
			version,
			peer_setup,
			peer_node: None,
			cost: None,
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});
		let viewer = subscriber.viewers.watch(bandwidth::Rate::from_mbps(1)).unwrap();
		let mut probe = RecvBandwidth::new(subscriber);

		// The stream type fills the stream, so the first target waits in the writer.
		let (_send, mut recv) = kio::wait(|waiter| {
			let _ = probe.poll(waiter);
			peer.poll_accept_bi(waiter)
		})
		.await
		.unwrap();
		assert!(probe.poll(&kio::Waiter::noop()).is_pending());
		for mbps in 2..=4 {
			viewer.update(bandwidth::Rate::from_mbps(mbps)).unwrap();
			assert!(probe.poll(&kio::Waiter::noop()).is_pending());
		}

		let target = |mbps| lite::Probe {
			bitrate: Some(bandwidth::Rate::from_mbps(mbps).as_bps()),
			rtt: None,
		};
		let expected = [
			lite::ControlType::Probe.encode_bytes(version).unwrap(),
			target(1).encode_bytes(version).unwrap(),
			target(4).encode_bytes(version).unwrap(),
		];
		for expected in expected {
			let chunk = kio::wait(|waiter| {
				let _ = probe.poll(waiter);
				recv.poll_read_chunk(waiter, usize::MAX)
			})
			.await
			.unwrap()
			.expect("the stream stays open");
			assert_eq!(chunk, expected);
		}
		assert!(probe.poll(&kio::Waiter::noop()).is_pending());
		assert!(recv.poll_read_chunk(&kio::Waiter::noop(), usize::MAX).is_pending());
	}

	/// The model's groups map to wire ranges starting and ending at a group's first frame,
	/// with empty ranges dropped. Off by one here would silently drop or duplicate a group
	/// at every relay hop.
	#[test]
	fn wire_ranges_name_whole_groups() {
		let group = |group| Some(lite::Anchor::Absolute(Position::group(group)));
		let time = |t| Some(lite::Anchor::Time(t));
		let ranges = wire_ranges(&Subscription::default().with_range(5..6), true);
		assert_eq!(
			ranges,
			Some(vec![lite::Range {
				start: time(5),
				end: time(5),
			}])
		);
		// Pre-lite-07 carries absolute group bounds.
		assert_eq!(
			wire_ranges(&Subscription::default().with_range(5..6), false),
			Some(vec![lite::Range {
				start: group(5),
				end: group(6),
			}])
		);
		assert_eq!(wire_ranges(&Subscription::default().with_end(0), true), None);

		// A time anchor names a time on lite-07, and a group start on older wires.
		let timed = Subscription::default().with_ranges([track::Range {
			start: crate::track::Start::Time(5),
			end: Some(9),
		}]);
		assert_eq!(
			wire_ranges(&timed, true),
			Some(vec![lite::Range {
				start: time(5),
				end: time(9),
			}])
		);
		assert_eq!(
			wire_ranges(&timed, false),
			Some(vec![lite::Range {
				start: group(5),
				end: group(10),
			}])
		);
		// An end before its start asks for nothing.
		let empty = Subscription::default().with_ranges([track::Range {
			start: crate::track::Start::Time(5),
			end: Some(4),
		}]);
		assert_eq!(wire_ranges(&empty, true), None);

		// The oldest group goes out as no start, and a live join as itself.
		let all = Subscription::default().with_ranges([track::Range::ALL]);
		assert_eq!(wire_ranges(&all, true), Some(vec![lite::Range::OPEN]));
		assert_eq!(
			wire_ranges(&Subscription::default(), true),
			Some(vec![lite::Range {
				start: Some(lite::Anchor::Live {
					min: Some(Duration::ZERO),
					max: Some(Duration::ZERO)
				}),
				end: None,
			}])
		);
	}

	/// A live join already placed goes out as `Joined`, from the frame a switch resumes at,
	/// on lite-07, and as a start at its group on a wire without ranges, whose one max age
	/// still takes its `max`.
	#[test]
	fn a_placed_join_goes_out_joined() {
		let max = Some(Duration::from_secs(2));
		let join = track::Range::from(crate::track::Start::Live {
			min: Some(Duration::from_millis(40)),
			max,
		});
		let mut placed = Subscription {
			ranges: vec![join, join],
			placed: [(1, 5)].into(),
			..Default::default()
		};
		let joined = |frame| lite::Range {
			start: Some(lite::Anchor::Joined {
				at: Position { group: 5, frame },
				max,
			}),
			end: None,
		};
		let live = lite::Range {
			start: Some(lite::Anchor::Live {
				min: Some(Duration::from_millis(40)),
				max,
			}),
			end: None,
		};
		assert_eq!(wire_ranges(&placed, true), Some(vec![live, joined(0)]));
		placed.resume = Some(Position::new(5, 2));
		assert_eq!(wire_ranges(&placed, true), Some(vec![live, joined(2)]));

		placed.ranges.remove(0);
		placed.placed = [(0, 5)].into();
		assert_eq!(placed.max_age(), max);
		let span = wire_span(placed);
		assert_eq!((span.start(), span.end()), (Some(5), None));
		assert_eq!(
			wire_ranges(&span, false),
			Some(vec![lite::Range {
				start: Some(lite::Anchor::Absolute(Position::new(5, 2))),
				end: None,
			}])
		);
	}

	/// A wire without ranges has no Position end, so a switch's frame cap goes out as the whole
	/// group, the update byte for byte the one an uncapped switch sends: lite-06 is the wire a
	/// relay of the previous release speaks.
	#[test]
	fn lite06_caps_a_switch_by_whole_group() {
		use crate::coding::Encoder;
		use crate::lite::Message;
		let uncapped = Subscription::default().with_range(3..6);
		let mut capped = uncapped.clone();
		capped.cap = Some(Position::new(5, 2));
		let update = |subscription: &Subscription| {
			let ranges = wire_ranges(&wire_span(subscription.clone()), false).unwrap();
			let msg = lite::SubscribeUpdate {
				wanted_group: None,
				priority: 0,
				max_delay: None,
				ranges: lite::RangeEdit::Replace(ranges),
				order: track::Order::default(),
			};
			let mut buf = Vec::new();
			msg.encode_msg(&mut Encoder::new(&mut buf, Version::Lite06.into()), Version::Lite06)
				.unwrap();
			buf
		};
		assert_eq!(update(&capped), update(&uncapped));
	}

	/// A wire without ranges asks from group 0 when no single start covers both a range
	/// with no start and one with a start.
	#[test]
	fn wire_span_covers_mixed_starts_from_group_zero() {
		let mixed = Subscription {
			ranges: vec![track::Range::ALL, track::Range::groups(5..6)],
			..Default::default()
		};
		let span = wire_span(mixed);
		assert_eq!((span.start(), span.end()), (Some(0), None));
		assert_eq!(wire_span(Subscription::default()).start(), None);
	}

	/// A subscription that asks for nothing opens nothing.
	///
	/// An end of group 0 is the empty range: nothing sorts below it. The wire cannot
	/// say that, and the nearest thing it can say is "through group 0", which would
	/// deliver the single group the caller excluded.
	#[kio::test]
	async fn an_empty_range_opens_no_subscription() {
		let mut h = Harness::new(Version::Lite06);
		let mut sub = Sub::None;

		let empty = Subscription::default().with_end(0);
		h.serve
			.handle_subscription(&mut h.producer, &mut sub, Some(empty), true)
			.await
			.unwrap();

		assert!(matches!(sub, Sub::None), "must not open a subscription");
		assert!(h.wire().is_empty(), "nothing reached the wire");
	}

	/// Demand collapsing to nothing cancels the upstream rather than sending a bound
	/// that means the opposite.
	#[kio::test]
	async fn an_empty_range_cancels_a_live_subscription() {
		let mut h = Harness::new(Version::Lite06);
		let mut sub = Sub::None;

		h.serve
			.handle_subscription(&mut h.producer, &mut sub, Some(Subscription::default()), true)
			.await
			.unwrap();
		assert!(matches!(sub, Sub::Active(_)), "the first subscriber opens one");
		let established = h.wire().len();

		let empty = Subscription::default().with_end(0);
		h.serve
			.handle_subscription(&mut h.producer, &mut sub, Some(empty), true)
			.await
			.unwrap();

		assert!(matches!(sub, Sub::None), "the upstream must be canceled");
		assert_eq!(h.wire().len(), established, "no SUBSCRIBE_UPDATE claiming group 0");
	}

	/// Bounds that meet anywhere in the track are just as empty as ones that meet at the
	/// first position.
	///
	/// The wire has no encoding for either: an exclusive end at a group head floors to
	/// the group below it, so group 5 through group 5 would go out as `start_group = 5`,
	/// `end_group = 4`, an inverted range the publisher happily parks on.
	#[kio::test]
	async fn a_nonzero_empty_range_opens_no_subscription() {
		let mut h = Harness::new(Version::Lite06);
		let mut sub = Sub::None;

		let empty = Subscription::default().with_start(5).with_end(5);
		h.serve
			.handle_subscription(&mut h.producer, &mut sub, Some(empty), true)
			.await
			.unwrap();

		assert!(matches!(sub, Sub::None), "must not open a subscription");
		assert!(h.wire().is_empty(), "nothing reached the wire");
	}

	/// Demand collapsing to an empty range mid-track cancels the upstream, the same way
	/// it does at the first position.
	#[kio::test]
	async fn a_nonzero_empty_range_cancels_a_live_subscription() {
		let mut h = Harness::new(Version::Lite06);
		let mut sub = Sub::None;

		h.serve
			.handle_subscription(&mut h.producer, &mut sub, Some(Subscription::default()), true)
			.await
			.unwrap();
		assert!(matches!(sub, Sub::Active(_)), "the first subscriber opens one");
		let established = h.wire().len();

		let empty = Subscription::default().with_start(5).with_end(5);
		h.serve
			.handle_subscription(&mut h.producer, &mut sub, Some(empty), true)
			.await
			.unwrap();

		assert!(matches!(sub, Sub::None), "the upstream must be canceled");
		assert_eq!(h.wire().len(), established, "no SUBSCRIBE_UPDATE inverting the range");
	}

	/// Concurrent joins over the refresh ratio send one SUBSCRIBE_UPDATE upstream, a new
	/// group past the largest sends the next, and every update restates the latest.
	#[kio::test]
	async fn refresh_joins_coalesce_upstream() {
		let h = Harness::new(Version::Lite07);
		let request = h._broadcast.reserve_track("refresh").unwrap();
		let mut serving = ServeLoop::new(
			&h.serve,
			request,
			track::Info::default().with_refresh(crate::track::Refresh::default().with_ratio(1)),
		);
		let track = h._broadcast.consume().track("refresh").unwrap();
		let _baseline = track.subscribe(None).await.unwrap();
		assert!(serving.poll(&h.serve, &kio::Waiter::noop()).is_pending());
		assert!(matches!(serving.sub, Sub::Active(_)), "the reader opens the upstream");
		let established = h.wire().len();

		let arrive = |serving: &ServeLoop, key| {
			let mut group = serving.serving.create_group(group::Info { start: key }).unwrap();
			for frame in [&b"key"[..], b"abc", b"d"] {
				group
					.write_frame(crate::Timestamp::unscaled(group.start), frame)
					.unwrap();
			}
			group.finish().unwrap();
		};
		let wanted = |h: &Harness| {
			let wire = h.wire();
			let mut wire = &wire[established..];
			let mut wanted = Vec::new();
			while !wire.is_empty() {
				let msg =
					crate::coding::decode_buf(&mut wire, Version::Lite07, |r, v| lite::SubscribeUpdate::decode(r, v))
						.unwrap();
				wanted.extend(msg.wanted_group);
			}
			wanted
		};

		// The deltas outweigh the first frame, so every join asks for group 1. Each joins
		// after another track's answer, so the joins differ in the tick they wait past.
		arrive(&serving, 0);
		let mut joins = Vec::new();
		for _ in 0..8 {
			crate::model::track::answer_elsewhere();
			joins.push(track.subscribe(None).await.unwrap());
			assert!(serving.poll(&h.serve, &kio::Waiter::noop()).is_pending());
		}
		assert_eq!(wanted(&h), [1], "joins coalesce into one request");

		arrive(&serving, 1);
		joins.push(track.subscribe(None).await.unwrap());
		assert!(serving.poll(&h.serve, &kio::Waiter::noop()).is_pending());
		assert_eq!(wanted(&h), [1, 2], "the requested group re-arms the next request");

		// A parameter change restates the pending wanted group rather than clearing it.
		let _urgent = track.subscribe(Subscription::default().with_priority(9)).await.unwrap();
		assert!(serving.poll(&h.serve, &kio::Waiter::noop()).is_pending());
		assert_eq!(wanted(&h), [1, 2, 2], "every update restates the wanted group");
	}

	/// A buffered SUBSCRIBE_START describes the demand its SUBSCRIBE carried, so
	/// it applies exactly while the current start matches that demand: an update
	/// that moves the start makes it stale (applying it could reopen a range the
	/// publisher no longer serves, or clamp one it still does), and an update
	/// that moves back restores it (the publisher declared that range gone and
	/// sends no replacement START).
	#[kio::test]
	async fn buffered_start_applies_iff_demand_matches() {
		let mut h = Harness::new(Version::Lite05);
		let mut sub = Sub::None;

		let demand = |group: u64| Some(Subscription::default().with_start(group));
		let applies = |sub: &Sub| matches!(sub, Sub::Active(active) if active.start == active.requested);

		// Establish from group 3; the peer's START is considered in flight.
		h.serve
			.handle_subscription(&mut h.producer, &mut sub, demand(3), true)
			.await
			.unwrap();
		assert!(applies(&sub), "a fresh subscription accepts its START");

		// An end-only update leaves the start intact: the START stays valid.
		h.serve
			.handle_subscription(
				&mut h.producer,
				&mut sub,
				Some(Subscription::default().with_start(3).with_end(9)),
				true,
			)
			.await
			.unwrap();
		assert!(applies(&sub), "an unmoved start keeps the START applicable");

		// The start moves: a buffered START is stale while it sits elsewhere.
		h.serve
			.handle_subscription(&mut h.producer, &mut sub, demand(8), true)
			.await
			.unwrap();
		assert!(!applies(&sub), "a moved start must invalidate a buffered START");

		// The start returns: the declaration matches the demand again, so the
		// publisher's skip (it sends no replacement START) must land.
		h.serve
			.handle_subscription(&mut h.producer, &mut sub, demand(3), true)
			.await
			.unwrap();
		assert!(applies(&sub), "demand returning restores the START");
	}

	/// The permanent-miss floor follows the demand in every direction: an update
	/// that moves the start forward retires the skipped range (the publisher
	/// stops serving it and no fresh START says so), one that moves it backward
	/// reopens it, and dropping to the live edge clears it entirely.
	#[kio::test]
	async fn updates_move_the_declared_floor_both_ways() {
		let mut h = Harness::new(Version::Lite05);
		let mut sub = Sub::None;

		let demand = |group: u64| Some(Subscription::default().with_start(group));

		// Establish from group 5: the floor tracks the request until START lands.
		h.serve
			.handle_subscription(&mut h.producer, &mut sub, demand(5), true)
			.await
			.unwrap();
		assert_eq!(h.producer.start_key(), Some(5));

		// Forward: a reader waiting in [5, 8) must fail over, not stall.
		h.serve
			.handle_subscription(&mut h.producer, &mut sub, demand(8), true)
			.await
			.unwrap();
		assert_eq!(h.producer.start_key(), Some(8));

		// Backward: the reopened range must stop being a permanent miss.
		h.serve
			.handle_subscription(&mut h.producer, &mut sub, demand(3), true)
			.await
			.unwrap();
		assert_eq!(h.producer.start_key(), Some(3));

		// Live edge: the floor is unknown until the next declaration, so a
		// group below the stale one must not be a permanent miss.
		h.serve
			.handle_subscription(&mut h.producer, &mut sub, Some(Subscription::default()), true)
			.await
			.unwrap();
		assert_eq!(h.producer.start_key(), None);
	}

	/// A second announce for a live path is a protocol error whatever its hops say. It
	/// must be caught before the reflection drops, because the caller has already bound
	/// an announce id to it: dropping it silently leaves that id pointing at a path
	/// owned by the earlier announce, and the `ANNOUNCE_END` that follows retires the
	/// wrong route.
	#[kio::test]
	async fn a_double_announce_is_an_error_even_when_reflected() {
		let assigned = crate::Hop::new(777).unwrap();
		let origin = origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce();
		let mut subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(SinkSession::new(Default::default())),
			origin,
			recv_bandwidth: None,
			version: VERSION,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			cost: None,
			peer_node: Some(assigned),
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});

		let path = Path::from_static("room/host");
		let mut announced = Announced::default();
		assert!(
			subscriber
				.start_announce(
					path.clone(),
					None,
					crate::Hops::new(),
					crate::origin::Cost::default(),
					0,
					Some(assigned),
					&mut announced,
				)
				.unwrap()
		);

		// The same path again, this time with a chain that names the sender.
		let mut reflected = crate::Hops::new();
		reflected.push(assigned).unwrap();
		assert!(
			matches!(
				subscriber.start_announce(
					path.clone(),
					None,
					reflected,
					crate::origin::Cost::default(),
					0,
					Some(assigned),
					&mut announced,
				),
				Err(Error::ProtocolViolation)
			),
			"the double announce must be reported, not silently dropped",
		);
	}

	/// Announce streams overlapping at a path share the instance minted for it, but a
	/// replacement delivered on both, interleaved, is a new stream on each: holder counts
	/// alone would hand it the old instance, splicing the two.
	#[test]
	fn an_interleaved_replacement_mints_a_fresh_instance() {
		let slots = crate::session::Slots::default();
		let minted = Minted::default();
		let mut first = Announced::new(slots.clone(), minted.clone());
		let mut second = Announced::new(slots, minted.clone());
		let path = Path::from_static("room/host");
		let instance = |announced: &Announced| announced.instance(&path).unwrap().0;

		first.reserve(path.clone(), None).unwrap();
		second.reserve(path.clone(), None).unwrap();
		let old = instance(&first);
		assert!(old.is_minted());
		assert_eq!(instance(&second), old, "one advertisement on two streams is one stream");

		// END1, START1, END2, START2.
		first.withdraw(&path);
		first.reserve(path.clone(), None).unwrap();
		let new = instance(&first);
		assert_ne!(new, old, "the replacement inherited the old instance");
		second.withdraw(&path);
		second.reserve(path.clone(), None).unwrap();
		assert_eq!(
			instance(&second),
			new,
			"both streams carry the replacement as one stream"
		);

		// A stream closing ends nothing: the other's advertisement keeps its instance.
		drop(first);
		let mut third = Announced::new(Default::default(), minted);
		third.reserve(path.clone(), None).unwrap();
		assert_eq!(instance(&third), new);
	}

	/// A serve pass polls only the routes whose request queue woke. The driver runs a
	/// pass on every wake, which is once per group the session carries, so polling
	/// every announced route there made each group cost the whole announce set.
	#[kio::test]
	async fn a_request_readies_only_its_route() {
		let origin = origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce();
		let mut subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(SinkSession::new(Default::default())),
			origin: origin.clone(),
			recv_bandwidth: None,
			version: VERSION,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			cost: None,
			peer_node: Some(crate::Hop::new(777).unwrap()),
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});

		let mut announced = Announced::default();
		for i in 0..64 {
			assert!(
				subscriber
					.start_announce(
						Path::new(&format!("room/{i}")).unwrap(),
						None,
						crate::Hops::new(),
						crate::origin::Cost::default(),
						0,
						None,
						&mut announced,
					)
					.unwrap()
			);
		}

		// Attaching queues each route for its first pass, and that pass leaves none queued.
		assert_eq!(announced.ready.len(), 64);
		announced.poll_serve(&subscriber, &kio::Waiter::noop());
		assert!(announced.ready.is_empty());

		let consumer = origin.consume();
		let request = kio::clock::sim::spawn(async move { consumer.request("room/7", None).await });

		let ready = kio::wait(|waiter| announced.ready.poll_pop(waiter)).await.unwrap();
		assert_eq!(ready.as_str(), "room/7");
		assert!(announced.ready.is_empty(), "only the requested route is ready");

		// Hand it back, as its waker did, and serve it.
		announced.ready.try_push(ready).unwrap();
		announced.poll_serve(&subscriber, &kio::Waiter::noop());
		request.await.unwrap().expect("the requested route serves it");
	}

	/// A source minted under a claim goes once nothing holds it: the front that read it
	/// ended, so the session keeps nothing for the path, and a later request asks the
	/// route afresh. Withdrawing the claim still closes a source in use at once.
	#[kio::test]
	async fn an_unheld_source_is_retired() {
		let origin = origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce();
		let mut subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(SinkSession::new(Default::default())),
			origin: origin.clone(),
			recv_bandwidth: None,
			version: VERSION,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			cost: None,
			peer_node: Some(crate::Hop::new(777).unwrap()),
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});
		let path = Path::from_static("pool");
		let mut announced = Announced::default();
		subscriber
			.start_announce(
				path.clone(),
				None,
				crate::Hops::new(),
				crate::origin::Cost::default(),
				0,
				None,
				&mut announced,
			)
			.unwrap();

		// Serve one request for `pool/p`, returning what the requester resolved and the
		// machine serving the source minted for it.
		async fn serve_one(
			origin: &origin::Producer,
			subscriber: &Subscriber,
			announced: &mut Announced,
		) -> (crate::broadcast::Consumer, SourceServe) {
			let consumer = origin.consume();
			let request = kio::clock::sim::spawn(async move { consumer.request("pool/p", None).await });
			let ready = kio::wait(|waiter| announced.ready.poll_pop(waiter)).await.unwrap();
			announced.ready.try_push(ready).unwrap();
			announced.poll_serve(subscriber, &kio::Waiter::noop());
			let minted = subscriber.sources.try_pop().unwrap().expect("a source was minted");
			let resolved = request.await.unwrap().expect("resolves");
			(resolved, SourceServe::new(subscriber.clone(), minted))
		}

		announced.poll_serve(&subscriber, &kio::Waiter::noop());
		let (resolved, mut serve) = serve_one(&origin, &subscriber, &mut announced).await;
		assert!(kio::Task::poll(&mut serve, &kio::Waiter::noop()).is_pending());

		// The requester leaves without reading: its front ends, and the source with it.
		drop(resolved);
		kio::clock::timeout(
			std::time::Duration::from_secs(1),
			kio::wait(|waiter| kio::Task::poll(&mut serve, waiter)),
		)
		.await
		.expect("the unheld source was retired");

		// Nothing cached it for the path: the route is asked again, and withdrawing the
		// claim closes the new source though its requester still holds it.
		let (_resolved, mut serve) = serve_one(&origin, &subscriber, &mut announced).await;
		assert!(kio::Task::poll(&mut serve, &kio::Waiter::noop()).is_pending());
		announced.withdraw(&path);
		assert!(kio::Task::poll(&mut serve, &kio::Waiter::noop()).is_ready());
	}

	/// A track read before the minted source's serve machine first runs queues for that
	/// machine, rather than the front finding no handler and ending the track `NotFound`.
	#[kio::test]
	async fn a_track_read_before_its_source_is_served_queues() {
		let origin = origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce();
		let mut subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(SinkSession::new(Default::default())),
			origin: origin.clone(),
			recv_bandwidth: None,
			version: VERSION,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			cost: None,
			peer_node: Some(crate::Hop::new(777).unwrap()),
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});
		let mut announced = Announced::default();
		subscriber
			.start_announce(
				Path::from_static("pool"),
				None,
				crate::Hops::new(),
				crate::origin::Cost::default(),
				0,
				None,
				&mut announced,
			)
			.unwrap();
		announced.poll_serve(&subscriber, &kio::Waiter::noop());

		let consumer = origin.consume();
		let request = kio::clock::sim::spawn(async move { consumer.request("pool/p", None).await });
		let ready = kio::wait(|waiter| announced.ready.poll_pop(waiter)).await.unwrap();
		announced.ready.try_push(ready).unwrap();
		announced.poll_serve(&subscriber, &kio::Waiter::noop());
		let resolved = request.await.unwrap().expect("resolves");

		// The front asks the source for the track while its serve machine is still queued.
		let track = resolved.track("video").unwrap();
		let _subscribing = track.subscribe(None);
		kio::clock::sleep(std::time::Duration::from_millis(1)).await;

		let minted = subscriber.sources.try_pop().unwrap().expect("a source was minted");
		let mut serve = SourceServe::new(subscriber.clone(), minted);
		match serve.dynamic.poll_requested_track(&kio::Waiter::noop()) {
			Poll::Ready(Ok(request)) => assert_eq!(request.name(), "video"),
			other => panic!("the track never queued for the source: {:?}", other.map(|r| r.err())),
		}
	}

	/// Every path out of `start_announce` that declines an announce records it first.
	///
	/// The decline paths are a list one edit can fall off the end of, and a miss is silent:
	/// the path reads as free, a later announce takes it, and the declined one's
	/// `ANNOUNCE_END` retires that route instead. This walks a reflection: the chain
	/// already names this session.
	#[kio::test]
	async fn every_declined_announce_is_recorded() {
		let assigned = crate::Hop::new(777).unwrap();
		let origin = origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce();
		let mut subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(SinkSession::new(Default::default())),
			origin,
			recv_bandwidth: None,
			version: Version::Lite03,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			cost: None,
			peer_node: Some(assigned),
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});

		let path = Path::from_static("room/host");
		let mut announced = Announced::default();

		// A chain that already names us is a reflection, declined but still recorded.
		let hops = crate::Hops::try_from(vec![crate::Hop::new(1).unwrap()]).unwrap();
		assert!(
			!subscriber
				.start_announce(
					path.clone(),
					None,
					hops,
					crate::origin::Cost::default(),
					0,
					None,
					&mut announced
				)
				.unwrap(),
			"a chain that already names this session must be declined",
		);

		// Declined, but still the peer's advertisement at that path.
		let mut fresh = crate::Hops::new();
		fresh.push(crate::Hop::new(7).unwrap()).unwrap();
		assert!(
			matches!(
				subscriber.start_announce(
					path.clone(),
					None,
					fresh,
					crate::origin::Cost::default(),
					0,
					None,
					&mut announced
				),
				Err(Error::ProtocolViolation)
			),
			"a declined announce still holds its path, so a second start for it is a violation",
		);
	}

	/// A dropped announce still holds its path until the peer retracts it.
	#[kio::test]
	async fn a_dropped_announce_still_holds_its_path() {
		let assigned = crate::Hop::new(777).unwrap();
		let origin = origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce();
		let consumer = origin.consume();
		let mut subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(SinkSession::new(Default::default())),
			origin,
			recv_bandwidth: None,
			version: VERSION,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			cost: None,
			peer_node: Some(assigned),
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});

		let path = Path::from_static("room/host");
		let mut announced = Announced::default();
		let mut reflected = crate::Hops::new();
		reflected.push(assigned).unwrap();
		assert!(
			!subscriber
				.start_announce(
					path.clone(),
					None,
					reflected,
					crate::origin::Cost::default(),
					0,
					Some(assigned),
					&mut announced,
				)
				.unwrap(),
			"a chain naming its own sender must be dropped",
		);
		assert!(consumer.get_broadcast("room/host").is_none());

		let mut hops = crate::Hops::new();
		hops.push(crate::Hop::new(7).unwrap()).unwrap();
		let err = subscriber
			.start_announce(
				path.clone(),
				None,
				hops,
				crate::origin::Cost::default(),
				0,
				Some(assigned),
				&mut announced,
			)
			.expect_err("a second start for the peer-owned path must be rejected");
		assert!(matches!(err, Error::ProtocolViolation));
	}

	/// An announce whose chain already names the sender came back through the sender.
	/// Appending it again would name one identity twice, which is tolerated in a lite
	/// chain but is a PROTOCOL_VIOLATION for an IETF peer the route is later forwarded
	/// to, so the route must never enter the model carrying it.
	#[kio::test]
	async fn an_announce_reflected_by_its_sender_is_dropped() {
		let assigned = crate::Hop::new(777).unwrap();

		let origin = origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce();
		let consumer = origin.consume();
		let mut subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(SinkSession::new(Default::default())),
			origin,
			recv_bandwidth: None,
			version: VERSION,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			cost: None,
			peer_node: Some(assigned),
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});

		// The sender's identity is already in the chain: the route came back through it.
		let mut hops = crate::Hops::new();
		hops.push(assigned).unwrap();

		let mut announced = Announced::default();
		let accepted = subscriber
			.start_announce(
				Path::from_static("room/host"),
				None,
				hops,
				crate::origin::Cost::default(),
				0,
				Some(assigned),
				&mut announced,
			)
			.unwrap();
		assert!(!accepted, "a chain naming its own sender must not become a route");

		kio::clock::sleep(std::time::Duration::from_millis(1)).await;
		assert!(consumer.get_broadcast("room/host").is_none());
	}

	/// A peer that was advertised a local path and announces it back with this
	/// origin's hop in the chain is a reflection, not a new path. The announce
	/// is dropped, the local front keeps serving, and the peer's own
	/// subscription (split-horizon excluded) still reads from it.
	#[kio::test]
	async fn a_reflected_announce_does_not_displace_the_local_front() {
		let relay = crate::Hop::new(1).unwrap();
		let origin = origin::Config::from(relay).produce();
		let assigned = crate::Hop::new(777).unwrap();

		let local = origin
			.route("room/host", crate::origin::RouteOptions::default())
			.unwrap();
		let track = local
			.broadcast()
			.create_track("video", crate::track::Info::timed())
			.unwrap();
		let mut group = track.append_next().unwrap();
		group.write_frame(group.key_time(), b"local".as_ref()).unwrap();
		group.finish().unwrap();

		// The peer's own subscription, excluding the hop the server minted for
		// it, is served from the local front before anything is announced back.
		let peer = origin.consume().excluding(assigned);
		let resolved = peer.request("room/host", None).await.expect("resolves");
		let mut sub = resolved
			.track("video")
			.unwrap()
			.subscribe(None)
			.await
			.expect("subscribe");
		let mut group = sub.recv_group().await.expect("recv group").expect("track ended early");
		assert_eq!(
			&group.read_frame().await.expect("read frame").expect("frame").payload[..],
			b"local"
		);

		let mut subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(SinkSession::new(Default::default())),
			origin: origin.clone(),
			recv_bandwidth: None,
			version: VERSION,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			cost: None,
			peer_node: Some(assigned),
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});

		// The path as advertised to the peer: our hop is already in the chain.
		let mut hops = crate::Hops::new();
		hops.push(relay).unwrap();
		let mut announced = Announced::default();
		let accepted = subscriber
			.start_announce(
				Path::from_static("room/host"),
				None,
				hops,
				crate::origin::Cost::default(),
				0,
				Some(assigned),
				&mut announced,
			)
			.unwrap();
		assert!(!accepted, "an announce that already names this origin must be dropped");

		// The local front is still the one at the path, and still serving: the
		// peer's next request joins it rather than minting another.
		let still = peer
			.request("room/host", None)
			.await
			.expect("the local front keeps serving");
		assert!(
			still.is_clone(&resolved),
			"the reflected announce must not replace the local front"
		);

		let mut group = track.append_next().unwrap();
		group.write_frame(group.key_time(), b"still".as_ref()).unwrap();
		group.finish().unwrap();
		let mut group = sub.recv_group().await.expect("recv group").expect("track ended early");
		assert_eq!(
			&group.read_frame().await.expect("read frame").expect("frame").payload[..],
			b"still"
		);
	}

	/// A peer that declares no identity is marked anonymous (hop 0). The assigned
	/// identity stays on `via` for split-horizon and is never written into the chain.
	#[kio::test]
	async fn assigned_peer_node_attributes_announces() {
		let session = SinkSession::new(Default::default());
		let assigned = crate::Hop::new(777).unwrap();

		let origin = origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce();
		let consumer = origin.consume();
		let mut subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(session.clone()),
			origin,
			recv_bandwidth: None,
			version: VERSION,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			peer_node: Some(assigned),
			cost: None,
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});

		// An announce with an empty chain and no responder id: the versions that
		// carry no hop information on the wire.
		let mut announced = Announced::default();
		let accepted = subscriber
			.start_announce(
				Path::from_static("room/host"),
				None,
				crate::Hops::new(),
				crate::origin::Cost::UNKNOWN,
				0,
				None,
				&mut announced,
			)
			.unwrap();
		assert!(accepted);

		// The route is announced synchronously: hop 0 on the wire, assigned id local.
		let mut cursor = consumer.routes();
		let route = cursor.expect_next_active("room/host").await;
		let hops: Vec<_> = route.hops.iter().copied().collect();
		assert_eq!(hops, vec![crate::Hop::UNKNOWN]);
		assert!(route.is_anonymous());

		let mut hidden = consumer.excluding(assigned).routes();
		hidden.assert_next_wait();
	}

	/// Lite03 hop-count placeholders stay 0 and count as anonymous; they are not
	/// rewritten with the assigned identity.
	#[kio::test]
	async fn lite03_placeholders_stay_anonymous() {
		let assigned = crate::Hop::new(777).unwrap();
		let origin = origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce();
		let consumer = origin.consume();
		let mut subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(SinkSession::new(Default::default())),
			origin,
			recv_bandwidth: None,
			version: Version::Lite03,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			cost: None,
			peer_node: Some(assigned),
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});

		let hops = crate::Hops::try_from(vec![crate::Hop::UNKNOWN, crate::Hop::UNKNOWN]).unwrap();
		let mut announced = Announced::default();
		assert!(
			subscriber
				.start_announce(
					Path::from_static("room/host"),
					None,
					hops,
					crate::origin::Cost::UNKNOWN,
					0,
					None,
					&mut announced,
				)
				.unwrap()
		);

		let mut cursor = consumer.routes();
		let route = cursor.expect_next_active("room/host").await;
		let hops: Vec<_> = route.hops.iter().copied().collect();
		assert_eq!(hops, vec![crate::Hop::UNKNOWN, crate::Hop::UNKNOWN]);
		assert!(route.is_anonymous());
	}

	/// A publisher that names nobody keeps the anonymous mark, and nothing in front of it:
	/// naming the stream is the route instance's job, so the relay never names the publisher. A
	/// reprice on the same connection keeps it and updates in place.
	#[kio::test]
	async fn an_unnamed_publisher_stays_anonymous() {
		let (mut subscriber, consumer) = restart_subscriber(SinkSession::new(Default::default()), false);

		let mut announced = Announced::default();
		subscriber
			.start_announce(
				Path::from_static("room/host"),
				None,
				crate::Hops::new(),
				crate::origin::Cost::UNKNOWN,
				0,
				Some(crate::Hop::UNKNOWN),
				&mut announced,
			)
			.unwrap();

		let mut cursor = consumer.routes();
		kio::clock::sim::yield_now().await;
		let route = cursor.expect_next_active("room/host").await;
		let hops: Vec<_> = route.hops.iter().copied().collect();
		assert_eq!(hops, vec![crate::Hop::UNKNOWN]);

		// A reprice from the same unnamed publisher stays anonymous and in place.
		subscriber
			.update_announce(
				Path::from_static("room/host"),
				crate::Hops::new(),
				crate::origin::Cost::UNKNOWN,
				4,
				Some(crate::Hop::UNKNOWN),
				&mut announced,
			)
			.unwrap();
		kio::clock::sim::yield_now().await;
		let route = cursor.expect_next_active("room/host").await;
		let hops: Vec<_> = route.hops.iter().copied().collect();
		assert_eq!(hops, vec![crate::Hop::UNKNOWN]);
	}

	/// A subscriber on a fresh origin, which forwards what it learned when `proxy`.
	pub(super) fn restart_subscriber(session: SinkSession, proxy: bool) -> (Subscriber, crate::origin::Consumer) {
		let mut config = origin::Config::new(std::num::NonZeroU64::new(1).unwrap());
		config.proxy = proxy;
		let origin = config.produce();
		let consumer = origin.consume();
		let subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(session.clone()),
			origin,
			recv_bandwidth: None,
			version: VERSION,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			cost: None,
			peer_node: None,
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});
		(subscriber, consumer)
	}

	/// An announce stream that dies without an explicit `ended` retracts the route
	/// as promptly as an explicit retraction: a route into a dead session must not
	/// stay announced.
	///
	/// This falls out of `Announced` being a local whose announcements drop,
	/// which is exactly what makes it worth pinning: a refactor that hoisted the map
	/// to the session (outliving the stream) would leak the announcement instead.
	#[kio::test]
	async fn a_lost_announce_stream_retracts_the_route() {
		let origin = crate::origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce();
		let consumer = origin.consume();
		let mut subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(SinkSession::new(Default::default())),
			origin,
			recv_bandwidth: None,
			version: VERSION,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			cost: None,
			peer_node: None,
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});

		let path = Path::from_static("room/host");
		let hops = crate::Hops::try_from(vec![crate::Hop::new(7).unwrap()]).unwrap();
		let mut announced = Announced::default();
		subscriber
			.start_announce(
				path.clone(),
				None,
				hops,
				crate::origin::Cost::default(),
				1,
				None,
				&mut announced,
			)
			.unwrap();
		let mut cursor = consumer.routes();
		cursor.expect_next_active("room/host").await;

		// The stream ends without retracting anything: the map dies with it and the
		// route retracts.
		drop(announced);
		cursor.expect_next_offline("room/host").await;

		// An explicit retraction retracts it the same way.
		let hops = crate::Hops::try_from(vec![crate::Hop::new(7).unwrap()]).unwrap();
		let mut announced = Announced::default();
		subscriber
			.start_announce(
				path.clone(),
				None,
				hops,
				crate::origin::Cost::default(),
				1,
				None,
				&mut announced,
			)
			.unwrap();
		cursor.expect_next_active("room/host").await;
		assert!(announced.contains(&path.clone()), "the announce was not recorded");
		announced.withdraw(&path);
		cursor.expect_next_ended("room/host").await;
	}

	/// lite-07 defines every message its Announce Stream carries, so an unknown type closes
	/// the session instead of being skipped as on lite-06.
	#[kio::test]
	async fn an_unknown_lite07_announce_type_closes_the_session() {
		const VERSION: Version = Version::Lite07;
		let mut script = announce_script(VERSION, &[&announce_ok(VERSION, 0), &announce_start(VERSION, "a")]);
		// An unknown type with an empty body, then a start that must never be read.
		script.extend([0x3f, 0x00]);
		script.extend(announce_script(VERSION, &[&announce_start(VERSION, "b")]));
		let session = crate::lite::test_transport::ScriptedSession::new(script);
		let log = session.log.clone();
		let (mut routes, _consumer) = root_prefix(VERSION, session);
		let result = routes.poll(&kio::Waiter::noop());
		assert!(
			matches!(result, Poll::Ready(Err(Error::ProtocolViolation))),
			"{result:?}"
		);
		assert_eq!(log.closes()[0].0, crate::SessionError::ProtocolViolation.to_code());
	}

	/// An unknown announce type between two starts is skipped without ending the
	/// stream.
	#[kio::test]
	async fn an_unknown_announce_type_keeps_the_stream() {
		const VERSION: Version = Version::Lite06;
		let start = |suffix| lite::AnnounceBroadcast::Active {
			suffix: lite::PathRef::literal(Path::new(suffix).unwrap()),
			hops: crate::Hops::new(),
			cost: crate::origin::Cost::default(),
		};
		let mut script = Vec::new();
		lite::AnnounceOk {
			origin: crate::Hop::new(9).unwrap(),
			active: 2,
		}
		.encode(&mut crate::coding::Encoder::new(&mut script, VERSION.into()), VERSION)
		.unwrap();
		start("a")
			.encode(&mut crate::coding::Encoder::new(&mut script, VERSION.into()), VERSION)
			.unwrap();
		// An unknown announce type with an empty body, which decodes as `Skipped`.
		script.extend([0x3f, 0x00]);
		start("b")
			.encode(&mut crate::coding::Encoder::new(&mut script, VERSION.into()), VERSION)
			.unwrap();

		let origin = origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce();
		let consumer = origin.consume();
		let subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(crate::lite::test_transport::ScriptedSession::new(script)),
			origin,
			recv_bandwidth: None,
			version: VERSION,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			cost: Some(1),
			peer_node: None,
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});
		let mut prefix = AnnouncePrefix::new(
			subscriber.clone(),
			Path::from_static(""),
			subscriber.origin.snapshot(""),
		);
		let mut cursor = consumer.routes();

		let mut run = std::pin::pin!(kio::wait(|waiter| prefix.poll(waiter)));
		assert!(futures::poll!(run.as_mut()).is_pending());

		cursor.expect_next_active("a").await;
		cursor.expect_next_active("b").await;
	}

	/// A peer's announce stream for the root, encoded for `version`.
	fn announce_script(version: Version, messages: &[&dyn Fn(&mut crate::coding::Encoder<'_>)]) -> Vec<u8> {
		let mut script = Vec::new();
		for message in messages {
			message(&mut crate::coding::Encoder::new(&mut script, version.into()));
		}
		script
	}

	/// An ANNOUNCE_OK counting `active` starts into the initial set, or a lite-07 one
	/// answering the root's request.
	fn announce_ok(version: Version, active: u64) -> impl Fn(&mut crate::coding::Encoder<'_>) {
		move |w| {
			if version.has_routes() {
				return lite::AnnounceBroadcast::Ok.encode(w, version).unwrap();
			}
			lite::AnnounceOk {
				origin: crate::Hop::new(9).unwrap(),
				active,
			}
			.encode(w, version)
			.unwrap()
		}
	}

	/// A lite-07 claim `above` segments broader than the path it is shown at.
	fn claim(above: u64) -> crate::route::node::Claim {
		crate::route::relay::Claim {
			item: crate::route::node::Attr { epoch: 0, above },
			cost: 0,
			priority: i8::MAX,
			draining: false,
			unavailable: false,
			chain: Vec::new(),
			marked: false,
		}
	}

	/// An ANNOUNCE_START for `suffix`.
	fn announce_start(version: Version, suffix: &'static str) -> impl Fn(&mut crate::coding::Encoder<'_>) {
		move |w| {
			let suffix = lite::PathRef::literal(Path::new(suffix).unwrap());
			let start = match version.has_routes() {
				true => lite::AnnounceBroadcast::Start {
					suffix,
					instance: crate::Instance::random(),
					claim: claim(0),
				},
				false => lite::AnnounceBroadcast::Active {
					suffix,
					hops: crate::Hops::new(),
					cost: crate::origin::Cost::default(),
				},
			};
			start.encode(w, version).unwrap()
		}
	}

	/// The lite-07 message `message`.
	fn wire(message: lite::AnnounceBroadcast) -> impl Fn(&mut crate::coding::Encoder<'_>) {
		move |w| message.encode(w, Version::Lite07).unwrap()
	}

	/// What reads a peer's announces of the root: its stream, or from lite-07 the
	/// session's route layer.
	enum Announcing {
		Prefix(Box<AnnouncePrefix>),
		Routes(Box<(route_stream::RouteStream, Subscriber)>),
	}

	impl Announcing {
		fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
			match self {
				Self::Prefix(prefix) => prefix.poll(waiter),
				Self::Routes(routes) => routes.0.poll(waiter).map(Err),
			}
		}

		fn poll_run(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
			match self {
				Self::Prefix(prefix) => prefix.poll_run(waiter),
				Self::Routes(routes) => routes.0.poll_run(waiter),
			}
		}
	}

	/// The announce stream for the root of a fresh origin, its peer replaying `session`.
	fn root_prefix(
		version: Version,
		session: crate::lite::test_transport::ScriptedSession,
	) -> (Announcing, origin::Consumer) {
		root_prefix_counted(version, session, Default::default())
	}

	/// [`root_prefix`], counting the session's stats into `stats`.
	fn root_prefix_counted(
		version: Version,
		session: crate::lite::test_transport::ScriptedSession,
		stats: crate::stats::Session,
	) -> (Announcing, origin::Consumer) {
		let origin = origin::Config::new(std::num::NonZeroU64::new(1).unwrap())
			.produce()
			.with_stats(stats);
		prefix_on(version, session, origin)
	}

	/// [`root_prefix`] into `origin`.
	fn prefix_on(
		version: Version,
		session: crate::lite::test_transport::ScriptedSession,
		origin: origin::Producer,
	) -> (Announcing, origin::Consumer) {
		let consumer = origin.consume();
		let subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(session.clone()),
			origin,
			recv_bandwidth: None,
			version,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			cost: Some(1),
			peer_node: None,
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});
		let snapshot = subscriber.origin.snapshot("");
		if !version.has_routes() {
			let prefix = AnnouncePrefix::new(subscriber.clone(), Path::from_static(""), snapshot);
			return (Announcing::Prefix(Box::new(prefix)), consumer);
		}
		// The peer's SETUP arrived, and it opened its Announce Stream.
		subscriber.peer_setup.set(lite::Setup::default()).unwrap();
		let recv = crate::transport::boxed::RecvStream::new(session.recv());
		let _ = subscriber
			.announce_streams
			.try_push(route_stream::Reader::new(recv, version));
		let mut routes = route_stream::RouteStream::new(subscriber.clone(), route_stream::Publish::idle());
		routes.want(Path::empty(), snapshot);
		(Announcing::Routes(Box::new((routes, subscriber))), consumer)
	}

	/// A lite-07 peer keeps one Announce Stream for the session: a second one closes the
	/// session, and so does one it resets. One it finishes means it is leaving, which
	/// ends its routes and keeps the session.
	#[kio::test]
	async fn a_lite07_peer_keeps_one_announce_stream() {
		const VERSION: Version = Version::Lite07;
		use crate::lite::test_transport::{Close, ScriptedSession};
		let script = || announce_script(VERSION, &[&announce_ok(VERSION, 0), &announce_start(VERSION, "room")]);
		let waiter = kio::Waiter::noop();

		let session = ScriptedSession::new(script());
		let (mut routes, consumer) = root_prefix(VERSION, session.clone());
		let mut cursor = consumer.routes();
		assert!(routes.poll(&waiter).is_pending());
		cursor.assert_next_active("room");
		let Announcing::Routes(running) = &routes else {
			unreachable!("lite-07 runs the route layer");
		};
		let subscriber = &running.1;
		let second = crate::transport::boxed::RecvStream::new(session.recv());
		let _ = (subscriber.announce_streams).try_push(route_stream::Reader::new(second, VERSION));
		let result = routes.poll(&waiter);
		assert!(
			matches!(result, Poll::Ready(Err(Error::ProtocolViolation))),
			"a second stream: {result:?}"
		);
		assert_eq!(
			session.log.closes()[0].0,
			crate::SessionError::ProtocolViolation.to_code()
		);

		let session = ScriptedSession::new(script());
		let (mut routes, _consumer) = root_prefix(VERSION, session.clone());
		assert!(routes.poll(&waiter).is_pending());
		session.close(Close::ResetStream(7));
		let result = routes.poll(&waiter);
		assert!(
			matches!(result, Poll::Ready(Err(Error::ProtocolViolation))),
			"a reset stream: {result:?}"
		);

		// A read that fails with the session's close, before the session reports it ending,
		// ends with that close, as a WebTransport reader learns of the close first.
		let session = ScriptedSession::new(script());
		let (mut routes, _consumer) = root_prefix(VERSION, session.clone());
		assert!(routes.poll(&waiter).is_pending());
		session.close(Close::Reset);
		let result = routes.poll(&waiter);
		assert!(
			matches!(result, Poll::Ready(Err(Error::Session(_)))),
			"a session's close: {result:?}"
		);
		assert!(session.log.closes().is_empty(), "{:?}", session.log.closes());

		let session = ScriptedSession::new(script());
		let (mut routes, consumer) = root_prefix(VERSION, session.clone());
		let mut cursor = consumer.routes();
		assert!(routes.poll(&waiter).is_pending());
		cursor.assert_next_active("room");
		session.close(Close::Fin);
		assert!(routes.poll(&waiter).is_pending());
		assert!(session.log.closes().is_empty(), "{:?}", session.log.closes());
		// Unavailable until the reader's debounce runs out, on a timer the origin's driver
		// fires.
		kio::clock::sleep(crate::route::relay::debounce(crate::route::relay::HOP) * 2).await;
		consumer.route_node().timeout();
		assert!(routes.poll(&waiter).is_pending());
		cursor.expect_next_offline("room").await;
	}

	/// Nothing comes between the ANNOUNCE_REQUESTs of one edit.
	#[kio::test]
	async fn a_message_inside_an_edit_closes_the_session() {
		const VERSION: Version = Version::Lite07;
		let part = |w: &mut crate::coding::Encoder<'_>| {
			lite::AnnounceBroadcast::Request {
				more: true,
				remove: Vec::new(),
				add: vec![(Path::from_static("a"), false)],
			}
			.encode(w, VERSION)
			.unwrap()
		};
		let script = announce_script(
			VERSION,
			&[&announce_ok(VERSION, 0), &part, &announce_start(VERSION, "room")],
		);
		let (mut routes, _consumer) = root_prefix(VERSION, crate::lite::test_transport::ScriptedSession::new(script));
		let result = routes.poll(&kio::Waiter::noop());
		assert!(
			matches!(result, Poll::Ready(Err(Error::ProtocolViolation))),
			"{result:?}"
		);
	}

	/// A grant that widens to cover a claim the answer already held back sends it at once,
	/// though the node queues nothing behind it to flush it out.
	#[kio::test]
	async fn a_widened_grant_flushes_the_claim_it_shows() {
		use crate::model::ProduceTest as _;
		const VERSION: Version = Version::Lite07;
		let publish = origin::Config::new(std::num::NonZeroU64::new(2).unwrap()).produce();
		let route = publish.route("room", crate::origin::RouteOptions::hidden()).unwrap();
		route.announce().unwrap();
		// The peer asks for the whole tree, hidden paths included.
		let request = |w: &mut crate::coding::Encoder<'_>| {
			lite::AnnounceBroadcast::Request {
				more: false,
				remove: Vec::new(),
				add: vec![(Path::empty(), true)],
			}
			.encode(w, VERSION)
			.unwrap()
		};
		let session = crate::lite::test_transport::ScriptedSession::new(announce_script(VERSION, &[&request]));
		// Only the half answering the peer is limited, so a change to it is all that happens.
		let auth = crate::auth::Link::new(false);
		auth.handle().authorize(&crate::auth::Grant::default());
		let subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(session.clone()),
			origin: origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce(),
			recv_bandwidth: None,
			version: VERSION,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			cost: Some(1),
			peer_node: None,
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});
		subscriber.peer_setup.set(lite::Setup::default()).unwrap();
		let recv = crate::transport::boxed::RecvStream::new(session.recv());
		let _ = (subscriber.announce_streams).try_push(route_stream::Reader::new(recv, VERSION));
		let mut routes = route_stream::RouteStream::new(
			subscriber.clone(),
			route_stream::Publish {
				origin: publish.consume(),
				auth: auth.clone(),
				withdrawal: Default::default(),
				routing: false,
			},
		);
		let written = || session.log.writes.lock().unwrap().len();
		// Run until everything parks: the request answered, the claim held back.
		let settled = kio::clock::timeout(
			std::time::Duration::from_millis(1),
			kio::wait(|waiter| routes.poll(waiter).map(|err| panic!("{err:?}"))),
		)
		.await;
		assert!(settled.is_err(), "the session ended");
		let held = written();

		// The grant change is the one wakeup: the pass it runs must send what it shows,
		// since nothing else is queued to wake the session again.
		auth.handle().authorize(&crate::auth::Grant::all());
		assert!(routes.poll(&kio::Waiter::noop()).is_pending());
		assert!(written() > held, "the claim the grant now shows stayed buffered");
	}

	/// A peer streaming announces gets a budget of them read per poll, so the session's
	/// other tasks run between them, and the announce behind the flood still arrives.
	#[kio::test]
	async fn a_flood_of_announces_yields() {
		const VERSION: Version = Version::Lite06;
		/// `AnnouncePrefix`'s announces per poll.
		const BUDGET: usize = 32;
		const SKIPS: usize = 4096;
		let mut script = announce_script(VERSION, &[&announce_ok(VERSION, 0)]);
		for _ in 0..SKIPS {
			// An unknown announce type with an empty body: no change.
			script.extend([0x3f, 0x00]);
		}
		script.extend(announce_script(VERSION, &[&announce_start(VERSION, "a")]));
		let (mut prefix, consumer) = root_prefix(VERSION, crate::lite::test_transport::ScriptedSession::new(script));
		let mut cursor = consumer.routes();

		let mut polls = 0;
		kio::wait(|waiter| {
			assert!(prefix.poll(waiter).is_pending(), "the announce stream ended");
			polls += 1;
			if cursor.try_next().is_some() {
				return Poll::Ready(());
			}
			assert!(polls <= SKIPS, "the announces stalled");
			Poll::Pending
		})
		.await;
		assert!(polls * BUDGET >= SKIPS, "{SKIPS} announces read in {polls} polls");
	}

	/// The origin's consumers sync once the counted starts are in and read, never before.
	#[kio::test]
	async fn a_counted_initial_set_catches_up() {
		const VERSION: Version = Version::Lite06;
		let script = announce_script(
			VERSION,
			&[
				&announce_ok(VERSION, 2),
				&announce_start(VERSION, "a"),
				&announce_start(VERSION, "b"),
			],
		);
		let (mut prefix, consumer) = root_prefix(VERSION, crate::lite::test_transport::ScriptedSession::new(script));
		let mut cursor = consumer.routes();
		assert!(!cursor.is_synced(), "held from the request on");

		let mut run = std::pin::pin!(kio::wait(|waiter| prefix.poll(waiter)));
		assert!(futures::poll!(run.as_mut()).is_pending());

		cursor.assert_next_active("a");
		assert!(!cursor.is_synced(), "never before an earlier event is read");
		cursor.assert_next_active("b");
		assert!(cursor.is_synced());
		// A consumer created after the boundary syncs on its replay.
		let mut late = consumer.routes();
		late.assert_next_active("a");
		late.assert_next_active("b");
		assert!(late.is_synced());
	}

	/// Part of a counted set is not synced, however much of it was read.
	#[kio::test]
	async fn a_partial_initial_set_holds() {
		const VERSION: Version = Version::Lite06;
		let script = announce_script(VERSION, &[&announce_ok(VERSION, 2), &announce_start(VERSION, "a")]);
		let (mut prefix, consumer) = root_prefix(VERSION, crate::lite::test_transport::ScriptedSession::new(script));
		let mut cursor = consumer.routes();
		let mut run = std::pin::pin!(kio::wait(|waiter| prefix.poll(waiter)));
		assert!(futures::poll!(run.as_mut()).is_pending());

		cursor.assert_next_active("a");
		assert!(!cursor.is_synced());
	}

	/// An empty lite-07 set waits for the ordered SYNCED event.
	#[kio::test]
	async fn an_empty_initial_set_catches_up_at_once() {
		const VERSION: Version = Version::Lite07;
		let synced = |w: &mut crate::coding::Encoder<'_>| {
			lite::AnnounceBroadcast::Synced { prefix: 0 }
				.encode(w, VERSION)
				.unwrap()
		};
		let script = announce_script(VERSION, &[&announce_ok(VERSION, 0), &synced]);
		let (mut prefix, consumer) = root_prefix(VERSION, crate::lite::test_transport::ScriptedSession::new(script));
		let cursor = consumer.routes();
		let mut run = std::pin::pin!(kio::wait(|waiter| prefix.poll(waiter)));
		assert!(futures::poll!(run.as_mut()).is_pending());
		assert!(cursor.is_synced());
	}

	/// A complete body cannot be repaired by bytes from the next announcement.
	/// Keep the peer stream open: FIN must not be what discovers the malformed body.
	#[kio::test]
	async fn malformed_complete_announce_closes_before_more_bytes() {
		for version in [Version::Lite05, Version::Lite06, Version::Lite07] {
			let mut script = announce_script(version, &[&announce_ok(version, 1)]);
			script.extend_from_slice(match version {
				Version::Lite05 => &[1, 1],
				_ => &[0, 1, 5],
			});
			let session = crate::lite::test_transport::ScriptedSession::new(script);
			let log = session.log.clone();
			let (mut prefix, _consumer) = root_prefix(version, session);
			let result = prefix.poll(&kio::Waiter::noop());
			assert!(
				matches!(result, Poll::Ready(Err(Error::ProtocolViolation))),
				"{version:?}: {result:?}"
			);
			assert_eq!(log.closes()[0].0, crate::SessionError::ProtocolViolation.to_code());
		}
	}

	/// Only an incomplete outer frame waits, then resumes on its missing byte.
	#[kio::test]
	async fn an_incomplete_announce_waits_for_its_body() {
		for version in [Version::Lite05, Version::Lite06, Version::Lite07] {
			let mut script = announce_script(version, &[&announce_ok(version, 1), &announce_start(version, "room")]);
			let last = script.pop().unwrap();
			let session = crate::lite::test_transport::ScriptedSession::new(script);
			let log = session.log.clone();
			let (mut prefix, consumer) = root_prefix(version, session.clone());
			let mut cursor = consumer.routes();
			assert!(prefix.poll(&kio::Waiter::noop()).is_pending());
			assert!(log.closes().is_empty());
			session.push(&[last]);
			assert!(prefix.poll(&kio::Waiter::noop()).is_pending());
			cursor.assert_next_active("room");
			assert!(log.closes().is_empty());
		}
	}

	/// A lite-07 peer that declared no Node ID answering the root with the claims `a`
	/// and `b`.
	fn two_claims() -> Vec<u8> {
		const VERSION: Version = Version::Lite07;
		announce_script(
			VERSION,
			&[
				&announce_ok(VERSION, 0),
				&announce_start(VERSION, "a"),
				&announce_start(VERSION, "b"),
			],
		)
	}

	/// A stats registry and one session counting into it.
	fn counted() -> (crate::stats::Registry, crate::stats::Session) {
		let registry = crate::stats::Registry::new(crate::stats::Config::new());
		let session = (registry.tier(crate::stats::Tier::default()))
			.session(crate::stats::SessionInfo::new(Path::from_static("peer")));
		(registry, session)
	}

	/// The claims `registry`'s sessions refused, by reason.
	fn refusals(registry: &crate::stats::Registry) -> crate::stats::RouteRefusals {
		let mut refused = crate::stats::RouteRefusals::default();
		for (_, presence) in registry.snapshot().sessions() {
			assert_eq!(presence.routes_refused, presence.routes.refused.total());
			refused.add(presence.routes.refused);
		}
		refused
	}

	/// A lite-07 ANNOUNCE_START at `suffix`, `above` segments broader than it.
	fn start_at(suffix: lite::PathRef, above: u64) -> impl Fn(&mut crate::coding::Encoder<'_>) {
		wire(lite::AnnounceBroadcast::Start {
			suffix,
			instance: crate::Instance::random(),
			claim: claim(above),
		})
	}

	/// Fields valid alone can still resolve past the path limits: that claim is refused
	/// and counted, keeps its Announce ID, and the stream keeps going.
	#[kio::test]
	async fn a_claim_resolved_too_deep_is_refused() {
		let session = crate::lite::test_transport::ScriptedSession::new(two_claims());
		let log = session.log.clone();
		let (registry, stats) = counted();
		let (mut prefix, consumer) = root_prefix_counted(Version::Lite07, session.clone(), stats);
		let mut cursor = consumer.routes();
		assert!(prefix.poll(&kio::Waiter::noop()).is_pending());
		cursor.assert_next_active("a");
		cursor.assert_next_active("b");

		// Id 2 sits at the depth limit; id 3 keeps all of it and adds a segment.
		let deep = vec!["s"; Path::MAX_PARTS].join("/");
		let past = lite::PathRef {
			base: 1,
			keep: Path::MAX_PARTS as u64,
			rest: Path::from_static("z"),
		};
		session.push(&announce_script(
			Version::Lite07,
			&[
				&start_at(lite::PathRef::literal(Path::new(&deep).unwrap()), 0),
				&start_at(past, 0),
				&announce_start(Version::Lite07, "c"),
				// The refused claim still holds id 3.
				&wire(lite::AnnounceBroadcast::End { id: 3, lost: false }),
			],
		));
		assert!(prefix.poll(&kio::Waiter::noop()).is_pending());
		assert!(log.closes().is_empty(), "{:?}", log.closes());
		// The node serves a burst in the order of its paths.
		cursor.assert_next_active("c");
		cursor.assert_next_active(&deep);
		cursor.assert_next_wait();
		let refused = refusals(&registry);
		assert_eq!((refused.path, refused.total()), (1, 1), "{refused:?}");
	}

	/// A peer that declared no Node ID and sends a route closes the session, counted under
	/// its label as a close for its role.
	#[kio::test]
	async fn a_route_from_a_peer_without_a_node_counts_its_close() {
		let node = crate::route::NodeId::new(7);
		let offer = crate::route::relay::Offer {
			path: vec![crate::route::relay::Hop { node, session: 0 }].into(),
			draining: false,
			priority: i8::MAX,
			cost: 0,
			marked: false,
		};
		let mut script = two_claims();
		script.extend(announce_script(
			Version::Lite07,
			&[&wire(lite::AnnounceBroadcast::Route { offer })],
		));
		let session = crate::lite::test_transport::ScriptedSession::new(script);
		let (registry, stats) = counted();
		let (mut prefix, _consumer) = root_prefix_counted(Version::Lite07, session, stats);
		let err = kio::wait(|waiter| prefix.poll_run(waiter)).await.unwrap_err();
		assert!(matches!(err, Error::ProtocolViolation), "{err}");
		let closed: Vec<_> = (registry.snapshot().sessions().into_iter())
			.map(|(_, presence)| presence.routes.closed)
			.collect();
		assert_eq!(closed.len(), 1);
		assert_eq!((closed[0].role, closed[0].total()), (1, 1), "{closed:?}");
	}

	/// A claim shown below the requested prefix may still be broader than its path, as a
	/// relay forwards one it learned from a narrower stream; one broader than its path has
	/// segments cannot be, and closes the session.
	#[kio::test]
	async fn a_claim_above_its_path_stays_within_the_path() {
		let at = || lite::PathRef::literal(Path::from_static("x/y"));
		let session = crate::lite::test_transport::ScriptedSession::new(two_claims());
		let (mut prefix, consumer) = root_prefix(Version::Lite07, session.clone());
		let mut cursor = consumer.routes();
		assert!(prefix.poll(&kio::Waiter::noop()).is_pending());
		cursor.assert_next_active("a");
		cursor.assert_next_active("b");
		session.push(&announce_script(Version::Lite07, &[&start_at(at(), 2)]));
		assert!(prefix.poll(&kio::Waiter::noop()).is_pending());
		cursor.assert_next_active("x/y");

		let mut script = two_claims();
		script.extend(announce_script(Version::Lite07, &[&start_at(at(), 3)]));
		let (mut prefix, _consumer) = root_prefix(
			Version::Lite07,
			crate::lite::test_transport::ScriptedSession::new(script),
		);
		let err = kio::wait(|waiter| prefix.poll_run(waiter)).await.unwrap_err();
		assert!(matches!(err, Error::ProtocolViolation), "{err}");
	}

	/// The bound counts the segments of the path as the peer names it, beneath the session's
	/// root: under the root `tenant`, a claim at `x` is at most one above, though its absolute
	/// path has two segments.
	#[kio::test]
	async fn a_claim_above_its_path_counts_from_the_sessions_root() {
		for (above, closes) in [(1, false), (2, true)] {
			let mut script = two_claims();
			let at = lite::PathRef::literal(Path::from_static("x"));
			script.extend(announce_script(Version::Lite07, &[&start_at(at, above)]));
			let origin = origin::Config::new(std::num::NonZeroU64::new(1).unwrap())
				.produce()
				.scope("tenant", &crate::Prefixes::all())
				.unwrap();
			let session = crate::lite::test_transport::ScriptedSession::new(script);
			let (mut prefix, _consumer) = prefix_on(Version::Lite07, session, origin);
			match closes {
				true => {
					let err = kio::wait(|waiter| prefix.poll_run(waiter)).await.unwrap_err();
					assert!(matches!(err, Error::ProtocolViolation), "above {above}: {err}");
				}
				false => assert!(prefix.poll(&kio::Waiter::noop()).is_pending(), "above {above}"),
			}
		}
	}

	/// A claim its publisher withdrew ends, and its Announce ID is retired: naming
	/// it again, or one the peer never assigned, closes the session.
	#[kio::test]
	async fn an_unknown_announce_id_closes_the_session() {
		let end = |id| lite::AnnounceBroadcast::End { id, lost: false };
		let session = crate::lite::test_transport::ScriptedSession::new(two_claims());
		let (mut prefix, consumer) = root_prefix(Version::Lite07, session.clone());
		let mut cursor = consumer.routes();
		assert!(prefix.poll(&kio::Waiter::noop()).is_pending());
		cursor.assert_next_active("a");
		cursor.assert_next_active("b");
		session.push(&announce_script(Version::Lite07, &[&wire(end(0))]));
		assert!(prefix.poll(&kio::Waiter::noop()).is_pending());
		cursor.expect_next_ended("a").await;

		let change = |id| lite::AnnounceBroadcast::Change { id, claim: claim(0) };
		for (what, message) in [
			("an end of a retired id", end(0)),
			("a change of a retired id", change(0)),
			("an end of an unassigned id", end(9)),
			("a change of an unassigned id", change(9)),
		] {
			let mut script = two_claims();
			script.extend(announce_script(Version::Lite07, &[&wire(end(0)), &wire(message)]));
			let (mut prefix, _consumer) = root_prefix(
				Version::Lite07,
				crate::lite::test_transport::ScriptedSession::new(script),
			);
			let err = kio::wait(|waiter| prefix.poll_run(waiter)).await.unwrap_err();
			assert!(matches!(err, Error::ProtocolViolation), "{what}: {err}");
		}
	}

	/// Two nodes exchange routes only when both declared a Node ID: from any other peer a
	/// route, its end, a floor, a dead hop, or a drain closes the session.
	#[kio::test]
	async fn a_peer_that_declared_no_node_sends_no_routes() {
		use crate::route::relay::{Hop, NodeId, Offer};
		let node = NodeId {
			relay: 7,
			marked: false,
		};
		let hop = Hop { node, session: 0 };
		let offer = Offer {
			path: [hop].into(),
			draining: false,
			priority: i8::MAX,
			cost: 0,
			marked: false,
		};
		for (what, message) in [
			("ROUTE", lite::AnnounceBroadcast::Route { offer }),
			("ROUTE_END", lite::AnnounceBroadcast::RouteEnd { id: 0 }),
			("ANNOUNCE_FLOOR", lite::AnnounceBroadcast::Floor { id: 0, revision: 1 }),
			("DEAD", lite::AnnounceBroadcast::Dead { hop, far: node }),
			("DRAIN", lite::AnnounceBroadcast::Drain { node }),
		] {
			let mut script = two_claims();
			script.extend(announce_script(Version::Lite07, &[&wire(message)]));
			let (mut prefix, _consumer) = root_prefix(
				Version::Lite07,
				crate::lite::test_transport::ScriptedSession::new(script),
			);
			let err = kio::wait(|waiter| prefix.poll_run(waiter)).await.unwrap_err();
			assert!(matches!(err, Error::ProtocolViolation), "{what}: {err}");
		}
	}

	#[kio::test]
	async fn a_duplicate_synced_is_refused() {
		const VERSION: Version = Version::Lite07;
		let synced = |w: &mut crate::coding::Encoder<'_>| {
			lite::AnnounceBroadcast::Synced { prefix: 0 }
				.encode(w, VERSION)
				.unwrap()
		};
		let script = announce_script(VERSION, &[&announce_ok(VERSION, 0), &synced, &synced]);
		let (mut prefix, _consumer) = root_prefix(VERSION, crate::lite::test_transport::ScriptedSession::new(script));
		let err = kio::wait(|waiter| prefix.poll_run(waiter)).await.unwrap_err();
		assert!(matches!(err, Error::ProtocolViolation));
	}

	/// Anything but a start inside the counted set cuts it short: a protocol violation.
	#[kio::test]
	async fn an_end_inside_the_initial_set_is_refused() {
		const VERSION: Version = Version::Lite06;
		let end =
			|w: &mut crate::coding::Encoder<'_>| lite::AnnounceBroadcast::EndedId { id: 0 }.encode(w, VERSION).unwrap();
		let script = announce_script(
			VERSION,
			&[&announce_ok(VERSION, 2), &announce_start(VERSION, "a"), &end],
		);
		let (mut prefix, _consumer) = root_prefix(VERSION, crate::lite::test_transport::ScriptedSession::new(script));
		let err = kio::wait(|waiter| prefix.poll_run(waiter)).await.unwrap_err();
		assert!(matches!(err, Error::ProtocolViolation), "{err}");
	}

	/// The stream ending inside the counted set cuts it short too.
	#[kio::test]
	async fn a_fin_inside_the_initial_set_is_refused() {
		const VERSION: Version = Version::Lite06;
		let script = announce_script(VERSION, &[&announce_ok(VERSION, 2), &announce_start(VERSION, "a")]);
		let session = crate::lite::test_transport::ScriptedSession::new(script);
		session.close(crate::lite::test_transport::Close::Fin);
		let (mut prefix, _consumer) = root_prefix(VERSION, session);
		let err = kio::wait(|waiter| prefix.poll_run(waiter)).await.unwrap_err();
		assert!(matches!(err, Error::ProtocolViolation), "{err}");
	}

	/// Lite03/04 mark no initial set: synced, best effort, once the request is sent.
	#[kio::test]
	async fn an_unmarked_version_catches_up_on_the_request() {
		const VERSION: Version = Version::Lite04;
		let (mut prefix, consumer) =
			root_prefix(VERSION, crate::lite::test_transport::ScriptedSession::new(Vec::new()));
		let cursor = consumer.routes();
		assert!(!cursor.is_synced());
		let mut run = std::pin::pin!(kio::wait(|waiter| prefix.poll(waiter)));
		assert!(futures::poll!(run.as_mut()).is_pending());
		assert!(cursor.is_synced());
	}

	/// Lite01/02 mark the initial set with ANNOUNCE_INIT.
	#[kio::test]
	async fn announce_init_is_the_initial_set() {
		const VERSION: Version = Version::Lite02;
		let init = |w: &mut crate::coding::Encoder<'_>| {
			lite::AnnounceInit {
				suffixes: vec![Path::from_static("a")],
			}
			.encode(w, VERSION)
			.unwrap()
		};
		let (mut prefix, consumer) = root_prefix(
			VERSION,
			crate::lite::test_transport::ScriptedSession::new(announce_script(VERSION, &[&init])),
		);
		let mut cursor = consumer.routes();
		let mut run = std::pin::pin!(kio::wait(|waiter| prefix.poll(waiter)));
		assert!(futures::poll!(run.as_mut()).is_pending());
		assert!(!cursor.is_synced());
		cursor.assert_next_active("a");
		assert!(cursor.is_synced());
	}

	/// An announce past the session's cap fails the session with TOO_MANY_REQUESTS, and
	/// the stream gives its slots back once the session drops it.
	#[kio::test]
	async fn announces_past_the_cap_close_the_session() {
		const VERSION: Version = Version::Lite06;
		let start = |suffix| lite::AnnounceBroadcast::Active {
			suffix: lite::PathRef::literal(Path::new(suffix).unwrap()),
			hops: crate::Hops::new(),
			cost: crate::origin::Cost::default(),
		};
		let mut script = Vec::new();
		lite::AnnounceOk {
			origin: crate::Hop::new(9).unwrap(),
			active: 3,
		}
		.encode(&mut crate::coding::Encoder::new(&mut script, VERSION.into()), VERSION)
		.unwrap();
		for suffix in ["a", "b", "c"] {
			start(suffix)
				.encode(&mut crate::coding::Encoder::new(&mut script, VERSION.into()), VERSION)
				.unwrap();
		}

		let origin = origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce();
		let session = crate::lite::test_transport::ScriptedSession::new(script);
		let mut subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(session.clone()),
			origin,
			recv_bandwidth: None,
			version: VERSION,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			cost: Some(1),
			peer_node: None,
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});
		let slots = crate::session::Slots::new(2);
		subscriber.announces = slots.clone();
		let mut prefix = AnnouncePrefix::new(
			subscriber.clone(),
			Path::from_static(""),
			subscriber.origin.snapshot(""),
		);

		let err = kio::wait(|waiter| prefix.poll(waiter))
			.await
			.expect_err("an announce past the cap must fail the session");
		assert_eq!(crate::SessionError::from(&err), crate::SessionError::TooManyRequests);

		// The session's end drops the stream, which gives its slots back.
		drop(prefix);
		let _a = slots.acquire().expect("the failed stream kept a slot");
		let _b = slots.acquire().expect("the failed stream kept a slot");
	}

	/// Distinct live GROUP starts are copies of the same group, including while
	/// readers retain every earlier tail. Exercise the real header/ingest path.
	#[kio::test]
	async fn live_group_tails_share_content_held_by_earlier_readers() {
		use crate::transport::poll::Session as _;
		let version = Version::Lite06;
		let session = crate::lite::test_transport::ScriptedSession::eof(Vec::new());
		let subscriber = Subscriber::new(SubscriberConfig {
			runtime: crate::time::sim(),
			session: crate::driver::Transport::new(session),
			origin: origin::Config::new(std::num::NonZeroU64::new(1).unwrap()).produce(),
			recv_bandwidth: None,
			version,
			peer_setup: crate::lite::PeerSetup::new(false, crate::transport::Address::Setup),
			peer_node: None,
			cost: None,
			going_away: Default::default(),
			auth: crate::auth::Link::new(false),
		});
		let track = track::Producer::new(
			std::sync::Arc::new(crate::broadcast::Info::default()),
			"video",
			crate::track::Info::timed(),
		);
		subscriber.subscribes.lock().insert(
			7,
			TrackEntry::accepted(
				track.clone(),
				lite::Framing {
					timescale: Some(Timescale::MILLI),
					ietf: None,
					compressed: false,
					length_first: false,
				},
				Default::default(),
			),
		);
		let mut held = Vec::new();
		for start in (1..=8).rev() {
			let mut script = Vec::new();
			let mut encoder = crate::coding::Encoder::new(&mut script, version.into());
			lite::Group {
				subscribe: 7,
				key: 0,
				frame_start: start,
			}
			.encode(&mut encoder, version)
			.unwrap();
			for index in start..9 {
				encoder.varint(crate::coding::varint::zigzag(0)).unwrap();
				encoder.bytes(&vec![index as u8; 1024]).unwrap();
			}
			let incoming = crate::lite::test_transport::ScriptedSession::eof(script);
			let (_, recv) = incoming.open_bi(Default::default()).await.unwrap();
			let mut group = GroupRecv::new(
				subscriber.clone(),
				Reader::new(crate::transport::boxed::RecvStream::new(recv), version),
			);
			kio::wait(|waiter| group.poll_serve(waiter)).await.unwrap();
			held.push((start, track.consume().peek_group(0).unwrap().with_range(start..)));
		}
		let mut pointers = std::collections::BTreeMap::new();
		for (start, reader) in &mut held {
			for index in *start..9 {
				assert_eq!(reader.index(), index);
				let frame = reader.read_frame().await.unwrap().unwrap();
				assert_eq!(frame.payload.as_ref(), vec![index as u8; 1024]);
				if let Some(prior) = pointers.insert(index, frame.payload.as_ptr() as usize) {
					assert_eq!(
						prior,
						frame.payload.as_ptr() as usize,
						"live GROUP frame {index} has one physical payload across held tails"
					);
				}
			}
			assert!(reader.read_frame().await.unwrap().is_none());
		}
		assert!(track.closed().now_or_never().is_none());
	}
}

/// The ranges a SUBSCRIBE carries for `subscription`, without the empty ones.
///
/// A group ID is the group's start, so a time anchor goes out as the group ID it names,
/// on every version: a time start as the start, from the frame it resumes at (see
/// [`Subscription::resume`]), and a time end as the exclusive group past it, which lite-07
/// carries as the inclusive time again (see [`lite::Anchor::Absolute`]), or before the frame
/// it is capped at (see [`Subscription::cap`]). A placed live join goes out as
/// [`lite::Anchor::Joined`] from the frame it resumes at too, which only lite-07 carries: the
/// narrowing below makes it a time start first on an older wire.
/// `None` when nothing is left to ask for, which no wire can say. The caller narrows a
/// subscription for a wire without ranges to the one span it carries first (see
/// [`wire_span`]).
///
/// A wire without ranges reads a start as the first group at or after it, so a time start
/// between two groups misses the one containing it there.
fn wire_ranges(subscription: &Subscription, ranged: bool) -> Option<Vec<lite::Range>> {
	let mut ranges = Vec::with_capacity(subscription.ranges.len());
	// A switch resumes the group it starts at past the frames the readers hold.
	let resumed = |group| match subscription.resume {
		Some(resume) if resume.group == group => resume,
		_ => Position::group(group),
	};
	for (index, range) in (0..).zip(&subscription.ranges) {
		if range.is_empty() {
			continue;
		}
		// One from the oldest group goes upstream with no start, for the max age to place there,
		// and a live join as itself, for the upstream to place behind its own live edge, or
		// where it was already placed.
		let start = match (range.start, subscription.placed.get(&index)) {
			(track::Start::Live { max, .. }, Some(group)) => Some(lite::Anchor::Joined {
				at: resumed(*group),
				max,
			}),
			(track::Start::Oldest, _) => None,
			(track::Start::Live { min, max }, None) => Some(lite::Anchor::Live { min, max }),
			// Exact group placement (Position) or pre-lite-07 wire (which has no Time anchor).
			(track::Start::Time(_), Some(group)) => Some(lite::Anchor::Absolute(resumed(*group))),
			(track::Start::Time(time), None) if !ranged => Some(lite::Anchor::Absolute(resumed(time))),
			// Lite-07 Time anchor: containment lookup, unless resuming mid-group at that exact start.
			(track::Start::Time(time), None) => match subscription.resume {
				Some(resume) if resume.group == time && resume.frame != 0 => Some(lite::Anchor::Absolute(resume)),
				_ => Some(lite::Anchor::Time(time)),
			},
		};
		// A switch caps the copy it replaces where the new one resumes, before a frame of the
		// group it ends at.
		let end = range.end.and_then(|time| match subscription.cap {
			Some(cap) if cap.group == time && cap.frame != 0 => Some(lite::Anchor::Absolute(cap)),
			_ if ranged => (time < u64::MAX).then_some(lite::Anchor::Time(time)),
			_ => Some(lite::Anchor::Absolute(Position::group(time.checked_add(1)?))),
		});
		ranges.push(lite::Range { start, end });
	}
	(!ranges.is_empty()).then_some(ranges)
}

/// `subscription` narrowed to the one span a wire without ranges carries, which the
/// subscribers' own reads narrow again.
///
/// It starts at the lowest start. A subscription with only ranges without a start asks with
/// none, which a pre-06 publisher reads as the latest group and a lite-06 one as no floor,
/// under the widest max age among them, the one `Subscriber Max Age` the wire carries: a
/// live join with no lag limit (see [`track::Start::within`]), or none while a
/// range from the oldest group asks for history. One mixing the two asks from group 0,
/// history, since no single start covers both: the latest group may sit below the start.
///
/// It ends at a group's end, never at a Position: a publisher without ranges caps its cursor
/// there and FINs the group, which would read as the group finishing short. A copy a switch
/// replaces sends its boundary group whole there instead (see [`Subscription::cap`]).
fn wire_span(mut subscription: Subscription) -> Subscription {
	subscription.cap = None;
	let start = match subscription.start() {
		Some(start) => Some(start),
		None if subscription.times().any(|(_, time)| time.is_some()) => Some(0),
		None => None,
	};
	let max = subscription.max_age();
	let mut span = subscription.with_start(start);
	for range in &mut span.ranges {
		if let (track::Start::Oldest, Some(max)) = (range.start, max) {
			range.start = track::Start::within(max);
		}
	}
	span
}

/// The at-most-one live upstream subscription: its control stream plus the params
/// echoed in every SUBSCRIBE_UPDATE.
struct SubStream {
	stream: Stream<Transport, Version>,
	id: u64,
	/// The `Subscriber Max Age` a wire before lite-07 carries, echoed in every
	/// SUBSCRIBE_UPDATE; refreshed as the downstream aggregate changes.
	max_delay: Option<Duration>,
	/// The narrowest live join's max age this subscription has asked, which a lite-07 stale
	/// drop is recorded under: the wire does not say which range or update a drop crossed, so
	/// it may have been judged under any live join's asked before it arrived, or the
	/// publisher's max age, which binds every reader alike. Too narrow costs a wider reader a
	/// request of its own; too wide would settle it on a group it could still get.
	stale_budget: Duration,
	/// The exclusive group end every live range the SUBSCRIBE and each SUBSCRIBE_UPDATE asked
	/// ended at, `None` once one followed the live edge or two ended apart: the cap a lite-07
	/// stale drop is recorded within, so a reader capped alike settles on it. Like the budget,
	/// the wire does not say which update a drop crossed, so a cap that ever varied settles no
	/// capped reader.
	stale_cap: Option<u64>,
	/// The lowest absolute start among the ranges, or `None` when one counts back from
	/// the latest group: the floor below which nothing is asked for.
	start: Option<u64>,
	/// The ranges on the wire by Range ID, never empty: the demand's, its probes, then the
	/// fills'.
	live: lite::LiveRanges,
	/// The demand the ranges ask for, `None` when they ask only for fills.
	demand: Option<Subscription>,
	/// The Range IDs of the fills' ranges; the rest are the demand's, which alone set where
	/// the live feed is. Held by ID, since a fill may ask for exactly what a demand range does,
	/// and until SUBSCRIBE_START even once retired, since it answers every range the
	/// SUBSCRIBE carried.
	fill_ids: std::collections::BTreeSet<u64>,
	/// The Range IDs of the probes: a range of its own from each time start the copy's readers
	/// wait on (see [`Subscription::asks`]), asking only where the publisher starts it, and
	/// one for each live join they wait on that no unanswered range asks (see
	/// [`Subscription::joins`]). Like a fill's, it says nothing of where the live feed is. Held
	/// until SUBSCRIBE_START even once retired, as [`Self::fill_ids`] is.
	probe_ids: std::collections::BTreeSet<u64>,
	/// The groups the ranges ask for fills.
	fill_groups: rangemap::RangeSet<u64>,
	priority: i8,
	order: crate::track::Order,
	/// The start the SUBSCRIBE itself carried, fixed for the stream's life. A
	/// SUBSCRIBE_START describes this demand and no fresh one follows an update,
	/// so a buffered START only applies while `start` still equals it: while the
	/// start sits elsewhere the request-tracked floor stands instead, and demand
	/// returning here makes the declaration valid again.
	requested: Option<u64>,
	/// The groups received for this subscription, shared with its [`TrackEntry`].
	tail: kio::Producer<Tail>,
	/// The first group the publisher serves, once SUBSCRIBE_START says: `Some(None)` when it
	/// starts no range.
	started: Option<Option<u64>>,
	/// Whether a lite-07 SUBSCRIBE_START took the copy live: later ones only move its edge.
	went_live: bool,
	/// The tail of a lite-07 subscription whose publisher sent SUBSCRIBE_END: once it
	/// settles the copy finishes and FINs, while demand and responses keep flowing meanwhile.
	finishing: Option<Settle>,
	/// A pipelined subscription's group streams stay parked until the responses that beat the
	/// accept are read, as they would have been without the wait: a SUBSCRIBE_START says where
	/// the feed starts before its first group does.
	parked: bool,
	/// Where each live range's SUBSCRIBE_START says its delivery starts, by
	/// Range ID, `None` for one with nothing to deliver.
	starts: std::collections::BTreeMap<u64, Option<u64>>,
	/// The tick the copy recorded each live join range's answer at, by Range ID (see
	/// [`track::Producer::answer_join`]): one with none is still unanswered.
	join_ticks: std::collections::BTreeMap<u64, u64>,
	/// The narrowest max age each live join range was asked under before its answer, or each
	/// placed one ever, by Range ID, beside the one it asks now: its publisher starts it past the groups
	/// that max expired, so the answer places only readers within it.
	join_budgets: std::collections::BTreeMap<u64, Duration>,
	/// What [`Self::declare`] last declared on the copy, so an unchanged one is not declared
	/// again: each declaration restarts the grace of the gaps above it.
	declared: Option<(Option<u64>, bool)>,
	/// The track's exclusive end (SUBSCRIBE_END), once declared.
	end: Option<lite::SubscribeEnd>,
	/// The refresh join's wanted group, restated in every SUBSCRIBE_UPDATE.
	wanted: Option<u64>,
	/// Until the publisher's first response, what the source must still hold: a SUBSCRIBE
	/// without an instance resolves whichever one wins the path when it arrives, so one
	/// still unanswered once another instance takes the path may be answered by it.
	unanswered: Option<crate::model::broadcast::RequestValidity>,
	/// Holds the session busy for as long as the subscription is open.
	_working: crate::session::Working,
}

impl SubStream {
	/// Whether a lite-07 publisher still owes SUBSCRIBE_START once it ended the track: it
	/// answers even when no group resolved the start, unless nothing was asked below the end.
	fn awaits_start(&self) -> bool {
		self.started.is_none() && self.end.as_ref().is_some_and(|end| end.group > self.start.unwrap_or(0))
	}

	/// Lite-07's declaration of where the live feed starts: the lowest start the demand's live
	/// ranges report, or the requested floor, pending, while SUBSCRIBE_START or a range's
	/// SUBSCRIBE_START has yet to say where one starts. A range added once the feed started
	/// leaves it standing while it can't start below it (see [`starts_past`]). `None` before
	/// lite-07, whose one start describes only the SUBSCRIBE's demand, and while the ranges ask
	/// only for fills.
	fn feed_start(&self) -> Option<(Option<u64>, bool)> {
		if !self.live.reports_starts() || self.demand.is_none() {
			return None;
		}
		let feeding = || self.live.iter().filter(|(id, _)| self.feeds(*id));
		let mut start: Option<u64> = None;
		let mut unanswered = false;
		for (id, _) in feeding() {
			match self.starts.get(&id) {
				// A range with no group there serves nothing to start from.
				Some(Some(at)) => start = Some(start.map_or(*at, |start| start.min(*at))),
				Some(None) => {}
				None => unanswered = true,
			}
		}
		let lower = |range: &lite::Range| match (self.started, start) {
			(Some(_), Some(start)) => !starts_past(range, start),
			_ => true,
		};
		if unanswered && feeding().any(|(id, range)| !self.starts.contains_key(&id) && lower(range)) {
			return Some((self.start, true));
		}
		Some((start, self.started.is_none()))
	}

	/// Tell the copy where the publisher started each live range: one asked from a group, or
	/// from the oldest group as time 0, which a reader's time start there resolves to when the
	/// copy can't place it among its groups; and one asked as a live join, whose answer alone
	/// places that join for the copy's readers, whatever max age each asks.
	/// Lite-07 only, where each range's start is reported.
	fn report_starts(&mut self, serving: &track::Producer) {
		if !self.live.reports_starts() {
			return;
		}
		let mut times = Vec::new();
		for (id, range) in self.live.iter() {
			let Some(&start) = self.starts.get(&id) else {
				continue;
			};
			let asked = match range.start {
				Some(lite::Anchor::Time(time)) => time,
				// A max age, now or under a retune since, may start it past groups the publisher
				// still holds, as stale for this join alone, so its answer proves nothing of history.
				// Its readers are placed already.
				Some(lite::Anchor::Joined { at, max }) => {
					let asked = self.join_budgets.get(&id).copied().unwrap_or(Duration::MAX);
					if asked.min(crate::track::budget(max)) < Duration::MAX {
						continue;
					}
					at.group
				}
				None => 0,
				Some(lite::Anchor::Live { min, max }) => {
					// Recorded once, as it arrives: a reader that began waiting since takes only a
					// newer answer.
					if let std::collections::btree_map::Entry::Vacant(entry) = self.join_ticks.entry(id) {
						let asked = self.join_budgets.get(&id).copied().unwrap_or(Duration::MAX);
						let budget = asked.min(crate::track::budget(max));
						entry.insert(serving.answer_join(min, through(range.end), start, budget));
					}
					continue;
				}
				// Position (Absolute) matches the exact group only, never proving time containment.
				Some(lite::Anchor::Absolute(_)) => continue,
			};
			// Whether a range with no group to deliver says its publisher held none at or before
			// its reach: only one whose end takes in the group containing its time, which a
			// range that ends before its time may not, and an open one always does.
			let reaches = match range.end {
				Some(lite::Anchor::Absolute(end)) if end.frame == 0 => end.group > asked,
				Some(lite::Anchor::Absolute(end)) => end.group >= asked,
				Some(lite::Anchor::Time(end)) => end >= asked,
				Some(lite::Anchor::Live { .. } | lite::Anchor::Joined { .. }) => false,
				None => true,
			};
			// One with no group to deliver says nothing of its time unless it reaches it.
			if start.is_some() || reaches {
				times.push((asked, start));
			}
		}
		serving.time_starts(times);
		let live = &self.live;
		serving.retain_joins(|min, end| live.iter().any(|(_, range)| wire_join(range) == Some((min, end))));
	}

	/// Declare on the copy where the live feed starts, by [`Self::feed_start`], so a reader
	/// waiting on each group in key starts there rather than at whatever arrives first.
	fn declare(&mut self, serving: &mut track::Producer) {
		let Some(next) = self.feed_start() else {
			return;
		};
		if self.declared.replace(next) == Some(next) {
			return;
		}
		let _ = match next {
			(start, true) => serving.request_start(start),
			(Some(start), false) => serving.start_resolved(start),
			(None, false) => serving.start_at(None),
		};
	}

	/// `largest`, when this bounded subscription's ranges, as SUBSCRIBE_START resolved them,
	/// stop short of it, so the feed never delivers it. The range and arrival ledger then wait
	/// for the groups it owes instead of an edge outside the requested history. A feed that
	/// serves Largest waits for it, so an old cache is judged against that edge. A range still
	/// unanswered is not waited on: going live is decided once, and it may yet answer with no
	/// group, which would leave the wait unreleased, while the recorded Largest still bounds reads.
	fn never_serves(&self, largest: Option<Position>) -> Option<Position> {
		largest.filter(|largest| {
			self.live.iter().all(|(_, range)| range.end.is_some())
				&& self
					.spans()
					.is_none_or(|spans| !spans.iter().any(|span| span.contains(&largest.group)))
		})
	}

	/// Where the live range `id` delivers from, `None` while nothing says yet, and `Some(None)`
	/// for a range with no group to deliver.
	///
	/// Lite-07 reports every range's start: SUBSCRIBE_START the SUBSCRIBE's, and
	/// SUBSCRIBE_START each one an update added. Before it, the one start describes the
	/// SUBSCRIBE's range, so a range an update replaced it with delivers from its absolute
	/// start, or else from where delivery started.
	fn range_start(&self, id: u64, range: &lite::Range) -> Option<Option<u64>> {
		if let Some(start) = self.starts.get(&id) {
			return Some(*start);
		}
		if self.live.reports_starts() {
			return None;
		}
		match range.start {
			Some(lite::Anchor::Absolute(start)) => Some(Some(start.group)),
			Some(lite::Anchor::Time(time)) => Some(Some(time)),
			_ => self.started.flatten().or(self.start).map(Some),
		}
	}

	/// Where each bounded range delivers, as its reported start resolved it, up to its end.
	/// `None` when a range is open, or nothing says yet where one starts.
	fn spans(&self) -> Option<Vec<std::ops::Range<u64>>> {
		self.live
			.iter()
			.map(|(id, range)| {
				range.end?;
				let end = through(range.end).map_or(0, |last| last.saturating_add(1));
				// Each range's own start accounts for its skipped prefix.
				let start = self.range_start(id, range)?.unwrap_or(end);
				Some(start..end)
			})
			.collect()
	}

	/// The groups the publisher still owes once it has ended the subscription, one run per
	/// range: the publisher accounts for nothing between disjoint ranges.
	///
	/// `None` when nothing says which: drafts before SUBSCRIBE_END only have the FIN, and a
	/// lite-07 range an update added owes nothing known until SUBSCRIBE_START says where it
	/// starts. Without a SUBSCRIBE_START the publisher served no group at all. Each range is
	/// owed from its reported start (see [`Self::range_start`]), which accounts for a skipped
	/// prefix and resolves a relative or time start. The groups the SUBSCRIBE asked for below
	/// its SUBSCRIBE_START are accounted for as unavailable when it arrives.
	///
	/// Before lite-07 nothing answers an update, and the publisher applies none once it has
	/// sent its FIN, so a floor an update lowered may never have reached it: nothing below where
	/// SUBSCRIBE_START served from is owed. One that arrives before the tail settles is still
	/// delivered.
	fn owed(&self, requested_end: Option<u64>) -> Option<Vec<std::ops::Range<u64>>> {
		let end = self.end.as_ref()?.group;
		let end = requested_end.map_or(end, |requested| requested.min(end));
		if self.started.is_none() {
			return Some(Vec::new());
		}
		let answered = match self.live.reports_starts() {
			true => None,
			false => self.started.flatten(),
		};
		let mut owed = Vec::new();
		for (id, range) in self.live.iter() {
			let Some(start) = self.range_start(id, range)? else {
				continue;
			};
			let start = answered.map_or(start, |answered| start.max(answered));
			let stop = match range.end {
				Some(_) => through(range.end).map_or(0, |last| last.saturating_add(1)).min(end),
				None => end,
			};
			if start < stop {
				owed.push(start..stop);
			}
		}
		Some(owed)
	}

	/// The groups the live range `id` asks for a fill, `None` for one of the demand's.
	fn fill_range(&self, id: u64) -> Option<std::ops::Range<u64>> {
		if !self.fill_ids.contains(&id) {
			return None;
		}
		let range = self.live.get(id)?;
		match (range.start, range.end) {
			(Some(lite::Anchor::Absolute(start)), Some(lite::Anchor::Absolute(end))) => {
				let stop = match end.frame {
					0 => end.group,
					_ => end.group.saturating_add(1),
				};
				Some(start.group..stop)
			}
			_ => None,
		}
	}

	/// Whether the live range `id` is the demand's, which alone says where the live feed is:
	/// neither a fill's nor a probe.
	fn feeds(&self, id: u64) -> bool {
		!self.fill_ids.contains(&id) && !self.probe_ids.contains(&id)
	}

	/// Edit the live ranges into `ranges`, the demand's then `probes` probes, then the last
	/// `fills` of which are the held fills', forgetting the starts of those retired. A fill or
	/// probe asking for what a demand range does still takes a range of its own, so each Range
	/// ID stays the demand's, a probe, or a fill's. A live join asked under another max age
	/// alone is retuned in place, keeping its answer, and an unanswered one remembers the
	/// narrowest it was asked under. Returns the edit, and whether it changed the demand's or
	/// the fills' ranges.
	///
	/// `reasked` retires each of those live probes and adds it afresh: a live join answered
	/// before a reader began waiting on it.
	fn edit(
		&mut self,
		ranges: &[lite::Range],
		(probes, fills): (usize, usize),
		reasked: &std::collections::BTreeSet<u64>,
	) -> Result<(lite::RangeEdit, bool), Error> {
		let before: Vec<_> = self
			.live
			.iter()
			.map(|(id, _)| id)
			.filter(|id| !self.probe_ids.contains(id))
			.collect();
		let demand = ranges.len().saturating_sub(fills).saturating_sub(probes);
		let kind = |index: usize| match index {
			index if index < demand => Kind::Demand,
			index if index < demand + probes => Kind::Probe,
			_ => Kind::Fill,
		};
		// Keyed by kind, and whether it is a range to ask afresh, which nothing wanted matches.
		let keyed: Vec<_> = ranges
			.iter()
			.enumerate()
			.map(|(index, range)| ((kind(index), false), *range))
			.collect();
		let stale = reasked;
		let (fill_ids, probe_ids) = (&self.fill_ids, &self.probe_ids);
		let kind_of = |id: u64| match (fill_ids.contains(&id), probe_ids.contains(&id)) {
			(true, _) => Kind::Fill,
			(_, true) => Kind::Probe,
			_ => Kind::Demand,
		};
		// Before a retune moves it: the publisher may answer under the max age asked until then.
		for (id, range) in self.live.iter() {
			let unanswered = match range.start {
				Some(lite::Anchor::Live { max, .. }) if !self.join_ticks.contains_key(&id) => Some(max),
				// Every max a placed join was ever asked under, answered or not: a retune keeps its
				// answer, which proves history only if none was finite.
				Some(lite::Anchor::Joined { max, .. }) => Some(max),
				_ => None,
			};
			if let Some(max) = unanswered {
				let asked = self.join_budgets.entry(id).or_insert(Duration::MAX);
				*asked = (*asked).min(crate::track::budget(max));
			}
		}
		let (edit, added) = self.live.edit_keyed(&keyed, |id| (kind_of(id), stale.contains(&id)))?;
		let live = &self.live;
		self.starts.retain(|id, _| live.get(*id).is_some());
		self.join_ticks.retain(|id, _| live.get(*id).is_some());
		self.join_budgets.retain(|id, _| live.get(*id).is_some());
		if self.started.is_some() {
			self.fill_ids.retain(|id| live.get(*id).is_some());
			self.probe_ids.retain(|id| live.get(*id).is_some());
		}
		self.stale_budget = self.stale_budget.min(narrowest(&self.live));
		if cap_of(&self.live) != self.stale_cap {
			self.stale_cap = None;
		}
		for (id, (kind, _)) in added {
			match kind {
				Kind::Fill => _ = self.fill_ids.insert(id),
				Kind::Probe => _ = self.probe_ids.insert(id),
				Kind::Demand => {}
			}
		}
		let changed = !(self.live.iter().map(|(id, _)| id))
			.filter(|id| !self.probe_ids.contains(id))
			.eq(before);
		Ok((edit, changed))
	}
}

/// Whether the live range `range`, once answered, starts at or past group `start`, which the
/// live feed already serves: from a time at or past it, as the group containing that time is,
/// from a join placed there, or at the latest group, which no group the feed serves is past.
fn starts_past(range: &lite::Range, start: u64) -> bool {
	match range.start {
		Some(lite::Anchor::Absolute(at) | lite::Anchor::Joined { at, .. }) => at.group >= start,
		Some(lite::Anchor::Time(time)) => time >= start,
		Some(lite::Anchor::Live { min, .. }) => min == Some(Duration::ZERO),
		None => false,
	}
}

/// The probes to ask for `asks` and `joins` beside the demand's `ranges` (see [`probes`] and
/// [`join_probes`]), and the live probes to ask afresh. A time start's and a join's in turn,
/// then whichever kind is left, so a flood of either leaves the other its share of what fits.
fn wanted_probes(
	active: Option<&SubStream>,
	asks: &[u64],
	joins: &[track::JoinAsk],
	ranges: &[lite::Range],
) -> (Vec<lite::Range>, std::collections::BTreeSet<u64>) {
	let times = probes(asks, ranges);
	let (joined, reasked) = join_probes(active, joins, ranges);
	let mut wanted = Vec::with_capacity(times.len() + joined.len());
	let (mut times, mut joined) = (times.into_iter(), joined.into_iter());
	loop {
		match (times.next(), joined.next()) {
			(None, None) => break,
			(time, join) => wanted.extend(time.into_iter().chain(join)),
		}
	}
	(wanted, reasked)
}

/// The probes to ask for `asks`, each a time start the copy's readers wait on, beside the
/// demand's `ranges`: a demand range from the same time answers it already.
fn probes(asks: &[u64], ranges: &[lite::Range]) -> Vec<lite::Range> {
	if asks.is_empty() {
		return Vec::new();
	}
	let answered: std::collections::HashSet<u64> = (ranges.iter())
		.filter_map(|range| match range.start {
			Some(lite::Anchor::Time(time)) => Some(time),
			_ => None,
		})
		.collect();
	(asks.iter())
		.filter(|time| !answered.contains(time))
		.map(|&time| probe(time))
		.collect()
}

/// The probes to ask for `joins`, each a live join the copy's readers wait on, beside the
/// demand's `ranges` on `active`, and the live probes to ask afresh: one answered before a
/// reader began waiting.
///
/// A live range asking the join answers it when it is still unanswered, or was answered after
/// every waiting reader began, under a max age as wide as theirs. So does a demand range asking
/// it that this edit adds, which its answer comes after. A probe answering it stays until its
/// readers stop waiting, so its answer outlives their placing, and asks the widest max age they
/// wait with, retuned in place as that moves.
fn join_probes(
	active: Option<&SubStream>,
	joins: &[track::JoinAsk],
	ranges: &[lite::Range],
) -> (Vec<lite::Range>, std::collections::BTreeSet<u64>) {
	let mut probes = Vec::new();
	let mut reasked = std::collections::BTreeSet::new();
	for join in joins {
		let key = Some((join.min, join.end));
		// Each live range asking the join, with whether it is a probe and whether it answers:
		// answered after every waiting reader began, or still to answer, under a max age as wide
		// as theirs, now and before any retune since: the publisher may answer under the max asked
		// until then.
		let answers = |id: u64| {
			let Some(active) = active else {
				return false;
			};
			let asked = active.join_budgets.get(&id).copied().unwrap_or(Duration::MAX);
			let now = match active.live.get(id).and_then(|range| range.start) {
				Some(lite::Anchor::Live { max, .. }) => crate::track::budget(max),
				_ => Duration::ZERO,
			};
			let wide = asked.min(now) >= join.budget;
			match active.join_ticks.get(&id) {
				Some(tick) => *tick > join.since && wide,
				None => !active.starts.contains_key(&id) && wide,
			}
		};
		let live: Vec<(u64, bool)> = active
			.into_iter()
			.flat_map(|active| {
				(active.live.iter())
					.filter(|(id, range)| wire_join(range) == key && !active.fill_ids.contains(id))
					.map(|(id, _)| (id, active.probe_ids.contains(&id)))
			})
			.collect();
		// A demand range answers only while this edit keeps it: one the going-out ranges no longer
		// ask, hidden by a wider range or collapsed into another, is retired with its answer.
		let kept = ranges.iter().any(|range| wire_join(range) == key);
		let demand_answers = kept && live.iter().any(|&(id, probe)| !probe && answers(id));
		let added = kept && !live.iter().any(|&(_, probe)| !probe);
		if demand_answers || added {
			continue;
		}
		let probing: Vec<u64> = live.iter().filter(|(_, probe)| *probe).map(|(id, _)| *id).collect();
		if !probing.iter().any(|&id| answers(id)) {
			reasked.extend(probing);
		}
		probes.push(join_probe(join));
	}
	(probes, reasked)
}

/// The live probes in `reasked` whose join a fresh probe among `probes` still asks, so each is
/// retired and asked afresh; the rest stay, as no probe that fit replaces them. Matched by the
/// join each asks, not the whole range: the fresh probe asks the max its readers now wait
/// with, which may differ from the one the live probe was answered under.
fn fitted_reasks(
	active: Option<&SubStream>,
	reasked: std::collections::BTreeSet<u64>,
	probes: &[lite::Range],
) -> std::collections::BTreeSet<u64> {
	if reasked.is_empty() {
		return reasked;
	}
	let kept: std::collections::HashSet<_> = probes.iter().filter_map(wire_join).collect();
	(reasked.into_iter())
		.filter(|id| {
			(active.and_then(|active| active.live.get(*id)))
				.and_then(wire_join)
				.is_some_and(|join| kept.contains(&join))
		})
		.collect()
}

/// The range a copy asks its upstream to place the live join `join` with: the join itself,
/// under the widest max age its readers wait with, so readers waiting on it share it, as a
/// demand range asking it goes out (see [`wire_ranges`]).
fn join_probe(join: &track::JoinAsk) -> lite::Range {
	lite::Range {
		start: Some(lite::Anchor::Live {
			min: join.min,
			max: join.max(),
		}),
		end: join.end.map(lite::Anchor::Time),
	}
}

/// The live join a wire range asks, as its lag and inclusive end (see [`through`]), `None`
/// for one that isn't a live join. Where it lands doesn't depend on its max age.
fn wire_join(range: &lite::Range) -> Option<(Option<Duration>, Option<u64>)> {
	match range.start {
		Some(lite::Anchor::Live { min, .. }) => Some((min, through(range.end))),
		_ => None,
	}
}

/// The narrowest max age a live join among `live` asks, as a budget: `Duration::MAX` when
/// none asks one.
fn narrowest(live: &lite::LiveRanges) -> Duration {
	(live.iter())
		.filter_map(|(_, range)| match range.start {
			Some(lite::Anchor::Live { max, .. } | lite::Anchor::Joined { max, .. }) => {
				Some(max.unwrap_or(Duration::MAX))
			}
			_ => None,
		})
		.min()
		.unwrap_or(Duration::MAX)
}

/// The exclusive group end past every range among `live`, `None` when one follows the live
/// edge: the cap a stale drop is judged within.
fn cap_of(live: &lite::LiveRanges) -> Option<u64> {
	let mut cap = None;
	for (_, range) in live.iter() {
		range.end?;
		// A range ending before the first group reaches none.
		let reach = through(range.end).map_or(0, |last| last.saturating_add(1));
		cap = Some(cap.unwrap_or(0).max(reach));
	}
	cap
}

/// The last time a live range reaches, as its own inclusive time: an exclusive group end stops
/// at the time before it. `None` for one following the live edge.
fn through(end: Option<lite::Anchor>) -> Option<u64> {
	match end? {
		lite::Anchor::Absolute(end) if end.frame == 0 => end.group.checked_sub(1),
		lite::Anchor::Absolute(end) => Some(end.group),
		lite::Anchor::Time(end) => Some(end),
		lite::Anchor::Live { .. } | lite::Anchor::Joined { .. } => None,
	}
}

/// The newer of two answers' edges: a Largest over none, the later of two. Two naming the same
/// position say it ended if either does: one from a copy that has yet to see the end says nothing.
fn newer(a: lite::Edge, b: lite::Edge) -> lite::Edge {
	match (a, b) {
		(lite::Edge::Largest(x), lite::Edge::Largest(y)) if x.position > y.position => a,
		(lite::Edge::Largest(x), lite::Edge::Largest(y)) if x.position == y.position => {
			lite::Edge::Largest(lite::Largest {
				position: x.position,
				ended: x.ended || y.ended,
			})
		}
		(lite::Edge::Largest(_), lite::Edge::Empty | lite::Edge::Unknown) => a,
		(_, lite::Edge::Largest(_) | lite::Edge::Empty) => b,
		_ => a,
	}
}

/// The range a copy asks its upstream to place time start `time` with: the group containing
/// it alone, so it delivers nothing another range does not, and readers starting there share
/// it.
fn probe(time: u64) -> lite::Range {
	lite::Range {
		start: Some(lite::Anchor::Time(time)),
		end: Some(lite::Anchor::Time(time)),
	}
}

/// What a live range of a lite-07 subscription asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Kind {
	/// The demand's, which sets where the live feed is.
	Demand,
	/// Where the publisher starts a time start the copy's readers wait on.
	Probe,
	/// A fill's groups.
	Fill,
}

#[expect(
	clippy::large_enum_variant,
	reason = "one per track copy, which holds a live stream while anyone reads it, so boxing it would add an allocation per upstream subscription without saving memory"
)]
enum Sub {
	None,
	Active(SubStream),
}

/// Every advertisement the peer currently has live on one announce stream.
///
/// A declined advertisement remains present with no route because the peer still
/// owns its path and announce id until it retracts or restarts it.
struct Announced {
	routes: HashMap<Path, Held>,
	/// The session's announce cap, and one slot taken from it per entry in `routes`.
	slots: crate::session::Slots,
	held: Vec<crate::session::Slot>,
	/// The instance and epoch each advertisement serves, kept while it stands even when
	/// declined, so a restart that attaches it later serves the same stream, and whether
	/// the session minted it.
	instances: HashMap<Path, ((crate::Instance, u64), bool)>,
	minted: Minted,
	/// Attached routes whose request queue woke since the last serve pass. The
	/// driver wakes for every group the session carries, so a pass must cost what
	/// was requested, not every route the peer announced.
	ready: kio::Queue<Path>,
}

/// What we made of one advertisement the peer holds.
enum Held {
	/// Not in the origin for good: a reflection, or outside our origin's scope.
	Declined,
	/// Outside the session's limit: the route as advertised, attached once a
	/// limit allows it.
	Withheld(crate::origin::RouteInfo),
	/// In the origin, serving requests.
	Attached(Box<AnnouncedRoute>),
}

#[cfg(test)]
impl Default for Announced {
	fn default() -> Self {
		Self::new(Default::default(), Default::default())
	}
}

impl Drop for Announced {
	fn drop(&mut self) {
		// The stream closing ends none of its advertisements, which another may still carry.
		for (path, ((instance, _), minted)) in std::mem::take(&mut self.instances) {
			if minted {
				self.minted.release(&path, &instance, false);
			}
		}
	}
}

impl Announced {
	fn new(slots: crate::session::Slots, minted: Minted) -> Self {
		Self {
			routes: HashMap::new(),
			slots,
			held: Vec::new(),
			instances: HashMap::new(),
			minted,
			ready: Default::default(),
		}
	}

	fn contains(&self, path: &Path) -> bool {
		self.routes.contains_key(path)
	}

	/// Attach a route, queued for its first serve pass.
	fn attach(&mut self, path: Path, route: crate::origin::RouteInfo, dynamic: crate::origin::Claim) {
		let wake = crate::util::QueueWake::new(path.clone(), self.ready.clone());
		let route = AnnouncedRoute::new(route, dynamic, wake);
		route.waker.wake_by_ref();
		self.routes.insert(path, Held::Attached(Box::new(route)));
	}

	/// Put the peer's route into the origin, so paths under it resolve through this
	/// session on demand, or hold it back when the session's limit does not cover it.
	/// Returns whether it attached.
	fn offer(&mut self, subscriber: &Subscriber, path: Path, route: crate::origin::RouteInfo) -> bool {
		if !subscriber
			.auth
			.within_limit(crate::auth::Direction::Subscribe, path.as_str())
		{
			tracing::debug!(route = %subscriber.log_path(&path), "withholding announce outside the limit");
			self.routes.insert(path, Held::Withheld(route));
			return false;
		}
		// An error means the prefix is outside our origin's scope, so don't serve it.
		match subscriber.origin.forward(&path, route.clone()) {
			Ok(dynamic) => {
				self.attach(path, route, dynamic);
				true
			}
			Err(_) => {
				self.declined(path);
				false
			}
		}
	}

	fn declined(&mut self, path: Path) {
		// Dropping a replaced route closes its sources.
		self.routes.insert(path, Held::Declined);
	}

	/// Record an advertisement before deciding what to do with it.
	///
	/// The peer owns the prefix from the moment it announces, whatever the receiver makes
	/// of it, so the record is taken up front and every way out of the decision leaves it
	/// standing. Accepting replaces it via [`Self::attach`]. Doing it this way rather than
	/// at each rejection is what stops the next early return from silently freeing a path
	/// the peer still holds.
	/// Only valid on a prefix the peer does not already hold, which the caller establishes
	/// with [`Self::contains`]. Overwriting an attached route is [`Self::declined`]'s job.
	///
	/// Fails with [`Error::TooManyRequests`] once the session holds as many as it allows.
	fn reserve(&mut self, path: Path, instance: Option<(crate::Instance, u64)>) -> Result<(), Error> {
		debug_assert!(!self.routes.contains_key(&path), "reserved a prefix already advertised");
		self.held.push(self.slots.acquire()?);
		let instance = match instance {
			Some(instance) => (instance, false),
			None => ((self.minted.acquire(&path), 0), true),
		};
		self.forget(&path, false);
		self.instances.insert(path.clone(), instance);
		self.routes.insert(path, Held::Declined);
		Ok(())
	}

	/// The instance and epoch the advertisement at `path` serves.
	fn instance(&self, path: &Path) -> Option<(crate::Instance, u64)> {
		self.instances.get(path).map(|(instance, _)| instance.clone())
	}

	/// Drop the instance recorded for `path`, releasing it if the session minted it, and
	/// retiring it if the peer `ended` the advertisement.
	fn forget(&mut self, path: &Path, ended: bool) {
		if let Some(((instance, _), true)) = self.instances.remove(path) {
			self.minted.release(path, &instance, ended);
		}
	}

	fn attached(&mut self, path: &Path) -> Option<&mut AnnouncedRoute> {
		match self.routes.get_mut(path)? {
			Held::Attached(route) => Some(route.as_mut()),
			_ => None,
		}
	}

	/// Retire this session's advertisement without invalidating another live
	/// session from the same peer. Dropping its sources closes their requests.
	fn withdraw(&mut self, path: &Path) {
		self.forget(path, true);
		let Some(held) = self.routes.remove(path) else {
			return;
		};
		self.held.pop();
		if let Held::Attached(entry) = held {
			entry.dynamic.withdrawn();
		}
	}

	/// Serve queued requests on every ready route: mint a source per requested
	/// path, answer the requester with its consumer, and hand the source to the
	/// driver's serve machines, which own it from then on.
	fn poll_serve(&mut self, subscriber: &Subscriber, waiter: &kio::Waiter) {
		while let Poll::Ready(Ok(path)) = self.ready.poll_pop(waiter) {
			// A route retired since it woke has nothing left to serve.
			let Some(Held::Attached(entry)) = self.routes.get_mut(&path) else {
				continue;
			};
			// No origin node to name before lite-07.
			mint_sources(subscriber, entry, None);
		}
	}

	/// Apply a new limit: hold back every attached route it no longer covers, closing
	/// its sources (tracks in flight end on their own gates, with `Unauthorized`), and
	/// attach every withheld one it now does.
	fn limit(&mut self, permit: &crate::auth::Permit, subscriber: &Subscriber) {
		let changed: Vec<Path> = self
			.routes
			.iter()
			.filter(|(path, held)| match held {
				Held::Attached(_) => !permit.within_limit(path.as_str()),
				Held::Withheld(_) => permit.within_limit(path.as_str()),
				Held::Declined => false,
			})
			.map(|(path, _)| path.clone())
			.collect();
		for path in changed {
			match self.routes.remove(&path) {
				Some(Held::Attached(entry)) => {
					tracing::info!(route = %subscriber.log_path(&path), "announce no longer authorized");
					self.routes.insert(path, Held::Withheld(entry.route.clone()));
				}
				Some(Held::Withheld(mut route)) => {
					tracing::info!(route = %subscriber.log_path(&path), "announce authorized again");
					route.draining |= subscriber.going_away.is_set();
					self.offer(subscriber, path, route);
				}
				_ => unreachable!("only attached and withheld routes change"),
			}
		}
	}

	/// Rank every attached route last (the peer sent a GOAWAY).
	fn drain(&mut self) {
		for held in self.routes.values_mut() {
			if let Held::Attached(entry) = held {
				entry.drain();
			}
		}
	}
}

/// Serve the queued requests on a route whose queue woke: mint a source per requested
/// path, answer the requester with its consumer, and hand the source to the driver's
/// serve machines. Every request through it names `node`, which claims the broadcast
/// (lite-07; none lets the peer select).
fn mint_sources<K: Clone + Send + Sync + 'static>(
	subscriber: &Subscriber,
	entry: &mut AnnouncedRoute<K>,
	node: Option<crate::route::NodeId>,
) {
	let root = subscriber.origin.root();
	// Cleared before polling, so a request landing mid-pass queues the route again.
	entry.wake.popped();
	let cx = std::task::Context::from_waker(&entry.waker);
	let route_waiter = entry.park.hold(&cx);
	while let Poll::Ready(Some(request)) = entry.dynamic.poll_requested(route_waiter) {
		// The request path is absolute; the wire (and our origin handle)
		// speak paths relative to the session's root.
		let Some(path) = request.path().strip_prefix(root) else {
			// Outside our root: nothing we could name on the wire.
			continue;
		};
		let path = path.clone();
		// A request naming the instance reaches only that stream, so only a source on
		// a wire that cannot name one must stop once the peer no longer selects what
		// minted it.
		let validity = if subscriber.version.has_instance() {
			Default::default()
		} else {
			crate::model::broadcast::RequestValidity::for_source(&entry.dynamic, request.path())
		};
		let source = subscriber.origin.create_source(path.clone(), validity);
		// The handler exists before the requester sees the source, so a track it
		// asks for before the serve machine first runs queues instead of failing
		// `NotFound`. Accepted before the push, so the requester holds the source
		// before its serve machine can see it unheld.
		let dynamic = source.requests();
		request.serve(&source);
		let _ = subscriber.sources.try_push(MintedSource {
			path,
			instance: entry.route.instance.clone(),
			node,
			source: crate::model::broadcast::SourceGuard::new(source),
			dynamic,
			route: entry.live.consume(),
		});
	}
}

/// One received announce: the served route announced into the origin (the
/// advertisement plus its request queue), which mints a source per requested path
/// beneath it.
struct AnnouncedRoute<K = Path> {
	/// The route as last announced (post-charge), so a drain can rank it last
	/// without recomputing the chain.
	route: crate::origin::RouteInfo,
	/// Dropping it retracts the route and rejects its queued requests.
	dynamic: crate::origin::Claim,
	/// Closes as the route goes, which closes every source it minted.
	live: kio::Producer<()>,
	/// Queues this route on its prefix's ready set when its request queue wakes.
	wake: Arc<crate::util::QueueWake<K>>,
	waker: std::task::Waker,
	/// Keeps this route's registration on its request queue alive between passes.
	park: kio::Park,
}

impl<K: Clone + Send + Sync + 'static> AnnouncedRoute<K> {
	fn new(
		route: crate::origin::RouteInfo,
		dynamic: crate::origin::Claim,
		wake: Arc<crate::util::QueueWake<K>>,
	) -> Self {
		Self {
			route,
			dynamic,
			live: Default::default(),
			waker: std::task::Waker::from(wake.clone()),
			wake,
			park: kio::Park::default(),
		}
	}

	/// Update the route metadata without changing its content identity.
	fn update(&mut self, route: crate::origin::RouteInfo) {
		self.route = route.clone();
		let _ = self.dynamic.update(route);
	}

	/// Rank the route last (the peer sent a GOAWAY): every other candidate outranks
	/// it while it stays selectable as the last path, at its real cost. Idempotent,
	/// since the signal stays set.
	fn drain(&mut self) {
		if self.route.draining {
			return;
		}
		self.route.draining = true;
		let _ = self.dynamic.update(self.route.clone());
	}
}

/// How a [`TrackServe`] run ends.
enum ServeEnd {
	/// The upstream FIN'd: the track is over for good.
	Finished,
	/// The route or session failed: abort the track so the origin serves it from
	/// another source.
	GiveBack(Error),
	/// No consumers or in-flight fetches remain; the owner must commit the idle abort.
	Idle,
}

/// Serves one requested track for a relay: owns this session's copy of the
/// track (pumped into the origin's logical track), driving the single upstream
/// subscription (opened lazily on the first downstream subscriber, canceled when
/// the last one leaves) concurrently with any number of one-shot fetches.
#[derive(Clone)]
struct TrackServe {
	validity: crate::model::broadcast::RequestValidity,
	/// The advertisement that minted this source, absent for standalone test sources.
	route: Option<kio::Consumer<()>>,
	/// The source broadcast, which a lite-05/06 track adds its name's datagram track to.
	/// Absent for standalone test sources.
	source: Option<crate::broadcast::Producer>,
	subscriber: Subscriber,
	path: Path,
	/// The instance to ask for; the peer refuses a request for another.
	instance: Option<crate::Instance>,
	/// The node to ask for (lite-07), or none to let the peer select.
	node: Option<crate::route::NodeId>,
	name: String,
}

impl TrackServe {
	/// Close the source before failing a copy or a request with the session's end. The front
	/// reads whether a source is closing from its broadcast, and takes a failure from a source
	/// still open as the track's verdict: a session that failed its copies before closing their
	/// sources would end tracks its relay could resume on the next route.
	fn close_if_ending(&self) {
		if let Some(source) = &self.source
			&& self.subscriber.session.poll_ending(&kio::Waiter::noop()).is_ready()
		{
			source.close();
		}
	}

	/// Watches whether the session still lets us receive this broadcast from the peer:
	/// our grant, and the ceiling on what the peer may publish.
	fn gate(&self) -> crate::auth::Gate {
		crate::auth::Gate::new(
			self.subscriber.auth.clone(),
			self.path.clone(),
			crate::auth::Direction::Subscribe,
		)
	}

	/// Apply a subscription-demand change: hand back an [`Establish`] to open the
	/// upstream SUBSCRIBE on the first subscriber, buffer a SUBSCRIBE_UPDATE while
	/// live (the caller flushes), or cancel outright when the last one leaves.
	fn begin_subscription(
		&self,
		producer: &mut track::Producer,
		sub: &mut Sub,
		pref: Option<Subscription>,
		range_fills: &mut RangeFills,
		supports_update: bool,
	) -> Result<Begin, Error> {
		let opening = matches!(sub, Sub::None);
		// Lite-07 asks the held fills' groups through the same subscription. Demand leaving
		// while they are out keeps what it asked until they are answered, rather than retire
		// the live feed's ranges under a subscription that still runs.
		let none = FillAsk::default();
		let ranged = self.subscriber.version.has_ranges();
		let fills = match ranged {
			true => &range_fills.ask,
			false => &none,
		};
		let pref = match (pref, &*sub) {
			(None, Sub::Active(active)) if !fills.is_empty() => active.demand.clone(),
			(pref, _) => pref,
		};
		let idle = producer.idle_newest().filter(|_| opening);
		let request = self.wire_request(pref, idle);
		// The demand's ranges and the fills' groups, widened together until they fit.
		let (request, groups) = match ranged {
			true => {
				let next = match &*sub {
					Sub::Active(active) => active.live.next(),
					Sub::None => 0,
				};
				let (demand, demand_ranges, max_age) = match request {
					Some(request) => (Some(request.subscription), request.ranges, request.max_age),
					None => (None, Vec::new(), None),
				};
				let asks = demand.as_ref().map_or(&[][..], |demand| &demand.asks[..]);
				let joins = demand.as_ref().map_or(&[][..], |demand| &demand.joins[..]);
				let count = demand_ranges.len() + asks.len() + joins.len() + fills.groups.iter().count();
				let fields = self.subscribe_fields(demand.as_ref().unwrap_or(&fills.subscription()));
				let budget = fit::Budget::new(self.subscriber.version, fields, next, count);
				let (demand_ranges, groups) = fit::fit(demand_ranges, &fills.groups, &range_fills.avoid(), budget)?;
				// A fill's range starting past its run speaks only while that run is asked whole.
				range_fills.retire_skipped(&groups);
				// The probes the readers' asks need, as many as fit beside the rest: those left out
				// go once an answer lets the readers waiting on others go.
				let active = match &*sub {
					Sub::Active(active) => Some(active),
					Sub::None => None,
				};
				let (probes, reasked) = wanted_probes(active, asks, joins, &demand_ranges);
				let fill_ranges: Vec<_> = FillAsk::wire(&groups).collect();
				let probes = budget.fit_after(demand_ranges.iter().chain(&fill_ranges), probes);
				let reasked = fitted_reasks(active, reasked, &probes);
				(
					demand.map(|demand| (demand, demand_ranges, probes, reasked, max_age)),
					groups,
				)
			}
			false => (
				request.map(|request| {
					let Request {
						subscription,
						ranges,
						max_age,
					} = request;
					(subscription, ranges, Vec::new(), Default::default(), max_age)
				}),
				Default::default(),
			),
		};
		let fills = match ranged {
			true => &range_fills.ask,
			false => &none,
		};
		// What goes on the wire: the demand's ranges, then the fills', under the demand's
		// parameters. The demand alone sets the floor.
		let request = match request {
			Some((demand, demand_ranges, probes, reasked, max_age)) => {
				let mut ranges = demand_ranges.clone();
				ranges.extend(probes.iter().copied());
				ranges.extend(FillAsk::wire(&groups));
				Some(Wire {
					subscription: fills.merge(demand.clone()),
					demand: Some(demand),
					demand_ranges,
					probes: probes.len(),
					reasked,
					ranges,
					max_age,
				})
			}
			None if fills.is_empty() => None,
			None => Some(Wire {
				subscription: fills.subscription(),
				demand: None,
				demand_ranges: Vec::new(),
				probes: 0,
				reasked: Default::default(),
				ranges: FillAsk::wire(&groups).collect(),
				max_age: fills.subscription().max_age(),
			}),
		};

		match request {
			Some(wire) => {
				match sub {
					// Open an upstream SUBSCRIBE for the first subscriber. Its terms are new, so a
					// stale run an earlier subscription reported is no verdict on them.
					Sub::None => {
						producer.supersede_stale();
						Ok(Begin::Establish(self.prepare_establish(producer, wire, &groups)))
					}
					Sub::Active(active) => {
						let Wire {
							subscription,
							demand,
							demand_ranges,
							probes,
							reasked,
							ranges,
							max_age,
						} = wire;
						// Downstream preferences changed: forward them upstream as a
						// SUBSCRIBE_UPDATE (Lite03+ only; older peers can't carry one).
						let start_moved = demand.is_some() && active.start != subscription.start();
						let terms = (active.priority, active.order, active.max_delay);
						let live = active.live.unchanged();
						active.priority = subscription.priority;
						active.order = subscription.order;
						active.max_delay = max_age;
						// A lowered floor owes groups nobody asked for until now, in the session's
						// tail as well as in the track's arrivals, which its start declaration
						// restarts.
						let lowered = subscription.start().map(|start| {
							let floor = active.start.or(active.started.flatten());
							start..floor.unwrap_or(u64::MAX)
						});
						// So do changed ranges that keep the floor, such as a widening that joins
						// two: each gap they reach waits a fresh grace, which only delays giving up
						// on one still owed.
						let fills = ranges.len() - demand_ranges.len() - probes;
						let (edit, changed) = active.edit(&ranges, (probes, fills), &reasked)?;
						let widened = (changed && demand.is_some())
							.then(|| subscription.start().or(active.started.flatten()).unwrap_or(0)..u64::MAX);
						if let Ok(mut tail) = active.tail.write() {
							tail.follow(producer, track::budget(max_age));
							let now = self.subscriber.runtime.now();
							for groups in lowered.into_iter().chain(widened) {
								tail.demand(groups, now);
							}
							// So do the groups a fill newly asks for.
							for groups in groups.iter() {
								let asked = active.fill_groups.gaps(groups).collect::<Vec<_>>();
								for groups in asked {
									tail.demand(groups, now);
								}
							}
						}
						active.fill_groups = groups;
						active.demand = demand;
						active.start = subscription.start();
						if supports_update {
							// The floor follows the requested start, in both directions:
							// moving below a declared SUBSCRIBE_START reopens those groups
							// (the peer may serve them now), moving forward retires the
							// skipped range (the peer stops serving it, and no fresh START
							// will say so), and dropping to the live edge clears it until
							// the next declaration. A buffered START re-applies only if the
							// start returns to the demand that produced it (see
							// `SubStream::requested`).
							// Lite-07 declares from each live range's reported start instead, pending
							// while a range the update adds that may start lower awaits SUBSCRIBE_START.
							if active.live.reports_starts() {
								active.declare(producer);
							} else if start_moved {
								let _ = producer.start_at(active.start);
							}
							// One restating what is live tells the publisher nothing, as when a join's
							// tick moves while its probe is still unanswered: joins coalesce.
							if edit != live || terms != (active.priority, active.order, active.max_delay) {
								if let Some(cap) = subscription.cap.filter(|cap| cap.frame != 0)
									&& let Some(entry) = self.subscriber.subscribes.lock().get_mut(&active.id)
								{
									entry.mark_capped(cap.group);
								}
								buffer_update(active, edit)?;
							}
						}
						Ok(Begin::None)
					}
				}
			}
			None => {
				// Last subscriber left: cancel the upstream subscription outright. An
				// idle subscription still streams every group into a cache nobody
				// reads, and the upstream counts it as a live viewer of the broadcast.
				// A returning subscriber re-establishes from the current demand.
				if let Sub::Active(mut active) = std::mem::replace(sub, Sub::None) {
					self.subscriber.remove_subscribe(active.id);
					if self.subscriber.version.keeps_subscribe_open() {
						// On lite-07, a clean FIN means "no further updates; deliver what was
						// requested", so cancelling an active subscription must reset it instead.
						active.stream.writer.abort(&Error::Cancel);
						active.stream.reader.abort(&Error::Cancel);
					} else if active.stream.writer.is_flushed() {
						// A FIN would cut short an update still waiting on credit: dropping the
						// stream resets it instead.
						let _ = active.stream.writer.finish();
					}
					tracing::debug!(track = %self.name, "subscribe canceled (idle)");
					// The copy is still held: what it cached goes stale from here, unless the
					// subscription only filled it and never fed it live.
					if active.demand.is_some() {
						producer.set_idle();
					}
				}
				Ok(Begin::None)
			}
		}
	}

	/// The bytes a SUBSCRIBE for `subscription` takes besides its ranges, at the widest id,
	/// measured with the smallest range, since it carries at least one.
	fn subscribe_fields(&self, subscription: &Subscription) -> usize {
		let msg = lite::Subscribe {
			instance: self.instance.clone(),
			node: self.node,
			id: u64::MAX,
			broadcast: self.path.clone(),
			track: self.name.as_str().into(),
			priority: subscription.priority,
			max_delay: subscription.max_age(),
			ranges: vec![lite::Range::OPEN],
			order: subscription.order,
			meta: false,
		};
		// One that can't go out leaves no room, and fails as it would have.
		msg.encode_bytes(self.subscriber.version)
			.map_or(usize::MAX, |bytes| bytes.len())
	}

	/// What a SUBSCRIBE carries for the demand `pref`: the subscription and its ranges, or
	/// `None` when it asks for nothing.
	///
	/// `idle` when this opens a subscription for a copy whose newest cached group, while
	/// idle, is its first.
	fn wire_request(&self, pref: Option<Subscription>, idle: Option<u64>) -> Option<Request> {
		// An empty half-open range asks for nothing, and the wire cannot say that: its
		// bounds are inclusive, so the nearest encoding either hands back the position
		// the caller excluded or inverts the range outright once the two bounds meet. No
		// demand at all is the faithful translation. An absent start is the live edge, wherever that lands, so the only end that is
		// certainly empty is the very first position, which is what `Position::default()`
		// stands in for.
		let subscription = pref.filter(|sub| sub.end().is_none_or(|end| end > sub.start().unwrap_or_default()))?;
		// Read before the span narrows: a span from a start is history, which drops the max of
		// every live join it covers.
		let max_age = subscription.max_age();
		let ranged = self.subscriber.version.has_ranges();
		// Lite-07 carries every range asked for; an older wire one span covering them.
		let mut subscription = match ranged {
			true => subscription,
			false => wire_span(subscription),
		};
		// An idle copy is asked from the head of the newest group it cached when
		// the subscriber has no start of its own: a quiet track (a catalog) sends
		// that group again, which says the cache is current. A lite-07 list keeps
		// any range of its own, so only one asking just for the oldest group onward
		// counts: a live join keeps its range, which the publisher places behind its own
		// live edge, so the join reaches back past what the copy holds. A copy that cached
		// nothing keeps the subscriber's ranges: no start would widen a live viewer's ask to
		// every group from the oldest.
		let newest = idle.filter(|_| {
			subscription.start().is_none()
				&& (!ranged || subscription.ranges.iter().all(|range| *range == track::Range::ALL))
		});
		if let Some(newest) = newest {
			subscription = subscription.with_start(newest);
		}
		// A wire without frame bounds asks the group whole.
		if !self.subscriber.version.has_frame_bounds() {
			subscription.resume = None;
		}
		let ranges = wire_ranges(&subscription, ranged)?;
		Some(Request {
			subscription,
			ranges,
			max_age,
		})
	}

	/// Open the SUBSCRIBE for `pref` alongside the TRACK still waiting on its TRACK_INFO,
	/// registered with its copy pending. `None` when it asks for nothing.
	fn pipeline(&self, pref: Subscription) -> Result<Option<Pipelined>, Error> {
		// A copy not accepted yet caches nothing.
		let Some(mut request) = self.wire_request(Some(pref.clone()), None) else {
			return Ok(None);
		};
		if self.subscriber.version.has_ranges() {
			let fields = self.subscribe_fields(&request.subscription);
			let budget = fit::Budget::new(self.subscriber.version, fields, 0, request.ranges.len());
			request.ranges = fit::fit(request.ranges, &Default::default(), &Default::default(), budget)?.0;
		}
		let establish = self.register(request, None);
		Ok(Some(Pipelined {
			pref,
			state: PipelinedState::Opening(establish),
		}))
	}

	/// Allocate the id, set the demand floor, and register the subscription, so the
	/// returned [`Establish`] can put the SUBSCRIBE on the wire.
	///
	/// The subscription's bounds come straight from the demand aggregate.
	fn prepare_establish(
		&self,
		producer: &mut track::Producer,
		wire: Wire,
		fills: &rangemap::RangeSet<u64>,
	) -> Establish {
		let Wire {
			subscription,
			demand,
			demand_ranges,
			probes,
			reasked: _,
			ranges,
			max_age,
		} = wire;
		// One asking only for fills is no live feed, so it leaves the floor alone.
		if demand.is_some() {
			self.request_floor(producer, subscription.start());
		}
		let framing = lite::Framing::new(producer.info(), self.subscriber.version);
		let request = Request {
			subscription,
			ranges,
			max_age,
		};
		let mut establish = self.register(
			request,
			Some(Accepted {
				producer: producer.clone(),
				framing,
			}),
		);
		establish.demand = demand;
		establish.demand_ranges = demand_ranges;
		establish.probes = probes;
		establish.fill_groups = fills.clone();
		establish
	}

	/// The floor tracks the requested start until this subscription's own
	/// SUBSCRIBE_START refines it: the peer never serves below the request, a
	/// previous subscription's declaration must not outlive its demand, and
	/// live-edge demand (None) starts with no floor at all. Lite-06+ only resolves
	/// the start with its SUBSCRIBE_START, so until then the floor is just a request.
	fn request_floor(&self, producer: &mut track::Producer, floor: Option<u64>) {
		let _ = match self.subscriber.version.resolves_start() {
			true => producer.request_start(floor),
			false => producer.start_at(floor),
		};
	}

	/// Give a pipelined subscription its accepted copy, the target its parked group streams
	/// resume into.
	fn accept_pipelined(&self, id: u64, producer: &track::Producer) {
		let framing = lite::Framing::new(producer.info(), self.subscriber.version);
		let mut subscribes = self.subscriber.subscribes.lock();
		let Some(entry) = subscribes.get_mut(&id) else {
			return;
		};
		let copy = Accepted {
			producer: producer.clone(),
			framing,
		};
		entry.accept(copy, self.subscriber.version, self.subscriber.runtime.now());
	}

	/// Allocate the id and register the subscription, so the returned [`Establish`] can
	/// put the SUBSCRIBE on the wire.
	///
	/// Registration happens here, before any of it reaches the transport: `id` is
	/// live the moment the peer reads it, and a publisher may serve its first group
	/// immediately, so a late insert races the group stream (a group whose id isn't
	/// in the map yet is dropped, stalling the track forever). The caller
	/// deregisters `id` if the establish fails.
	fn register(&self, request: Request, copy: Option<Accepted>) -> Establish {
		let Request {
			subscription,
			ranges,
			max_age,
		} = request;
		let id = self.subscriber.next_id.fetch_add(1, atomic::Ordering::Relaxed);

		tracing::debug!(id, broadcast = %self.subscriber.log_path(&self.path), track = %self.name, "subscribe started");

		// A pipelined SUBSCRIBE has no copy, so no max age, until it is adopted. Lite-07 names
		// every start, so its tail waits out no grace.
		let tail = match self.subscriber.version.has_skip() {
			true => Tail::proven(),
			false => Tail::new(tail::grace(track::budget(max_age))),
		};
		let tail = kio::Producer::new(tail);
		if let Some(copy) = &copy
			&& let Ok(mut tail) = tail.write()
		{
			tail.follow(&copy.producer, track::budget(max_age));
		}
		let mut entry = TrackEntry::new(copy, tail.clone());
		if let Some(cap) = subscription.cap.filter(|cap| cap.frame != 0) {
			entry.mark_capped(cap.group);
		}
		let version = self.subscriber.version;
		if version.has_datagrams() && !version.has_datagram_tracks() {
			entry.legacy = Some((self.path.clone(), self.name.clone()));
		}
		self.subscriber.subscribes.lock().insert(id, entry);

		let session = self.subscriber.session.clone();
		Establish {
			open: Default::default(),
			validity: self.validity.start(),
			serve: self.clone(),
			_working: session.work().start(),
			closed: session.clone(),
			session,
			id,
			demand: Some(subscription.clone()),
			demand_ranges: ranges.clone(),
			probes: 0,
			fill_groups: Default::default(),
			fills_changed: false,
			subscription,
			ranges,
			max_age,
			tail,
			parked: false,
			fin: false,
			state: EstablishState::Start,
		}
	}

	/// Test shim: drive the upstream SUBSCRIBE open like the old `establish`.
	#[cfg(test)]
	async fn establish(
		&self,
		producer: &mut track::Producer,
		sub: &mut Sub,
		subscription: Subscription,
	) -> Result<(), Error> {
		let Request {
			subscription,
			ranges,
			max_age,
		} = self
			.wire_request(Some(subscription), None)
			.expect("a non-empty subscription");
		let wire = Wire {
			max_age,
			subscription: subscription.clone(),
			demand: Some(subscription),
			demand_ranges: ranges.clone(),
			probes: 0,
			reasked: Default::default(),
			ranges,
		};
		let mut est = Box::new(self.prepare_establish(producer, wire, &Default::default()));
		let id = est.id;
		match kio::wait(move |waiter| est.poll(waiter)).await {
			Ok(active) => {
				*sub = Sub::Active(active);
				Ok(())
			}
			Err(err) => {
				self.subscriber.remove_subscribe(id);
				Err(err)
			}
		}
	}

	/// Test shim: apply one demand change like the old `handle_subscription`,
	/// driving the establish (or the update flush) to completion inline.
	#[cfg(test)]
	async fn handle_subscription(
		&self,
		producer: &mut track::Producer,
		sub: &mut Sub,
		pref: Option<Subscription>,
		supports_update: bool,
	) -> Result<(), Error> {
		match self.begin_subscription(producer, sub, pref, &mut RangeFills::default(), supports_update)? {
			Begin::Establish(est) => {
				let mut est = Box::new(est);
				let id = est.id;
				match kio::wait(move |waiter| est.poll(waiter)).await {
					Ok(active) => *sub = Sub::Active(active),
					Err(err) => {
						self.subscriber.remove_subscribe(id);
						return Err(err);
					}
				}
			}
			Begin::None => {
				if let Sub::Active(active) = sub {
					kio::wait(|waiter| active.stream.writer.poll_flush(waiter)).await?;
				}
			}
		}
		Ok(())
	}
}

/// What a demand change asks the serve loop to do next.
#[expect(
	clippy::large_enum_variant,
	reason = "a state machine's enum is its storage: one transient instance per stream, so the big variant is the working state, not padding held in bulk"
)]
enum Begin {
	/// Nothing further: the update (if any) sits in the active stream's write
	/// buffer, flushed by the loop.
	None,
	/// Open an upstream SUBSCRIBE for the first subscriber.
	Establish(Establish),
}

/// Buffer the current SUBSCRIBE parameters as the next update, with `ranges` the change to
/// the live ranges. The caller flushes. It reopens nothing: every drop the publisher sent,
/// a stale one included, stays final on the subscription.
fn buffer_update(active: &mut SubStream, ranges: lite::RangeEdit) -> Result<(), Error> {
	active.stream.writer.buffer(&lite::SubscribeUpdate {
		wanted_group: active.wanted,
		priority: active.priority,
		max_delay: active.max_delay,
		ranges,
		order: active.order,
	})
}

/// What a subscription puts on the wire: its parameters, the demand it asks for, and the
/// ranges, the demand's first, then its probes, then the held fills'.
struct Wire {
	subscription: Subscription,
	demand: Option<Subscription>,
	demand_ranges: Vec<lite::Range>,
	/// How many probes follow the demand's ranges.
	probes: usize,
	/// The live probes to ask afresh; see [`SubStream::probes`].
	reasked: std::collections::BTreeSet<u64>,
	ranges: Vec<lite::Range>,
	/// See [`Request::max_age`].
	max_age: Option<Duration>,
}

/// A demand as a SUBSCRIBE carries it.
struct Request {
	/// The demand, narrowed to one span on a wire without ranges.
	subscription: Subscription,
	ranges: Vec<lite::Range>,
	/// The `Subscriber Max Age` a wire before lite-07 carries: the demand's widest live
	/// join's max, read before its span narrows.
	max_age: Option<Duration>,
}

/// Opens the upstream SUBSCRIBE control stream: send the request, then (pre
/// lite-05) wait for the SUBSCRIBE_OK. Resolves with the live [`SubStream`];
/// the caller deregisters the id on failure.
struct Establish {
	/// Its place in line for the peer's stream credit while its stream opens.
	open: crate::transport::open::Open,
	validity: crate::model::broadcast::RequestValidity,
	serve: TrackServe,
	session: Transport,
	// A dedicated close-watch handle for the SUBSCRIBE_OK wait.
	closed: Transport,
	id: u64,
	/// The parameters the SUBSCRIBE carries.
	subscription: Subscription,
	/// The demand it asks for, `None` when it asks only for fills.
	demand: Option<Subscription>,
	/// The demand's ranges, which come first on the wire.
	demand_ranges: Vec<lite::Range>,
	/// How many probes follow them.
	probes: usize,
	/// Fill preferences changed while opening and need an update once active.
	fills_changed: bool,
	/// The groups it asks for fills.
	fill_groups: rangemap::RangeSet<u64>,
	/// The ranges the SUBSCRIBE carries: the demand's, its probes, then the fills'.
	ranges: Vec<lite::Range>,
	/// See [`Request::max_age`].
	max_age: Option<Duration>,
	tail: kio::Producer<Tail>,
	/// Its group streams stay parked once it is active; see [`SubStream::parked`].
	parked: bool,
	/// Lite-07: send clean FIN right after flushing SUBSCRIBE (one-shot, 1 RTT total).
	fin: bool,
	/// Holds the session busy while the SUBSCRIBE opens.
	_working: crate::session::Working,
	state: EstablishState,
}

#[kio::machine(self.state: EstablishState)]
impl Establish {
	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<SubStream, Error>> {
		#[every_poll]
		self.validity.check(waiter)?;

		let mut stream: Stream<Transport, Version> = wait!(Open => Stream::poll_open(
			&self.session,
			&mut self.open,
			self.serve.subscriber.version,
			waiter
		))?;
		let msg = lite::Subscribe {
			instance: self.serve.instance.clone(),
			node: self.serve.node,
			id: self.id,
			broadcast: self.serve.path.clone(),
			track: self.serve.name.as_str().into(),
			priority: self.subscription.priority,
			max_delay: self.max_age,
			ranges: self.ranges.clone(),
			order: self.subscription.order,
			meta: false,
		};
		stream = stream.with_stats(self.serve.subscriber.origin.stats());
		stream.writer.buffer(&lite::ControlType::Subscribe)?;
		stream.writer.buffer(&msg)?;
		wait!(Send => stream.writer.poll_flush(waiter))?;
		if self.fin {
			stream.writer.finish()?;
		}
		if self.serve.subscriber.version.has_track_stream() {
			return Poll::Ready(Ok(self.activate(stream, false)));
		}

		// Older drafts: the first SUBSCRIBE_OK confirms it. Bail if the session dies
		// meanwhile; a dying route hands the assignment back through the serve loop's
		// teardown instead.
		let resp = wait!(WaitOk => {
			if let Poll::Ready(err) = self.closed.poll_closed(waiter) {
				return Poll::Ready(Err(Error::from_transport(err)));
			}
			stream.reader.poll_decode::<lite::SubscribeResponse>(waiter)
		})?;
		if !matches!(resp, lite::SubscribeResponse::Ok(_)) {
			return Poll::Ready(Err(Error::ProtocolViolation));
		}
		Poll::Ready(Ok(self.activate(stream, true)))
	}

	/// Give up on the SUBSCRIBE, resetting it with `err` if it reached the wire.
	fn abort(self, err: &Error) {
		self.serve.subscriber.remove_subscribe(self.id);
		if let EstablishState::Send(EstablishStateSend { stream })
		| EstablishState::WaitOk(EstablishStateWaitOk { stream }) = self.state
		{
			stream.writer.abort(err);
		}
	}

	/// The live subscription, `answered` once the publisher's first response arrived.
	fn activate(&self, stream: Stream<Transport, Version>, answered: bool) -> SubStream {
		let demand = self.demand_ranges.len();
		let probes = demand + self.probes;
		let live = lite::LiveRanges::new(self.serve.subscriber.version, &self.ranges);
		SubStream {
			unanswered: (!answered).then(|| self.validity.clone()),
			stream,
			id: self.id,
			max_delay: self.max_age,
			stale_budget: narrowest(&live),
			stale_cap: cap_of(&live),
			start: self.subscription.start(),
			live,
			demand: self.demand.clone(),
			// The ranges take Range IDs in order: the demand's, its probes, then the fills'.
			probe_ids: (demand as u64..probes as u64).collect(),
			fill_ids: (probes as u64..self.ranges.len() as u64).collect(),
			fill_groups: self.fill_groups.clone(),
			priority: self.subscription.priority,
			order: self.subscription.order,
			requested: self.subscription.start(),
			tail: self.tail.clone(),
			started: None,
			went_live: false,
			finishing: None,
			parked: self.parked,
			starts: Default::default(),
			join_ticks: Default::default(),
			join_budgets: Default::default(),
			declared: None,
			end: None,
			wanted: None,
			_working: self.session.work().start(),
		}
	}
}

/// A SUBSCRIBE sent alongside its TRACK, before the TRACK_INFO its copy is accepted with.
struct Pipelined {
	/// The demand it asked for, which later demand changes are measured against.
	pref: Subscription,
	state: PipelinedState,
}

#[expect(
	clippy::large_enum_variant,
	reason = "a state machine's enum is its storage: one transient instance per stream, so the big variant is the working state, not padding held in bulk"
)]
enum PipelinedState {
	Opening(Establish),
	Open(SubStream),
}

impl Pipelined {
	/// Drive the SUBSCRIBE onto the wire; its responses wait for the accept. Only a failure
	/// to send it is ready.
	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Error> {
		let PipelinedState::Opening(establish) = &mut self.state else {
			return Poll::Pending;
		};
		match ready!(establish.poll(waiter)) {
			Ok(active) => {
				self.state = PipelinedState::Open(active);
				Poll::Pending
			}
			Err(err) => Poll::Ready(err),
		}
	}

	/// Give up on the subscription with its track, resetting it with `err`.
	fn abort(self, serve: &TrackServe, err: &Error) {
		match self.state {
			PipelinedState::Opening(establish) => establish.abort(err),
			PipelinedState::Open(active) => {
				serve.subscriber.remove_subscribe(active.id);
				active.stream.writer.abort(err);
			}
		}
	}
}

/// Drives one [`TrackServe`]: the TRACK_INFO fetch, then the serve loop, then
/// the teardown that decides how the origin sees this copy end.
struct TrackServeRun {
	serve: TrackServe,
	state: TrackRunState,
	/// Cancels the track once the session stops allowing its broadcast.
	gate: crate::auth::Gate,
	/// Lite-05/06: the name's datagram track, taking the datagrams its subscription carries.
	datagrams: Option<SharedDatagrams>,
	/// The copy once the publisher finished it, still serving fills below its end.
	finished: Option<FinishedServe>,
}

/// A copy whose publisher finished the track, which still takes a group below its end that a
/// reader misses or fetches, as a local [`track::Finished`] does. Its subscription is over,
/// and its tail settled only what that subscription owed: a group read partway, never
/// delivered, or delivered and since evicted continues only through upstream, so the copy
/// stays while anyone reads it or a fill is in flight.
struct FinishedServe {
	serving: Option<track::Producer>,
	demand: track::Demand,
	dynamic: track::Requests,
	fetches: kio::Tasks<FillServeRun>,
	/// Fills spanning groups, each run on a subscription of its own.
	range_fills: RangeFills,
	/// Lite-07: the time starts readers wait on, which the copy can't place, and the
	/// priority they ask with, as its demand last said.
	asks: Vec<u64>,
	priority: i8,
	/// Each of those asked on a subscription of its own from that time, as a live copy probes
	/// them on its subscription, by time.
	probes: BTreeMap<u64, Recovery>,
	/// Where each probe's publisher started, by time, which places the readers waiting on
	/// it, while they still do.
	placed: BTreeMap<u64, Option<u64>>,
	closed: Transport,
}

impl FinishedServe {
	/// Ask upstream for each time start readers wait on that no probe asks yet, and place
	/// those readers once upstream said where it starts and its group is arriving. A probe
	/// that fails before saying fails only the readers waiting on its time, with upstream's
	/// error, as a failed FETCH fails only its fill's groups. `read` is whether anyone still
	/// reads the copy.
	fn probe(&mut self, serve: &TrackServe, read: bool, waiter: &kio::Waiter) -> Result<(), Error> {
		let Some(serving) = &mut self.serving else {
			return Ok(());
		};
		while let Poll::Ready(Ok(demand)) = serving.poll_subscription_changed(waiter) {
			(self.asks, self.priority) = demand.map_or((Vec::new(), 0), |demand| (demand.asks, demand.priority));
		}
		let asks = &self.asks;
		// Sorted, as the demand counts them by time: a copy's readers can wait on thousands.
		let asked = |time: &u64| asks.binary_search(time).is_ok();
		// A time nobody waits on any more is owed nothing.
		let before = self.placed.len();
		self.placed.retain(|time, _| asked(time));
		let mut moved = self.placed.len() != before;
		// Its readers placed or gone, a probe stays only while its group is still arriving to
		// someone reading the copy, which ending it would cut.
		self.probes.retain(|time, probe| {
			let keep = asked(time) || (read && probe.reading());
			if !keep {
				probe.end(crate::StreamError::Cancel);
			}
			keep
		});
		for &time in asks {
			if self.placed.contains_key(&time) || self.probes.contains_key(&time) {
				continue;
			}
			// `u64::MAX` is reserved past the last valid group key.
			if time.checked_add(1).is_none() {
				serving.refuse_time(time, Error::BoundsExceeded(crate::coding::BoundsExceeded));
				continue;
			}
			self.probes
				.insert(time, Recovery::probe(serve, serving, time, self.priority));
		}
		let mut done = Vec::new();
		for (&time, probe) in &mut self.probes {
			let refused = match probe.poll(waiter) {
				Poll::Ready(refused) => Some(refused?),
				Poll::Pending => None,
			};
			if asked(&time) && !self.placed.contains_key(&time) {
				match (probe.landed(), &refused) {
					(Some(start), _) => {
						self.placed.insert(time, start);
						moved = true;
					}
					// Its readers stop asking, so the time is asked again only for a later one.
					(None, Some(refused)) => {
						let err = refused.get(&time).map_or(Error::NotFound, Refusal::error);
						tracing::debug!(track = %serve.name, time, %err, "time probe failed");
						serving.refuse_time(time, err);
					}
					(None, None) => {}
				}
			}
			if refused.is_some() {
				done.push(time);
			}
		}
		for time in done {
			self.probes.remove(&time);
		}
		// Replacing what the subscription last reported, whose ranges are over.
		if moved {
			serving.time_starts(self.placed.iter().map(|(&time, &start)| (time, start)));
		}
		Ok(())
	}

	/// On lite-07, whole-group fills go on one-shot bounded SUBSCRIBEs (`RangeFills`) and
	/// frame slices / flights go on `SubFillRun` (also one-shot SUBSCRIBE). On lite-05/06,
	/// fills spanning groups use `RangeFills` and single groups / slices use FETCH.
	fn take_fill(&mut self, serve: &TrackServe, fill: track::FillGroups) {
		let Some(serving) = &self.serving else {
			return;
		};
		let ranged = serve.subscriber.version.has_ranges();
		let use_range_fills = match ranged {
			true => whole_groups(&fill),
			false => spans_groups(&fill),
		};
		match use_range_fills {
			true => self.range_fills.push(fill),
			false => {
				let info = FetchInfo::Known(serving.info().clone());
				self.fetches.push(FillServeRun::new(serve.clone(), fill, info));
			}
		}
	}

	/// Ready once nobody reads the copy and no fill is in flight, or the session closed.
	fn poll(&mut self, serve: &TrackServe, waiter: &kio::Waiter) -> Poll<()> {
		loop {
			let Some(serving) = &self.serving else {
				return Poll::Ready(());
			};
			match self.dynamic.poll_requested_fill(waiter) {
				Poll::Ready(Ok(fill)) => {
					self.take_fill(serve, fill);
					continue;
				}
				// Aborted through another handle: nothing is left to serve.
				Poll::Ready(Err(_)) => {
					self.serving = None;
					return Poll::Ready(());
				}
				Poll::Pending => {}
			}
			let _ = self.fetches.poll(waiter);
			let read = crate::util::poll_unwanted(&mut self.demand, waiter).is_pending();
			if let Err(err) = self.range_fills.poll(serve, serving, None, read, waiter) {
				// A recovery's publisher broke the protocol, which closes the session.
				lite::error::control(&mut serve.subscriber.session.clone(), err);
			}
			if let Err(err) = self.probe(serve, read, waiter) {
				lite::error::control(&mut serve.subscriber.session.clone(), err);
			}
			let Some(serving) = &self.serving else {
				return Poll::Ready(());
			};
			ServeLoop::walk_fills(serve, &mut self.range_fills, &mut self.fetches, serving);
			// The groups it already holds stay readable, its clean end too once settled.
			if let Poll::Ready(err) = self.closed.poll_closed(waiter) {
				if let Some(serving) = self.serving.take() {
					serve.close_if_ending();
					let _ = serving.abort(Error::from_transport(err));
				}
				return Poll::Ready(());
			}
			if !self.fetches.is_empty() || !self.range_fills.is_empty() || !self.probes.is_empty() || read {
				return Poll::Pending;
			}
			// No linger: a new reader asks upstream afresh rather than reading this cache, which
			// no subscription keeps. A fetch made afterwards finds a cancelled copy, not a
			// dropped one.
			match self
				.serving
				.take()
				.expect("checked above")
				.abort_unwanted(Error::Cancel)
			{
				Ok(()) => {
					tracing::debug!(broadcast = %serve.subscriber.log_path(&serve.path), track = %serve.name, "finished track released");
					return Poll::Ready(());
				}
				Err(used) => self.serving = Some(used),
			}
		}
	}
}

/// Lite-05/06: the datagram track of a stream track's name, which the session adds to the
/// source once the track's TRACK_INFO arrives and routes the datagrams of the track's
/// subscription to. It adds no demand of its own, so a reader of either kind shares the
/// one upstream SUBSCRIBE. It ends as the track's serve does: finished with a finished
/// track, and otherwise with the error, or `Cancel` once nobody reads the stream track, so a
/// datagram reader left behind asks for the track again.
struct SharedDatagrams {
	subscriber: Subscriber,
	key: (Path, String),
	producer: Option<datagram::Producer>,
}

impl SharedDatagrams {
	/// The datagram track of `serve`'s name, unless its version carries none beside a
	/// stream track or the source already has one, such as a reader's request.
	fn new(serve: &TrackServe, info: &track::Info) -> Option<Self> {
		let version = serve.subscriber.version;
		if !version.has_datagrams() || version.has_datagram_tracks() {
			return None;
		}
		let info = datagram::Info::default()
			.with_timescale(info.timescale)
			.with_priority(info.priority);
		let producer = serve
			.source
			.as_ref()?
			.create_unlisted_datagrams(serve.name.as_str(), info)?;
		let key = (serve.path.clone(), serve.name.clone());
		serve.subscriber.legacy_datagrams.lock().insert(
			key.clone(),
			DatagramEntry {
				producer: producer.clone(),
				timescale: info.timescale,
			},
		);
		Some(Self {
			subscriber: serve.subscriber.clone(),
			key,
			producer: Some(producer),
		})
	}
}

impl SharedDatagrams {
	/// Stop routing to the track and end it with `end`. A clean end waits for the datagrams
	/// received with it, see [`Subscriber::end_datagrams`].
	fn end(&mut self, end: Result<(), Error>) {
		let Some(producer) = self.producer.take() else {
			return;
		};
		let Err(err) = end else {
			self.subscriber
				.datagram_ends
				.lock()
				.shared
				.push((self.key.clone(), producer));
			return;
		};
		let mut shared = self.subscriber.legacy_datagrams.lock();
		if shared
			.get(&self.key)
			.is_some_and(|entry| entry.producer.is_clone(&producer))
		{
			shared.remove(&self.key);
		}
		drop(shared);
		producer.abort(err);
	}
}

impl Drop for SharedDatagrams {
	fn drop(&mut self) {
		// Dropped with its serve when the session dies.
		let err = self.subscriber.end_reason();
		self.end(Err(err));
	}
}

#[expect(
	clippy::large_enum_variant,
	reason = "a state machine's enum is its storage: one transient instance per stream, so the big variant is the working state, not padding held in bulk"
)]
enum TrackRunState {
	/// Lite05+ learns the track's immutable properties once, up front, via a
	/// TRACK stream. The timescale then flows into every FETCH without a
	/// per-response header. A SUBSCRIBE and any fetches go out alongside, rather
	/// than a round trip later: they need the info only to read what they receive.
	Info {
		request: Option<track::Request>,
		info: TrackInfoFetch,
		/// The SUBSCRIBE sent alongside, once anyone subscribes.
		pipelined: Option<Pipelined>,
		dynamic: track::Requests,
		fetches: kio::Tasks<FillServeRun>,
		/// Lite-07: whole-group fills that arrived while a pipelined SUBSCRIBE is already
		/// opening; held until TRACK_INFO lands so they deduplicate onto that SUBSCRIBE.
		pending_fills: Vec<track::FillGroups>,
		known: crate::util::Learning<track::Info>,
		/// Holds the session busy while the request waits on its TRACK_INFO.
		_working: crate::session::Working,
	},
	Serve(ServeLoop),
	/// Lite-05/06: the publisher finished the track, whose subscription keeps its stream until
	/// the datagram ends apply, see [`Subscriber::end_datagrams`].
	Ending {
		applied: kio::Consumer<()>,
		active: SubStream,
	},
	/// Preserve the completion FIN until the publisher acknowledges it. Lite-07 also reads
	/// on to the publisher's FIN in reply, so dropping our receive half never refuses it.
	Finish {
		/// `None` once the publisher acknowledged our FIN.
		writer: Option<crate::coding::Writer<crate::transport::boxed::SendStream, Version>>,
		reader: Option<crate::coding::Reader<crate::transport::boxed::RecvStream, Version>>,
		/// The subscription keeps the session busy until then.
		_working: crate::session::Working,
	},
	Done,
}

impl TrackServeRun {
	fn new(serve: TrackServe, request: track::Request) -> Self {
		let state = if serve.subscriber.version.has_track_stream() {
			TrackRunState::Info {
				// Registered before accepting: `accept` releases the request's own fetch gate,
				// and a cache-miss fetch queued in the gap would be drained as NotFound.
				dynamic: request.requests(),
				request: Some(request),
				info: TrackInfoFetch::new(&serve),
				pipelined: None,
				fetches: kio::Tasks::new(),
				pending_fills: Vec::new(),
				known: crate::util::Learning::new(),
				_working: serve.subscriber.session.work().start(),
			}
		} else {
			// Older wires declare no publisher max age, and no timeline: their frames
			// arrive untimed.
			let info = track::Info::default().with_timescale(None).with_max_age(None);
			TrackRunState::Serve(ServeLoop::new(&serve, request, info))
		};
		let gate = serve.gate();
		Self {
			serve,
			state,
			gate,
			datagrams: None,
			finished: None,
		}
	}

	/// The session no longer allows the broadcast: end the track and cancel the
	/// upstream subscription and fetches, leaving the rest of the session alone.
	fn revoke(&mut self) {
		if let Some(datagrams) = &mut self.datagrams {
			datagrams.end(Err(Error::Unauthorized));
		}
		self.revoke_finished();
		match std::mem::replace(&mut self.state, TrackRunState::Done) {
			TrackRunState::Info {
				request,
				pipelined,
				pending_fills,
				known,
				..
			} => {
				// Each fetch sent alongside resets itself as it drops, seeing the denial.
				known.resolve(Err(Error::Unauthorized));
				for fill in pending_fills {
					fill.refuse(Error::Unauthorized);
				}
				if let Some(request) = request {
					request.reject(Error::Unauthorized);
				}
				// Reset like a live one.
				if let Some(pipelined) = pipelined {
					pipelined.abort(&self.serve, &Error::Unauthorized);
				}
			}
			TrackRunState::Serve(serve_loop) => {
				let _ = serve_loop.serving.abort(Error::Unauthorized);
				// Reset rather than finish, so the publisher reads a revocation instead of
				// a routine unsubscribe. A SUBSCRIBE still opening is on the wire too.
				if let Sub::Active(active) = serve_loop.sub {
					self.serve.subscriber.remove_subscribe(active.id);
					active.stream.writer.abort(&Error::Unauthorized);
				}
				if let ServeMode::Establish(establish) = serve_loop.mode {
					establish.abort(&Error::Unauthorized);
				}
				// Each fetch in flight resets itself as it drops, seeing the same denial.
			}
			TrackRunState::Ending { .. } | TrackRunState::Finish { .. } | TrackRunState::Done => {}
		}
	}

	/// The session no longer allows the broadcast: end the finished copy and its fetches.
	fn revoke_finished(&mut self) {
		tracing::info!(broadcast = %self.serve.subscriber.log_path(&self.serve.path), track = %self.serve.name, "subscription no longer authorized");
		// Each fetch in flight resets itself as it drops, seeing the denial.
		if let Some(serving) = self.finished.take().and_then(|finished| finished.serving) {
			let _ = serving.abort(Error::Unauthorized);
		}
	}
}

impl TrackServeRun {
	/// Finish our side of the subscription, `true` once it waits for the publisher to read it.
	fn finish_sub(&mut self, mut active: SubStream, finished: bool) -> bool {
		let version = self.serve.subscriber.version;
		// On lite-07, a clean FIN means "deliver what was requested and close when done", so
		// ending an unfinished subscription must abort it rather than FIN it.
		if !finished && version.keeps_subscribe_open() {
			active.stream.writer.abort(&Error::Cancel);
			active.stream.reader.abort(&Error::Cancel);
			return false;
		}
		// A FIN would cut short an update still waiting on credit: dropping the stream resets it
		// instead.
		if !active.stream.writer.is_flushed()
			|| active.stream.writer.finish().is_err()
			|| !finished
			|| !version.waits_for_subscriber_fin()
		{
			return false;
		}
		self.state = TrackRunState::Finish {
			writer: Some(active.stream.writer),
			reader: version.keeps_subscribe_open().then_some(active.stream.reader),
			_working: active._working,
		};
		true
	}

	/// Nobody wants the track anymore (the origin failed over, the reader left): stop
	/// waiting on a token or a peer that may never answer, which would otherwise hold
	/// this task, its TRACK stream, and the track for good. Dropping the fetch resets the
	/// stream. Ready once the request is released.
	fn poll_unwanted(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		loop {
			let TrackRunState::Info { request, .. } = &self.state else {
				return Poll::Pending;
			};
			let pending = request.as_ref().expect("request pending");
			ready!(crate::util::poll_unwanted(&mut pending.demand(), waiter));
			if pending.reject_unwanted(Error::Cancel) {
				if let TrackRunState::Info { pipelined, known, .. } =
					std::mem::replace(&mut self.state, TrackRunState::Done)
				{
					known.resolve(Err(Error::Cancel));
					if let Some(pipelined) = pipelined {
						pipelined.abort(&self.serve, &Error::Cancel);
					}
				}
				return Poll::Ready(());
			}
			// Demand returned in the gap and won inside `reject_unwanted`: poll again so the
			// unused wait is armed for when it leaves.
		}
	}
}

impl kio::Task for TrackServeRun {
	type Output = ();

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		let run = self.poll_run(waiter);
		if let Some(finished) = &mut self.finished
			&& finished.poll(&self.serve, waiter).is_ready()
		{
			self.finished = None;
		}
		match self.finished {
			Some(_) => Poll::Pending,
			None => run,
		}
	}
}

impl TrackServeRun {
	fn poll_run(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		// Checked before the auth wait too, so an abandoned request never parks on a token.
		if self.poll_unwanted(waiter).is_ready() {
			return Poll::Ready(());
		}
		ready!(self.gate.poll_ready(waiter));
		// A finished track only waits for its FIN to be acknowledged: only its finished copy,
		// still serving fetches, is left to revoke. Nothing is on a closed session, whose grants
		// end with it: the session's end decides how its tracks end.
		let ended = matches!(self.state, TrackRunState::Ending { .. } | TrackRunState::Finish { .. });
		if (!ended || self.finished.is_some())
			&& self.gate.poll_denied(waiter).is_ready()
			&& self.serve.subscriber.session.poll_ending(waiter).is_pending()
		{
			if !ended {
				self.revoke();
				return Poll::Ready(());
			}
			self.revoke_finished();
		}
		loop {
			match &mut self.state {
				TrackRunState::Info {
					request,
					info,
					pipelined,
					dynamic,
					fetches,
					pending_fills,
					known,
					..
				} => {
					// Abandonment was checked at the top of this poll, arming its wait.
					let pending = request.as_mut().expect("request pending");
					// Demand that arrives before the info goes upstream with it, the TRACK
					// first. One that needs the info to say what it asks for waits for it.
					let mut started = Ok(());
					if pipelined.is_none()
						&& let Poll::Ready(Some(pref)) = pending.poll_subscription_changed(waiter)
					{
						started = self.serve.pipeline(pref).map(|subscribing| *pipelined = subscribing);
					}
					// A fetch needs nothing from TRACK_INFO to go out, only to read its answer.
					// On lite-07, hold whole-group multi-group fills (so they go out as one bounded
					// SUBSCRIBE via RangeFills once TRACK_INFO lands) and single whole-group fills
					// when a pipelined SUBSCRIBE is already opening (so they deduplicate onto it).
					while let Poll::Ready(Ok(fill)) = dynamic.poll_requested_fill(waiter) {
						if self.serve.subscriber.version.has_ranges()
							&& whole_groups(&fill)
							&& (spans_groups(&fill) || pipelined.is_some())
						{
							pending_fills.push(fill);
						} else {
							fetches.push(FillServeRun::new(self.serve.clone(), fill, known.later()));
						}
					}
					pending_fills.retain_mut(|fill| fill.poll_withdrawn(waiter).is_pending());
					// TRACK opens first: its stream fails fast without credit, where a fetch's
					// waits for it, so fetches opened ahead could starve their own prerequisite.
					let subscribed = pipelined
						.as_ref()
						.is_some_and(|subscribing| matches!(subscribing.state, PipelinedState::Open(_)));
					let fetched = info.poll_fetch(&self.serve, subscribed, waiter);
					let failed = match pipelined {
						Some(subscribing) => subscribing.poll(waiter),
						None => Poll::Pending,
					};
					let res = match (started, failed, fetched) {
						(Err(err), _, _) | (_, Poll::Ready(err), _) => Err(err),
						(_, _, Poll::Ready(res)) => res,
						(_, _, Poll::Pending) => {
							let _ = fetches.poll(waiter);
							return Poll::Pending;
						}
					};
					let TrackRunState::Info {
						request,
						pipelined,
						dynamic,
						fetches,
						pending_fills,
						known,
						..
					} = std::mem::replace(&mut self.state, TrackRunState::Done)
					else {
						unreachable!()
					};
					let request = request.expect("request pending");
					match res {
						Ok((mut info, held)) => {
							// Lite05/06 timestamps may be send times, so the copy's groups
							// skip the rewind check.
							info.unchecked = !self.serve.subscriber.version.has_media_time();
							// Before the accept, so a relay serving the track on sees it.
							self.datagrams = SharedDatagrams::new(&self.serve, &info);
							let mut serve_loop = ServeLoop::adopt(&self.serve, request, info.clone(), dynamic, fetches);
							serve_loop.held = Some(held);
							if let Some(pipelined) = pipelined {
								serve_loop.adopt_pipelined(&self.serve, pipelined);
							}
							for fill in pending_fills {
								serve_loop.take_fill(&self.serve, fill);
							}
							// Only once accepted: a fetch answered now caches into the accepted copy.
							known.resolve(Ok(info));
							self.state = TrackRunState::Serve(serve_loop);
						}
						Err(err) => {
							let err = lite::error::control(&mut self.serve.subscriber.session.clone(), err);
							let broadcast = self.serve.subscriber.log_path(&self.serve.path);
							match &err {
								// A request racing the route's removal: a normal outcome.
								Error::Unroutable | Error::Stream(crate::StreamError::Unroutable) => {
									tracing::debug!(%broadcast, track = %self.serve.name, %err, "track info failed")
								}
								err => tracing::warn!(%broadcast, track = %self.serve.name, %err, "track info failed"),
							}
							// The fetches sent alongside fail with it, resetting their streams.
							known.resolve(Err(err.clone()));
							for fill in pending_fills {
								fill.refuse(err.clone());
							}
							drop(fetches);
							// Rejecting the request lets the origin retry (bounded) on
							// another source; waiting subscribers stall rather than error
							// meanwhile. The SUBSCRIBE sent alongside ends with it.
							if let Some(pipelined) = pipelined {
								pipelined.abort(&self.serve, &err);
							}
							self.serve.close_if_ending();
							request.reject(err);
							return Poll::Ready(());
						}
					}
				}
				TrackRunState::Serve(serve_loop) => {
					let teardown = ready!(serve_loop.poll(&self.serve, waiter));
					let finished = matches!(teardown, ServeEnd::Finished);
					let TrackRunState::Serve(mut serve_loop) = std::mem::replace(&mut self.state, TrackRunState::Done)
					else {
						unreachable!()
					};

					match teardown {
						ServeEnd::Idle => match serve_loop.serving.abort_unwanted(Error::Cancel) {
							Ok(()) => {
								tracing::debug!(broadcast = %self.serve.subscriber.log_path(&self.serve.path), track = %self.serve.name, "track released (idle)");
							}
							Err(used) => {
								serve_loop.serving = used;
								self.state = TrackRunState::Serve(serve_loop);
								continue;
							}
						},
						ServeEnd::Finished => {
							// Lite-07 declared the end already, which this leaves as it is.
							let _ = serve_loop.serving.finish_shared();
							// Before lite-05: nothing but the live subscription fills a group.
							if self.serve.subscriber.version.has_track_stream() {
								// Before lite-07 every run is on a subscription of its own already, which
								// outlives this one, so its recoveries carry on as they are.
								let per_run = !self.serve.subscriber.version.has_ranges();
								let mut range_fills = serve_loop.range_fills;
								// The time starts readers waited on already, which the live loop saw.
								let (asks, priority) = (serve_loop.serving.subscription())
									.map_or((Vec::new(), 0), |demand| (demand.asks, demand.priority));
								let mut finished = FinishedServe {
									serving: Some(serve_loop.serving),
									asks,
									priority,
									probes: Default::default(),
									placed: Default::default(),
									demand: serve_loop.demand,
									dynamic: serve_loop.dynamic,
									fetches: serve_loop.fetches,
									range_fills: match per_run {
										true => std::mem::take(&mut range_fills),
										false => RangeFills::per_run(),
									},
									closed: serve_loop.closed,
								};
								// Each fill the lite-07 subscription carried goes on its own now.
								ServeLoop::walk_fills(
									&self.serve,
									&mut range_fills,
									&mut finished.fetches,
									finished.serving.as_ref().expect("just set"),
								);
								for fill in range_fills.take_held() {
									finished.take_fill(&self.serve, fill);
								}
								self.finished = Some(finished);
							}
							if let Some(datagrams) = &mut self.datagrams {
								datagrams.end(Ok(()));
							}
						}
						ServeEnd::GiveBack(err) => {
							let err = lite::error::control(&mut self.serve.subscriber.session.clone(), err);
							if let Some(datagrams) = &mut self.datagrams {
								datagrams.end(Err(err.clone()));
							}
							self.serve.close_if_ending();
							let _ = serve_loop.serving.abort(err);
						}
					}
					// Nobody reads the stream track: a datagram reader left behind asks again.
					if let Some(datagrams) = &mut self.datagrams {
						datagrams.end(Err(Error::Cancel));
					}

					if let ServeMode::Establish(establish) = serve_loop.mode {
						establish.abort(&Error::Cancel);
					}

					if let Sub::Active(active) = serve_loop.sub {
						let applied = match finished {
							true => self.serve.subscriber.finish_subscribe(active.id),
							false => {
								self.serve.subscriber.remove_subscribe(active.id);
								None
							}
						};
						// The subscription carries its name's datagrams, which end with it.
						if let Some(applied) = applied {
							self.state = TrackRunState::Ending { applied, active };
							continue;
						}
						if self.finish_sub(active, finished) {
							continue;
						}
					}

					return Poll::Ready(());
				}
				TrackRunState::Ending { applied, .. } => {
					ready!(applied.poll_closed(waiter));
					let TrackRunState::Ending { active, .. } = std::mem::replace(&mut self.state, TrackRunState::Done)
					else {
						unreachable!()
					};
					if self.finish_sub(active, true) {
						continue;
					}
					return Poll::Ready(());
				}
				TrackRunState::Finish { writer, reader, .. } => {
					// Responses after the tail settled change nothing: read past them, yielding
					// to the session's other tasks while a peer streams them.
					let mut budget = kio::coop::Budget::new(32);
					while let Some(stream) = reader
						&& let Poll::Ready(res) = stream.poll_recv::<lite::SubscribeResponse>(waiter)
					{
						match res {
							Recv::Message(_) => {}
							// The publisher's FIN in reply, or a reset: the stream is done.
							Recv::Ended | Recv::Reset(_) => {
								*reader = None;
								break;
							}
							// Malformed bytes are a protocol violation here as on any Subscribe
							// Stream, which closes the session; a reset or a FIN inside a message
							// is not.
							Recv::Malformed(_) => {
								let _ = lite::error::control(
									&mut self.serve.subscriber.session.clone(),
									Error::ProtocolViolation,
								);
								*reader = None;
								break;
							}
						}
						ready!(budget.poll_yield(waiter));
					}
					if let Some(stream) = writer
						&& stream.poll_close(waiter).is_ready()
					{
						*writer = None;
					}
					if writer.is_some() || reader.is_some() {
						return Poll::Pending;
					}
					self.state = TrackRunState::Done;
				}
				TrackRunState::Done => return Poll::Ready(()),
			}
		}
	}
}

impl Drop for TrackServeRun {
	fn drop(&mut self) {
		// Still waiting on TRACK_INFO: the request has no copy for the driver's abort to
		// end. Reject with the session's error.
		let TrackRunState::Info {
			request,
			pipelined,
			known,
			..
		} = &mut self.state
		else {
			return;
		};
		let Some(request) = request.take() else {
			return;
		};
		let err = self.serve.subscriber.end_reason();
		known.resolve(Err(err.clone()));
		if let Some(pipelined) = pipelined.take() {
			pipelined.abort(&self.serve, &err);
		}
		self.serve.close_if_ending();
		request.reject(err);
	}
}

/// Opens a TRACK stream, reads the single TRACK_INFO, and maps it to the
/// model's [`track::Info`], handing back the stream to hold. Lite05+ only. Bails if the
/// session dies meanwhile.
struct TrackInfoFetch {
	/// Its place in line for the peer's stream credit while its stream opens.
	open: crate::transport::open::Open,
	validity: crate::model::broadcast::RequestValidity,
	session: Transport,
	// A dedicated close-watch handle for the read.
	closed: Transport,
	state: TrackInfoState,
}

#[kio::machine(self.state: TrackInfoState)]
impl TrackInfoFetch {
	fn new(serve: &TrackServe) -> Self {
		let session = serve.subscriber.session.clone();
		Self {
			open: Default::default(),
			validity: serve.validity.start(),
			closed: session.clone(),
			session,
			state: TrackInfoState::Start,
		}
	}

	fn poll_fetch(
		&mut self,
		serve: &TrackServe,
		subscribed: bool,
		waiter: &kio::Waiter,
	) -> Poll<Result<(track::Info, HeldTrack), Error>> {
		#[every_poll]
		if subscribed && matches!(self.state, TrackInfoState::Read(..) | TrackInfoState::Properties(..)) {
			// The request is on the wire: withdrawal leaves it to its answer, just like
			// the SUBSCRIBE sent alongside it. A replacement still invalidates that answer.
			self.validity.check_unanswered(waiter)?;
		} else {
			self.validity.check(waiter)?;
			if withdrawn(serve, waiter) {
				return Poll::Ready(Err(Error::Unroutable));
			}
		}

		let mut stream: Stream<Transport, Version> =
			wait!(Open => Stream::poll_open(&self.session, &mut self.open, serve.subscriber.version, waiter))?
				.with_stats(serve.subscriber.origin.stats());
		stream.writer.buffer(&lite::ControlType::Track)?;
		stream.writer.buffer(&lite::Track {
			instance: serve.instance.clone(),
			node: serve.node,
			broadcast: serve.path.clone(),
			track: serve.name.as_str().into(),
		})?;
		wait!(Send => stream.writer.poll_flush(waiter))?;

		let (ietf, info) = wait!(Read => {
			if let Poll::Ready(err) = self.closed.poll_closed(waiter) {
				return Poll::Ready(Err(Error::from_transport(err)));
			}
			match serve.subscriber.peer_setup.poll_ietf(waiter) {
				Poll::Ready(ietf) => stream.reader.poll_decode::<lite::TrackInfo>(waiter).map_ok(|info| (ietf, info)),
				Poll::Pending => Poll::Pending,
			}
		})?;

		// Publisher Max Age rides on the wire, so the local freshness
		// bound matches what the upstream advertises (relays re-serve with
		// the same bound). `broadcast` is left at its default here;
		// `track::Request::accept` stamps the track's real broadcast.
		let mut info: track::Info = track::Info::from(info);
		if !ietf {
			return Poll::Ready(Ok((info, HeldTrack(stream))));
		}

		// Reading the TRACK_PROPERTIES the `ietf` extension sends after TRACK_INFO.
		let properties = wait!(Properties => {
			if let Poll::Ready(err) = self.closed.poll_closed(waiter) {
				return Poll::Ready(Err(Error::from_transport(err)));
			}
			stream.reader.poll_decode::<lite::ietf::TrackProperties>(waiter)
		})?;
		// Refused as moq-transport ingress refuses it.
		if let Some(err) = properties.0.as_ref().and_then(|properties| properties.refused(&info)) {
			return Poll::Ready(Err(err));
		}
		info.properties = properties.0;
		Poll::Ready(Ok((info, HeldTrack(stream))))
	}
}

/// Ready once no group stream that beat `sub`'s accept still waits to be read: they arrived
/// before whatever ends the subscription, so they are delivered first.
fn poll_unparked(sub: &Sub, serve: &TrackServe, waiter: &kio::Waiter) -> Poll<()> {
	let Sub::Active(active) = sub else {
		return Poll::Ready(());
	};
	match serve.subscriber.subscribes.lock().get(&active.id) {
		Some(TrackEntry {
			parked: Some(parked), ..
		}) => parked.poll_closed(waiter),
		_ => Poll::Ready(()),
	}
}

/// Whether the advertisement that minted `serve`'s source was withdrawn.
fn withdrawn(serve: &TrackServe, waiter: &kio::Waiter) -> bool {
	serve
		.route
		.as_ref()
		.is_some_and(|route| route.poll_closed(waiter).is_ready())
}

/// An answered TRACK stream, kept open as interest in the track: the publisher holds the
/// track for us until we FIN it, which dropping this does. Held until the SUBSCRIBE that
/// follows has its first response, or until nobody wants this copy, so demand never
/// lapses between the two streams at any hop.
struct HeldTrack(Stream<Transport, Version>);

impl Drop for HeldTrack {
	fn drop(&mut self) {
		let _ = self.0.writer.finish();
	}
}

/// The serve loop proper: owns this session's copy of the track (pumped into
/// the origin's logical track), driving the single upstream subscription
/// (opened lazily on the first downstream subscriber, canceled when the last
/// one leaves) concurrently with any number of one-shot fetches.
struct ServeLoop {
	/// This session's copy, accepted with the resolved info. The origin pumps
	/// it into the logical track; demand from the logical subscribers arrives
	/// through the producer's aggregate (including the resume floor after a source
	/// change).
	serving: track::Producer,
	/// Follows `serving`'s largest, to announce a wanted group once one lands below it.
	edge: track::Consumer,
	largest: Option<u64>,
	/// Watches `serving`'s subscribers, to release the copy once nobody reads it.
	demand: track::Demand,
	/// Serve on-demand fetches of uncached groups from this session.
	dynamic: track::Requests,
	sub: Sub,
	fetches: kio::Tasks<FillServeRun>,
	/// Fetches a pre-lite-05 upstream has no FETCH for, held for the live feed.
	feed_fetches: FeedFetches,
	/// Fills a lite-07 upstream serves through the subscription.
	range_fills: RangeFills,
	// A dedicated close-watch handle for the session-died arm.
	closed: Transport,
	// SUBSCRIBE_UPDATE only exists on Lite03+, so older peers can't carry a
	// preference change to an established subscription.
	supports_update: bool,
	supports_fetch: bool,
	mode: ServeMode,
	/// Armed while nobody holds the copy: it stays, cache and all, for a returning
	/// reader until this fires.
	linger: crate::time::Deadline,
	/// The TRACK stream that answered, until a SUBSCRIBE has a response or nobody holds
	/// the copy.
	held: Option<HeldTrack>,
	/// A demand or fill change that arrived while an update was still flushing, applied once
	/// that one is on the wire.
	deferred: bool,
}

#[expect(
	clippy::large_enum_variant,
	reason = "a state machine's enum is its storage: one transient instance per stream, so the big variant is the working state, not padding held in bulk"
)]
enum ServeMode {
	/// Selecting the next event.
	Select,
	/// Driving an upstream SUBSCRIBE open. The demand arms wait meanwhile,
	/// exactly like the old inline await.
	Establish(Establish),
	/// The publisher FINned once the track ended, as before lite-07, but QUIC does not order
	/// streams: keep the subscription routable until every owed group is accounted for. The
	/// grace bounds an unaccounted key. Lite-07 keeps serving
	/// instead, settling its tail in [`SubStream::finishing`].
	Tail {
		settle: Settle,
		/// The demand's end when the tail started, which bounds the groups owed.
		requested_end: Option<u64>,
	},
	/// The upstream subscription failed with `err`. Every group stream whose header arrived
	/// before it, parked on the accept or not, is read to its end first, as a group ends on
	/// its own stream's FIN or reset: then the track is given back.
	Failed {
		err: Error,
		/// The subscription's tail, whose readers say which streams are still being read.
		tail: kio::Consumer<Tail>,
	},
}

/// Fills a pre-lite-05 upstream has no FETCH for, held while the live subscription may
/// still deliver their groups. A group the feed delivers lands in the cache, where the fill
/// answers it.
///
/// A fill waits on one group at a time, the next it still owes. Each verdict on that group
/// also settles, by run, every group of the fill the feed gave up on, so a fill a peer
/// made wide costs what the cache and ledger hold rather than its width.
///
/// There is at most one held entry per model fill, shared by its readers. Network readers
/// own request streams (bounded by their downstream transport's credit); local readers
/// own their demand. The last reader leaving wakes `poll` to release the fill, so holding
/// it adds state proportional to existing demand, not another allowance on this upstream.
struct FeedFetches {
	/// Each fill, by the group it waits on.
	held: BTreeMap<u64, HeldFetch>,
	ready: kio::Shared<BTreeSet<u64>>,
	arrivals: Option<track::Arrivals>,
	/// Only gaps containing a held group arm the earliest fold.
	fold: crate::time::Deadline,
	#[cfg(test)]
	polled: usize,
}

struct HeldFetch {
	fill: track::FillGroups,
	waiter: kio::Waiter,
	/// Not judged yet: what the feed already gave up on is settled at once.
	fresh: bool,
}

struct FetchWake {
	key: u64,
	ready: kio::Shared<BTreeSet<u64>>,
}

impl std::task::Wake for FetchWake {
	fn wake(self: Arc<Self>) {
		self.wake_by_ref();
	}
	fn wake_by_ref(self: &Arc<Self>) {
		self.ready.lock().insert(self.key);
	}
}

impl FeedFetches {
	fn new(clock: &crate::time::Deadlines) -> Self {
		Self {
			held: Default::default(),
			ready: Default::default(),
			arrivals: None,
			fold: crate::time::Deadline::new(clock),
			#[cfg(test)]
			polled: 0,
		}
	}

	/// Hold `fill` for the live feed.
	fn push(&mut self, fill: track::FillGroups, feed: &track::Consumer) {
		let Some(arrivals) = feed.arrivals() else {
			fill.drop_groups(fill.start().group..fill.end());
			return;
		};
		if let Some(previous) = &self.arrivals {
			assert!(previous.same_channel(&arrivals), "held fetches belong to one track");
		} else {
			self.arrivals = Some(arrivals);
		}
		self.hold(fill, true);
	}

	/// Wait on the next group `fill` still owes, if any.
	fn hold(&mut self, fill: track::FillGroups, fresh: bool) {
		let mut next = Some((fill, fresh));
		while let Some((fill, fresh)) = next.take() {
			let Some(key) = fill.wanted() else {
				continue;
			};
			// A replacement attempt may arrive before the poll that would see the held one's
			// readers leave this group: that one moves on to the next group it owes, if any.
			if let Some(previous) = self.held.remove(&key)
				&& previous.fill.wanted().is_some()
			{
				next = Some((previous.fill, false));
			}
			let waiter = kio::Waiter::new(std::task::Waker::from(Arc::new(FetchWake {
				key,
				ready: self.ready.clone(),
			})));
			self.held.insert(key, HeldFetch { fill, waiter, fresh });
			self.ready.lock().insert(key);
		}
	}

	fn poll(&mut self, sub: &Sub, now: crate::time::Instant, waiter: &kio::Waiter) {
		let Sub::Active(_) = sub else {
			for (_, held) in std::mem::take(&mut self.held) {
				held.fill.drop_groups(held.fill.start().group..held.fill.end());
			}
			let mut ready = self.ready.lock();
			if !ready.is_empty() {
				ready.clear();
			}
			drop(ready);
			self.arrivals = None;
			self.fold.set(None);
			return;
		};
		let Some(arrivals) = &self.arrivals else {
			return;
		};
		// One shared read arms the batch for new gaps, without copying the ledger or
		// re-polling held groups whose verdict did not change.
		let arrivals = arrivals.poll(waiter);
		if arrivals.is_closed() {
			self.ready.lock().extend(self.held.keys().copied());
		}
		for (gap, at) in arrivals.folds() {
			if at <= now {
				let mut groups = self.held.range(gap).map(|(key, _)| *key);
				if let Some(first) = groups.next() {
					let mut ready = self.ready.lock();
					ready.insert(first);
					ready.extend(groups);
				}
			}
		}
		// Drained until seen empty, which alone registers `waiter`: a key readied after a take, as a
		// fill's waker fires on another thread, is taken here or wakes it.
		let mut ready = BTreeSet::new();
		while let Poll::Ready(mut more) = self.ready.poll(waiter, |ready| match ready.is_empty() {
			true => Poll::Pending,
			false => Poll::Ready(()),
		}) {
			ready.append(&mut more);
		}
		let mut unused = Vec::new();
		let mut settled = Vec::new();
		for key in ready {
			let Some(mut held) = self.held.remove(&key) else {
				continue;
			};
			#[cfg(test)]
			{
				self.polled += 1;
			}
			if held.fill.poll_withdrawn(&held.waiter).is_ready() {
				unused.push(held.fill);
				continue;
			}
			let verdict = arrivals.poll_group(key, now, &held.waiter);
			if verdict.is_pending() && !held.fresh {
				self.held.insert(key, held);
				continue;
			}
			let gone = arrivals.given_up(held.fill.start().group..held.fill.end(), now);
			if verdict.is_pending() && gone.is_empty() {
				held.fresh = false;
				self.held.insert(key, held);
				continue;
			}
			// Never coming is a miss; a group given up keeps the error it was given up with.
			let decided = match verdict {
				Poll::Ready(res) => Some((key, res.err().unwrap_or(Error::NotFound))),
				Poll::Pending => None,
			};
			settled.push((held.fill, decided, gone));
		}
		let fold = arrivals
			.folds()
			.filter_map(|(gap, at)| self.held.range(gap).next().map(|_| at))
			.min();
		// Fills own track producers: answering or releasing one takes the track lock too.
		drop(arrivals);
		drop(unused);
		for (fill, decided, gone) in settled {
			// The group waited on is settled, so the fill moves past it: one landed without
			// the frames the fill starts from is no answer, and the cache serves what it has.
			// Answered before the bulk give-up, which knows no reason, so a drop's own error
			// reaches the fetch.
			if let Some((key, err)) = decided
				&& fill.wanted() == Some(key)
			{
				fill.group(key).reject(err);
			}
			for groups in gone.iter() {
				fill.drop_groups(groups.clone());
			}
			// Waits on the next group it owes, judged on the poll its readiness wakes.
			self.hold(fill, false);
		}
		self.fold.set(fold);
		let _ = self.fold.poll(waiter);
	}
}

impl ServeLoop {
	/// Serve a reader's or a fetch's `fill`:
	/// - On lite-07: whole-group fills ride the active subscription (or one-shot bounded
	///   SUBSCRIBE) via `range_fills`; frame slices / flights use one-shot SUBSCRIBE (`SubFillRun`).
	/// - On lite-05/06: multi-group fills use `range_fills`, single groups / slices use FETCH.
	/// - Before lite-05: wait on the live feed.
	fn take_fill(&mut self, serve: &TrackServe, fill: track::FillGroups) {
		if serve.subscriber.version.has_ranges() {
			let has_active_sub = !matches!(self.sub, Sub::None) || matches!(self.mode, ServeMode::Establish(_));
			let within_live = self
				.serving
				.largest()
				.is_some_and(|largest| fill.end() <= largest.group.saturating_add(1));
			if whole_groups(&fill) && (spans_groups(&fill) || (has_active_sub && within_live)) {
				self.range_fills.push(fill);
			} else {
				let info = FetchInfo::Known(self.serving.info().clone());
				self.fetches.push(FillServeRun::new(serve.clone(), fill, info));
			}
		} else if serve.subscriber.version.has_track_stream() && spans_groups(&fill) {
			self.range_fills.push(fill);
		} else if self.supports_fetch {
			let info = FetchInfo::Known(self.serving.info().clone());
			self.fetches.push(FillServeRun::new(serve.clone(), fill, info));
		} else {
			// No FETCH on the wire: wait for the live feed, which a subscription arriving
			// alongside may still be opening.
			self.feed_fetches.push(fill, &self.serving.consume());
		}
	}

	/// TEMPORARY MIGRATION, delete at the next version cut with `fills::Recovery::walk`: FETCH
	/// what a recovery stalled on behind a relay of the previous release, a key at a time.
	fn walk_fills(
		serve: &TrackServe,
		range_fills: &mut RangeFills,
		fetches: &mut kio::Tasks<FillServeRun>,
		serving: &track::Producer,
	) {
		for fill in range_fills.take_walks() {
			let info = FetchInfo::Known(serving.info().clone());
			fetches.push(FillServeRun::new(serve.clone(), fill, info));
		}
	}

	fn new(serve: &TrackServe, request: track::Request, info: track::Info) -> Self {
		// Register the fetch handler before accepting: `accept` releases the request's own
		// fetch gate, and a cache-miss fetch queued in the gap would be drained as NotFound.
		let dynamic = request.requests();
		Self::adopt(serve, request, info, dynamic, kio::Tasks::new())
	}

	/// Accept `request` with `info`, serving fetches through `dynamic`, registered before the
	/// accept, and carrying on with the `fetches` already in flight.
	fn adopt(
		serve: &TrackServe,
		request: track::Request,
		info: track::Info,
		dynamic: track::Requests,
		fetches: kio::Tasks<FillServeRun>,
	) -> Self {
		// Lite-06+ resolves each subscription's start from its budget, so the floor
		// the SUBSCRIBE asks for is not where delivery begins until SUBSCRIBE_START says.
		let request = match serve.subscriber.version.resolves_start() {
			true => request.request_start(None),
			false => request,
		};
		// Marked received before any reader sees it, so none resolves an anchor this wire dropped,
		// nor reads the live edge from what lands before the upstream says where it is.
		let serving = request.stage(info);
		serving.received();
		match serve.subscriber.version.has_ranges() {
			true => serving.await_edge(),
			false => serving.edge_from_groups(),
		}
		if serve.subscriber.version.resolves_anchors() {
			serving.anchored();
		}
		// Lite-07 names every start, so the copy waits for its wire rather than a grace.
		if serve.subscriber.version.has_skip() {
			serving.proven();
		}
		// A wire whose ranges carry each live join answers where each starts, which alone
		// places a reader's.
		if serve.subscriber.version.has_ranges() {
			serving.answers_joins();
		}
		// A wire without ranges starts a time anchor at the first group at or after it, so
		// its readers do too, rather than at an earlier group the copy happens to hold.
		if !serve.subscriber.version.has_ranges() {
			serving.starts_at_keys();
		}
		if serve.subscriber.version.fills_from_start() {
			serving.fills_from_start();
		}
		let serving = serving.commit();
		Self {
			edge: serving.consume(),
			largest: None,
			demand: serving.demand(),
			serving,
			dynamic,
			sub: Sub::None,
			fetches,
			feed_fetches: FeedFetches::new(&serve.subscriber.runtime),
			range_fills: RangeFills::new(serve.subscriber.version),
			closed: serve.subscriber.session.clone(),
			supports_update: !matches!(serve.subscriber.version, Version::Lite01 | Version::Lite02),
			supports_fetch: serve.subscriber.version.has_fetch(),
			mode: ServeMode::Select,
			linger: crate::time::Deadline::new(&serve.subscriber.runtime),
			held: None,
			deferred: false,
		}
	}

	/// Resume the parked group streams into the accepted copy.
	fn unpark(&mut self, serve: &TrackServe) {
		if let Sub::Active(active) = &mut self.sub
			&& std::mem::take(&mut active.parked)
		{
			serve.accept_pipelined(active.id, &self.serving);
		}
	}

	/// Take over the SUBSCRIBE sent alongside the TRACK, now that its copy is accepted. Its
	/// demand counts as asked for, so only a change since goes out as an update.
	fn adopt_pipelined(&mut self, serve: &TrackServe, pipelined: Pipelined) {
		let floor = match &pipelined.state {
			PipelinedState::Opening(establish) => establish.subscription.start(),
			PipelinedState::Open(active) => active.requested,
		};
		serve.request_floor(&mut self.serving, floor);
		// The tail's grace waited on the max age the accepted copy now carries.
		let tail = match &pipelined.state {
			PipelinedState::Opening(establish) => &establish.tail,
			PipelinedState::Open(active) => &active.tail,
		};
		if let Ok(mut tail) = tail.write() {
			tail.follow(&self.serving, pipelined.pref.widest());
		}
		self.serving.seen_subscription(Some(pipelined.pref));
		match pipelined.state {
			PipelinedState::Opening(mut establish) => {
				establish.parked = true;
				self.mode = ServeMode::Establish(establish);
			}
			PipelinedState::Open(mut active) => {
				active.parked = true;
				self.sub = Sub::Active(active);
			}
		}
	}

	/// Release a setup that no longer has demand, including the demand it acted on.
	fn abandon_setup(&mut self) {
		let ServeMode::Establish(est) = std::mem::replace(&mut self.mode, ServeMode::Select) else {
			unreachable!()
		};
		est.abort(&Error::Cancel);
		self.held = None;
		self.serving.seen_subscription(None);
	}

	/// Whether applying `pref` cancels the subscription: neither the demand nor the fills ask
	/// for anything. A cancel sends no update, so it never waits for one to flush.
	fn cancels(&self, pref: &Option<Subscription>) -> bool {
		pref.is_none() && self.range_fills.ask.is_empty()
	}

	/// Apply a demand change: open, update, or cancel the upstream subscription.
	fn begin(&mut self, serve: &TrackServe, pref: Option<Subscription>) -> Result<(), Error> {
		match serve.begin_subscription(
			&mut self.serving,
			&mut self.sub,
			pref,
			&mut self.range_fills,
			self.supports_update,
		)? {
			Begin::Establish(est) => {
				self.linger.set(None);
				// A fresh subscription may deliver what the last one would not.
				self.range_fills.forget_refused();
				self.mode = ServeMode::Establish(est);
			}
			Begin::None => {}
		}
		// A change held back for an update applies to the subscription that update was on, so
		// none outlives it: a fresh one asks for the demand as it is then.
		if matches!(self.sub, Sub::None) {
			self.deferred = false;
		}
		// A finished track's tail waits a fresh grace for what an update newly asked for,
		// rather than a deadline that may have passed while an earlier stream was read.
		if let Sub::Active(active) = &mut self.sub
			&& active.finishing.is_some()
		{
			let settle = Self::settle(serve, &self.serving, active);
			active.finishing = Some(settle);
		}
		Ok(())
	}

	/// Lite-07: record where one range starts, from a SUBSCRIBE_START, without yet publishing
	/// the full starts table to the track. Returns whether the range is still live. A range
	/// that follows the live edge is answered with where the edge is.
	fn record_start(&mut self, serve: &TrackServe, start: &lite::SubscribeStart) -> Result<bool, Error> {
		let Sub::Active(active) = &mut self.sub else {
			return Ok(false);
		};
		let id = start.id;
		let range = active.live.get(id).copied();
		if !active.live.assigned(id)
			|| active.starts.contains_key(&id)
			|| (start.largest == lite::Edge::Unknown && range.is_some_and(|range| range.follows_live()))
		{
			tracing::warn!(track = %serve.name, id, "unexpected subscribe start");
			return Err(Error::ProtocolViolation);
		}
		// One retired since is owed nothing.
		if range.is_none() {
			return Ok(false);
		}
		active.starts.insert(id, start.start);
		let now = serve.subscriber.runtime.now();
		// A fill's range starting past where it asked skips groups it will not deliver.
		if let Some(groups) = active.fill_range(id) {
			let skipped = groups.start..start.start.unwrap_or(groups.end).clamp(groups.start, groups.end);
			if let Ok(mut tail) = active.tail.write() {
				tail.account(skipped.clone(), now);
			}
			self.range_fills.skip(groups, skipped);
		}
		// The tail judges what is owed again.
		if let Ok(mut tail) = active.tail.write() {
			tail.expire(now);
		}
		Ok(true)
	}

	/// Lite-07: apply to the copy what a burst of SUBSCRIBE_STARTs said once their ranges are
	/// recorded: where the live feed starts, the newest `largest` among them, and, when
	/// `moved`, where each live range starts.
	fn apply_starts(&mut self, serve: &TrackServe, largest: lite::Edge, moved: bool) -> Result<(), Error> {
		let Sub::Active(active) = &mut self.sub else {
			return Ok(());
		};
		// Where the live feed starts so far: the lowest start the demand's ranges report.
		let served = active.feed_start().and_then(|(start, _)| start);
		active.started = Some(served);
		// Before the copy goes live, since a reader's live join resolves only then: where the
		// publisher placed it, rather than at whichever group landed first.
		if moved {
			active.report_starts(&self.serving);
		}
		let (position, ended) = match largest {
			lite::Edge::Largest(largest) => {
				self.serving.report_edge(largest.position.group);
				(Some(largest.position), Some(largest.ended))
			}
			lite::Edge::Empty | lite::Edge::Unknown => (None, None),
		};
		// The copy goes live at the first answer saying where the live feed is, or at the first
		// answer at all when the demand never follows it; a later one only moves the edge above
		// and the largest below. An answer behind the copy's cache fails it.
		if active.demand.is_some() && !active.went_live {
			let follows = active
				.live
				.iter()
				.any(|(id, range)| active.feeds(id) && range.follows_live());
			match (largest, active.never_serves(position)) {
				(lite::Edge::Unknown, _) if follows => Ok(()),
				(lite::Edge::Unknown, _) => self.serving.set_live_ahead(None, None, served),
				(_, Some(largest)) => self.serving.set_live_ahead(Some(largest), ended, served),
				(_, None) => self.serving.set_live(position, ended, served),
			}
			.inspect_err(|err| tracing::warn!(track = %serve.name, %err, "subscribe start behind the cache"))?;
			active.went_live = largest != lite::Edge::Unknown || !follows;
		} else if let lite::Edge::Largest(largest) = largest {
			// Recorded whole, as going live did, so this copy's own answers forward it.
			self.serving.report_largest(largest.position, largest.ended);
		}
		// An answer saying where the live feed is, or that the track holds nothing, tells this
		// copy's own readers too, once what it said is recorded above: they would read an edge
		// known before then as an empty track.
		if largest != lite::Edge::Unknown {
			self.serving.answer_edge();
		}
		// Only a live fill's or probe's Range ID matters from here.
		let live = &active.live;
		active.fill_ids.retain(|id| live.get(*id).is_some());
		active.probe_ids.retain(|id| live.get(*id).is_some());
		active.declare(&mut self.serving);
		// The groups the SUBSCRIBE asked for below where every demand range starts are not waited
		// for, whatever the demand asks later. One that still arrives is delivered, so they are
		// not dropped.
		if let Some(requested) = active.requested
			&& let Some((Some(served), false)) = active.feed_start()
			&& let Ok(mut tail) = active.tail.write()
		{
			tail.account(requested..served, serve.subscriber.runtime.now());
		}
		Ok(())
	}

	/// Apply one response read from the upstream subscription.
	fn apply_response(&mut self, serve: &TrackServe, msg: lite::SubscribeResponse) -> Result<(), Error> {
		let Sub::Active(active) = &mut self.sub else {
			return Ok(());
		};
		match &msg {
			// SUBSCRIBE_END declares the track's exclusive final
			// key, which may arrive while trailing groups are
			// still in flight. Record it on this segment's producer so
			// consumers learn the boundary early; the tail then waits for
			// the groups below it.
			lite::SubscribeResponse::End(end) => {
				if active.end.is_some() {
					tracing::warn!(track = %serve.name, group = end.group, "duplicate subscribe finish");
					return Err(Error::ProtocolViolation);
				}
				// Lower groups may still be on the wire, behind a higher
				// one, so the end holds readers at a hole until it fills or
				// its grace passes. finish_at rejects a boundary at or below a group
				// already received. lite-05 specified an inclusive end,
				// and `@moq/net` 0.1.3 to 0.1.9 sent one, so there it
				// only costs the early boundary: warn, and let the FIN
				// finish the track. Later drafts made it exclusive, so
				// the publisher contradicted its own end.
				let error = end.error.clone().map(Error::from);
				if let Err(err) = self.serving.end_at(Position::group(end.group), error) {
					match serve.subscriber.version {
						Version::Lite05 => {
							tracing::warn!(track = %serve.name, group = end.group, %err, "invalid subscribe end")
						}
						_ => {
							tracing::warn!(track = %serve.name, group = end.group, %err, "subscribe end below a received group");
							return Err(Error::ProtocolViolation);
						}
					}
				}
				active.end = Some(end.clone());
				// Lite-07 keeps the stream open past it, so its tail starts now rather
				// than at a FIN, while demand and responses keep flowing.
				if serve.subscriber.version.keeps_subscribe_open() {
					let settle = Self::settle(serve, &self.serving, active);
					active.finishing = Some(settle);
				}
			}
			// Lite-07 answers each range on its own: where its delivery starts, and what the
			// publisher knows of the largest position.
			lite::SubscribeResponse::Start(start) if serve.subscriber.version.has_ranges() => {
				let moved = self.record_start(serve, start)?;
				self.apply_starts(serve, start.largest, moved)?;
			}
			// Before lite-07, SUBSCRIBE_START names where the one range's delivery starts, the
			// first group this feed serves: the publisher skipped everything below it (e.g. it
			// could not serve the requested frame). Record it as a drop signal, so a reader
			// waiting on a skipped group fails over instead of stalling on a live route.
			lite::SubscribeResponse::Start(start) => {
				let start = start.start;
				// An update may have retired the range already, whose start says nothing now, though
				// it still says where the SUBSCRIBE's demand was served from.
				if active.live.get(0).is_some() {
					active.starts.insert(0, start);
				}
				let now = serve.subscriber.runtime.now();
				let served = match active.fill_range(0) {
					Some(groups) => {
						let skipped = groups.start..start.unwrap_or(groups.end).clamp(groups.start, groups.end);
						if let Ok(mut tail) = active.tail.write() {
							tail.account(skipped.clone(), now);
						}
						self.range_fills.skip(groups, skipped);
						None
					}
					// A fill an update retired since says nothing of the live feed either, nor
					// does a probe.
					None if !active.feeds(0) => None,
					None => start,
				};
				active.started = Some(served);
				// Only a live fill's or probe's Range ID matters from here.
				let live = &active.live;
				active.fill_ids.retain(|id| live.get(*id).is_some());
				active.probe_ids.retain(|id| live.get(*id).is_some());
				if active.demand.is_some()
					&& let Some(entry) = serve.subscriber.subscribes.lock().get_mut(&active.id)
				{
					entry.answered(served, None);
				}
				// A START describes the demand the SUBSCRIBE carried.
				// It applies only while the current start still matches
				// that demand (updates get no fresh START, so an update
				// that moved the start makes it stale, and one that
				// moved back restores it); elsewhere the
				// request-tracked floor stands rather than a guess.
				if active.start == active.requested && active.demand.is_some() {
					let _ = match (serve.subscriber.version.resolves_start(), served) {
						(true, Some(served)) => self.serving.start_resolved(served),
						_ => self.serving.start_at(served),
					};
				}
				// The groups the SUBSCRIBE asked for below it are not
				// waited for, whatever the demand asks later. One that
				// still arrives is delivered, so they are not dropped.
				if let Some(requested) = active.requested
					&& let Some(served) = served
					&& let Ok(mut tail) = active.tail.write()
				{
					tail.account(requested..served, serve.subscriber.runtime.now());
				}
			}
			// The publisher will never send these groups, so they
			// are accounted for without a stream. On lite-07, SKIP names starts never produced:
			// it ends no reader and replaces any drop of the same groups. Any other drop ends a
			// reader of one still arriving with the drop's error now. Every drop is final on the
			// subscription, a stale one included.
			lite::SubscribeResponse::Drop(dropped) => {
				let groups = dropped.groups.clone();
				if groups.is_empty() {
					return Ok(());
				}
				let error = dropped.error(serve.subscriber.version);
				if let Ok(mut tail) = active.tail.write() {
					tail.account(groups.clone(), serve.subscriber.runtime.now());
				}
				if serve.subscriber.version.has_skip() && error == crate::StreamError::Skip {
					// A fill owing them is answered: the source never had them.
					self.range_fills.refuse(groups.clone(), crate::StreamError::NotFound);
					// A group that arrived was produced after all, so the skip contradicts it: it
					// keeps the group, and the session goes on, since the draft requires no more.
					let cached = self.serving.skip_received(groups);
					if let Some(first) = cached.iter().next() {
						tracing::warn!(
							track = %serve.name,
							groups = ?dropped.groups,
							cached = first.start,
							"protocol error: SUBSCRIBE_DROP(SKIP) names a group that arrived; keeping the group"
						);
					}
					return Ok(());
				}
				match serve.subscriber.version.has_skip() && error == crate::StreamError::Old {
					// Lite-07 judged it stale, recorded under the narrowest max age it may have been
					// judged under (see [`SubStream::stale_budget`]). A reader no wider settles on it; a wider one asks for the group
					// on a request of its own, since this subscription never delivers it. A key already skipped stays
					// skipped, and one whose group arrived keeps it. A group a fill asked for is
					// recovered on its own, so the copy does not record it given up meanwhile.
					true => {
						let mut given = rangemap::RangeSet::new();
						given.insert(groups.clone());
						for asked in active.fill_groups.overlapping(&groups) {
							given.remove(asked.clone());
						}
						for given in given.iter() {
							self.serving.give_up(
								given.clone(),
								track::Hole::stale(active.stale_budget).capped(active.stale_cap),
							);
						}
					}
					false => {
						self.serving.give_up(groups.clone(), track::Hole::drop(error.clone()));
					}
				}
				self.range_fills.refuse(groups, error);
			}
			// OK just resolves the range (the producer already orders
			// groups).
			lite::SubscribeResponse::Ok(_) => {
				tracing::debug!(track = %serve.name, ?msg, "subscribe response")
			}
		}
		Ok(())
	}

	/// Start waiting out `active`'s tail once the publisher ended the track. The effective
	/// max delay is the stopgap grace: the wrong clock (it bounds presentation-time drift),
	/// but it is how long the subscriber was willing to wait for a late group anyway.
	fn settle(serve: &TrackServe, serving: &track::Producer, active: &SubStream) -> Settle {
		let budget = serving.subscription().map(|sub| sub.widest());
		if let Ok(mut tail) = active.tail.write() {
			// Lite-05+ declared the end with SUBSCRIBE_END instead.
			if !serve.subscriber.version.has_track_stream() {
				tail.end();
			}
			tail.follow(serving, budget.unwrap_or_default());
			tail.expire(serve.subscriber.runtime.now());
		}
		Settle::new(&serve.subscriber.runtime, active.tail.consume())
	}

	/// Whether the tail of a lite-07 subscription the publisher finished has settled, so the
	/// copy can FIN it: every owed group accounted for, or the grace passed.
	fn poll_finishing(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		let Sub::Active(active) = &mut self.sub else {
			return Poll::Pending;
		};
		if active.finishing.is_none() {
			return Poll::Pending;
		}
		// Nothing says which groups are owed until SUBSCRIBE_START says where delivery starts,
		// so until then only the grace settles.
		let owed = match active.awaits_start() {
			true => None,
			false => active.owed(self.serving.subscription().and_then(|sub| sub.end())),
		};
		let Some(settle) = &mut active.finishing else {
			return Poll::Pending;
		};
		settle.poll(waiter, |tail| {
			owed.as_ref()
				.is_some_and(|owed| owed.iter().all(|owed| tail.covers(owed.clone())))
		})
	}

	fn poll(&mut self, serve: &TrackServe, waiter: &kio::Waiter) -> Poll<ServeEnd> {
		// Immutable TRACK_INFO stays usable locally after withdrawal. Its wire hold
		// is only a bridge to SUBSCRIBE while this source is still advertised.
		if self.held.is_some() && withdrawn(serve, waiter) {
			self.held = None;
		}
		let mut budget = kio::coop::Budget::new(32);
		loop {
			match &mut self.mode {
				ServeMode::Establish(est) => {
					let _ = self.fetches.poll(waiter);
					let read = crate::util::poll_unwanted(&mut self.demand, waiter).is_pending();
					match self
						.range_fills
						.poll(serve, &self.serving, Some(&est.tail), read, waiter)
					{
						Ok(changed) => est.fills_changed |= changed,
						Err(err) => return Poll::Ready(ServeEnd::GiveBack(err)),
					}
					Self::walk_fills(serve, &mut self.range_fills, &mut self.fetches, &self.serving);
					// TRACK requesters own a Resolving handle through metadata acceptance
					// until they attach a reader or cancel. That ownership is demand too,
					// so no observed-arrival history is needed to bridge the handoff.
					// Older wires obtain metadata on SUBSCRIBE itself.
					if serve.subscriber.version.has_track_stream()
						&& self.fetches.is_empty()
						&& self.range_fills.is_empty()
						&& !read
					{
						self.abandon_setup();
						continue;
					}
					let res = ready!(est.poll(waiter));
					let id = est.id;
					let fills_changed = est.fills_changed;
					self.mode = ServeMode::Select;
					match res {
						Ok(active) => {
							self.sub = Sub::Active(active);
							if fills_changed && let Err(err) = self.begin(serve, self.serving.subscription()) {
								return Poll::Ready(ServeEnd::GiveBack(err));
							}
						}
						Err(err) => {
							// Opening the upstream failed (usually the session dying): hand
							// the track back for another route to resume.
							serve.subscriber.remove_subscribe(id);
							return Poll::Ready(ServeEnd::GiveBack(err));
						}
					}
				}
				ServeMode::Failed { .. } => {
					// The accept counts each parked stream as read until its group settles, so
					// the wait below covers the ones it opens.
					self.unpark(serve);
					let ServeMode::Failed { tail, .. } = &self.mode else {
						unreachable!()
					};
					let drained = tail
						.poll(waiter, |tail| match tail.is_reading() {
							true => Poll::Pending,
							false => Poll::Ready(()),
						})
						.is_ready();
					let _ = self.fetches.poll(waiter);
					self.feed_fetches
						.poll(&self.sub, serve.subscriber.runtime.now(), waiter);
					if !drained {
						// The groups are drained only for a reader: once nobody holds the
						// copy, a stream the publisher never finishes must not hold it.
						if self.fetches.is_empty()
							&& self.range_fills.is_empty()
							&& crate::util::poll_unwanted(&mut self.demand, waiter).is_ready()
						{
							return Poll::Ready(ServeEnd::Idle);
						}
						return Poll::Pending;
					}
					let ServeMode::Failed { err, .. } = std::mem::replace(&mut self.mode, ServeMode::Select) else {
						unreachable!()
					};
					return Poll::Ready(ServeEnd::GiveBack(err));
				}
				ServeMode::Tail { .. } => {
					// A reader's miss is history the tail does not settle, such as a group the copy
					// evicted, so it goes upstream. Before lite-07 it goes on a subscription of its
					// own, which the finished copy takes over. Lite-07's would ride this subscription,
					// which the publisher already ended, so it waits for the finished copy.
					if !serve.subscriber.version.has_ranges() {
						match self.dynamic.poll_requested_fill(waiter) {
							Poll::Ready(Ok(fill)) => {
								self.take_fill(serve, fill);
								continue;
							}
							Poll::Ready(Err(err)) => return Poll::Ready(ServeEnd::GiveBack(err)),
							Poll::Pending => {}
						}
					}
					let ServeMode::Tail { settle, requested_end } = &mut self.mode else {
						unreachable!()
					};
					// The tail waits for groups, parked ones included.
					if let Sub::Active(active) = &mut self.sub
						&& std::mem::take(&mut active.parked)
					{
						serve.accept_pipelined(active.id, &self.serving);
					}
					let owed = match &self.sub {
						Sub::Active(active) => active.owed(*requested_end),
						Sub::None => None,
					};
					let _ = self.fetches.poll(waiter);
					self.feed_fetches
						.poll(&self.sub, serve.subscriber.runtime.now(), waiter);
					// Before lite-07 each run is recovered on a subscription of its own, which
					// outlives this one: nothing here changes with it.
					let accounted = match &self.sub {
						Sub::Active(active) => Some(active.tail.clone()),
						Sub::None => None,
					};
					let read = crate::util::poll_unwanted(&mut self.demand, waiter).is_pending();
					if let Err(err) = self
						.range_fills
						.poll(serve, &self.serving, accounted.as_ref(), read, waiter)
					{
						return Poll::Ready(ServeEnd::GiveBack(err));
					}
					Self::walk_fills(serve, &mut self.range_fills, &mut self.fetches, &self.serving);
					// Ending lite-07's subscription drops a recovery still out, so its end waits for
					// them, as a lite-07 track does. Before lite-07 the finished copy takes them over.
					if settle
						.poll(waiter, |tail| {
							owed.as_ref()
								.is_some_and(|owed| owed.iter().all(|owed| tail.covers(owed.clone())))
						})
						.is_ready() && (!serve.subscriber.version.has_ranges() || !self.range_fills.recovering())
					{
						return Poll::Ready(ServeEnd::Finished);
					}
					if self.fetches.is_empty()
						&& self.range_fills.is_empty()
						&& crate::util::poll_unwanted(&mut self.demand, waiter).is_ready()
					{
						return Poll::Ready(ServeEnd::Idle);
					}
					// The tail is still unsettled, so a session that dies now cuts the track
					// off, even one the publisher ended: the abort ends readers cleanly only
					// once the declared end settled. Before lite-05 the FIN is that end, as in
					// `SubscriptionCleanup`.
					if let Poll::Ready(err) = self.closed.poll_closed(waiter) {
						if poll_unparked(&self.sub, serve, waiter).is_pending() {
							return Poll::Pending;
						}
						if !serve.subscriber.version.has_track_stream() {
							let _ = self.serving.finish_shared();
						}
						return Poll::Ready(ServeEnd::GiveBack(Error::from_transport(err)));
					}
					return Poll::Pending;
				}
				ServeMode::Select => {
					// A SUBSCRIBE still unanswered once another instance takes the path may have
					// resolved it: refuse it. A withdrawal alone leaves it to its answer. Checked
					// before reading the stream, so an answer that only arrived alongside the
					// replacement cannot claim it, and before the flush, so an update parked on
					// flow control cannot hold the refusal back.
					if let Sub::Active(active) = &self.sub
						&& let Some(validity) = &active.unanswered
						&& let Err(err) = validity.check_unanswered(waiter)
					{
						tracing::debug!(broadcast = %serve.subscriber.log_path(&serve.path), track = %serve.name, "unanswered subscribe refused: instance replaced");
						return Poll::Ready(ServeEnd::GiveBack(err));
					}

					// Deliver any buffered SUBSCRIBE_UPDATE. One the publisher has no credit for yet
					// holds back the next demand change, so withholding credit costs one update's
					// buffer, but nothing else waits on it: a publisher blocked writing responses
					// while it reads none of ours needs them read.
					let flushing = match &mut self.sub {
						Sub::Active(active) => match active.stream.writer.poll_flush(waiter) {
							Poll::Ready(Ok(())) => false,
							// The publisher stopped reading, as it does once it finishes the
							// track. The update is dropped, and the responses decide how the
							// subscription ends: cleanly at the FIN, or with a reset's error.
							Poll::Ready(Err(err)) => {
								tracing::debug!(track = %serve.name, %err, "subscribe update dropped");
								false
							}
							Poll::Pending => true,
						},
						Sub::None => false,
					};
					if !flushing && std::mem::take(&mut self.deferred) {
						// Acted on whole, so the change it folded in is not reported again.
						let pref = self.serving.subscription();
						self.serving.seen_subscription(pref.clone());
						if let Err(err) = self.begin(serve, pref) {
							return Poll::Ready(ServeEnd::GiveBack(err));
						}
						continue;
					}

					// Biased: demand first, then completions, then closures.

					// (1) Track demand: a fetch, a subscription change, or the origin
					// handing the track to another route.

					// A fill is bounded and serves readers already waiting, so serve it ahead of
					// subscription churn.
					match self.dynamic.poll_requested_fill(waiter) {
						Poll::Ready(Ok(fill)) => {
							self.take_fill(serve, fill);
							continue;
						}
						// A live clone can abort the producer with a specific cause.
						Poll::Ready(Err(err)) => return Poll::Ready(ServeEnd::GiveBack(err)),
						Poll::Pending => {}
					}
					match self.serving.poll_subscription_changed(waiter) {
						// Polled again until it parks, so a later change such as a cancel still wakes us.
						Poll::Ready(Ok(pref)) if flushing && !self.cancels(&pref) => {
							self.deferred = true;
							continue;
						}
						Poll::Ready(Ok(pref)) => {
							// Updating the upstream failed: hand the track back for another
							// route to resume.
							if let Err(err) = self.begin(serve, pref) {
								return Poll::Ready(ServeEnd::GiveBack(err));
							}
							continue;
						}
						Poll::Ready(Err(err)) => return Poll::Ready(ServeEnd::GiveBack(err)),
						Poll::Pending => {}
					}

					// The wanted group is subscription state: an update announces each new one
					// past our largest, and every later update restates it. Joins asking for
					// the same group coalesce into that one update.
					if serve.subscriber.version.has_refresh() {
						while let Poll::Ready(Some(largest)) = self.edge.poll_largest(waiter) {
							self.largest = Some(largest.group);
						}
						if let Sub::Active(active) = &mut self.sub
							&& !flushing && let Some(largest) = self.largest
							&& let Some(wanted) = self.demand.poll_asked(waiter).filter(|wanted| *wanted > largest)
							&& active.wanted != Some(wanted)
						{
							active.wanted = Some(wanted);
							let ranges = active.live.unchanged();
							if let Err(err) = buffer_update(active, ranges) {
								return Poll::Ready(ServeEnd::GiveBack(err));
							}
							continue;
						}
					}

					// (2) In-flight fetches; completions just retire. The demand is settled,
					// so a held fetch with no feed open is a miss.
					let _ = self.fetches.poll(waiter);
					self.feed_fetches
						.poll(&self.sub, serve.subscriber.runtime.now(), waiter);
					// A fill asking for other groups changes the subscription.
					let accounted = match &self.sub {
						Sub::Active(active) => Some(active.tail.clone()),
						Sub::None => None,
					};
					// A stream still arriving on a recovery is kept only for someone reading it.
					let read = crate::util::poll_unwanted(&mut self.demand, waiter).is_pending();
					let polled = self
						.range_fills
						.poll(serve, &self.serving, accounted.as_ref(), read, waiter);
					Self::walk_fills(serve, &mut self.range_fills, &mut self.fetches, &self.serving);
					match polled {
						Ok(true) => {
							let pref = self.serving.subscription();
							if flushing && !self.cancels(&pref) {
								self.deferred = true;
							} else {
								if let Err(err) = self.begin(serve, pref) {
									return Poll::Ready(ServeEnd::GiveBack(err));
								}
								continue;
							}
						}
						Ok(false) => {}
						Err(err) => return Poll::Ready(ServeEnd::GiveBack(err)),
					}

					// A lite-07 track the publisher finished ends once its tail settles, and once
					// every recovery delivered what the tail no longer waits for: ending drops it.
					if self.poll_finishing(waiter).is_ready() && !self.range_fills.recovering() {
						return Poll::Ready(ServeEnd::Finished);
					}

					// (3) Nobody holds this copy anymore. Its upstream subscription already
					// went with the last subscriber, so it lingers, cache and all, for a
					// reader or fetch that asks again soon, then is dropped. In-flight
					// fetches keep it alive: work already accepted still gets finished.
					if self.fetches.is_empty()
						&& self.range_fills.is_empty()
						&& crate::util::poll_unwanted(&mut self.demand, waiter).is_ready()
					{
						self.held = None;
						if self.linger.deadline().is_none() {
							let now = serve.subscriber.runtime.now();
							self.linger.set(now.checked_add(track::IDLE_LINGER));
						}
						// A reader returning restarts the countdown when it next leaves.
						if self.linger.poll(waiter).is_ready() {
							return Poll::Ready(ServeEnd::Idle);
						}
					} else {
						self.linger.set(None);
					}

					// (4) The upstream subscribe stream closed, or carried a START/END/DROP.
					// Partial message bytes persist in the reader's buffer across turns.
					if let Sub::Active(active) = &mut self.sub
						&& let Poll::Ready(res) = active.stream.reader.poll_recv::<lite::SubscribeResponse>(waiter)
					{
						// The publisher answered the SUBSCRIBE, so its demand stands on the
						// subscription now. A query still held once that subscription ends
						// stays local: nothing reopens the TRACK for it.
						self.held = None;
						active.unanswered = None;
						let mut next = Some(res);
						while let Some(res) = next.take() {
							match res {
								Recv::Message(lite::SubscribeResponse::Start(first))
									if serve.subscriber.version.has_ranges() =>
								{
									let mut dirty = match self.record_start(serve, &first) {
										Ok(changed) => changed,
										Err(err) => return Poll::Ready(ServeEnd::GiveBack(err)),
									};
									let mut largest = first.largest;
									// Drain the rest of the burst before applying it: applied in pieces,
									// each piece re-fits the demand, and an update sent between them
									// retires ranges whose answers are still unread. Each SUBSCRIBE_START
									// answers a Range ID this side assigned, at most once, so the drain
									// stops at that many rather than at the task budget.
									let mut left = match &self.sub {
										Sub::Active(active) => active.live.next(),
										Sub::None => 0,
									};
									while left > 0 {
										left -= 1;
										let Sub::Active(active) = &mut self.sub else {
											break;
										};
										match active.stream.reader.poll_recv::<lite::SubscribeResponse>(waiter) {
											Poll::Ready(Recv::Message(lite::SubscribeResponse::Start(start))) => {
												match self.record_start(serve, &start) {
													Ok(changed) => dirty |= changed,
													Err(err) => return Poll::Ready(ServeEnd::GiveBack(err)),
												}
												largest = newer(largest, start.largest);
											}
											Poll::Ready(other) => {
												next = Some(other);
												break;
											}
											Poll::Pending => break,
										}
									}
									if let Err(err) = self.apply_starts(serve, largest, dirty) {
										return Poll::Ready(ServeEnd::GiveBack(err));
									}
									if next.is_none() {
										ready!(budget.poll_yield(waiter));
									}
								}
								Recv::Message(msg) => {
									if let Err(err) = self.apply_response(serve, msg) {
										return Poll::Ready(ServeEnd::GiveBack(err));
									}
									// Each pass re-polls the whole loop, so a peer streaming
									// responses must not hold the session's other tasks.
									ready!(budget.poll_yield(waiter));
								}
								Recv::Ended => {
									let Sub::Active(active) = &mut self.sub else {
										break;
									};
									// The publisher ends the subscription only once it declared the
									// track's end: with SUBSCRIBE_END from lite-05, which lite-07
									// reads into the tail without a FIN.
									if serve.subscriber.version.has_track_stream() && active.end.is_none() {
										return Poll::Ready(ServeEnd::GiveBack(Error::ProtocolViolation));
									}
									tracing::debug!(broadcast = %serve.subscriber.log_path(&serve.path), track = %serve.name, "subscribe complete");
									// Upstream FIN'd the subscription: the publisher only FINs
									// once the track's final key is known and every group
									// stream finished, so the logical track is over for good
									// (bounded demand alone never FINs; the publisher parks, since
									// a cap can be raised). Those streams can still be in flight,
									// so wait for the tail before finishing.
									self.mode = ServeMode::Tail {
										settle: Self::settle(serve, &self.serving, active),
										requested_end: self.serving.subscription().and_then(|sub| sub.end()),
									};
								}
								// Malformed bytes are a protocol violation, which closes the session at
								// once: draining the groups first would drop the close if every reader
								// left meanwhile. A reset gives up the subscription with its reason.
								Recv::Malformed(err) => {
									tracing::warn!(broadcast = %serve.subscriber.log_path(&serve.path), track = %serve.name, %err, "subscribe error");
									return Poll::Ready(ServeEnd::GiveBack(Error::ProtocolViolation));
								}
								Recv::Reset(err) => {
									let Sub::Active(active) = &mut self.sub else {
										break;
									};
									tracing::warn!(broadcast = %serve.subscriber.log_path(&serve.path), track = %serve.name, %err, "subscribe error");
									self.mode = ServeMode::Failed {
										err,
										tail: active.tail.consume(),
									};
								}
							}
						}
						continue;
					}

					// Every response that beat the accept is read.
					self.unpark(serve);

					// (5) The session died: hand the track back for another route, with
					// the session's error in case none takes it.
					if let Poll::Ready(err) = self.closed.poll_closed(waiter) {
						if poll_unparked(&self.sub, serve, waiter).is_pending() {
							return Poll::Pending;
						}
						return Poll::Ready(ServeEnd::GiveBack(Error::from_transport(err)));
					}

					return Poll::Pending;
				}
			}
		}
	}
}

/// The track's info for a fetch: known, or still in the TRACK_INFO sent alongside it, which
/// the fetch's response waits for unread in the stream.
type FetchInfo = crate::util::Later<track::Info>;

/// Why the TRACK_INFO a fetch was sent alongside failed, once it has.
fn info_failed(info: &FetchInfo) -> Option<Error> {
	info.peek()?.err()
}

/// Whether `fill` asks for more than one whole group, which a pre-lite-07 subscription
/// carries rather than FETCH.
fn spans_groups(fill: &track::FillGroups) -> bool {
	let last = fill.end().saturating_sub(1);
	!fill.is_flight() && fill.start().frame == 0 && fill.start().group < last && fill.group(last).frame_end().is_none()
}

/// Lite-07: whether `fill` asks for one or more whole groups (not a flight, frame slice, or
/// mid-group start), so it can union into an active SUBSCRIBE or ride `RangeFills`.
fn whole_groups(fill: &track::FillGroups) -> bool {
	let last = fill.end().saturating_sub(1);
	!fill.is_flight() && fill.start().frame == 0 && fill.group(last).frame_end().is_none()
}

/// Serves one group of a fill: wire `FETCH` on lite-05/06, or one-shot `SUBSCRIBE` + FIN on
/// lite-07.
#[allow(clippy::large_enum_variant, reason = "one transient instance per active group fill")]
enum GroupFillRun {
	Fetch(FetchServeRun),
	Subscribe(SubFillRun),
}

impl GroupFillRun {
	fn new(serve: TrackServe, request: track::GroupFill, info: FetchInfo) -> Self {
		if serve.subscriber.version.has_fetch() {
			Self::Fetch(FetchServeRun::new(serve, request, info))
		} else {
			Self::Subscribe(SubFillRun::new(serve, request, info))
		}
	}
}

impl kio::Task for GroupFillRun {
	type Output = ();

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		match self {
			Self::Fetch(run) => run.poll(waiter),
			Self::Subscribe(run) => run.poll(waiter),
		}
	}
}

/// Serves one fill a group at a time, newest first for a descending reader, each with its
/// own [`GroupFillRun`], until it is answered or nobody waits on it.
struct FillServeRun {
	serve: TrackServe,
	fill: track::FillGroups,
	info: FetchInfo,
	run: Option<(u64, GroupFillRun)>,
	/// The group the last run answered.
	last: Option<u64>,
}

impl Drop for FillServeRun {
	fn drop(&mut self) {
		// The fetch went out before TRACK_INFO, which failed: that is the answer for every
		// group still owed, not a miss.
		if let Some(err) = info_failed(&self.info) {
			self.fill.refuse(err);
		}
	}
}

impl FillServeRun {
	fn new(serve: TrackServe, fill: track::FillGroups, info: FetchInfo) -> Self {
		Self {
			serve,
			fill,
			info,
			run: None,
			last: None,
		}
	}
}

impl kio::Task for FillServeRun {
	type Output = ();

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		loop {
			if let Some((key, run)) = &mut self.run {
				ready!(run.poll(waiter));
				self.last = Some(*key);
				self.run = None;
			}
			// Withdrawn: nobody waits on what is left, so it records nothing.
			if self.fill.poll_withdrawn(waiter).is_ready() {
				return Poll::Ready(());
			}
			// The next fetch waits for a reader to pick up the group the last one brought, so a
			// fill wider than the cache never evicts its own groups before they are read.
			ready!(self.fill.poll_paced(waiter));
			// Answered, the fill lives until its readers take what it holds for them.
			let Some(key) = self.fill.wanted() else {
				ready!(self.fill.poll_taken(waiter));
				return Poll::Ready(());
			};
			// Every run answers its group, so the same one coming back is a bug; ending the
			// fill names it dropped rather than fetching it forever.
			if self.last == Some(key) {
				debug_assert!(false, "fill group {key} left unanswered");
				return Poll::Ready(());
			}
			let run = GroupFillRun::new(self.serve.clone(), self.fill.group(key), self.info.clone());
			self.run = Some((key, run));
		}
	}
}

/// Serves one downstream fetch end-to-end on its own bidi stream: send FETCH,
/// then fill the group from the bare FRAME messages that follow. The track's
/// info comes from its TRACK_INFO, which the response waits for unread if it
/// is still in flight, and the group key is implicit from the request.
struct FetchServeRun {
	/// Its place in line for the peer's stream credit while its stream opens.
	open: crate::transport::open::Open,
	validity: crate::model::broadcast::RequestValidity,
	gate: crate::auth::Gate,
	serve: TrackServe,
	session: Transport,
	info: FetchInfo,
	group: u64,
	state: FetchRunState,
	/// Holds the session busy until the fetch ends.
	_working: crate::session::Working,
}

enum FetchRunState {
	Open {
		request: Option<track::GroupFill>,
	},
	Send {
		request: Option<track::GroupFill>,
		stream: Stream<Transport, Version>,
		frame_start: u64,
		frame_end: Option<u64>,
	},
	/// Flushed; waiting for the publisher to answer before accepting.
	Answer {
		request: Option<track::GroupFill>,
		stream: Stream<Transport, Version>,
		frame_start: u64,
		frame_end: Option<u64>,
	},
	Ingest {
		stream: Stream<Transport, Version>,
		// Boxed, as is the ingest, so the other states stay small.
		producer: Box<crate::recv::Group>,
		// The accepted group's readers that have not yet picked it up from the cache.
		readers: track::FillReaders,
		ingest: Box<FrameIngest>,
	},
	Done,
}

impl FetchRunState {
	/// The downstream request, while it waits on the publisher's answer.
	fn request(&self) -> Option<&track::GroupFill> {
		match self {
			Self::Open { request } | Self::Send { request, .. } | Self::Answer { request, .. } => request.as_ref(),
			Self::Ingest { .. } | Self::Done => None,
		}
	}
}

impl FetchServeRun {
	fn new(serve: TrackServe, request: track::GroupFill, info: FetchInfo) -> Self {
		let session = serve.subscriber.session.clone();
		let group = request.key();
		Self {
			open: Default::default(),
			validity: serve.validity.start(),
			gate: serve.gate(),
			serve,
			_working: session.work().start(),
			session,
			info,
			group,
			state: FetchRunState::Open { request: Some(request) },
		}
	}
}

impl Drop for FetchServeRun {
	fn drop(&mut self) {
		// Dropped because the session stopped allowing the broadcast, or because the
		// TRACK_INFO sent alongside failed: reset with that reason, so neither the publisher
		// nor the waiting reader reads a routine cancel. Any other drop keeps the default
		// cancel.
		if matches!(self.state, FetchRunState::Done) {
			return;
		}
		let err = match self
			.serve
			.subscriber
			.auth
			.allows(crate::auth::Direction::Subscribe, self.serve.path.as_str())
		{
			false => Error::Unauthorized,
			true => match info_failed(&self.info) {
				Some(err) => err,
				None => return,
			},
		};
		match std::mem::replace(&mut self.state, FetchRunState::Done) {
			FetchRunState::Open { request } => {
				if let Some(request) = request {
					request.reject(err);
				}
			}
			FetchRunState::Send { request, stream, .. } | FetchRunState::Answer { request, stream, .. } => {
				stream.writer.abort(&err);
				if let Some(request) = request {
					request.reject(err);
				}
			}
			FetchRunState::Ingest {
				mut stream,
				producer,
				ingest: _ingest,
				..
			} => {
				stream.reader.abort(&err);
				stream.writer.abort(&err);
				let _ = producer.abort(err);
			}
			FetchRunState::Done => {}
		}
	}
}

/// End a FETCH we no longer want, on both halves. A dropped read half stops with the
/// transport's code 0, which the publisher reads as INTERNAL_ERROR and reports as a fault.
fn cancel(mut stream: Stream<Transport, Version>) {
	stream.reader.abort(&Error::Cancel);
	stream.writer.abort(&Error::Cancel);
}

impl kio::Task for FetchServeRun {
	type Output = ();

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		if self.state.request().is_some()
			&& let Err(err) = self.validity.check(waiter)
		{
			match std::mem::replace(&mut self.state, FetchRunState::Done) {
				FetchRunState::Open { request } => request.expect("request pending").reject(err),
				FetchRunState::Send { request, stream, .. } | FetchRunState::Answer { request, stream, .. } => {
					stream.writer.abort(&err);
					request.expect("request pending").reject(err);
				}
				_ => unreachable!(),
			}
			return Poll::Ready(());
		}
		loop {
			// A fetch nobody waits on any more is cancelled upstream, so the publisher stops
			// serving it (and a relay there releases its own FETCH). Ingest has its own check.
			if let Some(request) = self.state.request()
				&& request.poll_unwanted(waiter).is_ready()
			{
				tracing::debug!(track = %self.serve.name, group = self.group, "fetch abandoned");
				if let FetchRunState::Send { stream, .. } | FetchRunState::Answer { stream, .. } =
					std::mem::replace(&mut self.state, FetchRunState::Done)
				{
					cancel(stream);
				}
				return Poll::Ready(());
			}

			match &mut self.state {
				FetchRunState::Open { request } => {
					ready!(self.gate.poll_ready(waiter));
					// The fill already has readers holding its demand. Backpressure keeps their
					// owned request alive; abandoning it above releases this wait too.
					let mut stream = match ready!(Stream::poll_open(
						&self.session,
						&mut self.open,
						self.serve.subscriber.version,
						waiter
					)) {
						Ok(stream) => stream,
						Err(err) => {
							tracing::warn!(track = %self.serve.name, %err, "fetch stream open failed");
							request.take().expect("request pending").reject(err);
							self.state = FetchRunState::Done;
							return Poll::Ready(());
						}
					};

					// Only once the stream opens: credit and its header can wait, and
					// re-polls this state, so logging before it repeats per poll.
					tracing::debug!(broadcast = %self.serve.subscriber.log_path(&self.serve.path), track = %self.serve.name, group = self.group, "fetch started");

					let request = request.take().expect("request pending");

					// A peer that predates lite-06 addresses whole groups only, so ask for
					// the whole group and number the response from 0. The wider group still
					// covers what the caller asked for (`fetch_group` positions their own
					// consumer), and is more reusable in the cache than the tail would have
					// been. Asking for the offset anyway would fail to encode and reject a
					// fetch we can serve.
					// Until TRACK_INFO says, the group may be compressed, which only a whole
					// group decodes: ask for all of it.
					let plain = matches!(self.info.peek(), Some(Ok(info)) if !info.compressed());
					let frame_start = match self.serve.subscriber.version.has_frame_bounds() {
						true if plain => request.frame_start(),
						true => 0,
						false => 0,
					};

					let frame_end = if self.serve.subscriber.version.has_frame_bounds() && plain {
						request.frame_end()
					} else {
						None
					};

					let msg = lite::Fetch {
						instance: self.serve.instance.clone(),
						node: self.serve.node,
						broadcast: self.serve.path.clone(),
						track: self.serve.name.as_str().into(),
						priority: request.priority(),
						group: self.group,
						start_frame: frame_start,
						// A bounded plain copy keeps this provenance in the cache; older wires
						// and compressed groups widen to a complete representation.
						end_frame: frame_end.and_then(|end| end.checked_sub(1)),
					};
					stream = stream.with_stats(self.serve.subscriber.origin.stats());
					let buffered = stream
						.writer
						.buffer(&lite::ControlType::Fetch)
						.and_then(|()| stream.writer.buffer(&msg));
					if let Err(err) = buffered {
						stream.writer.abort(&err);
						request.reject(err);
						self.state = FetchRunState::Done;
						return Poll::Ready(());
					}
					self.state = FetchRunState::Send {
						request: Some(request),
						stream,
						frame_start,
						frame_end,
					};
				}
				FetchRunState::Send { stream, .. } => {
					if let Err(err) = ready!(stream.writer.poll_flush(waiter)) {
						let FetchRunState::Send { request, stream, .. } =
							std::mem::replace(&mut self.state, FetchRunState::Done)
						else {
							unreachable!()
						};
						stream.writer.abort(&err);
						request.expect("request pending").reject(err);
						return Poll::Ready(());
					}
					let FetchRunState::Send {
						request,
						mut stream,
						frame_start,
						frame_end,
					} = std::mem::replace(&mut self.state, FetchRunState::Done)
					else {
						unreachable!()
					};
					// The answer may wait unread on the TRACK_INFO sent alongside, so a full
					// window stops it before that TRACK_INFO.
					stream.reader.set_priority(crate::priority::RECV_PARKED);
					self.state = FetchRunState::Answer {
						request,
						stream,
						frame_start,
						frame_end,
					};
				}
				FetchRunState::Answer { stream, .. } => {
					// The answer is framed in the track's units: it stays unread in the stream
					// until the TRACK_INFO sent alongside lands, watched meanwhile so a reset or
					// an eviction still ends it.
					let info = match self.info.poll(waiter) {
						Poll::Ready(info) => info,
						Poll::Pending => Err(ready!(stream.reader.poll_reset(waiter))),
					};
					let info = match info {
						Ok(info) => info,
						Err(err) => {
							let FetchRunState::Answer {
								request, mut stream, ..
							} = std::mem::replace(&mut self.state, FetchRunState::Done)
							else {
								unreachable!()
							};
							stream.writer.abort(&err);
							stream.reader.abort(&err);
							request.expect("request pending").reject(err);
							return Poll::Ready(());
						}
					};
					let ietf = ready!(self.serve.subscriber.peer_setup.poll_ietf(waiter));
					// Lite has no FETCH_OK: a publisher without the group resets the
					// stream instead. Accepting before the first byte (or a FIN, for an
					// empty group) would resolve every joined `fetch_group` to a group
					// that only fails on its first read, so wait for the answer.
					let answered = ready!(stream.reader.poll_has_more(waiter));
					let FetchRunState::Answer {
						request,
						mut stream,
						frame_start,
						frame_end,
					} = std::mem::replace(&mut self.state, FetchRunState::Done)
					else {
						unreachable!()
					};
					let request = request.expect("request pending");
					// Read now, so ranked as the group it fills.
					stream.reader.set_priority(crate::priority::recv_group(
						request.priority(),
						self.group,
						Default::default(),
					));
					if let Err(err) = answered {
						tracing::debug!(track = %self.serve.name, group = self.group, %err, "fetch refused");
						stream.writer.abort(&err);
						request.reject(err);
						return Poll::Ready(());
					}

					// Make the group available (resolving the downstream fetch) and fill
					// it. The track::Info only takes effect if the track isn't accepted yet
					// (a fetch with no live subscription); otherwise the group inherits the
					// accepted info.
					let group_info = info.clone();
					// The group's readers pick it up from the cache only when next polled,
					// so their demand outlives the request until then.
					let readers = request.readers();
					let producer = match request.accept_from(
						group_info,
						group::Bounds {
							start: frame_start,
							end: frame_end,
						},
					) {
						Ok(Some(producer)) => producer,
						// Already served, as by a concurrent fetch.
						Ok(None) => {
							tracing::debug!(track = %self.serve.name, group = self.group, "fetch already served");
							cancel(stream);
							return Poll::Ready(());
						}
						Err(err) => {
							// The track closed. Nothing failed upstream: we only stopped wanting the answer.
							tracing::debug!(track = %self.serve.name, group = self.group, %err, "fetch not served");
							cancel(stream);
							return Poll::Ready(());
						}
					};

					self.state = FetchRunState::Ingest {
						stream,
						producer: Box::new(crate::recv::Group::new(producer)),
						readers,
						ingest: Box::new({
							let version = self.serve.subscriber.version;
							let framing = lite::Framing::new(&info, version);
							let clock = lite::Clock::new(version, self.group);
							FrameIngest::new(framing, ietf && framing.ietf.is_some(), frame_start, clock)
						}),
					};
				}
				FetchRunState::Ingest {
					stream,
					producer,
					readers,
					ingest,
				} => {
					let Poll::Ready(res) = ingest.poll(&mut stream.reader, producer, waiter) else {
						// Still short of its end, so completion always wins. Once nobody
						// wants the rest (no reader left to pick it up, none reading it),
						// cancel upstream and abort the truncated group, never caching it
						// as complete. The abort is atomic with a new reader arriving.
						if readers.poll_unwanted(waiter).is_pending()
							|| producer.handle().poll_unwanted(waiter).is_pending()
						{
							return Poll::Pending;
						}
						if !producer.handle().abort_unwanted(Error::Cancel) {
							continue;
						}
						tracing::debug!(track = %self.serve.name, group = self.group, "fetch abandoned");
						let FetchRunState::Ingest { stream, .. } =
							std::mem::replace(&mut self.state, FetchRunState::Done)
						else {
							unreachable!()
						};
						cancel(stream);
						return Poll::Ready(());
					};
					let FetchRunState::Ingest { producer, .. } =
						std::mem::replace(&mut self.state, FetchRunState::Done)
					else {
						unreachable!()
					};
					match res {
						// Whole at a GROUP_END's end, else closed; an end the group refuses aborts it.
						Ok(end) => {
							let _ = producer.end(end);
						}
						Err(err) => {
							let _ = producer.abort(err);
						}
					}
					return Poll::Ready(());
				}
				FetchRunState::Done => return Poll::Ready(()),
			}
		}
	}
}

/// Lite-07: serves one group fill (frame slice, head fill, mid-group start, shed-frame flight,
/// or standalone pre-TRACK_INFO fetch) over a one-shot bounded `SUBSCRIBE` + immediate `FIN`.
struct SubFillRun {
	open: crate::transport::open::Open,
	validity: crate::model::broadcast::RequestValidity,
	gate: crate::auth::Gate,
	serve: TrackServe,
	session: Transport,
	info: FetchInfo,
	group: u64,
	id: u64,
	tail: kio::Producer<Tail>,
	readers: track::FillReaders,
	state: SubFillState,
	_working: crate::session::Working,
}

enum SubFillState {
	Open {
		request: Option<track::GroupFill>,
	},
	Send {
		request: Option<track::GroupFill>,
		stream: Stream<Transport, Version>,
		frame_end: Option<u64>,
	},
	Active {
		/// Held only until TRACK_INFO lands and installs the FillTarget on TrackEntry.
		request: Option<track::GroupFill>,
		stream: Stream<Transport, Version>,
		frame_end: Option<u64>,
		started: bool,
		ended: bool,
	},
	Drain {
		/// Kept if SUBSCRIBE FIN landed before TRACK_INFO installed the FillTarget.
		request: Option<track::GroupFill>,
		frame_end: Option<u64>,
		settle: Box<Settle>,
	},
	Done,
}

impl SubFillRun {
	fn new(serve: TrackServe, request: track::GroupFill, info: FetchInfo) -> Self {
		let session = serve.subscriber.session.clone();
		let group = request.key();
		let readers = request.readers();
		let id = serve.subscriber.next_id.fetch_add(1, atomic::Ordering::Relaxed);
		let tail = kio::Producer::new(Tail::proven());
		let entry = TrackEntry::new(None, tail.clone());
		serve.subscriber.subscribes.lock().insert(id, entry);
		Self {
			open: Default::default(),
			validity: serve.validity.start(),
			gate: serve.gate(),
			serve,
			_working: session.work().start(),
			session,
			info,
			group,
			id,
			tail,
			readers,
			state: SubFillState::Open { request: Some(request) },
		}
	}

	/// Take any still-unaccepted `GroupFill` (from `state` or `TrackEntry`).
	fn take_unaccepted(&mut self) -> Option<track::GroupFill> {
		let from_state = match &mut self.state {
			SubFillState::Open { request }
			| SubFillState::Send { request, .. }
			| SubFillState::Active { request, .. }
			| SubFillState::Drain { request, .. } => request.take(),
			SubFillState::Done => None,
		};
		from_state.or_else(|| {
			self.serve
				.subscriber
				.subscribes
				.lock()
				.get_mut(&self.id)
				.and_then(TrackEntry::take_fill_request)
		})
	}

	/// Whether no group stream has accepted this fill yet.
	fn is_unaccepted(&self) -> bool {
		let held = matches!(
			&self.state,
			SubFillState::Open { request: Some(_) }
				| SubFillState::Send { request: Some(_), .. }
				| SubFillState::Active { request: Some(_), .. }
				| SubFillState::Drain { request: Some(_), .. }
		);
		held || self
			.serve
			.subscriber
			.subscribes
			.lock()
			.get(&self.id)
			.is_some_and(TrackEntry::fill_pending)
	}

	fn install_target(
		serve: &TrackServe,
		id: u64,
		request: track::GroupFill,
		info: track::Info,
		frame_end: Option<u64>,
	) {
		let version = serve.subscriber.version;
		let framing = lite::Framing::new(&info, version);
		if let Some(entry) = serve.subscriber.subscribes.lock().get_mut(&id) {
			entry.accept_fill(FillTarget {
				request: Some(request),
				info,
				framing,
				frame_end,
			});
		}
	}

	fn finish_err(&mut self, err: Error) {
		let req = self.take_unaccepted();
		self.serve.subscriber.remove_subscribe(self.id);
		match std::mem::replace(&mut self.state, SubFillState::Done) {
			SubFillState::Send { mut stream, .. } => {
				stream.writer.abort(&err);
				stream.reader.abort(&err);
			}
			SubFillState::Active { mut stream, .. } => {
				stream.reader.abort(&err);
			}
			SubFillState::Open { .. } | SubFillState::Drain { .. } | SubFillState::Done => {}
		}
		if let Some(req) = req {
			req.reject(err);
		}
	}
}

impl Drop for SubFillRun {
	fn drop(&mut self) {
		if matches!(self.state, SubFillState::Done) {
			return;
		}
		let err = match self
			.serve
			.subscriber
			.auth
			.allows(crate::auth::Direction::Subscribe, self.serve.path.as_str())
		{
			false => Some(Error::Unauthorized),
			true => info_failed(&self.info),
		};
		let req = self.take_unaccepted();
		self.serve.subscriber.remove_subscribe(self.id);
		let abort = err.as_ref().unwrap_or(&Error::Cancel);
		match std::mem::replace(&mut self.state, SubFillState::Done) {
			SubFillState::Send { mut stream, .. } => {
				stream.writer.abort(abort);
				stream.reader.abort(abort);
			}
			SubFillState::Active { mut stream, .. } => {
				stream.reader.abort(abort);
			}
			SubFillState::Open { .. } | SubFillState::Drain { .. } | SubFillState::Done => {}
		}
		if let (Some(req), Some(err)) = (req, err) {
			req.reject(err);
		}
	}
}

impl kio::Task for SubFillRun {
	type Output = ();

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		if matches!(self.state, SubFillState::Open { .. } | SubFillState::Send { .. })
			&& let Err(err) = self.validity.check(waiter)
		{
			self.finish_err(err);
			return Poll::Ready(());
		}
		loop {
			// If every caller left before any group stream accepted the fill, cancel upstream.
			if !matches!(self.state, SubFillState::Done)
				&& self.is_unaccepted()
				&& self.readers.poll_unwanted(waiter).is_ready()
			{
				tracing::debug!(track = %self.serve.name, group = self.group, "subscribe fill abandoned");
				self.serve.subscriber.remove_subscribe(self.id);
				match std::mem::replace(&mut self.state, SubFillState::Done) {
					SubFillState::Send { mut stream, .. } => {
						stream.writer.abort(&Error::Cancel);
						stream.reader.abort(&Error::Cancel);
					}
					SubFillState::Active { mut stream, .. } => {
						stream.reader.abort(&Error::Cancel);
					}
					SubFillState::Open { .. } | SubFillState::Drain { .. } | SubFillState::Done => {}
				}
				return Poll::Ready(());
			}

			match &mut self.state {
				SubFillState::Open { request } => {
					ready!(self.gate.poll_ready(waiter));
					let mut stream = match ready!(Stream::poll_open(
						&self.session,
						&mut self.open,
						self.serve.subscriber.version,
						waiter
					)) {
						Ok(stream) => stream,
						Err(err) => {
							tracing::warn!(track = %self.serve.name, %err, "subscribe fill stream open failed");
							self.finish_err(err);
							return Poll::Ready(());
						}
					};
					let request = request.take().expect("request pending");
					let known = self.info.peek().and_then(Result::ok);
					let plain = known.as_ref().is_some_and(|info| !info.compressed());
					let frame_start = if plain { request.frame_start() } else { 0 };
					let frame_end = if plain { request.frame_end() } else { None };
					let end_pos = match frame_end {
						Some(end) => Position {
							group: self.group,
							frame: end,
						},
						None => match self.group.checked_add(1) {
							Some(next) => Position::group(next),
							None => {
								let err = Error::from(crate::coding::BoundsExceeded);
								stream.writer.abort(&err);
								self.serve.subscriber.remove_subscribe(self.id);
								request.reject(err);
								self.state = SubFillState::Done;
								return Poll::Ready(());
							}
						},
					};
					let msg = lite::Subscribe {
						instance: self.serve.instance.clone(),
						node: self.serve.node,
						id: self.id,
						broadcast: self.serve.path.clone(),
						track: self.serve.name.as_str().into(),
						priority: request.priority(),
						max_delay: None,
						ranges: vec![lite::Range {
							start: Some(lite::Anchor::Absolute(Position {
								group: self.group,
								frame: frame_start,
							})),
							end: Some(lite::Anchor::Absolute(end_pos)),
						}],
						order: crate::track::Order::Ascending,
						meta: false,
					};
					tracing::debug!(
						broadcast = %self.serve.subscriber.log_path(&self.serve.path),
						track = %self.serve.name,
						group = self.group,
						frame_start,
						?frame_end,
						id = self.id,
						"one-shot subscribe fill started"
					);
					stream = stream.with_stats(self.serve.subscriber.origin.stats());
					let buffered = stream
						.writer
						.buffer(&lite::ControlType::Subscribe)
						.and_then(|()| stream.writer.buffer(&msg));
					if let Err(err) = buffered {
						stream.writer.abort(&err);
						self.serve.subscriber.remove_subscribe(self.id);
						request.reject(err);
						self.state = SubFillState::Done;
						return Poll::Ready(());
					}
					let held = match known {
						Some(info) => {
							Self::install_target(&self.serve, self.id, request, info, frame_end);
							None
						}
						None => Some(request),
					};
					self.state = SubFillState::Send {
						request: held,
						stream,
						frame_end,
					};
				}
				SubFillState::Send { stream, .. } => {
					if let Err(err) = ready!(stream.writer.poll_flush(waiter)) {
						self.finish_err(err);
						return Poll::Ready(());
					}
					if let Err(err) = stream.writer.finish() {
						self.finish_err(err);
						return Poll::Ready(());
					}
					let SubFillState::Send {
						request,
						stream,
						frame_end,
					} = std::mem::replace(&mut self.state, SubFillState::Done)
					else {
						unreachable!()
					};
					self.state = SubFillState::Active {
						request,
						stream,
						frame_end,
						started: false,
						ended: false,
					};
				}
				SubFillState::Active {
					request,
					stream,
					frame_end,
					started,
					ended,
				} => {
					if request.is_some()
						&& let Poll::Ready(res) = self.info.poll(waiter)
					{
						match res {
							Ok(info) => {
								let req = request.take().expect("checked");
								let fe = *frame_end;
								Self::install_target(&self.serve, self.id, req, info, fe);
							}
							Err(err) => {
								self.finish_err(err);
								return Poll::Ready(());
							}
						}
					}
					let res = ready!(stream.reader.poll_recv::<lite::SubscribeResponse>(waiter));
					let version = self.serve.subscriber.version;
					let key = self.group;
					let now = self.serve.subscriber.runtime.now();
					match res {
						Recv::Message(lite::SubscribeResponse::Start(start)) if !*started && start.id == 0 => {
							*started = true;
							if start.start.is_none_or(|s| s > key) {
								if let Ok(mut tail) = self.tail.write() {
									tail.account(key..key.saturating_add(1), now);
								}
								if let Some(req) = self.take_unaccepted() {
									req.reject(Error::NotFound);
								}
							}
						}
						Recv::Message(lite::SubscribeResponse::Drop(dropped)) => {
							if let Ok(mut tail) = self.tail.write() {
								tail.account(dropped.groups.clone(), now);
							}
							if dropped.groups.contains(&key)
								&& let Some(req) = self.take_unaccepted()
							{
								let err = dropped.error(version);
								if version.has_skip() && err == crate::StreamError::Skip {
									req.skip();
								} else {
									req.reject(Error::from(err));
								}
							}
						}
						Recv::Message(lite::SubscribeResponse::End(end)) if !*ended => {
							*ended = true;
							if end.group <= key {
								if let Ok(mut tail) = self.tail.write() {
									tail.account(key..key.saturating_add(1), now);
								}
								if let Some(req) = self.take_unaccepted() {
									req.reject(Error::NotFound);
								}
							}
						}
						Recv::Message(lite::SubscribeResponse::Ok(_)) => {}
						Recv::Message(msg) => {
							tracing::warn!(?msg, "unexpected subscribe fill response");
							lite::error::control(&mut self.serve.subscriber.session.clone(), Error::ProtocolViolation);
							self.finish_err(Error::ProtocolViolation);
							return Poll::Ready(());
						}
						Recv::Ended => {
							let settled = {
								let tail = self.tail.read();
								tail.covers(key..key.saturating_add(1)) && !tail.is_reading()
							};
							if settled {
								if let Some(req) = self.take_unaccepted() {
									req.reject(Error::NotFound);
								}
								self.serve.subscriber.remove_subscribe(self.id);
								self.state = SubFillState::Done;
								return Poll::Ready(());
							}
							let request = request.take();
							let frame_end = *frame_end;
							self.state = SubFillState::Drain {
								request,
								frame_end,
								settle: Box::new(Settle::new(&self.serve.subscriber.runtime, self.tail.consume())),
							};
						}
						Recv::Malformed(err) => {
							tracing::warn!(%err, "malformed subscribe fill response");
							lite::error::control(&mut self.serve.subscriber.session.clone(), Error::ProtocolViolation);
							self.finish_err(Error::ProtocolViolation);
							return Poll::Ready(());
						}
						Recv::Reset(err) => {
							self.finish_err(err);
							return Poll::Ready(());
						}
					}
				}
				SubFillState::Drain {
					request,
					frame_end,
					settle,
				} => {
					if request.is_some()
						&& let Poll::Ready(res) = self.info.poll(waiter)
					{
						match res {
							Ok(info) => {
								let req = request.take().expect("checked");
								let fe = *frame_end;
								Self::install_target(&self.serve, self.id, req, info, fe);
							}
							Err(err) => {
								self.finish_err(err);
								return Poll::Ready(());
							}
						}
					}
					let key = self.group;
					ready!(settle.poll(waiter, |tail| tail.covers(key..key.saturating_add(1))));
					if let Some(req) = self.take_unaccepted() {
						req.reject(Error::NotFound);
					}
					self.serve.subscriber.remove_subscribe(self.id);
					self.state = SubFillState::Done;
					return Poll::Ready(());
				}
				SubFillState::Done => return Poll::Ready(()),
			}
		}
	}
}
