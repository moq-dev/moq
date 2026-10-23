//! Serving a `moq-archive` recording as HLS.
//!
//! A source broadcast, its catalog and timelines included, is recorded by an
//! [`moq_archive::Producer`] and served back by an [`moq_archive::Consumer`], so the exporter
//! serves the archived catalog exactly like a live broadcast. These tests pin the storage traffic
//! that composition produces: playlists read only the timelines, and a segment GETs only objects
//! of the requested rendition.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use futures::TryStreamExt;
use futures::stream::BoxStream;
use hang::timeline::{Position, Record};
use moq_archive::Store;
use moq_archive::object_store::memory::InMemory;
use moq_archive::object_store::path::Path;
use moq_archive::object_store::{
	self, CopyOptions, GetOptions, GetResult, ListResult, MultipartUpload, ObjectMeta, ObjectStore, ObjectStoreExt,
	PutMultipartOptions, PutOptions, PutPayload, PutResult,
};
use moq_json::window;
use moq_net::{Timescale, Timestamp, group, track};

use super::*;

/// In-memory store that records every GET path.
#[derive(Debug, Clone, Default)]
struct Counting {
	inner: Arc<InMemory>,
	gets: Arc<Mutex<Vec<String>>>,
	/// Fail every media GET, so nothing can quietly depend on one.
	reject_media: Arc<AtomicBool>,
}

impl Counting {
	/// Every GET since the last call.
	fn take(&self) -> Vec<String> {
		std::mem::take(&mut *self.gets.lock().unwrap())
	}

	fn reject_media(&self, reject: bool) {
		self.reject_media.store(reject, Ordering::SeqCst);
	}
}

impl std::fmt::Display for Counting {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "Counting")
	}
}

#[async_trait::async_trait]
impl ObjectStore for Counting {
	async fn put_opts(
		&self,
		location: &Path,
		payload: PutPayload,
		opts: PutOptions,
	) -> object_store::Result<PutResult> {
		self.inner.put_opts(location, payload, opts).await
	}

	async fn put_multipart_opts(
		&self,
		location: &Path,
		opts: PutMultipartOptions,
	) -> object_store::Result<Box<dyn MultipartUpload>> {
		self.inner.put_multipart_opts(location, opts).await
	}

	async fn get_opts(&self, location: &Path, options: GetOptions) -> object_store::Result<GetResult> {
		self.gets.lock().unwrap().push(location.to_string());
		if self.reject_media.load(Ordering::SeqCst) && is_media(location.as_ref()) {
			return Err(object_store::Error::NotImplemented {
				operation: "media GET".into(),
				implementer: "Counting".into(),
			});
		}
		self.inner.get_opts(location, options).await
	}

	fn delete_stream(
		&self,
		locations: BoxStream<'static, object_store::Result<Path>>,
	) -> BoxStream<'static, object_store::Result<Path>> {
		self.inner.delete_stream(locations)
	}

	fn list(&self, prefix: Option<&Path>) -> BoxStream<'static, object_store::Result<ObjectMeta>> {
		self.inner.list(prefix)
	}

	async fn list_with_delimiter(&self, prefix: Option<&Path>) -> object_store::Result<ListResult> {
		self.inner.list_with_delimiter(prefix).await
	}

	async fn copy_opts(&self, from: &Path, to: &Path, options: CopyOptions) -> object_store::Result<()> {
		self.inner.copy_opts(from, to, options).await
	}
}

/// One rendition of the source: its media track, and its timeline track with the window encoder
/// that writes it.
struct Source {
	media: track::Producer,
	timeline: track::Producer,
	encoder: window::Encoder<Record>,
	/// The timeline group still open, which a pop may extend.
	group: Option<group::Producer>,
	/// Next timeline group sequence.
	next: u64,
	/// Next record sequence.
	record: u64,
}

/// A source broadcast recorded into a store as it publishes.
struct Recording {
	store: Store<Counting>,
	broadcast: moq_net::broadcast::Producer,
	catalog: moq_json::snapshot::Producer<hang::Catalog>,
	sources: HashMap<String, Source>,
	recorder: tokio::task::JoinHandle<moq_archive::Result<()>>,
}

impl Recording {
	/// `archive` is the catalog's root entry; `checkpoint` bounds each timeline's checkpoint.
	async fn new(archive: hang::catalog::Archive, checkpoint: Option<usize>) -> Self {
		let store = Store::new(Counting::default(), "rec");
		let broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast
			.create_track(hang::Catalog::DEFAULT_NAME, hang::Catalog::default_track_info())
			.unwrap();
		let mut json = moq_json::snapshot::Config::default();
		json.deltas = false;
		let mut catalog = moq_json::snapshot::Producer::new(track, json);
		catalog
			.update(moq_net::Timed::from(&self::catalog(archive)).at(Timestamp::now()))
			.unwrap();

		let mut sources = HashMap::new();
		for name in ["360p", "1080p", "audio"] {
			// Legacy media is stamped in microseconds.
			let media = track::Info::default()
				.with_timescale(Timescale::new(1_000_000).unwrap())
				.with_priority(1);
			let media = broadcast.create_track(name, media).unwrap();
			let timeline = track::Info::default().with_timescale(Timescale::MILLI);
			let timeline = broadcast.create_track(timeline_name(name), timeline).unwrap();
			let mut config = window::ProducerConfig::default()
				.with_compression(true)
				.with_ops(false);
			if let Some(limit) = checkpoint {
				config = config.with_checkpoint_records(limit);
			}
			let source = Source {
				media,
				timeline,
				encoder: window::Encoder::new(config),
				group: None,
				next: 0,
				record: 0,
			};
			sources.insert(name.to_string(), source);
		}

		// The source publishes in bursts, so the budget covers a whole recording.
		let config = moq_archive::producer::Config::default()
			.with_flush(Duration::from_millis(100))
			.with_max_delay(Duration::from_secs(3600));
		let recorder = tokio::spawn(moq_archive::Producer::new(store.clone(), broadcast.consume(), config).run());
		// Subscribed before anything is published, so every group is recorded live.
		for source in sources.values() {
			while source.media.subscription().is_none() || source.timeline.subscription().is_none() {
				tokio::time::sleep(Duration::from_millis(1)).await;
			}
		}
		Self {
			store,
			broadcast,
			catalog,
			sources,
			recorder,
		}
	}

	/// Publish `track`'s next record: `groups`, each a list of frame timestamps in micros,
	/// spanning `pts..pts + duration` in milliseconds, popping `pop` older records.
	fn add(&mut self, track: &str, pts: u64, duration: u64, groups: &[(u64, &[u64])], pop: u64) {
		let source = self.sources.get_mut(track).unwrap();
		for &(sequence, frames) in groups {
			let mut group = source.media.create_group(group::Info { sequence }).unwrap();
			for (index, &micros) in frames.iter().enumerate() {
				let timestamp = Timestamp::from_micros(micros).unwrap();
				group.write_frame(timestamp, legacy(track, micros, index == 0)).unwrap();
			}
			group.finish().unwrap();
		}

		let sequence = source.record;
		source.record += 1;
		let first = groups.first().unwrap().0;
		let last = groups.last().unwrap().0;
		let record = Record::new(
			sequence,
			pts,
			duration,
			Position::group(first),
			Position::group(last + 1),
		);
		let mut payloads = Vec::new();
		let pending = source.encoder.push(&record).unwrap();
		payloads.push((pending.keyframe, pending.payload.clone()));
		pending.commit();
		if let Some(pending) = source.encoder.pop(pop).unwrap() {
			payloads.push((pending.keyframe, pending.payload.clone()));
			pending.commit();
		}
		for (keyframe, payload) in payloads {
			if keyframe || source.group.is_none() {
				if let Some(mut group) = source.group.take() {
					group.finish().unwrap();
				}
				let group = source
					.timeline
					.create_group(group::Info { sequence: source.next })
					.unwrap();
				source.group = Some(group);
				source.next += 1;
			}
			let timestamp = Timestamp::from_millis(pts).unwrap();
			source.group.as_mut().unwrap().write_frame(timestamp, payload).unwrap();
		}
	}

	/// Wait until the recorder subscribed to every track and stored what was published.
	async fn stored(&self) {
		tokio::time::sleep(Duration::from_secs(5)).await;
	}

	/// End the source cleanly, then wait for the recorder to mark the archive complete.
	async fn finish(mut self) -> Store<Counting> {
		self.catalog.finish().unwrap();
		for source in self.sources.values_mut() {
			if let Some(mut group) = source.group.take() {
				group.finish().unwrap();
			}
			source.media.finish().unwrap();
			source.timeline.finish().unwrap();
		}
		self.stored().await;
		self.broadcast.close();
		self.recorder.await.unwrap().unwrap();
		self.store
	}

	/// Every GET since the last call.
	fn gets(&self) -> Vec<String> {
		self.store.inner().take()
	}
}

fn timeline_name(track: &str) -> String {
	hang::timeline::default_name(track)
}

/// One Legacy frame: a VP8 keyframe for video (geometry the muxer can parse), filler otherwise.
fn legacy(track: &str, micros: u64, keyframe: bool) -> Bytes {
	let payload: &'static [u8] = match (track.starts_with("audio"), keyframe) {
		(true, _) => &[0xFC, 0xFF, 0xFE],
		(false, true) => &[0x10, 0x00, 0x00, 0x9d, 0x01, 0x2a, 0x40, 0x01, 0xf0, 0x00],
		(false, false) => &[0x31, 0x00, 0x00],
	};
	let frame = hang::container::Frame {
		timestamp: Timestamp::from_micros(micros).unwrap(),
		payload: Bytes::from_static(payload),
	};
	let mut encoded = BytesMut::new();
	frame.encode(&mut encoded).unwrap();
	encoded.freeze()
}

/// A live-style archive entry naming every rendition's timeline.
fn live() -> hang::catalog::Archive {
	let mut archive = hang::catalog::Archive::new();
	for track in ["360p", "1080p", "audio"] {
		archive.timelines.insert(
			track.to_string(),
			hang::catalog::Timeline::new(timeline_name(track), 4_000),
		);
	}
	archive
}

/// The archive entry of a source that declares itself durable: its timelines, in a store.
fn durable() -> hang::catalog::Archive {
	let mut archive = live();
	archive.store = Some("memory:///rec/".parse().unwrap());
	archive.version = Some(hang::catalog::Archive::VERSION);
	archive
}

/// The source's catalog: every rendition's config, plus `archive`. Out-of-band configs, so no
/// init needs media.
fn catalog(archive: hang::catalog::Archive) -> hang::Catalog {
	let mut catalog = hang::Catalog::default();
	catalog.archive = Some(archive);
	for (name, width, height) in [("360p", 640, 360), ("1080p", 1920, 1080)] {
		let mut config = hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::VP8);
		config.coded_width = Some(width);
		config.coded_height = Some(height);
		catalog.video.renditions.insert(name.to_string(), config);
	}
	let audio = hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Opus, 48_000, 2);
	catalog.audio.renditions.insert("audio".to_string(), audio);
	catalog
}

/// An archive served back through an origin by a consumer, and a broadcaster exporting it.
struct Replay {
	broadcaster: Arc<Broadcaster>,
	origin: moq_net::origin::Producer,
	consumer: tokio::task::JoinHandle<moq_archive::Result<()>>,
	driver: tokio::task::JoinHandle<moq_net::Error>,
}

impl Replay {
	async fn open(store: &Store<Counting>, cache: u64) -> Self {
		Self::open_with(store, cache, Config::default()).await
	}

	async fn open_with(store: &Store<Counting>, cache: u64, config: Config) -> Self {
		let (origin, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::default());
		let driver = tokio::spawn(moq_net::time::run(driver));
		let broadcast = origin.create_broadcast("rec").unwrap();
		let serving = moq_archive::consumer::Config::default().with_cache(cache);
		let consumer = moq_archive::Consumer::open(store.clone(), broadcast.clone(), serving)
			.await
			.unwrap();
		let consumer = tokio::spawn(consumer.run());
		broadcast.announce(Default::default()).unwrap();

		let broadcaster = Broadcaster::new(crate::resolved(&origin, "rec").await, config);
		tokio::time::timeout(Duration::from_secs(5), broadcaster.ready())
			.await
			.expect("a rendition becomes playable or fails");

		Self {
			broadcaster,
			origin,
			consumer,
			driver,
		}
	}

	fn rendition(&self, kind: Kind, name: &str) -> Arc<Rendition> {
		self.broadcaster
			.rendition(kind, name)
			.expect("rendition in the catalog")
	}

	async fn playlist(&self, kind: Kind, name: &str) -> String {
		let rendition = self.rendition(kind, name);
		tokio::time::timeout(Duration::from_secs(5), rendition.playlist(None, None))
			.await
			.expect("playlist renders")
			.unwrap()
			.expect("playlist is servable")
	}

	/// Wait until `name`'s playlist satisfies `ready`, then return it.
	async fn playlist_until(&self, kind: Kind, name: &str, ready: impl Fn(&str) -> bool) -> String {
		for _ in 0..500 {
			let playlist = self.playlist(kind, name).await;
			if ready(&playlist) {
				return playlist;
			}
			tokio::time::sleep(Duration::from_millis(10)).await;
		}
		panic!("{name} playlist never became ready");
	}

	/// Wait until `name`'s playlist answers a failed timeline, then return that error.
	async fn failure(&self, kind: Kind, name: &str) -> crate::Error {
		let rendition = self.rendition(kind, name);
		for _ in 0..500 {
			let answer = tokio::time::timeout(Duration::from_secs(5), rendition.playlist(None, None))
				.await
				.expect("playlist answers");
			if let Err(err) = answer {
				assert!(matches!(err, crate::Error::TimelineFailed { .. }), "{name}: {err}");
				return err;
			}
			tokio::time::sleep(Duration::from_millis(10)).await;
		}
		panic!("{name} playlist never failed");
	}
}

impl Drop for Replay {
	fn drop(&mut self) {
		self.consumer.abort();
		self.driver.abort();
	}
}

/// A stored media object: neither a timeline, the catalog, nor a marker.
fn is_media(path: &str) -> bool {
	let name = path.rsplit('/').next().unwrap_or_default();
	let marker = [".info", "end", ".end"].contains(&name);
	!marker && !path.contains("timeline") && !path.contains("catalog")
}

/// The keys of `track`'s stored objects, oldest first, with the first group each holds.
async fn objects(store: &Store<Counting>, track: &str) -> Vec<(u64, Path)> {
	// The names here escape only `.`, as a track directory spells it.
	let encoded = track.replace('.', "%2E");
	let dir = Path::parse(format!("rec/{encoded}")).unwrap();
	let metas: Vec<ObjectMeta> = store.inner().list(Some(&dir)).try_collect().await.unwrap();
	let mut objects: Vec<(u64, Path)> = metas
		.into_iter()
		.filter_map(|meta| {
			let name = meta.location.filename()?;
			let group = name.split('-').next()?.parse().ok()?;
			Some((group, meta.location))
		})
		.collect();
	objects.sort();
	objects
}

/// `count` 2s segments: one keyframe group per video record, four audio groups per audio record.
/// The first video rendition by name, 1080p, is the reference. `checkpoint` bounds each
/// timeline's checkpoint.
async fn segments(count: u64, checkpoint: Option<usize>) -> Recording {
	let mut recording = Recording::new(durable(), checkpoint).await;
	for segment in 0..count {
		add_segment(&mut recording, segment, 0);
	}
	recording
}

fn add_segment(recording: &mut Recording, segment: u64, pop: u64) {
	let pts = segment * 2_000_000;
	for video in ["360p", "1080p"] {
		recording.add(video, segment * 2000, 2000, &[(segment, &[pts, pts + 1_000_000])], pop);
	}
	let audio: Vec<(u64, [u64; 1])> = (0..4).map(|i| (segment * 4 + i, [pts + i * 500_000])).collect();
	let audio: Vec<(u64, &[u64])> = audio
		.iter()
		.map(|(sequence, frames)| (*sequence, &frames[..]))
		.collect();
	recording.add("audio", segment * 2000, 2000, &audio, pop);
}

#[tokio::test(start_paused = true)]
async fn playlists_read_only_timelines_and_segments_their_own_objects() {
	let recording = segments(3, None).await;
	recording.stored().await;
	let replay = Replay::open(&recording.store, 64 * 1024 * 1024).await;

	let master = replay.broadcaster.master_playlist(None);
	assert!(master.contains("video/360p/media.m3u8") && master.contains("video/1080p/media.m3u8"));

	// Render and reload every playlist with media GETs refused: shared numbering, and not one
	// media GET. The reference lists every segment; the others hold back the newest until their
	// own timeline passes its end, or ends.
	recording.store.inner().reject_media(true);
	for _ in 0..2 {
		for (kind, name, listed) in [
			(Kind::Video, "1080p", 3),
			(Kind::Video, "360p", 2),
			(Kind::Audio, "audio", 3),
		] {
			let playlist = replay.playlist(kind, name).await;
			for segment in 0..3 {
				let line = format!("seg/29b1cd67.{segment}.m4s\n");
				assert_eq!(playlist.contains(&line), segment < listed, "{name}: {playlist}");
			}
			assert!(!playlist.contains("#EXT-X-ENDLIST"), "the recording is still growing");
		}
	}
	let gets = recording.gets();
	for rendition in ["360p", "1080p", "audio"] {
		assert!(
			gets.contains(&format!("rec/{rendition}/.info")),
			"track properties bind before media: {gets:?}"
		);
	}
	assert!(gets.iter().any(|path| path.contains("timeline")), "{gets:?}");
	recording.store.inner().reject_media(false);

	// Switching renditions downloads only the selected rendition's objects.
	for (kind, name, segment) in [
		(Kind::Video, "360p", 1),
		// Not the newest: a growing archive keeps its newest group open, as a live track does.
		(Kind::Video, "1080p", 1),
		// An audio segment spans four groups.
		(Kind::Audio, "audio", 1),
	] {
		let media = replay
			.rendition(kind, name)
			.listed_segment(segment)
			.await
			.unwrap()
			.unwrap();
		assert_eq!(&media[4..8], b"moof");
		let gets = recording.gets();
		assert!(!gets.is_empty(), "{name}");
		assert!(
			gets.iter().all(|path| path.starts_with(&format!("rec/{name}/"))),
			"{name}: {gets:?}"
		);
	}

	// A repeated request hits the consumer's cache.
	replay
		.rendition(Kind::Video, "360p")
		.listed_segment(1)
		.await
		.unwrap()
		.unwrap();
	assert_eq!(recording.gets(), Vec::<String>::new());
}

#[tokio::test(start_paused = true)]
async fn a_bounded_cache_rereads_evicted_objects() {
	let recording = segments(3, None).await;
	recording.stored().await;
	// Too small for any object, so nothing stays cached.
	let replay = Replay::open(&recording.store, 1).await;
	replay.playlist(Kind::Audio, "audio").await;
	recording.gets();

	// Each of the segment's four groups misses the cache and GETs its object again.
	let audio = replay
		.rendition(Kind::Audio, "audio")
		.listed_segment(1)
		.await
		.unwrap()
		.unwrap();
	assert_eq!(&audio[4..8], b"moof");
	let gets: Vec<String> = recording.gets().into_iter().filter(|path| is_media(path)).collect();
	assert_eq!(gets.len(), 4, "{gets:?}");
}

#[tokio::test(start_paused = true)]
async fn missing_rendition_segments_are_gaps_and_time_jumps_are_discontinuities() {
	let mut recording = Recording::new(durable(), None).await;
	recording.add("1080p", 0, 2000, &[(0, &[0])], 0);
	recording.add("360p", 0, 8000, &[(0, &[0])], 0);
	recording.add("audio", 0, 2000, &[(0, &[0])], 0);
	// 360p stored nothing for segment 1.
	recording.add("1080p", 2000, 2000, &[(1, &[2_000_000])], 0);
	// Content time jumps from 4s to 10s.
	recording.add("1080p", 10_000, 2000, &[(2, &[10_000_000])], 0);
	recording.add("360p", 10_000, 2000, &[(1, &[10_000_000])], 0);
	// The completed archive resolves each rendition's newest segment.
	let store = recording.finish().await;

	let replay = Replay::open(&store, 64 * 1024 * 1024).await;
	let low = replay
		.playlist_until(Kind::Video, "360p", |playlist| playlist.contains("#EXT-X-ENDLIST"))
		.await;
	let expected = concat!(
		"#EXTINF:2.00000,\nseg/29b1cd67.0.m4s\n",
		"#EXT-X-GAP\n#EXTINF:2.00000,\nseg/29b1cd67.1.m4s\n",
		"#EXT-X-DISCONTINUITY\n#EXTINF:2.00000,\nseg/29b1cd67.2.m4s\n",
	);
	assert!(low.contains(expected), "{low}");
	let high = replay.playlist(Kind::Video, "1080p").await;
	assert!(!high.contains("#EXT-X-GAP"), "{high}");
	assert!(
		high.contains("#EXT-X-DISCONTINUITY\n#EXTINF:2.00000,\nseg/29b1cd67.2.m4s\n"),
		"{high}"
	);

	// A gap is never fetched.
	store.inner().take();
	let rendition = replay.rendition(Kind::Video, "360p");
	assert!(rendition.listed_segment(1).await.unwrap().is_none());
	assert!(!store.inner().take().iter().any(|path| is_media(path)));
	let after = rendition.listed_segment(2).await.unwrap().unwrap();
	assert_eq!(&after[4..8], b"moof");
}

/// A growing recording is followed as the archive grows, and ends once its producer marks it
/// complete.
#[tokio::test(start_paused = true)]
async fn a_growing_recording_ends_with_its_markers() {
	let mut recording = segments(3, None).await;
	recording.stored().await;
	let replay = Replay::open(&recording.store, 64 * 1024 * 1024).await;
	replay.playlist(Kind::Video, "1080p").await;

	// A DVR commit: segment 3 arrives and segment 0 expires.
	add_segment(&mut recording, 3, 1);
	recording.stored().await;
	let playlist = replay
		.playlist_until(Kind::Video, "1080p", |playlist| {
			playlist.contains("seg/29b1cd67.3.m4s\n")
		})
		.await;
	assert!(playlist.contains("#EXT-X-MEDIA-SEQUENCE:1\n"), "{playlist}");
	assert!(!playlist.contains("seg/29b1cd67.0.m4s"), "{playlist}");
	assert!(
		!playlist.contains("#EXT-X-ENDLIST"),
		"a recording without markers stays live"
	);

	recording.finish().await;
	let playlist = replay
		.playlist_until(Kind::Video, "1080p", |playlist| playlist.contains("#EXT-X-ENDLIST"))
		.await;
	assert!(playlist.contains("seg/29b1cd67.3.m4s\n#EXT-X-ENDLIST\n"), "{playlist}");
}

/// A durable timeline still lists only the default 16s window: a live playlist stays bounded
/// however much the store retains.
#[tokio::test(start_paused = true)]
async fn a_durable_timeline_lists_the_window() {
	let recording = segments(12, None).await;
	recording.stored().await;
	let replay = Replay::open(&recording.store, 64 * 1024 * 1024).await;
	let playlist = replay
		.playlist_until(Kind::Video, "1080p", |playlist| {
			playlist.contains("seg/29b1cd67.11.m4s\n")
		})
		.await;
	// Eight 2s segments cover the window.
	assert!(playlist.contains("#EXT-X-MEDIA-SEQUENCE:4\n"), "{playlist}");
	assert!(!playlist.contains("seg/29b1cd67.3.m4s\n"), "{playlist}");
}

fn history() -> Config {
	Config {
		history: true,
		..Config::default()
	}
}

/// History mode lists a durable timeline's whole recording from the archive, and DASH offers
/// the whole listed span. A live-style entry, or a `replay` path that moves the durable ranges to
/// another broadcast, keeps the window even then.
#[tokio::test(start_paused = true)]
async fn history_mode_lists_a_durable_timeline_past_the_window() {
	let recording = segments(12, Some(4)).await;
	let store = recording.finish().await;
	let replay = Replay::open_with(&store, 64 * 1024 * 1024, history()).await;
	// Every rendition reads its own timeline's history, not only the reference: otherwise the
	// reference's early rows resolve as gaps on its siblings.
	for (kind, name) in [(Kind::Video, "1080p"), (Kind::Video, "360p"), (Kind::Audio, "audio")] {
		let playlist = replay
			.playlist_until(kind, name, |playlist| playlist.contains("#EXT-X-ENDLIST"))
			.await;
		assert!(playlist.contains("#EXT-X-MEDIA-SEQUENCE:0\n"), "{name}: {playlist}");
		assert!(playlist.contains(".0.m4s\n"), "{name}: {playlist}");
		assert!(playlist.contains(".11.m4s\n"), "{name}: {playlist}");
		assert!(!playlist.contains("#EXT-X-GAP"), "{name}: {playlist}");
		assert!(replay.rendition(kind, name).listed_segment(0).await.unwrap().is_some());
		replay.rendition(kind, name).init().await.unwrap();
	}
	let manifest = replay.broadcaster.manifest(None).unwrap().expect("manifest renders");
	// The completed recording is a static manifest over the whole span.
	assert!(
		manifest.contains("mediaPresentationDuration=\"PT24.000S\""),
		"{manifest}"
	);

	let mut elsewhere = durable();
	elsewhere.replay = Some(moq_net::path::RelativeOwned::new("./recording"));
	for archive in [live(), elsewhere] {
		let mut recording = Recording::new(archive, Some(4)).await;
		for segment in 0..12 {
			add_segment(&mut recording, segment, 0);
		}
		let store = recording.finish().await;
		let windowed = Replay::open_with(&store, 64 * 1024 * 1024, history()).await;
		let playlist = windowed
			.playlist_until(Kind::Video, "1080p", |playlist| {
				playlist.contains("seg/29b1cd67.11.m4s\n")
			})
			.await;
		assert!(!playlist.contains("seg/29b1cd67.0.m4s\n"), "{playlist}");
	}
}

/// A media-time range lists the overlapping durable segments, uses their source sequence, and
/// becomes a VOD playlist once the requested end is covered. Independent edges agree.
#[tokio::test(start_paused = true)]
async fn range_playlist_clips_durable_history() {
	let recording = segments(12, Some(4)).await;
	recording.stored().await;
	let first = Replay::open_with(&recording.store, 64 * 1024 * 1024, history()).await;
	let second = Replay::open_with(&recording.store, 64 * 1024 * 1024, history()).await;
	for replay in [&first, &second] {
		replay
			.playlist_until(Kind::Video, "1080p", |playlist| {
				playlist.contains("seg/29b1cd67.11.m4s\n")
			})
			.await;
	}
	let range = super::Range::new(Duration::from_secs(5), Duration::from_secs(9)).unwrap();
	let playlist = |replay: &Replay| {
		replay
			.rendition(Kind::Video, "1080p")
			.range_playlist(range, Some("jwt=clip"))
			.unwrap()
			.unwrap()
	};
	let first_playlist = playlist(&first);
	assert_eq!(first_playlist, playlist(&second));
	assert!(first_playlist.contains("#EXT-X-MEDIA-SEQUENCE:2\n"), "{first_playlist}");
	assert!(
		first_playlist.contains("seg/29b1cd67.2.m4s?jwt=clip\n"),
		"{first_playlist}"
	);
	assert!(
		first_playlist.contains("seg/29b1cd67.4.m4s?jwt=clip\n"),
		"{first_playlist}"
	);
	assert!(!first_playlist.contains("seg/29b1cd67.5.m4s"), "{first_playlist}");
	assert!(first_playlist.ends_with("#EXT-X-ENDLIST\n"), "{first_playlist}");
}

/// An unsupported range is refused before waiting for media, so a server answers 416 rather than
/// timing out.
#[tokio::test(start_paused = true)]
async fn range_playlist_refuses_non_durable_or_windowed_timelines() {
	use futures::FutureExt;

	let range = super::Range::new(Duration::ZERO, Duration::from_secs(2)).unwrap();
	let recording = segments(3, Some(4)).await;
	recording.stored().await;
	let windowed = Replay::open(&recording.store, 64 * 1024 * 1024).await;
	assert!(matches!(
		windowed
			.rendition(Kind::Video, "1080p")
			.playlist(Some(range), None)
			.now_or_never(),
		Some(Err(crate::Error::RangeUnavailable(_)))
	));

	let mut recording = Recording::new(live(), Some(4)).await;
	for segment in 0..3 {
		add_segment(&mut recording, segment, 0);
	}
	recording.stored().await;
	let live = Replay::open_with(&recording.store, 64 * 1024 * 1024, history()).await;
	assert!(matches!(
		live.rendition(Kind::Video, "1080p")
			.playlist(Some(range), None)
			.now_or_never(),
		Some(Err(crate::Error::RangeNotDurable))
	));

	// A rendition without its own timeline would list only gaps, so it is not served at all.
	let mut archive = durable();
	archive.timelines.remove("360p");
	let mut recording = Recording::new(archive, Some(4)).await;
	for segment in 0..3 {
		add_segment(&mut recording, segment, 0);
	}
	recording.stored().await;
	let untimed = Replay::open_with(&recording.store, 64 * 1024 * 1024, history()).await;
	assert!(untimed.broadcaster.rendition(Kind::Video, "360p").is_none());
	assert!(untimed.broadcaster.rendition(Kind::Video, "1080p").is_some());
}

/// Retention prunes a recording's first timeline objects, so history mode refuses it before
/// listing anything rather than starting from a later checkpoint.
#[tokio::test(start_paused = true)]
async fn history_mode_refuses_a_retained_recording() {
	let recording = segments(12, Some(4)).await;
	recording.stored().await;
	for track in ["360p", "1080p", "audio"] {
		for (group, path) in objects(&recording.store, &timeline_name(track)).await {
			if group < 4 {
				recording.store.inner().delete(&path).await.unwrap();
			}
		}
	}
	let replay = Replay::open_with(&recording.store, 64 * 1024 * 1024, history()).await;
	for (kind, name) in [(Kind::Video, "360p"), (Kind::Video, "1080p"), (Kind::Audio, "audio")] {
		let err = replay.failure(kind, name).await;
		assert!(
			err.to_string().contains("retained recordings are not supported"),
			"{name}: {err}"
		);
	}

	let broadcast = replay.origin.consume().request_broadcast("rec", None).await.unwrap();
	let err = super::history::read(&broadcast, &durable(), "1080p", |_| panic!("nothing is listed"))
		.await
		.unwrap_err();
	assert!(matches!(err, crate::Error::HistoryPruned(_)), "{err}");
}

/// How a stored timeline object is damaged.
#[derive(Clone, Copy, Debug)]
enum Fault {
	/// The object is gone after it was listed, so its groups fail to FETCH.
	Missing,
	/// The object's bytes are garbage, so its groups fail to load.
	Corrupt,
}

/// A completed recording whose `track` timeline object holding group 6 is damaged by `fault`
/// once the consumer listed it, exported in history mode.
async fn damaged(track: &str, fault: Fault) -> Replay {
	let recording = segments(12, Some(4)).await;
	let store = recording.finish().await;
	let (_, path) = objects(&store, &timeline_name(track))
		.await
		.into_iter()
		.rfind(|(group, _)| *group <= 6)
		.unwrap();

	let (origin, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::default());
	let driver = tokio::spawn(moq_net::time::run(driver));
	let broadcast = origin.create_broadcast("rec").unwrap();
	let consumer = moq_archive::Consumer::open(store.clone(), broadcast.clone(), Default::default())
		.await
		.unwrap();
	match fault {
		Fault::Missing => store.inner().delete(&path).await.unwrap(),
		Fault::Corrupt => store
			.inner()
			.put(&path, Bytes::from_static(b"garbage").into())
			.await
			.map(|_| ())
			.unwrap(),
	}
	let consumer = tokio::spawn(consumer.run());
	broadcast.announce(Default::default()).unwrap();
	let broadcaster = Broadcaster::new(crate::resolved(&origin, "rec").await, history());
	tokio::time::timeout(Duration::from_secs(5), broadcaster.ready())
		.await
		.expect("a rendition becomes playable or fails");
	Replay {
		broadcaster,
		origin,
		consumer,
		driver,
	}
}

#[tokio::test(start_paused = true)]
async fn a_missing_mid_history_reference_group_fails_every_listing() {
	mid_history_reference_fault(Fault::Missing).await;
}

#[tokio::test(start_paused = true)]
async fn a_corrupt_mid_history_reference_group_fails_every_listing() {
	mid_history_reference_fault(Fault::Corrupt).await;
}

/// A finished recording missing part of its reference timeline would otherwise list a prefix
/// and end with `EXT-X-ENDLIST`, so every listing answers the failure instead, a clip of the
/// readable prefix included.
async fn mid_history_reference_fault(fault: Fault) {
	let replay = damaged("1080p", fault).await;
	for (kind, name) in [(Kind::Video, "1080p"), (Kind::Video, "360p"), (Kind::Audio, "audio")] {
		replay.failure(kind, name).await;
	}
	assert!(matches!(
		replay.broadcaster.manifest(None),
		Err(crate::Error::TimelineFailed { .. })
	));
	let range = super::Range::new(Duration::ZERO, Duration::from_secs(4)).unwrap();
	assert!(matches!(
		replay.rendition(Kind::Video, "1080p").playlist(Some(range), None).await,
		Err(crate::Error::TimelineFailed { .. })
	));
}

/// A rendition missing part of its own history fails alone: its siblings finish, and the DASH
/// manifest and master playlist leave it out.
#[tokio::test(start_paused = true)]
async fn a_corrupt_mid_history_rendition_group_fails_only_its_playlist() {
	let replay = damaged("360p", Fault::Corrupt).await;
	replay.failure(Kind::Video, "360p").await;
	for (kind, name) in [(Kind::Video, "1080p"), (Kind::Audio, "audio")] {
		let playlist = replay
			.playlist_until(kind, name, |playlist| playlist.contains("#EXT-X-ENDLIST"))
			.await;
		assert!(playlist.contains(".11.m4s\n"), "{name}: {playlist}");
	}
	let manifest = replay
		.broadcaster
		.manifest(None)
		.unwrap()
		.expect("the siblings stay listed");
	assert!(manifest.contains("id=\"video/1080p\""), "{manifest}");
	assert!(!manifest.contains("id=\"video/360p\""), "{manifest}");
	let master = replay.broadcaster.master_playlist(None);
	assert!(master.contains("video/1080p/"), "{master}");
	assert!(!master.contains("video/360p/"), "{master}");
}
