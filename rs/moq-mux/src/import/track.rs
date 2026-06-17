//! Bridge from the request-based single-track importers back to a broadcast catalog.
//!
//! A single-track importer (in [`crate::codec`]) produces frames on one track and
//! exposes the catalog renditions it publishes via [`Renditions`]. Most callers,
//! though, work with a whole [`moq_net::BroadcastProducer`] plus a shared
//! [`catalog::Producer`](crate::catalog::Producer). [`Track`] is the adapter:
//! it merges an importer's renditions into that catalog and removes them on drop.
//!
//! For the broadcast-push case, mint a track with [`unique_track`] and build the
//! importer `from_track`. A [`moq_net::TrackRequest`] (from
//! [`BroadcastDynamic::requested_track`](moq_net::BroadcastDynamic::requested_track))
//! is instead the on-demand path, fed directly to the importer's `new`.
//!
//! Some importers fill their catalog lazily (H.264 only knows its config once SPS
//! arrives) or refine it over time (jitter). Feed them through
//! [`Track::decode`] or [`Track::decoding`], which re-mirror the catalog
//! automatically so new/changed renditions always reach it.

use std::ops::{Deref, DerefMut};

use crate::catalog::hang::CatalogExt;

/// A single-track importer that exposes the catalog renditions it publishes.
///
/// Implemented by the per-codec importers so [`Track`] can merge their
/// renditions into a broadcast catalog generically. The returned catalog may be
/// empty (and grow later) for importers that initialize lazily.
pub trait Renditions {
	/// The standalone media catalog (video/audio renditions) this importer publishes.
	fn renditions(&self) -> &hang::Catalog;
}

/// A single-track importer that publishes already-split frames.
///
/// The uniform decode entry point: callers split bytes into [`Frame`](crate::container::Frame)s
/// (a per-format splitter, e.g. [`crate::codec::h264::Split`]) and hand them over.
/// [`Track`] wraps this so the catalog re-mirror can't be forgotten (see
/// [`Track::decode`]).
pub trait FrameDecode {
	/// Publish frames on this importer's track.
	fn decode<I: IntoIterator<Item = crate::container::Frame>>(&mut self, frames: I) -> crate::Result<()>;
}

/// Mint a fresh unique track for a legacy single-codec importer.
///
/// Picks a unique name from `suffix` and sets the microsecond
/// [`hang::container::TIMESCALE`] that the legacy importers stamp their frames
/// with, so the relay gets timing without parsing the payload. Hand the result
/// to the importer's `from_track`.
pub fn unique_track(broadcast: &mut moq_net::BroadcastProducer, suffix: &str) -> crate::Result<moq_net::TrackProducer> {
	let info = moq_net::TrackInfo::default().with_timescale(hang::container::TIMESCALE);
	Ok(broadcast.unique_track(suffix, info)?)
}

/// A single-track importer attached to a broadcast catalog.
///
/// Mirrors the importer's [`Renditions`] into a [`catalog::Producer`](crate::catalog::Producer)
/// and removes them on drop. Derefs to the inner importer, so all of its methods
/// (`decode`, `finish`, `seek`, ...) are available directly. Generic over the
/// catalog extension `E` so it can attach to an extended broadcast catalog (e.g.
/// the one a container holds).
pub struct Track<I: Renditions, E: CatalogExt = ()> {
	inner: I,
	catalog: crate::catalog::Producer<E>,
	/// The renditions we last mirrored into the catalog, so [`sync`](Self::sync)
	/// can diff against the importer's current state and retire them on drop.
	published: hang::Catalog,
}

impl<I: Renditions, E: CatalogExt> Track<I, E> {
	/// Attach `inner` to `catalog`, mirroring whatever renditions it already has.
	pub fn new(catalog: crate::catalog::Producer<E>, inner: I) -> Self {
		let mut this = Self {
			inner,
			catalog,
			published: hang::Catalog::default(),
		};
		this.sync();
		this
	}

	/// Re-mirror the importer's current renditions into the catalog.
	///
	/// Runs after each decode via [`decode`](Self::decode) / [`decoding`](Self::decoding),
	/// so callers never invoke it directly. A cheap comparison that touches the
	/// catalog only when a rendition actually appeared, changed, or was dropped.
	fn sync(&mut self) {
		let current = self.inner.renditions();
		if self.published == *current {
			return;
		}

		{
			let mut guard = self.catalog.lock();

			// Retire renditions we published before that the importer dropped.
			for name in self.published.video.renditions.keys() {
				if !current.video.renditions.contains_key(name) {
					guard.video.renditions.remove(name);
				}
			}
			for name in self.published.audio.renditions.keys() {
				if !current.audio.renditions.contains_key(name) {
					guard.audio.renditions.remove(name);
				}
			}

			// Insert or update the current ones.
			for (name, config) in &current.video.renditions {
				guard.video.renditions.insert(name.clone(), config.clone());
			}
			for (name, config) in &current.audio.renditions {
				guard.audio.renditions.insert(name.clone(), config.clone());
			}
		}

		self.published = current.clone();
	}

	/// Run a decode on the inner importer, then re-mirror the catalog.
	///
	/// The footgun-free wrapper for the byte-decode entry points (the importer's
	/// `decode_frame` / `decode_stream` / `initialize`): it re-mirrors the catalog
	/// after the closure returns, so a lazily-resolved config or refined jitter
	/// always reaches it. Prefer [`decode`](Self::decode) where the caller already
	/// has split frames.
	pub fn decoding<R>(&mut self, decode: impl FnOnce(&mut I) -> crate::Result<R>) -> crate::Result<R> {
		let out = decode(&mut self.inner)?;
		self.sync();
		Ok(out)
	}
}

impl<I: Renditions + FrameDecode, E: CatalogExt> Track<I, E> {
	/// Publish frames and re-mirror any catalog change in one call.
	///
	/// This is the footgun-free path: it re-mirrors the catalog after decoding, so
	/// a lazily-resolved config or refined jitter always reaches it.
	pub fn decode<It: IntoIterator<Item = crate::container::Frame>>(&mut self, frames: It) -> crate::Result<()> {
		self.inner.decode(frames)?;
		self.sync();
		Ok(())
	}
}

impl<I: Renditions, E: CatalogExt> Deref for Track<I, E> {
	type Target = I;

	fn deref(&self) -> &I {
		&self.inner
	}
}

impl<I: Renditions, E: CatalogExt> DerefMut for Track<I, E> {
	fn deref_mut(&mut self) -> &mut I {
		&mut self.inner
	}
}

impl<I: Renditions, E: CatalogExt> Drop for Track<I, E> {
	fn drop(&mut self) {
		if self.published == hang::Catalog::default() {
			return;
		}

		let mut guard = self.catalog.lock();
		for name in self.published.video.renditions.keys() {
			guard.video.renditions.remove(name);
		}
		for name in self.published.audio.renditions.keys() {
			guard.audio.renditions.remove(name);
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// An importer whose catalog we can mutate, to drive [`Track::sync`].
	struct Fake(hang::Catalog);

	impl Renditions for Fake {
		fn renditions(&self) -> &hang::Catalog {
			&self.0
		}
	}

	impl FrameDecode for Fake {
		fn decode<I: IntoIterator<Item = crate::container::Frame>>(&mut self, _frames: I) -> crate::Result<()> {
			// Simulate an importer resolving its config lazily while decoding.
			self.0.video.renditions.insert("v".to_string(), video());
			Ok(())
		}
	}

	fn video() -> hang::catalog::VideoConfig {
		let mut config = hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::VP8);
		config.container = hang::catalog::Container::Legacy;
		config
	}

	#[tokio::test(start_paused = true)]
	async fn sync_propagates_lazily_and_drop_retires() {
		let mut broadcast = moq_net::BroadcastInfo::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();

		// Importer starts with an empty catalog (lazy init): nothing merged yet.
		let mut published = Track::new(catalog.clone(), Fake(hang::Catalog::default()));
		assert!(catalog.snapshot().video.renditions.is_empty());

		// A rendition appears later; decoding mirrors it into the broadcast catalog.
		published
			.decoding(|i| {
				i.0.video.renditions.insert("v".to_string(), video());
				crate::Result::Ok(())
			})
			.unwrap();
		assert!(catalog.snapshot().video.renditions.contains_key("v"));

		// An update to the same rendition propagates too.
		published
			.decoding(|i| {
				i.0.video.renditions.get_mut("v").unwrap().bitrate = Some(1_000);
				crate::Result::Ok(())
			})
			.unwrap();
		assert_eq!(catalog.snapshot().video.renditions["v"].bitrate, Some(1_000));

		// Dropping the wrapper retires the rendition.
		drop(published);
		assert!(catalog.snapshot().video.renditions.is_empty());
	}

	#[tokio::test(start_paused = true)]
	async fn decode_auto_syncs_catalog() {
		let mut broadcast = moq_net::BroadcastInfo::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();

		let mut published = Track::new(catalog.clone(), Fake(hang::Catalog::default()));
		assert!(catalog.snapshot().video.renditions.is_empty());

		// `decode` resolves the rendition and mirrors it — no manual `sync()`.
		published.decode(std::iter::empty::<crate::container::Frame>()).unwrap();
		assert!(catalog.snapshot().video.renditions.contains_key("v"));
	}
}
