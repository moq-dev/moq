//! Export: serve a MoQ broadcast as HLS or DASH, fetching media on demand.
//!
//! A [`Broadcaster`] subscribes to one broadcast's catalog and the HLS record track of each
//! rendition that advertises one (its catalog `hls` field, see [`hang::hls`]). That is the *only*
//! standing traffic: playlists and manifests are a pure function of the records, and media bytes
//! move only when an HTTP client requests a segment, which FETCHes exactly the groups that segment
//! holds from the relay cache and transmuxes them to CMAF. A rendition without records gets no
//! playlist. A `moq-archive` recording served by its `Consumer` is served the same way: its
//! playlists read only the stored records, and a segment GETs only its rendition's objects.
//!
//! A rendition's record `n` is its segment `n`: each rendition numbers its own segments, so every
//! edge agrees without coordinating. Nothing compares numbers across renditions; players match
//! renditions by timestamp, and segments line up wherever the publisher's keyframes do.
//! An inline-parameter-set codec with no catalog `description` GETs one keyframe
//! group on the first playlist render (in the background on a manifest render) to build its
//! init, then caches it.
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

pub(crate) use playlist::render_media;
pub use rendition::{Kind, Rendition};
pub use route::{Resource, Route};

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
	/// however short they are. Keep it within the relay's group-cache retention, since segments
	/// are fetched from there on request. It also caps segment `Cache-Control: max-age`.
	///
	/// Durable records get the same window: a live playlist stays bounded however much its store
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
	/// Aborts the catalog watcher when the broadcaster is dropped; `Drop` then releases the
	/// renditions themselves (see [`renditions::Producer::clear`]), each of which stops following
	/// its records once no recording cursor holds it. Set once, right after construction.
	watcher: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Broadcaster {
	/// Start tracking the renditions of `source`'s catalog broadcast.
	pub fn new(source: moq_mux::Source, config: Config) -> Arc<Self> {
		let broadcast = source.broadcast();
		// An epoch names the instance on every edge, so each serves it under the same URLs.
		let instance = broadcast.info().epoch.clone().unwrap_or_else(moq_net::Epoch::mint);
		let renditions = renditions::Producer::new(config.window);
		let broadcaster = Arc::new(Self {
			broadcast,
			instance: Arc::from(instance.as_str()),
			renditions: renditions.clone(),
			watcher: Mutex::new(None),
		});
		// The watcher owns its own producer clone; the `Broadcaster`'s `Drop` aborts it so the
		// standing catalog subscription stops when nobody's serving from this broadcaster.
		let watcher = tokio::spawn(watch_catalog(source, renditions));
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
	#[cfg(feature = "server")]
	pub(crate) fn poll_closed(&self, waiter: &kio::Waiter) -> std::task::Poll<()> {
		self.broadcast.poll_closed(waiter)
	}

	/// Look up a rendition by kind and name. Video and audio are separate axes, so a
	/// video and an audio rendition may share a name without colliding. A rendition is found
	/// only once the catalog advertises its HLS records.
	pub fn rendition(&self, kind: Kind, name: &str) -> Option<Arc<Rendition>> {
		self.renditions.get(kind, name)
	}

	/// A cursor over rendition add/remove events, for a consumer that mirrors or records the
	/// whole broadcast. It replays the current renditions, then yields changes, and returns
	/// `Ok(None)` only once the source finishes cleanly. A rendition is added once the catalog
	/// advertises its HLS records, which an unhinted publisher declares after its first segment.
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
	/// Lists only enabled renditions a player can start, i.e. ones listing at least one segment,
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
		(video, audio)
	}

	/// Render the DASH manifest (MPD) from the current renditions' records, or `None` while nothing is listable yet (every `SegmentTimeline`
	/// would be empty, which players reject).
	///
	/// Live broadcasts render a `dynamic` presentation anchored at the catalog's declared
	/// wall clock (or, absent one, at an anchor estimated from the first record's arrival); a
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
	/// than turning a truncated presentation `static`. A rendition whose own records failed is
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

	/// Resolve once the broadcast has something listable (its first complete segment, or
	/// already-ended records). Renditions start their segments together, so the first
	/// rendition's readiness stands in for the broadcast's. Bounding the wait is the caller's
	/// policy.
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
		// Release the map: a surviving `renditions::Consumer` would otherwise pin every
		// `Rendition` forever. A recording cursor's own `Arc<Rendition>` keeps that rendition's
		// records flowing, so its final segments are not truncated.
		self.renditions.clear();
	}
}

async fn watch_catalog(source: moq_mux::Source, renditions: renditions::Producer) {
	// The source's catalog broadcast, so the catalog, the record subscriptions, and the closed
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
					_ = kio::clock::sleep(wait) => {}
					_ = kio::wait(|waiter| broadcast.poll_closed(waiter)) => {
						renditions.close();
						return;
					}
				}
			}
		}
	};

	let result = loop {
		match kio::wait(|waiter| consumer.poll_next(waiter)).await {
			Ok(Some(catalog)) => renditions.sync(&source, &catalog),
			Ok(None) => break Ok(()),
			Err(err) => {
				tracing::warn!(%err, "broadcast catalog stream ended with error");
				break Err(err.into());
			}
		}
	};
	renditions.finish(result);
}

#[cfg(test)]
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

	type Media<R> = moq_mux::container::Producer<moq_mux::catalog::hang::Container, R>;

	/// A catalog config recording HLS segments of two seconds, declared up front.
	fn catalog_config() -> moq_mux::catalog::Config {
		let duration = Duration::from_secs(2);
		moq_mux::catalog::Config::default().with_hls(
			moq_mux::hls::Config::default()
				.with_duration(duration)
				.with_duration_hint(duration),
		)
	}

	/// A catalog on `broadcast` that records HLS segments (see [`catalog_config`]).
	fn hls_catalog(broadcast: &mut moq_net::broadcast::Producer) -> moq_mux::catalog::Producer {
		moq_mux::catalog::Producer::new(broadcast, catalog_config()).unwrap()
	}

	/// Publish a video track `name` through `catalog`, which owns its rendition and records it.
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

	/// The catalog `hls` field of track `track`'s records, two-second segments.
	fn hls(track: &str) -> hang::catalog::Hls {
		hang::catalog::Hls::new(hang::hls::default_name(track), Duration::from_secs(2))
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
		audio.hls = Some(hls("audio0"));
		let mut video = video_config();
		video.hls = Some(hls("video0"));

		let mut catalog = moq_mux::catalog::hang::Catalog::default();
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
		config.hls = Some(hls("video"));
		let mut catalog = moq_mux::catalog::hang::Catalog::default();
		catalog.video.renditions.insert("video".to_string(), config);

		let renditions = renditions::Producer::new(Config::default().window);
		renditions.sync(&upstream, &catalog);
		assert!(renditions.get(Kind::Video, "video").is_none());
	}

	#[tokio::test(start_paused = true)]
	async fn audio_renditions_follow_catalog_quality_order() {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let source = moq_mux::Source::new(produce_origin().consume(), broadcast.consume());
		let mut catalog = moq_mux::catalog::hang::Catalog::default();
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
			config.hls = Some(hls(name));
			catalog.audio.renditions.insert(name.to_string(), config);
		}
		let renditions = renditions::Producer::new(Config::default().window);
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

	// A rendition is served only once its catalog entry names its HLS records; one without them
	// gets no playlist, and is added beside its siblings once they appear.
	#[tokio::test]
	async fn a_rendition_without_hls_records_gets_no_playlist() {
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").expect("publish allowed");
		broadcast.announce(Default::default()).expect("publish allowed");
		settle().await;
		let upstream = crate::resolved(&origin, "live").await;

		let renditions = renditions::Producer::new(Config::default().window);
		let mut cursor = renditions.subscribe();
		let mut catalog = catalog_with_both();
		catalog.audio.renditions.get_mut("audio0").unwrap().hls = None;
		renditions.sync(&upstream, &catalog);

		let video = renditions.get(Kind::Video, "video0").expect("video has records");
		assert!(renditions.get(Kind::Audio, "audio0").is_none(), "audio has none yet");
		assert!(
			matches!(next_event(&mut cursor).await, Some(renditions::Event::Added(added)) if added.kind == Kind::Video)
		);
		assert!(next_event(&mut cursor).await.is_none(), "audio is not announced early");

		catalog.audio.renditions.get_mut("audio0").unwrap().hls = Some(hls("audio0"));
		renditions.sync(&upstream, &catalog);
		assert!(
			matches!(next_event(&mut cursor).await, Some(renditions::Event::Added(added)) if added.kind == Kind::Audio)
		);
		let same = renditions.get(Kind::Video, "video0").unwrap();
		assert!(Arc::ptr_eq(&video, &same), "the video sibling keeps its identity");

		drop(broadcast);
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
		let renditions = renditions::Producer::new(Config::default().window);
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
		let renditions = renditions::Producer::new(Config::default().window);
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

	// The other half: a change that alters how the rendition decodes, or which records it
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
		let renditions = renditions::Producer::new(Config::default().window);
		let mut cursor = renditions.subscribe();
		renditions.sync(&upstream, &catalog);

		let video = renditions.get(Kind::Video, "video0").expect("video rendition");
		let audio = renditions.get(Kind::Audio, "audio0").expect("audio rendition");
		while next_event(&mut cursor).await.is_some() {}

		// A resolution change and a record track change both rebuild.
		catalog.video.renditions.get_mut("video0").unwrap().coded_width = Some(640);
		catalog.audio.renditions.get_mut("audio0").unwrap().hls = Some(hls("audio1"));
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
		let renditions = renditions::Producer::new(Config::default().window);
		for width in 1..=8 {
			catalog.video.renditions.get_mut("video0").unwrap().coded_width = Some(width);
			renditions.sync(&upstream, &catalog);
		}
		// The unchanged audio rendition, the current video one, and at most the one it replaced.
		assert!(renditions.targets() <= 3, "{} targets", renditions.targets());
	}

	// The whole fetch-on-demand path in process: a broadcast publishes media through the
	// catalog (which records its HLS segments), the Broadcaster renders playlists from the
	// records alone, and a segment request fetches and transmuxes exactly its groups.
	#[tokio::test]
	async fn master_lists_only_playable_renditions() {
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").expect("publish allowed");
		broadcast.announce(Default::default()).expect("publish allowed");
		settle().await;
		let catalog = hls_catalog(&mut broadcast.broadcast().clone());
		let mut media = video(&catalog, &broadcast, "video0", video_config());

		let broadcaster = Broadcaster::new(crate::resolved(&origin, "live").await, Config::default());
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
				for &(kind, name) in &all {
					match kind {
						Kind::Video => {
							let mut config = video_config();
							config.hls = Some(hls(name));
							catalog.video.renditions.insert(name.into(), config);
						}
						Kind::Audio => {
							let mut config = opus();
							config.hls = Some(hls(name));
							catalog.audio.renditions.insert(name.into(), config);
						}
					}
				}
			})
			.unwrap();
		let mut records: Vec<_> = all
			.iter()
			.map(|(_, name)| {
				let track = broadcast
					.create_track(hang::hls::default_name(name), moq_mux::hls::Producer::info())
					.unwrap();
				(track.clone(), moq_mux::hls::Producer::new(track))
			})
			.collect();
		for sequence in 0..4 {
			for (_, records) in &mut records {
				records.push(&gop(sequence)).unwrap();
			}
		}
		let broadcaster = Broadcaster::new(crate::resolved(&origin, "live").await, Config::default());
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

		let broadcaster = Broadcaster::new(crate::resolved(&origin, "live").await, Config::default());
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

	/// A completed segment already decoded its inline codec metadata. Finishing the
	/// catalog lists a trailing group whose media producer is still live.
	#[tokio::test(start_paused = true)]
	async fn a_transmuxed_segment_keeps_its_init_with_an_unfinished_tail() {
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").expect("publish allowed");
		let (mut catalog, _media) = publish_vp8(&mut broadcast.broadcast().clone(), inline_vp8());
		broadcast.announce(moq_net::origin::Route::default()).unwrap();
		settle().await;
		let broadcaster = Broadcaster::new(crate::resolved(&origin, "live").await, Config::default());
		broadcaster.ready().await;
		let rendition = broadcaster.rendition(Kind::Video, "video0").expect("rendition");
		let mut cursor = rendition.segments();
		let segment = cursor.next().await.unwrap().expect("completed segment");
		assert!(!segment.media.expect("media, not a gap").is_empty());

		catalog.finish().unwrap();
		while !rendition.snapshot().finished {
			tokio::task::yield_now().await;
		}
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
		let (origin, broadcast, _catalog, mut records) = inline_pair("video1").await;
		let media = broadcast.create_track("video1", timed()).unwrap();
		// At first only group 0 is listed, bounded by a stop while it is still open.
		let mut open = media.create_group(0u64.into()).unwrap();
		write_keyframe(&mut open, 0);
		for records in &mut records {
			records.push(&gop(0)).unwrap();
			records.push(&stop(1, 2_000)).unwrap();
		}

		let broadcaster = Broadcaster::new(crate::resolved(&origin, "live").await, Config::default());
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
		for group in 1..3 {
			let mut writing = media.create_group(group.into()).unwrap();
			write_keyframe(&mut writing, group);
			writing.finish().unwrap();
		}
		for records in &mut records {
			for group in 1..4 {
				records
					.push(&hang::hls::Record::new(group + 1, group, group * 2_000))
					.unwrap();
			}
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

	/// `video0` and `video1`, each with an empty record track: `inline` is a VP8 whose init needs
	/// a keyframe group, and the other's init builds from the catalog. Media is left to the test.
	async fn inline_pair(
		inline: &str,
	) -> (
		moq_net::origin::Producer,
		moq_net::origin::Publication,
		moq_mux::catalog::Producer,
		[moq_mux::hls::Producer; 2],
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
					let mut config = if name == inline { inline_vp8() } else { video_config() };
					config.hls = Some(hls(name));
					catalog.video.renditions.insert(name.into(), config);
				}
			})
			.unwrap();
		let records = ["video0", "video1"].map(|name| {
			let track = broadcast
				.create_track(hang::hls::default_name(name), moq_mux::hls::Producer::info())
				.unwrap();
			moq_mux::hls::Producer::new(track)
		});
		(origin, broadcast, catalog, records)
	}

	/// [`inline_pair`] with `video1` inline, recording four [`gop`]s on each, so `video0` lists
	/// three segments.
	/// `video1`'s last is bounded only by a stop, so its group may still be open, while group 2
	/// is complete.
	async fn inline_sibling() -> (
		moq_net::origin::Producer,
		moq_net::origin::Publication,
		moq_mux::catalog::Producer,
		[moq_mux::hls::Producer; 2],
	) {
		let (origin, broadcast, catalog, mut records) = inline_pair("video1").await;
		for records in &mut records {
			for sequence in 0..4 {
				records.push(&gop(sequence)).unwrap();
			}
		}
		records[1].push(&stop(4, 8_000)).unwrap();
		(origin, broadcast, catalog, records)
	}

	/// Write one VP8 keyframe at [`gop`] `sequence`'s pts into `group`.
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
		let (origin, broadcast, _catalog, _records) = inline_sibling().await;
		let media = broadcast.create_track("video1", timed()).unwrap();
		for sequence in 0..3 {
			let mut group = media.create_group(sequence.into()).unwrap();
			write_keyframe(&mut group, sequence);
			group.finish().unwrap();
		}
		let mut open = media.create_group(3u64.into()).unwrap();
		write_keyframe(&mut open, 3);

		let broadcaster = Broadcaster::new(crate::resolved(&origin, "live").await, Config::default());
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
		let (origin, broadcast, _catalog, _records) = inline_sibling().await;
		let media = broadcast.create_track("video1", timed()).unwrap();
		let mut open = media.create_group(3u64.into()).unwrap();
		write_keyframe(&mut open, 3);

		let broadcaster = Broadcaster::new(crate::resolved(&origin, "live").await, Config::default());
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
				for name in ["video0", "video1"] {
					let mut config = if name == "video1" { inline_vp8() } else { video_config() };
					config.hls = Some(hls(name));
					catalog.video.renditions.insert(name.into(), config);
				}
			})
			.unwrap();
		let [(video0_track, mut video0), (_video1_track, mut video1)] = ["video0", "video1"].map(|name| {
			let track = broadcast
				.create_track(hang::hls::default_name(name), moq_mux::hls::Producer::info())
				.unwrap();
			(track.clone(), moq_mux::hls::Producer::new(track))
		});
		for sequence in 0..4 {
			video0.push(&gop(sequence)).unwrap();
			video1.push(&gop(sequence)).unwrap();
		}
		video1.push(&stop(4, 8_000)).unwrap();
		// video1's only cached group is still open, so its init build parks on it.
		let media = broadcast.create_track("video1", timed()).unwrap();
		let mut open = media.create_group(3u64.into()).unwrap();
		write_keyframe(&mut open, 3);

		let broadcaster = Broadcaster::new(crate::resolved(&origin, "live").await, Config::default());
		broadcaster.advertised().await;
		let broken = broadcaster.rendition(Kind::Video, "video0").unwrap();
		until_listed(&broken, 3).await;
		cut(video0_track, video0);
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
		let (origin, broadcast, _catalog, _records) = inline_sibling().await;
		let media = broadcast.create_track("video1", timed()).unwrap();
		let mut open = media.create_group(3u64.into()).unwrap();
		write_keyframe(&mut open, 3);

		let broadcaster = Broadcaster::new(crate::resolved(&origin, "live").await, Config::default());
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
		let (origin, broadcast, _catalog, _records) = inline_sibling().await;
		let media = broadcast.create_track("video1", timed()).unwrap();
		let mut open = media.create_group(3u64.into()).unwrap();
		write_keyframe(&mut open, 3);

		let broadcaster = Broadcaster::new(crate::resolved(&origin, "live").await, Config::default());
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
		let (origin, broadcast, _catalog, _records) = inline_sibling().await;
		let media = broadcast.create_track("video1", timed()).unwrap();
		let mut open = media.create_group(3u64.into()).unwrap();
		write_keyframe(&mut open, 3);

		let broadcaster = Broadcaster::new(crate::resolved(&origin, "live").await, Config::default());
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
		let (origin, broadcast, mut catalog, mut records) = inline_sibling().await;
		let media = broadcast.create_track("video1", timed()).unwrap();
		let mut open = media.create_group(3u64.into()).unwrap();
		write_keyframe(&mut open, 3);

		let broadcaster = Broadcaster::new(crate::resolved(&origin, "live").await, Config::default());
		broadcaster.advertised().await;
		let rendition = broadcaster.rendition(Kind::Video, "video0").unwrap();
		until_listed(&rendition, 3).await;
		records[0].push(&stop(4, 8_000)).unwrap();
		for records in &mut records {
			records.finish().unwrap();
		}
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
		let (origin, broadcast, mut catalog, mut records) = inline_pair("video0").await;
		// The inline `video0` lists segments; `video1`'s init builds from the catalog alone, but
		// it records nothing.
		for sequence in 0..3 {
			records[0].push(&gop(sequence)).unwrap();
		}
		let media = broadcast.create_track("video0", timed()).unwrap();
		// Its earlier group already left the cache, so the open one is the only candidate.
		let mut open = media.create_group(1u64.into()).unwrap();
		write_keyframe(&mut open, 1);

		let broadcaster = Broadcaster::new(crate::resolved(&origin, "live").await, Config::default());
		let _ = kio::clock::timeout(Duration::from_secs(5), broadcaster.ready()).await;
		until_listed(&broadcaster.rendition(Kind::Video, "video0").unwrap(), 2).await;
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
	// The wait starts its build once its records arrive, rather than timing out without video.
	#[tokio::test(start_paused = true)]
	async fn the_first_video_wait_builds_an_init_listed_while_waiting() {
		let (origin, broadcast, _catalog, mut records) = inline_pair("video0").await;
		let media = broadcast.create_track("video0", timed()).unwrap();

		let broadcaster = Broadcaster::new(crate::resolved(&origin, "live").await, Config::default());
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
			records[0].push(&gop(sequence)).unwrap();
		}
		kio::clock::timeout(Duration::from_secs(1), waiting)
			.await
			.expect("the wait builds video0's init once its records arrive");
	}

	// The manifest leaves a disabled rendition out, so it never fetches media for its init.
	#[tokio::test(start_paused = true)]
	async fn a_disabled_rendition_never_builds_its_init() {
		let (origin, broadcast, mut catalog, _records) = inline_sibling().await;
		catalog
			.mutate(|catalog| catalog.video.renditions.get_mut("video1").unwrap().enabled = false)
			.unwrap();
		let media = broadcast.create_track("video1", timed()).unwrap();
		for sequence in 0..4 {
			let mut group = media.create_group(sequence.into()).unwrap();
			write_keyframe(&mut group, sequence);
			group.finish().unwrap();
		}

		let broadcaster = Broadcaster::new(crate::resolved(&origin, "live").await, Config::default());
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

	// A reconfigure that changes the init bytes changes the init URL, and the old URL stops
	// resolving, so a cache can never hand a player the previous init under the new one.
	#[tokio::test]
	async fn a_reconfigure_versions_the_init_url() {
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").expect("publish allowed");
		broadcast.announce(Default::default()).expect("publish allowed");
		settle().await;
		let (_catalog, mut media) = publish_vp8(&mut broadcast.broadcast().clone(), video_config());

		let broadcaster = Broadcaster::new(crate::resolved(&origin, "live").await, Config::default());
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
		// The replacement lists only records after the reconfigure.
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
		let new_hash = init_hash(&new_playlist);
		assert_ne!(new_hash, old_hash, "different init bytes get a different URL");
		assert_eq!(new.init(new_hash).await.unwrap(), Some(new_init));
		assert!(new.init(&old_hash).await.unwrap().is_none());
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

		let broadcaster = Broadcaster::new(crate::resolved(&origin, "live").await, Config::default());
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

		let broadcaster = Broadcaster::new(crate::resolved(&origin, "live").await, Config::default());
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
	async fn serves_playlist_and_segments_from_the_records() {
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").expect("publish allowed");
		broadcast.announce(Default::default()).expect("publish allowed");
		settle().await;
		// Three GOPs, 2s apart: segments 0 and 1 are complete, segment 2 is the live edge.
		let (catalog, media) = publish_vp8(&mut broadcast.broadcast().clone(), inline_vp8());

		let broadcaster = Broadcaster::new(crate::resolved(&origin, "live").await, Config::default());
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
		assert_eq!(playlist.target_duration, 2);
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

	// A fetched segment's last sample runs to where the next segment starts, as its record says,
	// rather than to a guess from the framerate.
	#[tokio::test]
	async fn a_segment_ends_where_its_record_says() {
		let origin = produce_origin();
		let broadcast = origin.create_broadcast("live").expect("publish allowed");
		broadcast.announce(Default::default()).expect("publish allowed");
		settle().await;
		let (catalog, media) = publish_vp8(&mut broadcast.broadcast().clone(), video_config());

		let broadcaster = Broadcaster::new(crate::resolved(&origin, "live").await, Config::default());
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

		let broadcaster = Broadcaster::new(crate::resolved(&origin, "live").await, Config::default());
		let _ = kio::clock::timeout(Duration::from_secs(5), broadcaster.ready()).await;
		let rendition = broadcaster.rendition(Kind::Video, "video0").expect("rendition");
		let _ = kio::clock::timeout(Duration::from_secs(5), rendition.playable()).await;

		let segment = rendition.listed_segment(0).await.unwrap().expect("segment 0");
		let timescale = rendition.muxer().unwrap().timescale().as_u64();
		let frame = Some(u32::try_from(timescale * 33 / 1_000).unwrap());
		assert_eq!(sample_durations(&segment), [frame, frame]);

		drop((media, catalog, broadcast));
	}

	// The DASH half of the fetch-on-demand path: the manifest renders from the same record
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

		let broadcaster = Broadcaster::new(crate::resolved(&origin, "live").await, Config::default());
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

		let broadcaster = Broadcaster::new(crate::resolved(&origin, "live").await, Config::default());
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

		// Write the catalog and records directly: a moq-mux catalog producer always advertises
		// its clock, as a publisher on a host whose wall clock is set does.
		let mut config = video_config();
		config.hls = Some(hls("video0"));
		let mut snapshot = moq_mux::catalog::hang::Catalog::<()>::default();
		snapshot.video.renditions.insert("video0".to_string(), config);
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
		let mut records = moq_mux::hls::Producer::new(
			broadcast
				.create_track(hang::hls::default_name("video0"), moq_mux::hls::Producer::info())
				.unwrap(),
		);
		let media = Records::write_media(&broadcast, "video0", 3);
		for sequence in 0..3 {
			records.push(&gop(sequence)).unwrap();
		}

		let broadcaster = Broadcaster::new(crate::resolved(&origin, "live").await, Config::default());
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

		drop((media, records, catalog, broadcast));
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
		let broadcaster = Broadcaster::new(crate::resolved(&origin, "live").await, Config::default());
		assert!(
			next_event(&mut broadcaster.renditions()).await.is_none(),
			"the moov alone lists no rendition"
		);

		import.decode(&fragments).unwrap();
		let anchored = catalog.snapshot().clock.expect("the catalog advertises a clock");

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
		// Reuse the A/V descriptors; opaque samples exercise record discovery and FETCH,
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
		let broadcaster = (!late).then(|| Broadcaster::new(source.clone(), Config::default()));
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
		let broadcaster = broadcaster.unwrap_or_else(|| Broadcaster::new(source, Config::default()));
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

		let broadcaster = Broadcaster::new(crate::resolved(&origin, "live").await, Config::default());
		let _ = kio::clock::timeout(Duration::from_secs(5), broadcaster.ready()).await;
		let _ = kio::clock::timeout(Duration::from_secs(5), broadcaster.playable()).await;

		// A clean finish stops the open segment and ends the records.
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
		// The trailing segment holds one frame that nothing bounds, so it lasts no time. HLS
		// lists it, but a SegmentTimeline entry at the presentation end would never be
		// requested, so the manifest omits it and the presentation ends at the last bounded one.
		assert!(manifest.contains("<S t=\"0\" d=\"2000\"/>"), "{manifest}");
		assert!(
			!manifest.contains("<S t=\"2000\""),
			"the zero-duration tail is not listed"
		);
		assert!(manifest.contains(" mediaPresentationDuration=\"PT2.000S\""));
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

		let broadcaster = Broadcaster::new(crate::resolved(&origin, "live").await, Config::default());
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

		let broadcaster = Broadcaster::new(crate::resolved(&origin, "live").await, Config::default());
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
	#[tokio::test]
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

		let broadcaster = Broadcaster::new(crate::resolved(&origin, "live").await, Config::default());
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
		// The finish's stop is the trailing gap.
		assert_eq!(
			listed,
			[(0, true), (1, true), (2, false)],
			"the final segment survives dropping the broadcaster"
		);

		drop((catalog, media, broadcast, rendition));
	}

	/// A one-video-rendition catalog snapshot reconciled by hand, which is what `watch_catalog`
	/// does with each snapshot: the rendition follows its records on its own.
	fn export(
		upstream: &moq_mux::Source,
		config: &hang::catalog::VideoConfig,
	) -> (renditions::Producer, Arc<Rendition>) {
		let mut config = config.clone();
		config.hls = Some(hls("video0"));
		let mut catalog = moq_mux::catalog::hang::Catalog::default();
		catalog.video.renditions.insert("video0".to_string(), config);
		let renditions = renditions::Producer::new(Config::default().window);
		renditions.sync(upstream, &catalog);
		let rendition = renditions.get(Kind::Video, "video0").expect("rendition synced");
		(renditions, rendition)
	}

	/// Publish `video0`'s records on `broadcast`.
	fn records(broadcast: &moq_net::broadcast::Producer) -> (moq_net::track::Producer, moq_mux::hls::Producer) {
		let track = broadcast
			.create_track(hang::hls::default_name("video0"), moq_mux::hls::Producer::info())
			.unwrap();
		(track.clone(), moq_mux::hls::Producer::new(track))
	}

	/// Write three keyframes of `payload` into `broadcast`'s `video0` track, 2s apart from
	/// `start_micros`, with the records numbering them from `sequence` (when given).
	fn write_routed_media(
		broadcast: &mut moq_net::broadcast::Producer,
		payload: &'static [u8],
		records: Option<(&mut moq_mux::hls::Producer, u64)>,
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
		if let Some((records, sequence)) = records {
			for group in 0..3 {
				let pts = (start_micros + group * 2_000_000) / 1_000;
				records
					.push(&hang::hls::Record::new(sequence + group, group, pts))
					.unwrap();
			}
		}
		media
	}

	struct Ended {
		rendition: Arc<Rendition>,
		origin: moq_net::origin::Producer,
		_held: Box<dyn std::any::Any>,
	}

	/// Finish the media and its records before End while retaining its track cache for a late
	/// recorder.
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
		let (_track, mut records) = records(&broadcast);
		let mut media = write_routed_media(&mut target.broadcast().clone(), KEYFRAME, Some((&mut records, 0)), 0);
		media.finish().unwrap();
		records.push(&stop(3, 6_000)).unwrap();
		records.finish().unwrap();
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
			_held: Box::new((renditions, media, broadcast, target, records)),
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
			// The end's stop is the trailing gap.
			assert_eq!(
				drained,
				[(0, true), (1, true), (2, true), (3, false)],
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
		// carry `payload`, so the two epochs' media differs byte for byte. The returned handle
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
		// the assertion above is about which epoch a rendition is bound to, not about a broadcast
		// that happens to be unreadable.
		let fresh = crate::resolved(&origin, "live").await;
		let (_renditions, rendition) = export(&fresh, &video_config());
		kio::clock::timeout(Duration::from_secs(5), rendition.playable())
			.await
			.expect("the new publisher's records arrive");
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
	// segment is requested: the records on the catalog broadcast keep describing the instance
	// they were written for, so a media publisher that reconnects (restarting its group
	// numbering) must not answer for segments the old records already described.
	#[tokio::test]
	async fn a_replacement_sibling_is_not_served_under_the_replaced_catalog() {
		const OLD: &[u8] = b"OLDOLDOLDOLDOLDO";
		const NEW: &[u8] = b"NEWNEWNEWNEWNEWN";

		fn publish_media(
			origin: &moq_net::origin::Producer,
			payload: &'static [u8],
			records: Option<&mut moq_mux::hls::Producer>,
		) -> Box<dyn std::any::Any> {
			let broadcast = origin.create_broadcast("media").expect("publish allowed");
			broadcast.announce(Default::default()).expect("publish allowed");
			let media = write_routed_media(
				&mut broadcast.broadcast().clone(),
				payload,
				records.map(|records| (records, 0)),
				0,
			);
			Box::new((broadcast, media))
		}

		let origin = produce_origin();

		// The catalog broadcast carries the records; the media lives next door.
		let live = origin.create_broadcast("live").expect("publish allowed");
		live.announce(Default::default()).expect("publish allowed");
		let (_track, mut records) = records(&live);
		let old_media = publish_media(&origin, OLD, Some(&mut records));
		settle().await;

		let upstream = crate::resolved(&origin, "live").await;

		// A relative reference replaces the base's last segment, so "media" is a sibling of the
		// catalog broadcast "live".
		let mut config = video_config();
		config.broadcast = Some(moq_net::path::Relative::new("media").unwrap());

		let (_renditions, rendition) = export(&upstream, &config);
		kio::clock::timeout(Duration::from_secs(5), rendition.playable())
			.await
			.expect("the records reach the rendition");

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

		// Control: an export started after the takeover does serve the new publisher's media.
		let fresh = crate::resolved(&origin, "live").await;
		let (_renditions, rendition) = export(&fresh, &config);
		kio::clock::timeout(Duration::from_secs(5), rendition.playable())
			.await
			.expect("the records reach the fresh rendition");
		let served = rendition
			.listed_segment(0)
			.await
			.unwrap()
			.expect("the new publisher is servable on a freshly bound rendition");
		assert!(
			contains(&served, NEW),
			"the new export serves the new publisher's media"
		);

		drop((new_media, records, live));
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

	/// A sibling-media fixture: the catalog broadcast `live` carries `video0`'s records, and the
	/// media lives in the sibling broadcast `media`.
	struct Sibling {
		origin: moq_net::origin::Producer,
		_live: moq_net::origin::Publication,
		records: moq_mux::hls::Producer,
		upstream: moq_mux::Source,
		config: hang::catalog::VideoConfig,
	}

	impl Sibling {
		async fn new() -> Self {
			let origin = produce_origin();
			let live = origin.create_broadcast("live").expect("publish allowed");
			live.announce(Default::default()).expect("publish allowed");
			let (_track, records) = records(&live);
			settle().await;
			let upstream = crate::resolved(&origin, "live").await;
			let mut config = video_config();
			config.broadcast = Some(moq_net::path::Relative::new("media").unwrap());
			Self {
				origin,
				_live: live,
				records,
				upstream,
				config,
			}
		}

		/// Publish the sibling's media and its records, unannounced.
		fn media(
			&mut self,
			payload: &'static [u8],
			sequence: u64,
			start_micros: u64,
		) -> (
			moq_net::origin::Publication,
			moq_mux::container::Producer<moq_mux::catalog::hang::Container>,
		) {
			let media = self.origin.create_broadcast("media").unwrap();
			let track = write_routed_media(
				&mut media.broadcast().clone(),
				payload,
				Some((&mut self.records, sequence)),
				start_micros,
			);
			(media, track)
		}
	}

	// A bound sibling ends the playlist while its retained rows drain. Another publisher at
	// that path is never bound, so its bytes cannot answer the old records.
	#[tokio::test]
	async fn a_replaced_sibling_ends_the_rendition() {
		tokio::time::pause();
		const OLD: &[u8] = b"OLDOLDOLDOLDOLDO";
		const NEW: &[u8] = b"NEWNEWNEWNEWNEWN";

		let mut test = Sibling::new().await;
		let (old_media, old_track) = test.media(OLD, 0, 0);
		settle().await;
		let (_renditions, rendition) = export(&test.upstream, &test.config);
		old_media.announce(sibling_route(10)).unwrap();
		kio::clock::timeout(Duration::from_secs(5), rendition.playable())
			.await
			.expect("the records reach the rendition");
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

		// A publisher serving the path afterwards, with records written for it, is never bound.
		let (new_media, new_track) = test.media(NEW, 3, 6_000_000);
		new_media.announce(sibling_route(11)).unwrap();
		kio::clock::sleep(Duration::from_secs(1)).await;
		let snapshot = rendition.snapshot();
		assert_eq!(snapshot.segments.len(), before, "nothing of the replacement is listed");
		assert!(snapshot.finished);
		drop((new_media, new_track, test));
	}

	// A restart can land while a segment request waits for its media broadcast. The init
	// decoded from the old run's groups must not be cached for the new run.
	#[tokio::test]
	async fn a_segment_straddling_a_restart_keeps_its_init_out_of_the_new_run() {
		tokio::time::pause();
		let mut test = Sibling::new().await;
		test.config = {
			let mut config = inline_vp8();
			config.broadcast = test.config.broadcast.clone();
			config
		};
		let (media, _track) = test.media(KEYFRAME, 0, 0);
		settle().await;
		let (renditions, rendition) = export(&test.upstream, &test.config);
		kio::clock::timeout(Duration::from_secs(5), rendition.playable())
			.await
			.expect("the records reach the rendition");

		// The sibling is unannounced, so the segment waits for its media broadcast.
		let mut segment = std::pin::pin!(rendition.listed_segment(0));
		assert!(
			kio::clock::timeout(Duration::from_millis(10), segment.as_mut())
				.await
				.is_err()
		);
		renditions.fanout().set_generation(Some("run-1".into()));
		renditions.fanout().set_generation(Some("run-2".into()));

		media.announce(sibling_route(10)).unwrap();
		segment
			.await
			.unwrap()
			.expect("the old run's rows were captured before the restart");
		assert!(
			rendition.latest_init().await.unwrap().is_none(),
			"the new run has no media of its own yet"
		);
	}

	// A replacement already present when the export starts is the sibling the request
	// resolves to: it is served, not treated as a stale epoch.
	#[tokio::test]
	async fn a_sibling_present_at_export_start_is_served() {
		const NEW: &[u8] = b"NEWNEWNEWNEWNEWN";

		let mut test = Sibling::new().await;
		let (media, _track) = test.media(NEW, 0, 0);
		media.announce(sibling_route(11)).unwrap();
		settle().await;

		let (_renditions, rendition) = export(&test.upstream, &test.config);
		kio::clock::timeout(Duration::from_secs(5), rendition.playable())
			.await
			.expect("the records reach the rendition");
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
		let (old_media, old_track) = test.media(OLD, 0, 0);
		settle().await;
		let (_renditions, rendition) = export(&test.upstream, &test.config);
		old_media.announce(sibling_route(10)).unwrap();
		kio::clock::timeout(Duration::from_secs(5), rendition.playable())
			.await
			.expect("the records reach the rendition");
		assert!(rendition.listed_segment(0).await.unwrap().is_some());

		drop((old_media, old_track));
		until_finished(&rendition).await;
		let before = rendition.snapshot().segments.len();
		assert!(before > 0);

		let (new_media, new_track) = test.media(NEW, 3, 6_000_000);
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
	// (a playlist request, a record) touching the rendition.
	#[tokio::test]
	async fn a_cursor_at_the_edge_ends_when_its_sibling_ends() {
		tokio::time::pause();
		const OLD: &[u8] = b"OLDOLDOLDOLDOLDO";

		let mut test = Sibling::new().await;
		let (media, track) = test.media(OLD, 0, 0);
		settle().await;
		let (_renditions, rendition) = export(&test.upstream, &test.config);
		media.announce(sibling_route(10)).unwrap();
		kio::clock::timeout(Duration::from_secs(5), rendition.playable())
			.await
			.expect("the records reach the rendition");

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

		// Only the sibling broadcast goes: its records stay quiet.
		drop(media);
		let end = kio::clock::timeout(Duration::from_secs(5), cursor.next())
			.await
			.expect("the cursor wakes when its sibling ends")
			.unwrap();
		assert!(end.is_none(), "the cursor ends with its sibling");
		drop(track);
	}

	// A sibling not yet announced when the rendition binds it is waited for. The catalog's rows
	// are still listed, and the binding resolves on the sibling's first announcement.
	#[tokio::test]
	async fn a_sibling_announced_after_export_start_is_bound() {
		tokio::time::pause();
		const OLD: &[u8] = b"OLDOLDOLDOLDOLDO";

		let mut test = Sibling::new().await;
		let (media, _track) = test.media(OLD, 0, 0);
		settle().await;
		let (_renditions, rendition) = export(&test.upstream, &test.config);
		kio::clock::timeout(Duration::from_secs(5), rendition.playable())
			.await
			.expect("the catalog's rows are listed before the sibling resolves");

		media.announce(sibling_route(10)).unwrap();
		let served = rendition
			.listed_segment(0)
			.await
			.unwrap()
			.expect("the late sibling is servable");
		assert!(contains(&served, OLD));
	}

	// A rendition the catalog drops must end its segment cursors. The cursor holds an
	// `Arc<Rendition>`, so without an explicit end the rendition (and its record subscription)
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
		renditions.sync(&upstream, &moq_mux::catalog::hang::Catalog::default());
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

		let broadcaster = Broadcaster::new(crate::resolved(&origin, "live").await, Config::default());

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

	// The record path: the renditions cursor yields the video rendition, its segment cursor
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

		let broadcaster = Broadcaster::new(crate::resolved(&origin, "live").await, Config::default());

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

		// Tear down the publisher. The records end abruptly, so the cursor fails rather than end
		// as if the recording were complete.
		drop((catalog, media));
		broadcast.close();

		let end = kio::clock::timeout(Duration::from_secs(5), segments.next())
			.await
			.expect("segment cursor resolves after the source is lost");
		assert!(
			matches!(end, Err(crate::Error::RecordsFailed { .. })),
			"an abrupt end fails the recording: {:?}",
			end.map(|segment| segment.map(|segment| segment.segment))
		);

		// The renditions cursor also fails when the source closes abruptly.
		let ended = kio::clock::timeout(Duration::from_secs(5), renditions.next())
			.await
			.expect("renditions cursor resolves");
		assert!(ended.is_err(), "an abrupt broadcast close fails the renditions cursor");
	}

	/// A record covering group `sequence` from `2 * sequence` seconds.
	fn gop(sequence: u64) -> hang::hls::Record {
		hang::hls::Record::new(sequence, sequence, sequence * 2_000)
	}

	/// A record where content stopped.
	fn stop(sequence: u64, pts_ms: u64) -> hang::hls::Record {
		hang::hls::Record::new(sequence, sequence, pts_ms).with_duration(0)
	}

	/// Renditions over hand-published records, reconciled the way `watch_catalog` does. A
	/// segment fetch misses unless [`media`](Records::media) published its groups.
	struct Records {
		broadcast: moq_net::broadcast::Producer,
		catalog: moq_mux::catalog::hang::Catalog,
		renditions: renditions::Producer,
	}

	impl Records {
		/// `video0` and `video1` renditions.
		fn new() -> Self {
			Self::with(&["video0", "video1"], &[])
		}

		/// `video` and `audio` renditions, each with its own records.
		fn with(video: &[&str], audio: &[&str]) -> Self {
			let mut catalog = moq_mux::catalog::hang::Catalog::default();
			for name in video {
				let mut config = video_config();
				config.hls = Some(hls(name));
				catalog.video.renditions.insert(name.to_string(), config);
			}
			for name in audio {
				let mut config = opus();
				config.hls = Some(hls(name));
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
			let renditions = renditions::Producer::new(Config::default().window);
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

		/// Another exporter over the same records: a second edge, or one a fresh viewer starts.
		fn join(&self) -> Self {
			Self {
				broadcast: self.broadcast.clone(),
				catalog: self.catalog.clone(),
				renditions: Self::export(&self.broadcast, &self.catalog),
			}
		}

		/// Publish `track`'s records. Returns the track, to fail it, with the records writing to it.
		fn publish(&self, track: &str) -> (moq_net::track::Producer, moq_mux::hls::Producer) {
			let track = self
				.broadcast
				.create_track(hang::hls::default_name(track), moq_mux::hls::Producer::info())
				.unwrap();
			(track.clone(), moq_mux::hls::Producer::new(track))
		}

		/// Publish `name`'s media: one keyframe group per [`gop`] record `0..groups`, so a cursor
		/// fetches real segments.
		fn media(&self, name: &str, groups: u64) -> moq_net::track::Producer {
			Self::write_media(&self.broadcast, name, groups)
		}

		fn write_media(broadcast: &moq_net::broadcast::Producer, name: &str, groups: u64) -> moq_net::track::Producer {
			let track = broadcast.create_track(name, timed()).unwrap();
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

		/// Drain `name`'s recording cursor, which must answer a record failure once drained,
		/// returning the segments it yielded first.
		async fn drain_failed(&self, name: &str) -> Vec<u64> {
			let (drained, end) = self.drain(name).await;
			assert!(matches!(end, Err(crate::Error::RecordsFailed { .. })), "{end:?}");
			drained
		}
	}

	/// The records' publisher loses its session: the track and its open group fail.
	fn cut(track: moq_net::track::Producer, records: moq_mux::hls::Producer) {
		drop(records);
		track
			.abort(moq_net::Error::Transport("connection lost".into()))
			.unwrap();
	}

	/// Wait until `rendition`'s playlist fails, returning the error it answers.
	async fn failed(rendition: &Rendition) -> crate::Error {
		for _ in 0..500 {
			if let Err(err) = rendition.try_snapshot() {
				assert!(matches!(err, crate::Error::RecordsFailed { .. }), "{err}");
				return err;
			}
			kio::clock::sleep(Duration::from_millis(10)).await;
		}
		panic!("{} never failed", rendition.name);
	}

	#[tokio::test(start_paused = true)]
	async fn a_segment_cursor_rejects_an_unfinished_window() {
		let test = Records::new();
		let rendition = test.rendition("video0");
		let mut cursor = rendition.segments();
		rendition.close();
		assert!(matches!(cursor.next().await, Err(crate::Error::IncompleteRecording)));
		assert!(matches!(cursor.next().await, Err(crate::Error::IncompleteRecording)));
	}

	#[tokio::test(start_paused = true)]
	async fn a_failed_catalog_fails_unfinished_segment_windows() {
		let test = Records::new();
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
		let test = Records::with(&["video0"], &[]);
		let rendition = test.rendition("video0");
		let mut slow = rendition.segments();
		let (_track, mut records) = test.publish("video0");
		for sequence in 0..21 {
			records.push(&gop(sequence)).unwrap();
		}
		assert!(test.listed("video0", 19).await.media_sequence > 0);
		assert!(matches!(slow.next().await, Err(crate::Error::RecordingLagged)));
		assert!(matches!(slow.next().await, Err(crate::Error::RecordingLagged)));
	}

	// A failed rendition's records are not retried, and never papered over with gaps or ended as
	// if complete: its listings answer the error, while the other renditions go on.
	#[tokio::test(start_paused = true)]
	async fn a_record_error_fails_only_its_playlist() {
		let test = Records::new();
		let (_video0, mut video0) = test.publish("video0");
		let (track, mut video1) = test.publish("video1");
		let _media = test.media("video1", 4);
		for sequence in 0..4 {
			video0.push(&gop(sequence)).unwrap();
			video1.push(&gop(sequence)).unwrap();
		}
		// The newest record waits for its successor.
		assert_eq!(numbers(&test.listed("video1", 2).await), [0, 1, 2]);

		cut(track, video1);
		for sequence in 4..7 {
			video0.push(&gop(sequence)).unwrap();
		}
		test.failed("video1").await;
		let video1 = test.rendition("video1");
		assert!(video1.media_playlist(None).is_err());
		assert!(video1.representation().is_err());
		assert!(video1.failed_alone());
		// Segments already listed stay fetchable while the listing refuses.
		assert!(video1.listed_segment(0).await.unwrap().is_some());
		// So does the cursor, once it drains what the records covered.
		assert_eq!(test.drain_failed("video1").await, [0, 1, 2]);

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
				for name in ["video0", "video1"] {
					let mut config = video_config();
					config.hls = Some(hls(name));
					catalog.video.renditions.insert(name.into(), config);
				}
			})
			.unwrap();
		let publish = |name: &str| {
			let track = broadcast
				.create_track(hang::hls::default_name(name), moq_mux::hls::Producer::info())
				.unwrap();
			(track.clone(), moq_mux::hls::Producer::new(track))
		};
		let (_video0_track, mut video0) = publish("video0");
		let (video1_track, mut video1) = publish("video1");
		for sequence in 0..4 {
			video0.push(&gop(sequence)).unwrap();
			video1.push(&gop(sequence)).unwrap();
		}
		let broadcaster = Broadcaster::new(crate::resolved(&origin, "live").await, Config::default());
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

		cut(video1_track, video1);
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

	// A playlist request waiting for its first row answers records that fail before one, rather
	// than timing out as not found or, without a router's timeout, waiting forever.
	#[tokio::test(start_paused = true)]
	async fn a_pending_playlist_answers_a_record_failure() {
		let test = Records::new();
		let (track, video1) = test.publish("video1");
		let rendition = test.rendition("video1");
		let pending = tokio::spawn(async move { rendition.playlist(None).await });
		kio::clock::sleep(Duration::from_secs(1)).await;
		assert!(!pending.is_finished(), "nothing is listed yet");

		cut(track, video1);
		let answer = kio::clock::timeout(Duration::from_secs(5), pending).await;
		let err = answer.expect("the failure wakes the request").unwrap().unwrap_err();
		assert!(matches!(err, crate::Error::RecordsFailed { .. }), "{err}");
	}

	// Malformed records fail their rendition the same way.
	#[tokio::test(start_paused = true)]
	async fn malformed_records_fail_their_playlist() {
		let test = Records::new();
		let (track, mut video0) = test.publish("video0");
		video0.push(&gop(0)).unwrap();
		video0.push(&gop(1)).unwrap();
		test.listed("video0", 0).await;

		let mut group = track.append_group().unwrap();
		group.write_frame(moq_net::Timestamp::now(), &b"garbage"[..]).unwrap();
		test.failed("video0").await;
	}

	// A record subscription still waiting on its track fails with the broadcast, so nothing
	// parks, and records never read are not listed as an empty finished recording.
	#[tokio::test(start_paused = true)]
	async fn a_pending_record_subscription_ends_with_the_broadcast() {
		let test = Records::new();
		// A handler keeps the unpublished records' requests waiting rather than refused.
		let _handler = test.broadcast.requests();
		kio::clock::sleep(Duration::from_secs(60)).await;
		assert!(!test.rendition("video0").snapshot().finished);

		test.broadcast.close();
		test.failed("video0").await;
		assert!(test.drain_failed("video0").await.is_empty());
	}

	// A recording refuses a listed segment whose media is gone, but reports a segment the
	// rendition holds nothing for (a stop) as a gap that keeps its number, so a recorder can tell
	// it from rows it missed.
	#[tokio::test(start_paused = true)]
	async fn a_recording_refuses_missing_media_but_reports_gaps() {
		let test = Records::new();
		let (_, mut video0) = test.publish("video0");
		let (_, mut video1) = test.publish("video1");
		let media = test.media("video0", 2);
		let sibling = test.media("video1", 2);
		media.finish().unwrap();
		sibling.finish().unwrap();
		for sequence in 0..3 {
			video0.push(&gop(sequence)).unwrap();
		}
		video0.push(&stop(3, 6_000)).unwrap();
		video1.push(&stop(0, 0)).unwrap();
		video1.push(&gop(1)).unwrap();
		video1.push(&stop(2, 4_000)).unwrap();
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

		let snapshot = test.finished("video1").await;
		assert_eq!(gaps(&snapshot), [0, 2]);
		let rendition = test.rendition("video1");
		assert!(
			rendition.listed_segment(0).await.unwrap().is_none(),
			"a gap is never fetched"
		);
		let mut cursor = rendition.segments();
		let mut next = async || {
			let segment = cursor.next().await.unwrap().unwrap();
			(segment.segment, segment.media.is_some())
		};
		assert_eq!(next().await, (0, false));
		assert_eq!(next().await, (1, true));
		assert_eq!(next().await, (2, false));
		assert!(cursor.next().await.unwrap().is_none());
	}

	// Each rendition lists its own records: a ladder whose boundaries and numbering differ, such as
	// a rendition that started late, lists every segment of each, with no gaps.
	#[tokio::test(start_paused = true)]
	async fn an_unaligned_ladder_lists_every_segment() {
		let test = Records::with(&["video0"], &["audio0"]);
		let (_t0, mut video0) = test.publish("video0");
		let (_t1, mut audio0) = test.publish("audio0");
		for sequence in 0..5 {
			video0.push(&gop(sequence)).unwrap();
		}
		// Audio joins at 5.5s, numbering its records from its own start.
		for (sequence, pts) in [(0, 5_500), (1, 8_000), (2, 10_000)] {
			audio0.push(&hang::hls::Record::new(sequence, sequence, pts)).unwrap();
		}
		assert_eq!(numbers(&test.listed("video0", 3).await), [0, 1, 2, 3]);
		let audio = test.listed("audio0", 1).await;
		assert_eq!(numbers(&audio), [0, 1]);
		assert!(gaps(&audio).is_empty());
		let rendition = test.rendition("audio0");
		assert!(rendition.is_advertised());
		let dash = rendition.representation().unwrap().expect("an out-of-band init");
		assert_eq!(dash.segments, [(5_500, 2_500), (8_000, 2_000)]);
	}

	// A break in one rendition starts a discontinuity on it alone: each rendition counts its own.
	#[tokio::test(start_paused = true)]
	async fn a_break_in_one_rendition_leaves_the_others_continuous() {
		let test = Records::new();
		let (_t0, mut video0) = test.publish("video0");
		let (_t1, mut video1) = test.publish("video1");
		for sequence in 0..4 {
			video1.push(&gop(sequence)).unwrap();
		}
		video0.push(&gop(0)).unwrap();
		video0.push(&stop(1, 2_000)).unwrap();
		video0.push(&hang::hls::Record::new(2, 1, 4_000)).unwrap();
		video0.push(&hang::hls::Record::new(3, 2, 6_000)).unwrap();

		let discontinuities = |snapshot: &playlist::Snapshot| {
			snapshot
				.segments
				.iter()
				.filter(|segment| segment.discontinuity)
				.map(|segment| segment.segment)
				.collect::<Vec<_>>()
		};
		assert_eq!(discontinuities(&test.listed("video0", 2).await), [2]);
		assert!(discontinuities(&test.listed("video1", 2).await).is_empty());
	}

	// A stop is listed as a gap at once, and what resumes after it starts a new discontinuity.
	#[tokio::test(start_paused = true)]
	async fn a_stop_is_a_gap_and_a_resume_a_discontinuity() {
		let test = Records::with(&["video0"], &[]);
		let (_track, mut video0) = test.publish("video0");
		video0.push(&gop(0)).unwrap();
		video0.push(&gop(1)).unwrap();
		video0.push(&stop(2, 4_000)).unwrap();
		video0.push(&hang::hls::Record::new(3, 2, 10_000)).unwrap();
		video0.push(&hang::hls::Record::new(4, 3, 12_000)).unwrap();

		let snapshot = test.listed("video0", 3).await;
		assert_eq!(numbers(&snapshot), [0, 1, 2, 3]);
		assert_eq!(gaps(&snapshot), [2]);
		let rendered = test
			.rendition("video0")
			.media_playlist(None)
			.unwrap()
			.expect("playable");
		assert!(rendered.contains("#EXT-X-GAP\n"), "{rendered}");
		assert!(rendered.contains("#EXT-X-DISCONTINUITY\n"), "{rendered}");
	}

	// A record whose successor goes backwards can't be bounded, but keeps its number as a gap:
	// a hole would renumber every later URI under a player.
	#[tokio::test(start_paused = true)]
	async fn a_record_its_successor_cannot_bound_is_a_gap() {
		let test = Records::with(&["video0"], &[]);
		let (_track, mut video0) = test.publish("video0");
		video0.push(&gop(0)).unwrap();
		// Record 2 starts at an earlier group than record 1.
		video0.push(&hang::hls::Record::new(1, 2, 4_000)).unwrap();
		video0.push(&hang::hls::Record::new(2, 1, 5_000)).unwrap();
		video0.push(&hang::hls::Record::new(3, 3, 6_000)).unwrap();

		let snapshot = test.listed("video0", 2).await;
		assert_eq!(numbers(&snapshot), [0, 1, 2]);
		assert_eq!(gaps(&snapshot), [1]);
		let rendered = test
			.rendition("video0")
			.media_playlist(None)
			.unwrap()
			.expect("playable");
		assert!(rendered.contains("#EXT-X-DISCONTINUITY\n"), "{rendered}");
	}

	// A successor starting at the same group leaves the record no group to serve, so it is a gap
	// rather than an advertised segment that can only 404.
	#[tokio::test(start_paused = true)]
	async fn a_record_its_successor_leaves_no_group_is_a_gap() {
		let test = Records::with(&["video0"], &[]);
		let (_track, mut video0) = test.publish("video0");
		video0.push(&gop(0)).unwrap();
		video0.push(&hang::hls::Record::new(1, 1, 2_000)).unwrap();
		video0.push(&hang::hls::Record::new(2, 1, 3_000)).unwrap();
		video0.push(&hang::hls::Record::new(3, 2, 4_000)).unwrap();

		let snapshot = test.listed("video0", 2).await;
		assert_eq!(numbers(&snapshot), [0, 1, 2]);
		assert_eq!(gaps(&snapshot), [1]);
	}

	#[tokio::test(start_paused = true)]
	async fn catalog_changes_report_removal_and_drain_old_cursors() {
		for reconfigure in [false, true] {
			let mut test = Records::with(&["video0"], &[]);
			let (_, mut records) = test.publish("video0");
			let _media = test.media("video0", 3);
			records.push(&gop(0)).unwrap();
			records.push(&gop(1)).unwrap();
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
			// The segment still waiting on its successor is listed once that arrives: the next
			// segment of a reconfigured rendition, or the stop of a dropped one. What bounds it
			// is no longer the old instance's.
			match reconfigure {
				true => records.push(&gop(2)).unwrap(),
				false => records.push(&stop(2, 4_000)).unwrap(),
			}
			assert_eq!(old.next().await.unwrap().unwrap().segment, 1);
			assert!(old.next().await.unwrap().is_none());
			if reconfigure {
				let Some(renditions::Event::Added(new)) = changes.next().await.unwrap() else {
					panic!("the replacement is reported");
				};
				// The replacement muxes the same records differently, so it lists them under its
				// own URLs, and only those its predecessor didn't.
				assert_ne!(new.tag(), rendition.tag(), "one URL never names two muxes");
				records.push(&gop(3)).unwrap();
				assert_eq!(numbers(&test.listed("video0", 2).await), [2]);
			}
		}
	}

	// A replacement reading another record track shares no numbering with its predecessor, so it
	// lists that track's records from the first.
	#[tokio::test(start_paused = true)]
	async fn a_replacement_on_another_record_track_lists_all_of_it() {
		let mut test = Records::with(&["video0"], &[]);
		let (_, mut records) = test.publish("video0");
		let _media = test.media("video0", 4);
		for sequence in 0..4 {
			records.push(&gop(sequence)).unwrap();
		}
		test.listed("video0", 2).await;

		test.catalog.video.renditions.get_mut("video0").unwrap().hls = Some(hls("video0b"));
		let (_, mut fresh) = test.publish("video0b");
		for sequence in 0..2 {
			fresh.push(&gop(sequence)).unwrap();
		}
		test.sync();
		assert_eq!(numbers(&test.listed("video0", 0).await), [0]);
	}

	// EXT-X-TARGETDURATION is the declared duration rounded up, however long the segments in the
	// window are. A segment that overruns it once rounded to the nearest second is listed anyway,
	// and counted for the warning.
	#[tokio::test(start_paused = true)]
	async fn the_target_duration_is_the_declared_one() {
		let test = Records::with(&["video0"], &[]);
		let (_, mut records) = test.publish("video0");
		for (sequence, pts) in (0..).zip([0, 2_499, 4_999, 6_000, 8_000]) {
			records.push(&hang::hls::Record::new(sequence, sequence, pts)).unwrap();
		}
		let snapshot = test.listed("video0", 3).await;
		let durations: Vec<_> = snapshot.segments.iter().map(|s| s.duration.as_millis()).collect();
		assert_eq!(durations, [2_499, 2_500, 1_001, 2_000]);
		assert_eq!(snapshot.target_duration, 2);
		assert_eq!(
			test.rendition("video0").overruns(),
			1,
			"only the 2.5s segment rounds past the target"
		);
	}

	// Each playlist URI keeps the EXT-X-TARGETDURATION it first got. The first renditions listed
	// share the longest duration they declare, which floors every later target; one added later
	// that declares longer segments raises only its own, and a reconfigure keeps the original.
	#[tokio::test(start_paused = true)]
	async fn each_playlist_keeps_its_target_duration() {
		let declare = |name: &str, secs: u64| {
			Some(hang::catalog::Hls::new(
				hang::hls::default_name(name),
				Duration::from_secs(secs),
			))
		};
		let mut test = Records::with(&[], &[]);
		let mut video = video_config();
		video.hls = declare("video0", 2);
		test.catalog.video.renditions.insert("video0".into(), video);
		let mut audio = opus();
		audio.hls = declare("audio0", 1);
		test.catalog.audio.renditions.insert("audio0".into(), audio);
		test.sync();
		let target = |name: &str| test.rendition(name).target_duration();
		assert_eq!((target("video0"), target("audio0")), (2, 2));

		let mut late = video_config();
		late.hls = declare("video1", 4);
		test.catalog.video.renditions.insert("video1".into(), late.clone());
		test.sync();
		late.hls = declare("video2", 2);
		test.catalog.video.renditions.insert("video2".into(), late);
		test.sync();
		let target = |name: &str| test.rendition(name).target_duration();
		assert_eq!(
			[target("video0"), target("audio0"), target("video1"), target("video2")],
			[2, 2, 4, 2]
		);

		let original = test.rendition("video0");
		test.catalog.video.renditions.get_mut("video0").unwrap().hls = declare("video0", 6);
		test.sync();
		let replacement = test.rendition("video0");
		assert!(!Arc::ptr_eq(&original, &replacement), "a new duration reconfigures");
		assert_eq!(replacement.target_duration(), 2);
		drop((original, replacement));

		// A removed rendition that returns serves the same playlist URI. Nothing of it is kept,
		// but the target derives from its declared duration and the first batch's, so one that
		// returns declaring what it first did comes back the same, even after a longer rendition
		// registers in between. One whose reconfigure kept an older target, though, returns with
		// the target its current declaration derives: the price of keeping no state per name.
		let video0 = test.catalog.video.renditions.remove("video0").unwrap();
		let video1 = test.catalog.video.renditions.remove("video1").unwrap();
		test.sync();
		let mut longer = video_config();
		longer.hls = declare("video3", 8);
		test.catalog.video.renditions.insert("video3".into(), longer);
		test.sync();
		test.catalog.video.renditions.insert("video0".into(), video0);
		test.catalog.video.renditions.insert("video1".into(), video1);
		test.sync();
		let target = |name: &str| test.rendition(name).target_duration();
		assert_eq!([target("video1"), target("video3"), target("video0")], [4, 8, 6]);
	}

	/// The `Push` events a fresh subscriber to `track`'s records reads before it is caught up.
	async fn fresh_join_reads(test: &Records, track: &str) -> usize {
		let mut records = moq_mux::hls::Consumer::subscribe(&test.broadcast.consume(), &hls(track))
			.await
			.unwrap();
		let mut pushes = 0;
		while let std::task::Poll::Ready(event) = records.poll_next(&kio::Waiter::noop()) {
			let event = event.unwrap().expect("the records are still live");
			pushes += matches!(event, moq_mux::hls::Event::Push { .. }) as usize;
		}
		pushes
	}

	// A viewer joining a days-old live broadcast costs what the window lists, not the broadcast's
	// age: its record subscription restates a bounded suffix of records that never pop, and its
	// playlist lists the window with the sequence every other edge shows.
	#[tokio::test(start_paused = true)]
	async fn a_fresh_join_reads_a_bounded_number_of_records() {
		let early = Records::with(&["video0"], &[]);
		let (_track, mut video0) = early.publish("video0");
		// A day of 10s GOPs per round.
		const DAY: u64 = 86_400 / 10;
		// A checkpoint, plus the edits one record group holds before a join over its ratio rolls it.
		let ratio = moq_mux::hls::Producer::info().refresh.unwrap() as usize;
		let bound = (segments::MAX_SEGMENTS + 1) * (1 + ratio);
		for day in 1..=3 {
			for sequence in (day - 1) * DAY..day * DAY {
				video0
					.push(&hang::hls::Record::new(sequence, sequence, sequence * 10_000))
					.unwrap();
			}
			let read = fresh_join_reads(&early, "video0").await;
			assert!(
				(segments::MAX_SEGMENTS..=bound).contains(&read),
				"day {day}: a fresh join read {read} records"
			);
		}

		let last = 3 * DAY - 2;
		let late = early.join();
		let joined = late.listed("video0", last).await;
		let followed = early.listed("video0", last).await;
		// Two 10s segments cover the 16s window.
		assert_eq!(numbers(&joined), [last - 1, last]);
		assert_eq!(numbers(&followed), numbers(&joined));
		assert_eq!(followed.media_sequence, joined.media_sequence);
	}

	// Very short segments: the window is capped at what a fresh join can see, so an edge that
	// followed the broadcast all along lists exactly what one joining now does.
	#[tokio::test(start_paused = true)]
	async fn dense_records_list_the_same_window_on_every_edge() {
		let early = Records::with(&["video0"], &[]);
		let (_track, mut video0) = early.publish("video0");
		// 40ms records: the 16s window spans 400 of them.
		for sequence in 0..1_000 {
			video0
				.push(&hang::hls::Record::new(sequence, sequence, sequence * 40))
				.unwrap();
			settle().await;
		}
		// A joiner wants a fresh group, so the next push restates the window and the joiner reads
		// only that checkpoint and on.
		let late = early.join();
		settle().await;
		video0.push(&hang::hls::Record::new(1_000, 1_000, 40_000)).unwrap();
		assert!(
			fresh_join_reads(&early, "video0").await < 1_000,
			"the push rolled the group"
		);

		let joined = late.listed("video0", 999).await;
		let followed = early.listed("video0", 999).await;
		assert_eq!(joined.segments.len(), segments::MAX_SEGMENTS);
		assert_eq!(numbers(&followed), numbers(&joined));
		assert_eq!(followed.media_sequence, joined.media_sequence);
	}

	// Two edges over one record track agree on the media sequence.
	#[tokio::test(start_paused = true)]
	async fn edges_agree_on_the_media_sequence() {
		let early = Records::with(&["video0"], &[]);
		let (_track, mut video0) = early.publish("video0");
		for sequence in 0..21 {
			video0.push(&gop(sequence)).unwrap();
			settle().await;
		}
		let late = early.join();
		let joined = late.listed("video0", 19).await;
		let followed = early.listed("video0", 19).await;
		assert_eq!(joined.media_sequence, 12, "eight 2s segments cover the 16s window");
		assert_eq!(numbers(&followed), numbers(&joined));
	}
}
