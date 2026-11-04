//! Export: serve a MoQ broadcast as HLS or DASH, fetching media on demand.
//!
//! A [`Broadcaster`] subscribes to one broadcast's catalog and each rendition's media track,
//! reading only each group's first frame timestamp, which the transport carries. Playlists and
//! manifests are a pure function of those timestamps and the catalog's `media.join.interval`,
//! and media bytes move only when an HTTP client requests a segment, which FETCHes exactly the
//! groups that segment holds from the relay cache and transmuxes them to CMAF. A broadcast whose
//! catalog declares no interval gets no playlists. A `moq-archive` recording served by its
//! `Consumer` is served the same way, and a segment GETs only its rendition's objects.
//!
//! Every rendition numbers its groups into segments on the grid the interval draws (see the
//! `grid` module), so every edge and every rendition agree on segment numbers without
//! coordinating, while each cuts its boundaries at its own keyframes.
//! An inline-parameter-set codec with no catalog `description` GETs one keyframe
//! group on the first playlist render (in the background on a manifest render) to build its
//! init, then caches it.
//!
//! That background work (the catalog and media watchers, and init builds) runs on the
//! [`Driver`] [`Broadcaster::new`] returns: poll it or spawn [`Driver::run`] wherever the
//! embedder runs tasks.
//!
//! The same machinery serves two kinds of consumer:
//!
//! * the HTTP serve path (pull): [`Route::parse`] names the resource a request path addresses,
//!   and [`Broadcaster::rendition`] / [`Broadcaster::master_playlist`] /
//!   [`Broadcaster::manifest`] and [`Rendition::playlist`] / [`Rendition::init`] /
//!   [`Rendition::segment`] render or fetch it per request, with or without the `server`
//!   feature's router; and
//! * a recorder (push): the [`renditions::Consumer`] and [`segments::Consumer`] cursors, which
//!   yield every rendition and every finalized segment in order, for mirroring a broadcast to
//!   storage.

#[cfg(test)]
mod archive_tests;
mod contributions;
mod driver;
mod grid;
pub mod master;
mod mpd;
mod playlist;
mod rendition;
mod route;

pub mod renditions;
pub mod segments;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use moq_mux::catalog::{self, CatalogFormat, Stream};
use rand::RngExt;

pub use driver::Driver;
pub(crate) use playlist::render_media;
#[cfg(feature = "server")]
pub(crate) use rendition::Demand;
pub use rendition::{Kind, Rendition};
pub use route::{Contribution, Resource, Route};

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
	/// Minimum duration of media listed in each rendition's playlist window. Older segments are
	/// evicted once the remaining ones still cover this span. Past 256 segments they are evicted
	/// however short they are. Keep it within the media's max age and the relay's group-cache
	/// retention, since segments are fetched from there on request. It also caps segment `Cache-Control: max-age`.
	///
	/// A durable archive gets the same window: a live playlist stays bounded however much its store
	/// retains.
	pub window: Duration,
}

impl Default for Config {
	fn default() -> Self {
		Self {
			window: Duration::from_secs(16),
		}
	}
}

/// All renditions of one broadcast, kept in sync with its catalog.
///
/// A cheap handle: clones share the one broadcaster, which stops watching the catalog once the
/// last clone drops.
#[derive(Clone)]
pub struct Broadcaster {
	#[cfg_attr(
		not(feature = "server"),
		allow(
			dead_code,
			reason = "read only by the serve path's `is_closed`/`closed` (server-gated); a recording consumer observes close through its `renditions()` cursor ending instead"
		)
	)]
	broadcast: moq_net::broadcast::Consumer,
	/// The stream served, which every URL the master playlist and manifest list starts with:
	/// its instance, or an id minted for a broadcast that came through no route.
	instance: Arc<str>,
	/// The current rendition set, reconciled from the catalog by the watcher task.
	renditions: renditions::Producer,
	contributions: Option<contributions::Producer>,
	/// Shared by every clone; its drop with the last one ends the catalog watcher.
	_guard: Arc<Guard>,
}

/// What the last [`Broadcaster`] clone releases.
struct Guard {
	renditions: renditions::Producer,
	contributions: Option<contributions::Producer>,
	/// Ends the catalog watcher when dropped, or when [`replaced`](Broadcaster::replaced) takes it.
	watcher: Mutex<Option<driver::Owner>>,
}

impl Drop for Guard {
	fn drop(&mut self) {
		// Release the map: a surviving `renditions::Consumer` would otherwise pin every
		// `Rendition` forever. A recording cursor's own `Arc<Rendition>` keeps that rendition's
		// media flowing, so its final segments are not truncated. Each rendition stops
		// following its media once no recording cursor holds it.
		self.renditions.clear();
		if let Some(contributions) = &self.contributions {
			contributions.clear();
		}
	}
}

impl Broadcaster {
	/// Start tracking the own renditions of `source`'s catalog broadcast, once the returned
	/// [`Driver`] runs.
	///
	/// Fails with [`Error::UnsupportedCpu`](crate::Error::UnsupportedCpu) on a CPU its
	/// cryptography cannot run on.
	pub fn new(source: moq_mux::Source, config: Config) -> crate::Result<(Self, Driver)> {
		rind::cpu::check().map_err(crate::Error::unsupported_cpu)?;
		Ok(Self::start(source, config, None))
	}

	/// [`new`](Self::new) for a caller that already checked the CPU.
	pub(crate) fn start(
		source: moq_mux::Source,
		config: Config,
		origin: Option<moq_net::origin::Consumer>,
	) -> (Self, Driver) {
		let broadcast = source.broadcast();
		// An instance names the stream on every edge, so each serves it under the same URLs.
		let instance = broadcast
			.info()
			.instance
			.clone()
			.unwrap_or_else(moq_net::Instance::random);
		let (spawner, mut driver) = driver::Spawner::new();
		let contributions = origin.map(|origin| contributions::Producer::new(origin, config.window, spawner.clone()));
		let renditions = renditions::Producer::new(config.window, spawner);
		// The watcher owns its own producer clone; dropping the `Broadcaster` ends it so the
		// standing catalog subscription stops when nobody's serving from this broadcaster.
		let watcher = driver.lead(watch_catalog(
			source,
			renditions.clone(),
			Role::Root(contributions.clone().map(Box::new)),
		));
		let broadcaster = Self {
			broadcast,
			instance: Arc::from(instance.as_str()),
			_guard: Arc::new(Guard {
				renditions: renditions.clone(),
				contributions: contributions.clone(),
				watcher: Mutex::new(Some(watcher)),
			}),
			renditions,
			contributions,
		};
		(broadcaster, driver)
	}

	/// [`new`](Self::new), with its driver spawned on the ambient runtime.
	#[cfg(test)]
	pub(crate) fn spawned(source: moq_mux::Source, config: Config) -> Self {
		let (broadcaster, driver) = Self::new(source, config).unwrap();
		tokio::spawn(driver.run());
		broadcaster
	}

	/// Return true if this is a clone of the same broadcaster.
	pub fn is_clone(&self, other: &Self) -> bool {
		Arc::ptr_eq(&self._guard, &other._guard)
	}

	/// How many clones of this broadcaster are alive.
	#[cfg(all(test, feature = "server"))]
	pub(crate) fn clones(&self) -> usize {
		Arc::strong_count(&self._guard)
	}

	/// A weak reference that reports, through its `strong_count`, whether any clone is alive.
	#[cfg(all(test, feature = "server"))]
	pub(crate) fn downgrade(&self) -> std::sync::Weak<impl Sized + use<>> {
		Arc::downgrade(&self._guard)
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
	#[cfg(feature = "server")]
	pub(crate) fn poll_closed(&self, waiter: &kio::Waiter) -> std::task::Poll<()> {
		self.broadcast.poll_closed(waiter)
	}

	/// Look up a rendition by kind and name. Video and audio are separate axes, so a
	/// video and an audio rendition may share a name without colliding. A rendition is found
	/// only once the catalog declares its join interval.
	pub fn rendition(&self, kind: Kind, name: &str) -> Option<Arc<Rendition>> {
		self.renditions.get(kind, name)
	}

	/// A cursor over rendition add/remove events, for a consumer that mirrors or records the
	/// whole broadcast. It replays the current renditions, then yields changes, and returns
	/// `Ok(None)` only once the source finishes cleanly. A rendition is added once the catalog
	/// declares its join interval, which an unhinted publisher measures over its first second or
	/// group.
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
		kio::wait(|waiter| {
			let own = self.renditions.poll_advertised(waiter);
			let contribution = self
				.contributions
				.as_ref()
				.is_some_and(|contributions| contributions.poll_advertised(waiter).is_ready());
			// Contributions alone answer only while no own rendition could still be listed.
			if own.is_ready() || (!self.renditions.listable() && contribution) {
				std::task::Poll::Ready(())
			} else {
				std::task::Poll::Pending
			}
		})
		.await;
	}

	/// Version every segment URL with `generation`, a label for the publisher run the
	/// broadcast carries now, or stop versioning with `None` (the default).
	///
	/// A restarted publisher is spliced into the same broadcast and restarts its segment
	/// numbers, so `seg/{tag}.0.m4s` would name different bytes on each run. Supply a new generation
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
	/// A broadcaster obtained from `Server` also lists contribution renditions from their
	/// catalogs, starting their media only on request. `Broadcaster::new` exports the own
	/// renditions only, so a recorder never demands contributions.
	///
	/// Lists only enabled own renditions a player can start, i.e. ones listing at least one segment,
	/// so an early render may list audio alone, or nothing at all. A disabled rendition (catalog `enabled: false`) is left out until
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
		if let Some(contributions) = &self.contributions {
			let (extra_video, extra_audio) = contributions.variants(&self.instance, query);
			video.extend(extra_video);
			audio.extend(extra_audio);
		}
		(video, audio)
	}

	#[cfg(feature = "server")]
	pub(crate) async fn contribution(
		&self,
		at: &Contribution,
		kind: Kind,
		name: &str,
	) -> crate::Result<Option<Arc<Rendition>>> {
		self.renditions.catalogued().await;
		match &self.contributions {
			Some(contributions) => contributions.rendition(at, kind, name).await,
			None => Ok(None),
		}
	}

	/// Render the DASH manifest (MPD) from the current renditions' segments, or `None` while nothing is listable yet (every `SegmentTimeline`
	/// would be empty, which players reject).
	///
	/// Live broadcasts render a `dynamic` presentation anchored at the catalog's declared
	/// wall clock (or, absent one, at an anchor estimated from the first segment's arrival); a
	/// finished broadcast renders `static`. `query` propagates to every child URL exactly as
	/// in [`master_playlist`](Self::master_playlist).
	///
	/// A disabled rendition is left out, as in the master playlist. Each representation names
	/// its init by a hash of the bytes, so a rendition is left out until its init is known. An
	/// inline-parameter-set codec only learns it from media, so rendering starts building each
	/// missing init in the background and a later render lists that rendition; no render waits
	/// on one.
	///
	/// Fails with [`CatalogFailed`](crate::Error::CatalogFailed) once the catalog failed, rather
	/// than turning a truncated presentation `static`. A rendition whose own media track failed is
	/// left out instead, and its [`Rendition`] listings fail.
	pub fn manifest(&self, query: Option<&str>) -> crate::Result<Option<String>> {
		if let Some(err) = self.renditions.failure() {
			return Err(err);
		}
		let mut video = Vec::new();
		let mut audio = Vec::new();
		let mut availability_start = None;
		// A failure left out of the listing, answered if no other rendition lists, now or once
		// its init is built.
		let mut skipped = None;
		let mut listable = false;
		// An enabled rendition whose init is still building, which a later render lists.
		let mut pending = false;
		for rendition in self.renditions.snapshot() {
			// Every rendition shares the catalog's one root clock, so any of them can
			// supply the declared wall anchor.
			if availability_start.is_none() {
				availability_start = rendition.wall_clock(moq_net::Timestamp::ZERO);
			}
			if !rendition.is_enabled() {
				continue;
			}
			// Without its init there is nothing to name in `initialization`, so it is left out
			// of this render.
			rendition.start_init();
			pending |= rendition.init_pending();
			let representation = match rendition.representation() {
				Ok(representation) => {
					listable = true;
					representation
				}
				// Its own playlist answers the error, while its siblings stay listed.
				Err(err) if rendition.failed_alone() => {
					skipped.get_or_insert(err);
					continue;
				}
				Err(err) => return Err(err),
			};
			match rendition.kind {
				Kind::Video => video.extend(representation),
				Kind::Audio => audio.extend(representation),
			}
		}

		if let (false, Some(err)) = (listable, skipped) {
			return Err(err);
		}
		let representations = || video.iter().chain(&audio);
		if representations().all(|rep| rep.segments.is_empty()) {
			return Ok(None);
		}
		// Static only once every representation is final and no init is still building, so
		// neither a listed nor a pending rendition stops being refreshed early.
		let finished = !pending && representations().all(|rep| rep.ended);
		if !finished {
			// A dynamic presentation needs pts 0 anchored to the wall clock. The fallback is
			// set whenever a segment has been listed.
			availability_start = availability_start.or_else(|| self.renditions.anchor());
			if availability_start.is_none() {
				return Ok(None);
			}
		}

		Ok(Some(mpd::render_manifest(
			&mpd::Manifest {
				instance: self.instance.clone(),
				availability_start,
				publish: kio::clock::wall(),
				window: self.renditions.window(),
				finished,
				video,
				audio,
			},
			query,
		)))
	}

	/// Build video inits until one enabled video rendition is listable, or none is left that
	/// could be. A DASH player picks its adaptation sets from the first manifest it loads, so that
	/// one must not lack video only because no init was built yet, while any one built init is
	/// enough to render at once. Bounding the wait is the caller's policy.
	#[cfg_attr(
		not(feature = "server"),
		allow(dead_code, reason = "only the `server` feature uses it")
	)]
	pub(crate) async fn video_init(&self) {
		self.renditions.video_init().await;
	}

	/// Resolve once the broadcast has something the DASH manifest lists (its first complete
	/// segment, or an already-ended track); a reconfigured rendition's predecessor's segments
	/// don't count. Renditions start their segments together, so the first rendition's readiness
	/// stands in for the broadcast's. Bounding the wait is the caller's policy.
	#[cfg_attr(
		not(feature = "server"),
		allow(dead_code, reason = "only the `server` feature uses it")
	)]
	pub(crate) async fn represented(&self) {
		if let Some(rendition) = self.renditions.snapshot().into_iter().next() {
			rendition.represented().await;
		}
	}

	/// End every playlist and recording cursor at the segments listed now, because another
	/// publisher run took the broadcast over, and stop following this one's catalog and media.
	/// The replaced publisher may keep sending, so nothing else would end them.
	#[cfg_attr(
		not(feature = "server"),
		allow(dead_code, reason = "only the `server` feature follows takeovers")
	)]
	pub(crate) fn replaced(&self) {
		let watcher = self._guard.watcher.lock().unwrap().take();
		drop(watcher);
		self.renditions.cut();
		if let Some(contributions) = &self.contributions {
			contributions.clear();
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

/// What a catalog watcher reconciles besides its own rendition set.
enum Role {
	/// A root broadcast, following its catalog's contributions when an HTTP server serves it.
	Root(Option<Box<contributions::Producer>>),
	/// A contribution included in a root broadcast. Its renditions take the root catalog's
	/// `media`, so they share the root's timeline and grid whatever the contribution declares.
	Contribution(kio::Consumer<hang::catalog::Media>),
}

async fn watch_catalog(source: moq_mux::Source, renditions: renditions::Producer, role: Role) {
	// The source's catalog broadcast, so the catalog, the media subscriptions, and the closed
	// check below all refer to the one broadcast it was built from.
	let broadcast = &source.broadcast();
	let mut delay = CATALOG_RETRY_MIN;
	let mut consumer = loop {
		match catalog::Consumer::<()>::new(broadcast, CatalogFormat::Hang).await {
			Ok(consumer) => break consumer,
			Err(err) => {
				tracing::warn!(%err, "failed to subscribe to broadcast catalog, retrying");
				let wait = delay.mul_f64(0.5 + kio::clock::rng().random::<f64>() / 2.0);
				delay = (delay * 2).min(CATALOG_RETRY_MAX);
				tokio::select! {
					_ = kio::clock::sleep(wait) => {}
					_ = kio::wait(|waiter| broadcast.poll_closed(waiter)) => {
						renditions.close();
						return;
					}
				}
			}
		}
	};

	// A contribution's latest catalog, re-synced when the root's media changes.
	let mut latest: Option<catalog::hang::Catalog> = None;
	let result = loop {
		let next = kio::wait(|waiter| {
			if let std::task::Poll::Ready(next) = consumer.poll_next(waiter) {
				return std::task::Poll::Ready(Some(next));
			}
			if let (Role::Contribution(root), Some(catalog)) = (&role, &latest)
				&& let std::task::Poll::Ready(Ok(())) = root.poll(waiter, |media| match **media != catalog.media {
					true => std::task::Poll::Ready(()),
					false => std::task::Poll::Pending,
				}) {
				return std::task::Poll::Ready(None);
			}
			std::task::Poll::Pending
		})
		.await;
		let mut catalog = match next {
			Some(Ok(Some(catalog))) => catalog,
			Some(Ok(None)) => break Ok(()),
			Some(Err(err)) => {
				tracing::warn!(%err, "broadcast catalog stream ended with error");
				break Err(err.into());
			}
			None => latest.take().expect("a root media change follows a catalog"),
		};
		match &role {
			Role::Root(Some(contributions)) => contributions.sync(&source, &catalog),
			Role::Root(None) => {}
			Role::Contribution(root) => catalog.media = *root.read(),
		}
		renditions.sync(&source, &catalog);
		if let Role::Contribution(_) = role {
			latest = Some(catalog);
		}
	};
	let failed = result.is_err();
	renditions.finish(result);
	// The root catalog lists the contributions: once it fails, stop following and advertising
	// them too. A clean end freezes the final catalog, which keeps its entries, as the own
	// renditions keep their final set.
	if failed && let Role::Root(Some(contributions)) = &role {
		contributions.clear();
	}
}

#[cfg(test)]
mod tests {
	/// A millisecond track: the frames written here carry timestamps.
	/// A millisecond track with no publisher max age, so a test's playlist window may span more
	/// media than the default max age keeps fresh.
	fn timed() -> moq_net::track::Info {
		moq_net::track::Info::default()
			.with_timescale(moq_net::Timescale::MILLI)
			.with_max_age(None)
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

	type Media<R> = moq_mux::container::Producer<moq_mux::catalog::hang::Container, R>;

	/// The join interval every test catalog declares.
	const INTERVAL: Duration = Duration::from_secs(2);

	/// An empty catalog declaring a two-second join interval.
	fn joined() -> moq_mux::catalog::hang::Catalog {
		let mut catalog = moq_mux::catalog::hang::Catalog::default();
		catalog.media.join = hang::catalog::Join::new(INTERVAL);
		catalog
	}

	/// A catalog config declaring a two-second join interval up front (see [`joined`]).
	fn catalog_config() -> moq_mux::catalog::Config {
		moq_mux::catalog::Config::default().with_catalog(joined())
	}

	/// A catalog on `broadcast` declaring its join interval up front (see [`catalog_config`]).
	fn hls_catalog(broadcast: &mut moq_net::broadcast::Producer) -> moq_mux::catalog::Producer {
		moq_mux::catalog::Producer::new(broadcast, catalog_config()).unwrap()
	}

	/// Publish a video track `name` through `catalog`, which owns its rendition.
	fn video(
		catalog: &moq_mux::catalog::Producer,
		broadcast: &moq_net::broadcast::Producer,
		name: &str,
		config: hang::catalog::VideoConfig,
	) -> Media<hang::catalog::VideoConfig> {
		let track = broadcast.create_track(name, timed()).unwrap();
		let container = moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Data);
		catalog.video(track, container, config).unwrap()
	}

	/// Publish an audio track `name` through `catalog`, as [`video`] does.
	fn audio(
		catalog: &moq_mux::catalog::Producer,
		broadcast: &moq_net::broadcast::Producer,
		name: &str,
		config: hang::catalog::AudioConfig,
	) -> Media<hang::catalog::AudioConfig> {
		let track = broadcast.create_track(name, timed()).unwrap();
		let container = moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Data);
		catalog.audio(track, container, config).unwrap()
	}

	fn opus() -> hang::catalog::AudioConfig {
		hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Opus, 48_000, 2)
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

	/// A VP8 key frame whose 320x240 geometry is only known inline.
	const KEYFRAME: &[u8] = &[0x10, 0x00, 0x00, 0x9d, 0x01, 0x2a, 0x40, 0x01, 0xf0, 0x00];

	fn vp8_frame(micros: u64, keyframe: bool) -> moq_mux::container::Frame {
		let payload = if keyframe { KEYFRAME } else { &[0x31, 0x00, 0x00][..] };
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

	/// The next rendition event, or `None` if the set is quiet.
	async fn next_event(cursor: &mut renditions::Consumer) -> Option<renditions::Event> {
		kio::clock::timeout(Duration::from_millis(50), cursor.next())
			.await
			.ok()
			.and_then(Result::unwrap)
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

	/// Whether `haystack` carries `needle` verbatim, i.e. whether a transmuxed segment carries a
	/// particular publisher's frame payloads.
	fn contains(haystack: &bytes::Bytes, needle: &[u8]) -> bool {
		haystack.windows(needle.len()).any(|window| window == needle)
	}

	fn numbers(snapshot: &playlist::Snapshot) -> Vec<u64> {
		snapshot.segments.iter().map(|s| s.segment).collect()
	}

	fn gaps(snapshot: &playlist::Snapshot) -> Vec<u64> {
		snapshot.segments.iter().filter(|s| s.gap).map(|s| s.segment).collect()
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

	/// The `trun` sample durations of a fragment.
	fn sample_durations(fragment: &bytes::Bytes) -> Vec<Option<u32>> {
		use moq_mux::container::fmp4::mp4_atom::{self, DecodeMaybe};

		let mut cursor = std::io::Cursor::new(fragment.as_ref());
		while let Some(atom) = mp4_atom::Any::decode_maybe(&mut cursor).unwrap() {
			if let mp4_atom::Any::Moof(moof) = atom {
				return moof.traf[0].trun[0]
					.samples
					.iter()
					.map(|entry| entry.duration)
					.collect();
			}
		}
		panic!("no moof in fragment");
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
		let video = video_config();

		let mut catalog = joined();
		catalog.video.renditions.insert("video0".to_string(), video);
		catalog.audio.renditions.insert("audio0".to_string(), audio);
		catalog
	}

	#[tokio::test]
	async fn escaping_broadcast_reference_is_not_advertised() {
		let origin = produce_origin();
		let _broadcast = origin.create_broadcast("a/pub").expect("publish allowed");
		_broadcast.announce(Default::default()).expect("publish allowed");
		settle().await;
		let upstream = crate::resolved(&origin, "a/pub").await;
		let mut config = hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::VP8);
		config.broadcast = Some(moq_net::path::Relative::new("../../source").unwrap());
		let mut catalog = joined();
		catalog.video.renditions.insert("video".to_string(), config);

		let renditions = renditions::Producer::spawned(Config::default().window);
		renditions.sync(&upstream, &catalog);
		assert!(renditions.get(Kind::Video, "video").is_none());
	}

	#[tokio::test(start_paused = true)]
	async fn audio_renditions_follow_catalog_quality_order() {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let source = moq_mux::Source::new(produce_origin().consume(), broadcast.consume());
		let mut catalog = joined();
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
		let renditions = renditions::Producer::spawned(Config::default().window);
		renditions.sync(&source, &catalog);
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

	// No rendition is served until the catalog declares its join interval: without that grid
	// nothing numbers their segments. Once it does, every rendition is added.
	#[tokio::test]
	async fn renditions_wait_for_the_join_interval() {
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").expect("publish allowed");
		broadcast.announce(Default::default()).expect("publish allowed");
		settle().await;
		let upstream = crate::resolved(&origin, "live").await;

		let renditions = renditions::Producer::spawned(Config::default().window);
		let mut cursor = renditions.subscribe();
		let mut catalog = catalog_with_both();
		catalog.media.join = None;
		renditions.sync(&upstream, &catalog);
		assert!(renditions.get(Kind::Video, "video0").is_none());
		assert!(renditions.get(Kind::Audio, "audio0").is_none());
		assert!(next_event(&mut cursor).await.is_none(), "nothing is announced early");

		catalog.media.join = hang::catalog::Join::new(INTERVAL);
		renditions.sync(&upstream, &catalog);
		let mut added = 0;
		while let Some(event) = next_event(&mut cursor).await {
			added += matches!(event, renditions::Event::Added(_)) as usize;
		}
		assert_eq!(added, 2);
		assert_eq!(renditions.get(Kind::Video, "video0").unwrap().interval(), INTERVAL);

		drop(broadcast);
	}

	// Until the interval is declared, each listed media track is held subscribed, so an on-demand
	// publisher produces the media its interval is measured from. A rendition that moves to another
	// track moves its hold with it.
	#[tokio::test]
	async fn a_held_track_follows_its_rendition() {
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").expect("publish allowed");
		broadcast.announce(Default::default()).expect("publish allowed");
		let first = broadcast.create_track("first", timed()).unwrap();
		let second = broadcast.create_track("second", timed()).unwrap();
		settle().await;
		let upstream = crate::resolved(&origin, "live").await;

		let renditions = renditions::Producer::spawned(Config::default().window);
		let mut catalog = moq_mux::catalog::hang::Catalog::default();
		let mut config = hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::VP8);
		config.track = Some("first".to_string());
		catalog.video.renditions.insert("video".to_string(), config.clone());
		renditions.sync(&upstream, &catalog);
		settle().await;
		assert!(first.demand().wanted().is_some(), "the listed track is held");
		assert!(second.demand().wanted().is_none());

		config.track = Some("second".to_string());
		catalog.video.renditions.insert("video".to_string(), config);
		renditions.sync(&upstream, &catalog);
		settle().await;
		assert!(first.demand().wanted().is_none(), "the old track is released");
		assert!(second.demand().wanted().is_some(), "the new track is held");
	}

	// A container this export can't read is no rendition: the hang catalog skips one it doesn't
	// know, rather than serving a playlist that never lists anything.
	#[tokio::test]
	async fn an_unknown_container_is_not_served() {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let source = moq_mux::Source::new(produce_origin().consume(), broadcast.consume());
		let config: hang::catalog::VideoConfig =
			serde_json::from_value(serde_json::json!({"codec": "vp8", "container": {"kind": "future"}})).unwrap();
		let mut catalog = joined();
		catalog.video.renditions.insert("video".to_string(), config);
		catalog.video.renditions.insert("known".to_string(), video_config());
		let renditions = renditions::Producer::spawned(Config::default().window);
		renditions.sync(&source, &catalog);
		assert!(renditions.get(Kind::Video, "video").is_none());
		assert!(renditions.get(Kind::Video, "known").is_some());
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
		let renditions = renditions::Producer::spawned(Config::default().window);
		let mut cursor = renditions.subscribe();
		renditions.sync(&upstream, &catalog);

		let video = renditions.get(Kind::Video, "video0").expect("video rendition");
		let audio = renditions.get(Kind::Audio, "audio0").expect("audio rendition");
		let audio_init = audio.latest_init().await.unwrap().expect("AAC init segment");
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
			audio.latest_init().await.unwrap().expect("cached AAC init segment"),
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
		let renditions = renditions::Producer::spawned(Config::default().window);
		let mut cursor = renditions.subscribe();
		renditions.sync(&upstream, &catalog);
		let video = renditions.get(Kind::Video, "video0").expect("video rendition");
		while next_event(&mut cursor).await.is_some() {}

		for framerate in [Some(30.0), Some(60.0), Some(60_000.0 / 1001.0)] {
			catalog.video.renditions.get_mut("video0").unwrap().framerate = framerate;
			renditions.sync(&upstream, &catalog);
			assert!(
				Arc::ptr_eq(&video, &renditions.get(Kind::Video, "video0").unwrap()),
				"the video rendition survives a framerate estimate of {framerate:?}"
			);
			assert!(next_event(&mut cursor).await.is_none(), "no rendition churn");
			// Still muxed at the timescale the rendition was built with.
			assert_eq!(video.muxer().unwrap().timescale().as_u64(), 90_000);
		}
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
		let renditions = renditions::Producer::spawned(Config::default().window);
		renditions.sync(&upstream, &catalog);
		let audio = renditions.get(Kind::Audio, "audio0").expect("audio rendition");

		catalog.audio.renditions.get_mut("audio0").unwrap().bitrate = Some(110_000);
		renditions.sync(&upstream, &catalog);
		assert!(
			Arc::ptr_eq(&audio, &renditions.get(Kind::Audio, "audio0").unwrap()),
			"the estimate updates the existing rendition"
		);
		let init = audio.latest_init().await.unwrap().expect("AAC init segment");

		assert_eq!(audio.bandwidth(), 110_000);
		assert_eq!(
			aac_avg_bitrate(&init),
			110_000,
			"the latest estimate reaches an init that was not cached yet"
		);
	}

	// The other half: a change that alters how the rendition decodes, or which track it
	// lists, still rebuilds it, so the cached init segment can never describe media it no
	// longer matches.
	#[tokio::test]
	async fn decoder_config_change_rebuilds_the_rendition() {
		let origin = produce_origin();
		let _broadcast = origin.create_broadcast("live").expect("publish allowed");
		_broadcast.announce(Default::default()).expect("publish allowed");
		settle().await;
		let upstream = empty_upstream(&origin, "live").await;

		let mut catalog = catalog_with_both();
		let renditions = renditions::Producer::spawned(Config::default().window);
		let mut cursor = renditions.subscribe();
		renditions.sync(&upstream, &catalog);

		let video = renditions.get(Kind::Video, "video0").expect("video rendition");
		let audio = renditions.get(Kind::Audio, "audio0").expect("audio rendition");
		while next_event(&mut cursor).await.is_some() {}

		// A resolution change and a media track change both rebuild.
		catalog.video.renditions.get_mut("video0").unwrap().coded_width = Some(640);
		catalog.audio.renditions.get_mut("audio0").unwrap().track = Some("audio1".into());
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

	#[tokio::test]
	async fn replaced_renditions_are_not_held() {
		let origin = produce_origin();
		let _broadcast = origin.create_broadcast("live").expect("publish allowed");
		_broadcast.announce(Default::default()).expect("publish allowed");
		settle().await;
		let upstream = empty_upstream(&origin, "live").await;

		let mut catalog = catalog_with_both();
		let renditions = renditions::Producer::spawned(Config::default().window);
		for width in 1..=8 {
			catalog.video.renditions.get_mut("video0").unwrap().coded_width = Some(width);
			renditions.sync(&upstream, &catalog);
		}
		// The unchanged audio rendition, the current video one, and at most the one it replaced.
		assert!(renditions.targets() <= 3, "{} targets", renditions.targets());
	}

	// The whole fetch-on-demand path in process: a broadcast publishes media through the
	// catalog (which declares its join interval), the Broadcaster renders playlists from the
	// groups' timestamps, and a segment request fetches and transmuxes exactly its groups.
	#[tokio::test]
	async fn master_lists_only_playable_renditions() {
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").expect("publish allowed");
		broadcast.announce(Default::default()).expect("publish allowed");
		settle().await;
		let catalog = hls_catalog(&mut broadcast.broadcast().clone());
		let mut media = video(&catalog, &broadcast, "video0", video_config());

		let broadcaster = Broadcaster::spawned(crate::resolved(&origin, "live").await, Config::default());
		let rendition = kio::clock::timeout(Duration::from_secs(5), async {
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
			kio::clock::timeout(Duration::from_millis(10), broadcaster.ready())
				.await
				.is_err(),
			"broadcaster became ready before it had a playable rendition"
		);

		media.write(vp8_frame(0, true)).unwrap();
		media.write(vp8_frame(2_000_000, true)).unwrap();
		media.write(vp8_frame(4_000_000, true)).unwrap();
		kio::clock::timeout(Duration::from_secs(5), broadcaster.ready())
			.await
			.expect("broadcaster becomes ready with a complete segment");
		assert!(broadcaster.master_playlist(None).contains("video/video0/media.m3u8"));

		drop((media, catalog, broadcast));
	}

	// A client resolves a `.` or `..` URL segment away and the router refuses an empty one, so
	// renditions with those names are never admitted, while their siblings keep serving.
	#[tokio::test(start_paused = true)]
	async fn renditions_named_outside_one_path_segment_are_refused() {
		// A video and an audio rendition can't share a name, so the refused names split.
		let refused = [(Kind::Video, ""), (Kind::Video, "."), (Kind::Audio, "..")];
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").unwrap();
		broadcast.announce(Default::default()).unwrap();
		settle().await;
		let mut catalog =
			moq_mux::catalog::Producer::new(&mut broadcast.broadcast().clone(), Default::default()).unwrap();
		let all: Vec<_> = refused.into_iter().chain([(Kind::Video, "video0")]).collect();
		catalog
			.mutate(|catalog| {
				catalog.media.join = hang::catalog::Join::new(INTERVAL);
				for &(kind, name) in &all {
					match kind {
						Kind::Video => {
							catalog.video.renditions.insert(name.into(), video_config());
						}
						Kind::Audio => {
							catalog.audio.renditions.insert(name.into(), opus());
						}
					}
				}
			})
			.unwrap();
		// Only the servable rendition is ever read.
		let mut video0 = Gops {
			track: broadcast.create_track("video0", timed()).unwrap(),
		};
		for sequence in 0..4 {
			video0.gop(sequence);
		}
		let broadcaster = Broadcaster::spawned(crate::resolved(&origin, "live").await, Config::default());
		kio::clock::timeout(Duration::from_secs(5), broadcaster.advertised())
			.await
			.expect("video0 is advertised");

		let master = broadcaster.master_playlist(None);
		let manifest = broadcaster.manifest(None).unwrap().expect("video0 is listed");
		assert!(master.contains("/video/video0/media.m3u8"), "{master}");
		assert!(manifest.contains("id=\"video/video0\""), "{manifest}");
		for (kind, name) in refused {
			assert!(broadcaster.rendition(kind, name).is_none(), "{kind:?} {name:?}");
			let segment = format!("/{}/{name}/", kind.as_str());
			assert!(!master.contains(&segment), "{master}");
			let id = format!("id=\"{}/{name}\"", kind.as_str());
			assert!(!manifest.contains(&id), "{manifest}");
		}
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
		let catalog = hls_catalog(&mut broadcast.broadcast().clone());
		let mut media = video(&catalog, &broadcast, "video0", video_config());
		for i in 0..3 {
			media.write(vp8_frame(i * 2_000_000, true)).unwrap();
		}

		let broadcaster = Broadcaster::spawned(crate::resolved(&origin, "live").await, Config::default());
		kio::clock::timeout(Duration::from_secs(5), broadcaster.advertised())
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
		media.set(disabled).unwrap();
		kio::clock::timeout(Duration::from_secs(5), async {
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
		kio::clock::sleep(Duration::from_millis(10)).await;
		assert!(!waiting.is_finished(), "nothing enabled is advertised");
		media.set(video_config()).unwrap();
		kio::clock::timeout(Duration::from_secs(5), waiting)
			.await
			.expect("a re-enable wakes the master's long-poll")
			.unwrap();
		assert!(listed());
		assert_eq!(sequence(&rendition), before, "the media sequence did not reset");

		drop((media, catalog, broadcast));
	}

	/// Publish a VP8 rendition with three GOPs, 2s apart, so segments 0 and 1 are listable.
	fn publish_vp8(
		broadcast: &mut moq_net::broadcast::Producer,
		config: hang::catalog::VideoConfig,
	) -> (moq_mux::catalog::Producer, Media<hang::catalog::VideoConfig>) {
		let catalog = hls_catalog(broadcast);
		let mut media = video(&catalog, broadcast, "video0", config);
		for (micros, keyframe) in [(0, true), (1_000_000, false), (2_000_000, true), (4_000_000, true)] {
			media.write(vp8_frame(micros, keyframe)).unwrap();
		}
		(catalog, media)
	}

	/// A VP8 config whose geometry is only known from a keyframe.
	fn inline_vp8() -> hang::catalog::VideoConfig {
		hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::VP8)
	}

	/// A completed segment already decoded its inline codec metadata, so the init never waits on
	/// the trailing group its media producer is still writing.
	#[tokio::test(start_paused = true)]
	async fn a_transmuxed_segment_keeps_its_init_with_an_unfinished_tail() {
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").expect("publish allowed");
		let (_catalog, _media) = publish_vp8(&mut broadcast.broadcast().clone(), inline_vp8());
		broadcast.announce(moq_net::origin::Route::default()).unwrap();
		settle().await;
		let broadcaster = Broadcaster::spawned(crate::resolved(&origin, "live").await, Config::default());
		broadcaster.ready().await;
		let rendition = broadcaster.rendition(Kind::Video, "video0").expect("rendition");
		let mut cursor = rendition.segments();
		let segment = cursor.next().await.unwrap().expect("completed segment");
		assert!(!segment.media.expect("media, not a gap").is_empty());

		let init = kio::clock::timeout(Duration::from_secs(1), rendition.latest_init())
			.await
			.expect("init must not wait for the unfinished trailing group")
			.unwrap()
			.expect("transmuxed segment has an init");
		assert_eq!(&init[4..8], b"ftyp");
	}

	/// An init bootstrap already blocked on an open group must not hold back a later request
	/// once a completed segment has cached the init.
	#[tokio::test(start_paused = true)]
	async fn a_blocked_init_bootstrap_does_not_hide_a_cached_init() {
		let (origin, broadcast, _catalog, _video0) = inline_pair("video1").await;
		// At first only segment 0 is listed, while its group is still open.
		let (media, open) = open_tail(&broadcast, "video1", 0);

		let broadcaster = Broadcaster::spawned(crate::resolved(&origin, "live").await, Config::default());
		broadcaster.ready().await;
		let rendition = broadcaster.rendition(Kind::Video, "video1").expect("rendition");
		until_listed(&rendition, 1).await;
		let mut blocked = std::pin::pin!(rendition.latest_init());
		assert!(
			kio::clock::timeout(Duration::from_millis(10), blocked.as_mut())
				.await
				.is_err(),
			"the bootstrap reads the open group"
		);

		// Closed groups follow, and transmuxing one caches the init.
		for group in 2..5 {
			let mut writing = media.create_group(group.into()).unwrap();
			write_keyframe(&mut writing, group);
			writing.finish().unwrap();
		}
		until_listed(&rendition, 4).await;
		let segment = rendition
			.numbered(rendition.tag(), 2)
			.await
			.unwrap()
			.expect("completed segment");
		assert!(!segment.is_empty());
		kio::clock::timeout(Duration::from_secs(1), rendition.latest_init())
			.await
			.expect("init must not wait behind the blocked bootstrap")
			.unwrap()
			.expect("transmuxed segment has an init");
		drop(open);
	}

	/// `video0` and `video1` on a two-second interval: `inline` is a VP8 whose init needs a keyframe
	/// group, and the other's init builds from the catalog. Returns the other's media to write, and
	/// leaves the inline one's track to the test.
	async fn inline_pair(
		inline: &str,
	) -> (
		moq_net::origin::Producer,
		moq_net::origin::Publication,
		moq_mux::catalog::Producer,
		Gops,
	) {
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").unwrap();
		broadcast.announce(Default::default()).unwrap();
		settle().await;
		let mut catalog =
			moq_mux::catalog::Producer::new(&mut broadcast.broadcast().clone(), Default::default()).unwrap();
		catalog
			.mutate(|catalog| {
				catalog.media.join = hang::catalog::Join::new(INTERVAL);
				for name in ["video0", "video1"] {
					let config = if name == inline { inline_vp8() } else { video_config() };
					catalog.video.renditions.insert(name.into(), config);
				}
			})
			.unwrap();
		let other = if inline == "video0" { "video1" } else { "video0" };
		let other = Gops {
			track: broadcast.create_track(other, timed()).unwrap(),
		};
		(origin, broadcast, catalog, other)
	}

	/// [`inline_pair`] with `video1` inline and four [`Gops::gop`]s on `video0`, which lists three
	/// segments.
	async fn inline_sibling() -> (
		moq_net::origin::Producer,
		moq_net::origin::Publication,
		moq_mux::catalog::Producer,
		Gops,
	) {
		let (origin, broadcast, catalog, mut video0) = inline_pair("video1").await;
		for sequence in 0..4 {
			video0.gop(sequence);
		}
		(origin, broadcast, catalog, video0)
	}

	/// Publish `name` with group `sequence` still open and a finished one after it, so the segment
	/// of the open group is listed while the group is the only one its init can build from.
	fn open_tail(
		broadcast: &moq_net::broadcast::Producer,
		name: &str,
		sequence: u64,
	) -> (moq_net::track::Producer, moq_net::group::Producer) {
		let media = broadcast.create_track(name, timed()).unwrap();
		let mut open = media.create_group(sequence.into()).unwrap();
		write_keyframe(&mut open, sequence);
		let mut next = media.create_group((sequence + 1).into()).unwrap();
		write_keyframe(&mut next, sequence + 1);
		next.finish().unwrap();
		(media, open)
	}

	/// Write one VP8 keyframe at [`Gops::gop`] `sequence`'s pts into `group`.
	fn write_keyframe(group: &mut moq_net::group::Producer, sequence: u64) {
		hang::container::Frame {
			timestamp: moq_net::Timestamp::from_millis(sequence * 2_000).unwrap(),
			payload: bytes::Bytes::from_static(KEYFRAME),
		}
		.write_to(group)
		.unwrap();
	}

	/// Wait until `rendition` lists at least `count` segments.
	async fn until_listed(rendition: &Rendition, count: usize) {
		kio::clock::timeout(Duration::from_secs(5), async {
			while rendition.snapshot().segments.len() < count {
				kio::clock::sleep(Duration::from_millis(10)).await;
			}
		})
		.await
		.expect("the segments are listed");
	}

	/// Render the manifest until it lists `video1`.
	async fn listed_in_manifest(broadcaster: &Broadcaster) -> String {
		kio::clock::timeout(Duration::from_secs(5), async {
			loop {
				let manifest = broadcaster.manifest(None).unwrap().unwrap();
				if manifest.contains("id=\"video/video1\"") {
					return manifest;
				}
				kio::clock::sleep(Duration::from_millis(10)).await;
			}
		})
		.await
		.expect("video1 is listed once its init is built")
	}

	// An inline init bootstraps from a complete group, read from cache, rather than waiting on a
	// newer group the publisher is still writing.
	#[tokio::test(start_paused = true)]
	async fn an_init_bootstraps_from_a_complete_group() {
		let (origin, broadcast, _catalog, _video0) = inline_sibling().await;
		let media = broadcast.create_track("video1", timed()).unwrap();
		for sequence in 0..3 {
			let mut group = media.create_group(sequence.into()).unwrap();
			write_keyframe(&mut group, sequence);
			group.finish().unwrap();
		}
		let mut open = media.create_group(3u64.into()).unwrap();
		write_keyframe(&mut open, 3);

		let broadcaster = Broadcaster::spawned(crate::resolved(&origin, "live").await, Config::default());
		broadcaster.advertised().await;
		until_listed(&broadcaster.rendition(Kind::Video, "video0").unwrap(), 3).await;
		listed_in_manifest(&broadcaster).await;
		drop(open);
	}

	// A rendition whose init bootstraps from a still-open group never holds up the manifest: its
	// sibling renders at once, and it joins once the group closes. Its complete groups already
	// left the cache, so the open one is the only candidate left.
	#[tokio::test(start_paused = true)]
	async fn a_stuck_init_leaves_the_manifest_to_its_siblings() {
		let (origin, broadcast, _catalog, _video0) = inline_sibling().await;
		let (_media, mut open) = open_tail(&broadcast, "video1", 3);

		let broadcaster = Broadcaster::spawned(crate::resolved(&origin, "live").await, Config::default());
		broadcaster.advertised().await;
		until_listed(&broadcaster.rendition(Kind::Video, "video0").unwrap(), 3).await;
		kio::clock::timeout(Duration::from_millis(1), broadcaster.video_init())
			.await
			.expect("one video init is enough to render");
		let manifest = broadcaster.manifest(None).unwrap().expect("video0 renders");
		assert!(manifest.contains("id=\"video/video0\""), "{manifest}");
		assert!(!manifest.contains("id=\"video/video1\""), "{manifest}");

		// The background build is parked on the open group, and every render stays current.
		kio::clock::sleep(Duration::from_secs(1)).await;
		let manifest = broadcaster.manifest(None).unwrap().expect("video0 renders");
		assert!(!manifest.contains("id=\"video/video1\""), "{manifest}");

		open.finish().unwrap();
		let manifest = listed_in_manifest(&broadcaster).await;
		assert!(manifest.contains("id=\"video/video0\""), "{manifest}");
	}

	// A failed rendition answers the manifest only when no sibling can list: one whose init is
	// still building lists later, so until then nothing renders rather than the error.
	#[tokio::test(start_paused = true)]
	async fn a_failed_rendition_waits_for_a_sibling_init() {
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").unwrap();
		broadcast.announce(Default::default()).unwrap();
		settle().await;
		let mut catalog =
			moq_mux::catalog::Producer::new(&mut broadcast.broadcast().clone(), Default::default()).unwrap();
		catalog
			.mutate(|catalog| {
				catalog.media.join = hang::catalog::Join::new(INTERVAL);
				for name in ["video0", "video1"] {
					let config = if name == "video1" { inline_vp8() } else { video_config() };
					catalog.video.renditions.insert(name.into(), config);
				}
			})
			.unwrap();
		let mut video0 = Gops {
			track: broadcast.create_track("video0", timed()).unwrap(),
		};
		for sequence in 0..4 {
			video0.gop(sequence);
		}
		// video1's only cached group is still open, so its init build parks on it.
		let (video1, mut open) = open_tail(&broadcast, "video1", 3);

		let broadcaster = Broadcaster::spawned(crate::resolved(&origin, "live").await, Config::default());
		broadcaster.advertised().await;
		let broken = broadcaster.rendition(Kind::Video, "video0").unwrap();
		until_listed(&broken, 3).await;
		video0.cut();
		failed(&broken).await;
		assert!(broadcaster.manifest(None).unwrap().is_none());

		open.finish().unwrap();
		let manifest = kio::clock::timeout(Duration::from_secs(5), async {
			loop {
				if let Some(manifest) = broadcaster.manifest(None).unwrap() {
					return manifest;
				}
				kio::clock::sleep(Duration::from_millis(10)).await;
			}
		})
		.await
		.expect("video1 is listed once its init is built");
		assert!(manifest.contains("id=\"video/video1\""), "{manifest}");
		assert!(!manifest.contains("id=\"video/video0\""), "{manifest}");
		drop((catalog, video1));
	}

	// A background build parked on an open group never holds its rendition, so dropping the
	// broadcaster releases it.
	#[tokio::test(start_paused = true)]
	async fn a_stuck_init_build_never_outlives_its_rendition() {
		let (origin, broadcast, _catalog, _video0) = inline_sibling().await;
		let (_media, open) = open_tail(&broadcast, "video1", 3);

		let broadcaster = Broadcaster::spawned(crate::resolved(&origin, "live").await, Config::default());
		broadcaster.advertised().await;
		until_listed(&broadcaster.rendition(Kind::Video, "video0").unwrap(), 3).await;
		let stuck = broadcaster.rendition(Kind::Video, "video1").unwrap();
		stuck.start_init();
		kio::clock::sleep(Duration::from_secs(1)).await;
		assert!(stuck.init_pending(), "the build is parked on the open group");
		let weak = Arc::downgrade(&stuck);
		drop(stuck);

		drop(broadcaster);
		settle().await;
		assert!(weak.upgrade().is_none(), "the build kept its rendition alive");
		drop(open);
	}

	// A synchronous render may run on a thread outside the runtime, and still starts the
	// background init build there.
	#[tokio::test(start_paused = true)]
	async fn a_manifest_renders_off_the_runtime() {
		let (origin, broadcast, _catalog, _video0) = inline_sibling().await;
		let (_media, open) = open_tail(&broadcast, "video1", 3);

		let broadcaster = Broadcaster::spawned(crate::resolved(&origin, "live").await, Config::default());
		broadcaster.advertised().await;
		until_listed(&broadcaster.rendition(Kind::Video, "video0").unwrap(), 3).await;
		let render = broadcaster.clone();
		std::thread::spawn(move || render.manifest(None).map(|_| ()))
			.join()
			.expect("the render panicked")
			.unwrap();
		let video1 = broadcaster.rendition(Kind::Video, "video1").unwrap();
		assert!(video1.init_pending(), "the render started no init build");
		drop(open);
	}

	// A new generation releases a build parked on the old run's open group, so the new run can
	// build its own init.
	#[tokio::test(start_paused = true)]
	async fn a_new_generation_releases_a_stuck_init_build() {
		let (origin, broadcast, _catalog, _video0) = inline_sibling().await;
		let (_media, open) = open_tail(&broadcast, "video1", 3);

		let broadcaster = Broadcaster::spawned(crate::resolved(&origin, "live").await, Config::default());
		broadcaster.advertised().await;
		broadcaster.set_generation(Some("run1")).unwrap();
		until_listed(&broadcaster.rendition(Kind::Video, "video0").unwrap(), 3).await;
		let stuck = broadcaster.rendition(Kind::Video, "video1").unwrap();
		stuck.start_init();
		kio::clock::sleep(Duration::from_secs(1)).await;
		assert!(stuck.init_pending(), "the build is parked on the open group");

		broadcaster.set_generation(Some("run2")).unwrap();
		kio::clock::sleep(Duration::from_secs(1)).await;
		assert!(!stuck.init_pending(), "the old run's build still holds the init lock");
		drop(open);
	}

	// An ended broadcast stays dynamic while an enabled rendition's init is still building, so a
	// player keeps refreshing until it is listed instead of loading a static presentation
	// without it.
	#[tokio::test(start_paused = true)]
	async fn an_ended_manifest_stays_dynamic_while_an_init_builds() {
		let (origin, broadcast, mut catalog, mut video0) = inline_sibling().await;
		let (media, mut open) = open_tail(&broadcast, "video1", 3);

		let broadcaster = Broadcaster::spawned(crate::resolved(&origin, "live").await, Config::default());
		broadcaster.advertised().await;
		let rendition = broadcaster.rendition(Kind::Video, "video0").unwrap();
		until_listed(&rendition, 3).await;
		video0.finish();
		media.finish().unwrap();
		catalog.finish().unwrap();
		kio::clock::timeout(Duration::from_secs(5), async {
			while !rendition.snapshot().finished {
				kio::clock::sleep(Duration::from_millis(10)).await;
			}
		})
		.await
		.expect("the playlist ends with the source");

		let manifest = broadcaster.manifest(None).unwrap().expect("video0 renders");
		assert!(!manifest.contains("id=\"video/video1\""), "{manifest}");
		assert!(!manifest.contains("type=\"static\""), "{manifest}");

		open.finish().unwrap();
		let manifest = listed_in_manifest(&broadcaster).await;
		assert!(manifest.contains("type=\"static\""), "{manifest}");
	}

	// The first manifest waits for a video it can list: not for an init alone, which a sibling
	// with no listed segment has at once, and not only until the catalog closes.
	#[tokio::test(start_paused = true)]
	async fn the_first_video_wait_needs_a_listable_video() {
		// The inline `video0` lists a segment; `video1`'s init builds from the catalog alone, but
		// it lists nothing.
		let (origin, broadcast, mut catalog, _video1) = inline_pair("video0").await;
		// Its earlier groups already left the cache, so the open one is the only candidate.
		let (_media, mut open) = open_tail(&broadcast, "video0", 1);

		let broadcaster = Broadcaster::spawned(crate::resolved(&origin, "live").await, Config::default());
		let _ = kio::clock::timeout(Duration::from_secs(5), broadcaster.ready()).await;
		until_listed(&broadcaster.rendition(Kind::Video, "video0").unwrap(), 1).await;
		catalog.finish().unwrap();
		settle().await;

		let mut waiting = std::pin::pin!(broadcaster.video_init());
		assert!(
			kio::clock::timeout(Duration::from_secs(1), waiting.as_mut())
				.await
				.is_err(),
			"resolved without a listable video"
		);
		open.finish().unwrap();
		kio::clock::timeout(Duration::from_secs(1), waiting)
			.await
			.expect("video0 is listable once its init is built");
	}

	// An inline video with nothing listed when the wait starts has nothing to build from yet.
	// The wait starts its build once its segments are listed, rather than timing out without video.
	#[tokio::test(start_paused = true)]
	async fn the_first_video_wait_builds_an_init_listed_while_waiting() {
		let (origin, broadcast, _catalog, _video1) = inline_pair("video0").await;
		let media = broadcast.create_track("video0", timed()).unwrap();

		let broadcaster = Broadcaster::spawned(crate::resolved(&origin, "live").await, Config::default());
		while broadcaster.rendition(Kind::Video, "video0").is_none() {
			kio::clock::sleep(Duration::from_millis(10)).await;
		}
		let mut waiting = std::pin::pin!(broadcaster.video_init());
		assert!(
			kio::clock::timeout(Duration::from_secs(1), waiting.as_mut())
				.await
				.is_err(),
			"resolved without a listable video"
		);

		for sequence in 0..3 {
			let mut group = media.create_group(sequence.into()).unwrap();
			write_keyframe(&mut group, sequence);
			group.finish().unwrap();
		}
		kio::clock::timeout(Duration::from_secs(1), waiting)
			.await
			.expect("the wait builds video0's init once its segments are listed");
	}

	// The manifest leaves a disabled rendition out, so it never fetches media for its init.
	#[tokio::test(start_paused = true)]
	async fn a_disabled_rendition_never_builds_its_init() {
		let (origin, broadcast, mut catalog, _video0) = inline_sibling().await;
		catalog
			.mutate(|catalog| catalog.video.renditions.get_mut("video1").unwrap().enabled = false)
			.unwrap();
		let media = broadcast.create_track("video1", timed()).unwrap();
		for sequence in 0..4 {
			let mut group = media.create_group(sequence.into()).unwrap();
			write_keyframe(&mut group, sequence);
			group.finish().unwrap();
		}

		let broadcaster = Broadcaster::spawned(crate::resolved(&origin, "live").await, Config::default());
		broadcaster.advertised().await;
		until_listed(&broadcaster.rendition(Kind::Video, "video0").unwrap(), 3).await;
		for _ in 0..3 {
			assert!(broadcaster.manifest(None).unwrap().is_some());
			kio::clock::sleep(Duration::from_secs(1)).await;
		}
		let disabled = broadcaster.rendition(Kind::Video, "video1").unwrap();
		assert!(!disabled.snapshot().segments.is_empty());
		assert!(
			disabled.media_playlist(None).unwrap().is_none(),
			"no init was built for the disabled rendition"
		);

		catalog
			.mutate(|catalog| catalog.video.renditions.get_mut("video1").unwrap().enabled = true)
			.unwrap();
		listed_in_manifest(&broadcaster).await;
	}

	// A reconfigure that changes the init bytes changes the init URL, so a cache can never hand a
	// player the previous init under the new one. The old URL keeps naming the old bytes while the
	// playlist still lists the segments muxed under them.
	#[tokio::test]
	async fn a_reconfigure_versions_the_init_url() {
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").expect("publish allowed");
		broadcast.announce(Default::default()).expect("publish allowed");
		settle().await;
		let (_catalog, mut media) = publish_vp8(&mut broadcast.broadcast().clone(), video_config());

		let broadcaster = Broadcaster::spawned(crate::resolved(&origin, "live").await, Config::default());
		let _ = kio::clock::timeout(Duration::from_secs(5), broadcaster.ready()).await;
		let old = broadcaster.rendition(Kind::Video, "video0").expect("rendition");
		let old_playlist = old
			.media_playlist(None)
			.unwrap()
			.expect("an out-of-band init renders without a fetch");
		let old_hash = init_hash(&old_playlist).to_string();

		let mut config = video_config();
		config.coded_width = Some(640);
		config.coded_height = Some(480);
		media.set(config).unwrap();
		// The replacement lists only segments after the reconfigure.
		for micros in [6_000_000, 8_000_000, 10_000_000] {
			media.write(vp8_frame(micros, true)).unwrap();
		}

		let deadline = kio::clock::now() + Duration::from_secs(5);
		let new = loop {
			let current = broadcaster.rendition(Kind::Video, "video0").expect("rendition");
			if !Arc::ptr_eq(&current, &old) {
				break current;
			}
			assert!(
				kio::clock::now() < deadline,
				"the reconfigure never rebuilt the rendition"
			);
			kio::clock::sleep(Duration::from_millis(10)).await;
		};
		let new_init = new.latest_init().await.unwrap().expect("init segment");
		let _ = kio::clock::timeout(Duration::from_secs(5), new.playable()).await;
		let new_playlist = new.media_playlist(None).unwrap().expect("playable");
		let maps: Vec<_> = new_playlist
			.split("#EXT-X-MAP:URI=\"init.")
			.skip(1)
			.map(|rest| rest.split_once(".mp4").unwrap().0)
			.collect();
		let new_hash = *maps.last().unwrap();
		assert_ne!(new_hash, old_hash, "different init bytes get a different URL");
		assert_eq!(maps, [old_hash.as_str(), new_hash], "{new_playlist}");
		assert_eq!(new.init(new_hash).await.unwrap(), Some(new_init));
		assert_eq!(
			new.init(&old_hash).await.unwrap(),
			old.latest_init().await.unwrap(),
			"the predecessor's segments still listed map its init"
		);
	}

	// Background work ends with its owners, not with the publisher: once a reconfigure replaced
	// the rendition, dropping the broadcaster and both renditions finishes the driver while the
	// media tracks stay open.
	#[tokio::test(start_paused = true)]
	async fn the_driver_finishes_with_its_owners() {
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").expect("publish allowed");
		broadcast.announce(Default::default()).expect("publish allowed");
		settle().await;
		let (_catalog, mut media) = publish_vp8(&mut broadcast.broadcast().clone(), video_config());

		let (broadcaster, driver) =
			Broadcaster::new(crate::resolved(&origin, "live").await, Config::default()).unwrap();
		let driving = tokio::spawn(driver.run());
		let _ = kio::clock::timeout(Duration::from_secs(5), broadcaster.ready()).await;
		let old = broadcaster.rendition(Kind::Video, "video0").expect("rendition");

		let mut config = video_config();
		config.coded_width = Some(640);
		media.set(config).unwrap();
		media.write(vp8_frame(6_000_000, true)).unwrap();
		let new = kio::clock::timeout(Duration::from_secs(5), async {
			loop {
				let current = broadcaster.rendition(Kind::Video, "video0").expect("rendition");
				if !Arc::ptr_eq(&current, &old) {
					break current;
				}
				kio::clock::sleep(Duration::from_millis(10)).await;
			}
		})
		.await
		.expect("the reconfigure rebuilt the rendition");

		drop((broadcaster, new));
		settle().await;
		assert!(!driving.is_finished(), "a held rendition keeps the driver running");
		drop(old);
		kio::clock::timeout(Duration::from_secs(5), driving)
			.await
			.expect("the driver finishes once its owners drop")
			.unwrap();
		drop(media);
	}

	// A generation rides every segment URL. The first one labels the run already listed;
	// replacing it drops that run's rows, and the next run lists under the new generation.
	#[tokio::test]
	async fn a_generation_versions_segment_urls_and_a_new_one_resets_the_window() {
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").expect("publish allowed");
		broadcast.announce(Default::default()).expect("publish allowed");
		settle().await;
		let (_catalog, mut media) = publish_vp8(&mut broadcast.broadcast().clone(), video_config());

		let broadcaster = Broadcaster::spawned(crate::resolved(&origin, "live").await, Config::default());
		let _ = kio::clock::timeout(Duration::from_secs(5), broadcaster.ready()).await;
		let rendition = broadcaster.rendition(Kind::Video, "video0").expect("rendition");
		let tag = rendition.tag().to_string();
		let unversioned = rendition.media_playlist(None).unwrap().expect("playable");
		assert!(unversioned.contains(&format!("\nseg/{tag}.0.m4s\n")), "{unversioned}");

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
		assert!(labeled.contains(&format!("\nseg/run-1.{tag}.0.m4s\n")));
		assert!(labeled.contains(&format!("\nseg/run-1.{tag}.1.m4s\n")));
		assert_eq!(
			init_hash(&labeled),
			init_hash(&unversioned),
			"the init needs no generation"
		);
		let manifest = broadcaster.manifest(None).unwrap().expect("manifest");
		let run = broadcaster.instance();
		assert!(manifest.contains(&format!("media=\"@{run}/video/video0/seg/run-1.{tag}.t$Time$.m4s\"")));

		broadcaster.set_generation(Some("run-2")).unwrap();
		assert!(
			rendition.media_playlist(None).unwrap().is_none(),
			"a new generation drops the old run's rows"
		);

		media.write(vp8_frame(6_000_000, true)).unwrap();
		let _ = kio::clock::timeout(Duration::from_secs(5), rendition.playable()).await;
		let restarted = rendition.media_playlist(None).unwrap().expect("the new run lists");
		assert!(restarted.contains(&format!("\nseg/run-2.{tag}.2.m4s\n")), "{restarted}");
		assert!(!restarted.contains("run-1"));

		broadcaster.set_generation(None).unwrap();
		assert!(
			rendition.media_playlist(None).unwrap().is_none(),
			"clearing the generation is a change too"
		);
	}

	// A replaced instance's ended playlists keep their URLs: a generation set after the cut would
	// otherwise relabel its frozen rows under a run they never belonged to.
	#[tokio::test]
	async fn a_replaced_instance_keeps_its_generation() {
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").expect("publish allowed");
		broadcast.announce(Default::default()).expect("publish allowed");
		settle().await;
		let (_catalog, _media) = publish_vp8(&mut broadcast.broadcast().clone(), video_config());

		let broadcaster = Broadcaster::spawned(crate::resolved(&origin, "live").await, Config::default());
		let _ = kio::clock::timeout(Duration::from_secs(5), broadcaster.ready()).await;
		let rendition = broadcaster.rendition(Kind::Video, "video0").expect("rendition");
		let tag = rendition.tag().to_string();
		broadcaster.set_generation(Some("run-1")).unwrap();
		broadcaster.replaced();
		let ended = rendition.media_playlist(None).unwrap().expect("the cut keeps the rows");
		assert!(ended.contains("#EXT-X-ENDLIST"), "{ended}");

		broadcaster.set_generation(Some("run-2")).unwrap();
		assert_eq!(rendition.media_playlist(None).unwrap().as_deref(), Some(ended.as_str()));
		assert!(ended.contains(&format!("\nseg/run-1.{tag}.0.m4s\n")), "{ended}");
	}

	// A restarted inline-codec publisher can carry new parameter sets under the same catalog, so
	// a new generation drops the init built from the old run's media along with its rows.
	#[tokio::test]
	async fn a_new_generation_rebuilds_an_inline_init() {
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").expect("publish allowed");
		broadcast.announce(Default::default()).expect("publish allowed");
		settle().await;
		let catalog = hls_catalog(&mut broadcast.broadcast().clone());
		let mut media = video(&catalog, &broadcast, "video0", inline_vp8());
		for micros in [0, 2_000_000, 4_000_000] {
			media.write(vp8_frame(micros, true)).unwrap();
		}

		let broadcaster = Broadcaster::spawned(crate::resolved(&origin, "live").await, Config::default());
		let _ = kio::clock::timeout(Duration::from_secs(5), broadcaster.ready()).await;
		let rendition = broadcaster.rendition(Kind::Video, "video0").expect("rendition");
		let _ = kio::clock::timeout(Duration::from_secs(5), rendition.playable()).await;
		let old = rendition.latest_init().await.unwrap().expect("init segment");
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
		let _ = kio::clock::timeout(Duration::from_secs(5), rendition.playable()).await;
		let new = rendition.latest_init().await.unwrap().expect("init segment");
		assert_ne!(new, old, "the new run's init is built from its own media");
		let playlist = rendition.media_playlist(None).unwrap().expect("playable");
		assert_eq!(rendition.init(init_hash(&playlist)).await.unwrap(), Some(new));
	}

	#[tokio::test]
	async fn serves_playlist_and_segments_from_the_media() {
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").expect("publish allowed");
		broadcast.announce(Default::default()).expect("publish allowed");
		settle().await;
		// Three GOPs, 2s apart: segments 0 and 1 are complete, segment 2 is the live edge.
		let (catalog, media) = publish_vp8(&mut broadcast.broadcast().clone(), inline_vp8());

		let broadcaster = Broadcaster::spawned(crate::resolved(&origin, "live").await, Config::default());
		let _ = kio::clock::timeout(Duration::from_secs(5), broadcaster.ready()).await;
		let rendition = broadcaster
			.rendition(Kind::Video, "video0")
			.expect("rendition discovered from the catalog");
		let _ = kio::clock::timeout(Duration::from_secs(5), rendition.playable()).await;

		let master = broadcaster.master_playlist(None);
		assert!(master.contains("video/video0/media.m3u8"), "master lists the rendition");

		let playlist = rendition.snapshot();
		assert_eq!(numbers(&playlist), [0, 1], "the live-edge segment is not listed");
		assert_eq!(playlist.segments[0].duration, Duration::from_secs(2));
		assert_eq!(playlist.target_duration, 4, "twice the interval");
		assert!(!playlist.finished);

		// Without coded dimensions VP8 learns its init from a keyframe, and the playlist names
		// the init by hash, so nothing renders until the init is built.
		assert!(rendition.media_playlist(None).unwrap().is_none());
		let init = rendition.latest_init().await.unwrap().expect("init segment");
		assert_eq!(&init[4..8], b"ftyp");

		let rendered = rendition.media_playlist(None).unwrap().expect("playable");
		let hash = init_hash(&rendered);
		let tag = rendition.tag();
		assert!(rendered.contains(&format!("seg/{tag}.0.m4s\n")));
		assert!(rendered.contains(&format!("seg/{tag}.1.m4s\n")));
		assert_eq!(rendition.init(hash).await.unwrap(), Some(init));
		assert!(rendition.init("0000000000000000").await.unwrap().is_none());

		// The same render, but carrying a credential into every child URL.
		let signed = rendition
			.media_playlist(Some("jwt=abc.def"))
			.unwrap()
			.expect("playable");
		assert!(signed.contains(&format!("#EXT-X-MAP:URI=\"init.{hash}.mp4?jwt=abc.def\"\n")));
		assert!(signed.contains(&format!("seg/{tag}.0.m4s?jwt=abc.def\n")));

		let segment = rendition
			.listed_segment(0)
			.await
			.unwrap()
			.expect("segment fetched on demand");
		assert_eq!(&segment[4..8], b"moof", "a fetched group transmuxes to moof+mdat");

		// The live-edge segment isn't listed yet, and unknown segments miss.
		assert!(rendition.listed_segment(2).await.unwrap().is_none());
		assert!(rendition.listed_segment(99).await.unwrap().is_none());

		drop((media, catalog, broadcast));
	}

	// A fetched segment's last sample runs to where the next group starts, as its row says,
	// rather than to a guess from the framerate.
	#[tokio::test]
	async fn a_segment_ends_where_its_record_says() {
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").expect("publish allowed");
		broadcast.announce(Default::default()).expect("publish allowed");
		settle().await;
		let (catalog, media) = publish_vp8(&mut broadcast.broadcast().clone(), video_config());

		let broadcaster = Broadcaster::spawned(crate::resolved(&origin, "live").await, Config::default());
		let _ = kio::clock::timeout(Duration::from_secs(5), broadcaster.ready()).await;
		let rendition = broadcaster.rendition(Kind::Video, "video0").expect("rendition");
		let _ = kio::clock::timeout(Duration::from_secs(5), rendition.playable()).await;

		// Segment 0 holds a keyframe at 0s and a delta at 1s; segment 1 starts at 2s.
		let segment = rendition.listed_segment(0).await.unwrap().expect("segment 0");
		let timescale = rendition.muxer().unwrap().timescale().as_u64();
		let durations = sample_durations(&segment);
		assert_eq!(durations.len(), 2);
		let total: u64 = durations.iter().map(|d| u64::from(d.expect("explicit duration"))).sum();
		assert_eq!(total, 2 * timescale, "the samples span the whole segment");

		drop((media, catalog, broadcast));
	}

	// A duration marker already times a video group's last sample, so the segment's end is not
	// pinned on an earlier sample instead.
	#[tokio::test]
	async fn a_timed_trailing_sample_keeps_its_duration() {
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").expect("publish allowed");
		broadcast.announce(Default::default()).expect("publish allowed");
		settle().await;
		let mut producer = broadcast.broadcast().clone();
		let catalog = hls_catalog(&mut producer);
		let track = producer.create_track("video0", timed()).unwrap();
		let container = moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video);
		let mut media = catalog.video(track, container, video_config()).unwrap();
		media.write(vp8_frame(0, true)).unwrap();
		media.write(vp8_frame(33_000, false)).unwrap();
		// The marker times the delta; the next segment starts on the grid at 2s.
		media
			.cut(Some(moq_net::Timestamp::from_micros(66_000).unwrap()))
			.unwrap();
		media.write(vp8_frame(2_000_000, true)).unwrap();
		media.write(vp8_frame(4_000_000, true)).unwrap();

		let broadcaster = Broadcaster::spawned(crate::resolved(&origin, "live").await, Config::default());
		let _ = kio::clock::timeout(Duration::from_secs(5), broadcaster.ready()).await;
		let rendition = broadcaster.rendition(Kind::Video, "video0").expect("rendition");
		let _ = kio::clock::timeout(Duration::from_secs(5), rendition.playable()).await;

		let segment = rendition.listed_segment(0).await.unwrap().expect("segment 0");
		let timescale = rendition.muxer().unwrap().timescale().as_u64();
		let frame = Some(u32::try_from(timescale * 33 / 1_000).unwrap());
		assert_eq!(sample_durations(&segment), [frame, frame]);

		drop((media, catalog, broadcast));
	}

	// The DASH half of the fetch-on-demand path: the manifest renders from the same segment
	// windows as the HLS playlists (dynamic while live, matching S entries across renditions),
	// and $Time$ addressing resolves a segment's pts to the same bytes its number does.
	#[tokio::test]
	async fn serves_dash_manifest_and_time_addressed_segments() {
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").expect("publish allowed");
		broadcast.announce(Default::default()).expect("publish allowed");
		settle().await;
		let catalog = hls_catalog(&mut broadcast.broadcast().clone());
		let mut video = video(&catalog, &broadcast, "video0", video_config());
		let mut audio = audio(&catalog, &broadcast, "audio0", opus());

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

		let broadcaster = Broadcaster::spawned(crate::resolved(&origin, "live").await, Config::default());
		let _ = kio::clock::timeout(Duration::from_secs(5), broadcaster.ready()).await;
		let video_rendition = broadcaster.rendition(Kind::Video, "video0").expect("video discovered");
		let audio_rendition = broadcaster.rendition(Kind::Audio, "audio0").expect("audio discovered");
		let _ = kio::clock::timeout(Duration::from_secs(5), video_rendition.playable()).await;
		let _ = kio::clock::timeout(Duration::from_secs(5), audio_rendition.playable()).await;

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
		let video_tag = broadcaster.rendition(Kind::Video, "video0").unwrap().tag().to_string();
		assert!(manifest.contains(&format!("media=\"@{run}/video/video0/seg/{video_tag}.t$Time$.m4s\"")));
		// Both renditions cut the same segment spans (the live edge is not listed).
		assert_eq!(manifest.matches("<S t=\"0\" d=\"2000\"/>").count(), 2, "{manifest}");
		assert_eq!(manifest.matches("<S t=\"2000\" d=\"2000\"/>").count(), 2);
		assert!(
			!manifest.contains("<S t=\"4000\""),
			"the live-edge segment is not listed"
		);
		assert_eq!(manifest.matches("<Period ").count(), 1);

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
		let init = video_rendition.latest_init().await.unwrap().expect("init segment");
		assert_eq!(video_rendition.init(hash).await.unwrap(), Some(init));
		assert!(signed.contains(&format!(
			"media=\"@{run}/video/video0/seg/{video_tag}.t$Time$.m4s?jwt=abc.def\""
		)));

		// $Time$ resolves to the same bytes the number does; unknown times miss.
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

		drop((video, audio, catalog, broadcast));
	}

	// The catalog root clock times EXT-X-PROGRAM-DATE-TIME and the DASH availabilityStartTime.
	#[tokio::test]
	async fn root_clock_times_playlists_and_manifest() {
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").expect("publish allowed");
		broadcast.announce(Default::default()).expect("publish allowed");
		settle().await;

		// PTS zero at exactly the moq epoch, so every timestamp maps to a fixed string.
		let wall = kio::clock::Wall::UNIX_EPOCH + Duration::from_millis(hang::catalog::MOQ_EPOCH_UNIX_MILLIS);
		let config = catalog_config()
			.with_clock(moq_mux::Clock::at(moq_net::time::Instant::now(), wall).expect("a representable wall"));
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast.broadcast().clone(), config).unwrap();
		let mut media = video(&catalog, &broadcast, "video0", video_config());
		media.write(frame(0, true)).unwrap();
		media.write(frame(2_000_000, true)).unwrap();
		media.write(frame(4_000_000, true)).unwrap(); // live edge

		let broadcaster = Broadcaster::spawned(crate::resolved(&origin, "live").await, Config::default());
		let _ = kio::clock::timeout(Duration::from_secs(5), broadcaster.ready()).await;
		let rendition = broadcaster.rendition(Kind::Video, "video0").expect("video discovered");
		let _ = kio::clock::timeout(Duration::from_secs(5), rendition.playable()).await;

		let snapshot = rendition.snapshot();
		assert_eq!(snapshot.segments[0].program_date_time, Some(wall));
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

		drop((media, catalog, broadcast));
	}

	// Without a catalog clock the DASH manifest still anchors, estimating pts 0 from the first
	// segment's arrival, and playlists simply omit EXT-X-PROGRAM-DATE-TIME.
	#[tokio::test]
	async fn manifest_falls_back_to_arrival_anchor_without_a_clock() {
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").expect("publish allowed");
		broadcast.announce(Default::default()).expect("publish allowed");
		settle().await;

		// Write the catalog directly: a moq-mux catalog producer always advertises its clock, as a
		// publisher on a host whose wall clock is set does.
		let mut snapshot = joined();
		snapshot.video.renditions.insert("video0".to_string(), video_config());
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
		let mut media = Gops {
			track: broadcast.create_track("video0", timed()).unwrap(),
		};
		for sequence in 0..3 {
			media.gop(sequence);
		}

		let broadcaster = Broadcaster::spawned(crate::resolved(&origin, "live").await, Config::default());
		let _ = kio::clock::timeout(Duration::from_secs(5), broadcaster.ready()).await;
		let rendition = broadcaster.rendition(Kind::Video, "video0").expect("video discovered");
		let _ = kio::clock::timeout(Duration::from_secs(5), rendition.playable()).await;

		assert_eq!(rendition.snapshot().segments[0].program_date_time, None);
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
			fragments.extend_from_slice(&muxer.fragment(i as u32, &[frame], None).unwrap());
		}

		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").expect("publish allowed");
		broadcast.announce(Default::default()).expect("publish allowed");
		settle().await;
		let catalog = hls_catalog(&mut broadcast.broadcast().clone());
		let mut import = moq_mux::container::fmp4::Import::new(broadcast.broadcast().clone(), catalog.reserve());
		import.decode(&init).unwrap();

		// Watch before the first fragment, as a live viewer would, so a catalog the moov
		// published on a provisional clock would stick.
		let broadcaster = Broadcaster::spawned(crate::resolved(&origin, "live").await, Config::default());
		assert!(
			next_event(&mut broadcaster.renditions()).await.is_none(),
			"the moov alone lists no rendition"
		);

		import.decode(&fragments).unwrap();
		let anchored = catalog.snapshot().media.clock.expect("the catalog advertises a clock");

		kio::clock::timeout(Duration::from_secs(5), broadcaster.ready())
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
		let playlist = kio::clock::timeout(Duration::from_secs(5), rendition.playlist(None))
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
	async fn a_live_fmp4_import_serves_segments_to_a_late_edge() {
		live_fmp4_segments(None, true).await;
	}

	async fn live_fmp4_segments(version: Option<&str>, late: bool) {
		use moq_mux::container::fmp4::mp4_atom::{self, DecodeMaybe, Encode};
		// Reuse the A/V descriptors; opaque samples exercise segment discovery and FETCH,
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
		let catalog = hls_catalog(&mut broadcast.broadcast().clone());
		let mut import = moq_mux::container::fmp4::Import::new(broadcast.broadcast().clone(), catalog.reserve());
		import.decode(&init).unwrap();
		let snapshot = catalog.snapshot();
		let video_muxer =
			moq_mux::container::fmp4::Muxer::video(snapshot.video.renditions.values().next().unwrap()).unwrap();
		let audio_muxer =
			moq_mux::container::fmp4::Muxer::audio(snapshot.audio.renditions.values().next().unwrap()).unwrap();
		let consuming = remote.as_ref().unwrap_or(&origin);
		consuming
			.consume()
			.follow("live", None)
			.unwrap()
			.available()
			.await
			.unwrap();
		let source = crate::resolved(consuming, "live").await;
		let broadcaster = (!late).then(|| Broadcaster::spawned(source.clone(), Config::default()));
		for i in 0..20u64 {
			let fragment = video_muxer
				.fragment(i as u32, &[vp8_frame(i * 1_000_000, i % 2 == 0)], None)
				.unwrap();
			import.decode(&fragment).unwrap();
			let mut sample = frame(i * 1_000_000, true);
			sample.duration = Some(moq_net::Timestamp::from_secs(1).unwrap());
			let audio = audio_muxer.fragment(i as u32, &[sample], None).unwrap();
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
		let broadcaster = broadcaster.unwrap_or_else(|| Broadcaster::spawned(source, Config::default()));
		kio::clock::timeout(Duration::from_secs(5), broadcaster.ready())
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
			kio::clock::timeout(Duration::from_secs(5), rendition.playlist(None))
				.await
				.unwrap()
				.unwrap()
				.unwrap();
			let rows = rendition.snapshot().segments;
			assert!(rows.len() >= 2, "multiple completed segments are listed");
			for row in rows {
				assert!(!row.gap, "{kind:?} segment {} lines up", row.segment);
				let media = rendition.numbered(&row.tag, row.segment).await.unwrap();
				assert!(media.is_some(), "listed segment {} must have media", row.segment);
			}
			let mut cursor = rendition.segments();
			let segment = kio::clock::timeout(Duration::from_secs(5), cursor.next())
				.await
				.expect("a live recording produces a media segment")
				.unwrap();
			assert!(segment.is_some(), "the live cursor has media");
		}
		drop((import, catalog));
	}

	// A finished broadcast renders a static presentation.
	#[tokio::test]
	async fn dash_manifest_turns_static_when_finished() {
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").expect("publish allowed");
		broadcast.announce(Default::default()).expect("publish allowed");
		settle().await;
		let mut catalog = hls_catalog(&mut broadcast.broadcast().clone());
		let mut media = video(&catalog, &broadcast, "video0", video_config());
		media.write(frame(0, true)).unwrap();
		media.write(frame(2_000_000, true)).unwrap();

		let broadcaster = Broadcaster::spawned(crate::resolved(&origin, "live").await, Config::default());
		let _ = kio::clock::timeout(Duration::from_secs(5), broadcaster.ready()).await;
		let _ = kio::clock::timeout(Duration::from_secs(5), broadcaster.represented()).await;

		// A clean finish lists the open segment and ends the playlist.
		media.finish().unwrap();
		catalog.finish().unwrap();

		let deadline = kio::clock::now() + Duration::from_secs(5);
		let manifest = loop {
			let manifest = broadcaster.manifest(None).unwrap().expect("manifest renders");
			if manifest.contains(" type=\"static\"") {
				break manifest;
			}
			assert!(kio::clock::now() < deadline, "manifest never turned static");
			kio::clock::sleep(Duration::from_millis(50)).await;
		};
		// The trailing segment runs to the duration marker the finish wrote after its one frame.
		assert!(manifest.contains("<S t=\"0\" d=\"2000\"/>"), "{manifest}");
		assert!(
			manifest.contains("<S t=\"2000\""),
			"the bounded tail is listed: {manifest}"
		);
		assert!(!manifest.contains("presentationTimeOffset"));
		assert!(!manifest.contains("availabilityStartTime"));

		drop((catalog, media, broadcast));
	}

	// A broadcast without video segments on its audio alone.
	#[tokio::test]
	async fn an_audio_only_broadcast_segments_on_its_audio() {
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").expect("publish allowed");
		broadcast.announce(Default::default()).expect("publish allowed");
		settle().await;
		let catalog = hls_catalog(&mut broadcast.broadcast().clone());
		let mut audio = audio(&catalog, &broadcast, "audio0", opus());
		// 500ms groups pack into the 2s grid.
		for micros in (0..=6_000_000u64).step_by(500_000) {
			let keyframe = audio.needs_keyframe();
			audio.write(frame(micros, keyframe)).unwrap();
			audio.cut(None).unwrap();
		}

		let broadcaster = Broadcaster::spawned(crate::resolved(&origin, "live").await, Config::default());
		let _ = kio::clock::timeout(Duration::from_secs(5), broadcaster.ready()).await;
		let rendition = broadcaster.rendition(Kind::Audio, "audio0").expect("audio discovered");
		let _ = kio::clock::timeout(Duration::from_secs(5), rendition.playable()).await;

		let playlist = rendition.snapshot();
		assert_eq!(numbers(&playlist), [0, 1, 2]);
		assert_eq!(playlist.segments[0].duration, Duration::from_secs(2));
		let segment = rendition.listed_segment(1).await.unwrap().expect("audio segment");
		assert_eq!(&segment[4..8], b"moof");

		drop((audio, catalog, broadcast));
	}

	// Audio and video renditions whose groups start on the same grid lines cut the same
	// boundaries, with no state shared between them. Video is one group per segment; an audio
	// segment packs every audio group inside the video segment's span.
	#[tokio::test]
	async fn audio_and_video_on_one_grid_line_up() {
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").expect("publish allowed");
		broadcast.announce(Default::default()).expect("publish allowed");
		settle().await;
		let catalog = hls_catalog(&mut broadcast.broadcast().clone());
		let mut video = video(&catalog, &broadcast, "video0", video_config());
		let mut audio = audio(&catalog, &broadcast, "audio0", opus());

		// Interleaved by pts, as a muxer would demux them: video keyframes every 2s open the
		// segments; audio publishes a 500ms group each cut.
		let mut audio_write = |micros: u64| {
			let keyframe = audio.needs_keyframe();
			audio.write(frame(micros, keyframe)).unwrap();
			audio.cut(None).unwrap();
		};
		video.write(frame(0, true)).unwrap(); // segment 0
		for micros in [0u64, 500_000, 1_000_000, 1_500_000] {
			audio_write(micros);
		}
		video.write(frame(2_000_000, true)).unwrap(); // segment 1
		for micros in [2_000_000u64, 2_500_000, 3_000_000, 3_500_000] {
			audio_write(micros);
		}
		video.write(frame(4_000_000, true)).unwrap(); // segment 2 (live edge)
		audio_write(4_000_000);

		let broadcaster = Broadcaster::spawned(crate::resolved(&origin, "live").await, Config::default());
		let _ = kio::clock::timeout(Duration::from_secs(5), broadcaster.ready()).await;
		let video_rendition = broadcaster.rendition(Kind::Video, "video0").expect("video discovered");
		let audio_rendition = broadcaster.rendition(Kind::Audio, "audio0").expect("audio discovered");
		let _ = kio::clock::timeout(Duration::from_secs(5), video_rendition.playable()).await;
		let _ = kio::clock::timeout(Duration::from_secs(5), audio_rendition.playable()).await;

		// Both playlists list the same segment numbers over the same spans.
		let video_playlist = video_rendition.snapshot();
		let audio_playlist = audio_rendition.snapshot();
		assert_eq!(video_playlist.media_sequence, audio_playlist.media_sequence);
		assert_eq!(numbers(&video_playlist), [0, 1]);
		assert_eq!(numbers(&audio_playlist), [0, 1], "audio cuts the same segments");
		assert!(gaps(&audio_playlist).is_empty());
		assert_eq!(
			audio_playlist.segments[0].duration,
			Duration::from_secs(2),
			"cut at the video boundary"
		);

		// A video segment is one group; the matching audio segment transmuxes its four groups.
		let video_segment = video_rendition.listed_segment(1).await.unwrap().expect("video segment");
		assert_eq!(&video_segment[4..8], b"moof");
		let audio_segment = audio_rendition.listed_segment(1).await.unwrap().expect("audio segment");
		assert_eq!(&audio_segment[4..8], b"moof");
		assert!(
			audio_segment.len() > video_segment.len(),
			"the audio segment packs multiple groups into one fragment"
		);

		drop((video, audio, catalog, broadcast));
	}

	// Dropping the broadcaster must not truncate a recording in progress. A cursor holds its
	// rendition alive, so the rendition's own watcher still lists the final segment once the
	// publisher finishes.
	#[tokio::test(start_paused = true)]
	async fn dropping_the_broadcaster_keeps_a_cursor_drainable() {
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").expect("publish allowed");
		broadcast.announce(Default::default()).expect("publish allowed");
		settle().await;
		let mut catalog = hls_catalog(&mut broadcast.broadcast().clone());
		// Segment 0 is complete, while segment 1 stays open until the publisher finishes.
		let mut media = video(&catalog, &broadcast, "video0", video_config());
		media.write(frame(0, true)).unwrap();
		media.write(frame(2_000_000, true)).unwrap();
		media.write(frame(3_000_000, false)).unwrap();

		let (broadcaster, driver) =
			Broadcaster::new(crate::resolved(&origin, "live").await, Config::default()).unwrap();
		let driving = tokio::spawn(driver.run());
		let _ = kio::clock::timeout(Duration::from_secs(5), broadcaster.ready()).await;
		let rendition = broadcaster
			.rendition(Kind::Video, "video0")
			.expect("rendition discovered");
		let mut segments = rendition.segments();

		// The recorder drops the broadcaster before finalizing its uploads.
		drop(broadcaster);

		media.finish().unwrap();
		catalog.finish().unwrap();

		let mut listed = Vec::new();
		while let Some(segment) = kio::clock::timeout(Duration::from_secs(5), segments.next())
			.await
			.expect("the cursor drains without parking")
			.unwrap()
		{
			listed.push((segment.segment, segment.media.is_some()));
		}
		assert_eq!(
			listed,
			[(0, true), (1, true)],
			"the final segment survives dropping the broadcaster"
		);

		// The cursor holds its rendition, and the rendition its share of the driver.
		assert!(!driving.is_finished(), "the driver outlives a held rendition");
		drop((segments, rendition));
		kio::clock::timeout(Duration::from_secs(5), driving)
			.await
			.expect("the driver finishes once the cursor drops")
			.unwrap();
		drop((catalog, media, broadcast));
	}

	/// A one-video-rendition catalog snapshot reconciled by hand, which is what `watch_catalog`
	/// does with each snapshot: the rendition follows its media on its own.
	fn export(
		upstream: &moq_mux::Source,
		config: &hang::catalog::VideoConfig,
	) -> (renditions::Producer, Arc<Rendition>) {
		let config = config.clone();
		let mut catalog = joined();
		catalog.video.renditions.insert("video0".to_string(), config);
		let renditions = renditions::Producer::spawned(Config::default().window);
		renditions.sync(upstream, &catalog);
		let rendition = renditions.get(Kind::Video, "video0").expect("rendition synced");
		(renditions, rendition)
	}

	/// Write three keyframes of `payload` into `broadcast`'s `video0` track, 2s apart from
	/// `start_micros`.
	fn write_routed_media(
		broadcast: &mut moq_net::broadcast::Producer,
		payload: &'static [u8],
		start_micros: u64,
	) -> moq_mux::container::Producer<moq_mux::catalog::hang::Container> {
		let track = broadcast.create_track("video0", timed()).unwrap();
		let mut media = moq_mux::container::Producer::new(
			track,
			moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Data),
		);
		for group in 0..3 {
			let micros = start_micros + group * 2_000_000;
			media.write(payload_frame(micros, true, payload)).unwrap();
		}
		media
	}

	struct Ended {
		rendition: Arc<Rendition>,
		origin: moq_net::origin::Producer,
		_held: Box<dyn std::any::Any>,
	}

	/// Finish the media before End while retaining its track cache for a late recorder.
	async fn ended_rendition(sibling: bool) -> Ended {
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").unwrap();
		broadcast.announce(Default::default()).unwrap();
		let target = if sibling {
			let target = origin.create_broadcast("media").unwrap();
			target.announce(Default::default()).unwrap();
			target
		} else {
			broadcast.clone()
		};
		let mut media = write_routed_media(&mut target.broadcast().clone(), KEYFRAME, 0);
		media.finish().unwrap();
		settle().await;

		let source = crate::resolved(&origin, "live").await;
		let mut config = inline_vp8();
		if sibling {
			config.broadcast = Some(moq_net::path::Relative::new("media").unwrap());
		}
		let (renditions, rendition) = export(&source, &config);
		until_finished(&rendition).await;
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
			origin,
			_held: Box::new((renditions, media, broadcast, target)),
		}
	}

	// First-time inline init bootstrap must use the bound track, even after End.
	#[tokio::test]
	async fn init_bootstraps_after_the_catalog_broadcast_ends() {
		tokio::time::pause();
		let ended = ended_rendition(false).await;
		let init = ended.rendition.latest_init().await.unwrap();
		assert!(contains(&init.expect("inline init bootstraps after End"), b"ftyp"));
	}

	#[tokio::test]
	async fn segments_drain_after_the_catalog_broadcast_ends() {
		tokio::time::pause();
		let ended = ended_rendition(false).await;
		let result = ended.rendition.listed_segment(0).await;
		assert!(result.unwrap().is_some(), "retained segment drains after End");
	}

	#[tokio::test]
	async fn init_bootstraps_after_the_sibling_broadcast_ends() {
		tokio::time::pause();
		let ended = ended_rendition(true).await;
		let result = ended.rendition.latest_init().await;
		assert!(result.unwrap().is_some(), "sibling init bootstraps after End");
	}

	#[tokio::test]
	async fn segments_drain_after_the_sibling_broadcast_ends() {
		tokio::time::pause();
		let ended = ended_rendition(true).await;
		let result = ended.rendition.listed_segment(0).await;
		assert!(result.unwrap().is_some(), "retained sibling segment drains after End");
	}

	// A replacement is readable on a fresh binding, while the old rendition bootstraps
	// its first init and drains only the old instance's retained groups.
	#[tokio::test]
	async fn retained_media_after_replacement_keeps_its_original_instance() {
		tokio::time::pause();
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
			let mut muxer = moq_mux::container::fmp4::Muxer::video(&inline_vp8()).unwrap();
			let frames = muxer.read(&mut group).await.unwrap();
			assert!(frames.iter().any(|frame| frame.payload.as_ref() == NEW));
			let next_init = muxer.init().unwrap().unwrap();
			let old_init = ended.rendition.latest_init().await.unwrap().unwrap();
			assert_ne!(
				old_init, next_init,
				"the old inline init must not acquire replacement geometry"
			);

			let mut cursor = ended.rendition.segments();
			let mut drained = Vec::new();
			while let Some(segment) = cursor.next().await.unwrap() {
				if let Some(media) = &segment.media {
					assert!(contains(media, KEYFRAME));
					assert!(!contains(media, NEW));
				}
				drained.push((segment.segment, segment.media.is_some()));
			}
			assert_eq!(
				drained,
				[(0, true), (1, true), (2, true)],
				"the recorder drains the complete retained tail"
			);
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
		// carry `payload`, so the two publishers' media differs byte for byte. The returned handle
		// keeps the publisher alive until it is dropped.
		fn publish(origin: &moq_net::origin::Producer, payload: &'static [u8]) -> Box<dyn std::any::Any> {
			let broadcast = origin.create_broadcast("live").expect("publish allowed");
			broadcast.announce(Default::default()).expect("publish allowed");
			let catalog = hls_catalog(&mut broadcast.broadcast().clone());
			let mut media = video(&catalog, &broadcast, "video0", video_config());
			media.write(payload_frame(0, true, payload)).unwrap();
			media.write(payload_frame(2_000_000, true, payload)).unwrap();
			media.write(payload_frame(4_000_000, true, payload)).unwrap();
			Box::new((broadcast, catalog, media))
		}

		let origin = produce_origin();
		let old = publish(&origin, OLD);
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
		let new = publish(&origin, NEW);
		settle().await;

		let (_renditions, rendition) = export(&upstream, &video_config());
		let _ = kio::clock::timeout(Duration::from_secs(1), rendition.playable()).await;
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

		// Control: an export started after the takeover does serve the new publisher's media, so
		// the assertion above is about which instance a rendition is bound to, not about a broadcast
		// that happens to be unreadable.
		let fresh = crate::resolved(&origin, "live").await;
		let (_renditions, rendition) = export(&fresh, &video_config());
		kio::clock::timeout(Duration::from_secs(5), rendition.playable())
			.await
			.expect("the new publisher's media arrives");
		let served = rendition
			.listed_segment(0)
			.await
			.unwrap()
			.expect("the new publisher is servable on its own catalog");
		assert!(
			contains(&served, NEW),
			"the new export serves the new publisher's media"
		);

		drop(new);
	}

	// The sibling half of the case above. A rendition whose catalog names another broadcast is
	// bound to whatever serves that path when the rendition is created, not when its first
	// segment is requested: its rows keep describing the instance they were read from, so a media
	// publisher that reconnects (restarting its group numbering) must not answer for them.
	#[tokio::test]
	async fn a_replacement_sibling_is_not_served_under_the_replaced_catalog() {
		const OLD: &[u8] = b"OLDOLDOLDOLDOLDO";
		const NEW: &[u8] = b"NEWNEWNEWNEWNEWN";

		fn publish_media(origin: &moq_net::origin::Producer, payload: &'static [u8]) -> Box<dyn std::any::Any> {
			let broadcast = origin.create_broadcast("media").expect("publish allowed");
			broadcast.announce(Default::default()).expect("publish allowed");
			let media = write_routed_media(&mut broadcast.broadcast().clone(), payload, 0);
			Box::new((broadcast, media))
		}

		let origin = produce_origin();

		// The catalog broadcast carries the catalog; the media lives next door.
		let live = origin.create_broadcast("live").expect("publish allowed");
		live.announce(Default::default()).expect("publish allowed");
		let old_media = publish_media(&origin, OLD);
		settle().await;

		let upstream = crate::resolved(&origin, "live").await;

		// A relative reference replaces the base's last segment, so "media" is a sibling of the
		// catalog broadcast "live".
		let mut config = video_config();
		config.broadcast = Some(moq_net::path::Relative::new("media").unwrap());

		let (_renditions, rendition) = export(&upstream, &config);
		kio::clock::timeout(Duration::from_secs(5), rendition.playable())
			.await
			.expect("the media reaches the rendition");

		// The media publisher goes away and comes back before any segment is requested, as a
		// brand new broadcast whose group numbering restarts from zero.
		drop(old_media);
		settle().await;
		let new_media = publish_media(&origin, NEW);
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

		// Control: an export started after the takeover does serve the new publisher's media.
		let fresh = crate::resolved(&origin, "live").await;
		let (_renditions, rendition) = export(&fresh, &config);
		kio::clock::timeout(Duration::from_secs(5), rendition.playable())
			.await
			.expect("the media reaches the fresh rendition");
		let served = rendition
			.listed_segment(0)
			.await
			.unwrap()
			.expect("the new publisher is servable on a freshly bound rendition");
		assert!(
			contains(&served, NEW),
			"the new export serves the new publisher's media"
		);

		drop((new_media, live));
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

	async fn until_finished(rendition: &Rendition) {
		let deadline = kio::clock::now() + Duration::from_secs(5);
		loop {
			if rendition.snapshot().finished {
				return;
			}
			assert!(kio::clock::now() < deadline, "the playlist did not end");
			tokio::task::yield_now().await;
		}
	}

	/// A sibling-media fixture: the catalog broadcast `live` lists `video0`, and its media lives in
	/// the sibling broadcast `media`.
	struct Sibling {
		origin: moq_net::origin::Producer,
		_live: moq_net::origin::Publication,
		upstream: moq_mux::Source,
		config: hang::catalog::VideoConfig,
	}

	impl Sibling {
		async fn new() -> Self {
			let origin = produce_origin();
			let live = origin.create_broadcast("live").expect("publish allowed");
			live.announce(Default::default()).expect("publish allowed");
			settle().await;
			let upstream = crate::resolved(&origin, "live").await;
			let mut config = video_config();
			config.broadcast = Some(moq_net::path::Relative::new("media").unwrap());
			Self {
				origin,
				_live: live,
				upstream,
				config,
			}
		}

		/// Publish the sibling's media, unannounced.
		fn media(
			&mut self,
			payload: &'static [u8],
			start_micros: u64,
		) -> (
			moq_net::origin::Publication,
			moq_mux::container::Producer<moq_mux::catalog::hang::Container>,
		) {
			let media = self.origin.create_broadcast("media").unwrap();
			let track = write_routed_media(&mut media.broadcast().clone(), payload, start_micros);
			(media, track)
		}
	}

	// A bound sibling ends the playlist while its retained rows drain. Another publisher at
	// that path is never bound, so its bytes cannot answer the old rows.
	#[tokio::test]
	async fn a_replaced_sibling_ends_the_rendition() {
		tokio::time::pause();
		const OLD: &[u8] = b"OLDOLDOLDOLDOLDO";
		const NEW: &[u8] = b"NEWNEWNEWNEWNEWN";

		let mut test = Sibling::new().await;
		let (old_media, old_track) = test.media(OLD, 0);
		settle().await;
		let (_renditions, rendition) = export(&test.upstream, &test.config);
		old_media.announce(sibling_route(10)).unwrap();
		kio::clock::timeout(Duration::from_secs(5), rendition.playable())
			.await
			.expect("the media reaches the rendition");
		let served = rendition
			.listed_segment(0)
			.await
			.unwrap()
			.expect("the original sibling is servable");
		assert!(contains(&served, OLD), "the first hop serves the original publisher");
		let mut early = rendition.segments();
		assert_eq!(early.next().await.unwrap().unwrap().discontinuity, 0);

		drop((old_media, old_track));
		until_finished(&rendition).await;
		let served = rendition
			.listed_segment(0)
			.await
			.unwrap()
			.expect("the retained old segment drains");
		assert!(contains(&served, OLD));
		while let Some(segment) = early.next().await.unwrap() {
			assert!(contains(&segment.media.expect("old media, not a gap"), OLD));
		}
		let before = rendition.snapshot().segments.len();

		// A publisher serving the path afterwards is never bound.
		let (new_media, new_track) = test.media(NEW, 6_000_000);
		new_media.announce(sibling_route(11)).unwrap();
		kio::clock::sleep(Duration::from_secs(1)).await;
		let snapshot = rendition.snapshot();
		assert_eq!(snapshot.segments.len(), before, "nothing of the replacement is listed");
		assert!(snapshot.finished);
		drop((new_media, new_track, test));
	}

	// A replacement already present when the export starts is the sibling the request
	// resolves to: it is served, not treated as a stale instance.
	#[tokio::test]
	async fn a_sibling_present_at_export_start_is_served() {
		const NEW: &[u8] = b"NEWNEWNEWNEWNEWN";

		let mut test = Sibling::new().await;
		let (media, _track) = test.media(NEW, 0);
		media.announce(sibling_route(11)).unwrap();
		settle().await;

		let (_renditions, rendition) = export(&test.upstream, &test.config);
		kio::clock::timeout(Duration::from_secs(5), rendition.playable())
			.await
			.expect("the media reaches the rendition");
		let served = rendition
			.listed_segment(0)
			.await
			.unwrap()
			.expect("the sibling present at export start is servable");
		assert!(contains(&served, NEW));
	}

	// The replacement is often announced after the incumbent is already gone. Nothing serves
	// the path in between, but a sibling that resolved once is never looked up again, so the
	// rendition stays ended rather than recovering onto the replacement.
	#[tokio::test]
	async fn a_replaced_sibling_stays_ended_across_an_unroutable_gap() {
		tokio::time::pause();
		const OLD: &[u8] = b"OLDOLDOLDOLDOLDO";
		const NEW: &[u8] = b"NEWNEWNEWNEWNEWN";

		let mut test = Sibling::new().await;
		let (old_media, old_track) = test.media(OLD, 0);
		settle().await;
		let (_renditions, rendition) = export(&test.upstream, &test.config);
		old_media.announce(sibling_route(10)).unwrap();
		kio::clock::timeout(Duration::from_secs(5), rendition.playable())
			.await
			.expect("the media reaches the rendition");
		assert!(rendition.listed_segment(0).await.unwrap().is_some());

		drop((old_media, old_track));
		until_finished(&rendition).await;
		let before = rendition.snapshot().segments.len();
		assert!(before > 0);

		let (new_media, new_track) = test.media(NEW, 6_000_000);
		new_media.announce(sibling_route(11)).unwrap();
		kio::clock::sleep(Duration::from_secs(1)).await;
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
		drop((new_media, new_track));
	}

	// A recorder waiting at the live edge ends when the bound sibling ends, with nothing else
	// (a playlist request) touching the rendition.
	#[tokio::test]
	async fn a_cursor_at_the_edge_ends_when_its_sibling_ends() {
		tokio::time::pause();
		const OLD: &[u8] = b"OLDOLDOLDOLDOLDO";

		let mut test = Sibling::new().await;
		let (media, track) = test.media(OLD, 0);
		settle().await;
		let (_renditions, rendition) = export(&test.upstream, &test.config);
		media.announce(sibling_route(10)).unwrap();
		kio::clock::timeout(Duration::from_secs(5), rendition.playable())
			.await
			.expect("the media reaches the rendition");

		let mut cursor = rendition.segments();
		loop {
			match kio::clock::timeout(Duration::from_secs(1), cursor.next()).await {
				Ok(Ok(Some(_))) => continue,
				Err(_) => break,
				other => panic!(
					"the cursor waits at the live edge: {:?}",
					other.map(|r| r.map(|s| s.is_some()))
				),
			}
		}

		// Only the sibling broadcast goes.
		drop(media);
		let end = kio::clock::timeout(Duration::from_secs(5), cursor.next())
			.await
			.expect("the cursor wakes when its sibling ends")
			.unwrap();
		assert!(end.is_none(), "the cursor ends with its sibling");
		drop(track);
	}

	// A sibling not yet announced when the rendition binds it is waited for: the binding resolves
	// on the sibling's first announcement, and its media is listed then.
	#[tokio::test]
	async fn a_sibling_announced_after_export_start_is_bound() {
		tokio::time::pause();
		const OLD: &[u8] = b"OLDOLDOLDOLDOLDO";

		let mut test = Sibling::new().await;
		let (media, _track) = test.media(OLD, 0);
		settle().await;
		let (_renditions, rendition) = export(&test.upstream, &test.config);
		assert!(
			kio::clock::timeout(Duration::from_secs(1), rendition.playable())
				.await
				.is_err(),
			"nothing is listed before the sibling resolves"
		);

		media.announce(sibling_route(10)).unwrap();
		kio::clock::timeout(Duration::from_secs(5), rendition.playable())
			.await
			.expect("the sibling's media is listed once it resolves");
		let served = rendition
			.listed_segment(0)
			.await
			.unwrap()
			.expect("the late sibling is servable");
		assert!(contains(&served, OLD));
	}

	// A rendition the catalog drops must end its segment cursors. The cursor holds an
	// `Arc<Rendition>`, so without an explicit end the rendition (and its media subscription)
	// would stay alive and the cursor would park at the live edge forever.
	#[tokio::test]
	async fn removing_a_rendition_ends_its_segment_cursor() {
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").expect("publish allowed");
		broadcast.announce(Default::default()).expect("publish allowed");
		settle().await;
		let upstream = crate::resolved(&origin, "live").await;

		// Drive the producer directly so the catalog can be reconciled synchronously.
		let (renditions, rendition) = export(&upstream, &inline_vp8());
		let mut segments = rendition.segments();

		// The catalog drops the rendition: its cursor must run dry rather than park.
		renditions.sync(&upstream, &joined());
		let ended = kio::clock::timeout(Duration::from_secs(5), segments.next())
			.await
			.expect("a removed rendition's cursor ends instead of parking")
			.unwrap();
		assert!(ended.is_none(), "no segments remained to drain");

		drop((broadcast, rendition));
	}

	// Dropping the broadcaster must release its renditions even when a cursor (and an
	// `Arc<Rendition>` it was handed) outlives it.
	#[tokio::test]
	async fn dropping_the_broadcaster_releases_its_renditions() {
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").expect("publish allowed");
		broadcast.announce(Default::default()).expect("publish allowed");
		settle().await;
		let catalog = hls_catalog(&mut broadcast.broadcast().clone());
		let mut media = video(&catalog, &broadcast, "video0", video_config());
		media.write(frame(0, true)).unwrap();
		media.write(frame(2_000_000, true)).unwrap();

		let broadcaster = Broadcaster::spawned(crate::resolved(&origin, "live").await, Config::default());

		let mut renditions = broadcaster.renditions();
		let added = kio::clock::timeout(Duration::from_secs(5), renditions.next())
			.await
			.expect("a rendition is discovered");
		let Ok(Some(renditions::Event::Added(_rendition))) = added else {
			panic!("expected an Added event");
		};

		// The cursor and the handed-out rendition both outlive the broadcaster.
		drop(broadcaster);

		let next = kio::clock::timeout(Duration::from_secs(5), renditions.next())
			.await
			.expect("the cursor resolves after the broadcaster drops");
		assert!(
			matches!(next, Ok(Some(renditions::Event::Removed { .. }))),
			"dropping the broadcaster releases its renditions to a live cursor"
		);

		// ...and the cursor then reaches a terminal state rather than parking.
		let ended = kio::clock::timeout(Duration::from_secs(5), renditions.next())
			.await
			.expect("the cursor terminates after the broadcaster drops");
		assert!(matches!(ended, Err(crate::Error::IncompleteRecording)));

		drop((catalog, media, broadcast));
	}

	// The recording path: the renditions cursor yields the video rendition, its segment cursor
	// yields each finalized segment with its media, and losing the publisher fails both cursors
	// instead of completing a truncated recording.
	#[tokio::test]
	async fn record_cursors_yield_renditions_and_segments() {
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").expect("publish allowed");
		broadcast.announce(Default::default()).expect("publish allowed");
		settle().await;
		let catalog = hls_catalog(&mut broadcast.broadcast().clone());
		// Segments 0 and 1 are complete; segment 2 is the live edge until the publisher drops.
		let mut media = video(&catalog, &broadcast, "video0", video_config());
		media.write(frame(0, true)).unwrap();
		media.write(frame(2_000_000, true)).unwrap();
		media.write(frame(4_000_000, true)).unwrap();

		let broadcaster = Broadcaster::spawned(crate::resolved(&origin, "live").await, Config::default());

		let mut renditions = broadcaster.renditions();
		let rendition = match kio::clock::timeout(Duration::from_secs(5), renditions.next())
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

		let first = kio::clock::timeout(Duration::from_secs(5), segments.next())
			.await
			.expect("first segment finalizes")
			.unwrap()
			.expect("a segment, not end");
		assert_eq!(first.segment, 0);
		let fragment = first.media.expect("media, not a gap");
		assert_eq!(&fragment[4..8], b"moof", "the segment carries its transmuxed media");
		assert_eq!(first.duration, Duration::from_secs(2));
		assert_eq!(first.discontinuity, 0, "a clean start is not a discontinuity");

		let second = segments.next().await.unwrap().expect("second segment");
		assert_eq!(second.segment, 1);
		assert_eq!(second.discontinuity, 0, "consecutive segments are continuous");

		// Tear down the publisher. The media ends abruptly, so the cursor fails rather than end
		// as if the recording were complete.
		drop((catalog, media));
		broadcast.close();

		let end = kio::clock::timeout(Duration::from_secs(5), segments.next())
			.await
			.expect("segment cursor resolves after the source is lost");
		// The record and catalog watchers race to see the close; either fails the recording.
		assert!(
			matches!(
				end,
				Err(crate::Error::MediaFailed { .. } | crate::Error::CatalogFailed { .. })
			),
			"an abrupt end fails the recording: {:?}",
			end.map(|segment| segment.map(|segment| segment.segment))
		);

		// The renditions cursor also fails when the source closes abruptly.
		let ended = kio::clock::timeout(Duration::from_secs(5), renditions.next())
			.await
			.expect("renditions cursor resolves");
		assert!(ended.is_err(), "an abrupt broadcast close fails the renditions cursor");
	}

	/// A media track written by hand: one keyframe group at each pts given.
	struct Gops {
		track: moq_net::track::Producer,
	}

	impl Gops {
		/// Write group `sequence` with a keyframe at `pts_ms` and, when `frames` is more than one,
		/// that many frames a tenth of the interval apart.
		fn group(&mut self, sequence: u64, pts_ms: u64, frames: u64) {
			let mut group = self.track.create_group(moq_net::group::Info { sequence }).unwrap();
			for index in 0..frames.max(1) {
				let micros = (pts_ms + index * INTERVAL.as_millis() as u64 / 10) * 1_000;
				let frame = vp8_frame(micros, index == 0);
				hang::container::Frame {
					timestamp: frame.timestamp,
					payload: frame.payload,
				}
				.write_to(&mut group)
				.unwrap();
			}
			group.finish().unwrap();
		}

		/// Write group `sequence` at `pts_ms` with a single keyframe.
		fn at(&mut self, sequence: u64, pts_ms: u64) {
			self.group(sequence, pts_ms, 1);
		}

		/// Write group `sequence` at `2 * sequence` seconds: one GOP per grid line.
		fn gop(&mut self, sequence: u64) {
			self.at(sequence, sequence * INTERVAL.as_millis() as u64);
		}

		/// Start group `sequence` at `pts_ms` with a keyframe, leaving it open for more frames.
		fn open(&mut self, sequence: u64, pts_ms: u64) -> moq_net::group::Producer {
			let mut group = self.track.create_group(moq_net::group::Info { sequence }).unwrap();
			let frame = vp8_frame(pts_ms * 1_000, true);
			hang::container::Frame {
				timestamp: frame.timestamp,
				payload: frame.payload,
			}
			.write_to(&mut group)
			.unwrap();
			group
		}

		/// The publisher loses its session: the track fails.
		fn cut(self) {
			self.track
				.abort(moq_net::Error::Transport("connection lost".into()))
				.unwrap();
		}

		/// The track ends cleanly, staying retained while the handle lives.
		fn finish(&mut self) {
			self.track.finish().unwrap();
		}
	}

	/// Renditions over hand-published media groups, reconciled the way `watch_catalog` does, on a
	/// two-second join interval.
	struct Groups {
		broadcast: moq_net::broadcast::Producer,
		catalog: moq_mux::catalog::hang::Catalog,
		renditions: renditions::Producer,
	}

	impl Groups {
		/// `video0` and `video1` renditions.
		fn new() -> Self {
			Self::with(&["video0", "video1"], &[])
		}

		/// `video` and `audio` renditions, each with its own media track.
		fn with(video: &[&str], audio: &[&str]) -> Self {
			Self::joined(video, audio, INTERVAL)
		}

		/// [`with`](Self::with), on a join interval of `interval`.
		fn joined(video: &[&str], audio: &[&str], interval: Duration) -> Self {
			let mut catalog = joined();
			catalog.media.join = hang::catalog::Join::new(interval);
			for name in video {
				let config = video_config();
				catalog.video.renditions.insert(name.to_string(), config);
			}
			for name in audio {
				let config = opus();
				catalog.audio.renditions.insert(name.to_string(), config);
			}
			let broadcast = moq_net::broadcast::Info::new().produce();
			let renditions = Self::export(&broadcast, &catalog);
			Self {
				broadcast,
				catalog,
				renditions,
			}
		}

		fn export(
			broadcast: &moq_net::broadcast::Producer,
			catalog: &moq_mux::catalog::hang::Catalog,
		) -> renditions::Producer {
			let renditions = renditions::Producer::spawned(Config::default().window);
			renditions.sync(&Self::source(broadcast), catalog);
			renditions
		}

		fn source(broadcast: &moq_net::broadcast::Producer) -> moq_mux::Source {
			moq_mux::Source::new(produce_origin().consume(), broadcast.consume())
		}

		/// Reconcile the current catalog again.
		fn sync(&self) {
			self.renditions.sync(&Self::source(&self.broadcast), &self.catalog);
		}

		/// Another exporter over the same media: a second edge, or one a fresh viewer starts.
		fn join(&self) -> Self {
			Self {
				broadcast: self.broadcast.clone(),
				catalog: self.catalog.clone(),
				renditions: Self::export(&self.broadcast, &self.catalog),
			}
		}

		/// Publish `track`'s media, written group by group.
		fn publish(&self, track: &str) -> Gops {
			Gops {
				track: self.broadcast.create_track(track, timed()).unwrap(),
			}
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
				kio::clock::sleep(Duration::from_millis(10)).await;
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
				kio::clock::sleep(Duration::from_millis(10)).await;
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
			let end = kio::clock::timeout(Duration::from_secs(5), async {
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

		/// Drain `name`'s recording cursor, which must answer a media failure once drained,
		/// returning the segments it yielded first.
		async fn drain_failed(&self, name: &str) -> Vec<u64> {
			let (drained, end) = self.drain(name).await;
			assert!(matches!(end, Err(crate::Error::MediaFailed { .. })), "{end:?}");
			drained
		}
	}

	/// Wait until `rendition`'s playlist fails, returning the error it answers.
	async fn failed(rendition: &Rendition) -> crate::Error {
		for _ in 0..500 {
			if let Err(err) = rendition.try_snapshot() {
				assert!(matches!(err, crate::Error::MediaFailed { .. }), "{err}");
				return err;
			}
			kio::clock::sleep(Duration::from_millis(10)).await;
		}
		panic!("{} never failed", rendition.name);
	}

	#[tokio::test(start_paused = true)]
	async fn a_segment_cursor_rejects_an_unfinished_window() {
		let test = Groups::new();
		let rendition = test.rendition("video0");
		let mut cursor = rendition.segments();
		rendition.close();
		assert!(matches!(cursor.next().await, Err(crate::Error::IncompleteRecording)));
		assert!(matches!(cursor.next().await, Err(crate::Error::IncompleteRecording)));
	}

	#[tokio::test(start_paused = true)]
	async fn a_failed_catalog_fails_unfinished_segment_windows() {
		let test = Groups::new();
		let rendition = test.rendition("video0");
		let mut segments = rendition.segments();
		test.renditions
			.finish(Err(crate::Error::Moq(moq_net::Error::Unroutable)));
		// Wrapped, so a recorder can tell the final failure from a retryable fetch error.
		for _ in 0..2 {
			let Err(err) = segments.next().await else {
				panic!("a failed catalog ends the cursor");
			};
			assert!(
				matches!(&err, crate::Error::CatalogFailed { source } if matches!(**source, crate::Error::Moq(moq_net::Error::Unroutable))),
				"{err}"
			);
		}
		// A rendition created after the failure starts failed.
		assert!(test.renditions.failure().is_some());
	}

	#[tokio::test(start_paused = true)]
	async fn a_slow_cursor_refuses_evicted_rows_before_its_first_read() {
		let test = Groups::with(&["video0"], &[]);
		let rendition = test.rendition("video0");
		let mut slow = rendition.segments();
		let mut video0 = test.publish("video0");
		for sequence in 0..21 {
			video0.gop(sequence);
		}
		assert!(test.listed("video0", 19).await.media_sequence > 0);
		assert!(matches!(slow.next().await, Err(crate::Error::RecordingLagged)));
		assert!(matches!(slow.next().await, Err(crate::Error::RecordingLagged)));
	}

	// A failed media track is not retried, and never papered over with gaps or ended as if
	// complete: its listings answer the error, while the other renditions go on.
	#[tokio::test(start_paused = true)]
	async fn a_media_error_fails_only_its_playlist() {
		let test = Groups::new();
		let mut video0 = test.publish("video0");
		let mut video1 = test.publish("video1");
		for sequence in 0..4 {
			video0.gop(sequence);
			video1.gop(sequence);
		}
		// The newest segment waits for a later one to start.
		assert_eq!(numbers(&test.listed("video1", 2).await), [0, 1, 2]);

		video1.cut();
		for sequence in 4..7 {
			video0.gop(sequence);
		}
		test.failed("video1").await;
		let video1 = test.rendition("video1");
		assert!(video1.media_playlist(None).is_err());
		assert!(video1.representation().is_err());
		assert!(video1.failed_alone());

		let video0 = test.listed("video0", 5).await;
		assert!(!video0.finished, "the other renditions go on");
		assert!(test.rendition("video0").representation().is_ok());
	}

	// A failed rendition leaves the broadcast-wide listings, so new players start on a sibling.
	#[tokio::test(start_paused = true)]
	async fn a_failed_rendition_leaves_the_manifest_and_master() {
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").unwrap();
		broadcast.announce(Default::default()).unwrap();
		settle().await;
		let mut catalog =
			moq_mux::catalog::Producer::new(&mut broadcast.broadcast().clone(), Default::default()).unwrap();
		catalog
			.mutate(|catalog| {
				catalog.media.join = hang::catalog::Join::new(INTERVAL);
				for name in ["video0", "video1"] {
					catalog.video.renditions.insert(name.into(), video_config());
				}
			})
			.unwrap();
		let publish = |name: &str| Gops {
			track: broadcast.create_track(name, timed()).unwrap(),
		};
		let mut video0 = publish("video0");
		let mut video1 = publish("video1");
		for sequence in 0..4 {
			video0.gop(sequence);
			video1.gop(sequence);
		}
		let broadcaster = Broadcaster::spawned(crate::resolved(&origin, "live").await, Config::default());
		broadcaster.advertised().await;
		let healthy = broadcaster.rendition(Kind::Video, "video0").unwrap();
		let broken = broadcaster.rendition(Kind::Video, "video1").unwrap();
		for rendition in [&healthy, &broken] {
			while rendition.snapshot().segments.len() < 3 {
				kio::clock::sleep(Duration::from_millis(10)).await;
			}
		}
		let before = broadcaster.manifest(None).unwrap().expect("both renditions are listed");
		assert!(before.contains("id=\"video/video1\""), "{before}");
		assert!(broadcaster.master_playlist(None).contains("video/video1/"));

		video1.cut();
		failed(&broken).await;
		assert!(broken.media_playlist(None).is_err());
		assert!(healthy.media_playlist(None).unwrap().is_some());
		let manifest = broadcaster.manifest(None).unwrap().expect("the sibling stays listed");
		assert!(manifest.contains("id=\"video/video0\""), "{manifest}");
		assert!(!manifest.contains("id=\"video/video1\""), "{manifest}");
		let master = broadcaster.master_playlist(None);
		assert!(master.contains("video/video0/"), "{master}");
		assert!(!master.contains("video/video1/"), "{master}");
		drop((catalog, video0));
	}

	// A playlist request waiting for its first row answers media that fails before one, rather
	// than timing out as not found or, without a router's timeout, waiting forever.
	#[tokio::test(start_paused = true)]
	async fn a_pending_playlist_answers_a_media_failure() {
		let test = Groups::new();
		let video1 = test.publish("video1");
		let rendition = test.rendition("video1");
		let pending = tokio::spawn(async move { rendition.playlist(None).await });
		kio::clock::sleep(Duration::from_secs(1)).await;
		assert!(!pending.is_finished(), "nothing is listed yet");

		video1.cut();
		let answer = kio::clock::timeout(Duration::from_secs(5), pending).await;
		let err = answer.expect("the failure wakes the request").unwrap().unwrap_err();
		assert!(matches!(err, crate::Error::MediaFailed { .. }), "{err}");
	}

	// A media subscription still waiting on its track fails with the broadcast, so nothing parks,
	// and media never read is not listed as an empty finished recording.
	#[tokio::test(start_paused = true)]
	async fn a_pending_media_subscription_ends_with_the_broadcast() {
		let test = Groups::new();
		// A handler keeps the unpublished tracks' requests waiting rather than refused.
		let _handler = test.broadcast.requests();
		kio::clock::sleep(Duration::from_secs(60)).await;
		assert!(!test.rendition("video0").snapshot().finished);

		test.broadcast.close();
		test.failed("video0").await;
		assert!(test.drain_failed("video0").await.is_empty());
	}

	// A recording refuses a listed segment whose media is gone, but reports a grid slot the
	// rendition holds nothing for (after an overrun) as a gap that keeps its number, so a recorder
	// can tell it from rows it missed.
	#[tokio::test(start_paused = true)]
	async fn a_recording_refuses_missing_media_but_reports_gaps() {
		let test = Groups::new();
		let mut video0 = test.publish("video0");
		let mut video1 = test.publish("video1");
		// Group 2 never lands, so segment 1, from groups 1 to 3, can't be served whole.
		video0.at(0, 0);
		video0.at(1, 2_000);
		video0.at(3, 3_000);
		video0.at(4, 4_000);
		video0.finish();
		// A group running from 0 to 7s overruns slots 1 and 2.
		video1.at(0, 0);
		video1.at(1, 7_000);
		video1.finish();
		test.finished("video0").await;

		let rendition = test.rendition("video0");
		let mut cursor = rendition.segments();
		assert_eq!(cursor.next().await.unwrap().unwrap().segment, 0);
		assert!(matches!(
			cursor.next().await,
			Err(crate::Error::SegmentUnavailable { segment: 1 })
		));
		assert!(matches!(
			cursor.next().await,
			Err(crate::Error::SegmentUnavailable { segment: 1 })
		));

		let snapshot = test.finished("video1").await;
		assert_eq!(gaps(&snapshot), [1, 2]);
		let rendition = test.rendition("video1");
		assert!(
			rendition.listed_segment(1).await.unwrap().is_none(),
			"a gap is never fetched"
		);
		let mut cursor = rendition.segments();
		let mut next = async || {
			let segment = cursor.next().await.unwrap().unwrap();
			(segment.segment, segment.media.is_some())
		};
		assert_eq!(next().await, (0, true));
		assert_eq!(next().await, (1, false));
		assert_eq!(next().await, (2, false));
		assert_eq!(next().await, (3, true));
		assert!(cursor.next().await.unwrap().is_none());
	}

	// Every rendition numbers its segments on the one grid while cutting at its own groups: audio
	// joining at 5.5s starts at segment 3, the line its first group reaches, as the video does.
	#[tokio::test(start_paused = true)]
	async fn an_unaligned_ladder_shares_the_grid() {
		let test = Groups::with(&["video0"], &["audio0"]);
		let mut video0 = test.publish("video0");
		let mut audio0 = test.publish("audio0");
		for sequence in 0..5 {
			video0.gop(sequence);
		}
		for (sequence, pts) in [(0, 5_500), (1, 8_000), (2, 10_000)] {
			audio0.at(sequence, pts);
		}
		assert_eq!(numbers(&test.listed("video0", 3).await), [0, 1, 2, 3]);
		let audio = test.listed("audio0", 4).await;
		assert_eq!(numbers(&audio), [3, 4]);
		assert!(gaps(&audio).is_empty());
		let rendition = test.rendition("audio0");
		assert!(rendition.is_advertised());
		let dash = rendition.representation().unwrap().expect("an out-of-band init");
		assert_eq!(dash.segments, [(5_500, 2_500), (8_000, 2_000)]);
	}

	// A discontinuity marker carries no media, so it starts no segment: the content before it
	// ends, the slots the pause skipped are gaps, and every listed segment serves media.
	#[tokio::test(start_paused = true)]
	async fn a_discontinuity_marker_lists_no_segment() {
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").expect("publish allowed");
		broadcast.announce(Default::default()).expect("publish allowed");
		settle().await;
		let catalog = hls_catalog(&mut broadcast.broadcast().clone());
		let mut media = video(&catalog, &broadcast, "video0", video_config());
		for micros in [0, 1_000_000, 2_000_000, 3_000_000] {
			media.write(vp8_frame(micros, micros % 2_000_000 == 0)).unwrap();
		}
		media.discontinuity().unwrap();
		for micros in [10_000_000, 12_000_000, 14_000_000] {
			media.write(vp8_frame(micros, true)).unwrap();
		}

		let broadcaster = Broadcaster::spawned(crate::resolved(&origin, "live").await, Config::default());
		let _ = kio::clock::timeout(Duration::from_secs(5), broadcaster.ready()).await;
		let rendition = broadcaster.rendition(Kind::Video, "video0").expect("rendition");
		let snapshot = kio::clock::timeout(Duration::from_secs(5), async {
			loop {
				let snapshot = rendition.snapshot();
				if snapshot.segments.last().is_some_and(|s| s.segment >= 6) {
					return snapshot;
				}
				kio::clock::sleep(Duration::from_millis(10)).await;
			}
		})
		.await
		.expect("the resumed content is listed");
		assert_eq!(numbers(&snapshot), [0, 1, 2, 3, 4, 5, 6]);
		assert_eq!(gaps(&snapshot), [2, 3, 4]);
		for segment in [0, 1, 5] {
			assert!(
				rendition.listed_segment(segment).await.unwrap().is_some(),
				"segment {segment} serves media"
			);
		}
		drop((catalog, media, broadcast));
	}

	// A pause leaves the grid slots it skipped as gaps, listed once content resumes, and needs no
	// discontinuity, since the timestamps never broke.
	#[tokio::test(start_paused = true)]
	async fn a_pause_is_listed_as_gaps() {
		let test = Groups::with(&["video0"], &[]);
		let mut video0 = test.publish("video0");
		video0.gop(0);
		video0.gop(1);
		video0.at(2, 10_000);
		video0.at(3, 12_000);

		let snapshot = test.listed("video0", 5).await;
		assert_eq!(numbers(&snapshot), [0, 1, 2, 3, 4, 5]);
		assert_eq!(gaps(&snapshot), [2, 3, 4]);
		let rendered = test
			.rendition("video0")
			.media_playlist(None)
			.unwrap()
			.expect("playable");
		assert!(rendered.contains("#EXT-X-GAP\n"), "{rendered}");
		assert!(!rendered.contains("#EXT-X-DISCONTINUITY\n"), "{rendered}");
	}

	#[tokio::test(start_paused = true)]
	async fn catalog_changes_report_removal_and_drain_old_cursors() {
		for reconfigure in [false, true] {
			let mut test = Groups::with(&["video0"], &[]);
			let mut records = test.publish("video0");
			records.gop(0);
			records.gop(1);
			test.listed("video0", 0).await;
			let mut changes = test.renditions.subscribe();
			let Some(renditions::Event::Added(rendition)) = changes.next().await.unwrap() else {
				panic!("the rendition is reported");
			};
			let mut old = rendition.segments();
			if reconfigure {
				test.catalog.video.renditions.get_mut("video0").unwrap().coded_width = Some(1280);
			} else {
				test.catalog.video.renditions.remove("video0");
			}
			test.sync();
			assert!(
				matches!(changes.next().await.unwrap(), Some(renditions::Event::Removed { name, .. }) if name == "video0")
			);
			assert_eq!(old.next().await.unwrap().unwrap().segment, 0);
			// The segment still waiting on a later one is listed once that starts. What starts it
			// is no longer the old instance's.
			records.gop(2);
			assert_eq!(old.next().await.unwrap().unwrap().segment, 1);
			assert!(old.next().await.unwrap().is_none());
			if reconfigure {
				let Some(renditions::Event::Added(new)) = changes.next().await.unwrap() else {
					panic!("the replacement is reported");
				};
				// The replacement muxes the same groups differently, so it lists them under its
				// own URLs, and only those its predecessor didn't. The playlist keeps the
				// predecessor's behind a discontinuity, each under its own init.
				assert_ne!(new.tag(), rendition.tag(), "one URL never names two muxes");
				records.gop(3);
				let snapshot = test.listed("video0", 2).await;
				assert_eq!(numbers(&snapshot), [0, 1, 2]);
				let tags: Vec<_> = snapshot.segments.iter().map(|s| s.tag.as_str()).collect();
				assert_eq!(tags, [rendition.tag(), rendition.tag(), new.tag()]);
				let breaks: Vec<_> = snapshot.segments.iter().map(|s| s.discontinuity).collect();
				assert_eq!(breaks, [false, false, true]);
				let maps: Vec<_> = snapshot.segments.iter().map(|s| s.map.is_some()).collect();
				assert_eq!(maps, [true, true, false], "only the predecessor's map their own init");
			}
		}
	}

	// A reconfigured rendition's playlist URI keeps serving its predecessor's segments, under their
	// own URLs, until the window slides past them (RFC 8216 6.2.2), then releases the predecessor.
	#[tokio::test(start_paused = true)]
	async fn a_reconfigure_keeps_serving_its_predecessors_segments() {
		let mut test = Groups::with(&["video0"], &[]);
		let mut records = test.publish("video0");
		for sequence in 0..3 {
			records.gop(sequence);
		}
		test.listed("video0", 1).await;
		let old = test.rendition("video0");
		test.catalog.video.renditions.get_mut("video0").unwrap().coded_width = Some(1280);
		test.sync();
		let new = test.rendition("video0");
		assert!(!Arc::ptr_eq(&old, &new), "the reconfigure replaces the rendition");
		assert_eq!(
			numbers(&new.snapshot()),
			[0, 1],
			"nothing the predecessor listed is dropped"
		);
		let id = |tag: &str, segment| segments::Id {
			generation: None,
			tag: tag.to_string(),
			at: segments::At::Sequence(segment),
		};
		assert!(new.segment(&id(old.tag(), 0)).await.unwrap().is_some());

		// The predecessor lists the segment it was waiting on, then the replacement its own.
		records.gop(3);
		records.gop(4);
		let snapshot = test.listed("video0", 3).await;
		assert_eq!(numbers(&snapshot), [0, 1, 2, 3]);
		let breaks: Vec<_> = snapshot.segments.iter().map(|s| s.discontinuity).collect();
		assert_eq!(breaks, [false, false, false, true]);
		assert!(new.segment(&id(old.tag(), 2)).await.unwrap().is_some());
		assert!(new.segment(&id(new.tag(), 3)).await.unwrap().is_some());
		assert!(
			new.segment(&id(new.tag(), 2)).await.unwrap().is_none(),
			"one URL never names two muxes"
		);

		// Once the window slides past the predecessor's segments, nothing holds it.
		for sequence in 5..24 {
			records.gop(sequence);
		}
		let snapshot = test.listed("video0", 22).await;
		assert!(snapshot.segments.iter().all(|s| s.segment > 3 && s.map.is_none()));
		assert_eq!(
			snapshot.discontinuity_sequence, 1,
			"the slid-out break is still counted"
		);
		assert_eq!(Arc::strong_count(&old), 1, "the replacement released its predecessor");
		assert!(new.segment(&id(old.tag(), 2)).await.unwrap().is_none());
	}

	// An inline-codec replacement builds its init only from a segment of its own, so until it lists
	// one, its playlist is the predecessor's segments under the predecessor's init.
	#[tokio::test(start_paused = true)]
	async fn an_inherited_playlist_needs_no_init_of_its_own() {
		let mut test = Groups::with(&["video0"], &[]);
		let mut records = test.publish("video0");
		for sequence in 0..3 {
			records.gop(sequence);
		}
		test.listed("video0", 1).await;
		let old = test.rendition("video0");
		let rendered = kio::clock::timeout(Duration::from_secs(5), old.playlist(None))
			.await
			.expect("the playlist renders")
			.unwrap()
			.expect("playable");
		let map = rendered
			.lines()
			.find(|line| line.starts_with("#EXT-X-MAP"))
			.expect("a map")
			.to_string();
		// The new config leaves its geometry to the keyframes, so only its media builds its init.
		let config = test.catalog.video.renditions.get_mut("video0").unwrap();
		config.coded_width = None;
		config.coded_height = None;
		test.sync();
		let new = test.rendition("video0");
		assert!(!Arc::ptr_eq(&old, &new), "the reconfigure replaces the rendition");

		let rendered = kio::clock::timeout(Duration::from_secs(5), new.playlist(None))
			.await
			.expect("the playlist renders without the replacement's media")
			.unwrap()
			.expect("the predecessor's segments are playable");
		assert_eq!(new.media_playlist(None).unwrap().as_deref(), Some(rendered.as_str()));
		let maps: Vec<_> = rendered.lines().filter(|line| line.starts_with("#EXT-X-MAP")).collect();
		assert_eq!(maps, [map.as_str()], "{rendered}");
		assert!(rendered.contains(&format!("seg/{}.1.m4s", old.tag())), "{rendered}");

		// Its own first segment needs its own init, which the playlist builds before listing it.
		records.gop(3);
		records.gop(4);
		test.listed("video0", 3).await;
		let rendered = kio::clock::timeout(Duration::from_secs(5), new.playlist(None))
			.await
			.expect("the playlist renders")
			.unwrap()
			.expect("playable");
		assert!(rendered.contains(&format!("seg/{}.3.m4s", new.tag())), "{rendered}");
		assert_eq!(
			new.media_playlist(None).unwrap().as_deref(),
			Some(rendered.as_str()),
			"its own init is built"
		);
	}

	// The DASH manifest lists only the current instance's segments, so its readiness waits for the
	// replacement's own first segment, even while the HLS playlist serves the predecessor's.
	#[tokio::test(start_paused = true)]
	async fn dash_readiness_waits_for_the_replacements_own_segment() {
		let mut test = Groups::with(&["video0"], &[]);
		let mut records = test.publish("video0");
		for sequence in 0..3 {
			records.gop(sequence);
		}
		test.listed("video0", 1).await;
		resize(&mut test, 1280);
		let new = test.rendition("video0");
		kio::clock::timeout(Duration::from_secs(1), new.playable())
			.await
			.expect("the playlist lists the predecessor's segments");
		assert!(
			kio::clock::timeout(Duration::from_secs(1), new.represented())
				.await
				.is_err(),
			"the replacement lists nothing of its own yet"
		);
		assert!(
			kio::clock::timeout(Duration::from_secs(1), test.renditions.video_init())
				.await
				.is_err(),
			"no manifest lists the replacement yet"
		);

		records.gop(3);
		records.gop(4);
		test.listed("video0", 3).await;
		kio::clock::timeout(Duration::from_secs(1), new.represented())
			.await
			.expect("the replacement lists its own segment");
		kio::clock::timeout(Duration::from_secs(1), test.renditions.video_init())
			.await
			.expect("a manifest lists the replacement");
	}

	/// Reconfigure `video0` to `width`.
	fn resize(test: &mut Groups, width: u32) {
		test.catalog.video.renditions.get_mut("video0").unwrap().coded_width = Some(width);
		test.sync();
	}

	// A predecessor still waiting on its first segment keeps it, whenever the playlist renders.
	#[tokio::test(start_paused = true)]
	async fn a_predecessors_pending_first_segment_is_kept() {
		let mut test = Groups::with(&["video0"], &[]);
		let mut records = test.publish("video0");
		records.gop(0);
		kio::clock::sleep(Duration::from_millis(100)).await;
		resize(&mut test, 1280);
		assert!(test.rendition("video0").snapshot().segments.is_empty());
		records.gop(1);
		records.gop(2);
		assert_eq!(numbers(&test.listed("video0", 1).await), [0, 1]);
	}

	// Reconfigures that list nothing keep only the instance that does, however many there are.
	#[tokio::test(start_paused = true)]
	async fn reconfigures_listing_nothing_hold_nothing() {
		let mut test = Groups::with(&["video0"], &[]);
		let mut records = test.publish("video0");
		records.gop(0);
		records.gop(1);
		test.listed("video0", 0).await;
		for width in 1..=8 {
			resize(&mut test, width);
			kio::clock::sleep(Duration::from_millis(10)).await;
		}
		assert_eq!(numbers(&test.rendition("video0").snapshot()), [0]);
		assert!(test.renditions.targets() <= 3, "{} targets", test.renditions.targets());
	}

	// A config that returns while its earlier instance's segments are still listed gets its tag
	// back, and each segment URL still reaches the instance that lists it.
	#[tokio::test(start_paused = true)]
	async fn a_returning_config_serves_both_instances_segments() {
		let mut test = Groups::with(&["video0"], &[]);
		let mut records = test.publish("video0");
		for sequence in 0..3 {
			records.gop(sequence);
		}
		test.listed("video0", 1).await;
		let first = test.rendition("video0");
		resize(&mut test, 1280);
		records.gop(3);
		records.gop(4);
		test.listed("video0", 3).await;
		resize(&mut test, 320);
		let returned = test.rendition("video0");
		assert_eq!(returned.tag(), first.tag(), "the same config muxes the same way");
		records.gop(5);
		records.gop(6);
		assert_eq!(numbers(&test.listed("video0", 5).await), [0, 1, 2, 3, 4, 5]);
		let id = |segment| segments::Id {
			generation: None,
			tag: first.tag().to_string(),
			at: segments::At::Sequence(segment),
		};
		for segment in [0, 2, 5] {
			assert!(
				returned.segment(&id(segment)).await.unwrap().is_some(),
				"segment {segment}"
			);
		}
	}

	// A replacement reading another track numbers its groups on the same grid, so it continues
	// where its predecessor stopped, behind a discontinuity.
	#[tokio::test(start_paused = true)]
	async fn a_replacement_on_another_track_continues_the_numbering() {
		let mut test = Groups::with(&["video0"], &[]);
		let mut records = test.publish("video0");
		for sequence in 0..4 {
			records.gop(sequence);
		}
		test.listed("video0", 2).await;

		test.catalog.video.renditions.get_mut("video0").unwrap().track = Some("video0b".into());
		test.sync();
		// The old track ends, completing the segment its rendition was waiting on.
		records.finish();
		let mut fresh = test.publish("video0b");
		for (sequence, gop) in (0..).zip(4..7) {
			fresh.at(sequence, gop * INTERVAL.as_millis() as u64);
		}
		let snapshot = test.listed("video0", 5).await;
		assert_eq!(numbers(&snapshot), [0, 1, 2, 3, 4, 5]);
		let breaks: Vec<_> = snapshot.segments.iter().map(|s| s.discontinuity).collect();
		assert_eq!(breaks, [false, false, false, false, true, false]);
	}

	// A segment holding both groups the retired instance had read and groups past them is listed
	// by the retired instance, ending where the replacement's first group starts: the replacement
	// can't list that number again, so otherwise up to an interval of media would reach neither.
	#[tokio::test(start_paused = true)]
	async fn a_retire_mid_segment_lists_the_predecessors_part() {
		let mut test = Groups::with(&["video0"], &[]);
		let mut media = test.publish("video0");
		// Four groups to a two-second slot: 0-3 are segment 0, and 4 and 5 open segment 1.
		for sequence in 0..6 {
			media.at(sequence, sequence * 500);
		}
		test.listed("video0", 0).await;
		kio::clock::sleep(Duration::from_millis(100)).await;
		let old = test.rendition("video0");
		test.catalog.video.renditions.get_mut("video0").unwrap().coded_width = Some(1280);
		test.sync();
		let new = test.rendition("video0");
		// 6 and 7 finish segment 1 past the retire; 8 to 11 are segment 2, which 12 completes.
		for sequence in 6..13 {
			media.at(sequence, sequence * 500);
		}
		let snapshot = test.listed("video0", 2).await;
		assert_eq!(numbers(&snapshot), [0, 1, 2], "no segment is lost to the hand-over");
		let tags: Vec<_> = snapshot.segments.iter().map(|s| s.tag.as_str()).collect();
		assert_eq!(tags, [old.tag(), old.tag(), new.tag()]);
		let durations: Vec<_> = snapshot.segments.iter().map(|s| s.duration.as_millis()).collect();
		assert_eq!(
			durations,
			[2_000, 1_000, 2_000],
			"the predecessor's part ends at group 6"
		);
		let id = segments::Id {
			generation: None,
			tag: old.tag().to_string(),
			at: segments::At::Sequence(1),
		};
		assert!(
			new.segment(&id).await.unwrap().is_some(),
			"the predecessor's part serves"
		);
	}

	// A segment spanning more groups than one request fetches is listed as a gap, never as a
	// segment that every request refuses.
	#[tokio::test(start_paused = true)]
	async fn a_segment_too_many_groups_long_is_a_gap() {
		let test = Groups::with(&["video0"], &[]);
		let mut media = test.publish("video0");
		let cap = test.rendition("video0").max_groups();
		assert_eq!(
			cap,
			INTERVAL.as_millis() as u64,
			"one group per millisecond of the interval"
		);
		for sequence in 0..=cap {
			media.at(sequence, 0);
		}
		media.at(cap + 1, 2_000);
		media.at(cap + 2, 4_000);
		let snapshot = test.listed("video0", 1).await;
		assert_eq!(numbers(&snapshot), [0, 1]);
		assert_eq!(gaps(&snapshot), [0]);
	}

	// A track ending while its last group failed fails the rendition: that group's segment would
	// otherwise be listed complete but truncated, under an ENDLIST.
	#[tokio::test(start_paused = true)]
	async fn a_failed_last_group_fails_the_rendition() {
		let test = Groups::with(&["video0"], &[]);
		let mut media = test.publish("video0");
		media.gop(0);
		let last = media.open(1, 2_000);
		media.finish();
		// The track has ended, so the watcher reads the last group for its content's end.
		kio::clock::sleep(Duration::from_millis(100)).await;
		last.abort(moq_net::Error::Transport("stream reset".into())).unwrap();
		let rendition = test.rendition("video0");
		let failed = kio::clock::timeout(Duration::from_secs(5), async {
			loop {
				if let Err(err) = rendition.media_playlist(None) {
					return err;
				}
				kio::clock::sleep(Duration::from_millis(10)).await;
			}
		})
		.await
		.expect("the rendition fails");
		assert!(matches!(failed, crate::Error::MediaFailed { .. }), "{failed:?}");
	}

	// A segment is the smallest whole multiple of the interval at least a second long
	// (maintainer, 2026-11-03), so an audio-only broadcast's 20 ms groups pack into 1 s segments and
	// an odd 300 ms interval gives 1.2 s ones. The target duration is twice the segment.
	#[tokio::test(start_paused = true)]
	async fn short_intervals_pack_into_whole_second_segments() {
		for (interval, groups, length, target) in [(20, 160, 1_000, 2), (300, 10, 1_200, 3)] {
			let test = Groups::joined(&[], &["audio0"], Duration::from_millis(interval));
			let rendition = test.rendition("audio0");
			assert_eq!(rendition.segment_length(), Duration::from_millis(length));
			assert_eq!(rendition.target_duration(), target);

			let mut media = test.publish("audio0");
			for sequence in 0..groups {
				media.at(sequence, sequence * interval);
			}
			let snapshot = test.listed("audio0", 1).await;
			assert!(gaps(&snapshot).is_empty(), "{interval} ms");
			let durations: Vec<_> = snapshot.segments.iter().map(|s| s.duration.as_millis()).collect();
			assert!(
				durations.len() >= 2 && durations.iter().all(|d| *d == u128::from(length)),
				"{interval} ms: {durations:?}"
			);
		}
	}

	// The last gap of a pause ends where the content resumes, which a group starting past its own
	// line puts almost two segments after the gap's line: the target duration still covers it.
	#[tokio::test(start_paused = true)]
	async fn the_target_duration_covers_a_pauses_last_gap() {
		// 999 ms intervals give 1998 ms segments. Group 1 resumes at 5990 ms and ends on line 3, so
		// it is segment 2, and gap 1 runs from line 1 to 5990 ms.
		let test = Groups::joined(&["video0"], &[], Duration::from_millis(999));
		let mut media = test.publish("video0");
		for (sequence, pts) in [0, 5_990, 5_994, 7_992].into_iter().enumerate() {
			media.at(sequence as u64, pts);
		}
		let snapshot = test.listed("video0", 3).await;
		assert_eq!(gaps(&snapshot), [1]);
		for segment in &snapshot.segments {
			let rounded = segment.duration.as_secs_f64().round() as u64;
			assert!(
				rounded <= snapshot.target_duration,
				"segment {} lasts {:?}, past the {} s target",
				segment.segment,
				segment.duration,
				snapshot.target_duration
			);
		}
	}

	// A join interval of a second or more is the segment length itself, as for a 2 s GOP.
	#[tokio::test(start_paused = true)]
	async fn a_two_second_gop_is_one_segment() {
		let test = Groups::with(&["video0"], &[]);
		let rendition = test.rendition("video0");
		assert_eq!(rendition.segment_length(), INTERVAL);
		assert_eq!(rendition.target_duration(), 4);
		let mut media = test.publish("video0");
		for sequence in 0..4 {
			media.gop(sequence);
		}
		let snapshot = test.listed("video0", 2).await;
		assert_eq!(numbers(&snapshot), [0, 1, 2]);
		assert!(snapshot.segments.iter().all(|s| s.duration == INTERVAL));
	}

	// EXT-X-TARGETDURATION is twice the interval, rounded up, however long the segments in the
	// window are. A segment past it once rounded to the nearest second, as only a final overrunning
	// group can be, is listed anyway, and counted for the warning.
	#[tokio::test(start_paused = true)]
	async fn the_target_duration_is_twice_the_interval() {
		let test = Groups::with(&["video0"], &[]);
		let mut records = test.publish("video0");
		records.gop(0);
		// 25 frames 200ms apart: the last starts at 6.8s, so the content runs to 7s.
		records.group(1, 2_000, 25);
		records.finish();
		let snapshot = test.finished("video0").await;
		let durations: Vec<_> = snapshot.segments.iter().map(|s| s.duration.as_millis()).collect();
		assert_eq!(durations, [2_000, 5_000]);
		assert_eq!(snapshot.target_duration, 4);
		assert_eq!(
			test.rendition("video0").overruns(),
			1,
			"only the 5s segment rounds past it"
		);
	}

	// Each playlist URI keeps the EXT-X-TARGETDURATION it first got: a rendition rebuilt on another
	// interval keeps its predecessor's, and one that returns derives its own, floored by the first.
	#[tokio::test(start_paused = true)]
	async fn each_playlist_keeps_its_target_duration() {
		let mut test = Groups::new();
		let target = |test: &Groups, name: &str| test.rendition(name).target_duration();
		assert_eq!((target(&test, "video0"), target(&test, "video1")), (4, 4));

		// The interval is fixed for a broadcast instance; should it change anyway, every rendition
		// is rebuilt on the new grid, keeping its playlist URI's target.
		let original = test.rendition("video0");
		test.catalog.media.join = hang::catalog::Join::new(Duration::from_secs(3));
		test.sync();
		let replacement = test.rendition("video0");
		assert!(!Arc::ptr_eq(&original, &replacement), "a new interval rebuilds");
		assert_eq!(replacement.interval(), Duration::from_secs(3));
		assert_eq!(replacement.target_duration(), 4);
		drop((original, replacement));

		let video1 = test.catalog.video.renditions.remove("video1").unwrap();
		test.sync();
		test.catalog.video.renditions.insert("video1".into(), video1);
		test.sync();
		assert_eq!(target(&test, "video1"), 6, "a returning rendition derives its own");
	}

	// Two edges over one media track agree on the media sequence.
	#[tokio::test(start_paused = true)]
	async fn edges_agree_on_the_media_sequence() {
		let early = Groups::with(&["video0"], &[]);
		let mut video0 = early.publish("video0");
		for sequence in 0..21 {
			video0.gop(sequence);
			settle().await;
		}
		let late = early.join();
		let joined = late.listed("video0", 19).await;
		let followed = early.listed("video0", 19).await;
		assert_eq!(joined.media_sequence, 12, "eight 2s segments cover the 16s window");
		assert_eq!(numbers(&followed), numbers(&joined));
	}
}
