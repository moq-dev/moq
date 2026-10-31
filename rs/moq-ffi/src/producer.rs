//! Publish broadcasts, tracks, and groups.

use std::sync::Arc;

use moq_mux::catalog::hang::Extra;

use crate::consumer::{MoqBroadcastConsumer, MoqGroupConsumer, MoqOrder, MoqSubscription, MoqTrackConsumer};
use crate::demand::MoqTrackDemand;
use crate::error::MoqError;
use crate::ffi::{Background, Task};
use crate::media::MoqFrame;

/// Publisher-side track properties, mirroring [`moq_net::track::Info`].
///
/// Construct with the fields you care about; the rest use raw-track defaults
/// (priority 0, no publisher age limit, untimed).
#[derive(Clone, uniffi::Record)]
pub struct MoqTrackInfo {
	/// Priority, used only to break ties between subscriptions of equal subscriber priority.
	/// Higher is more urgent; the default 0 is the midpoint.
	#[uniffi(default = 0)]
	pub priority: i8,
	/// Maximum age of a non-latest group before the publisher evicts it. Null imposes no
	/// publisher age limit. This is the publisher-side half of
	/// [`MoqSubscription::max_delay`](crate::consumer::MoqSubscription::max_delay).
	#[uniffi(default = None)]
	pub max_age: Option<std::time::Duration>,
	/// How many times its first frame a group's later bytes may grow before a joining
	/// subscriber asks for a new group, or null to never ask. See
	/// [`MoqTrackDemand::wanted`](crate::demand::MoqTrackDemand::wanted).
	#[uniffi(default = None)]
	pub refresh: Option<u32>,
	/// Per-frame timescale in ticks per second, or null for an untimed track.
	#[uniffi(default = None)]
	pub timescale: Option<u64>,
	/// Whether each group is DEFLATE-compressed on the wire to peers that support it.
	/// Readers always see the original frames.
	#[uniffi(default = false)]
	pub compress: bool,
}

impl TryFrom<MoqTrackInfo> for moq_net::track::Info {
	type Error = MoqError;

	fn try_from(info: MoqTrackInfo) -> Result<Self, MoqError> {
		let mut out = moq_net::track::Info::default()
			.with_priority(info.priority)
			.with_refresh(info.refresh);
		if let Some(max_age) = info.max_age {
			out = out.with_max_age(max_age);
		}
		if let Some(ticks) = info.timescale {
			let scale = moq_net::Timescale::new(ticks).map_err(|_| MoqError::Codec {
				reason: format!("invalid timescale: {ticks}"),
			})?;
			out = out.with_timescale(scale);
		}
		Ok(out.with_compress(info.compress))
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
			refresh: info.refresh,
			timescale: info.timescale.map(|timescale| timescale.as_u64()),
			compress: info.compress,
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

/// `Send + Sync` natively, where any thread may close a broadcast; nothing on single-threaded wasm.
#[cfg(not(target_family = "wasm"))]
pub(crate) trait Shared: Send + Sync {}
#[cfg(not(target_family = "wasm"))]
impl<T: Send + Sync + ?Sized> Shared for T {}
#[cfg(target_family = "wasm")]
pub(crate) trait Shared {}
#[cfg(target_family = "wasm")]
impl<T: ?Sized> Shared for T {}

/// A handle publishing tracks on a broadcast, which its broadcast ends on close.
pub(crate) trait Child: Shared {
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
		kept: Vec<Box<dyn kio::MaybeSend>>,
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
pub(crate) struct Children(Arc<std::sync::Mutex<Family>>);

impl Default for Children {
	fn default() -> Self {
		Self(Arc::new(std::sync::Mutex::new(Family::Open {
			open: Vec::new(),
			kept: Vec::new(),
		})))
	}
}

impl Children {
	/// Register a new child so a close ends it. Once the broadcast closed, the child is aborted
	/// and refused with `Closed`.
	pub(crate) fn adopt<T: Child + 'static>(&self, child: Arc<T>) -> Result<Arc<T>, MoqError> {
		let mut family = self.0.lock().unwrap();
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
		matches!(*self.0.lock().unwrap(), Family::Closed)
	}

	/// Keep a finished producer until the broadcast ends, or drop it now if it already has.
	pub(crate) fn keep(&self, producer: impl kio::MaybeSend + 'static) {
		let producer: Box<dyn kio::MaybeSend> = Box::new(producer);
		if let Family::Open { kept, .. } = &mut *self.0.lock().unwrap() {
			kept.push(producer);
		}
	}

	/// End every open child with `Unroutable`, then let go of the finished ones.
	fn close(&self) {
		let family = std::mem::replace(&mut *self.0.lock().unwrap(), Family::Closed);
		if let Family::Open { open, kept } = family {
			// Outside the lock: a child's own finish keeps into it while holding the child's lock.
			for child in open.iter().filter_map(std::sync::Weak::upgrade) {
				child.abort(moq_net::Error::Unroutable);
			}
			drop(kept);
		}
	}

	/// Let go of the finished producers, leaving open children to end on their own.
	fn release(&self) {
		let mut family = self.0.lock().unwrap();
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
	task: Task<RequestsProducer>,
}

/// Serves on-demand fetches of uncached groups for one track.
#[derive(uniffi::Object)]
pub struct MoqTrackRequests {
	task: Task<TrackDynamicProducer>,
}

impl MoqTrackRequests {
	fn new(inner: moq_net::track::Requests) -> Self {
		Self {
			task: Task::new(TrackDynamicProducer { inner }),
		}
	}
}

struct RequestsProducer {
	inner: moq_net::broadcast::Requests,
	children: Children,
}

struct TrackDynamicProducer {
	inner: moq_net::track::Requests,
}

impl RequestsProducer {
	async fn requested_track(&mut self) -> Result<Arc<MoqTrackRequest>, MoqError> {
		// Hand back the un-accepted request, mirroring `moq_net::broadcast::Requests`: the caller
		// accepts it (raw, at a chosen timescale) or publishes media onto it (importer accepts).
		// The subscriber's subscribe stays pending until then.
		let request = self.inner.requested_track().await?;
		Ok(Arc::new(MoqTrackRequest::new(request, self.children.clone())))
	}
}

impl TrackDynamicProducer {
	async fn requested_fill(&mut self) -> Result<Arc<MoqFill>, MoqError> {
		let fill = self.inner.requested_fill().await?;
		Ok(Arc::new(MoqFill { inner: Arc::new(fill) }))
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
				broadcast,
				catalog,
				timebases: Default::default(),
				driver,
				children: Default::default(),
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
			Ok(Arc::new(MoqBroadcastRequests {
				task: Task::new(RequestsProducer {
					inner: state.broadcast.requests(),
					children: state.children.clone(),
				})
				.supervised(state.driver.clone()),
			}))
		})
	}

	/// Create a standalone broadcast, not attached to any origin.
	///
	/// Use it for local pub/sub via [`consume`](Self::consume). To publish at a path, use
	/// [`MoqOriginProducer::create_broadcast`](crate::origin::MoqOriginProducer::create_broadcast) instead.
	#[uniffi::constructor]
	pub fn new() -> Result<Arc<Self>, MoqError> {
		let _guard = crate::ffi::enter();
		Ok(Arc::new(Self::from_inner(
			moq_net::broadcast::Info::new().produce(),
			Background::default(),
		)?))
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
			// Clone the broadcast handle (shared Arc internally) to get &mut access.
			let broadcast = state.broadcast.clone();
			let producer = broadcast.create_track(name, Some(info))?;
			MoqTrackProducer::child(producer, &state.children)
		})
	}

	/// End the broadcast for good: retract it, serve no new tracks, and finalize the catalog.
	///
	/// Every track handle still open on it ends with `Unroutable`, the way a reader sees a
	/// publisher leave, and its later calls fail with `Closed`. Finish a track first to end it
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

// ---- Broadcast Requests ----

#[uniffi::export]
impl MoqBroadcastRequests {
	/// Wait for the next subscriber-requested track.
	///
	/// Returns a [`MoqTrackRequest`]: accept it for raw writes with
	/// [`MoqTrackRequest::accept`], publish media onto it with
	/// [`crate::media::MoqMediaTrackProducer::audio`], or reject it with
	/// [`MoqTrackRequest::abort`]. The requesting subscriber stays pending until then.
	///
	/// Returns an error once the broadcast is closed or aborted.
	pub async fn requested_track(&self) -> Result<Arc<MoqTrackRequest>, MoqError> {
		self.task
			.run(|mut state| async move { state.requested_track().await })
			.await
	}

	/// Cancel all current and future `requested_track()` calls.
	///
	/// Terminal: the dynamic broadcast is released here, not when the handle is, so any pending
	/// request is rejected.
	pub fn cancel(&self) {
		self.task.cancel();
	}
}

// ---- Track Requests ----

#[uniffi::export]
impl MoqTrackRequests {
	/// Wait for the next run of groups readers miss: a fetch of an uncached group, or
	/// history a subscription reads that the cache no longer holds.
	///
	/// Cached groups are served without reaching this method.
	pub async fn requested_fill(&self) -> Result<Arc<MoqFill>, MoqError> {
		self.task
			.run(|mut state| async move { state.requested_fill().await })
			.await
	}

	/// Cancel all current and future `requested_fill()` calls.
	///
	/// Terminal: the dynamic track is released here, not when the handle is, so any pending
	/// fetch is rejected.
	pub fn cancel(&self) {
		self.task.cancel();
	}
}

/// A run of groups readers miss, mirroring [`moq_net::track::Fill`].
///
/// Call [`accept`](Self::accept) for each group you have and [`drop_groups`](Self::drop_groups)
/// for the rest; [`next`](Self::next) names what is still owed, in the fill's order. Releasing
/// it names whatever is left as dropped while a reader still wants it; once nobody does, it is
/// withdrawn, so stop serving it when [`unused`](Self::unused) resolves. Stop serving a single
/// group when [`unwanted`](Self::unwanted) resolves for it, and move on to the next.
#[derive(uniffi::Object)]
pub struct MoqFill {
	inner: Arc<moq_net::track::Fill>,
}

#[uniffi::export]
impl MoqFill {
	/// The first group to fill.
	pub fn start_group(&self) -> u64 {
		self.inner.start().group
	}

	/// The frame within the first group to start from.
	pub fn start_frame(&self) -> u64 {
		self.inner.start().frame
	}

	/// The exclusive end group.
	pub fn end_group(&self) -> u64 {
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

	/// The next group still owed in the fill's order, or null once every group is answered.
	pub fn next(&self) -> Option<u64> {
		self.inner.next()
	}

	/// Insert group `sequence` and return a producer for filling it. The first group starts
	/// at [`start_frame`](Self::start_frame), so its frames keep their indices.
	pub fn accept(&self, sequence: u64) -> Result<Arc<MoqGroupProducer>, MoqError> {
		let _guard = crate::ffi::enter();
		let group = self.inner.accept(sequence, None)?;
		Ok(Arc::new(MoqGroupProducer {
			sequence: group.sequence,
			inner: std::sync::Mutex::new(Some(group)),
		}))
	}

	/// Name groups `start` up to the exclusive `end` as never to be delivered.
	pub fn drop_groups(&self, start: u64, end: u64) {
		let _guard = crate::ffi::enter();
		self.inner.drop_groups(start..end);
	}

	/// Whether a reader still waits on this fill.
	pub fn is_used(&self) -> bool {
		self.inner.is_used()
	}

	/// Wait until no reader waits on this fill.
	pub async fn unused(&self) -> Result<(), MoqError> {
		let fill = self.inner.clone();
		crate::ffi::detached(async move {
			fill.unused().await;
			Ok::<_, MoqError>(())
		})
		.await
	}

	/// Whether a reader still waits for group `sequence` from this fill: false once every
	/// reader took it or narrowed past it, or nobody waits on the fill.
	pub fn is_wanted(&self, sequence: u64) -> bool {
		self.inner.is_wanted(sequence)
	}

	/// Wait until no reader waits for group `sequence` from this fill, so stop serving it.
	pub async fn unwanted(&self, sequence: u64) -> Result<(), MoqError> {
		let fill = self.inner.clone();
		crate::ffi::detached(async move {
			fill.unwanted(sequence).await;
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
/// or [`abort`](Self::abort) it to reject the waiting subscriber.
#[derive(uniffi::Object)]
pub struct MoqTrackRequest {
	inner: std::sync::Mutex<Option<moq_net::track::Request>>,
	children: Children,
}

impl MoqTrackRequest {
	pub(crate) fn new(request: moq_net::track::Request, children: Children) -> Self {
		Self {
			inner: std::sync::Mutex::new(Some(request)),
			children,
		}
	}

	/// Take the inner request so an importer can accept it (setting the timescale). Used by
	/// [`crate::media::MoqMediaTrackProducer::audio`].
	pub(crate) fn take(&self) -> Result<moq_net::track::Request, MoqError> {
		self.inner.lock().unwrap().take().ok_or(MoqError::Closed)
	}
}

#[uniffi::export]
impl MoqTrackRequest {
	/// The requested track name.
	pub fn name(&self) -> Result<String, MoqError> {
		let guard = self.inner.lock().unwrap();
		let request = guard.as_ref().ok_or(MoqError::Closed)?;
		Ok(request.name().to_string())
	}

	/// Watch this track's subscribers, including after the request is accepted.
	pub fn demand(&self) -> Result<Arc<MoqTrackDemand>, MoqError> {
		let guard = self.inner.lock().unwrap();
		Ok(MoqTrackDemand::new(guard.as_ref().ok_or(MoqError::Closed)?.demand()))
	}

	/// Create a handler for uncached group fetches before accepting this track.
	///
	/// Obtain and retain this handle before `accept()` when the track itself was
	/// requested by a fetch. This keeps the pending group request serviceable across
	/// the transition from request to producer.
	pub fn requests(&self) -> Result<Arc<MoqTrackRequests>, MoqError> {
		let _guard = crate::ffi::enter();
		let guard = self.inner.lock().unwrap();
		let request = guard.as_ref().ok_or(MoqError::Closed)?;
		Ok(Arc::new(MoqTrackRequests::new(request.requests())))
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
		let request = self.take()?;
		MoqTrackProducer::child(request.accept(Some(info)), &self.children)
	}

	/// Reject the request with an application error code, failing the waiting subscriber.
	pub fn abort(&self, error_code: u16) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let request = self.take()?;
		request.reject(moq_net::Error::App(error_code));
		Ok(())
	}
}

// ---- Track Producer ----

/// A track to which groups, frames, or datagrams can be published.
///
/// Released before its finish, or still open when its broadcast closes, it ends its track with
/// `Unroutable`, which readers take as the publisher leaving.
#[derive(uniffi::Object)]
pub struct MoqTrackProducer {
	inner: std::sync::Mutex<Option<moq_net::track::Producer>>,
	// Taken by the first finish, so repeating it keeps the track only once.
	keep: std::sync::Mutex<Option<Children>>,
}

impl Child for MoqTrackProducer {
	fn abort(&self, err: moq_net::Error) {
		let _guard = crate::ffi::enter();
		// A finished track ends at its declared end, as when its handle drops.
		let track = self.inner.lock().unwrap().take();
		if let Some(track) = track.filter(|track| track.finished().is_none()) {
			let _ = track.abort(err);
		}
	}
}

impl Drop for MoqTrackProducer {
	fn drop(&mut self) {
		let _guard = crate::ffi::enter();
		// A finished track lives on in its broadcast, and may still take groups below its end.
		let track = self.inner.get_mut().unwrap().take();
		if let Some(track) = track.filter(|track| track.finished().is_none()) {
			let _ = track.abort(moq_net::Error::Unroutable);
		}
	}
}

impl MoqTrackProducer {
	/// Wrap `track` as a child of the broadcast `children` belongs to.
	fn child(track: moq_net::track::Producer, children: &Children) -> Result<Arc<Self>, MoqError> {
		children.adopt(Arc::new(Self {
			inner: std::sync::Mutex::new(Some(track)),
			keep: std::sync::Mutex::new(Some(children.clone())),
		}))
	}

	pub(crate) fn track_demand(&self) -> Result<moq_net::track::Demand, MoqError> {
		let guard = self.inner.lock().unwrap();
		let track = guard.as_ref().ok_or(MoqError::Closed)?;
		Ok(track.demand())
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
		let track = self.inner.lock().unwrap().take().ok_or(MoqError::Closed)?;
		let writer = match f(track.clone()) {
			Ok(writer) => Arc::new(writer),
			Err(err) => {
				*self.inner.lock().unwrap() = Some(track);
				// A close during `f` found nothing to end, so the restored track ends here.
				let children = self.keep.lock().unwrap().clone();
				if children.is_some_and(|children| children.is_closed()) {
					Child::abort(self, moq_net::Error::Unroutable);
				}
				return Err(err);
			}
		};
		// A finished track has no family left to join, and ends at its declared end.
		let children = self.keep.lock().unwrap().clone();
		match children {
			Some(children) => children.adopt(writer),
			None => Ok(writer),
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
		let guard = self.inner.lock().unwrap();
		let track = guard.as_ref().ok_or(MoqError::Closed)?;
		Ok(Arc::new(MoqTrackRequests::new(track.requests())))
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
		let guard = self.inner.lock().unwrap();
		let track = guard.as_ref().ok_or(MoqError::Closed)?;
		let subscription = subscription.map(moq_net::track::Subscription::try_from).transpose()?;
		Ok(Arc::new(MoqTrackConsumer::new(
			track.subscribe(subscription),
			Background::default(),
		)))
	}

	/// Append a new group to the track, returning a producer for writing frames into it.
	pub fn append_group(&self) -> Result<Arc<MoqGroupProducer>, MoqError> {
		let _guard = crate::ffi::enter();
		let mut guard = self.inner.lock().unwrap();
		let track = guard.as_mut().ok_or(MoqError::Closed)?;
		let group = track.append_group()?;
		Ok(Arc::new(MoqGroupProducer {
			sequence: group.sequence,
			inner: std::sync::Mutex::new(Some(group)),
		}))
	}

	/// Create a group with an explicit sequence number.
	///
	/// Use this for sparse or replayed tracks. [`append_group`](Self::append_group)
	/// remains the convenient live-stream path.
	pub fn create_group(&self, sequence: u64) -> Result<Arc<MoqGroupProducer>, MoqError> {
		let _guard = crate::ffi::enter();
		let mut guard = self.inner.lock().unwrap();
		let track = guard.as_mut().ok_or(MoqError::Closed)?;
		let group = track.create_group(moq_net::group::Info { sequence })?;
		Ok(Arc::new(MoqGroupProducer {
			sequence: group.sequence,
			inner: std::sync::Mutex::new(Some(group)),
		}))
	}

	/// Write `frame` as a single-frame group.
	///
	/// Custom timescales may round the timestamp during conversion. The frame must
	/// match its track's timedness, otherwise it is refused with
	/// [`MoqProtocolKind::TimestampMismatch`](crate::error::MoqProtocolKind::TimestampMismatch).
	pub fn write_frame(&self, frame: MoqFrame) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let timestamp = frame.timestamp_us.map(moq_net::Timestamp::from_micros).transpose()?;
		let mut guard = self.inner.lock().unwrap();
		let track = guard.as_mut().ok_or(MoqError::Closed)?;
		track.write_frame(timestamp, frame.payload)?;
		Ok(())
	}

	/// Send `frame` as a best-effort datagram, returning the sequence number assigned to it.
	///
	/// The payload must be at most 1200 bytes. Datagrams are only delivered on transports and
	/// wire versions with a datagram channel; there is no stream fallback. Like a frame,
	/// its timestamp must match the track's timedness.
	pub fn append_datagram(&self, frame: MoqFrame) -> Result<u64, MoqError> {
		let _guard = crate::ffi::enter();
		let timestamp = frame.timestamp_us.map(moq_net::Timestamp::from_micros).transpose()?;
		let mut guard = self.inner.lock().unwrap();
		let track = guard.as_mut().ok_or(MoqError::Closed)?;
		Ok(track.append_datagram(timestamp, frame.payload)?)
	}

	/// Abort this track with an application error code.
	pub fn abort(&self, error_code: u16) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let mut guard = self.inner.lock().unwrap();
		let track = guard.take().ok_or(MoqError::Closed)?;
		track.abort(moq_net::Error::App(error_code))?;
		Ok(())
	}

	/// End the track at the live edge.
	///
	/// [`finish_at`](Self::finish_at) declares the boundary ahead of time, so this keeps
	/// that boundary. The handle remains so a later [`abort`](Self::abort) can still run, and
	/// the broadcast keeps the finished track until it closes.
	pub fn finish(&self) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let mut guard = self.inner.lock().unwrap();
		let track = guard.as_mut().ok_or(MoqError::Closed)?;
		if track.finished().is_none() {
			track.finish()?;
		}
		if let Some(children) = self.keep.lock().unwrap().take() {
			children.keep(track.clone());
		}
		Ok(())
	}

	/// Declare the exclusive final group sequence, possibly ahead of the live edge.
	///
	/// Groups below `final_sequence` may still be created afterwards. Groups at or
	/// above it are rejected. The producer remains open for groups below the boundary;
	/// call [`finish`](Self::finish) after producing the remaining groups.
	pub fn finish_at(&self, final_sequence: u64) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let mut guard = self.inner.lock().unwrap();
		let track = guard.as_mut().ok_or(MoqError::Closed)?;
		track.finish_at(moq_net::track::Position::group(final_sequence))?;
		Ok(())
	}
}

/// A group to which frames can be published.
#[derive(uniffi::Object)]
pub struct MoqGroupProducer {
	sequence: u64,
	inner: std::sync::Mutex<Option<moq_net::group::Producer>>,
}

#[uniffi::export]
impl MoqGroupProducer {
	/// The sequence number of this group within the track.
	pub fn sequence(&self) -> u64 {
		self.sequence
	}

	/// Create a consumer that reads frames from this group.
	pub fn consume(&self) -> Result<Arc<MoqGroupConsumer>, MoqError> {
		let _guard = crate::ffi::enter();
		let guard = self.inner.lock().unwrap();
		let group = guard.as_ref().ok_or(MoqError::Closed)?;
		Ok(Arc::new(MoqGroupConsumer::new(group.consume(), Background::default())))
	}

	/// Write `frame` into this group.
	///
	/// Custom timescales may round the timestamp during conversion. The frame must
	/// match its track's timedness, otherwise it is refused with
	/// [`MoqProtocolKind::TimestampMismatch`](crate::error::MoqProtocolKind::TimestampMismatch).
	pub fn write_frame(&self, frame: MoqFrame) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let timestamp = frame.timestamp_us.map(moq_net::Timestamp::from_micros).transpose()?;
		let mut guard = self.inner.lock().unwrap();
		let group = guard.as_mut().ok_or(MoqError::Closed)?;
		group.write_frame(timestamp, frame.payload)?;
		Ok(())
	}

	/// Mark the group as complete. No more frames can be written.
	///
	/// The handle remains so a later [`abort`](Self::abort) can still run.
	pub fn finish(&self) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let mut guard = self.inner.lock().unwrap();
		let group = guard.as_mut().ok_or(MoqError::Closed)?;
		group.finish()?;
		Ok(())
	}

	/// Abort this group with an application error code.
	pub fn abort(&self, error_code: u16) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let mut guard = self.inner.lock().unwrap();
		let group = guard.take().ok_or(MoqError::Closed)?;
		group.abort(moq_net::Error::App(error_code))?;
		Ok(())
	}
}

#[cfg(test)]
impl MoqGroupProducer {
	/// Wait until a consumer wants this group.
	pub(crate) async fn wanted(&self) -> Result<(), MoqError> {
		let mut demand = self.inner.lock().unwrap().as_ref().ok_or(MoqError::Closed)?.demand();
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
		let group = track.append_group().unwrap();
		let mut consumer = group.consume();
		let binding = MoqGroupProducer {
			sequence: group.sequence,
			inner: std::sync::Mutex::new(Some(group)),
		};
		drop(binding);
		assert!(matches!(
			consumer.poll_read_frame(&moq_net::kio::Waiter::noop()),
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
		let kept = match &*state.as_ref().unwrap().children.0.lock().unwrap() {
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
	fn optional_retention_survives_binding_conversion() {
		for max_age in [
			None,
			Some(std::time::Duration::ZERO),
			Some(std::time::Duration::from_secs(30)),
		] {
			let info = MoqTrackInfo {
				priority: 0,
				max_age,
				refresh: None,
				timescale: None,
				compress: false,
			};
			let model = moq_net::track::Info::try_from(info).unwrap();
			assert_eq!(model.max_age, max_age);
			assert_eq!(MoqTrackInfo::from(&model).max_age, max_age);
		}
	}

	/// C and C++ abort on a duration past `i64::MAX` nanoseconds, so a peer's longer age saturates.
	#[test]
	fn oversized_retention_saturates_for_the_bindings() {
		let model = moq_net::track::Info::default().with_max_age(std::time::Duration::MAX);
		let max_age = MoqTrackInfo::from(&model).max_age.unwrap();
		assert_eq!(max_age, std::time::Duration::from_nanos(i64::MAX as u64));
	}
}
