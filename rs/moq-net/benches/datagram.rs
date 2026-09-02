//! End-to-end MoQ Lite datagram routing benchmark over the in-memory transport.
//!
//! A burst stays on one subscription, matching a media track's common shape and
//! making the per-datagram session route lookup visible. The timed path includes
//! publisher encoding, transport queueing, subscriber routing, and model delivery.

use std::time::Instant;

use bytes::Bytes;
use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use moq_net::{Datagram, Hop, Timestamp, Version, announce, broadcast, origin, track};

#[path = "../tests/support/mod.rs"]
mod support;

use support::harness::{MockConnectOptions, TokioRuntime, connect_mock};

const BURST: usize = 1_024;
const PAYLOAD: usize = 256;
/// Generous bound so a burst whose tail was evicted fails loudly instead of hanging.
const DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

fn produce_origin(hop: u64) -> origin::Producer {
	let (producer, driver) = origin::Producer::new(origin::Info::new(Hop::new(hop).unwrap()));
	tokio::spawn(driver.run(TokioRuntime::<()>::new()));
	producer
}

struct Ctx {
	producer: track::Producer,
	subscriber: track::Subscriber,
	_broadcast: broadcast::Producer,
	_announcement: announce::Producer,
	_pair: support::harness::MockPair,
}

async fn setup_datagrams() -> Ctx {
	let publisher = produce_origin(1);
	let subscriber = produce_origin(2);

	let mut source_broadcast = publisher.create_broadcast("bench").unwrap();
	let announcement = publisher.announce("bench", Default::default()).unwrap();
	let track = source_broadcast.create_track("datagrams", None).unwrap();

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
		_announcement: announcement,
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
					// Datagrams are best-effort: the model buffer evicts by wall-clock age
					// (`MAX_DATAGRAM_AGE`), so a burst can lose its oldest entries before the
					// reader reaches them. Drain to the last sequence written, which nothing
					// evicts because no datagram is pushed during the drain, rather than
					// counting a fixed number that may never arrive. The outer timeout keeps
					// a lost tail from parking the run forever.
					let last = sequence - 1;
					let drain = async {
						loop {
							let datagram = ctx.subscriber.recv_datagram().await.unwrap().unwrap();
							assert_eq!(datagram.payload.len(), PAYLOAD);
							if datagram.sequence >= last {
								break;
							}
						}
					};
					tokio::time::timeout(DRAIN_TIMEOUT, drain).await.expect("drain stalled");
				}
				start.elapsed()
			})
		});
	});
	group.finish();
}

criterion_group!(benches, datagram_route);
criterion_main!(benches);
