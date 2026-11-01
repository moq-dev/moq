//! Controller-only per-packet measurements, including allocator-requested live bytes.

use super::*;
use crate::Acked;
use cpu_time::ThreadTime;
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};
use std::alloc::System;

#[global_allocator]
static ALLOCATOR: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

const PACKET: u16 = 1200;

/// Times the controller callbacks for `flight` packets acknowledged `batch` at a time, refilled as
/// they are acknowledged if `steady`. The timed region includes copying each packet's state into
/// the ACK, as a transport does, and excludes the fixture's own bookkeeping.
fn measure(flight: usize, batch: usize, steady: bool) -> (Duration, usize) {
    let base = Instant::now();
    let region = Region::new(ALLOCATOR);
    let config = Bbr3Config {
        probe_rng_seed: Some([7; 16]),
        ..Default::default()
    };
    let init = Init {
        now: base,
        mtu: PACKET,
        rtt: Duration::from_millis(10),
        max_pacing_rate: None,
    };
    let mut bbr = Bbr3::new(Arc::new(config), init);
    let bytes = u64::from(PACKET);
    let mut sent = Vec::with_capacity(2 * flight);
    for pn in 0..flight {
        sent.push(bbr.on_sent(Sent {
            now: base,
            bytes: PACKET,
            in_flight: (pn as u64 + 1) * bytes,
        }));
    }
    let mut acked = Vec::with_capacity(batch);
    let retained = || {
        let stats = region.change();
        stats
            .bytes_allocated
            .saturating_sub(stats.bytes_deallocated)
    };
    let mut peak = retained();
    let mut elapsed = Duration::ZERO;
    for first in (0..flight).step_by(batch) {
        let end = (first + batch).min(flight);
        let now = base + Duration::from_millis(10) + Duration::from_micros(first as u64);
        let in_flight = if steady {
            flight - (end - first)
        } else {
            flight - end
        } as u64
            * bytes;
        let start = ThreadTime::now();
        acked.clear();
        acked.extend(sent[first..end].iter().map(|&packet| Acked {
            sent: base,
            bytes: PACKET,
            packet,
            received: None,
        }));
        bbr.on_ack(Ack {
            now,
            packets: &acked,
            in_flight,
            app_limited: false,
            rtt: Duration::from_millis(10),
        });
        if steady {
            for pn in first..end {
                let packet = bbr.on_sent(Sent {
                    now,
                    bytes: PACKET,
                    in_flight: in_flight + (pn - first + 1) as u64 * bytes,
                });
                sent.push(packet);
            }
        }
        elapsed += start.elapsed();
        peak = peak.max(retained());
    }
    std::hint::black_box(&bbr);
    (elapsed, peak)
}

#[test]
#[ignore = "controller benchmark: run with --release and --success-output immediate"]
fn benchmark_per_packet() {
    println!(
        "bbr_packet controller_stack_bytes={} packet_state_bytes={}",
        size_of::<Bbr3>(),
        size_of::<Packet>()
    );
    for steady in [false, true] {
        for flight in [16, 64, 1000, 4000, 16000] {
            for batch in [1, 8, 32] {
                let mut samples = [Duration::ZERO; 3];
                let mut bytes = 0;
                for sample in &mut samples {
                    let (elapsed, retained) = measure(flight, batch, steady);
                    *sample = elapsed;
                    bytes = bytes.max(retained);
                }
                samples.sort();
                println!(
                    "bbr_packet steady={steady} flight={flight} batch={batch} cpu_ns={} peak_bytes={bytes}",
                    samples[1].as_nanos()
                );
            }
        }
    }
}
