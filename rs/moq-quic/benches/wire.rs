//! Wire codec and ACKs: packet and frame encode, decode, and ACK processing on one server
//! connection whose real client goes quiet after the handshake, so every client packet is forged.
//!
//! DATAGRAM frames carry the payload because they are never retransmitted: whatever an
//! iteration leaves unacknowledged is simply lost, and the next iteration starts clean.

use std::time::Duration;

use cpu_time::ThreadTime;

use bytes::BytesMut;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};

#[allow(
	dead_code,
	reason = "a harness shared by several benches, each using only part of it"
)]
mod common;

use common::{Forge, Pair, ServerPackets, Setup, ThreadCpu, ack_frame, datagram_frame, deliver, flush, payload, tick};

/// Frame bytes that fit one 1452-byte datagram after the header and tag, with room to spare
/// for only one more small frame, so N frames of `ROOM / N` bytes pack exactly N per packet.
const ROOM: usize = 1360;

/// A DATAGRAM payload that makes a frame of `ROOM / frames` bytes: a type byte and a two-byte
/// length precede it.
fn datagram(frames: usize) -> bytes::Bytes {
	payload(ROOM / frames - 3)
}

/// A quiet pair, a forger for its client, and a parser for its server's packet numbers.
fn quiet() -> (Pair, Forge, ServerPackets) {
	let setup = Setup::default();
	let parser = ServerPackets::new(setup.client_cid_len);
	let pair = Pair::new(setup);
	let forge = Forge::new(&pair);
	(pair, forge, parser)
}

/// Queues enough DATAGRAM frames on the server for about `packets` packets of `frames` each,
/// returning the free send buffer space once they are all sent.
///
/// A pacing burst can end mid-packet, so a flight runs a packet or two over.
fn queue(pair: &mut Pair, packets: usize, frames: usize) -> usize {
	let data = datagram(frames);
	let conn = pair.server.conn();
	// A forged packet the server rejected would close it quietly.
	assert!(!conn.is_closed(), "the server closed the connection");
	let mut datagrams = conn.datagrams();
	let empty = datagrams.send_buffer_space();
	for _ in 0..packets * frames {
		datagrams.send(data.clone(), Default::default()).unwrap();
	}
	empty
}

/// The packet numbers of the server's `datagrams`.
fn numbers(parser: &mut ServerPackets, datagrams: &[BytesMut]) -> Vec<u64> {
	datagrams.iter().map(|d| parser.number(d)).collect()
}

/// Acknowledges the packets in `pns` in one forged ACK, splitting it into about `ranges` ranges
/// by leaving evenly spaced packets out, which the server then declares lost.
fn ack(forge: &mut Forge, pns: &[u64], ranges: usize) -> BytesMut {
	let skip = pns.len() / ranges;
	let acked = pns
		.iter()
		.enumerate()
		.filter(|&(index, _)| ranges == 1 || index == 0 || index % skip != 0)
		.map(|(_, &pn)| pn);

	// Contiguous runs, highest first; packet numbers the server skipped split runs too.
	let mut runs: Vec<(u64, u64)> = Vec::new();
	for pn in acked {
		match runs.last_mut() {
			Some((_, end)) if *end + 1 == pn => *end = pn,
			_ => runs.push((pn, pn)),
		}
	}
	runs.reverse();

	let mut frames = BytesMut::new();
	ack_frame(&mut frames, &runs);
	forge.packet(&frames)
}

/// Packet encode: the server packs queued DATAGRAM frames, frames per packet against packets.
/// Only `poll_transmit` is timed; a forged ACK then clears the flight.
fn bench_encode(c: &mut Criterion<ThreadCpu>) {
	let mut group = c.benchmark_group("wire_encode");
	for frames in [1, 16] {
		for packets in [64, 1024] {
			let mut state = None;
			group.throughput(Throughput::Elements(packets as u64));
			group.bench_function(
				BenchmarkId::new(format!("frames_{frames}"), format!("packets_{packets}")),
				|b| {
					let (pair, forge, parser) = state.get_or_insert_with(quiet);
					b.iter_custom(|iters| {
						let mut busy = Duration::ZERO;
						for _ in 0..iters {
							let empty = queue(pair, packets, frames);
							let start = ThreadTime::now();
							let datagrams = flush(pair, empty);
							busy += start.elapsed();

							let sent = numbers(parser, &datagrams);
							deliver(pair, ack(forge, &sent, 1));
							tick(pair);
						}
						busy
					});
				},
			);
		}
	}
	group.finish();
}

/// Packet and frame decode: the server ingests forged packets of DATAGRAM frames, frames per
/// packet against packets. `padded` is one small DATAGRAM frame with PADDING filling the rest of
/// the packet, as a padded Initial or MTU probe arrives. Only the ingest is timed.
fn bench_decode(c: &mut Criterion<ThreadCpu>) {
	let mut group = c.benchmark_group("wire_decode");
	let mut shapes = Vec::new();
	for frames in [1, 16] {
		let data = datagram(frames);
		let mut body = BytesMut::new();
		for _ in 0..frames {
			datagram_frame(&mut body, &data);
		}
		shapes.push((format!("frames_{frames}"), frames, body));
	}
	let mut padded = BytesMut::new();
	datagram_frame(&mut padded, &payload(1));
	padded.resize(ROOM, 0);
	shapes.push(("padded".to_string(), 1, padded));
	for (shape, frames, body) in shapes {
		for packets in [64, 1024] {
			let mut state = None;
			group.throughput(Throughput::Elements(packets as u64));
			group.bench_function(BenchmarkId::new(&shape, format!("packets_{packets}")), |b| {
				let (pair, forge, _) = state.get_or_insert_with(quiet);
				b.iter_custom(|iters| {
					let mut busy = Duration::ZERO;
					for _ in 0..iters {
						let forged: Vec<_> = (0..packets).map(|_| forge.packet(&body)).collect();
						let start = ThreadTime::now();
						for packet in forged {
							deliver(pair, packet);
						}
						busy += start.elapsed();

						let conn = pair.server.conn();
						let mut received = 0;
						while conn.datagrams().recv().is_some() {
							received += 1;
						}
						assert_eq!(received, packets * frames, "server dropped datagrams");

						// Send the ACKs, which no forged packet acknowledges back.
						let empty = conn.datagrams().send_buffer_space();
						flush(pair, empty);
						tick(pair);
					}
					busy
				});
			});
		}
	}
	group.finish();
}

/// ACK processing: one forged ACK covers a flight of server packets in a number of ranges,
/// in-flight packets against ranges. Only the ACK's ingest is timed.
fn bench_ack(c: &mut Criterion<ThreadCpu>) {
	let mut group = c.benchmark_group("wire_ack");
	for packets in [256, 2048] {
		for ranges in [1, 16, 128] {
			let mut state = None;
			group.throughput(Throughput::Elements(packets as u64));
			group.bench_function(
				BenchmarkId::new(format!("packets_{packets}"), format!("ranges_{ranges}")),
				|b| {
					let (pair, forge, parser) = state.get_or_insert_with(quiet);
					b.iter_custom(|iters| {
						let mut busy = Duration::ZERO;
						for _ in 0..iters {
							let empty = queue(pair, packets, 1);
							let sent = numbers(parser, &flush(pair, empty));
							let ack = ack(forge, &sent, ranges);
							let start = ThreadTime::now();
							deliver(pair, ack);
							busy += start.elapsed();
							tick(pair);
						}
						busy
					});
				},
			);
		}
	}
	group.finish();
}

criterion_group! {
	name = benches;
	config = common::config();
	targets = bench_encode, bench_decode, bench_ack
}
criterion_main!(benches);
