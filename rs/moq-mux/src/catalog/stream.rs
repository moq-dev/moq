//! Catalog stream trait.
//!
//! [`Stream`] yields a sequence of [`hang::Catalog`] snapshots. Both the
//! raw [`Consumer`](super::Consumer) and the rendition-selecting
//! [`Filter`](super::Filter) / [`Target`](super::Target) wrappers implement
//! it, so exporters can be written against the trait and the caller picks
//! the selection policy.

use std::task::Poll;

use hang::Catalog;

use super::{Filter, Target};

/// A stream of catalog snapshots.
///
/// `poll_next` returns the next snapshot (a full catalog, not a delta), or
/// `None` once the underlying track has ended. Late snapshots supersede
/// earlier ones, so an implementation may drop intermediate snapshots.
///
/// Stream types are required to be `Send + 'static` so they can be moved
/// across threads and held inside exporters without per-call bounds.
pub trait Stream: Send + 'static {
	fn poll_next(&mut self, waiter: &conducer::Waiter) -> Poll<crate::Result<Option<Catalog>>>;

	/// Wait for the next snapshot.
	fn next(&mut self) -> impl std::future::Future<Output = crate::Result<Option<Catalog>>> + Send
	where
		Self: Sized,
	{
		async move { conducer::wait(|waiter| self.poll_next(waiter)).await }
	}

	/// Wrap this stream in a [`Filter`] that drops renditions which don't
	/// match a hard-match criterion (name or codec family).
	fn filter(self) -> Filter<Self>
	where
		Self: Sized,
	{
		Filter::new(self)
	}

	/// Wrap this stream in a [`Target`] that reduces each axis to at most
	/// one rendition by soft-matching against width / height / bitrate.
	fn target(self) -> Target<Self>
	where
		Self: Sized,
	{
		Target::new(self)
	}
}
