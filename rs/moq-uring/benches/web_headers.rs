//! The cost of ready stream headers against independently stalled headers.

use criterion::{Criterion, criterion_group, criterion_main};

#[cfg(target_os = "linux")]
#[path = "../tests/integration/web_headers.rs"]
mod headers;
#[cfg(target_os = "linux")]
#[path = "../tests/integration/support.rs"]
mod support;

fn benchmark(c: &mut Criterion) {
	#[cfg(target_os = "linux")]
	{
		match moq_uring::Worker::new(Default::default()) {
			Ok(worker) => drop(worker),
			Err(moq_uring::Error::Unsupported(reason)) if std::env::var_os("CI_STRICT").is_none() => {
				eprintln!("skipping io_uring header benchmark: {reason}");
				return;
			}
			Err(err) => panic!("worker: {err}"),
		}
		let mut group = c.benchmark_group("web_headers");
		for pending in [0, 64, 256] {
			for ready in [1, 16] {
				group.throughput(criterion::Throughput::Elements(2 * ready as u64));
				group.bench_function(format!("pending-{pending}/ready-{ready}"), |b| {
					b.iter_custom(|iterations| headers::run(pending, ready, iterations));
				});
			}
		}
		group.finish();
	}
	#[cfg(not(target_os = "linux"))]
	let _ = c;
}

criterion_group!(benches, benchmark);
criterion_main!(benches);
