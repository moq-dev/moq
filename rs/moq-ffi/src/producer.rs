use std::sync::Arc;

use moq_mux::catalog::hang::Extra;

use crate::consumer::{MoqBroadcastConsumer, MoqGroupConsumer, MoqTrackConsumer};
use crate::error::MoqError;
use crate::ffi::Task;

// ---- UniFFI Objects ----

pub(crate) struct BroadcastProducer {
	pub(crate) broadcast: moq_net::BroadcastProducer,
	pub(crate) catalog: moq_mux::catalog::Producer<Extra>,
}

/// A whole-frame importer: a single codec track, or a container that may publish
/// several tracks. The format string picks which when the producer is created.
enum MediaDecoder {
	// Boxed because the codec splitters/imports make this variant much larger
	// than the (already boxed) container one.
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
	Track(Box<moq_mux::import::TrackStream<Extra>>),
	Container(moq_mux::import::ContainerStream<Extra>),
}

impl StreamDecoder {
	fn decode(&mut self, data: &[u8]) -> moq_mux::Result<()> {
		match self {
			Self::Track(t) => t.decode(data),
			Self::Container(c) => c.decode(data),
		}
	}

	fn finish(&mut self) -> moq_mux::Result<()> {
		match self {
			Self::Track(t) => t.finish(),
			Self::Container(c) => c.finish(),
		}
	}
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
		let track = self.inner.requested_track().await?;
		Ok(Arc::new(MoqTrackProducer {
			inner: std::sync::Mutex::new(Some(track)),
		}))
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
		let mut broadcast = moq_net::Broadcast::new().produce();
		// The untyped `Extra` extension lets catalog sections be set by name across the FFI boundary.
		let catalog = moq_mux::catalog::Producer::new_extra(&mut broadcast)?;
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
		// A container may publish several tracks; a single codec fills one minted
		// track. Try the container first so a codec format doesn't mint a stray
		// track on the way to being recognized.
		let (decoder, demand) =
			match moq_mux::import::Container::new(state.broadcast.clone(), state.catalog.clone(), &format, &init) {
				Ok(container) => (MediaDecoder::Container(container), None),
				Err(moq_mux::Error::UnknownFormat(_)) => {
					let mut broadcast = state.broadcast.clone();
					let name = broadcast.unique_name(&format!(".{format}"));
					let track = broadcast.create_track(moq_net::Track::new(name))?;
					match moq_mux::import::Track::new(track, state.catalog.clone(), &format, &init) {
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

	/// Publish media on an existing track, usually one returned by
	/// [`MoqBroadcastDynamic::requested_track`].
	///
	/// `format` controls the encoding of `init` and frame payloads. Only
	/// single-track formats are supported.
	pub fn publish_media_on_track(
		&self,
		track: &MoqTrackProducer,
		format: String,
		init: Vec<u8>,
	) -> Result<Arc<MoqMediaProducer>, MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let guard = self.state.lock().unwrap();
		let state = guard.as_ref().ok_or_else(|| MoqError::Closed)?;
		let track_clone = {
			let guard = track.inner.lock().unwrap();
			guard.as_ref().ok_or_else(|| MoqError::Closed)?.clone()
		};

		let import = moq_mux::import::Track::new(track_clone, state.catalog.clone(), &format, &init)
			.map_err(|err| MoqError::Codec(format!("init failed: {err}")))?;
		let demand = import.demand();

		let mut guard = track.inner.lock().unwrap();
		guard.take().ok_or_else(|| MoqError::Closed)?;

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
		// A container stream may publish several tracks; a single codec fills one
		// minted track. Try the container first so a codec format doesn't mint a
		// stray track on the way to being recognized.
		let decoder =
			match moq_mux::import::ContainerStream::new(state.broadcast.clone(), state.catalog.clone(), &format) {
				Ok(container) => StreamDecoder::Container(container),
				Err(moq_mux::Error::UnknownFormat(_)) => {
					let mut broadcast = state.broadcast.clone();
					let name = broadcast.unique_name(&format!(".{format}"));
					let track = broadcast.create_track(moq_net::Track::new(name))?;
					match moq_mux::import::TrackStream::new(track, state.catalog.clone(), &format) {
						Ok(import) => StreamDecoder::Track(Box::new(import)),
						Err(moq_mux::Error::UnknownFormat(_)) => {
							return Err(MoqError::Codec(format!("unknown stream format: {format}")));
						}
						Err(err) => return Err(MoqError::Codec(format!("init failed: {err}"))),
					}
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
	/// bytes written directly to moq-lite groups with no media framing.
	pub fn publish_track(&self, name: String) -> Result<Arc<MoqTrackProducer>, MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let guard = self.state.lock().unwrap();
		let state = guard.as_ref().ok_or_else(|| MoqError::Closed)?;
		let track = moq_net::Track { name, priority: 0 };
		// Clone the broadcast handle (shared Arc internally) to get &mut access.
		let mut broadcast = state.broadcast.clone();
		let producer = broadcast.create_track(track)?;
		Ok(Arc::new(MoqTrackProducer {
			inner: std::sync::Mutex::new(Some(producer)),
		}))
	}

	/// Set (or replace) an untyped application section in this broadcast's catalog.
	///
	/// `value` is any JSON document (object, array, string, ...). The section lands as a
	/// top-level key alongside `video`/`audio` and reaches subscribers via
	/// [`MoqCatalog::extra`](crate::media::MoqCatalog). `name` must not be a reserved media
	/// section (`video`/`audio`). The catalog is republished automatically.
	///
	/// Use this to advertise a side-channel track (e.g. a transcript or captions track) that
	/// the catalog doesn't model natively, mirroring the JS catalog's pass-through sections.
	pub fn set_catalog_section(&self, name: String, value: String) -> Result<(), MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let value: serde_json::Value = serde_json::from_str(&value).map_err(|err| MoqError::Json(err.to_string()))?;
		self.with_state(|state| {
			state.catalog.set_section(name, value)?;
			Ok(())
		})
	}

	/// Remove an untyped application section from this broadcast's catalog by name.
	///
	/// A no-op if no section with that name exists. The catalog is republished automatically.
	pub fn remove_catalog_section(&self, name: String) -> Result<(), MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		self.with_state(|state| {
			state.catalog.remove_section(&name);
			Ok(())
		})
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
		let track = guard.as_ref().ok_or_else(|| MoqError::Closed)?;
		Ok(track.name.clone())
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
	/// Useful for local pub/sub without going through an origin/broadcast.
	pub fn consume(&self) -> Result<Arc<MoqTrackConsumer>, MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let guard = self.inner.lock().unwrap();
		let track = guard.as_ref().ok_or_else(|| MoqError::Closed)?;
		Ok(Arc::new(MoqTrackConsumer::new(track.consume())))
	}

	/// Append a new group to the track, returning a producer for writing frames into it.
	pub fn append_group(&self) -> Result<Arc<MoqGroupProducer>, MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let mut guard = self.inner.lock().unwrap();
		let track = guard.as_mut().ok_or_else(|| MoqError::Closed)?;
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
		let track = guard.as_mut().ok_or_else(|| MoqError::Closed)?;
		track.write_frame(payload)?;
		Ok(())
	}

	/// Abort this track with an application error code.
	pub fn abort(&self, error_code: i32) -> Result<(), MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let error_code = u16::try_from(error_code).map_err(|_| MoqError::InvalidErrorCode(error_code))?;
		let mut guard = self.inner.lock().unwrap();
		let mut track = guard.take().ok_or_else(|| MoqError::Closed)?;
		track.abort(moq_net::Error::App(error_code))?;
		Ok(())
	}

	pub fn finish(&self) -> Result<(), MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let mut guard = self.inner.lock().unwrap();
		let mut track = guard.take().ok_or_else(|| MoqError::Closed)?;
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
	///
	/// Errors for a multi-track container source, which has no single track name.
	pub fn name(&self) -> Result<String, MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let guard = self.inner.lock().unwrap();
		let media = guard.as_ref().ok_or_else(|| MoqError::Closed)?;
		let demand = media
			.demand
			.as_ref()
			.ok_or_else(|| MoqError::Codec("track name unavailable for a multi-track container".into()))?;
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
			.decode(payload.as_slice(), Some(timestamp))
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

		media
			.decoder
			.decode(&payload)
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
		media
			.decoder
			.finish()
			.map_err(|err| MoqError::Codec(format!("finish failed: {err}")))?;
		Ok(())
	}
}
