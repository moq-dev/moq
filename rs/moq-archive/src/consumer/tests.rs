use std::time::Duration;

use bytes::Bytes;
use kio::clock::{sleep, timeout};
use moq_net::track::{Order, Position, Subscription};
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
	Entry::Group {
		sequence,
		frames: frames.map(|index| frame(timed, sequence, index)).collect(),
	}
}

fn datagram(timed: bool, sequence: u64) -> Entry {
	Entry::Datagram {
		sequence,
		frame: frame(timed, sequence, 0),
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

fn read(timestamp: Option<Timestamp>, payload: Bytes) -> String {
	describe(&Frame {
		timestamp: timestamp.map(Timestamp::value),
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
		.with_range(0..)
		.with_order(Order::Ascending)
		.with_max_delay(None);
	track.subscribe(subscription).await.unwrap().ordered()
}

/// The next group's sequence, without waiting for it to end.
async fn sequence(reader: &mut track::Ordered) -> Option<u64> {
	timeout(Duration::from_secs(10), reader.next_group())
		.await
		.expect("a group arrives")
		.unwrap()
		.map(|group| group.sequence)
}

/// The next group and every frame in it, once the group ends.
async fn next(reader: &mut track::Ordered) -> Option<(u64, Vec<String>)> {
	let mut group = timeout(Duration::from_secs(10), reader.next_group())
		.await
		.expect("a group arrives")
		.unwrap()?;
	let mut frames = Vec::new();
	while let Some(frame) = timeout(Duration::from_secs(10), group.read_frame())
		.await
		.expect("the group ends")
		.unwrap()
	{
		frames.push(read(frame.timestamp, frame.payload));
	}
	Some((group.sequence, frames))
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
	let mut log = source
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
		let mut group = video.create_group(group::Info { sequence }).unwrap();
		for index in 0..3 {
			let pts = Timestamp::from_millis(sequence * 10_000 + index).unwrap();
			group
				.write_frame(pts, Bytes::from(vec![sequence as u8, index as u8, 0xff]))
				.unwrap();
		}
		group.finish().unwrap();
		sleep(Duration::from_millis(10)).await;
	}
	let mut group = log.create_group(group::Info { sequence: 0 }).unwrap();
	group.write_frame(None, "a").unwrap();
	group.finish().unwrap();
	sleep(Duration::from_millis(10)).await;
	log.insert_datagram(1, None, "b").unwrap();
	sleep(Duration::from_millis(10)).await;
	video.finish().unwrap();
	log.finish().unwrap();
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
		assert_eq!(group.sequence, sequence);
		for index in 0..3 {
			let frame = group.read_frame().await.unwrap().unwrap();
			assert_eq!(frame.timestamp.unwrap().value(), sequence * 10_000 + index);
			assert_eq!(&frame.payload[..], &[sequence as u8, index as u8, 0xff]);
		}
		assert!(group.read_frame().await.unwrap().is_none());
	}
	assert!(reader.next_group().await.unwrap().is_none());
	assert_eq!(reader.finished().await.unwrap(), Position::group(6));

	// The untimed track keeps its datagram a datagram.
	let info = broadcast.consume().track("log").unwrap().info().await.unwrap();
	assert_eq!((info.priority, info.timescale), (9, None));
	assert_eq!(fetch(&broadcast, "log", 0).await.unwrap(), ["a"]);
	assert!(matches!(
		fetch(&broadcast, "log", 1).await,
		Err(moq_net::Error::NotFetchable | moq_net::Error::NotFound)
	));
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
		archive.object("video", 0, vec![frames(true, sequence, 0..1)]).await;
	}

	let (broadcast, _run) = serve(&archive.store).await;
	let mock = archive.store.inner().clone();
	mock.take();
	let gate = mock.stall_gets("rec/video/");
	let ascending = |groups| {
		Subscription::default()
			.with_max_delay(None)
			.with_range(groups)
			.with_order(Order::Ascending)
	};
	let mut subscriber = broadcast
		.consume()
		.track("video")
		.unwrap()
		.subscribe(ascending(0..))
		.await
		.unwrap();
	let waiter = kio::Waiter::noop();
	let _ = subscriber.poll_recv_group(&waiter);
	assert!(object_read(&mock, 0).await, "group 0 is read from its object");

	subscriber.update(ascending(1..)).unwrap();
	let _ = subscriber.poll_recv_group(&waiter);
	assert!(
		object_read(&mock, 1).await,
		"group 1 waited behind group 0, which nobody reads"
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
	for (track, end) in [
		("video", Position::group(2)),
		("audio", Position { group: 1, frame: 2 }),
	] {
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
	let end = end.map(|group| group.map(|group| group.sequence));
	assert!(
		matches!(end, Err(moq_net::Error::Stream(moq_net::StreamError::App(7)))),
		"{end:?}"
	);
	assert_eq!(fetch(&broadcast, "errored", 0).await.unwrap(), expected(true, 0, 0..1));

	// The sequence below the end replays as an empty group, and those before it as dropped.
	let mut reader = subscribe(&broadcast, "tail").await;
	assert_eq!(next(&mut reader).await.unwrap(), (0, expected(true, 0, 0..1)));
	assert_eq!(next(&mut reader).await.unwrap(), (3, Vec::new()));
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

/// Groups and datagrams sharing one sequence space replay in their recorded forms.
#[tokio::test(start_paused = true)]
async fn groups_and_datagrams_keep_their_forms() {
	let archive = Archive::new();
	archive.track("mixed", 0, false).await;
	archive
		.object(
			"mixed",
			0,
			vec![frames(false, 0, 0..1), datagram(false, 1), frames(false, 2, 0..1)],
		)
		.await;

	let (broadcast, _run) = serve(&archive.store).await;
	let track = broadcast.consume().track("mixed").unwrap();
	let subscription = Subscription::default()
		.with_range(0..)
		.with_order(Order::Ascending)
		.with_max_delay(None);
	let mut reader = track.subscribe(subscription).await.unwrap();

	archive
		.object("mixed", 0, vec![datagram(false, 3), frames(false, 4, 0..1)])
		.await;
	let datagram = timeout(10 * REFRESH, reader.recv_datagram())
		.await
		.expect("the datagram arrives")
		.unwrap()
		.unwrap();
	assert_eq!((datagram.sequence, &datagram.payload[..]), (3, &b"3/0"[..]));

	// A stored datagram is never fetched as a group.
	assert!(fetch(&broadcast, "mixed", 1).await.is_err());
	assert_eq!(fetch(&broadcast, "mixed", 0).await.unwrap(), ["0/0"]);
	assert_eq!(fetch(&broadcast, "mixed", 2).await.unwrap(), ["2/0"]);

	// The newest stored group arrives live as a group, still open.
	let mut sequences = Vec::new();
	while sequences.last() != Some(&4) {
		let group = timeout(10 * REFRESH, reader.recv_group())
			.await
			.unwrap()
			.unwrap()
			.unwrap();
		sequences.push(group.sequence);
	}
	assert!(!sequences.contains(&1) && !sequences.contains(&3), "{sequences:?}");
}

/// A track removed and re-added under its name replays both runs in sequence.
#[tokio::test(start_paused = true)]
async fn a_re_added_track_replays_both_runs() {
	let store = Store::new(Mock::memory(), "rec");
	let source = broadcast::Info::new().produce();
	let info = track::Info::default().with_timescale(Timescale::MILLI);
	let config = crate::producer::Config::default().with_flush(Duration::from_secs(1));
	let recording = tokio::spawn(crate::Producer::new(store.clone(), source.consume(), config).run());

	for (run, sequences) in [(0, 0..2), (1, 2..4)] {
		let track = source.create_track("video", info.clone()).unwrap();
		while track.subscription().is_none() {
			sleep(Duration::from_millis(1)).await;
		}
		for sequence in sequences {
			let mut group = track.create_group(group::Info { sequence }).unwrap();
			let pts = Timestamp::from_millis(sequence * 1000).unwrap();
			group.write_frame(pts, format!("run{run}/{sequence}")).unwrap();
			group.finish().unwrap();
		}
		track.finish().unwrap();
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
