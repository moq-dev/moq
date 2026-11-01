//! SCReAMv2's delay-based congestion control, ported from draft-ietf-ccwg-rfc8298bis-screamv2-01.
//! <https://www.ietf.org/archive/id/draft-ietf-ccwg-rfc8298bis-screamv2-01.html>
//!
//! A reference window caps the bytes in flight. It grows with every byte acknowledged, and shrinks
//! on loss, on classic ECN-CE, and as the averaged queue delay passes half the target, in
//! proportion to how far past it is ("virtual L4S" marking). The queue delay is each packet's
//! one-way delay above the lowest seen recently, from the peer's receive timestamps
//! (draft-ietf-quic-receive-ts), so ACK-path delay never reads as a queue and the peer's unknown
//! clock basis cancels out.
//!
//! Only the queue-delay target path is ported. The L4S path waits on ECN being measured between
//! publishers and relays, and the competing-flow compensation that raises the target is left out:
//! the controller keeps the queue near empty and accepts a smaller share beside loss-based flows.
//! Checked against Ericsson's BSD-licensed reference, `ScreamV2Tx.cpp` in
//! <https://github.com/EricssonResearch/scream>.

use std::any::Any;
use std::sync::Arc;

use crate::pacer::{Pacer, Rate};
use crate::{Ack, Ce, Controller, ControllerFactory, ControllerMetrics, Init, Loss, Packet, Sent};
use crate::{Duration, Instant};

/// equivalent to MIN_REF_WND: the smallest reference window, in bytes
const MIN_REF_WND: f64 = 3000.0;

/// equivalent to BYTES_IN_FLIGHT_HEAD_ROOM: how far past the most bytes recently in flight the
/// reference window may grow, so an application-limited sender's window tracks what it sends
const BYTES_IN_FLIGHT_HEAD_ROOM: f64 = 1.5;

/// equivalent to BETA_LOSS: the reference window's scale on a loss event
const BETA_LOSS: f64 = 0.7;

/// equivalent to BETA_ECN: the reference window's scale on a classic ECN-CE event
const BETA_ECN: f64 = 0.8;

/// equivalent to POST_CONGESTION_DELAY_RTT: how many round trips after congestion the window
/// grows only additively
const POST_CONGESTION_DELAY_RTTS: f64 = 100.0;

/// equivalent to MUL_INCREASE_FACTOR: the multiplicative part of the window's growth per round
/// trip, as a fraction of the window
const MUL_INCREASE_FACTOR: f64 = 0.02;

/// equivalent to VIRTUAL_RTT: the shortest interval between reactions to congestion, and the
/// round trip below which the window grows more slowly
const VIRTUAL_RTT: Duration = Duration::from_millis(25);

/// equivalent to QDELAY_AVG_G: the gain of the slow-attack, fast-decay queue delay average
const QDELAY_AVG_G: f64 = 1.0 / 4.0;

/// equivalent to QDELAY_MIN_MAX_AVG_G: the gain pulling the queue delay's tracked maximum down and
/// its minimum up, with [`ScreamConfig::reduce_jitter`]
const QDELAY_MIN_MAX_AVG_G: f64 = 1.0 / 16.0;

/// equivalent to QDELAY_DEV_AVG_G: the gain of the queue delay's averaged spread
const QDELAY_DEV_AVG_G: f64 = 1.0 / 32.0;

/// equivalent to QDELAY_DEV_THRESHOLD: the spread at which jitter stops the window's growth
const QDELAY_DEV_THRESHOLD: Duration = Duration::from_millis(10);

/// equivalent to REF_WND_OVERHEAD_MIN: how far bytes in flight may exceed the reference window
/// once the queue delay varies widely
const REF_WND_OVERHEAD_MIN: f64 = 1.5;

/// equivalent to REF_WND_OVERHEAD_MAX: how far bytes in flight may exceed the reference window on
/// a steady path, so a large frame leaves without waiting on the window
const REF_WND_OVERHEAD_MAX: f64 = 3.0;

/// equivalent to RATE_PACE_MIN: the lowest pacing rate, 50 kbit/s, in bytes per second
const RATE_PACE_MIN: f64 = 50_000.0 / 8.0;

/// equivalent to PACKET_PACING_HEADROOM: how far past the target bitrate packets are paced
const PACKET_PACING_HEADROOM: f64 = 1.5;

/// equivalent to PACKET_OVERHEAD: bytes of packetization each packet adds to the media
const PACKET_OVERHEAD: f64 = 20.0;

/// How long one window of the base delay's history spans, as LEDBAT's minute-long windows do
/// over its ten-minute history, shortened as the reference implementation's are.
/// <https://www.rfc-editor.org/rfc/rfc6817#section-3.4.2>
const BASE_INTERVAL: Duration = Duration::from_secs(10);

/// How many [`BASE_INTERVAL`] windows the base delay is the lowest of
///
/// The receiver's clock drifts against the sender's by tens of parts per million, which a base
/// delay held too long reads as a queue that never drains. A hundred seconds holds that to a few
/// milliseconds, against the target's tens.
const BASE_HISTORY: usize = 10;

/// Configuration for the [`Scream`] congestion controller
#[derive(Debug, Clone)]
pub struct ScreamConfig {
    qdelay_target: Duration,
    reduce_jitter: bool,
}

impl ScreamConfig {
    /// The queue delay the controller holds below: it backs off from half of it, harder the
    /// closer the averaged delay comes to it.
    ///
    /// Equivalent to QDELAY_TARGET_LO. Defaults to 60ms.
    pub fn qdelay_target(&mut self, value: Duration) -> &mut Self {
        self.qdelay_target = value;
        self
    }

    /// Whether the window grows more slowly and runs closer to the reference window while the
    /// queue delay varies, as on a path with scheduling jitter.
    ///
    /// Equivalent to REDUCE_JITTER. Defaults to off.
    pub fn reduce_jitter(&mut self, value: bool) -> &mut Self {
        self.reduce_jitter = value;
        self
    }
}

impl Default for ScreamConfig {
    fn default() -> Self {
        Self {
            qdelay_target: Duration::from_millis(60),
            reduce_jitter: false,
        }
    }
}

impl ControllerFactory for ScreamConfig {
    fn build(self: Arc<Self>, init: Init) -> Box<dyn Controller> {
        Box::new(Scream::new(self, init))
    }
}

/// SCReAMv2's delay-based congestion control, which keeps the bottleneck queue near empty
///
/// Needs the peer's receive timestamps: without them it sees no queue and backs off only on loss.
#[derive(Debug, Clone)]
pub struct Scream {
    config: Arc<ScreamConfig>,
    /// equivalent to MSS: the path's datagram size
    mss: f64,
    /// When the controller was built, which every packet's send time is counted from, as the
    /// peer counts its receive times from a basis of its own
    epoch: Instant,
    /// equivalent to s_rtt: the transport's smoothed RTT
    srtt: Duration,

    /// equivalent to ref_wnd, in bytes
    ref_wnd: f64,
    /// equivalent to ref_wnd_i: the reference window when congestion last began, in bytes
    ref_wnd_i: f64,
    /// equivalent to ref_wnd_i_update_allowed
    ref_wnd_i_update_allowed: bool,
    /// equivalent to bytes_newly_acked
    bytes_newly_acked: u64,
    /// equivalent to max_bytes_in_flight: the most in flight this round trip
    max_bytes_in_flight: u64,
    /// equivalent to max_bytes_in_flight_prev: the most in flight the round trip before
    max_bytes_in_flight_prev: u64,
    /// When the current round trip of [`Self::max_bytes_in_flight`] began
    round_start: Instant,
    /// When the window was last updated
    last_update: Instant,

    /// The lowest forward delay recently seen, which the queue delay is measured above
    base: BaseDelay,
    /// equivalent to qdelay: the latest queue delay sample
    qdelay: Duration,
    /// equivalent to qdelay_avg
    qdelay_avg: Duration,
    /// equivalent to last_update_qdelay_avg_time
    last_qdelay_avg_update: Instant,
    /// equivalent to qdelay_max_avg, with [`ScreamConfig::reduce_jitter`]
    qdelay_max_avg: Duration,
    /// equivalent to qdelay_min_avg, with [`ScreamConfig::reduce_jitter`]
    qdelay_min_avg: Duration,
    /// equivalent to qdelay_dev_avg, with [`ScreamConfig::reduce_jitter`]
    qdelay_dev_avg: Duration,
    /// equivalent to ref_wnd_delay_scale: 1 on a steady path, falling to 0 as the queue delay
    /// varies by [`QDELAY_DEV_THRESHOLD`]
    ref_wnd_delay_scale: f64,

    /// Loss of a packet sent since the last reaction, not yet reacted to
    loss_detected: bool,
    /// ECN-CE on a packet sent since the last reaction, not yet reacted to
    ce_detected: bool,
    /// equivalent to last_congestion_detected_time
    last_congestion_detected: Option<Instant>,
    /// equivalent to last_reaction_to_congestion_time
    last_reaction: Option<Instant>,

    /// equivalent to target_bitrate, in bytes per second
    target_rate: f64,
    /// Whether an application-limited sender may resend in-flight data to raise the window
    probing: bool,
    pacer: Pacer,
}

impl Scream {
    fn new(config: Arc<ScreamConfig>, init: Init) -> Self {
        let mut scream = Self {
            mss: f64::from(init.mtu),
            epoch: init.now,
            srtt: init.rtt,
            ref_wnd: MIN_REF_WND,
            ref_wnd_i: 1.0,
            ref_wnd_i_update_allowed: true,
            bytes_newly_acked: 0,
            max_bytes_in_flight: 0,
            max_bytes_in_flight_prev: 0,
            round_start: init.now,
            last_update: init.now,
            base: BaseDelay::new(init.now),
            qdelay: Duration::ZERO,
            qdelay_avg: Duration::ZERO,
            last_qdelay_avg_update: init.now,
            qdelay_max_avg: config.qdelay_target,
            qdelay_min_avg: Duration::ZERO,
            qdelay_dev_avg: Duration::ZERO,
            ref_wnd_delay_scale: 1.0,
            loss_detected: false,
            ce_detected: false,
            last_congestion_detected: None,
            last_reaction: None,
            target_rate: 0.0,
            probing: false,
            pacer: Pacer::new(
                init.rtt,
                MIN_REF_WND as u64,
                init.mtu,
                init.max_pacing_rate,
                init.now,
            ),
            config,
        };
        scream.update_target_rate();
        scream
    }

    /// The bytes per second the media should be encoded at
    fn target_rate(&self) -> u64 {
        self.target_rate as u64
    }

    /// equivalent to the Media Rate Control's target_bitrate, without the media queue terms a
    /// transport cannot see.
    /// <https://www.ietf.org/archive/id/draft-ietf-ccwg-rfc8298bis-screamv2-01.html#section-4.4>
    fn update_target_rate(&mut self) {
        let ratio = (self.mss / self.ref_wnd).min(1.0);
        let mut scale = 1.0 - (ratio - 0.1).clamp(0.0, 0.2);
        scale *= self.mss / (self.mss + PACKET_OVERHEAD);
        scale /= 1.2;
        self.target_rate = scale * self.ref_wnd / self.srtt.as_secs_f64().max(1e-6);
    }

    /// The pacing rate, in bytes per second.
    /// <https://www.ietf.org/archive/id/draft-ietf-ccwg-rfc8298bis-screamv2-01.html#section-4.3.2>
    fn pacing_rate(&self) -> u64 {
        (self.target_rate.max(RATE_PACE_MIN) * PACKET_PACING_HEADROOM) as u64
    }

    /// The forward delay of a packet sent at `sent` and received at `received` on the peer's
    /// clock, in nanoseconds, offset by the peer's unknown basis
    fn forward_delay(&self, sent: Instant, received: Duration) -> i64 {
        let sent = sent.saturating_duration_since(self.epoch);
        nanos(received) - nanos(sent)
    }

    /// equivalent to the qdelay_avg and ref_wnd_delay_scale updates, once per
    /// min(VIRTUAL_RTT, s_rtt).
    /// <https://www.ietf.org/archive/id/draft-ietf-ccwg-rfc8298bis-screamv2-01.html#section-4.2.1.4>
    fn update_qdelay_avg(&mut self, now: Instant) {
        if now.saturating_duration_since(self.last_qdelay_avg_update) < self.interval() {
            return;
        }
        self.last_qdelay_avg_update = now;
        self.qdelay_avg = match self.qdelay < self.qdelay_avg {
            true => self.qdelay,
            false => self
                .qdelay
                .mul_f64(QDELAY_AVG_G)
                .saturating_add(self.qdelay_avg.mul_f64(1.0 - QDELAY_AVG_G)),
        };

        if self.config.reduce_jitter {
            self.qdelay_max_avg = self.qdelay_max_avg.mul_f64(1.0 - QDELAY_MIN_MAX_AVG_G);
            self.qdelay_min_avg = self
                .qdelay_min_avg
                .mul_f64(1.0 - QDELAY_MIN_MAX_AVG_G)
                .saturating_add(self.qdelay_max_avg.mul_f64(QDELAY_MIN_MAX_AVG_G));
            let spread = self.qdelay_max_avg.saturating_sub(self.qdelay_min_avg);
            self.qdelay_dev_avg = self
                .qdelay_dev_avg
                .mul_f64(1.0 - QDELAY_DEV_AVG_G)
                .saturating_add(spread.mul_f64(QDELAY_DEV_AVG_G));
            self.ref_wnd_delay_scale = (1.0
                - self.qdelay_dev_avg.as_secs_f64() / QDELAY_DEV_THRESHOLD.as_secs_f64())
            .clamp(0.0, 1.0);
        }
    }

    /// The shortest interval between reactions and window updates: min(VIRTUAL_RTT, s_rtt)
    fn interval(&self) -> Duration {
        Ord::min(VIRTUAL_RTT, self.srtt)
    }

    /// Updates the reference window if one is due, or at once on loss or ECN-CE: the reduction,
    /// then the increase, then the target rate.
    /// <https://www.ietf.org/archive/id/draft-ietf-ccwg-rfc8298bis-screamv2-01.html#section-4.2.2>
    fn update(&mut self, now: Instant) {
        let congested = self.loss_detected || self.ce_detected;
        if !congested && now.saturating_duration_since(self.last_update) < self.interval() {
            return;
        }
        self.last_update = now;
        self.reduce(now);
        self.increase(now);
        self.update_target_rate();
        self.bytes_newly_acked = 0;
    }

    /// equivalent to scl_t: 1 away from the window congestion last began at, falling to 0.1 near
    /// it
    fn scl(&self) -> f64 {
        let scl = (self.ref_wnd - self.ref_wnd_i) / self.ref_wnd_i * 8.0;
        (scl * scl).clamp(0.1, 1.0)
    }

    /// equivalent to the Reference Window Reduction.
    /// <https://www.ietf.org/archive/id/draft-ietf-ccwg-rfc8298bis-screamv2-01.html#section-4.2.2.1>
    fn reduce(&mut self, now: Instant) {
        if self.loss_detected || self.ce_detected {
            self.last_congestion_detected = Some(now);
        }
        let due = self
            .last_reaction
            .is_none_or(|last| now.saturating_duration_since(last) >= self.interval());
        if !due {
            return;
        }

        let target = self.config.qdelay_target.as_secs_f64();
        let qdelay_avg = self.qdelay_avg.as_secs_f64();
        let scale = if self.loss_detected {
            BETA_LOSS
        } else if self.ce_detected {
            BETA_ECN
        } else if qdelay_avg > target / 2.0 {
            // Averaged, so a delay spike from a link-layer retransmission or a handover is not
            // read as a queue
            let alpha = ((qdelay_avg - target / 2.0) / (target / 2.0)).clamp(0.0, 1.0);
            // Reactions come every min(VIRTUAL_RTT, s_rtt), so several a round trip on a long
            // path: each takes its share
            let backoff =
                alpha / 2.0 / (self.srtt.as_secs_f64() / VIRTUAL_RTT.as_secs_f64()).max(1.0);
            1.0 - backoff
        } else {
            return;
        };
        self.loss_detected = false;
        self.ce_detected = false;
        // A queue past half the target is congestion too, which the window's multiplicative
        // growth waits out, as the reference implementation's virtual CE events do
        self.last_congestion_detected = Some(now);

        if self.ref_wnd_i_update_allowed {
            self.ref_wnd_i = self.ref_wnd;
            self.ref_wnd_i_update_allowed = false;
        }
        self.ref_wnd = (self.ref_wnd * scale).max(MIN_REF_WND);
        self.last_reaction = Some(now);
    }

    /// equivalent to the Reference Window Increase.
    /// <https://www.ietf.org/archive/id/draft-ietf-ccwg-rfc8298bis-screamv2-01.html#section-4.2.2.2>
    fn increase(&mut self, now: Instant) {
        let srtt = self.srtt.as_secs_f64();
        let virtual_rtt = VIRTUAL_RTT.as_secs_f64();
        let post_congestion = self.last_congestion_detected.map_or(1.0, |last| {
            let since = now.saturating_duration_since(last).as_secs_f64();
            (since / (POST_CONGESTION_DELAY_RTTS * srtt.max(virtual_rtt))).clamp(0.0, 1.0)
        });
        let scl = self.scl();

        let mut increment = self.bytes_newly_acked as f64 * (self.mss / self.ref_wnd).min(1.0);
        increment *= (srtt / virtual_rtt).min(1.0);
        increment *= scl.max(0.25);
        increment *= self.ref_wnd_delay_scale.max(0.1);
        let multiplier = 1.0 + MUL_INCREASE_FACTOR * self.ref_wnd / self.mss;
        increment *= 1.0 + (multiplier - 1.0) * post_congestion * scl;

        // Grows only while enough is in flight: an application-limited sender's window stays
        // within reach of what it sends, so it has little to give back on congestion
        let in_flight = Ord::max(self.max_bytes_in_flight, self.max_bytes_in_flight_prev) as f64;
        let max_allowed = self.mss + in_flight * BYTES_IN_FLIGHT_HEAD_ROOM;
        if self.ref_wnd + increment <= max_allowed && increment > 0.0 {
            self.ref_wnd += increment;
            self.ref_wnd_i_update_allowed = true;
        }
    }
}

impl Controller for Scream {
    fn on_sent(&mut self, sent: Sent) -> Packet {
        self.pacer.on_transmit(sent.bytes);
        self.max_bytes_in_flight = Ord::max(self.max_bytes_in_flight, sent.in_flight);
        Packet::default()
    }

    fn on_ack(&mut self, ack: Ack<'_>) {
        let now = ack.now;
        self.srtt = ack.rtt;
        if now.saturating_duration_since(self.round_start) >= self.srtt {
            self.max_bytes_in_flight_prev = self.max_bytes_in_flight;
            self.max_bytes_in_flight = ack.in_flight;
            self.round_start = now;
        }

        // One sample per ACK: the least delayed of its packets, which a link that delivers in
        // bursts, as a cellular scheduler's grants do, delayed least past its queue
        let mut sample = None;
        for acked in ack.packets {
            self.bytes_newly_acked += u64::from(acked.bytes);
            if let Some(received) = acked.received {
                let delay = self.forward_delay(acked.sent, received);
                self.base.update(now, delay);
                sample = Some(Ord::min(sample.unwrap_or(i64::MAX), delay));
            }
        }
        if let Some(delay) = sample {
            let qdelay = delay.saturating_sub(self.base.get());
            self.qdelay = Duration::from_nanos(qdelay.max(0) as u64);
            if self.config.reduce_jitter {
                self.qdelay_max_avg = Ord::min(
                    self.config.qdelay_target,
                    Ord::max(self.qdelay, self.qdelay_max_avg),
                );
                self.qdelay_min_avg = Ord::min(self.qdelay, self.qdelay_min_avg);
            }
        }
        self.update_qdelay_avg(now);
        self.update(now);

        // An application-limited sender may fill in for the media it lacks while no queue is
        // building, so the window grows to what the path carries
        let target = self.config.qdelay_target;
        self.probing = self.qdelay_avg < target / 4
            && self
                .last_reaction
                .is_none_or(|last| now.saturating_duration_since(last) > self.srtt);
    }

    fn on_loss(&mut self, loss: Loss<'_>) {
        if loss.persistent {
            self.ref_wnd = MIN_REF_WND;
            self.last_congestion_detected = Some(loss.now);
            self.last_reaction = Some(loss.now);
            self.loss_detected = false;
            self.update_target_rate();
            return;
        }
        // A packet sent before the last reaction was lost to the congestion it already answered
        let newest = loss.packets.iter().map(|lost| lost.sent).max();
        if newest.is_some_and(|sent| self.last_reaction.is_none_or(|last| sent > last)) {
            self.loss_detected = true;
            self.update(loss.now);
        }
    }

    fn on_ce(&mut self, ce: Ce) {
        if self.last_reaction.is_none_or(|last| ce.sent > last) {
            self.ce_detected = true;
            self.update(ce.now);
        }
    }

    fn on_mtu_update(&mut self, new_mtu: u16) {
        self.mss = f64::from(new_mtu);
    }

    fn window(&self) -> u64 {
        let overhead = REF_WND_OVERHEAD_MIN
            + (REF_WND_OVERHEAD_MAX - REF_WND_OVERHEAD_MIN) * self.ref_wnd_delay_scale;
        (self.ref_wnd * overhead) as u64
    }

    fn pace(&mut self, now: Instant, bytes: u64) -> Option<Instant> {
        let rate = Rate {
            window: self.window(),
            pacing_rate: Some(self.pacing_rate()),
        };
        self.pacer
            .delay(self.srtt, bytes, self.mss as u16, now, rate)
    }

    fn metrics(&self) -> ControllerMetrics {
        ControllerMetrics {
            congestion_window: self.window(),
            ssthresh: None,
            pacing_rate: Some(self.pacing_rate()),
            bandwidth_estimate: Some(self.target_rate()),
            probe_inflight: self.probing.then(|| self.window()),
        }
    }

    fn probes(&self) -> bool {
        true
    }

    fn clone_box(&self) -> Box<dyn Controller> {
        Box::new(self.clone())
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }
}

/// The lowest forward delay over the last [`BASE_HISTORY`] windows of [`BASE_INTERVAL`], as
/// LEDBAT keeps its base delay.
/// <https://www.rfc-editor.org/rfc/rfc6817#section-3.4.2>
#[derive(Debug, Clone)]
struct BaseDelay {
    /// The lowest of each finished window, the newest last, in nanoseconds
    history: [i64; BASE_HISTORY],
    /// The lowest of the window in progress
    current: i64,
    /// When the window in progress began
    since: Instant,
}

impl BaseDelay {
    fn new(now: Instant) -> Self {
        Self {
            history: [i64::MAX; BASE_HISTORY],
            current: i64::MAX,
            since: now,
        }
    }

    fn update(&mut self, now: Instant, delay: i64) {
        // Every window that ended since the last sample, which a quiet spell leaves empty, so a
        // minimum from before it ages out as it would have without the gap
        let ended = now.saturating_duration_since(self.since).as_nanos() / BASE_INTERVAL.as_nanos();
        if ended > 0 {
            match usize::try_from(ended) {
                Ok(ended) if ended <= BASE_HISTORY => {
                    self.history.rotate_left(ended);
                    self.history[BASE_HISTORY - ended..].fill(i64::MAX);
                    self.history[BASE_HISTORY - ended] = self.current;
                }
                _ => self.history.fill(i64::MAX),
            }
            self.current = i64::MAX;
            self.since = now;
        }
        self.current = Ord::min(self.current, delay);
    }

    fn get(&self) -> i64 {
        self.history.iter().copied().fold(self.current, Ord::min)
    }
}

/// A duration in nanoseconds, saturating past the 292 years an `i64` holds
fn nanos(duration: Duration) -> i64 {
    i64::try_from(duration.as_nanos()).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;
    use crate::{Acked, Lost};

    const MTU: u16 = 1200;
    const PROPAGATION: Duration = Duration::from_millis(20);
    const TICK: Duration = Duration::from_millis(1);

    fn scream(now: Instant, config: ScreamConfig) -> Scream {
        Scream::new(
            Arc::new(config),
            Init {
                now,
                mtu: MTU,
                rtt: Duration::from_millis(333),
                max_pacing_rate: None,
            },
        )
    }

    /// A packet on its way
    struct Flight {
        sent: Instant,
        /// When it reaches the peer
        arrival: Instant,
        /// When its ACK reaches the sender
        acked: Instant,
    }

    /// A sender driving a [`Scream`] up through a FIFO bottleneck, with an ACK for every packet
    /// coming straight back, on a mocked clock
    struct Path {
        scream: Scream,
        now: Instant,
        /// The bottleneck's rate in bytes per second
        rate: f64,
        /// When the bottleneck finishes serving what it holds
        busy_until: Instant,
        /// The peer's clock basis, which its receive times count from
        basis: Instant,
        /// How much faster the peer's clock runs, in parts per million
        drift_ppm: f64,
        /// The media's rate in bytes per second, or `None` for an encoder that follows the
        /// controller's estimate
        media: Option<f64>,
        /// Media bytes written and not yet sent
        backlog: f64,
        flights: VecDeque<Flight>,
        in_flight: u64,
        srtt: Option<Duration>,
        /// Each packet's queueing delay at the bottleneck
        queue: Vec<Duration>,
        delivered: u64,
    }

    impl Path {
        fn new(rate: f64, media: Option<f64>, config: ScreamConfig) -> Self {
            let now = Instant::now();
            Self {
                scream: scream(now, config),
                now,
                rate,
                busy_until: now,
                basis: now - Duration::from_secs(1),
                drift_ppm: 0.0,
                media,
                backlog: 0.0,
                flights: VecDeque::new(),
                in_flight: 0,
                srtt: None,
                queue: Vec::new(),
                delivered: 0,
            }
        }

        /// Runs for `duration`, returning the bytes delivered and each packet's queueing delay
        fn run(&mut self, duration: Duration) -> (u64, Vec<Duration>) {
            let (delivered, queued) = (self.delivered, self.queue.len());
            let end = self.now + duration;
            while self.now < end {
                self.now += TICK;
                self.deliver();
                self.send();
            }
            (self.delivered - delivered, self.queue[queued..].to_vec())
        }

        fn send(&mut self) {
            let media = self.media.unwrap_or(self.scream.target_rate);
            self.backlog += media * TICK.as_secs_f64();
            let bytes = u64::from(MTU);
            while self.backlog >= bytes as f64
                && self.in_flight + bytes <= self.scream.window()
                && self.scream.pace(self.now, bytes).is_none()
            {
                self.backlog -= bytes as f64;
                self.in_flight += bytes;
                self.scream.on_sent(Sent {
                    now: self.now,
                    bytes: MTU,
                    in_flight: self.in_flight,
                });
                let start = Ord::max(self.busy_until, self.now);
                self.queue.push(start - self.now);
                self.busy_until = start + Duration::from_secs_f64(bytes as f64 / self.rate);
                let arrival = self.busy_until + PROPAGATION;
                self.flights.push_back(Flight {
                    sent: self.now,
                    arrival,
                    acked: arrival + PROPAGATION,
                });
            }
        }

        fn deliver(&mut self) {
            let mut acked = Vec::new();
            while self.flights.front().is_some_and(|f| f.acked <= self.now) {
                let flight = self.flights.pop_front().unwrap();
                let elapsed = flight.arrival - self.basis;
                acked.push(Acked {
                    sent: flight.sent,
                    bytes: MTU,
                    packet: Packet::default(),
                    received: Some(elapsed.mul_f64(1.0 + self.drift_ppm / 1e6)),
                });
                let rtt = self.now - flight.sent;
                self.srtt = Some(self.srtt.map_or(rtt, |srtt| (srtt * 7 + rtt) / 8));
            }
            if acked.is_empty() {
                return;
            }
            let bytes = acked.len() as u64 * u64::from(MTU);
            self.in_flight -= bytes;
            self.delivered += bytes;
            self.scream.on_ack(Ack {
                now: self.now,
                packets: &acked,
                in_flight: self.in_flight,
                app_limited: false,
                rtt: self.srtt.unwrap(),
            });
        }
    }

    fn percentile(delays: &mut [Duration], p: f64) -> Duration {
        delays.sort();
        delays[((delays.len() as f64 * p) as usize).min(delays.len() - 1)]
    }

    /// Media that follows the estimate fills a 2 Mbit/s path while its queue stays under the target
    #[test]
    fn fills_the_path_with_little_queue() {
        let mut path = Path::new(250_000.0, None, ScreamConfig::default());
        path.run(Duration::from_secs(10));
        let (delivered, mut queue) = path.run(Duration::from_secs(10));
        let throughput = delivered as f64 / 10.0;
        let p95 = percentile(&mut queue, 0.95);
        assert!(
            throughput > 250_000.0 * 0.8,
            "carried {throughput:.0} B/s of 250000"
        );
        assert!(p95 < Duration::from_millis(60), "queue p95 {p95:?}");
    }

    /// When the path halves, the target rate follows within a second and the queue settles back
    /// under the target
    #[test]
    fn follows_a_capacity_drop() {
        let mut path = Path::new(250_000.0, None, ScreamConfig::default());
        path.run(Duration::from_secs(10));
        path.rate = 125_000.0;
        let mut fell = None;
        for ms in 1..=5_000 {
            path.run(TICK);
            if fell.is_none() && path.scream.target_rate() < 125_000 {
                fell = Some(ms);
            }
        }
        let fell = fell.expect("the target rate never fell to the path's");
        assert!(
            fell < 1_000,
            "the target rate fell to the path's after {fell}ms"
        );
        let (_, mut queue) = path.run(Duration::from_secs(5));
        let p95 = percentile(&mut queue, 0.95);
        assert!(
            p95 < Duration::from_millis(60),
            "queue p95 {p95:?} after the drop"
        );
    }

    /// The peer's clock basis is unknown and cancels out: any basis gives the same run
    #[test]
    fn ignores_the_peer_basis() {
        let mut windows = Vec::new();
        for basis in [Duration::ZERO, Duration::from_secs(3600)] {
            let mut path = Path::new(250_000.0, None, ScreamConfig::default());
            path.basis = path.now - basis;
            path.run(Duration::from_secs(10));
            windows.push(path.scream.window());
        }
        assert_eq!(windows[0], windows[1]);
    }

    /// A peer clock running 100ppm fast reads as a queue growing 0.1ms a second, which the base
    /// delay's bounded history keeps from building into a backoff
    #[test]
    fn tolerates_clock_drift() {
        let mut path = Path::new(1_000_000.0, Some(125_000.0), ScreamConfig::default());
        path.drift_ppm = 100.0;
        path.run(Duration::from_secs(10));
        let reacted = path.scream.last_reaction;
        for _ in 0..300 {
            path.run(Duration::from_secs(1));
            assert!(
                path.scream.qdelay < Duration::from_millis(15),
                "queue delay {:?} on an empty queue",
                path.scream.qdelay
            );
        }
        assert_eq!(
            path.scream.last_reaction, reacted,
            "backed off an empty queue"
        );
    }

    /// An application-limited sender's window stops growing once it covers what it sends, short
    /// of the path, and it may probe while no queue builds
    #[test]
    fn app_limited_window_tracks_the_media() {
        let mut path = Path::new(1_000_000.0, Some(100_000.0), ScreamConfig::default());
        path.run(Duration::from_secs(10));
        let window = path.scream.ref_wnd;
        path.run(Duration::from_secs(10));
        let scream = &path.scream;
        assert_eq!(
            scream.ref_wnd, window,
            "the window grew past what the media needs"
        );
        let estimate = scream.metrics().bandwidth_estimate.unwrap();
        assert!(
            (100_000..500_000).contains(&estimate),
            "estimate {estimate} for 100000 B/s of media on a 1000000 B/s path"
        );
        assert!(
            scream.metrics().probe_inflight.is_some(),
            "not probing an idle path"
        );
    }

    /// A quiet spell ages the base delay out as the time passing would have, so a minimum from
    /// before it does not outlive the history; the minimum of windows still inside it holds
    #[test]
    fn base_delay_ages_across_a_quiet_spell() {
        let now = Instant::now();
        let mut base = BaseDelay::new(now);
        base.update(now, 1_000);
        base.update(now + BASE_INTERVAL * 3, 5_000);
        assert_eq!(base.get(), 1_000, "a minimum inside the history expired");
        base.update(now + BASE_INTERVAL * 3 + Duration::from_secs(600), 9_000);
        assert_eq!(
            base.get(),
            9_000,
            "a minimum from before the quiet spell outlived it"
        );
    }

    /// Probing stops once a queue builds, and the controller backs off
    #[test]
    fn stops_probing_on_a_queue() {
        let mut path = Path::new(250_000.0, None, ScreamConfig::default());
        path.run(Duration::from_secs(10));
        path.rate = 62_500.0;
        path.run(Duration::from_millis(500));
        assert!(path.scream.metrics().probe_inflight.is_none());
        let reacted = path.scream.last_reaction.expect("never backed off");
        assert!(path.now - reacted < Duration::from_millis(500));
    }

    fn lose(scream: &mut Scream, now: Instant, sent: Instant) {
        let packets = [Lost {
            sent,
            bytes: MTU,
            packet: Packet::default(),
            new_burst: true,
        }];
        scream.on_loss(Loss {
            now,
            packets: &packets,
            in_flight: 0,
            persistent: false,
        });
    }

    /// Loss backs off once, and a loss of a packet sent before that reaction does not again
    #[test]
    fn loss_backs_off_once() {
        let ms = |ms| Duration::from_millis(ms);
        let now = Instant::now();
        let mut scream = scream(now, ScreamConfig::default());
        scream.ref_wnd = 100_000.0;
        lose(&mut scream, now + ms(50), now + ms(1));
        assert_eq!(scream.ref_wnd, 100_000.0 * BETA_LOSS);

        lose(&mut scream, now + ms(200), now + ms(40));
        assert_eq!(
            scream.ref_wnd,
            100_000.0 * BETA_LOSS,
            "reacted to a loss it answered"
        );

        lose(&mut scream, now + ms(300), now + ms(100));
        assert_eq!(scream.ref_wnd, 100_000.0 * BETA_LOSS * BETA_LOSS);
    }
}
