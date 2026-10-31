//! Ed25519 cost per grouped frame through the public API: signing on write, verifying on read.
//!
//! Run with `cargo bench -p moq-sign`.

use std::hint::black_box;
use std::task::Poll;
use std::time::Duration;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use moq_sign::{SecretKey, broadcast};

fn produce(key: &SecretKey) -> (moq_sign::track::Producer, moq_net::broadcast::Producer) {
	// A zero max age keeps only the newest group cached.
	let info = moq_net::track::Info::default().with_max_age(Duration::ZERO);
	let inner = moq_net::broadcast::Info::new().produce();
	let path = moq_net::Path::new("bench").unwrap();
	let track = broadcast::Producer::new(inner.clone(), key.clone(), path)
		.create_track("stream", info)
		.unwrap();
	(track, inner)
}

/// A signed track published on an origin and a verifying subscription to it.
async fn roundtrip(
	key: &SecretKey,
) -> (
	moq_sign::track::Producer,
	moq_sign::track::Consumer,
	moq_net::origin::Publication,
) {
	let (origin, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::default());
	tokio::spawn(moq_net::time::run(driver));
	let path = moq_net::Path::new("bench").unwrap();
	let publication = origin.create_broadcast(&path).unwrap();
	let signed = broadcast::Producer::new(publication.broadcast().clone(), key.clone(), path.clone());
	let info = moq_net::track::Info::default().with_max_age(Duration::ZERO);
	let track = signed.create_track("stream", info).unwrap();
	publication
		.announce(moq_net::origin::Route::default().with_epoch(signed.epoch().clone()))
		.unwrap();
	let inner = origin.consume().request(&path, None).await.unwrap();
	let consumer = broadcast::Consumer::new(inner, key.public(), path).unwrap();
	let subscription = moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(60));
	let consumer = consumer.subscribe("stream", subscription).unwrap().await.unwrap();
	(track, consumer, publication)
}

fn bench_sign(c: &mut Criterion) {
	let key = SecretKey::generate();

	let mut group = c.benchmark_group("sign");
	for size in [1024usize, 64 * 1024] {
		let payload = vec![0u8; size];
		group.throughput(Throughput::Bytes(size as u64));
		group.bench_function(format!("write_{}k", size / 1024), |b| {
			let (producer, _inner) = produce(&key);
			b.iter(|| {
				let mut group = producer.append_group().unwrap();
				group.write_frame(None, black_box(&payload)).unwrap();
				group.finish().unwrap();
			});
		});
		group.bench_function(format!("roundtrip_{}k", size / 1024), |b| {
			let runtime = tokio::runtime::Builder::new_current_thread()
				.enable_all()
				.build()
				.unwrap();
			let (producer, mut consumer, _origin) = runtime.block_on(roundtrip(&key));
			let waiter = kio::Waiter::noop();
			b.iter(|| {
				let mut group = producer.append_group().unwrap();
				group.write_frame(None, black_box(&payload)).unwrap();
				group.finish().unwrap();
				let Poll::Ready(Ok(Some(mut group))) = consumer.poll_recv_group(&waiter) else {
					panic!("the group is delivered in-process");
				};
				let Poll::Ready(Ok(Some(frame))) = group.poll_read_frame(&waiter) else {
					panic!("the frame verifies");
				};
				black_box(frame);
			});
		});
	}
	group.finish();
}

criterion_group!(benches, bench_sign);
criterion_main!(benches);
