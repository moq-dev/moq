//! Cost of observing replacement at a covering source, swept over unrelated routes and held requests.
//! The route-only control models the old gate: a more-specific source does not retract the broad route.

use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use moq_net::{Path, fuzz::RequestValidity, kio, origin};
use std::{hint::black_box, time::Duration};

fn retirement(c: &mut Criterion) {
	let mut group = c.benchmark_group("retirement");
	group.sample_size(20);
	group.warm_up_time(Duration::from_millis(500));
	group.measurement_time(Duration::from_secs(2));
	for selected in [false, true] {
		for routes in [8, 32, 128] {
			for consumers in [1, 8, 32] {
				let name = if selected { "selection" } else { "route-only" };
				group.bench_with_input(
					BenchmarkId::new(name, format!("{routes}x{consumers}")),
					&(routes, consumers),
					|b, &(routes, consumers)| {
						b.iter_batched(
							|| {
								let (origin, driver) = origin::Producer::new(origin::Config::default());
								let routes: Vec<_> = (0..routes)
									.map(|i| origin.dynamic(format!("route/{i}")).unwrap())
									.collect();
								let mut watches = Vec::new();
								let mut controls = Vec::new();
								let mut pending = Vec::new();
								for (i, route) in routes.iter().enumerate() {
									for reader in 0..if i == 0 { consumers } else { 1 } {
										if selected {
											let watch = RequestValidity::new(
												route,
												&Path::new(&format!("route/{i}/jobs/{reader}")),
											);
											pending.push(watch.start());
											watches.push(watch);
										} else {
											let control = kio::Producer::new(());
											pending.push(control.consume());
											controls.push(control);
										}
									}
								}
								(origin, driver, routes, watches, controls, pending)
							},
							|fixture| {
								let replacement = fixture.0.dynamic("route/0/jobs").unwrap();
								let refused = fixture
									.5
									.iter()
									.take(consumers)
									.filter(|request| request.is_closed())
									.count();
								assert_eq!(refused, if selected { consumers } else { 0 });
								black_box((fixture, replacement))
							},
							BatchSize::SmallInput,
						);
					},
				);
			}
		}
	}
	group.finish();
}

criterion_group!(benches, retirement);
criterion_main!(benches);
