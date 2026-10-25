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
	pub(crate) publication: Option<moq_net::origin::Publication>,
	// Carries the untyped `Extra` extension so callers can attach application catalog
	// sections by name (the only extension shape that crosses the FFI boundary).
	pub(crate) catalog: moq_mux::catalog::Producer<Extra>,
	// Core timebases own catalog outputs, so the broadcast holds them and binding handles stay weak.
	pub(crate) timebases: std::collections::HashMap<usize, moq_mux::catalog::Timebase<Extra>>,
	pub(crate) driver: Background,
	pub(crate) finished: Finished,
	/// Set by [`MoqMediaCatalogProducer::finish`](crate::media::MoqMediaCatalogProducer::finish): the
	/// catalog is final, so a close leaves it alone.
	pub(crate) ended: bool,
}

impl Drop for BroadcastProducer {
	fn drop(&mut self) {
		self.finished.release();
	}
}

/// The finished importers a broadcast keeps, or `None` once it ended.
type Keeping = Option<Vec<Box<dyn kio::MaybeSend>>>;

/// Finished producers a broadcast keeps until it ends.
///
/// A finished track stops being served once its last producer drops. An importer lets go of
/// its producer when it finishes, and a binding may release a raw track's handle whenever its
/// garbage collector runs, so the broadcast keeps them here instead, and a subscriber arriving
/// after the end still finds the track until the broadcast closes.
#[derive(Clone)]
pub(crate) struct Finished(Arc<std::sync::Mutex<Keeping>>);

impl Default for Finished {
	fn default() -> Self {
		Self(Arc::new(std::sync::Mutex::new(Some(Vec::new()))))
	}
}

impl Finished {
	/// Keep a finished producer until the broadcast ends, or drop it now if it already has.
	pub(crate) fn keep(&self, producer: impl kio::MaybeSend + 'static) {
		let producer: Box<dyn kio::MaybeSend> = Box::new(producer);
		let mut kept = self.0.lock().unwrap();
		if let Some(kept) = kept.as_mut() {
			kept.push(producer);
		}
	}

	/// Stop keeping, handing back what was kept so the caller decides when it drops.
	fn release(&self) -> Vec<Box<dyn kio::MaybeSend>> {
		self.0.lock().unwrap().take().unwrap_or_default()
	}
}

/// A broadcast in which tracks can be published.
#[derive(uniffi::Object)]
pub struct MoqBroadcastProducer {
	pub(crate) state: std::sync::Mutex<Option<BroadcastProducer>>,
}

/// A handler for requests for tracks in a broadcast.
#[derive(uniffi::Object)]
pub struct MoqBroadcastDynamic {
	task: Task<DynamicProducer>,
}

/// Serves on-demand fetches of uncached groups for one track.
#[derive(uniffi::Object)]
pub struct MoqTrackDynamic {
	task: Task<TrackDynamicProducer>,
}

impl MoqTrackDynamic {
	fn new(inner: moq_net::track::Dynamic) -> Self {
		Self {
			task: Task::new(TrackDynamicProducer { inner }),
		}
	}
}

struct DynamicProducer {
	inner: moq_net::broadcast::Dynamic,
	finished: Finished,
}

struct TrackDynamicProducer {
	inner: moq_net::track::Dynamic,
}

impl DynamicProducer {
	async fn requested_track(&mut self) -> Result<Arc<MoqTrackRequest>, MoqError> {
		// Hand back the un-accepted request, mirroring `moq_net::broadcast::Dynamic`: the caller
		// accepts it (raw, at a chosen timescale) or publishes media onto it (importer accepts).
		// The subscriber's subscribe stays pending until then.
		let request = self.inner.requested_track().await?;
		Ok(Arc::new(MoqTrackRequest::new(request, self.finished.clone())))
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
				publication: None,
				catalog,
				timebases: Default::default(),
				driver,
				finished: Default::default(),
				ended: false,
			})),
		})
	}

	/// Retain the native publication capability alongside the binding's fill handle.
	pub(crate) fn from_publication(
		publication: moq_net::origin::Publication,
		driver: Background,
	) -> Result<Self, MoqError> {
		let result = Self::from_inner(publication.broadcast().clone(), driver)?;
		result.with_state(|state| {
			state.publication = Some(publication);
			Ok(())
		})?;
		Ok(result)
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
	pub fn dynamic(&self) -> Result<Arc<MoqBroadcastDynamic>, MoqError> {
		let _guard = crate::ffi::enter();
		self.with_state(|state| {
			Ok(Arc::new(MoqBroadcastDynamic {
				task: Task::new(DynamicProducer {
					inner: state.broadcast.dynamic(),
					finished: state.finished.clone(),
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

	/// Advertise this broadcast's exact path as a route.
	///
	/// Until announced, the broadcast is invisible and unroutable for local
	/// consumers and peers alike. Announcing again re-prices the route in place.
	/// Errors with `Closed` on a standalone broadcast (no origin to announce on).
	pub fn announce(&self, route: crate::origin::MoqAnnounceRoute) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let route: moq_net::origin::Route = route.try_into()?;
		self.with_state(|state| {
			state.publication.as_ref().ok_or(MoqError::Closed)?.announce(route)?;
			Ok(())
		})
	}

	/// Retract this broadcast's exact-path advertisement, if any.
	///
	/// Local consumers and peers alike stop discovering and requesting it;
	/// tracks already in flight carry on. Announcing again brings it back. A no-op
	/// on a standalone broadcast.
	pub fn unannounce(&self) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		self.with_state(|state| {
			if let Some(publication) = &state.publication {
				publication.unannounce();
			}
			Ok(())
		})
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
			Ok(Arc::new(MoqTrackProducer {
				inner: std::sync::Mutex::new(Some(producer)),
				finished: std::sync::Mutex::new(Some(state.finished.clone())),
			}))
		})
	}

	/// End the broadcast for good: retract it, serve no new tracks, and finalize the catalog.
	///
	/// Tracks already subscribed carry on to their own end. Every later call on this
	/// producer fails with `Closed`; closing again is a no-op.
	pub fn close(&self) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		// Hold the lock through shutdown so a concurrent close() returns only once it is done.
		let mut guard = self.state.lock().unwrap();
		let Some(mut state) = guard.take() else {
			return Ok(());
		};
		// Close the broadcast first so it ends even if finalizing the catalog fails.
		state.broadcast.close();
		let result = match state.ended {
			true => Ok(()),
			false => state.catalog.finish().map_err(Into::into),
		};
		state.driver.check()?;
		result
	}
}

// ---- Dynamic Broadcast Producer ----

#[uniffi::export]
impl MoqBroadcastDynamic {
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

// ---- Dynamic Track Producer ----

#[uniffi::export]
impl MoqTrackDynamic {
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
	finished: Finished,
}

impl MoqTrackRequest {
	pub(crate) fn new(request: moq_net::track::Request, finished: Finished) -> Self {
		Self {
			inner: std::sync::Mutex::new(Some(request)),
			finished,
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
	pub fn dynamic(&self) -> Result<Arc<MoqTrackDynamic>, MoqError> {
		let _guard = crate::ffi::enter();
		let guard = self.inner.lock().unwrap();
		let request = guard.as_ref().ok_or(MoqError::Closed)?;
		Ok(Arc::new(MoqTrackDynamic::new(request.dynamic())))
	}

	/// Accept the request as a raw track, fixing its [`MoqTrackInfo`] (timescale, etc.).
	///
	/// For media use [`crate::media::MoqMediaTrackProducer::audio`] instead, which lets
	/// the importer pick the timescale.
	pub fn accept(&self, info: Option<MoqTrackInfo>) -> Result<Arc<MoqTrackProducer>, MoqError> {
		let _guard = crate::ffi::enter();
		let info = raw_track_info(info)?;
		let request = self.take()?;
		Ok(Arc::new(MoqTrackProducer {
			inner: std::sync::Mutex::new(Some(request.accept(Some(info)))),
			finished: std::sync::Mutex::new(Some(self.finished.clone())),
		}))
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
#[derive(uniffi::Object)]
pub struct MoqTrackProducer {
	inner: std::sync::Mutex<Option<moq_net::track::Producer>>,
	// Taken by the first finish, so repeating it keeps the track only once.
	finished: std::sync::Mutex<Option<Finished>>,
}

impl MoqTrackProducer {
	pub(crate) fn track_demand(&self) -> Result<moq_net::track::Demand, MoqError> {
		let guard = self.inner.lock().unwrap();
		let track = guard.as_ref().ok_or(MoqError::Closed)?;
		Ok(track.demand())
	}

	/// Hand the track to a typed writer that `f` builds (a JSON producer, say), closing this
	/// handle once `f` succeeds so the writer is the track's only producer. On failure the
	/// handle stays open.
	pub(crate) fn adopt<R>(
		&self,
		f: impl FnOnce(moq_net::track::Producer) -> Result<R, MoqError>,
	) -> Result<R, MoqError> {
		let mut guard = self.inner.lock().unwrap();
		let out = f(guard.as_ref().ok_or(MoqError::Closed)?.clone())?;
		guard.take();
		Ok(out)
	}
}

#[uniffi::export]
impl MoqTrackProducer {
	/// Create a handler for uncached group fetches on this track.
	///
	/// Hold the returned object for as long as cache misses should wait to be
	/// served. Without a live dynamic handler, a missing group fails with `NotFound`.
	pub fn dynamic(&self) -> Result<Arc<MoqTrackDynamic>, MoqError> {
		let _guard = crate::ffi::enter();
		let guard = self.inner.lock().unwrap();
		let track = guard.as_ref().ok_or(MoqError::Closed)?;
		Ok(Arc::new(MoqTrackDynamic::new(track.dynamic())))
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
		if track.end().is_none() {
			track.finish()?;
		}
		if let Some(finished) = self.finished.lock().unwrap().take() {
			finished.keep(track.clone());
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
		let finished = &state.as_ref().unwrap().finished;
		let kept = finished.0.lock().unwrap().as_ref().map(Vec::len);
		drop(state);
		assert_eq!(kept, Some(1));
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
