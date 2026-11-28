//! Tests for [`Release`](super::Release).
//!
//! Each imports a hardware encoder's capture or a [fixture](super::fixture) with [`Passthrough`]
//! and releases the track it publishes, either replayed whole or republished object by object on
//! a paused clock, and grades the output with the [oracle](super::oracle).

use std::time::Duration;

use crate::container::test_util::Lazy;

use bytes::Bytes;
use kio::clock::Instant;

use super::oracle::{Admitted, Loss, Object};
use super::{Ext, Passthrough, PassthroughTrack, Release, fixture};
use crate::catalog::CatalogFormat;
use crate::container::Frame;

const KYRION: &[u8] = include_bytes!("test_data/kyrion_mpeg2av_ac3.ts");
const KYRION_H264: &[u8] = include_bytes!("test_data/scte35/kyrion_dirtystart.ts");

/// Longer than either capture, so a replay reads every group.
const RECORDING: Duration = Duration::from_secs(30);
/// How long a drain waits for the next object before taking the release to have stalled.
const STALL: Duration = RECORDING.saturating_mul(2);

/// How far off its PCR time a release may write an object.
const TOLERANCE: Duration = Duration::from_millis(1);

/// A capture imported whole, with what keeps its broadcast up.
struct Imported {
	_passthrough: crate::container::ts::passthrough::Finished,
	_catalog: crate::catalog::Producer<Ext>,
	_broadcast: moq_net::broadcast::Producer,
	consumer: moq_net::broadcast::Consumer,
	objects: Vec<Object>,
}

/// A catalog carrying the `mpegts` section a passthrough lists its track in.
fn config() -> crate::catalog::Config<Ext> {
	crate::catalog::Config::default().with_catalog(hang::catalog::Catalog::<Ext>::default())
}

async fn import(data: &[u8]) -> Imported {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let catalog = crate::catalog::Producer::new(&mut broadcast, config()).unwrap();
	let consumer = broadcast.consume();
	let mut passthrough = Passthrough::new(broadcast.clone(), catalog.reserve()).unwrap();
	let name = passthrough.track().to_string();
	passthrough.decode(data).unwrap();
	let passthrough = passthrough.finish().unwrap();

	let mut track = consumer
		.track(&name)
		.unwrap()
		.subscribe(
			moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
				moq_net::track::Start::Live {
					min: Some(RECORDING),
					max: Some(RECORDING),
				},
			)]),
		)
		.await
		.unwrap()
		.ordered();
	let mut objects = Vec::new();
	while let Some(mut group) = kio::clock::timeout(Duration::from_secs(1), track.next_group())
		.await
		.unwrap()
		.unwrap()
	{
		while let Some(frame) = group.read_frame().await.unwrap() {
			if frame.payload.is_empty() {
				let micros = objects.last().map_or(0, |object: &Object| object.micros);
				objects.push(Object {
					group: group.start,
					micros,
					payload: Bytes::new(),
				});
				continue;
			}
			let frame = hang::container::Frame::decode(frame.payload).unwrap();
			objects.push(Object {
				group: group.start,
				micros: frame.timestamp.as_micros().unwrap() as u64,
				payload: frame.payload,
			});
		}
	}
	// The importer appends each group, so nothing lies below its first or between two.
	for run in skips(&objects) {
		assert!(track.is_skipped(run.clone()), "the importer skips {run:?}");
	}
	Imported {
		_passthrough: passthrough,
		_catalog: catalog,
		_broadcast: broadcast,
		consumer,
		objects,
	}
}

/// The group starts the importer skipped: those below its first group's and between each two,
/// which no group holds.
fn skips(objects: &[Object]) -> Vec<std::ops::Range<u64>> {
	let mut starts: Vec<u64> = objects.iter().map(|object| object.group).collect();
	starts.dedup();
	let first = starts.first().map(|first| 0..*first);
	first
		.into_iter()
		.chain(starts.windows(2).map(|pair| pair[0] + 1..pair[1]))
		.collect()
}

/// Republish `objects` on a new broadcast's passthrough track, in the groups they were published in,
/// each once `at` says it is due after the start and not before the one ahead of it, as a relay
/// delivering them would, skipping the starts between them as the importer did.
fn republish(objects: Vec<Object>, at: impl Fn(usize, &Object) -> Duration + Send + 'static) -> Republished {
	let skips = skips(&objects);
	republish_cut(objects, skips, &[], at)
}

/// [`republish`], skipping `skips`, and aborting each group in `cut` once its last object in
/// `objects` is out, as a relay that evicts a group partway does. Each run of `skips` goes out
/// with the first group past it, as a relay forwarding the publisher's skips would: a group the
/// relay never delivers is not skipped, so a reader sees it missing.
fn republish_cut(
	objects: Vec<Object>,
	skips: Vec<std::ops::Range<u64>>,
	cut: &[u64],
	at: impl Fn(usize, &Object) -> Duration + Send + 'static,
) -> Republished {
	let broadcast = moq_net::broadcast::Info::new().produce();
	publish(broadcast, (), objects, skips, cut, at, Ending::Finish)
}

/// How a republished track ends once its objects are out.
enum Ending {
	/// Finished, its broadcast staying up.
	Finish,
	/// Silent, as a killed publisher whose route lingers until its session times out.
	Silent,
	/// Gone with this error, its route withdrawn, as a relay's lost upstream.
	Abort(moq_net::Error),
}

/// [`republish`] onto the route `options` names at `live` on `origin`, ending as `ending` says.
fn republish_route(
	origin: &moq_net::origin::Producer,
	options: moq_net::origin::RouteOptions,
	objects: Vec<Object>,
	at: impl Fn(usize, &Object) -> Duration + Send + 'static,
	ending: Ending,
) -> Republished {
	let route = origin.route("live", options).unwrap();
	let skips = skips(&objects);
	publish(route.broadcast().clone(), route, objects, skips, &[], at, ending)
}

/// [`republish_cut`] onto `broadcast`, holding `keep` until the track ends as `ending` says.
fn publish(
	mut broadcast: moq_net::broadcast::Producer,
	keep: impl Send + 'static,
	objects: Vec<Object>,
	skips: Vec<std::ops::Range<u64>>,
	cut: &[u64],
	at: impl Fn(usize, &Object) -> Duration + Send + 'static,
	ending: Ending,
) -> Republished {
	let cut = cut.to_vec();
	let mut catalog = crate::catalog::Producer::new(&mut broadcast, config()).unwrap();
	let track = broadcast.unique_track(".m2ts", catalog.track_info(hang::catalog::PRIORITY.video));
	let name = track.name().to_string();
	catalog
		.mutate(
			|catalog| catalog.ext.mpegts.passthrough = Some(PassthroughTrack::new(name)),
			catalog.next_timestamp(),
		)
		.unwrap();
	let consumer = broadcast.consume();
	let start = Instant::now();
	let task = tokio::spawn(async move {
		let _keep = (&broadcast, &keep);
		let mut group: Option<(u64, Lazy)> = None;
		let mut skips = skips.into_iter().peekable();
		let mut due = Duration::ZERO;
		for (i, object) in objects.into_iter().enumerate() {
			due = due.max(at(i, &object));
			kio::clock::sleep_until(start.saturating_add(due)).await;
			if group.as_ref().map(|(sequence, _)| *sequence) != Some(object.group) {
				if let Some((sequence, group)) = group.take() {
					end(sequence, group, &cut);
				}
				while let Some(run) = skips.next_if(|run| run.end <= object.group) {
					track.skip_groups(run);
				}
				group = Some((object.group, Lazy::new(&track)));
			}
			let timestamp = moq_net::Timestamp::from_micros(object.micros).unwrap();
			let group = &mut group.as_mut().unwrap().1;
			if object.payload.is_empty() {
				group.write_frame(timestamp, Bytes::new()).unwrap();
				continue;
			}
			let mut buf = bytes::BytesMut::new();
			hang::container::Frame {
				timestamp,
				payload: object.payload,
			}
			.encode(&mut buf)
			.unwrap();
			// The prefix keeps the presentation time, and the transport the newest so far.
			let mut writer = group.writer();
			let transport = writer.latest().filter(|latest| timestamp < *latest).unwrap_or(timestamp);
			writer.write_frame(transport, buf.freeze()).unwrap();
			// The importer keyed each group by its first frame, which recreates it at the same start.
			assert_eq!(group.start, object.group);
		}
		if let Some((sequence, group)) = group {
			end(sequence, group, &cut);
		}
		match ending {
			Ending::Finish => {
				let _finished = track.finish().unwrap();
				std::future::pending::<()>().await
			}
			// Held unfinished, so its route lingers.
			Ending::Silent => {
				let _track = track;
				std::future::pending::<()>().await
			}
			Ending::Abort(err) => {
				track.abort(err.clone()).unwrap();
				catalog.abort(err);
			}
		}
	});
	Republished { consumer, task }
}

/// Finish `group`, or abort it as evicted if it is one of the `cut`.
fn end(sequence: u64, group: Lazy, cut: &[u64]) {
	match cut.contains(&sequence) {
		true => group.abort(moq_net::Error::Old).unwrap(),
		false => {
			group.finish().unwrap();
		}
	}
}

struct Republished {
	consumer: moq_net::broadcast::Consumer,
	task: tokio::task::JoinHandle<()>,
}

impl Drop for Republished {
	fn drop(&mut self) {
		self.task.abort();
	}
}

/// Every object `release` writes, and when, until it ends or stalls.
async fn drain(mut release: Release) -> (Vec<(Instant, Frame)>, Release) {
	let mut released = Vec::new();
	while let Ok(next) = kio::clock::timeout(STALL, release.next()).await {
		let Some(frame) = next.expect("release error") else {
			break;
		};
		released.push((Instant::now(), frame));
	}
	(released, release)
}

/// Drain a release of `republished` with a 500 ms delay, in a task of its own so that no other
/// release waking polls it.
fn spawn_release(republished: &Republished) -> tokio::task::JoinHandle<(Vec<(Instant, Frame)>, Release)> {
	let consumer = republished.consumer.clone();
	tokio::spawn(async move {
		let source = crate::source::announced(&consumer).await;
		let release = Release::new(source, CatalogFormat::Hang, Duration::from_millis(500))
			.await
			.unwrap();
		drain(release).await
	})
}

fn bytes<'a>(payloads: impl IntoIterator<Item = &'a Bytes>) -> Vec<u8> {
	payloads
		.into_iter()
		.flat_map(|payload| payload.iter().copied())
		.collect()
}

/// Every byte `released` wrote, in order.
fn payloads(released: &[(Instant, Frame)]) -> Vec<u8> {
	bytes(released.iter().map(|(_, frame)| &frame.payload))
}

/// Assert `released` is the objects of each of `runs` in turn, each run going out on its own
/// PCR pace: every object starting its PCR step after the one before it.
fn assert_paced(released: &[(Instant, Frame)], runs: &[&[Object]]) {
	let mut released = released.iter();
	for (run, objects) in runs.iter().enumerate() {
		let mut previous: Option<(Instant, u64)> = None;
		for (i, object) in objects.iter().enumerate() {
			// An object goes out spread across its PCR interval, a run of packets at a time.
			let (at, frame) = released.next().expect("every object released");
			let mut payload = frame.payload.to_vec();
			while payload.len() < object.payload.len() {
				let (_, rest) = released.next().expect("every run of the object released");
				payload.extend_from_slice(&rest.payload);
			}
			assert!(
				payload == object.payload,
				"run {run} object {i} at {:?}, not the object at {} µs",
				frame.timestamp,
				object.micros
			);
			if let Some((then, micros)) = previous {
				let pause = at.duration_since(then).as_micros() as i64;
				let step = (object.micros - micros) as i64;
				assert!(
					(pause - step).abs() <= 10_000,
					"run {run} object {i} went out {pause} µs after the one before, not its PCR step of {step} µs"
				);
			}
			previous = Some((*at, object.micros));
		}
	}
	assert!(released.next().is_none(), "nothing past the runs");
}

/// Either catalog names the track: the `mpegts` section rides both.
#[tokio::test(start_paused = true)]
async fn a_capture_is_released_byte_identical_on_its_own_pcr() {
	for (data, format) in [
		(KYRION, CatalogFormat::Hang),
		(KYRION_H264, CatalogFormat::Hang),
		(KYRION, CatalogFormat::Msf),
	] {
		let imported = import(data).await;
		let source = crate::source::announced(&imported.consumer).await;
		let release = Release::new(source, format, RECORDING).await.unwrap().with_replay();
		let (released, release) = drain(release).await;

		let admitted = Admitted::new(data, &imported.objects);
		assert_eq!(admitted.grade(&released, TOLERANCE), Loss::default(), "{format:?}");
		assert_eq!(release.dropped(), 0);
	}
}

/// Each object goes out across its PCR interval, a UDP datagram's worth at a time, not in one
/// burst: only one whose successor is more than 100 ms on, or the last, is written whole.
#[tokio::test(start_paused = true)]
async fn an_object_is_spread_across_its_pcr_interval() {
	const RUN: usize = 7 * 188;
	let imported = import(KYRION).await;
	let source = crate::source::announced(&imported.consumer).await;
	let release = Release::new(source, CatalogFormat::Hang, RECORDING)
		.await
		.unwrap()
		.with_replay();
	let (released, _) = drain(release).await;

	let admitted = Admitted::new(KYRION, &imported.objects);
	assert_eq!(admitted.grade(&released, TOLERANCE), Loss::default());
	let spread = imported
		.objects
		.windows(2)
		.filter(|pair| pair[0].payload.len() > RUN && pair[1].micros - pair[0].micros <= 100_000)
		.count();
	assert!(spread > 0, "the capture has objects to spread");
	let bursts = released.iter().filter(|(_, run)| run.payload.len() > RUN).count();
	assert!(
		bursts <= imported.objects.len() - spread,
		"{bursts} writes over {RUN} bytes, {spread} of {} objects to spread",
		imported.objects.len()
	);
}

/// An object is spread only across a PCR interval on the source's clock, which the jitter clock's
/// drift may stretch on ours by no more than that drift: a further stretch, or a longer interval,
/// writes it whole.
#[test]
fn an_object_spreads_across_a_pcr_interval_its_drift_stretched() {
	const RUN: usize = 7 * 188;
	let start = Instant::now();
	let frame = Frame {
		timestamp: moq_net::Timestamp::from_micros(0).unwrap(),
		decode: None,
		duration: None,
		payload: Bytes::from(vec![0x47; 3 * RUN]),
		keyframe: true,
	};
	let first = |after: Duration, micros: u64| {
		let next = (
			start.saturating_add(after),
			moq_net::Timestamp::from_micros(micros).unwrap(),
		);
		super::release::Spread::new(frame.clone(), start, Some(next))
			.take(start)
			.unwrap()
			.payload
			.len()
	};
	assert_eq!(
		first(Duration::from_micros(100_002), 100_000),
		RUN,
		"stretched by drift"
	);
	assert_eq!(
		first(Duration::from_millis(100), 150_000),
		3 * RUN,
		"past a PCR interval"
	);
	assert_eq!(first(Duration::from_secs(1), 100_000), 3 * RUN, "stretched past drift");
}

/// Under no delay an object goes out whole as it arrives: a jump the importer did not mark
/// does not hold the output until its time.
#[tokio::test(start_paused = true)]
async fn no_delay_writes_an_object_as_it_arrives() {
	let mut objects: Vec<Object> = import(KYRION).await.objects.into_iter().take(2).collect();
	objects[1].micros = objects[0].micros + 10_000_000;
	let republished = republish(objects.clone(), |i, _| Duration::from_millis(40 * i as u64));
	let source = crate::source::announced(&republished.consumer).await;
	let mut release = Release::new(source, CatalogFormat::Hang, Duration::ZERO).await.unwrap();
	let start = Instant::now();
	for object in &objects {
		assert!(rest(&mut release, object.payload.len()).await == object.payload);
	}
	assert!(start.elapsed() < Duration::from_secs(1), "held {:?}", start.elapsed());
}

/// The oracle times each run from the source, not from its stamp: the same runs written in one
/// burst at their object's time fail it, stamped as released or with the object's time.
#[tokio::test(start_paused = true)]
async fn runs_written_in_one_burst_fail_the_grade() {
	let imported = import(KYRION).await;
	let source = crate::source::announced(&imported.consumer).await;
	let release = Release::new(source, CatalogFormat::Hang, RECORDING)
		.await
		.unwrap()
		.with_replay();
	let (released, _) = drain(release).await;
	let admitted = Admitted::new(KYRION, &imported.objects);

	for restamp in [false, true] {
		let mut burst = released.clone();
		let mut opening = burst[0].clone();
		for (at, run) in &mut burst {
			let micros = run.timestamp.as_micros().unwrap() as u64;
			if imported.objects.iter().any(|object| object.micros == micros) {
				opening = (*at, run.clone());
				continue;
			}
			*at = opening.0;
			if restamp {
				run.timestamp = opening.1.timestamp;
			}
		}
		let graded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| admitted.grade(&burst, TOLERANCE)));
		assert!(
			graded.is_err(),
			"a burst stamped {}",
			if restamp { "as its object" } else { "as released" }
		);
	}
}

/// The capture, then the capture again with its PCRs moved by `step` 90 kHz ticks around the
/// 33-bit wrap and its first PCR flagged as a discontinuity when `flagged`.
fn stepped(step: i64, flagged: bool) -> Vec<u8> {
	const WRAP: i64 = 1 << 33;
	let mut second = KYRION.to_vec();
	let mut pcrs = second
		.as_chunks_mut::<188>()
		.0
		.iter_mut()
		.filter(|pkt| pkt[3] & 0x20 != 0 && pkt[4] > 0 && pkt[5] & 0x10 != 0)
		.peekable();
	if flagged {
		pcrs.peek_mut().expect("a PCR")[5] |= 0x80;
	}
	for pkt in pcrs {
		let base = pkt[6..11].iter().fold(0u64, |base, byte| base << 8 | u64::from(*byte)) >> 7;
		let base = ((base as i64 + step).rem_euclid(WRAP) as u64) << 7 | u64::from(pkt[10] & 0x7f);
		pkt[6..11].copy_from_slice(&base.to_be_bytes()[3..]);
	}
	[KYRION, &second].concat()
}

/// Every clock break crosses the release without holding the output: a flagged jump forward,
/// which the timestamps carry, and the steps the importer splices, which they don't. The capture
/// repeated unchanged is an encoder restarting, its clock stepping back by the capture's length.
#[tokio::test(start_paused = true)]
async fn a_clock_break_crosses_without_holding_the_output() {
	for (name, step, flagged, carried) in [
		("flagged jump", 20 * 90_000, true, true),
		("unflagged back", 0, false, false),
		("flagged back", -90_000, true, false),
		("unflagged leap", 20 * 90_000, false, false),
	] {
		let input = stepped(step, flagged);
		let objects = import(&input).await.objects;
		assert_eq!(
			objects.iter().filter(|object| object.payload.is_empty()).count(),
			1,
			"{name}: a marker declares the break"
		);

		// The bytes keep coming at the source's pace across the break: only the PCR moves.
		let mut arrivals = vec![Duration::from_millis(40)];
		for pair in objects.windows(2) {
			let step = Duration::from_micros(pair[1].micros - pair[0].micros).min(Duration::from_millis(100));
			arrivals.push(*arrivals.last().unwrap() + step);
		}
		let republished = republish(objects.clone(), move |i, _| arrivals[i]);
		let source = crate::source::announced(&republished.consumer).await;
		let delay = Duration::from_millis(500);
		let release = Release::new(source, CatalogFormat::Hang, delay).await.unwrap();
		let (released, release) = drain(release).await;

		assert!(
			payloads(&released) == bytes(objects.iter().map(|object| &object.payload)),
			"{name}: both sides of the break go out, byte for byte"
		);
		assert!(
			payloads(&released) == input[input.len() - payloads(&released).len()..],
			"{name}: the output is the input"
		);
		assert_eq!(release.dropped(), 0, "{name}");
		let (jump, step) = released
			.windows(2)
			.enumerate()
			.map(|(i, pair)| {
				(
					i,
					pair[1].1.timestamp.as_micros().unwrap() as i128 - pair[0].1.timestamp.as_micros().unwrap() as i128,
				)
			})
			.max_by_key(|(_, step)| *step)
			.unwrap();
		match carried {
			true => assert!(step > 10_000_000, "{name}: the timestamps carry the jump"),
			false => assert!(
				(0..100_000).contains(&step),
				"{name}: the timestamps carry on past the splice: {step}"
			),
		}
		let pauses = released.windows(2).map(|pair| pair[1].0.duration_since(pair[0].0));
		let pause = pauses.max().unwrap();
		assert!(pause <= delay, "{name}: the output paused {pause:?} at {jump}");
	}
}

#[tokio::test(start_paused = true)]
async fn releases_fed_with_different_skew_write_the_same_bytes() {
	let objects = import(KYRION).await.objects;
	let first = objects[0].micros;
	let pcr = move |object: &Object| Duration::from_micros(object.micros - first);
	// Each object is complete once the next one starts, about a PCR interval after its own PCR.
	let steady = republish(objects.clone(), move |_, object| {
		pcr(object) + Duration::from_millis(40)
	});
	let skewed = republish(objects.clone(), move |i, object| {
		pcr(object) + Duration::from_millis(40 + 30 * (i as u64 % 3))
	});

	let (steady_task, skewed_task) = (spawn_release(&steady), spawn_release(&skewed));
	let (steady_out, steady_release) = steady_task.await.unwrap();
	let (skewed_out, skewed_release) = skewed_task.await.unwrap();

	assert_eq!(steady_release.dropped(), 0);
	assert_eq!(skewed_release.dropped(), 0);
	let admitted = Admitted::new(KYRION, &objects);
	assert_eq!(admitted.grade(&steady_out, TOLERANCE), Loss::default());
	assert_eq!(admitted.grade(&skewed_out, TOLERANCE), Loss::default());
	assert!(payloads(&steady_out) == payloads(&skewed_out), "the same bytes go out");
}

#[tokio::test(start_paused = true)]
async fn an_object_past_its_deadline_is_a_gap_in_that_release_only() {
	let objects = import(KYRION).await.objects;
	let first = objects[0].micros;
	let pcr = move |object: &Object| Duration::from_micros(object.micros - first);
	let steady = republish(objects.clone(), move |_, object| {
		pcr(object) + Duration::from_millis(40)
	});
	// The path stalls for 600 ms from 0.5 s in, then delivers what it held at once.
	let stalled = republish(objects.clone(), move |_, object| {
		let at = pcr(object) + Duration::from_millis(40);
		match at >= Duration::from_millis(500) && at < Duration::from_millis(1100) {
			true => Duration::from_millis(1100),
			false => at,
		}
	});

	let (steady_task, stalled_task) = (spawn_release(&steady), spawn_release(&stalled));
	let (steady_out, steady_release) = steady_task.await.unwrap();
	let (stalled_out, stalled_release) = stalled_task.await.unwrap();

	assert_eq!(steady_release.dropped(), 0);
	let admitted = Admitted::new(KYRION, &objects);
	assert_eq!(admitted.grade(&steady_out, TOLERANCE), Loss::default());

	// The stalled release writes the steady one's objects less a run of the late ones, each
	// counted, and the rest on the same clock as before the stall.
	let loss = admitted.grade(&stalled_out, TOLERANCE);
	assert_eq!(loss.first, 0);
	assert!(!loss.missing.is_empty(), "the stall makes objects late");
	assert_eq!(
		loss.missing.len() as u64,
		stalled_release.dropped(),
		"every late object is counted"
	);
	assert_eq!(stalled_release.stats().gaps, 0, "the late objects were delivered");
	assert!(
		loss.missing.windows(2).all(|pair| pair[1] == pair[0] + 1),
		"one run is missing: {:?}",
		loss.missing
	);
	assert!(payloads(&stalled_out) == admitted.expected(&loss));
}

/// The published capture, republished at its PCR pace, less the objects `omit` picks; groups in
/// `cut` end aborted, as the path evicting them partway would.
async fn release_omitting(cut: &[u64], omit: impl Fn(usize, &Object) -> bool) -> (Loss, Release) {
	let objects = import(KYRION).await.objects;
	let first = objects[0].micros;
	let skips = skips(&objects);
	let delivered = objects
		.iter()
		.enumerate()
		.filter(|(i, object)| !omit(*i, object))
		.map(|(_, object)| object.clone())
		.collect();
	let republished = republish_cut(delivered, skips, cut, move |_, object| {
		Duration::from_micros(object.micros - first) + Duration::from_millis(40)
	});
	let (released, release) = spawn_release(&republished).await.unwrap();
	let loss = Admitted::new(KYRION, &objects).grade(&released, TOLERANCE);
	(loss, release)
}

/// A group the path never delivers, as a relay that drops it whole: the rest go out on their
/// PCR time, leaving its time empty, and it is not late.
#[tokio::test(start_paused = true)]
async fn a_group_never_delivered_leaves_its_time_out() {
	let objects = import(KYRION).await.objects;
	let skipped = objects[objects.len() / 2].group;
	let (loss, release) = release_omitting(&[], |_, object| object.group == skipped).await;
	let expected: Vec<usize> = (0..objects.len()).filter(|&i| objects[i].group == skipped).collect();
	assert_eq!(
		loss,
		Loss {
			first: 0,
			missing: expected
		}
	);
	assert_eq!(release.dropped(), 0, "nothing arrived late");
	assert_eq!(release.stats().gaps, 1, "the source skipped once");
}

/// A group cut short loses the objects after the cut alone.
#[tokio::test(start_paused = true)]
async fn a_group_cut_short_loses_only_its_tail() {
	let objects = import(KYRION).await.objects;
	let cut = objects[objects.len() / 2].group;
	let members: Vec<usize> = (0..objects.len()).filter(|&i| objects[i].group == cut).collect();
	assert!(members.len() > 2, "a group of several objects");
	let tail = members[members.len() / 2..].to_vec();
	let (loss, release) = release_omitting(&[cut], |i, _| tail.contains(&i)).await;
	assert_eq!(
		loss,
		Loss {
			first: 0,
			missing: tail
		}
	);
	assert_eq!(release.dropped(), 0, "nothing arrived late");
	assert_eq!(release.stats().gaps, 1, "the source skipped once");
}

/// An object is never spread across a loss: with a group's last object lost, the one before it
/// goes out on its own interval, not stretched over the lost one's.
#[tokio::test(start_paused = true)]
async fn an_object_is_not_spread_across_a_loss() {
	const RUN: usize = 7 * 188;
	let objects = import(KYRION).await.objects;
	// A group whose last object a spread would otherwise cross: the one before it is over a run
	// and the next group's first is within 100 ms of it.
	let lost = (2..objects.len())
		.find(|&i| {
			objects[i].group != objects[i - 1].group
				&& objects[i - 1].group == objects[i - 2].group
				&& objects[i - 2].payload.len() > RUN
				&& objects[i].micros - objects[i - 2].micros <= 100_000
		})
		.map(|i| i - 1)
		.expect("a group to cut its last object from");
	let cut = objects[lost].group;
	let (loss, release) = release_omitting(&[cut], |i, _| i == lost).await;
	assert_eq!(
		loss,
		Loss {
			first: 0,
			missing: vec![lost]
		}
	);
	assert_eq!(release.stats().gaps, 1, "the source skipped once");
}

/// What the captures don't carry goes out at its exact position too: null packets, tables, a PID
/// no table lists, adaptation stuffing, a PES split across objects, the clock's wrap, and an MPTS
/// whose programs keep clocks and random access points of their own.
#[tokio::test(start_paused = true)]
async fn fixtures_are_released_at_their_exact_positions() {
	const GOP: u64 = 25;
	let fixtures = [
		(
			"assorted",
			fixture::assorted(fixture::WRAP - 2_000 * fixture::MS, 150, GOP),
		),
		("mpts", fixture::mpts(150, GOP)),
	];
	for (name, data) in fixtures {
		let objects = import(&data).await.objects;
		let mut groups: Vec<u64> = objects.iter().map(|object| object.group).collect();
		groups.dedup();
		let groups = groups.len();
		assert!(
			groups >= 5,
			"{name}: {groups} groups, one per random access point of program 1"
		);
		let admitted = Admitted::new(&data, &objects);
		assert!(
			admitted.prefix() < data.len() / 150 * GOP as usize,
			"{name}: acquisition withholds {} bytes, more than a group",
			admitted.prefix()
		);

		let first = objects[0].micros;
		let skewed = republish(objects.clone(), move |i, object| {
			Duration::from_micros(object.micros - first) + Duration::from_millis(40 + 30 * (i as u64 % 3))
		});
		let (released, release) = spawn_release(&skewed).await.unwrap();
		assert_eq!(admitted.grade(&released, TOLERANCE), Loss::default(), "{name}");
		assert_eq!(release.dropped(), 0, "{name}");

		let pids = pids(&payloads(&released));
		for pid in [0x0000, 0x1fff] {
			assert!(pids.contains(&pid), "{name}: the output carries PID {pid:#x}");
		}
		if name == "assorted" {
			assert!(
				pids.contains(&0x0011) && pids.contains(&0x1abc),
				"{name}: the SDT and the unlisted PID"
			);
			let last = released.last().unwrap().1.timestamp.as_micros().unwrap() as u64;
			assert!(last > fixture::WRAP / 27, "{name}: the release crosses the wrap");
		}
	}
}

/// Every PID `data`'s packets carry.
fn pids(data: &[u8]) -> std::collections::BTreeSet<u16> {
	data.as_chunks::<188>()
		.0
		.iter()
		.map(|pkt| u16::from_be_bytes([pkt[1] & 0x1f, pkt[2]]))
		.collect()
}

/// The next `len` bytes `release` writes, run by run.
async fn rest(release: &mut Release, len: usize) -> Vec<u8> {
	let mut out = Vec::with_capacity(len);
	while out.len() < len {
		let run = kio::clock::timeout(STALL, release.next())
			.await
			.expect("every arrived object is released")
			.unwrap()
			.unwrap();
		out.extend_from_slice(&run.payload);
	}
	out
}

/// Objects received while a sink stalls stay in the source and retain their arrival bound.
#[tokio::test(start_paused = true)]
async fn a_backlog_stays_in_the_source_until_due() {
	let mut objects = import(KYRION).await.objects;
	// Keep the capture in one retained group: joining newer groups is a separate
	// live-edge policy, while this regression bounds an unread group itself.
	for object in &mut objects {
		object.group = 0;
	}
	let origin = objects[0].micros;
	for delay in [Duration::ZERO, Duration::from_millis(100)] {
		let republished = republish(objects.clone(), move |_, object| {
			Duration::from_micros(object.micros - origin)
		});
		let source = crate::source::announced(&republished.consumer).await;
		let mut release = Release::new(source, CatalogFormat::Hang, delay).await.unwrap();
		let opened = release.next().await.unwrap().unwrap();
		let first = objects
			.iter()
			.position(|object| object.micros == opened.timestamp.as_micros().unwrap() as u64)
			.unwrap();
		let remainder = rest(&mut release, objects[first].payload.len() - opened.payload.len()).await;
		assert!(
			[&opened.payload[..], &remainder].concat() == objects[first].payload,
			"{delay:?}: the first object"
		);
		let acquired = release.buffered();

		// The publisher keeps its capture pace while the sink stops polling. Sleeping
		// on the paused clock lets each publication wake at its own scheduled instant.
		kio::clock::sleep(RECORDING).await;
		for (index, object) in objects.iter().enumerate().skip(first + 1) {
			let output = rest(&mut release, object.payload.len()).await;
			assert!(output == object.payload, "{delay:?}: object {index}");
			assert!(
				release.buffered() <= acquired.max(1),
				"{delay:?}: {} objects buffered after acquiring with {acquired}",
				release.buffered()
			);
		}
		assert_eq!(release.buffered(), 0);
		assert_eq!(
			release.dropped(),
			0,
			"the sink's stall does not make arrived objects late"
		);
	}
}

/// The identity that takes the path once the first stops, and when.
struct Restart {
	identity: moq_net::Identity,
	/// How long after the first's last object it starts.
	after: Duration,
	/// How far behind the first's last object its clock starts, or `None` to carry on from the
	/// source's own.
	back: Option<Duration>,
	/// The clip's group it opens on.
	from: usize,
}

impl Restart {
	/// `identity` a second after the first stops, on the source's clock.
	fn new(identity: moq_net::Identity) -> Self {
		Self {
			identity,
			after: Duration::from_secs(1),
			back: None,
			from: 3,
		}
	}
}

/// A clip into its second group published as identity `first`, ending as `ending` says, then
/// from a later group on as `second` says, released through a [`Follower`](super::Follower)
/// with `stitch`. Returns what it released, how it ended, and the objects each identity
/// published.
async fn follow(
	first: moq_net::Identity,
	ending: Ending,
	second: Restart,
	stitch: bool,
) -> (
	Vec<(Instant, Frame)>,
	Option<crate::Result<()>>,
	Vec<Object>,
	Vec<Object>,
) {
	// Six groups of a second each, from 10 s on the clock.
	let objects = import(&fixture::assorted(10_000 * fixture::MS, 150, 25)).await.objects;
	let mut groups: Vec<u64> = objects.iter().map(|object| object.group).collect();
	groups.dedup();
	assert!(groups.len() >= 5, "{} groups", groups.len());
	// The first identity stops partway into its second group.
	let before = objects.iter().position(|object| object.group == groups[1]).unwrap() + 2;
	let from = objects
		.iter()
		.position(|object| object.group == groups[second.from])
		.unwrap();
	let (head, mut tail) = (objects[..before].to_vec(), objects[from..].to_vec());
	if let Some(back) = second.back {
		let rewind = tail[0].micros - head.last().unwrap().micros + back.as_micros() as u64;
		for object in &mut tail {
			object.micros -= rewind;
		}
	}

	let origin = crate::source::produce_origin();
	let pcr = |base: u64| move |_: usize, object: &Object| Duration::from_micros(object.micros - base);
	let paced = pcr(head[0].micros);
	let _first = republish_route(
		&origin,
		moq_net::origin::RouteOptions::default().with_identity(first, 0),
		head.clone(),
		move |i, object| paced(i, object) + Duration::from_millis(40),
		ending,
	);
	let source = crate::source::resolved(&origin, "live").await;
	let release = Release::new(source, CatalogFormat::Hang, Duration::from_millis(500))
		.await
		.unwrap();
	let mut follower = super::Follower::new(release)
		.unwrap()
		.with_stitch(stitch)
		.with_follow(Duration::from_secs(5));
	let drain = tokio::spawn(async move {
		let mut released = Vec::new();
		loop {
			match kio::clock::timeout(STALL, follower.next()).await {
				Ok(Ok(Some(frame))) => released.push((Instant::now(), frame)),
				Ok(Ok(None)) => return (released, Some(Ok(()))),
				Ok(Err(err)) => return (released, Some(Err(err))),
				Err(_) => return (released, None),
			}
		}
	});

	// The second identity starts after the first's last object, at its own PCR pace.
	let down = Duration::from_micros(head.last().unwrap().micros - head[0].micros) + second.after;
	kio::clock::sleep(down).await;
	let paced = pcr(tail[0].micros);
	let _second = republish_route(
		&origin,
		moq_net::origin::RouteOptions::default().with_identity(second.identity, 0),
		tail.clone(),
		move |i, object| paced(i, object) + Duration::from_millis(40),
		Ending::Finish,
	);
	let (released, end) = drain.await.unwrap();
	(released, end, head, tail)
}

/// An import killed under the release leaves its route lingering, silent, while its restart, a
/// new identity, takes the path: stitching, the release switches to it at once and writes its
/// packets from its first group, after everything the first identity delivered, none of it late.
#[tokio::test(start_paused = true)]
async fn a_stitched_release_carries_on_into_a_restarted_import() {
	let (first, second) = (moq_net::Identity::random(), moq_net::Identity::random());
	let (released, end, head, tail) = follow(first, Ending::Silent, Restart::new(second), true).await;
	assert_paced(&released, &[&head, &tail]);
	let pauses = released.windows(2).map(|pair| pair[1].0.duration_since(pair[0].0));
	assert!(
		pauses.max().unwrap() < Duration::from_secs(2),
		"the output resumes within the downtime and the delay"
	);
	assert!(
		matches!(end, Some(Ok(()))),
		"the second identity's finish ends it: {end:?}"
	);
}

/// A replacement's clock may start behind the first's, as a restarted encoder's does: arriving
/// while the first's last objects still wait out the delay, a step back by less than the delay
/// carries no marker of its own, yet the replacement goes out whole on a clock of its own after
/// them, on its own pace and none of it late.
#[tokio::test(start_paused = true)]
async fn a_stitched_release_carries_on_into_a_rewound_clock() {
	let (first, second) = (moq_net::Identity::random(), moq_net::Identity::random());
	// It starts at once, while the first's last objects still wait out the delay.
	let second = Restart {
		identity: second,
		after: Duration::from_millis(40),
		back: Some(Duration::from_millis(300)),
		from: 3,
	};
	let (released, end, head, tail) = follow(first, Ending::Silent, second, true).await;
	assert_paced(&released, &[&head, &tail]);
	assert!(matches!(end, Some(Ok(()))), "{end:?}");
}

/// Without stitching the release stays on the silent identity, so the restart is never written.
#[tokio::test(start_paused = true)]
async fn an_unstitched_release_stays_on_the_identity_it_started_on() {
	let (first, second) = (moq_net::Identity::random(), moq_net::Identity::random());
	let (released, end, head, _) = follow(first, Ending::Silent, Restart::new(second), false).await;
	assert!(payloads(&released) == bytes(head.iter().map(|object| &object.payload)));
	assert!(end.is_none(), "still waiting on the first identity: {end:?}");
}

/// The same identity returning on a new route after its first went resumes on the same clock:
/// what it missed is a gap, and both runs go out, stitching or not. What the release buffered
/// when the route went still goes out on its pace, not in a burst once the identity returns.
#[tokio::test(start_paused = true)]
async fn the_same_identity_returning_resumes_the_release() {
	let identity = moq_net::Identity::random();
	let gone = Ending::Abort(moq_net::Error::Unroutable);
	let (released, end, head, tail) = follow(identity.clone(), gone, Restart::new(identity), false).await;
	assert_paced(&released, &[&head, &tail]);
	assert!(matches!(end, Some(Ok(()))), "the return's finish ends it: {end:?}");
}

/// The same identity returning, while what it last delivered still waits out the delay, with the
/// group it was reading published again from its start: the release carries on within that group
/// right after the objects it already took from it, writing none of them again and losing none
/// after them.
#[tokio::test(start_paused = true)]
async fn the_same_identity_returning_carries_on_within_the_group_it_was_reading() {
	let identity = moq_net::Identity::random();
	let gone = Ending::Abort(moq_net::Error::Unroutable);
	let again = Restart {
		identity: identity.clone(),
		after: Duration::from_millis(300),
		back: None,
		from: 1,
	};
	let (released, end, head, tail) = follow(identity, gone, again, false).await;
	let reading = head.last().unwrap().group;
	let taken = head.iter().filter(|object| object.group == reading).count();
	assert!(tail[..taken].iter().all(|object| object.group == reading));
	assert_paced(&released, &[&head, &tail[taken..]]);
	assert!(matches!(end, Some(Ok(()))), "{end:?}");
}
