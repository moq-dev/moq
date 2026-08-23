//! The PR-2 validation milestone: a raw-quiche echo over the worker's UDP
//! path, exercising the ring, provided-buffer RX with GRO, GSO TX, userspace
//! timers (quiche's timeout), local spawn, and parking together.
//!
//! Kernel-gated: skips loudly below the Linux 6.12 floor (GitHub-hosted CI),
//! and runs everywhere else.

#![cfg(target_os = "linux")]

#[path = "support/quiche.rs"]
mod support;

use std::net::UdpSocket;

use moq_uring::{Config, Error, Worker, udp};

fn worker() -> Option<Worker> {
	match Worker::new(Config::default()) {
		Ok(worker) => Some(worker),
		Err(Error::Unsupported(reason)) => {
			eprintln!("skipping io_uring echo test: {reason}");
			None
		}
		Err(err) => panic!("worker setup failed: {err}"),
	}
}

fn echo(config: udp::Config) {
	let Some(mut worker) = worker() else { return };
	let handle = worker.handle();

	let server_sock = UdpSocket::bind("127.0.0.1:0").expect("bind server");
	let client_sock = UdpSocket::bind("127.0.0.1:0").expect("bind client");

	let payload: Vec<u8> = (0..512 * 1024).map(|i| (i * 31 % 251) as u8).collect();
	let expect = payload.clone();

	let certs = support::certs().expect("certificates");
	let server_sock = handle.udp(server_sock, config.clone()).expect("server socket");
	let server_addr = server_sock.local_addr().expect("server addr");
	let client_sock = handle.udp(client_sock, config).expect("client socket");

	// The server runs as a spawned worker task; the client drives block_on.
	let server_handle = handle.clone();
	handle.spawn(async move {
		support::echo_server(server_handle, server_sock, certs)
			.await
			.expect("echo server");
	});

	let echoed = worker
		.block_on(async move { support::echo_client(&handle, client_sock, server_addr, &payload).await })
		.expect("worker")
		.expect("echo client");

	assert_eq!(echoed.len(), expect.len(), "echoed byte count");
	assert!(echoed == expect, "echoed bytes differ");
}

#[test]
fn echo_default() {
	// The production path: multishot + GRO + GSO.
	echo(udp::Config::default());
}

#[test]
fn echo_ablated() {
	// Everything off: oneshot receives, no GRO, one sendmsg per datagram.
	let mut config = udp::Config::default();
	config.gro = false;
	config.gso = false;
	config.multishot = false;
	echo(config);
}
