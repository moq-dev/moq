use std::marker::PhantomData;

use super::Producer;
use super::hang::{Catalog, CatalogExt};

mod sealed {
	pub trait Sealed {}
}

/// The media kind of a reserved rendition, selecting its config type and catalog slot.
///
/// Implemented only by [`Video`] and [`Audio`]; used as the `K` in
/// [`Rendition`] and [`Reserved::init`].
pub trait Kind: sealed::Sealed + 'static {
	/// The catalog config type carried by this kind ([`VideoConfig`](hang::catalog::VideoConfig)
	/// or [`AudioConfig`](hang::catalog::AudioConfig)).
	type Config;

	#[doc(hidden)]
	fn insert<E: CatalogExt>(catalog: &mut Catalog<E>, name: &str, config: Self::Config);
	#[doc(hidden)]
	fn with_mut<E: CatalogExt>(catalog: &mut Catalog<E>, name: &str, f: impl FnOnce(&mut Self::Config));
	#[doc(hidden)]
	fn remove<E: CatalogExt>(catalog: &mut Catalog<E>, name: &str);
}

/// Video rendition marker for [`Rendition`] / [`Reserved::init`].
pub enum Video {}
/// Audio rendition marker for [`Rendition`] / [`Reserved::init`].
pub enum Audio {}

impl sealed::Sealed for Video {}
impl sealed::Sealed for Audio {}

impl Kind for Video {
	type Config = hang::catalog::VideoConfig;

	fn insert<E: CatalogExt>(catalog: &mut Catalog<E>, name: &str, config: Self::Config) {
		catalog.video.renditions.insert(name.to_string(), config);
	}
	fn with_mut<E: CatalogExt>(catalog: &mut Catalog<E>, name: &str, f: impl FnOnce(&mut Self::Config)) {
		if let Some(config) = catalog.video.renditions.get_mut(name) {
			f(config);
		}
	}
	fn remove<E: CatalogExt>(catalog: &mut Catalog<E>, name: &str) {
		catalog.video.renditions.remove(name);
	}
}

impl Kind for Audio {
	type Config = hang::catalog::AudioConfig;

	fn insert<E: CatalogExt>(catalog: &mut Catalog<E>, name: &str, config: Self::Config) {
		catalog.audio.renditions.insert(name.to_string(), config);
	}
	fn with_mut<E: CatalogExt>(catalog: &mut Catalog<E>, name: &str, f: impl FnOnce(&mut Self::Config)) {
		if let Some(config) = catalog.audio.renditions.get_mut(name) {
			f(config);
		}
	}
	fn remove<E: CatalogExt>(catalog: &mut Catalog<E>, name: &str) {
		catalog.audio.renditions.remove(name);
	}
}

/// A clonable reservation context handed to importers so they declare their tracks up front.
///
/// Made via [`Producer::reserve`]. While any `Reserved` clone is alive the track set may still
/// grow, so the catalog is withheld from the broadcast. Each [`init`](Self::init) reserves a
/// rendition by name (config filled in later via the returned [`Rendition`] guard) and counts as
/// outstanding until that guard is fulfilled or dropped. Once every clone is dropped *and* every
/// reservation resolves, the first catalog snapshot is published atomically with the complete
/// track list, so a one-shot muxer (fMP4, MPEG-TS) never sees a half-converged catalog.
pub struct Reserved<E: CatalogExt = ()> {
	catalog: Producer<E>,
}

impl<E: CatalogExt> Reserved<E> {
	pub(super) fn new(catalog: Producer<E>) -> Self {
		catalog.add_reserver();
		Self { catalog }
	}

	/// Reserve a rendition of kind `K` under `name`, returning a guard to fill it in.
	///
	/// The guard holds its own `Reserved` clone, so the catalog stays withheld until the returned
	/// [`Rendition`] is [`set`](Rendition::set) (or dropped). Prefer [`video`](Self::video) /
	/// [`audio`](Self::audio) at call sites.
	pub fn init<K: Kind>(&self, name: impl Into<String>) -> Rendition<E, K> {
		Rendition::new(self.clone(), name)
	}

	/// Reserve a video rendition; shorthand for [`init::<Video>`](Self::init).
	pub fn video(&self, name: impl Into<String>) -> Rendition<E, Video> {
		self.init::<Video>(name)
	}

	/// Reserve an audio rendition; shorthand for [`init::<Audio>`](Self::init).
	pub fn audio(&self, name: impl Into<String>) -> Rendition<E, Audio> {
		self.init::<Audio>(name)
	}

	/// Resolve a timestamp on the broadcast's shared clock (see [`Producer::timestamp`]).
	pub fn timestamp(&self, hint: Option<moq_net::Timestamp>) -> crate::Result<moq_net::Timestamp> {
		self.catalog.timestamp(hint)
	}

	/// The underlying catalog [`Producer`], for edits that outlive this reservation.
	///
	/// A container importer holds one to edit the catalog directly (e.g. its per-frame reconcile, or
	/// track removals after its initial set is declared) while the reservation itself is dropped to
	/// open the gate. The returned handle does not gate: only live `Reserved`s do.
	pub fn producer(&self) -> Producer<E> {
		self.catalog.clone()
	}
}

impl<E: CatalogExt> Clone for Reserved<E> {
	fn clone(&self) -> Self {
		self.catalog.add_reserver();
		Self {
			catalog: self.catalog.clone(),
		}
	}
}

impl<E: CatalogExt> Drop for Reserved<E> {
	fn drop(&mut self) {
		self.catalog.release_reserver();
	}
}

/// A reserved rendition of kind `K`, retired from the catalog on drop.
///
/// Made via [`Reserved::init`] (or [`video`](Reserved::video) / [`audio`](Reserved::audio)). Fill
/// it in with [`set`](Self::set) and refine it in place with [`update`](Self::update). Until it's
/// set (or dropped) it holds a [`Reserved`] clone, so an unresolved rendition keeps the initial
/// catalog publish gated. On drop the rendition is removed from the shared catalog.
pub struct Rendition<E: CatalogExt, K: Kind> {
	catalog: Producer<E>,
	name: String,
	/// The reservation this rendition holds until its config is set (or it's dropped unfulfilled).
	/// `Some` gates the initial publish; cleared by [`set`](Self::set).
	gate: Option<Reserved<E>>,
	/// Whether a config has been published, so a lazily-configured importer (e.g. H.264 before its
	/// SPS) holds the handle without a catalog entry, and drops without a spurious removal.
	present: bool,
	_kind: PhantomData<fn() -> K>,
}

/// A single video track's catalog rendition. See [`Rendition`].
pub type VideoTrack<E = ()> = Rendition<E, Video>;
/// A single audio track's catalog rendition. See [`Rendition`].
pub type AudioTrack<E = ()> = Rendition<E, Audio>;

impl<E: CatalogExt, K: Kind> Rendition<E, K> {
	fn new(reserved: Reserved<E>, name: impl Into<String>) -> Self {
		Self {
			catalog: reserved.catalog.clone(),
			gate: Some(reserved),
			name: name.into(),
			present: false,
			_kind: PhantomData,
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

	/// Insert or replace the rendition, fulfilling the reservation and publishing the catalog.
	pub fn set(&mut self, config: K::Config) {
		// Write the config first (still withheld, since we're holding our reservation), then release
		// the reservation. If this was the last one, the release flushes a complete snapshot.
		{
			let mut guard = self.catalog.lock();
			K::insert(&mut guard, &self.name, config);
		}
		self.present = true;
		self.gate = None;
	}

	/// Refine the rendition in place (e.g. observed jitter), publishing if present.
	pub fn update(&mut self, f: impl FnOnce(&mut K::Config)) {
		if !self.present {
			return;
		}
		let mut guard = self.catalog.lock();
		K::with_mut(&mut guard, &self.name, f);
	}
}

impl<E: CatalogExt, K: Kind> Drop for Rendition<E, K> {
	fn drop(&mut self) {
		if self.present {
			// Removing mutates the catalog, so the guard publishes it (immediately if live, else it
			// accumulates until the gate opens).
			let mut guard = self.catalog.lock();
			K::remove(&mut guard, &self.name);
		}
		// Our reservation (`gate`) drops here. If still held (never set), its release flushes any
		// staged change; if already released by `set`, this is a no-op.
	}
}
