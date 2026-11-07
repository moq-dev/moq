//! Bandwidth estimation.

use crate::poll::RtcMut;
use crate::{Rtc, rtp_::Mid};

pub use crate::rtp_::Bitrate;

#[derive(Debug, PartialEq)]
#[non_exhaustive]
/// Bandwidth estimation kind.
pub enum BweKind {
    /// Transport wide congestion control.
    ///
    /// Emits the current estimate with `can_probe: true` when probing becomes
    /// available, initially using the configured starting bitrate. Estimate
    /// updates follow while probing is available. On loss of probing capability,
    /// emits the last reported estimate once with `can_probe: false`, then
    /// suppresses updates until probing becomes available again.
    Twcc {
        /// Estimated available bitrate. Initially this is the configured starting bitrate.
        /// When probing becomes unavailable, this retains the last estimate.
        estimate: Bitrate,
        /// Whether the local BWE can currently probe for capacity.
        ///
        /// Requires BWE to be enabled, SRTP keys, and a sending media section
        /// supporting TWCC feedback and the transport sequence extension.
        /// This does not guarantee that feedback is arriving.
        can_probe: bool,
    },
    /// REMB (Receiver Estimated Maximum Bitrate)
    Remb {
        /// Bitrate reported by the remote receiver.
        estimate: Bitrate,
        /// Media section associated with the report.
        mid: Mid,
    },
}

/// Access to the Bandwidth Estimate subsystem.
///
// The inner `RtcMut` arms the readiness on mutable deref only; see `crate::poll`.
pub struct Bwe<'a>(pub(crate) RtcMut<'a>);

impl<'a> Bwe<'a> {
    pub(crate) fn new(rtc: &'a mut Rtc) -> Self {
        Bwe(RtcMut::new(rtc))
    }

    /// Configure the minimum transmit bitrate and desired bandwidth.
    ///
    /// `minimum` is the total outgoing rate to sustain with media and padding. When
    /// media sends less, padding fills the gap, capped by the current bandwidth
    /// estimate. Padding is suppressed during overuse and requires an active sending
    /// media section with TWCC feedback and a usable padding source. This is not guaranteed bandwidth.
    /// The automatic 50 kbit/s padding target remains a floor, even with a zero
    /// minimum, and is also capped by the estimate. The minimum is clamped to
    /// `desired`; it does not force a probe or raise the estimate.
    ///
    /// `desired` is the bitrate you would like to eventually send at. BWE tries to
    /// discover this capacity through probing. It is not a hard ceiling: probes
    /// may exceed it, and the network may not sustain it. Allocate media using the
    /// estimates from [`Event::EgressBitrateEstimate`][crate::Event::EgressBitrateEstimate].
    ///
    /// Increasing the desired bitrate above the current estimate can trigger an allocation
    /// probe, whether or not the application is currently sending below its estimated capacity.
    /// Initial probing and congestion conditions can suppress this probe. Repeating or reducing
    /// the desired bitrate does not request an allocation probe.
    ///
    /// An SFU can set `minimum` to the rate needed to start its next video layer,
    /// and `desired` to the rate needed for its preferred quality. As the estimate
    /// grows, more padding is allowed until media can take its place. Lower the
    /// minimum when sustained traffic is no longer wanted.
    ///
    /// This only has an effect when BWE is enabled via
    /// [`RtcConfig::enable_bwe`][crate::RtcConfig::enable_bwe]. Both rates are retained
    /// across [`reset`][Self::reset].
    ///
    /// # Example
    ///
    /// ```
    /// # use str0m::{Rtc, bwe::Bitrate};
    /// # let mut rtc = Rtc::builder().enable_bwe(Some(Bitrate::kbps(300))).build(std::time::Instant::now());
    /// // Sustain traffic toward the next layer while probing for the preferred rate.
    /// rtc.bwe().set_bitrate(Bitrate::kbps(800), Bitrate::mbps(3));
    /// ```
    pub fn set_bitrate(&mut self, minimum: Bitrate, desired: Bitrate) {
        let mut m = self.0.mutate();
        m.session.set_bwe_bitrate(minimum, desired);
        // Reconfiguring BWE only re-arms the estimator/pacer timers; any probing
        // padding is produced later from the timeout path, so no event is queued.
        m.no_events();
    }

    /// Reset the BWE with a new init_bitrate
    ///
    /// This discards the current estimator state and starts estimation again
    /// from `init_bitrate`. Normal transitions from audio-only to video sending
    /// do not require a reset: with TWCC negotiated, probing can discover
    /// capacity before video starts. The desired bitrate controls the capacity
    /// the estimator attempts to discover.
    pub fn reset(&mut self, init_bitrate: Bitrate) {
        let mut m = self.0.mutate();
        m.session.reset_bwe(init_bitrate);
        // Resetting the estimator moves no output, only timers.
        m.no_events();
    }
}
