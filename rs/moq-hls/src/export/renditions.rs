//! A broadcast's rendition set, as a `Producer`/[`Consumer`] pair.
//!
//! The `Producer` holds the current renditions, reconciled from the catalog by its `sync`;
//! the HTTP serve path reads them synchronously (look one up, render the master playlist). It
//! is also the fan-out point for the broadcast's single timeline: the timeline watcher's
//! `Fanout` handle hands each record to every rendition, which keeps its own window over it.
//! A [`Consumer`] is a cursor for a recorder that mirrors the *whole* broadcast: [`next`](Consumer::next)
//! yields one [`Event`] at a time as renditions are added or removed, replaying the current
//! set as [`Added`](Event::Added) when first consumed, and returning `None` once the source
//! closes.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex, Weak};
use std::task::Poll;
use std::time::Duration;

use moq_mux::catalog::hang::Catalog;
use moq_mux::timeline::Entry;

use super::rendition::{Kind, Rendition};

/// The `(kind, name)` identity of a rendition. Video and audio are separate axes, so a video
/// and an audio rendition may share a name without colliding.
type Key = (Kind, String);

/// A change to the rendition set, yielded by [`Consumer::next`].
#[non_exhaustive]
pub enum Event {
	/// A rendition appeared (or was reconfigured: a `Removed` for the old one precedes it).
	Added(Arc<Rendition>),
	/// A rendition disappeared (removed from the catalog, or replaced by a reconfigure).
	Removed {
		/// Which axis (video or audio) the removed rendition was on.
		kind: Kind,
		/// The removed rendition's name.
		name: String,
	},
}

/// The timeline fan-out state: who gets fed, and what a late-created rendition replays.
///
/// Feeds by `Weak` reference rather than through the rendition map: a recording cursor's
/// `Arc<Rendition>` must keep receiving records after `Broadcaster::drop` clears the map, or
/// its final segments would be silently lost.
struct Feed {
	/// Every rendition ever created and still alive, pruned as they drop.
	targets: Vec<Weak<Rendition>>,
	/// Recent records, replayed into a rendition created mid-broadcast so its playlist window
	/// isn't empty until the next record. Evicted with the same policy as the windows.
	history: VecDeque<Entry>,
	/// The timeline ended cleanly; late-created renditions start ended (`EXT-X-ENDLIST`).
	ended: bool,
	/// The timeline stream is over (cleanly or not); late-created renditions start closed.
	closed: bool,
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
	fanout: Fanout,
}

/// The timeline fan-out handle: everything the timeline watcher needs, and nothing more.
///
/// Deliberately separate from [`Producer`]: the watcher outlives the `Broadcaster` while a
/// recording cursor drains, and holding a full `Producer` there would keep the rendition-set
/// channel open (so `renditions()` cursors would never terminate).
#[derive(Clone)]
pub(crate) struct Fanout {
	feed: Arc<Mutex<Feed>>,
	/// The playlist window duration (see [`Config::window`](super::Config::window)), applied on every push.
	window: Duration,
}

impl Producer {
	pub fn new(window: Duration) -> Self {
		Self {
			state: kio::Producer::new(BTreeMap::new()),
			fanout: Fanout {
				feed: Arc::new(Mutex::new(Feed {
					targets: Vec::new(),
					history: VecDeque::new(),
					ended: false,
					closed: false,
				})),
				window,
			},
		}
	}

	/// The timeline fan-out handle for the timeline watcher task.
	pub fn fanout(&self) -> Fanout {
		self.fanout.clone()
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

	/// Whether anything outside the map (a recording cursor, a handed-out `Arc`) still holds a
	/// rendition. `Broadcaster::drop` checks this to decide whether the timeline watcher must
	/// outlive it (so those holders keep receiving records) or can be aborted.
	pub fn any_held_externally(&self) -> bool {
		self.state.read().values().any(|r| Arc::strong_count(r) > 1)
	}

	/// Release every rendition the map is holding.
	///
	/// The map lives in shared state, so a surviving [`Consumer`] would otherwise keep every
	/// rendition alive after the owning `Broadcaster` is gone. `Broadcaster::drop` calls this
	/// so teardown doesn't depend on consumers dropping first.
	///
	/// Deliberately drops rather than [`Rendition::close`]s: a rendition a cursor still holds
	/// keeps receiving timeline records through the [`Feed`]'s weak list, so a recording in
	/// progress drains to the real end of the broadcast instead of being truncated here.
	pub fn clear(&self) {
		if let Ok(mut current) = self.state.write() {
			current.clear();
		}
	}

	/// Close the channel, signalling consumers that no more renditions will appear.
	///
	/// Deliberately does NOT cascade into the renditions' own segment windows: those are ended
	/// or closed by the timeline watcher (see [`Fanout::end_windows`] /
	/// [`Fanout::close_windows`]), whose clean-end path must run `end()` before `close()` or
	/// the playlist never gets its `EXT-X-ENDLIST`.
	pub fn close(&self) {
		let _ = self.state.close();
	}
}

impl Fanout {
	/// Fan one timeline record out to every living rendition, and into the replay history.
	pub fn push(&self, entry: Entry) {
		let mut feed = self.feed.lock().unwrap();

		// Same eviction policy as the per-rendition windows, so a replay reconstructs the
		// same window a live rendition would have.
		if let Some(back) = feed.history.back()
			&& (Duration::from(entry.pts) < Duration::from(back.pts) || entry.segment <= back.segment)
		{
			feed.history.clear();
		}
		feed.history.push_back(entry.clone());
		while feed.history.len() >= 2 {
			let newest = feed.history.back().unwrap();
			let span =
				(Duration::from(newest.pts) + newest.duration).saturating_sub(Duration::from(feed.history[1].pts));
			if span < self.window {
				break;
			}
			feed.history.pop_front();
		}

		let window = self.window;
		feed.targets.retain(|target| {
			let Some(rendition) = target.upgrade() else {
				return false;
			};
			rendition.push(&entry, window);
			true
		});
	}

	/// Mark every living rendition's window ended (the timeline finished cleanly).
	pub fn end_windows(&self) {
		let mut feed = self.feed.lock().unwrap();
		feed.ended = true;
		feed.targets.retain(|target| {
			let Some(rendition) = target.upgrade() else {
				return false;
			};
			rendition.end();
			true
		});
	}

	/// Close every living rendition's window: no more records will arrive. Cursors drain what
	/// they can still see and end; the serve path keeps reading the frozen windows.
	pub fn close_windows(&self) {
		let mut feed = self.feed.lock().unwrap();
		feed.closed = true;
		feed.targets.retain(|target| {
			let Some(rendition) = target.upgrade() else {
				return false;
			};
			rendition.close();
			true
		});
	}
}

impl Producer {
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

	/// Enroll a freshly-created rendition in the timeline feed, replaying the recent history
	/// (and the ended/closed markers) so its window matches its siblings'.
	fn register(&self, rendition: &Arc<Rendition>) {
		let mut feed = self.fanout.feed.lock().unwrap();
		for entry in &feed.history {
			rendition.push(entry, self.fanout.window);
		}
		if feed.ended {
			rendition.end();
		}
		if feed.closed {
			rendition.close();
		}
		feed.targets.push(Arc::downgrade(rendition));
	}

	/// Reconcile the rendition set with a complete catalog snapshot. Removed or reconfigured
	/// renditions are closed (so cursors over them end) before replacements become visible.
	///
	/// Renditions are only servable when the catalog advertises the broadcast's timeline (its
	/// root `timeline` section): without one there is nothing to render playlists from, so the
	/// whole catalog is skipped with a warning.
	pub fn sync(&self, source: &moq_mux::Source, catalog: &Catalog) {
		let Some(section) = catalog.timeline.clone() else {
			if !catalog.video.renditions.is_empty() || !catalog.audio.renditions.is_empty() {
				tracing::warn!("catalog advertises no timeline; its renditions can't be served as HLS");
			}
			return;
		};

		let Ok(mut current) = self.state.write() else {
			return;
		};

		// Renditions the catalog dropped or reconfigured. Close each as it goes so any cursor
		// over it drains and ends, instead of parking on a window that never finishes -- the
		// cursor's own `Arc<Rendition>` would otherwise keep it alive.
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
			let rendition = Arc::new(Rendition::video(name.clone(), video, source, section.clone()));
			self.register(&rendition);
			current.insert(key, rendition);
		}
		for (name, audio) in &catalog.audio.renditions {
			let key = (Kind::Audio, name.clone());
			if current.contains_key(&key) {
				continue;
			}
			let rendition = Arc::new(Rendition::audio(name.clone(), audio, source, section.clone()));
			self.register(&rendition);
			current.insert(key, rendition);
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
