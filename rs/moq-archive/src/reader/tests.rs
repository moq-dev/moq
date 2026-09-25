use bytes::Bytes;
use hang::timeline::{Range, Record};
use moq_json::window;
use moq_net::{broadcast, group};
use object_store::ObjectStoreExt;

use super::*;
use crate::mock::Mock;
use crate::segment::{Frame, Group};
use crate::{ID_MAX, Info};

const TIMELINE: &str = "timeline.z";

/// Writes archive objects the way a recording writer lays them out.
struct Archive {
	store: Store<Mock>,
	encoder: window::Encoder<Record>,
	/// Next timeline group sequence.
	sequence: u64,
}

impl Archive {
	async fn new() -> Self {
		Self::with_op_ratio(0).await
	}

	/// `op_ratio` 0 makes every window edit its own checkpoint group.
	async fn with_op_ratio(op_ratio: u32) -> Self {
		let store = Store::new(Mock::memory(), "rec");
		store.put_info(TIMELINE, &Info::new(0, 1000).unwrap()).await.unwrap();
		let config = window::ProducerConfig::default()
			.with_compression(true)
			.with_op_ratio(op_ratio);
		Self {
			store,
			encoder: window::Encoder::new(config),
			sequence: 0,
		}
	}

	/// Store one track object holding `groups` of `(sequence, frame count)`.
	async fn media(&self, track: &str, groups: &[(u64, usize)]) {
		self.store.put_info(track, &Info::new(1, 1000).unwrap()).await.unwrap();
		let groups = groups
			.iter()
			.map(|&(sequence, count)| Group {
				sequence,
				frames: (0..count).map(|i| frame(track, sequence, i)).collect(),
			})
			.collect();
		self.store.put_groups(track, &Object { groups }).await.unwrap();
	}

	/// Push `record`, pop `pop` records, and return the resulting timeline groups.
	fn edit(&mut self, record: &Record, pop: u64) -> Vec<Group> {
		let mut frames = Vec::new();
		let pending = self.encoder.push(record).unwrap();
		frames.push((pending.keyframe, pending.payload.clone()));
		pending.commit();
		if let Some(pending) = self.encoder.pop(pop).unwrap() {
			frames.push((pending.keyframe, pending.payload.clone()));
			pending.commit();
		}

		let mut groups: Vec<Group> = Vec::new();
		for (keyframe, payload) in frames {
			if keyframe {
				self.sequence += 1;
			}
			if groups.last().is_none_or(|group| group.sequence != self.sequence - 1) {
				groups.push(Group {
					sequence: self.sequence - 1,
					frames: Vec::new(),
				});
			}
			let timestamp = record.pts;
			groups.last_mut().unwrap().frames.push(Frame { timestamp, payload });
		}
		groups
	}

	async fn timeline(&self, segment: u64, groups: Vec<Group>) {
		self.store
			.put_segments(TIMELINE, segment, &Object { groups })
			.await
			.unwrap();
	}

	/// Commit `record` as timeline segment `record.segment`, popping `pop` older records.
	async fn commit(&mut self, record: &Record, pop: u64) {
		let groups = self.edit(record, pop);
		self.timeline(record.segment, groups).await;
	}

	async fn raw(&self, key: &Key, bytes: &'static [u8]) {
		let path = self.store.path(key).unwrap();
		self.store
			.inner()
			.put(&path, Bytes::from_static(bytes).into())
			.await
			.unwrap();
	}
}

fn frame(track: &str, sequence: u64, index: usize) -> Frame {
	Frame {
		timestamp: sequence * 100 + index as u64,
		payload: Bytes::from(format!("{track}/{sequence}/{index}")),
	}
}

fn record(segment: u64, tracks: &[(&str, &[(u64, u64)])]) -> Record {
	let mut record = Record::new(segment, segment * 2000, 2000);
	for (name, ranges) in tracks {
		let ranges = ranges.iter().map(|&(start, end)| Range::new(start, end)).collect();
		record.tracks.insert(name.to_string(), ranges);
	}
	record
}

async fn open(archive: &Archive) -> (broadcast::Producer, Reader<Mock>) {
	let broadcast = broadcast::Info::new().produce();
	let reader = Reader::open(archive.store.clone(), &broadcast, Config::new(TIMELINE))
		.await
		.unwrap();
	tokio::spawn(reader.serve());
	(broadcast, reader)
}

/// FETCH one group starting at `frame_start`, returning each frame's timestamp and payload.
async fn fetch(
	broadcast: &broadcast::Producer,
	track: &str,
	sequence: u64,
	frame_start: u64,
) -> std::result::Result<Vec<(u64, Bytes)>, moq_net::Error> {
	let track = broadcast.consume().track(track)?;
	let options = group::Fetch::default().with_frame_start(frame_start);
	let mut group = track.fetch_group(sequence, options).await?;
	let mut frames = Vec::new();
	while let Some(frame) = group.read_frame().await? {
		frames.push((frame.timestamp.value(), frame.payload));
	}
	Ok(frames)
}

fn expected(track: &str, sequence: u64, frames: std::ops::Range<usize>) -> Vec<(u64, Bytes)> {
	frames
		.map(|i| {
			let frame = frame(track, sequence, i);
			(frame.timestamp, frame.payload)
		})
		.collect()
}

#[tokio::test]
async fn fetch_replays_original_groups() {
	let mut archive = Archive::new().await;
	archive.media("video", &[(0, 3), (1, 2)]).await;
	archive.media("audio", &[(0, 1), (1, 1), (5, 2)]).await;
	archive.media("video", &[(2, 2)]).await;
	archive
		.commit(&record(0, &[("video", &[(0, 1)]), ("audio", &[(0, 1), (5, 5)])]), 0)
		.await;
	archive.commit(&record(1, &[("video", &[(2, 2)])]), 0).await;

	let (broadcast, _reader) = open(&archive).await;

	assert_eq!(
		fetch(&broadcast, "video", 0, 0).await.unwrap(),
		expected("video", 0, 0..3)
	);
	assert_eq!(
		fetch(&broadcast, "video", 2, 0).await.unwrap(),
		expected("video", 2, 0..2)
	);
	assert_eq!(
		fetch(&broadcast, "audio", 5, 0).await.unwrap(),
		expected("audio", 5, 0..2)
	);

	// A requested frame_start keeps the original frame indices and timestamps.
	let tail = fetch(&broadcast, "video", 1, 1).await.unwrap();
	assert_eq!(tail, expected("video", 1, 1..2));

	let track = broadcast.consume().track("video").unwrap();
	let info = track.query().await.unwrap();
	assert_eq!(info.priority, 1);
	assert_eq!(info.timescale.as_u64(), 1000);
}

#[tokio::test]
async fn requests_download_only_their_object() {
	let mut archive = Archive::new().await;
	archive.media("video", &[(0, 1)]).await;
	archive.media("audio", &[(0, 1), (1, 1), (2, 1)]).await;
	archive
		.commit(&record(0, &[("video", &[(0, 0)]), ("audio", &[(0, 2)])]), 0)
		.await;

	let (broadcast, _reader) = open(&archive).await;
	archive.store.inner().take();

	for sequence in 0..3 {
		assert_eq!(
			fetch(&broadcast, "audio", sequence, 0).await.unwrap(),
			expected("audio", sequence, 0..1)
		);
	}

	let gets = archive.store.inner().gets();
	assert_eq!(
		gets,
		vec![
			"rec/audio/.info".to_string(),
			"rec/audio/groups/0000000000000000002.0000000000000000000".to_string(),
		],
		"adjacent groups reuse one GET and never touch video"
	);
}

#[tokio::test]
async fn unadvertised_or_unreadable_groups_are_not_found() {
	let mut archive = Archive::new().await;
	archive.media("audio", &[(0, 1), (1, 1), (4, 1)]).await;
	// The object holds group 3, which its record does not advertise.
	archive.media("video", &[(0, 1), (3, 1)]).await;
	archive.raw(&Key::groups("bad", 0..=0).unwrap(), b"\x02garbage").await;
	archive
		.store
		.put_info("bad", &Info::new(0, 1000).unwrap())
		.await
		.unwrap();
	archive
		.store
		.put_info("missing", &Info::new(0, 1000).unwrap())
		.await
		.unwrap();
	archive
		.commit(
			&record(
				0,
				&[
					("audio", &[(0, 1), (4, 4)]),
					("video", &[(0, 0), (2, 3)]),
					("bad", &[(0, 0)]),
					("missing", &[(0, 0)]),
				],
			),
			0,
		)
		.await;

	let (broadcast, _reader) = open(&archive).await;

	let not_found = |result: std::result::Result<Vec<(u64, Bytes)>, moq_net::Error>| {
		assert!(matches!(result, Err(moq_net::Error::NotFound)), "{result:?}");
	};
	not_found(fetch(&broadcast, "audio", 2, 0).await); // internal gap
	not_found(fetch(&broadcast, "audio", 5, 0).await); // past the last range
	not_found(fetch(&broadcast, "video", 0, 0).await); // table disagrees with the record
	not_found(fetch(&broadcast, "bad", 0, 0).await); // unknown envelope version
	not_found(fetch(&broadcast, "missing", 0, 0).await); // no object
	not_found(fetch(&broadcast, "chat", 0, 0).await); // track the timeline never named

	// Siblings remain usable.
	assert_eq!(
		fetch(&broadcast, "audio", 4, 0).await.unwrap(),
		expected("audio", 4, 0..1)
	);
}

#[tokio::test]
async fn refresh_follows_new_segments_and_pops() {
	let mut archive = Archive::new().await;
	archive.media("video", &[(0, 1), (1, 1)]).await;
	archive.commit(&record(0, &[("video", &[(0, 1)])]), 0).await;

	let (broadcast, mut reader) = open(&archive).await;
	assert!(fetch(&broadcast, "video", 0, 0).await.is_ok());

	archive.media("video", &[(2, 1)]).await;
	// Refreshing before the commit finds nothing new; Not Found is not finality.
	reader.refresh().await.unwrap();
	assert!(matches!(
		fetch(&broadcast, "video", 2, 0).await,
		Err(moq_net::Error::NotFound)
	));

	archive.commit(&record(1, &[("video", &[(2, 2)])]), 1).await;
	reader.refresh().await.unwrap();
	assert_eq!(
		fetch(&broadcast, "video", 2, 0).await.unwrap(),
		expected("video", 2, 0..1)
	);

	// The popped record's groups are gone, although its object is still stored and was cached.
	archive.store.inner().take();
	assert!(matches!(
		fetch(&broadcast, "video", 1, 0).await,
		Err(moq_net::Error::NotFound)
	));
	assert_eq!(archive.store.inner().gets(), Vec::<String>::new());
}

#[tokio::test]
async fn a_missing_timeline_segment_recovers_from_the_next_checkpoint() {
	let mut archive = Archive::new().await;
	archive.media("video", &[(0, 1)]).await;
	archive.media("video", &[(1, 1)]).await;
	archive.media("video", &[(2, 1)]).await;
	archive.commit(&record(0, &[("video", &[(0, 0)])]), 0).await;
	// Segment 1 is committed to the window but its timeline object is unreadable.
	archive.edit(&record(1, &[("video", &[(1, 1)])]), 0);
	archive.raw(&Key::segments(TIMELINE, 1).unwrap(), b"\x01").await;
	archive.commit(&record(2, &[("video", &[(2, 2)])]), 0).await;

	let (broadcast, _reader) = open(&archive).await;

	// Segment 2's checkpoint restates record 1, so its group is still served.
	for sequence in 0..3 {
		assert_eq!(
			fetch(&broadcast, "video", sequence, 0).await.unwrap(),
			expected("video", sequence, 0..1)
		);
	}
}

#[tokio::test]
async fn multi_frame_timeline_groups_decode() {
	let mut archive = Archive::with_op_ratio(64).await;
	archive.media("video", &[(0, 1)]).await;
	archive.media("video", &[(1, 1)]).await;
	let mut groups = archive.edit(&record(0, &[("video", &[(0, 0)])]), 0);
	let ops = archive.edit(&record(1, &[("video", &[(1, 1)])]), 0);
	assert!(
		ops[0].sequence == groups[0].sequence,
		"the push stays in the open group"
	);
	groups[0].frames.extend(ops.into_iter().flat_map(|group| group.frames));
	archive.timeline(0, groups).await;

	let (broadcast, _reader) = open(&archive).await;
	assert!(fetch(&broadcast, "video", 1, 0).await.is_ok());
}

#[tokio::test]
async fn timeline_track_is_republished_and_finished_on_request() {
	let mut archive = Archive::new().await;
	archive.media("video", &[(0, 1)]).await;
	archive.commit(&record(0, &[("video", &[(0, 0)])]), 0).await;
	archive.commit(&record(1, &[]), 0).await;

	let (broadcast, reader) = open(&archive).await;
	let track = broadcast.consume().track(TIMELINE).unwrap();
	let subscriber = track.subscribe(None).await.unwrap();
	let mut timeline =
		window::Consumer::<Record>::new(subscriber, window::ConsumerConfig::default().with_compression(true));

	reader.finish().unwrap();

	let mut segments = Vec::new();
	while let Some(event) = timeline.next().await.unwrap() {
		if let window::Event::Push { value, .. } = event {
			segments.push(value.segment);
		}
	}
	assert_eq!(segments, vec![0, 1]);
}

#[tokio::test]
async fn open_requires_the_timeline_info() {
	let store = Store::new(Mock::memory(), "rec");
	let broadcast = broadcast::Info::new().produce();
	let result = Reader::open(store, &broadcast, Config::new(TIMELINE)).await;
	assert!(matches!(result, Err(Error::NotFound(_))));
}

#[test]
fn check_runs_requires_the_exact_sequences() {
	let object = Object {
		groups: [0, 1, 4, ID_MAX]
			.into_iter()
			.map(|sequence| Group {
				sequence,
				frames: Vec::new(),
			})
			.collect(),
	};
	check_runs(&object, &[0..=1, 4..=4, ID_MAX..=ID_MAX]).unwrap();
	assert!(check_runs(&object, &[0..=1, 4..=4]).is_err(), "extra group");
	assert!(
		check_runs(&object, &[0..=4, ID_MAX..=ID_MAX]).is_err(),
		"missing groups"
	);
	assert!(check_runs(&object, &[0..=1, 3..=4, ID_MAX..=ID_MAX]).is_err());
}
