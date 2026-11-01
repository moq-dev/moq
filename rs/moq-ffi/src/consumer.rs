//! Read broadcasts, tracks, and groups.

use std::sync::Arc;
use std::task::{Poll, ready};
use std::time::Duration;

use crate::error::MoqError;
use crate::ffi::{Background, Polled};
use crate::media::*;
use crate::producer::MoqTrackInfo;

pub(crate) fn timestamp_us(timestamp: moq_net::Timestamp) -> Result<u64, MoqError> {
	timestamp.as_micros().try_into().map_err(|_| MoqError::TimeOverflow)
}

fn raw_frame(frame: moq_net::frame::Frame) -> Result<MoqFrame, MoqError> {
	let timestamp_us = frame.timestamp.map(timestamp_us).transpose()?;
	Ok(MoqFrame {
		payload: frame.payload.to_vec(),
		timestamp_us,
	})
}

/// Subscriber-side delivery preferences, mirroring [`moq_net::track::Subscription`].
///
/// Start from [`moq_subscription_defaults`] for moq-net's defaults (priority 0, a `max_delay`
/// of 0, everything still fresh, newest first) and change the fields you care about. Kotlin and
/// Swift make a record built field by field name `max_delay`; Dart, Go, and C can still omit it,
/// which means no subscriber limit.
#[derive(Clone, uniffi::Record)]
pub struct MoqSubscription {
	/// Delivery priority; higher values preempt lower ones under bandwidth contention.
	/// The default 0 is the midpoint.
	#[uniffi(default = 0)]
	pub priority: i8,
	/// Maximum age of a non-latest group before it is skipped.
	/// `0` skips immediately; a larger value tolerates that much reordering; null sets no
	/// subscriber limit and leaves the publisher's max age alone to bound it. Required,
	/// since uniffi can't default a duration to `0`.
	///
	/// Enforced both by the publisher's cache (sent on the wire) and by any local
	/// buffering, such as [`MoqMediaTrackConsumer`]'s jitter buffer. It caps every range.
	pub max_delay: Option<Duration>,
	/// The spans of the track to deliver, or null for everything `max_delay` still
	/// considers fresh. An empty list asks for nothing.
	///
	/// A request to the publisher, aggregated across every subscriber, and this
	/// subscription's read filter: it reads only the groups its own ranges cover.
	#[uniffi(default = None)]
	pub ranges: Option<Vec<MoqRange>>,
	/// Which pending group the publisher sends first, or null for newest first.
	#[uniffi(default = None)]
	pub order: Option<MoqOrder>,
}

/// A subscription with moq-net's defaults, which skips a superseded group at once.
///
/// A null subscription argument means the same; build from this one to change a field.
#[uniffi::export]
pub fn moq_subscription_defaults() -> MoqSubscription {
	let defaults = moq_net::track::Subscription::default();
	MoqSubscription {
		priority: defaults.priority,
		max_delay: defaults.max_delay,
		ranges: None,
		order: None,
	}
}

/// A span of a track to deliver: from `start` up to `end`, or onward when `end` is null.
///
/// A relative or time start is the first frame of its group, and a relative or time end
/// includes its whole group. An absolute end is exclusive.
#[derive(Clone, Copy, uniffi::Record)]
pub struct MoqRange {
	/// Where delivery starts.
	pub start: MoqAnchor,
	/// Where delivery stops, or null to keep following the live edge.
	#[uniffi(default = None)]
	pub end: Option<MoqAnchor>,
}

/// Where one end of a [`MoqRange`] falls, mirroring [`moq_net::track::Anchor`].
#[derive(Clone, Copy, uniffi::Enum)]
pub enum MoqAnchor {
	/// This group, delivered from its first frame.
	Absolute {
		/// The absolute group sequence number.
		group: u64,
	},
	/// The group this many before the latest, clamped to group 0: `0` is the latest group.
	Relative {
		/// The number of groups back from the live edge.
		groups: u64,
	},
	/// The group containing this presentation time. An untimed track refuses it.
	Time {
		/// The presentation time in microseconds.
		timestamp_us: u64,
	},
}

/// Which of a subscription's pending groups the publisher sends first, mirroring
/// [`moq_net::track::Order`].
#[derive(Clone, Copy, uniffi::Enum)]
pub enum MoqOrder {
	/// Oldest first, draining a backlog in sequence order.
	Ascending,
	/// Newest first, so congestion sheds the backlog rather than the live edge.
	Descending,
}

impl TryFrom<MoqAnchor> for moq_net::track::Anchor {
	type Error = MoqError;

	fn try_from(anchor: MoqAnchor) -> Result<Self, MoqError> {
		Ok(match anchor {
			MoqAnchor::Absolute { group } => Self::Absolute(group),
			MoqAnchor::Relative { groups } => Self::Relative(groups),
			MoqAnchor::Time { timestamp_us } => {
				Self::Time(moq_net::Timestamp::from_micros(timestamp_us).map_err(|_| MoqError::TimeOverflow)?)
			}
		})
	}
}

impl TryFrom<MoqRange> for moq_net::track::Range {
	type Error = MoqError;

	fn try_from(range: MoqRange) -> Result<Self, MoqError> {
		Ok(Self {
			start: range.start.try_into()?,
			end: range.end.map(TryInto::try_into).transpose()?,
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

/// Options for fetching one past group by sequence.
#[derive(Clone, uniffi::Record)]
pub struct MoqFetchGroupOptions {
	/// Delivery priority for the fetch stream; higher values preempt lower ones.
	/// The default 0 is the midpoint.
	#[uniffi(default = 0)]
	pub priority: i8,
	/// Maximum age behind the live edge; null uses publisher retention alone.
	#[uniffi(default = None)]
	pub max_delay: Option<Duration>,
}

impl From<MoqFetchGroupOptions> for moq_net::group::Fetch {
	fn from(options: MoqFetchGroupOptions) -> Self {
		let fetch = moq_net::group::Fetch::default().with_priority(options.priority);
		match options.max_delay {
			Some(max_delay) => fetch.with_max_delay(max_delay),
			None => fetch,
		}
	}
}

impl TryFrom<MoqSubscription> for moq_net::track::Subscription {
	type Error = MoqError;

	fn try_from(s: MoqSubscription) -> Result<Self, MoqError> {
		let mut subscription = moq_net::track::Subscription::default()
			.with_priority(s.priority)
			.with_max_delay(s.max_delay);
		if let Some(ranges) = s.ranges {
			subscription = subscription.with_ranges(
				ranges
					.into_iter()
					.map(TryInto::try_into)
					.collect::<Result<Vec<_>, _>>()?,
			);
		}
		if let Some(order) = s.order {
			subscription = subscription.with_order(order.into());
		}
		Ok(subscription)
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

	/// Resolve a catalog rendition's `broadcast` reference to the broadcast serving its track,
	/// refusing one proven to serve another instance than `epoch`.
	pub(crate) async fn resolve_inner(
		&self,
		reference: Option<&str>,
		epoch: Option<&str>,
	) -> Result<moq_net::broadcast::Consumer, MoqError> {
		self.driver
			.watch(async {
				// A caller-supplied string, refused unless it is spelled as a catalog's reference must be.
				let reference = reference
					.map(moq_net::path::Relative::new)
					.transpose()
					.map_err(moq_net::Error::from)?;
				let epoch: Option<moq_net::Epoch> = epoch.map(str::parse).transpose()?;

				let Some(origin) = self.origin.clone() else {
					// A standalone broadcast came through no route, so it has no epoch to prove: the
					// epoch is best effort, as on an epochless route. Only its own broadcast resolves.
					return match reference.filter(|reference| !reference.is_empty()) {
						None => Ok(self.inner.clone()),
						Some(reference) => Err(MoqError::UnresolvableBroadcast {
							reason: reference.as_str().to_string(),
						}),
					};
				};

				// Even an empty reference goes through the source, which checks the epoch against the
				// catalog's own broadcast.
				let source = moq_mux::Source::new(origin, self.inner.clone());
				Ok(source.resolve(reference.as_ref(), epoch.as_ref()).await?)
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
	/// else names a sibling relative to it (e.g. `./source`). Its `epoch` is the publisher
	/// instance the reference names: a broadcast proven to serve another one fails with `Mux`
	/// instead of resolving, while a route without an epoch cannot prove one, so there it is best
	/// effort. Call it on a rendition that carries a reference before [`Self::subscribe_track`] or
	/// [`Self::fetch_group`], which take the rendition's `track` (or its ID when unset) rather
	/// than a rendition. The rendition-based consumers (`MoqMediaTrackConsumer::subscribe`,
	/// `MoqMediaGroupConsumer::fetch`, `MoqVideoConsumer::subscribe`,
	/// `MoqAudioConsumer::subscribe`) resolve it themselves.
	///
	/// Errors if this broadcast came from a local producer rather than an origin, since a
	/// standalone broadcast has no sibling to name. A sibling nothing announces yet is waited for,
	/// so a late one resolves once it is announced; cancel the call to give up on it.
	pub async fn resolve(&self, rendition: MoqRendition) -> Result<Arc<MoqBroadcastConsumer>, MoqError> {
		let (reference, epoch, _, _) = rendition.into_parts();
		let broadcast = self.resolve_inner(reference.as_deref(), epoch.as_deref()).await?;
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

	/// Fetch one complete group by track name and group sequence.
	///
	/// This does not create a live subscription. A retained group resolves immediately;
	/// otherwise the request waits for a dynamic producer to serve it. The returned
	/// group may still be in progress, so read frames until `read_frame()` returns `None`.
	pub async fn fetch_group(
		&self,
		name: String,
		sequence: u64,
		options: Option<MoqFetchGroupOptions>,
	) -> Result<Arc<MoqGroupConsumer>, MoqError> {
		self.driver
			.watch(async {
				let options = options.map(moq_net::group::Fetch::from);
				let track = self.inner.track(&name).map_err(map_fetch_error)?;
				let group = track.fetch_group(sequence, options).await.map_err(map_fetch_error)?;
				Ok(Arc::new(MoqGroupConsumer::new(group, self.driver.clone())))
			})
			.await
	}
}

pub(crate) fn map_fetch_error(err: moq_net::Error) -> MoqError {
	match err {
		moq_net::Error::NotFound | moq_net::Error::NotFetchable => MoqError::NotFound,
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
/// Datagrams are a separate cursor on either handle, so reading them never commits.
enum Cursor {
	/// No group read has happened yet; the track can still commit either way.
	Uncommitted(moq_net::track::Subscriber),
	/// Committed to arrival order by the first `recv_group`.
	Arrival(moq_net::track::Subscriber),
	/// Committed to sequence order by the first `next_group` or `read_frame`.
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
	/// once it committed to sequence order.
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

	/// The sequence cursor, committing this track to it, or [`MoqError::AlreadyCommitted`]
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
				ordered.track.poll_next_group(waiter).map(|r| r.map_err(Into::into))
			}
			Err(e) => Poll::Ready(Err(e)),
		}
	}

	/// The first frame of the next group in sequence order, skipping completed empty groups.
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
				// An empty group, or one that expired here or at a hop that reset it.
				Ok(None) | Err(moq_net::Error::Old | moq_net::Error::Stream(moq_net::StreamError::Old)) => continue,
				Err(err) => return Poll::Ready(Err(err.into())),
			}
		}
	}

	fn poll_recv_datagram(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<MoqDatagram>, MoqError>> {
		// Datagrams are unordered on either handle; reading them never commits the
		// group cursor.
		let poll = match &mut self.track {
			Cursor::Uncommitted(track) | Cursor::Arrival(track) => track.poll_recv_datagram(waiter),
			Cursor::Ordered(ordered) => ordered.track.poll_recv_datagram(waiter),
			// Poisoned mid-conversion; see `arrival`. Datagrams never commit a cursor,
			// so this is the only way they can fail on a live handle.
			Cursor::Converting => return Poll::Ready(Err(MoqError::Closed)),
		};
		let datagram = match ready!(poll) {
			Ok(Some(datagram)) => datagram,
			Ok(None) => return Poll::Ready(Ok(None)),
			Err(err) => return Poll::Ready(Err(err.into())),
		};
		Poll::Ready(Ok(Some(MoqDatagram {
			sequence: datagram.sequence,
			timestamp_us: datagram.timestamp.map(timestamp_us).transpose()?,
			payload: datagram.payload.to_vec(),
		})))
	}
}

/// A subscribed track whose groups, frames, or datagrams can be read.
#[derive(uniffi::Object)]
pub struct MoqTrackConsumer {
	/// Every read polls this; none holds it across a wait, so a datagram read progresses while
	/// a group read is parked, and vice versa.
	reads: Polled<TrackInner>,
	control: moq_net::track::Control,
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
	/// would miss whatever that read consumed. A datagram read still parked resolves `Cancelled`.
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
	/// Groups are returned as they arrive on the wire, which may be out of sequence
	/// order (e.g. if a later group lands before an earlier one on a separate stream).
	/// One that drifts further behind the live edge than the subscription's `max_delay`
	/// is skipped, or ends its pending read if already returned; raise it to keep stalled groups.
	///
	/// The first call commits this track to arrival order: sequence-order reads
	/// ([`Self::next_group`], [`Self::read_frame`]) fail with
	/// [`MoqError::AlreadyCommitted`] afterwards.
	pub async fn recv_group(&self) -> Result<Option<Arc<MoqGroupConsumer>>, MoqError> {
		let group = self.reads.wait(|inner, waiter| inner.poll_recv_group(waiter)).await?;
		Ok(self.group(group))
	}

	/// Return the next group with a higher sequence number than any previously
	/// returned, skipping late arrivals. Returns `None` when the track ends.
	///
	/// Shares the sequence cursor with [`Self::read_frame`]: a group one method
	/// has already taken is not returned by the other. A `read_frame` cancelled
	/// after acquiring a group leaves that group here.
	///
	/// The first call commits this track to sequence order: arrival-order reads
	/// ([`Self::recv_group`]) fail with [`MoqError::AlreadyCommitted`] afterwards.
	pub async fn next_group(&self) -> Result<Option<Arc<MoqGroupConsumer>>, MoqError> {
		let group = self.reads.wait(|inner, waiter| inner.poll_next_group(waiter)).await?;
		Ok(self.group(group))
	}

	/// Read the first frame of the next group, including its timestamp.
	///
	/// Convenience for tracks using one-frame-per-group (like moq-boy's
	/// status/command tracks). Completed empty groups are skipped. Returns `None`
	/// only when the track ends. Cancelling one call keeps the current group so a
	/// later `read_frame` or [`Self::next_group`] still sees it.
	///
	/// Shares the sequence cursor with [`Self::next_group`], committing the track
	/// the same way.
	pub async fn read_frame(&self) -> Result<Option<MoqFrame>, MoqError> {
		let frame = self.reads.wait(|inner, waiter| inner.poll_read_frame(waiter)).await?;
		frame.map(raw_frame).transpose()
	}

	/// Receive the next best-effort datagram in arrival order.
	///
	/// Returns `None` when the track ends. Datagram delivery is unavailable over
	/// pre-lite-05 moq-lite and stream-only transports.
	/// Datagrams are a separate cursor from groups, so this works alongside either
	/// group order, never commits the track to one, and progresses while a group
	/// read is pending.
	pub async fn recv_datagram(&self) -> Result<Option<MoqDatagram>, MoqError> {
		self.reads.wait(|inner, waiter| inner.poll_recv_datagram(waiter)).await
	}

	/// Return the publisher-side track properties learned during subscription.
	pub fn info(&self) -> MoqTrackInfo {
		MoqTrackInfo::from(&self.info)
	}

	/// Change this subscriber's delivery preferences.
	///
	/// Silently ignored if the track already ended; the update is meaningless at
	/// that point. Errors on a time anchor the track can't serve, leaving the
	/// preferences as they were.
	pub fn update(&self, subscription: MoqSubscription) -> Result<(), MoqError> {
		match self.control.update(subscription.try_into()?) {
			Ok(()) | Err(moq_net::Error::Closed) => Ok(()),
			Err(err) => Err(err.into()),
		}
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
	sequence: u64,
	reads: Polled<moq_net::group::Consumer>,
	driver: Background,
}

impl MoqGroupConsumer {
	pub(crate) fn new(group: moq_net::group::Consumer, driver: Background) -> Self {
		Self {
			sequence: group.sequence,
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
	/// The sequence number of this group within the track.
	pub fn sequence(&self) -> u64 {
		self.sequence
	}

	/// Read the next frame in this group, including its timestamp.
	///
	/// Returns `None` when the group ends.
	pub async fn read_frame(&self) -> Result<Option<MoqFrame>, MoqError> {
		let frame = self
			.reads
			.wait(|group, waiter| Poll::Ready(Ok(ready!(group.poll_read_frame(waiter))?)))
			.await?;
		frame.map(raw_frame).transpose()
	}

	/// Cancel all current and future `read_frame()` calls.
	///
	/// Terminal: the group and whatever it still buffers are released here, not when the handle is.
	pub fn cancel(&self) {
		self.reads.cancel();
	}
}
