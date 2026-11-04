//! Serving a `moq-archive` recording as HLS.
//!
//! A source broadcast, catalog included, is recorded by an [`moq_archive::Producer`] and served
//! back by an [`moq_archive::Consumer`], so the exporter serves the archived catalog exactly like a
//! live broadcast, numbering every rendition's groups on the catalog's join interval. These tests
//! pin the storage traffic that composition produces: a segment GETs only objects of the requested
//! rendition, and a cached one none.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use futures::stream::BoxStream;
use moq_archive::Store;
use moq_archive::object_store::memory::InMemory;
use moq_archive::object_store::path::Path;
use moq_archive::object_store::{
	self, CopyOptions, GetOptions, GetResult, ListResult, MultipartUpload, ObjectMeta, ObjectStore,
	PutMultipartOptions, PutOptions, PutPayload, PutResult,
};
use moq_net::{Timescale, Timestamp, group, track};

use super::*;

/// In-memory store that records every GET path.
#[derive(Debug, Clone, Default)]
struct Counting {
	inner: Arc<InMemory>,
	gets: Arc<Mutex<Vec<String>>>,
}

impl Counting {
	/// Every GET since the last call.
	fn take(&self) -> Vec<String> {
		std::mem::take(&mut *self.gets.lock().unwrap())
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

/// One rendition of the source: its media track.
struct Source {
	media: track::Producer,
}

/// A source broadcast recorded into a store as it publishes.
struct Recording {
	/// The store's own handle, for its GET log.
	counting: Counting,
	store: Store<Counting>,
	broadcast: moq_net::broadcast::Producer,
	catalog: moq_json::snapshot::Producer<hang::Catalog>,
	sources: HashMap<String, Source>,
	recorder: tokio::task::JoinHandle<moq_archive::Result<()>>,
}

impl Recording {
	async fn new() -> Self {
		let counting = Counting::default();
		let store = Store::new(counting.clone(), "rec");
		let broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast
			.create_track(hang::Catalog::DEFAULT_NAME, hang::Catalog::default_track_info())
			.unwrap();
		let mut json = moq_json::snapshot::Config::default();
		json.deltas = false;
		let mut catalog = moq_json::snapshot::Producer::new(track, json);
		catalog
			.update(moq_net::Timed::from(&self::catalog()).at(Timestamp::now()))
			.unwrap();

		let mut sources = HashMap::new();
		for name in ["360p", "1080p", "audio"] {
			// Legacy media is stamped in microseconds. It is published in bursts, so it sets no
			// publisher max age and the recorder stores every superseded group.
			let media = track::Info::default()
				.with_timescale(Timescale::new(1_000_000).unwrap())
				.with_priority(1)
				.with_max_age(None);
			let media = broadcast.create_track(name, media).unwrap();
			sources.insert(name.to_string(), Source { media });
		}

		// The source publishes in bursts, so the budget covers a whole recording.
		let config = moq_archive::producer::Config::default()
			.with_flush(Duration::from_millis(100))
			.with_max_delay(Duration::from_secs(3600));
		let recorder = tokio::spawn(moq_archive::Producer::new(store.clone(), broadcast.consume(), config).run());
		// Subscribed before anything is published, so every group is recorded live.
		for source in sources.values() {
			while source.media.subscription().is_none() {
				kio::clock::sleep(Duration::from_millis(1)).await;
			}
		}
		Self {
			counting,
			store,
			broadcast,
			catalog,
			sources,
			recorder,
		}
	}

	/// Publish `groups` on `track`, each a list of frame timestamps in micros.
	fn add(&mut self, track: &str, groups: &[(u64, &[u64])]) {
		let source = self.sources.get_mut(track).unwrap();
		for &(sequence, frames) in groups {
			let mut group = source.media.create_group(group::Info { sequence }).unwrap();
			for (index, &micros) in frames.iter().enumerate() {
				let timestamp = Timestamp::from_micros(micros).unwrap();
				group.write_frame(timestamp, legacy(track, micros, index == 0)).unwrap();
			}
			group.finish().unwrap();
		}
	}

	/// Wait until the recorder subscribed to every track and stored what was published.
	async fn stored(&self) {
		kio::clock::sleep(Duration::from_secs(5)).await;
	}

	/// End the source cleanly, then wait for the recorder to mark the archive complete.
	async fn finish(mut self) -> Store<Counting> {
		self.catalog.finish().unwrap();
		for source in self.sources.values_mut() {
			source.media.finish().unwrap();
		}
		self.stored().await;
		self.broadcast.close();
		self.recorder.await.unwrap().unwrap();
		self.store
	}

	/// Every GET since the last call.
	fn gets(&self) -> Vec<String> {
		self.counting.take()
	}
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

/// The source's catalog: every rendition's config, on a two-second join interval. Out-of-band
/// configs, so no init needs media.
fn catalog() -> hang::Catalog {
	let mut catalog = hang::Catalog::default();
	catalog.media.join = hang::catalog::Join::new(Duration::from_secs(2));
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
	broadcaster: Broadcaster,
	/// Keeps the served broadcast's origin open.
	_origin: moq_net::origin::Producer,
	consumer: tokio::task::JoinHandle<moq_archive::Result<()>>,
	driver: tokio::task::JoinHandle<moq_net::Error>,
}

impl Replay {
	async fn open(store: &Store<Counting>, cache: u64) -> Self {
		let (origin, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::default());
		let driver = tokio::spawn(moq_net::time::run(driver));
		let broadcast = origin.create_broadcast("rec").unwrap();
		let serving = moq_archive::consumer::Config::default().with_cache(cache);
		let consumer = moq_archive::Consumer::open(store.clone(), broadcast.broadcast().clone(), serving)
			.await
			.unwrap();
		let consumer = tokio::spawn(consumer.run());
		broadcast.announce(Default::default()).unwrap();

		let broadcaster = Broadcaster::spawned(crate::resolved(&origin, "rec").await, Config::default());
		kio::clock::timeout(Duration::from_secs(5), broadcaster.ready())
			.await
			.expect("a rendition becomes playable or fails");

		Self {
			broadcaster,
			_origin: origin,
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
		kio::clock::timeout(Duration::from_secs(5), rendition.playlist(None))
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
			kio::clock::sleep(Duration::from_millis(10)).await;
		}
		panic!("{name} playlist never became ready");
	}
}

impl Drop for Replay {
	fn drop(&mut self) {
		self.consumer.abort();
		self.driver.abort();
	}
}

/// A stored media object: neither the catalog nor a marker.
fn is_media(path: &str) -> bool {
	let name = path.rsplit('/').next().unwrap_or_default();
	let marker = [".info", "end", ".end"].contains(&name);
	!marker && !path.contains("catalog")
}

/// `count` 2s segments: one keyframe group per video segment, four audio groups per audio one.
async fn segments(count: u64) -> Recording {
	let mut recording = Recording::new().await;
	for segment in 0..count {
		add_segment(&mut recording, segment);
	}
	recording
}

fn add_segment(recording: &mut Recording, segment: u64) {
	let pts = segment * 2_000_000;
	for video in ["360p", "1080p"] {
		recording.add(video, &[(segment, &[pts, pts + 1_000_000])]);
	}
	let audio: Vec<(u64, [u64; 1])> = (0..4).map(|i| (segment * 4 + i, [pts + i * 500_000])).collect();
	let audio: Vec<(u64, &[u64])> = audio
		.iter()
		.map(|(sequence, frames)| (*sequence, &frames[..]))
		.collect();
	recording.add("audio", &audio);
}

#[tokio::test(start_paused = true)]
async fn renditions_share_the_grid_and_segments_get_their_own_objects() {
	let recording = segments(3).await;
	recording.stored().await;
	let replay = Replay::open(&recording.store, 64 * 1024 * 1024).await;

	let master = replay.broadcaster.master_playlist(None);
	assert!(master.contains("video/360p/media.m3u8") && master.contains("video/1080p/media.m3u8"));

	// Every rendition numbers its segments on the one grid. The newest waits for a later group.
	for (kind, name) in [(Kind::Video, "1080p"), (Kind::Video, "360p"), (Kind::Audio, "audio")] {
		let tag = replay.broadcaster.rendition(kind, name).unwrap().tag().to_string();
		let playlist = replay
			.playlist_until(kind, name, |playlist| playlist.contains(&format!("seg/{tag}.1.m4s\n")))
			.await;
		for segment in 0..3 {
			let line = format!("seg/{tag}.{segment}.m4s\n");
			assert_eq!(playlist.contains(&line), segment < 2, "{name}: {playlist}");
		}
		assert!(!playlist.contains("#EXT-X-ENDLIST"), "the recording is still growing");
	}
	let gets = recording.gets();
	for rendition in ["360p", "1080p", "audio"] {
		assert!(
			gets.contains(&format!("rec/{rendition}/.info")),
			"track properties bind before media: {gets:?}"
		);
	}

	// Switching renditions downloads only the selected rendition's objects, if any.
	for (kind, name, segment) in [
		(Kind::Video, "360p", 1),
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

/// A pause and an overrun leave grid slots no group starts a segment in, listed as gaps without a
/// discontinuity, since the timestamps never broke.
#[tokio::test(start_paused = true)]
async fn skipped_grid_slots_are_gaps() {
	let mut recording = Recording::new().await;
	// 1080p: two 2s groups, a pause, then content resumes at 10s.
	recording.add("1080p", &[(0, &[0, 1_000_000]), (1, &[2_000_000, 3_000_000])]);
	recording.add("1080p", &[(2, &[10_000_000, 11_000_000])]);
	// 360p overruns its first group to 3.5s, so it starts segment 2 there, and pauses too.
	recording.add("360p", &[(0, &[0, 1_000_000]), (1, &[3_500_000, 4_500_000])]);
	recording.add("360p", &[(2, &[10_000_000, 11_000_000])]);
	recording.add("audio", &[(0, &[0, 1_000_000])]);
	let counting = recording.counting.clone();
	let store = recording.finish().await;

	let replay = Replay::open(&store, 64 * 1024 * 1024).await;
	replay
		.playlist_until(Kind::Video, "360p", |playlist| playlist.contains("#EXT-X-ENDLIST"))
		.await;
	let low = replay.rendition(Kind::Video, "360p").snapshot();
	let flags = |snapshot: &playlist::Snapshot| {
		snapshot
			.segments
			.iter()
			.map(|s| (s.segment, s.gap, s.discontinuity))
			.collect::<Vec<_>>()
	};
	assert_eq!(
		flags(&low),
		[
			(0, false, false),
			(1, true, false),
			(2, false, false),
			(3, true, false),
			(4, true, false),
			(5, false, false),
		]
	);
	replay
		.playlist_until(Kind::Video, "1080p", |playlist| playlist.contains("#EXT-X-ENDLIST"))
		.await;
	let high = replay.rendition(Kind::Video, "1080p").snapshot();
	assert_eq!(
		flags(&high),
		[
			(0, false, false),
			(1, false, false),
			(2, true, false),
			(3, true, false),
			(4, true, false),
			(5, false, false),
		]
	);

	// A gap is never fetched.
	counting.take();
	let rendition = replay.rendition(Kind::Video, "360p");
	assert!(rendition.listed_segment(1).await.unwrap().is_none());
	assert!(!counting.take().iter().any(|path| is_media(path)));
	let late = rendition.listed_segment(2).await.unwrap().unwrap();
	assert_eq!(&late[4..8], b"moof");
	let after = rendition.listed_segment(5).await.unwrap().unwrap();
	assert_eq!(&after[4..8], b"moof");
}

/// A growing recording is followed as the archive grows, and ends once its producer marks it
/// complete.
#[tokio::test(start_paused = true)]
async fn a_growing_recording_ends_with_its_markers() {
	let mut recording = segments(3).await;
	recording.stored().await;
	let replay = Replay::open(&recording.store, 64 * 1024 * 1024).await;
	replay.playlist(Kind::Video, "1080p").await;
	let tag = replay
		.broadcaster
		.rendition(Kind::Video, "1080p")
		.unwrap()
		.tag()
		.to_string();

	add_segment(&mut recording, 3);
	recording.stored().await;
	let playlist = replay
		.playlist_until(Kind::Video, "1080p", |playlist| {
			playlist.contains(&format!("seg/{tag}.2.m4s\n"))
		})
		.await;
	assert!(
		!playlist.contains("#EXT-X-ENDLIST"),
		"a recording without markers stays live"
	);

	recording.finish().await;
	let playlist = replay
		.playlist_until(Kind::Video, "1080p", |playlist| playlist.contains("#EXT-X-ENDLIST"))
		.await;
	assert!(playlist.contains(&format!("seg/{tag}.3.m4s\n")), "{playlist}");
}

/// A durable track still lists only the default 16s window: a live playlist stays bounded however
/// much the store retains.
#[tokio::test(start_paused = true)]
async fn a_durable_recording_lists_the_window() {
	let recording = segments(13).await;
	recording.stored().await;
	let replay = Replay::open(&recording.store, 64 * 1024 * 1024).await;
	let tag = replay
		.broadcaster
		.rendition(Kind::Video, "1080p")
		.unwrap()
		.tag()
		.to_string();
	let playlist = replay
		.playlist_until(Kind::Video, "1080p", |playlist| {
			playlist.contains(&format!("seg/{tag}.11.m4s\n"))
		})
		.await;
	// Eight 2s segments cover the window.
	assert!(playlist.contains("#EXT-X-MEDIA-SEQUENCE:4\n"), "{playlist}");
	assert!(!playlist.contains(&format!("seg/{tag}.3.m4s\n")), "{playlist}");
}
