use std::time::Duration;

use bytes::Bytes;
use kio::clock::{sleep, timeout};
use moq_net::track::{Delay, Order, Position, Subscription};
use moq_net::{Timescale, Timestamp, broadcast, group, track};
use object_store::ObjectStoreExt;
use tokio::task::JoinHandle;

use super::*;
use crate::Info;
use crate::layout::{At, Entry, Frame};
use crate::mock::Mock;

const REFRESH: Duration = Duration::from_secs(1);

/// Writes a prefix the way a [`crate::Producer`] lays it out, object by object.
struct Archive {
	store: Store<Mock>,
}

impl Archive {
	fn new() -> Self {
		Self {
			store: Store::new(Mock::memory(), "rec"),
		}
	}

	/// A timed track, timescale 1000, or an untimed one.
	async fn track(&self, track: &str, priority: i8, timed: bool) {
		let info = Info::new(priority, timed.then_some(1000)).unwrap();
		self.store.put_info(track, &info).await.unwrap();
	}

	/// Store one object whose first entry starts at `frame_start`.
	async fn object(&self, track: &str, frame_start: u64, entries: Vec<Entry>) {
		let timed = self.store.get_info(track).await.unwrap().timescale.is_some();
		let object = Object { frame_start, entries };
		let name = object.start().unwrap().name().unwrap();
		let path = crate::path::push(&self.dir(track), &name).unwrap();
		let bytes = object.encode(timed).unwrap();
		self.store.inner().put(&path, bytes.into()).await.unwrap();
	}

	async fn end(&self, track: &str, end: End) {
		let path = crate::path::push(&self.dir(track), layout::TRACK_END).unwrap();
		let bytes = end.encode().unwrap();
		self.store.inner().put(&path, bytes.into()).await.unwrap();
	}

	/// Write the broadcast's `.end`, after every track's `end`.
	async fn complete(&self) {
		self.raw(layout::BROADCAST_END, b"").await;
	}

	async fn raw(&self, key: &str, bytes: &'static [u8]) {
		let path = object_store::path::Path::parse(format!("rec/{key}")).unwrap();
		self.store
			.inner()
			.put(&path, Bytes::from_static(bytes).into())
			.await
			.unwrap();
	}

	/// Delete the object starting group `sequence`, as retention would.
	async fn prune(&self, track: &str, sequence: u64) {
		let name = layout::Start {
			group: sequence,
			frame: 0,
		}
		.name()
		.unwrap();
		let path = crate::path::push(&self.dir(track), &name).unwrap();
		self.store.inner().delete(&path).await.unwrap();
	}

	fn dir(&self, track: &str) -> object_store::path::Path {
		layout::track_dir(self.store.prefix(), track).unwrap()
	}
}

/// Frame `index` of `sequence`, its timestamp and payload naming both.
fn frame(timed: bool, sequence: u64, index: u64) -> Frame {
	Frame {
		timestamp: timed.then_some(sequence * 1000 + index),
		payload: Bytes::from(format!("{sequence}/{index}")),
	}
}

/// Group `sequence`'s frames `frames`.
fn frames(timed: bool, sequence: u64, frames: std::ops::Range<u64>) -> Entry {
	Entry {
		sequence,
		frames: frames.map(|index| frame(timed, sequence, index)).collect(),
	}
}

/// What a reader sees of `frames(timed, sequence, indices)`.
fn expected(timed: bool, sequence: u64, indices: std::ops::Range<u64>) -> Vec<String> {
	indices.map(|index| describe(&frame(timed, sequence, index))).collect()
}

fn describe(frame: &Frame) -> String {
	let payload = String::from_utf8(frame.payload.to_vec()).unwrap();
	match frame.timestamp {
		Some(pts) => format!("{pts}@{payload}"),
		None => payload,
	}
}

fn read(timestamp: Timestamp, payload: Bytes) -> String {
	describe(&Frame {
		timestamp: timestamp.scale().map(|_| timestamp.value()),
		payload,
	})
}

async fn serve(store: &Store<Mock>) -> (broadcast::Producer, JoinHandle<Result<()>>) {
	let broadcast = broadcast::Info::new().produce();
	let config = Config::default().with_refresh(REFRESH);
	let consumer = Consumer::open(store.clone(), broadcast.clone(), config).await.unwrap();
	(broadcast, tokio::spawn(consumer.run()))
}

/// FETCH group `sequence`.
async fn fetch(broadcast: &broadcast::Producer, track: &str, sequence: u64) -> moq_net::Result<Vec<String>> {
	let track = broadcast.consume().track(track)?;
	let mut group = timeout(Duration::from_secs(10), track.fetch_group(sequence, None))
		.await
		.expect("the fetch is answered")?;
	let mut frames = Vec::new();
	while let Some(frame) = timeout(Duration::from_secs(10), group.read_frame())
		.await
		.expect("the group ends")?
	{
		frames.push(read(frame.timestamp, frame.payload));
	}
	Ok(frames)
}

/// Read every group from 0 on, in order, with no age limit.
async fn subscribe(broadcast: &broadcast::Producer, track: &str) -> track::Ordered {
	let track = broadcast.consume().track(track).unwrap();
	let subscription = Subscription::default()
		.with_ranges([from(0)])
		.with_order(Order::Ascending)
		.with_delay(Delay::default().with_max(None));
	track.subscribe(subscription).await.unwrap().ordered()
}

/// Every group from the one starting at `start` on.
fn from(start: u64) -> track::Range {
	track::Range {
		start: track::Anchor::Time(start),
		end: None,
	}
}

/// The next group, past the gaps the read reports for what the archive never held.
async fn group(reader: &mut track::Ordered) -> Option<moq_net::group::Consumer> {
	timeout(Duration::from_secs(10), reader.next_group())
		.await
		.expect("a group arrives")
		.unwrap()
}

/// The next group's start, without waiting for it to end.
async fn sequence(reader: &mut track::Ordered) -> Option<u64> {
	group(reader).await.map(|group| group.start)
}

/// The next group and every frame in it, once the group ends.
async fn next(reader: &mut track::Ordered) -> Option<(u64, Vec<String>)> {
	let mut group = group(reader).await?;
	let mut frames = Vec::new();
	while let Some(frame) = timeout(Duration::from_secs(10), group.read_frame())
		.await
		.expect("the group ends")
		.unwrap()
	{
		frames.push(read(frame.timestamp, frame.payload));
	}
	Some((group.start, frames))
}

fn not_found(result: moq_net::Result<Vec<String>>) {
	assert!(matches!(result, Err(moq_net::Error::NotFound)), "{result:?}");
}

/// Recorded through the real producer, served back with every group, frame, form, and
/// property it had, byte for byte.
#[tokio::test(start_paused = true)]
async fn a_recording_round_trips() {
	let store = Store::new(Mock::memory(), "rec");
	let source = broadcast::Info::new().produce();
	// The source keeps a second: the archive outlives it.
	let timed = track::Info::default()
		.with_timescale(Timescale::MILLI)
		.with_priority(3)
		.with_max_age(Duration::from_secs(1));
	let video = source.create_track("video", timed).unwrap();
	let log = source
		.create_track("log", track::Info::default().with_priority(9))
		.unwrap();
	let config = crate::producer::Config::default()
		.with_duration(Duration::from_secs(2))
		.with_flush(Duration::from_secs(100));
	let recording = tokio::spawn(crate::Producer::new(store.clone(), source.consume(), config).run());
	while video.subscription().is_none() || log.subscription().is_none() {
		sleep(Duration::from_millis(1)).await;
	}
	for sequence in 0..6 {
		let mut group = video.create_group(group::Info { start: sequence }).unwrap().unwrap();
		for index in 0..3 {
			let pts = Timestamp::from_millis(sequence * 10_000 + index).unwrap();
			group
				.write_frame(pts, Bytes::from(vec![sequence as u8, index as u8, 0xff]))
				.unwrap();
		}
		group.finish().unwrap();
		sleep(Duration::from_millis(10)).await;
	}
	let mut group = log.create_group(group::Info { start: 0 }).unwrap().unwrap();
	group.write_frame(moq_net::Timestamp::unscaled(0), "a").unwrap();
	group.finish().unwrap();
	sleep(Duration::from_millis(10)).await;
	let _video = video.finish().unwrap();
	let _log = log.finish().unwrap();
	sleep(Duration::from_millis(10)).await;
	source.close();
	recording.await.unwrap().unwrap();

	let (broadcast, _run) = serve(&store).await;
	let served = broadcast.consume().track("video").unwrap();
	let info = served.info().await.unwrap();
	assert_eq!(info.priority, 3);
	assert_eq!(info.timescale, Some(Timescale::MILLI));
	assert_eq!(info.max_age, None);

	let mut reader = subscribe(&broadcast, "video").await;
	for sequence in 0..6 {
		let mut group = reader.next_group().await.unwrap().unwrap();
		assert_eq!(group.start, sequence);
		for index in 0..3 {
			let frame = group.read_frame().await.unwrap().unwrap();
			assert_eq!(frame.timestamp.value(), sequence * 10_000 + index);
			assert_eq!(&frame.payload[..], &[sequence as u8, index as u8, 0xff]);
		}
		assert!(group.read_frame().await.unwrap().is_none());
	}
	assert!(reader.next_group().await.unwrap().is_none());
	assert_eq!(reader.finished().await.unwrap(), Position::group(6));

	let info = broadcast.consume().track("log").unwrap().info().await.unwrap();
	assert_eq!((info.priority, info.timescale), (9, None));
	assert_eq!(fetch(&broadcast, "log", 0).await.unwrap(), ["a"]);
}

/// History older than the source's max age relative to the newest group is still served.
#[tokio::test(start_paused = true)]
async fn history_older_than_any_max_age_is_served() {
	let archive = Archive::new();
	archive.track("video", 0, true).await;
	archive.object("video", 0, vec![frames(true, 0, 0..2)]).await;
	// Hours of media later.
	archive.object("video", 0, vec![frames(true, 10_000, 0..2)]).await;

	let (broadcast, _run) = serve(&archive.store).await;
	assert_eq!(fetch(&broadcast, "video", 0).await.unwrap(), expected(true, 0, 0..2));
	let mut reader = subscribe(&broadcast, "video").await;
	assert_eq!(next(&mut reader).await.unwrap(), (0, expected(true, 0, 0..2)));
}

/// One GET serves every group of an object, and a group split across objects is served whole.
#[tokio::test(start_paused = true)]
async fn groups_come_from_cached_objects_and_span_them() {
	let archive = Archive::new();
	archive.track("video", 0, true).await;
	archive
		.object(
			"video",
			0,
			vec![frames(true, 0, 0..2), frames(true, 1, 0..2), frames(true, 2, 0..3)],
		)
		.await;
	archive.object("video", 3, vec![frames(true, 2, 3..5)]).await;
	archive.object("video", 0, vec![frames(true, 3, 0..1)]).await;

	let (broadcast, _run) = serve(&archive.store).await;
	let mock = archive.store.inner().clone();
	mock.take();
	assert_eq!(fetch(&broadcast, "video", 0).await.unwrap(), expected(true, 0, 0..2));
	assert_eq!(fetch(&broadcast, "video", 1).await.unwrap(), expected(true, 1, 0..2));
	assert_eq!(mock.gets().len(), 1, "both groups come from one object");

	assert_eq!(fetch(&broadcast, "video", 2).await.unwrap(), expected(true, 2, 0..5));
}

/// A group every reader narrowed past stops its GET, so the groups the fill still owes are read
/// rather than waiting behind it.
#[tokio::test(start_paused = true)]
async fn a_narrowed_read_skips_the_group_it_left() {
	let archive = Archive::new();
	archive.track("video", 0, true).await;
	for sequence in 0..4 {
		archive.object("video", 0, vec![timed(sequence, 0..1)]).await;
	}

	let (broadcast, _run) = serve(&archive.store).await;
	let mock = archive.store.inner().clone();
	mock.take();
	let gate = mock.stall_gets("rec/video/");
	let ascending = |start| {
		Subscription::default()
			.with_delay(Delay::default().with_max(None))
			.with_ranges([from(start)])
			.with_order(Order::Ascending)
	};
	let mut subscriber = broadcast
		.consume()
		.track("video")
		.unwrap()
		.subscribe(ascending(0))
		.await
		.unwrap();
	let waiter = kio::Waiter::noop();
	let _ = subscriber.poll_recv_group(&waiter);
	assert!(object_read(&mock, 0).await, "group 0 is read from its object");

	subscriber.update(ascending(1_000)).unwrap();
	let _ = subscriber.poll_recv_group(&waiter);
	assert!(
		object_read(&mock, 1_000).await,
		"group 1000 waited behind group 0, which nobody reads"
	);
	gate.add_permits(16);
}

/// Whether the video object starting at `sequence` is requested once the runtime settles.
/// Bounded by yields rather than time, so the paused clock stays put.
async fn object_read(mock: &Mock, sequence: u64) -> bool {
	let prefix = format!("rec/video/{sequence:019}-");
	for _ in 0..1000 {
		if mock.gets().iter().any(|get| get.starts_with(&prefix)) {
			return true;
		}
		tokio::task::yield_now().await;
	}
	false
}

#[tokio::test(start_paused = true)]
async fn a_gap_reads_as_dropped() {
	let archive = Archive::new();
	archive.track("video", 0, true).await;
	archive
		.object("video", 0, vec![frames(true, 0, 0..1), frames(true, 2, 0..1)])
		.await;
	archive.object("video", 0, vec![frames(true, 5, 0..1)]).await;
	archive.object("video", 0, vec![frames(true, 6, 0..1)]).await;

	let (broadcast, _run) = serve(&archive.store).await;
	// Inside an object, and between two.
	not_found(fetch(&broadcast, "video", 1).await);
	not_found(fetch(&broadcast, "video", 3).await);
	not_found(fetch(&broadcast, "video", 4).await);

	let mut reader = subscribe(&broadcast, "video").await;
	for expected in [0, 2, 5, 6] {
		assert_eq!(sequence(&mut reader).await, Some(expected));
	}
}

/// A pruned start answers with what remains: the groups below it, and a group's pruned head,
/// read as dropped.
#[tokio::test(start_paused = true)]
async fn a_pruned_start_reads_as_dropped() {
	let archive = Archive::new();
	archive.track("video", 0, true).await;
	archive
		.object("video", 2, vec![frames(true, 5, 2..4), frames(true, 6, 0..2)])
		.await;
	archive.object("video", 0, vec![frames(true, 7, 0..2)]).await;
	archive.object("video", 0, vec![frames(true, 8, 0..1)]).await;

	let (broadcast, _run) = serve(&archive.store).await;
	not_found(fetch(&broadcast, "video", 0).await);
	not_found(fetch(&broadcast, "video", 4).await);
	not_found(fetch(&broadcast, "video", 5).await);
	assert_eq!(fetch(&broadcast, "video", 6).await.unwrap(), expected(true, 6, 0..2));

	// An object pruned after it was listed reads as dropped too.
	archive.store.inner().hide_gets("rec/video/0000000000000000007-");
	not_found(fetch(&broadcast, "video", 7).await);
}

/// A GET that fails while the store still lists the object refuses its group only for now, so
/// a later read asks again once the store recovers.
#[tokio::test(start_paused = true)]
async fn a_failed_get_refuses_its_group_for_now() {
	let archive = Archive::new();
	archive.track("video", 0, true).await;
	archive.object("video", 0, vec![frames(true, 0, 0..2)]).await;
	archive.object("video", 0, vec![frames(true, 1, 0..1)]).await;

	let (broadcast, _run) = serve(&archive.store).await;
	archive.store.inner().fail_gets("rec/video/0000000000000000000-");
	let err = fetch(&broadcast, "video", 0).await.unwrap_err();
	assert!(!matches!(err, moq_net::Error::NotFound), "{err}");
	archive.store.inner().heal_gets();
	assert_eq!(fetch(&broadcast, "video", 0).await.unwrap(), expected(true, 0, 0..2));
}

/// A growing archive is followed through each track's own cursor, a key sorting before
/// another track's cursor included, and a track added mid-recording joins.
#[tokio::test(start_paused = true)]
async fn a_growing_archive_is_followed() {
	let archive = Archive::new();
	archive.track("a", 0, true).await;
	archive.track("b", 0, true).await;
	archive.object("a", 0, vec![frames(true, 0, 0..1)]).await;
	archive.object("b", 0, vec![frames(true, 50, 0..1)]).await;

	let (broadcast, _run) = serve(&archive.store).await;
	let mut a = subscribe(&broadcast, "a").await;
	let mut b = subscribe(&broadcast, "b").await;
	assert_eq!(sequence(&mut b).await, Some(50));

	// `a/1-...` sorts before `b/50-...`: one prefix-wide cursor would skip it.
	archive.object("a", 0, vec![frames(true, 1, 0..2)]).await;
	archive.object("b", 0, vec![frames(true, 51, 0..1)]).await;
	// A new object continuing the open group 51.
	archive
		.object("b", 1, vec![frames(true, 51, 1..2), frames(true, 52, 0..1)])
		.await;
	assert_eq!(next(&mut a).await.unwrap(), (0, expected(true, 0, 0..1)));
	assert_eq!(sequence(&mut a).await, Some(1));
	assert_eq!(next(&mut b).await.unwrap(), (51, expected(true, 51, 0..2)));

	assert!(matches!(
		broadcast.consume().track("c").unwrap().info().await,
		Err(moq_net::Error::NotFound)
	));
	archive.track("c", 0, false).await;
	archive.object("c", 0, vec![frames(false, 0, 0..1)]).await;
	sleep(2 * REFRESH).await;
	let mut c = subscribe(&broadcast, "c").await;
	archive.object("c", 0, vec![frames(false, 1, 0..1)]).await;
	assert_eq!(next(&mut c).await.unwrap(), (0, expected(false, 0, 0..1)));
}

/// Without markers, an archive whose producer stopped stays live.
#[tokio::test(start_paused = true)]
async fn an_archive_without_markers_stays_open() {
	let archive = Archive::new();
	archive.track("video", 0, true).await;
	archive
		.object("video", 0, vec![frames(true, 0, 0..1), frames(true, 1, 0..1)])
		.await;
	// A track `end` alone is not final: the broadcast never wrote `.end`.
	archive.end("video", End::Finish(At { group: 2, frame: 0 })).await;

	let (broadcast, _run) = serve(&archive.store).await;
	let mut reader = subscribe(&broadcast, "video").await;
	assert_eq!(next(&mut reader).await.unwrap(), (0, expected(true, 0, 0..1)));
	let mut group = reader.next_group().await.unwrap().unwrap();
	assert!(group.read_frame().await.unwrap().is_some());
	let pending = timeout(30 * REFRESH, group.read_frame()).await;
	assert!(pending.is_err(), "the newest group stays open");
}

/// Closing the broadcast ends a followed archive and cancels the tracks it still served live,
/// the open newest group included.
#[tokio::test(start_paused = true)]
async fn closing_the_broadcast_cancels_the_live_tracks() {
	let archive = Archive::new();
	archive.track("video", 0, true).await;
	archive.object("video", 0, vec![frames(true, 0, 0..1)]).await;

	let (broadcast, run) = serve(&archive.store).await;
	let mut reader = subscribe(&broadcast, "video").await;
	let mut group = reader.next_group().await.unwrap().unwrap();
	assert!(group.read_frame().await.unwrap().is_some());

	broadcast.close();
	run.await.unwrap().unwrap();
	let err = group.read_frame().await.unwrap_err();
	assert!(matches!(err, moq_net::Error::Cancel), "{err}");
}

/// A continuation that skips frames of the open group fails the replay, and the open group is
/// aborted with the track rather than dropped.
#[tokio::test(start_paused = true)]
async fn a_gapped_continuation_aborts_the_open_group() {
	let archive = Archive::new();
	archive.track("video", 0, true).await;
	archive.object("video", 0, vec![frames(true, 0, 0..1)]).await;

	let (broadcast, run) = serve(&archive.store).await;
	let mut reader = subscribe(&broadcast, "video").await;
	let mut group = reader.next_group().await.unwrap().unwrap();
	assert!(group.read_frame().await.unwrap().is_some());

	// Frame 1 of group 0 is missing.
	archive.object("video", 2, vec![frames(true, 0, 2..3)]).await;
	assert!(matches!(run.await.unwrap(), Err(Error::Sequence)));
	let err = group.read_frame().await.unwrap_err();
	assert!(
		matches!(err, moq_net::Error::Stream(moq_net::StreamError::Internal)),
		"{err}"
	);
}

/// A store failure ends the replay and aborts every served track with it, the open newest group
/// included, so readers see the failure rather than a dropped producer.
#[tokio::test(start_paused = true)]
async fn a_store_failure_aborts_the_served_tracks() {
	let archive = Archive::new();
	archive.track("video", 0, true).await;
	archive.object("video", 0, vec![frames(true, 0, 0..1)]).await;

	let (broadcast, run) = serve(&archive.store).await;
	let mut reader = subscribe(&broadcast, "video").await;
	let mut group = reader.next_group().await.unwrap().unwrap();
	assert!(group.read_frame().await.unwrap().is_some());

	archive.store.inner().fail_lists();
	assert!(run.await.unwrap().is_err());
	let failed = |err: &moq_net::Error| matches!(err, moq_net::Error::Stream(moq_net::StreamError::Internal));
	let err = group.read_frame().await.unwrap_err();
	assert!(failed(&err), "{err}");
	let Err(err) = reader.next_group().await else {
		panic!("the track is aborted");
	};
	assert!(failed(&err), "{err}");
}

/// A completed archive ends each track at its recorded end, mid-group included, and an
/// errored track replays its stored groups, then its error.
#[tokio::test(start_paused = true)]
async fn a_completed_archive_ends_as_recorded() {
	let archive = Archive::new();
	archive.track("video", 0, true).await;
	archive.track("audio", 0, true).await;
	archive.track("errored", 0, true).await;
	archive.track("tail", 0, true).await;
	archive
		.object("video", 0, vec![frames(true, 0, 0..2), frames(true, 1, 0..2)])
		.await;
	archive
		.object("audio", 0, vec![frames(true, 0, 0..2), frames(true, 1, 0..2)])
		.await;
	archive
		.object("errored", 0, vec![frames(true, 0, 0..1), frames(true, 1, 0..1)])
		.await;
	archive.end("video", End::Finish(At { group: 2, frame: 0 })).await;
	archive.end("audio", End::Finish(At { group: 1, frame: 2 })).await;
	archive.end("errored", End::Error(64 + 7)).await;
	// Empty groups store no object, only the end past them.
	archive.object("tail", 0, vec![frames(true, 0, 0..1)]).await;
	archive.end("tail", End::Finish(At { group: 4, frame: 0 })).await;
	archive.complete().await;

	let (broadcast, _run) = serve(&archive.store).await;
	for (track, end) in [("video", Position::group(2)), ("audio", Position::new(1, 2))] {
		let mut reader = subscribe(&broadcast, track).await;
		assert_eq!(next(&mut reader).await.unwrap(), (0, expected(true, 0, 0..2)));
		assert_eq!(next(&mut reader).await.unwrap(), (1, expected(true, 1, 0..2)));
		assert!(next(&mut reader).await.is_none());
		assert_eq!(reader.finished().await.unwrap(), end, "{track}");
	}

	// Every stored group, then the error the source ended with, and still fetchable after.
	let mut reader = subscribe(&broadcast, "errored").await;
	assert_eq!(next(&mut reader).await.unwrap(), (0, expected(true, 0, 0..1)));
	assert_eq!(next(&mut reader).await.unwrap(), (1, expected(true, 1, 0..1)));
	let end = timeout(Duration::from_secs(10), reader.next_group())
		.await
		.expect("the track ends");
	let end = end.map(|group| group.map(|group| group.start));
	assert!(
		matches!(end, Err(moq_net::Error::Stream(moq_net::StreamError::App(7)))),
		"{end:?}"
	);
	assert_eq!(fetch(&broadcast, "errored", 0).await.unwrap(), expected(true, 0, 0..1));

	// The sequences the archive holds nothing for are skipped, and the end settles over them.
	let mut reader = subscribe(&broadcast, "tail").await;
	assert_eq!(next(&mut reader).await.unwrap(), (0, expected(true, 0, 0..1)));
	assert!(next(&mut reader).await.is_none());
	assert_eq!(reader.finished().await.unwrap(), Position::group(4));
}

/// Markers written while the consumer follows end the tracks then.
#[tokio::test(start_paused = true)]
async fn markers_end_a_followed_archive() {
	let archive = Archive::new();
	archive.track("video", 0, true).await;
	archive.object("video", 0, vec![frames(true, 0, 0..2)]).await;

	let (broadcast, _run) = serve(&archive.store).await;
	let mut reader = subscribe(&broadcast, "video").await;
	archive.object("video", 2, vec![frames(true, 0, 2..3)]).await;
	archive.end("video", End::Finish(At { group: 1, frame: 0 })).await;
	archive.complete().await;
	assert_eq!(next(&mut reader).await.unwrap(), (0, expected(true, 0, 0..3)));
	assert!(next(&mut reader).await.is_none());
}

/// A track removed and re-added under its name replays both runs in sequence.
#[tokio::test(start_paused = true)]
async fn a_re_added_track_replays_both_runs() {
	let store = Store::new(Mock::memory(), "rec");
	let source = broadcast::Info::new().produce();
	// Each run is written before the recorder reads it, so superseded groups must stay fresh.
	let info = track::Info::default()
		.with_timescale(Timescale::MILLI)
		.with_max_age(None);
	let config = crate::producer::Config::default().with_flush(Duration::from_secs(1));
	let recording = tokio::spawn(crate::Producer::new(store.clone(), source.consume(), config).run());

	for (run, sequences) in [(0, 0..2), (1, 2..4)] {
		let track = source.create_track("video", info.clone()).unwrap();
		while track.subscription().is_none() {
			sleep(Duration::from_millis(1)).await;
		}
		for sequence in sequences {
			let mut group = track.create_group(group::Info { start: sequence }).unwrap().unwrap();
			let pts = Timestamp::from_millis(sequence * 1000).unwrap();
			group.write_frame(pts, format!("run{run}/{sequence}")).unwrap();
			group.finish().unwrap();
		}
		let track = track.finish().unwrap();
		sleep(Duration::from_secs(2)).await;
		drop(track);
		sleep(Duration::from_millis(10)).await;
	}
	source.close();
	recording.await.unwrap().unwrap();

	let (broadcast, _run) = serve(&store).await;
	let mut reader = subscribe(&broadcast, "video").await;
	let mut read = Vec::new();
	while let Some((_, frames)) = next(&mut reader).await {
		read.extend(frames);
	}
	assert_eq!(read, ["0@run0/0", "1000@run0/1", "2000@run1/2", "3000@run1/3"]);
}

#[tokio::test(start_paused = true)]
async fn an_empty_or_unrecognized_prefix_is_refused() {
	let open = |store: Store<Mock>| async move {
		let broadcast = broadcast::Info::new().produce();
		Consumer::open(store, broadcast, Config::default()).await.err()
	};

	let archive = Archive::new();
	assert!(matches!(open(archive.store.clone()).await, Some(Error::NotFound(_))));

	// A key the layout does not name, at the root or under a track.
	let archive = Archive::new();
	archive.track("video", 0, true).await;
	archive.raw("stray", b"x").await;
	assert!(matches!(open(archive.store.clone()).await, Some(Error::Path(_))));

	let archive = Archive::new();
	archive.track("video", 0, true).await;
	archive.raw("video/segments/0000000000000000000", b"x").await;
	assert!(matches!(open(archive.store.clone()).await, Some(Error::Path(_))));

	// Release's version 1 layout and an older main build's version 4 are refused by version,
	// before any of their keys is read.
	for (info, key, version) in [
		(
			&br#"{"version":1,"priority":0,"timescale":1000}"#[..],
			"video/segments/0000000000000000000",
			1,
		),
		(
			br#"{"version":4,"priority":0,"timescale":1000}"#,
			"video/0000000000000000000-0000000000000000000-0000000000000000000",
			4,
		),
	] {
		let archive = Archive::new();
		archive.raw("video/.info", info).await;
		archive.raw(key, b"x").await;
		assert_eq!(open(archive.store.clone()).await, Some(Error::Version(version)));
	}

	// A directory that is not a canonical track name.
	let archive = Archive::new();
	archive
		.raw("catalog.json/.info", br#"{"version":2,"priority":0}"#)
		.await;
	assert!(matches!(open(archive.store.clone()).await, Some(Error::Track)));
}

/// Object stores promise no listing order.
#[tokio::test(start_paused = true)]
async fn an_unordered_listing_is_sorted() {
	let archive = Archive::new();
	archive.track("video", 0, true).await;
	for sequence in 0..4 {
		archive.object("video", 0, vec![frames(true, sequence, 0..1)]).await;
	}
	archive.store.inner().unordered();

	let (broadcast, _run) = serve(&archive.store).await;
	let mut reader = subscribe(&broadcast, "video").await;
	for sequence in 0..3 {
		assert_eq!(
			next(&mut reader).await.unwrap(),
			(sequence, expected(true, sequence, 0..1))
		);
	}
	assert_eq!(super::tests::sequence(&mut reader).await, Some(3));
}

/// Read `range` in order with no age limit: the starts of the groups it delivers before
/// it stops delivering. A range ending before the live edge parks once read, as on any live
/// track.
async fn read_range(broadcast: &broadcast::Producer, track: &str, range: track::Range) -> Vec<u64> {
	let track = broadcast.consume().track(track).unwrap();
	let subscription = Subscription::default()
		.with_ranges([range])
		.with_order(Order::Ascending)
		.with_delay(moq_net::track::Delay::default().with_max(None));
	let mut reader = track.subscribe(subscription).await.unwrap().ordered();
	let mut read = Vec::new();
	while let Ok(group) = timeout(5 * REFRESH, reader.next_group()).await {
		match group.unwrap() {
			Some(group) => read.push(group.start),
			None => break,
		}
	}
	read
}

/// A time on a millisecond track.
fn at(millis: u64) -> track::Anchor {
	track::Anchor::Time(millis)
}

/// Group `sequence`'s frames `frames` on a timed track, keyed by its first frame's time:
/// `sequence` seconds.
fn timed(sequence: u64, frames: std::ops::Range<u64>) -> Entry {
	Entry {
		sequence: sequence * 1000,
		..self::frames(true, sequence, frames)
	}
}

/// Groups `sequences` of a timed track, two to an object, each group starting at its
/// sequence in seconds.
async fn stored(archive: &Archive, sequences: impl IntoIterator<Item = u64>) {
	let sequences: Vec<u64> = sequences.into_iter().collect();
	for pair in sequences.chunks(2) {
		let entries = pair.iter().map(|sequence| timed(*sequence, 0..3)).collect();
		archive.object("video", 0, entries).await;
	}
}

/// A time range older than the newest group resolves from the archive's keys, not from
/// the one group its cache holds: the start reaches the group containing it, and the end
/// stops just after the group containing it.
#[tokio::test(start_paused = true)]
async fn a_time_range_resolves_from_the_archive() {
	let archive = Archive::new();
	archive.track("video", 0, true).await;
	stored(&archive, 0..10).await;

	let (broadcast, _run) = serve(&archive.store).await;
	let range = |start, end| track::Range { start, end };
	assert_eq!(
		read_range(&broadcast, "video", range(at(4_500), Some(at(6_500)))).await,
		[4_000, 5_000, 6_000]
	);
	// A start on the first frame of its group, in the second object of a pair.
	assert_eq!(
		read_range(&broadcast, "video", range(at(3_000), Some(at(3_001)))).await,
		[3_000]
	);
	// An end alone, from the oldest group.
	assert_eq!(
		read_range(&broadcast, "video", range(at(0), Some(at(1_200)))).await,
		[0, 1_000]
	);
	// The cache now holds groups 6 and 9, but not what lies between: still looked up.
	assert_eq!(
		read_range(&broadcast, "video", range(at(8_500), Some(at(8_600)))).await,
		[8_000]
	);

	// A start alone reads on through the live edge.
	let track = broadcast.consume().track("video").unwrap();
	let subscription = Subscription::default()
		.with_ranges([range(at(7_002), None)])
		.with_order(Order::Ascending)
		.with_delay(moq_net::track::Delay::default().with_max(None));
	let mut reader = track.subscribe(subscription).await.unwrap().ordered();
	for expected in 7..10 {
		assert_eq!(sequence(&mut reader).await, Some(expected * 1000));
	}
}

/// A completed archive's track ends only once a time range's lookup answers, so the range
/// still reads its history.
#[tokio::test(start_paused = true)]
async fn a_completed_archive_reads_a_time_range() {
	let archive = Archive::new();
	archive.track("video", 0, true).await;
	stored(&archive, 0..6).await;
	archive.end("video", End::Finish(At { group: 6_000, frame: 0 })).await;
	archive.complete().await;

	let (broadcast, _run) = serve(&archive.store).await;
	let range = track::Range {
		start: at(2_500),
		end: None,
	};
	assert_eq!(
		read_range(&broadcast, "video", range).await,
		[2_000, 3_000, 4_000, 5_000]
	);
}

/// A time older than every stored object reaches back to the oldest stored group.
#[tokio::test(start_paused = true)]
async fn a_time_before_the_archive_starts_at_its_oldest_group() {
	let archive = Archive::new();
	archive.track("video", 0, true).await;
	// Groups 0 to 2 were pruned.
	stored(&archive, 3..8).await;

	let (broadcast, _run) = serve(&archive.store).await;
	let range = track::Range {
		start: at(500),
		end: Some(at(4_500)),
	};
	assert_eq!(read_range(&broadcast, "video", range).await, [3_000, 4_000]);
}

/// A time older than an archive whose oldest object continues a pruned group reaches back to
/// the oldest group stored whole.
#[tokio::test(start_paused = true)]
async fn a_time_before_a_pruned_head_starts_at_the_oldest_whole_group() {
	let archive = Archive::new();
	archive.track("video", 0, true).await;
	archive.object("video", 2, vec![timed(5, 2..4), timed(6, 0..2)]).await;
	archive.object("video", 0, vec![timed(7, 0..2)]).await;
	archive.object("video", 0, vec![timed(8, 0..1)]).await;

	let (broadcast, _run) = serve(&archive.store).await;
	let range = track::Range {
		start: at(500),
		end: Some(at(7_500)),
	};
	assert_eq!(read_range(&broadcast, "video", range).await, [6_000, 7_000]);
}

/// A time in a gap the archive never stored starts at the group before the gap, the newest
/// starting before it, and the gap reads as missing.
#[tokio::test(start_paused = true)]
async fn a_time_in_a_gap_starts_before_it() {
	let archive = Archive::new();
	archive.track("video", 0, true).await;
	stored(&archive, [0, 1, 2, 3, 6, 7, 8, 9]).await;

	let (broadcast, _run) = serve(&archive.store).await;
	let range = track::Range {
		start: at(4_500),
		end: Some(at(6_500)),
	};
	assert_eq!(read_range(&broadcast, "video", range).await, [3_000, 6_000]);
}

/// A continuation's first frame bounds no group start: an object continuing group 0 at a
/// later timestamp than the group at 500 it then starts still holds the group containing a
/// time between the two.
#[tokio::test(start_paused = true)]
async fn a_continuation_does_not_bound_a_group_start() {
	let archive = Archive::new();
	archive.track("video", 0, true).await;
	let frame = |sequence: u64, index: u64, pts: u64| Frame {
		timestamp: Some(pts),
		payload: Bytes::from(format!("{sequence}/{index}")),
	};
	archive
		.object(
			"video",
			0,
			vec![Entry {
				sequence: 0,
				frames: vec![frame(0, 0, 0)],
			}],
		)
		.await;
	archive
		.object(
			"video",
			1,
			vec![
				Entry {
					sequence: 0,
					frames: vec![frame(0, 1, 1_000)],
				},
				Entry {
					sequence: 500,
					frames: vec![frame(1, 0, 500)],
				},
			],
		)
		.await;
	stored(&archive, 2..4).await;

	let (broadcast, _run) = serve(&archive.store).await;
	let range = track::Range {
		start: at(600),
		end: Some(at(700)),
	};
	assert_eq!(read_range(&broadcast, "video", range).await, [500]);
}

/// A group starts at its key even when its first frame comes later, as one a seek opened, so
/// the archive resolves a time to the group the live cache would.
#[tokio::test(start_paused = true)]
async fn a_time_resolves_by_key_not_first_frame() {
	let archive = Archive::new();
	archive.track("video", 0, true).await;
	let frame = |sequence: u64, pts: u64| Frame {
		timestamp: Some(pts),
		payload: Bytes::from(format!("{sequence}")),
	};
	for (sequence, pts) in [(1_000, 1_000), (2_000, 2_500), (3_000, 3_000)] {
		let entry = Entry {
			sequence,
			frames: vec![frame(sequence, pts)],
		};
		archive.object("video", 0, vec![entry]).await;
	}

	let (broadcast, _run) = serve(&archive.store).await;
	let range = track::Range {
		start: at(2_200),
		end: Some(at(2_300)),
	};
	assert_eq!(read_range(&broadcast, "video", range).await, [2_000]);
}

/// A scale-less track's times are bare starts, which the archive looks up by key.
#[tokio::test(start_paused = true)]
async fn a_scale_less_track_looks_up_a_time_by_key() {
	let archive = Archive::new();
	archive.track("log", 0, false).await;
	for sequence in [0, 2, 5, 7] {
		archive.object("log", 0, vec![frames(false, sequence, 0..1)]).await;
	}

	let (broadcast, _run) = serve(&archive.store).await;
	let range = track::Range {
		start: at(3),
		end: Some(at(5)),
	};
	assert_eq!(read_range(&broadcast, "log", range).await, [2, 5]);
}

/// Serve `store` on `broadcast` with `config`, returning each track's index.
async fn open(
	store: &Store<Mock>,
	broadcast: &broadcast::Producer,
	config: Config,
) -> (HashMap<String, Arc<Mutex<Index>>>, JoinHandle<Result<()>>) {
	let consumer = Consumer::open(store.clone(), broadcast.clone(), config.with_refresh(REFRESH))
		.await
		.unwrap();
	let indexes = consumer
		.tracks
		.iter()
		.map(|(name, track)| (name.clone(), track.index.clone()))
		.collect();
	(indexes, tokio::spawn(consumer.run()))
}

/// A reader attached to a rolling archive indexes about what the store retains, however many
/// retention rotations pass without it reading history.
#[tokio::test(start_paused = true)]
async fn a_rolling_archive_index_tracks_retention() {
	const RETAINED: u64 = 4;
	let archive = Archive::new();
	archive.track("video", 0, true).await;
	for sequence in 0..RETAINED {
		archive.object("video", 0, vec![timed(sequence, 0..1)]).await;
	}

	let broadcast = broadcast::Info::new().produce();
	let (indexes, _run) = open(&archive.store, &broadcast, Config::default()).await;
	let index = &indexes["video"];
	for sequence in RETAINED..RETAINED + 100 {
		archive.object("video", 0, vec![timed(sequence, 0..1)]).await;
		archive.prune("video", timed(sequence - RETAINED, 0..1).sequence).await;
		sleep(REFRESH).await;
		let held = index.lock().unwrap().len();
		// Twice the retained objects, plus the one a refresh adds before it lists them all.
		assert!(
			held <= 2 * RETAINED as usize + 1,
			"{held} objects indexed after {sequence}, {RETAINED} retained"
		);
	}
}

/// A time older than what retention kept reaches the oldest retained group, though the reader
/// still indexes the pruned objects and caches none of them, with one listing rather than a GET
/// per pruned object.
#[tokio::test(start_paused = true)]
async fn a_seek_across_pruned_history_reaches_the_oldest_retained_group() {
	let archive = Archive::new();
	archive.track("video", 0, true).await;
	stored(&archive, 0..10).await;

	let broadcast = broadcast::Info::new().produce();
	let (_indexes, _run) = open(&archive.store, &broadcast, Config::default().with_cache(0)).await;
	// Retention deletes the three oldest objects after the reader listed them.
	for sequence in [0, 2, 4] {
		archive.prune("video", sequence * 1000).await;
	}
	let mock = archive.store.inner().clone();
	mock.take();
	let range = |start| track::Range {
		start: at(start),
		end: Some(at(6_500)),
	};
	assert_eq!(read_range(&broadcast, "video", range(4_500)).await, [6_000]);
	let pruned = mock
		.gets()
		.into_iter()
		.filter(|get| {
			["0000000000000000000-", "0000000000000002000-", "0000000000000004000-"]
				.iter()
				.any(|key| get.contains(key))
		})
		.count();
	assert_eq!(pruned, 1, "one pruned GET lists the track again");
	assert_eq!(read_range(&broadcast, "video", range(500)).await, [6_000]);
}

/// Retention that replaced everything the reader indexed before it refreshed leaves a seek the
/// newer objects the store holds, which the listing the pruned GET starts adds to the index.
#[tokio::test(start_paused = true)]
async fn a_seek_finds_objects_retention_replaced_the_index_with() {
	let archive = Archive::new();
	archive.track("video", 0, true).await;
	stored(&archive, 0..2).await;

	let broadcast = broadcast::Info::new().produce();
	let (indexes, _run) = open(&archive.store, &broadcast, Config::default().with_cache(0)).await;
	stored(&archive, 2..6).await;
	archive.prune("video", 0).await;
	let mock = archive.store.inner().clone();
	mock.take();

	let track = broadcast.consume().track("video").unwrap();
	let subscription = Subscription::default()
		.with_ranges([from(500)])
		.with_order(Order::Ascending)
		.with_delay(Delay::default().with_max(None));
	let mut reader = track.subscribe(subscription).await.unwrap().ordered();
	assert!(sequence(&mut reader).await.is_some());
	// Before any refresh lists after the cursor: yields, not time.
	for _ in 0..1000 {
		if indexes["video"].lock().unwrap().len() == 2 {
			break;
		}
		tokio::task::yield_now().await;
	}
	assert_eq!(
		indexes["video"].lock().unwrap().len(),
		2,
		"both retained objects are indexed"
	);
	assert!(
		!mock
			.take()
			.iter()
			.any(|op| matches!(op, crate::mock::Op::List { offset: Some(offset), .. } if !offset.ends_with(".info"))),
		"no refresh listed after the cursor"
	);
}

/// A key the store still lists but no longer serves, as a listing that lags its deletes leaves
/// it, costs a seek one GET: the listing its 404 starts never adds it back, so the lookup moves
/// on to what the store serves instead of reading it again.
#[tokio::test(start_paused = true)]
async fn a_seek_reads_a_listed_but_missing_object_once() {
	let archive = Archive::new();
	archive.track("video", 0, true).await;
	stored(&archive, 0..10).await;

	let broadcast = broadcast::Info::new().produce();
	let (_indexes, _run) = open(&archive.store, &broadcast, Config::default().with_cache(0)).await;
	let mock = archive.store.inner().clone();
	const STALE: &str = "rec/video/0000000000000002000-";
	mock.hide_gets(STALE);
	// Room for a second read, which a lookup that relisted it back would take and then wait on.
	let gate = mock.stall_gets(STALE);
	gate.add_permits(2);
	mock.take();

	let range = track::Range {
		start: at(2_500),
		end: Some(at(4_500)),
	};
	// The newest group the store serves at or before the time, then the range's end.
	assert_eq!(read_range(&broadcast, "video", range).await, [1_000, 4_000]);
	let stale = mock.gets().iter().filter(|get| get.contains(STALE)).count();
	assert_eq!(stale, 1, "the missing object is read once");
}

/// Recorded through a real DVR producer while a reader stays attached for many rotations: its
/// index stays about the size of what the store retains, and a seek to the start of time
/// reaches the oldest group the store holds, as a direct listing finds it.
#[tokio::test(start_paused = true)]
async fn a_reader_follows_a_real_dvr_through_its_rotations() {
	let store = Store::new(Mock::memory(), "rec");
	let source = broadcast::Info::new().produce();
	let video = source
		.create_track("video", track::Info::default().with_timescale(Timescale::MILLI))
		.unwrap();
	let dvr = crate::producer::Dvr::new(Duration::from_secs(2), Duration::from_secs(1));
	let config = crate::producer::Config::default()
		.with_duration(Duration::from_secs(1))
		.with_flush(Duration::from_secs(100))
		.with_dvr(dvr);
	let _recording = tokio::spawn(crate::Producer::new(store.clone(), source.consume(), config).run());
	while video.subscription().is_none() {
		sleep(Duration::from_millis(1)).await;
	}
	let write = |sequence: u64| {
		let mut group = video.create_group(group::Info { start: sequence * 1000 }).unwrap().unwrap();
		let pts = Timestamp::from_millis(sequence * 1000).unwrap();
		group.write_frame(pts, Bytes::from(format!("{sequence}"))).unwrap();
		group.finish().unwrap();
	};
	// Each group is its own object, stored once the next one starts.
	for sequence in 0..2 {
		write(sequence);
		sleep(Duration::from_secs(1)).await;
	}

	// Groups nobody reads leave memory a second after their last write, as an origin expires
	// them, so the seek reaches the store.
	let mut info = broadcast::Info::new();
	info.pool = moq_net::cache::Pool::new(moq_net::cache::Config::default().with_expiry(Duration::from_secs(1)));
	let pool = info.pool.clone();
	let broadcast = info.produce();
	let (indexes, _run) = open(&store, &broadcast, Config::default().with_cache(0)).await;
	let index = &indexes["video"];
	let dir = layout::track_dir(store.prefix(), "video").unwrap();
	let mut retained = Vec::new();
	for sequence in 2..300 {
		write(sequence);
		sleep(Duration::from_secs(1)).await;
		pool.poll(kio::clock::now(), &kio::Waiter::noop());

		let listed = index::list(store.inner(), &dir, &dir.clone().join(layout::TRACK_INFO), false)
			.await
			.unwrap();
		retained.push(listed.objects.len());
		// Relisting halves the index back to what the store held at that listing, so the bound
		// follows the most it retained.
		let most = retained.iter().max().unwrap();
		let held = index.lock().unwrap().len();
		assert!(
			held <= 2 * most + 1,
			"{held} objects indexed after {sequence}, at most {most} retained"
		);

		if sequence % 50 == 0 {
			let oldest = listed.objects.first().unwrap().0.0;
			let track = broadcast.consume().track("video").unwrap();
			let subscription = Subscription::default()
				.with_ranges([from(0)])
				.with_order(Order::Ascending)
				.with_delay(Delay::default().with_max(None));
			let mut reader = track.subscribe(subscription).await.unwrap().ordered();
			assert_eq!(
				super::tests::sequence(&mut reader).await,
				Some(oldest),
				"seek after {sequence}"
			);
		}
	}
	// Retention really rotated: the store held a few objects at a time, not all of them.
	assert!(retained.iter().all(|count| *count <= 5), "{retained:?}");
}
