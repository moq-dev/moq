//! End-to-end MoQ Lite datagram routing benchmark over the in-memory transport.
//!
//! A burst stays on one subscription, matching a media track's common shape and
//! making the per-datagram session route lookup visible. The timed path includes
//! publisher encoding, transport queueing, subscriber routing, and model delivery.

use std::time::Instant;

use bytes::Bytes;
use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use moq_net::{Datagram, Hop, Timestamp, Version, broadcast, origin, track};

#[path = "../tests/support/mod.rs"]
mod support;

use support::harness::{MockConnectOptions, TokioRuntime, connect_mock};

const BURST: usize = 1_024;
const PAYLOAD: usize = 256;

fn produce_origin(hop: u64) -> origin::Producer {
	let (producer, driver) = origin::Producer::new(origin::Info::new(Hop::new(hop).unwrap()));
	tokio::spawn(driver.run(TokioRuntime::<()>::new()));
	producer
}

struct Ctx {
	producer: track::Producer,
	subscriber: track::Subscriber,
	_broadcast: broadcast::Producer,
	_pair: support::harness::MockPair,
}

async fn setup_datagrams() -> Ctx {
	let publisher = produce_origin(1);
	let subscriber = produce_origin(2);

	let mut source_broadcast = publisher.create_broadcast("bench").unwrap();
	let announcement = publisher.announce("bench", Default::default()).unwrap();
	let track = source_broadcast.create_track("datagrams", None).unwrap();
	// The benchmark owns this single setup for its process lifetime.
	std::mem::forget(announcement);

	let mut options = MockConnectOptions::new("moq-lite-05".parse::<Version>().unwrap());
	options.server_publish = Some(publisher);
	options.client_subscribe = Some(subscriber.clone());
	let pair = connect_mock(options).await;

	let consumer = subscriber.consume();
	consumer.routed("bench").await.unwrap();
	let remote_broadcast = consumer.request_broadcast("bench").await.unwrap();
	let subscriber = remote_broadcast
		.track("datagrams")
		.unwrap()
		.subscribe(None)
		.await
		.unwrap();

	Ctx {
		producer: track,
		subscriber,
		_broadcast: source_broadcast,
		_pair: pair,
	}
}

fn datagram_route(c: &mut Criterion) {
	let runtime = tokio::runtime::Builder::new_current_thread()
		.enable_time()
		.build()
		.unwrap();
	let mut ctx = runtime.block_on(setup_datagrams());
	let payload = Bytes::from(vec![0x5a; PAYLOAD]);
	let mut sequence = 0u64;

	let mut group = c.benchmark_group("datagram_route");
	group.throughput(Throughput::Elements(BURST as u64));
	group.bench_function("single-track-burst", |b| {
		b.iter_custom(|iterations| {
			runtime.block_on(async {
				let start = Instant::now();
				for _ in 0..iterations {
					for _ in 0..BURST {
						ctx.producer
							.write_datagram(Datagram {
								sequence,
								timestamp: Timestamp::ZERO,
								payload: payload.clone(),
							})
							.unwrap();
						sequence += 1;
					}
					for _ in 0..BURST {
						let datagram = ctx.subscriber.recv_datagram().await.unwrap().unwrap();
						assert_eq!(datagram.payload.len(), PAYLOAD);
					}
				}
				start.elapsed()
			})
		});
	});
	group.finish();
}

criterion_group!(benches, datagram_route);
criterion_main!(benches);
