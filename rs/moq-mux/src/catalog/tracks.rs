use super::Producer;
use super::hang::CatalogExt;

/// A single video track's catalog rendition, retired on drop.
///
/// Made via [`Producer::video_track`]. An importer holds one and publishes its
/// rendition through it ([`set`](Self::set), refined in place with
/// [`update`](Self::update)). When the importer drops, the rendition is removed
/// from the shared catalog, so the broadcast catalog stays out of the importer's
/// type while still being published into.
pub struct VideoTrack<E: CatalogExt = ()> {
	catalog: Producer<E>,
	name: String,
	/// Whether a config has been published yet, so a lazily-configured importer
	/// (e.g. H.264 before its SPS) can hold the handle without a catalog entry, and
	/// drop without a spurious removal.
	present: bool,
}

impl<E: CatalogExt> VideoTrack<E> {
	pub(super) fn new(catalog: Producer<E>, name: impl Into<String>) -> Self {
		Self {
			catalog,
			name: name.into(),
			present: false,
		}
	}

	/// The track name this rendition is keyed by.
	pub fn name(&self) -> &str {
		&self.name
	}

	/// Resolve a timestamp on the broadcast's shared clock (see [`Producer::timestamp`]).
	pub fn timestamp(&self, hint: Option<moq_net::Timestamp>) -> crate::Result<moq_net::Timestamp> {
		self.catalog.timestamp(hint)
	}

	/// Insert or replace the rendition, publishing the catalog.
	pub fn set(&mut self, config: hang::catalog::VideoConfig) {
		self.catalog.lock().video.renditions.insert(self.name.clone(), config);
		self.present = true;
	}

	/// Refine the rendition in place (e.g. observed jitter), publishing if present.
	pub fn update(&mut self, f: impl FnOnce(&mut hang::catalog::VideoConfig)) {
		if !self.present {
			return;
		}
		let mut guard = self.catalog.lock();
		if let Some(config) = guard.video.renditions.get_mut(&self.name) {
			f(config);
		}
	}
}

impl<E: CatalogExt> Drop for VideoTrack<E> {
	fn drop(&mut self) {
		if self.present {
			self.catalog.lock().video.renditions.remove(&self.name);
		}
	}
}

/// A single audio track's catalog rendition, retired on drop.
///
/// The audio counterpart of [`VideoTrack`]; made via [`Producer::audio_track`].
pub struct AudioTrack<E: CatalogExt = ()> {
	catalog: Producer<E>,
	name: String,
	present: bool,
}

impl<E: CatalogExt> AudioTrack<E> {
	pub(super) fn new(catalog: Producer<E>, name: impl Into<String>) -> Self {
		Self {
			catalog,
			name: name.into(),
			present: false,
		}
	}

	/// The track name this rendition is keyed by.
	pub fn name(&self) -> &str {
		&self.name
	}

	/// Resolve a timestamp on the broadcast's shared clock (see [`Producer::timestamp`]).
	pub fn timestamp(&self, hint: Option<moq_net::Timestamp>) -> crate::Result<moq_net::Timestamp> {
		self.catalog.timestamp(hint)
	}

	/// Insert or replace the rendition, publishing the catalog.
	pub fn set(&mut self, config: hang::catalog::AudioConfig) {
		self.catalog.lock().audio.renditions.insert(self.name.clone(), config);
		self.present = true;
	}

	/// Refine the rendition in place (e.g. a synthesized description or jitter),
	/// publishing if present.
	pub fn update(&mut self, f: impl FnOnce(&mut hang::catalog::AudioConfig)) {
		if !self.present {
			return;
		}
		let mut guard = self.catalog.lock();
		if let Some(config) = guard.audio.renditions.get_mut(&self.name) {
			f(config);
		}
	}
}

impl<E: CatalogExt> Drop for AudioTrack<E> {
	fn drop(&mut self) {
		if self.present {
			self.catalog.lock().audio.renditions.remove(&self.name);
		}
	}
}
