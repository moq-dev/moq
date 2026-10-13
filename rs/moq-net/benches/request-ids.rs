//! Request-ID admission cost across independent sessions and reordered windows.
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use std::{hint::black_box, time::Duration};

fn request_ids(c: &mut Criterion) {
	let mut group = c.benchmark_group("request-ids");
	group.sample_size(20);
	group.warm_up_time(Duration::from_millis(200));
	group.measurement_time(Duration::from_secs(1));
	// Up to the production window, which the session sizes to twice its 110,000 caps.
	for mode in ["ordered", "reordered", "scattered"] {
		for sessions in [1, 8, 32] {
			for window in [8, 128, 220_000] {
				let ids = ids(mode, window);
				println!(
					"admitted,{mode},{sessions},{window},{}",
					moq_net::fuzz::request_ids(window, &ids) * sessions
				);
				group.bench_function(BenchmarkId::new(mode, format!("{sessions}x{window}")), |b| {
					b.iter(|| {
						let accepted: usize = (0..sessions)
							.map(|_| moq_net::fuzz::request_ids(window, black_box(&ids)))
							.sum();
						black_box(accepted)
					});
				});
			}
		}
	}
	group.finish();
}

/// Draft-17+ admission, which spends IDs without a window but remembers skipped runs.
fn request_history(c: &mut Criterion) {
	let mut group = c.benchmark_group("modern-request-ids");
	group.sample_size(20);
	group.warm_up_time(Duration::from_millis(200));
	group.measurement_time(Duration::from_secs(1));
	for mode in ["ordered", "reordered", "scattered"] {
		for sessions in [1, 8, 32] {
			for requests in [8, 128, 220_000] {
				let ids = ids(mode, requests);
				println!(
					"admitted-modern,{mode},{sessions},{requests},{}",
					moq_net::fuzz::request_history(&ids) * sessions
				);
				group.bench_function(BenchmarkId::new(mode, format!("{sessions}x{requests}")), |b| {
					b.iter(|| {
						let accepted: usize = (0..sessions)
							.map(|_| moq_net::fuzz::request_history(black_box(&ids)))
							.sum();
						black_box(accepted)
					});
				});
			}
		}
	}
	group.finish();
}

/// Every ID in the window: in order, reversed (one skip to the top, then each fill
/// shortens it), or scattered (the top first, then every other ID descending, so each
/// splits the lowest skipped run and the remembered runs grow to half the window).
fn ids(mode: &str, window: u64) -> Vec<u64> {
	let all = (0..window).map(|id| id * 2);
	match mode {
		"ordered" => all.collect(),
		"reordered" => all.rev().collect(),
		"scattered" => {
			let below = (0..window - 1).rev().map(|id| id * 2);
			let mut ids = vec![2 * (window - 1)];
			ids.extend(below.clone().filter(|id| id / 2 % 2 == 1));
			ids.extend(below.filter(|id| id / 2 % 2 == 0));
			ids
		}
		_ => unreachable!(),
	}
}

criterion_group!(benches, request_ids, request_history);
criterion_main!(benches);
