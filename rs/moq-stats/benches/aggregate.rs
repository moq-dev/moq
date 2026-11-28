//! Cost of one merged bucket frame from [`moq_stats::aggregate`], swept over live nodes and
//! nodes that have come and gone, with and without the grace that folds departed nodes away.
//! Compare steady nodes with live nodes that resumed after their open pairs were closed.
//!
//! Each node reports the same broadcast paths, like a restarted node under a new name. A bounded
//! aggregate's cost follows the live nodes; an unbounded one (`grace` never elapsing) also pays for
//! every departed node on each frame. Time is paused, so the churn ages past the grace instantly.
//!
//! Run with `cargo bench -p moq-stats --bench aggregate`.

use std::collections::BTreeMap;
use std::fmt;
use std::time::Duration;

use moq_net::origin;
use moq_net::stats::Traffic;
use moq_stats::{Bucket, aggregate, bucket_track};
use swole::bench;

/// Broadcast paths each node reports.
const KEYS: usize = 16;

/// The grace configured when folding; the churn ages past it before measuring.
const GRACE: Duration = Duration::from_secs(1);

/// A bucket frame as the producer writes it.
#[derive(serde::Serialize)]
struct Body {
	epoch: String,
	tiers: BTreeMap<String, Bucket>,
}

/// A hand-published node stats broadcast with one bucket track carrying
/// default-tier egress.
struct Node {
	source: moq_net::origin::Route,
	bucket: moq_json::snapshot::Producer<Body>,
	frame: Body,
	/// How many snapshots it published, which stamps the next a millisecond after the last.
	published: u64,
}

impl Node {
	fn new(origin: &origin::Producer, name: &str) -> Self {
		let source = origin
			.route(format!(".stats/node/{name}").as_str(), origin::RouteOptions::default())
			.unwrap();
		let track = source
			.broadcast()
			.create_track(
				bucket_track(
					moq_net::Path::from_static("acme"),
					&"01900000-0000-7000-8000-000000000001".parse().unwrap(),
				),
				moq_net::track::Info::default()
					.with_max_age(None)
					.with_timescale(moq_net::Timescale::MILLI),
			)
			.unwrap();
		let config = moq_json::snapshot::Config::default().with_deltas(false);
		let mut bucket = Bucket::default();
		for key in 0..KEYS {
			let mut traffic = Traffic::default();
			traffic.subscriptions_started = 1;
			bucket.paths.entry(format!("acme/room-{key:02}")).or_default().publisher = traffic;
		}
		let frame = Body {
			epoch: "01900000-0000-7000-8000-000000000001".to_string(),
			tiers: BTreeMap::from([(String::new(), bucket)]),
		};
		Self {
			source,
			bucket: moq_json::snapshot::Producer::new(track, config),
			frame,
			published: 0,
		}
	}

	/// Bump every path's byte counter and publish the whole snapshot.
	fn publish(&mut self, bytes: u64) {
		for usage in self
			.frame
			.tiers
			.values_mut()
			.flat_map(|bucket| bucket.paths.values_mut())
		{
			usage.publisher.bytes += bytes;
		}
		let at = moq_net::Timestamp::from_millis(self.published).unwrap();
		self.published += 1;
		self.bucket.update(&self.frame, at).unwrap();
	}
}

/// The first path's merged egress.
fn first(frame: &BTreeMap<String, Bucket>) -> Option<Traffic> {
	let usage = frame.get("")?.paths.values().next()?;
	Some(usage.publisher)
}

/// Read merged frames until the first path reaches `want` bytes.
async fn read_until(traffic: &mut aggregate::BucketConsumer, want: u64) {
	loop {
		let frame = traffic.next().await.unwrap().unwrap();
		if first(&frame).is_some_and(|t| t.bytes >= want) {
			return;
		}
	}
}

/// Whether the live nodes keep their open pairs or resumed after they were closed.
#[derive(Clone, Copy)]
enum Metadata {
	Known,
	Resumed,
}

/// Live and departed nodes, the grace, and the live nodes' metadata, named
/// `<live>/<departed>/<grace>/<metadata>`.
#[derive(Clone, Copy)]
struct Case {
	live: usize,
	departed: usize,
	/// Whether departed nodes fold away after [`GRACE`], or never.
	grace: bool,
	metadata: Metadata,
}

impl fmt::Display for Case {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		let grace = if self.grace { "1s" } else { "never" };
		let metadata = match self.metadata {
			Metadata::Known => "known",
			Metadata::Resumed => "resumed",
		};
		write!(f, "{}/{}/{grace}/{metadata}", self.live, self.departed)
	}
}

fn cases() -> Vec<Case> {
	let mut cases = Vec::new();
	for live in [1, 16, 128] {
		for departed in [0, 256, 4096] {
			for grace in [true, false] {
				for metadata in [Metadata::Known, Metadata::Resumed] {
					cases.push(Case {
						live,
						departed,
						grace,
						metadata,
					});
				}
			}
		}
	}
	cases
}

/// An aggregate with `live` nodes publishing after `departed` nodes left, past the warm-up frame.
struct Aggregate {
	traffic: aggregate::BucketConsumer,
	nodes: Vec<Node>,
	/// Every node contributes 1 byte per path, so the total counts nodes seen.
	total: u64,
	/// Frames published so far, which picks the next node to publish.
	frames: usize,
	_agg: aggregate::Consumer,
	_origin: origin::Producer,
	/// Last, so everything above drops while its driver task still exists.
	runtime: tokio::runtime::Runtime,
}

impl Aggregate {
	/// Publish one frame from the next node and read until the aggregate merged it.
	fn frame(&mut self) {
		let index = self.frames % self.nodes.len();
		let node = &mut self.nodes[index];
		self.frames += 1;
		self.total += 1;
		self.runtime.block_on(async {
			node.publish(1);
			read_until(&mut self.traffic, self.total).await;
		});
	}
}

fn aggregate(case: Case) -> Aggregate {
	let runtime = tokio::runtime::Builder::new_current_thread()
		.enable_time()
		.start_paused(true)
		.build()
		.unwrap();
	let grace = if case.grace { GRACE } else { Duration::MAX };
	let live = case.live;
	let (origin, agg, traffic, nodes, total) = runtime.block_on(async {
		let (origin, driver) = origin::Producer::new(origin::Config::default());
		tokio::spawn(moq_net::time::run(driver));
		let agg = aggregate::Consumer::new(origin.consume(), aggregate::Config::new().with_grace(grace));
		let mut traffic = agg.bucket(moq_net::Path::from_static("acme"));

		// Every departed node publishes before any leaves, so one read covers them all
		// instead of one merge per node, which grows with every node seen.
		let departed: Vec<Node> = (0..case.departed)
			.map(|index| {
				let mut node = Node::new(&origin, &format!("gone-{index}"));
				node.publish(1);
				node
			})
			.collect();
		let mut total = case.departed as u64;
		if total > 0 {
			read_until(&mut traffic, total).await;
		}
		drop(departed);

		let mut nodes: Vec<Node> = (0..live)
			.map(|index| Node::new(&origin, &format!("live-{index}")))
			.collect();
		for node in &mut nodes {
			node.publish(1);
			total += 1;
			read_until(&mut traffic, total).await;
		}
		if let Metadata::Resumed = case.metadata {
			for node in &mut nodes {
				node.source.withdraw();
				loop {
					let frame = traffic.next().await.unwrap().unwrap();
					if first(&frame).unwrap().active_subscriptions() == live as u64 - 1 {
						break;
					}
				}
				node.source.announce().unwrap();
				node.publish(1);
				total += 1;
				read_until(&mut traffic, total).await;
			}
		}
		// Advance only after a read has seen the last departure, so every departed
		// node is past its grace.
		tokio::time::advance(GRACE * 2).await;
		(origin, agg, traffic, nodes, total)
	});

	let mut aggregate = Aggregate {
		traffic,
		nodes,
		total,
		frames: 0,
		_agg: agg,
		_origin: origin,
		runtime,
	};
	// The first frame folds the departed nodes; it is the warm-up.
	aggregate.frame();
	aggregate
}

/// One merged frame.
#[bench(args = cases(), setup = aggregate)]
fn frame(aggregate: &mut Aggregate) {
	aggregate.frame();
}

fn main() {
	swole::main();
}
