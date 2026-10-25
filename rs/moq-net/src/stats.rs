//! Traffic counter collection for moq-net sessions.
//!
//! This module only *collects*: build a [`Registry`], hand each session a
//! tier-scoped [`Handle`] via [`Registry::tier`], and read the counters back
//! with [`Registry::snapshot`] (host-level rollup, e.g. a `/metrics` scrape) or
//! [`Registry::report`] (per-broadcast detail). Publishing the counters as MoQ
//! broadcasts lives in the `moq-stats` crate, which drains a [`Registry`] on an
//! interval and writes the JSON stats tracks.
//!
//! Traffic is bucketed by an arbitrary [`Tier`] label chosen by business logic
//! (billing class, region, ...) and, within a tier, by broadcast path and
//! [`Role`] (publisher = egress, subscriber = ingress). Connected sessions are
//! tracked separately per (tier, label), counting presence regardless of
//! whether any data flows. The label is a stats key the auth decider picks,
//! independent of what the session may access; the empty label holds sessions
//! without one.
//!
//! # Where counting happens
//!
//! Counting lives in the model layer, not the wire loops. A session tags its
//! origin pair with a [`Session`] context (`crate::Client::with_stats` /
//! `crate::Server::with_stats`, which call `origin::{Consumer, Producer}`
//! `with_stats`); every derived handle (broadcast, announce, track, group, frame)
//! then attributes its reads (egress = publisher) and writes (ingress =
//! subscriber) through that context. So any protocol that drives the model gets
//! the full counter set for free, and an untagged handle pays nothing.
//!
//! Per-counter semantics:
//!
//! * `announces_started` / `announces_ended`: cumulative broadcast
//!   announce/unannounce events on this `(tier, role)`. Driven by the tagged
//!   announce stream on the egress side, and by `create_broadcast` route
//!   transitions on the ingress side.
//! * `announced_bytes`: cumulative broadcast-name length summed over each
//!   model-visible announce and unannounce of this broadcast (the name, not the
//!   encoded message size, so hop/framing overhead isn't charged, and the count
//!   is the same across protocol versions). Kept separate from the `bytes`
//!   payload counter.
//! * `broadcasts_started` / `broadcasts_ended`: per-(broadcast, context) egress
//!   subscription sentinel. The first active subscription a context opens for a
//!   broadcast bumps `broadcasts_started`; the last it closes bumps
//!   `broadcasts_ended`. Summed across contexts, `broadcasts_started -
//!   broadcasts_ended` is the number of distinct sessions currently subscribed
//!   (viewers on the egress side).
//! * `subscriptions_started` / `subscriptions_ended`: cumulative track-level
//!   subscriptions opened/dropped (egress `track::Subscriber`, ingress
//!   `track::Producer`).
//! * `fetches`: cumulative one-shot group fetches *requested* by a calling
//!   context, counted once per coalesced fetch at request time. A fetch that
//!   resolves to `NotFound` still counts. Separate from `subscriptions_started`
//!   and the viewer refcount; fetched payload still flows into `bytes` /
//!   `frames` / `groups`.
//! * `bytes` / `frames` / `groups`: cumulative payload counters bumped as
//!   groups/frames are read (egress) or written (ingress) in the model.
//! * `datagrams`: cumulative single-frame groups carried over unreliable QUIC
//!   datagrams. A datagram is metered as the group it stands in for, so it also
//!   bumps `groups`, `frames`, and `bytes`; this counter breaks out how many of
//!   those took the datagram path.
//! * `inflated`: cumulative bytes inflated from compressed groups for plain
//!   readers (in-process consumers, and egress that cannot carry compression).
//!   Every plain reader of a group shares one inflated copy, so each byte counts
//!   once, on the side of the reader whose read inflated it. A received compressed
//!   datagram inflates on arrival, counted on the receiving side. Kept separate from
//!   `bytes`, which counts payload as stored or sent.
//! * `lag` (egress only): a byte-weighted [`Histogram`] of how far behind each
//!   subscription's acknowledged frontier is. The frontier is the newest frame
//!   timestamp the peer acknowledged: it starts at the track's newest frame when the
//!   subscription opens and advances when a group stream's FIN is acknowledged.
//!   [`Registry::report`] is the sampler: each call weighs the bytes the subscription's
//!   tracks produced since the previous call at `max(newest produced - frontier,
//!   stall)`. The stall is the time, measured between the instants the calls are
//!   given, that the tracks kept producing while the peer acknowledged no FIN; any
//!   acknowledgement resets it. A paused source produces nothing, so it adds no
//!   weight. A subscription that closes is weighed once more, at its lag then and its
//!   stall as of the last call.
//! * `dropped` (egress only, a [`Dropped`]): media written toward a peer on a group
//!   stream that was reset or abandoned before its FIN was acknowledged. The duration
//!   is the span between the group's oldest and newest written frame timestamps:
//!   nothing inside a group is known to be acknowledged, so the whole written span
//!   counts.
//! * `requests` ([`Presence`]): cumulative request/response admissions under the
//!   label and tier at admission. [`Handle::request`] meters their traffic
//!   without starting or ending a connected session.
//! * `sent_bytes` / `received_bytes` ([`Presence`]): cumulative payload bytes
//!   this node sent to, or received from, the sessions and requests under a
//!   label. Each session counts its own bytes, so a write touches no shared
//!   row; a readout sums them.
//! * `sessions_started` / `sessions_ended` ([`Presence`]): cumulative count of
//!   sessions connected/disconnected under a label on this tier.
//!   Driven by [`Handle::session`] (the [`Session`] context); a
//!   [`Session::retag`] ends the session on the old label and starts it on
//!   the new one.
//! * `wire_tx` / `wire_rx` ([`Presence`]): cumulative transport bytes, including
//!   overhead and retransmissions. The transport updates its
//!   [`crate::transport::Wire`] counters as it sends and receives, and
//!   [`Registry::report`] and [`Registry::snapshot`] read each registered [`Wire`]
//!   without polling the session. A retag flushes its old tier and label; dropping the guard flushes
//!   its final nonnegative delta. Unsupported transports contribute zero.
//! * `announces_peak` / `subscriptions_peak` ([`Presence`]): the most
//!   announcements, or subscriptions, any one session under the label held
//!   at once, to compare against what bounds them. A peak never
//!   goes down; a rollup takes the largest rather than the sum.
//!
//! Counters are strictly monotonic (only `fetch_add`); a counter going
//! backwards across reads means the underlying entry was garbage collected
//! (see [`Registry::report`]) and re-created. Downstream consumers should
//! treat decreases as a fresh segment, summing across resets when computing
//! lifetime totals.
//!
//! # Disabled stats
//!
//! [`Registry::disabled`] builds a no-op registry: all counter bumps are
//! silently dropped and nothing is ever tracked. [`Registry::default`] /
//! [`Handle::default`] return one, so call sites can hold a [`Handle`]
//! unconditionally instead of threading an `Option`.
//!
//! # Garbage collection
//!
//! [`Registry::report`] refills a caller-owned [`Report`] with the current
//! per-broadcast detail and prunes
//! entries no longer referenced by any guard, so a publisher draining the
//! registry on an interval keeps it bounded. A registry that is never
//! drained accumulates one entry per broadcast path ever seen; call
//! [`Registry::report`] periodically if you enable a registry without
//! attaching a publisher. [`Registry::snapshot`] never prunes.
//!
//! # Snapshot atomicity
//!
//! Each counter readout loads `*_ended` atomics (with `Acquire`)
//! before their `*_started` counterparts (with `Relaxed`). The matching
//! end bumps in the RAII guards' `Drop` impls use `Release`. With this
//! pairing the readout always satisfies `started >= ended` even on
//! weakly-ordered architectures (ARM, POWER): the `Acquire` load of
//! ended synchronizes-with the `Release` bump that produced the
//! observed value, making every write that happened-before that end
//! (including the matching start bump on whichever thread opened the
//! guard) visible to the reading thread. Started / payload counters can
//! then stay `Relaxed` because the visibility comes for free through
//! the ended pairing. The cost is a slight upward bias on the started
//! counts when a bump lands between the two loads, which never produces
//! a logically impossible (`ended > started`) readout for downstream.
//!
//! # Cycles
//!
//! A [`Registry`] built with excluded patterns ([`Config::exclude`]) returns
//! empty handles (whose bumps no-op) for any path they match. The `moq-stats`
//! publisher excludes its own top-level subtree this way, breaking the feedback
//! loop where serving a stats broadcast would itself generate more stats
//! traffic.

use std::{
	collections::HashMap,
	fmt,
	sync::{
		Arc, Mutex, MutexGuard, PoisonError, Weak,
		atomic::{AtomicU64, AtomicUsize, Ordering, fence},
	},
	time::Duration,
};

use kio::Lock;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_with::{DurationMilliSecondsWithFrac, serde_as};

use crate::{AsPath, PathOwned, Pattern, Patterns, Timestamp, time::Instant, util::SendCell};

mod outcomes;
pub use outcomes::{Failure, Failures, RefusalKind, Refusals};
use outcomes::{FailureCounters, RefusalCounters};

/// Cumulative atomic counters for a single `(tier, role)` on a broadcast.
///
/// Started counters bump when a model handle records activity; their `_ended`
/// counterparts bump from the [`Scope`] / [`Subscription`] / [`Announce`] RAII
/// guards on drop. `broadcasts_started` / `broadcasts_ended` are the
/// per-(broadcast, context) egress subscription sentinel (the first active
/// subscription a context opens for the broadcast bumps `broadcasts_started`,
/// the last to close bumps `broadcasts_ended`), so summed across contexts
/// `broadcasts_started - broadcasts_ended` is the count of distinct sessions
/// currently subscribed.
// Kept crate-private: the load/store orderings are load-bearing (see the
// module-level "Snapshot atomicity" note), so external code only ever sees
// the derived [`Traffic`] readout.
#[derive(Default, Debug)]
pub(crate) struct Counters {
	announces_started: AtomicU64,
	announces_ended: AtomicU64,
	// Cumulative broadcast-name length summed over each announce and unannounce
	// of this broadcast. Counts the name, not the encoded message size, so it
	// doesn't penalize the broadcast for hop/framing overhead. Kept separate
	// from `bytes`, which is media payload.
	announced_bytes: AtomicU64,
	subscriptions_started: AtomicU64,
	subscriptions_ended: AtomicU64,
	// Cumulative one-shot group fetches requested by a calling context. Counted
	// once per coalesced fetch, at request time rather than on resolution; does
	// not touch `subscriptions_started` or the viewer refcount.
	fetches: AtomicU64,
	broadcasts_started: AtomicU64,
	broadcasts_ended: AtomicU64,
	bytes: AtomicU64,
	frames: AtomicU64,
	groups: AtomicU64,
	// Subset of `groups` carried over an unreliable QUIC datagram.
	datagrams: AtomicU64,
	// Bytes inflated from compressed groups for plain readers, once per group.
	inflated: AtomicU64,
	// Content the drift budget gave up on before delivery. Disjoint from the
	// top-level payload counters, which count only what was handed over.
	stale: ContentCounters,
	// Egress only: byte-weighted frontier lag, recorded by `sample`.
	lag: HistogramCounters,
	// Egress only: media written on a group stream that ended unacknowledged.
	dropped: DroppedCounters,
	// Egress only: the subscriptions `sample` walks, swept of closed ones there and
	// whenever the list fills.
	frontiers: Mutex<Vec<Weak<FrontierInner>>>,
}

/// Atomic backing for one [`Content`] readout.
#[derive(Default, Debug)]
struct ContentCounters {
	bytes: AtomicU64,
	frames: AtomicU64,
	groups: AtomicU64,
	datagrams: AtomicU64,
}

impl ContentCounters {
	fn snapshot(&self) -> Content {
		Content {
			bytes: self.bytes.load(Ordering::Relaxed),
			frames: self.frames.load(Ordering::Relaxed),
			groups: self.groups.load(Ordering::Relaxed),
			datagrams: self.datagrams.load(Ordering::Relaxed),
		}
	}

	fn add(&self, content: Content) {
		self.bytes.fetch_add(content.bytes, Ordering::Relaxed);
		self.frames.fetch_add(content.frames, Ordering::Relaxed);
		self.groups.fetch_add(content.groups, Ordering::Relaxed);
		self.datagrams.fetch_add(content.datagrams, Ordering::Relaxed);
	}
}

impl Counters {
	/// Read all atomics into a [`Traffic`]. Ended counters are read with
	/// `Acquire` ordering before their started counterparts so the readout
	/// always satisfies `started >= ended`; see the module-level "Snapshot
	/// atomicity" note. Started / payload counters stay `Relaxed`: the
	/// Acquire on ended synchronizes-with the matching Release on the
	/// end bump, which transitively makes all earlier writes (including
	/// the prior start bump) visible to this thread.
	fn snapshot(&self) -> Traffic {
		let announces_ended = self.announces_ended.load(Ordering::Acquire);
		let subscriptions_ended = self.subscriptions_ended.load(Ordering::Acquire);
		let broadcasts_ended = self.broadcasts_ended.load(Ordering::Acquire);
		let announces_started = self.announces_started.load(Ordering::Relaxed);
		let announced_bytes = self.announced_bytes.load(Ordering::Relaxed);
		let subscriptions_started = self.subscriptions_started.load(Ordering::Relaxed);
		let fetches = self.fetches.load(Ordering::Relaxed);
		let broadcasts_started = self.broadcasts_started.load(Ordering::Relaxed);
		let bytes = self.bytes.load(Ordering::Relaxed);
		let frames = self.frames.load(Ordering::Relaxed);
		let groups = self.groups.load(Ordering::Relaxed);
		let datagrams = self.datagrams.load(Ordering::Relaxed);
		let inflated = self.inflated.load(Ordering::Relaxed);
		let stale = self.stale.snapshot();
		let lag = self.lag.snapshot();
		let dropped = self.dropped.snapshot();
		Traffic {
			announces_started,
			announces_ended,
			announced_bytes,
			broadcasts_started,
			broadcasts_ended,
			subscriptions_started,
			subscriptions_ended,
			fetches,
			bytes,
			frames,
			groups,
			datagrams,
			inflated,
			stale,
			lag,
			dropped,
		}
	}

	/// Take one lag sample at `now` from every live egress subscription, dropping the closed.
	fn sample(&self, now: Instant) {
		let mut frontiers = self.frontiers.lock().unwrap_or_else(PoisonError::into_inner);
		frontiers.retain(|frontier| {
			let Some(frontier) = frontier.upgrade() else {
				return false;
			};
			frontier.sample(Some(now));
			true
		});
	}
}

/// Number of [`Histogram`] buckets: one below each edge, plus the open top bucket.
const BUCKETS: usize = Histogram::EDGES.len() + 1;

/// A byte-weighted histogram over media time, cumulative like every [`Traffic`] counter.
///
/// Each bucket counts the bytes whose sampled value fell below its edge in
/// [`Self::EDGES`] and at or above the edge before it; the last bucket is open
/// above the top edge. Buckets only grow, so the difference of two readouts is the
/// byte-weighted distribution over that interval.
///
/// On the wire it is an object keyed by each bucket's upper edge (`"50ms"` ..
/// `"5s"`, then `"inf"`), with empty buckets omitted.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "HistogramWire", into = "HistogramWire")]
pub struct Histogram {
	buckets: [u64; BUCKETS],
}

impl Histogram {
	/// The exclusive upper edge of every bucket but the last.
	pub const EDGES: [Duration; 7] = [
		Duration::from_millis(50),
		Duration::from_millis(100),
		Duration::from_millis(250),
		Duration::from_millis(500),
		Duration::from_secs(1),
		Duration::from_secs(2),
		Duration::from_secs(5),
	];

	/// The bucket `value` falls in.
	fn bucket(value: Duration) -> usize {
		Self::EDGES.partition_point(|edge| *edge <= value)
	}

	/// Bytes per bucket, lowest first; the last is open above the top edge.
	pub fn buckets(&self) -> &[u64] {
		&self.buckets
	}

	/// Total bytes across every bucket, saturating.
	pub fn total(&self) -> u64 {
		self.buckets.iter().fold(0, |sum, bucket| sum.saturating_add(*bucket))
	}

	/// True when nothing has been recorded.
	pub fn is_empty(&self) -> bool {
		self.buckets.iter().all(|bucket| *bucket == 0)
	}

	/// Fold another readout into this one, bucket by bucket, saturating: a merged
	/// readout may come from a peer.
	pub(crate) fn add(&mut self, other: &Histogram) {
		for (bucket, other) in self.buckets.iter_mut().zip(other.buckets) {
			*bucket = bucket.saturating_add(other);
		}
	}
}

/// The wire shape of a [`Histogram`]: one key per bucket, named by its upper edge.
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct HistogramWire {
	#[serde(rename = "50ms", skip_serializing_if = "is_zero")]
	b50ms: u64,
	#[serde(rename = "100ms", skip_serializing_if = "is_zero")]
	b100ms: u64,
	#[serde(rename = "250ms", skip_serializing_if = "is_zero")]
	b250ms: u64,
	#[serde(rename = "500ms", skip_serializing_if = "is_zero")]
	b500ms: u64,
	#[serde(rename = "1s", skip_serializing_if = "is_zero")]
	b1s: u64,
	#[serde(rename = "2s", skip_serializing_if = "is_zero")]
	b2s: u64,
	#[serde(rename = "5s", skip_serializing_if = "is_zero")]
	b5s: u64,
	#[serde(rename = "inf", skip_serializing_if = "is_zero")]
	inf: u64,
}

fn is_zero(value: &u64) -> bool {
	*value == 0
}

impl From<HistogramWire> for Histogram {
	fn from(w: HistogramWire) -> Self {
		Self {
			buckets: [w.b50ms, w.b100ms, w.b250ms, w.b500ms, w.b1s, w.b2s, w.b5s, w.inf],
		}
	}
}

impl From<Histogram> for HistogramWire {
	fn from(h: Histogram) -> Self {
		let [b50ms, b100ms, b250ms, b500ms, b1s, b2s, b5s, inf] = h.buckets;
		Self {
			b50ms,
			b100ms,
			b250ms,
			b500ms,
			b1s,
			b2s,
			b5s,
			inf,
		}
	}
}

/// Atomic backing for one [`Histogram`] readout.
#[derive(Default, Debug)]
struct HistogramCounters {
	buckets: [AtomicU64; BUCKETS],
}

impl HistogramCounters {
	fn record(&self, value: Duration, weight: u64) {
		if weight > 0 {
			self.buckets[Histogram::bucket(value)].fetch_add(weight, Ordering::Relaxed);
		}
	}

	fn snapshot(&self) -> Histogram {
		Histogram {
			buckets: std::array::from_fn(|i| self.buckets[i].load(Ordering::Relaxed)),
		}
	}
}

/// Media written toward a peer on group streams that were reset or abandoned
/// before the peer acknowledged them. The nested shape of [`Traffic::dropped`].
///
/// Group granularity counts each group's whole written span, so this over-reports
/// media that did arrive.
#[serde_as]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct Dropped {
	/// Cumulative media time: each group's oldest to newest written frame timestamp.
	/// Fractional milliseconds on the wire.
	#[serde_as(as = "DurationMilliSecondsWithFrac<f64>")]
	pub duration: Duration,
	/// Cumulative payload bytes the transport accepted on those streams.
	pub bytes: u64,
	/// Cumulative streams abandoned after writing at least one frame.
	pub groups: u64,
}

impl Dropped {
	/// True when nothing has been dropped.
	pub fn is_empty(&self) -> bool {
		self.duration.is_zero() && self.bytes == 0 && self.groups == 0
	}

	/// Fold another readout into this one, counter by counter, saturating: a merged
	/// readout may come from a peer.
	pub(crate) fn add(&mut self, other: Self) {
		self.duration = self.duration.saturating_add(other.duration);
		self.bytes = self.bytes.saturating_add(other.bytes);
		self.groups = self.groups.saturating_add(other.groups);
	}
}

/// Atomic backing for one [`Dropped`] readout.
#[derive(Default, Debug)]
struct DroppedCounters {
	nanos: AtomicU64,
	bytes: AtomicU64,
	groups: AtomicU64,
}

impl DroppedCounters {
	fn record(&self, span: Duration, bytes: u64) {
		let nanos = u64::try_from(span.as_nanos()).unwrap_or(u64::MAX);
		// Saturating: the span is the peer's timestamps, and a cumulative counter that
		// wrapped would read as a reset.
		let _ = self.nanos.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |total| {
			Some(total.saturating_add(nanos))
		});
		self.bytes.fetch_add(bytes, Ordering::Relaxed);
		self.groups.fetch_add(1, Ordering::Relaxed);
	}

	fn snapshot(&self) -> Dropped {
		Dropped {
			duration: Duration::from_nanos(self.nanos.load(Ordering::Relaxed)),
			bytes: self.bytes.load(Ordering::Relaxed),
			groups: self.groups.load(Ordering::Relaxed),
		}
	}
}

/// Payload-volume counters for content with the same delivery outcome.
///
/// This is the nested shape used by [`Traffic::stale`]. The successfully
/// delivered equivalents remain as top-level [`Traffic`] fields for wire
/// compatibility with existing stats consumers.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct Content {
	/// Cumulative payload bytes.
	pub bytes: u64,
	/// Cumulative frames.
	pub frames: u64,
	/// Cumulative groups.
	pub groups: u64,
	/// Cumulative single-frame groups carried as unreliable datagrams.
	pub datagrams: u64,
}

impl Content {
	/// Fold another readout into this one, counter by counter.
	pub(crate) fn add(&mut self, other: Self) {
		self.bytes += other.bytes;
		self.frames += other.frames;
		self.groups += other.groups;
		self.datagrams += other.datagrams;
	}
}

/// Per-(tier, label) session gauge. One of these is shared (via `Arc`) by every
/// [`Session`] guard for the same label on the same tier: `sessions_started`
/// bumps on connect, `sessions_ended` on disconnect.
#[derive(Default, Debug)]
struct SessionCounters {
	sessions_refused: RefusalCounters,
	sessions_failed: FailureCounters,
	requests: AtomicU64,
	sessions_started: AtomicU64,
	sessions_ended: AtomicU64,
	announces_peak: AtomicU64,
	subscriptions_peak: AtomicU64,
	/// Payload bytes of the sessions recording here. One lock covers the folded totals
	/// and the live sessions, so a readout never goes down when a session leaves.
	bytes: Mutex<SessionBytes>,
	wire_tx: AtomicU64,
	wire_rx: AtomicU64,
	control: [[AtomicU64; 2]; Control::ALL.len()],
}

impl SessionCounters {
	fn peak(&self, cap: Cap) -> &AtomicU64 {
		match cap {
			Cap::Announces => &self.announces_peak,
			Cap::Subscriptions => &self.subscriptions_peak,
		}
	}

	/// Read the gauge and whether all its payload meters have ended. Ended is loaded with `Acquire`
	/// before started with `Relaxed`, the same pairing as [`Counters::snapshot`],
	/// so the readout never shows `ended > started`.
	fn snapshot(&self) -> (Presence, bool) {
		let sessions_failed = self.sessions_failed.snapshot();
		let sessions_ended = self.sessions_ended.load(Ordering::Acquire);
		let sessions_started = self.sessions_started.load(Ordering::Relaxed);
		let (sent_bytes, received_bytes, empty) = {
			let mut bytes = self.bytes();
			let (sent, received) = bytes.read();
			(sent, received, bytes.live.is_empty())
		};
		let presence = Presence {
			sessions_refused: self.sessions_refused.snapshot(),
			sessions_failed,
			requests: self.requests.load(Ordering::Relaxed),
			wire_tx: self.wire_tx.load(Ordering::Relaxed),
			wire_rx: self.wire_rx.load(Ordering::Relaxed),
			sessions_started,
			sessions_ended,
			sent_bytes,
			received_bytes,
			announces_peak: self.announces_peak.load(Ordering::Relaxed),
			subscriptions_peak: self.subscriptions_peak.load(Ordering::Relaxed),
		};
		(presence, empty)
	}

	fn control(&self, totals: &mut [[u64; 2]; Control::ALL.len()]) {
		for (total, counters) in totals.iter_mut().zip(&self.control) {
			for (total, counter) in total.iter_mut().zip(counters) {
				*total += counter.load(Ordering::Relaxed);
			}
		}
	}

	fn bytes(&self) -> MutexGuard<'_, SessionBytes> {
		self.bytes.lock().unwrap_or_else(PoisonError::into_inner)
	}
}

/// The payload bytes of one `(tier, label)` row.
#[derive(Default, Debug)]
struct SessionBytes {
	/// Bytes recorded here by sessions that have since ended or retagged away.
	sent: u64,
	received: u64,
	/// The sessions recording here, keyed by identity so a retag leaves in constant time,
	/// each with its counters as of when it arrived.
	live: HashMap<usize, Arrival>,
}

/// A session recording on a row, and its byte counters when it arrived there.
#[derive(Debug)]
struct Arrival {
	traffic: Arc<SessionTraffic>,
	sent: u64,
	received: u64,
}

impl SessionBytes {
	/// The key a session's arrival lives under. The arrival holds the `Arc`, so no other
	/// session can reuse the address while it is here.
	fn key(traffic: &Arc<SessionTraffic>) -> usize {
		Arc::as_ptr(traffic) as usize
	}

	/// Count `traffic` here from `(sent, received)` on.
	fn join(&mut self, traffic: Arc<SessionTraffic>, (sent, received): (u64, u64)) {
		self.live.insert(
			Self::key(&traffic),
			Arrival {
				traffic,
				sent,
				received,
			},
		);
	}

	/// Stop counting `traffic` here, keeping what it recorded so far, and return the
	/// counters at that cut: a concurrent write lands on one side of the single load.
	fn leave(&mut self, traffic: &Arc<SessionTraffic>) -> (u64, u64) {
		let (sent, received) = traffic.load();
		let arrival = self
			.live
			.remove(&Self::key(traffic))
			.expect("a session is live on its current row");
		self.sent += sent - arrival.sent;
		self.received += received - arrival.received;
		(sent, received)
	}

	/// The row's `(sent, received)` totals. Folds in each session that no handle can
	/// write through anymore, since its count is final.
	fn read(&mut self) -> (u64, u64) {
		let Self { sent, received, live } = self;
		let (mut live_sent, mut live_received) = (0, 0);
		live.retain(|_, arrival| {
			// No Weak<SessionTraffic> exists, so a unique arrival cannot gain another writer.
			let ended = Arc::strong_count(&arrival.traffic) == 1;
			if ended {
				// Pair with the last writer's Arc drop before loading its final relaxed counters.
				fence(Ordering::Acquire);
			}
			let (now_sent, now_received) = arrival.traffic.load();
			let (delta_sent, delta_received) = (now_sent - arrival.sent, now_received - arrival.received);
			if ended {
				*sent += delta_sent;
				*received += delta_received;
			} else {
				live_sent += delta_sent;
				live_received += delta_received;
			}
			!ended
		});
		(*sent + live_sent, *received + live_received)
	}
}

/// A cumulative traffic counter readout for one slice (a broadcast on a
/// `(tier, role)`, or any sum of such slices).
///
/// Every counter is cumulative, so a rate is `delta / delta_t` and a live
/// count is `started - ended`. This is also the wire shape of one entry on a
/// published stats track (the `moq-stats` crate serializes maps of these).
/// Serialize writes both the canonical `*_started`/`*_ended` names and the
/// legacy `announced`/`*_closed` spellings so an older consumer still reads a
/// new relay; deserialize accepts either spelling, with the canonical name
/// winning when both are present. Unknown fields from a newer publisher are
/// ignored and missing fields from an older one default to zero.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Traffic {
	/// Cumulative broadcast announce events on this slice.
	pub announces_started: u64,
	/// Cumulative broadcast unannounce events on this slice.
	pub announces_ended: u64,
	/// Cumulative announce-control bytes: the broadcast name length summed
	/// over each announce and unannounce. Distinct from `bytes` (payload).
	pub announced_bytes: u64,
	/// Per-(broadcast, session) subscription sentinel opens: the first active
	/// subscription a session holds on a broadcast.
	pub broadcasts_started: u64,
	/// Sentinel closes: the session's last subscription to the broadcast ended.
	pub broadcasts_ended: u64,
	/// Cumulative track-level subscriptions opened.
	pub subscriptions_started: u64,
	/// Cumulative track-level subscriptions closed.
	pub subscriptions_ended: u64,
	/// Cumulative one-shot group fetches requested. Counted once per coalesced fetch
	/// when the fetch is issued, so one that resolves to `NotFound` still counts.
	/// Separate from `subscriptions_started` and the viewer refcount. Fetched payload still
	/// flows into `bytes`/`frames`/`groups`.
	pub fetches: u64,
	/// Cumulative payload bytes.
	pub bytes: u64,
	/// Cumulative frames delivered.
	pub frames: u64,
	/// Cumulative groups delivered.
	pub groups: u64,
	/// Cumulative single-frame groups delivered over an unreliable QUIC datagram.
	/// A subset of `groups`: each one also counts there and its payload in
	/// `frames` / `bytes`.
	pub datagrams: u64,
	/// Cumulative bytes inflated from compressed groups for plain readers. Every plain
	/// reader shares one inflated copy per group, so each byte counts once. A received
	/// compressed datagram counts here as it inflates on arrival. `bytes` still counts
	/// payload as stored or sent.
	pub inflated: u64,
	/// Content skipped because it fell behind a subscriber's
	/// [`max_delay`](crate::track::Subscription::max_delay) budget. Disjoint from the top-level payload
	/// counters: skipped content is never handed over. A steady rate here means
	/// subscribers are consistently behind the live edge.
	pub stale: Content,
	/// Egress only: how far behind the peer's acknowledged frontier was, weighted by
	/// the bytes the track produced while it was. Sampled on each
	/// [`Registry::report`]; see the module docs.
	pub lag: Histogram,
	/// Egress only: media written on group streams that were reset or abandoned
	/// before the peer acknowledged them.
	pub dropped: Dropped,
}

/// One spelling of a counter edge on the wire: absent, or a present integer.
///
/// Decoding goes through `u64`, so an explicit `null` is refused rather than
/// read as absent; only a missing field takes the default.
#[derive(Default, Clone, Copy)]
struct Edge(Option<u64>);

impl<'de> Deserialize<'de> for Edge {
	fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		u64::deserialize(deserializer).map(|v| Self(Some(v)))
	}
}

/// Prefer the canonical `*_started`/`*_ended` spelling; fall back to the
/// legacy name so a consumer built after the rename still reads an older relay.
fn counter_edge(canonical: Edge, legacy: Edge) -> u64 {
	canonical.0.or(legacy.0).unwrap_or(0)
}

#[derive(Serialize)]
struct TrafficSer {
	announces_started: u64,
	announced: u64,
	announces_ended: u64,
	announced_closed: u64,
	announced_bytes: u64,
	broadcasts_started: u64,
	broadcasts: u64,
	broadcasts_ended: u64,
	broadcasts_closed: u64,
	subscriptions_started: u64,
	subscriptions: u64,
	subscriptions_ended: u64,
	subscriptions_closed: u64,
	fetches: u64,
	bytes: u64,
	frames: u64,
	groups: u64,
	datagrams: u64,
	inflated: u64,
	stale: Content,
	#[serde(skip_serializing_if = "Histogram::is_empty")]
	lag: Histogram,
	#[serde(skip_serializing_if = "Dropped::is_empty")]
	dropped: Dropped,
}

impl From<Traffic> for TrafficSer {
	fn from(t: Traffic) -> Self {
		Self {
			announces_started: t.announces_started,
			announced: t.announces_started,
			announces_ended: t.announces_ended,
			announced_closed: t.announces_ended,
			announced_bytes: t.announced_bytes,
			broadcasts_started: t.broadcasts_started,
			broadcasts: t.broadcasts_started,
			broadcasts_ended: t.broadcasts_ended,
			broadcasts_closed: t.broadcasts_ended,
			subscriptions_started: t.subscriptions_started,
			subscriptions: t.subscriptions_started,
			subscriptions_ended: t.subscriptions_ended,
			subscriptions_closed: t.subscriptions_ended,
			fetches: t.fetches,
			bytes: t.bytes,
			frames: t.frames,
			groups: t.groups,
			datagrams: t.datagrams,
			inflated: t.inflated,
			stale: t.stale,
			lag: t.lag,
			dropped: t.dropped,
		}
	}
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct TrafficDe {
	announces_started: Edge,
	announced: Edge,
	announces_ended: Edge,
	announced_closed: Edge,
	announced_bytes: u64,
	broadcasts_started: Edge,
	broadcasts: Edge,
	broadcasts_ended: Edge,
	broadcasts_closed: Edge,
	subscriptions_started: Edge,
	subscriptions: Edge,
	subscriptions_ended: Edge,
	subscriptions_closed: Edge,
	fetches: u64,
	bytes: u64,
	frames: u64,
	groups: u64,
	datagrams: u64,
	inflated: u64,
	stale: Content,
	lag: Histogram,
	dropped: Dropped,
}

impl From<TrafficDe> for Traffic {
	fn from(d: TrafficDe) -> Self {
		Self {
			announces_started: counter_edge(d.announces_started, d.announced),
			announces_ended: counter_edge(d.announces_ended, d.announced_closed),
			announced_bytes: d.announced_bytes,
			broadcasts_started: counter_edge(d.broadcasts_started, d.broadcasts),
			broadcasts_ended: counter_edge(d.broadcasts_ended, d.broadcasts_closed),
			subscriptions_started: counter_edge(d.subscriptions_started, d.subscriptions),
			subscriptions_ended: counter_edge(d.subscriptions_ended, d.subscriptions_closed),
			fetches: d.fetches,
			bytes: d.bytes,
			frames: d.frames,
			groups: d.groups,
			datagrams: d.datagrams,
			inflated: d.inflated,
			stale: d.stale,
			lag: d.lag,
			dropped: d.dropped,
		}
	}
}

impl Serialize for Traffic {
	fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		TrafficSer::from(*self).serialize(serializer)
	}
}

impl<'de> Deserialize<'de> for Traffic {
	fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		TrafficDe::deserialize(deserializer).map(Into::into)
	}
}

impl Traffic {
	/// Fold another readout into this one, counter by counter.
	pub fn add(&mut self, other: Traffic) {
		self.announces_started += other.announces_started;
		self.announces_ended += other.announces_ended;
		self.announced_bytes += other.announced_bytes;
		self.broadcasts_started += other.broadcasts_started;
		self.broadcasts_ended += other.broadcasts_ended;
		self.subscriptions_started += other.subscriptions_started;
		self.subscriptions_ended += other.subscriptions_ended;
		self.fetches += other.fetches;
		self.bytes += other.bytes;
		self.frames += other.frames;
		self.groups += other.groups;
		self.datagrams += other.datagrams;
		self.inflated += other.inflated;
		self.stale.add(other.stale);
		self.lag.add(&other.lag);
		self.dropped.add(other.dropped);
	}

	/// True while the broadcast is announced (an announce guard is open).
	pub fn is_announced(&self) -> bool {
		self.announces_started > self.announces_ended
	}

	/// Distinct sessions currently subscribed (viewers on the egress side).
	pub fn active_broadcasts(&self) -> u64 {
		self.broadcasts_started.saturating_sub(self.broadcasts_ended)
	}

	/// Track subscriptions currently open.
	pub fn active_subscriptions(&self) -> u64 {
		self.subscriptions_started.saturating_sub(self.subscriptions_ended)
	}

	/// All bytes attributable to this slice: payload plus announce overhead.
	/// Both inputs are monotonic, so the sum regresses only when the entry was
	/// garbage collected and re-created.
	pub fn total_bytes(&self) -> u64 {
		self.bytes.saturating_add(self.announced_bytes)
	}

	/// True once every started counter equals its ended counterpart: no guard is
	/// held, so no more traffic can flow until a new start.
	pub fn is_idle(&self) -> bool {
		self.announces_started == self.announces_ended
			&& self.subscriptions_started == self.subscriptions_ended
			&& self.broadcasts_started == self.broadcasts_ended
	}
}

/// Request admissions and connected-session presence for a label on a tier, or their sum.
/// The session counters are cumulative connects and disconnects, so
/// `sessions_started - sessions_ended` is the current live session count. The peaks
/// show how close one session came to the bounds on what it holds.
///
/// Like [`Traffic`], this is also the wire shape of one entry on a published
/// sessions, presence, or totals track. Serialize writes both the canonical names and the legacy
/// `sessions`/`sessions_closed` spellings; deserialize accepts either, with
/// the canonical name winning when both are present.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Presence {
	/// Cumulative sessions refused before admission, by reason.
	pub sessions_refused: Refusals,
	/// Cumulative admitted sessions ending abnormally, by kind.
	pub sessions_failed: Failures,
	/// Cumulative request/response exchanges admitted.
	pub requests: u64,
	/// Cumulative sessions connected.
	pub sessions_started: u64,
	/// Cumulative sessions disconnected.
	pub sessions_ended: u64,
	/// Cumulative payload bytes sent by sessions under this label.
	pub sent_bytes: u64,
	/// Cumulative payload bytes received by sessions under this label.
	pub received_bytes: u64,
	/// The most broadcasts (moq-transport: namespaces) any one session has held announced
	/// to us at once.
	pub announces_peak: u64,
	/// The most subscriptions any one session has held on our broadcasts at once.
	pub subscriptions_peak: u64,
	/// Cumulative transport bytes sent, including transport framing and retransmits.
	pub wire_tx: u64,
	/// Cumulative transport bytes received, including transport framing.
	pub wire_rx: u64,
}

#[derive(Serialize)]
struct PresenceSer {
	sessions_refused: Refusals,
	sessions_failed: Failures,
	requests: u64,
	sessions_started: u64,
	sessions: u64,
	sessions_ended: u64,
	sessions_closed: u64,
	sent_bytes: u64,
	received_bytes: u64,
	announces_peak: u64,
	subscriptions_peak: u64,
	wire_tx: u64,
	wire_rx: u64,
}

impl From<Presence> for PresenceSer {
	fn from(p: Presence) -> Self {
		Self {
			sessions_refused: p.sessions_refused,
			sessions_failed: p.sessions_failed,
			requests: p.requests,
			wire_tx: p.wire_tx,
			wire_rx: p.wire_rx,
			sessions_started: p.sessions_started,
			sessions: p.sessions_started,
			sessions_ended: p.sessions_ended,
			sessions_closed: p.sessions_ended,
			sent_bytes: p.sent_bytes,
			received_bytes: p.received_bytes,
			announces_peak: p.announces_peak,
			subscriptions_peak: p.subscriptions_peak,
		}
	}
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct PresenceDe {
	sessions_refused: Refusals,
	sessions_failed: Failures,
	requests: u64,
	sessions_started: Edge,
	sessions: Edge,
	sessions_ended: Edge,
	sessions_closed: Edge,
	sent_bytes: u64,
	received_bytes: u64,
	announces_peak: u64,
	subscriptions_peak: u64,
	#[serde(default)]
	wire_tx: u64,
	#[serde(default)]
	wire_rx: u64,
}

impl From<PresenceDe> for Presence {
	fn from(d: PresenceDe) -> Self {
		Self {
			sessions_refused: d.sessions_refused,
			sessions_failed: d.sessions_failed,
			requests: d.requests,
			wire_tx: d.wire_tx,
			wire_rx: d.wire_rx,
			sessions_started: counter_edge(d.sessions_started, d.sessions),
			sessions_ended: counter_edge(d.sessions_ended, d.sessions_closed),
			sent_bytes: d.sent_bytes,
			received_bytes: d.received_bytes,
			announces_peak: d.announces_peak,
			subscriptions_peak: d.subscriptions_peak,
		}
	}
}

impl Serialize for Presence {
	fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		PresenceSer::from(*self).serialize(serializer)
	}
}

impl<'de> Deserialize<'de> for Presence {
	fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		PresenceDe::deserialize(deserializer).map(Into::into)
	}
}

impl Presence {
	/// Fold another readout into this one. The peaks are per session, so they take the larger.
	pub fn add(&mut self, other: Presence) {
		self.sessions_refused.add(other.sessions_refused);
		self.sessions_failed.add(other.sessions_failed);
		self.requests += other.requests;
		self.wire_tx += other.wire_tx;
		self.wire_rx += other.wire_rx;
		self.sessions_started += other.sessions_started;
		self.sessions_ended += other.sessions_ended;
		self.sent_bytes += other.sent_bytes;
		self.received_bytes += other.received_bytes;
		self.announces_peak = self.announces_peak.max(other.announces_peak);
		self.subscriptions_peak = self.subscriptions_peak.max(other.subscriptions_peak);
	}

	/// Sessions currently connected.
	pub fn active(&self) -> u64 {
		self.sessions_started.saturating_sub(self.sessions_ended)
	}
}

/// Traffic-class label that selects which counter set a session's bumps record
/// in, so a single [`Registry`] can split customer-facing, cluster-peer, regional,
/// etc. traffic. Each tracked broadcast keeps a per-tier counter set on both its
/// publisher and subscriber sides.
///
/// Published stats frames key each tier's counters by its label, so
/// `Tier::new("region/sjc")` records under `"region/sjc"`. The label is an
/// arbitrary path chosen by business logic; an empty label is the default tier.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Tier(PathOwned);

impl Tier {
	/// A tier with the given label. An empty label is the default tier.
	pub fn new(label: impl Into<PathOwned>) -> Self {
		Self(label.into())
	}

	/// The tier label, empty for the default tier.
	pub fn label(&self) -> &PathOwned {
		&self.0
	}

	/// True for the default (unprefixed) tier.
	pub fn is_default(&self) -> bool {
		self.0.is_empty()
	}

	/// The tier label as used in metrics: empty (`""`) for the default tier,
	/// otherwise the label (e.g. `"region/sjc"`), as stats frames key it.
	pub fn as_str(&self) -> &str {
		self.0.as_str()
	}
}

impl fmt::Display for Tier {
	/// The label, empty for the default unprefixed tier.
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		fmt::Display::fmt(&self.0, f)
	}
}

/// Publisher (egress) vs subscriber (ingress) side of a broadcast, used as a
/// label on a [`Snapshot`] traffic row. The internal bump paths track the
/// side statically, so this only surfaces on the aggregate read side.
///
/// This is the direction traffic flowed, not the session role a client advertises
/// in its SETUP ([`crate::setup::Role`]): one session records on both sides.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Role {
	/// Egress: bytes this node published to a peer.
	Publisher,
	/// Ingress: bytes this node consumed from a peer.
	Subscriber,
}

impl Role {
	fn idx(self) -> usize {
		match self {
			Role::Publisher => 0,
			Role::Subscriber => 1,
		}
	}

	/// Lowercase label for this role (`"publisher"` / `"subscriber"`).
	pub fn as_str(self) -> &'static str {
		match self {
			Role::Publisher => "publisher",
			Role::Subscriber => "subscriber",
		}
	}
}

/// Cumulative bytes sent and received, returned per [`Control`] class by [`Snapshot::control`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Bytes {
	/// Bytes successfully encoded for sending, counted even if the stream later fails.
	pub tx: u64,
	/// Bytes of complete messages decoded and consumed.
	pub rx: u64,
}

/// The bounded class of a successfully encoded or decoded control message.
///
/// Counters include prefixes inside a complete codec value, but exclude separately
/// coded selectors and raw payload writes. A framed IETF body counts after its
/// framing decodes, before request validation. Shared reply IDs use [`Self::Other`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Control {
	/// Session setup and information.
	Session,
	/// Namespace advertisements and listings.
	Announce,
	/// Track metadata and listings.
	TrackInfo,
	/// Live subscriptions and their updates.
	Subscribe,
	/// Finite fetch requests.
	Fetch,
	/// Bandwidth probes.
	Probe,
	/// Authorization messages.
	Auth,
	/// Session or request migration.
	Goaway,
	/// Other protocol control messages, including shared request responses.
	Other,
}

impl Control {
	const ALL: [Self; 9] = [
		Self::Session,
		Self::Announce,
		Self::TrackInfo,
		Self::Subscribe,
		Self::Fetch,
		Self::Probe,
		Self::Auth,
		Self::Goaway,
		Self::Other,
	];

	/// The stable label used by control metrics.
	pub fn as_str(self) -> &'static str {
		match self {
			Self::Session => "session",
			Self::Announce => "announce",
			Self::TrackInfo => "track_info",
			Self::Subscribe => "subscribe",
			Self::Fetch => "fetch",
			Self::Probe => "probe",
			Self::Auth => "auth",
			Self::Goaway => "goaway",
			Self::Other => "other",
		}
	}
}

/// A point-in-time, host-level rollup of a registry's counters, returned
/// by [`Registry::snapshot`].
///
/// Every counter is summed across all broadcasts the registry is tracking and
/// split by tier and role, plus per-tier connected-session presence. One entry
/// per tier that recorded any traffic or session, keyed by the tier's label (so
/// an idle tier is simply absent). Intended for a scrape / `/metrics`-style
/// endpoint where per-broadcast cardinality is unwanted; use
/// [`Registry::report`] for the per-broadcast breakdown. A disabled registry
/// yields no rows.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Snapshot {
	/// Traffic totals per tier, indexed by [`Role`] within each tier; read via
	/// [`Self::traffic`].
	traffic: HashMap<Tier, [Traffic; 2]>,
	/// Session presence per tier; read via [`Self::sessions`].
	sessions: HashMap<Tier, Presence>,
	control: HashMap<Tier, [[u64; 2]; Control::ALL.len()]>,
}

impl Snapshot {
	/// The `(tier, class, totals)` control-message rows with any traffic, sorted by tier and class.
	/// Includes prefixes inside complete codec values, but excludes separately coded type selectors,
	/// media payload, and transport framing, acknowledgements, and retransmissions.
	pub fn control(&self) -> Vec<(Tier, Control, Bytes)> {
		let mut rows = Vec::new();
		for (tier, totals) in &self.control {
			for class in Control::ALL {
				let [tx, rx] = totals[class as usize];
				if tx != 0 || rx != 0 {
					rows.push((tier.clone(), class, Bytes { tx, rx }));
				}
			}
		}
		rows.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()).then(a.1.as_str().cmp(b.1.as_str())));
		rows
	}

	/// The `(tier, role, totals)` traffic rows, one publisher and one subscriber
	/// row per tier present. Sorted by tier label then role for stable output.
	pub fn traffic(&self) -> Vec<(Tier, Role, Traffic)> {
		let mut rows = Vec::with_capacity(self.traffic.len() * 2);
		for (tier, roles) in &self.traffic {
			rows.push((tier.clone(), Role::Publisher, roles[Role::Publisher.idx()]));
			rows.push((tier.clone(), Role::Subscriber, roles[Role::Subscriber.idx()]));
		}
		rows.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()).then(a.1.idx().cmp(&b.1.idx())));
		rows
	}

	/// The `(tier, sessions)` presence rows, one per tier present, sorted by tier
	/// label.
	pub fn sessions(&self) -> Vec<(Tier, Presence)> {
		let mut rows: Vec<_> = self.sessions.iter().map(|(tier, s)| (tier.clone(), *s)).collect();
		rows.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
		rows
	}
}

/// The per-broadcast detail [`Registry::report`] fills: one traffic entry per
/// `(broadcast, tier)` and one session entry per `(tier, label)`. Entries are
/// unordered. Reuse one across drains to keep its capacity.
#[derive(Debug, Default, Clone)]
#[non_exhaustive]
pub struct Report {
	/// Per-`(broadcast, tier)` traffic, both roles per entry.
	pub traffic: Vec<TrafficEntry>,
	/// Per-`(tier, label)` connected-session presence.
	pub sessions: Vec<SessionEntry>,
}

/// One `(broadcast, tier)` row of a [`Report`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct TrafficEntry {
	/// The broadcast path the counters are keyed by.
	pub path: PathOwned,
	/// The tier the counters recorded under.
	pub tier: Tier,
	/// Egress counters (this node publishing to peers).
	pub publisher: Traffic,
	/// Ingress counters (this node consuming from peers).
	pub subscriber: Traffic,
	/// This is the entry's final readout: no guard holds it, so the registry
	/// pruned it, and a later entry at the same key starts from zero.
	pub closed: bool,
}

/// One `(tier, label)` row of a [`Report`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct SessionEntry {
	/// The tier the sessions recorded under.
	pub tier: Tier,
	/// The label the sessions record under, empty for sessions without one.
	pub label: PathOwned,
	/// Cumulative request admissions and connect/disconnect gauge.
	pub presence: Presence,
	/// This is the label's final readout: its last session or request ended, so the
	/// registry pruned it, and a later context under the label starts from zero.
	pub closed: bool,
}

/// Settings for a [`Registry`]. Construct with [`Config::new`] and chain the
/// `with_*` setters, then hand it to [`Registry::new`].
///
/// Every field here is about *collection*; the publishing knobs (origin,
/// interval, node, ...) live on the `moq-stats` producer config.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Config {
	/// Patterns whose broadcasts are not tracked: a matching path gets an empty
	/// handle whose bumps no-op. A publisher excludes its own stats subtree
	/// (`.stats/**`) this way, breaking the stats-of-stats feedback loop. Empty
	/// (the default) tracks everything.
	pub exclude: Patterns,
}

impl Config {
	/// A config with default settings: nothing excluded.
	pub fn new() -> Self {
		Self::default()
	}

	/// Add a pattern to exclude from tracking. May be chained to exclude
	/// several.
	pub fn with_exclude(mut self, pattern: Pattern) -> Self {
		self.exclude.insert(pattern);
		self
	}
}

/// Counter collection registry. Cheap to clone (`Arc` inside for the shared
/// state). One instance per relay; sessions get tier-scoped handles via
/// [`Registry::tier`]. The `moq-stats` crate drains it with
/// [`Registry::report`] to publish the counters as MoQ broadcasts.
#[derive(Clone)]
pub struct Registry {
	/// Paths these patterns match get empty handles (bumps no-op); see
	/// [`Config::exclude`].
	exclude: Patterns,
	/// `None` for a disabled registry: bumps are dropped and nothing is tracked.
	shared: Option<Arc<Shared>>,
}

/// State shared by every clone of a [`Registry`].
struct Shared {
	/// Completed entries folded by tier before pruning. Lock before either map
	/// so a snapshot sees each entry either here or live, never both or neither.
	retired: Lock<Snapshot>,
	entries: Lock<HashMap<PathOwned, Arc<BroadcastEntry>>>,
	/// Connected-session gauges keyed by `(tier, label)`. Independent of any
	/// broadcast; surfaced on the per-tier session tracks and in the totals and
	/// presence tracks. A tier's inner map is
	/// created the first time a session records under it.
	sessions: Lock<HashMap<Tier, HashMap<PathOwned, Arc<SessionCounters>>>>,
	wire: Lock<Vec<Weak<WireInner>>>,
}

/// Per-broadcast counters, lazily split by tier. A tier's [`TierCounters`] is
/// created the first time a guard records under that label, so the set of tiers
/// is fully dynamic. A [`Scope`] resolves the `Arc<TierCounters>` once per session
/// tier and hands it to the guards and meters it creates, so the per-byte path
/// never touches this map.
struct BroadcastEntry {
	tiers: Mutex<HashMap<Tier, Arc<TierCounters>>>,
}

impl BroadcastEntry {
	fn new() -> Self {
		Self {
			tiers: Mutex::new(HashMap::new()),
		}
	}

	/// Get-or-create the counters for `tier` on this broadcast.
	fn tier(&self, tier: &Tier) -> Arc<TierCounters> {
		self.tiers
			.lock()
			.unwrap_or_else(PoisonError::into_inner)
			.entry(tier.clone())
			.or_default()
			.clone()
	}
}

/// Publisher and subscriber [`Counters`] for one `(broadcast, tier)`. The two
/// sides are named explicitly (rather than indexed by a `Role` enum) because
/// the bump-path call sites always know which side they're on at compile time.
#[derive(Default)]
struct TierCounters {
	publisher: Counters,
	subscriber: Counters,
}

impl Registry {
	/// Build an enabled registry from `config`.
	pub fn new(config: Config) -> Self {
		let Config { exclude } = config;
		Self {
			exclude,
			shared: Some(Arc::new(Shared {
				retired: Lock::default(),
				entries: Lock::default(),
				sessions: Default::default(),
				wire: Lock::default(),
			})),
		}
	}

	/// Build a no-op registry: every handle is empty and all bumps are dropped.
	pub fn disabled() -> Self {
		Self {
			exclude: Patterns::new(),
			shared: None,
		}
	}

	/// Whether this registry collects counters.
	pub fn is_enabled(&self) -> bool {
		self.shared.is_some()
	}

	/// The excluded patterns. See [`Config::exclude`].
	pub fn exclude(&self) -> &Patterns {
		&self.exclude
	}

	/// The shared state, panicking for a disabled registry. Tests build enabled
	/// registries so this is always present.
	#[cfg(test)]
	fn shared(&self) -> &Arc<Shared> {
		self.shared.as_ref().expect("enabled stats registry")
	}

	/// Returns a tier-scoped handle. Bumps through this handle land in the
	/// tier's counters.
	pub fn tier(&self, tier: Tier) -> Handle {
		Handle {
			stats: self.clone(),
			tier,
		}
	}

	/// Add `n` publisher-side payload bytes on `(path, tier)`. The hidden `fuzz`
	/// module exposes this to the stats producer bench. A report drops the entry
	/// unless a guard still holds it.
	#[cfg(any(test, feature = "fuzz"))]
	#[cfg(feature = "async")]
	pub(crate) fn bump_publisher_bytes(&self, path: impl AsPath, tier: &Tier, n: u64) {
		let Some(entry) = self.entry(path) else {
			return;
		};
		entry.tier(tier).publisher.bytes.fetch_add(n, Ordering::Relaxed);
	}

	/// Resolve the pre-label payload meter used as the stats write benchmark's baseline.
	#[cfg(any(test, feature = "fuzz"))]
	#[cfg(feature = "async")]
	pub(crate) fn publisher_meter(&self, path: impl AsPath, tier: &Tier) -> Meter {
		Meter {
			counters: self.entry(path).map(|entry| entry.tier(tier)),
			traffic: None,
			side: Side::Publisher,
		}
	}

	fn entry(&self, path: impl AsPath) -> Option<Arc<BroadcastEntry>> {
		// A disabled registry never allocates state.
		let shared = self.shared.as_ref()?;
		let path = path.as_path();
		// Skip excluded paths (our own stats broadcasts and any sibling category
		// under the same prefix) so serving a stats broadcast doesn't generate
		// more stats.
		if self.exclude.matches(path.as_str()) {
			return None;
		}
		let owned = path.to_owned();
		let mut entries = shared.entries.lock();
		Some(
			entries
				.entry(owned)
				.or_insert_with(|| Arc::new(BroadcastEntry::new()))
				.clone(),
		)
	}

	/// Get-or-create the session gauge for `label` on `tier`. `None` for a
	/// disabled registry. Unlike [`Self::entry`], labels key sessions rather than
	/// broadcasts, so no cycle-breaking filter is needed.
	fn session_counters(&self, tier: &Tier, label: impl AsPath) -> Option<Arc<SessionCounters>> {
		let shared = self.shared.as_ref()?;
		let owned = label.as_path().to_owned();
		let mut sessions = shared.sessions.lock();
		Some(
			sessions
				.entry(tier.clone())
				.or_default()
				.entry(owned)
				.or_default()
				.clone(),
		)
	}

	/// Take a host-level [`Snapshot`]: every counter summed across all
	/// tracked broadcasts, split by tier and role, plus per-tier session
	/// presence. Briefly takes the entry then the session locks. Returns an
	/// all-zero snapshot for a disabled registry.
	///
	/// Unlike [`Registry::report`], this collapses per-broadcast detail into
	/// node lifetime totals (what a `/metrics`-style scrape wants) and never prunes.
	/// Retired entries remain in these totals after [`Self::report`] prunes them.
	pub fn snapshot(&self) -> Snapshot {
		self.collect_wire();
		let Some(shared) = self.shared.as_ref() else {
			return Snapshot::default();
		};
		let retired = shared.retired.lock();
		let mut snap = retired.clone();
		{
			let entries = shared.entries.lock();
			for entry in entries.values() {
				let tiers = entry.tiers.lock().unwrap_or_else(PoisonError::into_inner);
				for (tier, counters) in tiers.iter() {
					let totals = snap.traffic.entry(tier.clone()).or_default();
					totals[Role::Publisher.idx()].add(counters.publisher.snapshot());
					totals[Role::Subscriber.idx()].add(counters.subscriber.snapshot());
				}
			}
		}
		{
			let sessions = shared.sessions.lock();
			for (tier, labels) in sessions.iter() {
				let totals = snap.sessions.entry(tier.clone()).or_default();
				for counters in labels.values() {
					totals.add(counters.snapshot().0);
					counters.control(snap.control.entry(tier.clone()).or_default());
				}
			}
		}
		snap
	}

	/// Refill `report` with the per-broadcast detail and prune dead entries.
	///
	/// Clears `report`, keeping its capacity so a caller draining on an
	/// interval reuses one report instead of allocating per drain, then fills
	/// every `(broadcast, tier)` traffic readout and every `(tier, label)`
	/// session gauge. Entries no guard references anymore are dropped after
	/// their final readout, which is marked `closed`, so a publisher draining on
	/// an interval sees the closing readout exactly once. A pruned path that
	/// sees traffic again restarts from zero; see the module docs on counter
	/// resets. Leaves the report empty for a disabled registry.
	///
	/// Each call is also one lag sample of every egress subscription, taken at
	/// `now`: the interval since the previous call's instant is the one a stalled
	/// frontier is charged; see the module docs.
	pub fn report(&self, report: &mut Report, now: Instant) {
		self.collect_wire();
		report.traffic.clear();
		report.sessions.clear();
		let Some(shared) = self.shared.as_ref() else {
			return;
		};
		let mut retired = shared.retired.lock();
		// Each readout decides its prune first. Counters only the map holds can't
		// be reached by any guard (a new one needs the map lock held here), so
		// their readout is final: it is marked closed, folded into the retired
		// totals, and dropped, with no bump able to land after it.
		shared.entries.lock().retain(|path, entry| {
			let free = Arc::strong_count(entry) == 1;
			let mut tiers = entry.tiers.lock().unwrap_or_else(PoisonError::into_inner);
			tiers.retain(|tier, counters| {
				// A live frontier holds its counters, so a closed entry has none left to
				// sample: each recorded its last weight as it closed.
				let closed = free && Arc::strong_count(counters) == 1;
				counters.publisher.sample(now);
				let publisher = counters.publisher.snapshot();
				let subscriber = counters.subscriber.snapshot();
				report.traffic.push(TrafficEntry {
					path: path.clone(),
					tier: tier.clone(),
					publisher,
					subscriber,
					closed,
				});
				if closed {
					let totals = retired.traffic.entry(tier.clone()).or_default();
					totals[Role::Publisher.idx()].add(publisher);
					totals[Role::Subscriber.idx()].add(subscriber);
				}
				!closed
			});
			!free || !tiers.is_empty()
		});

		shared.sessions.lock().retain(|tier, labels| {
			labels.retain(|label, counters| {
				// Nothing can join a row only the map holds (that needs the lock held here),
				// but a meter outliving its session still writes until its last clone drops.
				let unheld = Arc::strong_count(counters) == 1;
				let (presence, empty) = counters.snapshot();
				let closed = unheld && empty;
				report.sessions.push(SessionEntry {
					tier: tier.clone(),
					label: label.clone(),
					presence,
					closed,
				});
				if closed {
					retired.sessions.entry(tier.clone()).or_default().add(presence);
					counters.control(retired.control.entry(tier.clone()).or_default());
				}
				!closed
			});
			!labels.is_empty()
		});
	}

	/// Sample each source once before taking the counter maps, independently of held tracks.
	fn collect_wire(&self) {
		let Some(shared) = &self.shared else { return };
		shared.wire.lock().retain(|source| {
			let Some(source) = source.upgrade() else { return false };
			source.sample(false)
		});
	}
}

impl Default for Registry {
	/// A disabled (no-op) registry; see [`Registry::disabled`].
	fn default() -> Self {
		Self::disabled()
	}
}

/// Tier-scoped wrapper around [`Registry`]. What `crate::Client::with_stats` and
/// `crate::Server::with_stats` accept. Cheap to clone.
#[derive(Clone)]
pub struct Handle {
	stats: Registry,
	tier: Tier,
}

impl Handle {
	/// Count a session refused outside relay admission under its resolved label and tier.
	pub fn refuse(&self, label: impl AsPath, reason: RefusalKind) {
		if let Some(presence) = self.stats.session_counters(&self.tier, label) {
			presence
				.sessions_refused
				.counter(reason)
				.fetch_add(1, Ordering::Relaxed);
		}
	}

	/// The registry this handle is tied to.
	pub fn parent(&self) -> &Registry {
		&self.stats
	}

	/// The tier this handle bumps into.
	pub fn tier(&self) -> &Tier {
		&self.tier
	}

	/// Record a connected session under `label` on this tier. Hold
	/// the returned guard for the session's lifetime; dropping it bumps
	/// `sessions_ended`. Counts presence regardless of any data flow, so a
	/// session that merely connects is still billable. Surfaced on the session
	/// track for this tier, keyed by `label`, in its totals, and in `label`'s
	/// presence track.
	pub fn session(&self, label: impl AsPath) -> Session {
		self.context(label, Context::Session)
	}

	/// Count one admitted request under `label` and meter its traffic without session presence.
	///
	/// A request has no session end to classify, so it never records a failure.
	pub fn request(&self, label: impl AsPath) -> Session {
		self.context(label, Context::Request)
	}

	fn context(&self, label: impl AsPath, context: Context) -> Session {
		let label = label.as_path().to_owned();
		let presence = self.stats.session_counters(&self.tier, &label);
		if let Some(presence) = &presence {
			match context {
				Context::Session => {
					presence.sessions_started.fetch_add(1, Ordering::Relaxed);
				}
				Context::Request => {
					presence.requests.fetch_add(1, Ordering::Relaxed);
				}
			}
		}
		let traffic = Arc::new(SessionTraffic::default());
		if let Some(presence) = &presence {
			presence.bytes().join(traffic.clone(), (0, 0));
		}
		Session {
			inner: Some(SendCell::new(Arc::new(SessionInner {
				registry: self.stats.clone(),
				connected: matches!(context, Context::Session),
				current: Mutex::new(Current {
					failure: None,
					tier: self.tier.clone(),
					label,
					presence,
				}),
				traffic,
				generation: AtomicU64::new(0),
				viewers: Mutex::new(HashMap::new()),
				held: Mutex::new(Vec::new()),
				wire: Mutex::new(Vec::new()),
			}))),
		}
	}
}

impl Default for Handle {
	/// A no-op handle backed by a disabled [`Registry`].
	fn default() -> Self {
		Registry::disabled().tier(Tier::default())
	}
}

enum Context {
	Session,
	Request,
}

/// Registered wire counters; dropping this adds their final reading and unregisters them.
#[derive(Default)]
pub struct Wire {
	inner: Option<Arc<WireInner>>,
}

struct WireInner {
	session: SendCell<Arc<SessionInner>>,
	counters: crate::transport::Wire,
	last: Mutex<WireState>,
}

#[derive(Default)]
struct WireState {
	sent_bytes: u64,
	received_bytes: u64,
	closed: bool,
}

impl WireInner {
	fn sample(&self, close: bool) -> bool {
		let current = self.session.current.lock().unwrap_or_else(PoisonError::into_inner);
		self.sample_on(&current, close)
	}

	/// Current always precedes last: tier changes and a tick cannot attribute one reading twice.
	fn sample_on(&self, current: &Current, close: bool) -> bool {
		let mut last = self.last.lock().unwrap_or_else(PoisonError::into_inner);
		if last.closed {
			return false;
		};
		let sent_bytes = self.counters.sent_bytes();
		let received_bytes = self.counters.received_bytes();
		if let Some(presence) = &current.presence {
			presence
				.wire_tx
				.fetch_add(sent_bytes.saturating_sub(last.sent_bytes), Ordering::Relaxed);
			presence
				.wire_rx
				.fetch_add(received_bytes.saturating_sub(last.received_bytes), Ordering::Relaxed);
		}
		*last = WireState {
			sent_bytes,
			received_bytes,
			closed: close,
		};
		!close
	}
}

impl Drop for Wire {
	fn drop(&mut self) {
		if let Some(inner) = &self.inner {
			inner.sample(true);
		}
	}
}

/// Which side of a [`TierCounters`] a bump lands on: publisher (egress) or
/// subscriber (ingress). `Default` is `Publisher`, chosen only so an empty
/// [`Meter`] / [`Scope`] has one; it never records because its counters are `None`.
#[derive(Copy, Clone, Default)]
enum Side {
	#[default]
	Publisher,
	Subscriber,
}

impl Side {
	fn counters(self, tier: &TierCounters) -> &Counters {
		match self {
			Side::Publisher => &tier.publisher,
			Side::Subscriber => &tier.subscriber,
		}
	}
}

/// Traffic context for a connection or request, created by [`Handle::session`] or [`Handle::request`].
///
/// Cheap to clone (an `Arc` inside): one context is shared by both origin handles
/// of a session (its publish and subscribe halves) so presence and viewer counts
/// are never double-attributed. It carries three things:
///
/// * the tier and label, so any broadcast reached through a tagged origin handle
///   resolves the right per-`(path, tier)` counters and its bytes count toward the label,
/// * the presence gauge: `sessions_started` bumps when the context is created and
///   `sessions_ended` when the last clone drops; a request instead bumps `requests`
///   once at admission and never touches session presence,
/// * the egress viewer refcount map (first/last active subscription per broadcast),
///   driving `broadcasts_started` / `broadcasts_ended`.
///
/// [`Session::retag`] moves a live context to another tier and label: presence and traffic
/// recorded afterwards land there, while what was already counted stays put and an
/// open subscription or announce closes on the tier it opened on.
///
/// [`Session::default`] is the no-op context (disabled registry / untagged caller):
/// every bump reached through it is silently dropped, so a handle can hold one
/// unconditionally instead of threading an `Option`.
#[derive(Clone, Default)]
pub struct Session {
	/// `None` for the no-op context (disabled registry or a `default()` handle).
	///
	/// The context and its wire counters reference each other, so every future
	/// holding a context would repeat that recursive Send proof; [`SendCell`]
	/// checks it once here.
	inner: Option<SendCell<Arc<SessionInner>>>,
}

/// Which peak a [`Session::hold`] raises.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Cap {
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	Announces,
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	Subscriptions,
}

/// The shared traffic context; its last clone closes presence only for a session.
struct SessionInner {
	/// Requests meter traffic but never open or close connected-session presence.
	connected: bool,
	registry: Registry,
	/// The current tier, label, and presence gauge, swapped together by [`Session::retag`].
	current: Mutex<Current>,
	/// This session's payload bytes, shared with the meters it hands out.
	traffic: Arc<SessionTraffic>,
	/// Bumped on every tier or label change, so a [`Scope`] notices without taking a lock.
	generation: AtomicU64,
	/// Egress viewer refcount, keyed by absolute broadcast path: the first active
	/// subscription this context opens for a broadcast bumps `broadcasts_started`, the last
	/// to close bumps `broadcasts_ended` on the same counters, even across a tier change.
	viewers: Mutex<HashMap<PathOwned, Viewer>>,
	/// What this session holds against each cap right now, so a tier change can seed the
	/// new tier's peaks with slots it still holds.
	held: Mutex<Vec<(Cap, Weak<AtomicUsize>)>>,
	/// Weak, because a wire guard retains this context until its final reading.
	wire: Mutex<Vec<Weak<WireInner>>>,
}

/// The tier and label a [`Session`] records under right now.
struct Current {
	failure: Option<Failure>,
	tier: Tier,
	label: PathOwned,
	/// The presence gauge for `(tier, label)`, or `None` for a disabled registry.
	presence: Option<Arc<SessionCounters>>,
}

/// One session's payload bytes. Its meters write here, never to a shared row, so a
/// meter handed out before a retag still counts toward the session's current label.
#[derive(Default, Debug)]
struct SessionTraffic {
	sent: AtomicU64,
	received: AtomicU64,
}

impl SessionTraffic {
	fn load(&self) -> (u64, u64) {
		(self.sent.load(Ordering::Relaxed), self.received.load(Ordering::Relaxed))
	}
}

/// One broadcast's viewer refcount within a [`Session`].
struct Viewer {
	subscriptions: u32,
	/// The counters `broadcasts_started` bumped on, where `broadcasts_ended` lands too.
	counters: Arc<TierCounters>,
}

impl Session {
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	pub(crate) fn control(&self, class: Control, bytes: usize, received: bool) {
		let Some(inner) = &self.inner else { return };
		let current = inner.current.lock().unwrap_or_else(PoisonError::into_inner);
		if let Some(counters) = &current.presence {
			counters.control[class as usize][usize::from(received)].fetch_add(bytes as u64, Ordering::Relaxed);
		}
	}

	/// Count a connection's wire counters under this context's label, following its retags.
	pub fn wire(&self, counters: crate::transport::Wire) -> Wire {
		let Some(inner) = &self.inner else {
			return Wire::default();
		};
		let Some(shared) = &inner.registry.shared else {
			return Wire::default();
		};
		let source = Arc::new(WireInner {
			session: inner.clone(),
			counters,
			last: Mutex::new(WireState::default()),
		});
		{
			let mut wire = inner.wire.lock().unwrap_or_else(PoisonError::into_inner);
			wire.retain(|source| source.strong_count() > 0);
			wire.push(Arc::downgrade(&source));
		}
		shared.wire.lock().push(Arc::downgrade(&source));
		Wire { inner: Some(source) }
	}

	/// Record how a transport session under this connected context ended, shared across
	/// clones. A reconnect's later end replaces an earlier one, so the context fails only
	/// if its last session did.
	///
	/// Only a context from [`Handle::session`] has an end to classify: on one from
	/// [`Handle::request`] this records nothing.
	pub(crate) fn end(&self, err: &crate::Error) {
		let Some(inner) = &self.inner else { return };
		if !inner.connected {
			return;
		}
		inner.current.lock().unwrap_or_else(PoisonError::into_inner).failure = Failure::of(err);
	}

	/// Record later traffic and session presence under `(tier, label)`; a request stays counted where admitted.
	pub fn retag(&self, tier: Tier, label: impl AsPath) {
		let Some(inner) = &self.inner else { return };
		let label = label.as_path().to_owned();
		let mut current = inner.current.lock().unwrap_or_else(PoisonError::into_inner);
		if current.tier == tier && current.label == label {
			return;
		}
		inner
			.wire
			.lock()
			.unwrap_or_else(PoisonError::into_inner)
			.retain(|source| {
				let Some(source) = source.upgrade() else { return false };
				source.sample_on(&current, false)
			});
		let presence = inner.registry.session_counters(&tier, &label);
		// Bytes before the cut stay on the old row; later ones, through any meter, go to the new.
		let cut = match &current.presence {
			Some(old) => old.bytes().leave(&inner.traffic),
			None => inner.traffic.load(),
		};
		if let Some(presence) = &presence {
			presence.bytes().join(inner.traffic.clone(), cut);
		}
		if inner.connected
			&& let Some(presence) = &presence
		{
			presence.sessions_started.fetch_add(1, Ordering::Relaxed);
		}
		if let Some(presence) = &presence {
			// Slots still held count on the new tier, but not the old tier's history.
			let mut held = inner.held.lock().unwrap_or_else(PoisonError::into_inner);
			held.retain(|(cap, live)| {
				let Some(live) = live.upgrade() else { return false };
				presence
					.peak(*cap)
					.fetch_max(live.load(Ordering::Acquire) as u64, Ordering::Relaxed);
				true
			});
		}
		if let Some(old) = std::mem::replace(&mut current.presence, presence.clone())
			&& inner.connected
		{
			// Release pairs with the readout's Acquire load of `sessions_ended`.
			old.sessions_ended.fetch_add(1, Ordering::Release);
		}
		current.tier = tier;
		current.label = label;
		inner.generation.fetch_add(1, Ordering::Release);
	}

	/// Count `live` as what this session holds against `cap`, so a tier change carries it.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn track_held(&self, cap: Cap, live: &Arc<AtomicUsize>) {
		let Some(inner) = &self.inner else { return };
		let mut held = inner.held.lock().unwrap_or_else(PoisonError::into_inner);
		// A context reused across reconnects registers again each time: drop the gone ones.
		held.retain(|(_, live)| live.strong_count() > 0);
		held.push((cap, Arc::downgrade(live)));
	}

	/// Record that this session holds `held` of what `cap` limits, raising that peak on its
	/// tier and label.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn hold(&self, cap: Cap, held: u64) {
		let Some(inner) = &self.inner else { return };
		let current = inner.current.lock().unwrap_or_else(PoisonError::into_inner);
		let Some(presence) = &current.presence else { return };
		presence.peak(cap).fetch_max(held, Ordering::Relaxed);
	}

	/// Egress (publisher / reads) scope for a broadcast path. The path is the
	/// absolute broadcast name.
	pub(crate) fn egress(&self, path: impl AsPath) -> Scope {
		self.scope(path, Side::Publisher)
	}

	/// Ingress (subscriber / writes) scope for a broadcast path.
	pub(crate) fn ingress(&self, path: impl AsPath) -> Scope {
		self.scope(path, Side::Subscriber)
	}

	fn scope(&self, path: impl AsPath, side: Side) -> Scope {
		let Some(inner) = &self.inner else {
			return Scope::default();
		};
		let path = path.as_path().to_owned();
		let resolved = inner.resolve(&path);
		Scope {
			session: self.clone(),
			resolved: Some(Box::new(Mutex::new(resolved))),
			side,
			path,
		}
	}

	/// Register one active egress subscription to `path` recording on `counters`.
	/// The first bumps `broadcasts_started` there.
	fn viewer_open(&self, path: &PathOwned, counters: &Arc<TierCounters>) {
		let Some(inner) = &self.inner else { return };
		let mut viewers = inner.viewers.lock().unwrap_or_else(PoisonError::into_inner);
		let viewer = viewers.entry(path.clone()).or_insert_with(|| {
			counters.publisher.broadcasts_started.fetch_add(1, Ordering::Relaxed);
			Viewer {
				subscriptions: 0,
				counters: counters.clone(),
			}
		});
		viewer.subscriptions += 1;
	}

	/// Release one active egress subscription to `path`. The last bumps
	/// `broadcasts_ended` on the counters the first opened on.
	fn viewer_close(&self, path: &PathOwned) {
		let Some(inner) = &self.inner else { return };
		let mut viewers = inner.viewers.lock().unwrap_or_else(PoisonError::into_inner);
		let Some(viewer) = viewers.get_mut(path) else { return };
		viewer.subscriptions -= 1;
		if viewer.subscriptions == 0
			&& let Some(viewer) = viewers.remove(path)
		{
			// Release pairs with the readout's Acquire load of `broadcasts_ended`.
			viewer
				.counters
				.publisher
				.broadcasts_ended
				.fetch_add(1, Ordering::Release);
		}
	}
}

impl SessionInner {
	/// The counters `path` records on under the current tier, tagged with the
	/// generation they were resolved at.
	fn resolve(&self, path: &PathOwned) -> Resolved {
		let (generation, tier) = {
			let current = self.current.lock().unwrap_or_else(PoisonError::into_inner);
			(self.generation.load(Ordering::Relaxed), current.tier.clone())
		};
		Resolved {
			generation,
			counters: self.registry.entry(path).map(|entry| entry.tier(&tier)),
		}
	}
}

impl Drop for SessionInner {
	fn drop(&mut self) {
		let current = self.current.get_mut().expect("stats session poisoned");
		if self.connected
			&& let Some(presence) = &current.presence
		{
			// Release pairs with the readout's Acquire load of `sessions_ended`
			// (see the module-level "Snapshot atomicity" note).
			presence.sessions_ended.fetch_add(1, Ordering::Release);
			if let Some(kind) = current.failure {
				// Read failures before ended; this release also publishes its matching end.
				presence.sessions_failed.counter(kind).fetch_add(1, Ordering::Release);
			}
		}
	}
}

// ---------------------------------------------------------------------------
// Model-layer carriers
//
// These are what a tagged `origin::{Consumer, Producer}` threads down through the
// derived handles (broadcast -> track -> group -> frame). A tagged origin creates a
// [`Scope`] per broadcast, which resolves the per-`(path, tier)` counters again
// only after [`Session::retag`]; child handles carry a cheap [`Meter`] for the
// payload bumps, fixed to the tier the group started under. All of them are no-ops when empty (a
// disabled registry, an excluded path, or an untagged caller), so an untagged
// handle pays nothing.
// ---------------------------------------------------------------------------

/// Payload bump handle carried by the group and frame model handles. Cheap to
/// clone (an `Option<Arc>` plus a `Side`); empty when the broadcast is untracked.
#[derive(Clone, Default)]
pub(crate) struct Meter {
	counters: Option<Arc<TierCounters>>,
	traffic: Option<Arc<SessionTraffic>>,
	side: Side,
}

impl Meter {
	fn counters(&self) -> Option<&Counters> {
		self.counters.as_ref().map(|c| self.side.counters(c))
	}

	/// Bump `groups` once (a group delivered/consumed on this side).
	pub(crate) fn group(&self) {
		if let Some(counters) = self.counters() {
			counters.groups.fetch_add(1, Ordering::Relaxed);
		}
	}

	/// Bump `frames` by `n`.
	pub(crate) fn frames(&self, n: u64) {
		if n == 0 {
			return;
		}
		if let Some(counters) = self.counters() {
			counters.frames.fetch_add(n, Ordering::Relaxed);
		}
	}

	/// Record one datagram of `n` payload bytes. A datagram stands in for the
	/// single-frame group it replaces, so this bumps `groups`, `frames`, and
	/// `bytes` alongside `datagrams`.
	pub(crate) fn datagram(&self, n: u64) {
		if let Some(counters) = self.counters() {
			counters.datagrams.fetch_add(1, Ordering::Relaxed);
			counters.groups.fetch_add(1, Ordering::Relaxed);
			counters.frames.fetch_add(1, Ordering::Relaxed);
			counters.bytes.fetch_add(n, Ordering::Relaxed);
		}
		self.session_bytes(n);
	}

	/// Whether this meter attributes anything, i.e. the broadcast is tracked and the
	/// handle was tagged. A caller holding a count that has to land exactly once can
	/// keep it rather than drop it into an untagged meter.
	pub(crate) fn is_tracked(&self) -> bool {
		self.counters.is_some()
	}

	/// Bump `inflated` by `n`, bytes a plain read inflated from a compressed group.
	pub(crate) fn inflated(&self, n: u64) {
		if let Some(counters) = self.counters() {
			counters.inflated.fetch_add(n, Ordering::Relaxed);
		}
	}

	/// Record content skipped before delivery by the drift budget.
	pub(crate) fn stale(&self, content: Content) {
		if let Some(counters) = self.counters() {
			counters.stale.add(content);
		}
	}

	/// Bump `bytes` by `n`.
	pub(crate) fn bytes(&self, n: u64) {
		if n == 0 {
			return;
		}
		if let Some(counters) = self.counters() {
			counters.bytes.fetch_add(n, Ordering::Relaxed);
		}
		self.session_bytes(n);
	}

	fn session_bytes(&self, n: u64) {
		if let Some(traffic) = &self.traffic {
			match self.side {
				Side::Publisher => &traffic.sent,
				Side::Subscriber => &traffic.received,
			}
			.fetch_add(n, Ordering::Relaxed);
		}
	}
}

/// A per-`(broadcast, side)` scope, carried by the broadcast and track model
/// handles. Created by a tagged origin at the broadcast handoff; hands out
/// [`Meter`]s for the payload path and RAII guards for the subscription / announce
/// lifecycle, each recording under the session's tier at the time. Cheap to clone;
/// empty (no-op) when the broadcast is untracked.
#[derive(Default)]
pub(crate) struct Scope {
	/// The owning context: its tier, and the egress viewer refcount map.
	session: Session,
	/// The counters for `(path, tier)`, re-resolved when the session changes tier.
	/// Per clone, so tracks sharing a broadcast never contend on the per-group path;
	/// boxed to keep every track handle small. `None` for the no-op context.
	resolved: Option<Box<Mutex<Resolved>>>,
	side: Side,
	/// Absolute broadcast path, used to key the viewer refcount and as the
	/// `announced_bytes` length.
	path: PathOwned,
}

/// A [`Scope`]'s counters and the session tier generation they belong to.
#[derive(Clone)]
struct Resolved {
	generation: u64,
	/// `None` when untracked.
	counters: Option<Arc<TierCounters>>,
}

impl Clone for Scope {
	fn clone(&self) -> Self {
		Self {
			session: self.session.clone(),
			resolved: self
				.resolved
				.as_ref()
				.map(|r| Box::new(Mutex::new(r.lock().unwrap_or_else(PoisonError::into_inner).clone()))),
			side: self.side,
			path: self.path.clone(),
		}
	}
}

impl Scope {
	/// The counters for the session's current tier. Skips the registry unless the
	/// tier changed since the last call, so the per-group path stays cheap.
	fn counters(&self) -> Option<Arc<TierCounters>> {
		let inner = self.session.inner.as_ref()?;
		let generation = inner.generation.load(Ordering::Acquire);
		let mut resolved = self.resolved.as_ref()?.lock().unwrap_or_else(PoisonError::into_inner);
		if resolved.generation != generation {
			*resolved = inner.resolve(&self.path);
		}
		resolved.counters.clone()
	}

	/// A payload [`Meter`] for a group/frame derived from this scope.
	pub(crate) fn meter(&self) -> Meter {
		let counters = self.counters();
		// An excluded path counts nowhere, including its session's label.
		let traffic = counters
			.as_ref()
			.and(self.session.inner.as_ref())
			.map(|inner| inner.traffic.clone());
		Meter {
			counters,
			traffic,
			side: self.side,
		}
	}

	/// Open a track-subscription guard: bumps `subscriptions_started` now and
	/// `subscriptions_ended` on drop. On the egress (publisher) side it also drives
	/// the context's viewer refcount (`broadcasts_started` / `broadcasts_ended`).
	pub(crate) fn subscribe(&self) -> Subscription {
		let counters = self.counters();
		let mut viewer = None;
		let mut frontier = Frontier::default();
		if let Some(counters) = &counters {
			self.side
				.counters(counters)
				.subscriptions_started
				.fetch_add(1, Ordering::Relaxed);
			// The viewer refcount and lag are egress-only: `broadcasts_started` counts
			// distinct sessions watching a broadcast.
			if matches!(self.side, Side::Publisher) {
				self.session.viewer_open(&self.path, counters);
				viewer = Some((self.session.clone(), self.path.clone()));
				frontier = Frontier::open(counters);
			}
		}
		Subscription {
			counters,
			side: self.side,
			viewer,
			frontier,
		}
	}

	/// Bump the `fetches` counter once (a coalesced group fetch served).
	pub(crate) fn fetch(&self) {
		if let Some(counters) = self.counters() {
			self.side.counters(&counters).fetches.fetch_add(1, Ordering::Relaxed);
		}
	}

	/// Open an announce guard: bumps `announces_started` and adds the path length to
	/// `announced_bytes` now; on drop bumps `announces_ended` and adds the path
	/// length again. Used for egress announce-stream events and ingress
	/// route-transition (un)announces.
	pub(crate) fn announce(&self) -> Announce {
		let len = self.path.as_str().len() as u64;
		let counters = self.counters();
		if let Some(counters) = &counters {
			let counters = self.side.counters(counters);
			counters.announces_started.fetch_add(1, Ordering::Relaxed);
			counters.announced_bytes.fetch_add(len, Ordering::Relaxed);
		}
		Announce {
			counters,
			side: self.side,
			len,
		}
	}
}

/// RAII guard for a track subscription (either side). See [`Scope::subscribe`].
/// [`Subscription::default`] is an empty no-op guard.
#[derive(Default)]
#[must_use = "drop the guard to record the subscription as closed"]
pub(crate) struct Subscription {
	counters: Option<Arc<TierCounters>>,
	side: Side,
	/// `Some((session, path))` on the egress side, to release the viewer refcount.
	viewer: Option<(Session, PathOwned)>,
	/// The peer's acknowledged frontier on the egress side, empty otherwise.
	frontier: Frontier,
}

impl Subscription {
	/// This subscription's acknowledged frontier, empty (no-op) off the egress side.
	pub(crate) fn frontier(&self) -> &Frontier {
		&self.frontier
	}
}

impl Drop for Subscription {
	fn drop(&mut self) {
		if let Some((session, path)) = &self.viewer {
			session.viewer_close(path);
		}
		if let Some(counters) = &self.counters {
			// Release pairs with the readout's Acquire load of `subscriptions_ended`.
			self.side
				.counters(counters)
				.subscriptions_ended
				.fetch_add(1, Ordering::Release);
		}
	}
}

/// RAII guard for one announce lifetime. See [`Scope::announce`].
#[must_use = "drop the guard to record the unannounce"]
pub(crate) struct Announce {
	counters: Option<Arc<TierCounters>>,
	side: Side,
	len: u64,
}

impl Drop for Announce {
	fn drop(&mut self) {
		if let Some(counters) = &self.counters {
			let counters = self.side.counters(counters);
			counters.announced_bytes.fetch_add(self.len, Ordering::Relaxed);
			// Release pairs with the readout's Acquire load of `announces_ended`.
			counters.announces_ended.fetch_add(1, Ordering::Release);
		}
	}
}

/// What one track has produced: the lag sampler's weight and the edge it measures a
/// subscription's frontier against. Recorded beside the ingress meter on every frame
/// write, whether or not the track is tagged, since its readers may be. An untimed
/// frame has no place in media time, so it records nothing.
#[derive(Default, Debug)]
pub(crate) struct Production {
	// Media-time nanoseconds plus one, so zero means nothing produced yet.
	first: AtomicU64,
	newest: AtomicU64,
	// Cumulative payload bytes of timed frames.
	bytes: AtomicU64,
}

impl Production {
	/// Record `bytes` of a frame at `timestamp`, nothing for an untimed one.
	pub(crate) fn record(&self, timestamp: Option<Timestamp>, bytes: u64) {
		let Some(timestamp) = timestamp else {
			return;
		};
		let at = u64::try_from(Duration::from(timestamp).as_nanos())
			.unwrap_or(u64::MAX)
			.saturating_add(1);
		if self.first.load(Ordering::Relaxed) == 0 {
			let _ = self.first.compare_exchange(0, at, Ordering::Relaxed, Ordering::Relaxed);
		}
		// A max, not a store: B-frames present out of order within a group.
		self.newest.fetch_max(at, Ordering::Relaxed);
		// Release: a sampler that acquires these bytes also sees the edges above.
		self.bytes.fetch_add(bytes, Ordering::Release);
	}

	fn load(cell: &AtomicU64) -> Option<Duration> {
		cell.load(Ordering::Relaxed).checked_sub(1).map(Duration::from_nanos)
	}
}

/// One egress subscription's acknowledged frontier, shared by the wire's group serve
/// tasks (which advance it through [`Delivery`]) and the lag sampler (which reads it on
/// every [`Registry::report`]). Empty (no-op) off the egress side.
#[derive(Clone, Default)]
pub(crate) struct Frontier(Option<Arc<FrontierInner>>);

struct FrontierInner {
	counters: Arc<TierCounters>,
	state: Mutex<FrontierState>,
}

#[derive(Default)]
struct FrontierState {
	/// The newest timestamp the peer acknowledged, seeded with the newest frame of the
	/// first track watched, so a backlog the subscription deliberately starts behind
	/// never reads as lag. `None` until either exists.
	acked: Option<Duration>,
	/// The peer acknowledged a FIN since the last sample.
	moved: bool,
	/// How long the sources kept producing while the peer acknowledged nothing, as of
	/// the last sample.
	stalled: Duration,
	/// The last sample's instant, `None` before the first.
	sampled: Option<Instant>,
	/// The tracks feeding the subscription: its own, or each route copy that may still
	/// deliver to it.
	sources: Vec<Source>,
	/// What retired sources produced since their last sample, each at the edge it had,
	/// so a sample records it even before a replacement has a frame, and never at the
	/// replacement's lag. At most [`MAX_FOLDED`] entries.
	folded: Vec<Folded>,
}

/// The most retired sources a frontier keeps apart between samples. A peer can retire
/// copies as fast as it moves its route, so past this the latest share one entry.
const MAX_FOLDED: usize = 8;

/// Bytes a retired source produced before it stopped feeding the subscription,
/// weighed at the edge it had then.
struct Folded {
	weight: u64,
	newest: Duration,
	first: Duration,
}

impl Folded {
	/// Merge `other` in at the later edge, so a shared entry's lag reads high, never low.
	fn absorb(&mut self, other: Folded) {
		self.weight = self.weight.saturating_add(other.weight);
		self.newest = self.newest.max(other.newest);
		self.first = self.first.min(other.first);
	}
}

/// A track feeding a [`Frontier`], with the produced bytes already sampled.
struct Source {
	production: Arc<Production>,
	sampled: u64,
	/// The [`Watch`] guards naming it; it leaves with the last one.
	watches: usize,
}

impl Frontier {
	fn open(counters: &Arc<TierCounters>) -> Self {
		let inner = Arc::new(FrontierInner {
			counters: counters.clone(),
			state: Default::default(),
		});
		let mut frontiers = counters
			.publisher
			.frontiers
			.lock()
			.unwrap_or_else(PoisonError::into_inner);
		// Sweep the closed once the list fills, so a registry reported rarely or never
		// still tracks the live count.
		if frontiers.len() == frontiers.capacity() {
			frontiers.retain(|frontier| frontier.strong_count() > 0);
			let live = frontiers.len();
			frontiers.reserve(live.max(1));
		}
		frontiers.push(Arc::downgrade(&inner));
		drop(frontiers);
		Self(Some(inner))
	}

	/// Weigh what `production` produces against this frontier until the returned guard
	/// drops. The first track watched seeds the frontier with its newest frame.
	pub(crate) fn watch(&self, production: &Arc<Production>) -> Watch {
		let Some(inner) = &self.0 else {
			return Watch::default();
		};
		let mut state = inner.state();
		match state
			.sources
			.iter_mut()
			.find(|source| Arc::ptr_eq(&source.production, production))
		{
			Some(source) => source.watches += 1,
			None => {
				if state.acked.is_none() {
					state.acked = Production::load(&production.newest);
				}
				state.sources.push(Source {
					production: production.clone(),
					sampled: production.bytes.load(Ordering::Acquire),
					watches: 1,
				});
			}
		}
		Watch {
			frontier: Arc::downgrade(inner),
			production: Some(production.clone()),
		}
	}

	/// Start tracking one group stream written toward the peer.
	pub(crate) fn delivery(&self) -> Delivery {
		Delivery {
			frontier: self.0.clone(),
			..Default::default()
		}
	}
}

impl FrontierInner {
	fn state(&self) -> MutexGuard<'_, FrontierState> {
		self.state.lock().unwrap_or_else(PoisonError::into_inner)
	}

	/// Drop one [`Watch`] on `production`. The last one retires the source, keeping
	/// what it produced since its last sample at the edge it has now.
	fn unwatch(&self, production: &Arc<Production>) {
		let mut state = self.state();
		let Some(index) = state
			.sources
			.iter()
			.position(|source| Arc::ptr_eq(&source.production, production))
		else {
			return;
		};
		let source = &mut state.sources[index];
		source.watches -= 1;
		if source.watches > 0 {
			return;
		}
		let source = state.sources.swap_remove(index);
		// Acquire pairs with `Production::record`, so the edges cover these bytes.
		let weight = production.bytes.load(Ordering::Acquire).saturating_sub(source.sampled);
		if weight > 0
			&& let Some(newest) = Production::load(&production.newest)
			&& let Some(first) = Production::load(&production.first)
		{
			let item = Folded { weight, newest, first };
			if state.folded.len() < MAX_FOLDED {
				state.folded.push(item);
			} else if let Some(last) = state.folded.last_mut() {
				last.absorb(item);
			}
		}
	}

	/// Weigh what the sources produced since the last sample. `now` is the sampler's
	/// instant, or `None` for the final sample as the subscription closes, which
	/// charges no further stall: nothing supplies the time it closed at.
	fn sample(&self, now: Option<Instant>) {
		let mut state = self.state();
		let folded = std::mem::take(&mut state.folded);

		// Any acknowledgement resets the stall, and an interval in which the sources
		// produced while none arrived adds to it. Judged by bytes rather than by
		// timestamps, so a publisher whose timestamps step back cannot hide a viewer
		// that stopped acknowledging.
		let produced = !folded.is_empty()
			|| (state.sources.iter()).any(|source| source.production.bytes.load(Ordering::Relaxed) > source.sampled);
		if state.moved {
			state.stalled = Duration::ZERO;
		}
		if let Some(now) = now {
			if !state.moved
				&& produced && let Some(at) = state.sampled
			{
				state.stalled = state.stalled.saturating_add(now.saturating_duration_since(at));
			}
			state.moved = false;
			state.sampled = Some(now);
		}

		// Opened before anything produced: the earliest frame is where it started.
		if state.acked.is_none() {
			let first = (state.sources.iter()).filter_map(|source| Production::load(&source.production.first));
			state.acked = folded.iter().map(|item| item.first).chain(first).min();
		}
		// Nothing timed produced yet: whatever lands from here is weighed next time.
		let Some(acked) = state.acked else {
			return;
		};
		let stalled = state.stalled;
		let histogram = &self.counters.publisher.lag;
		let weigh = |edge: Duration, weight: u64| histogram.record(edge.saturating_sub(acked).max(stalled), weight);
		// One source's bytes are not another's, a retired copy's nor a live one's: each
		// is weighed at its own edge.
		for item in &folded {
			weigh(item.newest, item.weight);
		}
		for source in &mut state.sources {
			let production = &source.production;
			// Acquire pairs with `Production::record`: the edge read below covers these bytes.
			let bytes = production.bytes.load(Ordering::Acquire);
			let weight = bytes.saturating_sub(source.sampled);
			source.sampled = bytes;
			if weight > 0
				&& let Some(newest) = Production::load(&production.newest)
			{
				weigh(newest, weight);
			}
		}
	}
}

impl Drop for FrontierInner {
	/// The last strong reference (the subscription guard or an in-flight [`Delivery`])
	/// is gone, so no sample will find this frontier again: weigh what the sources
	/// produced since the last one now.
	fn drop(&mut self) {
		self.sample(None);
	}
}

/// Keeps one track weighed against a [`Frontier`]; see [`Frontier::watch`]. Dropping it
/// keeps what the track produced so far, at the edge it had, and stops weighing the rest.
#[derive(Default)]
#[must_use = "dropping the guard stops weighing the track"]
pub(crate) struct Watch {
	frontier: Weak<FrontierInner>,
	production: Option<Arc<Production>>,
}

impl Drop for Watch {
	fn drop(&mut self) {
		if let Some(production) = self.production.take()
			&& let Some(inner) = self.frontier.upgrade()
		{
			inner.unwatch(&production);
		}
	}
}

/// One group stream written toward a peer, reported to its subscription's
/// [`Frontier`]: an acknowledged FIN advances the frontier, and anything else that ends
/// the stream counts what it wrote as dropped. Empty (no-op) off the egress side.
#[derive(Default)]
#[must_use = "an unacknowledged delivery counts as dropped on drop"]
pub(crate) struct Delivery {
	frontier: Option<Arc<FrontierInner>>,
	/// Frames the stream carries.
	frames: u64,
	oldest: Option<Duration>,
	newest: Option<Duration>,
	/// Payload bytes the transport accepted.
	bytes: u64,
}

impl Delivery {
	/// Record one frame written to the stream, at `timestamp` when timed. Its payload
	/// counts only as the transport accepts it; see [`Self::wrote`].
	pub(crate) fn frame(&mut self, timestamp: Option<Timestamp>) {
		if self.frontier.is_none() {
			return;
		}
		self.frames += 1;
		if let Some(at) = timestamp.map(Duration::from) {
			self.oldest = Some(self.oldest.map_or(at, |oldest| oldest.min(at)));
			self.newest = self.newest.max(Some(at));
		}
	}

	/// Record `bytes` of payload the transport accepted.
	pub(crate) fn wrote(&mut self, bytes: usize) {
		if self.frontier.is_some() {
			self.bytes = self.bytes.saturating_add(bytes as u64);
		}
	}

	/// The peer acknowledged the stream's FIN: the frontier moved, to the stream's
	/// newest frame if that is newer.
	pub(crate) fn acked(mut self) {
		let Some(inner) = self.frontier.take() else {
			return;
		};
		if self.frames == 0 {
			return;
		}
		let mut state = inner.state();
		state.moved = true;
		if let Some(newest) = self.newest
			&& state.acked.is_none_or(|acked| newest > acked)
		{
			state.acked = Some(newest);
		}
	}
}

impl Drop for Delivery {
	fn drop(&mut self) {
		let Some(inner) = self.frontier.take() else {
			return;
		};
		if self.frames == 0 {
			return;
		}
		let span = self
			.newest
			.zip(self.oldest)
			.map_or(Duration::ZERO, |(newest, oldest)| newest.saturating_sub(oldest));
		inner.counters.publisher.dropped.record(span, self.bytes);
	}
}

#[cfg(test)]
mod tests {
	use std::sync::{Arc, atomic::Ordering::Relaxed};

	use super::*;

	fn counters(sent: u64, received: u64) -> crate::transport::Wire {
		let wire = crate::transport::Wire::new();
		wire.set_sent_bytes(sent);
		wire.set_received_bytes(received);
		wire
	}

	#[test]
	fn wire_deltas_follow_label_and_tier_changes() {
		let registry = test_stats();
		let old = Tier::new("old");
		let new = Tier::new("new");
		let session = registry.tier(old.clone()).session("room");
		let bytes = counters(10, 20);
		let _wire = session.wire(bytes.clone());
		let report = drain(&registry);
		assert_eq!(report.sessions.len(), 1);
		assert_eq!(report.sessions[0].label.as_str(), "room");
		assert_eq!(report.sessions[0].tier, old);
		assert_eq!(
			(report.sessions[0].presence.wire_tx, report.sessions[0].presence.wire_rx),
			(10, 20)
		);
		bytes.set_sent_bytes(15);
		bytes.set_received_bytes(27);
		session.retag(new.clone(), "hall");
		bytes.set_sent_bytes(25);
		bytes.set_received_bytes(50);
		let report = drain(&registry);
		for (tier, label, expected) in [(old, "room", (15, 27)), (new, "hall", (10, 23))] {
			let row = report
				.sessions
				.iter()
				.find(|row| row.tier == tier && row.label.as_str() == label)
				.unwrap();
			assert_eq!((row.presence.wire_tx, row.presence.wire_rx), expected);
		}
	}

	#[test]
	fn a_wire_counter_reset_never_lowers_presence() {
		let registry = test_stats();
		let session = registry.tier(Tier::default()).session("room");
		let bytes = crate::transport::Wire::new();
		let _wire = session.wire(bytes.clone());
		for (reading, expected) in [((20, 30), (20, 30)), ((3, 4), (20, 30)), ((8, 10), (25, 36))] {
			bytes.set_sent_bytes(reading.0);
			bytes.set_received_bytes(reading.1);
			let report = drain(&registry);
			assert_eq!(
				(report.sessions[0].presence.wire_tx, report.sessions[0].presence.wire_rx),
				expected
			);
		}
	}

	#[test]
	fn disabled_wire_collection_reports_nothing() {
		let registry = Registry::disabled();
		let session = registry.tier(Tier::default()).session("root");
		let wire = session.wire(counters(5, 7));
		assert!(drain(&registry).sessions.is_empty());
		drop(wire);
		assert!(drain(&registry).sessions.is_empty());
	}

	#[test]
	fn wire_guards_hold_the_session_until_the_final_reading() {
		let registry = test_stats();
		let session = registry.tier(Tier::default()).session("room");
		let wire = session.wire(counters(19, 23));
		drop(session);
		assert_eq!(drain(&registry).sessions[0].presence.active(), 1);
		drop(wire);
		let report = drain(&registry);
		assert!(report.sessions[0].closed);
		assert_eq!(report.sessions[0].presence.active(), 0);
		assert_eq!(
			(report.sessions[0].presence.wire_tx, report.sessions[0].presence.wire_rx),
			(19, 23)
		);
	}

	#[test]
	fn presence_wire_fields_survive_serialization() {
		let value: Presence = serde_json::from_str(r#"{"wire_tx":71,"wire_rx":29}"#).unwrap();
		let output = serde_json::to_value(value).unwrap();
		assert_eq!(output["wire_tx"], 71);
		assert_eq!(output["wire_rx"], 29);
	}

	#[test]
	fn outcomes_survive_clones_tier_changes_and_pruning() {
		let stats = test_stats();
		let handle = stats.tier(Tier::new("gateway"));
		handle.refuse("root", RefusalKind::Forbidden);
		let session = handle.session("root");
		let clone = session.clone();
		session.retag(Tier::new("next"), "root");
		// A reconnect's later end replaces an earlier one.
		clone.end(&crate::Error::Transport("lost".into()));
		session.end(&crate::Error::IdleTimeout);
		drop(clone);
		drop(session);
		let before = stats.snapshot();
		let gateway = before
			.sessions()
			.into_iter()
			.find(|(tier, _)| tier.as_str() == "gateway")
			.unwrap()
			.1;
		assert_eq!(gateway.sessions_refused.forbidden, 1);
		assert_eq!(gateway.sessions_failed, Failures::default());
		let next = before
			.sessions()
			.into_iter()
			.find(|(tier, _)| tier.as_str() == "next")
			.unwrap()
			.1;
		assert_eq!(next.sessions_failed.idle_timeout, 1);
		assert_eq!(next.sessions_failed.transport, 0);
		assert_eq!(next.active(), 0);
		drain(&stats);
		let after = stats.snapshot();
		assert_eq!(before.sessions(), after.sessions(), "retirement keeps outcomes once");
		let disabled = Registry::disabled();
		disabled.tier(Tier::default()).refuse("root", RefusalKind::Refused);
		disabled
			.tier(Tier::default())
			.session("root")
			.end(&crate::Error::Expired);
		assert!(disabled.snapshot().sessions().is_empty());
	}

	#[test]
	fn outcome_wire_fields_default_and_roundtrip_without_changing_live_count() {
		let old: Presence = serde_json::from_str(r#"{"sessions":4,"sessions_closed":2}"#).unwrap();
		assert_eq!(old.sessions_refused, Refusals::default());
		assert_eq!(old.sessions_failed, Failures::default());
		let mut current = old;
		current.sessions_refused.request = 3;
		current.sessions_failed.expired = 1;
		assert_eq!(current.active(), 2);
		assert_eq!(
			serde_json::from_str::<Presence>(&serde_json::to_string(&current).unwrap()).unwrap(),
			current
		);
		let mut sum = current;
		sum.add(current);
		assert_eq!(sum.sessions_refused.request, 6);
		assert_eq!(sum.sessions_failed.expired, 2);
	}

	#[test]
	fn default_tier_has_empty_label() {
		let tier = Tier::default();
		assert_eq!(tier.as_str(), "");
		assert_eq!(tier.to_string(), "");
	}

	/// Counters for `(path, tier)`, creating the tier slot if absent.
	fn tier_counters(stats: &Registry, path: &str, tier: &Tier) -> Arc<TierCounters> {
		stats
			.shared()
			.entries
			.lock()
			.get(&PathOwned::from(path.to_string()))
			.expect("entry")
			.tier(tier)
	}

	/// The [`Presence`] for `(tier, label)`, or `None` if absent.
	fn session_snapshot(stats: &Registry, tier: &Tier, label: &str) -> Option<Presence> {
		stats
			.shared()
			.sessions
			.lock()
			.get(tier)
			.and_then(|labels| labels.get(&PathOwned::from(label.to_string())).map(|c| c.snapshot().0))
	}

	fn test_stats() -> Registry {
		Registry::new(Config::new().with_exclude(Pattern::subtree(".stats").unwrap()))
	}

	#[test]
	fn default_and_named_tiers_are_independent() {
		let stats = test_stats();
		let default = stats.tier(Tier::default()).session("root");
		let regional = stats.tier(Tier::new("region/sjc")).session("root");

		default.egress("demo/bbb").meter().bytes(100);
		regional.ingress("demo/bbb").meter().bytes(7);

		let default_counters = tier_counters(&stats, "demo/bbb", &Tier::default());
		let regional_counters = tier_counters(&stats, "demo/bbb", &Tier::new("region/sjc"));
		assert_eq!(default_counters.publisher.bytes.load(Relaxed), 100);
		assert_eq!(default_counters.subscriber.bytes.load(Relaxed), 0);
		assert_eq!(regional_counters.publisher.bytes.load(Relaxed), 0);
		assert_eq!(regional_counters.subscriber.bytes.load(Relaxed), 7);
	}

	/// Each label's sessions row carries the most any one of its sessions held against each
	/// cap, which a slot released does not lower and a rollup does not sum.
	#[test]
	#[cfg(feature = "async")]
	fn sessions_report_their_peaks_against_the_caps() {
		use crate::session::Slots;

		let stats = test_stats();
		let tier = stats.tier(Tier::default());
		let (s1, s2) = (tier.session("acme"), tier.session("acme"));
		let announces = Slots::new(10).with_stats(&s1, Cap::Announces);
		let subscriptions = Slots::new(10).with_stats(&s2, Cap::Subscriptions);
		let other = Slots::new(10).with_stats(&s2, Cap::Announces);

		let held: Vec<_> = (0..3).map(|_| announces.acquire().unwrap()).collect();
		drop(held);
		let _one = announces.acquire().unwrap();
		let _subs: Vec<_> = (0..5).map(|_| subscriptions.acquire().unwrap()).collect();
		let _two: Vec<_> = (0..2).map(|_| other.acquire().unwrap()).collect();

		let (_, presence) = stats
			.snapshot()
			.sessions()
			.into_iter()
			.find(|(t, _)| *t == Tier::default())
			.expect("sessions row");
		assert_eq!(presence.announces_peak, 3, "the peak of one session, not the sum");
		assert_eq!(presence.subscriptions_peak, 5);

		let json = serde_json::to_string(&presence).unwrap();
		assert_eq!(serde_json::from_str::<Presence>(&json).unwrap(), presence);
	}

	/// A session moving tiers brings the slots it still holds to the new tier's peak, but
	/// not the old tier's history.
	#[test]
	#[cfg(feature = "async")]
	fn a_tier_change_carries_the_slots_still_held() {
		use crate::session::Slots;

		let stats = test_stats();
		let session = stats.tier(Tier::default()).session("acme");
		let subscriptions = Slots::new(10).with_stats(&session, Cap::Subscriptions);
		let mut held: Vec<_> = (0..5).map(|_| subscriptions.acquire().unwrap()).collect();
		held.truncate(3);

		let gold = Tier::new("gold");
		session.retag(gold.clone(), "acme");

		let peak = |tier: &Tier| {
			let (_, presence) = stats
				.snapshot()
				.sessions()
				.into_iter()
				.find(|(t, _)| t == tier)
				.expect("sessions row");
			presence.subscriptions_peak
		};
		assert_eq!(peak(&Tier::default()), 5);
		assert_eq!(peak(&gold), 3, "the new tier sees what is still held");
	}

	#[test]
	fn snapshot_rolls_up_by_tier_role_and_sessions() {
		let stats = test_stats();
		let default = stats.tier(Tier::default());
		let regional = stats.tier(Tier::new("region/sjc"));

		// Two default-tier sessions under one label, one regional; presence sums them.
		let s1 = default.session("acme");
		let _s2 = default.session("acme");
		let s3 = regional.session("peer");

		// Default-tier egress across two broadcasts; the snapshot sums them.
		{
			let m = s1.egress("demo/aaa").meter();
			m.bytes(100);
			m.frames(1);
			m.group();
		}
		s1.egress("demo/bbb").meter().bytes(50);
		// Regional ingress on a different tier/role stays isolated.
		s3.ingress("demo/aaa").meter().bytes(7);

		let snap = stats.snapshot();

		let slot = |tier, role| {
			snap.traffic()
				.into_iter()
				.find(|(t, r, _)| *t == tier && *r == role)
				.map(|(_, _, c)| c)
				.expect("row present")
		};

		let default_publisher = slot(Tier::default(), Role::Publisher);
		assert_eq!(
			default_publisher.bytes, 150,
			"default egress bytes sum across broadcasts"
		);
		assert_eq!(default_publisher.frames, 1);
		assert_eq!(default_publisher.groups, 1);

		let regional_subscriber = slot(Tier::new("region/sjc"), Role::Subscriber);
		assert_eq!(regional_subscriber.bytes, 7, "regional ingress isolated by tier/role");
		assert_eq!(slot(Tier::default(), Role::Subscriber).bytes, 0);
		assert_eq!(slot(Tier::new("region/sjc"), Role::Publisher).bytes, 0);

		let sessions = |tier| {
			snap.sessions()
				.into_iter()
				.find(|(t, _)| *t == tier)
				.map(|(_, s)| s)
				.expect("tier present")
		};
		let default_sessions = sessions(Tier::default());
		assert_eq!(
			default_sessions.sessions_started, 2,
			"two default-tier sessions under one label"
		);
		assert_eq!(default_sessions.sessions_ended, 0, "guards still held");
		assert_eq!(sessions(Tier::new("region/sjc")).sessions_started, 1);
	}

	fn drain(stats: &Registry) -> Report {
		let mut report = Report::default();
		stats.report(&mut report, Instant::ZERO);
		report
	}

	#[test]
	fn report_reuses_capacity() {
		// A reused report is cleared, not appended to, and keeps its buffers.
		let stats = test_stats();
		let ctx = stats.tier(Tier::default()).session("root");
		let _scopes: Vec<_> = (0..8).map(|i| ctx.egress(format!("b/{i}").as_str())).collect();

		let mut report = Report::default();
		stats.report(&mut report, Instant::ZERO);
		assert_eq!(report.traffic.len(), 8);
		assert_eq!(report.sessions.len(), 1);
		let (traffic, sessions) = (report.traffic.as_ptr(), report.sessions.as_ptr());

		stats.report(&mut report, Instant::ZERO);
		assert_eq!(report.traffic.len(), 8, "refilled, not appended");
		assert_eq!(report.sessions.len(), 1);
		assert_eq!(report.traffic.as_ptr(), traffic, "traffic buffer reused");
		assert_eq!(report.sessions.as_ptr(), sessions, "sessions buffer reused");
	}

	#[test]
	fn report_returns_detail_and_prunes() {
		// report() surfaces per-broadcast rows while a guard is held, keeps the
		// entry across drains while live, and prunes it on the first drain
		// after the last guard drops (returning the final values that once).
		let stats = test_stats();
		let key = PathOwned::from("foo/bar");
		let ctx = stats.tier(Tier::default()).session("root");
		let scope = ctx.egress("foo/bar");
		let sub = scope.subscribe();
		scope.meter().bytes(42);

		let report = drain(&stats);
		let row = report
			.traffic
			.iter()
			.find(|row| row.path == key)
			.expect("live entry present");
		assert_eq!(row.publisher.bytes, 42);
		assert_eq!(row.publisher.subscriptions_started, 1);
		assert!(!row.publisher.is_idle(), "subscription guard still open");
		assert!(!row.closed, "a held entry is not final");
		assert!(
			stats.shared().entries.lock().contains_key(&key),
			"live entry kept across drains"
		);

		drop(sub);
		drop(scope);

		// The drain after the last guard drops still returns the final values,
		// then prunes the entry.
		let report = drain(&stats);
		let row = report
			.traffic
			.iter()
			.find(|row| row.path == key)
			.expect("closing values still reported once");
		assert_eq!(row.publisher.subscriptions_ended, 1);
		assert!(row.publisher.is_idle());
		assert!(row.closed, "the closing readout is marked final");
		assert!(
			!stats.shared().entries.lock().contains_key(&key),
			"fully-closed entry pruned"
		);
		assert!(drain(&stats).traffic.is_empty(), "nothing left after the prune");
	}

	#[test]
	fn report_keeps_idle_but_announced_entry() {
		// A broadcast with a live announce guard but no traffic must stay in
		// the registry indefinitely: announces_started != announces_ended means a
		// subscription could still begin at any moment.
		let stats = test_stats();
		let key = PathOwned::from("foo/bar");
		let ctx = stats.tier(Tier::default()).session("root");
		let scope = ctx.egress("foo/bar");
		let guard = scope.announce();

		for _ in 0..3 {
			let report = drain(&stats);
			assert!(
				report.traffic.iter().any(|row| row.path == key),
				"announced-but-idle broadcast stays while the guard is held"
			);
		}

		drop(guard);
		drop(scope);
		let report = drain(&stats);
		let row = report.traffic.iter().find(|row| row.path == key).expect("final report");
		assert!(row.publisher.is_idle());
		assert!(!stats.shared().entries.lock().contains_key(&key));
	}

	#[test]
	fn report_prunes_empty_session_labels() {
		// Once the last session under a label disconnects, the label leaves the
		// registry on the drain that reports its final gauge.
		let stats = test_stats();
		let session = stats.tier(Tier::default()).session("acme");

		let report = drain(&stats);
		let row = report
			.sessions
			.iter()
			.find(|row| row.label.as_str() == "acme")
			.expect("root present");
		assert_eq!(row.presence.active(), 1);
		assert!(!row.closed);

		drop(session);
		let report = drain(&stats);
		let row = report
			.sessions
			.iter()
			.find(|row| row.label.as_str() == "acme")
			.expect("final gauge reported once");
		assert_eq!(row.presence.active(), 0);
		assert!(row.closed, "the final gauge is marked closed");
		assert!(drain(&stats).sessions.is_empty(), "root pruned after the last drain");
		assert!(session_snapshot(&stats, &Tier::default(), "acme").is_none());
	}

	#[test]
	fn control_body_totals_follow_retags_and_survive_retirement() {
		let registry = Registry::new(Default::default());
		let first = Tier::new("internal");
		let second = Tier::new("edge");
		let context = registry.tier(first.clone()).session("");
		context.control(Control::Announce, 17, false);
		context.retag(second.clone(), "");
		context.control(Control::TrackInfo, 23, true);
		let expected = vec![
			(second, Control::TrackInfo, Bytes { tx: 0, rx: 23 }),
			(first, Control::Announce, Bytes { tx: 17, rx: 0 }),
		];
		assert_eq!(registry.snapshot().control(), expected);
		drop(context);
		registry.report(&mut Report::default(), Instant::ZERO);
		assert_eq!(registry.snapshot().control(), expected);
		assert_eq!(registry.snapshot().control(), expected, "scrapes do not drain totals");
	}

	#[test]
	fn snapshot_preserves_retired_counters() {
		let stats = test_stats();
		let tier = Tier::default();
		let live = stats.tier(tier.clone()).session("live");
		let scope = live.egress("live/video");
		let _live_sub = scope.subscribe();
		for _ in 0..2 {
			let session = stats.tier(tier.clone()).session("retired");
			let scope = session.egress("retired/video");
			let sub = scope.subscribe();
			scope.meter().bytes(100);
			drop(sub);
			drop(scope);
			drop(session);
			let before = stats.snapshot();
			drain(&stats);
			assert_eq!(stats.snapshot(), before, "pruning must not reset host counters");
		}
		let snap = stats.snapshot();
		let traffic = snap
			.traffic()
			.into_iter()
			.find(|(_, role, _)| *role == Role::Publisher)
			.unwrap()
			.2;
		assert_eq!(traffic.bytes, 200);
		assert_eq!(traffic.subscriptions_started, 3);
		assert_eq!(traffic.subscriptions_ended, 2);
		let sessions = snap.sessions().into_iter().find(|(label, _)| label == &tier).unwrap().1;
		assert_eq!(sessions.sessions_started, 3);
		assert_eq!(sessions.sessions_ended, 2);
		assert_eq!(stats.shared().entries.lock().len(), 1, "retired paths are still pruned");
		assert_eq!(
			stats.shared().sessions.lock()[&tier].len(),
			1,
			"retired labels are still pruned"
		);
		let retired = stats.shared().retired.lock();
		assert_eq!(retired.traffic.len(), 1, "retain only a total per tier");
		assert_eq!(retired.sessions.len(), 1);
	}

	#[cfg(not(target_family = "wasm"))]
	#[test]
	fn snapshot_and_report_transfer_counters_once() {
		let stats = test_stats();
		let worker_stats = stats.clone();
		let worker = std::thread::spawn(move || {
			for i in 0..256 {
				let path = format!("root/{i}");
				let session = worker_stats.tier(Tier::default()).session(path.as_str());
				session.ingress(path.as_str()).meter().bytes(1);
				drop(session);
				drain(&worker_stats);
			}
		});
		let mut previous = 0;
		let mut previous_received = 0;
		while !worker.is_finished() {
			let snap = stats.snapshot();
			let received = snap.sessions().iter().map(|(_, row)| row.received_bytes).sum::<u64>();
			assert!(
				received >= previous_received,
				"retiring a session must retain its final payload bytes"
			);
			assert!(received <= 256);
			previous_received = received;
			let bytes: u64 = snap.traffic().iter().map(|(_, _, traffic)| traffic.bytes).sum();
			assert!(
				bytes >= previous,
				"retiring an entry must not double-count or lose its bytes"
			);
			assert!(bytes <= 256);
			previous = bytes;
		}
		worker.join().unwrap();
		let snap = stats.snapshot();
		assert_eq!(
			snap.traffic().iter().map(|(_, _, traffic)| traffic.bytes).sum::<u64>(),
			256
		);
		assert_eq!(snap.sessions()[0].1.sessions_started, 256);
		assert_eq!(snap.sessions()[0].1.sessions_ended, 256);
		assert_eq!(snap.sessions()[0].1.received_bytes, 256);
		assert!(stats.shared().entries.lock().is_empty());
		assert!(stats.shared().sessions.lock().is_empty());
		let retired = stats.shared().retired.lock();
		assert_eq!(retired.traffic.len(), 1);
		assert_eq!(retired.sessions.len(), 1);
	}

	#[test]
	fn paths_under_exclude_are_no_op() {
		// Our own stats broadcasts (and any sibling category under the same
		// prefix) must not feed back into the registry.
		let stats = test_stats();
		let ctx = stats.tier(Tier::default()).session("root");
		let scope = ctx.egress(".stats/node/sjc");
		scope.meter().bytes(100);
		let _guard = scope.announce();
		let _sub = scope.subscribe();
		assert!(stats.shared().entries.lock().is_empty());
	}

	#[test]
	fn disabled_stats_are_noop() {
		// A disabled registry allocates no shared state; every handle is empty
		// and bumps are dropped.
		let stats = Registry::default();
		assert!(stats.shared.is_none());
		let ctx = stats.tier(Tier::default()).session("root");
		let scope = ctx.egress("demo/bbb");
		scope.meter().bytes(100);
		let _guard = scope.announce();
		let _sub = scope.subscribe();
		assert!(drain(&stats).traffic.is_empty());
		assert!(stats.snapshot().traffic().is_empty());
	}

	#[test]
	fn session_counts_by_label() {
		// session() counts connected sessions per label, independent of any
		// broadcast: open bumps `sessions_started`, drop bumps `sessions_ended`.
		let stats = test_stats();
		let ext = stats.tier(Tier::default());

		let snap = |label: &str| {
			session_snapshot(&stats, &Tier::default(), label).map(|p| (p.sessions_started, p.sessions_ended))
		};

		let a1 = ext.session("acme");
		let a2 = ext.session("acme");
		let b1 = ext.session("globex");
		assert_eq!(snap("acme"), Some((2, 0)), "two sessions under one label");
		assert_eq!(snap("globex"), Some((1, 0)), "a distinct label is counted separately");

		drop(a1);
		assert_eq!(snap("acme"), Some((2, 1)));
		drop(a2);
		drop(b1);
		assert_eq!(snap("acme"), Some((2, 2)));
		assert_eq!(snap("globex"), Some((1, 1)));
	}

	#[test]
	fn traffic_parses_with_missing_and_unknown_fields() {
		// Wire forward/backward compat: a frame entry from an older publisher
		// (missing fields) or a newer one (extra fields) must still parse.
		let old: Traffic = serde_json::from_str(r#"{"announced":1,"bytes":5}"#).expect("older shape parses");
		assert_eq!(old.announces_started, 1);
		assert_eq!(old.bytes, 5);
		assert_eq!(old.announced_bytes, 0, "missing fields default to zero");

		let new: Traffic = serde_json::from_str(r#"{"announces_started":1,"announces_ended":1,"future_counter":9}"#)
			.expect("newer shape parses");
		assert!(new.is_idle());
	}

	#[test]
	fn snapshot_reads_ended_before_started() {
		// Reading ended counters before their started counterparts is the
		// guarantee that a readout never shows ended > started under concurrent
		// bumps. This unit-test pins the ordering at the source level so a
		// future refactor that re-orders the loads trips the test.
		let src = include_str!("stats.rs");
		// Find the body of `impl Counters { fn snapshot(...) ... }` and
		// check the line order.
		let body_start = src.find("fn snapshot(&self) -> Traffic").expect("snapshot fn present");
		let body = &src[body_start..];
		let body = &body[..body.find("\n\t}\n").expect("snapshot fn end")];
		let ended_pos = body.find("self.announces_ended.load").expect("announces_ended load");
		let started_pos = body
			.find("self.announces_started.load")
			.expect("announces_started load");
		assert!(
			ended_pos < started_pos,
			"announces_ended must be loaded before announces_started; reversing breaks the started>=ended invariant",
		);
		let subs_ended_pos = body
			.find("self.subscriptions_ended.load")
			.expect("subscriptions_ended load");
		let subs_pos = body
			.find("self.subscriptions_started.load")
			.expect("subscriptions_started load");
		assert!(
			subs_ended_pos < subs_pos,
			"subscriptions_ended must be loaded before subscriptions_started",
		);
		let bcast_ended_pos = body.find("self.broadcasts_ended.load").expect("broadcasts_ended load");
		let bcast_pos = body
			.find("self.broadcasts_started.load")
			.expect("broadcasts_started load");
		assert!(
			bcast_ended_pos < bcast_pos,
			"broadcasts_ended must be loaded before broadcasts_started",
		);
	}

	#[test]
	fn context_presence_closes_on_last_clone() {
		// The reshaped Session context bumps `sessions_started` once at creation and
		// `sessions_ended` only when the last clone drops.
		let stats = test_stats();
		let snap = |label: &str| {
			session_snapshot(&stats, &Tier::default(), label).map(|p| (p.sessions_started, p.sessions_ended))
		};

		let ctx = stats.tier(Tier::default()).session("acme");
		assert_eq!(snap("acme"), Some((1, 0)));

		let clone = ctx.clone();
		// A clone shares the Arc: no extra `sessions_started`, and dropping one does nothing.
		assert_eq!(snap("acme"), Some((1, 0)));
		drop(ctx);
		assert_eq!(snap("acme"), Some((1, 0)));
		drop(clone);
		assert_eq!(snap("acme"), Some((1, 1)));
	}

	#[test]
	fn retag_moves_presence() {
		let stats = test_stats();
		let gold = Tier::new("gold");
		let snap = |tier: &Tier| session_snapshot(&stats, tier, "acme").map(|p| (p.sessions_started, p.sessions_ended));

		let ctx = stats.tier(Tier::default()).session("acme");
		ctx.retag(Tier::default(), "acme");
		assert_eq!(snap(&Tier::default()), Some((1, 0)), "the same tier is a no-op");

		ctx.retag(gold.clone(), "acme");
		assert_eq!(snap(&Tier::default()), Some((1, 1)));
		assert_eq!(snap(&gold), Some((1, 0)));

		drop(ctx);
		assert_eq!(snap(&gold), Some((1, 1)), "the session closes on its current tier");
	}

	#[test]
	fn retag_moves_subsequent_traffic() {
		let stats = test_stats();
		let gold = Tier::new("gold");
		let ctx = stats.tier(Tier::default()).session("acme");
		let scope = ctx.egress("demo/bbb");
		let clone = scope.clone();

		let before = scope.meter();
		let sub = scope.subscribe();
		let announce = scope.announce();
		before.bytes(10);

		ctx.retag(gold.clone(), "acme");
		// A meter handed out earlier keeps its tier; everything after moves.
		before.bytes(1);
		scope.meter().bytes(5);
		clone.meter().bytes(7);
		let sub2 = scope.subscribe();
		scope.fetch();

		drop(sub);
		drop(sub2);
		drop(announce);

		let old = tier_counters(&stats, "demo/bbb", &Tier::default()).publisher.snapshot();
		let new = tier_counters(&stats, "demo/bbb", &gold).publisher.snapshot();
		let old_presence = session_snapshot(&stats, &Tier::default(), "acme").unwrap();
		let new_presence = session_snapshot(&stats, &gold, "acme").unwrap();
		assert_eq!(old_presence.sent_bytes, 10);
		assert_eq!(new_presence.sent_bytes, 13, "a retained meter follows the retag");
		assert_eq!(old.bytes, 11);
		assert_eq!(new.bytes, 12);
		assert_eq!((old.fetches, new.fetches), (0, 1));

		// Each guard closes on the tier it opened on, so neither tier leaks a gauge.
		assert_eq!((old.subscriptions_started, old.subscriptions_ended), (1, 1));
		assert_eq!((new.subscriptions_started, new.subscriptions_ended), (1, 1));
		assert_eq!((old.announces_started, old.announces_ended), (1, 1));
		// The viewer opened on the old tier and closes there, even though the
		// session's last subscription was opened under the new one.
		assert_eq!((old.broadcasts_started, old.broadcasts_ended), (1, 1));
		assert_eq!((new.broadcasts_started, new.broadcasts_ended), (0, 0));
		assert!(old.is_idle() && new.is_idle());
	}

	#[test]
	fn retag_moves_presence_between_labels_on_one_tier() {
		let stats = test_stats();
		let tier = Tier::default();
		let session = stats.tier(tier.clone()).session("project/root");
		session.egress("project/live").meter().bytes(5);

		session.retag(tier.clone(), "project/key/123");
		session.egress("project/live").meter().bytes(7);

		let root = session_snapshot(&stats, &tier, "project/root").unwrap();
		let key = session_snapshot(&stats, &tier, "project/key/123").unwrap();
		assert_eq!((root.sessions_started, root.sessions_ended, root.sent_bytes), (1, 1, 5));
		assert_eq!((key.sessions_started, key.sessions_ended, key.sent_bytes), (1, 0, 7));
	}

	#[test]
	fn retag_back_counts_bytes_once() {
		let stats = test_stats();
		let tier = Tier::default();
		let session = stats.tier(tier.clone()).session("a");
		let meter = session.ingress("a/live").meter();
		meter.bytes(1);
		session.retag(tier.clone(), "b");
		meter.bytes(2);
		session.retag(tier.clone(), "a");
		meter.bytes(4);

		let received = |label| session_snapshot(&stats, &tier, label).unwrap().received_bytes;
		assert_eq!((received("a"), received("b")), (5, 2));
	}

	#[test]
	fn session_bytes_outlive_the_session() {
		let stats = test_stats();
		let tier = Tier::default();
		let session = stats.tier(tier.clone()).session("acme");
		let meter = session.egress("acme/live").meter();
		meter.bytes(3);
		drop(session);

		let mut report = Report::default();
		stats.report(&mut report, Instant::ZERO);
		assert!(!report.sessions[0].closed, "a live meter keeps its label's row open");
		meter.datagram(4);
		drop(meter);

		stats.report(&mut report, Instant::ZERO);
		assert_eq!(report.sessions[0].presence.sent_bytes, 7);
		assert!(report.sessions[0].closed);
		assert_eq!(
			stats.snapshot().sessions[&tier].sent_bytes,
			7,
			"retired totals keep the bytes"
		);
	}

	#[test]
	fn excluded_paths_skip_session_bytes() {
		let stats = test_stats();
		let tier = Tier::default();
		let session = stats.tier(tier.clone()).session("acme");
		session.egress(".stats/node").meter().bytes(9);
		assert_eq!(session_snapshot(&stats, &tier, "acme").unwrap().sent_bytes, 0);
	}

	#[test]
	fn meter_bumps_the_right_side() {
		// A payload meter records on its own side only.
		let stats = test_stats();
		let ctx = stats.tier(Tier::default()).session("root");

		let egress = ctx.egress("demo/bbb").meter();
		egress.group();
		egress.frames(3);
		egress.bytes(100);

		let ingress = ctx.ingress("demo/bbb").meter();
		ingress.group();
		ingress.frames(1);
		ingress.bytes(7);

		let counters = tier_counters(&stats, "demo/bbb", &Tier::default());
		let pub_ = counters.publisher.snapshot();
		let sub = counters.subscriber.snapshot();
		assert_eq!((pub_.groups, pub_.frames, pub_.bytes), (1, 3, 100));
		assert_eq!((sub.groups, sub.frames, sub.bytes), (1, 1, 7));
	}

	#[test]
	fn egress_subscribe_drives_subscriptions_and_viewers() {
		// An egress subscription bumps `subscriptions_started` and, being the context's first
		// for the broadcast, `broadcasts_started`. Dropping closes both.
		let stats = test_stats();
		let ctx = stats.tier(Tier::default()).session("root");
		let raw = || tier_counters(&stats, "demo/bbb", &Tier::default()).publisher.snapshot();

		let scope = ctx.egress("demo/bbb");
		let s1 = scope.subscribe();
		let s2 = scope.subscribe();
		let r = raw();
		assert_eq!(r.subscriptions_started, 2, "two track subs");
		assert_eq!(r.broadcasts_started, 1, "one context => one viewer");
		assert_eq!(r.broadcasts_ended, 0);

		drop(s1);
		assert_eq!(raw().broadcasts_ended, 0, "context still has a sub open");
		drop(s2);
		let r = raw();
		assert_eq!(r.subscriptions_ended, 2);
		assert_eq!(r.broadcasts_ended, 1, "last sub closed => one broadcasts_ended");
	}

	#[test]
	fn distinct_contexts_are_distinct_viewers() {
		// Two contexts (sessions) subscribing to the same broadcast are two viewers.
		let stats = test_stats();
		let raw = || tier_counters(&stats, "demo/bbb", &Tier::default()).publisher.snapshot();

		let v1 = stats.tier(Tier::default()).session("a").egress("demo/bbb").subscribe();
		assert_eq!(raw().broadcasts_started, 1);
		let v2 = stats.tier(Tier::default()).session("b").egress("demo/bbb").subscribe();
		assert_eq!(raw().broadcasts_started, 2, "two distinct contexts => two viewers");

		drop(v1);
		assert_eq!(raw().active_broadcasts(), 1);
		drop(v2);
		assert_eq!(raw().broadcasts_ended, 2);
	}

	#[test]
	fn ingress_subscription_has_no_viewer() {
		// An ingress (producer-lifetime) subscription bumps subscriptions but never
		// the viewer refcount, which is egress-only.
		let stats = test_stats();
		let ctx = stats.tier(Tier::default()).session("root");
		let guard = ctx.ingress("demo/bbb").subscribe();
		let sub = tier_counters(&stats, "demo/bbb", &Tier::default())
			.subscriber
			.snapshot();
		assert_eq!(sub.subscriptions_started, 1);
		assert_eq!(sub.broadcasts_started, 0, "ingress has no viewer refcount");
		drop(guard);
		assert_eq!(
			tier_counters(&stats, "demo/bbb", &Tier::default())
				.subscriber
				.snapshot()
				.subscriptions_ended,
			1
		);
	}

	#[test]
	fn request_admission_survives_clones_retiering_and_retirement_without_presence() {
		let stats = test_stats();
		let request = stats.tier(Tier::new("old")).request("root");
		let clone = request.clone();
		request.egress("root/video").meter().bytes(7);
		request.retag(Tier::new("new"), "root");
		clone.egress("root/video").meter().bytes(11);
		drop(request);
		drop(clone);
		let mut report = Report::default();
		stats.report(&mut report, Instant::ZERO);
		for (label, requests, bytes) in [("old", 1, 7), ("new", 0, 11)] {
			let tier = Tier::new(label);
			let presence = report.sessions.iter().find(|entry| entry.tier == tier).unwrap();
			assert_eq!(presence.presence.requests, requests);
			assert_eq!(presence.presence.active(), 0);
			assert_eq!(
				(presence.presence.sessions_started, presence.presence.sessions_ended),
				(0, 0)
			);
			assert!(presence.closed);
			assert_eq!(
				report
					.traffic
					.iter()
					.find(|entry| entry.tier == tier)
					.unwrap()
					.publisher
					.bytes,
				bytes
			);
		}
		stats.report(&mut report, Instant::ZERO);
		assert!(report.sessions.is_empty());
		let totals = stats.snapshot().sessions();
		assert_eq!(totals.iter().map(|(_, p)| p.requests).sum::<u64>(), 1);
	}

	#[test]
	fn request_counter_is_additive_and_compatible_with_old_frames() {
		let old: Presence = serde_json::from_str(r#"{"sessions":2,"sessions_closed":1}"#).unwrap();
		assert_eq!(old.requests, 0);
		let mut requests = Presence {
			requests: 3,
			..Default::default()
		};
		requests.add(old);
		assert_eq!(
			serde_json::from_str::<Presence>(&serde_json::to_string(&requests).unwrap()).unwrap(),
			requests
		);
		assert_eq!(
			(requests.requests, requests.sessions_started, requests.sessions_ended),
			(3, 2, 1)
		);
		assert!(serde_json::from_str::<Presence>(r#"{"requests":null}"#).is_err());
	}

	#[test]
	fn fetch_counts_separately_from_subscriptions() {
		// A fetch bumps `fetches`, not `subscriptions_started` or the viewer refcount.
		let stats = test_stats();
		let ctx = stats.tier(Tier::default()).session("root");
		let scope = ctx.egress("demo/bbb");
		scope.fetch();
		scope.fetch();
		let r = tier_counters(&stats, "demo/bbb", &Tier::default()).publisher.snapshot();
		assert_eq!(r.fetches, 2);
		assert_eq!(r.subscriptions_started, 0);
		assert_eq!(r.broadcasts_started, 0);
	}

	#[test]
	fn announce_guard_records_bytes_on_open_and_close() {
		// The announce guard bumps `announces_started` + the path length on open, and
		// `announces_ended` + the path length again on drop.
		let stats = test_stats();
		let ctx = stats.tier(Tier::default()).session("root");
		let path_len = "demo/bbb".len() as u64;

		let guard = ctx.egress("demo/bbb").announce();
		let r = tier_counters(&stats, "demo/bbb", &Tier::default()).publisher.snapshot();
		assert_eq!(r.announces_started, 1);
		assert_eq!(r.announces_ended, 0);
		assert_eq!(r.announced_bytes, path_len);

		drop(guard);
		let r = tier_counters(&stats, "demo/bbb", &Tier::default()).publisher.snapshot();
		assert_eq!(r.announces_ended, 1);
		assert_eq!(
			r.announced_bytes,
			path_len * 2,
			"path length recorded on open and close"
		);
	}

	#[test]
	fn disabled_context_is_noop() {
		// A default (disabled) context resolves empty scopes: every bump is dropped.
		let ctx = Session::default();
		let scope = ctx.egress("demo/bbb");
		scope.meter().bytes(100);
		let _guard = scope.announce();
		let _sub = scope.subscribe();
		scope.fetch();
		// No registry to inspect; the point is that none of this panics or allocates.
		assert!(ctx.inner.is_none());
	}

	#[test]
	fn fetches_serde_roundtrips() {
		// The new `fetches` field is additive: an older frame omits it (defaults to
		// zero), and it survives a roundtrip.
		let old: Traffic = serde_json::from_str(r#"{"bytes":5}"#).expect("older shape parses");
		assert_eq!(old.fetches, 0);

		let t = Traffic {
			fetches: 9,
			..Default::default()
		};
		let json = serde_json::to_string(&t).unwrap();
		let back: Traffic = serde_json::from_str(&json).unwrap();
		assert_eq!(back.fetches, 9);
	}

	#[test]
	fn session_snapshot_reads_ended_before_started() {
		// Same `ended`-before-`started` invariant as `Counters::snapshot`, pinned
		// at the source level so a reordering refactor can't let
		// `sessions_ended > sessions_started` leak into a readout.
		let src = include_str!("stats.rs");
		let body_start = src
			.find("fn snapshot(&self) -> (Presence, bool)")
			.expect("SessionCounters::snapshot fn present");
		let body = &src[body_start..];
		let body = &body[..body.find("\n\t}\n").expect("SessionCounters::snapshot fn end")];
		let ended_pos = body.find("self.sessions_ended.load").expect("sessions_ended load");
		let started_pos = body.find("self.sessions_started.load").expect("sessions_started load");
		assert!(
			ended_pos < started_pos,
			"sessions_ended must be loaded before sessions_started",
		);
	}

	fn expected_traffic() -> Traffic {
		Traffic {
			announces_started: 2,
			announces_ended: 1,
			broadcasts_started: 4,
			broadcasts_ended: 3,
			subscriptions_started: 6,
			subscriptions_ended: 5,
			bytes: 9,
			..Default::default()
		}
	}

	fn expected_presence() -> Presence {
		Presence {
			sessions_started: 3,
			sessions_ended: 1,
			..Default::default()
		}
	}

	#[test]
	fn traffic_decodes_old_new_and_both_spellings() {
		// A new consumer reads an old relay, a new relay, and the dual-name
		// frame this serializer actually emits, all as the same Traffic.
		let expected = expected_traffic();
		let old = r#"{"announced":2,"announced_closed":1,"broadcasts":4,"broadcasts_closed":3,"subscriptions":6,"subscriptions_closed":5,"bytes":9}"#;
		let new = r#"{"announces_started":2,"announces_ended":1,"broadcasts_started":4,"broadcasts_ended":3,"subscriptions_started":6,"subscriptions_ended":5,"bytes":9}"#;
		assert_eq!(serde_json::from_str::<Traffic>(old).unwrap(), expected);
		assert_eq!(serde_json::from_str::<Traffic>(new).unwrap(), expected);
		let both = serde_json::to_string(&expected).unwrap();
		assert!(both.contains("\"announces_started\":2"), "{both}");
		assert!(both.contains("\"announced\":2"), "{both}");
		assert!(both.contains("\"announces_ended\":1"), "{both}");
		assert!(both.contains("\"announced_closed\":1"), "{both}");
		assert!(both.contains("\"broadcasts_started\":4"), "{both}");
		assert!(both.contains("\"broadcasts\":4"), "{both}");
		assert!(both.contains("\"subscriptions_started\":6"), "{both}");
		assert!(both.contains("\"subscriptions\":6"), "{both}");
		assert_eq!(serde_json::from_str::<Traffic>(&both).unwrap(), expected);
	}

	#[test]
	fn presence_decodes_old_new_and_both_spellings() {
		let expected = expected_presence();
		assert_eq!(
			serde_json::from_str::<Presence>(r#"{"sessions":3,"sessions_closed":1}"#).unwrap(),
			expected
		);
		assert_eq!(
			serde_json::from_str::<Presence>(r#"{"sessions_started":3,"sessions_ended":1}"#).unwrap(),
			expected
		);
		let both = serde_json::to_string(&expected).unwrap();
		assert!(both.contains("\"sessions_started\":3"), "{both}");
		assert!(both.contains("\"sessions\":3"), "{both}");
		assert!(both.contains("\"sessions_ended\":1"), "{both}");
		assert!(both.contains("\"sessions_closed\":1"), "{both}");
		assert_eq!(serde_json::from_str::<Presence>(&both).unwrap(), expected);
	}

	#[test]
	fn counter_edge_canonical_wins_when_spellings_disagree() {
		let traffic: Traffic =
			serde_json::from_str(r#"{"announces_started":9,"announced":1,"announces_ended":8,"announced_closed":0}"#)
				.unwrap();
		assert_eq!(traffic.announces_started, 9);
		assert_eq!(traffic.announces_ended, 8);

		let presence: Presence =
			serde_json::from_str(r#"{"sessions_started":4,"sessions":0,"sessions_ended":2,"sessions_closed":9}"#)
				.unwrap();
		assert_eq!(presence.sessions_started, 4);
		assert_eq!(presence.sessions_ended, 2);
	}

	#[test]
	fn counter_edge_refuses_null() {
		// A present null is malformed, not absent: it must not fall through to the legacy spelling or to zero.
		assert!(serde_json::from_str::<Traffic>(r#"{"announces_started":null,"announced":7}"#).is_err());
		assert!(serde_json::from_str::<Traffic>(r#"{"subscriptions_closed":null}"#).is_err());
		assert!(serde_json::from_str::<Presence>(r#"{"sessions_started":null}"#).is_err());
		assert!(serde_json::from_str::<Presence>(r#"{"sessions":null,"sessions_closed":1}"#).is_err());
	}
}

/// The lag sampler and dropped-media counters, driven through the model the way the
/// wire's group serve tasks drive them, at report instants the test supplies.
#[cfg(all(test, not(loom)))]
mod lag_tests {
	use futures::FutureExt;

	use super::*;
	use crate::{broadcast, track};

	const PATH: &str = "demo";
	const TICK: Duration = Duration::from_secs(1);
	/// Bytes per produced group: the weight one tick of media carries.
	const GROUP_BYTES: u64 = 30 * 100;

	struct Harness {
		stats: Registry,
		session: Session,
		broadcast: broadcast::Producer,
		track: track::Producer,
		/// Media time of the next group's first frame.
		next: Duration,
		/// The instant the next report samples at.
		now: Instant,
	}

	impl Harness {
		fn new() -> Self {
			let stats = Registry::new(Config::new());
			let session = stats.tier(Tier::default()).session("root");
			let broadcast = broadcast::Info::new().produce();
			let track = broadcast.create_track("video", track::Info::timed()).unwrap();
			Self {
				stats,
				session,
				broadcast,
				track,
				next: Duration::ZERO,
				now: Instant::ZERO,
			}
		}

		/// A tagged egress subscription, as a session serving a viewer holds one.
		fn subscribe(&self) -> track::Subscriber {
			self.subscribe_as(&self.session)
		}

		fn subscribe_as(&self, session: &Session) -> track::Subscriber {
			self.broadcast
				.consume()
				.with_stats(session.egress(PATH))
				.track("video")
				.unwrap()
				.subscribe(None)
				.now_or_never()
				.expect("track is live")
				.expect("subscribed")
		}

		/// Produce one second of media as one group of 30 frames over one tick, and
		/// return the frame timestamps written.
		fn produce(&mut self) -> Vec<Timestamp> {
			let mut group = self.track.append_group().unwrap();
			let mut written = Vec::new();
			for i in 0..30u64 {
				// Whole milliseconds, the track's scale, so the timestamps written
				// toward the peer match what the track stored.
				let at = self.next + Duration::from_millis(i * 33);
				let ts = Timestamp::try_from(at).unwrap();
				group.write_frame(ts, vec![0u8; 100]).unwrap();
				written.push(ts);
			}
			group.finish().unwrap();
			self.next += TICK;
			self.now += TICK;
			written
		}

		/// Write a group toward the peer through `sub`'s frontier, every byte accepted.
		fn write(sub: &track::Subscriber, frames: &[Timestamp]) -> Delivery {
			let mut delivery = sub.delivery();
			for ts in frames {
				delivery.frame(Some(*ts));
				delivery.wrote(100);
			}
			delivery
		}

		/// One report; the egress row for the broadcast.
		fn tick(&self) -> Traffic {
			let mut report = Report::default();
			self.stats.report(&mut report, self.now);
			report
				.traffic
				.iter()
				.find(|e| e.path.as_str() == PATH)
				.map(|e| e.publisher)
				.expect("broadcast row")
		}
	}

	/// The buckets that grew between two readouts.
	fn grew(before: &Traffic, after: &Traffic) -> Vec<(usize, u64)> {
		before
			.lag
			.buckets()
			.iter()
			.zip(after.lag.buckets())
			.enumerate()
			.filter(|(_, (b, a))| a > b)
			.map(|(i, (b, a))| (i, a - b))
			.collect()
	}

	/// The egress lag bytes recorded so far, without a report sampling.
	fn recorded(stats: &Registry) -> u64 {
		stats
			.snapshot()
			.traffic()
			.into_iter()
			.find(|(_, role, _)| matches!(role, Role::Publisher))
			.map_or(0, |(_, _, traffic)| traffic.lag.total())
	}

	#[test]
	fn bucket_edges_are_upper_exclusive() {
		assert_eq!(Histogram::bucket(Duration::ZERO), 0);
		assert_eq!(Histogram::bucket(Duration::from_millis(49)), 0);
		assert_eq!(Histogram::bucket(Duration::from_millis(50)), 1);
		assert_eq!(Histogram::bucket(Duration::from_millis(4999)), 6);
		assert_eq!(Histogram::bucket(Duration::from_secs(5)), 7);
		assert_eq!(Histogram::bucket(Duration::MAX), 7);
	}

	#[test]
	fn media_rate_reader_stays_in_the_lowest_bucket() {
		let mut h = Harness::new();
		let sub = h.subscribe();
		let mut last = h.tick();
		for _ in 0..5 {
			let frames = h.produce();
			Harness::write(&sub, &frames).acked();
			let now = h.tick();
			assert_eq!(grew(&last, &now), vec![(0, GROUP_BYTES)]);
			last = now;
		}
		assert_eq!(last.dropped.groups, 0);
	}

	#[test]
	fn stalled_reader_walks_up_the_buckets() {
		let mut h = Harness::new();
		// Media already produced before the subscription opens is a deliberate start
		// point, not lag.
		h.produce();
		let sub = h.subscribe();
		let mut last = h.tick();
		assert!(last.lag.is_empty());

		// Never acknowledges anything: the frontier stands at the newest frame the
		// track held when it opened, and each second of media adds a second of lag.
		let mut groups = Vec::new();
		let mut buckets = Vec::new();
		for _ in 0..6 {
			let frames = h.produce();
			groups.push(Harness::write(&sub, &frames));
			let now = h.tick();
			let grew = grew(&last, &now);
			assert_eq!(grew.len(), 1, "one sample per tick: {grew:?}");
			assert_eq!(grew[0].1, GROUP_BYTES);
			buckets.push(grew[0].0);
			last = now;
		}
		// 1s, 2s, 3s, 4s, 5s, 6s behind.
		assert_eq!(buckets, vec![5, 6, 6, 6, 7, 7]);
	}

	#[test]
	fn slow_reader_backlog_grows_into_higher_buckets() {
		let mut h = Harness::new();
		let sub = h.subscribe();
		let mut last = h.tick();
		let mut pending = std::collections::VecDeque::new();
		let mut buckets = Vec::new();
		for tick in 0..8 {
			pending.push_back(Harness::write(&sub, &h.produce()));
			// Acknowledges one group every other tick: half the media rate.
			if tick % 2 == 1 {
				pending.pop_front().unwrap().acked();
			}
			let now = h.tick();
			buckets.push(grew(&last, &now)[0].0);
			last = now;
		}
		assert_eq!(buckets, vec![5, 5, 6, 6, 6, 6, 6, 6]);
	}

	#[test]
	fn the_report_clock_bounds_a_slow_timeline() {
		// The publisher's clock crawls: 10ms of media per second. Media time alone
		// says a viewer that never acknowledges is barely behind; each report
		// interval it produced through without an acknowledgement says otherwise.
		let mut h = Harness::new();
		let sub = h.subscribe();
		let mut stalled = Vec::new();
		let mut last = h.tick();
		let mut buckets = Vec::new();
		for i in 0..7u64 {
			let ts = Timestamp::from_millis(i * 10).unwrap();
			let mut group = h.track.append_group().unwrap();
			group.write_frame(ts, vec![0u8; 100]).unwrap();
			group.finish().unwrap();
			stalled.push(Harness::write(&sub, &[ts]));
			h.now += TICK;
			let now = h.tick();
			let grew = grew(&last, &now);
			assert_eq!(grew.len(), 1, "{grew:?}");
			buckets.push(grew[0].0);
			last = now;
		}
		// Stalled 1s .. 7s, against at most 60ms of media.
		assert_eq!(buckets, vec![5, 6, 6, 6, 7, 7, 7]);
	}

	#[test]
	fn timestamps_that_stop_advancing_do_not_hide_a_stalled_viewer() {
		let mut h = Harness::new();
		let sub = h.subscribe();
		let mut last = h.tick();
		for _ in 0..2 {
			let frames = h.produce();
			Harness::write(&sub, &frames).acked();
		}
		let now = h.tick();
		assert_eq!(grew(&last, &now), vec![(0, 2 * GROUP_BYTES)]);
		last = now;

		// The publisher's clock stops: each group repeats the last one's timestamps, an
		// equal start the track admits, so nothing is newer than what was acknowledged.
		// The viewer stops acknowledging. Media time reads the frontier as level with the
		// newest frame; the intervals produced through without an acknowledgement still
		// count.
		let mut stalled = Vec::new();
		let mut buckets = Vec::new();
		for _ in 0..3 {
			h.next = TICK;
			let frames = h.produce();
			stalled.push(Harness::write(&sub, &frames));
			let now = h.tick();
			buckets.push(grew(&last, &now)[0].0);
			last = now;
		}
		assert_eq!(buckets, vec![5, 6, 6]);
	}

	#[test]
	fn a_pause_adds_no_weight_and_no_lag() {
		let mut h = Harness::new();
		let sub = h.subscribe();
		Harness::write(&sub, &h.produce()).acked();
		let mut last = h.tick();

		// Nothing produced for a minute: the histogram does not move.
		for _ in 0..60 {
			h.now += TICK;
			let now = h.tick();
			assert_eq!(now.lag, last.lag);
			last = now;
		}

		// Resuming does not read the pause as a stalled frontier: the group still in
		// flight is one second of media behind, not the minute the source was away.
		let _in_flight = Harness::write(&sub, &h.produce());
		let now = h.tick();
		assert_eq!(grew(&last, &now), vec![(5, GROUP_BYTES)]);
	}

	#[test]
	fn a_subscription_opened_before_any_media_measures_from_the_first_frame() {
		let mut h = Harness::new();
		let sub = h.subscribe();
		let last = h.tick();
		let _stalled = Harness::write(&sub, &h.produce());
		h.produce();
		let now = h.tick();
		// Two seconds produced since it opened, none acknowledged: 1.96s from the
		// first frame to the newest, over two seconds stalled.
		assert_eq!(grew(&last, &now), vec![(6, 2 * GROUP_BYTES)]);
	}

	#[test]
	fn an_unacknowledged_group_counts_as_dropped() {
		let mut h = Harness::new();
		let sub = h.subscribe();
		let before = h.tick();

		let frames = h.produce();
		// Half the group written, then the stream is reset (skip, expiry, stop, or
		// session close all drop the serve task).
		drop(Harness::write(&sub, &frames[..15]));
		// Nothing written at all is not a dropped group.
		drop(sub.delivery());

		let after = h.tick();
		assert_eq!(after.dropped.groups, 1);
		assert_eq!(after.dropped.bytes, 15 * 100);
		assert_eq!(after.dropped.duration, Duration::from_millis(14 * 33));
		// The drop itself adds nothing to the histogram beyond the tick's own sample.
		assert_eq!(after.lag.total() - before.lag.total(), GROUP_BYTES);
	}

	#[test]
	fn a_partly_accepted_frame_drops_only_what_the_transport_took() {
		let mut h = Harness::new();
		let sub = h.subscribe();
		let frames = h.produce();
		let mut delivery = sub.delivery();
		delivery.frame(Some(frames[0]));
		delivery.wrote(40);
		delivery.frame(Some(frames[1]));
		drop(delivery);
		let dropped = h.tick().dropped;
		assert_eq!((dropped.groups, dropped.bytes), (1, 40));
	}

	/// The peer stamps the frames, so a dropped span can pass what the counter holds:
	/// the duration saturates rather than wrapping to read as a reset.
	#[test]
	fn a_dropped_span_past_the_counter_saturates() {
		let h = Harness::new();
		let sub = h.subscribe();
		let last = Timestamp::from_secs((1 << 61) - 1).unwrap();
		for _ in 0..2 {
			let mut delivery = sub.delivery();
			delivery.frame(Some(Timestamp::ZERO));
			delivery.wrote(1);
			delivery.frame(Some(last));
			delivery.wrote(1);
		}
		assert_eq!(h.tick().dropped.duration, Duration::from_nanos(u64::MAX));
	}

	#[test]
	fn a_frame_written_in_chunks_weighs_what_arrived() {
		let h = Harness::new();
		let _sub = h.subscribe();
		let last = h.tick();
		let group = h.track.append_group().unwrap();
		let mut frame = group
			.create_frame(crate::frame::Info {
				size: 1000,
				timestamp: Some(Timestamp::ZERO),
			})
			.unwrap();
		frame.write(vec![0u8; 400]).unwrap();
		let now = h.tick();
		assert_eq!(grew(&last, &now), vec![(0, 400)]);
	}

	/// A compressed track's frame weighs the slice it stores, whether written whole or
	/// streamed in chunks of plain payload.
	#[test]
	fn a_compressed_frame_weighs_its_stored_size_however_written() {
		let h = Harness::new();
		let info = track::Info::timed().with_compress(true);
		let track = h.broadcast.create_track("captions", info).unwrap();
		let _sub = (h.broadcast.consume().with_stats(h.session.egress(PATH)))
			.track("captions")
			.unwrap()
			.subscribe(None)
			.now_or_never()
			.expect("track is live")
			.expect("subscribed");
		let payload = vec![7u8; 4000];
		let mut last = h.tick();
		let mut weights = Vec::new();
		for (index, chunked) in [false, true].into_iter().enumerate() {
			let mut group = track.append_group().unwrap();
			let timestamp = Timestamp::from_millis(1000 * index as u64).unwrap();
			if chunked {
				let info = crate::frame::Info {
					size: payload.len() as u64,
					timestamp: Some(timestamp),
				};
				let mut frame = group.create_frame(info).unwrap();
				for chunk in payload.chunks(1000) {
					frame.write(chunk.to_vec()).unwrap();
				}
				group = frame.finish().unwrap();
			} else {
				group.write_frame(timestamp, payload.clone()).unwrap();
			}
			group.finish().unwrap();
			let now = h.tick();
			weights.push(now.lag.total() - last.lag.total());
			last = now;
		}
		assert!(weights[0] < payload.len() as u64, "the frame compressed: {weights:?}");
		assert_eq!(weights[0], weights[1]);
	}

	#[test]
	fn subscriptions_of_one_broadcast_sum_into_one_row() {
		let mut h = Harness::new();
		let other = h.stats.tier(Tier::default()).session("root");
		let a = h.subscribe();
		let b = h.subscribe_as(&other);
		let last = h.tick();
		let mut stalled = Vec::new();
		for _ in 0..2 {
			let frames = h.produce();
			Harness::write(&a, &frames).acked();
			stalled.push(Harness::write(&b, &frames));
		}
		h.produce();
		let now = h.tick();
		// `a` is one second behind, `b` three; both weigh the same three groups.
		assert_eq!(grew(&last, &now), vec![(5, 3 * GROUP_BYTES), (6, 3 * GROUP_BYTES)]);
	}

	#[test]
	fn a_subscription_closed_between_reports_is_sampled_once() {
		let mut h = Harness::new();
		let sub = h.subscribe();
		let last = h.tick();
		h.produce();
		drop(sub);

		// The report that follows finds no frontier, yet the group still lands, at the
		// 957ms it was behind when the guard dropped: the close charges no stall.
		let now = h.tick();
		assert_eq!(grew(&last, &now), vec![(4, GROUP_BYTES)]);

		// Sampled exactly once: the closing report pruned the row, and later media adds
		// nothing to the retired totals.
		h.produce();
		h.stats.report(&mut Report::default(), h.now);
		assert_eq!(recorded(&h.stats), GROUP_BYTES);
	}

	#[test]
	fn a_subscription_closed_mid_interval_adds_only_the_bytes_since_its_last_sample() {
		let mut h = Harness::new();
		let sub = h.subscribe();
		let mut last = h.tick();
		h.produce();
		last = {
			let now = h.tick();
			assert_eq!(grew(&last, &now), vec![(5, GROUP_BYTES)]);
			now
		};
		h.produce();
		h.produce();
		drop(sub);

		let now = h.tick();
		assert_eq!(
			grew(&last, &now).iter().map(|(_, bytes)| bytes).sum::<u64>(),
			2 * GROUP_BYTES
		);
	}

	#[test]
	fn an_in_flight_delivery_defers_the_final_sample_to_its_own_drop() {
		let mut h = Harness::new();
		let sub = h.subscribe();
		let delivery = Harness::write(&sub, &h.produce());
		drop(sub);
		// The delivery still holds the frontier, so it has not closed yet.
		assert_eq!(recorded(&h.stats), 0);

		drop(delivery);
		assert_eq!(recorded(&h.stats), GROUP_BYTES);
	}

	#[test]
	fn the_last_subscriber_samples_a_track_its_own_drop_releases() {
		let mut h = Harness::new();
		let sub = h.subscribe();
		h.produce();
		// The producer is gone, so the subscriber holds the last reference to the track.
		drop(h.track);
		drop(h.broadcast);

		drop(sub);
		assert_eq!(recorded(&h.stats), GROUP_BYTES);
	}

	#[test]
	fn an_in_flight_delivery_samples_a_track_that_already_closed() {
		let mut h = Harness::new();
		let sub = h.subscribe();
		let delivery = Harness::write(&sub, &h.produce());
		drop(h.track);
		drop(h.broadcast);
		drop(sub);
		assert_eq!(recorded(&h.stats), 0);

		// The track died with the subscriber; the delivery still owes its bytes.
		drop(delivery);
		assert_eq!(recorded(&h.stats), GROUP_BYTES);
	}

	#[test]
	fn a_closed_subscription_leaves_the_sampler() {
		let mut h = Harness::new();
		let sub = h.subscribe();
		h.tick();
		drop(sub);
		h.produce();
		let row = h.tick();
		assert!(row.lag.is_empty());
	}

	#[test]
	fn the_frontier_list_stays_bounded_without_reports() {
		let h = Harness::new();
		let scope = h.session.egress(PATH);
		for _ in 0..1000 {
			drop(scope.subscribe());
		}
		let counters = h.stats.shared().entries.lock()[&PathOwned::from(PATH.to_string())].tier(&Tier::default());
		let held = counters.publisher.frontiers.lock().unwrap().len();
		assert!(held <= 8, "{held} frontiers held with none live");
	}

	/// Two copies live at once, as a resumed subscription's old and new are until the
	/// old one drains, each weigh at their own edge: the one far ahead does not drag
	/// the other's bytes up the buckets.
	#[test]
	fn live_copies_weigh_at_their_own_edges() {
		let h = Harness::new();
		let subscription = h.session.egress(PATH).subscribe();
		let (near, far) = (Arc::new(Production::default()), Arc::new(Production::default()));
		let _watches = (
			subscription.frontier().watch(&near),
			subscription.frontier().watch(&far),
		);
		let at = |millis| Some(Timestamp::from_millis(millis).unwrap());
		near.record(at(0), 10);
		far.record(at(0), 10);
		let last = h.tick();
		near.record(at(100), 10);
		far.record(at(3000), 20);
		assert_eq!(grew(&last, &h.tick()), vec![(2, 10), (6, 20)]);
	}

	/// A route moved over and over between reports keeps only a few retired copies
	/// apart, and still weighs every byte they produced.
	#[test]
	fn retired_copies_stay_bounded_between_reports() {
		let h = Harness::new();
		let subscription = h.session.egress(PATH).subscribe();
		let frontier = subscription.frontier();
		for _ in 0..1000 {
			let copy = Arc::new(Production::default());
			let watch = frontier.watch(&copy);
			copy.record(Some(Timestamp::ZERO), 10);
			drop(watch);
		}
		let held = frontier.0.as_ref().expect("egress").state().folded.len();
		assert!(held <= MAX_FOLDED, "{held} retired copies held");
		assert_eq!(h.tick().lag.total(), 10_000);
	}

	#[test]
	fn lag_and_drops_round_trip_and_stay_off_the_wire_when_empty() {
		let empty = serde_json::to_string(&Traffic::default()).unwrap();
		assert!(!empty.contains("lag") && !empty.contains("dropped"), "{empty}");

		let mut traffic = Traffic::default();
		traffic.lag.buckets[0] = 7;
		traffic.lag.buckets[7] = 9;
		traffic.dropped.duration = Duration::from_micros(1500);
		traffic.dropped.bytes = 3;
		traffic.dropped.groups = 1;
		let json = serde_json::to_string(&traffic).unwrap();
		assert!(json.contains(r#""lag":{"50ms":7,"inf":9}"#), "{json}");
		assert!(
			json.contains(r#""dropped":{"duration":1.5,"bytes":3,"groups":1}"#),
			"{json}"
		);
		assert_eq!(serde_json::from_str::<Traffic>(&json).unwrap(), traffic);

		let mut sum = traffic;
		sum.add(traffic);
		assert_eq!(sum.lag.buckets(), &[14, 0, 0, 0, 0, 0, 0, 18]);
		assert_eq!(sum.dropped.duration, Duration::from_millis(3));
	}

	#[test]
	fn merging_peer_readouts_saturates() {
		let lag = Histogram {
			buckets: [u64::MAX; BUCKETS],
		};
		let dropped = Dropped {
			duration: Duration::MAX,
			bytes: u64::MAX,
			groups: u64::MAX,
		};
		let mut traffic = Traffic {
			lag,
			dropped,
			..Default::default()
		};
		traffic.add(traffic);
		assert_eq!((traffic.lag, traffic.dropped), (lag, dropped));
		assert_eq!(traffic.lag.total(), u64::MAX);
	}
}
