use std::str::FromStr;
use std::sync::Arc;

use bytes::Buf;

use crate::error::MoqError;

// ---- UniFFI Objects ----

#[derive(uniffi::Object)]
pub struct MoqBroadcastProducer {
	inner: std::sync::Mutex<Option<(moq_lite::BroadcastProducer, moq_mux::CatalogProducer)>>,
}

impl MoqBroadcastProducer {
	pub(crate) fn consume(&self) -> Result<moq_lite::BroadcastConsumer, MoqError> {
		let guard = self.inner.lock().unwrap();
		let inner = guard.as_ref().ok_or_else(|| MoqError::Closed)?;
		Ok(inner.0.consume())
	}
}

#[derive(uniffi::Object)]
pub struct MoqMediaProducer {
	inner: std::sync::Mutex<Option<moq_mux::import::Decoder>>,
}

// ---- Top-level functions ----

/// Create a new broadcast for publishing media tracks.
///
/// NOTE: This will do nothing until published to an origin.
#[uniffi::export]
pub fn moq_broadcast_produce() -> Result<Arc<MoqBroadcastProducer>, MoqError> {
	let mut broadcast = moq_lite::BroadcastProducer::new();
	let catalog = moq_mux::CatalogProducer::new(&mut broadcast)?;
	Ok(Arc::new(MoqBroadcastProducer {
		inner: std::sync::Mutex::new(Some((broadcast, catalog))),
	}))
}

// ---- Publisher ----

#[uniffi::export]
impl MoqBroadcastProducer {
	/// Create a new media track for this broadcast.
	///
	/// `format` controls the encoding of `init` and frame payloads.
	pub fn publish_media(&self, format: String, init: Vec<u8>) -> Result<Arc<MoqMediaProducer>, MoqError> {
		let guard = self.inner.lock().unwrap();
		let inner = guard.as_ref().ok_or_else(|| MoqError::Closed)?;
		let format = moq_mux::import::DecoderFormat::from_str(&format)
			.map_err(|_| MoqError::Codec(format!("unknown format: {format}")))?;

		let mut buf = init.as_slice();
		let decoder = moq_mux::import::Decoder::new(inner.0.clone(), inner.1.clone(), format, &mut buf)
			.map_err(|err| MoqError::Codec(format!("init failed: {err}")))?;

		Ok(Arc::new(MoqMediaProducer {
			inner: std::sync::Mutex::new(Some(decoder)),
		}))
	}

	/// Close this publisher, finishing the catalog stream.
	pub fn close(&self) -> Result<(), MoqError> {
		let mut guard = self.inner.lock().unwrap();
		if let Some((_, mut catalog)) = guard.take() {
			catalog.finish()?;
		}
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
		let mut guard = self.inner.lock().unwrap();
		if let Some(mut decoder) = guard.take() {
			decoder
				.finish()
				.map_err(|err| MoqError::Codec(format!("finish failed: {err}")))?;
		}
		Ok(())
	}
}

impl Drop for MoqMediaProducer {
	fn drop(&mut self) {
		let mut guard = self.inner.lock().unwrap();
		if let Some(mut decoder) = guard.take() {
			let _ = decoder.finish();
		}
	}
}
