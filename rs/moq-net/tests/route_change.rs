//! A subscription survives its route changing, end to end over real sessions.
//!
//! A publisher `P` is pulled by two relays `A` and `B`, both of which re-advertise
//! it to the subscribing relay `R`. Both routes carry `P`'s epoch, so `R` may resume
//! a subscription served through one onto the other. The reader on `R` must see
//! every frame exactly once, in order, whether the route changes between groups or
//! in the middle of one, and however it changes.
//!
//! Older wire versions cannot carry the epoch, so `R` cannot tell the two routes
//! serve the same bytes: its subscription stays on its route and ends with it.

mod support;

use std::time::Duration;

use futures::{StreamExt, channel::mpsc};
use moq_net::{Error, Hop, Timestamp, Version, broadcast, origin, track};
use support::harness::{MockConnectOptions, MockPair, connect_mock};

const TEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Frames per group.
const FRAMES: u64 = 4;

fn produce_origin(hop: u64) -> origin::Producer {
	let (producer, driver) = origin::Producer::new(origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

/// Have `to` pull everything `from` publishes.
async fn link(version: Version, from: &origin::Producer, to: &origin::Producer) -> MockPair {
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(from.consume());
	options.client_subscribe = Some(to.clone());
	connect_mock(options).await
}

fn payload(group: u64, frame: u64) -> Vec<u8> {
	format!("{group}.{frame}").into_bytes()
}

/// Let every task settle. Time is paused, so this returns once the runtime is idle.
async fn settle() {
	moq_net_sim::sleep(Duration::from_millis(500)).await;
}

/// How the serving route goes away (or is beaten).
#[derive(Clone, Copy, Debug)]
enum Trigger {
	/// The session to `A` dies abruptly.
	Disconnect,
	/// `A` loses the broadcast and withdraws it, while its session to `R` stays up.
	Unannounce,
	/// `R` gains a direct (shorter) route to `P` while `A` stays healthy.
	BetterRoute,
	/// The direct route from [`Trigger::BetterRoute`] dies, sending the track back.
	BetterRouteDies,
}

/// Where in the stream the route changes.
#[derive(Clone, Copy, Debug)]
enum Position {
	/// After a group finished, before the next one starts.
	BetweenGroups,
	/// After the reader saw half of an open group.
	MidGroup,
}

struct Topology {
	version: Version,
	publisher: origin::Producer,
	relay_b: origin::Producer,
	subscriber: origin::Producer,
	_broadcast: broadcast::Producer,
	track: track::Producer,
	p_to_a: Option<MockPair>,
	a_to_r: Option<MockPair>,
	direct: Option<MockPair>,
	_links: Vec<MockPair>,
}

impl Topology {
	/// `P -> A -> R` serving, with `P -> B` pulled and `B -> R` connected after the
	/// subscription started on `A`, so `A` is the incumbent.
	async fn new(version: Version) -> (Self, track::Subscriber) {
		let publisher = produce_origin(1);
		let relay_a = produce_origin(2);
		let relay_b = produce_origin(3);
		let subscriber = produce_origin(4);

		let broadcast = publisher.create_broadcast("live").unwrap();
		let track = broadcast.create_track("video", None).unwrap();
		broadcast
			.announce(origin::Route::default().with_epoch(moq_net::Epoch::mint()))
			.unwrap();

		let p_to_a = link(version, &publisher, &relay_a).await;
		let p_to_b = link(version, &publisher, &relay_b).await;
		let a_to_r = link(version, &relay_a, &subscriber).await;

		let consumer = subscriber.consume();
		consumer.routed("live").await.unwrap();
		let remote = consumer.request_broadcast("live").await.unwrap();
		let prefs = track::Subscription::default().with_max_delay(Duration::from_secs(60));
		let sub = remote.track("video").unwrap().subscribe(prefs).await.unwrap();

		let topology = Self {
			version,
			publisher,
			relay_b,
			subscriber,
			_broadcast: broadcast,
			track,
			p_to_a: Some(p_to_a),
			a_to_r: Some(a_to_r),
			direct: None,
			_links: vec![p_to_b],
		};
		(topology, sub)
	}

	/// Connect the standby route `B -> R`.
	async fn standby(&mut self) {
		let b_to_r = link(self.version, &self.relay_b, &self.subscriber).await;
		self._links.push(b_to_r);
		settle().await;
	}

	async fn trigger(&mut self, trigger: Trigger) {
		match trigger {
			Trigger::Disconnect => {
				let pair = self.a_to_r.take().unwrap();
				pair.server.abort(Error::Cancel);
				pair.client.abort(Error::Cancel);
			}
			Trigger::Unannounce => {
				let pair = self.p_to_a.take().unwrap();
				pair.server.abort(Error::Cancel);
				pair.client.abort(Error::Cancel);
			}
			Trigger::BetterRoute => {
				self.direct = Some(link(self.version, &self.publisher, &self.subscriber).await);
			}
			Trigger::BetterRouteDies => {
				let pair = self.direct.take().unwrap();
				pair.server.abort(Error::Cancel);
				pair.client.abort(Error::Cancel);
			}
		}
		settle().await;
	}
}

/// Read every group in full, reporting each frame as it arrives.
fn read(mut sub: track::Subscriber) -> mpsc::UnboundedReceiver<(u64, moq_net::Result<Vec<u8>>)> {
	let (tx, rx) = mpsc::unbounded();
	moq_net_sim::spawn(async move {
		loop {
			let mut group = match sub.recv_group().await {
				Ok(Some(group)) => group,
				Ok(None) => return,
				Err(err) => {
					let _ = tx.unbounded_send((u64::MAX, Err(err)));
					return;
				}
			};
			loop {
				match group.read_frame().await {
					Ok(Some(frame)) => {
						if tx.unbounded_send((group.sequence, Ok(frame.payload.to_vec()))).is_err() {
							return;
						}
					}
					Ok(None) => break,
					Err(err) => {
						let _ = tx.unbounded_send((group.sequence, Err(err)));
						break;
					}
				}
			}
		}
	});
	rx
}

/// Wait for the next frame the reader reports, failing on an error or a hang.
async fn next(rx: &mut mpsc::UnboundedReceiver<(u64, moq_net::Result<Vec<u8>>)>) -> (u64, Vec<u8>) {
	let (group, frame) = moq_net_sim::timeout(Duration::from_secs(10), rx.next())
		.await
		.expect("reader hung")
		.expect("reader ended");
	(group, frame.unwrap_or_else(|err| panic!("group {group} failed: {err}")))
}

async fn route_change(version: &str, trigger: Trigger, position: Position) {
	let version: Version = version.parse().unwrap();
	let (mut topology, sub) = Topology::new(version).await;
	let mut rx = read(sub);
	topology.standby().await;

	let mut expected = Vec::new();
	let mut seen = Vec::new();

	// Group 0 in full through `A`.
	let mut group = topology.track.append_group().unwrap();
	for frame in 0..FRAMES {
		group.write_frame(Timestamp::ZERO, payload(0, frame)).unwrap();
		expected.push((0, payload(0, frame)));
		seen.push(next(&mut rx).await);
	}
	group.finish().unwrap();

	// Group 1: half through `A`, then the route changes. Between groups, the change
	// lands before the group starts at all.
	let split = match position {
		Position::BetweenGroups => 0,
		Position::MidGroup => FRAMES / 2,
	};
	if split == 0 {
		topology.trigger(trigger).await;
	}
	let mut group = topology.track.append_group().unwrap();
	for frame in 0..FRAMES {
		if frame == split && split != 0 {
			topology.trigger(trigger).await;
		}
		group.write_frame(Timestamp::ZERO, payload(1, frame)).unwrap();
		expected.push((1, payload(1, frame)));
		seen.push(next(&mut rx).await);
	}
	group.finish().unwrap();

	// Group 2 entirely through the replacement.
	let mut group = topology.track.append_group().unwrap();
	for frame in 0..FRAMES {
		group.write_frame(Timestamp::ZERO, payload(2, frame)).unwrap();
		expected.push((2, payload(2, frame)));
		seen.push(next(&mut rx).await);
	}
	group.finish().unwrap();

	let render = |frames: &[(u64, Vec<u8>)]| {
		frames
			.iter()
			.map(|(group, frame)| format!("{group}:{}", String::from_utf8_lossy(frame)))
			.collect::<Vec<_>>()
			.join(" ")
	};
	assert_eq!(render(&seen), render(&expected), "{version} {trigger:?} {position:?}");

	// Nothing trails: no duplicate group or frame arrives later.
	settle().await;
	assert!(
		rx.try_recv().is_err(),
		"{version} {trigger:?} {position:?}: trailing delivery"
	);
}

/// The route changes again and again in one stream, each time mid-group: a direct
/// route beats `A`, then dies, then `A`'s session dies too, leaving `B`. The reader
/// still sees every frame exactly once.
async fn route_flaps(version: &str) {
	let version: Version = version.parse().unwrap();
	let (mut topology, sub) = Topology::new(version).await;
	let mut rx = read(sub);
	topology.standby().await;

	let triggers = [Trigger::BetterRoute, Trigger::BetterRouteDies, Trigger::Disconnect];
	let mut expected = Vec::new();
	let mut seen = Vec::new();
	for sequence in 0..=triggers.len() as u64 {
		let mut group = topology.track.append_group().unwrap();
		for frame in 0..FRAMES {
			if frame == FRAMES / 2
				&& let Some(trigger) = triggers.get(sequence as usize)
			{
				topology.trigger(*trigger).await;
			}
			group.write_frame(Timestamp::ZERO, payload(sequence, frame)).unwrap();
			expected.push((sequence, payload(sequence, frame)));
			seen.push(next(&mut rx).await);
		}
		group.finish().unwrap();
	}

	assert_eq!(seen, expected, "{version}");
	settle().await;
	assert!(rx.try_recv().is_err(), "{version}: trailing delivery");
}

/// A wire without epochs: the subscription stays on `A` while `B` stands by, and
/// ends when `A`'s route goes instead of resuming through `B`.
async fn route_dies_without_an_epoch(version: &str, trigger: Trigger) {
	let version: Version = version.parse().unwrap();
	let (mut topology, sub) = Topology::new(version).await;
	let mut rx = read(sub);
	topology.standby().await;

	let mut group = topology.track.append_group().unwrap();
	for frame in 0..FRAMES {
		group.write_frame(Timestamp::ZERO, payload(0, frame)).unwrap();
		assert_eq!(next(&mut rx).await, (0, payload(0, frame)), "{version}");
	}
	group.finish().unwrap();

	topology.trigger(trigger).await;
	let mut group = topology.track.append_group().unwrap();
	group.write_frame(Timestamp::ZERO, payload(1, 0)).unwrap();
	settle().await;
	match moq_net_sim::timeout(Duration::from_secs(10), rx.next())
		.await
		.expect("reader hung")
	{
		None | Some((_, Err(_))) => {}
		Some((group, Ok(frame))) => panic!(
			"{version} {trigger:?}: resumed through another route at {group}:{}",
			String::from_utf8_lossy(&frame)
		),
	}
}

macro_rules! route_change_tests {
	($($name:ident: $version:literal,)*) => {
		$(
			mod $name {
				use super::*;

				async fn run(trigger: Trigger, position: Position) {
					moq_net_sim::timeout(TEST_TIMEOUT, route_change($version, trigger, position))
						.await
						.expect("timed out");
				}

				#[moq_net_sim::test]
				async fn disconnect_between_groups() {
					run(Trigger::Disconnect, Position::BetweenGroups).await;
				}

				#[moq_net_sim::test]
				async fn disconnect_mid_group() {
					run(Trigger::Disconnect, Position::MidGroup).await;
				}

				#[moq_net_sim::test]
				async fn unannounce_between_groups() {
					run(Trigger::Unannounce, Position::BetweenGroups).await;
				}

				#[moq_net_sim::test]
				async fn unannounce_mid_group() {
					run(Trigger::Unannounce, Position::MidGroup).await;
				}

				#[moq_net_sim::test]
				async fn better_route_between_groups() {
					run(Trigger::BetterRoute, Position::BetweenGroups).await;
				}

				#[moq_net_sim::test]
				async fn better_route_mid_group() {
					run(Trigger::BetterRoute, Position::MidGroup).await;
				}

			}
		)*
	};
}

route_change_tests! {
	lite_07: "moq-lite-07-wip",
}

macro_rules! pinned_tests {
	($($name:ident: $version:literal,)*) => {
		$(
			mod $name {
				use super::*;

				async fn run(trigger: Trigger) {
					moq_net_sim::timeout(TEST_TIMEOUT, route_dies_without_an_epoch($version, trigger))
						.await
						.expect("timed out");
				}

				#[moq_net_sim::test]
				async fn disconnect_ends_the_subscription() {
					run(Trigger::Disconnect).await;
				}

				#[moq_net_sim::test]
				async fn unannounce_ends_the_subscription() {
					run(Trigger::Unannounce).await;
				}

				/// A better route without an epoch does not move the subscription either.
				#[moq_net_sim::test]
				async fn better_route_keeps_the_incumbent() {
					moq_net_sim::timeout(TEST_TIMEOUT, route_change($version, Trigger::BetterRoute, Position::MidGroup))
						.await
						.expect("timed out");
				}
			}
		)*
	};
}

pinned_tests! {
	lite_06: "moq-lite-06",
	lite_05: "moq-lite-05",
	lite_04: "moq-lite-04",
	ietf_19: "moq-transport-19",
	ietf_22: "moq-transport-22",
}

/// `P` feeds `A` slowly and `B` promptly, and `R` reads through `A` until that route dies
/// partway through group 1: `A` has handed on (1, 0) and (1, 1) while `P` has already
/// moved on to group 2. `B` (warmed by another reader on `W`) holds everything, so `R`
/// resumes there from (1, 2).
async fn lagging_route_dies(version: Version) -> mpsc::UnboundedReceiver<(u64, moq_net::Result<Vec<u8>>)> {
	async fn lagged(version: Version, from: &origin::Producer, to: &origin::Producer, lag: Duration) -> MockPair {
		let mut options = MockConnectOptions::new(version);
		options.server_publish = Some(from.consume());
		options.client_subscribe = Some(to.clone());
		options.latency = lag;
		connect_mock(options).await
	}
	async fn subscribe(origin: &origin::Producer) -> track::Subscriber {
		let consumer = origin.consume();
		consumer.routed("live").await.unwrap();
		let remote = consumer.request_broadcast("live").await.unwrap();
		let preferences = track::Subscription::default().with_max_delay(Duration::from_secs(60));
		remote.track("video").unwrap().subscribe(preferences).await.unwrap()
	}
	fn write(group: &mut moq_net::group::Producer, sequence: u64, frame: u64) {
		let timestamp = Timestamp::from_micros(1_000_000 + sequence * 100_000 + frame * 1_000).unwrap();
		group.write_frame(timestamp, payload(sequence, frame)).unwrap();
	}

	let (p, a, b, r, w) = (
		produce_origin(1),
		produce_origin(2),
		produce_origin(3),
		produce_origin(4),
		produce_origin(5),
	);
	let broadcast = p.create_broadcast("live").unwrap();
	let track = broadcast.create_track("video", None).unwrap();
	broadcast
		.announce(origin::Route::default().with_epoch(moq_net::Epoch::mint()))
		.unwrap();
	let p_a = lagged(version, &p, &a, Duration::from_millis(300)).await;
	let p_b = lagged(version, &p, &b, Duration::ZERO).await;
	let a_r = lagged(version, &a, &r, Duration::ZERO).await;
	let b_w = lagged(version, &b, &w, Duration::ZERO).await;
	let warm = read(subscribe(&w).await);
	let mut rx = read(subscribe(&r).await);
	let b_r = lagged(version, &b, &r, Duration::ZERO).await;
	settle().await;

	let mut group = track.append_group().unwrap();
	for frame in 0..4 {
		write(&mut group, 0, frame);
	}
	group.finish().unwrap();
	for frame in 0..4 {
		assert_eq!(next(&mut rx).await, (0, payload(0, frame)), "{version}");
	}

	let mut group = track.append_group().unwrap();
	write(&mut group, 1, 0);
	write(&mut group, 1, 1);
	assert_eq!(next(&mut rx).await, (1, payload(1, 0)), "{version}");
	assert_eq!(next(&mut rx).await, (1, payload(1, 1)), "{version}");
	// P moves on while A is still behind.
	write(&mut group, 1, 2);
	write(&mut group, 1, 3);
	group.finish().unwrap();
	let mut group = track.append_group().unwrap();
	write(&mut group, 2, 0);

	a_r.server.abort(Error::Cancel);
	a_r.client.abort(Error::Cancel);
	settle().await;
	moq_net_sim::spawn(async move {
		let _keep = (broadcast, track, group, p_a, p_b, b_w, b_r, warm, a_r, p, a, b, r, w);
		std::future::pending::<()>().await
	});
	rx
}

/// Lite carries the resume point upstream, so the rest of the group and the next one
/// arrive in order.
#[moq_net_sim::test]
async fn lite_resumes_after_a_lagging_route_dies() {
	let version: Version = "moq-lite-07-wip".parse().unwrap();
	let mut rx = lagging_route_dies(version).await;
	assert_eq!(next(&mut rx).await, (1, payload(1, 2)));
	assert_eq!(next(&mut rx).await, (1, payload(1, 3)));
	assert_eq!(next(&mut rx).await, (2, payload(2, 0)));
}

macro_rules! route_flap_tests {
	($($name:ident: $version:literal,)*) => {
		$(
			#[moq_net_sim::test]
			async fn $name() {
				moq_net_sim::timeout(TEST_TIMEOUT, route_flaps($version))
					.await
					.expect("timed out");
			}
		)*
	};
}

route_flap_tests! {
	flaps_lite_07: "moq-lite-07-wip",
}
