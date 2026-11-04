//! Rendition-selecting catalog stream.
//!
//! [`Select`] wraps any [`Stream`] and applies a [`select::Broadcast`](crate::select::Broadcast)
//! to each snapshot, dropping renditions that aren't selected before handing the
//! catalog on. Each snapshot holds what the previous one selected, so a rendition its
//! publisher disables stays while nothing else of its role is enabled.
//!
//! Protected renditions (see [`retain_clear`](super::hang::retain_clear)) are dropped before
//! selecting, since nothing that consumes a selection decrypts, and a snapshot whose video and
//! audio are all protected fails with [`Error::Protected`](crate::Error::Protected).

use std::task::{Poll, ready};

use super::Stream;
use super::hang::Catalog;
use crate::select;

/// A [`Stream`] that keeps only the renditions a [`select::Broadcast`] selects.
///
/// The selection is fixed at construction; every snapshot from the inner stream is
/// narrowed by it, holding the renditions the previous snapshot selected (see
/// [`select::Broadcast::retain`]). Build one with [`Stream::select`](super::Stream::select) or
/// [`Select::new`].
pub struct Select<S: Stream> {
	inner: S,
	selection: select::Broadcast,
	/// What the last snapshot selected.
	selected: select::Selected,
}

impl<S: Stream> Select<S> {
	/// Wrap `inner`, narrowing every snapshot by `selection`.
	pub fn new(inner: S, selection: select::Broadcast) -> Self {
		Self {
			inner,
			selection,
			selected: select::Selected::default(),
		}
	}
}

impl<S: Stream> Stream for Select<S> {
	type Ext = S::Ext;

	fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<crate::Result<Option<Catalog<S::Ext>>>> {
		let Some(mut catalog) = ready!(self.inner.poll_next(waiter))? else {
			return Poll::Ready(Ok(None));
		};
		// Nothing downstream of a selection decrypts, so a protected rendition is absent before it
		// is chosen from: otherwise one could displace a clear rendition, then be dropped.
		let media = |catalog: &Catalog<S::Ext>| catalog.video.renditions.len() + catalog.audio.renditions.len();
		let listed = media(&catalog);
		super::hang::retain_clear(&mut catalog);
		// A broadcast whose media is all protected can never export anything; say so rather than
		// export nothing and succeed.
		if listed > 0 && media(&catalog) == 0 {
			return Poll::Ready(Err(crate::Error::Protected));
		}
		self.selection.retain(&mut catalog, &self.selected);
		self.selected = select::Selected::of(&catalog);
		Poll::Ready(Ok(Some(catalog)))
	}
}

#[cfg(test)]
mod tests {
	use std::collections::BTreeMap;

	use hang::catalog::{Container, H264, VideoConfig};

	use super::super::hang::Catalog;
	use super::*;

	/// A finite stream: yields its snapshots in order, then ends.
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
			Poll::Ready(Ok(Some(c))) => c,
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

	/// A protected rendition is dropped before selecting, so a held clear rendition the publisher
	/// disabled is not passed over for an enabled protected one that is then removed too.
	#[test]
	fn protected_renditions_never_displace_a_clear_one() {
		let snapshot = |enabled: bool| {
			let mut catalog = Catalog::default();
			let (name, mut clear) = h264("clear");
			clear.enabled = enabled;
			catalog.video.renditions.insert(name, clear);
			let (name, mut protected) = h264("protected");
			protected.drm = vec!["1".to_string()];
			catalog.video.renditions.insert(name, protected);
			catalog
		};
		let selection = select::Broadcast::default().video(select::Video::default());
		let mut stream = Snapshots::new([snapshot(true), snapshot(false)]).select(selection);
		assert_eq!(next(&mut stream).video.renditions.keys().collect::<Vec<_>>(), ["clear"]);
		assert_eq!(
			next(&mut stream).video.renditions.keys().collect::<Vec<_>>(),
			["clear"],
			"held while disabled"
		);
	}

	/// A broadcast whose media is all protected fails rather than exporting nothing; one with no
	/// media yet just waits for it.
	#[test]
	fn only_protected_media_is_an_error() {
		let (name, mut protected) = h264("protected");
		protected.drm = vec!["1".to_string()];
		let mut catalog = Catalog::default();
		catalog.video.renditions.insert(name, protected);

		let selection = select::Broadcast::default().video(select::Video::default());
		let mut stream = Snapshots::new([Catalog::default(), catalog]).select(selection);
		assert!(next(&mut stream).video.renditions.is_empty());
		assert!(matches!(
			stream.poll_next(&kio::Waiter::noop()),
			Poll::Ready(Err(crate::Error::Protected))
		));
	}

	#[test]
	fn narrows_each_snapshot() {
		let mut catalog = Catalog::default();
		catalog.video.renditions = BTreeMap::from_iter(vec![h264("lo"), h264("hi")]);

		let selection = select::Broadcast::default().video(select::Video::default().name("hi"));
		let mut stream = Snapshots::new([catalog]).select(selection);

		let out = next(&mut stream);
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

		let out = next(&mut stream);
		assert_eq!(out.video.renditions.keys().collect::<Vec<_>>(), vec!["lo"]);
		assert!(matches!(stream.poll_next(&kio::Waiter::noop()), Poll::Ready(Ok(None))));
	}

	/// A selected rendition its publisher disables stays selected until it re-enables, while one
	/// disabled before it was ever selected is skipped.
	#[test]
	fn holds_what_the_previous_snapshot_selected() {
		let snapshot = |enabled: bool| {
			let mut catalog = Catalog::default();
			let (name, mut config) = h264("v");
			config.enabled = enabled;
			catalog.video.renditions.insert(name, config);
			catalog
		};
		let selection = select::Broadcast::default().video(select::Video::default());

		let mut stream =
			Snapshots::new([snapshot(false), snapshot(true), snapshot(false), snapshot(true)]).select(selection);
		assert!(
			next(&mut stream).video.renditions.is_empty(),
			"disabled before it was selected"
		);
		assert!(next(&mut stream).video.renditions["v"].enabled);
		assert!(!next(&mut stream).video.renditions["v"].enabled, "held while disabled");
		assert!(next(&mut stream).video.renditions["v"].enabled);
	}
}
