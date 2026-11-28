//! Endpoint routing: dispatching short header datagrams to the connection that owns their CID.
//!
//! Only `Endpoint::handle` is timed; the routed event is dropped before the connection sees it,
//! so the same datagrams replay every call.

use std::{fmt, hint::black_box};

use bytes::{BufMut, BytesMut};
use moq_quic::endpoint::{DatagramEvent, Received};
use swole::bench;

#[allow(
	dead_code,
	reason = "a harness shared by several benches, each using only part of it"
)]
mod common;

use common::{Pair, Setup};

/// Datagrams dispatched per call, drawn across every CID the server issued.
const BATCH: usize = 1024;

/// Connections against CIDs per connection. A client with zero-length CIDs advertises an
/// `active_connection_id_limit` of 2, otherwise 5, and the server issues that many.
#[derive(Clone, Copy)]
struct Case {
	connections: usize,
	cids: usize,
}

impl fmt::Display for Case {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "connections_{}/cids_{}", self.connections, self.cids)
	}
}

fn cases() -> Vec<Case> {
	[16, 256, 4096]
		.into_iter()
		.flat_map(|connections| [2, 5].map(|cids| Case { connections, cids }))
		.collect()
}

/// A connected server, the datagrams it routes, and the copy the next call consumes.
struct Route {
	pair: Pair,
	datagrams: Vec<BytesMut>,
	pending: Vec<BytesMut>,
	buf: Vec<u8>,
}

/// Refills the datagrams a call consumes, outside the measurement.
fn refill(route: &mut Route) {
	route.pending.clone_from(&route.datagrams);
}

#[bench(args = cases(), setup = route, before = refill, items = |_: Case| BATCH)]
fn endpoint_route(route: &mut Route) {
	let remote = route.pair.clients[0].addr;
	let now = route.pair.now;
	for datagram in route.pending.drain(..) {
		let event = route.pair.server.endpoint.handle(
			now,
			Received {
				remote,
				local_ip: None,
				ecn: None,
				data: datagram,
				arrival: None,
			},
			&mut route.buf,
		);
		assert!(matches!(event, Some(DatagramEvent::ConnectionEvent(..))));
		black_box(event);
	}
}

/// Connects the clients and forges one 1-RTT datagram per issued CID, in a scattered order.
fn route(case: Case) -> Route {
	let Case { connections, cids } = case;
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
	Route {
		pair,
		datagrams,
		pending: Vec::new(),
		buf: Vec::new(),
	}
}

fn main() {
	swole::main();
}
