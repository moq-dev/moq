//! Export: serve a MoQ broadcast as HLS or DASH, fetching media on demand.
//!
//! A [`Broadcaster`] subscribes to one broadcast's catalog and each rendition's timeline track
//! (see [`hang::timeline`]). That is the *only* standing traffic: playlists and manifests are a
//! pure function of the timeline records, and media bytes move only when an HTTP client requests
//! a segment, which FETCHes exactly the frames that segment covers from the relay cache and
//! transmuxes them to CMAF. A broadcast whose catalog advertises no timelines can't be served this
//! way and is skipped, and a rendition is served only once its own timeline is advertised. A `moq-archive` recording replayed onto a broadcast is served the same way:
//! its playlists read only the stored timelines, and a segment GETs only its rendition's objects.
//!
//! Segment boundaries come from one reference rendition's records, numbered by record sequence,
//! so edges that share the choice agree without the publisher cutting for HLS. The first choice
//! is the first enabled video rendition, or enabled audio when there is no video. A video
//! choice sticks while it stays enabled with a timeline; audio yields to enabled video. A
//! handoff continues past the last advertised record sequence, starting a DASH period. Every
//! other rendition resolves each segment against its own timeline.
//! An inline-parameter-set codec with no catalog `description` GETs one keyframe
//! group on the first playlist render to build its init, then caches it.
//!
//! The same machinery serves two kinds of consumer:
//!
//! * the HTTP serve path (pull): [`Broadcaster::rendition`] /
//!   [`Broadcaster::master_playlist`] / [`Broadcaster::manifest`] and
//!   [`Rendition::playlist`] / [`Rendition::init`] / [`Rendition::segment`], rendered or
//!   fetched per request, with or without the `server` feature's router; and
//! * a recorder (push): the [`renditions::Consumer`] and [`segments::Consumer`] cursors, which
//!   yield every rendition and every finalized segment in order, for mirroring a broadcast to
//!   storage.

#[cfg(test)]
#[expect(clippy::disallowed_methods, reason = "tests read the host clock directly")]
mod archive_tests;
mod history;
pub mod master;
mod mpd;
mod playlist;
mod rendition;
mod spans;

pub mod renditions;
pub mod segments;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use moq_mux::catalog::{self, CatalogFormat, Stream};
use rand::RngExt;

pub(crate) use playlist::render_media;
pub use rendition::{Kind, Rendition};
use segments::Reference;

type TimelineWatchers =
	Arc<Mutex<std::collections::BTreeMap<segments::ReferenceKey, (Reference, tokio::task::JoinHandle<()>)>>>;

/// A half-open media-time range for an HLS playlist.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Range {
	/// The first requested media time.
	start: Duration,
	/// The first media time outside the request.
	end: Duration,
}

impl Range {
	/// Build a non-empty media-time range.
	pub fn new(start: Duration, end: Duration) -> crate::Result<Self> {
		if start >= end {
			return Err(crate::Error::RangeUnavailable("start must be before end".into()));
		}
		Ok(Self { start, end })
	}
}

/// Backoff bounds for the initial catalog subscription.
///
/// The usual failure is a publisher that has announced its broadcast but not yet written its
/// catalog track, so this waits for external state rather than repeating a failed request.
/// Escalating for that reason: a source that stays silent for an hour must not be polled four times
/// a second for an hour.
///
/// Deliberately no give-up budget, and no attempt to judge which failures are worth waiting on. The
/// broadcast closing is what ends the wait, and a relay-side broadcast outlives its publisher's
/// session, so any deadline here is a window in which a publisher outage leaves the broadcaster
/// permanently empty with nothing to recover it.
const CATALOG_RETRY_MIN: Duration = Duration::from_millis(250);
const CATALOG_RETRY_MAX: Duration = Duration::from_secs(5);

/// Export tuning shared across renditions.
///
/// Construct via [`Config::default`] and set the fields you need, so new options
/// stay additive.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Config {
	/// Minimum duration of media listed in each rendition's playlist window. Older timeline
	/// records are evicted once the remaining segments still cover this span, except the newest
	/// one a video player can start at, so a GOP longer than the window stretches it. Past 256
	/// segments they are evicted however short they are. Keep it within the relay's group-cache retention, since
	/// segments are fetched from there on request. It also caps segment `Cache-Control: max-age`.
	///
	/// A durable timeline gets the same window: a live playlist stays bounded however much its
	/// store retains. See [`history`](Self::history) to list all of it.
	pub window: Duration,

	/// List a durable timeline past the window, trimmed only by the timeline's own pops, so a
	/// viewer can seek back through a recording. Off by default.
	///
	/// A timeline is durable when its catalog `archive` entry names a `store` and no `replay`
	/// path, so every range it lists stays FETCHable from this broadcast. Other timelines keep
	/// the window, since their segments leave the relay cache.
	///
	/// Durable history reads retained timeline groups from the beginning, including records older
	/// than the checkpoint a live join receives. Live playlists keep their bounded join. A
	/// recording whose start was pruned by retention is refused with [`crate::Error::HistoryPruned`].
	/// Across reference changes, retention retires a common playlist prefix so remaining
	/// segments keep their HLS sequence positions.
	pub history: bool,
}

impl Default for Config {
	fn default() -> Self {
		Self {
			window: Duration::from_secs(16),
			history: false,
		}
	}
}

/// All renditions of one broadcast, kept in sync with its catalog.
pub struct Broadcaster {
	#[cfg_attr(
		not(feature = "server"),
		allow(
			dead_code,
			reason = "read only by the serve path's `is_closed`/`closed` (server-gated); a recording consumer observes close through its `renditions()` cursor ending instead"
		)
	)]
	broadcast: moq_net::broadcast::Consumer,
	/// The publisher instance served, which every URL the master playlist and manifest list
	/// starts with: its epoch, or an id minted for an epochless instance.
	instance: Arc<str>,
	/// The current rendition set, reconciled from the catalog by the watcher task.
	renditions: renditions::Producer,
	/// Aborts the catalog watcher when the broadcaster is dropped; `Drop` then retires the
	/// renditions themselves (see [`renditions::Producer::clear`]). Set once, right after
	/// construction.
	watcher: Mutex<Option<tokio::task::JoinHandle<()>>>,
	/// Reference timeline readers. History mode retains past references to follow their pops.
	/// On drop they are aborted only when no recording cursor still needs their records.
	timeline_watchers: TimelineWatchers,
}

impl Broadcaster {
	/// Start tracking the renditions of `source`'s catalog broadcast.
	pub fn new(source: moq_mux::Source, config: Config) -> Arc<Self> {
		let broadcast = source.broadcast();
		// An epoch names the instance on every edge, so each serves it under the same URLs.
		let instance = broadcast.info().epoch.clone().unwrap_or_else(moq_net::Epoch::mint);
		let renditions = renditions::Producer::new(config.window);
		let timeline_watchers = Arc::new(Mutex::new(std::collections::BTreeMap::new()));
		let broadcaster = Arc::new(Self {
			broadcast,
			instance: Arc::from(instance.as_str()),
			renditions: renditions.clone(),
			watcher: Mutex::new(None),
			timeline_watchers: timeline_watchers.clone(),
		});
		// The watcher owns its own producer clone; the `Broadcaster`'s `Drop` aborts it so the
		// standing catalog subscription stops when nobody's serving from this broadcaster.
		let watcher = tokio::spawn(watch_catalog(source, renditions, timeline_watchers, config.history));
		*broadcaster.watcher.lock().unwrap() = Some(watcher);
		broadcaster
	}

	/// The publisher instance every listed URL is under (`@{instance}/{kind}/{rendition}/...`).
	#[cfg_attr(
		not(feature = "server"),
		allow(dead_code, reason = "only the `server` feature routes by instance")
	)]
	pub(crate) fn instance(&self) -> &Arc<str> {
		&self.instance
	}

	/// Whether the source broadcast has closed (ended or dropped).
	pub fn is_closed(&self) -> bool {
		self.broadcast.is_closed()
	}

	/// Resolve once the source broadcast closes, so a pool can evict a dead broadcaster.
	pub async fn closed(&self) {
		self.broadcast.closed().await;
	}

	/// [`closed`](Self::closed), polled: ready once the source broadcast closes.
	pub(crate) fn poll_closed(&self, waiter: &kio::Waiter) -> std::task::Poll<()> {
		self.broadcast.poll_closed(waiter)
	}

	/// Look up a rendition by kind and name. Video and audio are separate axes, so a
	/// video and an audio rendition may share a name without colliding. A rendition is found
	/// only once the catalog advertises its own timeline.
	pub fn rendition(&self, kind: Kind, name: &str) -> Option<Arc<Rendition>> {
		self.renditions.get(kind, name)
	}

	/// A cursor over rendition add/remove events, for a consumer that mirrors or records the
	/// whole broadcast. It replays the current renditions, then yields changes, and returns
	/// `Ok(None)` only once the source finishes cleanly. A rendition is added once the catalog
	/// advertises its own timeline, which an unhinted publisher declares after its first record.
	pub fn renditions(&self) -> renditions::Consumer {
		self.renditions.subscribe()
	}

	/// Resolve once at least one rendition has a playable media playlist or a settled failure.
	/// How long to wait is the caller's policy: wrap this in a timeout as needed.
	pub async fn ready(&self) {
		self.renditions.ready().await;
	}

	/// Resolve once the master playlist lists at least one rendition.
	#[cfg_attr(
		not(feature = "server"),
		allow(dead_code, reason = "only the `server` feature uses it")
	)]
	pub(crate) async fn advertised(&self) {
		self.renditions.advertised().await;
	}

	/// Version every segment URL with `generation`, a label for the publisher run the
	/// broadcast carries now, or stop versioning with `None` (the default).
	///
	/// A restarted publisher is spliced into the same broadcast and restarts its segment
	/// numbers, so `seg/{reference}.0.m4s` would name different bytes on each run. Supply a new generation
	/// before the new run's media flows. Replacing one drops every listed segment and every
	/// init built from the old run's media, while the first only labels the run already flowing. A segment URL carrying any other generation
	/// is refused. Init URLs need none: they carry a hash of their bytes.
	///
	/// Fails unless `generation` is non-empty ASCII letters, digits, `-`, and `_`.
	pub fn set_generation(&self, generation: Option<&str>) -> crate::Result<()> {
		if let Some(generation) = generation
			&& (generation.is_empty()
				|| !generation
					.bytes()
					.all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'))
		{
			return Err(crate::Error::InvalidGeneration(generation.to_string()));
		}
		self.renditions.fanout().set_generation(generation.map(Arc::from));
		Ok(())
	}

	/// Render the multivariant (master) playlist from the current renditions.
	///
	/// `query` is an optional query string (without the leading `?`, e.g. `jwt=<token>`)
	/// appended to every child media-playlist URL, so a credential the master was fetched
	/// with propagates to the rendition playlists a stock player loads next. It is an
	/// argument rather than a [`Config`] field because one broadcaster fans out to viewers
	/// holding different tokens; a credential in `Config` would embed one viewer's in
	/// another's playlist.
	///
	/// Lists only enabled renditions a player can start: a video rendition appears once a listed
	/// segment starts at a group start that is a sync point, so an early render may list audio
	/// alone, or nothing at all. A disabled rendition (catalog `enabled: false`) is left out until
	/// it re-enables, while its media playlist keeps serving.
	pub fn master_playlist(&self, query: Option<&str>) -> String {
		let (video, audio) = self.variants(query);
		master::render(&video, &audio)
	}

	/// The variants [`master_playlist`](Self::master_playlist) lists.
	pub(crate) fn variants(&self, query: Option<&str>) -> (Vec<master::VideoVariant>, Vec<master::AudioVariant>) {
		let mut video = Vec::new();
		let mut audio = Vec::new();
		for rendition in self.renditions.snapshot() {
			if !rendition.is_enabled() || !rendition.is_advertised() {
				continue;
			}
			match rendition.kind {
				Kind::Video => video.push(master::VideoVariant {
					uri: master::rendition_uri(&self.instance, Kind::Video, &rendition.name, query),
					bandwidth: rendition.bandwidth(),
					width: rendition.width,
					height: rendition.height,
					codec: rendition.codec.clone(),
				}),
				Kind::Audio => audio.push(master::AudioVariant {
					name: rendition.name.clone(),
					uri: master::rendition_uri(&self.instance, Kind::Audio, &rendition.name, query),
					bandwidth: rendition.bandwidth(),
					codec: rendition.codec.clone(),
				}),
			}
		}
		(video, audio)
	}

	/// Render the DASH manifest (MPD) from the current renditions and their views of the
	/// broadcast timeline, or `None` while nothing is listable yet (every `SegmentTimeline`
	/// would be empty, which players reject).
	///
	/// Live broadcasts render a `dynamic` presentation anchored at the catalog's declared
	/// wall clock (or, absent one, at an anchor estimated from the first record's arrival); a
	/// finished broadcast renders `static`. `query` propagates to every child URL exactly as
	/// in [`master_playlist`](Self::master_playlist).
	///
	/// A disabled rendition is left out, as in the master playlist. Each representation names
	/// its init by a hash of the bytes, so a rendition is left out until its init is known. An inline-parameter-set codec only learns it from media: await
	/// each rendition's [`init`](Rendition::init) first.
	///
	/// Fails with [`TimelineFailed`](crate::Error::TimelineFailed) once the reference timeline
	/// failed, rather than turning a truncated presentation `static`. A rendition whose own
	/// timeline failed is left out instead, and its [`Rendition`] listings fail.
	pub fn manifest(&self, query: Option<&str>) -> crate::Result<Option<String>> {
		if let Some(err) = self.renditions.failure() {
			return Err(err);
		}
		let mut video = Vec::new();
		let mut audio = Vec::new();
		let mut availability_start = None;
		for rendition in self.renditions.snapshot() {
			// Every rendition shares the catalog's one root clock, so any of them can
			// supply the declared wall anchor.
			if availability_start.is_none() {
				availability_start = rendition.wall_clock(moq_net::Timestamp::ZERO);
			}
			if !rendition.is_enabled() {
				continue;
			}
			// Without its init there is nothing to name in `initialization`; the serve path
			// builds every init before rendering (see `build_inits`).
			let representations = match rendition.representations() {
				Ok(representations) => representations,
				// Its own playlist answers the error, while its siblings stay listed.
				Err(_) if rendition.failed_alone() => continue,
				Err(err) => return Err(err),
			};
			match rendition.kind {
				Kind::Video => video.extend(representations),
				Kind::Audio => audio.extend(representations),
			}
		}

		let representations = || video.iter().chain(&audio);
		if representations().all(|rep| rep.segments.is_empty()) {
			return Ok(None);
		}
		// Static only once every representation is final, so none stops being refreshed early.
		let finished = representations().all(|rep| rep.ended);
		if !finished {
			// A dynamic presentation needs pts 0 anchored to the wall clock. The fallback is
			// set whenever a record has been pushed, which listing a segment implies.
			availability_start = availability_start.or_else(|| self.renditions.anchor());
			if availability_start.is_none() {
				return Ok(None);
			}
		}

		Ok(Some(mpd::render_manifest(
			&mpd::Manifest {
				instance: self.instance.clone(),
				availability_start,
				publish: moq_clock::wall(),
				window: self.renditions.window(),
				finished,
				video,
				audio,
			},
			query,
		)))
	}

	/// Build every rendition's init segment, so the manifest can name each by its hash. A
	/// rendition whose init can't be built yet is left out of the render.
	#[cfg_attr(
		not(feature = "server"),
		allow(dead_code, reason = "only the `server` feature uses it")
	)]
	pub(crate) async fn build_inits(&self) {
		for rendition in self.renditions.snapshot() {
			if let Err(err) = rendition.init().await {
				tracing::warn!(rendition = %rendition.name, %err, "failed to build init segment");
			}
		}
	}

	/// Resolve once the broadcast has something listable (its first complete segment, or an
	/// already-ended timeline). Every rendition's window is fed from the same timeline, so the
	/// first rendition's readiness stands in for the broadcast's. Bounding the wait is the
	/// caller's policy.
	#[cfg_attr(
		not(feature = "server"),
		allow(dead_code, reason = "only the `server` feature uses it")
	)]
	pub(crate) async fn playable(&self) {
		if let Some(rendition) = self.renditions.snapshot().into_iter().next() {
			rendition.playable().await;
		}
	}

	/// Whether the current catalog contains no servable renditions (serve path).
	#[cfg_attr(
		not(feature = "server"),
		allow(dead_code, reason = "only the `server` feature uses it")
	)]
	pub(crate) fn is_empty(&self) -> bool {
		self.renditions.is_empty()
	}
}

impl Drop for Broadcaster {
	fn drop(&mut self) {
		if let Some(watcher) = self.watcher.lock().unwrap().take() {
			watcher.abort();
		}
		// A recording cursor (holding an `Arc<Rendition>`) must keep receiving timeline
		// records after this broadcaster is gone, or its final segments would be truncated;
		// the timeline watcher then ends on its own when the timeline (or broadcast) does.
		// With no such holder, abort it now so teardown can't leak a standing subscription.
		let held = self.renditions.any_held_externally();
		// Release the map either way: a surviving `renditions::Consumer` would otherwise pin
		// every `Rendition` forever.
		self.renditions.clear();
		if !held {
			for (_, (_, watcher)) in std::mem::take(&mut *self.timeline_watchers.lock().unwrap()) {
				watcher.abort();
			}
		}
	}
}

async fn watch_catalog(
	source: moq_mux::Source,
	renditions: renditions::Producer,
	timeline_watchers: TimelineWatchers,
	history: bool,
) {
	// The source's catalog broadcast, so the catalog, the timeline subscription, and the closed
	// check below all refer to the one broadcast it was built from.
	let broadcast = &source.broadcast();
	let mut delay = CATALOG_RETRY_MIN;
	let mut consumer = loop {
		match catalog::Consumer::<()>::new(broadcast, CatalogFormat::Hang).await {
			Ok(consumer) => break consumer,
			Err(err) => {
				tracing::warn!(%err, "failed to subscribe to broadcast catalog, retrying");
				let wait = delay.mul_f64(0.5 + rand::rng().random::<f64>() / 2.0);
				delay = (delay * 2).min(CATALOG_RETRY_MAX);
				tokio::select! {
					_ = moq_clock::sleep(wait) => {}
					_ = kio::wait(|waiter| broadcast.poll_closed(waiter)) => {
						renditions.close();
						return;
					}
				}
			}
		}
	};

	let mut unbound = false;
	let mut reference: Option<Reference> = None;
	let result = loop {
		match kio::wait(|waiter| consumer.poll_next(waiter)).await {
			Ok(Some(catalog)) => {
				// Decide the playlists' retention before any rendition starts following its timeline.
				if history && !unbound && catalog.archive.as_ref().is_some_and(durable) {
					unbound = true;
					renditions.fanout().unbound();
				}
				renditions.sync(&source, &catalog);

				// Keep an enabled reference, preserving every row already advertised on a handoff.
				let next = self::reference(&catalog, reference.as_ref());
				if next != reference {
					let mut watchers = timeline_watchers.lock().unwrap();
					renditions.fanout().select(next.clone());
					if !unbound {
						for (_, (_, watcher)) in std::mem::take(&mut *watchers) {
							watcher.abort();
						}
					}
					// History rows stay listed until their original timeline pops them, so retain
					// those readers after a handoff. Selection suppresses their pushes and finish.
					// A reselected reference gets a fresh reader: the retained one consumed records
					// while unselected, and only a replay restates them.
					if let (Some(next), Some(archive)) = (&next, catalog.archive.clone()) {
						if let Some((_, watcher)) = watchers.remove(next.as_ref()) {
							watcher.abort();
						}
						let watcher = tokio::spawn(watch_timeline(
							broadcast.clone(),
							archive,
							next.clone(),
							renditions.fanout(),
							unbound,
						));
						watchers.insert(next.as_ref().clone(), (next.clone(), watcher));
					}
					reference = next;
				}
			}
			Ok(None) => break Ok(()),
			Err(err) => {
				tracing::warn!(%err, "broadcast catalog stream ended with error");
				break Err(err.into());
			}
		}
	};

	// With every rendition disabled, no reference watcher is left to end the windows.
	// Only a clean catalog finish confirms completion; a catalog failure fails the windows.
	if reference.is_none() && result.is_ok() {
		renditions.fanout().finish(None);
	}
	renditions.finish(result);
}

/// The rendition every segment boundary comes from, with the track its timeline indexes.
///
/// The first choice is the first enabled video rendition with a timeline, falling back to
/// enabled audio. Keep an enabled video choice while its timeline stays indexed, so a newly
/// added rendition cannot take it over. Audio yields once enabled video becomes available.
/// A handoff resumes at the replacement's first record numbered past the last advertised one,
/// and starts a DASH period.
fn reference(catalog: &moq_mux::catalog::hang::Catalog, current: Option<&Reference>) -> Option<Reference> {
	use hang::catalog::Reference as _;

	let archive = catalog.archive.as_ref()?;
	let indexed = |track: &&str| archive.timelines.contains_key(*track);
	let video = catalog
		.video
		.renditions
		.iter()
		.filter(|(_, config)| config.enabled)
		.map(|(id, config)| (Kind::Video, id, config.track(id)))
		.find(|(_, _, track)| indexed(track));
	if let Some(current) = current {
		let (kind, id, _) = current.as_ref();
		let track = match kind {
			Kind::Video => catalog
				.video
				.renditions
				.get(id)
				.filter(|config| config.enabled)
				.map(|config| config.track(id)),
			Kind::Audio if video.is_none() => catalog
				.audio
				.renditions
				.get(id)
				.filter(|config| config.enabled)
				.map(|config| config.track(id)),
			Kind::Audio => None,
		};
		if let Some(track) = track.filter(indexed) {
			return Some(Arc::new((*kind, id.clone(), track.to_string())));
		}
	}
	let audio = || {
		catalog
			.audio
			.renditions
			.iter()
			.filter(|(_, config)| config.enabled)
			.map(|(id, config)| (Kind::Audio, id, config.track(id)))
			.find(|(_, _, track)| indexed(track))
	};
	video
		.or_else(audio)
		.map(|(kind, id, track)| Arc::new((kind, id.clone(), track.to_string())))
}

/// Whether every range `archive` advertises stays FETCHable from this broadcast until the
/// timeline pops it: a store makes the ranges durable, and no `replay` path means this
/// broadcast serves them. The catalog states this, so [`Config::history`] can follow the
/// timeline's own retention rather than a window sized for relay caches.
fn durable(archive: &hang::catalog::Archive) -> bool {
	archive.store.is_some() && archive.replay.is_none()
}

/// The segment watcher: read the reference rendition's timeline and fan each record out to every
/// rendition's window as a segment.
async fn watch_timeline(
	broadcast: moq_net::broadcast::Consumer,
	section: hang::catalog::Archive,
	reference: Reference,
	renditions: renditions::Fanout,
	history: bool,
) {
	// A failed timeline is never retried: the origin already rides out transient source
	// failures, so an error here is final. It fails every playlist rather than ending it, so a
	// truncated recording never reads as complete, nor freezes live with no signal. A retained
	// history reader fails them too: its rows stay listed, and it can no longer report their pops.
	match watch(&broadcast, &section, &reference, &renditions, history).await {
		// A superseded watcher cannot finish the newly selected reference's windows.
		Ok(()) => {
			renditions.finish(Some(&reference));
		}
		Err(err) => {
			let err = crate::Error::TimelineFailed {
				track: reference.2.clone(),
				source: Box::new(err),
			};
			if renditions.fail(Some(&reference), err.clone()) {
				tracing::error!(%err, "reference timeline failed; failing every playlist");
			}
		}
	}
}

async fn watch(
	broadcast: &moq_net::broadcast::Consumer,
	section: &hang::catalog::Archive,
	reference: &Reference,
	renditions: &renditions::Fanout,
	history: bool,
) -> crate::Result<()> {
	let track = &reference.2;
	if history {
		return history::read(broadcast, section, track, |edit| match edit {
			history::Edit::Push { index, entry } => renditions.push(index, entry, reference),
			history::Edit::Pop(range) => renditions.pop(range, reference),
		})
		.await;
	}
	let mut timeline = moq_mux::timeline::Consumer::<()>::subscribe(broadcast, section, track).await?;
	while let Some(event) = timeline.next().await? {
		match event {
			moq_mux::timeline::Event::Push { index, entry } => renditions.push(index, entry, reference),
			moq_mux::timeline::Event::Pop(range) => renditions.pop(range, reference),
			moq_mux::timeline::Event::Skip(_) => renditions.skip(Some(reference)),
			_ => unreachable!("unknown timeline event"),
		}
	}
	Ok(())
}

#[cfg(test)]
#[expect(
	clippy::disallowed_methods,
	clippy::disallowed_types,
	reason = "tests read the host clock directly"
)]
mod tests {
	/// A millisecond track: the frames written here carry timestamps.
	fn timed() -> moq_net::track::Info {
		moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI)
	}

	/// Build an origin producer, spawning its driver on the ambient runtime.
	fn produce_origin() -> moq_net::origin::Producer {
		let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::default());
		if tokio::runtime::Handle::try_current().is_ok() {
			tokio::spawn(moq_net::time::run(driver));
		} else {
			// A sync test: nothing polls the driver, and dropping it would tear
			// the origin down, so leak it and rely on the synchronous half.
			std::mem::forget(driver);
		}
		producer
	}

	use super::*;

	#[allow(
		clippy::large_enum_variant,
		reason = "a test fixture built once per case; only large when extra features grow the video config"
	)]
	enum TestConfig {
		Video(hang::catalog::VideoConfig),
		Audio(hang::catalog::AudioConfig),
	}

	impl From<hang::catalog::VideoConfig> for TestConfig {
		fn from(config: hang::catalog::VideoConfig) -> Self {
			Self::Video(config)
		}
	}

	impl From<hang::catalog::AudioConfig> for TestConfig {
		fn from(config: hang::catalog::AudioConfig) -> Self {
			Self::Audio(config)
		}
	}

	struct TestRendition {
		catalog: moq_mux::catalog::Producer,
		name: String,
		config: Option<TestConfig>,
	}

	impl TestRendition {
		fn unwrap(self) -> Self {
			self
		}

		fn set(&mut self, config: impl Into<TestConfig>) -> moq_mux::Result<()> {
			let config = config.into();
			let mut catalog = self.catalog.modify()?;
			match &config {
				TestConfig::Video(config) => {
					catalog.video.renditions.insert(self.name.clone(), config.clone());
				}
				TestConfig::Audio(config) => {
					catalog.audio.renditions.insert(self.name.clone(), config.clone());
				}
			}
			catalog.commit()?;
			self.config = Some(config);
			Ok(())
		}
	}

	impl Drop for TestRendition {
		fn drop(&mut self) {
			let Ok(mut catalog) = self.catalog.modify() else {
				return;
			};
			match self.config {
				Some(TestConfig::Video(_)) => {
					catalog.video.renditions.remove(&self.name);
				}
				Some(TestConfig::Audio(_)) => {
					catalog.audio.renditions.remove(&self.name);
				}
				None => {}
			}
		}
	}

	trait RenditionTestExt {
		fn test_video(&self, name: &str) -> TestRendition;
		fn test_audio(&self, name: &str) -> TestRendition;
	}

	impl RenditionTestExt for moq_mux::catalog::Producer {
		fn test_video(&self, name: &str) -> TestRendition {
			TestRendition {
				catalog: self.clone(),
				name: name.to_string(),
				config: None,
			}
		}

		fn test_audio(&self, name: &str) -> TestRendition {
			self.test_video(name)
		}
	}

	trait CatalogTestExt {
		fn enroll_test(&self, track: &str) -> moq_mux::Result<moq_mux::timeline::Recorder>;
		fn media_producer<C: moq_mux::container::Container>(
			&self,
			track: moq_net::track::Producer,
			container: C,
		) -> moq_mux::Result<moq_mux::container::Producer<C>>
		where
			moq_mux::Error: From<C::Error>;
	}

	impl CatalogTestExt for moq_mux::catalog::Producer {
		fn enroll_test(&self, track: &str) -> moq_mux::Result<moq_mux::timeline::Recorder> {
			let duration = Duration::from_secs(2);
			let recorder = self.timeline().track(
				track,
				moq_mux::timeline::Config::default()
					.with_duration_min(duration)
					.with_duration_hint(duration),
			)?;
			let section = self.timeline().section();
			let mut producer = self.clone();
			let mut catalog = producer.modify()?;
			catalog.archive = Some(section);
			catalog.commit()?;
			Ok(recorder)
		}

		fn media_producer<C: moq_mux::container::Container>(
			&self,
			track: moq_net::track::Producer,
			container: C,
		) -> moq_mux::Result<moq_mux::container::Producer<C>>
		where
			moq_mux::Error: From<C::Error>,
		{
			let recorder = self.enroll_test(track.name())?;
			Ok(moq_mux::container::Producer::new(track, container).with_recorder(recorder))
		}
	}

	/// A catalog `archive` entry indexing `tracks` under their conventional timeline names.
	fn archive(tracks: &[&str]) -> hang::catalog::Archive {
		let mut archive = hang::catalog::Archive::new();
		for track in tracks {
			archive.timelines.insert(
				track.to_string(),
				hang::catalog::Timeline::new(hang::timeline::default_name(track), 4_000),
			);
		}
		archive
	}

	fn frame(micros: u64, keyframe: bool) -> moq_mux::container::Frame {
		payload_frame(micros, keyframe, &[0xDE, 0xAD, 0xBE, 0xEF])
	}

	fn payload_frame(micros: u64, keyframe: bool, payload: &'static [u8]) -> moq_mux::container::Frame {
		moq_mux::container::Frame {
			timestamp: moq_net::Timestamp::from_micros(micros).unwrap(),
			payload: bytes::Bytes::from_static(payload),
			keyframe,
			duration: None,
		}
	}

	fn vp8_frame(micros: u64, keyframe: bool) -> moq_mux::container::Frame {
		let payload = if keyframe {
			// Key frame tag, start code, and 320x240 geometry.
			&[0x10, 0x00, 0x00, 0x9d, 0x01, 0x2a, 0x40, 0x01, 0xf0, 0x00][..]
		} else {
			&[0x31, 0x00, 0x00][..]
		};
		moq_mux::container::Frame {
			timestamp: moq_net::Timestamp::from_micros(micros).unwrap(),
			payload: bytes::Bytes::copy_from_slice(payload),
			keyframe,
			duration: None,
		}
	}

	fn video_config() -> hang::catalog::VideoConfig {
		let mut config = hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::VP8);
		config.coded_width = Some(320);
		config.coded_height = Some(240);
		config.framerate = Some(30.0);
		config
	}

	// Let the origin's spawned attach task run so a created broadcast is routable.
	async fn settle() {
		for _ in 0..10 {
			tokio::task::yield_now().await;
		}
	}

	#[tokio::test]
	async fn escaping_broadcast_reference_is_not_advertised() {
		let origin = produce_origin();
		let _broadcast = origin.create_broadcast("a/pub").expect("publish allowed");
		_broadcast.announce(Default::default()).expect("publish allowed");
		settle().await;
		let upstream = crate::resolved(&origin, "a/pub").await;
		let mut config = hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::VP8);
		config.broadcast = Some(moq_net::path::Relative::new("../../source").to_owned());
		let mut catalog = moq_mux::catalog::hang::Catalog::default();
		catalog.archive = Some(archive(&["video0", "audio0"]));
		catalog.video.renditions.insert("video".to_string(), config);

		let renditions = renditions::Producer::new(Config::default().window);
		renditions.sync(&upstream, &catalog);
		assert!(renditions.get(Kind::Video, "video").is_none());
	}

	/// A source over an empty published broadcast, for the `sync` reconciliation tests.
	async fn empty_upstream(origin: &moq_net::origin::Producer, path: &str) -> moq_mux::Source {
		crate::resolved(origin, path).await
	}

	/// A catalog with one video and one audio rendition, both servable.
	fn catalog_with_both() -> moq_mux::catalog::hang::Catalog {
		let mut audio = hang::catalog::AudioConfig::new(
			hang::catalog::AudioCodec::AAC(hang::catalog::AAC { profile: 2 }),
			44_100,
			2,
		);
		audio.description = Some(bytes::Bytes::from_static(&[0x12, 0x10]));
		audio.bitrate = Some(96_000);

		let mut catalog = moq_mux::catalog::hang::Catalog::default();
		catalog.archive = Some(archive(&["video0", "audio0"]));
		catalog.video.renditions.insert("video0".to_string(), video_config());
		catalog.audio.renditions.insert("audio0".to_string(), audio);
		catalog
	}

	#[tokio::test(start_paused = true)]
	async fn audio_renditions_follow_catalog_quality_order() {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let source = moq_mux::Source::new(produce_origin().consume(), broadcast.consume());
		let names = ["a", "b", "c", "d", "e", "f", "g"];
		let mut catalog = moq_mux::catalog::hang::Catalog::default();
		catalog.archive = Some(archive(&names));
		for (name, rate, channels, bitrate) in [
			("a", 48_000, 2, None),
			("b", 48_000, 1, Some(64_000)),
			("c", 16_000, 1, Some(128_000)),
			("d", 48_000, 2, Some(64_000)),
			("e", 48_000, 2, Some(64_000)),
			("f", 44_100, 2, Some(64_000)),
			("g", 16_000, 1, None),
		] {
			let mut config = hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Opus, rate, channels);
			config.bitrate = bitrate;
			catalog.audio.renditions.insert(name.to_string(), config);
		}
		let renditions = renditions::Producer::new(Config::default().window);
		renditions.sync(&source, &catalog);
		assert!(
			!renditions.any_held_externally(),
			"ranking must not count as a recording consumer"
		);
		let listed = || {
			renditions
				.snapshot()
				.iter()
				.map(|rendition| rendition.name.clone())
				.collect::<Vec<_>>()
		};
		assert_eq!(listed(), ["c", "d", "e", "b", "f", "a", "g"]);
		catalog.audio.renditions.get_mut("c").unwrap().enabled = false;
		renditions.sync(&source, &catalog);
		assert_eq!(listed(), ["d", "e", "b", "f", "a", "g", "c"]);
		catalog.audio.renditions.get_mut("g").unwrap().bitrate = Some(256_000);
		renditions.sync(&source, &catalog);
		assert_eq!(listed(), ["g", "d", "e", "b", "f", "a", "c"]);
		renditions.clear();
		assert!(listed().is_empty());
	}

	/// The next rendition event, or `None` if the set is quiet.
	async fn next_event(cursor: &mut renditions::Consumer) -> Option<renditions::Event> {
		tokio::time::timeout(Duration::from_millis(50), cursor.next())
			.await
			.ok()
			.and_then(Result::unwrap)
	}

	/// The average bitrate encoded in a synthesized AAC init segment.
	fn aac_avg_bitrate(init: &bytes::Bytes) -> u32 {
		let wire = moq_mux::container::fmp4::Wire::from_init(init).unwrap();
		let codec = &wire.trak().mdia.minf.stbl.stsd.codecs[0];
		let moq_mux::container::fmp4::mp4_atom::Codec::Mp4a(mp4a) = codec else {
			panic!("expected mp4a, got {codec:?}");
		};
		mp4a.esds.es_desc.dec_config.avg_bitrate
	}

	// A publisher's estimator republishes the catalog every time its measured bitrate or jitter
	// moves, writing those two fields and nothing else. That must not churn the rendition:
	// rebuilding resets the playlist window, the cached init segment and EXT-X-MEDIA-SEQUENCE,
	// and makes every recording cursor see a Removed followed by an Added.
	#[tokio::test]
	async fn estimate_only_catalog_update_keeps_the_rendition() {
		let origin = produce_origin();
		let _broadcast = origin.create_broadcast("live").expect("publish allowed");
		_broadcast.announce(Default::default()).expect("publish allowed");
		settle().await;
		let upstream = empty_upstream(&origin, "live").await;

		let mut catalog = catalog_with_both();
		let renditions = renditions::Producer::new(Config::default().window);
		let mut cursor = renditions.subscribe();
		renditions.sync(&upstream, &catalog);

		let video = renditions.get(Kind::Video, "video0").expect("video rendition");
		let audio = renditions.get(Kind::Audio, "audio0").expect("audio rendition");
		let audio_init = audio.init().await.unwrap().expect("AAC init segment");
		assert_eq!(
			aac_avg_bitrate(&audio_init),
			96_000,
			"the muxer keeps the declared catalog bitrate"
		);
		assert_eq!(video.bandwidth(), 2_000_000, "the catalog carries no video bitrate yet");
		assert_eq!(audio.bandwidth(), 96_000);
		assert!(matches!(
			next_event(&mut cursor).await,
			Some(renditions::Event::Added(_))
		));
		assert!(matches!(
			next_event(&mut cursor).await,
			Some(renditions::Event::Added(_))
		));

		// Exactly what a catalog-owned media producer's estimator writes back.
		let video_config = catalog.video.renditions.get_mut("video0").unwrap();
		video_config.bitrate = Some(1_800_000);
		video_config.jitter = Some(Duration::from_millis(33));
		catalog.audio.renditions.get_mut("audio0").unwrap().bitrate = Some(110_000);
		renditions.sync(&upstream, &catalog);

		assert!(
			Arc::ptr_eq(&video, &renditions.get(Kind::Video, "video0").unwrap()),
			"the video rendition survives an estimate-only update"
		);
		assert!(
			Arc::ptr_eq(&audio, &renditions.get(Kind::Audio, "audio0").unwrap()),
			"the audio rendition survives an estimate-only update"
		);
		assert_eq!(
			video.bandwidth(),
			1_800_000,
			"the master playlist advertises the estimate"
		);
		assert_eq!(audio.bandwidth(), 110_000);
		assert_eq!(
			audio.init().await.unwrap().expect("cached AAC init segment"),
			audio_init,
			"an estimate update preserves the cached init segment"
		);
		assert!(next_event(&mut cursor).await.is_none(), "no rendition churn");

		// A bitrate the publisher retracts falls back to the advertised default.
		catalog.video.renditions.get_mut("video0").unwrap().bitrate = None;
		renditions.sync(&upstream, &catalog);
		assert_eq!(video.bandwidth(), 2_000_000);
	}

	// The estimator also measures the framerate and publishes it once it settles, so an
	// imported source gains one about a second in. Rebuilding for it would re-time the fMP4
	// (its timescale derives from the framerate) and churn every recording cursor mid-stream.
	#[tokio::test]
	async fn estimated_framerate_keeps_the_rendition() {
		let origin = produce_origin();
		let _broadcast = origin.create_broadcast("live").expect("publish allowed");
		_broadcast.announce(Default::default()).expect("publish allowed");
		settle().await;
		let upstream = empty_upstream(&origin, "live").await;

		let mut catalog = catalog_with_both();
		catalog.video.renditions.get_mut("video0").unwrap().framerate = None;
		let renditions = renditions::Producer::new(Config::default().window);
		let mut cursor = renditions.subscribe();
		renditions.sync(&upstream, &catalog);
		let video = renditions.get(Kind::Video, "video0").expect("video rendition");
		while next_event(&mut cursor).await.is_some() {}

		for framerate in [Some(30.0), Some(60.0)] {
			catalog.video.renditions.get_mut("video0").unwrap().framerate = framerate;
			renditions.sync(&upstream, &catalog);
			assert!(
				Arc::ptr_eq(&video, &renditions.get(Kind::Video, "video0").unwrap()),
				"the video rendition survives a framerate estimate of {framerate:?}"
			);
			assert!(next_event(&mut cursor).await.is_none(), "no rendition churn");
		}

		// The settled estimate still reaches the fallback cadence of a duration-less frame,
		// at the timescale the rendition was built with (90 kHz without a framerate).
		let muxer = video.muxer().unwrap();
		assert_eq!(muxer.timescale().as_u64(), 90_000);
		let fragment = muxer.fragment(0, &[vp8_frame(0, true)]).unwrap();
		assert_eq!(first_sample_duration(&fragment), Some(1_500), "16.7 ms at 90 kHz");

		// An NTSC estimate is 1501.5 ticks at 90 kHz: it snaps to whole ticks and still muxes.
		catalog.video.renditions.get_mut("video0").unwrap().framerate = Some(60_000.0 / 1001.0);
		renditions.sync(&upstream, &catalog);
		let fragment = video.muxer().unwrap().fragment(0, &[vp8_frame(0, true)]).unwrap();
		assert_eq!(first_sample_duration(&fragment), Some(1_502));
	}

	/// The `trun` duration of a fragment's first sample.
	fn first_sample_duration(fragment: &bytes::Bytes) -> Option<u32> {
		use moq_mux::container::fmp4::mp4_atom::{self, DecodeMaybe};

		let mut cursor = std::io::Cursor::new(fragment.as_ref());
		while let Some(atom) = mp4_atom::Any::decode_maybe(&mut cursor).unwrap() {
			if let mp4_atom::Any::Moof(moof) = atom {
				return moof.traf[0].trun[0].entries[0].duration;
			}
		}
		panic!("no moof in fragment");
	}

	#[tokio::test]
	async fn estimate_before_first_init_reaches_aac_descriptor() {
		let origin = produce_origin();
		let _broadcast = origin.create_broadcast("live").expect("publish allowed");
		_broadcast.announce(Default::default()).expect("publish allowed");
		settle().await;
		let upstream = empty_upstream(&origin, "live").await;

		let mut catalog = catalog_with_both();
		catalog.audio.renditions.get_mut("audio0").unwrap().bitrate = None;
		let renditions = renditions::Producer::new(Config::default().window);
		renditions.sync(&upstream, &catalog);
		let audio = renditions.get(Kind::Audio, "audio0").expect("audio rendition");

		catalog.audio.renditions.get_mut("audio0").unwrap().bitrate = Some(110_000);
		renditions.sync(&upstream, &catalog);
		assert!(
			Arc::ptr_eq(&audio, &renditions.get(Kind::Audio, "audio0").unwrap()),
			"the estimate updates the existing rendition"
		);
		let init = audio.init().await.unwrap().expect("AAC init segment");

		assert_eq!(audio.bandwidth(), 110_000);
		assert_eq!(
			aac_avg_bitrate(&init),
			110_000,
			"the latest estimate reaches an init that was not cached yet"
		);
	}

	// The other half: a change that alters how the rendition decodes still rebuilds it, so the
	// cached init segment can never describe media it no longer matches.
	#[tokio::test]
	async fn decoder_config_change_rebuilds_the_rendition() {
		let origin = produce_origin();
		let _broadcast = origin.create_broadcast("live").expect("publish allowed");
		_broadcast.announce(Default::default()).expect("publish allowed");
		settle().await;
		let upstream = empty_upstream(&origin, "live").await;

		let mut catalog = catalog_with_both();
		let renditions = renditions::Producer::new(Config::default().window);
		let mut cursor = renditions.subscribe();
		renditions.sync(&upstream, &catalog);

		let video = renditions.get(Kind::Video, "video0").expect("video rendition");
		let audio = renditions.get(Kind::Audio, "audio0").expect("audio rendition");
		assert!(matches!(
			next_event(&mut cursor).await,
			Some(renditions::Event::Added(_))
		));
		assert!(matches!(
			next_event(&mut cursor).await,
			Some(renditions::Event::Added(_))
		));

		// A resolution change and a sample-rate change both invalidate the init segment.
		catalog.video.renditions.get_mut("video0").unwrap().coded_width = Some(640);
		catalog.audio.renditions.get_mut("audio0").unwrap().sample_rate = 48_000;
		renditions.sync(&upstream, &catalog);

		let rebuilt = renditions.get(Kind::Video, "video0").expect("video rendition");
		assert!(!Arc::ptr_eq(&video, &rebuilt), "the video rendition is rebuilt");
		assert_eq!(rebuilt.width, Some(640));
		assert!(
			!Arc::ptr_eq(&audio, &renditions.get(Kind::Audio, "audio0").unwrap()),
			"the audio rendition is rebuilt"
		);

		// Both cursors see the replacement as a removal followed by an addition.
		let mut removed = 0;
		let mut added = 0;
		while let Some(event) = next_event(&mut cursor).await {
			match event {
				renditions::Event::Removed { .. } => removed += 1,
				renditions::Event::Added(_) => added += 1,
			}
		}
		assert_eq!((removed, added), (2, 2));
	}

	// The whole fetch-on-demand path in process: a broadcast publishes media through the
	// catalog (which records the timeline), the Broadcaster renders playlists from the
	// timeline alone, and a segment request fetches and transmuxes exactly its groups.
	#[tokio::test]
	async fn master_lists_only_playable_renditions() {
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").expect("publish allowed");
		broadcast.announce(Default::default()).expect("publish allowed");
		settle().await;
		let catalog =
			moq_mux::catalog::Producer::new(&mut broadcast.broadcast().clone(), moq_mux::catalog::Config::default())
				.unwrap();

		let reserved = catalog.reserve();
		let mut registration = catalog.test_video("video0").unwrap();
		registration.set(video_config()).unwrap();
		drop(reserved);

		let track = broadcast.create_track("video0", timed()).unwrap();
		let mut media = catalog
			.media_producer(
				track,
				moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Data),
			)
			.unwrap();

		let broadcaster = Broadcaster::new(crate::resolved(&origin, "live").await, Config::default());
		let rendition = tokio::time::timeout(Duration::from_secs(5), async {
			loop {
				if let Some(rendition) = broadcaster.rendition(Kind::Video, "video0") {
					break rendition;
				}
				tokio::task::yield_now().await;
			}
		})
		.await
		.expect("rendition discovered from the catalog");

		assert!(
			rendition.media_playlist(None).unwrap().is_none(),
			"rendition is not playable yet"
		);
		assert!(
			!broadcaster.master_playlist(None).contains("video/video0/media.m3u8"),
			"master advertised a media playlist that would return 404"
		);
		assert!(
			tokio::time::timeout(Duration::from_millis(10), broadcaster.ready())
				.await
				.is_err(),
			"broadcaster became ready before it had a playable rendition"
		);

		media.write(vp8_frame(0, true)).unwrap();
		media.write(vp8_frame(2_000_000, true)).unwrap();
		media.write(vp8_frame(4_000_000, true)).unwrap();
		tokio::time::timeout(Duration::from_secs(5), broadcaster.ready())
			.await
			.expect("broadcaster becomes ready with a complete segment");
		assert!(broadcaster.master_playlist(None).contains("video/video0/media.m3u8"));

		drop((media, registration, broadcast));
	}

	/// A disabled rendition leaves the master playlist but keeps its window and media playlist,
	/// so a re-enable lists it again without resetting `EXT-X-MEDIA-SEQUENCE`. The master's
	/// long-poll wakes for a re-enable even though the rendition set is unchanged.
	#[tokio::test(start_paused = true)]
	async fn a_disabled_rendition_leaves_the_master_and_keeps_its_playlist() {
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").expect("publish allowed");
		broadcast.announce(Default::default()).expect("publish allowed");
		settle().await;
		let catalog =
			moq_mux::catalog::Producer::new(&mut broadcast.broadcast().clone(), moq_mux::catalog::Config::default())
				.unwrap();

		let reserved = catalog.reserve();
		let mut registration = catalog.test_video("video0").unwrap();
		registration.set(video_config()).unwrap();
		drop(reserved);

		let track = broadcast.create_track("video0", timed()).unwrap();
		let mut media = catalog
			.media_producer(
				track,
				moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Data),
			)
			.unwrap();
		for i in 0..3 {
			media.write(vp8_frame(i * 2_000_000, true)).unwrap();
		}

		let broadcaster = Broadcaster::new(crate::resolved(&origin, "live").await, Config::default());
		tokio::time::timeout(Duration::from_secs(5), broadcaster.advertised())
			.await
			.expect("the rendition is advertised");
		let rendition = broadcaster.rendition(Kind::Video, "video0").unwrap();
		let sequence = |rendition: &Rendition| {
			let playlist = rendition
				.media_playlist(None)
				.unwrap()
				.expect("the media playlist serves");
			between(&playlist, "#EXT-X-MEDIA-SEQUENCE:", "\n").to_string()
		};
		let before = sequence(&rendition);
		let listed = || broadcaster.master_playlist(None).contains("video/video0/media.m3u8");
		assert!(listed());

		let mut disabled = video_config();
		disabled.enabled = false;
		registration.set(disabled).unwrap();
		tokio::time::timeout(Duration::from_secs(5), async {
			while listed() {
				tokio::task::yield_now().await;
			}
		})
		.await
		.expect("the disabled rendition leaves the master");
		let kept = broadcaster.rendition(Kind::Video, "video0").unwrap();
		assert!(Arc::ptr_eq(&rendition, &kept), "the rendition is kept, not rebuilt");
		assert_eq!(sequence(&kept), before, "its media playlist keeps serving");

		// A task of its own, so nothing but the long-poll's own registration can wake it.
		let waiting = tokio::spawn({
			let broadcaster = broadcaster.clone();
			async move { broadcaster.advertised().await }
		});
		tokio::time::sleep(Duration::from_millis(10)).await;
		assert!(!waiting.is_finished(), "nothing enabled is advertised");
		registration.set(video_config()).unwrap();
		tokio::time::timeout(Duration::from_secs(5), waiting)
			.await
			.expect("a re-enable wakes the master's long-poll")
			.unwrap();
		assert!(listed());
		assert_eq!(sequence(&rendition), before, "the media sequence did not reset");

		drop((media, registration, broadcast));
	}

	#[tokio::test(start_paused = true)]
	async fn disabled_reference_handoff_continues_the_source_media_sequence() {
		handoff(false).await;
	}

	#[tokio::test(start_paused = true)]
	async fn history_reference_handoff_preserves_the_retained_window() {
		handoff(true).await;
	}

	async fn handoff(history: bool) {
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").unwrap();
		broadcast.announce(Default::default()).unwrap();
		settle().await;
		let mut catalog =
			moq_mux::catalog::Producer::new(&mut broadcast.broadcast().clone(), Default::default()).unwrap();
		let reserved = catalog.reserve();
		let mut first = catalog.test_video("video0");
		first.set(video_config()).unwrap();
		let mut sibling = catalog.test_video("video1");
		sibling.set(video_config()).unwrap();
		drop(reserved);
		let media = |name: &str| {
			let track = broadcast.create_track(name, timed()).unwrap();
			catalog
				.media_producer(
					track,
					moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Data),
				)
				.unwrap()
		};
		let mut first_media = media("video0");
		let mut sibling_media = media("video1");
		for at in [0, 2, 4, 6] {
			first_media.write(vp8_frame(at * 1_000_000, true)).unwrap();
		}
		// Aligned groups number the same records, so the sibling continues at the 6s boundary.
		for at in [0, 2, 4, 6, 8] {
			sibling_media.write(vp8_frame(at * 1_000_000, true)).unwrap();
		}
		if history {
			catalog
				.mutate(|catalog| {
					catalog.archive.as_mut().unwrap().store = Some("https://example.com/recording/".parse().unwrap())
				})
				.unwrap();
		}
		let config = Config {
			window: Duration::from_secs(4),
			history,
		};
		let broadcaster = Broadcaster::new(crate::resolved(&origin, "live").await, config);
		broadcaster.advertised().await;
		let first_rendition = broadcaster.rendition(Kind::Video, "video0").unwrap();
		let sibling_rendition = broadcaster.rendition(Kind::Video, "video1").unwrap();
		tokio::time::timeout(Duration::from_secs(5), async {
			while first_rendition.snapshot().segments.last().is_none_or(|s| s.segment < 2) {
				tokio::time::sleep(Duration::from_millis(10)).await;
			}
		})
		.await
		.unwrap();
		let old = segments::tag(&(Kind::Video, "video0".into(), "video0".into()));
		let new = segments::tag(&(Kind::Video, "video1".into(), "video1".into()));
		let before = first_rendition.snapshot();
		let mut cursor = sibling_rendition.segments();
		// History lists the whole timeline; the live window starts at 2s.
		for segment in u64::from(!history)..=2 {
			assert_eq!(cursor.next().await.unwrap().unwrap().segment, segment);
		}
		let bytes = first_rendition.segment(&old, 2).await.unwrap().unwrap();
		let mut disabled = video_config();
		disabled.enabled = false;
		first.set(disabled).unwrap();
		tokio::time::timeout(Duration::from_secs(5), async {
			while sibling_rendition
				.snapshot()
				.segments
				.last()
				.is_none_or(|s| s.tag != new)
			{
				tokio::time::sleep(Duration::from_millis(10)).await;
			}
		})
		.await
		.expect("the enabled sibling takes over the reference");
		let after = sibling_rendition.snapshot();
		assert_eq!(
			cursor.next().await.unwrap().unwrap().segment,
			3,
			"the recorder advances once into the new reference"
		);
		assert_eq!(after.media_sequence, before.media_sequence + u64::from(!history));
		assert_eq!(after.segments.last().unwrap().segment, 3, "the next source record");
		assert!(after.segments.iter().any(|s| s.tag == old));
		assert!(after.segments.iter().all(|s| !s.discontinuity));
		assert_eq!(first_rendition.segment(&old, 2).await.unwrap().unwrap(), bytes);
		let mpd = broadcaster.manifest(None).unwrap().unwrap();
		assert_eq!(mpd.matches("<Period ").count(), 2, "{mpd}");
		assert!(mpd.contains("start=\"PT6.000S\""), "{mpd}");
		assert!(
			mpd.contains(&format!("{old}.t$Time$")) && mpd.contains(&format!("{new}.t$Time$")),
			"{mpd}"
		);
		for at in [10, 12, 14] {
			sibling_media.write(vp8_frame(at * 1_000_000, true)).unwrap();
		}
		tokio::time::timeout(Duration::from_secs(5), async {
			while sibling_rendition
				.snapshot()
				.segments
				.last()
				.is_none_or(|s| s.segment < 6)
			{
				tokio::time::sleep(Duration::from_millis(10)).await;
			}
		})
		.await
		.unwrap();
		let advanced = sibling_rendition.snapshot();
		assert_eq!(advanced.media_sequence, advanced.segments[0].segment);
		assert!(!advanced.finished);
	}

	#[tokio::test(start_paused = true)]
	async fn unaligned_reference_handoff_marks_the_gap_without_reusing_urls() {
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").unwrap();
		broadcast.announce(Default::default()).unwrap();
		settle().await;
		let mut catalog =
			moq_mux::catalog::Producer::new(&mut broadcast.broadcast().clone(), Default::default()).unwrap();
		catalog
			.mutate(|catalog| {
				for name in ["video0", "video1"] {
					catalog.video.renditions.insert(name.into(), video_config());
				}
				catalog.archive = Some(archive(&["video0", "video1"]));
			})
			.unwrap();
		let timeline = |name: &str| {
			moq_mux::timeline::Producer::new(
				broadcast
					.create_track(hang::timeline::default_name(name), moq_mux::timeline::Producer::info())
					.unwrap(),
			)
		};
		let mut old = timeline("video0");
		let mut new = timeline("video1");
		old.push(&span(0, 0, 2_000, 0)).unwrap();
		old.push(&span(1, 2_000, 1_000, 1)).unwrap();
		for sequence in 0..3 {
			new.push(&gop(sequence)).unwrap();
		}
		let broadcaster = Broadcaster::new(
			crate::resolved(&origin, "live").await,
			Config {
				window: Duration::from_secs(4),
				..Default::default()
			},
		);
		broadcaster.advertised().await;
		let first = broadcaster.rendition(Kind::Video, "video0").unwrap();
		let sibling = broadcaster.rendition(Kind::Video, "video1").unwrap();
		tokio::time::timeout(Duration::from_secs(5), async {
			while sibling
				.snapshot()
				.segments
				.last()
				.is_none_or(|segment| segment.segment < 1)
			{
				tokio::time::sleep(Duration::from_millis(10)).await;
			}
		})
		.await
		.unwrap();
		let before = sibling.snapshot();
		let content = first.listed_content(1).unwrap();
		let old_tag = segments::tag(&(Kind::Video, "video0".into(), "video0".into()));
		let new_tag = segments::tag(&(Kind::Video, "video1".into(), "video1".into()));
		catalog
			.mutate(|catalog| catalog.video.renditions.get_mut("video0").unwrap().enabled = false)
			.unwrap();
		tokio::time::timeout(Duration::from_secs(5), async {
			while sibling
				.snapshot()
				.segments
				.last()
				.is_none_or(|segment| segment.tag != new_tag)
			{
				tokio::time::sleep(Duration::from_millis(10)).await;
			}
		})
		.await
		.expect("the next replacement boundary advances beyond the unaligned tail");
		let after = sibling.snapshot();
		assert_eq!(after.media_sequence, before.media_sequence + 1);
		assert_eq!(after.segments.len(), 2);
		assert_eq!(after.segments[0].tag, old_tag);
		assert_eq!(
			after.segments[0].duration,
			Duration::from_secs(1),
			"the old 2s..3s URL keeps its advertised span"
		);
		assert_eq!(first.listed_content(1).unwrap(), content);
		assert_eq!(after.segments[1].segment, 2);
		assert!(after.segments[1].discontinuity);
		assert!(!after.finished);
		broadcaster.build_inits().await;
		let playlist = sibling.media_playlist(None).unwrap().unwrap();
		assert_eq!(playlist.matches("#EXT-X-DISCONTINUITY\n").count(), 1, "{playlist}");
		let mpd = broadcaster.manifest(None).unwrap().unwrap();
		assert_eq!(
			mpd.matches("<Period ").count(),
			1,
			"the old period has only a gap on the enabled sibling: {mpd}"
		);
		assert!(
			mpd.contains("start=\"PT4.000S\"") && mpd.contains("presentationTimeOffset=\"4000\""),
			"{mpd}"
		);
		new.push(&gop(3)).unwrap();
		tokio::time::timeout(Duration::from_secs(5), async {
			while sibling
				.snapshot()
				.segments
				.first()
				.is_none_or(|segment| segment.tag != new_tag)
			{
				tokio::time::sleep(Duration::from_millis(10)).await;
			}
		})
		.await
		.unwrap();
		let advanced = sibling.media_playlist(None).unwrap().unwrap();
		assert!(advanced.contains("#EXT-X-DISCONTINUITY-SEQUENCE:1\n"), "{advanced}");
		assert!(
			!advanced.contains("#EXT-X-DISCONTINUITY\n"),
			"the boundary tag retired with the prefix"
		);
	}

	// An edge that saw the handoff and one that started after it derive the same media sequence
	// from the source, so their playlists agree once the old reference's rows leave the window.
	#[tokio::test(start_paused = true)]
	async fn edges_before_and_after_an_aligned_handoff_agree_on_the_playlist() {
		edges_around_a_handoff(2_000).await;
	}

	// The replacement numbers its 1s records ahead of the old 2s ones: the edge that saw the
	// handoff drops its advertised rows rather than renumbering the replacement's.
	#[tokio::test(start_paused = true)]
	async fn edges_before_and_after_a_handoff_ahead_agree_on_the_media_sequence() {
		edges_around_a_handoff(1_000).await;
	}

	/// The old reference cuts 2s records through 6s; the replacement cuts `span_ms` records.
	async fn edges_around_a_handoff(span_ms: u64) {
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").unwrap();
		broadcast.announce(Default::default()).unwrap();
		settle().await;
		let mut catalog =
			moq_mux::catalog::Producer::new(&mut broadcast.broadcast().clone(), Default::default()).unwrap();
		catalog
			.mutate(|catalog| {
				for name in ["video0", "video1"] {
					catalog.video.renditions.insert(name.into(), video_config());
				}
				catalog.archive = Some(archive(&["video0", "video1"]));
			})
			.unwrap();
		let timeline = |name: &str| {
			moq_mux::timeline::Producer::new(
				broadcast
					.create_track(hang::timeline::default_name(name), moq_mux::timeline::Producer::info())
					.unwrap(),
			)
		};
		let mut old = timeline("video0");
		let mut new = timeline("video1");
		let record = |sequence: u64| span(sequence, sequence * span_ms, span_ms, sequence);
		for sequence in 0..3 {
			old.push(&gop(sequence)).unwrap();
		}
		// The sibling's timeline runs past the old reference's last record, so that record resolves.
		let mut next = 0;
		while next * span_ms < 8_000 {
			new.push(&record(next)).unwrap();
			next += 1;
		}
		let resumed = 6_000 / span_ms;
		let config = Config {
			window: Duration::from_secs(4),
			..Default::default()
		};
		let old_tag = segments::tag(&(Kind::Video, "video0".into(), "video0".into()));
		let new_tag = segments::tag(&(Kind::Video, "video1".into(), "video1".into()));
		let listing = |snapshot: &playlist::Snapshot| -> Vec<(String, u64)> {
			snapshot
				.segments
				.iter()
				.map(|segment| (segment.tag.clone(), segment.segment))
				.collect()
		};

		let saw = Broadcaster::new(crate::resolved(&origin, "live").await, config.clone());
		saw.advertised().await;
		let saw_video1 = saw.rendition(Kind::Video, "video1").unwrap();
		listed_under(&saw_video1, &old_tag, 2).await;
		catalog
			.mutate(|catalog| catalog.video.renditions.get_mut("video0").unwrap().enabled = false)
			.unwrap();
		let handed = listed_under(&saw_video1, &new_tag, next - 1).await;
		assert_eq!(handed.segments[0].segment, handed.media_sequence);
		let resumed_at = handed.segments.iter().position(|s| s.tag == new_tag).unwrap();
		assert_eq!(handed.segments[resumed_at].segment, resumed);
		if resumed == 3 {
			assert_eq!(
				listing(&handed)[..resumed_at],
				[(old_tag.clone(), 2)],
				"the advertised row stays"
			);
			assert!(!handed.segments[resumed_at].discontinuity);
		} else {
			assert_eq!(resumed_at, 0, "positions 3..6 are skipped, so the old rows go");
		}

		let after = Broadcaster::new(crate::resolved(&origin, "live").await, config);
		after.advertised().await;
		let after_video1 = after.rendition(Kind::Video, "video1").unwrap();
		let joined = listed_under(&after_video1, &new_tag, next - 1).await;
		assert_eq!(joined.segments[0].segment, joined.media_sequence);
		// Every segment number both edges list names the same playlist position.
		for segment in &joined.segments {
			if let Some(offset) = handed.segments.iter().position(|s| s.segment == segment.segment) {
				assert_eq!(handed.media_sequence + offset as u64, segment.segment);
			}
		}

		while next * span_ms < 12_000 {
			new.push(&record(next)).unwrap();
			next += 1;
		}
		let followed = listed_under(&saw_video1, &new_tag, next - 1).await;
		let joined = listed_under(&after_video1, &new_tag, next - 1).await;
		assert_eq!(listing(&followed), listing(&joined));
		assert_eq!(
			followed.media_sequence, joined.media_sequence,
			"one numbering on every edge"
		);
		if resumed == 3 {
			saw.build_inits().await;
			after.build_inits().await;
			let saw_playlist = saw_video1.media_playlist(None).unwrap().unwrap();
			assert!(saw_playlist.contains("#EXT-X-MEDIA-SEQUENCE:4\n"), "{saw_playlist}");
			assert_eq!(saw_playlist, after_video1.media_playlist(None).unwrap().unwrap());
		}
	}

	#[tokio::test(start_paused = true)]
	async fn history_handoff_keeps_following_the_original_sources_retention() {
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").unwrap();
		broadcast.announce(Default::default()).unwrap();
		settle().await;
		let mut catalog =
			moq_mux::catalog::Producer::new(&mut broadcast.broadcast().clone(), Default::default()).unwrap();
		catalog
			.mutate(|catalog| {
				for name in ["video0", "video1"] {
					catalog.video.renditions.insert(name.into(), video_config());
				}
				let mut section = archive(&["video0", "video1"]);
				section.store = Some("https://example.com/recording/".parse().unwrap());
				catalog.archive = Some(section);
			})
			.unwrap();
		let timeline = |name: &str| {
			moq_mux::timeline::Producer::new(
				broadcast
					.create_track(hang::timeline::default_name(name), moq_mux::timeline::Producer::info())
					.unwrap(),
			)
		};
		let mut old = timeline("video0");
		let mut new = timeline("video1");
		for sequence in 0..3 {
			old.push(&gop(sequence)).unwrap();
		}
		for sequence in 0..4 {
			new.push(&gop(sequence)).unwrap();
		}
		let broadcaster = Broadcaster::new(
			crate::resolved(&origin, "live").await,
			Config {
				history: true,
				..Default::default()
			},
		);
		let rendition = loop {
			if let Some(rendition) = broadcaster.rendition(Kind::Video, "video1") {
				break rendition;
			}
			tokio::time::sleep(Duration::from_millis(10)).await;
		};
		let next_tag = segments::tag(&(Kind::Video, "video1".into(), "video1".into()));
		tokio::time::timeout(Duration::from_secs(5), async {
			while rendition
				.snapshot()
				.segments
				.last()
				.is_none_or(|segment| segment.segment < 2)
			{
				tokio::time::sleep(Duration::from_millis(10)).await;
			}
		})
		.await
		.unwrap();
		catalog
			.mutate(|catalog| catalog.video.renditions.get_mut("video0").unwrap().enabled = false)
			.unwrap();
		tokio::time::timeout(Duration::from_secs(5), async {
			while rendition
				.snapshot()
				.segments
				.last()
				.is_none_or(|segment| segment.tag != next_tag)
			{
				tokio::time::sleep(Duration::from_millis(10)).await;
			}
		})
		.await
		.unwrap();
		old.pop(1).unwrap();
		tokio::time::timeout(Duration::from_secs(5), async {
			while rendition.snapshot().media_sequence != 1 {
				tokio::time::sleep(Duration::from_millis(10)).await;
			}
		})
		.await
		.expect("past reference still trims its retained history");
		new.pop(2).unwrap();
		settle().await;
		let snapshot = rendition.snapshot();
		assert_eq!(snapshot.media_sequence, 1);
		assert_eq!(
			snapshot.segments.len(),
			3,
			"new-source pops do not remove old-source rows"
		);
		old.finish().unwrap();
		new.push(&gop(4)).unwrap();
		tokio::time::timeout(Duration::from_secs(5), async {
			while rendition
				.snapshot()
				.segments
				.last()
				.is_none_or(|segment| segment.segment < 4)
			{
				tokio::time::sleep(Duration::from_millis(10)).await;
			}
		})
		.await
		.expect("past reference completion cannot end the new reference");
		assert!(!rendition.snapshot().finished);
		new.pop(2).unwrap();
		tokio::time::timeout(Duration::from_secs(5), async {
			while rendition.snapshot().media_sequence != 4 {
				tokio::time::sleep(Duration::from_millis(10)).await;
			}
		})
		.await
		.expect("retention retires a whole playlist prefix without changing any remaining segment position");
		assert_eq!(rendition.snapshot().segments.len(), 1);
	}

	/// A durable broadcast advertising `video0` and `video1`, so history mode follows retention.
	async fn durable_pair() -> (
		moq_net::origin::Producer,
		moq_net::origin::Publication,
		moq_mux::catalog::Producer,
	) {
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").unwrap();
		broadcast.announce(Default::default()).unwrap();
		settle().await;
		let mut catalog =
			moq_mux::catalog::Producer::new(&mut broadcast.broadcast().clone(), Default::default()).unwrap();
		catalog
			.mutate(|catalog| {
				for name in ["video0", "video1"] {
					catalog.video.renditions.insert(name.into(), video_config());
				}
				let mut section = archive(&["video0", "video1"]);
				section.store = Some("https://example.com/recording/".parse().unwrap());
				catalog.archive = Some(section);
			})
			.unwrap();
		(origin, broadcast, catalog)
	}

	fn timeline_track(
		broadcast: &moq_net::broadcast::Producer,
		name: &str,
	) -> (moq_net::track::Producer, moq_mux::timeline::Producer) {
		let track = broadcast
			.create_track(hang::timeline::default_name(name), moq_mux::timeline::Producer::info())
			.unwrap();
		(track.clone(), moq_mux::timeline::Producer::new(track))
	}

	async fn history_export(origin: &moq_net::origin::Producer) -> Arc<Broadcaster> {
		let config = Config {
			history: true,
			..Default::default()
		};
		let broadcaster = Broadcaster::new(crate::resolved(origin, "live").await, config);
		broadcaster.advertised().await;
		broadcaster
	}

	// The retained reader of a reference consumed its records while another was selected, so
	// reselecting the reference replays its timeline instead of skipping past them.
	#[tokio::test(start_paused = true)]
	async fn a_reselected_history_reference_replays_its_timeline() {
		let (origin, broadcast, mut catalog) = durable_pair().await;
		let (_old_track, mut old) = timeline_track(&broadcast, "video0");
		let (_new_track, mut new) = timeline_track(&broadcast, "video1");
		for sequence in 0..3 {
			old.push(&gop(sequence)).unwrap();
			new.push(&gop(sequence)).unwrap();
		}
		let broadcaster = history_export(&origin).await;
		let video0 = broadcaster.rendition(Kind::Video, "video0").unwrap();
		let video1 = broadcaster.rendition(Kind::Video, "video1").unwrap();
		let old_tag = segments::tag(&(Kind::Video, "video0".into(), "video0".into()));
		let new_tag = segments::tag(&(Kind::Video, "video1".into(), "video1".into()));
		listed_under(&video0, &old_tag, 2).await;
		catalog
			.mutate(|catalog| catalog.video.renditions.get_mut("video0").unwrap().enabled = false)
			.unwrap();
		new.push(&gop(3)).unwrap();
		listed_under(&video1, &new_tag, 3).await;
		old.push(&gop(3)).unwrap();
		old.push(&gop(4)).unwrap();
		settle().await;
		catalog
			.mutate(|catalog| {
				catalog.video.renditions.get_mut("video0").unwrap().enabled = true;
				catalog.video.renditions.get_mut("video1").unwrap().enabled = false;
			})
			.unwrap();
		let snapshot = listed_under(&video0, &old_tag, 4).await;
		assert_eq!(numbers(&snapshot), [0, 1, 2, 3, 4], "history survives both handoffs");
		assert!(snapshot.segments.iter().all(|segment| !segment.discontinuity));
	}

	// A failed past reference still lists rows whose pops it can no longer report, so every
	// listing answers the error rather than losing that prefix and still ending as complete.
	#[tokio::test(start_paused = true)]
	async fn a_failed_past_history_reference_fails_every_listing() {
		let (origin, broadcast, mut catalog) = durable_pair().await;
		let (old_track, mut old) = timeline_track(&broadcast, "video0");
		let (_new_track, mut new) = timeline_track(&broadcast, "video1");
		for sequence in 0..4 {
			if sequence < 3 {
				old.push(&gop(sequence)).unwrap();
			}
			new.push(&gop(sequence)).unwrap();
		}
		let broadcaster = history_export(&origin).await;
		let video1 = broadcaster.rendition(Kind::Video, "video1").unwrap();
		let old_tag = segments::tag(&(Kind::Video, "video0".into(), "video0".into()));
		let new_tag = segments::tag(&(Kind::Video, "video1".into(), "video1".into()));
		listed_under(&video1, &old_tag, 2).await;
		catalog
			.mutate(|catalog| catalog.video.renditions.get_mut("video0").unwrap().enabled = false)
			.unwrap();
		assert_eq!(numbers(&listed_under(&video1, &new_tag, 3).await), [0, 1, 2, 3]);
		cut(old_track, old);
		failed(&video1).await;
		new.push(&gop(4)).unwrap();
		settle().await;
		assert!(
			video1.try_snapshot().is_err(),
			"the selected reference cannot recover it"
		);
		let video0 = broadcaster.rendition(Kind::Video, "video0").unwrap();
		failed(&video0).await;
		assert!(matches!(
			broadcaster.manifest(None),
			Err(crate::Error::TimelineFailed { .. })
		));
		// A clip of the history answers the failure too, rather than refusing the range.
		let range = Range::new(Duration::ZERO, Duration::from_secs(4)).unwrap();
		assert!(matches!(
			video1.playlist(Some(range), None).await,
			Err(crate::Error::TimelineFailed { .. })
		));
	}

	// A rendition whose own timeline failed answers the error alone. Its siblings keep serving,
	// and the broadcast-wide listings leave it out so new players start on a sibling.
	#[tokio::test(start_paused = true)]
	async fn a_failed_rendition_leaves_the_manifest_and_master() {
		let (origin, broadcast, _catalog) = durable_pair().await;
		let (_video0_track, mut video0) = timeline_track(&broadcast, "video0");
		let (video1_track, mut video1) = timeline_track(&broadcast, "video1");
		for sequence in 0..4 {
			video0.push(&gop(sequence)).unwrap();
			video1.push(&gop(sequence)).unwrap();
		}
		let broadcaster = Broadcaster::new(crate::resolved(&origin, "live").await, Config::default());
		broadcaster.advertised().await;
		let healthy = broadcaster.rendition(Kind::Video, "video0").unwrap();
		let tag = segments::tag(&(Kind::Video, "video0".into(), "video0".into()));
		listed_under(&healthy, &tag, 3).await;
		let before = broadcaster.manifest(None).unwrap().expect("both renditions are listed");
		assert!(before.contains("id=\"video/video1\""), "{before}");
		assert!(broadcaster.master_playlist(None).contains("video/video1/"));

		cut(video1_track, video1);
		let broken = broadcaster.rendition(Kind::Video, "video1").unwrap();
		failed(&broken).await;
		assert!(broken.media_playlist(None).is_err());
		assert!(healthy.media_playlist(None).unwrap().is_some());
		let manifest = broadcaster.manifest(None).unwrap().expect("the sibling stays listed");
		assert!(manifest.contains("id=\"video/video0\""), "{manifest}");
		assert!(!manifest.contains("id=\"video/video1\""), "{manifest}");
		let master = broadcaster.master_playlist(None);
		assert!(master.contains("video/video0/"), "{master}");
		assert!(!master.contains("video/video1/"), "{master}");
	}

	// With every rendition disabled no reference watcher is left, so the source ending must
	// still end every playlist and recording cursor.
	#[tokio::test(start_paused = true)]
	async fn a_source_ending_with_every_rendition_disabled_ends_the_playlists() {
		let (origin, broadcast, mut catalog) = durable_pair().await;
		let (_video0_track, mut video0) = timeline_track(&broadcast, "video0");
		let (_video1_track, mut video1) = timeline_track(&broadcast, "video1");
		for sequence in 0..4 {
			video0.push(&gop(sequence)).unwrap();
			video1.push(&gop(sequence)).unwrap();
		}
		let media = broadcast.create_track("video0", timed()).unwrap();
		for sequence in 0..4 {
			let mut group = media.append_group().unwrap();
			let frame = vp8_frame(sequence * 2_000_000, true);
			hang::container::Frame {
				timestamp: frame.timestamp,
				payload: frame.payload,
			}
			.write_to(&mut group)
			.unwrap();
			group.finish().unwrap();
		}
		media.finish().unwrap();

		let broadcaster = Broadcaster::new(crate::resolved(&origin, "live").await, Config::default());
		broadcaster.advertised().await;
		let rendition = broadcaster.rendition(Kind::Video, "video0").unwrap();
		let tag = segments::tag(&(Kind::Video, "video0".into(), "video0".into()));
		listed_under(&rendition, &tag, 3).await;
		catalog
			.mutate(|catalog| {
				for config in catalog.video.renditions.values_mut() {
					config.enabled = false;
				}
			})
			.unwrap();
		settle().await;
		catalog.finish().unwrap();
		tokio::time::timeout(Duration::from_secs(5), async {
			while !rendition.snapshot().finished {
				tokio::time::sleep(Duration::from_millis(10)).await;
			}
		})
		.await
		.expect("the playlist ends with the source");
		let mut cursor = rendition.segments();
		tokio::time::timeout(Duration::from_secs(5), async {
			while cursor.next().await.unwrap().is_some() {}
		})
		.await
		.expect("the recording cursor ends");
	}

	/// Wait until `rendition` lists segment `segment` (or later) under the reference `tag`.
	async fn listed_under(rendition: &Rendition, tag: &str, segment: u64) -> playlist::Snapshot {
		tokio::time::timeout(Duration::from_secs(5), async {
			loop {
				let snapshot = rendition.snapshot();
				if snapshot
					.segments
					.last()
					.is_some_and(|last| last.tag == tag && last.segment >= segment)
				{
					return snapshot;
				}
				tokio::time::sleep(Duration::from_millis(10)).await;
			}
		})
		.await
		.unwrap_or_else(|_| panic!("segment {segment} never listed"))
	}

	/// The text between `start` and the next `end` in `haystack`.
	fn between<'a>(haystack: &'a str, start: &str, end: &str) -> &'a str {
		let rest = &haystack[haystack.find(start).unwrap_or_else(|| panic!("no {start:?}")) + start.len()..];
		&rest[..rest.find(end).unwrap_or_else(|| panic!("no {end:?}"))]
	}

	/// The init hash a media playlist maps.
	fn init_hash(playlist: &str) -> &str {
		between(playlist, "#EXT-X-MAP:URI=\"init.", ".mp4")
	}

	/// Publish a VP8 rendition with three GOPs, 2s apart, so segments 0 and 1 are listable.
	fn publish_vp8(
		broadcast: &mut moq_net::broadcast::Producer,
	) -> (
		moq_mux::catalog::Producer,
		TestRendition,
		moq_mux::container::Producer<moq_mux::catalog::hang::Container>,
	) {
		let catalog = moq_mux::catalog::Producer::new(broadcast, moq_mux::catalog::Config::default()).unwrap();
		let reserved = catalog.reserve();
		let mut registration = catalog.test_video("video0").unwrap();
		registration.set(video_config()).unwrap();
		drop(reserved);
		let track = broadcast.create_track("video0", timed()).unwrap();
		let mut media = catalog
			.media_producer(
				track,
				moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Data),
			)
			.unwrap();
		for (micros, keyframe) in [(0, true), (1_000_000, false), (2_000_000, true), (4_000_000, true)] {
			media.write(vp8_frame(micros, keyframe)).unwrap();
		}
		(catalog, registration, media)
	}

	/// A completed segment already decoded its inline codec metadata. Finishing the
	/// catalog can index a trailing group whose media producer is still live.
	#[tokio::test(start_paused = true)]
	async fn a_transmuxed_segment_keeps_its_init_with_an_unfinished_tail() {
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").expect("publish allowed");
		let (mut catalog, mut registration, _media) = publish_vp8(&mut broadcast.broadcast().clone());
		registration
			.set(hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::VP8))
			.unwrap();
		broadcast.announce(moq_net::origin::Route::default()).unwrap();
		settle().await;
		let broadcaster = Broadcaster::new(crate::resolved(&origin, "live").await, Config::default());
		broadcaster.ready().await;
		let rendition = broadcaster.rendition(Kind::Video, "video0").expect("rendition");
		let mut cursor = rendition.segments();
		let segment = cursor.next().await.unwrap().expect("completed segment");
		assert!(!segment.media.is_empty());

		catalog.finish().unwrap();
		while !rendition.snapshot().finished {
			tokio::task::yield_now().await;
		}
		let init = tokio::time::timeout(Duration::from_secs(1), rendition.init())
			.await
			.expect("init must not wait for the unfinished trailing group")
			.unwrap()
			.expect("transmuxed segment has an init");
		assert_eq!(&init[4..8], b"ftyp");
	}

	/// An init bootstrap already blocked on the unfinished tail must not hold back a later
	/// request once a completed segment has cached the init.
	#[tokio::test(start_paused = true)]
	async fn a_blocked_init_bootstrap_does_not_hide_a_cached_init() {
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").expect("publish allowed");
		let (mut catalog, mut registration, _media) = publish_vp8(&mut broadcast.broadcast().clone());
		registration
			.set(hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::VP8))
			.unwrap();
		broadcast.announce(moq_net::origin::Route::default()).unwrap();
		settle().await;
		let broadcaster = Broadcaster::new(crate::resolved(&origin, "live").await, Config::default());
		broadcaster.ready().await;
		let rendition = broadcaster.rendition(Kind::Video, "video0").expect("rendition");

		catalog.finish().unwrap();
		while !rendition.snapshot().finished {
			tokio::task::yield_now().await;
		}
		let mut blocked = std::pin::pin!(rendition.init());
		assert!(
			tokio::time::timeout(Duration::from_millis(10), blocked.as_mut())
				.await
				.is_err(),
			"the bootstrap reads the unfinished trailing group"
		);

		let segment = rendition.segments().next().await.unwrap().expect("completed segment");
		assert!(!segment.media.is_empty());
		tokio::time::timeout(Duration::from_secs(1), rendition.init())
			.await
			.expect("init must not wait behind the blocked bootstrap")
			.unwrap()
			.expect("transmuxed segment has an init");
	}

	// A reconfigure that changes the init bytes changes the init URL, and the old URL stops
	// resolving, so a cache can never hand a player the previous init under the new one.
	#[tokio::test]
	async fn a_reconfigure_versions_the_init_url() {
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").expect("publish allowed");
		broadcast.announce(Default::default()).expect("publish allowed");
		settle().await;
		let (_catalog, mut registration, _media) = publish_vp8(&mut broadcast.broadcast().clone());

		let broadcaster = Broadcaster::new(crate::resolved(&origin, "live").await, Config::default());
		let _ = tokio::time::timeout(Duration::from_secs(5), broadcaster.ready()).await;
		let old = broadcaster.rendition(Kind::Video, "video0").expect("rendition");
		let old_playlist = old
			.media_playlist(None)
			.unwrap()
			.expect("an out-of-band init renders without a fetch");
		let old_hash = init_hash(&old_playlist).to_string();

		let mut config = video_config();
		config.coded_width = Some(640);
		config.coded_height = Some(480);
		registration.set(config).unwrap();

		let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
		let new = loop {
			let current = broadcaster.rendition(Kind::Video, "video0").expect("rendition");
			if !Arc::ptr_eq(&current, &old) {
				break current;
			}
			assert!(
				tokio::time::Instant::now() < deadline,
				"the reconfigure never rebuilt the rendition"
			);
			tokio::time::sleep(Duration::from_millis(10)).await;
		};
		let new_init = new.init().await.unwrap().expect("init segment");
		let _ = tokio::time::timeout(Duration::from_secs(5), new.playable()).await;
		let new_playlist = new.media_playlist(None).unwrap().expect("playable");
		let new_hash = init_hash(&new_playlist);
		assert_ne!(new_hash, old_hash, "different init bytes get a different URL");
		assert_eq!(new.init_versioned(new_hash).await.unwrap(), Some(new_init));
		assert!(new.init_versioned(&old_hash).await.unwrap().is_none());
	}

	// A generation rides every segment URL. The first one labels the run already listed;
	// replacing it drops that run's rows, and the next run lists under the new generation.
	#[tokio::test]
	async fn a_generation_versions_segment_urls_and_a_new_one_resets_the_window() {
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").expect("publish allowed");
		broadcast.announce(Default::default()).expect("publish allowed");
		settle().await;
		let (_catalog, _registration, mut media) = publish_vp8(&mut broadcast.broadcast().clone());

		let broadcaster = Broadcaster::new(crate::resolved(&origin, "live").await, Config::default());
		let _ = tokio::time::timeout(Duration::from_secs(5), broadcaster.ready()).await;
		let rendition = broadcaster.rendition(Kind::Video, "video0").expect("rendition");
		let unversioned = rendition.media_playlist(None).unwrap().expect("playable");
		assert!(unversioned.contains("\nseg/656e3d1b.0.m4s\n"));

		assert!(matches!(
			broadcaster.set_generation(Some("run.1")),
			Err(crate::Error::InvalidGeneration(_))
		));
		assert!(broadcaster.set_generation(Some("")).is_err());

		broadcaster.set_generation(Some("run-1")).unwrap();
		let labeled = rendition
			.media_playlist(None)
			.unwrap()
			.expect("the first generation keeps the rows");
		assert!(labeled.contains("\nseg/run-1.656e3d1b.0.m4s\n"));
		assert!(labeled.contains("\nseg/run-1.656e3d1b.1.m4s\n"));
		assert_eq!(
			init_hash(&labeled),
			init_hash(&unversioned),
			"the init needs no generation"
		);
		let manifest = broadcaster.manifest(None).unwrap().expect("manifest");
		let run = broadcaster.instance();
		assert!(manifest.contains(&format!("media=\"@{run}/video/video0/seg/run-1.656e3d1b.t$Time$.m4s\"")));

		broadcaster.set_generation(Some("run-2")).unwrap();
		assert!(
			rendition.media_playlist(None).unwrap().is_none(),
			"a new generation drops the old run's rows"
		);

		media.write(vp8_frame(6_000_000, true)).unwrap();
		let _ = tokio::time::timeout(Duration::from_secs(5), rendition.playable()).await;
		let restarted = rendition.media_playlist(None).unwrap().expect("the new run lists");
		assert!(restarted.contains("\nseg/run-2.656e3d1b.2.m4s\n"));
		assert!(!restarted.contains("run-1"));

		broadcaster.set_generation(None).unwrap();
		assert!(
			rendition.media_playlist(None).unwrap().is_none(),
			"clearing the generation is a change too"
		);
	}

	// A restarted inline-codec publisher can carry new parameter sets under the same catalog, so
	// a new generation drops the init built from the old run's media along with its rows.
	#[tokio::test]
	async fn a_new_generation_rebuilds_an_inline_init() {
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").expect("publish allowed");
		broadcast.announce(Default::default()).expect("publish allowed");
		settle().await;
		let catalog =
			moq_mux::catalog::Producer::new(&mut broadcast.broadcast().clone(), moq_mux::catalog::Config::default())
				.unwrap();
		let reserved = catalog.reserve();
		let mut registration = catalog.test_video("video0").unwrap();
		let mut config = video_config();
		config.coded_width = None;
		config.coded_height = None;
		registration.set(config).unwrap();
		drop(reserved);
		let track = broadcast.create_track("video0", timed()).unwrap();
		let mut media = catalog
			.media_producer(
				track,
				moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Data),
			)
			.unwrap();
		for micros in [0, 2_000_000, 4_000_000] {
			media.write(vp8_frame(micros, true)).unwrap();
		}

		let broadcaster = Broadcaster::new(crate::resolved(&origin, "live").await, Config::default());
		let _ = tokio::time::timeout(Duration::from_secs(5), broadcaster.ready()).await;
		let rendition = broadcaster.rendition(Kind::Video, "video0").expect("rendition");
		let _ = tokio::time::timeout(Duration::from_secs(5), rendition.playable()).await;
		let old = rendition.init().await.unwrap().expect("init segment");
		broadcaster.set_generation(Some("run-1")).unwrap();
		broadcaster.set_generation(Some("run-2")).unwrap();

		// The new run's keyframes are 640x480 under the same catalog.
		for micros in [6_000_000, 8_000_000, 10_000_000] {
			media
				.write(moq_mux::container::Frame {
					timestamp: moq_net::Timestamp::from_micros(micros).unwrap(),
					payload: bytes::Bytes::from_static(&[0x10, 0x00, 0x00, 0x9d, 0x01, 0x2a, 0x80, 0x02, 0xe0, 0x01]),
					keyframe: true,
					duration: None,
				})
				.unwrap();
		}
		let _ = tokio::time::timeout(Duration::from_secs(5), rendition.playable()).await;
		let new = rendition.init().await.unwrap().expect("init segment");
		assert_ne!(new, old, "the new run's init is built from its own media");
		let playlist = rendition.media_playlist(None).unwrap().expect("playable");
		assert_eq!(rendition.init_versioned(init_hash(&playlist)).await.unwrap(), Some(new));
	}

	#[tokio::test]
	async fn serves_playlist_and_segments_from_the_timeline() {
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").expect("publish allowed");
		broadcast.announce(Default::default()).expect("publish allowed");
		settle().await;
		let catalog =
			moq_mux::catalog::Producer::new(&mut broadcast.broadcast().clone(), moq_mux::catalog::Config::default())
				.unwrap();

		let reserved = catalog.reserve();
		let mut registration = catalog.test_video("video0").unwrap();
		let mut config = video_config();
		config.coded_width = None;
		config.coded_height = None;
		registration.set(config).unwrap();
		drop(reserved);

		// Three GOPs, 2s apart: groups 0 and 1 are complete, group 2 is the live edge.
		let track = broadcast.create_track("video0", timed()).unwrap();
		let mut media = catalog
			.media_producer(
				track,
				moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Data),
			)
			.unwrap();
		media.write(vp8_frame(0, true)).unwrap();
		media.write(vp8_frame(1_000_000, false)).unwrap();
		media.write(vp8_frame(2_000_000, true)).unwrap();
		media.write(vp8_frame(3_000_000, false)).unwrap();
		media.write(vp8_frame(4_000_000, true)).unwrap();

		let broadcaster = Broadcaster::new(crate::resolved(&origin, "live").await, Config::default());
		let _ = tokio::time::timeout(Duration::from_secs(5), broadcaster.ready()).await;
		let rendition = broadcaster
			.rendition(Kind::Video, "video0")
			.expect("rendition discovered from the catalog");
		let _ = tokio::time::timeout(Duration::from_secs(5), rendition.playable()).await;

		let master = broadcaster.master_playlist(None);
		assert!(master.contains("video/video0/media.m3u8"), "master lists the rendition");

		let playlist = rendition.snapshot();
		assert_eq!(playlist.segments.len(), 2, "the live-edge group is not listed");
		assert_eq!(playlist.segments[0].segment, 0);
		assert_eq!(playlist.segments[0].duration, Duration::from_secs(2));
		assert_eq!(playlist.segments[1].segment, 1);
		assert_eq!(
			playlist.target_duration, 2,
			"the observed segment duration, in whole seconds"
		);
		assert!(!playlist.finished);

		// Without coded dimensions VP8 learns its init from a keyframe, and the playlist names
		// the init by hash, so nothing renders until the init is built.
		assert!(rendition.media_playlist(None).unwrap().is_none());
		let init = rendition.init().await.unwrap().expect("init segment");
		assert_eq!(&init[4..8], b"ftyp");

		let rendered = rendition.media_playlist(None).unwrap().expect("playable");
		let hash = init_hash(&rendered);
		assert!(rendered.contains("seg/656e3d1b.0.m4s\n"));
		assert!(rendered.contains("seg/656e3d1b.1.m4s\n"));
		assert_eq!(rendition.init_versioned(hash).await.unwrap(), Some(init));
		assert!(rendition.init_versioned("0000000000000000").await.unwrap().is_none());

		// The same render, but carrying a credential into every child URL.
		let signed = rendition
			.media_playlist(Some("jwt=abc.def"))
			.unwrap()
			.expect("playable");
		assert!(signed.contains(&format!("#EXT-X-MAP:URI=\"init.{hash}.mp4?jwt=abc.def\"\n")));
		assert!(signed.contains("seg/656e3d1b.0.m4s?jwt=abc.def\n"));

		let segment = rendition
			.listed_segment(0)
			.await
			.unwrap()
			.expect("segment fetched on demand");
		assert_eq!(&segment[4..8], b"moof", "a fetched group transmuxes to moof+mdat");

		// The live-edge group isn't a segment yet, and unknown groups miss.
		assert!(rendition.listed_segment(2).await.unwrap().is_none());
		assert!(rendition.listed_segment(99).await.unwrap().is_none());

		// Keep the publisher alive for the whole test.
		drop((media, registration, broadcast));
	}

	/// A duration marker times the trailing sample of a fetched HLS segment.
	#[tokio::test]
	async fn a_duration_marker_times_the_hls_trailing_sample() {
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").expect("publish allowed");
		broadcast.announce(Default::default()).expect("publish allowed");
		settle().await;
		let catalog =
			moq_mux::catalog::Producer::new(&mut broadcast.broadcast().clone(), moq_mux::catalog::Config::default())
				.unwrap();

		let reserved = catalog.reserve();
		let mut registration = catalog.test_video("video0").unwrap();
		registration.set(video_config()).unwrap();
		drop(reserved);

		let track = broadcast.create_track("video0", timed()).unwrap();
		let mut media = catalog
			.media_producer(
				track,
				moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video),
			)
			.unwrap();
		media.write(vp8_frame(0, true)).unwrap();
		media.write(vp8_frame(1_000_000, false)).unwrap();
		media.write(vp8_frame(2_000_000, true)).unwrap();
		media.finish().unwrap();

		let broadcaster = Broadcaster::new(crate::resolved(&origin, "live").await, Config::default());
		let _ = tokio::time::timeout(Duration::from_secs(5), broadcaster.ready()).await;
		let rendition = broadcaster
			.rendition(Kind::Video, "video0")
			.expect("rendition discovered from the catalog");
		let _ = tokio::time::timeout(Duration::from_secs(5), rendition.playable()).await;

		let segment = rendition
			.listed_segment(0)
			.await
			.unwrap()
			.expect("segment fetched on demand");
		assert_eq!(&segment[4..8], b"moof");
		drop((media, registration, broadcast));
	}

	// The DASH half of the fetch-on-demand path: the manifest renders from the same timeline
	// windows as the HLS playlists (dynamic while live, aligned S entries across renditions),
	// and $Time$ addressing resolves a segment's pts to the same bytes its number does.
	#[tokio::test]
	async fn serves_dash_manifest_and_time_addressed_segments() {
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").expect("publish allowed");
		broadcast.announce(Default::default()).expect("publish allowed");
		settle().await;
		let catalog =
			moq_mux::catalog::Producer::new(&mut broadcast.broadcast().clone(), moq_mux::catalog::Config::default())
				.unwrap();

		let reserved = catalog.reserve();
		let mut video_registration = catalog.test_video("video0").unwrap();
		video_registration.set(video_config()).unwrap();
		let mut audio_registration = catalog.test_audio("audio0").unwrap();
		let audio_config = hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Opus, 48_000, 2);
		audio_registration.set(audio_config).unwrap();
		drop(reserved);

		let video_track = broadcast.create_track("video0", timed()).unwrap();
		let mut video = catalog
			.media_producer(
				video_track,
				moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Data),
			)
			.unwrap();
		let audio_track = broadcast.create_track("audio0", timed()).unwrap();
		let mut audio = catalog
			.media_producer(
				audio_track,
				moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Data),
			)
			.unwrap();

		// Video keyframes every 2s cut the segments; audio contributes one group per cut.
		let mut audio_write = |micros: u64| {
			let keyframe = audio.needs_keyframe();
			audio.write(frame(micros, keyframe)).unwrap();
			audio.cut(None).unwrap();
		};
		video.write(frame(0, true)).unwrap();
		audio_write(0);
		video.write(frame(2_000_000, true)).unwrap();
		audio_write(2_000_000);
		video.write(frame(4_000_000, true)).unwrap(); // live edge
		audio_write(4_000_000);

		let broadcaster = Broadcaster::new(crate::resolved(&origin, "live").await, Config::default());
		let _ = tokio::time::timeout(Duration::from_secs(5), broadcaster.ready()).await;
		let _ = tokio::time::timeout(Duration::from_secs(5), broadcaster.playable()).await;
		let video_rendition = broadcaster.rendition(Kind::Video, "video0").expect("video discovered");
		let audio_rendition = broadcaster.rendition(Kind::Audio, "audio0").expect("audio discovered");
		let _ = tokio::time::timeout(Duration::from_secs(5), video_rendition.playable()).await;
		let _ = tokio::time::timeout(Duration::from_secs(5), audio_rendition.playable()).await;

		let manifest = broadcaster
			.manifest(None)
			.unwrap()
			.expect("manifest renders once playable");
		assert!(manifest.contains(" type=\"dynamic\""), "live broadcast is dynamic");
		assert!(
			manifest.contains(" availabilityStartTime=\""),
			"pts 0 is anchored by the catalog root clock"
		);
		assert!(manifest.contains("id=\"video/video0\""));
		assert!(manifest.contains("id=\"audio/audio0\""));
		let run = broadcaster.instance();
		assert!(manifest.contains(&format!("media=\"@{run}/video/video0/seg/656e3d1b.t$Time$.m4s\"")));
		// Both renditions list the same aligned segment spans (the live edge is not listed).
		assert_eq!(manifest.matches("<S t=\"0\" d=\"2000\"/>").count(), 2);
		assert_eq!(manifest.matches("<S t=\"2000\" d=\"2000\"/>").count(), 2);
		assert!(!manifest.contains("<S t=\"4000\""), "the live-edge group is not listed");

		// A credential rides every child URL, and the init is named by its hash.
		let signed = broadcaster
			.manifest(Some("jwt=abc.def"))
			.unwrap()
			.expect("manifest renders");
		let hash = between(
			&signed,
			&format!("initialization=\"@{run}/video/video0/init."),
			".mp4?jwt=abc.def\"",
		);
		let init = video_rendition.init().await.unwrap().expect("init segment");
		assert_eq!(video_rendition.init_versioned(hash).await.unwrap(), Some(init));
		assert!(signed.contains(&format!(
			"media=\"@{run}/video/video0/seg/656e3d1b.t$Time$.m4s?jwt=abc.def\""
		)));

		// $Time$ resolves to the same bytes the aligned number does; unknown times miss.
		let by_time = video_rendition
			.listed_segment_at(2_000)
			.await
			.unwrap()
			.expect("segment fetched by pts");
		let by_number = video_rendition
			.listed_segment(1)
			.await
			.unwrap()
			.expect("segment by number");
		assert_eq!(by_time, by_number);
		assert!(video_rendition.listed_segment_at(999).await.unwrap().is_none());

		drop((video, audio, video_registration, audio_registration, broadcast));
	}

	// The clock migration end to end: the catalog root clock (not the archive section) times
	// EXT-X-PROGRAM-DATE-TIME and the DASH availabilityStartTime, converting the timeline
	// timescale into the clock's.
	#[tokio::test]
	async fn root_clock_times_playlists_and_manifest() {
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").expect("publish allowed");
		broadcast.announce(Default::default()).expect("publish allowed");
		settle().await;

		// PTS zero at exactly the moq epoch, so every timestamp maps to a fixed string.
		let wall = moq_clock::Wall::UNIX_EPOCH + Duration::from_millis(hang::catalog::MOQ_EPOCH_UNIX_MILLIS);
		let config = moq_mux::catalog::Config::default()
			.with_clock(moq_mux::Clock::at(moq_net::time::Instant::now(), wall).expect("a representable wall"));
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast.broadcast().clone(), config).unwrap();

		let reserved = catalog.reserve();
		let mut registration = catalog.test_video("video0").unwrap();
		registration.set(video_config()).unwrap();
		drop(reserved);

		let track = broadcast.create_track("video0", timed()).unwrap();
		let mut media = catalog
			.media_producer(
				track,
				moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Data),
			)
			.unwrap();
		media.write(frame(0, true)).unwrap();
		media.write(frame(2_000_000, true)).unwrap();
		media.write(frame(4_000_000, true)).unwrap(); // live edge

		let broadcaster = Broadcaster::new(crate::resolved(&origin, "live").await, Config::default());
		let _ = tokio::time::timeout(Duration::from_secs(5), broadcaster.ready()).await;
		let rendition = broadcaster.rendition(Kind::Video, "video0").expect("video discovered");
		let _ = tokio::time::timeout(Duration::from_secs(5), rendition.playable()).await;

		// The timeline section carries no wall field anymore; the root clock names the epoch.
		let snapshot = rendition.snapshot();
		assert_eq!(
			snapshot.program_date_time,
			Some(moq_clock::Wall::UNIX_EPOCH + Duration::from_millis(hang::catalog::MOQ_EPOCH_UNIX_MILLIS))
		);
		let playlist = rendition.media_playlist(None).unwrap().expect("playlist renders");
		assert!(
			playlist.contains("#EXT-X-PROGRAM-DATE-TIME:2020-01-01T00:00:00.000Z\n"),
			"pts 0 maps through the root clock: {playlist}"
		);

		let manifest = broadcaster
			.manifest(None)
			.unwrap()
			.expect("manifest renders once playable");
		assert!(
			manifest.contains(" availabilityStartTime=\"2020-01-01T00:00:00.000Z\""),
			"the manifest anchors at the root clock: {manifest}"
		);

		drop((media, registration, broadcast));
	}

	// Without a catalog clock the DASH manifest still anchors, estimating pts 0 from the first
	// record's arrival, and playlists simply omit EXT-X-PROGRAM-DATE-TIME.
	#[tokio::test]
	async fn manifest_falls_back_to_arrival_anchor_without_a_clock() {
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").expect("publish allowed");
		broadcast.announce(Default::default()).expect("publish allowed");
		settle().await;

		// Write the catalog track directly: a moq-mux catalog producer always advertises its clock,
		// as a publisher on a host whose wall clock is set does.
		let timelines = moq_mux::timeline::Timelines::new(&broadcast);
		let duration = Duration::from_secs(2);
		let recorder = timelines
			.track(
				"video0",
				moq_mux::timeline::Config::default()
					.with_duration_min(duration)
					.with_duration_hint(duration),
			)
			.unwrap();
		let mut snapshot = moq_mux::catalog::hang::Catalog::<()>::default();
		snapshot.video.renditions.insert("video0".to_string(), video_config());
		snapshot.archive = Some(timelines.section());
		let mut json = moq_json::snapshot::Config::default();
		json.deltas = false;
		let mut catalog = moq_json::snapshot::Producer::new(
			broadcast
				.create_track(hang::Catalog::DEFAULT_NAME, hang::Catalog::default_track_info())
				.unwrap(),
			json,
		);
		catalog
			.update(moq_net::Timed::from(&snapshot).at(moq_net::Timestamp::now()))
			.unwrap();

		let track = broadcast.create_track("video0", timed()).unwrap();
		let mut media = moq_mux::container::Producer::new(
			track,
			moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Data),
		)
		.with_recorder(recorder);
		media.write(frame(0, true)).unwrap();
		media.write(frame(2_000_000, true)).unwrap();
		media.write(frame(4_000_000, true)).unwrap(); // live edge

		let broadcaster = Broadcaster::new(crate::resolved(&origin, "live").await, Config::default());
		let _ = tokio::time::timeout(Duration::from_secs(5), broadcaster.ready()).await;
		let rendition = broadcaster.rendition(Kind::Video, "video0").expect("video discovered");
		let _ = tokio::time::timeout(Duration::from_secs(5), rendition.playable()).await;

		assert_eq!(rendition.snapshot().program_date_time, None);
		let playlist = rendition.media_playlist(None).unwrap().expect("playlist renders");
		assert!(!playlist.contains("PROGRAM-DATE-TIME"), "{playlist}");

		let manifest = broadcaster
			.manifest(None)
			.unwrap()
			.expect("manifest renders once playable");
		assert!(
			manifest.contains(" availabilityStartTime=\""),
			"pts 0 is anchored even without a catalog clock"
		);

		drop((media, catalog, broadcast));
	}

	// The playlist copies the root clock when it first sees a rendition, so an fMP4 import must
	// not publish its catalog on a provisional clock that the first fragment then re-anchors.
	#[tokio::test(start_paused = true)]
	async fn fmp4_import_times_playlists_by_the_anchored_clock() {
		// An hour into the encoder's own timeline, a keyframe every 2s.
		let start = 3_600_000_000;
		let muxer = moq_mux::container::fmp4::Muxer::video(&video_config()).unwrap();
		let init = muxer.init().unwrap().expect("a VP8 init from the coded size");
		let mut fragments = Vec::new();
		for i in 0..6u64 {
			let frame = vp8_frame(start + i * 1_000_000, i % 2 == 0);
			fragments.extend_from_slice(&muxer.fragment(i as u32, &[frame]).unwrap());
		}

		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").expect("publish allowed");
		broadcast.announce(Default::default()).expect("publish allowed");
		settle().await;
		let catalog =
			moq_mux::catalog::Producer::new(&mut broadcast.broadcast().clone(), moq_mux::catalog::Config::default())
				.unwrap();
		let mut import = moq_mux::container::fmp4::Import::new(broadcast.broadcast().clone(), catalog.reserve());
		import.decode(&init).unwrap();

		// Watch before the first fragment, as a live viewer would, so a catalog the moov
		// published on a provisional clock would stick.
		let broadcaster = Broadcaster::new(crate::resolved(&origin, "live").await, Config::default());
		assert!(
			next_event(&mut broadcaster.renditions()).await.is_none(),
			"the moov alone lists no rendition"
		);

		import.decode(&fragments).unwrap();
		let anchored = catalog.snapshot().clock.expect("the catalog advertises a clock");

		tokio::time::timeout(Duration::from_secs(5), broadcaster.ready())
			.await
			.expect("a rendition becomes playable");
		let video = catalog
			.snapshot()
			.video
			.renditions
			.into_keys()
			.next()
			.expect("a video rendition");
		let rendition = broadcaster.rendition(Kind::Video, &video).expect("video discovered");
		let playlist = tokio::time::timeout(Duration::from_secs(5), rendition.playlist(None, None))
			.await
			.expect("a listed segment")
			.unwrap()
			.expect("playlist renders");

		let first = anchored
			.wall_clock(moq_net::Timestamp::from_micros(start).unwrap())
			.unwrap();
		let expected = format!(
			"#EXT-X-PROGRAM-DATE-TIME:{}\n",
			humantime::format_rfc3339_millis(first.into())
		);
		assert!(playlist.contains(&expected), "{expected} in {playlist}");

		drop((import, catalog));
	}

	#[tokio::test(start_paused = true)]
	async fn a_live_fmp4_import_serves_its_listed_segments() {
		live_fmp4_segments(None, false).await;
	}

	#[tokio::test(start_paused = true)]
	async fn a_live_fmp4_import_serves_segments_over_lite06() {
		live_fmp4_segments(Some("moq-lite-06"), false).await;
	}

	#[tokio::test(start_paused = true)]
	async fn a_live_fmp4_import_serves_segments_over_lite07() {
		live_fmp4_segments(Some("moq-lite-07-wip"), false).await;
	}

	#[tokio::test(start_paused = true)]
	async fn a_live_fmp4_import_serves_segments_after_timeline_declarations() {
		live_fmp4_segments(None, true).await;
	}

	async fn live_fmp4_segments(version: Option<&str>, late: bool) {
		use moq_mux::container::fmp4::mp4_atom::{self, DecodeMaybe, Encode};
		// Reuse the A/V descriptors; opaque samples exercise timeline discovery and FETCH,
		// without involving a codec decoder.
		let mut fixture =
			std::io::Cursor::new(include_bytes!("../../../moq-mux/src/container/fmp4/test_data/bbb.mp4").as_slice());
		let mut init = Vec::new();
		while let Some(atom) = mp4_atom::Any::decode_maybe(&mut fixture).unwrap() {
			let complete = matches!(atom, mp4_atom::Any::Moov(_));
			atom.encode(&mut init).unwrap();
			if complete {
				break;
			}
		}
		let origin = produce_origin();
		let remote = version.map(|_| produce_origin());
		let _sessions = if let Some(version) = version {
			let version: moq_net::Version = version.parse().unwrap();
			let mut config = moq_net::transport::memory::Config::default();
			config.protocol = Some(version.alpn().to_string());
			let (client_transport, server_transport) = moq_net::transport::memory::pair(config);
			let client = moq_net::Client::new()
				.with_versions(version.into())
				.with_publisher(origin.consume());
			let server = moq_net::Server::new()
				.with_versions(version.into())
				.with_subscriber(remote.as_ref().unwrap().clone());
			Some(tokio::join!(
				async {
					let (session, driver) = client
						.connect(moq_net::time::Instant::now(), client_transport)
						.await
						.unwrap();
					tokio::spawn(moq_net::time::run(driver));
					session
				},
				async {
					let (session, driver) = server
						.accept(moq_net::time::Instant::now(), server_transport)
						.await
						.unwrap();
					tokio::spawn(moq_net::time::run(driver));
					session
				}
			))
		} else {
			None
		};
		let broadcast = origin.create_broadcast("live").unwrap();
		broadcast.announce(Default::default()).unwrap();
		settle().await;
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast.broadcast().clone(), Default::default()).unwrap();
		let mut import = moq_mux::container::fmp4::Import::new(broadcast.broadcast().clone(), catalog.reserve());
		import.decode(&init).unwrap();
		let snapshot = catalog.snapshot();
		let video_muxer =
			moq_mux::container::fmp4::Muxer::video(snapshot.video.renditions.values().next().unwrap()).unwrap();
		let audio_muxer =
			moq_mux::container::fmp4::Muxer::audio(snapshot.audio.renditions.values().next().unwrap()).unwrap();
		let consuming = remote.as_ref().unwrap_or(&origin);
		consuming.consume().routed("live").await.unwrap();
		let source = crate::resolved(consuming, "live").await;
		let broadcaster = (!late).then(|| Broadcaster::new(source.clone(), Config::default()));
		for i in 0..20u64 {
			let fragment = video_muxer
				.fragment(i as u32, &[vp8_frame(i * 1_000_000, i % 2 == 0)])
				.unwrap();
			import.decode(&fragment).unwrap();
			let mut sample = frame(i * 1_000_000, true);
			sample.duration = Some(moq_net::Timestamp::from_secs(1).unwrap());
			let audio = audio_muxer.fragment(i as u32, &[sample]).unwrap();
			let mut audio = std::io::Cursor::new(audio.as_ref());
			let mut fragment = Vec::new();
			while let Some(mut atom) = mp4_atom::Any::decode_maybe(&mut audio).unwrap() {
				if let mp4_atom::Any::Moof(moof) = &mut atom {
					moof.traf[0].tfhd.track_id = 2;
				}
				atom.encode(&mut fragment).unwrap();
			}
			import.decode(&fragment).unwrap();
			settle().await;
		}
		let broadcaster = broadcaster.unwrap_or_else(|| Broadcaster::new(source, Config::default()));
		tokio::time::timeout(Duration::from_secs(5), broadcaster.ready())
			.await
			.unwrap();
		for (kind, name) in [
			(
				Kind::Video,
				catalog.snapshot().video.renditions.into_keys().next().unwrap(),
			),
			(
				Kind::Audio,
				catalog.snapshot().audio.renditions.into_keys().next().unwrap(),
			),
		] {
			let rendition = broadcaster.rendition(kind, &name).unwrap();
			tokio::time::timeout(Duration::from_secs(5), rendition.playlist(None, None))
				.await
				.unwrap()
				.unwrap()
				.unwrap();
			let rows = rendition.snapshot().segments;
			assert!(rows.len() >= 2, "multiple completed segments are listed");
			for row in rows {
				assert!(!row.gap, "{kind:?} segment {} has a timeline", row.segment);
				let media = rendition.segment(&row.tag, row.segment).await.unwrap();
				assert!(media.is_some(), "listed segment {} must have media", row.segment);
			}
			let mut cursor = rendition.segments();
			let segment = tokio::time::timeout(Duration::from_secs(5), cursor.next())
				.await
				.expect("a live recording produces a media segment")
				.unwrap();
			assert!(segment.is_some(), "the live cursor has media");
		}
		drop((import, catalog));
	}

	// A finished broadcast renders a static presentation, offset to its first listed segment.
	#[tokio::test]
	async fn dash_manifest_turns_static_when_finished() {
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").expect("publish allowed");
		broadcast.announce(Default::default()).expect("publish allowed");
		settle().await;
		let mut catalog =
			moq_mux::catalog::Producer::new(&mut broadcast.broadcast().clone(), moq_mux::catalog::Config::default())
				.unwrap();

		let reserved = catalog.reserve();
		let mut registration = catalog.test_video("video0").unwrap();
		registration.set(video_config()).unwrap();
		drop(reserved);

		let track = broadcast.create_track("video0", timed()).unwrap();
		let mut media = catalog
			.media_producer(
				track,
				moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Data),
			)
			.unwrap();
		media.write(frame(0, true)).unwrap();
		media.write(frame(2_000_000, true)).unwrap();

		let broadcaster = Broadcaster::new(crate::resolved(&origin, "live").await, Config::default());
		let _ = tokio::time::timeout(Duration::from_secs(5), broadcaster.ready()).await;
		let _ = tokio::time::timeout(Duration::from_secs(5), broadcaster.playable()).await;

		// A clean finish promotes the live edge into the final segment and ends the windows.
		media.finish().unwrap();
		catalog.finish().unwrap();

		let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
		let manifest = loop {
			let manifest = broadcaster.manifest(None).unwrap().expect("manifest renders");
			if manifest.contains(" type=\"static\"") {
				break manifest;
			}
			assert!(tokio::time::Instant::now() < deadline, "manifest never turned static");
			tokio::time::sleep(Duration::from_millis(50)).await;
		};
		// The clean finish promoted the live edge into a final zero-duration segment (its
		// single frame has no successor to bound it). HLS lists it as a trailing EXTINF:0,
		// but a SegmentTimeline entry at the presentation end would never be requested, so
		// the manifest omits it and the presentation ends at the last bounded segment.
		assert!(manifest.contains("<S t=\"0\" d=\"2000\"/>"));
		assert!(
			!manifest.contains("<S t=\"2000\""),
			"the zero-duration tail is not listed"
		);
		assert!(manifest.contains(" mediaPresentationDuration=\"PT2.000S\""));
		assert!(!manifest.contains("presentationTimeOffset"));
		assert!(!manifest.contains("availabilityStartTime"));

		drop((catalog, media, registration, broadcast));
	}

	// The property HLS needs from the timeline rework: audio and video renditions share one
	// segment numbering, cut at the same boundaries. Video is one group per segment; an audio
	// segment packs every audio group inside the video segment's span.
	// Every rendition has its own timeline. The reference (the first video rendition by name) sets
	// the segment boundaries; another rendition snaps them to its own keyframes, and a boundary with
	// no keyframe nearby, such as before a rendition starts, is a gap.
	#[tokio::test]
	async fn renditions_snap_to_their_own_keyframes() {
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").expect("publish allowed");
		broadcast.announce(Default::default()).expect("publish allowed");
		settle().await;
		let catalog =
			moq_mux::catalog::Producer::new(&mut broadcast.broadcast().clone(), moq_mux::catalog::Config::default())
				.unwrap();

		let reserved = catalog.reserve();
		let mut registrations = Vec::new();
		for name in ["video0", "video1", "video2"] {
			let mut registration = catalog.test_video(name).unwrap();
			registration.set(video_config()).unwrap();
			registrations.push(registration);
		}
		drop(reserved);

		let mut producers = Vec::new();
		// The reference cuts every 2s; video1's GOPs sit 300ms later; video2 starts at 4s.
		for (name, gops) in [
			("video0", &[0u64, 2_000, 4_000, 6_000, 8_000][..]),
			("video1", &[300, 2_300, 4_300, 6_300, 8_300]),
			("video2", &[4_000, 6_000, 8_000]),
		] {
			let track = broadcast.create_track(name, timed()).unwrap();
			let mut media = catalog
				.media_producer(
					track,
					moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Data),
				)
				.unwrap();
			for ms in gops {
				media.write(frame(ms * 1_000, true)).unwrap();
			}
			producers.push(media);
		}

		let broadcaster = Broadcaster::new(crate::resolved(&origin, "live").await, Config::default());
		let listed = |name: &'static str, count: usize| {
			let broadcaster = broadcaster.clone();
			async move {
				for _ in 0..500 {
					if let Some(rendition) = broadcaster.rendition(Kind::Video, name) {
						let snapshot = rendition.snapshot();
						if snapshot.segments.len() >= count {
							return snapshot;
						}
					}
					tokio::time::sleep(Duration::from_millis(10)).await;
				}
				panic!("{name} never listed {count} segments");
			}
		};

		let reference = listed("video0", 4).await;
		assert_eq!(
			reference.segments.iter().map(|s| s.segment).collect::<Vec<_>>(),
			vec![0, 1, 2, 3]
		);

		// A segment is listed once the rendition's own timeline is a tolerance past its end.
		let offset = listed("video1", 3).await;
		assert_eq!(offset.media_sequence, reference.media_sequence, "one numbering");
		assert!(offset.segments.iter().all(|s| !s.gap), "every boundary snaps");
		let snapped = broadcaster
			.rendition(Kind::Video, "video1")
			.unwrap()
			.listed_segment(1)
			.await
			.unwrap()
			.expect("the snapped segment is served");
		assert_eq!(&snapped[4..8], b"moof");

		let late = listed("video2", 3).await;
		assert_eq!(
			late.segments.iter().map(|s| s.gap).collect::<Vec<_>>(),
			vec![true, true, false],
			"nothing to serve before the rendition starts"
		);
		let video2 = broadcaster.rendition(Kind::Video, "video2").unwrap();
		assert!(
			video2.listed_segment(0).await.unwrap().is_none(),
			"a gap is never fetched"
		);

		drop((producers, registrations, broadcast));
	}

	// A broadcast without video takes its boundaries from its first audio rendition.
	#[tokio::test]
	async fn an_audio_only_broadcast_segments_on_its_audio() {
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").expect("publish allowed");
		broadcast.announce(Default::default()).expect("publish allowed");
		settle().await;
		let catalog =
			moq_mux::catalog::Producer::new(&mut broadcast.broadcast().clone(), moq_mux::catalog::Config::default())
				.unwrap();

		let reserved = catalog.reserve();
		let mut registration = catalog.test_audio("audio0").unwrap();
		registration
			.set(hang::catalog::AudioConfig::new(
				hang::catalog::AudioCodec::Opus,
				48_000,
				2,
			))
			.unwrap();
		drop(reserved);

		let track = broadcast.create_track("audio0", timed()).unwrap();
		let mut audio = catalog
			.media_producer(
				track,
				moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Data),
			)
			.unwrap();
		// 500ms groups pack into records of the default 2s minimum.
		for micros in (0..=6_000_000u64).step_by(500_000) {
			let keyframe = audio.needs_keyframe();
			audio.write(frame(micros, keyframe)).unwrap();
			audio.cut(None).unwrap();
		}

		let broadcaster = Broadcaster::new(crate::resolved(&origin, "live").await, Config::default());
		let _ = tokio::time::timeout(Duration::from_secs(5), broadcaster.ready()).await;
		let rendition = broadcaster.rendition(Kind::Audio, "audio0").expect("audio discovered");
		let _ = tokio::time::timeout(Duration::from_secs(5), rendition.playable()).await;

		let playlist = rendition.snapshot();
		assert_eq!(
			playlist.segments.iter().map(|s| s.segment).collect::<Vec<_>>(),
			vec![0, 1, 2]
		);
		assert_eq!(playlist.segments[0].duration, Duration::from_secs(2));
		let segment = rendition.listed_segment(1).await.unwrap().expect("audio segment");
		assert_eq!(&segment[4..8], b"moof");

		drop((audio, registration, broadcast));
	}

	// The declared duration is nominal, so the exporter derives EXT-X-TARGETDURATION from the
	// segments it has. A playlist whose EXTINF exceeds its target duration is invalid.
	#[tokio::test]
	async fn target_duration_follows_the_observed_segments() {
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").expect("publish allowed");
		broadcast.announce(Default::default()).expect("publish allowed");
		settle().await;
		let catalog =
			moq_mux::catalog::Producer::new(&mut broadcast.broadcast().clone(), moq_mux::catalog::Config::default())
				.unwrap();

		let reserved = catalog.reserve();
		let mut registration = catalog.test_video("video0").unwrap();
		registration
			.set(hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::VP8))
			.unwrap();
		drop(reserved);

		// 3s GOPs against the default 1s minimum: every segment is one whole GOP.
		let track = broadcast.create_track("video0", timed()).unwrap();
		let mut media = catalog
			.media_producer(
				track,
				moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Data),
			)
			.unwrap();
		media.write(frame(0, true)).unwrap();
		media.write(frame(3_000_000, true)).unwrap();
		media.write(frame(6_000_000, true)).unwrap();

		let broadcaster = Broadcaster::new(crate::resolved(&origin, "live").await, Config::default());
		let _ = tokio::time::timeout(Duration::from_secs(5), broadcaster.ready()).await;
		let rendition = broadcaster.rendition(Kind::Video, "video0").expect("rendition");
		let _ = tokio::time::timeout(Duration::from_secs(5), rendition.playable()).await;

		let playlist = rendition.snapshot();
		assert_eq!(playlist.segments[0].duration, Duration::from_secs(3));
		assert_eq!(
			playlist.target_duration, 3,
			"the target duration covers the 3s segment, not the 10s split ceiling"
		);
	}

	#[tokio::test]
	async fn audio_and_video_segments_are_aligned() {
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").expect("publish allowed");
		broadcast.announce(Default::default()).expect("publish allowed");
		settle().await;
		let catalog =
			moq_mux::catalog::Producer::new(&mut broadcast.broadcast().clone(), moq_mux::catalog::Config::default())
				.unwrap();

		let reserved = catalog.reserve();
		let mut video_registration = catalog.test_video("video0").unwrap();
		video_registration.set(video_config()).unwrap();
		let mut audio_registration = catalog.test_audio("audio0").unwrap();
		let audio_config = hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Opus, 48_000, 2);
		audio_registration.set(audio_config).unwrap();
		drop(reserved);

		let video_track = broadcast.create_track("video0", timed()).unwrap();
		let mut video = catalog
			.media_producer(
				video_track,
				moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Data),
			)
			.unwrap();
		let audio_track = broadcast.create_track("audio0", timed()).unwrap();
		let mut audio = catalog
			.media_producer(
				audio_track,
				moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Data),
			)
			.unwrap();

		// Interleaved by pts, as a muxer would demux them: video keyframes every 2s open the
		// segments; audio publishes a 500ms group each cut.
		let mut audio_write = |micros: u64| {
			let keyframe = audio.needs_keyframe();
			audio.write(frame(micros, keyframe)).unwrap();
			audio.cut(None).unwrap();
		};
		video.write(frame(0, true)).unwrap(); // segment 0
		for micros in [0u64, 500_000, 1_000_000, 1_500_000] {
			audio_write(micros); // audio groups 0..=3 pack into segment 0
		}
		video.write(frame(2_000_000, true)).unwrap(); // segment 1
		for micros in [2_000_000u64, 2_500_000, 3_000_000, 3_500_000] {
			audio_write(micros); // audio groups 4..=7 pack into segment 1
		}
		video.write(frame(4_000_000, true)).unwrap(); // segment 2 (live edge)
		audio_write(4_000_000); // audio group 8 opens its slice of segment 2

		let broadcaster = Broadcaster::new(crate::resolved(&origin, "live").await, Config::default());
		let _ = tokio::time::timeout(Duration::from_secs(5), broadcaster.ready()).await;
		let video_rendition = broadcaster.rendition(Kind::Video, "video0").expect("video discovered");
		let audio_rendition = broadcaster.rendition(Kind::Audio, "audio0").expect("audio discovered");
		let _ = tokio::time::timeout(Duration::from_secs(5), video_rendition.playable()).await;
		let _ = tokio::time::timeout(Duration::from_secs(5), audio_rendition.playable()).await;

		// Both playlists list the same segment numbers over the same spans.
		let video_playlist = video_rendition.snapshot();
		let audio_playlist = audio_rendition.snapshot();
		assert_eq!(video_playlist.media_sequence, audio_playlist.media_sequence);
		let video_segments: Vec<u64> = video_playlist.segments.iter().map(|s| s.segment).collect();
		let audio_segments: Vec<u64> = audio_playlist.segments.iter().map(|s| s.segment).collect();
		assert_eq!(video_segments, vec![0, 1]);
		assert_eq!(audio_segments, vec![0, 1], "audio lists the same aligned segments");
		assert_eq!(
			audio_playlist.segments[0].duration,
			Duration::from_secs(2),
			"cut at the video boundary"
		);

		// The same URI names the same span of content time on either rendition.
		let rendered = audio_rendition.media_playlist(None).unwrap().expect("playable");
		assert!(rendered.contains("seg/656e3d1b.0.m4s\n"));
		assert!(rendered.contains("seg/656e3d1b.1.m4s\n"));

		// A video segment is one group; the matching audio segment transmuxes its four groups.
		let video_segment = video_rendition.listed_segment(1).await.unwrap().expect("video segment");
		assert_eq!(&video_segment[4..8], b"moof");
		let audio_segment = audio_rendition.listed_segment(1).await.unwrap().expect("audio segment");
		assert_eq!(&audio_segment[4..8], b"moof");
		assert!(
			audio_segment.len() > video_segment.len(),
			"the audio segment packs multiple groups into one fragment"
		);

		drop((video, audio, video_registration, audio_registration, broadcast));
	}

	// Dropping the broadcaster must not truncate a recording in progress. A cursor holds its
	// rendition alive, so the rendition's own watcher still ends the timeline with `end()` --
	// which is what promotes the live-edge record into the final segment. Force-closing the
	// rendition here instead would race that (`end()` no-ops on a closed channel) and silently
	// drop the last segment of every recording.
	#[tokio::test]
	async fn dropping_the_broadcaster_keeps_a_cursor_drainable() {
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").expect("publish allowed");
		broadcast.announce(Default::default()).expect("publish allowed");
		settle().await;
		let mut catalog =
			moq_mux::catalog::Producer::new(&mut broadcast.broadcast().clone(), moq_mux::catalog::Config::default())
				.unwrap();

		let reserved = catalog.reserve();
		let mut registration = catalog.test_video("video0").unwrap();
		let config = video_config();
		registration.set(config).unwrap();
		drop(reserved);

		// Two GOPs: group 0 is complete, while group 1 stays at the live edge until the
		// publisher finishes.
		let track = broadcast.create_track("video0", timed()).unwrap();
		let mut media = catalog
			.media_producer(
				track,
				moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Data),
			)
			.unwrap();
		media.write(frame(0, true)).unwrap();
		media.write(frame(2_000_000, true)).unwrap();

		let broadcaster = Broadcaster::new(crate::resolved(&origin, "live").await, Config::default());
		let _ = tokio::time::timeout(Duration::from_secs(5), broadcaster.ready()).await;
		let rendition = broadcaster
			.rendition(Kind::Video, "video0")
			.expect("rendition discovered");
		let mut segments = rendition.segments();

		// The recorder drops the broadcaster before finalizing its uploads.
		drop(broadcaster);

		// Finish the source after teardown, forcing the broadcaster's drop to land before the
		// rendition watcher's clean-end path.
		media.finish().unwrap();
		catalog.finish().unwrap();

		let mut groups = Vec::new();
		while let Some(segment) = tokio::time::timeout(Duration::from_secs(5), segments.next())
			.await
			.expect("the cursor drains without parking")
			.unwrap()
		{
			groups.push(segment.segment);
		}
		assert!(
			groups.contains(&1),
			"the final segment survives dropping the broadcaster, got {groups:?}"
		);

		drop((catalog, media, registration, broadcast, rendition));
	}

	/// Whether `haystack` carries `needle` verbatim, i.e. whether a transmuxed segment carries a
	/// particular publisher's frame payloads.
	fn contains(haystack: &bytes::Bytes, needle: &[u8]) -> bool {
		haystack.windows(needle.len()).any(|window| window == needle)
	}

	/// Reconcile a one-video-rendition catalog snapshot by hand and drive the timeline, which is
	/// what `watch_catalog` does with each snapshot.
	fn export(
		upstream: &moq_mux::Source,
		config: &hang::catalog::VideoConfig,
	) -> (Arc<Rendition>, tokio::task::JoinHandle<()>) {
		let mut catalog = moq_mux::catalog::hang::Catalog::default();
		catalog.video.renditions.insert("video0".to_string(), config.clone());
		let archive = archive(&["video0", "audio0"]);
		catalog.archive = Some(archive.clone());

		let renditions = renditions::Producer::new(Config::default().window);
		renditions.sync(upstream, &catalog);
		let reference = Arc::new((Kind::Video, "video0".to_string(), "video0".to_string()));
		let watcher = tokio::spawn(watch_timeline(
			upstream.broadcast(),
			archive,
			reference,
			renditions.fanout(),
			false,
		));
		let rendition = renditions.get(Kind::Video, "video0").expect("rendition synced");
		(rendition, watcher)
	}

	struct Ended {
		rendition: Arc<Rendition>,
		watcher: tokio::task::JoinHandle<()>,
		origin: moq_net::origin::Producer,
		_held: Box<dyn std::any::Any>,
	}

	/// Finish the media before End while retaining its track cache for a late recorder.
	async fn ended_rendition(sibling: bool) -> Ended {
		const KEYFRAME: &[u8] = &[0x10, 0x00, 0x00, 0x9d, 0x01, 0x2a, 0x40, 0x01, 0xf0, 0x00];
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").unwrap();
		broadcast.announce(Default::default()).unwrap();
		let mut catalog =
			moq_mux::catalog::Producer::new(&mut broadcast.broadcast().clone(), Default::default()).unwrap();
		let target = if sibling {
			let target = origin.create_broadcast("media").unwrap();
			target.announce(Default::default()).unwrap();
			target
		} else {
			broadcast.clone()
		};
		let recorder = catalog.enroll_test("video0").unwrap();
		let mut media = write_routed_media(&mut target.broadcast().clone(), KEYFRAME, recorder, 0);
		media.finish().unwrap();
		catalog.finish().unwrap();
		settle().await;

		let source = crate::resolved(&origin, "live").await;
		let mut config = hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::VP8);
		if sibling {
			config.broadcast = Some(moq_net::path::Relative::new("media").to_owned());
		}
		let (rendition, watcher) = export(&source, &config);
		rendition.playable().await;
		assert!(!rendition.snapshot().segments.is_empty());
		let binding = rendition.poll_media(&kio::Waiter::noop());
		let bound = binding.broadcast().await.unwrap();
		target.close();
		bound.closed().await;
		assert!(
			target.consume().track("video0").is_err(),
			"new lookups are refused after End"
		);
		Ended {
			rendition,
			watcher,
			origin,
			_held: Box::new((catalog, media, broadcast, target)),
		}
	}

	// First-time inline init bootstrap must use the bound track, even after End.
	#[tokio::test]
	async fn init_bootstraps_after_the_catalog_broadcast_ends() {
		tokio::time::pause();
		let ended = ended_rendition(false).await;
		let result = ended.rendition.init().await;
		ended.watcher.abort();
		let init = result.unwrap().expect("inline init bootstraps after End");
		assert!(contains(&init, b"ftyp"));
	}

	#[tokio::test]
	async fn segments_drain_after_the_catalog_broadcast_ends() {
		tokio::time::pause();
		let ended = ended_rendition(false).await;
		let result = ended.rendition.listed_segment(0).await;
		ended.watcher.abort();
		assert!(result.unwrap().is_some(), "retained segment drains after End");
	}

	#[tokio::test]
	async fn init_bootstraps_after_the_sibling_broadcast_ends() {
		tokio::time::pause();
		let ended = ended_rendition(true).await;
		let result = ended.rendition.init().await;
		ended.watcher.abort();
		assert!(result.unwrap().is_some(), "sibling init bootstraps after End");
	}

	#[tokio::test]
	async fn segments_drain_after_the_sibling_broadcast_ends() {
		tokio::time::pause();
		let ended = ended_rendition(true).await;
		let result = ended.rendition.listed_segment(0).await;
		ended.watcher.abort();
		assert!(result.unwrap().is_some(), "retained sibling segment drains after End");
	}

	// A replacement is readable on a fresh binding, while the old rendition bootstraps
	// its first init and drains only the old instance's retained groups.
	#[tokio::test]
	async fn retained_media_after_replacement_keeps_its_original_instance() {
		tokio::time::pause();
		const OLD: &[u8] = &[0x10, 0x00, 0x00, 0x9d, 0x01, 0x2a, 0x40, 0x01, 0xf0, 0x00];
		const NEW: &[u8] = &[0x10, 0x00, 0x00, 0x9d, 0x01, 0x2a, 0x80, 0x02, 0xe0, 0x01];
		for sibling in [false, true] {
			let ended = ended_rendition(sibling).await;
			let path = if sibling { "media" } else { "live" };
			let next = ended.origin.create_broadcast(path).unwrap();
			next.announce(Default::default()).unwrap();
			let mut media = moq_mux::container::Producer::new(
				next.create_track("video0", timed()).unwrap(),
				moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Data),
			);
			media.write(payload_frame(0, true, NEW)).unwrap();
			media.finish().unwrap();
			settle().await;

			let fresh = crate::resolved(&ended.origin, path).await;
			let mut group = fresh
				.broadcast()
				.track("video0")
				.unwrap()
				.fetch_group(0, None)
				.await
				.unwrap();
			let config = hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::VP8);
			let mut muxer = moq_mux::container::fmp4::Muxer::video(&config).unwrap();
			let frames = muxer.read(&mut group).await.unwrap();
			assert!(frames.iter().any(|frame| frame.payload.as_ref() == NEW));
			let next_init = muxer.init().unwrap().unwrap();
			let old_init = ended.rendition.init().await.unwrap().unwrap();
			assert_ne!(
				old_init, next_init,
				"the old inline init must not acquire replacement geometry"
			);

			let mut cursor = ended.rendition.segments();
			let mut drained = Vec::new();
			while let Some(segment) = cursor.next().await.unwrap() {
				assert!(contains(&segment.media, OLD));
				assert!(!contains(&segment.media, NEW));
				drained.push(segment.segment);
			}
			assert_eq!(drained, [0, 1, 2], "the recorder drains the complete retained tail");
			ended.watcher.abort();
		}
	}

	// A path published again after its broadcast ended is a brand new broadcast. Renditions
	// derived from the old broadcast's catalog must not serve the replacement's media: its group
	// numbering restarts, so those bytes would be served under the replaced broadcast's segment
	// number, duration, and PROGRAM-DATE-TIME.
	#[tokio::test]
	async fn a_replacement_publisher_is_not_served_under_the_replaced_catalog() {
		const OLD: &[u8] = b"OLDOLDOLDOLDOLDO";
		const NEW: &[u8] = b"NEWNEWNEWNEWNEWN";

		// Publish broadcast "live": a `video0` rendition plus three keyframe groups whose frames
		// carry `payload`, so the two epochs' media differs byte for byte. The returned handle
		// keeps the publisher alive until it is dropped.
		fn publish(
			origin: &moq_net::origin::Producer,
			payload: &'static [u8],
		) -> (Box<dyn std::any::Any>, hang::catalog::VideoConfig) {
			let broadcast = origin.create_broadcast("live").expect("publish allowed");
			broadcast.announce(Default::default()).expect("publish allowed");
			let catalog = moq_mux::catalog::Producer::new(
				&mut broadcast.broadcast().clone(),
				moq_mux::catalog::Config::default(),
			)
			.unwrap();

			let reserved = catalog.reserve();
			let mut registration = catalog.test_video("video0").unwrap();
			let config = video_config();
			registration.set(config.clone()).unwrap();
			drop(reserved);

			let track = broadcast.create_track("video0", timed()).unwrap();
			let mut media = catalog
				.media_producer(
					track,
					moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Data),
				)
				.unwrap();
			media.write(payload_frame(0, true, payload)).unwrap();
			media.write(payload_frame(2_000_000, true, payload)).unwrap();
			media.write(payload_frame(4_000_000, true, payload)).unwrap();

			(Box::new((broadcast, catalog, registration, media)), config)
		}

		let origin = produce_origin();
		let (old, config) = publish(&origin, OLD);
		settle().await;

		// The export binds to the broadcast it reads the catalog from.
		let upstream = crate::resolved(&origin, "live").await;

		// The publisher goes away and comes back before the catalog snapshot is reconciled. That
		// gap is wide in practice: `watch_catalog` retries a not-yet-written catalog track with
		// backoff, so seconds can pass between resolving the broadcast and syncing its first
		// snapshot. The old broadcast ends first: a path still routed is one broadcast, so a
		// publisher back before then would resume it instead.
		drop(old);
		settle().await;
		let (new, _) = publish(&origin, NEW);
		settle().await;

		let (rendition, watcher) = export(&upstream, &config);
		let _ = tokio::time::timeout(Duration::from_secs(1), rendition.playable()).await;
		let served = rendition.listed_segment(0).await.unwrap();
		if let Some(served) = &served {
			assert!(
				!contains(served, NEW),
				"the replacement publisher's media was served under the replaced broadcast's catalog"
			);
		}
		assert!(
			served.is_none(),
			"the replaced broadcast is closed, so it serves nothing"
		);
		watcher.abort();

		// Control: an export started after the takeover does serve the new publisher's media, so
		// the assertion above is about which epoch a rendition is bound to, not about a broadcast
		// that happens to be unreadable.
		let fresh = crate::resolved(&origin, "live").await;
		let (rendition, watcher) = export(&fresh, &config);
		tokio::time::timeout(Duration::from_secs(5), rendition.playable())
			.await
			.expect("the new publisher's timeline arrives");
		let served = rendition
			.listed_segment(0)
			.await
			.unwrap()
			.expect("the new publisher is servable on its own catalog");
		assert!(
			contains(&served, NEW),
			"the new export serves the new publisher's media"
		);
		watcher.abort();

		drop(new);
	}

	// The sibling half of the case above. A rendition whose catalog names another broadcast is
	// bound to whatever serves that path when the rendition is created, not when its first
	// segment is requested: the timeline on the catalog broadcast keeps describing the epoch it
	// was written for, so a media publisher that reconnects (restarting its group numbering)
	// must not answer for segments the old catalog already described.
	#[tokio::test]
	async fn a_replacement_sibling_is_not_served_under_the_replaced_catalog() {
		const OLD: &[u8] = b"OLDOLDOLDOLDOLDO";
		const NEW: &[u8] = b"NEWNEWNEWNEWNEWN";

		// Publish broadcast "media": a `video0` track of three keyframe groups whose frames carry
		// `payload`, so the two epochs' media differs byte for byte. `recorder` enrolls the track
		// in the *catalog* broadcast's timeline; the replacement publishes without one, exactly
		// as a reconnecting media publisher does before the catalog broadcast catches up.
		fn publish_media(
			origin: &moq_net::origin::Producer,
			payload: &'static [u8],
			recorder: Option<moq_mux::timeline::Recorder>,
		) -> Box<dyn std::any::Any> {
			let broadcast = origin.create_broadcast("media").expect("publish allowed");
			broadcast.announce(Default::default()).expect("publish allowed");
			let track = broadcast.create_track("video0", timed()).unwrap();

			let mut media = moq_mux::container::Producer::new(
				track,
				moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Data),
			);
			if let Some(recorder) = recorder {
				media = media.with_recorder(recorder);
			}
			media.write(payload_frame(0, true, payload)).unwrap();
			media.write(payload_frame(2_000_000, true, payload)).unwrap();
			media.write(payload_frame(4_000_000, true, payload)).unwrap();

			Box::new((broadcast, media))
		}

		let origin = produce_origin();

		// The catalog broadcast carries the catalog and the timeline; the media lives next door.
		let live = origin.create_broadcast("live").expect("publish allowed");
		live.announce(Default::default()).expect("publish allowed");
		let catalog =
			moq_mux::catalog::Producer::new(&mut live.broadcast().clone(), moq_mux::catalog::Config::default())
				.unwrap();
		let recorder = catalog.enroll_test("video0").unwrap();

		let old_media = publish_media(&origin, OLD, Some(recorder));
		settle().await;

		let upstream = crate::resolved(&origin, "live").await;

		// A relative reference replaces the base's last segment, so "media" is a sibling of the
		// catalog broadcast "live".
		let mut config = video_config();
		config.broadcast = Some(moq_net::path::Relative::new("media").to_owned());

		let (rendition, watcher) = export(&upstream, &config);
		tokio::time::timeout(Duration::from_secs(5), rendition.playable())
			.await
			.expect("the timeline reaches the rendition");

		// The media publisher goes away and comes back before any segment is requested, as a
		// brand new broadcast whose group numbering restarts from zero.
		drop(old_media);
		settle().await;
		let new_media = publish_media(&origin, NEW, None);
		settle().await;

		let served = rendition
			.listed_segment(0)
			.await
			.expect("a replaced sibling is an unavailable group, not a server error");
		if let Some(served) = &served {
			assert!(
				!contains(served, NEW),
				"the replacement publisher's media was served under the replaced sibling's segment numbers"
			);
		}
		let served = served.expect("the old sibling's retained media drains after replacement");
		assert!(contains(&served, OLD));
		watcher.abort();

		// Control: an export started after the takeover does serve the new publisher's media, so
		// the assertion above is about which epoch the rendition is bound to, not about a
		// broadcast that happens to be unreadable.
		let fresh = crate::resolved(&origin, "live").await;
		let (rendition, watcher) = export(&fresh, &config);
		tokio::time::timeout(Duration::from_secs(5), rendition.playable())
			.await
			.expect("the timeline reaches the fresh rendition");
		let served = rendition
			.listed_segment(0)
			.await
			.unwrap()
			.expect("the new publisher is servable on a freshly bound rendition");
		assert!(
			contains(&served, NEW),
			"the new export serves the new publisher's media"
		);
		watcher.abort();

		drop((new_media, catalog, live));
	}

	fn hops(ids: &[u64]) -> moq_net::Hops {
		let mut hops = moq_net::Hops::new();
		for id in ids {
			hops.push(moq_net::Hop::new(*id).unwrap()).unwrap();
		}
		hops
	}

	fn sibling_route(first: u64) -> moq_net::origin::Route {
		moq_net::origin::Route::default().with_hops(hops(&[first]))
	}

	/// A standalone media publisher (not an origin leaf), so the sibling is served through a
	/// routed front.
	fn write_routed_media(
		broadcast: &mut moq_net::broadcast::Producer,
		payload: &'static [u8],
		recorder: moq_mux::timeline::Recorder,
		start_micros: u64,
	) -> moq_mux::container::Producer<moq_mux::catalog::hang::Container> {
		let track = broadcast.create_track("video0", timed()).unwrap();
		let mut media = moq_mux::container::Producer::new(
			track,
			moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Data),
		)
		.with_recorder(recorder);
		media.write(payload_frame(start_micros, true, payload)).unwrap();
		media
			.write(payload_frame(start_micros + 2_000_000, true, payload))
			.unwrap();
		media
			.write(payload_frame(start_micros + 4_000_000, true, payload))
			.unwrap();
		media
	}

	async fn accept_sibling(server: &moq_net::origin::Dynamic, broadcast: &moq_net::broadcast::Producer) {
		let request = tokio::time::timeout(Duration::from_secs(5), server.requested_broadcast())
			.await
			.expect("the sibling path is requested")
			.expect("the handler is open");
		request.accept(broadcast);
	}

	async fn until_finished(rendition: &Rendition) {
		let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
		loop {
			if rendition.snapshot().finished {
				return;
			}
			assert!(
				tokio::time::Instant::now() < deadline,
				"the old sibling's playlist did not end"
			);
			tokio::task::yield_now().await;
		}
	}

	// A bound sibling ends the playlist while its retained rows drain. Another publisher at
	// that path is never bound, so its bytes cannot answer the old timeline.
	#[tokio::test]
	async fn a_replaced_sibling_ends_the_rendition() {
		tokio::time::pause();
		const OLD: &[u8] = b"OLDOLDOLDOLDOLDO";
		const NEW: &[u8] = b"NEWNEWNEWNEWNEWN";

		let origin = produce_origin();
		let live = origin.create_broadcast("live").expect("publish allowed");
		live.announce(Default::default()).expect("publish allowed");
		let catalog =
			moq_mux::catalog::Producer::new(&mut live.broadcast().clone(), moq_mux::catalog::Config::default())
				.unwrap();
		let recorder = catalog.enroll_test("video0").unwrap();

		let mut old_media = moq_net::broadcast::Info::new().produce();
		let _old_track = write_routed_media(&mut old_media, OLD, recorder, 0);
		let old_server = origin.dynamic_route("media", sibling_route(10)).unwrap();
		settle().await;

		let upstream = crate::resolved(&origin, "live").await;
		let mut config = video_config();
		config.broadcast = Some(moq_net::path::Relative::new("media").to_owned());

		let mut snapshot = moq_mux::catalog::hang::Catalog::default();
		snapshot.video.renditions.insert("video0".to_string(), config.clone());
		let archive = archive(&["video0"]);
		snapshot.archive = Some(archive.clone());
		let renditions = renditions::Producer::new(Config::default().window);
		renditions.sync(&upstream, &snapshot);
		let fanout = renditions.fanout();
		let reference = Arc::new((Kind::Video, "video0".to_string(), "video0".to_string()));
		let watcher = tokio::spawn(watch_timeline(
			upstream.broadcast(),
			archive,
			reference,
			fanout.clone(),
			false,
		));
		let rendition = renditions.get(Kind::Video, "video0").unwrap();
		accept_sibling(&old_server, &old_media).await;
		tokio::time::timeout(Duration::from_secs(5), rendition.playable())
			.await
			.expect("the timeline reaches the rendition");
		let served = rendition
			.listed_segment(0)
			.await
			.unwrap()
			.expect("the original sibling is servable");
		assert!(contains(&served, OLD), "the first hop serves the original publisher");
		assert!(!rendition.snapshot().segments.is_empty());
		let mut early = rendition.segments();
		assert_eq!(early.next().await.unwrap().unwrap().discontinuity, 0);

		drop((old_server, old_media, _old_track));
		until_finished(&rendition).await;
		let served = rendition
			.listed_segment(0)
			.await
			.unwrap()
			.expect("the retained old segment drains");
		assert!(contains(&served, OLD));
		assert!(rendition.snapshot().finished, "the playlist ends with its sibling");
		while let Some(segment) = early.next().await.unwrap() {
			assert!(contains(&segment.media, OLD));
		}

		// A publisher serving the path afterwards, with rows recorded for it, is never bound.
		let new_server = origin.dynamic_route("media", sibling_route(11)).unwrap();
		let _ = rendition.snapshot();
		assert!(
			tokio::time::timeout(Duration::from_secs(1), new_server.requested_broadcast())
				.await
				.is_err(),
			"the replacement is never requested"
		);
		let recorder = catalog.enroll_test("video0").unwrap();
		fanout.skip(None);
		let mut new_media = moq_net::broadcast::Info::new().produce();
		let _new_track = write_routed_media(&mut new_media, NEW, recorder, 6_000_000);
		tokio::time::sleep(Duration::from_secs(1)).await;
		let snapshot = rendition.snapshot();
		assert!(snapshot.segments.is_empty(), "nothing of the replacement is listed");
		assert!(snapshot.finished);
		watcher.abort();
		drop((new_media, _new_track, new_server, catalog, live));
	}

	// A restart can land while a segment request waits for its media broadcast. The init
	// decoded from the old run's groups must not be cached for the new run.
	#[tokio::test]
	async fn a_segment_straddling_a_restart_keeps_its_init_out_of_the_new_run() {
		tokio::time::pause();
		// A VP8 keyframe whose 320x240 geometry is only known inline.
		const KEYFRAME: &[u8] = &[0x10, 0x00, 0x00, 0x9d, 0x01, 0x2a, 0x40, 0x01, 0xf0, 0x00];

		let origin = produce_origin();
		let live = origin.create_broadcast("live").expect("publish allowed");
		live.announce(Default::default()).expect("publish allowed");
		let catalog =
			moq_mux::catalog::Producer::new(&mut live.broadcast().clone(), moq_mux::catalog::Config::default())
				.unwrap();
		let recorder = catalog.enroll_test("video0").unwrap();

		let mut media = moq_net::broadcast::Info::new().produce();
		let _track = write_routed_media(&mut media, KEYFRAME, recorder, 0);
		let server = origin.dynamic_route("media", sibling_route(10)).unwrap();
		settle().await;

		let upstream = crate::resolved(&origin, "live").await;
		let mut config = hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::VP8);
		config.broadcast = Some(moq_net::path::Relative::new("media").to_owned());

		let mut snapshot = moq_mux::catalog::hang::Catalog::default();
		snapshot.video.renditions.insert("video0".to_string(), config);
		let archive = archive(&["video0"]);
		snapshot.archive = Some(archive.clone());
		let renditions = renditions::Producer::new(Config::default().window);
		renditions.sync(&upstream, &snapshot);
		let fanout = renditions.fanout();
		let reference = Arc::new((Kind::Video, "video0".to_string(), "video0".to_string()));
		let watcher = tokio::spawn(watch_timeline(
			upstream.broadcast(),
			archive,
			reference,
			fanout.clone(),
			false,
		));
		let rendition = renditions.get(Kind::Video, "video0").unwrap();
		tokio::time::timeout(Duration::from_secs(5), rendition.playable())
			.await
			.expect("the timeline reaches the rendition");

		// The sibling request is unanswered, so the segment waits for its media broadcast.
		let mut segment = std::pin::pin!(rendition.listed_segment(0));
		assert!(
			tokio::time::timeout(Duration::from_millis(10), segment.as_mut())
				.await
				.is_err()
		);
		fanout.set_generation(Some("run-1".into()));
		fanout.set_generation(Some("run-2".into()));

		accept_sibling(&server, &media).await;
		segment
			.await
			.unwrap()
			.expect("the old run's rows were captured before the restart");
		assert!(
			rendition.init().await.unwrap().is_none(),
			"the new run has no media of its own yet"
		);
		watcher.abort();
	}

	// A replacement already present when the export starts is the sibling the request
	// resolves to: it is served, not treated as a stale epoch.
	#[tokio::test]
	async fn a_sibling_present_at_export_start_is_served() {
		const NEW: &[u8] = b"NEWNEWNEWNEWNEWN";

		let origin = produce_origin();
		let live = origin.create_broadcast("live").expect("publish allowed");
		live.announce(Default::default()).expect("publish allowed");
		let catalog =
			moq_mux::catalog::Producer::new(&mut live.broadcast().clone(), moq_mux::catalog::Config::default())
				.unwrap();
		let recorder = catalog.enroll_test("video0").unwrap();

		let mut media = moq_net::broadcast::Info::new().produce();
		let _track = write_routed_media(&mut media, NEW, recorder, 0);
		let server = origin.dynamic_route("media", sibling_route(11)).unwrap();
		settle().await;

		let upstream = crate::resolved(&origin, "live").await;
		let mut config = video_config();
		config.broadcast = Some(moq_net::path::Relative::new("media").to_owned());

		let (rendition, watcher) = export(&upstream, &config);
		accept_sibling(&server, &media).await;
		tokio::time::timeout(Duration::from_secs(5), rendition.playable())
			.await
			.expect("the timeline reaches the rendition");
		let served = rendition
			.listed_segment(0)
			.await
			.unwrap()
			.expect("the sibling present at export start is servable");
		assert!(contains(&served, NEW));
		watcher.abort();
		drop((media, _track, server, catalog, live));
	}

	// The replacement is often announced after the incumbent is already gone. Nothing serves
	// the path in between, but a sibling that resolved once is never looked up again, so the
	// rendition stays ended rather than recovering onto the replacement.
	#[tokio::test]
	async fn a_replaced_sibling_stays_ended_across_an_unroutable_gap() {
		tokio::time::pause();
		const OLD: &[u8] = b"OLDOLDOLDOLDOLDO";
		const NEW: &[u8] = b"NEWNEWNEWNEWNEWN";

		let origin = produce_origin();
		let live = origin.create_broadcast("live").expect("publish allowed");
		live.announce(Default::default()).expect("publish allowed");
		let catalog =
			moq_mux::catalog::Producer::new(&mut live.broadcast().clone(), moq_mux::catalog::Config::default())
				.unwrap();
		let recorder = catalog.enroll_test("video0").unwrap();

		let mut old_media = moq_net::broadcast::Info::new().produce();
		let _old_track = write_routed_media(&mut old_media, OLD, recorder, 0);
		let old_server = origin.dynamic_route("media", sibling_route(10)).unwrap();
		settle().await;

		let upstream = crate::resolved(&origin, "live").await;
		let mut config = video_config();
		config.broadcast = Some(moq_net::path::Relative::new("media").to_owned());

		let (rendition, watcher) = export(&upstream, &config);
		accept_sibling(&old_server, &old_media).await;
		tokio::time::timeout(Duration::from_secs(5), rendition.playable())
			.await
			.expect("the timeline reaches the rendition");
		assert!(rendition.listed_segment(0).await.unwrap().is_some());

		drop((old_server, old_media, _old_track));
		until_finished(&rendition).await;
		let before = rendition.snapshot().segments.len();
		assert!(before > 0);
		let served = rendition
			.listed_segment(0)
			.await
			.unwrap()
			.expect("the old retained segment drains");
		assert!(contains(&served, OLD));

		let new_server = origin.dynamic_route("media", sibling_route(11)).unwrap();
		let mut new_media = moq_net::broadcast::Info::new().produce();
		let recorder = catalog.enroll_test("video0").unwrap();
		let _new_track = write_routed_media(&mut new_media, NEW, recorder, 6_000_000);
		let _ = rendition.snapshot();
		assert!(
			tokio::time::timeout(Duration::from_secs(1), new_server.requested_broadcast())
				.await
				.is_err(),
			"the replacement is never requested"
		);
		let snapshot = rendition.snapshot();
		assert_eq!(snapshot.segments.len(), before, "only the old rows remain listed");
		assert!(snapshot.finished, "the playlist ended with its sibling");
		let served = rendition
			.listed_segment(0)
			.await
			.unwrap()
			.expect("only old media drains");
		assert!(contains(&served, OLD));
		assert!(!contains(&served, NEW));
		watcher.abort();
		drop((new_media, _new_track, new_server, catalog, live));
	}

	// A recorder waiting at the live edge ends when the bound sibling ends, with nothing else
	// (a playlist request, a timeline update) touching the rendition.
	#[tokio::test]
	async fn a_cursor_at_the_edge_ends_when_its_sibling_ends() {
		tokio::time::pause();
		const OLD: &[u8] = b"OLDOLDOLDOLDOLDO";

		let origin = produce_origin();
		let live = origin.create_broadcast("live").expect("publish allowed");
		live.announce(Default::default()).expect("publish allowed");
		let catalog =
			moq_mux::catalog::Producer::new(&mut live.broadcast().clone(), moq_mux::catalog::Config::default())
				.unwrap();
		let recorder = catalog.enroll_test("video0").unwrap();
		let mut media = moq_net::broadcast::Info::new().produce();
		let track = write_routed_media(&mut media, OLD, recorder, 0);
		let server = origin.dynamic_route("media", sibling_route(10)).unwrap();
		settle().await;

		let upstream = crate::resolved(&origin, "live").await;
		let mut config = video_config();
		config.broadcast = Some(moq_net::path::Relative::new("media").to_owned());
		let (rendition, watcher) = export(&upstream, &config);
		accept_sibling(&server, &media).await;
		tokio::time::timeout(Duration::from_secs(5), rendition.playable())
			.await
			.expect("the timeline reaches the rendition");

		let mut cursor = rendition.segments();
		loop {
			match tokio::time::timeout(Duration::from_secs(1), cursor.next()).await {
				Ok(Ok(Some(_))) => continue,
				Err(_) => break,
				other => panic!(
					"the cursor waits at the live edge: {:?}",
					other.map(|r| r.map(|s| s.is_some()))
				),
			}
		}

		// Only the sibling broadcast goes: its recorder stays, so the timeline stays quiet.
		drop((server, media));
		let end = tokio::time::timeout(Duration::from_secs(5), cursor.next())
			.await
			.expect("the cursor wakes when its sibling ends")
			.unwrap();
		assert!(end.is_none(), "the cursor ends with its sibling");
		watcher.abort();
		drop((track, catalog, live));
	}

	// A sibling not yet announced when the rendition binds it is waited for. The catalog's rows
	// are still listed, and the binding resolves on the sibling's first announcement.
	#[tokio::test]
	async fn a_sibling_announced_after_export_start_is_bound() {
		tokio::time::pause();
		const OLD: &[u8] = b"OLDOLDOLDOLDOLDO";

		let origin = produce_origin();
		let live = origin.create_broadcast("live").expect("publish allowed");
		live.announce(Default::default()).expect("publish allowed");
		let catalog =
			moq_mux::catalog::Producer::new(&mut live.broadcast().clone(), moq_mux::catalog::Config::default())
				.unwrap();
		let recorder = catalog.enroll_test("video0").unwrap();
		let mut media = moq_net::broadcast::Info::new().produce();
		let _track = write_routed_media(&mut media, OLD, recorder, 0);
		settle().await;

		let upstream = crate::resolved(&origin, "live").await;
		let mut config = video_config();
		config.broadcast = Some(moq_net::path::Relative::new("media").to_owned());

		let (rendition, watcher) = export(&upstream, &config);
		tokio::time::timeout(Duration::from_secs(5), rendition.playable())
			.await
			.expect("the catalog's rows are listed before the sibling resolves");

		let server = origin.dynamic_route("media", sibling_route(10)).unwrap();
		let _ = rendition.snapshot();
		accept_sibling(&server, &media).await;
		let served = rendition
			.listed_segment(0)
			.await
			.unwrap()
			.expect("the late sibling is servable");
		assert!(contains(&served, OLD));
		watcher.abort();
		drop((media, _track, server, catalog, live));
	}

	// A rendition the catalog drops must end its segment cursors. The cursor holds an
	// `Arc<Rendition>`, so without an explicit close the rendition (and its timeline
	// subscription) would stay alive and the cursor would park at the live edge forever.
	#[tokio::test]
	async fn removing_a_rendition_ends_its_segment_cursor() {
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").expect("publish allowed");
		broadcast.announce(Default::default()).expect("publish allowed");
		settle().await;
		let upstream = crate::resolved(&origin, "live").await;

		// Drive the producer directly so the catalog can be reconciled synchronously.
		let renditions = renditions::Producer::new(Config::default().window);
		let media = hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::VP8);
		let mut catalog = moq_mux::catalog::hang::Catalog::default();
		catalog.video.renditions.insert("video0".to_string(), media);
		catalog.archive = Some(archive(&["video0", "audio0"]));
		renditions.sync(&upstream, &catalog);

		let rendition = renditions.get(Kind::Video, "video0").expect("rendition synced");
		let mut segments = rendition.segments();

		// The catalog drops the rendition: its cursor must run dry rather than park.
		let mut empty = moq_mux::catalog::hang::Catalog::default();
		empty.archive = Some(archive(&["video0", "audio0"]));
		renditions.sync(&upstream, &empty);
		let ended = tokio::time::timeout(Duration::from_secs(5), segments.next())
			.await
			.expect("a removed rendition's cursor ends instead of parking")
			.unwrap();
		assert!(ended.is_none(), "no segments remained to drain");

		drop((broadcast, rendition));
	}

	// A rendition is hidden until its own timeline is advertised, then added beside its siblings.
	#[tokio::test]
	async fn a_rendition_appears_once_its_timeline_is_advertised() {
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").expect("publish allowed");
		broadcast.announce(Default::default()).expect("publish allowed");
		settle().await;
		let upstream = crate::resolved(&origin, "live").await;

		let renditions = renditions::Producer::new(Config::default().window);
		let mut cursor = renditions.subscribe();
		let mut catalog = moq_mux::catalog::hang::Catalog::default();
		catalog.video.renditions.insert("video0".to_string(), video_config());
		catalog.audio.renditions.insert(
			"audio0".to_string(),
			hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Opus, 48_000, 2),
		);
		catalog.archive = Some(archive(&["video0"]));
		renditions.sync(&upstream, &catalog);

		let video = renditions.get(Kind::Video, "video0").expect("video has a timeline");
		assert!(
			renditions.get(Kind::Audio, "audio0").is_none(),
			"audio has no timeline yet"
		);
		assert!(
			matches!(next_event(&mut cursor).await, Some(renditions::Event::Added(added)) if added.kind == Kind::Video)
		);
		assert!(next_event(&mut cursor).await.is_none(), "audio is not announced early");

		catalog.archive = Some(archive(&["video0", "audio0"]));
		renditions.sync(&upstream, &catalog);
		assert!(
			matches!(next_event(&mut cursor).await, Some(renditions::Event::Added(added)) if added.kind == Kind::Audio)
		);
		let same = renditions.get(Kind::Video, "video0").unwrap();
		assert!(Arc::ptr_eq(&video, &same), "the video sibling keeps its identity");
		assert!(renditions.get(Kind::Audio, "audio0").is_some());

		drop(broadcast);
	}

	// Dropping the broadcaster must release its renditions even when a cursor (and an
	// `Arc<Rendition>` it was handed) outlives it. The map lives in shared state, so without an
	// explicit teardown those would pin every rendition's timeline watcher -- and the standing
	// source subscription it holds -- for as long as the consumer lived.
	#[tokio::test]
	async fn dropping_the_broadcaster_releases_its_renditions() {
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").expect("publish allowed");
		broadcast.announce(Default::default()).expect("publish allowed");
		settle().await;
		let catalog =
			moq_mux::catalog::Producer::new(&mut broadcast.broadcast().clone(), moq_mux::catalog::Config::default())
				.unwrap();

		let reserved = catalog.reserve();
		let mut registration = catalog.test_video("video0").unwrap();
		let config = video_config();
		registration.set(config).unwrap();
		drop(reserved);

		let track = broadcast.create_track("video0", timed()).unwrap();
		let mut media = catalog
			.media_producer(
				track,
				moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Data),
			)
			.unwrap();
		media.write(frame(0, true)).unwrap();
		media.write(frame(2_000_000, true)).unwrap();

		let broadcaster = Broadcaster::new(crate::resolved(&origin, "live").await, Config::default());

		let mut renditions = broadcaster.renditions();
		let added = tokio::time::timeout(Duration::from_secs(5), renditions.next())
			.await
			.expect("a rendition is discovered");
		let Ok(Some(renditions::Event::Added(_rendition))) = added else {
			panic!("expected an Added event");
		};

		// The cursor and the handed-out rendition both outlive the broadcaster.
		drop(broadcaster);

		let next = tokio::time::timeout(Duration::from_secs(5), renditions.next())
			.await
			.expect("the cursor resolves after the broadcaster drops");
		assert!(
			matches!(next, Ok(Some(renditions::Event::Removed { .. }))),
			"dropping the broadcaster releases its renditions to a live cursor"
		);

		// ...and the cursor then reaches a terminal state rather than parking. Releasing the
		// renditions is only safe if consumers actually finish.
		let ended = tokio::time::timeout(Duration::from_secs(5), renditions.next())
			.await
			.expect("the cursor terminates after the broadcaster drops");
		assert!(matches!(ended, Err(crate::Error::IncompleteRecording)));

		drop((catalog, media, registration, broadcast));
	}

	// The record path: the renditions cursor yields the video rendition, its segment cursor
	// yields each finalized segment with its media, and losing the publisher mid-group fails
	// both cursors instead of completing a truncated recording.
	#[tokio::test]
	async fn record_cursors_yield_renditions_and_segments() {
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").expect("publish allowed");
		broadcast.announce(Default::default()).expect("publish allowed");
		settle().await;
		let catalog =
			moq_mux::catalog::Producer::new(&mut broadcast.broadcast().clone(), moq_mux::catalog::Config::default())
				.unwrap();

		let reserved = catalog.reserve();
		let mut registration = catalog.test_video("video0").unwrap();
		let config = video_config();
		registration.set(config).unwrap();
		drop(reserved);

		// Groups 0 and 1 are complete; group 2 is the live edge until the publisher drops.
		let track = broadcast.create_track("video0", timed()).unwrap();
		let mut media = catalog
			.media_producer(
				track,
				moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Data),
			)
			.unwrap();
		media.write(frame(0, true)).unwrap();
		media.write(frame(2_000_000, true)).unwrap();
		media.write(frame(4_000_000, true)).unwrap();

		let broadcaster = Broadcaster::new(crate::resolved(&origin, "live").await, Config::default());

		let mut renditions = broadcaster.renditions();
		let rendition = match tokio::time::timeout(Duration::from_secs(5), renditions.next())
			.await
			.expect("a rendition is discovered")
			.unwrap()
		{
			Some(renditions::Event::Added(rendition)) => rendition,
			other => panic!("expected an Added event, got {:?}", other.is_some()),
		};
		assert_eq!(rendition.kind, Kind::Video);
		assert_eq!(rendition.name, "video0");

		let mut segments = rendition.segments();
		assert!(segments.init().await.unwrap().is_some(), "init is buildable");

		let first = tokio::time::timeout(Duration::from_secs(5), segments.next())
			.await
			.expect("first segment finalizes")
			.unwrap()
			.expect("a segment, not end");
		assert_eq!(first.segment, 0);
		assert_eq!(&first.media[4..8], b"moof", "the segment carries its transmuxed media");
		assert_eq!(first.duration, Duration::from_secs(2));
		assert_eq!(first.discontinuity, 0, "a clean start is not a discontinuity");

		let second = segments.next().await.unwrap().expect("second segment");
		assert_eq!(second.segment, 1);
		assert_eq!(second.discontinuity, 0, "consecutive segments are continuous");

		// Tear down the publisher mid-group. The timeline ends abruptly, so the cursor fails
		// rather than finalize the open live-edge group (a reset can't vouch that its media is
		// complete) or end as if the recording were complete.
		drop((catalog, media, registration));
		broadcast.close();

		let end = tokio::time::timeout(Duration::from_secs(5), segments.next())
			.await
			.expect("segment cursor resolves after the source is lost");
		assert!(
			matches!(end, Err(crate::Error::TimelineFailed { .. })),
			"an abrupt end fails the recording: {:?}",
			end.map(|segment| segment.map(|segment| segment.segment))
		);

		// The renditions cursor also fails when the source closes abruptly.
		let ended = tokio::time::timeout(Duration::from_secs(5), renditions.next())
			.await
			.expect("renditions cursor resolves");
		assert!(ended.is_err(), "an abrupt broadcast close fails the renditions cursor");
	}

	#[tokio::test(start_paused = true)]
	async fn a_segment_cursor_rejects_an_unfinished_window() {
		let test = Timelines::new();
		let rendition = test.rendition("video0");
		let mut cursor = rendition.segments();
		rendition.close();
		assert!(matches!(cursor.next().await, Err(crate::Error::IncompleteRecording)));
		assert!(matches!(cursor.next().await, Err(crate::Error::IncompleteRecording)));
	}

	#[tokio::test(start_paused = true)]
	async fn a_failed_catalog_fails_unfinished_segment_windows() {
		let test = Timelines::new();
		let rendition = test.rendition("video0");
		let mut segments = rendition.segments();
		test.renditions.finish(Err(crate::Error::NoVariants));
		assert!(matches!(segments.next().await, Err(crate::Error::NoVariants)));
	}

	#[tokio::test(start_paused = true)]
	async fn a_slow_cursor_refuses_evicted_rows_before_its_first_read() {
		let test = Timelines::new();
		let rendition = test.rendition("video0");
		let mut slow = rendition.segments();
		let reference = reference(&test.catalog, None).unwrap();
		for sequence in 0..20 {
			test.renditions.fanout().push(
				sequence,
				moq_mux::timeline::Entry::decode(gop(sequence), moq_net::Timescale::MILLI).unwrap(),
				&reference,
			);
		}
		assert!(rendition.snapshot().media_sequence > 0);
		assert!(matches!(slow.next().await, Err(crate::Error::RecordingLagged)));
		assert!(matches!(slow.next().await, Err(crate::Error::RecordingLagged)));
	}

	#[tokio::test(start_paused = true)]
	async fn a_recording_refuses_missing_media_but_skips_rendition_gaps() {
		let test = Timelines::new();
		let (_, mut video0) = test.publish("video0");
		let (_, mut video1) = test.publish("video1");
		let media = test.media("video0", 2);
		let sibling = test.media("video1", 2);
		media.finish().unwrap();
		sibling.finish().unwrap();
		for sequence in 0..3 {
			video0.push(&gop(sequence)).unwrap();
		}
		// The sibling timeline explicitly has a gap for segment 0, then covers segment 1.
		video1
			.push(&hang::timeline::Record::new(
				0,
				2_000,
				2_000,
				hang::timeline::Position::group(1),
				hang::timeline::Position::group(2),
			))
			.unwrap();
		video1.finish().unwrap();
		video0.finish().unwrap();
		test.finished("video0").await;
		let rendition = test.rendition("video0");
		let mut cursor = rendition.segments();
		assert_eq!(cursor.next().await.unwrap().unwrap().segment, 0);
		assert_eq!(cursor.next().await.unwrap().unwrap().segment, 1);
		assert!(matches!(
			cursor.next().await,
			Err(crate::Error::SegmentUnavailable { segment: 2 })
		));
		assert!(matches!(
			cursor.next().await,
			Err(crate::Error::SegmentUnavailable { segment: 2 })
		));

		let rendition = test.rendition("video1");
		let mut cursor = rendition.segments();
		assert_eq!(cursor.next().await.unwrap().unwrap().segment, 1);
		assert!(cursor.next().await.unwrap().is_none());
	}

	#[tokio::test(start_paused = true)]
	async fn catalog_changes_report_removal_and_drain_old_cursors() {
		for reconfigure in [false, true] {
			let mut test = Timelines::new();
			let (_, mut timeline) = test.publish("video0");
			let _media = test.media("video0", 2);
			timeline.push(&gop(0)).unwrap();
			test.listed("video0", 0).await;
			let mut changes = test.renditions.subscribe();
			let mut old = None;
			for _ in 0..2 {
				if let Some(renditions::Event::Added(rendition)) = changes.next().await.unwrap()
					&& rendition.name == "video0"
				{
					old = Some(rendition.segments());
				}
			}
			let mut old = old.unwrap();
			if reconfigure {
				test.catalog.video.renditions.get_mut("video0").unwrap().coded_width = Some(1280);
			} else {
				test.catalog.video.renditions.remove("video0");
			}
			let source = moq_mux::Source::new(produce_origin().consume(), test.broadcast.consume());
			test.renditions.sync(&source, &test.catalog);
			assert!(
				matches!(changes.next().await.unwrap(), Some(renditions::Event::Removed { name, .. }) if name == "video0")
			);
			assert_eq!(old.next().await.unwrap().unwrap().segment, 0);
			assert!(old.next().await.unwrap().is_none());
			if reconfigure {
				assert!(
					matches!(changes.next().await.unwrap(), Some(renditions::Event::Added(rendition)) if rendition.name == "video0")
				);
			}
		}
	}

	#[tokio::test(start_paused = true)]
	async fn a_removed_rendition_keeps_draining_its_delayed_timeline() {
		let mut test = Timelines::new();
		let (_, mut reference) = test.publish("video0");
		let (_, mut sibling) = test.publish("video1");
		let _media = test.media("video1", 1);
		reference.push(&gop(0)).unwrap();
		test.listed("video0", 0).await;
		let rendition = test.rendition("video1");
		let mut cursor = rendition.segments();
		test.catalog.video.renditions.remove("video1");
		let source = moq_mux::Source::new(produce_origin().consume(), test.broadcast.consume());
		test.renditions.sync(&source, &test.catalog);
		// Removal closes the window, while its existing own-timeline watcher stays alive.
		sibling.push(&gop(0)).unwrap();
		sibling.finish().unwrap();
		assert_eq!(cursor.next().await.unwrap().unwrap().segment, 0);
		assert!(cursor.next().await.unwrap().is_none());
	}

	/// A two-second GOP record: record `n` covers group `n` from `2n` seconds.
	fn gop(sequence: u64) -> hang::timeline::Record {
		hang::timeline::Record::new(
			sequence,
			sequence * 2_000,
			2_000,
			hang::timeline::Position::group(sequence),
			hang::timeline::Position::group(sequence + 1),
		)
	}

	/// Renditions over hand-published timelines, reconciled and watched the way `watch_catalog`
	/// does. A segment fetch misses unless [`media`](Timelines::media) published its groups.
	struct Timelines {
		broadcast: moq_net::broadcast::Producer,
		catalog: moq_mux::catalog::hang::Catalog,
		renditions: renditions::Producer,
		watcher: tokio::task::JoinHandle<()>,
	}

	impl Timelines {
		/// A `video0` reference and a `video1` rendition.
		fn new() -> Self {
			Self::with(&["video0", "video1"], &[])
		}

		/// `video` and `audio` renditions, each indexing its own timeline; the first video one is
		/// the reference.
		fn with(video: &[&str], audio: &[&str]) -> Self {
			let mut catalog = moq_mux::catalog::hang::Catalog::default();
			for name in video {
				catalog.video.renditions.insert(name.to_string(), video_config());
			}
			for name in audio {
				let config = hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Opus, 48_000, 2);
				catalog.audio.renditions.insert(name.to_string(), config);
			}
			let tracks: Vec<&str> = video.iter().chain(audio).copied().collect();
			catalog.archive = Some(archive(&tracks));
			let broadcast = moq_net::broadcast::Info::new().produce();
			let (renditions, watcher) = Self::export(&broadcast, &catalog);
			Self {
				broadcast,
				catalog,
				renditions,
				watcher,
			}
		}

		fn export(
			broadcast: &moq_net::broadcast::Producer,
			catalog: &moq_mux::catalog::hang::Catalog,
		) -> (renditions::Producer, tokio::task::JoinHandle<()>) {
			let upstream = moq_mux::Source::new(produce_origin().consume(), broadcast.consume());
			let renditions = renditions::Producer::new(Config::default().window);
			renditions.sync(&upstream, catalog);
			let watcher = tokio::spawn(watch_timeline(
				upstream.broadcast(),
				catalog.archive.clone().unwrap(),
				reference(catalog, None).unwrap(),
				renditions.fanout(),
				false,
			));
			(renditions, watcher)
		}

		/// Another exporter over the same timelines: a second edge, or one a fresh viewer starts.
		fn join(&self) -> Self {
			let (renditions, watcher) = Self::export(&self.broadcast, &self.catalog);
			Self {
				broadcast: self.broadcast.clone(),
				catalog: self.catalog.clone(),
				renditions,
				watcher,
			}
		}

		/// Publish `track`'s timeline. Returns the track, to fail it, with the timeline writing to it.
		fn publish(&self, track: &str) -> (moq_net::track::Producer, moq_mux::timeline::Producer) {
			let track = self
				.broadcast
				.create_track(hang::timeline::default_name(track), moq_mux::timeline::Producer::info())
				.unwrap();
			let timeline = moq_mux::timeline::Producer::new(track.clone());
			(track, timeline)
		}

		/// Publish `name`'s media: one keyframe group per [`gop`] record `0..groups`, so a cursor
		/// fetches real segments.
		fn media(&self, name: &str, groups: u64) -> moq_net::track::Producer {
			let track = self.broadcast.create_track(name, timed()).unwrap();
			for sequence in 0..groups {
				let mut group = track.append_group().unwrap();
				let frame = vp8_frame(sequence * 2_000_000, true);
				hang::container::Frame {
					timestamp: frame.timestamp,
					payload: frame.payload,
				}
				.write_to(&mut group)
				.unwrap();
				group.finish().unwrap();
			}
			track
		}

		fn rendition(&self, name: &str) -> Arc<Rendition> {
			self.renditions
				.get(Kind::Video, name)
				.or_else(|| self.renditions.get(Kind::Audio, name))
				.unwrap()
		}

		/// Wait until `name` lists segment `last`.
		async fn listed(&self, name: &str, last: u64) -> playlist::Snapshot {
			for _ in 0..500 {
				let snapshot = self.rendition(name).snapshot();
				if snapshot.segments.last().is_some_and(|s| s.segment >= last) {
					return snapshot;
				}
				tokio::time::sleep(Duration::from_millis(10)).await;
			}
			panic!("{name} never listed segment {last}");
		}

		/// Wait until `name`'s playlist ends.
		async fn finished(&self, name: &str) -> playlist::Snapshot {
			for _ in 0..500 {
				let snapshot = self.rendition(name).snapshot();
				if snapshot.finished {
					return snapshot;
				}
				tokio::time::sleep(Duration::from_millis(10)).await;
			}
			panic!("{name} never ended");
		}

		/// Wait until `name`'s playlist fails, returning the error it answers.
		async fn failed(&self, name: &str) -> crate::Error {
			failed(&self.rendition(name)).await
		}

		/// Drain `name`'s recording cursor, which must end rather than park, returning the
		/// segments it yielded and how it ended.
		async fn drain(&self, name: &str) -> (Vec<u64>, crate::Result<()>) {
			let mut segments = self.rendition(name).segments();
			let mut drained = Vec::new();
			let end = tokio::time::timeout(Duration::from_secs(5), async {
				loop {
					match segments.next().await {
						Ok(Some(segment)) => drained.push(segment.segment),
						Ok(None) => return Ok(()),
						Err(err) => return Err(err),
					}
				}
			})
			.await
			.unwrap_or_else(|_| panic!("{name}'s cursor parked"));
			if end.is_err() {
				assert!(segments.next().await.is_err(), "{name}'s cursor ended after failing");
			}
			(drained, end)
		}

		/// Drain `name`'s recording cursor, which must answer a timeline failure once drained,
		/// returning the segments it yielded first.
		async fn drain_failed(&self, name: &str) -> Vec<u64> {
			let (drained, end) = self.drain(name).await;
			assert!(matches!(end, Err(crate::Error::TimelineFailed { .. })), "{end:?}");
			drained
		}
	}

	impl Drop for Timelines {
		fn drop(&mut self) {
			self.watcher.abort();
		}
	}

	/// The timeline's publisher loses its session: the track and its open group fail.
	fn cut(track: moq_net::track::Producer, timeline: moq_mux::timeline::Producer) {
		drop(timeline);
		track
			.abort(moq_net::Error::Transport("connection lost".into()))
			.unwrap();
	}

	/// Wait until `rendition`'s playlist fails, returning the error it answers.
	async fn failed(rendition: &Rendition) -> crate::Error {
		for _ in 0..500 {
			if let Err(err) = rendition.try_snapshot() {
				assert!(matches!(err, crate::Error::TimelineFailed { .. }), "{err}");
				return err;
			}
			tokio::time::sleep(Duration::from_millis(10)).await;
		}
		panic!("{} never failed", rendition.name);
	}

	fn numbers(snapshot: &playlist::Snapshot) -> Vec<u64> {
		snapshot.segments.iter().map(|s| s.segment).collect()
	}

	// A failed rendition timeline is not retried, and never papered over with gaps or ended as if
	// complete: its listings answer the error, while the other renditions go on.
	#[tokio::test(start_paused = true)]
	async fn a_rendition_timeline_error_fails_only_its_playlist() {
		let test = Timelines::new();
		let (_reference, mut video0) = test.publish("video0");
		let (track, mut video1) = test.publish("video1");
		let _media = test.media("video1", 4);
		for sequence in 0..4 {
			video0.push(&gop(sequence)).unwrap();
			video1.push(&gop(sequence)).unwrap();
		}
		// A segment resolves once the rendition's timeline is a snap tolerance past its end.
		assert_eq!(numbers(&test.listed("video1", 2).await), [0, 1, 2]);

		cut(track, video1);
		for sequence in 4..7 {
			video0.push(&gop(sequence)).unwrap();
		}
		test.failed("video1").await;
		let video1 = test.rendition("video1");
		assert!(video1.media_playlist(None).is_err());
		assert!(video1.representations().is_err());
		// A range the failed timeline covers still answers the failure, not a refused range.
		let range = Range::new(Duration::ZERO, Duration::from_secs(4)).unwrap();
		assert!(matches!(
			video1.snapshot_range(range, None),
			Err(crate::Error::TimelineFailed { .. })
		));
		// Segments already listed stay fetchable while the listing refuses.
		assert_eq!(
			video1.listed_content(0),
			Some(spans::Content::Frames {
				ranges: vec![hang::timeline::Position::group(0)..hang::timeline::Position::group(1)],
				filter: None,
			})
		);
		// So does the cursor, once it drains what the timeline covers.
		assert_eq!(test.drain_failed("video1").await, [0, 1, 2, 3]);

		let reference = test.listed("video0", 6).await;
		assert!(!reference.finished, "the other renditions go on");
		assert!(test.rendition("video0").representations().is_ok());
	}

	// A rendition that failed while the reference was live stays failed once the reference
	// finishes, even when its records cover every listed row. One failing only after the
	// reference finished loses nothing it covers, so its playlist and cursor still end as complete.
	#[tokio::test(start_paused = true)]
	async fn a_rendition_failure_counts_only_before_the_reference_finishes() {
		for early in [true, false] {
			let test = Timelines::new();
			let (_reference, mut video0) = test.publish("video0");
			let (track, mut video1) = test.publish("video1");
			let _media = test.media("video1", 6);
			for sequence in 0..4 {
				video0.push(&gop(sequence)).unwrap();
			}
			// video1 runs past the last listed row, so its records cover every one.
			for sequence in 0..6 {
				video1.push(&gop(sequence)).unwrap();
			}
			test.listed("video1", 3).await;

			let mut publisher = Some((track, video1));
			if early {
				let (track, video1) = publisher.take().unwrap();
				cut(track, video1);
				test.failed("video1").await;
			}
			video0.finish().unwrap();
			test.finished("video0").await;
			let rendition = test.rendition("video1");
			if let Some((track, video1)) = publisher {
				test.finished("video1").await;
				cut(track, video1);
				while rendition.own_failure().is_none() {
					tokio::time::sleep(Duration::from_millis(10)).await;
				}
				let snapshot = rendition.snapshot();
				assert!(snapshot.finished, "a covered late failure loses nothing");
				assert_eq!(numbers(&snapshot), [0, 1, 2, 3]);
				let (drained, end) = test.drain("video1").await;
				assert_eq!(drained, [0, 1, 2, 3]);
				assert!(end.is_ok(), "the recording is complete: {end:?}");
			} else {
				settle().await;
				assert!(rendition.try_snapshot().is_err(), "the failure is sticky");
				assert!(rendition.failed_alone());
				assert_eq!(test.drain_failed("video1").await, [0, 1, 2, 3]);
			}
		}
	}

	// A failed reference timeline is not retried. Every playlist answers the error instead of
	// ending as if complete or freezing live, and recording cursors answer it once drained.
	#[tokio::test(start_paused = true)]
	async fn a_reference_timeline_error_fails_every_playlist() {
		let test = Timelines::new();
		let (track, mut video0) = test.publish("video0");
		let (_track, mut video1) = test.publish("video1");
		let _media = [test.media("video0", 4), test.media("video1", 5)];
		for sequence in 0..4 {
			video0.push(&gop(sequence)).unwrap();
			video1.push(&gop(sequence)).unwrap();
		}
		test.listed("video0", 3).await;

		cut(track, video0);
		for name in ["video0", "video1"] {
			test.failed(name).await;
			assert!(test.rendition(name).representations().is_err(), "{name}");
		}
		assert!(test.renditions.failure().is_some());
		// A rendition created after the failure starts failed.
		let late = test.join();
		late.failed("video1").await;
		assert_eq!(test.drain_failed("video0").await, [0, 1, 2, 3]);
		// The cursor still waits for the listed segment its own timeline has not passed.
		video1.push(&gop(4)).unwrap();
		assert_eq!(test.drain_failed("video1").await, [0, 1, 2, 3]);
	}

	// A playlist request waiting for its first row answers a timeline that fails before one,
	// rather than timing out as not found or, without a router's timeout, waiting forever.
	#[tokio::test(start_paused = true)]
	async fn a_pending_playlist_answers_a_rendition_timeline_failure() {
		let test = Timelines::new();
		let (_reference, _video0) = test.publish("video0");
		let (track, video1) = test.publish("video1");
		let video1_rendition = test.rendition("video1");
		let pending = tokio::spawn(async move { video1_rendition.playlist(None, None).await });
		tokio::time::sleep(Duration::from_secs(1)).await;
		assert!(!pending.is_finished(), "nothing is listed yet");

		cut(track, video1);
		let answer = tokio::time::timeout(Duration::from_secs(5), pending).await;
		let err = answer.expect("the failure wakes the request").unwrap().unwrap_err();
		assert!(matches!(err, crate::Error::TimelineFailed { .. }), "{err}");
	}

	// The same for the reference timeline, which every playlist lists.
	#[tokio::test(start_paused = true)]
	async fn a_pending_playlist_answers_a_reference_timeline_failure() {
		let test = Timelines::new();
		let (track, video0) = test.publish("video0");
		let (_track, _video1) = test.publish("video1");
		let video1 = test.rendition("video1");
		let pending = tokio::spawn(async move { video1.playlist(None, None).await });
		tokio::time::sleep(Duration::from_secs(1)).await;
		assert!(!pending.is_finished(), "nothing is listed yet");

		cut(track, video0);
		let answer = tokio::time::timeout(Duration::from_secs(5), pending).await;
		let err = answer.expect("the failure wakes the request").unwrap().unwrap_err();
		assert!(matches!(err, crate::Error::TimelineFailed { .. }), "{err}");
	}

	// A range over a recording that ended without media is refused rather than reported missing.
	#[tokio::test(start_paused = true)]
	async fn a_range_over_an_empty_ended_recording_is_refused() {
		let test = Timelines::new();
		let (track, video0) = test.publish("video0");
		let (_track, _video1) = test.publish("video1");
		drop(video0);
		track.finish().unwrap();
		assert!(test.finished("video1").await.segments.is_empty());

		let range = Range::new(Duration::ZERO, Duration::from_secs(2)).unwrap();
		assert!(matches!(
			test.rendition("video1").snapshot_range(range, None),
			Err(crate::Error::RangeUnavailable(_))
		));
	}

	// A rendition running far ahead of a stalled reference keeps the records its listed segments
	// resolve to, so a listed segment never turns into a gap.
	#[tokio::test(start_paused = true)]
	async fn a_rendition_ahead_of_the_reference_keeps_listed_segments() {
		let test = Timelines::new();
		let (_track0, mut video0) = test.publish("video0");
		let (_track1, mut video1) = test.publish("video1");
		// The reference stalls at 20s while video1 runs on to 80s, far past the 16s window.
		for sequence in 0..10 {
			video0.push(&gop(sequence)).unwrap();
		}
		for sequence in 0..40 {
			video1.push(&gop(sequence)).unwrap();
		}
		let listed = test.listed("video1", 9).await;
		assert!(!listed.segments.is_empty());
		assert!(
			listed.segments.iter().all(|s| !s.gap),
			"every listed segment still resolves: {:?}",
			numbers(&listed)
		);
	}

	// RFC 8216 §6.2.2: EXT-X-MEDIA-SEQUENCE must not decrease across reloads. A rendition that
	// sorts ahead of the reference must not take over, or the playlist rewinds to that
	// rendition's own record numbers and its URLs name other content.
	#[tokio::test(start_paused = true)]
	async fn an_earlier_rendition_does_not_rewind_the_media_sequence() {
		const OLD: &[u8] = b"OLDOLDOLDOLDOLDO";
		const NEW: &[u8] = b"NEWNEWNEWNEWNEWN";

		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").expect("publish allowed");
		broadcast.announce(Default::default()).expect("publish allowed");
		settle().await;
		let catalog =
			moq_mux::catalog::Producer::new(&mut broadcast.broadcast().clone(), moq_mux::catalog::Config::default())
				.unwrap();

		let reserved = catalog.reserve();
		let mut video0 = catalog.test_video("video0").unwrap();
		video0.set(video_config()).unwrap();
		drop(reserved);

		let track = broadcast.create_track("video0", timed()).unwrap();
		let mut media = catalog
			.media_producer(
				track,
				moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Data),
			)
			.unwrap();
		// 2s GOPs against a 6s window, so the oldest records leave and the sequence is no longer 0.
		for micros in (0..=10_000_000u64).step_by(2_000_000) {
			media.write(payload_frame(micros, true, OLD)).unwrap();
		}

		let config = Config {
			window: Duration::from_secs(6),
			..Config::default()
		};
		let broadcaster = Broadcaster::new(crate::resolved(&origin, "live").await, config);
		let sequence_of = |playlist: &str| -> u64 {
			between(playlist, "#EXT-X-MEDIA-SEQUENCE:", "\n")
				.parse()
				.expect("media sequence")
		};
		let rendition = tokio::time::timeout(Duration::from_secs(5), async {
			loop {
				if let Some(rendition) = broadcaster.rendition(Kind::Video, "video0")
					&& let Some(playlist) = rendition.media_playlist(None).unwrap()
					&& sequence_of(&playlist) >= 2
				{
					break rendition;
				}
				tokio::task::yield_now().await;
			}
		})
		.await
		.expect("video0 playlist slid past its first segments");

		let before = rendition.media_playlist(None).unwrap().expect("playable");
		let sequence = sequence_of(&before);
		assert!(sequence >= 2, "the window should have slid, got {sequence}\n{before}");
		let tag = segments::tag(&(Kind::Video, "video0".to_string(), "video0".to_string()));
		assert!(before.contains(&format!("seg/{tag}.{sequence}.m4s\n")), "{before}");
		let bytes = rendition
			.listed_segment(sequence)
			.await
			.unwrap()
			.expect("listed segment");
		assert!(contains(&bytes, OLD), "the listed segment is not video0's media");

		// Enroll the earlier timeline before the rendition, so the catalog snapshot that adds
		// the rendition already indexes it. The reference decision runs in that same update.
		let earlier = broadcast.create_track("a", timed()).unwrap();
		let mut earlier_media = catalog
			.media_producer(
				earlier,
				moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Data),
			)
			.unwrap();
		for micros in (0..=4_000_000u64).step_by(2_000_000) {
			earlier_media.write(payload_frame(micros, true, NEW)).unwrap();
		}
		let mut added = catalog.test_video("a").unwrap();
		added.set(video_config()).unwrap();

		tokio::time::timeout(Duration::from_secs(5), async {
			loop {
				if broadcaster.rendition(Kind::Video, "a").is_some() {
					break;
				}
				tokio::task::yield_now().await;
			}
		})
		.await
		.expect("the earlier rendition is in the catalog");

		let new_tag = segments::tag(&(Kind::Video, "a".to_string(), "a".to_string()));
		let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
		let mut previous = sequence;
		let mut reloads = 0;
		while reloads < 5 && tokio::time::Instant::now() < deadline {
			let Some(playlist) = rendition.media_playlist(None).unwrap() else {
				tokio::time::sleep(Duration::from_millis(10)).await;
				continue;
			};
			let next = sequence_of(&playlist);
			assert!(
				next >= previous,
				"EXT-X-MEDIA-SEQUENCE decreased from {previous} to {next}\n{playlist}"
			);
			assert!(
				playlist.contains(&format!("seg/{tag}.")),
				"segment URLs left the original reference\n{playlist}"
			);
			assert!(
				!playlist.contains(&format!("seg/{new_tag}.")),
				"the earlier rendition's numbering replaced the reference\n{playlist}"
			);
			previous = next;
			reloads += 1;
			tokio::time::sleep(Duration::from_millis(10)).await;
		}
		assert!(reloads >= 5, "the playlist did not stay reloadable");

		let again = rendition
			.listed_segment(sequence)
			.await
			.unwrap()
			.expect("the original segment URL still resolves");
		assert_eq!(again, bytes, "the original URL names other content");
		assert!(
			!contains(&again, NEW),
			"the earlier rendition's media replaced the segment"
		);

		drop((media, earlier_media, video0, added, broadcast));
	}

	#[tokio::test]
	#[ignore = "performance measurement; run by the nightly Rust benchmarks"]
	async fn benchmark_reference_handoff_fanout() {
		for count in [2, 8, 32] {
			for retained in [16, 64, 256] {
				let names: Vec<_> = (0..count).map(|index| format!("video{index:02}")).collect();
				let mut catalog = moq_mux::catalog::hang::Catalog::default();
				for name in &names {
					catalog.video.renditions.insert(name.clone(), video_config());
				}
				catalog.archive = Some(archive(&names.iter().map(String::as_str).collect::<Vec<_>>()));
				let broadcast = moq_net::broadcast::Info::new().produce();
				let source = moq_mux::Source::new(produce_origin().consume(), broadcast.consume());
				let renditions = renditions::Producer::new(Duration::from_secs(retained * 2));
				renditions.sync(&source, &catalog);
				let fanout = renditions.fanout();
				let reference = |index: usize| Arc::new((Kind::Video, names[index].clone(), names[index].clone()));
				let refs = [reference(0), reference(1)];
				let entry = |sequence, at| {
					let mut record = gop(sequence);
					record.pts = Some(at * 2_000);
					moq_mux::timeline::Entry::decode(record, moq_net::Timescale::MILLI).unwrap()
				};
				for at in 0..retained {
					fanout.push(at, entry(at, at), &refs[0]);
				}
				// Aligned references share record numbers, so every handoff continues seamlessly.
				let started = moq_net::time::Instant::now();
				for at in retained..retained + 1_024 {
					let reference = &refs[(at % 2) as usize];
					fanout.select(Some(reference.clone()));
					fanout.push(at, entry(at, at), reference);
				}
				let elapsed = started.elapsed();
				for rendition in renditions.snapshot() {
					assert!(rendition.is_playable());
				}
				println!(
					"hls handoff renditions={count} retained={retained} ns_per_push={}",
					elapsed.as_nanos() / 1_024
				);
			}
		}
	}

	// A video reference sticks while it stays listed with a timeline, and falls back to the
	// first choice once it leaves. An audio reference yields once video has a timeline.
	#[test]
	fn the_reference_sticks_until_it_leaves() {
		let catalog = |videos: &[&str], audios: &[&str]| {
			let mut catalog = moq_mux::catalog::hang::Catalog::default();
			for name in videos {
				catalog.video.renditions.insert(name.to_string(), video_config());
			}
			for name in audios {
				catalog.audio.renditions.insert(
					name.to_string(),
					hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Opus, 48_000, 2),
				);
			}
			catalog.archive = Some(archive(&[videos, audios].concat()));
			catalog
		};
		let video = |name: &str| Arc::new((Kind::Video, name.to_string(), name.to_string()));
		let audio = |name: &str| Arc::new((Kind::Audio, name.to_string(), name.to_string()));

		let first = reference(&catalog(&["b"], &[]), None);
		assert_eq!(first, Some(video("b")));
		assert_eq!(reference(&catalog(&["a", "b"], &[]), first.as_ref()), Some(video("b")));
		assert_eq!(reference(&catalog(&["a", "c"], &[]), first.as_ref()), Some(video("a")));

		let first = reference(&catalog(&[], &["b"]), None);
		assert_eq!(first, Some(audio("b")));
		assert_eq!(reference(&catalog(&[], &["a", "b"]), first.as_ref()), Some(audio("b")));
		// Video listed before its timeline exists doesn't take over yet.
		let mut pending = catalog(&[], &["b"]);
		pending.video.renditions.insert("v".to_string(), video_config());
		assert_eq!(reference(&pending, first.as_ref()), Some(audio("b")));
		assert_eq!(
			reference(&catalog(&["v"], &["a", "b"]), first.as_ref()),
			Some(video("v"))
		);
	}

	#[test]
	fn disabled_references_yield_only_to_enabled_indexed_renditions() {
		let mut catalog = moq_mux::catalog::hang::Catalog::default();
		let mut disabled = video_config();
		disabled.enabled = false;
		catalog.video.renditions.insert("a".into(), disabled);
		catalog.video.renditions.insert("b".into(), video_config());
		catalog.archive = Some(archive(&["a", "b", "audio"]));
		let previous = Arc::new((Kind::Video, "a".into(), "a".into()));
		assert_eq!(reference(&catalog, Some(&previous)).unwrap().1, "b");
		catalog.video.renditions.get_mut("b").unwrap().enabled = false;
		assert!(reference(&catalog, Some(&previous)).is_none());
		catalog.audio.renditions.insert(
			"audio".into(),
			hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Opus, 48_000, 2),
		);
		assert_eq!(reference(&catalog, Some(&previous)).unwrap().0, Kind::Audio);
		catalog.audio.renditions.get_mut("audio").unwrap().enabled = false;
		assert!(reference(&catalog, None).is_none());
	}

	// Timelines are keyed by the indexed track's name, so a rendition whose ID differs from its
	// track is found, and kept, by its track. Moving the ID to another track is a new reference.
	#[test]
	fn the_reference_timeline_is_found_by_track_name() {
		let catalog = |track: &str| {
			let mut catalog = moq_mux::catalog::hang::Catalog::default();
			let mut config = video_config();
			config.track = Some(track.to_string());
			catalog.video.renditions.insert("main".to_string(), config);
			catalog.archive = Some(archive(&["video-a", "video-b"]));
			catalog
		};
		let main = |track: &str| Arc::new((Kind::Video, "main".to_string(), track.to_string()));

		let first = reference(&catalog("video-a"), None);
		assert_eq!(first, Some(main("video-a")));
		assert_eq!(reference(&catalog("video-a"), first.as_ref()), first);
		let moved = reference(&catalog("video-b"), first.as_ref());
		assert_eq!(moved, Some(main("video-b")));
		assert_ne!(segments::tag(&main("video-a")), segments::tag(&main("video-b")));
		assert_eq!(reference(&catalog("main"), first.as_ref()), None);
	}

	// A new reference numbers segments from its own records, so its URLs carry its own tag and
	// never reuse one the old reference listed for other content.
	#[tokio::test(start_paused = true)]
	async fn a_reference_switch_never_reuses_a_segment_url() {
		let mut test = Timelines::new();
		let (_track0, mut video0) = test.publish("video0");
		let (_track1, mut video1) = test.publish("video1");
		for sequence in 0..4 {
			video0.push(&gop(sequence)).unwrap();
		}
		// video1's records are numbered from 0 but start 3s later, so its segment 0 is other content.
		for sequence in 0..4 {
			video1
				.push(&hang::timeline::Record::new(
					sequence,
					sequence * 2_000 + 3_000,
					2_000,
					hang::timeline::Position::group(sequence),
					hang::timeline::Position::group(sequence + 1),
				))
				.unwrap();
		}
		let before = test.listed("video0", 2).await;
		let old = segments::tag(&(Kind::Video, "video0".to_string(), "video0".to_string()));
		assert!(before.segments.iter().all(|s| s.tag == old));

		// Switch the reference the way `watch_catalog` does.
		test.watcher.abort();
		test.renditions.fanout().skip(None);
		test.watcher = tokio::spawn(watch_timeline(
			test.broadcast.consume(),
			archive(&["video0", "video1"]),
			Arc::new((Kind::Video, "video1".to_string(), "video1".to_string())),
			test.renditions.fanout(),
			false,
		));
		let after = test.listed("video1", 2).await;
		let new = segments::tag(&(Kind::Video, "video1".to_string(), "video1".to_string()));
		assert_eq!(numbers(&after)[0], 0, "the new reference numbers from its own records");
		assert!(after.segments.iter().all(|s| s.tag == new && s.tag != old));
		let video0 = test.rendition("video0");
		assert!(
			video0.segment(&old, 0).await.unwrap().is_none(),
			"the old URL is no longer served"
		);
	}

	// Moving the reference rendition to another track under the same ID is a reference switch too:
	// a rendition that stays lists the new track's records under a new tag, from their first.
	#[tokio::test(start_paused = true)]
	async fn a_retargeted_reference_never_reuses_a_segment_url() {
		let mut test = Timelines::with(&["main"], &["audio0"]);
		let (_track0, mut video_a) = test.publish("main");
		let (_track1, mut video_b) = test.publish("video-b");
		let (_track2, mut audio) = test.publish("audio0");
		for sequence in 0..8 {
			audio.push(&gop(sequence)).unwrap();
		}
		for sequence in 0..4 {
			video_a.push(&gop(sequence)).unwrap();
			video_b
				.push(&hang::timeline::Record::new(
					sequence,
					sequence * 2_000 + 3_000,
					2_000,
					hang::timeline::Position::group(sequence),
					hang::timeline::Position::group(sequence + 1),
				))
				.unwrap();
		}
		let before = test.listed("audio0", 2).await;
		let old = segments::tag(&(Kind::Video, "main".to_string(), "main".to_string()));
		assert!(before.segments.iter().all(|s| s.tag == old));

		test.watcher.abort();
		test.renditions.fanout().skip(None);
		test.watcher = tokio::spawn(watch_timeline(
			test.broadcast.consume(),
			archive(&["main", "video-b", "audio0"]),
			Arc::new((Kind::Video, "main".to_string(), "video-b".to_string())),
			test.renditions.fanout(),
			false,
		));
		let after = test.listed("audio0", 2).await;
		let new = segments::tag(&(Kind::Video, "main".to_string(), "video-b".to_string()));
		assert_ne!(new, old);
		assert_eq!(numbers(&after)[0], 0, "the new track numbers from its own records");
		assert!(after.segments.iter().all(|s| s.tag == new));
	}

	// A malformed reference timeline fails the same way.
	#[tokio::test(start_paused = true)]
	async fn a_malformed_reference_timeline_fails_every_playlist() {
		let test = Timelines::new();
		let (track, mut video0) = test.publish("video0");
		video0.push(&gop(0)).unwrap();
		test.listed("video0", 0).await;

		let mut group = track.append_group().unwrap();
		group.write_frame(moq_net::Timestamp::now(), &b"garbage"[..]).unwrap();
		test.failed("video0").await;
		test.failed("video1").await;
	}

	// A timeline subscription still waiting on its track fails with the broadcast, so nothing
	// parks, and a timeline never read is not listed as an empty finished recording.
	#[tokio::test(start_paused = true)]
	async fn a_pending_timeline_subscription_ends_with_the_broadcast() {
		let test = Timelines::new();
		// A handler keeps the unpublished timelines' requests waiting rather than refused.
		let _handler = test.broadcast.dynamic();
		tokio::time::sleep(Duration::from_secs(60)).await;
		assert!(!test.rendition("video0").snapshot().finished);

		test.broadcast.close();
		test.failed("video0").await;
		assert!(test.drain_failed("video0").await.is_empty());
	}

	/// Record `sequence` covering group `group` from `pts_ms` for `duration_ms`, starting on a sync
	/// point.
	fn span(sequence: u64, pts_ms: u64, duration_ms: u64, group: u64) -> hang::timeline::Record {
		hang::timeline::Record::new(
			sequence,
			pts_ms,
			duration_ms,
			hang::timeline::Position::group(group),
			hang::timeline::Position::group(group + 1),
		)
	}

	/// The renditions a master playlist would list, by name.
	fn advertised(renditions: &renditions::Producer) -> Vec<String> {
		renditions
			.snapshot()
			.iter()
			.filter(|rendition| rendition.is_advertised())
			.map(|rendition| rendition.name.clone())
			.collect()
	}

	/// The `Push` events a fresh subscriber to `track`'s timeline reads before it is caught up.
	async fn fresh_join_reads(test: &Timelines, track: &str) -> usize {
		let section = test.catalog.archive.clone().unwrap();
		let mut timeline = moq_mux::timeline::Consumer::<()>::subscribe(&test.broadcast.consume(), &section, track)
			.await
			.unwrap();
		let mut pushes = 0;
		while let std::task::Poll::Ready(event) = timeline.poll_next(&kio::Waiter::noop()) {
			let event = event.unwrap().expect("the timeline is still live");
			pushes += matches!(event, moq_mux::timeline::Event::Push { .. }) as usize;
		}
		pushes
	}

	// A viewer joining a days-old live broadcast costs what the window lists, not the broadcast's
	// age: its timeline subscription restates a bounded suffix of a timeline that never pops, and
	// its playlist lists the window with the sequence every other edge shows.
	#[tokio::test(start_paused = true)]
	async fn a_fresh_join_reads_a_bounded_number_of_records() {
		let early = Timelines::with(&["video0"], &[]);
		let (_track, mut video0) = early.publish("video0");
		// A day of 10s GOPs per round.
		const DAY: u64 = 86_400 / 10;
		// A checkpoint, plus the edits one timeline group holds before a join over its ratio rolls it.
		let ratio = moq_mux::timeline::Producer::info().refresh.unwrap() as usize;
		let bound = segments::MAX_SEGMENTS * (1 + ratio);
		for day in 1..=3 {
			for sequence in (day - 1) * DAY..day * DAY {
				video0
					.push(&span(sequence, sequence * 10_000, 10_000, sequence))
					.unwrap();
			}
			let read = fresh_join_reads(&early, "video0").await;
			assert!(
				(segments::MAX_SEGMENTS..=bound).contains(&read),
				"day {day}: a fresh join read {read} records"
			);
		}

		let last = 3 * DAY - 1;
		let late = early.join();
		let joined = late.listed("video0", last).await;
		let followed = early.listed("video0", last).await;
		// Two 10s segments cover the 16s window.
		assert_eq!(numbers(&joined), [last - 1, last]);
		assert_eq!(joined.media_sequence, last - 1);
		assert_eq!(numbers(&followed), numbers(&joined));
		assert_eq!(followed.media_sequence, joined.media_sequence);
	}

	// Very short segments: the window is capped at what a fresh join can see, so an edge that
	// followed the broadcast all along lists exactly what one joining now does.
	#[tokio::test(start_paused = true)]
	async fn a_dense_timeline_lists_the_same_window_on_every_edge() {
		let early = Timelines::with(&["video0"], &[]);
		let (_track, mut video0) = early.publish("video0");
		// 40ms records: the 16s window spans 400 of them.
		for sequence in 0..1_000 {
			video0.push(&span(sequence, sequence * 40, 40, sequence)).unwrap();
			settle().await;
		}
		// The publisher rolls its timeline group, so a joiner reads only the checkpoint and on.
		video0.flush().unwrap();
		video0.push(&span(1_000, 40_000, 40, 1_000)).unwrap();

		let late = early.join();
		let joined = late.listed("video0", 1_000).await;
		let followed = early.listed("video0", 1_000).await;
		assert_eq!(joined.segments.len(), segments::MAX_SEGMENTS);
		assert_eq!(numbers(&followed), numbers(&joined));
		assert_eq!(followed.media_sequence, joined.media_sequence);
	}

	// Two edges over one timeline agree on the media sequence, and so does a reload after the
	// publisher pops records out of the window.
	#[tokio::test(start_paused = true)]
	async fn edges_and_reloads_agree_on_the_media_sequence() {
		let early = Timelines::with(&["video0"], &[]);
		let (_track, mut video0) = early.publish("video0");
		for sequence in 0..20 {
			video0.push(&gop(sequence)).unwrap();
			settle().await;
		}
		let late = early.join();
		let joined = late.listed("video0", 19).await;
		let followed = early.listed("video0", 19).await;
		assert_eq!(joined.media_sequence, 12, "eight 2s segments cover the 16s window");
		assert_eq!(numbers(&followed), numbers(&joined));

		// A DVR expiry trims into the window.
		video0.pop(15).unwrap();
		video0.push(&gop(20)).unwrap();
		let joined = late.listed("video0", 20).await;
		let followed = early.listed("video0", 20).await;
		let fresh = early.join().listed("video0", 20).await;
		for snapshot in [&joined, &followed, &fresh] {
			assert_eq!(snapshot.media_sequence, 15);
			assert_eq!(numbers(snapshot), [15, 16, 17, 18, 19, 20]);
		}
	}

	// A video rendition is advertised once a listed segment starts at a group start that is a
	// sync point. Audio needs none, so a master rendered early may list audio alone.
	#[tokio::test(start_paused = true)]
	async fn video_is_advertised_from_its_first_sync_point() {
		let test = Timelines::with(&["video0", "video1"], &["audio0"]);
		assert!(advertised(&test.renditions).is_empty(), "nothing listed yet");

		let (_track0, mut video0) = test.publish("video0");
		let (_track1, mut video1) = test.publish("video1");
		let (_track2, mut audio0) = test.publish("audio0");
		for sequence in 0..4 {
			// Joined mid-group, then a group start that is not a sync point (an intra-refresh run).
			let mut record = gop(sequence);
			record.keyframe = false;
			if sequence < 2 {
				record.start.frame = 5;
			}
			video0.push(&record).unwrap();
			video1.push(&record).unwrap();
			audio0.push(&gop(sequence)).unwrap();
		}
		test.listed("video0", 3).await;
		test.listed("audio0", 2).await;
		assert_eq!(advertised(&test.renditions), ["audio0"]);

		// The master route long-polls on this: it must wake on the first sync start.
		let renditions = test.renditions.clone();
		let video = tokio::spawn(async move {
			kio::wait(|waiter| {
				let snapshot = renditions.snapshot();
				let video0 = snapshot.iter().find(|rendition| rendition.name == "video0").unwrap();
				video0.poll_advertised(waiter)
			})
			.await
		});
		tokio::time::sleep(Duration::from_secs(1)).await;
		assert!(!video.is_finished(), "video0 has no sync start yet");

		for sequence in 4..7 {
			video0.push(&gop(sequence)).unwrap();
			video1.push(&gop(sequence)).unwrap();
			audio0.push(&gop(sequence)).unwrap();
		}
		tokio::time::timeout(Duration::from_secs(5), video)
			.await
			.expect("the wait wakes on the first sync start")
			.unwrap();
		test.listed("video1", 4).await;
		assert_eq!(advertised(&test.renditions), ["video0", "video1", "audio0"]);
	}

	/// Record `sequence` covering frames `frames` of one long group from `pts_ms`, a sync point
	/// only at the group start: how `moq-mux` splits a GOP longer than its record limit.
	fn split(sequence: u64, pts_ms: u64, duration_ms: u64, frames: std::ops::Range<u64>) -> hang::timeline::Record {
		let mut record = span(sequence, pts_ms, duration_ms, 0);
		record.start.frame = frames.start;
		record.end = record.start;
		record.end.frame = frames.end;
		record.keyframe = frames.start == 0;
		record
	}

	// A 30s GOP is split into 10s records with one sync start. The 16s window keeps that record
	// rather than evict it, so video stays startable and advertised until the next GOP starts.
	#[tokio::test(start_paused = true)]
	async fn a_long_gop_keeps_its_sync_start_in_the_window() {
		let test = Timelines::with(&["video0"], &["audio0"]);
		let (_track0, mut video0) = test.publish("video0");
		let (_track1, mut audio0) = test.publish("audio0");
		for sequence in 0..3 {
			let frames = sequence * 300..(sequence + 1) * 300;
			video0
				.push(&split(sequence, sequence * 10_000, 10_000, frames))
				.unwrap();
			audio0
				.push(&span(sequence, sequence * 10_000, 10_000, sequence))
				.unwrap();
		}
		let window = test.listed("video0", 2).await;
		assert_eq!(numbers(&window), [0, 1, 2], "the sync start stays listed");
		let joined = test.join().listed("video0", 2).await;
		assert_eq!(numbers(&joined), [0, 1, 2], "a fresh edge lists the same window");
		assert_eq!(advertised(&test.renditions), ["video0", "audio0"]);

		video0.push(&span(3, 30_000, 10_000, 1)).unwrap();
		audio0.push(&span(3, 30_000, 10_000, 3)).unwrap();
		let window = test.listed("video0", 3).await;
		assert_eq!(numbers(&window), [2, 3], "the next sync start releases the old GOP");
		assert_eq!(advertised(&test.renditions), ["video0", "audio0"]);
	}

	// A GOP past the 256-record cap loses its sync start to the cap. The rendition stays
	// advertised for the rest of the run rather than vanish from a master players already loaded.
	#[tokio::test(start_paused = true)]
	async fn a_gop_past_the_cap_stays_advertised() {
		let test = Timelines::with(&["video0"], &[]);
		let (_track, mut video0) = test.publish("video0");
		let records = segments::MAX_SEGMENTS as u64 + 44;
		// The exporter follows from the GOP start; a later join is only restated the tail.
		video0.push(&split(0, 0, 40, 0..1)).unwrap();
		test.listed("video0", 0).await;
		for sequence in 1..records {
			video0
				.push(&split(sequence, sequence * 40, 40, sequence..sequence + 1))
				.unwrap();
		}
		let window = test.listed("video0", records - 1).await;
		assert_eq!(numbers(&window)[0], 44, "the cap evicted the sync start");
		assert_eq!(advertised(&test.renditions), ["video0"]);

		// A new run starts unadvertised until it lists its own sync start.
		test.renditions.fanout().set_generation(Some("a".into()));
		test.renditions.fanout().set_generation(Some("b".into()));
		video0
			.push(&split(records, records * 40, 40, records..records + 1))
			.unwrap();
		test.listed("video0", records).await;
		assert!(
			advertised(&test.renditions).is_empty(),
			"the latch belongs to the old run"
		);
	}

	// Clearing the window, whether a publisher restarted without a new generation (a rewind) or
	// records were skipped, drops the advertisement latch with it.
	#[tokio::test(start_paused = true)]
	async fn a_cleared_window_drops_the_latch() {
		let test = Timelines::with(&["video0"], &[]);
		let (_track, mut video0) = test.publish("video0");
		video0.push(&split(0, 0, 40, 0..1)).unwrap();
		test.listed("video0", 0).await;
		video0.push(&split(1, 40, 40, 1..2)).unwrap();
		test.listed("video0", 1).await;
		assert_eq!(advertised(&test.renditions), ["video0"]);

		// The new run opens mid-GOP at an earlier time, under later record numbers.
		video0.push(&split(2, 0, 40, 5..6)).unwrap();
		let window = test.listed("video0", 2).await;
		assert_eq!(numbers(&window), [2], "the rewind reset the window");
		assert!(advertised(&test.renditions).is_empty(), "a rewind drops the latch");

		video0.push(&split(3, 40, 40, 0..1)).unwrap();
		test.listed("video0", 3).await;
		assert_eq!(advertised(&test.renditions), ["video0"]);
		test.renditions.fanout().skip(None);
		video0.push(&split(4, 80, 40, 1..2)).unwrap();
		let window = test.listed("video0", 4).await;
		assert_eq!(numbers(&window), [4], "the skip cleared the window");
		assert!(advertised(&test.renditions).is_empty(), "a skip drops the latch");
	}

	// A span with no media in one rendition keeps its slot as a gap of the same duration there
	// alone. The rendition goes on listing after it, at a sync point a player can switch onto,
	// and its siblings never notice.
	#[tokio::test(start_paused = true)]
	async fn a_gap_in_one_rendition_holds_back_nothing() {
		let test = Timelines::with(&["video0", "video1"], &["audio0"]);
		let (_track0, mut video0) = test.publish("video0");
		let (_track1, mut video1) = test.publish("video1");
		let (_track2, mut audio0) = test.publish("audio0");
		for sequence in 0..8 {
			video0.push(&gop(sequence)).unwrap();
		}
		// video1 and audio0 have nothing from 4s to 8s (segments 2 and 3).
		for (sequence, gop) in [0, 1, 4, 5, 6, 7].into_iter().enumerate() {
			let record = span(sequence as u64, gop * 2_000, 2_000, sequence as u64);
			video1.push(&record).unwrap();
			audio0.push(&record).unwrap();
		}

		let reference = test.listed("video0", 7).await;
		assert!(
			reference.segments.iter().all(|s| !s.gap),
			"a sibling's gap is not the reference's"
		);
		for name in ["video1", "audio0"] {
			let snapshot = test.listed(name, 5).await;
			let gaps: Vec<u64> = snapshot.segments.iter().filter(|s| s.gap).map(|s| s.segment).collect();
			assert_eq!(gaps, [2, 3], "{name}");
			assert!(
				snapshot.segments.iter().all(|s| s.duration == Duration::from_secs(2)),
				"{name}: a gap keeps its duration"
			);
		}

		// Switching to video1 after its gap lands on a segment that starts at a sync point.
		let Some(spans::Content::Frames { ranges, .. }) = test.rendition("video1").listed_content(4) else {
			panic!("segment 4 resolves on video1");
		};
		assert_eq!(
			ranges[0].start,
			hang::timeline::Position::group(2),
			"its record after the gap"
		);
	}
}
