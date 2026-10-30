//! Cost of one merged group frame from [`moq_stats::aggregate`], swept over live nodes and
//! nodes that have come and gone, with and without the grace that folds departed nodes away.
//! Compare known metadata with one touched publisher losing and recovering its epoch,
//! and with live nodes that resumed after their open pairs were closed.
//!
//! Each node reports the same broadcast paths, like a restarted node under a new name. A bounded
//! aggregate's cost follows the live nodes; an unbounded one (`grace` never elapsing) also pays for
//! every departed node on each frame. Time is paused, so the churn ages past the grace instantly.
//!
//! Run `cargo bench -p moq-stats --bench aggregate` and compare the table across revisions.

#![expect(
	clippy::disallowed_methods,
	clippy::disallowed_types,
	reason = "custom timings measure the real time the work took, not the ambient clock"
)]

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use moq_net::{Epoch, origin};
use moq_stats::{Group, Traffic, aggregate, group_track};

/// Broadcast paths each node reports.
const KEYS: usize = 16;

/// Frames measured per configuration, after one warm-up frame.
const FRAMES: u32 = 64;

/// The grace configured when folding; the churn ages past it before measuring.
const GRACE: Duration = Duration::from_secs(1);

/// A group frame as the producer writes it.
#[derive(serde::Serialize)]
struct Body {
	epoch: String,
	tiers: BTreeMap<String, Group>,
}

/// A hand-published node stats broadcast with one group track carrying
/// default-tier egress.
struct Node {
	source: moq_net::origin::Publication,
	epoch: Epoch,
	known: bool,
	group: moq_json::snapshot::Producer<Body>,
	frame: Body,
}

impl Node {
	fn new(origin: &origin::Producer, name: &str) -> Self {
		let source = origin.create_broadcast(format!(".stats/node/{name}").as_str()).unwrap();
		let epoch = Epoch::mint();
		source
			.announce(origin::Route::default().with_epoch(epoch.clone()))
			.unwrap();
		let track = source
			.create_track(
				group_track(moq_net::Path::from_static("acme")),
				moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI),
			)
			.unwrap();
		let config = moq_json::snapshot::Config::default().with_deltas(false);
		let mut group = Group::default();
		for key in 0..KEYS {
			let mut traffic = Traffic::default();
			traffic.subscriptions_started = 1;
			group.paths.entry(format!("acme/room-{key:02}")).or_default().publisher = traffic;
		}
		let frame = Body {
			epoch: Epoch::mint().to_string(),
			tiers: BTreeMap::from([(String::new(), group)]),
		};
		Self {
			source,
			epoch,
			known: true,
			group: moq_json::snapshot::Producer::new(track, config),
			frame,
		}
	}

	/// Bump every path's byte counter and publish the whole snapshot.
	fn publish(&mut self, bytes: u64) {
		for usage in self.frame.tiers.values_mut().flat_map(|group| group.paths.values_mut()) {
			usage.publisher.bytes += bytes;
		}
		self.group
			.update(moq_net::Timed::from(&self.frame).at(moq_net::Timestamp::now()))
			.unwrap();
	}
}

/// The first path's merged egress.
fn first(frame: &BTreeMap<String, Group>) -> Option<Traffic> {
	let usage = frame.get("")?.paths.values().next()?;
	Some(usage.publisher)
}

/// Read merged frames until the first path reaches `want` bytes.
async fn read_until(traffic: &mut aggregate::GroupConsumer, want: u64) {
	loop {
		let frame = traffic.next().await.unwrap().unwrap();
		if first(&frame).is_some_and(|t| t.bytes >= want) {
			return;
		}
	}
}

#[derive(Clone, Copy)]
enum State {
	Known,
	Mixed,
	Resumed,
}

/// Mean wall time per merged frame with `live` nodes publishing after `departed` nodes left.
async fn measure(live: usize, departed: usize, grace: Duration, state: State) -> Duration {
	let (origin, driver) = origin::Producer::new(origin::Config::default());
	let driver = tokio::spawn(moq_net::time::run(driver));
	let agg = aggregate::Consumer::new(origin.consume(), aggregate::Config::new().with_grace(grace));
	let mut traffic = agg.group(moq_net::Path::from_static("acme"));

	// Every node contributes 1 byte per path, so the total counts nodes seen.
	let mut total = 0;
	for index in 0..departed {
		let mut node = Node::new(&origin, &format!("gone-{index}"));
		node.publish(1);
		total += 1;
		read_until(&mut traffic, total).await;
	}

	let mut nodes: Vec<Node> = (0..live)
		.map(|index| Node::new(&origin, &format!("live-{index}")))
		.collect();
	for node in &mut nodes {
		node.publish(1);
		total += 1;
		read_until(&mut traffic, total).await;
	}
	if matches!(state, State::Resumed) {
		for node in &mut nodes {
			node.source.unannounce();
			loop {
				let frame = traffic.next().await.unwrap().unwrap();
				if first(&frame).unwrap().active_subscriptions() == live as u64 - 1 {
					break;
				}
			}
			node.source
				.announce(origin::Route::default().with_epoch(node.epoch.clone()))
				.unwrap();
			node.publish(1);
			total += 1;
			read_until(&mut traffic, total).await;
		}
	}
	// Advance only after a read has seen the last departure, so every departed
	// node is past its grace.
	tokio::time::advance(GRACE * 2).await;

	let mut elapsed = Duration::ZERO;
	for frame in 0..=FRAMES {
		let node = &mut nodes[frame as usize % live];
		let start = Instant::now();
		if matches!(state, State::Mixed) {
			node.known = !node.known;
			let mut route = origin::Route::default();
			if node.known {
				route.epoch = Some(node.epoch.clone());
			}
			node.source.announce(route).unwrap();
		}
		node.publish(1);
		total += 1;
		read_until(&mut traffic, total).await;
		// The first frame folds the departed nodes; it is the warm-up.
		if frame > 0 {
			elapsed += start.elapsed();
		}
	}

	drop(nodes);
	driver.abort();
	elapsed / FRAMES
}

fn main() {
	// Nextest lists all targets as potential test binaries.
	if std::env::args().any(|arg| arg == "--list") {
		return;
	}
	let runtime = tokio::runtime::Builder::new_current_thread()
		.enable_time()
		.start_paused(true)
		.build()
		.unwrap();

	println!("live departed grace metadata    us/frame");
	for live in [1, 16, 128] {
		for departed in [0, 256, 4096] {
			for (label, grace) in [("1s", GRACE), ("never", Duration::MAX)] {
				for (metadata, state) in [
					("known", State::Known),
					("mixed", State::Mixed),
					("resumed", State::Resumed),
				] {
					let elapsed = runtime.block_on(measure(live, departed, grace, state));
					let micros = elapsed.as_secs_f64() * 1e6;
					println!("{live:>4} {departed:>8} {label:>5} {metadata:>8} {micros:>11.1}");
				}
			}
		}
	}
}
