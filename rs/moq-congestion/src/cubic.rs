use std::cmp;
use std::sync::Arc;

use super::{Ack, BASE_DATAGRAM_SIZE, Ce, Controller, Factory, Init, Loss, Packet, Sent};
use crate::pacer::{Pacer, Rate};
use crate::{Duration, Instant};

/// CUBIC Constants.
///
/// These are recommended value in RFC8312.
const BETA_CUBIC: f64 = 0.7;

const C: f64 = 0.4;

/// CUBIC State Variables.
///
/// We need to keep those variables across the connection.
/// k, w_max are described in the RFC.
#[derive(Debug, Default, Clone)]
pub(super) struct State {
    /// Time period that the cubic function takes to increase the window size to W_max.
    k: f64,

    /// Congestion window size when the last congestion event occurred.
    w_max: f64,

    /// Congestion window increment stored during congestion avoidance.
    cwnd_inc: u64,

    /// Maximum number of bytes in flight that may be sent.
    window: u64,

    /// Slow start threshold in bytes.
    ///
    /// When the congestion window is below ssthresh, the mode is slow start
    /// and the window grows by the number of bytes acknowledged.
    ssthresh: u64,

    /// The time when QUIC first detects a loss, causing it to enter recovery. When a packet sent
    /// after this time is acknowledged, QUIC exits recovery.
    recovery_start_time: Option<Instant>,
}

/// CUBIC Functions.
///
/// Note that these calculations are based on a count of cwnd as bytes,
/// not packets.
/// Unit of t (duration) and RTT are based on seconds (f64).
impl State {
    // K = cbrt(w_max * (1 - beta_cubic) / C) (Eq. 2)
    fn cubic_k(&self, max_datagram_size: u64) -> f64 {
        let w_max = self.w_max / max_datagram_size as f64;
        (w_max * (1.0 - BETA_CUBIC) / C).cbrt()
    }

    // W_cubic(t) = C * (t - K)^3 + w_max (Eq. 1)
    fn w_cubic(&self, t: Duration, max_datagram_size: u64) -> f64 {
        let w_max = self.w_max / max_datagram_size as f64;

        (C * (t.as_secs_f64() - self.k).powi(3) + w_max) * max_datagram_size as f64
    }

    // W_est(t) = w_max * beta_cubic + 3 * (1 - beta_cubic) / (1 + beta_cubic) *
    // (t / RTT) (Eq. 4)
    fn w_est(&self, t: Duration, rtt: Duration, max_datagram_size: u64) -> f64 {
        let w_max = self.w_max / max_datagram_size as f64;
        (w_max * BETA_CUBIC
            + 3.0 * (1.0 - BETA_CUBIC) / (1.0 + BETA_CUBIC) * t.as_secs_f64() / rtt.as_secs_f64())
            * max_datagram_size as f64
    }
}

/// The RFC8312 congestion controller, as widely used for TCP
#[derive(Debug, Clone)]
pub struct Cubic {
    current_mtu: u64,
    state: State,
    /// Copy of the controller state to restore when a spurious congestion event is detected.
    pre_congestion_state: Option<State>,
    /// The transport's smoothed RTT as of the last ACK, which also sets the pacing rate.
    rtt: Duration,
    pacer: Pacer,
}

impl Cubic {
    fn new(config: Arc<Config>, init: Init) -> Self {
        Self {
            state: State {
                window: config.initial_window,
                ssthresh: u64::MAX,
                ..Default::default()
            },
            current_mtu: init.mtu as u64,
            pre_congestion_state: None,
            rtt: init.rtt,
            pacer: Pacer::new(
                init.rtt,
                config.initial_window,
                init.mtu,
                init.max_pacing_rate,
                init.now,
            ),
        }
    }

    fn minimum_window(&self) -> u64 {
        2 * self.current_mtu
    }

    /// Grows the window for one acknowledged packet.
    fn on_acked(&mut self, now: Instant, sent: Instant, bytes: u64, app_limited: bool) {
        let rtt = self.rtt;
        if app_limited
            || self
                .state
                .recovery_start_time
                .map(|recovery_start_time| sent <= recovery_start_time)
                .unwrap_or(false)
        {
            return;
        }

        if self.state.window < self.state.ssthresh {
            // Slow start
            self.state.window += bytes;
        } else {
            // Congestion avoidance.
            let ca_start_time;

            match self.state.recovery_start_time {
                Some(t) => ca_start_time = t,
                None => {
                    // When we come here without congestion_event() triggered,
                    // initialize congestion_recovery_start_time, w_max and k.
                    ca_start_time = now;
                    self.state.recovery_start_time = Some(now);

                    self.state.w_max = self.state.window as f64;
                    self.state.k = 0.0;
                }
            }

            let t = now - ca_start_time;

            // w_cubic(t + rtt)
            let w_cubic = self.state.w_cubic(t + rtt, self.current_mtu);

            // w_est(t)
            let w_est = self.state.w_est(t, rtt, self.current_mtu);

            let mut cubic_cwnd = self.state.window;

            if w_cubic < w_est {
                // TCP friendly region.
                cubic_cwnd = cmp::max(cubic_cwnd, w_est as u64);
            } else if cubic_cwnd < w_cubic as u64 {
                // Concave region or convex region use same increment.
                let cubic_inc =
                    (w_cubic - cubic_cwnd as f64) / cubic_cwnd as f64 * self.current_mtu as f64;

                // w_cubic grows cubically with the time since the last congestion
                // event and can exceed `u64::MAX` after a long lossless period.
                cubic_cwnd = cubic_cwnd.saturating_add(cubic_inc as u64);
            }

            // Update the increment and increase cwnd by MSS.
            // Keep release builds from wrapping the retained credit.
            self.state.cwnd_inc = self
                .state
                .cwnd_inc
                .saturating_add(cubic_cwnd - self.state.window);

            // cwnd_inc can be more than 1 MSS in the late stage of max probing.
            // however RFC9002 §7.3.3 (Congestion Avoidance) limits
            // the increase of cwnd to 1 max_datagram_size per cwnd acknowledged.
            // Keep the excess credit for later ACKs instead of discarding it.
            // https://www.rfc-editor.org/rfc/rfc9002.html#section-7.3.3
            if self.state.cwnd_inc >= self.current_mtu {
                self.state.window += self.current_mtu;
                self.state.cwnd_inc -= self.current_mtu;
            }
        }
    }

    /// Reduces the window for a congestion event whose newest packet was sent at `sent`.
    fn on_congestion(
        &mut self,
        now: Instant,
        sent: Instant,
        is_persistent_congestion: bool,
        is_ecn: bool,
    ) {
        if self
            .state
            .recovery_start_time
            .map(|recovery_start_time| sent <= recovery_start_time)
            .unwrap_or(false)
        {
            return;
        }

        // Save state in case this event ends up being spurious
        if !is_ecn {
            self.pre_congestion_state = Some(self.state.clone());
        }

        self.state.recovery_start_time = Some(now);
        let window = self.state.window as f64;

        // Fast convergence lowers W_max first; the 0.7 loss reduction still
        // applies to the old window, not to that already-reduced W_max.
        // https://www.rfc-editor.org/rfc/rfc9438.html#section-4.7
        // https://www.rfc-editor.org/rfc/rfc9438.html#section-4.6
        if window < self.state.w_max {
            self.state.w_max = window * (1.0 + BETA_CUBIC) / 2.0;
        } else {
            self.state.w_max = window;
        }

        self.state.ssthresh = cmp::max((window * BETA_CUBIC) as u64, self.minimum_window());
        self.state.window = self.state.ssthresh;
        self.state.k = self.state.cubic_k(self.current_mtu);

        self.state.cwnd_inc = (self.state.cwnd_inc as f64 * BETA_CUBIC) as u64;

        if is_persistent_congestion {
            // ssthresh already carries the single β_cubic reduction of the pre-event window
            // applied above; RFC 9438 §4.8 (Timeout) requires no further reduction of ssthresh,
            // only cwnd lowered to the minimum window.
            self.state.recovery_start_time = None;
            self.state.w_max = self.state.window as f64;

            self.state.cwnd_inc = 0;

            self.state.window = self.minimum_window();
        }
    }
}

impl Controller for Cubic {
    fn on_sent(&mut self, sent: Sent) -> Packet {
        self.pacer.on_transmit(sent.bytes);
        Packet::default()
    }

    fn on_ack(&mut self, ack: Ack<'_>) {
        self.rtt = ack.rtt;
        for acked in ack.packets {
            self.on_acked(ack.now, acked.sent, acked.bytes.into(), ack.app_limited);
        }
    }

    fn on_loss(&mut self, loss: Loss<'_>) {
        if let Some(newest) = loss.packets.last() {
            self.on_congestion(loss.now, newest.sent, loss.persistent, false);
        }
    }

    fn on_ce(&mut self, ce: Ce) {
        self.on_congestion(ce.now, ce.sent, false, true);
    }

    fn on_spurious_loss(&mut self) {
        if let Some(prior_state) = self.pre_congestion_state.take()
            && self.state.window < prior_state.window
        {
            self.state = prior_state;
        }
    }

    fn on_mtu_update(&mut self, new_mtu: u16) {
        self.current_mtu = new_mtu as u64;
        self.state.window = self.state.window.max(self.minimum_window());
    }

    fn window(&self) -> u64 {
        self.state.window
    }

    fn pace(&mut self, now: Instant, bytes: u64) -> Option<Instant> {
        let rate = Rate {
            window: self.state.window,
            pacing_rate: None,
        };
        self.pacer
            .delay(self.rtt, bytes, self.current_mtu as u16, now, rate)
    }

    fn metrics(&self) -> super::Metrics {
        super::Metrics {
            congestion_window: self.window(),
            ssthresh: Some(self.state.ssthresh),
            pacing_rate: None,
            bandwidth_estimate: None,
            // CUBIC has no probing phase to fill
            probe_inflight: None,
        }
    }

    fn clone_box(&self) -> Box<dyn Controller> {
        Box::new(self.clone())
    }
}

/// Configuration for the `Cubic` congestion controller
#[derive(Debug, Clone)]
pub struct Config {
    initial_window: u64,
}

impl Config {
    /// Default limit on the amount of outstanding data in bytes.
    ///
    /// Recommended value: `min(10 * max_datagram_size, max(2 * max_datagram_size, 14720))`
    pub fn initial_window(&mut self, value: u64) -> &mut Self {
        self.initial_window = value;
        self
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            initial_window: 14720.clamp(2 * BASE_DATAGRAM_SIZE, 10 * BASE_DATAGRAM_SIZE),
        }
    }
}

impl Factory for Config {
    fn build(self: Arc<Self>, init: Init) -> Box<dyn Controller> {
        Box::new(Cubic::new(self, init))
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn cubic(now: Instant) -> Cubic {
        Cubic::new(
            Arc::new(Config::default()),
            Init {
                now,
                mtu: BASE_DATAGRAM_SIZE as u16,
                rtt: Duration::from_millis(100),
                max_pacing_rate: None,
            },
        )
    }

    #[test]
    fn fast_convergence_reduces_w_max_without_double_reducing_window() {
        let now = Instant::now();
        let mut cubic = cubic(now);
        let window = 8 * BASE_DATAGRAM_SIZE;

        cubic.state.window = window;
        cubic.state.ssthresh = window;
        cubic.state.w_max = 12.0 * BASE_DATAGRAM_SIZE as f64;

        cubic.on_congestion(now, now + Duration::from_millis(1), false, false);

        assert_eq!(cubic.state.w_max, window as f64 * (1.0 + BETA_CUBIC) / 2.0);
        assert_eq!(cubic.state.ssthresh, (window as f64 * BETA_CUBIC) as u64);
        assert_eq!(cubic.state.window, cubic.state.ssthresh);
    }

    #[test]
    fn persistent_congestion_does_not_double_reduce_ssthresh() {
        let now = Instant::now();
        let mut cubic = cubic(now);
        let window = 8 * BASE_DATAGRAM_SIZE;

        cubic.state.window = window;
        cubic.state.ssthresh = window;
        cubic.state.w_max = window as f64;

        cubic.on_congestion(now, now + Duration::from_millis(1), true, false);

        // RFC 9438 §4.8 (Timeout): ssthresh is the pre-event window reduced by β_cubic
        // exactly once, as in §4.6; only cwnd is lowered to the minimum window.
        assert_eq!(cubic.state.ssthresh, (window as f64 * BETA_CUBIC) as u64);
        assert_eq!(cubic.state.window, 2 * BASE_DATAGRAM_SIZE);
    }

    #[test]
    fn congestion_avoidance_preserves_excess_cwnd_increment() {
        let now = Instant::now();
        let mut cubic = cubic(now);

        // Put CUBIC directly into congestion avoidance.
        cubic.state.ssthresh = cubic.state.window;
        cubic.state.recovery_start_time = Some(now);
        cubic.state.w_max = cubic.state.window as f64;

        // Simulate accumulated credit from earlier ACKs. One ACK may only grow
        // the window by one MTU, but the extra credit must remain for later ACKs.
        cubic.state.cwnd_inc = 2 * BASE_DATAGRAM_SIZE + 1;
        let window = cubic.state.window;

        cubic.on_acked(
            now,
            now + Duration::from_millis(1),
            BASE_DATAGRAM_SIZE,
            false,
        );

        // Before this fix, the window grew by one MTU and the remainder was
        // reset to zero.
        assert_eq!(cubic.state.window, window + BASE_DATAGRAM_SIZE);
        assert_eq!(cubic.state.cwnd_inc, BASE_DATAGRAM_SIZE + 1);
    }

    #[test]
    fn congestion_avoidance_does_not_overflow_after_long_lossless_period() {
        let now = Instant::now();
        let mut cubic = cubic(now);

        // Put CUBIC directly into congestion avoidance.
        cubic.state.ssthresh = cubic.state.window;
        cubic.state.recovery_start_time = Some(now);
        let window = cubic.state.window;

        // After ten days without a congestion event, w_cubic exceeds u64::MAX.
        // Before this fix, computing the window increment overflowed.
        let later = now + Duration::from_secs(10 * 24 * 60 * 60);
        cubic.on_acked(later, later, BASE_DATAGRAM_SIZE, false);

        assert_eq!(cubic.state.window, window + BASE_DATAGRAM_SIZE);
    }
}
