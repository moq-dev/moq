//! An endpoint linked to two CDNs uses its preferred one and fails over to the next.
//!
//! A publisher `P` pushes one broadcast, under one epoch, to CDN relays `A` and `B`. A
//! subscriber `S` links to both, ranking `A` first with
//! [`origin::Producer::with_preference`] while pricing `B` cheaper, so only the
//! preference puts `A` ahead. Losing `A` moves `S` to `B`, and `A` returning moves it
//! back. Between lite-07 routes with the same epoch each move resumes the subscription.
//! A moq-transport `B` carries no epoch, so each move is a restart and a re-request.

mod support;

use std::time::Duration;

use futures::{StreamExt, channel::mpsc};
use moq_net::{Error, Hop, Timestamp, Version, announce, broadcast, origin, track};
use support::harness::{MockConnectOptions, MockPair, connect_mock};

const TEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Frames per group.
const FRAMES: u64 = 4;

/// The preferred CDN's hop.
const A: u64 = 2;

fn produce_origin(hop: u64) -> origin::Producer {
	let (producer, driver) = origin::Producer::new(origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

/// Let every task settle. Time is paused, so this returns once the runtime is idle.
async fn settle() {
	moq_net_sim::sleep(Duration::from_millis(500)).await;
}

/// `P` pushes everything it publishes to `cdn`.
async fn push(version: Version, publisher: &origin::Producer, cdn: &origin::Producer) -> MockPair {
	let mut options = MockConnectOptions::new(version);
	options.client_publish = Some(publisher.consume());
	options.server_subscribe = Some(cdn.clone());
	connect_mock(options).await
}

/// `S` dials `cdn`, ranking it `preference` and pricing it `cost`.
async fn dial(
	version: Version,
	cdn: &origin::Producer,
	subscriber: &origin::Producer,
	preference: u32,
	cost: u64,
) -> MockPair {
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(cdn.consume());
	options.client_subscribe = Some(subscriber.clone().with_preference(preference));
	options.cost = Some(cost);
	connect_mock(options).await
}

fn kill(pair: MockPair) {
	pair.server.abort(Error::Cancel);
	pair.client.abort(Error::Cancel);
}

fn payload(group: u64, frame: u64) -> Vec<u8> {
	format!("{group}.{frame}").into_bytes()
}

fn timestamp(group: u64, frame: u64) -> Timestamp {
	Timestamp::from_micros(1_000_000 + group * 100_000 + frame * 1_000).unwrap()
}

/// A frame as the reader saw it.
type Delivery = (u64, moq_net::Result<(Option<Timestamp>, Vec<u8>)>);

/// Read every group in full, reporting each frame as it arrives.
fn read(mut sub: track::Subscriber) -> mpsc::UnboundedReceiver<Delivery> {
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
						if tx
							.unbounded_send((group.sequence, Ok((frame.timestamp, frame.payload.to_vec()))))
							.is_err()
						{
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

/// The next frame the reader reports, failing on an error or a hang.
async fn next(rx: &mut mpsc::UnboundedReceiver<Delivery>) -> (u64, Option<Timestamp>, Vec<u8>) {
	let (group, frame) = moq_net_sim::timeout(Duration::from_secs(10), rx.next())
		.await
		.expect("reader hung")
		.expect("reader ended");
	let (timestamp, payload) = frame.unwrap_or_else(|err| panic!("group {group} failed: {err}"));
	(group, timestamp, payload)
}

/// Whether the route passes through the preferred CDN.
fn via_a(announce: &announce::Event) -> bool {
	let route = match announce {
		announce::Event::Start(a) | announce::Event::Update(a) | announce::Event::Restart(a) => &a.route,
		announce::Event::End(_) => return false,
	};
	route.hops.iter().any(|hop| *hop == Hop::new(A).unwrap())
}

struct Topology {
	publisher: origin::Producer,
	cdn_a: origin::Producer,
	cdn_b: origin::Producer,
	subscriber: origin::Producer,
	track: track::Producer,
	_broadcast: broadcast::Producer,
	_pushes: Vec<MockPair>,
	s_to_a: Option<MockPair>,
	s_to_b: MockPair,
	sequence: u64,
}

impl Topology {
	/// `P` pushes `live` to `A` over lite-07 and to `B` over `secondary`; `S` dials both,
	/// preferring `A` but pricing `B` cheaper.
	async fn new(secondary: Version) -> Self {
		let lite: Version = "moq-lite-07-wip".parse().unwrap();
		let publisher = produce_origin(1);
		let cdn_a = produce_origin(A);
		let cdn_b = produce_origin(3);
		let subscriber = produce_origin(4);

		let broadcast = publisher.create_broadcast("live").unwrap();
		let track = broadcast.create_track("video", None).unwrap();
		broadcast
			.announce(origin::Route::default().with_epoch(moq_net::Epoch::mint()))
			.unwrap();

		let pushes = vec![
			push(lite, &publisher, &cdn_a).await,
			push(secondary, &publisher, &cdn_b).await,
		];
		let s_to_a = dial(lite, &cdn_a, &subscriber, 0, 100).await;
		let s_to_b = dial(secondary, &cdn_b, &subscriber, 1, 0).await;
		settle().await;

		Self {
			publisher,
			cdn_a,
			cdn_b,
			subscriber,
			track,
			_broadcast: broadcast,
			_pushes: pushes,
			s_to_a: Some(s_to_a),
			s_to_b,
			sequence: 0,
		}
	}

	async fn subscribe(&self) -> mpsc::UnboundedReceiver<Delivery> {
		let remote = self.subscriber.consume().request_broadcast("live", None).await.unwrap();
		let prefs = track::Subscription::default().with_max_delay(Duration::from_secs(60));
		read(remote.track("video").unwrap().subscribe(prefs).await.unwrap())
	}

	/// Write one full group, returning what the reader should see.
	fn group(&mut self) -> Vec<(u64, Vec<u8>)> {
		let sequence = self.sequence;
		self.sequence += 1;
		let mut group = self.track.append_group().unwrap();
		let mut written = Vec::new();
		for frame in 0..FRAMES {
			group
				.write_frame(timestamp(sequence, frame), payload(sequence, frame))
				.unwrap();
			written.push((sequence, payload(sequence, frame)));
		}
		group.finish().unwrap();
		written
	}

	/// Write a group while `B` holds back everything it sends `S`: it arrives only if
	/// `A` serves it.
	async fn group_through_a(
		&mut self,
		rx: &mut mpsc::UnboundedReceiver<Delivery>,
	) -> Vec<(u64, Vec<u8>, Option<Timestamp>)> {
		self.s_to_b.server_transport.hold_unis();
		let written = self.group();
		let mut seen = Vec::new();
		for _ in &written {
			let (group, timestamp, payload) = next(rx).await;
			seen.push((group, payload, timestamp));
		}
		self.s_to_b.server_transport.release_unis();
		seen
	}

	/// Write a group and read it back, whichever CDN serves it.
	async fn group_anywhere(
		&mut self,
		rx: &mut mpsc::UnboundedReceiver<Delivery>,
	) -> Vec<(u64, Vec<u8>, Option<Timestamp>)> {
		let written = self.group();
		let mut seen = Vec::new();
		for _ in &written {
			let (group, timestamp, payload) = next(rx).await;
			seen.push((group, payload, timestamp));
		}
		seen
	}

	async fn lose_a(&mut self) {
		kill(self.s_to_a.take().unwrap());
		settle().await;
	}

	async fn restore_a(&mut self) {
		let lite: Version = "moq-lite-07-wip".parse().unwrap();
		self.s_to_a = Some(dial(lite, &self.cdn_a, &self.subscriber, 0, 100).await);
		settle().await;
	}
}

/// Every frame once, in order, with no timestamp rewinding.
fn assert_continuous(seen: &[(u64, Vec<u8>, Option<Timestamp>)], groups: u64) {
	let expected: Vec<_> = (0..groups)
		.flat_map(|group| (0..FRAMES).map(move |frame| (group, payload(group, frame))))
		.collect();
	let frames: Vec<_> = seen.iter().map(|(group, frame, _)| (*group, frame.clone())).collect();
	assert_eq!(frames, expected);
	for pair in seen.windows(2) {
		assert!(
			pair[0].2.expect("timed") < pair[1].2.expect("timed"),
			"timestamp rewound at {pair:?}"
		);
	}
}

/// Two lite-07 CDNs under one epoch: `S` reads through `A` although `B` is cheaper,
/// resumes on `B` when `A` goes, and moves back once `A` returns, every frame once.
#[moq_net_sim::test]
async fn preferred_cdn_fails_over_and_back() {
	moq_net_sim::timeout(TEST_TIMEOUT, async {
		let mut topology = Topology::new("moq-lite-07-wip".parse().unwrap()).await;
		let mut announced = topology.subscriber.consume().announced();
		let start = announced.next().await.expect("announced");
		assert!(
			matches!(start, announce::Event::Start(_)) && via_a(&start),
			"the preferred CDN wins over the cheaper one: {start:?}"
		);

		let mut rx = topology.subscribe().await;
		let mut seen = topology.group_through_a(&mut rx).await;

		topology.lose_a().await;
		let update = announced.next().await.expect("announced");
		assert!(
			matches!(update, announce::Event::Update(_)) && !via_a(&update),
			"same epoch over B: {update:?}"
		);
		seen.extend(topology.group_anywhere(&mut rx).await);

		topology.restore_a().await;
		let update = announced.next().await.expect("announced");
		assert!(
			matches!(update, announce::Event::Update(_)) && via_a(&update),
			"back on A: {update:?}"
		);
		seen.extend(topology.group_through_a(&mut rx).await);

		assert_continuous(&seen, 3);
		settle().await;
		assert!(rx.try_recv().is_err(), "trailing delivery");
		drop((topology.publisher, topology.cdn_b));
	})
	.await
	.expect("timed out");
}

/// A moq-transport `B` carries no epoch, so `S` cannot resume onto it: losing `A`
/// restarts the path and ends the subscription, and a re-request lands on `B`. `A`
/// returning is another restart, and a re-request lands back on `A`.
#[moq_net_sim::test]
async fn transport_secondary_restarts() {
	moq_net_sim::timeout(TEST_TIMEOUT, async {
		let mut topology = Topology::new("moq-transport-22".parse().unwrap()).await;
		let mut announced = topology.subscriber.consume().announced();
		let start = announced.next().await.expect("announced");
		assert!(via_a(&start), "{start:?}");

		let mut rx = topology.subscribe().await;
		let seen = topology.group_through_a(&mut rx).await;
		assert_continuous(&seen, 1);

		topology.lose_a().await;
		let restart = announced.next().await.expect("announced");
		assert!(
			matches!(restart, announce::Event::Restart(_)) && !via_a(&restart),
			"no epoch over B: {restart:?}"
		);
		topology.group();
		settle().await;
		match moq_net_sim::timeout(Duration::from_secs(10), rx.next())
			.await
			.expect("reader hung")
		{
			None | Some((_, Err(_))) => {}
			Some((group, Ok(_))) => panic!("resumed onto B at group {group}"),
		}

		// A re-request lands on B, the only CDN left. It may start at the latest group,
		// written while nobody was subscribed, before reaching the next one.
		let mut rx = topology.subscribe().await;
		topology.group();
		while next(&mut rx).await.0 != 2 {}

		topology.restore_a().await;
		let restart = announced.next().await.expect("announced");
		assert!(
			matches!(restart, announce::Event::Restart(_)) && via_a(&restart),
			"back on A: {restart:?}"
		);
		// A re-request lands on A: the next group arrives while B holds back its own.
		let mut rx = topology.subscribe().await;
		topology.s_to_b.server_transport.hold_unis();
		topology.group();
		while next(&mut rx).await.0 != 3 {}
		topology.s_to_b.server_transport.release_unis();
	})
	.await
	.expect("timed out");
}
