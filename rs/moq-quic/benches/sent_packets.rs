//! Sent-packet tracking: the inserts, lookups, walks, and removals that sending, ACK
//! processing, and loss detection make, with no connection around them.
//!
//! The first three scenarios are quinn-rs/quinn#2942's.

use std::hint::black_box;
use std::ops::{Bound, RangeBounds};

use criterion::{Criterion, criterion_group, criterion_main};
use moq_quic::fuzz::SentPackets;

#[allow(
	dead_code,
	reason = "a harness shared by several benches, each using only part of it"
)]
mod common;

use common::ThreadCpu;

/// A packet that counts toward congestion control.
const MTU: u16 = 1200;

/// Acknowledges every tracked packet in `range` but `skip`, as ACK processing collects then
/// removes them.
fn ack(packets: &mut SentPackets, range: impl RangeBounds<u64>, skip: Option<u64>) {
	let acked: Vec<u64> = packets
		.range(range)
		.map(|(pn, _)| pn)
		.filter(|&pn| Some(pn) != skip)
		.collect();
	for pn in acked {
		black_box(packets.remove(pn));
	}
}

/// Declares lost every tracked packet at least `threshold` below `largest`, walking everything
/// below `largest` as loss detection does.
fn detect_loss(packets: &mut SentPackets, largest: u64, threshold: u64) {
	let lost: Vec<u64> = packets
		.range(0..largest)
		.map(|(pn, _)| pn)
		.filter(|&pn| largest >= pn + threshold)
		.collect();
	for pn in lost {
		black_box(packets.remove(pn));
	}
}

/// Steady state of a healthy 200 Mbps connection at about 100 ms RTT: each round sends a
/// window, then the peer acknowledges all but the newest packet and one lost in the middle.
fn typical_connection(c: &mut Criterion<ThreadCpu>) {
	const WINDOW: u64 = 2048;
	const ROUNDS: u64 = 128;

	c.bench_function("sent_packets/typical_connection", |b| {
		b.iter(|| {
			let mut packets = SentPackets::default();
			for round in 0..ROUNDS {
				let base = round * WINDOW;
				for offset in 0..WINDOW {
					// One ACK-only reply per round.
					let eliciting = offset != 3;
					packets.insert(base + offset, if eliciting { MTU } else { 0 }, eliciting);
				}

				let largest = base + WINDOW - 2;
				black_box(packets.get(largest));
				for entry in packets.range(0..largest) {
					black_box(entry);
				}
				// Last round's lost packet ages out.
				let stale: Vec<u64> = packets.range(0..base).map(|(pn, _)| pn).collect();
				for pn in stale {
					black_box(packets.remove(pn));
				}
				ack(&mut packets, base..=largest, Some(base + WINDOW / 2));
				// A loss probe retransmits the oldest packet's data.
				for entry in packets.range(..).take(2) {
					black_box(entry);
				}
				black_box(packets.has_in_flight());
			}
			black_box(packets.get(ROUNDS * WINDOW - 1));
			black_box(packets.drain());
		})
	});
}

/// The peer stops acknowledging while ACK-only packets keep going out behind a pinned flight.
fn unacked_non_eliciting_tail(c: &mut Criterion<ThreadCpu>) {
	const PINNED: u64 = 50;
	const TAIL: u64 = 20_000;

	c.bench_function("sent_packets/unacked_non_eliciting_tail", |b| {
		b.iter(|| {
			let mut packets = SentPackets::default();
			for pn in 0..PINNED {
				packets.insert(pn, MTU, true);
			}
			for i in 0..TAIL {
				black_box(packets.insert(PINNED + i, 0, false));
				if i % 256 == 255 {
					// The loss timer keeps probing the pinned flight.
					black_box(packets.has_in_flight());
					black_box(packets.range(..).next());
					for entry in packets.range(0..PINNED) {
						black_box(entry);
					}
				}
			}
			black_box(packets.range((Bound::Excluded(PINNED - 1), Bound::Unbounded)).next());
		})
	});
}

/// The peer stops acknowledging, and each loss probe is ack-eliciting and never acknowledged.
fn pto_probe_holes(c: &mut Criterion<ThreadCpu>) {
	const PINNED: u64 = 50;
	const CYCLES: u64 = 8;
	const TAIL_PER_CYCLE: u64 = 2_000;

	c.bench_function("sent_packets/pto_probe_holes", |b| {
		b.iter(|| {
			let mut packets = SentPackets::default();
			for pn in 0..PINNED {
				packets.insert(pn, MTU, true);
			}
			let mut pn = PINNED;
			for _ in 0..CYCLES {
				for _ in 0..TAIL_PER_CYCLE {
					black_box(packets.insert(pn, 0, false));
					pn += 1;
				}
				packets.insert(pn, MTU, true);
				pn += 1;
				black_box(packets.has_in_flight());
				black_box(packets.range(..).next());
				for entry in packets.range(0..PINNED) {
					black_box(entry);
				}
			}
		})
	});
}

/// A moq subscriber: mostly ACK-only packets, with an ack-eliciting one every 64 that draws the
/// peer's ACK one round trip later. One ACK-only packet in 100 is lost.
fn ack_only_receiver(c: &mut Criterion<ThreadCpu>) {
	const PACKETS: u64 = 65_536;
	const ELICITING_EVERY: u64 = 64;
	/// Packets sent in one round trip.
	const RTT: u64 = 128;

	c.bench_function("sent_packets/ack_only_receiver", |b| {
		b.iter(|| {
			let mut packets = SentPackets::default();
			let mut acked = 0;
			for pn in 0..PACKETS {
				let eliciting = pn % ELICITING_EVERY == 0;
				black_box(packets.insert(pn, if eliciting { MTU } else { 0 }, eliciting));

				// The ACK for the ack-eliciting packet sent a round trip ago.
				if eliciting && pn >= RTT {
					let largest = pn - RTT;
					black_box(packets.get(largest));
					let acks = packets.range(acked..=largest).map(|(pn, _)| pn);
					let acks: Vec<u64> = acks.filter(|pn| pn % 100 != 99).collect();
					for pn in acks {
						black_box(packets.remove(pn));
					}
					detect_loss(&mut packets, largest, 3);
					black_box(packets.has_in_flight());
					acked = largest + 1;
				}
			}
			black_box(packets.drain());
		})
	});
}

/// A bulk sender with a 16k-packet window, about 1.5 Gbps at 100 ms: each ACK covers two
/// packets and repeats two older ranges, and two more go out. One packet in 1000 is lost, and
/// one in 512 is ACK-only.
fn bulk_sender(c: &mut Criterion<ThreadCpu>) {
	const WINDOW: u64 = 16_384;
	const ACKS: u64 = 65_536;

	c.bench_function("sent_packets/bulk_sender", |b| {
		b.iter(|| {
			let mut packets = SentPackets::default();
			let mut next = 0;
			let mut send = |packets: &mut SentPackets| {
				let eliciting = next % 512 != 511;
				packets.insert(next, if eliciting { MTU } else { 0 }, eliciting);
				next += 1;
			};
			for _ in 0..WINDOW {
				send(&mut packets);
			}
			for i in 0..ACKS {
				let largest = 2 * i + 1;
				black_box(packets.get(largest));
				// Older ranges the peer repeats find nothing left to acknowledge.
				for back in [8, 64] {
					let low = largest.saturating_sub(back);
					ack(&mut packets, low..=low + 1, None);
				}
				let lost = largest - largest % 1000;
				ack(&mut packets, largest - 1..=largest, Some(lost));
				detect_loss(&mut packets, largest, 3);
				black_box(packets.has_in_flight());
				send(&mut packets);
				send(&mut packets);
			}
			black_box(packets.drain());
		})
	});
}

fn benches(c: &mut Criterion<ThreadCpu>) {
	typical_connection(c);
	unacked_non_eliciting_tail(c);
	pto_probe_holes(c);
	ack_only_receiver(c);
	bulk_sender(c);
}

criterion_group! {
	name = group;
	config = common::config();
	targets = benches
}
criterion_main!(group);
