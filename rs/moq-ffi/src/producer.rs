use std::sync::Arc;

use crate::consumer::{MoqBroadcastConsumer, MoqGroupConsumer, MoqTrackConsumer};
use crate::error::MoqError;
use crate::ffi::Task;

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
		// dev's reshape yields a TrackRequest the producer must accept; accept with default
		// info (the caller then produces frames onto the returned track).
		let track = self.inner.requested_track().await?.accept(None);
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
		let track = moq_mux::import::unique_track(&mut broadcast, &format!(".{format}"))
			.map_err(|err| MoqError::Codec(format!("init failed: {err}")))?;
		let decoder = moq_mux::import::Track::new(track, state.catalog.clone(), &format, &init)
			.map_err(|err| MoqError::Codec(format!("init failed: {err}")))?;

		let demand = decoder.demand();

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

		let decoder = moq_mux::import::Track::new(track_clone.clone(), state.catalog.clone(), &format, &init)
			.map_err(|err| MoqError::Codec(format!("init failed: {err}")))?;

		let mut guard = track.inner.lock().unwrap();
		guard.take().ok_or_else(|| MoqError::Closed)?;

		Ok(Arc::new(MoqMediaProducer {
			inner: std::sync::Mutex::new(Some(MediaProducer {
				decoder,
				demand: track_clone.demand(),
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
		// A single codec streams onto one track; anything else is treated as a
		// container. `UnknownFormat` from the track importer is the signal to fall
		// through rather than a hard error.
		let decoder = match moq_mux::import::TrackStream::new(state.broadcast.clone(), state.catalog.clone(), &format) {
			Ok(track) => StreamDecoder::Track(Box::new(track)),
			Err(moq_mux::Error::UnknownFormat(_)) => StreamDecoder::Container(
				moq_mux::import::ContainerStream::new(state.broadcast.clone(), state.catalog.clone(), &format)
					.map_err(|err| MoqError::Codec(format!("init failed: {err}")))?,
			),
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
		// Clone the broadcast handle (shared Arc internally) to get &mut access.
		let mut broadcast = state.broadcast.clone();
		let producer = broadcast.create_track(name, None)?;
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
	/// Useful for local pub/sub without going through an origin/broadcast.
	pub fn consume(&self) -> Result<Arc<MoqTrackConsumer>, MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let guard = self.inner.lock().unwrap();
		let track = guard.as_ref().ok_or_else(|| MoqError::Closed)?;
		Ok(Arc::new(MoqTrackConsumer::new(track.subscribe(None))))
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
