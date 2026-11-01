//! A broadcast's rendition set, as a `Producer`/[`Consumer`] pair.
//!
//! The `Producer` holds the current renditions, reconciled from the catalog by its `sync`;
//! the HTTP serve path reads them synchronously (look one up, render the master playlist). Each
//! rendition follows its own HLS records; the `Fanout` handle holds what they share: the
//! discontinuity counts and the publisher run.
//! A [`Consumer`] is a cursor for a recorder that mirrors the *whole* broadcast: [`next`](Consumer::next)
//! yields one [`Event`] at a time as renditions are added or removed, replaying the current
//! set as [`Added`](Event::Added) when first consumed, and returning `None` once the catalog
//! finishes cleanly, or an error when the recording is incomplete.

use kio::clock::Wall;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Weak};
use std::task::Poll;
use std::time::Duration;

use moq_mux::catalog::hang::Catalog;

use super::driver::{Owner, Spawner};
use super::master;
use super::rendition::{Kind, Rendition};

/// The `(kind, name)` identity of a rendition. Video and audio are separate axes, so a video
/// and an audio rendition may share a name without colliding.
pub(crate) type Key = (Kind, String);

/// Numbers each rendition's content breaks: its segments after a stop, or after records nobody
/// saw, start a new discontinuity, as does every rendition's next segment after a publisher-run
/// restart.
///
/// Counted per rendition, since each numbers its own segments. A break in one rendition only
/// makes its count diverge from its siblings', which players resync across by
/// `EXT-X-PROGRAM-DATE-TIME`. Kept by name, so a reconfigured rendition's replacement never
/// lists a lower sequence than its predecessor.
#[derive(Default)]
struct Discontinuities {
	/// Each live rendition's newest sequence.
	renditions: BTreeMap<Key, u64>,
	/// Publisher-run restarts so far, where a rendition first seen starts.
	restarts: u64,
}

impl Discontinuities {
	/// The sequence of rendition `key`'s next segment, which starts a new one when `broken`.
	fn stamp(&mut self, key: &Key, broken: bool) -> u64 {
		if !self.renditions.contains_key(key) {
			self.renditions.insert(key.clone(), self.restarts);
		}
		let sequence = self.renditions.get_mut(key).expect("inserted above");
		*sequence += u64::from(broken);
		*sequence
	}

	/// Start a new discontinuity on every rendition, for a publisher run that restarts.
	fn restart(&mut self) {
		self.restarts += 1;
		for sequence in self.renditions.values_mut() {
			*sequence += 1;
		}
	}
}

/// A change to the rendition set, yielded by [`Consumer::next`].
#[non_exhaustive]
pub enum Event {
	/// A rendition appeared, with its HLS records advertised (or was reconfigured: a `Removed`
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

/// What every rendition of the broadcast shares.
///
/// Reaches renditions by `Weak` reference rather than through the rendition map: a recording
/// cursor's `Arc<Rendition>` must keep its records flowing after `Broadcaster::drop` clears the
/// map, or its final segments would be silently lost.
struct Feed {
	/// Every rendition created and still alive, the dropped ones pruned as the next registers.
	targets: Vec<Weak<Rendition>>,
	/// Each rendition's discontinuity count.
	discontinuities: Discontinuities,
	/// The longest duration the first catalog batch declared, in whole seconds rounded up: the
	/// floor of every target duration this run (see [`Producer::register`]).
	baseline: Option<u64>,
	/// The catalog failed; late-created renditions start failed.
	failed: Option<crate::Error>,
	/// The estimated wall-clock time of `pts` 0, anchored when the first segment was listed
	/// (assuming it had just ended). The DASH manifest's `availabilityStartTime` falls back to
	/// this when the catalog declares no `clock`; set once so reloads see a stable presentation,
	/// and reset with the publisher run.
	anchor: Option<Wall>,
	/// The publisher run every segment URL carries; late-created renditions inherit it.
	generation: Option<Arc<str>>,
	/// The playlist window duration applied on every push (see
	/// [`Config::window`](super::Config::window)).
	window: Duration,
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
	// Cache catalog quality order without counting it as a holder.
	audio: Vec<Weak<Rendition>>,
}

/// The shared state handle each rendition's record watcher reaches.
///
/// Deliberately separate from [`Producer`]: a watcher outlives the `Broadcaster` while a
/// recording cursor drains, and holding a full `Producer` there would keep the rendition-set
/// channel open (so `renditions()` cursors would never terminate).
#[derive(Clone)]
pub(crate) struct Fanout {
	feed: kio::Producer<Feed>,
	/// Starts each rendition's background work on the broadcaster's driver.
	spawner: Spawner,
}

impl Producer {
	pub fn new(window: Duration, spawner: Spawner) -> Self {
		Self {
			state: kio::Producer::new(State::default()),
			fanout: Fanout {
				feed: kio::Producer::new(Feed {
					targets: Vec::new(),
					discontinuities: Discontinuities::default(),
					baseline: None,
					failed: None,
					anchor: None,
					generation: None,
					window,
				}),
				spawner,
			},
		}
	}

	/// A producer whose renditions run on a driver spawned on the ambient runtime.
	#[cfg(test)]
	pub(super) fn spawned(window: Duration) -> Self {
		let (spawner, driver) = Spawner::new();
		tokio::spawn(driver.run());
		Self::new(window, spawner)
	}

	/// How many renditions the fanout still reaches, dropped ones not yet pruned included.
	#[cfg(test)]
	pub(super) fn targets(&self) -> usize {
		self.fanout.feed.read().targets.len()
	}

	/// The handle every rendition shares.
	pub fn fanout(&self) -> Fanout {
		self.fanout.clone()
	}

	/// The playlist window duration every rendition's window is trimmed to.
	pub fn window(&self) -> Duration {
		self.fanout.window()
	}

	/// The estimated wall-clock time of `pts` 0 (see [`Feed::anchor`]); `None` until the first
	/// segment is listed.
	pub fn anchor(&self) -> Option<Wall> {
		self.fanout.feed.read().anchor
	}

	/// Why the catalog failed, if it did.
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

	/// Release every rendition the map is holding.
	///
	/// The map lives in shared state, so a surviving [`Consumer`] would otherwise keep every
	/// rendition alive after the owning `Broadcaster` is gone. `Broadcaster::drop` calls this
	/// so teardown doesn't depend on consumers dropping first.
	///
	/// Deliberately drops rather than [`Rendition::close`]s: a rendition a cursor still holds
	/// keeps following its records, so a recording in progress drains to the real end of the
	/// broadcast instead of being truncated here.
	pub fn clear(&self) {
		if let Ok(mut current) = self.state.write() {
			current.renditions.clear();
			current.audio.clear();
		}
	}

	/// Record the catalog's end before closing its change stream. A failed catalog fails every
	/// rendition, so none reads as finished.
	pub fn finish(&self, result: crate::Result<()>) {
		if let Err(err) = &result {
			self.fanout.fail(crate::Error::CatalogFailed {
				source: Box::new(err.clone()),
			});
		}
		if let Ok(mut state) = self.state.write() {
			state.finished = Some(result);
		}
		self.close();
	}

	/// Close the channel, signalling consumers that no more renditions will appear.
	///
	/// Deliberately does NOT cascade into the renditions' own segment windows: each ends with its
	/// own records.
	pub fn close(&self) {
		let _ = self.state.close();
	}
}

impl Fanout {
	/// Run `task` on the broadcaster's driver until it finishes or the returned [`Owner`] drops.
	pub fn spawn(&self, task: impl Future<Output = ()> + Send + 'static) -> Owner {
		self.spawner.spawn(task)
	}

	/// The playlist window duration every rendition's window is trimmed to.
	pub fn window(&self) -> Duration {
		self.feed.read().window
	}

	/// The discontinuity sequence of rendition `key`'s next segment, which starts a new one when
	/// `broken` (see [`Discontinuities::stamp`]). Anchors the presentation at a segment ending at
	/// `end`, if nothing has.
	pub fn stamp(&self, key: &Key, broken: bool, end: Duration) -> u64 {
		let mut feed = self.feed.write().unwrap_or_else(|_| unreachable!("fanout is open"));
		if feed.anchor.is_none() {
			feed.anchor = Some(kio::clock::wall().checked_sub(end).unwrap_or(Wall::UNIX_EPOCH));
		}
		feed.discontinuities.stamp(key, broken)
	}

	/// Why the catalog failed, if it did.
	pub fn failure(&self) -> Option<crate::Error> {
		self.feed.read().failed.clone()
	}

	/// List every rendition under `generation` from now on.
	///
	/// Replacing a generation drops what was listed under it and the inits built from it: those
	/// belong to the run it named. The first one only labels the run already flowing, so an
	/// embedder that supplies it right after construction loses nothing to a race with the first
	/// records.
	pub fn set_generation(&self, generation: Option<Arc<str>>) {
		let mut feed = self.feed.write().unwrap_or_else(|_| unreachable!("fanout is open"));
		if feed.generation == generation {
			return;
		}
		let restart = feed.generation.is_some();
		feed.generation = generation.clone();
		if restart {
			feed.discontinuities.restart();
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

	/// Fail every window with `err`, so no playlist reads as finished.
	fn fail(&self, err: crate::Error) {
		let mut feed = self.feed.write().unwrap_or_else(|_| unreachable!("fanout is open"));
		if feed.failed.is_some() {
			return;
		}
		feed.failed = Some(err.clone());
		feed.targets.retain(|target| {
			let Some(rendition) = target.upgrade() else {
				return false;
			};
			rendition.fail(err.clone());
			rendition.close();
			true
		});
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

	/// Start every enabled video rendition's init build, then resolve once one of them has its
	/// init and is [advertised](Rendition::is_advertised), so a manifest lists it, or none is left
	/// that listings do not refuse: a denied or failed one can never be listed. A build that has
	/// nothing to read from yet starts on the wake that indexes its media.
	#[cfg_attr(
		not(feature = "server"),
		allow(dead_code, reason = "only the `server` feature uses it")
	)]
	pub(crate) async fn video_init(&self) {
		kio::wait(|waiter| {
			let poll = self.state.poll_ref(waiter, |current| {
				let mut video = current
					.renditions
					.values()
					.filter(|rendition| {
						rendition.kind == Kind::Video
							&& rendition.is_enabled()
							&& rendition.poll_refused(waiter).is_pending()
					})
					.peekable();
				if video.peek().is_none()
					|| video.any(|rendition| {
						// Rerun on every wake: `poll_refused` registered `waiter` on its records, so
						// the rows a build needs wake this wait to start it.
						rendition.start_init();
						rendition.poll_init(waiter).is_ready() && rendition.poll_advertised(waiter).is_ready()
					}) {
					Poll::Ready(())
				} else {
					Poll::Pending
				}
			});
			match poll {
				Poll::Ready(Ok(())) => Poll::Ready(()),
				// A closed catalog adds no rendition, but the existing ones' inits keep arriving,
				// and the predicate registered `waiter` on each of them.
				Poll::Ready(Err(_)) | Poll::Pending => Poll::Pending,
			}
		})
		.await
	}

	/// Enroll freshly created renditions with what their siblings share, and start following
	/// their own records.
	///
	/// A playlist URI keeps one `EXT-X-TARGETDURATION` while its rendition stays in the catalog
	/// (RFC 8216 6.2.1). The target is derived rather than stored per name, so a publisher
	/// churning names can't grow the feed: a new rendition gets the larger of its own declared
	/// duration and the [baseline](Feed::baseline), the longest the first batch declared.
	/// Renditions listed first all agree. A reconfigured rendition's replacement keeps its
	/// predecessor's (see [`Rendition::after`]), which already covers the baseline. One that
	/// leaves and returns gets the target its declaration derives: the same unless a reconfigure
	/// had kept an older one.
	fn register(&self, added: Vec<Rendition>) -> Vec<Arc<Rendition>> {
		// Most catalog updates only revise estimates; writing the feed would wake every cursor.
		if added.is_empty() {
			return Vec::new();
		}
		let mut feed = self
			.fanout
			.feed
			.write()
			.unwrap_or_else(|_| unreachable!("fanout is open"));
		let feed = &mut *feed;
		// Fixed by the first batch, while each target is still its own declared duration.
		let baseline = *feed
			.baseline
			.get_or_insert_with(|| added.iter().map(Rendition::target_duration).max().unwrap_or(1));
		let added: Vec<_> = added
			.into_iter()
			.map(|rendition| {
				let target_duration = rendition.target_duration().max(baseline);
				let rendition = Arc::new(rendition.pin(target_duration));
				rendition.label(feed.generation.clone());
				if let Some(err) = &feed.failed {
					rendition.fail(err.clone());
					rendition.close();
				} else {
					rendition.watch();
				}
				feed.targets.push(Arc::downgrade(&rendition));
				rendition
			})
			.collect();
		// Pruned here, so a catalog that keeps replacing renditions holds only the live ones. A
		// replacement shares its predecessor's name, so its count survives.
		feed.targets.retain(|target| target.strong_count() > 0);
		let live: BTreeSet<Key> = feed
			.targets
			.iter()
			.filter_map(Weak::upgrade)
			.map(|rendition| (rendition.kind, rendition.name.clone()))
			.collect();
		feed.discontinuities.renditions.retain(|key, _| live.contains(key));
		added
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
	/// A rendition joins the set only once its entry advertises HLS records (its `hls` field),
	/// which an unhinted publisher declares after its first segment: without them there is
	/// nothing to render its playlist from, so it is neither listed nor reported as
	/// [`Event::Added`].
	///
	/// `source` holds the broadcast the snapshot was read from, so each rendition serves media
	/// from that same broadcast rather than whatever is at the path by the time it is asked. A
	/// rendition whose `broadcast` reference escapes above the origin root names no broadcast at
	/// all, so it is dropped with a warning rather than served. So is a rendition named `""`, `.`, or
	/// `..`, which no URL path segment can address, so the master playlist, the DASH manifest, and
	/// the router agree on what is served.
	pub fn sync(&self, source: &moq_mux::Source, catalog: &Catalog) {
		let clock = catalog.clock;

		let Ok(mut current) = self.state.write() else {
			return;
		};

		// Renditions the catalog dropped or reconfigured. Retire each as it goes so any cursor
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
		// A replacement lists the records its retired predecessor won't, so no record is muxed
		// under both configs.
		let mut handoffs = BTreeMap::new();
		for key in stale {
			if let Some(rendition) = current.renditions.remove(&key) {
				handoffs.insert(key, rendition.retire());
			}
		}

		let mut added = Vec::new();
		for (name, video) in &catalog.video.renditions {
			if !master::is_path_segment(name) {
				tracing::warn!(
					rendition = name,
					"ignoring video rendition not named as a URL path segment"
				);
				continue;
			}
			let key = (Kind::Video, name.clone());
			if let Some(rendition) = current.renditions.get(&key) {
				// Survived the stale pass, so it decodes the same: keep its window, its cached
				// init segment and its media sequence, and just take the new bitrate, framerate,
				// and `enabled` flag.
				rendition.refresh(video.bitrate, video.framerate, video.enabled);
				continue;
			}
			let Some(hls) = video.hls.clone() else {
				continue;
			};
			match Rendition::video(name.clone(), video, source, hls, clock, self.fanout()) {
				Ok(rendition) => added.push(rendition.after(handoffs.get(&key))),
				Err(err) => tracing::warn!(rendition = name, %err, "ignoring unservable video rendition"),
			}
		}
		for (name, audio) in &catalog.audio.renditions {
			if !master::is_path_segment(name) {
				tracing::warn!(
					rendition = name,
					"ignoring audio rendition not named as a URL path segment"
				);
				continue;
			}
			let key = (Kind::Audio, name.clone());
			if let Some(rendition) = current.renditions.get(&key) {
				rendition.refresh(audio.bitrate, None, audio.enabled);
				continue;
			}
			let Some(hls) = audio.hls.clone() else {
				continue;
			};
			match Rendition::audio(name.clone(), audio, source, hls, clock, self.fanout()) {
				Ok(rendition) => added.push(rendition.after(handoffs.get(&key))),
				Err(err) => tracing::warn!(rendition = name, %err, "ignoring unservable audio rendition"),
			}
		}
		for rendition in self.register(added) {
			current
				.renditions
				.insert((rendition.kind, rendition.name.clone()), rendition);
		}
		if current.renditions.is_empty()
			&& (!catalog.video.renditions.is_empty() || !catalog.audio.renditions.is_empty())
		{
			tracing::debug!("no rendition advertises HLS records yet; nothing to serve");
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
	/// The next rendition change; `None` only after the catalog finishes cleanly.
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
		// A failed catalog cannot wait for the change stream to close.
		if let Poll::Ready(Ok(err)) = self
			.feed
			.poll(waiter, |feed| feed.failed.clone().map_or(Poll::Pending, Poll::Ready))
		{
			return Poll::Ready(Err(err));
		}
		match self
			.state
			.poll(waiter, |current| next_event(&current.renditions, &self.seen))
		{
			Poll::Ready(Ok(event)) => Poll::Ready(Ok(Some(event))),
			Poll::Ready(Err(state)) => Poll::Ready(match &state.finished {
				Some(result) => result.clone().map(|()| None),
				None => Err(crate::Error::IncompleteRecording),
			}),
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
	async fn cursors_end_only_with_a_finished_catalog() {
		tokio::time::pause();
		let producer = Producer::spawned(Duration::from_secs(30));
		let mut cursor = producer.subscribe();
		assert!(cursor.poll_next(&kio::Waiter::noop()).is_pending());
		producer.finish(Ok(()));
		assert!(cursor.next().await.unwrap().is_none());
		assert!(cursor.next().await.unwrap().is_none());

		let producer = Producer::spawned(Duration::from_secs(30));
		let mut cursor = producer.subscribe();
		producer.close();
		assert!(matches!(cursor.next().await, Err(crate::Error::IncompleteRecording)));
		assert!(matches!(cursor.next().await, Err(crate::Error::IncompleteRecording)));

		let producer = Producer::spawned(Duration::from_secs(30));
		let mut cursor = producer.subscribe();
		drop(producer);
		assert!(matches!(cursor.next().await, Err(crate::Error::IncompleteRecording)));
	}

	#[test]
	fn each_rendition_counts_its_own_discontinuities() {
		let video = (Kind::Video, "video".to_string());
		let audio = (Kind::Audio, "audio".to_string());
		let mut breaks = Discontinuities::default();
		assert_eq!(breaks.stamp(&video, false), 0);
		assert_eq!(
			breaks.stamp(&video, true),
			1,
			"the segment after a stop starts a new one"
		);
		assert_eq!(
			breaks.stamp(&audio, false),
			0,
			"a break in one rendition leaves the others"
		);
		breaks.restart();
		assert_eq!(breaks.stamp(&video, false), 2);
		assert_eq!(breaks.stamp(&audio, false), 1);
		let late = (Kind::Video, "late".to_string());
		assert_eq!(
			breaks.stamp(&late, false),
			1,
			"a rendition first seen starts at the run"
		);
	}

	#[tokio::test]
	async fn a_failed_catalog_never_completes_a_cursor() {
		tokio::time::pause();
		let producer = Producer::spawned(Duration::from_secs(30));
		let mut cursor = producer.subscribe();
		producer.finish(Err(crate::Error::NoVariants));
		assert!(matches!(
			cursor.next().await,
			Err(crate::Error::CatalogFailed { source }) if matches!(*source, crate::Error::NoVariants)
		));
		assert!(producer.failure().is_some());
	}
}
