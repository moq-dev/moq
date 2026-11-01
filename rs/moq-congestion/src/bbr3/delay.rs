//! Bottleneck queue growth, read off the forward delay that receive timestamps expose.
//!
//! The peer reports when each packet arrived, on a clock whose basis it never shares, so a
//! packet's forward delay is known only up to a constant: its trend is what carries the queue.
//! This follows libwebrtc's GoogCC delay detector (`InterArrivalDelta`, `TrendlineEstimator`,
//! and its adaptive threshold, under `modules/congestion_controller/goog_cc`), with its
//! constants:
//!
//! - Packets are grouped into bursts: those sent within [`GROUP_LENGTH`] of a group's first, and
//!   those that arrive together, as a cellular scheduler's grant releases them. Per-packet delay
//!   sees each grant's bunching as noise, which a group's last arrival does not.
//! - Each group's delay variation against the previous one (arrival spacing less send spacing)
//!   accumulates into the forward delay, up to the unknown constant. An exponential filter
//!   smooths it, and a least-squares line over the last [`WINDOW`] groups gives its trend.
//! - The trend, scaled by how many groups it has seen, is held to a threshold that adapts to
//!   the path's own noise. A trend past it for longer than [`OVERUSE_TIME`], still steepening,
//!   is queue growth, which stays reported until the trend falls back under the threshold.

use std::collections::VecDeque;

use crate::{Acked, Duration, Instant};

/// Packets sent within this of a group's first belong to it.
const GROUP_LENGTH: Duration = Duration::from_millis(5);
/// A packet arriving within this of the previous one, sooner than it was sent after it, was
/// released together with it.
const BURST_DELTA: Duration = Duration::from_millis(5);
/// The longest a burst of arrivals counts as one group.
const MAX_BURST: Duration = Duration::from_millis(100);
/// Arrival spacing this much past the local spacing of the ACKs that reported it means the
/// peer's clock jumped.
const ARRIVAL_OFFSET_THRESHOLD: Duration = Duration::from_secs(3);
/// Groups in a row that arrived before the previous one, after which the detector starts over.
const REORDERED_RESET: u32 = 3;
/// Groups the trend line is fit over.
const WINDOW: usize = 20;
/// The exponential filter's weight on the previous smoothed delay.
const SMOOTHING: f64 = 0.9;
/// Scales the trend before it meets the threshold.
const THRESHOLD_GAIN: f64 = 4.0;
/// The most groups the trend's scale counts.
const MIN_DELTAS: u32 = 60;
/// How long, in milliseconds, the trend must stay over the threshold to count as queue growth.
const OVERUSE_TIME: f64 = 10.0;
/// A trend this far past the threshold is a spike, such as a capacity drop, that the threshold
/// does not adapt to, in milliseconds.
const MAX_ADAPT_OFFSET: f64 = 15.0;
/// How fast the threshold rises toward a trend above it, per millisecond.
const K_UP: f64 = 0.0087;
/// How fast the threshold falls toward a trend below it, per millisecond.
const K_DOWN: f64 = 0.039;
const THRESHOLD_INITIAL: f64 = 12.5;
const THRESHOLD_MIN: f64 = 6.0;
const THRESHOLD_MAX: f64 = 600.0;
/// The most time, in milliseconds, one threshold update accounts for.
const THRESHOLD_MAX_STEP: f64 = 100.0;

/// What the forward delay's trend says of the bottleneck queue
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub(super) enum Usage {
    /// Neither growing nor shrinking past the path's noise.
    #[default]
    Normal,
    /// Growing: the sender is past the bottleneck's rate.
    Overusing,
    /// Shrinking.
    Underusing,
}

/// Packets sent, or released, together
#[derive(Debug, Clone, Copy)]
struct Group {
    first_send: Instant,
    /// The latest send time in the group.
    send: Instant,
    first_arrival: Duration,
    /// The last arrival.
    complete: Duration,
    /// When the ACK that reported the last arrival came.
    acked: Instant,
}

/// Reads queue growth from the forward delay's trend
#[derive(Debug, Clone)]
pub(super) struct Detector {
    current: Option<Group>,
    prev: Option<Group>,
    /// Groups in a row that arrived before the one before them.
    reordered: u32,
    /// Group deltas seen, up to a cap.
    deltas: u32,
    /// The first group's arrival, in milliseconds.
    first_arrival: Option<f64>,
    /// The forward delay, in milliseconds, up to the peer's unknown basis.
    accumulated: f64,
    smoothed: f64,
    /// The last [`WINDOW`] groups' arrival, since the first, and smoothed delay, in milliseconds.
    history: VecDeque<(f64, f64)>,
    trend: f64,
    /// How long the trend has been over the threshold, in milliseconds.
    overusing: Option<f64>,
    overuse_count: u32,
    threshold: f64,
    /// The arrival, in milliseconds, of the last threshold update.
    threshold_updated: Option<f64>,
    usage: Usage,
    /// Whether the trend has asserted growth since [`Self::take_signal`] last ran.
    signaled: bool,
    /// One ACK's arrivals and send times, sorted by arrival, kept to reuse its allocation.
    batch: Vec<(Duration, Instant)>,
}

impl Default for Detector {
    fn default() -> Self {
        Self {
            current: None,
            prev: None,
            reordered: 0,
            deltas: 0,
            first_arrival: None,
            accumulated: 0.0,
            smoothed: 0.0,
            history: VecDeque::with_capacity(WINDOW + 1),
            trend: 0.0,
            overusing: None,
            overuse_count: 0,
            threshold: THRESHOLD_INITIAL,
            threshold_updated: None,
            usage: Usage::Normal,
            signaled: false,
            batch: Vec::new(),
        }
    }
}

impl Detector {
    /// What the trend says of the queue as of the last packet.
    #[cfg(test)]
    pub(super) fn usage(&self) -> Usage {
        self.usage
    }

    /// Whether the trend asserted growth since the last call: it crossed the threshold, or
    /// stayed past it for another [`OVERUSE_TIME`] still steepening.
    ///
    /// [`Self::usage`] stays [`Usage::Overusing`] until the trend falls back under the threshold,
    /// which the window's lag puts well after the queue stops growing. A response read from this
    /// instead acts only on fresh growth.
    pub(super) fn take_signal(&mut self) -> bool {
        std::mem::take(&mut self.signaled)
    }

    /// The packets an ACK arriving at `acked` newly acknowledged, skipping any without a receive
    /// timestamp.
    ///
    /// They are fed in the order they arrived, as `DelayBasedBwe` sorts each feedback by
    /// receive time. Fed in send order, a reordered packet would show a negative arrival delta,
    /// which is discarded, while the rebound after it counts as delay, so reordering would
    /// accumulate into false queue growth. In arrival order, a packet sent before the group in
    /// progress is skipped instead.
    pub(super) fn on_ack(&mut self, packets: &[Acked], acked: Instant) {
        // Taken, since a reset in `on_packet` replaces the whole detector.
        let mut batch = std::mem::take(&mut self.batch);
        batch.extend(packets.iter().filter_map(|p| Some((p.received?, p.sent))));
        // Stable, so arrivals a grant released together stay in send order.
        batch.sort_by_key(|&(received, _)| received);
        for &(received, sent) in &batch {
            self.on_packet(sent, received, acked);
        }
        batch.clear();
        self.batch = batch;
    }

    /// A packet sent at `sent` arrived at `received` on the peer's clock, as an ACK arriving at
    /// `acked` reported. Packets are fed in the order they arrived.
    ///
    /// equivalent to `InterArrivalDelta::ComputeDeltas`, then `TrendlineEstimator::Update`
    fn on_packet(&mut self, sent: Instant, received: Duration, acked: Instant) {
        let Some(mut current) = self.current else {
            self.current = Some(Group {
                first_send: sent,
                send: sent,
                first_arrival: received,
                complete: received,
                acked,
            });
            return;
        };
        if sent < current.first_send {
            // Sent before the group in progress: a late ACK for a reordered packet.
            return;
        }
        if self.new_group(&current, sent, received) {
            if let Some(prev) = self.prev
                && let Some((send_delta, arrival_delta)) = self.deltas(&prev, &current)
            {
                self.update_trend(send_delta, arrival_delta, millis(current.complete));
            }
            if self.current.is_none() {
                // The deltas reset the detector.
                return self.on_packet(sent, received, acked);
            }
            self.prev = Some(current);
            current = Group {
                first_send: sent,
                send: sent,
                first_arrival: received,
                complete: received,
                acked,
            };
        } else {
            current.send = current.send.max(sent);
        }
        current.complete = received;
        current.acked = acked;
        self.current = Some(current);
    }

    /// equivalent to `InterArrivalDelta::NewTimestampGroup`
    fn new_group(&self, current: &Group, sent: Instant, received: Duration) -> bool {
        if Self::burst(current, sent, received) {
            return false;
        }
        sent.saturating_duration_since(current.first_send) > GROUP_LENGTH
    }

    /// equivalent to `InterArrivalDelta::BelongsToBurst`: arrived on the heels of the group,
    /// sooner than it was sent after it, as a batch the path released together.
    fn burst(current: &Group, sent: Instant, received: Duration) -> bool {
        let send_delta = sent.saturating_duration_since(current.send);
        if send_delta.is_zero() {
            return true;
        }
        let Some(arrival_delta) = received.checked_sub(current.complete) else {
            return false;
        };
        arrival_delta < send_delta
            && arrival_delta <= BURST_DELTA
            && received.saturating_sub(current.first_arrival) < MAX_BURST
    }

    /// The send and arrival spacing of `current` after `prev`, in milliseconds, or `None` if the
    /// pair says nothing: reordered, or across a jump in the peer's clock, which resets.
    fn deltas(&mut self, prev: &Group, current: &Group) -> Option<(f64, f64)> {
        let send_delta = signed_millis(current.send, prev.send);
        let arrival_delta = millis(current.complete) - millis(prev.complete);
        let acked_delta = signed_millis(current.acked, prev.acked);
        if arrival_delta - acked_delta >= millis(ARRIVAL_OFFSET_THRESHOLD) {
            *self = Self::default();
            return None;
        }
        if arrival_delta < 0.0 {
            self.reordered += 1;
            if self.reordered >= REORDERED_RESET {
                *self = Self::default();
            }
            return None;
        }
        self.reordered = 0;
        Some((send_delta, arrival_delta))
    }

    /// equivalent to `TrendlineEstimator::UpdateTrendline`, at `arrival` on the peer's clock
    fn update_trend(&mut self, send_delta: f64, arrival_delta: f64, arrival: f64) {
        let delta = arrival_delta - send_delta;
        self.deltas = (self.deltas + 1).min(1000);
        let first = *self.first_arrival.get_or_insert(arrival);
        self.accumulated += delta;
        self.smoothed = SMOOTHING * self.smoothed + (1.0 - SMOOTHING) * self.accumulated;
        self.history.push_back((arrival - first, self.smoothed));
        if self.history.len() > WINDOW {
            self.history.pop_front();
        }
        let mut trend = self.trend;
        if self.history.len() == WINDOW {
            trend = slope(&self.history).unwrap_or(trend);
        }
        self.detect(trend, send_delta, arrival);
    }

    /// equivalent to `TrendlineEstimator::Detect`
    fn detect(&mut self, trend: f64, send_delta: f64, arrival: f64) {
        if self.deltas < 2 {
            self.usage = Usage::Normal;
            return;
        }
        let modified = f64::from(self.deltas.min(MIN_DELTAS)) * trend * THRESHOLD_GAIN;
        if modified > self.threshold {
            // Assume it has been over for half the time since the previous group.
            let over = self
                .overusing
                .map_or(send_delta / 2.0, |over| over + send_delta);
            self.overusing = Some(over);
            self.overuse_count += 1;
            if over > OVERUSE_TIME && self.overuse_count > 1 && trend >= self.trend {
                self.overusing = Some(0.0);
                self.overuse_count = 0;
                self.usage = Usage::Overusing;
                self.signaled = true;
            }
        } else if modified < -self.threshold {
            self.overusing = None;
            self.overuse_count = 0;
            self.usage = Usage::Underusing;
        } else {
            self.overusing = None;
            self.overuse_count = 0;
            self.usage = Usage::Normal;
        }
        self.trend = trend;
        self.update_threshold(modified, arrival);
    }

    /// equivalent to `TrendlineEstimator::UpdateThreshold`
    fn update_threshold(&mut self, modified: f64, arrival: f64) {
        let last = *self.threshold_updated.get_or_insert(arrival);
        self.threshold_updated = Some(arrival);
        if modified.abs() > self.threshold + MAX_ADAPT_OFFSET {
            return;
        }
        let k = if modified.abs() < self.threshold {
            K_DOWN
        } else {
            K_UP
        };
        let elapsed = (arrival - last).clamp(0.0, THRESHOLD_MAX_STEP);
        self.threshold += k * (modified.abs() - self.threshold) * elapsed;
        self.threshold = self.threshold.clamp(THRESHOLD_MIN, THRESHOLD_MAX);
    }
}

/// The least-squares slope of `points`, or `None` if they share one x
fn slope(points: &VecDeque<(f64, f64)>) -> Option<f64> {
    let n = points.len() as f64;
    let x = points.iter().map(|p| p.0).sum::<f64>() / n;
    let y = points.iter().map(|p| p.1).sum::<f64>() / n;
    let (mut num, mut den) = (0.0, 0.0);
    for (px, py) in points {
        num += (px - x) * (py - y);
        den += (px - x).powi(2);
    }
    (den != 0.0).then(|| num / den)
}

fn millis(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

/// `a - b` in milliseconds, negative if `a` is earlier
fn signed_millis(a: Instant, b: Instant) -> f64 {
    millis(a.saturating_duration_since(b)) - millis(b.saturating_duration_since(a))
}

#[cfg(test)]
mod test {
    use super::*;

    /// Feeds `count` packets sent every `spacing`, each arriving `delay(i)` after it was sent,
    /// acknowledged on arrival, and returns the usage after each.
    fn feed(
        detector: &mut Detector,
        base: Instant,
        count: u64,
        spacing: Duration,
        delay: impl Fn(u64) -> Duration,
    ) -> Vec<Usage> {
        (0..count)
            .map(|i| {
                let sent = base + spacing * i as u32;
                let received = sent - base + delay(i);
                detector.on_packet(sent, received, sent + delay(i));
                detector.usage()
            })
            .collect()
    }

    #[test]
    fn steady_path_is_normal() {
        let mut detector = Detector::default();
        let usage = feed(
            &mut detector,
            Instant::now(),
            2000,
            Duration::from_millis(2),
            |_| Duration::from_millis(20),
        );
        assert!(usage.iter().all(|&u| u == Usage::Normal));
    }

    /// A queue growing by a millisecond every 10ms, as a sender 10% over the bottleneck builds
    #[test]
    fn growing_queue_overuses() {
        let mut detector = Detector::default();
        let base = Instant::now();
        let usage = feed(&mut detector, base, 1000, Duration::from_millis(2), |i| {
            Duration::from_millis(20) + Duration::from_micros(i.saturating_sub(500) * 200)
        });
        assert!(usage[..500].iter().all(|&u| u == Usage::Normal));
        let first = usage.iter().position(|&u| u == Usage::Overusing);
        // Within 100ms of the queue starting to grow, 10ms of it
        assert!(first.is_some_and(|i| i < 550), "first overuse at {first:?}");
    }

    /// A draining queue never reads as growth
    #[test]
    fn draining_queue_is_not_growth() {
        let mut detector = Detector::default();
        let usage = feed(
            &mut detector,
            Instant::now(),
            1000,
            Duration::from_millis(2),
            |i| Duration::from_millis(220) - Duration::from_micros(i * 200),
        );
        assert!(!usage.contains(&Usage::Overusing));
    }

    /// Arrivals bunched into 20ms grants, with no queue behind them, never read as growth
    #[test]
    fn grants_are_not_growth() {
        let mut detector = Detector::default();
        let grant = Duration::from_millis(20);
        let usage = feed(
            &mut detector,
            Instant::now(),
            3000,
            Duration::from_millis(2),
            |i| {
                // Released at the next grant boundary after a 20ms path
                let sent = Duration::from_millis(2 * i);
                let arrival = sent + Duration::from_millis(20);
                let released = arrival.as_nanos().div_ceil(grant.as_nanos()) * grant.as_nanos();
                Duration::from_nanos(released as u64) - sent
            },
        );
        assert!(!usage.contains(&Usage::Overusing));
    }

    /// A jump in the peer's clock starts the detector over rather than reading as a queue
    #[test]
    fn clock_jump_resets() {
        let mut detector = Detector::default();
        let base = Instant::now();
        let spacing = Duration::from_millis(2);
        let delay = Duration::from_millis(20);
        let mut usage = Vec::new();
        for i in 0..1000u32 {
            let sent = base + spacing * i;
            // The peer's clock jumps 10s ahead halfway, which its ACKs' arrivals do not
            let jump = if i < 500 {
                Duration::ZERO
            } else {
                Duration::from_secs(10)
            };
            detector.on_packet(sent, sent - base + delay + jump, sent + delay);
            usage.push(detector.usage());
        }
        assert!(!usage.contains(&Usage::Overusing));
    }

    /// Packets reordered within an ACK, with no queue behind them, never read as growth. Fed in
    /// send order, each reordered group's negative delta was discarded while the rebound after
    /// it counted, adding 30ms of delay every two packets.
    #[test]
    fn reordered_within_an_ack_is_not_growth() {
        let mut detector = Detector::default();
        let base = Instant::now();
        let spacing = Duration::from_millis(10);
        let interval = Duration::from_millis(40);
        // Every other packet is 30ms slower than its neighbors
        let packets: Vec<Acked> = (0..1000u32)
            .map(|i| {
                let sent = base + spacing * i;
                let delay = Duration::from_millis(if i % 2 == 0 { 20 } else { 50 });
                Acked {
                    sent,
                    bytes: 1200,
                    packet: Default::default(),
                    received: Some(sent - base + delay),
                }
            })
            .collect();
        let mut usage = Vec::new();
        let mut signaled = false;
        // Each ACK reports, in send order, whatever arrived in the 40ms before it
        for ack in 1..250u32 {
            let (from, to) = (interval * (ack - 1), interval * ack);
            let acked: Vec<Acked> = packets
                .iter()
                .filter(|p| p.received.is_some_and(|r| r >= from && r < to))
                .copied()
                .collect();
            detector.on_ack(&acked, base + to + Duration::from_millis(20));
            usage.push(detector.usage());
            signaled |= detector.take_signal();
        }
        assert!(!usage.contains(&Usage::Overusing));
        assert!(!signaled);
        assert!(
            detector.accumulated.abs() < 50.0,
            "accumulated {}ms",
            detector.accumulated
        );
    }

    /// Packets sent before the group in progress, from a late ACK, are skipped
    #[test]
    fn late_packets_are_skipped() {
        let mut detector = Detector::default();
        let base = Instant::now();
        feed(&mut detector, base, 100, Duration::from_millis(2), |_| {
            Duration::from_millis(20)
        });
        let before = detector.clone();
        detector.on_packet(
            base,
            Duration::from_millis(500),
            base + Duration::from_secs(1),
        );
        assert_eq!(detector.deltas, before.deltas);
        assert_eq!(detector.usage(), Usage::Normal);
    }
}
