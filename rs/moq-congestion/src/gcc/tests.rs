//! [`Gcc`] over a simulated uplink on mocked time: a FIFO bottleneck with a rate and a queue
//! bounded in time, a fixed propagation delay each way, and a receiver that acknowledges what
//! arrived each millisecond with its receive times.

use std::collections::VecDeque;

use super::*;
use crate::{Acked, Lost};

const MTU: u16 = 1200;
/// One-way propagation delay, each way
const DELAY: Duration = Duration::from_millis(20);
/// How long after sending a dropped packet the sender declares it lost without three later
/// packets acknowledged, standing in for QUIC's time threshold and probe timeout
const LOSS_TIMEOUT: Duration = Duration::from_secs(1);

/// A packet between the sender and its ACK
struct Flight {
    sent: Instant,
    bytes: u16,
    packet: Packet,
    /// When it reaches the receiver, or `None` if the bottleneck dropped it
    arrival: Option<Instant>,
}

/// What the sender has to send
#[derive(Clone, Copy)]
enum Source {
    /// Always more than the controller allows
    Bulk,
    /// Media at this many bytes per second, in 30 frames each second
    Media(u64),
}

struct Sim {
    gcc: Gcc,
    epoch: Instant,
    now: Instant,
    /// The bottleneck's rate in bytes per second
    rate: u64,
    /// The longest a packet waits in the bottleneck before it is dropped
    queue: Duration,
    /// When the bottleneck finishes serving what it holds
    busy_until: Instant,
    flight: VecDeque<Flight>,
    in_flight: u64,
    source: Source,
    /// Media bytes written and not yet sent
    backlog: u64,
    next_frame: Instant,
    /// Whether the sender fills the controller's probe with duplicates, as moq-quic's native
    /// probing does
    fill_probes: bool,
    /// Whether the receiver reports receive times
    timestamps: bool,
    /// When each delivered packet reached the bottleneck, and its queueing delay there
    queued: Vec<(Duration, Duration)>,
}

impl Sim {
    fn new(rate: u64, source: Source) -> Self {
        let epoch = Instant::ZERO + Duration::from_secs(1000);
        let gcc = Gcc::new(Init {
            now: epoch,
            mtu: MTU,
            rtt: Duration::from_millis(333),
            max_pacing_rate: None,
        });
        Self {
            gcc,
            epoch,
            now: epoch,
            rate,
            queue: Duration::from_millis(500),
            busy_until: epoch,
            flight: VecDeque::new(),
            in_flight: 0,
            source,
            backlog: 0,
            next_frame: epoch,
            fill_probes: false,
            timestamps: true,
            queued: Vec::new(),
        }
    }

    fn elapsed(&self) -> Duration {
        self.now - self.epoch
    }

    /// Runs until `until` since the start, in steps of a millisecond
    fn run(&mut self, until: Duration) {
        while self.elapsed() < until {
            self.ack();
            self.write();
            self.send();
            self.now += Duration::from_millis(1);
        }
    }

    fn write(&mut self) {
        if let Source::Media(rate) = self.source {
            while self.next_frame <= self.now {
                self.backlog += rate / 30;
                self.next_frame += Duration::from_secs(1) / 30;
            }
        }
    }

    fn send(&mut self) {
        loop {
            let duplicate = match self.source {
                Source::Bulk => false,
                Source::Media(_) if self.backlog > 0 => false,
                Source::Media(_) => {
                    let probe = self.gcc.metrics().probe_inflight;
                    if !self.fill_probes
                        || probe.is_none_or(|probe| self.in_flight + u64::from(MTU) > probe)
                    {
                        self.gcc.on_app_limited(self.in_flight);
                        return;
                    }
                    true
                }
            };
            if self.in_flight + u64::from(MTU) > self.gcc.window() {
                return;
            }
            if self.gcc.pace(self.now, MTU.into()).is_some() {
                return;
            }
            if duplicate {
                self.gcc.on_probe_filled(self.in_flight);
            } else {
                self.backlog = self.backlog.saturating_sub(MTU.into());
            }
            self.in_flight += u64::from(MTU);
            let packet = self.gcc.on_sent(Sent {
                now: self.now,
                bytes: MTU,
                in_flight: self.in_flight,
            });
            let start = self.busy_until.max(self.now);
            let wait = start - self.now;
            let arrival = (wait <= self.queue).then(|| {
                self.busy_until =
                    start + Duration::from_secs_f64(f64::from(MTU) / self.rate as f64);
                self.queued.push((self.elapsed(), wait));
                self.busy_until + DELAY
            });
            self.flight.push_back(Flight {
                sent: self.now,
                bytes: MTU,
                packet,
                arrival,
            });
        }
    }

    /// Acknowledges what arrived a propagation delay ago, and declares lost a packet the
    /// bottleneck dropped once three later packets were acknowledged or [`LOSS_TIMEOUT`] passed
    fn ack(&mut self) {
        let acked_by = |arrival: Instant| arrival + DELAY;
        let mut acked = Vec::new();
        let mut lost = Vec::new();
        while let Some(front) = self.flight.front() {
            match front.arrival {
                Some(arrival) if acked_by(arrival) <= self.now => {}
                Some(_) => break,
                None => {
                    let acked_later = self
                        .flight
                        .iter()
                        .skip(1)
                        .take(3)
                        .filter(|f| f.arrival.is_some_and(|a| acked_by(a) <= self.now))
                        .count();
                    if acked_later < 3 && self.now < front.sent + LOSS_TIMEOUT {
                        break;
                    }
                }
            }
            let flight = self.flight.pop_front().unwrap();
            self.in_flight -= u64::from(flight.bytes);
            match flight.arrival {
                Some(arrival) => acked.push(Acked {
                    sent: flight.sent,
                    bytes: flight.bytes,
                    packet: flight.packet,
                    received: self.timestamps.then(|| arrival - self.epoch),
                }),
                None => lost.push(Lost {
                    sent: flight.sent,
                    bytes: flight.bytes,
                    packet: flight.packet,
                    new_burst: true,
                }),
            }
        }
        if !lost.is_empty() {
            self.gcc.on_loss(Loss {
                now: self.now,
                packets: &lost,
                in_flight: self.in_flight,
                persistent: false,
            });
        }
        if !acked.is_empty() {
            self.gcc.on_ack(Ack {
                now: self.now,
                packets: &acked,
                in_flight: self.in_flight,
                app_limited: false,
                rtt: 2 * DELAY,
            });
        }
    }

    /// The bandwidth estimate, in bytes per second
    fn estimate(&self) -> u64 {
        self.gcc.metrics().bandwidth_estimate.unwrap()
    }

    /// The `p` quantile of the queueing delay of packets reaching the bottleneck since `from`
    fn queue(&self, from: Duration, p: f64) -> Duration {
        let mut delays: Vec<Duration> = self
            .queued
            .iter()
            .filter(|(at, _)| *at >= from)
            .map(|&(_, delay)| delay)
            .collect();
        delays.sort_unstable();
        delays[((delays.len() - 1) as f64 * p) as usize]
    }
}

/// 2 Mbit/s in bytes per second
const LINK: u64 = 250_000;

/// A bulk upload's initial probes find the link within a few seconds, and the delay-based
/// estimate then holds the queue near empty, below the link's rate
#[test]
fn finds_the_link_and_keeps_the_queue_short() {
    let mut sim = Sim::new(LINK, Source::Bulk);
    sim.run(Duration::from_secs(10));
    let estimate = sim.estimate();
    assert!(
        (LINK / 2..=LINK * 11 / 10).contains(&estimate),
        "estimate {estimate} B/s on a {LINK} B/s link"
    );
    let queue = sim.queue(Duration::from_secs(5), 0.95);
    assert!(queue < Duration::from_millis(50), "p95 queue {queue:?}");
}

/// When the link drops to a quarter, the queue's growth cuts the estimate to the new rate
/// within a couple of seconds, and the queue drains
#[test]
fn backs_off_a_capacity_drop() {
    let mut sim = Sim::new(LINK, Source::Bulk);
    sim.run(Duration::from_secs(10));
    sim.rate = LINK / 4;
    sim.run(Duration::from_secs(12));
    let estimate = sim.estimate();
    assert!(
        estimate <= LINK / 4 * 11 / 10,
        "estimate {estimate} B/s 2s after the link fell to {} B/s",
        LINK / 4
    );
    sim.run(Duration::from_secs(20));
    let queue = sim.queue(Duration::from_secs(17), 0.95);
    assert!(queue < Duration::from_millis(50), "p95 queue {queue:?}");
}

/// Without receive timestamps the ACK's arrival stands in for each packet's, so GoogCC still
/// finds the link on the round-trip delay
#[test]
fn falls_back_to_ack_arrival() {
    let mut sim = Sim::new(LINK, Source::Bulk);
    sim.timestamps = false;
    sim.run(Duration::from_secs(10));
    let estimate = sim.estimate();
    assert!(
        (LINK / 2..=LINK * 11 / 10).contains(&estimate),
        "estimate {estimate} B/s on a {LINK} B/s link"
    );
}

/// A packet acknowledged without a receive time, once the peer reports some, reaches the
/// feedback as received, for the loss-based estimate's loss rate, but not among the packets whose
/// times the delay-based estimate and the RTT read
#[test]
fn counts_packets_acknowledged_without_times() {
    let mut sim = Sim::new(LINK, Source::Bulk);
    let untimed = |gcc: &mut Gcc, now| {
        let packet = gcc.on_sent(Sent {
            now,
            bytes: MTU,
            in_flight: MTU.into(),
        });
        let acked = Acked {
            sent: now,
            bytes: MTU,
            packet,
            received: None,
        };
        TransportPacketsFeedback {
            packet_feedbacks: vec![gcc.acked(&acked, now)],
            ..Default::default()
        }
    };

    // Before any time is reported, the ACK's arrival is the packet's
    let feedback = untimed(&mut sim.gcc, sim.now);
    assert_eq!(feedback.received_with_send_info().count(), 1);

    sim.run(Duration::from_millis(500));
    let feedback = untimed(&mut sim.gcc, sim.now);
    assert!(feedback.packet_feedbacks[0].is_received());
    assert_eq!(feedback.received_with_send_info().count(), 0);
    assert!(feedback.sorted_by_receive_time().is_empty());
}

/// An application-limited sender gets probe clusters to fill: duplicates filling them carry the
/// estimate far past what the media alone reads from the same clusters
#[test]
fn filled_probes_raise_the_estimate() {
    const MEDIA: u64 = 125_000;
    let mut alone = Sim::new(10 * LINK, Source::Media(MEDIA));
    alone.run(Duration::from_secs(10));
    let mut filled = Sim::new(10 * LINK, Source::Media(MEDIA));
    filled.fill_probes = true;
    filled.run(Duration::from_secs(10));
    assert!(
        filled.estimate() >= 4 * alone.estimate(),
        "estimate {} B/s filling the probes, {} B/s without",
        filled.estimate(),
        alone.estimate()
    );
}

/// A probe target is the allocation GoogCC aims its probes at: no more than twice it, so filled
/// clusters raise the estimate to the target rather than to the link's rate, and the duplicates
/// cost no more than the target needs
#[test]
fn probes_aim_at_the_target() {
    const MEDIA: u64 = 125_000;
    /// 4 Mbit/s, a fifth of the link
    const TARGET: u64 = 500_000;
    let run = |target: Option<u64>| {
        let mut sim = Sim::new(10 * LINK, Source::Media(MEDIA));
        sim.fill_probes = true;
        if let Some(target) = target {
            sim.gcc.on_probe_target(target);
        }
        sim.run(Duration::from_secs(10));
        sim.estimate()
    };
    let aimed = run(Some(TARGET));
    let unaimed = run(None);
    assert!(
        (TARGET..=2 * TARGET).contains(&aimed),
        "estimate {aimed} B/s probing toward {TARGET} B/s"
    );
    assert!(
        unaimed > 2 * TARGET,
        "estimate {unaimed} B/s without a target"
    );
}

/// A cluster short of a packet's bytes leaves room for a whole packet, which ends it, rather than
/// holding the sender to the bytes and leaving it unfinished
#[test]
fn probe_inflight_rounds_up_to_a_packet() {
    let now = Instant::ZERO + Duration::from_secs(1000);
    let mut gcc = Gcc::new(Init {
        now,
        mtu: MTU,
        rtt: Duration::from_millis(100),
        max_pacing_rate: None,
    });
    // 3000 bytes, two and a half packets
    gcc.prober
        .create_probe_cluster(&api::transport::ProbeClusterConfig {
            at_time: timestamp(now),
            target_data_rate: DataRate::from_bytes_per_sec(200_000),
            target_duration: TimeDelta::from_millis(15),
            min_probe_delta: TimeDelta::from_millis(2),
            target_probe_count: 1,
            id: 1,
        });
    let mut in_flight = 0;
    for _ in 0..2 {
        in_flight += u64::from(MTU);
        gcc.on_sent(Sent {
            now,
            bytes: MTU,
            in_flight,
        });
    }
    assert_eq!(
        gcc.metrics().probe_inflight,
        Some(in_flight + u64::from(MTU))
    );

    in_flight += u64::from(MTU);
    gcc.on_sent(Sent {
        now,
        bytes: MTU,
        in_flight,
    });
    assert_eq!(gcc.metrics().probe_inflight, None, "the cluster went on");
}

/// A restarted cluster's short id never matches one of GoogCC's, whatever either counter reached,
/// so a packet resolves to the cluster it was sent in
#[test]
fn restart_ids_never_alias_googcc_ids() {
    let base = BitrateProber::RESTART_ID_BASE;
    for googcc in [
        1,
        2,
        16_384,
        16_385,
        32_767,
        32_768,
        65_535,
        65_536,
        1 << 20,
    ] {
        let short = short_id(googcc).unwrap().get();
        assert!(short < 1 << 15, "GoogCC id {googcc} took short id {short}");
    }
    for restart in [0, 1, 16_383, 16_384, 32_767, 32_768, 65_535, (1 << 30) - 1] {
        let short = short_id(base + restart).unwrap().get();
        assert!(short >= 1 << 15, "restart {restart} took short id {short}");
    }
    assert_eq!(short_id(0), None);
    assert_eq!(short_id(-1), None);
}

/// Receive times a peer controls, however far off, regressing, or unreported, never panic or
/// leave the estimate outside GoogCC's bounds
#[test]
fn survives_hostile_receive_times() {
    let mut now = Instant::ZERO + Duration::from_secs(1000);
    let mut gcc = Gcc::new(Init {
        now,
        mtu: MTU,
        rtt: Duration::from_millis(100),
        max_pacing_rate: None,
    });
    let times = [
        Some(Duration::ZERO),
        Some(Duration::MAX),
        Some(Duration::from_secs(u64::MAX / 2)),
        None,
        Some(Duration::from_secs(1)),
        Some(Duration::ZERO),
        Some(Duration::from_nanos(1)),
    ];
    for round in 0..2000 {
        now += Duration::from_millis(1);
        let packet = gcc.on_sent(Sent {
            now,
            bytes: MTU,
            in_flight: MTU.into(),
        });
        let acked = [Acked {
            sent: now,
            bytes: MTU,
            packet,
            received: times[round % times.len()],
        }];
        gcc.on_ack(Ack {
            now,
            packets: &acked,
            in_flight: 0,
            app_limited: false,
            rtt: Duration::from_millis(100),
        });
        let _ = gcc.pace(now, MTU.into());
        let metrics = gcc.metrics();
        assert!(metrics.bandwidth_estimate.unwrap() >= 5_000 / 8);
        assert!(metrics.congestion_window >= MIN_WINDOW);
    }
}
