//! Tests for [`Release`](super::Release).
//!
//! Each imports a hardware encoder's capture or a [fixture](super::fixture) with [`Passthrough`]
//! and releases the track it publishes, either replayed whole or republished object by object on
//! a paused clock, and grades the output with the [oracle](super::oracle).

use std::time::Duration;

use bytes::Bytes;
use kio::clock::Instant;

use super::oracle::{Admitted, Loss, Object};
use super::{Passthrough, Release, fixture};
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
	_passthrough: Passthrough,
	_catalog: crate::catalog::Producer,
	_broadcast: moq_net::broadcast::Producer,
	consumer: moq_net::broadcast::Consumer,
	objects: Vec<Object>,
}

async fn import(data: &[u8]) -> Imported {
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
	let consumer = broadcast.consume();
	let mut passthrough = Passthrough::new(broadcast.clone(), catalog.reserve()).unwrap();
	let name = passthrough.track().to_string();
	passthrough.decode(data).unwrap();
	passthrough.finish().unwrap();

	let mut track = consumer
		.track(&name)
		.unwrap()
		.subscribe(moq_net::track::Subscription::default().with_max_delay(RECORDING))
		.await
		.unwrap();
	let mut objects = Vec::new();
	while let Some(mut group) = kio::clock::timeout(Duration::from_secs(1), track.recv_group())
		.await
		.unwrap()
		.unwrap()
	{
		while let Some(frame) = group.read_frame().await.unwrap() {
			if frame.payload.is_empty() {
				let micros = objects.last().map_or(0, |object: &Object| object.micros);
				objects.push(Object {
					group: group.sequence,
					micros,
					payload: Bytes::new(),
				});
				continue;
			}
			let frame = hang::container::Frame::decode(frame.payload).unwrap();
			objects.push(Object {
				group: group.sequence,
				micros: frame.timestamp.as_micros() as u64,
				payload: frame.payload,
			});
		}
	}
	Imported {
		_passthrough: passthrough,
		_catalog: catalog,
		_broadcast: broadcast,
		consumer,
		objects,
	}
}

/// Republish `objects` on a new broadcast's `m2ts` track, in the groups they were published in,
/// each once `at` says it is due after the start and not before the one ahead of it, as a relay
/// delivering them would.
fn republish(objects: Vec<Object>, at: impl Fn(usize, &Object) -> Duration + Send + 'static) -> Republished {
	republish_cut(objects, &[], at)
}

/// [`republish`], aborting each group in `cut` once its last object in `objects` is out, as a
/// relay that evicts a group partway does.
fn republish_cut(
	objects: Vec<Object>,
	cut: &[u64],
	at: impl Fn(usize, &Object) -> Duration + Send + 'static,
) -> Republished {
	let cut = cut.to_vec();
	let mut broadcast = moq_net::broadcast::Info::new().produce();
	let mut catalog = crate::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
	let track = broadcast
		.unique_track(".m2ts", catalog.track_info(hang::catalog::PRIORITY.video))
		.unwrap();
	let name = track.name().to_string();
	catalog
		.mutate(|catalog| catalog.m2ts = Some(hang::catalog::M2ts::new(name)))
		.unwrap();
	let consumer = broadcast.consume();
	let start = Instant::now();
	let task = tokio::spawn(async move {
		let _keep = (&broadcast, &catalog);
		let mut group: Option<(u64, moq_net::group::Producer)> = None;
		let mut due = Duration::ZERO;
		for (i, object) in objects.into_iter().enumerate() {
			due = due.max(at(i, &object));
			kio::clock::sleep_until(start + due).await;
			if group.as_ref().map(|(sequence, _)| *sequence) != Some(object.group) {
				if let Some((sequence, group)) = group.take() {
					end(sequence, group, &cut);
				}
				let info = moq_net::group::Info { sequence: object.group };
				group = Some((object.group, track.create_group(info).unwrap()));
			}
			let timestamp = moq_net::Timestamp::from_micros(object.micros).unwrap();
			let group = &mut group.as_mut().unwrap().1;
			if object.payload.is_empty() {
				group.write_frame(timestamp, Bytes::new()).unwrap();
				continue;
			}
			let frame = hang::container::Frame {
				timestamp,
				payload: object.payload,
			};
			frame.write_to(group).unwrap();
		}
		if let Some((sequence, group)) = group {
			end(sequence, group, &cut);
		}
		track.finish().unwrap();
		std::future::pending::<()>().await;
	});
	Republished { consumer, task }
}

/// Finish `group`, or abort it as evicted if it is one of the `cut`.
fn end(sequence: u64, mut group: moq_net::group::Producer, cut: &[u64]) {
	match cut.contains(&sequence) {
		true => group.abort(moq_net::Error::Old).unwrap(),
		false => group.finish().unwrap(),
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
		let release = Release::new(source, CatalogFormat::Hang).await.unwrap();
		drain(release.with_delay(Duration::from_millis(500))).await
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

#[tokio::test(start_paused = true)]
async fn a_capture_is_released_byte_identical_on_its_own_pcr() {
	for data in [KYRION, KYRION_H264] {
		let imported = import(data).await;
		let source = crate::source::announced(&imported.consumer).await;
		let release = Release::new(source, CatalogFormat::Hang)
			.await
			.unwrap()
			.with_delay(RECORDING)
			.with_replay();
		let (released, release) = drain(release).await;

		let admitted = Admitted::new(data, &imported.objects);
		assert_eq!(admitted.grade(&released, TOLERANCE), Loss::default());
		assert_eq!(release.dropped(), 0);
	}
}

#[tokio::test(start_paused = true)]
async fn a_flagged_jump_crosses_without_holding_the_output() {
	// The capture twice, the second copy's clock twenty seconds ahead, its first PCR flagged
	// as the discontinuity.
	const STEP: u64 = 20 * 90_000;
	let mut second = KYRION.to_vec();
	let mut pcrs = second
		.as_chunks_mut::<188>()
		.0
		.iter_mut()
		.filter(|pkt| pkt[3] & 0x20 != 0 && pkt[4] > 0 && pkt[5] & 0x10 != 0)
		.peekable();
	pcrs.peek_mut().expect("a PCR")[5] |= 0x80;
	for pkt in pcrs {
		let base = pkt[6..11].iter().fold(0u64, |base, byte| base << 8 | u64::from(*byte)) >> 7;
		let base = ((base + STEP) & ((1 << 33) - 1)) << 7 | u64::from(pkt[10] & 0x7f);
		pkt[6..11].copy_from_slice(&base.to_be_bytes()[3..]);
	}
	let input = [KYRION, &second].concat();
	let objects = import(&input).await.objects;
	assert!(
		objects.iter().any(|object| object.payload.is_empty()),
		"a marker declares the break"
	);

	// The bytes keep coming at the source's pace across the jump: only the PCR leaps.
	let mut arrivals = vec![Duration::from_millis(40)];
	for pair in objects.windows(2) {
		let step = Duration::from_micros(pair[1].micros - pair[0].micros).min(Duration::from_millis(100));
		arrivals.push(*arrivals.last().unwrap() + step);
	}
	let republished = republish(objects.clone(), move |i, _| arrivals[i]);
	let source = crate::source::announced(&republished.consumer).await;
	let release = Release::new(source, CatalogFormat::Hang).await.unwrap();
	let delay = Duration::from_millis(500);
	let (released, release) = drain(release.with_delay(delay)).await;

	let out = bytes(released.iter().map(|(_, frame)| &frame.payload));
	assert!(
		out == bytes(objects.iter().map(|object| &object.payload)),
		"both sides of the jump go out, byte for byte"
	);
	assert_eq!(release.dropped(), 0);
	let (jump, step) = released
		.windows(2)
		.enumerate()
		.map(|(i, pair)| {
			(
				i,
				pair[1].1.timestamp.as_micros() as i128 - pair[0].1.timestamp.as_micros() as i128,
			)
		})
		.max_by_key(|(_, step)| *step)
		.unwrap();
	assert!(step > 10_000_000, "the timestamps carry the jump");
	let pause = released[jump + 1].0.duration_since(released[jump].0);
	assert!(pause <= delay, "the output paused {pause:?} at the jump");
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
	let delivered = objects
		.iter()
		.enumerate()
		.filter(|(i, object)| !omit(*i, object))
		.map(|(_, object)| object.clone())
		.collect();
	let republished = republish_cut(delivered, cut, move |_, object| {
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
		let groups = objects.last().unwrap().group - objects[0].group + 1;
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
			let last = released.last().unwrap().1.timestamp.as_micros() as u64;
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

/// The MSF catalog never names the `m2ts` track, so a release reading it would wait forever.
#[tokio::test(start_paused = true)]
async fn an_msf_catalog_is_refused() {
	let imported = import(KYRION).await;
	let source = crate::source::announced(&imported.consumer).await;
	let Err(err) = Release::new(source, CatalogFormat::Msf).await else {
		panic!("a release reading the MSF catalog must fail");
	};
	assert!(err.to_string().contains("MSF"), "{err}");
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
		let mut release = Release::new(source, CatalogFormat::Hang)
			.await
			.unwrap()
			.with_delay(delay);
		let first = release.next().await.unwrap().unwrap();
		let first = objects
			.iter()
			.position(|object| object.payload == first.payload)
			.unwrap();
		let acquired = release.buffered();

		// The publisher keeps its capture pace while the sink stops polling. Sleeping
		// on the paused clock lets each publication wake at its own scheduled instant.
		kio::clock::sleep(RECORDING).await;
		for (index, object) in objects.iter().enumerate().skip(first + 1) {
			let output = kio::clock::timeout(STALL, release.next())
				.await
				.expect("every arrived object is released")
				.unwrap()
				.unwrap();
			assert!(output.payload == object.payload, "{delay:?}: object {index}");
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
