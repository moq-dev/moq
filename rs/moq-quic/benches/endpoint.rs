//! Endpoint routing: dispatching short header datagrams to the connection that owns their CID.
//!
//! Only `Endpoint::handle` is timed; the routed event is dropped before the connection sees it,
//! so the same datagrams replay every iteration.

use std::hint::black_box;

use bytes::{BufMut, BytesMut};
use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use moq_quic::endpoint::{DatagramEvent, Received};

#[allow(
	dead_code,
	reason = "a harness shared by several benches, each using only part of it"
)]
mod common;

use common::{Pair, Setup, ThreadCpu};

/// Datagrams dispatched per iteration, drawn across every CID the server issued.
const BATCH: usize = 1024;

/// Connections against CIDs per connection. A client with zero-length CIDs advertises an
/// `active_connection_id_limit` of 2, otherwise 5, and the server issues that many.
fn bench_route(c: &mut Criterion<ThreadCpu>) {
	let mut group = c.benchmark_group("endpoint_route");
	group.throughput(Throughput::Elements(BATCH as u64));
	for connections in [16, 256, 4096] {
		for cids in [2, 5] {
			let mut setup = None;
			group.bench_function(
				BenchmarkId::new(format!("connections_{connections}"), format!("cids_{cids}")),
				|b| {
					let (pair, datagrams) = setup.get_or_insert_with(|| route(connections, cids));
					let remote = pair.clients[0].addr;
					let now = pair.now;
					let mut buf = Vec::new();
					b.iter_batched(
						|| datagrams.clone(),
						|datagrams| {
							for datagram in datagrams {
								let event = pair.server.endpoint.handle(
									now,
									Received {
										remote,
										local_ip: None,
										ecn: None,
										data: datagram,
									},
									&mut buf,
								);
								assert!(matches!(event, Some(DatagramEvent::ConnectionEvent(..))));
								black_box(event);
							}
						},
						BatchSize::SmallInput,
					);
				},
			);
		}
	}
	group.finish();
}

/// Connects the clients and forges one 1-RTT datagram per issued CID, in a scattered order.
fn route(connections: usize, cids: usize) -> (Pair, Vec<BytesMut>) {
	let pair = Pair::new(Setup {
		clients: connections,
		client_cid_len: if cids == 2 { 0 } else { 8 },
		..Default::default()
	});
	let issued = pair.server_cids.lock().unwrap().clone();
	assert_eq!(
		issued.len(),
		connections * cids,
		"server issued an unexpected number of CIDs"
	);

	// A fixed-seed LCG, so both sides of a comparison route the same scattered sequence.
	let mut state = 0x2545_f491_4f6c_dd1du64;
	let datagrams = (0..BATCH)
		.map(|_| {
			state = state
				.wrapping_mul(6364136223846793005)
				.wrapping_add(1442695040888963407);
			let cid = issued[(state >> 33) as usize % issued.len()];
			let mut datagram = BytesMut::with_capacity(1200);
			datagram.put_u8(0x40 | 0x03);
			datagram.put_slice(&cid);
			datagram.resize(1200, 0);
			datagram
		})
		.collect();
	(pair, datagrams)
}

criterion_group! {
	name = benches;
	config = common::config();
	targets = bench_route
}
criterion_main!(benches);
