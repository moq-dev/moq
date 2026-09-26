//! End to end: one multi-rendition broadcast recorded by a [`Writer`], stored byte-identically on
//! every backend, and replayed exactly through a [`Reader`].

use std::collections::BTreeMap;
use std::time::Duration;

use bytes::Bytes;
use futures::TryStreamExt;
use moq_net::{Timescale, Timestamp, broadcast, group, track};
use object_store::local::LocalFileSystem;
use object_store::path::Path;
use object_store::{ObjectStore, ObjectStoreExt};

use crate::mock::{Mock, Op};
use crate::reader::Config as ReaderConfig;
use crate::writer::{Config, Retention};
use crate::{Reader, Store, Writer};

const RENDITIONS: [&str; 2] = ["video/1080p", "video/360p"];
const MEDIA: [&str; 3] = ["video/1080p", "video/360p", "audio"];
const TRACKS: [&str; 5] = ["video/1080p", "video/360p", "audio", "catalog.json", "chat"];

/// Frames of one source group: (millisecond timestamp, payload).
type Frames = Vec<(u64, Bytes)>;

/// Source segments: three 2s segments, then a one-frame tail.
const SEGMENTS: u64 = 4;

/// The source groups of one segment: (track, sequence, frames).
///
/// Video renditions share a 2s GOP and audio has four 500ms groups per segment, which its own
/// timeline packs two to a record. The catalog and a non-media chat track publish sparsely,
/// including a sequence skip and an empty payload.
fn plan(segment: u64) -> Vec<(&'static str, u64, Frames)> {
	let pts = segment * 2000;
	let (frames, audio) = if segment + 1 < SEGMENTS { (4, 4) } else { (1, 1) };
	let mut groups = Vec::new();
	for track in RENDITIONS {
		let frames = (0..frames)
			.map(|i| (pts + i * 500, Bytes::from(format!("{track} {segment}.{i}"))))
			.collect();
		groups.push((track, segment, frames));
	}
	for sequence in segment * 4..segment * 4 + audio {
		let frames = (0..2)
			.map(|i| (sequence * 500 + i * 250, Bytes::from(format!("audio {sequence}.{i}"))))
			.collect();
		groups.push(("audio", sequence, frames));
	}
	let catalog = |sequence: u64| Bytes::from(format!(r#"{{"version":{sequence}}}"#));
	let chat = |text: &'static str| Bytes::from_static(text.as_bytes());
	match segment {
		0 => {
			groups.push(("catalog.json", 0, vec![(0, catalog(0))]));
			groups.push(("chat", 0, vec![(1200, chat("hello")), (1300, Bytes::new())]));
		}
		2 => {
			groups.push(("catalog.json", 1, vec![(4000, catalog(1))]));
			groups.push(("chat", 3, vec![(4100, chat("skip"))]));
			groups.push(("chat", 5, vec![(5100, chat("bye"))]));
		}
		_ => {}
	}
	groups
}

fn ms(value: u64) -> Timestamp {
	Timestamp::new(value, Timescale::MILLI).unwrap()
}

fn write(track: &track::Producer, sequence: u64, frames: &Frames) {
	let mut group = track.create_group(group::Info { sequence }).unwrap();
	for (timestamp, payload) in frames {
		group.write_frame(ms(*timestamp), payload.clone()).unwrap();
	}
	group.finish().unwrap();
}

/// Let the writer read and report everything already published. Reads never touch the store.
async fn settle() {
	for _ in 0..256 {
		tokio::task::yield_now().await;
	}
}

fn timeline(track: &str) -> String {
	hang::timeline::default_name(track)
}

/// Every recorded track's timeline, as the catalog's `archive` entry names them.
fn config() -> ReaderConfig {
	ReaderConfig::new(
		TRACKS
			.iter()
			.map(|track| (track.to_string(), timeline(track)))
			.collect(),
	)
}

/// Record every segment of [`plan`] into `store`.
async fn record<S: ObjectStore + Clone>(store: &Store<S>) {
	let source = broadcast::Info::new().produce();
	let tracks: BTreeMap<&str, track::Producer> = TRACKS
		.iter()
		.map(|&name| {
			let info = track::Info::default()
				.with_timescale(Timescale::MILLI)
				.with_max_age(Duration::from_secs(3600));
			(name, source.create_track(name, info).unwrap())
		})
		.collect();

	let writer = Writer::new(store.clone(), source.consume(), Config::default())
		.await
		.unwrap();
	let control = writer.control();
	for name in TRACKS {
		match MEDIA.contains(&name) {
			true => control.track(name).await.unwrap(),
			false => control.sparse(name).await.unwrap(),
		}
	}
	let run = tokio::spawn(writer.run());

	// Each track cuts its own timeline, so the interleave across tracks changes nothing stored.
	for segment in 0..SEGMENTS {
		for (name, sequence, frames) in plan(segment) {
			write(&tracks[name], sequence, &frames);
		}
		settle().await;
	}
	for track in tracks.values() {
		track.finish().unwrap();
	}
	source.close();
	run.await.unwrap().unwrap();
}

/// Every object under the store's prefix, by path relative to the store root.
async fn objects<S: ObjectStore>(store: &Store<S>) -> BTreeMap<String, Bytes> {
	let metas: Vec<_> = store.inner().list(Some(store.prefix())).try_collect().await.unwrap();
	let mut objects = BTreeMap::new();
	for meta in metas {
		let bytes = store.inner().get(&meta.location).await.unwrap().bytes().await.unwrap();
		objects.insert(meta.location.to_string(), bytes);
	}
	objects
}

fn id(value: u64) -> String {
	format!("{value:019}")
}

/// Each track's record count: a GOP per video record, two audio groups per record, and a group per
/// sparse record.
const RECORDS: [(&str, u64); 5] = [
	("video%2F1080p", 4),
	("video%2F360p", 4),
	("audio", 7),
	("catalog%2Ejson", 2),
	("chat", 3),
];

/// The exact layout [`plan`] produces: no manifest, index, or completion marker.
fn layout() -> Vec<String> {
	let mut keys = Vec::new();
	for (track, records) in RECORDS {
		for directory in [track.to_string(), format!("{track}%2Etimeline%2Ez")] {
			keys.push(format!("rec/{directory}/.info"));
			keys.extend((0..records).map(|record| format!("rec/{directory}/segments/{}", id(record))));
		}
	}
	keys.sort();
	keys
}

/// FETCH one whole group, returning its frames.
async fn fetch(broadcast: &broadcast::Producer, track: &str, sequence: u64) -> moq_net::Result<Frames> {
	let track = broadcast.consume().track(track)?;
	let mut group = track.fetch_group(sequence, group::Fetch::default()).await?;
	let mut frames = Vec::new();
	while let Some(frame) = group.read_frame().await? {
		frames.push((
			frame.timestamp.convert(Timescale::MILLI).unwrap().value(),
			frame.payload,
		));
	}
	Ok(frames)
}

#[tokio::test]
async fn recordings_are_byte_identical_on_every_backend() {
	let memory = Store::new(Mock::memory(), "rec");
	record(&memory).await;
	let expected = objects(&memory).await;
	assert_eq!(expected.keys().cloned().collect::<Vec<_>>(), layout());
	assert_eq!(
		&expected["rec/video%2F360p/.info"][..],
		br#"{"version":2,"priority":0,"timescale":1000}"#
	);

	let dir = tempfile::tempdir().unwrap();
	let local = Store::new(Mock::new(LocalFileSystem::new_with_prefix(dir.path()).unwrap()), "rec");
	record(&local).await;
	assert_eq!(objects(&local).await, expected, "local disk");

	// A backend with no listing order, and a sibling recording sharing the prefix's stem.
	let unordered = Mock::memory();
	unordered.unordered();
	let sibling = Store::new(unordered.clone(), "rec-other");
	record(&sibling).await;
	let unordered = Store::new(unordered, "rec");
	record(&unordered).await;
	assert_eq!(objects(&unordered).await, expected, "unordered listing");
	assert_eq!(objects(&sibling).await.len(), expected.len());

	// The media objects hold exactly the source groups, in order, byte for byte.
	let mut stored: BTreeMap<(String, u64), Frames> = BTreeMap::new();
	for (path, bytes) in &expected {
		let key = crate::Key::parse(&Path::from("rec"), &Path::parse(path).unwrap()).unwrap();
		if let crate::Key::Segments { track, .. } = key
			&& !track.ends_with(hang::timeline::SUFFIX)
		{
			let object = crate::Object::decode(bytes.clone()).unwrap();
			assert_eq!(object.frame_start, 0, "no source group outlived the maximum");
			for group in object.groups {
				let frames = group.frames.into_iter().map(|f| (f.timestamp, f.payload)).collect();
				stored.insert((track.clone(), group.sequence), frames);
			}
		}
	}
	let source: BTreeMap<(String, u64), Frames> = (0..SEGMENTS)
		.flat_map(plan)
		.map(|(track, sequence, frames)| ((track.to_string(), sequence), frames))
		.collect();
	assert_eq!(stored, source);
}

#[tokio::test]
async fn fetch_replays_the_recording_and_reads_only_the_requested_rendition() {
	let mock = Mock::memory();
	let store = Store::new(mock.clone(), "rec");
	record(&store).await;

	let broadcast = broadcast::Info::new().produce();
	let reader = Reader::open(store, &broadcast, config()).await.unwrap();
	tokio::spawn(reader.serve());
	mock.take();

	// Low-rendition playback never downloads the 1080p object.
	for sequence in 0..SEGMENTS {
		let expected = plan(sequence)
			.into_iter()
			.find(|(name, ..)| *name == "video/360p")
			.unwrap()
			.2;
		assert_eq!(fetch(&broadcast, "video/360p", sequence).await.unwrap(), expected);
	}
	let object = |track: &str, record: u64| format!("rec/{track}/segments/{}", id(record));
	// Each track request also GETs that track's `.info`, but nothing of another track.
	let media = |gets: Vec<String>, track: &str| {
		let prefix = format!("rec/{track}/");
		assert!(gets.iter().all(|path| path.starts_with(&prefix)), "{gets:?}");
		gets.into_iter()
			.filter(|path| path.contains("/segments/"))
			.collect::<Vec<_>>()
	};
	assert_eq!(
		media(mock.gets(), "video%2F360p"),
		(0..4).map(|record| object("video%2F360p", record)).collect::<Vec<_>>()
	);

	// Audio-only playback: two adjacent groups per GET, the rest from the cache.
	for segment in 0..SEGMENTS {
		for (_, sequence, frames) in plan(segment).into_iter().filter(|(name, ..)| *name == "audio") {
			assert_eq!(fetch(&broadcast, "audio", sequence).await.unwrap(), frames);
		}
	}
	assert_eq!(
		media(mock.gets(), "audio"),
		(0..7).map(|record| object("audio", record)).collect::<Vec<_>>()
	);

	// Every other enrolled group replays its original sequence, timestamps, and payloads.
	for segment in 0..SEGMENTS {
		for (name, sequence, frames) in plan(segment) {
			assert_eq!(
				fetch(&broadcast, name, sequence).await.unwrap(),
				frames,
				"{name} {sequence}"
			);
		}
	}
	for (track, sequence) in [("chat", 1), ("chat", 4), ("chat", 6), ("audio", 13), ("video/1080p", 4)] {
		assert!(
			matches!(fetch(&broadcast, track, sequence).await, Err(moq_net::Error::NotFound)),
			"{track} {sequence} was never recorded"
		);
	}
	assert!(
		!mock.take().iter().any(|op| matches!(op, Op::List { .. })),
		"a group request resolves its object without listing"
	);
}

#[tokio::test]
async fn an_offline_reader_follows_dvr_expiry() {
	let mock = Mock::memory();
	let store = Store::new(mock.clone(), "rec");
	let source = broadcast::Info::new().produce();
	let info = track::Info::default()
		.with_timescale(Timescale::MILLI)
		.with_max_age(Duration::from_secs(3600));
	let video = source.create_track("video", info).unwrap();
	let config = Config::default().with_retention(Retention::new(Duration::from_secs(2), Duration::ZERO));
	let writer = Writer::new(store.clone(), source.consume(), config).await.unwrap();
	writer.control().track("video").await.unwrap();
	let run = tokio::spawn(writer.run());

	let frames = |sequence: u64| vec![(sequence * 1000, Bytes::from(format!("video {sequence}")))];
	let committed = |segment: u64| {
		let store = store.clone();
		async move {
			while store.get_segments(&timeline("video"), segment).await.is_err() {
				tokio::time::sleep(Duration::from_millis(5)).await;
			}
		}
	};

	for sequence in 0..3 {
		write(&video, sequence, &frames(sequence));
	}
	committed(1).await;

	let broadcast = broadcast::Info::new().produce();
	let config = ReaderConfig::new([("video".to_string(), timeline("video"))].into());
	let mut reader = Reader::open(store.clone(), &broadcast, config).await.unwrap();
	tokio::spawn(reader.serve());
	assert_eq!(fetch(&broadcast, "video", 0).await.unwrap(), frames(0));

	// The reader is offline while the DVR commits and expires several records.
	for sequence in 3..10 {
		write(&video, sequence, &frames(sequence));
	}
	committed(8).await;
	// Records 7 and 8 hold the 2s window.
	while store.get_segments("video", 6).await.is_ok() {
		tokio::time::sleep(Duration::from_millis(5)).await;
	}

	mock.take();
	reader.refresh().await.unwrap();
	let ops = mock.take();
	let (list, gets) = ops.split_first().unwrap();
	assert_eq!(
		list,
		&Op::List {
			prefix: "rec/video%2Etimeline%2Ez/segments".to_string(),
			offset: Some(format!("rec/video%2Etimeline%2Ez/segments/{}", id(1))),
		}
	);
	let segments: Vec<_> = (2..=8)
		.map(|segment| Op::Get(format!("rec/video%2Etimeline%2Ez/segments/{}", id(segment))))
		.collect();
	assert_eq!(gets, segments, "only the new timeline keys are read");

	// Expired groups are gone from the index, so they cost no media GET.
	for sequence in 0..7 {
		assert!(
			matches!(
				fetch(&broadcast, "video", sequence).await,
				Err(moq_net::Error::NotFound)
			),
			"group {sequence} expired"
		);
	}
	let gets = mock.gets();
	assert!(
		!gets.iter().any(|path| path.starts_with("rec/video/segments/")),
		"{gets:?}"
	);
	for sequence in 7..9 {
		assert_eq!(fetch(&broadcast, "video", sequence).await.unwrap(), frames(sequence));
	}

	video.finish().unwrap();
	source.close();
	run.await.unwrap().unwrap();
}
