use std::collections::BTreeMap;

use bytes::Bytes;
use futures::TryStreamExt;
use kio::clock::sleep;
use moq_net::track::Position;
use moq_net::{Timescale, Timestamp, broadcast, group, track};
use object_store::ObjectStoreExt;
use tokio::task::JoinHandle;

use super::*;
use crate::Info;
use crate::layout::{At, Entry, Object};
use crate::mock::Mock;

const SECOND: Duration = Duration::from_secs(1);

fn store() -> Store<Mock> {
	Store::new(Mock::memory(), "rec")
}

/// No flush or gap verdict lands unless a test waits for it.
fn config() -> Config {
	Config::default()
		.with_duration(2 * SECOND)
		.with_flush(100 * SECOND)
		.with_max_delay(10 * SECOND)
}

fn spawn(store: &Store<Mock>, broadcast: &broadcast::Producer, config: Config) -> JoinHandle<Result<()>> {
	tokio::spawn(Producer::new(store.clone(), broadcast.consume(), config).run())
}

fn timed() -> track::Info {
	track::Info::default().with_timescale(Timescale::MILLI)
}

fn ms(ms: u64) -> Timestamp {
	Timestamp::from_millis(ms).unwrap()
}

/// Write a whole group of `(pts, payload)` frames.
fn group(track: &track::Producer, sequence: u64, frames: &[(u64, &str)]) {
	let mut group = track.create_group(group::Info { sequence }).unwrap();
	for (pts, payload) in frames {
		group.write_frame(ms(*pts), payload.to_string()).unwrap();
	}
	group.finish().unwrap();
}

/// Let the producer catch up without moving time far.
async fn settle() {
	for _ in 0..10 {
		sleep(Duration::from_millis(1)).await;
	}
}

/// Wait for the producer to subscribe to `track`.
async fn subscribed(track: &track::Producer) {
	while track.subscription().is_none() {
		sleep(Duration::from_millis(1)).await;
	}
}

async fn finished(task: JoinHandle<Result<()>>) {
	kio::clock::timeout(Duration::from_secs(1000), task)
		.await
		.expect("the producer ends with its broadcast")
		.unwrap()
		.expect("the producer succeeds");
}

/// Every stored key under the prefix, relative to it.
async fn keys(store: &Store<Mock>) -> Vec<String> {
	let mut keys: Vec<String> = store
		.inner()
		.list(Some(store.prefix()))
		.map_ok(|meta| meta.location.as_ref().strip_prefix("rec/").unwrap().to_string())
		.try_collect()
		.await
		.unwrap();
	keys.sort();
	keys
}

async fn get(store: &Store<Mock>, key: &str) -> Option<Bytes> {
	let path = object_store::path::Path::parse(format!("rec/{key}")).unwrap();
	match store.inner().get(&path).await {
		Ok(got) => Some(got.bytes().await.unwrap()),
		Err(object_store::Error::NotFound { .. }) => None,
		Err(err) => panic!("{err}"),
	}
}

/// A track's objects by key name, in key order.
async fn objects(store: &Store<Mock>, track: &str, timed: bool) -> BTreeMap<String, Object> {
	let dir = format!("{}/", crate::path::encode_track(track).unwrap());
	let mut objects = BTreeMap::new();
	for key in keys(store).await {
		let Some(name) = key.strip_prefix(&dir) else {
			continue;
		};
		if name == layout::TRACK_INFO || name == layout::TRACK_END {
			continue;
		}
		let object = Object::decode(get(store, &key).await.unwrap(), timed).unwrap();
		objects.insert(name.to_string(), object);
	}
	objects
}

/// Each object's entries as `(sequence, form, frames)`, where a frame is `pts/payload`.
fn contents(objects: &BTreeMap<String, Object>) -> Vec<Vec<(u64, &'static str, Vec<String>)>> {
	objects
		.values()
		.map(|object| {
			object
				.entries
				.iter()
				.map(|entry| {
					let (form, frames) = match entry {
						Entry::Group { frames, .. } => ("group", frames.as_slice()),
						Entry::Datagram { frame, .. } => ("datagram", std::slice::from_ref(frame)),
					};
					let frames = frames
						.iter()
						.map(|frame| {
							let payload = String::from_utf8(frame.payload.to_vec()).unwrap();
							match frame.timestamp {
								Some(pts) => format!("{pts}/{payload}"),
								None => payload,
							}
						})
						.collect();
					(entry.sequence(), form, frames)
				})
				.collect()
		})
		.collect()
}

fn key(group: u64, frame: u64, pts: u64) -> String {
	format!("{group:019}-{frame:019}-{pts:019}")
}

fn untimed(group: u64, frame: u64) -> String {
	format!("{group:019}-{frame:019}")
}

async fn end(store: &Store<Mock>, track: &str) -> Option<End> {
	let key = format!("{}/end", crate::path::encode_track(track).unwrap());
	get(store, &key).await.map(|bytes| End::decode(&bytes).unwrap())
}

async fn complete(store: &Store<Mock>) -> bool {
	get(store, layout::BROADCAST_END).await.is_some()
}

#[tokio::test(start_paused = true)]
async fn timed_objects_cut_at_group_boundaries_by_duration() {
	let store = store();
	let broadcast = broadcast::Info::new().produce();
	let video = broadcast.create_track("video", timed()).unwrap();
	for sequence in 0..5 {
		let pts = sequence * 1000;
		group(&video, sequence, &[(pts, "a"), (pts + 500, "b")]);
	}
	let task = spawn(&store, &broadcast, config());
	subscribed(&video).await;
	settle().await;
	video.finish().unwrap();
	broadcast.close();
	finished(task).await;

	let objects = objects(&store, "video", true).await;
	assert_eq!(
		objects.keys().cloned().collect::<Vec<_>>(),
		[key(0, 0, 0), key(2, 0, 2000), key(4, 0, 4000)]
	);
	assert_eq!(
		contents(&objects)[0],
		[
			(0, "group", vec!["0/a".to_string(), "500/b".to_string()]),
			(1, "group", vec!["1000/a".to_string(), "1500/b".to_string()]),
		]
	);
	let info = Info::decode(&get(&store, "video/.info").await.unwrap()).unwrap();
	assert_eq!(info.timescale, Some(1000));
	assert_eq!(end(&store, "video").await, Some(End::Finish(At { group: 5, frame: 0 })));
	assert!(complete(&store).await);
}

#[tokio::test(start_paused = true)]
async fn a_lost_group_is_a_gap_that_never_stalls_later_groups() {
	let store = store();
	let broadcast = broadcast::Info::new().produce();
	let video = broadcast.create_track("video", timed()).unwrap();
	let config = config().with_flush(SECOND).with_max_delay(2 * SECOND);
	let task = spawn(&store, &broadcast, config);
	subscribed(&video).await;

	group(&video, 0, &[(0, "a")]);
	group(&video, 2, &[(2000, "c")]);
	group(&video, 3, &[(3000, "d")]);
	settle().await;
	// Group 0 is flushed alone; 2 and 3 wait on the missing 1.
	sleep(SECOND + SECOND / 2).await;
	assert_eq!(
		objects(&store, "video", true).await.keys().cloned().collect::<Vec<_>>(),
		[key(0, 0, 0)]
	);

	// Once the max delay passes, the gap is given up and the rest follow within the flush.
	sleep(2 * SECOND).await;
	let stored = objects(&store, "video", true).await;
	assert_eq!(
		stored.keys().cloned().collect::<Vec<_>>(),
		[key(0, 0, 0), key(2, 0, 2000)]
	);
	assert_eq!(
		contents(&stored)[1],
		[
			(2, "group", vec!["2000/c".to_string()]),
			(3, "group", vec!["3000/d".to_string()]),
		]
	);

	// The lost group arriving too late stays a gap.
	group(&video, 1, &[(1000, "b")]);
	video.finish().unwrap();
	broadcast.close();
	finished(task).await;
	let stored = objects(&store, "video", true).await;
	assert_eq!(stored.len(), 2);
	assert_eq!(end(&store, "video").await, Some(End::Finish(At { group: 4, frame: 0 })));
}

/// Arrival is not sequence order, so a fresh track's first arrival waits for a lower sequence.
#[tokio::test(start_paused = true)]
async fn a_fresh_track_waits_for_a_group_below_its_first_arrival() {
	let store = store();
	let broadcast = broadcast::Info::new().produce();
	let video = broadcast.create_track("video", timed()).unwrap();
	let config = config().with_flush(SECOND).with_max_delay(2 * SECOND);
	let task = spawn(&store, &broadcast, config);
	subscribed(&video).await;

	group(&video, 2, &[(2000, "c")]);
	settle().await;
	group(&video, 1, &[(1000, "b")]);
	settle().await;
	// Nothing is stored until the wait from the first arrival gives up on group 0.
	sleep(SECOND + SECOND / 2).await;
	assert!(objects(&store, "video", true).await.is_empty());

	sleep(2 * SECOND).await;
	let stored = objects(&store, "video", true).await;
	assert_eq!(stored.keys().cloned().collect::<Vec<_>>(), [key(1, 0, 1000)]);
	assert_eq!(
		contents(&stored)[0],
		[
			(1, "group", vec!["1000/b".to_string()]),
			(2, "group", vec!["2000/c".to_string()]),
		]
	);

	// Group 0 arriving after that is too late.
	group(&video, 0, &[(0, "a")]);
	video.finish().unwrap();
	broadcast.close();
	finished(task).await;
	assert_eq!(objects(&store, "video", true).await.len(), 1);
}

#[tokio::test(start_paused = true)]
async fn an_oversized_timed_group_splits_into_ordered_unique_keys() {
	let store = store();
	let broadcast = broadcast::Info::new().produce();
	let video = broadcast.create_track("video", timed()).unwrap();
	// Timestamps repeat and go backward, as decode order allows.
	let frames = [
		(0, "0"),
		(1000, "1"),
		(2000, "2"),
		(2000, "3"),
		(1500, "4"),
		(4000, "5"),
		(4000, "6"),
	];
	group(&video, 0, &frames);
	let task = spawn(&store, &broadcast, config());
	subscribed(&video).await;
	settle().await;
	video.finish().unwrap();
	broadcast.close();
	finished(task).await;

	let objects = objects(&store, "video", true).await;
	assert_eq!(
		objects.keys().cloned().collect::<Vec<_>>(),
		[key(0, 0, 0), key(0, 2, 2000), key(0, 5, 4000)]
	);
	let stored: Vec<String> = contents(&objects)
		.into_iter()
		.flatten()
		.flat_map(|(_, _, frames)| frames)
		.collect();
	let expected: Vec<String> = frames.iter().map(|(pts, payload)| format!("{pts}/{payload}")).collect();
	assert_eq!(stored, expected);
	assert_eq!(end(&store, "video").await, Some(End::Finish(At { group: 1, frame: 0 })));
}

/// An open untimed group is cut at each flush, so it never holds more than a flush of frames in
/// memory, and a stored object is never rewritten.
#[tokio::test(start_paused = true)]
async fn an_open_untimed_group_is_cut_at_each_flush() {
	let store = store();
	let broadcast = broadcast::Info::new().produce();
	let data = broadcast.create_track("data", track::Info::default()).unwrap();
	let task = spawn(&store, &broadcast, config().with_flush(SECOND));
	subscribed(&data).await;

	let mut open = data.create_group(group::Info { sequence: 0 }).unwrap();
	open.write_frame(None, "a").unwrap();
	settle().await;
	sleep(SECOND).await;
	let head = untimed(0, 0);
	let first = get(&store, &format!("data/{head}")).await.unwrap();

	open.write_frame(None, "b").unwrap();
	settle().await;
	sleep(SECOND).await;
	assert_eq!(get(&store, &format!("data/{head}")).await.unwrap(), first);
	let stored = objects(&store, "data", false).await;
	assert_eq!(stored.keys().cloned().collect::<Vec<_>>(), [head, untimed(0, 1)]);
	assert_eq!(
		contents(&stored),
		[
			[(0, "group", vec!["a".to_string()])],
			[(0, "group", vec!["b".to_string()])]
		]
	);

	open.finish().unwrap();
	data.finish().unwrap();
	broadcast.close();
	finished(task).await;
	assert_eq!(end(&store, "data").await, Some(End::Finish(At { group: 1, frame: 0 })));
	let info = Info::decode(&get(&store, "data/.info").await.unwrap()).unwrap();
	assert_eq!(info.timescale, None);
}

/// Without a flush due, an untimed group splits once its own run in an object lasts the duration
/// of wall clock, and the next group starts the next object past the duration.
#[tokio::test(start_paused = true)]
async fn an_untimed_group_splits_by_wall_clock() {
	let store = store();
	let broadcast = broadcast::Info::new().produce();
	let data = broadcast.create_track("data", track::Info::default()).unwrap();
	let task = spawn(&store, &broadcast, config());
	subscribed(&data).await;

	let mut open = data.create_group(group::Info { sequence: 0 }).unwrap();
	for payload in ["a", "b", "c", "d", "e"] {
		open.write_frame(None, payload).unwrap();
		settle().await;
		sleep(SECOND / 2).await;
	}
	open.finish().unwrap();
	settle().await;
	sleep(2 * SECOND).await;
	let mut next = data.create_group(group::Info { sequence: 1 }).unwrap();
	next.write_frame(None, "f").unwrap();
	next.finish().unwrap();
	settle().await;
	data.finish().unwrap();
	broadcast.close();
	finished(task).await;

	let stored = objects(&store, "data", false).await;
	assert_eq!(
		stored.keys().cloned().collect::<Vec<_>>(),
		[untimed(0, 0), untimed(0, 4), untimed(1, 0)]
	);
	let strings = |frames: &[&str]| frames.iter().map(|frame| frame.to_string()).collect::<Vec<_>>();
	assert_eq!(
		contents(&stored),
		[
			vec![(0, "group", strings(&["a", "b", "c", "d"]))],
			vec![(0, "group", strings(&["e"]))],
			vec![(1, "group", strings(&["f"]))],
		]
	);
}

#[tokio::test(start_paused = true)]
async fn an_idle_track_is_flushed_within_the_bound() {
	let store = store();
	let broadcast = broadcast::Info::new().produce();
	let video = broadcast.create_track("video", timed()).unwrap();
	let task = spawn(&store, &broadcast, config().with_flush(SECOND));
	subscribed(&video).await;

	let mut open = video.create_group(group::Info { sequence: 0 }).unwrap();
	open.write_frame(ms(0), "a").unwrap();
	settle().await;
	sleep(SECOND / 2).await;
	assert!(objects(&store, "video", true).await.is_empty());
	sleep(SECOND / 2).await;
	assert_eq!(
		objects(&store, "video", true).await.keys().cloned().collect::<Vec<_>>(),
		[key(0, 0, 0)]
	);

	// The open group continues in the next object.
	open.write_frame(ms(100), "b").unwrap();
	open.finish().unwrap();
	settle().await;
	video.finish().unwrap();
	broadcast.close();
	finished(task).await;
	assert_eq!(
		objects(&store, "video", true).await.keys().cloned().collect::<Vec<_>>(),
		[key(0, 0, 0), key(0, 1, 100)]
	);
}

#[tokio::test(start_paused = true)]
async fn a_restart_mid_group_loses_and_duplicates_nothing() {
	let store = store();
	let broadcast = broadcast::Info::new().produce();
	let video = broadcast.create_track("video", timed()).unwrap();
	let data = broadcast.create_track("data", track::Info::default()).unwrap();
	let config = config().with_flush(SECOND);
	let task = spawn(&store, &broadcast, config.clone());
	subscribed(&video).await;
	subscribed(&data).await;

	let mut timed_group = video.create_group(group::Info { sequence: 0 }).unwrap();
	let mut untimed_group = data.create_group(group::Info { sequence: 0 }).unwrap();
	for (pts, payload) in [(0, "a"), (100, "b")] {
		timed_group.write_frame(ms(pts), payload).unwrap();
		untimed_group.write_frame(None, payload).unwrap();
	}
	settle().await;
	sleep(SECOND).await;
	task.abort();
	assert!(task.await.unwrap_err().is_cancelled());

	// Both groups grow while nothing records them.
	timed_group.write_frame(ms(200), "c").unwrap();
	untimed_group.write_frame(None, "c").unwrap();
	let task = spawn(&store, &broadcast, config);
	settle().await;
	timed_group.write_frame(ms(300), "d").unwrap();
	untimed_group.write_frame(None, "d").unwrap();
	timed_group.finish().unwrap();
	untimed_group.finish().unwrap();
	settle().await;
	video.finish().unwrap();
	data.finish().unwrap();
	broadcast.close();
	finished(task).await;

	let stored = objects(&store, "video", true).await;
	assert_eq!(
		stored.keys().cloned().collect::<Vec<_>>(),
		[key(0, 0, 0), key(0, 2, 200)]
	);
	let frames: Vec<String> = contents(&stored)
		.into_iter()
		.flatten()
		.flat_map(|(_, _, frames)| frames)
		.collect();
	assert_eq!(frames, ["0/a", "100/b", "200/c", "300/d"]);

	// The untimed group continues in its own object too.
	let stored = objects(&store, "data", false).await;
	assert_eq!(
		stored.keys().cloned().collect::<Vec<_>>(),
		[untimed(0, 0), untimed(0, 2)]
	);
	let frames: Vec<String> = contents(&stored)
		.into_iter()
		.flatten()
		.flat_map(|(_, _, frames)| frames)
		.collect();
	assert_eq!(frames, ["a", "b", "c", "d"]);
	assert!(complete(&store).await);
}

/// A later group that starts past frame 0, its head held elsewhere, opens its own object: an
/// entry after the first has no frame start, so a resume would otherwise rewind into it.
#[tokio::test(start_paused = true)]
async fn a_partial_group_resumes_after_its_last_stored_frame() {
	let store = store();
	let broadcast = broadcast::Info::new().produce();
	let video = broadcast.create_track("video", timed()).unwrap();
	let config = config().with_flush(SECOND);
	let task = spawn(&store, &broadcast, config.clone());
	subscribed(&video).await;

	group(&video, 0, &[(0, "a")]);
	let mut partial = video.create_group(group::Info { sequence: 1 }).unwrap();
	partial.start_at(2).unwrap();
	partial.write_frame(ms(500), "c").unwrap();
	settle().await;
	sleep(SECOND).await;
	task.abort();
	assert!(task.await.unwrap_err().is_cancelled());

	let task = spawn(&store, &broadcast, config);
	settle().await;
	partial.write_frame(ms(600), "d").unwrap();
	partial.finish().unwrap();
	settle().await;
	video.finish().unwrap();
	broadcast.close();
	finished(task).await;

	let stored = objects(&store, "video", true).await;
	assert_eq!(
		stored.keys().cloned().collect::<Vec<_>>(),
		[key(0, 0, 0), key(1, 2, 500), key(1, 3, 600)]
	);
	let frames: Vec<String> = contents(&stored)
		.into_iter()
		.flatten()
		.flat_map(|(_, _, frames)| frames)
		.collect();
	assert_eq!(frames, ["0/a", "500/c", "600/d"]);
	assert_eq!(end(&store, "video").await, Some(End::Finish(At { group: 2, frame: 0 })));
}

#[tokio::test(start_paused = true)]
async fn changed_properties_fail_the_resume() {
	let store = store();
	let broadcast = broadcast::Info::new().produce();
	let video = broadcast.create_track("video", timed()).unwrap();
	group(&video, 0, &[(0, "a")]);
	let task = spawn(&store, &broadcast, config());
	subscribed(&video).await;
	video.finish().unwrap();
	drop(video);
	broadcast.close();
	finished(task).await;

	// Another instance, recorded under the same prefix, which only a missing `.end` allows.
	let store = Store::new(store.inner().fork(), "rec");
	store
		.inner()
		.delete(&object_store::path::Path::from("rec/.end"))
		.await
		.unwrap();
	let broadcast = broadcast::Info::new().produce();
	let _video = broadcast
		.create_track("video", track::Info::default().with_timescale(Timescale::MICRO))
		.unwrap();
	let err = spawn(&store, &broadcast, config()).await.unwrap().unwrap_err();
	assert!(
		matches!(
			err,
			Error::TimescaleMismatch {
				existing: Some(1000),
				intended: Some(1_000_000)
			}
		),
		"{err}"
	);
}

/// A reader still reads a version 2 `.info`, but a producer never appends to one.
#[tokio::test(start_paused = true)]
async fn an_older_version_fails_the_resume() {
	let store = store();
	let info = object_store::path::Path::from("rec/video/.info");
	let v2 = br#"{"version":2,"priority":128,"timescale":1000}"#;
	store.inner().put(&info, v2.as_ref().into()).await.unwrap();
	let broadcast = broadcast::Info::new().produce();
	let _video = broadcast.create_track("video", timed()).unwrap();
	let err = spawn(&store, &broadcast, config()).await.unwrap().unwrap_err();
	assert!(matches!(err, Error::Version(2)), "{err}");
	assert_eq!(get(&store, "video/.info").await.as_deref(), Some(&v2[..]));
}

#[tokio::test(start_paused = true)]
async fn track_names_stay_one_segment_through_a_resume() {
	let store = store();
	let broadcast = broadcast::Info::new().produce();
	let names = ["a/b", ".hidden", "end"];
	let tracks: Vec<_> = names
		.iter()
		.map(|name| broadcast.create_track(*name, track::Info::default()).unwrap())
		.collect();
	let task = spawn(&store, &broadcast, config().with_flush(SECOND));
	for track in &tracks {
		subscribed(track).await;
		let mut group = track.create_group(group::Info { sequence: 0 }).unwrap();
		group.write_frame(None, "x").unwrap();
		group.finish().unwrap();
	}
	settle().await;
	sleep(SECOND).await;
	task.abort();
	let _ = task.await;

	let stored = keys(&store).await;
	for dir in ["a%2Fb", "%2Ehidden", "end"] {
		assert!(stored.contains(&format!("{dir}/{}", untimed(0, 0))), "{stored:?}");
		assert!(stored.contains(&format!("{dir}/.info")), "{stored:?}");
	}

	// The resume parses each one back to its name and continues after its stored group.
	let task = spawn(&store, &broadcast, config());
	for track in &tracks {
		subscribed(track).await;
		track.finish().unwrap();
	}
	broadcast.close();
	finished(task).await;
	for name in names {
		assert_eq!(end(&store, name).await, Some(End::Finish(At { group: 1, frame: 0 })));
		assert_eq!(objects(&store, name, false).await.len(), 1);
	}
}

#[tokio::test(start_paused = true)]
async fn a_dvr_prunes_superseded_objects_after_its_window_and_grace() {
	let store = store();
	let broadcast = broadcast::Info::new().produce();
	let video = broadcast.create_track("video", timed()).unwrap();
	let dvr = Dvr::new(2 * SECOND, SECOND);
	let task = spawn(&store, &broadcast, config().with_duration(SECOND).with_dvr(dvr));
	subscribed(&video).await;

	// Each group is its own object, closed when the next group starts.
	for sequence in 0..4 {
		group(&video, sequence, &[(sequence * 1000, "x")]);
		settle().await;
		sleep(SECOND).await;
	}
	// Object 0 was superseded when object 1 was stored, at 2s: it stays through 5s.
	let names =
		|objects: BTreeMap<String, Object>| objects.values().map(|o| o.start().unwrap().group).collect::<Vec<_>>();
	assert_eq!(names(objects(&store, "video", true).await), [0, 1, 2]);
	sleep(SECOND).await;
	settle().await;
	assert_eq!(names(objects(&store, "video", true).await), [1, 2]);
	sleep(3 * SECOND).await;
	settle().await;
	// The newest stored object is always kept.
	assert_eq!(names(objects(&store, "video", true).await), [2]);

	video.finish().unwrap();
	broadcast.close();
	finished(task).await;
	assert_eq!(names(objects(&store, "video", true).await), [2, 3]);
	assert!(end(&store, "video").await.is_some());
}

#[tokio::test(start_paused = true)]
async fn a_track_added_mid_recording_is_recorded() {
	let store = store();
	let broadcast = broadcast::Info::new().produce();
	let video = broadcast.create_track("video", timed()).unwrap();
	let task = spawn(&store, &broadcast, config());
	subscribed(&video).await;
	group(&video, 0, &[(0, "v")]);
	settle().await;

	let audio = broadcast.create_track("audio", timed()).unwrap();
	subscribed(&audio).await;
	group(&audio, 0, &[(0, "a")]);
	settle().await;
	video.finish().unwrap();
	audio.finish().unwrap();
	broadcast.close();
	finished(task).await;

	assert_eq!(objects(&store, "audio", true).await.len(), 1);
	assert_eq!(objects(&store, "video", true).await.len(), 1);
	assert!(end(&store, "audio").await.is_some());
	assert!(complete(&store).await);
}

#[tokio::test(start_paused = true)]
async fn groups_and_datagrams_keep_their_form_through_an_abrupt_end() {
	let store = store();
	let broadcast = broadcast::Info::new().produce();
	let mut track = broadcast.create_track("mixed", timed()).unwrap();
	let task = spawn(&store, &broadcast, config());
	subscribed(&track).await;

	group(&track, 0, &[(0, "g0")]);
	track.insert_datagram(1, ms(100), "d1").unwrap();
	group(&track, 2, &[(200, "g2")]);
	track.insert_datagram(3, ms(300), "d3").unwrap();
	track.insert_datagram(4, ms(400), "d4").unwrap();
	// Ends at once: every received datagram is stored before the end.
	track.finish().unwrap();
	broadcast.close();
	finished(task).await;

	let stored = objects(&store, "mixed", true).await;
	assert_eq!(
		contents(&stored),
		[[
			(0, "group", vec!["0/g0".to_string()]),
			(1, "datagram", vec!["100/d1".to_string()]),
			(2, "group", vec!["200/g2".to_string()]),
			(3, "datagram", vec!["300/d3".to_string()]),
			(4, "datagram", vec!["400/d4".to_string()]),
		]]
	);
	assert_eq!(end(&store, "mixed").await, Some(End::Finish(At { group: 5, frame: 0 })));
}

#[tokio::test(start_paused = true)]
async fn markers_wait_for_the_listing_to_end() {
	let store = store();
	let broadcast = broadcast::Info::new().produce();
	let mut video = broadcast.create_track("video", timed()).unwrap();
	let audio = broadcast.create_track("audio", timed()).unwrap();
	let task = spawn(&store, &broadcast, config());
	subscribed(&video).await;
	subscribed(&audio).await;

	group(&video, 0, &[(0, "a")]);
	let mut last = video.create_group(group::Info { sequence: 1 }).unwrap();
	last.write_frame(ms(1000), "b").unwrap();
	last.write_frame(ms(1100), "c").unwrap();
	// A mid-group end: frames 0 and 1 of group 1.
	video.finish_at(Position { group: 1, frame: 2 }, None).unwrap();
	last.finish().unwrap();
	settle().await;
	sleep(SECOND).await;
	// Ended, but a track re-added under the name would continue it.
	assert_eq!(end(&store, "video").await, None);
	assert!(!complete(&store).await);

	audio.abort(moq_net::Error::App(7)).unwrap();
	settle().await;
	assert_eq!(end(&store, "audio").await, None);

	broadcast.close();
	finished(task).await;
	assert_eq!(end(&store, "video").await, Some(End::Finish(At { group: 1, frame: 2 })));
	// An error end is recorded, and the broadcast still completes.
	assert_eq!(end(&store, "audio").await, Some(End::Error(64 + 7)));
	assert!(complete(&store).await);
}

/// A track that ends with an error records that error, though every group below the end stays.
#[tokio::test(start_paused = true)]
async fn an_errored_end_records_its_error() {
	let store = store();
	let broadcast = broadcast::Info::new().produce();
	let mut video = broadcast.create_track("video", timed()).unwrap();
	let task = spawn(&store, &broadcast, config());
	subscribed(&video).await;
	group(&video, 0, &[(0, "a")]);
	group(&video, 1, &[(1000, "b")]);
	video
		.finish_at(Position::group(2), Some(moq_net::Error::App(7)))
		.unwrap();
	settle().await;
	broadcast.close();
	finished(task).await;

	let frames: Vec<String> = contents(&objects(&store, "video", true).await)
		.into_iter()
		.flatten()
		.flat_map(|(_, _, frames)| frames)
		.collect();
	assert_eq!(frames, ["0/a", "1000/b"]);
	assert_eq!(end(&store, "video").await, Some(End::Error(64 + 7)));
}

#[tokio::test(start_paused = true)]
async fn a_re_added_track_continues_and_its_marker_records_the_last_run() {
	let store = store();
	let broadcast = broadcast::Info::new().produce();
	let first = broadcast.create_track("video", timed()).unwrap();
	let task = spawn(&store, &broadcast, config());
	subscribed(&first).await;
	group(&first, 0, &[(0, "a")]);
	group(&first, 1, &[(1000, "b")]);
	first.finish().unwrap();
	settle().await;
	drop(first);
	settle().await;

	let second = broadcast.create_track("video", timed()).unwrap();
	subscribed(&second).await;
	group(&second, 2, &[(2000, "c")]);
	settle().await;
	second.abort(moq_net::Error::App(3)).unwrap();
	settle().await;
	broadcast.close();
	finished(task).await;

	let frames: Vec<String> = contents(&objects(&store, "video", true).await)
		.into_iter()
		.flatten()
		.flat_map(|(_, _, frames)| frames)
		.collect();
	assert_eq!(frames, ["0/a", "1000/b", "2000/c"]);
	assert_eq!(end(&store, "video").await, Some(End::Error(64 + 3)));
}

/// A track re-added with other properties after it was listed is refused before any of its
/// frames is stored under the listed properties.
#[tokio::test(start_paused = true)]
async fn a_re_added_track_is_stored_under_its_own_properties() {
	let store = store();
	let broadcast = broadcast::Info::new().produce();
	let first = broadcast.create_track("video", timed().with_priority(1)).unwrap();
	store.inner().hold_puts("video/.info");
	let task = spawn(&store, &broadcast, config().with_flush(SECOND));
	settle().await;
	drop(first);
	settle().await;

	let second = broadcast.create_track("video", timed().with_priority(2)).unwrap();
	group(&second, 0, &[(0, "x")]);
	store.inner().release();
	settle().await;
	sleep(SECOND).await;
	settle().await;
	assert!(objects(&store, "video", true).await.is_empty());

	second.finish().unwrap();
	broadcast.close();
	let err = kio::clock::timeout(Duration::from_secs(1000), task)
		.await
		.unwrap()
		.unwrap()
		.unwrap_err();
	assert!(
		matches!(
			err,
			Error::Priority {
				existing: 1,
				intended: 2
			}
		),
		"{err}"
	);
	assert!(objects(&store, "video", true).await.is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_stopped_producer_writes_no_marker() {
	let store = store();
	let broadcast = broadcast::Info::new().produce();
	let video = broadcast.create_track("video", timed()).unwrap();
	let task = spawn(&store, &broadcast, config().with_flush(SECOND));
	subscribed(&video).await;
	group(&video, 0, &[(0, "a")]);
	settle().await;
	sleep(SECOND).await;
	task.abort();
	let _ = task.await;
	video.finish().unwrap();
	broadcast.close();

	assert_eq!(objects(&store, "video", true).await.len(), 1);
	assert_eq!(end(&store, "video").await, None);
	assert!(!complete(&store).await);
}

#[tokio::test(start_paused = true)]
async fn a_completed_broadcast_records_nothing() {
	let store = store();
	store
		.inner()
		.put(&object_store::path::Path::from("rec/.end"), Default::default())
		.await
		.unwrap();
	let broadcast = broadcast::Info::new().produce();
	let video = broadcast.create_track("video", timed()).unwrap();
	group(&video, 0, &[(0, "a")]);
	finished(spawn(&store, &broadcast, config())).await;
	assert_eq!(keys(&store).await, [".end"]);
	assert!(video.subscription().is_none());
}

#[tokio::test(start_paused = true)]
async fn a_track_that_ended_unseen_leaves_the_broadcast_open() {
	let store = store();
	let broadcast = broadcast::Info::new().produce();
	let video = broadcast.create_track("video", timed()).unwrap();
	let audio = broadcast.create_track("audio", timed()).unwrap();
	let task = spawn(&store, &broadcast, config().with_flush(SECOND));
	subscribed(&video).await;
	subscribed(&audio).await;
	group(&audio, 0, &[(0, "a")]);
	settle().await;
	sleep(SECOND).await;
	task.abort();
	let _ = task.await;

	// Audio ends while nothing records, so the resume never sees how.
	audio.finish().unwrap();
	drop(audio);
	let task = spawn(&store, &broadcast, config());
	subscribed(&video).await;
	video.finish().unwrap();
	broadcast.close();
	finished(task).await;
	assert!(end(&store, "video").await.is_some());
	assert_eq!(end(&store, "audio").await, None);
	assert!(!complete(&store).await);
}

#[tokio::test(start_paused = true)]
async fn a_failed_put_fails_the_recording() {
	let store = store();
	store.inner().fail_puts("video/0");
	let broadcast = broadcast::Info::new().produce();
	let video = broadcast.create_track("video", timed()).unwrap();
	group(&video, 0, &[(0, "a")]);
	let task = spawn(&store, &broadcast, config().with_flush(SECOND));
	let err = task.await.unwrap().unwrap_err();
	assert!(matches!(err, Error::Store(_)), "{err}");
	assert_eq!(keys(&store).await, ["video/.info"]);
}

#[tokio::test(start_paused = true)]
async fn a_failed_resume_listing_writes_nothing() {
	let store = store();
	store
		.put_info("video", &Info::new(0, Some(1000)).unwrap())
		.await
		.unwrap();
	store.inner().fail_lists();
	let broadcast = broadcast::Info::new().produce();
	let _video = broadcast.create_track("video", timed()).unwrap();
	store.inner().take();
	let err = spawn(&store, &broadcast, config()).await.unwrap().unwrap_err();
	assert!(matches!(err, Error::Store(_)), "{err}");
	let ops = store.inner().take();
	assert!(!ops.iter().any(|op| matches!(op, crate::mock::Op::Put(_))), "{ops:?}");
}

#[tokio::test(start_paused = true)]
async fn a_failed_prune_fails_the_dvr() {
	let store = store();
	store.inner().fail_deletes("video/");
	let broadcast = broadcast::Info::new().produce();
	let video = broadcast.create_track("video", timed()).unwrap();
	let dvr = Dvr::new(SECOND, Duration::ZERO);
	let task = spawn(&store, &broadcast, config().with_duration(SECOND).with_dvr(dvr));
	subscribed(&video).await;
	for sequence in 0..3 {
		group(&video, sequence, &[(sequence * 1000, "x")]);
	}
	settle().await;
	sleep(2 * SECOND).await;
	let err = task.await.unwrap().unwrap_err();
	assert!(matches!(err, Error::Store(_)), "{err}");
}

#[tokio::test(start_paused = true)]
async fn a_stalled_put_holds_only_its_track() {
	let store = store();
	store.inner().hold_puts("video/0");
	let broadcast = broadcast::Info::new().produce();
	let video = broadcast.create_track("video", timed()).unwrap();
	let audio = broadcast.create_track("audio", timed()).unwrap();
	let task = spawn(&store, &broadcast, config().with_flush(SECOND));
	subscribed(&video).await;
	subscribed(&audio).await;
	group(&video, 0, &[(0, "v")]);
	group(&audio, 0, &[(0, "a")]);
	settle().await;
	sleep(SECOND).await;
	settle().await;
	assert_eq!(objects(&store, "audio", true).await.len(), 1);
	assert!(objects(&store, "video", true).await.is_empty());

	store.inner().release();
	video.finish().unwrap();
	audio.finish().unwrap();
	broadcast.close();
	finished(task).await;
	assert_eq!(objects(&store, "video", true).await.len(), 1);
	assert!(complete(&store).await);
}
