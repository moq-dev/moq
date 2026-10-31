#![cfg(feature = "async")]

//! Delivery stalls that neither transport failure nor reconnect recovers, measured on
//! the same-epoch diamond `P -> O -> M1/M2 -> E` that route-health standbys target.
//!
//! `E` reads through `M1`, and `M2` is an eligible same-epoch route priced to stand
//! by. These fixtures pin what `E` gets today; a route-health standby on `M2` is meant
//! to change each of them.

use super::route_change::{Delivery, link, payload, priced_link, produce_origin, read, settle, timed};

use std::collections::BTreeSet;
use std::time::Duration;

use futures::channel::mpsc;
use moq_net::{Error, Timestamp, Version, origin, track};

const TEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Frames per one-second group.
const FRAMES: u64 = 10;
const STEP: Duration = Duration::from_millis(100);

/// moq-relay's default QUIC idle timeout: how long a peer that vanished without a
/// close keeps its sessions.
const IDLE_TIMEOUT: Duration = Duration::from_secs(10);

/// The group `M1` is killed in, after `E` read its first [`KILLED_AT`] frames. Off the
/// group boundary, so no group's age sits exactly on a budget.
const KILLED: u64 = 4;
const KILLED_AT: u64 = 3;

/// Collect the frames the reader has delivered so far. A group that fails, such as
/// one cut as too old, just stops contributing frames.
fn drain(rx: &mut mpsc::UnboundedReceiver<Delivery>, into: &mut BTreeSet<(u64, u64)>) {
	while let Ok((group, frame)) = rx.try_recv() {
		let Ok((_, payload)) = frame else { continue };
		let payload = String::from_utf8(payload).unwrap();
		let (_, frame) = payload.split_once('.').unwrap();
		into.insert((group, frame.parse().unwrap()));
	}
}

/// `M1` is killed without a close partway through group [`KILLED`]. `O` notices at
/// the idle timeout, and `E` notices `detect` after the kill and resumes on the cold `M2`.
///
/// Returns the groups after the killed one that `E` did not read in full.
async fn silent_kill(budget: Duration, detect: Duration) -> Vec<u64> {
	let version: Version = "moq-lite-07-wip".parse().unwrap();
	let (p, o, m1, m2, e) = (
		produce_origin(1),
		produce_origin(2),
		produce_origin(3),
		produce_origin(4),
		produce_origin(5),
	);
	let broadcast = p.create_broadcast("live").unwrap();
	let track = broadcast.create_track("video", timed()).unwrap();
	broadcast
		.announce(origin::Route::default().with_epoch(moq_net::Epoch::mint()))
		.unwrap();
	let p_o = link(version, &p, &o).await;
	let o_m1 = link(version, &o, &m1).await;
	let o_m2 = link(version, &o, &m2).await;
	let m1_e = link(version, &m1, &e).await;

	let consumer = e.consume();
	consumer.routed("live").await.unwrap();
	let remote = consumer.request_broadcast("live", None).await.unwrap();
	let prefs = track::Subscription::default().with_max_delay(budget);
	let mut rx = read(remote.track("video").unwrap().subscribe(prefs).await.unwrap());
	let m2_e = priced_link(version, &m2, &e, Some(1_000)).await;
	settle().await;

	let kill = KILLED * FRAMES + KILLED_AT;
	let e_notices = kill + (detect.as_millis() / STEP.as_millis()) as u64;
	let o_notices = kill + (IDLE_TIMEOUT.as_millis() / STEP.as_millis()) as u64;
	// Run three groups past whichever notices last, so the resumed feed settles.
	let groups = e_notices.max(o_notices) / FRAMES + 3;

	let mut seen = BTreeSet::new();
	let mut tick = 0;
	for sequence in 0..groups {
		let mut group = track.append_group().unwrap();
		for frame in 0..FRAMES {
			let timestamp = Timestamp::from_millis(tick * STEP.as_millis() as u64).unwrap();
			group.write_frame(timestamp, payload(sequence, frame)).unwrap();
			moq_clock::sleep(STEP).await;
			tick += 1;
			if tick == kill {
				drain(&mut rx, &mut seen);
				assert!(seen.contains(&(KILLED, KILLED_AT - 1)), "read up to the kill");
				for pair in [&o_m1, &m1_e] {
					pair.client_transport.set_latency(Duration::from_secs(3600));
				}
			}
			if tick == o_notices {
				o_m1.server.abort(Error::Cancel);
				o_m1.client.abort(Error::Cancel);
			}
			if tick == e_notices {
				m1_e.server.abort(Error::Cancel);
				m1_e.client.abort(Error::Cancel);
			}
		}
		group.finish().unwrap();
	}
	settle().await;
	drain(&mut rx, &mut seen);
	drop((broadcast, p_o, o_m2, m2_e));

	// The rest of the killed group is left out: whether `E` keeps reading it depends on
	// whether `O`'s copy went idle before the resume, not on the detection delay.
	(KILLED + 1..groups)
		.filter(|sequence| (0..FRAMES).any(|frame| !seen.contains(&(*sequence, frame))))
		.collect()
}

/// A group is stale once its successor arrived a full budget ago, so a resume at
/// `kill + detect` loses every group older than `detect - budget`: a silent kill costs
/// about `detect - budget` of media, and nothing once detection beats the budget. Today
/// detection is the idle timeout. A standby observing `M2` could detect after a frame
/// interval plus the paths' skew.
#[moq_clock::test]
async fn silent_kill_loses_what_detection_outlasts() {
	let cases = [
		// (budget, detection, groups lost)
		(Duration::from_secs(2), IDLE_TIMEOUT, (5..=11).collect::<Vec<_>>()),
		(Duration::from_secs(4), IDLE_TIMEOUT, (5..=9).collect()),
		(Duration::from_secs(4), Duration::from_secs(6), vec![5]),
		(Duration::from_secs(2), Duration::from_secs(4), vec![5]),
		(Duration::from_secs(2), Duration::from_secs(3), vec![]),
		(Duration::from_secs(2), Duration::from_millis(500), vec![]),
	];
	for (budget, detect, expected) in cases {
		let lost = moq_clock::timeout(TEST_TIMEOUT, silent_kill(budget, detect))
			.await
			.expect("timed out");
		assert_eq!(lost, expected, "budget {budget:?}, detection {detect:?}");
	}
}

/// How `M1` stops delivering payload to `E` while every session stays up.
#[derive(Clone, Copy, Debug)]
enum Stall {
	/// `O` runs out of stream credit toward `M1`, so `M1`'s copy receives no group.
	Upstream,
	/// `M1`'s group streams toward `E` park on stream credit `E` never grants.
	Downstream,
	/// `M1`'s group headers reach `E`, but no frame behind them does.
	Headers,
}

/// What a reader saw over a stall: frames read, groups that failed, the largest it
/// reported if that advanced, and whether its subscription ended.
#[derive(Debug, Default)]
struct Observed {
	frames: usize,
	failed: usize,
	largest: Option<track::Position>,
	ended: bool,
}

/// Drain what a reader saw since the last call, and the newest largest it reports.
fn observe(rx: &mut mpsc::UnboundedReceiver<Delivery>, sub: &mut track::Subscriber) -> Observed {
	let mut observed = Observed::default();
	while let Ok(delivery) = rx.try_recv() {
		match delivery {
			(_, Ok(_)) => observed.frames += 1,
			(u64::MAX, Err(_)) => observed.ended = true,
			(_, Err(_)) => observed.failed += 1,
		}
	}
	let waiter = moq_net::kio::Waiter::noop();
	while let std::task::Poll::Ready(Some(largest)) = sub.poll_largest(&waiter) {
		observed.largest = Some(largest);
	}
	observed
}

/// `E` reads through `M1` while `W` reads the same track through `M2`, and `M1` stalls
/// per `stall` after group 1 while `P` keeps writing for `span`.
///
/// Returns what `E` and `W` each observed after the stall began.
async fn payload_stall(stall: Stall, span: Duration) -> (Observed, Observed) {
	let version: Version = "moq-lite-07-wip".parse().unwrap();
	let (p, o, m1, m2, e, w) = (
		produce_origin(1),
		produce_origin(2),
		produce_origin(3),
		produce_origin(4),
		produce_origin(5),
		produce_origin(6),
	);
	let broadcast = p.create_broadcast("live").unwrap();
	let track = broadcast.create_track("video", timed()).unwrap();
	broadcast
		.announce(origin::Route::default().with_epoch(moq_net::Epoch::mint()))
		.unwrap();
	let p_o = link(version, &p, &o).await;
	let o_m1 = link(version, &o, &m1).await;
	let o_m2 = link(version, &o, &m2).await;
	let m1_e = link(version, &m1, &e).await;
	let m2_w = link(version, &m2, &w).await;

	async fn subscribe(origin: &origin::Producer) -> track::Subscriber {
		let consumer = origin.consume();
		consumer.routed("live").await.unwrap();
		let remote = consumer.request_broadcast("live", None).await.unwrap();
		let prefs = track::Subscription::default().with_max_delay(Duration::from_secs(2));
		remote.track("video").unwrap().subscribe(prefs).await.unwrap()
	}
	let mut e_sub = subscribe(&e).await;
	let mut e_rx = read(subscribe(&e).await);
	let mut w_sub = subscribe(&w).await;
	let mut w_rx = read(subscribe(&w).await);
	let m2_e = priced_link(version, &m2, &e, Some(1_000)).await;
	settle().await;

	let mut tick = 0u64;
	let mut write = async |groups: u64| {
		for _ in 0..groups {
			let sequence = tick / FRAMES;
			let mut group = track.append_group().unwrap();
			for frame in 0..FRAMES {
				let timestamp = Timestamp::from_millis(tick * STEP.as_millis() as u64).unwrap();
				group.write_frame(timestamp, payload(sequence, frame)).unwrap();
				moq_clock::sleep(STEP).await;
				tick += 1;
			}
			group.finish().unwrap();
		}
	};
	write(2).await;
	settle().await;
	let before = observe(&mut e_rx, &mut e_sub);
	assert_eq!(before.frames, 2 * FRAMES as usize, "E read through M1 before the stall");
	observe(&mut w_rx, &mut w_sub);

	match stall {
		Stall::Upstream => o_m1.server_transport.withhold_unis(),
		Stall::Downstream => m1_e.server_transport.withhold_unis(),
		Stall::Headers => m1_e.server_transport.split_unis(),
	}
	write(span.as_secs()).await;
	settle().await;

	let seen = (observe(&mut e_rx, &mut e_sub), observe(&mut w_rx, &mut w_sub));
	drop((broadcast, p_o, o_m1, o_m2, m1_e, m2_w, m2_e));
	seen
}

/// `M1` stops delivering payload for 30 seconds while every session stays up, so no idle
/// timeout fires and nothing reconnects. `E` reads nothing and its subscription neither
/// ends nor moves, while `W` reads every frame through `M2`. When only `M1`'s group
/// headers arrive, `E`'s largest keeps pace with `W`'s and its groups fail as too old:
/// comparing the two routes' largest positions finds nothing wrong.
#[moq_clock::test]
async fn a_payload_stall_with_live_sessions_is_never_recovered() {
	const SPAN: Duration = Duration::from_secs(30);
	// The stall starts after group 1, and `P` writes one group per second through it.
	let newest = 1 + SPAN.as_secs();
	for stall in [Stall::Upstream, Stall::Downstream, Stall::Headers] {
		let (e, w) = moq_clock::timeout(TEST_TIMEOUT, payload_stall(stall, SPAN))
			.await
			.expect("timed out");
		assert_eq!(e.frames, 0, "{stall:?}: E read through the stall");
		assert!(!e.ended, "{stall:?}: E's subscription ended");
		assert_eq!(w.frames, (SPAN.as_secs() * FRAMES) as usize, "{stall:?}: W lost media");
		assert_eq!(w.largest.map(|largest| largest.group), Some(newest), "{stall:?}");
		match stall {
			Stall::Upstream | Stall::Downstream => {
				assert_eq!(e.largest, None, "{stall:?}: E's largest moved");
				assert_eq!(e.failed, 0, "{stall:?}");
			}
			Stall::Headers => {
				assert_eq!(e.largest.map(|largest| largest.group), Some(newest), "{stall:?}");
				assert!(e.failed > 0, "{stall:?}: no group was cut");
			}
		}
	}
}
