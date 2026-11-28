//! Wire codec and ACKs: packet and frame encode, decode, and ACK processing on one server
//! connection whose real client goes quiet after the handshake, so every client packet is forged.
//!
//! DATAGRAM frames carry the payload because they are never retransmitted: whatever a
//! call leaves unacknowledged is simply lost, and the next iteration starts clean.

use std::fmt;

use bytes::BytesMut;
use swole::bench;

#[allow(
	dead_code,
	reason = "a harness shared by several benches, each using only part of it"
)]
mod common;

use common::{Forge, Pair, ServerPackets, Setup, ack_frame, datagram_frame, deliver, flush, payload, tick};

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

/// A quiet pair and its forger, with what the steps around each call hand on.
struct Quiet {
	pair: Pair,
	forge: Forge,
	parser: ServerPackets,
	/// Free datagram send buffer space once everything queued is sent.
	empty: usize,
	/// The server's datagrams from the last call, for the step after it to acknowledge.
	sent: Vec<BytesMut>,
	/// Forged packets for the next call to deliver.
	forged: Vec<BytesMut>,
	/// A forged ACK for the next call to deliver.
	ack: Option<BytesMut>,
}

impl Quiet {
	fn new() -> Self {
		let (pair, forge, parser) = quiet();
		Self {
			pair,
			forge,
			parser,
			empty: 0,
			sent: Vec::new(),
			forged: Vec::new(),
			ack: None,
		}
	}
}

/// Frames per packet against packets.
#[derive(Clone, Copy)]
struct Encode {
	frames: usize,
	packets: usize,
}

impl fmt::Display for Encode {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "frames_{}/packets_{}", self.frames, self.packets)
	}
}

fn encode_cases() -> Vec<Encode> {
	[1, 16]
		.into_iter()
		.flat_map(|frames| [64, 1024].map(|packets| Encode { frames, packets }))
		.collect()
}

struct Encoding {
	case: Encode,
	quiet: Quiet,
}

fn encoding(case: Encode) -> Encoding {
	Encoding {
		case,
		quiet: Quiet::new(),
	}
}

fn queue_flight(state: &mut Encoding) {
	state.quiet.empty = queue(&mut state.quiet.pair, state.case.packets, state.case.frames);
}

/// A forged ACK clears the flight.
fn ack_flight(state: &mut Encoding) {
	let quiet = &mut state.quiet;
	let sent = numbers(&mut quiet.parser, &std::mem::take(&mut quiet.sent));
	deliver(&mut quiet.pair, ack(&mut quiet.forge, &sent, 1));
	tick(&mut quiet.pair);
}

/// Packet encode: the server packs queued DATAGRAM frames, frames per packet against packets.
/// Only `poll_transmit` is timed.
#[bench(args = encode_cases(), setup = encoding, before = queue_flight, after = ack_flight, items = |case: Encode| case.packets)]
fn wire_encode(state: &mut Encoding) {
	state.quiet.sent = flush(&mut state.quiet.pair, state.quiet.empty);
}

/// The packet bodies a decode case forges.
#[derive(Clone, Copy)]
enum Shape {
	/// This many DATAGRAM frames filling the packet.
	Frames(usize),
	/// One small DATAGRAM frame with PADDING filling the rest of the packet.
	Padded,
}

/// Packet shape against packets.
#[derive(Clone, Copy)]
struct Decode {
	shape: Shape,
	packets: usize,
}

impl fmt::Display for Decode {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		match self.shape {
			Shape::Frames(frames) => write!(f, "frames_{frames}")?,
			Shape::Padded => write!(f, "padded")?,
		}
		write!(f, "/packets_{}", self.packets)
	}
}

fn decode_cases() -> Vec<Decode> {
	[Shape::Frames(1), Shape::Frames(16), Shape::Padded]
		.into_iter()
		.flat_map(|shape| [64, 1024].map(|packets| Decode { shape, packets }))
		.collect()
}

struct Decoding {
	packets: usize,
	/// DATAGRAM frames per packet.
	frames: usize,
	body: BytesMut,
	quiet: Quiet,
}

fn decoding(case: Decode) -> Decoding {
	let (frames, body) = match case.shape {
		Shape::Frames(frames) => {
			let data = datagram(frames);
			let mut body = BytesMut::new();
			for _ in 0..frames {
				datagram_frame(&mut body, &data);
			}
			(frames, body)
		}
		Shape::Padded => {
			let mut body = BytesMut::new();
			datagram_frame(&mut body, &payload(1));
			body.resize(ROOM, 0);
			(1, body)
		}
	};
	Decoding {
		packets: case.packets,
		frames,
		body,
		quiet: Quiet::new(),
	}
}

fn forge_flight(state: &mut Decoding) {
	let quiet = &mut state.quiet;
	quiet.forged = (0..state.packets).map(|_| quiet.forge.packet(&state.body)).collect();
}

/// Checks every datagram arrived and sends the ACKs, which no forged packet acknowledges back.
fn drain_flight(state: &mut Decoding) {
	let pair = &mut state.quiet.pair;
	let conn = pair.server.conn();
	let mut received = 0;
	while conn.datagrams().recv().is_some() {
		received += 1;
	}
	assert_eq!(received, state.packets * state.frames, "server dropped datagrams");

	let empty = conn.datagrams().send_buffer_space();
	flush(pair, empty);
	tick(pair);
}

/// Packet and frame decode: the server ingests forged packets of DATAGRAM frames, packet shape
/// against packets. `padded` is how a padded Initial or MTU probe arrives. Only the ingest is
/// timed.
#[bench(args = decode_cases(), setup = decoding, before = forge_flight, after = drain_flight, items = |case: Decode| case.packets)]
fn wire_decode(state: &mut Decoding) {
	for packet in std::mem::take(&mut state.quiet.forged) {
		deliver(&mut state.quiet.pair, packet);
	}
}

/// In-flight packets against ACK ranges.
#[derive(Clone, Copy)]
struct Ack {
	packets: usize,
	ranges: usize,
}

impl fmt::Display for Ack {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "packets_{}/ranges_{}", self.packets, self.ranges)
	}
}

fn ack_cases() -> Vec<Ack> {
	[256, 2048]
		.into_iter()
		.flat_map(|packets| [1, 16, 128].map(|ranges| Ack { packets, ranges }))
		.collect()
}

struct Acking {
	case: Ack,
	quiet: Quiet,
}

fn acking(case: Ack) -> Acking {
	Acking {
		case,
		quiet: Quiet::new(),
	}
}

/// Sends a flight and forges the ACK the next call delivers.
fn send_flight(state: &mut Acking) {
	let quiet = &mut state.quiet;
	let empty = queue(&mut quiet.pair, state.case.packets, 1);
	let sent = numbers(&mut quiet.parser, &flush(&mut quiet.pair, empty));
	quiet.ack = Some(ack(&mut quiet.forge, &sent, state.case.ranges));
}

fn tick_flight(state: &mut Acking) {
	tick(&mut state.quiet.pair);
}

/// ACK processing: one forged ACK covers a flight of server packets in a number of ranges,
/// in-flight packets against ranges. Only the ACK's ingest is timed.
#[bench(args = ack_cases(), setup = acking, before = send_flight, after = tick_flight, items = |case: Ack| case.packets)]
fn wire_ack(state: &mut Acking) {
	let ack = state.quiet.ack.take().expect("a flight to acknowledge");
	deliver(&mut state.quiet.pair, ack);
}

fn main() {
	swole::main();
}
