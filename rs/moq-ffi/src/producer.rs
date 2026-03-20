use std::str::FromStr;
use std::sync::Arc;

use bytes::Buf;

use crate::error::MoqError;

// ---- UniFFI Objects ----

struct BroadcastProducer {
	broadcast: moq_lite::BroadcastProducer,
	catalog: moq_mux::CatalogProducer,
}

#[derive(uniffi::Object)]
pub struct MoqBroadcastProducer {
	state: std::sync::Mutex<Option<BroadcastProducer>>,
}

impl MoqBroadcastProducer {
	pub(crate) fn consume(&self) -> Result<moq_lite::BroadcastConsumer, MoqError> {
		let guard = self.state.lock().unwrap();
		let state = guard.as_ref().ok_or_else(|| MoqError::Closed)?;
		Ok(state.broadcast.consume())
	}
}

#[derive(uniffi::Object)]
pub struct MoqMediaProducer {
	inner: std::sync::Mutex<Option<moq_mux::producer::Framed>>,
}

#[uniffi::export]
impl MoqBroadcastProducer {
	/// Create a new broadcast for publishing media tracks.
	///
	/// NOTE: This will do nothing until published to an origin.
	#[uniffi::constructor]
	pub fn new() -> Result<Arc<Self>, MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let mut broadcast = moq_lite::Broadcast::new().produce();
		let catalog = moq_mux::CatalogProducer::new(&mut broadcast)?;
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
		let format = moq_mux::producer::FramedFormat::from_str(&format)
			.map_err(|_| MoqError::Codec(format!("unknown format: {format}")))?;

		let mut buf = init.as_slice();
		let decoder = moq_mux::producer::Framed::new(state.broadcast.clone(), state.catalog.clone(), format, &mut buf)
			.map_err(|err| MoqError::Codec(format!("init failed: {err}")))?;

		if buf.has_remaining() {
			return Err(MoqError::Codec("init failed: trailing bytes".into()));
		}

		Ok(Arc::new(MoqMediaProducer {
			inner: std::sync::Mutex::new(Some(decoder)),
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

// ---- Media Producer ----

#[uniffi::export]
impl MoqMediaProducer {
	/// Write a frame to this media track.
	///
	/// `timestamp_us` is the presentation timestamp in microseconds.
	pub fn write_frame(&self, payload: Vec<u8>, timestamp_us: u64) -> Result<(), MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let mut guard = self.inner.lock().unwrap();
		let decoder = guard.as_mut().ok_or_else(|| MoqError::Closed)?;

		let timestamp = hang::container::Timestamp::from_micros(timestamp_us)?;
		let mut data = payload.as_slice();
		decoder
			.decode_frame(&mut data, Some(timestamp))
			.map_err(|err| MoqError::Codec(format!("decode failed: {err}")))?;

		if data.has_remaining() {
			return Err(MoqError::Codec("buffer was not fully consumed".into()));
		}

		Ok(())
	}

	/// Finish this media track and finalize encoding.
	pub fn finish(&self) -> Result<(), MoqError> {
		let _guard = crate::ffi::RUNTIME.enter();
		let mut guard = self.inner.lock().unwrap();
		let mut decoder = guard.take().ok_or_else(|| MoqError::Closed)?;
		decoder
			.finish()
			.map_err(|err| MoqError::Codec(format!("finish failed: {err}")))?;
		Ok(())
	}
}
