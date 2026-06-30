use std::sync::Arc;

use moq_mux::catalog::hang::Extra;

use crate::consumer::{MoqBroadcastConsumer, MoqGroupConsumer, MoqSubscription, MoqTrackConsumer};
use crate::error::MoqError;
use crate::ffi::Task;

/// Publisher-side track properties, mirroring [`moq_net::TrackInfo`].
///
/// Construct with the fields you care about; the rest default to moq-net's defaults
/// (priority 0, ordered, default cache, millisecond timescale).
#[derive(Clone, uniffi::Record)]
pub struct MoqTrackInfo {
	/// Priority, used only to break ties between subscriptions of equal subscriber priority.
	#[uniffi(default = 0)]
	pub priority: u8,
	/// Whether groups are delivered in sequence order.
	#[uniffi(default = true)]
	pub ordered: bool,
	/// How long the relay should cache past groups, in milliseconds. Null uses the default.
	#[uniffi(default = None)]
	pub cache_ms: Option<u64>,
	/// Per-frame timescale in ticks per second. Null uses the default (milliseconds);
	/// set it to match the scale of the timestamps you write. Frames written without an
	/// explicit timestamp are stamped with wall-clock time at this scale.
	#[uniffi(default = None)]
	pub timescale: Option<u64>,
}

impl TryFrom<MoqTrackInfo> for moq_net::TrackInfo {
	type Error = MoqError;

	fn try_from(info: MoqTrackInfo) -> Result<Self, MoqError> {
		let mut out = moq_net::TrackInfo::default()
			.with_priority(info.priority)
			.with_ordered(info.ordered);
		if let Some(ms) = info.cache_ms {
			out = out.with_cache(std::time::Duration::from_millis(ms));
		}
		if let Some(ticks) = info.timescale {
			let scale =
				moq_net::Timescale::new(ticks).map_err(|_| MoqError::Codec(format!("invalid timescale: {ticks}")))?;
			out = out.with_timescale(scale);
		}
		Ok(out)
	}
}

// ---- UniFFI Objects ----

pub(crate) struct BroadcastProducer {
	pub(crate) broadcast: moq_net::BroadcastProducer,
	// Carries the untyped `Extra` extension so callers can attach application catalog
	// sections by name (the only extension shape that crosses the FFI boundary).
	pub(crate) catalog: moq_mux::catalog::Producer<Extra>,
}

/// A whole-frame importer: a single codec track, or a container that may publish
/// several tracks. The format string picks which when the producer is created.
enum MediaDecoder {
	// Boxed because the codec splitters/imports make this variant much larger than
	// the (already boxed) container one.
	Track(Box<moq_mux::import::Track<Extra>>),
	Container(moq_mux::import::Container<Extra>),
}

impl MediaDecoder {
	fn decode(&mut self, frame: &[u8], pts: Option<hang::container::Timestamp>) -> moq_mux::Result<()> {
		match self {
			Self::Track(t) => t.decode(frame, pts),
			Self::Container(c) => c.decode(frame),
		}
	}

	fn finish(&mut self) -> moq_mux::Result<()> {
		match self {
			Self::Track(t) => t.finish(),
			Self::Container(c) => c.finish(),
		}
	}
}

struct MediaProducer {
	decoder: MediaDecoder,
	/// `Some` for a single codec track, whose subscriber demand (name/used/unused)
	/// is observable; `None` for a container that may publish several tracks.
	demand: Option<moq_net::TrackDemand>,
}

/// A byte-stream importer: a single codec track or a container that may publish
/// several tracks. The format string picks which when the producer is created.
enum StreamDecoder {
	// Boxed because the codec splitter/import make this variant much larger than
	// the (already boxed) container one.
	Track(Box<moq_mux::import::TrackStream<Extra>>),
	Container(moq_mux::import::ContainerStream<Extra>),
}

struct MediaStreamProducer {
	// The importer buffers any partial trailing frame internally, so callers can
	// write arbitrary chunks without retaining a remainder here.
	decoder: StreamDecoder,
}

#[derive(uniffi::Object)]
pub struct MoqBroadcastProducer {
	state: std::sync::Mutex<Option<BroadcastProducer>>,
}

#[derive(uniffi::Object)]
pub struct MoqBroadcastDynamic {
	task: Task<DynamicProducer>,
}

struct DynamicProducer {
	inner: moq_net::BroadcastDynamic,
}

impl DynamicProducer {
	async fn requested_track(&mut self) -> Result<Arc<MoqTrackRequest>, MoqError> {
		// Hand back the un-accepted request, mirroring `moq_net::BroadcastDynamic`: the caller
		// accepts it (raw, at a chosen timescale) or publishes media onto it (importer accepts).
		// The subscriber's subscribe stays pending until then.
		let request = self.inner.requested_track().await?;
		Ok(Arc::new(MoqTrackRequest::new(request)))
	}
}

impl MoqBroadcastProducer {
	pub(crate) fn consume_inner(&self) -> Result<moq_net::BroadcastConsumer, MoqError> {
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
		let _guard = crate::ffi::RUNTIME.enter();
		Ok(Arc::new(MoqBroadcastConsumer::new(self.consume_inner()?)))
	}

	/// Create a dynamic producer that yields tracks requested by subscribers.
	///
	/// Hold the returned object for as long as missing track requests should be
	/// accepted. Dropping it makes future subscriptions to unknown tracks fail.
	pub fn dynamic(&self) -> Result<Arc<MoqBroadcastDynamic>, MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let guard = self.state.lock().unwrap();
		let state = guard.as_ref().ok_or_else(|| MoqError::Closed)?;
		Ok(Arc::new(MoqBroadcastDynamic {
			task: Task::new(DynamicProducer {
				inner: state.broadcast.dynamic(),
			}),
		}))
	}

	/// Create a new broadcast for publishing media tracks.
	///
	/// NOTE: This will do nothing until published to an origin.
	#[uniffi::constructor]
	pub fn new() -> Result<Arc<Self>, MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let mut broadcast = moq_net::BroadcastInfo::new().produce();
		let catalog = moq_mux::catalog::Producer::new_extra(&mut broadcast)?;
		Ok(Arc::new(Self {
			state: std::sync::Mutex::new(Some(BroadcastProducer { broadcast, catalog })),
		}))
	}

	/// Set (or replace) a top-level application catalog section by name.
	///
	/// `json` is any JSON document (object, array, string, ...) serialized as a UTF-8 string.
	/// Errors with [`MoqError::Json`] if `json` doesn't parse, or with the reserved-section
	/// error if `name` is `video`/`audio` (owned by the media pipeline). The section is
	/// republished on the catalog track immediately.
	pub fn set_catalog_section(&self, name: String, json: String) -> Result<(), MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let value: serde_json::Value = serde_json::from_str(&json)?;
		self.with_state(|state| Ok(state.catalog.set_section(name, value)?))
	}

	/// Remove a top-level application catalog section by name.
	///
	/// Republishes the catalog if the section existed; a no-op otherwise.
	pub fn remove_catalog_section(&self, name: String) -> Result<(), MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		self.with_state(|state| {
			state.catalog.remove_section(&name);
			Ok(())
		})
	}

	/// Create a new media track for this broadcast.
	///
	/// `format` controls the encoding of `init` and frame payloads.
	pub fn publish_media(&self, format: String, init: Vec<u8>) -> Result<Arc<MoqMediaProducer>, MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let guard = self.state.lock().unwrap();
		let state = guard.as_ref().ok_or_else(|| MoqError::Closed)?;

		// A container may publish several tracks; a single codec fills one reserved
		// track. Try the container first so a codec format doesn't reserve a stray
		// track on the way to being recognized.
		let (decoder, demand) =
			match moq_mux::import::Container::new(state.broadcast.clone(), state.catalog.clone(), &format, &init) {
				Ok(container) => (MediaDecoder::Container(container), None),
				Err(moq_mux::Error::UnknownFormat(_)) => {
					let mut broadcast = state.broadcast.clone();
					let name = broadcast.unique_name(&format!(".{format}"));
					let request = broadcast
						.reserve_track(name)
						.map_err(|err| MoqError::Codec(format!("init failed: {err}")))?;
					match moq_mux::import::Track::new(request, state.catalog.clone(), &format, &init) {
						Ok(import) => {
							let demand = import.demand();
							(MediaDecoder::Track(Box::new(import)), Some(demand))
						}
						Err(moq_mux::Error::UnknownFormat(_)) => {
							return Err(MoqError::Codec(format!("unknown format: {format}")));
						}
						Err(err) => return Err(MoqError::Codec(format!("init failed: {err}"))),
					}
				}
				Err(err) => return Err(MoqError::Codec(format!("init failed: {err}"))),
			};

		Ok(Arc::new(MoqMediaProducer {
			inner: std::sync::Mutex::new(Some(MediaProducer { decoder, demand })),
		}))
	}

	/// Publish media on a requested track from
	/// [`MoqBroadcastDynamic::requested_track`].
	///
	/// The importer accepts the request, which is where the track's timescale is set.
	/// `format` controls the encoding of `init` and frame payloads. Only single-track
	/// formats are supported.
	pub fn publish_media_on_track(
		&self,
		request: &MoqTrackRequest,
		format: String,
		init: Vec<u8>,
	) -> Result<Arc<MoqMediaProducer>, MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let guard = self.state.lock().unwrap();
		let state = guard.as_ref().ok_or_else(|| MoqError::Closed)?;

		// The importer accepts the request itself, which is where the track's timescale is set.
		let request = request.take()?;

		let import = moq_mux::import::Track::new(request, state.catalog.clone(), &format, &init)
			.map_err(|err| MoqError::Codec(format!("init failed: {err}")))?;

		let demand = import.demand();

		Ok(Arc::new(MoqMediaProducer {
			inner: std::sync::Mutex::new(Some(MediaProducer {
				decoder: MediaDecoder::Track(Box::new(import)),
				demand: Some(demand),
			})),
		}))
	}

	/// Create a media track fed by a raw byte stream with unknown frame
	/// boundaries (e.g. piped Annex-B H.264 straight from an encoder).
	///
	/// Unlike [`Self::publish_media`], the importer infers frame boundaries, so
	/// the caller just pushes bytes via [`MoqMediaStreamProducer::write`]. Only
	/// self-describing stream formats are supported (avc3, hev1, av01, fmp4, mkv).
	pub fn publish_media_stream(&self, format: String) -> Result<Arc<MoqMediaStreamProducer>, MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let guard = self.state.lock().unwrap();
		let state = guard.as_ref().ok_or_else(|| MoqError::Closed)?;

		// A container may publish several tracks; a single codec fills one reserved
		// track. Try the container first so a codec format doesn't reserve a stray
		// track before being recognized.
		let decoder =
			match moq_mux::import::ContainerStream::new(state.broadcast.clone(), state.catalog.clone(), &format) {
				Ok(container) => StreamDecoder::Container(container),
				Err(moq_mux::Error::UnknownFormat(_)) => {
					let mut broadcast = state.broadcast.clone();
					let name = broadcast.unique_name(&format!(".{format}"));
					let request = broadcast
						.reserve_track(name)
						.map_err(|err| MoqError::Codec(format!("init failed: {err}")))?;
					StreamDecoder::Track(Box::new(
						moq_mux::import::TrackStream::new(request, state.catalog.clone(), &format)
							.map_err(|err| MoqError::Codec(format!("init failed: {err}")))?,
					))
				}
				Err(err) => return Err(MoqError::Codec(format!("init failed: {err}"))),
			};

		Ok(Arc::new(MoqMediaStreamProducer {
			inner: std::sync::Mutex::new(Some(MediaStreamProducer { decoder })),
		}))
	}

	/// Create a track for arbitrary byte payloads, no codec or container.
	///
	/// Same pattern as moq-boy's `status` and `command` tracks: raw UTF-8/JSON
	/// bytes written directly to moq-lite groups with no media framing. `info` sets
	/// track properties (priority, cache, timescale); omit for defaults.
	pub fn publish_track(&self, name: String, info: Option<MoqTrackInfo>) -> Result<Arc<MoqTrackProducer>, MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let guard = self.state.lock().unwrap();
		let state = guard.as_ref().ok_or_else(|| MoqError::Closed)?;
		let info = info.map(moq_net::TrackInfo::try_from).transpose()?;
		// Clone the broadcast handle (shared Arc internally) to get &mut access.
		let mut broadcast = state.broadcast.clone();
		let producer = broadcast.create_track(name, info)?;
		Ok(Arc::new(MoqTrackProducer {
			inner: std::sync::Mutex::new(Some(producer)),
		}))
	}

	/// Finish this publisher, finalizing the catalog stream.
	pub fn finish(&self) -> Result<(), MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let mut guard = self.state.lock().unwrap();
		let mut state = guard.take().ok_or_else(|| MoqError::Closed)?;
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
	/// [`MoqBroadcastProducer::publish_media_on_track`], or reject it with
	/// [`MoqTrackRequest::abort`]. The requesting subscriber stays pending until then.
	///
	/// Returns an error once the broadcast is closed or aborted.
	pub async fn requested_track(&self) -> Result<Arc<MoqTrackRequest>, MoqError> {
		self.task
			.run(|mut state| async move { state.requested_track().await })
			.await
	}

	/// Cancel all current and future `requested_track()` calls.
	pub fn cancel(&self) {
		self.task.cancel();
	}
}

// ---- Track Request ----

/// A track requested by a subscriber that hasn't been accepted yet.
///
/// Mirrors [`moq_net::TrackRequest`]: [`accept`](Self::accept) it to start producing raw
/// frames, hand it to [`MoqBroadcastProducer::publish_media_on_track`] to publish media,
/// or [`abort`](Self::abort) it to reject the waiting subscriber.
#[derive(uniffi::Object)]
pub struct MoqTrackRequest {
	inner: std::sync::Mutex<Option<moq_net::TrackRequest>>,
}

impl MoqTrackRequest {
	pub(crate) fn new(request: moq_net::TrackRequest) -> Self {
		Self {
			inner: std::sync::Mutex::new(Some(request)),
		}
	}

	/// Take the inner request so an importer can accept it (setting the timescale). Used by
	/// [`MoqBroadcastProducer::publish_media_on_track`].
	pub(crate) fn take(&self) -> Result<moq_net::TrackRequest, MoqError> {
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

	/// Accept the request as a raw track, fixing its [`MoqTrackInfo`] (timescale, etc.).
	///
	/// For media use [`MoqBroadcastProducer::publish_media_on_track`] instead, which lets
	/// the importer pick the timescale.
	pub fn accept(&self, info: Option<MoqTrackInfo>) -> Result<Arc<MoqTrackProducer>, MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let info = info.map(moq_net::TrackInfo::try_from).transpose()?;
		let request = self.take()?;
		Ok(Arc::new(MoqTrackProducer {
			inner: std::sync::Mutex::new(Some(request.accept(info))),
		}))
	}

	/// Reject the request with an application error code, failing the waiting subscriber.
	pub fn abort(&self, error_code: i32) -> Result<(), MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let error_code = u16::try_from(error_code).map_err(|_| MoqError::InvalidErrorCode(error_code))?;
		let request = self.take()?;
		request.reject(moq_net::Error::App(error_code));
		Ok(())
	}
}

// ---- Track Producer ----

#[derive(uniffi::Object)]
pub struct MoqTrackProducer {
	inner: std::sync::Mutex<Option<moq_net::TrackProducer>>,
}

#[uniffi::export]
impl MoqTrackProducer {
	/// Return the name of this track.
	pub fn name(&self) -> Result<String, MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let guard = self.inner.lock().unwrap();
		let track = guard.as_ref().ok_or(MoqError::Closed)?;
		Ok(track.name().to_string())
	}

	/// Wait until this track has at least one active consumer.
	pub async fn used(&self) -> Result<(), MoqError> {
		let track = self.inner.lock().unwrap().as_ref().ok_or(MoqError::Closed)?.clone();
		match crate::ffi::RUNTIME.spawn(async move { track.used().await }).await {
			Ok(result) => result.map_err(Into::into),
			Err(e) if e.is_cancelled() => Err(MoqError::Cancelled),
			Err(e) => Err(MoqError::Task(e)),
		}
	}

	/// Wait until this track has no active consumers.
	pub async fn unused(&self) -> Result<(), MoqError> {
		let track = self.inner.lock().unwrap().as_ref().ok_or(MoqError::Closed)?.clone();
		match crate::ffi::RUNTIME.spawn(async move { track.unused().await }).await {
			Ok(result) => result.map_err(Into::into),
			Err(e) if e.is_cancelled() => Err(MoqError::Cancelled),
			Err(e) => Err(MoqError::Task(e)),
		}
	}

	/// Create a consumer that reads from this producer's track.
	///
	/// Useful for local pub/sub without going through an origin/broadcast. `subscription`
	/// tunes delivery (priority, ordering, group range); omit for defaults.
	pub fn consume(&self, subscription: Option<MoqSubscription>) -> Result<Arc<MoqTrackConsumer>, MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let guard = self.inner.lock().unwrap();
		let track = guard.as_ref().ok_or(MoqError::Closed)?;
		let subscription = subscription.map(moq_net::Subscription::from);
		Ok(Arc::new(MoqTrackConsumer::new(track.subscribe(subscription))))
	}

	/// Append a new group to the track, returning a producer for writing frames into it.
	pub fn append_group(&self) -> Result<Arc<MoqGroupProducer>, MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let mut guard = self.inner.lock().unwrap();
		let track = guard.as_mut().ok_or(MoqError::Closed)?;
		let group = track.append_group()?;
		Ok(Arc::new(MoqGroupProducer {
			sequence: group.sequence,
			inner: std::sync::Mutex::new(Some(group)),
		}))
	}

	/// Convenience: write a single-frame group in one call, the same pattern
	/// used by moq-boy's status/command tracks.
	pub fn write_frame(&self, payload: Vec<u8>) -> Result<(), MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let mut guard = self.inner.lock().unwrap();
		let track = guard.as_mut().ok_or(MoqError::Closed)?;
		track.write_frame_now(payload)?;
		Ok(())
	}

	/// Abort this track with an application error code.
	pub fn abort(&self, error_code: i32) -> Result<(), MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let error_code = u16::try_from(error_code).map_err(|_| MoqError::InvalidErrorCode(error_code))?;
		let mut guard = self.inner.lock().unwrap();
		let mut track = guard.take().ok_or(MoqError::Closed)?;
		track.abort(moq_net::Error::App(error_code))?;
		Ok(())
	}

	pub fn finish(&self) -> Result<(), MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let mut guard = self.inner.lock().unwrap();
		let mut track = guard.take().ok_or(MoqError::Closed)?;
		track.finish()?;
		Ok(())
	}
}

#[derive(uniffi::Object)]
pub struct MoqGroupProducer {
	sequence: u64,
	inner: std::sync::Mutex<Option<moq_net::GroupProducer>>,
}

#[uniffi::export]
impl MoqGroupProducer {
	/// The sequence number of this group within the track.
	pub fn sequence(&self) -> u64 {
		self.sequence
	}

	/// Create a consumer that reads frames from this group.
	pub fn consume(&self) -> Result<Arc<MoqGroupConsumer>, MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let guard = self.inner.lock().unwrap();
		let group = guard.as_ref().ok_or_else(|| MoqError::Closed)?;
		Ok(Arc::new(MoqGroupConsumer::new(group.consume())))
	}

	/// Write a frame into this group.
	pub fn write_frame(&self, payload: Vec<u8>) -> Result<(), MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let mut guard = self.inner.lock().unwrap();
		let group = guard.as_mut().ok_or_else(|| MoqError::Closed)?;
		group.write_frame_now(payload)?;
		Ok(())
	}

	/// Mark the group as complete. No more frames can be written.
	pub fn finish(&self) -> Result<(), MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let mut guard = self.inner.lock().unwrap();
		let mut group = guard.take().ok_or_else(|| MoqError::Closed)?;
		group.finish()?;
		Ok(())
	}
}

// ---- Media Producer ----

#[uniffi::export]
impl MoqMediaProducer {
	/// Return the name of the media track.
	///
	/// Errors for a multi-track container source, which has no single track name.
	pub fn name(&self) -> Result<String, MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let guard = self.inner.lock().unwrap();
		let media = guard.as_ref().ok_or_else(|| MoqError::Closed)?;
		let demand = media
			.demand
			.as_ref()
			.ok_or_else(|| MoqError::Codec("demand unavailable for a multi-track container".into()))?;
		Ok(demand.name().to_string())
	}

	/// Wait until this media track has at least one active consumer.
	///
	/// Errors for a multi-track container source, which has no single demand.
	pub async fn used(&self) -> Result<(), MoqError> {
		let demand = self
			.inner
			.lock()
			.unwrap()
			.as_ref()
			.ok_or(MoqError::Closed)?
			.demand
			.clone()
			.ok_or_else(|| MoqError::Codec("demand unavailable for a multi-track container".into()))?;
		match crate::ffi::RUNTIME.spawn(async move { demand.used().await }).await {
			Ok(result) => result.map_err(Into::into),
			Err(e) if e.is_cancelled() => Err(MoqError::Cancelled),
			Err(e) => Err(MoqError::Task(e)),
		}
	}

	/// Wait until this media track has no active consumers.
	///
	/// Errors for a multi-track container source, which has no single demand.
	pub async fn unused(&self) -> Result<(), MoqError> {
		let demand = self
			.inner
			.lock()
			.unwrap()
			.as_ref()
			.ok_or(MoqError::Closed)?
			.demand
			.clone()
			.ok_or_else(|| MoqError::Codec("demand unavailable for a multi-track container".into()))?;
		match crate::ffi::RUNTIME.spawn(async move { demand.unused().await }).await {
			Ok(result) => result.map_err(Into::into),
			Err(e) if e.is_cancelled() => Err(MoqError::Cancelled),
			Err(e) => Err(MoqError::Task(e)),
		}
	}

	/// Write a frame to this media track.
	///
	/// `timestamp_us` is the presentation timestamp in microseconds.
	pub fn write_frame(&self, payload: Vec<u8>, timestamp_us: u64) -> Result<(), MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let mut guard = self.inner.lock().unwrap();
		let media = guard.as_mut().ok_or_else(|| MoqError::Closed)?;

		let timestamp = hang::container::Timestamp::from_micros(timestamp_us)?;
		media
			.decoder
			.decode(&payload, Some(timestamp))
			.map_err(|err| MoqError::Codec(format!("decode failed: {err}")))?;

		Ok(())
	}

	/// Finish this media track and finalize encoding.
	pub fn finish(&self) -> Result<(), MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let mut guard = self.inner.lock().unwrap();
		let mut media = guard.take().ok_or_else(|| MoqError::Closed)?;
		media
			.decoder
			.finish()
			.map_err(|err| MoqError::Codec(format!("finish failed: {err}")))?;
		Ok(())
	}
}

#[uniffi::export]
impl MoqMediaStreamProducer {
	/// Push raw stream bytes (e.g. Annex-B H.264 from an encoder). The importer
	/// frames whole access units and keeps any partial trailing frame for the
	/// next call, so callers can write arbitrary chunks.
	pub fn write(&self, payload: Vec<u8>) -> Result<(), MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let mut guard = self.inner.lock().unwrap();
		let media = guard.as_mut().ok_or_else(|| MoqError::Closed)?;

		match &mut media.decoder {
			StreamDecoder::Track(decoder) => decoder.decode(&payload),
			StreamDecoder::Container(decoder) => decoder.decode(&payload),
		}
		.map_err(|err| MoqError::Codec(format!("decode failed: {err}")))?;
		Ok(())
	}

	/// Finalize the track.
	///
	/// The importer emits each access unit when the *next* one's start code
	/// arrives, so a trailing access unit with no following delimiter (e.g. the
	/// last frame at EOF) is not emitted. This matches moq-cli's stdin path.
	pub fn finish(&self) -> Result<(), MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let mut guard = self.inner.lock().unwrap();
		let mut media = guard.take().ok_or_else(|| MoqError::Closed)?;
		match &mut media.decoder {
			StreamDecoder::Track(decoder) => decoder.finish(),
			StreamDecoder::Container(decoder) => decoder.finish(),
		}
		.map_err(|err| MoqError::Codec(format!("finish failed: {err}")))?;
		Ok(())
	}
}
