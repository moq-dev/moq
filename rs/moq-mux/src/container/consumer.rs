use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Poll, ready};

use kio::clock::Instant;
use moq_net::Timestamp;

use super::{Container, Frame, Read, TimestampRewind, time};

/// Media and group boundaries in delivery order.
pub(crate) enum Event {
	/// A media frame, with when the wire frame carrying it landed in this node's cache
	/// ([`moq_net::frame::Frame::arrived`]).
	Frame { frame: Frame, arrived: Instant },
	/// An endpoint marker. `opens` when no media precedes it in its group: a declared stop
	/// (a marker group or an audio terminal) rather than the previous frame's duration.
	FrameEnd { end: Timestamp, opens: bool },
	/// The group finished cleanly.
	GroupEnd,
	/// The group was lost upstream partway, so nothing more of it arrives. The gap it leaves is
	/// read once the next group resumes the track. `observed` is when the loss was read.
	Cut { observed: Instant },
}

/// Read items and exporter-only boundaries in delivery order.
pub(crate) enum Item {
	/// [`Read::Frame`], with when it landed as [`Event::Frame`] has.
	Frame {
		group: u64,
		frame: Frame,
		arrived: Instant,
	},
	/// [`Read::Gap`].
	Gap {
		timestamp: Timestamp,
		duration: Timestamp,
	},
	/// [`Read::End`].
	End {
		group: u64,
		end: Timestamp,
	},
	FrameEnd(Timestamp),
	GroupEnd,
	Cut {
		observed: Instant,
	},
}

/// Decode a moq-lite track into a stream of media [`Frame`]s in age-bounded
/// presentation order.
///
/// `Consumer` reads a [`moq_net::track::Subscriber`] through its
/// [`Ordered`](moq_net::track::Ordered) reader and decodes it with a [`Container`] format
/// implementation, typically [`catalog::hang::Container`](crate::catalog::hang::Container).
/// Yields decoded frames via [`poll_read`](Self::poll_read).
///
/// ## Ordering & age skipping
///
/// Groups can arrive on the wire out of order. The [`Ordered`](moq_net::track::Ordered) reader
/// hands them out by start and decides every missing one: it waits for a group the track still
/// owes until everything it could still present (bounded by where the next group begins) falls
/// [`Delay::max`] behind the newest content, and reports what it gives up as a gap. With the
/// default max delay of zero it gives up at once: any group that has a newer alternative is
/// dropped. [`Delay::min`] only sets where the reader starts. A run the publisher declared
/// skipped ([`skip_groups`](moq_net::track::Producer::skip_groups), which appending does) holds no
/// group, and the consumer steps over it at once, as it does over keys the subscription's ranges
/// leave out.
///
/// Within a group the consumer reads frames in arrival order. A group already handed out that
/// stalls behind newer ones is skipped by the same budget, measured from where the next group
/// begins.
///
/// Delivery starts at the first group the ordered reader hands out: the publisher resolved a
/// live join [`Delay::min`] plus any [`Playout::warmup`] behind the live edge, and the ordered
/// reader waits for the groups below it that the feed owes. Until the first item, the start also
/// moves past each group whose first frame trails the newest content by more than the max delay
/// plus the warmup, as `@moq/hang`'s consumer measures, unless that group covers the live edge
/// less the min delay and the warmup: a backlog that landed whole would otherwise play from its
/// oldest group, since nothing blocks, and stay that far behind. From there the same budget is
/// the one rule that catches the consumer up to the live edge.
///
/// A stalled group is also skipped early, regardless of the max delay budget, once it has
/// presented up to where the next group begins. CMAF frames carry a per-sample duration,
/// so a group whose most recent frame ends (timestamp + duration) at or past the next
/// group's first timestamp has nothing left worth waiting for. Containers without a
/// duration report zero, which disables this check and falls back to the max delay budget.
///
/// Put the initial [`Delay`] on the [`moq_net::track::Subscription`] with
/// [`Delay::subscription`] before subscribing. [`new`](Self::new) inherits it, taking the min
/// from the furthest live join among its ranges and reading none as zero, and
/// [`set_delay`](Self::set_delay) changes the whole [`Delay`] while reading. To build one at a
/// delay the subscription does not carry, or with a catalog `warmup`, use a [`Playout`].
///
/// ## Gaps and ends
///
/// [`poll_read`](Self::poll_read) yields [`Read::Gap`] wherever media after an item already read
/// will never arrive: a latency skip, an evicted current group, or keys between two groups the
/// publisher did not declare skipped whose boundary is not contiguous within 1 ms. The later group
/// continues the earlier when any of its frames presents by where the earlier ended, so an open
/// GOP's leading pictures count, and its first frame waits until one shows it, at most as many frames
/// as the track has reordered. Frames without a duration end one picture past the highest: the
/// largest step between the group's latest presentation times. A local track
/// owes nothing, so there a discontinuous run is a gap the moment the group past it lands. Whatever is lost before the first item is no
/// gap, since nothing was read before it. A marker group (no decodable frames: one empty
/// payload, or on a data track one object with no bytes) is the publisher declaring a stop,
/// read as [`Read::End`]; nothing is missing. A data track's marker carries no timestamp, so
/// it ends at the furthest point read, and reads as nothing before the first item.
/// Empty groups (zero objects) mean nothing. Errors come in delivery order: a payload that fails
/// to decode, or a group below an earlier group's start, ends the reader where it sits, and a
/// failed track once the groups already here drain. A skip returns the error of a malformed group
/// it already read rather than a gap; a group dropped unread is not validated, since decoding data
/// that is thrown away is work a peer could force. After an error the reader reads as finished.
///
/// ## Memory
///
/// Decoded frames waiting to be read are charged to a byte cap, one group at the transport's size cap,
/// counting the whole wire payload they share, along with each buffered frame's and group's own
/// overhead. Once it is full the consumer stops
/// decoding ahead and stops pulling new groups past one successor to the cursor's group, so the
/// undecoded rest stays in the transport's cache, which bounds it. The group the cursor is on still decodes a wire frame whenever it holds nothing, so
/// reading never waits on its own lookahead. If that group stalls while the cap is full, the cursor
/// moves on to the next group like a skip past a slow group, read or not: the consumer can hold no
/// more of what it would wait behind.
pub struct Consumer<F: Container> {
	reader: Reader<F::Error>,
	name: std::sync::Arc<str>,

	format: F,

	// The current group that we want to read from
	current: u64,

	// Groups that we are monitoring, sorted by sequence ascending.
	pending: VecDeque<GroupBuffer<F>>,

	// The last group whose input finished, so watching EOF never rescans that prefix.
	received: Option<u64>,

	// Reap unread evictions only after metadata doubles, charging each sweep to new handles.
	collect_at: usize,

	// What the pending groups hold, against the max buffer.
	memory: Memory,

	// The last pull stopped at a full max buffer rather than at the track, so nothing wakes the reader
	// once a skip refunds room: it pulls again itself.
	gated: bool,

	// How the cursor chooses its first group.
	startup: Startup,

	// How far behind the live edge the reader plays, before the warmup: see `Delay::min`. The start
	// skip keeps the group covering the live edge less it, reading `None` as zero.
	min_delay: Option<std::time::Duration>,

	// How much further than the min delay the start reaches: see `Playout`.
	warmup: std::time::Duration,

	// Where the warming ends: the warmup past the first frame delivered. `None` before it.
	warmed: Option<std::time::Duration>,

	// How far we may drift from the live edge before skipping a group.
	max_delay: std::time::Duration,

	// The furthest presentation point delivered so far: max(timestamp + duration), endpoints
	// included. `None` until the first item, so a loss before it is no gap.
	edge: Option<std::time::Duration>,

	// The first frame delivered from the latest group, and that group. Group starts never go
	// backwards, so a later group with a frame below this is malformed.
	start: Option<(u64, Timestamp)>,

	// The start of the group the cursor most recently left. A frame below it is a rewind. Every
	// frame also presents after that group's content, which a reader assumes rather than checks.
	floor: Option<Timestamp>,

	// Presentation end of the group we most recently advanced past, for the 1 ms
	// contiguity check on a delivered hole.
	presented_end: Option<std::time::Duration>,

	// How the groups read so far run, for judging an unproven step.
	cadence: Cadence,

	// The cursor's group's frames read so far.
	pictures: Pictures,

	// Media went missing since the last item read, so the next one is preceded by a gap.
	gap: bool,

	// The cursor stepped over keys nothing declared skipped, so whether they held media is judged
	// once the group it landed on shows where it starts: its first frame is held until then.
	unproven: bool,

	// The item a gap was returned ahead of.
	next: Option<Item>,

	// The group the last event came from.
	group: u64,

	// When the wire frame behind the last frame read landed in this node's cache.
	arrived: Option<Instant>,

	// The newest group the publisher's largest named so far.
	largest: Option<u64>,

	// The publisher's largest group as the start was decided: one at or below it was produced
	// before the subscription, so dropping it unread skips the join's backlog.
	joined: Option<u64>,

	// When the latest backlog group dropped without delivering all of it landed.
	backlog: Option<Instant>,
}

/// Whether a [`Consumer`] still reads its subscription.
enum Reader<E> {
	/// Receiving groups.
	Reading(moq_net::track::Ordered),
	/// The track failed, so no group can arrive. The error is returned once the groups already here
	/// drain.
	Failed(moq_net::track::Ordered, E),
	/// An error ended the reader, which reads as finished from then on. The subscription is
	/// released, and the publisher's demand with it.
	Ended,
	/// The track's own failure ended the reader once its groups drained, as [`Self::Ended`] does.
	Lost,
}

/// How a [`Consumer`]'s cursor chooses its first group.
#[derive(Clone, Copy)]
enum Startup {
	/// The first group the ordered reader hands out.
	Earliest,
	/// The newest group, discarding what arrived below it.
	Newest,
	/// The cursor is on its first group or past it.
	Started,
}

/// Two adjacent groups are timeline-contiguous when the next start is within this slack
/// of the current end. Per-sample durations and base-decode-times round to microseconds
/// independently, so a genuine boundary can be off by ~1 µs; a real missing group spans
/// about one group duration.
const CONTIGUITY_TOLERANCE: std::time::Duration = std::time::Duration::from_millis(1);

/// The default decoded-byte cap: one group at the transport's size cap.
pub(crate) const DEFAULT_MAX_BUFFER: usize = 32 * 1024 * 1024;

/// The decoded bytes a consumer's groups hold, shared with each group so one dropped by any path
/// refunds what it held.
#[derive(Clone)]
struct Memory(Arc<MemoryState>);

struct MemoryState {
	used: AtomicUsize,
	max: AtomicUsize,
}

impl Memory {
	fn new(max: usize) -> Self {
		Self(Arc::new(MemoryState {
			used: AtomicUsize::new(0),
			max: AtomicUsize::new(max),
		}))
	}

	fn used(&self) -> usize {
		self.0.used.load(Ordering::Relaxed)
	}

	fn full(&self) -> bool {
		self.used() >= self.0.max.load(Ordering::Relaxed)
	}

	#[cfg(test)]
	fn set_max(&self, max: usize) {
		self.0.max.store(max, Ordering::Relaxed);
	}

	fn charge(&self, bytes: usize) {
		self.0.used.fetch_add(bytes, Ordering::Relaxed);
	}

	fn refund(&self, bytes: usize) {
		self.0.used.fetch_sub(bytes, Ordering::Relaxed);
	}
}

fn pts_contiguous(end: Option<std::time::Duration>, next_start: std::time::Duration) -> bool {
	end.is_some_and(|end| next_start <= end.saturating_add(CONTIGUITY_TOLERANCE))
}

/// The deepest reordering a codec allows, in pictures: H.264 and HEVC hold at most 16 frames, so 32
/// fields. A reader measures a track's reordering and its last picture's duration within this many of
/// a group's latest presentation times.
const REORDER_MAX: usize = 32;

/// How the groups read so far run, for judging a step over keys nothing declared skipped.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Cadence {
	/// One picture, for a group whose frames carry no duration: its highest timestamp is where its
	/// last picture starts, not ends. The largest step between its latest presentation times, so a
	/// field/frame mix allows a frame. Zero once a group's end was exact.
	slack: std::time::Duration,
	/// The most frames that decoded ahead of a frame they present after, in any group read: an open
	/// GOP's leading pictures decode after the keyframe, so its lowest picture comes within that many.
	depth: usize,
}

/// Where the groups a reader read through ended, so a later reader of the track continues there.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Reached {
	end: Option<std::time::Duration>,
	cadence: Cadence,
}

/// The cursor's group's frames read so far.
#[derive(Default)]
struct Pictures {
	/// The group's latest presentation times, ascending, at most [`REORDER_MAX`].
	latest: Vec<std::time::Duration>,
}

impl Pictures {
	/// Note a frame presenting `at`, returning how many frames read before it present after it.
	fn read(&mut self, at: std::time::Duration) -> usize {
		let below = self.latest.partition_point(|&t| t <= at);
		let after = self.latest.len() - below;
		match self.latest.len() < REORDER_MAX {
			true => self.latest.insert(below, at),
			// Full: the earliest leaves, unless this frame presents before every time kept.
			false if below > 0 => {
				self.latest.remove(0);
				self.latest.insert(below - 1, at);
			}
			false => {}
		}
		after
	}

	/// The largest step between the latest presentation times read, zero for fewer than two.
	fn step(&self) -> std::time::Duration {
		(self.latest.windows(2))
			.map(|pair| pair[1] - pair[0])
			.max()
			.unwrap_or_default()
	}

	/// The highest presentation time read.
	fn highest(&self) -> Option<std::time::Duration> {
		self.latest.last().copied()
	}
}

/// A gap from the delivered `edge` to `resume`, in the resume's scale. Refused for an unscaled
/// `resume`, which has no time to resume at.
fn gap(edge: std::time::Duration, resume: Timestamp) -> Result<Item, moq_net::Error> {
	let scale = resume.scale().ok_or(moq_net::Error::TimestampMismatch)?;
	// A next group may start before the edge (an overlapping endpoint), so the span bottoms out at zero.
	let start = edge
		.as_nanos()
		.saturating_mul(scale.as_u64() as u128)
		.div_euclid(1_000_000_000)
		.min(resume.value() as u128) as u64;
	let timestamp = Timestamp::new(start, scale).expect("at or below a valid timestamp");
	let duration = resume
		.checked_sub(timestamp)
		.expect("at or below the resume, in its scale");
	Ok(Item::Gap { timestamp, duration })
}

/// How far behind the live edge a media reader plays: where it starts, and how late it may fall.
///
/// The default states no minimum and a zero maximum, which reaches back as far as the max allows
/// and skips a superseded group at once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Delay {
	/// The playout delay held behind the live edge, or `None` when the reader did not state one.
	///
	/// Each subscription range starting at [`Start::Oldest`](moq_net::track::Start::Oldest)
	/// joins live this far behind the live edge instead, as a
	/// [`Start::Live`](moq_net::track::Start::Live) `min`, so a player's buffer arrives full; a
	/// range with a time start keeps it. Unstated, the join reaches back as far as [`Self::max`]
	/// allows. A reader also keeps the group covering the live edge less this as it starts,
	/// reading an unstated one as zero. A missing group is given up by [`Self::max`] alone,
	/// whatever the minimum.
	pub min: Option<std::time::Duration>,
	/// How long a superseded group stays worth delivering, or `None` for as long as the publisher
	/// keeps it: each live join's [`Start::Live`](moq_net::track::Start::Live) `max`. With no
	/// [`Self::min`] either, a range starting at the oldest group stays history, delivered in full.
	pub max: Option<std::time::Duration>,
}

impl Default for Delay {
	fn default() -> Self {
		Self {
			min: None,
			max: Some(std::time::Duration::ZERO),
		}
	}
}

impl Delay {
	/// Set [`Self::min`], returning `self` for chaining.
	pub fn with_min(mut self, min: impl Into<Option<std::time::Duration>>) -> Self {
		self.min = min.into();
		self
	}

	/// Set [`Self::max`], returning `self` for chaining.
	pub fn with_max(mut self, max: impl Into<Option<std::time::Duration>>) -> Self {
		self.max = max.into();
		self
	}

	/// `subscription` at this delay: each range starting at the oldest group joins live
	/// [`Self::min`] behind the live edge, or [`Self::max`] when only that is stated, and each
	/// live join takes [`Self::max`] as its own. A time start stays history.
	///
	/// [`Subscription::default`](moq_net::track::Subscription::default) is already a live join,
	/// at the latest group, so a range this delay should place starts at
	/// [`Range::ALL`](moq_net::track::Range::ALL) instead.
	pub fn subscription(&self, subscription: moq_net::track::Subscription) -> moq_net::track::Subscription {
		use moq_net::track::{Range, Start};
		let ranges: Vec<Range> = subscription
			.ranges
			.iter()
			.map(|range| {
				let start = match (range.start, self.min.or(self.max)) {
					(Start::Oldest, Some(min)) => Start::Live {
						min: Some(min),
						max: self.max,
					},
					(Start::Live { min, .. }, _) => Start::Live { min, max: self.max },
					(start, _) => start,
				};
				Range { start, ..*range }
			})
			.collect();
		subscription.with_ranges(ranges)
	}

	/// [`Self::subscription`] as an update to `current`, the subscription it replaces: a range
	/// starting at the oldest group that `current` already joins live with the same end keeps
	/// that join's `min` and takes the new [`Self::max`], so changing [`Self::min`] places only
	/// the ranges it adds. A live join that already resolved stays where it started, as
	/// [`Consumer::set_delay`] leaves it.
	pub fn update(
		&self,
		current: &moq_net::track::Subscription,
		subscription: moq_net::track::Subscription,
	) -> moq_net::track::Subscription {
		use moq_net::track::Start;
		let starts: Vec<_> = subscription
			.ranges
			.iter()
			.map(|range| (range.start, range.end))
			.collect();
		let mut updated = self.subscription(subscription);
		for (range, (start, end)) in updated.ranges.iter_mut().zip(starts) {
			if start != Start::Oldest {
				continue;
			}
			// For a caller whose own ranges start at the oldest group or a time, as the bindings'
			// do, a live join in `current` is one a delay placed.
			let held = current.ranges.iter().find_map(|held| match held.start {
				Start::Live { min, .. } if held.end == end => Some(min),
				_ => None,
			});
			if let Some(min) = held {
				range.start = Start::Live { min, max: self.max };
			}
		}
		updated
	}
}

/// The playout delay `subscription` states: the furthest back any of its ranges joins live, or
/// `None` when none does. A join with no lag limit states none.
fn stated(subscription: &moq_net::track::Subscription) -> Option<std::time::Duration> {
	subscription
		.ranges
		.iter()
		.filter_map(|range| match range.start {
			moq_net::track::Start::Live { min, .. } => min,
			_ => None,
		})
		.max()
}

/// The longest any range of `subscription` keeps a superseded group: `None` while a history range
/// is asked for or a live join has no `max`.
fn widest(subscription: &moq_net::track::Subscription) -> Option<std::time::Duration> {
	let mut widest = Some(std::time::Duration::ZERO);
	for range in &subscription.ranges {
		let max = match range.start {
			moq_net::track::Start::Live { max, .. } => max,
			moq_net::track::Start::Oldest | moq_net::track::Start::Time(_) => None,
		};
		widest = widest.zip(max).map(|(widest, max)| widest.max(max));
	}
	widest
}

/// A [`Consumer`]'s [`Delay`] and the catalog rendition's warmup, applied together.
///
/// The warmup is how much media a decoder needs before its output is clean, such as an Opus
/// decoder's pre-roll: pass the catalog audio rendition's `warmup`. Subscribe with
/// [`subscription`](Self::subscription), then build the consumer with [`consumer`](Self::consumer).
///
/// The subscription carries the min and max delay plus the warmup: the publisher starts a live
/// join that far back, and it and the transport skip what falls outside that budget, while the
/// consumer skips at the max delay alone.
///
/// A live join starts at the group covering the live edge less [`Delay::min`] and the warmup,
/// which the publisher resolves, and the ordered reader waits for the groups from there. Until the
/// reader is the warmup past its first frame, the skip also tolerates the warmup, so the media
/// leading up to the start is not dropped as stale.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Playout {
	/// The delay the consumer plays at, and skips past.
	pub delay: Delay,
	/// How much further back than [`Delay::min`] the start reaches, at most 10 s.
	pub warmup: std::time::Duration,
}

/// The longest playout delay: it caps a reader's latency and how far catalog `warmup` reaches back,
/// so a publisher cannot hold a reader's media without bound. Mirrored by `@moq/hang`'s `DELAY_MAX`.
pub const DELAY_MAX: std::time::Duration = std::time::Duration::from_secs(10);

/// The delay a subscription carries for a reader starting `warmup` further back than `delay`:
/// both bounds widened, so a live join reaches back far enough and the budget keeps what it reaches.
fn warmed(delay: Delay, warmup: std::time::Duration) -> Delay {
	Delay {
		// An unstated minimum already reaches back as far as the max delay allows, further than
		// any live join, so it stays unstated.
		min: delay.min.map(|min| min.saturating_add(warmup)),
		max: delay.max.map(|max| max.saturating_add(warmup)),
	}
}

impl Playout {
	/// Play at `delay`, starting `warmup` further back.
	pub fn new(delay: Delay, warmup: std::time::Duration) -> Self {
		Self { delay, warmup }
	}

	/// `subscription` with this playout's delay, both bounds widened by the warmup: see
	/// [`Delay::subscription`].
	pub fn subscription(&self, subscription: moq_net::track::Subscription) -> moq_net::track::Subscription {
		self.subscribed().subscription(subscription)
	}

	/// Decode `track` as `format` at this playout.
	///
	/// Subscribe `track` with [`subscription`](Self::subscription): the publisher may already
	/// have skipped what a narrower one left out, and the live join starts where it was
	/// subscribed. A track subscribed otherwise has its max delay set here, widened or narrowed.
	pub fn consumer<F: Container>(&self, track: moq_net::track::Subscriber, format: F) -> Consumer<F> {
		let mut consumer = Consumer::new(track, format);
		consumer.warmup = self.reach();
		consumer.set_delay(self.delay);
		consumer
	}

	/// The delay the subscription carries.
	fn subscribed(&self) -> Delay {
		warmed(self.delay, self.reach())
	}

	/// The warmup, capped at [`DELAY_MAX`].
	fn reach(&self) -> std::time::Duration {
		self.warmup.min(DELAY_MAX)
	}
}

impl<F: Container> Consumer<F> {
	/// Create a Consumer wrapping the given moq-lite consumer, decoding `format`.
	///
	/// The ordering window inherits the widest live join `max` the subscriber asks, none while
	/// it asks for history, clamped to the track's max age. Put that budget on the
	/// [`moq_net::track::Subscription`] before awaiting the subscription, so the
	/// publisher preserves the same replay window.
	pub fn new(track: moq_net::track::Subscriber, format: F) -> Self {
		let subscription = track.subscription();
		// The budget is clamped to the track's max age, since a group the publisher already marks
		// stale can't be waited for and would otherwise stall the catch-up by the excess.
		let max_delay = widest(&subscription)
			.unwrap_or(std::time::Duration::MAX)
			.min(track.info().max_age.unwrap_or(std::time::Duration::MAX));
		Self {
			name: track.name().into(),
			reader: Reader::Reading(track.ordered()),
			format,
			current: 0,
			pending: VecDeque::new(),
			received: None,
			collect_at: 1,
			memory: Memory::new(DEFAULT_MAX_BUFFER),
			gated: false,
			startup: Startup::Earliest,
			min_delay: stated(&subscription),
			warmup: std::time::Duration::ZERO,
			warmed: None,
			max_delay,
			edge: None,
			start: None,
			floor: None,
			presented_end: None,
			cadence: Cadence::default(),
			pictures: Pictures::default(),
			gap: false,
			unproven: false,
			next: None,
			group: 0,
			arrived: None,
			largest: None,
			joined: None,
			backlog: None,
		}
	}

	/// Continue groups an earlier reader of this track read up to `reached`: the first group
	/// starting there reads as continuing them rather than past a hole.
	pub(crate) fn continuing(mut self, reached: Reached) -> Self {
		self.presented_end = reached.end;
		self.cadence = reached.cadence;
		self
	}

	/// Where the groups read through ended, for [`Self::continuing`].
	pub(crate) fn reached(&self) -> Reached {
		Reached {
			end: self.presented_end,
			cadence: self.cadence,
		}
	}

	/// Start at the newest group, the live edge, rather than the floor or the oldest group the
	/// publisher serves: the publisher's largest when it reports one past what has arrived. The
	/// subscription keeps its budget, so every group after the start arrives as it would for any
	/// reader; what arrived below the start is discarded.
	pub(crate) fn live(mut self) -> Self {
		self.startup = Startup::Newest;
		self
	}

	/// When the wire frame behind the latest frame read landed in this node's cache
	/// ([`moq_net::frame::Frame::arrived`]), or `None` before the first frame.
	///
	/// A gap or an end leaves it at the frame before. A frame from cache, such as a join on a
	/// track that already holds groups, reports when it landed, not when it was read. Frames
	/// packed into one wire frame, as a CMAF fragment packs its samples, share its arrival.
	pub fn arrival(&self) -> Option<Instant> {
		self.arrived
	}

	/// When the latest backlog group this reader dropped without delivering all of it landed in
	/// this node's cache, or `None` before the first.
	///
	/// Backlog is a group at or below the publisher's largest when the subscription began, so it
	/// was produced before. Its frames were never read, so a caller timing
	/// [`arrival`](Self::arrival)s learns when a join's backlog finished arriving even when the
	/// reader skipped it: one the ordered reader gave up as stale or late (see
	/// [`Item::Gap`](moq_net::track::Item::Gap)'s `landed`), or one a skip here passed over. A gap
	/// reaching past the join counts for none of it, since its landing may be a later group's.
	pub fn backlog(&self) -> Option<Instant> {
		self.backlog
	}

	/// The group sequence of the last event read.
	pub(crate) fn group(&self) -> u64 {
		self.group
	}

	/// The track's name.
	pub(crate) fn name(&self) -> &str {
		&self.name
	}

	/// Read the next frame from the track.
	///
	/// This method handles timestamp decoding, group ordering, and age management
	/// automatically. It will skip groups that are too far behind to maintain the
	/// configured max delay.
	///
	/// Returns `None` when the track has ended, and after an error. See [`Read`] for the items between frames;
	/// an undeclared forward timestamp jump is not a gap, since nothing says media is missing.
	#[cfg_attr(feature = "async", kio::wrapper)]
	pub fn poll_read(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Read>, F::Error>> {
		loop {
			let read = match ready!(self.poll_item(waiter))? {
				Some(Item::Frame { group, frame, arrived }) => {
					self.arrived = Some(arrived);
					Read::Frame { group, frame }
				}
				Some(Item::Gap { timestamp, duration }) => Read::Gap { timestamp, duration },
				Some(Item::End { group, end }) => Read::End { group, end },
				Some(Item::FrameEnd(_) | Item::GroupEnd | Item::Cut { .. }) => continue,
				None => return Poll::Ready(Ok(None)),
			};
			return Poll::Ready(Ok(Some(read)));
		}
	}

	/// The next frame, passing over gaps and ends.
	#[cfg(test)]
	pub(crate) async fn read_frame(&mut self) -> Result<Option<Frame>, F::Error> {
		loop {
			match self.read().await? {
				Some(Read::Frame { frame, .. }) => return Ok(Some(frame)),
				Some(Read::Gap { .. } | Read::End { .. }) => continue,
				None => return Ok(None),
			}
		}
	}

	/// Read items while preserving boundaries needed to finish exporter fragments.
	pub(crate) fn poll_item(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Item>, F::Error>> {
		if let Some(item) = self.next.take() {
			return Poll::Ready(Ok(Some(item)));
		}
		let edge = self.edge;
		let event = ready!(self.poll_event(waiter))?;
		let (item, resume) = match event {
			Some(Event::Frame { frame, arrived }) => {
				let resume = frame.timestamp;
				let group = self.group;
				(Item::Frame { group, frame, arrived }, resume)
			}
			Some(Event::FrameEnd { end, opens: true }) => (Item::End { group: self.group, end }, end),
			Some(Event::FrameEnd { end, opens: false }) => return Poll::Ready(Ok(Some(Item::FrameEnd(end)))),
			Some(Event::GroupEnd) => return Poll::Ready(Ok(Some(Item::GroupEnd))),
			Some(Event::Cut { observed }) => return Poll::Ready(Ok(Some(Item::Cut { observed }))),
			None => return Poll::Ready(Ok(None)),
		};
		if std::mem::take(&mut self.gap)
			&& let Some(edge) = edge
		{
			self.next = Some(item);
			return Poll::Ready(Ok(Some(gap(edge, resume)?)));
		}
		Poll::Ready(Ok(Some(item)))
	}

	/// Whether the error that ended the reader was its track's own failure, rather than its media.
	pub(crate) fn lost(&self) -> bool {
		matches!(self.reader, Reader::Lost)
	}

	/// Read media or a clean group boundary without waiting for a successor group.
	pub(crate) fn poll_event(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Event>, F::Error>> {
		// An error ended the reader, which reads as finished from then on: a decode error has
		// already consumed the frame it came from, so reading on would skip it.
		if let Reader::Ended | Reader::Lost = self.reader {
			return Poll::Ready(Ok(None));
		}
		let event = ready!(self.poll_event_inner(waiter));
		if event.is_err() {
			// Nothing more is read, so release the subscription and the groups it holds rather
			// than pin the publisher's demand and cache for as long as the caller keeps this.
			if !matches!(self.reader, Reader::Lost) {
				self.reader = Reader::Ended;
			}
			self.pending.clear();
		}
		Poll::Ready(event)
	}

	fn poll_event_inner(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Event>, F::Error>> {
		loop {
			match self.poll_event_once(waiter) {
				// A skip refunded room for groups the max buffer held back, so pull them.
				Poll::Pending if std::mem::take(&mut self.gated) && !self.pull_gated() => continue,
				res => return res,
			}
		}
	}

	fn poll_event_once(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Event>, F::Error>> {
		// Where the publisher stood as the subscription began, read as soon as its answer reports a
		// largest, before a group the reader waits on lets later ones raise it. A largest not yet in
		// range takes the first one that is.
		if self.joined.is_none() {
			self.join(waiter);
		}
		// Grab any new groups from the track, recording whether the track is finished.
		let finished = self.poll_read_finish(waiter).is_ready();

		match self.startup {
			// The ordered reader already waited for the groups below its first that the feed owes.
			Startup::Earliest => {
				if let Some(front) = self.pending.front() {
					self.current = front.start;
					self.join(waiter);
					self.startup = Startup::Started;
				}
			}
			Startup::Newest => {
				// NOTE: poll_min_timestamp buffers at least one frame per group and registers the
				// waiter on the ones still empty. Any wire frame counts, so a paused track whose only
				// group is a marker still reads its end. The oldest buffers past a full max buffer, so
				// a start is always found.
				let any_frame = self.pending.iter_mut().enumerate().any(|(i, group)| {
					let _ = group.poll_min_timestamp(waiter, &self.format, i == 0);
					!group.is_empty()
				});
				if any_frame {
					// A live reader takes the newest group, though its frames may still be on their
					// way, and discards the older ones the budget also delivers. The publisher's
					// largest names it before its stream lands.
					self.poll_largest(waiter);
					let back = self.pending.back().expect("a group has a frame").start;
					let start = back.max(self.largest.unwrap_or(back));
					self.join(waiter);
					// Nothing has been read, so discarding what lies below the start is no gap.
					self.discard(|group| group.start < start)?;
					self.current = start;
					self.startup = Startup::Started;
				}
			}
			Startup::Started => {}
		}

		loop {
			// Until the first item, a backlog that landed whole starts within the budget: nothing
			// blocks, so the skip below would play it from its oldest group and stay that far behind
			// for good. The start moves past each group whose first frame trails the newest content
			// by more than the budget, as `@moq/hang`'s consumer measures, unless that group covers
			// the live edge less the reach (see `Playout`), which the start promises.
			if self.edge.is_none() && self.start_skip(waiter)? {
				continue;
			}

			// Keys stepped over below the cursor's group lost media unless the group continues where
			// the content read so far ended, which may take frames after its first to show: an open
			// GOP's leading pictures decode after its keyframe but present before it. The group holds
			// its first frame until it shows, so a gap is still read ahead of it.
			let mut held = false;
			if self.unproven
				&& let Some(group) = self.pending.front_mut()
				&& group.start <= self.current
			{
				// Before the first item nothing was read to lose, so there is nothing to wait for.
				let bound = self.edge.and(self.presented_end).map(|end| end + self.cadence.slack);
				match group.poll_continues(waiter, &self.format, bound, self.cadence.depth) {
					Poll::Ready(Some(true)) => self.unproven = false,
					Poll::Ready(Some(false)) => self.lose(),
					// An endpoint, the group's end, or its failure comes first and is judged as it is read.
					Poll::Ready(None) => {}
					Poll::Pending => held = true,
				}
			}

			// Return the next frame from the current group if possible.
			// If the current group is finished or errored, advance to the next group.
			if !held
				&& let Some(group) = self.pending.front_mut()
				&& group.start <= self.current
			{
				match group.poll_read(waiter, &self.format) {
					Poll::Ready(Ok(Some(Event::Frame { frame, arrived }))) => {
						let seq = group.group.start;
						let ts = frame.timestamp;
						let first = time(ts).ok_or(moq_net::Error::TimestampMismatch)?;
						if let Some(floor) = self.floor
							&& time(floor).is_some_and(|floor| first.as_micros() < floor.as_micros())
						{
							return Poll::Ready(Err(TimestampRewind { timestamp: ts, floor }.into()));
						}
						if self.start.is_none_or(|(start, _)| start != seq) {
							self.start = Some((seq, ts));
							self.pictures.latest.clear();
						}
						let reordered = self.pictures.read(first);
						self.cadence.depth = self.cadence.depth.max(reordered);
						self.warmed.get_or_insert_with(|| first.saturating_add(self.warmup));
						let end = first + frame.duration.and_then(time).unwrap_or_default();
						self.advance(end);
						self.group = seq;
						return Poll::Ready(Ok(Some(Event::Frame { frame, arrived })));
					}
					Poll::Ready(Ok(Some(Event::FrameEnd { end, opens }))) => {
						self.group = group.group.start;
						let at = time(end).ok_or(moq_net::Error::TimestampMismatch)?;
						let bound = self.presented_end.map(|end| end + self.cadence.slack);
						if std::mem::take(&mut self.unproven) && !pts_contiguous(bound, at) {
							self.lose();
						}
						self.advance(at);
						return Poll::Ready(Ok(Some(Event::FrameEnd { end, opens })));
					}
					Poll::Ready(Ok(Some(event))) => return Poll::Ready(Ok(Some(event))),
					// Still blocked on this group, don't skip it yet.
					Poll::Pending => {}
					// A decode error is real and the caller must see it, not have the group
					// silently dropped.
					Poll::Ready(Err(Failure::Decode(e))) => {
						tracing::warn!(
							track = &*self.name,
							group = group.group.start,
							error = ?e,
							"group payload failed to decode; ending the reader"
						);
						return Poll::Ready(Err(e));
					}
					Poll::Ready(Err(Failure::Lost(e))) => {
						// The group aged out of the relay cache (`Error::Old`) or was otherwise
						// aborted. Any sequences between it and the next buffered group were
						// evicted alongside it, so jump straight to that group instead of
						// stepping one-by-one and then blocking on a sequence gap of groups
						// that will never arrive.
						tracing::warn!(
							track = &*self.name,
							group = group.group.start,
							error = ?e,
							"current group evicted; skipping to next buffered group"
						);
						// Whatever the group still held is lost, so this is a skip like any other.
						self.lose();
						let current = self.current;
						self.discard(|group| group.start <= current)?;
						self.current = self.pending.front().map_or(self.current + 1, |g| g.start);
						// Said now rather than with the gap, which waits for the next group: what
						// the caller holds of this one need not wait for a successor.
						return Poll::Ready(Ok(Some(Event::Cut {
							observed: kio::clock::now(),
						})));
					}
					// Cleanly finished group: advance past its key.
					Poll::Ready(Ok(None)) => {
						// A data track's marker carries no timestamp, so it ends where this reader
						// got to. Before the first item there is nothing to end.
						if let Content::Bare { ended } = &mut group.content
							&& !std::mem::replace(ended, true)
							&& let Some(edge) = self.edge
						{
							let end = u64::try_from(edge.as_micros())
								.ok()
								.and_then(|micros| Timestamp::from_micros(micros).ok())
								.ok_or(moq_net::Error::Decode(moq_net::DecodeError::BoundsExceeded))?;
							self.group = group.group.start;
							// It has no timestamp to prove the keys stepped over below it held nothing.
							if std::mem::take(&mut self.unproven) {
								self.lose();
							}
							return Poll::Ready(Ok(Some(Event::FrameEnd { end, opens: true })));
						}
						// A group its producer finished whole presents up to its end, which can pass
						// what its frames reach when they carry no duration.
						let whole = group
							.group
							.end()
							.zip(group.group.timescale())
							.and_then(|(end, scale)| Timestamp::new(end, scale).ok())
							.and_then(time);
						if let Some(end) = group.max_end.max(whole) {
							self.presented_end = Some(end);
							// Frames it carried say where its last picture ends: exactly when a duration
							// or its end reaches past them, otherwise one picture past the highest.
							if self.start.is_some_and(|(seq, _)| seq == group.group.start) {
								let step = self.pictures.step();
								if self.pictures.highest().is_some_and(|highest| end > highest) {
									self.cadence.slack = std::time::Duration::ZERO;
								} else if !step.is_zero() {
									self.cadence.slack = step;
								}
							}
						}
						self.pending.pop_front();
						self.current += 1;
						self.note_group_edge();
						return Poll::Ready(Ok(Some(Event::GroupEnd)));
					}
				}
			}

			// The ordered reader hands out a group only once it resolved every key below it: a
			// declared skip, one the ranges leave out, or one it gave up on. So the cursor steps to
			// the next group at once, past one aborted before anything of it was read.
			if let Some(front) = self.pending.front_mut()
				&& front.start > self.current
			{
				if front.reapable(waiter) {
					self.discard_front(1)?;
					continue;
				}
				let next = front.start;
				// Keys the publisher declared skipped hold no group. Any other run lost media unless
				// the group past it continues the timeline, which its first frame shows.
				if !self.is_skipped(self.current..next) {
					self.unproven = true;
				}
				self.current = next;
				self.note_group_edge();
				continue;
			}

			// Reap later aborts only when delivery blocks. An aborted group past the current one
			// must not hold its buffer. Buffered frames and faults stay in delivery order; the
			// current group has its eviction path above. Live groups register the waiter here so a
			// later abort wakes the reader.
			let current = self.current;
			self.discard(|group| group.start > current && group.reapable(waiter))?;

			// The current group's furthest presentation point (timestamp + duration).
			let current_end = if let Some(current) = self.pending.front_mut()
				&& current.start <= self.current
			{
				let polled = current.poll_min_timestamp(waiter, &self.format, true);
				// A frame that landed since the read above is delivered, not judged: its end can
				// reach the next group, and skipping there would throw it away as lost. A held frame
				// waits on frames still to come instead.
				if current.ready() && !held {
					continue;
				}
				match polled {
					Poll::Ready(Some(_)) => current.max_end,
					_ => None,
				}
			} else {
				None
			};

			// The first newer group with data (our skip target) and where it starts, and the newest
			// content past the cursor.
			let next_group = self.poll_next_group(waiter);
			let max_timestamp = self.poll_newest(waiter);

			// The current group is blocking. Everything it could still present ends
			// where the next group begins, so skip once that reach falls a full budget
			// behind the newest content, the same measure as a missing group. Its own
			// first frame is no bound once reading began: a group longer than the budget
			// would be cut short whenever it blocks behind a newer one racing ahead on its
			// own stream. Only the start, above, measures the first frame. Skip early too
			// once it has presented up to where the next group begins (duration
			// coverage), since nothing is left worth waiting for.
			// While the reader warms, the start reached the warmup further back than the max delay.
			let budget = match self.warming() {
				true => self.max_delay.saturating_add(self.warmup),
				false => self.max_delay,
			};
			let should_skip = next_group.is_some_and(|(_, next_start)| {
				max_timestamp.saturating_sub(next_start) >= budget || current_end.is_some_and(|end| end >= next_start)
			});
			let target = match should_skip {
				true => next_group.map(|(i, start)| (i, Some(start))),
				// Looking at the later groups can fill the max buffer while the cursor's group holds its
				// first frame, which it then judges by what it holds and reads, as any group holding a
				// frame does rather than be skipped. One still holding nothing is skipped below.
				false if held && self.memory.full() && self.pending.front().is_some_and(GroupBuffer::ready) => continue,
				// Once the later groups fill the max buffer, no more of them can be held while it waits,
				// so move on to the next group even with nothing of it read yet: it reads as the
				// cursor's group, and each such skip refunds what the group left behind held.
				false if self.memory.full() => self
					.pending
					.iter()
					.position(|group| group.start > self.current)
					.map(|i| (i, self.pending[i].min_timestamp.and_then(time))),
				false => None,
			};

			if let Some((new_idx, next_start)) = target {
				self.skip_to(new_idx, next_start, current_end)?;
				continue;
			}

			if finished && self.pending.is_empty() {
				if let Reader::Failed(..) = self.reader
					&& let Reader::Failed(_, err) = std::mem::replace(&mut self.reader, Reader::Lost)
				{
					// The caller only gets the bare error, with nothing to say which track died.
					tracing::warn!(track = &*self.name, error = ?err, "track failed; ending the reader");
					return Poll::Ready(Err(err));
				}
				return Poll::Ready(Ok(None));
			}

			return Poll::Pending;
		}
	}

	// Before the first item, move the cursor past its group when that group's first frame trails the
	// newest content by more than the budget and the next group still starts at or before the live
	// edge less the reach, so it is not what the start covers, and repeat for the group it lands on.
	// Returns whether it moved.
	fn start_skip(&mut self, waiter: &kio::Waiter) -> Result<bool, F::Error> {
		// The newest content lies in the highest group with a timestamp, which a skip never drops, so
		// one read serves every skip and a long backlog is walked once.
		let mut newest = None;
		let mut moved = false;
		while let Some(group) = self.pending.front_mut() {
			if group.start > self.current {
				break;
			}
			let _ = group.poll_min_timestamp(waiter, &self.format, true);
			let Some(first) = group.buffered.front().and_then(|(frame, ..)| time(frame.timestamp)) else {
				break;
			};
			let current_end = group.max_end;
			// The next group that arrived, even one whose first frame is still in flight: that frame
			// may land within the budget, so skipping on to a later group could drop the one the start
			// belongs on. Until it lands, nothing is skipped, as `@moq/hang`'s consumer waits on its
			// successor's start.
			let Some(idx) = self.pending.iter().position(|group| group.start > self.current) else {
				break;
			};
			let Poll::Ready(Some(next_start)) = self.pending[idx].poll_min_timestamp(waiter, &self.format, false)
			else {
				break;
			};
			let Some(next_start) = time(next_start) else {
				break;
			};
			let newest = *newest.get_or_insert_with(|| self.poll_newest(waiter));
			// Nothing has been read, so the reader still warms.
			if newest.saturating_sub(first) <= self.max_delay.saturating_add(self.warmup) {
				break;
			}
			// The group the start reaches back to is kept: the next one begins past that point.
			let reach = self
				.min_delay
				.unwrap_or_default()
				.min(self.max_delay)
				.saturating_add(self.warmup);
			if !reach.is_zero() && next_start > newest.saturating_sub(reach) {
				break;
			}
			self.skip_to(idx, Some(next_start), current_end)?;
			moved = true;
		}
		Ok(moved)
	}

	// The first group past the cursor with a timestamp, the skip target, and where it starts.
	fn poll_next_group(&mut self, waiter: &kio::Waiter) -> Option<(usize, std::time::Duration)> {
		for (i, group) in self.pending.iter_mut().enumerate() {
			if group.start <= self.current {
				continue;
			}

			if let Poll::Ready(Some(ts)) = group.poll_min_timestamp(waiter, &self.format, false)
				&& let Some(ts) = time(ts)
			{
				return Some((i, ts));
			}
		}
		None
	}

	// The max timestamp across the groups past the cursor, or zero without one.
	fn poll_newest(&mut self, waiter: &kio::Waiter) -> std::time::Duration {
		for group in self.pending.iter_mut().rev() {
			if group.start <= self.current {
				break;
			}

			// Older groups won't be newer than this.
			if let Poll::Ready(Some(ts)) = group.poll_max_timestamp(waiter, &self.format)
				&& let Some(ts) = time(ts)
			{
				return ts;
			}
		}
		std::time::Duration::ZERO
	}

	// Move the cursor to the pending group at `new_idx`, dropping what lies below it. `next_start` is
	// where that group begins, if read, and `current_end` how far the group left behind presented.
	fn skip_to(
		&mut self,
		new_idx: usize,
		next_start: Option<std::time::Duration>,
		current_end: Option<std::time::Duration>,
	) -> Result<(), F::Error> {
		// Without a start read yet, nothing proves the timeline continues.
		let hole = next_start.is_none_or(|start| !pts_contiguous(current_end.or(self.presented_end), start));
		let had_marker = self.pending.iter().take(new_idx).any(GroupBuffer::marker);
		// Buffered frames are thrown away, and `current_end` counts them, so contiguity alone would hide them.
		let discarded = self
			.pending
			.iter()
			.take(new_idx)
			.any(|group| !group.buffered.is_empty());
		// `pending` is in sequence order, so what lies below the target is its prefix.
		self.discard_front(new_idx)?;
		if hole || had_marker || discarded {
			self.lose();
		}
		let new_current = self.pending.front().map(|g| g.start).unwrap();

		tracing::debug!(old = self.current, new = new_current, "skipping slow groups");

		self.current = new_current;
		self.note_group_edge();
		Ok(())
	}

	// The reader has not yet read the warmup past its first frame.
	fn warming(&self) -> bool {
		self.warmed
			.is_none_or(|warmed| self.edge.is_none_or(|edge| edge < warmed))
	}

	// The subscription, which only an error ending the reader drops.
	fn subscriber(&mut self) -> &mut moq_net::track::Ordered {
		match &mut self.reader {
			Reader::Reading(track) | Reader::Failed(track, _) => track,
			Reader::Ended | Reader::Lost => unreachable!("read only until an error ends the reader"),
		}
	}

	// Media this reader will never deliver: the next item is preceded by a gap.
	fn lose(&mut self) {
		self.gap = true;
		self.unproven = false;
	}

	fn advance(&mut self, end: std::time::Duration) {
		if self.edge.is_none_or(|edge| end > edge) {
			self.edge = Some(end);
		}
	}

	fn note_group_edge(&mut self) {
		if let Some((_, ts)) = self.start {
			self.floor = Some(ts);
		}
	}

	/// Receive group handles and wait for the track and every received group to finish.
	pub(crate) fn poll_received(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		if let Reader::Ended | Reader::Lost = self.reader {
			return Poll::Ready(());
		}
		ready!(self.poll_read_finish(waiter));
		if let Reader::Failed(..) = self.reader {
			return Poll::Pending;
		}
		let first = self
			.pending
			.partition_point(|group| self.received.is_some_and(|last| group.start <= last));
		for group in self.pending.range_mut(first..) {
			if !matches!(group.group.poll_finished(waiter), Poll::Ready(Ok(_))) {
				return Poll::Pending;
			}
			self.received = Some(group.start);
		}
		Poll::Ready(())
	}

	// Reads any new groups from the track until it finished or failed.
	//
	// Returns Pending until all groups have been consumed.
	fn poll_read_finish(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		self.collect_pending(waiter);
		self.gated = false;
		if let Reader::Failed(..) = self.reader {
			return Poll::Ready(());
		}
		loop {
			// Past the max buffer, later groups wait in the transport's cache.
			if self.pull_gated() {
				self.gated = true;
				return Poll::Pending;
			}
			let next = match ready!(self.subscriber().poll_next_item(waiter)) {
				Ok(next) => next,
				Err(err) => {
					if let Reader::Reading(track) = std::mem::replace(&mut self.reader, Reader::Ended) {
						self.reader = Reader::Failed(track, err.into());
					}
					return Poll::Ready(());
				}
			};
			// Any item shows the subscription answered, so the publisher's largest now says where it
			// stood as it began.
			self.join(waiter);

			let group = match next {
				Some(moq_net::track::Item::Group(group)) => group,
				// What the ordered reader gave up was never read, so it is backlog when produced before
				// the subscription. Its landing is the latest group's, so a gap reaching past the join
				// may report one produced since, and folds nothing.
				Some(moq_net::track::Item::Gap { range, landed, .. }) => {
					if self.joined.is_some_and(|joined| range.end.saturating_sub(1) <= joined) {
						self.backlog = self.backlog.max(landed);
					}
					continue;
				}
				// An item this version does not know is not a group to read.
				Some(_) => continue,
				// Track is finished.
				None => return Poll::Ready(()),
			};

			let reader = GroupBuffer::new(group, self.memory.clone());
			// Groups come in key order, so only a live start, which jumped to the newest, leaves any
			// below the cursor.
			if reader.start < self.current {
				tracing::debug!(old = ?reader.start, current = ?self.current, "skipping old group");
				Self::skip(self.joined, &mut self.backlog, &reader);
				continue;
			}
			self.pending.push_back(reader);
			self.collect_pending(waiter);
		}
	}

	fn collect_pending(&mut self, waiter: &kio::Waiter) {
		if self.pending.len() < self.collect_at {
			return;
		}
		let current = self.current;
		self.pending.retain_mut(|group| {
			// Keep the cursor's gap and anything already decoded, including markers and faults.
			// A never-read evicted group holds no event that its missing sequence cannot say.
			group.start == current || !group.is_empty() || !group.poll_aborted(waiter)
		});
		self.collect_at = self.pending.len().saturating_mul(2).max(1);
	}

	// Whether the max buffer holds back the next group: it is full, and the cursor's group already has a
	// successor to move on to, so however small the cap, a stalled group can always be skipped.
	fn pull_gated(&self) -> bool {
		self.memory.full() && self.pending.len() >= 2
	}

	// Note `group` dropped without delivering all of it: backlog when the publisher's largest at the
	// subscription's start reached it. One above was produced since, such as an outage's.
	fn skip(joined: Option<u64>, backlog: &mut Option<Instant>, group: &GroupBuffer<F>) {
		if joined.is_some_and(|joined| group.start <= joined) {
			*backlog = (*backlog).max(Some(group.group.arrived()));
		}
	}

	// Take every largest the publisher reported since the last poll.
	fn poll_largest(&mut self, waiter: &kio::Waiter) {
		while let Poll::Ready(Some(largest)) = self.subscriber().poll_largest(waiter) {
			self.largest = self.largest.max(Some(largest.group));
		}
	}

	// Where the publisher stood as the subscription began, taken once: after its answer rather than
	// from a group that raced it, and at the latest as the start is decided, before anything is
	// dropped against it.
	fn join(&mut self, waiter: &kio::Waiter) {
		if self.joined.is_some() {
			return;
		}
		self.poll_largest(waiter);
		self.joined = self.largest;
	}

	// Drop the pending groups `pick` selects without delivering them. A drop must not hide what
	// was already found, so the lowest one holding a fault, or with frames below the start of the
	// latest delivered group, ends the reader instead. What was never read stays unread: decoding
	// data only to throw it away is work a peer could force.
	fn discard(&mut self, mut pick: impl FnMut(&mut GroupBuffer<F>) -> bool) -> Result<(), F::Error> {
		let start = self.start;
		let mut fault = None;
		let (joined, backlog) = (self.joined, &mut self.backlog);
		self.pending.retain_mut(|group| {
			if !pick(group) {
				return true;
			}
			Self::skip(joined, backlog, group);
			if fault.is_none() {
				fault = Self::discard_fault(start, group);
			}
			false
		});
		self.discarded(fault)
	}

	// `discard` of the lowest `count` pending groups, in time proportional to them rather than to every
	// pending group, so a start that skips a long backlog one group at a time stays linear.
	fn discard_front(&mut self, count: usize) -> Result<(), F::Error> {
		let start = self.start;
		let mut fault = None;
		for mut group in self.pending.drain(..count) {
			Self::skip(self.joined, &mut self.backlog, &group);
			if fault.is_none() {
				fault = Self::discard_fault(start, &mut group);
			}
		}
		self.discarded(fault)
	}

	// The fault a discarded group would hide: its own, or frames below the latest delivered start.
	fn discard_fault(start: Option<(u64, Timestamp)>, group: &mut GroupBuffer<F>) -> Option<F::Error> {
		match (start, group.min_timestamp) {
			(Some((prev, floor)), Some(min))
				if group.start > prev
					&& time(min)
						.zip(time(floor))
						.is_some_and(|(min, floor)| min.as_micros() < floor.as_micros()) =>
			{
				Some(TimestampRewind { timestamp: min, floor }.into())
			}
			_ => group.fault.take(),
		}
	}

	fn discarded(&self, fault: Option<F::Error>) -> Result<(), F::Error> {
		match fault {
			Some(err) => {
				tracing::warn!(track = &*self.name, error = ?err, "discarded a malformed group; ending the reader");
				Err(err)
			}
			None => Ok(()),
		}
	}

	/// Whether the publisher declared every group start in `groups` skipped, so no group lies
	/// there; see [`moq_net::track::Ordered::is_skipped`]. A group with nothing possible
	/// below it is its track's first. False once an error ended the reader.
	pub fn is_skipped(&self, groups: std::ops::Range<u64>) -> bool {
		match &self.reader {
			Reader::Reading(track) | Reader::Failed(track, _) => track.is_skipped(groups),
			Reader::Ended | Reader::Lost => false,
		}
	}

	/// Set the [`Delay`], mid-stream or before the first read.
	///
	/// [`Delay::max`] is clamped to the track's max age like [`new`](Self::new). The
	/// subscription keeps the requested max verbatim, plus any [`Playout::warmup`]; its ranges
	/// keep the live join they were subscribed with (see [`Delay::subscription`]). A
	/// [`Delay::min`] moves where this reader keeps a start still to come, reading an unstated one
	/// as zero. A min past the max starts at the max, since the publisher serves nothing older. A
	/// missing group is given up by the subscription's max alone.
	pub fn set_delay(&mut self, delay: Delay) {
		self.set_skip(delay);
		// An ended reader has no subscription left to tune.
		let (Reader::Reading(track) | Reader::Failed(track, _)) = &mut self.reader else {
			return;
		};
		// The transport enforces the same budget on the subscription itself, so a
		// tolerance set here has to reach it: otherwise moq-net skips the very groups
		// this consumer was told to wait for, before they ever get here. It keeps the
		// warmup too, as [`Playout::subscription`] does.
		// Only each live join's max, retuned in place: a replaced range resolves again, so the
		// ranges keep the join they were subscribed with, and history stays history.
		let max = warmed(delay, self.warmup).max;
		let current = track.subscription();
		let mut subscription = current.clone();
		for range in &mut subscription.ranges {
			if let moq_net::track::Start::Live { max: held, .. } = &mut range.start {
				*held = max;
			}
		}
		if subscription != current {
			let _ = track.update(subscription);
		}
	}

	/// Skip at `delay`, clamped like [`set_delay`](Self::set_delay), leaving the subscription's
	/// delay as it is. A delay past the subscription's skips nothing later, since the transport
	/// drops a group first.
	pub(crate) fn skipping_at(mut self, delay: Delay) -> Self {
		self.set_skip(delay);
		self
	}

	// Wait and skip at `delay`, its max clamped to the track's max age.
	fn set_skip(&mut self, delay: Delay) {
		if let Reader::Reading(track) | Reader::Failed(track, _) = &self.reader {
			self.min_delay = delay.min;
			self.max_delay = delay
				.max
				.unwrap_or(std::time::Duration::MAX)
				.min(track.info().max_age.unwrap_or(std::time::Duration::MAX));
		}
	}

	/// Cap the bytes of decoded frames held ahead of the reader, counting the wire payload they share
	/// and each frame's and group's own overhead (default [`DEFAULT_MAX_BUFFER`]). Past it the consumer stops decoding ahead
	/// and pulling new groups; see the type docs.
	#[cfg(test)]
	pub(crate) fn set_max_buffer(&mut self, bytes: usize) {
		self.memory.set_max(bytes);
	}

	/// The bytes currently charged against [`set_max_buffer`](Self::set_max_buffer).
	#[cfg(test)]
	pub(crate) fn buffered_bytes(&self) -> usize {
		self.memory.used()
	}
}

/// Why a group stopped buffering.
enum Failure<E> {
	/// A consumed payload failed to decode, so the track is malformed.
	Decode(E),
	/// The transport aborted or evicted the group before its next frame arrived.
	Lost(E),
}

/// Internal reader for a group of frames.
///
/// Handles two-phase frame reading (get FrameConsumer, then read all data),
/// timestamp parsing, and min/max timestamp tracking for age decisions.
struct GroupBuffer<F: Container> {
	group: moq_net::group::Consumer,

	// The current frame index within the group.
	index: usize,

	content: Content,

	// A decode error found while looking ahead, delivered after the frames buffered before it.
	fault: Option<F::Error>,

	// Read frames that haven't been consumed yet, each with when its wire frame landed.
	// Each with when it arrived and what it holds against the max buffer.
	buffered: VecDeque<(Frame, Instant, usize)>,
	markers: VecDeque<(usize, Timestamp)>,
	delivered: usize,

	// The consumer's max buffer, and how much of it this group holds, refunded when it drops.
	memory: Memory,
	charged: usize,

	// The minimum timestamp in the group.
	min_timestamp: Option<Timestamp>,

	// The maximum timestamp in the group.
	max_timestamp: Option<Timestamp>,

	// The furthest presentation point reached so far, i.e. max(timestamp + duration).
	// Equals the max timestamp when the container carries no per-frame duration.
	// Stored as a wall-clock duration so cross-scale comparisons are cheap.
	max_end: Option<std::time::Duration>,
}

/// What a group has carried so far.
enum Content {
	/// No wire frame: an empty group means nothing.
	Empty,
	/// Wire frames but no media, so a marker once it finishes.
	Marker,
	/// A data track's marker: one object with no bytes. `ended` once it was read as an end,
	/// since it has no timestamp of its own.
	Bare { ended: bool },
	/// A decodable (non-marker) frame was buffered.
	Media,
}

impl<F: Container> GroupBuffer<F> {
	fn new(group: moq_net::group::Consumer, memory: Memory) -> Self {
		let mut this = Self {
			group,
			index: 0,
			content: Content::Empty,
			fault: None,
			buffered: VecDeque::new(),
			markers: VecDeque::new(),
			delivered: 0,
			max_timestamp: None,
			min_timestamp: None,
			max_end: None,
			memory,
			charged: 0,
		};
		this.charge(std::mem::size_of::<Self>());
		this
	}

	/// Poll for the next frame from this group.
	/// Whether [`Self::poll_read`] would hand out an item without reading the wire: a frame
	/// looked ahead, or a marker it reached.
	fn ready(&self) -> bool {
		!self.buffered.is_empty() || self.markers.front().is_some_and(|(index, _)| *index <= self.delivered)
	}

	fn poll_read(&mut self, waiter: &kio::Waiter, format: &F) -> Poll<Result<Option<Event>, Failure<F::Error>>> {
		loop {
			if self.markers.front().is_some_and(|(index, _)| *index <= self.delivered) {
				let (index, end) = self.markers.pop_front().unwrap();
				self.refund(MARKER_COST);
				return Poll::Ready(Ok(Some(Event::FrameEnd { end, opens: index == 0 })));
			}
			if let Some((frame, arrived, cost)) = self.buffered.pop_front() {
				self.delivered += 1;
				self.refund(cost);
				return Poll::Ready(Ok(Some(Event::Frame { frame, arrived })));
			}
			if !ready!(self.buffer_once(waiter, format)?) {
				return Poll::Ready(Ok(None));
			}
		}
	}

	/// Whether this group, landed on past keys nothing declared skipped, continues the content read
	/// before it: a frame of it presents by `bound`, within one millisecond. Its lowest picture
	/// decodes within `depth` frames of its first, so it looks no further, nor past its end, its
	/// failure, or a full max buffer. `None` while it holds no frame, when an endpoint, its end, or a
	/// failure comes first.
	fn poll_continues(
		&mut self,
		waiter: &kio::Waiter,
		format: &F,
		bound: Option<std::time::Duration>,
		depth: usize,
	) -> Poll<Option<bool>> {
		// Nothing proves a step with nowhere to continue from, so the first frame decides.
		let depth = if bound.is_some() { depth } else { 0 };
		loop {
			if self.buffered.is_empty() && self.ready() {
				return Poll::Ready(None);
			}
			let continues = (self.buffered.iter().take(depth + 1))
				.any(|(frame, ..)| time(frame.timestamp).is_some_and(|at| pts_contiguous(bound, at)));
			if continues {
				return Poll::Ready(Some(true));
			}
			if self.buffered.len() > depth {
				return Poll::Ready(Some(false));
			}
			// A group holding nothing buffers past a full max buffer, as the cursor's does. One holding
			// a frame judges by what it holds, since only reading frames frees the buffer.
			let lead = self.buffered.is_empty();
			if !lead && self.memory.full() {
				return Poll::Ready(Some(false));
			}
			if !ready!(self.look_once(waiter, format, !lead)) {
				return Poll::Ready((!self.buffered.is_empty()).then_some(false));
			}
		}
	}

	// Add one more frame to the buffer if possible, first returning a decode error the lookahead kept.
	//
	// Returns false if the group is finished.
	fn buffer_once(&mut self, waiter: &kio::Waiter, format: &F) -> Poll<Result<bool, Failure<F::Error>>> {
		if let Some(err) = self.fault.take() {
			return Poll::Ready(Err(Failure::Decode(err)));
		}
		// A failed read is the transport's, while a failed decode is the payload's: an abort of
		// the tail behind a malformed frame must not hide that frame.
		let wire = match ready!(self.group.poll_read_frame(waiter)) {
			Ok(Some(wire)) => wire,
			Ok(None) => return Poll::Ready(Ok(false)),
			Err(err) if self.poll_aborted(waiter) => return Poll::Ready(Err(Failure::Lost(err.into()))),
			Err(err) => return Poll::Ready(Err(Failure::Decode(err.into()))),
		};
		if matches!(self.content, Content::Empty) {
			self.content = Content::Marker;
		}
		let arrived = wire.arrived();
		let size = wire.payload.len();
		let frames = format.decode(wire).map_err(Failure::Decode)?;
		if format.kind() == super::Kind::Data {
			// A data track's marker is one object with no bytes, alone in its group.
			match (&self.content, frames.is_empty()) {
				(Content::Bare { .. }, _) | (Content::Media, true) => {
					let err = moq_net::Error::Decode(moq_net::DecodeError::ExpectedEnd).into();
					return Poll::Ready(Err(Failure::Decode(err)));
				}
				(_, true) => self.content = Content::Bare { ended: false },
				(_, false) => {}
			}
		}

		// Decoded payloads are views of the wire payload, which stays allocated while any of them is
		// held, so the last one read also holds whatever of it the views leave out, such as a header.
		let mut views = 0;
		let first = self.buffered.len();
		// Media is timed: a scale-less track's unscaled keys carry no time to present at.
		let unscaled = || Failure::Decode(moq_net::Error::TimestampMismatch.into());
		for mut frame in frames {
			if let Some(bound) = format.end(&frame) {
				let end = time(bound).ok_or_else(unscaled)?;
				self.note_end(end);
				self.markers.push_back((self.index, bound));
				self.charge(MARKER_COST);
				continue;
			}
			let at = time(frame.timestamp).ok_or_else(unscaled)?;
			let duration = frame
				.duration
				.map(|duration| time(duration).ok_or_else(unscaled))
				.transpose()?;

			// Every buffered timestamp has a time, so any two of them compare.
			self.min_timestamp = Some(match self.min_timestamp {
				Some(existing) if existing <= frame.timestamp => existing,
				_ => frame.timestamp,
			});

			self.max_timestamp = Some(match self.max_timestamp {
				Some(existing) if existing > frame.timestamp => existing,
				_ => frame.timestamp,
			});

			// Furthest presentation point, in wall-clock terms so timestamp and
			// duration can be at different scales without extra conversions. A frame
			// with no duration contributes only its timestamp.
			self.note_end(at);
			if let Some(duration) = duration {
				self.note_end(at + duration);
			}

			// First frame of a group is always a keyframe by protocol invariant; trust
			// the container's flag otherwise so CMAF mid-group keyframes survive.
			frame.keyframe = frame.keyframe || self.index == 0;
			self.index += 1;
			self.content = Content::Media;

			let cost = frame_cost(&frame);
			views += frame.payload.len();
			self.charge(cost);
			self.buffered.push_back((frame, arrived, cost));
		}
		// Views that leave out more than they keep are copied out, freeing the wire payload, so
		// whoever holds a frame after the reader retains at most twice its payload.
		let rest = size.saturating_sub(views);
		if rest > views {
			for (frame, ..) in self.buffered.range_mut(first..) {
				frame.payload = bytes::Bytes::copy_from_slice(&frame.payload);
			}
		} else if self.buffered.len() > first
			&& rest > 0
			&& let Some((_, _, cost)) = self.buffered.back_mut()
		{
			*cost += rest;
			self.charge(rest);
		}

		Poll::Ready(Ok(true))
	}

	// Buffer one more frame ahead of the cursor. Nothing here hands an error to a caller, so a
	// decode error is kept for the reader rather than dropped with the frame it consumed; a
	// transport abort is left for the cursor to find.
	//
	// Returns false once this group can buffer nothing more. When `capped`, pends without registering
	// the waiter while the max buffer is full: only the reader taking frames frees it.
	fn look_once(&mut self, waiter: &kio::Waiter, format: &F, capped: bool) -> Poll<bool> {
		if self.fault.is_some() {
			return Poll::Ready(false);
		}
		if capped && self.memory.full() {
			return Poll::Pending;
		}
		match ready!(self.buffer_once(waiter, format)) {
			Ok(more) => Poll::Ready(more),
			Err(Failure::Decode(err)) => {
				self.fault = Some(err);
				Poll::Ready(false)
			}
			Err(Failure::Lost(_)) => Poll::Ready(false),
		}
	}

	fn look_all(&mut self, waiter: &kio::Waiter, format: &F) -> Poll<()> {
		while ready!(self.look_once(waiter, format, true)) {}
		Poll::Ready(())
	}

	/// Poll for the maximum timestamp in this group, or `None` if it ended without one.
	fn poll_max_timestamp(&mut self, waiter: &kio::Waiter, format: &F) -> Poll<Option<Timestamp>> {
		// Keep reading more frames just to advance the max timestamp.
		let looked = self.look_all(waiter, format);

		if let Some(max) = self.max_timestamp {
			return Poll::Ready(Some(max));
		}
		// Frames left unread behind a full max buffer may still carry one.
		ready!(looked);
		self.poll_ended(waiter)
	}

	/// Poll for the minimum timestamp in this group, or `None` if it ended without one. A `lead` group
	/// buffers past a full max buffer only while it holds nothing, endpoint markers included.
	fn poll_min_timestamp(&mut self, waiter: &kio::Waiter, format: &F, lead: bool) -> Poll<Option<Timestamp>> {
		// A wire frame can decode to no media frames, so keep going until one is buffered.
		let mut looked = Poll::Ready(());
		while self.buffered.is_empty() {
			let capped = !lead || !self.markers.is_empty();
			match self.look_once(waiter, format, capped) {
				Poll::Ready(true) => continue,
				Poll::Ready(false) => break,
				Poll::Pending => {
					looked = Poll::Pending;
					break;
				}
			}
		}

		if let Some(min) = self.min_timestamp {
			return Poll::Ready(Some(min));
		}
		// Frames left unread behind a full max buffer may still carry one.
		ready!(looked);
		self.poll_ended(waiter)
	}

	// Ready with no timestamp once the group can no longer produce one: it ended or failed to decode.
	fn poll_ended(&mut self, waiter: &kio::Waiter) -> Poll<Option<Timestamp>> {
		if self.fault.is_some() || self.group.poll_finished(waiter).is_ready() {
			return Poll::Ready(None);
		}
		Poll::Pending
	}

	/// True if the transport can no longer deliver the frame this group is stopped on:
	/// the stream was reset (evicted, `Old`, cancelled, oversized, ...).
	fn poll_aborted(&mut self, waiter: &kio::Waiter) -> bool {
		matches!(self.group.poll_finished(waiter), Poll::Ready(Err(_)))
	}

	/// True if this group holds nothing to deliver and never will: aborted with nothing
	/// buffered, no endpoint decoded, and no fault to report.
	fn reapable(&mut self, waiter: &kio::Waiter) -> bool {
		self.buffered.is_empty() && self.markers.is_empty() && self.fault.is_none() && self.poll_aborted(waiter)
	}

	fn note_end(&mut self, end: std::time::Duration) {
		self.max_end = Some(self.max_end.map_or(end, |existing| existing.max(end)));
	}

	fn marker(&self) -> bool {
		matches!(self.content, Content::Marker | Content::Bare { .. })
	}

	/// The group has carried no wire frame.
	fn is_empty(&self) -> bool {
		matches!(self.content, Content::Empty)
	}

	fn charge(&mut self, bytes: usize) {
		self.charged += bytes;
		self.memory.charge(bytes);
	}

	fn refund(&mut self, bytes: usize) {
		self.charged -= bytes;
		self.memory.refund(bytes);
	}
}

impl<F: Container> Drop for GroupBuffer<F> {
	fn drop(&mut self) {
		self.memory.refund(self.charged);
	}
}

/// What a buffered endpoint marker holds against the max buffer.
const MARKER_COST: usize = std::mem::size_of::<(usize, Timestamp)>();

/// What a buffered frame holds against the max buffer: its payload and its own entry.
pub(crate) fn frame_cost(frame: &Frame) -> usize {
	std::mem::size_of::<(Frame, Instant, usize)>() + frame.payload.len()
}

impl<F: Container> std::ops::Deref for GroupBuffer<F> {
	type Target = moq_net::group::Consumer;

	fn deref(&self) -> &Self::Target {
		&self.group
	}
}

#[cfg(test)]
mod tests {
	use super::Container as ContainerTrait;
	use super::*;
	use crate::catalog::hang::Container;

	/// The subscription a consumer still reads.
	fn subscribed<F: ContainerTrait>(consumer: &Consumer<F>) -> &moq_net::track::Ordered {
		match &consumer.reader {
			Reader::Reading(track) | Reader::Failed(track, _) => track,
			Reader::Ended | Reader::Lost => panic!("the reader ended"),
		}
	}
	use std::time::Duration;

	use bytes::Bytes;

	/// A track published at one origin and read from another over a lite-07 session: a received
	/// copy, whose ordered reader waits for a group the feed still owes, as a player's does. A
	/// locally produced track owes nothing, so a hole there is stepped over at once.
	struct Received {
		track: moq_net::track::Producer,
		remote: moq_net::broadcast::Consumer,
		_route: moq_net::origin::Route,
		_link: crate::container::test_util::Link,
	}

	impl Received {
		async fn new() -> Self {
			Self::with_info(hang::container::track_info(hang::catalog::PRIORITY.audio)).await
		}

		async fn with_info(info: moq_net::track::Info) -> Self {
			use crate::container::test_util::{Link, origin};

			let (publisher, subscriber) = (origin(), origin());
			let link = Link::new(&publisher, &subscriber).await;
			let route = publisher
				.route("room", moq_net::origin::RouteOptions::hidden())
				.unwrap();
			let track = route.broadcast().create_track("data", info).unwrap();
			route.announce().unwrap();
			let remote = subscriber
				.consume()
				.follow("room", None)
				.unwrap()
				.available()
				.await
				.unwrap()
				.unwrap();
			Self {
				track,
				remote,
				_route: route,
				_link: link,
			}
		}

		/// Subscribe to the copy at `delay`.
		async fn subscribe(&self, delay: Delay) -> moq_net::track::Subscriber {
			self.subscribe_with(
				delay.subscription(moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::ALL])),
			)
			.await
		}

		async fn subscribe_with(&self, subscription: moq_net::track::Subscription) -> moq_net::track::Subscriber {
			self.remote
				.track("data")
				.unwrap()
				.subscribe(subscription)
				.await
				.unwrap()
		}

		/// A reader of the copy at `delay`.
		async fn reader(&self, delay: Delay) -> Consumer<Container> {
			Consumer::new(
				self.subscribe(delay).await,
				Container::Legacy(crate::container::Kind::Data),
			)
		}
	}

	/// Let the session carry what was written to the copy, without moving the clock.
	async fn carried() {
		for _ in 0..64 {
			tokio::task::yield_now().await;
		}
	}

	/// A millisecond track: the frames written here carry timestamps. It sets no publisher max
	/// age, so the test's delay budget alone decides which superseded groups are read.
	fn timed() -> moq_net::track::Info {
		moq_net::track::Info::default()
			.with_timescale(moq_net::Timescale::MILLI)
			.with_max_age(None)
	}

	/// Mint a standalone track for tests via a throwaway broadcast, since tracks are
	/// born from their broadcast (no public `track::Producer::new`).
	fn track_producer(
		name: impl Into<std::sync::Arc<str>>,
		info: impl Into<Option<moq_net::track::Info>>,
	) -> moq_net::track::Producer {
		moq_net::broadcast::Info::new()
			.produce()
			.create_track(name, info)
			.unwrap()
	}

	fn ts(micros: u64) -> Timestamp {
		Timestamp::from_micros(micros).unwrap()
	}

	/// Test-only container that round-trips a per-sample duration on the wire, so the
	/// duration-based skip can be exercised without building a real CMAF init segment.
	/// Each frame is `[timestamp_us: u64 LE][duration_us: u64 LE][payload]`.
	struct DurationWire;

	/// Encode a `[timestamp][duration][payload]` DurationWire frame.
	fn encode_duration_frame(timestamp: Timestamp, duration: Timestamp) -> Vec<u8> {
		let mut buf = Vec::with_capacity(18);
		buf.extend_from_slice(&(timestamp.as_micros().unwrap() as u64).to_le_bytes());
		buf.extend_from_slice(&(duration.as_micros().unwrap() as u64).to_le_bytes());
		buf.extend_from_slice(&[0xDE, 0xAD]);
		buf
	}

	impl ContainerTrait for DurationWire {
		type Error = crate::Error;

		fn write(&self, group: &mut moq_net::group::Producer, frames: &[Frame]) -> Result<(), Self::Error> {
			// The duration tests write frames directly via `write_duration_frame`;
			// this path just preserves the timestamp with an unknown duration.
			for frame in frames {
				group.write_frame(frame.timestamp, encode_duration_frame(frame.timestamp, ts(0)))?;
			}
			Ok(())
		}

		fn decode(&self, frame: moq_net::frame::Frame) -> Result<Vec<Frame>, Self::Error> {
			use bytes::Buf;

			let mut data = frame.payload;
			let timestamp = ts(data.get_u64_le());
			let duration = ts(data.get_u64_le());
			let payload = data.copy_to_bytes(data.remaining());

			Ok(vec![Frame {
				timestamp,
				payload,
				keyframe: false,
				duration: Some(duration),
			}])
		}
	}

	/// Write one DurationWire frame (timestamp and duration in µs) into a group.
	fn write_duration_frame(group: &mut moq_net::group::Producer, timestamp: Timestamp, duration: Timestamp) {
		group
			.write_frame(timestamp, encode_duration_frame(timestamp, duration))
			.unwrap();
	}

	/// Append a finished group keyed by its first frame's time, as a container producer keys
	/// them, so a live join measures it in the track's timescale and the publisher says no group
	/// starts between it and the last (Container::Legacy(crate::container::Kind::Data) format).
	fn append_timed(track: &mut moq_net::track::Producer, timestamps: &[Timestamp]) {
		let mut group = track.append_group(timestamps[0]).unwrap();
		for &timestamp in timestamps {
			let frame = Frame {
				timestamp,
				payload: Bytes::from_static(&[0xDE, 0xAD]),
				keyframe: false,
				duration: None,
			};
			Container::Legacy(crate::container::Kind::Data)
				.write(&mut group, &[frame])
				.unwrap();
		}
		group.finish().unwrap();
	}

	/// Write a finished group with explicit sequence and timestamps (Container::Legacy(crate::container::Kind::Data) format).
	fn write_group(track: &mut moq_net::track::Producer, sequence: u64, timestamps: &[Timestamp]) {
		let mut group = track.create_group(moq_net::group::Info { start: sequence }).unwrap();
		for &timestamp in timestamps {
			let frame = Frame {
				timestamp,
				payload: Bytes::from_static(&[0xDE, 0xAD]),
				keyframe: false,
				duration: None,
			};
			Container::Legacy(crate::container::Kind::Data)
				.write(&mut group, &[frame])
				.unwrap();
		}
		group.finish().unwrap();
	}

	/// Drain all available frames with a per-read timeout.
	async fn read_all(consumer: &mut Consumer<Container>) -> Result<Vec<Frame>, crate::Error> {
		let mut frames = Vec::new();
		loop {
			match kio::clock::timeout(Duration::from_millis(200), consumer.read_frame()).await {
				Ok(Ok(Some(frame))) => frames.push(frame),
				Ok(Ok(None)) => break,
				Ok(Err(e)) => return Err(e),
				Err(_) => panic!(
					"read_all: Consumer::read timed out after 200ms ({} frames collected so far)",
					frames.len()
				),
			}
		}
		Ok(frames)
	}

	/// Read a continuous sequence, refusing gaps and declared stops.
	async fn continuous_frames<F: ContainerTrait>(consumer: &mut Consumer<F>) -> Vec<Frame>
	where
		F::Error: std::fmt::Debug,
	{
		let mut frames = Vec::new();
		while let Some(read) = consumer.read().await.unwrap() {
			let Read::Frame { frame, .. } = read else {
				panic!("unexpected break: {read:?}")
			};
			frames.push(frame);
		}
		frames
	}

	/// Wrap `track` so only the container consumer performs age skips. The
	/// transport gets the media track's full max age and hands over every
	/// fresh group; what the container does with them is what the test measures.
	///
	/// Both layers enforce the same budget, and normally should: the transport skipping
	/// a group the consumer was going to skip anyway just saves the bandwidth. These
	/// tests are the exception, since they are about the consumer's half of it.
	fn container_max_delay_only(
		track: moq_net::track::Subscriber,
		max_delay: std::time::Duration,
	) -> Consumer<Container> {
		let control = track.control();
		let mut consumer = Consumer::new(track, Container::Legacy(crate::container::Kind::Data));
		consumer.set_delay(Delay {
			max: Some(max_delay),
			..Delay::default()
		});
		control
			.update(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_secs(30)),
						max: Some(Duration::from_secs(30)),
					},
				)]),
			)
			.unwrap();
		consumer
	}

	// ---- Basic Reading ----

	#[kio::test]
	async fn read_single_group() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_millis(500)),
						max: Some(Duration::from_millis(500)),
					},
				)]),
			);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		write_group(&mut track, 0, &[ts(0)]);
		let _track = track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 1);
		assert_eq!(frames[0].timestamp, ts(0));
		assert!(frames[0].keyframe);

		// Next read returns None (track ended)
		assert!(consumer.read().await.unwrap().is_none());
	}

	fn write_marker_group(track: &mut moq_net::track::Producer, sequence: u64, timestamp: Timestamp) {
		let mut group = track.create_group(moq_net::group::Info { start: sequence }).unwrap();
		Container::Legacy(crate::container::Kind::Audio)
			.write(
				&mut group,
				&[Frame {
					timestamp,
					payload: Bytes::new(),
					keyframe: false,
					duration: None,
				}],
			)
			.unwrap();
		group.finish().unwrap();
	}

	/// Every item until the track ends: a frame as its timestamp in µs, a gap as its start and
	/// span in µs, or an end.
	async fn items(consumer: &mut Consumer<Container>) -> Vec<String> {
		let mut items = Vec::new();
		while let Some(read) = consumer.read().await.unwrap() {
			items.push(match read {
				Read::Frame { frame, .. } => frame.timestamp.as_micros().unwrap().to_string(),
				Read::Gap { timestamp, duration } => format!(
					"gap {}+{}",
					timestamp.as_micros().unwrap(),
					duration.as_micros().unwrap()
				),
				Read::End { end, .. } => format!("end {}", end.as_micros().unwrap()),
			});
		}
		items
	}

	/// An endpoint looked ahead in the next group is delivered though that group then aborts: it is
	/// decoded, so only a group holding nothing at all is reaped.
	#[kio::test]
	async fn a_looked_ahead_endpoint_outlives_its_groups_abort() {
		let track = track_producer("gaps", hang::container::track_info(hang::catalog::PRIORITY.audio));
		let subscription =
			track.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_secs(10)),
						max: Some(Duration::from_secs(10)),
					},
				)]),
			);
		let mut consumer = Consumer::new(subscription, Container::Legacy(crate::container::Kind::Audio));
		let mut head = track.create_group(moq_net::group::Info { start: 0 }).unwrap();
		Container::Legacy(crate::container::Kind::Audio)
			.write(
				&mut head,
				&[Frame {
					timestamp: ts(0),
					payload: Bytes::from_static(&[0xDE, 0xAD]),
					keyframe: true,
					duration: None,
				}],
			)
			.unwrap();
		assert!(matches!(
			consumer.read().await.unwrap(),
			Some(Read::Frame { group: 0, .. })
		));
		let mut next = track.create_group(moq_net::group::Info { start: 1 }).unwrap();
		write_marker(&mut next, ts(20_000));
		// Group 0 is still open, so the read blocks and looks ahead into group 1.
		assert!(consumer.poll_read(&kio::Waiter::noop()).is_pending());
		next.abort(moq_net::Error::Cancel).unwrap();
		assert!(consumer.poll_read(&kio::Waiter::noop()).is_pending());
		head.finish().unwrap();
		let _track = track.finish().unwrap();
		assert_eq!(items(&mut consumer).await, ["end 20000"]);
	}

	/// Joining a paused track, whose only served group is a marker, reads the end without
	/// waiting for media to resume.
	#[tokio::test(start_paused = true)]
	async fn a_join_on_a_marker_group_reads_its_end() {
		let mut track = track_producer("gaps", hang::container::track_info(hang::catalog::PRIORITY.audio));
		let subscription =
			track.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_secs(10)),
						max: Some(Duration::from_secs(10)),
					},
				)]),
			);
		let mut reader = Consumer::new(subscription, Container::Legacy(crate::container::Kind::Audio));
		write_marker_group(&mut track, 16, ts(1_000_000));

		let mut read = async || {
			kio::clock::timeout(Duration::from_secs(1), reader.read())
				.await
				.expect("the end is read while the track stays open")
				.unwrap()
		};
		assert!(matches!(read().await, Some(Read::End { group: 16, end }) if end == ts(1_000_000)));
	}

	/// A data track's marker carries no timestamp, so it reads as an end at the furthest point
	/// read rather than as a gap, and as nothing before the first item.
	#[tokio::test(start_paused = true)]
	async fn a_data_marker_reads_as_an_end_at_the_edge() {
		let hour = Duration::from_secs(3600);
		let info = hang::container::track_info(hang::catalog::PRIORITY.audio).with_max_age(hour);
		let track = track_producer("data", info);
		let subscription = moq_net::track::Subscription::default().with_ranges([moq_net::track::Range {
			start: moq_net::track::Start::Time(0),
			end: None,
			..Default::default()
		}]);
		let mut reader = Consumer::new(
			track.subscribe(subscription),
			Container::Legacy(crate::container::Kind::Data),
		);
		let mut producer = crate::container::Producer::new(track, Container::Legacy(crate::container::Kind::Data));
		let frame = |micros, payload: &'static [u8]| Frame {
			timestamp: ts(micros),
			payload: Bytes::from_static(payload),
			keyframe: true,
			duration: None,
		};

		producer.discontinuity().unwrap();
		producer.write(frame(0, b"a")).unwrap();
		producer.write(frame(10_000, b"")).unwrap();
		producer.discontinuity().unwrap();
		producer.write(frame(2_405_070_000, b"b")).unwrap();
		let _finished = producer.finish().unwrap();

		assert_eq!(items(&mut reader).await, ["0", "10000", "end 10000", "2405070000"]);
	}

	/// A data marker has no timestamp, so a group missing between the content read and the marker's
	/// start is lost, not covered by the break it declares.
	#[tokio::test(start_paused = true)]
	async fn a_hole_before_a_terminal_data_marker_is_a_gap() {
		let mut track = track_producer("data", hang::container::track_info(hang::catalog::PRIORITY.audio));
		let subscription = moq_net::track::Subscription::default().with_ranges([moq_net::track::Range {
			start: moq_net::track::Start::Time(0),
			end: None,
			..Default::default()
		}]);
		let mut reader = Consumer::new(
			track.subscribe(subscription),
			Container::Legacy(crate::container::Kind::Data),
		);
		write_group(&mut track, 0, &[ts(0), ts(10_000)]);
		let mut group = track.create_group(moq_net::group::Info { start: 20_000 }).unwrap();
		group.write_frame(ts(20_000), Bytes::new()).unwrap();
		group.finish().unwrap();
		let _track = track.finish().unwrap();

		assert_eq!(items(&mut reader).await, ["0", "10000", "gap 10000+0", "end 10000"]);
	}

	/// A missing group before a marker on a paused track is given up by the max delay's wall clock,
	/// since no newer media will age it out, and wakes the reader when it is.
	#[tokio::test(start_paused = true)]
	async fn a_sequence_hole_before_a_paused_data_marker_expires() {
		for budget in [Duration::ZERO, Duration::from_millis(100)] {
			let mut copy = Received::new().await;
			let mut reader = copy.reader(Delay::default().with_min(None).with_max(budget)).await;
			write_group(&mut copy.track, 0, &[ts(10_000)]);
			assert!(matches!(reader.read().await.unwrap(), Some(Read::Frame { .. })));
			let mut marker = copy.track.create_group(moq_net::group::Info { start: 2 }).unwrap();
			marker.write_frame(ts(20_000), Bytes::new()).unwrap();
			marker.finish().unwrap();
			carried().await;

			if !budget.is_zero() {
				assert!(reader.poll_read(&kio::Waiter::noop()).is_pending());
				tokio::time::advance(budget - Duration::from_millis(1)).await;
				assert!(reader.poll_read(&kio::Waiter::noop()).is_pending());
				let before = kio::clock::now();
				let read = kio::clock::timeout(Duration::from_millis(2), reader.read())
					.await
					.unwrap()
					.unwrap();
				assert!(matches!(read, Some(Read::Gap { .. })));
				assert_eq!(
					before.elapsed(),
					Duration::from_millis(1),
					"the budget wakes the reader"
				);
			} else {
				let read = kio::clock::timeout(Duration::ZERO, reader.read())
					.await
					.unwrap()
					.unwrap();
				assert!(matches!(read, Some(Read::Gap { .. })));
			}
			assert!(
				matches!(reader.poll_read(&kio::Waiter::noop()), Poll::Ready(Ok(Some(Read::End { group: 2, end }))) if end == ts(10_000))
			);
			let _track = copy.track.finish().unwrap();
		}
	}

	#[tokio::test(start_paused = true)]
	async fn a_filled_sequence_hole_gives_the_next_hole_a_fresh_budget() {
		let mut copy = Received::with_info(hang::container::track_info(hang::catalog::PRIORITY.audio)).await;
		let subscriber = copy
			.subscribe_with(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_millis(100)),
						max: Some(Duration::from_millis(100)),
					},
				)]),
			)
			.await;
		let mut reader = Consumer::new(subscriber, Container::Legacy(crate::container::Kind::Data));
		write_group(&mut copy.track, 0, &[ts(0)]);
		carried().await;
		assert!(matches!(
			reader.read().await.unwrap(),
			Some(Read::Frame { group: 0, .. })
		));
		write_group(&mut copy.track, 2, &[ts(20_000)]);
		carried().await;
		assert!(reader.poll_read(&kio::Waiter::noop()).is_pending());
		tokio::time::advance(Duration::from_millis(50)).await;
		carried().await;
		write_group(&mut copy.track, 1, &[ts(10_000)]);
		carried().await;
		assert!(matches!(
			reader.read().await.unwrap(),
			Some(Read::Frame { group: 1, .. })
		));
		assert!(matches!(
			reader.read().await.unwrap(),
			Some(Read::Frame { group: 2, .. })
		));
		let mut marker = copy.track.create_group(moq_net::group::Info { start: 4 }).unwrap();
		marker.write_frame(ts(30_000), Bytes::new()).unwrap();
		marker.finish().unwrap();
		carried().await;
		assert!(reader.poll_read(&kio::Waiter::noop()).is_pending());
		tokio::time::advance(Duration::from_millis(99)).await;
		carried().await;
		assert!(reader.poll_read(&kio::Waiter::noop()).is_pending());
		tokio::time::advance(Duration::from_millis(1)).await;
		carried().await;
		assert!(matches!(
			reader.poll_read(&kio::Waiter::noop()),
			Poll::Ready(Ok(Some(Read::Gap { .. })))
		));
		assert!(matches!(reader.read().await.unwrap(), Some(Read::End { group: 4, .. })));
		let _track = copy.track.finish().unwrap();
	}

	/// The arrival is when a frame landed, not when it was read, and a break keeps the last one.
	#[tokio::test(start_paused = true)]
	async fn arrival_is_when_the_frame_landed() {
		let mut track = track_producer("data", hang::container::track_info(hang::catalog::PRIORITY.video));
		let subscriber =
			track.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_secs(1)),
						max: Some(Duration::from_secs(1)),
					},
				)]),
			);
		let mut reader = Consumer::new(subscriber, Container::Legacy(crate::container::Kind::Data));
		assert_eq!(reader.arrival(), None, "nothing read yet");

		let first = kio::clock::now();
		write_group(&mut track, 0, &[ts(0)]);
		tokio::time::advance(Duration::from_millis(30)).await;
		let second = kio::clock::now();
		write_group(&mut track, 1, &[ts(33_000)]);
		tokio::time::advance(Duration::from_millis(30)).await;

		assert!(matches!(
			reader.read().await.unwrap(),
			Some(Read::Frame { group: 0, .. })
		));
		assert_eq!(reader.arrival(), Some(first));
		assert!(matches!(
			reader.read().await.unwrap(),
			Some(Read::Frame { group: 1, .. })
		));
		assert_eq!(reader.arrival(), Some(second));

		let mut marker = track.create_group(moq_net::group::Info { start: 2 }).unwrap();
		marker.write_frame(ts(66_000), Bytes::new()).unwrap();
		marker.finish().unwrap();
		assert!(matches!(reader.read().await.unwrap(), Some(Read::End { group: 2, .. })));
		assert_eq!(reader.arrival(), Some(second), "an end carries no arrival of its own");
	}

	/// A group at or below the publisher's largest when the subscription began was produced
	/// before it: dropped unread below the cursor, it is the join's backlog, and the reader
	/// reports when it landed. One produced since, such as an outage's, is not.
	#[tokio::test(start_paused = true)]
	async fn a_backlog_group_below_the_cursor_reports_when_it_landed() {
		let mut track = track_producer("data", hang::container::track_info(hang::catalog::PRIORITY.audio));
		let subscriber =
			track.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_secs(10)),
						max: Some(Duration::from_secs(10)),
					},
				)]),
			);
		let mut reader = Consumer::new(subscriber, Container::Legacy(crate::container::Kind::Audio));
		reader.set_delay(
			Delay::default()
				.with_min(Duration::from_millis(50))
				.with_max(Duration::from_secs(10)),
		);

		// The publisher stood at group 5 as the subscription began.
		write_group(&mut track, 5, &[ts(100_000)]);
		assert!(matches!(
			reader.read().await.unwrap(),
			Some(Read::Frame { group: 5, .. })
		));
		write_group(&mut track, 6, &[ts(120_000)]);
		assert!(matches!(
			reader.read().await.unwrap(),
			Some(Read::Frame { group: 6, .. })
		));

		tokio::time::advance(Duration::from_millis(30)).await;
		write_group(&mut track, 4, &[ts(80_000)]);
		write_group(&mut track, 7, &[ts(140_000)]);
		assert!(matches!(
			reader.read().await.unwrap(),
			Some(Read::Frame { group: 7, .. })
		));
		let landed = kio::clock::now();
		assert_eq!(reader.backlog(), Some(landed));

		// Group 6 itself came after the subscription began, so a copy of it would not be backlog.
		tokio::time::advance(Duration::from_millis(30)).await;
		write_group(&mut track, 8, &[ts(160_000)]);
		write_group(&mut track, 10, &[ts(200_000)]);
		assert!(matches!(
			reader.read().await.unwrap(),
			Some(Read::Frame { group: 8, .. })
		));
		assert!(matches!(reader.read().await.unwrap(), Some(Read::Gap { .. })));
		assert!(matches!(
			reader.read().await.unwrap(),
			Some(Read::Frame { group: 10, .. })
		));
		write_group(&mut track, 9, &[ts(180_000)]);
		write_group(&mut track, 11, &[ts(220_000)]);
		assert!(matches!(
			reader.read().await.unwrap(),
			Some(Read::Frame { group: 11, .. })
		));
		assert_eq!(
			reader.backlog(),
			Some(landed),
			"group 9 was produced after the subscription"
		);
	}

	/// The start drops the backlog below it unread, which still reports when it landed.
	#[tokio::test(start_paused = true)]
	async fn a_live_start_reports_the_backlog_it_drops() {
		let mut track = track_producer("data", hang::container::track_info(hang::catalog::PRIORITY.audio));
		let landed = kio::clock::now();
		write_group(&mut track, 3, &[ts(0)]);
		tokio::time::advance(Duration::from_millis(30)).await;
		write_group(&mut track, 4, &[ts(20_000)]);
		let subscriber =
			track.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_secs(1)),
						max: Some(Duration::from_secs(1)),
					},
				)]),
			);
		let mut reader = Consumer::new(subscriber, Container::Legacy(crate::container::Kind::Audio)).live();
		assert!(matches!(
			reader.read().await.unwrap(),
			Some(Read::Frame { group: 4, .. })
		));
		assert_eq!(reader.backlog(), Some(landed));
	}

	/// A join's backlog the ordered reader gives up as stale, never handing it to this reader, still
	/// reports when it landed: its gap carries the landing.
	#[kio::test]
	async fn a_stale_backlog_the_ordered_reader_gives_up_reports_when_it_landed() {
		let mut track = track_producer("data", hang::container::track_info(hang::catalog::PRIORITY.audio));
		let landed = kio::clock::now();
		write_group(&mut track, 0, &[ts(0)]);
		write_group(&mut track, 1, &[ts(20_000)]);
		kio::clock::sim::advance(Duration::from_millis(100)).await;
		write_group(&mut track, 2, &[ts(40_000)]);
		let subscriber =
			track.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_millis(50)),
						max: Some(Duration::from_millis(50)),
					},
				)]),
			);
		let mut reader = Consumer::new(subscriber, Container::Legacy(crate::container::Kind::Audio));
		assert!(matches!(
			reader.read().await.unwrap(),
			Some(Read::Frame { group: 1, .. })
		));
		assert_eq!(reader.backlog(), Some(landed), "group 0 went stale unread");
	}

	/// A gap's landing is when the latest group it gives up landed, so only a gap wholly at or below
	/// the join is backlog: one reaching past it may report a group produced after the subscription.
	#[tokio::test(start_paused = true)]
	async fn a_gap_reaching_past_the_join_folds_no_backlog() {
		let budget = Duration::from_secs(2);
		for (joined, backlog) in [(151, false), (152, true)] {
			let mut copy = Received::new().await;
			let mut reader = copy
				.reader(Delay::default().with_min(Duration::from_millis(100)).with_max(budget))
				.await;
			write_group(&mut copy.track, 150, &[ts(0)]);
			carried().await;
			assert!(matches!(
				reader.read().await.unwrap(),
				Some(Read::Frame { group: 150, .. })
			));
			// Where the publisher stood as the subscription began.
			reader.joined = Some(joined);
			write_group(&mut copy.track, 151, &[ts(20_000)]);
			tokio::time::advance(Duration::from_millis(10)).await;
			let landed = kio::clock::now();
			write_group(&mut copy.track, 152, &[ts(40_000)]);
			write_group(&mut copy.track, 153, &[ts(60_000)]);
			carried().await;
			// Groups 151 and 152 wait unread until their successors landed a budget ago, so both are
			// given up as one stale gap, landing when 152 did.
			tokio::time::advance(budget + Duration::from_millis(1)).await;
			write_group(&mut copy.track, 154, &[ts(80_000)]);
			carried().await;
			assert!(matches!(reader.read().await.unwrap(), Some(Read::Gap { .. })));
			assert!(matches!(
				reader.read().await.unwrap(),
				Some(Read::Frame { group: 153, .. })
			));
			assert_eq!(reader.backlog(), backlog.then_some(landed), "joined at {joined}");
		}
	}

	/// The join's boundary is where the publisher stood as it answered, though the groups it owes
	/// are held up while the publisher moves on: the first item the ordered reader hands out comes
	/// after the largest has passed it.
	#[tokio::test(start_paused = true)]
	async fn the_join_is_the_answer_though_the_first_group_is_held() {
		use crate::container::test_util::{Link, origin};

		let (publisher, subscriber) = (origin(), origin());
		// SETUP and the Announce Stream take the first two uni streams, so every group waits.
		let link = Link::held(&publisher, &subscriber, 2).await;
		let route = publisher
			.route("room", moq_net::origin::RouteOptions::hidden())
			.unwrap();
		let mut track = route
			.broadcast()
			.create_track("data", hang::container::track_info(hang::catalog::PRIORITY.audio))
			.unwrap();
		route.announce().unwrap();
		let remote = subscriber
			.consume()
			.follow("room", None)
			.unwrap()
			.available()
			.await
			.unwrap()
			.unwrap();
		write_group(&mut track, 0, &[ts(0)]);
		let subscription = remote
			.track("data")
			.unwrap()
			.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_secs(1)),
						max: Some(Duration::from_secs(1)),
					},
				)]),
			)
			.await
			.unwrap();
		let mut reader = Consumer::new(subscription, Container::Legacy(crate::container::Kind::Audio));
		carried().await;
		assert!(reader.poll_read(&kio::Waiter::noop()).is_pending());
		write_group(&mut track, 1, &[ts(20_000)]);
		write_group(&mut track, 2, &[ts(40_000)]);
		link.open();
		carried().await;
		assert!(matches!(
			reader.read().await.unwrap(),
			Some(Read::Frame { group: 0, .. })
		));
		assert_eq!(reader.joined, Some(0));
	}

	/// The join's boundary is where the publisher stood as it answered, not the newer largest by the
	/// time a live start found a frame to start on.
	#[tokio::test(start_paused = true)]
	async fn the_join_is_the_largest_at_the_answer_not_at_the_start() {
		let mut track = track_producer("data", hang::container::track_info(hang::catalog::PRIORITY.audio));
		let landed = kio::clock::now();
		let mut open = track.create_group(moq_net::group::Info { start: 3 }).unwrap();
		let subscriber =
			track.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_secs(1)),
						max: Some(Duration::from_secs(1)),
					},
				)]),
			);
		let mut reader = Consumer::new(subscriber, Container::Legacy(crate::container::Kind::Audio)).live();
		// Group 3 holds no frame yet, so the start waits, though the publisher answered.
		assert!(
			kio::clock::timeout(Duration::from_millis(1), reader.read())
				.await
				.is_err()
		);

		// Groups 4 and 5 were produced after the subscription began, so they are no backlog.
		tokio::time::advance(Duration::from_millis(30)).await;
		write_group(&mut track, 4, &[ts(20_000)]);
		write_group(&mut track, 5, &[ts(40_000)]);
		let frame = Frame {
			timestamp: ts(0),
			payload: Bytes::from_static(&[0xDE, 0xAD]),
			keyframe: false,
			duration: None,
		};
		Container::Legacy(crate::container::Kind::Audio)
			.write(&mut open, &[frame])
			.unwrap();
		open.finish().unwrap();
		assert!(matches!(
			reader.read().await.unwrap(),
			Some(Read::Frame { group: 5, .. })
		));
		assert_eq!(reader.backlog(), Some(landed), "group 4 was produced after the answer");
	}

	/// The holes a source left behind when it stopped expire together, a budget after the groups
	/// past them arrived, rather than one budget each as the cursor reaches them. The groups that
	/// waited behind them that long are a budget late too, superseded that long ago, so only the
	/// newest is read.
	#[tokio::test(start_paused = true)]
	async fn holes_behind_a_stalled_source_share_one_budget() {
		let budget = Duration::from_millis(100);
		let mut copy = Received::with_info(hang::container::track_info(hang::catalog::PRIORITY.audio)).await;
		let subscriber = copy
			.subscribe_with(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(budget),
						max: Some(budget),
					},
				)]),
			)
			.await;
		let mut reader = Consumer::new(subscriber, Container::Legacy(crate::container::Kind::Data));
		write_group(&mut copy.track, 0, &[ts(0)]);
		carried().await;
		assert!(matches!(
			reader.read().await.unwrap(),
			Some(Read::Frame { group: 0, .. })
		));
		for sequence in [2, 4, 6] {
			write_group(&mut copy.track, sequence, &[ts(sequence * 10_000)]);
			carried().await;
		}
		assert!(reader.poll_read(&kio::Waiter::noop()).is_pending());
		tokio::time::advance(budget).await;
		carried().await;
		let mut read = Vec::new();
		while let Poll::Ready(Ok(Some(item))) = reader.poll_read(&kio::Waiter::noop()) {
			read.push(match item {
				Read::Frame { group, .. } => format!("frame {group}"),
				Read::Gap { .. } => "gap".to_string(),
				Read::End { .. } => "end".to_string(),
			});
		}
		assert_eq!(read, ["gap", "frame 6"]);
	}

	/// A missing group is waited for the max delay, whatever the min delay: the min only sets
	/// where the reader starts.
	#[tokio::test(start_paused = true)]
	async fn a_hole_is_waited_for_the_max_delay_whatever_the_min() {
		let max = Duration::from_secs(1);
		for min in [
			Some(Duration::ZERO),
			Some(Duration::from_millis(100)),
			Some(Duration::from_secs(5)),
			None,
		] {
			let mut copy = Received::with_info(hang::container::track_info(hang::catalog::PRIORITY.audio)).await;
			let subscriber = copy
				.subscribe_with(
					Delay::default().with_min(min).with_max(max).subscription(
						moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::ALL]),
					),
				)
				.await;
			let mut reader = Consumer::new(subscriber, Container::Legacy(crate::container::Kind::Data));
			write_group(&mut copy.track, 0, &[ts(0)]);
			carried().await;
			assert!(matches!(
				reader.read().await.unwrap(),
				Some(Read::Frame { group: 0, .. })
			));
			// Group 1 never arrives, and group 2 starts right after it, so only the wall clock ends it.
			write_group(&mut copy.track, 2, &[ts(40_000)]);
			carried().await;
			let since = kio::clock::now();
			assert!(matches!(reader.read().await.unwrap(), Some(Read::Gap { .. })));
			assert_eq!(since.elapsed(), max, "min {min:?}");
		}
	}

	/// A burst of later groups spanning past the min delay doesn't age out a missing group that
	/// lands in the same burst, even once the min delay has passed: a reader starved of CPU takes in
	/// every stream at once, and only the max delay gives a hole up.
	#[tokio::test(start_paused = true)]
	async fn a_burst_past_the_min_delay_does_not_age_out_a_hole() {
		let mut copy = Received::with_info(hang::container::track_info(hang::catalog::PRIORITY.audio)).await;
		let subscriber = copy
			.subscribe_with(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_secs(1)),
						max: Some(Duration::from_secs(1)),
					},
				)]),
			)
			.await;
		let mut reader = Consumer::new(subscriber, Container::Legacy(crate::container::Kind::Data));
		reader.set_delay(
			Delay::default()
				.with_min(Duration::from_millis(100))
				.with_max(Duration::from_secs(1)),
		);
		write_group(&mut copy.track, 0, &[ts(0)]);
		carried().await;
		assert!(matches!(
			reader.read().await.unwrap(),
			Some(Read::Frame { group: 0, .. })
		));
		write_group(&mut copy.track, 2, &[ts(40_000), ts(200_000)]);
		carried().await;
		write_group(&mut copy.track, 3, &[ts(300_000)]);
		carried().await;
		assert!(reader.poll_read(&kio::Waiter::noop()).is_pending());
		tokio::time::advance(Duration::from_millis(100)).await;
		carried().await;
		assert!(
			reader.poll_read(&kio::Waiter::noop()).is_pending(),
			"the min delay gives nothing up"
		);
		write_group(&mut copy.track, 1, &[ts(20_000)]);
		carried().await;
		let mut read = Vec::new();
		while let Poll::Ready(Ok(Some(item))) = reader.poll_read(&kio::Waiter::noop()) {
			read.push(match item {
				Read::Frame { group, .. } => format!("frame {group}"),
				Read::Gap { .. } => "gap".to_string(),
				Read::End { .. } => "end".to_string(),
			});
		}
		assert_eq!(read, ["frame 1", "frame 2", "frame 2", "frame 3"]);
	}

	/// A group that arrives late below an earlier one is late from that earlier arrival, as is the
	/// hole past it, so they expire a budget after that earlier arrival. An unsettled hole below
	/// the late group waits a budget from the late group's own arrival, its immediate successor.
	#[tokio::test(start_paused = true)]
	async fn a_hole_is_late_from_the_earliest_arrival_past_it() {
		let budget = Duration::from_millis(100);
		let drain = |reader: &mut Consumer<Container>| {
			let mut read = Vec::new();
			while let Poll::Ready(Ok(Some(item))) = reader.poll_read(&kio::Waiter::noop()) {
				read.push(match item {
					Read::Frame { group, .. } => format!("frame {group}"),
					Read::Gap { .. } => "gap".to_string(),
					Read::End { .. } => "end".to_string(),
				});
			}
			read
		};

		// With an unsettled hole below late group 2, group 2 keeps hole 1 owed for a budget from
		// group 2's arrival; once hole 1 expires, already-stale group 2 and hole 3 go with it.
		let mut copy = Received::with_info(hang::container::track_info(hang::catalog::PRIORITY.audio)).await;
		let subscriber = copy
			.subscribe_with(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(budget),
						max: Some(budget),
					},
				)]),
			)
			.await;
		let mut reader = Consumer::new(subscriber, Container::Legacy(crate::container::Kind::Data));
		write_group(&mut copy.track, 0, &[ts(0)]);
		carried().await;
		assert!(matches!(
			reader.read().await.unwrap(),
			Some(Read::Frame { group: 0, .. })
		));
		write_group(&mut copy.track, 4, &[ts(40_000)]);
		carried().await;
		assert!(reader.poll_read(&kio::Waiter::noop()).is_pending());
		tokio::time::advance(budget / 2).await;
		carried().await;
		write_group(&mut copy.track, 2, &[ts(20_000)]);
		carried().await;
		assert!(reader.poll_read(&kio::Waiter::noop()).is_pending());
		tokio::time::advance(budget / 2).await;
		carried().await;
		assert!(
			reader.poll_read(&kio::Waiter::noop()).is_pending(),
			"hole 1 waits on its immediate successor, group 2"
		);
		tokio::time::advance(budget / 2).await;
		carried().await;
		assert_eq!(drain(&mut reader), ["gap", "frame 4"]);

		// When nothing is owed below late group 2, group 2 and hole 3 expire a budget after
		// group 4's earlier arrival, only half a budget after group 2 itself landed.
		let mut copy = Received::with_info(hang::container::track_info(hang::catalog::PRIORITY.audio)).await;
		let subscriber = copy
			.subscribe_with(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(budget),
						max: Some(budget),
					},
				)]),
			)
			.await;
		let mut reader = Consumer::new(subscriber, Container::Legacy(crate::container::Kind::Data));
		write_group(&mut copy.track, 0, &[ts(0)]);
		copy.track.skip_groups(1..2);
		carried().await;
		assert!(matches!(
			reader.read().await.unwrap(),
			Some(Read::Frame { group: 0, .. })
		));
		write_group(&mut copy.track, 4, &[ts(40_000)]);
		carried().await;
		assert!(reader.poll_read(&kio::Waiter::noop()).is_pending());
		tokio::time::advance(budget / 2).await;
		carried().await;
		write_group(&mut copy.track, 2, &[ts(20_000)]);
		carried().await;
		tokio::time::advance(budget / 2).await;
		carried().await;
		assert_eq!(drain(&mut reader), ["gap", "frame 4"]);
	}

	/// A player waits on a hole for the max delay, as it does on a group that did arrive, whatever
	/// its playout delay.
	#[tokio::test(start_paused = true)]
	async fn a_hole_and_an_arrived_group_both_wait_up_to_the_max_delay() {
		let mut copy = Received::with_info(hang::container::track_info(hang::catalog::PRIORITY.audio)).await;
		// From group 0, which the playout delay set below doesn't move: a live join, so a missing
		// group can go stale.
		let subscriber = copy
			.subscribe_with(crate::container::test_util::within(Duration::from_secs(1)))
			.await;
		let mut reader = Consumer::new(subscriber, Container::Legacy(crate::container::Kind::Data));
		reader.set_delay(
			Delay::default()
				.with_min(Duration::from_millis(100))
				.with_max(Duration::from_secs(1)),
		);
		let drain = |reader: &mut Consumer<Container>| {
			let mut read = Vec::new();
			while let Poll::Ready(Ok(Some(item))) = reader.poll_read(&kio::Waiter::noop()) {
				read.push(match item {
					Read::Frame { group, .. } => format!("frame {group}"),
					Read::Gap { .. } => "gap".to_string(),
					Read::End { .. } => "end".to_string(),
				});
			}
			read
		};

		// A stalled group 500 ms behind the newest content is still within the max delay.
		let mut stalled = copy.track.create_group(moq_net::group::Info { start: 0 }).unwrap();
		let frame = Frame {
			timestamp: ts(0),
			payload: Bytes::from_static(&[0xDE, 0xAD]),
			keyframe: false,
			duration: None,
		};
		Container::Legacy(crate::container::Kind::Data)
			.write(&mut stalled, &[frame])
			.unwrap();
		write_group(&mut copy.track, 1, &[ts(500_000)]);
		carried().await;
		tokio::time::advance(Duration::from_millis(500)).await;
		carried().await;
		assert_eq!(drain(&mut reader), ["frame 0"]);
		stalled.finish().unwrap();
		carried().await;
		assert_eq!(drain(&mut reader), ["frame 1"]);

		// A missing group is given up the max delay after the first group past it arrived, not the
		// min delay.
		write_group(&mut copy.track, 3, &[ts(520_000)]);
		carried().await;
		tokio::time::advance(Duration::from_millis(999)).await;
		carried().await;
		assert!(drain(&mut reader).is_empty());
		tokio::time::advance(Duration::from_millis(1)).await;
		carried().await;
		assert_eq!(drain(&mut reader), ["gap", "frame 3"]);

		// Content buffered past the hole doesn't skip it early: a burst may still carry the group.
		write_group(&mut copy.track, 5, &[ts(700_000)]);
		carried().await;
		write_group(&mut copy.track, 6, &[ts(800_000)]);
		carried().await;
		tokio::time::advance(Duration::from_millis(100)).await;
		carried().await;
		assert!(drain(&mut reader).is_empty());
		write_group(&mut copy.track, 4, &[ts(600_000)]);
		carried().await;
		assert_eq!(drain(&mut reader), ["frame 4", "frame 5", "frame 6"]);

		// It skips at once when the newest content is already the max delay past the group after it.
		write_group(&mut copy.track, 8, &[ts(900_000)]);
		carried().await;
		write_group(&mut copy.track, 9, &[ts(1_900_000)]);
		carried().await;
		assert_eq!(drain(&mut reader), ["gap", "frame 8", "frame 9"]);
	}

	/// Everything `reader` hands over now, polled with `waiter`.
	fn drain_with(reader: &mut Consumer<Container>, waiter: &kio::Waiter) -> Vec<String> {
		let mut read = Vec::new();
		while let Poll::Ready(Ok(Some(item))) = reader.poll_read(waiter) {
			read.push(match item {
				Read::Frame { group, .. } => format!("frame {group}"),
				Read::Gap { .. } => "gap".to_string(),
				Read::End { .. } => "end".to_string(),
			});
		}
		read
	}

	/// The browser's resume: a reader that delivered a tail before a pause, then a replayed backlog
	/// arriving newest first behind a hole. Nothing gives a hole up before the max delay, so the
	/// reader waits for the backlog's oldest group, which lands last, and plays the replay whole.
	#[tokio::test(start_paused = true)]
	async fn a_backlog_replayed_newest_first_plays_whole_within_the_max_delay() {
		let mut copy = Received::new().await;
		let mut reader = copy
			.reader(
				Delay::default()
					.with_min(Duration::from_millis(500))
					.with_max(Duration::from_secs(2)),
			)
			.await;
		let waiter = kio::Waiter::noop();
		// The tail before the pause, a group per 100 ms of media and of wall time.
		for sequence in 0..5 {
			write_group(&mut copy.track, sequence, &[ts(sequence * 100_000)]);
			carried().await;
			assert_eq!(drain_with(&mut reader, &waiter), [format!("frame {sequence}")]);
			tokio::time::advance(Duration::from_millis(100)).await;
		}
		// Paused until 1.4 s, when the publisher replays groups 14 down to 5, one every 50 ms.
		tokio::time::advance(Duration::from_millis(900)).await;
		let mut read = Vec::new();
		for sequence in (5..15).rev() {
			write_group(&mut copy.track, sequence, &[ts(sequence * 100_000)]);
			carried().await;
			read.extend(drain_with(&mut reader, &waiter));
			tokio::time::advance(Duration::from_millis(50)).await;
		}
		let expected: Vec<String> = (5..15).map(|sequence| format!("frame {sequence}")).collect();
		assert_eq!(read, expected);
	}

	/// A malformed data marker group found while looking ahead fails the reader, rather than
	/// reading as a gap when a latency skip passes over it.
	#[tokio::test(start_paused = true)]
	async fn a_malformed_marker_passed_by_a_skip_fails_the_reader() {
		let mut track = track_producer("data", hang::container::track_info(hang::catalog::PRIORITY.audio));
		let subscription = moq_net::track::Subscription::default().with_ranges([moq_net::track::Range {
			start: moq_net::track::Start::Time(0),
			end: None,
			..Default::default()
		}]);
		let mut reader = Consumer::new(
			track.subscribe(subscription),
			Container::Legacy(crate::container::Kind::Data),
		);
		reader.max_delay = Duration::ZERO;
		// The current group stays open, so only a skip moves past it.
		let mut slow = track.create_group(moq_net::group::Info { start: 0 }).unwrap();
		hang::container::Frame {
			timestamp: ts(0),
			payload: Bytes::from_static(b"a"),
		}
		.write_to(&mut slow)
		.unwrap();
		let mut group = track.create_group(moq_net::group::Info { start: 1 }).unwrap();
		group.write_frame(ts(10_000), Bytes::new()).unwrap();
		hang::container::Frame {
			timestamp: ts(10_000),
			payload: Bytes::from_static(b"b"),
		}
		.write_to(&mut group)
		.unwrap();
		group.finish().unwrap();
		write_group(&mut track, 2, &[ts(1_000_000)]);

		let read = loop {
			match reader.read().await {
				Ok(Some(Read::Frame { frame, .. })) if frame.timestamp == ts(0) => continue,
				other => break other,
			}
		};
		assert!(read.is_err(), "{read:?}");
	}

	/// An object with no bytes marks a break only on a data track: on a media track it is
	/// malformed, and fails to decode rather than read as a marker.
	#[test]
	fn an_empty_object_is_a_marker_only_on_a_data_track() {
		for kind in [
			crate::container::Kind::Data,
			crate::container::Kind::Audio,
			crate::container::Kind::Video,
		] {
			let read = Container::Legacy(kind).decode(moq_net::frame::Frame::new(ts(0), Bytes::new()));
			match kind {
				crate::container::Kind::Data => assert!(matches!(read, Ok(frames) if frames.is_empty())),
				_ => assert!(read.is_err(), "{kind:?}: {read:?}"),
			}
		}
	}

	/// A data track's marker is alone in its group: one sharing it with another object, a
	/// frame or a second marker, is malformed rather than a break or a frame.
	#[tokio::test(start_paused = true)]
	async fn a_data_marker_sharing_its_group_is_malformed() {
		for second in [Bytes::new(), Bytes::from_static(b"a")] {
			for marker_first in [true, false] {
				let mut track = track_producer("data", hang::container::track_info(hang::catalog::PRIORITY.audio));
				let subscription = moq_net::track::Subscription::default().with_ranges([moq_net::track::Range {
					start: moq_net::track::Start::Time(0),
					end: None,
					..Default::default()
				}]);
				let mut reader = Consumer::new(
					track.subscribe(subscription),
					Container::Legacy(crate::container::Kind::Data),
				);
				write_group(&mut track, 0, &[ts(0)]);
				let mut group = track.create_group(moq_net::group::Info { start: 1 }).unwrap();
				let mut objects = [Bytes::new(), second.clone()];
				if !marker_first {
					objects.reverse();
				}
				for object in objects {
					match object.is_empty() {
						true => group.write_frame(ts(10_000), object).unwrap(),
						false => hang::container::Frame {
							timestamp: ts(10_000),
							payload: object,
						}
						.write_to(&mut group)
						.unwrap(),
					}
				}
				group.finish().unwrap();
				let _track = track.finish().unwrap();

				let error = loop {
					match reader.read().await {
						// A frame ahead of the marker goes out before the marker is seen.
						Ok(Some(Read::Frame { .. })) => continue,
						other => break other,
					}
				};
				assert!(
					error.is_err(),
					"second {second:?}, marker first {marker_first}: {error:?}"
				);
			}
		}
	}

	/// A gap runs from the delivered edge to the resume point, in the resume's scale, and
	/// never past it.
	#[test]
	fn a_gap_spans_the_edge_to_the_resume_in_its_scale() {
		let scale = moq_net::Timescale::new(90_000).unwrap();
		let resume = Timestamp::new(90_000, scale).unwrap();
		let Item::Gap { timestamp, duration } = gap(Duration::from_millis(500), resume).unwrap() else {
			unreachable!()
		};
		assert_eq!(timestamp, Timestamp::new(45_000, scale).unwrap());
		assert_eq!(duration, Timestamp::new(45_000, scale).unwrap());

		// A next group overlapping the edge leaves an empty span at the resume point.
		let Item::Gap { timestamp, duration } = gap(Duration::from_secs(2), resume).unwrap() else {
			unreachable!()
		};
		assert_eq!(timestamp, resume);
		assert_eq!(duration, Timestamp::new(0, scale).unwrap());
	}

	#[kio::test]
	async fn gaps_mark_holes_while_a_marker_reads_as_an_end() {
		let mut track = track_producer("gaps", hang::container::track_info(hang::catalog::PRIORITY.audio));
		let subscription =
			track.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_secs(10)),
						max: Some(Duration::from_secs(10)),
					},
				)]),
			);
		let mut reader = Consumer::new(subscription, Container::Legacy(crate::container::Kind::Audio));
		write_group(&mut track, 0, &[ts(0), ts(10_000)]);
		write_group(&mut track, 2, &[ts(1_000_000), ts(1_010_000)]);
		write_marker_group(&mut track, 3, ts(1_020_000));
		write_group(&mut track, 4, &[ts(2_000_000), ts(2_010_000)]);
		let _track = track.finish().unwrap();
		assert_eq!(
			items(&mut reader).await,
			[
				// Nothing was read before the join, so it is no gap. Legacy carries no duration, so
				// the hole starts at the last frame read.
				"0",
				"10000",
				"gap 10000+990000",
				"1000000",
				"1010000",
				"end 1020000",
				"2000000",
				"2010000"
			]
		);
	}

	/// Starts are timestamps, so keys lie between any two groups. A run the publisher declared
	/// skipped holds nothing, so the reader steps over it without a gap even where the content
	/// jumps; one it never declared may have held a group that was lost.
	#[tokio::test(start_paused = true)]
	async fn only_an_undeclared_run_between_groups_is_a_gap() {
		let mut track = track_producer("gaps", hang::container::track_info(hang::catalog::PRIORITY.audio));
		let subscription =
			track.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_secs(10)),
						max: Some(Duration::from_secs(10)),
					},
				)]),
			);
		let mut reader = Consumer::new(subscription, Container::Legacy(crate::container::Kind::Audio));
		write_group(&mut track, 0, &[ts(0), ts(10_000)]);
		track.skip_groups(1..1_000_000);
		write_group(&mut track, 1_000_000, &[ts(1_000_000), ts(1_010_000)]);
		write_group(&mut track, 2_000_000, &[ts(2_000_000), ts(2_010_000)]);
		let _track = track.finish().unwrap();
		assert_eq!(
			items(&mut reader).await,
			[
				"0",
				"10000",
				"1000000",
				"1010000",
				"gap 1010000+990000",
				"2000000",
				"2010000"
			]
		);
	}

	/// Legacy frames carry no duration, so an audio reader reads each packet's from its codec.
	/// One-packet groups then run back to back even where nothing declared the starts between
	/// them skipped, as a moq-lite-06 hop can't, while a lost group is still a gap.
	#[tokio::test(start_paused = true)]
	async fn codec_durations_bridge_undeclared_runs_between_audio_groups() {
		let track = track_producer("gaps", hang::container::track_info(hang::catalog::PRIORITY.audio));
		let subscription =
			track.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_secs(10)),
						max: Some(Duration::from_secs(10)),
					},
				)]),
			);
		let config = hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Opus, 48_000, 2);
		let mut reader = Consumer::new(subscription, Container::try_from(&config).unwrap());
		// One 20 ms CELT packet per group. The group starting at 40 ms never arrives.
		for start in [0, 20_000, 60_000] {
			let mut group = track.create_group(moq_net::group::Info { start }).unwrap();
			let packet = Frame {
				timestamp: ts(start),
				payload: Bytes::from_static(&[31 << 3, 0]),
				keyframe: false,
				duration: None,
			};
			Container::try_from(&config)
				.unwrap()
				.write(&mut group, &[packet])
				.unwrap();
			group.finish().unwrap();
		}
		let _track = track.finish().unwrap();
		assert_eq!(items(&mut reader).await, ["0", "20000", "gap 40000+20000", "60000"]);
	}

	/// MP2 codes 1152 samples per frame, so its groups bridge as Opus's do.
	#[tokio::test(start_paused = true)]
	async fn mp2_durations_bridge_undeclared_runs_between_audio_groups() {
		let track = track_producer("gaps", hang::container::track_info(hang::catalog::PRIORITY.audio));
		let subscription =
			track.subscribe(moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::ALL]));
		let config = hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Mp2, 48_000, 2);
		let mut reader = Consumer::new(subscription, Container::try_from(&config).unwrap());
		// Two 24 ms frames per group. The group starting at 96 ms never arrives.
		for start in [0, 48_000, 144_000] {
			let mut group = track.create_group(moq_net::group::Info { start }).unwrap();
			for at in [start, start + 24_000] {
				let packet = Frame {
					timestamp: ts(at),
					payload: Bytes::from_static(&[0xFF, 0xFD]),
					keyframe: at == start,
					duration: None,
				};
				Container::try_from(&config)
					.unwrap()
					.write(&mut group, &[packet])
					.unwrap();
			}
			group.finish().unwrap();
		}
		let _track = track.finish().unwrap();
		assert_eq!(
			items(&mut reader).await,
			["0", "24000", "48000", "72000", "gap 96000+48000", "144000", "168000"]
		);
	}

	/// Legacy video groups, each keyed by its first frame's timestamp as a moq-lite-06 hop delivers
	/// them with nothing declared between them: the frames of each, in decode order, with no duration.
	async fn legacy_video(groups: &[&[u64]]) -> Vec<String> {
		let track = track_producer("gaps", hang::container::track_info(hang::catalog::PRIORITY.video));
		let subscription =
			track.subscribe(moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::ALL]));
		let wire = Container::Legacy(crate::container::Kind::Video);
		let mut reader = Consumer::new(subscription, Container::Legacy(crate::container::Kind::Video));
		for pictures in groups {
			let start = pictures[0];
			let mut group = track.create_group(moq_net::group::Info { start }).unwrap();
			for &micros in *pictures {
				let frame = Frame {
					timestamp: ts(micros),
					payload: Bytes::from_static(&[0xDE, 0xAD]),
					keyframe: micros == start,
					duration: None,
				};
				wire.write(&mut group, &[frame]).unwrap();
			}
			group.finish().unwrap();
		}
		let _track = track.finish().unwrap();
		items(&mut reader).await
	}

	/// The second group is an open GOP: its leading pictures present between the first group's last
	/// picture and its keyframe, so the two groups meet and nothing is lost.
	#[tokio::test(start_paused = true)]
	async fn an_open_gop_that_continues_the_previous_video_group_is_no_gap() {
		assert_eq!(
			legacy_video(&[&[0, 120_000, 40_000, 80_000], &[280_000, 160_000, 200_000, 240_000]]).await,
			["0", "120000", "40000", "80000", "280000", "160000", "200000", "240000"]
		);
	}

	/// A closed GOP with B-frames starts one picture past the previous group's highest timestamp,
	/// which is where its last picture starts rather than ends.
	#[tokio::test(start_paused = true)]
	async fn a_closed_gop_one_picture_past_the_previous_video_group_is_no_gap() {
		assert_eq!(
			legacy_video(&[&[0, 120_000, 40_000, 80_000], &[160_000, 280_000, 200_000, 240_000]]).await,
			["0", "120000", "40000", "80000", "160000", "280000", "200000", "240000"]
		);
	}

	/// Field-coded open GOPs: the keyframe's second field decodes ahead of the leading pictures, so
	/// the lowest one comes third, within how far the first group reordered. A 40 ms frame among the
	/// second group's 20 ms fields allows its last picture to be one too.
	#[tokio::test(start_paused = true)]
	async fn field_coded_open_gops_that_continue_each_other_are_no_gap() {
		let groups: &[&[u64]] = &[
			&[0, 20_000, 120_000, 140_000, 40_000, 60_000, 80_000, 100_000],
			&[240_000, 260_000, 160_000, 180_000, 220_000],
			&[380_000, 400_000, 300_000, 320_000, 340_000, 360_000],
		];
		let read = legacy_video(groups).await;
		assert!(!read.iter().any(|item| item.starts_with("gap")), "{read:?}");
		assert_eq!(read.len(), 19);
	}

	/// A lost group is still a gap, read ahead of the next group's keyframe, though its leading pictures
	/// would have presented nearer: the keyframe waits for them.
	#[tokio::test(start_paused = true)]
	async fn an_open_gop_past_a_lost_video_group_is_a_gap_before_its_keyframe() {
		assert_eq!(
			legacy_video(&[
				&[0, 120_000, 40_000, 80_000],
				&[280_000, 160_000, 200_000, 240_000],
				// The group at 560 ms never arrives.
				&[840_000, 720_000, 760_000, 800_000],
			])
			.await,
			[
				"0",
				"120000",
				"40000",
				"80000",
				"280000",
				"160000",
				"200000",
				"240000",
				"gap 280000+560000",
				"840000",
				"720000",
				"760000",
				"800000"
			]
		);
	}

	/// The keyframe of an open GOP is held only until a frame shows the group continues, so it reads
	/// while the rest of its group is still to come.
	#[tokio::test(start_paused = true)]
	async fn an_open_gops_keyframe_reads_once_a_leading_picture_lands() {
		let track = track_producer("gaps", hang::container::track_info(hang::catalog::PRIORITY.video));
		let subscription =
			track.subscribe(moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::ALL]));
		let wire = Container::Legacy(crate::container::Kind::Video);
		let mut reader = Consumer::new(subscription, Container::Legacy(crate::container::Kind::Video));
		let frame = |micros: u64, keyframe| Frame {
			timestamp: ts(micros),
			payload: Bytes::from_static(&[0xDE, 0xAD]),
			keyframe,
			duration: None,
		};
		let mut first = track.create_group(moq_net::group::Info { start: 0 }).unwrap();
		for (micros, keyframe) in [(0, true), (120_000, false), (40_000, false), (80_000, false)] {
			wire.write(&mut first, &[frame(micros, keyframe)]).unwrap();
		}
		first.finish().unwrap();
		for _ in 0..4 {
			assert!(matches!(reader.read().await.unwrap(), Some(Read::Frame { .. })));
		}
		let mut open = track.create_group(moq_net::group::Info { start: 280_000 }).unwrap();
		wire.write(&mut open, &[frame(280_000, true)]).unwrap();
		let waiter = kio::Waiter::noop();
		assert!(
			reader.poll_read(&waiter).is_pending(),
			"the keyframe waits for the next frame"
		);
		wire.write(&mut open, &[frame(160_000, false)]).unwrap();
		let Some(Read::Frame { frame: keyframe, .. }) = reader.read().await.unwrap() else {
			panic!("the keyframe reads, with no gap ahead of it");
		};
		assert_eq!(keyframe.timestamp, ts(280_000));
		let Some(Read::Frame { frame: leading, .. }) = reader.read().await.unwrap() else {
			panic!("the leading picture reads");
		};
		assert_eq!(leading.timestamp, ts(160_000));
		drop(open);
	}

	/// A held keyframe is judged by what its group holds once the max buffer fills, and read behind a
	/// gap rather than dropped with the group: the pressure skip moves past only a group holding nothing.
	#[tokio::test(start_paused = true)]
	async fn a_held_keyframe_is_read_once_the_max_buffer_fills() {
		let mut track = track_producer("gaps", hang::container::track_info(hang::catalog::PRIORITY.video));
		let subscription =
			track.subscribe(moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::ALL]));
		let wire = Container::Legacy(crate::container::Kind::Video);
		let mut reader = Consumer::new(subscription, Container::Legacy(crate::container::Kind::Video));
		let frame = |micros: u64, keyframe| Frame {
			timestamp: ts(micros),
			payload: Bytes::from_static(&[0xDE, 0xAD]),
			keyframe,
			duration: None,
		};
		let mut first = track.create_group(moq_net::group::Info { start: 0 }).unwrap();
		for (micros, keyframe) in [(0, true), (120_000, false), (40_000, false), (80_000, false)] {
			wire.write(&mut first, &[frame(micros, keyframe)]).unwrap();
		}
		first.finish().unwrap();
		for _ in 0..4 {
			assert!(matches!(reader.read().await.unwrap(), Some(Read::Frame { .. })));
		}
		// The open GOP's keyframe waits for a frame to show whether it continues the first group.
		let mut open = track.create_group(moq_net::group::Info { start: 280_000 }).unwrap();
		wire.write(&mut open, &[frame(280_000, true)]).unwrap();
		assert!(reader.poll_read(&kio::Waiter::noop()).is_pending());
		// A later group's first frame, decoded while the keyframe waits, fills the max buffer before
		// that frame lands. Its handle alone leaves room.
		reader.set_max_buffer(reader.buffered_bytes() + std::mem::size_of::<GroupBuffer<Container>>() + 1);
		write_group(&mut track, 560_000, &[ts(560_000)]);
		let Some(Read::Gap { timestamp, duration }) = reader.read().await.unwrap() else {
			panic!("the step reads as a gap, judged by the keyframe alone");
		};
		assert_eq!((timestamp, duration), (ts(120_000), ts(160_000)));
		let Some(Read::Frame { frame: keyframe, .. }) = reader.read().await.unwrap() else {
			panic!("the held keyframe reads rather than drop with its group");
		};
		assert_eq!(keyframe.timestamp, ts(280_000));
		drop(open);
	}

	/// A group the cursor stepped onto that holds nothing yet is skipped once the later groups fill the
	/// max buffer, as before it held anything to judge.
	#[tokio::test(start_paused = true)]
	async fn an_empty_unproven_group_is_skipped_once_the_max_buffer_fills() {
		let mut track = track_producer("gaps", hang::container::track_info(hang::catalog::PRIORITY.video));
		let subscription =
			track.subscribe(moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::ALL]));
		let mut reader = Consumer::new(subscription, Container::Legacy(crate::container::Kind::Video));
		write_group(&mut track, 0, &[ts(0), ts(40_000)]);
		for _ in 0..2 {
			assert!(matches!(reader.read().await.unwrap(), Some(Read::Frame { .. })));
		}
		let open = track.create_group(moq_net::group::Info { start: 280_000 }).unwrap();
		assert!(reader.poll_read(&kio::Waiter::noop()).is_pending());
		reader.set_max_buffer(reader.buffered_bytes() + std::mem::size_of::<GroupBuffer<Container>>() + 1);
		write_group(&mut track, 560_000, &[ts(560_000)]);
		assert!(matches!(reader.read().await.unwrap(), Some(Read::Gap { .. })));
		let Some(Read::Frame { frame, .. }) = reader.read().await.unwrap() else {
			panic!("the later group reads");
		};
		assert_eq!(frame.timestamp, ts(560_000));
		drop(open);
	}

	#[kio::test]
	async fn a_latency_skip_is_a_gap() {
		let mut track = track_producer("gaps", hang::container::track_info(hang::catalog::PRIORITY.audio));
		let subscription =
			track.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_secs(10)),
						max: Some(Duration::from_secs(10)),
					},
				)]),
			);
		let mut reader = Consumer::new(subscription, Container::Legacy(crate::container::Kind::Audio));
		reader.max_delay = Duration::ZERO;
		// Group 0 stays open, and is read before the newer group lands: the skip comes after the start.
		let mut group = track.create_group(moq_net::group::Info { start: 0 }).unwrap();
		Container::Legacy(crate::container::Kind::Audio)
			.write(
				&mut group,
				&[Frame {
					timestamp: ts(0),
					payload: Bytes::from_static(&[0xDE, 0xAD]),
					keyframe: false,
					duration: None,
				}],
			)
			.unwrap();
		assert!(matches!(reader.read().await.unwrap(), Some(Read::Frame { .. })));
		write_group(&mut track, 3, &[ts(1_000_000)]);
		let _track = track.finish().unwrap();
		assert_eq!(items(&mut reader).await, ["gap 0+1000000", "1000000"]);
	}

	/// A video group's trailing duration marker is that frame's end, not a declared stop.
	#[kio::test]
	async fn a_trailing_duration_marker_is_not_an_end() {
		let track = track_producer("gaps", hang::container::track_info(hang::catalog::PRIORITY.video));
		let subscription =
			track.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_secs(10)),
						max: Some(Duration::from_secs(10)),
					},
				)]),
			);
		let mut reader = Consumer::new(subscription, Container::Legacy(crate::container::Kind::Video));
		let wire = Container::Legacy(crate::container::Kind::Video);
		for (sequence, start) in [(0, 0), (1, 33_000)] {
			let mut group = track.create_group(moq_net::group::Info { start: sequence }).unwrap();
			let frame = |timestamp: u64, payload| Frame {
				timestamp: ts(timestamp),
				payload,
				keyframe: timestamp == start,
				duration: None,
			};
			wire.write(
				&mut group,
				&[
					frame(start, Bytes::from_static(&[1])),
					frame(start + 33_000, Bytes::new()),
				],
			)
			.unwrap();
			group.finish().unwrap();
		}
		let _track = track.finish().unwrap();
		assert_eq!(items(&mut reader).await, ["0", "33000"]);
	}

	/// An audio terminal opens its group with the endpoint, then the codec's final packets.
	#[kio::test]
	async fn an_audio_terminal_reads_its_end_before_the_final_packets() {
		let mut track = track_producer("gaps", hang::container::track_info(hang::catalog::PRIORITY.audio));
		let subscription =
			track.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_secs(10)),
						max: Some(Duration::from_secs(10)),
					},
				)]),
			);
		let mut reader = Consumer::new(subscription, Container::Legacy(crate::container::Kind::Audio));
		write_group(&mut track, 0, &[ts(0), ts(20_000)]);
		let mut group = track.create_group(moq_net::group::Info { start: 1 }).unwrap();
		let frame = |timestamp: u64, payload| Frame {
			timestamp: ts(timestamp),
			payload,
			keyframe: false,
			duration: None,
		};
		Container::Legacy(crate::container::Kind::Audio)
			.write(
				&mut group,
				&[frame(50_000, Bytes::new()), frame(40_000, Bytes::from_static(&[1]))],
			)
			.unwrap();
		group.finish().unwrap();
		let _track = track.finish().unwrap();
		assert_eq!(items(&mut reader).await, ["0", "20000", "end 50000", "40000"]);
	}

	#[kio::test]
	async fn a_marker_group_declares_an_end() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.audio));
		let consumer_track =
			track.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_secs(2)),
						max: Some(Duration::from_secs(2)),
					},
				)]),
			);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Audio));

		write_group(&mut track, 0, &[ts(0)]);
		write_marker_group(&mut track, 1, ts(10_000));
		write_group(&mut track, 2, &[ts(1_000_000)]);
		let _track = track.finish().unwrap();

		assert_eq!(items(&mut consumer).await, ["0", "end 10000", "1000000"]);
	}

	/// Missing media reads as a gap; only the publisher can declare an end.
	#[kio::test]
	async fn only_a_marker_declares_an_end() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.audio));
		let consumer_track =
			track.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_secs(2)),
						max: Some(Duration::from_secs(2)),
					},
				)]),
			);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Audio));

		write_group(&mut track, 0, &[ts(0)]);
		// Group 1 never arrives.
		write_group(&mut track, 2, &[ts(1_000_000)]);
		write_marker_group(&mut track, 3, ts(1_010_000));
		write_group(&mut track, 4, &[ts(2_000_000)]);
		let _track = track.finish().unwrap();

		assert_eq!(
			items(&mut consumer).await,
			["0", "gap 0+1000000", "1000000", "end 1010000", "2000000"]
		);
	}

	#[kio::test]
	async fn empty_groups_mean_nothing() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.audio));
		let consumer_track =
			track.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_secs(2)),
						max: Some(Duration::from_secs(2)),
					},
				)]),
			);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Audio));

		write_group(&mut track, 0, &[ts(0)]);
		track
			.create_group(moq_net::group::Info { start: 1 })
			.unwrap()
			.finish()
			.unwrap();
		write_group(&mut track, 2, &[ts(1_000)]);
		let _track = track.finish().unwrap();

		assert_eq!(items(&mut consumer).await, ["0", "1000"]);
	}

	/// A live-edge reader positioned on the largest group while it is still empty does not
	/// stall there: once a budget of newer media lands, it moves on like any stalled group.
	#[kio::test]
	async fn a_live_edge_start_on_an_empty_group_moves_on() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.audio));
		write_group(&mut track, 0, &[ts(0)]);
		let _empty = track.create_group(moq_net::group::Info { start: 1 }).unwrap();

		let mut subscriber =
			track.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_millis(500)),
						max: Some(Duration::from_millis(500)),
					},
				)]),
			);
		let edge = match subscriber.poll_largest(&kio::Waiter::noop()) {
			std::task::Poll::Ready(Some(edge)) => edge,
			_ => panic!("the empty group is the largest"),
		};
		assert_eq!(edge.group, 1);
		subscriber = subscriber.with_range(edge.group..);
		let mut consumer = Consumer::new(subscriber, Container::Legacy(crate::container::Kind::Audio));

		write_group(&mut track, 2, &[ts(1_000_000), ts(1_600_000)]);
		let _track = track.finish().unwrap();

		assert_eq!(items(&mut consumer).await, ["1000000", "1600000"]);
	}

	#[kio::test]
	async fn latency_skip_reports_a_gap() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.audio));
		let consumer_track =
			track.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_secs(2)),
						max: Some(Duration::from_secs(2)),
					},
				)]),
			);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Audio));
		// Keep transport filtering out of this test so it isolates the mux skip logic.
		consumer.max_delay = Duration::ZERO;

		// Group 0 stays open, and is read before the newer group lands: the skip comes after the start.
		let mut group = track.create_group(moq_net::group::Info { start: 0 }).unwrap();
		Container::Legacy(crate::container::Kind::Audio)
			.write(
				&mut group,
				&[Frame {
					timestamp: ts(0),
					payload: Bytes::from_static(&[0xDE, 0xAD]),
					keyframe: false,
					duration: None,
				}],
			)
			.unwrap();
		assert!(matches!(consumer.read().await.unwrap(), Some(Read::Frame { .. })));
		write_group(&mut track, 3, &[ts(1_000_000)]);
		let _track = track.finish().unwrap();

		assert_eq!(items(&mut consumer).await, ["gap 0+1000000", "1000000"]);
	}

	#[kio::test]
	async fn read_multiple_frames_single_group() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_millis(500)),
						max: Some(Duration::from_millis(500)),
					},
				)]),
			);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		write_group(&mut track, 0, &[ts(0), ts(33_000), ts(66_000)]);
		let _track = track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 3);
		assert_eq!(frames[0].timestamp, ts(0));
		assert_eq!(frames[1].timestamp, ts(33_000));
		assert_eq!(frames[2].timestamp, ts(66_000));

		assert!(frames[0].keyframe);
	}

	#[kio::test]
	async fn read_multiple_groups_within_latency() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_millis(500)),
						max: Some(Duration::from_millis(500)),
					},
				)]),
			);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		// 5 groups, 20ms spacing. Total span = 80ms, well within the 500ms max delay.
		for i in 0..5u64 {
			write_group(&mut track, i, &[ts(i * 20_000)]);
		}
		let _track = track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 5);
	}

	// ---- Age Skipping ----

	#[tokio::test]
	async fn latency_skip_delivers_recent_groups() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track = track.subscribe(crate::container::test_util::within(Duration::from_millis(100)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		// Group 0: 5 frames, NOT finished (blocks consumer)
		let mut group0 = track.create_group(moq_net::group::Info { start: 0 }).unwrap();
		for f in 0..5u64 {
			Container::Legacy(crate::container::Kind::Data)
				.write(
					&mut group0,
					&[Frame {
						timestamp: ts(f * 2_000),
						payload: Bytes::from_static(&[0xDE, 0xAD]),
						keyframe: false,
						duration: None,
					}],
				)
				.unwrap();
		}

		// Groups 1-19: finished, 15ms spacing, 5 frames each
		for g in 1..20u64 {
			let timestamps: Vec<_> = (0..5).map(|f| ts(g * 15_000 + f * 2_000)).collect();
			write_group(&mut track, g, &timestamps);
		}
		let _track = track.finish().unwrap();

		// Finish group 0 after consumer has had time to accumulate pending groups
		let finisher = tokio::spawn(async move {
			kio::clock::sleep(Duration::from_millis(50)).await;
			group0.finish().unwrap();
		});

		let frames = read_all(&mut consumer).await.unwrap();
		// Group 0's 5 frames + some later groups (earlier ones skipped by age)
		assert!(frames.len() >= 25, "Expected >= 25 frames, got {}", frames.len());
		finisher.await.expect("finisher task panicked");
	}

	#[tokio::test]
	async fn zero_latency_skips_aggressively() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track = track.subscribe(None);
		let mut consumer = container_max_delay_only(consumer_track, Duration::ZERO);

		// Group 0 at ts 0 keeps timestamps monotonic with sequence (groups 1-9 follow at
		// g*50 ms), so the test exercises age skipping and not rewind detection.
		let mut group0 = track.create_group(moq_net::group::Info { start: 0 }).unwrap();
		Container::Legacy(crate::container::Kind::Data)
			.write(
				&mut group0,
				&[Frame {
					timestamp: ts(0),
					payload: Bytes::from_static(&[0xDE, 0xAD]),
					keyframe: false,
					duration: None,
				}],
			)
			.unwrap();

		// Read group 0's frame before the later groups land, so they skip a stalled group rather
		// than move the start.
		let first = consumer.read_frame().await.unwrap().unwrap();
		assert_eq!(first.timestamp, ts(0));

		for g in 1..10u64 {
			let timestamps: Vec<_> = (0..3).map(|f| ts(g * 50_000 + f * 5_000)).collect();
			write_group(&mut track, g, &timestamps);
		}
		let _track = track.finish().unwrap();

		let finisher = tokio::spawn(async move {
			kio::clock::sleep(Duration::from_millis(50)).await;
			group0.finish().unwrap();
		});

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 27, "Expected groups 1-9 after group 0's frame");
		assert!(!frames.is_empty(), "Expected at least some frames");
		finisher.await.expect("finisher task panicked");
	}

	#[tokio::test]
	async fn latency_skip_correctness() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track = track.subscribe(None);
		let mut consumer = container_max_delay_only(consumer_track, Duration::from_millis(100));

		let mut group0 = track.create_group(moq_net::group::Info { start: 0 }).unwrap();
		Container::Legacy(crate::container::Kind::Data)
			.write(
				&mut group0,
				&[Frame {
					timestamp: ts(0),
					payload: Bytes::from_static(&[0xDE, 0xAD]),
					keyframe: false,
					duration: None,
				}],
			)
			.unwrap();

		// Read group 0's frame before the later groups land, so they skip a stalled group rather
		// than move the start.
		let first = consumer.read_frame().await.unwrap().unwrap();
		assert_eq!(first.timestamp, ts(0));

		for g in 1..10u64 {
			write_group(&mut track, g, &[ts(g * 30_000)]);
		}
		let _track = track.finish().unwrap();

		let finisher = tokio::spawn(async move {
			kio::clock::sleep(Duration::from_millis(50)).await;
			group0.finish().unwrap();
		});

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 9, "Expected groups 1-9 after group 0's frame");
		for (i, frame) in (1..10u64).zip(&frames) {
			assert_eq!(frame.timestamp, ts(i * 30_000));
		}
		finisher.await.expect("finisher task panicked");
	}

	/// A group longer than the budget that blocks for a moment behind a newer group is
	/// not cut short while what it could still present (up to where the newer group
	/// begins) is within the budget: the newer group races ahead on its own stream while
	/// the older one is still arriving.
	#[kio::test]
	async fn a_long_group_blocked_behind_a_newer_one_is_not_cut_short() {
		let waiter = kio::Waiter::noop();
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let mut consumer = container_max_delay_only(track.subscribe(None), Duration::from_secs(2));
		let write = |group: &mut moq_net::group::Producer, timestamp: Timestamp| {
			let frame = Frame {
				timestamp,
				payload: Bytes::from_static(&[0xDE, 0xAD]),
				keyframe: false,
				duration: None,
			};
			Container::Legacy(crate::container::Kind::Data)
				.write(group, &[frame])
				.unwrap();
		};
		let read = |consumer: &mut Consumer<Container>| loop {
			match consumer.poll_read(&waiter) {
				Poll::Ready(Ok(Some(Read::Frame { frame, .. }))) => break Some(frame.timestamp),
				Poll::Ready(Ok(Some(Read::Gap { .. }))) => continue,
				Poll::Pending => break None,
				other => panic!("unexpected read: {other:?}"),
			}
		};

		// A 2.5 s group, half arrived, and the next one already 0.5 s in. The reader started
		// before the next one landed, so this is a stall, not a join's backlog, which starts
		// within the budget instead (see `a_buffered_backlog_starts_within_the_budget`).
		let mut group0 = track.create_group(moq_net::group::Info { start: 0 }).unwrap();
		write(&mut group0, ts(0));
		write(&mut group0, ts(500_000));
		assert_eq!(read(&mut consumer), Some(ts(0)));
		let mut group1 = track.create_group(moq_net::group::Info { start: 1 }).unwrap();
		write(&mut group1, ts(2_500_000));
		write(&mut group1, ts(3_000_000));

		assert_eq!(read(&mut consumer), Some(ts(500_000)));
		// Group 0's reach (2.5 s) is 0.5 s behind the newest frame: wait for it.
		assert_eq!(read(&mut consumer), None, "cut group 0 short");
		write(&mut group0, ts(1_000_000));
		assert_eq!(read(&mut consumer), Some(ts(1_000_000)));
		group0.finish().unwrap();
		assert_eq!(read(&mut consumer), Some(ts(2_500_000)));

		// Once the reach falls a full budget behind, the stalled group is skipped.
		let mut group2 = track.create_group(moq_net::group::Info { start: 2 }).unwrap();
		write(&mut group2, ts(5_000_000));
		write(&mut group2, ts(7_000_000));
		assert_eq!(read(&mut consumer), Some(ts(3_000_000)));
		assert_eq!(read(&mut consumer), Some(ts(5_000_000)));
		group1.finish().unwrap();
		group2.finish().unwrap();
	}

	/// A backlog that lands whole before the first read starts at the first group whose first
	/// frame is within the budget of the newest one, though nothing blocks. Five one-second
	/// groups of ten frames and a one-frame sixth at 5 s, with a 1.5 s budget, start at the 4 s
	/// group, whether the sixth is open or finished. `js/hang`'s `consumer.skips.test.ts` pairs
	/// this case.
	#[kio::test]
	async fn a_buffered_backlog_starts_within_the_budget() {
		for open in [true, false] {
			let waiter = kio::Waiter::noop();
			let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
			let mut consumer = container_max_delay_only(track.subscribe(None), Duration::from_millis(1500));
			for g in 0..5u64 {
				let timestamps: Vec<_> = (0..10).map(|f| ts(g * 1_000_000 + f * 100_000)).collect();
				write_group(&mut track, g, &timestamps);
			}
			let mut last = track.create_group(moq_net::group::Info { start: 5 }).unwrap();
			Container::Legacy(crate::container::Kind::Data)
				.write(
					&mut last,
					&[Frame {
						timestamp: ts(5_000_000),
						payload: Bytes::from_static(&[0xDE, 0xAD]),
						keyframe: false,
						duration: None,
					}],
				)
				.unwrap();
			if !open {
				last.finish().unwrap();
			}

			let mut read = Vec::new();
			while let Poll::Ready(Ok(Some(item))) = consumer.poll_read(&waiter) {
				match item {
					Read::Frame { frame, .. } => read.push(frame.timestamp),
					other => panic!("unexpected read: {other:?}"),
				}
			}
			let expected: Vec<_> = (0..10)
				.map(|f| ts(4_000_000 + f * 100_000))
				.chain([ts(5_000_000)])
				.collect();
			assert_eq!(read, expected, "sixth group open: {open}");
		}
	}

	/// A backlog join's start skip waits on the next group's first frame, which may land within the
	/// budget, rather than skipping on to a later group with data and dropping it. Here the fifth
	/// group (4.0 s) arrived with its frames still in flight, so the start keeps the fourth and
	/// plays on into the fifth. `js/hang`'s `consumer.skips.test.ts` pairs this case.
	#[kio::test]
	async fn a_backlog_start_waits_on_a_successor_without_frames() {
		let waiter = kio::Waiter::noop();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let mut consumer = container_max_delay_only(track.subscribe(None), Duration::from_millis(1500));
		let frames = |g: u64| -> Vec<_> { (0..10).map(|f| ts(g * 1_000_000 + f * 100_000)).collect() };
		for g in 0..4u64 {
			write_group(&mut track, g, &frames(g));
		}
		let mut fifth = track.create_group(moq_net::group::Info { start: 4 }).unwrap();
		write_group(&mut track, 5, &[ts(5_000_000)]);

		let mut read = Vec::new();
		let mut drain = |consumer: &mut Consumer<Container>| {
			while let Poll::Ready(Ok(Some(item))) = consumer.poll_read(&waiter) {
				match item {
					Read::Frame { frame, .. } => read.push(frame.timestamp),
					other => panic!("unexpected read: {other:?}"),
				}
			}
		};
		drain(&mut consumer);
		for frame in frames(4) {
			Container::Legacy(crate::container::Kind::Data)
				.write(
					&mut fifth,
					&[Frame {
						timestamp: frame,
						payload: Bytes::from_static(&[0xDE, 0xAD]),
						keyframe: false,
						duration: None,
					}],
				)
				.unwrap();
		}
		fifth.finish().unwrap();
		drain(&mut consumer);

		let expected: Vec<_> = frames(3).into_iter().chain(frames(4)).chain([ts(5_000_000)]).collect();
		assert_eq!(read, expected);
	}

	// ---- Malformed rewind ----

	/// The track refuses a group whose first frame rewinds, but not a later frame that does,
	/// so the consumer floors every frame against the previous group's start.
	#[kio::test]
	async fn a_frame_below_the_previous_start_aborts() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let mut consumer = Consumer::new(track.subscribe(None), Container::Legacy(crate::container::Kind::Data));
		write_group(&mut track, 0, &[ts(100_000)]);
		assert_eq!(consumer.read_frame().await.unwrap().unwrap().timestamp, ts(100_000));
		write_group(&mut track, 1, &[ts(133_000), ts(0)]);
		let _track = track.finish().unwrap();
		let err = loop {
			match consumer.read().await {
				Ok(Some(_)) => continue,
				Ok(None) => panic!("the rewound frame ended the track cleanly"),
				Err(err) => break err,
			}
		};
		assert!(matches!(
			err,
			crate::Error::TimestampRewind(TimestampRewind { timestamp, floor })
				if timestamp == ts(0) && floor == ts(100_000)
		));
		assert_eq!(
			err.to_string(),
			"frame timestamp 0 µs is below the previous group's start 100000 µs"
		);
	}

	#[kio::test]
	async fn b_frames_within_a_group_are_accepted() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let mut consumer = Consumer::new(track.subscribe(None), Container::Legacy(crate::container::Kind::Data));
		write_group(&mut track, 0, &[ts(0), ts(66_000), ts(33_000)]);
		let _track = track.finish().unwrap();
		let frames = continuous_frames(&mut consumer).await;
		assert_eq!(
			frames.iter().map(|f| f.timestamp).collect::<Vec<_>>(),
			vec![ts(0), ts(66_000), ts(33_000)]
		);
	}

	#[kio::test]
	async fn open_gop_leading_pictures_above_the_previous_group_are_accepted() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let mut consumer = Consumer::new(track.subscribe(None), Container::Legacy(crate::container::Kind::Data));
		write_group(&mut track, 0, &[ts(0), ts(33_000)]);
		// Read the first group before the second arrives: a superseded group is skipped
		// for latency, which is not what this checks.
		for expected in [0, 33_000] {
			assert_eq!(consumer.read_frame().await.unwrap().unwrap().timestamp, ts(expected));
		}
		write_group(&mut track, 1, &[ts(66_000), ts(50_000)]);
		let _track = track.finish().unwrap();
		let frames = continuous_frames(&mut consumer).await;
		assert_eq!(
			frames
				.iter()
				.map(|f| f.timestamp.as_micros().unwrap())
				.collect::<Vec<_>>(),
			vec![66_000, 50_000]
		);
	}

	#[kio::test]
	async fn a_marker_group_and_forward_jump_reads_one_end() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.audio));
		let mut consumer = Consumer::new(
			track.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_secs(2)),
						max: Some(Duration::from_secs(2)),
					},
				)]),
			),
			Container::Legacy(crate::container::Kind::Audio),
		);
		write_group(&mut track, 0, &[ts(0)]);
		write_marker_group(&mut track, 1, ts(10_000));
		write_group(&mut track, 2, &[ts(1_000_000)]);
		let _track = track.finish().unwrap();
		assert_eq!(items(&mut consumer).await, ["0", "end 10000", "1000000"]);
	}

	/// A pause's timestamp jump is not lateness: the group that resumes it can arrive behind the
	/// one after it, and is waited for like any hole rather than skipped as too old.
	#[tokio::test(start_paused = true)]
	async fn the_head_after_a_declared_end_is_waited_for() {
		let mut copy = Received::with_info(hang::container::track_info(hang::catalog::PRIORITY.audio)).await;
		let mut consumer =
			Consumer::new(
				copy.subscribe_with(moq_net::track::Subscription::default().with_ranges([
					moq_net::track::Range::from(moq_net::track::Start::Live {
						min: Some(Duration::from_secs(30)),
						max: Some(Duration::from_secs(30)),
					}),
				]))
				.await,
				Container::Legacy(crate::container::Kind::Audio),
			);
		consumer.max_delay = Duration::from_millis(300);
		write_group(&mut copy.track, 0, &[ts(0)]);
		carried().await;
		// The end, then the stopped encoder's final packet, as `moq_audio`'s producer writes them.
		let mut group = copy.track.create_group(moq_net::group::Info { start: 1 }).unwrap();
		let frame = |timestamp, payload| Frame {
			timestamp,
			payload,
			keyframe: true,
			duration: None,
		};
		Container::Legacy(crate::container::Kind::Audio)
			.write(
				&mut group,
				&[
					frame(ts(60_000), Bytes::new()),
					frame(ts(40_000), Bytes::from_static(b"x")),
				],
			)
			.unwrap();
		group.finish().unwrap();
		carried().await;
		assert_eq!(consumer.read_frame().await.unwrap().unwrap().timestamp, ts(0));
		assert!(matches!(
			consumer.read().await.unwrap(),
			Some(Read::End { group: 1, .. })
		));
		assert_eq!(consumer.read_frame().await.unwrap().unwrap().timestamp, ts(40_000));
		write_group(&mut copy.track, 3, &[ts(60_060_000)]);
		carried().await;
		tokio::time::advance(Duration::from_millis(299)).await;
		carried().await;
		assert!(consumer.poll_read(&kio::Waiter::noop()).is_pending());
		write_group(&mut copy.track, 2, &[ts(60_000_000)]);
		carried().await;
		let _track = copy.track.finish().unwrap();
		assert_eq!(items(&mut consumer).await, ["60000000", "60060000"]);
	}

	#[kio::test]
	async fn a_zero_budget_idle_resume_reports_the_shed_marker_as_a_gap() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.audio));
		let mut consumer = Consumer::new(track.subscribe(None), Container::Legacy(crate::container::Kind::Audio));
		consumer.max_delay = Duration::ZERO;
		write_group(&mut track, 0, &[ts(0)]);
		assert_eq!(consumer.read_frame().await.unwrap().unwrap().timestamp, ts(0));
		write_marker_group(&mut track, 1, ts(10_000));
		write_group(&mut track, 2, &[ts(1_000_000)]);
		let _track = track.finish().unwrap();
		assert!(matches!(consumer.read().await.unwrap(), Some(Read::Gap { .. })));
		assert_eq!(consumer.read_frame().await.unwrap().unwrap().timestamp, ts(1_000_000));
	}

	#[kio::test]
	async fn a_zero_budget_skip_reports_a_contiguous_shed_marker_as_a_gap() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.audio));
		let mut consumer = Consumer::new(track.subscribe(None), Container::Legacy(crate::container::Kind::Audio));
		consumer.max_delay = Duration::ZERO;

		let mut group0 = track.create_group(moq_net::group::Info { start: 0 }).unwrap();
		Container::Legacy(crate::container::Kind::Audio)
			.write(
				&mut group0,
				&[Frame {
					timestamp: ts(0),
					payload: Bytes::from_static(&[0xDE, 0xAD]),
					keyframe: false,
					duration: None,
				}],
			)
			.unwrap();
		assert_eq!(consumer.read_frame().await.unwrap().unwrap().timestamp, ts(0));

		write_marker_group(&mut track, 1, ts(9_999));
		write_group(&mut track, 2, &[ts(10_000)]);

		assert!(matches!(consumer.read().await.unwrap(), Some(Read::Gap { .. })));
		assert_eq!(consumer.read_frame().await.unwrap().unwrap().timestamp, ts(10_000));
		group0.finish().unwrap();
	}

	#[kio::test]
	async fn a_later_frame_below_the_previous_group_start_aborts() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let mut consumer = Consumer::new(track.subscribe(None), Container::Legacy(crate::container::Kind::Data));
		write_group(&mut track, 0, &[ts(100_000)]);
		assert_eq!(consumer.read_frame().await.unwrap().unwrap().timestamp, ts(100_000));
		write_group(&mut track, 1, &[ts(200_000), ts(50_000)]);
		let _track = track.finish().unwrap();
		assert_eq!(consumer.read_frame().await.unwrap().unwrap().timestamp, ts(200_000));
		let err = consumer.read().await.unwrap_err();
		assert!(matches!(
			err,
			crate::Error::TimestampRewind(TimestampRewind { timestamp, floor })
				if timestamp == ts(50_000) && floor == ts(100_000)
		));
		assert_eq!(
			err.to_string(),
			"frame timestamp 50000 µs is below the previous group's start 100000 µs"
		);
	}

	// ---- Empty payloads ----

	/// Write one frame with an empty payload: a marker saying content stops at
	/// `timestamp`, carrying no media.
	fn write_marker(group: &mut moq_net::group::Producer, timestamp: Timestamp) {
		let frame = Frame {
			timestamp,
			payload: Bytes::new(),
			keyframe: false,
			duration: None,
		};
		Container::Legacy(crate::container::Kind::Data)
			.write(group, &[frame])
			.unwrap();
	}

	/// An empty payload carries no media, so it's skipped rather than surfaced as a
	/// frame or raised as an error. It times the previous frame and never means the
	/// track ended.
	#[kio::test]
	async fn empty_payload_is_skipped() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_millis(500)),
						max: Some(Duration::from_millis(500)),
					},
				)]),
			);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Video));

		let mut group = track.create_group(moq_net::group::Info { start: 0 }).unwrap();
		let media = |timestamp| Frame {
			timestamp,
			payload: Bytes::from_static(&[0xDE, 0xAD]),
			keyframe: false,
			duration: None,
		};
		Container::Legacy(crate::container::Kind::Video)
			.write(&mut group, &[media(ts(0))])
			.unwrap();
		write_marker(&mut group, ts(16_000)); // closes the first frame
		Container::Legacy(crate::container::Kind::Video)
			.write(&mut group, &[media(ts(33_000))])
			.unwrap();
		write_marker(&mut group, ts(50_000)); // the group's end
		group.finish().unwrap();
		let _track = track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 2, "markers are not surfaced as media");
		assert_eq!(frames[0].timestamp, ts(0));
		assert_eq!(frames[1].timestamp, ts(33_000));
	}

	#[kio::test]
	async fn leading_marker_preserves_the_first_media_keyframe() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_millis(500)),
						max: Some(Duration::from_millis(500)),
					},
				)]),
			);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Video));

		let mut group = track.create_group(moq_net::group::Info { start: 0 }).unwrap();
		write_marker(&mut group, ts(20_000));
		Container::Legacy(crate::container::Kind::Video)
			.write(
				&mut group,
				&[Frame {
					timestamp: ts(20_000),
					payload: Bytes::from_static(&[0xDE, 0xAD]),
					keyframe: false,
					duration: None,
				}],
			)
			.unwrap();
		group.finish().unwrap();
		let _track = track.finish().unwrap();

		let frame = consumer.read_frame().await.unwrap().unwrap();
		assert!(frame.keyframe, "the marker does not consume the first-media slot");
		assert!(frame.duration.is_none(), "a leading marker has no previous frame");
	}

	/// Reading a marker consumes its frame, so a run of them makes progress and the
	/// consumer reaches the next group instead of spinning. `read_all` times out per
	/// read, so a stall or an infinite loop fails this rather than hanging forever.
	#[kio::test]
	async fn consecutive_markers_do_not_stall() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_millis(500)),
						max: Some(Duration::from_millis(500)),
					},
				)]),
			);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Video));

		let mut group = track.create_group(moq_net::group::Info { start: 0 }).unwrap();
		Container::Legacy(crate::container::Kind::Video)
			.write(
				&mut group,
				&[Frame {
					timestamp: ts(0),
					payload: Bytes::from_static(&[0xDE, 0xAD]),
					keyframe: false,
					duration: None,
				}],
			)
			.unwrap();
		for i in 1..5u64 {
			write_marker(&mut group, ts(i * 1_000));
		}
		group.finish().unwrap();
		write_group(&mut track, 1, &[ts(100_000)]);
		let _track = track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		let micros: Vec<u128> = frames.iter().map(|f| f.timestamp.as_micros().unwrap()).collect();
		assert_eq!(micros, vec![0, 100_000], "markers skipped, next group reached");
	}

	/// LOC video consumers skip an empty payload: it is the duration marker, not media.
	#[kio::test]
	async fn loc_empty_payload_is_skipped() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_millis(500)),
						max: Some(Duration::from_millis(500)),
					},
				)]),
			);
		let mut consumer = Consumer::new(consumer_track, Container::Loc(crate::container::Kind::Video));

		let mut group = track.create_group(moq_net::group::Info { start: 0 }).unwrap();
		let media = Frame {
			timestamp: ts(0),
			payload: Bytes::from_static(&[0xDE, 0xAD]),
			keyframe: true,
			duration: None,
		};
		Container::Loc(crate::container::Kind::Video)
			.write(&mut group, &[media])
			.unwrap();
		Container::Loc(crate::container::Kind::Video)
			.write(
				&mut group,
				&[Frame {
					timestamp: ts(33_000),
					payload: Bytes::new(),
					keyframe: false,
					duration: None,
				}],
			)
			.unwrap();
		group.finish().unwrap();
		let _track = track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 1, "the empty LOC payload is not submitted");
		assert_eq!(frames[0].timestamp, ts(0));
	}

	// ---- Group Ordering ----

	#[tokio::test]
	async fn groups_delivered_in_sequence_order() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_millis(500)),
						max: Some(Duration::from_millis(500)),
					},
				)]),
			);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		let mut group0 = track.create_group(moq_net::group::Info { start: 0 }).unwrap();
		Container::Legacy(crate::container::Kind::Data)
			.write(
				&mut group0,
				&[Frame {
					timestamp: ts(0),
					payload: Bytes::from_static(&[0xDE, 0xAD]),
					keyframe: false,
					duration: None,
				}],
			)
			.unwrap();

		write_group(&mut track, 2, &[ts(60_000)]);
		write_group(&mut track, 1, &[ts(30_000)]);
		let _track = track.finish().unwrap();

		let finisher = tokio::spawn(async move {
			kio::clock::sleep(Duration::from_millis(10)).await;
			group0.finish().unwrap();
		});

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 3);
		assert_eq!(frames[0].timestamp, ts(0));
		assert_eq!(frames[1].timestamp, ts(30_000));
		assert_eq!(frames[2].timestamp, ts(60_000));
		finisher.await.expect("finisher task panicked");
	}

	#[kio::test]
	async fn adjacent_group_flushed_immediately() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_millis(500)),
						max: Some(Duration::from_millis(500)),
					},
				)]),
			);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		write_group(&mut track, 0, &[ts(0)]);
		write_group(&mut track, 1, &[ts(30_000)]);
		let _track = track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 2);
		assert_eq!(frames[0].timestamp, ts(0));
		assert_eq!(frames[1].timestamp, ts(30_000));
	}

	// ---- Track Lifecycle ----

	#[tokio::test]
	async fn empty_track_returns_none() {
		tokio::time::pause();
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_millis(500)),
						max: Some(Duration::from_millis(500)),
					},
				)]),
			);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		let _track = track.finish().unwrap();

		let result = kio::clock::timeout(Duration::from_millis(200), consumer.read()).await;
		match result {
			Ok(Ok(None)) => {} // expected: track ended
			Ok(Ok(Some(_))) => panic!("expected None for empty track, got Some"),
			Ok(Err(e)) => panic!("expected None for empty track, got error: {e}"),
			Err(_) => panic!("should not hang on empty track"),
		}
	}

	#[tokio::test]
	async fn track_closed_with_error() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_millis(500)),
						max: Some(Duration::from_millis(500)),
					},
				)]),
			);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		write_group(&mut track, 0, &[ts(0), ts(10_000)]);
		track.abort(moq_net::Error::Cancel).unwrap();

		// The groups already here drain before the track's error, as in JS.
		let mut frames = Vec::new();
		let end = loop {
			let read = kio::clock::timeout(Duration::from_millis(500), consumer.read())
				.await
				.expect("Consumer should not hang after track error");
			match read {
				Ok(Some(Read::Frame { frame, .. })) => frames.push(frame.timestamp),
				Ok(Some(_)) => continue,
				other => break other,
			}
		};
		assert_eq!(frames, [ts(0), ts(10_000)]);
		assert!(end.is_err(), "{end:?}");
		assert!(matches!(consumer.read().await, Ok(None)));
	}

	// ---- Gap Recovery ----

	#[kio::test]
	async fn gap_in_group_sequence_recovery() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track = track.subscribe(crate::container::test_util::within(Duration::from_millis(100)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		write_group(&mut track, 0, &[ts(0), ts(20_000)]);
		write_group(&mut track, 1, &[ts(40_000), ts(60_000)]);
		write_group(&mut track, 3, &[ts(120_000), ts(140_000)]);
		write_group(&mut track, 4, &[ts(160_000), ts(180_000)]);
		write_group(&mut track, 5, &[ts(200_000), ts(220_000)]);
		write_group(&mut track, 6, &[ts(240_000), ts(260_000)]);
		let _track = track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert!(frames.len() >= 4, "Expected >= 4 frames, got {}", frames.len());
	}

	#[kio::test]
	async fn gap_at_start_of_sequence() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track = track.subscribe(crate::container::test_util::within(Duration::from_millis(80)));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		write_group(&mut track, 5, &[ts(0), ts(20_000)]);
		write_group(&mut track, 7, &[ts(80_000), ts(100_000)]);
		write_group(&mut track, 8, &[ts(120_000), ts(140_000)]);
		write_group(&mut track, 9, &[ts(160_000), ts(180_000)]);
		let _track = track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert!(frames.len() >= 4, "Expected >= 4 frames, got {}", frames.len());
	}

	#[tokio::test(start_paused = true)]
	async fn truncated_resumed_group_skips_to_the_next_clean_group() {
		// One publisher identity behind every route, so each may resume the others.
		let identity = moq_net::Identity::random();
		let origin = crate::source::produce_origin();
		let route = |cost: u64| {
			moq_net::origin::RouteOptions::hidden()
				.with_identity(identity.clone(), 0)
				.with_cost(cost)
		};
		let first = origin.route("live", route(5)).unwrap();
		let info = hang::container::track_info(hang::catalog::PRIORITY.video);
		let first_track = first.broadcast().create_track("video", info.clone()).unwrap();
		first.announce().unwrap();
		let broadcast = origin.consume().request("live", None).await.unwrap();
		let track = broadcast
			.track("video")
			.unwrap()
			.subscribe(crate::container::test_util::within(Duration::from_secs(2)))
			.await
			.unwrap();
		let format = Container::Legacy(crate::container::Kind::Data);
		let mut consumer = Consumer::new(track, Container::Legacy(crate::container::Kind::Data));

		let mut open = first_track.create_group(0u64.into()).unwrap();
		format
			.write(
				&mut open,
				&[Frame {
					timestamp: ts(0),
					payload: Bytes::from_static(&[0xDE, 0xAD]),
					keyframe: true,
					duration: None,
				}],
			)
			.unwrap();

		// Each cheaper route takes over beyond group 0 without holding its continuation,
		// while the first route stays up but silent. They carry the same broadcast, so each
		// holds every group from 1 on. The open group is given up once the track runs a
		// full budget past it.
		let mut sources = Vec::new();
		for sequence in 1..=4 {
			let source = origin.route("live", route(5 - sequence)).unwrap();
			let mut track = source.broadcast().create_track("video", info.clone()).unwrap();
			source.announce().unwrap();
			let mut demand = track.demand();
			while !demand.wanted_changed().await.unwrap() {}
			if sequence == 1 {
				assert_eq!(consumer.read_frame().await.unwrap().unwrap().timestamp, ts(0));
			}
			for sequence in 1..=sequence {
				write_group(&mut track, sequence, &[ts(sequence * 1_000_000)]);
			}
			sources.push((source, track));
		}

		let waiter = kio::Waiter::noop();

		// The aborted group 0 says it was cut, then skips straight to its successor; a short
		// clean group emits GroupEnd.
		assert!(
			matches!(consumer.poll_event(&waiter), Poll::Ready(Ok(Some(Event::Cut { .. })))),
			"the cut is said before the successor"
		);
		let event = consumer.poll_event(&waiter);
		let Poll::Ready(Ok(Some(Event::Frame { frame, .. }))) = event else {
			panic!(
				"expected a successor frame, current={}; pending={}",
				consumer.current,
				event.is_pending()
			);
		};
		assert_eq!(frame.timestamp, ts(1_000_000));
		assert!(frame.keyframe);
		assert_eq!(consumer.current, 1);
		// Once the front settles on the newest route, whose info may still be in flight: a
		// short close waits to see whether that route holds more.
		assert!(
			matches!(
				kio::wait(|waiter| consumer.poll_event(waiter)).await,
				Ok(Some(Event::GroupEnd))
			),
			"the complete successor still emits its clean boundary"
		);
		assert_eq!(consumer.read_frame().await.unwrap().unwrap().timestamp, ts(2_000_000));
	}

	// ---- Eviction recovery (pause/resume) ----

	/// A group that aged out of the relay cache (aborted with `Error::Old`) while the
	/// consumer was parked on it must not hang the consumer: reading it errors, and
	/// the consumer skips the gap to the next live group even though the track is NOT
	/// finished. This is the resume-from-pause path (the recorder stops reading, the
	/// group + the sequences after it evict, then it resumes).
	#[kio::test]
	async fn evicted_group_with_gap_skips_to_live() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_millis(100)),
						max: Some(Duration::from_millis(100)),
					},
				)]),
			);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		// Group 0: a frame the consumer reads, positioning it there.
		let mut group0 = track.create_group(moq_net::group::Info { start: 0 }).unwrap();
		Container::Legacy(crate::container::Kind::Data)
			.write(
				&mut group0,
				&[Frame {
					timestamp: ts(0),
					payload: Bytes::from_static(&[0xDE, 0xAD]),
					keyframe: false,
					duration: None,
				}],
			)
			.unwrap();
		let first = consumer.read_frame().await.unwrap().unwrap();
		assert_eq!(first.timestamp, ts(0));

		// A live group arrives far ahead -- sequences 1..4 never come (evicted). The
		// track stays OPEN (not finished), the failure mode that used to hang.
		write_group(&mut track, 5, &[ts(150_000)]);

		// Group 0 ages out of the cache (the relay aborts it on eviction).
		group0.abort(moq_net::Error::Old).unwrap();

		// Must skip the evicted group + the gap and reach the live group, without
		// hanging on a track that never finishes.
		let next = kio::clock::timeout(Duration::from_secs(1), consumer.read())
			.await
			.expect("consumer hung on an evicted group / gap")
			.unwrap()
			.unwrap();
		assert!(
			matches!(next, Read::Gap { timestamp, duration } if timestamp == ts(0) && duration == ts(150_000)),
			"the evicted media is a gap from the last frame to the live group: {next:?}"
		);
		let next = consumer.read_frame().await.unwrap().unwrap();
		assert_eq!(next.timestamp, ts(150_000), "skipped the evicted gap to the live group");
	}

	/// A missing (evicted) sequence with a newer group buffered must be skipped once the
	/// age budget runs out, even while the track is still LIVE -- not only once it's
	/// finished. This is the recorder resume stall: `current` points at a sequence the
	/// cache dropped, higher groups are buffered, and the track never finishes. The gap
	/// is indistinguishable from a stream that lost the delivery race, so the skip fires
	/// only once the newest content is a full budget past what the gap could still hold.
	#[tokio::test]
	async fn missing_sequence_skips_on_live_track() {
		// The delivery budget runs on max(wall, pts): on wall time a stalled runner ages
		// group 2 out of delivery before the consumer reaches it.
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_millis(100)),
						max: Some(Duration::from_millis(100)),
					},
				)]),
			);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		// Read group 0 first, positioning the cursor there before the newer groups
		// arrive; otherwise delivery convicts group 0 and the cursor never meets the gap.
		write_group(&mut track, 0, &[ts(0), ts(20_000)]);
		for expected in [ts(0), ts(20_000)] {
			let frame = consumer.read_frame().await.unwrap().unwrap();
			assert_eq!(frame.timestamp, expected);
		}

		// Group 2 arrives but sequence 1 is missing (evicted) and never does. The track
		// is NOT finished (live), the case that used to hang. Group 3 pushes the live
		// edge a full budget past group 2's start, expiring the gap at 1.
		write_group(&mut track, 2, &[ts(200_000)]);
		write_group(&mut track, 3, &[ts(320_000)]);

		// The age budget alone closes the gap: the deadline sits short of the 100ms
		// hole timer, so waiting it out fails the test.
		let frame = kio::clock::timeout(Duration::from_millis(50), consumer.read_frame())
			.await
			.expect("consumer hung on a missing sequence on a live track")
			.unwrap()
			.unwrap();
		assert_eq!(frame.timestamp, ts(200_000), "skipped the missing sequence to group 2");
	}

	/// The other half of the budget-gated gap: while the newest content is still within
	/// the age budget of what the missing sequence could present, the consumer waits for
	/// it instead of writing it off, and delivers it when its stream loses the race but
	/// still arrives (#3258).
	#[tokio::test]
	async fn gap_keeps_late_arriving_group_within_max_delay() {
		tokio::time::pause();
		let mut copy = Received::with_info(hang::container::track_info(hang::catalog::PRIORITY.video)).await;
		let consumer_track = copy
			.subscribe_with(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_millis(500)),
						max: Some(Duration::from_millis(500)),
					},
				)]),
			)
			.await;
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		// Group 2's stream beats group 1's, which hasn't arrived at all yet.
		write_group(&mut copy.track, 0, &[ts(0)]);
		carried().await;
		write_group(&mut copy.track, 2, &[ts(200_000)]);
		carried().await;

		let first = consumer.read_frame().await.unwrap().expect("group 0 frame");
		assert_eq!(first.timestamp, ts(0));

		// Group 0 is done; group 1's stream is still racing. Within the budget the
		// consumer waits at the gap rather than skipping to group 2.
		assert!(
			kio::clock::timeout(Duration::from_millis(50), consumer.read())
				.await
				.is_err(),
			"the gap at sequence 1 is within budget and must be waited for"
		);

		// Group 1's stream opens moments later, well within the 500ms budget.
		write_group(&mut copy.track, 1, &[ts(100_000)]);
		carried().await;
		let _track = copy.track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		let micros: Vec<u128> = frames.iter().map(|f| f.timestamp.as_micros().unwrap()).collect();
		assert_eq!(micros, vec![100_000, 200_000], "the late group is delivered in order");
	}

	/// A group whose frames lost the race to a newer group's stream is still read once
	/// they land: the cursor starts at group 0 and waits under the budget (#3258).
	#[tokio::test]
	async fn startup_keeps_slow_earlier_stream_within_max_delay() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_millis(500)),
						max: Some(Duration::from_millis(500)),
					},
				)]),
			);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		// Group 0's stream opens first but carries no frames yet; group 1's frame wins.
		let mut group0 = track.create_group(moq_net::group::Info { start: 0 }).unwrap();
		write_group(&mut track, 1, &[ts(100_000)]);

		// Startup latches onto the lowest arrived sequence and waits under the budget.
		assert!(
			kio::clock::timeout(Duration::from_millis(50), consumer.read())
				.await
				.is_err(),
			"group 0's frames are within budget and must be waited for"
		);

		Container::Legacy(crate::container::Kind::Data)
			.write(
				&mut group0,
				&[Frame {
					timestamp: ts(0),
					payload: Bytes::from_static(&[0xDE, 0xAD]),
					keyframe: false,
					duration: None,
				}],
			)
			.unwrap();
		group0.finish().unwrap();
		let _track = track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		let micros: Vec<u128> = frames.iter().map(|f| f.timestamp.as_micros().unwrap()).collect();
		assert_eq!(micros, vec![0, 100_000], "the slow stream's frames are delivered");
	}

	// ---- Start reach ----

	/// An unfloored consumer that starts `min` plus `warmup` behind the live edge and skips at `max`.
	fn reach_consumer(
		track: &mut moq_net::track::Producer,
		min: Duration,
		max: Duration,
		warmup: Duration,
	) -> Consumer<Container> {
		let playout = Playout::new(
			Delay {
				min: Some(min),
				max: Some(max),
			},
			warmup,
		);
		let consumer_track = track.subscribe(
			playout.subscription(moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::ALL])),
		);
		playout.consumer(consumer_track, Container::Legacy(crate::container::Kind::Data))
	}

	/// A reader of `copy` that starts `min` plus `warmup` behind the live edge and skips at `max`.
	async fn reach_reader(copy: &Received, min: Duration, max: Duration, warmup: Duration) -> Consumer<Container> {
		let playout = Playout::new(
			Delay {
				min: Some(min),
				max: Some(max),
			},
			warmup,
		);
		let subscriber = copy
			.subscribe_with(
				playout.subscription(moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::ALL])),
			)
			.await;
		playout.consumer(subscriber, Container::Legacy(crate::container::Kind::Data))
	}

	/// The publisher sends the live edge first, newest first, yet the reader starts at the earlier
	/// group its join owes, which the ordered reader waits for.
	#[tokio::test(start_paused = true)]
	async fn the_start_waits_for_an_earlier_group_when_the_live_edge_arrives_first() {
		let mut copy = Received::with_info(hang::container::track_info(hang::catalog::PRIORITY.video)).await;
		append_timed(&mut copy.track, &[ts(4_800_000)]);
		append_timed(&mut copy.track, &[ts(5_000_000)]);
		let mut consumer = reach_reader(
			&copy,
			Duration::from_millis(500),
			Duration::from_millis(500),
			Duration::ZERO,
		)
		.await;
		let _track = copy.track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		let micros: Vec<u128> = frames.iter().map(|f| f.timestamp.as_micros().unwrap()).collect();
		assert_eq!(
			micros,
			vec![4_800_000, 5_000_000],
			"the earlier group is delivered first"
		);
	}

	/// With no min delay and no warmup, the same race starts at the live edge and drops the earlier
	/// group, as before.
	#[tokio::test]
	async fn a_zero_reach_starts_at_a_live_edge_that_arrives_first() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let mut consumer = reach_consumer(&mut track, Duration::ZERO, Duration::ZERO, Duration::ZERO);

		write_group(&mut track, 5, &[ts(5_000_000)]);
		let frame = kio::clock::timeout(Duration::from_millis(50), consumer.read_frame())
			.await
			.expect("nothing to wait for")
			.unwrap()
			.unwrap();
		assert_eq!(frame.timestamp, ts(5_000_000));

		write_group(&mut track, 4, &[ts(4_000_000)]);
		let _track = track.finish().unwrap();
		assert!(
			read_all(&mut consumer).await.unwrap().is_empty(),
			"the earlier group is old"
		);
	}

	/// The warmup reaches a live join further back, even one at the live edge, while an unstated
	/// min delay stays unstated: it reaches back from the oldest group already.
	#[test]
	fn the_warmup_widens_only_a_stated_min() {
		let warmup = Duration::from_millis(300);
		let unstated = Delay::default().with_min(None);
		assert_eq!(warmed(unstated, warmup).min, None);
		assert_eq!(warmed(unstated, Duration::ZERO).min, None);
		assert_eq!(warmed(unstated.with_min(Duration::ZERO), warmup).min, Some(warmup));
	}

	/// The warmup reaches past the min delay for the start: the subscription widens the max by it,
	/// so the publisher serves the group covering the live edge less both.
	#[tokio::test(start_paused = true)]
	async fn the_warmup_reaches_further_back_for_the_start() {
		for (warmup, expected) in [
			(Duration::ZERO, vec![4_700_000, 5_000_000]),
			(Duration::from_millis(300), vec![4_400_000, 4_700_000, 5_000_000]),
		] {
			let mut copy = Received::with_info(hang::container::track_info(hang::catalog::PRIORITY.video)).await;
			// Group 4 covers the min delay alone, but not the warmup too.
			append_timed(&mut copy.track, &[ts(4_400_000)]);
			append_timed(&mut copy.track, &[ts(4_700_000)]);
			append_timed(&mut copy.track, &[ts(5_000_000)]);
			let mut consumer =
				reach_reader(&copy, Duration::from_millis(200), Duration::from_millis(200), warmup).await;
			let _track = copy.track.finish().unwrap();

			let frames = read_all(&mut consumer).await.unwrap();
			let micros: Vec<u128> = frames.iter().map(|f| f.timestamp.as_micros().unwrap()).collect();
			assert_eq!(micros, expected, "warmup {warmup:?}");
		}
	}

	/// A hole is skipped at the max delay alone: the warmup does not widen it.
	#[tokio::test]
	async fn the_warmup_does_not_widen_the_skip() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let mut consumer = reach_consumer(
			&mut track,
			Duration::from_millis(200),
			Duration::from_millis(200),
			Duration::from_millis(300),
		);

		write_group(&mut track, 0, &[ts(0)]);
		let frame = consumer.read_frame().await.unwrap().unwrap();
		assert_eq!(frame.timestamp, ts(0));

		// Group 1 is missing, and group 2's content runs 250ms past where it starts: more than the
		// max delay, though less than the max delay plus the warmup.
		write_group(&mut track, 2, &[ts(100_000), ts(350_000)]);
		let frame = kio::clock::timeout(Duration::from_millis(50), consumer.read_frame())
			.await
			.expect("the hole is skipped at the max delay")
			.unwrap()
			.unwrap();
		assert_eq!(frame.timestamp, ts(100_000));
	}

	/// The group covering the reach stays while the rest of its warmup arrives after the first read,
	/// though its successor starts past the max delay alone.
	#[tokio::test]
	async fn the_covering_group_is_kept_while_its_warmup_arrives_after_the_first_read() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.audio));
		let mut consumer = reach_consumer(&mut track, Duration::ZERO, Duration::ZERO, Duration::from_millis(80));

		write_group(&mut track, 5_000_000, &[ts(5_000_000)]);
		// Group 4 covers the 4.92s target, and its first frame is read before the rest of it lands.
		let mut covering = track.create_group(moq_net::group::Info { start: 4_900_000 }).unwrap();
		let write = |group: &mut moq_net::group::Producer, micros| {
			let frame = Frame {
				timestamp: ts(micros),
				payload: Bytes::from_static(&[0xDE, 0xAD]),
				keyframe: false,
				duration: None,
			};
			Container::Legacy(crate::container::Kind::Data)
				.write(group, &[frame])
				.unwrap();
		};
		write(&mut covering, 4_900_000);
		let frame = kio::clock::timeout(Duration::from_millis(50), consumer.read_frame())
			.await
			.expect("group 4 covers the reach")
			.unwrap()
			.unwrap();
		assert_eq!(frame.timestamp, ts(4_900_000));
		assert!(
			kio::clock::timeout(Duration::from_millis(50), consumer.read())
				.await
				.is_err(),
			"the covering group's next frame is waited for"
		);

		write(&mut covering, 4_950_000);
		covering.finish().unwrap();
		let _track = track.finish().unwrap();
		let micros: Vec<u128> = read_all(&mut consumer)
			.await
			.unwrap()
			.iter()
			.map(|f| f.timestamp.as_micros().unwrap())
			.collect();
		assert_eq!(micros, vec![4_950_000, 5_000_000], "the warmup is not skipped as stale");
	}

	/// The start keeps the group covering the reach though its first frame trails the newest
	/// content by more than the max delay plus the warmup. `js/hang`'s
	/// `consumer.warmup.test.ts` pairs this case.
	#[tokio::test]
	async fn the_start_keeps_a_group_that_began_before_the_reach_but_covers_it() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let mut consumer = reach_consumer(
			&mut track,
			Duration::from_millis(200),
			Duration::from_millis(200),
			Duration::from_millis(300),
		);

		write_group(&mut track, 5_000_000, &[ts(5_000_000)]);
		// Group 4 begins 600ms before the newest content, past the 500ms reach, yet covers its 4.5s target.
		write_group(&mut track, 4_400_000, &[ts(4_400_000), ts(4_600_000), ts(4_900_000)]);
		let frame = kio::clock::timeout(Duration::from_millis(50), consumer.read_frame())
			.await
			.expect("group 4 covers the reach")
			.unwrap()
			.unwrap();
		assert_eq!(frame.timestamp, ts(4_400_000));
	}

	/// The max age clamps the max delay alone, not the subscription's max plus the warmup, so a max
	/// delay within the track's max age stays whole.
	#[tokio::test]
	async fn the_warmup_is_carved_before_the_max_age_clamp() {
		let mut info = hang::container::track_info(hang::catalog::PRIORITY.audio);
		info.max_age = Some(Duration::from_secs(1));
		let mut track = track_producer("test", info);
		let consumer = reach_consumer(
			&mut track,
			Duration::ZERO,
			Duration::from_secs(1),
			Duration::from_millis(300),
		);
		assert_eq!(consumer.max_delay, Duration::from_secs(1));
	}

	/// A quiet copy whose publisher holds nothing earlier starts at once: the ordered reader knows
	/// no earlier group is owed, so nothing waits a reach for one.
	#[tokio::test(start_paused = true)]
	async fn a_quiet_track_starts_at_once_when_nothing_earlier_is_owed() {
		let mut copy = Received::with_info(hang::container::track_info(hang::catalog::PRIORITY.video)).await;
		append_timed(&mut copy.track, &[ts(5_000_000)]);
		let mut consumer = reach_reader(
			&copy,
			Duration::from_millis(200),
			Duration::from_millis(200),
			Duration::from_millis(300),
		)
		.await;
		let frame = kio::clock::timeout(Duration::from_millis(50), consumer.read_frame())
			.await
			.expect("nothing earlier is owed")
			.unwrap()
			.unwrap();
		assert_eq!(frame.timestamp, ts(5_000_000));
	}

	/// Media a reach past where the newest group starts ends the wait without the wall clock.
	#[tokio::test]
	async fn the_start_needs_no_wait_once_a_group_covers_the_reach() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let mut consumer = reach_consumer(
			&mut track,
			Duration::from_millis(500),
			Duration::from_millis(500),
			Duration::ZERO,
		);

		write_group(&mut track, 5, &[ts(5_000_000), ts(5_500_000)]);
		let frame = kio::clock::timeout(Duration::from_millis(50), consumer.read_frame())
			.await
			.expect("the newest group covers the reach")
			.unwrap()
			.unwrap();
		assert_eq!(frame.timestamp, ts(5_000_000));
	}

	/// A full max buffer can hold none of what the start waits for, so the wait ends at once.
	#[tokio::test]
	async fn the_start_wait_ends_once_the_max_buffer_is_full() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let mut consumer = reach_consumer(
			&mut track,
			Duration::from_millis(500),
			Duration::from_millis(500),
			Duration::ZERO,
		);
		consumer.set_max_buffer(1);

		write_group(&mut track, 5, &[ts(5_000_000), ts(5_100_000)]);
		let frame = kio::clock::timeout(Duration::from_millis(50), consumer.read_frame())
			.await
			.expect("the max buffer cannot hold an earlier group")
			.unwrap()
			.unwrap();
		assert_eq!(frame.timestamp, ts(5_000_000));
	}

	/// A group below the subscriber's range never arrives, so the start does not wait for it.
	#[tokio::test]
	async fn the_start_stops_at_the_subscriber_range() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track = track
			.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_millis(500)),
						max: Some(Duration::from_millis(500)),
					},
				)]),
			)
			.with_range(5..);
		let mut consumer = Playout::new(
			Delay {
				min: Some(Duration::from_millis(500)),
				max: Some(Duration::from_millis(500)),
			},
			Duration::ZERO,
		)
		.consumer(consumer_track, Container::Legacy(crate::container::Kind::Data));
		assert!(
			matches!(consumer.startup, Startup::Earliest),
			"a local range names no start, so the start still reaches back"
		);

		write_group(&mut track, 5, &[ts(5_000_000)]);
		let frame = kio::clock::timeout(Duration::from_millis(50), consumer.read_frame())
			.await
			.expect("nothing below the range can arrive")
			.unwrap()
			.unwrap();
		assert_eq!(frame.timestamp, ts(5_000_000));
	}

	/// A default min delay starts at a live edge that arrives first however large the max delay is:
	/// the max delay bounds lateness, not the start.
	#[tokio::test]
	async fn a_large_max_delay_alone_starts_at_once() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let mut consumer = reach_consumer(&mut track, Duration::ZERO, Duration::from_secs(10), Duration::ZERO);

		write_group(&mut track, 5, &[ts(5_000_000)]);
		let frame = kio::clock::timeout(Duration::from_millis(50), consumer.read_frame())
			.await
			.expect("nothing to wait for")
			.unwrap()
			.unwrap();
		assert_eq!(frame.timestamp, ts(5_000_000));
	}

	/// A min delay past the max delay reaches back only the max delay, since the publisher serves
	/// nothing older.
	#[tokio::test(start_paused = true)]
	async fn a_min_delay_past_the_max_delay_reaches_the_max_delay() {
		let mut copy = Received::with_info(hang::container::track_info(hang::catalog::PRIORITY.video)).await;
		append_timed(&mut copy.track, &[ts(4_400_000)]);
		append_timed(&mut copy.track, &[ts(4_700_000)]);
		append_timed(&mut copy.track, &[ts(5_000_000)]);
		let mut consumer = reach_reader(
			&copy,
			Duration::from_secs(2),
			Duration::from_millis(200),
			Duration::ZERO,
		)
		.await;
		let _track = copy.track.finish().unwrap();
		let frames = read_all(&mut consumer).await.unwrap();
		let micros: Vec<u128> = frames.iter().map(|f| f.timestamp.as_micros().unwrap()).collect();
		assert_eq!(micros, vec![4_700_000, 5_000_000], "group 3 is past the max delay");
	}

	/// The subscription carries the min and max delay, each plus the warmup, so a live join
	/// reaches back far enough to warm the decoder before its first played frame.
	#[tokio::test]
	async fn the_subscription_carries_the_warmed_min_and_max_delay() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer = reach_consumer(
			&mut track,
			Duration::from_secs(2),
			Duration::from_millis(200),
			Duration::from_millis(300),
		);
		let subscription = subscribed(&consumer).subscription();
		assert_eq!(
			subscription.ranges,
			[moq_net::track::Range::from(moq_net::track::Start::Live {
				min: Some(Duration::from_millis(2_300)),
				max: Some(Duration::from_millis(500)),
			})]
		);
		assert_eq!(consumer.max_delay, Duration::from_millis(200));
	}

	/// A track subscribed without the warmup has its max widened by the playout, keeping the live
	/// join it resolved, and its consumer still starts the warmup further back.
	#[tokio::test]
	async fn the_playout_widens_a_narrow_subscription() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let delay = Delay {
			min: Some(Duration::from_millis(100)),
			max: Some(Duration::from_millis(200)),
		};
		let consumer_track = track.subscribe(
			delay.subscription(moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::ALL])),
		);
		let consumer = Playout::new(delay, Duration::from_millis(300))
			.consumer(consumer_track, Container::Legacy(crate::container::Kind::Data));
		let subscription = subscribed(&consumer).subscription();
		assert_eq!(
			subscription.ranges,
			[moq_net::track::Range::from(moq_net::track::Start::Live {
				min: Some(Duration::from_millis(100)),
				max: Some(Duration::from_millis(500)),
			})]
		);
		assert_eq!(consumer.max_delay, Duration::from_millis(200));
		assert_eq!(consumer.warmup, Duration::from_millis(300));
	}

	/// A catalog warmup past the cap reaches back, and widens the subscription, by the cap alone.
	#[tokio::test]
	async fn the_playout_caps_a_huge_warmup() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let delay = Delay::default().with_max(Some(Duration::from_millis(200)));
		let playout = Playout::new(delay, Duration::from_secs(3600));
		let consumer = playout.consumer(
			track.subscribe(
				playout.subscription(moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::ALL])),
			),
			Container::Legacy(crate::container::Kind::Data),
		);
		assert_eq!(
			subscribed(&consumer).subscription().ranges,
			[moq_net::track::Range::from(moq_net::track::Start::Live {
				min: Some(Duration::from_millis(200) + DELAY_MAX),
				max: Some(Duration::from_millis(200) + DELAY_MAX),
			})]
		);
		assert_eq!(consumer.warmup, DELAY_MAX);
	}

	/// An unlimited max delay keeps the subscription unlimited and skips nothing: a reader from a
	/// time start, which the min delay doesn't place, reads all of the history the copy holds, even
	/// past the min delay's reach.
	#[tokio::test(start_paused = true)]
	async fn an_unlimited_max_delay_reads_all_history() {
		let mut copy = Received::with_info(hang::container::track_info(hang::catalog::PRIORITY.video)).await;
		append_timed(&mut copy.track, &[ts(3_000_000)]);
		append_timed(&mut copy.track, &[ts(4_500_000)]);
		append_timed(&mut copy.track, &[ts(5_000_000)]);
		let playout = Playout::new(
			Delay {
				min: Some(Duration::from_millis(500)),
				max: None,
			},
			Duration::ZERO,
		);
		let from_zero = moq_net::track::Subscription::default()
			.with_ranges([moq_net::track::Range::from(moq_net::track::Start::Time(0))]);
		let subscriber = copy.subscribe_with(playout.subscription(from_zero)).await;
		let mut consumer = playout.consumer(subscriber, Container::Legacy(crate::container::Kind::Data));
		assert_eq!(
			subscribed(&consumer).subscription().ranges,
			[moq_net::track::Range::from(moq_net::track::Start::Time(0))],
			"a time start stays history, so nothing it reaches goes stale"
		);
		let _track = copy.track.finish().unwrap();
		let frames = read_all(&mut consumer).await.unwrap();
		let micros: Vec<u128> = frames.iter().map(|f| f.timestamp.as_micros().unwrap()).collect();
		assert_eq!(micros, vec![3_000_000, 4_500_000, 5_000_000]);
	}

	/// A subscription without a max delay reaches back for history, so it starts at once.
	#[tokio::test]
	async fn an_unbounded_subscription_starts_at_once() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track = track.subscribe(moq_net::track::Subscription::default());
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		write_group(&mut track, 5, &[ts(5_000_000)]);
		let frame = kio::clock::timeout(Duration::from_millis(50), consumer.read_frame())
			.await
			.expect("an unbounded reach waits for nothing")
			.unwrap()
			.unwrap();
		assert_eq!(frame.timestamp, ts(5_000_000));
	}

	// ---- Decode errors ----

	/// A container that decodes each frame's payload as an 8-byte LE microsecond
	/// timestamp, but treats a `FAIL` payload as a malformed frame. Lets a test put a
	/// decodable frame first (so the consumer reads the group) and a decode failure after.
	struct FailingDecode;

	impl ContainerTrait for FailingDecode {
		type Error = crate::Error;

		fn write(&self, group: &mut moq_net::group::Producer, frames: &[Frame]) -> Result<(), Self::Error> {
			for frame in frames {
				group.write_frame(moq_net::Timestamp::ZERO, frame.payload.clone())?;
			}
			Ok(())
		}

		fn decode(&self, frame: moq_net::frame::Frame) -> Result<Vec<Frame>, Self::Error> {
			use bytes::Buf;

			let mut data = frame.payload;
			if data.as_ref() == b"FAIL" {
				return Err(crate::Error::UnknownFormat("malformed payload".into()));
			}
			Ok(vec![Frame {
				timestamp: ts(data.get_u64_le()),
				payload: Bytes::new(),
				keyframe: false,
				duration: None,
			}])
		}
	}

	/// A decode error on a cleanly-finished group must propagate to the caller, not be
	/// mistaken for a relay eviction and silently skipped. Eviction-skip only fires when
	/// the group's stream was actually aborted.
	#[tokio::test]
	async fn decode_error_propagates() {
		tokio::time::pause();
		let track = track_producer("test", timed());
		let consumer_track = track.subscribe(None);
		let mut consumer = Consumer::new(consumer_track, FailingDecode);

		// A decodable frame first (so the consumer reads the group), then a malformed one.
		let mut group = track.create_group(moq_net::group::Info { start: 0 }).unwrap();
		group
			.write_frame(moq_net::Timestamp::ZERO, Bytes::from(0u64.to_le_bytes().to_vec()))
			.unwrap();
		group
			.write_frame(moq_net::Timestamp::ZERO, Bytes::from_static(b"FAIL"))
			.unwrap();
		group.finish().unwrap();
		let _track = track.finish().unwrap();

		// The first frame decodes; the malformed second frame must surface as an error.
		let first = consumer.read_frame().await;
		assert!(matches!(first, Ok(Some(_))), "first frame should decode, got {first:?}");

		let second = kio::clock::timeout(Duration::from_millis(200), consumer.read())
			.await
			.expect("consumer hung on a decode error");
		assert!(
			matches!(second, Err(crate::Error::UnknownFormat(_))),
			"decode error must propagate, got {second:?}"
		);
	}

	/// Write one FailingDecode frame: a microsecond timestamp, or `None` for a malformed payload,
	/// stamped a microsecond past a second per group start so a group it opens starts after the
	/// groups before it.
	fn write_failing(group: &mut moq_net::group::Producer, micros: Option<u64>) {
		let payload = match micros {
			Some(micros) => Bytes::from(micros.to_le_bytes().to_vec()),
			None => Bytes::from_static(b"FAIL"),
		};
		let stamp = micros.unwrap_or(group.start * 1_000_000 + 1);
		group.write_frame(ts(stamp), payload).unwrap();
	}

	/// Read until something other than a frame, failing if the reader stalls.
	async fn read_past_frames(consumer: &mut Consumer<FailingDecode>) -> Result<Option<Read>, crate::Error> {
		loop {
			let read = kio::clock::timeout(Duration::from_millis(200), consumer.read())
				.await
				.expect("consumer hung");
			match read {
				Ok(Some(Read::Frame { .. })) => continue,
				other => return other,
			}
		}
	}

	/// A malformed object found by the startup lookahead, at the head of the first group, ends the
	/// reader rather than being consumed and dropped.
	#[tokio::test]
	async fn a_decode_error_at_the_head_of_a_group_ends_the_reader() {
		tokio::time::pause();
		let track = track_producer("test", timed());
		let subscription = moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
			moq_net::track::Start::Live {
				min: Some(Duration::from_secs(10)),
				max: Some(Duration::from_secs(10)),
			},
		)]);
		let mut consumer = Consumer::new(track.subscribe(subscription), FailingDecode);

		let mut group = track.create_group(moq_net::group::Info { start: 0 }).unwrap();
		write_failing(&mut group, None);
		write_failing(&mut group, Some(0));
		group.finish().unwrap();
		let _track = track.finish().unwrap();

		let read = read_past_frames(&mut consumer).await;
		assert!(matches!(read, Err(crate::Error::UnknownFormat(_))), "{read:?}");
		// The reader stays ended rather than resuming past the malformed object.
		let read = consumer.read().await;
		assert!(matches!(read, Ok(None)), "{read:?}");
	}

	/// The exporter path reads events directly, and it stays ended after an error too.
	#[tokio::test]
	async fn a_decode_error_ends_the_event_reader() {
		tokio::time::pause();
		let track = track_producer("test", timed());
		let mut consumer = Consumer::new(track.subscribe(None), FailingDecode);

		let mut group = track.create_group(moq_net::group::Info { start: 0 }).unwrap();
		write_failing(&mut group, None);
		write_failing(&mut group, Some(0));
		group.finish().unwrap();
		let _track = track.finish().unwrap();

		let event = kio::wait(|waiter| consumer.poll_event(waiter)).await;
		assert!(matches!(event, Err(crate::Error::UnknownFormat(_))));
		let event = kio::wait(|waiter| consumer.poll_event(waiter)).await;
		assert!(matches!(event, Ok(None)), "the event reader resumed after an error");
	}

	/// An error releases the subscription and the groups it holds, so a reader kept after it pins
	/// neither the publisher's demand nor its cache.
	#[tokio::test]
	async fn an_error_releases_the_subscription_and_its_groups() {
		tokio::time::pause();
		let track = track_producer("test", timed());
		let subscription = moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
			moq_net::track::Start::Live {
				min: Some(Duration::from_secs(10)),
				max: Some(Duration::from_secs(10)),
			},
		)]);
		let mut consumer = Consumer::new(track.subscribe(subscription), FailingDecode);

		let mut malformed = track.create_group(moq_net::group::Info { start: 0 }).unwrap();
		write_failing(&mut malformed, None);
		let malformed_demand = malformed.demand();
		let _malformed = malformed.finish().unwrap();
		let mut later = track.create_group(moq_net::group::Info { start: 1 }).unwrap();
		write_failing(&mut later, Some(10_000));

		let read = read_past_frames(&mut consumer).await;
		assert!(matches!(read, Err(crate::Error::UnknownFormat(_))), "{read:?}");
		assert!(!track.demand().wanted(), "the ended reader still subscribes");
		assert!(!malformed_demand.wanted(), "the ended reader still holds its group");
		assert!(!later.demand().wanted(), "the ended reader still holds a later group");
	}

	/// A malformed object mid-group, found while looking ahead past a blocked group, ends the
	/// reader at its place rather than reading as a shorter group.
	#[tokio::test]
	async fn a_decode_error_mid_group_found_by_lookahead_ends_the_reader() {
		tokio::time::pause();
		let track = track_producer("test", timed());
		let subscription = moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
			moq_net::track::Start::Live {
				min: Some(Duration::from_secs(10)),
				max: Some(Duration::from_secs(10)),
			},
		)]);
		let mut consumer = Consumer::new(track.subscribe(subscription), FailingDecode);

		// The current group stays open, so the reader looks ahead into the next one.
		let mut slow = track.create_group(moq_net::group::Info { start: 0 }).unwrap();
		write_failing(&mut slow, Some(0));
		let mut group = track.create_group(moq_net::group::Info { start: 1 }).unwrap();
		write_failing(&mut group, Some(10_000));
		write_failing(&mut group, None);
		write_failing(&mut group, Some(20_000));
		group.finish().unwrap();

		let first = consumer.read_frame().await.unwrap().unwrap();
		assert_eq!(first.timestamp, ts(0));
		// Lookahead finds the error while the reader waits on the open group, which comes first.
		assert!(
			kio::clock::timeout(Duration::from_millis(50), consumer.read())
				.await
				.is_err()
		);

		slow.finish().unwrap();
		let frame = consumer.read_frame().await.unwrap().unwrap();
		assert_eq!(frame.timestamp, ts(10_000));
		let read = kio::clock::timeout(Duration::from_millis(50), consumer.read()).await;
		assert!(matches!(read, Ok(Err(crate::Error::UnknownFormat(_)))), "{read:?}");
		// The reader stays ended rather than resuming with the group's later frames.
		let read = kio::clock::timeout(Duration::from_millis(50), consumer.read()).await;
		assert!(matches!(read, Ok(Ok(None))), "{read:?}");
	}

	/// A malformed head of a later group waits for the current group's frames: errors come in
	/// delivery order, however early the lookahead finds them.
	#[tokio::test]
	async fn a_decode_error_in_a_later_group_waits_for_current_frames() {
		tokio::time::pause();
		let track = track_producer("test", timed());
		let subscription = moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
			moq_net::track::Start::Live {
				min: Some(Duration::from_secs(10)),
				max: Some(Duration::from_secs(10)),
			},
		)]);
		let mut consumer = Consumer::new(track.subscribe(subscription), FailingDecode);

		let mut current = track.create_group(moq_net::group::Info { start: 0 }).unwrap();
		write_failing(&mut current, Some(0));
		write_failing(&mut current, Some(10_000));
		write_failing(&mut current, Some(20_000));
		let first = consumer.read_frame().await.unwrap().unwrap();
		assert_eq!(first.timestamp, ts(0));

		let mut later = track.create_group(moq_net::group::Info { start: 1 }).unwrap();
		write_failing(&mut later, None);
		later.finish().unwrap();
		for expected in [10_000, 20_000] {
			let frame = consumer.read_frame().await.unwrap().unwrap();
			assert_eq!(frame.timestamp, ts(expected));
		}

		current.finish().unwrap();
		let read = read_past_frames(&mut consumer).await;
		assert!(matches!(read, Err(crate::Error::UnknownFormat(_))), "{read:?}");
	}

	/// A later group's rewind does not mask an earlier group's decode error: the first in delivery
	/// order is the one returned.
	#[tokio::test]
	async fn a_decode_error_comes_before_a_later_rewind() {
		tokio::time::pause();
		let track = track_producer("test", timed());
		let subscription = moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
			moq_net::track::Start::Live {
				min: Some(Duration::from_secs(10)),
				max: Some(Duration::from_secs(10)),
			},
		)]);
		let mut consumer = Consumer::new(track.subscribe(subscription), FailingDecode);

		let mut current = track.create_group(moq_net::group::Info { start: 0 }).unwrap();
		write_failing(&mut current, Some(100_000));
		let first = consumer.read_frame().await.unwrap().unwrap();
		assert_eq!(first.timestamp, ts(100_000));

		let mut malformed = track.create_group(moq_net::group::Info { start: 1 }).unwrap();
		write_failing(&mut malformed, Some(110_000));
		malformed.write_frame(ts(110_000), Bytes::from_static(b"FAIL")).unwrap();
		malformed.finish().unwrap();
		// The transport only checks a group's first frame, so the rewind is its second.
		let mut rewound = track.create_group(moq_net::group::Info { start: 2 }).unwrap();
		write_failing(&mut rewound, Some(150_000));
		write_failing(&mut rewound, Some(50_000));
		rewound.finish().unwrap();
		// The reader looks ahead into both while it waits on the open group.
		assert!(
			kio::clock::timeout(Duration::from_millis(50), consumer.read())
				.await
				.is_err()
		);

		current.finish().unwrap();
		let _track = track.finish().unwrap();
		let read = read_past_frames(&mut consumer).await;
		assert!(matches!(read, Err(crate::Error::UnknownFormat(_))), "{read:?}");
	}

	/// A live reader discards the groups below the newest, and a malformed one it already read
	/// while finding the start still ends the reader.
	#[tokio::test]
	async fn a_startup_discard_returns_a_malformed_group() {
		tokio::time::pause();
		let track = track_producer("test", timed());
		let subscription = moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
			moq_net::track::Start::Live {
				min: Some(Duration::from_secs(10)),
				max: Some(Duration::from_secs(10)),
			},
		)]);
		let mut consumer = Consumer::new(track.subscribe(subscription), FailingDecode).live();

		let mut old = track.create_group(moq_net::group::Info { start: 0 }).unwrap();
		write_failing(&mut old, None);
		old.finish().unwrap();
		let mut newest = track.create_group(moq_net::group::Info { start: 1 }).unwrap();
		write_failing(&mut newest, Some(10_000));
		newest.finish().unwrap();
		let _track = track.finish().unwrap();

		let read = read_past_frames(&mut consumer).await;
		assert!(matches!(read, Err(crate::Error::UnknownFormat(_))), "{read:?}");
	}

	/// An aborted later group holding a fault is not reaped ahead of its place: its error waits
	/// for the current group's frames, as a live group's does.
	#[tokio::test]
	async fn an_aborted_later_group_keeps_its_fault_in_delivery_order() {
		tokio::time::pause();
		let track = track_producer("test", timed());
		let subscription = moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
			moq_net::track::Start::Live {
				min: Some(Duration::from_secs(10)),
				max: Some(Duration::from_secs(10)),
			},
		)]);
		let mut consumer = Consumer::new(track.subscribe(subscription), FailingDecode);

		let mut current = track.create_group(moq_net::group::Info { start: 0 }).unwrap();
		write_failing(&mut current, Some(0));
		let first = consumer.read_frame().await.unwrap().unwrap();
		assert_eq!(first.timestamp, ts(0));

		let mut later = track.create_group(moq_net::group::Info { start: 1 }).unwrap();
		write_failing(&mut later, None);
		// The reader looks ahead into the later group while it waits on the open one.
		assert!(
			kio::clock::timeout(Duration::from_millis(50), consumer.read())
				.await
				.is_err()
		);
		later.abort(moq_net::Error::Cancel).unwrap();

		write_failing(&mut current, Some(10_000));
		let frame = consumer.read_frame().await.unwrap().unwrap();
		assert_eq!(frame.timestamp, ts(10_000));

		current.finish().unwrap();
		let _track = track.finish().unwrap();
		let read = read_past_frames(&mut consumer).await;
		assert!(matches!(read, Err(crate::Error::UnknownFormat(_))), "{read:?}");
	}

	/// A malformed object read from the current group ends the reader even when the group's
	/// unread tail was reset afterwards: the reset is loss after it, not a reason to drop it.
	#[tokio::test]
	async fn a_decode_error_before_a_reset_tail_ends_the_reader() {
		tokio::time::pause();
		let track = track_producer("test", timed());
		let subscription = moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
			moq_net::track::Start::Live {
				min: Some(Duration::from_secs(10)),
				max: Some(Duration::from_secs(10)),
			},
		)]);
		let mut consumer = Consumer::new(track.subscribe(subscription), FailingDecode);

		let mut group = track.create_group(moq_net::group::Info { start: 0 }).unwrap();
		write_failing(&mut group, Some(0));
		write_failing(&mut group, None);
		let first = consumer.read_frame().await.unwrap().unwrap();
		assert_eq!(first.timestamp, ts(0));

		group.abort(moq_net::Error::Cancel).unwrap();
		let _track = track.finish().unwrap();
		let read = kio::clock::timeout(Duration::from_millis(50), consumer.read()).await;
		assert!(matches!(read, Ok(Err(crate::Error::UnknownFormat(_)))), "{read:?}");
	}

	/// The lookahead keeps a malformed object it already holds even when the group is evicted
	/// before it gets there: the eviction is loss behind that object, not a reason to drop it.
	#[test]
	fn the_lookahead_keeps_a_decode_error_before_an_evicted_tail() {
		let waiter = kio::Waiter::noop();
		let track = track_producer("test", timed());
		let mut group = track.create_group(moq_net::group::Info { start: 0 }).unwrap();
		write_failing(&mut group, Some(0));
		write_failing(&mut group, None);
		write_failing(&mut group, Some(10_000));
		let group = group.finish().unwrap();

		// Finding the start buffers one frame; the read behind it already holds the rest.
		let mut buffer = GroupBuffer::<FailingDecode>::new(group.consume(), Memory::new(DEFAULT_MAX_BUFFER));
		assert_eq!(
			buffer.poll_min_timestamp(&waiter, &FailingDecode, false),
			Poll::Ready(Some(ts(0)))
		);
		group.abort(moq_net::Error::Old).unwrap();

		let _ = buffer.look_all(&waiter, &FailingDecode);
		assert!(
			matches!(buffer.fault, Some(crate::Error::UnknownFormat(_))),
			"{:?}",
			buffer.fault
		);
	}

	// ---- Frame Decoding ----

	#[kio::test]
	async fn frame_payload_preserved() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_millis(500)),
						max: Some(Duration::from_millis(500)),
					},
				)]),
			);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		let payload_bytes = vec![0x01, 0x02, 0x03, 0x04, 0x05];
		let mut group = track.create_group(moq_net::group::Info { start: 0 }).unwrap();
		Container::Legacy(crate::container::Kind::Data)
			.write(
				&mut group,
				&[Frame {
					timestamp: ts(0),
					payload: Bytes::from(payload_bytes.clone()),

					keyframe: false,
					duration: None,
				}],
			)
			.unwrap();
		group.finish().unwrap();
		let _track = track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 1);

		use bytes::Buf;
		let mut received = Vec::new();
		let mut payload = frames[0].payload.clone();
		while payload.has_remaining() {
			received.push(payload.get_u8());
		}
		assert_eq!(received, payload_bytes);
	}

	// ---- Regression ----

	#[tokio::test]
	async fn no_infinite_loop_with_buffered_frames() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_secs(10)),
						max: Some(Duration::from_secs(10)),
					},
				)]),
			);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		let mut group0 = track.create_group(moq_net::group::Info { start: 0 }).unwrap();
		Container::Legacy(crate::container::Kind::Data)
			.write(
				&mut group0,
				&[Frame {
					timestamp: ts(0),
					payload: Bytes::from_static(&[0xDE, 0xAD]),
					keyframe: false,
					duration: None,
				}],
			)
			.unwrap();

		write_group(&mut track, 1, &[ts(100_000)]);

		let finisher = tokio::spawn(async move {
			kio::clock::sleep(Duration::from_millis(20)).await;
			// Write group 2: recv_group fires, drops current buffer_until for group 1
			write_group(&mut track, 2, &[ts(200_000)]);
			kio::clock::sleep(Duration::from_millis(20)).await;
			group0.finish().unwrap();
			let _track = track.finish().unwrap();
		});

		let frames = kio::clock::timeout(Duration::from_secs(2), async {
			let mut frames = Vec::new();
			while let Some(frame) = consumer.read_frame().await.unwrap() {
				frames.push(frame);
			}
			frames
		})
		.await
		.expect("consumer hung - possible infinite loop regression");

		assert_eq!(frames.len(), 3);
		finisher.await.expect("finisher task panicked");
	}

	// ---- Edge Cases ----

	#[kio::test]
	async fn large_timestamps() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_secs(3700)),
						max: Some(Duration::from_secs(3700)),
					},
				)]),
			);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		let one_hour = 3_600_000_000u64;
		write_group(&mut track, 0, &[ts(one_hour)]);
		let _track = track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 1);
		assert_eq!(frames[0].timestamp, ts(one_hour));
		assert_eq!(frames[0].timestamp.as_micros().unwrap(), one_hour as u128);
	}

	#[kio::test]
	async fn set_delay_changes_behavior() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_secs(10)),
						max: Some(Duration::from_secs(10)),
					},
				)]),
			);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		write_group(&mut track, 0, &[ts(0)]);
		let _track = track.finish().unwrap();

		let frame = consumer.read_frame().await.unwrap().unwrap();
		assert_eq!(frame.timestamp, ts(0));

		consumer.set_delay(Delay {
			max: Some(Duration::from_millis(100)),
			..Delay::default()
		});

		assert!(consumer.read().await.unwrap().is_none());
	}

	#[tokio::test]
	async fn max_timestamp_tracks_through_bframes() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_millis(110)),
						max: Some(Duration::from_millis(110)),
					},
				)]),
			);
		// max delay must exceed (group1_max - group0_min) = 100ms - 0ms = 100ms
		// to avoid the age skip and test B-frame timestamp tracking.
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		let mut group0 = track.create_group(moq_net::group::Info { start: 0 }).unwrap();
		for &timestamp in &[ts(0), ts(66_000), ts(33_000)] {
			Container::Legacy(crate::container::Kind::Data)
				.write(
					&mut group0,
					&[Frame {
						timestamp,
						payload: Bytes::from_static(&[0xDE, 0xAD]),
						keyframe: false,
						duration: None,
					}],
				)
				.unwrap();
		}

		write_group(&mut track, 1, &[ts(100_000)]);
		let _track = track.finish().unwrap();

		let finisher = tokio::spawn(async move {
			kio::clock::sleep(Duration::from_millis(50)).await;
			group0.finish().unwrap();
		});

		let frames = kio::clock::timeout(Duration::from_secs(2), async {
			let mut frames = Vec::new();
			while let Some(frame) = consumer.read_frame().await.unwrap() {
				frames.push(frame);
			}
			frames
		})
		.await
		.expect("consumer hung - max_timestamp regression");

		assert_eq!(frames.len(), 4, "Expected all 4 frames, got {}", frames.len());
		assert_eq!(frames[0].timestamp, ts(0));
		assert_eq!(frames[1].timestamp, ts(66_000));
		assert_eq!(frames[2].timestamp, ts(33_000));
		assert_eq!(frames[3].timestamp, ts(100_000));
		finisher.await.expect("finisher task panicked");
	}

	// ---- Startup Behavior ----

	#[tokio::test]
	async fn startup_selects_earliest_group() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_millis(100)),
						max: Some(Duration::from_millis(100)),
					},
				)]),
			);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		write_group(&mut track, 3, &[ts(0)]);
		write_group(&mut track, 5, &[ts(150_000)]);

		let mut group7 = track.create_group(moq_net::group::Info { start: 7 }).unwrap();
		Container::Legacy(crate::container::Kind::Data)
			.write(
				&mut group7,
				&[Frame {
					timestamp: ts(300_000),
					payload: Bytes::from_static(&[0xDE, 0xAD]),
					keyframe: false,
					duration: None,
				}],
			)
			.unwrap();

		let finisher = tokio::spawn(async move {
			kio::clock::sleep(Duration::from_millis(50)).await;
			Container::Legacy(crate::container::Kind::Data)
				.write(
					&mut group7,
					&[Frame {
						timestamp: ts(400_000),
						payload: Bytes::from_static(&[0xBE, 0xEF]),
						keyframe: false,
						duration: None,
					}],
				)
				.unwrap();
			group7.finish().unwrap();
			let _track = track.finish().unwrap();
		});

		let _frames = kio::clock::timeout(Duration::from_secs(2), async {
			let mut frames = Vec::new();
			while let Some(frame) = consumer.read_frame().await.unwrap() {
				frames.push(frame);
			}
			frames
		})
		.await
		.expect("should not hang");

		finisher.await.unwrap();
	}

	/// An arrived group with no frames yet is settled only by its own FIN, abort, or the
	/// age budget, never by the track finishing: the track boundary ends new groups, not
	/// the frames still flowing on open ones. Here the budget expires it.
	#[tokio::test]
	async fn startup_skips_groups_without_data() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_millis(500)),
						max: Some(Duration::from_millis(500)),
					},
				)]),
			);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		let _group5 = track.create_group(moq_net::group::Info { start: 5 }).unwrap();
		write_group(&mut track, 7, &[ts(210_000)]);

		// Group 5 is open and within budget: its frames may still arrive, so wait.
		assert!(
			kio::clock::timeout(Duration::from_millis(50), consumer.read())
				.await
				.is_err(),
			"an open frameless group within budget must be waited for"
		);

		// Group 9 pushes the newest content a full budget past what group 5 could still
		// present (bounded by group 7's start), expiring it. Exactly a budget, so group 7's
		// first frame is not past it and the start stays there.
		write_group(&mut track, 9, &[ts(710_000)]);
		let _track = track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		let micros: Vec<u128> = frames.iter().map(|f| f.timestamp.as_micros().unwrap()).collect();
		assert_eq!(micros, vec![210_000, 710_000], "the expired frameless group is skipped");
	}

	/// An aborted frameless group beyond a sequence gap is reaped rather than parked on
	/// forever: nothing else settles a group the cursor hasn't reached, so a finished
	/// track would otherwise never report its end.
	#[tokio::test]
	async fn aborted_frameless_group_after_a_gap_ends_the_track() {
		tokio::time::pause();
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_millis(500)),
						max: Some(Duration::from_millis(500)),
					},
				)]),
			);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		// Sequence 0 never arrives; sequence 1's stream opens but never carries a frame.
		let group1 = track.create_group(moq_net::group::Info { start: 1 }).unwrap();
		let _track = track.finish().unwrap();

		// While group 1 is open its frames may still come, so the consumer waits.
		assert!(
			kio::clock::timeout(Duration::from_millis(50), consumer.read())
				.await
				.is_err(),
			"an open frameless group must be waited for"
		);

		// The abort (an eviction) settles it, and the finished track ends cleanly.
		group1.abort(moq_net::Error::Old).unwrap();
		let end = kio::clock::timeout(Duration::from_millis(200), consumer.read())
			.await
			.expect("an aborted frameless group must not park the consumer");
		assert!(end.unwrap().is_none(), "the track ends cleanly");
	}

	/// Track completion is not group completion: a group already open when the track
	/// finishes can still receive frames, so it must not be skipped as if it were a
	/// missing sequence. Its late frames are delivered when they land within budget.
	#[tokio::test]
	async fn finished_track_waits_for_an_open_head_group() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_millis(500)),
						max: Some(Duration::from_millis(500)),
					},
				)]),
			);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		// Group 0's stream is open but its frames lose the race; the track boundary
		// arrives before them.
		let mut group0 = track.create_group(moq_net::group::Info { start: 0 }).unwrap();
		write_group(&mut track, 1, &[ts(100_000)]);
		let _track = track.finish().unwrap();

		assert!(
			kio::clock::timeout(Duration::from_millis(50), consumer.read())
				.await
				.is_err(),
			"the open head group is within budget and must be waited for"
		);

		// Its frames land moments later, well within the 500ms budget.
		Container::Legacy(crate::container::Kind::Data)
			.write(
				&mut group0,
				&[Frame {
					timestamp: ts(0),
					payload: Bytes::from_static(&[0xDE, 0xAD]),
					keyframe: false,
					duration: None,
				}],
			)
			.unwrap();
		group0.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		let micros: Vec<u128> = frames.iter().map(|f| f.timestamp.as_micros().unwrap()).collect();
		assert_eq!(micros, vec![0, 100_000], "the open group's late frames are delivered");
	}

	/// A gap between two ranges is never read, so the reader walks over it at once rather
	/// than waiting a budget, or forever on an open track, for groups it never gets.
	#[tokio::test]
	async fn an_excluded_gap_between_ranges_does_not_stall() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let subscription = moq_net::track::Subscription::default().with_ranges([
			moq_net::track::Range {
				start: moq_net::track::Start::Time(0),
				end: Some(0),
				..Default::default()
			},
			moq_net::track::Range {
				start: moq_net::track::Start::Time(10),
				end: Some(10),
				..Default::default()
			},
		]);
		let mut consumer = Consumer::new(
			track.subscribe(subscription),
			Container::Legacy(crate::container::Kind::Data),
		);
		for sequence in [0, 5, 10] {
			write_group(&mut track, sequence, &[ts(sequence * 100_000)]);
		}

		let mut micros = Vec::new();
		for _ in 0..2 {
			let frame = kio::clock::timeout(Duration::from_millis(200), consumer.read_frame())
				.await
				.expect("the open track still delivers the later range")
				.unwrap()
				.expect("a frame");
			micros.push(frame.timestamp.as_micros().unwrap());
		}
		assert_eq!(micros, vec![0, 1_000_000]);
	}

	/// A live join names its floor once resolved: delivery starts at the group containing the
	/// live edge minus its lag, rather than at whichever group arrives first.
	#[tokio::test]
	async fn startup_at_a_live_join() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		for sequence in 5..=9 {
			write_group(&mut track, sequence * 100_000, &[ts(sequence * 100_000)]);
		}
		let subscription = Delay::default()
			.with_min(Duration::from_millis(200))
			.with_max(Duration::from_secs(10))
			.subscription(moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::ALL]));
		let consumer_track = track.subscribe(subscription);
		assert_eq!(consumer_track.floor(), Some(700_000));
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));
		let _track = track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		let micros: Vec<u128> = frames.iter().map(|f| f.timestamp.as_micros().unwrap()).collect();
		assert_eq!(micros, vec![700_000, 800_000, 900_000]);
	}

	/// The reader starts where the publisher resolved the join, though the newest group lands first.
	#[tokio::test(start_paused = true)]
	async fn startup_at_the_publishers_start_over_a_newer_first_arrival() {
		let mut copy = Received::with_info(hang::container::track_info(hang::catalog::PRIORITY.video)).await;
		for sequence in [7, 8, 9, 10] {
			append_timed(&mut copy.track, &[ts(sequence * 100_000)]);
		}
		let subscriber = copy
			.subscribe_with(
				Delay::default()
					.with_min(Duration::from_millis(200))
					.with_max(Duration::from_secs(10))
					.subscription(moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::ALL])),
			)
			.await;
		let mut consumer = Consumer::new(subscriber, Container::Legacy(crate::container::Kind::Data));
		let _track = copy.track.finish().unwrap();

		assert_eq!(items(&mut consumer).await, ["800000", "900000", "1000000"]);
	}

	/// A live join takes the start the publisher resolved for it: the publisher starts at a
	/// newest group that may hold no frame yet, which neither the largest it reports nor this copy
	/// counts, so the reader waits for no group below it.
	#[tokio::test(start_paused = true)]
	async fn a_waiting_live_join_takes_the_publishers_resolved_start() {
		let mut copy = Received::with_info(hang::container::track_info(hang::catalog::PRIORITY.video)).await;
		for sequence in 0..5 {
			write_group(&mut copy.track, sequence * 100_000, &[ts(sequence * 100_000)]);
		}
		let mut newest = copy
			.track
			.create_group(moq_net::group::Info { start: 500_000 })
			.unwrap();
		let subscriber = copy
			.subscribe_with(
				Delay::default()
					.with_min(Duration::ZERO)
					.with_max(Duration::from_secs(10))
					.subscription(moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::ALL])),
			)
			.await;
		let mut consumer = Consumer::new(subscriber, Container::Legacy(crate::container::Kind::Data));

		let frame = Frame {
			timestamp: ts(500_000),
			payload: Bytes::from_static(&[0xDE, 0xAD]),
			keyframe: false,
			duration: None,
		};
		Container::Legacy(crate::container::Kind::Data)
			.write(&mut newest, &[frame])
			.unwrap();
		newest.finish().unwrap();
		let _track = copy.track.finish().unwrap();
		assert_eq!(items(&mut consumer).await, ["500000"]);
	}

	#[kio::test]
	async fn startup_single_group_mid_stream() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_millis(500)),
						max: Some(Duration::from_millis(500)),
					},
				)]),
			);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		write_group(&mut track, 100, &[ts(3_000_000)]);
		let _track = track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 1);
	}

	/// An unfloored mid-stream join adopts the publisher's served start instead of
	/// waiting for a gap below it to expire: on a quiet track that gap never would,
	/// since nothing newer arrives to age it out.
	#[tokio::test]
	async fn startup_mid_stream_live_track_starts_at_the_served_group() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_millis(500)),
						max: Some(Duration::from_millis(500)),
					},
				)]),
			);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		// The track is far along and stays live (never finished); only the current
		// group is served, and nothing else arrives.
		write_group(&mut track, 100, &[ts(3_000_000)]);

		let frame = kio::clock::timeout(Duration::from_millis(200), consumer.read_frame())
			.await
			.expect("an unfloored join must not wait out a gap below the served start")
			.unwrap()
			.expect("track still live");
		assert_eq!(frame.timestamp, ts(3_000_000));
	}

	#[tokio::test]
	async fn multiple_sequential_latency_skips() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_millis(50)),
						max: Some(Duration::from_millis(50)),
					},
				)]),
			);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		let mut group0 = track.create_group(moq_net::group::Info { start: 0 }).unwrap();
		Container::Legacy(crate::container::Kind::Data)
			.write(
				&mut group0,
				&[Frame {
					timestamp: ts(0),
					payload: Bytes::from_static(&[0xAA]),

					keyframe: false,
					duration: None,
				}],
			)
			.unwrap();

		write_group(&mut track, 1, &[ts(100_000)]);
		write_group(&mut track, 2, &[ts(200_000)]);
		write_group(&mut track, 3, &[ts(300_000)]);
		let _track = track.finish().unwrap();

		let finisher = tokio::spawn(async move {
			kio::clock::sleep(Duration::from_millis(20)).await;
			group0.finish().unwrap();
		});

		let frames = read_all(&mut consumer).await.unwrap();
		assert!(!frames.is_empty());
		finisher.await.unwrap();
	}

	#[tokio::test]
	async fn latency_skip_boundary_exact() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_millis(100)),
						max: Some(Duration::from_millis(100)),
					},
				)]),
			);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		let mut group0 = track.create_group(moq_net::group::Info { start: 0 }).unwrap();
		Container::Legacy(crate::container::Kind::Data)
			.write(
				&mut group0,
				&[Frame {
					timestamp: ts(0),
					payload: Bytes::from_static(&[0xAA]),

					keyframe: false,
					duration: None,
				}],
			)
			.unwrap();

		write_group(&mut track, 1, &[ts(100_000)]);
		let _track = track.finish().unwrap();

		let finisher = tokio::spawn(async move {
			kio::clock::sleep(Duration::from_millis(20)).await;
			group0.finish().unwrap();
		});

		let frames = read_all(&mut consumer).await.unwrap();
		assert!(!frames.is_empty());
		finisher.await.unwrap();
	}

	/// Regression: a single stalled group with one newer group should trigger
	/// a delay skip when the timestamp difference exceeds the max delay.
	/// Previously, the span was computed across newer groups only (zero for one
	/// group), so the skip never fired.
	#[tokio::test]
	async fn single_newer_group_triggers_skip() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track = track.subscribe(None);
		let mut consumer = container_max_delay_only(consumer_track, Duration::from_millis(100));

		// Group 0: stalled at ts=0, NOT finished
		let mut group0 = track.create_group(moq_net::group::Info { start: 0 }).unwrap();
		Container::Legacy(crate::container::Kind::Data)
			.write(
				&mut group0,
				&[Frame {
					timestamp: ts(0),
					payload: Bytes::from_static(&[0xDE, 0xAD]),
					keyframe: false,
					duration: None,
				}],
			)
			.unwrap();

		// Read before group 1 lands, so the skip comes after the start.
		assert_eq!(consumer.read_frame().await.unwrap().unwrap().timestamp, ts(0));

		// Group 1: finished, 200ms ahead (well beyond the 100ms max delay)
		write_group(&mut track, 1, &[ts(200_000)]);
		let _track = track.finish().unwrap();

		let finisher = tokio::spawn(async move {
			kio::clock::sleep(Duration::from_millis(50)).await;
			group0.finish().unwrap();
		});

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 1, "Expected group 1's frame after group 0's");
		finisher.await.unwrap();
	}

	/// Regression: when the current group is fully consumed and the next sequence
	/// is missing (gap), the consumer should skip to the next available group
	/// once the track is fully received, rather than hanging forever.
	#[tokio::test]
	async fn single_missing_sequence_near_eof_skips() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track = track.subscribe(None);
		let mut consumer = container_max_delay_only(consumer_track, Duration::from_millis(100));

		// Group 0: finished normally, its first frame read before group 2 lands.
		write_group(&mut track, 0, &[ts(0), ts(20_000)]);
		assert_eq!(consumer.read_frame().await.unwrap().unwrap().timestamp, ts(0));
		// Group 2: finished (group 1 is missing - sequence gap)
		write_group(&mut track, 2, &[ts(200_000)]);
		let _track = track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 2, "Expected group 0's second frame + group 2 (1 frame)");
	}

	#[kio::test]
	async fn group_error_skips_to_next() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_millis(500)),
						max: Some(Duration::from_millis(500)),
					},
				)]),
			);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		let group0 = track.create_group(moq_net::group::Info { start: 0 }).unwrap();
		group0.abort(moq_net::Error::Cancel).unwrap();

		write_group(&mut track, 1, &[ts(30_000)]);
		let _track = track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 1);
	}

	/// A finished group aborted afterwards (idle past the cache pool's expiry window)
	/// keeps serving the frames it still holds: an abort only matters where a frame is
	/// missing, so a reader that stopped short of the end drains the rest and moves on
	/// to the next buffered group instead of ending the track.
	#[kio::test]
	async fn finished_group_aborted_mid_read_drains_then_continues() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_millis(500)),
						max: Some(Duration::from_millis(500)),
					},
				)]),
			);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		let mut group0 = track.create_group(moq_net::group::Info { start: 0 }).unwrap();
		for timestamp in [ts(0), ts(10_000)] {
			let frame = Frame {
				timestamp,
				payload: Bytes::from_static(&[0xDE, 0xAD]),
				keyframe: false,
				duration: None,
			};
			Container::Legacy(crate::container::Kind::Data)
				.write(&mut group0, &[frame])
				.unwrap();
		}
		let group0 = group0.finish().unwrap();
		write_group(&mut track, 1, &[ts(30_000)]);
		let _track = track.finish().unwrap();

		// Take the first frame only, leaving the second unread.
		let first = consumer.read_frame().await.unwrap().unwrap();
		assert_eq!(first.timestamp, ts(0));

		// Expiry aborts the finished group while the reader is still inside it.
		group0.abort(moq_net::Error::Old).unwrap();

		let rest = read_all(&mut consumer).await.unwrap();
		assert_eq!(rest.len(), 2, "expected the held frame then group 1, got {rest:?}");
		assert_eq!(rest[0].timestamp, ts(10_000));
		assert_eq!(rest[1].timestamp, ts(30_000));
	}

	/// A live group its upstream gave up as too large is aborted. A reader whose current
	/// group died that way skips forward; the gap is not a decode error.
	#[kio::test]
	async fn oversized_group_skips_to_next() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_millis(500)),
						max: Some(Duration::from_millis(500)),
					},
				)]),
			);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		let mut group0 = track.create_group(moq_net::group::Info { start: 0 }).unwrap();
		let first = Frame {
			timestamp: ts(0),
			payload: Bytes::from(vec![0xDEu8; 1024]),
			keyframe: false,
			duration: None,
		};
		Container::Legacy(crate::container::Kind::Data)
			.write(&mut group0, &[first])
			.unwrap();
		group0.abort(moq_net::Error::GroupTooLarge).unwrap();

		write_group(&mut track, 1, &[ts(30_000)]);
		let _track = track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 1, "expected group 1 only, got {} frames", frames.len());
		assert_eq!(frames[0].timestamp, ts(30_000));
	}

	#[tokio::test]
	async fn track_finishes_while_reading() {
		tokio::time::pause();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_millis(500)),
						max: Some(Duration::from_millis(500)),
					},
				)]),
			);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		write_group(&mut track, 0, &[ts(0)]);

		let finisher = tokio::spawn(async move {
			kio::clock::sleep(Duration::from_millis(20)).await;
			write_group(&mut track, 1, &[ts(30_000)]);
			kio::clock::sleep(Duration::from_millis(20)).await;
			let _track = track.finish().unwrap();
		});

		let frames = kio::clock::timeout(Duration::from_secs(2), async {
			let mut frames = Vec::new();
			while let Some(frame) = consumer.read_frame().await.unwrap() {
				frames.push(frame);
			}
			frames
		})
		.await
		.expect("consumer should not hang");

		assert_eq!(frames.len(), 2);
		finisher.await.unwrap();
	}

	#[kio::test]
	async fn empty_group_advances() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_millis(500)),
						max: Some(Duration::from_millis(500)),
					},
				)]),
			);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		let group0 = track.create_group(moq_net::group::Info { start: 0 }).unwrap();
		group0.finish().unwrap();

		write_group(&mut track, 1, &[ts(30_000)]);
		let _track = track.finish().unwrap();

		let frames = read_all(&mut consumer).await.unwrap();
		assert_eq!(frames.len(), 1);
	}

	// ---- VideoConfig Container ----

	#[tokio::test]
	async fn video_container_legacy() {
		tokio::time::pause();

		let track = track_producer("video", hang::container::track_info(hang::catalog::PRIORITY.video));
		let consumer_track =
			track.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_millis(500)),
						max: Some(Duration::from_millis(500)),
					},
				)]),
			);
		let mut consumer = Consumer::new(consumer_track, Container::Legacy(crate::container::Kind::Data));

		// Write frames using Container::Legacy(crate::container::Kind::Data) encoding
		let mut group = track.create_group(moq_net::group::Info { start: 0 }).unwrap();
		for i in 0..3u64 {
			let frame = Frame {
				timestamp: ts(i * 33_333),
				payload: Bytes::from_static(&[0xDE, 0xAD]),
				keyframe: false,
				duration: None,
			};
			Container::Legacy(crate::container::Kind::Data)
				.write(&mut group, &[frame])
				.unwrap();
		}
		group.finish().unwrap();
		let _track = track.finish().unwrap();

		let mut frames = Vec::new();
		while let Some(frame) = consumer.read_frame().await.unwrap() {
			frames.push(frame);
		}

		assert_eq!(frames.len(), 3);
		assert_eq!(frames[0].timestamp, ts(0));
		assert!(frames[0].keyframe);
		assert_eq!(frames[1].timestamp, ts(33_333));
		assert!(!frames[1].keyframe);
		assert_eq!(frames[2].timestamp, ts(66_666));
		assert!(!frames[2].keyframe);
	}

	// ---- Duration Skipping ----

	/// A stalled group whose frame covers up to the next group's start is skipped
	/// immediately, even with a max delay budget far larger than the gap. Without
	/// duration support the consumer would block on the unfinished group forever.
	#[tokio::test]
	async fn duration_skip_advances_to_next_group() {
		tokio::time::pause();
		// DurationWire is a test-only container that doesn't stamp moq_net frame
		// timestamps; leave the track untimed so model-layer validation matches.
		let track = track_producer("test", timed());
		let consumer_track =
			track.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_secs(10)),
						max: Some(Duration::from_secs(10)),
					},
				)]),
			);
		// The max delay dwarfs the gap, so only duration coverage can trigger the skip.
		let mut consumer = Consumer::new(consumer_track, DurationWire);

		// Group 0: one frame at ts=0 lasting 33ms, never finished.
		let mut group0 = track.create_group(moq_net::group::Info { start: 0 }).unwrap();
		write_duration_frame(&mut group0, ts(0), ts(33_000));

		// Group 1: finished, starts exactly where group 0's frame ends.
		let mut group1 = track.create_group(moq_net::group::Info { start: 1 }).unwrap();
		write_duration_frame(&mut group1, ts(33_000), ts(33_000));
		group1.finish().unwrap();

		let _track = track.finish().unwrap();

		let frames = kio::clock::timeout(Duration::from_secs(2), async { continuous_frames(&mut consumer).await })
			.await
			.expect("consumer hung - duration skip regression");

		assert_eq!(frames.len(), 2);
		assert_eq!(frames[0].timestamp, ts(0));
		assert_eq!(frames[1].timestamp, ts(33_000));

		// group0 is intentionally never finished.
		drop(group0);
	}

	#[kio::test]
	async fn a_nonsequential_contiguous_jump_has_no_gap() {
		let track = track_producer("test", timed());
		let mut consumer = Consumer::new(
			track.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_secs(10)),
						max: Some(Duration::from_secs(10)),
					},
				)]),
			),
			DurationWire,
		);
		let mut group0 = track.create_group(moq_net::group::Info { start: 1_000_000 }).unwrap();
		write_duration_frame(&mut group0, ts(0), ts(33_000));
		group0.finish().unwrap();
		let mut group1 = track.create_group(moq_net::group::Info { start: 1_090_000 }).unwrap();
		write_duration_frame(&mut group1, ts(33_000), ts(33_000));
		group1.finish().unwrap();
		let _track = track.finish().unwrap();
		let frames = continuous_frames(&mut consumer).await;
		assert_eq!(frames.len(), 2);
	}

	/// When the current group's frame ends before the next group begins, there is
	/// still a gap to cover, so we don't skip early: a late-arriving frame on the
	/// slow group is delivered rather than dropped.
	#[tokio::test]
	async fn duration_below_gap_does_not_skip() {
		tokio::time::pause();
		// DurationWire is untimed at the moq_net frame layer.
		let track = track_producer("test", timed());
		let consumer_track =
			track.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_secs(10)),
						max: Some(Duration::from_secs(10)),
					},
				)]),
			);
		let mut consumer = Consumer::new(consumer_track, DurationWire);

		// Group 0: frame at ts=0 lasting only 10ms, far short of group 1 at 33ms.
		let mut group0 = track.create_group(moq_net::group::Info { start: 0 }).unwrap();
		write_duration_frame(&mut group0, ts(0), ts(10_000));

		// Group 1: finished at 33ms.
		let mut group1 = track.create_group(moq_net::group::Info { start: 1 }).unwrap();
		write_duration_frame(&mut group1, ts(33_000), ts(33_000));
		group1.finish().unwrap();
		let _track = track.finish().unwrap();

		// A second frame lands on group 0 and finishes it after the consumer has
		// had a chance to (incorrectly) skip.
		let finisher = tokio::spawn(async move {
			kio::clock::sleep(Duration::from_millis(20)).await;
			write_duration_frame(&mut group0, ts(20_000), ts(10_000));
			group0.finish().unwrap();
		});

		let frames = kio::clock::timeout(Duration::from_secs(2), async {
			let mut frames = Vec::new();
			while let Some(frame) = consumer.read_frame().await.unwrap() {
				frames.push(frame);
			}
			frames
		})
		.await
		.expect("consumer hung");

		// The slow group's late frame survives because nothing covered the gap.
		assert_eq!(frames.len(), 3);
		assert_eq!(frames[0].timestamp, ts(0));
		assert_eq!(frames[1].timestamp, ts(20_000));
		assert_eq!(frames[2].timestamp, ts(33_000));
		finisher.await.unwrap();
	}
	#[kio::test]
	async fn live_duration_marker_follows_an_immediately_delivered_frame() {
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let mut consumer = Consumer::new(track.subscribe(None), Container::Legacy(crate::container::Kind::Video));
		let mut group = track.append_group(Timestamp::from_micros(0).unwrap()).unwrap();
		Container::Legacy(crate::container::Kind::Video)
			.write(
				&mut group,
				&[Frame {
					timestamp: ts(0),
					payload: Bytes::from_static(b"video"),
					keyframe: true,
					duration: None,
				}],
			)
			.unwrap();
		let frame = kio::clock::timeout(Duration::from_secs(1), consumer.read_frame())
			.await
			.unwrap()
			.unwrap()
			.unwrap();
		assert_eq!(frame.timestamp, ts(0));
		write_marker(&mut group, ts(15_000));
		let event = kio::wait(|waiter| consumer.poll_event(waiter)).await.unwrap();
		assert!(matches!(event, Some(Event::FrameEnd { end, opens: false }) if end == ts(15_000)));
	}

	// ---- Max buffer ----

	/// Write one unfinished Legacy data frame into a new group, leaving it open.
	fn open_group(
		track: &mut moq_net::track::Producer,
		sequence: u64,
		timestamp: Timestamp,
	) -> moq_net::group::Producer {
		let mut group = track.create_group(moq_net::group::Info { start: sequence }).unwrap();
		Container::Legacy(crate::container::Kind::Data)
			.write(
				&mut group,
				&[Frame {
					timestamp,
					payload: Bytes::from_static(&[0xDE, 0xAD]),
					keyframe: false,
					duration: None,
				}],
			)
			.unwrap();
		group
	}

	/// Read every item, checking after each that the consumer holds about the max buffer at most:
	/// past it, one more wire frame and the groups the cursor still needs.
	async fn read_bounded(consumer: &mut Consumer<Container>, max_buffer: usize) -> Vec<Read> {
		let slack = 4 * std::mem::size_of::<GroupBuffer<Container>>() + 1024;
		let mut reads = Vec::new();
		loop {
			let read = kio::clock::timeout(Duration::from_secs(5), consumer.read())
				.await
				.expect("consumer hung")
				.unwrap();
			let used = consumer.buffered_bytes();
			assert!(
				used <= max_buffer + slack,
				"holding {used} bytes past a {max_buffer} byte max buffer"
			);
			match read {
				Some(read) => reads.push(read),
				None => return reads,
			}
		}
	}

	fn timestamps(reads: &[Read]) -> Vec<Timestamp> {
		reads
			.iter()
			.filter_map(|read| match read {
				Read::Frame { frame, .. } => Some(frame.timestamp),
				_ => None,
			})
			.collect()
	}

	/// Frames packed into one timestamp have no span, so the age budget never skips the stalled group
	/// they queue behind. Read ahead as the newest group, they fill the max buffer instead, which skips
	/// it, and the rest stays unread in the transport's cache until the reader gets to it.
	#[tokio::test]
	async fn frames_sharing_a_timestamp_stay_within_the_max_buffer() {
		tokio::time::pause();
		const MAX_BUFFER: usize = 4096;
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let mut consumer = container_max_delay_only(track.subscribe(None), Duration::from_secs(5));
		consumer.set_max_buffer(MAX_BUFFER);

		let _stalled = open_group(&mut track, 0, ts(0));
		write_group(&mut track, 1, &vec![ts(10_000); 2_000]);
		let _track = track.finish().unwrap();

		let reads = read_bounded(&mut consumer, MAX_BUFFER).await;
		let timestamps = timestamps(&reads);
		assert_eq!(timestamps.len(), 1 + 2_000);
		assert_eq!(timestamps.first(), Some(&ts(0)));
		assert_eq!(timestamps.last(), Some(&ts(10_000)));
		// The stalled group's tail is lost.
		assert!(matches!(reads[1], Read::Gap { .. }), "{:?}", reads[1]);
	}

	/// A publisher opening many groups fills the max buffer with their overhead and first frames, so
	/// the rest wait in the transport's cache: playback still reaches the live edge, and what arrives
	/// below the cursor once there is room is dropped as old.
	#[tokio::test]
	async fn many_groups_stay_within_the_max_buffer() {
		tokio::time::pause();
		const MAX_BUFFER: usize = 8192;
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let subscriber =
			track.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_millis(100)),
						max: Some(Duration::from_millis(100)),
					},
				)]),
			);
		let mut consumer = Consumer::new(subscriber, Container::Legacy(crate::container::Kind::Data));
		consumer.set_max_buffer(MAX_BUFFER);

		let _stalled = open_group(&mut track, 0, ts(0));
		for g in 1..=500u64 {
			write_group(&mut track, g, &[ts(g * 1_000)]);
		}
		let _track = track.finish().unwrap();

		let reads = read_bounded(&mut consumer, MAX_BUFFER).await;
		let timestamps = timestamps(&reads);
		assert_eq!(timestamps.last(), Some(&ts(500_000)));
		assert!(timestamps.len() < 501, "nothing was skipped: {}", timestamps.len());
	}

	/// A successor holding only endpoint markers stops buffering at the max buffer like any other
	/// lookahead, rather than reading past it in search of a media frame.
	#[kio::test]
	async fn an_endpoint_only_successor_stays_within_the_max_buffer() {
		const MAX_BUFFER: usize = 1024;
		let waiter = kio::Waiter::noop();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let subscriber =
			track.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_secs(30)),
						max: Some(Duration::from_secs(30)),
					},
				)]),
			);
		let mut consumer = Consumer::new(subscriber, Container::Legacy(crate::container::Kind::Video));
		consumer.set_max_buffer(MAX_BUFFER);

		let _stalled = open_group(&mut track, 0, ts(0));
		let mut markers = track.create_group(moq_net::group::Info { start: 1 }).unwrap();
		for _ in 0..2_000 {
			write_marker(&mut markers, ts(10_000));
		}
		Container::Legacy(crate::container::Kind::Data)
			.write(
				&mut markers,
				&[Frame {
					timestamp: ts(20_000),
					payload: Bytes::from_static(b"video"),
					keyframe: false,
					duration: None,
				}],
			)
			.unwrap();
		markers.finish().unwrap();

		// The markers fill the max buffer behind the stalled group, which moves the cursor on to them,
		// and the media past them still plays.
		let slack = 2 * std::mem::size_of::<GroupBuffer<Container>>() + MARKER_COST;
		let mut frames = Vec::new();
		loop {
			let read = consumer.poll_read(&waiter);
			let used = consumer.buffered_bytes();
			assert!(
				used <= MAX_BUFFER + slack,
				"holding {used} bytes past a {MAX_BUFFER} byte max buffer"
			);
			match read {
				Poll::Ready(Ok(Some(Read::Frame { frame, .. }))) => frames.push(frame.timestamp),
				Poll::Ready(Ok(Some(_))) => continue,
				Poll::Pending => break,
				other => panic!("unexpected read: {other:?}"),
			}
		}
		assert_eq!(frames, vec![ts(0), ts(20_000)]);
	}

	/// A decoded payload is a view of its wire frame, which stays allocated while the view is held, so a
	/// view that leaves out most of its wire frame is copied out: what holds the frame after the reader,
	/// such as an export's jitter buffer, retains only its payload. Here each is a 1 byte payload behind
	/// a large LOC property block.
	#[kio::test]
	async fn a_small_view_of_a_large_wire_frame_is_copied_out() {
		const PADDING: usize = 60_000;

		// A QUIC varint, which is all LOC's framing needs.
		fn varint(buf: &mut Vec<u8>, value: usize) {
			match value {
				0..64 => buf.push(value as u8),
				64..16_384 => buf.extend_from_slice(&(value as u16 | 0x4000).to_be_bytes()),
				_ => buf.extend_from_slice(&(value as u32 | 0x8000_0000).to_be_bytes()),
			}
		}

		let waiter = kio::Waiter::noop();
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let subscriber =
			track.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_secs(30)),
						max: Some(Duration::from_secs(30)),
					},
				)]),
			);
		let mut consumer = Consumer::new(subscriber, Container::Loc(crate::container::Kind::Data));

		let mut padded = track.create_group(moq_net::group::Info { start: 0 }).unwrap();
		for i in 0..8 {
			let timestamp = i * 1_000;
			// An unknown video-config property (0x0d) padding the frame, then the timestamp (0x10).
			let mut props = Vec::new();
			varint(&mut props, 0x0d);
			varint(&mut props, PADDING);
			props.resize(props.len() + PADDING, 0);
			varint(&mut props, 0x10 - 0x0d);
			varint(&mut props, timestamp);
			let mut wire = Vec::new();
			varint(&mut wire, props.len());
			wire.extend_from_slice(&props);
			wire.push(0xAD);
			padded.write_frame(ts(timestamp as u64), wire).unwrap();
		}
		padded.finish().unwrap();

		let mut reads = Vec::new();
		while let Poll::Ready(read) = consumer.poll_read(&waiter) {
			match read.unwrap() {
				Some(Read::Frame { frame, .. }) => {
					assert_eq!(frame.payload[..], [0xAD]);
					// The group still holds its wire frames, so a view of one would share it.
					assert!(frame.payload.is_unique(), "the payload still shares its wire frame");
					reads.push(frame.timestamp);
				}
				Some(Read::End { .. }) | None => break,
				other => panic!("unexpected read: {other:?}"),
			}
		}
		assert_eq!(reads, (0..8).map(|i| ts(i * 1_000)).collect::<Vec<_>>());
	}

	/// A stalled successor does not hide a readable group behind it once group overhead alone fills the
	/// max buffer: the cursor moves on group by group until one has something to read.
	#[kio::test]
	async fn a_stalled_successor_does_not_hide_a_readable_group() {
		let waiter = kio::Waiter::noop();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let mut consumer = container_max_delay_only(track.subscribe(None), Duration::from_secs(5));
		consumer.set_max_buffer(2 * std::mem::size_of::<GroupBuffer<Container>>());

		let _stalled = open_group(&mut track, 0, ts(0));
		let _empty = track.create_group(moq_net::group::Info { start: 1 }).unwrap();
		write_group(&mut track, 2, &[ts(20_000)]);

		let mut reads = Vec::new();
		while let Poll::Ready(read) = consumer.poll_read(&waiter) {
			match read.unwrap() {
				Some(Read::Frame { frame, .. }) => reads.push(Some(frame.timestamp)),
				Some(Read::Gap { .. }) => reads.push(None),
				other => panic!("unexpected read: {other:?}"),
			}
		}
		assert_eq!(reads, vec![Some(ts(0)), None, Some(ts(20_000))]);
	}

	/// Groups 0 and 1 open behind a cursor that read group 0's frame, and group 2's large frame that
	/// fills the max buffer once read ahead. Returns groups 0 and 1.
	fn fill_behind_a_stalled_group(
		track: &mut moq_net::track::Producer,
		consumer: &mut Consumer<Container>,
		waiter: &kio::Waiter,
	) -> (moq_net::group::Producer, moq_net::group::Producer) {
		let stalled = open_group(track, 0, ts(0));
		assert!(matches!(
			consumer.poll_read(waiter),
			Poll::Ready(Ok(Some(Read::Frame { group: 0, .. })))
		));
		let between = track.create_group(moq_net::group::Info { start: 1 }).unwrap();
		let mut later = track.create_group(moq_net::group::Info { start: 2 }).unwrap();
		Container::Legacy(crate::container::Kind::Data)
			.write(
				&mut later,
				&[Frame {
					timestamp: ts(20_000),
					payload: Bytes::from(vec![0; 4096]),
					keyframe: false,
					duration: None,
				}],
			)
			.unwrap();
		later.finish().unwrap();
		(stalled, between)
	}

	// A max buffer that holds three groups' overhead with room to spare, short of group 2's large frame.
	fn pressured_data_consumer(track: &mut moq_net::track::Producer) -> Consumer<Container> {
		let mut consumer = container_max_delay_only(track.subscribe(None), Duration::from_secs(5));
		consumer.set_max_buffer(1024 + 3 * std::mem::size_of::<GroupBuffer<Container>>());
		consumer
	}

	/// A malformed frame landing in the group the cursor moves on to under pressure fails the reader
	/// rather than leaving it waiting on the group.
	#[kio::test]
	async fn a_malformed_frame_in_a_group_moved_to_under_pressure_fails_the_reader() {
		let waiter = kio::Waiter::noop();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let mut consumer = pressured_data_consumer(&mut track);
		let (_stalled, mut between) = fill_behind_a_stalled_group(&mut track, &mut consumer, &waiter);

		between.write_frame(ts(10_000), Bytes::from_static(&[0x80])).unwrap();
		let read = loop {
			match consumer.poll_read(&waiter) {
				Poll::Ready(Ok(Some(Read::Gap { .. }))) => continue,
				other => break other,
			}
		};
		assert!(matches!(read, Poll::Ready(Err(_))), "{read:?}");
	}

	/// A data marker group that finished before the cursor moved on to it under pressure still reads
	/// its end before the group after it.
	#[kio::test]
	async fn a_finished_marker_group_moved_to_under_pressure_reads_its_end() {
		let waiter = kio::Waiter::noop();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let mut consumer = pressured_data_consumer(&mut track);
		let (_stalled, mut between) = fill_behind_a_stalled_group(&mut track, &mut consumer, &waiter);

		between.write_frame(ts(10_000), Bytes::new()).unwrap();
		between.finish().unwrap();
		let mut reads = Vec::new();
		while let Poll::Ready(read) = consumer.poll_read(&waiter) {
			reads.push(match read.unwrap() {
				Some(Read::Frame { group, frame }) => format!("frame {group} {}", frame.timestamp.as_micros().unwrap()),
				Some(Read::Gap { .. }) => "gap".to_string(),
				Some(Read::End { group, .. }) => format!("end {group}"),
				None => break,
			});
		}
		assert_eq!(reads, ["gap", "end 1", "frame 2 20000"]);
	}

	/// A hole's budget runs from when the group past it landed, not from when the reader pulled it:
	/// a full max buffer holds pulls back, so a hole that already expired is passed at once.
	#[tokio::test(start_paused = true)]
	async fn a_hole_behind_a_full_max_buffer_expires_from_its_successors_arrival() {
		let max_delay = Duration::from_millis(500);
		let track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let mut consumer = container_max_delay_only(track.subscribe(None), max_delay);
		consumer.set_max_buffer(4096);
		for sequence in 0..2u64 {
			let mut group = track.create_group(moq_net::group::Info { start: sequence }).unwrap();
			Container::Legacy(crate::container::Kind::Data)
				.write(
					&mut group,
					&[Frame {
						timestamp: ts(sequence * 10_000),
						payload: Bytes::from(vec![0; 4096]),
						keyframe: false,
						duration: None,
					}],
				)
				.unwrap();
			group.finish().unwrap();
		}
		let mut marker = track.create_group(moq_net::group::Info { start: 3 }).unwrap();
		marker.write_frame(ts(20_000), Bytes::new()).unwrap();
		marker.finish().unwrap();

		// The reader gets to the track only after the hole before group 3 has expired.
		tokio::time::advance(2 * max_delay).await;
		let waiter = kio::Waiter::noop();
		let mut reads = Vec::new();
		while let Poll::Ready(read) = consumer.poll_read(&waiter) {
			reads.push(match read.unwrap() {
				Some(Read::Frame { group, frame }) => format!("frame {group} {}", frame.timestamp.as_micros().unwrap()),
				Some(Read::Gap { .. }) => "gap".to_string(),
				Some(Read::End { group, .. }) => format!("end {group}"),
				None => break,
			});
		}
		assert_eq!(reads, ["frame 0 0", "frame 1 10000", "gap", "end 3"]);
	}

	/// A joining reader drains the backlog in sequence order, not arrival order, so a group can land
	/// before the one below it is pulled: the hole before that lower group is late from the earlier one.
	#[tokio::test(start_paused = true)]
	async fn a_hole_in_a_drained_backlog_is_late_from_the_earliest_arrival_past_it() {
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let marker = |track: &mut moq_net::track::Producer, sequence: u64| {
			let mut group = track.create_group(moq_net::group::Info { start: sequence }).unwrap();
			group.write_frame(ts(sequence * 10_000), Bytes::new()).unwrap();
			group.finish().unwrap();
		};
		write_group(&mut track, 0, &[ts(0)]);
		marker(&mut track, 5);
		tokio::time::advance(Duration::from_millis(900)).await;
		marker(&mut track, 3);
		tokio::time::advance(Duration::from_millis(100)).await;

		let mut consumer = container_max_delay_only(track.subscribe(None), Duration::from_millis(500));
		let waiter = kio::Waiter::noop();
		let mut reads = Vec::new();
		while let Poll::Ready(read) = consumer.poll_read(&waiter) {
			reads.push(match read.unwrap() {
				Some(Read::Frame { group, .. }) => format!("frame {group}"),
				Some(Read::Gap { .. }) => "gap".to_string(),
				Some(Read::End { group, .. }) => format!("end {group}"),
				None => break,
			});
		}
		// Group 5 landed a second ago, so the holes before groups 3 and 5 both expired.
		assert_eq!(reads, ["frame 0", "gap", "end 3", "gap", "end 5"]);
	}

	/// No cap is too small to read through: the cursor's group and one successor are always pulled, so
	/// a stalled group can be moved past even when one group's overhead fills the max buffer.
	#[kio::test]
	async fn a_zero_max_buffer_still_moves_past_a_stalled_group() {
		let waiter = kio::Waiter::noop();
		let mut track = track_producer("test", hang::container::track_info(hang::catalog::PRIORITY.video));
		let mut consumer = container_max_delay_only(track.subscribe(None), Duration::from_secs(5));
		consumer.set_max_buffer(0);

		let _stalled = open_group(&mut track, 0, ts(0));
		write_group(&mut track, 1, &[ts(10_000)]);

		let mut reads = Vec::new();
		while let Poll::Ready(read) = consumer.poll_read(&waiter) {
			match read.unwrap() {
				Some(Read::Frame { frame, .. }) => reads.push(Some(frame.timestamp)),
				Some(Read::Gap { .. }) => reads.push(None),
				other => panic!("unexpected read: {other:?}"),
			}
		}
		assert_eq!(reads, vec![Some(ts(0)), None, Some(ts(10_000))]);
	}

	/// A paused payload reader must not retain metadata for cache entries that were evicted.
	#[kio::test]
	async fn receiving_under_backpressure_releases_aborted_group_handles() {
		let track = track_producer("backpressured", timed());
		let mut consumer = Consumer::new(
			track.subscribe(moq_net::track::Subscription::default()),
			Container::Legacy(crate::container::Kind::Data),
		);
		for sequence in 0..256 {
			let mut group = track.create_group(moq_net::group::Info { start: sequence }).unwrap();
			Container::Legacy(crate::container::Kind::Data)
				.write(
					&mut group,
					&[Frame {
						timestamp: ts(sequence * 20_000),
						payload: Bytes::from_static(b"payload"),
						keyframe: true,
						duration: None,
					}],
				)
				.unwrap();
			assert!(consumer.poll_received(&kio::Waiter::noop()).is_pending());
			group.abort(moq_net::Error::Evicted).unwrap();
		}
		let _ = consumer.poll_received(&kio::Waiter::noop());
		assert!(
			consumer.pending.len() <= 2,
			"doubling compaction bounds metadata by twice the cursor's one retained gap, retained {} aborted group handles",
			consumer.pending.len()
		);
	}

	/// A finished track may still have bytes in flight on a group already received.
	#[kio::test]
	async fn received_input_waits_for_group_fin_without_consuming_payload() {
		let mut track = track_producer("finite", timed());
		let mut consumer = Consumer::new(
			track.subscribe(
				moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
					moq_net::track::Start::Live {
						min: Some(Duration::from_millis(500)),
						max: Some(Duration::from_millis(500)),
					},
				)]),
			),
			Container::Legacy(crate::container::Kind::Data),
		);
		write_group(&mut track, 0, &[ts(0)]);
		let mut group = track.create_group(moq_net::group::Info { start: 1 }).unwrap();
		Container::Legacy(crate::container::Kind::Data)
			.write(
				&mut group,
				&[Frame {
					timestamp: ts(20_000),
					payload: Bytes::from_static(b"tail"),
					keyframe: true,
					duration: None,
				}],
			)
			.unwrap();
		let _track = track.finish().unwrap();
		assert!(consumer.poll_received(&kio::Waiter::noop()).is_pending());
		assert!(
			consumer.pending.iter().all(|group| group.is_empty()),
			"receipt must not decode payload"
		);
		group.finish().unwrap();
		assert!(consumer.poll_received(&kio::Waiter::noop()).is_ready());
		let mut frames = Vec::new();
		while let Some(frame) = consumer.read_frame().await.unwrap() {
			frames.push(frame.timestamp);
		}
		assert_eq!(frames, [ts(0), ts(20_000)]);
	}
}
