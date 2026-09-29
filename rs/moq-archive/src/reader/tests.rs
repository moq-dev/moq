use std::collections::HashMap;
use std::time::Duration;

use bytes::Bytes;
use hang::timeline::{Position, Record};
use moq_json::window;
use moq_net::{broadcast, group};
use object_store::ObjectStoreExt;

use super::*;
use crate::Info;
use crate::mock::{Mock, Op};
use crate::segment::{Frame, Group};

fn timeline(track: &str) -> String {
	hang::timeline::default_name(track)
}

/// One track's timeline encoder, as a recording writer drives it.
struct Encoder {
	encoder: window::Encoder<Record>,
	/// Next timeline group sequence.
	group: u64,
	/// Next record sequence.
	record: u64,
}

/// Writes archive objects the way a recording writer lays them out.
struct Archive {
	store: Store<Mock>,
	op_ratio: u32,
	encoders: HashMap<String, Encoder>,
}

impl Archive {
	async fn new() -> Self {
		Self::with_op_ratio(0).await
	}

	/// `op_ratio` 0 makes every window edit its own checkpoint group.
	async fn with_op_ratio(op_ratio: u32) -> Self {
		Self {
			store: Store::new(Mock::memory(), "rec"),
			op_ratio,
			encoders: HashMap::new(),
		}
	}

	async fn encoder(&mut self, track: &str) -> &mut Encoder {
		if !self.encoders.contains_key(track) {
			self.store.put_info(track, &Info::new(1, 1000).unwrap()).await.unwrap();
			self.store
				.put_info(&timeline(track), &Info::new(0, 1000).unwrap())
				.await
				.unwrap();
			let config = window::ProducerConfig::default()
				.with_compression(true)
				.with_op_ratio(self.op_ratio);
			self.encoders.insert(
				track.to_string(),
				Encoder {
					encoder: window::Encoder::new(config),
					group: 0,
					record: 0,
				},
			);
		}
		self.encoders.get_mut(track).unwrap()
	}

	/// The next record of `track`, spanning `start..end` where every group holds `frames` frames.
	/// Stores its object unless `store` is false.
	async fn record(&mut self, track: &str, start: Position, end: Position, frames: u64) -> Record {
		let encoder = self.encoder(track).await;
		let sequence = encoder.record;
		encoder.record += 1;
		let record = Record::new(sequence, start.group * 100, 100, start, end);

		let last = match end.frame {
			0 => end.group - 1,
			_ => end.group,
		};
		let groups = (start.group..=last)
			.map(|group| {
				let first = if group == start.group { start.frame } else { 0 };
				let stop = if group == end.group { end.frame } else { frames };
				Group {
					sequence: group,
					frames: (first..stop).map(|i| frame(track, group, i)).collect(),
				}
			})
			.collect();
		let object = Object {
			frame_start: start.frame,
			groups,
		};
		self.store.put_segments(track, sequence, &object).await.unwrap();
		record
	}

	/// Store and commit a record of whole groups `groups`, each holding `frames` frames.
	async fn add(&mut self, track: &str, groups: std::ops::Range<u64>, frames: u64) -> Record {
		let record = self
			.record(
				track,
				Position::group(groups.start),
				Position::group(groups.end),
				frames,
			)
			.await;
		self.commit(track, &record, 0).await;
		record
	}

	/// Push `record`, pop `pop` records, and return the resulting timeline groups.
	async fn edit(&mut self, track: &str, record: &Record, pop: u64) -> Vec<Group> {
		let encoder = self.encoder(track).await;
		let mut frames = Vec::new();
		let pending = encoder.encoder.push(record).unwrap();
		frames.push((pending.keyframe, pending.payload.clone()));
		pending.commit();
		if let Some(pending) = encoder.encoder.pop(pop).unwrap() {
			frames.push((pending.keyframe, pending.payload.clone()));
			pending.commit();
		}

		let mut groups: Vec<Group> = Vec::new();
		for (keyframe, payload) in frames {
			if keyframe {
				encoder.group += 1;
			}
			if groups.last().is_none_or(|group| group.sequence != encoder.group - 1) {
				groups.push(Group {
					sequence: encoder.group - 1,
					frames: Vec::new(),
				});
			}
			let timestamp = record.pts;
			groups.last_mut().unwrap().frames.push(Frame { timestamp, payload });
		}
		groups
	}

	async fn timeline(&self, track: &str, segment: u64, groups: Vec<Group>) {
		self.store
			.put_segments(&timeline(track), segment, &Object::new(groups))
			.await
			.unwrap();
	}

	/// Commit `record` as its track's timeline segment, popping `pop` older records.
	async fn commit(&mut self, track: &str, record: &Record, pop: u64) {
		let groups = self.edit(track, record, pop).await;
		self.timeline(track, record.sequence, groups).await;
	}

	async fn raw(&self, key: &Key, bytes: &'static [u8]) {
		let path = self.store.path(key).unwrap();
		self.store
			.inner()
			.put(&path, Bytes::from_static(bytes).into())
			.await
			.unwrap();
	}

	fn config(&self) -> Config {
		Config::new(
			self.encoders
				.keys()
				.map(|track| (track.clone(), timeline(track)))
				.collect(),
		)
	}
}

fn frame(track: &str, sequence: u64, index: u64) -> Frame {
	Frame {
		timestamp: sequence * 100 + index,
		payload: Bytes::from(format!("{track}/{sequence}/{index}")),
	}
}

async fn open(archive: &Archive) -> (broadcast::Producer, Reader<Mock>) {
	open_with(archive, archive.config()).await
}

async fn open_with(archive: &Archive, config: Config) -> (broadcast::Producer, Reader<Mock>) {
	let broadcast = broadcast::Info::new().produce();
	let reader = Reader::open(archive.store.clone(), &broadcast, config).await.unwrap();
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

fn expected(track: &str, sequence: u64, frames: std::ops::Range<u64>) -> Vec<(u64, Bytes)> {
	frames
		.map(|i| {
			let frame = frame(track, sequence, i);
			(frame.timestamp, frame.payload)
		})
		.collect()
}

fn not_found(result: std::result::Result<Vec<(u64, Bytes)>, moq_net::Error>) {
	assert!(matches!(result, Err(moq_net::Error::NotFound)), "{result:?}");
}

#[tokio::test]
async fn fetch_replays_original_groups() {
	let mut archive = Archive::new().await;
	archive.add("video", 0..2, 3).await;
	archive.add("video", 2..3, 2).await;
	archive.add("audio", 0..2, 1).await;
	archive.add("audio", 5..6, 2).await;

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
	assert_eq!(tail, expected("video", 1, 1..3));

	let track = broadcast.consume().track("video").unwrap();
	let info = track.query().await.unwrap();
	assert_eq!(info.priority, 1);
	assert_eq!(info.timescale.as_u64(), 1000);
}

#[tokio::test]
async fn a_group_split_across_records_is_served_whole() {
	let mut archive = Archive::new().await;
	let head = archive
		.record("video", Position::group(7), Position::new(7, 3), 5)
		.await;
	archive.commit("video", &head, 0).await;
	let tail = archive
		.record("video", Position::new(7, 3), Position::group(9), 5)
		.await;
	archive.commit("video", &tail, 0).await;

	let (broadcast, _reader) = open(&archive).await;
	assert_eq!(
		fetch(&broadcast, "video", 7, 0).await.unwrap(),
		expected("video", 7, 0..5)
	);
	assert_eq!(
		fetch(&broadcast, "video", 7, 4).await.unwrap(),
		expected("video", 7, 4..5)
	);
	assert_eq!(
		fetch(&broadcast, "video", 8, 0).await.unwrap(),
		expected("video", 8, 0..5)
	);
}

#[tokio::test]
async fn an_open_group_grows_as_records_commit() {
	let mut archive = Archive::new().await;
	let head = archive.record("log", Position::group(0), Position::new(0, 2), 0).await;
	archive.commit("log", &head, 0).await;

	let (broadcast, mut reader) = open(&archive).await;
	let track = broadcast.consume().track("log").unwrap();
	let mut group = tokio::time::timeout(Duration::from_secs(1), track.fetch_group(0, group::Fetch::default()))
		.await
		.expect("fetch")
		.unwrap();
	for index in 0..2 {
		let frame = group.read_frame().await.unwrap().unwrap();
		assert_eq!(frame.payload, frame_payload("log", 0, index));
	}
	let pending = tokio::time::timeout(Duration::from_millis(50), group.read_frame()).await;
	assert!(pending.is_err(), "the group waits for its next record");

	let more = archive.record("log", Position::new(0, 2), Position::new(0, 3), 0).await;
	archive.commit("log", &more, 0).await;
	reader.refresh().await.unwrap();
	let frame = group.read_frame().await.unwrap().unwrap();
	assert_eq!(frame.payload, frame_payload("log", 0, 2));

	// Finishing the reader ends the group where the recording stopped.
	reader.finish().unwrap();
	assert!(group.read_frame().await.unwrap().is_none());
}

fn frame_payload(track: &str, sequence: u64, index: u64) -> Bytes {
	frame(track, sequence, index).payload
}

#[tokio::test]
async fn an_expired_head_is_not_found() {
	let mut archive = Archive::new().await;
	let head = archive
		.record("video", Position::group(7), Position::new(7, 3), 5)
		.await;
	archive.commit("video", &head, 0).await;
	let tail = archive
		.record("video", Position::new(7, 3), Position::group(8), 5)
		.await;
	archive.commit("video", &tail, 1).await;

	let (broadcast, _reader) = open(&archive).await;
	not_found(fetch(&broadcast, "video", 7, 0).await);
	assert_eq!(
		fetch(&broadcast, "video", 7, 3).await.unwrap(),
		expected("video", 7, 3..5)
	);
}

#[tokio::test]
async fn requests_download_only_their_object() {
	let mut archive = Archive::new().await;
	archive.add("video", 0..1, 1).await;
	archive.add("audio", 0..3, 1).await;

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
			"rec/audio/segments/0000000000000000000".to_string(),
		],
		"adjacent groups reuse one GET and never touch video"
	);
}

#[tokio::test]
async fn unadvertised_or_unreadable_groups_are_not_found() {
	let mut archive = Archive::new().await;
	archive.add("audio", 0..2, 1).await;
	archive.add("audio", 4..5, 1).await;
	// The object holds groups 0 and 1, but its record names group 2 too.
	let mismatched = archive.record("video", Position::group(0), Position::group(2), 1).await;
	archive
		.commit(
			"video",
			&Record::new(0, 0, 100, Position::group(0), Position::group(3)),
			0,
		)
		.await;
	let bad = archive.record("bad", Position::group(0), Position::group(1), 1).await;
	archive.raw(&Key::segments("bad", 0).unwrap(), b"\x02garbage").await;
	archive.commit("bad", &bad, 0).await;
	let missing = Record::new(0, 0, 100, Position::group(0), Position::group(1));
	archive.encoder("missing").await;
	archive.commit("missing", &missing, 0).await;
	assert_eq!(mismatched.end, Position::group(2));

	let (broadcast, _reader) = open(&archive).await;

	not_found(fetch(&broadcast, "audio", 2, 0).await); // between records
	not_found(fetch(&broadcast, "audio", 5, 0).await); // past the last record
	not_found(fetch(&broadcast, "video", 0, 0).await); // table disagrees with the record
	not_found(fetch(&broadcast, "bad", 0, 0).await); // malformed envelope
	not_found(fetch(&broadcast, "missing", 0, 0).await); // no object
	not_found(fetch(&broadcast, "chat", 0, 0).await); // a track no timeline indexes

	// Siblings remain usable.
	assert_eq!(
		fetch(&broadcast, "audio", 4, 0).await.unwrap(),
		expected("audio", 4, 0..1)
	);
}

#[tokio::test]
async fn refresh_follows_new_records_and_pops() {
	let mut archive = Archive::new().await;
	archive.add("video", 0..2, 1).await;

	let (broadcast, mut reader) = open(&archive).await;
	assert!(fetch(&broadcast, "video", 0, 0).await.is_ok());

	let next = archive.record("video", Position::group(2), Position::group(3), 1).await;
	// Refreshing before the commit finds nothing new; Not Found is not finality.
	reader.refresh().await.unwrap();
	not_found(fetch(&broadcast, "video", 2, 0).await);

	archive.commit("video", &next, 1).await;
	reader.refresh().await.unwrap();
	assert_eq!(
		fetch(&broadcast, "video", 2, 0).await.unwrap(),
		expected("video", 2, 0..1)
	);

	// The popped record's groups are gone, although its object is still stored and was cached.
	archive.store.inner().take();
	not_found(fetch(&broadcast, "video", 1, 0).await);
	assert_eq!(archive.store.inner().gets(), Vec::<String>::new());
}

#[tokio::test]
async fn a_missing_timeline_segment_recovers_from_the_next_checkpoint() {
	let mut archive = Archive::new().await;
	archive.add("video", 0..1, 1).await;
	// Record 1 is committed to the window but its timeline object is unreadable.
	let lost = archive.record("video", Position::group(1), Position::group(2), 1).await;
	archive.edit("video", &lost, 0).await;
	archive
		.raw(&Key::segments(timeline("video"), 1).unwrap(), b"\x02")
		.await;
	archive.add("video", 2..3, 1).await;

	let (broadcast, _reader) = open(&archive).await;

	// Record 2's checkpoint restates record 1, so its group is still served.
	for sequence in 0..3 {
		assert_eq!(
			fetch(&broadcast, "video", sequence, 0).await.unwrap(),
			expected("video", sequence, 0..1)
		);
	}
}

#[tokio::test]
async fn a_missing_tail_is_retried_on_the_next_refresh() {
	let mut archive = Archive::new().await;
	for group in 0..3 {
		archive.add("video", group..group + 1, 1).await;
	}
	// Listed, but not yet readable.
	archive.store.inner().hide_gets("segments/0000000000000000002");

	let (broadcast, mut reader) = open(&archive).await;
	assert!(fetch(&broadcast, "video", 1, 0).await.is_ok());
	not_found(fetch(&broadcast, "video", 2, 0).await);

	archive.store.inner().heal();
	archive.store.inner().take();
	reader.refresh().await.unwrap();
	assert_eq!(
		archive.store.inner().take(),
		[
			Op::List {
				prefix: "rec/video%2Etimeline%2Ez/segments".to_string(),
				offset: Some("rec/video%2Etimeline%2Ez/segments/0000000000000000001".to_string()),
			},
			Op::Get("rec/video%2Etimeline%2Ez/segments/0000000000000000002".to_string()),
		],
		"the cursor stays before the missing tail"
	);
	assert_eq!(
		fetch(&broadcast, "video", 2, 0).await.unwrap(),
		expected("video", 2, 0..1)
	);
}

#[tokio::test]
async fn following_lists_only_new_timeline_keys() {
	let mut archive = Archive::new().await;
	archive.add("video", 0..1, 1).await;
	let (broadcast, mut reader) = open(&archive).await;

	for group in 1..4 {
		// A media object stored ahead of its commit is invisible until the timeline names it.
		let record = archive
			.record("video", Position::group(group), Position::group(group + 1), 1)
			.await;
		reader.refresh().await.unwrap();
		not_found(fetch(&broadcast, "video", group, 0).await);

		archive.commit("video", &record, 0).await;
		archive.store.inner().take();
		reader.refresh().await.unwrap();
		let previous = format!("rec/video%2Etimeline%2Ez/segments/{:019}", group - 1);
		assert_eq!(
			archive.store.inner().take(),
			[
				Op::List {
					prefix: "rec/video%2Etimeline%2Ez/segments".to_string(),
					offset: Some(previous),
				},
				Op::Get(format!("rec/video%2Etimeline%2Ez/segments/{group:019}")),
			],
			"following record {group} touches no media listing"
		);
		assert_eq!(
			fetch(&broadcast, "video", group, 0).await.unwrap(),
			expected("video", group, 0..1)
		);
	}
}

#[tokio::test]
async fn an_unordered_listing_replays_in_segment_order() {
	let mut archive = Archive::new().await;
	archive.store.inner().unordered();
	for group in 0..4 {
		let record = archive
			.record("video", Position::group(group), Position::group(group + 1), 1)
			.await;
		archive.commit("video", &record, u64::from(group >= 2)).await;
	}

	let (broadcast, mut reader) = open(&archive).await;

	// The cursor is the newest segment, not the last one listed.
	let record = archive.record("video", Position::group(4), Position::group(5), 1).await;
	archive.commit("video", &record, 1).await;
	archive.store.inner().take();
	reader.refresh().await.unwrap();
	assert_eq!(
		archive.store.inner().gets(),
		["rec/video%2Etimeline%2Ez/segments/0000000000000000004"]
	);

	for group in 0..3 {
		not_found(fetch(&broadcast, "video", group, 0).await);
	}
	for group in 3..5 {
		assert_eq!(
			fetch(&broadcast, "video", group, 0).await.unwrap(),
			expected("video", group, 0..1)
		);
	}
}

#[tokio::test]
async fn a_track_without_usable_info_is_not_found() {
	let mut archive = Archive::new().await;
	archive.add("audio", 0..1, 1).await;
	archive.add("bare", 0..1, 1).await;
	archive.add("future", 0..1, 1).await;
	let path = archive.store.path(&Key::info("bare").unwrap()).unwrap();
	archive.store.inner().delete(&path).await.unwrap();
	let path = archive.store.path(&Key::info("future").unwrap()).unwrap();
	archive.store.inner().delete(&path).await.unwrap();
	archive
		.raw(
			&Key::info("future").unwrap(),
			br#"{"version":3,"priority":0,"timescale":1000}"#,
		)
		.await;

	let (broadcast, _reader) = open(&archive).await;
	for track in ["bare", "future"] {
		not_found(fetch(&broadcast, track, 0, 0).await);
	}
	assert_eq!(
		fetch(&broadcast, "audio", 0, 0).await.unwrap(),
		expected("audio", 0, 0..1)
	);
}

#[tokio::test]
async fn multi_frame_timeline_groups_decode() {
	let mut archive = Archive::with_op_ratio(64).await;
	let first = archive.record("video", Position::group(0), Position::group(1), 1).await;
	let second = archive.record("video", Position::group(1), Position::group(2), 1).await;
	let mut groups = archive.edit("video", &first, 0).await;
	let ops = archive.edit("video", &second, 0).await;
	assert!(
		ops[0].sequence == groups[0].sequence,
		"the push stays in the open group"
	);
	groups[0].frames.extend(ops.into_iter().flat_map(|group| group.frames));
	archive.timeline("video", 0, groups).await;

	let (broadcast, _reader) = open(&archive).await;
	assert!(fetch(&broadcast, "video", 1, 0).await.is_ok());
}

#[tokio::test]
async fn timeline_tracks_are_republished_and_finished_on_request() {
	let mut archive = Archive::new().await;
	archive.add("video", 0..1, 1).await;
	archive.add("video", 1..2, 1).await;
	archive.add("audio", 0..1, 1).await;

	let (broadcast, reader) = open(&archive).await;
	let track = broadcast.consume().track(&timeline("video")).unwrap();
	let subscriber = track.subscribe(None).await.unwrap();
	let mut timeline =
		window::Consumer::<Record>::new(subscriber, window::ConsumerConfig::default().with_compression(true));

	reader.finish().unwrap();

	let mut records = Vec::new();
	while let Some(event) = timeline.next().await.unwrap() {
		if let window::Event::Push { value, .. } = event {
			records.push(value.sequence);
		}
	}
	assert_eq!(records, vec![0, 1]);
}

#[tokio::test]
async fn open_requires_every_timeline_info() {
	let store = Store::new(Mock::memory(), "rec");
	let broadcast = broadcast::Info::new().produce();
	let config = Config::new([("video".to_string(), timeline("video"))].into());
	let result = Reader::open(store, &broadcast, config).await;
	assert!(matches!(result, Err(Error::NotFound(_))));
}
