//! Request-ID admission cost across independent sessions and reordered windows.
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use std::{hint::black_box, time::Duration};

fn request_ids(c: &mut Criterion) {
	let mut group = c.benchmark_group("request-ids");
	group.sample_size(20);
	group.warm_up_time(Duration::from_millis(200));
	group.measurement_time(Duration::from_secs(1));
	for reversed in [false, true] {
		for sessions in [1, 8, 32] {
			for window in [8, 32, 128] {
				let mut ids: Vec<_> = (0..window).map(|id| id * 2).collect();
				if reversed {
					ids.reverse();
				}
				let mode = if reversed { "reordered" } else { "ordered" };
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
fn request_history(c: &mut Criterion) {
	let mut group = c.benchmark_group("modern-request-ids");
	group.sample_size(20);
	group.warm_up_time(Duration::from_millis(200));
	group.measurement_time(Duration::from_secs(1));
	for fragmented in [false, true] {
		for sessions in [1, 8, 32] {
			for ranges in [8, 32, 128] {
				let mut ids: Vec<_> = (0..ranges).map(|id| id * if fragmented { 4 } else { 2 }).collect();
				if fragmented {
					ids.reverse();
				}
				let mode = if fragmented { "fragmented" } else { "contiguous" };
				println!(
					"admitted,{mode},{sessions},{ranges},{}",
					moq_net::fuzz::request_history(&ids) * sessions
				);
				group.bench_function(BenchmarkId::new(mode, format!("{sessions}x{ranges}")), |b| {
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
criterion_group!(benches, request_ids, request_history);
criterion_main!(benches);
