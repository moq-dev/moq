//! Rendition-selecting catalog stream.
//!
//! [`Select`] wraps any [`Stream`] and applies a [`select::Broadcast`](crate::select::Broadcast)
//! to each snapshot, dropping renditions that aren't selected before handing the
//! catalog to an exporter.
//!
//! Protected renditions are dropped too (see [`Clear`](super::hang::Clear)), since nothing that
//! consumes a selection decrypts. A snapshot whose media is all protected reads as one with no
//! media yet.

use std::task::{Poll, ready};

use super::Stream;
use super::hang::Catalog;
use crate::select;

/// A [`Stream`] that keeps only the renditions a [`select::Broadcast`] selects.
///
/// The selection is fixed at construction; every snapshot from the inner stream is
/// narrowed by it. Build one with [`Stream::select`](super::Stream::select) or
/// [`Select::new`].
pub struct Select<S: Stream> {
	inner: S,
	selection: select::Broadcast,
	clear: super::hang::Clear,
}

impl<S: Stream> Select<S> {
	/// Wrap `inner`, narrowing every snapshot by `selection`.
	pub fn new(inner: S, selection: select::Broadcast) -> Self {
		Self {
			inner,
			selection,
			clear: Default::default(),
		}
	}
}

impl<S: Stream> Stream for Select<S> {
	type Ext = S::Ext;

	fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<crate::Result<Option<Catalog<S::Ext>>>> {
		let Some(mut catalog) = ready!(self.inner.poll_next(waiter))? else {
			return Poll::Ready(Ok(None));
		};
		self.clear.retain(&mut catalog);
		self.selection.retain(&mut catalog);
		Poll::Ready(Ok(Some(catalog)))
	}
}

#[cfg(test)]
mod tests {
	use std::collections::BTreeMap;

	use hang::catalog::{Container, H264, VideoConfig};

	use super::super::hang::Catalog;
	use super::*;

	/// A stream that yields its snapshots in order, then ends.
	struct Snapshots(std::collections::VecDeque<Catalog>);

	impl Snapshots {
		fn new(snapshots: impl IntoIterator<Item = Catalog>) -> Self {
			Self(snapshots.into_iter().collect())
		}
	}

	impl Stream for Snapshots {
		type Ext = ();

		fn poll_next(&mut self, _: &kio::Waiter) -> Poll<crate::Result<Option<Catalog>>> {
			Poll::Ready(Ok(self.0.pop_front()))
		}
	}

	fn next(stream: &mut impl Stream<Ext = ()>) -> Catalog {
		match stream.poll_next(&kio::Waiter::noop()) {
			Poll::Ready(Ok(Some(catalog))) => catalog,
			other => panic!("expected snapshot, got {other:?}"),
		}
	}

	fn h264(name: &str) -> (String, VideoConfig) {
		let mut config = VideoConfig::new(H264 {
			profile: 0x42,
			constraints: 0,
			level: 0x1e,
			inline: false,
		});
		config.container = Container::Legacy;
		(name.to_string(), config)
	}

	/// A protected rendition is dropped, and a snapshot whose media is all protected reads as one
	/// with no media yet, so a clear rendition published later is still exported.
	#[test]
	fn protected_renditions_are_dropped() {
		let mut catalog = Catalog::default();
		let (name, mut protected) = h264("protected");
		protected.drm = vec!["1".to_string()];
		catalog.video.renditions.insert(name, protected);
		let only_protected = catalog.clone();
		let (name, clear) = h264("clear");
		catalog.video.renditions.insert(name, clear);

		let selection = select::Broadcast::default().video(select::Video::default());
		let mut stream = Snapshots::new([only_protected, catalog]).select(selection);
		assert!(next(&mut stream).video.renditions.is_empty());
		assert_eq!(next(&mut stream).video.renditions.keys().collect::<Vec<_>>(), ["clear"]);
	}

	#[test]
	fn narrows_each_snapshot() {
		let mut catalog = Catalog::default();
		catalog.video.renditions = BTreeMap::from_iter(vec![h264("lo"), h264("hi")]);

		let selection = select::Broadcast::default().video(select::Video::default().name("hi"));
		let mut stream = Snapshots::new([catalog]).select(selection);

		let out = match stream.poll_next(&kio::Waiter::noop()) {
			Poll::Ready(Ok(Some(c))) => c,
			other => panic!("expected snapshot, got {other:?}"),
		};
		assert_eq!(out.video.renditions.keys().collect::<Vec<_>>(), vec!["hi"]);
		assert!(matches!(stream.poll_next(&kio::Waiter::noop()), Poll::Ready(Ok(None))));
	}

	#[test]
	fn selects_boxed_stream() {
		let mut catalog = Catalog::default();
		catalog.video.renditions = BTreeMap::from_iter(vec![h264("lo"), h264("hi")]);

		let boxed: Box<dyn Stream<Ext = ()>> = Box::new(Snapshots::new([catalog]));
		let selection = select::Broadcast::default().video(select::Video::default().name("lo"));
		let mut stream = boxed.select(selection);

		let out = match stream.poll_next(&kio::Waiter::noop()) {
			Poll::Ready(Ok(Some(c))) => c,
			other => panic!("expected snapshot, got {other:?}"),
		};
		assert_eq!(out.video.renditions.keys().collect::<Vec<_>>(), vec!["lo"]);
		assert!(matches!(stream.poll_next(&kio::Waiter::noop()), Poll::Ready(Ok(None))));
	}
}
