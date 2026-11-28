//! Receive state across bursts: what a long-lived connection pays when a burst of streams
//! returns after the last one ended.
//!
//! The client opens `streams` uni streams of `packets` packets each and ends them.
//! `ordered` buffers the whole burst before the server's reader catches up, as a reader under
//! backpressure does; `loss` reads as data arrives while every eighth datagram is lost, so data
//! waits behind holes; `cancel` buffers the burst unread, then the reader stops every stream.
//!
//! Each case times the server's thread CPU over a burst on a connection that already freed the
//! previous one's streams, so the cost of allocating fresh receive state for each stream shows.
//!
//! Run with `cargo bench -p moq-quic --features fuzz --bench receive_bursts`.

use std::{fmt, task::Poll, time::Duration};

use moq_quic::stream::Dir;
use swole::{Iterations, bench};

#[allow(
	dead_code,
	reason = "a harness shared by several benches, each using only part of it"
)]
mod common;

use common::{Pair, Setup, Sink, WriteNow, payload};

/// Stream data per packet, so a stream of `n` packets spans about `n` datagrams.
const PACKET: usize = 1100;

/// Packets written between exchanges, about what one socket receive batch carries.
const BATCH: usize = 64;

/// How the server's application meets a burst.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
	Ordered,
	Loss,
	Cancel,
}

impl Mode {
	fn name(self) -> &'static str {
		match self {
			Self::Ordered => "ordered",
			Self::Loss => "loss",
			Self::Cancel => "cancel",
		}
	}
}

/// Opens `streams` streams of `packets` packets on every client, finishes them, and runs until
/// the server's application has read or stopped them all, so their receive state is freed.
fn burst(pair: &mut Pair, mode: Mode, streams: usize, packets: usize) {
	pair.lose_every = (mode == Mode::Loss).then_some(8);
	pair.server.sink = match mode {
		Mode::Loss => Sink::Read,
		Mode::Ordered | Mode::Cancel => Sink::Ignore,
	};
	open(pair, streams, packets);
	match mode {
		Mode::Loss => finish(pair, streams),
		Mode::Ordered | Mode::Cancel => {
			// Every byte has arrived once the clients forget their acknowledged streams.
			pair.drive_until(|pair| {
				pair.clients
					.iter_mut()
					.all(|client| client.conn().streams().send_streams() == 0)
			});
			let start = cpu_time::ThreadTime::now();
			for conn in pair.server.conns.values_mut() {
				while let Some(id) = conn.streams().accept(Dir::Uni) {
					let mut stream = conn.recv_stream(id);
					if mode == Mode::Cancel {
						stream.stop(0u32.into()).unwrap();
						continue;
					}
					let mut chunks = stream.read().unwrap();
					loop {
						match chunks.poll_next(&kio::Waiter::noop(), usize::MAX) {
							Poll::Ready(Ok(Some(chunk))) => pair.server.received.bytes += chunk.bytes.len() as u64,
							Poll::Ready(Ok(None)) => break,
							Poll::Ready(Err(err)) => panic!("read failed: {err}"),
							Poll::Pending => panic!("read blocked"),
						}
					}
					pair.server.received.finished += 1;
					chunks.finalize();
				}
			}
			pair.server.busy += start.elapsed();
		}
	}
	settle(pair);
}

/// Opens `streams` streams on every client, writes `packets` packets to each a packet per stream
/// at a time, then finishes them.
///
/// The exchange steps every [`BATCH`] packets, so the server takes the burst a receive batch at a
/// time, as a socket driver does, rather than every datagram before its application runs.
fn open(pair: &mut Pair, streams: usize, packets: usize) {
	let data = payload(PACKET);
	let ids: Vec<Vec<_>> = pair
		.clients
		.iter_mut()
		.map(|client| {
			let conn = client.conn();
			(0..streams)
				.map(|_| conn.streams().open(Dir::Uni, Default::default()).expect("stream limit"))
				.collect()
		})
		.collect();
	let mut written = 0;
	for _ in 0..packets {
		for (client, ids) in ids.iter().enumerate() {
			for &id in ids {
				let mut stream = pair.clients[client].conn().send_stream(id);
				assert_eq!(stream.write_now(&mut data.clone()), PACKET);
				written += 1;
				if written % BATCH == 0 {
					pair.step();
				}
			}
		}
	}
	for (client, ids) in ids.iter().enumerate() {
		for &id in ids {
			// Released, or each finished stream's end would stay on the client for the run.
			let mut stream = pair.clients[client].conn().send_stream(id);
			stream.finish().unwrap();
			stream.release();
		}
	}
}

/// Runs until the server's application has read `streams` more streams per client to the end.
fn finish(pair: &mut Pair, streams: usize) {
	let target = pair.server.received.finished + (pair.clients.len() * streams) as u64;
	pair.drive_until(|pair| pair.server.received.finished >= target);
}

/// Lets the exchange go quiet, checking every burst stream's receive state was freed.
fn settle(pair: &mut Pair) {
	pair.settle();
	for conn in pair.server.conns.values_mut() {
		assert_eq!(
			conn.streams().remote_open_streams(Dir::Uni),
			0,
			"a burst stream outlived it"
		);
	}
}

/// How the server meets a burst, against streams per burst and packets per stream.
#[derive(Clone, Copy)]
struct Case {
	mode: Mode,
	streams: usize,
	packets: usize,
}

impl fmt::Display for Case {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(
			f,
			"{}/streams_{}/packets_{}",
			self.mode.name(),
			self.streams,
			self.packets
		)
	}
}

fn cases() -> Vec<Case> {
	let mut cases = Vec::new();
	for mode in [Mode::Ordered, Mode::Loss, Mode::Cancel] {
		for streams in [1, 16, 256] {
			for packets in [1, 16, 256] {
				cases.push(Case { mode, streams, packets });
			}
		}
	}
	cases
}

/// A connection that has already taken and freed one burst.
fn burst_once(case: Case) -> (Case, Pair) {
	let mut pair = Pair::new(Setup::default());
	burst(&mut pair, case.mode, case.streams, case.packets);
	(case, pair)
}

/// The server's side of a burst on one connection that freed the previous burst's streams.
#[bench(args = cases(), setup = burst_once, bytes = |case: Case| case.streams * case.packets * PACKET)]
fn receive_bursts((case, pair): &mut (Case, Pair), iterations: Iterations) -> Duration {
	let mut busy = Duration::ZERO;
	for _ in iterations {
		pair.server.busy = Duration::ZERO;
		burst(pair, case.mode, case.streams, case.packets);
		busy += pair.server.busy;
	}
	busy
}

fn main() {
	swole::main();
}
