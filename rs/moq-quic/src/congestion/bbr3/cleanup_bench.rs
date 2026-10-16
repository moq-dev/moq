//! Controller-only cleanup measurements, including allocator-requested live bytes.

use super::*;
use cpu_time::ThreadTime;
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};
use std::alloc::System;

#[global_allocator]
static ALLOCATOR: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

const PACKET: u16 = 1200;

/// Fixture work and memory snapshots stay outside the timed controller callbacks.
fn measure(flight: usize, batch: usize, steady: bool, indexed: bool) -> (Duration, usize) {
    let base = Instant::now();
    let rtt = RttEstimator::new(Duration::from_millis(10));
    let region = Region::new(ALLOCATOR);
    let config = Bbr3Config {
        probe_rng_seed: Some([7; 16]),
        ..Default::default()
    };
    let mut bbr = Bbr3::new(Arc::new(config), PACKET);
    if !indexed {
        bbr.packets = Packets::scanned();
    }
    for pn in 0..flight {
        bbr.on_packet_sent(base, PACKET, pn as u64, SpaceId::Data);
    }
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
        let inflight = if steady {
            flight - (end - first)
        } else {
            flight - end
        };
        let start = ThreadTime::now();
        for pn in first..end {
            bbr.on_ack(
                now,
                base,
                u64::from(PACKET),
                pn as u64,
                SpaceId::Data,
                false,
                &rtt,
            );
        }
        bbr.on_end_acks(
            now,
            inflight as u64 * u64::from(PACKET),
            false,
            Some((end - 1) as u64),
            SpaceId::Data,
        );
        if steady {
            for pn in first..end {
                bbr.on_packet_sent(now, PACKET, (flight + pn) as u64, SpaceId::Data);
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
fn benchmark_ack_cleanup() {
    println!(
        "bbr_cleanup controller_test_stack_bytes={}",
        size_of::<Bbr3>()
    );
    for steady in [false, true] {
        for flight in [16, 64, 1000, 4000, 16000] {
            for batch in [1, 8, 32] {
                for indexed in [false, true] {
                    let mut samples = [Duration::ZERO; 3];
                    let mut bytes = 0;
                    for sample in &mut samples {
                        let (elapsed, retained) = measure(flight, batch, steady, indexed);
                        *sample = elapsed;
                        bytes = bytes.max(retained);
                    }
                    samples.sort();
                    println!(
                        "bbr_cleanup indexed={indexed} steady={steady} flight={flight} batch={batch} cpu_ns={} peak_bytes={bytes}",
                        samples[1].as_nanos()
                    );
                }
            }
        }
    }
}
