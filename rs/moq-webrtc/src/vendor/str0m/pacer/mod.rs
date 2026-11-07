use std::time::Instant;

use crate::Reason;
use crate::bwe_::ProbeClusterConfig;
use crate::rtp_::{Bitrate, DataSize, MidRid, TwccClusterId};

mod control;
pub(crate) use control::PacerControl;

mod null;
use null::NullPacer;

mod leaky;
use leaky::LeakyBucketPacer;

mod queue;
pub(crate) use queue::{PaddingRequest, QueuePriority, QueueSnapshot, QueueState};

#[allow(clippy::large_enum_variant)]
pub(crate) enum PacerImpl {
    Null(NullPacer),
    LeakyBucket(LeakyBucketPacer),
}

impl PacerImpl {
    pub fn leaky_bucket(rate: Bitrate) -> PacerImpl {
        PacerImpl::LeakyBucket(LeakyBucketPacer::new(rate))
    }

    pub fn null() -> PacerImpl {
        PacerImpl::Null(NullPacer::default())
    }

    pub fn start_probe(&mut self, config: ProbeClusterConfig) {
        match self {
            PacerImpl::Null(_) => {
                // NullPacer doesn't support probing
            }
            PacerImpl::LeakyBucket(v) => v.start_probe(config),
        }
    }

    pub fn active_cluster(&self) -> Option<TwccClusterId> {
        match self {
            PacerImpl::Null(_) => None,
            PacerImpl::LeakyBucket(v) => v.active_cluster(),
        }
    }

    pub fn check_probe_complete(&mut self, now: Instant) -> Option<TwccClusterId> {
        match self {
            PacerImpl::Null(_) => None,
            PacerImpl::LeakyBucket(v) => v.check_probe_complete(now),
        }
    }
}

impl Pacer for PacerImpl {
    fn set_pacing_rate(&mut self, pacing_bitrate: Bitrate) {
        match self {
            PacerImpl::Null(v) => v.set_pacing_rate(pacing_bitrate),
            PacerImpl::LeakyBucket(v) => v.set_pacing_rate(pacing_bitrate),
        }
    }

    fn set_padding_rate(&mut self, padding_bitrate: Bitrate) {
        match self {
            PacerImpl::Null(v) => v.set_padding_rate(padding_bitrate),
            PacerImpl::LeakyBucket(v) => v.set_padding_rate(padding_bitrate),
        }
    }

    fn poll_timeout(&self) -> (Option<Instant>, Reason) {
        match self {
            PacerImpl::Null(v) => v.poll_timeout(),
            PacerImpl::LeakyBucket(v) => v.poll_timeout(),
        }
    }

    fn handle_timeout(
        &mut self,
        now: Instant,
        iter: impl Iterator<Item = QueueState>,
    ) -> Option<PaddingRequest> {
        match self {
            PacerImpl::Null(v) => v.handle_timeout(now, iter),
            PacerImpl::LeakyBucket(v) => v.handle_timeout(now, iter),
        }
    }

    fn poll_queue(&mut self) -> Option<(MidRid, Option<TwccClusterId>)> {
        match self {
            PacerImpl::Null(v) => v.poll_queue(),
            PacerImpl::LeakyBucket(v) => v.poll_queue(),
        }
    }

    fn register_send(&mut self, now: Instant, packet_size: DataSize, from: MidRid) {
        match self {
            PacerImpl::Null(v) => v.register_send(now, packet_size, from),
            PacerImpl::LeakyBucket(v) => v.register_send(now, packet_size, from),
        }
    }
}

/// A packet Pacer.
///
/// The pacer is responsible for ensuring correct pacing of packets onto the network at a given
/// bitrate.
pub trait Pacer {
    /// Set the pacing bitrate. The pacing rate can be exceeded if required to drain excessively
    /// long packet queues.
    fn set_pacing_rate(&mut self, pacing_bitrate: Bitrate);

    /// Set the padding bitrate to send when there's no media to send
    fn set_padding_rate(&mut self, padding_bitrate: Bitrate);

    /// Poll for a timeout.
    fn poll_timeout(&self) -> (Option<Instant>, Reason);

    /// Handle time moving forward, should be called periodically as indicated by [`Pacer::poll_timeout`].
    fn handle_timeout(
        &mut self,
        now: Instant,
        iter: impl Iterator<Item = QueueState>,
    ) -> Option<PaddingRequest>;

    /// Determines which mid to poll, if any.
    ///
    /// Returns the MidRid to poll and the probe cluster ID if this packet belongs to a probe.
    fn poll_queue(&mut self) -> Option<(MidRid, Option<TwccClusterId>)>;

    /// Register a packet having been sent.
    ///
    /// **MUST** be called each time [`Pacer::poll_queue`] produces a mid.
    fn register_send(&mut self, now: Instant, packet_size: DataSize, from: MidRid);
}

/// The sub-reason for the [`Reason::Pacer`][crate::Reason::Pacer].
///
/// This enum is not considered stable API and may change in minor revisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PacerReason {
    /// Handle to update pacer budgets.
    Handle,
    /// First ever call to handle_timeout().
    FirstEver,
    /// Unpaced content such as audio.
    Unpaced,
    /// BWE probe cluster call 1.
    Probe1,
    /// BWE probe cluster call 2.
    Probe2,
    /// Regular paced content like video.
    Paced,
    /// Padding to inflate used bandwidth.
    Padding,
    /// Immediate timeout for state keeping reasons
    Immediate,
}
