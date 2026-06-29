//! Catalog stream trait.
//!
//! [`Stream`] yields a sequence of [`Catalog<E>`](super::hang::Catalog) snapshots. Both the
//! raw [`Consumer`](super::Consumer) and the rendition-selecting
//! [`Select`](super::Select) wrapper implement it, so exporters can be written
//! against the trait and the caller picks the selection policy.
//!
//! The yielded catalog carries the application extension `E` (defaulting to
//! `()` for media-only catalogs) via the [`Ext`](Stream::Ext) associated type,
//! so an exporter that only touches `video`/`audio` works for any extension.

use std::task::Poll;

use super::Select;
use super::hang::{Catalog, CatalogExt};

/// A stream of catalog snapshots.
///
/// `poll_next` returns the next snapshot (a full catalog, not a delta), or
/// `None` once the underlying track has ended. Late snapshots supersede
/// earlier ones, so an implementation may drop intermediate snapshots.
///
/// Stream types are required to be `Send + 'static` so they can be moved
/// across threads and held inside exporters without per-call bounds.
pub trait Stream: Send + 'static {
	/// The application extension carried by the yielded catalog (`()` for media-only).
	type Ext: CatalogExt;

	fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<crate::Result<Option<Catalog<Self::Ext>>>>;

	/// Wait for the next snapshot.
	fn next(&mut self) -> impl std::future::Future<Output = crate::Result<Option<Catalog<Self::Ext>>>> + Send
	where
		Self: Sized,
	{
		async move { kio::wait(|waiter| self.poll_next(waiter)).await }
	}

	/// Wrap this stream in a [`Select`] that drops every rendition `selection`
	/// doesn't keep.
	fn select(self, selection: crate::select::Broadcast) -> Select<Self>
	where
		Self: Sized,
	{
		Select::new(self, selection)
	}
}
