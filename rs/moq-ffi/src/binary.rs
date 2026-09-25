//! Binary data tracks over the FFI boundary, advertised in the catalog.
//!
//! The binary counterpart of [`crate::json`]: opaque payloads (for example a camera's latest JPEG
//! thumbnail) on a named track, in either mode — `snapshot` (each payload supersedes the last) or
//! `stream` (every payload preserved in order). The broadcast's catalog carries
//! `binary.tracks.<name>` (mode, plus `mime` and `compression` when set) for as long as the track
//! lives, so a consumer discovers it without knowing the application.

use std::sync::Arc;

use moq_mux::catalog::hang::Extra;

use crate::error::MoqError;
use crate::producer::MoqBroadcastProducer;

/// Options for a binary data track, in either mode (the mode is fixed by the constructor).
#[derive(Clone, uniffi::Record)]
pub struct MoqBinaryConfig {
	/// DEFLATE-compress each payload, advertised in the catalog entry.
	#[uniffi(default = false)]
	pub compression: bool,

	/// The payloads' media type (e.g. `image/jpeg`), or `None` to leave it unstated.
	#[uniffi(default = None)]
	pub mime: Option<String>,
}

impl From<MoqBinaryConfig> for moq_mux::binary::Config {
	fn from(config: MoqBinaryConfig) -> Self {
		let mut out = moq_mux::binary::Config::default().with_compression(config.compression);
		if let Some(mime) = config.mime {
			out = out.with_mime(mime);
		}
		out
	}
}

#[uniffi::export]
impl MoqBroadcastProducer {
	/// Publish a binary snapshot track (lossy latest-value) by name, advertised in the catalog.
	///
	/// Errors if the catalog already carries an entry under `name`.
	pub fn publish_binary_snapshot(
		&self,
		name: String,
		config: MoqBinaryConfig,
	) -> Result<Arc<MoqBinarySnapshotProducer>, MoqError> {
		let _guard = crate::ffi::enter();
		self.with_state(|state| {
			let track = state.broadcast.create_track(name, None)?;
			let producer = state.catalog.binary_snapshot(track, moq_mux::binary::Config::from(config))?;
			Ok(Arc::new(MoqBinarySnapshotProducer {
				inner: std::sync::Mutex::new(Some(producer)),
			}))
		})
	}

	/// Publish a binary stream track (lossless append-log) by name, advertised in the catalog.
	///
	/// Errors if the catalog already carries an entry under `name`.
	pub fn publish_binary_stream(
		&self,
		name: String,
		config: MoqBinaryConfig,
	) -> Result<Arc<MoqBinaryStreamProducer>, MoqError> {
		let _guard = crate::ffi::enter();
		self.with_state(|state| {
			let track = state.broadcast.create_track(name, None)?;
			let producer = state.catalog.binary_stream(track, moq_mux::binary::Config::from(config))?;
			Ok(Arc::new(MoqBinaryStreamProducer {
				inner: std::sync::Mutex::new(Some(producer)),
			}))
		})
	}
}

/// Publishes opaque payloads that consumers see as a single latest value.
#[derive(uniffi::Object)]
pub struct MoqBinarySnapshotProducer {
	inner: std::sync::Mutex<Option<moq_mux::binary::Snapshot<Extra>>>,
}

#[uniffi::export]
impl MoqBinarySnapshotProducer {
	/// Publish a new payload, superseding the last.
	pub fn update(&self, payload: Vec<u8>) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let mut guard = self.inner.lock().unwrap();
		guard.as_mut().ok_or(MoqError::Closed)?.update(payload)?;
		Ok(())
	}

	/// Finish the track and retire its catalog entry.
	pub fn finish(&self) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let producer = self.inner.lock().unwrap().take().ok_or(MoqError::Closed)?;
		producer.finish()?;
		Ok(())
	}
}

/// Publishes an ordered log of opaque payloads, one per append.
#[derive(uniffi::Object)]
pub struct MoqBinaryStreamProducer {
	inner: std::sync::Mutex<Option<moq_mux::binary::Stream<Extra>>>,
}

#[uniffi::export]
impl MoqBinaryStreamProducer {
	/// Append one payload to the log.
	pub fn append(&self, payload: Vec<u8>) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let mut guard = self.inner.lock().unwrap();
		guard.as_mut().ok_or(MoqError::Closed)?.append(payload)?;
		Ok(())
	}

	/// Finish the track and retire its catalog entry.
	pub fn finish(&self) -> Result<(), MoqError> {
		let _guard = crate::ffi::enter();
		let producer = self.inner.lock().unwrap().take().ok_or(MoqError::Closed)?;
		producer.finish()?;
		Ok(())
	}
}
