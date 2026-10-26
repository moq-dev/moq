#![cfg(feature = "async")]

//! SPIKE (not for merge): what breaks when group IDs are sparse start timestamps
//! instead of contiguous sequences. Each scenario publishes groups whose ID is the
//! group's first PTS in the track's timescale (2 s GOPs at 1 kHz: 0, 2000, 4000, ...)
//! and reports what a subscriber receives, how much simulated time it took, and how
//! many bidi streams the subscribing side opened upstream.
//!
//! Run: `cargo test -p moq-net --features async --test integration time_addressing_spike -- --nocapture`

use crate::support;

use std::time::Duration;

use moq_net::track::{Anchor, Info, Range, Subscription};
use moq_net::{Hop, Timescale, Timestamp, Version};
use support::harness::{MockConnectOptions, MockPair, connect_mock};

const VERSIONS: &[&str] = &[
	"moq-lite-03",
	"moq-lite-06",
	"moq-lite-07-wip",
	"moq-transport-14",
	"moq-transport-18",
];
const GOP: u64 = 2000;
const GROUPS: u64 = 4;
const FOREVER: Duration = Duration::from_millis((1 << 53) - 1);
const WAIT: Duration = Duration::from_secs(10);

fn origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

struct Net {
	publisher: moq_net::origin::Producer,
	relay: Option<moq_net::origin::Producer>,
	client: moq_net::origin::Producer,
	upstream: MockPair,
	_downstream: Option<MockPair>,
	track: moq_net::track::Producer,
}

async fn net(version: &str, relayed: bool) -> Net {
	let version: Version = version.parse().unwrap();
	let publisher = origin(1);
	let relay = origin(2);
	let client = origin(3);
	let broadcast = publisher.create_broadcast("b").unwrap();
	let info = Info::default().with_timescale(Timescale::MILLI).with_max_age(FOREVER);
	let track = broadcast.create_track("t", info).unwrap();
	broadcast.announce(Default::default()).unwrap();
	std::mem::forget(broadcast);
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(publisher.consume());
	options.client_subscribe = Some(if relayed { relay.clone() } else { client.clone() });
	let upstream = connect_mock(options).await;
	let downstream = if relayed {
		let mut options = MockConnectOptions::new(version);
		options.server_publish = Some(relay.consume());
		options.client_subscribe = Some(client.clone());
		Some(connect_mock(options).await)
	} else {
		None
	};
	Net {
		publisher,
		relay: relayed.then_some(relay),
		client,
		upstream,
		_downstream: downstream,
		track,
	}
}

fn publish(track: &moq_net::track::Producer, ids: impl IntoIterator<Item = u64>) {
	for id in ids {
		let pts = id / stride() * GOP;
		let mut group = track.create_group(moq_net::group::Info { sequence: id }).unwrap();
		group.write_frame(Timestamp::from_millis(pts).unwrap(), b"key".as_ref()).unwrap();
		group
			.write_frame(Timestamp::from_millis(pts + GOP / 2).unwrap(), b"delta".as_ref())
			.unwrap();
		group.finish().unwrap();
	}
}

/// The ID step between consecutive groups: `GOP` (sparse, the default) or 1 (today's
/// contiguous baseline), set with `SPIKE_STRIDE`.
fn stride() -> u64 {
	std::env::var("SPIKE_STRIDE").map_or(GOP, |s| s.parse().unwrap())
}

fn ids() -> Vec<u64> {
	(0..GROUPS).map(|i| i * stride()).collect()
}

/// Read up to `want` groups, giving up after `WAIT` of simulated time with no progress.
async fn read(sub: &mut moq_net::track::Subscriber, want: usize) -> (Vec<u64>, Option<String>) {
	let mut got = Vec::new();
	while got.len() < want {
		match moq_clock::timeout(WAIT, sub.recv_group()).await {
			Err(_) => return (got, Some("stalled".into())),
			Ok(Ok(Some(mut group))) => {
				let frames = moq_clock::timeout(WAIT, async {
					let mut n = 0;
					while let Ok(Some(_)) = group.read_frame().await {
						n += 1;
					}
					n
				})
				.await
				.unwrap_or(usize::MAX);
				if frames != 2 {
					return (got, Some(format!("group {} had {frames} frames", group.sequence)));
				}
				got.push(group.sequence);
			}
			Ok(Ok(None)) => return (got, Some("ended".into())),
			Ok(Err(err)) => return (got, Some(format!("err {err:?}"))),
		}
	}
	(got, None)
}

async fn read_ordered(sub: moq_net::track::Subscriber, want: usize) -> (Vec<u64>, Option<String>) {
	let mut sub = sub.ordered();
	let mut got = Vec::new();
	while got.len() < want {
		match moq_clock::timeout(WAIT, sub.next_group()).await {
			Err(_) => return (got, Some("stalled".into())),
			Ok(Ok(Some(group))) => got.push(group.sequence),
			Ok(Ok(None)) => return (got, Some("ended".into())),
			Ok(Err(err)) => return (got, Some(format!("err {err:?}"))),
		}
	}
	(got, None)
}

fn report(scenario: &str, version: &str, relayed: bool, start: moq_clock::Instant, net: &Net, got: (Vec<u64>, Option<String>)) {
	let elapsed = moq_clock::now().duration_since(start);
	let bidis = net.upstream.client_transport.bidi_types();
	let mut counts = std::collections::BTreeMap::new();
	for kind in bidis {
		*counts.entry(kind).or_insert(0usize) += 1;
	}
	let bidis = counts;
	eprintln!(
		"SPIKE {scenario:<22} {version:<17} relayed={relayed:<5} got={:?} problem={:?} sim={:?} upstream_bidis={:?}",
		got.0, got.1, elapsed, bidis
	);
}

async fn remote(net: &Net) -> moq_net::broadcast::Consumer {
	net.client.consume().routed("b").await.unwrap();
	net.client.consume().request_broadcast("b", None).await.unwrap()
}

/// Subscribed before publishing: every group is live.
#[moq_clock::test]
async fn spike_live() {
	for version in VERSIONS {
		for relayed in [false, true] {
			for ordered in [false, true] {
				let net = net(version, relayed).await;
				let remote = remote(&net).await;
				let sub = remote
					.track("t")
					.unwrap()
					.subscribe(Subscription::default().with_max_delay(None))
					.await
					.unwrap();
				let start = moq_clock::now();
				let reader = async {
					match ordered {
						true => read_ordered(sub, GROUPS as usize).await,
						false => {
							let mut sub = sub;
							read(&mut sub, GROUPS as usize).await
						}
					}
				};
				let writer = async {
					for id in ids() {
						publish(&net.track, [id]);
						moq_clock::sleep(Duration::from_millis(GOP)).await;
					}
				};
				let (got, ()) = futures::join!(reader, writer);
				let name = if ordered { "live-ordered" } else { "live" };
				report(name, version, relayed, start, &net, got);
			}
		}
	}
}

/// Published before subscribing: the subscriber reaches back for history.
#[moq_clock::test]
async fn spike_history() {
	for version in VERSIONS {
		for relayed in [false, true] {
			for (name, range) in [
				("history-oldest", Range::oldest()),
				("history-absolute", Range::groups(stride()..3 * stride())),
				(
					"history-relative1",
					Range {
						start: Anchor::Relative(1),
						end: None,
					},
				),
			] {
				let net = net(version, relayed).await;
				publish(&net.track, ids());
				let remote = remote(&net).await;
				let start = moq_clock::now();
				let sub = remote
					.track("t")
					.unwrap()
					.subscribe(Subscription::default().with_max_delay(None).with_ranges([range]))
					.await;
				let mut sub = match sub {
					Ok(sub) => sub,
					Err(err) => {
						report(name, version, relayed, start, &net, (vec![], Some(format!("subscribe {err:?}"))));
						continue;
					}
				};
				let want = match name {
					"history-oldest" => GROUPS as usize,
					_ => 2,
				};
				let got = read(&mut sub, want).await;
				report(name, version, relayed, start, &net, got);
			}
		}
	}
}

/// A relay already serving the live edge is asked for the history below it: the hole
/// between what it holds and the requested start must be filled from upstream.
#[moq_clock::test]
async fn spike_warm_relay_fill() {
	for version in VERSIONS {
		let net = net(version, true).await;
		let relay = net.relay.clone().unwrap();
		publish(&net.track, ids());
		// The relay's live subscriber pulls only the newest group into its cache.
		relay.consume().routed("b").await.unwrap();
		let cached = relay.consume().request_broadcast("b", None).await.unwrap();
		let mut live = cached.track("t").unwrap().subscribe(Subscription::default().with_ranges([Range::live()])).await.unwrap();
		let first = moq_clock::timeout(WAIT, live.recv_group()).await;
		let first = first.ok().and_then(|g| g.ok()).flatten().map(|g| g.sequence);
		let remote = remote(&net).await;
		let start = moq_clock::now();
		let mut sub = remote
			.track("t")
			.unwrap()
			.subscribe(Subscription::default().with_max_delay(None).with_ranges([Range::groups(0..)]))
			.await
			.unwrap();
		let got = read(&mut sub, GROUPS as usize).await;
		eprintln!("SPIKE warm relay live subscriber first group {first:?}");
		report("warm-relay-fill", version, true, start, &net, got);
		drop(live);
	}
}

/// Fetch one group by ID through the relay, and an ID between two groups that was never
/// published.
#[moq_clock::test]
async fn spike_fetch() {
	for version in VERSIONS {
		for (name, id) in [("fetch-existing", stride()), ("fetch-between", stride() + 1)] {
			let net = net(version, true).await;
			publish(&net.track, ids());
			let remote = remote(&net).await;
			let start = moq_clock::now();
			let track = remote.track("t").unwrap();
			let got = match moq_clock::timeout(WAIT, track.fetch_group(id, None)).await {
				Err(_) => (vec![], Some("stalled".into())),
				Ok(Ok(group)) => (vec![group.sequence], None),
				Ok(Err(err)) => (vec![], Some(format!("err {err:?}"))),
			};
			report(name, version, true, start, &net, got);
		}
	}
}

/// Live subscriber with the default zero budget while groups arrive in order but the
/// publisher's track is aged by max_age: sparse IDs must not leave the cache unbounded.
#[moq_clock::test]
async fn spike_expiry() {
	for version in VERSIONS {
		let version_parsed: Version = version.parse().unwrap();
		let publisher = origin(1);
		let relay = origin(2);
		let broadcast = publisher.create_broadcast("b").unwrap();
		let info = Info::default()
			.with_timescale(Timescale::MILLI)
			.with_max_age(Duration::from_secs(5));
		let track = broadcast.create_track("t", info).unwrap();
		broadcast.announce(Default::default()).unwrap();
		let mut options = MockConnectOptions::new(version_parsed);
		options.server_publish = Some(publisher.consume());
		options.client_subscribe = Some(relay.clone());
		let _upstream = connect_mock(options).await;
		relay.consume().routed("b").await.unwrap();
		let remote = relay.consume().request_broadcast("b", None).await.unwrap();
		let mut sub = remote.track("t").unwrap().subscribe(Subscription::default()).await.unwrap();
		let writer = async {
			for i in 0..20 {
				publish(&track, [i * stride()]);
				moq_clock::sleep(Duration::from_millis(GOP)).await;
			}
		};
		let reader = read(&mut sub, 20);
		let (got, ()) = futures::join!(reader, writer);
		// Now ask the relay for the oldest group it still serves.
		let mut late = remote
			.track("t")
			.unwrap()
			.subscribe(Subscription::default().with_max_delay(None).with_ranges([Range::groups(0..)]))
			.await
			.unwrap();
		let first = moq_clock::timeout(Duration::from_secs(1), late.recv_group())
			.await
			.ok()
			.and_then(|g| g.ok())
			.flatten()
			.map(|g| g.sequence);
		eprintln!(
			"SPIKE expiry {version:<17} live got {} groups problem={:?}; late oldest-served={first:?} (max_age 5s, newest {})",
			got.0.len(),
			got.1,
			19 * stride()
		);
		drop(publisher);
	}
}
