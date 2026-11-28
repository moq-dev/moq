//! Receive, reassembly, and reset: what a server pays to ingest, and to abandon, many streams.

use std::{fmt, task::Poll, time::Duration};

use bytes::Bytes;
use moq_quic::{VarInt, recv, stream::Dir};
use swole::{Iterations, bench};

#[allow(
	dead_code,
	reason = "a harness shared by several benches, each using only part of it"
)]
mod common;

use common::{Pair, Setup, Sink, WriteNow, payload};

/// Stream data per packet, so a stream of `n` packets spans about `n` datagrams.
const PACKET: usize = 1100;

/// Streams against packets per stream.
#[derive(Clone, Copy)]
struct Ingest {
	streams: usize,
	packets: usize,
}

impl Ingest {
	fn bytes(self) -> usize {
		self.streams * self.packets * PACKET
	}
}

impl fmt::Display for Ingest {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "streams_{}/packets_{}", self.streams, self.packets)
	}
}

fn ingests() -> Vec<Ingest> {
	[1, 16, 256]
		.into_iter()
		.flat_map(|streams| [2, 16].map(|packets| Ingest { streams, packets }))
		.collect()
}

fn lossy(case: Ingest) -> (Ingest, Pair) {
	let mut pair = Pair::new(Setup::default());
	pair.lose_every = Some(8);
	(case, pair)
}

/// Out-of-order ingest: every eighth client datagram is lost, so the server buffers data past a
/// hole in many partial streams at once until each retransmission arrives.
///
/// Times the server's thread CPU alone, not the client's side of the exchange.
#[bench(args = ingests(), setup = lossy, bytes = Ingest::bytes)]
fn receive_loss((case, pair): &mut (Ingest, Pair), iterations: Iterations) -> Duration {
	let data = payload(case.packets * PACKET);
	let mut busy = Duration::ZERO;
	for _ in iterations {
		pair.server.busy = Duration::ZERO;
		let target = pair.server.received.finished + case.streams as u64;
		let conn = pair.client();
		for _ in 0..case.streams {
			let id = conn.streams().open(Dir::Uni, Default::default()).expect("stream limit");
			let mut stream = conn.send_stream(id);
			assert_eq!(stream.write_now(&mut data.clone()), data.len());
			stream.finish().unwrap();
		}
		pair.drive_until(|pair| pair.server.received.finished >= target);
		pair.settle();
		busy += pair.server.busy;
	}
	busy
}

/// Streams against the bytes each carries.
#[derive(Clone, Copy)]
struct Abandon {
	streams: usize,
	bytes: usize,
}

impl fmt::Display for Abandon {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "streams_{}/bytes_{}", self.streams, self.bytes)
	}
}

fn abandons(streams: [usize; 3], bytes: [usize; 2]) -> Vec<Abandon> {
	streams
		.into_iter()
		.flat_map(|streams| bytes.map(|bytes| Abandon { streams, bytes }))
		.collect()
}

/// A pair and the payload each stream carries.
struct Abandoning {
	case: Abandon,
	pair: Pair,
	data: Bytes,
}

fn abandoning(case: Abandon) -> Abandoning {
	Abandoning {
		case,
		pair: Pair::new(Setup::default()),
		data: payload(case.bytes),
	}
}

/// Sender resets: streams carry `bytes` each, of which one burst leaves before every stream is
/// reset, and the exchange runs until the server has seen every reset.
#[bench(args = abandons([16, 256, 1024], [0, 16384]), setup = abandoning, items = |case: Abandon| case.streams)]
fn receive_reset(state: &mut Abandoning) {
	let Abandon { streams, bytes } = state.case;
	let (pair, data) = (&mut state.pair, &state.data);
	let target = pair.server.received.reset + streams as u64;
	let conn = pair.client();
	let ids: Vec<_> = (0..streams)
		.map(|_| {
			let id = conn.streams().open(Dir::Uni, Default::default()).expect("stream limit");
			assert_eq!(conn.send_stream(id).write_now(&mut data.clone()), bytes);
			id
		})
		.collect();
	pair.step();
	let conn = pair.client();
	for id in ids {
		conn.send_stream(id).reset(VarInt::from_u32(1)).unwrap();
	}
	pair.drive_until(|pair| pair.server.received.reset >= target);
	pair.settle();
}

/// How the server reads, against streams and packets per stream.
#[derive(Clone, Copy)]
struct Lease {
	lease: bool,
	ingest: Ingest,
}

impl fmt::Display for Lease {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		let read = if self.lease { "lease" } else { "read" };
		write!(f, "{read}/{}", self.ingest)
	}
}

fn leases() -> Vec<Lease> {
	[false, true]
		.into_iter()
		.flat_map(|lease| ingests().into_iter().map(move |ingest| Lease { lease, ingest }))
		.collect()
}

fn leasing(case: Lease) -> (Ingest, Pair) {
	let mut pair = Pair::new(Setup::default());
	pair.server.sink = if case.lease { Sink::Lease(1) } else { Sink::Read };
	(case.ingest, pair)
}

/// In-order ingest read plainly or through leases, so the lease path's cost shows beside the plain
/// read's as the stream count grows.
///
/// Times the server's thread CPU alone, not the client's side of the exchange.
#[bench(args = leases(), setup = leasing, bytes = |case: Lease| case.ingest.bytes())]
fn receive_lease((case, pair): &mut (Ingest, Pair), iterations: Iterations) -> Duration {
	let data = payload(case.packets * PACKET);
	let mut busy = Duration::ZERO;
	for _ in iterations {
		pair.server.busy = Duration::ZERO;
		let target = pair.server.received.finished + case.streams as u64;
		for _ in 0..case.streams {
			let id = pair.open_uni(&data);
			pair.client().send_stream(id).finish().unwrap();
		}
		pair.drive_until(|pair| pair.server.received.finished >= target);
		pair.settle();
		busy += pair.server.busy;
	}
	busy
}

/// A pair whose server's receive window fits `streams` packets, each reader waiting for all of
/// it, which no single stream fills.
fn stuck(streams: usize) -> (usize, Pair) {
	let window = streams * PACKET;
	let mut pair = Pair::new(Setup {
		server_receive_window: VarInt::from_u64(window as u64).unwrap(),
		..Default::default()
	});
	pair.server.sink = Sink::Lease(window);
	(streams, pair)
}

mod receive_evict {
	use std::time::Duration;

	use moq_quic::VarInt;
	use swole::{Iterations, bench};

	use super::{PACKET, Pair, payload, stuck};

	/// A stuck receive window: every stream holds one packet of a unit that never completes, the
	/// last one fills the window, and the server evicts one stream to free it.
	///
	/// The stuck pass ranks every holder, so the cost per stream must stay flat as the count grows.
	/// Times the server's thread CPU alone, filling the window and evicting.
	#[bench(args = [16, 64, 256, 1024, 4096], setup = stuck, items = |streams: usize| streams)]
	fn streams((streams, pair): &mut (usize, Pair), iterations: Iterations) -> Duration {
		let data = payload(PACKET);
		let mut busy = Duration::ZERO;
		for _ in iterations {
			pair.server.busy = Duration::ZERO;
			let target = pair.server.received.evicted + 1;
			let ids: Vec<_> = (0..*streams).map(|_| pair.open_uni(&data)).collect();
			pair.drive_until(|pair| pair.server.received.evicted >= target);
			busy += pair.server.busy;

			// Reset the rest, which returns their credit for the next round.
			let conn = pair.client();
			for id in ids {
				let _ = conn.send_stream(id).reset(VarInt::from_u32(0));
			}
			pair.drive_until(|pair| pair.client().streams().send_streams() == 0);
			pair.settle();
		}
		busy
	}
}

mod receive_evict_churn {
	use std::time::Duration;

	use swole::{Iterations, bench};

	use super::{PACKET, Pair, payload, stuck};

	/// A stuck window `streams` holders keep full, with one stream already evicted.
	fn churning(streams: usize) -> Pair {
		let (_, mut pair) = stuck(streams);
		let data = payload(PACKET);
		for _ in 0..streams {
			pair.open_uni(&data);
		}
		pair.drive_until(|pair| pair.server.received.evicted >= 1);
		pair.settle();
		pair
	}

	/// Repeated eviction: `streams` holders keep a stuck window full while the client spends each
	/// credit an eviction frees on one more incomplete unit, forcing one eviction per round trip.
	///
	/// One eviction per iteration, timing the server's thread CPU alone, so its cost must not
	/// grow with the holders.
	#[bench(args = [16, 256, 2048], setup = churning, items = |_: usize| 1)]
	fn streams(pair: &mut Pair, iterations: Iterations) -> Duration {
		let data = payload(PACKET);
		let mut busy = Duration::ZERO;
		for _ in iterations {
			pair.server.busy = Duration::ZERO;
			let target = pair.server.received.evicted + 1;
			pair.open_uni(&data);
			pair.drive_until(|pair| pair.server.received.evicted >= target);
			busy += pair.server.busy;
			// The evicted stream's stop and reset settle untimed.
			pair.settle();
		}
		busy
	}
}

/// Leased holders against the unread streams reset into a shrunk window.
#[derive(Clone, Copy)]
struct Debt {
	holders: usize,
	resets: usize,
}

impl Debt {
	fn window(self) -> usize {
		(self.holders + self.resets) * PACKET
	}
}

impl fmt::Display for Debt {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "holders_{}/resets_{}", self.holders, self.resets)
	}
}

fn debts() -> Vec<Debt> {
	[16, 256, 2048]
		.into_iter()
		.flat_map(|holders| [16, 256].map(|resets| Debt { holders, resets }))
		.collect()
}

fn indebted(case: Debt) -> (Debt, Pair) {
	let window = case.window();
	let mut pair = Pair::new(Setup {
		server_receive_window: VarInt::from_u64(window as u64).unwrap(),
		..Default::default()
	});
	// Each reader waits for the whole window, which no single stream fills.
	pair.server.sink = Sink::Lease(window);
	let data = payload(PACKET);
	for _ in 0..case.holders {
		pair.open_uni(&data);
	}
	pair.settle();
	pair.server.sink = Sink::Ignore;
	(case, pair)
}

/// Resets into a shrunk window: `holders` leased streams wait on a window that `resets` unread
/// streams fill, the server shrinks its window, and the client resets the unread streams in one
/// burst. Each reset frees credit that only pays the shrink debt, so the window stays stuck and
/// every RESET_STREAM frame reruns the stuck check.
///
/// Times the server's thread CPU alone over the burst, and the cost per reset must not grow with
/// the holders.
#[bench(args = debts(), setup = indebted, items = |case: Debt| case.resets)]
fn receive_reset_debt((case, pair): &mut (Debt, Pair), iterations: Iterations) -> Duration {
	let Debt { holders, resets } = *case;
	let window = case.window();
	// Paid by all but the last reset, which frees the window again.
	let debt = (resets - 1) * PACKET;
	let data = payload(PACKET);
	let mut busy = Duration::ZERO;
	for _ in iterations {
		let shrunk = VarInt::from_u64((window - debt) as u64).unwrap();
		let mut limits = moq_quic::Limits::default();
		limits.receive_window = std::num::NonZeroU64::new(shrunk.into_inner());
		pair.server.conn().set_limits(limits).unwrap();
		let conn = pair.client();
		let ids: Vec<_> = (0..resets)
			.map(|_| {
				let id = conn.streams().open(Dir::Uni, Default::default()).expect("stream limit");
				assert_eq!(conn.send_stream(id).write_now(&mut data.clone()), PACKET);
				id
			})
			.collect();
		pair.settle();

		pair.server.busy = Duration::ZERO;
		let conn = pair.client();
		for id in ids {
			conn.send_stream(id).reset(VarInt::from_u32(0)).unwrap();
		}
		pair.drive_until(|pair| pair.client().streams().send_streams() == holders);
		busy += pair.server.busy;

		// Read the resets, freeing the streams, and restore the window.
		let conn = pair.server.conn();
		while let Some(id) = conn.streams().accept(Dir::Uni) {
			let mut stream = conn.recv_stream(id);
			let mut chunks = stream.read().unwrap();
			assert!(matches!(
				chunks.poll_next(&kio::Waiter::noop(), usize::MAX),
				Poll::Ready(Err(recv::Error::Reset))
			));
			chunks.finalize();
			assert!(stream.received_reset().unwrap().is_some());
		}
		let mut limits = moq_quic::Limits::default();
		limits.receive_window = std::num::NonZeroU64::new(window as u64);
		conn.set_limits(limits).unwrap();
		pair.settle();
	}
	busy
}

fn stopping(case: Abandon) -> Abandoning {
	let mut state = abandoning(case);
	state.pair.server.sink = Sink::Stop;
	state
}

/// Receiver stops: the server sends STOP_SENDING for every stream as it opens, and the exchange
/// runs until the client has reset and forgotten them all.
#[bench(args = abandons([16, 256, 1024], [1100, 16384]), setup = stopping, items = |case: Abandon| case.streams)]
fn receive_stop(state: &mut Abandoning) {
	let pair = &mut state.pair;
	let target = pair.server.received.stopped + state.case.streams as u64;
	for _ in 0..state.case.streams {
		pair.open_uni(&state.data);
	}
	pair.drive_until(|pair| pair.server.received.stopped >= target && pair.client().streams().send_streams() == 0);
	pair.settle();
}

fn main() {
	swole::main();
}
