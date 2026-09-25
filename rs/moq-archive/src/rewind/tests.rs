use std::time::Duration;

use bytes::Bytes;
use object_store::memory::InMemory;

use super::*;
use crate::writer::{Config, Retention};
use crate::{Key, Reader, Store, Writer, reader};

const TIMELINE: &str = hang::timeline::DEFAULT_NAME;

fn ms(v: u64) -> Timestamp {
	Timestamp::from_millis(v).unwrap()
}

/// A live source recorded by a [`Writer`] and replayed by a [`Reader`], as a DVR deployment runs.
struct Dvr {
	store: Store<InMemory>,
	source: broadcast::Producer,
	video: track::Producer,
	replay: broadcast::Producer,
	reader: Reader<InMemory>,
}

impl Dvr {
	async fn new(config: Config) -> Self {
		let store = Store::new(InMemory::new(), "rec");
		let (source, video) = record(&store, config).await;
		let replay = broadcast::Info::new().produce();
		let reader = Reader::open(store.clone(), &replay, reader::Config::new(TIMELINE))
			.await
			.unwrap();
		tokio::spawn(reader.serve());
		Self {
			store,
			source,
			video,
			replay,
			reader,
		}
	}

	/// Write one second-long group per sequence.
	fn write(&self, sequences: impl IntoIterator<Item = u64>) {
		for sequence in sequences {
			let mut group = self.video.create_group(group::Info { sequence }).unwrap();
			for timestamp in [sequence * 1000, sequence * 1000 + 500] {
				group.write_frame(ms(timestamp), payload(sequence, timestamp)).unwrap();
			}
			group.finish().unwrap();
		}
	}

	/// Wait for the writer to store timeline `segment`, then replay it.
	async fn commit(&mut self, segment: u64) {
		while self.store.get_segments(TIMELINE, segment).await.is_err() {
			tokio::task::yield_now().await;
		}
		self.reader.refresh().await.unwrap();
	}

	fn rewind(&self) -> Rewind {
		Rewind::new(
			self.source.consume(),
			self.replay.consume(),
			catalog::Archive::new(TIMELINE),
		)
	}
}

/// Start recording a fresh source's `video` track into `store`, resuming what it already holds.
async fn record(store: &Store<InMemory>, config: Config) -> (broadcast::Producer, track::Producer) {
	let source = broadcast::Info::new().produce();
	let info = track::Info::default()
		.with_timescale(Timescale::MILLI)
		.with_max_age(Duration::from_secs(3600));
	let video = source.create_track("video", info).unwrap();
	let writer = Writer::new(store.clone(), source.consume(), config).await.unwrap();
	writer.control().pacing_track("video").await.unwrap();
	tokio::spawn(writer.run());
	(source, video)
}

fn payload(sequence: u64, timestamp: u64) -> Bytes {
	Bytes::from(format!("{sequence}@{timestamp}"))
}

fn dvr() -> Config {
	Config::default().with_retention(Retention::new(Duration::from_secs(4), Duration::ZERO))
}

/// Read the next group, checking its frames are the source's, and report where it came from.
async fn next(track: &mut Track) -> (u64, bool) {
	let mut group = track.next_group().await.unwrap().expect("the track is still live");
	let sequence = group.sequence;
	let mut frames = Vec::new();
	while let Some(frame) = group.read_frame().await.unwrap() {
		frames.push((frame.timestamp, frame.payload));
	}
	let expected: Vec<_> = [sequence * 1000, sequence * 1000 + 500]
		.into_iter()
		.map(|timestamp| (ms(timestamp), payload(sequence, timestamp)))
		.collect();
	assert_eq!(frames, expected, "group {sequence}");
	(sequence, track.is_live())
}

#[tokio::test]
async fn a_seek_plays_the_recording_then_splices_to_live() {
	let mut dvr = Dvr::new(dvr()).await;
	dvr.write(0..10);
	// Group 9 is still open in its segment, so the recording ends at group 8.
	dvr.commit(8).await;

	let mut video = dvr.rewind().seek("video", ms(6_200)).await.unwrap();
	assert!(!video.is_live());
	assert_eq!(next(&mut video).await, (6, false));
	assert_eq!(next(&mut video).await, (7, false));
	assert_eq!(next(&mut video).await, (8, false));

	// Live runs ahead of what the recording has replayed, so the splice reaches back into the
	// live cache instead of jumping to its newest group.
	dvr.write(10..12);
	for sequence in 9..12 {
		assert_eq!(next(&mut video).await, (sequence, true));
	}
}

#[tokio::test]
async fn a_seek_before_the_window_starts_at_its_oldest_segment() {
	let mut dvr = Dvr::new(dvr()).await;
	dvr.write(0..10);
	dvr.commit(8).await;

	// Four seconds of one-second segments: 5 through 8 are retained.
	let mut video = dvr.rewind().seek("video", ms(0)).await.unwrap();
	assert_eq!(next(&mut video).await, (5, false));
}

#[tokio::test]
async fn expiry_during_a_seek_skips_to_the_retained_window() {
	let mut dvr = Dvr::new(dvr()).await;
	dvr.write(0..10);
	dvr.commit(8).await;

	let mut video = dvr.rewind().seek("video", ms(0)).await.unwrap();
	assert_eq!(next(&mut video).await, (5, false));

	// The viewer pauses while the window moves past it, popping 5 through 8.
	dvr.write(10..14);
	dvr.commit(12).await;

	// The timeline, not the replay track's cache of group 5, decides what comes next.
	assert_eq!(next(&mut video).await, (9, false));
	assert_eq!(next(&mut video).await, (10, false));
}

#[tokio::test]
async fn missing_groups_are_gaps() {
	let mut dvr = Dvr::new(Config::default()).await;
	// The source never produced group 3.
	dvr.write([0, 1, 2, 4, 5, 6]);
	dvr.commit(4).await;
	// Group 1's object is lost after its record was committed.
	dvr.store.delete(&Key::groups("video", 1..=1).unwrap()).await.unwrap();

	let mut video = dvr.rewind().seek("video", ms(0)).await.unwrap();
	assert_eq!(next(&mut video).await, (0, false));
	assert_eq!(next(&mut video).await, (2, false));
	assert_eq!(next(&mut video).await, (4, false));
	assert_eq!(next(&mut video).await, (5, false));
	assert_eq!(next(&mut video).await, (6, true));
}

#[tokio::test]
async fn a_seek_spans_a_writer_restart() {
	let mut dvr = Dvr::new(Config::default()).await;
	dvr.write(0..4);
	dvr.commit(2).await;
	dvr.video.finish().unwrap();
	dvr.source.finish();
	// The clean end flushes group 3 as the final segment.
	dvr.commit(3).await;

	// The publisher restarts, continuing its sequences, and a new writer resumes the recording.
	let (source, video) = record(&dvr.store, Config::default()).await;
	dvr.source = source;
	dvr.video = video;
	dvr.write(4..9);
	dvr.commit(7).await;

	let mut video = dvr.rewind().seek("video", ms(1_000)).await.unwrap();
	for sequence in 1..8 {
		assert_eq!(next(&mut video).await, (sequence, false));
	}
	assert_eq!(next(&mut video).await, (8, true));
}

#[tokio::test]
async fn a_track_the_recording_never_names_starts_live() {
	let mut dvr = Dvr::new(Config::default()).await;
	dvr.write(0..3);
	dvr.commit(1).await;

	let info = track::Info::default().with_timescale(Timescale::MILLI);
	let chat = dvr.source.create_track("chat", info).unwrap();
	let mut track = dvr.rewind().seek("chat", ms(0)).await.unwrap();
	assert!(track.is_live());

	let mut group = chat.append_group().unwrap();
	group.write_frame(ms(0), "hi").unwrap();
	group.finish().unwrap();
	assert_eq!(track.next_group().await.unwrap().unwrap().sequence, 0);
}
