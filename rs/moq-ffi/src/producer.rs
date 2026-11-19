//! Publish broadcasts, tracks, and groups.

use std::sync::Arc;
use std::task::{Poll, ready};

use moq_mux::catalog::hang::Extra;

use crate::consumer::{MoqBroadcastConsumer, MoqGroupConsumer, MoqOrder, MoqSubscription, MoqTrackConsumer};
use crate::demand::MoqTrackDemand;
use crate::error::MoqError;
use crate::ffi::{Background, OneShot, Polled};
use crate::media::{MoqFrame, MoqTimestamp};

/// A location within a track, mirroring [`moq_net::track::Position`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct MoqPosition {
	/// The group start.
	pub group: u64,
	/// The frame index within the group, numbered from 0 in write order.
	pub frame: u64,
}

impl From<moq_net::track::Position> for MoqPosition {
	fn from(position: moq_net::track::Position) -> Self {
		Self {
			group: position.group,
			frame: position.frame,
		}
	}
}

/// Publisher-side track properties, mirroring [`moq_net::track::Info`].
///
/// Construct with the fields you care about; the rest use raw-track defaults
/// (priority 0, untimed, uncompressed). A null record uses moq-net's zero `max_age` too.
/// Uniffi can't default a duration, so Kotlin and Swift name `max_age`, the Python, Go, and
/// Dart wrappers default an omitted one to zero, and a bare C++ record leaves it unset, as
/// [`MoqDelay`](crate::consumer::MoqDelay) does.
#[derive(Clone, uniffi::Record)]
pub struct MoqTrackInfo {
	/// Priority, used only to break ties between subscriptions of equal subscriber priority.
	/// Higher is more urgent; the default 0 is the midpoint.
	#[uniffi(default = 0)]
	pub priority: i8,
	/// How long a superseded group stays fresh, or null for no publisher limit, leaving only
	/// the cache to bound how long one is served. Zero serves only the live edge. The
	/// publisher-side half of [`MoqDelay::max`](crate::consumer::MoqDelay::max), which reads
	/// null the same way.
	pub max_age: Option<std::time::Duration>,
	/// When a subscriber asks for a new group, which the publisher hears through
	/// [`MoqTrackDemand::refresh`](crate::demand::MoqTrackDemand::refresh). The default never
	/// asks.
	#[uniffi(default)]
	pub refresh: MoqTrackRefresh,
	/// Per-frame timescale in ticks per second, or null for an untimed track.
	#[uniffi(default = None)]
	pub timescale: Option<u64>,
	/// How each group is compressed on the wire to peers that support it, or null to send
	/// frames as written. Readers always see the original frames.
	#[uniffi(default = None)]
	pub compression: Option<Arc<MoqCompression>>,
}

/// When a subscriber asks a track's publisher for a new group, mirroring
/// [`moq_net::track::Refresh`]. The default asks for nothing.
///
/// A track opting in starts each group with an independent frame (a keyframe or a snapshot), so
/// a new group is something the publisher can make on request. A live join asks once the newest
/// group outweighs `ratio`, or on any join without one. Either way the request waits out
/// `interval`, which caps how often the publisher makes groups however many subscribers ask.
#[derive(Clone, Debug, Default, PartialEq, Eq, uniffi::Record)]
pub struct MoqTrackRefresh {
	/// How many times its first frame a group's later bytes may grow before a joining subscriber
	/// asks for a new group. Zero asks on any join once a frame follows the first; null leaves
	/// joins to `interval` alone.
	#[uniffi(default = None)]
	pub ratio: Option<u32>,
	/// How long after the newest group arrived a request may ask for another, or null for no
	/// wait. A request made sooner takes effect then. Sent as milliseconds.
	#[uniffi(default = None)]
	pub interval: Option<std::time::Duration>,
}

impl From<MoqTrackRefresh> for moq_net::track::Refresh {
	fn from(refresh: MoqTrackRefresh) -> Self {
		moq_net::track::Refresh::default()
			.with_ratio(refresh.ratio)
			.with_interval(refresh.interval)
	}
}

impl From<moq_net::track::Refresh> for MoqTrackRefresh {
	fn from(refresh: moq_net::track::Refresh) -> Self {
		Self {
			ratio: refresh.ratio,
			interval: refresh.interval.map(crate::ffi::duration),
		}
	}
}

/// How a track's frames are compressed, mirroring [`moq_net::track::Compression`]; an
/// uncompressed track has none.
///
/// An immutable object so adding a compression later does not break callers switching over a
/// closed enum. Compare a received one by [`name`](Self::name).
#[derive(Debug, PartialEq, Eq, Hash, uniffi::Object)]
#[uniffi::export(Debug, Eq, Hash)]
pub struct MoqCompression(moq_net::track::Compression);

#[uniffi::export]
impl MoqCompression {
	/// Each group is one DEFLATE stream, which suits small, repetitive frames like JSON.
	#[uniffi::constructor]
	pub fn deflate() -> Arc<Self> {
		Arc::new(Self(moq_net::track::Compression::Deflate))
	}

	/// The compression's lowercase name, such as `deflate`.
	pub fn name(&self) -> String {
		match self.0 {
			moq_net::track::Compression::Deflate => "deflate".into(),
			// A compression added to moq-net before this binding names it.
			other => format!("{other:?}").to_lowercase(),
		}
	}
}

impl TryFrom<MoqTrackInfo> for moq_net::track::Info {
	type Error = MoqError;

	fn try_from(info: MoqTrackInfo) -> Result<Self, MoqError> {
		let mut out = moq_net::track::Info::default()
			.with_priority(info.priority)
			.with_refresh(info.refresh.into())
			.with_max_age(info.max_age);
		if let Some(ticks) = info.timescale {
			let scale = moq_net::Timescale::new(ticks).map_err(|_| MoqError::Codec {
				reason: format!("invalid timescale: {ticks}"),
			})?;
			out = out.with_timescale(scale);
		}
		let compression = info
			.compression
			.map_or(moq_net::track::Compression::None, |compression| compression.0);
		Ok(out.with_compression(compression))
	}
}

fn raw_track_info(info: Option<MoqTrackInfo>) -> Result<moq_net::track::Info, MoqError> {
	info.map(moq_net::track::Info::try_from)
		.transpose()
		.map(|info| info.unwrap_or_default())
}

impl From<&moq_net::track::Info> for MoqTrackInfo {
	fn from(info: &moq_net::track::Info) -> Self {
		Self {
			priority: info.priority,
			max_age: info.max_age.map(crate::ffi::duration),
			refresh: info.refresh.into(),
			timescale: info.timescale.map(|timescale| timescale.as_u64()),
			compression: (info.compression != moq_net::track::Compression::None)
				.then(|| Arc::new(MoqCompression(info.compression))),
		}
	}
}

// ---- UniFFI Objects ----

pub(crate) struct BroadcastProducer {
	pub(crate) broadcast: moq_net::broadcast::Producer,
	// Carries the untyped `Extra` extension so callers can attach application catalog
	// sections by name (the only extension shape that crosses the FFI boundary).
	pub(crate) catalog: moq_mux::catalog::Producer<Extra>,
	// Core timebases own catalog outputs, so the broadcast holds them and binding handles stay weak.
	pub(crate) timebases: std::collections::HashMap<usize, moq_mux::catalog::Timebase<Extra>>,
	pub(crate) driver: Background,
	pub(crate) children: Children,
	/// Set by [`MoqMediaCatalogProducer::finish`](crate::media::MoqMediaCatalogProducer::finish): the
	/// catalog is final, so a close leaves it alone.
	pub(crate) ended: bool,
}

impl Drop for BroadcastProducer {
	fn drop(&mut self) {
		self.children.release();
	}
}

/// A handle publishing tracks on a broadcast, which its broadcast ends on close.
///
/// `Send + Sync`, since any thread may close a broadcast.
pub(crate) trait Child: Send + Sync {
	/// End whatever this handle still publishes with `err`, so later calls fail with `Closed`.
	///
	/// A track that already finished ends cleanly instead.
	fn abort(&self, err: moq_net::Error);
}

/// What a broadcast holds of its children.
enum Family {
	Open {
		/// Handles that may still publish, ended on close.
		open: Vec<std::sync::Weak<dyn Child>>,
		/// Finished producers, kept so a late subscriber still finds their tracks.
		kept: Vec<Box<dyn Send>>,
		/// What [`Children::broadcast`] reaches while the broadcast handle is open.
		_broadcast: Arc<moq_net::broadcast::Producer>,
	},
	/// The broadcast handle was dropped without a close: each child ends on its own.
	Released,
	/// The broadcast closed: its children ended and new ones are refused.
	Closed,
}

/// The track-publishing handles of one broadcast.
///
/// A child that drops without finishing aborts itself, so a garbage-collected binding never
/// leaves a track to drop without a cause, and closing the broadcast aborts every child still
/// open. A finished track stops being served once its last producer drops, and an importer lets
/// go of its producer when it finishes, so the broadcast keeps finished producers here instead:
/// a subscriber arriving after the end still finds the track until the broadcast closes.
#[derive(Clone)]
pub(crate) struct Children {
	family: Arc<std::sync::Mutex<Family>>,
	/// Reachable while a handle keeping the broadcast open holds it: the broadcast handle, or a
	/// request handler. Never kept alive from here, so a child never holds its broadcast open.
	broadcast: std::sync::Weak<moq_net::broadcast::Producer>,
}

impl Children {
	fn new(broadcast: moq_net::broadcast::Producer) -> Self {
		let broadcast = Arc::new(broadcast);
		Self {
			broadcast: Arc::downgrade(&broadcast),
			family: Arc::new(std::sync::Mutex::new(Family::Open {
				open: Vec::new(),
				kept: Vec::new(),
				_broadcast: broadcast,
			})),
		}
	}

	/// The broadcast, while a handle keeping it open still holds it.
	fn broadcast(&self) -> Option<Arc<moq_net::broadcast::Producer>> {
		self.broadcast.upgrade()
	}

	/// Register a new child so a close ends it. Once the broadcast closed, the child is aborted
	/// and refused with `Closed`.
	pub(crate) fn adopt<T: Child + 'static>(&self, child: Arc<T>) -> Result<Arc<T>, MoqError> {
		let mut family = self.family.lock().unwrap();
		match &mut *family {
			Family::Open { open, .. } => {
				open.retain(|child| child.strong_count() > 0);
				let weak: std::sync::Weak<dyn Child> = Arc::downgrade(&child) as _;
				open.push(weak);
				Ok(child)
			}
			Family::Released => Ok(child),
			Family::Closed => {
				drop(family);
				child.abort(moq_net::Error::Unroutable);
				Err(MoqError::Closed)
			}
		}
	}

	/// Whether the broadcast closed, ending its children.
	fn is_closed(&self) -> bool {
		matches!(*self.family.lock().unwrap(), Family::Closed)
	}

	/// Keep a finished producer until the broadcast ends, or drop it now if it already has.
	pub(crate) fn keep(&self, producer: impl Send + 'static) {
		let producer: Box<dyn Send> = Box::new(producer);
		if let Family::Open { kept, .. } = &mut *self.family.lock().unwrap() {
			kept.push(producer);
		}
	}

	/// End every open child with `Unroutable`, then let go of the finished ones.
	fn close(&self) {
		let family = std::mem::replace(&mut *self.family.lock().unwrap(), Family::Closed);
		if let Family::Open { open, kept, .. } = family {
			// Outside the lock: a child's own finish keeps into it while holding the child's lock.
			for child in open.iter().filter_map(std::sync::Weak::upgrade) {
				child.abort(moq_net::Error::Unroutable);
			}
			drop(kept);
		}
	}

	/// Let go of the finished producers, leaving open children to end on their own.
	fn release(&self) {
		let mut family = self.family.lock().unwrap();
		if matches!(*family, Family::Open { .. }) {
			let released = std::mem::replace(&mut *family, Family::Released);
			drop(family);
			drop(released);
		}
	}
}

/// A broadcast in which tracks can be published.
#[derive(uniffi::Object)]
pub struct MoqBroadcastProducer {
	pub(crate) state: std::sync::Mutex<Option<BroadcastProducer>>,
}

/// A handler for requests for tracks in a broadcast.
#[derive(uniffi::Object)]
pub struct MoqBroadcastRequests {
	reads: Polled<RequestsInner>,
	children: Children,
	driver: Background,
}

struct RequestsInner {
	requests: moq_net::broadcast::Requests,
	/// Holds the broadcast for [`Children::broadcast`] as long as `requests` keeps it open, so a
	/// request can still be accepted after the broadcast handle dropped.
	_broadcast: Arc<moq_net::broadcast::Producer>,
}

/// Serves on-demand fetches of uncached groups for one track.
#[derive(uniffi::Object)]
pub struct MoqTrackRequests {
	reads: Polled<moq_net::track::Requests>,
}

impl MoqTrackRequests {
	fn new(inner: moq_net::track::Requests) -> Self {
		Self {
			reads: Polled::new(inner),
		}
	}
}

impl MoqBroadcastProducer {
	/// Wrap a `moq_net::broadcast::Producer` (standalone or origin-created), attaching
	/// the catalog track every FFI broadcast carries.
	pub(crate) fn from_inner(
		mut broadcast: moq_net::broadcast::Producer,
		driver: Background,
	) -> Result<Self, MoqError> {
		let config =
			moq_mux::catalog::Config::default().with_catalog(moq_mux::catalog::hang::Catalog::<Extra>::default());
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, config)?;
		Ok(Self {
			state: std::sync::Mutex::new(Some(BroadcastProducer {
				children: Children::new(broadcast.clone()),
				broadcast,
				catalog,
				timebases: Default::default(),
				driver,
				ended: false,
			})),
		})
	}

	#[cfg(test)]
	pub(crate) fn consume_inner(&self) -> Result<moq_net::broadcast::Consumer, MoqError> {
		self.with_state(|state| Ok(state.broadcast.consume()))
	}

	/// Run `f` against the open broadcast and catalog. Errors with
	/// [`MoqError::Closed`] if `close()` has already run. Used by
	/// sibling modules (e.g. `audio`) that need joint access.
	pub(crate) fn with_state<R>(
		&self,
		f: impl FnOnce(&mut BroadcastProducer) -> Result<R, MoqError>,
	) -> Result<R, MoqError> {
		let mut guard = self.state.lock().unwrap();
		let state = guard.as_mut().ok_or(MoqError::Closed)?;
		state.driver.check()?;
		let result = f(state);
		state.driver.check()?;
		result
	}
}

#[uniffi::export]
impl MoqBroadcastProducer {
	/// Create a consumer that reads from this broadcast's tracks.
	pub fn consume(&self) -> Result<Arc<MoqBroadcastConsumer>, MoqError> {
		let _guard = crate::ffi::enter();
		self.with_state(|state| {
			Ok(Arc::new(MoqBroadcastConsumer {
				inner: state.broadcast.consume(),
				origin: None,
				driver: state.driver.clone(),
			}))
		})
	}

	/// Create a dynamic producer that yields tracks requested by subscribers.
	///
	/// Hold the returned object for as long as missing track requests should be
	/// accepted. Dropping it makes future subscriptions to unknown tracks fail.
	pub fn requests(&self) -> Result<Arc<MoqBroadcastRequests>, MoqError> {
		let _guard = crate::ffi::enter();
		self.with_state(|state| {
			let inner = RequestsInner {
				requests: state.broadcast.requests(),
				_broadcast: state.children.broadcast().ok_or(MoqError::Closed)?,
			};
			Ok(Arc::new(MoqBroadcastRequests {
				reads: Polled::new(inner).supervised(state.driver.clone()),
				driver: state.driver.clone(),
				children: state.children.clone(),
			}))
		})
	}

	/// Create a standalone broadcast, not attached to any origin.
	///
	/// Use it for local pub/sub via [`consume`](Self::consume). To publish at a path, use
	/// [`MoqOriginProducer::route`](crate::origin::MoqOriginProducer::route) instead.
	///
	/// No origin collects its cache, so it drives its own until it closes: that is what fires a
	/// refresh deferred by its track's interval.
	#[uniffi::constructor]
	pub fn new() -> Result<Arc<Self>, MoqError> {
		let _guard = crate::ffi::enter();
		let broadcast = moq_net::broadcast::Info::new().produce();
		let driver = Background::driver(PoolDriver(broadcast.info().pool.downgrade()));
		Ok(Arc::new(Self::from_inner(broadcast, driver)?))
	}

	/// Create a track for arbitrary byte payloads, no codec or container.
	///
	/// Same pattern as moq-boy's `status` and `command` tracks: raw UTF-8/JSON
	/// bytes written directly to moq-lite groups with no media framing. `info` sets
	/// track properties (priority, max age, timescale); omit for defaults.
	pub fn publish_track(&self, name: String, info: Option<MoqTrackInfo>) -> Result<Arc<MoqTrackProducer>, MoqError> {
		let _guard = crate::ffi::enter();
		self.with_state(|state| {
			let info = raw_track_info(info)?;
			let track = state
				.broadcast
				.create_track(name, Some(info))
				.ok_or(moq_net::Error::Duplicate)?;
			MoqTrackProducer::child(track, &state.children)
		})
	}

	/// End the broadcast for good: retract it, serve no new tracks, and finalize the catalog.
	///
	/// Every track handle still open on it ends with `Unroutable` (`Internal` to a moq-transport
	/// reader), and its later calls fail with `Closed`. Finish a track first to end it
	/// cleanly. Every later call on this producer fails with `Closed`; closing again is a no-op.
	pub fn close(&self) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		// Hold the lock through shutdown so a concurrent close() returns only once it is done.
		let mut guard = self.state.lock().unwrap();
		let Some(mut state) = guard.take() else {
			return Ok(());
		};
		// The children first, so none outlives the broadcast it publishes on, and the broadcast
		// before the catalog so it ends even if finalizing the catalog fails.
		state.children.close();
		state.broadcast.close();
		let result = match state.ended {
			true => Ok(()),
			false => state.catalog.finish().map_err(Into::into),
		};
		state.driver.check()?;
		result
	}
}

/// Collects a standalone broadcast's cache pool, which no origin drives, so its wall-clock
/// deadlines fire.
///
/// Runs while anything still caches into the pool: the broadcast, a track it released when its
/// handle dropped without a close, or a reader. Holding it weakly, it ends at the first poll
/// after the last of them drops, which the pool's periodic sweep bounds.
struct PoolDriver(moq_net::cache::PoolWeak);

impl moq_net::time::Driver for PoolDriver {
	fn poll(
		&mut self,
		now: moq_net::time::Instant,
		waiter: &kio::Waiter,
	) -> Result<Option<moq_net::time::Instant>, moq_net::Error> {
		let pool = self.0.upgrade().ok_or(moq_net::Error::Closed)?;
		Ok(pool.poll(now, waiter))
	}
}

// ---- Broadcast Requests ----

#[uniffi::export]
impl MoqBroadcastRequests {
	/// Wait for the next subscriber-requested track.
	///
	/// Returns a [`MoqTrackRequest`]: accept it for raw writes with
	/// [`MoqTrackRequest::accept`], publish media onto it with
	/// [`crate::media::MoqMediaTrackProducer::audio`], or reject it with
	/// [`MoqTrackRequest::reject`]. The requesting subscriber stays pending until then.
	///
	/// Returns an error once the broadcast is closed or aborted.
	pub async fn requested_track(&self) -> Result<Arc<MoqTrackRequest>, MoqError> {
		// Hand back the un-accepted request, mirroring `moq_net::broadcast::Requests`: the caller
		// accepts it (raw, at a chosen timescale) or publishes media onto it (importer accepts).
		// The subscriber's subscribe stays pending until then.
		let request = self
			.reads
			.wait(|inner, waiter| Poll::Ready(Ok(ready!(inner.requests.poll_requested_track(waiter))?)))
			.await?;
		Ok(Arc::new(MoqTrackRequest::new(request, self.children.clone())))
	}

	/// Create an on-demand handler for datagram tracks.
	pub fn datagrams(&self) -> Result<Arc<crate::datagram::MoqDatagramRequests>, MoqError> {
		let _guard = crate::ffi::enter();
		let requests = self
			.reads
			.peek(|inner| inner.requests.datagrams())
			.ok_or(MoqError::Closed)?;
		Ok(Arc::new(crate::datagram::MoqDatagramRequests::new(
			requests,
			self.children.clone(),
			self.driver.clone(),
		)))
	}

	/// Cancel all current and future `requested_track()` calls.
	///
	/// Terminal: the dynamic broadcast is released here, not when the handle is, so any pending
	/// request is rejected.
	pub fn cancel(&self) {
		self.reads.cancel();
	}
}

// ---- Track Requests ----

#[uniffi::export]
impl MoqTrackRequests {
	/// Wait for the next run of groups readers miss: a fetch of an uncached group, or
	/// history a subscription reads that the cache no longer holds.
	///
	/// Cached groups are served without reaching this method.
	///
	/// A reader's time start that no cached group places is looked up rather than filled; a
	/// fill names groups, so the lookup is dropped unanswered here, and the reader falls back
	/// to the groups held.
	pub async fn requested_fill(&self) -> Result<Arc<MoqFill>, MoqError> {
		let fill = self
			.reads
			.wait(|requests, waiter| {
				loop {
					if let moq_net::track::Fill::Groups(fill) = ready!(requests.poll_requested_fill(waiter))? {
						return Poll::Ready(Ok(fill));
					}
				}
			})
			.await?;
		Ok(Arc::new(MoqFill::new(fill)))
	}

	/// Cancel all current and future `requested_fill()` calls.
	///
	/// Terminal: the dynamic track is released here, not when the handle is, so any pending
	/// fetch is rejected.
	pub fn cancel(&self) {
		self.reads.cancel();
	}
}

/// A run of groups readers miss, mirroring [`moq_net::track::FillGroups`].
///
/// Call [`accept`](Self::accept) for each group you have and [`drop_groups`](Self::drop_groups)
/// for the rest, one group at a time as [`wanted_changed`](Self::wanted_changed) yields them;
/// [`wanted`](Self::wanted) names what is still owed, in the fill's order. Releasing
/// it names whatever is left as dropped while a reader still wants it; once nobody does, it is
/// withdrawn, so stop serving it once [`wanted`](Self::wanted) is null. Stop serving a single
/// group when [`unwanted`](Self::unwanted) resolves for it, and move on to the next. Serve the
/// fills you take concurrently: each waits on its own readers.
#[derive(uniffi::Object)]
pub struct MoqFill {
	inner: moq_net::track::FillGroups,
	/// The watcher `wanted_changed` advances, kept apart from `inner` so its cursor is its own.
	changes: Polled<moq_net::track::FillGroups>,
}

impl MoqFill {
	fn new(inner: moq_net::track::FillGroups) -> Self {
		let changes = Polled::new(inner.clone());
		Self { inner, changes }
	}
}

#[uniffi::export]
impl MoqFill {
	/// The first group to fill, and the frame within it to start from.
	pub fn start(&self) -> MoqPosition {
		self.inner.start().into()
	}

	/// The exclusive end group.
	pub fn end(&self) -> u64 {
		self.inner.end()
	}

	/// The most urgent delivery priority among the readers waiting on it.
	pub fn priority(&self) -> i8 {
		self.inner.priority()
	}

	/// The most tolerant age budget among the readers waiting on it, or null for the
	/// publisher's max age alone.
	pub fn max_delay(&self) -> Option<std::time::Duration> {
		self.inner.max_delay().map(crate::ffi::duration)
	}

	/// Which end to fill first.
	pub fn order(&self) -> MoqOrder {
		self.inner.order().into()
	}

	/// The next group still owed in the fill's order, or null once every group is answered or
	/// no reader waits on the fill. It does not wait for the readers; `wanted_changed` does.
	pub fn wanted(&self) -> Option<u64> {
		self.inner.wanted()
	}

	/// The next group to serve: `wanted` once it changed and every group this fill accepted
	/// reached a reader, which picks it up when it next reads. Null, once the fill is done,
	/// never waits; the first call returns at once unless an accepted group has yet to reach a
	/// reader. Serve one group per call, so the fill writes no faster than its readers read.
	pub async fn wanted_changed(&self) -> Result<Option<u64>, MoqError> {
		// Polled in place: a cancelled call left unfreed must not advance the
		// watcher and swallow the change the next call waits for.
		self.changes
			.wait(|fill, waiter| Poll::Ready(Ok(ready!(fill.poll_wanted_changed(waiter)))))
			.await
	}

	/// Insert group `start` and return a producer for filling it. The first group starts
	/// at [`start`](Self::start)'s frame, so its frames keep their indices.
	pub fn accept(&self, start: u64) -> Result<Arc<MoqGroupProducer>, MoqError> {
		let _guard = crate::ffi::enter();
		let group = self.inner.accept(start, None)?.ok_or(moq_net::Error::Duplicate)?;
		Ok(Arc::new(MoqGroupProducer::from_inner(group)))
	}

	/// Name groups `start` up to the exclusive `end` as never to be delivered.
	pub fn drop_groups(&self, start: u64, end: u64) {
		let _guard = crate::ffi::enter();
		self.inner.drop_groups(start..end);
	}

	/// Whether a reader still waits for group `start` from this fill: false once every
	/// reader took it or narrowed past it, or nobody waits on the fill.
	pub fn is_wanted(&self, start: u64) -> bool {
		self.inner.is_wanted(start)
	}

	/// Wait until no reader waits for group `start` from this fill, so stop serving it.
	pub async fn unwanted(&self, start: u64) -> Result<(), MoqError> {
		crate::ffi::run(async {
			self.inner.unwanted(start).await;
			Ok::<_, MoqError>(())
		})
		.await
	}
}

// ---- Track Request ----

/// A track requested by a subscriber that hasn't been accepted yet.
///
/// Mirrors [`moq_net::track::Request`]: [`accept`](Self::accept) it to start producing raw
/// frames, hand it to [`crate::media::MoqMediaTrackProducer::audio`] to publish media,
/// or [`reject`](Self::reject) it to fail the waiting subscriber.
#[derive(uniffi::Object)]
pub struct MoqTrackRequest {
	inner: OneShot<moq_net::track::Request>,
	children: Children,
}

impl MoqTrackRequest {
	pub(crate) fn new(request: moq_net::track::Request, children: Children) -> Self {
		Self {
			inner: OneShot::new(request),
			children,
		}
	}

	/// Answer with an importer that accepts the request itself (setting the timescale), as
	/// [`crate::media::MoqMediaTrackProducer::audio`] does.
	pub(crate) fn answer<R>(
		&self,
		f: impl FnOnce(moq_net::track::Request) -> Result<R, MoqError>,
	) -> Result<R, MoqError> {
		self.inner.answer(f)
	}
}

#[uniffi::export]
impl MoqTrackRequest {
	/// The requested track name.
	pub fn name(&self) -> Result<String, MoqError> {
		self.inner.peek(|request| request.name().to_string())
	}

	/// Watch this track's subscribers, including after the request is accepted.
	pub fn demand(&self) -> Result<Arc<MoqTrackDemand>, MoqError> {
		self.inner.peek(|request| MoqTrackDemand::new(request.demand()))
	}

	/// Create a handler for uncached group fetches before accepting this track.
	///
	/// Obtain and retain this handle before `accept()` when the track itself was
	/// requested by a fetch. This keeps the pending group request serviceable across
	/// the transition from request to producer.
	pub fn requests(&self) -> Result<Arc<MoqTrackRequests>, MoqError> {
		let _guard = crate::ffi::enter();
		self.inner
			.peek(|request| Arc::new(MoqTrackRequests::new(request.requests())))
	}

	/// Accept the request as a raw track, fixing its [`MoqTrackInfo`] (timescale, etc.).
	///
	/// Once the broadcast closed, the track ends with `Unroutable` and this fails with `Closed`.
	///
	/// For media use [`crate::media::MoqMediaTrackProducer::audio`] instead, which lets
	/// the importer pick the timescale.
	pub fn accept(&self, info: Option<MoqTrackInfo>) -> Result<Arc<MoqTrackProducer>, MoqError> {
		let _guard = crate::ffi::enter();
		let info = raw_track_info(info)?;
		self.inner.answer(|request| {
			// Without a handle keeping it open, the broadcast has ended.
			let Some(_broadcast) = self.children.broadcast() else {
				request.reject(moq_net::Error::Unroutable);
				return Err(MoqError::Closed);
			};
			let track = request.accept(Some(info));
			MoqTrackProducer::child(track, &self.children)
		})
	}

	/// Reject the request with an application error code, failing the waiting subscriber.
	pub fn reject(&self, error_code: u16) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		self.inner.answer(|request| {
			request.reject(moq_net::Error::App(error_code));
			Ok(())
		})
	}
}

// ---- Track Producer ----

/// A stream track to which groups and frames can be published.
///
/// Released before its finish, it ends its track with `Dropped`, which readers elsewhere see as
/// `Internal`. Still open when its broadcast closes, it ends it with `Unroutable`, which a
/// moq-transport reader sees as `Internal`: its wire has no such code.
#[derive(uniffi::Object)]
pub struct MoqTrackProducer {
	phase: std::sync::Mutex<TrackPhase>,
}

/// How far a track binding got.
#[derive(Default)]
enum TrackPhase {
	/// Publishing, as a child of `family`.
	Open {
		raw: moq_net::track::Producer,
		family: Children,
	},
	/// Finished, and kept by its family until the broadcast closes. It may still take groups
	/// below its end, and abort.
	Finished(moq_net::track::Producer),
	/// Aborted, handed to a typed writer, or ended with its broadcast.
	#[default]
	Closed,
}

impl TrackPhase {
	fn raw(&mut self) -> Result<&mut moq_net::track::Producer, MoqError> {
		match self {
			Self::Open { raw, .. } | Self::Finished(raw) => Ok(raw),
			Self::Closed => Err(MoqError::Closed),
		}
	}

	/// End the track with `err`, unless it already finished.
	fn abort(self, err: moq_net::Error) {
		if let Self::Open { raw, .. } | Self::Finished(raw) = self
			&& raw.finished().is_none()
		{
			let _ = raw.abort(err);
		}
	}
}

impl Child for MoqTrackProducer {
	fn abort(&self, err: moq_net::Error) {
		let _guard = crate::ffi::enter();
		let phase = std::mem::take(&mut *self.phase.lock().unwrap());
		phase.abort(err);
	}
}

impl Drop for MoqTrackProducer {
	fn drop(&mut self) {
		let _guard = crate::ffi::enter();
		// A finished track lives on in its broadcast, and may still take groups below its end.
		std::mem::take(self.phase.get_mut().unwrap()).abort(moq_net::Error::Dropped);
	}
}

impl MoqTrackProducer {
	/// Wrap `raw` as a child of the broadcast `children` belongs to.
	fn child(raw: moq_net::track::Producer, children: &Children) -> Result<Arc<Self>, MoqError> {
		children.adopt(Arc::new(Self {
			phase: std::sync::Mutex::new(TrackPhase::Open {
				raw,
				family: children.clone(),
			}),
		}))
	}

	pub(crate) fn track_demand(&self) -> Result<moq_net::track::Demand, MoqError> {
		Ok(self.phase.lock().unwrap().raw()?.demand())
	}

	/// Hand the track to a typed writer that `f` builds (a JSON producer, say), closing this
	/// handle once `f` succeeds so the writer is the track's only producer. The writer takes this
	/// handle's place among its broadcast's children. On failure the handle stays open.
	pub(crate) fn adopt<T: Child + 'static>(
		&self,
		f: impl FnOnce(moq_net::track::Producer) -> Result<T, MoqError>,
	) -> Result<Arc<T>, MoqError> {
		// Taken for the whole construction, so a racing adopt finds the handle closed. `f` runs
		// outside the lock: it may take the broadcast's, which a close holds while it takes this one.
		let mut phase = std::mem::take(&mut *self.phase.lock().unwrap());
		let raw = phase.raw()?.clone();
		match (f(raw), phase) {
			(Ok(writer), TrackPhase::Open { family, .. }) => family.adopt(Arc::new(writer)),
			// A finished track has no family left to join, and ends at its declared end.
			(Ok(writer), _) => Ok(Arc::new(writer)),
			(Err(err), phase) => {
				let family = match &phase {
					TrackPhase::Open { family, .. } => Some(family.clone()),
					TrackPhase::Finished(_) | TrackPhase::Closed => None,
				};
				*self.phase.lock().unwrap() = phase;
				// Checked after the restore: a close during `f` found nothing to end, so the
				// restored track ends here, and a close after the restore ends it there.
				if family.is_some_and(|family| family.is_closed()) {
					Child::abort(self, moq_net::Error::Unroutable);
				}
				Err(err)
			}
		}
	}
}

#[uniffi::export]
impl MoqTrackProducer {
	/// Create a handler for uncached group fetches on this track.
	///
	/// Hold the returned object for as long as cache misses should wait to be
	/// served. Without a live dynamic handler, a missing group fails with `NotFound`.
	pub fn requests(&self) -> Result<Arc<MoqTrackRequests>, MoqError> {
		let _guard = crate::ffi::enter();
		let mut phase = self.phase.lock().unwrap();
		let raw = phase.raw()?;
		Ok(Arc::new(MoqTrackRequests::new(raw.requests())))
	}

	/// A watch-only handle to this track's name and whether it has subscribers.
	pub fn demand(&self) -> Result<Arc<MoqTrackDemand>, MoqError> {
		Ok(MoqTrackDemand::new(self.track_demand()?))
	}

	/// Create a consumer that reads from this producer's track.
	///
	/// Useful for local pub/sub without going through an origin/broadcast. `subscription`
	/// tunes delivery priority, group range, and staleness; omit for defaults.
	pub fn consume(&self, subscription: Option<MoqSubscription>) -> Result<Arc<MoqTrackConsumer>, MoqError> {
		let _guard = crate::ffi::enter();
		let mut phase = self.phase.lock().unwrap();
		let raw = phase.raw()?;
		let subscription = subscription.map(moq_net::track::Subscription::try_from).transpose()?;
		let track = raw.subscribe(subscription);
		Ok(Arc::new(MoqTrackConsumer::new(track, Background::default())))
	}

	/// Append the next group, starting at `start`, returning a producer for writing frames into it.
	///
	/// A timed track converts `start` into its timescale, as [`write_frame`](Self::write_frame)
	/// does; a scale-less track takes the [unscaled](MoqTimestamp::unscaled) group key. A start
	/// at or before a group the track holds, or one that doesn't match the track, is refused with
	/// [`MoqProtocolKind::TimestampMismatch`](crate::error::MoqProtocolKind::TimestampMismatch).
	pub fn append_group(&self, start: MoqTimestamp) -> Result<Arc<MoqGroupProducer>, MoqError> {
		let _guard = crate::ffi::enter();
		let start: moq_net::Timestamp = start.try_into()?;
		let mut phase = self.phase.lock().unwrap();
		let raw = phase.raw()?;
		let group = raw.append_group(start)?;
		Ok(Arc::new(MoqGroupProducer::from_inner(group)))
	}

	/// Create a group starting at `start`, in the track's timescale, which names it.
	///
	/// Use this for sparse or replayed tracks: unlike [`append_group`](Self::append_group), it
	/// declares nothing below it skipped.
	pub fn create_group(&self, start: u64) -> Result<Arc<MoqGroupProducer>, MoqError> {
		let _guard = crate::ffi::enter();
		let mut phase = self.phase.lock().unwrap();
		let raw = phase.raw()?;
		let group = raw.create_group(moq_net::group::Info { start })?;
		Ok(Arc::new(MoqGroupProducer::from_inner(group)))
	}

	/// Declare that groups `start` up to the exclusive `end` will never be produced.
	///
	/// Readers waiting on them move past without ending, and a relay forwards the skip. A
	/// skip is final: creating a group there is then refused. A group already written keeps
	/// what it is. [`append_group`](Self::append_group) skips the gap below it on its own.
	pub fn skip_groups(&self, start: u64, end: u64) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let mut phase = self.phase.lock().unwrap();
		let raw = phase.raw()?;
		raw.skip_groups(start..end);
		Ok(())
	}

	/// Write `frame` as a single-frame group.
	///
	/// A timestamp at another scale than the track's is converted to it, rounding down. The frame must
	/// match its track's timedness, otherwise it is refused with
	/// [`MoqProtocolKind::TimestampMismatch`](crate::error::MoqProtocolKind::TimestampMismatch).
	pub fn write_frame(&self, frame: MoqFrame) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let timestamp: moq_net::Timestamp = frame.timestamp.try_into()?;
		let mut phase = self.phase.lock().unwrap();
		// The group starts at its frame's time, or its unscaled key on a scale-less track.
		phase.raw()?.write_frame(timestamp, frame.payload)?;
		Ok(())
	}

	/// The newest content this track holds: its newest group, at that group's newest frame
	/// (0 while it is empty), or null before any group was written.
	///
	/// A publisher compares it with [`MoqTrackDemand`]'s wanted group to tell whether a join
	/// is already served by a group it wrote.
	pub fn largest(&self) -> Result<Option<MoqPosition>, MoqError> {
		let _guard = crate::ffi::enter();
		let mut phase = self.phase.lock().unwrap();
		let raw = phase.raw()?;
		Ok(raw.largest().map(MoqPosition::from))
	}

	/// Abort this track with an application error code.
	pub fn abort(&self, error_code: u16) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let mut phase = self.phase.lock().unwrap();
		match std::mem::take(&mut *phase) {
			TrackPhase::Open { raw, .. } | TrackPhase::Finished(raw) => raw.abort(moq_net::Error::App(error_code))?,
			TrackPhase::Closed => return Err(MoqError::Closed),
		}
		Ok(())
	}

	/// Abort this track with an application error code, but only while nobody wants it.
	///
	/// This is how an on-demand publisher ends a track once [`MoqTrackDemand::wanted_changed`]
	/// returns null. Releasing or [`abort`](Self::abort)ing it instead fails a reader that
	/// joined in between. Returns true once the track is closed, including when it already
	/// was, and this handle's later calls fail with `Closed`. Returns false, leaving the track
	/// unchanged, when a reader wants it again, so keep serving and wait for the next null
	/// on a fresh `demand()`: one that already returned null misses a reader that came and
	/// went, and waits on.
	pub fn abort_unwanted(&self, error_code: u16) -> Result<bool, MoqError> {
		let _guard = crate::ffi::enter();
		let mut phase = self.phase.lock().unwrap();
		let err = moq_net::Error::App(error_code);
		*phase = match std::mem::take(&mut *phase) {
			TrackPhase::Open { raw, family } => match raw.abort_unwanted(err) {
				Ok(()) => return Ok(true),
				Err(raw) => TrackPhase::Open { raw, family },
			},
			TrackPhase::Finished(raw) => match raw.abort_unwanted(err) {
				Ok(()) => return Ok(true),
				Err(raw) => TrackPhase::Finished(raw),
			},
			TrackPhase::Closed => return Err(MoqError::Closed),
		};
		Ok(false)
	}

	/// End the track at the live edge.
	///
	/// [`finish_at`](Self::finish_at) declares the boundary ahead of time, so this keeps
	/// that boundary. The handle remains so a later [`abort`](Self::abort) can still run, and
	/// the broadcast keeps the finished track until it closes.
	pub fn finish(&self) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let mut phase = self.phase.lock().unwrap();
		*phase = match std::mem::take(&mut *phase) {
			TrackPhase::Open { raw, family } => {
				// The kept handle stays a producer, whose appends the end refuses.
				if raw.finished().is_none()
					&& let Err(err) = raw.clone().finish()
				{
					*phase = TrackPhase::Open { raw, family };
					return Err(err.into());
				}
				family.keep(raw.clone());
				TrackPhase::Finished(raw)
			}
			finished @ TrackPhase::Finished(_) => finished,
			TrackPhase::Closed => return Err(MoqError::Closed),
		};
		Ok(())
	}

	/// Declare the exclusive final group start, possibly ahead of the live edge.
	///
	/// Groups below `final_start` may still be created afterwards. Groups at or
	/// above it are rejected. The producer remains open for groups below the boundary;
	/// call [`finish`](Self::finish) after producing the remaining groups.
	pub fn finish_at(&self, final_start: u64) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let mut phase = self.phase.lock().unwrap();
		let raw = phase.raw()?;
		raw.finish_at(moq_net::track::Position::group(final_start))?;
		Ok(())
	}
}

/// A group to which frames can be published.
#[derive(uniffi::Object)]
pub struct MoqGroupProducer {
	start: u64,
	/// The group keeps one writer waker, so every [`ready`](Self::ready) call polls it with the
	/// bridge's, which wakes them all. Every other call locks it through the bridge too, so a
	/// wake it causes reaches a foreign continuation only after the lock is released.
	inner: kio::Bridge<GroupPhase>,
}

/// How far a group binding got: writing, finished but still abortable, or closed.
#[derive(Default)]
enum GroupPhase {
	Writing(moq_net::group::Producer),
	Finished(moq_net::group::Finished),
	#[default]
	Closed,
}

impl Drop for MoqGroupProducer {
	fn drop(&mut self) {
		let _guard = crate::ffi::enter();
		if let GroupPhase::Writing(group) = std::mem::take(self.inner.get_mut())
			&& group.poll_closed(&kio::Waiter::noop()).is_pending()
		{
			let _ = group.abort(moq_net::Error::Dropped);
		}
	}
}

impl MoqGroupProducer {
	/// Wrap `group`, for a track's writer.
	pub(crate) fn from_inner(group: moq_net::group::Producer) -> Self {
		Self {
			start: group.start,
			inner: kio::Bridge::new(GroupPhase::Writing(group)),
		}
	}
}

#[uniffi::export]
impl MoqGroupProducer {
	/// The start of this group, in its track's timescale, which names it within the track.
	pub fn start(&self) -> u64 {
		self.start
	}

	/// Create a consumer that reads frames from this group.
	pub fn consume(&self) -> Result<Arc<MoqGroupConsumer>, MoqError> {
		let _guard = crate::ffi::enter();
		let consumer = match &*self.inner.lock() {
			GroupPhase::Writing(group) => group.consume(),
			GroupPhase::Finished(group) => group.consume(),
			GroupPhase::Closed => return Err(MoqError::Closed),
		};
		Ok(Arc::new(MoqGroupConsumer::new(consumer, Background::default())))
	}

	/// Write `frame` into this group.
	///
	/// A timestamp at another scale than the track's is converted to it, rounding down. The frame must
	/// match its track's timedness, otherwise it is refused with
	/// [`MoqProtocolKind::TimestampMismatch`](crate::error::MoqProtocolKind::TimestampMismatch).
	pub fn write_frame(&self, frame: MoqFrame) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let timestamp = frame.timestamp.try_into()?;
		let mut guard = self.inner.lock();
		let GroupPhase::Writing(group) = &mut *guard else {
			return Err(MoqError::Closed);
		};
		group.write_frame(timestamp, frame.payload)?;
		Ok(())
	}

	/// Wait until a frame of `size` bytes fits, so a writer runs no further ahead of its
	/// fastest reader than the group's window.
	///
	/// Await this before each [`write_frame`](Self::write_frame) for backpressure: that reader
	/// paces the writer instead of the group shedding frames it has not taken, which fails it
	/// with `Evicted`; slower readers are not waited on. Writing without waiting never fails; the group
	/// sheds its front instead. Resolves once a reader holds the group and the frame fits, at
	/// once while a reader has taken every frame, and once the group is closed, so the next
	/// write reports why. Fails with `Closed` after [`abort`](Self::abort).
	pub async fn ready(&self, size: u64) -> Result<(), MoqError> {
		let mut park = kio::Park::default();
		crate::ffi::run(kio::wait(|waiter| {
			self.inner.poll(waiter, |group, cx| match group {
				GroupPhase::Writing(group) => {
					// Only the size paces; the timestamp is checked by the write.
					let frame = moq_net::frame::Info {
						size,
						timestamp: moq_net::Timestamp::unscaled(group.start),
					};
					group.poll_ready(park.hold(cx), &frame).map(Ok)
				}
				// The next write reports why.
				GroupPhase::Finished(_) => Poll::Ready(Ok(())),
				GroupPhase::Closed => Poll::Ready(Err(MoqError::Closed)),
			})
		}))
		.await
	}

	/// Mark the group as complete. No more frames can be written.
	///
	/// The handle remains so a later [`abort`](Self::abort) can still run.
	pub fn finish(&self) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let mut guard = self.inner.lock();
		match std::mem::take(&mut *guard) {
			GroupPhase::Writing(group) => *guard = GroupPhase::Finished(group.finish()?),
			GroupPhase::Finished(group) => *guard = GroupPhase::Finished(group),
			GroupPhase::Closed => return Err(MoqError::Closed),
		}
		Ok(())
	}

	/// Abort this group with an application error code.
	pub fn abort(&self, error_code: u16) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let err = moq_net::Error::App(error_code);
		match std::mem::take(&mut *self.inner.lock()) {
			GroupPhase::Writing(group) => group.abort(err)?,
			GroupPhase::Finished(group) => group.abort(err)?,
			GroupPhase::Closed => return Err(MoqError::Closed),
		}
		Ok(())
	}
}

#[cfg(test)]
impl MoqGroupProducer {
	/// Wait until a consumer wants this group.
	pub(crate) async fn wanted(&self) -> Result<(), MoqError> {
		let mut demand = match &*self.inner.lock() {
			GroupPhase::Writing(group) => group.demand(),
			_ => return Err(MoqError::Closed),
		};
		while !demand.wanted_changed().await? {}
		Ok(())
	}
}

#[cfg(test)]
mod metadata_tests {
	use super::*;

	#[test]
	fn dropping_a_group_binding_ends_its_unfinished_group() {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast.create_track("test", moq_net::track::Info::default()).unwrap();
		let group = track.append_group(moq_net::Timestamp::unscaled(0)).unwrap();
		let mut consumer = group.consume();
		let binding = MoqGroupProducer::from_inner(group);
		drop(binding);
		assert!(matches!(
			consumer.poll_read_frame(&kio::Waiter::noop()),
			std::task::Poll::Ready(Err(moq_net::Error::Dropped))
		));
	}

	#[test]
	fn repeated_finishes_keep_the_track_once() {
		let broadcast = MoqBroadcastProducer::new().unwrap();
		let track = broadcast.publish_track("data".into(), None).unwrap();
		for _ in 0..3 {
			track.finish().unwrap();
		}
		let state = broadcast.state.lock().unwrap();
		let kept = match &*state.as_ref().unwrap().children.family.lock().unwrap() {
			Family::Open { kept, .. } => kept.len(),
			_ => unreachable!("the broadcast is still open"),
		};
		drop(state);
		assert_eq!(kept, 1);
	}

	#[test]
	fn untimed_info_round_trips_through_the_binding() {
		let original = moq_net::track::Info::default();
		let info = MoqTrackInfo::from(&original);
		assert_eq!(moq_net::track::Info::try_from(info).unwrap().timescale, None);
		assert_eq!(raw_track_info(None).unwrap().timescale, None);
	}

	#[test]
	fn max_age_survives_binding_conversion() {
		let info = |max_age| MoqTrackInfo {
			priority: 0,
			max_age,
			refresh: MoqTrackRefresh::default(),
			timescale: None,
			compression: None,
		};
		let zero = Some(std::time::Duration::ZERO);
		assert_eq!(raw_track_info(None).unwrap().max_age, zero);
		// Null is no limit, as `MoqDelay.max` reads it, and zero is the live edge only.
		for max_age in [None, zero, Some(std::time::Duration::from_secs(5))] {
			let model = moq_net::track::Info::try_from(info(max_age)).unwrap();
			assert_eq!(model.max_age, max_age);
			// A received record republishes unchanged.
			let back = MoqTrackInfo::from(&model);
			assert_eq!(back.max_age, max_age);
		}
	}

	#[test]
	fn compression_survives_binding_conversion() {
		let model = moq_net::track::Info::default();
		assert!(MoqTrackInfo::from(&model).compression.is_none());
		let deflate = model.with_compression(moq_net::track::Compression::Deflate);
		let info = MoqTrackInfo::from(&deflate);
		let read = info.compression.clone().unwrap();
		assert_eq!(*read, *MoqCompression::deflate());
		assert_eq!(read.name(), "deflate");
		assert_eq!(
			moq_net::track::Info::try_from(info).unwrap().compression,
			moq_net::track::Compression::Deflate
		);
	}

	/// C and C++ abort on a duration past `i64::MAX` nanoseconds, so a peer's longer age saturates.
	#[test]
	fn oversized_max_age_saturates_for_the_bindings() {
		let model = moq_net::track::Info::default().with_max_age(std::time::Duration::MAX);
		let max_age = MoqTrackInfo::from(&model).max_age.unwrap();
		assert_eq!(max_age, std::time::Duration::from_nanos(i64::MAX as u64));
	}
}
