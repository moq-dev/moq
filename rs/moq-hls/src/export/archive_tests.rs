//! Serving a `moq-archive` recording as HLS.
//!
//! A [`moq_archive::Reader`] replays the recording's timeline onto a broadcast and answers FETCH
//! from range-named objects, so the exporter serves it exactly like a live broadcast. These tests
//! pin the storage traffic that composition produces: playlists read only the timeline, and a
//! segment GETs exactly one object of the requested rendition.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use futures::stream::BoxStream;
use hang::timeline::{Range, Record};
use moq_archive::object_store::memory::InMemory;
use moq_archive::object_store::path::Path;
use moq_archive::object_store::{
	self, CopyOptions, GetOptions, GetResult, ListResult, MultipartUpload, ObjectMeta, ObjectStore,
	PutMultipartOptions, PutOptions, PutPayload, PutResult,
};
use moq_archive::{Frame, Group, Info, Object, Store, reader};
use moq_json::window;

use super::*;

const TIMELINE: &str = hang::timeline::DEFAULT_NAME;

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

/// Writes recording objects the way `moq_archive::Writer` lays them out, with full control over
/// each record's timing and ranges.
struct Recording {
	store: Store<Counting>,
	encoder: window::Encoder<Record>,
	/// Next timeline group sequence.
	sequence: u64,
}

impl Recording {
	async fn new(tracks: &[&str]) -> Self {
		let store = Store::new(Counting::default(), "rec");
		store.put_info(TIMELINE, &Info::new(0, 1000).unwrap()).await.unwrap();
		for track in tracks {
			// Legacy media is stamped in microseconds.
			store.put_info(track, &Info::new(1, 1_000_000).unwrap()).await.unwrap();
		}
		let config = window::ProducerConfig::default()
			.with_compression(true)
			.with_op_ratio(0);
		Self {
			store,
			encoder: window::Encoder::new(config),
			sequence: 0,
		}
	}

	/// Store one object for `track` holding `groups`, each a list of frame timestamps in micros.
	async fn media(&self, track: &str, groups: &[(u64, &[u64])]) {
		let groups = groups
			.iter()
			.map(|&(sequence, frames)| Group {
				sequence,
				frames: frames
					.iter()
					.enumerate()
					.map(|(index, &micros)| legacy(track, micros, index == 0))
					.collect(),
			})
			.collect();
		self.store.put_groups(track, &Object { groups }).await.unwrap();
	}

	/// Commit `record` as timeline segment `record.segment`, popping `pop` older records.
	async fn commit(&mut self, record: &Record, pop: u64) {
		let mut payloads = Vec::new();
		let pending = self.encoder.push(record).unwrap();
		payloads.push((pending.keyframe, pending.payload.clone()));
		pending.commit();
		if let Some(pending) = self.encoder.pop(pop).unwrap() {
			payloads.push((pending.keyframe, pending.payload.clone()));
			pending.commit();
		}

		let mut groups: Vec<Group> = Vec::new();
		for (keyframe, payload) in payloads {
			if keyframe || groups.is_empty() {
				groups.push(Group {
					sequence: self.sequence,
					frames: Vec::new(),
				});
				self.sequence += 1;
			}
			let frame = Frame {
				timestamp: record.pts,
				payload,
			};
			groups.last_mut().unwrap().frames.push(frame);
		}
		self.store
			.put_segments(TIMELINE, record.segment, &Object { groups })
			.await
			.unwrap();
	}

	/// Every GET since the last call.
	fn gets(&self) -> Vec<String> {
		self.store.inner().take()
	}
}

/// One Legacy frame: a VP8 keyframe for video (geometry the muxer can parse), filler otherwise.
fn legacy(track: &str, micros: u64, keyframe: bool) -> Frame {
	let payload: &'static [u8] = match (track.starts_with("audio"), keyframe) {
		(true, _) => &[0xFC, 0xFF, 0xFE],
		(false, true) => &[0x10, 0x00, 0x00, 0x9d, 0x01, 0x2a, 0x40, 0x01, 0xf0, 0x00],
		(false, false) => &[0x31, 0x00, 0x00],
	};
	let frame = hang::container::Frame {
		timestamp: moq_net::Timestamp::from_micros(micros).unwrap(),
		payload: Bytes::from_static(payload),
	};
	let mut encoded = BytesMut::new();
	frame.encode(&mut encoded).unwrap();
	Frame {
		timestamp: micros,
		payload: encoded.freeze(),
	}
}

fn record(segment: u64, pts: u64, duration: u64, tracks: &[(&str, u64, u64)]) -> Record {
	let mut record = Record::new(segment, pts, duration);
	for &(track, start, end) in tracks {
		record.tracks.insert(track.to_string(), vec![Range::new(start, end)]);
	}
	record
}

/// The catalog an exporter is handed: every rendition's config, plus the archive entry naming
/// the recording's timeline. Out-of-band configs, so no init needs media.
fn catalog() -> hang::Catalog {
	let mut catalog = hang::Catalog::default();
	catalog.archive = Some(hang::catalog::Archive::new(TIMELINE));
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

/// A recording served back through an origin: the reader's broadcast carries the supplied
/// catalog, and a broadcaster exports it.
struct Replay {
	/// Taken to supply finality.
	reader: Option<moq_archive::Reader<Counting>>,
	broadcaster: Arc<Broadcaster>,
	_catalog: moq_json::snapshot::Producer<hang::Catalog>,
	_broadcast: moq_net::broadcast::Producer,
	_origin: moq_net::origin::Producer,
}

impl Replay {
	async fn open(recording: &Recording, cache: u64) -> Self {
		let (origin, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::default());
		tokio::spawn(moq_net::time::run(driver));
		let broadcast = origin.create_broadcast("rec").unwrap();

		let track = broadcast
			.create_track(hang::Catalog::DEFAULT_NAME, hang::Catalog::default_track_info())
			.unwrap();
		let mut json = moq_json::snapshot::Config::default();
		json.delta_ratio = 0;
		let mut catalog = moq_json::snapshot::Producer::new(track, json);
		catalog.update(&self::catalog()).unwrap();

		let config = reader::Config::new(TIMELINE).with_cache(cache);
		let reader = moq_archive::Reader::open(recording.store.clone(), &broadcast, config)
			.await
			.unwrap();
		tokio::spawn(reader.serve());
		broadcast.announce(Default::default()).unwrap();
		for _ in 0..10 {
			tokio::task::yield_now().await;
		}

		let source = moq_mux::Source::new(origin.consume(), "rec");
		let broadcaster = Broadcaster::new(source, Config::default()).await.unwrap();
		tokio::time::timeout(Duration::from_secs(5), broadcaster.ready())
			.await
			.expect("a rendition becomes playable");

		Self {
			reader: Some(reader),
			broadcaster,
			_catalog: catalog,
			_broadcast: broadcast,
			_origin: origin,
		}
	}

	fn rendition(&self, kind: Kind, name: &str) -> Arc<Rendition> {
		self.broadcaster
			.rendition(kind, name)
			.expect("rendition in the catalog")
	}

	async fn playlist(&self, kind: Kind, name: &str) -> String {
		let rendition = self.rendition(kind, name);
		tokio::time::timeout(Duration::from_secs(5), rendition.playlist(None))
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
}

fn is_media(path: &str) -> bool {
	path.contains("/groups/")
}

/// Three aligned 2s segments: one keyframe group per video rendition, four audio groups each.
async fn three_segments() -> Recording {
	let mut recording = Recording::new(&["360p", "1080p", "audio"]).await;
	for segment in 0..3u64 {
		let pts = segment * 2_000_000;
		for video in ["360p", "1080p"] {
			recording.media(video, &[(segment, &[pts, pts + 1_000_000])]).await;
		}
		let audio: Vec<(u64, [u64; 1])> = (0..4).map(|i| (segment * 4 + i, [pts + i * 500_000])).collect();
		let audio: Vec<(u64, &[u64])> = audio
			.iter()
			.map(|(sequence, frames)| (*sequence, &frames[..]))
			.collect();
		recording.media("audio", &audio).await;

		let tracks = [
			("360p", segment, segment),
			("1080p", segment, segment),
			("audio", segment * 4, segment * 4 + 3),
		];
		recording
			.commit(&record(segment, segment * 2000, 2000, &tracks), 0)
			.await;
	}
	recording
}

#[tokio::test]
async fn playlists_read_only_the_timeline_and_segments_one_object() {
	let recording = three_segments().await;
	let replay = Replay::open(&recording, 64 * 1024 * 1024).await;

	let master = replay.broadcaster.master_playlist(None);
	assert!(master.contains("video/360p/media.m3u8") && master.contains("video/1080p/media.m3u8"));

	// Render and reload every playlist: aligned numbering, and not one media GET.
	for _ in 0..2 {
		for (kind, name) in [(Kind::Video, "360p"), (Kind::Video, "1080p"), (Kind::Audio, "audio")] {
			let playlist = replay.playlist(kind, name).await;
			for segment in 0..3 {
				assert!(playlist.contains(&format!("seg/{segment}.m4s\n")), "{name}: {playlist}");
			}
			assert!(!playlist.contains("#EXT-X-ENDLIST"), "no finality was supplied");
		}
	}
	let gets = recording.gets();
	assert!(gets.iter().any(|path| path.contains("/segments/")), "{gets:?}");
	assert!(
		!gets.iter().any(|path| is_media(path)),
		"playlists must not GET media: {gets:?}"
	);

	// Switching renditions downloads only the selected rendition's object.
	let low = replay.rendition(Kind::Video, "360p").segment(1).await.unwrap().unwrap();
	assert_eq!(&low[4..8], b"moof");
	assert_eq!(
		recording.gets(),
		[
			"rec/360p/.info",
			"rec/360p/groups/0000000000000000001.0000000000000000001"
		]
	);
	let high = replay
		.rendition(Kind::Video, "1080p")
		.segment(2)
		.await
		.unwrap()
		.unwrap();
	assert_eq!(&high[4..8], b"moof");
	assert_eq!(
		recording.gets(),
		[
			"rec/1080p/.info",
			"rec/1080p/groups/0000000000000000002.0000000000000000002"
		]
	);

	// An audio segment spans four groups but still costs one object GET.
	let audio = replay
		.rendition(Kind::Audio, "audio")
		.segment(1)
		.await
		.unwrap()
		.unwrap();
	assert_eq!(&audio[4..8], b"moof");
	assert_eq!(
		recording.gets(),
		[
			"rec/audio/.info",
			"rec/audio/groups/0000000000000000007.0000000000000000004"
		]
	);

	// A repeated request hits the reader's cache.
	replay.rendition(Kind::Video, "360p").segment(1).await.unwrap().unwrap();
	assert_eq!(recording.gets(), Vec::<String>::new());
}

#[tokio::test]
async fn a_bounded_cache_rereads_evicted_objects() {
	let recording = three_segments().await;
	// Too small for any object, so nothing stays cached.
	let replay = Replay::open(&recording, 1).await;
	replay.playlist(Kind::Audio, "audio").await;
	recording.gets();

	// Each of the segment's four groups misses the cache and GETs the same object again.
	let audio = replay
		.rendition(Kind::Audio, "audio")
		.segment(1)
		.await
		.unwrap()
		.unwrap();
	assert_eq!(&audio[4..8], b"moof");
	let gets: Vec<String> = recording.gets().into_iter().filter(|path| is_media(path)).collect();
	assert_eq!(
		gets,
		vec!["rec/audio/groups/0000000000000000007.0000000000000000004"; 4]
	);
}

#[tokio::test]
async fn missing_track_segments_are_gaps_and_time_jumps_are_discontinuities() {
	let mut recording = Recording::new(&["360p", "1080p", "audio"]).await;
	recording.media("360p", &[(0, &[0])]).await;
	recording.media("1080p", &[(0, &[0])]).await;
	recording
		.commit(&record(0, 0, 2000, &[("360p", 0, 0), ("1080p", 0, 0)]), 0)
		.await;
	// 1080p stored nothing for segment 1.
	recording.media("360p", &[(1, &[2_000_000])]).await;
	recording.commit(&record(1, 2000, 2000, &[("360p", 1, 1)]), 0).await;
	// Content time jumps from 4s to 10s.
	recording.media("360p", &[(2, &[10_000_000])]).await;
	recording.media("1080p", &[(1, &[10_000_000])]).await;
	recording
		.commit(&record(2, 10_000, 2000, &[("360p", 2, 2), ("1080p", 1, 1)]), 0)
		.await;

	let replay = Replay::open(&recording, 64 * 1024 * 1024).await;
	let high = replay.playlist(Kind::Video, "1080p").await;
	let expected = concat!(
		"#EXTINF:2.00000,\nseg/0.m4s\n",
		"#EXT-X-GAP\n#EXTINF:2.00000,\nseg/1.m4s\n",
		"#EXT-X-DISCONTINUITY\n#EXTINF:2.00000,\nseg/2.m4s\n",
	);
	assert!(high.contains(expected), "{high}");
	let low = replay.playlist(Kind::Video, "360p").await;
	assert!(!low.contains("#EXT-X-GAP"), "{low}");
	assert!(
		low.contains("#EXT-X-DISCONTINUITY\n#EXTINF:2.00000,\nseg/2.m4s\n"),
		"{low}"
	);

	// A gap is never fetched.
	recording.gets();
	let rendition = replay.rendition(Kind::Video, "1080p");
	assert!(rendition.segment(1).await.unwrap().is_none());
	assert!(!recording.gets().iter().any(|path| is_media(path)));
	let after = rendition.segment(2).await.unwrap().unwrap();
	assert_eq!(&after[4..8], b"moof");
}

#[tokio::test]
async fn a_growing_recording_ends_only_on_caller_finality() {
	let mut recording = three_segments().await;
	let mut replay = Replay::open(&recording, 64 * 1024 * 1024).await;
	replay.playlist(Kind::Video, "360p").await;

	// A DVR commit: segment 3 arrives and segment 0 expires.
	recording.media("360p", &[(3, &[6_000_000])]).await;
	recording.commit(&record(3, 6000, 2000, &[("360p", 3, 3)]), 1).await;
	recording.gets();
	let mut reader = replay.reader.take().unwrap();
	reader.refresh().await.unwrap();
	let playlist = replay
		.playlist_until(Kind::Video, "360p", |playlist| playlist.contains("seg/3.m4s\n"))
		.await;
	assert!(playlist.contains("#EXT-X-MEDIA-SEQUENCE:1\n"), "{playlist}");
	assert!(!playlist.contains("seg/0.m4s"), "{playlist}");
	assert!(
		!playlist.contains("#EXT-X-ENDLIST"),
		"a recording without finality stays live"
	);
	let gets = recording.gets();
	assert!(
		!gets.iter().any(|path| is_media(path)),
		"following reads only the timeline: {gets:?}"
	);

	// The store holds no completion marker; the caller supplies finality.
	reader.finish().unwrap();
	let playlist = replay
		.playlist_until(Kind::Video, "360p", |playlist| playlist.contains("#EXT-X-ENDLIST"))
		.await;
	assert!(playlist.contains("seg/3.m4s\n#EXT-X-ENDLIST\n"), "{playlist}");
}
