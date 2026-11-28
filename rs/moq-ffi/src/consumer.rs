//! Read broadcasts, tracks, and groups.

use std::sync::Arc;
use std::task::{Poll, ready};
use std::time::Duration;

use crate::error::MoqError;
use crate::ffi::{Background, Polled};
use crate::media::*;
use crate::producer::MoqTrackInfo;

fn raw_frame(frame: moq_net::frame::Frame) -> MoqFrame {
	MoqFrame {
		payload: frame.payload.to_vec(),
		timestamp: frame.timestamp.into(),
	}
}

/// How far behind the live edge a subscriber plays, mirroring [`moq_mux::container::Delay`]:
/// where it starts, and how late it may fall.
///
/// [`moq_subscription_defaults`] carries moq-net's default: no `min` and a zero `max`. Uniffi
/// can't default a duration, so Kotlin and Swift name both fields, the Python, Go, and Dart
/// wrappers leave an omitted `min` unstated and default an omitted `max` to zero, and a bare C or
/// C++ record leaves both unset.
#[derive(Clone, Copy, Debug, uniffi::Record)]
pub struct MoqDelay {
	/// The target latency: the playout or jitter-buffer delay held behind the live edge, or null
	/// to leave it unstated. A range with no start is a live join once this is stated, zero
	/// included: it starts at the group containing the live edge minus `min`
	/// ([`Start::Live`](moq_net::track::Start::Live)). Unstated, such a range joins `max` behind
	/// the live edge, and is history from the oldest group when `max` is null too. A range with a
	/// start ignores it, and a missing group is given up by `max` alone. A media consumer starts an unstated minimum as zero. moq-lite-07 sends each live join
	/// upstream. A `min` past `max` starts a live join no further back than
	/// the group covering the point `max` behind the live edge, since the publisher serves no
	/// older group. An audio decoder refuses a `min` beyond the track's max age instead.
	pub min: Option<Duration>,
	/// How far a non-latest group may fall behind the live edge before it is skipped, and how
	/// far newer media may run past a missing group before it is given up, or null for no
	/// subscriber limit, leaving the publisher's max age alone to bound it. `0` skips immediately;
	/// a larger value tolerates that much reordering. It bounds lateness only, never the start.
	///
	/// Each live join carries it as its [`Start::Live`](moq_net::track::Start::Live) `max`, and
	/// any local buffering, such as [`MoqMediaTrackConsumer`]'s jitter buffer, applies it too. A
	/// range with a start is history, delivered in full whatever this is.
	pub max: Option<Duration>,
}

impl From<MoqDelay> for moq_mux::container::Delay {
	fn from(delay: MoqDelay) -> Self {
		Self::default().with_min(delay.min).with_max(delay.max)
	}
}

impl From<moq_mux::container::Delay> for MoqDelay {
	fn from(delay: moq_mux::container::Delay) -> Self {
		Self {
			min: delay.min,
			max: delay.max,
		}
	}
}

/// Subscriber-side delivery preferences, mirroring [`moq_net::track::Subscription`] with its
/// live joins stated as `delay.min`.
///
/// Start from [`moq_subscription_defaults`] for moq-net's defaults (priority 0, no `delay.min`
/// and a zero `delay.max`, everything still fresh, newest first) and change the fields you care
/// about.
#[derive(Clone, uniffi::Record)]
pub struct MoqSubscription {
	/// Delivery priority; higher values preempt lower ones under bandwidth contention.
	/// The default 0 is the midpoint.
	#[uniffi(default = 0)]
	pub priority: i8,
	/// How far behind the live edge this subscriber plays: where a range with no start begins,
	/// and how late a non-latest group may fall before it is skipped.
	pub delay: MoqDelay,
	/// The spans of the track to deliver, or null for one range with no start, which `delay`
	/// places: a live join `delay.min` behind the live edge, or everything `delay.max` still
	/// considers fresh when `delay.min` is unstated. An empty list asks for nothing.
	///
	/// A request to the publisher, aggregated across every subscriber, and this
	/// subscription's read filter: it reads only the groups its own ranges cover.
	#[uniffi(default = None)]
	pub ranges: Option<Vec<MoqRange>>,
	/// Which group comes first, as the publisher sends them and `recv_group` returns those held,
	/// or null for newest first. Before moq-lite-07 the publisher sends newest first whatever this
	/// asks, and `recv_group` still returns those held in this order.
	#[uniffi(default = None)]
	pub order: Option<MoqOrder>,
}

/// A subscription with moq-net's defaults, which starts at the newest group and skips a
/// superseded group at once.
///
/// A null subscription argument means the same; build from this one to change a field.
#[uniffi::export]
pub fn moq_subscription_defaults() -> MoqSubscription {
	MoqSubscription {
		priority: moq_net::track::Subscription::default().priority,
		delay: moq_mux::container::Delay::default().into(),
		ranges: None,
		order: None,
	}
}

/// A span of a track to deliver, mirroring [`moq_net::track::Range`]: from the group containing
/// `start` through the group containing `end`.
///
/// Every start is the first frame of its group, and every end includes its whole group. A time
/// names the group containing it, the newest one starting at or before it unless that group ended
/// whole at or before the time, so a group's start names exactly it. A start in the pause past a
/// whole group's end begins at the next group. Both are given [unscaled](MoqTimestamp::unscaled), in the track's
/// timescale; a scaled one is refused with a timestamp mismatch until the bindings convert it.
///
/// Over a session older than moq-lite-07, a start between two groups reaches the publisher as
/// the first group at or after it, missing the one containing it.
#[derive(Clone, Copy, uniffi::Record)]
pub struct MoqRange {
	/// Where delivery starts, or null for no start, which the subscription's [`MoqDelay`]
	/// places: a stated `min` is a live join that far behind the live edge (zero is the latest
	/// group), and an unstated one joins `max` behind it. A start is history: every group from it
	/// on is delivered, bounded only by the publisher's max age.
	#[uniffi(default = None)]
	pub start: Option<MoqTimestamp>,
	/// Where delivery stops, including the group containing it, or null to keep following the
	/// live edge.
	#[uniffi(default = None)]
	pub end: Option<MoqTimestamp>,
}

/// Which of a subscription's groups comes first, as the publisher sends them and `recv_group`
/// returns those held, mirroring [`moq_net::track::Order`]. Before moq-lite-07 the publisher
/// sends newest first whatever this asks.
#[derive(Clone, Copy, uniffi::Enum)]
pub enum MoqOrder {
	/// Oldest first, draining a backlog in start order.
	Ascending,
	/// Newest first, so congestion sheds the backlog rather than the live edge.
	Descending,
}

/// A range's time as moq-net reads it. It must be unscaled, in the track's timescale: the
/// timescale is unknown here, so a scaled one is refused rather than read in the wrong units.
fn range_time(timestamp: MoqTimestamp) -> Result<u64, MoqError> {
	match timestamp.scale {
		None => Ok(timestamp.value),
		Some(_) => Err(moq_net::Error::TimestampMismatch.into()),
	}
}

impl MoqRange {
	/// This range as moq-net reads it.
	pub(crate) fn resolve(self) -> Result<moq_net::track::Range, MoqError> {
		Ok(moq_net::track::Range {
			start: match self.start {
				Some(start) => moq_net::track::Start::Time(range_time(start)?),
				None => moq_net::track::Start::Oldest,
			},
			end: self.end.map(range_time).transpose()?,
		})
	}
}

impl From<MoqOrder> for moq_net::track::Order {
	fn from(order: MoqOrder) -> Self {
		match order {
			MoqOrder::Ascending => Self::Ascending,
			MoqOrder::Descending => Self::Descending,
		}
	}
}

impl From<moq_net::track::Order> for MoqOrder {
	fn from(order: moq_net::track::Order) -> Self {
		match order {
			moq_net::track::Order::Ascending => Self::Ascending,
			moq_net::track::Order::Descending => Self::Descending,
		}
	}
}

/// Options for fetching one past group by start.
#[derive(Clone, uniffi::Record)]
pub struct MoqFetchGroupOptions {
	/// Delivery priority for the fetch stream; higher values preempt lower ones.
	/// The default 0 is the midpoint.
	#[uniffi(default = 0)]
	pub priority: i8,
}

impl From<MoqFetchGroupOptions> for moq_net::group::Fetch {
	fn from(options: MoqFetchGroupOptions) -> Self {
		moq_net::group::Fetch::default().with_priority(options.priority)
	}
}

impl MoqSubscription {
	/// The subscription without its delay, each range with no start still from the oldest group,
	/// and that delay, for a caller widening it before it subscribes.
	pub(crate) fn resolve(self) -> Result<(moq_net::track::Subscription, moq_mux::container::Delay), MoqError> {
		let mut subscription = moq_net::track::Subscription::default()
			.with_priority(self.priority)
			.with_ranges([moq_net::track::Range::ALL]);
		if let Some(ranges) = self.ranges {
			let ranges = ranges
				.into_iter()
				.map(MoqRange::resolve)
				.collect::<Result<Vec<_>, _>>()?;
			subscription = subscription.with_ranges(ranges);
		}
		if let Some(order) = self.order {
			subscription = subscription.with_order(order.into());
		}
		Ok((subscription, self.delay.into()))
	}
}

/// A stated `delay.min` joins each range with no start live that far behind the live edge.
impl TryFrom<MoqSubscription> for moq_net::track::Subscription {
	type Error = MoqError;

	fn try_from(s: MoqSubscription) -> Result<Self, MoqError> {
		let (subscription, delay) = s.resolve()?;
		Ok(delay.subscription(subscription))
	}
}

/// A broadcast from which tracks and groups can be read.
#[derive(Clone, uniffi::Object)]
pub struct MoqBroadcastConsumer {
	pub(crate) inner: moq_net::broadcast::Consumer,
	/// The origin this broadcast was resolved through, if any. A catalog rendition may name a
	/// sibling broadcast, and only an origin can fetch one; a standalone broadcast (a local
	/// producer's `consume`) has none, so such a rendition is unresolvable rather than wrong.
	pub(crate) origin: Option<moq_net::origin::Consumer>,
	pub(crate) driver: Background,
}

impl MoqBroadcastConsumer {
	/// Wrap a standalone broadcast, with no origin to resolve cross-broadcast references against.
	#[cfg(test)]
	pub(crate) fn new(inner: moq_net::broadcast::Consumer) -> Self {
		Self {
			inner,
			origin: None,
			driver: Background::default(),
		}
	}

	/// Wrap a broadcast the origin handed out, so a rendition referencing a sibling broadcast
	/// resolves through the same origin (which deduplicates the shared subscription).
	///
	/// `origin` must be the cursor that named `inner`: the broadcast's path is stamped per
	/// cursor, and a reference resolves against that path, so an origin rooted elsewhere would
	/// read a legal reference as escaping (or worse, resolve it to the wrong broadcast).
	pub(crate) fn routed(
		inner: moq_net::broadcast::Consumer,
		origin: moq_net::origin::Consumer,
		driver: Background,
	) -> Self {
		Self {
			inner,
			origin: Some(origin),
			driver,
		}
	}

	#[cfg(test)]
	pub(crate) fn inner(&self) -> &moq_net::broadcast::Consumer {
		&self.inner
	}

	/// Resolve a catalog rendition's `broadcast` reference to the broadcast serving its track.
	pub(crate) async fn resolve_inner(
		&self,
		reference: Option<&str>,
	) -> Result<moq_net::broadcast::Consumer, MoqError> {
		self.driver
			.watch(async {
				// A caller-supplied string, refused unless it is spelled as a catalog's reference must be.
				let reference = reference
					.map(moq_net::path::Relative::new)
					.transpose()
					.map_err(moq_net::Error::from)?;

				let Some(origin) = self.origin.clone() else {
					// A standalone broadcast has no sibling to name: only its own broadcast resolves.
					return match reference.filter(|reference| !reference.is_empty()) {
						None => Ok(self.inner.clone()),
						Some(reference) => Err(MoqError::UnresolvableBroadcast {
							reason: reference.as_str().to_string(),
						}),
					};
				};

				let source = moq_mux::Source::new(origin, self.inner.clone());
				Ok(source.resolve(reference.as_ref()).await?)
			})
			.await
	}
}

// ---- Broadcast ----

#[uniffi::export]
impl MoqBroadcastConsumer {
	/// Resolve a catalog rendition to the broadcast serving its track.
	///
	/// The rendition's `broadcast` reference, absent or empty, names this broadcast; anything
	/// else names a sibling relative to it (e.g. `./source`). Call it on a rendition that
	/// carries a reference before [`Self::subscribe_track`] or
	/// [`Self::fetch_group`], which take the rendition's `track` (or its ID when unset) rather
	/// than a rendition. The rendition-based consumers (`MoqMediaTrackConsumer::subscribe`,
	/// `MoqMediaGroupConsumer::fetch`, `MoqVideoConsumer::subscribe`,
	/// `MoqAudioConsumer::subscribe`) resolve it themselves.
	///
	/// Errors if this broadcast came from a local producer rather than an origin, since a
	/// standalone broadcast has no sibling to name. A sibling nothing announces yet is waited for,
	/// so a late one resolves once it is announced; cancel the call to give up on it.
	pub async fn resolve(&self, rendition: MoqRendition) -> Result<Arc<MoqBroadcastConsumer>, MoqError> {
		let (reference, _, _) = rendition.into_parts();
		let broadcast = self.resolve_inner(reference.as_deref()).await?;
		Ok(Arc::new(match self.origin.clone() {
			Some(origin) => Self::routed(broadcast, origin, self.driver.clone()),
			None => Self {
				inner: broadcast,
				origin: None,
				driver: self.driver.clone(),
			},
		}))
	}

	/// Wait until the broadcast ends: its publisher closed it, or every producer of it left.
	///
	/// Resolves rather than erroring when it ends, since an ended broadcast is not a failure.
	/// Errors only when the session driving it fails first. Cancel the call to stop waiting.
	pub async fn closed(&self) -> Result<(), MoqError> {
		self.driver
			.watch(async {
				self.inner.closed().await;
				Ok(())
			})
			.await
	}

	/// Subscribe to a track by name, the same pattern as moq-boy's command/status tracks.
	///
	/// Frames are returned as plain byte payloads with no codec or container parsing.
	/// `subscription` tunes delivery priority, group range, and staleness; omit for defaults.
	pub async fn subscribe_track(
		&self,
		name: String,
		subscription: Option<MoqSubscription>,
	) -> Result<Arc<MoqTrackConsumer>, MoqError> {
		self.driver
			.watch(async {
				let subscription = subscription.map(moq_net::track::Subscription::try_from).transpose()?;
				let track = self.inner.track(&name)?.subscribe(subscription).await?;
				Ok(Arc::new(MoqTrackConsumer::new(track, self.driver.clone())))
			})
			.await
	}

	/// Subscribe to a track's metadata by name: its groups and each frame's header, without the
	/// payload.
	///
	/// A subscription of its own beside any [`Self::subscribe_track`] of the same track. Over
	/// moq-lite-07 no payload crosses a hop for it; over an older version it rides the track's
	/// payload subscription and drops the payload. To read the payload after all, subscribe to the
	/// track from the group reached. `subscription` works as in [`Self::subscribe_track`].
	pub async fn subscribe_meta(
		&self,
		name: String,
		subscription: Option<MoqSubscription>,
	) -> Result<Arc<MoqMetaConsumer>, MoqError> {
		self.driver
			.watch(async {
				let subscription = subscription.map(moq_net::track::Subscription::try_from).transpose()?;
				let track = self.inner.meta(&name)?.subscribe(subscription).await?;
				Ok(Arc::new(MoqMetaConsumer::new(track, self.driver.clone())))
			})
			.await
	}

	/// Fetch one complete group by track name and group start.
	///
	/// This does not create a live subscription. A retained group resolves immediately;
	/// otherwise the request waits for a dynamic producer to serve it. The returned
	/// group may still be in progress, so read frames until `read_frame()` returns `None`.
	pub async fn fetch_group(
		&self,
		name: String,
		start: u64,
		options: Option<MoqFetchGroupOptions>,
	) -> Result<Arc<MoqGroupConsumer>, MoqError> {
		self.driver
			.watch(async {
				let options = options.map(moq_net::group::Fetch::from);
				let track = self.inner.track(&name).map_err(map_fetch_error)?;
				let group = track.fetch_group(start, options).await.map_err(map_fetch_error)?;
				Ok(Arc::new(MoqGroupConsumer::new(group, self.driver.clone())))
			})
			.await
	}
}

pub(crate) fn map_fetch_error(err: moq_net::Error) -> MoqError {
	match err {
		moq_net::Error::NotFound => MoqError::NotFound,
		moq_net::Error::Unsupported | moq_net::Error::Version => MoqError::Unsupported,
		err => err.into(),
	}
}

// ---- Track Consumer ----

/// A track subscription reading one way or the other.
///
/// `moq_net` makes the choice a handle you consume the subscriber into, which a foreign
/// binding can't express in its type system, so the handle commits on the first group
/// read instead: whichever group cursor a caller reaches for first is the one this
/// track keeps, and reaching for the other afterwards is [`MoqError::AlreadyCommitted`].
enum Cursor {
	/// No group read has happened yet; the track can still commit either way.
	Uncommitted(moq_net::track::Subscriber),
	/// Committed to arrival order by the first `recv_group`.
	Arrival(moq_net::track::Subscriber),
	/// Committed to start order by the first `next_group` or `read_frame`.
	Ordered(Box<OrderedCursor>),
	/// Transient, while the subscriber is moved out to be converted.
	Converting,
}

/// Sequence-order handle plus the group `read_frame` is in the middle of.
///
/// `pending` stays with this cursor so a cancelled `read_frame` does not drop
/// the group, and so `next_group` takes it rather than skipping ahead.
struct OrderedCursor {
	track: moq_net::track::Ordered,
	/// Group whose first frame `read_frame` has not returned yet.
	pending: Option<moq_net::group::Consumer>,
}

struct TrackInner {
	track: Cursor,
}

impl TrackInner {
	/// The arrival cursor, committing this track to it, or [`MoqError::AlreadyCommitted`]
	/// once it committed to start order.
	fn arrival(&mut self) -> Result<&mut moq_net::track::Subscriber, MoqError> {
		if let Cursor::Uncommitted(_) = &self.track {
			let Cursor::Uncommitted(track) = std::mem::replace(&mut self.track, Cursor::Converting) else {
				unreachable!("just matched Uncommitted");
			};
			self.track = Cursor::Arrival(track);
		}
		match &mut self.track {
			Cursor::Arrival(track) => Ok(track),
			Cursor::Ordered(_) => Err(MoqError::AlreadyCommitted),
			// Only reachable if a previous conversion unwound mid-swap, leaving the
			// handle with no subscriber to read: poisoned, not misused.
			Cursor::Uncommitted(_) | Cursor::Converting => Err(MoqError::Closed),
		}
	}

	/// The ordered cursor, committing this track to it, or [`MoqError::AlreadyCommitted`]
	/// once it committed to arrival order.
	fn ordered(&mut self) -> Result<&mut OrderedCursor, MoqError> {
		// Only move the subscriber out when there is one to convert: `ordered()` consumes
		// it, and swapping unconditionally would drop an already-converted handle.
		if let Cursor::Uncommitted(_) = &self.track {
			let Cursor::Uncommitted(track) = std::mem::replace(&mut self.track, Cursor::Converting) else {
				unreachable!("just matched Uncommitted");
			};
			self.track = Cursor::Ordered(Box::new(OrderedCursor {
				track: track.ordered(),
				pending: None,
			}));
		}
		match &mut self.track {
			Cursor::Ordered(track) => Ok(track.as_mut()),
			Cursor::Arrival(_) => Err(MoqError::AlreadyCommitted),
			// See `arrival`: unreachable unless the handle was left mid-conversion.
			Cursor::Uncommitted(_) | Cursor::Converting => Err(MoqError::Closed),
		}
	}

	fn poll_recv_group(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<moq_net::group::Consumer>, MoqError>> {
		match self.arrival() {
			Ok(track) => track.poll_recv_group(waiter).map(|r| r.map_err(Into::into)),
			Err(e) => Poll::Ready(Err(e)),
		}
	}

	fn poll_next_group(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<moq_net::group::Consumer>, MoqError>> {
		match self.ordered() {
			Ok(ordered) => {
				if let Some(group) = ordered.pending.take() {
					// A frame reader parked on it would otherwise wait for a frame it no longer
					// reads. The waker wakes every call parked on this handle, once it unlocks.
					waiter.waker().wake_by_ref();
					return Poll::Ready(Ok(Some(group)));
				}
				// The bindings report no gaps, so a group the track gave up on is passed over.
				ordered.track.poll_next_group(waiter).map_err(Into::into)
			}
			Err(e) => Poll::Ready(Err(e)),
		}
	}

	/// The first frame of the next group in start order, skipping completed empty and failed groups.
	///
	/// One poll does the whole step, so it never trusts a slot it filled on an earlier poll:
	/// a group `next_group` took from `pending` in between is simply gone, and the wake it
	/// sends makes this acquire the next one. A group still waiting on its first frame stays
	/// in `pending`, so a cancelled call leaves it for the next `read_frame` or `next_group`.
	fn poll_read_frame(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<moq_net::frame::Frame>, MoqError>> {
		let ordered = match self.ordered() {
			Ok(ordered) => ordered,
			Err(e) => return Poll::Ready(Err(e)),
		};
		loop {
			let group = match &mut ordered.pending {
				Some(group) => group,
				None => match ready!(ordered.track.poll_next_group(waiter)) {
					Ok(Some(group)) => ordered.pending.insert(group),
					Ok(None) => return Poll::Ready(Ok(None)),
					Err(err) => return Poll::Ready(Err(err.into())),
				},
			};
			let result = ready!(group.poll_read_frame(waiter));
			// Drop a completed or failed group so the next pass moves on.
			ordered.pending = None;
			match result {
				Ok(Some(frame)) => return Poll::Ready(Ok(Some(frame))),
				// An empty group, or a failed one, which is that group's gap: only the track's own
				// end, from the next group read, ends this read.
				Ok(None) | Err(_) => continue,
			}
		}
	}
}

/// A subscribed stream track whose groups and frames can be read.
#[derive(uniffi::Object)]
pub struct MoqTrackConsumer {
	/// Reads poll in place without holding a lock across a wait.
	reads: Polled<TrackInner>,
	pub(crate) control: moq_net::track::Control,
	info: moq_net::track::Info,
	/// The owner whose panic ends this track's reads, handed to every group and reader taken from it.
	driver: Background,
}

impl MoqTrackConsumer {
	pub(crate) fn new(track: moq_net::track::Subscriber, driver: Background) -> Self {
		let control = track.control();
		let info = track.info().clone();
		Self {
			reads: Polled::new(TrackInner {
				track: Cursor::Uncommitted(track),
			})
			.supervised(driver.clone()),
			control,
			info,
			driver,
		}
	}

	/// Hand the subscription to a typed reader (a JSON consumer, say), closing this handle.
	///
	/// Errors with [`MoqError::AlreadyCommitted`] once a group read has started, since the reader
	/// would miss whatever that read consumed.
	pub(crate) fn take(&self) -> Result<(moq_net::track::Subscriber, Background), MoqError> {
		let inner = self
			.reads
			.take(|inner| match inner.track {
				Cursor::Uncommitted(_) => Ok(()),
				_ => Err(MoqError::AlreadyCommitted),
			})
			.map_err(|err| match err {
				MoqError::Cancelled => MoqError::Closed,
				err => err,
			})?;
		let Cursor::Uncommitted(track) = inner.track else {
			unreachable!("just checked Uncommitted");
		};
		Ok((track, self.driver.clone()))
	}

	fn group(&self, group: Option<moq_net::group::Consumer>) -> Option<Arc<MoqGroupConsumer>> {
		group.map(|group| Arc::new(MoqGroupConsumer::new(group, self.driver.clone())))
	}
}

#[uniffi::export]
impl MoqTrackConsumer {
	/// Return the next group in arrival order. Returns `None` when the track ends.
	///
	/// Groups are returned as they arrive on the wire, which may be out of start
	/// order (e.g. if a later group lands before an earlier one on a separate stream).
	/// Of the groups already held, it returns them in the subscription's group order:
	/// newest first unless the subscription asks ascending. One that drifts further behind the live edge than the subscription's `delay.max`
	/// is skipped, or ends its pending read if already returned; raise it to keep stalled groups.
	///
	/// The first call commits this track to arrival order: start-order reads
	/// ([`Self::next_group`], [`Self::read_frame`]) fail with
	/// [`MoqError::AlreadyCommitted`] afterwards.
	pub async fn recv_group(&self) -> Result<Option<Arc<MoqGroupConsumer>>, MoqError> {
		let group = self.reads.wait(|inner, waiter| inner.poll_recv_group(waiter)).await?;
		Ok(self.group(group))
	}

	/// Return the next group with a later start than any previously
	/// returned, skipping late arrivals. Returns `None` when the track ends.
	///
	/// Shares the ordered cursor with [`Self::read_frame`]: a group one method
	/// has already taken is not returned by the other. A `read_frame` cancelled
	/// after acquiring a group leaves that group here.
	///
	/// The first call commits this track to start order: arrival-order reads
	/// ([`Self::recv_group`]) fail with [`MoqError::AlreadyCommitted`] afterwards.
	pub async fn next_group(&self) -> Result<Option<Arc<MoqGroupConsumer>>, MoqError> {
		let group = self.reads.wait(|inner, waiter| inner.poll_next_group(waiter)).await?;
		Ok(self.group(group))
	}

	/// Read the first frame of the next group, including its timestamp.
	///
	/// Convenience for tracks using one-frame-per-group (like moq-boy's
	/// status/command tracks). Completed empty groups are skipped, and so is a group that
	/// fails, which is that group's gap. Returns `None` only when the track ends, and an error
	/// only when it ends with one. Cancelling one call keeps the current group so a later
	/// `read_frame` or [`Self::next_group`] still sees it.
	///
	/// Shares the ordered cursor with [`Self::next_group`], committing the track
	/// the same way.
	pub async fn read_frame(&self) -> Result<Option<MoqFrame>, MoqError> {
		let frame = self.reads.wait(|inner, waiter| inner.poll_read_frame(waiter)).await?;
		Ok(frame.map(raw_frame))
	}

	/// Return the publisher-side track properties learned during subscription.
	pub fn info(&self) -> MoqTrackInfo {
		MoqTrackInfo::from(&self.info)
	}

	/// Change this subscriber's delivery preferences.
	///
	/// A range with no start that the subscription already asks, with the same end, keeps the
	/// live join it was subscribed with: a changed `delay.min` joins only the ranges it adds.
	///
	/// Silently ignored if the track already ended; the update is meaningless at
	/// that point. Errors on a range time the track can't serve, leaving the
	/// preferences as they were.
	pub fn update(&self, subscription: MoqSubscription) -> Result<(), MoqError> {
		let (subscription, delay) = subscription.resolve()?;
		let subscription = delay.update(&self.control.subscription(), subscription);
		match self.control.update(subscription) {
			Ok(()) | Err(moq_net::Error::Closed) => {}
			Err(err) => return Err(err.into()),
		}
		Ok(())
	}

	/// Ask the publisher for a group starting after the newest one this node holds, judged by
	/// the track's [`MoqTrackRefresh`](crate::producer::MoqTrackRefresh), returning once the
	/// request is sent.
	///
	/// It is sent once the newest group is old enough, when whatever serves the track reads
	/// it: the session forwarding it upstream, or a local publisher waiting in
	/// [`MoqTrackDemand::refresh`](crate::demand::MoqTrackDemand::refresh). Concurrent calls
	/// share one request and all return when it is sent. Cancelling the call before then
	/// withdraws its request. It returns at once when there is nothing to send: a track that
	/// does not refresh, a ratio the newest group is under, a track with no group yet, or a
	/// request already sent. It also returns when a newer group lands first. The new group
	/// arrives through the normal reads, such as [`Self::next_group`].
	pub async fn refresh(&self) -> Result<(), MoqError> {
		crate::ffi::run(self.control.refresh()).await
	}

	/// Cancel all current and future reads.
	///
	/// Terminal: the subscription is released here, not when the handle is.
	pub fn cancel(&self) {
		self.reads.cancel();
	}
}

/// A group whose frames can be read in order.
#[derive(uniffi::Object)]
pub struct MoqGroupConsumer {
	start: u64,
	reads: Polled<moq_net::group::Consumer>,
	driver: Background,
}

impl MoqGroupConsumer {
	pub(crate) fn new(group: moq_net::group::Consumer, driver: Background) -> Self {
		Self {
			start: group.start,
			reads: Polled::new(group).supervised(driver.clone()),
			driver,
		}
	}

	/// Hand the group to a typed reader (a media group consumer, say), closing this handle.
	///
	/// Errors with [`MoqError::AlreadyCommitted`] once a frame has been read, since the reader
	/// would start mid-group, and [`MoqError::Busy`] while a read is in flight.
	pub(crate) fn take(&self) -> Result<(moq_net::group::Consumer, Background), MoqError> {
		if self.reads.busy() {
			return Err(MoqError::Busy);
		}
		let group = self.reads.take(|group| match group.index() {
			0 => Ok(()),
			_ => Err(MoqError::AlreadyCommitted),
		})?;
		Ok((group, self.driver.clone()))
	}
}

#[uniffi::export]
impl MoqGroupConsumer {
	/// The start of this group, in its track's timescale, which names it within the track.
	pub fn start(&self) -> u64 {
		self.start
	}

	/// Read the next frame in this group, including its timestamp.
	///
	/// Returns `None` when the group ends.
	pub async fn read_frame(&self) -> Result<Option<MoqFrame>, MoqError> {
		let frame = self
			.reads
			.wait(|group, waiter| Poll::Ready(Ok(ready!(group.poll_read_frame(waiter))?)))
			.await?;
		Ok(frame.map(raw_frame))
	}

	/// Cancel all current and future `read_frame()` calls.
	///
	/// Terminal: the group and whatever it still buffers are released here, not when the handle is.
	pub fn cancel(&self) {
		self.reads.cancel();
	}
}

// ---- Metadata Consumer ----

/// A metadata subscription: a track's groups, each frame as its header alone.
#[derive(uniffi::Object)]
pub struct MoqMetaConsumer {
	reads: Polled<moq_net::meta::Subscriber>,
	control: moq_net::track::Control,
	info: moq_net::track::Info,
	driver: Background,
}

impl MoqMetaConsumer {
	pub(crate) fn new(track: moq_net::meta::Subscriber, driver: Background) -> Self {
		let control = track.control();
		let info = track.info().clone();
		Self {
			reads: Polled::new(track).supervised(driver.clone()),
			control,
			info,
			driver,
		}
	}
}

#[uniffi::export]
impl MoqMetaConsumer {
	/// Return the next group in arrival order, as [`MoqTrackConsumer::recv_group`] does.
	/// Returns `None` when the track ends.
	pub async fn recv_group(&self) -> Result<Option<Arc<MoqMetaGroup>>, MoqError> {
		let group = self
			.reads
			.wait(|track, waiter| Poll::Ready(Ok(ready!(track.poll_recv_group(waiter))?)))
			.await?;
		Ok(group.map(|group| Arc::new(MoqMetaGroup::new(group, self.driver.clone()))))
	}

	/// Return the publisher-side track properties learned during subscription.
	pub fn info(&self) -> MoqTrackInfo {
		MoqTrackInfo::from(&self.info)
	}

	/// Change this subscriber's delivery preferences, as [`MoqTrackConsumer::update`] does.
	pub fn update(&self, subscription: MoqSubscription) -> Result<(), MoqError> {
		let (subscription, delay) = subscription.resolve()?;
		let subscription = delay.update(&self.control.subscription(), subscription);
		match self.control.update(subscription) {
			Ok(()) | Err(moq_net::Error::Closed) => {}
			Err(err) => return Err(err.into()),
		}
		Ok(())
	}

	/// Cancel all current and future reads.
	///
	/// Terminal: the subscription is released here, not when the handle is.
	pub fn cancel(&self) {
		self.reads.cancel();
	}
}

/// One group of a metadata subscription, read header by header.
#[derive(uniffi::Object)]
pub struct MoqMetaGroup {
	start: u64,
	reads: Polled<moq_net::meta::Group>,
}

impl MoqMetaGroup {
	fn new(group: moq_net::meta::Group, driver: Background) -> Self {
		Self {
			start: group.start,
			reads: Polled::new(group).supervised(driver),
		}
	}
}

#[uniffi::export]
impl MoqMetaGroup {
	/// The start of this group, in its track's timescale, which names it within the track.
	pub fn start(&self) -> u64 {
		self.start
	}

	/// Read the next frame's header: its declared size and timestamp, without its payload.
	///
	/// Returns `None` when the group ends.
	pub async fn next_header(&self) -> Result<Option<MoqFrameHeader>, MoqError> {
		let header = self
			.reads
			.wait(|group, waiter| Poll::Ready(Ok(ready!(group.poll_next_header(waiter))?)))
			.await?;
		Ok(header.map(|header| MoqFrameHeader {
			size: header.size,
			timestamp: header.timestamp.into(),
		}))
	}

	/// Cancel all current and future reads.
	///
	/// Terminal: the group is released here, not when the handle is.
	pub fn cancel(&self) {
		self.reads.cancel();
	}
}
