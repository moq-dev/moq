//! Cost of one merged traffic frame from [`moq_stats::aggregate`], swept over live nodes and
//! nodes that have come and gone, with and without the grace that folds departed nodes away.
//!
//! Each node reports the same broadcast paths, like a restarted node under a new name. A bounded
//! aggregate's cost follows the live nodes; an unbounded one (`grace` never elapsing) also pays for
//! every departed node on each frame. Time is paused, so the churn ages past the grace instantly.
//!
//! Run `cargo bench -p moq-stats --bench aggregate` and compare the table across revisions.

use std::time::{Duration, Instant};

use moq_net::origin;
use moq_stats::{Role, Tier, Traffic, TrafficFrame, aggregate, traffic_track};

/// Broadcast paths each node reports.
const KEYS: usize = 16;

/// Frames measured per configuration, after one warm-up frame.
const FRAMES: u32 = 64;

/// The grace configured when folding; the churn ages past it before measuring.
const GRACE: Duration = Duration::from_secs(1);

/// A hand-published node stats broadcast with a default-tier publisher traffic track.
struct Node {
	_source: moq_net::broadcast::Producer,
	traffic: moq_json::snapshot::Producer<TrafficFrame>,
	frame: TrafficFrame,
}

impl Node {
	fn new(origin: &origin::Producer, name: &str) -> Self {
		let source = origin
			.create_broadcast(format!(".stats/acme/node/{name}").as_str())
			.unwrap();
		source.announce(origin::Route::default()).unwrap();
		let track = source
			.create_track(traffic_track(&Tier::default(), Role::Publisher, false), None)
			.unwrap();
		let config = moq_json::snapshot::Config::default().with_delta_ratio(0);
		let frame = (0..KEYS)
			.map(|key| (format!("acme/room-{key:02}"), Traffic::default()))
			.collect();
		Self {
			_source: source,
			traffic: moq_json::snapshot::Producer::new(track, config),
			frame,
		}
	}

	/// Bump every path's byte counter and publish the whole snapshot.
	fn publish(&mut self, bytes: u64) {
		for traffic in self.frame.values_mut() {
			traffic.bytes += bytes;
		}
		self.traffic.update(&self.frame).unwrap();
	}
}

/// Read merged frames until the first path reaches `want` bytes.
async fn read_until(traffic: &mut aggregate::TrafficConsumer, want: u64) {
	loop {
		let frame = traffic.next().await.unwrap().unwrap();
		if frame.values().next().is_some_and(|t| t.bytes >= want) {
			return;
		}
	}
}

/// Mean wall time per merged frame with `live` nodes publishing after `departed` nodes left.
async fn measure(live: usize, departed: usize, grace: Duration) -> Duration {
	let (origin, driver) = origin::Producer::new(origin::Config::default());
	let driver = tokio::spawn(moq_net::time::run(driver));
	let agg = aggregate::Consumer::new(
		origin.consume(),
		aggregate::Config::new().with_depth(1).with_grace(grace),
	);
	let mut traffic = agg.traffic(&Tier::default(), Role::Publisher);

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
	// Advance only after a read has seen the last departure, so every departed
	// node is past its grace.
	tokio::time::advance(GRACE * 2).await;

	let mut elapsed = Duration::ZERO;
	for frame in 0..=FRAMES {
		let node = &mut nodes[frame as usize % live];
		let start = Instant::now();
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

	println!("live departed grace    us/frame");
	for live in [1, 16, 128] {
		for departed in [0, 256, 4096] {
			for (label, grace) in [("1s", GRACE), ("never", Duration::MAX)] {
				let elapsed = runtime.block_on(measure(live, departed, grace));
				let micros = elapsed.as_secs_f64() * 1e6;
				println!("{live:>4} {departed:>8} {label:>5} {micros:>11.1}");
			}
		}
	}
}
