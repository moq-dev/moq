use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::{Catalog, CatalogConsumer};

/// Produces a catalog track that describes the available media tracks.
///
/// The JSON catalog is updated when tracks are added/removed but is *not* automatically published.
/// You'll have to call [`lock`](Self::lock) to update and publish the catalog.
#[derive(Clone)]
pub struct CatalogProducer {
	/// Access to the underlying track producer.
	pub track: moq_lite::TrackProducer,
	current: Arc<Mutex<Catalog>>,
}

impl CatalogProducer {
	/// Create a new catalog producer with the given track and initial catalog.
	pub fn new(track: moq_lite::TrackProducer, init: Catalog) -> Self {
		Self {
			current: Arc::new(Mutex::new(init)),
			track,
		}
	}

	/// Get mutable access to the catalog, publishing it after any changes.
	pub fn lock(&mut self) -> CatalogGuard<'_> {
		CatalogGuard {
			catalog: self.current.lock().unwrap(),
			track: &mut self.track,
			updated: false,
		}
	}

	/// Create a consumer for this catalog, receiving updates as they're published.
	pub fn consume(&self) -> CatalogConsumer {
		CatalogConsumer::new(self.track.consume())
	}

	/// Finish publishing to this catalog and close the track.
	pub fn close(self) {
		self.track.close();
	}
}

impl From<moq_lite::TrackProducer> for CatalogProducer {
	fn from(inner: moq_lite::TrackProducer) -> Self {
		Self::new(inner, Catalog::default())
	}
}

impl Default for CatalogProducer {
	fn default() -> Self {
		Self::new(Catalog::default_track().produce(), Catalog::default())
	}
}

/// RAII guard for modifying a catalog with automatic publishing on drop.
///
/// Obtained via [`CatalogProducer::lock`].
pub struct CatalogGuard<'a> {
	catalog: MutexGuard<'a, Catalog>,
	track: &'a mut moq_lite::TrackProducer,
	updated: bool,
}

impl<'a> Deref for CatalogGuard<'a> {
	type Target = Catalog;

	fn deref(&self) -> &Self::Target {
		&self.catalog
	}
}

impl<'a> DerefMut for CatalogGuard<'a> {
	fn deref_mut(&mut self) -> &mut Self::Target {
		self.updated = true;
		&mut self.catalog
	}
}

impl Drop for CatalogGuard<'_> {
	fn drop(&mut self) {
		// Avoid publishing if we didn't use `&mut self` at all.
		if !self.updated {
			return;
		}

		let mut group = self.track.append_group();

		// TODO decide if this should return an error, or be impossible to fail
		let frame = self.catalog.to_string().expect("invalid catalog");
		group.write_frame(frame);
		group.close();
	}
}
