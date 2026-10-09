use std::sync::Arc;

use moq_mux::catalog::hang::Extra;

use crate::consumer::{MoqBroadcastConsumer, MoqGroupConsumer, MoqSubscription, MoqTrackConsumer};
use crate::demand::MoqTrackDemand;
use crate::error::MoqError;
use crate::ffi::Task;
use crate::media::MoqFrame;

/// Publisher-side track properties, mirroring [`moq_net::track::Info`].
///
/// Construct with the fields you care about; the rest use raw-track defaults
/// (priority 127, no publisher age limit, microsecond timescale).
#[derive(Clone, uniffi::Record)]
pub struct MoqTrackInfo {
	/// Priority, used only to break ties between subscriptions of equal subscriber priority.
	/// Higher is more urgent; the default 127 is the midpoint.
	#[uniffi(default = 127)]
	pub priority: u8,
	/// Maximum age of a non-latest group before the publisher evicts it, in
	/// microseconds. Null imposes no publisher age limit. This is the publisher-side half of
	/// [`MoqSubscription::max_delay_us`](crate::consumer::MoqSubscription::max_delay_us).
	#[uniffi(default = None)]
	pub max_age_us: Option<u64>,
	/// Per-frame timescale in ticks per second. Null uses microseconds when publishing, and
	/// means the source declared no timeline on a received track.
	#[uniffi(default = None)]
	pub timescale: Option<u64>,
}

impl TryFrom<MoqTrackInfo> for moq_net::track::Info {
	type Error = MoqError;

	fn try_from(info: MoqTrackInfo) -> Result<Self, MoqError> {
		let mut out = moq_net::track::Info::default()
			.with_timescale(moq_net::Timescale::MICRO)
			.with_priority(info.priority);
		if let Some(us) = info.max_age_us {
			out = out.with_max_age(std::time::Duration::from_micros(us));
		}
		if let Some(ticks) = info.timescale {
			let scale =
				moq_net::Timescale::new(ticks).map_err(|_| MoqError::Codec(format!("invalid timescale: {ticks}")))?;
			out = out.with_timescale(scale);
		}
		Ok(out)
	}
}

fn raw_track_info(info: Option<MoqTrackInfo>) -> Result<moq_net::track::Info, MoqError> {
	info.map(moq_net::track::Info::try_from)
		.transpose()
		.map(|info| info.unwrap_or_else(|| moq_net::track::Info::default().with_timescale(moq_net::Timescale::MICRO)))
}

impl TryFrom<&moq_net::track::Info> for MoqTrackInfo {
	type Error = MoqError;

	fn try_from(info: &moq_net::track::Info) -> Result<Self, MoqError> {
		let max_age_us = info
			.max_age
			.map(|age| u64::try_from(age.as_micros()))
			.transpose()
			.map_err(|_| MoqError::Codec("track max_age duration overflow".into()))?;
		Ok(Self {
			priority: info.priority,
			max_age_us,
			timescale: info.timescale.map(|timescale| timescale.as_u64()),
		})
	}
}

// ---- UniFFI Objects ----

pub(crate) struct BroadcastProducer {
	pub(crate) broadcast: moq_net::broadcast::Producer,
	// Carries the untyped `Extra` extension so callers can attach application catalog
	// sections by name (the only extension shape that crosses the FFI boundary).
	pub(crate) catalog: moq_mux::catalog::Producer<Extra>,
}

#[derive(uniffi::Object)]
pub struct MoqBroadcastProducer {
	pub(crate) state: std::sync::Mutex<Option<BroadcastProducer>>,
}

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
		Ok(Arc::new(MoqTrackRequest::new(request)))
	}
}

impl TrackDynamicProducer {
	async fn requested_group(&mut self) -> Result<Arc<MoqGroupRequest>, MoqError> {
		let request = self.inner.requested_group().await?;
		Ok(Arc::new(MoqGroupRequest::new(request)))
	}
}

impl MoqBroadcastProducer {
	/// Wrap a `moq_net::broadcast::Producer` (standalone or origin-created), attaching
	/// the catalog track every FFI broadcast carries.
	pub(crate) fn from_inner(mut broadcast: moq_net::broadcast::Producer) -> Result<Self, MoqError> {
		let config =
			moq_mux::catalog::Config::default().with_catalog(moq_mux::catalog::hang::Catalog::<Extra>::default());
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, config)?;
		Ok(Self {
			state: std::sync::Mutex::new(Some(BroadcastProducer { broadcast, catalog })),
		})
	}

	pub(crate) fn consume_inner(&self) -> Result<moq_net::broadcast::Consumer, MoqError> {
		let guard = self.state.lock().unwrap();
		let state = guard.as_ref().ok_or(MoqError::Closed)?;
		Ok(state.broadcast.consume())
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
		f(state)
	}
}

#[uniffi::export]
impl MoqBroadcastProducer {
	/// Create a consumer that reads from this broadcast's tracks.
	pub fn consume(&self) -> Result<Arc<MoqBroadcastConsumer>, MoqError> {
		let _guard = crate::ffi::enter();
		Ok(Arc::new(MoqBroadcastConsumer::new(self.consume_inner()?)))
	}

	/// Create a dynamic producer that yields tracks requested by subscribers.
	///
	/// Hold the returned object for as long as missing track requests should be
	/// accepted. Dropping it makes future subscriptions to unknown tracks fail.
	pub fn dynamic(&self) -> Result<Arc<MoqBroadcastDynamic>, MoqError> {
		let _guard = crate::ffi::enter();
		let guard = self.state.lock().unwrap();
		let state = guard.as_ref().ok_or(MoqError::Closed)?;
		Ok(Arc::new(MoqBroadcastDynamic {
			task: Task::new(DynamicProducer {
				inner: state.broadcast.dynamic(),
			}),
		}))
	}

	/// Create a standalone broadcast, not attached to any origin.
	///
	/// Use it to serve a dynamic broadcast request ([`MoqBroadcastRequest::accept`](crate::origin::MoqBroadcastRequest::accept))
	/// or for local pub/sub via [`consume`](Self::consume). To publish at a path, use
	/// [`MoqOriginProducer::create_broadcast`](crate::origin::MoqOriginProducer::create_broadcast) instead.
	#[uniffi::constructor]
	pub fn new() -> Result<Arc<Self>, MoqError> {
		let _guard = crate::ffi::enter();
		Ok(Arc::new(Self::from_inner(moq_net::broadcast::Info::new().produce())?))
	}

	/// Advertise this broadcast's exact path as a route.
	///
	/// Until announced, the broadcast is invisible and unroutable for local
	/// consumers and peers alike. Announcing again re-prices the route in place.
	/// Errors with `Closed` on a standalone broadcast (no origin to announce on).
	pub fn announce(&self, route: crate::origin::MoqRoute) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let route: moq_net::origin::Route = route.try_into()?;
		self.with_state(|state| {
			state.broadcast.announce(route)?;
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
			state.broadcast.unannounce();
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
		let guard = self.state.lock().unwrap();
		let state = guard.as_ref().ok_or(MoqError::Closed)?;
		let info = raw_track_info(info)?;
		// Clone the broadcast handle (shared Arc internally) to get &mut access.
		let broadcast = state.broadcast.clone();
		let producer = broadcast.create_track(name, Some(info))?;
		Ok(Arc::new(MoqTrackProducer {
			inner: std::sync::Mutex::new(Some(producer)),
		}))
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
		state.catalog.finish()?;
		Ok(())
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
	/// Wait for the next fetch of an uncached group.
	///
	/// Accept the returned request to produce the group, or abort it with an
	/// application error. Cached groups are served without reaching this method.
	pub async fn requested_group(&self) -> Result<Arc<MoqGroupRequest>, MoqError> {
		self.task
			.run(|mut state| async move { state.requested_group().await })
			.await
	}

	/// Cancel all current and future `requested_group()` calls.
	///
	/// Terminal: the dynamic track is released here, not when the handle is, so any pending
	/// fetch is rejected.
	pub fn cancel(&self) {
		self.task.cancel();
	}
}

/// An uncached group requested by a fetch consumer.
#[derive(uniffi::Object)]
pub struct MoqGroupRequest {
	sequence: u64,
	priority: u8,
	inner: std::sync::Mutex<Option<moq_net::group::Request>>,
}

impl MoqGroupRequest {
	fn new(request: moq_net::group::Request) -> Self {
		Self {
			sequence: request.sequence(),
			priority: request.priority(),
			inner: std::sync::Mutex::new(Some(request)),
		}
	}

	fn take(&self) -> Result<moq_net::group::Request, MoqError> {
		self.inner.lock().unwrap().take().ok_or(MoqError::Closed)
	}
}

#[uniffi::export]
impl MoqGroupRequest {
	/// The requested group sequence within the track.
	pub fn sequence(&self) -> u64 {
		self.sequence
	}

	/// The consumer's delivery priority for this fetch.
	pub fn priority(&self) -> u8 {
		self.priority
	}

	/// Accept the request and return a producer for filling the fetched group.
	pub fn accept(&self) -> Result<Arc<MoqGroupProducer>, MoqError> {
		let _guard = crate::ffi::enter();
		let group = self.take()?.accept(None)?;
		Ok(Arc::new(MoqGroupProducer {
			sequence: group.sequence,
			inner: std::sync::Mutex::new(Some(group)),
		}))
	}

	/// Reject the fetch with an application error code.
	pub fn abort(&self, error_code: u16) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		self.take()?.reject(moq_net::Error::App(error_code));
		Ok(())
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
}

impl MoqTrackRequest {
	pub(crate) fn new(request: moq_net::track::Request) -> Self {
		Self {
			inner: std::sync::Mutex::new(Some(request)),
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

#[derive(uniffi::Object)]
pub struct MoqTrackProducer {
	inner: std::sync::Mutex<Option<moq_net::track::Producer>>,
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
		let subscription = subscription.map(moq_net::track::Subscription::from);
		Ok(Arc::new(MoqTrackConsumer::new(track.subscribe(subscription))))
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
	/// Raw tracks default to a microsecond timescale. Custom timescales may round
	/// the timestamp during conversion. A frame without one is refused with
	/// [`MoqProtocolKind::TimestampMismatch`](crate::error::MoqProtocolKind::TimestampMismatch): raw
	/// tracks are timed.
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
	/// wire versions with a datagram channel; there is no stream fallback. Like a frame, a
	/// datagram without a timestamp is refused.
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
	/// that boundary. The handle remains so a later [`abort`](Self::abort) can still run.
	pub fn finish(&self) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let mut guard = self.inner.lock().unwrap();
		let track = guard.as_mut().ok_or(MoqError::Closed)?;
		if track.final_sequence().is_none() {
			track.finish()?;
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
		track.finish_at(final_sequence)?;
		Ok(())
	}
}

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
		Ok(Arc::new(MoqGroupConsumer::new(group.consume())))
	}

	/// Write `frame` into this group.
	///
	/// Raw tracks default to a microsecond timescale. Custom timescales may round
	/// the timestamp during conversion. A frame without one is refused with
	/// [`MoqProtocolKind::TimestampMismatch`](crate::error::MoqProtocolKind::TimestampMismatch): raw
	/// tracks are timed.
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
	/// Wait until a consumer has this group.
	pub(crate) async fn used(&self) -> Result<(), MoqError> {
		let producer = self.inner.lock().unwrap().as_ref().ok_or(MoqError::Closed)?.clone();
		Ok(producer.demand().used().await?)
	}
}

#[cfg(test)]
mod metadata_tests {
	use super::*;

	#[test]
	fn optional_retention_survives_binding_conversion() {
		for max_age_us in [None, Some(0), Some(30_000_000)] {
			let info = MoqTrackInfo {
				priority: 0,
				max_age_us,
				timescale: None,
			};
			let model = moq_net::track::Info::try_from(info).unwrap();
			assert_eq!(model.max_age, max_age_us.map(std::time::Duration::from_micros));
			assert_eq!(MoqTrackInfo::try_from(&model).unwrap().max_age_us, max_age_us);
		}
	}
}
