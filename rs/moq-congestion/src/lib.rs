//! Congestion controllers that any transport can drive.
//!
//! A transport reports what it sends, what the peer acknowledges, and what it declares lost, and
//! asks the controller how much may be in flight and when the next packet may leave. Each sent
//! packet's [`Packet`] state is stored by the transport and handed back on ACK or loss, so a
//! controller keeps no per-packet table of its own.

use kio::clock::Instant;
use std::time::Duration;

use std::any::Any;
use std::sync::Arc;

mod bbr3;
mod cubic;
mod pacer;
mod rate;
mod unlimited;

pub use bbr3::{Bbr3, Bbr3Config};
pub use cubic::{Cubic, CubicConfig};
pub use rate::Packet;
pub use unlimited::Unlimited;

/// Common interface for different congestion controllers
pub trait Controller: Send + Sync {
    /// A packet counted in flight was just sent.
    ///
    /// The transport stores the returned state with the packet and hands it back in [`Acked`] or
    /// [`Lost`]. A packet sent before the controller was built, or rebuilt, is never handed back.
    fn on_sent(&mut self, sent: Sent) -> Packet;

    /// The connection had data to send but was blocked by the congestion window
    ///
    /// Reports the spec's `C.is_cwnd_limited` signal to the controller: the sender fully utilized
    /// the congestion window at this point in the current round trip.
    fn on_cwnd_limited(&mut self) {}

    /// A transmit poll drained the source without reaching a sender limit.
    ///
    /// Reports the spec's `CheckIfApplicationLimited` event with `in_flight` bytes outstanding,
    /// including after a partial send. Congestion, pacing, and the local send window are sender
    /// limits. Receiver credit counts as application-limited: for MoQ, peer flow control is a
    /// configuration limit rather than a measure of path capacity.
    #[allow(
        unused_variables,
        reason = "a no-op default that keeps the parameter names for implementors"
    )]
    fn on_app_limited(&mut self, in_flight: u64) {}

    /// An ACK newly acknowledged packets.
    fn on_ack(&mut self, ack: Ack<'_>);

    /// Loss detection declared packets lost.
    fn on_loss(&mut self, loss: Loss<'_>);

    /// An ACK reported newly CE-marked packets.
    fn on_ce(&mut self, ce: Ce);

    /// Packets were incorrectly deemed lost
    ///
    /// This function is called when all packets that were deemed lost (for instance because
    /// of packet reordering) are acknowledged after the loss was reported.
    fn on_spurious_loss(&mut self) {}

    /// The known MTU for the current network path has been updated
    fn on_mtu_update(&mut self, new_mtu: u16);

    /// The peer's ACK-frequency parameters have changed
    ///
    /// `ack_eliciting_threshold` is the number of ack-eliciting packets the peer may receive
    /// before being required to send an immediate ACK (per the QUIC ACK frequency extension).
    /// `requested_max_ack_delay` is the maximum delay we asked the peer to wait before sending
    /// an ACK when the threshold hasn't been reached.
    ///
    /// Controllers can use this to refine estimates that depend on peer ACK behavior (e.g.
    /// BBR's offload budget).
    #[allow(
        unused_variables,
        reason = "a no-op default that keeps the parameter names for implementors"
    )]
    fn on_ack_frequency_update(
        &mut self,
        ack_eliciting_threshold: u64,
        requested_max_ack_delay: Duration,
    ) {
    }

    /// Number of ack-eliciting bytes that may be in flight
    fn window(&self) -> u64;

    /// When a packet of `bytes` may leave: `None` for now, or the instant to ask again.
    fn pace(&mut self, now: Instant, bytes: u64) -> Option<Instant>;

    /// The most bytes to hand the network back to back, or `None` for no limit.
    fn burst(&self) -> Option<u64> {
        None
    }

    /// Retrieve implementation-specific metrics used to populate connection statistics and `qlog`
    /// traces when they are enabled
    fn metrics(&self) -> ControllerMetrics {
        ControllerMetrics {
            congestion_window: self.window(),
            ssthresh: None,
            pacing_rate: None,
            bandwidth_estimate: None,
            probe_inflight: None,
        }
    }

    /// Whether the controller ever reports [`ControllerMetrics::probe_inflight`], so an
    /// application-limited sender can probe for bandwidth through it
    fn probes(&self) -> bool {
        false
    }

    /// Duplicate the controller's state
    fn clone_box(&self) -> Box<dyn Controller>;

    /// Returns Self for use in down-casting to extract implementation details
    fn into_any(self: Box<Self>) -> Box<dyn Any>;
}

/// A packet the transport just sent.
#[derive(Debug, Clone, Copy)]
pub struct Sent {
    /// When it was sent.
    pub now: Instant,
    /// Its size in bytes.
    pub bytes: u16,
    /// Bytes in flight, including this packet.
    pub in_flight: u64,
}

/// An ACK that newly acknowledged packets.
#[derive(Debug, Clone, Copy)]
pub struct Ack<'a> {
    /// When the ACK arrived.
    pub now: Instant,
    /// The packets it newly acknowledged that carry controller state, in the order they were sent.
    /// Empty when it acknowledged only packets without any.
    pub packets: &'a [Acked],
    /// Bytes still in flight after this ACK.
    pub in_flight: u64,
    /// Whether the connection was blocked on outgoing application data before this ACK.
    pub app_limited: bool,
    /// The transport's smoothed RTT, including this ACK's sample.
    pub rtt: Duration,
}

/// One packet an ACK newly acknowledged.
#[derive(Debug, Clone, Copy)]
pub struct Acked {
    /// When it was sent.
    pub sent: Instant,
    /// Its size in bytes.
    pub bytes: u16,
    /// The state [`Controller::on_sent`] returned for it.
    pub packet: Packet,
}

/// Packets loss detection declared lost together.
#[derive(Debug, Clone, Copy)]
pub struct Loss<'a> {
    /// When they were declared lost.
    pub now: Instant,
    /// The lost packets that carry controller state, in the order they were sent. Never empty.
    pub packets: &'a [Lost],
    /// Bytes still in flight after the loss.
    pub in_flight: u64,
    /// Whether every packet sent across the persistent congestion period was lost (RFC 9002
    /// section 7.6).
    pub persistent: bool,
}

/// One packet declared lost.
#[derive(Debug, Clone, Copy)]
pub struct Lost {
    /// When it was sent.
    pub sent: Instant,
    /// Its size in bytes.
    pub bytes: u16,
    /// The state [`Controller::on_sent`] returned for it.
    pub packet: Packet,
    /// Whether the packet sent just before it was delivered, so it starts a new run of losses.
    pub new_burst: bool,
}

/// An ACK whose ECN counts reported newly CE-marked packets.
#[derive(Debug, Clone, Copy)]
pub struct Ce {
    /// When the ACK arrived.
    pub now: Instant,
    /// When the most recently sent packet that ACK acknowledged was sent.
    pub sent: Instant,
}

/// Common congestion controller metrics used for statistics and logging
#[derive(Default)]
pub struct ControllerMetrics {
    /// Congestion window (bytes)
    pub congestion_window: u64,
    /// Slow start threshold (bytes)
    pub ssthresh: Option<u64>,
    /// Pacing rate (bytes/s), for a transport that paces on its own
    pub pacing_rate: Option<u64>,
    /// Estimated sustainable path bandwidth (bytes/s)
    pub bandwidth_estimate: Option<u64>,
    /// Bytes in flight up to which an application-limited sender may resend in-flight data,
    /// while the controller probes for bandwidth (bytes)
    ///
    /// `None` when the controller is not probing, or cannot learn from a sender that fills its
    /// pacing with redundant data.
    pub probe_inflight: Option<u64>,
}

/// The path a controller starts on.
#[derive(Debug, Clone, Copy)]
pub struct Init {
    /// When the controller is built.
    pub now: Instant,
    /// The path's current MTU.
    pub mtu: u16,
    /// The transport's RTT estimate before any sample.
    pub rtt: Duration,
    /// The most bytes per second to pace at, whatever the controller computes.
    pub max_pacing_rate: Option<u64>,
}

/// Constructs controllers on demand
pub trait ControllerFactory {
    /// Construct a fresh `Controller`
    fn build(self: Arc<Self>, init: Init) -> Box<dyn Controller>;
}

const BASE_DATAGRAM_SIZE: u64 = 1200;
