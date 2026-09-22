//! Cost of one announcement against a fleet-shaped origin route table.
//!
//! The table is a trie keyed by path segment: an announcement visits the
//! cursors hanging on the walk down to its prefix and beneath it, and each of
//! those recomputes its best route from the entries at that prefix alone. So
//! the cost is bounded by the tree around the announced prefix, and these
//! shapes, fleet-sized on both axes, are where a scan of every cursor and
//! every route once became the whole CPU budget.

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use moq_net::{Pattern, Patterns, origin};

/// A relay's own stats fan-out: `.stats/<project>/node/<node>` for every project
/// on every node, watched by one cursor per peer.
struct Fleet {
	producer: origin::Producer,
	// Held: dropping a handle retracts its route, and dropping a consumer
	// unregisters its cursor. The table only stays fleet-sized while they live.
	_driver: origin::Driver,
	_routes: Vec<moq_net::broadcast::Producer>,
	_cursors: Vec<moq_net::announce::Consumer>,
}

fn fleet(projects: usize, nodes: usize, peers: usize) -> Fleet {
	let (producer, driver) = origin::Producer::new(origin::Config::default());
	let consumer = producer.consume();

	let mut routes = Vec::with_capacity(projects * nodes);
	for project in 0..projects {
		for node in 0..nodes {
			let path = format!(".stats/p{project}/node/edge{node}");
			routes.push(producer.publish(path, origin::Route::default()).unwrap());
		}
	}

	// Each peer watches one project's subtree, so cursors differ in scope and
	// none of them can be collapsed away.
	let mut cursors = Vec::with_capacity(peers);
	for peer in 0..peers {
		let patterns: Patterns = [Pattern::subtree(&format!(".stats/p{}", peer % projects)).unwrap()]
			.into_iter()
			.collect();
		cursors.push(consumer.scope("", &patterns).unwrap().announced());
	}

	Fleet {
		producer,
		_driver: driver,
		_routes: routes,
		_cursors: cursors,
	}
}

/// One announce and its retraction: two full sweeps of the table.
fn bench_announce(c: &mut Criterion) {
	let mut group = c.benchmark_group("origin_announce_sweep");
	// (projects, nodes, peers). The last is live's shape today.
	for (projects, nodes, peers) in [(1, 8, 4), (4, 30, 30), (8, 30, 60)] {
		let id = BenchmarkId::from_parameter(format!("{projects}p_{nodes}n_{peers}c"));
		group.bench_function(id, |b| {
			let fleet = fleet(projects, nodes, peers);
			b.iter(|| {
				let handle = fleet
					.producer
					.publish(".stats/p0/node/incoming", origin::Route::default())
					.unwrap();
				drop(handle);
			});
		});
	}
	group.finish();
}

criterion_group!(benches, bench_announce);
criterion_main!(benches);
