//! Rendition-selecting catalog stream.
//!
//! [`Select`] wraps any [`Stream`] and applies a [`select::Broadcast`](crate::select::Broadcast)
//! to each snapshot, dropping renditions that aren't selected before handing the
//! catalog to an exporter.

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
}

impl<S: Stream> Select<S> {
	/// Wrap `inner`, narrowing every snapshot by `selection`.
	pub fn new(inner: S, selection: select::Broadcast) -> Self {
		Self { inner, selection }
	}
}

impl<S: Stream> Stream for Select<S> {
	type Ext = S::Ext;

	fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<crate::Result<Option<Catalog<S::Ext>>>> {
		let next = ready!(self.inner.poll_next(waiter))?;
		Poll::Ready(Ok(next.map(|mut catalog| {
			self.selection.retain(&mut catalog);
			catalog
		})))
	}
}

#[cfg(test)]
mod tests {
	use std::collections::BTreeMap;

	use hang::catalog::{Container, H264, VideoConfig};

	use super::super::hang::Catalog;
	use super::*;

	/// A one-shot stream: yields its snapshot once, then ends.
	struct Once(Option<Catalog>);

	impl Stream for Once {
		type Ext = ();

		fn poll_next(&mut self, _: &kio::Waiter) -> Poll<crate::Result<Option<Catalog>>> {
			Poll::Ready(Ok(self.0.take()))
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

	#[test]
	fn narrows_each_snapshot() {
		let mut catalog = Catalog::default();
		catalog.video.renditions = BTreeMap::from_iter(vec![h264("lo"), h264("hi")]);

		let selection = select::Broadcast::default().video(select::Video::default().name("hi"));
		let mut stream = Once(Some(catalog)).select(selection);

		let out = match stream.poll_next(&kio::Waiter::noop()) {
			Poll::Ready(Ok(Some(c))) => c,
			other => panic!("expected snapshot, got {other:?}"),
		};
		assert_eq!(out.video.renditions.keys().collect::<Vec<_>>(), vec!["hi"]);
		assert!(matches!(stream.poll_next(&kio::Waiter::noop()), Poll::Ready(Ok(None))));
	}
}
