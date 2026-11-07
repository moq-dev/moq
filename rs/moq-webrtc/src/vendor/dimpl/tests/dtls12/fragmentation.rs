//! DTLS 1.2 fragmentation test (MTU-based).

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::vendor::dimpl::{Config, Dtls, Output};

use crate::vendor::dimpl::tests::dtls12::common::*;

#[test]
fn dtls12_fragmentation_dimpl_to_dimpl() {
	//! Verify that a dimpl-to-dimpl handshake completes successfully with a
	//! small MTU that forces handshake message fragmentation.

	use crate::vendor::dimpl::certificate::generate_self_signed_certificate;

	let client_cert = generate_self_signed_certificate().expect("gen client cert");
	let server_cert = generate_self_signed_certificate().expect("gen server cert");

	let config = dtls12_config_with_mtu(200);

	let mut now = Instant::now();

	let mut client = Dtls::new_12(Arc::clone(&config), client_cert, now);
	client.set_active(true);

	let mut server = Dtls::new_12(config, server_cert, now);
	server.set_active(false);

	let mut client_connected = false;
	let mut server_connected = false;
	let mut max_packet_size = 0usize;

	for _ in 0..40 {
		client.handle_timeout(now).expect("client timeout");
		server.handle_timeout(now).expect("server timeout");

		let client_out = drain_outputs(&mut client);
		let server_out = drain_outputs(&mut server);

		// Track max packet size to verify MTU compliance
		for p in &client_out.packets {
			if p.len() > max_packet_size {
				max_packet_size = p.len();
			}
		}
		for p in &server_out.packets {
			if p.len() > max_packet_size {
				max_packet_size = p.len();
			}
		}

		if client_out.connected {
			client_connected = true;
		}
		if server_out.connected {
			server_connected = true;
		}

		deliver_packets(&client_out.packets, &mut server);
		deliver_packets(&server_out.packets, &mut client);

		if client_connected && server_connected {
			break;
		}

		now += Duration::from_millis(10);
	}

	assert!(
		client_connected,
		"Client should connect with small MTU (dimpl-to-dimpl)"
	);
	assert!(
		server_connected,
		"Server should connect with small MTU (dimpl-to-dimpl)"
	);
	assert!(
		max_packet_size <= 200,
		"All packets should respect 200-byte MTU: max was {}",
		max_packet_size
	);
}

#[test]
fn dtls12_fragmented_handshake_with_packet_loss() {
	//! Use a small MTU so flights are fragmented into multiple packets.
	//! Drop the first packet of the first flight, then trigger a retransmit.
	//! Verify the handshake still completes.

	use crate::vendor::dimpl::certificate::generate_self_signed_certificate;

	let client_cert = generate_self_signed_certificate().expect("gen client cert");
	let server_cert = generate_self_signed_certificate().expect("gen server cert");

	let config = dtls12_config_with_mtu(200);

	let mut now = Instant::now();

	let mut client = Dtls::new_12(Arc::clone(&config), client_cert, now);
	client.set_active(true);

	let mut server = Dtls::new_12(config, server_cert, now);
	server.set_active(false);

	// FLIGHT 1: Client sends ClientHello (possibly fragmented).
	// Trigger the initial timeout to start the handshake.
	client.handle_timeout(now).expect("client timeout start");
	client.handle_timeout(now).expect("client arm flight 1");
	let flight1 = collect_packets(&mut client);
	assert!(
		!flight1.is_empty(),
		"Client should emit at least one packet for flight 1"
	);

	// Drop the first packet of flight 1 and deliver the rest (if any).
	deliver_packets(&flight1[1..], &mut server);

	// Trigger retransmit on the client so it resends the full flight.
	trigger_timeout(&mut client, &mut now);
	let retransmit1 = collect_packets(&mut client);
	assert!(!retransmit1.is_empty(), "Client should retransmit flight 1");

	// Deliver the full retransmitted flight to the server.
	deliver_packets(&retransmit1, &mut server);

	// Now continue the handshake normally until completion.
	let mut client_connected = false;
	let mut server_connected = false;

	for _ in 0..40 {
		server.handle_timeout(now).expect("server timeout");
		let server_out = drain_outputs(&mut server);
		if server_out.connected {
			server_connected = true;
		}
		deliver_packets(&server_out.packets, &mut client);

		client.handle_timeout(now).expect("client timeout");
		let client_out = drain_outputs(&mut client);
		if client_out.connected {
			client_connected = true;
		}
		deliver_packets(&client_out.packets, &mut server);

		if client_connected && server_connected {
			break;
		}

		now += Duration::from_millis(10);
	}

	assert!(
		client_connected,
		"Client should connect after retransmit of dropped fragment"
	);
	assert!(
		server_connected,
		"Server should connect after retransmit of dropped fragment"
	);
}

#[test]
fn dtls12_out_of_order_fragments() {
	//! Use a small MTU to force fragmentation. Before delivering each flight,
	//! reverse the packet order. Verify the handshake still completes, proving
	//! fragment reassembly handles out-of-order delivery.

	use crate::vendor::dimpl::certificate::generate_self_signed_certificate;

	let client_cert = generate_self_signed_certificate().expect("gen client cert");
	let server_cert = generate_self_signed_certificate().expect("gen server cert");

	let config = dtls12_config_with_mtu(200);

	let mut now = Instant::now();

	let mut client = Dtls::new_12(Arc::clone(&config), client_cert, now);
	client.set_active(true);

	let mut server = Dtls::new_12(config, server_cert, now);
	server.set_active(false);

	let mut client_connected = false;
	let mut server_connected = false;

	for _ in 0..40 {
		client.handle_timeout(now).expect("client timeout");
		server.handle_timeout(now).expect("server timeout");

		let client_out = drain_outputs(&mut client);
		let server_out = drain_outputs(&mut server);

		if client_out.connected {
			client_connected = true;
		}
		if server_out.connected {
			server_connected = true;
		}

		// Reverse packet order before delivery to simulate out-of-order fragments
		let mut client_packets = client_out.packets;
		let mut server_packets = server_out.packets;
		client_packets.reverse();
		server_packets.reverse();

		deliver_packets(&client_packets, &mut server);
		deliver_packets(&server_packets, &mut client);

		if client_connected && server_connected {
			break;
		}

		// Reordered protected records may be discarded until CCS is processed.
		// Give the retransmission timer enough time to recover those records.
		now += Duration::from_secs(1);
	}

	assert!(client_connected, "Client should connect with out-of-order fragments");
	assert!(server_connected, "Server should connect with out-of-order fragments");
}
