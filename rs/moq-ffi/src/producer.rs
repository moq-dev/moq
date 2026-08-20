use std::sync::Arc;

use moq_mux::catalog::hang::Extra;

use crate::consumer::{MoqBroadcastConsumer, MoqGroupConsumer, MoqSubscription, MoqTrackConsumer};
use crate::error::MoqError;
use crate::ffi::Task;
use crate::media::{MoqAudioInit, MoqContainerFormat, MoqContainerInit, MoqFrame, MoqVideoInit, MoqVideoProperties};
use crate::origin::MoqRoute;

/// Publisher-side track properties, mirroring [`moq_net::track::Info`].
///
/// Construct with the fields you care about; the rest use raw-track defaults
/// (priority 0, unordered, the publisher's default max age, microsecond timescale).
#[derive(Clone, uniffi::Record)]
pub struct MoqTrackInfo {
	/// Priority, used only to break ties between subscriptions of equal subscriber priority.
	#[uniffi(default = 0)]
	pub priority: u8,
	/// Whether groups are prioritized in sequence order. Groups may always arrive
	/// out-of-order (or not at all) over the network. Defaults to false.
	#[uniffi(default = false)]
	pub ordered: bool,
	/// Maximum age of a non-latest group before the publisher evicts it, in
	/// milliseconds. Null uses the default. This is the publisher-side half of
	/// [`MoqSubscription::max_age_ms`](crate::consumer::MoqSubscription::max_age_ms).
	#[uniffi(default = None)]
	pub max_age_ms: Option<u64>,
	/// Per-frame timescale in ticks per second. Null uses microseconds.
	#[uniffi(default = None)]
	pub timescale: Option<u64>,
}

impl TryFrom<MoqTrackInfo> for moq_net::track::Info {
	type Error = MoqError;

	fn try_from(info: MoqTrackInfo) -> Result<Self, MoqError> {
		let mut out = moq_net::track::Info::default()
			.with_timescale(moq_net::Timescale::MICRO)
			.with_priority(info.priority)
			.with_ordered(info.ordered);
		if let Some(ms) = info.max_age_ms {
			out = out.with_max_age(std::time::Duration::from_millis(ms));
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
		let max_age_ms = u64::try_from(info.max_age.as_millis())
			.map_err(|_| MoqError::Codec("track max_age duration overflow".into()))?;
		Ok(Self {
			priority: info.priority,
			ordered: info.ordered,
			max_age_ms: Some(max_age_ms),
			timescale: Some(info.timescale.as_u64()),
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

/// A whole-frame importer for one codec track.
///
/// Separate from [`ContainerProducer`] because a container publishes several tracks and takes
/// chunks rather than timestamped frames. Sharing one type meant a `write_frame` timestamp that one
/// arm silently dropped.
struct MediaProducer {
	// Boxed because the codec splitters/imports make this much larger than the container one.
	import: Box<moq_mux::import::Track<Extra>>,
	/// Subscriber demand (name/used/unused) for the one track this publishes.
	demand: moq_net::track::Demand,
}

/// A whole-chunk importer for a container, which may publish several tracks.
struct ContainerProducer {
	import: moq_mux::import::Container<Extra>,
}

/// A byte-stream importer for one codec track, where frame boundaries are inferred.
struct MediaStreamProducer {
	import: Box<moq_mux::import::TrackStream<Extra>>,
}

/// A byte-stream importer for a container, which recovers its own framing.
struct ContainerStreamProducer {
	import: moq_mux::import::ContainerStream<Extra>,
}

#[derive(uniffi::Object)]
pub struct MoqBroadcastProducer {
	state: std::sync::Mutex<Option<BroadcastProducer>>,
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
		let catalog =
			moq_mux::catalog::Producer::with_catalog(&mut broadcast, moq_mux::catalog::hang::Catalog::default())?;
		Ok(Self {
			state: std::sync::Mutex::new(Some(BroadcastProducer { broadcast, catalog })),
		})
	}

	pub(crate) fn consume_inner(&self) -> Result<moq_net::broadcast::Consumer, MoqError> {
		let guard = self.state.lock().unwrap();
		let state = guard.as_ref().ok_or_else(|| MoqError::Closed)?;
		Ok(state.broadcast.consume())
	}

	/// Run `f` against the open broadcast and catalog. Errors with
	/// [`MoqError::Closed`] if `finish()` has already run. Used by
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

#[derive(uniffi::Object)]
pub struct MoqContainerProducer {
	inner: std::sync::Mutex<Option<ContainerProducer>>,
}

#[derive(uniffi::Object)]
pub struct MoqContainerStreamProducer {
	inner: std::sync::Mutex<Option<ContainerStreamProducer>>,
}

#[derive(uniffi::Object)]
pub struct MoqMediaProducer {
	inner: std::sync::Mutex<Option<MediaProducer>>,
}

#[derive(uniffi::Object)]
pub struct MoqMediaStreamProducer {
	inner: std::sync::Mutex<Option<MediaStreamProducer>>,
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
		let state = guard.as_ref().ok_or_else(|| MoqError::Closed)?;
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

	/// Update the broadcast's route: the hop chain, cost, and announce flag it advertises.
	///
	/// Use this as conditions shift (e.g. a standby transcoder lowering its cost
	/// once it is warm); consumers observe the change via
	/// `MoqBroadcastConsumer::route_updates` and sessions forward it downstream.
	pub fn set_route(&self, route: MoqRoute) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let route = route.try_into()?;
		self.with_state(|state| Ok(state.broadcast.set_route(route)?))
	}

	/// Set whether the broadcast is announced, keeping the rest of its route (hops, cost).
	///
	/// The origin advertises the path only while announced; an unannounced
	/// broadcast stays reachable by exact path for subscribes and fetches. This is
	/// how a publisher goes on and off the air without tearing down the broadcast.
	pub fn set_announce(&self, announce: bool) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		self.with_state(|state| {
			let route = state.broadcast.consume().route();
			Ok(state.broadcast.set_route(route.with_announce(announce))?)
		})
	}

	/// Replace the catalog properties shared by every video rendition.
	///
	/// Rotation is clockwise and normalized to the nearest quarter turn. An absent field is removed from the next catalog update.
	pub fn set_video_properties(&self, properties: MoqVideoProperties) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let mut value = hang::catalog::VideoProperties::default();
		value.display = properties.display.map(|display| hang::catalog::Display {
			width: display.width,
			height: display.height,
		});
		value.rotation = properties.rotation;
		value.flip = properties.flip;

		self.with_state(|state| {
			let mut catalog = state.catalog.lock();
			catalog.video.set_properties(value)?;
			catalog.commit()?;
			Ok(())
		})
	}

	/// Set (or replace) a top-level application catalog section by name.
	///
	/// `json` is any JSON document (object, array, string, ...) serialized as a UTF-8 string.
	/// Errors with [`MoqError::Json`] if `json` doesn't parse, or with the reserved-section
	/// error if `name` is `video`/`audio` (owned by the media pipeline). The section is
	/// republished on the catalog track immediately.
	pub fn set_catalog_section(&self, name: String, json: String) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let value: serde_json::Value = serde_json::from_str(&json)?;
		self.with_state(|state| Ok(state.catalog.lock().set_section(name, value)?))
	}

	/// Remove a top-level application catalog section by name.
	///
	/// Republishes the catalog if the section existed; a no-op otherwise.
	pub fn remove_catalog_section(&self, name: String) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		self.with_state(|state| {
			state.catalog.lock().remove_section(&name);
			Ok(())
		})
	}

	/// Publish one audio codec as a new track.
	///
	/// The track is named after the format (`0.opus`), so the catalog is how a subscriber finds it.
	/// [`MoqAudioInit::data`] is required: audio resolves its rendition entirely from those bytes.
	pub fn publish_audio(&self, init: MoqAudioInit) -> Result<Arc<MoqMediaProducer>, MoqError> {
		let _guard = crate::ffi::enter();
		let guard = self.state.lock().unwrap();
		let state = guard.as_ref().ok_or_else(|| MoqError::Closed)?;

		let init: moq_mux::import::AudioInit = init.into();
		let mut broadcast = state.broadcast.clone();
		let name = broadcast.unique_name(&format!(".{}", init.format));
		let request = broadcast
			.reserve_track(name)
			.map_err(|err| MoqError::Codec(format!("init failed: {err}")))?;

		let import = moq_mux::import::Track::audio(request, state.catalog.reserve(), init)
			.map_err(|err| MoqError::Codec(format!("init failed: {err}")))?;
		Ok(MoqMediaProducer::new(import))
	}

	/// Publish one video codec as a new track.
	///
	/// Named as in [`publish_audio`](Self::publish_audio). [`MoqVideoInit::data`] may be empty for a
	/// format that resolves in band; a hint carrying the codec publishes the catalog before the
	/// first keyframe.
	pub fn publish_video(&self, init: MoqVideoInit) -> Result<Arc<MoqMediaProducer>, MoqError> {
		let _guard = crate::ffi::enter();
		let guard = self.state.lock().unwrap();
		let state = guard.as_ref().ok_or_else(|| MoqError::Closed)?;

		let init: moq_mux::import::VideoInit = init.into();
		let mut broadcast = state.broadcast.clone();
		let name = broadcast.unique_name(&format!(".{}", init.format));
		let request = broadcast
			.reserve_track(name)
			.map_err(|err| MoqError::Codec(format!("init failed: {err}")))?;

		let import = moq_mux::import::Track::video(request, state.catalog.reserve(), init)
			.map_err(|err| MoqError::Codec(format!("init failed: {err}")))?;
		Ok(MoqMediaProducer::new(import))
	}

	/// Publish a container, which demuxes and publishes its own tracks.
	///
	/// Unlike the codec entry points there is no label or hint: a container describes each track it
	/// publishes from its own metadata, so a rendition field would have no single track to land on.
	pub fn publish_container(&self, init: MoqContainerInit) -> Result<Arc<MoqContainerProducer>, MoqError> {
		let _guard = crate::ffi::enter();
		let guard = self.state.lock().unwrap();
		let state = guard.as_ref().ok_or_else(|| MoqError::Closed)?;

		let init: moq_mux::import::ContainerInit = init.into();
		let import = moq_mux::import::Container::new(state.broadcast.clone(), state.catalog.reserve(), &init)
			.map_err(|err| MoqError::Codec(format!("init failed: {err}")))?;

		Ok(Arc::new(MoqContainerProducer {
			inner: std::sync::Mutex::new(Some(ContainerProducer { import })),
		}))
	}

	/// Publish one audio codec onto a track requested through
	/// [`MoqBroadcastDynamic::requested_track`], which the importer accepts.
	pub fn publish_audio_on_track(
		&self,
		request: &MoqTrackRequest,
		init: MoqAudioInit,
	) -> Result<Arc<MoqMediaProducer>, MoqError> {
		let _guard = crate::ffi::enter();
		let guard = self.state.lock().unwrap();
		let state = guard.as_ref().ok_or_else(|| MoqError::Closed)?;

		let request = request.take()?;
		let import = moq_mux::import::Track::audio(request, state.catalog.reserve(), init.into())
			.map_err(|err| MoqError::Codec(format!("init failed: {err}")))?;
		Ok(MoqMediaProducer::new(import))
	}

	/// Publish one video codec onto a requested track. See
	/// [`publish_audio_on_track`](Self::publish_audio_on_track).
	pub fn publish_video_on_track(
		&self,
		request: &MoqTrackRequest,
		init: MoqVideoInit,
	) -> Result<Arc<MoqMediaProducer>, MoqError> {
		let _guard = crate::ffi::enter();
		let guard = self.state.lock().unwrap();
		let state = guard.as_ref().ok_or_else(|| MoqError::Closed)?;

		let request = request.take()?;
		let import = moq_mux::import::Track::video(request, state.catalog.reserve(), init.into())
			.map_err(|err| MoqError::Codec(format!("init failed: {err}")))?;
		Ok(MoqMediaProducer::new(import))
	}

	/// Publish one video codec fed by a raw byte stream, inferring frame boundaries.
	///
	/// Only the self-delimiting formats work here (`Avc3`, `Hev1`, `Av01`); the rest need length
	/// prefixes or an out-of-band config record. There is no audio counterpart for the same reason.
	pub fn publish_video_stream(&self, init: MoqVideoInit) -> Result<Arc<MoqMediaStreamProducer>, MoqError> {
		let _guard = crate::ffi::enter();
		let guard = self.state.lock().unwrap();
		let state = guard.as_ref().ok_or_else(|| MoqError::Closed)?;

		let init: moq_mux::import::VideoInit = init.into();
		let mut broadcast = state.broadcast.clone();
		let name = broadcast.unique_name(&format!(".{}", init.format));
		let request = broadcast
			.reserve_track(name)
			.map_err(|err| MoqError::Codec(format!("init failed: {err}")))?;

		let import = moq_mux::import::TrackStream::video(request, state.catalog.reserve(), init)
			.map_err(|err| MoqError::Codec(format!("init failed: {err}")))?;

		Ok(Arc::new(MoqMediaStreamProducer {
			inner: std::sync::Mutex::new(Some(MediaStreamProducer {
				import: Box::new(import),
			})),
		}))
	}

	/// Publish a container fed by a raw byte stream, which recovers its own framing.
	pub fn publish_container_stream(
		&self,
		format: MoqContainerFormat,
	) -> Result<Arc<MoqContainerStreamProducer>, MoqError> {
		let _guard = crate::ffi::enter();
		let guard = self.state.lock().unwrap();
		let state = guard.as_ref().ok_or_else(|| MoqError::Closed)?;

		let import =
			moq_mux::import::ContainerStream::new(state.broadcast.clone(), state.catalog.reserve(), format.into())
				.map_err(|err| MoqError::Codec(format!("init failed: {err}")))?;

		Ok(Arc::new(MoqContainerStreamProducer {
			inner: std::sync::Mutex::new(Some(ContainerStreamProducer { import })),
		}))
	}

	/// Create a track for arbitrary byte payloads, no codec or container.
	///
	/// Same pattern as moq-boy's `status` and `command` tracks: raw UTF-8/JSON
	/// bytes written directly to moq-lite groups with no media framing. `info` sets
	/// track properties (priority, max age, timescale); omit for defaults.
	pub fn publish_track(&self, name: String, info: Option<MoqTrackInfo>) -> Result<Arc<MoqTrackProducer>, MoqError> {
		let _guard = crate::ffi::enter();
		let guard = self.state.lock().unwrap();
		let state = guard.as_ref().ok_or_else(|| MoqError::Closed)?;
		let info = raw_track_info(info)?;
		// Clone the broadcast handle (shared Arc internally) to get &mut access.
		let mut broadcast = state.broadcast.clone();
		let producer = broadcast.create_track(name, Some(info))?;
		Ok(Arc::new(MoqTrackProducer {
			inner: std::sync::Mutex::new(Some(producer)),
		}))
	}

	/// Finish this publisher, finalizing the catalog stream and cleanly closing the
	/// broadcast so subscribers see a normal end rather than `Error::Dropped`.
	pub fn finish(&self) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let mut guard = self.state.lock().unwrap();
		let mut state = guard.take().ok_or(MoqError::Closed)?;
		// Finish the broadcast first so the clean end reaches subscribers even if
		// finalizing the catalog fails.
		state.broadcast.finish();
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
	/// [`MoqBroadcastProducer::publish_audio_on_track`], or reject it with
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
	inner: std::sync::Mutex<Option<moq_net::track::GroupRequest>>,
}

impl MoqGroupRequest {
	fn new(request: moq_net::track::GroupRequest) -> Self {
		Self {
			sequence: request.sequence(),
			priority: request.priority(),
			inner: std::sync::Mutex::new(Some(request)),
		}
	}

	fn take(&self) -> Result<moq_net::track::GroupRequest, MoqError> {
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
/// frames, hand it to [`MoqBroadcastProducer::publish_audio_on_track`] to publish media,
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
	/// [`MoqBroadcastProducer::publish_audio_on_track`].
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
	/// For media use [`MoqBroadcastProducer::publish_audio_on_track`] instead, which lets
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

#[uniffi::export]
impl MoqTrackProducer {
	/// Return the name of this track.
	pub fn name(&self) -> Result<String, MoqError> {
		let _guard = crate::ffi::enter();
		let guard = self.inner.lock().unwrap();
		let track = guard.as_ref().ok_or(MoqError::Closed)?;
		Ok(track.name().to_string())
	}

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

	/// Wait until this track has at least one active consumer.
	pub async fn used(&self) -> Result<(), MoqError> {
		let track = self.inner.lock().unwrap().as_ref().ok_or(MoqError::Closed)?.clone();
		crate::ffi::detached(async move { track.used().await }).await
	}

	/// Wait until this track has no active consumers.
	pub async fn unused(&self) -> Result<(), MoqError> {
		let track = self.inner.lock().unwrap().as_ref().ok_or(MoqError::Closed)?.clone();
		crate::ffi::detached(async move { track.unused().await }).await
	}

	/// Create a consumer that reads from this producer's track.
	///
	/// Useful for local pub/sub without going through an origin/broadcast. `subscription`
	/// tunes delivery priority, group ordering priority, and group range; omit for defaults.
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
	/// the timestamp during conversion.
	pub fn write_frame(&self, frame: MoqFrame) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let timestamp = moq_net::Timestamp::from_micros(frame.timestamp_us)?;
		let mut guard = self.inner.lock().unwrap();
		let track = guard.as_mut().ok_or(MoqError::Closed)?;
		track.write_frame(timestamp, frame.payload)?;
		Ok(())
	}

	/// Send `frame` as a best-effort datagram, returning the sequence number assigned to it.
	///
	/// The payload must be at most 1200 bytes. Datagrams are only delivered on transports and
	/// wire versions with a datagram channel; there is no stream fallback.
	pub fn append_datagram(&self, frame: MoqFrame) -> Result<u64, MoqError> {
		let _guard = crate::ffi::enter();
		let timestamp = moq_net::Timestamp::from_micros(frame.timestamp_us)?;
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

	/// Release this producer, ending the track at the live edge.
	///
	/// [`finish_at`](Self::finish_at) declares the boundary ahead of time, so this keeps
	/// that boundary and only releases the producer.
	pub fn finish(&self) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let mut guard = self.inner.lock().unwrap();
		let mut track = guard.take().ok_or(MoqError::Closed)?;
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
		let group = guard.as_ref().ok_or_else(|| MoqError::Closed)?;
		Ok(Arc::new(MoqGroupConsumer::new(group.consume())))
	}

	/// Write `frame` into this group.
	///
	/// Raw tracks default to a microsecond timescale. Custom timescales may round
	/// the timestamp during conversion.
	pub fn write_frame(&self, frame: MoqFrame) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let timestamp = moq_net::Timestamp::from_micros(frame.timestamp_us)?;
		let mut guard = self.inner.lock().unwrap();
		let group = guard.as_mut().ok_or_else(|| MoqError::Closed)?;
		group.write_frame(timestamp, frame.payload)?;
		Ok(())
	}

	/// Mark the group as complete. No more frames can be written.
	pub fn finish(&self) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let mut guard = self.inner.lock().unwrap();
		let mut group = guard.take().ok_or_else(|| MoqError::Closed)?;
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

// ---- Media Producer ----

impl MoqMediaProducer {
	/// Wrap a single-codec importer, capturing the demand handle its track exposes.
	fn new(import: moq_mux::import::Track<Extra>) -> Arc<Self> {
		let demand = import.demand();
		Arc::new(Self {
			inner: std::sync::Mutex::new(Some(MediaProducer {
				import: Box::new(import),
				demand,
			})),
		})
	}
}

#[uniffi::export]
impl MoqMediaProducer {
	/// The name of the track this publishes.
	pub fn name(&self) -> Result<String, MoqError> {
		let _guard = crate::ffi::enter();
		let guard = self.inner.lock().unwrap();
		let media = guard.as_ref().ok_or_else(|| MoqError::Closed)?;
		Ok(media.demand.name().to_string())
	}

	/// Wait until this track has at least one active consumer.
	pub async fn used(&self) -> Result<(), MoqError> {
		let demand = self
			.inner
			.lock()
			.unwrap()
			.as_ref()
			.ok_or(MoqError::Closed)?
			.demand
			.clone();
		crate::ffi::detached(async move { demand.used().await }).await
	}

	/// Wait until this track has no active consumers.
	pub async fn unused(&self) -> Result<(), MoqError> {
		let demand = self
			.inner
			.lock()
			.unwrap()
			.as_ref()
			.ok_or(MoqError::Closed)?
			.demand
			.clone();
		crate::ffi::detached(async move { demand.unused().await }).await
	}

	/// Write `frame` to this track.
	///
	/// The importer derives keyframe status from the bitstream, so a [`MoqFrame`] carries only the
	/// payload and its timestamp.
	pub fn write_frame(&self, frame: MoqFrame) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let mut guard = self.inner.lock().unwrap();
		let media = guard.as_mut().ok_or_else(|| MoqError::Closed)?;

		let timestamp = hang::container::Timestamp::from_micros(frame.timestamp_us)?;
		media
			.import
			.decode(&frame.payload, Some(timestamp))
			.map_err(|err| MoqError::Codec(format!("decode failed: {err}")))?;

		Ok(())
	}

	/// Draw a group boundary here.
	///
	/// Audio has no boundary of its own (every packet is independently decodable), so this is the
	/// only thing that gives it groups: call it after every frame for one group (one QUIC stream)
	/// the relay forwards without waiting, or at a segment cadence to align with video for
	/// HLS/DASH. Video groups at its own keyframes and needs this only to override that.
	pub fn cut(&self) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let mut guard = self.inner.lock().unwrap();
		let media = guard.as_mut().ok_or(MoqError::Closed)?;
		media
			.import
			.cut(None)
			.map_err(|err| MoqError::Codec(format!("cut failed: {err}")))?;
		Ok(())
	}

	/// Draw a group boundary and number the next group `sequence`.
	///
	/// [`cut`](Self::cut) with an explicit sequence, for a publisher whose group numbers have to
	/// be deterministic: two encoders aligning per GOP so a consumer can fail over between them.
	pub fn seek(&self, sequence: u64) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let mut guard = self.inner.lock().unwrap();
		let media = guard.as_mut().ok_or(MoqError::Closed)?;
		media
			.import
			.seek(sequence)
			.map_err(|err| MoqError::Codec(format!("seek failed: {err}")))?;
		Ok(())
	}

	/// Finish this track and finalize encoding.
	pub fn finish(&self) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let mut guard = self.inner.lock().unwrap();
		let mut media = guard.take().ok_or_else(|| MoqError::Closed)?;
		media
			.import
			.finish()
			.map_err(|err| MoqError::Codec(format!("finish failed: {err}")))?;
		Ok(())
	}
}

#[uniffi::export]
impl MoqContainerProducer {
	/// Write a whole chunk of the container.
	///
	/// No timestamp: a container carries its tracks' timing itself, and the importer reads it out
	/// rather than taking the caller's word for it.
	pub fn write(&self, payload: Vec<u8>) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let mut guard = self.inner.lock().unwrap();
		let container = guard.as_mut().ok_or_else(|| MoqError::Closed)?;
		container
			.import
			.decode(&payload)
			.map_err(|err| MoqError::Codec(format!("decode failed: {err}")))?;
		Ok(())
	}

	/// Declare that the next chunk starts a new segment, rolling a group on every track this
	/// publishes.
	///
	/// For a caller that knows its source's segmentation out of band. An fMP4 source carrying
	/// `styp` atoms declares its own, so this is only needed when it doesn't, and formats with no
	/// segment concept (MKV, TS, FLV) ignore it.
	pub fn cut(&self) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let mut guard = self.inner.lock().unwrap();
		let container = guard.as_mut().ok_or(MoqError::Closed)?;
		container.import.cut();
		Ok(())
	}

	/// Start a new segment and number its groups `sequence`.
	pub fn seek(&self, sequence: u64) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let mut guard = self.inner.lock().unwrap();
		let container = guard.as_mut().ok_or(MoqError::Closed)?;
		container
			.import
			.seek(sequence)
			.map_err(|err| MoqError::Codec(format!("seek failed: {err}")))?;
		Ok(())
	}

	/// Finish every track this container publishes.
	pub fn finish(&self) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let mut guard = self.inner.lock().unwrap();
		let mut container = guard.take().ok_or_else(|| MoqError::Closed)?;
		container
			.import
			.finish()
			.map_err(|err| MoqError::Codec(format!("finish failed: {err}")))?;
		Ok(())
	}
}

#[uniffi::export]
impl MoqMediaStreamProducer {
	/// Push raw stream bytes (e.g. Annex-B H.264 from an encoder). The importer frames whole access
	/// units and keeps any partial trailing frame for the next call, so callers can write arbitrary
	/// chunks.
	pub fn write(&self, payload: Vec<u8>) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let mut guard = self.inner.lock().unwrap();
		let media = guard.as_mut().ok_or_else(|| MoqError::Closed)?;
		media
			.import
			.decode(&payload)
			.map_err(|err| MoqError::Codec(format!("decode failed: {err}")))?;
		Ok(())
	}

	/// Finalize the track.
	///
	/// The importer emits each access unit when the *next* one's start code arrives, so a trailing
	/// access unit with no following delimiter (e.g. the last frame at EOF) is not emitted. This
	/// matches moq-cli's stdin path.
	pub fn finish(&self) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let mut guard = self.inner.lock().unwrap();
		let mut media = guard.take().ok_or_else(|| MoqError::Closed)?;
		media
			.import
			.finish()
			.map_err(|err| MoqError::Codec(format!("finish failed: {err}")))?;
		Ok(())
	}
}

#[uniffi::export]
impl MoqContainerStreamProducer {
	/// Push raw container bytes. The importer recovers its own framing, so callers can write
	/// arbitrary chunks.
	pub fn write(&self, payload: Vec<u8>) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let mut guard = self.inner.lock().unwrap();
		let container = guard.as_mut().ok_or_else(|| MoqError::Closed)?;
		container
			.import
			.decode(&payload)
			.map_err(|err| MoqError::Codec(format!("decode failed: {err}")))?;
		Ok(())
	}

	/// Finish every track this container publishes.
	pub fn finish(&self) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let mut guard = self.inner.lock().unwrap();
		let mut container = guard.take().ok_or_else(|| MoqError::Closed)?;
		container
			.import
			.finish()
			.map_err(|err| MoqError::Codec(format!("finish failed: {err}")))?;
		Ok(())
	}
}
