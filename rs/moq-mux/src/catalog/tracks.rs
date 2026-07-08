use super::Producer;
use super::hang::CatalogExt;
use crate::container::Timestamp;
use crate::container::jitter::Metrics;

/// Overwrite a catalog `jitter` field with a newly detected value.
fn set_jitter(field: &mut Option<moq_net::Time>, jitter: std::time::Duration) {
	*field = moq_net::Time::try_from(jitter).ok();
}

/// Raise a catalog `bitrate` field, never lowering a larger declared or previously seen value.
fn raise_bitrate(field: &mut Option<u64>, bitrate: u64) {
	if field.is_none_or(|current| bitrate > current) {
		*field = Some(bitrate);
	}
}

/// A single video track's catalog rendition, retired on drop.
///
/// Made via [`Producer::video_track`]. An importer holds one and publishes its
/// rendition through it ([`set`](Self::set), refined in place with
/// [`update`](Self::update)). When the importer drops, the rendition is removed
/// from the shared catalog, so the broadcast catalog stays out of the importer's
/// type while still being published into.
///
/// The rendition also owns a [`Metrics`] detector: feed it frames with
/// [`observe_frame`](Self::observe_frame) / [`observe_reorder`](Self::observe_reorder) and group
/// boundaries with [`finish_group`](Self::finish_group), and it keeps the catalog's `jitter` and
/// `bitrate` current, republishing only when a value moves.
pub struct VideoTrack<E: CatalogExt = ()> {
	catalog: Producer<E>,
	name: String,
	/// Whether a config has been published yet, so a lazily-configured importer
	/// (e.g. H.264 before its SPS) can hold the handle without a catalog entry, and
	/// drop without a spurious removal.
	present: bool,
	metrics: Metrics,
}

impl<E: CatalogExt> VideoTrack<E> {
	pub(super) fn new(catalog: Producer<E>, name: impl Into<String>) -> Self {
		Self {
			catalog,
			name: name.into(),
			present: false,
			metrics: Metrics::new(),
		}
	}

	/// The track name this rendition is keyed by.
	pub fn name(&self) -> &str {
		&self.name
	}

	/// Resolve a timestamp on the broadcast's shared clock (see [`Producer::timestamp`]).
	pub fn timestamp(&self, hint: Option<crate::container::Timestamp>) -> crate::Result<crate::container::Timestamp> {
		self.catalog.timestamp(hint)
	}

	/// Insert or replace the rendition, publishing the catalog.
	///
	/// Seeds `config` with any metrics already accumulated: a dirty start or a B-frame reorder
	/// can feed observations before the rendition exists, which would otherwise be lost.
	pub fn set(&mut self, mut config: hang::catalog::VideoConfig) {
		if let Some(jitter) = self.metrics.jitter() {
			set_jitter(&mut config.jitter, jitter);
		}
		if let Some(bitrate) = self.metrics.bitrate() {
			raise_bitrate(&mut config.bitrate, bitrate);
		}
		self.catalog.lock().video.renditions.insert(self.name.clone(), config);
		self.present = true;
	}

	/// Refine the rendition in place (e.g. a synthesized description), publishing if present.
	pub fn update(&mut self, f: impl FnOnce(&mut hang::catalog::VideoConfig)) {
		if !self.present {
			return;
		}
		let mut guard = self.catalog.lock();
		if let Some(config) = guard.video.renditions.get_mut(&self.name) {
			f(config);
		}
	}

	/// Record one frame (presentation timestamp + encoded size), republishing the jitter if it changed.
	pub fn observe_frame(&mut self, ts: Timestamp, bytes: usize) {
		if let Some(jitter) = self.metrics.observe_frame(ts, bytes) {
			self.update(|config| set_jitter(&mut config.jitter, jitter));
		}
	}

	/// Record a frame's reorder delay (`PTS - DTS`), republishing the jitter if it changed.
	pub fn observe_reorder(&mut self, reorder: Timestamp) {
		if let Some(jitter) = self.metrics.observe_reorder(reorder) {
			self.update(|config| set_jitter(&mut config.jitter, jitter));
		}
	}

	/// Close the current group (`next` is its end timestamp when known), republishing the bitrate if it rose.
	pub fn finish_group(&mut self, next: Option<Timestamp>) {
		if let Some(bitrate) = self.metrics.finish_group(next) {
			self.update(|config| raise_bitrate(&mut config.bitrate, bitrate));
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
/// The audio counterpart of [`VideoTrack`]; made via [`Producer::audio_track`]. Audio has no
/// B-frame reorder, so it exposes [`observe_frame`](Self::observe_frame) and
/// [`finish_group`](Self::finish_group) but no `observe_reorder`.
pub struct AudioTrack<E: CatalogExt = ()> {
	catalog: Producer<E>,
	name: String,
	present: bool,
	metrics: Metrics,
}

impl<E: CatalogExt> AudioTrack<E> {
	pub(super) fn new(catalog: Producer<E>, name: impl Into<String>) -> Self {
		Self {
			catalog,
			name: name.into(),
			present: false,
			metrics: Metrics::new(),
		}
	}

	/// The track name this rendition is keyed by.
	pub fn name(&self) -> &str {
		&self.name
	}

	/// Resolve a timestamp on the broadcast's shared clock (see [`Producer::timestamp`]).
	pub fn timestamp(&self, hint: Option<crate::container::Timestamp>) -> crate::Result<crate::container::Timestamp> {
		self.catalog.timestamp(hint)
	}

	/// Insert or replace the rendition, publishing the catalog.
	///
	/// Seeds `config` with any metrics already accumulated before the rendition existed.
	pub fn set(&mut self, mut config: hang::catalog::AudioConfig) {
		if let Some(jitter) = self.metrics.jitter() {
			set_jitter(&mut config.jitter, jitter);
		}
		if let Some(bitrate) = self.metrics.bitrate() {
			raise_bitrate(&mut config.bitrate, bitrate);
		}
		self.catalog.lock().audio.renditions.insert(self.name.clone(), config);
		self.present = true;
	}

	/// Refine the rendition in place (e.g. a synthesized description), publishing if present.
	pub fn update(&mut self, f: impl FnOnce(&mut hang::catalog::AudioConfig)) {
		if !self.present {
			return;
		}
		let mut guard = self.catalog.lock();
		if let Some(config) = guard.audio.renditions.get_mut(&self.name) {
			f(config);
		}
	}

	/// Record one frame (presentation timestamp + encoded size), republishing the jitter if it changed.
	pub fn observe_frame(&mut self, ts: Timestamp, bytes: usize) {
		if let Some(jitter) = self.metrics.observe_frame(ts, bytes) {
			self.update(|config| set_jitter(&mut config.jitter, jitter));
		}
	}

	/// Close the current group (`next` is its end timestamp when known), republishing the bitrate if it rose.
	pub fn finish_group(&mut self, next: Option<Timestamp>) {
		if let Some(bitrate) = self.metrics.finish_group(next) {
			self.update(|config| raise_bitrate(&mut config.bitrate, bitrate));
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
