//! libwebrtc's Google Congestion Control (GoogCC), driven by QUIC receive timestamps.
//!
//! The submodules are a Rust port of WebRTC's `modules/congestion_controller/goog_cc`, with the
//! parts of `api/transport`, `api/units`, `modules/pacing` and `modules/remote_bitrate_estimator`
//! it uses. They began as [goog_cc](https://github.com/kixelated/goog_cc) 0.1.4, a port of WebRTC
//! [da2a3d6c4d](https://webrtc.googlesource.com/src/+/da2a3d6c4d) (2024-12-09), and are refreshed
//! to WebRTC [556f7eec27](https://webrtc.googlesource.com/src/+/556f7eec27) (2026-11-01). They keep
//! WebRTC's BSD license, in `LICENSE` beside them, with its `PATENTS` grant and `AUTHORS`.
//!
//! Each file follows its upstream counterpart, so a refresh diffs WebRTC between the recorded
//! revision and a newer one and applies the changes file by file. What a QUIC sender has no source
//! for is gone, and each file's own docs say what: RTCP receiver reports and REMB, route changes,
//! the network state estimate, separate audio detection, RTP sequence numbers, ECN feedback,
//! padding, and the `AcknowledgedBitrateEstimator` that a disabled-by-default field trial selects.
//! Field trials are typed settings with upstream's defaults (`FieldTrials`).
//!
//! [`Gcc`] adapts it to [`Controller`]: each ACK becomes transport feedback carrying the peer's
//! receive times and each loss a feedback of lost packets, GoogCC's process timer runs from the
//! controller's calls, and a port of WebRTC's `BitrateProber` paces its probe clusters.

/// `approx::assert_relative_eq!`: within `epsilon`, or within a relative `f64::EPSILON`.
#[cfg(test)]
macro_rules! assert_relative_eq {
    ($a:expr, $b:expr $(,)?) => {
        assert_relative_eq!($a, $b, epsilon = f64::EPSILON)
    };
    ($a:expr, $b:expr, epsilon = $e:expr $(,)?) => {{
        let (a, b, e): (f64, f64, f64) = ($a.into(), $b.into(), $e.into());
        assert!(
            (a - b).abs() <= e || (a - b).abs() <= a.abs().max(b.abs()) * f64::EPSILON,
            "{a} is not within {e} of {b}"
        );
    }};
}

mod api;
mod experiments;
mod pacing;
mod remote_bitrate_estimator;

mod alr_detector;
mod congestion_window_pushback_controller;
mod delay_based_bwe;
mod delay_increase_detector_interface;
mod goog_cc_network_control;
mod inter_arrival_delta;
mod link_capacity_estimator;
mod loss_based_bwe_v2;
mod probe_bitrate_estimator;
mod probe_controller;
mod robust_throughput_estimator;
mod send_side_bandwidth_estimation;
mod trendline_estimator;

use alr_detector::*;
use congestion_window_pushback_controller::*;
use delay_based_bwe::*;
use delay_increase_detector_interface::*;
use inter_arrival_delta::*;
use link_capacity_estimator::*;
use loss_based_bwe_v2::*;
use probe_bitrate_estimator::*;
use probe_controller::*;
use robust_throughput_estimator::*;
use send_side_bandwidth_estimation::*;
use trendline_estimator::*;

use goog_cc_network_control::{GoogCcNetworkController, NetworkControllerConfig};

#[cfg(test)]
mod tests;

#[cfg(test)]
mod bench;

use std::{collections::VecDeque, num::NonZeroU16, sync::Arc, time::Duration};

use kio::clock::Instant;

use crate::{
    Ack, Ce, Controller, Factory, Init, Loss, Metrics, Packet, Sent,
    pacer::{self, Pacer, Rate},
};
use api::{
    transport::{
        NetworkAvailability, NetworkControlUpdate, PacedPacketInfo, PacerConfig, PacketResult,
        ProcessInterval, RoundTripTimeUpdate, SentPacket, StreamsConfig, TargetRateConstraints,
        TransportPacketsFeedback,
    },
    units::{DataRate, DataSize, TimeDelta, Timestamp},
};
use pacing::BitrateProber;

/// How often GoogCC expects its process timer, as `GetProcessInterval`
const PROCESS_INTERVAL: Duration = Duration::from_millis(25);
/// How far a mapped peer receive time may sit from the local clock: a peer's clock drifts far
/// less over a connection, and the bound keeps a hostile time from overflowing GoogCC's units.
const RECEIVE_SKEW: TimeDelta = TimeDelta::from_seconds(3600);
/// The smallest window GoogCC's congestion window settles on, `kMinCwnd`
const MIN_WINDOW: u64 = 2 * 1500;
/// How long a probe cluster's settings are kept for its packets' feedback, past the
/// `ProbeBitrateEstimator`'s own one second of history
const CLUSTER_HISTORY: TimeDelta = TimeDelta::from_seconds(2);

/// The rate GoogCC starts at, whose initial probes go out at three and six times it: 1 Mbit/s,
/// rather than WebRTC's 300 kbit/s.
///
/// WebRTC's encoder starts at GoogCC's rate, but a transport's sender writes at its own. From
/// 300 kbit/s the initial probing took several round trips to reach a few Mbit/s while 2 Mbit/s
/// of media queued behind the pacer. Draining that backlog once a probe found the path queued it
/// at the bottleneck, and the delay-based estimate cut the rate to 85% of the throughput
/// acknowledged so far, from a sender still starting up. Held above the estimate, the media kept
/// GoogCC out of the application-limited region, so no probe followed, and the estimate climbed
/// back at 8% a second. Only the 5 Mbit/s ceiling `ProbeController` keeps without a maximum rate
/// hid this: the probe it capped happened to restore the estimate after the cut.
const INITIAL_RATE: DataRate = DataRate::from_kilobits_per_sec(1000);
/// The most GoogCC estimates or probes when the transport sets no pacing limit, the 1 Gbit/s
/// `SendSideBandwidthEstimation` already defaults to. Without one, `ProbeController` caps every
/// probe at 5 Mbit/s, so a probe target above that could never be reached.
const MAX_RATE: DataRate = DataRate::from_kilobits_per_sec(1_000_000);

/// Configuration for the [`Gcc`] congestion controller, which has no settings yet.
#[derive(Debug, Clone, Default)]
pub struct Config {
    _private: (),
}

impl Factory for Config {
    fn build(self: Arc<Self>, init: Init) -> Box<dyn Controller> {
        Box::new(Gcc::new(init))
    }
}

/// A probe cluster's settings, kept for its packets' feedback
#[derive(Debug, Clone, Copy)]
struct Cluster {
    info: PacedPacketInfo,
    /// When its last packet was sent
    sent: Timestamp,
}

/// libwebrtc's Google Congestion Control, fed by receive timestamps.
///
/// The window is GoogCC's congestion window (its target rate times the round trip plus 350ms),
/// the pacing rate its pacer's, and the bandwidth estimate its target rate. Each probe cluster
/// GoogCC requests paces at its rate while it runs and reports what it still needs as
/// [`Metrics::probe_inflight`], so an application-limited sender can fill it.
#[derive(Clone)]
pub struct Gcc {
    cc: GoogCcNetworkController,
    prober: BitrateProber,
    pacer: Pacer,
    mtu: u16,
    /// The pacing rate GoogCC last configured, in bytes per second
    pacing_rate: u64,
    /// The target rate GoogCC last reported, in bytes per second
    target_rate: u64,
    /// The window until GoogCC computes one from feedback
    initial_window: u64,
    in_flight: u64,
    /// Whether an ACK has arrived, which makes the network available to GoogCC and starts its
    /// initial probes: before then a probe would only pace the handshake, which waits a round
    /// trip between flights
    available: bool,
    next_process: Instant,
    /// The local time, in microseconds, a peer receive time of zero maps to, fixed by the first
    /// receive time the peer reports
    receive_basis: Option<i128>,
    /// Recently sent probe clusters, oldest first
    clusters: VecDeque<Cluster>,
    /// A probe target not yet handed to GoogCC, in bytes per second, which waits for a call that
    /// knows the time
    probe_target: Option<u64>,
    /// Reused for each ACK's and loss's feedback
    feedback: TransportPacketsFeedback,
}

impl Gcc {
    fn new(init: Init) -> Self {
        let now = timestamp(init.now);
        let starting_rate = INITIAL_RATE;
        let cc = GoogCcNetworkController::new(NetworkControllerConfig {
            constraints: TargetRateConstraints {
                at_time: now,
                min_data_rate: None,
                max_data_rate: Some(init.max_pacing_rate.map_or(MAX_RATE, |rate| {
                    DataRate::from_bytes_per_sec(rate.min(i64::MAX as u64) as i64)
                })),
                starting_rate: Some(starting_rate),
            },
            stream_based_config: StreamsConfig {
                // Periodic probing while application-limited, so a sender held below its target
                // finds the capacity to resume a stopped rendition
                requests_alr_probing: Some(true),
                ..Default::default()
            },
            default_pacing_time_window: PacerConfig::DEFAULT_TIME_INTERVAL,
            ..Default::default()
        });
        let window_time = TimeDelta::from_micros(init.rtt.as_micros().min(i64::MAX as u128) as i64)
            + TimeDelta::from_millis(experiments::CongestionWindowConfig::default().queue_size_ms);
        let initial_window = ((starting_rate * window_time).bytes().max(0) as u64).max(MIN_WINDOW);
        let mut gcc = Self {
            cc,
            prober: BitrateProber::default(),
            pacer: Pacer::new(
                init.rtt,
                initial_window,
                init.mtu,
                init.max_pacing_rate,
                init.now,
            ),
            mtu: init.mtu,
            pacing_rate: starting_rate.bytes_per_sec() as u64,
            target_rate: starting_rate.bytes_per_sec() as u64,
            initial_window,
            in_flight: 0,
            available: false,
            next_process: init.now,
            receive_basis: None,
            clusters: VecDeque::new(),
            probe_target: None,
            feedback: TransportPacketsFeedback::default(),
        };
        gcc.process(init.now);
        gcc
    }

    /// Hands GoogCC a new probe target, then runs its process timer if it is due.
    fn process(&mut self, now: Instant) {
        if let Some(target) = self.probe_target.take() {
            // The session's target is the rate its streams would send at, which GoogCC probes
            // toward as soon as it exceeds the estimate, capping its probes at twice the target
            let update = self.cc.on_streams_config(StreamsConfig {
                at_time: timestamp(now),
                max_total_allocated_bitrate: Some(DataRate::from_bytes_per_sec(
                    target.min(i64::MAX as u64) as i64,
                )),
                ..Default::default()
            });
            self.apply(update);
        }
        if now < self.next_process {
            return;
        }
        self.next_process = now + PROCESS_INTERVAL;
        let update = self.cc.on_process_interval(ProcessInterval {
            at_time: timestamp(now),
            pacer_queue: None,
        });
        self.apply(update);
    }

    fn apply(&mut self, update: NetworkControlUpdate) {
        if let Some(target) = update.target_rate {
            self.target_rate = target.target_rate.bytes_per_sec().max(0) as u64;
        }
        if let Some(pacer) = update.pacer_config {
            self.pacing_rate = (pacer.data_rate().bytes_per_sec().max(1)) as u64;
        }
        for cluster in &update.probe_cluster_configs {
            self.prober.create_probe_cluster(cluster);
        }
    }

    /// Maps a peer receive time onto the local clock.
    ///
    /// The first one the peer reports lands at the local time its ACK arrived, so the mapped
    /// times carry the forward delay of that packet as a constant offset, which GoogCC's
    /// gradients ignore.
    fn receive_time(&mut self, received: Duration, now: Instant) -> Timestamp {
        let now = timestamp(now).us();
        // In i128, where a peer's time, at most 2^64 seconds, cannot overflow
        let received = received.as_micros() as i128;
        let basis = *self
            .receive_basis
            .get_or_insert_with(|| i128::from(now) - received);
        let skew = i128::from(RECEIVE_SKEW.us());
        let mapped = (received + basis).clamp(i128::from(now) - skew, i128::from(now) + skew);
        Timestamp::from_micros(mapped as i64)
    }

    /// The feedback for `packet`, acknowledged at `now`.
    ///
    /// Until the peer reports a receive time, the ACK's arrival stands in for each packet's, so
    /// GoogCC runs on the round-trip delay. Once it reports one, a packet without one, as when
    /// the ACK carrying its time was lost, still counts as received for the loss-based estimate,
    /// but not toward delay, rather than mix the two clocks.
    fn acked(&mut self, packet: &crate::Acked, now: Instant) -> PacketResult {
        let (receive_time, ambiguous_receive_time) = match packet.received {
            Some(received) => (self.receive_time(received, now), false),
            None => (timestamp(now), self.receive_basis.is_some()),
        };
        PacketResult {
            sent_packet: SentPacket {
                send_time: timestamp(packet.sent),
                size: DataSize::from_bytes(packet.bytes.into()),
                pacing_info: self.cluster(packet.packet.probe_cluster),
                data_in_flight: DataSize::zero(),
            },
            receive_time,
            ambiguous_receive_time,
        }
    }

    /// The pacing info of the probe cluster `id` was sent in.
    fn cluster(&self, id: Option<NonZeroU16>) -> PacedPacketInfo {
        id.and_then(|id| {
            self.clusters
                .iter()
                .rev()
                .find(|cluster| short_id(cluster.info.probe_cluster_id) == Some(id))
        })
        .map_or_else(PacedPacketInfo::default, |cluster| cluster.info)
    }

    fn feed(&mut self, now: Instant, in_flight: u64) {
        if self.feedback.packet_feedbacks.is_empty() {
            return;
        }
        self.feedback.feedback_time = timestamp(now);
        self.feedback.data_in_flight = size(in_flight);
        let update = self.cc.on_transport_packets_feedback(&self.feedback);
        self.feedback.packet_feedbacks.clear();
        self.apply(update);
    }

    /// The window GoogCC allows in flight.
    fn congestion_window(&self) -> u64 {
        let window = self
            .cc
            .congestion_window()
            .map_or(self.initial_window, |window| window.bytes().max(0) as u64);
        window.max(MIN_WINDOW)
    }

    /// The bytes in flight up to which the current probe cluster may be filled.
    ///
    /// In whole packets: a cluster short of a packet's bytes still needs one more, and a sender
    /// held to the bytes alone would leave it unfinished.
    fn probe_inflight(&self) -> Option<u64> {
        let (bytes, packets, _) = self.prober.remaining()?;
        let mtu = u64::from(self.mtu);
        let packets = u64::try_from(packets).unwrap_or(0) * mtu;
        let bytes = u64::try_from(bytes.bytes())
            .unwrap_or(0)
            .next_multiple_of(mtu);
        Some(self.in_flight + bytes.max(packets))
    }
}

impl Controller for Gcc {
    fn on_sent(&mut self, sent: Sent) -> Packet {
        self.process(sent.now);
        let now = timestamp(sent.now);
        self.in_flight = sent.in_flight;
        self.pacer.on_transmit(sent.bytes);
        let bytes = DataSize::from_bytes(sent.bytes.into());

        // A large enough packet starts a pending probe cluster, as WebRTC's pacer starts one
        // when media is queued, so the handshake's small last flight does not start a cluster
        // only for the idle round trip after it to abandon
        self.prober.on_incoming_packet(bytes);
        let info = self.prober.current_cluster(now);
        if info.is_some() {
            self.prober.probe_sent(now, bytes);
        }
        let info = info.unwrap_or_default();
        let probe_cluster = short_id(info.probe_cluster_id);
        if probe_cluster.is_some() {
            match self.clusters.back_mut() {
                Some(last) if last.info.probe_cluster_id == info.probe_cluster_id => {
                    last.sent = now;
                }
                _ => self.clusters.push_back(Cluster { info, sent: now }),
            }
        }
        while self
            .clusters
            .front()
            .is_some_and(|cluster| now - cluster.sent > CLUSTER_HISTORY)
        {
            self.clusters.pop_front();
        }

        let update = self.cc.on_sent_packet(SentPacket {
            send_time: now,
            size: bytes,
            pacing_info: info,
            data_in_flight: size(sent.in_flight),
        });
        self.apply(update);
        Packet {
            probe_cluster,
            ..Default::default()
        }
    }

    fn on_ack(&mut self, ack: Ack<'_>) {
        self.process(ack.now);
        self.in_flight = ack.in_flight;
        if !self.available {
            self.available = true;
            let update = self.cc.on_network_availability(NetworkAvailability {
                at_time: timestamp(ack.now),
                network_available: true,
            });
            self.apply(update);
        }
        if !ack.rtt.is_zero() {
            self.cc.on_round_trip_time_update(RoundTripTimeUpdate {
                round_trip_time: TimeDelta::from_micros(
                    ack.rtt.as_micros().min(i64::MAX as u128) as i64
                ),
            });
        }
        for packet in ack.packets {
            let result = self.acked(packet, ack.now);
            self.feedback.packet_feedbacks.push(result);
        }
        self.feed(ack.now, ack.in_flight);
    }

    fn on_loss(&mut self, loss: Loss<'_>) {
        self.process(loss.now);
        self.in_flight = loss.in_flight;
        for packet in loss.packets {
            let result = PacketResult {
                sent_packet: SentPacket {
                    send_time: timestamp(packet.sent),
                    size: DataSize::from_bytes(packet.bytes.into()),
                    pacing_info: self.cluster(packet.packet.probe_cluster),
                    data_in_flight: DataSize::zero(),
                },
                receive_time: Timestamp::plus_infinity(),
                ambiguous_receive_time: false,
            };
            self.feedback.packet_feedbacks.push(result);
        }
        self.feed(loss.now, loss.in_flight);
    }

    // GoogCC reacts to delay and loss; ECN is SCReAM's signal.
    fn on_ce(&mut self, _ce: Ce) {}

    fn on_mtu_update(&mut self, new_mtu: u16) {
        self.mtu = new_mtu;
    }

    fn on_app_limited(&mut self, in_flight: u64) {
        self.in_flight = in_flight;
        self.prober.on_app_limited();
    }

    fn window(&self) -> u64 {
        self.congestion_window()
    }

    fn pace(&mut self, now: Instant, bytes: u64) -> Option<Instant> {
        self.process(now);
        let at = timestamp(now);
        if self.prober.current_cluster(at).is_some() {
            let next = self.prober.next_probe_time();
            if next <= at {
                return None;
            }
            return Some(now + Duration::from_micros((next - at).us().max(0) as u64));
        }
        let rate = Rate {
            window: self.congestion_window(),
            pacing_rate: Some(self.pacing_rate),
        };
        // A pacing rate makes the RTT irrelevant; the MTU only sizes the bursts.
        self.pacer.delay(Duration::ZERO, bytes, self.mtu, now, rate)
    }

    /// The pacer's own burst budget, or a single packet while a probe cluster runs.
    ///
    /// A transport checks each datagram's pace while the one before it is still being built, so
    /// a batch longer than the budget releases a packet the budget never pays for. Under
    /// 2 Mbit/s the budget is one packet, and batching two would pace at twice the rate.
    fn burst(&self) -> Option<u64> {
        let mtu = u64::from(self.mtu);
        if self.prober.remaining().is_some() {
            return Some(mtu);
        }
        Some(pacer::rate_capacity(self.pacing_rate, self.mtu).max(mtu))
    }

    fn metrics(&self) -> Metrics {
        Metrics {
            congestion_window: self.congestion_window(),
            ssthresh: None,
            pacing_rate: Some(self.pacing_rate),
            bandwidth_estimate: Some(self.target_rate),
            probe_inflight: self.probe_inflight(),
        }
    }

    fn probes(&self) -> bool {
        true
    }

    /// GoogCC reads a probe at the slower of its send and receive rates, so a cluster held below
    /// its rate reads no higher, and its size bounds what the duplicates filling it cost.
    fn paces_probe(&self) -> bool {
        true
    }

    fn on_probe_target(&mut self, bytes_per_second: u64) {
        self.probe_target = Some(bytes_per_second);
    }

    fn clone_box(&self) -> Box<dyn Controller> {
        Box::new(self.clone())
    }
}

fn timestamp(instant: Instant) -> Timestamp {
    let micros = instant.duration_since(Instant::ZERO).as_micros();
    Timestamp::from_micros(micros.min(i64::MAX as u128 / 2) as i64)
}

fn size(bytes: u64) -> DataSize {
    DataSize::from_bytes(bytes.min(i64::MAX as u64 / 2) as i64)
}

/// The probe cluster id a [`Packet`] stores: GoogCC's, which counts up from one, wrapped to
/// sixteen bits without zero.
fn short_id(id: i32) -> Option<NonZeroU16> {
    let id = u32::try_from(id).ok().filter(|&id| id > 0)?;
    NonZeroU16::new(((id - 1) % u32::from(u16::MAX) + 1) as u16)
}
