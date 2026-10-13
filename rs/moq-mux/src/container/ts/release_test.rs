//! Tests for [`Release`](super::Release).
//!
//! Each imports a hardware encoder's capture with [`Passthrough`] and releases the track it
//! publishes, either replayed whole or republished object by object on a paused clock.

use std::time::Duration;

use bytes::Bytes;
use tokio::time::Instant;

use super::{Passthrough, Release};
use crate::catalog::CatalogFormat;
use crate::container::Frame;

const KYRION: &[u8] = include_bytes!("test_data/kyrion_mpeg2av_ac3.ts");
const KYRION_H264: &[u8] = include_bytes!("test_data/scte35/kyrion_dirtystart.ts");

/// Longer than either capture, so a replay reads every group.
const RECORDING: Duration = Duration::from_secs(30);
/// How long a drain waits for the next object before taking the release to have stalled.
const STALL: Duration = RECORDING.saturating_mul(2);

/// One object as published.
#[derive(Clone, Debug, PartialEq)]
struct Object {
	group: u64,
	micros: u64,
	payload: Bytes,
}

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
	while let Some(mut group) = tokio::time::timeout(Duration::from_secs(1), track.recv_group())
		.await
		.unwrap()
		.unwrap()
	{
		while let Some(frame) = group.read_frame().await.unwrap() {
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
			tokio::time::sleep_until(start + due).await;
			if group.as_ref().map(|(sequence, _)| *sequence) != Some(object.group) {
				if let Some((_, group)) = group.take() {
					group.finish().unwrap();
				}
				let info = moq_net::group::Info { sequence: object.group };
				group = Some((object.group, track.create_group(info).unwrap()));
			}
			let frame = hang::container::Frame {
				timestamp: moq_net::Timestamp::from_micros(object.micros).unwrap(),
				payload: object.payload,
			};
			frame.write_to(&mut group.as_mut().unwrap().1).unwrap();
		}
		if let Some((_, group)) = group {
			group.finish().unwrap();
		}
		track.finish().unwrap();
		std::future::pending::<()>().await;
	});
	Republished { consumer, task }
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
	while let Ok(next) = tokio::time::timeout(STALL, release.next()).await {
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

/// How much later than the first object each one went out, less how much later its PCR time is.
fn lag(released: &[(Instant, Frame)]) -> Vec<i128> {
	let (start, first) = &released[0];
	released
		.iter()
		.map(|(at, frame)| {
			let out = at.duration_since(*start).as_micros() as i128;
			out - (frame.timestamp.as_micros() as i128 - first.timestamp.as_micros() as i128)
		})
		.collect()
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

		let out = bytes(released.iter().map(|(_, frame)| &frame.payload));
		assert!(
			out == bytes(imported.objects.iter().map(|object| &object.payload)),
			"every published object goes out, in order"
		);
		assert!(data.ends_with(&out), "the output is the input from the first group on");
		assert_eq!(release.dropped(), 0);
		for (i, lag) in lag(&released).into_iter().enumerate() {
			assert!(lag.abs() < 1_000, "object {i} goes out {lag} us off its PCR time");
		}
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
		objects.windows(2).any(|pair| pair[1].group > pair[0].group + 1),
		"the break skips a sequence"
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
	let payloads = |released: &[(Instant, Frame)]| bytes(released.iter().map(|(_, frame)| &frame.payload));
	assert!(payloads(&steady_out) == payloads(&skewed_out), "the same bytes go out");
	assert!(
		payloads(&steady_out) == bytes(objects.iter().map(|object| &object.payload)),
		"from the first group"
	);
	for released in [&steady_out, &skewed_out] {
		for (i, lag) in lag(released).into_iter().enumerate() {
			assert!(lag.abs() < 1_000, "object {i} goes out {lag} us off its PCR time");
		}
	}
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
	let dropped = stalled_release.dropped() as usize;
	assert!(dropped > 0, "the stall makes objects late");
	assert_eq!(
		stalled_out.len() + dropped,
		steady_out.len(),
		"every late object is counted"
	);

	// The stalled release writes the steady one's objects less a run of the late ones.
	let gap = steady_out
		.iter()
		.zip(&stalled_out)
		.position(|((_, steady), (_, stalled))| steady.payload != stalled.payload)
		.expect("a gap");
	let frames = |released: &[(Instant, Frame)]| -> Vec<(u64, Bytes)> {
		released
			.iter()
			.map(|(_, frame)| (frame.timestamp.as_micros() as u64, frame.payload.clone()))
			.collect()
	};
	let mut expected = frames(&steady_out);
	expected.drain(gap..gap + dropped);
	assert!(frames(&stalled_out) == expected, "only the late objects are missing");

	// The rest still go out on time: on the same clock as before the stall.
	for (i, lag) in lag(&stalled_out).into_iter().enumerate() {
		assert!(lag.abs() < 1_000, "object {i} goes out {lag} us off its PCR time");
	}
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
