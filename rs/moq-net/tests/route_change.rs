//! A subscription survives its route changing, end to end over real sessions.
//!
//! A publisher `P` is pulled by two relays `A` and `B`, both of which re-advertise
//! it to the subscribing relay `R`. A path is one broadcast whoever serves it, so `R`
//! may resume a subscription served through one onto the other. The reader on `R`
//! must see every frame exactly once, in order, whether the route changes between
//! groups or in the middle of one, and however it changes.

mod support;

use std::time::Duration;

use moq_net::{Error, Hop, Timestamp, Version, broadcast, origin, track};
use support::harness::{MockConnectOptions, MockPair, connect_mock};
use tokio::sync::mpsc;

const TEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Frames per group.
const FRAMES: u64 = 4;

fn produce_origin(hop: u64) -> origin::Producer {
	let (producer, driver) = origin::Producer::new(origin::Config::new(Hop::new(hop).unwrap()));
	tokio::spawn(support::harness::run(driver));
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
	tokio::time::sleep(Duration::from_millis(500)).await;
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
		broadcast.announce(Default::default()).unwrap();

		let p_to_a = link(version, &publisher, &relay_a).await;
		let p_to_b = link(version, &publisher, &relay_b).await;
		let a_to_r = link(version, &relay_a, &subscriber).await;

		let consumer = subscriber.consume();
		consumer.routed("live").await.unwrap();
		let remote = consumer.request_broadcast("live").await.unwrap();
		let prefs = track::Subscription::default().with_max_age(Duration::from_secs(60));
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
				let direct = link(self.version, &self.publisher, &self.subscriber).await;
				self._links.push(direct);
			}
		}
		settle().await;
	}
}

/// Read every group in full, reporting each frame as it arrives.
fn read(mut sub: track::Subscriber) -> mpsc::UnboundedReceiver<(u64, moq_net::Result<Vec<u8>>)> {
	let (tx, rx) = mpsc::unbounded_channel();
	tokio::spawn(async move {
		loop {
			let mut group = match sub.recv_group().await {
				Ok(Some(group)) => group,
				Ok(None) => return,
				Err(err) => {
					let _ = tx.send((u64::MAX, Err(err)));
					return;
				}
			};
			loop {
				match group.read_frame().await {
					Ok(Some(frame)) => {
						if tx.send((group.sequence, Ok(frame.payload.to_vec()))).is_err() {
							return;
						}
					}
					Ok(None) => break,
					Err(err) => {
						let _ = tx.send((group.sequence, Err(err)));
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
	let (group, frame) = tokio::time::timeout(Duration::from_secs(10), rx.recv())
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

macro_rules! route_change_tests {
	($($name:ident: $version:literal,)*) => {
		$(
			mod $name {
				use super::*;

				async fn run(trigger: Trigger, position: Position) {
					tokio::time::timeout(TEST_TIMEOUT, route_change($version, trigger, position))
						.await
						.expect("timed out");
				}

				#[tokio::test(start_paused = true)]
				async fn disconnect_between_groups() {
					run(Trigger::Disconnect, Position::BetweenGroups).await;
				}

				#[tokio::test(start_paused = true)]
				async fn disconnect_mid_group() {
					run(Trigger::Disconnect, Position::MidGroup).await;
				}

				#[tokio::test(start_paused = true)]
				async fn unannounce_between_groups() {
					run(Trigger::Unannounce, Position::BetweenGroups).await;
				}

				#[tokio::test(start_paused = true)]
				async fn unannounce_mid_group() {
					run(Trigger::Unannounce, Position::MidGroup).await;
				}

				#[tokio::test(start_paused = true)]
				async fn better_route_between_groups() {
					run(Trigger::BetterRoute, Position::BetweenGroups).await;
				}

				#[tokio::test(start_paused = true)]
				async fn better_route_mid_group() {
					run(Trigger::BetterRoute, Position::MidGroup).await;
				}
			}
		)*
	};
}

route_change_tests! {
	lite_06: "moq-lite-06",
	lite_05: "moq-lite-05",
	lite_04: "moq-lite-04",
	ietf_19: "moq-transport-19",
	ietf_22: "moq-transport-22",
}
