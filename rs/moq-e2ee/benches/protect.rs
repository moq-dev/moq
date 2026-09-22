//! AES-128-GCM throughput for grouped frames and datagrams through the public API.
//!
//! Run with `cargo bench -p moq-e2ee`.

use std::hint::black_box;
use std::time::Duration;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use moq_e2ee::credential::Config;
use moq_e2ee::{Credential, Epoch};
use moq_net::Timestamp;

fn credential() -> Credential {
	Credential::new(Config {
		context: "bench".into(),
		kid: 0,
		secret: [0x11; 32],
	})
	.unwrap()
}

/// Each producer is its own publisher instance, so it gets its own epoch.
fn produce(credential: &Credential, semantic: &str) -> moq_e2ee::track::Producer {
	let generation = credential.generation(Epoch::mint());
	let name = generation.name(semantic).unwrap();
	// Advancing timestamps plus a zero max age keep only the newest group cached.
	let info = moq_net::track::Info::default().with_max_age(Duration::ZERO);
	let net = moq_net::broadcast::Info::new()
		.produce()
		.create_track(name.as_str(), info)
		.unwrap();
	generation.produce(net).unwrap()
}

fn bench_protect(c: &mut Criterion) {
	let credential = credential();
	let grouped = vec![0u8; 1024];
	let opus = vec![0u8; 160];

	let mut group = c.benchmark_group("protect");
	group.throughput(Throughput::Bytes(grouped.len() as u64));
	group.bench_function("grouped_1k", |b| {
		let mut producer = produce(&credential, "video");
		let mut ms = 0u64;
		b.iter(|| {
			ms += 1;
			let mut group = producer.append_group().unwrap();
			group
				.write_frame(Timestamp::from_millis(ms).unwrap(), black_box(&grouped))
				.unwrap();
			group.finish().unwrap();
		});
	});
	group.throughput(Throughput::Bytes(opus.len() as u64));
	group.bench_function("datagram_opus160", |b| {
		let mut producer = produce(&credential, "audio");
		let mut ms = 0u64;
		b.iter(|| {
			ms += 1;
			producer
				.append_datagram(Timestamp::from_millis(ms).unwrap(), black_box(&opus))
				.unwrap()
		});
	});
	group.finish();
}

criterion_group!(benches, bench_protect);
criterion_main!(benches);
