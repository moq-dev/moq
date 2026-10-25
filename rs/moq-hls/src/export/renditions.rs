//! A broadcast's rendition set, as a `Producer`/[`Consumer`] pair.
//!
//! The `Producer` holds the current renditions, reconciled from the catalog by its `sync`;
//! the HTTP serve path reads them synchronously (look one up, render the master playlist). It
//! is also the fan-out point for the broadcast's segments: the reference rendition's timeline
//! watcher hands each record to every rendition through the `Fanout` handle, and each rendition
//! keeps its own window over them.
//! A [`Consumer`] is a cursor for a recorder that mirrors the *whole* broadcast: [`next`](Consumer::next)
//! yields one [`Event`] at a time as renditions are added or removed, replaying the current
//! set as [`Added`](Event::Added) when first consumed, and returning `None` once the source
//! finishes cleanly, or an error when the recording is incomplete.

use moq_clock::Wall;
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Weak};
use std::task::Poll;
use std::time::Duration;

use moq_mux::catalog::hang::Catalog;
use moq_mux::timeline::Entry;

use super::rendition::{Kind, Rendition};
use super::segments::{self, Discontinuities, Reference};

/// The `(kind, name)` identity of a rendition. Video and audio are separate axes, so a video
/// and an audio rendition may share a name without colliding.
type Key = (Kind, String);

/// A change to the rendition set, yielded by [`Consumer::next`].
#[non_exhaustive]
pub enum Event {
	/// A rendition appeared, with its own timeline advertised (or was reconfigured: a `Removed`
	/// for the old one precedes it).
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
	/// Recent reference records with their discontinuity sequence, replayed into a rendition
	/// created mid-broadcast so its playlist window isn't empty until the next record. Evicted with
	/// the same policy as the windows.
	history: VecDeque<segments::Row>,
	/// The last accepted row survives source pops, so a handoff cannot rewind or reuse a position.
	last: Option<segments::Row>,
	/// Catalog-managed reader identity; pointer identity also rejects a cancelled reader of
	/// the same reference after a rapid disable/re-enable. No selected reader freezes the window.
	selection: Option<Option<Reference>>,
	/// A new watcher first replays its checkpoint, including records already advertised.
	joining: bool,
	/// Stamps each record once, so every rendition agrees on where the timeline breaks.
	discontinuities: Discontinuities,
	/// The timeline ended cleanly; late-created renditions start ended (`EXT-X-ENDLIST`).
	ended: bool,
	/// A reference timeline the playlists still list failed; late-created renditions start failed.
	failed: Option<crate::Error>,
	/// The timeline stream is over (cleanly or not); late-created renditions start closed.
	closed: bool,
	/// The estimated wall-clock time of timeline `pts` 0, anchored when the first record
	/// arrived (assuming its segment had just ended). The DASH manifest's
	/// `availabilityStartTime` falls back to this when the catalog declares no `clock`; set
	/// once so reloads see a stable presentation, and reset with the window when the
	/// timeline restarts.
	anchor: Option<Wall>,
	/// The publisher run every segment URL carries; late-created renditions inherit it.
	generation: Option<Arc<str>>,
	/// The playlist window duration applied on every push (see
	/// [`Config::window`](super::Config::window)).
	window: Duration,
}

impl Feed {
	fn accepts(&self, reference: &Reference) -> bool {
		self.selection.as_ref().is_none_or(|selected| {
			selected
				.as_ref()
				.is_some_and(|selected| Arc::ptr_eq(selected, reference))
		})
	}
}

/// The producing side of a broadcast's rendition set.
///
/// [`sync`](Self::sync) reconciles it against a catalog snapshot; [`close`](Self::close) marks
/// the source ended (so consumers stop). Cloneable: the catalog watcher and the owning
/// `Broadcaster` each hold one, and the channel stays open until both drop or one calls
/// `close`.
#[derive(Clone)]
pub(crate) struct Producer {
	state: kio::Producer<State>,
	fanout: Fanout,
}

#[derive(Default)]
struct State {
	renditions: BTreeMap<Key, Arc<Rendition>>,
	/// The catalog's confirmed end, distinct from dropping its watcher.
	finished: Option<crate::Result<()>>,
	// Cache catalog quality order without making any_held_externally count it as a consumer.
	audio: Vec<Weak<Rendition>>,
}

/// The timeline fan-out handle: everything the timeline watcher needs, and nothing more.
///
/// Deliberately separate from [`Producer`]: the watcher outlives the `Broadcaster` while a
/// recording cursor drains, and holding a full `Producer` there would keep the rendition-set
/// channel open (so `renditions()` cursors would never terminate).
#[derive(Clone)]
pub(crate) struct Fanout {
	feed: kio::Producer<Feed>,
}

impl Producer {
	pub fn new(window: Duration) -> Self {
		Self {
			state: kio::Producer::new(State::default()),
			fanout: Fanout {
				feed: kio::Producer::new(Feed {
					targets: Vec::new(),
					history: VecDeque::new(),
					last: None,
					selection: None,
					joining: false,
					discontinuities: Discontinuities::default(),
					ended: false,
					failed: None,
					closed: false,
					anchor: None,
					generation: None,
					window,
				}),
			},
		}
	}

	/// The timeline fan-out handle for the timeline watcher task.
	pub fn fanout(&self) -> Fanout {
		self.fanout.clone()
	}

	/// The playlist window duration every rendition's window is trimmed to.
	pub fn window(&self) -> Duration {
		self.fanout.feed.read().window
	}

	/// The estimated wall-clock time of timeline `pts` 0 (see [`Feed::anchor`]); `None` until
	/// the first record arrives.
	pub fn anchor(&self) -> Option<Wall> {
		self.fanout.feed.read().anchor
	}

	/// Why a reference timeline the playlists list failed, if one did.
	pub fn failure(&self) -> Option<crate::Error> {
		self.fanout.feed.read().failed.clone()
	}

	/// A cursor over rendition changes, replaying the current set as [`Event::Added`].
	pub fn subscribe(&self) -> Consumer {
		Consumer {
			state: self.state.consume(),
			feed: self.fanout.feed.consume(),
			seen: BTreeMap::new(),
		}
	}

	/// Look up a rendition by kind and name (serve path).
	pub fn get(&self, kind: Kind, name: &str) -> Option<Arc<Rendition>> {
		self.state.read().renditions.get(&(kind, name.to_string())).cloned()
	}

	/// Snapshot video by name and audio in catalog quality order (for the master playlist).
	pub fn snapshot(&self) -> Vec<Arc<Rendition>> {
		let current = self.state.read();
		let mut snapshot: Vec<_> = current
			.renditions
			.values()
			.filter(|rendition| rendition.kind == Kind::Video)
			.cloned()
			.collect();
		snapshot.extend(
			current
				.audio
				.iter()
				.map(|rendition| rendition.upgrade().expect("ranked audio is held in the rendition map")),
		);
		snapshot
	}

	/// Whether the current catalog contains no servable renditions (serve path).
	#[cfg_attr(
		not(feature = "server"),
		allow(dead_code, reason = "only the `server` feature uses it")
	)]
	pub fn is_empty(&self) -> bool {
		self.state.read().renditions.is_empty()
	}

	/// Whether anything outside the map (a recording cursor, a handed-out `Arc`) still holds a
	/// rendition. `Broadcaster::drop` checks this to decide whether the timeline watcher must
	/// outlive it (so those holders keep receiving records) or can be aborted.
	pub fn any_held_externally(&self) -> bool {
		self.state.read().renditions.values().any(|r| Arc::strong_count(r) > 1)
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
			current.renditions.clear();
			current.audio.clear();
		}
	}

	/// Record the catalog's end before closing its change stream.
	pub fn finish(&self, result: crate::Result<()>) {
		if let Err(err) = &result {
			let failed = crate::Error::CatalogFailed {
				source: Box::new(err.clone()),
			};
			self.fanout.fail(None, failed);
		}
		if let Ok(mut state) = self.state.write() {
			state.finished = Some(result);
		}
		self.close();
	}

	/// Close the channel, signalling consumers that no more renditions will appear.
	///
	/// Deliberately does NOT cascade into the renditions' own segment windows: those are ended
	/// or failed by the timeline watcher (see [`Fanout::finish`] and [`Fanout::fail`]), which must
	/// mark them before closing them, or the playlist never gets its `EXT-X-ENDLIST` or error.
	pub fn close(&self) {
		let _ = self.state.close();
	}
}

impl Fanout {
	/// Select the next reference, whose records list only past the last advertised one.
	pub fn select(&self, reference: Option<Reference>) {
		let mut feed = self.feed.write().unwrap_or_else(|_| unreachable!("fanout is open"));
		feed.selection = Some(reference);
		feed.joining = true;
	}

	/// Fan one reference timeline record out to every living rendition, and into replay history.
	pub fn push(&self, index: u64, entry: Entry, reference: &Reference) {
		let mut feed = self.feed.write().unwrap_or_else(|_| unreachable!("fanout is open"));
		if !feed.accepts(reference) || feed.failed.is_some() {
			return;
		}
		let pts = Duration::from(entry.pts);
		let end = entry.end_time();
		let mut skipped = false;
		if let Some(last) = &feed.last {
			if feed.joining || last.reference != *reference {
				// The media sequence is the source record sequence, so every edge derives it alone.
				// A replacement resumes only past every advertised position and content time; a
				// lagging one holds the playlist until its numbering catches up.
				if entry.sequence <= last.segment || (pts < last.end && !segments::continues(last.end, pts)) {
					return;
				}
				// HLS numbers a playlist's segments consecutively, so a replacement that skips
				// positions cannot follow the advertised rows.
				skipped = entry.sequence > last.segment + 1;
			} else if pts < Duration::from(last.pts) || entry.sequence <= last.segment {
				feed.history.clear();
				feed.last = None;
				feed.anchor = None;
			}
		}
		feed.joining = false;
		if skipped {
			feed.history.clear();
			feed.discontinuities.interrupt();
			feed.targets.retain(|target| {
				let Some(rendition) = target.upgrade() else {
					return false;
				};
				rendition.clear();
				true
			});
		}
		if feed.anchor.is_none() {
			feed.anchor = Some(moq_clock::wall().checked_sub(end).unwrap_or(Wall::UNIX_EPOCH));
		}
		let period = feed.last.as_ref().map_or(moq_net::Timestamp::ZERO, |last| {
			if last.reference == *reference {
				last.period
			} else {
				entry.pts
			}
		});
		let discontinuity = feed.discontinuities.stamp(pts, end);
		let row = segments::Row {
			index,
			segment: entry.sequence,
			period,
			reference: reference.clone(),
			frames: entry.start..entry.end,
			keyframe: entry.keyframe,
			duration: entry.duration,
			pts: entry.pts,
			end,
			discontinuity,
		};
		feed.last = Some(row.clone());
		feed.history.push_back(row.clone());
		let window = feed.window;
		loop {
			let rest = feed.history.get(1).map_or(Duration::ZERO, |second| {
				feed.history.back().unwrap().end.saturating_sub(second.pts.into())
			});
			let keeps_start = !feed.history[0].starts() || feed.history.iter().skip(1).any(segments::Row::starts);
			if !segments::evicts(window, feed.history.len(), rest, keeps_start) {
				break;
			}
			feed.history.pop_front();
		}
		feed.targets.retain(|target| {
			let Some(rendition) = target.upgrade() else {
				return false;
			};
			rendition.push(row.clone(), window);
			true
		});
	}

	/// Remove records that left the source timeline window from every rendition window.
	pub fn pop(&self, range: std::ops::Range<u64>, reference: &Reference) {
		let mut feed = self.feed.write().unwrap_or_else(|_| unreachable!("fanout is open"));
		let Some(through) = feed
			.history
			.iter()
			.filter(|row| row.reference == *reference && range.contains(&row.index))
			.map(|row| row.segment)
			.max()
		else {
			return;
		};
		// HLS assigns consecutive positions within a playlist. Removing an interior row
		// would renumber every later URI, so retention always retires a whole prefix.
		feed.history.retain(|row| row.segment > through);
		feed.targets.retain(|target| {
			let Some(rendition) = target.upgrade() else {
				return false;
			};
			rendition.retire(through);
			true
		});
	}

	/// List every rendition under `generation` from now on.
	///
	/// Replacing a generation drops what was listed under it and the inits built from it: those
	/// belong to the run it named. The first one only labels the run already flowing, so an embedder that supplies
	/// it right after construction loses nothing to a race with the first records.
	pub fn set_generation(&self, generation: Option<Arc<str>>) {
		let mut feed = self.feed.write().unwrap_or_else(|_| unreachable!("fanout is open"));
		if feed.generation == generation {
			return;
		}
		let restart = feed.generation.is_some();
		feed.generation = generation.clone();
		if restart {
			feed.history.clear();
			feed.last = None;
			feed.discontinuities.interrupt();
			feed.anchor = None;
		}
		feed.targets.retain(|target| {
			let Some(rendition) = target.upgrade() else {
				return false;
			};
			if restart {
				rendition.restart(generation.clone());
			} else {
				rendition.label(generation.clone());
			}
			true
		});
	}

	/// Clear stale rows after source records were skipped before this reader saw them.
	pub fn skip(&self, reference: Option<&Reference>) {
		let mut feed = self.feed.write().unwrap_or_else(|_| unreachable!("fanout is open"));
		if let Some(reference) = reference
			&& (feed.joining || !feed.accepts(reference))
		{
			return;
		}
		feed.history.clear();
		if reference.is_none() {
			feed.last = None;
		}
		feed.discontinuities.interrupt();
		feed.targets.retain(|target| {
			let Some(rendition) = target.upgrade() else {
				return false;
			};
			rendition.clear();
			true
		});
	}

	/// End every window for the selected watcher, ignoring a superseded watcher's completion.
	/// `None` ends them when the source ended with no reference selected, so no watcher will.
	pub fn finish(&self, reference: Option<&Reference>) -> bool {
		let mut feed = self.feed.write().unwrap_or_else(|_| unreachable!("fanout is open"));
		let selected = match reference {
			Some(reference) => feed.accepts(reference),
			None => feed.selection.as_ref().is_none_or(Option::is_none),
		};
		if !selected || feed.failed.is_some() {
			return false;
		}
		feed.ended = true;
		feed.closed = true;
		feed.targets.retain(|target| {
			let Some(rendition) = target.upgrade() else {
				return false;
			};
			rendition.end();
			rendition.close();
			true
		});
		true
	}

	/// Fail every window with `err` when the selected `reference`'s timeline failed, so no
	/// playlist reads as finished. A live window trims a superseded reference's rows on its own.
	/// `None` fails the whole source when the catalog fails.
	pub fn fail(&self, reference: Option<&Reference>, err: crate::Error) -> bool {
		let mut feed = self.feed.write().unwrap_or_else(|_| unreachable!("fanout is open"));
		if feed.failed.is_some() || !reference.is_none_or(|reference| feed.accepts(reference)) {
			return false;
		}
		feed.failed = Some(err.clone());
		feed.closed = true;
		feed.targets.retain(|target| {
			let Some(rendition) = target.upgrade() else {
				return false;
			};
			rendition.fail(err.clone());
			rendition.close();
			true
		});
		true
	}
}

impl Producer {
	/// Resolve once at least one rendition has a playable media playlist. Bounding how long to
	/// wait is the caller's policy, so wrap this in a timeout rather than passing one in.
	pub async fn ready(&self) {
		let _ = kio::wait(|waiter| {
			self.state.poll_ref(waiter, |current| {
				if current
					.renditions
					.values()
					.any(|rendition| rendition.poll_playable(waiter).is_ready())
				{
					Poll::Ready(())
				} else {
					Poll::Pending
				}
			})
		})
		.await;
	}

	/// Resolve once at least one enabled rendition is [advertised](Rendition::is_advertised), so a
	/// master playlist can list it.
	#[cfg_attr(
		not(feature = "server"),
		allow(dead_code, reason = "only the `server` feature uses it")
	)]
	pub(crate) async fn advertised(&self) {
		let _ = kio::wait(|waiter| {
			self.state.poll_ref(waiter, |current| {
				if current
					.renditions
					.values()
					.any(|rendition| rendition.is_enabled() && rendition.poll_advertised(waiter).is_ready())
				{
					Poll::Ready(())
				} else {
					Poll::Pending
				}
			})
		})
		.await;
	}

	/// Enroll a freshly-created rendition in the timeline feed, replaying the recent history
	/// (and the ended/failed/closed markers) so its window matches its siblings', and start following its
	/// own timeline.
	fn register(&self, rendition: &Arc<Rendition>) {
		let mut feed = self
			.fanout
			.feed
			.write()
			.unwrap_or_else(|_| unreachable!("fanout is open"));
		rendition.label(feed.generation.clone());
		rendition.watch();
		for row in &feed.history {
			rendition.push(row.clone(), feed.window);
		}
		if feed.ended {
			rendition.end();
		}
		if let Some(err) = &feed.failed {
			rendition.fail(err.clone());
		}
		if feed.closed {
			rendition.close();
		}
		feed.targets.push(Arc::downgrade(rendition));
	}

	/// Reconcile the rendition set with a complete catalog snapshot. Removed or reconfigured
	/// renditions are closed (so cursors over them end) before replacements become visible.
	///
	/// A disabled rendition stays in the set, serving its window and media playlist, and is only
	/// left out of the master playlist and the DASH manifest. Closing it would end its playlist
	/// with `EXT-X-ENDLIST`, which reads as a finished broadcast, and a rendition that
	/// re-enables keeps its media sequence and history.
	///
	/// "Reconfigured" means the media itself changed (see [`Rendition::matches_video`]). A
	/// rendition whose entry only carries a revised estimate is kept as-is and just takes the
	/// new advertised bitrate, since the publisher republishes the catalog every time its
	/// measured bitrate, jitter, or framerate moves.
	///
	/// Renditions are only servable when the catalog advertises its timelines (its root `archive`
	/// entry): without one there is nothing to render playlists from, so the whole catalog is
	/// skipped with a warning. A rendition joins the set only once the archive indexes its own
	/// timeline, which a publisher may declare after the rendition's init: until then every
	/// segment would be a gap, so it is neither listed nor reported as [`Event::Added`].
	///
	/// `source` holds the broadcast the snapshot was read from, so each rendition serves media
	/// from that same broadcast rather than whatever is at the path by the time it is asked. A
	/// rendition whose `broadcast` reference escapes above the origin root names no broadcast at
	/// all, so it is dropped with a warning rather than served.
	pub fn sync(&self, source: &moq_mux::Source, catalog: &Catalog) {
		use hang::catalog::Reference as _;

		let Some(archive) = catalog.archive.clone() else {
			if !catalog.video.renditions.is_empty() || !catalog.audio.renditions.is_empty() {
				tracing::warn!("catalog advertises no archive; its renditions can't be served as HLS");
			}
			return;
		};
		let section = archive;
		let clock = catalog.clock;

		let Ok(mut current) = self.state.write() else {
			return;
		};

		// Renditions the catalog dropped or reconfigured. Close each as it goes so any cursor
		// over it drains and ends, instead of parking on a window that never finishes -- the
		// cursor's own `Arc<Rendition>` would otherwise keep it alive.
		let stale: Vec<Key> = current
			.renditions
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
			if let Some(rendition) = current.renditions.remove(&key) {
				rendition.end();
				rendition.close();
			}
		}

		for (name, video) in &catalog.video.renditions {
			let key = (Kind::Video, name.clone());
			if let Some(rendition) = current.renditions.get(&key) {
				// Survived the stale pass, so it decodes the same: keep its window, its cached
				// init segment and its media sequence, and just take the new bitrate, framerate,
				// and `enabled` flag.
				rendition.refresh(video.bitrate, video.framerate, video.enabled);
				continue;
			}
			if !section.timelines.contains_key(video.track(name)) {
				continue;
			}
			let rendition = match Rendition::video(name.clone(), video, source, section.clone(), clock) {
				Ok(rendition) => Arc::new(rendition),
				Err(err) => {
					tracing::warn!(rendition = name, %err, "ignoring unservable video rendition");
					continue;
				}
			};
			self.register(&rendition);
			current.renditions.insert(key, rendition);
		}
		for (name, audio) in &catalog.audio.renditions {
			let key = (Kind::Audio, name.clone());
			if let Some(rendition) = current.renditions.get(&key) {
				rendition.refresh(audio.bitrate, None, audio.enabled);
				continue;
			}
			if !section.timelines.contains_key(audio.track(name)) {
				continue;
			}
			let rendition = match Rendition::audio(name.clone(), audio, source, section.clone(), clock) {
				Ok(rendition) => Arc::new(rendition),
				Err(err) => {
					tracing::warn!(rendition = name, %err, "ignoring unservable audio rendition");
					continue;
				}
			};
			self.register(&rendition);
			current.renditions.insert(key, rendition);
		}
		// Rank once per catalog update, keeping playlist reads linear. Updating state also wakes
		// the master's long-poll when metadata changes without replacing a rendition.
		current.audio = catalog
			.audio
			.ranked()
			.filter_map(|(name, _)| current.renditions.get(&(Kind::Audio, name.clone())))
			.map(Arc::downgrade)
			.collect();
	}
}

/// A cursor over one broadcast's rendition changes.
///
/// Each [`next`](Self::next) yields a single [`Event`]. The cursor keeps its own view of the
/// set it has reported, so it diffs against the producer independently of any other consumer:
/// a fresh consumer replays every current rendition as [`Event::Added`].
pub struct Consumer {
	state: kio::Consumer<State>,
	feed: kio::Consumer<Feed>,
	/// The renditions this cursor has already reported, by identity, so a reconfigure (a
	/// replaced `Arc`) is seen as a remove followed by an add.
	seen: BTreeMap<Key, Arc<Rendition>>,
}

impl Consumer {
	/// The next rendition change; `None` only after the catalog and reference finish cleanly.
	pub async fn next(&mut self) -> crate::Result<Option<Event>> {
		let Some(event) = kio::wait(|waiter| self.poll_next(waiter)).await? else {
			return Ok(None);
		};
		match &event {
			Event::Added(rendition) => {
				self.seen
					.insert((rendition.kind, rendition.name.clone()), rendition.clone());
			}
			Event::Removed { kind, name } => {
				self.seen.remove(&(*kind, name.clone()));
			}
		}
		Ok(Some(event))
	}

	fn poll_next(&self, waiter: &kio::Waiter) -> Poll<crate::Result<Option<Event>>> {
		let finished = match self
			.state
			.poll(waiter, |current| next_event(&current.renditions, &self.seen))
		{
			Poll::Ready(Ok(event)) => return Poll::Ready(Ok(Some(event))),
			Poll::Ready(Err(state)) => {
				match &state.finished {
					Some(result) => result.clone()?,
					None => return Poll::Ready(Err(crate::Error::IncompleteRecording)),
				}
				true
			}
			Poll::Pending => false,
		};
		// A failed reference cannot wait for an otherwise live catalog to close.
		match self.feed.poll(waiter, |feed| {
			if let Some(err) = &feed.failed {
				Poll::Ready(Err(err.clone()))
			} else if feed.ended && finished {
				Poll::Ready(Ok(None))
			} else {
				Poll::Pending
			}
		}) {
			Poll::Ready(Ok(result)) => Poll::Ready(result),
			Poll::Ready(Err(_)) => Poll::Ready(Err(crate::Error::IncompleteRecording)),
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

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn cursors_require_both_catalog_and_reference_completion() {
		tokio::time::pause();
		let producer = Producer::new(Duration::from_secs(30));
		let mut cursor = producer.subscribe();
		producer.finish(Ok(()));
		// Catalog completion alone cannot vouch for the media timeline.
		assert!(cursor.poll_next(&kio::Waiter::noop()).is_pending());
		producer.fanout.finish(None);
		assert!(cursor.next().await.unwrap().is_none());
		assert!(cursor.next().await.unwrap().is_none());

		let producer = Producer::new(Duration::from_secs(30));
		let mut cursor = producer.subscribe();
		producer.fanout.finish(None);
		assert!(cursor.poll_next(&kio::Waiter::noop()).is_pending());
		producer.finish(Ok(()));
		assert!(cursor.next().await.unwrap().is_none());

		let producer = Producer::new(Duration::from_secs(30));
		let mut cursor = producer.subscribe();
		producer.close();
		assert!(matches!(cursor.next().await, Err(crate::Error::IncompleteRecording)));
		assert!(matches!(cursor.next().await, Err(crate::Error::IncompleteRecording)));

		let producer = Producer::new(Duration::from_secs(30));
		let mut cursor = producer.subscribe();
		drop(producer);
		assert!(matches!(cursor.next().await, Err(crate::Error::IncompleteRecording)));
	}

	#[test]
	fn a_reference_failure_is_reported_without_catalog_completion() {
		let producer = Producer::new(Duration::from_secs(30));
		let cursor = producer.subscribe();
		let reference = Arc::new((Kind::Video, "video".into(), "video".into()));
		let waiter = kio::Waiter::noop();
		assert!(cursor.poll_next(&waiter).is_pending());
		producer.fanout.fail(Some(&reference), crate::Error::NoVariants);
		assert!(matches!(
			cursor.poll_next(&waiter),
			Poll::Ready(Err(crate::Error::NoVariants))
		));
	}

	#[tokio::test]
	async fn a_failed_catalog_or_reference_never_completes_a_cursor() {
		tokio::time::pause();
		let producer = Producer::new(Duration::from_secs(30));
		let mut cursor = producer.subscribe();
		producer.fanout.finish(None);
		producer.finish(Err(crate::Error::NoVariants));
		assert!(matches!(cursor.next().await, Err(crate::Error::NoVariants)));

		let producer = Producer::new(Duration::from_secs(30));
		let mut cursor = producer.subscribe();
		let reference = Arc::new((Kind::Video, "video".into(), "video".into()));
		producer.fanout.fail(Some(&reference), crate::Error::NoVariants);
		producer.finish(Ok(()));
		assert!(matches!(cursor.next().await, Err(crate::Error::NoVariants)));
	}

	fn entry(sequence: u64, pts: u64) -> Entry {
		let record = hang::timeline::Record::new(
			sequence,
			pts,
			2_000,
			hang::timeline::Position::group(sequence),
			hang::timeline::Position::group(sequence + 1),
		);
		Entry::decode(record, moq_net::Timescale::MILLI).unwrap()
	}

	fn handed_off(old_last: u64) -> (Fanout, Reference) {
		let fanout = Producer::new(Duration::from_secs(30)).fanout();
		let old = Arc::new((Kind::Video, "old".into(), "old".into()));
		let new = Arc::new((Kind::Video, "new".into(), "new".into()));
		fanout.select(Some(old.clone()));
		fanout.push(old_last, entry(old_last, old_last * 2_000), &old);
		fanout.select(Some(new.clone()));
		(fanout, new)
	}

	fn rows(fanout: &Fanout) -> Vec<(u64, u64, Duration)> {
		let feed = fanout.feed.read();
		feed.history
			.iter()
			.map(|row| (row.segment, row.discontinuity, Duration::from(row.period)))
			.collect()
	}

	#[test]
	fn an_aligned_replacement_continues_the_source_numbering() {
		let (fanout, new) = handed_off(5);
		// The replacement's checkpoint restates records the old reference already advertised.
		fanout.push(4, entry(4, 8_000), &new);
		fanout.push(5, entry(5, 10_000), &new);
		fanout.push(6, entry(6, 12_000), &new);
		let period = Duration::from_secs(12);
		assert_eq!(rows(&fanout), [(5, 0, Duration::ZERO), (6, 0, period)]);
	}

	#[test]
	fn a_replacement_ahead_drops_the_advertised_rows() {
		let (fanout, new) = handed_off(5);
		fanout.push(8, entry(8, 16_000), &new);
		assert_eq!(rows(&fanout), [(8, 1, Duration::from_secs(16))]);
	}

	#[test]
	fn a_lagging_replacement_holds_until_its_numbering_passes_the_advertised_rows() {
		let (fanout, new) = handed_off(5);
		for sequence in [2, 3, 5] {
			fanout.push(sequence, entry(sequence, 12_000 + sequence * 2_000), &new);
			assert_eq!(rows(&fanout), [(5, 0, Duration::ZERO)], "record {sequence} holds");
		}
		fanout.push(6, entry(6, 24_000), &new);
		assert_eq!(
			rows(&fanout),
			[(5, 0, Duration::ZERO), (6, 1, Duration::from_secs(24))],
			"the skipped content time is a discontinuity"
		);
	}

	#[test]
	fn a_source_skip_preserves_the_handoff_period() {
		let (fanout, new) = handed_off(5);
		fanout.push(6, entry(6, 12_000), &new);
		fanout.skip(Some(&new));
		fanout.push(9, entry(9, 18_000), &new);
		assert_eq!(rows(&fanout), [(9, 1, Duration::from_secs(12))]);
	}

	#[test]
	fn an_aborted_reader_of_the_same_reference_cannot_finish_its_replacement() {
		let fanout = Producer::new(Duration::from_secs(30)).fanout();
		let old = Arc::new((Kind::Video, "video".into(), "video".into()));
		fanout.select(Some(old.clone()));
		fanout.push(0, entry(0, 0), &old);
		let replacement = Arc::new(old.as_ref().clone());
		fanout.select(Some(replacement.clone()));
		assert!(!fanout.finish(Some(&old)));
		fanout.skip(Some(&old));
		fanout.push(1, entry(1, 2_000), &old);
		fanout.push(1, entry(1, 2_000), &replacement);
		assert_eq!(fanout.feed.read().history.len(), 2);
	}

	#[test]
	fn only_the_selected_reader_fails_the_feed() {
		let err = || crate::Error::NoVariants;
		// A superseded reader's rows trim with the window, so its failure changes nothing.
		let (fanout, new) = handed_off(5);
		let old = fanout.feed.read().history[0].reference.clone();
		assert!(!fanout.fail(Some(&old), err()));
		assert!(fanout.fail(Some(&new), err()));
		assert!(!fanout.finish(Some(&new)), "a failed feed never finishes");
	}

	#[test]
	fn handoff_ignores_replayed_and_superseded_events() {
		let producer = Producer::new(Duration::from_secs(30));
		let fanout = producer.fanout();
		let old = Arc::new((Kind::Video, "old".into(), "old".into()));
		let new = Arc::new((Kind::Video, "new".into(), "new".into()));
		fanout.select(Some(old.clone()));
		fanout.push(5, entry(5, 10_000), &old);
		fanout.select(Some(new.clone()));
		fanout.push(6, entry(6, 12_000), &old);
		assert!(!fanout.finish(Some(&old)));
		fanout.skip(Some(&old));
		fanout.skip(Some(&new));
		fanout.push(0, entry(0, 0), &new);
		fanout.push(6, entry(6, 12_000), &new);
		{
			let feed = fanout.feed.read();
			assert_eq!(feed.history.len(), 2);
			assert_eq!(feed.history.back().unwrap().segment, 6);
			assert!(!feed.ended);
			assert_eq!(feed.history.back().unwrap().discontinuity, 0);
		}
		fanout.pop(0..6, &new);
		assert_eq!(fanout.feed.read().history.len(), 2);
		fanout.pop(0..6, &old);
		assert_eq!(fanout.feed.read().history.len(), 1);
		fanout.pop(6..7, &new);
		assert!(fanout.feed.read().history.is_empty());
		fanout.push(7, entry(7, 14_000), &new);
		assert_eq!(fanout.feed.read().history.back().unwrap().segment, 7);
	}
}
