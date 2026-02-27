use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::{Catalog, CatalogConsumer, Error};

/// A mirror track that receives content derived from the catalog.
struct Mirror {
	track: moq_lite::TrackProducer,
	convert: Box<dyn Fn(&Catalog) -> String + Send + Sync>,
}

struct CatalogInner {
	catalog: Catalog,
	mirrors: Vec<Mirror>,
}

/// Produces a catalog track that describes the available media tracks.
///
/// The JSON catalog is updated when tracks are added/removed but is *not* automatically published.
/// You'll have to call [`lock`](Self::lock) to update and publish the catalog.
///
/// Additional mirror tracks can be registered via [`add_mirror`](Self::add_mirror) to
/// automatically publish derived catalog formats (e.g. MSF) whenever the catalog changes.
#[derive(Clone)]
pub struct CatalogProducer {
	/// Access to the underlying catalog track producer.
	pub track: moq_lite::TrackProducer,

	inner: Arc<Mutex<CatalogInner>>,
}

impl CatalogProducer {
	/// Create a new catalog producer with the given track and initial catalog.
	pub fn new(track: moq_lite::TrackProducer, init: Catalog) -> Self {
		Self {
			track,
			inner: Arc::new(Mutex::new(CatalogInner {
				catalog: init,
				mirrors: Vec::new(),
			})),
		}
	}

	/// Register a mirror track that receives content derived from the catalog.
	///
	/// Whenever the catalog changes, the conversion function is called and the
	/// result is published to the mirror track.
	pub fn add_mirror(
		&self,
		track: moq_lite::TrackProducer,
		convert: impl Fn(&Catalog) -> String + Send + Sync + 'static,
	) {
		self.inner.lock().unwrap().mirrors.push(Mirror {
			track,
			convert: Box::new(convert),
		});
	}

	/// Get mutable access to the catalog, publishing it after any changes.
	pub fn lock(&mut self) -> CatalogGuard<'_> {
		CatalogGuard {
			inner: self.inner.lock().unwrap(),
			track: &mut self.track,
			updated: false,
		}
	}

	/// Create a consumer for this catalog, receiving updates as they're published.
	pub fn consume(&self) -> CatalogConsumer {
		CatalogConsumer::new(self.track.consume())
	}

	/// Finish publishing to this catalog and all mirror tracks.
	pub fn finish(&mut self) -> Result<(), Error> {
		self.track.finish()?;
		let mut inner = self.inner.lock().unwrap();
		for mirror in &mut inner.mirrors {
			mirror.track.finish()?;
		}
		Ok(())
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
///
/// On drop, the catalog track and all registered mirror tracks are updated
/// if the catalog was mutated.
pub struct CatalogGuard<'a> {
	inner: MutexGuard<'a, CatalogInner>,
	track: &'a mut moq_lite::TrackProducer,
	updated: bool,
}

impl<'a> Deref for CatalogGuard<'a> {
	type Target = Catalog;

	fn deref(&self) -> &Self::Target {
		&self.inner.catalog
	}
}

impl<'a> DerefMut for CatalogGuard<'a> {
	fn deref_mut(&mut self) -> &mut Self::Target {
		self.updated = true;
		&mut self.inner.catalog
	}
}

impl Drop for CatalogGuard<'_> {
	fn drop(&mut self) {
		// Avoid publishing if we didn't use `&mut self` at all.
		if !self.updated {
			return;
		}

		let CatalogInner { catalog, mirrors } = &mut *self.inner;

		// Publish the catalog.
		let Ok(mut group) = self.track.append_group() else {
			return;
		};

		// TODO decide if this should return an error, or be impossible to fail
		let frame = catalog.to_string().expect("invalid catalog");
		let _ = group.write_frame(frame);
		let _ = group.finish();

		// Publish to all mirror tracks.
		for mirror in mirrors {
			let Ok(mut group) = mirror.track.append_group() else {
				continue;
			};
			let frame = (mirror.convert)(catalog);
			let _ = group.write_frame(frame);
			let _ = group.finish();
		}
	}
}
