#![cfg(feature = "async")]

//! A catalog's one open group (a snapshot at frame 0, deltas after) survives its
//! route changing in the middle of that group, end to end over real sessions.
//!
//! A publisher `P` is pulled by two relays `A` and `B` carrying its epoch, and `R`
//! reads through `A` until the route changes mid-group. The reader already on `R`
//! continues with the next delta, and every reader after the change (a fresh one, a
//! returning one, and a burst of short ones) still starts from the snapshot. The
//! replacement's copy of the group was opened mid-group for the continuing reader,
//! so a reader that starts at frame 0 must not be refused as lagged and left with
//! no catalog.

use crate::support;

use std::time::Duration;

use futures::{StreamExt, channel::mpsc};
use moq_net::{Error, Hop, Timestamp, Version, origin, track};
use support::harness::{MockConnectOptions, MockPair, connect_mock};

const TEST_TIMEOUT: Duration = Duration::from_secs(300);

/// How long any one read may take, in virtual time.
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Short readers in each burst, re-reading the catalog and leaving.
const BURST: usize = 4;

/// When the returning reader comes back: twice a relay's 30 s idle linger.
const RETURN_AFTER: Duration = Duration::from_secs(62);

fn timed() -> track::Info {
	track::Info::default().with_timescale(moq_net::Timescale::MILLI)
}

fn produce_origin(hop: u64) -> origin::Producer {
	let (producer, driver) = origin::Producer::new(origin::Config::new(Hop::new(hop).unwrap()));
	support::harness::spawn(driver);
	producer
}

async fn link(version: Version, from: &origin::Producer, to: &origin::Producer, cost: Option<u64>) -> MockPair {
	let mut options = MockConnectOptions::new(version);
	options.server_publish = Some(from.consume());
	options.client_subscribe = Some(to.clone());
	options.cost = cost;
	connect_mock(options).await
}

fn abort(pair: MockPair) {
	pair.server.abort(Error::Cancel);
	pair.client.abort(Error::Cancel);
}

/// Let every task settle. Time is paused, so this returns once the runtime is idle.
async fn settle() {
	kio::clock::sleep(Duration::from_millis(500)).await;
}

/// How the serving route changes.
#[derive(Clone, Copy, Debug)]
enum Trigger {
	/// The session `A -> R` dies; `B` takes over.
	Disconnect,
	/// `A` loses `P` and withdraws the broadcast while its session to `R` stays up.
	Unannounce,
	/// `R` gains a direct route to `P` that beats `A`.
	BetterRoute,
	/// The session `A -> R` dies with no standby, then reconnects. The broadcast closes
	/// with its last route, so a viewer's subscription ends rather than resuming.
	Reconnect,
}

/// Who holds the catalog on `R` while the route changes.
#[derive(Clone, Copy, Debug)]
enum Holder {
	/// A viewer stays subscribed and must see the next delta exactly once.
	Viewer,
	/// Nobody: the last reader left, so the change lands on a parked track.
	Parked,
}

struct Topology {
	version: Version,
	publisher: origin::Producer,
	relay_a: origin::Producer,
	relay_b: origin::Producer,
	subscriber: origin::Producer,
	group: moq_net::group::Producer,
	p_to_a: Option<MockPair>,
	a_to_r: Option<MockPair>,
	links: Vec<MockPair>,
	_broadcast: moq_net::origin::Publication,
	_track: track::Producer,
	/// Every payload written to the open group so far.
	written: Vec<String>,
}

impl Topology {
	async fn new(version: Version) -> Self {
		let publisher = produce_origin(1);
		let relay_a = produce_origin(2);
		let relay_b = produce_origin(3);
		let subscriber = produce_origin(4);

		let broadcast = publisher.create_broadcast("live").unwrap();
		let track = broadcast.create_track("catalog.json", timed()).unwrap();
		broadcast
			.announce(origin::Route::default().with_epoch(moq_net::Epoch::mint()))
			.unwrap();
		let group = track.append_group().unwrap();

		let p_to_a = link(version, &publisher, &relay_a, None).await;
		let p_to_b = link(version, &publisher, &relay_b, None).await;
		let a_to_r = link(version, &relay_a, &subscriber, None).await;
		crate::support::announced(&subscriber.consume(), "live").await;

		let mut topology = Self {
			version,
			publisher,
			relay_a,
			relay_b,
			subscriber,
			group,
			p_to_a: Some(p_to_a),
			a_to_r: Some(a_to_r),
			links: vec![p_to_b],
			_broadcast: broadcast,
			_track: track,
			written: Vec::new(),
		};
		topology.write("snapshot");
		topology
	}

	fn write(&mut self, payload: &str) {
		let at = Timestamp::from_millis(self.written.len() as u64 * 10).unwrap();
		self.group.write_frame(at, payload.as_bytes()).unwrap();
		self.written.push(payload.to_string());
	}

	/// Connect `B -> R`, priced worse than `A` so it only stands by.
	async fn standby(&mut self) {
		let b_to_r = link(self.version, &self.relay_b, &self.subscriber, Some(1_000)).await;
		self.links.push(b_to_r);
		settle().await;
	}

	async fn trigger(&mut self, trigger: Trigger) {
		match trigger {
			Trigger::Disconnect => abort(self.a_to_r.take().unwrap()),
			Trigger::Unannounce => abort(self.p_to_a.take().unwrap()),
			Trigger::BetterRoute => {
				let direct = link(self.version, &self.publisher, &self.subscriber, None).await;
				self.links.push(direct);
			}
			Trigger::Reconnect => {
				abort(self.a_to_r.take().unwrap());
				settle().await;
				let a_to_r = link(self.version, &self.relay_a, &self.subscriber, None).await;
				self.a_to_r = Some(a_to_r);
			}
		}
		settle().await;
	}

	/// A new reader on `R`: a fresh broadcast request and a floorless subscription.
	async fn reader(&self) -> Reader {
		let remote = kio::clock::timeout(READ_TIMEOUT, self.subscriber.consume().request("live", None))
			.await
			.expect("request hung")
			.expect("request refused");
		let sub = kio::clock::timeout(READ_TIMEOUT, remote.track("catalog.json").unwrap().subscribe(None))
			.await
			.expect("subscribe hung")
			.expect("subscribe refused");
		Reader::new(sub)
	}
}

/// Reports every frame a subscription delivers, as `group:payload`, or the error that
/// ended it.
struct Reader {
	rx: mpsc::UnboundedReceiver<Result<String, String>>,
	/// Owns the subscription, so dropping the reader leaves the track.
	task: kio::clock::sim::JoinHandle<()>,
}

impl Drop for Reader {
	fn drop(&mut self) {
		self.task.abort();
	}
}

impl Reader {
	fn new(mut sub: track::Subscriber) -> Self {
		let (tx, rx) = mpsc::unbounded();
		let task = kio::clock::sim::spawn(async move {
			loop {
				let mut group = match sub.recv_group().await {
					Ok(Some(group)) => group,
					Ok(None) => return,
					Err(err) => {
						let _ = tx.unbounded_send(Err(format!("track: {err}")));
						return;
					}
				};
				loop {
					let sent = match group.read_frame().await {
						Ok(Some(frame)) => tx.unbounded_send(Ok(format!(
							"{}:{}",
							group.sequence,
							String::from_utf8_lossy(&frame.payload)
						))),
						Ok(None) => break,
						Err(err) => {
							let _ = tx.unbounded_send(Err(format!("group {} frame: {err}", group.sequence)));
							break;
						}
					};
					if sent.is_err() {
						return;
					}
				}
			}
		});
		Self { rx, task }
	}

	/// The next `n` frames, failing on an error or a hang.
	async fn take(&mut self, n: usize, who: &str) -> Vec<String> {
		let mut frames = Vec::new();
		while frames.len() < n {
			match kio::clock::timeout(READ_TIMEOUT, self.rx.next()).await {
				Ok(Some(Ok(frame))) => frames.push(frame),
				Ok(Some(Err(err))) => panic!("{who}: failed after {frames:?}: {err}"),
				Ok(None) => panic!("{who}: ended after {frames:?}"),
				Err(_) => panic!("{who}: hung after {frames:?}"),
			}
		}
		frames
	}

	/// Expect the whole open group so far, from the snapshot.
	async fn expect_all(&mut self, topology: &Topology, who: &str) {
		let expected: Vec<String> = topology.written.iter().map(|payload| format!("0:{payload}")).collect();
		assert_eq!(self.take(expected.len(), who).await, expected, "{who}");
	}

	/// The subscription fails or ends, rather than hanging or delivering more.
	async fn expect_end(&mut self, who: &str) {
		match kio::clock::timeout(READ_TIMEOUT, self.rx.next()).await {
			Ok(None | Some(Err(_))) => {}
			Ok(Some(Ok(frame))) => panic!("{who}: delivered {frame:?} past its route"),
			Err(_) => panic!("{who}: hung instead of ending"),
		}
	}

	/// Nothing more has arrived: no duplicate frame or group trails.
	fn expect_quiet(&mut self, who: &str) {
		if let Ok(extra) = self.rx.try_recv() {
			panic!("{who}: trailing delivery {extra:?}");
		}
	}
}

/// Short readers that each read the catalog from its snapshot and leave.
async fn burst(topology: &Topology, label: &str) {
	for i in 0..BURST {
		let who = format!("{label} reader {i}");
		let mut reader = topology.reader().await;
		reader.expect_all(topology, &who).await;
	}
}

async fn handover(version: &str, trigger: Trigger, holder: Holder) {
	let version: Version = version.parse().unwrap();
	let mut topology = Topology::new(version).await;
	let tag = format!("{version} {trigger:?} {holder:?}");

	let mut viewer = topology.reader().await;
	viewer.expect_all(&topology, &format!("{tag}: viewer")).await;
	topology.write("delta1");
	assert_eq!(viewer.take(1, &tag).await, ["0:delta1"], "{tag}: viewer");

	// Readers come and go inside the linger, re-splicing the catalog.
	burst(&topology, &format!("{tag}: before")).await;

	if !matches!(trigger, Trigger::Reconnect) {
		topology.standby().await;
	}
	let mut viewer = match holder {
		Holder::Viewer => Some(viewer),
		Holder::Parked => {
			drop(viewer);
			settle().await;
			None
		}
	};

	topology.trigger(trigger).await;
	if let (Trigger::Reconnect, Some(mut ended)) = (trigger, viewer.take()) {
		ended.expect_end(&format!("{tag}: viewer")).await;
	}
	topology.write("delta2");

	if let Some(viewer) = &mut viewer {
		assert_eq!(viewer.take(1, &tag).await, ["0:delta2"], "{tag}: viewer continues");
	}

	// A fresh reader after the change starts from the snapshot.
	let mut fresh = topology.reader().await;
	fresh.expect_all(&topology, &format!("{tag}: fresh")).await;

	// Short readers again, then one that leaves and returns after the next delta.
	burst(&topology, &format!("{tag}: after")).await;
	drop(fresh);
	settle().await;
	topology.write("delta3");
	if let Some(viewer) = &mut viewer {
		assert_eq!(viewer.take(1, &tag).await, ["0:delta3"], "{tag}: viewer continues");
	}
	// Returning well past the relays' idle linger, so no copy of the head is warm.
	kio::clock::sleep(RETURN_AFTER).await;
	let mut returning = topology.reader().await;
	returning.expect_all(&topology, &format!("{tag}: returning")).await;

	// Everyone still reading follows the live edge, exactly once.
	topology.write("delta4");
	assert_eq!(returning.take(1, &tag).await, ["0:delta4"], "{tag}: returning");
	if let Some(viewer) = &mut viewer {
		assert_eq!(viewer.take(1, &tag).await, ["0:delta4"], "{tag}: viewer");
	}
	settle().await;
	returning.expect_quiet(&format!("{tag}: returning"));
	if let Some(viewer) = &mut viewer {
		viewer.expect_quiet(&format!("{tag}: viewer"));
	}
}

/// What happens on `R` before a reader joins, long after, with nothing in between.
#[derive(Clone, Copy, Debug)]
enum Before {
	/// A viewer stays while a direct route beats `A` mid-group.
	RouteChange,
	/// A reader leaves, so the catalog parks, and another resumes it.
	Resume,
}

/// The minimal sequences the release-line splice left a late reader stuck on: it
/// subscribes, and nothing ever arrives.
async fn late_reader(version: &str, before: Before) {
	let version: Version = version.parse().unwrap();
	let mut topology = Topology::new(version).await;
	let tag = format!("{version} {before:?}");

	let mut first = topology.reader().await;
	first.expect_all(&topology, &format!("{tag}: first")).await;
	let _holder = match before {
		Before::RouteChange => {
			topology.trigger(Trigger::BetterRoute).await;
			first
		}
		Before::Resume => {
			settle().await;
			drop(first);
			kio::clock::sleep(Duration::from_secs(5)).await;
			let mut resumed = topology.reader().await;
			resumed.expect_all(&topology, &format!("{tag}: resumed")).await;
			resumed
		}
	};

	kio::clock::sleep(RETURN_AFTER).await;
	let mut late = topology.reader().await;
	late.expect_all(&topology, &format!("{tag}: late")).await;
}

/// A short reader fetches the open group just after the last subscriber left, while
/// the newest delta is still on its way: the fetch must deliver it, not stop at the
/// frames the departing subscription had cached.
async fn fetch_after_leave(version: &str, pause: Duration) {
	let version: Version = version.parse().unwrap();
	let mut topology = Topology::new(version).await;
	let tag = format!("{version} {pause:?}");

	let mut reader = topology.reader().await;
	reader.expect_all(&topology, &format!("{tag}: reader")).await;
	topology.write("delta1");
	drop(reader);
	kio::clock::sleep(pause).await;

	let remote = topology.subscriber.consume().request("live", None).await.unwrap();
	let fetch = remote.track("catalog.json").unwrap().fetch_group(0, None);
	let mut group = kio::clock::timeout(READ_TIMEOUT, fetch)
		.await
		.unwrap_or_else(|_| panic!("{tag}: fetch hung"))
		.unwrap_or_else(|err| panic!("{tag}: fetch refused: {err}"));
	let mut got = Vec::new();
	while got.len() < topology.written.len() {
		match kio::clock::timeout(READ_TIMEOUT, group.read_frame()).await {
			Ok(Ok(Some(frame))) => got.push(String::from_utf8_lossy(&frame.payload).to_string()),
			Ok(Ok(None)) => panic!("{tag}: fetch ended after {got:?}"),
			Ok(Err(err)) => panic!("{tag}: fetch failed after {got:?}: {err}"),
			Err(_) => panic!("{tag}: fetch hung after {got:?}"),
		}
	}
	assert_eq!(got, topology.written, "{tag}");
}

macro_rules! handover_tests {
	($($name:ident: $version:literal,)*) => {
		$(
			mod $name {
				use super::*;

				async fn run(trigger: Trigger, holder: Holder) {
					kio::clock::timeout(TEST_TIMEOUT, handover($version, trigger, holder))
						.await
						.expect("timed out");
				}

				#[kio::test]
				async fn disconnect_with_viewer() {
					run(Trigger::Disconnect, Holder::Viewer).await;
				}

				#[kio::test]
				async fn disconnect_parked() {
					run(Trigger::Disconnect, Holder::Parked).await;
				}

				#[kio::test]
				async fn unannounce_with_viewer() {
					run(Trigger::Unannounce, Holder::Viewer).await;
				}

				#[kio::test]
				async fn unannounce_parked() {
					run(Trigger::Unannounce, Holder::Parked).await;
				}

				#[kio::test]
				async fn better_route_with_viewer() {
					run(Trigger::BetterRoute, Holder::Viewer).await;
				}

				#[kio::test]
				async fn better_route_parked() {
					run(Trigger::BetterRoute, Holder::Parked).await;
				}

				#[kio::test]
				async fn reconnect_with_viewer() {
					run(Trigger::Reconnect, Holder::Viewer).await;
				}

				#[kio::test]
				async fn reconnect_parked() {
					run(Trigger::Reconnect, Holder::Parked).await;
				}

				#[kio::test]
				async fn late_reader_after_route_change() {
					kio::clock::timeout(TEST_TIMEOUT, late_reader($version, Before::RouteChange))
						.await
						.expect("timed out");
				}

				#[kio::test]
				async fn late_reader_after_resume() {
					kio::clock::timeout(TEST_TIMEOUT, late_reader($version, Before::Resume))
						.await
						.expect("timed out");
				}
			}
		)*
	};
}

// Only an epoch lets `R` resume onto another route; older wires end the subscription
// with its route instead (see `route_change`).
handover_tests! {
	lite_07: "moq-lite-07-wip",
}

macro_rules! fetch_tests {
	($($name:ident: $version:literal,)*) => {
		$(
			mod $name {
				use super::*;

				#[kio::test]
				async fn fetch_right_after_leave() {
					kio::clock::timeout(TEST_TIMEOUT, fetch_after_leave($version, Duration::ZERO))
						.await
						.expect("timed out");
				}

				#[kio::test]
				async fn fetch_after_leave_settles() {
					kio::clock::timeout(TEST_TIMEOUT, fetch_after_leave($version, Duration::from_millis(500)))
						.await
						.expect("timed out");
				}
			}
		)*
	};
}

// A fetch needs no epoch. moq-transport is left out: its copy goes idle on the leave
// and still answers from the open group it can no longer feed.
fetch_tests! {
	fetch_lite_06: "moq-lite-06",
	fetch_lite_07: "moq-lite-07-wip",
}
