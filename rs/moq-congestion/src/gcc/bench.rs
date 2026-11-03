//! Controller-only per-packet measurements, as BBR3's: each ACK becomes GoogCC transport feedback,
//! so its cost grows with the packets each ACK carries.

use super::*;
use crate::Acked;
use cpu_time::ThreadTime;

const PACKET: u16 = 1200;

/// Times the controller callbacks for `flight` packets acknowledged `batch` at a time, refilled as
/// they are acknowledged if `steady`, each with a receive time
fn measure(flight: usize, batch: usize, steady: bool) -> Duration {
    let base = Instant::ZERO + Duration::from_secs(1);
    let mut gcc = Gcc::new(Init {
        now: base,
        mtu: PACKET,
        rtt: Duration::from_millis(10),
        max_pacing_rate: None,
    });
    let bytes = u64::from(PACKET);
    let mut sent = Vec::with_capacity(2 * flight);
    for pn in 0..flight {
        let now = base + Duration::from_micros(pn as u64);
        sent.push((
            now,
            gcc.on_sent(Sent {
                now,
                bytes: PACKET,
                in_flight: (pn as u64 + 1) * bytes,
            }),
        ));
    }
    let mut acked = Vec::with_capacity(batch);
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
        acked.extend(sent[first..end].iter().map(|&(at, packet)| Acked {
            sent: at,
            bytes: PACKET,
            packet,
            received: Some(at - base + Duration::from_millis(5)),
        }));
        gcc.on_ack(Ack {
            now,
            packets: &acked,
            in_flight,
            app_limited: false,
            rtt: Duration::from_millis(10),
        });
        if steady {
            for pn in first..end {
                let packet = gcc.on_sent(Sent {
                    now,
                    bytes: PACKET,
                    in_flight: in_flight + (pn - first + 1) as u64 * bytes,
                });
                sent.push((now, packet));
            }
        }
        elapsed += start.elapsed();
    }
    std::hint::black_box(&gcc);
    elapsed
}

#[test]
#[ignore = "controller benchmark: run with --release and --success-output immediate"]
fn benchmark_per_packet() {
    println!("gcc_packet controller_stack_bytes={}", size_of::<Gcc>());
    for steady in [false, true] {
        for flight in [16, 64, 1000, 4000, 16000] {
            for batch in [1, 8, 32] {
                let mut samples = [Duration::ZERO; 3];
                for sample in &mut samples {
                    *sample = measure(flight, batch, steady);
                }
                samples.sort();
                println!(
                    "gcc_packet steady={steady} flight={flight} batch={batch} cpu_ns={}",
                    samples[1].as_nanos()
                );
            }
        }
    }
}
