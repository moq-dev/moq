use std::sync::Arc;

use crate::consumer::{MoqBroadcastConsumer, MoqGroupConsumer, MoqSubscription, MoqTrackConsumer};
use crate::error::MoqError;
use crate::ffi::Task;

/// Publisher-side track properties, mirroring [`moq_net::TrackInfo`].
///
/// Construct with the fields you care about; the rest default to moq-net's defaults
/// (priority 0, ordered, uncompressed, default cache, untimed).
#[derive(Clone, uniffi::Record)]
pub struct MoqTrackInfo {
	/// Priority, used only to break ties between subscriptions of equal subscriber priority.
	#[uniffi(default = 0)]
	pub priority: u8,
	/// Whether groups are delivered in sequence order.
	#[uniffi(default = true)]
	pub ordered: bool,
	/// Hint that this track's frames are worth compressing (e.g. a JSON catalog).
	#[uniffi(default = false)]
	pub compress: bool,
	/// How long the relay should cache past groups, in milliseconds. Null uses the default.
	#[uniffi(default = None)]
	pub cache_ms: Option<u64>,
	/// Per-frame timescale in ticks per second. Null leaves the track untimed; set it only
	/// when writing timestamped frames, since raw `write_frame` is untimed and would mismatch.
	#[uniffi(default = None)]
	pub timescale: Option<u64>,
}

impl TryFrom<MoqTrackInfo> for moq_net::TrackInfo {
	type Error = MoqError;

	fn try_from(info: MoqTrackInfo) -> Result<Self, MoqError> {
		let mut out = moq_net::TrackInfo::default()
			.with_priority(info.priority)
			.with_ordered(info.ordered)
			.with_compress(info.compress);
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
	pub(crate) catalog: moq_mux::catalog::Producer,
}

struct MediaProducer {
	decoder: moq_mux::import::Track,
	demand: moq_net::TrackDemand,
}

/// A byte-stream importer: a single codec track or a container that may publish
/// several tracks. The format string picks which when the producer is created.
enum StreamDecoder {
	// Boxed because the codec splitter/import make this variant much larger than
	// the (already boxed) container one.
	Track(Box<moq_mux::import::TrackStream>),
	Container(moq_mux::import::ContainerStream),
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
	async fn requested_track(&mut self) -> Result<Arc<MoqTrackProducer>, MoqError> {
		// Hand back the request un-accepted so the publishing operation picks the timescale
		// at accept: raw writes accept untimed, publish_media_on_track at the media timescale.
		// The subscriber's subscribe stays pending until that first use.
		let request = self.inner.requested_track().await?;
		Ok(Arc::new(MoqTrackProducer::pending(request)))
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
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast)?;
		Ok(Arc::new(Self {
			state: std::sync::Mutex::new(Some(BroadcastProducer { broadcast, catalog })),
		}))
	}

	/// Create a new media track for this broadcast.
	///
	/// `format` controls the encoding of `init` and frame payloads.
	pub fn publish_media(&self, format: String, init: Vec<u8>) -> Result<Arc<MoqMediaProducer>, MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let guard = self.state.lock().unwrap();
		let state = guard.as_ref().ok_or_else(|| MoqError::Closed)?;

		let mut broadcast = state.broadcast.clone();
		let name = broadcast.unique_name(&format!(".{format}"));
		let request = broadcast
			.reserve_track(name)
			.map_err(|err| MoqError::Codec(format!("init failed: {err}")))?;
		let decoder = moq_mux::import::Track::new(request, state.catalog.clone(), &format, &init)
			.map_err(|err| MoqError::Codec(format!("init failed: {err}")))?;

		let demand = decoder.demand();

		Ok(Arc::new(MoqMediaProducer {
			inner: std::sync::Mutex::new(Some(MediaProducer { decoder, demand })),
		}))
	}

	/// Publish media on a requested track, usually one returned by
	/// [`MoqBroadcastDynamic::requested_track`].
	///
	/// The importer accepts the request, which is where the track's timescale is set,
	/// so the track must still be pending (not already used for raw writes). `format`
	/// controls the encoding of `init` and frame payloads. Only single-track formats
	/// are supported.
	pub fn publish_media_on_track(
		&self,
		track: &MoqTrackProducer,
		format: String,
		init: Vec<u8>,
	) -> Result<Arc<MoqMediaProducer>, MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let guard = self.state.lock().unwrap();
		let state = guard.as_ref().ok_or_else(|| MoqError::Closed)?;

		// The importer accepts the request itself (setting the timescale), so hand it
		// the un-accepted request rather than an already-active producer.
		let request = track.take_pending()?;

		let decoder = moq_mux::import::Track::new(request, state.catalog.clone(), &format, &init)
			.map_err(|err| MoqError::Codec(format!("init failed: {err}")))?;

		let demand = decoder.demand();

		Ok(Arc::new(MoqMediaProducer {
			inner: std::sync::Mutex::new(Some(MediaProducer { decoder, demand })),
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
	/// track properties (priority, cache, compression); omit for defaults.
	pub fn publish_track(&self, name: String, info: Option<MoqTrackInfo>) -> Result<Arc<MoqTrackProducer>, MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let guard = self.state.lock().unwrap();
		let state = guard.as_ref().ok_or_else(|| MoqError::Closed)?;
		let info = info.map(moq_net::TrackInfo::try_from).transpose()?;
		// Clone the broadcast handle (shared Arc internally) to get &mut access.
		let mut broadcast = state.broadcast.clone();
		let producer = broadcast.create_track(name, info)?;
		Ok(Arc::new(MoqTrackProducer::active(producer)))
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
	/// The returned track is not yet accepted: the requesting subscriber stays pending until
	/// the first producer operation on it. Publish media with
	/// [`MoqBroadcastProducer::publish_media_on_track`] (accepts at the media timescale) or
	/// write raw frames with [`MoqTrackProducer::write_frame`] (accepts untimed); call
	/// [`MoqTrackProducer::abort`] to reject the request.
	///
	/// Returns an error once the broadcast is closed or aborted.
	pub async fn requested_track(&self) -> Result<Arc<MoqTrackProducer>, MoqError> {
		self.task
			.run(|mut state| async move { state.requested_track().await })
			.await
	}

	/// Cancel all current and future `requested_track()` calls.
	pub fn cancel(&self) {
		self.task.cancel();
	}
}

// ---- Track Producer ----

/// Either a not-yet-accepted dynamic request or an accepted track.
///
/// A track from [`MoqBroadcastDynamic::requested_track`] starts `Pending` so the publishing
/// operation can pick the timescale at accept: raw operations accept untimed,
/// [`MoqBroadcastProducer::publish_media_on_track`] accepts at the media timescale. A track
/// from [`MoqBroadcastProducer::publish_track`] is `Active` from the start.
enum TrackState {
	Pending(moq_net::TrackRequest),
	Active(moq_net::TrackProducer),
}

#[derive(uniffi::Object)]
pub struct MoqTrackProducer {
	inner: std::sync::Mutex<Option<TrackState>>,
}

impl MoqTrackProducer {
	/// Wrap a not-yet-accepted dynamic request (see [`MoqBroadcastDynamic::requested_track`]).
	pub(crate) fn pending(request: moq_net::TrackRequest) -> Self {
		Self {
			inner: std::sync::Mutex::new(Some(TrackState::Pending(request))),
		}
	}

	/// Wrap an already-accepted track producer (see [`MoqBroadcastProducer::publish_track`]).
	pub(crate) fn active(producer: moq_net::TrackProducer) -> Self {
		Self {
			inner: std::sync::Mutex::new(Some(TrackState::Active(producer))),
		}
	}

	/// Ensure the track is accepted and return mutable access to the producer.
	///
	/// A still-pending request is accepted with `info`, fixing the timescale on its first
	/// use as a producer. Raw operations pass `None` (untimed); media instead takes the
	/// request via [`Self::take_pending`] so the importer accepts it. An already-active
	/// track ignores `info`.
	fn producer(
		slot: &mut Option<TrackState>,
		info: impl Into<Option<moq_net::TrackInfo>>,
	) -> Result<&mut moq_net::TrackProducer, MoqError> {
		match slot.take().ok_or(MoqError::Closed)? {
			TrackState::Pending(request) => *slot = Some(TrackState::Active(request.accept(info))),
			active => *slot = Some(active),
		}
		match slot.as_mut() {
			Some(TrackState::Active(producer)) => Ok(producer),
			_ => unreachable!("set to Active above"),
		}
	}

	/// Take the un-accepted request so an importer can accept it (and set the
	/// timescale) itself. Errors if the track was already used directly and thus
	/// accepted as a raw, untimed track.
	fn take_pending(&self) -> Result<moq_net::TrackRequest, MoqError> {
		match self.inner.lock().unwrap().take().ok_or(MoqError::Closed)? {
			TrackState::Pending(request) => Ok(request),
			TrackState::Active(producer) => {
				// Put it back so the handle stays usable, then report the misuse.
				*self.inner.lock().unwrap() = Some(TrackState::Active(producer));
				Err(MoqError::Codec("track already in use".into()))
			}
		}
	}
}

#[uniffi::export]
impl MoqTrackProducer {
	/// Return the name of this track.
	pub fn name(&self) -> Result<String, MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let guard = self.inner.lock().unwrap();
		let name = match guard.as_ref().ok_or(MoqError::Closed)? {
			TrackState::Pending(request) => request.name().to_string(),
			TrackState::Active(producer) => producer.name().to_string(),
		};
		Ok(name)
	}

	/// Wait until this track has at least one active consumer.
	///
	/// A still-pending request is accepted untimed (its first producer use).
	pub async fn used(&self) -> Result<(), MoqError> {
		let track = {
			let mut guard = self.inner.lock().unwrap();
			Self::producer(&mut guard, None)?.clone()
		};
		match crate::ffi::RUNTIME.spawn(async move { track.used().await }).await {
			Ok(result) => result.map_err(Into::into),
			Err(e) if e.is_cancelled() => Err(MoqError::Cancelled),
			Err(e) => Err(MoqError::Task(e)),
		}
	}

	/// Wait until this track has no active consumers.
	///
	/// A still-pending request is accepted untimed (its first producer use).
	pub async fn unused(&self) -> Result<(), MoqError> {
		let track = {
			let mut guard = self.inner.lock().unwrap();
			Self::producer(&mut guard, None)?.clone()
		};
		match crate::ffi::RUNTIME.spawn(async move { track.unused().await }).await {
			Ok(result) => result.map_err(Into::into),
			Err(e) if e.is_cancelled() => Err(MoqError::Cancelled),
			Err(e) => Err(MoqError::Task(e)),
		}
	}

	/// Create a consumer that reads from this producer's track.
	///
	/// Useful for local pub/sub without going through an origin/broadcast. `subscription`
	/// tunes delivery (priority, ordering, group range); omit for defaults. A still-pending
	/// request is accepted untimed (its first producer use).
	pub fn consume(&self, subscription: Option<MoqSubscription>) -> Result<Arc<MoqTrackConsumer>, MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let mut guard = self.inner.lock().unwrap();
		let subscription = subscription.map(moq_net::Subscription::from);
		let subscriber = Self::producer(&mut guard, None)?.subscribe(subscription);
		Ok(Arc::new(MoqTrackConsumer::new(subscriber)))
	}

	/// Append a new group to the track, returning a producer for writing frames into it.
	///
	/// A still-pending request is accepted untimed (its first producer use).
	pub fn append_group(&self) -> Result<Arc<MoqGroupProducer>, MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let mut guard = self.inner.lock().unwrap();
		let group = Self::producer(&mut guard, None)?.append_group()?;
		Ok(Arc::new(MoqGroupProducer {
			sequence: group.sequence,
			inner: std::sync::Mutex::new(Some(group)),
		}))
	}

	/// Convenience: write a single-frame group in one call, the same pattern
	/// used by moq-boy's status/command tracks.
	///
	/// A still-pending request is accepted untimed (its first producer use), so a track
	/// destined for media must go through [`MoqBroadcastProducer::publish_media_on_track`]
	/// before any raw write.
	pub fn write_frame(&self, payload: Vec<u8>) -> Result<(), MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let mut guard = self.inner.lock().unwrap();
		Self::producer(&mut guard, None)?.write_frame(payload)?;
		Ok(())
	}

	/// Abort this track with an application error code.
	///
	/// A still-pending request is rejected (the subscriber's subscribe fails); an accepted
	/// track is aborted with the code.
	pub fn abort(&self, error_code: i32) -> Result<(), MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let error_code = u16::try_from(error_code).map_err(|_| MoqError::InvalidErrorCode(error_code))?;
		let mut guard = self.inner.lock().unwrap();
		match guard.take().ok_or(MoqError::Closed)? {
			TrackState::Pending(request) => request.reject(moq_net::Error::App(error_code)),
			TrackState::Active(mut producer) => producer.abort(moq_net::Error::App(error_code))?,
		}
		Ok(())
	}

	/// Finish this track. A still-pending request is accepted untimed and finished empty.
	pub fn finish(&self) -> Result<(), MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let mut guard = self.inner.lock().unwrap();
		Self::producer(&mut guard, None)?.finish()?;
		*guard = None;
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
		group.write_frame(payload)?;
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
	pub fn name(&self) -> Result<String, MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let guard = self.inner.lock().unwrap();
		let media = guard.as_ref().ok_or_else(|| MoqError::Closed)?;
		Ok(media.demand.name().to_string())
	}

	/// Wait until this media track has at least one active consumer.
	pub async fn used(&self) -> Result<(), MoqError> {
		let demand = self
			.inner
			.lock()
			.unwrap()
			.as_ref()
			.ok_or(MoqError::Closed)?
			.demand
			.clone();
		match crate::ffi::RUNTIME.spawn(async move { demand.used().await }).await {
			Ok(result) => result.map_err(Into::into),
			Err(e) if e.is_cancelled() => Err(MoqError::Cancelled),
			Err(e) => Err(MoqError::Task(e)),
		}
	}

	/// Wait until this media track has no active consumers.
	pub async fn unused(&self) -> Result<(), MoqError> {
		let demand = self
			.inner
			.lock()
			.unwrap()
			.as_ref()
			.ok_or(MoqError::Closed)?
			.demand
			.clone();
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
