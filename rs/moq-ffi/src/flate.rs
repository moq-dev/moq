//! Opaque data tracks over the FFI boundary, advertised in the catalog.
//!
//! The `moq-flate` counterpart of [`crate::json`]: opaque payloads (for example a camera's latest
//! JPEG thumbnail) on a track, in either mode: `snapshot` (each payload supersedes the last)
//! or `stream` (every payload preserved in order). The broadcast's catalog carries
//! `binary.tracks.<name>` (mode, plus `mime` and `compression` when set) for as long as the track
//! lives, so a consumer discovers it without knowing the application.

use std::sync::Arc;

use moq_mux::binary::Config;
use moq_mux::catalog::hang::Extra;

use crate::error::MoqError;
use crate::producer::{MoqBroadcastProducer, MoqTrackProducer};

/// Options for an opaque data track, in either mode (the mode is fixed by the constructor).
#[derive(Clone, uniffi::Record)]
pub struct MoqFlateConfig {
	/// DEFLATE-compress each payload, advertised in the catalog entry.
	#[uniffi(default = false)]
	pub compression: bool,

	/// The payloads' media type (e.g. `image/jpeg`), or `None` to leave it unstated.
	#[uniffi(default = None)]
	pub mime: Option<String>,
}

impl From<MoqFlateConfig> for Config {
	fn from(config: MoqFlateConfig) -> Self {
		let mut out = Config::default().with_compression(config.compression);
		if let Some(mime) = config.mime {
			out = out.with_mime(mime);
		}
		out
	}
}

/// Publishes opaque payloads that consumers see as a single latest value.
#[derive(uniffi::Object)]
pub struct MoqFlateSnapshotProducer {
	inner: std::sync::Mutex<Option<moq_mux::binary::Snapshot<Extra>>>,
}

#[uniffi::export]
impl MoqFlateSnapshotProducer {
	/// Publish `track` as an opaque snapshot track (lossy latest-value), advertised in `broadcast`'s catalog.
	///
	/// `track` must come from `broadcast`; otherwise the catalog advertises a track the broadcast
	/// does not carry.
	///
	/// Takes over `track`, whose handle is closed afterward. Errors if the catalog already
	/// carries an entry under the track's name.
	#[uniffi::constructor]
	pub fn new(
		broadcast: &MoqBroadcastProducer,
		track: &MoqTrackProducer,
		config: MoqFlateConfig,
	) -> Result<Arc<Self>, MoqError> {
		let _guard = crate::ffi::enter();
		let config = Config::from(config);
		let producer =
			track.adopt(|track| broadcast.with_state(|state| Ok(state.catalog.binary_snapshot(track, config)?)))?;
		Ok(Arc::new(Self {
			inner: std::sync::Mutex::new(Some(producer)),
		}))
	}

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
pub struct MoqFlateStreamProducer {
	inner: std::sync::Mutex<Option<moq_mux::binary::Stream<Extra>>>,
}

#[uniffi::export]
impl MoqFlateStreamProducer {
	/// Publish `track` as an opaque stream track (lossless append-log), advertised in `broadcast`'s catalog.
	///
	/// `track` must come from `broadcast`; otherwise the catalog advertises a track the broadcast
	/// does not carry.
	///
	/// Takes over `track`, whose handle is closed afterward. Errors if the catalog already
	/// carries an entry under the track's name.
	#[uniffi::constructor]
	pub fn new(
		broadcast: &MoqBroadcastProducer,
		track: &MoqTrackProducer,
		config: MoqFlateConfig,
	) -> Result<Arc<Self>, MoqError> {
		let _guard = crate::ffi::enter();
		let config = Config::from(config);
		let producer =
			track.adopt(|track| broadcast.with_state(|state| Ok(state.catalog.binary_stream(track, config)?)))?;
		Ok(Arc::new(Self {
			inner: std::sync::Mutex::new(Some(producer)),
		}))
	}

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
