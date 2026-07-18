//! A broadcast's rendition set, as a `Producer`/[`Consumer`] pair.
//!
//! The `Producer` holds the current renditions, reconciled from the catalog by
//! its `sync`; the HTTP serve path reads them synchronously (look one up, render
//! the master playlist). A [`Consumer`] is a cursor for a recorder that mirrors the *whole*
//! broadcast: [`next`](Consumer::next) yields one [`Event`] at a time as renditions are added
//! or removed, replaying the current set as [`Added`](Event::Added) when first consumed, and
//! returning `None` once the source closes.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::task::Poll;

use moq_mux::catalog::hang::Catalog;

use super::Config;
use super::rendition::{Kind, Rendition};

/// The `(kind, name)` identity of a rendition. Video and audio are separate axes, so a video
/// and an audio rendition may share a name without colliding.
type Key = (Kind, String);

/// A change to the rendition set, yielded by [`Consumer::next`].
pub enum Event {
	/// A rendition appeared (or was reconfigured: a `Removed` for the old one precedes it).
	Added(Arc<Rendition>),
	/// A rendition disappeared (removed from the catalog, or replaced by a reconfigure).
	Removed { kind: Kind, name: String },
}

/// The producing side of a broadcast's rendition set.
///
/// [`sync`](Self::sync) reconciles it against a catalog snapshot; [`close`](Self::close) marks
/// the source ended (so consumers stop). Cloneable: the catalog watcher and the owning
/// `Broadcaster` each hold one, and the channel stays open until both drop or one calls
/// `close`.
#[derive(Clone)]
pub(crate) struct Producer {
	state: kio::Producer<BTreeMap<Key, Arc<Rendition>>>,
}

impl Producer {
	pub fn new() -> Self {
		Self {
			state: kio::Producer::new(BTreeMap::new()),
		}
	}

	/// A cursor over rendition changes, replaying the current set as [`Event::Added`].
	pub fn subscribe(&self) -> Consumer {
		Consumer {
			state: self.state.consume(),
			seen: BTreeMap::new(),
		}
	}

	/// Look up a rendition by kind and name (serve path).
	pub fn get(&self, kind: Kind, name: &str) -> Option<Arc<Rendition>> {
		self.state.read().get(&(kind, name.to_string())).cloned()
	}

	/// Snapshot the current renditions in `(kind, name)` order (for the master playlist).
	pub fn snapshot(&self) -> Vec<Arc<Rendition>> {
		self.state.read().values().cloned().collect()
	}

	/// Whether the current catalog contains no servable renditions (serve path).
	#[cfg_attr(not(feature = "server"), allow(dead_code))]
	pub fn is_empty(&self) -> bool {
		self.state.read().is_empty()
	}

	/// Release every rendition the map is holding.
	///
	/// The map lives in shared state, so a surviving [`Consumer`] would otherwise keep every
	/// rendition -- and the standing timeline subscription each watcher holds -- alive after the
	/// owning `Broadcaster` is gone. `Broadcaster::drop` calls this so teardown doesn't depend on
	/// consumers dropping first: a rendition nobody else holds drops here, and its `Drop` aborts
	/// its watcher.
	///
	/// Deliberately drops rather than [`Rendition::close`]s. A rendition a cursor still holds is
	/// left to finish on its own, because its watcher ends a cleanly-finished timeline with
	/// `end()` *then* `close()`, and `end()` is what promotes the live-edge record into the final
	/// segment. Force-closing here would race that: `end()` is a no-op on an already-closed
	/// channel, so the last segment of a recording would be silently dropped.
	pub fn clear(&self) {
		if let Ok(mut current) = self.state.write() {
			current.clear();
		}
	}

	/// Close the channel, signalling consumers that no more renditions will appear.
	///
	/// Deliberately does NOT cascade into the renditions' own segment channels, for two reasons.
	/// On a clean end it's unnecessary and harmful: each rendition's watcher already ends its
	/// timeline with `end()` then `close()`, and cascading a bare `close()` here would race that
	/// and lose the final segment (see [`Self::clear`]). On a catalog-stream *error* the media
	/// tracks are usually still fine, so cutting every in-flight segment cursor would truncate a
	/// recording over a transient fault.
	pub fn close(&self) {
		let _ = self.state.close();
	}

	/// Resolve once at least one rendition has been discovered. Bounding how long to wait is
	/// the caller's policy, so wrap this in a timeout rather than passing one in.
	pub async fn ready(&self) {
		let _ = kio::wait(|waiter| {
			self.state.poll_ref(waiter, |current| {
				if current.is_empty() {
					Poll::Pending
				} else {
					Poll::Ready(())
				}
			})
		})
		.await;
	}

	/// Reconcile the rendition set with a complete catalog snapshot. Removed or reconfigured
	/// renditions are dropped before replacements become visible, which also aborts their
	/// timeline watchers and releases their subscriptions.
	pub fn sync(&self, source: &moq_mux::Source, config: &Config, catalog: &Catalog) {
		let Ok(mut current) = self.state.write() else {
			return;
		};

		// Renditions the catalog dropped or reconfigured. Close each as it goes so any cursor
		// over it drains and ends, instead of parking on a timeline that never finishes -- the
		// cursor's own `Arc<Rendition>` would otherwise keep it (and its subscription) alive.
		let stale: Vec<Key> = current
			.iter()
			.filter(|((kind, name), rendition)| match kind {
				Kind::Video => !catalog
					.video
					.renditions
					.get(name)
					.is_some_and(|config| rendition.matches_video(config)),
				Kind::Audio => !catalog
					.audio
					.renditions
					.get(name)
					.is_some_and(|config| rendition.matches_audio(config)),
			})
			.map(|(key, _)| key.clone())
			.collect();
		for key in stale {
			if let Some(rendition) = current.remove(&key) {
				rendition.close();
			}
		}

		for (name, video) in &catalog.video.renditions {
			let key = (Kind::Video, name.clone());
			if current.contains_key(&key) {
				continue;
			}
			match Rendition::video(name.clone(), video, source, config.window) {
				Some(rendition) => {
					current.insert(key, Arc::new(rendition));
				}
				None => tracing::warn!(%name, "skipping video rendition without a timeline track"),
			}
		}
		for (name, audio) in &catalog.audio.renditions {
			let key = (Kind::Audio, name.clone());
			if current.contains_key(&key) {
				continue;
			}
			match Rendition::audio(name.clone(), audio, source, config.window) {
				Some(rendition) => {
					current.insert(key, Arc::new(rendition));
				}
				None => tracing::warn!(%name, "skipping audio rendition without a timeline track"),
			}
		}
	}
}

/// A cursor over one broadcast's rendition changes.
///
/// Each [`next`](Self::next) yields a single [`Event`]. The cursor keeps its own view of the
/// set it has reported, so it diffs against the producer independently of any other consumer:
/// a fresh consumer replays every current rendition as [`Event::Added`].
pub struct Consumer {
	state: kio::Consumer<BTreeMap<Key, Arc<Rendition>>>,
	/// The renditions this cursor has already reported, by identity, so a reconfigure (a
	/// replaced `Arc`) is seen as a remove followed by an add.
	seen: BTreeMap<Key, Arc<Rendition>>,
}

impl Consumer {
	/// The next rendition change; `None` once the source broadcast closes.
	pub async fn next(&mut self) -> Option<Event> {
		let event = kio::wait(|waiter| self.poll_next(waiter)).await?;
		match &event {
			Event::Added(rendition) => {
				self.seen
					.insert((rendition.kind, rendition.name.clone()), rendition.clone());
			}
			Event::Removed { kind, name } => {
				self.seen.remove(&(*kind, name.clone()));
			}
		}
		Some(event)
	}

	fn poll_next(&self, waiter: &kio::Waiter) -> Poll<Option<Event>> {
		match self.state.poll(waiter, |current| next_event(current, &self.seen)) {
			Poll::Ready(Ok(event)) => Poll::Ready(Some(event)),
			// Closed with no pending diff: no more changes.
			Poll::Ready(Err(_)) => Poll::Ready(None),
			Poll::Pending => Poll::Pending,
		}
	}
}

/// The single next event that reconciles `seen` toward `current`: removals (including the
/// remove half of a reconfigure) before additions. `Pending` once they match.
fn next_event(current: &BTreeMap<Key, Arc<Rendition>>, seen: &BTreeMap<Key, Arc<Rendition>>) -> Poll<Event> {
	for (key, rendition) in seen {
		match current.get(key) {
			Some(current) if Arc::ptr_eq(current, rendition) => {}
			// Missing, or replaced by a reconfigure: report the old one gone. The replacement
			// (if any) is reported as an add on a later call, once this removal clears `seen`.
			_ => {
				return Poll::Ready(Event::Removed {
					kind: key.0,
					name: key.1.clone(),
				});
			}
		}
	}
	for (key, rendition) in current {
		if !seen.contains_key(key) {
			return Poll::Ready(Event::Added(rendition.clone()));
		}
	}
	Poll::Pending
}
