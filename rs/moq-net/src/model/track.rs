//! A track is a collection of semi-reliable and semi-ordered streams, split into a [Producer] and [Subscriber] handle.
//!
//! A [Producer] creates streams with a sequence number and priority.
//! The sequence number is used to determine the order of streams, while the priority is used to determine which stream to transmit first.
//! This may seem counter-intuitive, but is designed for live streaming where the newest streams may be higher priority.
//! A cloned [Producer] can be used to create streams in parallel, but will error if a duplicate sequence number is used.
//!
//! A [Subscriber] may not receive all streams in order or at all.
//! These streams are meant to be transmitted over congested networks and the key to MoQ Transport is to not block on them.
//! Streams will be cached for a potentially limited duration added to the unreliable nature.
//! A [Consumer] is a cheap, cloneable handle; subscribing it multiple times fans the same
//! cached streams out to each independent [Subscriber].
//!
//! The track is closed with [Error] when all writers or readers are dropped.

use crate::sync::Mutex;
use crate::{Error, Result, StreamError, Timescale, Timestamp, coding};
use crate::{broadcast, cache, group, stats};

use super::Datagram;
use super::delivered::Delivered;
use super::skipped::Skipped;
use crate::ledger::{self, Ledger};

pub use super::subscription::{Anchor, Order, Position, Range, Subscription};
use super::subscription::{Containing, Held, Holdings, Resolved, Window, later};
use rangemap::{RangeInclusiveSet, RangeMap, RangeSet};

mod demand;
pub(super) use demand::Preferences;
use demand::Subscriptions;

mod fill;
mod miss;
pub use fill::Fill;
use fill::FillState;

#[cfg_attr(not(feature = "async"), allow(unused_imports, reason = "async-only"))]
pub(crate) use fill::{GroupFill, Readers as FillReaders};

use std::{
	collections::{BTreeMap, BTreeSet, VecDeque},
	ops::{self, Bound, RangeBounds},
	sync::Arc,
	sync::OnceLock,
	sync::PoisonError,
	sync::atomic::{AtomicBool, Ordering},
	task::{Poll, ready},
	time::Duration,
};

// The unset priority, the midpoint of the signed range. It goes out as byte 128 on moq-lite
// and 127 on IETF, which flips priority (lower first): one urgency on both.
const DEFAULT_PRIORITY: i8 = 0;

/// Maximum number of datagrams retained in the per-track send buffer.
///
/// Datagrams are a best-effort send buffer, not a replay cache (unlike groups): only the last
/// 64 datagrams are kept, so a stalled consumer cannot retain an unbounded backlog. The buffer
/// bills its bytes to the track's [`cache::Pool`], which drops the oldest under pressure.
pub(super) const MAX_DATAGRAMS: usize = 64;

/// Slack before the eviction order is rebuilt, so a track holding just a few groups
/// doesn't rebuild on every write.
const EVICT_SLACK: usize = 64;

/// How many live eviction candidates one debt payment examines (Redis-style
/// bounded sampling): enough to step over a few protected (recently accessed)
/// groups, small enough that a write never scans a long queue.
const EVICT_SCAN: usize = 4;

/// One pass over the eviction order at a fixed cache time.
#[derive(Clone, Copy)]
pub(super) struct ExpiryScan {
	start: usize,
	// Ceiling on how many entries this pass examines. `EVICT_SCAN` from the write
	// path, the whole queue from the pool's cleanup pass.
	width: usize,
	now: u64,
	max_ticks: u64,
	// Cleanup dates pending activity; write-driven scans only check dated entries.
	gc: bool,
}

/// How long a track nobody reads stays, with its cache, before it is let go.
///
/// Within the window a returning viewer, or the next of a run of back-to-back
/// fetches, finds it: a front its verdicts, a session's copy the groups it cached,
/// without another round trip upstream. Sized above the fetch cadence of a segmented
/// consumer: HLS polls every `TARGETDURATION` seconds, commonly 6 or 10. A lingering
/// track holds no upstream subscription, so waiting longer costs cached state, not a
/// viewer.
pub(crate) const IDLE_LINGER: Duration = Duration::from_secs(30);

/// Publisher-side properties of a track.
///
/// These are fixed by the publisher when the track is created and don't change
/// while the track is alive. A subscriber learns them via
/// [`broadcast::Consumer::track`](broadcast::Consumer::track),
/// which returns the publisher's [`Info`] once the subscription is accepted.
//
// Deliberately not `Copy`, even though it's now a plain value: adding `Copy` turns
// every existing `info.clone()` in a consumer's code into a `clippy::clone_on_copy`
// error under `-D warnings`.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Info {
	/// Units per second for per-frame timestamps on this track, or `None` for an untimed
	/// track.
	///
	/// A track is all timed or all untimed: every frame and datagram on a timed track
	/// carries a timestamp, none on an untimed one does, and a write that doesn't match
	/// is refused with [`Error::TimestampMismatch`]. Defaults to `None` (untimed). On
	/// Lite05+ it is reported in TRACK_INFO and the publisher zigzag-delta encodes
	/// per-frame timestamps at this scale on the wire; on moq-transport it is the
	/// TIMESCALE Track Property. A track received without one (pre-Lite05 moq-lite, or
	/// moq-transport without TIMESCALE) is untimed.
	pub timescale: Option<Timescale>,
	/// How long a superseded group stays fresh. The newest group is never stale.
	///
	/// A group is stale once either clock reaches the budget: the wall-clock time since
	/// its successor arrived at this hop, or how far its successor's first timestamp
	/// trails the live edge (see [`Subscription::max_delay`]). An untimed track is judged
	/// by the wall clock alone, and a peer whose timestamps stop advancing cannot pin a
	/// superseded group.
	///
	/// A retention bound rather than a delivery one, the inverse of an HTTP
	/// `Cache-Control: max-age`. [`Subscription::max_delay`] is clamped to this, since a
	/// group can't be waited for longer than it's kept around. Reported in TRACK_INFO so
	/// relays re-serve with the same window. `None` (the default) sets no limit;
	/// the origin cache ceiling and pool still apply. `Some(Duration::ZERO)` keeps the live edge.
	/// Stale groups are skipped by readers; reclaiming their bytes is the cache pool's
	/// [`expiry`](crate::cache::Pool::expiry) window.
	///
	/// This is the `Publisher Max Age` on the wire, the publisher-side half of the
	/// budget [`Subscription::max_delay`] sets for a subscriber.
	///
	/// Encoded as milliseconds in a QUIC varint, so a duration of `2^62` milliseconds
	/// or more cannot be put on lite-07 or IETF wires. Lite05/06 map values at or above
	/// `2^53 - 1` milliseconds to no limit for older JavaScript readers. Sub-millisecond precision is truncated
	/// (`Duration::as_millis`) at encode time.
	pub max_age: Option<Duration>,
	/// The publisher's priority for this track, higher first, defaulting to 0.
	///
	/// It breaks ties between subscriptions of equal subscriber priority. Reported in
	/// TRACK_INFO (Lite05+).
	pub priority: i8,
	/// Whether frames are DEFLATE compressed per group, so later frames compress against
	/// earlier ones in the same group. Defaults to `false`.
	///
	/// Writers and readers see plain frames either way: the producer compresses each
	/// frame as it is written and the cache stores the compressed form. A moq-lite-07 peer or a
	/// moq-transport peer negotiating the Flate extension receives the compressed
	/// bytes and the flag. Other sessions receive one shared inflated copy per group,
	/// unless a sealed compression declaration makes that fallback unsupported.
	/// Worth it for small, repetitive payloads like JSON; wasted on media.
	pub compress: bool,
	/// The ratio of a group's bytes after its first frame to the first frame past which a
	/// joining subscriber asks for a new group, or `None` (the default) to never ask.
	///
	/// A track opting in starts each group with an independent frame (a snapshot or a
	/// keyframe) that the deltas after it build on. A joiner reads the newest group from its
	/// start, so once the deltas outweigh `refresh` times the first frame, a fresh group is
	/// cheaper: [`Demand::wanted`] moves past the newest group and the publisher opens one.
	/// The joiner still receives the cached group while the new one is pending. Every hop judges a join against its own cache, which holds the same group.
	/// `Some(0)` asks on any join once a delta follows the first frame.
	///
	/// On moq-transport, `Some` is advertised as DYNAMIC_GROUPS, which arrives as `Some(0)`.
	pub refresh: Option<u32>,
	/// The draft a moq-transport publisher spoke and the Track Properties moq-net does not
	/// interpret, forwarded unchanged. `None` for a track published over moq-lite, which
	/// therefore carries no moq-transport state.
	///
	/// Relay-only: moq-lite carries them only between relays, with the `ietf` extension.
	/// Shared, since every group holds a copy of the info.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) properties: Option<Arc<crate::model::object::TrackProperties>>,
}

impl Default for Info {
	fn default() -> Self {
		Self {
			timescale: None,
			max_age: None,
			priority: DEFAULT_PRIORITY,
			compress: false,
			refresh: None,
			properties: None,
		}
	}
}

impl Info {
	/// Set the per-frame timestamp scale, or `None` for an untimed track, returning `self`
	/// for chaining.
	///
	/// Defaults to `None` (untimed). On Lite05+ this scale is reported in TRACK_INFO
	/// and used to encode per-frame timestamps on the wire.
	pub fn with_timescale(mut self, timescale: impl Into<Option<Timescale>>) -> Self {
		self.timescale = timescale.into();
		self
	}

	/// Set how old a non-latest group may get before eviction, returning `self` for chaining.
	pub fn with_max_age(mut self, max_age: impl Into<Option<Duration>>) -> Self {
		self.max_age = max_age.into();
		self
	}

	/// Set the publisher's priority, returning `self` for chaining.
	pub fn with_priority(mut self, priority: i8) -> Self {
		self.priority = priority;
		self
	}

	/// Set whether frames are DEFLATE compressed per group, returning `self` for chaining.
	pub fn with_compress(mut self, compress: bool) -> Self {
		self.compress = compress;
		self
	}

	/// Set the [`refresh`](Self::refresh) ratio, returning `self` for chaining.
	pub fn with_refresh(mut self, refresh: impl Into<Option<u32>>) -> Self {
		self.refresh = refresh.into();
		self
	}

	/// The timescale served on a moq-lite hop, preserving moq-transport timestamps only
	/// when the hop negotiated the `ietf` extension.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn lite_timescale(&self, ietf: bool) -> Option<Timescale> {
		self.timescale.filter(|_| self.ietf().is_none() || ietf)
	}

	/// The original moq-transport publisher's draft, whose timestamps may rewind, or
	/// `None` for a native moq-lite track.
	pub(crate) fn ietf(&self) -> Option<crate::ietf::Version> {
		self.properties.as_ref().map(|properties| properties.version)
	}
}

#[cfg(test)]
impl Info {
	/// A millisecond track, for tests that write timestamps.
	pub(crate) fn timed() -> Self {
		Self::default().with_timescale(Timescale::MILLI)
	}
}

/// Whether a track's cache reflects its live feed; see [`TrackState::set_idle`]. Each
/// state holds what only that feed knows, so nothing an ended feed set outlives it.
#[derive(Clone, Copy, Debug)]
enum Feed {
	/// Readers take from the cache.
	Live {
		/// Where a relative start taken before the feed produced anything counts back
		/// from: the newest group when it went live, or else the first it produced since.
		/// Kept after the group is evicted.
		first: Option<u64>,
		/// The group the route answered with, which its datagrams can show before any
		/// group reaches it: relative starts count back from it until a newer group
		/// arrives.
		edge: Option<u64>,
	},
	/// The upstream subscription ended with the copy still held, so how stale the cache
	/// is cannot be told until the route answers again.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	Idle {
		/// The newest group cached when the track went idle: what the route's answer is
		/// judged against, whatever lands before it.
		newest: Option<u64>,
	},
	/// The route answered that its feed reaches group `from`, which the cache cannot show
	/// yet: until it can, the newest group cached would read as the live edge.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	Answered { from: u64 },
}

impl Default for Feed {
	fn default() -> Self {
		Self::Live {
			first: None,
			edge: None,
		}
	}
}

impl Feed {
	fn is_live(&self) -> bool {
		matches!(self, Self::Live { .. })
	}
}

#[derive(Default)]
pub(crate) struct TrackState {
	// The publisher's properties, once known; always Some for Subscriber/Producer.
	// Copied by value into each group it creates.
	info: Option<Info>,
	// Whether a live Producer was minted. A reverse fetch may install `info`
	// before acceptance, so the two states are deliberately separate.
	published: bool,
	// Minted by `Request::stage` and not committed yet: the producer writes, but waiting
	// subscribers resolve only at `Staged::commit`, with everything written by then.
	staged: bool,
	// Whether a dynamic handler took the request. Its answer is the handler's call,
	// so ending the broadcast leaves it pending rather than rejecting it.
	claimed: bool,

	// The broadcast this track belongs to. Supplies the cache pool its groups charge
	// into and the local `cache_duration` ceiling.
	broadcast: Arc<broadcast::Info>,

	// This track's account against the shared cache pool, shared with every group it
	// creates (see `cache::Track`). Holds the gross-write counter `charge_debt` drains,
	// and the weak link a frame write follows back here to settle its own debt.
	cache: Arc<cache::Track>,
	// The upstream publisher's Largest Object, retained even on a fetch-only copy, with the
	// track's largest position once it was reported: anything received past that outdates it.
	object_edge: Option<((u64, u64), Position)>,

	// Cached groups by sequence: the single source of truth for what is cached. The
	// two orderings below hold bare sequences and validate against this map, so a
	// removed or replaced group turns their entries into discarded-on-pop hints.
	// Ordered so an in-range subscriber can seek to its cursor instead of scanning
	// every cached group for each delivery.
	lookup: BTreeMap<u64, Slot>,
	contents: group::Contents,
	// Copies that left `lookup`, or never entered it, while a fetch or reader may still
	// hold them, so a teardown ends those still being written. Pruned once it doubles
	// past `detached_kept`, the count its last prune left.
	detached: Vec<group::Detached>,
	detached_kept: usize,

	// Publisher-produced groups in arrival order as (sequence, stamp), walked by
	// subscriptions; an entry only resolves while its stamp matches the slot's.
	// Fetched backfill (`insert_group_request`) is deliberately absent: it is
	// served by sequence, never replayed to arrival-order subscribers. A fetched
	// head of a live group is the exception, served through that group's entry.
	arrival: VecDeque<(u64, u32)>,

	// Eviction order under memory pressure as (sequence, stamp): every cached
	// group except the protected latest. `pay_debt` scans victims from the front;
	// groups accessed more recently than the pool-wide average rotate to the back
	// instead of dying, decoupling eviction order from arrival order. Entries are
	// hints that only resolve while their stamp matches the slot's, so a re-served
	// sequence can't accumulate duplicate hints that alias its replacement.
	// Eviction is deliberately approximate: a bounded scan per write.
	evict: VecDeque<(u64, u32)>,

	// Outstanding eviction debt in bytes, accrued by writes while the shared pool
	// is over capacity (see `cache::Pool::accrue`) and paid by aborting this
	// track's own oldest groups. Per track, so eviction lands proportionally to
	// what each track writes and never touches another track's cache.
	debt: u64,

	// Datagrams in arrival order, bounded by `MAX_DATAGRAMS` and billed to the pool, which
	// drops the oldest under pressure. Shares the group sequence namespace but is otherwise
	// independent.
	datagrams: VecDeque<super::datagram::Buffered>,

	// Number of datagrams dropped off the front (over capacity), mapping a subscriber's absolute
	// cursor to an index into `datagrams` (mirrors `offset` for groups).
	datagram_offset: usize,

	// We've popped the front of `arrival` this many times, mapping a subscriber's
	// absolute cursor to an index.
	offset: usize,

	// The highest sequence number successfully appended to the track. Shared with
	// datagrams, so it can run ahead of any cached group.
	max_sequence: Option<u64>,

	// The newest group the publisher produced, which fetched backfill never moves: where
	// relative starts count back from (see `latest_live`).
	newest_produced: Option<u64>,

	// The checked starts cached groups hold, by sequence (see [`group::Origin::Bound`]), so a
	// first frame finds its neighbors without walking groups not yet stamped. Filled through
	// the read guard a first frame holds, so it needs its own lock, never contended.
	bounds: Mutex<BTreeMap<u64, Timestamp>>,

	// Where each cached group's frame 0 presented, by sequence, received groups too: what
	// a time anchor resolves against without walking groups that have not started. Filled
	// through the read guard a first frame holds, like `bounds`.
	starts: Mutex<BTreeMap<u64, Timestamp>>,

	// The sequence and start of the highest-sequence group that left the cache holding a
	// checked start (see [`group::Origin::Bound`]). Eviction is not a sequence prefix, so a
	// group above it is checked against it as well as its cached neighbors; one at or below
	// it may have lost its lower bound. One entry, so nothing grows with the cache.
	evicted: Option<(u64, Timestamp)>,

	// The sequence of the newest cached group: the live edge, protected from
	// eviction by never entering the eviction order until the track is `closed`.
	// Tracked separately from `max_sequence` because datagrams advance that shared
	// counter, and the live edge must still demote correctly when the next group
	// lands past one.
	latest_group: Option<u64>,

	// Incarnation counter for `Slot::stamp`.
	next_stamp: u32,

	// The exclusive end: no frame at or past it exists. Retained independently of the
	// cache, so an evicted last group does not lose where the track ended.
	end: Option<Position>,

	// Nothing more can arrive: the last producer dropped after a declared boundary.
	sealed: bool,

	// Set once a received copy's end is declared, ahead of group streams below it that QUIC
	// may deliver in any order: when the end or a start was declared, or a group last arrived since.
	// Readers wait out the grace for that tail (see [`Self::tail_deadline`]), rather than
	// end at the boundary with groups still to arrive.
	tail: Option<crate::time::Instant>,

	// The sequences the live feed delivered, as a group or a datagram: what a pending tail
	// is judged by. Fetched copies never count, and eviction never touches it. A gap folds
	// away a subscription's grace after a later arrival opened it, as the session's tail
	// does, so this holds only the gaps still worth waiting for.
	arrivals: Ledger,

	// No producer remains (aborted, sealed, or dropped), so nothing protects the live
	// edge any longer: the pool's idle expiry reclaims it like every other group, and a
	// stale consumer cannot pin the cache.
	closed: bool,

	// The first sequence the live feed serves, once the publisher declared one
	// (the wire's SUBSCRIBE_START). Lower groups are not waited for, though one
	// may still arrive late or be fetched.
	start_sequence: Option<u64>,

	// Whether `start_sequence` is only the floor a subscription asked for, still
	// waiting on the serving session to resolve where the live feed begins (a
	// lite-06+ SUBSCRIBE_START). Readers that must know the resolved start (see
	// [`Subscriber::poll_start`]) wait on it; everything else treats the floor as usual.
	start_pending: bool,

	// The budget the publisher resolved `start_sequence` from (a lite-06+ SUBSCRIBE_START),
	// skipping every older group as stale under it. Cleared by any other declaration.
	start_budget: Option<Duration>,

	// A copy received from a remote publisher, whose declared end waits for groups still
	// in flight below it (see `tail`), and whose readers' relative and time anchors are
	// refused unless `anchored` (see `Holdings::drops_anchors`).
	received: bool,
	// Whether a received copy's wire tells it what relative and time anchors resolve
	// against, so they resolve here as at the publisher.
	anchored: bool,
	// This copy can translate only time anchors its cache has already settled. A relative one
	// waits for the live edge its wire reports, as on an `anchored` copy.
	cached_anchors: bool,
	// Whether readers ask no fills below `start_sequence`: its publisher resolved it from the
	// budget, but neither this copy's timestamps nor a fill can judge an older group against
	// a reader's budget. A widened budget reaches lower groups through the feed's re-offer.
	fills_from_start: bool,
	// Sequences the upstream publisher gave up, with the error each was given up with: an
	// incoming SUBSCRIBE_DROP or a producer cut. Kept apart from the cache so reclaiming a
	// slot cannot erase a disposition a relay must forward, and as ranges so a wide DROP
	// costs one entry.
	drops: RangeMap<u64, StreamError>,
	// Bumped on every insert into `drops`, so a cursor rescans it only after a change.
	drops_revision: u64,
	drops_changes: ledger::Changes,

	// Whether the cache reflects the live feed: readers get nothing from it while it does
	// not, since how stale it is cannot be told; fetches still do. A track is live from
	// creation, and only a session's copy goes idle: when its upstream subscription ends
	// with the copy still held, until the route's answer shows in the cache.
	feed: Feed,
	// Readers start at this group: the route's live feed starts past a gap after
	// everything cached, so nothing bounds how old the cache below it is.
	live_floor: Option<u64>,

	// Where production stopped, snapshotted when the open groups are released (an
	// abort, or the last producer dropping). Computed live from the cache otherwise;
	// see [`Self::resume_position`].
	resume: Option<Position>,

	// The error that caused the track to be aborted, if any.
	abort: Option<Error>,

	// Whether the declared end settled before a write that could reopen it or an abort, or
	// `closed()` reported it: every group below it was produced and finished then, so the
	// clean end stands for good. Atomic so reporting it latches through a read guard. See
	// [`Self::latch_settled`].
	settled: AtomicBool,

	// Active subscriptions, in their own [`kio::Lock`] so a read-only `Consumer`
	// registers under that lock instead of writing back into the track state.
	// Kept here (rather than threaded through every handle) so any holder reaches it.
	subscriptions: kio::Lock<Subscriptions>,

	// Demand wakes a write owes, fired once the state guard is released (see
	// [`Self::deferred`]). A woken waker may read this track, so none fires under its lock.
	deferred: Vec<kio::WaiterList>,

	// The fills readers queued for groups they miss (see [`Fill`]), same reasoning: a miss
	// queues here and a `Requests` drains.
	fills: kio::Shared<FillState>,

	// A front's logical track: read straight from its routes' copies rather than this
	// cache, which stays empty; see [`super::resume`].
	routes: Option<super::resume::Consumer>,
}

/// The starts a group's first frame must fall between: a timed track never holds a group
/// whose start is earlier than the one before it. Read under the track lock and checked
/// under it, so no two groups stamped concurrently can each accept a rewind.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Neighbors {
	floor: Option<Timestamp>,
	ceiling: Option<Timestamp>,
	// At or below the eviction watermark, where the floor may have been evicted.
	late: bool,
}

impl Neighbors {
	/// Refuse a `start` that rewinds past a neighbor's with [`Error::TimestampMismatch`],
	/// or say how the group holds it. An equal start is allowed: a keyframe may land at the
	/// previous group's start.
	pub(super) fn admit(&self, start: Timestamp) -> Result<group::Origin> {
		if self.floor.is_some_and(|floor| start < floor) || self.ceiling.is_some_and(|ceiling| start > ceiling) {
			return Err(Error::TimestampMismatch);
		}
		Ok(if self.late {
			group::Origin::Unverified
		} else {
			group::Origin::Bound
		})
	}
}

/// A cached group plus its bookkeeping in the track's `lookup` map.
///
/// Access times and the evictable-population sample live in the group's own
/// `cache::Charge`, so they share the group's lifecycle exactly: an abort from any
/// handle releases the bytes and the sample together.
struct Slot {
	group: group::Handle,

	// Incarnation stamp, echoed by this slot's arrival entry (if any). A re-served
	// sequence (an aborted group re-created by the publisher or re-fetched as
	// backfill) gets a fresh stamp, so a historical arrival entry can't resolve to
	// the replacement and deliver it twice or at the wrong position.
	stamp: u32,

	// Whether this incarnation came from the live publisher and can replace older
	// subscription content. Fetch-only backfill stays cached but never anchors drift.
	visible: bool,

	// When the group arrived here, before its first frame: the start of the wall clock for
	// the group before it (see [`TrackState::arrival`]). Read on the pool's clock.
	arrived: crate::time::Instant,

	// A fetched copy of a live `group` that the feed started past the frames a fetch
	// asked for. One that runs to the end of the group is served in the live group's
	// place while it lasts; `group` stays the slot, still written by the feed,
	// so an abandoned fetch leaves the group servable rather than gone. Boxed: rare.
	head: Option<Box<group::Handle>>,

	// A cut on an ended track that outlived its expiry: readers no longer get it, as they
	// no longer get the group it was. Atomic because an ended track's sweep holds only a
	// read guard, so it cannot reclaim the slot.
	retired: AtomicBool,
}

impl Slot {
	/// Whether the live feed's group still holds this slot.
	fn is_live(&self) -> bool {
		self.visible && !self.group.is_aborted()
	}

	/// Whether this slot counts as live content: [`Self::is_live`], and its first frame
	/// opened or it finished. A frameless group is handed to readers, but no judgment
	/// counts it until then: not the live edge, a successor, nor the rejoin wait.
	fn is_shown(&self) -> bool {
		self.is_live() && self.group.is_started()
	}

	/// Whether a group-only reader may treat this slot as reaching the original end.
	fn serves_end(&self) -> bool {
		let group = self.serving();
		!group.is_aborted() && !group.back_cut()
	}

	/// Whether the arrival walk hands this slot out: the live feed's, and still serving, a
	/// fetched head counting for an aborted live group.
	fn is_walked(&self) -> bool {
		self.visible && self.serves_end()
	}

	/// Why the publisher gave up this live-feed group readers were shown, once nothing
	/// serves its end: a reader is handed the group to see that error, never a missing group.
	fn cut(&self) -> Option<&Error> {
		match self.visible && !self.serves_end() && !self.retired.load(Ordering::Relaxed) {
			true => self.group.cut_error(),
			false => None,
		}
	}

	/// Whether a cursor hands this slot out: walked, or cut.
	fn is_read(&self) -> bool {
		self.is_walked() || self.cut().is_some()
	}

	/// The copy to hand readers: the fetched head while it lasts, else the live group.
	fn serving(&self) -> &group::Handle {
		match &self.head {
			Some(head) if !head.is_aborted() && !head.back_cut() => head,
			_ => &self.group,
		}
	}

	/// Whether the copy serving this slot finished, even one a transport bound cut short, so
	/// nothing it promised a reader is still in flight. A fetched head serving the group to
	/// its end is judged in place of the live copy beside it, which may still be written or
	/// may have been cut or evicted. Once no head serves, an aborted live copy holds the end
	/// open: a session that died mid-group delivered less than the end promised.
	fn is_settled(&self) -> bool {
		self.serving().is_settled()
	}

	/// Every copy this slot holds, the live group first.
	fn copies(&self) -> impl Iterator<Item = &group::Handle> {
		std::iter::once(&self.group).chain(self.head.as_deref())
	}

	/// Whether every copy is gone, so the slot has nothing left to serve.
	fn is_aborted(&self) -> bool {
		self.copies().all(group::Handle::is_aborted)
	}

	/// The mean access over copies still held, weighed the way each one samples the
	/// pool's average: a maximum would sit above the mean forever, so a lone slot with
	/// two copies could never be evicted.
	fn cache_accessed(&self) -> u64 {
		let (sum, count) = self
			.copies()
			.filter(|group| !group.is_aborted())
			.fold((0u128, 0u128), |(sum, count), group| {
				(sum + group.cache_accessed() as u128, count + 1)
			});
		(sum / count.max(1)) as u64
	}

	/// The bytes every copy holds, all freed together.
	fn cache_size(&self) -> u64 {
		let mut size = self.group.cache_size();
		if let Some(head) = &self.head {
			size += if head.same_content(&self.group) {
				head.delivery_size()
			} else {
				head.cache_size()
			};
		}
		size
	}

	/// Whether every copy still held sat idle past the scan's expiry window. Asks
	/// each one, so a cleanup scan dates them all.
	fn is_expired(&self, scan: &ExpiryScan) -> bool {
		let idle = |group: &group::Handle| {
			group
				.cache_accessed_tick(scan.gc.then_some(scan.now))
				.is_some_and(|tick| scan.now.saturating_sub(tick) > scan.max_ticks)
		};
		// No copy of a cut serves its end, so it ages as the live group it was: a reader
		// behind still gets it for as long as it would have got that group.
		let mut expired = self.cut().is_none() || idle(&self.group);
		// Not `all`, which stops at the first live copy and leaves the rest undated.
		for group in self.copies().filter(|group| !group.is_aborted()) {
			expired &= idle(group);
		}
		expired
	}

	/// Whether nothing is left to keep the slot for: aborted, and not a cut that ages
	/// like a group.
	fn is_spent(&self) -> bool {
		self.is_aborted() && self.cut().is_none()
	}

	/// Enter every copy into the evictable population, so the pool's access average
	/// samples whichever one readers use.
	fn cache_demote(&self) {
		for group in self.copies() {
			group.cache_demote();
		}
	}

	/// Where the group starts in presentation time, from a live copy holding frame 0. A
	/// copy joined mid-group stamps a later frame, which proves nothing about the start.
	fn start(&self) -> Option<Timestamp> {
		self.copies()
			.filter(|group| group.live_first_frame() == Some(0))
			.find_map(group::Handle::timestamp)
	}

	/// Whether the group has no start yet but may still get one: a copy holding frame 0
	/// has not presented it.
	fn may_start(&self) -> bool {
		self.start().is_none() && self.copies().any(group::Handle::may_start)
	}

	/// The checked start of whichever copy began at frame 0.
	fn bound(&self) -> Option<Timestamp> {
		self.copies().find_map(group::Handle::bound)
	}

	/// Abort every copy this slot holds.
	fn abort(&self, err: Error) {
		for group in self.copies() {
			let _ = group.clone().abort(err.clone());
		}
	}
}

/// Heap the track keeps per cached group, excluding the group itself
/// ([`group::CACHE_OVERHEAD`]).
///
/// One [`Slot`] under its sequence in `lookup`, its start in `starts` once frame 0
/// arrives, plus a hint in each of `arrival` and `evict`. Doubled because the
/// containers run half empty in the worst case: a
/// `BTreeMap` node sits between half and fully packed, and a `VecDeque` holds up to
/// twice the entries in it. Half of [`cache::ENTRY_OVERHEAD`]; see it for why this is
/// derived rather than measured.
pub(super) const CACHE_OVERHEAD: u64 =
	2 * (size_of::<u64>() + size_of::<Slot>() + size_of::<(u64, Timestamp)>() + 2 * size_of::<(u64, u32)>()) as u64;

/// Where the groups this hop holds start, for resolving time anchors: ordered lookups in
/// the starts index and the cache, so no query walks more than the run it answers.
impl Holdings for TrackState {
	fn timed(&self) -> bool {
		self.info.as_ref().is_none_or(|info| info.timescale.is_some())
	}

	fn drops_anchors(&self) -> bool {
		self.received && !self.anchored
	}

	fn containing(&self, pts: Timestamp) -> Containing {
		let starts = self.starts.lock().unwrap_or_else(PoisonError::into_inner);
		let (Some((&first, &first_at)), Some((&last, &last_at))) = (starts.first_key_value(), starts.last_key_value())
		else {
			return Containing::Unknown;
		};
		if later(first_at, pts) {
			return Containing::Before;
		}
		if !later(last_at, pts) {
			return Containing::Group(last);
		}
		// Timestamps never rewind, so the starts ascend with the sequence: bisect the
		// sequences between the newest start known at or before `pts` and one known after.
		let (mut best, mut low, mut high) = (first, first, last);
		while high - low > 1 {
			let mid = low + (high - low) / 2;
			match starts.range(low + 1..=mid).next_back() {
				Some((&sequence, &at)) if later(at, pts) => high = sequence,
				Some((&sequence, _)) => (best, low) = (sequence, mid),
				None => low = mid,
			}
		}
		Containing::Group(best)
	}

	fn next_start(&self, from: u64) -> Option<(u64, Timestamp)> {
		let starts = self.starts.lock().unwrap_or_else(PoisonError::into_inner);
		starts.range(from..).next().map(|(&sequence, &at)| (sequence, at))
	}

	fn next_held(&self, from: u64) -> Option<(u64, Held)> {
		let (&sequence, slot) = self.lookup.range(from..).next()?;
		let held = match slot.start() {
			Some(at) => Held::Started(at),
			None if slot.may_start() => Held::Pending,
			None => Held::Never,
		};
		Some((sequence, held))
	}

	fn gone(&self, sequence: u64) -> Option<u64> {
		// Without a declared start, the first arrival stands in for it, as the tail's does.
		let floor = self.start_sequence.or(self.arrivals.first()).unwrap_or(0);
		if sequence < floor {
			return Some(floor);
		}
		self.drops
			.get_key_value(&sequence)
			.map(|(dropped, _)| dropped.end)
			.or_else(|| self.arrivals.run_end(sequence))
	}

	fn complete(&self) -> bool {
		self.sealed || self.abort.is_some()
	}
}

impl TrackState {
	/// The upstream publisher's Largest Object, unless a frame or group received since its
	/// report outdates it, even one the cache has since evicted.
	fn object_edge(&self) -> Option<(u64, u64)> {
		let (edge, mark) = self.object_edge?;
		self.cache
			.largest()
			.is_none_or(|largest| largest <= mark)
			.then_some(edge)
	}

	/// A consumer joined at the live edge: ask for a group past every sequence written when
	/// the newest group's bytes after its first frame exceed [`Info::refresh`] times that
	/// frame.
	///
	/// Joins while the request is pending ask for the same group, so they coalesce until
	/// the publisher creates it. A join under the ratio reads the current group and asks
	/// for nothing, even while a request is pending.
	///
	/// Takes the read guard to release it before locking the demand, so the two locks
	/// never nest.
	fn join(state: kio::Ref<'_, TrackState>) {
		let Some(ratio) = state.info.as_ref().and_then(|info| info.refresh) else {
			return;
		};
		let Some(latest) = state.latest_group else {
			return;
		};
		if !state
			.lookup
			.get(&latest)
			.is_some_and(|slot| slot.serving().over_refresh(ratio))
		{
			return;
		}
		Self::refresh(state);
	}

	/// A reader of group `sequence` reached a frame its copy shed: ask for a new group as
	/// a join over [`Info::refresh`] does, if it is the newest and the track opts in. An
	/// older group's reader moves on to the groups after it instead.
	pub(super) fn shed_read(state: kio::Ref<'_, TrackState>, sequence: u64) {
		let opted = state.info.as_ref().is_some_and(|info| info.refresh.is_some());
		if opted && state.latest_group == Some(sequence) {
			Self::refresh(state);
		}
	}

	/// Ask for a group past every sequence written, releasing the read guard before
	/// locking the demand.
	fn refresh(state: kio::Ref<'_, TrackState>) {
		let subscriptions = state.subscriptions.clone();
		drop(state);
		let mut wakes = subscriptions.lock().refresh();
		wakes.wake();
	}

	/// The starts a first frame of group `sequence` must fall between: the nearest checked
	/// starts cached on either side, since groups land out of sequence order, and the
	/// eviction watermark. A group at or below the watermark is late: its floor may be gone,
	/// so the watermark caps it instead.
	///
	/// Usually one group lookup each way; only unstarted or partial neighbors are skipped.
	/// `None` on a track with moq-transport provenance, whose timestamps may rewind.
	pub(super) fn neighbors(&self, sequence: u64) -> Option<Neighbors> {
		if self.info.as_ref().is_some_and(|info| info.ietf().is_some()) {
			return None;
		}
		let bounds = self.bounds.lock().unwrap_or_else(PoisonError::into_inner);
		let below = bounds.range(..sequence).next_back().map(|(_, start)| *start);
		let above = bounds
			.range((Bound::Excluded(sequence), Bound::Unbounded))
			.next()
			.map(|(_, start)| *start);
		drop(bounds);
		Some(match self.evicted {
			Some((evicted, start)) if sequence <= evicted => Neighbors {
				floor: below,
				ceiling: Some(above.map_or(start, |above| above.min(start))),
				late: true,
			},
			Some((_, start)) => Neighbors {
				floor: Some(below.map_or(start, |below| below.max(start))),
				ceiling: above,
				late: false,
			},
			None => Neighbors {
				floor: below,
				ceiling: above,
				late: false,
			},
		})
	}

	/// Group `sequence` wrote its first frame: index where it starts, and share a
	/// just-checked `bound` with its neighbors.
	pub(super) fn first_frame(&self, sequence: u64, bound: Option<Timestamp>) {
		if let Some(bound) = bound {
			self.share_bound(sequence, bound);
		}
		self.index_start(sequence);
	}

	/// Index where cached group `sequence` starts, read back through its slot so only the
	/// cached copy holding frame 0 counts, and wake the reads a time end parks on it.
	fn index_start(&self, sequence: u64) {
		let Some(start) = self.lookup.get(&sequence).and_then(Slot::start) else {
			return;
		};
		self.starts
			.lock()
			.unwrap_or_else(PoisonError::into_inner)
			.insert(sequence, start);
		self.cache.wakes().started(sequence);
	}

	/// Record group `sequence`'s just-checked `start` as a bound for its neighbors. A group
	/// stamped after it left the cache bounds nothing.
	fn share_bound(&self, sequence: u64, start: Timestamp) {
		if self.lookup.get(&sequence).and_then(Slot::bound) != Some(start) {
			return;
		}
		self.bounds
			.lock()
			.unwrap_or_else(PoisonError::into_inner)
			.insert(sequence, start);
	}

	/// Take group `sequence` out of the cache, raising the eviction watermark to its start.
	fn uncache(&mut self, sequence: u64) -> Option<Slot> {
		let slot = self.remove_slot(sequence)?;
		if let Some(start) = slot.bound()
			&& self.evicted.is_none_or(|(evicted, _)| sequence > evicted)
		{
			self.evicted = Some((sequence, start));
		}
		Some(slot)
	}

	/// [`Self::uncache`] a group that another copy displaces rather than one that ends, so a
	/// teardown still ends the copies its readers hold.
	fn displace(&mut self, sequence: u64) {
		if let Some(slot) = self.uncache(sequence) {
			for group in slot.copies() {
				self.detach(group);
			}
		}
	}

	/// Keep `group` for a teardown to end, though the cache does not hold it.
	fn detach(&mut self, group: &group::Handle) {
		self.detached.push(group.detach());
		if self.detached.len() > 2 * self.detached_kept {
			self.detached.retain(|copy| copy.writing().is_some());
			self.detached_kept = self.detached.len();
		}
	}

	/// Record sequences the upstream publisher gave up, with `err`.
	fn drop_groups(&mut self, groups: ops::Range<u64>, err: StreamError) {
		if groups.is_empty() {
			return;
		}
		self.cache.wakes().arrived(groups.clone());
		self.change_drops(groups, Some(err));
	}

	/// Keep the change index aligned with the range map, including the neighbours an
	/// insertion coalesces and the two pieces a removal can leave.
	fn change_drops(&mut self, groups: ops::Range<u64>, err: Option<StreamError>) {
		let mut affected = groups.start.saturating_sub(1)..groups.end.saturating_add(1);
		let old: Vec<_> = self.drops.overlapping(&affected).map(|(run, _)| run.clone()).collect();
		for run in old {
			self.drops_changes.remove(run.start);
			affected.start = affected.start.min(run.start);
			affected.end = affected.end.max(run.end);
		}
		match err {
			Some(err) => self.drops.insert(groups, err),
			None => self.drops.remove(groups),
		}
		for (run, _) in self.drops.overlapping(&affected) {
			self.drops_changes.insert(run.clone());
		}
		self.drops_revision += 1;
	}

	/// Remove an aborted slot, keeping a cut as a drop: reclaiming it must not erase a
	/// disposition a relay still has to forward.
	fn reclaim_aborted(&mut self, sequence: u64) {
		if let Some(slot) = self.uncache(sequence)
			&& slot.visible
			&& let Some(err) = slot.group.cut_error()
			&& let Some(end) = sequence.checked_add(1)
		{
			self.drop_groups(sequence..end, StreamError::from(err));
		}
	}

	/// Why `sequence` was given up, if it was: cut while cached, or dropped.
	fn given_up(&self, sequence: u64) -> Option<Error> {
		if let Some(err) = self.lookup.get(&sequence).and_then(Slot::cut) {
			return Some(err.clone());
		}
		self.drops.get(&sequence).cloned().map(Error::Stream)
	}

	fn accept(&mut self, info: Info) {
		self.published = true;
		self.install(info);
	}

	fn poll_info(&self) -> Poll<Result<Info>> {
		if let Some(info) = self.info.as_ref().filter(|_| !self.staged) {
			Poll::Ready(Ok(info.clone()))
		} else if let Some(err) = &self.abort {
			// Aborted before anyone served it, so the info can never arrive: fail the
			// waiting subscribes instead of parking them on a track nobody will fill.
			Poll::Ready(Err(err.clone()))
		} else {
			Poll::Pending
		}
	}

	/// Whether readers may take from the cache: it reflects the live feed, or the track
	/// reached its end, which makes the cache the whole of it. An end declared but not
	/// reached does not: the groups before it are still to come from the feed.
	fn readable(&self) -> bool {
		self.feed.is_live() || self.sealed || self.abort.is_some() || self.is_complete()
	}

	/// Where relative starts count back from: the live feed's newest group, produced or
	/// answered. `None` while the cache does not reflect the feed, since an idle copy's
	/// newest group is only what an ended feed left: a start taken then waits, as on an
	/// empty track. An idle copy that ends counts from its newest cached group.
	fn latest_live(&self) -> Option<u64> {
		let edge = match self.feed {
			Feed::Live { edge, .. } => edge,
			_ => None,
		};
		let floor = self.live_floor.unwrap_or(0);
		self.newest_produced
			.max(edge)
			.filter(|&latest| self.readable() && latest >= floor)
	}

	/// Where a relative start that waited on [`Self::latest_live`] counts back from. An idle
	/// copy that ended without going live counts from its newest group, which its cache now
	/// holds the whole of, and not from where an earlier feed began.
	fn first_live(&self) -> Option<u64> {
		match self.feed {
			Feed::Live { first, .. } => first,
			_ => self.latest_live(),
		}
	}

	/// The cache reflects the live feed again, which reaches `edge` if the route named it:
	/// a relative start that waited counts back from its newest group now.
	fn go_live(&mut self, edge: Option<u64>) {
		self.feed = Feed::Live { first: None, edge };
		self.feed = Feed::Live {
			first: self.latest_live(),
			edge,
		};
	}

	/// Find the next live group at or after `index` in arrival order.
	///
	/// Returns the group and its absolute index so the consumer can advance past it.
	/// The cached producer rather than a consumer: `consume` reads the clock and can
	/// take the group's own lock, which the caller does once the track guard is gone.
	fn poll_recv_group(&self, index: usize, min_sequence: u64) -> Poll<Result<Option<(group::Handle, usize)>>> {
		let start = index.saturating_sub(self.offset);
		let readable = self.readable();
		let min_sequence = min_sequence.max(self.live_floor.unwrap_or(0));
		for (i, (sequence, stamp)) in self.arrival.iter().enumerate().skip(start).filter(|_| readable) {
			// A cut group is handed out too, so its reader sees why it ended.
			if *sequence >= min_sequence
				&& let Some(slot) = self.lookup.get(sequence)
				&& slot.stamp == *stamp
				&& slot.is_read()
			{
				return Poll::Ready(Ok(Some((slot.serving().clone(), self.offset + i))));
			}
		}

		// TODO once we have drop notifications, check if index == final_sequence.
		if self.is_complete() {
			Poll::Ready(Ok(None))
		} else if let Some(err) = &self.abort {
			Poll::Ready(Err(err.clone()))
		} else {
			Poll::Pending
		}
	}

	/// Find the next datagram at or after the subscriber's absolute `index`.
	///
	/// Returns the datagram and its absolute index so the consumer can advance past it. A
	/// consumer whose `index` has fallen behind `datagram_offset` (older datagrams dropped)
	/// resumes at the oldest still-buffered datagram, skipping the lost ones.
	fn poll_recv_datagram(&self, index: usize) -> Poll<Result<Option<(super::datagram::Stored, usize)>>> {
		let start = index.saturating_sub(self.datagram_offset);
		// An ended track's expiry releases its oldest in place, which a reader skips like a drop.
		for (i, datagram) in self
			.datagrams
			.iter()
			.enumerate()
			.skip(start)
			.filter(|_| self.readable())
		{
			if let Some(stored) = datagram.stored() {
				return Poll::Ready(Ok(Some((stored, self.datagram_offset + i))));
			}
		}

		// Nothing buffered at the cursor: the track ending terminates the datagram stream too.
		if self.is_complete() {
			Poll::Ready(Ok(None))
		} else if let Some(err) = &self.abort {
			Poll::Ready(Err(err.clone()))
		} else {
			Poll::Pending
		}
	}

	/// Whether `sequence` was sent as a datagram still in the send buffer.
	fn holds_datagram(&self, sequence: u64) -> bool {
		self.datagrams.iter().any(|datagram| datagram.sequence == sequence)
	}

	/// Push a datagram, dropping the oldest when the send buffer is full.
	///
	/// Billed before the debt is settled, so its bytes accrue debt, but pushed after, so the
	/// newcomer is never a victim of its own write.
	fn push_datagram(&mut self, datagram: super::datagram::Stored) {
		self.latch_settled();
		let datagram = super::datagram::Buffered::new(datagram, &self.cache);
		self.charge_debt();
		if self.datagrams.len() == MAX_DATAGRAMS {
			self.drop_datagram();
		}
		let sequence = datagram.sequence;
		self.datagrams.push_back(datagram);
		let now = self.cache.pool().now_wall();
		self.record_arrival(sequence, now);
		self.shown(sequence);
	}

	/// Drop the oldest buffered datagram, releasing its bill.
	fn drop_datagram(&mut self) {
		if self.datagrams.pop_front().is_some() {
			self.datagram_offset += 1;
		}
	}

	/// Find the smallest-sequence cached group in `window` satisfying
	/// `next_sequence <= seq < end_sequence (if set)`. Used by
	/// [`Subscriber::next_group`] so the range can be widened (or unset)
	/// after the fact and previously-skipped cached groups become available
	/// without scanning past them in arrival order.
	///
	/// Returns `Poll::Pending` when no in-range group is currently cached but
	/// future groups could still arrive in range; returns `Ok(None)` only when
	/// the track is finalized and no further in-range group is possible.
	fn poll_next_in_range(
		&self,
		next_sequence: u64,
		end_sequence: Option<u64>,
		window: &Window,
	) -> Poll<Result<Option<group::Handle>>> {
		// Nothing more can arrive once the track was sealed (dropped with its end
		// declared) or aborted. Either closed the channel, so parking is not an option:
		// the consumer would be handed the abort, or `Dropped`, instead of how it ended.
		let closed = self.sealed || self.abort.is_some();
		// Once nothing more is in range: an abort before the end settled cut the track
		// off. One after it is a clean end (see `is_complete`), as is reaching the end.
		let end = || match &self.abort {
			Some(err) if !self.settled.load(Ordering::Relaxed) => Err(err.clone()),
			_ => Ok(None),
		};

		// If the exclusive end is already at or below where we'd resume, no
		// group can ever satisfy this call until the cap rises or the ranges widen.
		// Pending (not None) so the consumer is parked rather than told the stream
		// is over, unless the track declared its end: a finished track ends a reader
		// with nothing left in range, as the arrival path does. An empty range
		// (`end == 0`) parks even at the first sequence.
		let cap = match (end_sequence, window.end()) {
			(Some(cap), Some(end)) => Some(cap.min(end)),
			(cap, end) => cap.or(end),
		};
		if let Some(cap) = cap
			&& cap <= next_sequence
		{
			if closed || self.final_sequence().is_some() {
				return Poll::Ready(end());
			}
			return Poll::Pending;
		}

		let best = window
			.runs(next_sequence.max(self.live_floor.unwrap_or(0)))
			.filter(|_| self.readable())
			.find_map(|run| {
				// A cut group is handed out too, so its reader sees why it ended.
				self.lookup
					.range(run)
					.map(|(_, slot)| slot)
					.take_while(|slot| super::subscription::before_end(slot.group.sequence, end_sequence))
					.find(|slot| slot.serves_end() || slot.cut().is_some())
					.map(Slot::serving)
			});

		if let Some(group) = best {
			// Deliberately no cache refresh here: this is a pure seek, and the caller may
			// convict the candidate rather than deliver it. The caller stamps what it
			// delivers; a group walked off must not be shielded from eviction. The caller
			// consumes it with the track guard released, like `poll_recv_group`.
			return Poll::Ready(Ok(Some(group.clone())));
		}

		// No in-range group is cached. Decide whether more could ever arrive.
		// `final_sequence` is one past the last possible sequence. If our
		// floor is already at/past it, nothing else can land in range.
		// A closed track produces nothing more either, so a gap below its
		// boundary is the end, not a wait, as is one in a complete track, which
		// ends the arrival path too. Cached in-range groups were returned above,
		// so an aborted track drains what finished first.
		// This is the cursor the ordered reader uses.
		if closed || self.is_complete() || self.final_sequence().is_some_and(|fin| next_sequence >= fin) {
			return Poll::Ready(end());
		}
		Poll::Pending
	}

	/// The cached group for `sequence`, but only when it holds every frame from
	/// `frame_start` onward.
	///
	/// A group cached from a frame-bounded subscription starts partway in, and serving
	/// it to someone who asked for the whole group would silently hand back a tail. It
	/// is a miss instead, so the fetch goes upstream for the frames that are missing.
	fn covering_group(&self, sequence: u64, frame_start: u64) -> Option<&group::Handle> {
		self.covering_range(
			sequence,
			group::Bounds {
				start: frame_start,
				end: None,
			},
		)
	}

	fn covering_range(&self, sequence: u64, frames: group::Bounds) -> Option<&group::Handle> {
		self.lookup.get(&sequence)?.copies().find(|group| group.covers(frames))
	}

	/// The local retention bound, or `None` when unknown or unlimited.
	/// Bounds the aggregate subscription without changing the publisher's metadata.
	fn max_age_bound(&self) -> Option<Duration> {
		let published = self.info.as_ref()?.max_age;
		let ceiling = self.broadcast.cache_duration;
		match published {
			Some(age) => Some(age.min(ceiling)),
			None => (ceiling != Duration::MAX).then_some(ceiling),
		}
	}

	/// The live edge a subscription served `servable` measures drift against: the
	/// highest-sequence group in it that has presented a frame, or `None` while nothing
	/// stamped is servable (an unstamped track drives no media-time expiry; the wall
	/// clock alone judges it).
	///
	/// One scan answers a whole poll. The highest-sequence edge in range is above
	/// every candidate below it and above none at or past it. Recomputing per candidate
	/// would make walking a backlog of N groups off in one poll cost O(N^2).
	///
	/// Bounded because a group can only be late relative to data that would actually be
	/// served in its place: a reader capped by [`Subscriber::with_range`] can't jump past
	/// the cap, and one reading disjoint ranges never reads the gap between them, so the
	/// groups it still wants aren't stale just because the track ran on.
	/// Fetched backfill is absent from `arrival`, so it cannot age subscription content
	/// as though it were a live replacement.
	fn live_edge(&self, servable: &Servable) -> Option<PresentationEdge> {
		self.find_servable(servable, 0, true, |slot| {
			if !slot.is_shown() {
				return None;
			}
			// The map is ordered by sequence, so the first stamped group from the
			// back is the newest content that exists.
			let timestamp = slot.group.timestamp()?;
			Some(PresentationEdge {
				sequence: slot.group.sequence,
				stamp: slot.stamp,
				timestamp: slot.group.latest().unwrap_or(timestamp),
			})
		})
	}

	/// This track's own edge within `servable`, for measuring drift.
	///
	/// None on an untimed track, where nothing is ever stale: its groups carry no
	/// timestamp, so looking for one would scan the whole cache on every poll.
	fn drift_edge(&self, servable: &Servable) -> Edge {
		let untimed = self.info.as_ref().is_some_and(|info| info.timescale.is_none());
		Edge {
			presentation: if untimed { None } else { self.live_edge(servable) },
			servable: servable.clone(),
		}
	}

	/// The first cached group at or above `from` that `servable` covers for which `f`
	/// answers, lowest first or, with `rev`, highest first: one lookup for a single run.
	/// Several runs leapfrog the cache, so a run holding nothing (a reader's misses) costs
	/// one lookup rather than a walk over every run past it.
	fn find_servable<'a, R>(
		&'a self,
		servable: &Servable,
		from: u64,
		rev: bool,
		mut f: impl FnMut(&'a Slot) -> Option<R>,
	) -> Option<R> {
		let mut scan = |range: std::collections::btree_map::Range<'a, u64, Slot>| match rev {
			true => range.rev().find_map(|(_, slot)| f(slot)),
			false => range.map(|(_, slot)| slot).find_map(&mut f),
		};
		match servable {
			Servable::Run { start, end } => {
				let from = from.max(*start);
				// A range ending before it starts would panic, and holds nothing anyway.
				if end.is_some_and(|end| end < from) {
					return None;
				}
				let end = end.map_or(Bound::Unbounded, Bound::Excluded);
				scan(self.lookup.range((Bound::Included(from), end)))
			}
			Servable::Runs(runs) => {
				// What is left to search, shrinking from the end the search starts at.
				let (mut lo, mut hi) = (from, u64::MAX);
				while lo <= hi {
					let mut left = runs.overlapping(lo..=hi);
					let run = match rev {
						true => left.next_back(),
						false => left.next(),
					}?;
					let (start, end) = ((*run.start()).max(lo), (*run.end()).min(hi));
					// The cache from the run on, toward the far end: one lookup either finds
					// the run's groups or the next cached group past a run holding none.
					let leap = match rev {
						true => leap(self.lookup.range(lo..=end).rev(), start..=end, &mut f),
						false => leap(self.lookup.range(start..=hi), start..=end, &mut f),
					};
					match leap {
						Leap::Found(found) => return Some(found),
						Leap::Empty => return None,
						Leap::Past(sequence) if rev => hi = sequence,
						Leap::Past(sequence) => lo = sequence,
						Leap::Exhausted if rev => hi = start.checked_sub(1)?,
						Leap::Exhausted => lo = end.checked_add(1)?,
					}
				}
				None
			}
		}
	}

	/// Whether `sequence` still holds the servable incarnation `stamp`.
	fn holds(&self, sequence: u64, stamp: u32) -> bool {
		self.lookup
			.get(&sequence)
			.is_some_and(|slot| slot.stamp == stamp && slot.serves_end())
	}

	/// The furthest presentation time the group at `sequence` could still reach: where
	/// its immediate successor begins, or `None` when nothing proves where it stops.
	///
	/// An upper bound, deliberately. A frame's duration is not on the wire, so a group's
	/// own last timestamp says where it *starts* presenting, not where it ends; only its
	/// successor's start proves it cannot run past it. The bound is the *immediate*
	/// servable successor's start, so an unstamped successor leaves the reach unbounded
	/// until it presents its first frame, and so does having no successor in `servable`
	/// at all.
	fn reach(&self, sequence: u64, servable: &Servable) -> Option<Timestamp> {
		self.successor(sequence, servable)?.group.timestamp()
	}

	/// The group after `sequence` that bounds it: the first servable one in `servable` whose
	/// first frame opened or that finished, stamped or not, empty or not.
	fn successor(&self, sequence: u64, servable: &Servable) -> Option<&Slot> {
		self.find_servable(servable, sequence.checked_add(1)?, false, |slot| {
			slot.is_shown().then_some(slot)
		})
	}

	/// The group after `sequence` that starts its wall clock: like [`Self::successor`], but
	/// counting a group before its first frame. Its header arrived, so a relay ages the group
	/// before it as the origin does, even if its first frame never comes.
	fn arrival(&self, sequence: u64, servable: &Servable) -> Option<&Slot> {
		self.find_servable(servable, sequence.checked_add(1)?, false, |slot| {
			slot.is_live().then_some(slot)
		})
	}

	/// The slot `select` picks, with `waiter` parked on its abort.
	///
	/// An abort only closes the group, never touching the track, so one that landed before
	/// registering re-selects, or the replacement would go unwatched.
	fn watch_abort<'a>(
		&'a self,
		waiter: &kio::Waiter,
		select: impl Fn(&'a Self) -> Option<&'a Slot>,
	) -> Option<&'a Slot> {
		loop {
			match select(self) {
				Some(slot) if slot.group.poll_aborted(waiter).is_ready() => continue,
				slot => return slot,
			}
		}
	}

	/// Whether `sequence`'s successor arrived a full `budget` ago.
	///
	/// The wall-clock twin of [`Self::reach`], re-read the same way rather than latched:
	/// a group that lands between the two restarts the clock, and an aborted successor
	/// hands it to the next one. The clock is read only once there is a successor,
	/// so judging the newest group, the common case, costs no clock read.
	fn superseded(&self, sequence: u64, servable: &Servable, budget: Duration) -> bool {
		self.arrival(sequence, servable)
			.is_some_and(|successor| self.aged(successor.arrived, budget))
	}

	/// Whether a full `budget` passed since a successor `arrived`.
	fn aged(&self, arrived: crate::time::Instant, budget: Duration) -> bool {
		let now = self.cache.pool().now_wall();
		now.saturating_duration_since(arrived) >= budget
	}

	/// When a group was superseded, given `arrival`, its cached successor in `servable`: when
	/// `arrival` arrived, or when the successor `superseded` recorded did, unless a group landed
	/// between them since. The record outlives the successor's eviction: otherwise an evicted
	/// successor restarts the clock at whichever newer group the cache still holds, which a
	/// budget past the cache window never reaches. A recorded successor that failed hands the
	/// clock on, as a re-read does, whether or not the cache has reclaimed it yet.
	fn superseded_at(
		&self,
		arrival: Option<&Slot>,
		servable: &Servable,
		superseded: Option<&Superseded>,
	) -> Option<crate::time::Instant> {
		let Some(superseded) = superseded else {
			return arrival.map(|slot| slot.arrived);
		};
		let mut last = superseded.0.lock().unwrap_or_else(PoisonError::into_inner);
		if let Some(recorded) = &*last
			&& !recorded.fate.failed()
			&& servable.contains(recorded.sequence)
			&& arrival.is_none_or(|slot| slot.group.sequence > recorded.sequence)
		{
			return Some(recorded.arrived);
		}
		*last = arrival.map(|slot| Successor {
			sequence: slot.group.sequence,
			arrived: slot.arrived,
			fate: slot.group.fate(),
		});
		arrival.map(|slot| slot.arrived)
	}

	/// Whether the group at `sequence` has drifted further behind `edge` than `budget`
	/// tolerates, so a subscriber should skip it rather than hand it over.
	///
	/// Presentation time measures a group by how far it could still *reach*, not by how far
	/// behind it started. Being behind is survivable: priority transmits newer groups
	/// first, so a backlog bursts at whatever rate is left over and closes the gap faster
	/// than the live edge advances. What is not survivable is having nothing left worth
	/// delivering, so a group is abandoned only once everything it could still present
	/// falls outside the budget.
	///
	/// A group's reach is bounded by its immediate successor (see [`Self::reach`]): it
	/// cannot present past where the next group begins. The candidate itself needs no
	/// timestamp: an empty group is bounded by its stamped successor the same way.
	///
	/// The wall clock judges it too: a group is also stale once its successor arrived a
	/// full `budget` ago (see [`Self::superseded`]), so a peer whose timestamps stop
	/// advancing, an untimed track, or an empty successor that never presents cannot pin
	/// a superseded group. Media time still convicts first when a burst arrives at once.
	/// The newest group has no successor, so neither clock ever convicts it.
	///
	/// The bound is exclusive, so the comparison is `>=` rather than `>`: the freshest frame
	/// a group could still hold sits just *below* its reach, so an age equal to the budget
	/// already puts every frame in it strictly past the budget. That also makes a zero
	/// budget fall out for free instead of needing a special case.
	///
	/// The edge must sit strictly above the candidate. The live edge is never late
	/// against itself, and a group received from past its first frame carries a later
	/// frame's timestamp on a low sequence without being an edge at all.
	fn is_stale(&self, sequence: u64, edge: &Edge, budget: Duration) -> bool {
		self.lookup.contains_key(&sequence) && self.drifted(sequence, edge, budget)
	}

	/// [`Self::is_stale`] for group `sequence`, whether this track holds it or not: its
	/// successor here starts a full `budget` behind the edge, or arrived a full `budget` ago.
	fn drifted(&self, sequence: u64, edge: &Edge, budget: Duration) -> bool {
		if self.superseded(sequence, &edge.servable, budget) {
			return true;
		}
		self.live_above(sequence, edge)
			.is_some_and(|live| live.behind(self.reach(sequence, &edge.servable), budget))
	}

	/// The edge that could convict group `sequence`: above it, and still holding the
	/// servable incarnation it was resolved from. It was resolved under an earlier lock,
	/// and failing safe (delivering) is right, since the next poll resolves a fresh edge.
	fn live_above(&self, sequence: u64, edge: &Edge) -> Option<PresentationEdge> {
		edge.presentation
			.filter(|live| live.sequence > sequence && self.holds(live.sequence, live.stamp))
	}

	/// [`Self::drifted`] within `servable`, parking `waiter` on exactly what else can move the
	/// verdict: the successor's first frame or abort, the edge's abort, the edge crossing
	/// the media deadline, and the wall clock crossing the successor's arrival plus
	/// `budget`. Parking on every track change instead wakes each of N parked reads per append.
	///
	/// The caller parks on a group landing above `sequence` first, in the way that fits
	/// whether this track holds it. A reader holding the group passes its `superseded`, so
	/// the wall clock outlives the successor's eviction; see [`Self::superseded_at`].
	fn poll_drifted(
		&self,
		sequence: u64,
		servable: &Servable,
		budget: Duration,
		superseded: Option<&Superseded>,
		waiter: &kio::Waiter,
	) -> bool {
		// Unbounded until a group lands above. Registered before judging, so a racing abort
		// is observed or wakes us, and armed for whichever successor this pass selected, so
		// re-selecting re-arms it.
		let arrival = self.watch_abort(waiter, |state| state.arrival(sequence, servable));
		let Some(arrived) = self.superseded_at(arrival, servable, superseded) else {
			return false;
		};
		if let Some(deadline) = arrived.checked_add(budget) {
			self.cache.watch_wall(deadline, waiter);
		}
		if self.aged(arrived, budget) {
			return true;
		}
		let reach = loop {
			// An abort can change reach even after the successor is stamped. Unbounded
			// until a group shows above, or the successor presents its first frame.
			let Some(successor) = self.watch_abort(waiter, |state| state.successor(sequence, servable)) else {
				return false;
			};
			let successor = &successor.group;
			if let Some(reach) = successor.timestamp() {
				break reach;
			}
			if successor.poll_started(waiter).is_pending() || successor.timestamp().is_none() {
				return false;
			}
		};
		// Registered before the edge is resolved, so a write crossing the deadline is
		// either seen here or wakes us.
		self.cache.wakes().watch_deadline(reach, budget, waiter);
		loop {
			let edge = self.drift_edge(servable);
			// The edge's abort presents nothing and never touches the track, yet hands the
			// edge to a lower group that may already sit past the deadline. Watch it, and
			// re-resolve if it already landed. Terminates: an aborted group is never the
			// edge again, so each pass resolves a lower one.
			if let Some(live) = edge.presentation
				&& let Some(slot) = self.lookup.get(&live.sequence)
				&& slot.group.poll_aborted(waiter).is_ready()
			{
				continue;
			}
			return self.drifted(sequence, &edge, budget);
		}
	}

	/// Resolve a one-shot fetch from the track side: the cached group, or an [`Error`]
	/// once it can never be served. A missing group is a failure ([`Error::NotFound`]), not an
	/// end-of-stream. The handler side (a refusal, or no [`Requests`] at all) lives
	/// in the track's fills; [`Fetching`] polls both.
	fn poll_fetch_cached(&self, sequence: u64, frames: group::Bounds) -> Poll<Result<group::Consumer>> {
		// A group aborted between the lookup and the consume (an abandoned fetch cut
		// short) is a miss, not a hit that fails on its first read.
		if let Some(group) = self.covering_range(sequence, frames)
			&& let Some(consumer) = group.try_consume()
		{
			// A cache hit refreshes the group: it resets both its age (expiry keys
			// off the last access) and its standing against the pool-wide average,
			// so the eviction walk keeps it over never-read groups.
			group.cache_refresh();
			return Poll::Ready(Ok(consumer));
		}

		if let Some(err) = &self.abort {
			return Poll::Ready(Err(err.clone()));
		}

		// Past the final sequence: the group can never exist.
		if self.final_sequence().is_some_and(|fin| sequence >= fin) {
			return Poll::Ready(Err(Error::NotFound));
		}

		Poll::Pending
	}

	/// Expire groups idle past the pool's wall-clock LRU window, never the latest.
	///
	/// The window is the pool's [`expiry`](cache::Pool::expiry), not the track's
	/// [`max_age`](Info::max_age): retention is a media-timestamp promise, while this
	/// is the cache's own guard against unread content pinning RAM, shared by every
	/// track in the pool.
	///
	/// One bounded, rotating scan over the eviction order, which holds every cached
	/// group except the protected latest. The cursor persists in the cache account, so
	/// entries beyond one scan window can't be starved by fresh (recently read,
	/// fetched, or written) entries in front of them: every position is revisited
	/// within a few writes. Expiry throughput is therefore EVICT_SCAN groups per write; the
	/// byte budget reclaims the remainder under memory pressure, and the pool's sweep
	/// ([`Self::expiry_scan_drain`]) covers a track that stopped writing entirely.
	pub(super) fn evict_expired(&mut self) {
		let scan = self.expiry_scan();
		self.evict_expired_scan(scan);
	}

	/// Describe the next bounded expiry scan without mutating observable track state.
	pub(super) fn expiry_scan(&self) -> ExpiryScan {
		ExpiryScan {
			start: self.cache.next_expiry_scan(EVICT_SCAN),
			width: EVICT_SCAN,
			now: self.cache.pool().now(),
			max_ticks: self.cache.pool().expiry_ticks(),
			gc: false,
		}
	}

	/// Scan every cached candidate so undated accesses and old entries cannot hide
	/// behind fresh entries at the front of the eviction order.
	pub(super) fn expiry_scan_drain(&self) -> ExpiryScan {
		ExpiryScan {
			start: 0,
			width: self.evict.len(),
			now: self.cache.pool().now(),
			max_ticks: self.cache.pool().expiry_ticks(),
			gc: true,
		}
	}

	#[cfg(test)]
	pub(super) fn date_cache_accesses(&self, now: u64) {
		for slot in self.lookup.values() {
			slot.group.cache_accessed_tick(Some(now));
		}
	}

	/// Whether an expiry scan would change observable track state.
	///
	/// Mirrors [`Self::evict_expired_scan`]'s walk exactly, stop rule included: a memo
	/// that scanned further would report work the scan itself will not do.
	pub(super) fn expiry_mutation_due(&self, scan: ExpiryScan) -> bool {
		let len = self.evict.len();
		if len > 0 {
			let start = scan.start % len;
			let mut retained = 0;
			for step in 0..len.min(scan.width) {
				let (sequence, stamp) = self.evict[(start + step) % len];
				let Some(slot) = self.lookup.get(&sequence) else {
					continue;
				};
				if slot.stamp != stamp {
					continue;
				}
				if slot.is_spent() || (!self.protects(sequence) && slot.is_expired(&scan)) {
					return true;
				}
				retained += 1;
				if !scan.gc && retained >= EVICT_SCAN {
					break;
				}
			}
		}

		self.datagrams.front().is_some_and(|datagram| datagram.expired())
			|| self
				.arrival
				.front()
				.is_some_and(|(sequence, stamp)| !self.is_current(*sequence, *stamp))
			|| self
				.evict
				.front()
				.is_some_and(|(sequence, stamp)| !self.is_current(*sequence, *stamp))
			|| self.evict.len() > 2 * self.lookup.len() + EVICT_SLACK
	}

	/// Expire an ended track's idle groups and datagrams, whose closed channel refuses the
	/// write [`Self::evict_expired_scan`] takes: abort the groups in place, releasing their
	/// frames, retire expired cuts, and release the datagrams, leaving entries that every
	/// read path skips.
	pub(super) fn expire_closed(&self, scan: ExpiryScan) {
		for (sequence, stamp) in &self.evict {
			let Some(slot) = self.lookup.get(sequence) else {
				continue;
			};
			if slot.stamp != *stamp || (slot.is_aborted() && slot.cut().is_none()) || !slot.is_expired(&scan) {
				continue;
			}
			if slot.is_aborted() {
				slot.retired.store(true, Ordering::Relaxed);
			} else {
				slot.abort(Error::Old);
			}
		}
		// Released oldest first, so the expired ones stay a prefix that readers skip.
		for datagram in &self.datagrams {
			if !datagram.release_expired() {
				break;
			}
		}
	}

	/// Apply a scan previously selected by [`Self::expiry_scan`] or
	/// [`Self::expiry_scan_drain`].
	pub(super) fn evict_expired_scan(&mut self, scan: ExpiryScan) {
		let len = self.evict.len();
		if len > 0 {
			let start = scan.start % len;
			let mut retained = 0;
			for step in 0..len.min(scan.width) {
				let (sequence, stamp) = self.evict[(start + step) % len];
				let Some(slot) = self.lookup.get(&sequence) else {
					continue;
				};
				if slot.stamp != stamp {
					// A historical hint; the live entry is elsewhere in the queue.
					continue;
				}
				// Already aborted: the frames are gone, reclaim the slot so a
				// later fetch can serve the sequence again.
				if slot.is_spent() {
					self.reclaim_aborted(sequence);
					continue;
				}
				if self.protects(sequence) || !slot.is_expired(&scan) {
					// Writes keep their scan bounded. Cleanup visits the entire
					// queue to date pending accesses and find idle entries behind them.
					retained += 1;
					if !scan.gc && retained >= EVICT_SCAN {
						break;
					}
					continue;
				}
				// An expired cut goes like the group it was, still forwarded as a drop.
				if slot.is_aborted() {
					self.reclaim_aborted(sequence);
					continue;
				}
				// Take the group out of the cache and abort it, so any consumer
				// still reading surfaces `Error::Old` instead of blocking forever
				// on a frame that will never arrive.
				self.uncache(sequence).unwrap().abort(Error::Old);
			}
		}

		// A datagram is never the live edge a late joiner needs, so even the newest goes.
		while self.datagrams.front().is_some_and(|datagram| datagram.expired()) {
			self.drop_datagram();
		}

		// Trim dead leading arrival entries to advance the subscriber offset. An
		// entry is dead once its slot is gone or re-stamped by a newer incarnation.
		while let Some((sequence, stamp)) = self.arrival.front() {
			if self.lookup.get(sequence).is_some_and(|slot| slot.stamp == *stamp) {
				break;
			}
			self.arrival.pop_front();
			self.offset += 1;
		}

		// Drop dead leading eviction entries so scans stay over live candidates.
		while let Some((sequence, stamp)) = self.evict.front() {
			if self.lookup.get(sequence).is_some_and(|slot| slot.stamp == *stamp) {
				break;
			}
			self.evict.pop_front();
		}

		// Dead entries behind a live front can linger; rebuild once they clearly
		// outnumber the live slots.
		if self.evict.len() > 2 * self.lookup.len() + EVICT_SLACK {
			let lookup = &self.lookup;
			self.evict
				.retain(|(sequence, stamp)| lookup.get(sequence).is_some_and(|slot| slot.stamp == *stamp));
		}
	}

	/// Whether `(sequence, stamp)` names the currently cached incarnation.
	fn is_current(&self, sequence: u64, stamp: u32) -> bool {
		self.lookup.get(&sequence).is_some_and(|slot| slot.stamp == stamp)
	}

	/// Whether `sequence` is the live edge, which eviction and expiry never take
	/// while a producer remains.
	fn protects(&self, sequence: u64) -> bool {
		!self.closed && Some(sequence) == self.latest_group
	}

	/// No producer remains: stop protecting the live edge, so the pool's idle expiry
	/// reclaims every group once readers stop touching it, and a stale consumer can't
	/// pin the cache (and its frame buffers) forever.
	fn close_cache(&mut self) {
		if std::mem::replace(&mut self.closed, true) {
			return;
		}
		self.cache.end_largest();
		if let Some(latest) = self.latest_group
			&& let Some(slot) = self.lookup.get(&latest)
		{
			slot.cache_demote();
			self.evict.push_back((latest, slot.stamp));
		}
	}

	/// End the track abruptly with `err`: drop the open groups nobody will finish, keep the
	/// finished ones for readers still draining, and stop protecting the live edge.
	///
	/// Returns every unfinished copy, cached or not, to abort once the track lock is released,
	/// so a reader that already pulled one ends with `err` too and its writer fails.
	fn tear_down(&mut self, err: Error) -> OpenGroups {
		// Snapshot the frame boundary before the open group it may sit in goes away: an
		// abrupt end is exactly when a replacement route asks where to resume.
		self.resume = self.resume_position();
		let groups = self
			.lookup
			.values()
			.flat_map(Slot::copies)
			.filter(|group| !group.is_settled())
			.cloned()
			.chain(self.detached.drain(..).filter_map(|copy| copy.writing()))
			.collect();
		let dropped = StreamError::from(&err);
		// A slot survives only on a copy holding the whole group. A finished bounded copy
		// serves no group reader, so it leaves the cache like an open one; a reader already
		// holding it keeps it, since its writer settled and it is not aborted above.
		let open: Vec<_> = self
			.lookup
			.iter()
			.filter(|(_, slot)| slot.visible && !slot.copies().any(group::Handle::is_finished))
			.filter_map(|(&sequence, _)| Some(sequence..sequence.checked_add(1)?))
			.collect();
		for groups in open {
			self.drop_groups(groups, dropped.clone());
		}
		self.lookup
			.retain(|_, slot| slot.copies().any(group::Handle::is_finished));
		let lookup = &self.lookup;
		self.bounds
			.get_mut()
			.unwrap_or_else(PoisonError::into_inner)
			.retain(|sequence, _| lookup.contains_key(sequence));
		self.starts
			.get_mut()
			.unwrap_or_else(PoisonError::into_inner)
			.retain(|sequence, _| lookup.contains_key(sequence));
		self.close_cache();
		OpenGroups { groups, err }
	}

	/// Take `sequence` out of the cache, along with its checked start.
	fn remove_slot(&mut self, sequence: u64) -> Option<Slot> {
		let slot = self.lookup.remove(&sequence)?;
		self.bounds
			.get_mut()
			.unwrap_or_else(PoisonError::into_inner)
			.remove(&sequence);
		self.starts
			.get_mut()
			.unwrap_or_else(PoisonError::into_inner)
			.remove(&sequence);
		Some(slot)
	}

	/// Record that the live feed delivered `sequence`, then fold the gaps that aged out.
	fn record_arrival(&mut self, sequence: u64, now: crate::time::Instant) {
		self.arrivals.arrive(sequence, now);
		// A feed still delivering keeps the gap above its last arrival open.
		if self.tail.is_some() {
			self.tail = Some(now);
		}
		self.fold_arrivals(now);
	}

	/// Fold every gap in the arrivals a subscription's grace stopped waiting for: the grace
	/// the session's tail uses, from the max delay the track's subscribers ask for.
	fn fold_arrivals(&mut self, now: crate::time::Instant) {
		// Only a gap folds, so an in-order feed never reads the subscriptions.
		if !self.arrivals.has_gaps() {
			return;
		}
		self.arrivals.set_grace(self.arrival_grace());
		self.arrivals.expire(now);
	}

	/// How long a missing live group is waited for, from the max delay the track's
	/// subscribers ask for.
	fn arrival_grace(&self) -> Duration {
		let max_delay = clamp_max_delay(self.subscriptions.lock().max_delay(), self.max_age_bound());
		ledger::grace(max_delay)
	}

	/// Attach the publisher's immutable metadata without replacing it with local cache policy.
	fn install(&mut self, info: Info) {
		// A replaced max age moves every parked read's budget.
		if self.info.replace(info).is_some() {
			self.cache.wakes().wake_all();
		}
	}

	/// Create the shared state for a track under `broadcast`, along with the cache
	/// account it and its groups charge into.
	///
	/// The account holds a [`kio::Weak`] back to this state: a group must be able to
	/// settle the track's eviction debt as it writes, but the track owns its cached
	/// groups, so anything stronger would make the pair immortal.
	fn spawn(broadcast: Arc<broadcast::Info>) -> kio::Producer<Self> {
		let state = kio::Producer::new(Self {
			broadcast: broadcast.clone(),
			..Default::default()
		});
		let cache = cache::Track::new(broadcast.pool.clone(), broadcast.frames.clone(), state.downgrade());
		state.write().ok().expect("a new track is open").cache = cache;
		state
	}

	/// Record a written sequence. It raises the largest too, so a later group below it never
	/// changes it, even one below a reached end.
	///
	/// `answers` when it is a group subscribers receive, which answers a pending refresh.
	///
	/// The demand wakes it owes wait in [`Self::deferred`] for the caller to fire once its guard
	/// is released.
	fn advance(&mut self, sequence: u64, answers: bool) {
		self.max_sequence = Some(self.max_sequence.map_or(sequence, |max| max.max(sequence)));
		self.cache.advance_largest(Position::group(sequence));
		let wakes = self.subscriptions.lock().wrote(sequence, answers);
		if !wakes.is_empty() {
			self.deferred.push(wakes);
		}
	}

	/// Take the wakes [`Self::advance`] deferred, to fire once the state guard is released.
	pub(super) fn deferred(&mut self) -> Deferred {
		Deferred(std::mem::take(&mut self.deferred))
	}

	/// The sequence the next append takes: one past this track's own writes.
	fn next_sequence(&self) -> Result<u64> {
		Ok(match self.max_sequence {
			Some(max) => max.checked_add(1).ok_or(coding::BoundsExceeded)?,
			None => 0,
		})
	}

	/// Reject a sequence that is still cached; a dead (aborted or evicted)
	/// incarnation is removed so a fresh group can serve the sequence again. Subscribers may
	/// have seen its start, so it stays as the eviction watermark.
	///
	/// Best effort: nothing remembers a sequence whose slot is already gone, so a
	/// publisher re-sending a long-evicted sequence is accepted as new.
	/// A group that starts above `frame_start` is also replaceable: it cannot answer a
	/// request from there, so the wider producer takes the slot. Readers already
	/// draining the old one keep their own handle.
	fn claim_sequence(&mut self, sequence: u64, frame_start: u64) -> Result<()> {
		// Either copy that can still answer from `frame_start` takes the sequence.
		if self.covering_group(sequence, frame_start).is_some() {
			return Err(Error::Duplicate);
		}
		self.displace(sequence);
		Ok(())
	}

	/// Insert a freshly-created group into the cache.
	///
	/// Updates the live edge, demoting the previous latest into the eviction order;
	/// the current latest is never enqueued, which is what protects it from
	/// eviction. `visible` controls arrival-order delivery: publisher-produced
	/// groups reach subscribers, fetched backfill is served by sequence only. A group
	/// counts as content only once [`Self::reveal`] reports its first frame or finish.
	fn insert_group(&mut self, group: &group::Handle, visible: bool) {
		self.latch_settled();
		let sequence = group.sequence;
		if visible {
			self.contents.remove(sequence);
		}
		group.register_content(&self.contents);
		self.next_stamp = self.next_stamp.wrapping_add(1);
		let stamp = self.next_stamp;

		// The live edge is tracked separately from `max_sequence`, which datagrams
		// share and can push past any cached group: demotion must still fire when
		// the next group lands beyond a datagram-advanced counter.
		if self.latest_group.is_none_or(|latest| sequence >= latest) {
			// Demote the previous latest: it joins the eviction order (and the
			// pool's access average) like any other cached group.
			if let Some(latest) = self.latest_group
				&& sequence > latest
				&& let Some(prev) = self.lookup.get(&latest)
			{
				prev.cache_demote();
				self.evict.push_back((latest, prev.stamp));
			}
			self.latest_group = Some(sequence);
		}
		if visible {
			if let Feed::Live { first, .. } = &mut self.feed {
				first.get_or_insert(sequence);
			}
			self.newest_produced = self.newest_produced.max(Some(sequence));
		}
		if !self.protects(sequence) {
			group.cache_demote();
			self.evict.push_back((sequence, stamp));
		}

		self.advance(sequence, visible);
		// A publisher may re-create a group it cut; the sequence is no longer given up.
		if visible
			&& self.drops.contains_key(&sequence)
			&& let Some(end) = sequence.checked_add(1)
		{
			self.change_drops(sequence..end, None);
		}
		let now = self.cache.pool().now_wall();
		if visible {
			self.record_arrival(sequence, now);
		}
		let replaced = self.lookup.insert(
			sequence,
			Slot {
				group: group.clone(),
				stamp,
				visible,
				arrived: now,
				head: None,
				retired: AtomicBool::new(false),
			},
		);
		if replaced.is_some() {
			self.bounds
				.get_mut()
				.unwrap_or_else(PoisonError::into_inner)
				.remove(&sequence);
		}
		// A group inserted with its first frame already written started before it was here.
		self.index_start(sequence);
		self.cache.wakes().arrived(sequence..=sequence);
		if visible {
			// Readers and group demand reach it at once. Its arrival starts the wall clock of
			// the reads it succeeds, but it counts as content only once it starts.
			self.arrival.push_back((sequence, stamp));
			// Publishes the landing to reads judging expiry without the track lock, then
			// checks for one parked. Reveal only holds a read lock and cannot use the shortcut.
			if self.cache.wakes().land(sequence) {
				self.wake_landed(sequence);
			}
		}
	}

	/// Count `group` as content now that its first frame opened or it finished, if it
	/// still holds its slot: wake the reads it succeeds. Returns whether an answer waits on
	/// it, which the caller shows with [`Self::shown`] under the write lock.
	///
	/// Takes the read lock so a group starting wakes only the reads it can move, not every
	/// reader parked on the track.
	pub(super) fn reveal(&self, group: &group::Handle) -> bool {
		let sequence = group.sequence;
		match self.lookup.get(&sequence) {
			Some(slot) if slot.visible && slot.group.is_clone(group) && !slot.group.is_aborted() => {}
			_ => return false,
		}
		self.wake_landed(sequence);
		matches!(self.feed, Feed::Answered { from } if sequence >= from)
	}

	/// Wake the reads a group landing at `sequence` succeeds.
	fn wake_landed(&self, sequence: u64) {
		let below = self
			.lookup
			.range(..sequence)
			.rev()
			.find(|(_, slot)| slot.is_shown())
			.map(|(below, _)| *below);
		let oldest = self.lookup.first_key_value().map_or(sequence, |(oldest, _)| *oldest);
		self.cache.wakes().landed(below, sequence, oldest);
	}

	/// Content at `sequence` became readable: an answer waiting on it now shows.
	pub(super) fn shown(&mut self, sequence: u64) {
		if let Feed::Answered { from } = self.feed
			&& sequence >= from
		{
			self.go_live(Some(from));
		}
	}

	/// Admit a freshly-created group: settle eviction debt first (so the newcomer
	/// can never be a victim of the very write that created it), insert it, then
	/// expire idle groups.
	fn commit_group(&mut self, group: &group::Handle) {
		self.charge_debt();
		self.insert_group(group, true);
		self.evict_expired();
	}

	/// Accrue and pay eviction debt for everything written since the last charge:
	/// this track's account, which the groups' charges feed on every frame (so
	/// growth on already-demoted groups and backfill is billed too).
	///
	/// Runs BEFORE the new group is inserted, so a brand-new entry is never a
	/// victim of the very write that created it. A track whose oldest content is
	/// staler than the pool-wide average access time accrues at double rate, so
	/// stale-heavy tracks drain first.
	///
	/// Also runs from the frame-write path via [`cache::Track::settle`], which is
	/// why it's reachable from the account, so a track that only appends frames to
	/// open groups still pays.
	pub(super) fn charge_debt(&mut self) {
		let written = self.cache.take_written();
		let pool = self.cache.pool().clone();
		// Held heads go first: they are a failed copy's frames that no track can evict.
		pool.reclaim_held();
		match pool.accrue(written) {
			Some(mut accrued) => {
				if self.oldest_is_stale(&pool) {
					accrued = accrued.saturating_mul(2);
				}
				// `used` bounds what eviction could ever free, keeping a track that
				// can't pay (everything protected) from hoarding a stale schedule.
				self.debt = self.debt.saturating_add(accrued).min(pool.used());
				// Cap each payment at twice what was written so one write never dumps
				// a deep backlog at once; the remainder carries to the next write.
				let cap = written.saturating_mul(2);
				let paid = self.pay_debt(&pool, cap);
				self.pay_datagrams(cap.saturating_sub(paid));
			}
			// Under capacity there is nothing to work off, and stale debt would
			// cause a spurious eviction burst at the next pressure spike.
			None => self.debt = 0,
		}
	}

	/// Whether this track's oldest evictable group was accessed at or before the
	/// pool-wide average, doubling the debt it accrues. A dead entry at the front
	/// just reads as not-stale until the next payment or expiry cleans it up.
	fn oldest_is_stale(&self, pool: &cache::Pool) -> bool {
		let Some(average) = pool.average() else {
			return false;
		};
		let Some((sequence, stamp)) = self.evict.front() else {
			return false;
		};
		let Some(slot) = self.lookup.get(sequence) else {
			return false;
		};
		slot.stamp == *stamp && !slot.is_aborted() && slot.cache_accessed() <= average
	}

	/// Abort this track's stalest groups until the outstanding debt is paid, or
	/// `cap` bytes have been freed by this call.
	///
	/// Deliberately approximate, Redis-style: at most a handful of live candidates
	/// are examined per call, from the front of the eviction order. A group
	/// accessed more recently than the pool-wide average is protected and rotates
	/// to the back, so fresh content in this track never dies while staler content
	/// survives elsewhere; the unfreed bytes keep the pool over budget, shifting
	/// the debt onto the tracks holding that staler content. When the next victim
	/// is larger than the remaining debt it is left in place and the debt carries
	/// over, so a small write never evicts a huge group (once the debt does cover
	/// it, that one victim may overshoot `cap`). Returns the bytes it freed.
	fn pay_debt(&mut self, pool: &cache::Pool, cap: u64) -> u64 {
		let average = pool.average().unwrap_or(0);
		let mut paid = 0u64;
		let mut scanned = 0usize;
		let mut fresh = Vec::new();
		for _ in 0..self.evict.len() {
			if self.debt == 0 || paid >= cap || scanned >= EVICT_SCAN {
				break;
			}
			let Some((sequence, stamp)) = self.evict.pop_front() else {
				break;
			};
			let Some(slot) = self.lookup.get(&sequence) else {
				// Evicted or expired; discard the dead entry.
				continue;
			};
			if slot.stamp != stamp {
				// A historical hint; the live entry is elsewhere in the queue.
				continue;
			}
			if slot.is_aborted() {
				// Aborted upstream: the frames are already gone, reclaim the slot.
				self.reclaim_aborted(sequence);
				continue;
			}
			if self.protects(sequence) {
				// The live edge is never enqueued, but tolerate finding it anyway.
				self.evict.push_back((sequence, stamp));
				continue;
			}

			scanned += 1;
			// Protected: accessed more recently than the average (a fresh insert,
			// an active reader, or a FETCH hit, which also covers a backfill still
			// being filled). Rotate to the back, but shed its front below if the debt
			// outlives the scan.
			if slot.cache_accessed() > average {
				self.evict.push_back((sequence, stamp));
				fresh.push(sequence);
				continue;
			}
			// The full footprint including overhead, so even empty groups repay
			// their share of the budget when evicted.
			let size = slot.cache_size();
			if size > self.debt {
				self.evict.push_front((sequence, stamp));
				break;
			}

			self.debt -= size;
			paid = paid.saturating_add(size);
			self.uncache(sequence).unwrap().abort(Error::Evicted);
		}
		// No cold group left to pay with: shed the front of the groups that could not go
		// whole, the ones being read and then the live edge, so neither grows without bound.
		fresh.extend(self.latest_group.filter(|latest| self.protects(*latest)));
		for sequence in fresh {
			if self.debt == 0 || paid >= cap {
				break;
			}
			let Some(slot) = self.lookup.get(&sequence) else {
				continue;
			};
			for group in slot.copies() {
				let freed = group.shed(self.debt, cap - paid.min(cap));
				self.debt -= freed.min(self.debt);
				paid = paid.saturating_add(freed);
			}
		}
		paid
	}

	/// Drop this track's oldest buffered datagrams toward the debt its groups left, freeing
	/// at most `cap` bytes. Like a group, one larger than the remaining debt stays.
	fn pay_datagrams(&mut self, cap: u64) {
		let mut paid = 0u64;
		while let Some(datagram) = self.datagrams.front()
			&& paid < cap
			&& datagram.bytes() <= self.debt
		{
			self.debt -= datagram.bytes();
			paid = paid.saturating_add(datagram.bytes());
			self.drop_datagram();
		}
	}

	/// Group `sequence` once cached, `None` once the feed starts past it or ends before it,
	/// and pending while it still may land.
	fn landing(&self, sequence: u64) -> Poll<Option<group::Consumer>> {
		if let Some(slot) = self.lookup.get(&sequence)
			&& slot.serves_end()
		{
			return Poll::Ready(Some(slot.serving().consume()));
		}
		match self.start_sequence.is_some_and(|start| start > sequence)
			|| self.final_sequence().is_some_and(|fin| fin <= sequence)
		{
			true => Poll::Ready(None),
			false => Poll::Pending,
		}
	}

	/// A live group, or a verdict from its arrival record, independent of stream order:
	/// the error it was given up with, or `None` once it is gone or never coming.
	fn poll_arrival(
		&self,
		sequence: u64,
		now: crate::time::Instant,
		grace: Duration,
		waiter: &kio::Waiter,
	) -> Poll<Result<Option<group::Consumer>>> {
		if let Poll::Ready(Some(group)) = self.landing(sequence) {
			return Poll::Ready(Ok(Some(group)));
		}
		if let Some(err) = self.given_up(sequence) {
			return Poll::Ready(Err(err));
		}
		if self.landing(sequence).is_ready()
			|| sequence
				.checked_add(1)
				.is_some_and(|end| self.arrivals.covers(sequence..end))
			|| self.arrivals.folds_at(sequence, grace).is_some_and(|at| at <= now)
		{
			return Poll::Ready(Ok(None));
		}
		self.cache.wakes().watch_arrival(sequence, waiter);
		Poll::Pending
	}

	/// Whether a group in `groups` may still land from the live feed, by the rule
	/// [`Self::poll_arrival`] applies: not accounted for, not dropped, within the feed, and
	/// in a gap a later arrival opened less than `grace` ago, or that nothing later opened.
	/// Below the first arrival only when the feed was asked to start lower, as `floor` says.
	/// With `delivered`, only a group it lacks counts.
	fn may_land(&self, groups: ops::Range<u64>, floor: bool, grace: Duration, delivered: Option<&Delivered>) -> bool {
		let start = match floor {
			true => groups.start,
			false => match self.arrivals.first() {
				Some(first) => groups.start.max(first),
				None => return false,
			},
		};
		let start = start.max(self.start_sequence.unwrap_or(0));
		let end = groups.end.min(self.final_sequence().unwrap_or(u64::MAX));
		let now = self.cache.pool().now_wall();
		self.arrivals.gaps(start..end).any(|(gap, opened)| {
			opened
				.and_then(|opened| opened.checked_add(grace))
				.is_none_or(|folds| now < folds)
				&& self
					.drops
					.gaps(&gap)
					.any(|open| delivered.is_none_or(|delivered| !delivered.covers(open)))
		})
	}

	/// Record the declared first sequence of the live feed, replacing any earlier
	/// declaration: the signal is scoped to the current subscription's demand,
	/// which may legitimately move in either direction. `None` clears it (the
	/// demand dropped to the live edge, whose floor is unknown until declared).
	///
	/// A declared start asks the feed for every group from it, so a gap in the arrivals at
	/// or above it, and a pending tail above the last arrival, is waited for a fresh grace:
	/// the feed may only now be sending it.
	fn set_start(&mut self, start_sequence: Option<u64>, pending: bool) {
		self.latch_settled();
		self.start_sequence = start_sequence;
		self.start_pending = pending;
		self.start_budget = None;
		if let Some(start) = start_sequence {
			self.cache.wakes().arrived(..start);
			let now = self.cache.pool().now_wall();
			self.arrivals.demand(start..u64::MAX, now);
			if self.tail.is_some() {
				self.tail = Some(now);
			}
		}
	}

	/// Record the exclusive end, rejecting a second end, one that would orphan
	/// already-produced groups, or one inside a group that already holds a frame past it.
	/// A mid-group end caps that group, so no later write can land past it either.
	fn set_end(&mut self, end: Position) -> Result<()> {
		if self.end.is_some() {
			return Err(Error::Closed);
		}
		let fin = end.final_sequence().ok_or(coding::BoundsExceeded)?;
		if let Some(max) = self.max_sequence
			&& fin <= max
		{
			return Err(Error::ProtocolViolation);
		}
		if end.frame != 0
			&& let Some(slot) = self.lookup.get(&end.group)
		{
			for (capped, group) in slot.copies().enumerate() {
				if let Err(err) = group.limit(Some(end.frame)) {
					// A refused end declares nothing, so it caps nothing either.
					for group in slot.copies().take(capped) {
						let _ = group.limit(None);
					}
					return Err(err);
				}
			}
		}
		self.end = Some(end);
		self.cache.wakes().arrived(fin..);
		Ok(())
	}

	/// The first group the end excludes whole: one past the last group that may exist.
	fn final_sequence(&self) -> Option<u64> {
		self.end.and_then(Position::final_sequence)
	}

	/// Cap a new group the end cuts, so it never holds a frame past the end.
	fn cap(&self, group: &group::Handle) -> Result<()> {
		match self.end {
			Some(end) if end.frame != 0 && end.group == group.sequence => group.limit(Some(end.frame)),
			_ => Ok(()),
		}
	}

	/// Whether the track has reached its end: the final boundary is set and the live
	/// edge has caught up to it, so no further group can arrive. A future boundary
	/// (declared via [`Producer::finish_at`] ahead of the live edge) stays incomplete
	/// until the remaining groups are produced, the last producer drops without them, or,
	/// on a received copy, the grace for them passes. Drives the end-of-stream signal from
	/// the read methods (`recv_group` / `next_group` / `read_frame` return `None`).
	///
	/// An abort before the end settled wins over it: a group below the boundary was
	/// still open, so the track was cut off rather than ended.
	fn is_complete(&self) -> bool {
		// `sealed` is a declared end the last producer dropped short of. An abort still wins
		// unless that end had already settled: a group below it was still open.
		let reached = self.sealed
			|| self
				.final_sequence()
				.is_some_and(|fin| self.reaches(fin) && !self.awaits_tail(fin));
		reached && (self.abort.is_none() || self.settled.load(Ordering::Relaxed))
	}

	/// Whether the live edge reached `fin`, or a received copy's tail decides it instead:
	/// what it still misses up to `fin` is waited for only as long as the grace.
	fn reaches(&self, fin: u64) -> bool {
		self.edge() >= fin || self.tail.is_some()
	}

	/// Whether the largest can no longer advance while a producer is still held: the track
	/// reached its declared end, and the largest group's content is final. Every sequence below
	/// the end raised it already, and a group name fixes its content, so no legal write moves
	/// it after this: not a late group below it, nor a recreated or fetched copy of it.
	fn largest_settled(&self) -> bool {
		self.is_complete() && !self.cache.largest_open()
	}

	/// One past the highest sequence produced: where the live edge stands against the end.
	fn edge(&self) -> u64 {
		self.max_sequence.map_or(0, |max| max.saturating_add(1))
	}

	/// Whether a group below `fin` the wire subscription still owes has yet to arrive.
	/// Only such a hole holds readers, so a tail with nothing missing ends them at once.
	fn awaits_tail(&self, fin: u64) -> bool {
		// Nothing arrives after an abort, so a hole left then is final.
		self.abort.is_none() && self.owes(fin)
	}

	/// Whether the tail misses a sequence from where the feed starts up to `fin` that is
	/// still worth waiting for; see [`Self::tail_deadline`].
	fn owes(&self, fin: u64) -> bool {
		self.tail_deadline(fin)
			.is_some_and(|at| at.is_none_or(|at| self.cache.pool().now_wall() < at))
	}

	/// When a received copy's declared end stops waiting for its tail: the latest instant a
	/// sequence below `fin` it still misses is given up, or `Some(None)` when one is never given
	/// up, as under an unbounded max delay, whose grace runs past the clock's range. `None`
	/// when nothing is missing.
	///
	/// A missing sequence is one no live group or datagram delivered and the publisher did
	/// not drop. A gap below an arrival is given up a grace after that arrival opened it, as
	/// the session's tail does. The gap above the last arrival is given up a grace after
	/// the end or a start was declared, or a group last arrived, so a feed still delivering toward an
	/// end declared ahead of it is not cut off. Judged by the arrivals, not the cache, so an evicted group or a
	/// datagram counts as delivered, and the check costs the same however much is cached.
	fn tail_deadline(&self, fin: u64) -> Option<Option<crate::time::Instant>> {
		let active = self.tail?;
		let grace = self.arrival_grace();
		// Fetched backfill never reaches a reader in arrival order, so only the live feed's
		// arrivals count. Without a declared start, the first one stands in for it.
		let floor = match self.start_sequence {
			Some(start) => start,
			// Where the feed starts is still unknown, so everything below the end not yet
			// delivered or dropped is owed.
			None if self.start_pending => {
				let missing = self
					.arrivals
					.gaps(0..fin)
					.any(|(gap, _)| self.drops.gaps(&gap).next().is_some());
				return missing.then(|| active.checked_add(grace));
			}
			None => self.arrivals.first().unwrap_or(0),
		};
		// Nothing opened the gap above the last arrival, so its grace runs from `active`.
		self.arrivals
			.gaps(floor.min(fin)..fin)
			.filter(|(gap, _)| self.drops.gaps(gap).next().is_some())
			.map(|(_, opened)| opened.unwrap_or(active).checked_add(grace))
			// A gap that is never given up holds the end for good.
			.reduce(|a, b| a.zip(b).map(|(a, b)| a.max(b)))
	}

	/// Wake `waiter` when a received copy's declared end stops waiting for its tail.
	fn watch_tail(&self, waiter: &kio::Waiter) {
		if let Some(Some(at)) = self.final_sequence().and_then(|fin| self.tail_deadline(fin))
			&& self.cache.pool().now_wall() < at
		{
			self.cache.watch_wall(at, waiter);
		}
	}

	/// Whether the declared end is reached, the pending tail owes nothing below it, and
	/// every cached slot below it settled ([`Slot::is_settled`]), so nothing the end
	/// promised is still in flight.
	fn is_settled(&self) -> bool {
		let Some(fin) = self.final_sequence() else {
			return false;
		};
		let reached = self.sealed || self.reaches(fin);
		reached && !self.owes(fin) && self.lookup.range(..fin).all(|(_, slot)| slot.is_settled())
	}

	/// Record that the end settled before a write that could reopen it: an abort, a late
	/// group below the end, or a fresh grace for the tail. The clean outcome `closed()` saw
	/// then stands, and a later abort ends readers cleanly. A late group or a wider
	/// subscription is still served; it just cannot turn the outcome into an error.
	fn latch_settled(&mut self) {
		if self.abort.is_none() && self.is_settled() {
			self.settled.store(true, Ordering::Relaxed);
		}
	}

	/// Where a replacement route should pick this track up: one past the last frame
	/// produced, rolling to the start of the next group once the latest group is
	/// complete (nothing more can be appended to it).
	///
	/// `None` while the track has produced nothing, which is an unbounded takeover.
	fn resume_position(&self) -> Option<Position> {
		// A snapshot taken when the open groups were released wins; the group it was
		// derived from may be gone.
		if self.resume.is_some() {
			return self.resume;
		}

		let max = self.latest_group?;
		match self.lookup.get(&max).and_then(|slot| slot.group.resume_frame()) {
			// Still open *and* carrying frames, so the replacement continues it
			// frame-by-frame.
			Some(frame) => Some(Position {
				group: max,
				frame: frame as u64,
			}),
			// A copy that wrote nothing has no frames to continue, and the reader
			// already holds its (empty) handle. Pointing a replacement at frame 0 would
			// hand the same sequence out twice, so roll to the next group exactly as a
			// finished one does.
			None => Some(Position::group(max.saturating_add(1))),
		}
	}

	/// The upstream subscription ended with the copy still held: what it cached may go
	/// stale, so readers get nothing from it until the route answers again. Buffered
	/// datagrams go too, since a reader returning later must not be handed them.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	fn set_idle(&mut self) {
		let newest = self
			.lookup
			.iter()
			.rev()
			.find(|(_, slot)| slot.is_shown())
			.map(|(sequence, _)| *sequence);
		self.feed = Feed::Idle { newest };
		self.datagram_offset += self.datagrams.len();
		self.datagrams.clear();
		// An open group stops because this copy dropped its feed, not because the publisher
		// gave it up: its stream resets as a cut would, so it leaves the cache first, and the
		// next feed sends it again.
		let open: Vec<u64> = self
			.lookup
			.iter()
			.filter(|(_, slot)| slot.visible && !slot.is_aborted() && !slot.copies().any(group::Handle::is_finished))
			.map(|(&sequence, _)| sequence)
			.collect();
		for sequence in open {
			self.displace(sequence);
		}
	}

	/// The route answered with its largest position (`None` for nothing yet): the cache
	/// is current up to there. `start` is the first group the feed serves, when the answer
	/// names it; otherwise the largest stands in. A feed that starts past a gap after
	/// everything cached leaves the cache unjudgeable, since nothing bounds how far an old
	/// group reached, so readers skip it; whatever the feed delivers stays.
	///
	/// Readers wait until the cache shows that position: before then its newest group would
	/// read as the live edge, though the route already holds a newer one.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	fn set_live(&mut self, largest: Option<Position>, start: Option<u64>) {
		let Feed::Idle { newest } = self.feed else {
			return;
		};
		// Judged by what survived: the cancel that idled the copy resets the group in flight,
		// and an answer past it leaves a gap after the newest group still cached.
		let cached = newest.and_then(|newest| {
			self.lookup
				.range(..=newest)
				.rev()
				.find(|(_, slot)| slot.is_shown())
				.map(|(sequence, _)| *sequence)
		});
		// A feed asked from the cache's head resends what follows it, newest first on a
		// descending track, so its largest alone would read as a gap.
		let start = start.or(largest.map(|largest| largest.group));
		self.live_floor = match (largest, cached) {
			(Some(_), Some(cached)) if start.is_some_and(|start| start > cached.saturating_add(1)) => Some(cached + 1),
			// The route has nothing, so whatever is cached is not its feed.
			(None, Some(cached)) => Some(cached.saturating_add(1)),
			_ => self.live_floor,
		};
		match largest.map(|largest| largest.group) {
			Some(from) if !self.shows(from) => self.feed = Feed::Answered { from },
			edge => self.go_live(edge),
		}
	}

	/// Whether readers can already see content at or past `sequence`.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	fn shows(&self, sequence: u64) -> bool {
		self.lookup.range(sequence..).any(|(_, slot)| slot.is_shown())
			|| self.datagrams.iter().any(|datagram| datagram.sequence >= sequence)
	}

	/// The newest live group the cache holds, and the number of frames it has so far.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	fn newest(&self) -> Option<(u64, u64)> {
		let (sequence, slot) = self
			.lookup
			.iter()
			.rev()
			.find(|(_, slot)| slot.visible && slot.group.is_started())?;
		Some((*sequence, slot.group.frame_count() as u64))
	}

	/// The largest position the cache holds: the newest live group's last frame (its head,
	/// while it has none).
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	fn largest(&self) -> Option<Position> {
		self.newest().map(|(group, frames)| Position {
			group,
			frame: frames.saturating_sub(1),
		})
	}

	fn poll_finished(&self) -> Poll<Result<Position>> {
		if let Some(end) = self.end {
			Poll::Ready(Ok(end))
		} else if let Some(err) = &self.abort {
			Poll::Ready(Err(err.clone()))
		} else {
			Poll::Pending
		}
	}

	fn modify(producer: &kio::Producer<Self>) -> Result<kio::Mut<'_, Self>> {
		producer.write().map_err(|r| r.abort.clone().unwrap_or(Error::Dropped))
	}

	/// Observe a shared track's abort, or every producer leaving, without extending its
	/// publisher lifetime. A finish alone does not end it: lower groups may still arrive.
	// Sessions retain shared state while a FETCH owns its group writer.
	pub(crate) fn poll_aborted(producer: &kio::Producer<Self>, waiter: &kio::Waiter) -> Poll<Error> {
		producer
			.poll_closed(waiter)
			.map(|()| producer.read().abort.clone().unwrap_or(Error::Dropped))
	}

	/// The track's outcome once final: `Ok` once its declared end is reached with every
	/// cached group below it finished, which a later abort keeps, or the abort error.
	///
	/// A clean outcome latches once reported: a received copy's tail grace is judged when
	/// polled, and a later subscription asking for a longer one must not reopen it.
	fn poll_outcome(&self, waiter: &kio::Waiter) -> Poll<Result<()>> {
		if self.settled.load(Ordering::Relaxed) {
			return Poll::Ready(Ok(()));
		}
		if self.is_complete() {
			// An abort after the end settled keeps the clean end; until then a group below it
			// may still abort, which would cut the track off, so wait for each slot to settle
			// ([`Slot::is_settled`]). One whose last serving copy aborts leaves the outcome to
			// the track's own abort or drop.
			if self.abort.is_none()
				&& let Some(fin) = self.final_sequence()
			{
				for (_, slot) in self.lookup.range(..fin).filter(|(_, slot)| !slot.is_settled()) {
					// Watch every copy, since a serving head that aborts hands the slot back to
					// the live group, then judge again so a copy closing meanwhile is seen.
					for group in slot.copies() {
						let _ = group.poll_closed(waiter);
					}
					if !slot.is_settled() {
						return Poll::Pending;
					}
				}
			}
			self.settled.store(true, Ordering::Relaxed);
			Poll::Ready(Ok(()))
		} else if let Some(err) = &self.abort {
			Poll::Ready(Err(err.clone()))
		} else {
			self.watch_tail(waiter);
			Poll::Pending
		}
	}

	/// [`Self::poll_outcome`] read through a handle: the channel closing short of an
	/// outcome (every producer gone without an end or abort) reads as [`Error::Dropped`].
	fn settle(poll: Poll<std::result::Result<Result<()>, kio::Ref<'_, Self>>>) -> Poll<Result<()>> {
		Poll::Ready(match ready!(poll) {
			Ok(outcome) => outcome,
			Err(closed) if closed.is_complete() => Ok(()),
			Err(closed) => Err(closed.abort.clone().unwrap_or(Error::Dropped)),
		})
	}

	/// Abort shared track state without extending its publisher lifetime.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn abort(producer: &kio::Producer<Self>, err: Error) -> Result<()> {
		commit_abort(Self::modify(producer)?, err).abort();
		Ok(())
	}

	fn content_for_copy(&self, sequence: u64) -> Result<Option<Arc<group::Content>>> {
		if let Some(content) = self.contents.get(sequence) {
			return Ok(Some(content));
		}
		if let Some(slot) = self.lookup.get(&sequence) {
			for copy in slot.copies() {
				if let Some(content) = copy.activate_content()? {
					return Ok(Some(content));
				}
			}
		}
		Ok(None)
	}

	/// Insert a group a [`Fill`] answered, setting the track's [`Info`]
	/// if it isn't accepted yet. The group's timescale comes from that info, so a
	/// fetch can serve an as-yet-unaccepted track (e.g. a relay with no live
	/// subscription). The group lands in the cache so a waiting
	/// [`Fetching`] resolves via [`Self::poll_fetch`].
	pub(super) fn insert_group_request(
		&mut self,
		sequence: u64,
		frames: group::Bounds,
		info: Option<Info>,
	) -> Result<group::Producer> {
		if let Some(err) = &self.abort {
			return Err(err.clone());
		}
		if let Some(fin) = self.final_sequence()
			&& sequence >= fin
		{
			return Err(Error::Closed);
		}

		// Adopt the supplied info only if the track hasn't been accepted yet. Groups
		// created here charge the same account as any other, so backfill written
		// before the track is accepted settles its debt like the rest.
		if self.info.is_none() {
			self.install(info.unwrap_or_default());
		}
		let info = self.info.clone().unwrap();

		if self.covering_range(sequence, frames).is_some() {
			return Err(Error::Duplicate);
		}
		let content = self.content_for_copy(sequence)?;

		// A copy cut short at the back never displaces a fetched one that reaches the end,
		// which whole-group readers still take. The fill hands it to its own fetch instead.
		let kept = frames.end.is_some()
			&& self.lookup.get(&sequence).is_some_and(|slot| match slot.is_live() {
				true => slot
					.head
					.as_deref()
					.is_some_and(|head| !head.is_aborted() && !head.back_cut()),
				false => slot.serves_end(),
			});
		if !kept && !self.lookup.get(&sequence).is_some_and(Slot::is_live) {
			self.displace(sequence);
		}

		let mut group = group::Producer::new(group::Info { sequence }, info, self.cache.clone());
		if let Some(content) = content {
			group.handle().share_content(content);
		}
		group.handle().register_content(&self.contents);
		// Start where the request did before the group is visible: a fetch looking it up
		// in between would otherwise see it begin at 0 and get a reader that later skips
		// the head it asked for.
		group.bound(frames)?;
		self.cap(&group.handle())?;
		// A backfill exists because someone is fetching it right now: stamp that
		// access so the eviction walk can't kill it before the fetch resolves.
		group.handle().cache_refresh();

		// Settled first, like `commit_group`, which may evict the live slot itself.
		self.charge_debt();
		let protected = self.protects(sequence);
		match self.lookup.get_mut(&sequence).filter(|slot| slot.is_live()) {
			_ if kept => self.detach(&group.handle()),
			Some(slot) => {
				// Joins the slot's standing: demoted already unless it is the live edge.
				if !protected {
					group.handle().cache_demote();
				}
				if let Some(head) = slot.head.replace(Box::new(group.handle())) {
					self.detach(&head);
				}
			}
			// Invisible to arrival-order subscribers: fetched on demand, not produced
			// live by the publisher.
			None => self.insert_group(&group.handle(), false),
		}
		self.evict_expired();
		Ok(group)
	}
}

/// Demand wakes taken out of the track state, fired once its guard is gone.
#[must_use = "fire the wakes once the guard is released"]
pub(super) struct Deferred(Vec<kio::WaiterList>);

impl Deferred {
	pub(super) fn wake(self) {
		for mut wakes in self.0 {
			wakes.wake();
		}
	}
}

/// Record `err` and close the track, the shared tail of every abort, returning the open
/// groups to abort once the guard is gone.
fn commit_abort(mut state: kio::Mut<'_, TrackState>, err: Error) -> OpenGroups {
	// Decided before the open groups go: the groups below the end are the evidence. A gap
	// past the grace was lost before the abort rather than cut off by it.
	let now = state.cache.pool().now_wall();
	state.fold_arrivals(now);
	state.latch_settled();
	state.abort = Some(err.clone());
	// Keep what finished for consumers still draining: they get it, then the abort (or
	// the clean end, when the end settled). Clearing here would abort finished groups a
	// slower reader has not pulled yet. The pool's idle expiry bounds how long they stay.
	let open = state.tear_down(err);
	state.close();
	open
}

/// The unfinished groups a teardown took out of the track, aborted once the track lock
/// is released.
#[must_use = "abort the open groups after releasing the track lock"]
struct OpenGroups {
	groups: Vec<group::Handle>,
	err: Error,
}

impl OpenGroups {
	/// End each group with the teardown's error: its readers end and its writer fails.
	fn abort(self) {
		for group in self.groups {
			let _ = group.abort(self.err.clone());
		}
	}
}

/// A reader's local group bounds: the floor, and the exclusive cap (`None` for none).
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Bounds {
	pub start: u64,
	pub end: Option<u64>,
}

impl Bounds {
	/// Whether group `sequence` is within these bounds.
	pub fn contains(&self, sequence: u64) -> bool {
		sequence >= self.start && super::subscription::before_end(sequence, self.end)
	}
}

/// Poll `cache` for a largest location within `bounds` newer than `seen`, recording it there.
///
/// The largest is what the source reported and what its feed delivered since, never what
/// the cache happens to hold, so `live` holds the stream until the track's feed is live: an
/// idle copy's largest is only what an ended feed left. Past the cap or below the floor it
/// reports nothing. `ended` polls the track for [`TrackState::largest_settled`], or its
/// channel closing without ending the cache (a dropped request), either of which ends the
/// stream.
fn poll_largest(
	cache: &cache::Track,
	seen: &mut Option<Position>,
	bounds: Bounds,
	waiter: &kio::Waiter,
	live: impl FnOnce(&kio::Waiter) -> Poll<()>,
	ended: impl FnOnce(&kio::Waiter) -> Poll<()>,
) -> Poll<Option<Position>> {
	ready!(live(waiter));
	if let Poll::Ready(next) = cache.poll_largest(seen, bounds, waiter) {
		return Poll::Ready(next);
	}
	ready!(ended(waiter));
	Poll::Ready(cache.take_largest(seen, bounds))
}

/// Ready once the track's feed is live, or nothing more can move its largest. A declared
/// end that is not reached yet does not count: an idle copy's largest is still only what
/// its ended feed left.
fn largest_live(state: &TrackState, waiter: &kio::Waiter) -> Poll<()> {
	match state.feed.is_live() || state.sealed || state.abort.is_some() || state.largest_settled() {
		true => Poll::Ready(()),
		false => {
			state.watch_tail(waiter);
			Poll::Pending
		}
	}
}

/// Ready once the track's largest can no longer advance; see [`TrackState::largest_settled`].
fn largest_ended(state: &TrackState, waiter: &kio::Waiter) -> Poll<()> {
	match state.largest_settled() {
		true => Poll::Ready(()),
		false => {
			state.watch_tail(waiter);
			Poll::Pending
		}
	}
}

/// A producer for a track, used to create new groups.
#[derive(Clone)]
pub struct Producer {
	name: Arc<str>,
	info: Info,
	// The parent broadcast's info, inherited from [`broadcast::Producer::create_track`].
	// Top link of the ownership chain; carried for identity and future inheritance.
	broadcast: Arc<broadcast::Info>,
	state: kio::Producer<TrackState>,
	prev_subscription: Option<Subscription>,
	// Shared with every clone and every `Requests`: its `Drop` is the teardown.
	alive: Arc<Alive>,
	// Ingress stats scope, inherited from a tagged [`broadcast::Producer`]. Bumped as
	// one subscription on tag and closed when the last producer clone drops. Empty
	// (no-op) for an untagged broadcast.
	stats: stats::Scope,
}

impl Producer {
	/// The immutable publisher priority committed when this track was accepted.
	#[cfg(test)]
	#[cfg(feature = "async")]
	pub(crate) fn publisher_priority(&self) -> i8 {
		self.info.priority
	}

	/// Build a producer for the given track metadata.
	///
	/// Crate-private: tracks are born from their broadcast via
	/// [`broadcast::Producer::create_track`] (or served on demand through a
	/// [`Request`]), which threads the broadcast's `Arc<broadcast::Info>` down. The
	/// track opens its cache account against that broadcast's origin pool, and every
	/// group it creates charges into it.
	pub(crate) fn new(
		broadcast: Arc<broadcast::Info>,
		name: impl Into<Arc<str>>,
		info: impl Into<Option<Info>>,
	) -> Self {
		let name = name.into();
		let info = info.into().unwrap_or_default();
		let state = TrackState::spawn(broadcast.clone());
		state.write().ok().expect("a new track is open").accept(info.clone());
		let alive = Alive::new(name.clone(), state.clone());
		alive.publish(None);
		Self {
			name,
			info,
			state,
			broadcast,
			prev_subscription: None,
			alive,
			stats: stats::Scope::default(),
		}
	}

	/// Attach the parent broadcast's ingress stats scope, counting this track as one
	/// ingress subscription (closed when the last producer clone drops). Called by a
	/// tagged [`broadcast::Producer`] when it creates the track.
	pub(super) fn with_stats(mut self, scope: stats::Scope) -> Self {
		self.alive.publish(Some(&scope));
		self.stats = scope;
		self
	}

	/// Keep the broadcast's listing of this track until it leaves the table.
	pub(super) fn listed(self, unlist: broadcast::Unlist) -> Self {
		self.alive.list(unlist);
		self
	}

	/// The track's name, unique within its broadcast.
	pub fn name(&self) -> &str {
		&self.name
	}

	/// The parent broadcast this track belongs to.
	pub fn broadcast(&self) -> &broadcast::Info {
		&self.broadcast
	}

	/// The properties this track was created with.
	pub fn info(&self) -> &Info {
		&self.info
	}

	/// Create a new group with the given sequence number.
	///
	/// Readers and group demand reach it at once, but the track counts it as content (its
	/// live edge, or the successor that ages the group before it) only once its first frame
	/// opens or it finishes.
	pub fn create_group(&self, group: group::Info) -> Result<group::Producer> {
		self.create_group_inner(group, None)
	}

	/// A received copy starts at its wire offset before any reader can see it.
	#[cfg(feature = "async")]
	pub(crate) fn create_group_at(&self, group: group::Info, start: u64) -> Result<group::Producer> {
		self.create_group_inner(group, Some(start))
	}

	fn create_group_inner(&self, info: group::Info, received: Option<u64>) -> Result<group::Producer> {
		let mut state = self.modify()?;
		if state.sealed || state.final_sequence().is_some_and(|fin| info.sequence >= fin) {
			return Err(Error::Closed);
		}
		let start = received.unwrap_or(0);
		let content = if received.is_some() {
			if state.covering_group(info.sequence, start).is_some() {
				return Err(Error::Duplicate);
			}
			state.content_for_copy(info.sequence)?
		} else {
			None
		};
		let track = state.info.clone().unwrap();
		state.claim_sequence(info.sequence, start)?;
		let mut group = group::Producer::new(info, track, state.cache.clone()).with_meter(self.stats.meter());
		if start != 0 {
			group.bound(group::Bounds { start, end: None })?;
		}
		if let Some(content) = content {
			group.handle().share_content(content);
		}
		state.cap(&group.handle())?;
		state.commit_group(&group.handle());
		let deferred = state.deferred();
		drop(state);
		deferred.wake();
		Ok(group)
	}

	/// Create a new group with the next sequence number: one past this producer's own writes.
	///
	/// The broadcast remembers nothing of an earlier producer of the name, so a replacement
	/// starts at 0. Since a name always means the same content, a replacement continues past
	/// its predecessor's groups with [`Self::create_group`].
	pub fn append_group(&self) -> Result<group::Producer> {
		let mut state = self.modify()?;
		let sequence = state.next_sequence()?;
		if let Some(fin) = state.final_sequence()
			&& sequence >= fin
		{
			return Err(Error::Closed);
		}

		let track = state.info.clone().unwrap();

		let group =
			group::Producer::new(group::Info { sequence }, track, state.cache.clone()).with_meter(self.stats.meter());
		state.cap(&group.handle())?;
		state.commit_group(&group.handle());
		let deferred = state.deferred();
		drop(state);
		deferred.wake();

		Ok(group)
	}

	/// Append a datagram with the next sequence number, returning the assigned sequence.
	///
	/// A datagram is delivered best-effort over a single QUIC datagram, parallel to the
	/// track's groups but drawing from the same sequence namespace (so interleaving with
	/// [`Self::append_group`] never reuses a number). There is no group fallback: each
	/// session drops (with a debug log) any datagram whose encoded body exceeds the
	/// transport's datagram size, and sessions that can't carry datagrams at all (moq-lite
	/// before 05, or stream-only transports like WebSocket) never deliver them. Keep payloads well under the 1200-byte minimum path MTU.
	/// A datagram is never cached or served by a fetch, so use a group for anything a late
	/// joiner needs. An origin publisher uses this; a relay preserving upstream numbering
	/// uses [`Self::insert_datagram`].
	///
	/// Pass `None` on an untimed track; a datagram that doesn't match its track is
	/// [`Error::TimestampMismatch`].
	pub fn append_datagram<B: crate::IntoBytes>(
		&mut self,
		timestamp: impl Into<Option<Timestamp>>,
		payload: B,
	) -> Result<u64> {
		let payload = payload.into_bytes();
		if payload.len() > super::datagram::MAX_DATAGRAM_PAYLOAD {
			return Err(Error::FrameTooLarge);
		}
		// Resolved before the state guard borrows `self`.
		let meter = self.stats.meter();
		let mut state = self.modify()?;
		// Normalize into the track's timescale, like frames (see `group::Producer::create_frame`).
		let timescale = state.info.as_ref().unwrap().timescale;
		let timestamp = group::on_track(timestamp.into(), timescale)?;
		let sequence = state.next_sequence()?;
		if let Some(fin) = state.final_sequence()
			&& sequence >= fin
		{
			return Err(Error::Closed);
		}
		state.advance(sequence, false);
		state.cache.finish_largest(sequence);
		// A datagram is its own group, so it compresses alone.
		let deflated = state
			.info
			.as_ref()
			.unwrap()
			.compress
			.then(|| super::flate::Encoder::new().frame(&payload));
		let datagram = super::datagram::Stored {
			datagram: Datagram {
				sequence,
				timestamp,
				payload,
			},
			wire: None,
			deflated,
		};
		meter.datagram(datagram.stored_len());
		state.push_datagram(datagram);
		let cache = state.cache.clone();
		let deferred = state.deferred();
		drop(state);
		deferred.wake();
		cache.settle(None);
		Ok(sequence)
	}

	/// Insert a datagram with an explicit sequence number.
	///
	/// Preserves the supplied sequence (bumping the shared `max_sequence` if needed), so a
	/// relay can forward a datagram without renumbering it. Most origin publishers want
	/// [`Self::append_datagram`] instead.
	pub fn insert_datagram<B: crate::IntoBytes>(
		&mut self,
		sequence: u64,
		timestamp: impl Into<Option<Timestamp>>,
		payload: B,
	) -> Result<()> {
		self.insert_wire_datagram(
			Datagram {
				sequence,
				timestamp: timestamp.into(),
				payload: payload.into_bytes(),
			}
			.into(),
		)
	}

	pub(crate) fn insert_wire_datagram(&mut self, mut datagram: super::datagram::Stored) -> Result<()> {
		if datagram.payload.len() > super::datagram::MAX_DATAGRAM_PAYLOAD {
			return Err(Error::FrameTooLarge);
		}
		let meter = self.stats.meter();
		let mut state = self.modify()?;
		let info = state.info.as_ref().unwrap();
		let timescale = info.timescale;
		let status = datagram.wire.as_ref().and_then(|wire| wire.status);
		// A datagram is its own group, so it compresses alone. A status has no payload.
		if info.compress && datagram.deflated.is_none() && status.is_none() {
			datagram.deflated = Some(super::flate::Encoder::new().frame(&datagram.payload));
		}
		if status.is_none_or(|status| status == 0) {
			datagram.datagram.timestamp = group::on_track(datagram.timestamp, timescale)?;
		}
		if state.final_sequence().is_some_and(|fin| datagram.sequence >= fin) {
			return Err(Error::Closed);
		}
		state.advance(datagram.sequence, false);
		state.cache.finish_largest(datagram.sequence);
		meter.datagram(datagram.stored_len());
		state.push_datagram(datagram);
		let cache = state.cache.clone();
		let deferred = state.deferred();
		drop(state);
		deferred.wake();
		cache.settle(None);
		Ok(())
	}

	/// Insert a received datagram of a compressed track, whose payload is its own DEFLATE
	/// stream that must inflate to exactly `size` bytes. Kept as it arrived, beside the
	/// inflated payload.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn insert_deflated_datagram(&mut self, mut datagram: super::datagram::Stored, size: u64) -> Result<()> {
		if !self.info.compress {
			return Err(Error::ProtocolViolation);
		}
		// Refused before inflating, like an oversized plain payload.
		let size = usize::try_from(size)
			.ok()
			.filter(|size| *size <= super::datagram::MAX_DATAGRAM_PAYLOAD)
			.ok_or(Error::FrameTooLarge)?;
		let payload = super::flate::Decoder::new().frame(&datagram.payload, size)?;
		let inflated = payload.len() as u64;
		datagram.deflated = Some(std::mem::replace(&mut datagram.datagram.payload, payload));
		self.insert_wire_datagram(datagram)?;
		// Only once stored, so a refused datagram counts toward nothing.
		self.stats.meter().inflated(inflated);
		Ok(())
	}

	/// Create a group with a single frame, at the given presentation timestamp.
	///
	/// The timestamp is converted into the track's timescale. Pass `None` on an untimed
	/// track; a frame that doesn't match its track is [`Error::TimestampMismatch`].
	pub fn write_frame<B: crate::IntoBytes>(
		&mut self,
		timestamp: impl Into<Option<Timestamp>>,
		frame: B,
	) -> Result<()> {
		let frame = crate::IntoBytes::into_bytes(frame);
		if frame.len() as u64 > group::MAX_CACHE_BYTES {
			return Err(Error::FrameTooLarge);
		}
		// Checked before the group exists, so a refused frame leaves no empty group behind.
		let timescale = self.modify()?.info.as_ref().unwrap().timescale;
		let timestamp = group::on_track(timestamp.into(), timescale)?;
		let mut group = self.append_group()?;
		if let Err(err) = group.write_frame(timestamp, frame) {
			// A rewind is only known once the group exists; end it rather than leave it open.
			let _ = group.abort(err.clone());
			return Err(err);
		}
		group.finish()?;
		Ok(())
	}

	/// Mark the track as finished after the last appended group.
	///
	/// Ends the track at the head of the group after the current max_sequence.
	/// No new groups at or above it can be appended.
	/// NOTE: Old groups with lower sequence numbers can still arrive.
	pub fn finish(&self) -> Result<()> {
		let mut state = self.modify()?;
		let final_sequence = match state.max_sequence {
			Some(max) => max.checked_add(1).ok_or(coding::BoundsExceeded)?,
			None => 0,
		};
		state.set_end(Position::group(final_sequence))
	}

	/// Declare the track's exclusive end, possibly ahead of the live edge.
	///
	/// `end` is the first position that will never be produced. [`Position::group`]
	/// ends before a group, so a track whose last group is 89 finishes at
	/// `Position::group(90)`. A mid-group end such as `{ group: 89, frame: 5 }` also
	/// caps group 89 at frames 0 to 4. Passing an end beyond the live edge records a
	/// known ending before the remaining groups arrive (e.g. learning a track ends at
	/// group 89 while only 87 has been received).
	///
	/// Groups and frames below `end` may still be created afterwards; anything at or past
	/// it is rejected ([`Error::Closed`]). An end that would orphan a group or frame that
	/// already exists is [`Error::ProtocolViolation`]. Consumers only see end-of-stream
	/// once the live edge reaches the end's group. Use [`Self::finish`] to finish exactly
	/// at the live edge.
	///
	/// Repeating the declared end is a no-op, and a mid-group end may refine the group
	/// boundary it falls just before (a peer may name only the boundary first). Any other
	/// second end is [`Error::ProtocolViolation`].
	///
	/// On a copy a session received, groups below the end may still be in flight:
	/// readers wait for a missing one until a grace passes, the subscription's max delay
	/// or one second without one, before ending at the boundary.
	pub fn finish_at(&mut self, end: Position) -> Result<()> {
		let mut state = self.modify()?;
		match state.end {
			Some(declared) if declared == end => return Ok(()),
			Some(declared) if declared.frame == 0 && end.frame != 0 && end.final_sequence() == Some(declared.group) => {
				state.end = None;
				if let Err(err) = state.set_end(end) {
					state.end = Some(declared);
					return Err(err);
				}
			}
			Some(_) => return Err(Error::ProtocolViolation),
			None => state.set_end(end)?,
		}
		if state.received && state.tail.is_none() {
			state.tail = Some(state.cache.pool().now_wall());
		}
		Ok(())
	}

	/// Declare the first group the live feed serves (the wire's SUBSCRIBE_START,
	/// or the start the subscription itself requested): groups below `sequence`
	/// are not waited for, so a reader waiting for one fails over instead of
	/// stalling. One may still arrive late, and a fetch can still retrieve them.
	///
	/// Scoped to the current subscription's demand, so a later declaration
	/// replaces this one in either direction: a re-subscription may start
	/// earlier, and a narrowed subscription skips groups an earlier declaration
	/// still promised. Pass `None` to clear it, for demand at the live edge:
	/// its floor is unknown until the feed declares one.
	///
	/// Resolves a start a subscriber session left pending.
	pub fn start_at(&mut self, sequence: impl Into<Option<u64>>) -> Result<()> {
		self.modify()?.set_start(sequence.into(), false);
		Ok(())
	}

	/// [`Self::start_at`] a start the publisher resolved from `budget`: every older group
	/// is stale under it, so a reader whose budget is no looser gives them up unasked.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn start_resolved(&mut self, sequence: u64, budget: Duration) -> Result<()> {
		let mut state = self.modify()?;
		state.set_start(Some(sequence), false);
		state.start_budget = Some(budget);
		Ok(())
	}

	/// Mark this track as a copy received from a remote publisher, before writing to it.
	///
	/// Its declared end waits for groups still in flight below it; see [`Self::finish_at`].
	/// Its readers' relative and time anchors, other than [`Range::live`](crate::track::Range::live)
	/// and [`Range::oldest`](crate::track::Range::oldest), fail with [`Error::Unsupported`]
	/// unless its session's wire lets it resolve them, since it holds only part of the track.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn received(&self) {
		if let Ok(mut state) = self.modify() {
			state.received = true;
		}
	}

	/// Let this received copy's readers resolve relative and time anchors, for a wire that
	/// reports the live edge and media time and asks for every group the budget allows.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn anchored(&self) {
		if let Ok(mut state) = self.modify() {
			state.anchored = true;
		}
	}

	/// Let readers use only time anchors this cache can already translate to group bounds. A
	/// relative anchor counts back from the live edge the wire reports, once it does.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn anchored_cached(&self) {
		if let Ok(mut state) = self.modify() {
			state.anchored = true;
			state.cached_anchors = true;
		}
	}

	/// Have this received copy's readers ask no fills below the start its publisher resolved
	/// from the budget, for a wire whose fills and timestamps cannot bound them by it.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn fills_from_start(&self) {
		if let Ok(mut state) = self.modify() {
			state.fills_from_start = true;
		}
	}

	/// Record that the publisher gave up `groups` with `err`: none will arrive, so a reader
	/// waiting on one moves past it, a group still arriving in them ends with `err`, the
	/// declared end does not wait for them, and a relay serving this track forwards the drop.
	pub fn drop_groups(&self, groups: ops::Range<u64>, err: StreamError) {
		if groups.is_empty() {
			return;
		}
		let open: Vec<group::Handle> = match self.modify() {
			Ok(mut state) => {
				state.drop_groups(groups.clone(), err.clone());
				// Only the live feed's copy: a drop judges the subscription, so a fetched head
				// or fetch-only backfill may still complete.
				state
					.lookup
					.range(groups)
					.filter(|(_, slot)| slot.visible)
					.map(|(_, slot)| &slot.group)
					.filter(|group| !group.is_aborted())
					.cloned()
					.collect()
			}
			Err(_) => return,
		};
		// Settle the readers of a group still arriving now, rather than once its stream's
		// reset lands, with the track guard released.
		for group in open {
			group.abort_open(Error::Stream(err.clone()));
		}
	}

	/// Readers get nothing from the cache until [`Self::set_live`]: the upstream
	/// subscription ended with this copy still held, so what it cached may go stale.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn set_idle(&mut self) {
		if let Ok(mut state) = self.modify() {
			state.set_idle();
		}
	}

	/// The route answered with its largest position, `None` for nothing yet, and the first
	/// group its feed serves when the answer names it; see [`Self::set_idle`]. Readers
	/// resume once the cache shows the largest position. A no-op unless idle.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn set_live(&mut self, largest: Option<Position>, start: Option<u64>) {
		if let Ok(mut state) = self.modify() {
			state.set_live(largest, start);
		}
	}

	/// The live feed's route answered that it reaches `group`: a relative start counts back
	/// from it, rather than from whichever group lands first. A no-op unless live.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn report_edge(&self, group: u64) {
		if let Ok(mut state) = self.modify()
			&& let Feed::Live { first, edge } = &mut state.feed
		{
			*edge = (*edge).max(Some(group));
			first.get_or_insert(group);
		}
	}

	/// Record the upstream Object ID separately from the frame ordinal readers count.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn report_object(&self, group: u64, object: u64) {
		if let Ok(mut state) = self.modify() {
			// A frame received past the report outdates it on arrival. A frame's ordinal never
			// exceeds its Object ID, so one past the reported object proves the report stale.
			if state
				.cache
				.largest()
				.is_some_and(|largest| largest > Position { group, frame: object })
			{
				return;
			}
			let edge = state.object_edge().max(Some((group, object)));
			state.cache.advance_largest(Position::group(group));
			let mark = state.cache.largest().unwrap_or(Position::group(group));
			state.object_edge = edge.map(|edge| (edge, mark));
		}
	}

	/// Resolve cached time anchors before sending a wire that can name only groups.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn resolve_subscription(&self, subscription: &Subscription) -> Result<Subscription> {
		if !subscription.ranges.iter().any(|range| !range.is_wire()) {
			return Ok(subscription.clone());
		}
		let state = self.state.read();
		let window = Window::new(&subscription.ranges, state.latest_live(), &*state)?;
		if !window.is_settled() {
			return Err(Error::Unsupported);
		}
		let resolved = window.resolved();
		let mut subscription = subscription.clone();
		for (range, resolved) in subscription.ranges.iter_mut().zip(resolved) {
			if match range.start {
				super::subscription::Anchor::Time(_) => true,
				super::subscription::Anchor::Relative(back) => back != 0 && back != u64::MAX,
				_ => false,
			} {
				range.start = super::subscription::Anchor::Absolute(resolved.ok_or(Error::Unsupported)?.start);
			}
			if matches!(
				range.end,
				Some(super::subscription::Anchor::Time(_) | super::subscription::Anchor::Relative(_))
			) {
				range.end = resolved
					.ok_or(Error::Unsupported)?
					.end
					.map(super::subscription::Anchor::Absolute);
			}
		}
		Ok(subscription)
	}

	/// The source reported its largest frame, which [`Self::largest`] follows before the
	/// frame itself arrives. A hint about the edge: it never makes the frame readable.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn report_largest(&self, largest: Position) {
		self.state.read().cache.advance_largest(largest);
	}

	/// Whether readers may take from the cache; see [`Self::set_idle`].
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn is_live(&self) -> bool {
		self.state.read().feed.is_live()
	}

	/// While idle, the newest group cached when the track went idle: a route asked from
	/// its head sends it again, and its first frame says whether the cache is current.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn idle_newest(&self) -> Option<u64> {
		let state = self.state.read();
		match state.feed {
			Feed::Idle { newest } => newest,
			_ => None,
		}
	}

	/// [`Request::request_start`] for a copy already accepted, which a held copy's next
	/// subscription asks for again.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn request_start(&mut self, sequence: Option<u64>) -> Result<()> {
		self.modify()?.set_start(sequence, true);
		Ok(())
	}

	/// The declared first sequence of the live feed, once [`Self::start_at`]
	/// declared one. `None` while nothing was declared.
	#[cfg(test)]
	#[cfg(feature = "async")]
	pub(crate) fn start_sequence(&self) -> Option<u64> {
		self.state.read().start_sequence
	}

	/// The exclusive end, once [`Self::finish`] or [`Self::finish_at`] declared one.
	///
	/// `None` while the track is still open ended. Both methods reject a second end, so
	/// callers that may have already declared one check here first.
	pub fn finished(&self) -> Option<Position> {
		self.state.read().end
	}

	/// Abort the track with the given error.
	///
	/// Consumes the handle, since nothing can be written to an aborted track. Consumers
	/// still draining get the finished groups, then the abort error. Open groups, fetched
	/// copies included, are aborted with the same error and leave the cache, so a consumer
	/// that already pulled one ends with it too, and their writers fail. The pool's idle expiry (see
	/// [`cache::Config::with_expiry`]) reclaims the rest, the latest group included, so a
	/// stale [`Consumer`] can't pin them in memory forever.
	///
	/// If the declared end had settled (the final sequence from
	/// [`finish_at`](Self::finish_at) was reached and every group below it finished),
	/// consumers get a clean end instead of the error, as after [`finish`](Self::finish).
	///
	/// [`finish`](Self::finish) is deliberately not terminal: it declares the final
	/// sequence, and lower-numbered groups may still be written afterwards.
	pub fn abort(self, err: Error) -> Result<()> {
		// Unlisted first, so the track leaves the listing before its name is free again.
		self.alive.unlist(false);
		commit_abort(self.modify()?, err).abort();
		Ok(())
	}

	/// Abort an unwanted track, returning the producer unchanged if consumers remain.
	///
	/// Consumer creation and the unwanted check share a lock, so demand returning
	/// after [`Demand::wanted`] went to `None` prevents the abort. `Ok(())` means
	/// the track is closed, including when it was already closed; existing handles
	/// may still observe its final state. `Err(producer)` leaves the track unchanged
	/// so the caller can continue serving and wait for the next unwanted wake.
	///
	/// The abort is the same as [`Self::abort`]. A reader still in a group it was handed
	/// counts as a consumer until it reads the group's end.
	#[cfg_attr(
		target_pointer_width = "64",
		expect(
			clippy::result_large_err,
			reason = "return the owned producer without allocating on an idle check"
		)
	)]
	pub fn abort_unwanted(self, err: Error) -> std::result::Result<(), Self> {
		let wanted = match self.state.write_unwanted() {
			kio::Unwanted::Idle(guard) => {
				commit_abort(guard, err).abort();
				false
			}
			kio::Unwanted::Closed => false,
			kio::Unwanted::Wanted => true,
		};
		if wanted {
			return Err(self);
		}
		// After the track's own lock is released; a later track of the name, listed in
		// between, keeps its listing.
		self.alive.unlist(false);
		Ok(())
	}

	/// Block until the track's outcome is final: `Ok` once its declared end is reached, or
	/// the abort error, or [`Error::Dropped`] once every producer left without either.
	#[cfg(feature = "async")]
	pub async fn closed(&self) -> Result<()> {
		kio::wait(|waiter| self.poll_closed(waiter)).await
	}

	/// Poll until the track's outcome is final: `Ok` once its declared end is reached, or
	/// the abort error, or [`Error::Dropped`] once every producer left without either.
	pub fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<Result<()>> {
		TrackState::settle(self.state.poll_ref(waiter, |state| state.poll_outcome(waiter)))
	}

	/// Poll until the track is aborted, or every producer left; ready with the cause.
	// Sessions stop serving on an abort, but keep taking lower groups past a finish.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn poll_aborted(&self, waiter: &kio::Waiter) -> Poll<Error> {
		TrackState::poll_aborted(&self.state, waiter)
	}

	/// Return true if the track has been closed.
	pub fn is_closed(&self) -> bool {
		self.state.read().is_closed()
	}

	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn latest_sequence(&self) -> Option<u64> {
		self.state.read().max_sequence
	}

	/// The track's largest location: its newest group or datagram at the newest frame (0 while empty), `None` before any.
	pub fn largest(&self) -> Option<Position> {
		self.state.read().cache.largest()
	}

	/// Ready once the track holds content past `sequence`, or is closed: a later group whose
	/// first frame opened or that finished, or a later datagram. A frameless group header
	/// does not count, since nothing says the copy reached it yet.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn poll_past(&self, sequence: u64, waiter: &kio::Waiter) -> Poll<()> {
		let Some(next) = sequence.checked_add(1) else {
			return Poll::Pending;
		};
		let past = self.state.poll_ref(waiter, |state| {
			if state.shows(next) {
				return Poll::Ready(());
			}
			// A group starting wakes no reader parked on the track, so wait on each one past
			// it that has yet to start.
			let started = state
				.lookup
				.range(next..)
				.filter(|(_, slot)| slot.is_live())
				.any(|(_, slot)| slot.group.poll_started(waiter).is_ready() && !slot.group.is_aborted());
			match started {
				true => Poll::Ready(()),
				false => Poll::Pending,
			}
		});
		past.map(|_| ())
	}

	/// Return true if this is the same track.
	pub fn is_clone(&self, other: &Self) -> bool {
		self.state.same_channel(&other.state)
	}

	/// Create a weak reference that doesn't prevent auto-close.
	pub(super) fn weak(&self) -> TrackWeak {
		TrackWeak {
			name: self.name.clone(),
			state: self.state.weak(),
		}
	}

	/// Create a [`Demand`]: a cloneable, watch-only handle to this track's
	/// subscriber demand.
	///
	/// Lets a publisher gate work (e.g. on-demand capture) on whether anyone is
	/// subscribed, without the ability to publish frames or close the track. The
	/// handle is weak, so holding one neither keeps the track alive nor pins its
	/// cached groups.
	pub fn demand(&self) -> Demand {
		Demand::new(self.name.clone(), self.state.weak())
	}

	/// Get a consumer handle for this in-process track.
	///
	/// Unlike a wire subscription, the info is already known, so a subscription
	/// opened from this handle resolves immediately.
	pub fn consume(&self) -> Consumer {
		Consumer::new(self.name.clone(), self.state.weak().consume_weak())
	}

	/// Subscribing to this in-process track, resolving synchronously.
	///
	/// The info is fixed at creation, so there's nothing to wait for (no
	/// SUBSCRIBE_OK round trip). Pass `None` for [`Subscription::default`].
	///
	/// Reads only the groups the subscription's ranges cover.
	/// [`Subscription::max_delay`] is what asks for data: delivery skips everything in range
	/// that the budget convicts, so the default budget of zero delivers only the latest
	/// group and a larger one reaches back over what it can still use.
	pub fn subscribe(&self, subscription: impl Into<Option<Subscription>>) -> Subscriber {
		let preferences = subscription.into().unwrap_or_default();

		// Info is fixed at creation and survives a close/abort, so read it without
		// requiring a live producer state. If the track already ended, the returned
		// subscriber surfaces the close/abort on its first read; the preferences are
		// simply never registered (nothing aggregates them anymore).
		let info = self.info.clone();
		let subscription = Preferences::register(self.state.read(), preferences);

		// Hoisted: an inline `read()` guard would live to the end of the struct literal,
		// deadlocking against the `consume()` below.
		let broadcast = self.state.read().broadcast.clone();
		Subscriber {
			name: self.name.clone(),
			broadcast,
			info,
			inner: Inner::Plain(Cursor::new(self.state.consume(), subscription)),
			delivered: Delivered::default(),
			// A producer-side (in-process) subscribe is not egress: stay untagged.
			stats: stats::Scope::default(),
			_stats_sub: stats::Subscription::default(),
			largest: Reported::default(),
		}
	}

	/// Block until the aggregate subscription changes, then return the new value.
	///
	/// Yields the most demanding request across all live subscribers, or `None`
	/// once the last one drops. Used by relays to forward downstream demand
	/// upstream (e.g. SUBSCRIBE_UPDATE).
	#[cfg(feature = "async")]
	pub async fn subscription_changed(&mut self) -> Result<Option<Subscription>> {
		kio::wait(|waiter| self.poll_subscription_changed(waiter)).await
	}

	/// A non-blocking snapshot of the current aggregate subscription, or `None`
	/// when there are no live subscribers. Unlike [`Self::subscription`], this
	/// doesn't wait for a change or advance the change cursor.
	///
	/// The aggregate's [`Subscription::max_delay`] is clamped to this track's
	/// [`Info::max_age`]: no subscriber can wait for a late group longer than the
	/// publisher keeps it.
	pub fn subscription(&self) -> Option<Subscription> {
		let state = self.state.read();
		let (subs, bound) = (state.subscriptions.clone(), state.max_age_bound());
		drop(state);
		snapshot_subscription(&subs, bound)
	}

	/// Poll for a changed subscription: the
	/// aggregate subscription whenever it changes, or `None` once nobody is subscribed.
	/// Errors once the track is aborted.
	pub fn poll_subscription_changed(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Subscription>>> {
		// Surface an abort as the stream ending. `poll_closed` parks on the closed
		// waiters, so per-group churn on the track state never wakes this poll.
		if self.state.poll_closed(waiter).is_ready() {
			let abort = self.state.read().abort.clone();
			return Poll::Ready(Err(abort.unwrap_or(Error::Dropped)));
		}

		// Read the bound before locking `subs`, so the aggregation never nests the two locks.
		let state = self.state.read();
		let (subs, bound) = (state.subscriptions.clone(), state.max_age_bound());
		drop(state);

		poll_combined_changed(&subs, bound, &mut self.prev_subscription, waiter).map(Ok)
	}

	/// Create a [`Requests`] handle that serves on-demand fetches of uncached
	/// (old) groups. Most producers never need this; a relay creates one to fetch
	/// past groups from upstream.
	pub fn requests(&self) -> Requests {
		Requests::new(self.name.clone(), self.state.clone(), self.alive.clone())
	}

	fn modify(&self) -> Result<kio::Mut<'_, TrackState>> {
		TrackState::modify(&self.state)
	}
}

/// Serves the groups a track's readers miss, the group-level analogue of
/// [`broadcast::Requests`].
///
/// Most tracks never serve old content, so this capability lives on a dedicated
/// handle rather than [`Producer`]: a relay creates one (via
/// [`Producer::requests`] or [`Request::requests`]) to pull past groups
/// from upstream. While at least one is alive a reader's miss queues a [`Fill`] for it
/// to serve; with none, the miss is named dropped, and a cache-miss
/// [`Consumer::fetch_group`] on an accepted track fails fast with [`Error::NotFound`].
pub struct Requests {
	name: Arc<str>,
	// Kept to insert served groups into the cache and observe track abort.
	state: kio::Producer<TrackState>,
	// The fills this handle drains; its handler count gates queueing.
	fills: kio::Shared<FillState>,
	// Shared with the track's producers: a handler still serving fills keeps the
	// track alive, like a producer clone does.
	alive: Arc<Alive>,
}

impl Requests {
	fn new(name: Arc<str>, state: kio::Producer<TrackState>, alive: Arc<Alive>) -> Self {
		let fills = state.read().fills.clone();
		fills.lock().add_handler();
		Self {
			name,
			state,
			fills,
			alive,
		}
	}

	/// The track's name, unique within its broadcast.
	pub fn name(&self) -> &str {
		&self.name
	}

	/// Block until readers miss a run of groups, returning a [`Fill`] to serve via
	/// [`Fill::accept`] and [`Fill::drop_groups`].
	///
	/// A relay fetches the run upstream; an origin already has its groups cached, so
	/// nothing reaches here. Errors once the track is aborted.
	#[cfg(feature = "async")]
	pub async fn requested_fill(&self) -> Result<Fill> {
		kio::wait(|waiter| self.poll_requested_fill(waiter)).await
	}

	/// Poll for a fill to serve.
	pub fn poll_requested_fill(&self, waiter: &kio::Waiter) -> Poll<Result<Fill>> {
		fill::poll_requested(&self.state, &self.fills, waiter)
	}

	/// The next fill, which a test expects to name a single group.
	#[cfg(test)]
	pub(crate) fn poll_requested_fill_group(&self, waiter: &kio::Waiter) -> Poll<Result<GroupFill>> {
		self.poll_requested_fill(waiter).map_ok(|fill| {
			assert_eq!(fill.end() - fill.start().group, 1, "a one-group fill");
			fill.group(fill.start().group)
		})
	}

	/// [`Self::poll_requested_fill_group`], awaited.
	#[cfg(all(test, feature = "async"))]
	pub(crate) async fn requested_fill_group(&self) -> Result<GroupFill> {
		kio::wait(|waiter| self.poll_requested_fill_group(waiter)).await
	}

	/// Watch subscriber demand without keeping the track alive.
	pub fn demand(&self) -> Demand {
		Demand::new(self.name.clone(), self.state.weak())
	}
}

impl Clone for Requests {
	fn clone(&self) -> Self {
		// Count each live handle (mirrors `broadcast::Requests`).
		self.fills.lock().add_handler();
		Self {
			name: self.name.clone(),
			state: self.state.clone(),
			fills: self.fills.clone(),
			alive: self.alive.clone(),
		}
	}
}

impl Drop for Requests {
	fn drop(&mut self) {
		// Unlike `broadcast::Requests`, dropping the last handle doesn't abort the track:
		// a live `Producer` may still be serving the subscription. It just stops serving
		// fills: a queued one no handler will ever take closes, so its readers stop
		// waiting on it, while one already taken stays with its `Fill`.
		self.fills.lock().remove_handler();
	}
}

/// Ends the track when the last [`Producer`] or [`Requests`] drops.
///
/// A refcount rather than a "am I the last one?" check inside `Drop`: that answer is a
/// snapshot, and acting on it is exactly what invalidates it. The track state's own
/// producer count can't answer it either, since a group settling its eviction debt
/// upgrades the account's weak handle and counts there for the duration (see
/// [`cache::Track::settle`]). Holding a producer of its own also keeps the state
/// writable until the teardown has run, whatever order the last owner's fields drop in.
struct Alive {
	name: Arc<str>,
	state: kio::Producer<TrackState>,

	// Set when a `Producer` is first minted, so a `Request` nobody accepted (its
	// `Requests` holds this guard too) isn't reported as an abandoned publisher.
	published: AtomicBool,

	// Ingress subscription for this track, opened by the tagged producer that claimed
	// it and closed when this guard drops.
	stats: OnceLock<stats::Subscription>,

	// The broadcast's listing of a track its producer created, withdrawn once the track
	// leaves the table: on abort, or when this guard drops.
	listed: kio::Lock<Option<broadcast::Unlist>>,
}

impl Alive {
	fn new(name: Arc<str>, state: kio::Producer<TrackState>) -> Arc<Self> {
		Arc::new(Self {
			name,
			state,
			published: Default::default(),
			stats: Default::default(),
			listed: kio::Lock::new(None),
		})
	}

	fn list(&self, unlist: broadcast::Unlist) {
		*self.listed.lock() = Some(unlist);
	}

	/// Withdraw the track from its broadcast's listing, as one that `finished` or left
	/// unfinished. Never under the broadcast's lock.
	fn unlist(&self, finished: bool) {
		let unlist = self.listed.lock().take();
		if let Some(unlist) = unlist {
			unlist.unlist(finished);
		}
	}

	/// Note that a [`Producer`] was minted from this track, optionally under a tagged
	/// broadcast's ingress scope (counted as one subscription for as long as the track
	/// has a publisher).
	fn publish(&self, stats: Option<&stats::Scope>) {
		self.published.store(true, Ordering::Relaxed);
		if let Some(scope) = stats {
			// At most one scope ever arrives: a track is minted either through
			// `Producer::new` (+ `with_stats`) or through `Request::accept`, never both.
			let _ = self.stats.set(scope.subscribe());
		}
	}
}

impl Drop for Alive {
	fn drop(&mut self) {
		// Withdrawn before the state closes, so the track leaves the listing before a
		// later one can take its name. It finished if its producer declared its end.
		let finished = self.state.read().end.is_some();
		self.unlist(finished);
		// A request nobody accepted was never publishing; there's nothing to tear down.
		if !self.published.load(Ordering::Relaxed) {
			return;
		}
		// The last producer going away ends the track: nothing protects its live edge
		// anymore, so the pool's idle expiry reclaims what a stale consumer would pin.
		// Without a finish it is an abrupt teardown, the same as an explicit abort:
		// the open groups go and the finished ones stay for readers still draining.
		// `abort()` closes the channel, so `write()` returns `Err(Ref)`. `finish()`
		// leaves it open with `end` set, so inspect both outcomes.
		match self.state.write() {
			Ok(mut state) => {
				if state.end.is_some() {
					// Groups still missing below the boundary can no longer arrive, so a
					// reader waiting on one ends cleanly instead of with `Dropped`.
					state.sealed = true;
					state.close_cache();
					return;
				}
				if state.abort.is_some() {
					return;
				}
				tracing::warn!(
					track = %self.name,
					"track::Producer dropped without finish() or abort()"
				);
				let open = state.tear_down(Error::Dropped);
				drop(state);
				open.abort();
			}
			Err(state) => {
				if state.sealed || state.end.is_some() || state.abort.is_some() {
					return;
				}
				tracing::warn!(
					track = %self.name,
					"track::Producer dropped without finish() or abort()"
				);
			}
		}
	}
}

/// Poll the counted aggregate without registering on individual readers.
fn poll_combined_changed(
	subs: &kio::Lock<Subscriptions>,
	bound: Option<Duration>,
	prev: &mut Option<Subscription>,
	waiter: &kio::Waiter,
) -> Poll<Option<Subscription>> {
	subs.lock().poll_changed(bound, prev, waiter)
}

/// Read the counted aggregate without walking the audience.
fn snapshot_subscription(subs: &kio::Lock<Subscriptions>, bound: Option<Duration>) -> Option<Subscription> {
	clamp_combined(subs.lock().combined(), bound)
}

/// Clamp a drift budget to the publisher's retention window: nobody can wait for a late
/// group longer than the publisher keeps it around.
///
/// The single clamp point. Subscribers hold their preferences verbatim, so what they asked
/// for stays readable, and it is applied here on both sides of the aggregate: to the
/// combined request the publisher sees ([`clamp_combined`]) and to one subscriber's own
/// budget when it decides a group is stale ([`TrackState::is_stale`]). Those agree because
/// `min` distributes over the `max` that combines them. `bound` is `None` on a track whose
/// info isn't known yet (an unaccepted [`Request`]), which imposes no window.
fn clamp_max_delay(mut max_delay: Duration, bound: Option<Duration>) -> Duration {
	if let Some(bound) = bound {
		max_delay = max_delay.min(bound);
	}
	max_delay
}

/// Clamp the aggregate's max delay budget to the publisher's window; see [`clamp_max_delay`].
fn clamp_combined(combined: Option<Subscription>, bound: Option<Duration>) -> Option<Subscription> {
	let mut combined = combined?;
	if bound.is_some() {
		combined.max_delay = Some(clamp_max_delay(combined.budget(), bound));
	}
	Some(combined)
}

/// A weak reference to a track that doesn't prevent auto-close.
#[derive(Clone)]
pub(super) struct TrackWeak {
	name: Arc<str>,
	state: kio::ProducerWeak<TrackState>,
}

impl TrackWeak {
	/// Join the cached track's demand, or `None` once it has closed.
	///
	/// The count moves under the same lock the close takes, which is the other half
	/// of [`Producer::abort_unwanted`]: a request either joins in time to decline the
	/// teardown, or gets nothing and re-requests the track. It is never handed a track
	/// that is already on its way out.
	pub(super) fn try_consume(
		&self,
		subscription: Option<&Subscription>,
	) -> Option<(kio::Consumer<TrackState>, Option<Preferences>)> {
		// Register before the first consumer can wake the serving session, while
		// still refusing a copy whose idle teardown already committed.
		let preferences = subscription.map(|value| Preferences::register(self.state.read(), value.clone()));
		Some((self.state.try_consume()?, preferences))
	}

	/// Read the cached track without joining its demand.
	pub(super) fn peek(&self) -> kio::ConsumerWeak<TrackState> {
		self.state.consume_weak()
	}

	/// The shared name handle, for use as a broadcast lookup key (clone is a
	/// refcount bump, and the same `Arc` is shared with the track's handles).
	pub(super) fn name(&self) -> &Arc<str> {
		&self.name
	}

	/// Reject a track nothing ever served, resolving its pending subscribes with `err`.
	///
	/// A track whose [`Producer`] was minted is left alone and this returns false;
	/// so is one a handler claimed, and one that already carries an abort reason. Fetched backfill can install
	/// [`Info`] before acceptance, so metadata alone does not prove a publisher exists.
	///
	/// Closes the state like [`Producer::abort`], so a [`Request`] still held by the
	/// publisher can't `accept` its way back to life afterwards.
	pub(super) fn reject(&self, err: Error) -> bool {
		let Some(producer) = self.state.produce() else {
			return false;
		};
		let Ok(mut state) = producer.write() else {
			return false;
		};
		if state.published || state.claimed || state.abort.is_some() {
			return false;
		}
		state.abort = Some(err);
		state.close();
		true
	}

	/// Whether its producer declared the track's end, which is how its listing ends too.
	pub(super) fn is_finished(&self) -> bool {
		self.state.read().end.is_some()
	}

	/// Whether anyone is consuming the track right now. A closed track doesn't
	/// count even if consumers linger to drain its cache: no new work is owed.
	pub(super) fn is_wanted(&self) -> bool {
		!self.state.is_closed() && self.state.wanted()
	}

	/// A [`Demand`] on the cached track, for a broadcast to watch.
	pub(super) fn demand(&self) -> Demand {
		Demand::new(self.name.clone(), self.state.clone())
	}

	/// The readers' aggregate demand and contribution count, or `None` without one.
	pub(super) fn subscription(&self) -> Option<(Subscription, usize)> {
		let state = self.state.read();
		let (subs, bound) = (state.subscriptions.clone(), state.max_age_bound());
		drop(state);
		let subs = subs.lock();
		Some((clamp_combined(subs.combined(), bound)?, subs.len()))
	}

	/// End the track with `err` unless a reader holds it, atomically with lookups: a
	/// lookup either gets a consumer in time to keep it, or finds it closed and asks
	/// afresh. True once the track is gone.
	pub(super) fn abort_unwanted(&self, err: Error) -> bool {
		let Some(producer) = self.state.produce() else {
			return true;
		};
		match producer.write_unwanted() {
			kio::Unwanted::Idle(guard) => {
				commit_abort(guard, err).abort();
				true
			}
			kio::Unwanted::Closed => true,
			kio::Unwanted::Wanted => false,
		}
	}

	/// `Ready(Ok)` while anyone reads the open track, `Ready(Err)` once it closed, and
	/// otherwise park `waiter` for the next reader. Only `Pending` registers: act on the
	/// answer, not a later [`Self::is_wanted`], or a reader leaving in between is never seen.
	pub(super) fn poll_wanted(&self, waiter: &kio::Waiter) -> Poll<std::result::Result<(), kio::Closed>> {
		self.state.poll_wanted_changed(waiter, false).map_ok(drop)
	}

	/// `Ready(Ok)` while nobody reads the track, `Ready(Err)` once it closed, and
	/// otherwise park `waiter` for the last reader leaving. Only `Pending` registers,
	/// as with [`Self::poll_wanted`].
	pub(super) fn poll_unwanted(&self, waiter: &kio::Waiter) -> Poll<std::result::Result<(), kio::Closed>> {
		self.state.poll_wanted_changed(waiter, true).map_ok(drop)
	}
}

impl super::WeakEntry for TrackWeak {
	fn is_closed(&self) -> bool {
		self.state.is_closed()
	}

	fn same_channel(&self, other: &Self) -> bool {
		self.state.same_channel(&other.state)
	}
}

/// A cloneable, watch-only handle to a track's subscriber demand.
///
/// Obtained from [`Producer::demand`], [`Request::demand`], or [`Requests::demand`].
/// A publisher uses it to learn which group anyone subscribed, querying, or fetching
/// wants (on-demand capture / encoding) without being able to publish frames or close the
/// track. A bare [`Consumer`] is not demand. A group a subscription or fetch handed out
/// keeps its track wanted until a reader reads its end, and a wanted track makes its
/// broadcast wanted ([`broadcast::Demand`]). It's a weak handle, so it neither keeps
/// the track alive nor pins its cached groups; once the owning [`Producer`]
/// goes away, [`poll_wanted_changed`](Self::poll_wanted_changed) reports the track's
/// closure. Each clone watches on its own, so its first
/// [`poll_wanted_changed`](Self::poll_wanted_changed) reports the current value.
pub struct Demand {
	name: Arc<str>,
	state: kio::ProducerWeak<TrackState>,
	// The value `poll_wanted_changed` last returned, or `None` before it returned any.
	prev: Option<Option<u64>>,
}

impl Clone for Demand {
	fn clone(&self) -> Self {
		Self::new(self.name.clone(), self.state.clone())
	}
}

impl Demand {
	fn new(name: Arc<str>, state: kio::ProducerWeak<TrackState>) -> Self {
		Self {
			name,
			state,
			prev: None,
		}
	}

	/// The track name this handle is bound to.
	pub fn name(&self) -> &str {
		&self.name
	}

	/// The group consumers want: `None` while nobody consumes the open track, and
	/// `Some(n)` while they want group `n` or later.
	///
	/// A publisher opens a group whenever `n` passes its largest. The first consumer of a
	/// track with no group asks for `Some(0)`, and a join judged by [`Info::refresh`]
	/// asks for the next free sequence until a group lands there, so a datagram taking it
	/// moves the ask past. `n` never goes back, so once a group reaches it nothing is
	/// pending until another join asks.
	pub fn wanted(&self) -> Option<u64> {
		if !self.is_wanted() {
			return None;
		}
		let wanted = self.subscriptions().lock().wanted();
		Some(wanted)
	}

	/// Return [`wanted`](Self::wanted) once it differs from what this handle last returned:
	/// immediately on the first call, then on each change. Errors once the track is closed
	/// or aborted.
	///
	/// So `while let Some(n) = demand.wanted_changed().await?` runs while anyone wants the
	/// track, and `while demand.wanted_changed().await?.is_none()` waits for someone to.
	#[cfg(feature = "async")]
	pub async fn wanted_changed(&mut self) -> Result<Option<u64>> {
		kio::wait(|waiter| self.poll_wanted_changed(waiter)).await
	}

	/// The demand aggregate.
	fn subscriptions(&self) -> kio::Lock<Subscriptions> {
		self.state.read().subscriptions.clone()
	}

	/// Poll for [`wanted`](Self::wanted) to differ from what this handle last returned.
	pub fn poll_wanted_changed(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<u64>>> {
		let active = match self.poll_state(waiter) {
			DemandState::Closed => return Poll::Ready(Err(self.abort_reason())),
			DemandState::Active => true,
			DemandState::Idle => false,
		};
		let wanted = self.subscriptions().lock().poll_wanted(waiter);
		let next = active.then_some(wanted);
		if self.prev == Some(next) {
			return Poll::Pending;
		}
		self.prev = Some(next);
		Poll::Ready(Ok(next))
	}

	/// Block until the track's outcome is final; see [`Producer::closed`].
	#[cfg(feature = "async")]
	pub async fn closed(&self) -> Result<()> {
		kio::wait(|waiter| {
			TrackState::settle(
				self.state
					.consume_weak()
					.poll(waiter, |state| state.poll_outcome(waiter)),
			)
		})
		.await
	}

	/// The publisher's tie-break priority, as set in [`Info::priority`].
	pub(super) fn priority(&self) -> i8 {
		// Always Some once the track exists; a closed one reads its last value.
		self.state
			.read()
			.info
			.as_ref()
			.map_or(DEFAULT_PRIORITY, |info| info.priority)
	}

	/// Whether the track is open and anyone consumes it right now, without waiting.
	pub(super) fn is_wanted(&self) -> bool {
		!self.state.is_closed() && self.state.wanted()
	}

	/// Whether the track is gone, without waiting.
	pub(super) fn is_closed(&self) -> bool {
		self.state.is_closed()
	}

	/// Read the current demand and arm `waiter` for the next transition.
	///
	/// Level-triggered: it answers what the demand is *now* and wakes on whichever way it
	/// can move next, so a poll-driven caller watching several tracks at once doesn't have
	/// to track which edge it's waiting for.
	pub(super) fn poll_state(&self, waiter: &kio::Waiter) -> DemandState {
		loop {
			match self.state.poll_wanted_changed(waiter, false) {
				Poll::Ready(Err(_)) => return DemandState::Closed,
				// Not wanted, and armed for it becoming wanted.
				Poll::Pending => return DemandState::Idle,
				// Wanted, so arm for the reverse.
				Poll::Ready(Ok(_)) => match self.state.poll_wanted_changed(waiter, true) {
					Poll::Ready(Err(_)) => return DemandState::Closed,
					Poll::Pending => return DemandState::Active,
					// Went idle between the two reads, so neither poll armed anything.
					// Start over rather than returning a state with no waker behind it.
					Poll::Ready(Ok(_)) => continue,
				},
			}
		}
	}

	/// The recorded abort reason, or [`Error::Dropped`] if the track closed without one.
	fn abort_reason(&self) -> Error {
		self.state.read().abort.clone().unwrap_or(Error::Dropped)
	}
}

#[cfg(all(test, feature = "async"))]
impl Demand {
	/// Wait until [`Self::wanted`] satisfies `f`, returning it. Errors once the track closes.
	pub(crate) async fn until(&mut self, f: impl Fn(Option<u64>) -> bool) -> Result<Option<u64>> {
		kio::wait(|waiter| {
			loop {
				match self.poll_wanted_changed(waiter) {
					Poll::Ready(Ok(wanted)) if f(wanted) => return Poll::Ready(Ok(wanted)),
					Poll::Ready(Ok(_)) => continue,
					Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
					Poll::Pending => return Poll::Pending,
				}
			}
		})
		.await
	}
}

/// What [`Demand::poll_state`] found.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum DemandState {
	/// At least one subscriber.
	Active,
	/// No subscribers, but the track is still open.
	Idle,
	/// The track is gone; it will never be demanded again.
	Closed,
}

/// A watch-only handle to a track's live arrivals.
// Legacy fetch batches share one ledger read per poll.
pub(crate) struct Arrivals {
	state: kio::ConsumerWeak<TrackState>,
}

#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
impl Arrivals {
	/// Whether both handles watch the same track.
	// A fetch batch may share only one track's arrival ledger.
	pub(crate) fn same_channel(&self, other: &Self) -> bool {
		self.state.same_channel(&other.state)
	}

	/// Read the ledger and wake its owner when arrivals or terminal bounds change.
	// One owner watch opens batch deadlines without waking each held fetch.
	pub(crate) fn poll(&self, waiter: &kio::Waiter) -> ArrivalRead<'_> {
		let _ = self.state.poll(waiter, |_| Poll::<()>::Pending);
		let state = self.state.read();
		let max_delay = clamp_max_delay(state.subscriptions.lock().max_delay(), state.max_age_bound());
		ArrivalRead {
			state,
			grace: ledger::grace(max_delay),
		}
	}
}

/// A locked arrival ledger, so a batch never copies or re-locks its runs per fetch.
pub(crate) struct ArrivalRead<'a> {
	state: kio::Ref<'a, TrackState>,
	grace: Duration,
}

#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
impl ArrivalRead<'_> {
	/// Whether the live feed can no longer deliver any missing group.
	// The batch checks closure without re-locking the track.
	pub(crate) fn is_closed(&self) -> bool {
		self.state.is_closed()
	}

	/// Gaps that can expire held fetches, including the gap before the first arrival.
	// Only the session's held-fetch deadline index needs all live gaps.
	pub(crate) fn folds(&self) -> impl Iterator<Item = (ops::Range<u64>, crate::time::Instant)> + '_ {
		self.state.arrivals.folds(self.grace)
	}

	/// Judge one live group and register only for its own arrival or terminal bound: the
	/// group, the error it was given up with, or `None` once it is never coming.
	// Legacy wires lack FETCH, so held requests need the live-feed verdict.
	pub(crate) fn poll_group(
		&self,
		sequence: u64,
		now: crate::time::Instant,
		waiter: &kio::Waiter,
	) -> Poll<Result<Option<group::Consumer>>> {
		match self.state.poll_arrival(sequence, now, self.grace, waiter) {
			Poll::Pending if self.state.is_closed() => Poll::Ready(Ok(None)),
			res => res,
		}
	}

	/// The groups of `groups` [`Self::poll_group`] gives up on, judged by run, so a span a
	/// peer made wide costs what the cache and ledger hold rather than its width.
	// Legacy wires lack FETCH, so a held fill settles what the feed gave up in bulk.
	pub(crate) fn given_up(&self, groups: ops::Range<u64>, now: crate::time::Instant) -> RangeSet<u64> {
		let state = &*self.state;
		let mut gone = RangeSet::new();
		let mut give_up = |groups: ops::Range<u64>| {
			if !groups.is_empty() {
				gone.insert(groups);
			}
		};
		let closed = self.state.is_closed();
		let start = state.start_sequence.unwrap_or(0);
		let fin = state.final_sequence().unwrap_or(u64::MAX);
		for hole in miss::holes(state, groups) {
			if closed {
				give_up(hole);
				continue;
			}
			give_up(hole.start..hole.end.min(start));
			give_up(hole.start.max(fin)..hole.end);
			for (dropped, _) in state.drops.overlapping(&hole) {
				give_up(dropped.start.max(hole.start)..dropped.end.min(hole.end));
			}
			// Every group of a gap folds at once: its grace runs from the run above it.
			let folded = |gap: u64| state.arrivals.folds_at(gap, self.grace).is_some_and(|at| at <= now);
			let mut from = hole.start;
			for arrived in state.arrivals.accounted(hole.clone()) {
				if from < arrived.start && folded(from) {
					give_up(from..arrived.start);
				}
				from = arrived.end;
				give_up(arrived);
			}
			if from < hole.end && folded(from) {
				give_up(from..hole.end);
			}
		}
		gone
	}
}

/// A lazy handle to a single track within a broadcast.
///
/// Obtained from [`broadcast::Consumer::track`]. Holding it sends nothing and is not
/// [`Demand`]: the track is requested, and counts as demanded, only while an
/// [`info`](Self::info), [`subscribe`](Self::subscribe) (a live, ongoing stream of
/// groups), or [`fetch_group`](Self::fetch_group) (one group) made from it is held. The
/// same handle can be used any number of times, and clones are cheap.
#[derive(Clone)]
pub struct Consumer {
	name: Arc<str>,
	// The broadcast this track belongs to, so a catalog track can name the path its
	// relative references resolve against. Rebound by `broadcast::Consumer::track` to
	// that handle's view, which may name the broadcast differently than its producer did.
	broadcast: Arc<broadcast::Info>,
	source: Source,
	// Egress stats scope, set by a tagged [`broadcast::Consumer`] via
	// [`Self::with_stats`]. Empty (no-op) for an untagged track.
	stats: stats::Scope,
	// Last position this handle yielded through `largest`.
	largest: Option<Position>,
}

/// Where a [`Consumer`] finds its track. Neither joins the track's consumer count, which
/// is its demand: only the requests made from the handle do.
#[derive(Clone)]
enum Source {
	/// One track instance, from its producer or request.
	Track(kio::ConsumerWeak<TrackState>),
	/// Whatever the broadcast serves under the name when a request is made, asking for
	/// the track only then. Once the broadcast has ended, the track it served when the
	/// handle was made, if any: a handle already handed out keeps reading it. Boxed so the
	/// handle stays small.
	Broadcast(Box<broadcast::Consumer>, Option<kio::ConsumerWeak<TrackState>>),
}

impl Consumer {
	/// Whether these handles resolve to the same cached track, without subscribing.
	pub(super) fn same_channel(&self, other: &Self) -> bool {
		self.with_state(|left| {
			other.with_state(|right| left.zip(right).is_some_and(|(left, right)| left.same_channel(right)))
		})
	}

	/// An already declared end, without resolving a pending subscription or registering a wake.
	pub(super) fn declared_end(&self) -> Option<Position> {
		self.with_state(|state| state?.read().end)
	}

	fn new(name: Arc<str>, state: kio::ConsumerWeak<TrackState>) -> Self {
		let broadcast = state.read().broadcast.clone();
		Self {
			name,
			broadcast,
			source: Source::Track(state),
			stats: stats::Scope::default(),
			largest: None,
		}
	}

	/// A handle resolving `name` through `broadcast` on each request. Called by
	/// [`broadcast::Consumer::track`] with that handle's view of the broadcast, and the
	/// track it serves under the name now, if any.
	pub(super) fn lookup(
		name: Arc<str>,
		info: Arc<broadcast::Info>,
		broadcast: broadcast::Consumer,
		current: Option<kio::ConsumerWeak<TrackState>>,
	) -> Self {
		Self {
			name,
			broadcast: info,
			source: Source::Broadcast(Box::new(broadcast), current),
			stats: stats::Scope::default(),
			largest: None,
		}
	}

	/// The track a request made now reads, joined to its consumer count. A broadcast
	/// that cannot serve the name answers with a track closed with its error.
	fn resolve(&self, subscription: Option<&Subscription>) -> (kio::Consumer<TrackState>, Option<Preferences>) {
		let state = match &self.source {
			Source::Track(state) => state.clone(),
			Source::Broadcast(broadcast, current) => match broadcast.resolve_track(&self.name, subscription) {
				Ok(resolved) => return resolved,
				Err(Error::Unroutable) if let Some(current) = current => current.clone(),
				Err(err) => {
					let request = Request::new(self.broadcast.clone(), self.name.clone());
					let state = request.peek();
					request.reject(err);
					state
				}
			},
		};
		let preferences = subscription.map(|value| Preferences::register(state.read(), value.clone()));
		(state.consume(), preferences)
	}

	/// Run `f` on the track instance this handle reads now, without joining its consumer
	/// count: `None` when an open broadcast serves nothing under the name now.
	fn with_state<R>(&self, f: impl FnOnce(Option<&kio::ConsumerWeak<TrackState>>) -> R) -> R {
		match &self.source {
			Source::Track(state) => f(Some(state)),
			Source::Broadcast(broadcast, current) => f(broadcast.peek_track(&self.name, current.as_ref()).as_ref()),
		}
	}

	/// Attach an egress stats scope, inherited by the subscriptions, fetches, and
	/// groups derived from this handle. Called by a tagged [`broadcast::Consumer`].
	pub(super) fn with_stats(mut self, scope: stats::Scope) -> Self {
		self.stats = scope;
		self
	}

	/// The track name this handle is bound to.
	pub fn name(&self) -> &str {
		&self.name
	}

	/// The broadcast this track belongs to, as reached through the handle it came from.
	/// Its [`path`](broadcast::Info::path) is what a catalog's relative `broadcast`
	/// references resolve against.
	pub fn broadcast(&self) -> &broadcast::Info {
		&self.broadcast
	}

	/// Open a live subscription.
	///
	/// Registers the subscription on the track and returns a [`kio::Pending`] that resolves to the
	/// [`Subscriber`] once the track info is available, or the track's abort error (or
	/// [`Error::Dropped`]) if it is already closed.
	///
	/// Reads only the groups the subscription's ranges cover.
	/// [`Subscription::max_delay`] is what asks for data: delivery skips everything in range
	/// that the budget convicts, so the default budget of zero delivers only the latest
	/// group and a larger one reaches back over what it can still use.
	pub fn subscribe(&self, subscription: impl Into<Option<Subscription>>) -> kio::Pending<Subscribing> {
		let (state, subscription) = self.resolve(Some(&subscription.into().unwrap_or_default()));
		let subscription = subscription.expect("a subscription was requested");

		kio::Pending::new(Subscribing {
			name: self.name.clone(),
			broadcast: self.broadcast.clone(),
			state,
			subscription,
			stats: self.stats.clone(),
		})
	}

	/// Poll for the first group the live feed serves, once the serving session has
	/// resolved it: `Some` for a declared start, `None` for none (the live edge, or a
	/// source that never declares one). Parks while a lite-06+ session still owes its
	/// SUBSCRIBE_START (see [`Producer::request_start`]); a closed track is ready with
	/// whatever it last declared, since nothing will resolve it anymore.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	fn poll_state_start(state: &kio::Consumer<TrackState>, waiter: &kio::Waiter) -> Poll<Option<u64>> {
		let res = state.poll(waiter, |state| match state.start_pending && state.abort.is_none() {
			true => Poll::Pending,
			false => Poll::Ready(state.start_sequence),
		});
		match ready!(res) {
			Ok(start) => Poll::Ready(start),
			Err(state) => Poll::Ready(state.start_sequence),
		}
	}

	/// Refuse a new range before it changes the demand shared by this route's readers.
	fn refuse_subscription(&self, subscription: &Subscription) -> Result<()> {
		if let Some(serving) = self.serving() {
			return serving.refuse_subscription(subscription);
		}
		self.with_state(|state| match state {
			Some(state) => refuse_cached_anchors(&state.read(), subscription),
			None => Ok(()),
		})
	}

	/// The upstream publisher's Largest Object, independently of which groups are cached.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn reported_object(&self) -> Option<(u64, u64)> {
		if let Some(serving) = self.serving() {
			return serving.reported_object();
		}
		self.with_state(|state| state?.read().object_edge())
	}

	/// The newest group, when it is already cached: resolved synchronously, without
	/// counting as a fetch or a delivery. The IETF publisher snapshots its frame count to
	/// resolve Largest Object; a group that is not immediately available reads as no edge.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn peek_latest(&self) -> Option<group::Consumer> {
		if let Some(serving) = self.serving() {
			return serving.peek_latest();
		}
		let sequence = self.with_state(|state| state?.read().max_sequence)?;
		self.peek_group(sequence)
	}

	/// The nearest cached group below `sequence`, under the same terms as
	/// [`Self::peek_group`]. Walks the cache's own order, so gaps in the group numbering
	/// are crossed and aborted (evicted) entries are skipped.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn peek_before(&self, sequence: u64) -> Option<group::Consumer> {
		if let Some(serving) = self.serving() {
			return serving.peek_before(sequence);
		}
		self.with_state(|state| {
			state?
				.read()
				.lookup
				.range(..sequence)
				.rev()
				.find(|(_, slot)| slot.serves_end())
				.map(|(_, slot)| slot.serving().consume())
		})
	}

	/// A cached group by sequence, under the same terms as [`Self::peek_latest`]. Unlike a
	/// fetch, a peek does not refresh the group's cache standing, so it never keeps a
	/// group alive over one a subscriber actually read; an aborted (evicted) group, or a
	/// copy cut short at the back, is a miss.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn peek_group(&self, sequence: u64) -> Option<group::Consumer> {
		if let Some(serving) = self.serving() {
			return serving.peek_group(sequence);
		}
		self.with_state(|state| {
			let state = state?.read();
			let slot = state.lookup.get(&sequence).filter(|slot| slot.serves_end())?;
			Some(slot.serving().consume())
		})
	}

	/// The live arrivals of the track this handle reads now, without joining its demand.
	// This batch view avoids per-fetch ledger copies and locks in the session.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn arrivals(&self) -> Option<Arrivals> {
		self.with_state(|state| state.map(|state| Arrivals { state: state.clone() }))
	}

	/// Poll for a cached group, a terminal disposition, or a gap outliving its grace: the
	/// group, the error a dropped or cut group was given up with, or `None` once it is gone.
	pub(crate) fn poll_group(&self, sequence: u64, waiter: &kio::Waiter) -> Poll<Result<Option<group::Consumer>>> {
		self.with_state(|state| {
			let Some(state) = state else {
				return Poll::Ready(Ok(None));
			};
			let res = state.poll(waiter, |state| {
				let max_delay = clamp_max_delay(state.subscriptions.lock().max_delay(), state.max_age_bound());
				let grace = ledger::grace(max_delay);
				let now = state.cache.pool().now_wall();
				let res = state.poll_arrival(sequence, now, grace, waiter);
				if res.is_pending() {
					let Some(at) = state.arrivals.folds_at(sequence, grace) else {
						// No later arrival yet: only this wait uses the track's change channel.
						return Poll::Pending;
					};
					state.cache.watch_wall(at, waiter);
				}
				// A known gap waits on its own arrival and deadline, not unrelated writes.
				Poll::Ready(res)
			});
			let res = match res {
				Poll::Ready(Ok(res)) => res,
				Poll::Ready(Err(_)) => Poll::Ready(Ok(None)),
				Poll::Pending => Poll::Pending,
			};
			if res.is_pending() && state.poll_closed(waiter).is_ready() {
				// A cached arrival may have raced closure after the first lookup.
				let state = state.read();
				return Poll::Ready(match state.landing(sequence) {
					Poll::Ready(Some(group)) => Ok(Some(group)),
					_ => state.given_up(sequence).map_or(Ok(None), Err),
				});
			}
			res
		})
	}

	/// Lend `head` to the cached copies of group `sequence`; see
	/// [`group::Handle::adopt_head`].
	pub(super) fn adopt_head(&self, sequence: u64, head: &group::Head) {
		let copies: Vec<group::Handle> = self.with_state(|state| {
			let Some(state) = state else {
				return Vec::new();
			};
			let state = state.read();
			state
				.lookup
				.get(&sequence)
				.map_or_else(Vec::new, |slot| slot.copies().cloned().collect())
		});
		// With the track lock released (lock order is track then group): adopting settles
		// the group's cache debt.
		for group in copies {
			group.adopt_head(head);
		}
	}

	/// Poll for group `sequence` falling a full `budget`, clamped to this track's retention,
	/// behind its live edge, as a reader would judge it, whether the track holds it or not;
	/// see `TrackState::drifted`. Parks in this track's expiry index, so only a change that
	/// can make the group stale, or land it here, wakes it. Pending for good once the track closes.
	pub(super) fn poll_stale(&self, sequence: u64, budget: Duration, waiter: &kio::Waiter) -> Poll<()> {
		self.with_state(|state| {
			let Some(state) = state else {
				return Poll::Pending;
			};
			let state = state.read();
			let budget = clamp_max_delay(budget, state.max_age_bound());
			state.cache.wakes().watch_held(sequence, waiter);
			match state.poll_drifted(sequence, &Servable::ALL, budget, None, waiter) {
				true => Poll::Ready(()),
				false => Poll::Pending,
			}
		})
	}

	/// Whether a group in `groups` may still land here; see `TrackState::may_land`. Never
	/// once no producer is left to write one.
	pub(super) fn may_land(
		&self,
		groups: ops::Range<u64>,
		floor: bool,
		grace: Duration,
		delivered: Option<&Delivered>,
	) -> bool {
		if let Some(serving) = self.serving() {
			return serving.may_land(groups, floor, grace, delivered);
		}
		self.with_state(|state| {
			state.is_some_and(|state| !state.is_closed() && state.read().may_land(groups, floor, grace, delivered))
		})
	}

	/// The wall clock this track's cache judges ages by.
	pub(super) fn now(&self) -> crate::time::Instant {
		if let Some(serving) = self.serving() {
			return serving.now();
		}
		self.with_state(|state| match state {
			Some(state) => state.read().cache.pool().now_wall(),
			None => super::clock::wall(),
		})
	}

	/// Poll for this track's wall clock reaching `at`; see [`Self::now`].
	pub(super) fn poll_wall(&self, at: crate::time::Instant, waiter: &kio::Waiter) -> Poll<()> {
		if let Some(serving) = self.serving() {
			return serving.poll_wall(at, waiter);
		}
		self.with_state(|state| {
			let Some(state) = state else {
				return Poll::Ready(());
			};
			let state = state.read();
			if state.cache.pool().now_wall() >= at {
				return Poll::Ready(());
			}
			state.cache.watch_wall(at, waiter);
			Poll::Pending
		})
	}

	/// The group a front's parked copy would serve for `sequence` from `frame_start`, when it
	/// is whole: finished from frame 0 on a copy still open. Anything less (open, headless,
	/// aborted, or on a failed copy) needs the route. The fetch joins the copy's demand, so
	/// a copy kept busy only by fetches stays cached at its session.
	fn parked_whole(&self, sequence: u64, frame_start: u64) -> Option<(kio::Consumer<TrackState>, group::Consumer)> {
		let copy = self.with_state(|state| state?.read().routes.as_ref()?.parked())?;
		copy.with_state(|state| {
			let state = state?;
			let group = {
				let track = state.read();
				let group = track.covering_group(sequence, frame_start)?;
				let whole = !track.is_closed()
					&& track.abort.is_none()
					&& group.live_first_frame() == Some(0)
					&& group.is_finished();
				if !whole {
					return None;
				}
				let consumer = group.try_consume()?;
				group.cache_refresh();
				consumer
			};
			Some((state.consume(), group))
		})
	}

	/// The serving route's copy, for a front's logical track; see [`super::resume`].
	fn serving(&self) -> Option<Consumer> {
		let routes = self.with_state(|state| state?.read().routes.clone())?;
		routes.serving()
	}

	/// Join the instance this handle reads at the live edge; see [`Subscriber::join`].
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	fn join(&self) {
		if let Some(serving) = self.serving() {
			return serving.join();
		}
		self.with_state(|state| {
			if let Some(state) = state {
				TrackState::join(state.read());
			}
		});
	}

	/// Whether the cache is fed live, so the newest object it holds is the track's largest.
	/// Fetch-only, closed, and parked copies are not, even once they learn the end.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn is_live(&self) -> bool {
		if let Some(serving) = self.serving() {
			return serving.is_live();
		}
		// A front's track with no copy spliced is not fed at all.
		self.with_state(|state| {
			state.is_some_and(|state| {
				let state = state.read();
				state.routes.is_none() && state.published && !state.closed && state.feed.is_live()
			})
		})
	}

	/// Fetching a single past group, without holding a live subscription.
	///
	/// Returns a [`kio::Pending`] that resolves to the [`group::Consumer`]:
	/// immediately if the group is cached, otherwise once a [`Requests`] serves
	/// the request (a wire FETCH for a relay). `options` accepts `None`, a [`group::Fetch`],
	/// or `group::Fetch::default()`.
	///
	/// The returned future resolves to [`Error::NotFound`] when the group can never be served
	/// (past the final sequence, or no [`Requests`] on the track), [`Error::NotFetchable`]
	/// instead when this track sent the sequence as a datagram still buffered, the handler's
	/// rejection (a relay's upstream miss is [`StreamError::NotFound`](StreamError::NotFound)),
	/// or the track's abort error if it's already closed. A miss queues a one-group [`Fill`],
	/// which concurrent fetches of the same sequence join.
	pub fn fetch_group(&self, sequence: u64, options: impl Into<Option<group::Fetch>>) -> kio::Pending<Fetching> {
		self.fetching(
			sequence,
			FetchOptions {
				options: options.into().unwrap_or_default(),
				end: None,
			},
		)
	}

	/// Fill a wire request, widening a cold or compressed group to a reusable prefix.
	pub(crate) fn fetch_range(&self, sequence: u64, mut request: FetchOptions) -> kio::Pending<Fetching> {
		let state = self.resolve(None).0;
		{
			let state = state.read();
			if state.lookup.get(&sequence).is_none_or(Slot::is_aborted)
				|| state.info.as_ref().is_none_or(|info| info.compress)
			{
				request.options.frame_start = 0;
			}
			// A compressed stream needs the entire dictionary; an unknown track is conservative
			// until TRACK_INFO has told the relay which representation it will receive.
			if state.info.as_ref().is_none_or(|info| info.compress) {
				request.end = None;
			}
		}
		self.fetching(sequence, request)
	}

	fn fetching(&self, sequence: u64, request: FetchOptions) -> kio::Pending<Fetching> {
		let FetchOptions {
			options,
			end: frame_end,
		} = request;
		let frames = group::Bounds {
			start: options.frame_start,
			end: frame_end,
		};

		// One fetch per calling context, counted here (coalesced upstream work is
		// still one request served). Independent of `subscriptions` and the viewer
		// refcount.
		self.stats.fetch();

		// Nothing more feeds a whole group, so no route needs to stay up for it: the copy a
		// front parked serves it without reviving the logical track for one read.
		if let Some((state, group)) = self.parked_whole(sequence, options.frame_start) {
			let fills = state.read().fills.clone();
			return kio::Pending::new(Fetching {
				state,
				fills,
				sequence,
				frame_start: options.frame_start,
				frame_end,
				priority: options.priority,
				max_delay: options.max_delay,
				result: None,
				hit: Some(Box::new(group)),
				stats: self.stats.clone(),
				resume: None,
			});
		}

		let state = self.resolve(None).0;
		let resume = state.read().routes.clone();
		let mut result = None;

		// A front's logical track caches nothing: the serving route answers.
		if let Some(resume) = resume {
			return kio::Pending::new(Fetching {
				state: state.clone(),
				fills: state.read().fills.clone(),
				sequence,
				frame_start: options.frame_start,
				frame_end,
				priority: options.priority,
				max_delay: options.max_delay,
				result: None,
				hit: None,
				stats: self.stats.clone(),
				resume: Some(Box::new(resume.fetch_group(
					sequence,
					FetchOptions {
						options,
						end: frame_end,
					},
					state.clone(),
				))),
			});
		}

		// Queue a request only when the group isn't already resolvable from the track
		// (cached, aborted, or past-final all resolve through `Fetching::poll` without
		// a queue entry).
		let (fills, cached) = {
			let state = state.read();
			(state.fills.clone(), state.poll_fetch_cached(sequence, frames))
		};
		let unresolved = cached.is_pending();
		// Hold a hit until the caller polls: the held consumer keeps the group wanted,
		// so an abandoned fetch filling it can't abort it in between.
		let hit = match cached {
			Poll::Ready(Ok(group)) => Some(Box::new(group)),
			_ => None,
		};

		if unresolved {
			result = queue(
				&fills,
				sequence,
				&FetchOptions {
					options: options.clone(),
					end: frame_end,
				},
			);
		}

		kio::Pending::new(Fetching {
			state,
			fills,
			sequence,
			frame_start: options.frame_start,
			frame_end,
			priority: options.priority,
			max_delay: options.max_delay,
			result,
			hit,
			stats: self.stats.clone(),
			resume: None,
		})
	}

	/// The sequences of `span` the cache holds, lowest first.
	#[cfg(feature = "async")]
	pub(crate) fn cached_groups(&self, span: ops::Range<u64>) -> Vec<u64> {
		if let Some(serving) = self.serving() {
			return serving.cached_groups(span);
		}
		self.with_state(|state| {
			let Some(state) = state else {
				return Vec::new();
			};
			let state = state.read();
			state
				.lookup
				.range(span)
				.filter(|(_, slot)| slot.serves_end())
				.map(|(sequence, _)| *sequence)
				.collect()
		})
	}

	/// Resolve the track's [`Info`] without subscribing.
	///
	/// A [`Consumer`] is a lazy handle, so the info may not be known yet: this waits
	/// for the producer to [`Request::accept`] the track (a wire TRACK_INFO round-trip
	/// for a relay), and errors with the track's abort error if it closes first.
	/// [`Subscriber::info`] is the already-resolved counterpart.
	pub fn info(&self) -> kio::Pending<Resolving> {
		kio::Pending::new(Resolving {
			name: self.name.clone(),
			broadcast: self.broadcast.clone(),
			state: self.resolve(None).0,
			stats: self.stats.clone(),
		})
	}

	/// Poll for the track's largest location: the current one, then each newer one, and `None` once the track ends.
	pub fn poll_largest(&mut self, waiter: &kio::Waiter) -> Poll<Option<Position>> {
		let mut seen = self.largest;
		let next = self.poll_largest_after(&mut seen, Bounds::default(), waiter);
		self.largest = seen;
		next
	}

	/// Poll for a largest frame within `bounds` newer than `seen`, recording it there. A
	/// front's logical track follows the route serving it. Reads the instance served now
	/// without asking for it, so a broadcast serving nothing under the name ends the stream.
	pub(super) fn poll_largest_after(
		&self,
		seen: &mut Option<Position>,
		bounds: Bounds,
		waiter: &kio::Waiter,
	) -> Poll<Option<Position>> {
		self.with_state(|state| {
			let Some(state) = state else {
				return Poll::Ready(None);
			};
			let (routes, cache) = {
				let state = state.read();
				(state.routes.clone(), state.cache.clone())
			};
			match routes {
				Some(routes) => routes.poll_largest(seen, bounds, waiter),
				None => poll_largest(
					&cache,
					seen,
					bounds,
					waiter,
					|waiter| state.poll(waiter, |state| largest_live(state, waiter)).map(|_| ()),
					|waiter| state.poll(waiter, |state| largest_ended(state, waiter)).map(|_| ()),
				),
			}
		})
	}

	/// The track's largest location, its newest group or datagram at the newest frame (0 while empty): the current one, then each newer one, and `None` once the track ends.
	#[cfg(feature = "async")]
	pub async fn largest(&mut self) -> Option<Position> {
		kio::wait(|waiter| self.poll_largest(waiter)).await
	}

	pub(crate) fn latest_sequence(&self) -> Option<u64> {
		if let Some(serving) = self.serving() {
			return serving.latest_sequence();
		}
		self.with_state(|state| state?.read().max_sequence)
	}

	/// The first group the declared end excludes whole, or `None` while the track is open ended.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn final_sequence(&self) -> Option<u64> {
		if let Some(serving) = self.serving() {
			return serving.final_sequence();
		}
		self.with_state(|state| state?.read().final_sequence())
	}

	/// The frame-precise point a replacement route should resume from: one past the
	/// last frame this copy produced. `None` if it produced nothing.
	///
	/// Survives the track aborting, which is when a route change asks.
	#[cfg(test)]
	pub(crate) fn resume_position(&self) -> Option<Position> {
		self.with_state(|state| state?.read().resume_position())
	}

	/// Poll for the track closing: no producer is left to write it, so nothing more
	/// will ever arrive. A name the broadcast serves nothing under reads as closed.
	pub(super) fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<()> {
		self.with_state(|state| state.map_or(Poll::Ready(()), |state| state.poll_closed(waiter)))
	}

	/// Poll for the track reaching a terminal state: `Ok(())` once it is complete
	/// (the final group was produced), `Err` once it closed or aborted before
	/// completing. This tells a track that truly ended from one whose serving route
	/// died mid-stream.
	pub(super) fn poll_complete(&self, waiter: &kio::Waiter) -> Poll<Result<()>> {
		self.with_state(|state| {
			let Some(state) = state else {
				return Poll::Ready(Err(Error::Dropped));
			};
			match ready!(state.poll(waiter, |state| {
				if state.is_complete() {
					Poll::Ready(())
				} else {
					state.watch_tail(waiter);
					Poll::Pending
				}
			})) {
				Ok(_) => Poll::Ready(Ok(())),
				// Closed before completing. Read through the returned guard: it holds
				// the lock, so re-locking the channel here would deadlock.
				Err(closed) => Poll::Ready(Err(closed.abort.clone().unwrap_or(Error::Dropped))),
			}
		})
	}
}

/// The pollable state of a [`Consumer::subscribe`]; awaited via the
/// [`kio::Pending`] wrapper, whose `DerefMut` exposes [`Self::update`].
pub struct Subscribing {
	name: Arc<str>,
	broadcast: Arc<broadcast::Info>,
	state: kio::Consumer<TrackState>,
	subscription: Preferences,
	stats: stats::Scope,
}

impl Subscribing {
	/// Resolve the same track's info with this subscription already registered.
	pub(super) fn info(&self) -> Resolving {
		Resolving {
			name: self.name.clone(),
			broadcast: self.broadcast.clone(),
			state: self.state.clone(),
			stats: self.stats.clone(),
		}
	}

	/// Poll until the peer confirms the subscription, yielding the [`Subscriber`].
	/// Errors if the track is aborted or not found.
	pub fn poll_ok(&self, waiter: &kio::Waiter) -> Poll<Result<Subscriber>> {
		// Wait until the track info is available
		let info = ready!(self.state.poll(waiter, |state| state.poll_info()))
			.map_err(|e| e.abort.clone().unwrap_or(Error::Dropped))??;
		refuse_untimed(&info, &self.subscription.read())?;
		Consumer::new(self.name.clone(), self.state.weak()).refuse_subscription(&self.subscription.read())?;

		let resume = self.state.read().routes.clone();
		let inner = match resume {
			Some(resume) => Inner::Resume(
				Box::new(resume.subscribe(self.subscription.clone())),
				self.state.clone(),
			),
			None => Inner::Plain(Cursor::new(self.state.clone(), self.subscription.clone())),
		};
		Poll::Ready(Ok(Subscriber {
			name: self.name.clone(),
			broadcast: self.broadcast.clone(),
			info,
			inner,
			delivered: Delivered::default(),
			stats: self.stats.clone(),
			_stats_sub: self.stats.subscribe(),
			largest: Reported::default(),
		}))
	}

	/// Change the subscription preferences before (or after) it resolves.
	///
	/// Returns [`Error::Closed`] if the track already ended; the update is
	/// meaningless at that point and can usually be ignored.
	pub fn update(&mut self, subscription: Subscription) -> Result<()> {
		if subscription.ranges != self.subscription.read().ranges {
			Consumer::new(self.name.clone(), self.state.weak()).refuse_subscription(&subscription)?;
		}
		self.subscription.update(subscription)
	}
}

impl kio::Task for Subscribing {
	type Output = Result<Subscriber>;

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Self::Output> {
		self.poll_ok(waiter)
	}
}

/// The pollable state of a [`Consumer::info`]; awaited via the
/// [`kio::Pending`] wrapper.
///
/// Counts as the track's [`Demand`] while held, answered or not.
pub struct Resolving {
	name: Arc<str>,
	broadcast: Arc<broadcast::Info>,
	state: kio::Consumer<TrackState>,
	stats: stats::Scope,
}

impl Resolving {
	/// A handle on the track instance this resolves, whatever its broadcast serves later.
	pub fn track(&self) -> Consumer {
		Consumer {
			name: self.name.clone(),
			broadcast: self.broadcast.clone(),
			source: Source::Track(self.state.weak()),
			stats: self.stats.clone(),
			largest: None,
		}
	}

	/// Poll until the track's [`Info`] is known, without subscribing to its groups.
	pub fn poll_ok(&self, waiter: &kio::Waiter) -> Poll<Result<Info>> {
		// Wait until the track info is available
		let info = ready!(self.state.poll(waiter, |state| state.poll_info()))
			.map_err(|e| e.abort.clone().unwrap_or(Error::Dropped))??;
		Poll::Ready(Ok(info))
	}
}

impl kio::Task for Resolving {
	type Output = Result<Info>;

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Self::Output> {
		self.poll_ok(waiter)
	}
}

/// Join the fill covering `sequence`, or queue a one-group fill; `None` when no handler
/// exists to take one, or for the last sequence, which a fill's exclusive end can't name.
fn queue(fills: &kio::Shared<FillState>, sequence: u64, request: &FetchOptions) -> Option<Box<fill::Join>> {
	let options = &request.options;
	let end = sequence.checked_add(1)?;
	let ask = fill::Ask {
		start: Position {
			group: sequence,
			frame: options.frame_start,
		},
		end,
		frame_end: request.end,
		asking: fill::Asking {
			priority: options.priority,
			max_delay: options.max_delay,
			// One group either way, so it never turns a wider fill it joins newest first.
			order: Order::Ascending,
		},
	};
	// The handler gate is atomic with a handler dropping (no fill stranded on a queue
	// nobody drains); with no handler, `Fetching::poll` fails fast.
	let (joins, _) = fills.lock().join(fills, &ask);
	joins.into_iter().next().map(Box::new)
}

/// One transport request, whose end may cut the stored copy short.
#[derive(Clone)]
pub(crate) struct FetchOptions {
	pub options: group::Fetch,
	pub end: Option<u64>,
}

/// The pollable state of a [`Consumer::fetch_group`].
///
/// Awaited via the [`kio::Pending`] wrapper; resolves to the
/// [`group::Consumer`] once the group lands in the track's cache (already present,
/// or produced after a wire FETCH), or [`Error::NotFound`] if it can never exist.
pub struct Fetching {
	state: kio::Consumer<TrackState>,
	fills: kio::Shared<FillState>,
	sequence: u64,
	// This caller's own start, so a cached group that begins above it is a miss
	// rather than a short answer.
	frame_start: u64,
	frame_end: Option<u64>,
	// This caller's priority, for a fetch that moves to a front's routes.
	priority: i8,
	// This caller's own age cap. None means only the publisher's max age.
	max_delay: Option<Duration>,
	// The fill this fetch joined; `None` on a cache hit, or when no handler existed to
	// queue on. Boxed: a miss already allocates, and the handle stays small.
	result: Option<Box<fill::Join>>,
	// The group already cached when the fetch was made, held so it stays wanted.
	// Boxed: a group consumer dwarfs the rest of the handle.
	hit: Option<Box<group::Consumer>>,
	// Egress stats scope, so the resolved group carries a payload meter (and counts
	// as one delivered group). Empty (no-op) for an untagged track.
	stats: stats::Scope,
	// A front's logical track: fetched from whichever route serves it. Boxed: rare.
	resume: Option<Box<super::resume::Fetching>>,
}

impl kio::Task for Fetching {
	type Output = Result<group::Consumer>;

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Self::Output> {
		// Made before a front accepted its logical track, which caches nothing: the
		// serving route answers instead.
		if self.resume.is_none()
			&& let Some(routes) = self.state.read().routes.clone()
		{
			let options = group::Fetch {
				priority: self.priority,
				max_delay: self.max_delay,
				frame_start: self.frame_start,
			};
			self.resume = Some(Box::new(routes.fetch_group(
				self.sequence,
				FetchOptions {
					options,
					end: self.frame_end,
				},
				self.state.clone(),
			)));
		}
		if let Some(resume) = &mut self.resume {
			let group = ready!(kio::Task::poll(&mut **resume, waiter))?;
			return Poll::Ready(Ok(group.with_meter(self.stats.meter())));
		}
		let (state, fills, sequence, frame_start, result) = (
			&self.state,
			&self.fills,
			self.sequence,
			self.frame_start,
			self.result.as_ref(),
		);
		let cache = {
			let state = state.read();
			let budget = clamp_max_delay(self.max_delay.unwrap_or(Duration::MAX), state.max_age_bound());
			let _ = state.cache.wakes().watch_landing(sequence, waiter);
			if state.poll_drifted(sequence, &Servable::ALL, budget, None, waiter) {
				return Poll::Ready(Err(Error::Old));
			}
			state.cache.clone()
		};

		// Hand back a consumer sitting where the caller asked rather than at the group's
		// own start. Coverage was checked first, so this can only skip frames they excluded.
		let expiry: Arc<dyn group::Expiry> = Arc::new(FetchExpiry {
			state: state.weak(),
			cache,
			reading: Reading::new(state.clone()),
			sequence,
			max_delay: self.max_delay,
			superseded: Superseded::default(),
		});
		let resolve = |group: group::Consumer| {
			group
				.with_range(frame_start..)
				.with_end(self.frame_end)
				.with_meter(self.stats.meter())
				.with_expiry(expiry.clone())
		};
		if let Some(group) = &self.hit {
			return Poll::Ready(Ok(resolve((**group).clone())));
		}

		// Track side: the cached group, the abort error, or past-final. The outer
		// error is the channel closing without any of those.
		let frames = group::Bounds {
			start: frame_start,
			end: self.frame_end,
		};
		let cached = |waiter: &kio::Waiter| match state.poll(waiter, |state| state.poll_fetch_cached(sequence, frames))
		{
			Poll::Ready(Ok(res)) => Poll::Ready(res.map(resolve)),
			Poll::Ready(Err(closed)) => Poll::Ready(Err(closed.abort.clone().unwrap_or(Error::Dropped))),
			Poll::Pending => Poll::Pending,
		};
		if let Poll::Ready(res) = cached(waiter) {
			return Poll::Ready(res);
		}

		// Handler side.
		let Some(result) = result else {
			// A fill's end is exclusive, so no handler can be asked for the last sequence:
			// only a local write, already looked for above, can hold it.
			if sequence == u64::MAX {
				return Poll::Ready(Err(Error::NotFound));
			}
			// Never queued: no handler existed when the fetch was made. Fail fast while
			// that's still true; a handler that appeared since may yet fill the cache.
			// A sequence this track sent as a datagram says so, rather than a plain miss.
			return match fills.poll(waiter, |fills| match fills.has_handlers() {
				false => Poll::Ready(()),
				true => Poll::Pending,
			}) {
				Poll::Ready(_guard) => Poll::Ready(Err(match state.read().holds_datagram(sequence) {
					true => Error::NotFetchable,
					false => Error::NotFound,
				})),
				Poll::Pending => Poll::Pending,
			};
		};

		// An answered fill fails a joined fetch the cache cannot cover. The channel
		// closing without an answer means its handlers went away.
		let (err, asked, partial) = match result
			.outcome()
			.poll(waiter, |outcome| match outcome.refused(sequence) {
				Some((err, asked)) => Poll::Ready((err.clone(), asked, None)),
				None if outcome.is_accepted(sequence) => {
					Poll::Ready((Error::NotFound, None, outcome.partial(sequence, frames)))
				}
				None => Poll::Pending,
			}) {
			Poll::Ready(Ok(res)) => res,
			Poll::Ready(Err(_closed)) => (Error::NotFound, None, None),
			Poll::Pending => return Poll::Pending,
		};
		// An accept caches the group before it answers the channel, which may land
		// between the track check above and this one: look again before failing.
		if let Poll::Ready(res) = cached(waiter) {
			return Poll::Ready(res);
		}
		// A later copy displaced the one accepted for this fetch from the cache.
		if let Some(group) = partial.as_ref().and_then(group::Handle::try_consume) {
			return Poll::Ready(Ok(resolve(group)));
		}
		// Joined after a tighter budget went on the wire, which judged the group too old
		// for that budget, not ours: ask again with ours. Terminates, since the new
		// fill asks at least our budget.
		let looser = match (asked, self.max_delay) {
			(Some(asked), Some(ours)) => ours > asked,
			(Some(_), None) => true,
			(None, _) => false,
		};
		if matches!(err, Error::Old) && looser {
			let options = group::Fetch {
				priority: self.priority,
				max_delay: self.max_delay,
				frame_start: self.frame_start,
			};
			self.result = queue(
				&self.fills,
				self.sequence,
				&FetchOptions {
					options,
					end: self.frame_end,
				},
			);
			return self.poll(waiter);
		}
		Poll::Ready(Err(err))
	}
}

/// A live subscription to a track, used to read its groups.
///
/// Created via [`Consumer::subscribe`](Consumer::subscribe), or
/// directly from a [`Producer`] for an in-process track. Carries this
/// subscriber's [`Subscription`] preferences, which feed the producer's aggregate.
///
/// # Local cursor vs wire preference
///
/// Group bounds exist at two levels:
///
/// - [`Subscription::ranges`], set via [`Self::update`], are both a **request to the
///   publisher** and **this subscriber's read filter**. The publisher sees the union across
///   every live subscriber, but each subscriber reads only the groups its own ranges cover,
///   resolved where it is served: a relative anchor counted back from the latest group (or
///   the first to arrive, before any exists), again whenever the ranges are replaced.
/// - [`Self::with_range`] narrows **this subscriber's reads** further, without changing the
///   publisher's demand.
///
/// A group outside either is held rather than skipped past, so widening either one later
/// makes it available again. Within both, [`Subscription::max_delay`] decides what is
/// still worth delivering, measured against the track's live edge.
pub struct Subscriber {
	name: Arc<str>,
	// The broadcast this track belongs to; see [`Self::broadcast`].
	broadcast: Arc<broadcast::Info>,
	info: Info,
	inner: Inner,
	/// The groups handed out, so none is handed out twice: not when its sequence is
	/// re-created, nor when another route delivers it too.
	delivered: Delivered,
	// Egress stats scope, used to meter the groups this subscriber reads. Empty
	// (no-op) for an untagged track.
	stats: stats::Scope,
	// The subscription guard: bumps `subscriptions` (and the egress viewer refcount)
	// while held, closing them on drop. Empty (no-op) for an untagged track.
	_stats_sub: stats::Subscription,
	largest: Reported,
}

/// What a [`Subscriber`]'s `largest` stream last reported.
#[derive(Default)]
struct Reported {
	position: Option<Position>,
	// The stream ended: moving the bounds afterwards does not bring it back.
	ended: bool,
	// The parked poll, woken when the bounds move. Its registration dies with its waiter,
	// so a dropped `largest()` future leaves nothing behind.
	parked: kio::WaiterList,
}

impl Reported {
	/// The bounds moved: wake a parked poll, since the largest may now be within them.
	fn bounds_moved(&mut self) {
		self.parked.wake();
	}
}

/// Unread groups a logical reader can still take from a replaced copy.
pub(super) struct Unread<'a> {
	pub start: u64,
	pub end: Option<u64>,
	pub delivered: &'a Delivered,
}

/// How a [`Subscriber`] reads: its track's own cache, or a front's routes.
enum Inner {
	Plain(Cursor),
	/// A front's logical track, read straight from its routes' copies; see
	/// [`super::resume`]. Holds the logical track so it counts as a reader.
	/// Boxed: the plain cursor is the hot path.
	Resume(
		Box<super::resume::Subscriber>,
		#[allow(dead_code, reason = "held so the logical track counts as a reader")] kio::Consumer<TrackState>,
	),
}

/// One poll's view of how far this subscription may drift: the clamped budget and the
/// live edge to measure a candidate group against. Resolved once, then applied to every
/// group that poll considers.
#[derive(Clone)]
struct Drift {
	budget: Duration,
	/// The budget is the track's max age bound, which no widening passes: its verdicts are final.
	bounded: bool,
	edge: Edge,
}

/// Keeps one handed-out group tied to the subscription whose cursor selected it.
struct GroupExpiry {
	state: kio::ConsumerWeak<TrackState>,
	/// The track's cache account, whose wakes judge a group with nothing above it without
	/// locking `state`.
	cache: Arc<cache::Track>,
	reading: Reading,
	subscription: kio::Consumer<Subscription>,
	/// What the cursor can be served; see [`Cursor::published`].
	servable: kio::Consumer<Published>,
	sequence: u64,
	superseded: Superseded,
}

/// Keeps one fetched group tied to this caller's age budget.
struct FetchExpiry {
	state: kio::ConsumerWeak<TrackState>,
	/// See [`GroupExpiry::cache`].
	cache: Arc<cache::Track>,
	reading: Reading,
	sequence: u64,
	max_delay: Option<Duration>,
	superseded: Superseded,
}

/// The track demand a handed-out group holds for its readers, and the frames they take,
/// until one of them reads the group's end: from then on the track owes it nothing.
struct Reading(Mutex<Option<kio::Consumer<TrackState>>>);

impl Reading {
	fn new(track: kio::Consumer<TrackState>) -> Self {
		Self(Mutex::new(Some(track)))
	}

	/// The same hold on the copy a reader moved to, unless the group already ended.
	fn moved(&self, track: &kio::ConsumerWeak<TrackState>) -> Self {
		let held = self.0.lock().unwrap_or_else(PoisonError::into_inner).is_some();
		Self(Mutex::new(held.then(|| track.consume())))
	}

	fn release(&self) {
		let track = self.0.lock().unwrap_or_else(PoisonError::into_inner).take();
		// Dropped outside the lock: the last reader leaving wakes the track's watchers.
		drop(track);
	}
}

/// The successor a handed-out group was last judged by, so the group stays judgeable once
/// the cache evicts both; see [`TrackState::superseded_at`]. Each reader keeps its own, so
/// the memory is bounded by the groups readers hold.
#[derive(Default)]
struct Superseded(Mutex<Option<Successor>>);

/// A successor, when it arrived, and whether it has since failed.
struct Successor {
	sequence: u64,
	arrived: crate::time::Instant,
	fate: group::Fate,
}

impl group::Expiry for FetchExpiry {
	fn for_track(&self, track: &Consumer) -> Arc<dyn group::Expiry> {
		// A replacement copy is always one track instance; the fallback is never read.
		let state = track.with_state(|state| state.cloned());
		let (state, cache) = match state {
			Some(state) => {
				let cache = state.read().cache.clone();
				(state, cache)
			}
			None => (self.state.clone(), self.cache.clone()),
		};
		Arc::new(Self {
			reading: self.reading.moved(&state),
			state,
			cache,
			sequence: self.sequence,
			max_delay: self.max_delay,
			superseded: Superseded::default(),
		})
	}

	fn ended(&self) {
		self.reading.release();
	}

	fn is_expired(&self, max_delay: Option<Duration>, waiter: &kio::Waiter) -> bool {
		// Parked first, so a group landing above while this judges wakes it.
		if !self.cache.wakes().watch_landing(self.sequence, waiter) {
			return false;
		}
		let state = self.state.read();
		let budget = match (self.max_delay, max_delay) {
			(Some(fetch), Some(route)) => fetch.min(route),
			(Some(fetch), None) => fetch,
			(None, Some(route)) => route,
			(None, None) => Duration::MAX,
		};
		let budget = clamp_max_delay(budget, state.max_age_bound());
		state.poll_drifted(self.sequence, &Servable::ALL, budget, Some(&self.superseded), waiter)
	}
}

impl group::Expiry for GroupExpiry {
	fn for_track(&self, track: &Consumer) -> Arc<dyn group::Expiry> {
		// A replacement copy is always one track instance; the fallback is never read.
		let state = track.with_state(|state| state.cloned());
		let (state, cache) = match state {
			Some(state) => {
				let cache = state.read().cache.clone();
				(state, cache)
			}
			None => (self.state.clone(), self.cache.clone()),
		};
		Arc::new(Self {
			reading: self.reading.moved(&state),
			state,
			cache,
			subscription: self.subscription.clone(),
			servable: self.servable.clone(),
			sequence: self.sequence,
			superseded: Superseded::default(),
		})
	}

	fn ended(&self) {
		self.reading.release();
	}

	/// A group nothing ever landed above has no successor, so neither clock can convict it:
	/// that verdict, the common one at the live edge, takes neither the track's lock nor the
	/// subscription's. Nothing it skips parking on can move it until a group lands above,
	/// which wakes the read.
	fn is_expired(&self, max_delay: Option<Duration>, waiter: &kio::Waiter) -> bool {
		// Parked first, so a group landing above while this judges wakes it.
		if !self.cache.wakes().watch_landing(self.sequence, waiter) {
			return false;
		}
		let max_delay = max_delay.unwrap_or_else(|| {
			let mut max_delay = Duration::default();
			let _ = self.subscription.poll(waiter, |subscription| {
				max_delay = subscription.budget();
				Poll::<()>::Pending
			});
			max_delay
		});

		let mut servable = Servable::ALL;
		let _ = self.servable.poll(waiter, |current| {
			servable = current.servable.clone();
			Poll::<()>::Pending
		});

		let state = self.state.read();
		let budget = clamp_max_delay(max_delay, state.max_age_bound());
		state.poll_drifted(self.sequence, &servable, budget, Some(&self.superseded), waiter)
	}
}

/// The group a poll's drift is measured against, identified well enough to tell it apart
/// from whatever may occupy its sequence by the time a candidate is judged.
#[derive(Clone)]
struct Edge {
	/// This track's own edge, revalidated before it convicts anything.
	presentation: Option<PresentationEdge>,
	/// What the edge was resolved within, so per-candidate reach lookups measure
	/// against the same servable window.
	servable: Servable,
}

/// The groups `window` covers from `from`, below the exclusive `cap`.
fn admitted(window: &Window, from: u64, cap: Option<u64>) -> RangeInclusiveSet<u64> {
	below(window.runs(from), cap)
}

/// `runs`, lowest first, below the exclusive `cap`.
fn below(runs: impl Iterator<Item = ops::RangeInclusive<u64>>, cap: Option<u64>) -> RangeInclusiveSet<u64> {
	let mut below = RangeInclusiveSet::new();
	let last = match cap {
		Some(end) => match end.checked_sub(1) {
			Some(last) => last,
			None => return below,
		},
		None => u64::MAX,
	};
	for run in runs {
		if *run.start() > last {
			break;
		}
		below.insert(*run.start()..=(*run.end()).min(last));
	}
	below
}

/// A cursor's [`Servable`] as the groups it handed out see it, derived from its two
/// inputs: the window, which a [`Control`] replacing the ranges republishes, and the cap,
/// which only the cursor moves. Each writer replaces its own input and derives the servable
/// under the same write, so neither can publish one cut from the other's stale input.
#[derive(Clone, PartialEq)]
struct Published {
	/// The window's groups, resolved for `revision` of the ranges.
	runs: Servable,
	/// The [`Preferences::ranges_revision`] `runs` were resolved for.
	revision: u64,
	cap: Option<u64>,
	/// `runs` below `cap`.
	servable: Servable,
}

impl Published {
	fn new(runs: Servable, revision: u64, cap: Option<u64>) -> Self {
		let servable = runs.below(cap);
		Self {
			runs,
			revision,
			cap,
			servable,
		}
	}
}

/// What one run's lookup found, for [`TrackState::find_servable`] leapfrogging the cache.
enum Leap<R> {
	/// A group in the run that the search answered.
	Found(R),
	/// The run's groups, none answering.
	Exhausted,
	/// Nothing in the run: the nearest cached group past it.
	Past(u64),
	/// Nothing cached from the run on.
	Empty,
}

/// Search `cached`, ordered from the run toward the far end, for a group in `run` that `f`
/// answers.
fn leap<'a, R>(
	mut cached: impl Iterator<Item = (&'a u64, &'a Slot)>,
	run: ops::RangeInclusive<u64>,
	f: &mut impl FnMut(&'a Slot) -> Option<R>,
) -> Leap<R> {
	let Some((&first, slot)) = cached.next() else {
		return Leap::Empty;
	};
	if !run.contains(&first) {
		return Leap::Past(first);
	}
	let mut within = std::iter::once(slot).chain(
		cached
			.take_while(|(sequence, _)| run.contains(*sequence))
			.map(|(_, slot)| slot),
	);
	match within.find_map(f) {
		Some(found) => Leap::Found(found),
		None => Leap::Exhausted,
	}
}

/// The groups a reader can be served, the only ones its drift is measured against: one it
/// never reads, past its cap or in a gap between its ranges, never stands in for one it does.
#[derive(Clone, PartialEq)]
enum Servable {
	/// From `start` up to the exclusive `end`, or onward without one: one run, the common
	/// case, kept as cheap to copy and look up as a cap. Its start still bounds a group
	/// handed out before the run moved above it, which is judged against the run.
	Run { start: u64, end: Option<u64> },
	/// Disjoint runs, lowest first.
	Runs(Arc<RangeInclusiveSet<u64>>),
}

impl Servable {
	/// Every group, for a reader no window narrows.
	const ALL: Self = Self::Run { start: 0, end: None };

	/// No group.
	const NONE: Self = Self::Run { start: 0, end: Some(0) };

	/// Whether group `sequence` is among these.
	fn contains(&self, sequence: u64) -> bool {
		match self {
			Self::Run { start, end } => sequence >= *start && end.is_none_or(|end| sequence < end),
			Self::Runs(runs) => runs.contains(&sequence),
		}
	}

	/// The groups in `runs`, disjoint and lowest first. Only several runs allocate: every
	/// subscriber resolves one, so the common single run stays off the heap.
	fn new(runs: impl Iterator<Item = ops::RangeInclusive<u64>>) -> Self {
		let mut runs = runs.peekable();
		let Some(first) = runs.next() else {
			return Self::NONE;
		};
		if runs.peek().is_none() {
			return Self::Run {
				start: *first.start(),
				end: first.end().checked_add(1),
			};
		}
		let mut set = RangeInclusiveSet::new();
		set.insert(first);
		set.extend(runs);
		Self::Runs(Arc::new(set))
	}

	/// These groups below the exclusive `cap`.
	fn below(&self, cap: Option<u64>) -> Self {
		let Some(cap) = cap else {
			return self.clone();
		};
		match self {
			Self::Run { start, end } => {
				let end = end.map_or(cap, |end| end.min(cap));
				match end > *start {
					true => Self::Run {
						start: *start,
						end: Some(end),
					},
					false => Self::NONE,
				}
			}
			Self::Runs(runs) => {
				let Some(last) = cap.checked_sub(1) else {
					return Self::NONE;
				};
				Self::new(
					runs.iter()
						.take_while(|run| *run.start() <= last)
						.map(|run| *run.start()..=(*run.end()).min(last)),
				)
			}
		}
	}
}

/// The newest servable group that has presented at least one frame.
#[derive(Clone, Copy)]
struct PresentationEdge {
	sequence: u64,
	/// The slot incarnation this timestamp was read from, so an eviction or a re-served
	/// sequence between resolving the edge and using it is detectable.
	stamp: u32,
	/// The newest frame this group has presented, not its first. The candidate side of the
	/// comparison is an upper bound on what a group could still reach, so the edge side has
	/// to be the newest content that actually exists, or the two meet and nothing is ever
	/// convicted. Only ever grows as the group fills.
	timestamp: Timestamp,
}

impl PresentationEdge {
	/// Whether a group that can present until `reach` is a full `budget` behind this edge.
	fn behind(&self, reach: Option<Timestamp>, budget: Duration) -> bool {
		reach.is_some_and(|reach| matches!(self.timestamp.checked_sub(reach), Ok(age) if Duration::from(age) >= budget))
	}
}

/// The read cursor behind a [`Subscriber`].
struct Cursor {
	state: kio::Consumer<TrackState>,
	/// The track's cache account, handed to each group's [`GroupExpiry`] without locking
	/// `state` per delivery.
	cache: Arc<cache::Track>,

	subscription: Preferences,
	/// Arrival-order cursor used by `recv_group`.
	index: usize,
	/// Arrival-order cursor used by `recv_datagram`, independent of groups.
	datagram_index: usize,
	/// Minimum sequence to return from any `recv` method. Set by `set_range`.
	min_sequence: u64,
	/// This subscriber's own ranges, resolved. Boxed: every reader embeds a cursor.
	filter: Box<Filter>,
	/// One past the highest sequence returned by `next_group`, or `None` once it returned
	/// `u64::MAX` and no group can follow.
	/// Used only by that method to skip late arrivals; does not affect `recv_group`.
	next_sequence: Option<u64>,
	/// The exact candidate a route-copy selection inspected, including its arrival
	/// position. Consuming it must not look up a different group after eviction.
	peeked: Option<Box<Peeked>>,
	/// Exclusive upper sequence bound for `next_group` and `recv_group`. `None` means no
	/// cap. Set by `set_range`;
	/// can be raised, lowered, or unset at any time. Groups at or past the cap stay in
	/// the producer's cache and become eligible again when the cap rises (or is removed).
	/// `Some(0)` is the empty range.
	end_sequence: Option<u64>,
	/// What this cursor can be served, the only groups its drift is measured within: its
	/// window below its cap, so a range behind the live edge is judged against its own
	/// newest group, and a group in a gap between ranges against nothing.
	servable: Servable,
	/// [`Self::servable`], shared with the groups this cursor hands out so their expiry
	/// measures drift against the same groups after the cap or ranges move, and with its
	/// [`Control`]s, which republish it when they replace the ranges.
	published: kio::Producer<Published>,
	/// Groups the drift budget skipped since the count was last drained. Accumulated
	/// here rather than metered in place because the stats scope lives on the owning
	/// [`Subscriber`], which drains this after every read.
	stale: stats::Content,
	/// Groups the seek path has convicted but whose sequences no caller has committed
	/// past yet, keyed by sequence; see [`Self::commit_seek_stale`].
	seek_pending: BTreeMap<u64, stats::Content>,
	/// Cached groups the arrival path skipped as stale, which a wider budget owes again.
	skipped: Skipped,
	/// The drops a wire publisher forwards, once it first polls for one.
	drops: Option<Box<DropCursor>>,
	/// What this cursor misses and asks fills for, once it first reads.
	misses: Option<Box<miss::Misses>>,
}

struct Peeked {
	group: group::Consumer,
	taken: Taken,
	ordered: bool,
}

/// Where a recv-order candidate came from, which taking it advances past.
#[derive(Clone, Copy)]
enum Taken {
	/// The arrival walk, which moves to this index.
	Walk(usize),
	/// What a widening owes.
	Owed,
	/// What a fill answered.
	Filled,
}

/// The sequences a [`DropCursor`] reports from: a cursor's span, narrowed to its window.
struct Within<'a> {
	span: ops::Range<u64>,
	window: &'a Window,
	revision: u64,
}

/// What a wire publisher's cursor gave up, found as it changes rather than by rescanning
/// the cache on every poll.
struct DropCursor {
	/// Every group the publisher cuts.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	cuts: super::aborts::Watch,
	/// Sequences given up and not reported yet, with why: stale skips, cuts, and upstream
	/// drops.
	pending: RangeMap<u64, StreamError>,
	/// The [`TrackState::drops_revision`], span, and window revision last folded into
	/// `pending`.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	scanned: Option<(u64, ops::Range<u64>, u64)>,
	/// Every sequence this cursor already reported.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	reported: RangeSet<u64>,
}

#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
impl DropCursor {
	fn new(state: &TrackState) -> Self {
		// Watch before the first scan, so a cut landing between the two is still seen.
		Self {
			cuts: state.cache.watch_cuts(),
			pending: RangeMap::new(),
			scanned: None,
			reported: RangeSet::new(),
		}
	}

	/// Record a run this cursor's fills gave up on as a miss: why it is gone is unknown, so
	/// a reason already pending, such as the track's own drop, is kept.
	fn give_up(&mut self, run: ops::Range<u64>) {
		let gaps: Vec<_> = self.pending.gaps(&run).collect();
		for gap in gaps {
			self.pending.insert(gap, StreamError::NotFound);
		}
	}

	/// Record `slot`, cached at `sequence`, if it holds a cut.
	fn cut(&mut self, sequence: u64, slot: &Slot) {
		if slot.visible
			&& let Some(err) = slot.group.cut_error()
			&& let Some(end) = sequence.checked_add(1)
		{
			self.pending.insert(sequence..end, StreamError::from(err));
		}
	}

	/// The lowest unreported run of `within` given up, now marked reported, with why.
	fn next(
		&mut self,
		state: &TrackState,
		cut: BTreeSet<u64>,
		within: &Within<'_>,
	) -> Option<(ops::Range<u64>, StreamError)> {
		let (span, window) = (&within.span, within.window);
		// A cut already reclaimed from the cache is in the track's drops instead.
		for sequence in cut {
			if let Some(slot) = state.lookup.get(&sequence) {
				self.cut(sequence, slot);
			}
		}
		// Rescan only after the drops change or the window moves; out-of-window runs were
		// skipped and may be owed now.
		let scan = (state.drops_revision, span.clone(), within.revision);
		if self.scanned.as_ref() != Some(&scan) {
			self.scanned = Some(scan);
			for (dropped, err) in state.drops.overlapping(span.clone()) {
				self.pending
					.insert(dropped.start.max(span.start)..dropped.end.min(span.end), err.clone());
			}
			for (&sequence, slot) in state.lookup.range(span.clone()) {
				self.cut(sequence, slot);
			}
		}
		while let Some((range, err)) = self
			.pending
			.iter()
			.next()
			.map(|(range, err)| (range.clone(), err.clone()))
		{
			self.pending.remove(range.clone());
			// Only what this subscriber asked for is owed: the first run of its ranges in it,
			// the rest of the range waiting its turn.
			let Some(clipped) = window.clip(range.start.max(span.start)..range.end.min(span.end)) else {
				continue;
			};
			if clipped.end < range.end {
				self.pending.insert(clipped.end..range.end, err.clone());
			}
			if let Some(gap) = self.reported.gaps(&clipped).next() {
				if gap.end < range.end {
					self.pending.insert(gap.end..range.end, err.clone());
				}
				self.reported.insert(gap.clone());
				return Some((gap, err));
			}
		}
		None
	}
}

/// A cursor's own ranges, resolved, and what keeps them current.
struct Filter {
	/// Only the groups in it are read.
	window: Window,
	/// The [`Preferences::ranges_revision`] the window was resolved from.
	resolved: u64,
	/// Bumped whenever the window moves, so the drop scan knows to look again.
	revision: u64,
	/// Why the ranges can't be served here: a time anchor on an untimed track.
	refused: Option<Error>,
	/// What the arrival walk reads: the window from the floor, below the cap. The walk
	/// passes a group outside it holding nothing, so widening it re-scans the cache.
	admitted: RangeInclusiveSet<u64>,
	/// The floor, [`Self::revision`], and cap [`Self::admitted`] was taken at.
	admitted_at: Option<(u64, u64, Option<u64>)>,
	/// The spans a widening newly covers, from the ranges, the floor, the cap, or the
	/// budget, scanned in the cache in the subscription's order and handed out ahead of the
	/// walk: each cached group in them is visited once.
	owed: RangeInclusiveSet<u64>,
}

impl Cursor {
	fn new(state: kio::Consumer<TrackState>, subscription: Preferences) -> Self {
		// The ranges floor the cursor, and the budget picks where above it to start, timed
		// or not: delivery skips the cached groups it already judges stale, on whichever
		// clock convicts them.
		if subscription.read().joins_live() {
			TrackState::join(state.read());
		}
		let (filter, datagram_index, cache) = {
			let preferences = subscription.read();
			let ranges_revision = subscription.ranges_revision();
			let state = state.read();
			let (window, refused) = match Window::new(&preferences.ranges, state.latest_live(), &*state) {
				Ok(window) => (window, None),
				Err(err) => (Window::new(&[], None, &*state).expect("nothing to refuse"), Some(err)),
			};
			// Arrival order need not be timestamp order. Only skip the initial stale prefix;
			// subsequent delivery stays in arrival order, including late datagrams.
			let newest = state.datagrams.iter().filter_map(|datagram| datagram.timestamp).max();
			let start = newest.map_or(0, |newest| {
				// Every datagram carries the track's scale: convert the cutoff into it once,
				// rounded up, rather than each timestamp into a `Duration` (a 128-bit division).
				let cutoff = Duration::from(newest).saturating_sub(preferences.budget());
				let scale = newest.scale().as_u64() as u128;
				let cutoff = (cutoff.as_nanos() * scale).div_ceil(1_000_000_000) as u64;
				let cutoff = Timestamp::new(cutoff, newest.scale()).expect("at most the newest");
				state
					.datagrams
					.iter()
					.position(|datagram| datagram.timestamp.is_none_or(|timestamp| timestamp >= cutoff))
					.expect("the newest datagram is within max delay")
			});
			let filter = Filter {
				window,
				resolved: ranges_revision,
				revision: 0,
				refused,
				admitted: RangeInclusiveSet::new(),
				admitted_at: None,
				owed: RangeInclusiveSet::new(),
			};
			(filter, state.datagram_offset + start, state.cache.clone())
		};
		let published = Published::new(Servable::new(filter.window.runs(0)), filter.resolved, None);
		Self {
			state,
			cache,
			subscription,
			min_sequence: 0,
			filter: Box::new(filter),
			index: 0,
			datagram_index,
			next_sequence: Some(0),
			peeked: None,
			end_sequence: None,
			// Uncapped, the servable groups are the window's.
			servable: published.servable.clone(),
			published: kio::Producer::new(published),
			stale: stats::Content::default(),
			seek_pending: BTreeMap::new(),
			skipped: Skipped::default(),
			drops: None,
			misses: None,
		}
	}

	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	fn poll_drop(&mut self, waiter: &kio::Waiter) -> Poll<(ops::Range<u64>, StreamError)> {
		self.sync();
		if self.drops.is_none() {
			let mut drops = Box::new(DropCursor::new(&self.state.read()));
			if let Some(misses) = &self.misses {
				for run in misses.given_up().iter() {
					drops.give_up(run.clone());
				}
			}
			self.drops = Some(drops);
		}
		if self.filter.refused.is_none()
			&& let Poll::Ready(Ok(drift)) = self.poll_drift(&self.servable, waiter)
		{
			self.probe(self.floor(), &drift, waiter);
		}
		// A cap at or below the floor leaves nothing to report, not a reversed span.
		let floor = self.floor();
		let within = Within {
			span: floor..self.end_sequence.unwrap_or(u64::MAX).max(floor),
			window: &self.filter.window,
			revision: self.filter.revision,
		};
		let drops = match &mut self.drops {
			Some(drops) => drops,
			None => self.drops.insert(Box::new(DropCursor::new(&self.state.read()))),
		};
		// A cut closes its group without modifying the track state, so its own watch wakes
		// this. Taken before reading the track, so a cut after the read still wakes.
		let cut = drops.cuts.poll(waiter);
		match self.state.poll(waiter, |state| {
			drops.next(state, cut, &within).map_or(Poll::Pending, Poll::Ready)
		}) {
			Poll::Ready(Ok(range)) => Poll::Ready(range),
			Poll::Ready(Err(state)) => drops
				.next(&state, BTreeSet::new(), &within)
				.map_or(Poll::Pending, Poll::Ready),
			Poll::Pending => Poll::Pending,
		}
	}

	/// Resolve [`Self::servable`] again once the window or cap moved, and publish it to the
	/// groups already handed out. Costs the ranges, never the cache.
	fn update_servable(&mut self) {
		let runs = Servable::new(self.filter.window.runs(0));
		let (revision, cap) = (self.filter.resolved, self.end_sequence);
		self.servable = runs.below(cap);
		let next = |current: &Published| {
			// Ranges a `Control` republished past this window stay until this cursor syncs
			// to them; the cap is this cursor's own.
			let runs = match current.revision > revision {
				true => current.runs.clone(),
				false => runs.clone(),
			};
			let next = Published::new(runs, revision.max(current.revision), cap);
			(next != *current).then_some(next)
		};
		// A no-op write is skipped: every handed-out group's expiry watches the channel.
		if next(&self.published.read()).is_none() {
			return;
		}
		if let Ok(mut current) = self.published.write()
			&& let Some(next) = next(&current)
		{
			*current = next;
		}
	}

	/// Bring [`Self::window`] up to date: resolved again once the subscriber replaces its
	/// ranges, counting relative anchors back from the latest group then, and settled as
	/// the groups it waits on arrive. Returns whether the window moved.
	fn sync(&mut self) -> bool {
		let replaced = {
			let preferences = self.subscription.read();
			let revision = self.subscription.ranges_revision();
			(revision != self.filter.resolved).then(|| (revision, preferences.ranges.clone()))
		};
		let state = self.state.read();
		let moved = match replaced {
			Some((revision, ranges)) => {
				self.filter.resolved = revision;
				match Window::new(&ranges, state.latest_live(), &*state) {
					Ok(window) => {
						self.filter.window = window;
						self.filter.refused = None;
					}
					Err(err) => self.filter.refused = Some(err),
				}
				true
			}
			None if self.filter.window.is_settled() => false,
			// The live feed had no group when the ranges resolved, so the newest one then was
			// the first to arrive, or the newest once it went live, however many have arrived
			// or left since.
			None if self.filter.window.is_pending() => self.filter.window.settle(state.first_live(), &*state),
			None => self.filter.window.settle(state.latest_live(), &*state),
		};
		drop(state);
		if moved {
			self.filter.revision += 1;
			self.update_servable();
		}
		moved
	}

	/// Park a read the cap of an unsettled time end holds back on what raises it: the
	/// speculative group starting or ending, or any group it waits on starting. Returns
	/// whether the cap already moved, so the read goes again rather than parking.
	fn watch_time(&mut self, waiter: &kio::Waiter) -> bool {
		if self.filter.window.is_settled() || self.filter.refused.is_some() {
			return false;
		}
		{
			let state = self.state.read();
			if let Some(from) = self.filter.window.watch_starts() {
				state.cache.wakes().watch_start(from, waiter);
			}
			for sequence in self.filter.window.speculative() {
				if let Some(slot) = state.lookup.get(&sequence) {
					let _ = slot.serving().poll_started(waiter);
				}
			}
		}
		// Registered first, so a start landing since the last sync is either seen here or
		// wakes the read.
		self.sync()
	}

	/// The lowest group this cursor reads: its own floor, raised to its window's.
	fn floor(&self) -> u64 {
		self.min_sequence.max(self.filter.window.floor().unwrap_or(0))
	}

	/// Whether group `sequence`, at or above [`Self::floor`], is one this cursor reads now:
	/// in its window and below its cap. One that isn't may be later.
	fn admits(&self, sequence: u64) -> bool {
		super::subscription::before_end(sequence, self.end_sequence) && self.filter.window.contains(sequence)
	}

	/// The window from `from`, below the exclusive `cap`.
	fn admitted(&self, from: u64, cap: Option<u64>) -> RangeInclusiveSet<u64> {
		admitted(&self.filter.window, from, cap)
	}

	/// [`Filter::owed`] as of now: what was owed and what the walk newly reads since
	/// [`Filter::admitted`] was taken, both cut to `admitted`.
	fn owed(&self, admitted: &RangeInclusiveSet<u64>) -> RangeInclusiveSet<u64> {
		// Nothing is owed before the walk first reads: it starts at the oldest arrival.
		if self.filter.admitted_at.is_none() {
			return RangeInclusiveSet::new();
		}
		let mut owed = RangeInclusiveSet::new();
		for run in admitted.iter() {
			for gap in self.filter.admitted.gaps(run) {
				owed.insert(gap);
			}
			for span in self.filter.owed.overlapping(run) {
				owed.insert((*span.start()).max(*run.start())..=(*span.end()).min(*run.end()));
			}
		}
		owed
	}

	/// Take in a move of the floor, the ranges, or the cap: whatever it newly admits is
	/// owed. Costs the ranges, never the cache, and only when one moved.
	fn admit(&mut self) {
		let at = (self.floor(), self.filter.revision, self.end_sequence);
		if self.filter.admitted_at == Some(at) {
			return;
		}
		// The arrival walk reads the window from the floor.
		let admitted = self.admitted(self.floor(), self.end_sequence);
		self.filter.owed = self.owed(&admitted);
		self.filter.admitted = admitted;
		self.filter.admitted_at = Some(at);
	}

	/// The next cached group still owed in the subscription's order, newest first unless
	/// ascending, consuming the spans scanned on the way: only one the walk could have read,
	/// never fetched backfill or a withheld group. The group itself stays owed until
	/// [`Self::advance_arrival`] takes it, so a peek keeps it.
	fn next_owed(&mut self) -> Option<group::Handle> {
		if self.filter.owed.is_empty() {
			return None;
		}
		let ascending = self.subscription.read().order == Order::Ascending;
		let state = self.state.read();
		// Nothing is read from the cache now, so leave what is owed until it is.
		if !state.readable() {
			return None;
		}
		let low = state.live_floor.unwrap_or(0);
		loop {
			let run = match ascending {
				true => self.filter.owed.first(),
				false => self.filter.owed.last(),
			}
			.cloned()?;
			let from = (*run.start()).max(low);
			let found = (from <= *run.end())
				.then(|| {
					let mut slots = state.lookup.range(from..=*run.end()).map(|(_, slot)| slot);
					match ascending {
						true => slots.find(|slot| slot.is_read()),
						false => slots.rev().find(|slot| slot.is_read()),
					}
					.map(Slot::serving)
				})
				.flatten();
			let Some(group) = found else {
				self.filter.owed.remove(run);
				continue;
			};
			let scanned = match ascending {
				true => group.sequence.checked_sub(1).map(|below| *run.start()..=below),
				false => group.sequence.checked_add(1).map(|above| above..=*run.end()),
			};
			if let Some(scanned) = scanned.filter(|scanned| !scanned.is_empty()) {
				self.filter.owed.remove(scanned);
			}
			return Some(group.clone());
		}
	}

	/// Record `sequence` as skipped stale under `budget`, so a wider one owes it again.
	fn skip(&mut self, sequence: u64, budget: Duration) {
		self.skipped.insert(sequence, budget);
		self.prune_skipped();
	}

	/// Forget the skipped runs no cached group is left in: nothing can offer them again.
	fn prune_skipped(&mut self) {
		// Most cursors never skip, and this runs on every hand-out.
		if self.skipped.is_empty() {
			return;
		}
		let state = self.state.read();
		self.skipped.prune(state.lookup.len(), |run| {
			!state.lookup.range(run.clone()).any(|(_, slot)| slot.is_read())
		});
	}

	/// Owe again the groups this cursor skipped as stale under a budget narrower than
	/// `budget`: only a widening reverses a stale verdict. Costs the skipped runs, never
	/// the cache, and nothing when the budget did not widen.
	fn reoffer(&mut self, budget: Duration) {
		for run in self.skipped.widened(budget) {
			// Judged again, so a drop not reported yet is withdrawn; a reported one stands.
			if let Some(drops) = &mut self.drops {
				let span = *run.start()..run.end().saturating_add(1);
				let old: Vec<_> = drops
					.pending
					.overlapping(span.clone())
					.filter(|(_, err)| **err == StreamError::Old)
					.map(|(range, _)| range.start.max(span.start)..range.end.min(span.end))
					.collect();
				for range in old {
					drops.pending.remove(range);
				}
			}
			let admitted: Vec<_> = self
				.filter
				.admitted
				.overlapping(&run)
				.map(|admitted| (*admitted.start()).max(*run.start())..=(*admitted.end()).min(*run.end()))
				.collect();
			for owed in admitted {
				self.filter.owed.insert(owed);
			}
		}
	}

	// A helper to automatically apply Dropped if the state is closed without an error.
	fn poll<F, R>(&self, waiter: &kio::Waiter, f: F) -> Poll<Result<R>>
	where
		F: Fn(&kio::Ref<'_, TrackState>) -> Poll<Result<R>>,
	{
		let polled = self.state.poll(waiter, |state| {
			let res = f(state);
			if res.is_pending() {
				// The end can settle with no write: when the grace for a missing group passes.
				state.watch_tail(waiter);
			}
			res
		});
		Poll::Ready(match ready!(polled) {
			Ok(res) => res,
			// We try to clone abort just in case the function forgot to check for terminal state.
			Err(state) => Err(state.abort.clone().unwrap_or(Error::Dropped)),
		})
	}

	/// Take the groups skipped since the last call, for the owner to meter.
	fn take_stale(&mut self) -> stats::Content {
		std::mem::take(&mut self.stale)
	}

	/// This subscriber's clamped drift budget and the live edge to measure against,
	/// resolved once per poll.
	///
	/// `servable` bounds the edge: the caller passes the same window it reads from, so a
	/// group is only ever judged against content that could actually be served in its
	/// place. Read fresh each poll, so a mid-stream [`Control::update`] applies
	/// to the very next group, and shared across every candidate that poll walks off, so
	/// discarding a backlog of N groups costs one scan rather than N. Only ever
	/// [`Poll::Ready`]; the track ending surfaces as the error the caller was going to
	/// get anyway.
	fn poll_drift(&self, servable: &Servable, waiter: &kio::Waiter) -> Poll<Result<Drift>> {
		let mut max_delay = Duration::default();
		let _ = self.subscription.poll_ref(waiter, |subscription| {
			max_delay = subscription.budget();
			Poll::<()>::Pending
		});
		self.poll(waiter, |state| {
			let bound = state.max_age_bound();
			Poll::Ready(Ok(Drift {
				budget: clamp_max_delay(max_delay, bound),
				bounded: bound.is_some_and(|bound| max_delay >= bound),
				edge: state.drift_edge(servable),
			}))
		})
	}

	/// Whether the drift budget says to skip `group`, against a [`Drift`] already resolved
	/// for this poll.
	fn poll_stale(&self, group: &group::Consumer, drift: &Drift, waiter: &kio::Waiter) -> Poll<Result<bool>> {
		self.poll(waiter, |state| {
			Poll::Ready(Ok(state.is_stale(group.sequence, &drift.edge, drift.budget)))
		})
	}

	/// Forget the gaps in `delivered` below each group past this cursor's budget now,
	/// whether this track holds it or not: one landing there would be written off. A group
	/// skipped as stale is not forgotten, since a wider budget owes it again, and neither is
	/// one a widening still owes, which a newest-first re-offer has yet to judge.
	///
	/// One edge for every group judged, so forgetting N gaps never resolves it N times.
	fn forget_past_budget(&self, delivered: &mut Delivered) {
		if delivered.above_gap().is_none() {
			return;
		}
		let owed = self.filter.owed.first().map(|run| *run.start());
		let skipped = self.skipped.lowest().into_iter().chain(owed).min();
		let max_delay = self.subscription.read().budget();
		let state = self.state.read();
		let budget = clamp_max_delay(max_delay, state.max_age_bound());
		let edge = state.drift_edge(&self.servable);
		let now = state.cache.pool().now_wall();
		while let Some(oldest) = delivered.above_gap() {
			// Either clock, as `drifted` judges a group: without a successor, neither.
			let Some(arrival) = state.arrival(oldest, &edge.servable) else {
				break;
			};
			let shared = if now.saturating_duration_since(arrival.arrived) >= budget {
				arrival.group.sequence
			} else if let Some(successor) = state.successor(oldest, &edge.servable)
				&& let Some(live) = state.live_above(oldest, &edge)
				&& live.behind(successor.group.timestamp(), budget)
			{
				successor.group.sequence.min(live.sequence)
			} else {
				break;
			};
			// Every delivered run below this successor shares its verdict. Retire
			// them in one split, including the highest run, rather than once per gap.
			let last = delivered
				.last_before(shared)
				.expect("the oldest run is below its successor");
			if let Some(skipped) = skipped
				&& skipped < last
			{
				delivered.forget_below(skipped);
				break;
			}
			delivered.forget_below(last);
		}
	}

	/// Hand `group` out whole from its first frame, expiring against its budget.
	///
	/// A group missing its head reads it as [`Error::Lagged`], which a route copy recovers
	/// from another.
	fn deliver(&self, group: group::Consumer) -> group::Consumer {
		let sequence = group.sequence;
		group.with_expiry(Arc::new(GroupExpiry {
			state: self.state.weak(),
			cache: self.cache.clone(),
			reading: Reading::new(self.state.clone()),
			subscription: self.subscription.consume(),
			servable: self.published.consume(),
			sequence,
			superseded: Superseded::default(),
		}))
	}

	/// Ask fills for what this cursor misses from `from`, and take in what they answered,
	/// parking `waiter` on the fills still out. Whatever it gives up goes to the drop cursor.
	fn probe(&mut self, from: u64, drift: &Drift, waiter: &kio::Waiter) {
		let (window, cap, revision) = (&self.filter.window, self.end_sequence, self.filter.revision);
		let asking = {
			let subscription = self.subscription.read();
			miss::Asking {
				priority: subscription.priority,
				max_delay: subscription.max_delay,
				order: subscription.order,
			}
		};
		let fills = self.state.read().fills.clone();
		let misses = self.misses.get_or_insert_with(Box::default);
		let mut given_up = Vec::new();
		if misses.lowest().is_some() {
			misses.retain(from, &below(window.missable(from), cap));
		}
		let asks = {
			let state = self.state.read();
			misses.judge(
				&state,
				(from, revision, cap),
				window.named(),
				|span| {
					let end = cap.map_or(*span.end(), |cap| (*span.end()).min(cap.saturating_sub(1)));
					if cap == Some(0) || *span.start() > end {
						return RangeInclusiveSet::new();
					}
					window
						.missable(*span.start())
						.take_while(|run| *run.start() <= end)
						.map(|run| *run.start()..=(*run.end()).min(end))
						.collect()
				},
				drift,
				&mut given_up,
				waiter,
			)
		};
		misses.ask(&self.state, &fills, &asks, &asking, &mut given_up);
		loop {
			let retry = misses.resolve(&self.state, asking.max_delay, &mut given_up, waiter);
			if retry.is_empty() {
				break;
			}
			misses.ask(&self.state, &fills, &retry, &asking, &mut given_up);
		}
		misses.expire(&self.state.read(), drift, &mut given_up, waiter);
		if let Some(drops) = &mut self.drops {
			for run in given_up {
				drops.give_up(run);
			}
		}
	}

	/// The next group a fill answered for this cursor in its order, cached and not handed
	/// out yet.
	fn next_filled(&mut self) -> Option<group::Handle> {
		let misses = self.misses.as_mut()?;
		let order = self.subscription.read().order;
		let mut given_up = Vec::new();
		let group = misses.next_filled(&self.state.read(), order, &mut given_up);
		if let Some(drops) = &mut self.drops {
			for run in given_up {
				drops.give_up(run);
			}
		}
		group
	}

	/// Whether a fill still owes this cursor a group, so the end of the feed is not its end.
	fn awaits_fill(&self) -> bool {
		self.misses.as_ref().is_some_and(|misses| misses.is_waiting())
	}

	fn poll_recv_group(&mut self, take: bool, waiter: &kio::Waiter) -> Poll<Result<Option<group::Consumer>>> {
		if let Some(peeked) = self.peeked.take().filter(|peeked| take && !peeked.ordered) {
			// The reserved group is still the one handed out, but bounded by the ranges in
			// force now, which a `Control` may have replaced since the peek.
			self.sync();
			self.advance_arrival(peeked.group.sequence, peeked.taken);
			self.skipped.remove(peeked.group.sequence);
			peeked.group.cache_refresh();
			return Poll::Ready(Ok(Some(self.deliver(peeked.group))));
		}
		self.sync();
		if let Some(err) = &self.filter.refused {
			return Poll::Ready(Err(err.clone()));
		}
		self.admit();

		// One scan for the whole poll, so walking a backlog off stays linear in its size.
		let drift = ready!(self.poll_drift(&self.servable, waiter))?;
		self.reoffer(drift.budget);
		self.probe(self.floor(), &drift, waiter);
		self.poll_recv_with(take, drift, waiter)
	}

	/// [`Self::poll_recv_group`] once synced, judging against `drift`. Without `take`, it
	/// peeks: the candidate stays at its cursor position until a taking read.
	fn poll_recv_with(
		&mut self,
		take: bool,
		mut drift: Drift,
		waiter: &kio::Waiter,
	) -> Poll<Result<Option<group::Consumer>>> {
		loop {
			// Re-offer what a widening newly covers ahead of the arrival walk, in the
			// subscription's order: it is the oldest thing still owed. A group the walk has
			// yet to reach comes again from it, and the subscriber's delivered set drops the
			// repeat.
			let owed = match self.next_owed() {
				Some(producer) => Some((producer, Taken::Owed)),
				// A fill answered what the walk never reaches: backfill is not in it.
				None => self.next_filled().map(|producer| (producer, Taken::Filled)),
			};
			let (consumer, taken) = match owed {
				Some((producer, taken)) => {
					let consumer = producer.consume();
					// A re-offer is a delivery: stamp it like a fresh hand-out. Peeking is
					// not an access.
					if take {
						consumer.cache_refresh();
					}
					(consumer, taken)
				}
				None => {
					let floor = self.floor();
					let polled = self.poll(waiter, |state| state.poll_recv_group(self.index, floor));
					// A moved window moved the servable edge `drift` was judged against.
					if polled.is_pending() && self.watch_time(waiter) {
						self.admit();
						drift = ready!(self.poll_drift(&self.servable, waiter))?;
						continue;
					}
					let Some((producer, found_index)) = ready!(polled)? else {
						// The feed ended, but a fill still owes a group. Its answer wakes us.
						if self.awaits_fill() {
							return Poll::Pending;
						}
						return Poll::Ready(Ok(None));
					};
					let consumer = producer.consume();
					// Stamp with the track guard released, so delivery never nests the
					// group's state lock under the track's. Peeking is not an access.
					if take {
						consumer.cache_refresh();
					}

					// The first group to arrive resolves a relative floor, which may land
					// above it, and a later one may settle where a time end falls.
					if !self.filter.window.is_settled() {
						self.sync();
						self.admit();
						// The snapshot saw the pending window, which serves everything.
						if !self.filter.window.is_pending() {
							drift = ready!(self.poll_drift(&self.servable, waiter))?;
						}
						if consumer.sequence < self.floor() {
							self.index = found_index + 1;
							continue;
						}
					}

					// Pass a group beyond the cap or outside the ranges, holding nothing: a
					// widening finds it in the cache again. Keep scanning so an in-range group
					// that arrived behind it still flows.
					if !self.admits(consumer.sequence) {
						self.index = found_index + 1;
						continue;
					}
					(consumer, Taken::Walk(found_index + 1))
				}
			};

			// Drop a group the drift budget has given up on and keep scanning, so one
			// poll walks a whole backlog off rather than handing it out group by group.
			let stale = ready!(self.poll_stale(&consumer, &drift, waiter))?;
			// A peek discards what cannot be delivered, but keeps its candidate at the
			// same cursor position until the reader chooses this copy.
			if take || stale {
				self.advance_arrival(consumer.sequence, taken);
			}
			if stale {
				self.stale.add(consumer.content());
				if let Some(drops) = &mut self.drops
					&& let Some(end) = consumer.sequence.checked_add(1)
				{
					drops.pending.insert(consumer.sequence..end, StreamError::Old);
				}
				match drift.bounded {
					// A re-offered group stale at the bound is final.
					true => self.skipped.remove(consumer.sequence),
					false => self.skip(consumer.sequence, drift.budget),
				}
				continue;
			}
			return Poll::Ready(Ok(Some(if take {
				self.skipped.remove(consumer.sequence);
				self.deliver(consumer)
			} else {
				self.peeked = Some(Box::new(Peeked {
					group: consumer.clone(),
					taken,
					ordered: false,
				}));
				consumer
			})));
		}
	}

	/// Move the arrival cursor past a group it read: from the walk, to `index`; from what
	/// a widening owes, out of the owed spans.
	fn advance_arrival(&mut self, sequence: u64, taken: Taken) {
		match taken {
			Taken::Walk(index) => self.index = index,
			Taken::Owed => self.filter.owed.remove(sequence..=sequence),
			Taken::Filled => {
				if let Some(misses) = &mut self.misses {
					misses.take_filled(sequence);
				}
			}
		}
	}

	/// The next datagram inside this subscriber's range, dropping the ones outside it.
	///
	/// The floor, cap and time/group ranges select whole datagram groups here; frame
	/// bounds are applied by the sending session. A datagram is judged when the cursor reaches
	/// it, against the range in force then, and one outside it is gone for good: unlike a
	/// group past the cap, nothing holds it for a later raise.
	fn poll_recv_datagram(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<super::datagram::Stored>>> {
		loop {
			// Refuse before reading, so a refused subscription consumes no datagram.
			self.sync();
			if let Some(err) = &self.filter.refused {
				return Poll::Ready(Err(err.clone()));
			}
			let Some((datagram, found_index)) =
				ready!(self.poll(waiter, |state| state.poll_recv_datagram(self.datagram_index))?)
			else {
				return Poll::Ready(Ok(None));
			};
			self.datagram_index = found_index + 1;
			self.sync();

			let sequence = datagram.sequence;
			// A datagram is a whole group, so its own timestamp places it against a time end.
			let admits = super::subscription::before_end(sequence, self.end_sequence)
				&& self.filter.window.admits_datagram(sequence, datagram.timestamp);
			if sequence >= self.floor() && admits {
				return Poll::Ready(Ok(Some(datagram)));
			}
		}
	}

	/// Pass groups another copy already handed out, without delivering or metering them.
	fn skip_ordered(&mut self, floor: Option<u64>) {
		let next = self.next_sequence.zip(floor).map(|(next, floor)| next.max(floor));
		if next == self.next_sequence {
			return;
		}
		self.next_sequence = next;
		if self
			.peeked
			.as_ref()
			.is_some_and(|peeked| next.is_none_or(|next| peeked.group.sequence < next))
		{
			self.peeked = None;
		}
		// A group delivered on another copy is not a stale skip on this one.
		if !self.seek_pending.is_empty() {
			self.seek_pending = next.map_or_else(BTreeMap::new, |next| self.seek_pending.split_off(&next));
		}
	}

	/// The lowest servable group past the last one returned, walking off everything the
	/// drift budget has convicted on the way.
	fn poll_next_group(&mut self, take: bool, waiter: &kio::Waiter) -> Poll<Result<Option<group::Consumer>>> {
		if let Some(peeked) = self.peeked.take().filter(|peeked| take && peeked.ordered) {
			// Bounded by the ranges in force now, as on the arrival path.
			self.sync();
			return Poll::Ready(Ok(Some(self.advance_sequence(peeked.group))));
		}
		let Some(next) = self.next_sequence else {
			return Poll::Ready(Ok(None));
		};
		self.sync();
		if let Some(err) = &self.filter.refused {
			return Poll::Ready(Err(err.clone()));
		}
		let floor = next.max(self.floor());
		// One scan for the whole poll, so walking a backlog off stays linear in its size.
		let drift = ready!(self.poll_drift(&self.servable, waiter))?;
		self.probe(floor, &drift, waiter);
		self.poll_next_with(take, floor, drift, waiter)
	}

	/// [`Self::poll_next_group`] once synced, from `floor`, judging against `drift`.
	/// Without `take`, it peeks: the candidate is reserved, not delivered.
	fn poll_next_with(
		&mut self,
		take: bool,
		mut floor: u64,
		mut drift: Drift,
		waiter: &kio::Waiter,
	) -> Poll<Result<Option<group::Consumer>>> {
		let end = self.end_sequence;
		let group = loop {
			let window = &self.filter.window;
			let polled = self.poll(waiter, |state| state.poll_next_in_range(floor, end, window));
			// A moved window moved the servable edge `drift` was judged against.
			if polled.is_pending() && self.watch_time(waiter) {
				drift = ready!(self.poll_drift(&self.servable, waiter))?;
				continue;
			}
			let Some(producer) = ready!(polled)? else {
				// The feed ended, but a fill still owes a group. Its answer wakes us.
				if self.awaits_fill() {
					return Poll::Pending;
				}
				// Deliberately no flush of `seek_pending` here: only a delivery commit
				// may count a conviction. This `None` can be an artifact of a floor
				// that will lower again. A conviction never committed is dropped
				// uncounted with the cursor.
				return Poll::Ready(Ok(None));
			};
			// In order means a group a fill still owes below this one comes first. Its
			// answer, or giving it up, wakes us.
			if self
				.misses
				.as_ref()
				.and_then(|misses| misses.lowest_wanted())
				.is_some_and(|wanted| wanted < producer.sequence)
			{
				return Poll::Pending;
			}
			let group = producer.consume();

			// The first group to arrive resolves a relative floor, which may land above it, and
			// a later one may settle where a time end falls.
			if !self.filter.window.is_settled() {
				self.sync();
				// The snapshot saw the pending window, which serves everything.
				if !self.filter.window.is_pending() {
					drift = ready!(self.poll_drift(&self.servable, waiter))?;
				}
				if group.sequence < self.floor() || !self.filter.window.contains(group.sequence) {
					floor = floor.max(self.floor()).max(group.sequence.saturating_add(1));
					continue;
				}
			}

			// Skip a group the budget has given up on and keep scanning, so one poll
			// walks a whole backlog off rather than handing it out group by group.
			if ready!(self.poll_stale(&group, &drift, waiter))? {
				// Not counted yet: a conviction is not permanent (the budget can widen,
				// the edge can be evicted), so the group may still be delivered.
				// Re-snapshot on every re-examination so the eventual count reflects the
				// group's latest observed content.
				self.seek_pending.insert(group.sequence, group.content());
				floor = group.sequence.saturating_add(1);
				continue;
			}

			// A conviction the budget walked back: the group is handed over after all,
			// so it must never reach the stale count.
			self.seek_pending.remove(&group.sequence);
			break group;
		};

		Poll::Ready(Ok(Some(if take {
			self.advance_sequence(group)
		} else {
			self.peeked = Some(Box::new(Peeked {
				group: group.clone(),
				taken: Taken::Owed,
				ordered: true,
			}));
			group
		})))
	}

	fn advance_sequence(&mut self, group: group::Consumer) -> group::Consumer {
		self.next_sequence = group.sequence.checked_add(1);
		// The reader holds the group itself now, as on the arrival path.
		if let Some(misses) = &mut self.misses {
			misses.take_filled(group.sequence);
		}
		// The delivery commits everything the seek stepped over to reach this group.
		self.commit_seek_stale(self.next_sequence.unwrap_or(u64::MAX));
		group.cache_refresh();
		self.deliver(group)
	}

	/// Count the sequence path's convictions below `committed` into [`Self::stale`],
	/// exactly once each.
	///
	/// A conviction only becomes a real skip once a delivery commits past it. Until then
	/// the entry waits in [`Self::seek_pending`], where a delivered group removes itself
	/// (see [`Self::poll_next_group`]). `committed` must be the sequence watermark (one
	/// past the last delivery), which only rises. The seek's floor is NOT that: it
	/// includes the local floor, which can be lowered again, and a conviction it flushed could
	/// then be delivered after all.
	fn commit_seek_stale(&mut self, committed: u64) {
		let keep = self.seek_pending.split_off(&committed);
		for (_, content) in std::mem::replace(&mut self.seek_pending, keep) {
			self.stale.add(content);
		}
	}
}

/// A cloneable handle to a subscriber's delivery preferences.
///
/// This updates the same subscription as the owning [`Subscriber`] without
/// borrowing its read cursor, so callers can change delivery priority, the max delay
/// budget, or group bounds while another task is waiting for groups.
#[derive(Clone)]
pub struct Control {
	subscription: Preferences,
	track: Consumer,
	/// A plain cursor's servable window, republished when the ranges are replaced, so a
	/// group it handed out is judged against them before the cursor next reads. `None`
	/// for a reader that follows a route, which [`Self::mirror`] serves instead.
	republish: Option<Republish>,
	/// A reader that follows a route, whose copies take the new preferences at once.
	mirror: Option<super::resume::Mirror>,
	// Whether the track is timed, so a time anchor on an untimed one is refused.
	timed: bool,
}

/// What a [`Control`] needs to republish a plain cursor's [`Published`] window.
#[derive(Clone)]
struct Republish {
	state: kio::ConsumerWeak<TrackState>,
	published: kio::ProducerWeak<Published>,
}

impl Republish {
	/// Resolve the ranges in `preferences` as the cursor will, and publish them under the
	/// cap the cursor holds then, unless the cursor already published them or newer.
	fn publish(&self, preferences: &Preferences) {
		let Some(published) = self.published.produce() else {
			return;
		};
		let (ranges, revision) = {
			let current = preferences.read();
			(current.ranges.clone(), preferences.ranges_revision())
		};
		if published.read().revision >= revision {
			return;
		}
		let state = self.state.read();
		// The update was refused before it got here if the track can't resolve it.
		let Ok(window) = Window::new(&ranges, state.latest_live(), &*state) else {
			return;
		};
		drop(state);
		let runs = Servable::new(window.runs(0));
		if let Ok(mut current) = published.write()
			&& current.revision < revision
		{
			*current = Published::new(runs, revision, current.cap);
		}
	}
}

impl Control {
	/// This subscriber's current preferences.
	pub fn subscription(&self) -> Subscription {
		self.subscription.read().clone()
	}

	/// Replace this subscriber's preferences, updating the producer's aggregate.
	///
	/// Returns [`Error::Closed`] if the track already ended; the update is
	/// meaningless at that point and can usually be ignored. Refuses a time anchor on
	/// an untimed track with [`Error::TimestampMismatch`], leaving the preferences as they were.
	pub fn update(&self, subscription: Subscription) -> Result<()> {
		if !self.timed && subscription.has_time() {
			return Err(Error::TimestampMismatch);
		}
		if subscription.ranges != self.subscription.read().ranges {
			self.track.refuse_subscription(&subscription)?;
		}
		self.subscription.update(subscription)?;
		if let Some(republish) = &self.republish {
			republish.publish(&self.subscription);
		}
		if let Some(mirror) = &self.mirror {
			mirror.sync();
		}
		Ok(())
	}
}

/// A group-only wire refuses an unresolved time anchor before changing shared demand.
fn refuse_cached_anchors(state: &TrackState, subscription: &Subscription) -> Result<()> {
	if state.cached_anchors && subscription.has_time() {
		let window = Window::new(&subscription.ranges, state.latest_live(), state)?;
		if !window.is_settled() {
			return Err(Error::Unsupported);
		}
	}
	Ok(())
}

/// Refuse `subscription` when it asks an untimed track for a presentation time.
fn refuse_untimed(info: &Info, subscription: &Subscription) -> Result<()> {
	match info.timescale.is_none() && subscription.has_time() {
		true => Err(Error::TimestampMismatch),
		false => Ok(()),
	}
}

impl Subscriber {
	/// The track's [`Info`], resolved when the subscription was established.
	///
	/// Free, unlike [`Consumer::info`]: subscribing already waited for the info
	/// (SUBSCRIBE_OK on the wire), so a subscriber always has it.
	pub fn info(&self) -> &Info {
		&self.info
	}

	/// The track's name, unique within its broadcast.
	pub fn name(&self) -> &str {
		&self.name
	}

	/// The broadcast this track belongs to, as reached through the handle it came from.
	/// Its [`path`](broadcast::Info::path) is what a catalog's relative `broadcast`
	/// references resolve against.
	pub fn broadcast(&self) -> &broadcast::Info {
		&self.broadcast
	}

	/// Attribute the groups the drift budget skipped since the last read.
	fn count_stale(&mut self, meter: &stats::Meter) {
		// An untagged subscriber leaves the count where it is.
		if meter.is_tracked() {
			meter.stale(self.take_stale());
		}
	}

	/// The groups the drift budget skipped since the last call; see [`Cursor::take_stale`].
	pub(super) fn take_stale(&mut self) -> stats::Content {
		match &mut self.inner {
			Inner::Plain(cursor) => cursor.take_stale(),
			Inner::Resume(resume, _) => resume.take_stale(),
		}
	}

	/// Read this subscription's track cache without sharing its cursor.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn consume(&self) -> Consumer {
		let state = match &self.inner {
			Inner::Plain(cursor) => cursor.state.weak(),
			Inner::Resume(_, state) => state.weak(),
		};
		Consumer::new(self.name.clone(), state)
	}

	/// Create a handle for updating this subscriber's delivery preferences.
	pub fn control(&self) -> Control {
		let timed = self.info.timescale.is_some();
		match &self.inner {
			Inner::Plain(cursor) => Control {
				track: self.consume(),
				subscription: cursor.subscription.clone(),
				republish: Some(Republish {
					state: cursor.state.weak(),
					published: cursor.published.weak(),
				}),
				mirror: None,
				timed,
			},
			Inner::Resume(resume, _) => Control {
				track: self.consume(),
				subscription: resume.subscription().clone(),
				republish: None,
				mirror: Some(resume.mirror()),
				timed,
			},
		}
	}

	/// Poll for the next group in arrival order, without blocking.
	///
	/// Returns each group it delivers exactly once, in the order it landed on the wire,
	/// which may be out of sequence due to network reordering or loss. Use
	/// [`Self::ordered`] if you only want groups whose sequence number is higher than any
	/// previously returned.
	///
	/// Once means once per sequence: a group the publisher aborts and re-creates, or that
	/// a second route delivers too, is not handed out again. Once a group is past the
	/// budget below, the subscriber stops expecting every group before it, so a gap that
	/// old is never filled, which holds because a timed track refuses a group whose start
	/// rewinds. The exception is a cached group it skipped as stale, which a wider budget
	/// offers again.
	///
	/// Groups are semi-reliable, and the [`Subscription::max_delay`] budget is the other
	/// thing (alongside eviction and a moving start) that decides which of them arrive:
	/// one that has drifted further behind the live edge than the budget tolerates is
	/// skipped rather than handed over, so a single poll walks off a whole backlog. The
	/// default is [`Duration::ZERO`], which takes the live
	/// edge and writes the rest off; raise it to read history. [`Self::with_range`] and
	/// [`Subscription::ranges`] are filters, not exemptions: backfill needs a budget that
	/// covers it. [`Consumer::fetch_group`] is the way to ask for one old group outright.
	/// Across same-epoch route copies, the lowest next buffered group is delivered
	/// first; a lower group that arrives later is still delivered in arrival order.
	/// The budget remains attached to a returned group: if it stalls while newer data
	/// advances, its pending frame read ends with [`Error::Old`].
	///
	/// Honors the group range set by [`Self::with_range`]: a group beyond the cap is
	/// passed without blocking in-range groups that arrive behind it, and holds nothing
	/// here. Raising the cap, or widening the ranges, offers the groups it newly covers
	/// that the cache still holds, newest first unless [`Order::Ascending`]. Widening the
	/// budget offers the cached groups it skipped as stale and now covers the same way.
	/// Once the track finishes, a reader with nothing left in range ends.
	///
	/// A group one of [`Subscription::ranges`] starts or ends inside reads only the frames
	/// they cover there. One missing the frames asked for reads [`Error::Lagged`] rather
	/// than starting short.
	///
	/// Returns `Poll::Ready(Ok(Some(group)))` when a group is available,
	/// `Poll::Ready(Ok(None))` when the track is finished,
	/// `Poll::Ready(Err(e))` when the track has been aborted, or
	/// `Poll::Pending` when no group is available yet.
	pub fn poll_recv_group(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<group::Consumer>>> {
		self.poll_group(false, waiter)
	}

	/// The next group in arrival order, or in sequence order for `ordered`, skipping any
	/// already handed out.
	fn poll_group(&mut self, ordered: bool, waiter: &kio::Waiter) -> Poll<Result<Option<group::Consumer>>> {
		let meter = self.stats.meter();
		let res = match &mut self.inner {
			Inner::Plain(cursor) => loop {
				let res = match ordered {
					true => cursor.poll_next_group(true, waiter),
					false => cursor.poll_recv_group(true, waiter),
				};
				match res {
					Poll::Ready(Ok(Some(group))) if self.delivered.contains(group.sequence) => continue,
					res => break res,
				}
			},
			Inner::Resume(resume, _) => resume.poll_group(ordered, &self.delivered, waiter),
		};
		if let Poll::Ready(Ok(Some(group))) = &res {
			self.hand_out(group.sequence, ordered);
		}
		self.count_stale(&meter);
		res.map(|res| res.map(|group| group.map(|group| group.with_meter(meter))))
	}

	/// Reserve the next group without advancing past it, so the subsequent read
	/// takes that exact group even if a producer evicts it meanwhile.
	pub(super) fn poll_peek_group(&mut self, ordered: bool, waiter: &kio::Waiter) -> Poll<Result<Option<u64>>> {
		match &mut self.inner {
			Inner::Plain(cursor) => loop {
				let group = ready!(match ordered {
					true => cursor.poll_next_group(false, waiter),
					false => cursor.poll_recv_group(false, waiter),
				})?;
				let Some(group) = group else {
					return Poll::Ready(Ok(None));
				};
				if !self.delivered.contains(group.sequence) {
					return Poll::Ready(Ok(Some(group.sequence)));
				}
				// A replay advances only this copy's cursor, as an ordinary read would.
				ready!(match ordered {
					true => cursor.poll_next_group(true, waiter),
					false => cursor.poll_recv_group(true, waiter),
				})?;
			},
			Inner::Resume(resume, _) => resume.poll_peek_group(ordered, &self.delivered, waiter),
		}
	}

	/// Pass groups another route copy handed out, preserving the arrival cursor and bounds.
	pub(super) fn skip_ordered(&mut self, floor: Option<u64>) {
		match &mut self.inner {
			Inner::Plain(cursor) => cursor.skip_ordered(floor),
			Inner::Resume(resume, _) => {
				// Its retention decisions also need the containing reader's past prefix.
				// This records what is no longer owed, without handing a group out.
				self.delivered.forget_below(floor.unwrap_or(u64::MAX));
				if floor.is_none() {
					self.delivered.insert(u64::MAX);
				}
				resume.skip_ordered(floor);
			}
		}
	}

	/// Record `sequence` as handed out. An ordered read never goes back, and any reader
	/// stops expecting what is past its budget, so only the gaps within it cost memory.
	fn hand_out(&mut self, sequence: u64, ordered: bool) {
		if ordered {
			self.delivered.forget_below(sequence);
		}
		if let Inner::Plain(cursor) = &mut self.inner {
			cursor.prune_skipped();
		}
		self.delivered.insert(sequence);
		let mut delivered = std::mem::take(&mut self.delivered);
		self.forget_past_budget(&mut delivered);
		self.delivered = delivered;
	}

	/// Forget the gaps in `delivered` below each group past this subscriber's budget now:
	/// one landing there would be written off.
	pub(super) fn forget_past_budget(&self, delivered: &mut Delivered) {
		match &self.inner {
			Inner::Plain(cursor) => cursor.forget_past_budget(delivered),
			Inner::Resume(resume, _) => resume.forget_past_budget(delivered),
		}
	}

	/// How many runs of handed-out groups this subscriber remembers.
	#[cfg(test)]
	pub(super) fn delivered_runs(&self) -> usize {
		self.delivered.runs()
	}

	/// Whether an unread cached group needs this subscription, without moving its cursor.
	pub(super) fn has_unread_group(&self, unread: &Unread<'_>) -> bool {
		let cursor = match &self.inner {
			Inner::Plain(cursor) => cursor,
			Inner::Resume(resume, _) => return resume.has_unread_group(unread),
		};
		let state = cursor.state.read();
		let floor = cursor.floor().max(state.live_floor.unwrap_or(0)).max(unread.start);
		let eligible = |sequence: u64| {
			sequence >= floor
				&& super::subscription::before_end(sequence, unread.end)
				&& cursor.filter.window.contains(sequence)
				&& !unread.delivered.contains(sequence)
		};
		// What a widening owes, the walk has yet to take in, or lifting the local cap would:
		// a group past it is unread, not gone.
		let owed = cursor.owed(&cursor.admitted(cursor.floor(), None));
		let last = unread.end.map(|end| end.checked_sub(1));
		let owed = || {
			owed.iter().any(|run| {
				let end = match last {
					Some(Some(last)) => (*run.end()).min(last),
					Some(None) => return false,
					None => *run.end(),
				};
				*run.start() <= end
					&& state
						.lookup
						.range(*run.start()..=end)
						.any(|(sequence, slot)| eligible(*sequence) && slot.is_read())
			})
		};
		state.readable()
			&& (state
				.arrival
				.iter()
				.skip(cursor.index.saturating_sub(state.offset))
				.any(|(sequence, stamp)| {
					eligible(*sequence)
						&& state
							.lookup
							.get(sequence)
							.is_some_and(|slot| slot.stamp == *stamp && slot.is_read())
				}) || owed())
	}

	/// Receive the next group in arrival order.
	///
	/// Every group sequence is returned at most once, in the order it landed on the wire, which may
	/// be out of sequence due to network reordering or loss. Use [`Self::ordered`] if you
	/// only want groups whose sequence number is higher than any previously returned.
	/// See [`Self::poll_recv_group`] for how [`Self::with_range`] applies.
	#[cfg(feature = "async")]
	pub async fn recv_group(&mut self) -> Result<Option<group::Consumer>> {
		kio::wait(|waiter| self.poll_recv_group(waiter)).await
	}

	/// Poll for the next datagram in arrival order, without blocking.
	///
	/// Datagrams are a separate best-effort channel from groups (see
	/// [`Producer::append_datagram`]); they share only the sequence namespace, and
	/// neither cursor moves the other. A new timed subscriber skips the buffered prefix older
	/// than its max delay behind the highest timestamp; untimed subscribers keep the whole
	/// buffer. Later arrival-order delivery is unchanged, and any outside its group range
	/// are skipped. A consumer that falls too far behind silently loses the oldest datagrams.
	///
	/// A moq-transport group delivers only its normal Object ID 0 payload here. Other
	/// Object IDs and statuses are skipped; relay paths preserve them with their IDs intact.
	///
	/// Returns `Poll::Ready(Ok(Some(datagram)))` when one is available,
	/// `Poll::Ready(Ok(None))` when the track is finished, `Poll::Ready(Err(e))` when the track
	/// is aborted, or `Poll::Pending` when none is buffered yet.
	pub fn poll_recv_datagram(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Datagram>>> {
		loop {
			let Some(datagram) = ready!(self.poll_recv_stored_datagram(waiter))? else {
				return Poll::Ready(Ok(None));
			};
			// A plain payload cannot identify another object in the same group or a status.
			if let Some(wire) = &datagram.wire
				&& (wire.object_id.is_some_and(|id| id > 0) || wire.status.is_some_and(|status| status != 0))
			{
				tracing::debug!(
					sequence = datagram.sequence,
					object_id = wire.object_id,
					status = wire.status,
					"dropping datagram not representable as a plain payload"
				);
				continue;
			}
			self.meter_datagram(datagram.payload.len() as u64);
			return Poll::Ready(Ok(Some(datagram.datagram)));
		}
	}

	/// Count a datagram handed on as `bytes` of payload, the single-frame group it stands in
	/// for. Unlike a group (metered lazily as its frames are read), a datagram goes whole.
	pub(crate) fn meter_datagram(&self, bytes: u64) {
		self.stats.meter().datagram(bytes);
	}

	/// The next datagram as the track stores it: its wire metadata, any status, and a
	/// compressed track's DEFLATE slice. Unmetered: the caller meters the form it hands on
	/// with [`Self::meter_datagram`].
	pub(crate) fn poll_recv_stored_datagram(
		&mut self,
		waiter: &kio::Waiter,
	) -> Poll<Result<Option<super::datagram::Stored>>> {
		match &mut self.inner {
			Inner::Plain(cursor) => cursor.poll_recv_datagram(waiter),
			Inner::Resume(resume, _) => resume.poll_recv_datagram(&self.delivered, waiter),
		}
	}

	/// [`Self::poll_recv_stored_datagram`], metered as its plain payload: for a session that
	/// never carries compression.
	pub(crate) fn poll_recv_wire_datagram(
		&mut self,
		waiter: &kio::Waiter,
	) -> Poll<Result<Option<super::datagram::Stored>>> {
		let res = self.poll_recv_stored_datagram(waiter);
		if let Poll::Ready(Ok(Some(datagram))) = &res {
			self.meter_datagram(datagram.payload.len() as u64);
		}
		res
	}

	/// Receive the next datagram in arrival order.
	///
	/// A best-effort channel parallel to [`Self::recv_group`]; the two share only the sequence
	/// namespace. To receive both concurrently from one subscriber, poll
	/// [`Self::poll_recv_group`] and [`Self::poll_recv_datagram`] together in a single `poll`
	/// closure (sequential `&mut` borrows), rather than awaiting the two `recv` futures at once.
	#[cfg(feature = "async")]
	pub async fn recv_datagram(&mut self) -> Result<Option<Datagram>> {
		kio::wait(|waiter| self.poll_recv_datagram(waiter)).await
	}

	/// The sequence cursor behind [`Ordered`], which owns the only public door to it.
	pub(super) fn poll_next_group(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<group::Consumer>>> {
		self.poll_group(true, waiter)
	}

	/// Read this track's groups in sequence order instead of arrival order.
	///
	/// Consumes the subscriber, so one handle carries exactly one cursor: an
	/// arrival-order [`Subscriber`] or a sequence-order [`Ordered`], never both at once.
	/// The two advance independently, and interleaving them produces a group stream that
	/// is neither, which is why the choice is a handle rather than a method.
	///
	/// Datagrams come along: they are a separate cursor either way, so the choice of group
	/// order says nothing about them.
	pub fn ordered(self) -> Ordered {
		Ordered { inner: self }
	}

	/// Whether `other` was cloned from this subscriber (shares the same underlying state).
	pub fn is_clone(&self, other: &Self) -> bool {
		match (&self.inner, &other.inner) {
			(Inner::Plain(a), Inner::Plain(b)) => a.state.same_channel(&b.state),
			(Inner::Resume(a, _), Inner::Resume(b, _)) => a.subscription().same_channel(b.subscription()),
			_ => false,
		}
	}

	/// Poll for where the source's feed starts, raised to this cursor's floor, once
	/// resolved: the first group a publisher tells its peer it serves. A feed starting
	/// below the floor serves the floor's group too, and a fill this cursor waits on starts it
	/// lower still. `None` when the source declares none and no fill is out.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn poll_start(&mut self, waiter: &kio::Waiter) -> Poll<Option<u64>> {
		let cursor = match &mut self.inner {
			Inner::Plain(cursor) => cursor,
			Inner::Resume(resume, _) => return resume.poll_start(waiter),
		};
		let start = ready!(Consumer::poll_state_start(&cursor.state, waiter));
		cursor.sync();
		let start = start.map(|start| start.max(cursor.floor()));
		let filling = cursor.misses.as_ref().and_then(|misses| misses.lowest());
		Poll::Ready(match (start, filling) {
			(Some(start), Some(filling)) => Some(start.min(filling)),
			(start, filling) => start.or(filling),
		})
	}

	/// The lowest group this subscriber's ranges name, as resolved here when it last read,
	/// raised to the start of [`Self::with_range`]: `None` while one reaches back to the
	/// oldest group still around, or a relative start still waits on the first group from
	/// the live feed.
	///
	/// Where a reader that waits on each group in sequence starts, rather than at whatever
	/// arrives first, which newest-first delivery makes the latest.
	pub fn floor(&self) -> Option<u64> {
		let start = match &self.inner {
			Inner::Plain(cursor) => cursor.filter.window.start(),
			Inner::Resume(resume, _) => resume.floor(),
		};
		Some(start?.max(self.bounds().start))
	}

	/// Where each of this subscriber's ranges starts, by index, as resolved here and raised
	/// to `from`, where delivery starts: `None` for one with no group there or still waiting
	/// on the first group from the live feed.
	///
	/// A publisher tells its peer where a relative or time start landed, which the peer
	/// can't work out from the groups it receives.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn starts(&mut self, from: u64) -> Vec<Option<u64>> {
		match &mut self.inner {
			Inner::Plain(cursor) => {
				cursor.sync();
				cursor.filter.window.starts(from)
			}
			Inner::Resume(resume, _) => resume.starts(from),
		}
	}

	/// The lowest group at or above `from` this subscriber's ranges and
	/// [`Self::with_range`] floor cover, `None` when none does: the groups between are
	/// never read, so a reader waiting on each group in sequence need not wait for them.
	/// `Some` from there while a relative anchor waits on the first group, which can't
	/// say yet.
	pub fn next_in_range(&self, from: u64) -> Option<u64> {
		let from = from.max(self.bounds().start);
		match &self.inner {
			Inner::Plain(cursor) => cursor.filter.window.runs(from).next().map(|run| *run.start()),
			Inner::Resume(resume, _) => resume.next_in_range(from),
		}
	}

	/// Whether `accounted` holds every group from `from` on that this subscriber's ranges
	/// and [`Self::with_range`] bounds name, once every range has a resolved end: `false`
	/// while one stays open, counts back from a group not seen yet, or ends at a time not
	/// settled yet.
	///
	/// A publisher that accounts for each group it serves or drops learns from it that a
	/// bounded subscription has nothing more to deliver.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn covered(&mut self, from: u64, accounted: &rangemap::RangeSet<u64>) -> bool {
		let bounds = self.bounds();
		let from = from.max(bounds.start);
		match &mut self.inner {
			Inner::Plain(cursor) => {
				cursor.sync();
				cursor.filter.window.covered_by(from, bounds.end, accounted)
			}
			Inner::Resume(resume, _) => resume.covered(from, bounds.end, accounted),
		}
	}

	/// This subscriber's own ranges, by index, as resolved here: `None` for one empty or
	/// still waiting, and for every range of a front's reader.
	pub(super) fn resolved_ranges(&mut self) -> Vec<Option<Resolved>> {
		match &mut self.inner {
			Inner::Plain(cursor) => {
				cursor.sync();
				cursor.filter.window.resolved()
			}
			Inner::Resume(..) => Vec::new(),
		}
	}

	/// Why this subscriber's ranges can't be served from its track, the error its reads
	/// fail with; `None` for a front's reader, whose copies answer for it.
	pub(super) fn refused(&mut self) -> Option<Error> {
		match &mut self.inner {
			Inner::Plain(cursor) => {
				cursor.sync();
				cursor.filter.refused.clone()
			}
			Inner::Resume(..) => None,
		}
	}

	/// `ranges` resolved against this track's cache now, by index.
	pub(super) fn resolve_ranges(&self, ranges: &[Range]) -> Vec<Option<Resolved>> {
		match &self.inner {
			Inner::Plain(cursor) => {
				let state = cursor.state.read();
				Window::new(ranges, state.latest_live(), &*state)
					.map(|window| window.resolved())
					.unwrap_or_default()
			}
			Inner::Resume(..) => Vec::new(),
		}
	}

	/// Poll for a run of sequences this subscription gave up without serving, with the
	/// error it was given up with, so a publisher can tell its peer not to wait for them.
	///
	/// Stale skips are recorded only once this has been polled, so a reader that never
	/// forwards drops keeps none.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn poll_drop(&mut self, waiter: &kio::Waiter) -> Poll<(ops::Range<u64>, StreamError)> {
		match &mut self.inner {
			Inner::Plain(cursor) => cursor.poll_drop(waiter),
			Inner::Resume(resume, _) => resume.poll_drop(&self.delivered, waiter),
		}
	}

	/// Poll for the track's cache reflecting its live feed (or the track ending), with the
	/// largest position it holds; `None` for a track with nothing yet. A front's logical
	/// track answers for the route serving it. See [`Producer::set_idle`].
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn poll_live(&mut self, waiter: &kio::Waiter) -> Poll<Option<Position>> {
		let cursor = match &mut self.inner {
			Inner::Plain(cursor) => cursor,
			Inner::Resume(resume, _) => return resume.poll_live(waiter),
		};
		let res = cursor.state.poll(waiter, |state| match state.readable() {
			true => Poll::Ready(state.largest()),
			false => Poll::Pending,
		});
		match res {
			Poll::Ready(Ok(largest)) => Poll::Ready(largest),
			Poll::Ready(Err(state)) => Poll::Ready(state.largest()),
			Poll::Pending => Poll::Pending,
		}
	}

	/// Poll for the track's declared exclusive end, without blocking.
	pub fn poll_finished(&mut self, waiter: &kio::Waiter) -> Poll<Result<Position>> {
		match &mut self.inner {
			Inner::Plain(cursor) => cursor.poll(waiter, |state| state.poll_finished()),
			Inner::Resume(resume, _) => resume.poll_finished(waiter),
		}
	}

	/// Block until the track declares its end, returning the exclusive end position, or the
	/// cause on an abort. See [`Producer::finish_at`] for what the position means.
	///
	/// Resolves as soon as the end is known, which may be ahead of the live edge
	/// when the producer finished via [`Producer::finish_at`]. This reports the declared
	/// end, not that every group has arrived. A local session close without a declared
	/// end returns [`Error::Closed`]. Drive [`Self::recv_group`] (or
	/// [`Ordered::next_group`]) until it yields `None` to observe the track fully drained.
	#[cfg(feature = "async")]
	pub async fn finished(&mut self) -> Result<Position> {
		kio::wait(|waiter| self.poll_finished(waiter)).await
	}

	/// Limit subsequent reads to these group sequences, returning `self` for chaining.
	///
	/// `2..=5` includes groups 2 through 5; `2..5` excludes group 5, and `..0` is the
	/// empty range. Both bounds are assigned, in either direction: an omitted start is
	/// group 0 and an omitted end removes the cap. Lowering the floor or raising the cap
	/// offers the groups it newly covers that the cache still holds and this reader never
	/// handed out, in its [`Order`]; a group is never handed out twice. Groups past the cap
	/// stay cached, and lowering the cap below the cursor waits until it rises again or
	/// the track finishes. [`Self::poll_largest`] reports within these bounds.
	///
	/// A local filter, not a request: it tells the publisher nothing, so groups outside it
	/// are still delivered and simply not returned. To change what is requested, set
	/// [`Subscription::ranges`] via [`Self::update`]. See [Local cursor vs wire
	/// preference](Self#local-cursor-vs-wire-preference). One exception: on a track served
	/// across route changes, lowering the floor asks the serving route again for the groups
	/// it newly covers within those ranges, since the route that delivered them may be gone.
	pub fn with_range(mut self, groups: impl RangeBounds<u64>) -> Self {
		self.set_range(groups);
		self
	}

	/// [`Self::with_range`] on a reader held in place.
	pub(crate) fn set_range(&mut self, groups: impl RangeBounds<u64>) {
		let (start, end) = super::subscription::sequence_bounds(groups);
		match &mut self.inner {
			Inner::Plain(cursor) => {
				cursor.peeked = None;
				cursor.min_sequence = start;
				cursor.end_sequence = end;
				cursor.update_servable();
			}
			Inner::Resume(resume, _) => resume.set_range(start, end, &self.delivered),
		}
		self.largest.bounds_moved();
	}

	/// The local bounds `largest` reports within.
	fn bounds(&self) -> Bounds {
		match &self.inner {
			Inner::Plain(cursor) => Bounds {
				start: cursor.min_sequence,
				end: cursor.end_sequence,
			},
			Inner::Resume(resume, _) => resume.bounds(),
		}
	}

	/// This subscriber's current preferences.
	pub fn subscription(&self) -> Subscription {
		self.control().subscription()
	}

	/// Replace this subscriber's delivery preferences.
	///
	/// Stored verbatim; the publisher's max age window is applied to the aggregate, not
	/// here (see [`Producer::subscription`]). Replaced ranges resolve again here, a
	/// relative anchor counting back from the latest group now, and widening them offers
	/// the cached groups they newly cover, even ones that arrived before the groups
	/// already read. Returns [`Error::Closed`]
	/// if the track already ended; the update is meaningless at that point and can usually
	/// be ignored. Refuses a time anchor on an untimed track with
	/// [`Error::TimestampMismatch`], leaving the preferences as they were.
	pub fn update(&mut self, subscription: Subscription) -> Result<()> {
		refuse_untimed(&self.info, &subscription)?;
		let channel = match &self.inner {
			Inner::Plain(cursor) => &cursor.subscription,
			Inner::Resume(resume, _) => resume.subscription(),
		};
		if subscription.ranges != channel.read().ranges {
			self.consume().refuse_subscription(&subscription)?;
		}
		channel.update(subscription)?;
		match &mut self.inner {
			Inner::Plain(cursor) => {
				cursor.sync();
			}
			// The copies take the change now, not on the next read, so it reaches their
			// sources even while nobody reads.
			Inner::Resume(resume, _) => resume.mirror().sync(),
		}
		Ok(())
	}

	/// Poll for the track's largest location within [`Self::with_range`]: the current one, then each newer one, and `None` once the track ends.
	///
	/// The largest is the newest group or datagram at the newest frame (0 while empty), as
	/// the source reported it and its live feed delivered since, so nothing reports until
	/// the track's feed is live. A largest past the cap or below the floor reports nothing:
	/// the cache does not stand in for it, so a cap moved below the largest stays pending.
	/// Moving the bounds wakes a parked poll, which reports the largest once it is within
	/// them and newer than the last report.
	pub fn poll_largest(&mut self, waiter: &kio::Waiter) -> Poll<Option<Position>> {
		if self.largest.ended {
			return Poll::Ready(None);
		}
		let bounds = self.bounds();
		let poll = self.poll_largest_within(bounds, waiter);
		// Only a parked poll waits on the bounds; a finished one is not woken by them.
		match poll {
			Poll::Pending => waiter.register(&mut self.largest.parked),
			Poll::Ready(next) => {
				self.largest.parked = kio::WaiterList::new();
				self.largest.ended = next.is_none();
			}
		}
		poll
	}

	fn poll_largest_within(&mut self, bounds: Bounds, waiter: &kio::Waiter) -> Poll<Option<Position>> {
		let seen = &mut self.largest.position;
		match &mut self.inner {
			Inner::Plain(cursor) => {
				let state = &cursor.state;
				let cache = state.read().cache.clone();
				poll_largest(
					&cache,
					seen,
					bounds,
					waiter,
					|waiter| state.poll(waiter, |state| largest_live(state, waiter)).map(|_| ()),
					|waiter| state.poll(waiter, |state| largest_ended(state, waiter)).map(|_| ()),
				)
			}
			Inner::Resume(resume, _) => resume.poll_largest(seen, bounds, waiter),
		}
	}

	/// The largest location within [`Self::with_range`]; see [`Self::poll_largest`].
	#[cfg(feature = "async")]
	pub async fn largest(&mut self) -> Option<Position> {
		kio::wait(|waiter| self.poll_largest(waiter)).await
	}

	/// The track's largest location known now, ignoring [`Self::with_range`], without waiting
	/// or moving [`Self::poll_largest`] on: past what the cache holds once the source
	/// reported a newer one.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn peek_largest(&mut self) -> Option<Position> {
		let mut seen = None;
		match &mut self.inner {
			Inner::Plain(cursor) => cursor.state.read().cache.take_largest(&mut seen, Bounds::default()),
			Inner::Resume(resume, _) => match resume.poll_largest(&mut seen, Bounds::default(), &kio::Waiter::noop()) {
				Poll::Ready(largest) => largest,
				Poll::Pending => None,
			},
		}
	}

	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn latest_sequence(&self) -> Option<u64> {
		match &self.inner {
			Inner::Plain(cursor) => cursor.state.read().max_sequence,
			Inner::Resume(resume, _) => resume.latest(),
		}
	}

	/// Join again at the live edge, judged by [`Info::refresh`] like a new subscriber: a
	/// moq-transport NEW_GROUP_REQUEST. A front's reader joins the copy serving it.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn join(&self) {
		match &self.inner {
			Inner::Plain(cursor) => TrackState::join(cursor.state.read()),
			Inner::Resume(_, state) => {
				let routes = state.read().routes.clone();
				if let Some(copy) = routes.and_then(|routes| routes.serving()) {
					copy.join();
				}
			}
		}
	}
}

/// A [`Subscriber`] that reads groups in sequence order.
///
/// Created by [`Subscriber::ordered`], which consumes the subscriber, so a track is read
/// one way or the other and never both. Every group it returns has a higher sequence
/// than the last, so a late arrival (network reordering, or a gap the cache filled after
/// the fact) is skipped rather than delivered out of turn.
///
/// # Age and skipping
///
/// [`Subscription::max_delay`] applies as this cursor reads, exactly as it does on the
/// arrival cursor: a group is skipped once its *reach*, where its immediate successor
/// begins, is that far behind the newest frame on the track. Nothing weaker convicts it,
/// because the reach is the only proof that *every* frame it could still hold is past the
/// budget: a group's own timestamps say where it starts presenting, not where it stops,
/// and an unstamped successor leaves it unbounded and therefore kept. A backlog inside
/// the budget is still delivered whole, as a burst in order, which is what a decoder
/// reading a gap-free sequence needs.
///
/// The same budget follows a group already handed out: once newer content pulls that far
/// ahead, a read still waiting on it ends with [`Error::Old`] and the cursor moves on. A
/// gap costs nothing either way, since the cursor seeks to the lowest cached group in
/// range rather than waiting for the sequence it would have read next.
///
/// The budget is updatable mid-stream through [`Self::control`]. [`Duration::ZERO`] (the
/// default) keeps only what nothing newer has superseded, so a consumer that stalls and
/// resumes rejoins the live edge instead of replaying at 1x.
pub struct Ordered {
	inner: Subscriber,
}

impl Ordered {
	/// The track's [`Info`], resolved when the subscription was established.
	pub fn info(&self) -> &Info {
		self.inner.info()
	}

	/// The track's name, unique within its broadcast.
	pub fn name(&self) -> &str {
		self.inner.name()
	}

	/// The broadcast this track belongs to, as reached through the handle it came from.
	pub fn broadcast(&self) -> &broadcast::Info {
		self.inner.broadcast()
	}

	/// Poll for the next group with a higher sequence number than any previously
	/// returned, without blocking.
	///
	/// Honors the group range set by [`Self::with_range`]:
	/// a group past the cap stays in the producer's cache and becomes eligible again if
	/// the cap rises or is removed.
	///
	/// Returns `Poll::Ready(Ok(Some(group)))` when a group is available,
	/// `Poll::Ready(Ok(None))` when the track is finished (including an abort after its
	/// declared end settled, see [`Producer::abort`]),
	/// `Poll::Ready(Err(e))` when the track has been aborted short of its end, or
	/// `Poll::Pending` when no group is available yet.
	pub fn poll_next_group(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<group::Consumer>>> {
		self.inner.poll_next_group(waiter)
	}

	/// Return the next group with a higher sequence number than any previously returned.
	#[cfg(feature = "async")]
	pub async fn next_group(&mut self) -> Result<Option<group::Consumer>> {
		kio::wait(|waiter| self.poll_next_group(waiter)).await
	}

	/// Poll for the next datagram in arrival order, without blocking.
	///
	/// Datagrams are a separate best-effort channel from groups (see
	/// [`Producer::append_datagram`]); they share only the sequence namespace, and neither
	/// cursor moves the other. Unordered by construction, so this behaves identically on
	/// either handle; it is here so a track carrying both channels needs one subscription
	/// rather than two.
	pub fn poll_recv_datagram(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Datagram>>> {
		self.inner.poll_recv_datagram(waiter)
	}

	/// Receive the next datagram in arrival order.
	///
	/// To read groups and datagrams concurrently from one handle, poll
	/// [`Self::poll_next_group`] and [`Self::poll_recv_datagram`] together in a single
	/// `poll` closure (sequential `&mut` borrows), rather than awaiting the two `recv`
	/// futures at once.
	#[cfg(feature = "async")]
	pub async fn recv_datagram(&mut self) -> Result<Option<Datagram>> {
		kio::wait(|waiter| self.poll_recv_datagram(waiter)).await
	}

	/// Limit subsequent reads to these group sequences, returning `self` for chaining.
	///
	/// See [`Subscriber::with_range`] for how the bounds apply. An ordered read never goes
	/// back, so lowering the floor below a group it already returned offers nothing.
	pub fn with_range(mut self, groups: impl RangeBounds<u64>) -> Self {
		self.set_range(groups);
		self
	}

	/// [`Self::with_range`] on a reader held in place.
	fn set_range(&mut self, groups: impl RangeBounds<u64>) {
		self.inner.set_range(groups);
	}

	/// Create a handle for updating this subscriber's delivery preferences without
	/// borrowing the read cursor.
	pub fn control(&self) -> Control {
		self.inner.control()
	}

	/// This subscriber's current preferences.
	pub fn subscription(&self) -> Subscription {
		self.inner.subscription()
	}

	/// Replace this subscriber's delivery preferences.
	///
	/// Returns [`Error::Closed`] if the track already ended.
	pub fn update(&mut self, subscription: Subscription) -> Result<()> {
		self.inner.update(subscription)
	}

	/// Block until the track declares its end, returning the exclusive end position.
	///
	/// See [`Subscriber::finished`]: this reports the declared end, not that every group
	/// has arrived.
	#[cfg(feature = "async")]
	pub async fn finished(&mut self) -> Result<Position> {
		kio::wait(|waiter| self.inner.poll_finished(waiter)).await
	}

	/// Poll for the largest location within [`Self::with_range`]; see [`Subscriber::poll_largest`].
	pub fn poll_largest(&mut self, waiter: &kio::Waiter) -> Poll<Option<Position>> {
		self.inner.poll_largest(waiter)
	}

	/// The largest location within [`Self::with_range`]; see [`Subscriber::poll_largest`].
	#[cfg(feature = "async")]
	pub async fn largest(&mut self) -> Option<Position> {
		kio::wait(|waiter| self.poll_largest(waiter)).await
	}

	/// Whether `other` reads the same underlying track state.
	pub fn is_clone(&self, other: &Self) -> bool {
		self.inner.is_clone(&other.inner)
	}
}

/// A requested track being written before any subscriber sees it; see [`Request::stage`].
///
/// Write through it as through the [`Producer`] it derefs to. Subscribers waiting on the
/// request resolve only at [`Self::commit`], with everything written by then. Dropping it
/// uncommitted rejects the request with [`Error::Dropped`].
#[must_use = "a staged track is rejected unless committed"]
pub(crate) struct Staged {
	// Taken only by `commit`.
	producer: Option<Producer>,
	// The request's fetch claim, held until the track is published like an unanswered
	// request's: a cache miss meanwhile waits rather than racing its own handler.
	_dynamic: Requests,
}

impl Staged {
	/// Publish the track: every waiting subscriber resolves now, with what was written.
	pub(crate) fn commit(mut self) -> Producer {
		let producer = self.producer.take().expect("taken only by commit");
		if let Ok(mut state) = producer.state.write() {
			state.staged = false;
		}
		producer
	}
}

impl ops::Deref for Staged {
	type Target = Producer;

	fn deref(&self) -> &Producer {
		self.producer.as_ref().expect("taken only by commit")
	}
}

impl ops::DerefMut for Staged {
	fn deref_mut(&mut self) -> &mut Producer {
		self.producer.as_mut().expect("taken only by commit")
	}
}

impl Drop for Staged {
	fn drop(&mut self) {
		let Some(producer) = self.producer.take() else {
			return;
		};
		// Aborted like the producer it derefs to, so a group written through it ends too.
		let _ = producer.abort(Error::Dropped);
	}
}

/// A subscriber asked for a track this broadcast doesn't have yet.
///
/// Yielded by [`poll_requested_track`](crate::broadcast::Requests::poll_requested_track),
/// or created up front with [`broadcast::Producer::reserve_track`](crate::broadcast::Producer::reserve_track).
/// Subscribers block until the request is
/// resolved: call [`accept`](Self::accept) to serve it with a [`Producer`], or
/// [`reject`](Self::reject) to fail them. Dropping it without either rejects with
/// [`Error::Dropped`].
///
/// Concurrent requests for one name are coalesced, so exactly one of these exists per
/// name at a time.
pub struct Request {
	name: Arc<str>,
	// The parent broadcast's info, threaded into the [`Producer`] on accept.
	broadcast: Arc<broadcast::Info>,
	state: kio::Producer<TrackState>,

	// The previous subscription that was combined, used to detect changes.
	prev_subscription: Option<Subscription>,

	// Shared with the accepted [`Producer`] and every [`Requests`]: its `Drop` is the
	// teardown, and it stays inert until a producer is minted.
	alive: Arc<Alive>,

	// A requested track is served on demand, so it counts as fetch-capable from
	// birth: a consumer's cache-miss `fetch_group` waits to be served instead of
	// racing the producer (e.g. a relay) into creating its own handler. Released
	// when the request is accepted or dropped; by then the relay holds its own.
	_dynamic: Requests,

	// Ingress stats scope, threaded into the accepted [`Producer`]. Empty (no-op)
	// unless this request was reserved on a tagged broadcast.
	stats: stats::Scope,

	// The floor the accepted track's start is pending at, until the source resolves it
	// (see [`Self::request_start`]).
	requested_start: Option<Option<u64>>,

	// Whether the accepted track starts idle; see [`Self::idle`].
	idle: bool,

	// Served from a front's routes; see [`Self::routes`].
	routes: Option<super::resume::Consumer>,

	// A track the producer reserved, listed by its broadcast once accepted.
	list: Option<broadcast::ListOnAccept>,
}

impl Request {
	pub(super) fn new(broadcast: Arc<broadcast::Info>, name: impl Into<Arc<str>>) -> Self {
		let name = name.into();
		let state = TrackState::spawn(broadcast.clone());
		let alive = Alive::new(name.clone(), state.clone());
		let dynamic = Requests::new(name.clone(), state.clone(), alive.clone());
		Self {
			name,
			broadcast,
			state,
			prev_subscription: None,
			alive,
			_dynamic: dynamic,
			stats: stats::Scope::default(),
			requested_start: None,
			idle: false,
			routes: None,
			list: None,
		}
	}

	/// List the track in its broadcast once accepted: the producer reserved it.
	pub(super) fn listed(mut self, list: broadcast::ListOnAccept) -> Self {
		self.list = Some(list);
		self
	}

	/// Serve the track from a front's routes, read straight from their copies; see
	/// [`super::resume`]. Applied atomically with [`Self::accept`], so no reader ever sees
	/// the accepted track without it.
	pub(super) fn routes(mut self, routes: super::resume::Consumer) -> Self {
		self.routes = Some(routes);
		self
	}

	/// Accept the track with its start pending at `floor`, the first group its source was
	/// asked for (`None` for the live edge), until the source says where its feed starts
	/// with [`Producer::start_at`].
	///
	/// Nothing below `floor` arrives, as with [`Producer::start_at`], and groups are read
	/// as they arrive, but a relay serving the track waits for the resolved start before
	/// declaring its own. Applied atomically with [`Self::accept`], before any reader can
	/// see the track.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn request_start(mut self, floor: impl Into<Option<u64>>) -> Self {
		self.requested_start = Some(floor.into());
		self
	}

	/// Accept the track idle, as a copy that no live feed fills yet: readers get nothing
	/// from it until the feed goes live. Applied atomically with [`Self::accept`].
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn idle(mut self) -> Self {
		self.idle = true;
		self
	}

	/// Attach an ingress stats scope, applied to the [`Producer`] on accept. Set by
	/// a tagged [`broadcast::Producer::reserve_track`].
	pub(super) fn with_stats(mut self, scope: stats::Scope) -> Self {
		self.stats = scope;
		self
	}

	/// The requested track name.
	pub fn name(&self) -> &str {
		&self.name
	}

	/// A [`Consumer`] for the eventual track, usable before the request is accepted.
	pub fn consume(&self) -> Consumer {
		Consumer::new(self.name.clone(), self.state.weak().consume_weak())
	}

	/// Create a [`Requests`] handle that serves on-demand fetches of uncached
	/// groups, before [`Self::accept`] is even called. A relay creates one to fetch
	/// past groups from upstream while (or instead of) serving a live subscription.
	pub fn requests(&self) -> Requests {
		Requests::new(self.name.clone(), self.state.clone(), self.alive.clone())
	}

	/// Watch subscriber demand without keeping the track alive.
	pub fn demand(&self) -> Demand {
		Demand::new(self.name.clone(), self.state.weak())
	}

	/// Join the eventual track's demand, for a request made through its broadcast.
	pub(super) fn join(&self, subscription: Option<&Subscription>) -> (kio::Consumer<TrackState>, Option<Preferences>) {
		let preferences = subscription.map(|value| Preferences::register(self.state.read(), value.clone()));
		(self.state.consume(), preferences)
	}

	/// Read the eventual track without joining its demand.
	pub(super) fn peek(&self) -> kio::ConsumerWeak<TrackState> {
		self.state.weak().consume_weak()
	}

	/// Mark this request as taken by a dynamic handler, which alone decides its answer.
	pub(super) fn claim(self) -> Self {
		if let Ok(mut state) = self.state.write() {
			state.claimed = true;
		}
		self
	}

	/// Reject only while no consumer needs this pending track. Demand and the check
	/// share one lock, so demand returning after `Demand::wanted` went to `None` wins the race, and the
	/// close under that lock stops a later consumer attaching to a dead request.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn reject_unwanted(&self, err: Error) -> bool {
		match self.state.write_unwanted() {
			kio::Unwanted::Idle(guard) => {
				commit_abort(guard, err).abort();
				true
			}
			kio::Unwanted::Closed => true,
			kio::Unwanted::Wanted => false,
		}
	}

	/// Serve the request with the given track, resolving every waiting subscriber.
	///
	/// The name is taken from [`Self::name`]; `info` supplies the remaining knobs
	/// (`None` for the defaults). If the track was already aborted, the returned
	/// [`Producer`] is inert: writes fail with the abort error, as if it had been
	/// aborted immediately after accepting.
	pub fn accept(self, info: impl Into<Option<Info>>) -> Producer {
		self.stage(info).commit()
	}

	/// Mint the track's [`Producer`] without resolving any subscriber yet, so content
	/// written through the returned [`Staged`] is there when the first reader looks.
	///
	/// For a relay that already holds groups of the track: write them, then
	/// [`Staged::commit`]. Dropping the [`Staged`] uncommitted rejects the request with
	/// [`Error::Dropped`].
	pub(crate) fn stage(self, info: impl Into<Option<Info>>) -> Staged {
		let info = info.into().unwrap_or_default();
		// A closed state means the track was aborted under us. Mirror `reject` and
		// tolerate it: the Producer we hand back simply can't write.
		let open = match self.state.write() {
			Ok(mut state) => {
				state.accept(info.clone());
				state.staged = true;
				if let Some(floor) = self.requested_start {
					state.set_start(floor, true);
				}
				if self.idle {
					state.set_idle();
				}
				state.routes = self.routes;
				true
			}
			Err(_) => false,
		};
		// Accepting the request creates the track producer: count it as one ingress
		// subscription (closed when the last handle drops). No-op when untagged.
		self.alive.publish(Some(&self.stats));
		// An aborted track already left the table, so it is never listed.
		if let Some(list) = self.list.filter(|_| open) {
			self.alive.list(list.list(&info));
		}
		Staged {
			producer: Some(Producer {
				name: self.name,
				info,
				broadcast: self.broadcast,
				state: self.state,
				prev_subscription: None,
				alive: self.alive,
				stats: self.stats,
			}),
			_dynamic: self._dynamic,
		}
	}

	/// Reject the request, waking all waiting subscribers with `err`.
	pub fn reject(self, err: Error) {
		if let Ok(mut state) = self.state.write() {
			state.abort = Some(err);
		}
	}

	/// The delivery preferences aggregated across everyone waiting on this request,
	/// or `None` if nobody is waiting. Useful for sizing the track before accepting.
	pub fn subscription(&self) -> Option<Subscription> {
		let state = self.state.read();
		let (subs, bound) = (state.subscriptions.clone(), state.max_age_bound());
		drop(state);
		snapshot_subscription(&subs, bound)
	}

	/// Block until the aggregate [`subscription`](Self::subscription) changes,
	/// yielding `None` once nobody is waiting.
	#[cfg(feature = "async")]
	pub async fn subscription_changed(&mut self) -> Option<Subscription> {
		kio::wait(|waiter| self.poll_subscription_changed(waiter)).await
	}

	/// Poll for a changed subscription.
	pub fn poll_subscription_changed(&mut self, waiter: &kio::Waiter) -> Poll<Option<Subscription>> {
		let state = self.state.read();
		let (subs, bound) = (state.subscriptions.clone(), state.max_age_bound());
		drop(state);

		poll_combined_changed(&subs, bound, &mut self.prev_subscription, waiter)
	}

	pub(super) fn weak(&self) -> TrackWeak {
		TrackWeak {
			name: self.name.clone(),
			state: self.state.weak(),
		}
	}
}

#[cfg(test)]
use futures::FutureExt;

#[cfg(test)]
#[expect(missing_docs, reason = "test-only assertion helpers")]
impl Subscriber {
	pub fn assert_group(&mut self) -> group::Consumer {
		kio::wait(|waiter| self.poll_recv_group(waiter))
			.now_or_never()
			.expect("group would have blocked")
			.expect("would have errored")
			.expect("track was closed")
	}

	pub fn assert_no_group(&mut self) {
		assert!(
			kio::wait(|waiter| self.poll_recv_group(waiter))
				.now_or_never()
				.is_none(),
			"recv_group would not have blocked"
		);
	}

	pub fn assert_not_closed(&mut self) {
		assert!(
			kio::wait(|waiter| self.poll_finished(waiter)).now_or_never().is_none(),
			"should not be closed"
		);
	}

	pub fn assert_closed(&mut self) {
		assert!(
			kio::wait(|waiter| self.poll_finished(waiter)).now_or_never().is_some(),
			"should be closed"
		);
	}

	pub fn assert_is_clone(&self, other: &Self) {
		assert!(self.is_clone(other), "should be clone");
	}

	pub fn assert_not_clone(&self, other: &Self) {
		assert!(!self.is_clone(other), "should not be clone");
	}
}

#[cfg(test)]
mod test {
	use super::*;
	use crate::frame;
	use crate::model::test_tracing::count_drop_warnings;
	use std::time::Duration;

	/// Mint a track for tests with a default parent broadcast, since tracks are
	/// normally born from a [`broadcast::Producer`].
	fn track_producer(name: impl Into<Arc<str>>, info: impl Into<Option<Info>>) -> Producer {
		Producer::new(Arc::new(broadcast::Info::default()), name, info)
	}

	#[test]
	fn drop_changes_follow_split_coalesced_and_recreated_runs() {
		let producer = track_producer("test", Info::default());
		let indexed = || {
			let state = producer.state.read();
			let mut changed: Vec<_> = state.drops_changes.since(0).collect();
			changed.sort_by_key(|run| run.start);
			let current: Vec<_> = state.drops.iter().map(|(run, _)| run.clone()).collect();
			assert_eq!(changed, current, "one change entry per current drop run");
		};
		producer.drop_groups(10..20, StreamError::Old);
		producer.drop_groups(20..30, StreamError::Old);
		indexed();
		producer.drop_groups(15..25, StreamError::Cancel);
		indexed();
		producer
			.create_group(group::Info { sequence: 17 })
			.unwrap()
			.finish()
			.unwrap();
		indexed();
		producer.drop_groups(14..26, StreamError::Old);
		indexed();
		assert_eq!(
			producer.state.read().drops_changes.since(0).collect::<Vec<_>>(),
			vec![10..30]
		);
	}

	/// Let `duration` pass on the cache pool `producer`'s groups charge into.
	fn elapse(producer: &Producer, duration: Duration) {
		producer.broadcast.pool.step(duration);
	}

	#[test]
	fn a_missing_group_uses_the_subscription_grace() {
		let producer = track_producer("test", Info::default());
		let budget = Duration::from_millis(50);
		let _sub = producer.subscribe(Subscription::default().with_max_delay(budget));
		let feed = producer.consume();
		let waiter = kio::Waiter::noop();
		producer
			.create_group(group::Info { sequence: 1 })
			.unwrap()
			.finish()
			.unwrap();
		assert!(feed.poll_group(0, &waiter).is_pending());
		assert!(feed.poll_group(u64::MAX, &waiter).is_pending());
		elapse(&producer, budget / 2);
		assert!(feed.poll_group(0, &waiter).is_pending());
		elapse(&producer, budget / 2);
		assert!(matches!(feed.poll_group(0, &waiter), Poll::Ready(Ok(None))));
	}

	#[test]
	fn an_explicit_drop_does_not_wait_for_an_arrival_gap() {
		let producer = track_producer("test", Info::default());
		let feed = producer.consume();
		let waiter = kio::Waiter::noop();
		assert!(feed.poll_group(1, &waiter).is_pending());
		producer.drop_groups(1..2, StreamError::Old);
		// The wait ends with the drop's own error.
		assert!(matches!(
			feed.poll_group(1, &waiter),
			Poll::Ready(Err(Error::Stream(StreamError::Old)))
		));
	}

	/// A bounded replay window for tests whose subject requires every buffered group.
	fn replay() -> Subscription {
		Subscription::default().with_max_delay(Duration::from_secs(30))
	}

	/// Helper: count live cached groups in state.
	fn live_groups(state: &TrackState) -> usize {
		state.lookup.len()
	}

	/// Helper: get the sequence number of the first live group in arrival order.
	fn first_live_sequence(state: &TrackState) -> u64 {
		state
			.arrival
			.iter()
			.find(|(sequence, stamp)| state.lookup.get(sequence).is_some_and(|slot| slot.stamp == *stamp))
			.map(|(sequence, _)| *sequence)
			.unwrap()
	}

	/// Helper: non-blocking datagram receive that must be ready with a datagram.
	#[cfg(feature = "async")]
	fn recv_datagram(dg: &mut Subscriber) -> Datagram {
		dg.recv_datagram()
			.now_or_never()
			.expect("datagram would have blocked")
			.expect("would have errored")
			.expect("track was closed")
	}

	#[test]
	fn append_datagram_shares_group_sequence() {
		let mut producer = track_producer("test", Info::timed());
		let ts = Timestamp::from_millis(10).unwrap();

		// Interleave groups and datagrams: they draw from one monotonic counter.
		assert_eq!(producer.append_group().unwrap().sequence, 0);
		assert_eq!(producer.append_datagram(ts, &b"a"[..]).unwrap(), 1);
		assert_eq!(producer.append_group().unwrap().sequence, 2);
		assert_eq!(producer.append_datagram(ts, &b"b"[..]).unwrap(), 3);
		assert_eq!(producer.latest_sequence(), Some(3));
	}

	/// An empty group raises the largest at its head, so its first frame names nothing newer.
	#[cfg(feature = "async")]
	#[moq_clock::test]
	async fn largest_streams_groups_and_frames_and_coalesces() {
		let producer = track_producer("test", Info::default());
		let mut subscriber = producer.subscribe(replay());

		assert_eq!(producer.largest(), None);
		let mut group = producer.append_group().unwrap();
		assert_eq!(
			subscriber.largest().await,
			Some(Position::group(0)),
			"an empty group is the largest at its head"
		);

		group.write_frame(None, &b"a"[..]).unwrap();
		assert_eq!(producer.largest(), Some(Position { group: 0, frame: 0 }));
		assert!(
			subscriber.largest().now_or_never().is_none(),
			"the first frame is the head already named"
		);

		group.write_frame(None, &b"b"[..]).unwrap();
		group.write_frame(None, &b"c"[..]).unwrap();
		assert_eq!(subscriber.largest().await, Some(Position { group: 0, frame: 2 }));

		group.finish().unwrap();
		producer.finish().unwrap();
		drop(group);
		drop(producer);
		assert_eq!(subscriber.largest().await, None);
	}

	/// Every sequence below a reached end already raised the largest, so a late group below
	/// it changes nothing, and the ended stream stays ended.
	#[test]
	fn a_late_group_below_the_largest_changes_nothing() {
		let mut producer = track_producer("test", Info::default());
		let noop = kio::Waiter::noop();
		let mut framed = producer.create_group(group::Info { sequence: 2 }).unwrap();
		framed.write_frame(None, &b"a"[..]).unwrap();
		framed.finish().unwrap();
		producer
			.create_group(group::Info { sequence: 5 })
			.unwrap()
			.finish()
			.unwrap();
		producer.finish_at(Position::group(6)).unwrap();

		let mut reader = producer.consume();
		assert_eq!(reader.poll_largest(&noop), Poll::Ready(Some(Position::group(5))));
		assert_eq!(reader.poll_largest(&noop), Poll::Ready(None));

		// Late content below the end is still admitted; it just isn't the largest.
		let mut late = producer.create_group(group::Info { sequence: 4 }).unwrap();
		late.write_frame(None, &b"b"[..]).unwrap();
		assert_eq!(reader.poll_largest(&noop), Poll::Ready(None));
		let mut fresh = producer.consume();
		assert_eq!(fresh.poll_largest(&noop), Poll::Ready(Some(Position::group(5))));
	}

	/// An aborted largest group can still be recreated, so the stream stays open past the
	/// end until a copy of it finishes, and the recreated frames are reported before it ends.
	#[test]
	fn a_recreated_largest_group_is_reported_before_the_end() {
		let mut producer = track_producer("test", Info::default());
		let noop = kio::Waiter::noop();
		let mut first = producer.create_group(group::Info { sequence: 0 }).unwrap();
		first.write_frame(None, &b"a"[..]).unwrap();
		first.abort(Error::Cancel).unwrap();
		producer.finish_at(Position::group(1)).unwrap();

		let mut reader = producer.consume();
		assert_eq!(reader.poll_largest(&noop), Poll::Ready(Some(Position::group(0))));
		assert!(
			reader.poll_largest(&noop).is_pending(),
			"the aborted group can be recreated"
		);

		let mut again = producer.create_group(group::Info { sequence: 0 }).unwrap();
		for payload in [&b"a"[..], b"b"] {
			again.write_frame(None, payload).unwrap();
		}
		assert_eq!(
			reader.poll_largest(&noop),
			Poll::Ready(Some(Position { group: 0, frame: 1 }))
		);
		again.finish().unwrap();
		assert_eq!(reader.poll_largest(&noop), Poll::Ready(None));
	}

	/// A copy a transport bound cut short finishes only its own range, so the largest group
	/// may still grow and the stream stays open past the end.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn a_finished_bounded_copy_keeps_the_largest_open() {
		let mut producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		let noop = kio::Waiter::noop();
		let mut first = producer.create_group(group::Info { sequence: 0 }).unwrap();
		first.write_frame(Timestamp::ZERO, &b"a"[..]).unwrap();
		first.abort(Error::Cancel).unwrap();
		producer.finish_at(Position::group(1)).unwrap();

		let mut reader = producer.consume();
		assert_eq!(reader.poll_largest(&noop), Poll::Ready(Some(Position::group(0))));
		let fetch = reader.fetch_range(
			0,
			FetchOptions {
				options: group::Fetch::default(),
				end: Some(1),
			},
		);
		let mut bounded = dynamic
			.requested_fill_group()
			.await
			.unwrap()
			.accept(Info::timed())
			.unwrap();
		bounded.write_frame(Timestamp::ZERO, &b"a"[..]).unwrap();
		bounded.finish().unwrap();
		fetch.await.unwrap();
		assert!(
			reader.poll_largest(&noop).is_pending(),
			"the bounded copy does not hold the whole group"
		);
	}

	/// A datagram raises the largest like a group, at its only object.
	#[test]
	fn a_datagram_raises_the_largest() {
		let mut producer = track_producer("test", Info::default());
		producer.append_group().unwrap().finish().unwrap();
		let sequence = producer.append_datagram(None, &b"d"[..]).unwrap();
		assert_eq!(sequence, 1);
		assert_eq!(producer.largest(), Some(Position::group(1)));
	}

	/// A reader that starts at the largest group while it is still empty is at the live edge:
	/// it gets that group, which ends with nothing, then the next one.
	#[test]
	fn a_live_edge_reader_starts_on_an_empty_largest_group() {
		let producer = track_producer("test", Info::default());
		let noop = kio::Waiter::noop();
		let mut first = producer.append_group().unwrap();
		first.write_frame(None, &b"a"[..]).unwrap();
		first.finish().unwrap();
		let mut empty = producer.append_group().unwrap();

		let mut reader = producer.subscribe(replay());
		let Poll::Ready(Some(edge)) = reader.poll_largest(&noop) else {
			panic!("the empty group is the largest");
		};
		assert_eq!(edge, Position::group(1));
		reader.set_range(edge.group..);

		empty.finish().unwrap();
		let mut next = producer.append_group().unwrap();
		next.write_frame(None, &b"b"[..]).unwrap();

		let mut group = reader.assert_group();
		assert_eq!(group.sequence, 1);
		assert!(
			matches!(group.poll_read_frame(&noop), Poll::Ready(Ok(None))),
			"it ends with nothing"
		);
		let mut group = reader.assert_group();
		assert_eq!(group.sequence, 2);
		assert!(matches!(group.poll_read_frame(&noop), Poll::Ready(Ok(Some(_)))));
	}

	/// A finished track ends the stream while its producer is still held, once the last
	/// group finishes, and finishing it wakes a parked reader.
	#[test]
	fn largest_ends_at_the_declared_end() {
		use std::sync::atomic::{AtomicBool, Ordering};

		struct Flag(AtomicBool);
		impl futures::task::ArcWake for Flag {
			fn wake_by_ref(flag: &Arc<Self>) {
				flag.0.store(true, Ordering::SeqCst);
			}
		}

		let producer = track_producer("test", Info::default());
		let mut subscriber = producer.subscribe(replay());
		let mut group = producer.append_group().unwrap();
		group.write_frame(None, &b"a"[..]).unwrap();
		let noop = kio::Waiter::noop();
		assert_eq!(
			subscriber.poll_largest(&noop),
			Poll::Ready(Some(Position { group: 0, frame: 0 }))
		);

		producer.finish().unwrap();
		assert!(
			subscriber.poll_largest(&noop).is_pending(),
			"the open group can still grow"
		);
		group.write_frame(None, &b"b"[..]).unwrap();
		assert_eq!(
			subscriber.poll_largest(&noop),
			Poll::Ready(Some(Position { group: 0, frame: 1 }))
		);

		let flag = Arc::new(Flag(AtomicBool::new(false)));
		let waiter = kio::Waiter::new(futures::task::waker(flag.clone()));
		assert!(subscriber.poll_largest(&waiter).is_pending());
		group.finish().unwrap();
		assert!(
			flag.0.load(Ordering::SeqCst),
			"finishing the last group woke the reader"
		);
		assert_eq!(subscriber.poll_largest(&noop), Poll::Ready(None));
		drop(producer);
	}

	#[cfg(feature = "async")]
	#[moq_clock::test]
	async fn largest_starts_with_the_sources_report() {
		let producer = track_producer("test", Info::default());
		let mut subscriber = producer.subscribe(replay());
		producer.report_largest(Position { group: 7, frame: 3 });
		assert_eq!(subscriber.largest().await, Some(Position { group: 7, frame: 3 }));

		// A report is a hint about the edge, not a cached group.
		assert!(subscriber.recv_group().now_or_never().is_none());

		// An older report or frame does not move it back.
		producer.report_largest(Position { group: 7, frame: 1 });
		let mut _open = producer.create_group(group::Info { sequence: 2 }).unwrap();
		_open.write_frame(None, &b"a"[..]).unwrap();
		assert!(subscriber.largest().now_or_never().is_none());

		producer.report_largest(Position { group: 8, frame: 0 });
		assert_eq!(subscriber.largest().await, Some(Position { group: 8, frame: 0 }));
	}

	/// Wakes record that a parked poll was woken.
	struct Flag(std::sync::atomic::AtomicBool);

	impl futures::task::ArcWake for Flag {
		fn wake_by_ref(flag: &Arc<Self>) {
			flag.0.store(true, std::sync::atomic::Ordering::SeqCst);
		}
	}

	impl Flag {
		fn waiter() -> (Arc<Self>, kio::Waiter) {
			let flag = Arc::new(Self(Default::default()));
			let waiter = kio::Waiter::new(futures::task::waker(flag.clone()));
			(flag, waiter)
		}

		fn take(&self) -> bool {
			self.0.swap(false, std::sync::atomic::Ordering::SeqCst)
		}
	}

	/// Write group `sequence` with `frames` frames, left open.
	fn framed(producer: &Producer, sequence: u64, frames: usize) -> group::Producer {
		let mut group = producer.create_group(group::Info { sequence }).unwrap();
		for _ in 0..frames {
			group.write_frame(None, &b"x"[..]).unwrap();
		}
		group
	}

	/// Every group a reader hands out with the frame indices it reads, until one would block.
	fn read_groups(
		mut next: impl FnMut(&kio::Waiter) -> Poll<Result<Option<group::Consumer>>>,
	) -> Vec<(u64, Vec<u64>)> {
		let noop = kio::Waiter::noop();
		let mut groups = Vec::new();
		while let Poll::Ready(group) = next(&noop) {
			let Some(mut group) = group.unwrap() else { break };
			let mut frames = Vec::new();
			loop {
				let index = group.index();
				match group.poll_read_frame(&noop) {
					Poll::Ready(Ok(Some(_))) => frames.push(index),
					Poll::Ready(Ok(None)) => break,
					res => panic!("group {} read {res:?}", group.sequence),
				}
			}
			groups.push((group.sequence, frames));
		}
		groups
	}

	/// A range starting and ending inside cached groups reads only the frames it covers,
	/// in arrival and sequence order alike, however wide another subscriber made the
	/// aggregate.
	#[test]
	fn a_range_reads_only_its_groups() {
		let producer = track_producer("test", Info::default());
		let _groups: Vec<_> = (3..8)
			.map(|sequence| {
				let mut group = framed(&producer, sequence, 8);
				group.finish().unwrap();
				group
			})
			.collect();
		let _wide = producer.subscribe(replay());
		let range = Range {
			start: Anchor::Absolute(4),
			end: Some(Anchor::Absolute(6)),
		};
		let bounded = || producer.subscribe(replay().with_ranges([range]));

		let want = vec![(4, (0..8).collect()), (5, (0..8).collect())];
		let mut arrival = bounded();
		assert_eq!(read_groups(|waiter| arrival.poll_recv_group(waiter)), want);
		let mut ordered = bounded().ordered();
		assert_eq!(read_groups(|waiter| ordered.poll_next_group(waiter)), want);
	}

	/// A group missing its head is never served short: reading it is [`Error::Lagged`],
	/// which a route copy recovers.
	#[test]
	fn a_group_missing_its_head_lags() {
		let producer = track_producer("test", Info::default());
		let mut headless = producer.create_group(group::Info { sequence: 4 }).unwrap();
		headless.start_at(2).unwrap();
		for _ in 0..3 {
			headless.write_frame(None, &b"x"[..]).unwrap();
		}
		headless.finish().unwrap();

		let noop = kio::Waiter::noop();
		let start = Anchor::Absolute(4);
		let mut sub = producer.subscribe(replay().with_ranges([Range { start, end: None }]));
		let Poll::Ready(Ok(Some(mut group))) = sub.poll_recv_group(&noop) else {
			panic!("no group");
		};
		assert_eq!(group.index(), 0);
		assert!(matches!(group.poll_read_frame(&noop), Poll::Ready(Err(Error::Lagged))));
	}

	/// The floor a reader waiting on each group starts at is the higher of its ranges' and
	/// its local one, so it never waits on a group the local floor withholds.
	#[test]
	fn floor_honors_with_range() {
		let producer = track_producer("test", Info::default());
		let _groups: Vec<_> = (0..6).map(|sequence| framed(&producer, sequence, 2)).collect();

		let reader = producer.subscribe(replay().with_range(2..));
		assert_eq!((reader.floor(), reader.next_in_range(0)), (Some(2), Some(2)));
		let reader = reader.with_range(4..);
		assert_eq!((reader.floor(), reader.next_in_range(0)), (Some(4), Some(4)));
		assert_eq!(reader.next_in_range(5), Some(5));

		// Below the ranges' floor, the ranges still decide.
		let reader = producer.subscribe(replay().with_range(3..)).with_range(1..);
		assert_eq!((reader.floor(), reader.next_in_range(0)), (Some(3), Some(3)));

		// A floor reaching back to the oldest group stays unknown.
		let reader = producer.subscribe(replay()).with_range(4..);
		assert_eq!(reader.floor(), None);
	}

	/// The largest reports only while it is within the bounds: a cap below it or a floor
	/// above it reports nothing, rather than a cached group standing in for it.
	#[test]
	fn largest_reports_only_within_the_bounds() {
		let producer = track_producer("test", Info::default());
		let noop = kio::Waiter::noop();
		let _groups: Vec<_> = (0..6).map(|sequence| framed(&producer, sequence, 2)).collect();

		let mut capped = producer.subscribe(replay());
		capped.set_range(..3);
		assert!(capped.poll_largest(&noop).is_pending(), "the largest is past the cap");

		// An inclusive cap reports the group it names.
		let mut ordered = producer.subscribe(replay()).ordered();
		ordered.set_range(..=5);
		assert_eq!(
			ordered.poll_largest(&noop),
			Poll::Ready(Some(Position { group: 5, frame: 1 }))
		);

		let mut floored = producer.subscribe(replay());
		floored.set_range(8..);
		assert!(
			floored.poll_largest(&noop).is_pending(),
			"the largest is below the floor"
		);
		let _late = framed(&producer, 8, 0);
		assert_eq!(floored.poll_largest(&noop), Poll::Ready(Some(Position::group(8))));
	}

	/// Moving the bounds wakes a parked reader, which reports the largest once it is within
	/// them; bounds that leave it where it was report nothing new.
	#[test]
	fn largest_follows_the_bounds_as_they_move() {
		let producer = track_producer("test", Info::default());
		let noop = kio::Waiter::noop();
		let _groups: Vec<_> = (0..6).map(|sequence| framed(&producer, sequence, 1)).collect();

		let mut reader = producer.subscribe(replay()).ordered();
		reader.set_range(..3);
		let (flag, waiter) = Flag::waiter();
		assert!(reader.poll_largest(&waiter).is_pending());
		reader.set_range(..);
		assert!(flag.take(), "lifting the cap woke the reader");
		assert_eq!(reader.poll_largest(&noop), Poll::Ready(Some(Position::group(5))));

		assert!(reader.poll_largest(&waiter).is_pending());
		reader.set_range(..2);
		assert!(flag.take(), "lowering the cap woke the reader");
		assert!(
			reader.poll_largest(&noop).is_pending(),
			"the largest is past the new cap"
		);

		reader.set_range(..);
		assert!(
			reader.poll_largest(&noop).is_pending(),
			"lifting it again leaves the last report standing"
		);
		reader.set_range(3..);
		assert!(
			reader.poll_largest(&noop).is_pending(),
			"a floor below the largest leaves it where it was"
		);
	}

	/// Once the stream ended, widening the bounds over a largest it never reported does not
	/// bring it back.
	#[test]
	fn largest_stays_ended_when_the_bounds_widen() {
		let producer = track_producer("test", Info::default());
		framed(&producer, 5, 1).finish().unwrap();
		producer.finish().unwrap();
		let noop = kio::Waiter::noop();

		let mut reader = producer.subscribe(replay());
		reader.set_range(..3);
		assert_eq!(reader.poll_largest(&noop), Poll::Ready(None));
		reader.set_range(..);
		assert_eq!(
			reader.poll_largest(&noop),
			Poll::Ready(None),
			"the stream already ended"
		);
	}

	/// A dropped poll leaves no registration behind for a bounds move to wake.
	#[test]
	fn largest_forgets_a_dropped_poll() {
		let producer = track_producer("test", Info::default());
		let mut reader = producer.subscribe(replay());
		let (flag, waiter) = Flag::waiter();
		assert!(reader.poll_largest(&waiter).is_pending());
		drop(waiter);
		reader.set_range(..);
		assert!(!flag.take(), "the dropped poll was not woken");
	}

	/// A poll that reported is not parked, so moving the bounds afterwards does not wake it.
	#[test]
	fn largest_wakes_only_a_parked_poll_on_a_bounds_move() {
		let producer = track_producer("test", Info::default());
		let _group = framed(&producer, 0, 1);
		let mut reader = producer.subscribe(replay());
		let (flag, waiter) = Flag::waiter();
		assert_eq!(reader.poll_largest(&waiter), Poll::Ready(Some(Position::group(0))));
		reader.set_range(..);
		assert!(!flag.take(), "the finished poll was not woken");

		assert!(reader.poll_largest(&waiter).is_pending());
		reader.set_range(..);
		assert!(flag.take(), "the parked poll was woken");
	}

	/// Bounds that move while the copy is idle keep the last report, so going live again
	/// does not repeat it.
	#[test]
	fn largest_keeps_its_last_report_across_an_idle_move() {
		let mut producer = track_producer("test", Info::default());
		let noop = kio::Waiter::noop();
		let _groups: Vec<_> = (0..3).map(|sequence| framed(&producer, sequence, 1)).collect();

		let mut reader = producer.subscribe(replay());
		assert_eq!(reader.poll_largest(&noop), Poll::Ready(Some(Position::group(2))));
		producer.set_idle();
		reader.set_range(..);
		assert!(reader.poll_largest(&noop).is_pending(), "the copy is idle");

		producer.set_live(Some(Position::group(2)), None);
		assert!(
			reader.poll_largest(&noop).is_pending(),
			"the same largest is not reported again"
		);
		let _next = framed(&producer, 3, 1);
		assert_eq!(reader.poll_largest(&noop), Poll::Ready(Some(Position::group(3))));
	}

	/// An idle copy that declared an end it has not reached yet still waits for the live
	/// feed, while one that finished reports its settled largest.
	#[test]
	fn largest_waits_for_an_idle_copy_with_a_future_end() {
		let mut producer = track_producer("test", Info::default());
		let noop = kio::Waiter::noop();
		let _groups: Vec<_> = (0..3).map(|sequence| framed(&producer, sequence, 1)).collect();
		producer.finish_at(Position::group(10)).unwrap();
		producer.set_idle();

		let mut reader = producer.subscribe(replay());
		assert!(reader.poll_largest(&noop).is_pending(), "the copy is idle");
		producer.set_live(Some(Position::group(5)), None);
		let _answer = framed(&producer, 5, 1);
		assert_eq!(reader.poll_largest(&noop), Poll::Ready(Some(Position::group(5))));

		let mut finished = track_producer("test", Info::default());
		framed(&finished, 0, 1).finish().unwrap();
		finished.set_idle();
		finished.finish().unwrap();
		let mut reader = finished.subscribe(replay());
		assert_eq!(reader.poll_largest(&noop), Poll::Ready(Some(Position::group(0))));
	}

	/// An idle copy's largest is what an ended feed left, so nothing reports until the
	/// route's answer shows, though the cache still holds groups.
	#[test]
	fn largest_waits_for_an_idle_copy_to_go_live() {
		let mut producer = track_producer("test", Info::default());
		let noop = kio::Waiter::noop();
		let _groups: Vec<_> = (0..3).map(|sequence| framed(&producer, sequence, 1)).collect();
		producer.set_idle();

		let mut reader = producer.subscribe(replay());
		let (flag, waiter) = Flag::waiter();
		assert!(reader.poll_largest(&waiter).is_pending(), "the copy is idle");

		producer.set_live(Some(Position::group(5)), None);
		producer.report_largest(Position::group(5));
		assert!(
			reader.poll_largest(&waiter).is_pending(),
			"the answer has not shown yet"
		);
		flag.take();

		let _answer = framed(&producer, 5, 1);
		assert!(flag.take(), "the answer showing woke the reader");
		assert_eq!(reader.poll_largest(&noop), Poll::Ready(Some(Position::group(5))));
	}

	/// A relay copy holding groups 0 to 3 from an ended feed, idle until its route answers.
	fn idle_copy() -> Producer {
		let mut producer = track_producer("test", Info::default());
		for sequence in 0..4 {
			framed(&producer, sequence, 1).finish().unwrap();
		}
		producer.set_idle();
		producer
	}

	/// Every group `reader` can take right now, in delivery order.
	fn ready_groups(reader: &mut Subscriber) -> Vec<u64> {
		let noop = kio::Waiter::noop();
		let mut sequences = Vec::new();
		while let Poll::Ready(Ok(Some(group))) = reader.poll_recv_group(&noop) {
			sequences.push(group.sequence);
		}
		sequences
	}

	/// A relative start taken while the copy is idle counts back from the live feed's
	/// latest group once the route answers, not from the newest one an ended feed left.
	#[test]
	fn a_relative_start_on_an_idle_copy_counts_from_the_live_feed() {
		let behind = Range {
			start: Anchor::Relative(1),
			end: None,
		};
		for (ranges, expected) in [(Range::live(), vec![4]), (behind, vec![3, 4])] {
			let mut producer = idle_copy();
			let mut reader = producer.subscribe(replay().with_ranges([ranges]));
			assert_eq!(ready_groups(&mut reader), Vec::<u64>::new(), "the copy is idle");
			assert_eq!(reader.floor(), None, "the start waits for the live feed");

			producer.set_live(Some(Position::group(4)), None);
			framed(&producer, 4, 1).finish().unwrap();
			assert_eq!(ready_groups(&mut reader), expected, "{ranges:?}");
		}
	}

	/// A relative start waits past groups the route resends below its answer: the feed
	/// goes live, and the start resolves, only once the answer shows.
	#[test]
	fn a_relative_start_on_an_idle_copy_waits_for_the_answer_to_show() {
		let mut producer = idle_copy();
		let mut reader = producer.subscribe(replay().with_ranges([Range::live()]));
		producer.set_live(Some(Position::group(5)), None);
		framed(&producer, 4, 1).finish().unwrap();
		assert_eq!(ready_groups(&mut reader), Vec::<u64>::new(), "the answer has not shown");

		framed(&producer, 5, 1).finish().unwrap();
		assert_eq!(ready_groups(&mut reader), vec![5]);
	}

	/// A route that answers with nothing leaves the cache unrelated to its feed, so a
	/// relative start resolves at the first group the feed then produces.
	#[test]
	fn a_relative_start_on_an_idle_copy_skips_a_cache_the_route_disowns() {
		let mut producer = idle_copy();
		let mut reader = producer.subscribe(replay().with_ranges([Range::live()]));
		producer.set_live(None, None);
		assert_eq!(ready_groups(&mut reader), Vec::<u64>::new());
		assert_eq!(reader.floor(), None, "no cached group anchors the start");

		framed(&producer, 7, 1).finish().unwrap();
		assert_eq!(ready_groups(&mut reader), vec![7]);
		assert_eq!(reader.floor(), Some(7));
	}

	/// An end the idle copy has not reached does not make its cache the whole track: until
	/// the route answers, it serves nothing and anchors no relative start.
	#[test]
	fn an_idle_copy_with_a_future_end_waits_for_the_live_feed() {
		let mut producer = idle_copy();
		producer.finish_at(Position::group(10)).unwrap();
		let mut live = producer.subscribe(replay().with_ranges([Range::live()]));
		let mut oldest = producer.subscribe(replay());
		assert_eq!(ready_groups(&mut live), Vec::<u64>::new(), "the copy is idle");
		assert_eq!(live.floor(), None, "the start waits for the live feed");
		assert_eq!(ready_groups(&mut oldest), Vec::<u64>::new(), "the copy is idle");

		producer.set_live(Some(Position::group(4)), None);
		framed(&producer, 4, 1).finish().unwrap();
		assert_eq!(ready_groups(&mut live), vec![4]);
		assert_eq!(ready_groups(&mut oldest), vec![0, 1, 2, 3, 4]);
	}

	/// An idle copy that ends without going live makes its cache the whole track, so a
	/// relative start that waited counts back from its newest group, not from where the
	/// earlier feed began.
	#[test]
	fn a_relative_start_on_an_idle_copy_that_ends_counts_from_its_newest_group() {
		let producer = idle_copy();
		let mut reader = producer.subscribe(replay().with_ranges([Range::live()]));
		assert_eq!(ready_groups(&mut reader), Vec::<u64>::new(), "the copy is idle");

		producer.finish().unwrap();
		assert_eq!(ready_groups(&mut reader), vec![3]);
	}

	/// A datagram can show the route's answer before any group does, as it does when a
	/// datagram is the first thing the resumed feed sends. A relative start counts back
	/// from that answer, not from the older groups cached below it.
	#[test]
	#[cfg(feature = "async")]
	fn a_relative_start_on_an_idle_copy_counts_from_a_datagram_edge() {
		let mut producer = idle_copy();
		let mut reader = producer.subscribe(replay().with_ranges([Range::live()]));
		producer
			.insert_datagram(4, None, bytes::Bytes::from_static(b"x"))
			.unwrap();
		producer.set_live(Some(Position::group(4)), None);
		assert_eq!(
			ready_groups(&mut reader),
			Vec::<u64>::new(),
			"no group reached the edge"
		);
		assert_eq!(reader.floor(), Some(4), "the answer anchors the start");

		framed(&producer, 5, 1).finish().unwrap();
		assert_eq!(ready_groups(&mut reader), vec![5]);
	}

	/// A live join's answer anchors a relative start where it says the feed is, both for the
	/// copy's readers and for the bounds sent upstream, even once a newer group lands before
	/// the one it named.
	#[test]
	fn a_relative_start_counts_from_the_answered_edge() {
		let producer = track_producer("test", Info::default());
		let back = Range {
			start: super::super::subscription::Anchor::Relative(2),
			end: None,
		};
		let mut reader = producer.subscribe(replay().with_ranges([back]));
		producer.report_edge(4);
		let upstream = producer.resolve_subscription(&replay().with_ranges([back])).unwrap();
		assert_eq!(upstream.start(), Some(2), "the answer resolves the upstream bounds");

		framed(&producer, 5, 1).finish().unwrap();
		assert_eq!(ready_groups(&mut reader), vec![5]);
		assert_eq!(reader.floor(), Some(2), "the answer anchors the start");
	}

	/// An idle copy forgets the edge its previous feed answered with, which only a datagram
	/// it dropped showed: when it ends without going live, a relative start counts back
	/// from its newest cached group.
	#[test]
	#[cfg(feature = "async")]
	fn an_idle_copy_that_ends_forgets_an_earlier_datagram_edge() {
		let mut producer = idle_copy();
		producer
			.insert_datagram(4, None, bytes::Bytes::from_static(b"x"))
			.unwrap();
		producer.set_live(Some(Position::group(4)), None);
		producer.set_idle();
		let mut reader = producer.subscribe(replay().with_ranges([Range::live()]));
		assert_eq!(ready_groups(&mut reader), Vec::<u64>::new(), "the copy is idle");
		producer.finish().unwrap();
		assert_eq!(ready_groups(&mut reader), vec![3]);
	}

	#[test]
	#[cfg(feature = "async")]
	fn append_datagram_roundtrip() {
		let mut producer = track_producer("test", Info::timed());
		let mut dg = producer.subscribe(None);

		let ts = Timestamp::from_millis(42).unwrap();
		let seq = producer.append_datagram(ts, &b"hello"[..]).unwrap();

		let got = recv_datagram(&mut dg);
		assert_eq!(got.sequence, seq);
		assert_eq!(got.timestamp, Some(ts));
		assert_eq!(&got.payload[..], b"hello");
	}

	#[test]
	#[cfg(feature = "async")]
	fn insert_datagram_preserves_sequence() {
		let mut producer = track_producer("test", Info::timed());
		let mut dg = producer.subscribe(None);

		let ts = Timestamp::from_millis(5).unwrap();
		// A relay forwarding an upstream datagram keeps its sequence number.
		producer
			.insert_datagram(100, ts, bytes::Bytes::from_static(b"x"))
			.unwrap();

		assert_eq!(recv_datagram(&mut dg).sequence, 100);
		// max_sequence advanced, so the next appended group/datagram continues past it.
		assert_eq!(producer.append_group().unwrap().sequence, 101);
	}

	#[test]
	#[cfg(feature = "async")]
	fn insert_datagram_leaves_a_gap() {
		let mut producer = track_producer("test", Info::timed());
		let mut dg = producer.subscribe(None);
		let ts = Timestamp::from_millis(0).unwrap();

		producer
			.insert_datagram(10, ts, bytes::Bytes::from_static(b"gap"))
			.unwrap();
		assert_eq!(recv_datagram(&mut dg).sequence, 10);
		assert_eq!(producer.append_datagram(ts, &b"next"[..]).unwrap(), 11);
		assert_eq!(producer.append_group().unwrap().sequence, 12);
	}

	#[test]
	#[cfg(feature = "async")]
	fn insert_datagram_out_of_order_does_not_rewind() {
		let mut producer = track_producer("test", Info::timed());
		let mut dg = producer.subscribe(None);
		let ts = Timestamp::from_millis(0).unwrap();

		producer
			.insert_datagram(10, ts, bytes::Bytes::from_static(b"high"))
			.unwrap();
		producer
			.insert_datagram(5, ts, bytes::Bytes::from_static(b"low"))
			.unwrap();

		assert_eq!(recv_datagram(&mut dg).sequence, 10);
		assert_eq!(recv_datagram(&mut dg).sequence, 5);
		assert_eq!(producer.append_datagram(ts, &b"next"[..]).unwrap(), 11);
	}

	#[test]
	#[cfg(feature = "async")]
	fn insert_datagram_duplicate_is_best_effort() {
		let mut producer = track_producer("test", Info::timed());
		let mut dg = producer.subscribe(None);
		let ts = Timestamp::from_millis(0).unwrap();

		producer
			.insert_datagram(3, ts, bytes::Bytes::from_static(b"first"))
			.unwrap();
		producer
			.insert_datagram(3, ts, bytes::Bytes::from_static(b"again"))
			.unwrap();

		assert_eq!(&recv_datagram(&mut dg).payload[..], b"first");
		assert_eq!(&recv_datagram(&mut dg).payload[..], b"again");
		assert_eq!(producer.append_datagram(ts, &b"next"[..]).unwrap(), 4);
	}

	#[test]
	#[cfg(feature = "async")]
	fn insert_datagram_stale_does_not_rewind_after_append() {
		let mut producer = track_producer("test", Info::timed());
		let mut dg = producer.subscribe(None);
		let ts = Timestamp::from_millis(0).unwrap();

		assert_eq!(producer.append_datagram(ts, &b"0"[..]).unwrap(), 0);
		assert_eq!(producer.append_datagram(ts, &b"1"[..]).unwrap(), 1);
		producer
			.insert_datagram(0, ts, bytes::Bytes::from_static(b"stale"))
			.unwrap();

		assert_eq!(recv_datagram(&mut dg).sequence, 0);
		assert_eq!(recv_datagram(&mut dg).sequence, 1);
		assert_eq!(recv_datagram(&mut dg).sequence, 0);
		assert_eq!(producer.append_datagram(ts, &b"2"[..]).unwrap(), 2);
		assert_eq!(producer.append_group().unwrap().sequence, 3);
	}

	#[test]
	#[cfg(feature = "async")]
	fn insert_datagram_cloned_producers_share_counter() {
		let mut producer = track_producer("test", Info::timed());
		let mut other = producer.clone();
		let mut dg = producer.subscribe(None);
		let ts = Timestamp::from_millis(0).unwrap();

		producer
			.insert_datagram(4, ts, bytes::Bytes::from_static(b"a"))
			.unwrap();
		assert_eq!(other.append_datagram(ts, &b"b"[..]).unwrap(), 5);
		other.insert_datagram(8, ts, bytes::Bytes::from_static(b"c")).unwrap();
		assert_eq!(producer.append_group().unwrap().sequence, 9);

		assert_eq!(recv_datagram(&mut dg).sequence, 4);
		assert_eq!(recv_datagram(&mut dg).sequence, 5);
		assert_eq!(recv_datagram(&mut dg).sequence, 8);
	}

	#[test]
	fn insert_datagram_after_finish_is_closed() {
		let mut producer = track_producer("test", Info::timed());
		let ts = Timestamp::from_millis(0).unwrap();
		producer.finish().unwrap();
		assert!(matches!(
			producer.insert_datagram(0, ts, bytes::Bytes::from_static(b"x")),
			Err(Error::Closed)
		));
		assert!(matches!(producer.append_datagram(ts, &b"x"[..]), Err(Error::Closed)));
	}

	#[test]
	fn insert_datagram_after_abort_fails() {
		let producer = track_producer("test", Info::timed());
		let mut other = producer.clone();
		let ts = Timestamp::from_millis(0).unwrap();
		producer.abort(Error::Cancel).unwrap();
		assert!(other.insert_datagram(0, ts, bytes::Bytes::from_static(b"x")).is_err());
	}

	#[test]
	fn insert_datagram_respects_finish_at() {
		let mut producer = track_producer("test", Info::timed());
		let ts = Timestamp::from_millis(0).unwrap();
		producer.finish_at(Position::group(10)).unwrap();
		producer
			.insert_datagram(5, ts, bytes::Bytes::from_static(b"ok"))
			.unwrap();
		assert!(matches!(
			producer.insert_datagram(10, ts, bytes::Bytes::from_static(b"late")),
			Err(Error::Closed)
		));
		assert_eq!(producer.append_group().unwrap().sequence, 6);
	}

	/// A cached observer must not keep an unfinished writer alive or serve its dead copy.
	#[moq_clock::test]
	async fn dropped_group_writer_aborts_and_releases_its_cached_copy() {
		let pool = cache::Pool::unbounded();
		let producer = track_producer_pooled("test", pool.clone());
		let mut group = producer.create_group(group::Info { sequence: 3 }).unwrap();
		group
			.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"head"))
			.unwrap();
		let mut reader = group.consume();
		assert!(pool.used() > 0);
		drop(group);

		assert!(matches!(
			reader.poll_read_frame(&kio::Waiter::noop()),
			Poll::Ready(Err(Error::Dropped))
		));
		let state = producer.state.read();
		assert!(state.lookup.get(&3).unwrap().is_aborted());
		assert_eq!(state.lookup.get(&3).unwrap().cache_size(), 0);
		drop(state);
		assert_eq!(pool.used(), 0);
	}

	#[moq_clock::test]
	async fn dropped_group_writer_is_absent_and_its_sequence_can_be_recreated() {
		let producer = track_producer("test", Info::timed());
		let mut group = producer.create_group(group::Info { sequence: 3 }).unwrap();
		group
			.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"head"))
			.unwrap();
		drop(group);

		// The writer gave the group up: a reader gets it, to read why it ended.
		let mut reader = producer.subscribe(Subscription::default().with_range(0..));
		let Poll::Ready(Ok(Some(mut cut))) = reader.poll_recv_group(&kio::Waiter::noop()) else {
			panic!("the cut group is handed out");
		};
		assert!(matches!(
			cut.poll_read_frame(&kio::Waiter::noop()),
			Poll::Ready(Err(Error::Dropped))
		));
		let mut replacement = producer
			.create_group(group::Info { sequence: 3 })
			.expect("the dead copy is replaceable");
		replacement
			.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"head"))
			.unwrap();
		replacement.finish().unwrap();
		let mut later = producer.subscribe(Subscription::default().with_range(0..));
		assert!(matches!(
			later.poll_recv_group(&kio::Waiter::noop()),
			Poll::Ready(Ok(Some(_)))
		));
	}

	#[moq_clock::test]
	async fn dropped_group_writer_preserves_the_committed_resume_position() {
		let producer = track_producer("test", Info::timed());
		let consumer = producer.consume();
		let mut group = producer.create_group(group::Info { sequence: 3 }).unwrap();
		group
			.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"head"))
			.unwrap();
		let handle = group.handle();
		let _partial = group
			.create_frame(frame::Info {
				size: 2,
				timestamp: Some(Timestamp::ZERO),
			})
			.unwrap();
		assert_eq!(handle.frame_count(), 2);
		drop(_partial);
		assert_eq!(handle.resume_frame(), Some(1));
		assert_eq!(consumer.resume_position(), Some(Position { group: 3, frame: 1 }));
	}

	/// Datagram sequence advances do not move a route takeover past an open group.
	#[test]
	fn resume_position_uses_the_latest_group() {
		let mut datagram_only = track_producer("datagram-only", Info::timed());
		let datagram_only_consumer = datagram_only.consume();
		datagram_only
			.insert_datagram(8, Timestamp::ZERO, bytes::Bytes::from_static(b"x"))
			.unwrap();
		assert_eq!(
			datagram_only_consumer.resume_position(),
			None,
			"a datagram creates no group position to resume"
		);

		let mut producer = track_producer("mixed", Info::timed());
		let consumer = producer.consume();
		let mut group = producer.create_group(group::Info { sequence: 3 }).unwrap();
		group
			.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"head"))
			.unwrap();
		producer
			.insert_datagram(8, Timestamp::ZERO, bytes::Bytes::from_static(b"x"))
			.unwrap();

		assert_eq!(
			consumer.resume_position(),
			Some(Position { group: 3, frame: 1 }),
			"the replacement must continue the open group"
		);
		group.abort(Error::Cancel).unwrap();
	}

	/// Datagrams and groups are separate channels that share only a sequence namespace,
	/// so consuming one must not move the other's cursor.
	#[test]
	#[cfg(feature = "async")]
	fn recv_datagram_leaves_the_ordered_cursor_alone() {
		let mut producer = track_producer("test", Info::timed());
		let mut datagrams = producer.subscribe(replay());
		let mut subscriber = producer.subscribe(replay()).ordered();
		let ts = Timestamp::from_millis(5).unwrap();

		producer
			.insert_datagram(5, ts, bytes::Bytes::from_static(b"x"))
			.unwrap();
		assert_eq!(recv_datagram(&mut datagrams).sequence, 5);

		producer
			.create_group(group::Info { sequence: 3 })
			.unwrap()
			.finish()
			.unwrap();
		producer
			.create_group(group::Info { sequence: 6 })
			.unwrap()
			.finish()
			.unwrap();

		let mut next = || {
			subscriber
				.next_group()
				.now_or_never()
				.expect("group would have blocked")
				.expect("would have errored")
				.expect("track was closed")
				.sequence
		};
		assert_eq!(next(), 3, "the datagram at sequence 5 did not consume group 3");
		assert_eq!(next(), 6);
	}

	#[test]
	#[cfg(feature = "async")]
	fn datagram_normalized_to_track_timescale() {
		let info = Info::default().with_timescale(Timescale::MICRO);
		let mut producer = track_producer("test", info);
		let mut dg = producer.subscribe(None);

		// Supplied at millis; stored/emitted at the track's micro timescale.
		producer
			.append_datagram(Timestamp::from_millis(2).unwrap(), &b"z"[..])
			.unwrap();
		let got = recv_datagram(&mut dg);
		assert_eq!(got.timestamp.unwrap().scale(), Timescale::MICRO);
		assert_eq!(got.timestamp.unwrap().value(), 2_000);
	}

	#[test]
	fn datagram_rejects_oversized() {
		let mut producer = track_producer("test", Info::timed());
		let big = bytes::Bytes::from(vec![0u8; crate::model::datagram::MAX_DATAGRAM_PAYLOAD + 1]);
		let ts = Timestamp::from_millis(0).unwrap();
		assert!(matches!(
			producer.append_datagram(ts, big.clone()),
			Err(Error::FrameTooLarge)
		));
		assert!(matches!(
			producer.insert_datagram(0, ts, big),
			Err(Error::FrameTooLarge)
		));
	}

	#[test]
	#[cfg(feature = "async")]
	fn datagram_fanout_to_subscribers() {
		let mut producer = track_producer("test", Info::timed());
		// Two independent subscribers, each with its own datagram cursor.
		let mut a = producer.subscribe(None);
		let mut b = producer.subscribe(None);
		let ts = Timestamp::from_millis(1).unwrap();

		producer.append_datagram(ts, &b"first"[..]).unwrap();
		producer.append_datagram(ts, &b"second"[..]).unwrap();

		// Both receive every datagram in order, independently.
		assert_eq!(&recv_datagram(&mut a).payload[..], b"first");
		assert_eq!(&recv_datagram(&mut a).payload[..], b"second");
		assert_eq!(&recv_datagram(&mut b).payload[..], b"first");
		assert_eq!(&recv_datagram(&mut b).payload[..], b"second");
	}

	#[test]
	#[cfg(feature = "async")]
	fn datagram_replay_starts_within_max_delay() {
		let info = Info::default().with_timescale(Timescale::MICRO);
		let mut producer = track_producer("sparse", info);
		let timestamps = [0, 179_949, 179_950, 180_000, 179_975];
		for millis in timestamps {
			producer
				.append_datagram(Timestamp::from_millis(millis).unwrap(), b"x".as_slice())
				.unwrap();
			elapse(&producer, Duration::from_secs(1));
		}
		let mut subscriber = producer.subscribe(Subscription::default().with_max_delay(Duration::from_millis(50)));
		// The edge is captured at attachment, not when the subscriber first reads.
		producer
			.append_datagram(Timestamp::from_secs(600).unwrap(), b"newer".as_slice())
			.unwrap();
		for sequence in [2, 3, 4, 5] {
			assert_eq!(recv_datagram(&mut subscriber).sequence, sequence);
		}
		assert!(subscriber.poll_recv_datagram(&kio::Waiter::noop()).is_pending());

		// The initial bound does not filter arrivals after attachment, even when they are old.
		producer
			.append_datagram(Timestamp::from_millis(0).unwrap(), b"later".as_slice())
			.unwrap();
		assert_eq!(recv_datagram(&mut subscriber).sequence, 6);
	}

	#[test]
	#[cfg(feature = "async")]
	fn datagram_replay_cutoff_between_track_units() {
		// A 90 kHz unit doesn't divide the nanosecond cutoff, which lands inside unit 89_995.
		let info = Info::default().with_timescale(Timescale::new(90_000).unwrap());
		let mut producer = track_producer("video", info);
		for units in [89_995, 89_996, 90_000] {
			producer
				.append_datagram(Timestamp::from_scale(units, 90_000).unwrap(), b"x".as_slice())
				.unwrap();
		}
		let mut subscriber = producer.subscribe(Subscription::default().with_max_delay(Duration::from_micros(50)));
		for sequence in [1, 2] {
			assert_eq!(recv_datagram(&mut subscriber).sequence, sequence);
		}
		assert!(subscriber.poll_recv_datagram(&kio::Waiter::noop()).is_pending());
	}

	#[test]
	#[cfg(feature = "async")]
	fn datagram_replay_starts_after_evicted_entries_with_zero_delay() {
		let mut producer = track_producer("test", Info::timed());
		for _ in 0..MAX_DATAGRAMS + 1 {
			producer.append_datagram(Timestamp::ZERO, b"old".as_slice()).unwrap();
		}
		let sequence = producer
			.append_datagram(Timestamp::from_secs(180).unwrap(), b"new".as_slice())
			.unwrap();
		let mut subscriber = producer.subscribe(Subscription::default().with_max_delay(Duration::ZERO));
		assert_eq!(recv_datagram(&mut subscriber).sequence, sequence);
		assert!(subscriber.poll_recv_datagram(&kio::Waiter::noop()).is_pending());
	}

	#[test]
	#[cfg(feature = "async")]
	fn untimed_datagram_replay_keeps_the_buffer() {
		let mut producer = untimed_producer();
		// Far past the delay, but within the pool's idle window.
		for _ in 0..3 {
			producer.append_datagram(None, b"x".as_slice()).unwrap();
			elapse(&producer, Duration::from_secs(5));
		}
		let mut subscriber = producer.subscribe(Subscription::default().with_max_delay(Duration::ZERO));
		for sequence in 0..3 {
			assert_eq!(recv_datagram(&mut subscriber).sequence, sequence);
		}
		assert!(subscriber.poll_recv_datagram(&kio::Waiter::noop()).is_pending());
	}

	#[test]
	#[cfg(feature = "async")]
	fn datagram_buffer_drops_oldest_at_capacity() {
		let mut producer = track_producer("test", Info::timed());
		let mut slow = producer.subscribe(None);
		let mut fast = producer.subscribe(None);
		let count = MAX_DATAGRAMS * 3;
		for sequence in 0..count {
			producer.append_datagram(Timestamp::ZERO, b"x".as_slice()).unwrap();
			assert_eq!(recv_datagram(&mut fast).sequence, sequence as u64);
		}
		assert_eq!(producer.state.read().datagrams.len(), MAX_DATAGRAMS);
		for sequence in count - MAX_DATAGRAMS..count {
			assert_eq!(recv_datagram(&mut slow).sequence, sequence as u64);
		}
		assert!(slow.poll_recv_datagram(&kio::Waiter::noop()).is_pending());
	}

	#[test]
	#[cfg(feature = "async")]
	fn datagram_recv_pends_until_written() {
		let mut producer = track_producer("test", Info::timed());
		let mut dg = producer.subscribe(None);

		assert!(
			dg.recv_datagram().now_or_never().is_none(),
			"should block with no datagrams"
		);

		producer
			.append_datagram(Timestamp::from_millis(0).unwrap(), &b"go"[..])
			.unwrap();
		assert_eq!(&recv_datagram(&mut dg).payload[..], b"go");
	}

	/// A fetch never serves a datagram group, and says so rather than reporting a plain
	/// miss, whether or not a group followed it.
	#[test]
	fn a_datagram_group_is_not_fetchable() {
		let mut producer = track_producer("test", Info::timed());
		let consumer = producer.consume();
		let _subscriber = producer.subscribe(None);
		let sequence = producer.append_datagram(Timestamp::ZERO, &b"x"[..]).unwrap();
		let fetch = |sequence| consumer.fetch_group(sequence, None).now_or_never();
		assert!(
			matches!(fetch(sequence), Some(Err(Error::NotFetchable))),
			"the newest sequence"
		);

		let _open = producer.append_group().unwrap();
		assert!(
			matches!(fetch(sequence), Some(Err(Error::NotFetchable))),
			"below a newer group"
		);
		// A sequence that was never a datagram is a plain miss.
		assert!(
			matches!(fetch(sequence + 2), Some(Err(Error::NotFound))),
			"a missing group"
		);
	}

	/// A fill's end is exclusive, so the last sequence can't be asked of a handler: its fetch
	/// fails at once rather than parking until every handler goes.
	#[test]
	#[cfg(feature = "async")]
	fn the_last_sequence_misses_with_a_handler() {
		let producer = track_producer("test", None);
		let dynamic = producer.requests();
		let consumer = producer.consume();
		let fetch = consumer.fetch_group(u64::MAX, None).now_or_never();
		assert!(matches!(fetch, Some(Err(Error::NotFound))));
		assert!(dynamic.requested_fill().now_or_never().is_none(), "nothing to fill");
	}

	/// The subscription's floor and cap bound datagrams as they bound groups. The datagrams
	/// in range, sent between the dropped ones, show the reader is live, so the range is
	/// what dropped the others.
	#[test]
	#[cfg(feature = "async")]
	fn the_group_range_bounds_datagrams() {
		let mut producer = track_producer("test", Info::timed());
		let mut subscriber = producer.subscribe(Subscription::default().with_start(5));
		subscriber.set_range(..7);
		for sequence in [4, 5, 7, 6] {
			producer
				.insert_datagram(sequence, Timestamp::ZERO, bytes::Bytes::from_static(b"x"))
				.unwrap();
		}

		assert_eq!(recv_datagram(&mut subscriber).sequence, 5);
		assert_eq!(recv_datagram(&mut subscriber).sequence, 6);
		assert!(subscriber.poll_recv_datagram(&kio::Waiter::noop()).is_pending());
	}

	/// A range update judges the datagrams still buffered when they are read, and one it
	/// dropped stays dropped once the cap rises again: nothing holds it like a parked group.
	#[test]
	#[cfg(feature = "async")]
	fn a_range_update_applies_to_buffered_datagrams() {
		let mut producer = track_producer("test", Info::timed());
		let mut subscriber = producer.subscribe(None);
		for sequence in 1..=4 {
			producer
				.insert_datagram(sequence, Timestamp::ZERO, bytes::Bytes::from_static(b"x"))
				.unwrap();
		}

		subscriber.set_range(2..4);
		assert_eq!(recv_datagram(&mut subscriber).sequence, 2);
		assert_eq!(recv_datagram(&mut subscriber).sequence, 3);
		assert!(subscriber.poll_recv_datagram(&kio::Waiter::noop()).is_pending());

		subscriber.set_range(..);
		assert!(subscriber.poll_recv_datagram(&kio::Waiter::noop()).is_pending());
		producer
			.insert_datagram(5, Timestamp::ZERO, bytes::Bytes::from_static(b"x"))
			.unwrap();
		assert_eq!(recv_datagram(&mut subscriber).sequence, 5);
	}

	/// Plain readers cannot identify another object in the same datagram group. Skipping it
	/// must neither consume a late object 0 nor alter the typed reader's arrival stream.
	#[test]
	fn plain_datagrams_never_substitute_another_object() {
		for objects in [&[0, 1, 2][..], &[1, 2, 0], &[1, 2]] {
			let mut producer = track_producer("test", Info::default());
			let mut plain = producer.subscribe(None);
			let mut typed = producer.subscribe(None);
			let waiter = kio::Waiter::noop();
			for &object in objects {
				let stored = super::super::datagram::Stored {
					datagram: Datagram {
						sequence: 5,
						timestamp: None,
						payload: bytes::Bytes::from(vec![object as u8]),
					},
					wire: Some(Arc::new(super::super::datagram::Wire {
						object_id: Some(object),
						priority: Some(200),
						end_of_group: object == 2,
						status: None,
						properties: Default::default(),
					})),
					deflated: None,
				};
				producer.insert_wire_datagram(stored.clone()).unwrap();
				let Poll::Ready(Ok(Some(kept))) = typed.poll_recv_stored_datagram(&waiter) else {
					panic!("typed reader lost object {object}");
				};
				assert_eq!(kept, stored);
				match object {
					0 => {
						let Poll::Ready(Ok(Some(datagram))) = plain.poll_recv_datagram(&waiter) else {
							panic!("object 0 was lost in {objects:?}");
						};
						assert_eq!((datagram.sequence, &datagram.payload[..]), (5, &[0][..]));
					}
					_ => assert!(
						plain.poll_recv_datagram(&waiter).is_pending(),
						"object {object} substituted"
					),
				}
			}
			producer.insert_datagram(6, None, b"next".as_slice()).unwrap();
			producer.finish().unwrap();
			let Poll::Ready(Ok(Some(datagram))) = plain.poll_recv_datagram(&waiter) else {
				panic!("the next datagram was lost in {objects:?}");
			};
			assert_eq!((datagram.sequence, &datagram.payload[..]), (6, &b"next"[..]));
			assert!(matches!(plain.poll_recv_datagram(&waiter), Poll::Ready(Ok(None))));
		}
	}

	/// Exercises the full producer -> publisher-encode -> subscriber-decode -> producer seam
	/// (everything but the QUIC datagram send/recv), catching any field-order mismatch between
	/// the wire codec and the model.
	#[test]
	#[cfg(feature = "async")]
	fn datagram_wire_roundtrip_between_tracks() {
		use crate::coding::Encode;
		use crate::lite;

		let version = lite::Version::Lite05;

		// Origin publishes a datagram; the publisher reads it and encodes the wire body.
		let mut origin = track_producer("test", Info::timed());
		let mut origin_dg = origin.subscribe(None);
		let ts = Timestamp::from_millis(7).unwrap();
		let seq = origin.append_datagram(ts, &b"payload"[..]).unwrap();

		let d = recv_datagram(&mut origin_dg);
		let body = lite::Datagram {
			subscribe: 5,
			sequence: d.sequence,
			timestamp: Some(d.timestamp.unwrap().value()),
			payload: d.payload.clone(),
			inflated: None,
			ietf: None,
		}
		.encode_bytes(version)
		.unwrap();

		// Subscriber decodes the body and writes it downstream, preserving the sequence.
		let wire = lite::Datagram::decode(body, version, false, |_| lite::Framing {
			timescale: Some(crate::Timescale::MICRO),
			ietf: None,
			compressed: false,
		})
		.unwrap();
		let mut downstream = track_producer("test", Info::timed());
		let mut downstream_dg = downstream.subscribe(None);
		downstream
			.insert_datagram(
				wire.sequence,
				Timestamp::new(wire.timestamp.unwrap(), Timescale::MILLI).unwrap(),
				wire.payload,
			)
			.unwrap();

		let got = recv_datagram(&mut downstream_dg);
		assert_eq!(got.sequence, seq);
		assert_eq!(got.timestamp, Some(ts));
		assert_eq!(&got.payload[..], b"payload");
	}

	#[test]
	fn evict_expired_groups() {
		let producer = track_producer("test", Info::timed());

		// Create 3 groups at time 0.
		producer.append_group().unwrap().finish().unwrap(); // seq 0
		producer.append_group().unwrap().finish().unwrap(); // seq 1
		producer.append_group().unwrap().finish().unwrap(); // seq 2

		{
			let state = producer.state.read();
			assert_eq!(live_groups(&state), 3);
			assert_eq!(state.offset, 0);
		}

		// Advance time past the pool's LRU window.
		elapse(&producer, cache::DEFAULT_EXPIRY + Duration::from_secs(1));

		// Append a new group to trigger eviction.
		producer.append_group().unwrap().finish().unwrap(); // seq 3

		// Groups 0, 1, 2 are expired but seq 3 (the live edge) is kept. Their arrival
		// entries no longer resolve, so the leading ones are trimmed and the offset
		// advances past them.
		{
			let state = producer.state.read();
			assert_eq!(live_groups(&state), 1);
			assert_eq!(first_live_sequence(&state), 3);
			assert_eq!(state.offset, 3);
			assert!(!state.lookup.contains_key(&0));
			assert!(!state.lookup.contains_key(&1));
			assert!(!state.lookup.contains_key(&2));
			assert!(state.lookup.contains_key(&3));
		}
	}

	/// A group whose frames outlive `max_age` is aged out when the next group starts, but
	/// a subscriber that already drained it must still see the clean end of group. Otherwise a
	/// track with long groups (a per-minute rollup, say) fails its readers at every boundary.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn aging_out_a_finished_group_keeps_the_clean_end() {
		let producer = track_producer("test", Info::timed());
		let mut group = producer.create_group(group::Info { sequence: 0 }).unwrap();
		let mut consumer = group.consume();

		group
			.write_frame(Timestamp::from_millis(0).unwrap(), b"hello".as_slice())
			.unwrap();
		assert_eq!(consumer.next_frame().await.unwrap().unwrap().size, 5);

		// The group stays open well past the LRU window, then the next period starts.
		elapse(&producer, cache::DEFAULT_EXPIRY * 2);
		group.finish().unwrap();
		let _next = producer.create_group(group::Info { sequence: 1 }).unwrap();

		assert!(consumer.next_frame().await.unwrap().is_none());
	}

	/// An actively-read group is not expired out from under its reader: every frame
	/// read restarts the retention clock. A group nobody reads still ages out on
	/// schedule, so reclamation stays intact.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn active_reader_survives_expiry() {
		let producer = track_producer("test", Info::timed());
		let mut subscriber = producer.subscribe(None);

		// A finished group with one frame per step of the read loop below.
		let mut group = producer.create_group(0u64.into()).unwrap();
		for _ in 0..10 {
			group.write_frame(Timestamp::ZERO, b"x".as_slice()).unwrap();
		}
		group.finish().unwrap();
		let mut reading = subscriber.assert_group();

		// A sibling written at the same time that nobody ever reads.
		producer.create_group(1u64.into()).unwrap().finish().unwrap();

		// Each step stays well inside the retention window, but the whole read
		// spans several windows. New groups keep the expiry scan running.
		for seq in 2..12u64 {
			elapse(&producer, cache::DEFAULT_EXPIRY / 2);
			let frame = reading.next_frame().await;
			assert!(
				matches!(frame, Ok(Some(_))),
				"an actively-read group must not expire mid-read (step {seq})"
			);
			producer.create_group(seq.into()).unwrap().finish().unwrap();
		}

		let state = producer.state.read();
		assert!(state.lookup.contains_key(&0), "the read group survived");
		assert!(!state.lookup.contains_key(&1), "the unread group still expired");
	}

	/// Whole-frame reads served from the prefetch batch must also keep the group
	/// alive: the batch is filled (and stamped) once per `Prefetch::CAP` frames,
	/// which bounds frames, not elapsed time, so a slow `read_frame` reader has to
	/// re-stamp on a time bound between refills.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn slow_prefetch_reader_survives_expiry() {
		let producer = track_producer("test", Info::timed());
		let mut subscriber = producer.subscribe(None);

		let mut group = producer.create_group(0u64.into()).unwrap();
		for _ in 0..20 {
			group.write_frame(Timestamp::ZERO, b"x".as_slice()).unwrap();
		}
		group.finish().unwrap();
		let mut reading = subscriber.assert_group();

		// One whole-frame read per half-window: most are served straight from the
		// prefetch without locking. New groups keep the expiry scan running.
		for seq in 1..20u64 {
			elapse(&producer, cache::DEFAULT_EXPIRY / 2);
			let frame = reading.read_frame().await;
			assert!(
				matches!(frame, Ok(Some(_))),
				"a slow prefetch reader must not expire mid-read (step {seq})"
			);
			producer.create_group(seq.into()).unwrap().finish().unwrap();
		}
	}

	/// Receiving a group is itself a cache access: a subscriber that takes
	/// delivery just before the group would age out still gets to read it a full
	/// window later.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn delivery_restarts_the_expiry_clock() {
		let producer = track_producer("test", Info::timed());
		let mut subscriber = producer.subscribe(replay());

		let mut group = producer.create_group(0u64.into()).unwrap();
		group.write_frame(Timestamp::ZERO, b"x".as_slice()).unwrap();
		group.finish().unwrap();
		// A second group so seq 0 leaves the protected live edge.
		producer.create_group(1u64.into()).unwrap().finish().unwrap();

		// Deliver just inside the window: the delivery stamps the group.
		elapse(&producer, cache::DEFAULT_EXPIRY - Duration::from_secs(1));
		let mut reading = subscriber.assert_group();

		// Almost another full window passes: far beyond the write, inside the
		// delivery stamp. The new group runs the expiry scan.
		elapse(&producer, cache::DEFAULT_EXPIRY - Duration::from_secs(1));
		producer.create_group(2u64.into()).unwrap().finish().unwrap();

		let frame = reading.read_frame().await.unwrap();
		assert!(frame.is_some(), "a just-delivered group must not expire unread");
	}

	/// Streaming chunks into an in-flight frame is a write access: a straggler
	/// group (behind the live edge) trickling a large frame across several
	/// retention windows must not be expired mid-write.
	#[test]
	fn streaming_frame_writes_keep_the_group_alive() {
		let producer = track_producer("test", Info::timed());
		let straggler = producer.create_group(0u64.into()).unwrap();
		// The live edge moves on, so the straggler is demoted and expirable.
		producer.create_group(1u64.into()).unwrap().finish().unwrap();

		let mut frame = straggler
			.create_frame(frame::Info {
				size: 10,
				timestamp: Some(Timestamp::ZERO),
			})
			.unwrap();
		// One chunk per half-window; the whole frame spans several windows. New
		// groups keep the expiry scan running.
		for seq in 2..12u64 {
			elapse(&producer, cache::DEFAULT_EXPIRY / 2);
			frame.write(bytes::Bytes::from_static(b"x")).unwrap();
			producer.create_group(seq.into()).unwrap().finish().unwrap();
		}
		let mut straggler = frame.finish().unwrap();
		straggler.finish().unwrap();

		let state = producer.state.read();
		assert!(
			state.lookup.contains_key(&0),
			"a group streaming a frame survives expiry"
		);
	}

	/// The wire ingest coalesces its chunk wakes to the poll boundary, so a payload
	/// whose tail arrives all at once completes without a single `frame_notify`.
	/// Committing is itself a write access: a group that just finished a frame must
	/// not be expired by the next track write on its stale frame-open stamp.
	#[test]
	fn coalesced_frame_completion_keeps_the_group_alive() {
		let producer = track_producer("test", Info::timed());
		let straggler = producer.create_group(0u64.into()).unwrap();
		// The live edge moves on, so the straggler is demoted and expirable.
		producer.create_group(1u64.into()).unwrap().finish().unwrap();

		let mut frame = straggler
			.create_frame(frame::Info {
				size: 3,
				timestamp: Some(Timestamp::ZERO),
			})
			.unwrap();

		// The sender stalls past the retention window, then the whole payload lands in
		// one poll turn: the loop never returns `Pending`, so `notify` is never reached
		// and `finish` is the only write the charge sees.
		elapse(&producer, cache::DEFAULT_EXPIRY + Duration::from_secs(1));
		frame.write_deferred(bytes::Bytes::from_static(b"abc")).unwrap();
		let mut straggler = frame.finish().unwrap();
		straggler.finish().unwrap();

		// A new group runs the expiry scan.
		producer.create_group(2u64.into()).unwrap().finish().unwrap();

		let state = producer.state.read();
		assert!(
			state.lookup.contains_key(&0),
			"a group whose frame just completed must not expire"
		);
	}

	/// Re-offering a parked group (once the cap rises) is a delivery: it restarts
	/// the expiry clock so the subscriber gets to read what it was just handed.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn parked_reoffer_restarts_the_expiry_clock() {
		let producer = track_producer("test", Info::timed());
		let mut subscriber = producer.subscribe(None);
		subscriber.set_range(..1);

		for seq in 0..2u64 {
			let mut group = producer.create_group(seq.into()).unwrap();
			group.write_frame(Timestamp::ZERO, b"x".as_slice()).unwrap();
			group.finish().unwrap();
		}

		// Group 0 is in range; group 1 is beyond the cap and parks.
		assert_eq!(subscriber.assert_group().sequence, 0);
		subscriber.assert_no_group();

		// Just inside the window, the cap rises and the re-offer stamps group 1.
		elapse(&producer, cache::DEFAULT_EXPIRY - Duration::from_secs(1));
		subscriber.set_range(..2);
		let mut reading = subscriber.assert_group();
		assert_eq!(reading.sequence, 1);

		// Almost another full window passes: far beyond the write, inside the
		// re-offer stamp. The new group runs the expiry scan.
		elapse(&producer, cache::DEFAULT_EXPIRY - Duration::from_secs(1));
		producer.create_group(2u64.into()).unwrap().finish().unwrap();

		let frame = reading.read_frame().await.unwrap();
		assert!(frame.is_some(), "a just-re-offered group must not expire unread");
	}

	#[test]
	fn evict_keeps_max_sequence() {
		let producer = track_producer("test", Info::timed());
		let _open = producer.append_group().unwrap(); // seq 0

		// Advance time past the LRU window.
		elapse(&producer, cache::DEFAULT_EXPIRY + Duration::from_secs(1));

		// Append another group; seq 0 is expired and evicted.
		let _open = producer.append_group().unwrap(); // seq 1

		{
			let state = producer.state.read();
			assert_eq!(live_groups(&state), 1);
			assert_eq!(first_live_sequence(&state), 1);
			assert_eq!(state.offset, 1);
		}
	}

	#[test]
	fn no_eviction_when_fresh() {
		let producer = track_producer("test", Info::timed());
		producer.append_group().unwrap().finish().unwrap(); // seq 0
		producer.append_group().unwrap().finish().unwrap(); // seq 1
		producer.append_group().unwrap().finish().unwrap(); // seq 2

		{
			let state = producer.state.read();
			assert_eq!(live_groups(&state), 3);
			assert_eq!(state.offset, 0);
		}
	}

	#[test]
	fn consumer_skips_evicted_groups() {
		let producer = track_producer("test", Info::timed());
		producer.append_group().unwrap().finish().unwrap(); // seq 0

		let mut consumer = producer.subscribe(None);

		elapse(&producer, cache::DEFAULT_EXPIRY + Duration::from_secs(1));
		producer.append_group().unwrap().finish().unwrap(); // seq 1

		// Group 0 was evicted. Consumer should get group 1.
		let group = consumer.assert_group();
		assert_eq!(group.sequence, 1);
	}

	/// Mint a track under an origin whose pool has the given wall-clock LRU window.
	fn track_producer_expiring(name: impl Into<Arc<str>>, expiry: impl Into<Option<Duration>>) -> Producer {
		track_producer_pooled(name, cache::Pool::new(cache::Config::default().with_expiry(expiry)))
	}

	/// Mint a track under an origin caching into `pool`.
	fn track_producer_pooled(name: impl Into<Arc<str>>, pool: cache::Pool) -> Producer {
		Producer::new(
			Arc::new(broadcast::Info {
				pool,
				..Default::default()
			}),
			name,
			Info::timed(),
		)
	}

	/// The write path is not the only thing that runs expiry: a pool sweep reclaims a
	/// track's idle groups even when the track never writes again, which is the only
	/// bound on a publisher that stalls with a group still open.
	#[test]
	fn pool_sweep_expires_without_a_write() {
		let pool = cache::Pool::new(cache::Config::default().with_expiry(Duration::from_secs(1)));
		let producer = track_producer_pooled("test", pool.clone());
		let mut stalled = producer.append_group().unwrap(); // seq 0, left open
		stalled.write_frame(Timestamp::ZERO, b"x".as_slice()).unwrap();
		let _open = producer.append_group().unwrap(); // seq 1, the live edge

		elapse(&producer, Duration::from_secs(2));
		pool.sweep();

		assert!(
			!producer.state.read().lookup.contains_key(&0),
			"the sweep reclaimed an idle open group with no write behind it"
		);
	}

	#[test]
	fn cache_gc_dates_activity_before_expiring_it() {
		let pool = cache::Pool::new(cache::Config::default().with_expiry(Duration::from_secs(1)));
		let now = crate::model::clock::now();
		assert_eq!(
			pool.poll(now, &kio::Waiter::noop()),
			Some(now + Duration::from_millis(500))
		);
		let producer = track_producer_pooled("test", pool.clone());
		let mut group = producer.append_group().unwrap();
		group.write_frame(Timestamp::ZERO, b"first".as_slice()).unwrap();
		let _open = producer.append_group().unwrap();
		// Activity written while cleanup was idle gets the new supplied time.
		let later = now + Duration::from_secs(60);
		pool.poll(later, &kio::Waiter::noop());
		assert!(producer.state.read().lookup.contains_key(&0));
		pool.poll(later + Duration::from_secs(2), &kio::Waiter::noop());
		assert!(!producer.state.read().lookup.contains_key(&0));
	}

	#[test]
	fn cache_gc_reaches_old_entries_behind_a_fresh_front() {
		let expiry = Duration::from_secs(1);
		let pool = cache::Pool::new(cache::Config::default().with_expiry(expiry));
		let producer = track_producer_pooled("test", pool.clone());
		let now = crate::model::clock::now();
		let groups: Vec<_> = (0..EVICT_SCAN * 3).map(|_| producer.append_group().unwrap()).collect();
		let _open = producer.append_group().unwrap();
		pool.poll(now, &kio::Waiter::noop());
		// Keep more than a write scan's worth of leading entries fresh.
		for group in &groups[..EVICT_SCAN * 2] {
			group.handle().cache_refresh();
		}
		pool.poll(now + expiry * 2, &kio::Waiter::noop());
		let state = producer.state.read();
		for sequence in 0..EVICT_SCAN * 2 {
			assert!(state.lookup.contains_key(&(sequence as u64)), "fresh front survives");
		}
		for sequence in EVICT_SCAN * 2..EVICT_SCAN * 3 {
			assert!(!state.lookup.contains_key(&(sequence as u64)), "old tail is reclaimed");
		}
	}

	/// One sweep drains a whole idle backlog, not a rotating window of it: a quiet
	/// track has no writes left to revisit the rest of the queue with, so a bounded
	/// pass would leave the oldest groups parked for a backlog's length in windows.
	#[test]
	fn pool_sweep_drains_a_deep_backlog() {
		let pool = cache::Pool::new(cache::Config::default().with_expiry(Duration::from_secs(1)));
		let producer = track_producer_pooled("test", pool.clone());

		// Comfortably more than one write-driven scan window (EVICT_SCAN).
		let backlog = 4 * EVICT_SCAN;
		let mut _open = Vec::new();
		for _ in 0..backlog {
			let mut group = producer.append_group().unwrap();
			group.write_frame(Timestamp::ZERO, b"x".as_slice()).unwrap();
			_open.push(group);
		}
		let _open = producer.append_group().unwrap(); // the live edge, always protected

		elapse(&producer, Duration::from_secs(2));
		pool.sweep();

		let state = producer.state.read();
		let stale = (0..backlog as u64).filter(|seq| state.lookup.contains_key(seq)).count();
		assert_eq!(stale, 0, "one sweep reclaimed the whole idle backlog");
	}

	#[test]
	fn pool_expiry_controls_eviction() {
		// A shorter LRU window on the pool evicts sooner than the default.
		let producer = track_producer_expiring("test", Duration::from_secs(1));
		let _open = producer.append_group().unwrap(); // seq 0

		// Past the pool's window but well within cache::DEFAULT_EXPIRY.
		elapse(&producer, Duration::from_secs(2));
		let _open = producer.append_group().unwrap(); // seq 1

		// Seq 0 is gone because the pool only keeps idle groups for 1s.
		let state = producer.state.read();
		assert_eq!(live_groups(&state), 1);
		assert_eq!(first_live_sequence(&state), 1);
	}

	#[test]
	fn small_frame_write_expires_idle_siblings() {
		let producer = track_producer_expiring("test", Duration::from_secs(1));
		producer.append_group().unwrap().finish().unwrap(); // seq 0
		let mut live = producer.append_group().unwrap(); // seq 1

		elapse(&producer, Duration::from_secs(2));
		live.write_frame(Timestamp::ZERO, b"x".as_slice()).unwrap();

		let expired = !producer.state.read().lookup.contains_key(&0);
		assert!(expired, "a small frame write runs expiry");
	}

	#[test]
	fn fresh_expiry_scan_does_not_wake_track_consumers() {
		use std::sync::atomic::{AtomicBool, Ordering};

		let producer = track_producer_expiring("test", cache::DEFAULT_EXPIRY);
		producer.append_group().unwrap().finish().unwrap();
		let mut live = producer.append_group().unwrap();
		let mut consumer = producer.subscribe(replay());
		assert_eq!(consumer.assert_group().sequence, 0);
		assert_eq!(consumer.assert_group().sequence, 1);

		let woken = Arc::new(AtomicBool::new(false));
		let waiter = kio::Waiter::new(futures::task::waker(Arc::new(FlagWake(woken.clone()))));
		assert!(consumer.poll_recv_group(&waiter).is_pending());

		// A frame write wakes the readers following the largest, and only them. The live group's
		// first frame is the head it already raised the largest to, so the second one advances it.
		let mut follower = producer.subscribe(replay());
		let noop = kio::Waiter::noop();
		assert_eq!(follower.poll_largest(&noop), Poll::Ready(Some(Position::group(1))));
		live.write_frame(Timestamp::ZERO, b"x".as_slice()).unwrap();
		let advanced = Arc::new(AtomicBool::new(false));
		let largest = kio::Waiter::new(futures::task::waker(Arc::new(FlagWake(advanced.clone()))));
		assert!(follower.poll_largest(&largest).is_pending());

		live.write_frame(Timestamp::ZERO, b"y".as_slice()).unwrap();
		assert!(
			!woken.load(Ordering::SeqCst),
			"a no-op expiry scan must not wake track consumers"
		);
		assert!(advanced.load(Ordering::SeqCst), "the frame advanced the largest");
	}

	#[test]
	fn streaming_frame_write_expires_idle_siblings() {
		let producer = track_producer_expiring("test", Duration::from_secs(1));
		producer.append_group().unwrap().finish().unwrap(); // seq 0
		let live = producer.append_group().unwrap(); // seq 1
		let mut frame = live
			.create_frame(frame::Info {
				size: 1,
				timestamp: Some(Timestamp::ZERO),
			})
			.unwrap();

		elapse(&producer, Duration::from_secs(2));
		frame.write(b"x".as_slice()).unwrap();

		let expired = !producer.state.read().lookup.contains_key(&0);
		assert!(expired, "a streamed chunk runs expiry");
	}

	#[test]
	fn appended_datagram_expires_idle_groups() {
		let mut producer = track_producer_expiring("test", Duration::from_secs(1));
		producer.append_group().unwrap().finish().unwrap(); // seq 0
		producer.append_group().unwrap().finish().unwrap(); // seq 1

		elapse(&producer, Duration::from_secs(2));
		producer.append_datagram(Timestamp::ZERO, b"x".as_slice()).unwrap();

		let expired = !producer.state.read().lookup.contains_key(&0);
		assert!(expired, "an appended datagram runs expiry");
	}

	#[test]
	fn forwarded_datagram_expires_idle_groups() {
		let mut producer = track_producer_expiring("test", Duration::from_secs(1));
		producer.append_group().unwrap().finish().unwrap(); // seq 0
		producer.append_group().unwrap().finish().unwrap(); // seq 1

		elapse(&producer, Duration::from_secs(2));
		producer
			.insert_datagram(2, Timestamp::ZERO, bytes::Bytes::from_static(b"x"))
			.unwrap();

		let expired = !producer.state.read().lookup.contains_key(&0);
		assert!(expired, "a forwarded datagram runs expiry");
	}

	/// A stalled track keeps its newest group and gives up the ones before it by the wall
	/// clock: `max_age` measures how long ago each group's successor arrived as well as
	/// media time, so a publisher that stops writing cannot pin its superseded groups.
	#[test]
	#[cfg(feature = "async")]
	fn a_stalled_track_keeps_only_its_newest_group() {
		let mut producer = track_producer("test", Info::timed().with_max_age(Duration::from_secs(1)));
		for millis in [0, 10, 20] {
			append_at(&mut producer, millis);
		}
		assert_eq!(drain(&mut producer.subscribe(replay())), vec![0, 1, 2]);

		// Nothing is written again: media time stands still.
		elapse(&producer, Duration::from_secs(2));
		assert_eq!(
			drain(&mut producer.subscribe(replay())),
			vec![2],
			"the newest group is kept"
		);
		assert_eq!(
			live_groups(&producer.state.read()),
			3,
			"reclaiming the bytes is the pool's job"
		);
	}

	/// A peer whose timestamps stop advancing never ages a group out on media time. The wall
	/// clock still does.
	#[test]
	#[cfg(feature = "async")]
	fn frozen_timestamps_still_age_out_by_wall_clock() {
		let mut producer = track_producer("test", Info::timed());
		let budget = Subscription::default().with_max_delay(Duration::from_secs(1));
		append_at(&mut producer, 0);
		elapse(&producer, Duration::from_millis(500));
		append_at(&mut producer, 0);
		elapse(&producer, Duration::from_millis(800));
		append_at(&mut producer, 0);

		// Group 0's successor arrived 800ms ago, group 1's just now.
		assert_eq!(drain(&mut producer.subscribe(budget.clone())), vec![0, 1, 2]);
		elapse(&producer, Duration::from_millis(200));
		assert_eq!(drain(&mut producer.subscribe(budget.clone())), vec![1, 2]);
		elapse(&producer, Duration::from_secs(1));
		assert_eq!(drain(&mut producer.subscribe(budget)), vec![2]);
	}

	/// Groups that arrive at once are judged by media time, which convicts them before the
	/// wall clock can.
	#[test]
	#[cfg(feature = "async")]
	fn a_startup_burst_ages_out_by_media_time() {
		let mut producer = track_producer("test", Info::timed());
		for second in 0..5 {
			append_at(&mut producer, second * 1000);
		}

		// Group N reaches N + 1 seconds against the 4s edge: groups 0 and 1 are 3s and 2s
		// behind, past 1.5s. Their successors arrived just now.
		let mut subscriber = producer.subscribe(Subscription::default().with_max_delay(Duration::from_millis(1500)));
		assert_eq!(drain(&mut subscriber), vec![2, 3, 4]);
	}

	/// A read parked on a superseded group is released by the wall clock alone: nothing is
	/// written after its successor arrives, so no write could wake it. That holds whether a
	/// driver collected the pool before the read parked or only after.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn a_parked_read_wakes_at_its_wall_deadline() {
		for (info, primed) in [Info::default(), Info::timed()]
			.into_iter()
			.flat_map(|info| [(info.clone(), true), (info, false)])
		{
			let producer = track_producer("test", info.clone());
			let at = info.timescale.map(|_| Timestamp::ZERO);
			if primed {
				elapse(&producer, Duration::ZERO);
			}
			let mut subscriber = producer.subscribe(Subscription::default().with_max_delay(Duration::from_secs(1)));
			let mut held = producer.append_group().unwrap();
			held.write_frame(at, bytes::Bytes::from_static(b"x")).unwrap();
			let mut reading = subscriber.assert_group();
			assert!(reading.read_frame().now_or_never().unwrap().unwrap().is_some());

			// The successor never presents a frame, so media time never bounds the held group.
			let _successor = producer.append_group().unwrap();
			let woken = Arc::new(std::sync::atomic::AtomicBool::new(false));
			let waiter = kio::Waiter::new(futures::task::waker(Arc::new(FlagWake(woken.clone()))));
			assert!(reading.poll_read_frame(&waiter).is_pending());
			if !primed {
				elapse(&producer, Duration::ZERO);
				assert!(
					woken.swap(false, Ordering::SeqCst),
					"{info:?}: the first collection never woke the read"
				);
				assert!(reading.poll_read_frame(&waiter).is_pending());
			}

			elapse(&producer, Duration::from_millis(900));
			assert!(
				!woken.load(Ordering::SeqCst),
				"{info:?} primed={primed}: woken before the deadline"
			);
			elapse(&producer, Duration::from_millis(100));
			assert!(
				woken.load(Ordering::SeqCst),
				"{info:?} primed={primed}: the wall deadline never fired"
			);
			let result = reading.poll_read_frame(&waiter);
			assert!(matches!(result, Poll::Ready(Err(Error::Old))), "{info:?}: {result:?}");
		}
	}

	/// A successor whose header landed but whose first frame never does still starts the
	/// wall clock, so a relay ages the group before it as the origin does.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn a_frameless_successor_ages_its_predecessor() {
		for info in [Info::default(), Info::timed()] {
			let producer = track_producer("test", info.clone());
			let at = info.timescale.map(|_| Timestamp::ZERO);
			elapse(&producer, Duration::ZERO);
			let mut subscriber = producer.subscribe(Subscription::default().with_max_delay(Duration::from_secs(1)));
			let mut held = producer.append_group().unwrap();
			held.write_frame(at, bytes::Bytes::from_static(b"x")).unwrap();
			let mut reading = subscriber.assert_group();
			assert!(reading.read_frame().now_or_never().unwrap().unwrap().is_some());

			let woken = Arc::new(std::sync::atomic::AtomicBool::new(false));
			let waiter = kio::Waiter::new(futures::task::waker(Arc::new(FlagWake(woken.clone()))));
			assert!(reading.poll_read_frame(&waiter).is_pending());
			let _successor = producer.create_group(group::Info { sequence: 1 }).unwrap();
			assert!(
				woken.swap(false, Ordering::SeqCst),
				"{info:?}: the header never woke the read"
			);
			assert!(reading.poll_read_frame(&waiter).is_pending());

			elapse(&producer, Duration::from_millis(900));
			assert!(!woken.load(Ordering::SeqCst), "{info:?}: woken before the deadline");
			elapse(&producer, Duration::from_millis(100));
			assert!(woken.load(Ordering::SeqCst), "{info:?}: the wall deadline never fired");
			let result = reading.poll_read_frame(&waiter);
			assert!(matches!(result, Poll::Ready(Err(Error::Old))), "{info:?}: {result:?}");
		}
	}

	/// A handed-out group stays judgeable after the cache evicts it, and its successor too:
	/// under a budget past the cache window, a stream still waiting on it (a FIN never
	/// acknowledged) is convicted once its successor's arrival plus the budget passes, not
	/// by whichever newer group the cache still holds. The newest group never is. A
	/// subscribed group and a fetched one are judged alike.
	#[test]
	fn an_evicted_group_is_judged_by_its_evicted_successor() {
		let mut producer = track_producer_expiring("test", cache::DEFAULT_EXPIRY);
		let budget = cache::DEFAULT_EXPIRY * 2;
		let mut subscriber = producer.subscribe(Subscription::default().with_max_delay(budget));
		let waiter = kio::Waiter::noop();
		let expired = |held: &mut [group::Consumer; 2]| held.each_mut().map(|group| group.poll_expired(&waiter));

		// Frozen timestamps, so the wall clock alone convicts.
		append_at(&mut producer, 0);
		let fetch = group::Fetch::default().with_max_delay(budget);
		let mut held = [
			subscriber.assert_group(),
			producer
				.consume()
				.fetch_group(0, fetch)
				.now_or_never()
				.unwrap()
				.unwrap(),
		];
		for group in &mut held {
			assert!(matches!(group.poll_read_frame(&waiter), Poll::Ready(Ok(Some(_)))));
			assert!(matches!(group.poll_read_frame(&waiter), Poll::Ready(Ok(None))));
		}
		assert_eq!(expired(&mut held), [false; 2], "the newest group");

		elapse(&producer, Duration::from_secs(1));
		let mut last = append_at(&mut producer, 0); // the successor
		assert_eq!(expired(&mut held), [false; 2]);

		// The track runs on, a group a second, until a second short of the deadline.
		for _ in 0..budget.as_secs() - 1 {
			elapse(&producer, Duration::from_secs(1));
			last = append_at(&mut producer, 0);
		}
		producer.broadcast.pool.sweep();
		let state = producer.state.read();
		assert!(
			!state.lookup.contains_key(&0) && !state.lookup.contains_key(&1),
			"both evicted"
		);
		drop(state);
		assert_eq!(expired(&mut held), [false; 2], "early");

		elapse(&producer, Duration::from_secs(1));
		assert_eq!(expired(&mut held), [true; 2], "the successor arrived a budget ago");

		// The newest group outlives any budget.
		let mut newest = subscriber.assert_group();
		while newest.sequence < last {
			newest = subscriber.assert_group();
		}
		elapse(&producer, budget * 2);
		assert!(!newest.poll_expired(&waiter), "the newest group");
	}

	/// A recorded successor that failed hands the clock on to the next live group even once
	/// the cache reclaimed it before the reader looked again, rather than convicting by the
	/// failed one's arrival. A subscribed group and a fetched one are judged alike.
	#[test]
	fn a_reclaimed_failed_successor_hands_the_clock_on() {
		let mut producer = track_producer_expiring("test", cache::DEFAULT_EXPIRY);
		let budget = cache::DEFAULT_EXPIRY * 2;
		let mut subscriber = producer.subscribe(Subscription::default().with_max_delay(budget));
		let waiter = kio::Waiter::noop();
		let expired = |held: &mut [group::Consumer; 2]| held.each_mut().map(|group| group.poll_expired(&waiter));

		append_at(&mut producer, 0);
		let fetch = group::Fetch::default().with_max_delay(budget);
		let mut held = [
			subscriber.assert_group(),
			producer
				.consume()
				.fetch_group(0, fetch)
				.now_or_never()
				.unwrap()
				.unwrap(),
		];
		for group in &mut held {
			assert!(matches!(group.poll_read_frame(&waiter), Poll::Ready(Ok(Some(_)))));
			assert!(matches!(group.poll_read_frame(&waiter), Poll::Ready(Ok(None))));
		}

		let failed = producer.append_group().unwrap();
		assert_eq!(expired(&mut held), [false; 2], "the successor is recorded");
		failed.abort(Error::Cancel).unwrap();
		let replacement = Duration::from_secs(10);
		elapse(&producer, replacement);
		append_at(&mut producer, 0);
		producer.broadcast.pool.sweep();
		assert!(!producer.state.read().lookup.contains_key(&1), "reclaimed");

		elapse(&producer, budget - replacement);
		assert_eq!(
			expired(&mut held),
			[false; 2],
			"the failed successor arrived a budget ago"
		);

		elapse(&producer, replacement);
		assert_eq!(expired(&mut held), [true; 2], "the next live one arrived a budget ago");
	}

	/// On an ended track the cache expires idle groups in place rather than reclaiming them,
	/// so the reads skip them like any aborted group. A recorded successor that aged out that
	/// way still convicts the group before it once it arrived a budget ago.
	#[test]
	fn an_ended_track_judges_by_its_expired_successor() {
		let pool = cache::Pool::new(cache::Config::default().with_expiry(cache::DEFAULT_EXPIRY));
		let mut producer = track_producer_pooled("test", pool.clone());
		let budget = cache::DEFAULT_EXPIRY * 2;
		let mut subscriber = producer.subscribe(Subscription::default().with_max_delay(budget));
		let waiter = kio::Waiter::noop();

		append_at(&mut producer, 0);
		let mut held = subscriber.assert_group();
		assert!(matches!(held.poll_read_frame(&waiter), Poll::Ready(Ok(Some(_)))));
		assert!(matches!(held.poll_read_frame(&waiter), Poll::Ready(Ok(None))));

		pool.step(Duration::from_secs(1));
		append_at(&mut producer, 0); // the successor
		assert!(!held.poll_expired(&waiter), "the successor is recorded");
		let state = producer.state.downgrade();
		producer.finish().unwrap();
		drop(producer);

		// The first sweep dates the groups idle, the second expires them.
		let idle = cache::DEFAULT_EXPIRY + Duration::from_secs(1);
		pool.sweep();
		pool.step(idle);
		pool.sweep();
		assert_eq!(
			state.read(|state| state.lookup.get(&1).is_some_and(Slot::is_aborted)),
			Some(true),
			"expired in place"
		);
		assert!(!held.poll_expired(&waiter), "early");

		pool.step(budget - idle);
		assert!(held.poll_expired(&waiter), "the successor arrived a budget ago");
	}

	/// Empty groups are handed out like any other and retired by the wall clock like an
	/// untimed one, whether they finished without a frame or are still open with none. A
	/// publisher flooding a run of them cannot pin the group before the run either.
	#[test]
	#[cfg(feature = "async")]
	fn a_run_of_empty_groups_ages_out_by_wall_clock() {
		const RUN: u64 = 1000;
		let mut producer = track_producer("test", Info::timed());
		let budget = Subscription::default().with_max_delay(Duration::from_secs(1));
		let mut subscriber = producer.subscribe(budget.clone());
		append_at(&mut producer, 0);
		let mut open = Vec::new();
		for sequence in 1..=RUN {
			let mut group = producer.append_group().unwrap();
			match sequence % 2 {
				0 => group.finish().unwrap(),
				_ => open.push(group),
			}
		}
		append_at(&mut producer, 10);
		assert_eq!(drain(&mut producer.subscribe(budget.clone())).len() as u64, RUN + 2);

		elapse(&producer, Duration::from_secs(1));
		assert_eq!(drain(&mut subscriber), vec![RUN + 1], "the whole run is retired");
		assert_eq!(drain(&mut producer.subscribe(budget)), vec![RUN + 1]);
	}

	/// Disabling the pool's expiry keeps idle groups until byte pressure reclaims them.
	#[test]
	fn disabled_pool_expiry_never_reclaims() {
		let producer = track_producer_expiring("test", None);
		producer.append_group().unwrap().finish().unwrap(); // seq 0

		elapse(&producer, Duration::from_secs(3600));
		producer.append_group().unwrap().finish().unwrap(); // seq 1

		let state = producer.state.read();
		assert_eq!(live_groups(&state), 2);
	}

	#[test]
	fn max_delay_clamped_to_cache() {
		let producer = track_producer("test", Info::timed().with_max_age(Duration::from_secs(2)));

		// A max delay budget beyond the cache is capped in the aggregate; a group can't be
		// waited for longer than the publisher keeps it. The subscriber's own preference
		// is stored verbatim, so what it asked for stays readable.
		let mut subscriber = producer.subscribe(Subscription::default().with_max_delay(Duration::from_secs(10)));
		assert_eq!(subscriber.subscription().max_delay, Some(Duration::from_secs(10)));
		assert_eq!(producer.subscription().unwrap().max_delay, Some(Duration::from_secs(2)));

		// A budget within the cache is left alone, and ZERO (skip immediately) stays ZERO.
		subscriber
			.update(Subscription::default().with_max_delay(Duration::from_millis(500)))
			.unwrap();
		assert_eq!(
			producer.subscription().unwrap().max_delay,
			Some(Duration::from_millis(500))
		);

		subscriber
			.update(Subscription::default().with_max_delay(Duration::ZERO))
			.unwrap();
		assert_eq!(producer.subscription().unwrap().max_delay, Some(Duration::ZERO));
	}

	/// Mint a track under an origin whose retention ceiling is `cap`, so the
	/// local delivery budget is capped without rewriting the publisher's window.
	fn track_producer_capped(name: impl Into<Arc<str>>, info: Info, cap: Duration) -> Producer {
		Producer::new(
			Arc::new(broadcast::Info {
				cache_duration: cap,
				..Default::default()
			}),
			name,
			info,
		)
	}

	#[test]
	fn optional_age_keeps_publisher_metadata_and_local_policy_separate() {
		assert_eq!(Info::timed().max_age, None);
		for age in [None, Some(Duration::ZERO), Some(Duration::from_secs(30))] {
			let producer = track_producer_capped("optional", Info::timed().with_max_age(age), Duration::from_secs(1));
			assert_eq!(producer.info.max_age, age);
			assert_eq!(producer.subscribe(None).info().max_age, age);
			assert_eq!(
				producer.state.read().max_age_bound(),
				Some(age.unwrap_or(Duration::MAX).min(Duration::from_secs(1)))
			);
		}
	}

	#[test]
	fn origin_cache_duration_clamps_max_age() {
		// A publisher asking to keep groups for a minute is capped to the origin's 1s
		// ceiling; a publisher already below the ceiling is left alone (it's a min).
		let capped = track_producer_capped(
			"test",
			Info::timed().with_max_age(Duration::from_secs(60)),
			Duration::from_secs(1),
		);
		assert_eq!(capped.state.read().max_age_bound(), Some(Duration::from_secs(1)));
		assert_eq!(capped.subscribe(None).info().max_age, Some(Duration::from_secs(60)));

		let under = track_producer_capped(
			"test",
			Info::timed().with_max_age(Duration::from_millis(500)),
			Duration::from_secs(1),
		);
		assert_eq!(under.state.read().max_age_bound(), Some(Duration::from_millis(500)));
	}

	/// The origin ceiling clamps the media-timestamp budget only; wall-clock
	/// reclamation belongs to the pool's LRU window, not the ceiling.
	#[test]
	fn origin_cache_duration_does_not_wall_evict() {
		let producer = track_producer_capped(
			"test",
			Info::timed().with_max_age(Duration::from_secs(60)),
			Duration::from_secs(1),
		);
		producer.append_group().unwrap().finish().unwrap(); // seq 0

		// Far past the ceiling in wall time, but inside the pool's LRU window.
		elapse(&producer, Duration::from_secs(2));
		producer.append_group().unwrap().finish().unwrap(); // seq 1

		let state = producer.state.read();
		assert_eq!(live_groups(&state), 2);
	}

	#[test]
	fn max_delay_clamped_via_every_update_path() {
		let producer = track_producer("test", Info::timed().with_max_age(Duration::from_secs(2)));
		let over = Subscription::default().with_max_delay(Duration::from_secs(10));

		// The clamp lives in the aggregation, so it applies no matter which entry point
		// wrote the raw preference. Previously only `Subscriber::update` clamped.
		let mut subscriber = producer.subscribe(over.clone());
		assert_eq!(producer.subscription().unwrap().max_delay, Some(Duration::from_secs(2)));

		subscriber.control().update(over.clone()).unwrap();
		assert_eq!(producer.subscription().unwrap().max_delay, Some(Duration::from_secs(2)));

		subscriber.update(over).unwrap();
		assert_eq!(producer.subscription().unwrap().max_delay, Some(Duration::from_secs(2)));
	}

	#[test]
	fn max_delay_aggregate_clamps_across_subscribers() {
		let producer = track_producer("test", Info::timed().with_max_age(Duration::from_secs(2)));

		// The aggregate takes the max, then clamps once. Equivalent to clamping each
		// subscriber first, since `min` distributes over `max`.
		let _a = producer.subscribe(Subscription::default().with_max_delay(Duration::from_millis(500)));
		let _b = producer.subscribe(Subscription::default().with_max_delay(Duration::from_secs(10)));

		assert_eq!(producer.subscription().unwrap().max_delay, Some(Duration::from_secs(2)));
	}

	#[test]
	fn churned_subscribers_do_not_accumulate() {
		let producer = track_producer("test", Info::timed());
		let consumer = producer.consume();
		let _steady = producer.subscribe(None);

		// Departures remove their contribution even when nobody polls the aggregate.
		for _ in 0..100 {
			drop(producer.subscribe(None));
			drop(consumer.subscribe(None));
		}

		// Only the steady reader remains registered.
		let subs = producer.state.read().subscriptions.clone();
		let len = subs.lock().len();
		assert_eq!(len, 1, "departed subscribers accumulated");
	}

	#[test]
	fn departures_release_demand_after_a_peak() {
		let mut producer = track_producer("test", Info::timed());
		let waiter = kio::Waiter::noop();
		let _steady = producer.subscribe(None);
		assert!(producer.poll_subscription_changed(&waiter).is_ready());

		// A departed burst leaves no demand, even when its preferences matched the steady reader.
		drop((0..1000).map(|_| producer.subscribe(None)).collect::<Vec<_>>());
		for _ in 0..100 {
			drop(producer.subscribe(None));
			assert!(producer.poll_subscription_changed(&waiter).is_pending());

			let subs = producer.state.read().subscriptions.clone();
			assert_eq!(subs.lock().len(), 1, "departed demand survived");
		}
	}

	/// Append a finished group presenting at `millis`, so the track carries a media
	/// timeline for the drift budget to measure against.
	fn append_at(producer: &mut Producer, millis: u64) -> u64 {
		let mut group = producer.append_group().unwrap();
		group
			.write_frame(Timestamp::from_millis(millis).unwrap(), bytes::Bytes::from_static(b"x"))
			.unwrap();
		group.finish().unwrap();
		group.sequence
	}

	/// Every group the subscriber can read right now, in delivery order.
	#[cfg(feature = "async")]
	fn drain(subscriber: &mut Subscriber) -> Vec<u64> {
		let mut sequences = Vec::new();
		while let Some(Ok(Some(group))) = subscriber.recv_group().now_or_never() {
			sequences.push(group.sequence);
		}
		sequences
	}

	/// A rejoined copy judges the route's answer by the groups that survived the leave.
	///
	/// The cancel that idles a copy resets the group in flight. The route then answers past
	/// it, so the newest group still cached is two behind: a gap, which the cached group
	/// cannot be measured across. Judged by the group the copy held when it went idle, the
	/// answer would read as contiguous, and the cached group would reach all the way to the
	/// answer, fresh to any positive budget.
	#[test]
	#[cfg(feature = "async")]
	fn an_answer_past_a_reset_group_skips_the_cache() {
		let mut producer = track_producer("test", Info::timed());
		append_at(&mut producer, 0);
		let mut open = producer.append_group().unwrap();
		open.write_frame(Timestamp::from_millis(500).unwrap(), bytes::Bytes::from_static(b"x"))
			.unwrap();

		producer.set_idle();
		open.abort(Error::Cancel).unwrap();

		let mut answer = producer.create_group(group::Info { sequence: 2 }).unwrap();
		producer.set_live(Some(Position::group(2)), None);
		answer
			.write_frame(Timestamp::from_millis(533).unwrap(), bytes::Bytes::from_static(b""))
			.unwrap();

		let mut subscriber = producer.subscribe(Subscription::default().with_max_delay(Duration::from_millis(400)));
		assert_eq!(drain(&mut subscriber), vec![2]);
	}

	/// The cancel that idles a copy resets the group in flight, which the publisher never
	/// gave up: no reader hears it dropped, and the next feed sends it again.
	#[test]
	#[cfg(feature = "async")]
	fn a_reset_group_is_not_dropped_and_comes_again() {
		let mut producer = track_producer("test", Info::timed());
		append_at(&mut producer, 0);
		let mut open = producer.append_group().unwrap();
		open.write_frame(Timestamp::from_millis(1000).unwrap(), bytes::Bytes::from_static(b"x"))
			.unwrap();

		producer.set_idle();
		open.abort(Error::Cancel).unwrap();

		let mut subscriber = producer.subscribe(replay());
		let waiter = kio::Waiter::noop();
		assert!(subscriber.poll_drop(&waiter).is_pending(), "nothing was given up");

		let mut again = producer.create_group(group::Info { sequence: 1 }).unwrap();
		producer.set_live(Some(Position::group(1)), None);
		again
			.write_frame(Timestamp::from_millis(1000).unwrap(), bytes::Bytes::from_static(b"x"))
			.unwrap();
		assert_eq!(drain(&mut subscriber), vec![0, 1]);
		assert!(subscriber.poll_drop(&waiter).is_pending(), "nothing was given up");
	}

	#[test]
	#[cfg(feature = "async")]
	fn real_time_skips_a_backlog_to_the_live_edge() {
		let mut producer = track_producer("test", Info::timed());
		for second in 0..5 {
			append_at(&mut producer, second * 1000);
		}

		// The default budget is REAL_TIME: a subscriber joining a track that already
		// holds five seconds of history takes the live edge, not the history. This is
		// the ceiling a takeover backlog runs into.
		let mut subscriber = producer.subscribe(None);
		assert_eq!(drain(&mut subscriber), vec![4]);

		// And it stays caught up: the next group is live when it lands.
		append_at(&mut producer, 5000);
		assert_eq!(drain(&mut subscriber), vec![5]);
	}

	#[test]
	#[cfg(feature = "async")]
	fn real_time_skips_a_backlog_after_catching_up() {
		let mut producer = track_producer("test", Info::timed());
		append_at(&mut producer, 0);
		let mut subscriber = producer.subscribe(None);
		assert_eq!(drain(&mut subscriber), vec![0]);

		// Catch-up is a subscriber state, not a startup-only choice. A reader can pause
		// between groups and still needs to shed the backlog that accumulated meanwhile.
		for second in 1..6 {
			append_at(&mut producer, second * 1000);
		}
		assert_eq!(drain(&mut subscriber), vec![5]);
	}

	#[test]
	#[cfg(feature = "async")]
	fn a_newer_edge_changes_an_active_catch_up() {
		let mut producer = track_producer("test", Info::timed());
		for second in 0..5 {
			append_at(&mut producer, second * 1000);
		}
		let mut subscriber = producer.subscribe(Subscription::default().with_max_delay(Duration::from_secs(1)));
		assert_eq!(
			subscriber
				.recv_group()
				.now_or_never()
				.unwrap()
				.unwrap()
				.unwrap()
				.sequence,
			3
		);

		// Group 4 was the edge at the first delivery. Advancing twice puts it outside
		// the budget before the subscriber asks for another group.
		append_at(&mut producer, 5000);
		append_at(&mut producer, 10000);
		assert_eq!(drain(&mut subscriber), vec![5, 6]);
	}

	#[test]
	#[cfg(feature = "async")]
	fn a_growing_edge_changes_an_active_catch_up() {
		let mut producer = track_producer("test", Info::timed());
		append_at(&mut producer, 0);
		append_at(&mut producer, 1000);
		let mut edge = producer.append_group().unwrap();
		edge.write_frame(Timestamp::from_millis(2000).unwrap(), bytes::Bytes::from_static(b"a"))
			.unwrap();

		let mut subscriber = producer.subscribe(Subscription::default().with_max_delay(Duration::from_secs(2)));
		assert_eq!(
			subscriber
				.recv_group()
				.now_or_never()
				.unwrap()
				.unwrap()
				.unwrap()
				.sequence,
			0
		);

		// The edge is still the same group, but its newest presentation moved far
		// enough that group 1 has fallen out of range.
		edge.write_frame(Timestamp::from_millis(5000).unwrap(), bytes::Bytes::from_static(b"b"))
			.unwrap();
		assert_eq!(drain(&mut subscriber), vec![2]);
	}

	#[test]
	#[cfg(feature = "async")]
	fn a_budget_admits_groups_within_it() {
		let mut producer = track_producer("test", Info::timed());
		for second in 0..5 {
			append_at(&mut producer, second * 1000);
		}

		// Two seconds of tolerance keeps the groups presenting within 2s of the live
		// edge (2s, 3s, 4s) and drops the two below it. Group 1 reaches exactly 2s behind
		// the edge, and the reach bound is exclusive, so every frame it could hold is
		// already past the budget.
		let mut subscriber = producer.subscribe(Subscription::default().with_max_delay(Duration::from_secs(2)));
		assert_eq!(drain(&mut subscriber), vec![2, 3, 4]);
	}

	/// A fetch with a budget parks on its deadlines even when it returns a cached group at
	/// once, and fetches with distinct budgets never share one. Fetches that finish must not
	/// leave their deadlines indexed for as long as the track lives.
	#[test]
	#[cfg(feature = "async")]
	fn departed_budgets_leave_no_deadlines_behind() {
		let mut producer = track_producer("test", Info::timed());
		for second in 0..5 {
			append_at(&mut producer, second * 1000);
		}
		let consumer = producer.consume();
		for i in 0..1_000 {
			let budget = Duration::from_millis(1_500) + Duration::from_micros(i);
			let fetch = group::Fetch::default().with_max_delay(budget);
			let group = consumer.fetch_group(2, fetch).now_or_never().unwrap().unwrap();
			assert_eq!(group.sequence, 2);
		}
		let cache = producer.state.read().cache.clone();
		let (media, wall) = cache.wakes().deadlines_indexed();
		assert!(
			media <= 32 && wall <= 32,
			"{media} media and {wall} wall deadlines indexed"
		);
	}

	#[test]
	#[cfg(feature = "async")]
	fn a_budget_reaches_back_over_the_cache() {
		let mut producer = track_producer("test", Info::timed());
		for second in 0..5 {
			append_at(&mut producer, second * 1000);
		}

		// The default zero budget calls every non-latest group stale, so a subscription
		// that says nothing joins at the live edge.
		let mut live = producer.subscribe(None);
		assert_eq!(drain(&mut live), vec![4]);

		// Two seconds of tolerance covers the groups presenting within 2s of the live
		// edge, so the same join is handed the head of what it can still use. One bound
		// decides both what is sent and what is expired, so a subscriber is never sent
		// history it would discard on arrival.
		let budget = Subscription::default().with_max_delay(Duration::from_secs(2));
		let mut subscriber = producer.subscribe(budget);
		assert_eq!(drain(&mut subscriber), vec![2, 3, 4]);
	}

	#[test]
	#[cfg(feature = "async")]
	fn a_named_start_is_a_floor_not_a_request() {
		let mut producer = track_producer("test", Info::timed());
		for second in 0..5 {
			append_at(&mut producer, second * 1000);
		}

		// The budget is the only thing that asks for data; a named start only bounds how
		// far back it may reach. Naming group 1 at real time still delivers the live edge
		// alone, since the zero budget calls everything older stale.
		let named = Subscription::default().with_start(1);
		let mut subscriber = producer.subscribe(named);
		assert_eq!(drain(&mut subscriber), vec![4]);

		// A budget reaching further back than the floor is cut off at it.
		let floored = Subscription::default()
			.with_start(3)
			.with_max_delay(Duration::from_secs(10));
		let mut subscriber = producer.subscribe(floored);
		assert_eq!(drain(&mut subscriber), vec![3, 4]);

		// A floor below what the budget admits changes nothing.
		let slack = Subscription::default()
			.with_start(1)
			.with_max_delay(Duration::from_secs(2));
		let mut subscriber = producer.subscribe(slack);
		assert_eq!(drain(&mut subscriber), vec![2, 3, 4]);
	}

	#[test]
	#[cfg(feature = "async")]
	fn a_floor_above_the_live_edge_waits_there() {
		let mut producer = track_producer("test", Info::timed());
		for second in 0..3 {
			append_at(&mut producer, second * 1000);
		}

		// A resumed subscription names where it left off, which may not exist yet. The
		// cursor sits at the floor rather than sliding back to what is cached.
		let resumed = Subscription::default()
			.with_start(7)
			.with_max_delay(Duration::from_secs(10));
		let mut subscriber = producer.subscribe(resumed);
		assert_eq!(drain(&mut subscriber), Vec::<u64>::new());
		append_at(&mut producer, 3000); // sequence 3: still below the floor
		assert_eq!(drain(&mut subscriber), Vec::<u64>::new());
		for second in 4..8 {
			append_at(&mut producer, second * 1000);
		}
		assert_eq!(drain(&mut subscriber), vec![7]);
	}

	#[test]
	#[cfg(feature = "async")]
	fn a_late_lower_group_within_the_budget_is_delivered() {
		let producer = track_producer("test", Info::timed());
		for (sequence, millis) in [(5, 1000), (6, 2000), (7, 3000)] {
			let mut group = producer.create_group(group::Info { sequence }).unwrap();
			group
				.write_frame(Timestamp::from_millis(millis).unwrap(), bytes::Bytes::from_static(b"x"))
				.unwrap();
			group.finish().unwrap();
		}

		let mut subscriber = producer.subscribe(Subscription::default().with_max_delay(Duration::from_secs(5)));
		assert_eq!(drain(&mut subscriber), vec![5, 6, 7]);

		// Arriving below everything already delivered is not what makes content stale:
		// the budget is the only gate, and this straggler's timestamp is within it. A
		// consumer that needs sequence order reorders (or drops) it itself.
		let mut late = producer.create_group(group::Info { sequence: 4 }).unwrap();
		late.write_frame(Timestamp::from_millis(500).unwrap(), bytes::Bytes::from_static(b"late"))
			.unwrap();
		late.finish().unwrap();
		assert_eq!(drain(&mut subscriber), vec![4]);
	}

	#[test]
	#[cfg(feature = "async")]
	fn drift_is_measured_in_presentation_time_not_arrival_time() {
		let mut producer = track_producer("test", Info::timed());
		// A relay ingesting a backlog creates every group at once, so arrival time says
		// they are all equally fresh. Their timestamps say otherwise, which is the whole
		// point of measuring in presentation time.
		for second in 0..4 {
			append_at(&mut producer, second * 1000);
		}

		let mut subscriber = producer.subscribe(Subscription::default().with_max_delay(Duration::from_millis(1500)));
		assert_eq!(drain(&mut subscriber), vec![1, 2, 3]);
	}

	#[test]
	#[cfg(feature = "async")]
	fn a_stamped_successor_expires_an_unstamped_group() {
		let mut producer = track_producer("test", Info::timed());
		let mut subscriber = producer.subscribe(None);
		let _open = producer.append_group().unwrap(); // seq 0 stalls before its first frame

		append_at(&mut producer, 1000); // seq 1 proves the live feed moved on

		// The candidate needs no timestamp of its own: its reach is where its stamped
		// successor begins, which the zero budget already puts out of range.
		assert_eq!(drain(&mut subscriber), vec![1]);
	}

	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn a_handed_out_group_expires_while_its_first_frame_is_stalled() {
		let mut producer = track_producer("test", Info::timed());
		let mut subscriber = producer.subscribe(None);
		let _writer_1 = producer.append_group().unwrap();

		let mut stalled = subscriber.recv_group().await.unwrap().expect("stalled group");
		let pending = moq_clock::sim::spawn(async move { stalled.read_frame().await });
		moq_clock::sim::yield_now().await;
		assert!(
			!pending.is_finished(),
			"the empty live edge still waits for its first frame"
		);

		elapse(&producer, Duration::from_secs(1));
		append_at(&mut producer, 1000);

		// The producer never finished it, so even an empty prefix is not the group.
		let result = pending.await.unwrap();
		assert!(matches!(result, Err(Error::Old)), "the held group fails: {result:?}");
	}

	/// A first timestamp on a *newer* group can convict a held one, so the held reader has
	/// to be woken by it. The conviction needs a group beyond the held one's successor:
	/// a group is bounded by where its successor begins, so the successor itself never
	/// proves it stale.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn a_handed_out_group_wakes_when_a_newer_group_gets_its_first_timestamp() {
		let mut producer = track_producer("test", Info::timed());
		let mut subscriber = producer.subscribe(Subscription::default().with_max_delay(Duration::from_millis(500)));
		let mut old = producer.append_group().unwrap();
		old.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"old"))
			.unwrap();

		// Group 1 bounds group 0's reach at 1s.
		append_at(&mut producer, 1000);

		let mut held = subscriber.recv_group().await.unwrap().expect("old group");
		assert!(held.read_frame().await.unwrap().is_some());

		let mut live = producer.append_group().unwrap();
		let pending = moq_clock::sim::spawn(async move { held.read_frame().await });
		moq_clock::sim::yield_now().await;
		assert!(!pending.is_finished(), "the newer group has no timestamp yet");

		// 2s edge against group 0's 1s reach: a full second past the budget.
		live.write_frame(
			Timestamp::from_millis(2000).unwrap(),
			bytes::Bytes::from_static(b"live"),
		)
		.unwrap();
		moq_clock::sim::yield_now().await;

		assert!(pending.is_finished(), "the new presentation edge wakes the held reader");
		// Drained but never finished, so the budget fails the group.
		let result = pending.await.unwrap();
		assert!(matches!(result, Err(Error::Old)), "the held group fails: {result:?}");
	}

	/// An appended group wakes only the parked reads whose expiry it crosses, plus the
	/// newest, whose successor it is. A 2s audio backlog at 400 groups/s parks ~800 reads,
	/// and waking every one per append pins a publisher's runtime.
	#[test]
	fn an_append_wakes_only_the_parked_reads_it_expires() {
		struct Count(std::sync::atomic::AtomicUsize);
		impl std::task::Wake for Count {
			fn wake(self: Arc<Self>) {
				self.wake_by_ref();
			}
			fn wake_by_ref(self: &Arc<Self>) {
				self.0.fetch_add(1, Ordering::SeqCst);
			}
		}

		const FRAME: u64 = 2500; // micros, one Opus frame per group
		const PARKED: u64 = 64;
		let producer = track_producer("test", Info::timed());
		let mut subscriber = producer.subscribe(Subscription::default().with_max_delay(Duration::from_millis(200)));
		// Left open, so every read parks instead of ending.
		let _open: Vec<_> = (0..PARKED)
			.map(|i| {
				let mut group = producer.append_group().unwrap();
				group
					.write_frame(
						Timestamp::from_micros(i * FRAME).unwrap(),
						bytes::Bytes::from_static(b"x"),
					)
					.unwrap();
				group
			})
			.collect();

		let counts: Vec<_> = (0..PARKED)
			.map(|_| Arc::new(Count(std::sync::atomic::AtomicUsize::new(0))))
			.collect();
		let waiters: Vec<_> = counts
			.iter()
			.map(|count| kio::Waiter::new(std::task::Waker::from(count.clone())))
			.collect();
		let mut held: Vec<_> = waiters
			.iter()
			.map(|waiter| {
				let Poll::Ready(Ok(Some(mut group))) = subscriber.poll_recv_group(waiter) else {
					panic!("every group is cached");
				};
				assert!(matches!(group.poll_read_frame(waiter), Poll::Ready(Ok(Some(_)))));
				group
			})
			.collect();
		for (group, waiter) in held.iter_mut().zip(&waiters) {
			assert!(group.poll_read_frame(waiter).is_pending(), "nothing is stale yet");
		}
		for count in &counts {
			count.0.store(0, Ordering::SeqCst);
		}

		// Read `i` can reach its successor's start, `(i + 1) * FRAME`, so an edge at 210ms
		// puts reads 0..=3 a full 200ms behind and leaves every later one in budget.
		let mut next = producer.append_group().unwrap();
		next.write_frame(
			Timestamp::from_micros(210_000).unwrap(),
			bytes::Bytes::from_static(b"x"),
		)
		.unwrap();

		let woken: Vec<_> = (0..PARKED)
			.filter(|&i| counts[i as usize].0.load(Ordering::SeqCst) > 0)
			.collect();
		assert_eq!(
			woken,
			vec![0, 1, 2, 3, PARKED - 1],
			"only the expired reads and the newest wake"
		);

		for (i, (group, waiter)) in held.iter_mut().zip(&waiters).enumerate() {
			let result = group.poll_read_frame(waiter);
			if i < 4 {
				assert!(
					matches!(result, Poll::Ready(Err(Error::Old))),
					"read {i} expired: {result:?}"
				);
			} else {
				assert!(result.is_pending(), "read {i} is in budget: {result:?}");
			}
		}
	}

	/// A parked read wakes only once the edge reaches its deadline: its successor's start
	/// plus its budget. A first frame below the edge moves nothing, and a new edge short of
	/// the deadline cannot convict it, so neither wakes it.
	#[test]
	#[cfg(feature = "async")]
	fn a_parked_read_wakes_only_once_the_edge_reaches_its_deadline() {
		let mut producer = track_producer("test", Info::timed());
		let mut subscriber = producer.subscribe(Subscription::default().with_max_delay(Duration::from_secs(10)));
		let mut head = producer.append_group().unwrap();
		head.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"head"))
			.unwrap();
		append_at(&mut producer, 1000); // the successor bounds the head's reach
		let mut between = producer.append_group().unwrap();
		append_at(&mut producer, 3000); // the edge

		let mut held = subscriber
			.recv_group()
			.now_or_never()
			.unwrap()
			.unwrap()
			.expect("head group");
		assert_eq!(held.sequence, 0);
		assert!(held.read_frame().now_or_never().unwrap().unwrap().is_some());

		let woken = Arc::new(std::sync::atomic::AtomicBool::new(false));
		let waker = futures::task::waker(Arc::new(FlagWake(woken.clone())));
		let mut cx = std::task::Context::from_waker(&waker);
		let mut next = std::pin::pin!(held.read_frame());
		assert!(next.as_mut().poll(&mut cx).is_pending());

		between
			.write_frame(Timestamp::from_millis(2000).unwrap(), bytes::Bytes::from_static(b"x"))
			.unwrap();
		assert!(
			!woken.load(Ordering::SeqCst),
			"a first frame below the edge moves neither the reach nor the edge"
		);

		let mut beyond = producer.append_group().unwrap();
		beyond
			.write_frame(Timestamp::from_millis(4000).unwrap(), bytes::Bytes::from_static(b"x"))
			.unwrap();
		assert!(!woken.load(Ordering::SeqCst), "a new edge short of the deadline");

		// The edge's own later frame reaches the deadline.
		beyond
			.write_frame(Timestamp::from_millis(11_000).unwrap(), bytes::Bytes::from_static(b"x"))
			.unwrap();
		assert!(
			woken.load(Ordering::SeqCst),
			"the edge reaching the deadline wakes the read"
		);
		let result = next.as_mut().poll(&mut cx);
		assert!(
			matches!(result, Poll::Ready(Err(Error::Old))),
			"the head is stale: {result:?}"
		);
	}

	/// A batch must notify expiry at its highest PTS even when its decode-order tail is lower.
	#[test]
	#[cfg(feature = "async")]
	fn a_reordered_batch_wakes_the_group_pts_drift_deadline() {
		let mut producer = track_producer("test", Info::timed());
		let mut subscriber = producer.subscribe(Subscription::default().with_max_delay(Duration::from_secs(10)));
		let mut head = producer.append_group().unwrap();
		head.write_frame(Timestamp::from_millis(0).unwrap(), bytes::Bytes::new())
			.unwrap();
		append_at(&mut producer, 1000);
		let mut edge = producer.append_group().unwrap();
		edge.write_frame(Timestamp::from_millis(2000).unwrap(), bytes::Bytes::new())
			.unwrap();
		let mut held = subscriber.recv_group().now_or_never().unwrap().unwrap().unwrap();
		assert!(held.read_frame().now_or_never().unwrap().unwrap().is_some());
		let woken = Arc::new(std::sync::atomic::AtomicBool::new(false));
		let waker = futures::task::waker(Arc::new(FlagWake(woken.clone())));
		let mut cx = std::task::Context::from_waker(&waker);
		let mut next = std::pin::pin!(held.read_frame());
		assert!(next.as_mut().poll(&mut cx).is_pending());
		let mut frames = crate::frame::Buffer::<2>::new();
		for ms in [11_000, 10_000] {
			frames
				.push(crate::frame::Frame::new(
					Timestamp::from_millis(ms).unwrap(),
					bytes::Bytes::new(),
				))
				.unwrap();
		}
		edge.write_frames(&mut frames).unwrap();
		assert!(
			woken.load(Ordering::SeqCst),
			"the batch's highest PTS reaches the deadline"
		);
		assert!(matches!(next.as_mut().poll(&mut cx), Poll::Ready(Err(Error::Old))));
		head.finish().unwrap();
		edge.finish().unwrap();
	}

	#[test]
	#[cfg(feature = "async")]
	fn an_adopted_high_pts_wakes_the_group_drift_deadline() {
		let mut producer = track_producer("test", Info::timed());
		let mut subscriber = producer.subscribe(Subscription::default().with_max_delay(Duration::from_secs(10)));
		let mut old = producer.append_group().unwrap();
		old.write_frame(Timestamp::from_millis(0).unwrap(), bytes::Bytes::new())
			.unwrap();
		append_at(&mut producer, 1000);
		let mut edge = producer.append_group().unwrap();
		edge.start_at(2).unwrap();
		edge.write_frame(Timestamp::from_millis(2000).unwrap(), bytes::Bytes::new())
			.unwrap();
		let mut held = subscriber.recv_group().now_or_never().unwrap().unwrap().unwrap();
		assert!(held.read_frame().now_or_never().unwrap().unwrap().is_some());
		let woken = Arc::new(std::sync::atomic::AtomicBool::new(false));
		let waker = futures::task::waker(Arc::new(FlagWake(woken.clone())));
		let mut cx = std::task::Context::from_waker(&waker);
		let mut next = std::pin::pin!(held.read_frame());
		assert!(next.as_mut().poll(&mut cx).is_pending());
		let mut prefix = group::Info { sequence: 2 }.produce();
		for ms in [1500, 11_000] {
			prefix
				.write_frame(Timestamp::from_millis(ms).unwrap(), bytes::Bytes::new())
				.unwrap();
		}
		let head = prefix.consume().handoff().head().unwrap();
		edge.handle().adopt_head(&head);
		assert_eq!(edge.handle().latest(), Some(Timestamp::from_millis(11_000).unwrap()));
		assert!(
			woken.load(Ordering::SeqCst),
			"the adopted prefix reaches the drift deadline"
		);
		assert!(matches!(next.as_mut().poll(&mut cx), Poll::Ready(Err(Error::Old))));
		old.finish().unwrap();
		edge.finish().unwrap();
		prefix.finish().unwrap();
	}

	/// An aborted edge hands the edge to a group between it and the successor, and neither
	/// the abort nor that group's frames touch the track, so the read must still wake.
	#[test]
	#[cfg(feature = "async")]
	fn a_parked_read_watches_its_edge_abort() {
		let mut producer = track_producer("test", crate::track::Info::timed());
		let mut subscriber = producer.subscribe(Subscription::default().with_max_delay(Duration::from_secs(5)));
		let mut head = producer.append_group().unwrap();
		head.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"head"))
			.unwrap();
		append_at(&mut producer, 1000); // the successor bounds the head's reach
		let mut between = producer.append_group().unwrap();
		let mut edge = producer.append_group().unwrap();
		edge.write_frame(Timestamp::from_millis(2000).unwrap(), bytes::Bytes::from_static(b"x"))
			.unwrap();
		edge.finish().unwrap();

		let mut held = subscriber
			.recv_group()
			.now_or_never()
			.unwrap()
			.unwrap()
			.expect("head group");
		assert_eq!(held.sequence, 0);
		assert!(held.read_frame().now_or_never().unwrap().unwrap().is_some());

		let woken = Arc::new(std::sync::atomic::AtomicBool::new(false));
		let waker = futures::task::waker(Arc::new(FlagWake(woken.clone())));
		let mut cx = std::task::Context::from_waker(&waker);
		let mut next = std::pin::pin!(held.read_frame());
		assert!(next.as_mut().poll(&mut cx).is_pending());

		edge.abort(Error::Cancel).unwrap();
		// Starts between its neighbors, then runs past the deadline.
		for millis in [1500, 10_000] {
			between
				.write_frame(Timestamp::from_millis(millis).unwrap(), bytes::Bytes::from_static(b"x"))
				.unwrap();
		}
		assert!(
			woken.load(Ordering::SeqCst),
			"the edge's abort, then a new edge below it, lost the wakeup"
		);
		let result = next.as_mut().poll(&mut cx);
		assert!(
			matches!(result, Poll::Ready(Err(Error::Old))),
			"the head is stale: {result:?}"
		);
	}

	/// An aborted successor hands the reach to the next group, which sits below the edge
	/// and so is watched only because the read re-selects its successor.
	#[test]
	#[cfg(feature = "async")]
	fn a_parked_read_watches_the_replacement_for_an_aborted_successor() {
		let mut producer = track_producer("test", Info::timed());
		let mut subscriber = producer.subscribe(Subscription::default().with_max_delay(Duration::from_millis(500)));
		let mut head = producer.append_group().unwrap();
		head.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"head"))
			.unwrap();
		let successor = producer.append_group().unwrap();
		let mut replacement = producer.append_group().unwrap();
		append_at(&mut producer, 20_000); // the edge

		let mut held = subscriber
			.recv_group()
			.now_or_never()
			.unwrap()
			.unwrap()
			.expect("head group");
		assert_eq!(held.sequence, 0, "an unstamped successor leaves the reach unbounded");
		assert!(held.read_frame().now_or_never().unwrap().unwrap().is_some());

		let woken = Arc::new(std::sync::atomic::AtomicBool::new(false));
		let waker = futures::task::waker(Arc::new(FlagWake(woken.clone())));
		let mut cx = std::task::Context::from_waker(&waker);
		let mut next = std::pin::pin!(held.read_frame());
		assert!(next.as_mut().poll(&mut cx).is_pending());

		successor.abort(Error::Cancel).unwrap();
		assert!(
			woken.load(Ordering::SeqCst),
			"the successor's abort wakes the parked read"
		);
		woken.store(false, Ordering::SeqCst);
		assert!(
			next.as_mut().poll(&mut cx).is_pending(),
			"the unstamped replacement leaves the reach unbounded"
		);

		replacement
			.write_frame(Timestamp::from_millis(1000).unwrap(), bytes::Bytes::from_static(b"x"))
			.unwrap();
		assert!(
			woken.load(Ordering::SeqCst),
			"the replacement's first frame bounds the reach"
		);
		let result = next.as_mut().poll(&mut cx);
		assert!(
			matches!(result, Poll::Ready(Err(Error::Old))),
			"the head is stale: {result:?}"
		);
	}

	/// An edge's abort hands the edge back to a lower group whose frames already ran past
	/// the deadline. No frame write crosses it afterwards, and the abort never touches the
	/// track, so only the read watching the edge's abort sees the head go stale.
	#[test]
	#[cfg(feature = "async")]
	fn a_parked_read_watches_its_overtaken_edge_abort() {
		let mut producer = track_producer("test", crate::track::Info::timed());
		let mut subscriber = producer.subscribe(Subscription::default().with_max_delay(Duration::from_secs(5)));
		let mut head = producer.append_group().unwrap();
		head.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"head"))
			.unwrap();
		append_at(&mut producer, 1000); // the successor bounds the head's reach
		// Starts below the edge but presents past the deadline.
		let mut long = producer.append_group().unwrap();
		for millis in [1500, 10_000] {
			long.write_frame(Timestamp::from_millis(millis).unwrap(), bytes::Bytes::from_static(b"x"))
				.unwrap();
		}
		long.finish().unwrap();
		let mut edge = producer.append_group().unwrap();
		edge.write_frame(Timestamp::from_millis(2000).unwrap(), bytes::Bytes::from_static(b"x"))
			.unwrap();
		edge.finish().unwrap();

		let mut held = subscriber
			.recv_group()
			.now_or_never()
			.unwrap()
			.unwrap()
			.expect("head group");
		assert_eq!(held.sequence, 0, "the edge keeps the head within budget");
		assert!(held.read_frame().now_or_never().unwrap().unwrap().is_some());

		let woken = Arc::new(std::sync::atomic::AtomicBool::new(false));
		let waker = futures::task::waker(Arc::new(FlagWake(woken.clone())));
		let mut cx = std::task::Context::from_waker(&waker);
		let mut next = std::pin::pin!(held.read_frame());
		assert!(next.as_mut().poll(&mut cx).is_pending());

		edge.abort(Error::Cancel).unwrap();
		assert!(woken.load(Ordering::SeqCst), "the edge's abort lost its wakeup");
		let result = next.as_mut().poll(&mut cx);
		assert!(
			matches!(result, Poll::Ready(Err(Error::Old))),
			"the head is stale: {result:?}"
		);
	}

	/// A group's header lands before its first frame, which moves no edge. The first frame
	/// has to wake a parked read it expires, though the read sits below the group's nearest
	/// started predecessor.
	#[test]
	#[cfg(feature = "async")]
	fn revealing_a_presented_group_wakes_a_read_it_expires() {
		let mut producer = track_producer("test", Info::timed());
		let mut subscriber = producer.subscribe(Subscription::default().with_max_delay(Duration::from_millis(500)));
		let mut head = producer.append_group().unwrap();
		head.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"head"))
			.unwrap();
		append_at(&mut producer, 1000); // the successor bounds the head's reach

		let mut held = subscriber
			.recv_group()
			.now_or_never()
			.unwrap()
			.unwrap()
			.expect("head group");
		assert_eq!(held.sequence, 0);
		assert!(held.read_frame().now_or_never().unwrap().unwrap().is_some());

		let woken = Arc::new(std::sync::atomic::AtomicBool::new(false));
		let waker = futures::task::waker(Arc::new(FlagWake(woken.clone())));
		let mut cx = std::task::Context::from_waker(&waker);
		let mut next = std::pin::pin!(held.read_frame());
		assert!(next.as_mut().poll(&mut cx).is_pending());

		let mut received = producer.create_group(group::Info { sequence: 2 }).unwrap();
		woken.store(false, Ordering::SeqCst);
		assert!(
			next.as_mut().poll(&mut cx).is_pending(),
			"the frameless group moves no edge"
		);

		received
			.write_frame(Timestamp::from_millis(2000).unwrap(), bytes::Bytes::from_static(b"x"))
			.unwrap();
		assert!(woken.load(Ordering::SeqCst), "the first frame wakes the expired read");
		let result = next.as_mut().poll(&mut cx);
		assert!(
			matches!(result, Poll::Ready(Err(Error::Old))),
			"the head is stale: {result:?}"
		);
	}

	/// A route can declare the end, then land every group's header before its first frame.
	/// Readers reach those groups at once, and the end with them, but nothing counts a
	/// frameless group as content: the track shows nothing past the first until it starts.
	#[test]
	#[cfg(feature = "async")]
	fn a_frameless_group_does_not_count_as_content() {
		let mut producer = track_producer("test", crate::track::Info::timed());
		let mut subscriber = producer.subscribe(Subscription::default().with_max_delay(Duration::from_secs(60)));
		producer.finish_at(Position::group(2)).unwrap();
		let mut received: Vec<_> = (0..2)
			.map(|sequence| producer.create_group(group::Info { sequence }).unwrap())
			.collect();
		let mut first = subscriber.assert_group();
		let mut second = subscriber.assert_group();
		assert_eq!((first.sequence, second.sequence), (0, 1));
		assert!(subscriber.recv_group().now_or_never().unwrap().unwrap().is_none());

		let waiter = kio::Waiter::noop();
		assert!(
			producer.poll_past(0, &waiter).is_pending(),
			"a frameless group is not content"
		);
		for group in &mut received {
			group
				.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"x"))
				.unwrap();
		}
		assert!(
			producer.poll_past(0, &waiter).is_ready(),
			"its first frame makes it content"
		);
		assert!(first.read_frame().now_or_never().unwrap().unwrap().is_some());
		assert!(second.read_frame().now_or_never().unwrap().unwrap().is_some());
	}

	/// A parked `poll_past` wakes when a later group starts, not when its header lands.
	#[test]
	fn poll_past_wakes_when_a_later_group_starts() {
		use std::sync::atomic::{AtomicBool, Ordering};

		let producer = track_producer("test", Info::timed());
		let woken = Arc::new(AtomicBool::new(false));
		let waiter = kio::Waiter::new(futures::task::waker(Arc::new(FlagWake(woken.clone()))));
		assert!(producer.poll_past(0, &waiter).is_pending());

		let mut group = producer.create_group(group::Info { sequence: 1 }).unwrap();
		woken.store(false, Ordering::SeqCst);
		assert!(
			producer.poll_past(0, &waiter).is_pending(),
			"a frameless header is not content"
		);
		group
			.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"x"))
			.unwrap();
		assert!(woken.load(Ordering::SeqCst), "its first frame wakes the parked poll");
		assert!(producer.poll_past(0, &waiter).is_ready());
	}

	/// A frameless group whose stream ends with no frame, cleanly or reset, ends its readers
	/// with it, and the track's end follows.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn a_frameless_group_that_ends_empty_ends_its_readers() {
		for abort in [false, true] {
			let mut producer = track_producer("test", None);
			let mut subscriber = producer.subscribe(Subscription::default().with_max_delay(Duration::from_secs(60)));
			producer.finish_at(Position::group(1)).unwrap();
			let mut group = producer.create_group(group::Info { sequence: 0 }).unwrap();
			let mut reading = subscriber.assert_group();
			assert_eq!(reading.sequence, 0);

			let ended = {
				let mut next = std::pin::pin!(reading.read_frame());
				assert!(futures::poll!(next.as_mut()).is_pending(), "the group is still open");
				match abort {
					true => group.abort(Error::Cancel).unwrap(),
					false => group.finish().unwrap(),
				}
				moq_clock::timeout(Duration::from_secs(1), next)
					.await
					.expect("the end wakes the reader")
			};
			match abort {
				true => assert!(matches!(ended, Err(Error::Cancel)), "{ended:?}"),
				false => assert!(matches!(ended, Ok(None)), "{ended:?}"),
			}
			assert!(subscriber.recv_group().now_or_never().unwrap().unwrap().is_none());
		}
	}

	/// The ordinary live case, at the default real-time budget: 2s GOPs produced one at
	/// a time and read as they arrive. The budget must take the live edge without
	/// shortening the group the reader is already on, so every frame of every group is
	/// delivered.
	///
	/// The reader is parked at its group's end when the successor opens, because the FIN
	/// and the next group's first frame are separate events and the wire does not order
	/// them. A zero budget gives up there, and the group fails with [`Error::Old`] since
	/// its producer never finished it, but only after the reader took every frame.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn real_time_reads_a_live_stream_without_truncating_it() {
		let producer = track_producer("test", Info::timed());
		let mut subscriber = producer.subscribe(None);

		let gop = |n: u64| {
			[
				Timestamp::from_millis(n * 2000).unwrap(),
				Timestamp::from_millis(n * 2000 + 1900).unwrap(),
			]
		};
		let write = |group: &mut group::Producer, timestamp| {
			group.write_frame(timestamp, bytes::Bytes::from_static(b"x")).unwrap();
		};

		let mut open = producer.append_group().unwrap();
		write(&mut open, gop(0)[0]);
		write(&mut open, gop(0)[1]);
		let mut reading = subscriber.recv_group().await.unwrap().expect("the live group");

		let mut read = Vec::new();
		for n in 1..5u64 {
			let sequence = reading.sequence;
			let mut frames = 0;
			while let Some(res) = reading.read_frame().now_or_never() {
				match res.expect("no truncation while draining") {
					Some(_) => frames += 1,
					None => panic!("group {sequence} ended early"),
				}
			}
			read.push((sequence, frames));

			let next = {
				// Parked at the end of the current group: every frame is read and no FIN
				// has landed.
				let mut end = std::pin::pin!(reading.read_frame());
				assert!(futures::poll!(end.as_mut()).is_pending(), "parked on the FIN");

				// The next keyframe opens its group. The verdict is taken here, in the
				// window before the previous group's FIN arrives.
				let mut opened = producer.append_group().unwrap();
				write(&mut opened, gop(n)[0]);
				let verdict = futures::poll!(end.as_mut());

				open.finish().unwrap();
				let res = match verdict {
					Poll::Ready(res) => res,
					Poll::Pending => end.await,
				};
				assert!(
					matches!(res, Err(Error::Old)),
					"group {sequence} gives up at the boundary, after its last frame: {res:?}"
				);
				opened
			};
			let mut next = next;
			write(&mut next, gop(n)[1]);

			reading = subscriber.recv_group().await.unwrap().expect("the next live group");
			open = next;
		}

		assert_eq!(read, vec![(0, 2), (1, 2), (2, 2), (3, 2)], "every frame of every group");
	}

	/// Decode-order B-frames cannot rewind the presentation edge and re-admit an old group.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn a_reordered_tail_keeps_the_group_pts_drift_verdict() {
		for reordered in [false, true] {
			let producer = track_producer("test", Info::timed());
			let mut subscriber = producer.subscribe(Subscription::default().with_max_delay(Duration::from_millis(80)));
			for ms in [0, 100] {
				let mut group = producer.append_group().unwrap();
				group
					.write_frame(Timestamp::from_millis(ms).unwrap(), bytes::Bytes::new())
					.unwrap();
				group.finish().unwrap();
			}
			let mut edge = producer.append_group().unwrap();
			for ms in [150, 200] {
				edge.write_frame(Timestamp::from_millis(ms).unwrap(), bytes::Bytes::new())
					.unwrap();
			}
			if reordered {
				edge.write_frame(Timestamp::from_millis(160).unwrap(), bytes::Bytes::new())
					.unwrap();
			}
			edge.finish().unwrap();
			// Group 0 can reach 100ms: 100ms behind the 200ms edge, past its 80ms budget.
			// Group 1 can reach 150ms and stays within the same budget.
			assert_eq!(
				subscriber.recv_group().await.unwrap().unwrap().sequence,
				1,
				"reordered {reordered}"
			);
		}
	}

	/// A budget is spent from where the reader stands, not from where its group opened.
	///
	/// A 2s GOP with a 1s budget: the reader has drained to 1900ms when the next group
	/// opens at 2000ms, so it is 100ms behind the live edge and well inside what it
	/// asked for. A straggling frame of the old group arriving after the new one opened
	/// (which is ordinary, the two are separate streams) must still reach it.
	///
	/// Measuring from the group's first frame instead makes the drift 2000ms, so a
	/// budget shorter than one GOP would drop the tail of every GOP.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn a_budget_is_measured_from_the_readers_position() {
		let producer = track_producer("test", Info::timed());
		let mut subscriber = producer.subscribe(Subscription::default().with_max_delay(Duration::from_secs(1)));

		let mut open = producer.append_group().unwrap();
		open.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"key"))
			.unwrap();
		open.write_frame(
			Timestamp::from_millis(1900).unwrap(),
			bytes::Bytes::from_static(b"tail"),
		)
		.unwrap();

		let mut reading = subscriber.recv_group().await.unwrap().expect("the live group");
		assert!(reading.read_frame().await.unwrap().is_some());
		assert!(reading.read_frame().await.unwrap().is_some());

		let mut end = std::pin::pin!(reading.read_frame());
		assert!(futures::poll!(end.as_mut()).is_pending(), "parked at 1900ms");

		// The next GOP opens. The reader is 100ms behind it, inside its 1s budget.
		let mut next = producer.append_group().unwrap();
		next.write_frame(Timestamp::from_millis(2000).unwrap(), bytes::Bytes::from_static(b"key"))
			.unwrap();
		assert!(
			futures::poll!(end.as_mut()).is_pending(),
			"a reader inside its budget is not expired by the next group opening"
		);

		// A straggler from the old group, still within the budget.
		open.write_frame(
			Timestamp::from_millis(1950).unwrap(),
			bytes::Bytes::from_static(b"late"),
		)
		.unwrap();
		let late = end.await.expect("the straggler is not truncated");
		assert_eq!(
			late.map(|frame| frame.timestamp),
			Some(Some(Timestamp::from_millis(1950).unwrap()))
		);
	}

	/// A drained group the budget gives up on fails with [`Error::Old`] on every probe,
	/// including [`group::Consumer::finished`]: the producer never finished it.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn an_expired_group_stays_old_when_probed_again() {
		let mut producer = track_producer("test", Info::timed());
		let mut subscriber = producer.subscribe(None);
		let mut open = producer.append_group().unwrap();
		open.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"a"))
			.unwrap();

		let mut group = subscriber.recv_group().await.unwrap().expect("group");
		assert!(group.read_frame().await.unwrap().is_some());

		let probes = moq_clock::sim::spawn(async move {
			let first = group.read_frame().await;
			let second = group.read_frame().await;
			let finished = group.finished().await;
			(first, second, finished)
		});
		moq_clock::sim::yield_now().await;

		append_at(&mut producer, 1000);

		let (first, second, finished) = probes.await.unwrap();
		assert!(matches!(first, Err(Error::Old)), "the group fails: {first:?}");
		assert!(matches!(second, Err(Error::Old)), "and stays failed: {second:?}");
		assert!(matches!(finished, Err(Error::Old)), "never finished: {finished:?}");
	}

	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn a_drained_group_finishes_cleanly_after_the_live_edge_advances() {
		let mut producer = track_producer("test", Info::timed());
		let mut subscriber = producer.subscribe(None);
		append_at(&mut producer, 0);

		let mut group = subscriber.recv_group().await.unwrap().expect("first group");
		assert!(group.read_frame().await.unwrap().is_some());

		append_at(&mut producer, 1000);

		assert!(group.read_frame().await.unwrap().is_none());
	}

	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn a_handed_out_partial_frame_expires_while_its_payload_is_stalled() {
		let mut producer = track_producer("test", Info::timed());
		let mut subscriber = producer.subscribe(None);
		let source = producer.append_group().unwrap();
		let mut writing = source
			.create_frame(frame::Info {
				size: 6,
				timestamp: Some(Timestamp::ZERO),
			})
			.unwrap();
		writing.write(bytes::Bytes::from_static(b"old")).unwrap();

		let mut group = subscriber.recv_group().await.unwrap().expect("partial group");
		let mut frame = group.next_frame().await.unwrap().expect("partial frame");
		assert_eq!(
			frame.read_chunk().await.unwrap(),
			Some(bytes::Bytes::from_static(b"old"))
		);
		let pending = moq_clock::sim::spawn(async move { frame.read_chunk().await });
		moq_clock::sim::yield_now().await;
		assert!(!pending.is_finished(), "the partial payload is still stalled");

		elapse(&producer, Duration::from_secs(1));
		append_at(&mut producer, 1000);

		let result = pending.await.unwrap();
		assert!(
			matches!(result, Err(Error::Old)),
			"the in-flight frame expires: {result:?}"
		);
		writing.abort(Error::Cancel).unwrap();
	}

	#[test]
	#[cfg(feature = "async")]
	fn max_age_bounds_the_budget() {
		// The publisher only keeps a group around for 500ms, so a subscriber asking to
		// wait ten seconds for one still gives up at 500ms: the same clamp the aggregate
		// applies, on the subscriber's own side of it.
		let mut producer = track_producer("test", Info::timed().with_max_age(Duration::from_millis(500)));
		append_at(&mut producer, 0);
		append_at(&mut producer, 1000);
		append_at(&mut producer, 2000);

		// Group 0 reaches 1s, a full second behind the 2s edge, so the clamped 500ms
		// budget drops it. An unclamped ten seconds would have kept it.
		let mut subscriber = producer.subscribe(Subscription::default().with_max_delay(Duration::from_secs(10)));
		assert_eq!(drain(&mut subscriber), vec![1, 2]);
	}

	#[test]
	#[cfg(feature = "async")]
	fn an_explicit_start_gets_no_exemption_from_the_budget() {
		let mut producer = track_producer("test", Info::timed());
		for second in 0..4 {
			append_at(&mut producer, second * 1000);
		}

		// Asking to start at the beginning is a filter, not a request for reliability.
		// Backfill needs a budget that covers it; without one the live edge still wins.
		let mut subscriber = producer.subscribe(Subscription::default().with_start(0));
		subscriber.set_range(0..);
		assert_eq!(drain(&mut subscriber), vec![3]);

		let mut patient = producer.subscribe(
			Subscription::default()
				.with_start(0)
				.with_max_delay(Duration::from_secs(10)),
		);
		patient.set_range(0..);
		assert_eq!(drain(&mut patient), vec![0, 1, 2, 3]);
	}

	/// A group's age is where its content *ends*, not where it began. A long group whose
	/// tail is level with the live edge still owes the reader every frame in it, so
	/// judging it by its first timestamp would discard exactly the group being filled.
	#[test]
	#[cfg(feature = "async")]
	fn a_long_group_is_not_stale_while_its_tail_reaches_the_edge() {
		let mut producer = track_producer("test", Info::timed());

		// Group 0 spans 0..2000ms; group 1 starts at 2000ms, where group 0 ends.
		let mut long = producer.append_group().unwrap();
		for ms in [0u64, 500, 1000, 1500, 2000] {
			long.write_frame(Timestamp::from_millis(ms).unwrap(), bytes::Bytes::from_static(b"x"))
				.unwrap();
		}
		long.finish().unwrap();
		append_at(&mut producer, 2000);

		// A budget far shorter than the group's own span still keeps it.
		let mut sub = producer.subscribe(Subscription::default().with_max_delay(Duration::from_millis(500)));
		assert_eq!(drain(&mut sub), vec![0, 1]);
	}

	/// The same group, once its tail has fallen behind, is stale like any other.
	/// The same long group, once its *successor* has itself fallen behind. A frame's
	/// duration is not on the wire, so group 0's own last timestamp proves nothing about
	/// where it ends; only group 1's start bounds it. Group 0 is convicted once that bound
	/// is further behind the edge than the budget allows.
	#[test]
	#[cfg(feature = "async")]
	fn a_long_group_is_stale_once_its_successor_falls_behind() {
		let mut producer = track_producer("test", Info::timed());

		let mut long = producer.append_group().unwrap();
		for ms in [0u64, 500, 1000] {
			long.write_frame(Timestamp::from_millis(ms).unwrap(), bytes::Bytes::from_static(b"x"))
				.unwrap();
		}
		long.finish().unwrap();
		// Group 0 reaches at most 3s (where group 1 starts), a full second behind the 4s
		// edge and so past the budget. Group 1 reaches the edge itself and is kept.
		append_at(&mut producer, 3000);
		append_at(&mut producer, 4000);

		let mut sub = producer.subscribe(Subscription::default().with_max_delay(Duration::from_millis(500)));
		assert_eq!(drain(&mut sub), vec![1, 2]);
	}

	/// A frameless successor is not content, so the next started group bounds the group
	/// before it. That bound is sound: the frameless group's first frame is checked to start
	/// at or below the next group's, so the true reach can only be earlier.
	#[test]
	#[cfg(feature = "async")]
	fn a_frameless_successor_leaves_the_next_started_group_as_the_bound() {
		let mut producer = track_producer("test", Info::timed());
		// Any budget, so only media time judges: the wall clock stands still here.
		let mut subscriber = producer.subscribe(Subscription::default().with_max_delay(Duration::from_millis(500)));

		append_at(&mut producer, 0); // seq 0
		let _writer_1 = producer.append_group().unwrap(); // seq 1 stalls before its first frame
		append_at(&mut producer, 10_000); // seq 2
		append_at(&mut producer, 11_000); // seq 3

		// Groups 0 and 1 both reach at most group 2's start, a second behind the edge: stale.
		assert_eq!(drain(&mut subscriber), vec![2, 3]);
	}

	fn untimed_producer() -> Producer {
		track_producer("test", None)
	}

	/// A track that declares no timescale is untimed, whether created or accepted, so a
	/// timestamped write is refused rather than given an implicit scale.
	#[test]
	fn an_undeclared_timescale_is_untimed() {
		let broadcast = broadcast::Info::new().produce();
		let mut created = broadcast.create_track("created", None).unwrap();
		let mut accepted = broadcast.reserve_track("accepted").unwrap().accept(None);

		assert_eq!(Info::default().timescale, None);
		for track in [&mut created, &mut accepted] {
			assert!(matches!(
				track.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"x")),
				Err(Error::TimestampMismatch)
			));
			track.write_frame(None, bytes::Bytes::from_static(b"x")).unwrap();
		}
	}

	/// A group of untimed frames.
	#[cfg(feature = "async")]
	fn append_untimed(producer: &mut Producer) -> u64 {
		let mut group = producer.append_group().unwrap();
		group.write_frame(None, bytes::Bytes::from_static(b"x")).unwrap();
		group.finish().unwrap();
		group.sequence
	}

	/// The wall clock judges an untimed track, so a new subscriber starts at the oldest
	/// cached group it doesn't find stale, the way a timed one does: a group is stale once
	/// its successor arrived a full budget ago.
	#[test]
	#[cfg(feature = "async")]
	fn an_untimed_subscriber_starts_at_the_oldest_fresh_group() {
		let mut producer = untimed_producer();
		// Group N's successor arrives N + 1 seconds in.
		for _ in 0..5 {
			append_untimed(&mut producer);
			elapse(&producer, Duration::from_secs(1));
		}

		// Four seconds after group 4 arrived, groups 0 to 2 were superseded 4, 3, and 2
		// seconds ago: past a 1.5s budget. Group 3 was superseded 1s ago.
		let mut patient = producer.subscribe(Subscription::default().with_max_delay(Duration::from_millis(1500)));
		assert_eq!(drain(&mut patient), vec![3, 4]);
		// No budget takes the newest group alone, which is never stale.
		let mut live = producer.subscribe(None);
		assert_eq!(drain(&mut live), vec![4]);
		let mut replaying = producer.subscribe(replay());
		assert_eq!(drain(&mut replaying), vec![0, 1, 2, 3, 4]);

		append_untimed(&mut producer);
		assert_eq!(drain(&mut live), vec![5], "every later group is delivered");
	}

	/// An explicit start is a floor, not a request: the budget still skips what it finds
	/// stale above it, as on a timed track.
	#[test]
	#[cfg(feature = "async")]
	fn an_explicit_start_floors_an_untimed_track() {
		let mut producer = untimed_producer();
		for _ in 0..5 {
			append_untimed(&mut producer);
		}

		let mut subscriber = producer.subscribe(replay().with_start(2));
		assert_eq!(drain(&mut subscriber), vec![2, 3, 4]);
		let mut live = producer.subscribe(Subscription::default().with_start(2));
		assert_eq!(drain(&mut live), vec![4]);
	}

	/// An unfloored untimed subscription with an end starts at the latest group below it,
	/// as the lite publisher serves a SUBSCRIBE with an end and no start.
	#[test]
	#[cfg(feature = "async")]
	fn an_untimed_start_stays_under_the_end() {
		let mut producer = untimed_producer();
		for _ in 0..5 {
			append_untimed(&mut producer);
		}

		let mut subscriber = producer.subscribe(Subscription::default().with_end(3));
		subscriber.set_range(..3);
		assert_eq!(drain(&mut subscriber), vec![2]);
	}

	/// A refused single-frame write leaves no group behind: an empty open group would hold a
	/// subscriber waiting for a frame that never comes.
	#[test]
	fn a_mismatched_frame_appends_no_group() {
		let mut producer = track_producer("test", Info::timed());
		assert!(matches!(
			producer.write_frame(None, bytes::Bytes::from_static(b"x")),
			Err(Error::TimestampMismatch)
		));
		assert_eq!(producer.append_group().unwrap().sequence, 0);
	}

	/// A track is all timed or all untimed, so a datagram that doesn't match it is refused.
	#[test]
	fn a_mismatched_datagram_is_refused() {
		let mut untimed = track_producer("untimed", Info::default().with_timescale(None));
		assert!(matches!(
			untimed.append_datagram(Timestamp::ZERO, &b"x"[..]),
			Err(Error::TimestampMismatch)
		));
		untimed.append_datagram(None, &b"x"[..]).unwrap();

		let mut timed = track_producer("timed", Info::timed());
		assert!(matches!(
			timed.append_datagram(None, &b"x"[..]),
			Err(Error::TimestampMismatch)
		));
		assert!(matches!(
			timed.insert_datagram(5, None, &b"x"[..]),
			Err(Error::TimestampMismatch)
		));
	}

	/// Datagrams are unordered by construction, so the sequence cursor carries them too:
	/// a track using both channels needs one subscription, not two.
	#[test]
	#[cfg(feature = "async")]
	fn ordered_carries_datagrams() {
		let mut producer = track_producer("test", Info::timed());
		let mut sub = producer.subscribe(None).ordered();

		producer
			.insert_datagram(5, Timestamp::from_millis(5).unwrap(), bytes::Bytes::from_static(b"x"))
			.unwrap();
		producer
			.create_group(group::Info { sequence: 3 })
			.unwrap()
			.finish()
			.unwrap();

		let datagram = sub
			.recv_datagram()
			.now_or_never()
			.expect("datagram would have blocked")
			.expect("would have errored")
			.expect("track was closed");
		assert_eq!(datagram.sequence, 5);

		// The datagram did not consume the group cursor.
		let group = sub
			.next_group()
			.now_or_never()
			.expect("group would have blocked")
			.expect("would have errored")
			.expect("track was closed");
		assert_eq!(group.sequence, 3);
	}

	/// A subscriber read by either cursor, so one test covers both.
	#[cfg(feature = "async")]
	enum Reader {
		Arrival(Subscriber),
		Ordered(Ordered),
	}

	#[cfg(feature = "async")]
	impl Reader {
		fn new(subscriber: Subscriber, ordered: bool) -> Self {
			match ordered {
				true => Self::Ordered(subscriber.ordered()),
				false => Self::Arrival(subscriber),
			}
		}

		fn drain(&mut self) -> Vec<u64> {
			match self {
				Self::Arrival(subscriber) => drain(subscriber),
				Self::Ordered(ordered) => drain_ordered(ordered),
			}
		}

		fn resolved(&mut self) -> Vec<Option<Resolved>> {
			match self {
				Self::Arrival(subscriber) => subscriber.resolved_ranges(),
				Self::Ordered(ordered) => ordered.inner.resolved_ranges(),
			}
		}
	}

	/// Every group the ordered cursor can read right now, in sequence order.
	#[cfg(feature = "async")]
	fn drain_ordered(subscriber: &mut Ordered) -> Vec<u64> {
		let mut sequences = Vec::new();
		while let Some(Ok(Some(group))) = subscriber.next_group().now_or_never() {
			sequences.push(group.sequence);
		}
		sequences
	}

	/// A cached backlog is not free to deliver: a consumer that stalls and resumes would
	/// otherwise replay it at 1x and stay behind forever, since nothing it reads ever
	/// blocks. The budget applies as the cursor reads, exactly as on the arrival cursor.
	#[test]
	#[cfg(feature = "async")]
	fn next_group_sheds_a_stale_backlog() {
		let mut producer = track_producer("test", Info::timed());
		for second in 0..4 {
			append_at(&mut producer, second * 1000);
		}

		let mut subscriber = producer.subscribe(None).ordered();
		assert_eq!(drain_ordered(&mut subscriber), vec![3]);

		// The arrival cursor, which the relay forwarders use, sheds the same backlog.
		let mut arrival = producer.subscribe(None);
		assert_eq!(drain(&mut arrival), vec![3]);
	}

	/// Only what is provably too old goes. A group is convicted by its *reach* (where its
	/// successor begins), so a budget spanning part of the backlog keeps every group that
	/// could still present something inside it, and the burst is delivered gap-free.
	#[test]
	#[cfg(feature = "async")]
	fn next_group_keeps_a_backlog_inside_the_budget() {
		let mut producer = track_producer("test", Info::timed());
		for second in 0..4 {
			append_at(&mut producer, second * 1000);
		}

		// Group 0 reaches 1s, a full 2s behind the 3s edge, so it is gone. Group 1
		// reaches 2s and could still present up to it: inside a 1.5s budget.
		let mut subscriber = producer
			.subscribe(Subscription::default().with_max_delay(Duration::from_millis(1500)))
			.ordered();
		assert_eq!(drain_ordered(&mut subscriber), vec![1, 2, 3]);

		// A budget covering the whole history still bursts it in full.
		let mut replay = producer.subscribe(replay()).ordered();
		assert_eq!(drain_ordered(&mut replay), vec![0, 1, 2, 3]);
	}

	/// The ordered cursor judges a group past a frameless successor by the next started one.
	#[test]
	#[cfg(feature = "async")]
	fn next_group_judges_past_a_frameless_successor() {
		let mut producer = track_producer("test", Info::timed());
		append_at(&mut producer, 0); // seq 0
		let _writer_1 = producer.append_group().unwrap(); // seq 1 stalls before its first frame
		append_at(&mut producer, 10_000); // seq 2
		append_at(&mut producer, 11_000); // seq 3

		// Groups 0 and 1 reach at most 10s, a second behind the edge: convicted.
		let mut subscriber = producer
			.subscribe(Subscription::default().with_max_delay(Duration::from_millis(500)))
			.ordered();
		assert_eq!(drain_ordered(&mut subscriber), vec![2, 3]);
	}

	#[test]
	#[cfg(feature = "async")]
	fn real_time_skips_older_sequences_with_equal_ages() {
		let mut producer = track_producer("test", Info::timed());
		append_at(&mut producer, 0);
		append_at(&mut producer, 0);

		let mut subscriber = producer.subscribe(None);
		assert_eq!(drain(&mut subscriber), vec![1]);
	}

	#[test]
	fn fetch_omission_keeps_unbounded_history() {
		let mut producer = track_producer("test", Info::timed());
		for second in 0..4 {
			append_at(&mut producer, second * 1000);
		}

		// Omission adds no reader bound when the publisher declared none.
		let consumer = producer.consume();
		let group = consumer.fetch_group(0, None).now_or_never().unwrap().unwrap();
		assert_eq!(group.sequence, 0);
	}

	#[test]
	fn fetch_applies_zero_and_publisher_budgets() {
		let mut producer = track_producer("test", Info::timed().with_max_age(Duration::from_millis(1500)));
		for second in 0..4 {
			append_at(&mut producer, second * 1000);
		}

		let consumer = producer.consume();
		let zero = group::Fetch::default().with_max_delay(Duration::ZERO);
		assert!(matches!(
			consumer.fetch_group(2, zero).now_or_never().unwrap(),
			Err(Error::Old)
		));
		assert!(matches!(
			consumer.fetch_group(0, None).now_or_never().unwrap(),
			Err(Error::Old)
		));

		// A reader cannot relax max_age, but can tighten it independently.
		let loose = group::Fetch::default().with_max_delay(Duration::from_secs(60));
		assert_eq!(
			consumer.fetch_group(2, loose).now_or_never().unwrap().unwrap().sequence,
			2
		);
		let tight = group::Fetch::default().with_max_delay(Duration::from_millis(500));
		assert!(matches!(
			consumer.fetch_group(1, tight).now_or_never().unwrap(),
			Err(Error::Old)
		));
	}

	#[test]
	fn cold_fetch_expires_without_waiting_for_the_upstream_group() {
		let mut producer = track_producer("test", Info::timed());
		let _dynamic = producer.requests();
		let mut successor = producer.create_group(group::Info { sequence: 1 }).unwrap();
		successor
			.write_frame(Timestamp::from_millis(1000).unwrap(), bytes::Bytes::from_static(b"x"))
			.unwrap();
		successor.finish().unwrap();
		append_at(&mut producer, 2000);

		let options = group::Fetch::default().with_max_delay(Duration::ZERO);
		assert!(matches!(
			producer.consume().fetch_group(0, options).now_or_never().unwrap(),
			Err(Error::Old)
		));
	}

	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn fetched_group_keeps_its_budget_while_reading() {
		let mut producer = track_producer("test", Info::timed());
		let mut old = producer.append_group().unwrap();
		old.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"first"))
			.unwrap();

		let consumer = producer.consume();
		let options = group::Fetch::default().with_max_delay(Duration::from_millis(500));
		let mut fetched = consumer.fetch_group(0, options).await.unwrap();
		assert!(fetched.read_frame().await.unwrap().is_some());

		let pending = moq_clock::sim::spawn(async move { fetched.read_frame().await });
		moq_clock::sim::yield_now().await;
		elapse(&producer, Duration::from_secs(1));
		append_at(&mut producer, 1000);
		append_at(&mut producer, 2000);

		let result = pending.await.unwrap();
		assert!(
			matches!(result, Err(Error::Old)),
			"a drained but unfinished group fails: {result:?}"
		);
	}

	/// A start requested before the accept holds a reader of the start from the moment the
	/// track is visible, while its groups are read as they arrive, until the source
	/// resolves it.
	#[test]
	#[cfg(feature = "async")]
	fn a_requested_start_waits_for_the_source() {
		let waiter = kio::Waiter::noop();
		let request = Request::new(Arc::new(broadcast::Info::default()), "test");
		let consumer = request.consume();
		let mut producer = request.request_start(2).accept(None);
		let mut subscriber = consumer.subscribe(None).now_or_never().unwrap().unwrap();
		assert!(
			subscriber.poll_start(&waiter).is_pending(),
			"the floor is not the start"
		);

		// Nothing below the floor is waited for, and a group above it reads at once.
		producer
			.create_group(group::Info { sequence: 3 })
			.unwrap()
			.finish()
			.unwrap();
		assert_eq!(subscriber.assert_group().sequence, 3);
		assert!(subscriber.poll_start(&waiter).is_pending());

		producer.start_at(3).unwrap();
		assert_eq!(subscriber.poll_start(&waiter), Poll::Ready(Some(3)));
	}

	/// A copy accepted idle is never readable as live in between.
	#[test]
	fn an_idle_accept_is_not_live() {
		let request = Request::new(Arc::new(broadcast::Info::default()), "test");
		let consumer = request.consume();
		let mut producer = request.idle().accept(None);
		assert!(!consumer.is_live(), "idle from the accept");
		producer.set_live(None, None);
		assert!(consumer.is_live());
	}

	/// A fetched group installs metadata but never accepts the track's live feed.
	#[test]
	fn a_fetch_only_cache_is_not_live() {
		let waiter = kio::Waiter::noop();
		let request = Request::new(Arc::new(broadcast::Info::default()), "test");
		let dynamic = request.requests();
		let consumer = request.consume();
		let mut pending = consumer.fetch_group(2, None);
		let Poll::Ready(Ok(req)) = dynamic.poll_requested_fill_group(&waiter) else {
			panic!("fetch request should be ready");
		};
		let mut fetched = req.accept(None).unwrap();
		fetched.write_frame(None, b"2-0".as_ref()).unwrap();
		fetched.finish().unwrap();
		let Poll::Ready(Ok(mut group)) = kio::Task::poll(&mut *pending, &waiter) else {
			panic!("fetched group should be cached");
		};
		assert_eq!(group.sequence, 2);
		let Poll::Ready(Ok(Some(frame))) = group.poll_read_frame(&waiter) else {
			panic!("fetched frame should be cached");
		};
		assert_eq!(frame.payload.as_ref(), b"2-0");
		assert!(!consumer.is_live(), "a fetch-only cache is not live");

		let mut producer = request.accept(None);
		assert!(consumer.is_live(), "accepting the track starts its live feed");
		producer.set_idle();
		assert!(!consumer.is_live(), "an idle accepted copy is not live");
		producer.set_live(None, None);
		assert!(consumer.is_live(), "the answered copy is live again");

		// The fetch handler holds the same producer lifetime, so both leave.
		drop(dynamic);
		drop(producer);
		assert!(!consumer.is_live(), "a track whose producer left is not live");
	}

	/// A one-shot fetch populates the shared cache but never the live arrival cursor,
	/// so it cannot make content available to a subscription look stale.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn fetched_group_is_not_a_live_drift_edge() {
		let mut producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		let consumer = producer.consume();
		append_at(&mut producer, 0);

		let pending = consumer.fetch_group(100, None);
		let req = dynamic
			.requested_fill_group()
			.now_or_never()
			.expect("fetch request is ready")
			.unwrap();
		let mut fetched = req.accept(Info::timed()).unwrap();
		fetched
			.write_frame(
				Timestamp::from_millis(100_000).unwrap(),
				bytes::Bytes::from_static(b"fetched"),
			)
			.unwrap();
		fetched.finish().unwrap();
		pending.await.unwrap();

		let mut groups = producer.subscribe(None);
		assert_eq!(groups.assert_group().sequence, 0);
		groups.assert_no_group();
	}

	/// The live edge is resolved once per poll and applied to every group that poll
	/// walks off, so it has to be revalidated before it convicts anything: an eviction
	/// in between would otherwise discard a group on the strength of content that is no
	/// longer there to jump to.
	#[test]
	fn an_evicted_live_edge_convicts_nothing() {
		let mut producer = track_producer("test", Info::timed());
		append_at(&mut producer, 0);
		let edge = append_at(&mut producer, 30_000);

		let state = producer.state.read();
		let drift = Drift {
			budget: Duration::ZERO,
			bounded: false,
			edge: state.drift_edge(&Servable::ALL),
		};
		assert!(
			state.is_stale(0, &drift.edge, drift.budget),
			"stale against a live edge"
		);
		drop(state);

		// The edge dies between resolving it and judging the candidate.
		let slot = producer.modify().unwrap().lookup.remove(&edge).unwrap();
		let _ = slot.group.abort(Error::Evicted);

		let state = producer.state.read();
		assert!(
			!state.is_stale(0, &drift.edge, drift.budget),
			"a vanished edge is no reason to drop what is left"
		);
	}

	#[test]
	#[cfg(feature = "async")]
	fn a_lower_sequence_is_never_the_live_edge() {
		let producer = track_producer("test", Info::timed());
		// A high timestamp on a lower sequence is not a live edge: a group whose frames run
		// past where its successor starts sits there. Only groups above the candidate
		// anchor the measure, so it cannot convict the group that follows it.
		let mut straggler = producer.create_group(0u64.into()).unwrap();
		for millis in [0, 60_000] {
			straggler
				.write_frame(Timestamp::from_millis(millis).unwrap(), bytes::Bytes::from_static(b"x"))
				.unwrap();
		}
		straggler.finish().unwrap();

		let mut next = producer.create_group(1u64.into()).unwrap();
		next.write_frame(Timestamp::from_millis(0).unwrap(), bytes::Bytes::from_static(b"x"))
			.unwrap();
		next.finish().unwrap();

		let mut subscriber = producer.subscribe(replay());
		assert_eq!(drain(&mut subscriber), vec![0, 1]);
	}

	#[test]
	#[cfg(feature = "async")]
	fn a_range_behind_the_live_edge_keeps_its_newest_group() {
		let mut producer = track_producer("test", Info::timed());
		for second in 0..=10 {
			append_at(&mut producer, second * 1000);
		}

		// Drift is measured under where the range ends, so with no budget its own newest
		// group is still fresh: the groups past it are not ones it reads.
		let mut subscriber = producer.subscribe(Subscription::default().with_range(0..2));
		assert_eq!(drain(&mut subscriber), vec![1]);
		let mut subscriber = producer.subscribe(Subscription::default().with_range(0..2)).ordered();
		let next = subscriber.next_group().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(next.sequence, 1);

		// A budget that covers it delivers the range, and nothing above it.
		let mut subscriber = producer.subscribe(replay().with_end(2));
		assert_eq!(drain(&mut subscriber), vec![0, 1]);
	}

	/// Drift is measured only against groups the ranges cover: a group in the gap between
	/// two spans is never read, so it neither sets the edge nor replaces the newest group
	/// below it. A group in the later span is read, so it does.
	#[test]
	#[cfg(feature = "async")]
	fn a_gap_between_ranges_is_not_a_drift_edge() {
		let ranges = || Subscription::default().with_ranges([Range::groups(0..2), Range::groups(10..12)]);
		let producer = track_producer("test", Info::timed());
		for (sequence, millis) in [(0, 0), (1, 1000), (9, 9000)] {
			let mut group = producer.create_group(group::Info { sequence }).unwrap();
			group
				.write_frame(Timestamp::from_millis(millis).unwrap(), bytes::Bytes::from_static(b"x"))
				.unwrap();
			group.finish().unwrap();
		}
		// With no budget, the newest group the ranges cover is the edge.
		assert_eq!(drain(&mut producer.subscribe(ranges())), vec![1]);

		let mut ten = producer.create_group(group::Info { sequence: 10 }).unwrap();
		ten.write_frame(Timestamp::from_millis(10_000).unwrap(), bytes::Bytes::from_static(b"x"))
			.unwrap();
		ten.finish().unwrap();
		assert_eq!(drain(&mut producer.subscribe(ranges())), vec![10]);
	}

	/// A group handed out before the ranges moved above it is judged against the new run,
	/// not a cached group below its start: here group 1 would put group 0 18s behind the
	/// edge, past the budget, where the run's own group 10 puts it 9s behind.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn a_held_group_is_judged_against_the_moved_run() {
		let at = |millis| Timestamp::from_millis(millis).unwrap();
		let budget = Subscription::default().with_max_delay(Duration::from_secs(15));
		let producer = track_producer("test", Info::timed());
		let mut subscriber = producer.subscribe(budget.clone());
		let mut zero = producer.create_group(group::Info { sequence: 0 }).unwrap();
		zero.write_frame(at(0), bytes::Bytes::from_static(b"x")).unwrap();
		let mut held = subscriber.recv_group().await.unwrap().expect("group 0");
		assert!(held.read_frame().await.unwrap().is_some());

		subscriber.update(budget.with_range(10..20)).unwrap();
		for (sequence, millis) in [(1, 1000), (10, 10_000), (19, 19_000)] {
			let mut group = producer.create_group(group::Info { sequence }).unwrap();
			group.write_frame(at(millis), bytes::Bytes::from_static(b"x")).unwrap();
			group.finish().unwrap();
		}

		let pending = moq_clock::sim::spawn(async move { held.read_frame().await });
		moq_clock::sim::yield_now().await;
		assert!(!pending.is_finished(), "group 0 is within the budget of its run");
	}

	/// A drift snapshot taken while the window waits on its first group is taken again once
	/// that group settles it. Groups 0 and 1 land between the snapshot and the arrival: the
	/// settled `0..=0` judges group 0 against itself, not group 1, which it never reads.
	#[test]
	fn the_first_arrival_retakes_a_pending_drift_snapshot() {
		for ordered in [false, true] {
			let mut producer = track_producer("test", Info::timed());
			let subscription = Subscription::default()
				.with_max_delay(Duration::ZERO)
				.with_ranges([Range {
					start: Anchor::Absolute(0),
					end: Some(Anchor::Relative(0)),
				}]);
			let mut subscriber = producer.subscribe(subscription);
			let Inner::Plain(cursor) = &mut subscriber.inner else {
				unreachable!("a local subscriber reads its own cursor");
			};
			let waiter = kio::Waiter::noop();
			cursor.sync();
			assert!(cursor.filter.window.is_pending());
			let Poll::Ready(Ok(drift)) = cursor.poll_drift(&cursor.servable, &waiter) else {
				unreachable!("a drift snapshot is always ready");
			};

			append_at(&mut producer, 0);
			append_at(&mut producer, 1000);
			let res = match ordered {
				true => cursor.poll_next_with(true, 0, drift, &waiter),
				false => cursor.poll_recv_with(true, drift, &waiter),
			};
			let Poll::Ready(Ok(Some(group))) = res else {
				panic!("group 0 is the newest group its range reads (ordered: {ordered})");
			};
			assert_eq!(group.sequence, 0);
		}
	}

	/// A [`Control`] replacing the ranges and the cursor moving its cap each change only
	/// their own part of what handed-out groups are judged against, whichever goes first,
	/// and with the cap moving between the ranges being written and republished.
	#[test]
	fn a_control_update_and_a_cap_never_publish_a_mismatched_pair() {
		let published = |subscriber: &Subscriber| {
			let Inner::Plain(cursor) = &subscriber.inner else {
				unreachable!("a local subscriber reads its own cursor");
			};
			let current = cursor.published.read();
			(current.servable.clone(), current.cap)
		};
		let ranges = Subscription::default().with_range(10..20);
		// Nothing in 10..20 sits below a cap at 5.
		let expected = (Servable::NONE, Some(5));
		for order in ["ranges first", "cap first", "cap between"] {
			let producer = track_producer("test", Info::timed());
			let mut subscriber = producer.subscribe(Subscription::default());
			let control = subscriber.control();
			match order {
				"ranges first" => {
					control.update(ranges.clone()).unwrap();
					subscriber.set_range(..5);
				}
				"cap first" => {
					subscriber.set_range(..5);
					control.update(ranges.clone()).unwrap();
				}
				_ => {
					control.subscription.update(ranges.clone()).unwrap();
					subscriber.set_range(..5);
					control.republish.as_ref().unwrap().publish(&control.subscription);
				}
			}
			assert!(published(&subscriber) == expected, "{order}");
			let _ = subscriber.poll_recv_group(&kio::Waiter::noop());
			assert!(published(&subscriber) == expected, "{order}, once the cursor syncs");
		}
	}

	/// Capping a servable window in place keeps exactly the groups the set of its runs
	/// keeps below the cap, so the single run stays off the heap without moving a bound.
	#[test]
	fn servable_below_matches_its_runs_below_the_cap() {
		// Inclusive (first, last) bounds, lowest first.
		let windows: [&[(u64, u64)]; 5] = [
			&[],
			&[(0, u64::MAX)],
			&[(5, 9)],
			&[(5, u64::MAX)],
			&[(2, 3), (6, 8), (12, u64::MAX)],
		];
		let caps = [
			None,
			Some(0),
			Some(2),
			Some(3),
			Some(5),
			Some(7),
			Some(9),
			Some(10),
			Some(13),
			Some(u64::MAX),
		];
		for bounds in windows {
			let runs = || bounds.iter().map(|&(first, last)| first..=last);
			let servable = Servable::new(runs());
			if let [(first, last)] = bounds {
				assert!(
					servable
						== Servable::Run {
							start: *first,
							end: last.checked_add(1)
						},
					"{bounds:?}"
				);
			}
			for cap in caps {
				let expected = Servable::new(below(runs(), cap).into_iter());
				assert!(servable.below(cap) == expected, "{bounds:?} below {cap:?}");
			}
		}
	}

	/// Ranges replaced through a [`Control`] republish what a held group is judged against
	/// at once, before the cursor next reads: group 0 is judged against the run's own
	/// group 10, not group 1, which the new ranges never read.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn a_control_update_moves_the_run_a_held_group_is_judged_against() {
		let at = |millis| Timestamp::from_millis(millis).unwrap();
		let budget = Subscription::default().with_max_delay(Duration::from_secs(15));
		let producer = track_producer("test", Info::timed());
		let mut subscriber = producer.subscribe(budget.clone());
		let mut zero = producer.create_group(group::Info { sequence: 0 }).unwrap();
		zero.write_frame(at(0), bytes::Bytes::from_static(b"x")).unwrap();
		let mut held = subscriber.recv_group().await.unwrap().expect("group 0");
		assert!(held.read_frame().await.unwrap().is_some());

		subscriber.control().update(budget.with_range(10..20)).unwrap();
		for (sequence, millis) in [(1, 1000), (10, 10_000), (19, 19_000)] {
			let mut group = producer.create_group(group::Info { sequence }).unwrap();
			group.write_frame(at(millis), bytes::Bytes::from_static(b"x")).unwrap();
			group.finish().unwrap();
		}

		let pending = moq_clock::sim::spawn(async move { held.read_frame().await });
		moq_clock::sim::yield_now().await;
		assert!(!pending.is_finished(), "group 0 is within the budget of its run");
		drop(subscriber);
	}

	#[test]
	#[cfg(feature = "async")]
	fn a_capped_subscriber_measures_drift_against_its_cap() {
		let mut producer = track_producer("test", Info::timed());
		append_at(&mut producer, 0);
		append_at(&mut producer, 1000);

		// Capped at group 0: the route running on past the cap is data this subscriber
		// can never be served, so it isn't a live edge to be late against.
		let mut subscriber = producer.subscribe(Subscription::default());
		subscriber.set_range(..1);
		assert_eq!(drain(&mut subscriber), vec![0]);

		// Raising the cap re-offers the parked group, now measured against the wider
		// edge it just admitted.
		subscriber.set_range(..);
		assert_eq!(drain(&mut subscriber), vec![1]);
	}

	#[test]
	#[cfg(feature = "async")]
	fn subscriber_control_updates_while_read_future_is_pending() {
		let producer = track_producer("test", Info::timed());
		let mut subscriber = producer.subscribe(None);
		let control = subscriber.control();

		let mut recv = Box::pin(subscriber.recv_group());
		assert!(recv.as_mut().now_or_never().is_none());

		control.update(Subscription::default().with_priority(7)).unwrap();

		let aggregate = producer.subscription().expect("expected an active subscription");
		assert_eq!(aggregate.priority, 7);
	}

	#[test]
	fn dropped_subscriber_leaves_no_ghost_in_aggregate() {
		// Regression (#2351): a departed subscriber must not keep contributing its
		// last subscription to the aggregate. When it did, a relay's linger loop
		// never observed the track going idle, and an identical viewer reconnecting
		// within the linger window was reset when the stale timer fired.
		let mut producer = track_producer("test", Info::timed());
		let a = producer.subscribe(Subscription::default().with_priority(5));

		// Prime the change cursor: the aggregate currently has one subscriber.
		let waiter = kio::Waiter::noop();
		assert!(
			matches!(producer.poll_subscription_changed(&waiter), Poll::Ready(Ok(Some(_)))),
			"one live subscriber should aggregate to Some",
		);

		// The only subscriber leaves.
		drop(a);

		// The aggregate must report the drop to None, not the ghost's last value.
		assert!(
			matches!(producer.poll_subscription_changed(&waiter), Poll::Ready(Ok(None))),
			"a dropped subscriber must not linger in the aggregate",
		);

		// And the snapshot used by the linger loop must agree.
		assert!(
			producer.subscription().is_none(),
			"snapshot must exclude a dropped subscriber",
		);
	}

	#[test]
	fn dropped_subscriber_wakes_the_aggregate() {
		// The value being right isn't enough: nothing re-polls the aggregate on its
		// own, so the drop has to wake the waiter. A subscriber contributing demand
		// takes `kio::Consumer::poll`'s Ready path, which registers no waiter, so
		// the departure needs the closed waiter armed explicitly. Without it a relay
		// never learns the last viewer left and holds the upstream subscription (and
		// the upstream's viewer count) open forever.
		use std::sync::atomic::{AtomicBool, Ordering};

		let mut producer = track_producer("test", Info::timed());
		let a = producer.subscribe(Subscription::default().with_priority(5));

		let woken = Arc::new(AtomicBool::new(false));
		let waiter = kio::Waiter::new(futures::task::waker(Arc::new(FlagWake(woken.clone()))));

		// Prime the cursor, then confirm the next poll parks.
		assert!(matches!(
			producer.poll_subscription_changed(&waiter),
			Poll::Ready(Ok(Some(_)))
		));
		assert!(
			producer.poll_subscription_changed(&waiter).is_pending(),
			"the aggregate is unchanged, so this poll must park",
		);
		assert!(!woken.load(Ordering::SeqCst), "nothing happened yet");

		drop(a);
		assert!(
			woken.load(Ordering::SeqCst),
			"the last subscriber leaving must wake the aggregate watcher",
		);
	}

	#[test]
	fn widest_subscriber_update_wakes_the_aggregate() {
		// The value counterpart of the drop above. A subscriber that widens the
		// fold takes the Ready path too, so its next update registered no waiter.
		// A relay whose upstream cap came from a downstream reader then never
		// learned that the reader lifted it, and the groups parked upstream never
		// resumed: every session stayed up and nothing flowed.
		use std::sync::atomic::{AtomicBool, Ordering};

		let mut producer = track_producer("test", Info::timed());
		let _narrow = producer.subscribe(Subscription::default().with_end(4));
		let mut wide = producer.subscribe(Subscription::default());

		let woken = Arc::new(AtomicBool::new(false));
		let waiter = kio::Waiter::new(futures::task::waker(Arc::new(FlagWake(woken.clone()))));

		assert!(matches!(
			producer.poll_subscription_changed(&waiter),
			Poll::Ready(Ok(Some(_)))
		));
		assert!(producer.poll_subscription_changed(&waiter).is_pending());
		assert!(!woken.load(Ordering::SeqCst), "nothing happened yet");

		wide.update(Subscription::default().with_end(6)).unwrap();
		assert!(
			woken.load(Ordering::SeqCst),
			"the widest subscriber changing must wake the aggregate watcher",
		);
		match producer.poll_subscription_changed(&waiter) {
			Poll::Ready(Ok(Some(sub))) => assert_eq!(sub.end(), Some(6)),
			other => panic!("expected the narrowed aggregate, got {other:?}"),
		}
	}

	/// An [`ArcWake`] that just records that it was woken.
	struct FlagWake(Arc<std::sync::atomic::AtomicBool>);

	impl futures::task::ArcWake for FlagWake {
		fn wake_by_ref(arc_self: &Arc<Self>) {
			arc_self.0.store(true, std::sync::atomic::Ordering::SeqCst);
		}
	}

	#[test]
	fn out_of_order_max_sequence_at_front() {
		let producer = track_producer("test", Info::timed());

		// Arrive out of order: seq 5 first, then 3, then 4.
		let _open = producer.create_group(group::Info { sequence: 5 }).unwrap();
		let _open = producer.create_group(group::Info { sequence: 3 }).unwrap();
		let _open = producer.create_group(group::Info { sequence: 4 }).unwrap();

		// max_sequence = 5, which is at the front of the VecDeque.
		{
			let state = producer.state.read();
			assert_eq!(state.max_sequence, Some(5));
		}

		// Expire all three groups.
		elapse(&producer, cache::DEFAULT_EXPIRY + Duration::from_secs(1));

		// Append seq 6 (becomes new max_sequence).
		let _open = producer.append_group().unwrap(); // seq 6

		// Seq 3, 4, 5 are all expired. Seq 5 was the old max_sequence but now 6 is.
		// All old groups are evicted.
		{
			let state = producer.state.read();
			assert_eq!(live_groups(&state), 1);
			assert_eq!(first_live_sequence(&state), 6);
			assert!(!state.lookup.contains_key(&3));
			assert!(!state.lookup.contains_key(&4));
			assert!(!state.lookup.contains_key(&5));
			assert!(state.lookup.contains_key(&6));
		}
	}

	#[test]
	fn max_sequence_at_front_blocks_trim() {
		let producer = track_producer("test", Info::timed());

		// Arrive: seq 5, then seq 3.
		producer
			.create_group(group::Info { sequence: 5 })
			.unwrap()
			.finish()
			.unwrap();

		elapse(&producer, cache::DEFAULT_EXPIRY + Duration::from_secs(1));

		// Seq 3 arrives late; max_sequence is still 5 (at front).
		producer
			.create_group(group::Info { sequence: 3 })
			.unwrap()
			.finish()
			.unwrap();

		// Seq 5 is max_sequence (protected). Seq 3 is not expired (just created).
		// Nothing should be evicted.
		{
			let state = producer.state.read();
			assert_eq!(live_groups(&state), 2);
			assert_eq!(state.offset, 0);
		}

		// Expire seq 3 as well.
		elapse(&producer, cache::DEFAULT_EXPIRY + Duration::from_secs(1));

		// Seq 2 arrives late, triggering eviction.
		producer
			.create_group(group::Info { sequence: 2 })
			.unwrap()
			.finish()
			.unwrap();

		// Seq 5 is the live edge (protected) and still resolves at the front of
		// `arrival`, so nothing is trimmed and the offset stays. Seq 3 expired out of
		// `lookup`, leaving a hole its arrival entry no longer resolves; seq 2 is
		// fresh and kept.
		{
			let state = producer.state.read();
			assert_eq!(live_groups(&state), 2);
			assert_eq!(state.offset, 0);
			assert!(state.lookup.contains_key(&5));
			assert!(!state.lookup.contains_key(&3));
			assert!(state.lookup.contains_key(&2));
		}

		// Consumer should still be able to read through the hole.
		let mut consumer = producer.subscribe(None);
		let group = consumer.assert_group();
		// consume() starts at index 0; the first arrival entry that still resolves is seq 5.
		assert_eq!(group.sequence, 5);
	}

	#[test]
	#[cfg(feature = "async")]
	fn abort_drops_open_groups() {
		let producer = track_producer("test", Info::timed());
		let _writer_1 = producer.append_group().unwrap();
		let _writer_2 = producer.append_group().unwrap();

		let mut consumer = producer.subscribe(None);
		assert_eq!(live_groups(&producer.state.read()), 2);

		producer.clone().abort(Error::Cancel).unwrap();

		// Nobody will finish them, so they leave the cache rather than park a reader.
		assert!(
			producer.state.read().lookup.is_empty(),
			"open groups are dropped on abort"
		);
		let result = consumer.recv_group().now_or_never().expect("should not block");
		assert!(matches!(result, Err(Error::Cancel)));
	}

	#[test]
	#[cfg(feature = "async")]
	fn drop_unfinished_clears_cached_groups() {
		let producer = track_producer("test", Info::timed());
		let writer = producer.clone();
		let _open = writer.append_group().unwrap();

		// A stale consumer keeps the channel (and thus the cache) alive.
		let mut consumer = producer.subscribe(None);
		assert_eq!(live_groups(&producer.state.read()), 1);

		// Drop every producer without finishing: the cache is released.
		drop(writer);
		drop(producer);

		let result = consumer.recv_group().now_or_never().expect("should not block");
		assert!(matches!(result, Err(Error::Dropped)));
	}

	/// Aborting a track aborts its open groups: a reader that already pulled one ends with
	/// the error, and the group's writer fails.
	#[test]
	#[cfg(feature = "async")]
	fn abort_ends_a_pulled_open_group() {
		let producer = track_producer("test", Info::timed());
		let mut consumer = producer.subscribe(None);
		let mut writer = producer.append_group().unwrap();
		writer
			.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"x"))
			.unwrap();
		let mut group = consumer.assert_group();
		assert!(group.read_frame().now_or_never().unwrap().unwrap().is_some());

		producer.clone().abort(Error::Cancel).unwrap();
		let read = group.read_frame().now_or_never().expect("should not block");
		assert!(matches!(read, Err(Error::Cancel)), "{read:?}");
		let write = writer.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"y"));
		assert!(matches!(write, Err(Error::Cancel)), "{write:?}");
	}

	/// A reader still in an open group keeps the track used after its subscription leaves,
	/// so only its leaving lets an unused abort end the group's writer.
	#[test]
	#[cfg(feature = "async")]
	fn abort_unwanted_waits_for_a_pulled_open_group() {
		let producer = track_producer("test", Info::timed());
		let mut consumer = producer.subscribe(None);
		let mut writer = producer.append_group().unwrap();
		writer
			.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"x"))
			.unwrap();
		let mut group = consumer.assert_group();
		assert!(group.read_frame().now_or_never().unwrap().unwrap().is_some());
		drop(consumer);

		let producer = producer
			.abort_unwanted(Error::Cancel)
			.expect_err("the group's reader keeps the track");
		drop(group);
		assert!(producer.abort_unwanted(Error::Cancel).is_ok());
		let write = writer.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"y"));
		assert!(matches!(write, Err(Error::Cancel)), "{write:?}");
	}

	/// Dropping the last producer without a finish aborts its open groups with
	/// [`Error::Dropped`].
	#[test]
	#[cfg(feature = "async")]
	fn drop_unfinished_ends_a_pulled_open_group() {
		let producer = track_producer("test", Info::timed());
		let mut consumer = producer.subscribe(None);
		let mut writer = producer.append_group().unwrap();
		writer
			.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"x"))
			.unwrap();
		let mut group = consumer.assert_group();
		assert!(group.read_frame().now_or_never().unwrap().unwrap().is_some());

		drop(producer);
		let read = group.read_frame().now_or_never().expect("should not block");
		assert!(matches!(read, Err(Error::Dropped)), "{read:?}");
		let write = writer.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"y"));
		assert!(matches!(write, Err(Error::Dropped)), "{write:?}");
	}

	/// Aborting a track also aborts a fetched head still being written, so its reader ends
	/// with the track's error rather than waiting on a fetch that can no longer land.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn abort_ends_an_open_fetched_head() {
		let producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		let consumer = producer.consume();
		let mut live = producer.create_group(group::Info { sequence: 0 }).unwrap();
		live.start_at(1).unwrap();
		live.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"delta"))
			.unwrap();

		let _fetch = consumer.fetch_group(0, None);
		let request = dynamic.requested_fill_group().await.unwrap();
		let mut head = request.accept(Info::timed()).unwrap();
		head.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"snapshot"))
			.unwrap();
		let mut reading = consumer.peek_group(0).expect("the head serves group 0");
		assert!(reading.read_frame().now_or_never().unwrap().unwrap().is_some());

		producer.abort(Error::App(7)).unwrap();
		let read = reading.read_frame().now_or_never().expect("the abort settles the head");
		assert!(matches!(read, Err(Error::App(7))), "{read:?}");
		let write = head.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"delta"));
		assert!(matches!(write, Err(Error::App(7))), "{write:?}");
	}

	#[test]
	fn drop_after_abort_does_not_warn() {
		// abort() closes the channel after recording `abort`. Drop must treat the
		// read-only guard returned by write() as clean or it emits a false WARN.
		let warns = count_drop_warnings("track::Producer dropped without finish", || {
			let producer = track_producer("test", Info::timed());
			let keep = producer.clone();
			let writer = producer.clone();
			let mut group = writer.append_group().unwrap();
			group.finish().unwrap();
			let _consumer = producer.subscribe(None);
			writer.abort(Error::Cancel).unwrap();
			drop(keep);
		});
		assert_eq!(warns, 0, "abort-then-drop must not emit unfinished-producer WARN");
	}

	#[test]
	fn drop_unfinished_warns() {
		let warns = count_drop_warnings("track::Producer dropped without finish", || {
			let producer = track_producer("test", Info::timed());
			let writer = producer.clone();
			let _open = writer.append_group().unwrap();
			let _consumer = producer.subscribe(None);
			drop(writer);
			drop(producer);
		});
		assert_eq!(warns, 1, "unfinished drop must emit one unfinished-producer WARN");
	}

	#[test]
	#[cfg(feature = "async")]
	fn drop_finished_keeps_cached_groups() {
		let producer = track_producer("test", Info::timed());
		producer.append_group().unwrap().finish().unwrap();
		producer.finish().unwrap();

		let mut consumer = producer.subscribe(None);
		drop(producer);

		// A cleanly finished track keeps its cache so the consumer can still drain.
		assert_eq!(consumer.assert_group().sequence, 0);
		let done = consumer.recv_group().now_or_never().expect("should not block").unwrap();
		assert!(done.is_none(), "consumer should drain then see clean finish");
	}

	/// A boundary declared ahead of the live edge, then the last producer dropping, means
	/// the missing groups will never come: the reader ends cleanly rather than with
	/// `Dropped`, as it would for a track that never declared its end.
	#[test]
	#[cfg(feature = "async")]
	fn drop_short_of_the_boundary_ends_cleanly() {
		let mut producer = track_producer("test", Info::timed());
		producer.append_group().unwrap().finish().unwrap();
		producer.finish_at(Position::group(3)).unwrap();

		let mut consumer = producer.subscribe(None);
		assert_eq!(consumer.assert_group().sequence, 0);
		assert!(
			consumer.recv_group().now_or_never().is_none(),
			"groups 1 and 2 are owed"
		);

		drop(producer);
		let done = consumer.recv_group().now_or_never().expect("should not block").unwrap();
		assert!(
			done.is_none(),
			"the track ends at its boundary without the missing groups"
		);
	}

	/// The sequence cursor ends the same way. A gap below the boundary is skipped,
	/// a later cached group is still delivered, and the read finishes cleanly.
	#[test]
	#[cfg(feature = "async")]
	fn ordered_ends_cleanly_when_sealed_short_of_the_boundary() {
		let mut producer = track_producer("test", Info::timed());
		producer
			.create_group(group::Info { sequence: 0 })
			.unwrap()
			.finish()
			.unwrap();
		producer
			.create_group(group::Info { sequence: 2 })
			.unwrap()
			.finish()
			.unwrap();
		producer.finish_at(Position::group(4)).unwrap();

		let mut ordered = producer.subscribe(replay()).ordered();
		drop(producer);

		let first = ordered.next_group().now_or_never().expect("group 0").unwrap().unwrap();
		assert_eq!(first.sequence, 0);
		let second = ordered.next_group().now_or_never().expect("group 2").unwrap().unwrap();
		assert_eq!(second.sequence, 2);
		let done = ordered.next_group().now_or_never().expect("end").unwrap();
		assert!(done.is_none(), "missing groups below the boundary are not an error");
	}

	/// A finished track ends a sequence reader with nothing left in its range, as it ends
	/// an arrival reader: whether the range ends at the cursor or the rest was given up.
	#[test]
	fn ordered_ends_once_a_finished_track_leaves_nothing_in_range() {
		for given_up in [false, true] {
			let producer = track_producer("test", Info::timed());
			let end = if given_up { 4 } else { 3 };
			let mut ordered = producer.subscribe(replay().with_range(0..end)).ordered();
			if given_up {
				producer.drop_groups(3..4, StreamError::DeliveryTimeout);
			}
			for sequence in [0, 1, 2, 4, 5] {
				write_group(&producer, sequence);
			}
			producer.finish().unwrap();
			for sequence in 0..3 {
				let Poll::Ready(Ok(Some(group))) = ordered.poll_next_group(&kio::Waiter::noop()) else {
					panic!("group {sequence}, given_up={given_up}")
				};
				assert_eq!(group.sequence, sequence);
			}
			assert!(
				matches!(ordered.poll_next_group(&kio::Waiter::noop()), Poll::Ready(Ok(None))),
				"given_up={given_up}"
			);
		}
	}

	#[test]
	fn append_finish_cannot_be_rewritten() {
		let producer = track_producer("test", Info::timed());

		// Finishing an empty track is valid (fin = 0, total groups = 0).
		assert!(producer.finish().is_ok());
		assert!(producer.finish().is_err());
		assert!(producer.append_group().is_err());
	}

	#[test]
	fn finish_after_groups() {
		let producer = track_producer("test", Info::timed());

		let _open = producer.append_group().unwrap();
		assert!(producer.finish().is_ok());
		assert!(producer.finish().is_err());
		assert!(producer.append_group().is_err());
	}

	#[test]
	fn finish_at_rejects_a_boundary_at_or_below_the_live_edge() {
		let mut producer = track_producer("test", Info::timed());
		let _open = producer.create_group(group::Info { sequence: 5 }).unwrap();

		// The boundary is exclusive, so it must be strictly above the highest produced
		// group. 5 or below would orphan groups that already exist.
		assert!(producer.finish_at(Position::group(4)).is_err());
		assert!(producer.finish_at(Position::group(5)).is_err());
		assert!(producer.finish_at(Position::group(6)).is_ok());

		assert_eq!(producer.finished(), Some(Position::group(6)));

		// Repeating the end is a no-op, moving it is rejected, and no group at or above the
		// boundary can be created.
		assert!(producer.finish_at(Position::group(6)).is_ok());
		assert!(producer.finish_at(Position::group(7)).is_err());
		assert!(producer.create_group(group::Info { sequence: 4 }).is_ok());
		assert!(producer.create_group(group::Info { sequence: 6 }).is_err());
	}

	#[test]
	fn a_mid_group_end_survives_eviction_and_filtered_readers() {
		let pool = cache::Pool::new(cache::Config::default().with_expiry(Duration::from_secs(1)));
		let mut producer = track_producer_pooled("test", pool.clone());
		let end = Position { group: 5, frame: 5 };
		producer.finish_at(end).unwrap();
		let mut group = producer.create_group(group::Info { sequence: 5 }).unwrap();
		for _ in 0..5 {
			group.write_frame(Timestamp::ZERO, b"x".as_slice()).unwrap();
		}
		group.finish().unwrap();
		let consumer = producer.consume();
		drop(producer);
		// Date the last activity before advancing the cache clock past expiry.
		pool.sweep();
		pool.step(Duration::from_secs(2));
		pool.sweep();
		assert!(consumer.peek_group(5).is_none(), "payload cache expired");
		let mut sub = consumer.subscribe(replay()).now_or_never().unwrap().unwrap();
		sub.set_range(..5);
		assert_eq!(
			sub.poll_finished(&kio::Waiter::noop()).map(|result| result.unwrap()),
			Poll::Ready(end)
		);
	}

	#[test]
	fn a_wire_end_repeats_or_refines_but_never_moves() {
		let mut producer = track_producer("test", Info::timed());
		// A FETCH_OK names only the boundary; END_OF_TRACK may then name the frame before it.
		producer.finish_at(Position::group(6)).unwrap();
		producer.finish_at(Position { group: 5, frame: 5 }).unwrap();
		producer.finish_at(Position { group: 5, frame: 5 }).unwrap();
		assert_eq!(producer.finished(), Some(Position { group: 5, frame: 5 }));
		for moved in [Position { group: 5, frame: 4 }, Position::group(6), Position::group(5)] {
			assert!(matches!(producer.finish_at(moved), Err(Error::ProtocolViolation)));
		}
	}

	/// An end inside a group that already holds a frame at or past it would leave content
	/// the end says does not exist.
	#[test]
	fn a_mid_group_end_refuses_to_cut_existing_frames() {
		let mut producer = track_producer("test", Info::timed());
		let mut group = producer.create_group(group::Info { sequence: 5 }).unwrap();
		for _ in 0..6 {
			group.write_frame(Timestamp::ZERO, b"x".as_slice()).unwrap();
		}
		assert!(matches!(
			producer.finish_at(Position { group: 5, frame: 5 }),
			Err(Error::ProtocolViolation)
		));
		assert_eq!(producer.finished(), None, "a refused end declares nothing");
		producer.finish_at(Position { group: 5, frame: 6 }).unwrap();
	}

	/// A mid-group end caps its group, whether the group exists yet or not.
	#[test]
	fn a_mid_group_end_caps_later_writes() {
		for early in [true, false] {
			let mut producer = track_producer("test", Info::timed());
			let end = Position { group: 5, frame: 2 };
			if early {
				producer.finish_at(end).unwrap();
			}
			let mut group = producer.create_group(group::Info { sequence: 5 }).unwrap();
			group.write_frame(Timestamp::ZERO, b"x".as_slice()).unwrap();
			if !early {
				producer.finish_at(end).unwrap();
			}
			group.write_frame(Timestamp::ZERO, b"x".as_slice()).unwrap();
			assert!(matches!(
				group.write_frame(Timestamp::ZERO, b"x".as_slice()),
				Err(Error::Closed)
			));
			let mut frames = frame::Buffer::<1>::new();
			assert!(
				frames
					.push(frame::Frame::new(Timestamp::ZERO, bytes::Bytes::from_static(b"x")))
					.is_ok()
			);
			assert!(matches!(group.write_frames(&mut frames), Err(Error::Closed)));
			assert_eq!(group.frame_count(), 2);
			// A refused frame aborts its group, since a write past the end is a bug.
			assert!(matches!(
				group.create_frame(frame::Info {
					size: 1,
					timestamp: Some(Timestamp::ZERO),
				}),
				Err(Error::Closed)
			));
			assert!(producer.create_group(group::Info { sequence: 6 }).is_err());
		}
	}

	#[test]
	fn end_reports_the_declared_end() {
		let mut producer = track_producer("test", Info::timed());
		assert_eq!(producer.finished(), None);

		let _open = producer.create_group(group::Info { sequence: 5 }).unwrap();
		assert_eq!(producer.finished(), None, "a group does not declare an end");

		producer.finish_at(Position::group(9)).unwrap();
		assert_eq!(producer.finished(), Some(Position::group(9)));

		// finish() would try to declare a second boundary, so callers check first.
		assert!(producer.finish().is_err());
	}

	#[test]
	fn end_reports_the_live_edge_after_finish() {
		let producer = track_producer("test", Info::timed());
		let _open = producer.create_group(group::Info { sequence: 5 }).unwrap();
		producer.finish().unwrap();
		assert_eq!(producer.finished(), Some(Position::group(6)));
	}

	#[test]
	#[cfg(feature = "async")]
	fn finish_at_declares_a_future_boundary() {
		let mut producer = track_producer("test", Info::timed());
		producer
			.create_group(group::Info { sequence: 5 })
			.unwrap()
			.finish()
			.unwrap();

		// Learn the track ends at group 6 (exclusive 7) while the live edge is still 5.
		producer.finish_at(Position::group(7)).unwrap();

		let mut consumer = producer.subscribe(None);
		assert_eq!(consumer.assert_group().sequence, 5);

		// The boundary is known immediately, but the track isn't done: group 6 is still
		// outstanding, so the consumer parks rather than seeing end-of-stream.
		let boundary = consumer
			.finished()
			.now_or_never()
			.expect("boundary is known immediately")
			.expect("would have errored");
		assert_eq!(boundary, Position::group(7));
		assert!(
			consumer.recv_group().now_or_never().is_none(),
			"should wait for the outstanding group"
		);

		// The trailing group arrives (below the boundary), then the track completes.
		producer
			.create_group(group::Info { sequence: 6 })
			.unwrap()
			.finish()
			.unwrap();
		assert_eq!(consumer.assert_group().sequence, 6);
		let done = consumer
			.recv_group()
			.now_or_never()
			.expect("should not block")
			.expect("would have errored");
		assert!(done.is_none(), "track completes once the boundary is reached");
	}

	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn readers_end_at_the_boundary_with_missing_lower_groups() {
		let mut producer = track_producer("test", Info::timed());
		let mut arrival = producer.subscribe(None);
		let mut ordered = producer.subscribe(None).ordered();
		producer.finish_at(Position::group(3)).unwrap();
		let _open = producer.create_group(group::Info { sequence: 2 }).unwrap();

		// Missing lower groups do not hold either reader for the wire's grace.
		// The last group's own stream remains open independently of the track end.
		assert_eq!(arrival.assert_group().sequence, 2);
		assert_eq!(ordered.next_group().await.unwrap().unwrap().sequence, 2);
		assert!(arrival.recv_group().now_or_never().unwrap().unwrap().is_none());
		assert!(ordered.next_group().now_or_never().unwrap().unwrap().is_none());

		// Reaching the boundary did not settle the open group: an abort still wins.
		producer.abort(Error::Timeout).unwrap();
		assert!(matches!(
			arrival.recv_group().now_or_never().unwrap(),
			Err(Error::Timeout)
		));
		assert!(matches!(
			ordered.next_group().now_or_never().unwrap(),
			Err(Error::Timeout)
		));
	}

	/// A received copy's end holds readers at a hole below it, since a lower group's stream
	/// may arrive after a higher one, until the hole fills or its grace passes. The last
	/// producer going ends them regardless.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn a_pending_tail_holds_readers_at_a_hole() {
		let mut producer = track_producer("test", None);
		producer.received();
		let mut arrival = producer.subscribe(replay());
		// The wire declares where the feed starts.
		producer.start_at(0).unwrap();
		producer.finish_at(Position::group(3)).unwrap();
		let _high = producer.create_group(group::Info { sequence: 2 }).unwrap();
		assert_eq!(arrival.assert_group().sequence, 2);
		assert!(arrival.recv_group().now_or_never().is_none(), "held at the hole");

		// The late lower groups are delivered, then the full tail ends the reader.
		let _low = producer.create_group(group::Info { sequence: 0 }).unwrap();
		assert_eq!(arrival.assert_group().sequence, 0);
		assert!(arrival.recv_group().now_or_never().is_none(), "group 1 is still owed");
		let _mid = producer.create_group(group::Info { sequence: 1 }).unwrap();
		assert_eq!(arrival.assert_group().sequence, 1);
		assert!(arrival.recv_group().now_or_never().unwrap().unwrap().is_none());

		// The grace passing ends readers at the boundary, hole and all.
		let mut producer = track_producer("test", None);
		producer.received();
		let mut arrival = producer.subscribe(replay());
		producer.start_at(0).unwrap();
		producer.finish_at(Position::group(2)).unwrap();
		let _high = producer.create_group(group::Info { sequence: 1 }).unwrap();
		assert_eq!(arrival.assert_group().sequence, 1);
		assert!(arrival.recv_group().now_or_never().is_none());
		// The subscription's max delay is the grace.
		elapse(&producer, replay().budget());
		assert!(arrival.recv_group().now_or_never().unwrap().unwrap().is_none());

		// So does the last producer going.
		let mut producer = track_producer("test", None);
		producer.received();
		let mut arrival = producer.subscribe(replay());
		producer.start_at(0).unwrap();
		producer.finish_at(Position::group(2)).unwrap();
		let _high = producer.create_group(group::Info { sequence: 1 }).unwrap();
		assert_eq!(arrival.assert_group().sequence, 1);
		drop(producer);
		assert!(arrival.recv_group().now_or_never().unwrap().unwrap().is_none());
	}

	/// The grace alone ends a reader parked at a hole, with no write to wake it: below an
	/// arrival it runs from that arrival, and above the last one from the declared end.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn a_parked_reader_wakes_when_the_tail_grace_passes() {
		// The subscription's max delay is the grace.
		let grace = replay().budget();
		// Hole 0 sits below arrival 1; holes 2..4 sit above the last arrival.
		for (end, arrived_late) in [(2, false), (4, true)] {
			let mut producer = track_producer("test", None);
			producer.received();
			elapse(&producer, Duration::ZERO);
			let mut arrival = producer.subscribe(replay());
			producer.start_at(0).unwrap();
			producer
				.create_group(group::Info { sequence: 1 })
				.unwrap()
				.finish()
				.unwrap();
			assert_eq!(arrival.assert_group().sequence, 1);
			if arrived_late {
				// The end comes later than the arrival, so its gap runs from the end.
				elapse(&producer, grace / 2);
			}
			producer.finish_at(Position::group(end)).unwrap();

			let woken = Arc::new(std::sync::atomic::AtomicBool::new(false));
			let waiter = kio::Waiter::new(futures::task::waker(Arc::new(FlagWake(woken.clone()))));
			assert!(
				arrival.poll_recv_group(&waiter).is_pending(),
				"end {end}: held at the hole"
			);
			if arrived_late {
				// Hole 0 folds a grace after the arrival: the reader wakes to judge it a miss,
				// which nothing fills, and stays held for the holes above.
				elapse(&producer, grace / 2);
				assert!(woken.swap(false, Ordering::SeqCst), "end {end}: the fold went unjudged");
				assert!(arrival.poll_recv_group(&waiter).is_pending());
				elapse(&producer, grace / 2 - Duration::from_millis(1));
			} else {
				elapse(&producer, grace - Duration::from_millis(1));
			}
			assert!(!woken.load(Ordering::SeqCst), "end {end}: woken before the grace");
			assert!(arrival.poll_recv_group(&waiter).is_pending());
			elapse(&producer, Duration::from_millis(1));
			assert!(
				woken.load(Ordering::SeqCst),
				"end {end}: the grace never woke the reader"
			);
			assert!(matches!(arrival.poll_recv_group(&waiter), Poll::Ready(Ok(None))));
		}
	}

	/// An unbounded max delay never gives up on a hole, below an arrival or above the last
	/// one, until it fills.
	#[test]
	#[cfg(feature = "async")]
	fn an_unbounded_tail_holds_readers_at_a_hole() {
		// Hole 0 sits below arrival 1; hole 1 sits above arrival 0.
		for (arrived, missing) in [(1, 0), (0, 1)] {
			let mut producer = track_producer("test", None);
			producer.received();
			let mut arrival = producer.subscribe(replay().with_max_delay(None));
			producer.start_at(0).unwrap();
			producer.finish_at(Position::group(2)).unwrap();
			producer
				.create_group(group::Info { sequence: arrived })
				.unwrap()
				.finish()
				.unwrap();
			assert_eq!(arrival.assert_group().sequence, arrived);

			elapse(&producer, Duration::from_secs(3600));
			assert!(
				arrival.recv_group().now_or_never().is_none(),
				"hole {missing}: given up on"
			);

			producer
				.create_group(group::Info { sequence: missing })
				.unwrap()
				.finish()
				.unwrap();
			assert_eq!(arrival.assert_group().sequence, missing);
			assert!(arrival.recv_group().now_or_never().unwrap().unwrap().is_none());
		}
	}

	/// An end declared ahead of a feed still delivering does not cut it off: each arrival
	/// restarts the grace for the groups above it. A feed that stalls past it ends readers.
	#[test]
	#[cfg(feature = "async")]
	fn a_delivering_feed_holds_an_end_declared_ahead() {
		let mut producer = track_producer("test", None);
		producer.received();
		let grace = replay().budget();
		let mut arrival = producer.subscribe(replay());
		producer.start_at(0).unwrap();
		producer.finish_at(Position::group(5)).unwrap();
		for sequence in 0..3 {
			elapse(&producer, grace / 2);
			producer
				.create_group(group::Info { sequence })
				.unwrap()
				.finish()
				.unwrap();
			assert_eq!(arrival.assert_group().sequence, sequence);
		}
		elapse(&producer, grace - Duration::from_millis(1));
		assert!(arrival.recv_group().now_or_never().is_none(), "still within the grace");
		elapse(&producer, Duration::from_millis(1));
		assert!(arrival.recv_group().now_or_never().unwrap().unwrap().is_none());
	}

	/// A start declared after the end asks the feed for groups it may only now send, so the
	/// tail above the last arrival waits a fresh grace from it, not what is left of the end's.
	#[test]
	#[cfg(feature = "async")]
	fn a_late_start_restarts_the_tail_grace() {
		let mut producer = track_producer("test", None);
		producer.received();
		let grace = replay().budget();
		let mut arrival = producer.subscribe(replay());
		producer.finish_at(Position::group(2)).unwrap();
		elapse(&producer, grace - Duration::from_millis(1));
		producer.start_at(0).unwrap();
		elapse(&producer, Duration::from_millis(1));
		assert!(arrival.recv_group().now_or_never().is_none(), "a fresh grace");
		elapse(&producer, grace - Duration::from_millis(1));
		assert!(arrival.recv_group().now_or_never().unwrap().unwrap().is_none());
	}

	/// Only a received copy waits for its tail: a local publisher's end below a hole ends
	/// readers once the edge reaches it, as the publisher skipped the group.
	#[test]
	#[cfg(feature = "async")]
	fn a_local_end_does_not_wait_for_a_tail() {
		let mut producer = track_producer("test", None);
		let mut arrival = producer.subscribe(replay());
		producer.start_at(0).unwrap();
		producer.finish_at(Position::group(2)).unwrap();
		producer
			.create_group(group::Info { sequence: 1 })
			.unwrap()
			.finish()
			.unwrap();
		assert_eq!(arrival.assert_group().sequence, 1);
		assert!(arrival.recv_group().now_or_never().unwrap().unwrap().is_none());
	}

	/// A group the publisher dropped is not in flight, so it holds no reader at the end.
	#[test]
	#[cfg(feature = "async")]
	fn a_dropped_group_fills_the_tail() {
		let mut producer = track_producer("test", None);
		producer.received();
		let mut arrival = producer.subscribe(replay());
		producer.start_at(0).unwrap();
		producer.finish_at(Position::group(3)).unwrap();
		producer
			.create_group(group::Info { sequence: 2 })
			.unwrap()
			.finish()
			.unwrap();
		assert_eq!(arrival.assert_group().sequence, 2);
		assert!(arrival.recv_group().now_or_never().is_none(), "held at the hole");
		producer.drop_groups(0..2, StreamError::NotFound);
		assert!(arrival.recv_group().now_or_never().unwrap().unwrap().is_none());
	}

	/// While the start is pending, everything below the end is owed, but only what has
	/// not arrived or been dropped: an end with nothing missing below it ends readers at once.
	#[test]
	#[cfg(feature = "async")]
	fn a_pending_start_owes_nothing_already_accounted_for() {
		// An end before the first group owes nothing.
		let mut producer = track_producer("test", None);
		producer.received();
		producer.request_start(None).unwrap();
		let mut arrival = producer.subscribe(replay());
		producer.finish_at(Position::group(0)).unwrap();
		assert!(arrival.recv_group().now_or_never().unwrap().unwrap().is_none());

		// Nor does one whose groups below it all arrived or were dropped.
		let mut producer = track_producer("test", None);
		producer.received();
		producer.request_start(None).unwrap();
		let mut arrival = producer.subscribe(replay());
		producer
			.create_group(group::Info { sequence: 0 })
			.unwrap()
			.finish()
			.unwrap();
		assert_eq!(arrival.assert_group().sequence, 0);
		producer.drop_groups(1..2, StreamError::NotFound);
		producer.finish_at(Position::group(2)).unwrap();
		assert!(arrival.recv_group().now_or_never().unwrap().unwrap().is_none());

		// A hole is still owed until the grace passes.
		let mut producer = track_producer("test", None);
		producer.received();
		producer.request_start(None).unwrap();
		let mut arrival = producer.subscribe(replay());
		producer
			.create_group(group::Info { sequence: 1 })
			.unwrap()
			.finish()
			.unwrap();
		assert_eq!(arrival.assert_group().sequence, 1);
		producer.finish_at(Position::group(2)).unwrap();
		assert!(arrival.recv_group().now_or_never().is_none(), "group 0 is owed");
		elapse(&producer, replay().budget());
		assert!(arrival.recv_group().now_or_never().unwrap().unwrap().is_none());
	}

	/// A fetched copy of a group the live feed still owes never reaches an arrival-order
	/// reader, so it does not fill the hole: the reader waits for the live group.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn a_fetched_group_does_not_fill_a_pending_tail() {
		let mut producer = track_producer("test", None);
		producer.received();
		let dynamic = producer.requests();
		let consumer = producer.consume();
		let mut arrival = producer.subscribe(None);
		producer.start_at(0).unwrap();
		producer.finish_at(Position::group(2)).unwrap();
		let _high = producer.create_group(group::Info { sequence: 1 }).unwrap();
		assert_eq!(arrival.assert_group().sequence, 1);

		let _pending = consumer.fetch_group(0, None);
		let req = dynamic.requested_fill_group().now_or_never().unwrap().unwrap();
		req.accept(None).unwrap().finish().unwrap();
		assert!(
			arrival.recv_group().now_or_never().is_none(),
			"the live group 0 is still owed"
		);
	}

	/// An abort ends the wait for a pending tail. A group the tail still owed was cut off,
	/// so readers get the abort rather than a clean end without it.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn an_abort_errors_a_pending_tail_short_of_a_group() {
		let mut producer = track_producer("test", None);
		producer.received();
		let mut arrival = producer.subscribe(None);
		producer.start_at(0).unwrap();
		producer.finish_at(Position::group(2)).unwrap();
		let mut high = producer.create_group(group::Info { sequence: 1 }).unwrap();
		high.finish().unwrap();
		assert_eq!(arrival.assert_group().sequence, 1);
		assert!(arrival.recv_group().now_or_never().is_none(), "held at the hole");

		producer.abort(Error::Dropped).unwrap();
		assert!(matches!(
			arrival.recv_group().now_or_never().unwrap(),
			Err(Error::Dropped)
		));
	}

	/// A reader that got every group ends cleanly, even when the track aborts before the
	/// reader polls past the last one.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn an_abort_after_a_full_tail_ends_cleanly() {
		let mut producer = track_producer("test", None);
		producer.received();
		let mut arrival = producer.subscribe(replay());
		producer.start_at(0).unwrap();
		producer.finish_at(Position::group(2)).unwrap();
		for sequence in [1, 0] {
			producer
				.create_group(group::Info { sequence })
				.unwrap()
				.finish()
				.unwrap();
		}
		producer.clone().abort(Error::Dropped).unwrap();
		assert_eq!(arrival.assert_group().sequence, 1);
		assert_eq!(arrival.assert_group().sequence, 0);
		assert!(arrival.recv_group().now_or_never().unwrap().unwrap().is_none());
	}

	/// A gap the grace already gave up on was lost before the abort, not cut off by it, so
	/// readers end cleanly. Within the grace, the same abort errors them.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn an_abort_past_the_grace_ends_a_pending_tail_cleanly() {
		for (waited, ended) in [(ledger::GRACE / 2, false), (ledger::GRACE, true)] {
			let mut producer = track_producer("test", None);
			producer.received();
			let mut arrival = producer.subscribe(None);
			producer.start_at(0).unwrap();
			producer.finish_at(Position::group(3)).unwrap();
			for sequence in [0, 2] {
				producer
					.create_group(group::Info { sequence })
					.unwrap()
					.finish()
					.unwrap();
				assert_eq!(arrival.assert_group().sequence, sequence);
			}
			assert!(arrival.recv_group().now_or_never().is_none(), "held at the hole");

			elapse(&producer, waited);
			producer.abort(Error::Dropped).unwrap();
			let res = arrival.recv_group().now_or_never().unwrap();
			assert_eq!(matches!(res, Ok(None)), ended, "waited {waited:?}");
		}
	}

	/// An evicted group still arrived, so the pending tail counts it: readers end at the
	/// declared end rather than wait for the session to settle the tail.
	#[test]
	#[cfg(feature = "async")]
	fn an_evicted_group_still_fills_a_pending_tail() {
		let mut producer = track_producer("test", None);
		producer.received();
		let mut arrival = producer.subscribe(replay());
		producer.start_at(0).unwrap();
		producer.finish_at(Position::group(3)).unwrap();
		for sequence in [0, 1] {
			producer
				.create_group(group::Info { sequence })
				.unwrap()
				.finish()
				.unwrap();
			assert_eq!(arrival.assert_group().sequence, sequence);
		}
		elapse(&producer, cache::DEFAULT_EXPIRY + Duration::from_secs(1));
		producer
			.create_group(group::Info { sequence: 2 })
			.unwrap()
			.finish()
			.unwrap();
		assert!(!producer.state.read().lookup.contains_key(&0), "group 0 was evicted");
		assert_eq!(arrival.assert_group().sequence, 2);
		assert!(arrival.recv_group().now_or_never().unwrap().unwrap().is_none());
	}

	/// A datagram is a live arrival too, so it fills its sequence in a pending tail.
	#[test]
	#[cfg(feature = "async")]
	fn a_datagram_fills_a_pending_tail() {
		let mut producer = track_producer("test", None);
		producer.received();
		let mut arrival = producer.subscribe(replay());
		producer.start_at(0).unwrap();
		producer.finish_at(Position::group(2)).unwrap();
		producer.insert_datagram(0, None, b"x".as_slice()).unwrap();
		producer
			.create_group(group::Info { sequence: 1 })
			.unwrap()
			.finish()
			.unwrap();
		assert_eq!(arrival.assert_group().sequence, 1);
		assert!(arrival.recv_group().now_or_never().unwrap().unwrap().is_none());
	}

	/// The wire's end can land after the highest group, with lower groups still owed. A
	/// reader already past that group stays held at the hole: the end never shows without
	/// its pending tail.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn an_end_after_the_highest_group_holds_the_reader() {
		let mut producer = track_producer("test", None);
		producer.received();
		let mut arrival = producer.subscribe(replay());
		producer.start_at(0).unwrap();
		let _high = producer.create_group(group::Info { sequence: 2 }).unwrap();
		assert_eq!(arrival.assert_group().sequence, 2);
		assert!(arrival.recv_group().now_or_never().is_none());

		producer.finish_at(Position::group(3)).unwrap();
		assert!(arrival.recv_group().now_or_never().is_none(), "held at the hole");
		assert!(
			matches!(producer.finish_at(Position::group(2)), Err(Error::ProtocolViolation)),
			"a second end is refused"
		);
		assert_eq!(producer.finished(), Some(Position::group(3)), "the first end stands");

		let mut _low = Vec::new();
		for sequence in [0, 1] {
			_low.push(producer.create_group(group::Info { sequence }).unwrap());
			assert_eq!(arrival.assert_group().sequence, sequence);
		}
		assert!(arrival.recv_group().now_or_never().unwrap().unwrap().is_none());
	}

	/// An abort before the declared end settled wins over it: the boundary was reached,
	/// but a group below it was still open, so the track was cut off rather than ended.
	#[test]
	#[cfg(feature = "async")]
	fn abort_before_the_end_settles_wins() {
		let mut producer = track_producer("test", Info::timed());
		let mut consumer = producer.subscribe(replay());

		let mut head = producer.create_group(group::Info { sequence: 0 }).unwrap();
		head.finish().unwrap();
		let _tail = producer.create_group(group::Info { sequence: 1 }).unwrap();
		producer.finish_at(Position::group(2)).unwrap();
		assert_eq!(consumer.assert_group().sequence, 0);

		producer.abort(Error::Timeout).unwrap();
		let res = consumer.recv_group().now_or_never().expect("should not block");
		assert!(matches!(res, Err(Error::Timeout)));
	}

	/// A local session closing is a dead route to the origin, not finished content, so a
	/// replacement route can take over where it left off.
	#[test]
	fn session_close_is_not_completion_and_keeps_the_resume_position() {
		let producer = track_producer("local", Info::timed());
		let consumer = producer.consume();
		producer.append_group().unwrap().finish().unwrap();
		let mut group = producer.append_group().unwrap();
		group
			.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"a"))
			.unwrap();
		group
			.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"b"))
			.unwrap();
		producer
			.abort(Error::Stream(StreamError::Session(crate::SessionError::Cancel)))
			.unwrap();
		assert!(matches!(
			consumer.poll_complete(&kio::Waiter::noop()),
			Poll::Ready(Err(_))
		));
		assert_eq!(consumer.resume_position(), Some(Position { group: 1, frame: 2 }));
	}

	/// `closed` settles once the declared end is reached, not when it is declared, and an
	/// abort after that keeps the clean outcome.
	#[test]
	#[cfg(feature = "async")]
	fn closed_settles_on_a_reached_end() {
		let waiter = kio::Waiter::noop();
		let mut producer = track_producer("test", Info::timed());
		let demand = producer.demand();
		producer.append_group().unwrap().finish().unwrap();
		producer.finish_at(Position::group(2)).unwrap();
		assert!(producer.poll_closed(&waiter).is_pending(), "group 1 is still to come");
		assert!(demand.closed().now_or_never().is_none());

		producer.append_group().unwrap().finish().unwrap();
		assert!(matches!(producer.poll_closed(&waiter), Poll::Ready(Ok(()))));
		assert!(matches!(demand.closed().now_or_never(), Some(Ok(()))));
		assert!(
			producer.poll_aborted(&waiter).is_pending(),
			"lower groups may still arrive"
		);

		producer.clone().abort(Error::Cancel).unwrap();
		assert!(matches!(producer.poll_closed(&waiter), Poll::Ready(Ok(()))));
		assert!(matches!(demand.closed().now_or_never(), Some(Ok(()))));
	}

	/// A group still open below a reached end keeps the outcome pending: it finishing settles
	/// the end and wakes the wait, while an abort before then cuts the track off.
	#[test]
	#[cfg(feature = "async")]
	fn closed_waits_for_open_groups_below_the_end() {
		use std::sync::atomic::{AtomicBool, Ordering};

		let mut producer = track_producer("test", Info::timed());
		let mut open = producer.append_group().unwrap();
		producer.finish_at(Position::group(1)).unwrap();
		let woken = Arc::new(AtomicBool::new(false));
		let waiter = kio::Waiter::new(futures::task::waker(Arc::new(FlagWake(woken.clone()))));
		assert!(producer.poll_closed(&waiter).is_pending(), "group 0 may still abort");
		open.finish().unwrap();
		assert!(woken.load(Ordering::SeqCst), "the group finishing wakes the wait");
		assert!(matches!(producer.poll_closed(&waiter), Poll::Ready(Ok(()))));

		let mut cut = track_producer("cut", Info::timed());
		let _open = cut.append_group().unwrap();
		cut.finish_at(Position::group(1)).unwrap();
		assert!(cut.poll_closed(&kio::Waiter::noop()).is_pending());
		cut.clone().abort(Error::Cancel).unwrap();
		assert!(matches!(
			cut.poll_closed(&kio::Waiter::noop()),
			Poll::Ready(Err(Error::Cancel))
		));
	}

	/// A received end whose tail grace passed is final once `closed()` reports it: a later
	/// subscription with a longer grace, and an abort within it, leave the clean outcome.
	#[test]
	fn a_reported_end_survives_a_longer_grace() {
		let waiter = kio::Waiter::noop();
		let mut producer = track_producer("test", None);
		producer.received();
		let _short = producer.subscribe(None);
		producer.start_at(0).unwrap();
		producer.finish_at(Position::group(3)).unwrap();
		for sequence in [0, 2] {
			producer
				.create_group(group::Info { sequence })
				.unwrap()
				.finish()
				.unwrap();
		}
		assert!(producer.poll_closed(&waiter).is_pending(), "held at the hole");
		elapse(&producer, ledger::GRACE);
		assert!(matches!(producer.poll_closed(&waiter), Poll::Ready(Ok(()))));

		let _long = producer.subscribe(Subscription::default().with_max_delay(ledger::GRACE * 10));
		assert!(matches!(producer.poll_closed(&waiter), Poll::Ready(Ok(()))));
		producer.clone().abort(Error::Cancel).unwrap();
		assert!(matches!(producer.poll_closed(&waiter), Poll::Ready(Ok(()))));
	}

	/// A settled end is final: a late group below it and a later abort leave the outcome
	/// `closed()` already saw.
	#[test]
	#[cfg(feature = "async")]
	fn a_settled_end_survives_a_late_group_and_an_abort() {
		let waiter = kio::Waiter::noop();
		let mut producer = track_producer("test", Info::timed());
		let demand = producer.demand();
		producer
			.create_group(group::Info { sequence: 5 })
			.unwrap()
			.finish()
			.unwrap();
		producer.finish_at(Position::group(6)).unwrap();
		assert!(matches!(producer.poll_closed(&waiter), Poll::Ready(Ok(()))));

		let _late = producer.create_group(group::Info { sequence: 4 }).unwrap();
		assert!(matches!(producer.poll_closed(&waiter), Poll::Ready(Ok(()))));
		producer.clone().abort(Error::Cancel).unwrap();
		assert!(matches!(producer.poll_closed(&waiter), Poll::Ready(Ok(()))));
		assert!(matches!(demand.closed().now_or_never(), Some(Ok(()))));
	}

	/// An abort short of the end, or the last producer leaving without one, is the outcome.
	#[test]
	#[cfg(feature = "async")]
	fn closed_reports_an_unfinished_end() {
		let waiter = kio::Waiter::noop();
		let aborted = track_producer("aborted", Info::timed());
		aborted.clone().abort(Error::Timeout).unwrap();
		assert!(matches!(aborted.poll_closed(&waiter), Poll::Ready(Err(Error::Timeout))));

		let dropped = track_producer("dropped", Info::timed());
		let demand = dropped.demand();
		drop(dropped);
		assert!(matches!(demand.closed().now_or_never(), Some(Err(Error::Dropped))));
	}

	/// An abort short of the end keeps the groups that finished for a reader that has not
	/// pulled them yet: it gets them, then the abort. The open group nobody will finish
	/// is gone, on both cursors.
	#[test]
	#[cfg(feature = "async")]
	fn abort_keeps_finished_groups_for_a_slow_reader() {
		let producer = track_producer("test", Info::timed());
		let mut arrival = producer.subscribe(replay());
		let mut ordered = producer.subscribe(replay()).ordered();

		for sequence in 0..2 {
			producer
				.create_group(group::Info { sequence })
				.unwrap()
				.finish()
				.unwrap();
		}
		let _open = producer.create_group(group::Info { sequence: 2 }).unwrap();
		producer.abort(Error::Timeout).unwrap();

		assert_eq!(arrival.assert_group().sequence, 0);
		assert_eq!(arrival.assert_group().sequence, 1);
		let res = arrival.recv_group().now_or_never().expect("should not block");
		assert!(matches!(res, Err(Error::Timeout)), "expected the abort");

		assert_eq!(drain_ordered(&mut ordered), [0, 1]);
		let res = ordered
			.next_group()
			.now_or_never()
			.expect("should not block")
			.map(|group| group.map(|group| group.sequence));
		assert!(matches!(res, Err(Error::Timeout)), "expected the abort, got {res:?}");
	}

	/// The last producer dropping without a finish keeps the finished groups the same way.
	#[test]
	#[cfg(feature = "async")]
	fn dropped_producer_keeps_finished_groups_for_a_slow_reader() {
		let producer = track_producer("test", Info::timed());
		let mut consumer = producer.subscribe(None);
		producer
			.create_group(group::Info { sequence: 0 })
			.unwrap()
			.finish()
			.unwrap();
		drop(producer);

		assert_eq!(consumer.assert_group().sequence, 0);
		let res = consumer.recv_group().now_or_never().expect("should not block");
		assert!(matches!(res, Err(Error::Dropped)), "expected the drop");
	}

	/// A closed track's groups, its latest included, expire once idle, so a stale
	/// consumer cannot pin them. A live track's latest stays protected.
	#[test]
	fn closed_track_expires_its_latest_group() {
		let pool = cache::Pool::new(cache::Config::default().with_expiry(Duration::from_secs(1)));

		let live = track_producer_pooled("live", pool.clone());
		live.append_group().unwrap().finish().unwrap();

		let aborted = track_producer_pooled("aborted", pool.clone());
		aborted.append_group().unwrap().finish().unwrap();
		let stale_aborted = aborted.consume();
		aborted.abort(Error::Timeout).unwrap();

		let finished = track_producer_pooled("finished", pool.clone());
		finished.append_group().unwrap().finish().unwrap();
		finished.finish().unwrap();
		let stale_finished = finished.consume();
		drop(finished);

		// The first pass dates the activity it has not seen yet; the next one expires it.
		for _ in 0..2 {
			pool.step(Duration::from_secs(2));
			pool.sweep();
		}

		assert!(live.consume().peek_group(0).is_some(), "a live track keeps its latest");
		assert!(
			stale_aborted.peek_group(0).is_none(),
			"an aborted track's latest expired"
		);
		assert!(
			stale_finished.peek_group(0).is_none(),
			"a sealed track's latest expired"
		);
	}

	/// A cut on an ended track expires like the group it was: a stale consumer's reader
	/// gets it within the window, and nothing once the sweep retires it.
	#[test]
	#[cfg(feature = "async")]
	fn closed_track_retires_an_expired_cut() {
		let pool = cache::Pool::new(cache::Config::default().with_expiry(Duration::from_secs(1)));
		let producer = track_producer_pooled("cut", pool.clone());
		let mut group = producer.append_group().unwrap();
		group.write_frame(Timestamp::ZERO, &b"a"[..]).unwrap();
		group.abort(Error::Stream(StreamError::App(5))).unwrap();
		producer.finish().unwrap();
		let stale = producer.consume();
		drop(producer);

		let mut reader = stale.subscribe(replay()).now_or_never().unwrap().unwrap();
		assert_eq!(
			reader.assert_group().sequence,
			0,
			"the cut is handed out within the window"
		);

		// The first pass dates the activity it has not seen yet; the next one expires it.
		for _ in 0..2 {
			pool.step(Duration::from_secs(2));
			pool.sweep();
		}

		let mut late = stale.subscribe(replay()).now_or_never().unwrap().unwrap();
		let res = late.recv_group().now_or_never().expect("should not block");
		assert!(matches!(res, Ok(None)), "the expired cut is gone");
	}

	/// An abort after every group below the declared end finished leaves the end standing.
	#[test]
	#[cfg(feature = "async")]
	fn abort_after_the_end_settles_ends_clean() {
		let mut producer = track_producer("test", Info::timed());
		let mut consumer = producer.subscribe(replay());

		for sequence in 0..2 {
			let mut group = producer.create_group(group::Info { sequence }).unwrap();
			group.finish().unwrap();
		}
		producer.finish_at(Position::group(2)).unwrap();
		assert_eq!(consumer.assert_group().sequence, 0);
		assert_eq!(consumer.assert_group().sequence, 1);

		producer.abort(Error::Timeout).unwrap();
		let res = consumer.recv_group().now_or_never().expect("should not block");
		assert!(matches!(res, Ok(None)));
	}

	/// The settled end stands on the ordered cursor too: a reader that starts after the
	/// abort gets every group below the end, then the clean end, not the abort.
	#[test]
	#[cfg(feature = "async")]
	fn abort_after_the_end_settles_ends_clean_for_an_ordered_reader() {
		let mut producer = track_producer("test", Info::timed());
		let mut consumer = producer.subscribe(replay()).ordered();

		for sequence in 0..2 {
			let mut group = producer.create_group(group::Info { sequence }).unwrap();
			group.finish().unwrap();
		}
		producer.finish_at(Position::group(2)).unwrap();
		producer.abort(Error::Timeout).unwrap();

		assert_eq!(drain_ordered(&mut consumer), [0, 1]);
		let end = consumer
			.next_group()
			.now_or_never()
			.expect("should not block")
			.map(|group| group.map(|group| group.sequence));
		assert!(matches!(end, Ok(None)), "expected the clean end, got {end:?}");
	}

	/// The settled end stands on the ordered cursor even where it has nothing to read:
	/// capped below the end, and with a gap below the cap. Parking is not an option on a
	/// closed track (the channel would turn it into the abort), so these end clean.
	#[test]
	#[cfg(feature = "async")]
	fn abort_after_the_end_settles_ends_clean_below_the_cap() {
		let mut producer = track_producer("test", Info::timed());
		let mut consumer = producer.subscribe(None).ordered();
		for sequence in 0..2 {
			producer
				.create_group(group::Info { sequence })
				.unwrap()
				.finish()
				.unwrap();
		}
		producer.finish_at(Position::group(2)).unwrap();
		producer.abort(Error::Timeout).unwrap();

		consumer.set_range(..1);
		assert_eq!(drain_ordered(&mut consumer), [0]);
		let end = consumer
			.next_group()
			.now_or_never()
			.expect("should not block")
			.map(|group| group.map(|group| group.sequence));
		assert!(
			matches!(end, Ok(None)),
			"capped at the end: expected the clean end, got {end:?}"
		);

		let mut producer = track_producer("test", Info::timed());
		let mut consumer = producer.subscribe(None).ordered();
		for sequence in [0, 2] {
			producer
				.create_group(group::Info { sequence })
				.unwrap()
				.finish()
				.unwrap();
		}
		producer.finish_at(Position::group(3)).unwrap();
		producer.abort(Error::Timeout).unwrap();

		consumer.set_range(..2);
		assert_eq!(drain_ordered(&mut consumer), [0]);
		let end = consumer
			.next_group()
			.now_or_never()
			.expect("should not block")
			.map(|group| group.map(|group| group.sequence));
		assert!(
			matches!(end, Ok(None)),
			"gap below the cap: expected the clean end, got {end:?}"
		);
	}

	#[test]
	#[cfg(feature = "async")]
	fn recv_group_finishes_without_waiting_for_gaps() {
		let producer = track_producer("test", Info::timed());
		producer
			.create_group(group::Info { sequence: 1 })
			.unwrap()
			.finish()
			.unwrap();
		producer.finish().unwrap();

		let mut consumer = producer.subscribe(None);
		assert_eq!(consumer.assert_group().sequence, 1);

		let done = consumer
			.recv_group()
			.now_or_never()
			.expect("should not block")
			.expect("would have errored");
		assert!(done.is_none(), "track should finish without waiting for gaps");
	}

	#[test]
	#[cfg(feature = "async")]
	fn next_group_skips_late_arrivals() {
		let producer = track_producer("test", Info::timed());
		let mut consumer = producer.subscribe(None).ordered();

		// Seq 5 arrives first.
		producer
			.create_group(group::Info { sequence: 5 })
			.unwrap()
			.finish()
			.unwrap();
		let group = consumer
			.next_group()
			.now_or_never()
			.expect("should not block")
			.expect("would have errored")
			.expect("track should not be closed");
		assert_eq!(group.sequence, 5);

		// Seq 3 arrives late, skipped because 3 <= 5.
		producer
			.create_group(group::Info { sequence: 3 })
			.unwrap()
			.finish()
			.unwrap();
		// Seq 4 arrives late and is also skipped.
		producer
			.create_group(group::Info { sequence: 4 })
			.unwrap()
			.finish()
			.unwrap();
		// Seq 7 arrives and is returned.
		producer
			.create_group(group::Info { sequence: 7 })
			.unwrap()
			.finish()
			.unwrap();

		let group = consumer
			.next_group()
			.now_or_never()
			.expect("should not block")
			.expect("would have errored")
			.expect("track should not be closed");
		assert_eq!(group.sequence, 7);

		// No more groups. This would block.
		assert!(
			consumer.next_group().now_or_never().is_none(),
			"should block waiting for a higher sequence"
		);
	}

	#[test]
	#[cfg(feature = "async")]
	fn next_group_returns_arrivals_in_order() {
		let producer = track_producer("test", Info::timed());
		let mut consumer = producer.subscribe(replay()).ordered();

		// Seq 3 arrives first, then seq 5. Both should be returned in arrival order.
		producer
			.create_group(group::Info { sequence: 3 })
			.unwrap()
			.finish()
			.unwrap();
		producer
			.create_group(group::Info { sequence: 5 })
			.unwrap()
			.finish()
			.unwrap();

		let group = consumer
			.next_group()
			.now_or_never()
			.expect("should not block")
			.expect("would have errored")
			.expect("track should not be closed");
		assert_eq!(group.sequence, 3);

		let group = consumer
			.next_group()
			.now_or_never()
			.expect("should not block")
			.expect("would have errored")
			.expect("track should not be closed");
		assert_eq!(group.sequence, 5);
	}

	#[test]
	fn skipping_another_copys_groups_preserves_arrival_bounds_and_the_last_sequence() {
		let producer = track_producer("test", Info::timed());
		let mut subscriber = producer.subscribe(replay());
		for sequence in [u64::MAX - 1, u64::MAX] {
			producer
				.create_group(group::Info { sequence })
				.unwrap()
				.finish()
				.unwrap();
		}
		let waiter = kio::Waiter::noop();
		assert_eq!(
			subscriber.poll_peek_group(true, &waiter).map(Result::unwrap),
			Poll::Ready(Some(u64::MAX - 1))
		);
		subscriber.skip_ordered(Some(u64::MAX));
		assert_eq!(
			subscriber.poll_peek_group(true, &waiter).map(Result::unwrap),
			Poll::Ready(Some(u64::MAX))
		);
		subscriber.skip_ordered(None);
		subscriber.skip_ordered(Some(0));
		assert_eq!(
			subscriber.poll_peek_group(true, &waiter).map(Result::unwrap),
			Poll::Ready(None)
		);
		assert_eq!(subscriber.delivered.runs(), 0, "skips are not deliveries");
		assert_eq!(subscriber.take_stale().groups, 0, "duplicates are not stale skips");
		let Inner::Plain(cursor) = &subscriber.inner else {
			unreachable!()
		};
		assert_eq!(cursor.min_sequence, 0);
		assert_eq!(cursor.end_sequence, None);
		assert_eq!(cursor.index, 0);
		assert_eq!(
			subscriber.poll_peek_group(false, &waiter).map(Result::unwrap),
			Poll::Ready(Some(u64::MAX - 1))
		);
	}

	#[test]
	fn an_ordered_skip_counts_only_convictions_not_delivered_elsewhere() {
		let mut producer = track_producer("test", Info::timed());
		for second in 0..4 {
			append_at(&mut producer, second * 1000);
		}
		let mut subscriber = producer.subscribe(None);
		let waiter = kio::Waiter::noop();
		assert_eq!(
			subscriber.poll_peek_group(true, &waiter).map(Result::unwrap),
			Poll::Ready(Some(3))
		);
		subscriber.skip_ordered(Some(2));
		let Poll::Ready(Ok(Some(group))) = subscriber.poll_next_group(&waiter) else {
			panic!("the reserved group is still delivered");
		};
		assert_eq!(group.sequence, 3);
		assert_eq!(subscriber.take_stale().groups, 1, "only group 2 was skipped for age");
		assert_eq!(subscriber.take_stale().groups, 0, "counted once");
	}

	#[test]
	fn changed_bounds_invalidate_a_peeked_group() {
		for ordered in [false, true] {
			for raise_floor in [false, true] {
				let producer = track_producer("test", Info::timed());
				let mut subscriber = producer.subscribe(replay().with_order(Order::Ascending));
				for sequence in [1, 2] {
					producer
						.create_group(group::Info { sequence })
						.unwrap()
						.finish()
						.unwrap();
				}
				let waiter = kio::Waiter::noop();
				assert_eq!(
					subscriber.poll_peek_group(ordered, &waiter).map(Result::unwrap),
					Poll::Ready(Some(1))
				);
				if raise_floor {
					subscriber.set_range(2..);
					let Poll::Ready(Ok(Some(group))) = subscriber.poll_group(ordered, &waiter) else {
						panic!("group 2 is inside the new floor");
					};
					assert_eq!(group.sequence, 2);
				} else {
					subscriber.set_range(..0);
					assert!(subscriber.poll_group(ordered, &waiter).is_pending());
					subscriber.set_range(..);
					let Poll::Ready(Ok(Some(group))) = subscriber.poll_group(ordered, &waiter) else {
						panic!("group 1 was parked, not consumed");
					};
					assert_eq!(group.sequence, 1);
				}
			}
		}
	}

	#[test]
	fn taking_a_peeked_group_does_not_substitute_after_eviction() {
		for ordered in [false, true] {
			let producer = track_producer("test", Info::timed());
			let mut subscriber = producer.subscribe(replay());
			for sequence in [1, 3] {
				producer
					.create_group(group::Info { sequence })
					.unwrap()
					.finish()
					.unwrap();
			}
			let waiter = kio::Waiter::noop();
			assert_eq!(
				subscriber.poll_peek_group(ordered, &waiter).map(Result::unwrap),
				Poll::Ready(Some(1))
			);
			evict(&producer, 1);
			let Poll::Ready(Ok(Some(group))) = subscriber.poll_group(ordered, &waiter) else {
				panic!("the exact reserved group must be returned");
			};
			assert_eq!(
				group.sequence, 1,
				"ordered={ordered}: eviction must not substitute group 3"
			);
		}
	}

	/// A group a widening re-offers stays owed while it is only peeked.
	#[test]
	fn peeking_keeps_a_re_offered_group_owed() {
		let producer = track_producer("test", Info::timed());
		let mut subscriber = producer.subscribe(replay().with_order(Order::Ascending));
		subscriber.set_range(..0);
		for sequence in [1, 2] {
			producer
				.create_group(group::Info { sequence })
				.unwrap()
				.finish()
				.unwrap();
		}
		let waiter = kio::Waiter::noop();
		assert!(subscriber.poll_group(false, &waiter).is_pending());
		subscriber.set_range(..);
		for _ in 0..2 {
			assert_eq!(
				subscriber.poll_peek_group(false, &waiter).map(Result::unwrap),
				Poll::Ready(Some(1))
			);
		}
		let mut read = Vec::new();
		while let Poll::Ready(Ok(Some(group))) = subscriber.poll_group(false, &waiter) {
			read.push(group.sequence);
		}
		assert_eq!(read, [1, 2]);
	}

	#[test]
	fn peeking_keeps_each_orders_cursor_and_rechecks_bounds() {
		for ordered in [false, true] {
			let producer = track_producer("test", Info::timed());
			let mut subscriber = producer.subscribe(replay());
			for sequence in [2, 1] {
				producer
					.create_group(group::Info { sequence })
					.unwrap()
					.finish()
					.unwrap();
			}
			let waiter = kio::Waiter::noop();
			let first = if ordered { 1 } else { 2 };
			for _ in 0..2 {
				assert_eq!(
					subscriber.poll_peek_group(ordered, &waiter).map(Result::unwrap),
					Poll::Ready(Some(first))
				);
			}
			let Poll::Ready(Ok(Some(group))) = subscriber.poll_group(ordered, &waiter) else {
				panic!("the peeked group is still available");
			};
			assert_eq!(group.sequence, first);
			let second = if ordered { 2 } else { 1 };
			subscriber.set_range(..0);
			assert!(subscriber.poll_peek_group(ordered, &waiter).is_pending());
			subscriber.set_range(..);
			assert_eq!(
				subscriber.poll_peek_group(ordered, &waiter).map(Result::unwrap),
				Poll::Ready(Some(second))
			);
		}
	}

	#[test]
	#[cfg(feature = "async")]
	fn ordered_and_arrival_cursors_are_independent() {
		let producer = track_producer("test", Info::timed());
		let mut ordered = producer.subscribe(replay()).ordered();
		let mut arrival = producer.subscribe(replay());

		// Out-of-order arrivals: seq 5 first, then seq 3.
		producer
			.create_group(group::Info { sequence: 5 })
			.unwrap()
			.finish()
			.unwrap();
		producer
			.create_group(group::Info { sequence: 3 })
			.unwrap()
			.finish()
			.unwrap();

		// The ordered handle returns the smallest sequence first, regardless of
		// arrival order.
		let group = ordered
			.next_group()
			.now_or_never()
			.expect("should not block")
			.expect("would have errored")
			.expect("track should not be closed");
		assert_eq!(group.sequence, 3);

		// The plain handle walks arrivals, so it still starts at seq 5.
		assert_eq!(arrival.assert_group().sequence, 5);
	}

	#[test]
	#[cfg(feature = "async")]
	fn end_at_caps_next_group() {
		let producer = track_producer("test", Info::timed());
		let mut consumer = producer.subscribe(replay()).ordered();

		for s in 0..6 {
			producer
				.create_group(group::Info { sequence: s })
				.unwrap()
				.finish()
				.unwrap();
		}

		consumer.set_range(..3);

		// Groups 0, 1, 2 are within the cap.
		assert_eq!(
			consumer.next_group().now_or_never().unwrap().unwrap().unwrap().sequence,
			0
		);
		assert_eq!(
			consumer.next_group().now_or_never().unwrap().unwrap().unwrap().sequence,
			1
		);
		assert_eq!(
			consumer.next_group().now_or_never().unwrap().unwrap().unwrap().sequence,
			2
		);

		// Group 3 is beyond the cap: next_group parks even though cached groups exist.
		assert!(
			consumer.next_group().now_or_never().is_none(),
			"capped consumer must block instead of returning out-of-range groups"
		);
	}

	#[test]
	#[cfg(feature = "async")]
	fn end_at_release_drains_cached_groups() {
		let producer = track_producer("test", Info::timed());
		let mut consumer = producer.subscribe(replay()).ordered();

		for s in 0..6 {
			producer
				.create_group(group::Info { sequence: s })
				.unwrap()
				.finish()
				.unwrap();
		}

		consumer.set_range(..2);
		assert_eq!(
			consumer.next_group().now_or_never().unwrap().unwrap().unwrap().sequence,
			0
		);
		assert_eq!(
			consumer.next_group().now_or_never().unwrap().unwrap().unwrap().sequence,
			1
		);
		assert!(consumer.next_group().now_or_never().is_none(), "capped at 2");

		// Raise the cap; previously-blocked cached groups become available again.
		consumer.set_range(..5);
		assert_eq!(
			consumer.next_group().now_or_never().unwrap().unwrap().unwrap().sequence,
			2
		);
		assert_eq!(
			consumer.next_group().now_or_never().unwrap().unwrap().unwrap().sequence,
			3
		);
		assert_eq!(
			consumer.next_group().now_or_never().unwrap().unwrap().unwrap().sequence,
			4
		);
		assert!(consumer.next_group().now_or_never().is_none(), "capped at 5");

		// Remove the cap; everything remaining flows.
		consumer.set_range(..);
		assert_eq!(
			consumer.next_group().now_or_never().unwrap().unwrap().unwrap().sequence,
			5
		);
		assert!(consumer.next_group().now_or_never().is_none(), "no more groups");
	}

	#[test]
	#[cfg(feature = "async")]
	fn end_at_lower_than_cursor_parks_consumer() {
		let producer = track_producer("test", Info::timed());
		let mut consumer = producer.subscribe(replay()).ordered();

		for s in 0..3 {
			producer
				.create_group(group::Info { sequence: s })
				.unwrap()
				.finish()
				.unwrap();
		}

		// Drain everything with no cap.
		assert_eq!(
			consumer.next_group().now_or_never().unwrap().unwrap().unwrap().sequence,
			0
		);
		assert_eq!(
			consumer.next_group().now_or_never().unwrap().unwrap().unwrap().sequence,
			1
		);
		assert_eq!(
			consumer.next_group().now_or_never().unwrap().unwrap().unwrap().sequence,
			2
		);

		// Lower the cap below the cursor. New groups beyond the cap are blocked.
		consumer.set_range(..2);
		producer
			.create_group(group::Info { sequence: 3 })
			.unwrap()
			.finish()
			.unwrap();
		producer
			.create_group(group::Info { sequence: 4 })
			.unwrap()
			.finish()
			.unwrap();
		assert!(
			consumer.next_group().now_or_never().is_none(),
			"cap is below cursor; nothing returnable until cap rises"
		);

		// Restoring the cap to no-limit (or any value >= cursor) releases them.
		consumer.set_range(..);
		assert_eq!(
			consumer.next_group().now_or_never().unwrap().unwrap().unwrap().sequence,
			3
		);
		assert_eq!(
			consumer.next_group().now_or_never().unwrap().unwrap().unwrap().sequence,
			4
		);
	}

	#[test]
	#[cfg(feature = "async")]
	fn end_at_toggling_around_late_arrivals() {
		let producer = track_producer("test", Info::timed());
		let mut consumer = producer.subscribe(replay()).ordered();

		consumer.set_range(..6);

		// Out-of-order arrivals all within the cap.
		producer
			.create_group(group::Info { sequence: 2 })
			.unwrap()
			.finish()
			.unwrap();
		producer
			.create_group(group::Info { sequence: 5 })
			.unwrap()
			.finish()
			.unwrap();
		producer
			.create_group(group::Info { sequence: 3 })
			.unwrap()
			.finish()
			.unwrap();
		// One beyond the cap; should be held even though it arrived in the middle.
		producer
			.create_group(group::Info { sequence: 8 })
			.unwrap()
			.finish()
			.unwrap();
		producer
			.create_group(group::Info { sequence: 4 })
			.unwrap()
			.finish()
			.unwrap();

		// next_group walks in sequence order through everything <= cap.
		assert_eq!(
			consumer.next_group().now_or_never().unwrap().unwrap().unwrap().sequence,
			2
		);
		assert_eq!(
			consumer.next_group().now_or_never().unwrap().unwrap().unwrap().sequence,
			3
		);
		assert_eq!(
			consumer.next_group().now_or_never().unwrap().unwrap().unwrap().sequence,
			4
		);
		assert_eq!(
			consumer.next_group().now_or_never().unwrap().unwrap().unwrap().sequence,
			5
		);
		// Now blocked: 8 is still beyond the cap.
		assert!(consumer.next_group().now_or_never().is_none());

		// Raise the cap; cached seq 8 is finally served.
		consumer.set_range(..11);
		assert_eq!(
			consumer.next_group().now_or_never().unwrap().unwrap().unwrap().sequence,
			8
		);
	}

	/// `recv_group` (arrival order) honors the `end_at` cap like `next_group`: a group
	/// beyond it is passed, holding nothing, and a raised cap finds it in the cache again.
	/// Once the track finishes, a capped reader ends with what it could read.
	#[test]
	#[cfg(feature = "async")]
	fn end_at_caps_recv_group() {
		let producer = track_producer("test", Info::timed());
		let mut consumer = producer.subscribe(replay());

		for s in 0..3 {
			producer
				.create_group(group::Info { sequence: s })
				.unwrap()
				.finish()
				.unwrap();
		}

		consumer.set_range(..2);
		assert_eq!(
			consumer.recv_group().now_or_never().unwrap().unwrap().unwrap().sequence,
			0
		);
		assert_eq!(
			consumer.recv_group().now_or_never().unwrap().unwrap().unwrap().sequence,
			1
		);
		assert!(consumer.recv_group().now_or_never().is_none(), "capped at 2");

		consumer.set_range(..);
		assert_eq!(
			consumer.recv_group().now_or_never().unwrap().unwrap().unwrap().sequence,
			2
		);

		consumer.set_range(..3);
		let _open = producer.create_group(group::Info { sequence: 3 }).unwrap();
		assert!(consumer.recv_group().now_or_never().is_none(), "capped at 3");
		producer.finish().unwrap();
		assert!(
			matches!(consumer.recv_group().now_or_never(), Some(Ok(None))),
			"a finished track ends a capped reader"
		);
	}

	/// A group beyond the cap must not block in-range groups that arrive behind
	/// it: a relay can ingest a burst micro-reordered (newest first).
	#[test]
	#[cfg(feature = "async")]
	fn recv_group_serves_arrivals_behind_the_cap() {
		let producer = track_producer("test", Info::timed());
		let mut consumer = producer.subscribe(replay());

		consumer.set_range(..2);

		// Reordered burst: the beyond-cap group arrives first.
		producer
			.create_group(group::Info { sequence: 2 })
			.unwrap()
			.finish()
			.unwrap();
		producer
			.create_group(group::Info { sequence: 0 })
			.unwrap()
			.finish()
			.unwrap();
		producer
			.create_group(group::Info { sequence: 1 })
			.unwrap()
			.finish()
			.unwrap();

		assert_eq!(
			consumer.recv_group().now_or_never().unwrap().unwrap().unwrap().sequence,
			0
		);
		assert_eq!(
			consumer.recv_group().now_or_never().unwrap().unwrap().unwrap().sequence,
			1
		);
		assert!(consumer.recv_group().now_or_never().is_none(), "capped at 2");

		consumer.set_range(..3);
		assert_eq!(
			consumer.recv_group().now_or_never().unwrap().unwrap().unwrap().sequence,
			2
		);
	}

	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn group_ranges_preserve_the_floor_when_the_cap_changes() {
		let producer = track_producer("test", Info::timed());
		let mut consumer = producer.subscribe(None);
		consumer.set_range(2..=2);
		producer
			.create_group(group::Info { sequence: 2 })
			.unwrap()
			.finish()
			.unwrap();
		assert_eq!(consumer.recv_group().await.unwrap().unwrap().sequence, 2);
		consumer.set_range(..4);
		producer
			.create_group(group::Info { sequence: 1 })
			.unwrap()
			.finish()
			.unwrap();
		producer
			.create_group(group::Info { sequence: 3 })
			.unwrap()
			.finish()
			.unwrap();
		assert_eq!(consumer.recv_group().await.unwrap().unwrap().sequence, 3);
		consumer.set_range(0..=4);
		producer
			.create_group(group::Info { sequence: 0 })
			.unwrap()
			.finish()
			.unwrap();
		producer
			.create_group(group::Info { sequence: 4 })
			.unwrap()
			.finish()
			.unwrap();
		assert_eq!(consumer.recv_group().await.unwrap().unwrap().sequence, 4);
	}

	/// A raised `start_at` drops parked groups it overtook instead of re-offering
	/// them once the cap rises.
	#[test]
	#[cfg(feature = "async")]
	fn a_raised_floor_drops_parked_recv_groups() {
		let producer = track_producer("test", Info::timed());
		let mut consumer = producer.subscribe(None);

		consumer.set_range(..1);
		producer
			.create_group(group::Info { sequence: 1 })
			.unwrap()
			.finish()
			.unwrap();
		assert!(
			consumer.recv_group().now_or_never().is_none(),
			"group 1 parked at the cap"
		);

		consumer.set_range(2..);
		producer
			.create_group(group::Info { sequence: 2 })
			.unwrap()
			.finish()
			.unwrap();
		assert_eq!(
			consumer.recv_group().now_or_never().unwrap().unwrap().unwrap().sequence,
			2,
			"the overtaken parked group is dropped, not re-offered"
		);
	}

	/// A parked group the producer aborts (eviction/expiry) is dropped: it is
	/// neither delivered once the cap rises nor allowed to hold the stream open
	/// after the track finishes. This is what bounds parking by the cache policy.
	#[test]
	#[cfg(feature = "async")]
	fn evicted_parked_recv_groups_are_dropped() {
		let producer = track_producer("test", Info::timed());
		let mut consumer = producer.subscribe(None);

		producer
			.create_group(group::Info { sequence: 0 })
			.unwrap()
			.finish()
			.unwrap();
		assert_eq!(
			consumer.recv_group().now_or_never().unwrap().unwrap().unwrap().sequence,
			0
		);

		consumer.set_range(..1);
		let straggler = producer.create_group(group::Info { sequence: 1 }).unwrap();
		assert!(
			consumer.recv_group().now_or_never().is_none(),
			"group 1 parked at the cap"
		);

		// The cache evicts the parked group (abort-as-tombstone), then the track ends.
		straggler.abort(Error::Old).unwrap();
		producer.finish().unwrap();

		consumer.set_range(..);
		assert!(
			matches!(consumer.recv_group().now_or_never(), Some(Ok(None))),
			"a dead parked group must not be delivered or hold the stream open"
		);
	}

	/// An exclusive cap at 0 is the empty range: no group is delivered, even group 0.
	#[test]
	#[cfg(feature = "async")]
	fn end_at_zero_is_the_empty_range() {
		let producer = track_producer("test", Info::timed());
		let mut consumer = producer.subscribe(replay());
		producer
			.create_group(group::Info { sequence: 0 })
			.unwrap()
			.finish()
			.unwrap();
		producer
			.create_group(group::Info { sequence: 1 })
			.unwrap()
			.finish()
			.unwrap();

		consumer.set_range(..0);
		assert!(
			consumer.recv_group().now_or_never().is_none(),
			"empty cap delivers nothing"
		);

		consumer.set_range(..1);
		assert_eq!(
			consumer.recv_group().now_or_never().unwrap().unwrap().unwrap().sequence,
			0
		);
		assert!(consumer.recv_group().now_or_never().is_none(), "group 1 stays parked");
	}

	/// A requested empty range plus a local empty cap still parks while another
	/// subscriber keeps aggregate demand unbounded.
	#[test]
	#[cfg(feature = "async")]
	fn empty_local_cap_holds_while_another_subscriber_requests_everything() {
		let producer = track_producer("test", Info::timed());
		let mut everything = producer.subscribe(replay());
		let mut empty = producer.subscribe(replay().with_order(Order::Ascending));
		empty.set_range((Bound::Unbounded, Position::group(0).group_end()));

		for s in 0..3 {
			producer
				.create_group(group::Info { sequence: s })
				.unwrap()
				.finish()
				.unwrap();
		}

		assert_eq!(
			everything
				.recv_group()
				.now_or_never()
				.unwrap()
				.unwrap()
				.unwrap()
				.sequence,
			0
		);
		assert!(
			empty.recv_group().now_or_never().is_none(),
			"local empty cap must not ride the unbounded aggregate"
		);

		empty.set_range(..2);
		assert_eq!(empty.recv_group().now_or_never().unwrap().unwrap().unwrap().sequence, 0);
		assert_eq!(empty.recv_group().now_or_never().unwrap().unwrap().unwrap().sequence, 1);
		assert!(empty.recv_group().now_or_never().is_none(), "still capped at 2");

		empty.set_range(..);
		assert_eq!(empty.recv_group().now_or_never().unwrap().unwrap().unwrap().sequence, 2);
	}

	/// A frame-limited exclusive end includes the last group and stops before that frame.
	#[test]
	#[cfg(feature = "async")]
	fn end_at_frame_limited_last_group() {
		let producer = track_producer("test", Info::timed());
		let mut consumer = producer.subscribe(replay()).ordered();
		let end = Position::after(1, 1).unwrap();
		consumer.set_range((Bound::Unbounded, end.group_end()));

		for s in 0..3u64 {
			let mut group = producer.create_group(group::Info { sequence: s }).unwrap();
			for i in 0..3u8 {
				group.write_frame(Timestamp::ZERO, vec![i]).unwrap();
			}
			group.finish().unwrap();
		}

		assert_eq!(
			consumer.next_group().now_or_never().unwrap().unwrap().unwrap().sequence,
			0
		);
		let mut last = consumer.next_group().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(last.sequence, 1);
		last = last.with_range(..end.frame);
		assert_eq!(
			last.read_frame().now_or_never().unwrap().unwrap().unwrap().payload[0],
			0
		);
		assert_eq!(
			last.read_frame().now_or_never().unwrap().unwrap().unwrap().payload[0],
			1
		);
		assert!(
			last.read_frame().now_or_never().unwrap().unwrap().is_none(),
			"frame cap is exclusive"
		);
		assert!(
			consumer.next_group().now_or_never().is_none(),
			"group 2 is past the exclusive group cap"
		);
	}

	/// An inclusive bound at the last group withholds nothing.
	#[test]
	#[cfg(feature = "async")]
	fn end_at_maximum_group_is_unbounded() {
		let producer = track_producer("test", Info::timed());
		let mut consumer = producer.subscribe(replay()).ordered();
		consumer.set_range(..=u64::MAX);

		producer
			.create_group(group::Info { sequence: 0 })
			.unwrap()
			.finish()
			.unwrap();
		producer
			.create_group(group::Info { sequence: u64::MAX })
			.unwrap()
			.finish()
			.unwrap();

		assert_eq!(
			consumer.next_group().now_or_never().unwrap().unwrap().unwrap().sequence,
			0
		);
		assert_eq!(
			consumer.next_group().now_or_never().unwrap().unwrap().unwrap().sequence,
			u64::MAX
		);
		// Nothing can follow the maximum sequence.
		assert!(consumer.next_group().now_or_never().unwrap().unwrap().is_none());
	}

	#[test]
	fn write_frame_rejects_an_oversized_frame_before_appending_its_group() {
		let mut producer = track_producer("test", Info::timed());
		let frame = bytes::Bytes::from(vec![0; group::MAX_CACHE_BYTES as usize + 1]);

		assert!(matches!(
			producer.write_frame(Timestamp::ZERO, frame),
			Err(Error::FrameTooLarge)
		));
		assert_eq!(
			producer.latest_sequence(),
			None,
			"the rejected frame did not publish a group"
		);
	}

	#[test]
	fn append_group_returns_bounds_exceeded_on_sequence_overflow() {
		let producer = track_producer("test", Info::timed());
		{
			let mut state = producer.state.write().ok().unwrap();
			state.max_sequence = Some(u64::MAX);
		}

		assert!(matches!(producer.append_group(), Err(Error::BoundsExceeded(_))));
	}

	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn fetch_cache_hit() {
		let producer = track_producer("test", Info::timed());

		// Produce a cached group.
		let mut group = producer.append_group().unwrap(); // seq 0
		group
			.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"hello"))
			.unwrap();
		group.finish().unwrap();

		// A cached group resolves immediately and never queues a request. `peek_group`
		// also returns it synchronously.
		let dynamic = producer.requests();
		let consumer = producer.consume();
		assert!(consumer.peek_group(0).is_some());
		let mut g = consumer.fetch_group(0, None).await.unwrap();
		assert_eq!(g.sequence, 0);
		assert_eq!(&g.read_frame().await.unwrap().unwrap().payload[..], b"hello");

		// Nothing was queued for the dynamic handler to serve.
		assert!(dynamic.poll_requested_fill_group(&kio::Waiter::noop()).is_pending());
	}

	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn fetch_miss_signals_dynamic() {
		let producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		let consumer = producer.consume();

		// A cache miss isn't in `peek_group`, but a dynamic handler exists, so
		// `fetch_group` stays pending and queues a request. `*pending` derefs the
		// wrapper to the inner `Fetching` (a `kio::Task`).
		assert!(consumer.peek_group(5).is_none());
		let mut pending = consumer.fetch_group(5, group::Fetch::default().with_priority(7));
		assert!(kio::Task::poll(&mut *pending, &kio::Waiter::noop()).is_pending());

		let req = dynamic
			.requested_fill_group()
			.now_or_never()
			.expect("should not block")
			.unwrap();
		assert_eq!(req.sequence(), 5);
		assert_eq!(req.priority(), 7);

		// Serve it by accepting the request; the fetch then resolves.
		let mut group = req.accept(Info::timed()).unwrap();
		group
			.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"hi"))
			.unwrap();
		group.finish().unwrap();

		let mut g = pending.await.unwrap();
		assert_eq!(g.sequence, 5);
		assert_eq!(&g.read_frame().await.unwrap().unwrap().payload[..], b"hi");
	}

	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn fetch_miss_rejects() {
		let producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		let consumer = producer.consume();

		let pending = consumer.fetch_group(5, None);
		let req = dynamic
			.requested_fill_group()
			.now_or_never()
			.expect("should not block")
			.unwrap();

		req.reject(Error::Cancel);
		assert!(matches!(pending.await, Err(Error::Cancel)));
		let fetch = producer.state.read().fills.clone();
		assert!(fetch.read().is_empty());
	}

	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn fetch_miss_drop_rejects() {
		let producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		let consumer = producer.consume();

		let pending = consumer.fetch_group(5, None);
		let req = dynamic
			.requested_fill_group()
			.now_or_never()
			.expect("should not block")
			.unwrap();

		drop(req);
		assert!(matches!(pending.await, Err(Error::Dropped)));
	}

	/// A request stays wanted while any joined fetch waits, and once the last leaves it
	/// is withdrawn: a later fetch starts a fresh request instead of joining it.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn fetch_request_unused_once_every_fetch_leaves() {
		let producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		let consumer = producer.consume();

		let first = consumer.fetch_group(5, None);
		let req = dynamic
			.requested_fill_group()
			.now_or_never()
			.expect("should not block")
			.unwrap();
		let second = consumer.fetch_group(5, None);

		drop(first);
		assert!(req.poll_unwanted(&kio::Waiter::noop()).is_pending());
		drop(second);
		assert!(req.poll_unwanted(&kio::Waiter::noop()).is_ready());

		let retry = consumer.fetch_group(5, None);
		let fresh = dynamic
			.requested_fill_group()
			.now_or_never()
			.expect("the retry queues a fresh request")
			.unwrap();
		drop(req);
		assert!(fresh.poll_unwanted(&kio::Waiter::noop()).is_pending());
		fresh.accept(Info::timed()).unwrap().finish().unwrap();
		assert_eq!(retry.await.unwrap().sequence, 5);
	}

	/// A fetch abandoned before any handler took it is withdrawn from the queue too, so
	/// no handler serves it.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn fetch_abandoned_while_queued_never_reaches_the_handler() {
		let producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		let consumer = producer.consume();

		drop(consumer.fetch_group(5, None));
		assert!(dynamic.requested_fill_group().now_or_never().is_none());
		let fetch = producer.state.read().fills.clone();
		assert!(fetch.read().is_empty());
	}

	/// The withdrawal happens as the last caller leaves, not when the handler notices: a
	/// fetch arriving before the handler drops the abandoned request is not failed with it.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn fetch_after_the_last_caller_left_is_not_dropped() {
		let producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		let consumer = producer.consume();

		let first = consumer.fetch_group(5, None);
		let req = dynamic
			.requested_fill_group()
			.now_or_never()
			.expect("should not block")
			.unwrap();
		drop(first);

		let mut retry = consumer.fetch_group(5, None);
		drop(req);
		assert!(kio::Task::poll(&mut *retry, &kio::Waiter::noop()).is_pending());

		let fresh = dynamic
			.requested_fill_group()
			.now_or_never()
			.expect("the retry queues a fresh request")
			.unwrap();
		fresh.accept(Info::timed()).unwrap().finish().unwrap();
		assert_eq!(retry.await.unwrap().sequence, 5);
	}

	/// A handler that accepts after every caller left still caches the group, so a fetch
	/// that queued a fresh request meanwhile resolves from it and that request is moot.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn fetch_accept_after_withdrawal_caches_the_group() {
		let producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		let consumer = producer.consume();

		let first = consumer.fetch_group(5, None);
		let req = dynamic
			.requested_fill_group()
			.now_or_never()
			.expect("should not block")
			.unwrap();
		drop(first);

		let retry = consumer.fetch_group(5, None);
		let fresh = dynamic
			.requested_fill_group()
			.now_or_never()
			.expect("the retry queues a fresh request")
			.unwrap();

		let mut group = req.accept(Info::timed()).expect("a withdrawn request still accepts");
		group
			.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"hi"))
			.unwrap();
		group.finish().unwrap();

		let mut fetched = retry.await.unwrap();
		assert_eq!(&fetched.read_frame().await.unwrap().unwrap().payload[..], b"hi");
		assert!(!fresh.is_wanted(), "the retry left the fresh request");
		assert!(matches!(fresh.accept(Info::timed()), Err(Error::Duplicate)));
	}

	/// Dropping an auto trait from a published type is a semver break, so the group
	/// consumer a cached fetch holds must not cost `Fetching` its unwind safety.
	#[test]
	fn fetching_is_unwind_safe() {
		fn assert_unwind_safe<T: std::panic::UnwindSafe + std::panic::RefUnwindSafe>() {}
		assert_unwind_safe::<Fetching>();
		assert_unwind_safe::<group::Consumer>();
	}

	/// A fetch that hits the cache holds the group until polled, so a handler that aborts
	/// the group once nobody wants it (an abandoned upstream fetch) leaves it alone.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn fetch_hit_keeps_the_group_wanted() {
		let producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		let consumer = producer.consume();

		let first = consumer.fetch_group(5, None);
		let req = dynamic
			.requested_fill_group()
			.now_or_never()
			.expect("should not block")
			.unwrap();
		let mut group = req.accept(Info::timed()).unwrap();
		group
			.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"head"))
			.unwrap();
		drop(first.await.unwrap());

		let hit = consumer.fetch_group(5, None);
		assert!(
			!group.handle().abort_unwanted(Error::Cancel),
			"the pending hit wants the group"
		);
		let mut fetched = hit.await.unwrap();
		assert_eq!(&fetched.read_frame().await.unwrap().unwrap().payload[..], b"head");
	}

	/// A bounded FIN is local to its request, never proof that the original group ended.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn bounded_transport_copies_never_answer_whole_fetches() {
		let producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		let consumer = producer.consume();
		let request = |start, end| FetchOptions {
			options: group::Fetch {
				frame_start: start,
				..Default::default()
			},
			end,
		};
		let bounded = consumer.fetch_range(0, request(2, Some(4)));
		let fill = dynamic.requested_fill_group().await.unwrap();
		assert_eq!((fill.frame_start(), fill.frame_end()), (0, Some(4)));
		let mut prefix = fill.accept(Info::timed()).unwrap();
		for _ in 0..4 {
			prefix.write_frame(Timestamp::ZERO, b"prefix".as_slice()).unwrap();
		}
		prefix.finish().unwrap();
		let mut bounded = bounded.await.unwrap().with_range(2..).with_end(Some(4));
		for expected in 2..4 {
			assert_eq!(bounded.index(), expected);
			assert!(bounded.read_frame().await.unwrap().is_some());
		}
		assert!(bounded.read_frame().await.unwrap().is_none());
		assert!(
			consumer.cached_groups(0..1).is_empty(),
			"a bounded prefix is no whole cached group"
		);
		// A narrower bound reuses the same stored prefix.
		assert!(
			consumer
				.fetch_range(0, request(1, Some(3)))
				.now_or_never()
				.unwrap()
				.is_ok()
		);
		let mut whole = consumer.fetch_group(0, None);
		assert!(kio::Task::poll(&mut *whole, &kio::Waiter::noop()).is_pending());
		let fill = dynamic.requested_fill_group().await.unwrap();
		assert_eq!((fill.frame_start(), fill.frame_end()), (0, None));
		let mut complete = fill.accept(Info::timed()).unwrap();
		for _ in 0..6 {
			complete.write_frame(Timestamp::ZERO, b"prefix".as_slice()).unwrap();
		}
		complete.finish().unwrap();
		let mut whole = whole.await.unwrap();
		for _ in 0..6 {
			assert!(whole.read_frame().await.unwrap().is_some());
		}
		assert!(whole.read_frame().await.unwrap().is_none());
		assert_eq!(producer.state.read().lookup[&0].copies().count(), 1);
	}

	/// Tail starts replace one backfill slot rather than retaining a copy per peer range.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn bounded_tail_refills_coalesce_behind_a_live_copy() {
		let producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		let consumer = producer.consume();
		let mut live = producer.create_group(group::Info { sequence: 0 }).unwrap();
		live.start_at(200).unwrap();
		live.write_frame(Timestamp::ZERO, b"live".as_slice()).unwrap();
		for start in (1..100).rev() {
			let fetched = consumer.fetch_range(
				0,
				FetchOptions {
					options: group::Fetch {
						frame_start: start,
						..Default::default()
					},
					end: Some(start + 1),
				},
			);
			let fill = dynamic.requested_fill_group().await.unwrap();
			assert_eq!((fill.frame_start(), fill.frame_end()), (start, Some(start + 1)));
			let mut tail = fill.accept(Info::timed()).unwrap();
			tail.write_frame(Timestamp::ZERO, b"tail".as_slice()).unwrap();
			tail.finish().unwrap();
			assert_eq!(fetched.await.unwrap().index(), start);
			assert_eq!(producer.state.read().lookup[&0].copies().count(), 2);
		}
		let mut sub = producer.subscribe(None);
		let group = sub.recv_group().await.unwrap().unwrap();
		assert_eq!(
			group.with_range(0..).index(),
			200,
			"a bounded head does not replace the live feed for whole-group readers"
		);
		live.abort(Error::Cancel).unwrap();
		// The backfill remains cache-owned after the live copy failed.
		assert!(!producer.state.read().lookup[&0].is_aborted());
	}

	/// A bounded fill that a later, disjoint one displaces from the cache still answers the
	/// fetch that asked for it.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn a_displaced_bounded_fill_still_answers_its_fetch() {
		let producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		let consumer = producer.consume();
		let mut live = producer.create_group(group::Info { sequence: 0 }).unwrap();
		live.start_at(200).unwrap();
		live.write_frame(Timestamp::ZERO, b"live".as_slice()).unwrap();
		let range = |start, end| FetchOptions {
			options: group::Fetch {
				frame_start: start,
				..Default::default()
			},
			end: Some(end),
		};
		let fill = async |frames: std::ops::Range<u64>| {
			let fill = dynamic.requested_fill_group().await.unwrap();
			assert_eq!((fill.frame_start(), fill.frame_end()), (frames.start, Some(frames.end)));
			let mut group = fill.accept(Info::timed()).unwrap();
			for _ in frames {
				group.write_frame(Timestamp::ZERO, b"tail".as_slice()).unwrap();
			}
			group.finish().unwrap();
		};
		let later = consumer.fetch_range(0, range(4, 10));
		fill(4..10).await;
		let earlier = consumer.fetch_range(0, range(0, 3));
		fill(0..3).await;
		assert_eq!(earlier.await.unwrap().index(), 0);
		assert_eq!(later.await.unwrap().index(), 4);
	}

	/// A copy cut short at the back never displaces a fetched one that reaches the group's
	/// end, so whole-group readers keep it, and it still answers its own fetch.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn a_bounded_fill_never_displaces_a_whole_tail() {
		let producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		let consumer = producer.consume();
		let mut live = producer.create_group(group::Info { sequence: 0 }).unwrap();
		live.start_at(200).unwrap();
		live.write_frame(Timestamp::ZERO, b"live".as_slice()).unwrap();
		let fill = async |start: u64, end: Option<u64>| {
			let fill = dynamic.requested_fill_group().await.unwrap();
			assert_eq!((fill.frame_start(), fill.frame_end()), (start, end));
			let mut group = fill.accept(Info::timed()).unwrap();
			group.write_frame(Timestamp::ZERO, b"tail".as_slice()).unwrap();
			group
		};
		let tail = consumer.fetch_group(
			0,
			group::Fetch {
				frame_start: 5,
				..Default::default()
			},
		);
		let _tail = fill(5, None).await;
		let head = consumer.fetch_range(
			0,
			FetchOptions {
				options: group::Fetch::default(),
				end: Some(1),
			},
		);
		fill(0, Some(1)).await.finish().unwrap();
		assert_eq!(head.await.unwrap().index(), 0);
		assert_eq!(tail.await.unwrap().index(), 5);
		let mut sub = producer.subscribe(None);
		assert_eq!(sub.assert_group().with_range(0..).index(), 5);
	}

	/// A track abort ends every copy still being written, including one the cache never held
	/// or has since displaced: its reader gets the abort and its writer fails.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn a_track_abort_ends_uncached_copies() {
		let producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		let consumer = producer.consume();
		let range = |start, end| FetchOptions {
			options: group::Fetch {
				frame_start: start,
				..Default::default()
			},
			end,
		};
		let fill = async |frames: FetchOptions| {
			let fill = dynamic.requested_fill_group().await.unwrap();
			assert_eq!(
				(fill.frame_start(), fill.frame_end()),
				(frames.options.frame_start, frames.end)
			);
			let mut group = fill.accept(Info::timed()).unwrap();
			group.write_frame(Timestamp::ZERO, b"copy".as_slice()).unwrap();
			group
		};
		let fetch = async |sequence, frames: FetchOptions| {
			let pending = consumer.fetch_range(sequence, frames.clone());
			let held = fill(frames).await;
			(held, pending.await.unwrap())
		};
		let mut lives = Vec::new();
		for sequence in 0..2 {
			let mut live = producer.create_group(group::Info { sequence }).unwrap();
			live.start_at(200).unwrap();
			live.write_frame(Timestamp::ZERO, b"live".as_slice()).unwrap();
			lives.push(live);
		}
		// A bounded copy beside a cached whole tail never enters the cache.
		let _tail = fetch(0, range(5, None)).await;
		let kept = fetch(0, range(0, Some(3))).await;
		// A later copy displaces a live group's bounded head, and a fetched-only bounded copy.
		let head = fetch(1, range(4, Some(10))).await;
		let _wider = fetch(1, range(0, Some(3))).await;
		let fetched = fetch(2, range(0, Some(3))).await;
		let _wider = fetch(2, range(0, None)).await;
		let mut detached = [kept, head, fetched];
		for (_, fetched) in &mut detached {
			fetched.read_frame().await.unwrap().unwrap();
		}

		producer.abort(Error::Cancel).unwrap();
		for (mut held, mut fetched) in detached {
			assert!(matches!(fetched.read_frame().now_or_never(), Some(Err(Error::Cancel))));
			assert!(held.write_frame(Timestamp::ZERO, b"late".as_slice()).is_err());
		}
	}

	/// A track abort keeps a finished bounded copy for the reader still draining it, as it
	/// does a finished whole group.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn a_track_abort_keeps_a_finished_bounded_copy() {
		let producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		let consumer = producer.consume();
		let fetch = consumer.fetch_range(
			0,
			FetchOptions {
				options: group::Fetch::default(),
				end: Some(2),
			},
		);
		let mut group = dynamic
			.requested_fill_group()
			.await
			.unwrap()
			.accept(Info::timed())
			.unwrap();
		for _ in 0..2 {
			group.write_frame(Timestamp::ZERO, b"copy".as_slice()).unwrap();
		}
		group.finish().unwrap();
		let mut fetched = fetch.await.unwrap();

		producer.abort(Error::Cancel).unwrap();
		assert!(fetched.read_frame().await.unwrap().is_some());
		assert!(fetched.read_frame().await.unwrap().is_some());
		assert!(fetched.read_frame().await.unwrap().is_none());
	}

	/// A finished bounded copy below the declared end is nothing in flight, so an abort after
	/// the end is reached still ends the track cleanly.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn a_finished_bounded_copy_lets_the_end_settle() {
		let mut producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		let consumer = producer.consume();
		let fetch = consumer.fetch_range(
			0,
			FetchOptions {
				options: group::Fetch::default(),
				end: Some(1),
			},
		);
		let fill = dynamic.requested_fill_group().await.unwrap();
		assert_eq!(fill.frame_end(), Some(1));
		let mut group = fill.accept(Info::timed()).unwrap();
		group.write_frame(Timestamp::ZERO, b"copy".as_slice()).unwrap();
		// The end is reached while the copy is still being written, so only the abort
		// can find it settled.
		producer
			.create_group(group::Info { sequence: 1 })
			.unwrap()
			.finish()
			.unwrap();
		producer.finish_at(Position::group(2)).unwrap();
		group.finish().unwrap();
		fetch.await.unwrap();

		producer.abort(Error::Timeout).unwrap();
		assert!(matches!(
			consumer.poll_complete(&kio::Waiter::noop()),
			Poll::Ready(Ok(()))
		));
	}

	/// A copy cut short at the back never answers a whole-group reader. A live group the
	/// publisher cut is still handed out as that cut, and one it cancelled stays a hole,
	/// whatever bounded copy of it remains.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn a_back_cut_copy_never_answers_a_whole_group_reader() {
		let producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		let consumer = producer.consume();
		let bounded = async |sequence: u64| {
			let fetch = consumer.fetch_range(
				sequence,
				FetchOptions {
					options: group::Fetch::default(),
					end: Some(1),
				},
			);
			let mut group = dynamic
				.requested_fill_group()
				.await
				.unwrap()
				.accept(Info::timed())
				.unwrap();
			group.write_frame(Timestamp::ZERO, b"head".as_slice()).unwrap();
			group.finish().unwrap();
			fetch.await.unwrap();
		};
		let mut arrival = producer.subscribe(replay());
		let mut ordered = producer.subscribe(replay()).ordered();
		for (sequence, err) in [(0, Error::App(5)), (1, Error::Cancel)] {
			let mut live = producer.create_group(group::Info { sequence }).unwrap();
			live.start_at(200).unwrap();
			live.write_frame(Timestamp::ZERO, b"live".as_slice()).unwrap();
			bounded(sequence).await;
			live.abort(err).unwrap();
		}
		// Fetched only, so it reaches the ordered reader alone.
		bounded(2).await;

		let cut = arrival.assert_group();
		assert_eq!(cut.sequence, 0);
		assert!(matches!(cut.with_range(200..).read_frame().await, Err(Error::App(5))));
		arrival.assert_no_group();
		let cut = ordered.next_group().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(cut.sequence, 0);
		assert!(matches!(cut.with_range(200..).read_frame().await, Err(Error::App(5))));
		assert!(
			ordered.next_group().now_or_never().is_none(),
			"neither the cancelled group nor a copy ending short of its group is read"
		);
	}

	/// Queued ranges widen before dispatch; a taken cap does not promise an unseen suffix.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn bounded_fetches_widen_before_dispatch_and_split_after() {
		let producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		let consumer = producer.consume();
		let request = |end| FetchOptions {
			options: group::Fetch::default(),
			end: Some(end),
		};
		let _a = consumer.fetch_range(0, request(3));
		let _b = consumer.fetch_range(0, request(5));
		let first = dynamic.requested_fill_group().await.unwrap();
		assert_eq!(first.frame_end(), Some(5));
		let _c = consumer.fetch_range(0, request(8));
		let second = dynamic.requested_fill_group().await.unwrap();
		assert_eq!((first.frame_end(), second.frame_end()), (Some(5), Some(8)));
	}

	/// An accepted group starts where its request did before anyone can see it, so a
	/// wider fetch arriving before the handler writes misses instead of being handed a
	/// reader that would skip the head it asked for.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn accepted_group_starts_at_the_request() {
		let producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		let consumer = producer.consume();

		let _tail = consumer.fetch_group(
			5,
			group::Fetch {
				frame_start: 3,
				..Default::default()
			},
		);
		let req = dynamic
			.requested_fill_group()
			.now_or_never()
			.expect("should not block")
			.unwrap();
		let _group = req.accept(Info::timed()).unwrap();

		let mut whole = consumer.fetch_group(5, None);
		let req = dynamic
			.requested_fill_group()
			.now_or_never()
			.expect("the wider fetch misses and queues its own request")
			.unwrap();
		assert_eq!(req.frame_start(), 0);
		assert!(kio::Task::poll(&mut *whole, &kio::Waiter::noop()).is_pending());
	}

	/// Once a group nobody wanted is aborted, a later fetch misses rather than reading
	/// the abort, and queues a fresh request.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn fetch_misses_an_abandoned_group() {
		let producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		let consumer = producer.consume();

		let first = consumer.fetch_group(5, None);
		let req = dynamic
			.requested_fill_group()
			.now_or_never()
			.expect("should not block")
			.unwrap();
		let group = req.accept(Info::timed()).unwrap();
		drop(first.await.unwrap());
		assert!(group.handle().abort_unwanted(Error::Cancel));

		let retry = consumer.fetch_group(5, None);
		let req = dynamic
			.requested_fill_group()
			.now_or_never()
			.expect("the retry queues a fresh request")
			.unwrap();
		req.accept(Info::timed()).unwrap().finish().unwrap();
		assert!(retry.await.unwrap().read_frame().await.unwrap().is_none());
	}

	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn fetch_reject_does_not_poison_retry() {
		let producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		let consumer = producer.consume();

		let pending = consumer.fetch_group(5, None);
		let req = dynamic
			.requested_fill_group()
			.now_or_never()
			.expect("should not block")
			.unwrap();
		req.reject(Error::Cancel);
		assert!(matches!(pending.await, Err(Error::Cancel)));

		let retry = consumer.fetch_group(5, None);
		let req = dynamic
			.requested_fill_group()
			.now_or_never()
			.expect("should not block")
			.unwrap();
		let mut group = req.accept(Info::timed()).unwrap();
		group
			.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"retry"))
			.unwrap();
		group.finish().unwrap();

		let mut group = retry.await.unwrap();
		assert_eq!(&group.read_frame().await.unwrap().unwrap().payload[..], b"retry");
	}

	/// A group cached from a frame-bounded subscription starts partway in. Serving it to
	/// someone who asked for the whole group would silently hand back a tail, so it is a
	/// miss and the fetch goes upstream instead.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn fetch_ignores_a_group_that_starts_too_late() {
		let producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		let consumer = producer.consume();

		// The live subscription resumed mid-group, so only the tail is cached.
		let mut group = producer.create_group(group::Info { sequence: 0 }).unwrap();
		group.start_at(3).unwrap();
		group
			.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"tail"))
			.unwrap();
		group.finish().unwrap();

		// A fetch for the tail is covered and resolves from the cache.
		let fetch = consumer.fetch_group(
			0,
			group::Fetch {
				frame_start: 3,
				..Default::default()
			},
		);
		let cached = fetch.now_or_never().expect("covered by the cache").unwrap();
		assert_eq!(cached.index(), 3);

		// A fetch for the whole group is not, so it queues for a handler rather than
		// resolving to the tail.
		let mut fetch = std::pin::pin!(consumer.fetch_group(0, None));
		assert!(
			futures::poll!(fetch.as_mut()).is_pending(),
			"must not answer from the tail"
		);

		let request = dynamic.requested_fill_group().await.unwrap();
		assert_eq!((request.sequence(), request.frame_start()), (0, 0));

		// Serving it replaces the too-narrow entry rather than colliding with it.
		let mut whole = request.accept(Info::timed()).unwrap();
		whole
			.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"head"))
			.unwrap();
		whole.finish().unwrap();

		let mut served = fetch.await.unwrap();
		assert_eq!(served.index(), 0);
		assert_eq!(
			served.read_frame().await.unwrap().unwrap().payload,
			bytes::Bytes::from_static(b"head")
		);
	}

	/// A fetch for the head of a live group the feed started partway into keeps that group
	/// in arrival order: subscribers get the fetched copy while it lasts, and the live
	/// group again once the fetch is gone.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn fetch_fills_the_head_of_a_live_group() {
		let producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		let consumer = producer.consume();

		let mut live = producer.create_group(group::Info { sequence: 0 }).unwrap();
		live.start_at(1).unwrap();
		live.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"delta"))
			.unwrap();

		let fetch = consumer.fetch_group(0, None);
		let request = dynamic.requested_fill_group().await.unwrap();
		let mut head = request.accept(Info::timed()).unwrap();
		for payload in [&b"snapshot"[..], b"delta"] {
			head.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(payload))
				.unwrap();
		}
		assert_eq!(fetch.await.unwrap().index(), 0);

		// Arrival order hands out the fetched copy, from its first frame.
		let mut sub = producer.subscribe(None);
		let mut group = sub.recv_group().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(
			group.read_frame().await.unwrap().unwrap().payload,
			bytes::Bytes::from_static(b"snapshot")
		);

		// An abandoned fetch leaves the live group in its slot, still written by the feed.
		head.abort(Error::Cancel).unwrap();
		live.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"delta2"))
			.unwrap();
		let mut sub = producer.subscribe(None);
		let group = sub.recv_group().now_or_never().unwrap().unwrap().unwrap();
		// Positioning at the start clamps up to the live group's first frame.
		let mut group = group.with_range(0..);
		assert_eq!(group.index(), 1);
		assert_eq!(
			group.read_frame().await.unwrap().unwrap().payload,
			bytes::Bytes::from_static(b"delta")
		);
	}

	/// A refused mid-group end caps none of its group's copies, even one that fit under it.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn a_refused_end_leaves_every_copy_uncapped() {
		let mut producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		let consumer = producer.consume();

		// The live copy holds frames 0 and 1, the fetched head frames 0 to 2.
		let mut live = producer.create_group(group::Info { sequence: 0 }).unwrap();
		live.start_at(1).unwrap();
		live.write_frame(Timestamp::ZERO, b"delta".as_slice()).unwrap();
		let fetch = consumer.fetch_group(0, None);
		let mut head = dynamic
			.requested_fill_group()
			.await
			.unwrap()
			.accept(Info::timed())
			.unwrap();
		for index in 0..3 {
			let payload = if index == 0 {
				b"x".as_slice()
			} else {
				b"delta".as_slice()
			};
			head.write_frame(Timestamp::ZERO, payload).unwrap();
		}
		fetch.await.unwrap();

		assert!(matches!(
			producer.finish_at(Position { group: 0, frame: 2 }),
			Err(Error::ProtocolViolation)
		));
		assert_eq!(producer.finished(), None);
		live.write_frame(Timestamp::ZERO, b"delta".as_slice()).unwrap();
	}

	/// A live group with a fetched head: group 0, fed from frame 1, plus a two-frame head.
	#[cfg(feature = "async")]
	async fn live_group_with_head(producer: &Producer) -> (group::Producer, group::Producer) {
		let dynamic = producer.requests();
		let consumer = producer.consume();

		let mut live = producer.create_group(group::Info { sequence: 0 }).unwrap();
		live.start_at(1).unwrap();
		live.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"delta"))
			.unwrap();

		let fetch = consumer.fetch_group(0, None);
		let request = dynamic.requested_fill_group().await.unwrap();
		let mut head = request.accept(Info::timed()).unwrap();
		for payload in [&b"snapshot"[..], b"delta"] {
			head.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(payload))
				.unwrap();
		}
		head.finish().unwrap();
		assert_eq!(fetch.await.unwrap().index(), 0);
		(live, head)
	}

	/// A live group with a fetched head of group 0 still writing, and the track's end
	/// declared past it. The live copy ended as `live` says: aborted with it, else finished.
	#[cfg(feature = "async")]
	async fn live_group_under_a_writing_head(live: Option<Error>) -> (Producer, group::Producer) {
		let mut producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		let consumer = producer.consume();

		let mut group = producer.create_group(group::Info { sequence: 0 }).unwrap();
		group.start_at(1).unwrap();
		group
			.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"delta"))
			.unwrap();
		let _fetch = consumer.fetch_group(0, None);
		let mut head = dynamic
			.requested_fill_group()
			.await
			.unwrap()
			.accept(Info::timed())
			.unwrap();
		for payload in [&b"snapshot"[..], b"delta"] {
			head.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(payload))
				.unwrap();
		}

		match live {
			Some(err) => group.abort(err).unwrap(),
			None => group.finish().unwrap(),
		}
		producer.finish_at(Position::group(1)).unwrap();
		(producer, head)
	}

	/// The fetched head serving a group to its end settles the declared end once it finishes,
	/// whether the live copy beside it was cut, evicted, or finished, and a later abort keeps
	/// that clean end, whether `closed()` saw it first or only the abort did.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn a_fetched_head_settles_the_end_once_it_finishes() {
		use std::sync::atomic::{AtomicBool, Ordering};

		for live in [Some(Error::Cancel), Some(Error::Evicted), None] {
			for polled in [false, true] {
				let (producer, mut head) = live_group_under_a_writing_head(live.clone()).await;
				let woken = Arc::new(AtomicBool::new(false));
				let waiter = kio::Waiter::new(futures::task::waker(Arc::new(FlagWake(woken.clone()))));
				if polled {
					assert!(
						producer.poll_closed(&waiter).is_pending(),
						"{live:?}: the head still writes"
					);
				}
				head.finish().unwrap();
				if polled {
					assert!(
						woken.load(Ordering::SeqCst),
						"{live:?}: the head finishing wakes the wait"
					);
					assert!(matches!(producer.poll_closed(&waiter), Poll::Ready(Ok(()))), "{live:?}");
				}

				producer.clone().abort(Error::Timeout).unwrap();
				let noop = kio::Waiter::noop();
				assert!(
					matches!(producer.poll_closed(&noop), Poll::Ready(Ok(()))),
					"{live:?} polled={polled}"
				);
				assert!(
					matches!(producer.consume().poll_complete(&noop), Poll::Ready(Ok(()))),
					"{live:?} polled={polled}"
				);
			}
		}
	}

	/// A fetched head aborting hands its group back to the live copy: a finished one settles
	/// the end, while a cut one leaves nothing that delivered the group, so an abort still
	/// cuts the track off.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn an_aborted_head_hands_the_end_back_to_the_live_copy() {
		use std::sync::atomic::{AtomicBool, Ordering};

		for (live, settles) in [(None, true), (Some(Error::App(5)), false)] {
			let (producer, head) = live_group_under_a_writing_head(live.clone()).await;
			let woken = Arc::new(AtomicBool::new(false));
			let waiter = kio::Waiter::new(futures::task::waker(Arc::new(FlagWake(woken.clone()))));
			assert!(producer.poll_closed(&waiter).is_pending(), "{live:?}");

			head.abort(Error::Cancel).unwrap();
			assert!(
				woken.load(Ordering::SeqCst),
				"{live:?}: the head aborting wakes the wait"
			);
			assert_eq!(producer.poll_closed(&waiter).is_ready(), settles, "{live:?}");
			producer.clone().abort(Error::Timeout).unwrap();
			let outcome = producer.poll_closed(&kio::Waiter::noop());
			match settles {
				true => assert!(matches!(outcome, Poll::Ready(Ok(()))), "{live:?}: {outcome:?}"),
				false => assert!(
					matches!(outcome, Poll::Ready(Err(Error::Timeout))),
					"{live:?}: {outcome:?}"
				),
			}
		}
	}

	/// A finished fetched head serves its group to the end, so the declared end settles while
	/// the live copy beside it is still written, and that copy aborting later keeps it clean.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn a_finished_head_settles_the_end_over_an_open_live_copy() {
		let mut producer = track_producer("test", Info::timed());
		let consumer = producer.consume();
		let noop = kio::Waiter::noop();
		let (live, _head) = live_group_with_head(&producer).await;

		producer.finish_at(Position::group(1)).unwrap();
		assert!(matches!(producer.poll_closed(&noop), Poll::Ready(Ok(()))));

		live.abort(Error::Cancel).unwrap();
		producer.clone().abort(Error::Timeout).unwrap();
		assert!(matches!(producer.poll_closed(&noop), Poll::Ready(Ok(()))));
		assert!(matches!(consumer.poll_complete(&noop), Poll::Ready(Ok(()))));
	}

	/// A fetched head still writing holds the largest stream open after the live copy
	/// aborted and the track ended, so a later head frame is reported rather than lost.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn a_fetched_head_holds_the_largest_open() {
		let producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		let consumer = producer.consume();
		let mut reader = consumer.clone();
		let noop = kio::Waiter::noop();

		let mut live = producer.create_group(group::Info { sequence: 0 }).unwrap();
		live.start_at(1).unwrap();
		live.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"delta"))
			.unwrap();
		assert_eq!(
			reader.poll_largest(&noop),
			Poll::Ready(Some(Position { group: 0, frame: 1 }))
		);

		let _fetch = consumer.fetch_group(0, None);
		let request = dynamic.requested_fill_group().await.unwrap();
		let mut head = request.accept(Info::timed()).unwrap();
		for payload in [&b"snapshot"[..], b"delta"] {
			head.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(payload))
				.unwrap();
		}

		live.abort(Error::Cancel).unwrap();
		producer.finish().unwrap();
		assert!(reader.poll_largest(&noop).is_pending(), "the head still writes");

		head.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"more"))
			.unwrap();
		assert_eq!(
			reader.poll_largest(&noop),
			Poll::Ready(Some(Position { group: 0, frame: 2 }))
		);

		head.finish().unwrap();
		assert_eq!(reader.poll_largest(&noop), Poll::Ready(None));
	}

	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn a_live_abort_does_not_remove_a_parked_fetched_head() {
		let producer = track_producer("test", Info::timed());
		let (live, _head) = live_group_with_head(&producer).await;
		let mut subscriber = producer.subscribe(replay());
		subscriber.set_range(..0);
		assert!(subscriber.poll_recv_group(&kio::Waiter::noop()).is_pending());
		live.abort(Error::Cancel).unwrap();
		assert!(subscriber.poll_recv_group(&kio::Waiter::noop()).is_pending());
		subscriber.set_range(..);
		let mut group = subscriber.recv_group().await.unwrap().unwrap();
		assert_eq!(
			group.read_frame().await.unwrap().unwrap().payload,
			bytes::Bytes::from_static(b"snapshot")
		);
	}

	/// The live copy ending early (its subscription went away) leaves a fetched head
	/// that still holds the whole group servable, instead of reclaiming the slot.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn aborted_live_group_keeps_its_fetched_head() {
		let producer = track_producer("test", Info::timed());
		let (live, _head) = live_group_with_head(&producer).await;

		live.abort(Error::Cancel).unwrap();
		// The next group demotes 0 into the eviction order and runs an expiry scan.
		let _open = producer.create_group(group::Info { sequence: 1 }).unwrap();

		let mut group = producer.consume().peek_group(0).expect("the head still serves group 0");
		assert_eq!(
			group.read_frame().await.unwrap().unwrap().payload,
			bytes::Bytes::from_static(b"snapshot")
		);
	}

	/// Reads land on the fetched head, so they keep the slot from expiring even though
	/// the live copy beside it sits idle.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn reading_the_head_keeps_the_slot_from_expiring() {
		let producer = track_producer("test", Info::timed());
		let (mut live, _head) = live_group_with_head(&producer).await;
		live.finish().unwrap();
		let _open = producer.create_group(group::Info { sequence: 1 }).unwrap();

		elapse(&producer, cache::DEFAULT_EXPIRY + Duration::from_secs(1));
		// A fetch hit on the head, past the window the live copy was last written in.
		producer.consume().fetch_group(0, None).await.unwrap();
		let _open = producer.create_group(group::Info { sequence: 2 }).unwrap();

		assert!(producer.consume().peek_group(0).is_some(), "a read head keeps its slot");
	}

	/// Fill group 0 as a live copy plus a fetched head written after it, optionally
	/// abort the live copy, then write past capacity into a protected latest group, so
	/// slot 0 is the only evictable content. Returns whether pressure evicted it.
	#[cfg(feature = "async")]
	async fn pressure_evicts_a_headed_slot(abort_live: bool) -> bool {
		let (producer, _pool) = pooled_producer(10_000);
		let dynamic = producer.requests();
		let consumer = producer.consume();

		let mut live = producer.create_group(group::Info { sequence: 0 }).unwrap();
		live.start_at(1).unwrap();
		live.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"delta"))
			.unwrap();
		live.finish().unwrap();
		let fetch = consumer.fetch_group(0, None);
		let mut head = dynamic
			.requested_fill_group()
			.await
			.unwrap()
			.accept(Info::timed())
			.unwrap();
		head.write_frame(Timestamp::ZERO, bytes::Bytes::from(vec![0u8; 10_000]))
			.unwrap();
		head.finish().unwrap();
		drop(fetch.await.unwrap());
		if abort_live {
			live.abort(Error::Cancel).unwrap();
		}

		let mut next = producer.create_group(group::Info { sequence: 1 }).unwrap();
		for _ in 0..30 {
			next.write_frame(Timestamp::ZERO, bytes::Bytes::from(vec![0u8; 10_000]))
				.unwrap();
		}
		consumer.peek_group(0).is_none()
	}

	/// A head left alone in its slot still joins the pool's access average when the
	/// slot is demoted, so memory pressure can evict it.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn a_head_only_slot_yields_to_memory_pressure() {
		assert!(pressure_evicts_a_headed_slot(true).await);
	}

	/// A slot weighs its copies the way the pool's average samples them, so two copies
	/// with different access stamps can't keep it above the average on their own.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn a_two_copy_slot_yields_to_memory_pressure() {
		assert!(pressure_evicts_a_headed_slot(false).await);
	}

	/// Joining a queued fetch widens its range. A caller that arrives once the range is
	/// already on the wire asks for its own frames rather than being handed the narrower group.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn fetch_widens_or_asks_afresh() {
		let producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		let consumer = producer.consume();
		let from = |frame_start| group::Fetch {
			frame_start,
			..Default::default()
		};

		let mut narrow = std::pin::pin!(consumer.fetch_group(0, from(5)));
		assert!(futures::poll!(narrow.as_mut()).is_pending());

		// Still queued: widening is honored, because nothing has read the range yet.
		let mut wider = std::pin::pin!(consumer.fetch_group(0, from(2)));
		assert!(futures::poll!(wider.as_mut()).is_pending());

		let request = dynamic.requested_fill_group().await.unwrap();
		assert_eq!(request.frame_start(), 2, "widened while queued");

		// Handed off now: the handler holds its own copy of the range, so a later wider
		// caller cannot move what is already being served. It asks for its own.
		let mut widest = std::pin::pin!(consumer.fetch_group(0, from(0)));
		assert!(futures::poll!(widest.as_mut()).is_pending());
		assert_eq!(request.frame_start(), 2, "the in-flight range is already on the wire");
		let whole = dynamic.requested_fill_group().await.unwrap();
		assert_eq!(
			(whole.sequence(), whole.frame_start()),
			(0, 0),
			"the earlier frames are asked"
		);

		// A handler may keep tracking the group's readers past the accept, as the lite
		// subscriber does, which holds the result channel open.
		let _readers = request.readers();
		let mut group = request.accept(Info::timed()).unwrap();
		// A handler numbers the frames from where it was asked to start, as `serve_fetch`
		// does; that offset is what makes the cached group too narrow for `widest`.
		group.start_at(2).unwrap();
		group
			.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"from2"))
			.unwrap();

		// The two callers it covers resolve; the one it doesn't waits on its own fill
		// rather than being handed a group that starts above what it asked for.
		assert_eq!(narrow.await.unwrap().index(), 5);
		assert_eq!(wider.await.unwrap().index(), 2);
		assert!(futures::poll!(widest.as_mut()).is_pending());
		group.finish().unwrap();

		let mut group = whole.accept(None).unwrap();
		group
			.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"from0"))
			.unwrap();
		assert_eq!(widest.await.unwrap().index(), 0);
		group.finish().unwrap();
	}

	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn fetch_coalesces_concurrent() {
		let producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		let consumer = producer.consume();

		// Two fetches for the same uncached group produce ONE handler request,
		// carrying the higher of the two priorities.
		let mut first = consumer.fetch_group(
			5,
			group::Fetch::default()
				.with_priority(1)
				.with_max_delay(Duration::from_secs(1)),
		);
		let second = consumer.fetch_group(
			5,
			group::Fetch::default()
				.with_priority(7)
				.with_max_delay(Duration::from_secs(10)),
		);
		assert!(kio::Task::poll(&mut *first, &kio::Waiter::noop()).is_pending());

		let req = dynamic
			.requested_fill_group()
			.now_or_never()
			.expect("should not block")
			.unwrap();
		assert_eq!(req.sequence(), 5);
		assert_eq!(req.priority(), 7);
		assert_eq!(req.max_delay(), Some(Duration::from_secs(10)));
		assert!(
			dynamic.poll_requested_fill_group(&kio::Waiter::noop()).is_pending(),
			"the second fetch queued a duplicate request"
		);

		// A fetch arriving while the request is already in flight joins it too.
		let third = consumer.fetch_group(5, None);

		// One accept resolves all of them.
		let mut group = req.accept(Info::timed()).unwrap();
		group
			.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"hi"))
			.unwrap();
		group.finish().unwrap();

		assert_eq!(first.await.unwrap().sequence, 5);
		assert_eq!(second.await.unwrap().sequence, 5);
		assert_eq!(third.await.unwrap().sequence, 5);
	}

	/// A looser caller that joined after a tighter budget went on the wire is not failed by
	/// that budget's `Old`: it asks again with its own.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn a_late_looser_fetch_asks_again_after_old() {
		let producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		let consumer = producer.consume();

		let tight = consumer.fetch_group(5, group::Fetch::default().with_max_delay(Duration::from_secs(1)));
		let req = dynamic.requested_fill_group().now_or_never().unwrap().unwrap();
		assert_eq!(req.max_delay(), Some(Duration::from_secs(1)));
		let mut loose = consumer.fetch_group(5, None);
		assert!(kio::Task::poll(&mut *loose, &kio::Waiter::noop()).is_pending());

		req.reject(Error::Old);
		assert!(matches!(tight.await, Err(Error::Old)));
		assert!(kio::Task::poll(&mut *loose, &kio::Waiter::noop()).is_pending());

		let req = dynamic.requested_fill_group().now_or_never().unwrap().unwrap();
		assert_eq!(req.max_delay(), None);
		let mut group = req.accept(Info::timed()).unwrap();
		group
			.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"hi"))
			.unwrap();
		group.finish().unwrap();
		assert_eq!(loose.await.unwrap().sequence, 5);
	}

	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn fetch_coalesced_reject_fails_all() {
		let producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		let consumer = producer.consume();

		let first = consumer.fetch_group(5, None);
		let second = consumer.fetch_group(5, None);
		let req = dynamic
			.requested_fill_group()
			.now_or_never()
			.expect("should not block")
			.unwrap();
		req.reject(Error::Cancel);

		assert!(matches!(first.await, Err(Error::Cancel)));
		assert!(matches!(second.await, Err(Error::Cancel)));

		// The rejected attempt is gone: a retry starts a fresh one.
		let mut retry = consumer.fetch_group(5, None);
		assert!(kio::Task::poll(&mut *retry, &kio::Waiter::noop()).is_pending());
		let req = dynamic
			.requested_fill_group()
			.now_or_never()
			.expect("should not block")
			.unwrap();
		assert_eq!(req.sequence(), 5);
	}

	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn fetch_queued_fails_when_handlers_leave() {
		let producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		let consumer = producer.consume();

		// Queued but never popped: the last handler leaving fails it fast.
		let mut pending = consumer.fetch_group(5, None);
		assert!(kio::Task::poll(&mut *pending, &kio::Waiter::noop()).is_pending());
		drop(dynamic);
		assert!(matches!(pending.await, Err(Error::NotFound)));

		// And the attempt didn't leak.
		let fetch = producer.state.read().fills.clone();
		assert!(fetch.read().is_empty());
	}

	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn fetch_miss_no_dynamic_not_found() {
		// A track with no `Requests` can't serve old content, so a cache miss
		// resolves to NotFound instead of blocking forever.
		let producer = track_producer("test", Info::timed());
		let _open = producer.append_group().unwrap(); // seq 0, but we miss on seq 5
		let consumer = producer.consume();
		assert!(matches!(consumer.fetch_group(5, None).await, Err(Error::NotFound)));
	}

	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn fetch_past_final_not_found() {
		let producer = track_producer("test", Info::timed());
		let _open = producer.append_group().unwrap(); // seq 0
		producer.finish().unwrap(); // final_sequence = 1

		// A group at or past the final sequence can never exist, even with a handler,
		// so it resolves to NotFound.
		let dynamic = producer.requests();
		let consumer = producer.consume();
		assert!(matches!(consumer.fetch_group(5, None).await, Err(Error::NotFound)));

		// And it doesn't signal the dynamic handler.
		assert!(dynamic.poll_requested_fill_group(&kio::Waiter::noop()).is_pending());
	}

	/// Mint a track whose groups charge into a bounded [`cache::Pool`].
	fn pooled_producer(capacity: u64) -> (Producer, cache::Pool) {
		let config = cache::Config::default()
			.with_capacity(capacity)
			.with_expiry(cache::DEFAULT_EXPIRY);
		let pool = cache::Pool::new(config);
		let broadcast = broadcast::Info {
			pool: pool.clone(),
			..Default::default()
		};
		let producer = Producer::new(Arc::new(broadcast), "test", Info::timed());
		(producer, pool)
	}

	fn finished_group(producer: &mut Producer, size: usize) -> u64 {
		let mut group = producer.append_group().unwrap();
		group
			.write_frame(Timestamp::ZERO, bytes::Bytes::from(vec![0u8; size]))
			.unwrap();
		group.finish().unwrap();
		group.sequence
	}

	/// While the pool is over capacity, every append accrues debt and pays it by
	/// evicting this track's own oldest groups, so the newest content survives.
	#[test]
	fn debt_evicts_oldest_group() {
		// Fits one 10k group; each additional group pushes the pool over budget.
		let (mut producer, pool) = pooled_producer(10_000);

		finished_group(&mut producer, 10_000); // seq 0
		finished_group(&mut producer, 10_000); // seq 1: over budget, debt starts accruing
		finished_group(&mut producer, 10_000); // seq 2: pays by evicting seq 0

		let consumer = producer.consume();
		assert!(consumer.peek_group(0).is_none(), "oldest group is evicted");
		assert!(consumer.peek_group(2).is_some(), "latest group survives");
		// Steady state carries the protected live edge plus the just-demoted group
		// (debt is charged before the demotion, so eviction lags one append).
		assert!(
			pool.used() <= 2 * (10_000 + cache::ENTRY_OVERHEAD),
			"usage hovers near capacity: {}",
			pool.used()
		);

		// A fresh subscriber skips the evicted groups entirely.
		let mut subscriber = producer.subscribe(replay());
		assert!(subscriber.assert_group().sequence > 0, "evicted group is not delivered");
	}

	/// A plain reader inflating a finished compressed track pays the debt its inflated copies
	/// add, though no writer is left to: the pool returns to its capacity.
	#[test]
	#[cfg(feature = "async")]
	fn inflating_settles_the_pool_debt() {
		let capacity = 1 << 20;
		let pool = cache::Pool::new(cache::Config::default().with_capacity(capacity));
		let broadcast = Arc::new(broadcast::Info {
			pool: pool.clone(),
			..Default::default()
		});
		let producer = Producer::new(broadcast, "test", Info::timed().with_compress(true));
		let frame = bytes::Bytes::from(vec![0u8; 256 * 1024]);
		let groups = 16;
		for _ in 0..groups {
			let mut group = producer.append_group().unwrap();
			group.write_frame(Timestamp::ZERO, frame.clone()).unwrap();
			group.finish().unwrap();
		}
		assert!(pool.used() < capacity, "the slices fit: {}", pool.used());

		let consumer = producer.consume();
		for sequence in 0..groups {
			let Some(mut group) = consumer.peek_group(sequence) else {
				continue;
			};
			match group.read_frame().now_or_never().expect("cached") {
				Ok(Some(read)) => assert_eq!(read.payload.len(), frame.len()),
				Ok(None) => panic!("group {sequence} ended without its frame"),
				Err(err) => assert!(matches!(err, Error::Evicted), "group {sequence}: {err}"),
			}
		}
		// The live edge and the group being read stay, whatever the budget.
		let slack = 2 * (frame.len() as u64 + cache::ENTRY_OVERHEAD);
		assert!(
			pool.used() <= capacity + slack,
			"inflated copies left the pool over capacity: {}",
			pool.used()
		);
	}

	/// The latest group is never in the eviction order, so it survives any budget.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn latest_group_never_evicted() {
		// Far too small for even one group: the latest survives anyway.
		let (mut producer, pool) = pooled_producer(100);
		finished_group(&mut producer, 1000); // seq 0
		assert!(pool.used() > 100, "the latest may exceed the budget");

		// Later writes evict the demoted seq 0; each new latest is untouchable in turn.
		finished_group(&mut producer, 1000); // seq 1: demotes seq 0
		finished_group(&mut producer, 1000); // seq 2: pays by evicting seq 0

		let consumer = producer.consume();
		assert!(consumer.peek_group(0).is_none());
		let mut group = consumer.peek_group(2).expect("latest survives");
		assert_eq!(group.read_frame().await.unwrap().unwrap().payload.len(), 1000);
	}

	/// A FETCH cache hit refreshes the group's access time: anything accessed more
	/// recently than the pool-wide average is protected, so the eviction walk skips
	/// it and evicts a never-read group instead, even one that arrived later.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn fetch_refresh_survives_eviction() {
		let (mut producer, _pool) = pooled_producer(10_000);
		let consumer = producer.consume();

		finished_group(&mut producer, 3_000); // seq 0
		elapse(&producer, Duration::from_secs(1));
		finished_group(&mut producer, 3_000); // seq 1
		elapse(&producer, Duration::from_secs(1));
		finished_group(&mut producer, 3_000); // seq 2
		elapse(&producer, Duration::from_millis(500));

		// FETCH seq 0: the cache hit lifts its access time above the average.
		let mut fetched = consumer.fetch_group(0, None).await.unwrap();
		assert_eq!(fetched.read_frame().await.unwrap().unwrap().payload.len(), 3_000);
		elapse(&producer, Duration::from_millis(500));

		// Pressure: seq 0 is first in eviction order but freshly accessed, so it
		// rotates to the back and the never-read seq 1 dies instead.
		finished_group(&mut producer, 3_000); // seq 3
		elapse(&producer, Duration::from_secs(1));
		finished_group(&mut producer, 3_000); // seq 4

		assert!(consumer.peek_group(0).is_some(), "refreshed group survives");
		assert!(consumer.peek_group(1).is_none(), "unread group is evicted instead");
	}

	/// A consumer holding an evicted group surfaces the eviction, not a hang or a
	/// truncated clean end.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn eviction_aborts_readers() {
		let (mut producer, _pool) = pooled_producer(10_000);
		let mut subscriber = producer.subscribe(None);

		finished_group(&mut producer, 10_000); // seq 0
		let mut group0 = subscriber.assert_group();

		finished_group(&mut producer, 10_000); // seq 1: demotes seq 0
		finished_group(&mut producer, 10_000); // seq 2: pays by evicting seq 0

		let read = group0.read_frame().await;
		assert!(matches!(read, Err(Error::Evicted)), "expected Evicted, got {read:?}");
	}

	/// A write smaller than the next victim carries debt instead of evicting: a
	/// large group dies only once enough debt accumulates, never to pay off a
	/// far smaller write.
	#[test]
	fn small_writes_carry_debt() {
		// Payloads dwarf the fixed per-group charge, so the budget arithmetic below is
		// about bytes written rather than bookkeeping.
		let unit = 100 * cache::ENTRY_OVERHEAD;
		let (mut producer, pool) = pooled_producer(22 * unit);
		let consumer = producer.consume();

		finished_group(&mut producer, 20 * unit as usize); // seq 0, the large victim-to-be

		// The first few small writes owe far less than seq 0's size: the debt
		// carries over instead of evicting it.
		for _ in 0..3 {
			finished_group(&mut producer, unit as usize);
		}
		assert!(consumer.peek_group(0).is_some(), "debt smaller than the victim carries");

		// Enough small writes accumulate the debt to finally evict it.
		for _ in 0..20 {
			finished_group(&mut producer, unit as usize);
		}
		assert!(
			consumer.peek_group(0).is_none(),
			"accumulated debt evicts the large group"
		);
		// Steady state hovers within about one group of capacity: a victim smaller
		// than the outstanding debt is never evicted, so the excess stays bounded.
		assert!(pool.used() <= 24 * unit, "usage hovers near capacity: {}", pool.used());
	}

	/// A capacity shrink reclaims a failed group's held head before any cached group, even
	/// in a pool with no idle window and once the failed copy's track is gone.
	#[test]
	fn a_shrink_reclaims_held_heads_first() {
		let pool = cache::Pool::new(cache::Config::default().with_capacity(1 << 40));
		let broadcast = Arc::new(broadcast::Info {
			pool: pool.clone(),
			..Default::default()
		});
		let failed = Producer::new(broadcast.clone(), "failed", Info::timed());
		let mut group = failed.append_group().unwrap();
		group
			.write_frame(Timestamp::ZERO, bytes::Bytes::from(vec![0u8; 1_000]))
			.unwrap();
		let handoff = group.consume().handoff();
		group.abort(crate::Error::Cancel).unwrap();
		drop(failed);
		assert!(handoff.head().is_some(), "held for a successor route");

		let mut producer = Producer::new(broadcast, "live", Info::timed());
		finished_group(&mut producer, 1_000);
		// Room for the cached group, not the held head beside it.
		pool.resize(pool.used() - 1);
		finished_group(&mut producer, 1_000);

		assert!(
			handoff.head().is_none(),
			"the shrink left a stalled resumer's head held"
		);
		assert!(producer.consume().peek_group(0).is_some(), "a cached group went first");
	}

	/// One write pays at most twice what it produced, so a capacity shrink (or one
	/// track's burst) drains gradually instead of one writer dumping its whole
	/// backlog in a single call.
	#[test]
	fn payment_capped_per_write() {
		let (mut producer, pool) = pooled_producer(1 << 40);
		for _ in 0..10 {
			finished_group(&mut producer, 1_000);
		}

		// The governor slashes the target; nothing is reclaimed synchronously.
		pool.resize(100);
		let before = pool.used();

		// One 1k write may evict at most ~2k of backlog, not all ten groups.
		finished_group(&mut producer, 1_000);

		let consumer = producer.consume();
		assert!(consumer.peek_group(0).is_none(), "the oldest groups are evicted");
		assert!(consumer.peek_group(1).is_none());
		assert!(consumer.peek_group(2).is_some(), "the backlog drains gradually");
		assert!(pool.used() > before - 4_000, "one write must not dump the backlog");
	}

	/// Accepting a track after pre-accept backfill must keep the same write
	/// counter: the counter is owned by the track state, so replacing the info
	/// can't strand the bytes already-created groups keep charging.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn accept_preserves_write_accounting() {
		let config = cache::Config::default()
			.with_capacity(12_000)
			.with_expiry(cache::DEFAULT_EXPIRY);
		let pool = cache::Pool::new(config);
		let broadcast = broadcast::Info {
			pool: pool.clone(),
			..Default::default()
		};
		let request = Request::new(Arc::new(broadcast), "test");
		let dynamic = request.requests();
		let consumer = request.consume();

		// Serve a backfill before the track is accepted, then grow it.
		let pending = consumer.fetch_group(0, None);
		let req = dynamic
			.requested_fill_group()
			.now_or_never()
			.expect("should not block")
			.unwrap();
		let mut backfill = req.accept(Info::timed()).unwrap();
		pending.await.unwrap();
		backfill
			.write_frame(Timestamp::ZERO, bytes::Bytes::from(vec![0u8; 30_000]))
			.unwrap();

		// Accept with a fresh Info: the pre-accept group's writes must still be
		// drained by this track's future charges.
		let producer = request.accept(Info::timed());
		producer.append_group().unwrap().finish().unwrap();
		producer.append_group().unwrap().finish().unwrap();

		assert!(
			producer.consume().peek_group(0).is_none(),
			"pre-accept backfill growth is reclaimed after accept"
		);
		assert!(pool.used() <= 13_000, "usage converges: {}", pool.used());
	}

	/// Re-serving a sequence many times must not accumulate eviction hints: stale
	/// hints die on stamp mismatch and compaction reclaims them.
	#[test]
	fn recreated_sequence_bounds_eviction_hints() {
		let (producer, _pool) = pooled_producer(1 << 40);
		producer.create_group(5u64.into()).unwrap().finish().unwrap();

		for _ in 0..200 {
			let group = producer.create_group(1u64.into()).unwrap();
			group.abort(Error::Cancel).unwrap();
		}

		let state = producer.state.read();
		assert!(
			state.evict.len() <= 2 * state.lookup.len() + EVICT_SLACK,
			"stale hints are compacted: {} entries for {} slots",
			state.evict.len(),
			state.lookup.len()
		);
	}

	/// A frame write within the same coarse tick still outranks merely-inserted
	/// content, so the freshly-written group survives and the empty one pays.
	#[test]
	fn same_tick_write_outranks_inserted() {
		// Payloads dwarf the fixed per-group charge, so the budget arithmetic below is
		// about bytes written rather than bookkeeping.
		let unit = 100 * cache::ENTRY_OVERHEAD;
		// No time advances: every stamp lands in the same tick.
		let (mut producer, _pool) = pooled_producer(10 * unit);

		producer.append_group().unwrap().finish().unwrap(); // seq 0: empty
		finished_group(&mut producer, 3 * unit as usize); // seq 1: written
		finished_group(&mut producer, 3 * unit as usize); // seq 2
		finished_group(&mut producer, 3 * unit as usize); // seq 3
		finished_group(&mut producer, 3 * unit as usize); // seq 4: over budget, pays

		let consumer = producer.consume();
		assert!(consumer.peek_group(0).is_none(), "insert-only content pays first");
		assert!(consumer.peek_group(1).is_some(), "same-tick written content survives");
	}

	/// A track that only appends frames to an open group, never inserting another
	/// group, still settles its eviction debt once enough bytes accumulate.
	#[test]
	fn frame_only_writer_pays() {
		let (producer, pool) = pooled_producer(2_000);
		let mut demoted = producer.append_group().unwrap(); // seq 0
		producer.append_group().unwrap().finish().unwrap(); // seq 1 demotes seq 0

		// One large frame crosses the charge threshold: the write itself pays,
		// with no further group insert on this track.
		demoted
			.write_frame(Timestamp::ZERO, bytes::Bytes::from(vec![0u8; 300_000]))
			.unwrap();

		assert!(
			pool.used() <= 5_000,
			"the frame write settled the debt: {}",
			pool.used()
		);
		assert!(matches!(demoted.finish(), Err(Error::Evicted)));
	}

	/// One `Info` describing several tracks must not join their eviction accounting:
	/// each track opens its own account against the pool.
	#[test]
	fn each_track_owns_its_account() {
		let broadcast = Arc::new(broadcast::Info::default());
		let info = Info::timed();
		let a = Producer::new(broadcast.clone(), "a", info.clone());
		let b = Producer::new(broadcast, "b", info);

		let a = a.state.read().cache.clone();
		let b = b.state.read().cache.clone();
		assert!(!Arc::ptr_eq(&a, &b), "each track owns its account");
	}

	/// A `Requests` still serving fetches keeps the track alive, so the publisher
	/// letting go isn't an abrupt teardown: the handler can still serve the cache.
	#[test]
	fn a_dynamic_defers_teardown() {
		let (mut producer, pool) = pooled_producer(1 << 40);
		let dynamic = producer.requests();
		finished_group(&mut producer, 100);

		drop(producer);
		assert!(pool.used() > 0, "the handler still serves the cache");

		drop(dynamic);
		assert_eq!(pool.used(), 0, "the last handle tears it down");
	}

	/// A finished track releases everything once every handle is gone.
	///
	/// Its groups hold the cache account, and the account links back here, so that link
	/// has to be weak: anything stronger makes the state (and every cached frame in it)
	/// immortal, even with no producer or consumer left.
	#[test]
	fn finished_track_frees_its_cache() {
		let (mut producer, pool) = pooled_producer(1 << 40);
		finished_group(&mut producer, 100);
		producer.finish().unwrap();

		let state = producer.state.downgrade();
		drop(producer);

		assert!(state.upgrade().is_none(), "the track state is freed");
		assert_eq!(pool.used(), 0, "so are its cached bytes");
	}

	/// A group settling its eviction debt upgrades the account's weak handle, which
	/// counts as a producer on the track state. Teardown must not mistake that for a
	/// surviving publisher, or an abrupt drop silently behaves like a clean finish.
	#[test]
	fn teardown_ignores_a_settling_group() {
		let (producer, pool) = pooled_producer(1 << 40);
		// Open, so only the abrupt teardown releases it.
		let mut group = producer.append_group().unwrap();
		group
			.write_frame(Timestamp::ZERO, bytes::Bytes::from(vec![0u8; 100]))
			.unwrap();
		drop(group);

		// Stand in for a concurrent `cache::Track::settle`, mid-upgrade.
		let settling = producer.state.downgrade().upgrade().expect("open");
		drop(producer);

		assert_eq!(pool.used(), 0, "the abrupt teardown still released the cache");
		drop(settling);
	}

	/// A subscriber holding one cached group must not pin the whole track: a group
	/// carries the track's properties by value, not a handle back to its state.
	#[test]
	fn cached_group_outlives_its_track() {
		let (mut producer, pool) = pooled_producer(1 << 40);
		let sequence = finished_group(&mut producer, 100);
		let group = producer.consume().peek_group(sequence).expect("cached");
		producer.finish().unwrap();

		let state = producer.state.downgrade();
		drop(producer);
		assert!(state.upgrade().is_none(), "the track state is freed");
		assert!(pool.used() > 0, "the retained group keeps its own bytes");

		drop(group);
		assert_eq!(pool.used(), 0, "which it releases when dropped");
	}

	/// A backfill served before the track was accepted settles its own debt: the
	/// account exists from the moment the state does, so acceptance replacing the
	/// `Info` can't leave already-created groups writing for free.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn pre_accept_backfill_settles_late_writes() {
		let config = cache::Config::default()
			.with_capacity(2_000)
			.with_expiry(cache::DEFAULT_EXPIRY);
		let pool = cache::Pool::new(config);
		let broadcast = broadcast::Info {
			pool: pool.clone(),
			..Default::default()
		};
		let request = Request::new(Arc::new(broadcast), "test");
		let dynamic = request.requests();
		let consumer = request.consume();

		// Serve backfill seq 0 before the track is accepted.
		let pending = consumer.fetch_group(0, None);
		let req = dynamic
			.requested_fill_group()
			.now_or_never()
			.expect("should not block")
			.unwrap();
		let mut backfill = req.accept(Info::timed()).unwrap();
		pending.await.unwrap();

		// Accept, then demote the backfill with a live group.
		let producer = request.accept(Info::timed());
		producer.append_group().unwrap().finish().unwrap();

		// No further insert: the late write into the demoted backfill is the only
		// thing that can pay the debt it just took on.
		backfill
			.write_frame(Timestamp::ZERO, bytes::Bytes::from(vec![0u8; 300_000]))
			.unwrap();

		assert!(
			pool.used() <= 5_000,
			"the frame write settled the debt: {}",
			pool.used()
		);
	}

	/// A late frame write restarts the LRU clock (the window measures time since
	/// last written or fetched), so an actively-growing group is not expired as
	/// idle mid-write.
	#[test]
	fn write_restarts_retention_clock() {
		let (producer, _pool) = pooled_producer(1 << 40);
		let mut straggler = producer.append_group().unwrap(); // seq 0
		producer.append_group().unwrap().finish().unwrap(); // seq 1 demotes seq 0

		// Idle past the window, then the straggler receives a late frame.
		elapse(&producer, cache::DEFAULT_EXPIRY + Duration::from_secs(1));
		straggler
			.write_frame(Timestamp::ZERO, bytes::Bytes::from(vec![0u8; 100]))
			.unwrap();
		producer.append_group().unwrap().finish().unwrap(); // seq 2 runs expiry

		let consumer = producer.consume();
		assert!(consumer.peek_group(0).is_some(), "the write restarted the clock");

		// Once the writes stop, the group ages out normally.
		elapse(&producer, cache::DEFAULT_EXPIRY + Duration::from_secs(1));
		producer.append_group().unwrap().finish().unwrap(); // seq 3 runs expiry
		assert!(consumer.peek_group(0).is_none(), "idle content still expires");
	}

	/// Continuously refreshed entries at the front of the eviction order must not
	/// starve expiry of entries behind them: the scan cursor rotates.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn refreshed_front_does_not_starve_expiry() {
		let (producer, _pool) = pooled_producer(1 << 40);
		let dynamic = producer.requests();
		let consumer = producer.consume();

		producer.create_group(10u64.into()).unwrap().finish().unwrap();
		for sequence in 1..=5u64 {
			let pending = consumer.fetch_group(sequence, None);
			let req = dynamic
				.requested_fill_group()
				.now_or_never()
				.expect("should not block")
				.unwrap();
			let mut group = req.accept(Info::timed()).unwrap();
			group
				.write_frame(Timestamp::ZERO, bytes::Bytes::from(vec![0u8; 100]))
				.unwrap();
			group.finish().unwrap();
			pending.await.unwrap();
		}

		// Age everything out, then refresh the first four backfills so they sit
		// fresh at the front of the eviction order, hiding the expired fifth.
		elapse(&producer, cache::DEFAULT_EXPIRY + Duration::from_secs(1));
		for sequence in 1..=4u64 {
			consumer.fetch_group(sequence, None).await.unwrap();
		}

		// The rotating cursor reaches the fifth entry within a few writes.
		for _ in 0..3 {
			producer.append_group().unwrap().finish().unwrap();
		}
		assert!(consumer.peek_group(5).is_none(), "expired backfill is reclaimed");
		assert!(consumer.peek_group(1).is_some(), "refreshed backfill survives");
	}

	/// A publisher re-creating an aborted sequence is delivered exactly once, at
	/// its actual arrival position: the historical arrival entry is dead.
	#[test]
	fn recreated_sequence_delivered_once() {
		let (producer, _pool) = pooled_producer(1 << 40);

		producer.create_group(0u64.into()).unwrap().finish().unwrap();
		let aborted = producer.create_group(1u64.into()).unwrap();
		aborted.abort(Error::Cancel).unwrap();
		producer.create_group(2u64.into()).unwrap().finish().unwrap();
		producer.create_group(1u64.into()).unwrap().finish().unwrap();

		let mut subscriber = producer.subscribe(replay());
		assert_eq!(subscriber.assert_group().sequence, 0);
		assert_eq!(subscriber.assert_group().sequence, 2);
		assert_eq!(
			subscriber.assert_group().sequence,
			1,
			"replacement arrives at its own position"
		);
		subscriber.assert_no_group();
	}

	/// Datagrams share `max_sequence` but must not break group demotion: the live
	/// edge is tracked per group, so interleaving datagrams can't strand groups
	/// outside the eviction order and bypass the budget.
	#[test]
	fn datagrams_do_not_block_eviction() {
		let (mut producer, pool) = pooled_producer(1_000);
		for _ in 0..10 {
			finished_group(&mut producer, 1_000);
			producer.append_datagram(Timestamp::ZERO, &b"beat"[..]).unwrap();
		}

		let consumer = producer.consume();
		assert!(consumer.peek_group(0).is_none(), "old groups still evict");
		assert!(
			pool.used() < 4 * 1_256,
			"interleaved datagrams must not bypass the budget: {}",
			pool.used()
		);
	}

	/// A received compressed datagram inflates far past its wire size, so the send buffer
	/// bills both forms to the pool, and pressure drops each track's oldest instead of
	/// letting every track retain a full buffer of inflated payloads.
	#[test]
	#[cfg(feature = "async")]
	fn received_compressed_datagrams_are_billed() {
		let capacity = 1024;
		let pool = cache::Pool::new(cache::Config::default().with_capacity(capacity));
		let broadcast = Arc::new(broadcast::Info {
			pool: pool.clone(),
			..Default::default()
		});
		let plain = bytes::Bytes::from(vec![0u8; super::super::datagram::MAX_DATAGRAM_PAYLOAD]);
		let deflated = super::super::flate::Encoder::new().frame(&plain);
		assert!(
			deflated.len() < 128,
			"the payload compresses to {} bytes",
			deflated.len()
		);

		let tracks = 16;
		let mut held = Vec::new();
		for track in 0..tracks {
			let mut producer = Producer::new(
				broadcast.clone(),
				format!("track{track}").as_str(),
				Info::timed().with_compress(true),
			);
			let subscriber = producer.subscribe(None);
			for sequence in 0..MAX_DATAGRAMS as u64 {
				let datagram = Datagram {
					sequence,
					timestamp: Some(Timestamp::ZERO),
					payload: deflated.clone(),
				};
				producer
					.insert_deflated_datagram(datagram.into(), plain.len() as u64)
					.unwrap();
			}
			held.push((producer, subscriber));
		}

		let retained: u64 = held
			.iter()
			.flat_map(|(producer, _)| {
				let state = producer.state.read();
				state
					.datagrams
					.iter()
					.map(|datagram| {
						let stored = datagram.stored().unwrap();
						(stored.payload.len() + stored.deflated.as_ref().unwrap().len()) as u64
					})
					.collect::<Vec<_>>()
			})
			.sum();
		assert!(
			pool.used() >= retained,
			"{retained} retained bytes, {} billed",
			pool.used()
		);
		// Each track keeps only its newest, which no write ever evicts on its own account.
		let newest = (plain.len() + deflated.len()) as u64;
		assert!(
			retained <= tracks * newest,
			"{retained} bytes retained across {tracks} tracks"
		);

		for (_, subscriber) in &mut held {
			assert_eq!(
				recv_datagram(subscriber).payload,
				plain,
				"the newest is still delivered"
			);
		}
		drop(held);
		assert_eq!(pool.used(), 0, "the buffers release their bill when the tracks go");
	}

	/// A datagram the full buffer pushes out releases its bill, so the pool holds exactly
	/// what the buffer retains.
	#[test]
	fn a_full_datagram_buffer_bills_what_it_retains() {
		let pool = cache::Pool::unbounded();
		let mut producer = track_producer_pooled("test", pool.clone());
		for _ in 0..MAX_DATAGRAMS * 2 {
			producer.append_datagram(Timestamp::ZERO, vec![0u8; 1000]).unwrap();
		}
		let state = producer.state.read();
		assert_eq!(state.datagrams.len(), MAX_DATAGRAMS);
		let billed: u64 = state.datagrams.iter().map(|datagram| datagram.bytes()).sum();
		assert_eq!(pool.used(), billed);
	}

	/// A track that stops sending datagrams cannot pin its buffer in the shared pool: the
	/// sweep reclaims it once idle, the newest too, since no late joiner needs it.
	#[test]
	fn pool_sweep_expires_idle_datagrams() {
		let pool = cache::Pool::new(cache::Config::default().with_expiry(Duration::from_secs(1)));
		let mut producer = track_producer_pooled("test", pool.clone());
		for _ in 0..3 {
			producer.append_datagram(Timestamp::ZERO, b"x".as_slice()).unwrap();
		}
		assert!(pool.used() > 0);

		elapse(&producer, Duration::from_secs(2));
		pool.sweep();

		assert!(
			producer.state.read().datagrams.is_empty(),
			"the sweep reclaimed idle datagrams"
		);
		assert_eq!(pool.used(), 0);
	}

	/// An ended track's closed state refuses the write that pops a datagram, so a stale
	/// reader would pin its buffer: the sweep releases each idle one in place instead,
	/// and the reader skips it like a dropped one.
	#[test]
	#[cfg(feature = "async")]
	fn pool_sweep_expires_an_ended_tracks_datagrams() {
		let pool = cache::Pool::new(cache::Config::default().with_expiry(Duration::from_secs(10)));
		let mut producer = track_producer_pooled("test", pool.clone());
		let mut stale = producer.subscribe(None);
		producer.append_datagram(Timestamp::ZERO, b"old".as_slice()).unwrap();
		pool.step(Duration::from_secs(6));
		producer.append_datagram(Timestamp::ZERO, b"new".as_slice()).unwrap();
		producer.finish().unwrap();
		drop(producer);

		// Only the oldest is idle for the whole window.
		pool.step(Duration::from_secs(6));
		pool.sweep();
		assert!(pool.used() > 0, "the newest is still within the window");
		assert_eq!(
			recv_datagram(&mut stale).payload,
			b"new".as_slice(),
			"the reader skips the released one"
		);

		pool.step(Duration::from_secs(6));
		pool.sweep();
		assert_eq!(pool.used(), 0, "the sweep released every idle datagram");
	}

	/// An aborted group releases its access sample along with its bytes, from any
	/// handle: ghost samples must not linger in the pool mean where they'd hold it
	/// in the past and over-protect every live group.
	#[test]
	fn aborted_group_leaves_no_ghost_sample() {
		let (producer, pool) = pooled_producer(1 << 40);
		let group0 = producer.append_group().unwrap();
		let _open = producer.append_group().unwrap(); // demotes seq 0 into the mean

		assert!(pool.average().is_some(), "demoted group is sampled");
		group0.abort(Error::Cancel).unwrap();
		assert_eq!(pool.average(), None, "the abort must remove the sample");
	}

	/// Empty groups still carry fixed overhead; they must repay the budget when
	/// evicted rather than being unevictable freeloaders.
	#[test]
	fn empty_groups_repay_overhead() {
		let (producer, pool) = pooled_producer(1_000);
		for _ in 0..100 {
			let mut group = producer.append_group().unwrap();
			group.finish().unwrap();
		}

		assert!(
			pool.used() <= 3_000,
			"empty-group overhead must stay near the budget: {}",
			pool.used()
		);
	}

	/// Late growth on an already-demoted group is billed: the gross-write counter
	/// feeds debt on the next append, so a straggler can't grow unbounded.
	#[test]
	fn growth_on_demoted_group_is_billed() {
		let (producer, pool) = pooled_producer(2_000);
		let mut straggler = producer.append_group().unwrap(); // seq 0
		producer.append_group().unwrap().finish().unwrap(); // seq 1 demotes seq 0

		// The demoted group balloons: no eviction yet (nothing ran), but billed.
		straggler
			.write_frame(Timestamp::ZERO, bytes::Bytes::from(vec![0u8; 10_000]))
			.unwrap();

		// The next append observes the growth and evicts the straggler.
		producer.append_group().unwrap().finish().unwrap(); // seq 2

		let consumer = producer.consume();
		assert!(consumer.peek_group(0).is_none(), "the ballooned group is evicted");
		assert!(pool.used() <= 3_000, "growth is reclaimed: {}", pool.used());
	}

	/// A stale arrival entry whose sequence was later re-served by fetched backfill
	/// must not leak the replacement into arrival-order subscriptions.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn refilled_sequence_stays_out_of_subscriptions() {
		let (producer, _pool) = pooled_producer(1 << 40);
		let dynamic = producer.requests();
		let consumer = producer.consume();

		producer.create_group(0u64.into()).unwrap().finish().unwrap();
		let aborted = producer.create_group(1u64.into()).unwrap();
		aborted.abort(Error::Cancel).unwrap();
		producer.create_group(2u64.into()).unwrap().finish().unwrap();

		// Re-serve seq 1 as backfill; its slot replaces the aborted one, and the
		// old arrival entry for seq 1 now resolves to it.
		let pending = consumer.fetch_group(1, None);
		let req = dynamic
			.requested_fill_group()
			.now_or_never()
			.expect("should not block")
			.unwrap();
		let mut group = req.accept(Info::timed()).unwrap();
		group
			.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"backfill"))
			.unwrap();
		group.finish().unwrap();
		pending.await.unwrap();

		// The backfill serves by sequence, but never in arrival order.
		assert!(consumer.peek_group(1).is_some());
		let mut walked = Vec::new();
		let mut index = 0;
		while let Poll::Ready(Ok(Some((group, next)))) = producer.state.read().poll_recv_group(index, 0) {
			walked.push(group.sequence);
			index = next + 1;
		}
		assert_eq!(walked, [0, 2]);
		// A reader takes it as the fill answer for the group the feed gave up, as it would
		// have asked a fill for it, and the stale arrival entry adds nothing.
		let mut subscriber = producer.subscribe(replay());
		let mut read: Vec<_> = (0..3).map(|_| subscriber.assert_group().sequence).collect();
		read.sort();
		assert_eq!(read, [0, 1, 2]);
		subscriber.assert_no_group();
	}

	/// An expired backfill can't hide behind a refreshed one: the eviction-order
	/// expiry scans a bounded prefix instead of stopping at the first fresh entry.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn expired_backfill_behind_refreshed_reclaimed() {
		let (producer, _pool) = pooled_producer(1 << 40);
		let dynamic = producer.requests();
		let consumer = producer.consume();

		producer.create_group(5u64.into()).unwrap().finish().unwrap();
		for sequence in [2u64, 3u64] {
			let pending = consumer.fetch_group(sequence, None);
			let req = dynamic
				.requested_fill_group()
				.now_or_never()
				.expect("should not block")
				.unwrap();
			let mut group = req.accept(Info::timed()).unwrap();
			group
				.write_frame(Timestamp::ZERO, bytes::Bytes::from(vec![0u8; 100]))
				.unwrap();
			group.finish().unwrap();
			pending.await.unwrap();
		}

		// Keep seq 2 fresh while seq 3 (behind it in eviction order) expires.
		elapse(&producer, cache::DEFAULT_EXPIRY / 2 + Duration::from_secs(1));
		consumer.fetch_group(2, None).await.unwrap();
		elapse(&producer, cache::DEFAULT_EXPIRY / 2 + Duration::from_secs(1));
		producer.create_group(6u64.into()).unwrap().finish().unwrap();

		let consumer = producer.consume();
		assert!(consumer.peek_group(2).is_some(), "refreshed backfill survives");
		assert!(consumer.peek_group(3).is_none(), "expired backfill is reclaimed");
	}

	/// A FETCH hit within the same coarse clock tick still protects the group: the
	/// refresh stamps one tick ahead, so it reads strictly newer than the mean.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn same_tick_fetch_protects() {
		// No time advances at all: every timestamp lands in the same tick.
		let (mut producer, _pool) = pooled_producer(10_000);
		let consumer = producer.consume();

		finished_group(&mut producer, 3_000); // seq 0
		finished_group(&mut producer, 3_000); // seq 1
		finished_group(&mut producer, 3_000); // seq 2

		consumer.fetch_group(0, None).await.unwrap();

		finished_group(&mut producer, 3_000); // seq 3
		finished_group(&mut producer, 3_000); // seq 4

		assert!(consumer.peek_group(0).is_some(), "same-tick refresh protects");
		assert!(consumer.peek_group(1).is_none(), "the unread group dies instead");
	}

	/// A refetched group that reclaims max_sequence is the live edge again: it must
	/// not re-enter the eviction order, or memory pressure could evict the newest
	/// content.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn refetched_latest_stays_protected() {
		let (producer, _pool) = pooled_producer(10_000);
		let dynamic = producer.requests();
		let consumer = producer.consume();

		let straggler = producer.append_group().unwrap(); // seq 0

		// The publisher aborts its own latest group; the sequence stays at the live edge.
		let latest = producer.append_group().unwrap(); // seq 1
		latest.abort(Error::Cancel).unwrap();

		// Re-fetch it: the replacement takes over max_sequence.
		let pending = consumer.fetch_group(1, None);
		let req = dynamic
			.requested_fill_group()
			.now_or_never()
			.expect("should not block")
			.unwrap();
		let mut group = req.accept(Info::timed()).unwrap();
		group
			.write_frame(Timestamp::ZERO, bytes::Bytes::from(vec![0u8; 1000]))
			.unwrap();
		group.finish().unwrap();
		pending.await.unwrap();

		// The refetched latest is protected by omission: it has no entry in the
		// eviction order, so no amount of debt can select it.
		{
			let state = producer.state.read();
			assert!(state.lookup.contains_key(&1), "refetched group is cached");
			assert!(
				state.evict.iter().all(|(sequence, _)| *sequence != 1),
				"the live edge must not be an eviction candidate"
			);
		}
		drop(straggler);
	}

	/// An evicted group is a cache miss, so a fetch re-fetches it and the accepted
	/// replacement serves the sequence again (not `Error::Duplicate`).
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn eviction_allows_refetch() {
		let (mut producer, _pool) = pooled_producer(10_000);
		let dynamic = producer.requests();

		finished_group(&mut producer, 10_000); // seq 0
		finished_group(&mut producer, 10_000); // seq 1: demotes seq 0
		finished_group(&mut producer, 10_000); // seq 2: pays by evicting seq 0

		let consumer = producer.consume();
		assert!(consumer.peek_group(0).is_none());
		let pending = consumer.fetch_group(0, None);

		let req = dynamic
			.requested_fill_group()
			.now_or_never()
			.expect("should not block")
			.unwrap();
		assert_eq!(req.sequence(), 0);

		let mut group = req.accept(Info::timed()).unwrap();
		group
			.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"refetched"))
			.unwrap();
		group.finish().unwrap();

		let mut group = pending.await.unwrap();
		assert_eq!(&group.read_frame().await.unwrap().unwrap().payload[..], b"refetched");
	}

	/// An aborted group is dead whatever frame it starts at: the sequence is claimable
	/// again, and a cache lookup misses rather than handing back a slot that can no
	/// longer serve it.
	///
	/// The abort and the group's first frame decide this together, so they are read
	/// under one guard. Read separately, the abort can land between them and the slot
	/// answers as a live duplicate on the strength of an offset it only still has
	/// because it died. That interleaving is what the single guard rules out; this
	/// pins the committed semantics it has to preserve.
	#[test]
	fn an_aborted_group_releases_its_sequence() {
		let producer = track_producer("test", Info::timed());
		let consumer = producer.consume();

		let mut group = producer.create_group(group::Info { sequence: 3 }).unwrap();
		group
			.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"head"))
			.unwrap();

		// While it lives the slot answers, so the sequence is taken.
		assert!(matches!(
			producer.create_group(group::Info { sequence: 3 }),
			Err(Error::Duplicate)
		));
		assert!(consumer.peek_group(3).is_some());

		group.abort(Error::Cancel).unwrap();

		assert!(consumer.peek_group(3).is_none(), "an aborted slot is a cache miss");
		producer
			.create_group(group::Info { sequence: 3 })
			.expect("an aborted slot releases its sequence")
			.finish()
			.unwrap();
	}

	/// Lowering the floor re-offers only what the arrival walk could have read: a fetched
	/// group stays out of arrival order, while a live one is offered before its first frame.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn a_lowered_floor_reoffers_live_groups_but_not_fetched_ones() {
		let (producer, _pool) = pooled_producer(1 << 40);
		let dynamic = producer.requests();
		let consumer = producer.consume();
		producer.create_group(5u64.into()).unwrap().finish().unwrap();
		let mut subscriber = producer.subscribe(replay().with_range(5..));
		assert_eq!(subscriber.assert_group().sequence, 5);

		let pending = consumer.fetch_group(2, None);
		let req = dynamic
			.requested_fill_group()
			.now_or_never()
			.expect("should not block")
			.unwrap();
		let mut fetched = req.accept(Info::timed()).unwrap();
		fetched
			.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"backfill"))
			.unwrap();
		fetched.finish().unwrap();
		pending.await.unwrap();
		let _frameless = producer.create_group(group::Info { sequence: 3 }).unwrap();

		subscriber.update(replay().with_range(0..)).unwrap();
		assert_eq!(subscriber.assert_group().sequence, 3, "offered before its first frame");
		subscriber.assert_no_group();

		// A cursor floor lowered before the next read counts only walked groups as unread.
		let mut lowered = producer.subscribe(replay());
		lowered.set_range(5..);
		assert_eq!(lowered.assert_group().sequence, 5);
		lowered.set_range(0..);
		let delivered = super::super::delivered::Delivered::default();
		let unread = Unread {
			start: 0,
			end: Some(3),
			delivered: &delivered,
		};
		assert!(!lowered.has_unread_group(&unread), "the fetched group 2 is not unread");
	}

	/// A fetched (backfill) group is served by sequence but never replayed to
	/// arrival-order subscribers.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn fetched_backfill_not_subscribed() {
		let (producer, _pool) = pooled_producer(1 << 40);
		let dynamic = producer.requests();
		let consumer = producer.consume();

		// The publisher starts at seq 5; earlier groups exist only upstream.
		producer.create_group(5u64.into()).unwrap().finish().unwrap();
		producer.create_group(6u64.into()).unwrap().finish().unwrap();

		// Fetch the gap: it lands in the cache and resolves the fetch...
		let pending = consumer.fetch_group(2, None);
		let req = dynamic
			.requested_fill_group()
			.now_or_never()
			.expect("should not block")
			.unwrap();
		let mut group = req.accept(Info::timed()).unwrap();
		group
			.write_frame(Timestamp::ZERO, bytes::Bytes::from_static(b"backfill"))
			.unwrap();
		group.finish().unwrap();
		let mut fetched = pending.await.unwrap();
		assert_eq!(&fetched.read_frame().await.unwrap().unwrap().payload[..], b"backfill");
		assert!(consumer.peek_group(2).is_some(), "backfill is cached for later fetches");

		// ...but an arrival-order subscriber only sees the live groups.
		let mut subscriber = producer.subscribe(replay());
		assert_eq!(subscriber.assert_group().sequence, 5);
		assert_eq!(subscriber.assert_group().sequence, 6);
		subscriber.assert_no_group();
	}

	/// Fetched backfill isn't in arrival order, so it ages out through the eviction
	/// order instead of lingering until the track closes.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn expired_backfill_reclaimed() {
		let (producer, pool) = pooled_producer(1 << 40);
		let dynamic = producer.requests();
		let consumer = producer.consume();

		producer.create_group(5u64.into()).unwrap().finish().unwrap();

		// Serve a backfill fetch for an old sequence.
		let pending = consumer.fetch_group(2, None);
		let req = dynamic
			.requested_fill_group()
			.now_or_never()
			.expect("should not block")
			.unwrap();
		let mut group = req.accept(Info::timed()).unwrap();
		group
			.write_frame(Timestamp::ZERO, bytes::Bytes::from(vec![0u8; 1000]))
			.unwrap();
		group.finish().unwrap();
		pending.await.unwrap();
		let used = pool.used();

		// Age past the pool's LRU window; the next write reclaims the backfill.
		elapse(&producer, cache::DEFAULT_EXPIRY + Duration::from_secs(1));
		producer.create_group(6u64.into()).unwrap().finish().unwrap();

		assert!(consumer.peek_group(2).is_none(), "expired backfill is reclaimed");
		assert!(pool.used() < used, "its bytes are released");
	}

	/// Append a finished group of a 3 byte first frame and a `delta` byte delta.
	fn refresh_group(producer: &Producer, delta: usize) {
		let mut group = producer.append_group().unwrap();
		group.write_frame(None, &b"key"[..]).unwrap();
		if delta > 0 {
			group.write_frame(None, vec![0u8; delta]).unwrap();
		}
		group.finish().unwrap();
	}

	/// Poll `demand` for a change without waiting.
	fn wanted_changed(demand: &mut Demand) -> Option<Option<u64>> {
		match demand.poll_wanted_changed(&kio::Waiter::noop()) {
			Poll::Ready(res) => Some(res.unwrap()),
			Poll::Pending => None,
		}
	}

	/// The first consumer of a track with no group wants group 0, and nobody wants it once
	/// the last one leaves.
	#[test]
	fn wanted_follows_the_first_consumer() {
		let producer = track_producer("test", Info::default().with_refresh(1));
		let mut demand = producer.demand();
		assert_eq!(demand.wanted(), None);
		assert_eq!(wanted_changed(&mut demand), Some(None), "the first call reports");
		assert_eq!(wanted_changed(&mut demand), None, "then only changes");

		let subscriber = producer.subscribe(None);
		assert_eq!(demand.wanted(), Some(0));
		assert_eq!(wanted_changed(&mut demand), Some(Some(0)));
		drop(subscriber);
		assert_eq!(demand.wanted(), None);
		assert_eq!(wanted_changed(&mut demand), Some(None));
	}

	/// A join reads the newest group, and once its bytes after the first frame exceed the
	/// ratio times the first frame it also wants the group after the newest. The cached group
	/// is served either way: refresh is a request, not a hold.
	#[test]
	fn a_join_over_the_ratio_wants_a_new_group() {
		for (delta, wanted) in [(0, 0), (3, 0), (4, 1)] {
			let producer = track_producer("test", Info::default().with_refresh(1));
			refresh_group(&producer, delta);
			let mut subscriber = producer.subscribe(None);
			assert_eq!(producer.demand().wanted(), Some(wanted), "{delta} byte delta");
			assert_eq!(subscriber.assert_group().sequence, 0, "{delta} byte delta");
		}
	}

	/// A handed-out group keeps its track wanted once its subscription leaves, until a
	/// reader reads its end: a finished group owes nothing more, even while still held.
	#[test]
	fn a_group_read_keeps_its_track_wanted_until_its_end() {
		let producer = track_producer("test", Info::timed());
		let mut subscriber = producer.subscribe(None);
		let mut writing = producer.append_group().unwrap();
		writing.write_frame(Timestamp::ZERO, &b"key"[..]).unwrap();
		let mut group = subscriber.assert_group();
		let other = group.clone();
		drop(subscriber);
		assert_eq!(producer.demand().wanted(), Some(0), "the group is read");

		writing.finish().unwrap();
		assert_eq!(producer.demand().wanted(), Some(0), "not read to its end yet");
		let waiter = kio::Waiter::noop();
		assert!(matches!(group.poll_read_frame(&waiter), Poll::Ready(Ok(Some(_)))));
		assert!(matches!(group.poll_read_frame(&waiter), Poll::Ready(Ok(None))));
		assert_eq!(
			producer.demand().wanted(),
			None,
			"a clone reading the end ends it for all"
		);
		drop(other);
	}

	/// A reader stopping at its range cap has not reached the group's end: the group is
	/// still open, so it keeps its track wanted for the others and for a raised cap.
	#[test]
	fn a_range_cap_is_not_the_groups_end() {
		let producer = track_producer("test", Info::timed());
		let mut subscriber = producer.subscribe(None);
		let mut writing = producer.append_group().unwrap();
		writing.write_frame(Timestamp::ZERO, &b"key"[..]).unwrap();
		let group = subscriber.assert_group();
		let mut capped = group.clone().with_range(..1);
		drop(subscriber);

		let waiter = kio::Waiter::noop();
		assert!(matches!(capped.poll_read_frame(&waiter), Poll::Ready(Ok(Some(_)))));
		assert!(matches!(capped.poll_read_frame(&waiter), Poll::Ready(Ok(None))));
		assert!(matches!(capped.poll_finished(&waiter), Poll::Ready(Ok(1))));
		assert_eq!(producer.demand().wanted(), Some(0), "the cap is not the group's end");
		drop(group);
		let producer = producer
			.abort_unwanted(Error::Cancel)
			.expect_err("the capped reader can raise its cap");

		writing.write_frame(Timestamp::ZERO, &b"delta"[..]).unwrap();
		let mut capped = capped.with_range(..);
		assert!(matches!(capped.poll_read_frame(&waiter), Poll::Ready(Ok(Some(_)))));
		writing.finish().unwrap();
		assert!(matches!(capped.poll_read_frame(&waiter), Poll::Ready(Ok(None))));
		assert_eq!(producer.demand().wanted(), None, "read to the group's end");
	}

	/// A fetched group keeps its track wanted after the fetch resolved, while it is read.
	#[test]
	#[cfg(feature = "async")]
	fn a_fetched_group_keeps_its_track_wanted() {
		let producer = track_producer("test", Info::timed());
		let mut writing = producer.append_group().unwrap();
		writing.write_frame(Timestamp::ZERO, &b"key"[..]).unwrap();
		let group = producer
			.consume()
			.fetch_group(0, None)
			.now_or_never()
			.expect("cached")
			.unwrap();
		assert_eq!(producer.demand().wanted(), Some(0));
		drop(group);
		assert_eq!(producer.demand().wanted(), None);
	}

	/// A datagram past the newest group took the sequence after it, so a join over the
	/// ratio wants the next free one rather than one already written.
	#[test]
	fn a_join_wants_past_a_later_datagram() {
		let mut producer = track_producer("test", Info::default().with_refresh(1));
		refresh_group(&producer, 4);
		assert_eq!(producer.append_datagram(None, &b"dgram"[..]).unwrap(), 1);
		let _subscriber = producer.subscribe(None);
		assert_eq!(producer.demand().wanted(), Some(2));
	}

	/// A datagram taking the sequence a pending refresh asked for moves the ask past it, so
	/// it still names a group the publisher has to open. The group landing answers it.
	#[test]
	fn a_datagram_never_overtakes_a_pending_refresh() {
		let mut producer = track_producer("test", Info::default().with_refresh(1));
		let mut demand = producer.demand();
		refresh_group(&producer, 4);
		let _subscriber = producer.subscribe(None);
		assert_eq!(wanted_changed(&mut demand), Some(Some(1)));

		assert_eq!(producer.append_datagram(None, &b"dgram"[..]).unwrap(), 1);
		assert_eq!(wanted_changed(&mut demand), Some(Some(2)), "the datagram took group 1");

		refresh_group(&producer, 0);
		assert_eq!(wanted_changed(&mut demand), None, "group 2 answers it");
		producer.append_datagram(None, &b"dgram"[..]).unwrap();
		assert_eq!(wanted_changed(&mut demand), None, "nothing is pending");
		assert_eq!(demand.wanted(), Some(2));
	}

	/// Joins while a request is pending ask for the same group, so the publisher hears it
	/// once. The group reaching it leaves nothing pending, and only a join over the ratio of
	/// that group asks again.
	#[test]
	fn joins_coalesce_until_the_group_lands() {
		let producer = track_producer("test", Info::default().with_refresh(1));
		let mut demand = producer.demand();
		refresh_group(&producer, 4);
		let _a = producer.subscribe(None);
		assert_eq!(wanted_changed(&mut demand), Some(Some(1)));
		let _b = producer.subscribe(None);
		assert_eq!(wanted_changed(&mut demand), None, "coalesced");

		refresh_group(&producer, 0);
		assert_eq!(wanted_changed(&mut demand), None, "group 1 answers it");
		let _c = producer.subscribe(None);
		assert_eq!(wanted_changed(&mut demand), None, "group 1 is under the ratio");

		refresh_group(&producer, 4);
		let _d = producer.subscribe(None);
		assert_eq!(wanted_changed(&mut demand), Some(Some(3)));
	}

	/// A track without a ratio never asks for a group on a join.
	#[test]
	fn joins_without_refresh_ask_nothing() {
		let producer = track_producer("test", Info::default());
		refresh_group(&producer, 1000);
		let _subscriber = producer.subscribe(None);
		assert_eq!(producer.demand().wanted(), Some(0));
	}

	/// Only a join at the live edge counts: a subscription resuming from a fixed group
	/// already has the newest group's start.
	#[test]
	fn a_fixed_start_is_no_join() {
		let producer = track_producer("test", Info::default().with_refresh(1));
		refresh_group(&producer, 4);
		let _subscriber = producer.subscribe(Subscription::default().with_range(0..));
		assert_eq!(producer.demand().wanted(), Some(0));
	}

	/// The demand reports the track's closure as its abort reason.
	#[test]
	fn wanted_changed_errors_once_closed() {
		let producer = track_producer("test", Info::default());
		let mut demand = producer.demand();
		producer.abort(Error::Cancel).unwrap();
		assert!(matches!(
			demand.poll_wanted_changed(&kio::Waiter::noop()),
			Poll::Ready(Err(Error::Cancel))
		));
		assert_eq!(demand.wanted(), None);
	}

	/// A NEW_GROUP_REQUEST joins again: under the ratio it asks nothing, over it the group
	/// past the newest.
	#[test]
	fn a_subscriber_joins_again() {
		let producer = track_producer("test", Info::default().with_refresh(1));
		let subscriber = producer.subscribe(None);
		refresh_group(&producer, 3);
		subscriber.join();
		assert_eq!(producer.demand().wanted(), Some(0));
		refresh_group(&producer, 4);
		subscriber.join();
		assert_eq!(producer.demand().wanted(), Some(2));
	}

	#[test]
	#[cfg(feature = "async")]
	fn request_and_dynamic_share_weak_track_demand() {
		let request = Request::new(Arc::new(broadcast::Info::default()), "demand");
		let dynamic = request.requests();
		let demand = request.demand();
		let consumer = request.consume();
		assert!(!demand.is_wanted(), "a bare handle is not demand");
		let query = consumer.info();
		assert!(demand.is_wanted());
		assert!(dynamic.demand().is_wanted());
		drop(query);
		assert!(!demand.is_wanted());
		let producer = request.accept(Info::timed());
		assert_eq!(producer.demand().name(), demand.name());
		drop(producer);
		drop(dynamic);
		assert!(matches!(
			demand.clone().wanted_changed().now_or_never(),
			Some(Err(Error::Dropped))
		));
	}

	/// Every fetch that joined a fill counts as its demand, and the last to leave
	/// withdraws it.
	#[test]
	fn fill_demand_counts_every_joined_caller() {
		let producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		let consumer = producer.consume();
		let first = consumer.fetch_group(3, None);
		let second = consumer.fetch_group(3, None);
		let waiter = kio::Waiter::noop();
		let Poll::Ready(Ok(fill)) = dynamic.poll_requested_fill(&waiter) else {
			panic!("one fill for both fetches");
		};
		assert!(dynamic.poll_requested_fill(&waiter).is_pending(), "the second joined");
		assert_eq!((fill.start(), fill.end()), (Position::group(3), 4));
		assert_eq!(fill.wanted(), Some(3));
		drop(first);
		assert!(fill.poll_withdrawn(&waiter).is_pending());
		drop(second);
		assert!(fill.poll_withdrawn(&waiter).is_ready());
		assert_eq!(fill.wanted(), None);
		drop(fill);
		assert!(
			!producer.state.read().drops.contains_key(&3),
			"a withdrawn fill records nothing"
		);
	}

	/// A fill accepting a group resolves the fetch from the cache.
	#[test]
	fn accepted_fill_resolves_its_fetch() {
		let producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		let consumer = producer.consume();
		let mut fetch = consumer.fetch_group(3, None);
		let waiter = kio::Waiter::noop();
		let Poll::Ready(Ok(fill)) = dynamic.poll_requested_fill(&waiter) else {
			panic!("the fill is ready");
		};
		let mut group = fill.accept(3, Info::timed()).unwrap();
		assert!(matches!(fill.accept(3, None), Err(Error::Duplicate)));
		assert!(matches!(fill.accept(4, None), Err(Error::InvalidRange)));
		assert!(
			matches!(kio::Task::poll(&mut *fetch, &waiter), Poll::Ready(Ok(_))),
			"the fetch gets the accepted group"
		);
		group.finish().unwrap();
	}

	/// A feed declared to start at group 2, with groups 2 and 3 written, read from group 0.
	#[cfg(feature = "async")]
	fn started_late() -> (Producer, Requests, Subscriber) {
		let mut producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		let subscriber = producer.subscribe(replay().with_range(0..));
		producer.start_at(2).unwrap();
		write_group(&producer, 2);
		write_group(&producer, 3);
		(producer, dynamic, subscriber)
	}

	fn write_filled(group: Result<group::Producer>, millis: u64) {
		let mut group = group.unwrap();
		group
			.write_frame(Timestamp::from_millis(millis).unwrap(), &b"x"[..])
			.unwrap();
		group.finish().unwrap();
	}

	#[cfg(feature = "async")]
	fn drained_drops(subscriber: &mut Subscriber) -> Vec<(u64, u64)> {
		let mut dropped = Vec::new();
		while let Poll::Ready((range, _)) = subscriber.poll_drop(&kio::Waiter::noop()) {
			dropped.push((range.start, range.end));
		}
		dropped
	}

	/// What a reader asks for under the feed's start is a miss, asked at once: the reader
	/// reads what the handler accepts, and what it refuses is named dropped only once the
	/// feed gives up on it too, since it may still arrive late.
	#[test]
	#[cfg(feature = "async")]
	fn a_miss_below_the_start_is_filled() {
		let (producer, dynamic, mut subscriber) = started_late();
		let waiter = kio::Waiter::noop();
		assert!(drained_drops(&mut subscriber).is_empty());
		let mut read = Vec::new();
		while let Poll::Ready(Ok(Some(group))) = subscriber.poll_recv_group(&waiter) {
			read.push(group.sequence);
		}
		assert_eq!(read, [2, 3]);
		let Poll::Ready(Ok(fill)) = dynamic.poll_requested_fill(&waiter) else {
			panic!("below the start is a miss");
		};
		assert_eq!((fill.start().group, fill.end()), (0, 2));
		write_filled(fill.accept(0, None), 0);
		fill.drop_groups(1..2);
		let Poll::Ready(Ok(Some(group))) = subscriber.poll_recv_group(&waiter) else {
			panic!("the filled group is read");
		};
		assert_eq!(group.sequence, 0);
		assert!(subscriber.poll_recv_group(&waiter).is_pending());
		assert!(drained_drops(&mut subscriber).is_empty(), "group 1 may still arrive");

		elapse(&producer, Duration::from_secs(31));
		assert_eq!(drained_drops(&mut subscriber), [(1, 2)]);
		assert!(
			dynamic.poll_requested_fill(&waiter).is_pending(),
			"a refusal is not asked again"
		);
	}

	/// A copy whose fills cannot carry a reader's budget asks none below the start its
	/// publisher resolved from that budget, and names nothing there dropped: what a wider
	/// budget reaches comes through the feed's re-offer, which a reader takes as it arrives.
	#[test]
	#[cfg(feature = "async")]
	fn no_fill_below_a_start_resolved_from_the_budget() {
		let write = |producer: &Producer, sequence| {
			let mut group = producer.create_group(group::Info { sequence }).unwrap();
			group.write_frame(None, &b"x"[..]).unwrap();
			group.finish().unwrap();
		};
		let mut producer = track_producer("test", Info::default());
		producer.fills_from_start();
		let dynamic = producer.requests();
		let mut subscriber = producer.subscribe(replay().with_range(0..));
		producer.request_start(None).unwrap();
		producer.start_at(2).unwrap();
		write(&producer, 2);
		write(&producer, 3);
		let waiter = kio::Waiter::noop();
		assert_eq!(drain_groups(&mut subscriber), [2, 3]);
		assert!(
			dynamic.poll_requested_fill(&waiter).is_pending(),
			"below the start is no miss"
		);
		assert!(drained_drops(&mut subscriber).is_empty());

		// The re-offer of a widened budget.
		write(&producer, 1);
		assert_eq!(drain_groups(&mut subscriber), [1]);
		assert!(dynamic.poll_requested_fill(&waiter).is_pending());
	}

	/// Below a start its publisher resolved from a budget, a reader no looser than it gives
	/// every group up unasked, while a looser one may still reach some, so it asks.
	#[test]
	#[cfg(feature = "async")]
	fn below_a_start_resolved_from_a_budget_only_a_looser_reader_asks() {
		let mut producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		let mut narrow = producer.subscribe(replay());
		let mut wide = producer.subscribe(Subscription::default().with_max_delay(Duration::from_secs(60)));
		producer.start_resolved(2, Duration::from_secs(30)).unwrap();
		write_group(&producer, 2);
		write_group(&producer, 3);
		let waiter = kio::Waiter::noop();
		assert_eq!(drain_groups(&mut narrow), [2, 3]);
		assert_eq!(drained_drops(&mut narrow), [(0, 2)]);
		assert!(
			dynamic.poll_requested_fill(&waiter).is_pending(),
			"the narrow reader asks nothing"
		);

		assert_eq!(drain_groups(&mut wide), [2, 3]);
		let Poll::Ready(Ok(fill)) = dynamic.poll_requested_fill(&waiter) else {
			panic!("the wide reader asks below the start");
		};
		assert_eq!((fill.start().group, fill.end()), (0, 2));
	}

	/// A reader arriving after another's fill cached the groups under the start reads them
	/// from the cache: the arrival walk never reaches a backfill, and no fill is asked again.
	#[test]
	#[cfg(feature = "async")]
	fn a_newcomer_reads_the_backfill_another_fill_cached() {
		let (producer, dynamic, mut first) = started_late();
		let waiter = kio::Waiter::noop();
		while let Poll::Ready(Ok(Some(_))) = first.poll_recv_group(&waiter) {}
		let Poll::Ready(Ok(fill)) = dynamic.poll_requested_fill(&waiter) else {
			panic!("below the start is a miss");
		};
		while let Some(sequence) = fill.wanted() {
			write_filled(fill.accept(sequence, None), sequence);
		}
		let mut newcomer = producer.subscribe(replay().with_range(0..));
		let mut read = Vec::new();
		while let Poll::Ready(Ok(Some(group))) = newcomer.poll_recv_group(&waiter) {
			read.push(group.sequence);
		}
		read.sort();
		assert_eq!(read, [0, 1, 2, 3]);
		assert!(
			dynamic.poll_requested_fill(&waiter).is_pending(),
			"the cache answers it"
		);
	}

	/// Groups a fill answered before the reader came back are handed out in the reader's
	/// order, so a warm cache keeps a descending reader newest first.
	#[test]
	#[cfg(feature = "async")]
	fn filled_groups_are_read_in_the_readers_order() {
		for order in [Order::Descending, Order::Ascending] {
			let mut producer = track_producer("test", Info::timed());
			let dynamic = producer.requests();
			let mut subscriber = producer.subscribe(replay().with_range(0..).with_order(order));
			producer.start_at(2).unwrap();
			write_group(&producer, 2);
			let waiter = kio::Waiter::noop();
			while let Poll::Ready(Ok(Some(_))) = subscriber.poll_recv_group(&waiter) {}
			let Poll::Ready(Ok(fill)) = dynamic.poll_requested_fill(&waiter) else {
				panic!("below the start is a miss");
			};
			while let Some(sequence) = fill.wanted() {
				write_filled(fill.accept(sequence, None), sequence);
			}
			let mut read = Vec::new();
			while let Poll::Ready(Ok(Some(group))) = subscriber.poll_recv_group(&waiter) {
				read.push(group.sequence);
			}
			let expected = match order {
				Order::Descending => [1, 0],
				Order::Ascending => [0, 1],
			};
			assert_eq!(read, expected, "{order:?}");
		}
	}

	/// An ordered reader waiting on a stalled fill gives up the groups it owes once they are
	/// stale under its budget, as it skips a stale cached group, and reads on.
	#[test]
	#[cfg(feature = "async")]
	fn a_stalled_fill_goes_stale_under_the_readers_budget() {
		let (producer, dynamic, mut subscriber) = started_late();
		let waiter = kio::Waiter::noop();
		let next = |subscriber: &mut Subscriber| match subscriber.poll_next_group(&waiter) {
			Poll::Ready(Ok(Some(group))) => Some(group.sequence),
			Poll::Pending => None,
			_ => panic!("the read ended"),
		};
		assert_eq!(next(&mut subscriber), None, "groups 0 and 1 are being filled");
		// The handler takes the fill and never answers it.
		let Poll::Ready(Ok(fill)) = dynamic.poll_requested_fill(&waiter) else {
			panic!("below the start is a miss");
		};
		elapse(&producer, Duration::from_secs(29));
		assert_eq!(next(&mut subscriber), None, "still within the budget");
		elapse(&producer, Duration::from_secs(2));
		// Group 2 is as stale as the groups below it, superseded by group 3 as long ago.
		assert_eq!(next(&mut subscriber), Some(3));
		assert_eq!(drained_drops(&mut subscriber), [(0, 2)]);
		assert_eq!(fill.wanted(), None, "nobody waits on the stale fill");
	}

	/// Groups arriving out of order inside a stalled fill judge each stretch between them by
	/// its own successor: a fresh late group keeps the groups below it owed while the stretches
	/// around it go stale.
	#[test]
	#[cfg(feature = "async")]
	fn a_late_fresh_group_keeps_its_stretch_of_a_stalled_fill() {
		let mut producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		let mut subscriber = producer.subscribe(replay().with_range(0..));
		producer.start_at(10).unwrap();
		write_group(&producer, 10);
		let waiter = kio::Waiter::noop();
		assert!(
			subscriber.poll_next_group(&waiter).is_pending(),
			"group 0 is being filled"
		);
		// The handler takes the fill and never answers it.
		let Poll::Ready(Ok(fill)) = dynamic.poll_requested_fill(&waiter) else {
			panic!("below the start is a miss");
		};
		assert_eq!((fill.start().group, fill.end()), (0, 10));
		write_group(&producer, 2);
		write_group(&producer, 8);
		elapse(&producer, Duration::from_secs(20));
		write_group(&producer, 5);
		elapse(&producer, Duration::from_secs(11));

		let Poll::Ready(Ok(Some(group))) = subscriber.poll_next_group(&waiter) else {
			panic!("groups 0 and 1 went stale behind group 2");
		};
		assert_eq!(group.sequence, 2, "superseded by group 5 only 11s ago");
		assert!(
			subscriber.poll_next_group(&waiter).is_pending(),
			"group 3 is still owed"
		);
		for sequence in [3, 4] {
			assert!(fill.is_wanted(sequence), "group {sequence} is superseded by group 5");
		}
		for sequence in [0, 1, 6, 7, 9] {
			assert!(!fill.is_wanted(sequence), "group {sequence} went stale");
		}
	}

	/// A fetched group aborted once its readers left is a hole, not an arrival: the fill asked
	/// to replace it keeps it wanted.
	#[test]
	#[cfg(feature = "async")]
	fn an_aborted_fetched_group_stays_wanted_by_its_replacement_fill() {
		let mut producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		producer.start_at(10).unwrap();
		write_group(&producer, 10);
		let waiter = kio::Waiter::noop();
		let fetch = producer.consume().fetch_group(3, None);
		let Poll::Ready(Ok(first)) = dynamic.poll_requested_fill(&waiter) else {
			panic!("below the start is a miss");
		};
		let group = first.accept(3, None).unwrap();
		drop(fetch);
		group.abort(Error::Cancel).unwrap();
		drop(first);

		let mut subscriber = producer.subscribe(replay().with_range(0..));
		assert!(
			subscriber.poll_next_group(&waiter).is_pending(),
			"group 0 is being filled"
		);
		let Poll::Ready(Ok(fill)) = dynamic.poll_requested_fill(&waiter) else {
			panic!("the aborted group is a miss again");
		};
		assert_eq!((fill.start().group, fill.end()), (0, 10));
		assert!(fill.is_wanted(3), "the aborted group never arrived");
	}

	/// Refusing an evicted group the feed already passed fails the readers waiting on it
	/// without naming it dropped, so a later reader of the whole group still gets a fill.
	#[test]
	#[cfg(feature = "async")]
	fn a_refused_group_is_asked_again_by_a_later_reader() {
		let producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		for sequence in 0..5 {
			write_group(&producer, sequence);
		}
		evict(&producer, 3);
		let waiter = kio::Waiter::noop();
		let tail = producer.consume().fetch_group(
			3,
			group::Fetch {
				frame_start: 2,
				..Default::default()
			},
		);
		let Poll::Ready(Ok(first)) = dynamic.poll_requested_fill(&waiter) else {
			panic!("an evicted group is a miss");
		};
		first.refuse_groups(3..4, Error::Unsupported);
		assert!(matches!(tail.now_or_never(), Some(Err(Error::Unsupported))));
		drop(first);

		let mut subscriber = producer.subscribe(replay().with_range(3..4));
		assert!(
			subscriber.poll_next_group(&waiter).is_pending(),
			"group 3 is being filled"
		);
		let Poll::Ready(Ok(fill)) = dynamic.poll_requested_fill(&waiter) else {
			panic!("the source may still have group 3");
		};
		assert!(fill.is_wanted(3));
	}

	/// A source refusing a fetch as not found answers only that request, such as a head that
	/// expired while the tail is kept, so a later reader still gets a fill for the group.
	#[test]
	#[cfg(feature = "async")]
	fn a_source_miss_is_asked_again_by_a_later_reader() {
		let producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		for sequence in 0..5 {
			write_group(&producer, sequence);
		}
		evict(&producer, 3);
		let waiter = kio::Waiter::noop();
		let fetch = producer.consume().fetch_group(3, None);
		let Poll::Ready(Ok(first)) = dynamic.poll_requested_fill(&waiter) else {
			panic!("an evicted group is a miss");
		};
		first.group(3).reject(Error::NotFound);
		assert!(matches!(fetch.now_or_never(), Some(Err(Error::NotFound))));
		drop(first);

		let mut subscriber = producer.subscribe(replay().with_range(3..4));
		assert!(
			subscriber.poll_next_group(&waiter).is_pending(),
			"group 3 is being filled"
		);
		let Poll::Ready(Ok(fill)) = dynamic.poll_requested_fill(&waiter) else {
			panic!("the source may still have part of group 3");
		};
		assert!(fill.is_wanted(3));
	}

	/// A backfill cached before its fill publishes the answer is not a live arrival: the reader
	/// keeps it owed, so the answer still reaches it.
	#[test]
	#[cfg(feature = "async")]
	fn a_backfill_cached_before_its_answer_stays_owed() {
		let mut producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		let ascending = |groups| replay().with_range(groups).with_order(Order::Ascending);
		let mut subscriber = producer.subscribe(ascending(0..));
		producer.start_at(10).unwrap();
		write_group(&producer, 10);
		let waiter = kio::Waiter::noop();
		while let Poll::Ready(Ok(Some(_))) = subscriber.poll_recv_group(&waiter) {}
		let Poll::Ready(Ok(fill)) = dynamic.poll_requested_fill(&waiter) else {
			panic!("below the start is a miss");
		};
		// The window inside `Fill::accept` between caching the group and publishing the answer.
		let _writing = producer
			.state
			.write()
			.ok()
			.unwrap()
			.insert_group_request(3, group::Bounds::default(), None)
			.unwrap();
		assert!(subscriber.poll_recv_group(&waiter).is_pending());
		assert!(fill.is_wanted(3), "the answer has not reached the reader yet");
	}

	/// An ordered reader taking a filled group leaves the fill at once, so a reader that drops
	/// the group without reading on stops holding the fill's source.
	#[test]
	#[cfg(feature = "async")]
	fn an_ordered_reader_leaves_a_filled_group_it_takes() {
		let mut producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		let mut subscriber = producer.subscribe(replay().with_range(0..1));
		producer.start_at(10).unwrap();
		write_group(&producer, 10);
		let waiter = kio::Waiter::noop();
		assert!(
			subscriber.poll_next_group(&waiter).is_pending(),
			"group 0 is being filled"
		);
		let Poll::Ready(Ok(fill)) = dynamic.poll_requested_fill(&waiter) else {
			panic!("below the start is a miss");
		};
		let _writing = fill.accept(0, None).unwrap();
		let Poll::Ready(Ok(Some(group))) = subscriber.poll_next_group(&waiter) else {
			panic!("the filled group is read");
		};
		assert_eq!(group.sequence, 0);
		drop(group);
		assert!(fill.poll_withdrawn(&waiter).is_ready(), "nobody waits on the fill now");
	}

	/// A parked [`Fill::poll_wanted_changed`] wakes on every move of the group owed: a
	/// reader narrowing past it, an answer, and the last reader leaving, which ends at `None`.
	#[test]
	fn fill_wanted_changed_follows_the_group_owed() {
		let mut producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		let ascending = |groups| replay().with_range(groups).with_order(Order::Ascending);
		let mut subscriber = producer.subscribe(ascending(0..));
		producer.start_at(10).unwrap();
		write_group(&producer, 10);
		let noop = kio::Waiter::noop();
		while let Poll::Ready(Ok(Some(_))) = subscriber.poll_recv_group(&noop) {}
		let Poll::Ready(Ok(mut fill)) = dynamic.poll_requested_fill(&noop) else {
			panic!("below the start is a miss");
		};
		let (flag, waiter) = Flag::waiter();
		assert_eq!(
			fill.poll_wanted_changed(&waiter),
			Poll::Ready(Some(0)),
			"the first poll reports"
		);
		assert!(fill.poll_wanted_changed(&waiter).is_pending());

		subscriber.update(ascending(8..)).unwrap();
		assert!(subscriber.poll_recv_group(&noop).is_pending());
		assert!(flag.take(), "narrowing woke the parked poll");
		assert_eq!(fill.poll_wanted_changed(&waiter), Poll::Ready(Some(8)));
		assert!(fill.poll_wanted_changed(&waiter).is_pending());

		write_filled(fill.accept(8, None), 8);
		assert!(flag.take(), "an answer woke the parked poll");
		assert_eq!(fill.poll_wanted_changed(&waiter), Poll::Ready(Some(9)));
		assert!(fill.poll_wanted_changed(&waiter).is_pending());

		drop(subscriber);
		assert!(flag.take(), "the last reader leaving woke the parked poll");
		assert_eq!(fill.poll_wanted_changed(&waiter), Poll::Ready(None));
		assert_eq!(fill.wanted(), None);
	}

	/// The owed group arriving through the live feed wakes a parked
	/// [`Fill::poll_wanted_changed`]: the cache answers it, so the fill no longer owes it.
	#[test]
	#[cfg(feature = "async")]
	fn fill_wanted_changed_follows_a_live_arrival() {
		let mut producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		let ascending = |groups| replay().with_range(groups).with_order(Order::Ascending);
		let mut subscriber = producer.subscribe(ascending(9..));
		producer.start_at(10).unwrap();
		write_group(&producer, 10);
		let noop = kio::Waiter::noop();
		while let Poll::Ready(Ok(Some(_))) = subscriber.poll_recv_group(&noop) {}
		let Poll::Ready(Ok(mut fill)) = dynamic.poll_requested_fill(&noop) else {
			panic!("below the start is a miss");
		};
		let (flag, waiter) = Flag::waiter();
		assert_eq!(fill.poll_wanted_changed(&waiter), Poll::Ready(Some(9)));
		assert!(fill.poll_wanted_changed(&waiter).is_pending());

		write_group(&producer, 9);
		assert!(flag.take(), "the owed group arriving live woke the parked poll");
		assert_eq!(fill.poll_wanted_changed(&waiter), Poll::Ready(None));
		assert_eq!(fill.wanted(), None);
		drop(subscriber);
	}

	/// A reader narrowing its ranges leaves the part of its fill it no longer reads: the
	/// handler is not asked for it, a fetch of it in flight is cancelled, and neither a bulk
	/// refusal nor dropping the fill names it dropped.
	#[test]
	#[cfg(feature = "async")]
	fn a_narrowed_reader_leaves_the_rest_of_its_fill() {
		let mut producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		let ascending = |groups| replay().with_range(groups).with_order(Order::Ascending);
		let mut subscriber = producer.subscribe(ascending(0..));
		producer.start_at(10).unwrap();
		write_group(&producer, 10);
		let waiter = kio::Waiter::noop();
		while let Poll::Ready(Ok(Some(_))) = subscriber.poll_recv_group(&waiter) {}
		let Poll::Ready(Ok(fill)) = dynamic.poll_requested_fill(&waiter) else {
			panic!("below the start is a miss");
		};
		assert_eq!(fill.wanted(), Some(0));
		assert!(fill.is_wanted(0));
		assert!(fill.poll_unwanted(0, &waiter).is_pending());

		subscriber.update(ascending(8..)).unwrap();
		assert!(subscriber.poll_recv_group(&waiter).is_pending());
		assert!(fill.poll_unwanted(0, &waiter).is_ready(), "nobody reads group 0 now");
		assert!(!fill.is_wanted(0));
		assert!(fill.is_wanted(8));
		assert!(fill.poll_unwanted(8, &waiter).is_pending());
		assert_eq!(fill.wanted(), Some(8), "nobody reads below 8 any more");
		write_filled(fill.accept(8, None), 8);
		assert_eq!(fill.wanted(), Some(9));
		// Over, so what the fill refuses is a drop rather than a group still to come.
		producer.finish().unwrap();
		fill.drop_groups(0..9);
		drop(fill);

		let Poll::Ready(Ok(Some(group))) = subscriber.poll_recv_group(&waiter) else {
			panic!("the filled group is read");
		};
		assert_eq!(group.sequence, 8);
		assert_eq!(drained_drops(&mut subscriber), [(9, 10)], "only what it still read");
		assert!(producer.state.read().drops.overlapping(&(0..8)).next().is_none());
	}

	/// A group the fill accepted stays wanted until its reader takes it, even once the rest of
	/// the fill is answered, so its source keeps writing it meanwhile; narrowing past it
	/// before then leaves it.
	#[test]
	#[cfg(feature = "async")]
	fn a_filled_group_is_wanted_until_taken() {
		let mut producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		let ascending = |groups| replay().with_range(groups).with_order(Order::Ascending);
		let mut subscriber = producer.subscribe(ascending(0..));
		producer.start_at(10).unwrap();
		write_group(&producer, 10);
		let waiter = kio::Waiter::noop();
		while let Poll::Ready(Ok(Some(_))) = subscriber.poll_recv_group(&waiter) {}
		let Poll::Ready(Ok(fill)) = dynamic.poll_requested_fill(&waiter) else {
			panic!("below the start is a miss");
		};
		let _writing = [fill.accept(0, None).unwrap(), fill.accept(1, None).unwrap()];
		for sequence in 2..10 {
			fill.group(sequence).reject(Error::NotFound);
		}
		let Poll::Ready(Ok(Some(group))) = subscriber.poll_recv_group(&waiter) else {
			panic!("the first filled group is read");
		};
		assert_eq!(group.sequence, 0);
		assert!(!fill.is_wanted(0), "taken: its reader holds the group itself");
		assert!(fill.is_wanted(1), "filled, not taken yet");

		subscriber.update(ascending(8..)).unwrap();
		assert!(subscriber.poll_recv_group(&waiter).is_pending());
		assert!(!fill.is_wanted(1), "nobody reads group 1 now");
		assert!(fill.poll_unwanted(1, &waiter).is_ready());
	}

	/// A session still writing an accepted group stops once every reader narrowed past it,
	/// though the fill still owes them later groups.
	#[test]
	#[cfg(feature = "async")]
	fn an_accepted_groups_readers_leave_before_its_fill() {
		let mut producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		let ascending = |groups| replay().with_range(groups).with_order(Order::Ascending);
		let mut subscriber = producer.subscribe(ascending(0..));
		producer.start_at(10).unwrap();
		write_group(&producer, 10);
		let waiter = kio::Waiter::noop();
		while let Poll::Ready(Ok(Some(_))) = subscriber.poll_recv_group(&waiter) {}
		let Poll::Ready(Ok(fill)) = dynamic.poll_requested_fill(&waiter) else {
			panic!("below the start is a miss");
		};
		let group = fill.group(0);
		let readers = group.readers();
		let _writing = group.accept(None).unwrap();
		assert!(readers.poll_unwanted(&waiter).is_pending());

		subscriber.update(ascending(1..)).unwrap();
		assert!(subscriber.poll_recv_group(&waiter).is_pending());
		assert!(readers.poll_unwanted(&waiter).is_ready(), "nobody reads group 0 now");
		assert!(
			fill.poll_withdrawn(&waiter).is_pending(),
			"the fill still owes 1 and up"
		);
	}

	/// A time start before every group held asks no fill below the oldest, since which of
	/// those groups it reaches is unknown until they start; another range still asks for its
	/// own misses.
	#[test]
	#[cfg(feature = "async")]
	fn a_time_start_before_the_cache_asks_nothing_below_it() {
		let mut producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		let at = |millis| Timestamp::from_millis(millis).unwrap();
		let since = Range {
			start: Anchor::Time(at(5)),
			end: None,
		};
		let mut subscriber = producer.subscribe(
			replay()
				.with_ranges([Range::groups(0..2), since])
				.with_order(Order::Ascending),
		);
		producer.start_at(10).unwrap();
		write_group(&producer, 10);
		let waiter = kio::Waiter::noop();
		let mut read = drain_groups(&mut subscriber);
		let Poll::Ready(Ok(fill)) = dynamic.poll_requested_fill(&waiter) else {
			panic!("the group range misses its groups");
		};
		assert_eq!(
			(fill.start().group, fill.end()),
			(0, 2),
			"nothing below 10 for the time start"
		);
		while let Some(sequence) = fill.wanted() {
			write_filled(fill.accept(sequence, None), sequence);
		}
		drop(fill);
		read.extend(drain_groups(&mut subscriber));
		assert_eq!(read, [10, 0, 1]);
		assert!(dynamic.poll_requested_fill(&waiter).is_pending());
	}

	/// A wide fill refused group by group, each with its own error, holds only the answers its
	/// readers have yet to take, not one per refusal.
	#[test]
	#[cfg(feature = "async")]
	fn a_wide_fill_forgets_refusals_its_readers_took() {
		let mut producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		let mut subscriber = producer.subscribe(replay().with_range(0..).with_order(Order::Ascending));
		producer.start_at(100).unwrap();
		write_group(&producer, 100);
		let waiter = kio::Waiter::noop();
		while let Poll::Ready(Ok(Some(_))) = subscriber.poll_recv_group(&waiter) {}
		let Poll::Ready(Ok(fill)) = dynamic.poll_requested_fill(&waiter) else {
			panic!("below the start is a miss");
		};
		assert_eq!((fill.start().group, fill.end()), (0, 100));
		for sequence in 0..50 {
			assert_eq!(fill.wanted(), Some(sequence));
			fill.group(sequence).reject(Error::App(sequence as u16));
			let _ = subscriber.poll_recv_group(&waiter);
			assert!(fill.answers() <= 1, "group {sequence}: {} answers held", fill.answers());
		}
	}

	/// What a fill stopped owing is asked afresh by a reader that wants it later, rather than
	/// joining a fetch its handler is cancelling.
	#[test]
	#[cfg(feature = "async")]
	fn a_later_reader_asks_afresh_for_what_a_fill_stopped_owing() {
		let mut producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		let ascending = |groups| replay().with_range(groups).with_order(Order::Ascending);
		let mut narrowed = producer.subscribe(ascending(0..));
		producer.start_at(10).unwrap();
		write_group(&producer, 10);
		let waiter = kio::Waiter::noop();
		while let Poll::Ready(Ok(Some(_))) = narrowed.poll_recv_group(&waiter) {}
		let Poll::Ready(Ok(fill)) = dynamic.poll_requested_fill(&waiter) else {
			panic!("below the start is a miss");
		};
		narrowed.update(ascending(8..)).unwrap();
		assert!(narrowed.poll_recv_group(&waiter).is_pending());

		let mut later = producer.subscribe(ascending(0..));
		while let Poll::Ready(Ok(Some(_))) = later.poll_recv_group(&waiter) {}
		let Poll::Ready(Ok(again)) = dynamic.poll_requested_fill(&waiter) else {
			panic!("what the first fill stopped owing is asked afresh");
		};
		assert_eq!((again.start().group, again.end()), (0, 8));
		assert_eq!(fill.wanted(), Some(8), "the first fill still owes only 8 and up");
		assert!(!fill.is_wanted(0), "a group stops being wanted for good");
	}

	/// A descending reader's disjoint misses queue newest first, so a handler serving one fill
	/// at a time reaches the newer history first.
	#[test]
	#[cfg(feature = "async")]
	fn a_descending_reader_asks_its_newest_miss_first() {
		let mut producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		let mut subscriber =
			producer.subscribe(replay().with_ranges([Range::groups(0..2), Range::groups(4..6), Range::groups(8..)]));
		producer.start_at(8).unwrap();
		write_group(&producer, 8);
		let waiter = kio::Waiter::noop();
		while let Poll::Ready(Ok(Some(_))) = subscriber.poll_recv_group(&waiter) {}
		let mut asked = Vec::new();
		while let Poll::Ready(Ok(fill)) = dynamic.poll_requested_fill(&waiter) {
			asked.push((fill.start().group, fill.end()));
		}
		assert_eq!(asked, [(4, 6), (0, 2)]);
	}

	/// A reader forgets the misses it gave up once its window moves past them, as it forgets
	/// what it judged, so a feed skipping every other group costs it only the gaps it can
	/// still read.
	#[test]
	#[cfg(feature = "async")]
	fn a_reader_forgets_misses_behind_its_window() {
		let mut producer = track_producer("test", Info::timed());
		let _dynamic = producer.requests();
		let ascending = |groups| Subscription::default().with_range(groups).with_order(Order::Ascending);
		let mut subscriber = producer.subscribe(ascending(0..));
		producer.start_at(0).unwrap();
		for sequence in (0..20).step_by(2) {
			write_group(&producer, sequence);
			drain_groups(&mut subscriber);
			elapse(&producer, Duration::from_secs(2));
			drain_groups(&mut subscriber);
		}
		let given_up = |subscriber: &Subscriber| -> Vec<(u64, u64)> {
			let Inner::Plain(cursor) = &subscriber.inner else {
				unreachable!("a plain subscription");
			};
			let misses = cursor.misses.as_ref().expect("the reader judged its misses");
			misses.given_up().iter().map(|run| (run.start, run.end)).collect()
		};
		assert_eq!(given_up(&subscriber).len(), 9, "each skipped group is given up");

		subscriber.update(ascending(15..)).unwrap();
		write_group(&producer, 20);
		drain_groups(&mut subscriber);
		assert_eq!(given_up(&subscriber), [(15, 16), (17, 18)]);
	}

	/// A descending reader's miss split by a fill already in flight queues its newer fragment
	/// first too.
	#[test]
	#[cfg(feature = "async")]
	fn a_descending_reader_asks_its_newest_fragment_first() {
		let mut producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		let mut middle = producer.subscribe(replay().with_range(5..6));
		producer.start_at(8).unwrap();
		write_group(&producer, 8);
		let waiter = kio::Waiter::noop();
		while let Poll::Ready(Ok(Some(_))) = middle.poll_recv_group(&waiter) {}
		let Poll::Ready(Ok(taken)) = dynamic.poll_requested_fill(&waiter) else {
			panic!("group 5 is a miss");
		};
		assert_eq!((taken.start().group, taken.end()), (5, 6));

		let mut subscriber = producer.subscribe(replay().with_range(0..).with_order(Order::Descending));
		while let Poll::Ready(Ok(Some(_))) = subscriber.poll_recv_group(&waiter) {}
		let mut asked = Vec::new();
		while let Poll::Ready(Ok(fill)) = dynamic.poll_requested_fill(&waiter) {
			asked.push((fill.start().group, fill.end()));
		}
		assert_eq!(asked, [(6, 8), (0, 5)]);
	}

	/// A group the handler refused that still arrives live is delivered.
	#[test]
	#[cfg(feature = "async")]
	fn a_refused_miss_that_arrives_late_is_read() {
		let (producer, dynamic, mut subscriber) = started_late();
		let waiter = kio::Waiter::noop();
		assert!(drained_drops(&mut subscriber).is_empty());
		while let Poll::Ready(Ok(Some(_))) = subscriber.poll_recv_group(&waiter) {}
		let Poll::Ready(Ok(fill)) = dynamic.poll_requested_fill(&waiter) else {
			panic!("below the start is a miss");
		};
		fill.drop_groups(0..2);
		write_group(&producer, 1);
		let Poll::Ready(Ok(Some(group))) = subscriber.poll_recv_group(&waiter) else {
			panic!("the late group is read");
		};
		assert_eq!(group.sequence, 1);
		elapse(&producer, Duration::from_secs(31));
		assert_eq!(drained_drops(&mut subscriber), [(0, 1)]);
	}

	/// A refused miss is named dropped as soon as the feed ends, since nothing more arrives,
	/// and the read ends with it.
	#[test]
	#[cfg(feature = "async")]
	fn a_refused_miss_is_dropped_once_the_feed_ends() {
		let (producer, dynamic, mut subscriber) = started_late();
		let waiter = kio::Waiter::noop();
		while let Poll::Ready(Ok(Some(_))) = subscriber.poll_recv_group(&waiter) {}
		let Poll::Ready(Ok(fill)) = dynamic.poll_requested_fill(&waiter) else {
			panic!("below the start is a miss");
		};
		fill.drop_groups(0..2);
		assert!(subscriber.poll_recv_group(&waiter).is_pending());
		assert!(
			drained_drops(&mut subscriber).is_empty(),
			"the feed may still deliver them"
		);
		producer.finish().unwrap();
		assert_eq!(drained_drops(&mut subscriber), [(0, 2)]);
		assert!(matches!(subscriber.poll_recv_group(&waiter), Poll::Ready(Ok(None))));
	}

	/// An end declared ahead of the live edge does not end the feed yet: a group missing
	/// below the edge may still arrive, so it is not named dropped.
	#[test]
	#[cfg(feature = "async")]
	fn an_end_ahead_of_the_edge_keeps_a_late_group() {
		let mut producer = track_producer("test", Info::timed());
		let mut subscriber = producer.subscribe(replay().with_range(0..));
		for sequence in [0, 1, 2, 4, 5] {
			write_group(&producer, sequence);
		}
		producer.finish_at(Position::group(10)).unwrap();
		assert_eq!(drain_groups(&mut subscriber), [0, 1, 2, 4, 5]);
		assert!(drained_drops(&mut subscriber).is_empty(), "group 3 may still arrive");
		write_group(&producer, 3);
		assert_eq!(drain_groups(&mut subscriber), [3]);
		assert!(drained_drops(&mut subscriber).is_empty());
	}

	/// With no handler to fill it, a miss is named dropped once the feed gives up on it.
	#[test]
	#[cfg(feature = "async")]
	fn a_miss_with_no_handler_is_named_dropped() {
		let (producer, dynamic, mut subscriber) = started_late();
		drop(dynamic);
		let waiter = kio::Waiter::noop();
		assert!(drained_drops(&mut subscriber).is_empty());
		let mut read = Vec::new();
		while let Poll::Ready(Ok(Some(group))) = subscriber.poll_recv_group(&waiter) {
			read.push(group.sequence);
		}
		assert_eq!(read, [2, 3]);
		assert!(drained_drops(&mut subscriber).is_empty());
		elapse(&producer, Duration::from_secs(31));
		assert_eq!(drained_drops(&mut subscriber), [(0, 2)]);
	}

	/// A fill whose handler leaves before taking it settles by run: a peer that starts its
	/// feed far past the reader's floor makes the miss wide, and walking it group by group
	/// would never finish.
	#[test]
	#[cfg(feature = "async")]
	fn a_wide_fill_its_handler_left_settles_by_run() {
		const WIDE: u64 = 1 << 40;
		let mut producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		let mut subscriber = producer.subscribe(replay().with_range(0..));
		producer.start_at(WIDE).unwrap();
		write_group(&producer, WIDE);
		assert_eq!(drain_groups(&mut subscriber), [WIDE]);
		drop(dynamic);
		assert!(subscriber.poll_recv_group(&kio::Waiter::noop()).is_pending());
		assert!(
			drained_drops(&mut subscriber).is_empty(),
			"the feed may still deliver them"
		);
		elapse(&producer, Duration::from_secs(31));
		assert_eq!(drained_drops(&mut subscriber), [(0, WIDE)]);
	}

	/// A handler refusing a wide fill one group at a time, as a FETCH-capable session does,
	/// pays for each refusal rather than for every refusal before it.
	#[test]
	#[cfg(feature = "async")]
	fn a_fill_refused_group_by_group_stays_linear() {
		const WIDE: u64 = 1 << 40;
		const REFUSED: u64 = 100_000;
		let mut producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		let mut subscriber = producer.subscribe(replay().with_range(0..));
		producer.start_at(WIDE).unwrap();
		write_group(&producer, WIDE);
		assert_eq!(drain_groups(&mut subscriber), [WIDE]);

		let Poll::Ready(Ok(fill)) = dynamic.poll_requested_fill(&kio::Waiter::noop()) else {
			panic!("the miss below the start is asked");
		};
		for _ in 0..REFUSED {
			let sequence = fill.wanted().unwrap();
			fill.drop_groups(sequence..sequence + 1);
			assert!(subscriber.poll_recv_group(&kio::Waiter::noop()).is_pending());
		}
		assert_eq!(fill.wanted(), Some(WIDE - REFUSED - 1));
	}

	/// A descending reader joining a queued fill makes it newest first, as any descending
	/// subscriber does a subscription.
	#[test]
	fn a_descending_reader_turns_a_queued_fill_newest_first() {
		let mut producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		let mut ascending = producer.subscribe(replay().with_range(0..).with_order(Order::Ascending));
		let mut descending = producer.subscribe(replay().with_range(0..));
		producer.start_at(4).unwrap();
		write_group(&producer, 4);
		let _ = ascending.poll_recv_group(&kio::Waiter::noop());
		let _ = descending.poll_recv_group(&kio::Waiter::noop());

		let Poll::Ready(Ok(fill)) = dynamic.poll_requested_fill(&kio::Waiter::noop()) else {
			panic!("the miss below the start is asked");
		};
		assert_eq!((fill.start().group, fill.end()), (0, 4));
		assert_eq!(fill.order(), Order::Descending);
		assert!(
			dynamic.poll_requested_fill(&kio::Waiter::noop()).is_pending(),
			"both readers share the fill"
		);
	}

	/// A reader changing its priority, budget, or order moves the fill it waits on, queued
	/// or taken, so the handler serves the rest under the new values, even ones lower than
	/// it was asked with.
	#[test]
	#[cfg(feature = "async")]
	fn a_reader_update_reaches_its_fill() {
		let mut producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		let mut subscriber = producer.subscribe(replay().with_range(0..).with_order(Order::Ascending));
		producer.start_at(4).unwrap();
		write_group(&producer, 4);
		let waiter = kio::Waiter::noop();
		assert_eq!(drain_groups(&mut subscriber), [4]);

		subscriber.update(subscriber.subscription().with_priority(3)).unwrap();
		assert!(subscriber.poll_recv_group(&waiter).is_pending());
		let Poll::Ready(Ok(fill)) = dynamic.poll_requested_fill(&waiter) else {
			panic!("the miss below the start is asked");
		};
		assert_eq!((fill.start().group, fill.end()), (0, 4));
		assert_eq!(
			(fill.priority(), fill.order()),
			(3, Order::Ascending),
			"refreshed while queued"
		);
		assert_eq!(fill.wanted(), Some(0));
		write_filled(fill.accept(0, None), 0);

		let budget = Duration::from_secs(20);
		let update = subscriber
			.subscription()
			.with_priority(-2)
			.with_max_delay(budget)
			.with_order(Order::Descending);
		subscriber.update(update).unwrap();
		assert_eq!(drain_groups(&mut subscriber), [0]);
		assert_eq!(fill.priority(), -2);
		assert_eq!(fill.max_delay(), Some(budget));
		assert_eq!(fill.order(), Order::Descending);
		assert_eq!(fill.wanted(), Some(3), "the rest is served newest first");
	}

	/// An ordered reader waits on a fill for its miss rather than skipping past it.
	#[test]
	#[cfg(feature = "async")]
	fn an_ordered_read_waits_on_a_fill() {
		let (_producer, dynamic, mut subscriber) = started_late();
		let waiter = kio::Waiter::noop();
		let mut next = || match subscriber.poll_next_group(&waiter) {
			Poll::Ready(Ok(Some(group))) => Some(group.sequence),
			Poll::Pending => None,
			_ => panic!("the read ended"),
		};
		assert_eq!(next(), None, "group 0 is being filled");
		let Poll::Ready(Ok(fill)) = dynamic.poll_requested_fill(&waiter) else {
			panic!("below the start is a miss");
		};
		write_filled(fill.accept(0, None), 0);
		assert_eq!(next(), Some(0));
		assert_eq!(next(), None, "group 1 is being filled");
		fill.drop_groups(1..2);
		assert_eq!(next(), Some(2));
		assert_eq!(next(), Some(3));
	}

	/// A fill nobody waits on any more is withdrawn: it names nothing dropped, so a later
	/// reader can still ask for the same history.
	#[test]
	#[cfg(feature = "async")]
	fn a_withdrawn_fill_names_nothing_dropped() {
		let (producer, dynamic, mut subscriber) = started_late();
		let waiter = kio::Waiter::noop();
		while let Poll::Ready(Ok(Some(_))) = subscriber.poll_recv_group(&waiter) {}
		let Poll::Ready(Ok(fill)) = dynamic.poll_requested_fill(&waiter) else {
			panic!("the gap is a miss");
		};
		drop(subscriber);
		assert_eq!(fill.wanted(), None);
		drop(fill);
		assert!(drops(&producer).is_empty());
	}

	/// Fetch group 3 and drop it through the fill it queues, returning how the fetch ended.
	fn drop_through_fill(producer: &Producer, dynamic: &Requests, drop_all: bool) -> Error {
		let consumer = producer.consume();
		let mut fetch = consumer.fetch_group(3, None);
		let waiter = kio::Waiter::noop();
		let Poll::Ready(Ok(fill)) = dynamic.poll_requested_fill(&waiter) else {
			panic!("the fill is ready");
		};
		match drop_all {
			true => fill.drop_groups(0..10),
			false => drop(fill),
		}
		match kio::Task::poll(&mut *fetch, &waiter) {
			Poll::Ready(Err(err)) => err,
			_ => panic!("the fetch fails"),
		}
	}

	fn drops(producer: &Producer) -> Vec<ops::Range<u64>> {
		producer
			.state
			.read()
			.drops
			.iter()
			.map(|(range, _)| range.clone())
			.collect()
	}

	/// A group the source no longer has fails its fetch, and is named dropped for good only
	/// once the live feed gave up on it too: a fetch answered before a late group header
	/// arrives says nothing about the feed.
	#[moq_clock::test]
	async fn dropped_fill_group_fails_its_fetch() {
		let producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		write_group(&producer, 5);
		assert!(matches!(drop_through_fill(&producer, &dynamic, true), Error::NotFound));
		assert!(drops(&producer).is_empty(), "the feed may still deliver 3");

		moq_clock::sim::advance(Duration::from_secs(60)).await;
		assert!(matches!(drop_through_fill(&producer, &dynamic, true), Error::NotFound));
		assert_eq!(drops(&producer), vec![3..4], "clipped to the fill");
	}

	/// Dropping a fill a reader still waits on names what it left as dropped.
	#[moq_clock::test]
	async fn fill_dropped_unanswered_names_the_rest_dropped() {
		let producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		write_group(&producer, 5);
		moq_clock::sim::advance(Duration::from_secs(60)).await;
		assert!(matches!(drop_through_fill(&producer, &dynamic, false), Error::Dropped));
		assert_eq!(drops(&producer), vec![3..4]);
	}

	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn fetch_aborts_with_track() {
		let producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		let consumer = producer.consume();

		let mut pending = consumer.fetch_group(3, None);
		assert!(kio::Task::poll(&mut *pending, &kio::Waiter::noop()).is_pending());

		producer.abort(Error::Cancel).unwrap();
		assert!(pending.await.is_err());
		drop(dynamic);
	}

	/// Write a finished one-frame group, presented at its sequence in milliseconds.
	fn write_group(producer: &Producer, sequence: u64) {
		let mut group = producer.create_group(group::Info { sequence }).unwrap();
		group
			.write_frame(Timestamp::from_millis(sequence).unwrap(), &b"x"[..])
			.unwrap();
		group.finish().unwrap();
	}

	/// Every group the subscriber hands out right now, in arrival order.
	fn drain_groups(subscriber: &mut Subscriber) -> Vec<u64> {
		let mut sequences = Vec::new();
		while let Poll::Ready(Ok(Some(group))) = subscriber.poll_recv_group(&kio::Waiter::noop()) {
			sequences.push(group.sequence);
		}
		sequences
	}

	/// A sequence the publisher aborts and re-creates is handed out once.
	#[test]
	fn a_recreated_group_is_handed_out_once() {
		let producer = track_producer("test", Info::timed());
		let mut subscriber = producer.subscribe(replay());
		write_group(&producer, 0);
		let open = producer.create_group(group::Info { sequence: 1 }).unwrap();
		assert_eq!(drain_groups(&mut subscriber), [0, 1]);

		open.abort(Error::Cancel).unwrap();
		write_group(&producer, 1);
		write_group(&producer, 2);
		assert_eq!(drain_groups(&mut subscriber), [2]);
	}

	/// A gap is remembered until a group above it is past the budget, max delay or max
	/// age alike. Then every group below that one counts as handed out.
	#[test]
	fn a_gap_is_forgotten_once_a_group_above_it_is_past_the_budget() {
		let budget = Duration::from_millis(10);
		let readers = [
			(Info::timed(), Subscription::default().with_max_delay(budget)),
			(
				Info::timed().with_max_age(budget),
				Subscription::default().with_max_delay(Duration::from_secs(3600)),
			),
		];
		for (info, subscription) in readers {
			let producer = track_producer("test", info);
			let mut subscriber = producer.subscribe(subscription);
			// Group 3 never arrives.
			for sequence in [0, 1, 2, 4, 5, 6] {
				write_group(&producer, sequence);
			}
			assert_eq!(drain_groups(&mut subscriber), [0, 1, 2, 4, 5, 6]);
			// Group 4 reaches until group 5 presents at 5ms, so it is past a 10ms budget once
			// the edge presents at 15ms.
			for sequence in 7..=14 {
				write_group(&producer, sequence);
				assert_eq!(drain_groups(&mut subscriber), [sequence]);
				assert_eq!(subscriber.delivered_runs(), 2, "the gap at 3 is kept at {sequence}");
			}
			write_group(&producer, 15);
			assert_eq!(drain_groups(&mut subscriber), [15]);
			assert_eq!(subscriber.delivered_runs(), 1, "the gap at 3 is forgotten");
			write_group(&producer, 3);
			assert_eq!(drain_groups(&mut subscriber), Vec::<u64>::new());
		}
	}

	/// An endless track with groups missing remembers only the gaps within the budget, and
	/// still delivers an unseen group that arrives late within it.
	#[test]
	fn an_endless_track_with_gaps_stays_bounded() {
		let producer = track_producer("test", Info::timed());
		// Groups present 1ms apart, so the budget spans ten of them.
		let mut subscriber = producer.subscribe(Subscription::default().with_max_delay(Duration::from_millis(10)));
		let mut most = 0;
		for sequence in 0..3000u64 {
			// Every third group never arrives.
			if sequence % 3 == 1 {
				continue;
			}
			write_group(&producer, sequence);
			assert_eq!(drain_groups(&mut subscriber), [sequence]);
			most = most.max(subscriber.delivered_runs());
		}
		assert_eq!(most, 5, "the floor and the gaps within the budget");

		// 2998 is within the budget, 2500 far past it.
		write_group(&producer, 2998);
		write_group(&producer, 2500);
		assert_eq!(drain_groups(&mut subscriber), [2998]);
	}

	/// A group of one untimed frame at `sequence`.
	fn write_untimed_group(producer: &Producer, sequence: u64) {
		let mut group = producer.create_group(group::Info { sequence }).unwrap();
		group.write_frame(None, &b"x"[..]).unwrap();
		group.finish().unwrap();
	}

	/// The wall clock forgets the gaps on an untimed track, which has no media time to
	/// measure them by.
	#[test]
	fn an_endless_untimed_track_with_gaps_stays_bounded() {
		let producer = track_producer("test", None);
		// Groups arrive 1ms apart, so the budget spans ten of them.
		let mut subscriber = producer.subscribe(Subscription::default().with_max_delay(Duration::from_millis(10)));
		let mut most = 0;
		for sequence in 0..3000u64 {
			elapse(&producer, Duration::from_millis(1));
			// Every third group never arrives.
			if sequence % 3 == 1 {
				continue;
			}
			write_untimed_group(&producer, sequence);
			assert_eq!(drain_groups(&mut subscriber), [sequence]);
			most = most.max(subscriber.delivered_runs());
		}
		assert_eq!(most, 5, "the floor and the gaps within the budget");

		// 2998 is within the budget, 2500 far past it.
		write_untimed_group(&producer, 2998);
		write_untimed_group(&producer, 2500);
		assert_eq!(drain_groups(&mut subscriber), [2998]);
	}

	/// An empty successor gives the lowest group above a gap no reach, so media time never
	/// forgets that gap or any after it. The wall clock still does.
	#[test]
	fn an_empty_successor_still_lets_the_gaps_go() {
		let producer = track_producer("test", Info::timed());
		let mut subscriber = producer.subscribe(Subscription::default().with_max_delay(Duration::from_millis(10)));
		let mut most = 0;
		for sequence in 0..3000u64 {
			elapse(&producer, Duration::from_millis(1));
			if sequence % 3 == 1 {
				continue;
			}
			// Group 2 sits above the first gap, and its successor never presents a frame.
			match sequence {
				3 => producer
					.create_group(group::Info { sequence })
					.unwrap()
					.finish()
					.unwrap(),
				_ => write_group(&producer, sequence),
			}
			assert_eq!(drain_groups(&mut subscriber), [sequence]);
			most = most.max(subscriber.delivered_runs());
		}
		assert_eq!(most, 5, "the floor and the gaps within the budget");
	}

	/// A gap within the budget below a run of frameless groups is forgotten once the group
	/// above it is superseded for a full budget, so the run stops being rescanned.
	#[test]
	fn a_gap_below_a_frameless_run_lets_the_floor_advance() {
		const RUN: u64 = 100;
		let producer = track_producer("test", Info::timed());
		let mut subscriber = producer.subscribe(Subscription::default().with_max_delay(Duration::from_secs(1)));
		write_group(&producer, 0);
		// Group 1 never arrives.
		let open: Vec<_> = (2..2 + RUN)
			.map(|sequence| producer.create_group(group::Info { sequence }).unwrap())
			.collect();
		assert_eq!(
			drain_groups(&mut subscriber),
			[0].into_iter().chain(2..2 + RUN).collect::<Vec<_>>()
		);
		assert_eq!(subscriber.delivered_runs(), 2, "the floor and the run above the gap");

		elapse(&producer, Duration::from_secs(1));
		write_group(&producer, 2 + RUN);
		assert_eq!(drain_groups(&mut subscriber), [2 + RUN]);
		assert_eq!(subscriber.delivered_runs(), 1, "the gap is forgotten");
		drop(open);
	}

	/// Gaps below groups aborted since, but still cached, share the successor past them, so
	/// one hand-out that puts them all past the budget forgets every one. The gap below a
	/// group still within it is kept. `track_gap_flush` measures the cost.
	#[test]
	fn gaps_below_aborted_groups_are_forgotten_together() {
		let producer = track_producer("test", Info::timed());
		let mut subscriber = producer.subscribe(Subscription::default().with_max_delay(Duration::from_secs(1)));
		let mut handed = Vec::new();
		for sequence in (0..20).step_by(2) {
			let mut group = producer.create_group(group::Info { sequence }).unwrap();
			group
				.write_frame(Timestamp::from_millis(sequence).unwrap(), &b"x"[..])
				.unwrap();
			handed.push(group);
		}
		assert_eq!(drain_groups(&mut subscriber).len(), 10);
		for group in handed.drain(..5) {
			group.abort(Error::Cancel).unwrap();
		}
		// Groups 0 to 8 reach until group 10 at 10ms, which a group at 1.01s puts past the
		// budget. Group 10 reaches until 12ms, so the gap at 9 is kept.
		write_group(&producer, 1010);
		assert_eq!(drain_groups(&mut subscriber), [1010]);
		assert_eq!(
			subscriber.delivered_runs(),
			7,
			"the floor at 8, then 10 to 18 and 1010 apart"
		);
	}

	/// A common wall-clock successor retires the gaps below an aborted prefix, but a
	/// later arrival keeps the next gap within its budget.
	#[test]
	fn wall_time_batches_aborted_gaps_without_forgetting_a_fresh_successor() {
		let budget = Duration::from_secs(1);
		let producer = track_producer("test", None);
		let mut subscriber = producer.subscribe(Subscription::default().with_max_delay(budget));
		let mut open = Vec::new();
		for sequence in (0..=10).step_by(2) {
			let mut group = producer.create_group(group::Info { sequence }).unwrap();
			group.write_frame(None, &b"x"[..]).unwrap();
			open.push(group);
		}
		assert_eq!(drain_groups(&mut subscriber), [0, 2, 4, 6, 8, 10]);
		for group in open.drain(..5) {
			group.abort(Error::Cancel).unwrap();
		}
		elapse(&producer, budget / 2);
		write_untimed_group(&producer, 12);
		assert_eq!(drain_groups(&mut subscriber), [12]);
		elapse(&producer, budget / 2);
		write_untimed_group(&producer, 14);
		assert_eq!(drain_groups(&mut subscriber), [14]);
		assert_eq!(subscriber.delivered_runs(), 4, "floor through 8, then 10, 12 and 14");
		assert!(!subscriber.delivered.contains(9), "the next gap is still remembered");
		write_untimed_group(&producer, 11);
		assert_eq!(
			drain_groups(&mut subscriber),
			[11],
			"the gap below fresh group 12 remains"
		);
	}

	/// An untimed sparse feed whose groups all arrive within the budget keeps every gap
	/// and still hands out every group. The hand-out cost stays flat over the retained
	/// gaps; `track_sparse_read` measures it.
	#[test]
	fn an_untimed_sparse_feed_keeps_its_gaps() {
		let producer = untimed_producer();
		let mut subscriber = producer.subscribe(replay());
		for sequence in (0..2000u64).step_by(2) {
			let mut group = producer.create_group(group::Info { sequence }).unwrap();
			group.write_frame(None, &b"x"[..]).unwrap();
			group.finish().unwrap();
			assert_eq!(drain_groups(&mut subscriber), [sequence]);
		}
		assert_eq!(subscriber.delivered_runs(), 1000);
	}

	/// A track whose newest group is `newest`, every group fresh to [`replay`].
	#[cfg(feature = "async")]
	fn quiet_track(newest: u64) -> Producer {
		let mut producer = track_producer("test", Info::timed());
		for millis in 0..=newest {
			append_at(&mut producer, millis);
		}
		producer
	}

	/// A live range merged with one above the live edge still starts at the newest group,
	/// and the aggregate keeps both rather than flooring the live one at the higher start.
	#[test]
	#[cfg(feature = "async")]
	fn a_live_range_merged_with_one_above_the_live_edge_gets_the_newest_group() {
		let mut producer = quiet_track(3);
		let mut ahead = producer.subscribe(replay().with_range(4..));
		let mut live = producer.subscribe(replay().with_ranges([Range::live()]));
		assert_eq!(
			producer.subscription().unwrap().ranges,
			vec![Range::groups(4..), Range::live()]
		);
		assert_eq!(drain(&mut ahead), Vec::<u64>::new());
		assert_eq!(drain(&mut live), vec![3]);

		append_at(&mut producer, 4);
		assert_eq!(drain(&mut ahead), vec![4]);
		assert_eq!(drain(&mut live), vec![4]);
	}

	/// A live range on a track with no groups yet starts at the first one to arrive.
	#[test]
	#[cfg(feature = "async")]
	fn a_live_range_on_an_empty_track_starts_at_the_first_group() {
		let mut producer = track_producer("test", Info::timed());
		let mut live = producer.subscribe(replay().with_ranges([Range::live()]));
		assert_eq!(drain(&mut live), Vec::<u64>::new());
		append_at(&mut producer, 0);
		append_at(&mut producer, 1);
		assert_eq!(drain(&mut live), vec![0, 1]);
	}

	/// A live range starts at the newest group even when a datagram took a later sequence.
	#[test]
	#[cfg(feature = "async")]
	fn a_live_range_ignores_a_datagram_past_the_newest_group() {
		let mut producer = track_producer("test", Info::timed());
		append_at(&mut producer, 0);
		assert_eq!(
			producer
				.append_datagram(Timestamp::from_millis(1).unwrap(), &b"x"[..])
				.unwrap(),
			1
		);
		let mut live = producer.subscribe(replay().with_ranges([Range::live()]));
		assert_eq!(drain(&mut live), vec![0]);
	}

	/// A live range on a track without groups delivers datagrams as they arrive, rather than
	/// holding them for a group a datagram-only track never produces.
	#[test]
	#[cfg(feature = "async")]
	fn a_live_range_on_a_track_without_groups_delivers_datagrams() {
		let mut producer = track_producer("test", Info::timed());
		let mut live = producer.subscribe(replay().with_ranges([Range::live()]));
		producer
			.append_datagram(Timestamp::from_millis(0).unwrap(), &b"x"[..])
			.unwrap();
		assert_eq!(recv_datagram(&mut live).sequence, 0);
	}

	/// Fetched backfill past the newest produced group moves no relative start, whether the
	/// track had a produced group at subscribe or not.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn a_live_range_ignores_a_fetched_group_past_the_newest_group() {
		let producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		let consumer = producer.consume();
		let mut early = producer.subscribe(replay().with_ranges([Range::live()]));
		let _fetching = consumer.fetch_group(5, None);
		let req = dynamic.requested_fill_group().now_or_never().unwrap().unwrap();
		req.accept(None).unwrap().finish().unwrap();
		producer
			.create_group(group::Info { sequence: 0 })
			.unwrap()
			.finish()
			.unwrap();
		let mut late = producer.subscribe(replay().with_ranges([Range::live()]));
		assert_eq!(drain(&mut early), vec![0]);
		assert_eq!(drain(&mut late), vec![0]);
	}

	/// A relative start on an empty track counts back from the first group to arrive, even
	/// once that group is evicted before the subscriber reads.
	#[test]
	#[cfg(feature = "async")]
	fn a_relative_start_on_an_empty_track_survives_its_first_group_leaving() {
		let producer = untimed_producer();
		let buffering = Range {
			start: Anchor::Relative(2),
			end: None,
		};
		let mut behind = producer.subscribe(replay().with_ranges([buffering]));
		let write = |sequence| {
			let mut group = producer.create_group(group::Info { sequence }).unwrap();
			group.write_frame(None, &b"x"[..]).unwrap();
			group.finish().unwrap();
		};
		write(10);
		elapse(&producer, cache::DEFAULT_EXPIRY + Duration::from_secs(1));
		write(11);
		assert!(!producer.state.read().lookup.contains_key(&10), "group 10 evicted");
		write(8);
		let mut delivered = drain(&mut behind);
		delivered.sort_unstable();
		assert_eq!(delivered, vec![8, 11]);
	}

	/// A relative start buffers that many groups behind the live edge, whatever a range
	/// above the edge merged beside it asks for.
	#[test]
	#[cfg(feature = "async")]
	fn a_buffering_range_merged_with_one_above_the_live_edge_keeps_its_backlog() {
		let mut producer = quiet_track(5);
		let mut ahead = producer.subscribe(replay().with_range(8..));
		let buffering = Range {
			start: Anchor::Relative(2),
			end: None,
		};
		let mut behind = producer.subscribe(replay().with_ranges([buffering]));
		assert_eq!(
			producer.subscription().unwrap().ranges,
			vec![Range::groups(8..), buffering]
		);
		assert_eq!(drain(&mut behind), vec![3, 4, 5]);

		for millis in 6..=8 {
			append_at(&mut producer, millis);
		}
		assert_eq!(drain(&mut behind), vec![6, 7, 8]);
		assert_eq!(drain(&mut ahead), vec![8]);
	}

	/// An untimed track replays from group 0 only what the wall clock hasn't made stale.
	#[test]
	#[cfg(feature = "async")]
	fn an_untimed_range_from_group_zero_replays_what_is_still_fresh() {
		let mut producer = untimed_producer();
		for _ in 0..5 {
			append_untimed(&mut producer);
			elapse(&producer, Duration::from_secs(1));
		}
		let budget = Duration::from_millis(1500);
		let mut subscriber = producer.subscribe(Subscription::default().with_max_delay(budget).with_range(0..));
		assert_eq!(drain(&mut subscriber), vec![3, 4]);
	}

	/// A group `sequence` whose first frame lands at `millis`, or the write's refusal. A
	/// refused group is aborted, as a caller would.
	fn start_at_millis(producer: &Producer, sequence: u64, millis: u64) -> Result<group::Producer> {
		let mut group = producer.create_group(group::Info { sequence })?;
		match group.write_frame(Timestamp::from_millis(millis).unwrap(), &b"x"[..]) {
			Ok(()) => Ok(group),
			Err(err) => {
				group.abort(err.clone()).unwrap();
				Err(err)
			}
		}
	}

	/// Take group `sequence` out of the cache as eviction does.
	fn evict(producer: &Producer, sequence: u64) {
		producer
			.modify()
			.unwrap()
			.uncache(sequence)
			.unwrap()
			.abort(Error::Evicted);
	}

	/// A rewound first frame is refused and leaves the group as it was, so the publisher can
	/// still write a start at or after the group before it.
	#[test]
	fn an_appended_rewind_is_refused() {
		let mut producer = track_producer("test", Info::timed());
		append_at(&mut producer, 1000);

		let mut group = producer.append_group().unwrap();
		let refused = group.write_frame(Timestamp::from_millis(500).unwrap(), &b"x"[..]);
		assert!(matches!(refused, Err(Error::TimestampMismatch)), "{refused:?}");
		assert_eq!(group.frame_count(), 0);
		assert_eq!(group.handle().timestamp(), None);

		// An equal start is a keyframe at the previous group's start.
		group
			.write_frame(Timestamp::from_millis(1000).unwrap(), &b"x"[..])
			.unwrap();
	}

	/// The one-shot write ends the group it appended rather than leaving it open.
	#[test]
	fn a_rewound_single_frame_group_is_aborted() {
		let mut producer = track_producer("test", Info::timed());
		append_at(&mut producer, 1000);
		let refused = producer.write_frame(Timestamp::from_millis(500).unwrap(), &b"x"[..]);
		assert!(matches!(refused, Err(Error::TimestampMismatch)), "{refused:?}");
		let state = producer.state.read();
		assert!(state.lookup.get(&1).unwrap().group.is_aborted());
	}

	/// Groups land out of sequence order, so a start is bounded from above as well as below.
	#[test]
	fn a_rewind_is_refused_whichever_group_lands_first() {
		let producer = track_producer("test", Info::timed());
		start_at_millis(&producer, 5, 1000).unwrap().finish().unwrap();
		assert!(matches!(
			start_at_millis(&producer, 3, 2000),
			Err(Error::TimestampMismatch)
		));
		start_at_millis(&producer, 3, 500).unwrap().finish().unwrap();
		assert!(matches!(
			start_at_millis(&producer, 4, 400),
			Err(Error::TimestampMismatch)
		));
		start_at_millis(&producer, 4, 1000).unwrap().finish().unwrap();
	}

	/// The batched and streamed writes check frame 0 the same way, before writing anything.
	#[test]
	fn every_write_path_refuses_a_rewind() {
		let mut producer = track_producer("test", Info::timed());
		append_at(&mut producer, 1000);
		let rewound = Timestamp::from_millis(500).unwrap();

		let mut batched = producer.append_group().unwrap();
		let mut frames = frame::Buffer::<4>::new();
		frames
			.push(frame::Frame::new(rewound, bytes::Bytes::from_static(b"x")))
			.unwrap();
		assert!(matches!(
			batched.write_frames(&mut frames),
			Err(Error::TimestampMismatch)
		));
		assert_eq!(frames.len(), 1, "the batch is left to retry");
		assert_eq!(batched.frame_count(), 0);

		let streamed = producer.append_group().unwrap();
		let handle = streamed.handle();
		let refused = streamed
			.create_frame(frame::Info {
				size: 1,
				timestamp: Some(rewound),
			})
			.err();
		assert!(matches!(refused, Some(Error::TimestampMismatch)), "{refused:?}");
		assert_eq!(handle.frame_count(), 0);
		assert!(handle.is_aborted(), "a refused frame aborts its group");
	}

	/// Eviction is not a sequence prefix: a group stamped after its predecessor was evicted
	/// is still held above it, though an older group survives below.
	#[test]
	fn an_evicted_predecessor_still_bounds_a_delayed_stamp() {
		let producer = track_producer("test", Info::timed());
		start_at_millis(&producer, 0, 0).unwrap().finish().unwrap();
		start_at_millis(&producer, 1, 1000).unwrap().finish().unwrap();
		let mut delayed = producer.create_group(group::Info { sequence: 2 }).unwrap();
		evict(&producer, 1);

		let refused = delayed.write_frame(Timestamp::from_millis(500).unwrap(), &b"x"[..]);
		assert!(matches!(refused, Err(Error::TimestampMismatch)), "{refused:?}");
		delayed
			.write_frame(Timestamp::from_millis(1000).unwrap(), &b"x"[..])
			.unwrap();
	}

	/// A group at or below the eviction watermark is checked against what survives, and
	/// otherwise accepted unverified: it bounds no later group, so a refetch of evicted
	/// history is never refused on its account, and it drives no media-time expiry.
	#[test]
	fn a_late_group_below_the_watermark_is_unverified() {
		let producer = track_producer("test", Info::timed());
		for (sequence, millis) in [(0, 1000), (1, 2000), (2, 3000), (3, 4000)] {
			start_at_millis(&producer, sequence, millis).unwrap().finish().unwrap();
		}
		evict(&producer, 1);
		evict(&producer, 2);

		// Below the surviving group 0, or above the watermark at group 2.
		for millis in [500, 3500] {
			assert!(matches!(
				start_at_millis(&producer, 1, millis),
				Err(Error::TimestampMismatch)
			));
		}
		let late = start_at_millis(&producer, 1, 2900).unwrap();
		assert_eq!(late.handle().bound(), None, "a late group bounds nothing");
		assert_eq!(late.handle().timestamp(), None, "nor drives media-time expiry");

		// Group 2 really started before late group 1 claimed to.
		start_at_millis(&producer, 2, 2500).unwrap().finish().unwrap();
	}

	/// Subscribers may have seen an aborted group's start, so a replacement at its sequence
	/// leaves that start as the watermark, and no later group can start before it.
	#[test]
	fn a_replaced_aborted_group_still_bounds_later_ones() {
		let producer = track_producer("test", Info::timed());
		start_at_millis(&producer, 0, 1000)
			.unwrap()
			.abort(Error::Cancel)
			.unwrap();
		let replacement = start_at_millis(&producer, 0, 500).unwrap();
		assert_eq!(replacement.handle().bound(), None, "a late replacement bounds nothing");
		assert!(matches!(
			start_at_millis(&producer, 1, 600),
			Err(Error::TimestampMismatch)
		));
		start_at_millis(&producer, 1, 1000).unwrap().finish().unwrap();
	}

	/// A group received from past its first frame stamps a later frame, which may overlap
	/// the next group's start. It has no known start, so it is neither checked nor a bound.
	#[test]
	fn a_resumed_partial_group_is_neither_checked_nor_a_bound() {
		let producer = track_producer("test", Info::timed());
		start_at_millis(&producer, 0, 1000).unwrap().finish().unwrap();
		let mut partial = producer.create_group(group::Info { sequence: 1 }).unwrap();
		partial.start_at(3).unwrap();
		partial
			.write_frame(Timestamp::from_millis(2100).unwrap(), &b"x"[..])
			.unwrap();
		start_at_millis(&producer, 2, 2000).unwrap().finish().unwrap();
	}

	/// An untimed track has no timeline, so its groups land in any order.
	#[test]
	fn an_untimed_track_takes_groups_in_any_order() {
		let producer = untimed_producer();
		for sequence in [5, 3, 4] {
			let mut group = producer.create_group(group::Info { sequence }).unwrap();
			group.write_frame(None, &b"x"[..]).unwrap();
		}
	}

	/// A subscriber reads only its own ranges, whatever another subscriber's widen the
	/// aggregate to, in arrival order and in sequence order alike.
	#[test]
	#[cfg(feature = "async")]
	fn a_subscriber_reads_only_its_own_ranges() {
		let mut producer = track_producer("test", Info::timed());
		let mut everything = producer.subscribe(replay());
		let ranges = [Range::groups(1..3), Range::groups(5..)];
		let mut mine = producer.subscribe(replay().with_ranges(ranges));
		let mut ordered = producer.subscribe(replay().with_ranges(ranges)).ordered();
		for second in 0..7 {
			append_at(&mut producer, second * 1000);
		}
		assert_eq!(drain(&mut everything), vec![0, 1, 2, 3, 4, 5, 6]);
		assert_eq!(drain(&mut mine), vec![1, 2, 5, 6]);
		assert_eq!(drain_ordered(&mut ordered), vec![1, 2, 5, 6]);
	}

	/// Replacing the ranges re-offers what the old ones held back, and a relative anchor
	/// counts back from the latest group as of the update, not the subscribe.
	#[test]
	#[cfg(feature = "async")]
	fn replaced_ranges_resolve_again() {
		let mut producer = track_producer("test", Info::timed());
		let mut subscriber = producer.subscribe(replay().with_range(0..2));
		for second in 0..4 {
			append_at(&mut producer, second * 1000);
		}
		assert_eq!(drain(&mut subscriber), vec![0, 1]);

		subscriber.update(replay().with_range(0..)).unwrap();
		assert_eq!(
			drain(&mut subscriber),
			vec![3, 2],
			"held back, now in range, newest first"
		);

		// The latest group is 3 now, so one back is 2, already read: only new groups follow.
		let mut late = producer.subscribe(replay().with_range(..1));
		assert_eq!(drain(&mut late), vec![0]);
		late.update(replay().with_ranges([Range {
			start: Anchor::Relative(1),
			end: None,
		}]))
		.unwrap();
		assert_eq!(drain(&mut late), vec![3, 2]);
		append_at(&mut producer, 4000);
		assert_eq!(drain(&mut late), vec![4]);
	}

	/// A narrow reader under an open aggregate passes the groups outside its range holding
	/// nothing for them, however many arrive, and widening its ranges still serves the
	/// ones the cache holds in the new span, newest first and once each.
	#[test]
	#[cfg(feature = "async")]
	fn a_narrow_reader_holds_nothing_for_excluded_groups() {
		let producer = track_producer("test", Info::timed());
		let _wide = producer.subscribe(replay());
		let mut narrow = producer.subscribe(replay().with_range(0..2));
		for sequence in 0..1000 {
			producer
				.create_group(group::Info { sequence })
				.unwrap()
				.finish()
				.unwrap();
		}
		assert_eq!(drain(&mut narrow), vec![0, 1]);
		let Inner::Plain(cursor) = &narrow.inner else {
			unreachable!()
		};
		assert!(cursor.filter.owed.is_empty(), "nothing owed for excluded groups");
		assert_eq!(cursor.filter.admitted.iter().count(), 1, "only the range is kept");

		narrow
			.update(replay().with_ranges([Range::groups(0..2), Range::groups(5..8)]))
			.unwrap();
		assert_eq!(drain(&mut narrow), vec![7, 6, 5]);
		narrow.set_range(..9);
		narrow.update(replay().with_range(0..)).unwrap();
		assert_eq!(drain(&mut narrow), vec![8, 4, 3, 2]);
		narrow.set_range(..);
		assert_eq!(drain(&mut narrow), (9..1000).rev().collect::<Vec<_>>());
	}

	/// Lowering the floor re-offers the cached groups the arrival walk passed below it,
	/// as widening any range does.
	#[test]
	#[cfg(feature = "async")]
	fn a_lowered_floor_reoffers_what_arrived_below_it() {
		let mut producer = track_producer("test", Info::timed());
		for second in 0..6 {
			append_at(&mut producer, second * 1000);
		}
		let mut subscriber = producer.subscribe(replay().with_range(5..));
		assert_eq!(drain(&mut subscriber), vec![5]);

		subscriber.update(replay().with_range(2..)).unwrap();
		assert_eq!(
			drain(&mut subscriber),
			vec![4, 3, 2],
			"passed below the floor, now in range"
		);
		subscriber.update(replay().with_range(0..)).unwrap();
		assert_eq!(drain(&mut subscriber), vec![1, 0]);

		// Raising and lowering it again re-offers only what is cached in between, and
		// nothing already served goes out twice.
		subscriber.update(replay().with_range(4..)).unwrap();
		assert_eq!(drain(&mut subscriber), Vec::<u64>::new());
		subscriber.update(replay().with_range(0..)).unwrap();
		assert_eq!(drain(&mut subscriber), Vec::<u64>::new());
		append_at(&mut producer, 6000);
		assert_eq!(drain(&mut subscriber), vec![6]);
	}

	/// Lowering a reader's local floor offers the cached groups below the old one that it
	/// passed over, in its order, and never a group twice.
	#[test]
	#[cfg(feature = "async")]
	fn a_lowered_range_reoffers_the_unread_cached_groups() {
		let mut producer = track_producer("test", Info::timed());
		for second in 0..6 {
			append_at(&mut producer, second * 1000);
		}
		let mut subscriber = producer.subscribe(replay()).with_range(4..);
		assert_eq!(drain(&mut subscriber), vec![4, 5]);

		let mut subscriber = subscriber.with_range(1..);
		assert_eq!(drain(&mut subscriber), vec![3, 2, 1], "passed below the floor");
		let mut subscriber = subscriber.with_range(..=1);
		assert_eq!(drain(&mut subscriber), vec![0], "the floor and the cap both assign");
		let mut subscriber = subscriber.with_range(..);
		assert_eq!(drain(&mut subscriber), Vec::<u64>::new(), "nothing goes out twice");
		append_at(&mut producer, 6000);
		assert_eq!(drain(&mut subscriber), vec![6]);
	}

	/// Datagrams outside a subscriber's ranges are dropped like the groups.
	#[test]
	#[cfg(feature = "async")]
	fn datagrams_outside_the_ranges_are_skipped() {
		let mut producer = track_producer("test", Info::timed());
		let mut subscriber = producer.subscribe(replay().with_ranges([Range::groups(1..2), Range::groups(3..)]));
		for millis in 0..5 {
			producer
				.append_datagram(Timestamp::from_millis(millis).unwrap(), &b"x"[..])
				.unwrap();
		}
		let mut sequences = Vec::new();
		while let Some(Ok(Some(datagram))) = subscriber.recv_datagram().now_or_never() {
			sequences.push(datagram.sequence);
		}
		assert_eq!(sequences, vec![1, 3, 4]);
	}

	/// Only the sequences a subscriber asked for are named dropped to it.
	#[test]
	#[cfg(feature = "async")]
	fn drops_are_named_only_within_the_ranges() {
		let producer = track_producer("test", Info::timed());
		let mut subscriber = producer.subscribe(replay().with_ranges([Range::groups(2..4), Range::groups(6..8)]));
		producer.drop_groups(0..10, StreamError::NotFound);
		let waiter = kio::Waiter::noop();
		let mut dropped = Vec::new();
		while let Poll::Ready(range) = subscriber.poll_drop(&waiter) {
			dropped.push(range);
		}
		assert_eq!(
			dropped,
			vec![(2..4, StreamError::NotFound), (6..8, StreamError::NotFound)]
		);
	}

	/// A report describes a miss, not a delivery: a late group is still handed out once.
	#[test]
	#[cfg(feature = "async")]
	fn a_reported_drop_still_allows_a_late_group() {
		let producer = track_producer("test", Info::timed());
		let mut subscriber = producer.subscribe(replay());
		let waiter = kio::Waiter::noop();
		producer.drop_groups(2..4, StreamError::NotFound);
		assert_eq!(
			subscriber.poll_drop(&waiter),
			Poll::Ready((2..4, StreamError::NotFound))
		);
		assert!(subscriber.poll_drop(&waiter).is_pending());
		write_group(&producer, 3);
		assert_eq!(drain_groups(&mut subscriber), [3]);
		assert!(subscriber.poll_drop(&waiter).is_pending());
		assert!(drain_groups(&mut subscriber).is_empty());
	}

	/// A drop ends a group still arriving with the drop's error at once, for the reader
	/// already holding it, before any stream reset or track end. A group that finished here
	/// stays whole.
	#[test]
	#[cfg(feature = "async")]
	fn a_drop_ends_a_held_group_with_its_error() {
		let producer = track_producer("test", Info::timed());
		let mut subscriber = producer.subscribe(replay());
		let mut open = producer.create_group(group::Info { sequence: 0 }).unwrap();
		open.write_frame(Timestamp::ZERO, &b"a"[..]).unwrap();
		let mut finished = producer.create_group(group::Info { sequence: 1 }).unwrap();
		finished.write_frame(Timestamp::ZERO, &b"b"[..]).unwrap();
		finished.finish().unwrap();

		let mut held = subscriber.assert_group();
		assert_eq!(
			&held.read_frame().now_or_never().unwrap().unwrap().unwrap().payload[..],
			b"a"
		);
		assert!(
			held.read_frame().now_or_never().is_none(),
			"the group is still arriving"
		);

		producer.drop_groups(0..2, StreamError::App(5));
		assert!(matches!(
			held.read_frame().now_or_never(),
			Some(Err(Error::Stream(StreamError::App(5))))
		));
		let mut whole = subscriber.assert_group();
		assert_eq!(whole.sequence, 1);
		assert_eq!(
			&whole.read_frame().now_or_never().unwrap().unwrap().unwrap().payload[..],
			b"b"
		);
		assert!(matches!(whole.read_frame().now_or_never(), Some(Ok(None))));
	}

	/// A drop judges the subscription, not a fetch: a fetched head still arriving keeps
	/// serving the group to its end after the drop ends the live copy.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn a_drop_spares_a_fetched_head_still_arriving() {
		let producer = track_producer("test", Info::timed());
		let dynamic = producer.requests();
		let consumer = producer.consume();

		let mut live = producer.create_group(group::Info { sequence: 0 }).unwrap();
		live.start_at(1).unwrap();
		live.write_frame(Timestamp::ZERO, &b"delta"[..]).unwrap();
		let _fetch = consumer.fetch_group(0, None);
		let fill = dynamic.requested_fill().await.unwrap();
		let mut head = fill.accept(0, Info::timed()).unwrap();
		head.write_frame(Timestamp::ZERO, &b"snapshot"[..]).unwrap();

		producer.drop_groups(0..1, StreamError::App(5));
		head.write_frame(Timestamp::ZERO, &b"delta"[..]).unwrap();
		head.finish().unwrap();

		let mut group = consumer.peek_group(0).expect("the head still serves group 0");
		for payload in [&b"snapshot"[..], b"delta"] {
			assert_eq!(&group.read_frame().await.unwrap().unwrap().payload[..], payload);
		}
		assert!(matches!(group.read_frame().await, Ok(None)));
	}

	/// A group the publisher cuts before a reader takes it is still handed out, so the
	/// reader sees why it ended rather than a group never sent: in arrival and in sequence
	/// order, and on a track that then finishes, before the clean end.
	#[test]
	#[cfg(feature = "async")]
	fn a_cut_group_reaches_its_readers() {
		let producer = track_producer("test", Info::timed());
		let mut arrival = producer.subscribe(replay());
		let mut ordered = producer.subscribe(replay()).ordered();
		let mut cut = producer.create_group(group::Info { sequence: 0 }).unwrap();
		cut.write_frame(Timestamp::ZERO, &b"a"[..]).unwrap();
		cut.abort(Error::App(5)).unwrap();
		producer
			.create_group(group::Info { sequence: 1 })
			.unwrap()
			.finish()
			.unwrap();
		producer.finish().unwrap();

		let mut first = arrival.assert_group();
		assert_eq!(first.sequence, 0);
		assert!(matches!(first.read_frame().now_or_never(), Some(Err(Error::App(5)))));
		assert_eq!(arrival.assert_group().sequence, 1);
		assert!(matches!(arrival.recv_group().now_or_never(), Some(Ok(None))));

		let mut first = ordered.next_group().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(first.sequence, 0);
		assert!(matches!(first.read_frame().now_or_never(), Some(Err(Error::App(5)))));
		assert_eq!(drain_ordered(&mut ordered), [1]);
		assert!(matches!(ordered.next_group().now_or_never(), Some(Ok(None))));
	}

	/// Cache policy and a cancel are deliberate holes, not cuts: an expired, evicted, or
	/// cancelled group, and a reset carrying such a code, stay out of the readers' way.
	#[test]
	#[cfg(feature = "async")]
	fn a_policy_abort_is_a_hole() {
		for err in [
			Error::Old,
			Error::Evicted,
			Error::Timeout,
			Error::Cancel,
			Error::Stream(StreamError::Old),
			Error::Stream(StreamError::DeliveryTimeout),
			Error::Stream(StreamError::Cancel),
		] {
			let producer = track_producer("test", Info::timed());
			let mut subscriber = producer.subscribe(replay());
			let mut group = producer.create_group(group::Info { sequence: 0 }).unwrap();
			group.write_frame(Timestamp::ZERO, &b"a"[..]).unwrap();
			group.abort(err.clone()).unwrap();
			producer
				.create_group(group::Info { sequence: 1 })
				.unwrap()
				.finish()
				.unwrap();
			assert_eq!(subscriber.assert_group().sequence, 1, "{err:?}");
		}
	}

	/// A cut stays readable for as long as the group it was: the sweep that reclaims
	/// spent slots leaves it to a reader still behind, and once it expires like a group,
	/// a relay still forwards it as a drop.
	#[test]
	#[cfg(feature = "async")]
	fn a_cut_outlives_the_sweep_until_it_expires() {
		let pool = cache::Pool::new(cache::Config::default().with_expiry(Duration::from_secs(10)));
		let producer = track_producer_pooled("test", pool.clone());
		let mut cut = producer.create_group(group::Info { sequence: 0 }).unwrap();
		cut.write_frame(Timestamp::ZERO, &b"a"[..]).unwrap();
		cut.abort(Error::App(5)).unwrap();
		producer
			.create_group(group::Info { sequence: 1 })
			.unwrap()
			.finish()
			.unwrap();
		let _live = producer.create_group(group::Info { sequence: 2 }).unwrap();

		elapse(&producer, Duration::from_secs(1));
		pool.sweep();
		let mut late = producer.subscribe(replay());
		let mut first = late.assert_group();
		assert_eq!(first.sequence, 0, "the sweep kept the cut");
		assert!(matches!(first.read_frame().now_or_never(), Some(Err(Error::App(5)))));

		for _ in 0..2 {
			elapse(&producer, Duration::from_secs(20));
			pool.sweep();
		}
		assert!(!producer.state.read().lookup.contains_key(&0), "the cut expired");
		let mut relay = producer.subscribe(replay());
		let waiter = kio::Waiter::noop();
		assert!(matches!(
			relay.poll_drop(&waiter),
			Poll::Ready((range, StreamError::App(5))) if range == (0..1)
		));
	}

	/// A cut above a reader's cap is handed out once the cap rises over it.
	#[test]
	#[cfg(feature = "async")]
	fn a_raised_cap_reoffers_a_cut() {
		let producer = track_producer("test", Info::timed());
		let mut arrival = producer.subscribe(replay()).with_range(..1);
		let mut ordered = producer.subscribe(replay()).ordered().with_range(..1);
		producer
			.create_group(group::Info { sequence: 0 })
			.unwrap()
			.finish()
			.unwrap();
		let mut cut = producer.create_group(group::Info { sequence: 1 }).unwrap();
		cut.write_frame(Timestamp::ZERO, &b"a"[..]).unwrap();
		cut.abort(Error::App(5)).unwrap();

		assert_eq!(arrival.assert_group().sequence, 0);
		assert!(arrival.recv_group().now_or_never().is_none(), "capped");
		arrival = arrival.with_range(..);
		let mut reoffered = arrival.assert_group();
		assert_eq!(reoffered.sequence, 1);
		assert!(matches!(
			reoffered.read_frame().now_or_never(),
			Some(Err(Error::App(5)))
		));

		assert_eq!(drain_ordered(&mut ordered), [0]);
		ordered = ordered.with_range(..);
		let mut reoffered = ordered.next_group().now_or_never().unwrap().unwrap().unwrap();
		assert_eq!(reoffered.sequence, 1);
		assert!(matches!(
			reoffered.read_frame().now_or_never(),
			Some(Err(Error::App(5)))
		));
	}

	/// A time start is the cached group containing it and a time end stops just past that
	/// group; one past every start held stays open until a group starts after it.
	#[test]
	#[cfg(feature = "async")]
	fn time_anchors_resolve_against_the_cache() {
		let mut producer = track_producer("test", Info::timed());
		for second in 0..4 {
			append_at(&mut producer, second * 1000);
		}
		let time = |millis| Anchor::Time(Timestamp::from_millis(millis).unwrap());

		let mut seek = producer.subscribe(replay().with_ranges([Range {
			start: time(1500),
			end: Some(time(2000)),
		}]));
		assert_eq!(drain(&mut seek), vec![1, 2]);

		// A start before every group reaches back to the oldest one.
		let mut early = producer.subscribe(replay().with_ranges([Range {
			start: Anchor::Time(Timestamp::from_micros(0).unwrap()),
			end: Some(time(500)),
		}]));
		assert_eq!(drain(&mut early), vec![0]);

		// Past the newest start: open until a group is known to start after the end.
		let mut live = producer.subscribe(replay().with_ranges([Range {
			start: time(3000),
			end: Some(time(4500)),
		}]));
		assert_eq!(drain(&mut live), vec![3]);
		append_at(&mut producer, 4000);
		append_at(&mut producer, 5000);
		append_at(&mut producer, 6000);
		assert_eq!(drain(&mut live), vec![4]);
	}

	/// A group joined mid-group stamps a later frame than its start, so it never proves a
	/// time end falls before it.
	#[test]
	#[cfg(feature = "async")]
	fn a_partial_group_proves_no_start() {
		let mut producer = track_producer("test", Info::timed());
		append_at(&mut producer, 0);
		append_at(&mut producer, 1000);
		let mut partial = producer.append_group().unwrap();
		partial.start_at(1).unwrap();
		partial
			.write_frame(Timestamp::from_millis(2500).unwrap(), bytes::Bytes::from_static(b"x"))
			.unwrap();
		partial.finish().unwrap();

		let time = |millis| Anchor::Time(Timestamp::from_millis(millis).unwrap());
		let mut seek = producer.subscribe(replay().with_ranges([Range {
			start: time(1500),
			end: Some(time(2200)),
		}]));
		assert_eq!(drain(&mut seek), vec![1, 2]);
	}

	/// A time end stays open across a group that has yet to start: it may start before the
	/// end, so a later group starting after it settles nothing.
	#[test]
	#[cfg(feature = "async")]
	fn a_time_end_waits_on_an_unstarted_group_below_it() {
		let producer = track_producer("test", Info::timed());
		let at = |millis| Timestamp::from_millis(millis).unwrap();
		let mut zero = producer.create_group(group::Info { sequence: 0 }).unwrap();
		zero.write_frame(at(0), bytes::Bytes::from_static(b"x")).unwrap();
		let mut hole = producer.create_group(group::Info { sequence: 1 }).unwrap();
		let mut two = producer.create_group(group::Info { sequence: 2 }).unwrap();
		two.write_frame(at(2000), bytes::Bytes::from_static(b"x")).unwrap();

		let mut subscriber = producer.subscribe(replay().with_ranges([Range {
			start: Anchor::Absolute(0),
			end: Some(Anchor::Time(at(500))),
		}]));
		hole.write_frame(at(1000), bytes::Bytes::from_static(b"x")).unwrap();
		assert_eq!(drain(&mut subscriber), vec![0]);
	}

	/// A time end stays open across a sequence yet to arrive: it may start before the end,
	/// so a later group starting after it settles nothing. Once that sequence is reported
	/// dropped, nothing below the later group can start before the end, so it settles.
	#[test]
	#[cfg(feature = "async")]
	fn a_time_end_waits_on_an_absent_group_below_it() {
		let at = |millis| Timestamp::from_millis(millis).unwrap();
		let ranges = [Range {
			start: Anchor::Absolute(0),
			end: Some(Anchor::Time(at(500))),
		}];
		for dropped in [false, true] {
			let producer = track_producer("test", Info::timed());
			let mut zero = producer.create_group(group::Info { sequence: 0 }).unwrap();
			zero.write_frame(at(0), bytes::Bytes::from_static(b"x")).unwrap();
			let mut two = producer.create_group(group::Info { sequence: 2 }).unwrap();
			two.write_frame(at(2000), bytes::Bytes::from_static(b"x")).unwrap();

			let mut subscriber = producer.subscribe(replay().with_ranges(ranges));
			assert_eq!(
				subscriber.resolved_ranges(),
				vec![Some(Resolved { start: 0, end: None })],
				"group 1 may still start before the end"
			);
			if dropped {
				producer.drop_groups(1..2, StreamError::NotFound);
				assert_eq!(drain(&mut subscriber), vec![0]);
				assert_eq!(
					subscriber.resolved_ranges(),
					vec![Some(Resolved { start: 0, end: Some(2) })],
					"the drop settles the end"
				);
				continue;
			}
			let mut one = producer.create_group(group::Info { sequence: 1 }).unwrap();
			one.write_frame(at(1000), bytes::Bytes::from_static(b"x")).unwrap();
			assert_eq!(drain(&mut subscriber), vec![0]);
		}
	}

	/// A time end at the live edge reads at most one group past it: groups opened before
	/// their first frame wait on the one below them starting, rather than every one being
	/// handed out while the end is unsettled.
	#[test]
	#[cfg(feature = "async")]
	fn a_time_end_reads_one_group_past_it_at_the_live_edge() {
		let at = |millis| Timestamp::from_millis(millis).unwrap();
		let ranges = [Range {
			start: Anchor::Time(at(3000)),
			end: Some(Anchor::Time(at(3500))),
		}];
		for ordered in [false, true] {
			let mut producer = track_producer("test", Info::timed());
			append_at(&mut producer, 3000);
			let mut groups: Vec<_> = (1..4)
				.map(|sequence| producer.create_group(group::Info { sequence }).unwrap())
				.collect();
			let subscriber = producer.subscribe(replay().with_ranges(ranges));
			let mut reader = Reader::new(subscriber, ordered);
			assert_eq!(reader.drain(), vec![0, 1], "1 is read on speculation, 2 and 3 wait");
			groups[0]
				.write_frame(at(3600), bytes::Bytes::from_static(b"x"))
				.unwrap();
			groups[1]
				.write_frame(at(4000), bytes::Bytes::from_static(b"x"))
				.unwrap();
			assert_eq!(reader.drain(), Vec::<u64>::new(), "ordered: {ordered}");
			assert_eq!(
				reader.resolved(),
				vec![Some(Resolved { start: 0, end: Some(1) })],
				"1 starting past the end settles it"
			);
		}
	}

	/// A read parked on a time end's cap wakes when the speculative group starts, and when
	/// a later group starts inside the end while the speculative one stalls.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn a_read_parked_on_a_time_end_wakes_as_groups_start() {
		let at = |millis| Timestamp::from_millis(millis).unwrap();
		let producer = track_producer("test", Info::timed());
		let mut groups: Vec<_> = (0..4)
			.map(|sequence| producer.create_group(group::Info { sequence }).unwrap())
			.collect();
		let mut subscriber = producer.subscribe(replay().with_order(Order::Ascending).with_ranges([Range {
			start: Anchor::Absolute(0),
			end: Some(Anchor::Time(at(3500))),
		}]));
		assert_eq!(drain(&mut subscriber), vec![0]);

		let parked = moq_clock::sim::spawn(async move {
			let one = subscriber.recv_group().await.unwrap().unwrap().sequence;
			let two = subscriber.recv_group().await.unwrap().unwrap().sequence;
			(one, two, subscriber)
		});
		moq_clock::sim::yield_now().await;
		assert!(!parked.is_finished(), "1 waits on 0 starting");
		groups[0]
			.write_frame(at(3000), bytes::Bytes::from_static(b"x"))
			.unwrap();
		moq_clock::sim::yield_now().await;
		assert!(!parked.is_finished(), "1 is read, and 2 waits on 1 starting");

		// 1 stalls, so only 2 starting inside the end lifts the cap past it.
		groups[2]
			.write_frame(at(3200), bytes::Bytes::from_static(b"x"))
			.unwrap();
		let (one, two, mut subscriber) = parked.await.unwrap();
		assert_eq!((one, two), (1, 2));
		assert_eq!(drain(&mut subscriber), vec![3], "3 is the speculative one now");
		groups[3]
			.write_frame(at(3600), bytes::Bytes::from_static(b"x"))
			.unwrap();
		groups[1]
			.write_frame(at(3100), bytes::Bytes::from_static(b"x"))
			.unwrap();
		assert_eq!(
			subscriber.resolved_ranges(),
			vec![Some(Resolved { start: 0, end: Some(3) })]
		);
	}

	/// A datagram is a whole group of one frame, so its own timestamp says whether it falls
	/// inside a time end, without one ever settling from the groups.
	#[test]
	#[cfg(feature = "async")]
	fn a_datagram_is_judged_against_a_time_end_by_its_timestamp() {
		let at = |millis| Timestamp::from_millis(millis).unwrap();
		let mut producer = track_producer("test", Info::timed());
		let mut subscriber = producer.subscribe(replay().with_ranges([Range {
			start: Anchor::Absolute(0),
			end: Some(Anchor::Time(at(3500))),
		}]));
		for millis in [3000, 3400, 3600, 3700] {
			producer.append_datagram(at(millis), &b"x"[..]).unwrap();
		}
		let mut read = Vec::new();
		while let Some(Ok(Some(datagram))) = subscriber.recv_datagram().now_or_never() {
			read.push(datagram.sequence);
		}
		assert_eq!(read, vec![0, 1]);
	}

	/// A track of only datagrams starts no group, so a time start is judged against each
	/// datagram's own timestamp rather than letting every one through.
	#[test]
	#[cfg(feature = "async")]
	fn a_datagram_is_judged_against_a_time_start_by_its_timestamp() {
		let at = |millis| Timestamp::from_millis(millis).unwrap();
		let mut producer = track_producer("test", Info::timed());
		let mut subscriber = producer.subscribe(replay().with_ranges([Range {
			start: Anchor::Time(at(3500)),
			end: None,
		}]));
		for millis in [3000, 3400, 3500, 3700] {
			producer.append_datagram(at(millis), &b"x"[..]).unwrap();
		}
		let mut read = Vec::new();
		while let Some(Ok(Some(datagram))) = subscriber.recv_datagram().now_or_never() {
			read.push(datagram.sequence);
		}
		assert_eq!(read, vec![2, 3]);
	}

	/// An untimed track refuses a time anchor wherever the ranges reach it.
	#[test]
	#[cfg(feature = "async")]
	fn an_untimed_track_refuses_time_anchors() {
		let mut producer = untimed_producer();
		let timed = replay().with_ranges([Range {
			start: Anchor::Time(Timestamp::from_millis(0).unwrap()),
			end: None,
		}]);

		let pending = producer.consume().subscribe(timed.clone());
		assert!(matches!(
			pending.poll_ok(&kio::Waiter::noop()),
			Poll::Ready(Err(Error::TimestampMismatch))
		));

		let mut subscriber = producer.subscribe(replay());
		assert!(matches!(
			subscriber.update(timed.clone()),
			Err(Error::TimestampMismatch)
		));
		assert!(matches!(
			subscriber.control().update(timed.clone()),
			Err(Error::TimestampMismatch)
		));
		assert_eq!(subscriber.subscription(), replay(), "a refused update changes nothing");

		let mut refused = producer.subscribe(timed);
		assert!(matches!(
			refused.recv_group().now_or_never(),
			Some(Err(Error::TimestampMismatch))
		));
		producer.append_datagram(None, &b"x"[..]).unwrap();
		assert!(matches!(
			refused.recv_datagram().now_or_never(),
			Some(Err(Error::TimestampMismatch))
		));

		// Ranges it can serve lift the refusal.
		refused.update(replay()).unwrap();
		let mut group = producer.append_group().unwrap();
		group.finish().unwrap();
		let read = refused.recv_group().now_or_never().expect("the group is cached");
		assert_eq!(
			read.unwrap().map(|group| group.sequence),
			Some(1),
			"the datagram took sequence 0"
		);
	}

	/// A widened budget re-offers the cached groups the cursor skipped as stale, newest
	/// first by default, and never a group it already handed out.
	#[test]
	#[cfg(feature = "async")]
	fn a_widened_budget_reoffers_what_it_skipped_as_stale() {
		let mut producer = track_producer("test", Info::timed());
		let narrow = Subscription::default().with_max_delay(Duration::from_millis(1500));
		let mut subscriber = producer.subscribe(narrow.clone());
		append_at(&mut producer, 0);
		assert_eq!(drain(&mut subscriber), vec![0]);
		for millis in [1000, 2000, 3000, 6000] {
			append_at(&mut producer, millis);
		}
		// Groups 1 and 2 reach 2s and 3s, at least 1.5s behind the 6s edge.
		assert_eq!(drain(&mut subscriber), vec![3, 4]);

		subscriber
			.update(narrow.with_max_delay(Duration::from_secs(10)))
			.unwrap();
		assert_eq!(
			drain(&mut subscriber),
			vec![2, 1],
			"newest first, never 0, 3, or 4 again"
		);
		assert_eq!(drain(&mut subscriber), Vec::<u64>::new());
	}

	/// An ascending subscription gets the re-offer lowest first.
	#[test]
	#[cfg(feature = "async")]
	fn an_ascending_widening_reoffers_lowest_first() {
		let mut producer = track_producer("test", Info::timed());
		for millis in [0, 1000, 2000, 5000] {
			append_at(&mut producer, millis);
		}
		let narrow = Subscription::default()
			.with_max_delay(Duration::from_millis(1500))
			.with_order(Order::Ascending);
		let mut subscriber = producer.subscribe(narrow.clone());
		assert_eq!(drain(&mut subscriber), vec![2, 3]);
		subscriber
			.update(narrow.with_max_delay(Duration::from_secs(10)))
			.unwrap();
		assert_eq!(drain(&mut subscriber), vec![0, 1]);
	}

	/// A widening that covers only the newer of two skipped groups hands it out without
	/// folding the older one into the delivered floor: a further widening still owes it.
	#[test]
	#[cfg(feature = "async")]
	fn a_partial_widening_keeps_the_older_skip_owed() {
		let mut producer = track_producer("test", Info::timed());
		let narrow = Subscription::default().with_max_delay(Duration::from_millis(1500));
		let mut subscriber = producer.subscribe(narrow.clone());
		append_at(&mut producer, 0);
		assert_eq!(drain(&mut subscriber), vec![0]);
		for millis in [1000, 2000, 4000] {
			append_at(&mut producer, millis);
		}
		assert_eq!(drain(&mut subscriber), vec![2, 3]);
		for millis in [5000, 6000, 9000] {
			append_at(&mut producer, millis);
		}
		assert_eq!(drain(&mut subscriber), vec![5, 6]);

		// Group 4 reaches 6s, 3s behind the 9s edge; group 1 reaches 2s, 7s behind.
		subscriber
			.update(narrow.clone().with_max_delay(Duration::from_secs(4)))
			.unwrap();
		assert_eq!(drain(&mut subscriber), vec![4]);
		subscriber
			.update(narrow.with_max_delay(Duration::from_secs(10)))
			.unwrap();
		assert_eq!(drain(&mut subscriber), vec![1]);
	}

	/// Handing out the newest group a widened range owes never folds the older ones it still
	/// owes into the delivered floor before they are judged: a later budget widening gets the
	/// ones judged stale.
	#[test]
	#[cfg(feature = "async")]
	fn a_newest_first_range_reoffer_keeps_the_older_owed_groups() {
		let mut producer = track_producer("test", Info::timed());
		for millis in [0, 1000, 2000, 3000, 6000] {
			append_at(&mut producer, millis);
		}
		let narrow = Subscription::default().with_max_delay(Duration::from_millis(1500));
		let mut subscriber = producer.subscribe(narrow.clone());
		subscriber.set_range(3..5);
		assert_eq!(drain(&mut subscriber), vec![3, 4]);
		for millis in [20_000, 40_000] {
			append_at(&mut producer, millis);
		}

		subscriber.set_range(0..);
		// Groups 0 through 2 reach 3s at most, far behind the 40s edge.
		assert_eq!(drain(&mut subscriber), vec![6, 5]);
		subscriber
			.update(narrow.with_max_delay(Duration::from_secs(60)))
			.unwrap();
		assert_eq!(drain(&mut subscriber), vec![2, 1, 0]);
	}

	/// How many runs of skipped groups `subscriber` holds.
	#[cfg(feature = "async")]
	fn skipped_runs(subscriber: &Subscriber) -> usize {
		let Inner::Plain(cursor) = &subscriber.inner else {
			unreachable!()
		};
		cursor.skipped.runs()
	}

	/// The groups handed out around a skipped one aging past the budget does not fold it in
	/// with them: a widening still owes it.
	#[test]
	#[cfg(feature = "async")]
	fn a_skipped_group_outlives_the_fold_of_its_neighbours() {
		let mut producer = track_producer("test", Info::timed());
		let narrow = Subscription::default().with_max_delay(Duration::from_millis(1500));
		let mut subscriber = producer.subscribe(narrow.clone());
		append_at(&mut producer, 0);
		assert_eq!(drain(&mut subscriber), vec![0]);
		for millis in [1000, 2000, 3000, 6000] {
			append_at(&mut producer, millis);
		}
		assert_eq!(drain(&mut subscriber), vec![3, 4]);
		// Groups 3 and 4 are past the budget now, which would write off the gap below them.
		for millis in [20_000, 40_000] {
			append_at(&mut producer, millis);
			assert_eq!(drain(&mut subscriber).len(), 1);
		}
		assert_eq!(
			subscriber.delivered_runs(),
			2,
			"folded up to the skipped groups, no further"
		);

		subscriber
			.update(narrow.with_max_delay(Duration::from_secs(60)))
			.unwrap();
		assert_eq!(drain(&mut subscriber), vec![2, 1]);
	}

	/// A group skipped at the track's max age bound is final: no widening passes the bound, so
	/// the cursor keeps no record of it and its gap folds as the groups above it age out.
	#[test]
	#[cfg(feature = "async")]
	fn a_skip_at_the_max_age_bound_folds_into_the_floor() {
		let mut producer = track_producer("test", Info::timed().with_max_age(Duration::from_millis(500)));
		for millis in [0, 1000, 2000] {
			append_at(&mut producer, millis);
		}
		let mut subscriber = producer.subscribe(Subscription::default().with_max_delay(Duration::from_secs(10)));
		assert_eq!(drain(&mut subscriber), vec![1, 2]);
		assert_eq!(skipped_runs(&subscriber), 0);
		append_at(&mut producer, 3000);
		assert_eq!(drain(&mut subscriber), vec![3]);
		assert_eq!(subscriber.delivered_runs(), 1, "the gap at group 0 folded");
	}

	/// A skipped group gone from the cache can never be offered again, so the cursor forgets
	/// it and a widening finds nothing.
	#[test]
	#[cfg(feature = "async")]
	fn an_evicted_skip_is_forgotten() {
		let mut producer = track_producer("test", Info::timed());
		let narrow = Subscription::default().with_max_delay(Duration::from_millis(1500));
		let mut subscriber = producer.subscribe(narrow.clone());
		append_at(&mut producer, 0);
		assert_eq!(drain(&mut subscriber), vec![0]);
		for millis in [1000, 2000, 3000, 6000] {
			append_at(&mut producer, millis);
		}
		assert_eq!(drain(&mut subscriber), vec![3, 4]);
		assert_eq!(skipped_runs(&subscriber), 1);

		evict(&producer, 1);
		evict(&producer, 2);
		append_at(&mut producer, 7000);
		assert_eq!(drain(&mut subscriber), vec![5]);
		assert_eq!(skipped_runs(&subscriber), 0);
		subscriber
			.update(narrow.with_max_delay(Duration::from_secs(60)))
			.unwrap();
		assert_eq!(drain(&mut subscriber), Vec::<u64>::new());
	}
	/// Distinct transport starts retain delivery outcomes, but share the same indexed
	/// bytes even while every earlier delivery is held by an unread consumer.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn distinct_tail_readers_share_canonical_content() {
		for size in [0, 1, 1024] {
			for bounded in [false, true] {
				let producer = track_producer("test", Info::timed());
				let pool = producer.broadcast.pool.clone();
				let dynamic = producer.requests();
				let consumer = producer.consume();
				let mut live = producer.create_group(group::Info { sequence: 0 }).unwrap();
				live.start_at(32).unwrap();
				live.write_frame(Timestamp::ZERO, vec![32; size]).unwrap();
				assert!(
					live.handle().content().is_none(),
					"a single copy keeps its fast representation"
				);
				let mut held = Vec::new();
				let mut deliveries = vec![live.handle()];
				let end = if bounded { 17 } else { 33 };
				for start in (1..=8).rev() {
					let pending = consumer.fetch_range(
						0,
						FetchOptions {
							options: group::Fetch {
								frame_start: start,
								..Default::default()
							},
							end: bounded.then_some(end),
						},
					);
					let fill = dynamic.requested_fill_group().await.unwrap();
					let mut tail = fill.accept(Info::timed()).unwrap();
					for frame in start..end {
						tail.write_frame(Timestamp::ZERO, vec![frame as u8; size]).unwrap();
					}
					tail.finish().unwrap();
					deliveries.push(tail.handle());
					held.push(pending.await.unwrap());
				}
				let owner = deliveries[0].content().unwrap();
				for delivery in &deliveries {
					assert!(Arc::ptr_eq(&owner, &delivery.content().unwrap()));
				}
				let (rows, bytes, shared_charge) = owner.cost();
				assert_eq!(rows, if bounded { 17 } else { 32 });
				assert_eq!(bytes, rows * size);
				let total = pool.used();
				let delivery_charge: u64 = deliveries.iter().map(group::Handle::delivery_size).sum();
				eprintln!(
					"CANONICAL_TAIL_COST size={size} bounded={bounded} delivery_handles={} canonical_rows={rows} unique_payload={bytes} shared_charge={shared_charge} delivery_charge={delivery_charge} discovery_charge={} total={total}",
					deliveries.len(),
					total - shared_charge - delivery_charge
				);
				let mut pointers = BTreeMap::new();
				for (copy, reader) in held.iter_mut().enumerate() {
					for index in (8 - copy) as u64..end {
						assert_eq!(reader.index(), index);
						let frame = reader.read_frame().await.unwrap().unwrap();
						assert_eq!(frame.payload.as_ref(), vec![index as u8; size]);
						if size != 0
							&& let Some(prior) = pointers.insert(index, frame.payload.as_ptr() as usize)
						{
							assert_eq!(
								prior,
								frame.payload.as_ptr() as usize,
								"one physical payload for every matching frame"
							);
						}
					}
					if bounded {
						assert!(reader.read_frame().await.unwrap().is_none());
					}
				}
				drop((pointers, owner, deliveries));
				assert_eq!(
					pool.used(),
					total,
					"identity probes neither own nor conceal payload copies"
				);
				drop(held);
				live.abort(Error::Cancel).unwrap();
			}
		}
	}

	/// Evicting a Slot does not hide a canonical owner from still-held older tails;
	/// explicitly recreating the live group, however, starts a new incarnation.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn tail_owner_survives_slot_removal_but_not_live_recreation() {
		let producer = track_producer("test", Info::timed());
		let mut live = producer.create_group(group::Info { sequence: 0 }).unwrap();
		live.start_at(8).unwrap();
		live.write_frame(Timestamp::ZERO, b"eight".as_slice()).unwrap();
		let mut tails = Vec::new();
		let mut held = Vec::new();
		for start in [4, 2] {
			let mut tail = producer
				.state
				.write()
				.ok()
				.unwrap()
				.insert_group_request(
					0,
					group::Bounds {
						start,
						end: Some(start + 1),
					},
					None,
				)
				.unwrap();
			tail.write_frame(Timestamp::ZERO, vec![start as u8; 4]).unwrap();
			tail.finish().unwrap();
			held.push(tail.consume().with_range(start..));
			tails.push(tail);
		}
		let owner = tails[0].handle().content().unwrap();
		producer
			.state
			.write()
			.ok()
			.unwrap()
			.uncache(0)
			.unwrap()
			.abort(Error::Evicted);
		assert!(
			!tails[0].handle().is_aborted(),
			"the replaced delivery is held outside the Slot"
		);
		let mut refill = producer
			.state
			.write()
			.ok()
			.unwrap()
			.insert_group_request(0, group::Bounds { start: 0, end: Some(1) }, None)
			.unwrap();
		assert!(Arc::ptr_eq(&owner, &refill.handle().content().unwrap()));
		refill.write_frame(Timestamp::ZERO, b"zero".as_slice()).unwrap();
		refill.finish().unwrap();
		refill.abort(Error::Cancel).unwrap();
		let mut recreated = producer.create_group(group::Info { sequence: 0 }).unwrap();
		recreated.start_at(8).unwrap();
		recreated.write_frame(Timestamp::ZERO, b"new eight".as_slice()).unwrap();
		assert!(
			recreated.handle().content().is_none(),
			"an explicit new live incarnation starts independently"
		);
		let mut new = producer
			.state
			.write()
			.ok()
			.unwrap()
			.insert_group_request(0, group::Bounds { start: 0, end: Some(1) }, None)
			.unwrap();
		new.write_frame(Timestamp::ZERO, b"new zero".as_slice()).unwrap();
		assert!(!Arc::ptr_eq(&owner, &new.handle().content().unwrap()));
		assert_eq!(held[0].read_frame().await.unwrap().unwrap().payload.as_ref(), &[4; 4]);
	}

	/// Activation preserves an already-streaming reader and producer, and every
	/// delivery's progress, arrival and abort while sharing the origin's admission.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn canonical_activation_preserves_inflight_delivery_ownership() {
		for abort_first in [false, true] {
			let budget = frame::Budget::new(1024);
			let broadcast = Arc::new(broadcast::Info {
				frames: budget.clone(),
				..Default::default()
			});
			let producer = Producer::new(broadcast, "test", Info::timed());
			let mut first = producer.create_group(group::Info { sequence: 0 }).unwrap();
			first.start_at(4).unwrap();
			let mut first_read = first.consume().with_range(4..);
			let mut first = first
				.create_frame(frame::Info {
					size: 1024,
					timestamp: Some(Timestamp::ZERO),
				})
				.unwrap();
			first.write(vec![4; 512]).unwrap();
			let mut streaming = first_read.next_frame().await.unwrap().unwrap();
			let early = streaming.read_chunk().await.unwrap().unwrap();
			assert_eq!(early.as_ref(), &[4; 512]);
			let mut second = producer
				.state
				.write()
				.ok()
				.unwrap()
				.insert_group_request(0, group::Bounds { start: 3, end: Some(5) }, None)
				.unwrap();
			second.write_frame(Timestamp::ZERO, b"three".as_slice()).unwrap();
			let mut second_read = second.consume().with_range(4..);
			let mut second = second
				.create_frame(frame::Info {
					size: 1024,
					timestamp: Some(Timestamp::ZERO),
				})
				.unwrap();
			assert!(
				budget.reserve(1).is_none(),
				"overlap shares the first producer's reservation"
			);
			assert_eq!(first.remaining(), 512);
			second.write(vec![4; 256]).unwrap();
			assert_eq!(first.remaining(), 512, "another delivery credits no progress");
			assert!(streaming.read_chunk().now_or_never().is_none());
			moq_clock::sim::advance(Duration::from_millis(20)).await;
			if abort_first {
				first.abort(Error::Cancel).unwrap();
				assert!(budget.reserve(1).is_none(), "the survivor still holds admission");
				second.write(vec![4; 768]).unwrap();
				let mut second = second.finish().unwrap();
				second.finish().unwrap();
				assert!(matches!(streaming.read_chunk().await, Err(Error::Cancel)));
				let frame = second_read.read_frame().await.unwrap().unwrap();
				assert_eq!(frame.payload.as_ref(), &[4; 1024]);
				assert_eq!(
					early.as_ptr(),
					frame.payload.as_ptr(),
					"the survivor reuses the published storage"
				);
			} else {
				second.write(vec![4; 768]).unwrap();
				let mut second = second.finish().unwrap();
				second.finish().unwrap();
				let later_copy = second_read.read_frame().await.unwrap().unwrap();
				assert!(
					budget.reserve(1).is_none(),
					"the slower first delivery still owns admission"
				);
				assert_eq!(first.remaining(), 512);
				assert!(streaming.read_chunk().now_or_never().is_none());
				moq_clock::sim::advance(Duration::from_millis(10)).await;
				first.write(vec![4; 512]).unwrap();
				let mut first = first.finish().unwrap();
				first.finish().unwrap();
				let original_copy = first.consume().with_range(4..).read_frame().await.unwrap().unwrap();
				assert_eq!(
					original_copy.arrived().duration_since(later_copy.arrived()),
					Duration::from_millis(10)
				);
				assert_eq!(original_copy.payload.as_ptr(), later_copy.payload.as_ptr());
				assert_eq!(early.as_ptr(), later_copy.payload.as_ptr());
				assert_eq!(streaming.read_chunk().await.unwrap().unwrap().as_ref(), &[4; 512]);
			}
			assert!(
				budget.reserve(1024).is_some(),
				"all completed producers return admission"
			);
		}
	}
	/// IETF Object IDs can occupy different delivery ordinals. Sharing bytes must
	/// preserve their typed fields and each delivery's independent terminal record.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn canonical_content_keys_typed_objects_by_id_not_ordinal() {
		fn wire(first: u64) -> group::Wire {
			group::Wire {
				sparse: true,
				first_object: false,
				first_id: first,
				extensions: true,
				has_end: true,
				fetchable: true,
				last: None,
			}
		}
		fn write(group: group::Producer, id: u64, payload: &'static [u8]) -> group::Producer {
			let mut frame = group
				.create_object(frame::Header {
					info: frame::Info {
						size: payload.len() as u64,
						timestamp: Some(Timestamp::ZERO),
					},
					fields: Some(Arc::new(super::super::object::Fields {
						id,
						properties: super::super::object::Properties {
							sealed_time: Some(Timestamp::ZERO),
							..Default::default()
						},
					})),
					inflated: None,
				})
				.unwrap();
			frame.write(payload).unwrap();
			frame.finish().unwrap()
		}
		let producer = track_producer("test", Info::timed());
		let mut live = producer.create_group(group::Info { sequence: 0 }).unwrap();
		live.start_at(2).unwrap();
		live.set_wire(wire(100)).unwrap();
		let live = write(live, 100, b"a");
		let head = producer
			.state
			.write()
			.ok()
			.unwrap()
			.insert_group_request(0, group::Bounds { start: 1, end: Some(3) }, None)
			.unwrap();
		head.set_wire(wire(90)).unwrap();
		let head = write(head, 90, b"b");
		let mut head = write(head, 100, b"a");
		head.set_end_object(super::super::object::Fields {
			id: 101,
			properties: Default::default(),
		})
		.unwrap();
		head.finish().unwrap();
		let earlier = producer
			.state
			.write()
			.ok()
			.unwrap()
			.insert_group_request(0, group::Bounds { start: 0, end: Some(2) }, None)
			.unwrap();
		earlier.set_wire(wire(90)).unwrap();
		let earlier = write(earlier, 90, b"b");
		let mut earlier = write(earlier, 95, b"c");
		earlier.finish().unwrap();
		assert!(head.consume().end_object().is_some_and(|fields| fields.id == 101));
		assert!(
			earlier.consume().end_object().is_none(),
			"content sharing copies no terminal metadata"
		);
		let mut prior = head.consume().with_range(1..).compressed();
		let mut later = earlier.consume().compressed();
		let mut a = arrayvec::ArrayVec::<frame::Object, 4>::new();
		let mut b = arrayvec::ArrayVec::<frame::Object, 4>::new();
		assert!(matches!(
			prior.poll_read_objects(&kio::Waiter::noop(), &mut a),
			Poll::Ready(Ok(2))
		));
		assert!(matches!(
			later.poll_read_objects(&kio::Waiter::noop(), &mut b),
			Poll::Ready(Ok(2))
		));
		assert_eq!(
			a.iter()
				.map(|object| object.fields.as_ref().unwrap().id)
				.collect::<Vec<_>>(),
			[90, 100]
		);
		assert_eq!(
			b.iter()
				.map(|object| object.fields.as_ref().unwrap().id)
				.collect::<Vec<_>>(),
			[90, 95]
		);
		assert_eq!(
			a[0].payload.chunk_at(0).as_ptr(),
			b[0].payload.chunk_at(0).as_ptr(),
			"one Object ID shares bytes across different ordinals"
		);
		assert_eq!(
			b[1].payload.contiguous(0),
			b"c".as_slice(),
			"the same ordinal cannot alias a different Object ID"
		);
		assert!(
			a.iter()
				.chain(&b)
				.all(|object| object.fields.as_ref().unwrap().properties.sealed_time == Some(Timestamp::ZERO))
		);
		assert_eq!(live.handle().content().unwrap().cost().0, 3);
	}

	/// Compressed transport refills keep the received slices, share one inflation,
	/// and continue widening hostile tail requests rather than minting local encoders.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn canonical_compressed_ingress_preserves_raw_plain_and_widening() {
		fn ingest(group: group::Producer, object: &frame::Object) -> group::Producer {
			assert!(
				object.inflated.is_some(),
				"compressed ingress carries the stored representation"
			);
			let mut frame = group
				.create_object(frame::Header {
					info: frame::Info {
						size: object.payload.len() as u64,
						timestamp: object.timestamp,
					},
					fields: object.fields.clone(),
					inflated: object.inflated,
				})
				.unwrap();
			let mut at = 0;
			while at < object.payload.len() {
				let chunk = object.payload.chunk_at(at);
				at += chunk.len();
				frame.write(bytes::Bytes::copy_from_slice(&chunk)).unwrap();
			}
			frame.finish().unwrap()
		}
		let info = Info::timed().with_compress(true);
		let reference = track_producer("reference", info.clone());
		let mut source = reference.create_group(group::Info { sequence: 0 }).unwrap();
		let expected: Vec<_> = (0..9)
			.map(|index| bytes::Bytes::from(format!("shared dictionary frame {index}; ").repeat(64)))
			.collect();
		for payload in &expected {
			source.write_frame(Timestamp::ZERO, payload.clone()).unwrap();
		}
		source.finish().unwrap();
		let mut stored = arrayvec::ArrayVec::<frame::Object, 16>::new();
		assert!(matches!(
			source
				.consume()
				.compressed()
				.poll_read_objects(&kio::Waiter::noop(), &mut stored),
			Poll::Ready(Ok(9))
		));
		let producer = track_producer("test", info.clone());
		let requests = producer.requests();
		let consumer = producer.consume();
		let mut live = producer.create_group(group::Info { sequence: 0 }).unwrap();
		live.start_at(8).unwrap();
		let live = ingest(live, &stored[8]);
		let mut raw_live = live.consume().with_range(8..).compressed();
		let raw_live = raw_live.read_frame().await.unwrap().unwrap();
		assert!(
			matches!(live.consume().with_range(8..).read_frame().await, Err(Error::Lagged)),
			"canonical storage does not grant a missing compression prefix"
		);
		let pending = consumer.fetch_range(
			0,
			FetchOptions {
				options: group::Fetch {
					frame_start: 2,
					..Default::default()
				},
				end: Some(4),
			},
		);
		let fill = requests.requested_fill_group().await.unwrap();
		assert_eq!((fill.frame_start(), fill.frame_end()), (0, None));
		let mut head = fill.accept(info).unwrap();
		for object in &stored[..8] {
			head = ingest(head, object);
		}
		let registry = stats::Registry::new(stats::Config::new());
		let meter = registry.publisher_meter(crate::Path::new("demo").unwrap(), &stats::Tier::default());
		let mut first = head.consume().with_meter(meter.clone());
		assert_eq!(first.read_frame().await.unwrap().unwrap().payload, expected[0]);
		moq_clock::sim::advance(Duration::from_millis(20)).await;
		head = ingest(head, &stored[8]);
		head.finish().unwrap();
		pending.await.unwrap();
		let mut raw_head = head.consume().with_range(8..).compressed();
		let raw_head = raw_head.read_frame().await.unwrap().unwrap();
		assert_eq!(
			raw_live.payload.as_ptr(),
			raw_head.payload.as_ptr(),
			"stored compressed bytes are retained once"
		);
		assert_eq!(
			raw_head.payload,
			stored[8].payload.contiguous(0),
			"the relay does not recompress"
		);
		assert_eq!(
			raw_head.arrived().duration_since(raw_live.arrived()),
			Duration::from_millis(20)
		);
		let mut second = head.consume().with_meter(meter);
		for (index, payload) in expected.iter().enumerate() {
			if index != 0 {
				assert_eq!(first.read_frame().await.unwrap().unwrap().payload, *payload);
			}
			assert_eq!(second.read_frame().await.unwrap().unwrap().payload, *payload);
		}
		let inflated: u64 = registry
			.snapshot()
			.traffic()
			.into_iter()
			.map(|(_, _, traffic)| traffic.inflated)
			.sum();
		assert_eq!(
			inflated,
			expected.iter().map(|payload| payload.len() as u64).sum::<u64>()
		);
		for start in 1..9 {
			consumer
				.fetch_range(
					0,
					FetchOptions {
						options: group::Fetch {
							frame_start: start,
							..Default::default()
						},
						end: Some(start + 1),
					},
				)
				.await
				.unwrap();
		}
		assert!(
			requests.poll_requested_fill_group(&kio::Waiter::noop()).is_pending(),
			"compressed tail starts coalesce through the established widening policy"
		);
		assert_eq!(head.handle().content().unwrap().cost().0, 9);
	}

	/// An evicted streaming reader still owns in-flight storage until it is dropped.
	/// A refill must discover those same bytes without changing that reader's abort.
	#[moq_clock::test]
	#[cfg(feature = "async")]
	async fn canonical_owner_follows_an_evicted_streaming_frame_reader() {
		let producer = track_producer("test", Info::timed());
		let mut live = producer.create_group(group::Info { sequence: 0 }).unwrap();
		live.start_at(4).unwrap();
		let observing = live.handle();
		let mut reader = live.consume().with_range(4..);
		let mut live = live
			.create_frame(frame::Info {
				size: 1024,
				timestamp: Some(Timestamp::ZERO),
			})
			.unwrap();
		live.write(vec![4; 512]).unwrap();
		let mut streaming = reader.next_frame().await.unwrap().unwrap();
		let first = streaming.read_chunk().await.unwrap().unwrap();
		let original = first.as_ptr() as usize;
		drop(first);
		let mut head = producer
			.state
			.write()
			.ok()
			.unwrap()
			.insert_group_request(0, group::Bounds { start: 3, end: Some(4) }, None)
			.unwrap();
		head.write_frame(Timestamp::ZERO, b"three".as_slice()).unwrap();
		head.finish().unwrap();
		let identity = Arc::downgrade(&observing.content().unwrap());
		producer
			.state
			.write()
			.ok()
			.unwrap()
			.uncache(0)
			.unwrap()
			.abort(Error::Evicted);
		drop(live);
		assert!(
			identity.upgrade().is_some(),
			"the streaming frame still owns canonical storage"
		);
		let refill = producer
			.state
			.write()
			.ok()
			.unwrap()
			.insert_group_request(0, group::Bounds { start: 4, end: Some(5) }, None)
			.unwrap();
		assert!(Arc::ptr_eq(
			&identity.upgrade().unwrap(),
			&refill.handle().content().unwrap()
		));
		let mut refill = refill
			.create_frame(frame::Info {
				size: 1024,
				timestamp: Some(Timestamp::ZERO),
			})
			.unwrap();
		refill.write(vec![4; 1024]).unwrap();
		let mut refill = refill.finish().unwrap();
		refill.finish().unwrap();
		let complete = refill.consume().with_range(4..).read_frame().await.unwrap().unwrap();
		assert_eq!(
			complete.payload.as_ptr() as usize,
			original,
			"refill reuses the reader-held content"
		);
		assert_eq!(complete.payload.as_ref(), &[4; 1024]);
		assert!(matches!(streaming.read_chunk().await, Err(Error::Evicted)));
	}

	/// Existing limits apply to each delivery, not the union of disjoint valid ranges.
	#[test]
	fn canonical_content_preserves_individually_valid_disjoint_ranges() {
		let producer = track_producer("test", Info::timed());
		let mut live = producer.create_group(group::Info { sequence: 0 }).unwrap();
		const FRAMES: usize = 8192;
		live.start_at(FRAMES as u64).unwrap();
		for _ in 0..FRAMES {
			live.write_frame(Timestamp::ZERO, bytes::Bytes::new()).unwrap();
		}
		let mut head = producer
			.state
			.write()
			.ok()
			.unwrap()
			.insert_group_request(0, group::Bounds { start: 0, end: Some(1) }, None)
			.unwrap();
		head.write_frame(Timestamp::ZERO, bytes::Bytes::new()).unwrap();
		head.finish().unwrap();
		assert_eq!(live.handle().content().unwrap().cost().0, FRAMES + 1);
		assert_eq!(live.frame_count(), 2 * FRAMES);
		assert_eq!(head.frame_count(), 1);
	}

	/// Frame `index` of the groups below: 16 KiB naming its index.
	fn numbered(index: u32) -> bytes::Bytes {
		bytes::Bytes::from(index.to_le_bytes().repeat(4096))
	}

	/// One live group grows through a pool far smaller than it: pool debt sheds its front,
	/// memory stays near the pool, a reader that keeps up reads every frame, and a late
	/// reader is `Evicted`.
	#[test]
	#[cfg(feature = "async")]
	fn a_live_group_sheds_its_front_under_pressure() {
		const POOL: u64 = 4 * 1024 * 1024;
		let pool = cache::Pool::new(cache::Config::default().with_capacity(POOL));
		let producer = track_producer_pooled("test", pool.clone());
		let mut live = producer.create_group(group::Info { sequence: 0 }).unwrap();
		let mut reader = live.consume();
		let mut late = live.consume();
		// 48 MiB, past the 32 MiB tail: pool debt sheds first.
		for index in 0..3_000 {
			live.write_frame(Timestamp::ZERO, numbered(index)).unwrap();
			let frame = reader.read_frame().now_or_never().unwrap();
			let frame = frame.unwrap_or_else(|err| panic!("frame {index}: {err}")).unwrap();
			assert_eq!(frame.payload, numbered(index));
			assert!(pool.used() < 2 * POOL, "frame {index}: {} bytes held", pool.used());
		}
		assert!(matches!(late.read_frame().now_or_never().unwrap(), Err(Error::Evicted)));
		assert!(!live.is_aborted(), "shedding never ends the group");
	}

	/// A join onto a group whose first frame was shed asks for a new group on a track that
	/// opts into refresh, rather than reading a group it could only see `Evicted` in.
	#[test]
	fn a_join_past_a_shed_first_frame_wants_a_new_group() {
		let pool = cache::Pool::new(cache::Config::default().with_capacity(256 * 1024));
		let producer = Producer::new(
			Arc::new(broadcast::Info {
				pool,
				..Default::default()
			}),
			"test",
			Info::default().with_refresh(1_000_000),
		);
		let mut live = producer.append_group().unwrap();
		for index in 0..1_000 {
			live.write_frame(None, numbered(index)).unwrap();
		}
		let _subscriber = producer.subscribe(None);
		assert_eq!(producer.demand().wanted(), Some(1));
	}

	/// A reader that reaches a shed frame of the newest group asks for a new group on a
	/// track that opts into refresh.
	#[test]
	#[cfg(feature = "async")]
	fn a_reader_at_a_shed_frame_wants_a_new_group() {
		let pool = cache::Pool::new(cache::Config::default().with_capacity(256 * 1024));
		let producer = Producer::new(
			Arc::new(broadcast::Info {
				pool,
				..Default::default()
			}),
			"test",
			Info::default().with_refresh(1_000_000),
		);
		// Joined before the group grew, so the join asks for nothing.
		let _subscriber = producer.subscribe(None);
		let mut live = producer.append_group().unwrap();
		let mut reader = live.consume();
		for index in 0..1_000 {
			live.write_frame(None, numbered(index)).unwrap();
		}
		assert_eq!(producer.demand().wanted(), Some(0), "nothing asked yet");
		assert!(matches!(
			reader.read_frame().now_or_never().unwrap(),
			Err(Error::Evicted)
		));
		assert_eq!(producer.demand().wanted(), Some(1), "the reader asked");

		let _joined = producer.subscribe(None);
		assert_eq!(producer.demand().wanted(), Some(1), "a join asks for the same group");
	}
}
