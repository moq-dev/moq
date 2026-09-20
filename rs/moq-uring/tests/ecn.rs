//! ECN through the worker's UDP path: the codepoint a send asks for is what
//! the peer's `Packet::ecn` reads back, over IPv4 and IPv6, as a lone
//! datagram and as a GSO train, with CE surviving too. Without the TOS and
//! traffic-class control messages the peer sees no mark, its ACKs carry no
//! ECN counts, and noq disables ECN on the first one.
//!
//! Kernel-gated: skips loudly below the Linux 6.12 floor (GitHub-hosted CI),
//! and runs everywhere else.

#![cfg(target_os = "linux")]

use std::net::UdpSocket;

use moq_uring::{Config, Error, Worker, udp};

fn worker() -> Option<Worker> {
	match Worker::new(Config::default()) {
		Ok(worker) => Some(worker),
		Err(Error::Unsupported(reason)) => {
			eprintln!("skipping io_uring ecn test: {reason}");
			None
		}
		Err(err) => panic!("worker setup failed: {err}"),
	}
}

/// Send `datagrams` datagrams of `segment` bytes marked `ecn` from one
/// worker socket to another and return what the receiver read back.
fn round_trip(bind: &str, config: udp::Config, ecn: Option<udp::Ecn>, datagrams: usize) -> Vec<udp::Packet> {
	const SEGMENT: usize = 1200;
	let Some(mut worker) = worker() else { return Vec::new() };
	let handle = worker.handle();

	let rx = UdpSocket::bind(bind).expect("bind receiver");
	let tx = UdpSocket::bind(bind).expect("bind sender");
	let rx = handle.udp(rx, config.clone()).expect("receiver socket");
	let to = rx.local_addr().expect("receiver addr");
	let tx = handle.udp(tx, config).expect("sender socket");

	worker
		.block_on(async move {
			let mut buf = tx.acquire().await.expect("acquire");
			let len = SEGMENT * datagrams;
			buf[..len].fill(0xAB);
			buf.send(udp::Transmit {
				to,
				len,
				segment: SEGMENT,
				ecn,
			})
			.expect("send");

			let mut packets = Vec::new();
			let mut received = 0;
			while received < datagrams {
				let packet = rx.recv().await.expect("recv");
				received += packet.payload().len().div_ceil(packet.stride());
				packets.push(packet);
			}
			packets
		})
		.expect("worker")
}

fn assert_marked(bind: &str, config: udp::Config, ecn: Option<udp::Ecn>, datagrams: usize) {
	let packets = round_trip(bind, config, ecn, datagrams);
	for packet in &packets {
		assert_eq!(packet.ecn(), ecn, "codepoint read back from {packet:?}");
	}
}

fn ablated() -> udp::Config {
	let mut config = udp::Config::default();
	config.gro = false;
	config.gso = false;
	config.multishot = false;
	config
}

#[test]
fn ect0_v4() {
	assert_marked("127.0.0.1:0", udp::Config::default(), Some(udp::Ecn::Ect0), 1);
}

#[test]
fn ect0_v6() {
	assert_marked("[::1]:0", udp::Config::default(), Some(udp::Ecn::Ect0), 1);
}

#[test]
fn ect0_gso_train() {
	// One `sendmsg` carries the mark on every segment, and GRO keeps it.
	assert_marked("127.0.0.1:0", udp::Config::default(), Some(udp::Ecn::Ect0), 8);
	assert_marked("[::1]:0", udp::Config::default(), Some(udp::Ecn::Ect0), 8);
}

#[test]
fn ect0_ablated() {
	// One `sendmsg` per datagram and oneshot receives read the same mark.
	assert_marked("127.0.0.1:0", ablated(), Some(udp::Ecn::Ect0), 4);
	assert_marked("[::1]:0", ablated(), Some(udp::Ecn::Ect0), 4);
}

#[test]
fn ce_survives() {
	assert_marked("127.0.0.1:0", udp::Config::default(), Some(udp::Ecn::Ce), 1);
	assert_marked("[::1]:0", udp::Config::default(), Some(udp::Ecn::Ce), 1);
}

#[test]
fn unmarked() {
	assert_marked("127.0.0.1:0", udp::Config::default(), None, 1);
	assert_marked("[::1]:0", udp::Config::default(), None, 1);
}
